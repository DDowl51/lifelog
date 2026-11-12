//! 一个组件的配置数据。
//!
//! kernel 不读配置文件，也不认识文件格式：启动器把文件读成数据交进来，
//! kernel 只看 `enabled`，其余原样交给组件自己解析。

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::AppError;

/// 没有配置项的组件用的配置类型：写了任何键都在启动时报错。
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NoConfig {}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfigSection(Map<String, Value>);

impl ConfigSection {
    pub fn new(map: Map<String, Value>) -> Self {
        Self(map)
    }

    /// 从一个 JSON 值构造；不是表就报错。
    pub fn from_value(value: Value) -> Result<Self, AppError> {
        match value {
            Value::Object(map) => Ok(Self(map)),
            other => Err(AppError::config(format!(
                "配置段必须是一张表，实际是 {other}"
            ))),
        }
    }

    /// `enabled` 键，缺省为 `true`。
    pub fn enabled(&self) -> Result<bool, AppError> {
        match self.0.get("enabled") {
            None => Ok(true),
            Some(Value::Bool(b)) => Ok(*b),
            Some(other) => Err(AppError::config(format!(
                "enabled 必须是 true 或 false，实际是 {other}"
            ))),
        }
    }

    /// 解析成组件自己的配置类型。`enabled` 由 kernel 处理，这里先去掉。
    ///
    /// 配置类型应该加 `#[serde(deny_unknown_fields)]`，这样写错键名会在启动时报错。
    /// 一个配置类型的默认值，给 [`Component::config_defaults`](crate::Component::config_defaults) 用。
    /// 值是 `null` 的键表示「不写时另有规则」（比如按环境变量找）。
    pub fn defaults<T: Default + Serialize>() -> Option<Self> {
        match serde_json::to_value(T::default()) {
            Ok(Value::Object(map)) => Some(Self(map)),
            _ => None,
        }
    }

    /// 全部键值。
    pub fn as_map(&self) -> &Map<String, Value> {
        &self.0
    }

    pub fn parse<T: DeserializeOwned>(&self) -> Result<T, AppError> {
        let mut map = self.0.clone();
        map.remove("enabled");
        serde_json::from_value(Value::Object(map)).map_err(|e| AppError::config(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use serde_json::json;

    use super::*;
    use crate::ErrorKind;

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(deny_unknown_fields, default)]
    struct Sample {
        interval_secs: u64,
    }

    impl Default for Sample {
        fn default() -> Self {
            Self { interval_secs: 30 }
        }
    }

    fn section(v: Value) -> ConfigSection {
        ConfigSection::from_value(v).unwrap()
    }

    #[test]
    fn enabled_defaults_to_true() {
        assert!(ConfigSection::default().enabled().unwrap());
        assert!(!section(json!({ "enabled": false })).enabled().unwrap());
        assert_eq!(
            section(json!({ "enabled": "no" }))
                .enabled()
                .unwrap_err()
                .kind,
            ErrorKind::Config
        );
    }

    #[test]
    fn parse_strips_enabled_and_uses_defaults() {
        assert_eq!(
            section(json!({ "enabled": true }))
                .parse::<Sample>()
                .unwrap(),
            Sample::default()
        );
        assert_eq!(
            section(json!({ "interval_secs": 5 }))
                .parse::<Sample>()
                .unwrap(),
            Sample { interval_secs: 5 }
        );
    }

    #[test]
    fn unknown_key_is_a_config_error() {
        let err = section(json!({ "intervall_secs": 5 }))
            .parse::<Sample>()
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Config);
        assert!(err.message.contains("intervall_secs"), "{}", err.message);
    }

    #[test]
    fn section_must_be_a_table() {
        assert_eq!(
            ConfigSection::from_value(json!(3)).unwrap_err().kind,
            ErrorKind::Config
        );
    }
}
