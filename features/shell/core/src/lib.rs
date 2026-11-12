//! `shell` 的纯逻辑：把执行过的命令变成活动。

use activity_contracts::{Activity, payload_keys};
use shell_contract::ShellCommand;

pub const SOURCE: &str = "shell";
pub const KIND: &str = "command";

/// 多行命令在标题里用这个连起来，标题保持一行。
const NEWLINE: &str = " ↵ ";

/// 一条命令对应的活动。空命令是 `None`。
///
/// 标题是命令（一行）；工作目录放进约定的 `cwd` 键，`project` 靠它归类；
/// 去重键用提供者给的标识，同一条命令重复读到只存一条。
pub fn to_activity(command: &ShellCommand, tz_offset_min: i32) -> Option<Activity> {
    let title = command
        .command
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(NEWLINE);
    if title.is_empty() {
        return None;
    }
    let mut activity = Activity::new(SOURCE, KIND, command.started_at, tz_offset_min)
        .with_title(&title)
        .with_dedup_key(&format!("{SOURCE}:{}", command.id));
    if let Some(ms) = command.duration_ms {
        activity = activity.with_end(command.started_at + ms.max(0));
    }
    if let Some(cwd) = &command.cwd {
        activity = activity.with_payload(payload_keys::CWD, cwd.as_str());
    }
    if let Some(exit) = command.exit {
        activity = activity.with_payload("exit", exit);
    }
    if let Some(session) = &command.session {
        activity = activity.with_payload("session", session.as_str());
    }
    Some(activity)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::json;

    use super::*;

    fn command(text: &str) -> ShellCommand {
        ShellCommand {
            id: "01h".to_owned(),
            started_at: 10_000,
            duration_ms: Some(2_500),
            exit: Some(1),
            command: text.to_owned(),
            cwd: Some("/srv/projects/demo".to_owned()),
            session: Some("s1".to_owned()),
        }
    }

    #[test]
    fn a_command_becomes_a_timed_activity() {
        let a = to_activity(&command("cargo test"), 540).unwrap();
        assert_eq!((a.source.as_str(), a.kind.as_str()), ("shell", "command"));
        assert_eq!((a.ts_start, a.ts_end), (10_000, Some(12_500)));
        assert_eq!(a.tz_offset_min, 540);
        assert_eq!(a.title.as_deref(), Some("cargo test"));
        assert_eq!(a.dedup_key.as_deref(), Some("shell:01h"));
        assert_eq!(a.payload[payload_keys::CWD], json!("/srv/projects/demo"));
        assert_eq!(a.payload["exit"], json!(1));
        assert_eq!(a.payload["session"], json!("s1"));
        assert_eq!(a.project, None, "归类是 project 的事");
    }

    #[test]
    fn unknown_fields_are_left_out() {
        let c = ShellCommand {
            duration_ms: None,
            exit: None,
            cwd: None,
            session: None,
            ..command("ls")
        };
        let a = to_activity(&c, 0).unwrap();
        assert_eq!(a.ts_end, None);
        assert!(a.payload.is_empty());
    }

    #[test]
    fn multi_line_commands_fit_on_one_line_and_blank_ones_are_skipped() {
        let a = to_activity(&command("for f in *\n    echo $f\nend\n"), 0).unwrap();
        assert_eq!(a.title.as_deref(), Some("for f in * ↵ echo $f ↵ end"));
        assert_eq!(to_activity(&command("  \n "), 0), None);
    }
}
