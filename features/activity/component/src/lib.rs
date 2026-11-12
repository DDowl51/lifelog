//! 应用功能 `activity`：活动记录的入库和读取。
//!
//! 这个包是适配层和组件定义：用 `Sql` 契约实现存储，把用例接到契约上。
//! 规则在 `activity-core` 里。

mod repo;

use std::sync::{Arc, OnceLock};

use activity_contracts::{
    ACTIVITY_COMMITTED, ACTIVITY_PROCESSORS, ACTIVITY_READ, ACTIVITY_WRITE, Activity, ActivityId,
    ActivityProcessor, ActivityQuery, ActivityRead, ActivityWrite, Committed, DurationRow, GroupBy,
    StoredActivity,
};
use activity_core::{Submit, order_processors};
use clock_contract::CLOCK;
use kernel_api::{
    AppError, Component, ConfigSection, Host, Kind, Manifest, Publisher, async_trait,
    register_component,
};
use log_contract::{LOG, Logger};
use serde::{Deserialize, Serialize};
use sql_contract::{MIGRATIONS, SQL, StaticMigrations};

use crate::repo::SqlRepo;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
struct Config {
    /// 处理器的执行顺序（策略）。启用了的处理器都要列在这里；列了但没启用的跳过。
    processors: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // 先归类再脱敏：归类要看原始的路径和标题；脱敏放最后，后面没有谁再往里写文字
            processors: vec!["project".to_owned(), "privacy".to_owned()],
        }
    }
}

type Chain = Arc<OnceLock<Vec<Arc<dyn ActivityProcessor>>>>;

struct Write {
    submit: Submit,
    chain: Chain,
    committed: Publisher<Committed>,
}

#[async_trait]
impl ActivityWrite for Write {
    async fn submit(&self, activities: Vec<Activity>) -> Result<Vec<Option<ActivityId>>, AppError> {
        let chain = self
            .chain
            .get()
            .ok_or_else(|| AppError::unavailable("activity 还没就绪，不能提交"))?;
        let slots = self.submit.run(activities, chain).await?;
        let ids: Vec<ActivityId> = slots.iter().flatten().copied().collect();
        if !ids.is_empty()
            && let Err(e) = self.committed.publish(Committed { ids }).await
        {
            // 已经入库了，通知发不出去不算提交失败
            self.submit.log.warn(format!("入库通知没发出去：{e}")).await;
        }
        Ok(slots)
    }
}

struct Read {
    repo: Arc<SqlRepo>,
}

#[async_trait]
impl ActivityRead for Read {
    async fn list(&self, query: ActivityQuery) -> Result<Vec<StoredActivity>, AppError> {
        self.repo.list(&query).await
    }

    async fn durations(
        &self,
        query: ActivityQuery,
        group_by: GroupBy,
    ) -> Result<Vec<DurationRow>, AppError> {
        self.repo.durations(&query, group_by).await
    }

    async fn since(
        &self,
        after: Option<ActivityId>,
        limit: u32,
    ) -> Result<Vec<StoredActivity>, AppError> {
        self.repo.since(after, limit).await
    }

    async fn get(&self, ids: Vec<ActivityId>) -> Result<Vec<StoredActivity>, AppError> {
        self.repo.get(&ids).await
    }
}

pub struct ActivityComponent;

impl Component for ActivityComponent {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "activity",
            kind: Kind::Feature,
            summary: "活动记录的入库和读取",
            provides: vec![
                ACTIVITY_WRITE.id(),
                ACTIVITY_READ.id(),
                ACTIVITY_PROCESSORS.id(),
            ],
            requires: vec![SQL.id(), MIGRATIONS.id(), CLOCK.id(), LOG.id()],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let config: Config = config.parse()?;
        let repo = Arc::new(SqlRepo::new(host.resolve(&SQL)?));
        let submit = Submit {
            repo: repo.clone(),
            clock: host.resolve(&CLOCK)?,
            log: Logger::new(host.resolve(&LOG)?, host.id()),
        };

        host.contribute(
            &MIGRATIONS,
            Arc::new(StaticMigrations::new("activity", repo::MIGRATIONS)),
        )?;

        // 处理器要等所有组件登记完才齐，所以顺序在就绪时排
        let processors = host.collector(&ACTIVITY_PROCESSORS)?;
        let chain = Chain::default();
        let ready_chain = chain.clone();
        host.on_ready(move || async move {
            let all = processors.all();
            let mut names = Vec::with_capacity(all.len());
            for processor in &all {
                names.push(processor.name().await?);
            }
            let order = order_processors(&names, &config.processors).map_err(AppError::config)?;
            let _ = ready_chain.set(order.into_iter().map(|i| all[i].clone()).collect());
            Ok(())
        });

        let committed = host.publisher(&ACTIVITY_COMMITTED)?;
        host.provide(
            &ACTIVITY_WRITE,
            Arc::new(Write {
                submit,
                chain,
                committed,
            }),
        )?;
        host.provide(&ACTIVITY_READ, Arc::new(Read { repo }))
    }
}

register_component!(ActivityComponent);
