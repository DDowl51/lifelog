//! 应用功能 `vectors`：给文档（摘要、笔记）算向量，按意思检索。
//!
//! - 建索引：有定时器的进程（常驻进程）里，按文档的修订号增量做：切块（`vectors-core`）、
//!   用 `Embedder` 算向量、存进自己的表。模型暂时不可用（还在下载）时提醒一次，下一轮再试。
//! - 检索：算出查询的向量，和存下的逐个比（文档是几百到几千篇，不需要向量扩展）。
//!   只和同一个模型算出来的比：换了模型，旧的向量不算数，会被慢慢重算。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use documents_contracts::{DOCUMENT_READ, DocumentRead};
use embedder_contract::{EMBEDDER, EmbedPurpose, Embedder};
use kernel_api::{
    AppError, Component, ConfigSection, ErrorKind, Host, Kind, Manifest, async_trait,
    register_component,
};
use log_contract::{LOG, Logger};
use serde::{Deserialize, Serialize};
use sql_contract::{MIGRATIONS, SQL, Sql, SqlValue, Statement, StaticMigrations};
use timer_contract::{Fired, TIMER, TIMER_FIRED, TimerId};
use vectors_contracts::{VECTOR_SEARCH, VectorHit, VectorQuery, VectorSearch};
use vectors_core::{best_per_document, chunks, cosine, from_bytes, to_bytes};

/// 已经执行过的迁移不能改。改表结构在末尾追加。
const TABLES: &[(&str, &str)] = &[(
    "文档的向量",
    "CREATE TABLE vectors_chunks (
        id INTEGER PRIMARY KEY,
        doc_id INTEGER NOT NULL,
        key TEXT NOT NULL,
        ord INTEGER NOT NULL,
        text TEXT NOT NULL,
        model TEXT NOT NULL,
        embedding BLOB NOT NULL
     ) STRICT;
     CREATE INDEX vectors_chunks_doc ON vectors_chunks(doc_id);
     CREATE TABLE vectors_cursor (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        revision INTEGER NOT NULL
     ) STRICT;",
)];

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
struct Config {
    /// 多久看一次有没有新文档要算向量。
    index_secs: u64,
    /// 一块最多多少字。
    chunk_chars: usize,
    /// 相似度低于这个的不算命中。不同模型的相似度范围不一样，换模型时要跟着调。
    min_score: f64,
    /// 一轮最多处理几篇文档。
    batch: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            index_secs: 60,
            chunk_chars: 500,
            min_score: 0.85,
            batch: 50,
        }
    }
}

struct Vectors {
    sql: Arc<dyn Sql>,
    documents: Arc<dyn DocumentRead>,
    embedder: Arc<dyn Embedder>,
    log: Logger,
    timer: OnceLock<TimerId>,
    config: Config,
    warned: AtomicBool,
}

impl Vectors {
    /// 处理一批变了的文档。返回处理了几篇。
    async fn index(&self) -> Result<usize, AppError> {
        let rows = self
            .sql
            .query(Statement::new(
                "SELECT revision FROM vectors_cursor WHERE id = 1",
            ))
            .await?;
        let after = rows
            .rows
            .first()
            .and_then(|r| r.first())
            .and_then(SqlValue::as_i64)
            .unwrap_or(0);
        let changed = self
            .documents
            .changed_since(after, self.config.batch)
            .await?;
        let Some(last) = changed.last().map(|d| d.revision) else {
            return Ok(0);
        };
        let mut statements = Vec::new();
        for doc in &changed {
            statements.push(
                Statement::new("DELETE FROM vectors_chunks WHERE doc_id = ?1").bind(doc.id.0),
            );
            if doc.deleted {
                continue;
            }
            let pieces = chunks(
                &doc.document.title,
                &doc.document.body,
                self.config.chunk_chars,
            );
            if pieces.is_empty() {
                continue;
            }
            // 模型不可用时这里返回错误：整批不写、游标不动，下一轮再来
            let embedded = self
                .embedder
                .embed(pieces.clone(), EmbedPurpose::Passage)
                .await?;
            for (ord, (text, vector)) in pieces.iter().zip(&embedded.vectors).enumerate() {
                statements.push(
                    Statement::new(
                        "INSERT INTO vectors_chunks (doc_id, key, ord, text, model, embedding) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    )
                    .bind(doc.id.0)
                    .bind(doc.document.key.as_str())
                    .bind(i64::try_from(ord).unwrap_or(i64::MAX))
                    .bind(text.as_str())
                    .bind(embedded.model.as_str())
                    .bind(to_bytes(vector)),
                );
            }
        }
        statements.push(
            Statement::new(
                "INSERT INTO vectors_cursor (id, revision) VALUES (1, ?1)
                 ON CONFLICT(id) DO UPDATE SET revision = excluded.revision",
            )
            .bind(last),
        );
        self.sql.execute(statements).await?;
        Ok(changed.len())
    }

    async fn on_timer(&self, fired: Fired) -> Result<(), AppError> {
        if self.timer.get() != Some(&fired.id) {
            return Ok(());
        }
        match self.index().await {
            Ok(_) => {
                self.warned.store(false, Ordering::Relaxed);
                Ok(())
            }
            Err(e) if e.kind == ErrorKind::Unavailable => {
                if !self.warned.swap(true, Ordering::Relaxed) {
                    self.log.warn(format!("暂时算不了向量：{e}")).await;
                }
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}

#[async_trait]
impl VectorSearch for Vectors {
    async fn search(&self, query: VectorQuery) -> Result<Vec<VectorHit>, AppError> {
        if query.text.trim().is_empty() {
            return Ok(Vec::new());
        }
        let embedded = self
            .embedder
            .embed(vec![query.text.clone()], EmbedPurpose::Query)
            .await?;
        let Some(target) = embedded.vectors.first() else {
            return Ok(Vec::new());
        };
        let rows = self
            .sql
            .query(
                Statement::new("SELECT key, text, embedding FROM vectors_chunks WHERE model = ?1")
                    .bind(embedded.model.as_str()),
            )
            .await?;
        let scored: Vec<(String, String, f64)> = rows
            .rows
            .iter()
            .filter_map(|r| {
                let key = r.first()?.as_str()?.to_owned();
                let text = r.get(1)?.as_str()?.to_owned();
                let SqlValue::Blob(bytes) = r.get(2)? else {
                    return None;
                };
                Some((key, text, cosine(target, &from_bytes(bytes))))
            })
            .collect();
        Ok(
            best_per_document(scored, self.config.min_score, query.limit as usize)
                .into_iter()
                .map(|(key, chunk, score)| VectorHit { key, score, chunk })
                .collect(),
        )
    }
}

pub struct VectorsComponent;

impl Component for VectorsComponent {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "vectors",
            kind: Kind::Feature,
            summary: "给文档算向量，按意思检索",
            provides: vec![VECTOR_SEARCH.id()],
            requires: vec![
                SQL.id(),
                MIGRATIONS.id(),
                DOCUMENT_READ.id(),
                EMBEDDER.id(),
                LOG.id(),
            ],
            // 有定时器的进程才建索引
            optional_requires: vec![TIMER.id()],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let config: Config = config.parse()?;
        if config.index_secs == 0 || config.batch == 0 {
            return Err(AppError::config("index_secs、batch 都不能是 0"));
        }
        host.contribute(
            &MIGRATIONS,
            Arc::new(StaticMigrations::new("vectors", TABLES)),
        )?;
        let index_secs = config.index_secs;
        let vectors = Arc::new(Vectors {
            sql: host.resolve(&SQL)?,
            documents: host.resolve(&DOCUMENT_READ)?,
            embedder: host.resolve(&EMBEDDER)?,
            log: Logger::new(host.resolve(&LOG)?, host.id()),
            timer: OnceLock::new(),
            config,
            warned: AtomicBool::new(false),
        });
        if let Some(timer) = host.try_resolve(&TIMER)? {
            let v = vectors.clone();
            host.subscribe(&TIMER_FIRED, "index", move |fired| {
                let v = v.clone();
                async move { v.on_timer(fired).await }
            })?;
            let v = vectors.clone();
            host.on_ready(move || async move {
                let id = timer.every(index_secs * 1_000).await?;
                let _ = v.timer.set(id);
                Ok(())
            });
        }
        host.provide(&VECTOR_SEARCH, vectors)
    }
}

register_component!(VectorsComponent);
