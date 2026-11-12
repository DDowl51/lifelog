//! 某一天执行过的命令：从活动里取出来，排成给人看的样子。

use activity_contracts::{Activity, payload_keys};
use query_contracts::{CommandEntry, Commands};
use serde_json::Value;
use tui_contract::{Block, Line, PageView, Span, Style};
use unicode_width::UnicodeWidthStr;

use crate::render::{clock, weekday};

/// `shell` 记下的命令活动 → 命令列表的项，按开始时刻排序。
pub fn command_entries(activities: &[Activity]) -> Vec<CommandEntry> {
    let mut entries: Vec<CommandEntry> = activities
        .iter()
        .map(|a| CommandEntry {
            at: a.ts_start,
            tz_offset_min: a.tz_offset_min,
            duration_ms: a.ts_end.map(|end| end - a.ts_start),
            exit: a
                .payload
                .get("exit")
                .and_then(Value::as_i64)
                .and_then(|e| i32::try_from(e).ok()),
            command: a.title.clone().unwrap_or_default(),
            cwd: a
                .payload
                .get(payload_keys::CWD)
                .and_then(Value::as_str)
                .map(str::to_owned),
            project: a.project.clone(),
        })
        .collect();
    entries.sort_by_key(|e| e.at);
    entries
}

/// 命令的耗时：`<1s`、`12s`、`3m05s`、`1h02m`。
pub(crate) fn elapsed(ms: i64) -> String {
    let secs = ms.max(0) / 1_000;
    match secs {
        0 => "<1s".to_owned(),
        1..60 => format!("{secs}s"),
        60..3600 => format!("{}m{:02}s", secs / 60, secs % 60),
        _ => format!("{}h{:02}m", secs / 3600, secs % 3600 / 60),
    }
}

/// 一行命令拆成几段：时刻、耗时、项目、命令、失败时的退出码。
fn columns(e: &CommandEntry, project_width: usize) -> (String, String, String, String, String) {
    let duration = e.duration_ms.map(elapsed).unwrap_or_default();
    let project = e.project.clone().unwrap_or_default();
    let pad = " ".repeat(project_width.saturating_sub(project.width()));
    let failed = match e.exit {
        Some(code) if code != 0 => format!("  ✗{code}"),
        _ => String::new(),
    };
    (
        clock(e.at, e.tz_offset_min),
        format!("{duration:>6}"),
        format!("{project}{pad}"),
        e.command.clone(),
        failed,
    )
}

fn project_width(commands: &Commands) -> usize {
    commands
        .entries
        .iter()
        .filter_map(|e| e.project.as_deref())
        .map(UnicodeWidthStr::width)
        .max()
        .unwrap_or(0)
}

fn heading(commands: &Commands) -> String {
    format!(
        "{} {}（{} 起）",
        commands.date,
        weekday(&commands.date),
        commands.day_boundary
    )
}

/// 给人看的文本。`width` 是终端的列数，过长的命令会被截断。
///
/// ```text
/// 2026-11-12 周四（04:00 起）  共 2 条
///
///   16:32    1.2s  weather-station  cargo build --release
///   16:33  3m05s   lifelog  cargo test  ✗101
/// ```
pub fn render_commands(commands: &Commands, color: bool, width: u16) -> String {
    let head = heading(commands);
    if commands.entries.is_empty() {
        return format!("{head}  没有记录\n");
    }
    let pw = project_width(commands);
    let mut out = format!("{head}  共 {} 条\n\n", commands.entries.len());
    for entry in &commands.entries {
        let (at, duration, project, command, failed) = columns(entry, pw);
        let gap = if pw > 0 { "  " } else { "" };
        let prefix = 2 + at.width() + 2 + duration.width() + 2 + project.width() + gap.len();
        let room = usize::from(width)
            .saturating_sub(prefix + failed.width())
            .max(10);
        let command = crate::render::truncate(&command, room);
        let paint = |text: &str, code: &str| {
            if color && !text.is_empty() {
                format!("{code}{text}\x1b[0m")
            } else {
                text.to_owned()
            }
        };
        out.push_str(&format!(
            "  {}  {duration}  {}{gap}{command}{}\n",
            paint(&at, "\x1b[2m"),
            paint(&project, "\x1b[36m"),
            paint(&failed, "\x1b[31m"),
        ));
    }
    out
}

/// 终端界面的「命令」页。
pub fn commands_page(commands: &Commands) -> PageView {
    let pw = project_width(commands);
    let lines: Vec<Line> = commands
        .entries
        .iter()
        .map(|entry| {
            let (at, duration, project, command, failed) = columns(entry, pw);
            let mut spans = vec![
                Span::new(format!("  {at}  "), Style::Dim),
                Span::new(format!("{duration}  "), Style::Normal),
            ];
            if pw > 0 {
                spans.push(Span::new(format!("{project}  "), Style::Accent));
            }
            spans.push(Span::new(command, Style::Normal));
            if !failed.is_empty() {
                spans.push(Span::new(failed, Style::Strong));
            }
            Line { spans }
        })
        .collect();
    PageView {
        heading: heading(commands),
        note: format!("{} 条", commands.entries.len()),
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

    fn command(text: &str, offset_s: i64, duration_ms: i64) -> Activity {
        Activity::new("shell", "command", AT + offset_s * 1_000, 540)
            .with_end(AT + offset_s * 1_000 + duration_ms)
            .with_title(text)
    }

    fn day(entries: Vec<CommandEntry>) -> Commands {
        Commands {
            date: "2026-11-12".to_owned(),
            day_boundary: "04:00".to_owned(),
            start: 0,
            end: 0,
            tz_offset_min: 540,
            entries,
        }
    }

    #[test]
    fn activities_become_entries_in_order() {
        let entries = command_entries(&[
            command("cargo test", 60, 185_000)
                .with_project("lifelog")
                .with_payload("exit", 101)
                .with_payload(payload_keys::CWD, "/srv/lifelog"),
            Activity::new("shell", "command", AT, 540).with_title("ls"),
        ]);
        assert_eq!(entries[0].command, "ls");
        assert_eq!(entries[0].duration_ms, None);
        assert_eq!(entries[1].exit, Some(101));
        assert_eq!(entries[1].cwd.as_deref(), Some("/srv/lifelog"));
        assert_eq!(entries[1].project.as_deref(), Some("lifelog"));
    }

    #[test]
    fn durations_are_short() {
        assert_eq!(elapsed(400), "<1s");
        assert_eq!(elapsed(12_900), "12s");
        assert_eq!(elapsed(185_000), "3m05s");
        assert_eq!(elapsed(3_725_000), "1h02m");
    }

    #[test]
    fn text_has_aligned_columns_and_marks_failures() {
        let text = render_commands(
            &day(command_entries(&[
                command("cargo build --release", 0, 1_200).with_project("weather-station"),
                command("cargo test", 60, 185_000)
                    .with_project("lifelog")
                    .with_payload("exit", 101),
                command("ls", 120, 10),
            ])),
            false,
            80,
        );
        assert_eq!(
            text,
            "\
2026-11-12 周四（04:00 起）  共 3 条

  16:32      1s  weather-station  cargo build --release
  16:33   3m05s  lifelog          cargo test  ✗101
  16:34     <1s                   ls
"
        );
        assert_eq!(
            render_commands(&day(vec![]), false, 80),
            "2026-11-12 周四（04:00 起）  没有记录\n"
        );
    }

    #[test]
    fn page_lines() {
        let view = commands_page(&day(command_entries(&[command("ls", 0, 10)])));
        assert_eq!(view.note, "1 条");
        let Block::Lines { lines } = &view.blocks[0] else {
            panic!("{:?}", view.blocks)
        };
        let text: String = lines[0].spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "  16:32     <1s  ls", "没有项目时不留项目那一列");
    }
}
