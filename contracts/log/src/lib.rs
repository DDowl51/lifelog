//! 契约：写一条日志。
//!
//! 组件不直接打印，日志都经过这个契约；写到哪、写多细由提供者的配置决定。

use std::sync::Arc;

use kernel_api::{AppError, contract};
use serde::{Deserialize, Serialize};

/// 日志级别，从细到粗。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// 一条日志。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub level: Level,
    /// 谁写的：组件 id。
    pub component: String,
    pub message: String,
}

contract! {
    /// 写日志。
    pub trait Log as LOG = service("log", 1) {
        /// 写一条。级别低于提供者配置的门槛时直接丢弃，也算成功。
        async fn write(&self, entry: Entry) -> Result<(), AppError>;
    }
}

/// 调用方的便利写法：记住自己是谁，省得每次填 `Entry`。不属于契约。
///
/// 日志写不出去时不报错：没有别处可报，也不该因此让业务失败。
#[derive(Clone)]
pub struct Logger {
    log: Arc<dyn Log>,
    component: String,
}

impl Logger {
    pub fn new(log: Arc<dyn Log>, component: &str) -> Self {
        Self {
            log,
            component: component.to_owned(),
        }
    }

    async fn write(&self, level: Level, message: String) {
        let entry = Entry {
            level,
            component: self.component.clone(),
            message,
        };
        let _ = self.log.write(entry).await;
    }

    pub async fn debug(&self, message: impl Into<String>) {
        self.write(Level::Debug, message.into()).await;
    }

    pub async fn info(&self, message: impl Into<String>) {
        self.write(Level::Info, message.into()).await;
    }

    pub async fn warn(&self, message: impl Into<String>) {
        self.write(Level::Warn, message.into()).await;
    }

    pub async fn error(&self, message: impl Into<String>) {
        self.write(Level::Error, message.into()).await;
    }
}

#[cfg(any(test, feature = "testing"))]
pub mod testing {
    //! 收在内存里的日志和契约测试。

    use std::sync::{Arc, Mutex, PoisonError};

    use kernel_api::{AppError, Component, ConfigSection, Host, Kind, Manifest, async_trait};

    use super::{Entry, LOG, Level, Log};

    /// 把日志收在内存里。克隆出来的是同一份。
    #[derive(Clone, Default)]
    pub struct MemoryLog(Arc<Mutex<Vec<Entry>>>);

    impl MemoryLog {
        pub fn entries(&self) -> Vec<Entry> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        /// 写成 `级别 组件: 消息`，方便断言。
        pub fn lines(&self) -> Vec<String> {
            self.entries()
                .into_iter()
                .map(|e| format!("{} {}: {}", e.level.as_str(), e.component, e.message))
                .collect()
        }
    }

    #[async_trait]
    impl Log for MemoryLog {
        async fn write(&self, entry: Entry) -> Result<(), AppError> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(entry);
            Ok(())
        }
    }

    /// 组件 `log-memory`。
    impl Component for MemoryLog {
        fn manifest(&self) -> Manifest {
            Manifest {
                id: "log-memory",
                kind: Kind::Service,
                summary: "测试用：把日志收在内存里",
                provides: vec![LOG.id()],
                requires: vec![],
                optional_requires: vec![],
            }
        }

        fn start(&self, _config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
            host.provide(&LOG, Arc::new(self.clone()))
        }
    }

    /// `Log` 的契约测试。每个实现都要跑。
    pub async fn log_contract(log: Arc<dyn Log>) -> Result<(), AppError> {
        for level in [Level::Debug, Level::Info, Level::Warn, Level::Error] {
            let entry = Entry {
                level,
                component: "contract-test".to_owned(),
                message: "多行\n和 unicode ✓ 都要能写".to_owned(),
            };
            log.write(entry).await?;
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::super::Logger;
        use super::*;

        #[tokio::test]
        async fn memory_log_obeys_the_contract() {
            log_contract(Arc::new(MemoryLog::default())).await.unwrap();
        }

        #[tokio::test]
        async fn logger_fills_in_the_component() {
            let log = MemoryLog::default();
            let logger = Logger::new(Arc::new(log.clone()), "focus");
            logger.warn("断线了").await;
            assert_eq!(log.lines(), ["warn focus: 断线了"]);
        }
    }
}
