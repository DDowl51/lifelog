//! `browsing` 和真实的 `browser-history`、内存库装在一起，浏览器用假的。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use activity_contracts::testing::RecordingWrite;
use browser_contract::testing::{MemoryBrowser, visit};
use browser_history::BrowserHistoryService;
use browsing::BrowsingComponent;
use clock_contract::testing::FakeClock;
use log_contract::testing::MemoryLog;
use serde_json::{Value, json};
use storage_sqlite::StorageSqlite;
use timer_contract::testing::ManualTimer;

struct Fixture {
    app: testkit::TestApp,
    timer: ManualTimer,
    recorded: RecordingWrite,
}

async fn boot(browser: MemoryBrowser, config: Value) -> Fixture {
    let (timer, recorded) = (ManualTimer::default(), RecordingWrite::default());
    let app = testkit::app()
        .with(StorageSqlite)
        .config("storage-sqlite", json!({ "path": ":memory:" }))
        .unwrap()
        .with(FakeClock::at(1_000_000, 540))
        .with(MemoryLog::default())
        .with(timer.clone())
        .with(recorded.clone())
        .with(BrowserHistoryService)
        .with(browser)
        .with(BrowsingComponent)
        .config("browsing", config)
        .unwrap()
        .boot()
        .await
        .unwrap();
    Fixture {
        app,
        timer,
        recorded,
    }
}

impl Fixture {
    async fn tick(&self) {
        self.timer.fire_all().await.unwrap();
        self.app.settle().await;
    }

    fn keys(&self) -> Vec<String> {
        self.recorded
            .submitted()
            .into_iter()
            .map(|a| a.dedup_key.unwrap())
            .collect()
    }
}

#[tokio::test]
async fn every_source_is_imported_with_its_own_cursor() {
    let browser = MemoryBrowser::default();
    browser.push(
        "zen/Default",
        (1..=3).map(|i| visit("", i * 1_000, &format!("https://z/{i}"), "z")),
    );
    browser.push(
        "qutebrowser/lifelog",
        [
            visit("", 500, "https://q/", "q"),
            visit("", 600, "about:blank", ""),
        ],
    );
    let f = boot(browser.clone(), json!({ "batch": 2 })).await;
    f.tick().await;
    let mut keys = f.keys();
    keys.sort();
    assert_eq!(
        keys,
        [
            "browsing:qutebrowser/lifelog:1",
            "browsing:zen/Default:1",
            "browsing:zen/Default:2",
            "browsing:zen/Default:3",
        ],
        "内部页面不导入"
    );
    let q = f
        .recorded
        .submitted()
        .into_iter()
        .find(|a| a.app.as_deref() == Some("qutebrowser"))
        .unwrap();
    assert_eq!(q.payload["profile"], json!("lifelog"));

    // 下一轮只要新的
    browser.push("zen/Default", [visit("", 9_000, "https://z/new", "新")]);
    f.tick().await;
    assert_eq!(f.recorded.submitted().len(), 5, "没有重复提交");
}
