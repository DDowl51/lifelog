//! 契约：把文字变成向量（嵌入）。
//!
//! 意思相近的文字，向量的方向也相近。用哪个模型是实现的配置；同一批向量只能和同一个模型算出来的比较。

use kernel_api::{AppError, contract};
use serde::{Deserialize, Serialize};

/// 这段文字是拿来找的，还是被找的。有的模型对两者处理不同（比如加不同的前缀）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbedPurpose {
    Query,
    Passage,
}

/// 一批向量。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Embeddings {
    /// 和输入一一对应，都已经归一化（长度为 1），余弦相似度就是点积。
    pub vectors: Vec<Vec<f32>>,
    /// 模型的名字。换了模型，旧的向量就不能用了。
    pub model: String,
}

contract! {
    /// 嵌入模型。
    pub trait Embedder as EMBEDDER = service("embedder", 1) {
        /// 模型还没下载好、加载失败是 `unavailable`。
        async fn embed(&self, texts: Vec<String>, purpose: EmbedPurpose) -> Result<Embeddings, AppError>;
    }
}

#[cfg(feature = "testing")]
pub mod testing {
    //! 不用模型的假实现：按字符哈希到固定维度，再归一化。
    //! 结果确定、可重复；共享的字符越多越相近，够测「找得到 / 排得对」。

    use std::sync::Arc;

    use kernel_api::{AppError, Component, ConfigSection, Host, Kind, Manifest, async_trait};

    use super::{EMBEDDER, EmbedPurpose, Embedder, Embeddings};

    const DIM: usize = 64;

    pub fn vector(text: &str) -> Vec<f32> {
        let mut v = vec![0f32; DIM];
        for c in text.to_lowercase().chars().filter(|c| !c.is_whitespace()) {
            let slot = (c as usize).wrapping_mul(2_654_435_761) % DIM;
            v[slot] += 1.0;
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            v.iter_mut().for_each(|x| *x /= norm);
        }
        v
    }

    /// 组件 `embedder-hash`。
    #[derive(Clone, Default)]
    pub struct HashEmbedder;

    #[async_trait]
    impl Embedder for HashEmbedder {
        async fn embed(
            &self,
            texts: Vec<String>,
            _purpose: EmbedPurpose,
        ) -> Result<Embeddings, AppError> {
            Ok(Embeddings {
                vectors: texts.iter().map(|t| vector(t)).collect(),
                model: "hash-64".to_owned(),
            })
        }
    }

    impl Component for HashEmbedder {
        fn manifest(&self) -> Manifest {
            Manifest {
                id: "embedder-hash",
                kind: Kind::Service,
                summary: "测试用：按字符哈希的假嵌入",
                provides: vec![EMBEDDER.id()],
                requires: vec![],
                optional_requires: vec![],
            }
        }

        fn start(&self, _config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
            host.provide(&EMBEDDER, Arc::new(self.clone()))
        }
    }
}
