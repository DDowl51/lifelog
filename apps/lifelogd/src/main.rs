//! 启动器 `lifelogd`：常驻进程，记录电脑上的活动。
//!
//! 很薄：读参数 → 启动 → 等信号 → 停止。装哪些组件由 `Cargo.toml` 的依赖决定。

use std::process::ExitCode;

use boot::{App, Shutdown};
use cli_contract::{EXIT_USAGE, exit_code};
use kernel_api::AppError;

include!(concat!(env!("OUT_DIR"), "/components.rs"));

const NAME: &str = "lifelogd";

const USAGE: &str = "\
用法：lifelogd [--config <文件>] [--trace]

记录电脑上的活动，写进数据库。平时由 systemd 用户服务启动；用 `lifelog` 查看记录。

  --config <文件>  用另一个配置文件（默认 ~/.config/lifelog/config.toml）
  --trace          把组件之间的每次请求和事件按行写到 stderr
";

/// 后台任务崩溃时的退出码（sysexits 的 EX_SOFTWARE）。
const EXIT_SOFTWARE: u8 = 70;

async fn run(app: &App) -> Result<Shutdown, AppError> {
    let running = boot::start(app, |_| {}).await?;
    let why = boot::wait(&running).await;
    running.stop().await;
    why
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
            boot::report(
                NAME,
                &AppError::validation(format!("不认识的参数 {other:?}。用 --help 看用法")),
            );
            return ExitCode::from(EXIT_USAGE);
        }
    }

    let app = App {
        name: NAME,
        config_dir: "lifelog",
        known_components: KNOWN_COMPONENTS,
        config: flags.config,
        trace: flags.trace,
        // 写命令（生成摘要）由这个进程做，命令行进程通过 socket 调用
        export: &["summary", "vectors.search"],
        connect: false,
    };
    match boot::block_on(run(&app)).and_then(|outcome| outcome) {
        Ok(Shutdown::Signal) => ExitCode::SUCCESS,
        Ok(Shutdown::Fault(task)) => {
            boot::report(
                NAME,
                &AppError::unavailable(format!("后台任务 {task} 崩溃，退出")),
            );
            ExitCode::from(EXIT_SOFTWARE)
        }
        Err(e) => {
            boot::report(NAME, &e);
            ExitCode::from(exit_code(e.kind))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 依赖里写了的组件都真的链接进来并登记上了。
    #[test]
    fn every_linked_component_is_registered() {
        let registered: Vec<&str> = kernel_api::registered_components()
            .iter()
            .map(|c| c.manifest().id)
            .collect();
        assert_eq!(registered, LINKED_COMPONENTS);
        assert_eq!(
            LINKED_COMPONENTS,
            [
                "activity",
                "afk",
                "browser-chromium",
                "browser-firefox",
                "browser-history",
                "browser-qutebrowser",
                "browsing",
                "clock-system",
                "documents",
                "embed-fastembed",
                "files-std",
                "focus",
                "idle-wayland",
                "llm-openai-compat",
                "log-stderr",
                "notes",
                "privacy",
                "project",
                "query",
                "search",
                "shell",
                "shell-atuin",
                "storage-sqlite",
                "summary",
                "timer-tokio",
                "tokenizer-cjk",
                "vectors",
                "wm-niri"
            ]
        );
    }
}
