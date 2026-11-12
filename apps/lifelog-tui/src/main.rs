//! 启动器 `lifelog-tui`：终端界面，翻看记录。
//!
//! 很薄：启动 → 把终端交给界面服务 → 退出。有哪些页面由装进来的组件决定。

use std::process::ExitCode;

use boot::App;
use cli_contract::{EXIT_USAGE, exit_code};
use kernel_api::AppError;
use tui_contract::TUI;

include!(concat!(env!("OUT_DIR"), "/components.rs"));

const NAME: &str = "lifelog-tui";

kernel_api::register_component!(kernel_api::Import::new(
    "vectors-remote",
    "转给常驻进程：按意思检索文档（模型留在常驻进程里）",
    &vectors_contracts::VECTOR_SEARCH
));

const USAGE: &str = "\
用法：lifelog-tui [--config <文件>]

在终端里翻看记录：按天、按时间顺序、按周。进去之后按 q 退出，其余按键见最下面一行。

  --config <文件>  用另一个配置文件（默认 ~/.config/lifelog/config.toml）
  --trace          把组件之间的每次请求和事件按行写到 stderr（会弄乱界面，配合 2>文件 使用）
";

async fn run(app: &App) -> Result<u8, AppError> {
    // 只有常驻进程写数据库；这个进程只读
    let running = boot::start(app, |settings| {
        settings.set("storage-sqlite", "read_only", serde_json::Value::Bool(true));
    })
    .await?;
    let code = match running.resolve(&TUI) {
        Ok(tui) => tui.run().await,
        Err(e) => Err(e),
    };
    running.stop().await;
    code
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flags = match boot::launcher_flags(&args) {
        Ok(flags) => flags,
        Err(e) => {
            boot::report(NAME, &e);
            return ExitCode::from(EXIT_USAGE);
        }
    };
    match flags.rest.as_slice() {
        [] => {}
        [help] if help == "--help" || help == "-h" => {
            boot::say(USAGE);
            return ExitCode::SUCCESS;
        }
        [other, ..] => {
            let e = AppError::validation(format!("不认识的参数 {other:?}。用 --help 看用法"));
            boot::report(NAME, &e);
            return ExitCode::from(EXIT_USAGE);
        }
    }

    let app = App {
        name: NAME,
        config_dir: "lifelog",
        known_components: KNOWN_COMPONENTS,
        config: flags.config,
        trace: flags.trace,
        export: &[],
        connect: true,
    };
    match boot::block_on(run(&app)).and_then(|outcome| outcome) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            boot::report(NAME, &e);
            ExitCode::from(exit_code(e.kind))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 从常驻进程导入的组件（不在 Cargo.toml 的依赖里，是上面登记的代理）。
    const IMPORTED: &[&str] = &["vectors-remote"];

    /// 依赖里写了的组件都真的链接进来并登记上了。
    #[test]
    fn every_linked_component_is_registered() {
        let registered: Vec<&str> = kernel_api::registered_components()
            .iter()
            .map(|c| c.manifest().id)
            .filter(|id| !IMPORTED.contains(id))
            .collect();
        assert_eq!(registered, LINKED_COMPONENTS);
        assert_eq!(
            LINKED_COMPONENTS,
            [
                "activity",
                "clock-system",
                "documents",
                "log-stderr",
                "query",
                "search",
                "storage-sqlite",
                "tokenizer-cjk",
                "tui-ratatui"
            ]
        );
    }
}
