//! 对着按 Firefox 表结构建的临时 profile 测。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};

use browser_contract::BrowserAdapter;
use browser_contract::testing::{AdapterProbe, browser_adapter_contract};
use browser_firefox::BrowserFirefox;
use rusqlite::{Connection, params};
use serde_json::json;
use std::sync::Arc;

const SCHEMA: &str = "
    CREATE TABLE moz_places (id INTEGER PRIMARY KEY, url LONGVARCHAR, title LONGVARCHAR);
    CREATE TABLE moz_historyvisits (id INTEGER PRIMARY KEY, from_visit INTEGER, place_id INTEGER,
                                    visit_date INTEGER, visit_type INTEGER, session INTEGER);";

fn setup(root: &Path) -> std::path::PathBuf {
    std::fs::write(
        root.join("profiles.ini"),
        "[Profile0]\nName=Default (release)\nIsRelative=1\nPath=abc.Default (release)\n\n\
         [Profile1]\nName=empty\nIsRelative=1\nPath=nothing-here\n\n[General]\nVersion=2\n",
    )
    .unwrap();
    let profile = root.join("abc.Default (release)");
    std::fs::create_dir_all(&profile).unwrap();
    let db = profile.join("places.sqlite");
    Connection::open(&db)
        .unwrap()
        .execute_batch(SCHEMA)
        .unwrap();
    db
}

fn insert(db: &Path, at_ms: i64, url: &str, title: Option<&str>, visit_type: i64) {
    let conn = Connection::open(db).unwrap();
    conn.execute(
        "INSERT INTO moz_places (url, title) VALUES (?1, ?2)",
        params![url, title],
    )
    .unwrap();
    let place = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO moz_historyvisits (place_id, visit_date, visit_type) VALUES (?1, ?2, ?3)",
        params![place, at_ms * 1_000, visit_type],
    )
    .unwrap();
}

async fn boot(root: &Path) -> (testkit::TestApp, Arc<dyn BrowserAdapter>) {
    let probe = AdapterProbe::default();
    let app = testkit::app()
        .with(probe.clone())
        .with(BrowserFirefox)
        .config(
            "browser-firefox",
            json!({ "installs": [
                { "name": "zen", "dir": root },
                { "name": "firefox", "dir": root.join("not-installed") },
            ] }),
        )
        .unwrap()
        .boot()
        .await
        .unwrap();
    let history = probe.all().pop().unwrap();
    (app, history)
}

#[tokio::test]
async fn passes_the_contract() {
    let dir = tempfile::tempdir().unwrap();
    let db = setup(dir.path());
    let (_app, history) = boot(dir.path()).await;
    let sources = history.sources().await.unwrap();
    assert_eq!(
        sources.len(),
        1,
        "没有 places.sqlite 的 profile、没装的浏览器都不算：{sources:?}"
    );
    assert_eq!(sources[0].profile.as_deref(), Some("Default (release)"));
    browser_adapter_contract(history, "zen/Default (release)", |visits| {
        for v in &visits {
            insert(&db, v.at, &v.url, v.title.as_deref(), 1);
        }
        async {}
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn embeds_downloads_and_reloads_are_left_out() {
    let dir = tempfile::tempdir().unwrap();
    let db = setup(dir.path());
    let n = AtomicI64::new(1_000);
    for (url, kind) in [
        ("https://page/", 1),
        ("https://ad/", 4),
        ("https://file.zip", 7),
        ("https://page/", 9),
        ("https://typed/", 2),
    ] {
        insert(&db, n.fetch_add(1_000, Ordering::Relaxed), url, None, kind);
    }
    let (_app, history) = boot(dir.path()).await;
    let page = history
        .read("zen/Default (release)".to_owned(), None, 10)
        .await
        .unwrap();
    let urls: Vec<&str> = page.visits.iter().map(|v| v.url.as_str()).collect();
    assert_eq!(urls, ["https://page/", "https://typed/"]);
    assert_eq!(page.visits[0].title, None);
}
