//! `summary` 的纯逻辑：把一天压成发给模型的原料、默认提示词、摘要文件的格式。

use std::collections::BTreeMap;
use std::fmt::Write as _;

use jiff::Timestamp;
use jiff::civil::{Date, Weekday};
use jiff::tz::Offset;
use query_contracts::{Commands, EntryKind, ProjectDay, Timeline, Visits};
use sha2::{Digest, Sha256};

/// 默认的提示词。
pub const DEFAULT_PROMPT: &str = "\
你是帮用户回顾一天的助手。用户会给你这一天在电脑上的活动记录（窗口、命令、网页），已经脱敏过。
请用简体中文写一份简洁的每日回顾，Markdown 格式，只用下面这几节（二级标题）：
## 概览：两三句话，这一天主要在做什么。
## 按项目：每个项目做了什么、进展到哪；没归类的时间如果能看出在做什么也写上。
## 时间线要点：几个关键时段，一行一个。
## 未完成 / 明天继续：从记录里能推断出来的才写，推断不出来就省略这一节。
只根据记录写，不要编造；拿不准的写「可能」。不要写前言和结尾的客套话，不要重复日期标题。";

/// 一天的原料。
pub struct DayInput<'a> {
    pub projects: &'a ProjectDay,
    pub timeline: &'a Timeline,
    pub commands: &'a Commands,
    pub visits: &'a Visits,
}

/// 原料里各部分最多列几条。
const TIMELINE_ITEMS: usize = 40;
const TIMELINE_MIN_MS: i64 = 5 * 60_000;
const COMMAND_ITEMS: usize = 40;
const VISIT_ITEMS: usize = 30;

pub fn weekday(date: &str) -> &'static str {
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

/// `1 小时 05 分`、`40 分`、`不到 1 分`。
pub fn duration(ms: i64) -> String {
    let minutes = ms.max(0) / 60_000;
    match (minutes / 60, minutes % 60) {
        (0, 0) => "不到 1 分".to_owned(),
        (0, m) => format!("{m} 分"),
        (h, m) => format!("{h} 小时 {m:02} 分"),
    }
}

fn clock(utc_ms: i64, offset_min: i32) -> String {
    Timestamp::from_millisecond(utc_ms)
        .ok()
        .zip(Offset::from_seconds(offset_min * 60).ok())
        .map(|(ts, offset)| offset.to_datetime(ts))
        .map_or_else(
            || "--:--".to_owned(),
            |dt| format!("{:02}:{:02}", dt.hour(), dt.minute()),
        )
}

/// 相同的合在一起计数，按次数降序（次数一样按第一次出现）。
fn tally<'a>(items: impl Iterator<Item = &'a str>) -> Vec<(&'a str, usize)> {
    let mut order: Vec<&str> = Vec::new();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for item in items {
        let n = counts.entry(item).or_insert(0);
        if *n == 0 {
            order.push(item);
        }
        *n += 1;
    }
    let mut out: Vec<(&str, usize)> = order.into_iter().map(|i| (i, counts[i])).collect();
    out.sort_by_key(|item| std::cmp::Reverse(item.1));
    out
}

fn times(n: usize) -> String {
    if n > 1 {
        format!(" ×{n}")
    } else {
        String::new()
    }
}

fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
}

/// 这一天在电脑前的总时长（窗口时间，已扣离开）。
pub fn active_ms(input: &DayInput<'_>) -> i64 {
    input.projects.rows.iter().map(|r| r.window_ms).sum()
}

/// 把一天压成发给模型的原料。`hidden` 里的来源（`focus`、`shell`、`browsing`）不出现；
/// 超过 `max_chars` 个字符的部分截掉。只给网页的标题和域名，不给完整网址。
pub fn digest(input: &DayInput<'_>, hidden: &[String], max_chars: usize) -> String {
    let hide = |s: &str| hidden.iter().any(|h| h == s);
    let p = input.projects;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "日期：{} {}（一天从 {} 开始）",
        p.date,
        weekday(&p.date),
        p.day_boundary
    );
    let _ = writeln!(
        out,
        "在电脑前共 {}，离开 {}。",
        duration(active_ms(input)),
        duration(p.away_ms)
    );

    out.push_str("\n## 按项目\n");
    for row in &p.rows {
        let mut parts = vec![duration(row.window_ms)];
        if !hide("shell") && row.commands > 0 {
            parts.push(format!("{} 条命令", row.commands));
        }
        if !hide("browsing") && row.visits > 0 {
            parts.push(format!("{} 个网页", row.visits));
        }
        let name = row.project.as_deref().unwrap_or("（未归类）");
        let _ = writeln!(out, "- {name}：{}", parts.join("，"));
    }

    if !hide("focus") {
        out.push_str("\n## 时间线（5 分钟以上的段）\n");
        let long = input
            .timeline
            .entries
            .iter()
            .filter(|e| e.end - e.start >= TIMELINE_MIN_MS)
            .take(TIMELINE_ITEMS);
        for e in long {
            let span = format!(
                "{}–{}",
                clock(e.start, e.tz_offset_min),
                clock(e.end, e.tz_offset_min)
            );
            match e.kind {
                EntryKind::Away => {
                    let _ = writeln!(out, "- {span} 离开");
                }
                EntryKind::Window => {
                    let what = match (&e.app, &e.title) {
                        (Some(app), Some(title)) if title != app => format!("{app}「{title}」"),
                        (Some(app), _) => app.clone(),
                        (None, Some(title)) => format!("「{title}」"),
                        (None, None) => "（不明）".to_owned(),
                    };
                    let mut extra = Vec::new();
                    if !hide("shell") && !e.commands.is_empty() {
                        let cmds: Vec<String> = e
                            .commands
                            .iter()
                            .take(3)
                            .map(|m| format!("{}{}", m.text, times(m.count as usize)))
                            .collect();
                        extra.push(format!("命令：{}", cmds.join("；")));
                    }
                    if !hide("browsing") && !e.visits.is_empty() {
                        // 有标题的给标题；没有标题时 `text` 是完整网址，只给域名
                        let pages: Vec<String> = e
                            .visits
                            .iter()
                            .take(3)
                            .map(|m| match &m.detail {
                                Some(_) => m.text.clone(),
                                None => host(&m.text).to_owned(),
                            })
                            .filter(|p| !p.is_empty())
                            .collect();
                        extra.push(format!("网页：{}", pages.join("；")));
                    }
                    let extra = if extra.is_empty() {
                        String::new()
                    } else {
                        format!("（{}）", extra.join("，"))
                    };
                    let _ = writeln!(out, "- {span} {}，{what}{extra}", duration(e.end - e.start));
                }
            }
        }
    }

    if !hide("shell") && !input.commands.entries.is_empty() {
        out.push_str("\n## 常用的命令\n");
        let mut by_project: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for c in &input.commands.entries {
            if !c.command.trim().is_empty() {
                by_project
                    .entry(c.project.as_deref().unwrap_or("（未归类）"))
                    .or_default()
                    .push(c.command.as_str());
            }
        }
        let mut left = COMMAND_ITEMS;
        for (project, list) in &by_project {
            if left == 0 {
                break;
            }
            let _ = writeln!(out, "### {project}");
            for (command, n) in tally(list.iter().copied()).into_iter().take(left.min(15)) {
                let _ = writeln!(out, "- {command}{}", times(n));
                left = left.saturating_sub(1);
            }
        }
    }

    if !hide("browsing") && !input.visits.entries.is_empty() {
        out.push_str("\n## 看过的网页\n");
        let labels: Vec<String> = input
            .visits
            .entries
            .iter()
            .map(|v| match (&v.title, host(&v.url)) {
                (Some(t), "") => t.clone(),
                (Some(t), h) => format!("{t}（{h}）"),
                (None, h) => h.to_owned(),
            })
            .filter(|label| !label.is_empty())
            .collect();
        for (label, n) in tally(labels.iter().map(String::as_str))
            .into_iter()
            .take(VISIT_ITEMS)
        {
            let _ = writeln!(out, "- {label}{}", times(n));
        }
    }

    if out.chars().count() > max_chars {
        let mut cut: String = out.chars().take(max_chars).collect();
        cut.push_str("\n…（太长，后面省略）\n");
        return cut;
    }
    out
}

fn hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

const MARK: &str = "<!-- lifelog:";

/// 摘要文件：第一行记下生成时内容的哈希，下面是标题和正文。
pub fn render_file(date: &str, body: &str, model: &str) -> String {
    let content = format!("# {date} {}\n\n{}\n", weekday(date), body.trim());
    format!(
        "{MARK} 由 {model} 生成，hash={}。手改之后不会被自动覆盖；想重新生成就删掉这个文件，或者用 --force -->\n{content}",
        hash(&content)
    )
}

/// 文件的正文（去掉第一行的标记和日期标题），存进文档库用。
pub fn body_of(file: &str) -> String {
    let content = match file.split_once('\n') {
        Some((first, rest)) if first.starts_with(MARK) => rest,
        _ => file,
    };
    let content = content.trim_start();
    match content.split_once('\n') {
        Some((first, rest)) if first.starts_with("# ") => rest.trim().to_owned(),
        _ => content.trim().to_owned(),
    }
}

/// 已经有的摘要文件是什么情况。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    /// 生成之后没人动过。
    Untouched,
    /// 被手改过，或者不是生成的。
    Edited,
}

pub fn file_state(file: &str) -> FileState {
    let Some((first, content)) = file.split_once('\n') else {
        return FileState::Edited;
    };
    let Some(recorded) = first
        .strip_prefix(MARK)
        .and_then(|r| r.split_once("hash="))
        .map(|(_, h)| {
            h.chars()
                .take_while(char::is_ascii_hexdigit)
                .collect::<String>()
        })
    else {
        return FileState::Edited;
    };
    if recorded == hash(content) {
        FileState::Untouched
    } else {
        FileState::Edited
    }
}

#[cfg(test)]
mod tests;
