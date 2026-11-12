//! 启动和等待退出。

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use kernel::{Delivery, Export, Options, Running};
use kernel_api::{AppError, registered_components};
use tokio::signal::unix::{SignalKind, signal};

use crate::{Settings, StderrFaults, StderrTrace, config_path, load};

/// 一个启动器对自己的描述。
pub struct App {
    /// 程序名，用在报错前缀里。
    pub name: &'static str,
    /// 配置目录名：`~/.config/<config_dir>/config.toml`。同一套程序的几个进程共用。
    pub config_dir: &'static str,
    /// 仓库里存在的全部组件 id（构建脚本生成），用来区分「别的进程的组件」和「拼写错误」。
    pub known_components: &'static [&'static str],
    /// 配置文件路径；`None` 用默认位置。
    pub config: Option<PathBuf>,
    /// 把监听记录按行写到 stderr。
    pub trace: bool,
    /// 导出给别的进程用的契约（常驻进程）。空表示不导出。
    pub export: &'static [&'static str],
    /// 要不要连常驻进程（进程里有 `Import` 组件时要）。
    pub connect: bool,
}

/// 同一套程序的进程之间用的 socket：`$XDG_RUNTIME_DIR/<config_dir>/daemon.sock`。
/// 没有 `XDG_RUNTIME_DIR` 时退到临时目录下按用户名区分的目录。
pub fn socket_path(config_dir: &str) -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map_or_else(
            || {
                let user = std::env::var("USER").unwrap_or_else(|_| "user".to_owned());
                std::env::temp_dir().join(format!("{config_dir}-{user}"))
            },
            |runtime| runtime.join(config_dir),
        );
    base.join("daemon.sock")
}

/// 读配置、收集链接进来的组件、启动。`adjust` 让启动器覆盖配置。
pub async fn start(app: &App, adjust: impl FnOnce(&mut Settings)) -> Result<Running, AppError> {
    let path = app
        .config
        .clone()
        .unwrap_or_else(|| config_path(app.config_dir));
    let mut settings = load(&path)?;
    adjust(&mut settings);

    let components = registered_components();
    let linked: Vec<&str> = components.iter().map(|c| c.manifest().id).collect();
    let configs = settings.for_process(&linked, app.known_components)?;

    let options = Options {
        trace: app.trace.then(|| Arc::new(StderrTrace) as _),
        faults: Some(Arc::new(StderrFaults { app: app.name })),
        delivery: Delivery::Background,
        queue_capacity: settings.kernel.queue_capacity,
        stop_timeout: settings.kernel.stop_timeout(),
        export: (!app.export.is_empty()).then(|| Export {
            socket: socket_path(app.config_dir),
            contracts: app.export.iter().map(|c| (*c).to_owned()).collect(),
        }),
        connect: app.connect.then(|| socket_path(app.config_dir)),
    };
    kernel::start(components, &configs, options).await
}

/// 常驻进程为什么要退出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shutdown {
    /// 收到 SIGINT 或 SIGTERM。
    Signal,
    /// 有后台任务崩溃，值是任务名。应该以失败退出，交给进程管理器重启。
    Fault(String),
}

/// 等到该退出为止。
pub async fn wait(running: &Running) -> Result<Shutdown, AppError> {
    let fail = |e: std::io::Error| AppError::unavailable(format!("装不上信号处理：{e}"));
    let mut interrupt = signal(SignalKind::interrupt()).map_err(fail)?;
    let mut terminate = signal(SignalKind::terminate()).map_err(fail)?;
    Ok(tokio::select! {
        _ = interrupt.recv() => Shutdown::Signal,
        _ = terminate.recv() => Shutdown::Signal,
        label = running.fault() => Shutdown::Fault(label),
    })
}

/// 建一个运行时并跑到结束。启动器的 `main` 用。
pub fn block_on<T>(fut: impl Future<Output = T>) -> Result<T, AppError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| AppError::unavailable(format!("建不了异步运行时：{e}")))?;
    Ok(runtime.block_on(fut))
}
