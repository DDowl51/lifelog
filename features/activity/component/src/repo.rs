//! 用 `Sql` 契约实现活动的存储。表和 SQL 都归这个组件。

use std::sync::Arc;

use activity_contracts::{
    Activity, ActivityId, ActivityQuery, DurationRow, GroupBy, StoredActivity,
};
use activity_core::Repo;
use kernel_api::{AppError, async_trait};
use sql_contract::{Sql, SqlValue, Statement};

/// 已经执行过的迁移不能改。改表结构在末尾追加。
pub(crate) const MIGRATIONS: &[(&str, &str)] = &[(
    "活动表",
    "
    CREATE TABLE activity_records (
        id INTEGER PRIMARY KEY,
        source TEXT NOT NULL,
        kind TEXT NOT NULL,
        ts_start INTEGER NOT NULL,
        ts_end INTEGER,
        tz_offset_min INTEGER NOT NULL,
        app TEXT,
        title TEXT,
        project TEXT,
        payload TEXT NOT NULL DEFAULT '{}',
        dedup_key TEXT UNIQUE,
        redacted INTEGER NOT NULL DEFAULT 0,
        ingested_at INTEGER NOT NULL
    ) STRICT;
    CREATE INDEX activity_records_time ON activity_records(ts_start);
    CREATE INDEX activity_records_src_time ON activity_records(source, kind, ts_start);
    ",
)];

/// 写入一条；`dedup_key` 冲突时更新那一行，`id` 和 `ingested_at` 保持不变。
const UPSERT: &str = "
    INSERT INTO activity_records (source, kind, ts_start, ts_end, tz_offset_min,
                                  app, title, project, payload, dedup_key, redacted, ingested_at)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
    ON CONFLICT (dedup_key) DO UPDATE SET
        source = excluded.source,
        kind = excluded.kind,
        ts_start = excluded.ts_start,
        ts_end = excluded.ts_end,
        tz_offset_min = excluded.tz_offset_min,
        app = excluded.app,
        title = excluded.title,
        project = excluded.project,
        payload = excluded.payload,
        redacted = excluded.redacted
    RETURNING id
";

const COLUMNS: &str = "id, source, kind, ts_start, ts_end, tz_offset_min, app, title, project, \
                       payload, dedup_key, redacted, ingested_at";

//               00:00     01:00            02:00     03:00
//                           │    查询范围    │
// before        ├──────────┤                                 ts_end = 01:00，不大于 start → 不算
// cross-start         ├──────────┤                           算
// inside                       ├──┤                          算
// cross-end                              ├──────┤            算
// after                                      ├──────┤        ts_start = 02:00，不小于 end → 不算
// at-start（瞬时）           ●                                 01:00 在 [01:00, 02:00) 里 → 算
// at-end（瞬时）                             ●                 02:00 不在范围里 → 不算
/// 和 `[?1, ?2)` 有重叠，并且符合过滤条件（`?3` 来源、`?4` 种类，为 NULL 时不过滤）。
const FILTER: &str = "
    ts_start < ?2
    AND (ts_end > ?1 OR (ts_end IS NULL AND ts_start >= ?1))
    AND (?3 IS NULL OR source = ?3)
    AND (?4 IS NULL OR kind = ?4)
";

fn filtered(sql: String, query: &ActivityQuery) -> Statement {
    Statement::new(sql)
        .bind(query.range.start)
        .bind(query.range.end)
        .bind(query.source.clone())
        .bind(query.kind.clone())
}

fn corrupt(what: &str) -> AppError {
    AppError::unavailable(format!("活动表里的数据不对：{what}"))
}

fn int(value: &SqlValue, column: &str) -> Result<i64, AppError> {
    value.as_i64().ok_or_else(|| corrupt(column))
}

fn opt_int(value: &SqlValue, column: &str) -> Result<Option<i64>, AppError> {
    if value.is_null() {
        Ok(None)
    } else {
        int(value, column).map(Some)
    }
}

fn text(value: &SqlValue, column: &str) -> Result<String, AppError> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| corrupt(column))
}

fn opt_text(value: &SqlValue, column: &str) -> Result<Option<String>, AppError> {
    if value.is_null() {
        Ok(None)
    } else {
        text(value, column).map(Some)
    }
}

fn stored(row: &[SqlValue]) -> Result<StoredActivity, AppError> {
    let [
        id,
        source,
        kind,
        ts_start,
        ts_end,
        offset,
        app,
        title,
        project,
        payload,
        dedup_key,
        redacted,
        ingested_at,
    ] = row
    else {
        return Err(corrupt("列数"));
    };
    let payload =
        serde_json::from_str(&text(payload, "payload")?).map_err(|_| corrupt("payload"))?;
    Ok(StoredActivity {
        id: ActivityId(int(id, "id")?),
        activity: Activity {
            source: text(source, "source")?,
            kind: text(kind, "kind")?,
            ts_start: int(ts_start, "ts_start")?,
            ts_end: opt_int(ts_end, "ts_end")?,
            tz_offset_min: i32::try_from(int(offset, "tz_offset_min")?)
                .map_err(|_| corrupt("tz_offset_min"))?,
            app: opt_text(app, "app")?,
            title: opt_text(title, "title")?,
            project: opt_text(project, "project")?,
            payload,
            dedup_key: opt_text(dedup_key, "dedup_key")?,
            redacted: int(redacted, "redacted")? != 0,
        },
        ingested_at: int(ingested_at, "ingested_at")?,
    })
}

pub(crate) struct SqlRepo {
    sql: Arc<dyn Sql>,
}

impl SqlRepo {
    pub(crate) fn new(sql: Arc<dyn Sql>) -> Self {
        Self { sql }
    }

    pub(crate) async fn list(
        &self,
        query: &ActivityQuery,
    ) -> Result<Vec<StoredActivity>, AppError> {
        let sql =
            format!("SELECT {COLUMNS} FROM activity_records WHERE {FILTER} ORDER BY ts_start, id");
        let rows = self.sql.query(filtered(sql, query)).await?;
        rows.rows.iter().map(|row| stored(row)).collect()
    }

    /// 编号大于 `after` 的，按编号升序，最多 `limit` 条。
    pub(crate) async fn since(
        &self,
        after: Option<ActivityId>,
        limit: u32,
    ) -> Result<Vec<StoredActivity>, AppError> {
        let sql =
            format!("SELECT {COLUMNS} FROM activity_records WHERE id > ?1 ORDER BY id LIMIT ?2");
        let statement = Statement::new(sql)
            .bind(after.map_or(0, |a| a.0))
            .bind(i64::from(limit));
        let rows = self.sql.query(statement).await?;
        rows.rows.iter().map(|row| stored(row)).collect()
    }

    /// 按编号取，顺序和 `ids` 一致；不存在的跳过。
    pub(crate) async fn get(&self, ids: &[ActivityId]) -> Result<Vec<StoredActivity>, AppError> {
        let mut out = Vec::with_capacity(ids.len());
        // SQLite 一条语句的参数个数有上限，分批取
        for chunk in ids.chunks(500) {
            let marks = vec!["?"; chunk.len()].join(", ");
            let sql = format!("SELECT {COLUMNS} FROM activity_records WHERE id IN ({marks})");
            let statement = chunk.iter().fold(Statement::new(sql), |s, id| s.bind(id.0));
            let rows = self.sql.query(statement).await?;
            for row in &rows.rows {
                out.push(stored(row)?);
            }
        }
        out.sort_by_key(|s| ids.iter().position(|id| *id == s.id).unwrap_or(usize::MAX));
        Ok(out)
    }

    pub(crate) async fn durations(
        &self,
        query: &ActivityQuery,
        group_by: GroupBy,
    ) -> Result<Vec<DurationRow>, AppError> {
        let column = match group_by {
            GroupBy::App => "app",
            GroupBy::Title => "title",
            GroupBy::Project => "project",
        };
        let sql = format!(
            "SELECT {column},                                        -- 分组的列
                    SUM(MIN(ts_end, ?2) - MAX(ts_start, ?1)),        -- 裁剪后的总毫秒数
                    COUNT(*)                                         -- 时段个数
             FROM activity_records
             WHERE ts_end IS NOT NULL AND {FILTER}
             GROUP BY {column}
             ORDER BY 2 DESC, 1"
        );
        let rows = self.sql.query(filtered(sql, query)).await?;
        rows.rows
            .iter()
            .map(|row| {
                let [key, total, spans] = row.as_slice() else {
                    return Err(corrupt("列数"));
                };
                Ok(DurationRow {
                    key: opt_text(key, column)?,
                    total_ms: int(total, "总时长")?,
                    spans: u64::try_from(int(spans, "时段个数")?)
                        .map_err(|_| corrupt("时段个数"))?,
                })
            })
            .collect()
    }
}

#[async_trait]
impl Repo for SqlRepo {
    async fn upsert(
        &self,
        activities: Vec<Activity>,
        ingested_at: i64,
    ) -> Result<Vec<ActivityId>, AppError> {
        let mut statements = Vec::with_capacity(activities.len());
        for activity in activities {
            let payload = serde_json::to_string(&activity.payload)
                .map_err(|e| AppError::validation(format!("payload 写不成 JSON：{e}")))?;
            statements.push(
                Statement::new(UPSERT)
                    .bind(activity.source)
                    .bind(activity.kind)
                    .bind(activity.ts_start)
                    .bind(activity.ts_end)
                    .bind(activity.tz_offset_min)
                    .bind(activity.app)
                    .bind(activity.title)
                    .bind(activity.project)
                    .bind(payload)
                    .bind(activity.dedup_key)
                    .bind(activity.redacted)
                    .bind(ingested_at),
            );
        }
        let results = self.sql.execute(statements).await?;
        results
            .iter()
            .map(|rows| {
                rows.rows
                    .first()
                    .and_then(|row| row.first())
                    .and_then(SqlValue::as_i64)
                    .map(ActivityId)
                    .ok_or_else(|| corrupt("写入后没有带回编号"))
            })
            .collect()
    }
}
