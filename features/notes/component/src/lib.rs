//! 应用功能 `notes`：把一个目录里的 Markdown 笔记同步进文档库。
//!
//! 定时列出文件（`Files`），按内容哈希判断哪些变了：变了的转成文档（`notes-core`）写进文档库，
//! 删掉的从文档库删掉。符合摘要格式的文件同步回那一天的摘要：手改过的摘要因此也能被检索到。

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use documents_contracts::{DOCUMENT_WRITE, DocumentWrite};
use files_contract::{FILES, Files};
use kernel_api::{AppError, Component, ConfigSection, Host, Kind, Manifest, register_component};
use log_contract::{LOG, Logger};
use notes_core::{content_hash, to_document};
use serde::{Deserialize, Serialize};
use sql_contract::{MIGRATIONS, SQL, Sql, Statement, StaticMigrations};
use timer_contract::{Fired, TIMER, TIMER_FIRED, TimerId};

/// 已经执行过的迁移不能改。改表结构在末尾追加。
const TABLES: &[(&str, &str)] = &[(
    "同步过的文件",
    "CREATE TABLE notes_files (
        path TEXT PRIMARY KEY,
        key TEXT NOT NULL,
        hash TEXT NOT NULL
     ) STRICT;",
)];

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
struct Config {
    /// 文件根目录下的哪些目录（`""` 是整个根目录）。
    dirs: Vec<String>,
    /// 多久看一次。
    poll_secs: u64,
    /// 摘要文件的格式，和 `summary` 的 `path` 一致。
    summary_path: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dirs: vec![String::new()],
            poll_secs: 300,
            summary_path: "daily/{date}.md".to_owned(),
        }
    }
}

struct Notes {
    files: Arc<dyn Files>,
    documents: Arc<dyn DocumentWrite>,
    sql: Arc<dyn Sql>,
    log: Logger,
    timer: OnceLock<TimerId>,
    config: Config,
}

impl Notes {
    /// 同步一轮。返回（写入的，删掉的）篇数。
    async fn sync(&self) -> Result<(usize, usize), AppError> {
        let rows = self
            .sql
            .query(Statement::new("SELECT path, key, hash FROM notes_files"))
            .await?;
        let mut known: BTreeMap<String, (String, String)> = rows
            .rows
            .iter()
            .filter_map(|r| {
                Some((
                    r.first()?.as_str()?.to_owned(),
                    (
                        r.get(1)?.as_str()?.to_owned(),
                        r.get(2)?.as_str()?.to_owned(),
                    ),
                ))
            })
            .collect();
        let mut seen = Vec::new();
        let (mut written, mut removed) = (0, 0);
        for dir in &self.config.dirs {
            for file in self.files.list(dir.clone(), ".md".to_owned()).await? {
                if seen.contains(&file.path) {
                    continue;
                }
                seen.push(file.path.clone());
                let Some(text) = self.files.read(file.path.clone()).await? else {
                    continue;
                };
                let hash = content_hash(&text);
                if known.get(&file.path).is_some_and(|(_, h)| *h == hash) {
                    continue;
                }
                let document = to_document(&file.path, &text, &self.config.summary_path);
                let key = document.key.clone();
                self.documents.put(document).await?;
                self.sql
                    .execute(vec![
                        Statement::new(
                            "INSERT INTO notes_files (path, key, hash) VALUES (?1, ?2, ?3)
                             ON CONFLICT(path) DO UPDATE SET key = excluded.key, hash = excluded.hash",
                        )
                        .bind(file.path.as_str())
                        .bind(key.as_str())
                        .bind(hash),
                    ])
                    .await?;
                written += 1;
            }
        }
        known.retain(|path, _| !seen.contains(path));
        for (path, (key, _)) in known {
            self.documents.remove(key).await?;
            self.sql
                .execute(vec![
                    Statement::new("DELETE FROM notes_files WHERE path = ?1").bind(path),
                ])
                .await?;
            removed += 1;
        }
        Ok((written, removed))
    }

    async fn on_timer(&self, fired: Fired) -> Result<(), AppError> {
        if self.timer.get() != Some(&fired.id) {
            return Ok(());
        }
        if let Err(e) = self.sync().await {
            self.log.warn(format!("同步笔记失败：{e}")).await;
        }
        Ok(())
    }
}

pub struct NotesComponent;

impl Component for NotesComponent {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "notes",
            kind: Kind::Feature,
            summary: "把 Markdown 笔记同步进文档库",
            provides: vec![],
            requires: vec![
                FILES.id(),
                DOCUMENT_WRITE.id(),
                SQL.id(),
                MIGRATIONS.id(),
                TIMER.id(),
                LOG.id(),
            ],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let config: Config = config.parse()?;
        if config.poll_secs == 0 {
            return Err(AppError::config("poll_secs 不能是 0"));
        }
        if !config.summary_path.contains("{date}") {
            return Err(AppError::config("summary_path 里要有 {date}"));
        }
        host.contribute(
            &MIGRATIONS,
            Arc::new(StaticMigrations::new("notes", TABLES)),
        )?;
        let timer = host.resolve(&TIMER)?;
        let poll = config.poll_secs;
        let notes = Arc::new(Notes {
            files: host.resolve(&FILES)?,
            documents: host.resolve(&DOCUMENT_WRITE)?,
            sql: host.resolve(&SQL)?,
            log: Logger::new(host.resolve(&LOG)?, host.id()),
            timer: OnceLock::new(),
            config,
        });
        let n = notes.clone();
        host.subscribe(&TIMER_FIRED, "sync", move |fired| {
            let n = n.clone();
            async move { n.on_timer(fired).await }
        })?;
        host.on_ready(move || async move {
            let id = timer.every(poll * 1_000).await?;
            let _ = notes.timer.set(id);
            Ok(())
        });
        Ok(())
    }
}

register_component!(NotesComponent);
