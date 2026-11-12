//! 契约：给 agent 用的工具服务（MCP）。扩展点：各组件登记自己的工具。
//!
//! 和命令行子命令是同一个模式：登记方给出工具的描述（纯数据）和一个按调用生成结果的方法，
//! 协议、传输、参数的 JSON Schema 都是工具服务的事。参数和结果都是文本：
//! 参数按名字给成字符串（数字也是），结果是一段文本（通常是 JSON）。

use std::collections::BTreeMap;

use kernel_api::{AppError, contract};
use serde::{Deserialize, Serialize};

/// 参数的类型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ParamKind {
    String,
    Integer,
    /// 只能是其中之一。
    Choice {
        values: Vec<String>,
    },
}

/// 一个参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolParam {
    pub name: String,
    /// 写给 agent 看的说明。
    pub description: String,
    pub kind: ParamKind,
    pub required: bool,
}

/// 一个工具的描述。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// 在所有登记者之间唯一。
    pub name: String,
    /// 写给 agent 看的说明：做什么、什么时候用。
    pub description: String,
    pub params: Vec<ToolParam>,
}

/// 一次调用。参数已经按描述检查过：必填的都在、类型和取值对得上。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: String,
    pub args: BTreeMap<String, String>,
}

/// 调用的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutput {
    pub text: String,
}

contract! {
    /// 扩展点：有工具的组件来登记。
    pub trait ToolProvider as MCP_TOOLS = point("mcp.tools", 1) {
        async fn tools(&self) -> Result<Vec<ToolSpec>, AppError>;

        /// 出错时返回的错误会原样告诉 agent（作为工具的错误结果，不是协议错误）。
        async fn call(&self, call: ToolCall) -> Result<ToolOutput, AppError>;
    }
}

contract! {
    /// 工具服务。
    pub trait Mcp as MCP = service("mcp", 1) {
        /// 接管标准输入输出，直到对方关闭连接。返回进程应该用的退出码。
        async fn serve(&self) -> Result<u8, AppError>;
    }
}

#[cfg(feature = "testing")]
pub mod testing {
    //! 假的工具服务：不碰标准输入输出，直接把调用交给登记了那个工具的组件。

    use std::sync::{Arc, OnceLock};

    use kernel_api::{AppError, Collector, Component, ConfigSection, Host, Kind, Manifest};

    use super::{MCP_TOOLS, ToolCall, ToolOutput, ToolProvider, ToolSpec};

    /// 组件 `mcp-fake`。克隆出来的是同一个。
    #[derive(Clone, Default)]
    pub struct FakeMcp(Arc<OnceLock<Collector<dyn ToolProvider>>>);

    impl FakeMcp {
        fn providers(&self) -> Vec<Arc<dyn ToolProvider>> {
            self.0.get().map(Collector::all).unwrap_or_default()
        }

        /// 所有登记的工具。
        pub async fn tools(&self) -> Result<Vec<ToolSpec>, AppError> {
            let mut all = Vec::new();
            for p in self.providers() {
                all.extend(p.tools().await?);
            }
            Ok(all)
        }

        /// 调用一个工具。参数按名字给。
        pub async fn call(
            &self,
            name: &str,
            args: &[(&str, &str)],
        ) -> Result<ToolOutput, AppError> {
            for p in self.providers() {
                if p.tools().await?.iter().any(|t| t.name == name) {
                    return p
                        .call(ToolCall {
                            name: name.to_owned(),
                            args: args
                                .iter()
                                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                                .collect(),
                        })
                        .await;
                }
            }
            Err(AppError::not_found(format!("没有工具 `{name}`")))
        }
    }

    impl Component for FakeMcp {
        fn manifest(&self) -> Manifest {
            Manifest {
                id: "mcp-fake",
                kind: Kind::Service,
                summary: "测试用：不碰标准输入输出的工具服务",
                provides: vec![MCP_TOOLS.id()],
                requires: vec![],
                optional_requires: vec![],
            }
        }

        fn start(&self, _config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
            let _ = self.0.set(host.collector(&MCP_TOOLS)?);
            Ok(())
        }
    }
}
