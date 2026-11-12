//! MCP 的消息：解析一行、生成参数的 JSON Schema、检查参数、拼响应。纯逻辑。

use std::collections::BTreeMap;

use mcp_contract::{ParamKind, ToolSpec};
use serde_json::{Map, Value, json};

/// 支持的协议版本，新的在前。
pub(crate) const VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// JSON-RPC 的错误码。
pub(crate) const PARSE_ERROR: i64 = -32_700;
pub(crate) const INVALID_REQUEST: i64 = -32_600;
pub(crate) const METHOD_NOT_FOUND: i64 = -32_601;
pub(crate) const INVALID_PARAMS: i64 = -32_602;

/// 收到的一行。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Incoming {
    /// 要回应的请求。
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// 不用回应的通知。
    Notification,
    /// 对方发来的响应（我们不发请求，忽略）。
    Response,
    /// 看不懂：解析失败（`id` 未知）或者不是合法的请求。
    Invalid {
        id: Value,
        code: i64,
        message: String,
    },
}

pub(crate) fn parse(line: &str) -> Incoming {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Incoming::Invalid {
            id: Value::Null,
            code: PARSE_ERROR,
            message: "不是合法的 JSON".to_owned(),
        };
    };
    let Some(object) = value.as_object() else {
        return Incoming::Invalid {
            id: Value::Null,
            code: INVALID_REQUEST,
            message: "要的是一个 JSON 对象（不支持批量）".to_owned(),
        };
    };
    let id = object.get("id").cloned();
    match (object.get("method").and_then(Value::as_str), id) {
        (Some(method), Some(id)) if !id.is_null() => Incoming::Request {
            id,
            method: method.to_owned(),
            params: object.get("params").cloned().unwrap_or(Value::Null),
        },
        (Some(_), _) => Incoming::Notification,
        (None, Some(_)) if object.contains_key("result") || object.contains_key("error") => {
            Incoming::Response
        }
        (None, id) => Incoming::Invalid {
            id: id.unwrap_or(Value::Null),
            code: INVALID_REQUEST,
            message: "没有 method".to_owned(),
        },
    }
}

pub(crate) fn result(id: &Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

pub(crate) fn error(id: &Value, code: i64, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }).to_string()
}

/// 对方要的版本我们支持就用它，否则用我们最新的。
pub(crate) fn negotiate(requested: Option<&str>) -> &'static str {
    requested
        .and_then(|r| VERSIONS.iter().find(|v| **v == r))
        .copied()
        .unwrap_or(VERSIONS[0])
}

/// `tools/list` 里的一项。
pub(crate) fn describe(spec: &ToolSpec) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for p in &spec.params {
        let mut schema = match &p.kind {
            ParamKind::String => json!({ "type": "string" }),
            ParamKind::Integer => json!({ "type": "integer" }),
            ParamKind::Choice { values } => json!({ "type": "string", "enum": values }),
        };
        schema["description"] = Value::String(p.description.clone());
        properties.insert(p.name.clone(), schema);
        if p.required {
            required.push(Value::String(p.name.clone()));
        }
    }
    json!({
        "name": spec.name,
        "description": spec.description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        },
    })
}

/// 按描述检查参数，换成字符串。不对时返回给 agent 看的原因。
pub(crate) fn arguments(
    spec: &ToolSpec,
    given: &Value,
) -> Result<BTreeMap<String, String>, String> {
    let empty = Map::new();
    let given = match given {
        Value::Null => &empty,
        Value::Object(map) => map,
        _ => return Err("arguments 要是一个对象".to_owned()),
    };
    if let Some(unknown) = given
        .keys()
        .find(|k| !spec.params.iter().any(|p| &p.name == *k))
    {
        return Err(format!("没有参数 `{unknown}`"));
    }
    let mut out = BTreeMap::new();
    for p in &spec.params {
        let value = match given.get(&p.name) {
            None | Some(Value::Null) => {
                if p.required {
                    return Err(format!("缺少参数 `{}`", p.name));
                }
                continue;
            }
            Some(v) => v,
        };
        let text = match (&p.kind, value) {
            (ParamKind::Integer, Value::Number(n)) if n.is_i64() || n.is_u64() => n.to_string(),
            // 有的客户端把数字当字符串发
            (ParamKind::Integer, Value::String(s)) if s.trim().parse::<i64>().is_ok() => {
                s.trim().to_owned()
            }
            (ParamKind::Integer, _) => return Err(format!("参数 `{}` 要是整数", p.name)),
            (ParamKind::String, Value::String(s)) => s.clone(),
            (ParamKind::String, _) => return Err(format!("参数 `{}` 要是字符串", p.name)),
            (ParamKind::Choice { values }, Value::String(s)) if values.contains(s) => s.clone(),
            (ParamKind::Choice { values }, _) => {
                return Err(format!("参数 `{}` 只能是 {}", p.name, values.join("、")));
            }
        };
        out.insert(p.name.clone(), text);
    }
    Ok(out)
}

/// `tools/call` 的结果。
pub(crate) fn tool_result(text: &str, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

#[cfg(test)]
mod tests {
    use mcp_contract::ToolParam;

    use super::*;

    fn spec() -> ToolSpec {
        let param = |name: &str, kind: ParamKind, required: bool| ToolParam {
            name: name.to_owned(),
            description: format!("{name} 的说明"),
            kind,
            required,
        };
        ToolSpec {
            name: "find".to_owned(),
            description: "找".to_owned(),
            params: vec![
                param("query", ParamKind::String, true),
                param("limit", ParamKind::Integer, false),
                param(
                    "source",
                    ParamKind::Choice {
                        values: vec!["a".to_owned(), "b".to_owned()],
                    },
                    false,
                ),
            ],
        }
    }

    #[test]
    fn parsing_lines() {
        assert_eq!(
            parse(r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#),
            Incoming::Request {
                id: json!(7),
                method: "ping".to_owned(),
                params: Value::Null
            }
        );
        assert_eq!(
            parse(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
            Incoming::Notification
        );
        assert_eq!(
            parse(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#),
            Incoming::Response
        );
        assert!(matches!(
            parse("not json"),
            Incoming::Invalid {
                code: PARSE_ERROR,
                ..
            }
        ));
        assert!(matches!(
            parse("[1]"),
            Incoming::Invalid {
                code: INVALID_REQUEST,
                ..
            }
        ));
        assert!(matches!(
            parse(r#"{"id":"x"}"#),
            Incoming::Invalid { code: INVALID_REQUEST, id, .. } if id == json!("x")
        ));
    }

    #[test]
    fn versions() {
        assert_eq!(negotiate(Some("2025-06-18")), "2025-06-18");
        assert_eq!(negotiate(Some("2099-01-01")), VERSIONS[0]);
        assert_eq!(negotiate(None), VERSIONS[0]);
    }

    #[test]
    fn schemas() {
        assert_eq!(
            describe(&spec())["inputSchema"],
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "query 的说明" },
                    "limit": { "type": "integer", "description": "limit 的说明" },
                    "source": { "type": "string", "enum": ["a", "b"], "description": "source 的说明" },
                },
                "required": ["query"],
                "additionalProperties": false,
            })
        );
    }

    #[test]
    fn checking_arguments() {
        let s = spec();
        let ok = arguments(&s, &json!({ "query": "气象", "limit": 5, "source": "a" })).unwrap();
        assert_eq!(ok["limit"], "5");
        assert_eq!(
            arguments(&s, &json!({ "query": "x", "limit": "7" })).unwrap()["limit"],
            "7"
        );
        assert_eq!(
            arguments(&s, &json!({ "query": "x", "limit": null }))
                .unwrap()
                .len(),
            1
        );
        for (bad, why) in [
            (json!({}), "缺少参数 `query`"),
            (json!({ "query": 1 }), "要是字符串"),
            (json!({ "query": "x", "limit": 1.5 }), "要是整数"),
            (json!({ "query": "x", "source": "c" }), "只能是 a、b"),
            (json!({ "query": "x", "nope": 1 }), "没有参数 `nope`"),
            (json!([1]), "要是一个对象"),
        ] {
            let err = arguments(&s, &bad).unwrap_err();
            assert!(err.contains(why), "{bad}: {err}");
        }
    }
}
