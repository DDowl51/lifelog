//! 契约：让语言模型补全一段对话。
//!
//! 契约里没有服务商、模型名：那些是实现的配置。换一家（或者换成本机模型）= 换一个提供这个契约的组件。

use kernel_api::{AppError, contract};
use serde::{Deserialize, Serialize};

/// 一条消息是谁说的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// 给模型的说明。
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }
}

/// 一次补全。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub messages: Vec<ChatMessage>,
    /// 最多生成多少 token；`None` 用实现的默认值。
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

/// 补全的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Completion {
    pub text: String,
    /// 实际用的模型（给人看、记日志用）。
    pub model: String,
    #[serde(default)]
    pub prompt_tokens: Option<u32>,
    #[serde(default)]
    pub completion_tokens: Option<u32>,
}

contract! {
    /// 语言模型。
    pub trait Llm as LLM = service("llm", 1) {
        /// 接口没配好、连不上是 `unavailable`；没有权限是 `unauthorized`；被限流是 `temp_fail`。
        async fn complete(&self, request: CompletionRequest) -> Result<Completion, AppError>;
    }
}

#[cfg(feature = "testing")]
pub mod testing {
    //! 按脚本回答的假模型。

    use std::sync::{Arc, Mutex, PoisonError};

    use kernel_api::{AppError, Component, ConfigSection, Host, Kind, Manifest, async_trait};

    use super::{Completion, CompletionRequest, LLM, Llm};

    /// 每次都回同一段话，并记下收到的请求。克隆出来的是同一个。
    #[derive(Clone)]
    pub struct ScriptedLlm {
        reply: Arc<Mutex<Result<String, AppError>>>,
        seen: Arc<Mutex<Vec<CompletionRequest>>>,
    }

    impl ScriptedLlm {
        pub fn replying(text: &str) -> Self {
            Self {
                reply: Arc::new(Mutex::new(Ok(text.to_owned()))),
                seen: Arc::default(),
            }
        }

        /// 以后的回答改成这个（可以是错误）。
        pub fn set(&self, reply: Result<String, AppError>) {
            *self.reply.lock().unwrap_or_else(PoisonError::into_inner) = reply;
        }

        /// 收到过的请求，按先后。
        pub fn seen(&self) -> Vec<CompletionRequest> {
            self.seen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }
    }

    #[async_trait]
    impl Llm for ScriptedLlm {
        async fn complete(&self, request: CompletionRequest) -> Result<Completion, AppError> {
            self.seen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(request);
            let reply = self
                .reply
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()?;
            Ok(Completion {
                text: reply,
                model: "scripted".to_owned(),
                prompt_tokens: None,
                completion_tokens: None,
            })
        }
    }

    /// 组件 `llm-scripted`。
    impl Component for ScriptedLlm {
        fn manifest(&self) -> Manifest {
            Manifest {
                id: "llm-scripted",
                kind: Kind::Service,
                summary: "测试用：按脚本回答的模型",
                provides: vec![LLM.id()],
                requires: vec![],
                optional_requires: vec![],
            }
        }

        fn start(&self, _config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
            host.provide(&LLM, Arc::new(self.clone()))
        }
    }
}
