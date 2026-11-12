//! 应用功能 `shell`：把 shell 执行过的命令导入成活动。
//!
//! 定时向 `ShellHistory` 要新的命令，转成活动（`shell-core`）交给 `activity`，
//! 成功之后才把游标存进自己的表。中途失败下次重读，靠去重键不会存两份。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use activity_contracts::{ACTIVITY_WRITE, ActivityWrite};
use clock_contract::{CLOCK, Clock};
use kernel_api::{
    AppError, Component, ConfigSection, ErrorKind, Host, Kind, Manifest, register_component,
};
use log_contract::{LOG, Logger};
use serde::{Deserialize, Serialize};
use shell_contract::{SHELL_HISTORY, ShellHistory};
use sql_contract::{MIGRATIONS, SQL, Sql, Statement, StaticMigrations};
use timer_contract::{Fired, TIMER, TIMER_FIRED, TimerId};

/// 已经执行过的迁移不能改。改表结构在末尾追加。
const TABLES: &[(&str, &str)] = &[(
    "读到哪了",
    "CREATE TABLE shell_cursor (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        cursor TEXT NOT NULL,
        updated_at INTEGER NOT NULL
    ) STRICT;",
)];

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
struct Config {
    /// 多久看一次有没有新命令。
    poll_secs: u64,
    /// 一次要多少条。
    batch: u32,
    /// 一轮最多要几次（第一次导入很多历史时，分几轮做完，不长时间占着事件投递）。
    max_batches: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            poll_secs: 60,
            batch: 500,
            max_batches: 20,
        }
    }
}

struct Shell {
    history: Arc<dyn ShellHistory>,
    write: Arc<dyn ActivityWrite>,
    sql: Arc<dyn Sql>,
    clock: Arc<dyn Clock>,
    log: Logger,
    timer: OnceLock<TimerId>,
    batch: u32,
    max_batches: u32,
    /// 历史暂时读不到时只提醒一次，恢复后再出问题再提醒。
    warned: AtomicBool,
}

impl Shell {
    async fn cursor(&self) -> Result<Option<String>, AppError> {
        let rows = self
            .sql
            .query(Statement::new(
                "SELECT cursor FROM shell_cursor WHERE id = 1",
            ))
            .await?;
        Ok(rows
            .rows
            .first()
            .and_then(|r| r.first())
            .and_then(|v| v.as_str())
            .map(str::to_owned))
    }

    async fn save(&self, cursor: &str, now: i64) -> Result<(), AppError> {
        self.sql
            .execute(vec![
                Statement::new(
                    "INSERT INTO shell_cursor (id, cursor, updated_at) VALUES (1, ?1, ?2)
                     ON CONFLICT(id) DO UPDATE SET cursor = excluded.cursor,
                                                   updated_at = excluded.updated_at",
                )
                .bind(cursor)
                .bind(now),
            ])
            .await
            .map(|_| ())
    }

    /// 导入一轮。返回这一轮交出去的条数。
    async fn import(&self) -> Result<usize, AppError> {
        let mut cursor = self.cursor().await?;
        let mut total = 0;
        for _ in 0..self.max_batches {
            let page = self.history.read(cursor.clone(), self.batch).await?;
            let now = self.clock.now().await?;
            let activities: Vec<_> = page
                .commands
                .iter()
                .filter_map(|c| shell_core::to_activity(c, now.offset_min))
                .collect();
            total += activities.len();
            if !activities.is_empty() {
                self.write.submit(activities).await?;
            }
            self.save(&page.next, now.utc_ms).await?;
            cursor = Some(page.next);
            if !page.more {
                break;
            }
        }
        Ok(total)
    }

    async fn on_timer(&self, fired: Fired) -> Result<(), AppError> {
        if self.timer.get() != Some(&fired.id) {
            return Ok(());
        }
        match self.import().await {
            Ok(_) => {
                self.warned.store(false, Ordering::Relaxed);
                Ok(())
            }
            // 历史暂时读不到（比如还没装）：提醒一次，下一轮再试
            Err(e) if e.kind == ErrorKind::Unavailable => {
                if !self.warned.swap(true, Ordering::Relaxed) {
                    self.log.warn(format!("暂时导入不了 shell 命令：{e}")).await;
                }
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}

pub struct ShellComponent;

impl Component for ShellComponent {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "shell",
            kind: Kind::Feature,
            summary: "把 shell 执行过的命令导入成活动",
            provides: vec![],
            requires: vec![
                SHELL_HISTORY.id(),
                TIMER.id(),
                CLOCK.id(),
                SQL.id(),
                MIGRATIONS.id(),
                ACTIVITY_WRITE.id(),
                LOG.id(),
            ],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let config: Config = config.parse()?;
        if config.poll_secs == 0 || config.batch == 0 || config.max_batches == 0 {
            return Err(AppError::config("poll_secs、batch、max_batches 都不能是 0"));
        }
        host.contribute(
            &MIGRATIONS,
            Arc::new(StaticMigrations::new("shell", TABLES)),
        )?;
        let timer = host.resolve(&TIMER)?;
        let shell = Arc::new(Shell {
            history: host.resolve(&SHELL_HISTORY)?,
            write: host.resolve(&ACTIVITY_WRITE)?,
            sql: host.resolve(&SQL)?,
            clock: host.resolve(&CLOCK)?,
            log: Logger::new(host.resolve(&LOG)?, host.id()),
            timer: OnceLock::new(),
            batch: config.batch,
            max_batches: config.max_batches,
            warned: AtomicBool::new(false),
        });

        let s = shell.clone();
        host.subscribe(&TIMER_FIRED, "import", move |fired| {
            let s = s.clone();
            async move { s.on_timer(fired).await }
        })?;

        host.on_ready(move || async move {
            let id = timer.every(config.poll_secs * 1_000).await?;
            let _ = shell.timer.set(id);
            Ok(())
        });
        Ok(())
    }
}

register_component!(ShellComponent);
