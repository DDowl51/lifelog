//! 系统服务 `tokenizer-cjk`：不用词典的分词。
//!
//! - 英文、数字、代码：按非字母数字切开，统一小写；`camelCase` 再拆一次（整词也留着）。
//!   所以 `snake_case`、`weather-station`、路径、网址都能按其中的词搜到。
//! - 中、日、韩文字：没有空格，也不带词典，切成相邻两字（`气象站` → `气象`、`象站`）。
//!   建索引时再加上每个单字，这样搜一个字也能命中；查询时两个字以上只用两字组合，少误中。

use std::sync::Arc;

use kernel_api::{
    AppError, Component, ConfigSection, Host, Kind, Manifest, NoConfig, async_trait,
    register_component,
};
use tokenizer_contract::{Purpose, TOKENIZER, Tokenizer};

/// 中日韩文字：统一表意文字（含扩展 A）、兼容表意文字、平假名、片假名、谚文。
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF
        | 0x3040..=0x309F | 0x30A0..=0x30FF | 0x31F0..=0x31FF
        | 0xAC00..=0xD7AF | 0x1100..=0x11FF | 0x20000..=0x2FA1F)
}

/// `camelCase`、`HTTPServer` 拆成几段；拆不开时返回空。
fn camel_parts(word: &str) -> Vec<String> {
    let chars: Vec<char> = word.chars().collect();
    let mut parts = Vec::new();
    let mut start = 0;
    for i in 1..chars.len() {
        let (prev, cur) = (chars[i - 1], chars[i]);
        let next_lower = chars.get(i + 1).is_some_and(|c| c.is_lowercase());
        let boundary = (prev.is_lowercase() && cur.is_uppercase())
            || (prev.is_uppercase() && cur.is_uppercase() && next_lower)
            || (prev.is_alphabetic() != cur.is_alphabetic());
        if boundary {
            parts.push(chars[start..i].iter().collect::<String>().to_lowercase());
            start = i;
        }
    }
    if start == 0 {
        return Vec::new();
    }
    parts.push(chars[start..].iter().collect::<String>().to_lowercase());
    parts
}

/// 切一段文字。
pub fn tokenize(text: &str, purpose: Purpose) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut cjk: Vec<char> = Vec::new();
    let flush_word = |word: &mut String, out: &mut Vec<String>| {
        if word.is_empty() {
            return;
        }
        let parts = camel_parts(word);
        out.push(word.to_lowercase());
        if purpose == Purpose::Index {
            out.extend(parts);
        }
        word.clear();
    };
    let flush_cjk = |run: &mut Vec<char>, out: &mut Vec<String>| {
        match run.len() {
            0 => {}
            1 => out.push(run[0].to_string()),
            _ => {
                for pair in run.windows(2) {
                    out.push(pair.iter().collect());
                }
                if purpose == Purpose::Index {
                    out.extend(run.iter().map(char::to_string));
                }
            }
        }
        run.clear();
    };
    for c in text.chars() {
        if is_cjk(c) {
            flush_word(&mut word, &mut out);
            cjk.push(c);
        } else if c.is_alphanumeric() {
            flush_cjk(&mut cjk, &mut out);
            word.push(c);
        } else {
            flush_word(&mut word, &mut out);
            flush_cjk(&mut cjk, &mut out);
        }
    }
    flush_word(&mut word, &mut out);
    flush_cjk(&mut cjk, &mut out);
    out
}

struct Cjk;

#[async_trait]
impl Tokenizer for Cjk {
    async fn tokenize(
        &self,
        texts: Vec<String>,
        purpose: Purpose,
    ) -> Result<Vec<Vec<String>>, AppError> {
        Ok(texts.iter().map(|t| tokenize(t, purpose)).collect())
    }
}

pub struct TokenizerCjk;

impl Component for TokenizerCjk {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<NoConfig>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "tokenizer-cjk",
            kind: Kind::Service,
            summary: "不用词典的分词：英文按词，中日韩按相邻两字",
            provides: vec![TOKENIZER.id()],
            requires: vec![],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        config.parse::<NoConfig>()?;
        host.provide(&TOKENIZER, Arc::new(Cjk))
    }
}

register_component!(TokenizerCjk);

#[cfg(test)]
mod tests {
    use super::*;

    fn index(text: &str) -> Vec<String> {
        tokenize(text, Purpose::Index)
    }

    fn query(text: &str) -> Vec<String> {
        tokenize(text, Purpose::Query)
    }

    #[test]
    fn words_and_code() {
        assert_eq!(
            query("cargo test --workspace"),
            ["cargo", "test", "workspace"]
        );
        assert_eq!(
            query("weather-station/src/main.rs"),
            ["weather", "station", "src", "main", "rs"]
        );
        assert_eq!(
            query("snake_case CamelCase"),
            ["snake", "case", "camelcase"]
        );
        assert_eq!(index("CamelCase"), ["camelcase", "camel", "case"]);
        assert_eq!(
            index("HTTPServer v2"),
            ["httpserver", "http", "server", "v2", "v", "2"]
        );
        assert_eq!(index("plain"), ["plain"], "拆不开的不重复");
    }

    #[test]
    fn cjk_in_pairs() {
        assert_eq!(query("气象站"), ["气象", "象站"]);
        assert_eq!(index("气象站"), ["气象", "象站", "气", "象", "站"]);
        assert_eq!(query("屏"), ["屏"]);
        assert_eq!(query("Rust 程序设计"), ["rust", "程序", "序设", "设计"]);
        assert_eq!(query("jiff文档"), ["jiff", "文档"], "中英紧挨着也分得开");
        assert_eq!(query("カタカナ"), ["カタ", "タカ", "カナ"]);
        assert_eq!(query("한국어"), ["한국", "국어"]);
        assert!(query(" — · ").is_empty());
    }

    #[test]
    fn every_query_word_is_in_the_index_of_the_same_text() {
        for text in ["气象站固件调试", "Wikipedia - qutebrowser", "屏 a"] {
            let index = index(text);
            for word in query(text) {
                assert!(index.contains(&word), "{text}: {word}");
            }
        }
    }
}
