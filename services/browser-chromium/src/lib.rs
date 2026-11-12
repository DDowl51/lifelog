//! 系统服务 `browser-chromium`：读 Chrome 系浏览器（Chrome、Chromium……）的访问历史。
//!
//! profile 列表来自配置目录里的 `Local State`；每个 profile 目录下有一个 `History`（SQLite）。
//! 浏览器开着时库被锁住，所以每次读都先复制到临时目录，读副本。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use browser_contract::{BROWSER_ADAPTERS, BrowserAdapter, HistorySource, Visit, VisitPage};
use kernel_api::{
    AppError, Component, ConfigSection, Host, Kind, Manifest, async_trait, register_component,
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 一个浏览器的安装：名字 + 配置目录（放 `Local State` 的那个）。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Install {
    name: String,
    dir: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
struct Config {
    /// 不存在的目录跳过。
    installs: Vec<Install>,
    /// 不读的 profile：写 profile 的显示名或目录名（`Default`、`Profile 3`）。
    skip_profiles: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        let install = |name: &str, dir: &str| Install {
            name: name.to_owned(),
            dir: dir.to_owned(),
        };
        Self {
            installs: vec![
                install("chrome", "~/.config/google-chrome"),
                install("chromium", "~/.config/chromium"),
            ],
            skip_profiles: Vec::new(),
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

/// `Local State` 里的 profile：（目录名，显示名）。
fn profiles(dir: &Path) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(dir.join("Local State")) else {
        return Vec::new();
    };
    let Ok(state) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let Some(cache) = state
        .pointer("/profile/info_cache")
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = cache
        .iter()
        .map(|(key, info)| {
            let name = info
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(key)
                .to_owned();
            (key.clone(), name)
        })
        .collect();
    out.sort();
    out
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

/// Chrome 的时间是从 1601-01-01 起的微秒。
const EPOCH_1601_MS: i64 = 11_644_473_600_000;

/// 子框架里的导航（3、4）不算「访问了一个网页」。
const SELECT: &str = "
    SELECT v.id, v.visit_time, u.url, u.title
    FROM visits v JOIN urls u ON u.id = v.url
    WHERE v.id > ?1 AND (v.transition & 255) NOT IN (3, 4)
    ORDER BY v.id
    LIMIT ?2";

fn read(db: &Path, after: i64, limit: u32) -> Result<Vec<(i64, Visit)>, AppError> {
    let (_dir, copy) = snapshot(db)?;
    let fail = |e: rusqlite::Error| AppError::unavailable(format!("读 {} 失败：{e}", db.display()));
    let conn = Connection::open(&copy).map_err(fail)?;
    let mut stmt = conn.prepare(SELECT).map_err(fail)?;
    stmt.query_map(params![after, limit], |r| {
        let id: i64 = r.get(0)?;
        let micros: i64 = r.get(1)?;
        Ok((
            id,
            Visit {
                id: id.to_string(),
                at: micros / 1_000 - EPOCH_1601_MS,
                url: r.get(2)?,
                title: r.get::<_, Option<String>>(3)?.filter(|t| !t.is_empty()),
            },
        ))
    })
    .map_err(fail)?
    .collect::<Result<Vec<_>, _>>()
    .map_err(fail)
}

struct Chromium {
    installs: Vec<(String, PathBuf)>,
    skip: Vec<String>,
}

impl Chromium {
    /// （来源，History 的位置）。
    fn scan(&self) -> Vec<(HistorySource, PathBuf)> {
        let mut out = Vec::new();
        for (browser, dir) in &self.installs {
            for (key, name) in profiles(dir) {
                if self.skip.iter().any(|s| *s == key || *s == name) {
                    continue;
                }
                let db = dir.join(&key).join("History");
                if db.exists() {
                    out.push((
                        HistorySource {
                            // 目录名不会变，显示名可以改：id 用目录名
                            id: format!("{browser}/{key}"),
                            browser: browser.clone(),
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
impl BrowserAdapter for Chromium {
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

pub struct BrowserChromium;

impl Component for BrowserChromium {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "browser-chromium",
            kind: Kind::Service,
            summary: "读 Chrome 系浏览器的访问历史",
            provides: vec![],
            requires: vec![BROWSER_ADAPTERS.id()],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let config: Config = config.parse()?;
        let installs = config
            .installs
            .into_iter()
            .map(|i| Ok((i.name, expand(&i.dir)?)))
            .collect::<Result<_, AppError>>()?;
        host.contribute(
            &BROWSER_ADAPTERS,
            Arc::new(Chromium {
                installs,
                skip: config.skip_profiles,
            }),
        )
    }
}

register_component!(BrowserChromium);
