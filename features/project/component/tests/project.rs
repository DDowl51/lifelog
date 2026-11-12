//! `project` 和真实的 `activity`、`privacy`、内存库装在一起。
//!
//! 重点是两个处理器的先后：归类先看原始标题，脱敏再把标题换掉，项目名留下。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use activity::ActivityComponent;
use activity_contracts::{
    ACTIVITY_READ, ACTIVITY_WRITE, Activity, ActivityQuery, TimeRange, payload_keys,
};
use clock_contract::testing::FakeClock;
use kernel_api::ErrorKind;
use log_contract::testing::MemoryLog;
use privacy::PrivacyComponent;
use project::ProjectComponent;
use serde_json::{Value, json};
use storage_sqlite::StorageSqlite;

fn builder(activity: Value, project: Value) -> testkit::Builder {
    testkit::app()
        .with(StorageSqlite)
        .config("storage-sqlite", json!({ "path": ":memory:" }))
        .unwrap()
        .with(FakeClock::at(1_000_000, 480))
        .with(MemoryLog::default())
        .with(ActivityComponent)
        .config("activity", activity)
        .unwrap()
        .with(PrivacyComponent)
        .with(ProjectComponent)
        .config("project", project)
        .unwrap()
}

async fn round_trip(activity: Value, activities: Vec<Activity>) -> Vec<Activity> {
    let app = builder(activity, json!({ "roots": ["/srv/projects"] }))
        .boot()
        .await
        .unwrap();
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

fn inputs() -> Vec<Activity> {
    vec![
        Activity::new("focus", "window_focus", 0, 480)
            .with_end(60_000)
            .with_app("ghostty")
            .with_title("/srv/projects/lifelog token=abcdef"),
        Activity::new("shell", "command", 1, 480)
            .with_title("cargo test")
            .with_payload(payload_keys::CWD, "/srv/projects/demo/src"),
        Activity::new("focus", "window_focus", 2, 480)
            .with_end(60_000)
            .with_app("zen")
            .with_title("新标签页"),
    ]
}

#[tokio::test]
async fn projects_are_inferred_before_redaction() {
    let stored = round_trip(json!({}), inputs()).await;
    let brief: Vec<(Option<&str>, &str)> = stored
        .iter()
        .map(|a| (a.project.as_deref(), a.title.as_deref().unwrap()))
        .collect();
    assert_eq!(
        brief,
        [
            (Some("lifelog"), "/srv/projects/lifelog token=[redacted]"),
            (Some("demo"), "cargo test"),
            (None, "新标签页"),
        ]
    );
}

#[tokio::test]
async fn the_order_is_policy() {
    // 先脱敏再归类：规则把整个标题换掉的话，归类就看不到路径了
    let stored = round_trip(
        json!({ "processors": ["privacy", "project"] }),
        vec![
            Activity::new("focus", "window_focus", 0, 480)
                .with_end(60_000)
                .with_app("org.keepassxc.KeePassXC")
                .with_title("/srv/projects/vault"),
        ],
    )
    .await;
    assert_eq!(stored[0].title.as_deref(), Some("[redacted]"));
    assert_eq!(stored[0].project, None);
}

#[tokio::test]
async fn bad_config_fails_at_startup() {
    for config in [
        json!({ "roots": ["codes"] }),
        json!({ "rules": [{ "title": "x" }] }),
        json!({ "root": [] }),
    ] {
        let err = builder(json!({}), config.clone())
            .boot()
            .await
            .err()
            .unwrap();
        assert_eq!(err.kind, ErrorKind::Config, "{config}: {err}");
    }
}
