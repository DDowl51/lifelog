//! 应用功能 `documents`：长文本（摘要、笔记）的存取。表和 SQL 都归这个组件。

use std::sync::Arc;

use clock_contract::{CLOCK, Clock};
use documents_contracts::{
    DOCUMENT_READ, DOCUMENT_WRITE, Document, DocumentId, DocumentRead, DocumentWrite,
    StoredDocument,
};
use kernel_api::{
    AppError, Component, ConfigSection, Host, Kind, Manifest, NoConfig, async_trait,
    register_component,
};
use sql_contract::{MIGRATIONS, SQL, Sql, SqlValue, Statement, StaticMigrations};

/// 已经执行过的迁移不能改。改表结构在末尾追加。
const TABLES: &[(&str, &str)] = &[(
    "文档表",
    "CREATE TABLE documents_items (
        id INTEGER PRIMARY KEY,
        key TEXT NOT NULL UNIQUE,
        kind TEXT NOT NULL,
        date TEXT,
        title TEXT NOT NULL,
        body TEXT NOT NULL,
        origin TEXT,
        revision INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        deleted INTEGER NOT NULL DEFAULT 0
     ) STRICT;
     CREATE INDEX documents_items_revision ON documents_items(revision);
     CREATE INDEX documents_items_kind ON documents_items(kind, date);",
)];

const COLUMNS: &str = "id, key, kind, date, title, body, origin, revision, updated_at, deleted";
const NEXT_REVISION: &str = "(SELECT coalesce(max(revision), 0) + 1 FROM documents_items)";

fn corrupt(column: &str) -> AppError {
    AppError::unavailable(format!("documents_items 的 {column} 列读出来不对"))
}

fn text(v: &SqlValue, column: &str) -> Result<String, AppError> {
    v.as_str().map(str::to_owned).ok_or_else(|| corrupt(column))
}

fn opt_text(v: &SqlValue, column: &str) -> Result<Option<String>, AppError> {
    if v.is_null() {
        Ok(None)
    } else {
        text(v, column).map(Some)
    }
}

fn int(v: &SqlValue, column: &str) -> Result<i64, AppError> {
    v.as_i64().ok_or_else(|| corrupt(column))
}

fn stored(row: &[SqlValue]) -> Result<StoredDocument, AppError> {
    let [
        id,
        key,
        kind,
        date,
        title,
        body,
        origin,
        revision,
        updated_at,
        deleted,
    ] = row
    else {
        return Err(corrupt("列数"));
    };
    Ok(StoredDocument {
        id: DocumentId(int(id, "id")?),
        document: Document {
            key: text(key, "key")?,
            kind: text(kind, "kind")?,
            date: opt_text(date, "date")?,
            title: text(title, "title")?,
            body: text(body, "body")?,
            origin: opt_text(origin, "origin")?,
        },
        revision: int(revision, "revision")?,
        updated_at: int(updated_at, "updated_at")?,
        deleted: int(deleted, "deleted")? != 0,
    })
}

struct Store {
    sql: Arc<dyn Sql>,
    clock: Arc<dyn Clock>,
}

impl Store {
    async fn rows(&self, statement: Statement) -> Result<Vec<StoredDocument>, AppError> {
        let rows = self.sql.query(statement).await?;
        rows.rows.iter().map(|r| stored(r)).collect()
    }
}

#[async_trait]
impl DocumentWrite for Store {
    async fn put(&self, document: Document) -> Result<DocumentId, AppError> {
        if document.key.trim().is_empty() || document.kind.trim().is_empty() {
            return Err(AppError::validation("文档的 key 和 kind 不能是空的"));
        }
        let now = self.clock.now().await?.utc_ms;
        let sql = format!(
            "INSERT INTO documents_items (key, kind, date, title, body, origin, revision, updated_at, deleted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, {NEXT_REVISION}, ?7, 0)
             ON CONFLICT(key) DO UPDATE SET kind = excluded.kind, date = excluded.date,
                 title = excluded.title, body = excluded.body, origin = excluded.origin,
                 revision = excluded.revision, updated_at = excluded.updated_at, deleted = 0
             RETURNING id"
        );
        let results = self
            .sql
            .execute(vec![
                Statement::new(sql)
                    .bind(document.key)
                    .bind(document.kind)
                    .bind(document.date)
                    .bind(document.title)
                    .bind(document.body)
                    .bind(document.origin)
                    .bind(now),
            ])
            .await?;
        results
            .first()
            .and_then(|r| r.rows.first())
            .and_then(|r| r.first())
            .and_then(SqlValue::as_i64)
            .map(DocumentId)
            .ok_or_else(|| corrupt("id"))
    }

    async fn remove(&self, key: String) -> Result<bool, AppError> {
        let now = self.clock.now().await?.utc_ms;
        let sql = format!(
            "UPDATE documents_items SET deleted = 1, body = '', revision = {NEXT_REVISION}, updated_at = ?2
             WHERE key = ?1 AND deleted = 0"
        );
        let results = self
            .sql
            .execute(vec![Statement::new(sql).bind(key).bind(now)])
            .await?;
        Ok(results.first().is_some_and(|r| r.affected > 0))
    }
}

#[async_trait]
impl DocumentRead for Store {
    async fn get(&self, key: String) -> Result<Option<StoredDocument>, AppError> {
        let sql = format!("SELECT {COLUMNS} FROM documents_items WHERE key = ?1 AND deleted = 0");
        Ok(self.rows(Statement::new(sql).bind(key)).await?.pop())
    }

    async fn list(&self, kind: String) -> Result<Vec<StoredDocument>, AppError> {
        let sql = format!(
            "SELECT {COLUMNS} FROM documents_items WHERE kind = ?1 AND deleted = 0 ORDER BY date, key"
        );
        self.rows(Statement::new(sql).bind(kind)).await
    }

    async fn changed_since(&self, after: i64, limit: u32) -> Result<Vec<StoredDocument>, AppError> {
        let sql = format!(
            "SELECT {COLUMNS} FROM documents_items WHERE revision > ?1 ORDER BY revision LIMIT ?2"
        );
        self.rows(Statement::new(sql).bind(after).bind(i64::from(limit)))
            .await
    }
}

pub struct DocumentsComponent;

impl Component for DocumentsComponent {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<NoConfig>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "documents",
            kind: Kind::Feature,
            summary: "长文本（摘要、笔记）的存取",
            provides: vec![DOCUMENT_WRITE.id(), DOCUMENT_READ.id()],
            requires: vec![SQL.id(), MIGRATIONS.id(), CLOCK.id()],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        config.parse::<NoConfig>()?;
        host.contribute(
            &MIGRATIONS,
            Arc::new(StaticMigrations::new("documents", TABLES)),
        )?;
        let store = Arc::new(Store {
            sql: host.resolve(&SQL)?,
            clock: host.resolve(&CLOCK)?,
        });
        host.provide(&DOCUMENT_WRITE, store.clone())?;
        host.provide(&DOCUMENT_READ, store)
    }
}

register_component!(DocumentsComponent);
