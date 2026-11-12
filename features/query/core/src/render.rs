//! 把报告排成给人看的文本。

use jiff::Timestamp;
use jiff::civil::{Date, Weekday};
use jiff::tz::Offset;
use query_contracts::{
    Brief, DayReport, EntryKind, Mark, ProjectDay, Timeline, TimelineEntry, WeekReport,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const BAR_WIDTH: usize = 20;
const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

/// 时长写成「2 小时 13 分」。不足一分钟的零头舍去。
pub fn format_duration(ms: i64) -> String {
    let minutes = ms.max(0) / 60_000;
    match (minutes / 60, minutes % 60) {
        (0, 0) if ms >= 1_000 => format!("{} 秒", ms / 1_000),
        (0, 0) if ms > 0 => "不到 1 秒".to_owned(),
        (0, m) => format!("{m} 分"),
        (h, m) => format!("{h} 小时 {m:02} 分"),
    }
}

pub(crate) fn weekday(date: &str) -> &'static str {
    match date.parse::<Date>().map(|d| d.weekday()) {
        Ok(Weekday::Monday) => "周一",
        Ok(Weekday::Tuesday) => "周二",
        Ok(Weekday::Wednesday) => "周三",
        Ok(Weekday::Thursday) => "周四",
        Ok(Weekday::Friday) => "周五",
        Ok(Weekday::Saturday) => "周六",
        Ok(Weekday::Sunday) => "周日",
        Err(_) => "",
    }
}

fn paint(text: &str, code: &str, color: bool) -> String {
    if color {
        format!("{code}{text}{RESET}")
    } else {
        text.to_owned()
    }
}

pub(crate) fn away_note(away_ms: i64, color: bool) -> String {
    if away_ms < 60_000 {
        return String::new();
    }
    paint(
        &format!("（另有离开 {}）", format_duration(away_ms)),
        DIM,
        color,
    )
}

/// 左边补空格，按显示宽度右对齐。
fn right(text: &str, width: usize) -> String {
    format!("{}{text}", " ".repeat(width.saturating_sub(text.width())))
}

/// 右边补空格，按显示宽度左对齐。
fn left(text: &str, width: usize) -> String {
    format!("{text}{}", " ".repeat(width.saturating_sub(text.width())))
}

/// 截到不超过 `width` 列，截掉了就以 `…` 结尾。
pub(crate) fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// UTC 毫秒在给定偏移下的本地时刻 `HH:MM`。
pub(crate) fn clock(utc_ms: i64, offset_min: i32) -> String {
    let local = Timestamp::from_millisecond(utc_ms)
        .ok()
        .zip(Offset::from_seconds(offset_min * 60).ok())
        .map(|(ts, offset)| offset.to_datetime(ts));
    local.map_or_else(
        || "--:--".to_owned(),
        |dt| format!("{:02}:{:02}", dt.hour(), dt.minute()),
    )
}

/// 时长写成紧凑的 `H:MM`，给表格用。
pub(crate) fn compact(ms: i64) -> String {
    if (1_000..60_000).contains(&ms) {
        return format!("{}s", ms / 1_000);
    }
    let minutes = ms.max(0) / 60_000;
    format!("{}:{:02}", minutes / 60, minutes % 60)
}

/// 一天按时间顺序做了什么。`width` 是终端的列数，过长的标题会被截断。
///
/// ```text
/// 2026-10-20 周二（04:00 起）
///
///   09:00–09:40  40 分  com.mitchellh.ghostty  nvim ~/work
///   09:50–10:00  10 分  （离开）
/// ```
pub fn render_timeline(timeline: &Timeline, color: bool, width: u16) -> String {
    let since = paint(&format!("（{} 起）", timeline.day_boundary), DIM, color);
    let head = format!("{} {}{since}", timeline.date, weekday(&timeline.date));
    if timeline.entries.is_empty() {
        return format!("{head}  没有记录\n");
    }
    let durations: Vec<String> = timeline
        .entries
        .iter()
        .map(|e| format_duration(e.end - e.start))
        .collect();
    let widest = durations.iter().map(|d| d.width()).max().unwrap_or(0);

    let mut out = format!("{head}\n\n");
    for (entry, duration) in timeline.entries.iter().zip(&durations) {
        let span = format!(
            "{}–{}",
            clock(entry.start, entry.tz_offset_min),
            clock(entry.end, entry.tz_offset_min)
        );
        let prefix_width = 2 + span.width() + 2 + widest + 2;
        let room = usize::from(width).saturating_sub(prefix_width).max(10);
        let what = match entry.kind {
            EntryKind::Away => paint("（离开）", DIM, color),
            EntryKind::Window => {
                let app = entry.app.as_deref().unwrap_or("（无）");
                let rest = match entry.title.as_deref() {
                    Some(title) if title != app => format!("{app}  {title}"),
                    _ => app.to_owned(),
                };
                let rest = truncate(&rest, room);
                match rest.strip_prefix(app) {
                    Some(tail) => format!("{}{tail}", paint(app, CYAN, color)),
                    None => paint(&rest, CYAN, color),
                }
            }
        };
        out.push_str(&format!(
            "  {}  {}  {what}\n",
            paint(&span, DIM, color),
            right(duration, widest)
        ));
        let indent = " ".repeat(prefix_width);
        if !entry.during.is_empty() {
            let text = truncate(&during_text(&entry.during), room);
            out.push_str(&format!("{indent}{}\n", paint(&text, DIM, color)));
        }
        for (main, dim) in mark_lines(entry) {
            let main = truncate(&main, room.saturating_sub(dim.width() + 2).max(10));
            let dim = match (main.is_empty(), dim.is_empty()) {
                (_, true) => String::new(),
                (true, false) => paint(&dim, DIM, color),
                (false, false) => format!("  {}", paint(&dim, DIM, color)),
            };
            out.push_str(&format!("{indent}{main}{dim}\n"));
        }
    }
    out
}

/// 时间线一项下面每类最多列几条，其余只说个数。
const MARKS_SHOWN: usize = 3;

/// 时间线一项下面的命令和网页，一行一条：（正文，次要的补充）。
pub(crate) fn mark_lines(entry: &TimelineEntry) -> Vec<(String, String)> {
    let mut lines = Vec::new();
    let mut list = |marks: &[Mark], sign: &str, noun: &str| {
        for mark in marks.iter().take(MARKS_SHOWN) {
            let times = if mark.count > 1 {
                format!(" ×{}", mark.count)
            } else {
                String::new()
            };
            lines.push((
                format!("{sign} {}{times}", mark.text),
                mark.detail.clone().unwrap_or_default(),
            ));
        }
        if marks.len() > MARKS_SHOWN {
            lines.push((
                String::new(),
                format!("… 还有 {} {noun}", marks.len() - MARKS_SHOWN),
            ));
        }
    };
    list(&entry.commands, "$", "条命令");
    list(&entry.visits, "↗", "个网页");
    lines
}

/// 「其间：A 30 秒 · B 20 秒 等 5 项」。只列最长的三项。
pub(crate) fn during_text(during: &[Brief]) -> String {
    const SHOWN: usize = 3;
    let parts: Vec<String> = during
        .iter()
        .take(SHOWN)
        .map(|b| {
            let what = b.title.as_deref().or(b.app.as_deref()).unwrap_or("（无）");
            format!("{what} {}", format_duration(b.total_ms))
        })
        .collect();
    let mut text = format!("其间：{}", parts.join(" · "));
    if during.len() > SHOWN {
        text.push_str(&format!(" 等 {} 项", during.len()));
    }
    text
}

/// 「12 条命令 · 5 个网页」：没有的不写。
pub(crate) fn counts(commands: u32, visits: u32) -> String {
    let mut parts = Vec::new();
    if commands > 0 {
        parts.push(format!("{commands} 条命令"));
    }
    if visits > 0 {
        parts.push(format!("{visits} 个网页"));
    }
    parts.join(" · ")
}

/// 一天按项目分。
///
/// ```text
/// 2026-11-12 周四（04:00 起）
///
///   1 小时 05 分  lifelog     40 条命令 · 5 个网页
///      不到 1 秒  weather-station  3 条命令
///          23 分  （未归类）  12 条命令 · 30 个网页
/// ```
pub fn render_projects(day: &ProjectDay, color: bool) -> String {
    let since = paint(&format!("（{} 起）", day.day_boundary), DIM, color);
    let head = format!("{} {}{since}", day.date, weekday(&day.date));
    if day.rows.is_empty() {
        return format!("{head}  没有记录\n");
    }
    let away = away_note(day.away_ms, color);
    let mut out = if away.is_empty() {
        format!("{head}\n\n")
    } else {
        format!("{head}  {away}\n\n")
    };
    let durations: Vec<String> = day
        .rows
        .iter()
        .map(|r| {
            if r.window_ms > 0 {
                format_duration(r.window_ms)
            } else {
                "·".to_owned()
            }
        })
        .collect();
    let names: Vec<&str> = day
        .rows
        .iter()
        .map(|r| r.project.as_deref().unwrap_or("（未归类）"))
        .collect();
    let dw = durations.iter().map(|d| d.width()).max().unwrap_or(0);
    let nw = names.iter().map(|n| n.width()).max().unwrap_or(0);
    for ((row, duration), name) in day.rows.iter().zip(&durations).zip(&names) {
        let painted = if row.project.is_some() {
            paint(&left(name, nw), CYAN, color)
        } else {
            paint(&left(name, nw), DIM, color)
        };
        let extra = counts(row.commands, row.visits);
        let line = format!(
            "  {}  {painted}  {}",
            right(duration, dw),
            paint(&extra, DIM, color)
        );
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

pub(crate) const WEEKDAYS: [&str; 7] = ["一", "二", "三", "四", "五", "六", "日"];
const KEY_WIDTH: usize = 28;
const CELL: usize = 6;

/// 一周在各个窗口上花的时间。
///
/// ```text
/// 2026-10-19 ～ 2026-10-25（04:00 起）  共 12 小时 30 分
///
///                  一     二     三     四     五     六     日    合计
///   ghostty      1:45   2:10      ·      ·      ·      ·      ·    3:55
/// ```
pub fn render_week(report: &WeekReport, color: bool) -> String {
    let last = report.days.last().map_or("", |d| d.date.as_str());
    let since = paint(&format!("（{} 起）", report.day_boundary), DIM, color);
    let head = format!("{} ～ {last}{since}", report.week_start);
    if report.rows.is_empty() {
        return format!("{head}  没有记录\n");
    }
    let key_width = report
        .rows
        .iter()
        .map(|r| r.key.as_deref().unwrap_or("（无）").width())
        .chain(std::iter::once("合计".width()))
        .max()
        .unwrap_or(0)
        .min(KEY_WIDTH);

    let mut out = format!(
        "{head}  共 {}{}\n\n",
        format_duration(report.total_ms),
        away_note(report.away_ms, color)
    );
    let mut header = format!("  {}", " ".repeat(key_width));
    for day in WEEKDAYS {
        header.push_str(&right(day, CELL + 1));
    }
    header.push_str(&right("合计", CELL + 2));
    out.push_str(&paint(&header, DIM, color));
    out.push('\n');

    let cell = |ms: i64| {
        if ms < 60_000 {
            right("·", CELL)
        } else {
            right(&compact(ms), CELL)
        }
    };
    // 一周加起来都不到一分钟的，表格里每格都是「·」，不占地方
    let (shown, tiny): (Vec<_>, Vec<_>) = report.rows.iter().partition(|r| r.total_ms >= 60_000);
    for row in shown {
        let key = truncate(row.key.as_deref().unwrap_or("（无）"), key_width);
        let mut line = format!("  {}", paint(&left(&key, key_width), CYAN, color));
        for ms in &row.days_ms {
            line.push(' ');
            line.push_str(&cell(*ms));
        }
        line.push_str("  ");
        line.push_str(&right(&compact(row.total_ms), CELL));
        out.push_str(&line);
        out.push('\n');
    }
    if !tiny.is_empty() {
        out.push_str(&paint(
            &format!("  另有 {} 项，一周都不到 1 分钟", tiny.len()),
            DIM,
            color,
        ));
        out.push('\n');
    }
    let mut totals = format!("  {}", left("合计", key_width));
    for day in &report.days {
        totals.push(' ');
        totals.push_str(&cell(day.total_ms));
    }
    totals.push_str("  ");
    totals.push_str(&right(&compact(report.total_ms), CELL));
    out.push_str(&paint(&totals, DIM, color));
    out.push('\n');
    out
}

/// 一天的时长报告。
///
/// ```text
/// 2026-10-20 周二（04:00 起）  共 2 小时 30 分
///
///   1 小时 45 分  ████████████████████  com.mitchellh.ghostty
///          40 分  ████████              zen
/// ```
pub fn render_day(report: &DayReport, color: bool) -> String {
    let since = paint(&format!("（{} 起）", report.day_boundary), DIM, color);
    let head = format!("{} {}{since}", report.date, weekday(&report.date));
    if report.rows.is_empty() {
        return format!("{head}  没有记录\n");
    }

    let durations: Vec<String> = report
        .rows
        .iter()
        .map(|r| format_duration(r.total_ms))
        .collect();
    // 中文字符占两列，按显示宽度对齐
    let widest = durations.iter().map(|d| d.width()).max().unwrap_or(0);
    let longest = report
        .rows
        .iter()
        .map(|r| r.total_ms)
        .max()
        .unwrap_or(0)
        .max(1);

    let mut out = format!(
        "{head}  共 {}{}\n\n",
        format_duration(report.total_ms),
        away_note(report.away_ms, color)
    );
    for (row, duration) in report.rows.iter().zip(&durations) {
        let pad = " ".repeat(widest - duration.width());
        // 四舍五入；有时长就至少画一格
        let filled = usize::try_from((row.total_ms * 20 + longest / 2) / longest).unwrap_or(0);
        let filled = filled.clamp(usize::from(row.total_ms > 0), BAR_WIDTH);
        let bar = paint(&"█".repeat(filled), CYAN, color);
        let gap = " ".repeat(BAR_WIDTH - filled);
        let key = row.key.as_deref().unwrap_or("（无）");
        out.push_str(&format!("  {pad}{duration}  {bar}{gap}  {key}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use query_contracts::{By, DayRow, TimelineEntry, WeekDay, WeekRow};

    use super::*;

    fn minutes(m: i64) -> i64 {
        m * 60_000
    }

    fn report(rows: &[(Option<&str>, i64)]) -> DayReport {
        DayReport {
            date: "2026-10-20".to_owned(),
            day_boundary: "04:00".to_owned(),
            start: 0,
            end: 0,
            tz_offset_min: 480,
            by: By::App,
            total_ms: rows.iter().map(|r| r.1).sum(),
            away_ms: 0,
            rows: rows
                .iter()
                .map(|(key, total_ms)| DayRow {
                    key: key.map(str::to_owned),
                    total_ms: *total_ms,
                    spans: 1,
                })
                .collect(),
        }
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(0), "0 分");
        assert_eq!(format_duration(59_999), "59 秒");
        assert_eq!(format_duration(999), "不到 1 秒");
        assert_eq!(format_duration(minutes(41)), "41 分");
        assert_eq!(format_duration(minutes(60)), "1 小时 00 分");
        assert_eq!(format_duration(minutes(133) + 59_000), "2 小时 13 分");
        assert_eq!(format_duration(minutes(26 * 60 + 5)), "26 小时 05 分");
        assert_eq!(format_duration(-5), "0 分");
    }

    #[test]
    fn a_day_with_records() {
        let text = render_day(
            &report(&[
                (Some("com.mitchellh.ghostty"), minutes(105)),
                (Some("zen"), minutes(40)),
                (None, minutes(5)),
                (Some("blip"), 20_000),
            ]),
            false,
        );
        let expected = "\
2026-10-20 周二（04:00 起）  共 2 小时 30 分

  1 小时 45 分  ████████████████████  com.mitchellh.ghostty
         40 分  ████████              zen
          5 分  █                     （无）
         20 秒  █                     blip
";
        assert_eq!(text, expected);
    }

    #[test]
    fn an_empty_day() {
        assert_eq!(
            render_day(&report(&[]), false),
            "2026-10-20 周二（04:00 起）  没有记录\n"
        );
    }

    #[test]
    fn color_only_wraps_the_bar_and_the_boundary() {
        let text = render_day(&report(&[(Some("zen"), minutes(40))]), true);
        assert!(
            text.starts_with("2026-10-20 周二\x1b[2m（04:00 起）\x1b[0m  共 40 分\n"),
            "{text:?}"
        );
        assert!(
            text.contains(&format!("\x1b[36m{}\x1b[0m  zen\n", "█".repeat(20))),
            "{text:?}"
        );
        // 去掉转义码之后和不带颜色的一样
        let stripped = text
            .replace("\x1b[2m", "")
            .replace("\x1b[36m", "")
            .replace("\x1b[0m", "");
        assert_eq!(
            stripped,
            render_day(&report(&[(Some("zen"), minutes(40))]), false)
        );
    }

    #[test]
    fn a_day_with_away_time() {
        let mut r = report(&[(Some("zen"), minutes(40))]);
        r.away_ms = minutes(25);
        assert!(
            render_day(&r, false)
                .starts_with("2026-10-20 周二（04:00 起）  共 40 分（另有离开 25 分）\n")
        );
    }

    fn entry(
        kind: EntryKind,
        start: &str,
        end: &str,
        app: Option<&str>,
        title: Option<&str>,
    ) -> TimelineEntry {
        let ms = |s: &str| s.parse::<Timestamp>().unwrap().as_millisecond();
        TimelineEntry {
            kind,
            start: ms(start),
            end: ms(end),
            tz_offset_min: 480,
            app: app.map(str::to_owned),
            title: title.map(str::to_owned),
            during: Vec::new(),
            commands: Vec::new(),
            visits: Vec::new(),
        }
    }

    fn timeline(entries: Vec<TimelineEntry>) -> Timeline {
        Timeline {
            date: "2026-10-20".to_owned(),
            day_boundary: "04:00".to_owned(),
            start: 0,
            end: 0,
            tz_offset_min: 480,
            entries,
        }
    }

    #[test]
    fn timeline_text() {
        let t = timeline(vec![
            entry(
                EntryKind::Window,
                "2026-10-20T01:00:00Z",
                "2026-10-20T01:40:00Z",
                Some("ghostty"),
                Some("nvim ~/work"),
            ),
            entry(
                EntryKind::Away,
                "2026-10-20T01:50:00Z",
                "2026-10-20T02:00:00Z",
                None,
                None,
            ),
            entry(
                EntryKind::Window,
                "2026-10-20T02:00:00Z",
                "2026-10-20T03:05:00Z",
                Some("zen"),
                Some("zen"),
            ),
        ]);
        let expected = "\
2026-10-20 周二（04:00 起）

  09:00–09:40         40 分  ghostty  nvim ~/work
  09:50–10:00         10 分  （离开）
  10:00–11:05  1 小时 05 分  zen
";
        assert_eq!(render_timeline(&t, false, 120), expected);
        assert_eq!(
            render_timeline(&timeline(vec![]), false, 80),
            "2026-10-20 周二（04:00 起）  没有记录\n"
        );
    }

    #[test]
    fn brief_items_are_listed_under_their_entry() {
        let mut main = entry(
            EntryKind::Window,
            "2026-10-20T01:00:00Z",
            "2026-10-20T01:40:00Z",
            Some("ghostty"),
            Some("气象站固件调试"),
        );
        main.during = ["nvim", "yazi", "shell", "zen"]
            .iter()
            .enumerate()
            .map(|(i, t)| Brief {
                app: Some("ghostty".to_owned()),
                title: Some((*t).to_owned()),
                total_ms: 40_000 - i64::try_from(i).unwrap() * 10_000,
            })
            .collect();
        let text = render_timeline(&timeline(vec![main]), false, 120);
        assert_eq!(
            text.lines().nth(3).unwrap(),
            "                      其间：nvim 40 秒 · yazi 30 秒 · shell 20 秒 等 4 项"
        );
    }

    #[test]
    fn commands_and_visits_are_listed_under_their_entry() {
        let mut main = entry(
            EntryKind::Window,
            "2026-10-20T01:00:00Z",
            "2026-10-20T01:40:00Z",
            Some("ghostty"),
            Some("nvim"),
        );
        let mark = |text: &str, detail: Option<&str>, count: u32| Mark {
            at: 0,
            text: text.to_owned(),
            detail: detail.map(str::to_owned),
            count,
        };
        main.commands = ["cargo test", "git status", "ls", "cd .."]
            .iter()
            .enumerate()
            .map(|(i, c)| mark(c, None, if i == 0 { 3 } else { 1 }))
            .collect();
        main.visits = vec![mark("jiff - Rust", Some("docs.rs"), 1)];
        let text = render_timeline(&timeline(vec![main]), false, 120);
        let lines: Vec<&str> = text.lines().skip(3).collect();
        assert_eq!(
            lines,
            [
                "                      $ cargo test ×3",
                "                      $ git status",
                "                      $ ls",
                "                      … 还有 1 条命令",
                "                      ↗ jiff - Rust  docs.rs",
            ]
        );
    }

    #[test]
    fn projects_text() {
        let row = |p: Option<&str>, ms: i64, c: u32, v: u32| query_contracts::ProjectRow {
            project: p.map(str::to_owned),
            window_ms: ms,
            commands: c,
            visits: v,
        };
        let day = ProjectDay {
            date: "2026-10-20".to_owned(),
            day_boundary: "04:00".to_owned(),
            start: 0,
            end: 0,
            tz_offset_min: 480,
            away_ms: minutes(20),
            rows: vec![
                row(Some("lifelog"), minutes(65), 40, 5),
                row(Some("weather"), 0, 3, 0),
                row(None, minutes(23), 0, 0),
            ],
        };
        assert_eq!(
            render_projects(&day, false),
            "\
2026-10-20 周二（04:00 起）  （另有离开 20 分）

  1 小时 05 分  lifelog     40 条命令 · 5 个网页
             ·  weather     3 条命令
         23 分  （未归类）
"
        );
    }

    #[test]
    fn long_titles_are_cut_to_the_terminal_width() {
        let t = timeline(vec![entry(
            EntryKind::Window,
            "2026-10-20T01:00:00Z",
            "2026-10-20T01:40:00Z",
            Some("zen"),
            Some("一个非常非常长的网页标题，长到一行放不下"),
        )]);
        let text = render_timeline(&t, false, 40);
        let line = text.lines().nth(2).unwrap();
        assert!(line.width() <= 40, "{line:?} 占了 {} 列", line.width());
        assert!(line.ends_with('…'), "{line:?}");
    }

    #[test]
    fn week_text() {
        let days: Vec<WeekDay> = (19..=25)
            .map(|d| WeekDay {
                date: format!("2026-10-{d}"),
                total_ms: 0,
                away_ms: 0,
            })
            .collect();
        let mut r = WeekReport {
            week_start: "2026-10-19".to_owned(),
            day_boundary: "04:00".to_owned(),
            by: By::App,
            days,
            total_ms: minutes(235),
            away_ms: 0,
            rows: vec![
                WeekRow {
                    key: Some("ghostty".to_owned()),
                    days_ms: vec![minutes(105), minutes(90), 0, 0, 0, 0, 0],
                    total_ms: minutes(195),
                },
                WeekRow {
                    key: Some("zen".to_owned()),
                    days_ms: vec![minutes(40), 0, 0, 0, 0, 0, 0],
                    total_ms: minutes(40),
                },
                WeekRow {
                    key: Some("blip".to_owned()),
                    days_ms: vec![5_000, 0, 0, 0, 0, 0, 0],
                    total_ms: 5_000,
                },
            ],
        };
        r.days[0].total_ms = minutes(145);
        r.days[1].total_ms = minutes(90);
        let expected = "\
2026-10-19 ～ 2026-10-25（04:00 起）  共 3 小时 55 分

              一     二     三     四     五     六     日    合计
  ghostty   1:45   1:30      ·      ·      ·      ·      ·    3:15
  zen       0:40      ·      ·      ·      ·      ·      ·    0:40
  另有 1 项，一周都不到 1 分钟
  合计      2:25   1:30      ·      ·      ·      ·      ·    3:55
";
        assert_eq!(render_week(&r, false), expected);
    }
}
