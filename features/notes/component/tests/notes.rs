#![allow(clippy::unwrap_used, clippy::expect_used)]

use clock_contract::testing::FakeClock;
use documents::DocumentsComponent;
use documents_contracts::DOCUMENT_READ;
use files_contract::testing::MemoryFiles;
use log_contract::testing::MemoryLog;
use notes::NotesComponent;
use serde_json::json;
use storage_sqlite::StorageSqlite;
use timer_contract::testing::ManualTimer;

#[tokio::test]
async fn notes_and_edited_summaries_follow_the_files() {
    let (files, timer) = (MemoryFiles::default(), ManualTimer::default());
    let app = testkit::app()
        .with(StorageSqlite)
        .config("storage-sqlite", json!({ "path": ":memory:" }))
        .unwrap()
        .with(FakeClock::at(1_000, 480))
        .with(MemoryLog::default())
        .with(timer.clone())
        .with(files.clone())
        .with(DocumentsComponent)
        .with(NotesComponent)
        .boot()
        .await
        .unwrap();
    let read = app.resolve(&DOCUMENT_READ).unwrap();
    let sync = || async {
        timer.fire_all().await.unwrap();
        app.settle().await;
    };
    let revision = |key: &'static str| {
        let read = read.clone();
        async move {
            read.get(key.to_owned())
                .await
                .unwrap()
                .map(|d| (d.revision, d.document.body))
        }
    };

    files.put("ideas/气象站.md", "# 气象站\n想做一个玄关的。", 1);
    files.put(
        "daily/2026-11-12.md",
        "<!-- lifelog: hash=x -->\n# 2026-11-12 周四\n\n手改的摘要。",
        1,
    );
    files.put("ideas/readme.txt", "不是 md", 1);
    sync().await;
    assert_eq!(
        revision("note:ideas/气象站.md").await.unwrap().1,
        "想做一个玄关的。"
    );
    assert_eq!(
        revision("summary_daily:2026-11-12").await.unwrap().1,
        "手改的摘要。"
    );
    assert_eq!(
        read.list("note".to_owned()).await.unwrap().len(),
        1,
        "只收 .md"
    );

    // 没变：不重写（修订号不变）
    let before = revision("note:ideas/气象站.md").await.unwrap().0;
    sync().await;
    assert_eq!(revision("note:ideas/气象站.md").await.unwrap().0, before);

    // 改了、删了
    files.put("ideas/气象站.md", "# 气象站\n改成放书桌上。", 2);
    use files_contract::Files;
    files
        .remove("daily/2026-11-12.md".to_owned())
        .await
        .unwrap();
    sync().await;
    assert_eq!(
        revision("note:ideas/气象站.md").await.unwrap().1,
        "改成放书桌上。"
    );
    assert_eq!(
        revision("summary_daily:2026-11-12").await,
        None,
        "文件删了，文档也删"
    );
}
