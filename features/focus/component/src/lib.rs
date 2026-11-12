//! 应用功能 `focus`：把焦点变化变成窗口使用时段，交给 `activity`。
//!
//! 这个包只做接线：订阅焦点变化和定时器，把规则（`focus-core`）算出来的活动提交出去。
//! 它不知道窗口管理器是哪一个。

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use activity_contracts::{ACTIVITY_WRITE, Activity, ActivityWrite};
use clock_contract::{CLOCK, Clock, Moment};
use focus_core::{FocusTracker, TitleRules};
use kernel_api::{AppError, Component, ConfigSection, Host, Kind, Manifest, register_component};
use log_contract::{LOG, Logger};
use serde::{Deserialize, Serialize};
use timer_contract::{Fired, TIMER, TIMER_FIRED, TimerId};
use window_contract::{FOCUS_CHANGED, FocusChanged, WINDOWS};

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
struct Config {
    /// 进行中的时段每隔多少秒写一次。崩溃时最多丢这么长。
    flush_interval_secs: u64,
    /// 标题末尾要去掉的应用名后缀。按顺序只去第一个匹配的。
    title_suffixes: Vec<String>,
    /// 标题改写规则，按顺序各替换一次。用来去掉标题里频繁变化、但不代表「换了一件事」的部分。
    title_rewrites: Vec<Rewrite>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Rewrite {
    /// 正则。
    pattern: String,
    /// 替换成什么；可以用 `$1`、`${名字}` 引用分组。
    replace: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            flush_interval_secs: 30,
            title_suffixes: [
                " — Zen Browser",
                " — Mozilla Firefox",
                " - Google Chrome",
                " - Chromium",
            ]
            .map(str::to_owned)
            .to_vec(),
            title_rewrites: vec![Rewrite {
                // kakoune 默认把状态栏写进终端标题：「文件 行:列 选区…… - 客户端@[会话] - Kakoune」，
                // 光标一动标题就变。只留下文件名。
                pattern: r"^(?<file>.+?) \d+:\d+ .* - \S+@\[[^\]]*\] - Kakoune$".to_owned(),
                replace: "${file} - Kakoune".to_owned(),
            }],
        }
    }
}

struct Focus {
    rules: TitleRules,
    /// 第一次用到时才建：这次启动的时刻要从那时的「现在」取。
    tracker: Mutex<Option<FocusTracker>>,
    timer: OnceLock<TimerId>,
    clock: Arc<dyn Clock>,
    write: Arc<dyn ActivityWrite>,
    log: Logger,
}

impl Focus {
    /// 用追踪器算一步。锁只在算的时候拿着，不跨越提交。
    fn step<T>(&self, now: Moment, f: impl FnOnce(&mut FocusTracker) -> T) -> T {
        let mut slot = self.tracker.lock().unwrap_or_else(PoisonError::into_inner);
        let tracker = slot.get_or_insert_with(|| FocusTracker::new(self.rules.clone(), now.utc_ms));
        f(tracker)
    }

    async fn submit(&self, activities: Vec<Activity>) -> Result<(), AppError> {
        if activities.is_empty() {
            return Ok(());
        }
        self.write.submit(activities).await.map(|_| ())
    }

    async fn on_focus(&self, event: FocusChanged) -> Result<(), AppError> {
        let activities = self.step(event.at, |t| t.on_focus(event.window, event.at));
        self.submit(activities).await
    }

    async fn on_timer(&self, fired: Fired) -> Result<(), AppError> {
        if self.timer.get() != Some(&fired.id) {
            return Ok(());
        }
        let now = self.clock.now().await?;
        let activity = self.step(now, |t| t.tick(now));
        self.submit(activity.into_iter().collect()).await
    }

    async fn on_stop(&self) {
        let result = async {
            let now = self.clock.now().await?;
            let activity = self.step(now, |t| t.close(now));
            self.submit(activity.into_iter().collect()).await
        }
        .await;
        if let Err(e) = result {
            self.log
                .warn(format!("退出时没能写下最后一个时段：{e}"))
                .await;
        }
    }
}

pub struct FocusComponent;

impl Component for FocusComponent {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "focus",
            kind: Kind::Feature,
            summary: "把焦点变化变成窗口使用时段",
            provides: vec![],
            requires: vec![
                WINDOWS.id(),
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
        let rewrites: Vec<(String, String)> = config
            .title_rewrites
            .into_iter()
            .map(|r| (r.pattern, r.replace))
            .collect();
        let timer = host.resolve(&TIMER)?;
        let focus = Arc::new(Focus {
            rules: TitleRules::new(config.title_suffixes)
                .with_rewrites(&rewrites)
                .map_err(AppError::config)?,
            tracker: Mutex::default(),
            timer: OnceLock::new(),
            clock: host.resolve(&CLOCK)?,
            write: host.resolve(&ACTIVITY_WRITE)?,
            log: Logger::new(host.resolve(&LOG)?, host.id()),
        });

        let f = focus.clone();
        host.subscribe(&FOCUS_CHANGED, "track", move |event| {
            let f = f.clone();
            async move { f.on_focus(event).await }
        })?;

        let f = focus.clone();
        host.subscribe(&TIMER_FIRED, "flush", move |fired| {
            let f = f.clone();
            async move { f.on_timer(fired).await }
        })?;

        let f = focus.clone();
        host.on_ready(move || async move {
            let id = timer.every(config.flush_interval_secs * 1_000).await?;
            let _ = f.timer.set(id);
            Ok(())
        });

        host.on_stop(move || async move { focus.on_stop().await });
        Ok(())
    }
}

register_component!(FocusComponent);
