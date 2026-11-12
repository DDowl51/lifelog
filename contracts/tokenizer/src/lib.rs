//! 契约：把文字切成可以检索的词。
//!
//! 建索引和查询必须用同一个实现切，否则切出来的词对不上。

use kernel_api::{AppError, contract};
use serde::{Deserialize, Serialize};

/// 切来做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    /// 建索引：尽量多切出可能被搜到的词。
    Index,
    /// 查询：切出的每个词都要在索引里出现才算命中。
    Query,
}

contract! {
    /// 分词。
    pub trait Tokenizer as TOKENIZER = service("tokenizer", 1) {
        /// 一批文字，各切成一组词。返回值和输入一一对应。
        async fn tokenize(&self, texts: Vec<String>, purpose: Purpose) -> Result<Vec<Vec<String>>, AppError>;
    }
}

#[cfg(feature = "testing")]
pub mod testing {
    //! 契约测试。

    use std::sync::Arc;

    use kernel_api::AppError;

    use super::{Purpose, Tokenizer};

    /// 每个实现都要满足的：查询切出的词都能在同一段文字的索引词里找到；空文字没有词；一一对应。
    pub async fn tokenizer_contract(tokenizer: Arc<dyn Tokenizer>) -> Result<(), AppError> {
        let texts: Vec<String> = [
            "Rust 程序设计语言 — docs.rs",
            "cargo test --workspace",
            "气象站固件调试",
            "",
            "   ",
        ]
        .map(str::to_owned)
        .to_vec();
        let index = tokenizer.tokenize(texts.clone(), Purpose::Index).await?;
        let query = tokenizer.tokenize(texts.clone(), Purpose::Query).await?;
        assert_eq!(index.len(), texts.len());
        assert_eq!(query.len(), texts.len());
        for ((text, index), query) in texts.iter().zip(&index).zip(&query) {
            for word in query {
                assert!(
                    index.contains(word),
                    "{text:?}：查询词 {word:?} 不在索引词 {index:?} 里"
                );
            }
            if text.trim().is_empty() {
                assert!(index.is_empty() && query.is_empty(), "空文字不该有词");
            } else {
                assert!(!query.is_empty(), "{text:?} 什么也没切出来");
            }
        }
        Ok(())
    }
}
