//! 从读出来的行算出一页：哪些给出去、游标挪到哪。纯逻辑。
//!
//! atuin 在命令开始时就写一行（耗时、退出码是 -1），结束时再补上。
//! 还在执行的命令先不给；游标停在它前面，下次连它带它后面的一起重读。
//! 等太久（超过 `grace_ns`）的就不等了：可能是 shell 被杀了，也可能是从别的历史导入、本来就没有耗时。
//!
//! 游标是（时间戳，id）：从别的历史导入的命令常常共用同一个时间戳，只记时间戳会在分页时丢掉一部分。

use shell_contract::{HistoryPage, ShellCommand};

/// atuin 表里的一行。时间是纳秒。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub timestamp: i64,
    pub duration: i64,
    pub exit: i64,
    pub command: String,
    pub cwd: String,
    pub session: String,
}

const NS_PER_MS: i64 = 1_000_000;

/// 游标：读过的最后一行的（时间戳，id）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cursor {
    pub timestamp: i64,
    pub id: String,
}

impl Cursor {
    /// 从头读。
    pub fn start() -> Self {
        Self {
            timestamp: i64::MIN,
            id: String::new(),
        }
    }

    /// `时间戳:id`。只有时间戳的也认，id 当作空：同一时刻的会重读，按 id 去重。
    pub fn parse(text: &str) -> Option<Self> {
        let (ts, id) = text.split_once(':').unwrap_or((text, ""));
        Some(Self {
            timestamp: ts.parse().ok()?,
            id: id.to_owned(),
        })
    }

    fn of(row: &Row) -> Self {
        Self {
            timestamp: row.timestamp,
            id: row.id.clone(),
        }
    }
}

impl std::fmt::Display for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.timestamp, self.id)
    }
}

/// zsh 的扩展历史格式 `: 开始秒:耗时秒;命令`。atuin 按别的格式导入时会把它原样当成命令，
/// 时间记成导入那一刻。认出来就取出真实的开始时刻、耗时和命令。
fn zsh_extended(command: &str) -> Option<(i64, i64, &str)> {
    let rest = command.strip_prefix(": ")?;
    let (start, rest) = rest.split_once(':')?;
    let (elapsed, command) = rest.split_once(';')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(start) || !digits(elapsed) {
        return None;
    }
    Some((start.parse().ok()?, elapsed.parse().ok()?, command))
}

fn command(row: &Row) -> ShellCommand {
    let finished = row.duration >= 0;
    let mut started_at = row.timestamp.div_euclid(NS_PER_MS);
    let mut duration_ms = finished.then_some(row.duration / NS_PER_MS);
    let mut text = row.command.as_str();
    if let Some((start_s, elapsed_s, command)) = zsh_extended(text) {
        started_at = start_s.saturating_mul(1_000);
        duration_ms = Some(elapsed_s.saturating_mul(1_000));
        text = command;
    }
    ShellCommand {
        id: row.id.clone(),
        started_at,
        duration_ms,
        exit: finished.then(|| i32::try_from(row.exit).ok()).flatten(),
        command: text.to_owned(),
        // 导入的历史里 cwd 可能是 "unknown" 之类
        cwd: row.cwd.starts_with('/').then(|| row.cwd.clone()),
        session: (!row.session.is_empty()).then(|| row.session.clone()),
    }
}

/// `rows` 是（时间戳，id）大于 `after` 的行，按（时间戳，id）升序，最多 `limit` 条；
/// `now` 是现在（纳秒）。
pub fn page(rows: &[Row], after: &Cursor, limit: u32, now: i64, grace_ns: i64) -> HistoryPage {
    // 导入的历史也没有耗时，但目录是 "unknown"：执行中的命令开始时就记下了绝对路径
    let running = |row: &Row| {
        row.duration < 0 && row.cwd.starts_with('/') && now.saturating_sub(row.timestamp) < grace_ns
    };
    let next = match rows.iter().position(running) {
        // 停在第一个还在执行的命令前面
        Some(0) => after.clone(),
        Some(k) => Cursor::of(&rows[k - 1]),
        None => rows.last().map_or_else(|| after.clone(), Cursor::of),
    };
    let full = rows.len() >= limit as usize;
    HistoryPage {
        commands: rows.iter().filter(|r| !running(r)).map(command).collect(),
        // 游标没动时再读也是同一页，不提示还有
        more: full && next > *after,
        next: next.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i64 = 1_000_000_000;
    const GRACE: i64 = 3600 * S;

    fn row(id: &str, secs: i64, duration: i64) -> Row {
        Row {
            id: id.to_owned(),
            timestamp: secs * S,
            duration,
            exit: if duration < 0 { -1 } else { 0 },
            command: format!("cmd {id}"),
            cwd: "/home/me".to_owned(),
            session: "s".to_owned(),
        }
    }

    fn ids(page: &HistoryPage) -> Vec<&str> {
        page.commands.iter().map(|c| c.id.as_str()).collect()
    }

    fn start() -> Cursor {
        Cursor::start()
    }

    #[test]
    fn finished_rows_advance_the_cursor() {
        let rows = [row("a", 10, 2 * S), row("b", 20, 0)];
        let p = page(&rows, &start(), 10, 100 * S, GRACE);
        assert_eq!(ids(&p), ["a", "b"]);
        assert_eq!(p.next, format!("{}:b", 20 * S));
        assert!(!p.more);
        assert_eq!(p.commands[0].started_at, 10_000);
        assert_eq!(p.commands[0].duration_ms, Some(2_000));
        assert_eq!(p.commands[0].exit, Some(0));
    }

    #[test]
    fn a_running_command_holds_the_cursor_back() {
        let rows = [row("a", 10, S), row("running", 20, -1), row("c", 30, S)];
        let p = page(&rows, &start(), 10, 100 * S, GRACE);
        assert_eq!(ids(&p), ["a", "c"], "c 先给出去，下次还会再给一次");
        assert_eq!(p.next, format!("{}:a", 10 * S), "停在执行中的那条前面");
    }

    #[test]
    fn stale_running_commands_are_given_up_on() {
        let mut imported = row("old", 10, -1);
        imported.cwd = "unknown".to_owned();
        imported.session = String::new();
        let p = page(&[imported], &start(), 10, 10 * S + GRACE, GRACE);
        assert_eq!(ids(&p), ["old"]);
        let c = &p.commands[0];
        assert_eq!(
            (c.duration_ms, c.exit, &c.cwd, &c.session),
            (None, None, &None, &None)
        );
        assert_eq!(p.next, format!("{}:old", 10 * S));
    }

    #[test]
    fn imported_rows_are_not_waited_for() {
        let mut imported = row("imported", 10, -1);
        imported.cwd = "unknown".to_owned();
        let p = page(&[imported], &start(), 10, 11 * S, GRACE);
        assert_eq!(ids(&p), ["imported"]);
        assert_eq!(p.commands[0].duration_ms, None);
    }

    #[test]
    fn a_full_page_stuck_behind_a_running_command_does_not_loop() {
        let rows = [row("running", 10, -1), row("b", 20, S)];
        let after = Cursor::parse("9:z").unwrap();
        let p = page(&rows, &after, 2, 30 * S, GRACE);
        assert_eq!(p.next, "9:z");
        assert!(!p.more, "游标没动，不能让读的一方原地反复读");
        let p = page(
            &[row("a", 1, S), row("b", 2, S)],
            &start(),
            2,
            30 * S,
            GRACE,
        );
        assert!(p.more);
    }

    #[test]
    fn cursors_order_by_time_then_id_and_accept_a_bare_timestamp() {
        let a = Cursor::parse("5:b").unwrap();
        assert!(Cursor::parse("5:c").unwrap() > a);
        assert!(Cursor::parse("6:a").unwrap() > a);
        assert_eq!(
            Cursor::parse("5").unwrap(),
            Cursor {
                timestamp: 5,
                id: String::new()
            }
        );
        assert_eq!(Cursor::parse("x:y"), None);
        assert_eq!(a.to_string(), "5:b");
    }

    #[test]
    fn zsh_extended_history_lines_get_their_real_time() {
        let mut r = row("z", 100, -1);
        r.command = ": 1766908283:3;cargo build".to_owned();
        let p = page(&[r], &start(), 10, 100 * S + GRACE, GRACE);
        let c = &p.commands[0];
        assert_eq!(c.command, "cargo build");
        assert_eq!(c.started_at, 1_766_908_283_000);
        assert_eq!(c.duration_ms, Some(3_000));
        for plain in [": not:a;history", ":1:2;x", "echo : 1:2;x"] {
            assert_eq!(zsh_extended(plain), None, "{plain}");
        }
    }
}
