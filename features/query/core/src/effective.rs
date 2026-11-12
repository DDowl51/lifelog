//! 扣除离开时间。
//!
//! 窗口时段记录的是「焦点停在哪」，人走开了焦点也还停在那里。
//! 所以真正花在窗口上的时间 = 窗口时段 - 离开时段。

use std::collections::{BTreeMap, BTreeSet};

use activity_contracts::Activity;
use query_contracts::{By, EntryKind, TimelineEntry};

/// 扣除离开之后、裁剪到查询范围之内的一段窗口时间。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub start: i64,
    pub end: i64,
    pub tz_offset_min: i32,
    pub app: Option<String>,
    pub title: Option<String>,
    pub project: Option<String>,
    /// 来自 `windows` 里的第几个时段。
    pub span: usize,
}

/// 合并重叠和相接的区间，按开始排序。
fn union(mut intervals: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
    intervals.retain(|(s, e)| e > s);
    intervals.sort_unstable();
    let mut out: Vec<(i64, i64)> = Vec::new();
    for (start, end) in intervals {
        match out.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => out.push((start, end)),
        }
    }
    out
}

fn clip(activity: &Activity, range: (i64, i64)) -> Option<(i64, i64)> {
    let end = activity.ts_end?;
    let (start, end) = (activity.ts_start.max(range.0), end.min(range.1));
    (end > start).then_some((start, end))
}

/// 窗口时段扣掉离开时段，裁剪到 `range` 之内。
///
/// 返回扣除后的各段（按开始排序），以及裁剪、合并后的离开区间。
pub fn effective(
    windows: &[Activity],
    away: &[Activity],
    range: (i64, i64),
) -> (Vec<Piece>, Vec<(i64, i64)>) {
    let away = union(away.iter().filter_map(|a| clip(a, range)).collect());
    let mut pieces = Vec::new();
    for (index, window) in windows.iter().enumerate() {
        let Some((mut start, end)) = clip(window, range) else {
            continue;
        };
        let mut emit = |s: i64, e: i64| {
            if e > s {
                pieces.push(Piece {
                    start: s,
                    end: e,
                    tz_offset_min: window.tz_offset_min,
                    app: window.app.clone(),
                    title: window.title.clone(),
                    project: window.project.clone(),
                    span: index,
                });
            }
        };
        for &(away_start, away_end) in &away {
            if away_end <= start || away_start >= end {
                continue;
            }
            emit(start, away_start.min(end));
            start = start.max(away_end);
            if start >= end {
                break;
            }
        }
        emit(start, end);
    }
    pieces.sort_by_key(|p| (p.start, p.span));
    (pieces, away)
}

/// 按应用、标题或项目分组，返回（组名，总毫秒数，涉及几个时段），按时长降序。
pub fn group(pieces: &[Piece], by: By) -> Vec<(Option<String>, i64, u64)> {
    let mut groups: BTreeMap<Option<String>, (i64, BTreeSet<usize>)> = BTreeMap::new();
    for piece in pieces {
        let key = match by {
            By::App => piece.app.clone(),
            By::Title => piece.title.clone(),
            By::Project => piece.project.clone(),
        };
        let entry = groups.entry(key).or_default();
        entry.0 += piece.end - piece.start;
        entry.1.insert(piece.span);
    }
    let mut rows: Vec<_> = groups
        .into_iter()
        .map(|(key, (total, spans))| (key, total, u64::try_from(spans.len()).unwrap_or(u64::MAX)))
        .collect();
    // 时长降序；一样长时按名字，没有名字的排最后
    rows.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.0.is_none().cmp(&b.0.is_none()))
            .then_with(|| a.0.cmp(&b.0))
    });
    rows
}

/// 排出时间线。
///
/// - 短于 `min_ms` 的窗口段不列出来（切窗口时一闪而过的那些）；
/// - 同一个应用、同一个标题，中间只隔了不到 `merge_gap_ms`（而且中间没有离开）的，合成一项。
pub fn timeline_entries(
    pieces: &[Piece],
    away: &[(i64, i64)],
    away_offset_min: i32,
    min_ms: i64,
    merge_gap_ms: i64,
) -> Vec<TimelineEntry> {
    let mut entries: Vec<TimelineEntry> = Vec::new();
    for piece in pieces.iter().filter(|p| p.end - p.start >= min_ms) {
        if let Some(last) = entries.last_mut()
            && last.app == piece.app
            && last.title == piece.title
            && piece.start - last.end < merge_gap_ms
            && !away.iter().any(|&(s, e)| s < piece.start && e > last.end)
        {
            last.end = last.end.max(piece.end);
            continue;
        }
        entries.push(TimelineEntry {
            kind: EntryKind::Window,
            start: piece.start,
            end: piece.end,
            tz_offset_min: piece.tz_offset_min,
            app: piece.app.clone(),
            title: piece.title.clone(),
            during: Vec::new(),
            commands: Vec::new(),
            visits: Vec::new(),
        });
    }
    entries.extend(away.iter().map(|&(start, end)| TimelineEntry {
        kind: EntryKind::Away,
        start,
        end,
        tz_offset_min: away_offset_min,
        app: None,
        title: None,
        during: Vec::new(),
        commands: Vec::new(),
        visits: Vec::new(),
    }));
    entries.sort_by_key(|e| (e.start, e.kind == EntryKind::Window));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: i64 = 60_000;

    fn window(app: &str, start: i64, end: i64) -> Activity {
        Activity::new("focus", "window_focus", start * M, 480)
            .with_end(end * M)
            .with_app(app)
            .with_title(app)
    }

    fn away(start: i64, end: i64) -> Activity {
        Activity::new("afk", "away", start * M, 480).with_end(end * M)
    }

    fn brief(pieces: &[Piece]) -> Vec<(i64, i64, &str)> {
        pieces
            .iter()
            .map(|p| (p.start / M, p.end / M, p.app.as_deref().unwrap()))
            .collect()
    }

    #[test]
    fn away_time_is_cut_out_of_windows() {
        let (pieces, away) = effective(
            &[window("ghostty", 0, 60), window("zen", 60, 90)],
            &[away(20, 30), away(50, 70)],
            (0, 1000 * M),
        );
        assert_eq!(
            brief(&pieces),
            [(0, 20, "ghostty"), (30, 50, "ghostty"), (70, 90, "zen")]
        );
        assert_eq!(away, [(20 * M, 30 * M), (50 * M, 70 * M)]);
    }

    #[test]
    fn everything_is_clipped_to_the_range() {
        let (pieces, away) = effective(
            &[window("ghostty", 0, 60)],
            &[away(50, 100)],
            (10 * M, 55 * M),
        );
        assert_eq!(brief(&pieces), [(10, 50, "ghostty")]);
        assert_eq!(away, [(50 * M, 55 * M)]);
    }

    #[test]
    fn overlapping_away_spans_are_merged_and_instants_ignored() {
        let instant = Activity::new("focus", "window_focus", 5 * M, 480).with_app("x");
        let (pieces, away) = effective(
            &[window("a", 0, 100), instant],
            &[away(10, 30), away(20, 40), away(40, 50)],
            (0, 1000 * M),
        );
        assert_eq!(away, [(10 * M, 50 * M)]);
        assert_eq!(brief(&pieces), [(0, 10, "a"), (50, 100, "a")]);
    }

    #[test]
    fn a_window_entirely_inside_away_disappears() {
        let (pieces, _) = effective(&[window("a", 10, 20)], &[away(0, 30)], (0, 1000 * M));
        assert!(pieces.is_empty());
    }

    #[test]
    fn grouping_counts_source_spans_not_pieces() {
        let (pieces, _) = effective(
            &[
                window("ghostty", 0, 60),
                window("zen", 60, 70),
                window("ghostty", 70, 80),
            ],
            &[away(20, 30)],
            (0, 1000 * M),
        );
        let rows = group(&pieces, By::App);
        assert_eq!(
            rows,
            [
                (Some("ghostty".to_owned()), 60 * M, 2),
                (Some("zen".to_owned()), 10 * M, 1)
            ]
        );
    }

    #[test]
    fn ties_sort_by_name_and_unnamed_last() {
        let mut nameless = window("x", 20, 30);
        nameless.app = None;
        let (pieces, _) = effective(
            &[window("b", 0, 10), window("a", 10, 20), nameless],
            &[],
            (0, 1000 * M),
        );
        let keys: Vec<Option<String>> = group(&pieces, By::App).into_iter().map(|r| r.0).collect();
        assert_eq!(keys, [Some("a".to_owned()), Some("b".to_owned()), None]);
    }

    #[test]
    fn timeline_drops_blips_merges_repeats_and_shows_away() {
        let blip = Activity::new("focus", "window_focus", 30 * M, 480)
            .with_end(30 * M + 5_000)
            .with_app("blip");
        let (pieces, away_spans) = effective(
            &[
                window("ghostty", 0, 30),
                blip,
                // 一闪而过之后又回到同一个窗口：合成一项
                Activity::new("focus", "window_focus", 30 * M + 5_000, 480)
                    .with_end(40 * M)
                    .with_app("ghostty")
                    .with_title("ghostty"),
                window("zen", 40, 80),
            ],
            &[away(50, 60)],
            (0, 1000 * M),
        );
        let entries = timeline_entries(&pieces, &away_spans, 480, 10_000, 60_000);
        let brief: Vec<(i64, i64, EntryKind, Option<&str>)> = entries
            .iter()
            .map(|e| (e.start / M, e.end / M, e.kind, e.app.as_deref()))
            .collect();
        assert_eq!(
            brief,
            [
                (0, 40, EntryKind::Window, Some("ghostty")),
                (40, 50, EntryKind::Window, Some("zen")),
                (50, 60, EntryKind::Away, None),
                // 中间隔着离开，不和前面那段 zen 合并
                (60, 80, EntryKind::Window, Some("zen")),
            ]
        );
    }
}
