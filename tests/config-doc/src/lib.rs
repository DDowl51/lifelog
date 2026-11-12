//! 检查配置文档和代码对得上。
//!
//! 文档里每个组件一个 TOML 块，第一行是 `[components.<id>]`（或 `[kernel]`），下面是全部键和默认值：
//!
//! - 默认不写的键（代码里是 `None`）注释掉：`# path = "…"`；
//! - 太长的字符串默认值写成 `"…"`，只核对它是字符串。
//!
//! 别的 TOML 块（例子）第一行不是段名，不参与检查。

use std::collections::BTreeMap;

use serde_json::{Map, Value};

include!(concat!(env!("OUT_DIR"), "/components.rs"));

/// 占位：只核对类型是字符串。
pub const PLACEHOLDER: &str = "…";

/// 文档里一个段写了什么。
#[derive(Debug, Default, PartialEq)]
pub struct Section {
    pub values: Map<String, Value>,
    /// 注释掉的键：默认不写。
    pub unset: Vec<String>,
}

/// 从文档里取出所有段，键是 `kernel` 或组件 id。
pub fn parse(doc: &str) -> Result<BTreeMap<String, Section>, String> {
    let mut out = BTreeMap::new();
    let mut lines = doc.lines();
    while let Some(line) = lines.next() {
        if line.trim() != "```toml" {
            continue;
        }
        let block: Vec<&str> = lines.by_ref().take_while(|l| l.trim() != "```").collect();
        let Some(header) = block.first().map(|l| l.trim()) else {
            continue;
        };
        let Some(id) = header_id(header) else {
            continue;
        };
        let section = parse_block(&id, &block)?;
        if out.insert(id.clone(), section).is_some() {
            return Err(format!("`{header}` 写了两次"));
        }
    }
    Ok(out)
}

fn header_id(header: &str) -> Option<String> {
    let inner = header.strip_prefix('[')?.strip_suffix(']')?;
    if inner == "kernel" {
        return Some(inner.to_owned());
    }
    inner.strip_prefix("components.").map(str::to_owned)
}

fn parse_block(id: &str, block: &[&str]) -> Result<Section, String> {
    let text = block.join("\n");
    let table: toml::Table = text
        .parse()
        .map_err(|e| format!("[{id}] 那一块不是合法的 TOML：{e}"))?;
    let section = if id == "kernel" {
        table.get("kernel")
    } else {
        table.get("components").and_then(|c| c.get(id))
    };
    let values = match section.map(serde_json::to_value) {
        Some(Ok(Value::Object(map))) => map,
        _ => return Err(format!("[{id}] 那一块读不出这一段")),
    };
    if table.len() != 1
        || (id != "kernel" && table["components"].as_table().map(toml::Table::len) != Some(1))
    {
        return Err(format!("[{id}] 那一块只能有这一段"));
    }
    let unset = block
        .iter()
        .filter_map(|l| {
            let key = l.trim().strip_prefix('#')?.split_once('=')?.0.trim();
            let plain = !key.is_empty() && key.chars().all(|c| c.is_ascii_lowercase() || c == '_');
            plain.then(|| key.to_owned())
        })
        .collect();
    Ok(Section { values, unset })
}

/// 文档里的一段和代码里的默认值有哪些地方对不上。
pub fn compare(id: &str, defaults: &Map<String, Value>, doc: &Section) -> Vec<String> {
    let mut errors = Vec::new();
    for (key, default) in defaults {
        let unset = doc.unset.contains(key);
        match (doc.values.get(key), unset) {
            (None, false) => errors.push(format!("[{id}] 缺 `{key}`，默认值是 {default}")),
            (Some(_), true) => errors.push(format!("[{id}] `{key}` 既写了值又注释掉了")),
            (None, true) if !default.is_null() => errors.push(format!(
                "[{id}] `{key}` 注释掉了（表示默认不写），但默认值是 {default}"
            )),
            (Some(_), false) if default.is_null() => {
                errors.push(format!("[{id}] `{key}` 默认不写，文档里应该注释掉"))
            }
            (Some(Value::String(s)), false) if s == PLACEHOLDER => {
                if !default.is_string() {
                    errors.push(format!("[{id}] `{key}` 写的是 \"…\"，但默认值是 {default}"));
                }
            }
            (Some(written), false) if written != default => errors.push(format!(
                "[{id}] `{key}` 文档写的是 {written}，代码里是 {default}"
            )),
            _ => {}
        }
    }
    for key in doc.values.keys().chain(&doc.unset) {
        if !defaults.contains_key(key) {
            errors.push(format!("[{id}] 没有 `{key}` 这个键"));
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::json;

    use super::*;

    const DOC: &str = r#"
说明

```toml
[components.wm-niri]
# socket = "/run/niri.sock"
reconnect_max_ms = 60000
prompt = "…"
```

```toml
# 例子
[components.wm-niri]
socket = "x"
```

```toml
[kernel]
queue_capacity = 1024
```
"#;

    fn defaults(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn reads_default_blocks_and_skips_examples() {
        let doc = parse(DOC).unwrap();
        assert_eq!(doc.keys().collect::<Vec<_>>(), ["kernel", "wm-niri"]);
        let niri = &doc["wm-niri"];
        assert_eq!(niri.unset, ["socket"]);
        assert_eq!(niri.values["reconnect_max_ms"], json!(60000));
    }

    #[test]
    fn matching_defaults_have_no_errors() {
        let doc = parse(DOC).unwrap();
        let code = defaults(json!({ "socket": null, "reconnect_max_ms": 60000, "prompt": "很长" }));
        assert!(compare("wm-niri", &code, &doc["wm-niri"]).is_empty());
    }

    #[test]
    fn reports_every_kind_of_mismatch() {
        let doc = parse(DOC).unwrap();
        let code =
            defaults(json!({ "socket": "/x", "reconnect_max_ms": 1, "prompt": 3, "new": true }));
        let errors = compare("wm-niri", &code, &doc["wm-niri"]);
        assert_eq!(errors.len(), 4, "{errors:#?}");
        let missing = defaults(json!({}));
        assert_eq!(compare("wm-niri", &missing, &doc["wm-niri"]).len(), 3);
    }

    #[test]
    fn a_section_twice_is_an_error() {
        let twice = "```toml\n[kernel]\n```\n```toml\n[kernel]\n```\n";
        assert!(parse(twice).is_err());
    }
}
