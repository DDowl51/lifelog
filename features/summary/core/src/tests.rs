#![allow(clippy::unwrap_used)]

use query_contracts::{
    CommandEntry, Commands, EntryKind, Mark, ProjectDay, ProjectRow, Timeline, TimelineEntry,
    VisitEntry, Visits,
};

use super::*;

/// 2026-11-12 09:00 东京。
const AT: i64 = 1_794_441_600_000;
const M: i64 = 60_000;

fn projects() -> ProjectDay {
    let row = |p: Option<&str>, min: i64, c: u32, v: u32| ProjectRow {
        project: p.map(str::to_owned),
        window_ms: min * M,
        commands: c,
        visits: v,
    };
    ProjectDay {
        date: "2026-11-12".to_owned(),
        day_boundary: "04:00".to_owned(),
        start: 0,
        end: 0,
        tz_offset_min: 540,
        away_ms: 48 * M,
        rows: vec![row(Some("lifelog"), 65, 3, 1), row(None, 20, 0, 0)],
    }
}

fn mark(text: &str, count: u32) -> Mark {
    Mark {
        at: AT,
        text: text.to_owned(),
        detail: None,
        count,
    }
}

fn timeline() -> Timeline {
    let entry = |start: i64, end: i64, title: &str| TimelineEntry {
        kind: EntryKind::Window,
        start: AT + start * M,
        end: AT + end * M,
        tz_offset_min: 540,
        app: Some("ghostty".to_owned()),
        title: Some(title.to_owned()),
        during: vec![],
        commands: vec![],
        visits: vec![],
    };
    let mut work = entry(0, 65, "nvim lifelog");
    work.commands = vec![mark("cargo test", 3)];
    let mut titled = mark("jiff - Rust", 1);
    titled.detail = Some("docs.rs".to_owned());
    // 没有标题的网页：text 是完整网址
    let untitled = mark(
        "https://auth.example/authorize?code_challenge=SECRET&state=SECRET",
        1,
    );
    work.visits = vec![titled, untitled];
    Timeline {
        date: "2026-11-12".to_owned(),
        day_boundary: "04:00".to_owned(),
        start: 0,
        end: 0,
        tz_offset_min: 540,
        entries: vec![work, entry(70, 72, "一闪而过")],
    }
}

fn commands() -> Commands {
    let c = |text: &str| CommandEntry {
        at: AT,
        tz_offset_min: 540,
        duration_ms: None,
        exit: None,
        command: text.to_owned(),
        cwd: None,
        project: Some("lifelog".to_owned()),
    };
    Commands {
        date: "2026-11-12".to_owned(),
        day_boundary: "04:00".to_owned(),
        start: 0,
        end: 0,
        tz_offset_min: 540,
        entries: vec![c("cargo test"), c("git push"), c("cargo test")],
    }
}

fn visits() -> Visits {
    Visits {
        date: "2026-11-12".to_owned(),
        day_boundary: "04:00".to_owned(),
        start: 0,
        end: 0,
        tz_offset_min: 540,
        entries: vec![
            VisitEntry {
                at: AT,
                tz_offset_min: 540,
                browser: Some("zen".to_owned()),
                title: Some("jiff - Rust".to_owned()),
                url: "https://docs.rs/jiff/latest/jiff/?search=secret-query".to_owned(),
                project: None,
            },
            VisitEntry {
                at: AT,
                tz_offset_min: 540,
                browser: Some("zen".to_owned()),
                title: Some("书桌收纳".to_owned()),
                url: "file:///home/me/table.pdf".to_owned(),
                project: None,
            },
        ],
    }
}

fn full(hidden: &[&str], max: usize) -> String {
    let (p, t, c, v) = (projects(), timeline(), commands(), visits());
    let input = DayInput {
        projects: &p,
        timeline: &t,
        commands: &c,
        visits: &v,
    };
    let hidden: Vec<String> = hidden.iter().map(|h| (*h).to_owned()).collect();
    digest(&input, &hidden, max)
}

#[test]
fn a_day_in_a_few_sections() {
    let text = full(&[], 10_000);
    for expected in [
        "日期：2026-11-12 周四（一天从 04:00 开始）",
        "在电脑前共 1 小时 25 分，离开 48 分。",
        "- lifelog：1 小时 05 分，3 条命令，1 个网页",
        "- （未归类）：20 分",
        "- 09:00–10:05 1 小时 05 分，ghostty「nvim lifelog」（命令：cargo test ×3，网页：jiff - Rust；auth.example）",
        "### lifelog\n- cargo test ×2\n- git push",
        "- jiff - Rust（docs.rs）",
        "- 书桌收纳\n",
    ] {
        assert!(text.contains(expected), "缺 {expected:?}：\n{text}");
    }
    assert!(!text.contains("一闪而过"), "不到 5 分钟的段不写");
    assert!(!text.contains("secret-query"), "不发完整网址");
    assert!(
        !text.contains("SECRET"),
        "时间线里没有标题的网页也只给域名：\n{text}"
    );
}

#[test]
fn hidden_sources_are_left_out_entirely() {
    let text = full(&["shell", "browsing"], 10_000);
    for gone in ["命令", "cargo test", "网页", "jiff - Rust", "docs.rs"] {
        assert!(!text.contains(gone), "不该有 {gone:?}：\n{text}");
    }
    assert!(text.contains("nvim lifelog"));
    assert!(!full(&["focus"], 10_000).contains("## 时间线"));
}

#[test]
fn long_digests_are_cut() {
    let text = full(&[], 60);
    assert!(text.chars().count() < 100, "{text}");
    assert!(text.ends_with("…（太长，后面省略）\n"));
}

#[test]
fn files_remember_whether_they_were_edited() {
    let file = render_file("2026-11-12", "## 概览\n写了 lifelog。\n", "deepseek-chat");
    assert!(file.starts_with("<!-- lifelog: 由 deepseek-chat 生成，hash="));
    assert!(file.contains("\n# 2026-11-12 周四\n\n## 概览\n写了 lifelog。\n"));
    assert_eq!(file_state(&file), FileState::Untouched);
    assert_eq!(
        file_state(&file.replace("写了", "写完了")),
        FileState::Edited
    );
    assert_eq!(
        file_state("# 我自己写的日记\n"),
        FileState::Edited,
        "不是生成的也不覆盖"
    );
    assert_eq!(body_of(&file), "## 概览\n写了 lifelog。");
    assert_eq!(body_of("# 2026-11-12 周四\n\n手写的"), "手写的");
}

#[test]
fn small_helpers() {
    assert_eq!(duration(65 * M), "1 小时 05 分");
    assert_eq!(duration(30_000), "不到 1 分");
    assert_eq!(weekday("2026-11-12"), "周四");
    assert_eq!(
        tally(["b", "a", "b", "c", "a", "b"].into_iter()),
        [("b", 3), ("a", 2), ("c", 1)]
    );
}
