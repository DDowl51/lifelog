//! 系统服务 `embed-fastembed`：在本机用 ONNX 模型提供 `Embedder`，内容不出本机。
//!
//! 模型第一次用到时才下载（从 HuggingFace）、加载，之后一直留在内存里：
//! 启动时不加载，所以下载慢或失败都不影响常驻进程启动，只是那一次报 `unavailable`、下次再试。
//! 模型自己的约定（E5 要给查询和段落加不同的前缀）在这里处理，使用方不用知道。

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use embedder_contract::{EMBEDDER, EmbedPurpose, Embedder, Embeddings};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use kernel_api::{
    AppError, Component, ConfigSection, Host, Kind, Manifest, async_trait, register_component,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
struct Config {
    /// `multilingual-e5-small`（约 470 MB，默认）、`multilingual-e5-base`、`bge-m3`（约 2.3 GB）。
    model: String,
    /// 模型放在哪。
    cache_dir: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: "multilingual-e5-small".to_owned(),
            cache_dir: "~/.cache/lifelog/models".to_owned(),
        }
    }
}

/// 认识的模型：（配置里的名字，fastembed 的型号，查询前缀，段落前缀）。
const MODELS: &[(&str, EmbeddingModel, &str, &str)] = &[
    (
        "multilingual-e5-small",
        EmbeddingModel::MultilingualE5Small,
        "query: ",
        "passage: ",
    ),
    (
        "multilingual-e5-base",
        EmbeddingModel::MultilingualE5Base,
        "query: ",
        "passage: ",
    ),
    ("bge-m3", EmbeddingModel::BGEM3, "", ""),
];

fn find(name: &str) -> Option<&'static (&'static str, EmbeddingModel, &'static str, &'static str)> {
    MODELS.iter().find(|(n, ..)| *n == name)
}

/// 加上模型要的前缀。
fn prepare(texts: Vec<String>, purpose: EmbedPurpose, query: &str, passage: &str) -> Vec<String> {
    let prefix = match purpose {
        EmbedPurpose::Query => query,
        EmbedPurpose::Passage => passage,
    };
    texts.into_iter().map(|t| format!("{prefix}{t}")).collect()
}

/// 归一化（长度 1），这样余弦相似度就是点积。
fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
    v
}

fn expand(path: &str) -> Result<PathBuf, AppError> {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(rest))
            .ok_or_else(|| AppError::config(format!("`{path}` 用了 ~，但环境里没有 HOME"))),
        None => Ok(PathBuf::from(path)),
    }
}

struct Local {
    name: &'static str,
    model: EmbeddingModel,
    prefixes: (&'static str, &'static str),
    cache_dir: PathBuf,
    /// 第一次用到时才加载。
    loaded: Arc<Mutex<Option<TextEmbedding>>>,
}

#[async_trait]
impl Embedder for Local {
    async fn embed(
        &self,
        texts: Vec<String>,
        purpose: EmbedPurpose,
    ) -> Result<Embeddings, AppError> {
        if texts.is_empty() {
            return Ok(Embeddings {
                vectors: Vec::new(),
                model: self.name.to_owned(),
            });
        }
        let texts = prepare(texts, purpose, self.prefixes.0, self.prefixes.1);
        let (loaded, model, cache_dir, name) = (
            self.loaded.clone(),
            self.model.clone(),
            self.cache_dir.clone(),
            self.name,
        );
        let vectors = tokio::task::spawn_blocking(move || -> Result<Vec<Vec<f32>>, AppError> {
            let mut slot = loaded.lock().unwrap_or_else(PoisonError::into_inner);
            if slot.is_none() {
                let options = InitOptions::new(model)
                    .with_cache_dir(cache_dir)
                    .with_show_download_progress(false);
                let model = TextEmbedding::try_new(options).map_err(|e| {
                    AppError::unavailable(format!("下载或加载模型 {name} 失败（下次再试）：{e}"))
                })?;
                *slot = Some(model);
            }
            let Some(model) = slot.as_mut() else {
                return Err(AppError::unavailable("模型没加载上"));
            };
            model
                .embed(texts, None)
                .map_err(|e| AppError::unavailable(format!("用模型 {name} 算向量失败：{e}")))
        })
        .await
        .map_err(|e| AppError::unavailable(format!("算向量的线程崩了：{e}")))??;
        Ok(Embeddings {
            vectors: vectors.into_iter().map(normalize).collect(),
            model: self.name.to_owned(),
        })
    }
}

pub struct EmbedFastembed;

impl Component for EmbedFastembed {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "embed-fastembed",
            kind: Kind::Service,
            summary: "在本机用 ONNX 模型把文字变成向量",
            provides: vec![EMBEDDER.id()],
            requires: vec![],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let config: Config = config.parse()?;
        let (name, model, query, passage) = find(&config.model).ok_or_else(|| {
            let known: Vec<&str> = MODELS.iter().map(|(n, ..)| *n).collect();
            AppError::config(format!(
                "不认识模型 `{}`，可以用：{}",
                config.model,
                known.join("、")
            ))
        })?;
        let local = Local {
            name,
            model: model.clone(),
            prefixes: (query, passage),
            cache_dir: expand(&config.cache_dir)?,
            loaded: Arc::default(),
        };
        host.provide(&EMBEDDER, Arc::new(local))
    }
}

register_component!(EmbedFastembed);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_follow_the_model() {
        let (_, _, q, p) = find("multilingual-e5-small").unwrap_or(&MODELS[0]);
        assert_eq!(
            prepare(vec!["气象站".to_owned()], EmbedPurpose::Query, q, p),
            ["query: 气象站"]
        );
        assert_eq!(
            prepare(vec!["气象站".to_owned()], EmbedPurpose::Passage, q, p),
            ["passage: 气象站"]
        );
        let (_, _, q, p) = find("bge-m3").unwrap_or(&MODELS[0]);
        assert_eq!(
            prepare(vec!["x".to_owned()], EmbedPurpose::Query, q, p),
            ["x"]
        );
        assert!(find("gpt-embed").is_none());
    }

    #[test]
    fn vectors_are_normalized() {
        let v = normalize(vec![3.0, 4.0]);
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
        assert_eq!(normalize(vec![0.0, 0.0]), [0.0, 0.0]);
    }
}
