//! 系统服务 `wm-niri`：连 niri 的事件流，提供 `Windows`。
//!
//! niri 相关的东西全在这个包里：socket、消息格式、窗口表。
//! 契约里没有 niri 这个词，别的组件也搜不到它。

mod ipc;
pub mod replay;
mod table;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use clock_contract::{CLOCK, Clock, Moment};
use kernel_api::{
    AppError, Component, ConfigSection, Host, Kind, Manifest, Publisher, async_trait,
    register_component,
};
use log_contract::{LOG, Logger};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use window_contract::{FOCUS_CHANGED, FocusChanged, WINDOWS, WindowInfo, Windows};

use crate::table::Table;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
struct Config {
    /// niri 的 socket。不写时用环境变量 `NIRI_SOCKET`。
    socket: Option<String>,
    /// 断线后第一次重连前等多久；之后每次翻倍。
    reconnect_initial_ms: u64,
    /// 重连等待的上限。
    reconnect_max_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            socket: None,
            reconnect_initial_ms: 1_000,
            reconnect_max_ms: 60_000,
        }
    }
}

/// 窗口表加上发布。真实连接和回放共用。
struct Engine {
    table: Mutex<Table>,
    publisher: Publisher<FocusChanged>,
}

impl Engine {
    fn new(publisher: Publisher<FocusChanged>) -> Self {
        Self {
            table: Mutex::default(),
            publisher,
        }
    }

    fn table(&self) -> std::sync::MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 看 niri 的一行消息。返回（这一行是不是我们关心的事件，焦点有没有因此变化）。
    /// 不发布：发布要带时刻，而大多数行不会引起变化，没必要为它们去问时钟。
    fn observe(&self, line: &str) -> Result<(bool, Option<Option<WindowInfo>>), AppError> {
        let event = ipc::parse_line(line)
            .map_err(|e| AppError::validation(format!("niri 的消息看不懂：{e}")))?;
        Ok(match event {
            Some(event) => (true, self.table().apply(event)),
            None => (false, None),
        })
    }

    /// 回放用：在给定的时刻处理一行。返回这一行是不是我们关心的事件。
    async fn line(&self, line: &str, at: Moment) -> Result<bool, AppError> {
        let (relevant, change) = self.observe(line)?;
        self.publish(change, at).await?;
        Ok(relevant)
    }

    async fn disconnected(&self, at: Moment) -> Result<(), AppError> {
        let change = self.table().disconnect();
        self.publish(change, at).await
    }

    async fn publish(
        &self,
        change: Option<Option<WindowInfo>>,
        at: Moment,
    ) -> Result<(), AppError> {
        match change {
            Some(window) => self.publisher.publish(FocusChanged { at, window }).await,
            None => Ok(()),
        }
    }
}

struct Service(Arc<Engine>);

#[async_trait]
impl Windows for Service {
    async fn focused(&self) -> Result<Option<WindowInfo>, AppError> {
        Ok(self.0.table().current())
    }
}

/// 连上去读到断开为止。返回（有没有连上过，为什么结束）。
async fn stream(engine: &Engine, clock: &dyn Clock, log: &Logger, socket: &str) -> (bool, String) {
    let mut conn = match UnixStream::connect(socket).await {
        Ok(conn) => conn,
        Err(e) => return (false, format!("连不上 {socket}：{e}")),
    };
    if let Err(e) = conn.write_all(b"\"EventStream\"\n").await {
        return (false, format!("请求事件流失败：{e}"));
    }
    let mut lines = BufReader::new(conn).lines();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => return (true, "niri 关闭了连接".to_owned()),
            Err(e) => return (true, format!("读取失败：{e}")),
        };
        let change = match engine.observe(&line) {
            Ok((_, Some(change))) => change,
            Ok((_, None)) => continue,
            // 一行看不懂不影响后面的
            Err(e) => {
                log.warn(e.message).await;
                continue;
            }
        };
        let at = match clock.now().await {
            Ok(at) => at,
            Err(e) => return (true, format!("读不到时钟：{e}")),
        };
        if let Err(e) = engine.publish(Some(change), at).await {
            return (true, format!("发布失败：{e}"));
        }
    }
}

pub struct WmNiri;

impl Component for WmNiri {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "wm-niri",
            kind: Kind::Service,
            summary: "连 niri 的事件流，报告哪个窗口有焦点",
            provides: vec![WINDOWS.id()],
            requires: vec![CLOCK.id(), LOG.id()],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let config: Config = config.parse()?;
        let socket = config
            .socket
            .clone()
            .or_else(|| std::env::var("NIRI_SOCKET").ok())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AppError::config(
                    "找不到 niri 的 socket：环境变量 NIRI_SOCKET 没有设置，配置里也没有写 socket。\
                     不在 niri 会话里运行的话，停用这个组件：[components.wm-niri] enabled = false",
                )
            })?;
        let clock = host.resolve(&CLOCK)?;
        let log = Logger::new(host.resolve(&LOG)?, host.id());
        let engine = Arc::new(Engine::new(host.publisher(&FOCUS_CHANGED)?));

        let task_engine = engine.clone();
        host.spawn("stream", move |cancel| async move {
            let engine = task_engine;
            let initial = Duration::from_millis(config.reconnect_initial_ms.max(1));
            let max = Duration::from_millis(config.reconnect_max_ms).max(initial);
            let mut delay = initial;
            loop {
                let (connected, why) = tokio::select! {
                    () = cancel.cancelled() => break,
                    outcome = stream(&engine, clock.as_ref(), &log, &socket) => outcome,
                };
                if connected {
                    delay = initial;
                    // 之前在记的焦点到此为止
                    if let Ok(at) = clock.now().await {
                        let _ = engine.disconnected(at).await;
                    }
                }
                log.warn(format!("{why}；{} 毫秒后重连", delay.as_millis()))
                    .await;
                tokio::select! {
                    () = cancel.cancelled() => break,
                    () = tokio::time::sleep(delay) => {}
                }
                delay = (delay * 2).min(max);
            }
        });

        host.provide(&WINDOWS, Arc::new(Service(engine)))
    }
}

register_component!(WmNiri);

#[cfg(test)]
mod tests;
