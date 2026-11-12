//! `vectors` 对外的契约：按意思找文档（向量检索）。
//!
//! 只覆盖文档（摘要、笔记）：短的窗口标题和命令用全文检索就够了。
//! 查询要先算出查询本身的向量，所以要用到嵌入模型；模型一直留在常驻进程里，
//! 命令行进程通过跨进程代理调用这个契约。

use kernel_api::{AppError, contract};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VectorQuery {
    pub text: String,
    /// 最多几篇文档。
    pub limit: u32,
}

/// 一篇文档里和查询意思最近的那一段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorHit {
    /// 文档的键。
    pub key: String,
    /// 相似度，越大越近（余弦，-1～1）。
    pub score: f64,
    /// 最近的那一段原文。
    pub chunk: String,
}

contract! {
    /// 向量检索。
    pub trait VectorSearch as VECTOR_SEARCH = service("vectors.search", 1) {
        /// 最相近的在前，每篇文档一条。
        async fn search(&self, query: VectorQuery) -> Result<Vec<VectorHit>, AppError>;
    }
}
