//! 监听点：谁请求了谁、发布了什么、投给了谁。
//!
//! 记录本身是纯数据，怎么输出由启动器决定。

use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::AppError;

/// 一条监听记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TraceRecord {
    /// 组件生命周期的一步：`start`、`ready`、`stop`。
    Lifecycle { component: String, phase: String },
    /// 一次请求。`outcome` 是 `ok` 或错误种类。
    Request {
        caller: String,
        provider: String,
        contract: String,
        method: String,
        outcome: String,
        micros: u64,
    },
    /// 一次发布。
    Publish { publisher: String, event: String },
    /// 一次投递。`outcome` 是 `ok` 或错误种类；失败时 `error` 是错误消息。
    Deliver {
        event: String,
        component: String,
        subscriber: String,
        outcome: String,
        error: Option<String>,
        micros: u64,
    },
}

/// 记录交给谁。由启动器提供。
pub trait TraceSink: Send + Sync + 'static {
    fn record(&self, record: TraceRecord);
}

/// 挂在一个句柄上的记录点：知道调用方和提供方是谁。
#[derive(Clone)]
pub struct Tracer {
    sink: Arc<dyn TraceSink>,
    caller: Arc<str>,
    provider: Arc<str>,
}

impl Tracer {
    pub fn new(sink: Arc<dyn TraceSink>, caller: &str, provider: &str) -> Self {
        Self {
            sink,
            caller: caller.into(),
            provider: provider.into(),
        }
    }

    /// 记一次请求。由 `contract!` 生成的包装调用。
    pub fn request<T>(
        &self,
        contract: &'static str,
        method: &'static str,
        started: Instant,
        result: &Result<T, AppError>,
    ) {
        self.sink.record(TraceRecord::Request {
            caller: self.caller.to_string(),
            provider: self.provider.to_string(),
            contract: contract.to_owned(),
            method: method.to_owned(),
            outcome: outcome_of(result).to_owned(),
            micros: micros_since(started),
        });
    }
}

/// 把结果变成记录里的 `outcome`。
pub(crate) fn outcome_of<T>(result: &Result<T, AppError>) -> &'static str {
    match result {
        Ok(_) => "ok",
        Err(e) => e.kind.as_str(),
    }
}

pub(crate) fn micros_since(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}
