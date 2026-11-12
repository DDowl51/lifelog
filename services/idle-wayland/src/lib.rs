//! 系统服务 `idle-wayland`：用 Wayland 的 ext-idle-notify-v1 协议报告用户离开和回来。
//!
//! 合成器在「多久没有操作」之后通知我们；这个时长是配置。

mod wayland;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use clock_contract::{CLOCK, Clock, Moment};
use idle_contract::{IDLE, IDLE_CHANGED, Idle, IdleChanged};
use kernel_api::{
    AppError, Component, ConfigSection, Host, Kind, Manifest, async_trait, register_component,
};
use log_contract::{LOG, Logger};
use serde::{Deserialize, Serialize};

use crate::wayland::Signal;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
struct Config {
    /// 多少秒没有键盘鼠标操作算离开。
    timeout_secs: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self { timeout_secs: 300 }
    }
}

/// 合成器的通知变成契约里的事件。离开的起点 = 发现的时刻 - 判定时长。
fn translate(signal: Signal, at: Moment, timeout_ms: i64) -> IdleChanged {
    match signal {
        Signal::Idled => IdleChanged::Away {
            since: Moment {
                utc_ms: at.utc_ms - timeout_ms,
                ..at
            },
            at,
        },
        Signal::Resumed => IdleChanged::Back { at },
    }
}

struct Service {
    away: Arc<AtomicBool>,
}

#[async_trait]
impl Idle for Service {
    async fn away(&self) -> Result<bool, AppError> {
        Ok(self.away.load(Ordering::Relaxed))
    }
}

async fn relay(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Signal>,
    clock: Arc<dyn Clock>,
    away: Arc<AtomicBool>,
    publisher: kernel_api::Publisher<IdleChanged>,
    log: Logger,
    timeout_ms: i64,
    cancel: kernel_api::Cancel,
) {
    loop {
        let signal = tokio::select! {
            () = cancel.cancelled() => return,
            signal = rx.recv() => signal,
        };
        let Some(signal) = signal else { return };
        // 合成器不会连着报两次同一个状态，但这里不依赖它
        let now_away = signal == Signal::Idled;
        if away.swap(now_away, Ordering::Relaxed) == now_away {
            continue;
        }
        let at = match clock.now().await {
            Ok(at) => at,
            Err(e) => {
                log.warn(format!("读不到时钟，这次离开 / 回来没有记下：{e}"))
                    .await;
                continue;
            }
        };
        if publisher
            .publish(translate(signal, at, timeout_ms))
            .await
            .is_err()
        {
            return;
        }
    }
}

pub struct IdleWayland;

impl Component for IdleWayland {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "idle-wayland",
            kind: Kind::Service,
            summary: "用 Wayland 的空闲通知报告用户离开和回来",
            provides: vec![IDLE.id()],
            requires: vec![CLOCK.id(), LOG.id()],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let config: Config = config.parse()?;
        if config.timeout_secs == 0 {
            return Err(AppError::config("timeout_secs 不能是 0"));
        }
        let timeout_ms = config.timeout_secs.saturating_mul(1_000);
        let (queue, state) = wayland::connect(timeout_ms).map_err(|why| {
            AppError::config(format!(
                "{why}。不在 Wayland 会话里运行的话，停用它和依赖它的组件：\
                 [components.idle-wayland] enabled = false、[components.afk] enabled = false"
            ))
        })?;

        let clock = host.resolve(&CLOCK)?;
        let log = Logger::new(host.resolve(&LOG)?, host.id());
        let publisher = host.publisher(&IDLE_CHANGED)?;
        let away = Arc::new(AtomicBool::new(false));

        let task_away = away.clone();
        host.spawn("notifications", move |cancel| async move {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let stop = Arc::new(AtomicBool::new(false));
            let thread_stop = stop.clone();
            let thread = std::thread::Builder::new()
                .name("idle-wayland".to_owned())
                .spawn(move || wayland::pump(queue, state, tx, thread_stop));
            let thread = match thread {
                Ok(thread) => thread,
                Err(e) => {
                    log.error(format!("起不了读合成器消息的线程：{e}")).await;
                    return;
                }
            };
            relay(
                rx,
                clock,
                task_away,
                publisher,
                log.clone(),
                i64::from(timeout_ms),
                cancel,
            )
            .await;

            stop.store(true, Ordering::Relaxed);
            match tokio::task::spawn_blocking(move || thread.join()).await {
                Ok(Ok(Err(why))) => log.error(format!("{why}；之后不再报告离开和回来")).await,
                Ok(Err(_)) => log.error("读合成器消息的线程崩溃了").await,
                _ => {}
            }
        });

        host.provide(&IDLE, Arc::new(Service { away }))
    }
}

register_component!(IdleWayland);

#[cfg(test)]
mod tests {
    use clock_contract::testing::FakeClock;
    use log_contract::testing::MemoryLog;
    use serde_json::json;

    use super::*;

    fn at(ms: i64) -> Moment {
        Moment {
            utc_ms: ms,
            offset_min: 480,
        }
    }

    #[test]
    fn away_starts_one_timeout_before_it_was_noticed() {
        assert_eq!(
            translate(Signal::Idled, at(1_000_000), 300_000),
            IdleChanged::Away {
                since: at(700_000),
                at: at(1_000_000)
            }
        );
        assert_eq!(
            translate(Signal::Resumed, at(5), 300_000),
            IdleChanged::Back { at: at(5) }
        );
    }

    #[test]
    fn config() {
        assert_eq!(
            ConfigSection::default().parse::<Config>().unwrap(),
            Config::default()
        );
        assert!(
            ConfigSection::from_value(json!({ "timeout": 60 }))
                .unwrap()
                .parse::<Config>()
                .is_err()
        );
    }

    /// 需要一个支持 ext-idle-notify-v1 的 Wayland 合成器（比如 niri）。
    /// `cargo test -p idle-wayland -- --ignored`
    #[tokio::test]
    #[ignore = "需要 Wayland 会话"]
    async fn connects_to_the_running_compositor() {
        let app = testkit::app()
            .with(IdleWayland)
            .with(FakeClock::at(0, 480))
            .with(MemoryLog::default())
            .background_delivery()
            .boot()
            .await
            .unwrap();
        assert!(!app.resolve(&IDLE).unwrap().away().await.unwrap());
        app.stop().await;
    }
}
