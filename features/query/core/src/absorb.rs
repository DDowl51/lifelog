//! 把穿插的短暂窗口并进前后的长项，让时间线读起来像「做了哪几件事」。
//!
//! 真实的数据里，人会在几个窗口之间来回切：看一眼 Claude 的回复、跑一条命令、又回到编辑器。
//! 每一次都是一个真实的时段，但一天下来时间线有几十项不到一分钟的，看不出在做什么。
//!
//! 规则（按离开切成若干段，各段分别处理）：
//!
//! 1. 不短于 `absorb_ms` 的项是「锚点」。
//! 2. 短项并进它前面最近的锚点；段首的短项并进第一个锚点。
//! 3. 相邻的两个锚点是同一个应用、同一个标题（中间只被短项打断）时，合成一项。
//! 4. 一段里一个锚点都没有时，整段合成一项，用其中最长的那个作标签。
//!
//! 并进去的短项记在 `during` 里，时间仍然算在这一项的 `[start, end)` 内。

use std::collections::BTreeMap;

use query_contracts::{Brief, EntryKind, TimelineEntry};

type Key = (Option<String>, Option<String>);

fn key(entry: &TimelineEntry) -> Key {
    (entry.app.clone(), entry.title.clone())
}

/// 合并一组（同一段里连续的若干项），`main` 是作标签的那一项在组里的下标。
fn merge(group: &[TimelineEntry], main: usize) -> TimelineEntry {
    let label = &group[main];
    let main_key = key(label);
    let mut others: BTreeMap<Key, i64> = BTreeMap::new();
    for entry in group {
        if key(entry) != main_key {
            *others.entry(key(entry)).or_default() += entry.end - entry.start;
        }
        // 已经并过一次的项，带着它自己的穿插
        for brief in &entry.during {
            if (brief.app.clone(), brief.title.clone()) != main_key {
                *others
                    .entry((brief.app.clone(), brief.title.clone()))
                    .or_default() += brief.total_ms;
            }
        }
    }
    let mut during: Vec<Brief> = others
        .into_iter()
        .map(|((app, title), total_ms)| Brief {
            app,
            title,
            total_ms,
        })
        .collect();
    during.sort_by(|a, b| {
        b.total_ms
            .cmp(&a.total_ms)
            .then_with(|| a.title.cmp(&b.title))
    });
    TimelineEntry {
        kind: EntryKind::Window,
        start: group.iter().map(|e| e.start).min().unwrap_or(label.start),
        end: group.iter().map(|e| e.end).max().unwrap_or(label.end),
        tz_offset_min: label.tz_offset_min,
        app: label.app.clone(),
        title: label.title.clone(),
        during,
        commands: Vec::new(),
        visits: Vec::new(),
    }
}

/// 处理一段（中间没有离开的连续窗口项）。
fn absorb_segment(segment: &[TimelineEntry], absorb_ms: i64) -> Vec<TimelineEntry> {
    if segment.is_empty() {
        return Vec::new();
    }
    let anchors: Vec<usize> = (0..segment.len())
        .filter(|&i| segment[i].end - segment[i].start >= absorb_ms)
        .collect();
    if anchors.is_empty() {
        // 按合计时长挑标签：同一个窗口来回几次也算在一起
        let mut totals: BTreeMap<Key, i64> = BTreeMap::new();
        for entry in segment {
            *totals.entry(key(entry)).or_default() += entry.end - entry.start;
        }
        let main = (0..segment.len())
            .max_by_key(|&i| (totals[&key(&segment[i])], std::cmp::Reverse(i)))
            .unwrap_or(0);
        return vec![merge(segment, main)];
    }

    // 每组从一个锚点开始，到下一个「不同的」锚点之前为止；段首的短项归第一组
    let mut out = Vec::new();
    let mut group_start = 0;
    let mut main = anchors[0];
    for &anchor in &anchors[1..] {
        if key(&segment[anchor]) == key(&segment[main]) {
            continue;
        }
        out.push(merge(&segment[group_start..anchor], main - group_start));
        group_start = anchor;
        main = anchor;
    }
    out.push(merge(&segment[group_start..], main - group_start));
    out
}

/// 对整条时间线（窗口项和离开项按时间排好）做合并。`absorb_ms` 为 0 时原样返回。
pub fn absorb(entries: Vec<TimelineEntry>, absorb_ms: i64) -> Vec<TimelineEntry> {
    if absorb_ms <= 0 {
        return entries;
    }
    let mut out = Vec::with_capacity(entries.len());
    let mut segment: Vec<TimelineEntry> = Vec::new();
    for entry in entries {
        match entry.kind {
            EntryKind::Window => segment.push(entry),
            EntryKind::Away => {
                out.extend(absorb_segment(&segment, absorb_ms));
                segment.clear();
                out.push(entry);
            }
        }
    }
    out.extend(absorb_segment(&segment, absorb_ms));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i64 = 1_000;

    fn window(title: &str, start: i64, end: i64) -> TimelineEntry {
        TimelineEntry {
            kind: EntryKind::Window,
            start: start * S,
            end: end * S,
            tz_offset_min: 480,
            app: Some("ghostty".to_owned()),
            title: Some(title.to_owned()),
            during: Vec::new(),
            commands: Vec::new(),
            visits: Vec::new(),
        }
    }

    fn away(start: i64, end: i64) -> TimelineEntry {
        TimelineEntry {
            kind: EntryKind::Away,
            app: None,
            title: None,
            ..window("", start, end)
        }
    }

    /// （开始秒，结束秒，标题，穿插的标题和秒数）
    type Brief = (i64, i64, String, Vec<(String, i64)>);

    fn brief(entries: &[TimelineEntry]) -> Vec<Brief> {
        entries
            .iter()
            .map(|e| {
                (
                    e.start / S,
                    e.end / S,
                    e.title.clone().unwrap_or_else(|| "离开".to_owned()),
                    e.during
                        .iter()
                        .map(|b| (b.title.clone().unwrap_or_default(), b.total_ms / S))
                        .collect(),
                )
            })
            .collect()
    }

    fn t(s: &str) -> String {
        s.to_owned()
    }

    #[test]
    fn short_items_fold_into_the_previous_long_one() {
        let out = absorb(
            vec![
                window("claude", 0, 600),
                window("shell", 600, 620),
                window("nvim", 620, 640),
                window("shell", 640, 650),
                window("editor", 650, 900),
            ],
            60 * S,
        );
        assert_eq!(
            brief(&out),
            [
                (0, 650, t("claude"), vec![(t("shell"), 30), (t("nvim"), 20)]),
                (650, 900, t("editor"), vec![]),
            ]
        );
    }

    #[test]
    fn interrupted_work_on_the_same_thing_is_one_item() {
        let out = absorb(
            vec![
                window("claude", 0, 300),
                window("shell", 300, 330),
                window("claude", 330, 700),
            ],
            60 * S,
        );
        assert_eq!(brief(&out), [(0, 700, t("claude"), vec![(t("shell"), 30)])]);
    }

    #[test]
    fn leading_short_items_join_the_first_long_one() {
        let out = absorb(
            vec![window("Ghostty", 0, 20), window("nvim", 20, 400)],
            60 * S,
        );
        assert_eq!(brief(&out), [(0, 400, t("nvim"), vec![(t("Ghostty"), 20)])]);
    }

    #[test]
    fn away_splits_segments_and_stays() {
        let out = absorb(
            vec![
                window("claude", 0, 300),
                window("shell", 300, 320),
                away(320, 900),
                window("shell", 900, 920),
                window("claude", 920, 930),
                window("shell", 930, 950),
            ],
            60 * S,
        );
        assert_eq!(
            brief(&out),
            [
                (0, 320, t("claude"), vec![(t("shell"), 20)]),
                (320, 900, t("离开"), vec![]),
                // 没有锚点的一段：按合计时长挑标签
                (900, 950, t("shell"), vec![(t("claude"), 10)]),
            ]
        );
    }

    #[test]
    fn zero_turns_it_off() {
        let input = vec![window("a", 0, 10), window("b", 10, 20)];
        assert_eq!(absorb(input.clone(), 0), input);
    }
}
