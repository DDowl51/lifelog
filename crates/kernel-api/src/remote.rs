//! 跨进程调用：同一个契约，换一种传输（复杂档）。
//!
//! `contract!` 为每个契约生成两样东西：
//!
//! - **分派**：按方法名和 JSON 参数调用一个实现，结果编成 JSON。导出契约的进程用它。
//! - **代理**：实现同一个 trait，每次调用把参数编成 JSON 交给一个 [`Transport`]，再把结果解回来。
//!   导入契约的进程用它，使用方拿到的句柄和进程内的没有区别。
//!
//! 契约只传纯数据（原则 5），所以这里不需要契约做任何改动。传输（socket）属于 kernel；
//! 这里只有消息格式，不碰 IO。

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    AppError, BoxFuture, Component, ConfigSection, Contract, ErrorKind, Host, Kind, Manifest,
};

/// 把一次调用送到另一个进程，带回结果。由 kernel 实现。
pub trait Transport: Send + Sync {
    /// `params` 和返回值都是 JSON 文本。
    fn call(
        &self,
        contract: &'static str,
        version: u32,
        method: &'static str,
        params: String,
    ) -> BoxFuture<'static, Result<String, AppError>>;
}

/// 按方法名调用一个实现：（方法名，参数的 JSON）→ 结果的 JSON。
pub type Dispatcher =
    Arc<dyn Fn(String, String) -> BoxFuture<'static, Result<String, AppError>> + Send + Sync>;

/// 线上的请求：一行一个。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireRequest {
    pub contract: String,
    pub version: u32,
    pub method: String,
    pub params: Value,
}

/// 线上的响应：一行一个。`ok` 和 `err` 只有一个。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub err: Option<AppError>,
}

fn broken(what: &str, e: impl std::fmt::Display) -> AppError {
    AppError::unavailable(format!("跨进程的消息{what}：{e}"))
}

pub fn encode_request(
    contract: &str,
    version: u32,
    method: &str,
    params: &str,
) -> Result<String, AppError> {
    let params: Value = serde_json::from_str(params).map_err(|e| broken("参数不是 JSON", e))?;
    serde_json::to_string(&WireRequest {
        contract: contract.to_owned(),
        version,
        method: method.to_owned(),
        params,
    })
    .map_err(|e| broken("编不成 JSON", e))
}

pub fn decode_request(line: &str) -> Result<WireRequest, AppError> {
    serde_json::from_str(line).map_err(|e| AppError::validation(format!("看不懂的请求：{e}")))
}

pub fn encode_response(result: Result<String, AppError>) -> String {
    let response = match result.and_then(|text| {
        serde_json::from_str::<Value>(&text).map_err(|e| broken("结果不是 JSON", e))
    }) {
        Ok(value) => WireResponse {
            ok: Some(value),
            err: None,
        },
        Err(e) => WireResponse {
            ok: None,
            err: Some(e),
        },
    };
    serde_json::to_string(&response).unwrap_or_else(|e| {
        format!(r#"{{"err":{{"kind":"unavailable","message":"响应编不成 JSON：{e}"}}}}"#)
    })
}

/// 解出结果的 JSON 文本；对方返回的错误原样还原。
pub fn decode_response(line: &str) -> Result<String, AppError> {
    let response: WireResponse =
        serde_json::from_str(line).map_err(|e| broken("看不懂对方的响应", e))?;
    match (response.ok, response.err) {
        (_, Some(e)) => Err(e),
        (Some(value), None) => Ok(value.to_string()),
        (None, None) => Err(broken("响应是空的", "既没有 ok 也没有 err")),
    }
}

// ---------- 给 `contract!` 生成的代码用 ----------

#[doc(hidden)]
pub fn __params(text: &str) -> Result<Map<String, Value>, AppError> {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(Value::Null) => Ok(Map::new()),
        _ => Err(AppError::validation("参数要是一个 JSON 对象")),
    }
}

#[doc(hidden)]
pub fn __take<T: DeserializeOwned>(
    params: &mut Map<String, Value>,
    name: &str,
) -> Result<T, AppError> {
    let value = params.remove(name).unwrap_or(Value::Null);
    serde_json::from_value(value)
        .map_err(|e| AppError::validation(format!("参数 `{name}` 不对：{e}")))
}

#[doc(hidden)]
pub fn __put<T: Serialize>(
    params: &mut Map<String, Value>,
    name: &str,
    value: &T,
) -> Result<(), AppError> {
    let value = serde_json::to_value(value).map_err(|e| broken("参数编不成 JSON", e))?;
    params.insert(name.to_owned(), value);
    Ok(())
}

#[doc(hidden)]
pub fn __params_text(params: Map<String, Value>) -> String {
    Value::Object(params).to_string()
}

#[doc(hidden)]
pub fn __encode<T: Serialize>(value: &T) -> Result<String, AppError> {
    serde_json::to_string(value).map_err(|e| broken("结果编不成 JSON", e))
}

#[doc(hidden)]
pub fn __decode<T: DeserializeOwned>(text: &str) -> Result<T, AppError> {
    serde_json::from_str(text).map_err(|e| broken("结果的类型对不上（两边的契约版本不一致？）", e))
}

#[doc(hidden)]
pub fn __no_method(contract: &str, method: &str) -> AppError {
    AppError::new(
        ErrorKind::NotFound,
        format!("契约 `{contract}` 没有方法 `{method}`"),
    )
}

/// 导入：在这个进程里「提供」一个契约，实际的调用都转给另一个进程。
///
/// 它就是一个普通组件：kernel 照常给它排序、启动、检查依赖；使用方 `resolve` 到的是代理。
/// 由启动器登记（`register_component!(Import::new(…))`），组件和契约都不知道它的存在。
pub struct Import<T: ?Sized + 'static> {
    id: &'static str,
    summary: &'static str,
    contract: &'static Contract<T>,
}

impl<T: ?Sized + Send + Sync + 'static> Import<T> {
    pub const fn new(
        id: &'static str,
        summary: &'static str,
        contract: &'static Contract<T>,
    ) -> Self {
        Self {
            id,
            summary,
            contract,
        }
    }
}

impl<T: ?Sized + Send + Sync + 'static> Component for Import<T> {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<crate::NoConfig>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: self.id,
            kind: Kind::Service,
            summary: self.summary,
            provides: vec![self.contract.id()],
            requires: vec![],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        config.parse::<crate::NoConfig>()?;
        let transport = host.remote()?;
        host.provide(self.contract, self.contract.__proxy(transport))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn requests_and_responses_round_trip() {
        let line = encode_request("summary", 1, "summarize", r#"{"date":"yesterday"}"#).unwrap();
        let request = decode_request(&line).unwrap();
        assert_eq!(
            (
                request.contract.as_str(),
                request.version,
                request.method.as_str()
            ),
            ("summary", 1, "summarize")
        );
        assert_eq!(request.params["date"], "yesterday");

        assert_eq!(
            decode_response(&encode_response(Ok("[1,2]".to_owned()))).unwrap(),
            "[1,2]"
        );
        let err = decode_response(&encode_response(Err(AppError::not_found("没有")))).unwrap_err();
        assert_eq!(
            (err.kind, err.message.as_str()),
            (ErrorKind::NotFound, "没有"),
            "错误原样还原"
        );
        assert_eq!(
            decode_response("oops").unwrap_err().kind,
            ErrorKind::Unavailable
        );
        assert_eq!(
            decode_request("oops").unwrap_err().kind,
            ErrorKind::Validation
        );
    }
}
