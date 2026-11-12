//! `privacy` 和真实的 `activity`、内存库装在一起：提交上去的活动读回来已经脱敏。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use activity::ActivityComponent;
use activity_contracts::{
    ACTIVITY_READ, ACTIVITY_WRITE, Activity, ActivityQuery, TimeRange, payload_keys,
};
use clock_contract::testing::FakeClock;
use kernel_api::ErrorKind;
use log_contract::testing::MemoryLog;
use privacy::PrivacyComponent;
use serde_json::{Value, json};
use storage_sqlite::StorageSqlite;

fn builder(privacy: Value) -> testkit::Builder {
    testkit::app()
        .with(StorageSqlite)
        .config("storage-sqlite", json!({ "path": ":memory:" }))
        .unwrap()
        .with(FakeClock::at(1_000_000, 480))
        .with(MemoryLog::default())
        .with(ActivityComponent)
        .with(PrivacyComponent)
        .config("privacy", privacy)
        .unwrap()
}

/// 提交，再按顺序读回来。
async fn round_trip(privacy: Value, activities: Vec<Activity>) -> Vec<Activity> {
    let app = builder(privacy).boot().await.unwrap();
    app.resolve(&ACTIVITY_WRITE)
        .unwrap()
        .submit(activities)
        .await
        .unwrap();
    let stored = app
        .resolve(&ACTIVITY_READ)
        .unwrap()
        .list(ActivityQuery {
            range: TimeRange {
                start: 0,
                end: i64::MAX,
            },
            source: None,
            kind: None,
        })
        .await
        .unwrap();
    app.stop().await;
    stored.into_iter().map(|s| s.activity).collect()
}

fn window(app: &str, title: &str, start: i64) -> Activity {
    Activity::new("focus", "window_focus", start, 480)
        .with_end(start + 60_000)
        .with_app(app)
        .with_title(title)
}

#[tokio::test]
async fn builtin_rules_apply_by_default() {
    let stored = round_trip(
        json!({}),
        vec![
            window("org.keepassxc.KeePassXC", "工作 - KeePassXC", 0),
            window("ghostty", "echo sk-abcdefghijklmnopqrstuvwx", 1),
            window("ghostty", "~/w/p/lifelog", 2),
        ],
    )
    .await;
    let brief: Vec<(&str, &str, bool)> = stored
        .iter()
        .map(|a| {
            (
                a.app.as_deref().unwrap(),
                a.title.as_deref().unwrap(),
                a.redacted,
            )
        })
        .collect();
    assert_eq!(
        brief,
        [
            ("org.keepassxc.KeePassXC", "[redacted]", true),
            ("ghostty", "echo [redacted]", true),
            ("ghostty", "~/w/p/lifelog", false),
        ]
    );
    assert_eq!(stored[0].ts_end, Some(60_000), "时长照记");
}

#[tokio::test]
async fn builtin_rules_can_be_replaced() {
    let visit = Activity::new("browsing", "visit", 0, 480)
        .with_title("余额")
        .with_payload(payload_keys::URL, "https://www.bank.example/");
    let stored = round_trip(
        json!({
            "builtin_rules": false,
            "exclude": [{ "domain": "bank.example" }],
            "secrets": ["工号(?<secret>\\d+)"],
        }),
        vec![
            visit,
            window("org.keepassxc.KeePassXC", "工作 - KeePassXC", 1),
            window("ghostty", "工号12345", 2),
        ],
    )
    .await;
    assert_eq!(stored[0].title.as_deref(), Some("[redacted]"));
    assert_eq!(stored[0].payload[payload_keys::URL], json!("[redacted]"));
    assert_eq!(
        stored[1].title.as_deref(),
        Some("工作 - KeePassXC"),
        "关掉内置规则后不再排除"
    );
    assert_eq!(stored[2].title.as_deref(), Some("工号[redacted]"));
}

#[tokio::test]
async fn bad_rules_fail_at_startup() {
    for config in [
        json!({ "exclude": [{}] }),
        json!({ "secrets": ["("] }),
        json!({ "exclude": [{ "titel": "x" }] }),
    ] {
        let err = builder(config.clone()).boot().await.err().unwrap();
        assert_eq!(err.kind, ErrorKind::Config, "{config}: {err}");
    }
}

#[tokio::test]
async fn the_excluded_commands_come_from_config() {
    let command = |text: &str, at: i64| Activity::new("shell", "command", at, 480).with_title(text);
    let titles = |stored: Vec<Activity>| -> Vec<String> {
        stored.into_iter().map(|a| a.title.unwrap()).collect()
    };
    let inputs = || vec![command("pass show x", 0), command("bw get item y", 1)];
    assert_eq!(
        titles(round_trip(json!({}), inputs()).await),
        ["[redacted]", "bw get item y"],
        "默认屏蔽 pass、gopass、passage"
    );
    assert_eq!(
        titles(round_trip(json!({ "exclude_commands": ["bw"] }), inputs()).await),
        ["pass show x", "[redacted]"],
        "换成自己的列表"
    );
    assert_eq!(
        titles(round_trip(json!({ "builtin_rules": false }), inputs()).await),
        ["[redacted]", "bw get item y"],
        "和内置规则的开关无关"
    );
    let err = builder(json!({ "exclude_commands": ["pass show"] }))
        .boot()
        .await
        .err()
        .unwrap();
    assert_eq!(err.kind, ErrorKind::Config);
}
