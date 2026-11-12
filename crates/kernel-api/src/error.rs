//! 错误是数据：种类固定且少，可以序列化，可以跨进程。

use serde::{Deserialize, Serialize};

/// 预期内失败的种类。边界上的系统服务按它映射退出码、状态码、是否重试。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// 输入不合法。
    Validation,
    /// 目标不存在。
    NotFound,
    /// 并发修改冲突。
    VersionConflict,
    /// 没有权限。
    Unauthorized,
    /// 暂时失败，可以重试。
    TempFail,
    /// 提供者不可用，可以重试。
    Unavailable,
    /// 配置或装配错误。
    Config,
}

impl ErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Validation => "validation",
            Self::NotFound => "not_found",
            Self::VersionConflict => "version_conflict",
            Self::Unauthorized => "unauthorized",
            Self::TempFail => "temp_fail",
            Self::Unavailable => "unavailable",
            Self::Config => "config",
        }
    }

    /// 过一会儿再试有没有可能成功。
    pub const fn retryable(self) -> bool {
        matches!(self, Self::TempFail | Self::Unavailable)
    }
}

impl std::fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 预期内的失败。作为响应的一部分返回，不抛出。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{kind}：{message}")]
pub struct AppError {
    pub kind: ErrorKind,
    pub message: String,
}

impl AppError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Validation, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, message)
    }

    pub fn version_conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::VersionConflict, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unauthorized, message)
    }

    pub fn temp_fail(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::TempFail, message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unavailable, message)
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Config, message)
    }

    /// 在消息前面加一段上下文，种类不变。
    #[must_use]
    pub fn context(mut self, prefix: impl AsRef<str>) -> Self {
        self.message = format!("{}：{}", prefix.as_ref(), self.message);
        self
    }
}
