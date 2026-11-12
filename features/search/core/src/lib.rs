//! `search` 的纯逻辑：一条活动要索引哪些文字、把查询词变成 FTS5 的表达式、合并结果。

use activity_contracts::{Activity, StoredActivity, payload_keys};
use search_contracts::Hit;
use serde_json::Value;

fn url(activity: &Activity) -> Option<&str> {
    activity
        .payload
        .get(payload_keys::URL)
        .and_then(Value::as_str)
}

/// 一条活动里可以被搜到的文字：标题（命令的标题就是命令）、网址、项目名。
pub fn searchable_text(activity: &Activity) -> String {
    [
        activity.title.as_deref(),
        url(activity),
        activity.project.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
}

/// 查询词 → FTS5 的 MATCH 表达式：每个词都要出现；最后一个词是英文或数字时按前缀匹配
/// （`jif` 也能找到 `jiff`）。没有词是 `None`。
pub fn match_expression(words: &[String]) -> Option<String> {
    let mut seen: Vec<&String> = Vec::new();
    for w in words {
        if !seen.contains(&w) {
            seen.push(w);
        }
    }
    let last = seen.len().checked_sub(1)?;
    let terms: Vec<String> = seen
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let quoted = format!("\"{}\"", w.replace('"', "\"\""));
            if i == last && w.chars().all(|c| c.is_ascii_alphanumeric()) {
                format!("{quoted}*")
            } else {
                quoted
            }
        })
        .collect();
    Some(terms.join(" "))
}

/// 把命中的活动（已经按想要的顺序排好）合并：同一来源里标题和网址都一样的合成一条，
/// 位置和时刻取最近的那一次。
pub fn group_hits(found: &[StoredActivity]) -> Vec<Hit> {
    let mut hits: Vec<Hit> = Vec::new();
    for stored in found {
        let a = &stored.activity;
        let url = url(a).map(str::to_owned);
        match hits
            .iter_mut()
            .find(|h| h.source == a.source && h.title == a.title && h.url == url)
        {
            Some(hit) => {
                hit.count += 1;
                if a.ts_start > hit.at {
                    hit.at = a.ts_start;
                    hit.tz_offset_min = a.tz_offset_min;
                }
            }
            None => hits.push(Hit {
                source: a.source.clone(),
                kind: a.kind.clone(),
                at: a.ts_start,
                tz_offset_min: a.tz_offset_min,
                app: a.app.clone(),
                title: a.title.clone(),
                url,
                project: a.project.clone(),
                count: 1,
            }),
        }
    }
    hits
}

/// 正文里第一个命中的词附近的一段（前后各留一些字）；哪个词都没找到就给开头。换行换成空格。
pub fn snippet(body: &str, words: &[&str], max_chars: usize) -> String {
    let flat: String = body
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    let lower = flat.to_lowercase();
    let chars: Vec<char> = flat.chars().collect();
    // 找到的位置换算成字符下标（to_lowercase 对中日文不改变长度，对英文按字节找后再换算）
    let hit = words
        .iter()
        .filter(|w| !w.is_empty())
        .filter_map(|w| lower.find(&w.to_lowercase()))
        .min()
        .map(|byte| lower[..byte].chars().count());
    let start = hit.map_or(0, |i| i.saturating_sub(max_chars / 3));
    let end = (start + max_chars).min(chars.len());
    let mut out: String = chars[start..end].iter().collect();
    out = out.trim().to_owned();
    if start > 0 {
        out = format!("…{out}");
    }
    if end < chars.len() {
        out.push('…');
    }
    out
}

/// 倒数排名融合（RRF，k = 60）：几路各自排好的结果合成一路。在越多路里、排得越前的越靠前。
pub fn rrf(lists: &[Vec<String>]) -> Vec<String> {
    const K: f64 = 60.0;
    let mut scores: Vec<(String, f64, usize)> = Vec::new();
    for list in lists {
        for (rank, key) in list.iter().enumerate() {
            let add = 1.0 / (K + rank as f64 + 1.0);
            match scores.iter_mut().find(|(k, _, _)| k == key) {
                Some(entry) => entry.1 += add,
                None => {
                    let order = scores.len();
                    scores.push((key.clone(), add, order));
                }
            }
        }
    }
    scores.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.2.cmp(&b.2)));
    scores.into_iter().map(|(k, _, _)| k).collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use activity_contracts::ActivityId;

    use super::*;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn text_to_index() {
        let visit = Activity::new("browsing", "visit", 0, 0)
            .with_title("jiff - Rust")
            .with_payload(payload_keys::URL, "https://docs.rs/jiff")
            .with_project("lifelog");
        assert_eq!(
            searchable_text(&visit),
            "jiff - Rust https://docs.rs/jiff lifelog"
        );
        assert_eq!(searchable_text(&Activity::new("afk", "away", 0, 0)), "");
    }

    #[test]
    fn expressions() {
        assert_eq!(
            match_expression(&words(&["jiff", "文档"])).as_deref(),
            Some(r#""jiff" "文档""#)
        );
        assert_eq!(
            match_expression(&words(&["文档", "jif"])).as_deref(),
            Some(r#""文档" "jif"*"#)
        );
        assert_eq!(
            match_expression(&words(&["a\"b", "a\"b"])).as_deref(),
            Some(r#""a""b""#),
            "引号要转义；重复的去掉"
        );
        assert_eq!(match_expression(&[]), None);
    }

    #[test]
    fn identical_hits_are_merged_keeping_the_latest() {
        let stored = |id: i64, at: i64, title: &str| StoredActivity {
            id: ActivityId(id),
            activity: Activity::new("focus", "window_focus", at, 480).with_title(title),
            ingested_at: 0,
        };
        let hits = group_hits(&[
            stored(1, 300, "a"),
            stored(2, 100, "b"),
            stored(3, 200, "a"),
        ]);
        let brief: Vec<(&str, i64, u32)> = hits
            .iter()
            .map(|h| (h.title.as_deref().unwrap(), h.at, h.count))
            .collect();
        assert_eq!(brief, [("a", 300, 2), ("b", 100, 1)]);
    }
    #[test]
    fn snippets_center_on_the_first_hit() {
        let body = "## 概览\n这一天主要在调试 weather-station 的固件，后来又重装了 lifelog。";
        assert_eq!(snippet(body, &["固件"], 12), "…on 的固件，后来又重装…");
        assert_eq!(snippet(body, &["没有"], 8), "## 概览 这一…");
        assert_eq!(snippet("短", &["短"], 50), "短");
        assert_eq!(
            snippet("Tokio Runtime", &["runtime"], 50),
            "Tokio Runtime",
            "不分大小写"
        );
    }

    #[test]
    fn rrf_rewards_agreement() {
        let fts = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        let vec = vec!["c".to_owned(), "d".to_owned()];
        assert_eq!(
            rrf(&[fts, vec]),
            ["c", "a", "b", "d"],
            "两路都有的 c 排第一"
        );
        assert!(rrf(&[]).is_empty());
    }
}
