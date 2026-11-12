//! `browsing` 的纯逻辑：把浏览器的访问变成活动。

use activity_contracts::{Activity, payload_keys};
use browser_contract::{HistorySource, Visit};

pub const SOURCE: &str = "browsing";
pub const KIND: &str = "visit";

/// 只收这些协议的网址。`about:`、扩展页、浏览器内部页不算「看了一个网页」。
fn worth_keeping(url: &str) -> bool {
    ["http://", "https://", "file://"]
        .iter()
        .any(|p| url.len() > p.len() && url[..p.len()].eq_ignore_ascii_case(p))
}

/// 一次访问对应的活动。不值得记的是 `None`。
///
/// 应用是浏览器名，标题是网页标题，网址放进约定的 `url` 键（隐私规则按它的域名排除）；
/// 去重键带上来源，不同 profile 里同一个编号不会撞。
pub fn to_activity(source: &HistorySource, visit: &Visit, tz_offset_min: i32) -> Option<Activity> {
    if !worth_keeping(&visit.url) {
        return None;
    }
    let mut activity = Activity::new(SOURCE, KIND, visit.at, tz_offset_min)
        .with_app(&source.browser)
        .with_payload(payload_keys::URL, visit.url.as_str())
        .with_dedup_key(&format!("{SOURCE}:{}:{}", source.id, visit.id));
    if let Some(title) = visit
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        activity = activity.with_title(title);
    }
    if let Some(profile) = &source.profile {
        activity = activity.with_payload(payload_keys::PROFILE, profile.as_str());
    }
    Some(activity)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::json;

    use super::*;

    fn source(profile: Option<&str>) -> HistorySource {
        HistorySource {
            id: "qutebrowser/lifelog".to_owned(),
            browser: "qutebrowser".to_owned(),
            profile: profile.map(str::to_owned),
        }
    }

    fn visit(url: &str, title: Option<&str>) -> Visit {
        Visit {
            id: "42".to_owned(),
            at: 1_000,
            url: url.to_owned(),
            title: title.map(str::to_owned),
        }
    }

    #[test]
    fn a_visit_becomes_an_activity() {
        let a = to_activity(
            &source(Some("lifelog")),
            &visit("https://docs.rs/jiff", Some(" jiff - Rust ")),
            540,
        )
        .unwrap();
        assert_eq!((a.source.as_str(), a.kind.as_str()), ("browsing", "visit"));
        assert_eq!((a.ts_start, a.ts_end, a.tz_offset_min), (1_000, None, 540));
        assert_eq!(a.app.as_deref(), Some("qutebrowser"));
        assert_eq!(a.title.as_deref(), Some("jiff - Rust"));
        assert_eq!(a.payload[payload_keys::URL], json!("https://docs.rs/jiff"));
        assert_eq!(a.payload[payload_keys::PROFILE], json!("lifelog"));
        assert_eq!(
            a.dedup_key.as_deref(),
            Some("browsing:qutebrowser/lifelog:42")
        );
    }

    #[test]
    fn untitled_and_profileless_visits_leave_those_out() {
        let a = to_activity(&source(None), &visit("HTTP://x/", Some("  ")), 0).unwrap();
        assert_eq!(a.title, None);
        assert!(!a.payload.contains_key(payload_keys::PROFILE));
    }

    #[test]
    fn browser_internal_pages_are_skipped() {
        for url in [
            "about:newtab",
            "chrome://settings",
            "moz-extension://x/a.html",
            "qute://help",
            "data:text/html,x",
            "https://",
        ] {
            assert_eq!(
                to_activity(&source(None), &visit(url, None), 0),
                None,
                "{url}"
            );
        }
        assert!(to_activity(&source(None), &visit("file:///home/me/a.pdf", None), 0).is_some());
    }
}
