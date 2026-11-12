//! 系统服务 `log-stderr`：把日志按行写到 stderr。
//!
//! 不加时间戳：常驻进程的 stderr 由 journald 接管，它会加。

use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError};

use kernel_api::{
    AppError, Component, ConfigSection, Host, Kind, Manifest, async_trait, register_component,
};
use log_contract::{Entry, LOG, Level, Log};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
struct Config {
    /// 低于这个级别的不写。
    level: Level,
}

impl Default for Config {
    fn default() -> Self {
        Self { level: Level::Info }
    }
}

/// 一条日志写成一行。消息里的换行缩进续行，保证每条日志的第一行都以级别开头。
fn format(entry: &Entry) -> String {
    let message = entry.message.replace('\n', "\n    ");
    format!(
        "{:<5} {}: {message}\n",
        entry.level.as_str().to_uppercase(),
        entry.component
    )
}

struct Writer<W> {
    min: Level,
    out: Mutex<W>,
}

#[async_trait]
impl<W: Write + Send + 'static> Log for Writer<W> {
    async fn write(&self, entry: Entry) -> Result<(), AppError> {
        if entry.level < self.min {
            return Ok(());
        }
        let mut out = self.out.lock().unwrap_or_else(PoisonError::into_inner);
        out.write_all(format(&entry).as_bytes())
            .map_err(|e| AppError::unavailable(format!("日志写不出去：{e}")))
    }
}

pub struct LogStderr;

impl Component for LogStderr {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "log-stderr",
            kind: Kind::Service,
            summary: "把日志按行写到 stderr",
            provides: vec![LOG.id()],
            requires: vec![],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let config: Config = config.parse()?;
        let writer = Writer {
            min: config.level,
            out: Mutex::new(std::io::stderr()),
        };
        host.provide(&LOG, Arc::new(writer))
    }
}

register_component!(LogStderr);

#[cfg(test)]
mod tests {
    use log_contract::testing::log_contract;
    use serde_json::json;

    use super::*;

    fn entry(level: Level, message: &str) -> Entry {
        Entry {
            level,
            component: "focus".to_owned(),
            message: message.to_owned(),
        }
    }

    #[tokio::test]
    async fn obeys_the_log_contract() {
        // 换成内存写入器，契约测试的内容就不会刷到测试输出里
        let writer = Writer {
            min: Level::Debug,
            out: Mutex::new(Vec::new()),
        };
        log_contract(Arc::new(writer)).await.unwrap();
    }

    #[tokio::test]
    async fn component_provides_log() {
        let app = testkit::app()
            .with(LogStderr)
            .config("log-stderr", json!({ "level": "error" }))
            .unwrap()
            .boot()
            .await
            .unwrap();
        assert_eq!(app.started(), ["log-stderr"]);
        app.resolve(&LOG)
            .unwrap()
            .write(entry(Level::Info, "低于门槛，不输出"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn drops_entries_below_the_configured_level() {
        let writer = Arc::new(Writer {
            min: Level::Warn,
            out: Mutex::new(Vec::new()),
        });
        writer.write(entry(Level::Info, "不该出现")).await.unwrap();
        writer.write(entry(Level::Warn, "断线了")).await.unwrap();
        writer
            .write(entry(Level::Error, "第一行\n第二行"))
            .await
            .unwrap();
        let text = String::from_utf8(writer.out.lock().unwrap().clone()).unwrap();
        assert_eq!(
            text,
            "WARN  focus: 断线了\nERROR focus: 第一行\n    第二行\n"
        );
    }

    #[test]
    fn config_defaults_and_typos() {
        assert_eq!(
            ConfigSection::default().parse::<Config>().unwrap(),
            Config::default()
        );
        let section = ConfigSection::from_value(json!({ "level": "loud" })).unwrap();
        assert!(section.parse::<Config>().is_err());
        let section = ConfigSection::from_value(json!({ "levle": "warn" })).unwrap();
        assert!(section.parse::<Config>().is_err());
    }
}
