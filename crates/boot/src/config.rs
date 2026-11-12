//! 配置文件：`[kernel]` 一段，`[components.<id>]` 每个组件一段。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use kernel_api::{AppError, ConfigSection};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// kernel 的运行参数。kernel 自己没有默认值，默认值在这里。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct KernelSettings {
    pub queue_capacity: usize,
    pub stop_timeout_secs: u64,
}

impl Default for KernelSettings {
    fn default() -> Self {
        Self {
            queue_capacity: 1024,
            stop_timeout_secs: 5,
        }
    }
}

impl KernelSettings {
    pub fn stop_timeout(&self) -> Duration {
        Duration::from_secs(self.stop_timeout_secs)
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct File {
    kernel: KernelSettings,
    components: Map<String, Value>,
}

/// 读进来的配置。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Settings {
    pub kernel: KernelSettings,
    sections: HashMap<String, Map<String, Value>>,
}

/// 配置文件在哪：`$<APP>_CONFIG` → `$XDG_CONFIG_HOME/<app>/config.toml` → `~/.config/<app>/config.toml`。
pub fn config_path(app: &str) -> PathBuf {
    let var = format!("{}_CONFIG", app.to_uppercase().replace('-', "_"));
    if let Some(path) = std::env::var_os(var) {
        return PathBuf::from(path);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(app).join("config.toml")
}

/// 读配置文件。文件不存在等于空文件：全部用默认值。
pub fn load(path: &Path) -> Result<Settings, AppError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(AppError::config(format!(
                "读不了配置文件 {}：{e}",
                path.display()
            )));
        }
    };
    Settings::parse(&text).map_err(|e| e.context(path.display().to_string()))
}

impl Settings {
    pub fn parse(text: &str) -> Result<Self, AppError> {
        let file: File = toml::from_str(text).map_err(|e| AppError::config(e.to_string()))?;
        let mut sections = HashMap::new();
        for (id, value) in file.components {
            match value {
                Value::Object(map) => {
                    sections.insert(id, map);
                }
                _ => {
                    return Err(AppError::config(format!("[components.{id}] 必须是一张表")));
                }
            }
        }
        Ok(Self {
            kernel: file.kernel,
            sections,
        })
    }

    /// 启动器覆盖一个键（比如命令行进程把存储改成只读）。组件只看到最终结果。
    pub fn set(&mut self, component: &str, key: &str, value: Value) {
        self.sections
            .entry(component.to_owned())
            .or_default()
            .insert(key.to_owned(), value);
    }

    /// 整理成交给 kernel 的配置。
    ///
    /// - `linked`：链接进当前进程的组件。
    /// - `known`：仓库里存在的全部组件。配置里出现不在其中的段，按拼写错误报错；
    ///   在其中但没链接进当前进程的段（别的进程的组件）忽略。
    ///
    /// 只对会在当前进程启动的组件解析 `xxx_cmd`。
    pub fn for_process(
        &self,
        linked: &[&str],
        known: &[&str],
    ) -> Result<HashMap<String, ConfigSection>, AppError> {
        let mut out = HashMap::new();
        let mut ids: Vec<&String> = self.sections.keys().collect();
        ids.sort();
        for id in ids {
            if !known.contains(&id.as_str()) && !linked.contains(&id.as_str()) {
                let mut all: Vec<&str> = known.iter().chain(linked).copied().collect();
                all.sort_unstable();
                all.dedup();
                return Err(AppError::config(format!(
                    "[components.{id}]：没有这个组件。可用的：{}",
                    all.join("、")
                )));
            }
            if !linked.contains(&id.as_str()) {
                continue;
            }
            let mut map = self.sections[id].clone();
            if map.get("enabled") != Some(&Value::Bool(false)) {
                resolve_cmds(&mut map).map_err(|e| e.context(format!("[components.{id}]")))?;
            }
            out.insert(id.clone(), ConfigSection::new(map));
        }
        Ok(out)
    }
}

/// 把每个 `xxx_cmd = "命令"` 换成 `xxx = "<命令的输出>"`。密钥因此不用写进配置文件。
fn resolve_cmds(map: &mut Map<String, Value>) -> Result<(), AppError> {
    let keys: Vec<String> = map
        .keys()
        .filter(|k| k.ends_with("_cmd"))
        .cloned()
        .collect();
    for key in keys {
        let Some(Value::String(cmd)) = map.remove(&key) else {
            return Err(AppError::config(format!("{key} 必须是一条命令（字符串）")));
        };
        let target = key.trim_end_matches("_cmd").to_owned();
        if map.contains_key(&target) {
            return Err(AppError::config(format!("{target} 和 {key} 不能同时写")));
        }
        let output = Command::new("sh")
            .args(["-c", &cmd])
            .output()
            .map_err(|e| AppError::config(format!("{key}：命令没能运行：{e}")))?;
        if !output.status.success() {
            return Err(AppError::config(format!(
                "{key}：命令失败（{}）",
                output.status
            )));
        }
        let text = String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned();
        map.insert(target, Value::String(text));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use kernel_api::ErrorKind;
    use serde_json::json;

    use super::*;

    const TEXT: &str = r#"
        [kernel]
        stop_timeout_secs = 2

        [components.focus]
        flush_interval_secs = 10

        [components.wm-niri]
        enabled = false
    "#;

    #[test]
    fn missing_file_means_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let settings = load(&dir.path().join("nope.toml")).unwrap();
        assert_eq!(settings, Settings::default());
        assert_eq!(settings.kernel.queue_capacity, 1024);
    }

    #[test]
    fn sections_reach_only_linked_components() {
        let settings = Settings::parse(TEXT).unwrap();
        assert_eq!(settings.kernel.stop_timeout(), Duration::from_secs(2));
        let known = ["focus", "wm-niri"];
        let out = settings.for_process(&["focus"], &known).unwrap();
        assert_eq!(out.len(), 1);
        #[derive(Deserialize)]
        struct Focus {
            flush_interval_secs: u64,
        }
        assert_eq!(
            out["focus"].parse::<Focus>().unwrap().flush_interval_secs,
            10
        );
    }

    #[test]
    fn unknown_component_is_a_typo() {
        let settings = Settings::parse("[components.fcous]\n").unwrap();
        let err = settings
            .for_process(&["focus"], &["focus", "wm-niri"])
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Config);
        assert!(
            err.message.contains("fcous") && err.message.contains("focus、wm-niri"),
            "{err}"
        );
    }

    #[test]
    fn unknown_top_level_key_is_rejected() {
        let err = Settings::parse("[plugins.focus]\n").unwrap_err();
        assert!(err.message.contains("plugins"), "{err}");
    }

    #[test]
    fn cmd_keys_are_replaced_by_command_output() {
        let mut settings =
            Settings::parse("[components.llm]\ntoken_cmd = \"printf 's3cret\\n'\"\n").unwrap();
        let out = settings.for_process(&["llm"], &["llm"]).unwrap();
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Llm {
            token: String,
        }
        assert_eq!(out["llm"].parse::<Llm>().unwrap().token, "s3cret");

        settings.set("llm", "token_cmd", json!("exit 3"));
        let err = settings.for_process(&["llm"], &["llm"]).unwrap_err();
        assert!(
            err.message.contains("[components.llm]") && err.message.contains("token_cmd"),
            "{err}"
        );
    }

    #[test]
    fn disabled_components_do_not_run_commands() {
        let settings =
            Settings::parse("[components.llm]\nenabled = false\ntoken_cmd = \"exit 3\"\n").unwrap();
        assert!(settings.for_process(&["llm"], &["llm"]).is_ok());
    }

    #[test]
    fn launcher_can_override() {
        let mut settings = Settings::parse(TEXT).unwrap();
        settings.set("focus", "flush_interval_secs", json!(1));
        settings.set("storage", "read_only", json!(true));
        let out = settings
            .for_process(&["focus", "storage"], &["focus", "storage", "wm-niri"])
            .unwrap();
        assert_eq!(out.len(), 2);
    }
}
