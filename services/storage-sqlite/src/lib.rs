//! 系统服务 `storage-sqlite`：用一个 SQLite 文件提供 `Sql`。
//!
//! 它不认识任何表。有表的组件把迁移登记到 `MIGRATIONS`，这里在全部组件登记完之后执行。

mod db;
mod migrate;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use kernel_api::{
    AppError, Component, ConfigSection, Host, Kind, Manifest, async_trait, register_component,
};
use serde::{Deserialize, Serialize};
use sql_contract::{MIGRATIONS, Rows, SQL, Sql, Statement};

use crate::db::Db;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
struct Config {
    /// 数据库文件。`:memory:` 是内存数据库。
    /// 不写时用 `$XDG_DATA_HOME/lifelog/lifelog.db`（通常是 `~/.local/share/lifelog/lifelog.db`）。
    path: Option<String>,
    /// 只读打开：不建文件、不执行迁移、拒绝写入。给只查询的进程用。
    read_only: bool,
    /// 别的连接占着锁时最多等多久。
    busy_timeout_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            path: None,
            read_only: false,
            busy_timeout_ms: 5000,
        }
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn resolve_path(configured: Option<&str>) -> Result<String, AppError> {
    match configured {
        Some(":memory:") => Ok(":memory:".to_owned()),
        Some(path) => match path.strip_prefix("~/") {
            Some(rest) => home()
                .map(|h| h.join(rest).to_string_lossy().into_owned())
                .ok_or_else(|| AppError::config("path 用了 ~，但环境里没有 HOME")),
            None => Ok(path.to_owned()),
        },
        None => {
            let base = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .or_else(|| home().map(|h| h.join(".local/share")))
                .ok_or_else(|| AppError::config("没有配置 path，环境里也没有 HOME"))?;
            Ok(base
                .join("lifelog/lifelog.db")
                .to_string_lossy()
                .into_owned())
        }
    }
}

/// 只读进程里存储用不了的原因。
///
/// 只读进程（命令行）不该因为数据库还没建好就整个起不来：`--help` 和不查库的命令应该照常能用。
/// 所以只读模式下把问题记在这里，等到真的有人查询时再报。可写进程仍然在启动时就失败。
type Broken = Arc<OnceLock<AppError>>;

struct Service {
    db: Option<Db>,
    read_only: bool,
    broken: Broken,
}

impl Service {
    fn db(&self) -> Result<&Db, AppError> {
        match (self.broken.get(), &self.db) {
            (Some(e), _) => Err(e.clone()),
            (None, Some(db)) => Ok(db),
            (None, None) => Err(AppError::unavailable("数据库没有打开")),
        }
    }
}

#[async_trait]
impl Sql for Service {
    async fn query(&self, statement: Statement) -> Result<Rows, AppError> {
        self.db()?
            .run(move |conn| db::run_statement(conn, &statement, true))
            .await
    }

    async fn execute(&self, statements: Vec<Statement>) -> Result<Vec<Rows>, AppError> {
        if self.read_only {
            return Err(AppError::unauthorized("数据库是只读打开的，这个进程不能写"));
        }
        self.db()?
            .run(move |conn| {
                let tx = conn.transaction().map_err(db::sql_err)?;
                let mut results = Vec::with_capacity(statements.len());
                for statement in &statements {
                    results.push(db::run_statement(&tx, statement, false)?);
                }
                tx.commit().map_err(db::sql_err)?;
                Ok(results)
            })
            .await
    }
}

pub struct StorageSqlite;

impl Component for StorageSqlite {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "storage-sqlite",
            kind: Kind::Service,
            summary: "用一个 SQLite 文件执行 SQL，启动时执行各组件登记的迁移",
            provides: vec![SQL.id(), MIGRATIONS.id()],
            requires: vec![],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let config: Config = config.parse()?;
        let path = resolve_path(config.path.as_deref())?;
        let read_only = config.read_only;
        let broken = Broken::default();

        let db = match Db::open(&path, read_only, config.busy_timeout_ms) {
            Ok(db) => Some(db),
            Err(e) if read_only => {
                let _ = broken.set(e);
                None
            }
            Err(e) => return Err(e),
        };

        let sources = host.collector(&MIGRATIONS)?;
        if let Some(migrating) = db.clone() {
            let broken = broken.clone();
            host.on_ready(move || async move {
                let mut sets = Vec::new();
                for source in sources.all() {
                    sets.push(source.migrations().await?);
                }
                match migrating
                    .run(move |conn| migrate::migrate(conn, sets, read_only))
                    .await
                {
                    Err(e) if read_only => {
                        let _ = broken.set(e);
                        Ok(())
                    }
                    other => other,
                }
            });
        }

        host.provide(
            &SQL,
            Arc::new(Service {
                db,
                read_only,
                broken,
            }),
        )
    }
}

register_component!(StorageSqlite);

#[cfg(test)]
mod tests;
