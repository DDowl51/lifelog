//! 系统服务 `browser-qutebrowser`：读 qutebrowser 的访问历史。
//!
//! 默认实例的数据在 `~/.local/share/qutebrowser`；用 `--basedir` 开的独立实例
//! （比如每个项目一个）放在某个目录下，每个子目录一个实例，库在 `<子目录>/data/history.sqlite`。
//! 子目录名就是 profile 名。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use browser_contract::{BROWSER_ADAPTERS, BrowserAdapter, HistorySource, Visit, VisitPage};
use kernel_api::{
    AppError, Component, ConfigSection, Host, Kind, Manifest, async_trait, register_component,
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

const BROWSER: &str = "qutebrowser";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
struct Config {
    /// 默认实例的数据目录。不存在就跳过。
    data_dir: String,
    /// 放独立实例的目录：每个子目录是一个实例（`--basedir`）。不存在的跳过。
    basedirs: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_dir: "~/.local/share/qutebrowser".to_owned(),
            basedirs: vec!["~/.local/share/qutebrowser-profiles".to_owned()],
        }
    }
}

fn expand(path: &str) -> Result<PathBuf, AppError> {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(rest))
            .ok_or_else(|| AppError::config(format!("`{path}` 用了 ~，但环境里没有 HOME"))),
        None => Ok(PathBuf::from(path)),
    }
}

/// 把库和它的 `-wal` 复制到临时目录。返回的目录要活到读完。
fn snapshot(db: &Path) -> Result<(tempfile::TempDir, PathBuf), AppError> {
    let fail =
        |e: std::io::Error| AppError::unavailable(format!("复制 {} 失败：{e}", db.display()));
    let dir = tempfile::tempdir().map_err(fail)?;
    let copy = dir.path().join("history.sqlite");
    std::fs::copy(db, &copy).map_err(fail)?;
    let wal = PathBuf::from(format!("{}-wal", db.display()));
    if wal.exists() {
        std::fs::copy(&wal, dir.path().join("history.sqlite-wal")).map_err(fail)?;
    }
    Ok((dir, copy))
}

/// 重定向不算「访问了一个网页」。
const SELECT: &str = "
    SELECT rowid, atime, url, title FROM History
    WHERE rowid > ?1 AND redirect = 0
    ORDER BY rowid
    LIMIT ?2";

fn read(db: &Path, after: i64, limit: u32) -> Result<Vec<(i64, Visit)>, AppError> {
    let (_dir, copy) = snapshot(db)?;
    let fail = |e: rusqlite::Error| AppError::unavailable(format!("读 {} 失败：{e}", db.display()));
    let conn = Connection::open(&copy).map_err(fail)?;
    let mut stmt = conn.prepare(SELECT).map_err(fail)?;
    stmt.query_map(params![after, limit], |r| {
        let id: i64 = r.get(0)?;
        let secs: i64 = r.get(1)?;
        Ok((
            id,
            Visit {
                id: id.to_string(),
                at: secs.saturating_mul(1_000),
                url: r.get(2)?,
                title: r.get::<_, Option<String>>(3)?.filter(|t| !t.is_empty()),
            },
        ))
    })
    .map_err(fail)?
    .collect::<Result<Vec<_>, _>>()
    .map_err(fail)
}

struct Qutebrowser {
    data_dir: PathBuf,
    basedirs: Vec<PathBuf>,
}

impl Qutebrowser {
    /// （来源，history.sqlite 的位置）。
    fn scan(&self) -> Vec<(HistorySource, PathBuf)> {
        let mut out = Vec::new();
        let main = self.data_dir.join("history.sqlite");
        if main.exists() {
            out.push((
                HistorySource {
                    id: BROWSER.to_owned(),
                    browser: BROWSER.to_owned(),
                    profile: None,
                },
                main,
            ));
        }
        for base in &self.basedirs {
            let Ok(entries) = std::fs::read_dir(base) else {
                continue;
            };
            let mut names: Vec<String> = entries
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .collect();
            names.sort();
            for name in names {
                let db = base.join(&name).join("data/history.sqlite");
                if db.exists() {
                    out.push((
                        HistorySource {
                            id: format!("{BROWSER}/{name}"),
                            browser: BROWSER.to_owned(),
                            profile: Some(name),
                        },
                        db,
                    ));
                }
            }
        }
        out
    }
}

#[async_trait]
impl BrowserAdapter for Qutebrowser {
    async fn sources(&self) -> Result<Vec<HistorySource>, AppError> {
        Ok(self.scan().into_iter().map(|(s, _)| s).collect())
    }

    async fn read(
        &self,
        source: String,
        after: Option<String>,
        limit: u32,
    ) -> Result<VisitPage, AppError> {
        let Some((_, db)) = self.scan().into_iter().find(|(s, _)| s.id == source) else {
            return Err(AppError::not_found(format!("没有来源 `{source}`")));
        };
        let after = match after {
            Some(c) => c
                .parse::<i64>()
                .map_err(|_| AppError::validation(format!("看不懂的游标 `{c}`")))?,
            None => 0,
        };
        let rows = tokio::task::spawn_blocking(move || read(&db, after, limit))
            .await
            .map_err(|e| AppError::unavailable(format!("读浏览历史的线程崩了：{e}")))??;
        let next = rows.last().map_or(after, |(id, _)| *id);
        Ok(VisitPage {
            more: rows.len() >= limit as usize,
            visits: rows.into_iter().map(|(_, v)| v).collect(),
            next: next.to_string(),
        })
    }
}

pub struct BrowserQutebrowser;

impl Component for BrowserQutebrowser {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "browser-qutebrowser",
            kind: Kind::Service,
            summary: "读 qutebrowser 的访问历史",
            provides: vec![],
            requires: vec![BROWSER_ADAPTERS.id()],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let config: Config = config.parse()?;
        let adapter = Qutebrowser {
            data_dir: expand(&config.data_dir)?,
            basedirs: config
                .basedirs
                .iter()
                .map(|d| expand(d))
                .collect::<Result<_, AppError>>()?,
        };
        host.contribute(&BROWSER_ADAPTERS, Arc::new(adapter))
    }
}

register_component!(BrowserQutebrowser);
