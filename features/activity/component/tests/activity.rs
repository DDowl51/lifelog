//! 把 `activity` 和真实的 SQLite（内存库）、假时钟、内存日志装在一起测。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use activity::ActivityComponent;
use activity_contracts::{
    ACTIVITY_COMMITTED, ACTIVITY_PROCESSORS, ACTIVITY_READ, ACTIVITY_WRITE, Activity, ActivityId,
    ActivityProcessor, ActivityQuery, ActivityRead, ActivityWrite, Committed, DurationRow, GroupBy,
    TimeRange,
};
use clock_contract::testing::FakeClock;
use kernel_api::{
    AppError, Component, ConfigSection, ErrorKind, Host, Kind, Manifest, async_trait,
};
use log_contract::testing::MemoryLog;
use serde_json::json;
use sql_contract::{SQL, Statement};
use storage_sqlite::StorageSqlite;
use testkit::{Builder, TestApp};

fn ts(s: &str) -> i64 {
    s.parse::<jiff::Timestamp>().unwrap().as_millisecond()
}

fn focus(app: &str, start: &str, end: &str) -> Activity {
    Activity::new("focus", "window_focus", ts(start), 480)
        .with_end(ts(end))
        .with_app(app)
}

fn command(at: &str) -> Activity {
    Activity::new("shell", "command", ts(at), 0)
}

fn query(start: &str, end: &str) -> ActivityQuery {
    ActivityQuery {
        range: TimeRange {
            start: ts(start),
            end: ts(end),
        },
        source: None,
        kind: None,
    }
}

const DAY: (&str, &str) = ("2026-10-20T00:00:00Z", "2026-10-21T00:00:00Z");

struct Fixture {
    app: TestApp,
    write: Arc<dyn ActivityWrite>,
    read: Arc<dyn ActivityRead>,
    clock: FakeClock,
    log: MemoryLog,
}

fn base(clock: &FakeClock, log: &MemoryLog) -> Builder {
    testkit::app()
        .with(StorageSqlite)
        .config("storage-sqlite", json!({ "path": ":memory:" }))
        .unwrap()
        .with(clock.clone())
        .with(log.clone())
        .with(ActivityComponent)
}

async fn fixture_with(extend: impl FnOnce(Builder) -> Builder) -> Fixture {
    let (clock, log) = (FakeClock::at(1_000_000, 480), MemoryLog::default());
    let app = extend(base(&clock, &log)).boot().await.unwrap();
    let write = app.resolve(&ACTIVITY_WRITE).unwrap();
    let read = app.resolve(&ACTIVITY_READ).unwrap();
    Fixture {
        app,
        write,
        read,
        clock,
        log,
    }
}

async fn fixture() -> Fixture {
    fixture_with(|b| b).await
}

impl Fixture {
    async fn count(&self) -> i64 {
        let sql = self.app.resolve(&SQL).unwrap();
        let rows = sql
            .query(Statement::new("SELECT count(*) FROM activity_records"))
            .await
            .unwrap();
        rows.rows[0][0].as_i64().unwrap()
    }
}

// ---------- 写入 ----------

#[tokio::test]
async fn round_trip() {
    let f = fixture().await;
    let activity = focus(
        "com.mitchellh.ghostty",
        "2026-10-20T01:00:00.123Z",
        "2026-10-20T01:25:00Z",
    )
    .with_title("nvim")
    .with_project("lifelog")
    .with_payload("cwd", "/home/me")
    .with_payload("window_id", 42)
    .with_dedup_key("focus:1:42");
    let ids = f.write.submit(vec![activity.clone()]).await.unwrap();

    let got = f.read.list(query(DAY.0, DAY.1)).await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(Some(got[0].id), ids[0]);
    assert_eq!(got[0].activity, activity);
    assert_eq!(got[0].ingested_at, 1_000_000);
}

#[tokio::test]
async fn submit_returns_ids_in_order() {
    let f = fixture().await;
    let ids = f
        .write
        .submit(vec![
            focus("a", "2026-10-20T01:00:00Z", "2026-10-20T01:10:00Z"),
            focus("b", "2026-10-20T01:10:00Z", "2026-10-20T01:20:00Z"),
        ])
        .await
        .unwrap();
    assert_eq!(ids.len(), 2);
    assert!(ids[0].unwrap() < ids[1].unwrap());
    assert!(f.write.submit(vec![]).await.unwrap().is_empty());
}

#[tokio::test]
async fn same_dedup_key_updates_in_place() {
    let f = fixture().await;
    let first = focus("ghostty", "2026-10-20T01:00:00Z", "2026-10-20T01:00:30Z")
        .with_dedup_key("focus:1:42");
    let id1 = f.write.submit(vec![first]).await.unwrap()[0];

    // 过了一会儿，同一个时段变长了、标题也有了
    f.clock.advance_ms(30_000);
    let second = focus("ghostty", "2026-10-20T01:00:00Z", "2026-10-20T01:01:00Z")
        .with_title("nvim")
        .with_dedup_key("focus:1:42");
    let id2 = f.write.submit(vec![second.clone()]).await.unwrap()[0];

    assert_eq!(id1, id2);
    assert_eq!(f.count().await, 1);
    let got = f.read.list(query(DAY.0, DAY.1)).await.unwrap();
    assert_eq!(got[0].activity, second);
    assert_eq!(got[0].ingested_at, 1_000_000, "更新不改第一次入库的时刻");
}

#[tokio::test]
async fn activities_without_dedup_key_are_all_kept() {
    let f = fixture().await;
    let a = command("2026-10-20T01:00:00Z");
    f.write.submit(vec![a.clone(), a.clone()]).await.unwrap();
    f.write.submit(vec![a]).await.unwrap();
    assert_eq!(f.count().await, 3);
}

#[tokio::test]
async fn an_invalid_activity_is_dropped_alone() {
    let f = fixture().await;
    let mut bad = command("2026-10-20T01:00:00Z");
    bad.source = "Shell".to_owned();
    let ids = f
        .write
        .submit(vec![
            focus("a", "2026-10-20T01:00:00Z", "2026-10-20T01:10:00Z"),
            bad,
        ])
        .await
        .unwrap();
    assert!(ids[0].is_some() && ids[1].is_none());
    assert_eq!(f.count().await, 1);
    assert_eq!(f.log.lines().len(), 1);
    assert!(
        f.log.lines()[0].starts_with("warn activity: 丢弃第 2 条活动"),
        "{:?}",
        f.log.lines()
    );
}

// ---------- 读取 ----------

#[tokio::test]
async fn list_overlap_rules() {
    let f = fixture().await;
    f.write
        .submit(vec![
            // 在范围之前结束，正好碰到 start：不算重叠
            focus("before", "2026-10-20T00:00:00Z", "2026-10-20T01:00:00Z"),
            // 跨过 start
            focus(
                "cross-start",
                "2026-10-20T00:30:00Z",
                "2026-10-20T01:30:00Z",
            ),
            // 完全在范围内
            focus("inside", "2026-10-20T01:10:00Z", "2026-10-20T01:20:00Z"),
            // 跨过 end
            focus("cross-end", "2026-10-20T01:50:00Z", "2026-10-20T02:10:00Z"),
            // 正好从 end 开始：不算
            focus("after", "2026-10-20T02:00:00Z", "2026-10-20T02:10:00Z"),
            // 瞬时活动：在 start 上算，在 end 上不算
            command("2026-10-20T01:00:00Z").with_app("at-start"),
            command("2026-10-20T02:00:00Z").with_app("at-end"),
        ])
        .await
        .unwrap();

    let got = f
        .read
        .list(query("2026-10-20T01:00:00Z", "2026-10-20T02:00:00Z"))
        .await
        .unwrap();
    let apps: Vec<&str> = got
        .iter()
        .map(|s| s.activity.app.as_deref().unwrap())
        .collect();
    assert_eq!(apps, ["cross-start", "at-start", "inside", "cross-end"]);
}

#[tokio::test]
async fn list_filters_by_source_and_kind() {
    let f = fixture().await;
    f.write
        .submit(vec![
            focus("a", "2026-10-20T01:00:00Z", "2026-10-20T01:10:00Z"),
            command("2026-10-20T01:05:00Z"),
        ])
        .await
        .unwrap();

    let mut q = query(DAY.0, DAY.1);
    q.source = Some("shell".to_owned());
    let got = f.read.list(q.clone()).await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].activity.source, "shell");

    q.source = None;
    q.kind = Some("window_focus".to_owned());
    assert_eq!(f.read.list(q).await.unwrap().len(), 1);
}

#[tokio::test]
async fn durations_are_clipped_and_grouped() {
    let f = fixture().await;
    // 逻辑日 2026-10-20（上海，04:00 起）= UTC 10-19 20:00 ~ 10-20 20:00
    f.write
        .submit(vec![
            // 本地 03:30 ~ 04:30，只算 30 分钟
            focus("ghostty", "2026-10-19T19:30:00Z", "2026-10-19T20:30:00Z"),
            focus("zen", "2026-10-20T01:00:00Z", "2026-10-20T01:40:00Z"),
            focus("ghostty", "2026-10-20T02:00:00Z", "2026-10-20T03:15:00Z"),
            // 没有 app 的时段
            Activity::new("focus", "window_focus", ts("2026-10-20T04:00:00Z"), 0)
                .with_end(ts("2026-10-20T04:05:00Z")),
            // 瞬时活动不参与统计
            Activity::new("focus", "window_focus", ts("2026-10-20T05:00:00Z"), 0).with_app("zen"),
            // 别的日子
            focus("zen", "2026-10-21T01:00:00Z", "2026-10-21T02:00:00Z"),
        ])
        .await
        .unwrap();

    let rows = f
        .read
        .durations(
            query("2026-10-19T20:00:00Z", "2026-10-20T20:00:00Z"),
            GroupBy::App,
        )
        .await
        .unwrap();
    let minutes = |m: i64| m * 60_000;
    assert_eq!(
        rows,
        [
            DurationRow {
                key: Some("ghostty".to_owned()),
                total_ms: minutes(105),
                spans: 2
            },
            DurationRow {
                key: Some("zen".to_owned()),
                total_ms: minutes(40),
                spans: 1
            },
            DurationRow {
                key: None,
                total_ms: minutes(5),
                spans: 1
            },
        ]
    );
}

#[tokio::test]
async fn durations_on_an_empty_range() {
    let f = fixture().await;
    assert!(
        f.read
            .durations(query(DAY.0, DAY.1), GroupBy::Title)
            .await
            .unwrap()
            .is_empty()
    );
}

// ---------- 事件 ----------

/// 订阅「有活动入库」的假功能。
struct Listener(Arc<Mutex<Vec<Committed>>>);

impl Component for Listener {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: "listener",
            kind: Kind::Feature,
            summary: "测试用",
            provides: vec![],
            requires: vec![ACTIVITY_WRITE.id()],
            optional_requires: vec![],
        }
    }

    fn start(&self, _: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let seen = self.0.clone();
        host.subscribe(&ACTIVITY_COMMITTED, "note", move |c: Committed| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(c);
                Ok(())
            }
        })
    }
}

#[tokio::test]
async fn publishes_committed_with_the_stored_ids() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let f = fixture_with(|b| b.with(Listener(seen.clone()))).await;

    let mut bad = command("2026-10-20T01:00:00Z");
    bad.kind = String::new();
    let ids = f
        .write
        .submit(vec![command("2026-10-20T01:00:00Z"), bad])
        .await
        .unwrap();
    f.write.submit(vec![]).await.unwrap();
    f.app.settle().await;

    let expected: Vec<ActivityId> = ids.into_iter().flatten().collect();
    assert_eq!(
        *seen.lock().unwrap(),
        [Committed { ids: expected }],
        "空的一批不发通知"
    );
}

#[tokio::test]
async fn read_by_id_for_incremental_consumers() {
    let f = fixture().await;
    let ids: Vec<ActivityId> = f
        .write
        .submit(vec![
            command("2026-10-20T03:00:00Z").with_title("c"),
            command("2026-10-20T01:00:00Z").with_title("a"),
            command("2026-10-20T02:00:00Z").with_title("b"),
        ])
        .await
        .unwrap()
        .into_iter()
        .flatten()
        .collect();
    let titles = |list: Vec<activity_contracts::StoredActivity>| -> Vec<String> {
        list.into_iter()
            .map(|s| s.activity.title.unwrap())
            .collect()
    };
    assert_eq!(
        titles(f.read.since(None, 10).await.unwrap()),
        ["c", "a", "b"],
        "按编号，不按时间"
    );
    assert_eq!(titles(f.read.since(Some(ids[0]), 1).await.unwrap()), ["a"]);
    assert!(f.read.since(Some(ids[2]), 10).await.unwrap().is_empty());
    assert_eq!(
        titles(
            f.read
                .get(vec![ids[2], ActivityId(999), ids[0]])
                .await
                .unwrap()
        ),
        ["b", "c"],
        "顺序跟请求一致，不存在的跳过"
    );
}

// ---------- 处理器 ----------

/// 往标题后面盖个戳的假功能。
struct Stamp(&'static str);

#[async_trait]
impl ActivityProcessor for Stamp {
    async fn name(&self) -> Result<String, AppError> {
        Ok(self.0.to_owned())
    }

    async fn process(&self, mut activity: Activity) -> Result<Option<Activity>, AppError> {
        let title = activity.title.unwrap_or_default();
        activity.title = Some(format!("{title}[{}]", self.0));
        Ok(Some(activity))
    }
}

impl Component for Stamp {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: self.0,
            kind: Kind::Feature,
            summary: "测试用",
            provides: vec![],
            requires: vec![ACTIVITY_PROCESSORS.id()],
            optional_requires: vec![],
        }
    }

    fn start(&self, _: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        host.contribute(&ACTIVITY_PROCESSORS, Arc::new(Stamp(self.0)))
    }
}

#[tokio::test]
async fn processors_run_in_the_configured_order() {
    let f = fixture_with(|b| {
        b.with(Stamp("project"))
            .with(Stamp("privacy"))
            // 启动顺序是 privacy、project；配置要求反过来
            .config(
                "activity",
                json!({ "processors": ["project", "privacy", "not-installed"] }),
            )
            .unwrap()
    })
    .await;
    f.write
        .submit(vec![command("2026-10-20T01:00:00Z").with_title("t")])
        .await
        .unwrap();
    let got = f.read.list(query(DAY.0, DAY.1)).await.unwrap();
    assert_eq!(
        got[0].activity.title.as_deref(),
        Some("t[project][privacy]")
    );
}

#[tokio::test]
async fn by_default_projects_come_before_privacy() {
    let f = fixture_with(|b| b.with(Stamp("privacy")).with(Stamp("project"))).await;
    f.write
        .submit(vec![command("2026-10-20T01:00:00Z").with_title("t")])
        .await
        .unwrap();
    let got = f.read.list(query(DAY.0, DAY.1)).await.unwrap();
    assert_eq!(
        got[0].activity.title.as_deref(),
        Some("t[project][privacy]")
    );
}

#[tokio::test]
async fn an_enabled_processor_missing_from_the_order_fails_startup() {
    let (clock, log) = (FakeClock::at(0, 0), MemoryLog::default());
    let err = base(&clock, &log)
        .with(Stamp("tagger"))
        .boot()
        .await
        .err()
        .unwrap();
    assert_eq!(err.kind, ErrorKind::Config);
    assert!(
        err.message.contains("`tagger`") && err.message.contains("processors"),
        "{err}"
    );
}

// ---------- 架构 ----------

#[tokio::test]
async fn talks_to_storage_only_through_the_sql_contract() {
    let f = fixture().await;
    f.write
        .submit(vec![command("2026-10-20T01:00:00Z")])
        .await
        .unwrap();
    f.read.list(query(DAY.0, DAY.1)).await.unwrap();
    let requests = f.app.requests();
    assert_eq!(
        &requests[requests.len() - 5..],
        [
            "activity -> clock-fake clock.now = ok",
            "activity -> storage-sqlite sql.execute = ok",
            "launcher -> activity activity.write.submit = ok",
            "activity -> storage-sqlite sql.query = ok",
            "launcher -> activity activity.read.list = ok",
        ]
    );
    assert_eq!(f.app.published(), ["activity.write.committed"]);
}

#[tokio::test]
async fn cannot_start_without_storage() {
    let err = testkit::app()
        .with(FakeClock::at(0, 0))
        .with(MemoryLog::default())
        .with(ActivityComponent)
        .boot()
        .await
        .err()
        .unwrap();
    assert!(
        err.message.contains("`activity`") && err.message.contains("sql"),
        "{err}"
    );
}
