//! `notes` 的纯逻辑：一个 Markdown 文件变成一篇文档。

use documents_contracts::Document;
use sha2::{Digest, Sha256};

/// 内容的哈希，判断文件变没变。
pub fn content_hash(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 路径里的 `YYYY-MM-DD`（取第一个）。
pub fn date_in(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    (0..bytes.len().saturating_sub(9)).find_map(|i| {
        let s = path.get(i..i + 10)?;
        let b = s.as_bytes();
        let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
        (digits(0..4) && b[4] == b'-' && digits(5..7) && b[7] == b'-' && digits(8..10))
            .then(|| s.to_owned())
    })
}

/// 路径符不符合摘要文件的格式（`daily/{date}.md` 这种）；符合的话是哪一天。
pub fn summary_date(path: &str, pattern: &str) -> Option<String> {
    let (before, after) = pattern.split_once("{date}")?;
    let date = path.strip_prefix(before)?.strip_suffix(after)?;
    (date_in(date).as_deref() == Some(date)).then(|| date.to_owned())
}

/// 去掉摘要文件第一行的生成标记（`<!-- lifelog: … -->`）。
fn without_marker(text: &str) -> &str {
    match text.split_once('\n') {
        Some((first, rest))
            if first.trim_start().starts_with("<!--") && first.trim_end().ends_with("-->") =>
        {
            rest
        }
        _ => text,
    }
}

/// 文件 → 文档。标题是第一个 `# ` 标题，没有就是文件名；`# ` 标题那一行不进正文。
/// 符合摘要格式的路径变成那一天的摘要文档（同一个键，覆盖生成的那份），其余是笔记。
pub fn to_document(path: &str, text: &str, summary_pattern: &str) -> Document {
    let content = without_marker(text).trim();
    let (title, body) = match content.split_once('\n') {
        Some((first, rest)) if first.starts_with("# ") => {
            (first[2..].trim().to_owned(), rest.trim().to_owned())
        }
        None if content.starts_with("# ") => (content[2..].trim().to_owned(), String::new()),
        _ => {
            let name = path.rsplit('/').next().unwrap_or(path);
            (name.trim_end_matches(".md").to_owned(), content.to_owned())
        }
    };
    match summary_date(path, summary_pattern) {
        Some(date) => Document {
            key: format!("summary_daily:{date}"),
            kind: "summary_daily".to_owned(),
            date: Some(date),
            title,
            body,
            origin: Some(path.to_owned()),
        },
        None => Document {
            key: format!("note:{path}"),
            kind: "note".to_owned(),
            date: date_in(path),
            title,
            body,
            origin: Some(path.to_owned()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_get_a_title_and_a_date() {
        let d = to_document(
            "journal/2026-11-12-读书.md",
            "# 读《人月神话》\n\n没有银弹。",
            "daily/{date}.md",
        );
        assert_eq!(
            (d.key.as_str(), d.kind.as_str()),
            ("note:journal/2026-11-12-读书.md", "note")
        );
        assert_eq!(
            (d.title.as_str(), d.body.as_str()),
            ("读《人月神话》", "没有银弹。")
        );
        assert_eq!(d.date.as_deref(), Some("2026-11-12"));

        let plain = to_document("ideas/气象站.md", "直接写正文\n第二行", "daily/{date}.md");
        assert_eq!(
            (plain.title.as_str(), plain.body.as_str(), plain.date),
            ("气象站", "直接写正文\n第二行", None)
        );
    }

    #[test]
    fn summary_files_go_back_to_their_summary() {
        let text =
            "<!-- lifelog: 由 x 生成，hash=ab -->\n# 2026-11-12 周四\n\n## 概览\n手改过的。\n";
        let d = to_document("daily/2026-11-12.md", text, "daily/{date}.md");
        assert_eq!(
            (d.key.as_str(), d.kind.as_str()),
            ("summary_daily:2026-11-12", "summary_daily")
        );
        assert_eq!(
            (d.title.as_str(), d.body.as_str()),
            ("2026-11-12 周四", "## 概览\n手改过的。")
        );
        assert_eq!(
            summary_date("daily/sub/2026-11-12.md", "daily/{date}.md"),
            None
        );
        assert_eq!(summary_date("daily/notes.md", "daily/{date}.md"), None);
    }

    #[test]
    fn hashes_and_dates() {
        assert_eq!(content_hash("a"), content_hash("a"));
        assert_ne!(content_hash("a"), content_hash("b"));
        assert_eq!(date_in("x/2026-1-12.md"), None);
        assert_eq!(date_in("2026-11-12.md").as_deref(), Some("2026-11-12"));
    }
}
