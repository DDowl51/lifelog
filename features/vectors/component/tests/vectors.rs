//! `vectors` 和真实的 `documents`、内存库装在一起，嵌入用按字符哈希的假模型。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use clock_contract::testing::FakeClock;
use documents::DocumentsComponent;
use documents_contracts::{DOCUMENT_WRITE, Document};
use embedder_contract::testing::HashEmbedder;
use log_contract::testing::MemoryLog;
use serde_json::json;
use storage_sqlite::StorageSqlite;
use timer_contract::testing::ManualTimer;
use vectors::VectorsComponent;
use vectors_contracts::{VECTOR_SEARCH, VectorQuery};

fn doc(key: &str, body: &str) -> Document {
    Document {
        key: key.to_owned(),
        kind: "summary_daily".to_owned(),
        date: None,
        title: String::new(),
        body: body.to_owned(),
        origin: None,
    }
}

#[tokio::test]
async fn documents_are_found_by_their_closest_chunk_and_follow_changes() {
    let timer = ManualTimer::default();
    let app = testkit::app()
        .with(StorageSqlite)
        .config("storage-sqlite", json!({ "path": ":memory:" }))
        .unwrap()
        .with(FakeClock::at(1_000, 480))
        .with(MemoryLog::default())
        .with(timer.clone())
        .with(DocumentsComponent)
        .with(HashEmbedder)
        .with(VectorsComponent)
        .config("vectors", json!({ "min_score": 0.5, "chunk_chars": 40 }))
        .unwrap()
        .boot()
        .await
        .unwrap();
    let write = app.resolve(&DOCUMENT_WRITE).unwrap();
    write
        .put(doc(
            "a",
            "气象站固件调试\n\nkakoune configuration and plugins",
        ))
        .await
        .unwrap();
    write
        .put(doc("b", "完全无关的内容：做饭买菜"))
        .await
        .unwrap();
    let index = || async {
        timer.fire_all().await.unwrap();
        app.settle().await;
    };
    index().await;

    let search = app.resolve(&VECTOR_SEARCH).unwrap();
    let find = |text: &str| {
        let search = search.clone();
        let text = text.to_owned();
        async move {
            search
                .search(VectorQuery { text, limit: 5 })
                .await
                .unwrap()
                .into_iter()
                .map(|h| (h.key, h.chunk))
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(
        find("kakoune plugins configuration").await.first().cloned(),
        Some((
            "a".to_owned(),
            "kakoune configuration and plugins".to_owned()
        )),
        "每篇只给最相近的那一块"
    );

    // 删了：不再出现
    write.remove("a".to_owned()).await.unwrap();
    index().await;
    assert!(
        find("kakoune plugins configuration")
            .await
            .iter()
            .all(|(k, _)| k != "a")
    );
    assert!(find("  ").await.is_empty());
}
