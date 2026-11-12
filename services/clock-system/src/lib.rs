//! 系统服务 `clock-system`：用操作系统的时钟提供 `Clock`。

use std::sync::Arc;

use clock_contract::{CLOCK, Clock, Moment};
use jiff::Zoned;
use kernel_api::{
    AppError, Component, ConfigSection, Host, Kind, Manifest, async_trait, register_component,
};
use serde::{Deserialize, Serialize};

/// 没有可配的东西；空结构体用来拒绝写错的键。
#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Config {}

struct System;

#[async_trait]
impl Clock for System {
    async fn now(&self) -> Result<Moment, AppError> {
        let now = Zoned::now();
        Ok(Moment {
            utc_ms: now.timestamp().as_millisecond(),
            offset_min: now.offset().seconds() / 60,
        })
    }
}

pub struct ClockSystem;

impl Component for ClockSystem {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<Config>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "clock-system",
            kind: Kind::Service,
            summary: "用操作系统的时钟报告当前时刻",
            provides: vec![CLOCK.id()],
            requires: vec![],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        let _: Config = config.parse()?;
        host.provide(&CLOCK, Arc::new(System))
    }
}

register_component!(ClockSystem);

#[cfg(test)]
mod tests {
    use clock_contract::testing::clock_contract;
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn obeys_the_clock_contract() {
        let app = testkit::app().with(ClockSystem).boot().await.unwrap();
        clock_contract(app.resolve(&CLOCK).unwrap()).await.unwrap();
    }

    #[tokio::test]
    async fn reports_a_recent_time() {
        let app = testkit::app().with(ClockSystem).boot().await.unwrap();
        let now = app.resolve(&CLOCK).unwrap().now().await.unwrap();
        // 2026-01-01 之后
        assert!(now.utc_ms > 1_767_225_600_000, "{now:?}");
    }

    #[tokio::test]
    async fn rejects_unknown_config_keys() {
        let result = testkit::app()
            .with(ClockSystem)
            .config("clock-system", json!({ "timezone": "UTC" }))
            .unwrap()
            .boot()
            .await;
        let err = result.err().unwrap();
        assert!(
            err.message.contains("clock-system") && err.message.contains("timezone"),
            "{err}"
        );
    }
}
