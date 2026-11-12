//! 系统服务 `llm-openai-compat`：用 OpenAI 兼容的 `/chat/completions` 接口提供 `Llm`。
//!
//! 服务商、模型、key 都是配置。key 不要写进配置文件，三种给法：
//!
//! - `key_command`：第一次调用时才运行（比如 `pass show api/llm`），结果记在内存里。
//!   推荐：gpg 还没解锁时只影响摘要，不影响常驻进程启动。
//! - `api_key_cmd`：启动库在进程启动时运行，失败则整个进程起不来。
//! - `api_key_env`：环境变量。
//!
//! 没配好时进程照常启动，调用时才报 `unavailable`：摘要是可选的，不该拖住常驻进程。

use std::sync::Arc;
use std::time::Duration;

use kernel_api::{
    AppError, Component, ConfigSection, ErrorKind, Host, Kind, Manifest, async_trait,
    register_component,
};
use llm_contract::{Completion, CompletionRequest, LLM, Llm, Role};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
struct Config {
    /// 接口地址，到 `/v1` 为止，例如 `https://api.deepseek.com/v1`。
    base_url: String,
    model: Option<String>,
    /// key 本身。最好用 `api_key_cmd`（启动库把命令的输出填到这里），或者 `api_key_env`。
    api_key: Option<String>,
    /// 从这个环境变量读 key。
    api_key_env: Option<String>,
    /// 第一次调用时运行这条命令（`sh -c`），输出就是 key。
    key_command: Option<String>,
    /// 一次请求最多等多久。
    timeout_secs: u64,
    /// 默认最多生成多少 token。
    max_tokens: u32,
    /// 越低越稳定。
    temperature: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_owned(),
            model: None,
            api_key: None,
            api_key_env: None,
            key_command: None,
            timeout_secs: 180,
            max_tokens: 2_000,
            temperature: 0.3,
        }
    }
}

struct OpenAi {
    url: String,
    model: Option<String>,
    key: std::sync::Mutex<Option<String>>,
    key_command: Option<String>,
    timeout: Duration,
    max_tokens: u32,
    temperature: f64,
}

fn role(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

/// HTTP 状态码 → 错误种类。
fn kind_of(status: u16) -> ErrorKind {
    match status {
        401 | 403 => ErrorKind::Unauthorized,
        429 => ErrorKind::TempFail,
        400..=499 => ErrorKind::Validation,
        _ => ErrorKind::Unavailable,
    }
}

/// 错误响应里服务商给的说明（`{"error":{"message":…}}`），没有就用整段。
fn provider_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| body.chars().take(300).collect())
}

/// 运行取 key 的命令。
fn run_key_command(command: &str) -> Result<String, AppError> {
    let out = std::process::Command::new("sh")
        .args(["-c", command])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| AppError::unavailable(format!("运行 key_command 失败：{e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(AppError::unavailable(format!(
            "key_command 以 {} 退出：{}",
            out.status,
            stderr.trim().chars().take(200).collect::<String>()
        )));
    }
    let key = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if key.is_empty() {
        return Err(AppError::unavailable("key_command 没有输出"));
    }
    Ok(key)
}

impl OpenAi {
    /// 拿 key：配置里有就用；否则第一次调用时运行 `key_command`，之后用记下的。
    async fn key(&self) -> Result<String, AppError> {
        if let Some(key) = self
            .key
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            return Ok(key);
        }
        let Some(command) = self.key_command.clone() else {
            return Err(AppError::unavailable(
                "没有配置 key：[components.llm-openai-compat] key_command = \"pass show …\"",
            ));
        };
        let key = tokio::task::spawn_blocking(move || run_key_command(&command))
            .await
            .map_err(|e| AppError::unavailable(format!("取 key 的线程崩了：{e}")))??;
        *self
            .key
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(key.clone());
        Ok(key)
    }

    fn body(&self, model: &str, request: &CompletionRequest) -> Value {
        json!({
            "model": model,
            "messages": request.messages.iter()
                .map(|m| json!({ "role": role(m.role), "content": m.content }))
                .collect::<Vec<_>>(),
            "max_tokens": request.max_tokens.unwrap_or(self.max_tokens),
            "temperature": self.temperature,
            "stream": false,
        })
    }

    /// 阻塞的那一半：发请求、读响应。
    fn send(url: &str, key: &str, timeout: Duration, body: &Value) -> Result<Value, AppError> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .build()
            .into();
        let mut response = agent
            .post(url)
            .header("Authorization", &format!("Bearer {key}"))
            .send_json(body)
            .map_err(|e| AppError::unavailable(format!("连不上模型接口 {url}：{e}")))?;
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|e| AppError::unavailable(format!("读模型接口的响应失败：{e}")))?;
        if !(200..300).contains(&status) {
            return Err(AppError::new(
                kind_of(status),
                format!("模型接口返回 {status}：{}", provider_message(&text)),
            ));
        }
        serde_json::from_str(&text)
            .map_err(|e| AppError::unavailable(format!("模型接口的响应不是 JSON：{e}")))
    }
}

#[async_trait]
impl Llm for OpenAi {
    async fn complete(&self, request: CompletionRequest) -> Result<Completion, AppError> {
        let model = self.model.clone().ok_or_else(|| {
            AppError::unavailable("没有配置模型：[components.llm-openai-compat] model = \"…\"")
        })?;
        let key = self.key().await?;
        let body = self.body(&model, &request);
        let (url, timeout) = (self.url.clone(), self.timeout);
        let value = tokio::task::spawn_blocking(move || Self::send(&url, &key, timeout, &body))
            .await
            .map_err(|e| AppError::unavailable(format!("调用模型的线程崩了：{e}")))??;
        let text = value
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AppError::unavailable("模型接口的响应里没有 choices[0].message.content")
            })?
            .to_owned();
        let tokens = |name: &str| {
            value
                .pointer(&format!("/usage/{name}"))
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
        };
        Ok(Completion {
            text,
            model: value
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or(&model)
                .to_owned(),
            prompt_tokens: tokens("prompt_tokens"),
            completion_tokens: tokens("completion_tokens"),
        })
    }
}

pub struct LlmOpenAiCompat;

impl Component for LlmOpenAiCompat {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "llm-openai-compat",
            kind: Kind::Service,
            summary: "用 OpenAI 兼容的接口提供语言模型",
            provides: vec![LLM.id()],
            requires: vec![],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let config: Config = config.parse()?;
        if config.timeout_secs == 0 {
            return Err(AppError::config("timeout_secs 不能是 0"));
        }
        let key = config
            .api_key
            .or_else(|| config.api_key_env.and_then(|name| std::env::var(name).ok()))
            .map(|k| k.trim().to_owned())
            .filter(|k| !k.is_empty());
        let llm = OpenAi {
            url: format!("{}/chat/completions", config.base_url.trim_end_matches('/')),
            model: config.model,
            key: std::sync::Mutex::new(key),
            key_command: config.key_command,
            timeout: Duration::from_secs(config.timeout_secs),
            max_tokens: config.max_tokens,
            temperature: config.temperature,
        };
        host.provide(&LLM, Arc::new(llm))
    }
}

register_component!(LlmOpenAiCompat);
