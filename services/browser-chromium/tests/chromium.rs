//! 对着按 Chrome 表结构建的临时配置目录测。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use browser_chromium::BrowserChromium;
use browser_contract::BrowserAdapter;
use browser_contract::testing::{AdapterProbe, browser_adapter_contract};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

const SCHEMA: &str = "
    CREATE TABLE urls (id INTEGER PRIMARY KEY, url LONGVARCHAR, title LONGVARCHAR);
    CREATE TABLE visits (id INTEGER PRIMARY KEY, url INTEGER NOT NULL, visit_time INTEGER NOT NULL,
                         from_visit INTEGER, transition INTEGER DEFAULT 0 NOT NULL);";

/// 两个 profile：`Default`（显示名「工作」）和 `Profile 3`（显示名「别人」）。返回 `Default` 的库。
fn setup(root: &Path) -> PathBuf {
    std::fs::write(
        root.join("Local State"),
        json!({ "profile": { "info_cache": {
            "Default": { "name": "工作" },
            "Profile 3": { "name": "别人" },
            "Profile 9": { "name": "没有历史" },
        } } })
        .to_string(),
    )
    .unwrap();
    for dir in ["Default", "Profile 3"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
        Connection::open(root.join(dir).join("History"))
            .unwrap()
            .execute_batch(SCHEMA)
            .unwrap();
    }
    root.join("Default/History")
}

fn insert(db: &Path, at_ms: i64, url: &str, title: Option<&str>, transition: i64) {
    let conn = Connection::open(db).unwrap();
    conn.execute(
        "INSERT INTO urls (url, title) VALUES (?1, ?2)",
        params![url, title],
    )
    .unwrap();
    let id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO visits (url, visit_time, transition) VALUES (?1, ?2, ?3)",
        params![id, (at_ms + 11_644_473_600_000) * 1_000, transition],
    )
    .unwrap();
}

async fn boot(root: &Path, extra: Value) -> (testkit::TestApp, Arc<dyn BrowserAdapter>) {
    let probe = AdapterProbe::default();
    let mut config = json!({ "installs": [
        { "name": "chrome", "dir": root },
        { "name": "chromium", "dir": root.join("not-installed") },
    ] });
    if let (Some(c), Some(e)) = (config.as_object_mut(), extra.as_object()) {
        c.extend(e.clone());
    }
    let app = testkit::app()
        .with(probe.clone())
        .with(BrowserChromium)
        .config("browser-chromium", config)
        .unwrap()
        .boot()
        .await
        .unwrap();
    let adapter = probe.all().pop().unwrap();
    (app, adapter)
}

#[tokio::test]
async fn passes_the_contract() {
    let dir = tempfile::tempdir().unwrap();
    let db = setup(dir.path());
    let (_app, adapter) = boot(dir.path(), json!({})).await;
    let sources = adapter.sources().await.unwrap();
    let brief: Vec<(&str, Option<&str>)> = sources
        .iter()
        .map(|s| (s.id.as_str(), s.profile.as_deref()))
        .collect();
    assert_eq!(
        brief,
        [
            ("chrome/Default", Some("工作")),
            ("chrome/Profile 3", Some("别人"))
        ],
        "id 用目录名，profile 用显示名；没有历史的 profile 不算"
    );
    browser_adapter_contract(adapter, "chrome/Default", |visits| {
        for v in &visits {
            insert(&db, v.at, &v.url, v.title.as_deref(), 0);
        }
        async {}
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn subframes_are_left_out_and_profiles_can_be_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let db = setup(dir.path());
    insert(&db, 1_000, "https://page/", Some("页"), 0x3000_0000 | 1);
    insert(&db, 2_000, "https://frame/", None, 3);
    insert(&db, 3_000, "https://frame2/", None, 4);
    let (_app, adapter) = boot(dir.path(), json!({ "skip_profiles": ["别人"] })).await;
    let ids: Vec<String> = adapter
        .sources()
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(ids, ["chrome/Default"], "按显示名跳过");
    let page = adapter
        .read("chrome/Default".to_owned(), None, 10)
        .await
        .unwrap();
    let urls: Vec<&str> = page.visits.iter().map(|v| v.url.as_str()).collect();
    assert_eq!(urls, ["https://page/"], "高位的限定标志不影响判断");
    assert_eq!(page.visits[0].at, 1_000);
}
