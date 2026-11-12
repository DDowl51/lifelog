//! 系统服务 `browser-history`：把各浏览器适配登记的来源合在一起。
//!
//! 它拥有扩展点 `BROWSER_ADAPTERS`，不认识任何具体的浏览器。和 `storage-sqlite` 拥有迁移扩展点是同一个形状：
//! 扩展点由系统服务拥有，登记它的一方（这里也是系统服务）才不会依赖应用功能。

use std::sync::Arc;

use browser_contract::{
    BROWSER_ADAPTERS, BROWSER_HISTORY, BrowserAdapter, BrowserHistory, HistorySource, VisitPage,
};
use kernel_api::{
    AppError, Collector, Component, ConfigSection, Host, Kind, Manifest, NoConfig, async_trait,
    register_component,
};
use log_contract::{LOG, Logger};

struct History {
    adapters: Collector<dyn BrowserAdapter>,
    log: Logger,
}

impl History {
    /// 每个适配和它的来源。列来源出错的适配跳过并记日志：一个浏览器出问题不影响别的。
    async fn all(&self) -> Vec<(Arc<dyn BrowserAdapter>, Vec<HistorySource>)> {
        let mut out = Vec::new();
        for adapter in self.adapters.all() {
            match adapter.sources().await {
                Ok(sources) => out.push((adapter, sources)),
                Err(e) => {
                    self.log
                        .warn(format!("有一个浏览器的历史列不出来：{e}"))
                        .await
                }
            }
        }
        out
    }
}

#[async_trait]
impl BrowserHistory for History {
    async fn sources(&self) -> Result<Vec<HistorySource>, AppError> {
        let mut out: Vec<HistorySource> = Vec::new();
        for (_, sources) in self.all().await {
            for source in sources {
                // 两个适配报了同一个来源：只认先登记的那个
                if !out.iter().any(|s| s.id == source.id) {
                    out.push(source);
                }
            }
        }
        Ok(out)
    }

    async fn read(
        &self,
        source: String,
        after: Option<String>,
        limit: u32,
    ) -> Result<VisitPage, AppError> {
        for (adapter, sources) in self.all().await {
            if sources.iter().any(|s| s.id == source) {
                return adapter.read(source, after, limit).await;
            }
        }
        Err(AppError::not_found(format!("没有来源 `{source}`")))
    }
}

pub struct BrowserHistoryService;

impl Component for BrowserHistoryService {
    fn config_defaults(&self) -> Option<ConfigSection> {
        ConfigSection::defaults::<NoConfig>()
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            id: "browser-history",
            kind: Kind::Service,
            summary: "把各浏览器的访问历史合在一起",
            provides: vec![BROWSER_HISTORY.id(), BROWSER_ADAPTERS.id()],
            requires: vec![LOG.id()],
            optional_requires: vec![],
        }
    }

    fn start(&self, config: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        config.parse::<NoConfig>()?;
        let history = History {
            adapters: host.collector(&BROWSER_ADAPTERS)?,
            log: Logger::new(host.resolve(&LOG)?, host.id()),
        };
        host.provide(&BROWSER_HISTORY, Arc::new(history))
    }
}

register_component!(BrowserHistoryService);
