//! `shell` 和真实的 SQLite（内存库）装在一起，历史和活动写入用假实现。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use activity_contracts::testing::RecordingWrite;
use clock_contract::testing::FakeClock;
use kernel_api::{
    AppError, Component, ConfigSection, ErrorKind, Host, Kind, Manifest, async_trait,
};
use log_contract::testing::MemoryLog;
use serde_json::{Value, json};
use shell::ShellComponent;
use shell_contract::testing::{MemoryShell, finished};
use shell_contract::{HistoryPage, SHELL_HISTORY, ShellHistory};
use std::sync::Arc;
use storage_sqlite::StorageSqlite;
use timer_contract::testing::ManualTimer;

struct Fixture {
    app: testkit::TestApp,
    timer: ManualTimer,
    recorded: RecordingWrite,
    log: MemoryLog,
}

async fn boot(history: impl Component + 'static, shell: Value) -> Fixture {
    let (timer, recorded, log) = (
        ManualTimer::default(),
        RecordingWrite::default(),
        MemoryLog::default(),
    );
    let app = testkit::app()
        .with(StorageSqlite)
        .config("storage-sqlite", json!({ "path": ":memory:" }))
        .unwrap()
        .with(FakeClock::at(1_000_000, 540))
        .with(log.clone())
        .with(timer.clone())
        .with(recorded.clone())
        .with(history)
        .with(ShellComponent)
        .config("shell", shell)
        .unwrap()
        .boot()
        .await
        .unwrap();
    Fixture {
        app,
        timer,
        recorded,
        log,
    }
}

impl Fixture {
    async fn tick(&self) {
        self.timer.fire_all().await.unwrap();
        self.app.settle().await;
    }

    fn titles(&self) -> Vec<String> {
        self.recorded
            .stored()
            .into_iter()
            .map(|a| a.title.unwrap())
            .collect()
    }
}

#[tokio::test]
async fn imports_new_commands_each_tick_in_batches() {
    let history = MemoryShell::default();
    history.push((1..=5).map(|i| finished(&i.to_string(), i * 1_000, &format!("cmd {i}"))));
    let f = boot(history.clone(), json!({ "batch": 2 })).await;
    f.tick().await;
    assert_eq!(f.titles(), ["cmd 1", "cmd 2", "cmd 3", "cmd 4", "cmd 5"]);
    let first = &f.recorded.stored()[0];
    assert_eq!(first.tz_offset_min, 540);
    assert_eq!(first.dedup_key.as_deref(), Some("shell:1"));

    // 下一轮只交新的
    history.push([finished("6", 6_000, "cmd 6")]);
    f.tick().await;
    assert_eq!(f.recorded.submitted().len(), 6, "没有重复提交");
}

#[tokio::test]
async fn one_tick_is_bounded_and_the_rest_comes_next_time() {
    let history = MemoryShell::default();
    history.push((1..=5).map(|i| finished(&i.to_string(), i * 1_000, "x")));
    let f = boot(history, json!({ "batch": 2, "max_batches": 1 })).await;
    f.tick().await;
    assert_eq!(f.recorded.submitted().len(), 2);
    f.tick().await;
    assert_eq!(f.recorded.submitted().len(), 4);
}

/// 一直不可用的历史。
#[derive(Clone)]
struct Missing;

#[async_trait]
impl ShellHistory for Missing {
    async fn read(&self, _: Option<String>, _: u32) -> Result<HistoryPage, AppError> {
        Err(AppError::unavailable("还没装"))
    }
}

impl Component for Missing {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: "shell-missing",
            kind: Kind::Service,
            summary: "测试用",
            provides: vec![SHELL_HISTORY.id()],
            requires: vec![],
            optional_requires: vec![],
        }
    }

    fn start(&self, _: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        host.provide(&SHELL_HISTORY, Arc::new(self.clone()))
    }
}

#[tokio::test]
async fn unavailable_history_warns_once_and_is_not_a_fault() {
    let f = boot(Missing, json!({})).await;
    f.tick().await;
    f.tick().await;
    let warnings: Vec<String> = f.log.lines();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("还没装"));
    assert!(f.app.faults().is_empty(), "{:?}", f.app.faults());
}

#[tokio::test]
async fn zero_settings_are_rejected() {
    let err = testkit::app()
        .with(StorageSqlite)
        .config("storage-sqlite", json!({ "path": ":memory:" }))
        .unwrap()
        .with(FakeClock::at(0, 0))
        .with(MemoryLog::default())
        .with(ManualTimer::default())
        .with(RecordingWrite::default())
        .with(MemoryShell::default())
        .with(ShellComponent)
        .config("shell", json!({ "poll_secs": 0 }))
        .unwrap()
        .boot()
        .await
        .err()
        .unwrap();
    assert_eq!(err.kind, ErrorKind::Config);
}
