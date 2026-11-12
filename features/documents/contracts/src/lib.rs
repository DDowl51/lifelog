//! `documents` 对外的契约：长文本（每日摘要、笔记）的存取。
//!
//! 一篇文档有一个键（比如 `summary_daily:2026-11-12`），同一个键再写就是覆盖。
//! 每次写入或删除都会得到一个递增的修订号，给「按变化往下拉」的使用方（建索引）用。

use kernel_api::{AppError, contract};
use serde::{Deserialize, Serialize};

/// 一篇文档。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    /// 唯一的键。
    pub key: String,
    /// 种类，例如 `summary_daily`、`note`。
    pub kind: String,
    /// 说的是哪一天（`YYYY-MM-DD`），没有就是 `None`。
    #[serde(default)]
    pub date: Option<String>,
    pub title: String,
    /// 正文，通常是 Markdown。
    pub body: String,
    /// 从哪来的：比如文件的相对路径。
    #[serde(default)]
    pub origin: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DocumentId(pub i64);

/// 存下来的文档。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredDocument {
    pub id: DocumentId,
    pub document: Document,
    /// 这次写入或删除的修订号，全局递增。
    pub revision: i64,
    /// 最后一次写入的时刻，UTC 毫秒。
    pub updated_at: i64,
    /// 被删掉了（只有 `changed_since` 会给出删掉的）。
    pub deleted: bool,
}

contract! {
    /// 写文档。
    pub trait DocumentWrite as DOCUMENT_WRITE = service("documents.write", 1) {
        /// 写入或覆盖（按 `key`）。返回它的编号。
        async fn put(&self, document: Document) -> Result<DocumentId, AppError>;

        /// 删掉。本来就没有返回 `false`。
        async fn remove(&self, key: String) -> Result<bool, AppError>;
    }
}

contract! {
    /// 读文档。
    pub trait DocumentRead as DOCUMENT_READ = service("documents.read", 1) {
        /// 按键取；没有或已删是 `None`。
        async fn get(&self, key: String) -> Result<Option<StoredDocument>, AppError>;

        /// 某种类的全部文档（不含已删的），按日期、键排序。
        async fn list(&self, kind: String) -> Result<Vec<StoredDocument>, AppError>;

        /// 修订号大于 `after` 的（含已删的），按修订号升序，最多 `limit` 条。
        async fn changed_since(&self, after: i64, limit: u32) -> Result<Vec<StoredDocument>, AppError>;
    }
}
