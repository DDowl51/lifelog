//! 窗口标题规范化：去掉会抖动、但不改变「在做什么」的部分。

use regex::Regex;

/// 规范化规则。
#[derive(Debug, Clone)]
pub struct TitleRules {
    suffixes: Vec<String>,
    rewrites: Vec<(Regex, String)>,
}

impl TitleRules {
    /// `suffixes`：要去掉的应用名后缀（如 `" — Zen Browser"`）。
    pub fn new(suffixes: Vec<String>) -> Self {
        Self {
            suffixes,
            rewrites: Vec::new(),
        }
    }

    /// 再加上改写规则：`(正则, 替换)`，按顺序对前面几步的结果各做一次替换。
    /// 替换里可以用 `$1`、`${名字}` 引用分组。正则写错时返回原因。
    pub fn with_rewrites(mut self, rewrites: &[(String, String)]) -> Result<Self, String> {
        for (pattern, replace) in rewrites {
            let regex =
                Regex::new(pattern).map_err(|e| format!("标题改写规则 {pattern:?} 写错了：{e}"))?;
            self.rewrites.push((regex, replace.clone()));
        }
        Ok(self)
    }

    /// 规范化一个标题。去完只剩空白时返回 `None`。
    ///
    /// 依次去掉：一个应用名后缀，开头的 spinner / 盲文 / 圆点字符，计数徽标 `(3)`。
    /// 后缀要最先去：否则 `" — Zen Browser"` 这种标题开头的空格会先被当作装饰去掉，
    /// 后缀就匹配不上了。
    pub fn normalize(&self, raw: &str) -> Option<String> {
        let mut title = raw.trim_end();
        for suffix in &self.suffixes {
            if let Some(rest) = title.strip_suffix(suffix.as_str()) {
                title = rest;
                break;
            }
        }
        title = title.trim_start_matches(is_decoration);
        title = strip_badge(title).trim();
        let mut title = title.to_owned();
        for (regex, replace) in &self.rewrites {
            title = regex.replace(&title, replace.as_str()).trim().to_owned();
        }
        if title.is_empty() { None } else { Some(title) }
    }
}

/// 标题开头的装饰字符：空白、盲文点阵（终端 spinner）、圆形 spinner、圆点和星号类符号。
fn is_decoration(c: char) -> bool {
    c.is_whitespace()
        || ('\u{2800}'..='\u{28FF}').contains(&c)
        || matches!(
            c,
            '◐' | '◑'
                | '◒'
                | '◓'
                | '●'
                | '○'
                | '•'
                | '·'
                | '✳'
                | '✶'
                | '✻'
                | '✽'
                | '✢'
                | '*'
        )
}

/// 去掉开头的 `(数字)` 徽标。不是这个格式就原样返回。
fn strip_badge(title: &str) -> &str {
    let Some(rest) = title.strip_prefix('(') else {
        return title;
    };
    let Some((digits, rest)) = rest.split_once(')') else {
        return title;
    };
    if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
        rest
    } else {
        title
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(raw: &str) -> Option<String> {
        TitleRules::new(vec![
            " — Zen Browser".to_owned(),
            " - Google Chrome".to_owned(),
        ])
        .normalize(raw)
    }

    fn some(s: &str) -> Option<String> {
        Some(s.to_owned())
    }

    #[test]
    fn ghostty_spinner_is_removed() {
        assert_eq!(norm("◐ nvim ~/work"), some("nvim ~/work"));
        assert_eq!(norm("◑ nvim ~/work"), norm("◐ nvim ~/work"));
    }

    #[test]
    fn braille_and_star_spinners_are_removed() {
        assert_eq!(norm("⠋ cargo build"), some("cargo build"));
        assert_eq!(norm("✳ Claude Code"), some("Claude Code"));
    }

    #[test]
    fn badge_is_removed() {
        assert_eq!(norm("(3) Telegram"), some("Telegram"));
        assert_eq!(norm("(12) Telegram"), norm("(3) Telegram"));
        assert_eq!(
            norm("(3)Telegram"),
            some("Telegram"),
            "徽标后面没有空格也算"
        );
        // 标题后面还有括号
        assert_eq!(norm("(3) 群聊 (工作)"), some("群聊 (工作)"));
        // 不是纯数字的括号保留
        assert_eq!(norm("(draft) 周报"), some("(draft) 周报"));
        assert_eq!(norm("() x"), some("() x"));
    }

    #[test]
    fn app_suffix_is_removed() {
        assert_eq!(
            norm("Rust 程序设计语言 — Zen Browser"),
            some("Rust 程序设计语言")
        );
        assert_eq!(norm("Inbox - Google Chrome"), some("Inbox"));
        // 中间出现的不算后缀
        assert_eq!(norm("a — Zen Browser b"), some("a — Zen Browser b"));
        // 只去一个
        assert_eq!(
            norm("x - Google Chrome — Zen Browser"),
            some("x - Google Chrome")
        );
    }

    /// kakoune 默认把状态栏写进终端标题：光标一动标题就变。
    const KAKOUNE: (&str, &str) = (
        r"^(?<file>.+?) \d+:\d+ .* - \S+@\[[^\]]*\] - Kakoune$",
        "${file} - Kakoune",
    );

    fn kak(raw: &str) -> Option<String> {
        TitleRules::new(vec![])
            .with_rewrites(&[(KAKOUNE.0.to_owned(), KAKOUNE.1.to_owned())])
            .unwrap()
            .normalize(raw)
    }

    #[test]
    fn rewrites_drop_the_cursor_position() {
        let a = kak("/tmp/notes.txt 1:1  1 sel - client0@[466213] - Kakoune");
        let b = kak("/tmp/notes.txt 31:7 [+] insert 2 sels - client0@[466213] - Kakoune");
        assert_eq!(a, some("/tmp/notes.txt - Kakoune"));
        assert_eq!(a, b);
        assert_eq!(
            kak("~/x y.md 3:1  1 sel - client1@[9] - Kakoune"),
            some("~/x y.md - Kakoune")
        );
        // 不匹配的原样留着
        assert_eq!(kak("nvim ~/work"), some("nvim ~/work"));
    }

    #[test]
    fn a_bad_rewrite_is_reported() {
        let err = TitleRules::new(vec![])
            .with_rewrites(&[("(".to_owned(), String::new())])
            .unwrap_err();
        assert!(err.contains("\"(\""), "{err}");
    }

    #[test]
    fn everything_combined_and_empty_results() {
        assert_eq!(norm("● (2) 收件箱 — Zen Browser  "), some("收件箱"));
        assert_eq!(norm(""), None);
        assert_eq!(norm("  ◐  "), None);
        assert_eq!(norm(" — Zen Browser"), None);
        assert_eq!(norm("(7)"), None);
    }
}
