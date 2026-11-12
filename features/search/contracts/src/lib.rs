//! `search` 对外的契约：在记下的活动（窗口标题、命令、网页）和文档（摘要、笔记）里检索。
//!
//! 两类结果分开排：活动是短文本，最近的在前；文档是长文本，按相关度（全文，
//! 有向量检索时再和向量结果合并）。按来源过滤时，文档的来源名是 [`DOCUMENTS`]。

use kernel_api::{AppError, contract};
use serde::{Deserialize, Serialize};

/// 一次检索。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchQuery {
    /// 要找的词，空格分开；每个词都要出现。
    pub text: String,
    /// 只找 `[since, until)` 之间的，UTC 毫秒；`None` 表示不限。
    #[serde(default)]
    pub since: Option<i64>,
    #[serde(default)]
    pub until: Option<i64>,
    /// 只找这些来源的；空表示不限。
    #[serde(default)]
    pub sources: Vec<String>,
    /// 不找这些来源的（隐藏规则用）。
    #[serde(default)]
    pub exclude_sources: Vec<String>,
    /// 最多返回几条（合并之后）。
    pub limit: u32,
}

/// 一条结果。同一来源里标题和网址都一样的活动合成一条。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hit {
    pub source: String,
    pub kind: String,
    /// 最近一次的时刻，UTC 毫秒。
    pub at: i64,
    pub tz_offset_min: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// 合进来了几条活动。
    pub count: u32,
}

/// 按来源过滤时文档用的来源名。
pub const DOCUMENTS: &str = "documents";

/// 一篇命中的文档。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentHit {
    pub key: String,
    /// 种类：`summary_daily`、`note`……
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    pub title: String,
    /// 正文里命中处附近的一段。
    pub snippet: String,
}

/// 检索结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    /// 最近的在前。
    pub hits: Vec<Hit>,
    /// 命中的文档，最相关的在前。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub documents: Vec<DocumentHit>,
    /// 还有更多没给出来（加大 `limit` 或缩小范围）。
    pub truncated: bool,
}

contract! {
    /// 检索。
    pub trait Search as SEARCH = service("search", 1) {
        /// 没有可找的词时返回 `validation`。
        async fn search(&self, query: SearchQuery) -> Result<SearchResult, AppError>;
    }
}
