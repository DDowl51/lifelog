//! 应用功能 `search`：给记下的活动建全文索引，并在里面检索。
//!
//! - 建索引：有定时器的进程（常驻进程）里，定时按编号往下拉还没索引的活动，
//!   用 `Tokenizer` 切词，写进自己的 FTS5 表；索引和游标在同一个事务里写。
//!   没有定时器的进程（只读的命令行）只检索。
//! - 检索：查询词用同一个 `Tokenizer` 切，在索引里找，再向 `activity` 取回活动、合并结果。
//!
//! 索引用的是 SQLite 的 FTS5，属于设计里说的「各功能的 SQL 是 SQLite 方言」这一已知限制。

use std::sync::{Arc, OnceLock};

use activity_contracts::{ACTIVITY_READ, ActivityId, ActivityRead};
use documents_contracts::{DOCUMENT_READ, DocumentRead};
use kernel_api::{
    AppError, Component, ConfigSection, Host, Kind, Manifest, async_trait, register_component,
};
use log_contract::{LOG, Logger};
use search_contracts::{DOCUMENTS, DocumentHit, SEARCH, Search, SearchQuery, SearchResult};
use search_core::{group_hits, match_expression, rrf, searchable_text, snippet};
use serde::{Deserialize, Serialize};
use sql_contract::{MIGRATIONS, SQL, Sql, Statement, StaticMigrations};
use timer_contract::{Fired, TIMER, TIMER_FIRED, TimerId};
use tokenizer_contract::{Purpose, TOKENIZER, Tokenizer};
use vectors_contracts::{VECTOR_SEARCH, VectorQuery, VectorSearch};

/// 已经执行过的迁移不能改。改表结构在末尾追加。
const TABLES: &[(&str, &str)] = &[
    (
    "全文索引",
    "CREATE VIRTUAL TABLE search_index USING fts5(text, content = '', tokenize = 'unicode61 remove_diacritics 2');
     CREATE TABLE search_docs (
        id INTEGER PRIMARY KEY,
        source TEXT NOT NULL,
        ts INTEGER NOT NULL
     ) STRICT;
     CREATE INDEX search_docs_ts ON search_docs(ts);
     CREATE TABLE search_cursor (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        last_id INTEGER NOT NULL
     ) STRICT;",
),
    (
        "文档的全文索引",
        "CREATE VIRTUAL TABLE search_doc_index USING fts5(text, tokenize = 'unicode61 remove_diacritics 2');
         CREATE TABLE search_doc_meta (
            id INTEGER PRIMARY KEY,
            key TEXT NOT NULL UNIQUE,
            kind TEXT NOT NULL,
            date TEXT,
            title TEXT NOT NULL,
            body TEXT NOT NULL
         ) STRICT;
         CREATE TABLE search_doc_cursor (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            revision INTEGER NOT NULL
         ) STRICT;",
    ),
];

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
struct Config {
    /// 多久看一次有没有新活动要索引。
    index_secs: u64,
    /// 一次索引多少条。
    batch: u32,
    /// 一轮最多索引几批（第一次建索引时分几轮做完）。
    max_batches: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            index_secs: 30,
            batch: 2_000,
            max_batches: 25,
        }
    }
}

/// 文档最多给几篇。
const DOCUMENT_HITS: usize = 10;
/// 文档的摘录多长。
const SNIPPET_CHARS: usize = 80;

/// 文档的日期在不在 `[since, until)` 里（按那天的中午算；没有日期的都算在里面）。
fn in_range(date: Option<&str>, since: Option<i64>, until: Option<i64>) -> bool {
    let Some(ms) = date
        .and_then(|d| d.parse::<jiff::civil::Date>().ok())
        .and_then(|d| d.at(12, 0, 0, 0).to_zoned(jiff::tz::TimeZone::UTC).ok())
        .map(|z| z.timestamp().as_millisecond())
    else {
        return true;
    };
    since.is_none_or(|s| ms >= s) && until.is_none_or(|u| ms < u)
}

/// 检索时一次取多少个候选。合并之后可能少很多（同一个窗口标题出现几十次），
/// 不够就接着往下取，最多看 `MAX_CANDIDATES` 个。
const CANDIDATE_PAGE: u32 = 500;
const MAX_CANDIDATES: u32 = 20_000;

struct Index {
    sql: Arc<dyn Sql>,
    read: Arc<dyn ActivityRead>,
    tokenizer: Arc<dyn Tokenizer>,
    /// 没有启用 `documents` 时是 `None`：只检索活动。
    documents: Option<Arc<dyn DocumentRead>>,
    /// 没有启用向量检索时是 `None`：文档只按全文排。
    vectors: Option<Arc<dyn VectorSearch>>,
    log: Logger,
    timer: OnceLock<TimerId>,
    batch: u32,
    max_batches: u32,
}

impl Index {
    async fn last_id(&self) -> Result<Option<ActivityId>, AppError> {
        let rows = self
            .sql
            .query(Statement::new(
                "SELECT last_id FROM search_cursor WHERE id = 1",
            ))
            .await?;
        Ok(rows
            .rows
            .first()
            .and_then(|r| r.first())
            .and_then(|v| v.as_i64())
            .map(ActivityId))
    }

    /// 索引一轮。返回这一轮索引了几条。
    async fn index(&self) -> Result<usize, AppError> {
        let mut after = self.last_id().await?;
        let mut total = 0;
        for _ in 0..self.max_batches {
            let batch = self.read.since(after, self.batch).await?;
            let Some(last) = batch.last().map(|s| s.id) else {
                break;
            };
            let texts: Vec<String> = batch.iter().map(|s| searchable_text(&s.activity)).collect();
            let words = self.tokenizer.tokenize(texts, Purpose::Index).await?;
            let mut statements = Vec::new();
            for (stored, words) in batch.iter().zip(words) {
                if words.is_empty() {
                    continue;
                }
                statements.push(
                    Statement::new("INSERT INTO search_index (rowid, text) VALUES (?1, ?2)")
                        .bind(stored.id.0)
                        .bind(words.join(" ")),
                );
                statements.push(
                    Statement::new("INSERT INTO search_docs (id, source, ts) VALUES (?1, ?2, ?3)")
                        .bind(stored.id.0)
                        .bind(stored.activity.source.as_str())
                        .bind(stored.activity.ts_start),
                );
                total += 1;
            }
            statements.push(
                Statement::new(
                    "INSERT INTO search_cursor (id, last_id) VALUES (1, ?1)
                     ON CONFLICT(id) DO UPDATE SET last_id = excluded.last_id",
                )
                .bind(last.0),
            );
            // 索引和游标一起写：要么都在，要么都不在，不会重复索引
            self.sql.execute(statements).await?;
            after = Some(last);
            if batch.len() < self.batch as usize {
                break;
            }
        }
        Ok(total)
    }

    /// 文档按修订号增量索引：改了的重新索引，删了的去掉。
    async fn index_documents(&self) -> Result<usize, AppError> {
        let Some(documents) = &self.documents else {
            return Ok(0);
        };
        let rows = self
            .sql
            .query(Statement::new(
                "SELECT revision FROM search_doc_cursor WHERE id = 1",
            ))
            .await?;
        let mut after = rows
            .rows
            .first()
            .and_then(|r| r.first())
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let mut total = 0;
        for _ in 0..self.max_batches {
            let changed = documents.changed_since(after, self.batch).await?;
            let Some(last) = changed.last().map(|d| d.revision) else {
                break;
            };
            let texts: Vec<String> = changed
                .iter()
                .map(|d| format!("{}\n{}", d.document.title, d.document.body))
                .collect();
            let words = self.tokenizer.tokenize(texts, Purpose::Index).await?;
            let mut statements = Vec::new();
            for (doc, words) in changed.iter().zip(words) {
                statements.push(
                    Statement::new("DELETE FROM search_doc_index WHERE rowid = ?1").bind(doc.id.0),
                );
                statements.push(
                    Statement::new("DELETE FROM search_doc_meta WHERE id = ?1").bind(doc.id.0),
                );
                if doc.deleted || words.is_empty() {
                    continue;
                }
                statements.push(
                    Statement::new("INSERT INTO search_doc_index (rowid, text) VALUES (?1, ?2)")
                        .bind(doc.id.0)
                        .bind(words.join(" ")),
                );
                let d = &doc.document;
                statements.push(
                    Statement::new(
                        "INSERT INTO search_doc_meta (id, key, kind, date, title, body) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    )
                    .bind(doc.id.0)
                    .bind(d.key.as_str())
                    .bind(d.kind.as_str())
                    .bind(d.date.clone())
                    .bind(d.title.as_str())
                    .bind(d.body.as_str()),
                );
                total += 1;
            }
            statements.push(
                Statement::new(
                    "INSERT INTO search_doc_cursor (id, revision) VALUES (1, ?1)
                     ON CONFLICT(id) DO UPDATE SET revision = excluded.revision",
                )
                .bind(last),
            );
            self.sql.execute(statements).await?;
            after = last;
            if changed.len() < self.batch as usize {
                break;
            }
        }
        Ok(total)
    }

    /// 文档：全文和（有的话）向量两路，按 RRF 合并，最多 `limit` 篇。
    async fn search_documents(
        &self,
        query: &SearchQuery,
        expression: &str,
        limit: usize,
    ) -> Result<Vec<DocumentHit>, AppError> {
        let wanted = |s: &str| {
            (query.sources.is_empty() || query.sources.iter().any(|x| x == s))
                && !query.exclude_sources.iter().any(|x| x == s)
        };
        if self.documents.is_none() || !wanted(DOCUMENTS) || limit == 0 {
            return Ok(Vec::new());
        }
        let rows = self
            .sql
            .query(
                Statement::new(
                    "SELECT m.key FROM search_doc_index JOIN search_doc_meta m ON m.id = search_doc_index.rowid
                     WHERE search_doc_index MATCH ?1 ORDER BY bm25(search_doc_index) LIMIT 30",
                )
                .bind(expression),
            )
            .await?;
        let by_words: Vec<String> = rows
            .rows
            .iter()
            .filter_map(|r| r.first().and_then(|v| v.as_str()).map(str::to_owned))
            .collect();
        // 向量那一路出错（比如常驻进程没在跑）：只用全文，不让整次检索失败
        let mut chunks: Vec<(String, String)> = Vec::new();
        if let Some(vectors) = &self.vectors {
            match vectors
                .search(VectorQuery {
                    text: query.text.clone(),
                    limit: 20,
                })
                .await
            {
                Ok(hits) => chunks = hits.into_iter().map(|h| (h.key, h.chunk)).collect(),
                // 向量检索被停用了：安静地只按全文
                Err(e) if e.kind == kernel_api::ErrorKind::NotFound => {}
                Err(e) => {
                    self.log
                        .warn(format!("向量检索暂时用不了，只按全文找：{e}"))
                        .await
                }
            }
        }
        let by_meaning: Vec<String> = chunks.iter().map(|(k, _)| k.clone()).collect();
        let words: Vec<&str> = query.text.split_whitespace().collect();
        let mut out = Vec::new();
        for key in rrf(&[by_words.clone(), by_meaning]) {
            if out.len() >= limit {
                break;
            }
            let rows = self
                .sql
                .query(
                    Statement::new(
                        "SELECT kind, date, title, body FROM search_doc_meta WHERE key = ?1",
                    )
                    .bind(key.as_str()),
                )
                .await?;
            let Some(row) = rows.rows.first() else {
                continue;
            };
            let text = |i: usize| row.get(i).and_then(|v| v.as_str()).map(str::to_owned);
            let date = text(1);
            if !in_range(date.as_deref(), query.since, query.until) {
                continue;
            }
            let body = text(3).unwrap_or_default();
            // 只靠向量找到的：给最相近的那一段；全文找到的：给命中词附近的一段
            let snippet = match chunks.iter().find(|(k, _)| *k == key) {
                Some((_, chunk)) if !by_words.contains(&key) => snippet(chunk, &[], SNIPPET_CHARS),
                _ => snippet(&body, &words, SNIPPET_CHARS),
            };
            out.push(DocumentHit {
                key,
                kind: text(0).unwrap_or_default(),
                date,
                title: text(2).unwrap_or_default(),
                snippet,
            });
        }
        Ok(out)
    }

    async fn on_timer(&self, fired: Fired) -> Result<(), AppError> {
        if self.timer.get() != Some(&fired.id) {
            return Ok(());
        }
        if let Err(e) = self.index().await {
            // 下一轮从游标处再来；这里只记下来
            self.log.warn(format!("建索引失败：{e}")).await;
        }
        if let Err(e) = self.index_documents().await {
            self.log.warn(format!("给文档建索引失败：{e}")).await;
        }
        Ok(())
    }
}

#[async_trait]
impl Search for Index {
    async fn search(&self, query: SearchQuery) -> Result<SearchResult, AppError> {
        let words = self
            .tokenizer
            .tokenize(vec![query.text.clone()], Purpose::Query)
            .await?
            .pop()
            .unwrap_or_default();
        let expression =
            match_expression(&words).ok_or_else(|| AppError::validation("没有可以检索的词"))?;
        let mut sql = String::from(
            "SELECT d.id FROM search_index JOIN search_docs d ON d.id = search_index.rowid
             WHERE search_index MATCH ?1 AND d.ts >= ?2 AND d.ts < ?3",
        );
        let mut source_params: Vec<String> = Vec::new();
        let mut list = |sql: &mut String, op: &str, sources: &[String]| {
            if sources.is_empty() {
                return;
            }
            let marks = vec!["?"; sources.len()].join(", ");
            sql.push_str(&format!(" AND d.source {op} ({marks})"));
            source_params.extend(sources.iter().cloned());
        };
        list(&mut sql, "IN", &query.sources);
        list(&mut sql, "NOT IN", &query.exclude_sources);
        sql.push_str(" ORDER BY d.ts DESC, d.id DESC LIMIT ? OFFSET ?");

        let want = query.limit as usize;
        let mut found = Vec::new();
        let mut scanned: u32 = 0;
        let mut exhausted = false;
        // 一页一页地取候选，直到合并后的结果多于要的条数（才知道有没有更多），或者没有了
        loop {
            let mut statement = Statement::new(sql.clone())
                .bind(expression.clone())
                .bind(query.since.unwrap_or(i64::MIN))
                .bind(query.until.unwrap_or(i64::MAX));
            for p in &source_params {
                statement = statement.bind(p.as_str());
            }
            let statement = statement
                .bind(i64::from(CANDIDATE_PAGE))
                .bind(i64::from(scanned));
            let rows = self.sql.query(statement).await?;
            let ids: Vec<ActivityId> = rows
                .rows
                .iter()
                .filter_map(|r| r.first().and_then(|v| v.as_i64()))
                .map(ActivityId)
                .collect();
            scanned = scanned.saturating_add(u32::try_from(ids.len()).unwrap_or(u32::MAX));
            if ids.len() < CANDIDATE_PAGE as usize {
                exhausted = true;
            }
            found.extend(self.read.get(ids).await?);
            if exhausted || scanned >= MAX_CANDIDATES || group_hits(&found).len() > want {
                break;
            }
        }
        let mut hits = group_hits(&found);
        // 没看完全部候选时，最后一条的次数可能还不全，也算「还有更多」
        let truncated = hits.len() > want || !exhausted;
        hits.truncate(want);
        let documents = self
            .search_documents(&query, &expression, (want).min(DOCUMENT_HITS))
            .await?;
        Ok(SearchResult {
            hits,
            documents,
            truncated,
        })
    }
}

pub struct SearchComponent;

impl Component for SearchComponent {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "search",
            kind: Kind::Feature,
            summary: "给记下的活动建全文索引，并在里面检索",
            provides: vec![SEARCH.id()],
            requires: vec![
                SQL.id(),
                MIGRATIONS.id(),
                ACTIVITY_READ.id(),
                TOKENIZER.id(),
                LOG.id(),
            ],
            // 有定时器的进程才建索引
            optional_requires: vec![TIMER.id(), DOCUMENT_READ.id(), VECTOR_SEARCH.id()],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let config: Config = config.parse()?;
        if config.index_secs == 0 || config.batch == 0 || config.max_batches == 0 {
            return Err(AppError::config(
                "index_secs、batch、max_batches 都不能是 0",
            ));
        }
        host.contribute(
            &MIGRATIONS,
            Arc::new(StaticMigrations::new("search", TABLES)),
        )?;
        let index = Arc::new(Index {
            sql: host.resolve(&SQL)?,
            read: host.resolve(&ACTIVITY_READ)?,
            tokenizer: host.resolve(&TOKENIZER)?,
            documents: host.try_resolve(&DOCUMENT_READ)?,
            vectors: host.try_resolve(&VECTOR_SEARCH)?,
            log: Logger::new(host.resolve(&LOG)?, host.id()),
            timer: OnceLock::new(),
            batch: config.batch,
            max_batches: config.max_batches,
        });

        if let Some(timer) = host.try_resolve(&TIMER)? {
            let i = index.clone();
            host.subscribe(&TIMER_FIRED, "index", move |fired| {
                let i = i.clone();
                async move { i.on_timer(fired).await }
            })?;
            let i = index.clone();
            host.on_ready(move || async move {
                let id = timer.every(config.index_secs * 1_000).await?;
                let _ = i.timer.set(id);
                Ok(())
            });
        }
        host.provide(&SEARCH, index)
    }
}

register_component!(SearchComponent);
