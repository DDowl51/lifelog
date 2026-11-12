//! 把窗口、命令、网页合在一起看：命令和网页挂到时间线的各段上；按项目汇总一天。

use std::collections::BTreeMap;

use query_contracts::{By, CommandEntry, EntryKind, Mark, ProjectRow, TimelineEntry, VisitEntry};

use crate::effective::{Piece, group};
use crate::visits::host;

/// 命令和网页统一成（时刻，文字，补充），好用同一套规则挂上去。
fn command_marks(commands: &[CommandEntry]) -> Vec<(i64, String, Option<String>)> {
    commands
        .iter()
        .filter(|c| !c.command.is_empty())
        .map(|c| (c.at, c.command.clone(), None))
        .collect()
}

fn visit_marks(visits: &[VisitEntry]) -> Vec<(i64, String, Option<String>)> {
    visits
        .iter()
        .map(|v| match &v.title {
            Some(title) => (v.at, title.clone(), Some(host(&v.url).to_owned())),
            None => (v.at, v.url.clone(), None),
        })
        .collect()
}

/// 时刻 `at` 归时间线的哪一项：开始不晚于它的最后一项；比所有项都早的归第一个窗口项。
/// 落在离开期间的不归任何一项。
fn owner(entries: &[TimelineEntry], at: i64) -> Option<usize> {
    let index = match entries.iter().rposition(|e| e.start <= at) {
        Some(i) => i,
        None => entries.iter().position(|e| e.kind == EntryKind::Window)?,
    };
    (entries[index].kind == EntryKind::Window).then_some(index)
}

/// 把一组（时刻，文字，补充）挂到各项上：同一项里相同的合在一起计数，按第一次出现的先后。
fn distribute(
    entries: &[TimelineEntry],
    mut items: Vec<(i64, String, Option<String>)>,
) -> Vec<Vec<Mark>> {
    items.sort_by_key(|(at, _, _)| *at);
    let mut out: Vec<Vec<Mark>> = vec![Vec::new(); entries.len()];
    for (at, text, detail) in items {
        let Some(i) = owner(entries, at) else {
            continue;
        };
        match out[i]
            .iter_mut()
            .find(|m| m.text == text && m.detail == detail)
        {
            Some(mark) => mark.count += 1,
            None => out[i].push(Mark {
                at,
                text,
                detail,
                count: 1,
            }),
        }
    }
    out
}

/// 把命令和网页挂到时间线的各项上。`entries` 按开始时刻排好。
pub fn attach(entries: &mut [TimelineEntry], commands: &[CommandEntry], visits: &[VisitEntry]) {
    let by_command = distribute(entries, command_marks(commands));
    let by_visit = distribute(entries, visit_marks(visits));
    for ((entry, commands), visits) in entries.iter_mut().zip(by_command).zip(by_visit) {
        entry.commands = commands;
        entry.visits = visits;
    }
}

/// 一天按项目汇总。窗口时长来自扣掉离开之后的各段。
pub fn project_rows(
    pieces: &[Piece],
    commands: &[CommandEntry],
    visits: &[VisitEntry],
) -> Vec<ProjectRow> {
    fn row<'a>(
        rows: &'a mut BTreeMap<Option<String>, ProjectRow>,
        project: &Option<String>,
    ) -> &'a mut ProjectRow {
        rows.entry(project.clone()).or_insert_with(|| ProjectRow {
            project: project.clone(),
            window_ms: 0,
            commands: 0,
            visits: 0,
        })
    }
    let mut rows: BTreeMap<Option<String>, ProjectRow> = BTreeMap::new();
    for (project, ms, _) in group(pieces, By::Project) {
        row(&mut rows, &project).window_ms += ms;
    }
    for c in commands {
        row(&mut rows, &c.project).commands += 1;
    }
    for v in visits {
        row(&mut rows, &v.project).visits += 1;
    }
    let mut rows: Vec<ProjectRow> = rows.into_values().collect();
    rows.sort_by(|a, b| {
        a.project
            .is_none()
            .cmp(&b.project.is_none())
            .then_with(|| b.window_ms.cmp(&a.window_ms))
            .then_with(|| b.commands.cmp(&a.commands))
            .then_with(|| b.visits.cmp(&a.visits))
            .then_with(|| a.project.cmp(&b.project))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: i64 = 60_000;

    fn entry(kind: EntryKind, start: i64, end: i64, title: &str) -> TimelineEntry {
        TimelineEntry {
            kind,
            start: start * M,
            end: end * M,
            tz_offset_min: 480,
            app: None,
            title: Some(title.to_owned()),
            during: Vec::new(),
            commands: Vec::new(),
            visits: Vec::new(),
        }
    }

    fn command(at: i64, text: &str, project: Option<&str>) -> CommandEntry {
        CommandEntry {
            at: at * M,
            tz_offset_min: 480,
            duration_ms: None,
            exit: None,
            command: text.to_owned(),
            cwd: None,
            project: project.map(str::to_owned),
        }
    }

    fn visit(at: i64, title: Option<&str>, url: &str, project: Option<&str>) -> VisitEntry {
        VisitEntry {
            at: at * M,
            tz_offset_min: 480,
            browser: Some("zen".to_owned()),
            title: title.map(str::to_owned),
            url: url.to_owned(),
            project: project.map(str::to_owned),
        }
    }

    fn texts(marks: &[Mark]) -> Vec<(String, u32)> {
        marks.iter().map(|m| (m.text.clone(), m.count)).collect()
    }

    #[test]
    fn marks_go_to_the_entry_they_happen_in() {
        let mut entries = vec![
            entry(EntryKind::Window, 10, 20, "editor"),
            entry(EntryKind::Window, 25, 30, "claude"),
            entry(EntryKind::Away, 30, 40, ""),
            entry(EntryKind::Window, 40, 50, "editor"),
        ];
        attach(
            &mut entries,
            &[
                command(5, "早于第一段", None),
                command(12, "cargo test", None),
                command(15, "cargo test", None),
                command(22, "间隙里", None),
                command(35, "离开期间", None),
                command(45, "git push", None),
            ],
            &[
                visit(26, Some("jiff - Rust"), "https://docs.rs/jiff", None),
                visit(27, None, "file:///a.pdf", None),
            ],
        );
        let t = |s: &str, n: u32| (s.to_owned(), n);
        assert_eq!(
            texts(&entries[0].commands),
            [t("早于第一段", 1), t("cargo test", 2), t("间隙里", 1)],
            "到下一段开始之前都算这一段"
        );
        assert!(entries[1].commands.is_empty());
        assert_eq!(
            texts(&entries[1].visits),
            [t("jiff - Rust", 1), t("file:///a.pdf", 1)]
        );
        assert_eq!(entries[1].visits[0].detail.as_deref(), Some("docs.rs"));
        assert_eq!(entries[1].visits[1].detail, None);
        assert!(entries[2].commands.is_empty(), "离开期间的不挂");
        assert_eq!(texts(&entries[3].commands), [t("git push", 1)]);
    }

    #[test]
    fn nothing_to_attach_to() {
        let mut entries = vec![entry(EntryKind::Away, 0, 10, "")];
        attach(&mut entries, &[command(5, "x", None)], &[]);
        assert!(entries[0].commands.is_empty());
    }

    #[test]
    fn projects_combine_windows_commands_and_visits() {
        let piece = |start: i64, end: i64, project: Option<&str>| Piece {
            start: start * M,
            end: end * M,
            tz_offset_min: 480,
            app: None,
            title: None,
            project: project.map(str::to_owned),
            span: usize::try_from(start).unwrap_or(0),
        };
        let rows = project_rows(
            &[
                piece(0, 60, None),
                piece(60, 90, Some("a")),
                piece(90, 100, Some("b")),
            ],
            &[
                command(1, "x", Some("b")),
                command(2, "y", Some("b")),
                command(3, "z", Some("c")),
            ],
            &[
                visit(1, None, "https://a/", Some("a")),
                visit(2, None, "https://n/", None),
            ],
        );
        let brief: Vec<(Option<&str>, i64, u32, u32)> = rows
            .iter()
            .map(|r| (r.project.as_deref(), r.window_ms / M, r.commands, r.visits))
            .collect();
        assert_eq!(
            brief,
            [
                (Some("a"), 30, 0, 1),
                (Some("b"), 10, 2, 0),
                (Some("c"), 0, 1, 0),
                (None, 60, 0, 1),
            ],
            "归不了类的排最后，哪怕时间最长"
        );
    }
}
