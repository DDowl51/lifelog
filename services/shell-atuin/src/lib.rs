//! 系统服务 `shell-atuin`：只读地读 atuin 的历史库，提供 `ShellHistory`。
//!
//! 每次读都重新只读地打开一次库：atuin 自己在写，不长期占着连接；
//! atuin 还没装、库还不存在时也不影响启动，只是读的时候报「不可用」。

mod page;

use std::path::PathBuf;
use std::sync::Arc;

use clock_contract::{CLOCK, Clock};
use kernel_api::{
    AppError, Component, ConfigSection, Host, Kind, Manifest, async_trait, register_component,
};
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use shell_contract::{HistoryPage, SHELL_HISTORY, ShellHistory};

use crate::page::{Cursor, Row, page};

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
struct Config {
    /// atuin 的历史库。不写时按 atuin 自己的规则找，用第一个存在的：
    /// `$ATUIN_HOME/data/history.db`、`~/.atuin/data/history.db`、
    /// `$XDG_DATA_HOME/atuin/history.db`、`~/.local/share/atuin/history.db`。
    path: Option<String>,
    /// atuin 会同步别的机器的历史。默认只读这台机器的；为真时全部都读。
    all_hosts: bool,
    /// 没有耗时的命令（还在执行）最多等多久，过了就当它结束了。
    running_grace_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            path: None,
            all_hosts: false,
            running_grace_secs: 6 * 3600,
        }
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// 可能的库文件位置，按先后。配置了 `path` 就只有它。
fn candidates(configured: Option<&str>) -> Result<Vec<PathBuf>, AppError> {
    if let Some(path) = configured {
        return match path.strip_prefix("~/") {
            Some(rest) => home()
                .map(|h| vec![h.join(rest)])
                .ok_or_else(|| AppError::config("path 用了 ~，但环境里没有 HOME")),
            None => Ok(vec![PathBuf::from(path)]),
        };
    }
    let absolute = |var: &str| {
        std::env::var_os(var)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    let home = home();
    let found: Vec<PathBuf> = [
        absolute("ATUIN_HOME").map(|p| p.join("data/history.db")),
        home.as_ref().map(|h| h.join(".atuin/data/history.db")),
        absolute("XDG_DATA_HOME").map(|p| p.join("atuin/history.db")),
        home.as_ref()
            .map(|h| h.join(".local/share/atuin/history.db")),
    ]
    .into_iter()
    .flatten()
    .collect();
    if found.is_empty() {
        return Err(AppError::config("没有配置 path，环境里也没有 HOME"));
    }
    Ok(found)
}

/// 这台机器的主机名。atuin 记的是「主机名:用户名」。
fn this_host() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty())
}

const SELECT: &str = "
    SELECT id, timestamp, duration, exit, command, cwd, session FROM history
    WHERE (timestamp, id) > (?1, ?2) AND deleted_at IS NULL
      AND (?3 IS NULL OR hostname = ?3 OR hostname LIKE ?3 || ':%')
    ORDER BY timestamp, id
    LIMIT ?4";

fn read_rows(
    candidates: &[PathBuf],
    after: &Cursor,
    host: Option<&str>,
    limit: u32,
) -> Result<Vec<Row>, AppError> {
    let Some(path) = candidates.iter().find(|p| p.exists()) else {
        let tried: Vec<String> = candidates.iter().map(|p| p.display().to_string()).collect();
        return Err(AppError::unavailable(format!(
            "没找到 atuin 的历史库（找过 {}）。atuin 在 shell 里执行过第一条命令后才会建库；\
             库在别处的话，在 [components.shell-atuin] 里写 path",
            tried.join("、")
        )));
    };
    let fail = |e: rusqlite::Error| {
        AppError::unavailable(format!("读 atuin 的历史库 {} 失败：{e}", path.display()))
    };
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(fail)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(fail)?;
    let mut stmt = conn.prepare(SELECT).map_err(fail)?;
    let rows = stmt
        .query_map(params![after.timestamp, after.id, host, limit], |r| {
            Ok(Row {
                id: r.get(0)?,
                timestamp: r.get(1)?,
                duration: r.get(2)?,
                exit: r.get(3)?,
                command: r.get(4)?,
                cwd: r.get(5)?,
                session: r.get(6)?,
            })
        })
        .map_err(fail)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(fail)?;
    Ok(rows)
}

struct Atuin {
    candidates: Vec<PathBuf>,
    host: Option<String>,
    grace_ns: i64,
    clock: Arc<dyn Clock>,
}

#[async_trait]
impl ShellHistory for Atuin {
    async fn read(&self, after: Option<String>, limit: u32) -> Result<HistoryPage, AppError> {
        let after = match after {
            Some(cursor) => Cursor::parse(&cursor)
                .ok_or_else(|| AppError::validation(format!("看不懂的游标 `{cursor}`")))?,
            None => Cursor::start(),
        };
        let now = self.clock.now().await?.utc_ms.saturating_mul(1_000_000);
        let (candidates, host, from) = (self.candidates.clone(), self.host.clone(), after.clone());
        let rows = tokio::task::spawn_blocking(move || {
            read_rows(&candidates, &from, host.as_deref(), limit)
        })
        .await
        .map_err(|e| AppError::unavailable(format!("读 atuin 历史的线程崩了：{e}")))??;
        Ok(page(&rows, &after, limit, now, self.grace_ns))
    }
}

pub struct ShellAtuin;

impl Component for ShellAtuin {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "shell-atuin",
            kind: Kind::Service,
            summary: "只读地读 atuin 的历史库",
            provides: vec![SHELL_HISTORY.id()],
            requires: vec![CLOCK.id()],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let config: Config = config.parse()?;
        let atuin = Atuin {
            candidates: candidates(config.path.as_deref())?,
            host: if config.all_hosts { None } else { this_host() },
            grace_ns: i64::try_from(config.running_grace_secs)
                .unwrap_or(i64::MAX / 1_000_000_000)
                .saturating_mul(1_000_000_000),
            clock: host.resolve(&CLOCK)?,
        };
        host.provide(&SHELL_HISTORY, Arc::new(atuin))
    }
}

register_component!(ShellAtuin);
