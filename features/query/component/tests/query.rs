//! `query` 的组件测试：真实的 SQLite（内存库）和 activity，假时钟，假命令行。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use activity::ActivityComponent;
use activity_contracts::{ACTIVITY_WRITE, Activity};
use cli_contract::Format;
use cli_contract::testing::FakeCli;
use clock_contract::testing::FakeClock;
use kernel_api::ErrorKind;
use log_contract::testing::MemoryLog;
use query::QueryComponent;
use query_contracts::{By, DayReport, EntryKind, MEMORY, Memory, WeekReport};
use serde_json::{Value, json};
use storage_sqlite::StorageSqlite;
use testkit::{Builder, TestApp};
use tui_contract::PageRequest;
use tui_contract::testing::FakeTui;

fn ts(s: &str) -> i64 {
    s.parse::<jiff::Timestamp>().unwrap().as_millisecond()
}

fn focus(app: &str, title: &str, start: &str, end: &str) -> Activity {
    Activity::new("focus", "window_focus", ts(start), 480)
        .with_end(ts(end))
        .with_app(app)
        .with_title(title)
}

struct Fixture {
    app: TestApp,
    memory: Arc<dyn Memory>,
    cli: FakeCli,
    clock: FakeClock,
}

fn base(clock: &FakeClock, cli: &FakeCli) -> Builder {
    testkit::app()
        .with(StorageSqlite)
        .config("storage-sqlite", json!({ "path": ":memory:" }))
        .unwrap()
        .with(clock.clone())
        .with(MemoryLog::default())
        .with(ActivityComponent)
        .with(cli.clone())
        .with(QueryComponent)
}

/// 「现在」是上海时间 2026-10-20 12:00。库里有 19、20 两个逻辑日的记录。
async fn fixture_with(config: Value) -> Fixture {
    let (clock, cli) = (
        FakeClock::at(ts("2026-10-20T04:00:00Z"), 480),
        FakeCli::default(),
    );
    let app = base(&clock, &cli)
        .config("query", config)
        .unwrap()
        .boot()
        .await
        .unwrap();
    let ghostty = "com.mitchellh.ghostty";
    app.resolve(&ACTIVITY_WRITE)
        .unwrap()
        .submit(vec![
            // 逻辑日 10-19：本地 19 日 22:00–23:00
            focus(
                "zen",
                "旧闻",
                "2026-10-19T14:00:00Z",
                "2026-10-19T15:00:00Z",
            ),
            // 跨过日界：本地 20 日 03:30–04:30，各算 30 分钟
            focus(
                ghostty,
                "nvim",
                "2026-10-19T19:30:00Z",
                "2026-10-19T20:30:00Z",
            ),
            // 逻辑日 10-20
            focus(
                "zen",
                "Rust 程序设计语言",
                "2026-10-20T01:00:00Z",
                "2026-10-20T01:40:00Z",
            ),
            focus(
                ghostty,
                "cargo test",
                "2026-10-20T02:00:00Z",
                "2026-10-20T03:15:00Z",
            ),
            // 别的来源不算窗口时长
            Activity::new("shell", "command", ts("2026-10-20T02:30:00Z"), 480)
                .with_end(ts("2026-10-20T02:31:00Z"))
                .with_app("zsh"),
        ])
        .await
        .unwrap();
    let memory = app.resolve(&MEMORY).unwrap();
    Fixture {
        app,
        memory,
        cli,
        clock,
    }
}

async fn fixture() -> Fixture {
    fixture_with(json!({})).await
}

fn minutes(m: i64) -> i64 {
    m * 60_000
}

fn brief(report: &DayReport) -> Vec<(&str, i64, u64)> {
    report
        .rows
        .iter()
        .map(|r| {
            (
                r.key.as_deref().unwrap_or("-"),
                r.total_ms / 60_000,
                r.spans,
            )
        })
        .collect()
}

const TEXT: Format = Format::Text {
    color: false,
    width: 80,
};

// ---------- Memory 契约 ----------

#[tokio::test]
async fn today_covers_the_logical_day() {
    let f = fixture().await;
    let report = f.memory.day("today".to_owned(), By::App).await.unwrap();
    assert_eq!(report.date, "2026-10-20");
    assert_eq!(report.day_boundary, "04:00");
    assert_eq!(
        (report.start, report.end),
        (ts("2026-10-19T20:00:00Z"), ts("2026-10-20T20:00:00Z"))
    );
    assert_eq!(report.tz_offset_min, 480);
    assert_eq!(
        brief(&report),
        [("com.mitchellh.ghostty", 105, 2), ("zen", 40, 1)]
    );
    assert_eq!(report.total_ms, minutes(145));
}

#[tokio::test]
async fn other_days_and_grouping_by_title() {
    let f = fixture().await;
    let yesterday = f
        .memory
        .day("yesterday".to_owned(), By::Title)
        .await
        .unwrap();
    assert_eq!(yesterday.date, "2026-10-19");
    assert_eq!(brief(&yesterday), [("旧闻", 60, 1), ("nvim", 30, 1)]);

    let explicit = f
        .memory
        .day("2026-10-19".to_owned(), By::Title)
        .await
        .unwrap();
    assert_eq!(explicit, yesterday);
    assert_eq!(
        f.memory.day("1d".to_owned(), By::Title).await.unwrap(),
        yesterday
    );

    let empty = f
        .memory
        .day("2026-01-01".to_owned(), By::App)
        .await
        .unwrap();
    assert!(empty.rows.is_empty());
    assert_eq!(empty.total_ms, 0);
}

#[tokio::test]
async fn before_the_boundary_today_is_still_the_previous_day() {
    let f = fixture().await;
    // 本地 21 日 01:00：还算 20 号
    f.clock.set(ts("2026-10-20T17:00:00Z"));
    assert_eq!(
        f.memory
            .day("today".to_owned(), By::App)
            .await
            .unwrap()
            .date,
        "2026-10-20"
    );
}

#[tokio::test]
async fn a_bad_date_is_a_validation_error() {
    let f = fixture().await;
    let err = f
        .memory
        .day("tomorow".to_owned(), By::App)
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Validation);
    assert!(
        err.message.contains("tomorow") && err.message.contains("yesterday"),
        "{err}"
    );
}

#[tokio::test]
async fn the_day_boundary_comes_from_config() {
    let f = fixture_with(json!({ "day_boundary": "00:00" })).await;
    let report = f.memory.day("today".to_owned(), By::App).await.unwrap();
    assert_eq!(report.start, ts("2026-10-19T16:00:00Z"));
    // 午夜为界：03:30–04:30 那一段整个算今天
    assert_eq!(
        brief(&report),
        [("com.mitchellh.ghostty", 135, 2), ("zen", 40, 1)]
    );
}

#[tokio::test]
async fn hidden_sources_are_not_shown() {
    let f = fixture_with(json!({ "hidden_sources": ["focus"] })).await;
    let report = f.memory.day("today".to_owned(), By::App).await.unwrap();
    assert!(report.rows.is_empty());
}

#[tokio::test]
async fn a_bad_boundary_fails_startup() {
    let (clock, cli) = (FakeClock::at(0, 0), FakeCli::default());
    let err = base(&clock, &cli)
        .config("query", json!({ "day_boundary": "4am" }))
        .unwrap()
        .boot()
        .await
        .err()
        .unwrap();
    assert_eq!(err.kind, ErrorKind::Config);
    assert!(
        err.message.contains("`query`") && err.message.contains("HH:MM"),
        "{err}"
    );
}

// ---------- 离开时间、时间线、周 ----------

fn away(start: &str, end: &str) -> Activity {
    Activity::new("afk", "away", ts(start), 480).with_end(ts(end))
}

async fn add(f: &Fixture, activities: Vec<Activity>) {
    f.app
        .resolve(&ACTIVITY_WRITE)
        .unwrap()
        .submit(activities)
        .await
        .unwrap();
}

#[tokio::test]
async fn away_time_is_not_counted() {
    let f = fixture().await;
    // cargo test 那一段（10:00–11:15）中间离开了 20 分钟
    add(
        &f,
        vec![away("2026-10-20T02:30:00Z", "2026-10-20T02:50:00Z")],
    )
    .await;
    let report = f.memory.day("today".to_owned(), By::App).await.unwrap();
    assert_eq!(
        brief(&report),
        [("com.mitchellh.ghostty", 85, 2), ("zen", 40, 1)]
    );
    assert_eq!(report.total_ms, minutes(125));
    assert_eq!(report.away_ms, minutes(20));
}

#[tokio::test]
async fn hidden_away_is_still_subtracted_but_not_shown() {
    let f = fixture_with(json!({ "hidden_sources": ["afk"] })).await;
    add(
        &f,
        vec![away("2026-10-20T02:30:00Z", "2026-10-20T02:50:00Z")],
    )
    .await;
    let report = f.memory.day("today".to_owned(), By::App).await.unwrap();
    assert_eq!(report.total_ms, minutes(125));
    assert_eq!(report.away_ms, 0);
    let timeline = f.memory.timeline("today".to_owned()).await.unwrap();
    assert!(timeline.entries.iter().all(|e| e.kind == EntryKind::Window));
}

#[tokio::test]
async fn timeline_in_order_with_away() {
    let f = fixture().await;
    add(
        &f,
        vec![away("2026-10-20T02:30:00Z", "2026-10-20T02:50:00Z")],
    )
    .await;
    let timeline = f.memory.timeline("today".to_owned()).await.unwrap();
    let brief: Vec<(String, EntryKind, Option<&str>)> = timeline
        .entries
        .iter()
        .map(|e| {
            let at = jiff::Timestamp::from_millisecond(e.start)
                .unwrap()
                .to_string();
            (at, e.kind, e.title.as_deref())
        })
        .collect();
    assert_eq!(
        brief,
        [
            // 跨日界的那一段从日界开始
            (
                "2026-10-19T20:00:00Z".to_owned(),
                EntryKind::Window,
                Some("nvim")
            ),
            (
                "2026-10-20T01:00:00Z".to_owned(),
                EntryKind::Window,
                Some("Rust 程序设计语言")
            ),
            (
                "2026-10-20T02:00:00Z".to_owned(),
                EntryKind::Window,
                Some("cargo test")
            ),
            ("2026-10-20T02:30:00Z".to_owned(), EntryKind::Away, None),
            (
                "2026-10-20T02:50:00Z".to_owned(),
                EntryKind::Window,
                Some("cargo test")
            ),
        ]
    );

    let out = f.cli.invoke("timeline", &[], TEXT).await.unwrap();
    assert!(
        out.text.contains("  10:30–10:50  20 分  （离开）\n"),
        "{}",
        out.text
    );
}

#[tokio::test]
async fn timeline_thresholds_come_from_config() {
    let f = fixture_with(json!({ "timeline_min_secs": 3600 })).await;
    let timeline = f.memory.timeline("today".to_owned()).await.unwrap();
    let titles: Vec<_> = timeline
        .entries
        .iter()
        .map(|e| e.title.as_deref())
        .collect();
    assert_eq!(titles, [Some("cargo test")], "只剩超过一小时的那一段");
}

#[tokio::test]
async fn brief_switches_fold_into_the_surrounding_entry() {
    let switches = vec![
        focus(
            "zen",
            "文档",
            "2026-10-20T04:00:00Z",
            "2026-10-20T04:10:00Z",
        ),
        focus(
            "ghostty",
            "ls",
            "2026-10-20T04:10:00Z",
            "2026-10-20T04:10:30Z",
        ),
        focus(
            "zen",
            "文档",
            "2026-10-20T04:10:30Z",
            "2026-10-20T04:20:00Z",
        ),
    ];
    let titles = |timeline: &query_contracts::Timeline| -> Vec<(String, usize)> {
        timeline
            .entries
            .iter()
            .filter(|e| e.start >= ts("2026-10-20T04:00:00Z"))
            .map(|e| (e.title.clone().unwrap(), e.during.len()))
            .collect()
    };

    let f = fixture().await;
    add(&f, switches.clone()).await;
    let timeline = f.memory.timeline("today".to_owned()).await.unwrap();
    assert_eq!(titles(&timeline), [("文档".to_owned(), 1)]);
    let last = timeline.entries.last().unwrap();
    assert_eq!(last.during[0].title.as_deref(), Some("ls"));
    assert_eq!(last.during[0].total_ms, 30_000);

    let f = fixture_with(json!({ "timeline_absorb_secs": 0 })).await;
    add(&f, switches).await;
    let timeline = f.memory.timeline("today".to_owned()).await.unwrap();
    assert_eq!(
        titles(&timeline),
        [
            ("文档".to_owned(), 0),
            ("ls".to_owned(), 0),
            ("文档".to_owned(), 0)
        ],
        "0 = 不并"
    );
}

#[tokio::test]
async fn week_from_monday_to_sunday() {
    let f = fixture().await;
    let report = f.memory.week("today".to_owned(), By::App).await.unwrap();
    assert_eq!(report.week_start, "2026-10-19");
    let dates: Vec<&str> = report.days.iter().map(|d| d.date.as_str()).collect();
    assert_eq!(
        dates,
        [
            "2026-10-19",
            "2026-10-20",
            "2026-10-21",
            "2026-10-22",
            "2026-10-23",
            "2026-10-24",
            "2026-10-25"
        ]
    );
    let rows: Vec<(&str, Vec<i64>, i64)> = report
        .rows
        .iter()
        .map(|r| {
            (
                r.key.as_deref().unwrap(),
                r.days_ms.iter().map(|ms| ms / 60_000).collect(),
                r.total_ms / 60_000,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("com.mitchellh.ghostty", vec![30, 105, 0, 0, 0, 0, 0], 135),
            ("zen", vec![60, 40, 0, 0, 0, 0, 0], 100),
        ]
    );
    assert_eq!(report.total_ms, minutes(235));

    // 这一周里任意一天都得到同一周
    assert_eq!(
        f.memory
            .week("2026-10-25".to_owned(), By::App)
            .await
            .unwrap(),
        report
    );
    let out = f.cli.invoke("week", &[("by", "app")], TEXT).await.unwrap();
    assert!(
        out.text
            .starts_with("2026-10-19 ～ 2026-10-25（04:00 起）  共 3 小时 55 分\n"),
        "{}",
        out.text
    );
}

#[tokio::test]
async fn grouping_by_project() {
    let f = fixture().await;
    add(
        &f,
        vec![
            focus(
                "com.mitchellh.ghostty",
                "~/w/p/lifelog",
                "2026-10-20T03:20:00Z",
                "2026-10-20T03:50:00Z",
            )
            .with_project("lifelog"),
        ],
    )
    .await;
    let report = f.memory.day("today".to_owned(), By::Project).await.unwrap();
    assert_eq!(report.by, By::Project);
    assert_eq!(brief(&report), [("-", 145, 3), ("lifelog", 30, 1)]);

    let out = f
        .cli
        .invoke("week", &[("by", "project")], Format::Json)
        .await
        .unwrap();
    let week: WeekReport = serde_json::from_str(&out.text).unwrap();
    assert_eq!(week.by, By::Project);
    assert!(
        week.rows
            .iter()
            .any(|r| r.key.as_deref() == Some("lifelog") && r.total_ms == minutes(30)),
        "{week:?}"
    );
}

#[tokio::test]
async fn commands_of_a_day() {
    let f = fixture().await;
    add(
        &f,
        vec![
            Activity::new("shell", "command", ts("2026-10-20T03:00:00Z"), 480)
                .with_end(ts("2026-10-20T03:00:02Z"))
                .with_title("cargo test")
                .with_project("lifelog")
                .with_payload("exit", 101),
            // 前一个逻辑日的
            Activity::new("shell", "command", ts("2026-10-19T15:00:00Z"), 480).with_title("ls"),
        ],
    )
    .await;
    let out = f.cli.invoke("commands", &[], Format::Json).await.unwrap();
    let commands: query_contracts::Commands = serde_json::from_str(&out.text).unwrap();
    assert_eq!(commands.date, "2026-10-20");
    // 夹具里本来就有一条（10:30，没有标题）；前一个逻辑日的那条不在
    assert_eq!(commands.entries.len(), 2, "{commands:?}");
    let entry = &commands.entries[1];
    assert_eq!(
        (
            entry.command.as_str(),
            entry.exit,
            entry.duration_ms,
            entry.project.as_deref()
        ),
        ("cargo test", Some(101), Some(2_000), Some("lifelog"))
    );
    let text = f.cli.invoke("commands", &[], TEXT).await.unwrap().text;
    assert!(
        text.contains("11:00      2s  lifelog  cargo test  ✗101"),
        "{text}"
    );

    let hidden = fixture_with(json!({ "hidden_sources": ["shell"] })).await;
    add(
        &hidden,
        vec![Activity::new("shell", "command", ts("2026-10-20T03:00:00Z"), 480).with_title("ls")],
    )
    .await;
    let commands = hidden.memory.commands("today".to_owned()).await.unwrap();
    assert!(commands.entries.is_empty(), "隐藏的来源不展示");
}

#[tokio::test]
async fn visits_of_a_day() {
    let f = fixture().await;
    add(
        &f,
        vec![
            Activity::new("browsing", "visit", ts("2026-10-20T03:00:00Z"), 480)
                .with_app("zen")
                .with_title("jiff - Rust")
                .with_payload("url", "https://docs.rs/jiff")
                .with_project("lifelog"),
            Activity::new("browsing", "visit", ts("2026-10-19T15:00:00Z"), 480)
                .with_app("zen")
                .with_payload("url", "https://yesterday/"),
        ],
    )
    .await;
    let out = f.cli.invoke("visits", &[], Format::Json).await.unwrap();
    let visits: query_contracts::Visits = serde_json::from_str(&out.text).unwrap();
    assert_eq!(visits.entries.len(), 1, "{visits:?}");
    let e = &visits.entries[0];
    assert_eq!(
        (
            e.browser.as_deref(),
            e.title.as_deref(),
            e.url.as_str(),
            e.project.as_deref()
        ),
        (
            Some("zen"),
            Some("jiff - Rust"),
            "https://docs.rs/jiff",
            Some("lifelog")
        )
    );
    let text = f.cli.invoke("visits", &[], TEXT).await.unwrap().text;
    assert!(
        text.contains("11:00  zen  lifelog  jiff - Rust  docs.rs"),
        "{text}"
    );
}

#[tokio::test]
async fn a_day_put_together_by_project() {
    let mixed = || {
        vec![
            Activity::new("shell", "command", ts("2026-10-20T02:10:00Z"), 480)
                .with_title("cargo test")
                .with_project("lifelog"),
            Activity::new("shell", "command", ts("2026-10-20T02:20:00Z"), 480)
                .with_title("cargo test")
                .with_project("lifelog"),
            Activity::new("browsing", "visit", ts("2026-10-20T01:05:00Z"), 480)
                .with_app("zen")
                .with_title("Rust 程序设计语言")
                .with_payload("url", "https://kaisery.github.io/trpl-zh-cn/"),
        ]
    };
    let f = fixture().await;
    add(&f, mixed()).await;

    let timeline = f.memory.timeline("today".to_owned()).await.unwrap();
    let cargo = timeline
        .entries
        .iter()
        .find(|e| e.title.as_deref() == Some("cargo test"))
        .unwrap();
    assert_eq!(cargo.commands.len(), 1);
    assert_eq!(
        (cargo.commands[0].text.as_str(), cargo.commands[0].count),
        ("cargo test", 2)
    );
    let rust = timeline
        .entries
        .iter()
        .find(|e| e.title.as_deref() == Some("Rust 程序设计语言"))
        .unwrap();
    assert_eq!(rust.visits[0].detail.as_deref(), Some("kaisery.github.io"));

    let projects = f.memory.projects("today".to_owned()).await.unwrap();
    let brief: Vec<(Option<&str>, i64, u32, u32)> = projects
        .rows
        .iter()
        .map(|r| {
            (
                r.project.as_deref(),
                r.window_ms / 60_000,
                r.commands,
                r.visits,
            )
        })
        .collect();
    assert_eq!(
        brief,
        [(Some("lifelog"), 0, 2, 0), (None, 145, 1, 1)],
        "夹具里那条没有项目的命令也算进「未归类」"
    );
    let text = f.cli.invoke("projects", &[], TEXT).await.unwrap().text;
    assert!(
        text.lines()
            .any(|l| l.contains("lifelog") && l.ends_with("2 条命令")),
        "{text}"
    );

    // 隐藏了来源就不挂
    let hidden = fixture_with(json!({ "hidden_sources": ["shell", "browsing"] })).await;
    add(&hidden, mixed()).await;
    let timeline = hidden.memory.timeline("today".to_owned()).await.unwrap();
    assert!(
        timeline
            .entries
            .iter()
            .all(|e| e.commands.is_empty() && e.visits.is_empty())
    );
}

#[tokio::test]
async fn search_through_the_command_line() {
    let (clock, cli, timer) = (
        FakeClock::at(ts("2026-10-20T04:00:00Z"), 480),
        FakeCli::default(),
        timer_contract::testing::ManualTimer::default(),
    );
    let tui = FakeTui::default();
    let app = base(&clock, &cli)
        .with(tui.clone())
        .with(timer.clone())
        .with(tokenizer_cjk::TokenizerCjk)
        .with(search::SearchComponent)
        .config("query", json!({ "hidden_sources": ["browsing"] }))
        .unwrap()
        .boot()
        .await
        .unwrap();
    app.resolve(&ACTIVITY_WRITE)
        .unwrap()
        .submit(vec![
            focus(
                "zen",
                "jiff 文档",
                "2026-10-19T14:00:00Z",
                "2026-10-19T15:00:00Z",
            ),
            focus(
                "zen",
                "jiff 文档",
                "2026-10-20T01:00:00Z",
                "2026-10-20T01:10:00Z",
            ),
            Activity::new("browsing", "visit", ts("2026-10-20T02:00:00Z"), 480)
                .with_title("jiff 文档")
                .with_payload("url", "https://docs.rs/jiff"),
        ])
        .await
        .unwrap();
    timer.fire_all().await.unwrap();
    app.settle().await;

    let out = cli
        .invoke("search", &[("words", "jiff 文档")], Format::Json)
        .await
        .unwrap();
    let report: query_contracts::SearchReport = serde_json::from_str(&out.text).unwrap();
    assert_eq!(report.hits.len(), 1, "隐藏的来源不出现：{report:?}");
    assert_eq!(
        (report.hits[0].source.as_str(), report.hits[0].count),
        ("focus", 2)
    );

    let today = cli
        .invoke(
            "search",
            &[("words", "jiff"), ("since", "today")],
            Format::Json,
        )
        .await
        .unwrap();
    let today: query_contracts::SearchReport = serde_json::from_str(&today.text).unwrap();
    assert_eq!(today.hits[0].count, 1, "昨天那次不算");

    // 终端界面的搜索页：提交的文字就是查询
    let keys: Vec<char> = tui
        .pages()
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.key)
        .collect();
    assert_eq!(keys.last(), Some(&'s'));
    let page = |input: &str| PageRequest {
        offset: 0,
        width: 80,
        input: input.to_owned(),
    };
    let view = tui.render("search", page("文档")).await.unwrap();
    assert_eq!(view.heading, "「文档」");
    assert_eq!(view.note, "1 条，最近的在前");
    let blank = tui.render("search", page("")).await.unwrap();
    assert_eq!(blank.heading, "搜索", "还没输入");
    let punct = tui.render("search", page(" — ")).await.unwrap();
    assert!(
        matches!(&punct.blocks[..], [tui_contract::Block::Empty { message }] if message == "没有找到"),
        "只有标点不算出错"
    );

    let err = cli.invoke("search", &[], Format::Json).await.unwrap_err();
    assert_eq!(err.kind, kernel_api::ErrorKind::Validation);
    let err = cli
        .invoke("search", &[("words", "x"), ("limit", "0")], Format::Json)
        .await
        .unwrap_err();
    assert_eq!(err.kind, kernel_api::ErrorKind::Validation);
}

#[tokio::test]
async fn search_without_the_search_component_is_unavailable() {
    let f = fixture().await;
    let err = f
        .cli
        .invoke("search", &[("words", "jiff")], Format::Json)
        .await
        .unwrap_err();
    assert_eq!(err.kind, kernel_api::ErrorKind::Unavailable);
}

#[tokio::test]
async fn tools_for_agents() {
    let (clock, cli, mcp) = (
        FakeClock::at(ts("2026-10-20T04:00:00Z"), 480),
        FakeCli::default(),
        mcp_contract::testing::FakeMcp::default(),
    );
    let app = base(&clock, &cli)
        .with(mcp.clone())
        .config("query", json!({ "agent_hidden_sources": ["browsing"] }))
        .unwrap()
        .boot()
        .await
        .unwrap();
    app.resolve(&ACTIVITY_WRITE)
        .unwrap()
        .submit(vec![
            focus(
                "zen",
                "文档",
                "2026-10-20T01:00:00Z",
                "2026-10-20T01:40:00Z",
            ),
            Activity::new("browsing", "visit", ts("2026-10-20T01:05:00Z"), 480)
                .with_title("私人")
                .with_payload("url", "https://private.example/"),
            Activity::new("shell", "command", ts("2026-10-20T01:10:00Z"), 480).with_title("ls"),
            Activity::new("shell", "command", ts("2026-10-20T01:11:00Z"), 480).with_title("pwd"),
        ])
        .await
        .unwrap();

    let names: Vec<String> = mcp
        .tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(
        names,
        [
            "lifelog_day",
            "lifelog_timeline",
            "lifelog_week",
            "lifelog_projects",
            "lifelog_commands",
            "lifelog_visits"
        ],
        "没有启用检索就没有 lifelog_search"
    );

    let day: DayReport =
        serde_json::from_str(&mcp.call("lifelog_day", &[]).await.unwrap().text).unwrap();
    assert_eq!(day, f_day(&app).await, "就是 Memory 的返回值");

    // agent 看不到 browsing，命令行照样能看到
    let visits: Value =
        serde_json::from_str(&mcp.call("lifelog_visits", &[]).await.unwrap().text).unwrap();
    assert_eq!(visits["entries"], json!([]));
    let mine = cli.invoke("visits", &[], Format::Json).await.unwrap().text;
    assert!(mine.contains("私人"), "{mine}");
    let timeline = mcp.call("lifelog_timeline", &[]).await.unwrap().text;
    assert!(!timeline.contains("私人"), "{timeline}");

    // 列表只给最近的几条
    let commands: Value = serde_json::from_str(
        &mcp.call("lifelog_commands", &[("limit", "1")])
            .await
            .unwrap()
            .text,
    )
    .unwrap();
    let kept: Vec<&str> = commands["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["command"].as_str().unwrap())
        .collect();
    assert_eq!(kept, ["pwd"]);
    assert_eq!(
        (commands["truncated"].clone(), commands["total"].clone()),
        (json!(true), json!(2))
    );

    let err = mcp
        .call("lifelog_day", &[("date", "someday")])
        .await
        .unwrap_err();
    assert_eq!(err.kind, kernel_api::ErrorKind::Validation);
}

async fn f_day(app: &TestApp) -> DayReport {
    app.resolve(&MEMORY)
        .unwrap()
        .day("today".to_owned(), By::App)
        .await
        .unwrap()
}

// ---------- 命令行 ----------

#[tokio::test]
async fn registers_its_commands() {
    let f = fixture().await;
    let names: Vec<String> = f
        .cli
        .commands()
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(
        names,
        [
            "commands", "day", "projects", "search", "timeline", "today", "visits", "week"
        ]
    );
}

#[tokio::test]
async fn json_output_is_the_memory_report() {
    let f = fixture().await;
    let out = f
        .cli
        .invoke(
            "day",
            &[("date", "yesterday"), ("by", "title")],
            Format::Json,
        )
        .await
        .unwrap();
    let parsed: DayReport = serde_json::from_str(&out.text).unwrap();
    assert_eq!(
        parsed,
        f.memory
            .day("yesterday".to_owned(), By::Title)
            .await
            .unwrap()
    );
    assert!(!out.text.contains('\n'), "一行一个 JSON");
}

#[tokio::test]
async fn text_output_for_people() {
    let f = fixture().await;
    let out = f.cli.invoke("today", &[("by", "app")], TEXT).await.unwrap();
    let expected = "\
2026-10-20 周二（04:00 起）  共 2 小时 25 分

  1 小时 45 分  ████████████████████  com.mitchellh.ghostty
         40 分  ████████              zen
";
    assert_eq!(out.text, expected);

    // day 不给日期就是今天
    assert_eq!(f.cli.invoke("day", &[], TEXT).await.unwrap().text, expected);
}

#[tokio::test]
async fn command_errors_keep_their_kind() {
    let f = fixture().await;
    let err = f
        .cli
        .invoke("day", &[("date", "nope")], TEXT)
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Validation);
}

// ---------- 终端界面 ----------

#[tokio::test]
async fn registers_pages_and_pages_through_days_and_weeks() {
    let tui = FakeTui::default();
    let (clock, cli) = (
        FakeClock::at(ts("2026-10-20T04:00:00Z"), 480),
        FakeCli::default(),
    );
    let app = base(&clock, &cli).with(tui.clone()).boot().await.unwrap();
    app.resolve(&ACTIVITY_WRITE)
        .unwrap()
        .submit(vec![
            focus(
                "zen",
                "旧闻",
                "2026-10-19T14:00:00Z",
                "2026-10-19T15:00:00Z",
            ),
            focus(
                "zen",
                "Rust",
                "2026-10-20T01:00:00Z",
                "2026-10-20T01:40:00Z",
            ),
        ])
        .await
        .unwrap();

    let keys: Vec<char> = tui
        .pages()
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.key)
        .collect();
    assert_eq!(keys, ['d', 't', 'c', 'v', 'w'], "没有启用检索就没有搜索页");

    let now = || PageRequest {
        offset: 0,
        width: 80,
        input: String::new(),
    };
    let back = || PageRequest {
        offset: -1,
        width: 80,
        input: String::new(),
    };
    assert!(
        tui.render("day", now())
            .await
            .unwrap()
            .heading
            .starts_with("2026-10-20")
    );
    assert_eq!(tui.render("day", now()).await.unwrap().note, "共 40 分");
    assert!(
        tui.render("day", back())
            .await
            .unwrap()
            .heading
            .starts_with("2026-10-19"),
        "往前一格是前一天"
    );
    assert!(
        tui.render("timeline", back())
            .await
            .unwrap()
            .heading
            .starts_with("2026-10-19")
    );
    assert!(
        tui.render("week", back())
            .await
            .unwrap()
            .heading
            .starts_with("2026-10-12 ～ 2026-10-18"),
        "周页往前一格是上一周"
    );
    // 往后翻不会超过现在
    let ahead = || PageRequest {
        offset: 3,
        width: 80,
        input: String::new(),
    };
    assert_eq!(
        tui.render("day", ahead()).await.unwrap(),
        tui.render("day", now()).await.unwrap()
    );
}

// ---------- 架构 ----------

#[tokio::test]
async fn works_without_a_command_line() {
    // 没有命令行的进程里（比如以后的 MCP）照样提供 Memory
    let app = testkit::app()
        .with(StorageSqlite)
        .config("storage-sqlite", json!({ "path": ":memory:" }))
        .unwrap()
        .with(FakeClock::at(ts("2026-10-20T04:00:00Z"), 480))
        .with(MemoryLog::default())
        .with(ActivityComponent)
        .with(QueryComponent)
        .boot()
        .await
        .unwrap();
    assert_eq!(
        app.resolve(&MEMORY)
            .unwrap()
            .day("today".to_owned(), By::App)
            .await
            .unwrap()
            .date,
        "2026-10-20"
    );
}

#[tokio::test]
async fn reads_only_through_the_activity_contract() {
    let f = fixture().await;
    f.memory.day("today".to_owned(), By::App).await.unwrap();
    let requests = f.app.requests();
    assert_eq!(
        &requests[requests.len() - 6..],
        [
            "query -> clock-fake clock.now = ok",
            "activity -> storage-sqlite sql.query = ok",
            "query -> activity activity.read.list = ok",
            "activity -> storage-sqlite sql.query = ok",
            "query -> activity activity.read.list = ok",
            "launcher -> query memory.day = ok",
        ]
    );
}
