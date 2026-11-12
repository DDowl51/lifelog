//! 两个假的浏览器登记上来，合在一起读。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use browser_contract::testing::{MemoryBrowser, visit};
use browser_contract::{
    BROWSER_ADAPTERS, BROWSER_HISTORY, BrowserAdapter, HistorySource, VisitPage,
};
use browser_history::BrowserHistoryService;
use kernel_api::{
    AppError, Component, ConfigSection, ErrorKind, Host, Kind, Manifest, async_trait,
};
use log_contract::testing::MemoryLog;

/// 第二种浏览器：和第一种用不同的组件 id 登记。
#[derive(Clone)]
struct Other(MemoryBrowser);

#[async_trait]
impl BrowserAdapter for Other {
    async fn sources(&self) -> Result<Vec<HistorySource>, AppError> {
        self.0.sources().await
    }

    async fn read(&self, s: String, a: Option<String>, l: u32) -> Result<VisitPage, AppError> {
        self.0.read(s, a, l).await
    }
}

impl Component for Other {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: "browser-other",
            kind: Kind::Service,
            summary: "测试用",
            provides: vec![],
            requires: vec![BROWSER_ADAPTERS.id()],
            optional_requires: vec![],
        }
    }

    fn start(&self, _: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        host.contribute(&BROWSER_ADAPTERS, Arc::new(self.clone()))
    }
}

/// 列不出来源的浏览器。
#[derive(Clone)]
struct Broken;

#[async_trait]
impl BrowserAdapter for Broken {
    async fn sources(&self) -> Result<Vec<HistorySource>, AppError> {
        Err(AppError::unavailable("配置目录读不了"))
    }

    async fn read(&self, _: String, _: Option<String>, _: u32) -> Result<VisitPage, AppError> {
        Err(AppError::unavailable("配置目录读不了"))
    }
}

impl Component for Broken {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: "browser-broken",
            kind: Kind::Service,
            summary: "测试用",
            provides: vec![],
            requires: vec![BROWSER_ADAPTERS.id()],
            optional_requires: vec![],
        }
    }

    fn start(&self, _: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        host.contribute(&BROWSER_ADAPTERS, Arc::new(self.clone()))
    }
}

#[tokio::test]
async fn sources_from_all_browsers_and_reads_are_routed() {
    let (zen, chrome, log) = (
        MemoryBrowser::default(),
        MemoryBrowser::default(),
        MemoryLog::default(),
    );
    zen.push("zen/Default", [visit("", 1_000, "https://a/", "甲")]);
    chrome.push("chrome/Default", [visit("", 2_000, "https://b/", "乙")]);
    let app = testkit::app()
        .with(log.clone())
        .with(BrowserHistoryService)
        .with(zen)
        .with(Other(chrome))
        .with(Broken)
        .boot()
        .await
        .unwrap();
    let history = app.resolve(&BROWSER_HISTORY).unwrap();
    let ids: Vec<String> = history
        .sources()
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(ids, ["zen/Default", "chrome/Default"]);
    let page = history
        .read("chrome/Default".to_owned(), None, 10)
        .await
        .unwrap();
    assert_eq!(page.visits[0].url, "https://b/");
    let err = history.read("nope".to_owned(), None, 10).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);
    assert!(
        log.lines().iter().any(|l| l.contains("配置目录读不了")),
        "{:?}",
        log.lines()
    );
}
