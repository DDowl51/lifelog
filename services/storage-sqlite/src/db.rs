//! 连接、执行语句、错误映射。

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use kernel_api::AppError;
use rusqlite::types::{Value, ValueRef};
use rusqlite::{Connection, ErrorCode, OpenFlags};
use sql_contract::{Rows, SqlValue, Statement};

/// 一个连接。rusqlite 是同步的，所以每次操作都挪到阻塞线程上做。
#[derive(Clone)]
pub(crate) struct Db(Arc<Mutex<Connection>>);

impl Db {
    pub(crate) fn open(
        path: &str,
        read_only: bool,
        busy_timeout_ms: u64,
    ) -> Result<Self, AppError> {
        let conn = if path == ":memory:" {
            Connection::open_in_memory().map_err(sql_err)?
        } else if read_only {
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| {
                AppError::unavailable(format!(
                    "打不开数据库 {path}：{e}。它由可写的进程（常驻进程）创建，先启动那个进程"
                ))
            })?
        } else {
            if let Some(parent) = Path::new(path).parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    AppError::unavailable(format!("建不了目录 {}：{e}", parent.display()))
                })?;
            }
            let conn = Connection::open(path)
                .map_err(|e| AppError::unavailable(format!("打不开数据库 {path}：{e}")))?;
            conn.pragma_update(None, "journal_mode", "WAL")
                .map_err(sql_err)?;
            conn.pragma_update(None, "synchronous", "NORMAL")
                .map_err(sql_err)?;
            conn
        };
        conn.busy_timeout(Duration::from_millis(busy_timeout_ms))
            .map_err(sql_err)?;
        conn.pragma_update(None, "foreign_keys", true)
            .map_err(sql_err)?;
        Ok(Self(Arc::new(Mutex::new(conn))))
    }

    /// 在阻塞线程上用连接做一件事。
    pub(crate) async fn run<T, F>(&self, f: F) -> Result<T, AppError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, AppError> + Send + 'static,
    {
        let conn = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = conn.lock().unwrap_or_else(PoisonError::into_inner);
            f(&mut conn)
        })
        .await
        .map_err(|e| AppError::unavailable(format!("数据库操作被中断：{e}")))?
    }
}

fn to_sqlite(value: &SqlValue) -> Value {
    match value {
        SqlValue::Null => Value::Null,
        SqlValue::Integer(v) => Value::Integer(*v),
        SqlValue::Real(v) => Value::Real(*v),
        SqlValue::Text(v) => Value::Text(v.clone()),
        SqlValue::Blob(v) => Value::Blob(v.clone()),
    }
}

fn from_sqlite(value: ValueRef<'_>) -> SqlValue {
    match value {
        ValueRef::Null => SqlValue::Null,
        ValueRef::Integer(v) => SqlValue::Integer(v),
        ValueRef::Real(v) => SqlValue::Real(v),
        ValueRef::Text(v) => SqlValue::Text(String::from_utf8_lossy(v).into_owned()),
        ValueRef::Blob(v) => SqlValue::Blob(v.to_vec()),
    }
}

/// 执行一条语句，带回它的行和改动的行数。
pub(crate) fn run_statement(
    conn: &Connection,
    statement: &Statement,
    must_be_read_only: bool,
) -> Result<Rows, AppError> {
    let mut stmt = conn.prepare_cached(&statement.sql).map_err(sql_err)?;
    if must_be_read_only && !stmt.readonly() {
        return Err(AppError::validation(
            "query 只能执行只读语句；写入用 execute",
        ));
    }
    if stmt.parameter_count() != statement.params.len() {
        return Err(AppError::validation(format!(
            "语句有 {} 个参数，给了 {} 个",
            stmt.parameter_count(),
            statement.params.len()
        )));
    }
    for (i, value) in statement.params.iter().enumerate() {
        stmt.raw_bind_parameter(i + 1, to_sqlite(value))
            .map_err(sql_err)?;
    }
    let columns: Vec<String> = stmt.column_names().into_iter().map(str::to_owned).collect();
    let mut rows = Vec::new();
    {
        let mut cursor = stmt.raw_query();
        while let Some(row) = cursor.next().map_err(sql_err)? {
            let mut values = Vec::with_capacity(columns.len());
            for i in 0..columns.len() {
                values.push(from_sqlite(row.get_ref(i).map_err(sql_err)?));
            }
            rows.push(values);
        }
    }
    let affected = if stmt.readonly() { 0 } else { conn.changes() };
    Ok(Rows {
        columns,
        rows,
        affected,
    })
}

/// SQLite 的错误映射到固定的错误种类。
pub(crate) fn sql_err(e: rusqlite::Error) -> AppError {
    let text = e.to_string();
    // `sqlite_error_code()` 只认 `SqliteFailure`，语法错误是另一个变体，这里两个都看
    let code = match &e {
        rusqlite::Error::SqliteFailure(error, _) | rusqlite::Error::SqlInputError { error, .. } => {
            Some(error.code)
        }
        _ => None,
    };
    match code {
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => AppError::temp_fail(text),
        Some(ErrorCode::ConstraintViolation) => AppError::validation(text),
        Some(ErrorCode::ReadOnly) => AppError::unauthorized(text),
        // SQLITE_ERROR：语法错误、没有这张表、参数不对——都是调用方给的语句有问题
        Some(ErrorCode::Unknown) => AppError::validation(text),
        _ => AppError::unavailable(text),
    }
}
