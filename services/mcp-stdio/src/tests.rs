//! 不碰标准输入输出，直接一行一行地喂给 `Server::handle`。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use kernel_api::{AppError, Component, ConfigSection, Host, Kind, Manifest, async_trait};
use mcp_contract::{MCP_TOOLS, ParamKind, ToolCall, ToolOutput, ToolParam, ToolProvider, ToolSpec};
use serde_json::{Value, json};

use super::*;

/// 一个工具 `echo`：把参数原样写成 JSON；`fail=yes` 时报错。
struct Echo(&'static str);

#[async_trait]
impl ToolProvider for Echo {
    async fn tools(&self) -> Result<Vec<ToolSpec>, AppError> {
        Ok(vec![ToolSpec {
            name: self.0.to_owned(),
            description: "回声".to_owned(),
            params: vec![
                ToolParam {
                    name: "text".to_owned(),
                    description: "要说的".to_owned(),
                    kind: ParamKind::String,
                    required: true,
                },
                ToolParam {
                    name: "fail".to_owned(),
                    description: "要不要失败".to_owned(),
                    kind: ParamKind::Choice {
                        values: vec!["yes".to_owned()],
                    },
                    required: false,
                },
            ],
        }])
    }

    async fn call(&self, call: ToolCall) -> Result<ToolOutput, AppError> {
        if call.args.contains_key("fail") {
            return Err(AppError::not_found("故意的"));
        }
        Ok(ToolOutput {
            text: serde_json::to_string(&call.args).unwrap(),
        })
    }
}

struct Contributor(&'static str, &'static str);

impl Component for Contributor {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: self.0,
            kind: Kind::Feature,
            summary: "测试用",
            provides: vec![],
            requires: vec![MCP_TOOLS.id()],
            optional_requires: vec![],
        }
    }

    fn start(&self, _: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        host.contribute(&MCP_TOOLS, Arc::new(Echo(self.1)))
    }
}

/// 起一个服务，返回它的 `Server`（通过一个只拿 Collector 的探针重建）。
async fn server(contributors: &[(&'static str, &'static str)]) -> (testkit::TestApp, Server) {
    let probe = Probe::default();
    let mut builder = testkit::app().with(probe.clone());
    for (id, tool) in contributors {
        builder = builder.with(Contributor(id, tool));
    }
    let app = builder.boot().await.unwrap();
    let server = Server {
        providers: probe.0.get().cloned().unwrap(),
        name: "lifelog-mcp".to_owned(),
        instructions: Some("说明".to_owned()),
    };
    (app, server)
}

/// 扩展点的所有者，测试里代替 `McpStdio`：只把 Collector 交出来。
#[derive(Clone, Default)]
struct Probe(Arc<std::sync::OnceLock<Collector<dyn ToolProvider>>>);

impl Component for Probe {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: "mcp-probe",
            kind: Kind::Service,
            summary: "测试用",
            provides: vec![MCP_TOOLS.id()],
            requires: vec![],
            optional_requires: vec![],
        }
    }

    fn start(&self, _: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let _ = self.0.set(host.collector(&MCP_TOOLS)?);
        Ok(())
    }
}

async fn ask(server: &Server, request: Value) -> Value {
    let reply = server
        .handle(&request.to_string())
        .await
        .expect("请求要有回应");
    serde_json::from_str(&reply).unwrap()
}

#[tokio::test]
async fn handshake_and_ping() {
    let (_app, s) = server(&[]).await;
    let init = ask(
        &s,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": "2025-06-18", "capabilities": {},
                            "clientInfo": { "name": "claude-code", "version": "1" } } }),
    )
    .await;
    assert_eq!(init["id"], 1);
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "lifelog-mcp");
    assert_eq!(init["result"]["capabilities"], json!({ "tools": {} }));
    assert_eq!(init["result"]["instructions"], "说明");
    assert_eq!(
        s.handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await,
        None,
        "通知不回"
    );
    let pong = ask(&s, json!({ "jsonrpc": "2.0", "id": "p", "method": "ping" })).await;
    assert_eq!(pong, json!({ "jsonrpc": "2.0", "id": "p", "result": {} }));
}

#[tokio::test]
async fn tools_are_listed_and_called() {
    let (_app, s) = server(&[("a", "echo")]).await;
    let list = ask(
        &s,
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    )
    .await;
    let tools = list["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "echo");
    assert_eq!(tools[0]["inputSchema"]["required"], json!(["text"]));

    let call = |args: Value| {
        json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": { "name": "echo", "arguments": args } })
    };
    let ok = ask(&s, call(json!({ "text": "气象站" }))).await;
    assert_eq!(ok["result"]["isError"], false);
    assert_eq!(ok["result"]["content"][0]["text"], r#"{"text":"气象站"}"#);

    let failed = ask(&s, call(json!({ "text": "x", "fail": "yes" }))).await;
    assert_eq!(
        failed["result"]["isError"], true,
        "工具出错是工具的结果，不是协议错误"
    );
    assert_eq!(failed["result"]["content"][0]["text"], "找不到：故意的");

    let bad = ask(&s, call(json!({}))).await;
    assert_eq!(bad["result"]["isError"], true);
    assert!(
        bad["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("缺少参数 `text`")
    );

    let unknown = ask(
        &s,
        json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "nope" } }),
    )
    .await;
    assert_eq!(unknown["error"]["code"], -32_602);
}

#[tokio::test]
async fn protocol_errors() {
    let (_app, s) = server(&[]).await;
    let not_found = ask(
        &s,
        json!({ "jsonrpc": "2.0", "id": 5, "method": "resources/list" }),
    )
    .await;
    assert_eq!(not_found["error"]["code"], -32_601);
    let broken: Value = serde_json::from_str(&s.handle("{oops").await.unwrap()).unwrap();
    assert_eq!(
        (broken["id"].clone(), broken["error"]["code"].clone()),
        (Value::Null, json!(-32_700))
    );
    assert_eq!(
        s.handle(r#"{"jsonrpc":"2.0","id":9,"result":{}}"#).await,
        None,
        "对方的响应不回"
    );
}

#[tokio::test]
async fn two_tools_with_the_same_name_is_an_error() {
    let (_app, s) = server(&[("a", "echo"), ("b", "echo")]).await;
    let list = ask(
        &s,
        json!({ "jsonrpc": "2.0", "id": 6, "method": "tools/list" }),
    )
    .await;
    assert!(
        list["error"]["message"]
            .as_str()
            .unwrap()
            .contains("两个工具都叫 `echo`")
    );
}
