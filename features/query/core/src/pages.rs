//! 把报告变成终端界面的页面内容（纯数据，外壳负责画）。

use query_contracts::{DayReport, EntryKind, ProjectDay, Timeline, WeekReport};
use tui_contract::{Bar, Block, Line, PageView, Span, Style};

use crate::render::{
    WEEKDAYS, clock, compact, counts, during_text, format_duration, mark_lines, weekday,
};

fn note(total_ms: i64, away_ms: i64) -> String {
    let mut note = format!("共 {}", format_duration(total_ms));
    if away_ms >= 60_000 {
        note.push_str(&format!("，另有离开 {}", format_duration(away_ms)));
    }
    note
}

fn bars(report: &DayReport) -> Block {
    if report.rows.is_empty() {
        return Block::Empty {
            message: "没有记录".to_owned(),
        };
    }
    Block::Bars {
        bars: report
            .rows
            .iter()
            .map(|r| Bar {
                label: r.key.clone().unwrap_or_else(|| "（无）".to_owned()),
                value: r.total_ms,
                value_text: format_duration(r.total_ms),
            })
            .collect(),
    }
}

/// 「按项目」那一块：条形是窗口时长，后面带上命令数、网页数。
fn project_bars(projects: &ProjectDay) -> Block {
    Block::Bars {
        bars: projects
            .rows
            .iter()
            .map(|r| {
                let mut value_text = format_duration(r.window_ms);
                let extra = counts(r.commands, r.visits);
                if !extra.is_empty() {
                    value_text = format!("{value_text} · {extra}");
                }
                Bar {
                    label: r.project.clone().unwrap_or_else(|| "（未归类）".to_owned()),
                    value: r.window_ms,
                    value_text,
                }
            })
            .collect(),
    }
}

/// 「日」页：按应用、按项目（有归了类的才显示）、按标题。三份报告是同一天。
pub fn day_page(by_app: &DayReport, projects: &ProjectDay, by_title: &DayReport) -> PageView {
    let mut blocks = vec![bars(by_app)];
    if projects.rows.iter().any(|r| r.project.is_some()) {
        blocks.push(Block::Heading {
            text: "按项目".to_owned(),
        });
        blocks.push(project_bars(projects));
    }
    if !by_title.rows.is_empty() {
        blocks.push(Block::Heading {
            text: "按标题".to_owned(),
        });
        blocks.push(bars(by_title));
    }
    PageView {
        heading: format!(
            "{} {}（{} 起）",
            by_app.date,
            weekday(&by_app.date),
            by_app.day_boundary
        ),
        note: note(by_app.total_ms, by_app.away_ms),
        blocks,
    }
}

/// 「时间线」页。
pub fn timeline_page(timeline: &Timeline) -> PageView {
    let lines: Vec<Line> = timeline
        .entries
        .iter()
        .flat_map(|e| {
            let span = format!(
                "  {}–{}  ",
                clock(e.start, e.tz_offset_min),
                clock(e.end, e.tz_offset_min)
            );
            let indent = " ".repeat(span.chars().count() + 8);
            let duration = format!("{:>6}  ", compact(e.end - e.start));
            let mut spans = vec![
                Span::new(span, Style::Dim),
                Span::new(duration, Style::Normal),
            ];
            match e.kind {
                EntryKind::Away => spans.push(Span::new("（离开）", Style::Dim)),
                EntryKind::Window => {
                    let app = e.app.clone().unwrap_or_else(|| "（无）".to_owned());
                    let title = e.title.clone().filter(|t| *t != app);
                    spans.push(Span::new(app, Style::Accent));
                    if let Some(title) = title {
                        spans.push(Span::new(format!("  {title}"), Style::Normal));
                    }
                }
            }
            let mut lines = vec![Line { spans }];
            if !e.during.is_empty() {
                lines.push(Line {
                    spans: vec![Span::new(
                        format!("{indent}{}", during_text(&e.during)),
                        Style::Dim,
                    )],
                });
            }
            for (main, dim) in mark_lines(e) {
                let mut spans = vec![Span::new(format!("{indent}{main}"), Style::Normal)];
                if !dim.is_empty() {
                    let gap = if main.is_empty() { "" } else { "  " };
                    spans.push(Span::new(format!("{gap}{dim}"), Style::Dim));
                }
                lines.push(Line { spans });
            }
            lines
        })
        .collect();
    let windows = timeline
        .entries
        .iter()
        .filter(|e| e.kind == EntryKind::Window)
        .count();
    PageView {
        heading: format!(
            "{} {}（{} 起）",
            timeline.date,
            weekday(&timeline.date),
            timeline.day_boundary
        ),
        note: format!("{windows} 段"),
        blocks: vec![if lines.is_empty() {
            Block::Empty {
                message: "没有记录".to_owned(),
            }
        } else {
            Block::Lines { lines }
        }],
    }
}

/// 「周」页。
pub fn week_page(report: &WeekReport) -> PageView {
    let last = report.days.last().map_or("", |d| d.date.as_str());
    let cell = |ms: i64| {
        if ms < 60_000 {
            "·".to_owned()
        } else {
            compact(ms)
        }
    };
    let mut header = vec![String::new()];
    header.extend(WEEKDAYS.iter().map(|d| (*d).to_owned()));
    header.push("合计".to_owned());

    let mut rows: Vec<Vec<String>> = report
        .rows
        .iter()
        .filter(|r| r.total_ms >= 60_000)
        .map(|r| {
            let mut row = vec![r.key.clone().unwrap_or_else(|| "（无）".to_owned())];
            row.extend(r.days_ms.iter().map(|ms| cell(*ms)));
            row.push(compact(r.total_ms));
            row
        })
        .collect();
    let blocks = if rows.is_empty() {
        vec![Block::Empty {
            message: "没有记录".to_owned(),
        }]
    } else {
        let mut totals = vec!["合计".to_owned()];
        totals.extend(report.days.iter().map(|d| cell(d.total_ms)));
        totals.push(compact(report.total_ms));
        rows.push(totals);
        vec![Block::Table { header, rows }]
    };
    PageView {
        heading: format!(
            "{} ～ {last}（{} 起）",
            report.week_start, report.day_boundary
        ),
        note: note(report.total_ms, report.away_ms),
        blocks,
    }
}

#[cfg(test)]
mod tests {
    use query_contracts::{By, DayRow, TimelineEntry, WeekDay, WeekRow};

    use super::*;

    const M: i64 = 60_000;

    fn day(by: By, rows: &[(&str, i64)], away_ms: i64) -> DayReport {
        DayReport {
            date: "2026-10-20".to_owned(),
            day_boundary: "04:00".to_owned(),
            start: 0,
            end: 0,
            tz_offset_min: 480,
            by,
            total_ms: rows.iter().map(|r| r.1).sum(),
            away_ms,
            rows: rows
                .iter()
                .map(|(k, ms)| DayRow {
                    key: Some((*k).to_owned()),
                    total_ms: *ms,
                    spans: 1,
                })
                .collect(),
        }
    }

    fn projects(rows: &[(Option<&str>, i64, u32, u32)]) -> ProjectDay {
        ProjectDay {
            date: "2026-10-20".to_owned(),
            day_boundary: "04:00".to_owned(),
            start: 0,
            end: 0,
            tz_offset_min: 480,
            away_ms: 0,
            rows: rows
                .iter()
                .map(|(p, ms, c, v)| query_contracts::ProjectRow {
                    project: p.map(str::to_owned),
                    window_ms: *ms,
                    commands: *c,
                    visits: *v,
                })
                .collect(),
        }
    }

    #[test]
    fn day_page_has_apps_then_titles() {
        let view = day_page(
            &day(By::App, &[("ghostty", 105 * M), ("zen", 40 * M)], 20 * M),
            &projects(&[(None, 145 * M, 0, 0)]),
            &day(By::Title, &[("nvim", 105 * M), ("Rust", 40 * M)], 20 * M),
        );
        assert_eq!(view.heading, "2026-10-20 周二（04:00 起）");
        assert_eq!(view.note, "共 2 小时 25 分，另有离开 20 分");
        assert!(
            matches!(&view.blocks[..], [Block::Bars { bars }, Block::Heading { .. }, Block::Bars { .. }] if bars[0].label == "ghostty" && bars[0].value_text == "1 小时 45 分")
        );
    }

    #[test]
    fn projects_show_up_once_something_is_classified() {
        let by_project = projects(&[(Some("lifelog"), 60 * M, 40, 5), (None, 40 * M, 0, 0)]);
        let view = day_page(
            &day(By::App, &[("ghostty", 100 * M)], 0),
            &by_project,
            &day(By::Title, &[("nvim", 100 * M)], 0),
        );
        let headings: Vec<&str> = view
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Heading { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(headings, ["按项目", "按标题"]);
        let Block::Bars { bars } = &view.blocks[2] else {
            panic!("{:?}", view.blocks)
        };
        let labels: Vec<(&str, &str)> = bars
            .iter()
            .map(|b| (b.label.as_str(), b.value_text.as_str()))
            .collect();
        assert_eq!(
            labels,
            [
                ("lifelog", "1 小时 00 分 · 40 条命令 · 5 个网页"),
                ("（未归类）", "40 分")
            ]
        );
    }

    #[test]
    fn an_empty_day() {
        let view = day_page(
            &day(By::App, &[], 0),
            &projects(&[]),
            &day(By::Title, &[], 0),
        );
        assert_eq!(view.note, "共 0 分");
        assert_eq!(
            view.blocks,
            [Block::Empty {
                message: "没有记录".to_owned()
            }]
        );
    }

    #[test]
    fn timeline_page_lines() {
        let entry =
            |kind, start: i64, end: i64, app: Option<&str>, title: Option<&str>| TimelineEntry {
                kind,
                start: 1_792_458_000_000 + start * M,
                end: 1_792_458_000_000 + end * M,
                tz_offset_min: 480,
                app: app.map(str::to_owned),
                title: title.map(str::to_owned),
                during: Vec::new(),
                commands: Vec::new(),
                visits: Vec::new(),
            };
        let view = timeline_page(&Timeline {
            date: "2026-10-20".to_owned(),
            day_boundary: "04:00".to_owned(),
            start: 0,
            end: 0,
            tz_offset_min: 480,
            entries: vec![
                entry(EntryKind::Window, 0, 40, Some("ghostty"), Some("nvim")),
                entry(EntryKind::Away, 50, 60, None, None),
                entry(EntryKind::Window, 60, 125, Some("zen"), Some("zen")),
            ],
        });
        assert_eq!(view.note, "2 段");
        let Block::Lines { lines } = &view.blocks[0] else {
            panic!("{view:?}")
        };
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.text.as_str()).collect())
            .collect();
        assert_eq!(
            text,
            [
                "  09:00–09:40    0:40  ghostty  nvim",
                "  09:50–10:00    0:10  （离开）",
                "  10:00–11:05    1:05  zen"
            ]
        );
        assert_eq!(lines[0].spans[2].style, Style::Accent);
    }

    #[test]
    fn week_page_table() {
        let view = week_page(&WeekReport {
            week_start: "2026-10-19".to_owned(),
            day_boundary: "04:00".to_owned(),
            by: By::App,
            days: (19..=25)
                .map(|d| WeekDay {
                    date: format!("2026-10-{d}"),
                    total_ms: if d == 19 { 105 * M } else { 0 },
                    away_ms: 0,
                })
                .collect(),
            total_ms: 105 * M,
            away_ms: 0,
            rows: vec![
                WeekRow {
                    key: Some("ghostty".to_owned()),
                    days_ms: vec![105 * M, 0, 0, 0, 0, 0, 0],
                    total_ms: 105 * M,
                },
                WeekRow {
                    key: Some("blip".to_owned()),
                    days_ms: vec![5_000, 0, 0, 0, 0, 0, 0],
                    total_ms: 5_000,
                },
            ],
        });
        assert_eq!(view.heading, "2026-10-19 ～ 2026-10-25（04:00 起）");
        let Block::Table { header, rows } = &view.blocks[0] else {
            panic!("{view:?}")
        };
        assert_eq!(header.len(), 9);
        assert_eq!(rows.len(), 2, "不到一分钟的那一行不列，最后一行是合计");
        assert_eq!(
            rows[0],
            ["ghostty", "1:45", "·", "·", "·", "·", "·", "·", "1:45"]
        );
        assert_eq!(rows[1][0], "合计");
    }
}
