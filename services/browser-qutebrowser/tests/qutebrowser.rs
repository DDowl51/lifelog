//! 对着按 qutebrowser 表结构建的临时目录测。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use browser_contract::BrowserAdapter;
use browser_contract::testing::{AdapterProbe, browser_adapter_contract};
use browser_qutebrowser::BrowserQutebrowser;
use rusqlite::{Connection, params};
use serde_json::json;

const SCHEMA: &str =
    "CREATE TABLE History (url NOT NULL, title NOT NULL, atime NOT NULL, redirect NOT NULL);";

fn make(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let db = dir.join("history.sqlite");
    Connection::open(&db)
        .unwrap()
        .execute_batch(SCHEMA)
        .unwrap();
    db
}

fn insert(db: &Path, at_ms: i64, url: &str, title: &str, redirect: bool) {
    Connection::open(db)
        .unwrap()
        .execute(
            "INSERT INTO History VALUES (?1, ?2, ?3, ?4)",
            params![url, title, at_ms / 1_000, redirect],
        )
        .unwrap();
}

async fn boot(root: &Path) -> (testkit::TestApp, Arc<dyn BrowserAdapter>) {
    let probe = AdapterProbe::default();
    let app = testkit::app()
        .with(probe.clone())
        .with(BrowserQutebrowser)
        .config(
            "browser-qutebrowser",
            json!({
                "data_dir": root.join("main"),
                "basedirs": [root.join("profiles"), root.join("missing")],
            }),
        )
        .unwrap()
        .boot()
        .await
        .unwrap();
    let adapter = probe.all().pop().unwrap();
    (app, adapter)
}

#[tokio::test]
async fn the_main_instance_and_per_project_instances_are_sources() {
    let dir = tempfile::tempdir().unwrap();
    let main = make(&dir.path().join("main"));
    make(&dir.path().join("profiles/lifelog/data"));
    std::fs::create_dir_all(dir.path().join("profiles/no-history")).unwrap();
    let (_app, adapter) = boot(dir.path()).await;
    let brief: Vec<(String, Option<String>)> = adapter
        .sources()
        .await
        .unwrap()
        .into_iter()
        .map(|s| (s.id, s.profile))
        .collect();
    assert_eq!(
        brief,
        [
            ("qutebrowser".to_owned(), None),
            ("qutebrowser/lifelog".to_owned(), Some("lifelog".to_owned()))
        ]
    );
    browser_adapter_contract(adapter, "qutebrowser", |visits| {
        for v in &visits {
            insert(&main, v.at, &v.url, v.title.as_deref().unwrap_or(""), false);
        }
        async {}
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn redirects_are_left_out() {
    let dir = tempfile::tempdir().unwrap();
    let db = make(&dir.path().join("profiles/p/data"));
    insert(&db, 1_000, "http://short/", "", true);
    insert(&db, 1_000, "https://long/", "长", false);
    let (_app, adapter) = boot(dir.path()).await;
    let page = adapter
        .read("qutebrowser/p".to_owned(), None, 10)
        .await
        .unwrap();
    let urls: Vec<&str> = page.visits.iter().map(|v| v.url.as_str()).collect();
    assert_eq!(urls, ["https://long/"]);
}
