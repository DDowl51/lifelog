//! 执行各组件登记的迁移。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use kernel_api::AppError;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use sql_contract::MigrationSet;

use crate::db::sql_err;

const BOOKKEEPING: &str = "
    CREATE TABLE IF NOT EXISTS _migrations (
        component TEXT NOT NULL,
        version INTEGER NOT NULL,
        name TEXT NOT NULL,
        checksum TEXT NOT NULL,
        PRIMARY KEY (component, version)
    ) STRICT
";

fn checksum(sql: &str) -> String {
    Sha256::digest(sql.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 组件 id 对应的表名前缀：`sync-dida` → `sync_dida_`。
fn prefix(component: &str) -> String {
    format!("{}_", component.replace('-', "_"))
}

fn has_bookkeeping(conn: &Connection) -> Result<bool, AppError> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_migrations'",
        [],
        |_| Ok(()),
    )
    .optional()
    .map(|found| found.is_some())
    .map_err(sql_err)
}

fn applied(conn: &Connection, component: &str) -> Result<BTreeMap<u32, String>, AppError> {
    let mut stmt = conn
        .prepare("SELECT version, checksum FROM _migrations WHERE component = ?1")
        .map_err(sql_err)?;
    let rows = stmt
        .query_map([component], |row| {
            Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(sql_err)?;
    rows.collect::<Result<_, _>>().map_err(sql_err)
}

/// 把登记上来的迁移对一遍账，执行还没执行的。
///
/// 只读时不执行任何东西：有没执行的迁移就报错，让可写的进程先升级。
pub(crate) fn migrate(
    conn: &mut Connection,
    mut sets: Vec<MigrationSet>,
    read_only: bool,
) -> Result<(), AppError> {
    sets.sort_by(|a, b| a.component.cmp(&b.component));
    for pair in sets.windows(2) {
        if pair[0].component == pair[1].component {
            return Err(AppError::config(format!(
                "组件 `{}` 登记了两份迁移",
                pair[0].component
            )));
        }
    }

    let initialized = has_bookkeeping(conn)?;
    if !read_only {
        conn.execute_batch(BOOKKEEPING).map_err(sql_err)?;
    }

    for set in &sets {
        for (expected, migration) in (1u32..).zip(&set.migrations) {
            if migration.version != expected {
                return Err(AppError::config(format!(
                    "组件 `{}` 的迁移版本必须从 1 连续递增：第 {expected} 个写的是 {}",
                    set.component, migration.version
                )));
            }
        }

        let done = if initialized || !read_only {
            applied(conn, &set.component)?
        } else {
            BTreeMap::new()
        };

        if let Some((version, _)) = done
            .iter()
            .find(|(v, _)| **v as usize > set.migrations.len())
        {
            return Err(AppError::config(format!(
                "数据库里组件 `{}` 已经执行到迁移 {version}，程序只认识到 {}：数据库比程序新，升级程序",
                set.component,
                set.migrations.len()
            )));
        }

        for migration in &set.migrations {
            let sum = checksum(&migration.sql);
            match done.get(&migration.version) {
                Some(recorded) if *recorded == sum => {}
                Some(_) => {
                    return Err(AppError::config(format!(
                        "组件 `{}` 的迁移 {}（{}）在执行之后被改动了。已经执行过的迁移不能改：\
                         把它改回去，表结构的变化写成一个新版本",
                        set.component, migration.version, migration.name
                    )));
                }
                None if read_only => {
                    return Err(AppError::unavailable(format!(
                        "数据库的结构落后于程序：组件 `{}` 的迁移 {}（{}）还没执行。\
                         这个进程是只读的，先启动可写的进程（常驻进程）让它升级",
                        set.component, migration.version, migration.name
                    )));
                }
                None => apply(
                    conn,
                    &set.component,
                    migration.version,
                    &migration.name,
                    &migration.sql,
                    &sum,
                )?,
            }
        }
    }
    Ok(())
}

/// 执行一步迁移并记账，两件事在同一个事务里。
fn apply(
    conn: &mut Connection,
    component: &str,
    version: u32,
    name: &str,
    sql: &str,
    sum: &str,
) -> Result<(), AppError> {
    let context = format!("组件 `{component}` 的迁移 {version}（{name}）");
    let tx = conn.transaction().map_err(sql_err)?;

    // 迁移只能动自己前缀的表。用 SQLite 的授权钩子检查，不用解析 SQL。
    let violation: Arc<Mutex<Option<String>>> = Arc::default();
    let guard = Guard {
        prefix: prefix(component),
        violation: violation.clone(),
    };
    tx.authorizer(Some(move |ctx: AuthContext<'_>| guard.check(&ctx)))
        .map_err(sql_err)?;
    let result = tx.execute_batch(sql);
    tx.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
        .map_err(sql_err)?;

    if let Some(what) = violation
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
    {
        return Err(AppError::config(format!(
            "{context}动了不属于它的东西：{what}。组件只能创建和修改名字以 `{}` 开头的表、索引、视图",
            prefix(component)
        )));
    }
    result.map_err(|e| sql_err(e).context(&context))?;

    tx.execute(
        "INSERT INTO _migrations (component, version, name, checksum) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![component, version, name, sum],
    )
    .map_err(sql_err)?;
    tx.commit().map_err(sql_err)
}

struct Guard {
    prefix: String,
    violation: Arc<Mutex<Option<String>>>,
}

impl Guard {
    fn own(&self, name: &str) -> bool {
        name.starts_with(&self.prefix)
    }

    /// SQLite 自己的表（建表时它会写 `sqlite_master`）。
    fn internal(name: &str) -> bool {
        name.starts_with("sqlite_")
    }

    fn check(&self, ctx: &AuthContext<'_>) -> Authorization {
        let offending = match ctx.action {
            AuthAction::CreateTable { table_name }
            | AuthAction::DropTable { table_name }
            | AuthAction::AlterTable { table_name, .. }
            | AuthAction::CreateVtable { table_name, .. }
            | AuthAction::DropVtable { table_name, .. } => {
                (!self.own(table_name)).then_some(table_name)
            }
            AuthAction::CreateIndex {
                index_name,
                table_name,
            }
            | AuthAction::DropIndex {
                index_name,
                table_name,
            } => {
                // UNIQUE 约束会让 SQLite 自己建一个 `sqlite_autoindex_…` 索引
                if !self.own(table_name) {
                    Some(table_name)
                } else if !self.own(index_name) && !Self::internal(index_name) {
                    Some(index_name)
                } else {
                    None
                }
            }
            AuthAction::CreateView { view_name } | AuthAction::DropView { view_name } => {
                (!self.own(view_name)).then_some(view_name)
            }
            AuthAction::CreateTrigger {
                trigger_name,
                table_name,
            }
            | AuthAction::DropTrigger {
                trigger_name,
                table_name,
            } => {
                if !self.own(table_name) {
                    Some(table_name)
                } else if !self.own(trigger_name) {
                    Some(trigger_name)
                } else {
                    None
                }
            }
            AuthAction::Insert { table_name }
            | AuthAction::Delete { table_name }
            | AuthAction::Update { table_name, .. } => {
                (!self.own(table_name) && !Self::internal(table_name)).then_some(table_name)
            }
            AuthAction::Attach { filename } => Some(filename),
            _ => None,
        };
        match offending {
            None => Authorization::Allow,
            Some(name) => {
                let mut slot = self
                    .violation
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                slot.get_or_insert_with(|| format!("`{name}`"));
                Authorization::Deny
            }
        }
    }
}
