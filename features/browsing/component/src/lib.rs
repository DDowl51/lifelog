//! 应用功能 `browsing`：把浏览器的访问历史导入成活动。
//!
//! 定时向 `BrowserHistory` 列出来源，逐个来源要新的访问，转成活动（`browsing-core`）交给 `activity`，
//! 成功之后才把这个来源的游标存进自己的表。一个来源出错不影响别的来源。

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use activity_contracts::{ACTIVITY_WRITE, ActivityWrite};
use browser_contract::{BROWSER_HISTORY, BrowserHistory, HistorySource};
use clock_contract::{CLOCK, Clock};
use kernel_api::{
    AppError, Component, ConfigSection, ErrorKind, Host, Kind, Manifest, register_component,
};
use log_contract::{LOG, Logger};
use serde::{Deserialize, Serialize};
use sql_contract::{MIGRATIONS, SQL, Sql, Statement, StaticMigrations};
use timer_contract::{Fired, TIMER, TIMER_FIRED, TimerId};

/// 已经执行过的迁移不能改。改表结构在末尾追加。
const TABLES: &[(&str, &str)] = &[(
    "每个来源读到哪了",
    "CREATE TABLE browsing_cursor (
        source TEXT PRIMARY KEY,
        cursor TEXT NOT NULL,
        updated_at INTEGER NOT NULL
    ) STRICT;",
)];

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
struct Config {
    /// 多久看一次。每次都要复制浏览器的库（几十 MB），不要太频繁。
    poll_secs: u64,
    /// 一次要多少条。
    batch: u32,
    /// 每个来源一轮最多要几次（第一次导入大量历史时分几轮做完）。
    max_batches: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            poll_secs: 300,
            batch: 1_000,
            max_batches: 20,
        }
    }
}

struct Browsing {
    history: Arc<dyn BrowserHistory>,
    write: Arc<dyn ActivityWrite>,
    sql: Arc<dyn Sql>,
    clock: Arc<dyn Clock>,
    log: Logger,
    timer: OnceLock<TimerId>,
    batch: u32,
    max_batches: u32,
    /// 暂时读不了的来源只提醒一次，恢复后再出问题再提醒。
    warned: Mutex<HashSet<String>>,
}

impl Browsing {
    async fn cursor(&self, source: &str) -> Result<Option<String>, AppError> {
        let rows = self
            .sql
            .query(
                Statement::new("SELECT cursor FROM browsing_cursor WHERE source = ?1").bind(source),
            )
            .await?;
        Ok(rows
            .rows
            .first()
            .and_then(|r| r.first())
            .and_then(|v| v.as_str())
            .map(str::to_owned))
    }

    async fn save(&self, source: &str, cursor: &str, now: i64) -> Result<(), AppError> {
        self.sql
            .execute(vec![
                Statement::new(
                    "INSERT INTO browsing_cursor (source, cursor, updated_at) VALUES (?1, ?2, ?3)
                     ON CONFLICT(source) DO UPDATE SET cursor = excluded.cursor,
                                                       updated_at = excluded.updated_at",
                )
                .bind(source)
                .bind(cursor)
                .bind(now),
            ])
            .await
            .map(|_| ())
    }

    /// 导入一个来源。返回交出去的条数。
    async fn import(&self, source: &HistorySource) -> Result<usize, AppError> {
        let mut cursor = self.cursor(&source.id).await?;
        let mut total = 0;
        for _ in 0..self.max_batches {
            let page = self
                .history
                .read(source.id.clone(), cursor.clone(), self.batch)
                .await?;
            let now = self.clock.now().await?;
            let activities: Vec<_> = page
                .visits
                .iter()
                .filter_map(|v| browsing_core::to_activity(source, v, now.offset_min))
                .collect();
            total += activities.len();
            if !activities.is_empty() {
                self.write.submit(activities).await?;
            }
            self.save(&source.id, &page.next, now.utc_ms).await?;
            cursor = Some(page.next);
            if !page.more {
                break;
            }
        }
        Ok(total)
    }

    fn warned(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.warned.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn on_timer(&self, fired: Fired) -> Result<(), AppError> {
        if self.timer.get() != Some(&fired.id) {
            return Ok(());
        }
        for source in self.history.sources().await? {
            match self.import(&source).await {
                Ok(_) => {
                    self.warned().remove(&source.id);
                }
                // 这一个来源暂时读不了（库正被写到一半、刚被删）：提醒一次，下一轮再试，别的来源照常
                Err(e) if matches!(e.kind, ErrorKind::Unavailable | ErrorKind::NotFound) => {
                    if self.warned().insert(source.id.clone()) {
                        self.log
                            .warn(format!("暂时导入不了 {} 的浏览历史：{e}", source.id))
                            .await;
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

pub struct BrowsingComponent;

impl Component for BrowsingComponent {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "browsing",
            kind: Kind::Feature,
            summary: "把浏览器的访问历史导入成活动",
            provides: vec![],
            requires: vec![
                BROWSER_HISTORY.id(),
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
            Arc::new(StaticMigrations::new("browsing", TABLES)),
        )?;
        let timer = host.resolve(&TIMER)?;
        let browsing = Arc::new(Browsing {
            history: host.resolve(&BROWSER_HISTORY)?,
            write: host.resolve(&ACTIVITY_WRITE)?,
            sql: host.resolve(&SQL)?,
            clock: host.resolve(&CLOCK)?,
            log: Logger::new(host.resolve(&LOG)?, host.id()),
            timer: OnceLock::new(),
            batch: config.batch,
            max_batches: config.max_batches,
            warned: Mutex::default(),
        });

        let b = browsing.clone();
        host.subscribe(&TIMER_FIRED, "import", move |fired| {
            let b = b.clone();
            async move { b.on_timer(fired).await }
        })?;

        host.on_ready(move || async move {
            let id = timer.every(config.poll_secs * 1_000).await?;
            let _ = browsing.timer.set(id);
            Ok(())
        });
        Ok(())
    }
}

register_component!(BrowsingComponent);
