//! 检索结果排成给人看的样子。

use jiff::Timestamp;
use jiff::tz::Offset;
use query_contracts::{Hit, SearchReport};
use tui_contract::{Block, Line, PageView, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::render::truncate;
use crate::visits::host;

/// 本地的 `MM-DD HH:MM`。检索结果跨好多天，要带日期。
fn stamp(utc_ms: i64, offset_min: i32) -> String {
    Timestamp::from_millisecond(utc_ms)
        .ok()
        .zip(Offset::from_seconds(offset_min * 60).ok())
        .map(|(ts, offset)| offset.to_datetime(ts))
        .map_or_else(
            || "--- --:--".to_owned(),
            |dt| {
                format!(
                    "{:02}-{:02} {:02}:{:02}",
                    dt.month(),
                    dt.day(),
                    dt.hour(),
                    dt.minute()
                )
            },
        )
}

/// 来源的中文叫法。
fn label(source: &str) -> &str {
    match source {
        "focus" => "窗口",
        "shell" => "命令",
        "browsing" => "网页",
        other => other,
    }
}

/// 一条结果：（正文，补充）。网页补充域名，其余补充项目或应用；重复的标上次数。
fn describe(hit: &Hit) -> (String, String) {
    let main = hit
        .title
        .clone()
        .or_else(|| hit.url.clone())
        .unwrap_or_default();
    let mut detail: Vec<String> = Vec::new();
    if hit.title.is_some()
        && let Some(url) = &hit.url
    {
        detail.push(host(url).to_owned());
    }
    if let Some(project) = &hit.project {
        detail.push(project.clone());
    } else if hit.source == "focus"
        && let Some(app) = &hit.app
    {
        detail.push(app.clone());
    }
    if hit.count > 1 {
        detail.push(format!("×{}", hit.count));
    }
    (main, detail.join("  "))
}

/// 给人看的文本。`width` 是终端的列数，过长的正文会被截断。
///
/// ```text
/// 「jiff」  共 3 条，最近的在前
///
///   11-12 16:32  网页  Timestamp in jiff  docs.rs
///   11-10 09:15  命令  cargo add jiff  lifelog
///   11-09 22:01  窗口  jiff - Rust  zen  ×2
/// ```
pub fn render_search(report: &SearchReport, color: bool, width: u16) -> String {
    let head = format!("「{}」", report.text);
    if report.hits.is_empty() && report.documents.is_empty() {
        return format!("{head}  没有找到\n");
    }
    let dim = |text: &str| {
        if color && !text.is_empty() {
            format!("\x1b[2m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    };
    let mut out = format!("{head}  共 {} 条，最近的在前\n\n", report.hits.len());
    if !report.documents.is_empty() {
        out.push_str("  摘要和笔记\n");
        for doc in &report.documents {
            let room = usize::from(width).saturating_sub(4).max(10);
            out.push_str(&format!("    {}\n", truncate(&doc.title, room)));
            out.push_str(&format!("    {}\n", dim(&truncate(&doc.snippet, room))));
        }
        out.push('\n');
    }
    for hit in &report.hits {
        let (main, detail) = describe(hit);
        let at = stamp(hit.at, hit.tz_offset_min);
        let prefix = 2 + at.width() + 2 + label(&hit.source).width() + 2;
        let room = usize::from(width)
            .saturating_sub(
                prefix
                    + if detail.is_empty() {
                        0
                    } else {
                        detail.width() + 2
                    },
            )
            .max(10);
        let main = truncate(&main, room);
        let detail = if detail.is_empty() {
            String::new()
        } else {
            format!("  {}", dim(&detail))
        };
        out.push_str(&format!(
            "  {}  {}  {main}{detail}\n",
            dim(&at),
            label(&hit.source)
        ));
    }
    if report.truncated {
        out.push_str(&format!(
            "\n{}\n",
            dim("还有更多：加大 --limit，或用 --since / --until / --source 缩小范围")
        ));
    }
    out
}

/// 终端界面的「搜索」页。还没输入过时 `report` 是 `None`。
pub fn search_page(report: Option<&SearchReport>) -> PageView {
    let Some(report) = report else {
        return PageView {
            heading: "搜索".to_owned(),
            note: String::new(),
            blocks: vec![Block::Empty {
                message: "按 / 输入要找的词：窗口标题、命令、网页都能找".to_owned(),
            }],
        };
    };
    let mut lines: Vec<Line> = Vec::new();
    if !report.documents.is_empty() {
        lines.push(Line {
            spans: vec![Span::new("  摘要和笔记", Style::Strong)],
        });
        for doc in &report.documents {
            lines.push(Line {
                spans: vec![Span::new(format!("    {}", doc.title), Style::Accent)],
            });
            lines.push(Line {
                spans: vec![Span::new(format!("    {}", doc.snippet), Style::Dim)],
            });
        }
        lines.push(Line::default());
    }
    lines.extend(report.hits.iter().map(|hit| {
        let (main, detail) = describe(hit);
        let mut spans = vec![
            Span::new(
                format!("  {}  ", stamp(hit.at, hit.tz_offset_min)),
                Style::Dim,
            ),
            Span::new(format!("{}  ", label(&hit.source)), Style::Accent),
            Span::new(main, Style::Normal),
        ];
        if !detail.is_empty() {
            spans.push(Span::new(format!("  {detail}"), Style::Dim));
        }
        Line { spans }
    }));
    let mut note = format!("{} 条，最近的在前", report.hits.len());
    if report.truncated {
        note.push_str("；只列了最近的这些");
    }
    PageView {
        heading: format!("「{}」", report.text),
        note,
        blocks: vec![if lines.is_empty() {
            Block::Empty {
                message: "没有找到".to_owned(),
            }
        } else {
            Block::Lines { lines }
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-12 16:32 东京（UTC+9）。
    const AT: i64 = 1_794_468_720_000;

    fn hit(
        source: &str,
        title: Option<&str>,
        url: Option<&str>,
        project: Option<&str>,
        count: u32,
    ) -> Hit {
        Hit {
            source: source.to_owned(),
            kind: String::new(),
            at: AT,
            tz_offset_min: 540,
            app: Some("zen".to_owned()),
            title: title.map(str::to_owned),
            url: url.map(str::to_owned),
            project: project.map(str::to_owned),
            count,
        }
    }

    #[test]
    fn page_before_and_after_searching() {
        let before = search_page(None);
        assert!(
            matches!(&before.blocks[..], [Block::Empty { message }] if message.contains("按 /"))
        );
        let report = SearchReport {
            documents: vec![],
            text: "jiff".to_owned(),
            hits: vec![hit(
                "shell",
                Some("cargo add jiff"),
                None,
                Some("lifelog"),
                2,
            )],
            truncated: true,
        };
        let view = search_page(Some(&report));
        assert_eq!(view.heading, "「jiff」");
        assert_eq!(view.note, "1 条，最近的在前；只列了最近的这些");
        let Block::Lines { lines } = &view.blocks[0] else {
            panic!("{:?}", view.blocks)
        };
        let text: String = lines[0].spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "  11-12 16:32  命令  cargo add jiff  lifelog  ×2");
    }

    #[test]
    fn results_with_dates_and_details() {
        let report = SearchReport {
            documents: vec![],
            text: "jiff".to_owned(),
            hits: vec![
                hit(
                    "browsing",
                    Some("Timestamp in jiff"),
                    Some("https://docs.rs/jiff"),
                    None,
                    1,
                ),
                hit("shell", Some("cargo add jiff"), None, Some("lifelog"), 1),
                hit("focus", Some("jiff - Rust"), None, None, 2),
                hit("browsing", None, Some("file:///a/jiff.pdf"), None, 1),
            ],
            truncated: true,
        };
        assert_eq!(
            render_search(&report, false, 100),
            "\
「jiff」  共 4 条，最近的在前

  11-12 16:32  网页  Timestamp in jiff  docs.rs
  11-12 16:32  命令  cargo add jiff  lifelog
  11-12 16:32  窗口  jiff - Rust  zen  ×2
  11-12 16:32  网页  file:///a/jiff.pdf

还有更多：加大 --limit，或用 --since / --until / --source 缩小范围
"
        );
        let empty = SearchReport {
            documents: vec![],
            text: "x".to_owned(),
            hits: vec![],
            truncated: false,
        };
        assert_eq!(render_search(&empty, false, 80), "「x」  没有找到\n");
    }

    #[test]
    fn documents_come_first() {
        let report = SearchReport {
            text: "气象站".to_owned(),
            hits: vec![hit("focus", Some("气象站固件调试"), None, None, 3)],
            documents: vec![query_contracts::DocumentHit {
                key: "summary_daily:2026-11-12".to_owned(),
                kind: "summary_daily".to_owned(),
                date: Some("2026-11-12".to_owned()),
                title: "2026-11-12 周四".to_owned(),
                snippet: "…调试气象站固件，加入了 uucp 组…".to_owned(),
            }],
            truncated: false,
        };
        let text = render_search(&report, false, 80);
        assert!(
            text.contains("  摘要和笔记\n    2026-11-12 周四\n    …调试气象站固件，加入了 uucp 组…\n\n  11-12 16:32  窗口"),
            "{text}"
        );
        let view = search_page(Some(&report));
        let Block::Lines { lines } = &view.blocks[0] else {
            panic!("{:?}", view.blocks)
        };
        assert_eq!(lines[1].spans[0].text, "    2026-11-12 周四");
        let only_docs = SearchReport {
            hits: vec![],
            ..report
        };
        assert!(
            !render_search(&only_docs, false, 80).contains("没有找到"),
            "只有文档也算找到了"
        );
    }
}
