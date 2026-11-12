//! `focus` 的组件测试。
//!
//! 前一半全用假实现（假窗口管理器、手动定时器、假时钟、只记录的 activity），
//! 后一半把真实的 niri 消息解析、SQLite、activity 都装上，回放一段录制，
//! 和旧实现（`~/work/projects/lifelog` 的 `ll_source_niri`）的输出对照。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use activity::ActivityComponent;
use activity_contracts::testing::RecordingWrite;
use activity_contracts::{ACTIVITY_READ, ActivityQuery, TimeRange};
use clock_contract::Moment;
use clock_contract::testing::FakeClock;
use focus::FocusComponent;
use kernel_api::ErrorKind;
use log_contract::testing::MemoryLog;
use serde::Deserialize;
use serde_json::{Value, json};
use storage_sqlite::StorageSqlite;
use testkit::TestApp;
use timer_contract::testing::ManualTimer;
use timer_contract::{TIMER, TimerId};
use window_contract::testing::{FakeWindows, window};
use wm_niri::replay::NiriReplay;

/// 上海时间 2026-10-20 09:00:00 之后第 `secs` 秒。
const BASE_MS: i64 = 1_792_458_000_000;

fn at(secs: i64) -> Moment {
    Moment {
        utc_ms: BASE_MS + secs * 1_000,
        offset_min: 480,
    }
}

// ---------- 全用假实现 ----------

struct Fakes {
    app: TestApp,
    windows: FakeWindows,
    timer: ManualTimer,
    clock: FakeClock,
    recorded: RecordingWrite,
}

async fn fakes(config: Value) -> Fakes {
    let (windows, timer, clock, recorded) = (
        FakeWindows::default(),
        ManualTimer::default(),
        FakeClock::at(BASE_MS, 480),
        RecordingWrite::default(),
    );
    let app = testkit::app()
        .with(windows.clone())
        .with(timer.clone())
        .with(clock.clone())
        .with(MemoryLog::default())
        .with(recorded.clone())
        .with(FocusComponent)
        .config("focus", config)
        .unwrap()
        .boot()
        .await
        .unwrap();
    Fakes {
        app,
        windows,
        timer,
        clock,
        recorded,
    }
}

impl Fakes {
    async fn focus(&self, secs: i64, w: Option<window_contract::WindowInfo>) {
        self.clock.set(at(secs).utc_ms);
        self.windows.focus(at(secs), w).await.unwrap();
        self.app.settle().await;
    }

    async fn tick(&self, secs: i64) {
        self.clock.set(at(secs).utc_ms);
        self.timer.fire_all().await.unwrap();
        self.app.settle().await;
    }

    /// 入库后会是什么样：(开始秒, 结束秒, 应用, 标题)。
    fn spans(&self) -> Vec<(i64, i64, String, String)> {
        self.recorded
            .stored()
            .into_iter()
            .map(|a| {
                (
                    (a.ts_start - BASE_MS) / 1_000,
                    (a.ts_end.unwrap() - BASE_MS) / 1_000,
                    a.app.unwrap_or_default(),
                    a.title.unwrap_or_default(),
                )
            })
            .collect()
    }
}

fn span(start: i64, end: i64, app: &str, title: &str) -> (i64, i64, String, String) {
    (start, end, app.to_owned(), title.to_owned())
}

#[tokio::test]
async fn turns_focus_changes_into_spans() {
    let f = fakes(json!({})).await;
    f.focus(0, Some(window(1, "ghostty", "◐ nvim"))).await;
    f.focus(1, Some(window(1, "ghostty", "◑ nvim"))).await; // 抖动
    f.focus(750, Some(window(2, "zen", "Rust — Zen Browser")))
        .await;
    f.focus(900, None).await;

    assert_eq!(
        f.spans(),
        [
            span(0, 750, "ghostty", "nvim"),
            span(750, 900, "zen", "Rust")
        ]
    );
    // 打开各写一次，关闭各写一次；抖动没有产生写入
    assert_eq!(f.recorded.submitted().len(), 4);
}

#[tokio::test]
async fn the_timer_flushes_the_open_span() {
    let f = fakes(json!({ "flush_interval_secs": 10 })).await;
    assert_eq!(f.timer.registered(), [(TimerId(1), 10_000)], "间隔来自配置");

    f.tick(5).await;
    assert!(
        f.recorded.submitted().is_empty(),
        "没有焦点时定时器什么都不写"
    );

    f.focus(10, Some(window(1, "ghostty", "nvim"))).await;
    f.tick(20).await;
    f.tick(30).await;
    assert_eq!(f.spans(), [span(10, 30, "ghostty", "nvim")]);
    assert_eq!(f.recorded.submitted().len(), 3, "打开一次，刷新两次");
}

#[tokio::test]
async fn ignores_timers_that_belong_to_others() {
    let f = fakes(json!({})).await;
    // 别的组件也登记了一个定时器
    let other = f.app.resolve(&TIMER).unwrap().every(1_000).await.unwrap();
    f.focus(0, Some(window(1, "ghostty", "nvim"))).await;
    f.tick(30).await;
    // 两个定时器各触发一次，focus 只认自己的那个
    assert_eq!(f.timer.registered().len(), 2);
    assert_ne!(other, TimerId(1));
    assert_eq!(f.recorded.submitted().len(), 2);
}

#[tokio::test]
async fn stopping_closes_the_open_span() {
    let f = fakes(json!({})).await;
    f.focus(0, Some(window(1, "ghostty", "nvim"))).await;
    f.clock.set(at(42).utc_ms);
    let recorded = f.recorded.clone();
    f.app.stop().await;
    let stored = recorded.stored();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].ts_end, Some(at(42).utc_ms));
}

#[tokio::test]
async fn title_suffixes_come_from_config() {
    let f = fakes(json!({ "title_suffixes": [" | MyBrowser"] })).await;
    f.focus(0, Some(window(1, "b", "Docs | MyBrowser"))).await;
    f.focus(5, Some(window(1, "b", "Docs — Zen Browser"))).await;
    f.focus(9, None).await;
    assert_eq!(
        f.spans(),
        [
            span(0, 5, "b", "Docs"),
            span(5, 9, "b", "Docs — Zen Browser")
        ]
    );
}

#[tokio::test]
async fn moving_the_cursor_in_kakoune_is_not_a_new_span() {
    let f = fakes(json!({})).await;
    let title = |pos: &str| format!("/tmp/notes.txt {pos}  1 sel - client0@[466213] - Kakoune");
    f.focus(0, Some(window(1, "com.mitchellh.ghostty", &title("1:1"))))
        .await;
    f.focus(1, Some(window(1, "com.mitchellh.ghostty", &title("2:1"))))
        .await;
    f.focus(2, Some(window(1, "com.mitchellh.ghostty", &title("31:7"))))
        .await;
    f.focus(
        3,
        Some(window(
            1,
            "com.mitchellh.ghostty",
            "/tmp/other.md 1:1  1 sel - client0@[466213] - Kakoune",
        )),
    )
    .await;
    f.focus(9, None).await;
    assert_eq!(
        f.spans(),
        [
            span(0, 3, "com.mitchellh.ghostty", "/tmp/notes.txt - Kakoune"),
            span(3, 9, "com.mitchellh.ghostty", "/tmp/other.md - Kakoune"),
        ]
    );
}

#[tokio::test]
async fn a_bad_rewrite_fails_startup() {
    let err = testkit::app()
        .with(FakeWindows::default())
        .with(ManualTimer::default())
        .with(FakeClock::at(0, 0))
        .with(MemoryLog::default())
        .with(RecordingWrite::default())
        .with(FocusComponent)
        .config(
            "focus",
            json!({ "title_rewrites": [{ "pattern": "(", "replace": "" }] }),
        )
        .unwrap()
        .boot()
        .await
        .err()
        .unwrap();
    assert_eq!(err.kind, ErrorKind::Config);
    assert!(
        err.message.contains("`focus`") && err.message.contains("标题改写规则"),
        "{err}"
    );
}

#[tokio::test]
async fn only_talks_through_contracts() {
    let f = fakes(json!({})).await;
    f.focus(0, Some(window(1, "ghostty", "nvim"))).await;
    f.tick(30).await;
    assert_eq!(
        f.app.requests(),
        [
            "focus -> timer-manual timer.every = ok",
            "focus -> activity-recorder activity.write.submit = ok",
            "focus -> clock-fake clock.now = ok",
            "focus -> activity-recorder activity.write.submit = ok",
        ]
    );
    assert!(f.app.faults().is_empty());
}

#[tokio::test]
async fn zero_flush_interval_is_rejected() {
    let err = testkit::app()
        .with(FakeWindows::default())
        .with(ManualTimer::default())
        .with(FakeClock::at(0, 0))
        .with(MemoryLog::default())
        .with(RecordingWrite::default())
        .with(FocusComponent)
        .config("focus", json!({ "flush_interval_secs": 0 }))
        .unwrap()
        .boot()
        .await
        .err()
        .unwrap();
    assert_eq!(err.kind, ErrorKind::Config);
    assert!(
        err.message.contains("`focus`") && err.message.contains("flush_interval_secs"),
        "{err}"
    );
}

#[tokio::test]
async fn cannot_start_without_a_window_manager() {
    let err = testkit::app()
        .with(ManualTimer::default())
        .with(FakeClock::at(0, 0))
        .with(MemoryLog::default())
        .with(RecordingWrite::default())
        .with(FocusComponent)
        .boot()
        .await
        .err()
        .unwrap();
    assert!(
        err.message.contains("`focus`") && err.message.contains("windows@1"),
        "{err}"
    );
}

// ---------- 回放录制，和旧实现对照 ----------

/// 录制文件的一行。格式沿用旧仓库：
/// `{"at": "<带时区的时刻>", "niri": {niri 的原始消息}}` 或 `{"at": …, "disconnect": true}`。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Line {
    at: jiff::Zoned,
    #[serde(default)]
    niri: Option<Value>,
    #[serde(default)]
    disconnect: bool,
}

fn moment(z: &jiff::Zoned) -> Moment {
    Moment {
        utc_ms: z.timestamp().as_millisecond(),
        offset_min: z.offset().seconds() / 60,
    }
}

fn utc(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms).unwrap().to_string()
}

/// 一个时段：(开始, 结束, 应用, 标题, 窗口编号, 去重键)。
type Row = (String, String, String, String, u64, String);

/// 回放一个文件：按录制的时刻喂消息，从第一行起每 30 秒触发一次定时器。
///
/// 读完文件就直接去查库，不执行停止：相当于程序在最后一行之后崩溃了，
/// 进行中的时段停在最后一次刷新的时刻。
async fn replay(text: &str) -> Vec<Row> {
    let (niri, timer, clock) = (
        NiriReplay::default(),
        ManualTimer::default(),
        FakeClock::at(0, 480),
    );
    let app = testkit::app()
        .with(niri.clone())
        .with(timer.clone())
        .with(clock.clone())
        .with(MemoryLog::default())
        .with(StorageSqlite)
        .config("storage-sqlite", json!({ "path": ":memory:" }))
        .unwrap()
        .with(ActivityComponent)
        .with(FocusComponent)
        .boot()
        .await
        .unwrap();

    let flush_ms = 30_000;
    let mut next_tick = None;
    for raw in text.lines().filter(|l| !l.is_empty()) {
        let line: Line = serde_json::from_str(raw).unwrap();
        let now = moment(&line.at);

        let tick = next_tick.get_or_insert(now.utc_ms + flush_ms);
        while *tick <= now.utc_ms {
            clock.set(*tick);
            timer.fire_all().await.unwrap();
            app.settle().await;
            *tick += flush_ms;
        }

        clock.set(now.utc_ms);
        if let Some(message) = line.niri {
            niri.feed(now, &message.to_string()).await.unwrap();
        }
        if line.disconnect {
            niri.disconnect(now).await.unwrap();
        }
        app.settle().await;
    }
    assert!(app.faults().is_empty(), "{:?}", app.faults());

    let whole = ActivityQuery {
        range: TimeRange {
            start: 0,
            end: i64::MAX,
        },
        source: Some("focus".to_owned()),
        kind: None,
    };
    let stored = app
        .resolve(&ACTIVITY_READ)
        .unwrap()
        .list(whole)
        .await
        .unwrap();
    stored
        .into_iter()
        .map(|s| {
            let a = s.activity;
            (
                utc(a.ts_start),
                utc(a.ts_end.unwrap()),
                a.app.unwrap_or_default(),
                a.title.unwrap_or_default(),
                a.payload["window_id"].as_u64().unwrap(),
                a.dedup_key.unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn replaying_the_recording_matches_the_old_implementation() {
    let got = replay(include_str!("fixtures/morning.jsonl")).await;

    // 旧实现的输出：
    //   cargo run -p ll_source_niri --example replay -- scratch/m1-4/morning.jsonl --jsonl
    // 只有两处按设计不同：来源从 `niri` 改成 `focus`，去重键的前缀跟着改。
    let ghostty = "com.mitchellh.ghostty";
    let expected = [
        (
            "01:00:00",
            "01:12:30",
            ghostty,
            "nvim ~/work/projects/lifelog",
            1,
        ),
        ("01:12:30", "01:20:00", "zen", "Rust 程序设计语言", 2),
        ("01:20:00", "01:25:00", "zen", "rusqlite - Rust", 2),
        (
            "01:25:00",
            "01:25:05",
            ghostty,
            "nvim ~/work/projects/lifelog",
            1,
        ),
        ("01:25:05", "01:40:00", ghostty, "cargo test", 1),
        (
            "01:40:00",
            "01:40:20",
            "org.keepassxc.KeePassXC",
            "KeePassXC",
            4,
        ),
        ("01:40:20", "01:50:00", ghostty, "cargo test", 1),
        // 09:50:00 断线，09:50:03 重连
        ("01:50:03", "01:51:00", ghostty, "cargo test", 1),
        // 录制到 10:01:15 没有正常退出：停在最后一次刷新的 10:01:00
        (
            "01:55:00",
            "02:01:00",
            "org.telegram.desktop",
            "Telegram",
            3,
        ),
    ];
    let expected: Vec<_> = expected
        .into_iter()
        .zip(1..)
        .map(|((start, end, app, title, window), seq)| {
            (
                format!("2026-10-20T{start}Z"),
                format!("2026-10-20T{end}Z"),
                app.to_owned(),
                title.to_owned(),
                window,
                format!("focus:{BASE_MS}:{seq}"),
            )
        })
        .collect();
    assert_eq!(got, expected);
}
