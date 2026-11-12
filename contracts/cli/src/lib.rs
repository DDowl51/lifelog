//! 契约：命令行。扩展点：各组件登记自己的子命令。
//!
//! 命令行服务负责解析参数、分派、读写标准流、把错误映射成退出码。
//! 提供子命令的组件只交出两样纯数据的东西：命令的描述，和「参数 → 输出文本」的执行。
//! 它们不碰命令行解析库、不碰标准流、不碰退出码。

use std::collections::BTreeMap;

use kernel_api::{AppError, ErrorKind, contract};
use serde::{Deserialize, Serialize};

/// 一个子命令的描述。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSpec {
    /// 子命令的名字，例如 `day`。
    pub name: String,
    /// 一句话说明，出现在帮助里。
    pub about: String,
    pub args: Vec<ArgSpec>,
}

/// 子命令的一个参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgSpec {
    pub name: String,
    pub help: String,
    pub kind: ArgKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgKind {
    /// 位置参数，可以不给。
    Positional,
    /// `--名字 值`。`values` 非空时只接受其中之一；`default` 是不给时的值。
    Option {
        values: Vec<String>,
        default: Option<String>,
    },
    /// `--名字`，给了就是 `"true"`。
    Flag,
    /// 剩下的全部位置参数，用一个空格连起来。只能是最后一个位置参数；可以不给。
    Rest,
}

/// 输出要什么格式。由命令行服务根据参数和终端情况决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// 给程序读的：一行一个 JSON。
    Json,
    /// 给人看的。`color`：能不能用颜色；`width`：终端有多少列。
    Text { color: bool, width: u16 },
}

/// 一次调用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invocation {
    /// 子命令的名字。
    pub command: String,
    /// 给了的参数（含默认值）。没给的不出现。
    pub args: BTreeMap<String, String>,
    pub format: Format,
}

/// 命令的输出：要写到 stdout 的文本。出错时返回错误，stdout 什么都不写。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Output {
    pub text: String,
}

contract! {
    /// 扩展点：有子命令的组件来登记。
    pub trait CommandProvider as CLI_COMMANDS = point("cli.commands", 1) {
        /// 这个组件提供哪些子命令。
        async fn commands(&self) -> Result<Vec<CommandSpec>, AppError>;

        /// 执行其中一个。
        async fn run(&self, invocation: Invocation) -> Result<Output, AppError>;
    }
}

contract! {
    /// 命令行。
    pub trait Cli as CLI = service("cli", 1) {
        /// 处理一次命令行调用。`argv` 含程序名。
        ///
        /// 结果写到 stdout，错误写到 stderr，返回进程应该用的退出码。
        /// 只有命令行服务自己坏了（比如两个组件登记了同名的子命令）才返回错误。
        async fn run(&self, argv: Vec<String>) -> Result<u8, AppError>;
    }
}

/// 命令行用法不对（参数写错、没有这个子命令）时的退出码。
pub const EXIT_USAGE: u8 = 64;

/// 错误种类对应的退出码（sysexits）。这张表只在这里写一次：
/// 命令行服务用它，启动器在启动失败时也用它。
pub const fn exit_code(kind: ErrorKind) -> u8 {
    match kind {
        ErrorKind::Validation => 65, // EX_DATAERR
        ErrorKind::NotFound => 66,   // EX_NOINPUT
        ErrorKind::VersionConflict => 3,
        ErrorKind::Unauthorized => 77, // EX_NOPERM
        ErrorKind::TempFail => 75,     // EX_TEMPFAIL
        ErrorKind::Unavailable => 69,  // EX_UNAVAILABLE
        ErrorKind::Config => 78,       // EX_CONFIG
    }
}

#[cfg(feature = "testing")]
pub mod testing {
    //! 假的命令行：不解析参数、不碰标准流，直接把一次调用交给登记了那个子命令的组件。
    //! 给登记子命令的功能做测试用。

    use std::collections::BTreeMap;
    use std::sync::{Arc, OnceLock};

    use kernel_api::{AppError, Collector, Component, ConfigSection, Host, Kind, Manifest};

    use super::{CLI_COMMANDS, CommandProvider, CommandSpec, Format, Invocation, Output};

    /// 假的命令行。克隆出来的是同一个。
    #[derive(Clone, Default)]
    pub struct FakeCli(Arc<OnceLock<Collector<dyn CommandProvider>>>);

    impl FakeCli {
        fn providers(&self) -> Result<Vec<Arc<dyn CommandProvider>>, AppError> {
            Ok(self
                .0
                .get()
                .ok_or_else(|| AppError::unavailable("cli-fake 还没启动"))?
                .all())
        }

        /// 所有登记上来的子命令，按名字排序。
        pub async fn commands(&self) -> Result<Vec<CommandSpec>, AppError> {
            let mut all = Vec::new();
            for provider in self.providers()? {
                all.extend(provider.commands().await?);
            }
            all.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(all)
        }

        /// 调用一个子命令。`args` 是已经解析好的（名字，值）。
        pub async fn invoke(
            &self,
            command: &str,
            args: &[(&str, &str)],
            format: Format,
        ) -> Result<Output, AppError> {
            for provider in self.providers()? {
                if provider
                    .commands()
                    .await?
                    .iter()
                    .any(|spec| spec.name == command)
                {
                    let args: BTreeMap<String, String> = args
                        .iter()
                        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                        .collect();
                    return provider
                        .run(Invocation {
                            command: command.to_owned(),
                            args,
                            format,
                        })
                        .await;
                }
            }
            Err(AppError::not_found(format!(
                "没有组件登记子命令 `{command}`"
            )))
        }
    }

    /// 组件 `cli-fake`：只拥有扩展点，不提供 `Cli`。
    impl Component for FakeCli {
        fn manifest(&self) -> Manifest {
            Manifest {
                id: "cli-fake",
                kind: Kind::Service,
                summary: "测试用：假的命令行",
                provides: vec![CLI_COMMANDS.id()],
                requires: vec![],
                optional_requires: vec![],
            }
        }

        fn start(&self, _config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
            let _ = self.0.set(host.collector(&CLI_COMMANDS)?);
            Ok(())
        }
    }
}
