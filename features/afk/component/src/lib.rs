//! 应用功能 `afk`：把离开和回来变成离开时段，交给 `activity`。
//!
//! 和 `focus` 一样只做接线；它不知道「离开」是怎么检测出来的。

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use activity_contracts::{ACTIVITY_WRITE, Activity, ActivityWrite};
use afk_core::AwayTracker;
use clock_contract::{CLOCK, Clock, Moment};
use idle_contract::{IDLE, IDLE_CHANGED, IdleChanged};
use kernel_api::{AppError, Component, ConfigSection, Host, Kind, Manifest, register_component};
use log_contract::{LOG, Logger};
use serde::{Deserialize, Serialize};
use timer_contract::{Fired, TIMER, TIMER_FIRED, TimerId};

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
struct Config {
    /// 进行中的离开时段每隔多少秒写一次。
    flush_interval_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            flush_interval_secs: 30,
        }
    }
}

struct Afk {
    tracker: Mutex<Option<AwayTracker>>,
    timer: OnceLock<TimerId>,
    clock: Arc<dyn Clock>,
    write: Arc<dyn ActivityWrite>,
    log: Logger,
}

impl Afk {
    fn step<T>(&self, now: Moment, f: impl FnOnce(&mut AwayTracker) -> T) -> T {
        let mut slot = self.tracker.lock().unwrap_or_else(PoisonError::into_inner);
        f(slot.get_or_insert_with(|| AwayTracker::new(now.utc_ms)))
    }

    async fn submit(&self, activity: Option<Activity>) -> Result<(), AppError> {
        match activity {
            Some(activity) => self.write.submit(vec![activity]).await.map(|_| ()),
            None => Ok(()),
        }
    }

    async fn on_change(&self, change: IdleChanged) -> Result<(), AppError> {
        let at = match change {
            IdleChanged::Away { at, .. } | IdleChanged::Back { at } => at,
        };
        let activity = self.step(at, |t| t.on_change(change));
        self.submit(activity).await
    }

    async fn on_timer(&self, fired: Fired) -> Result<(), AppError> {
        if self.timer.get() != Some(&fired.id) {
            return Ok(());
        }
        let now = self.clock.now().await?;
        let activity = self.step(now, |t| t.tick(now));
        self.submit(activity).await
    }

    async fn on_stop(&self) {
        let result = async {
            let now = self.clock.now().await?;
            let activity = self.step(now, |t| t.close(now));
            self.submit(activity).await
        }
        .await;
        if let Err(e) = result {
            self.log
                .warn(format!("退出时没能写下最后一个离开时段：{e}"))
                .await;
        }
    }
}

pub struct AfkComponent;

impl Component for AfkComponent {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "afk",
            kind: Kind::Feature,
            summary: "把离开和回来变成离开时段",
            provides: vec![],
            requires: vec![
                IDLE.id(),
                TIMER.id(),
                CLOCK.id(),
                ACTIVITY_WRITE.id(),
                LOG.id(),
            ],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let config: Config = config.parse()?;
        if config.flush_interval_secs == 0 {
            return Err(AppError::config("flush_interval_secs 不能是 0"));
        }
        let timer = host.resolve(&TIMER)?;
        let afk = Arc::new(Afk {
            tracker: Mutex::default(),
            timer: OnceLock::new(),
            clock: host.resolve(&CLOCK)?,
            write: host.resolve(&ACTIVITY_WRITE)?,
            log: Logger::new(host.resolve(&LOG)?, host.id()),
        });

        let a = afk.clone();
        host.subscribe(&IDLE_CHANGED, "track", move |change| {
            let a = a.clone();
            async move { a.on_change(change).await }
        })?;
        let a = afk.clone();
        host.subscribe(&TIMER_FIRED, "flush", move |fired| {
            let a = a.clone();
            async move { a.on_timer(fired).await }
        })?;
        let a = afk.clone();
        host.on_ready(move || async move {
            let id = timer.every(config.flush_interval_secs * 1_000).await?;
            let _ = a.timer.set(id);
            Ok(())
        });
        host.on_stop(move || async move { afk.on_stop().await });
        Ok(())
    }
}

register_component!(AfkComponent);
