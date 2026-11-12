#![allow(clippy::unwrap_used, clippy::expect_used)]

use clock_contract::testing::FakeClock;
use documents::DocumentsComponent;
use documents_contracts::{DOCUMENT_READ, DOCUMENT_WRITE, Document};
use kernel_api::ErrorKind;
use log_contract::testing::MemoryLog;
use serde_json::json;
use storage_sqlite::StorageSqlite;

fn doc(key: &str, date: &str, body: &str) -> Document {
    Document {
        key: key.to_owned(),
        kind: "summary_daily".to_owned(),
        date: Some(date.to_owned()),
        title: format!("{date} 摘要"),
        body: body.to_owned(),
        origin: Some(format!("daily/{date}.md")),
    }
}

#[tokio::test]
async fn put_overwrite_remove_and_changes() {
    let clock = FakeClock::at(1_000, 480);
    let app = testkit::app()
        .with(StorageSqlite)
        .config("storage-sqlite", json!({ "path": ":memory:" }))
        .unwrap()
        .with(clock.clone())
        .with(MemoryLog::default())
        .with(DocumentsComponent)
        .boot()
        .await
        .unwrap();
    let (write, read) = (
        app.resolve(&DOCUMENT_WRITE).unwrap(),
        app.resolve(&DOCUMENT_READ).unwrap(),
    );

    let a = write
        .put(doc("s:12", "2026-11-12", "写了 lifelog"))
        .await
        .unwrap();
    write
        .put(doc("s:11", "2026-11-11", "前一天"))
        .await
        .unwrap();
    clock.set(2_000);
    let again = write.put(doc("s:12", "2026-11-12", "改过")).await.unwrap();
    assert_eq!(a, again, "同一个键覆盖，编号不变");

    let got = read.get("s:12".to_owned()).await.unwrap().unwrap();
    assert_eq!(
        (got.document.body.as_str(), got.updated_at, got.revision),
        ("改过", 2_000, 3)
    );
    let dates: Vec<Option<String>> = read
        .list("summary_daily".to_owned())
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.document.date)
        .collect();
    assert_eq!(
        dates,
        [Some("2026-11-11".to_owned()), Some("2026-11-12".to_owned())]
    );

    assert!(write.remove("s:11".to_owned()).await.unwrap());
    assert!(!write.remove("s:11".to_owned()).await.unwrap(), "已经删了");
    assert_eq!(read.get("s:11".to_owned()).await.unwrap(), None);

    let changes: Vec<(String, i64, bool)> = read
        .changed_since(1, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|d| (d.document.key, d.revision, d.deleted))
        .collect();
    assert_eq!(
        changes,
        [("s:12".to_owned(), 3, false), ("s:11".to_owned(), 4, true)],
        "按修订号；删掉的也给"
    );

    let err = write.put(doc("", "2026-11-12", "x")).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Validation);
}
