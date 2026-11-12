//! 系统服务 `mcp-stdio`：在标准输入输出上讲 MCP（JSON-RPC 2.0，一行一条）。
//!
//! 它不认识任何工具：工具由各组件登记到 `MCP_TOOLS`。它负责协议握手、列出工具、
//! 按描述检查参数、把调用交给登记者、把结果或错误包成 MCP 的格式。
//! 标准输出只写协议消息；日志走标准错误（由日志服务负责）。

mod protocol;

use std::collections::BTreeMap;
use std::sync::Arc;

use kernel_api::{
    AppError, Collector, Component, ConfigSection, ErrorKind, Host, Kind, Manifest, async_trait,
    register_component,
};
use mcp_contract::{MCP, MCP_TOOLS, Mcp, ToolCall, ToolProvider, ToolSpec};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::protocol::{
    INVALID_PARAMS, Incoming, METHOD_NOT_FOUND, arguments, describe, error, negotiate, parse,
    result, tool_result,
};

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
struct Config {
    /// 告诉对方的服务名。不写就用程序名。
    name: Option<String>,
    /// 握手时给对方的一段说明（什么时候用这些工具）。不写就不给。
    instructions: Option<String>,
}

/// 处理协议。和标准输入输出分开，好测。
pub struct Server {
    providers: Collector<dyn ToolProvider>,
    name: String,
    instructions: Option<String>,
}

impl Server {
    /// 所有工具，记下各自的登记者。两个工具同名是装配错误。
    async fn tools(&self) -> Result<Vec<(Arc<dyn ToolProvider>, ToolSpec)>, AppError> {
        let mut all: Vec<(Arc<dyn ToolProvider>, ToolSpec)> = Vec::new();
        for provider in self.providers.all() {
            for spec in provider.tools().await? {
                if all.iter().any(|(_, s)| s.name == spec.name) {
                    return Err(AppError::config(format!("有两个工具都叫 `{}`", spec.name)));
                }
                all.push((provider.clone(), spec));
            }
        }
        Ok(all)
    }

    /// 处理收到的一行，返回要回的一行（通知和响应不用回）。
    pub async fn handle(&self, line: &str) -> Option<String> {
        match parse(line) {
            Incoming::Notification | Incoming::Response => None,
            Incoming::Invalid { id, code, message } => Some(error(&id, code, &message)),
            Incoming::Request { id, method, params } => {
                Some(match self.request(&method, &params).await {
                    Ok(value) => result(&id, value),
                    Err((code, message)) => error(&id, code, &message),
                })
            }
        }
    }

    async fn request(&self, method: &str, params: &Value) -> Result<Value, (i64, String)> {
        let internal = |e: AppError| (-32_603, e.message);
        match method {
            "initialize" => {
                let version = negotiate(params.get("protocolVersion").and_then(Value::as_str));
                let mut out = json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": self.name, "version": env!("CARGO_PKG_VERSION") },
                });
                if let Some(instructions) = &self.instructions {
                    out["instructions"] = Value::String(instructions.clone());
                }
                Ok(out)
            }
            "ping" => Ok(json!({})),
            "tools/list" => {
                let tools = self.tools().await.map_err(internal)?;
                Ok(json!({ "tools": tools.iter().map(|(_, s)| describe(s)).collect::<Vec<_>>() }))
            }
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or((INVALID_PARAMS, "缺少工具名 name".to_owned()))?;
                let tools = self.tools().await.map_err(internal)?;
                let Some((provider, spec)) = tools.into_iter().find(|(_, s)| s.name == name) else {
                    return Err((INVALID_PARAMS, format!("没有工具 `{name}`")));
                };
                // 参数不对、工具出错：作为工具的错误结果给 agent，让它能自己改了再试
                let args: BTreeMap<String, String> =
                    match arguments(&spec, params.get("arguments").unwrap_or(&Value::Null)) {
                        Ok(args) => args,
                        Err(why) => return Ok(tool_result(&why, true)),
                    };
                let call = ToolCall {
                    name: name.to_owned(),
                    args,
                };
                Ok(match provider.call(call).await {
                    Ok(output) => tool_result(&output.text, false),
                    Err(e) => tool_result(&format!("{}：{}", kind_name(e.kind), e.message), true),
                })
            }
            other => Err((METHOD_NOT_FOUND, format!("不支持 `{other}`"))),
        }
    }
}

fn kind_name(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Validation => "参数不对",
        ErrorKind::NotFound => "找不到",
        ErrorKind::Unavailable | ErrorKind::TempFail => "暂时不可用",
        _ => "出错了",
    }
}

#[async_trait]
impl Mcp for Server {
    async fn serve(&self) -> Result<u8, AppError> {
        let io = |e: std::io::Error| AppError::unavailable(format!("标准输入输出出错：{e}"));
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        let mut out = tokio::io::stdout();
        while let Some(line) = lines.next_line().await.map_err(io)? {
            if line.trim().is_empty() {
                continue;
            }
            if let Some(reply) = self.handle(&line).await {
                out.write_all(reply.as_bytes()).await.map_err(io)?;
                out.write_all(b"\n").await.map_err(io)?;
                out.flush().await.map_err(io)?;
            }
        }
        // 对方关闭了标准输入：正常结束
        Ok(0)
    }
}

pub struct McpStdio;

impl Component for McpStdio {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "mcp-stdio",
            kind: Kind::Service,
            summary: "在标准输入输出上讲 MCP",
            provides: vec![MCP.id(), MCP_TOOLS.id()],
            requires: vec![],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let config: Config = config.parse()?;
        let name = config.name.unwrap_or_else(|| {
            std::env::args()
                .next()
                .and_then(|p| {
                    std::path::Path::new(&p)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| "mcp".to_owned())
        });
        let server = Server {
            providers: host.collector(&MCP_TOOLS)?,
            name,
            instructions: config.instructions,
        };
        host.provide(&MCP, Arc::new(server))
    }
}

register_component!(McpStdio);

#[cfg(test)]
mod tests;
