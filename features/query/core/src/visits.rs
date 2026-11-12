//! 某一天访问过的网页：从活动里取出来，排成给人看的样子。

use activity_contracts::{Activity, payload_keys};
use query_contracts::{VisitEntry, Visits};
use serde_json::Value;
use tui_contract::{Block, Line, PageView, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::render::{clock, truncate, weekday};

/// `browsing` 记下的访问活动 → 网页列表的项，按时刻排序。
pub fn visit_entries(activities: &[Activity]) -> Vec<VisitEntry> {
    let mut entries: Vec<VisitEntry> = activities
        .iter()
        .map(|a| VisitEntry {
            at: a.ts_start,
            tz_offset_min: a.tz_offset_min,
            browser: a.app.clone(),
            title: a.title.clone(),
            url: a
                .payload
                .get(payload_keys::URL)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            project: a.project.clone(),
        })
        .collect();
    entries.sort_by_key(|e| e.at);
    entries
}

/// 网址里的域名，给人看的时候只显示它。
pub(crate) fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
}

/// 一行：（时刻，浏览器，项目，标题，域名）。没有标题时标题位置放网址。
fn columns(e: &VisitEntry, bw: usize, pw: usize) -> (String, String, String, String, String) {
    let pad = |s: &str, w: usize| format!("{s}{}", " ".repeat(w.saturating_sub(s.width())));
    let browser = e.browser.as_deref().unwrap_or("");
    let project = e.project.as_deref().unwrap_or("");
    let (title, host) = match &e.title {
        Some(t) => (t.clone(), host(&e.url).to_owned()),
        None => (e.url.clone(), String::new()),
    };
    (
        clock(e.at, e.tz_offset_min),
        pad(browser, bw),
        pad(project, pw),
        title,
        host,
    )
}

fn widths(visits: &Visits) -> (usize, usize) {
    let max = |f: fn(&VisitEntry) -> Option<&str>| {
        visits
            .entries
            .iter()
            .filter_map(f)
            .map(UnicodeWidthStr::width)
            .max()
            .unwrap_or(0)
    };
    (max(|e| e.browser.as_deref()), max(|e| e.project.as_deref()))
}

fn heading(visits: &Visits) -> String {
    format!(
        "{} {}（{} 起）",
        visits.date,
        weekday(&visits.date),
        visits.day_boundary
    )
}

/// 给人看的文本。`width` 是终端的列数，过长的标题会被截断。
///
/// ```text
/// 2026-11-12 周四（04:00 起）  共 2 个
///
///   16:32  zen          jiff - Rust  docs.rs
///   16:40  qutebrowser  lifelog  Issues  github.com
/// ```
pub fn render_visits(visits: &Visits, color: bool, width: u16) -> String {
    let head = heading(visits);
    if visits.entries.is_empty() {
        return format!("{head}  没有记录\n");
    }
    let (bw, pw) = widths(visits);
    let mut out = format!("{head}  共 {} 个\n\n", visits.entries.len());
    for entry in &visits.entries {
        let (at, browser, project, title, host) = columns(entry, bw, pw);
        let gap = if pw > 0 { "  " } else { "" };
        let prefix = 2 + at.width() + 2 + browser.width() + 2 + project.width() + gap.len();
        let host_part = if host.is_empty() { 0 } else { host.width() + 2 };
        let room = usize::from(width)
            .saturating_sub(prefix + host_part)
            .max(10);
        let title = truncate(&title, room);
        let paint = |text: &str, code: &str| {
            if color && !text.is_empty() {
                format!("{code}{text}\x1b[0m")
            } else {
                text.to_owned()
            }
        };
        let host = if host.is_empty() {
            String::new()
        } else {
            format!("  {}", paint(&host, "\x1b[2m"))
        };
        out.push_str(&format!(
            "  {}  {browser}  {}{gap}{title}{host}\n",
            paint(&at, "\x1b[2m"),
            paint(&project, "\x1b[36m"),
        ));
    }
    out
}

/// 终端界面的「网页」页。
pub fn visits_page(visits: &Visits) -> PageView {
    let (bw, pw) = widths(visits);
    let lines: Vec<Line> = visits
        .entries
        .iter()
        .map(|entry| {
            let (at, browser, project, title, host) = columns(entry, bw, pw);
            let mut spans = vec![
                Span::new(format!("  {at}  "), Style::Dim),
                Span::new(format!("{browser}  "), Style::Normal),
            ];
            if pw > 0 {
                spans.push(Span::new(format!("{project}  "), Style::Accent));
            }
            spans.push(Span::new(title, Style::Normal));
            if !host.is_empty() {
                spans.push(Span::new(format!("  {host}"), Style::Dim));
            }
            Line { spans }
        })
        .collect();
    PageView {
        heading: heading(visits),
        note: format!("{} 个", visits.entries.len()),
        blocks: vec![if lines.is_empty() {
            Block::Empty {
                message: "没有记录".to_owned(),
            }
        } else {
            Block::Lines { lines }
        }],
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// 2026-11-12 16:32 东京（UTC+9）。
    const AT: i64 = 1_794_468_720_000;

    fn visit(browser: &str, offset_min: i64, url: &str, title: Option<&str>) -> Activity {
        let mut a = Activity::new("browsing", "visit", AT + offset_min * 60_000, 540)
            .with_app(browser)
            .with_payload(payload_keys::URL, url);
        if let Some(t) = title {
            a = a.with_title(t);
        }
        a
    }

    fn day(entries: Vec<VisitEntry>) -> Visits {
        Visits {
            date: "2026-11-12".to_owned(),
            day_boundary: "04:00".to_owned(),
            start: 0,
            end: 0,
            tz_offset_min: 540,
            entries,
        }
    }

    #[test]
    fn text_shows_title_and_host() {
        let text = render_visits(
            &day(visit_entries(&[
                visit(
                    "qutebrowser",
                    8,
                    "https://github.com/x/y/issues?q=1",
                    Some("Issues"),
                )
                .with_project("lifelog"),
                visit("zen", 0, "https://docs.rs/jiff", Some("jiff - Rust")),
                visit("zen", 1, "file:///home/me/a.pdf", None),
            ])),
            false,
            100,
        );
        assert_eq!(
            text,
            "\
2026-11-12 周四（04:00 起）  共 3 个

  16:32  zen                   jiff - Rust  docs.rs
  16:33  zen                   file:///home/me/a.pdf
  16:40  qutebrowser  lifelog  Issues  github.com
"
        );
    }

    #[test]
    fn page_lines_and_empty_day() {
        let view = visits_page(&day(visit_entries(&[visit(
            "zen",
            0,
            "https://a/",
            Some("甲"),
        )])));
        let Block::Lines { lines } = &view.blocks[0] else {
            panic!("{:?}", view.blocks)
        };
        let text: String = lines[0].spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "  16:32  zen  甲  a");
        assert_eq!(
            render_visits(&day(vec![]), false, 80),
            "2026-11-12 周四（04:00 起）  没有记录\n"
        );
    }
}
