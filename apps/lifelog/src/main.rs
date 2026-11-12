//! 启动器 `lifelog`：命令行，查看记录。
//!
//! 很薄：启动 → 把命令行交给命令行服务 → 用它给的退出码退出。
//! 有哪些子命令由装进来的组件决定，这里一个都不认识。

use std::process::ExitCode;

use boot::App;
use cli_contract::{CLI, EXIT_USAGE, exit_code};
use kernel_api::AppError;

include!(concat!(env!("OUT_DIR"), "/components.rs"));

const NAME: &str = "lifelog";

kernel_api::register_component!(kernel_api::Import::new(
    "vectors-remote",
    "转给常驻进程：按意思检索文档（模型留在常驻进程里）",
    &vectors_contracts::VECTOR_SEARCH
));

// 写命令交给常驻进程：这个进程里的 `Summarize` 是跨进程代理（复杂档）
kernel_api::register_component!(kernel_api::Import::new(
    "summary-remote",
    "转给常驻进程：生成每日摘要",
    &summary_contracts::SUMMARIZE
));

async fn run(app: &App, argv: Vec<String>) -> Result<u8, AppError> {
    // 只有常驻进程写数据库；这个进程只读
    let running = boot::start(app, |settings| {
        settings.set("storage-sqlite", "read_only", serde_json::Value::Bool(true));
    })
    .await?;
    let code = match running.resolve(&CLI) {
        Ok(cli) => cli.run(argv).await,
        Err(e) => Err(e),
    };
    running.stop().await;
    code
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    let flags = match boot::launcher_flags(argv.get(1..).unwrap_or_default()) {
        Ok(flags) => flags,
        Err(e) => {
            boot::report(NAME, &e);
            return ExitCode::from(EXIT_USAGE);
        }
    };

    let app = App {
        name: NAME,
        config_dir: "lifelog",
        known_components: KNOWN_COMPONENTS,
        config: flags.config,
        trace: flags.trace,
        export: &[],
        connect: true,
    };
    match boot::block_on(run(&app, argv)).and_then(|outcome| outcome) {
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
    const IMPORTED: &[&str] = &["summary-remote", "vectors-remote"];

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
                "cli-clap",
                "clock-system",
                "documents",
                "log-stderr",
                "query",
                "search",
                "storage-sqlite",
                "summary-cli",
                "tokenizer-cjk"
            ]
        );
    }
}
