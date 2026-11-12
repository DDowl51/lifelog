//! `privacy` 的纯逻辑：入库之前排除和脱敏。
//!
//! 两种规则：
//!
//! - **排除**：命中的活动保留时长和应用，标题和 `payload` 里的文字都换成 [`REDACTED`]。
//!   用在密码管理器、浏览器隐私窗口这类「在用它」可以记、「在看什么」不能记的地方。
//! - **密钥**：在标题和 `payload` 的文字里找像密钥的片段，只换掉那一段。
//!
//! 被改动过的活动 `redacted` 置为 `true`。

use activity_contracts::{Activity, payload_keys};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 被换掉的文字变成这个。
pub const REDACTED: &str = "[redacted]";

/// 一条排除规则。写了的条件都满足才算命中；一个条件都不写是配置错误。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExcludeSpec {
    /// 来源，完全相等。
    #[serde(default)]
    pub source: Option<String>,
    /// 应用标识，正则。
    #[serde(default)]
    pub app: Option<String>,
    /// 标题，正则。
    #[serde(default)]
    pub title: Option<String>,
    /// 网址的域名：等于它，或是它的子域名。
    #[serde(default)]
    pub domain: Option<String>,
}

/// 内置的排除规则：密码管理器、浏览器隐私窗口。
pub fn builtin_excludes() -> Vec<ExcludeSpec> {
    vec![
        ExcludeSpec {
            // 密码管理器；输入密码的弹窗（标题里常带着要解密的文件路径和命令行）
            app: Some("(?i)keepassxc|bitwarden|1password|gcr-prompter|pinentry".to_owned()),
            ..ExcludeSpec::default()
        },
        ExcludeSpec {
            // 浏览器隐私窗口的标题后缀；中文界面的也算上
            title: Some("(?i)private browsing|incognito|隐私浏览|无痕".to_owned()),
            ..ExcludeSpec::default()
        },
    ]
}

/// 默认屏蔽的命令：命令行的密码管理器，命令本身就暴露了密码库里有哪些条目。
pub const DEFAULT_COMMANDS: [&str; 3] = ["pass", "gopass", "passage"];

/// 把「执行了这些命令」变成一条排除规则：命令开头，或者在管道、`;`、`&&`、`$( )`、
/// 反引号之后调用的都算（前面可以有 `sudo`）。空列表是 `None`（不屏蔽）。
pub fn command_exclude(commands: &[String]) -> Result<Option<ExcludeSpec>, String> {
    if commands.is_empty() {
        return Ok(None);
    }
    for c in commands {
        if c.trim().is_empty() || c.chars().any(char::is_whitespace) {
            return Err(format!(
                "exclude_commands 里的 `{c}` 要是一个命令名（不能有空格）"
            ));
        }
    }
    let names: Vec<String> = commands.iter().map(|c| regex::escape(c)).collect();
    Ok(Some(ExcludeSpec {
        source: Some("shell".to_owned()),
        title: Some(format!(
            r"(^|[|;&(`])\s*(sudo\s+)?({})(\s|$|\))",
            names.join("|")
        )),
        ..ExcludeSpec::default()
    }))
}

/// 内置的密钥规则。有名为 `secret` 的分组时只换掉那一组，否则换掉整段。
///
/// 词边界用 `(?-u:\b)`（只认 ASCII 字母数字）：中文紧挨着密钥时也算边界。
pub fn builtin_secrets() -> Vec<String> {
    [
        // OpenAI、Anthropic 等
        r"(?-u:\b)sk-[A-Za-z0-9_-]{20,}",
        // GitHub
        r"(?-u:\b)gh[pousr]_[A-Za-z0-9]{36,}",
        r"(?-u:\b)github_pat_[A-Za-z0-9_]{22,}",
        // GitLab
        r"(?-u:\b)glpat-[A-Za-z0-9_-]{20,}",
        // Slack
        r"(?-u:\b)xox[abprs]-[A-Za-z0-9-]{10,}",
        // AWS 访问密钥
        r"(?-u:\b)AKIA[0-9A-Z]{16}(?-u:\b)",
        // Google API key
        r"(?-u:\b)AIza[0-9A-Za-z_-]{35}",
        // JWT
        r"(?-u:\b)eyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
        // 私钥文件头
        r"-----BEGIN [A-Z ]*PRIVATE KEY-----",
        // Authorization: Bearer …
        r"(?i)(?-u:\b)bearer\s+(?<secret>[A-Za-z0-9._~+/-]{16,}=*)",
        // password=…、token: …、api_key=…；前面可以带前缀：access_token、id_token、ba_token
        r#"(?i)(?-u:\b)(?:[a-z0-9]+[_-])?(?:password|passwd|pwd|secret|token|api[_-]?key|access[_-]?key)\s*[=:]\s*(?<secret>[^\s&'";]+)"#,
        // --password …、--token=…
        r"(?i)--(?:password|passwd|token|api-key|secret)[ =](?<secret>\S+)",
        // 网址里的用户名和密码：https://user:pass@host
        r"://(?<secret>[^/\s:@]+:[^/\s@]+)@",
    ]
    .map(str::to_owned)
    .to_vec()
}

struct Exclude {
    source: Option<String>,
    app: Option<Regex>,
    title: Option<Regex>,
    domain: Option<String>,
}

impl Exclude {
    fn matches(&self, activity: &Activity) -> bool {
        let source = self.source.as_ref().is_none_or(|s| *s == activity.source);
        let app = self
            .app
            .as_ref()
            .is_none_or(|re| activity.app.as_deref().is_some_and(|a| re.is_match(a)));
        let title = self
            .title
            .as_ref()
            .is_none_or(|re| activity.title.as_deref().is_some_and(|t| re.is_match(t)));
        let domain = self.domain.as_ref().is_none_or(|d| {
            activity
                .payload
                .get(payload_keys::URL)
                .and_then(Value::as_str)
                .and_then(host)
                .is_some_and(|h| h == *d || h.ends_with(&format!(".{d}")))
        });
        source && app && title && domain
    }
}

/// 网址里的域名，小写。不是 `scheme://host…` 的样子就是 `None`。
fn host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    // 去掉端口；IPv6 地址在方括号里
    let host = match host.strip_prefix('[') {
        Some(v6) => v6.split_once(']').map_or(v6, |(h, _)| h),
        None => host.split_once(':').map_or(host, |(h, _)| h),
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

fn compile(pattern: &str, what: &str) -> Result<Regex, String> {
    Regex::new(pattern).map_err(|e| format!("{what} `{pattern}` 不是合法的正则：{e}"))
}

/// 编译好的规则。
pub struct Rules {
    excludes: Vec<Exclude>,
    secrets: Vec<Regex>,
}

impl Rules {
    pub fn new(excludes: &[ExcludeSpec], secrets: &[String]) -> Result<Self, String> {
        let excludes = excludes
            .iter()
            .enumerate()
            .map(|(i, spec)| {
                if spec.source.is_none()
                    && spec.app.is_none()
                    && spec.title.is_none()
                    && spec.domain.is_none()
                {
                    return Err(format!(
                        "第 {} 条排除规则一个条件都没写，会排除所有活动",
                        i + 1
                    ));
                }
                Ok(Exclude {
                    source: spec.source.clone(),
                    app: spec
                        .app
                        .as_deref()
                        .map(|p| compile(p, "排除规则的 app"))
                        .transpose()?,
                    title: spec
                        .title
                        .as_deref()
                        .map(|p| compile(p, "排除规则的 title"))
                        .transpose()?,
                    domain: spec
                        .domain
                        .as_deref()
                        .map(|d| d.trim_start_matches('.').to_ascii_lowercase()),
                })
            })
            .collect::<Result<_, String>>()?;
        let secrets = secrets
            .iter()
            .map(|p| compile(p, "密钥规则"))
            .collect::<Result<_, String>>()?;
        Ok(Self { excludes, secrets })
    }

    /// 加工一条活动。
    pub fn apply(&self, mut activity: Activity) -> Activity {
        if self.excludes.iter().any(|e| e.matches(&activity)) {
            if activity.title.is_some() {
                activity.title = Some(REDACTED.to_owned());
            }
            for value in activity.payload.values_mut() {
                visit_strings(value, &mut |s| *s = REDACTED.to_owned());
            }
            activity.redacted = true;
            return activity;
        }

        let mut changed = false;
        let mut mask = |text: &mut String| {
            let masked = self.mask(text);
            if masked != *text {
                *text = masked;
                changed = true;
            }
        };
        if let Some(title) = activity.title.as_mut() {
            mask(title);
        }
        for value in activity.payload.values_mut() {
            visit_strings(value, &mut mask);
        }
        activity.redacted |= changed;
        activity
    }

    /// 把文字里像密钥的片段换掉。
    pub fn mask(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for re in &self.secrets {
            if !re.is_match(&text) {
                continue;
            }
            text = re
                .replace_all(&text, |caps: &regex::Captures<'_>| {
                    let Some(whole) = caps.get(0) else {
                        return String::new();
                    };
                    match caps.name("secret") {
                        Some(secret) => {
                            let s = whole.as_str();
                            let (from, to) =
                                (secret.start() - whole.start(), secret.end() - whole.start());
                            format!("{}{REDACTED}{}", &s[..from], &s[to..])
                        }
                        None => REDACTED.to_owned(),
                    }
                })
                .into_owned();
        }
        text
    }
}

/// 对 JSON 里每一个字符串（含嵌套的）执行 `f`。
fn visit_strings(value: &mut Value, f: &mut impl FnMut(&mut String)) {
    match value {
        Value::String(s) => f(s),
        Value::Array(items) => items.iter_mut().for_each(|v| visit_strings(v, f)),
        Value::Object(map) => map.values_mut().for_each(|v| visit_strings(v, f)),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::json;

    use super::*;

    fn builtin() -> Rules {
        Rules::new(&builtin_excludes(), &builtin_secrets()).unwrap()
    }

    fn window(app: &str, title: &str) -> Activity {
        Activity::new("focus", "window_focus", 0, 480)
            .with_end(60_000)
            .with_app(app)
            .with_title(title)
    }

    #[test]
    fn ordinary_activities_pass_untouched() {
        let a = window("com.mitchellh.ghostty", "~/w/p/lifelog")
            .with_payload("window_id", 3)
            .with_payload(payload_keys::CWD, "/home/me");
        assert_eq!(builtin().apply(a.clone()), a);
    }

    #[test]
    fn excluded_apps_keep_time_but_lose_text() {
        let a = window("org.keepassxc.KeePassXC", "银行 - KeePassXC")
            .with_payload("window_id", 3)
            .with_payload("nested", json!({ "a": ["x", 1] }));
        let out = builtin().apply(a.clone());
        assert_eq!(out.title.as_deref(), Some(REDACTED));
        assert_eq!(out.app, a.app);
        assert_eq!((out.ts_start, out.ts_end), (a.ts_start, a.ts_end));
        assert_eq!(out.payload["window_id"], json!(3), "数字留着");
        assert_eq!(out.payload["nested"], json!({ "a": [REDACTED, 1] }));
        assert!(out.redacted);
    }

    #[test]
    fn password_prompts_are_excluded() {
        let title = "[1689]@me (gpg2 -d --quiet /home/me/.password-store/x/token.gpg)";
        for app in ["gcr-prompter", "pinentry-qt"] {
            let out = builtin().apply(window(app, title));
            assert_eq!(out.title.as_deref(), Some(REDACTED), "{app}");
        }
    }

    fn with_commands(commands: &[&str]) -> Rules {
        let commands: Vec<String> = commands.iter().map(|c| (*c).to_owned()).collect();
        let mut excludes = builtin_excludes();
        excludes.extend(command_exclude(&commands).unwrap());
        Rules::new(&excludes, &builtin_secrets()).unwrap()
    }

    #[test]
    fn password_manager_commands_are_excluded() {
        let command = |text: &str| Activity::new("shell", "command", 0, 0).with_title(text);
        let builtin = || with_commands(&DEFAULT_COMMANDS);
        for text in [
            "pass show api/llm",
            "pass delete -r website/google.com",
            "  gopass ls",
            "pass",
            "sudo pass ls",
            "set -x KEY $(pass api/x)",
            "zbarimg -q --raw a.png | pass otp insert website/y",
            "cd x && pass git push",
        ] {
            let out = builtin().apply(command(text));
            assert_eq!(out.title.as_deref(), Some(REDACTED), "{text}");
        }
        for fine in [
            "passwd",
            "cargo run -- pass",
            "passenger start",
            "gpass list",
            "echo bypass",
        ] {
            assert_eq!(
                builtin().apply(command(fine)).title.as_deref(),
                Some(fine),
                "{fine}"
            );
        }
        // 只针对命令：窗口标题里有 pass 不算
        let window = Activity::new("focus", "window_focus", 0, 0).with_title("pass show x");
        assert_eq!(
            builtin().apply(window).title.as_deref(),
            Some("pass show x")
        );
    }

    #[test]
    fn private_browser_windows_are_excluded() {
        for title in [
            "Example — Mozilla Firefox Private Browsing",
            "New Tab - Chromium (Incognito)",
            "某页面 — Mozilla Firefox 隐私浏览",
        ] {
            let out = builtin().apply(window("firefox", title));
            assert_eq!(out.title.as_deref(), Some(REDACTED), "{title}");
        }
    }

    #[test]
    fn all_conditions_of_a_rule_must_hold() {
        let rules = Rules::new(
            &[ExcludeSpec {
                source: Some("browsing".to_owned()),
                domain: Some("bank.example".to_owned()),
                ..ExcludeSpec::default()
            }],
            &[],
        )
        .unwrap();
        let visit = |source: &str, url: &str| {
            Activity::new(source, "visit", 0, 0)
                .with_title("余额")
                .with_payload(payload_keys::URL, url)
        };
        assert!(
            rules
                .apply(visit("browsing", "https://bank.example/home"))
                .redacted
        );
        assert!(
            rules
                .apply(visit("browsing", "https://me@WWW.Bank.Example:443/x?y"))
                .redacted,
            "子域名、大小写、端口、用户名都不影响"
        );
        assert!(
            !rules
                .apply(visit("browsing", "https://notbank.example/"))
                .redacted
        );
        assert!(
            !rules
                .apply(visit("other", "https://bank.example/"))
                .redacted
        );
        assert!(!rules.apply(visit("browsing", "not a url")).redacted);
    }

    #[test]
    fn secrets_are_masked_in_place() {
        let rules = builtin();
        let cases = [
            (
                "export OPENAI=sk-proj-abcdefghijklmnopqrstuvwx",
                "export OPENAI=[redacted]",
            ),
            ("密钥sk-abcdefghijklmnopqrstuvwx在这", "密钥[redacted]在这"),
            (
                "git push https://me:hunter2@github.com/x",
                "git push https://[redacted]@github.com/x",
            ),
            (
                "curl -H 'Authorization: Bearer abcdefghijklmnop1234'",
                "curl -H 'Authorization: Bearer [redacted]'",
            ),
            (
                "mysql --password=hunter2 db",
                "mysql --password=[redacted] db",
            ),
            ("login password: hunter2", "login password: [redacted]"),
            (
                "https://x/cb?access_token=abc123&state=1&ba_token=BA-7X71",
                "https://x/cb?access_token=[redacted]&state=1&ba_token=[redacted]",
            ),
            (
                "AKIAABCDEFGHIJKLMNOP and ghp_abcdefghijklmnopqrstuvwxyz0123456789",
                "[redacted] and [redacted]",
            ),
            ("-----BEGIN OPENSSH PRIVATE KEY----- …", "[redacted] …"),
        ];
        for (input, expected) in cases {
            assert_eq!(rules.mask(input), expected, "{input}");
        }
        // 长得像但不是的不动
        for fine in [
            "task-runner",
            "desk-abcdefghijklmnopqrstuvwxyz",
            "tokens 很多",
        ] {
            assert_eq!(rules.mask(fine), fine);
        }
    }

    #[test]
    fn masking_marks_the_activity_and_reaches_payload() {
        let a = Activity::new("shell", "command", 0, 0)
            .with_title("echo sk-abcdefghijklmnopqrstuvwx")
            .with_payload(payload_keys::URL, "https://me:pw@host/");
        let out = builtin().apply(a);
        assert_eq!(out.title.as_deref(), Some("echo [redacted]"));
        assert_eq!(
            out.payload[payload_keys::URL],
            json!("https://[redacted]@host/")
        );
        assert!(out.redacted);
        // 再加工一遍结果不变：同一个时段会反复提交
        let again = builtin().apply(out.clone());
        assert_eq!(again, out);
    }

    #[test]
    fn bad_rules_are_reported() {
        let err = Rules::new(&[ExcludeSpec::default()], &[]).err().unwrap();
        assert!(err.contains("第 1 条"), "{err}");
        let err = Rules::new(&[], &["(".to_owned()]).err().unwrap();
        assert!(err.contains("密钥规则 `(`"), "{err}");
    }

    #[test]
    fn the_command_list_is_configurable() {
        let command = |text: &str| Activity::new("shell", "command", 0, 0).with_title(text);
        let rules = with_commands(&["bw", "op"]);
        assert!(rules.apply(command("bw get item github")).redacted);
        assert!(
            rules
                .apply(command("export T=$(op read op://x/y)"))
                .redacted
        );
        assert!(
            !rules.apply(command("pass show x")).redacted,
            "没列的不屏蔽"
        );
        assert!(
            !rules.apply(command("opam install dune")).redacted,
            "只认整个命令名"
        );
        assert!(
            !with_commands(&[]).apply(command("pass show x")).redacted,
            "空列表 = 不屏蔽"
        );
        assert!(
            command_exclude(&["c++".to_owned()]).unwrap().is_some(),
            "特殊字符会转义"
        );
        assert!(command_exclude(&["pass show".to_owned()]).is_err());
    }
}
