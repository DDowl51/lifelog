//! `summary` 和真实的 `activity`、`query`、`documents`、内存库装在一起；文件和模型用假的。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use activity::ActivityComponent;
use activity_contracts::{ACTIVITY_WRITE, Activity};
use clock_contract::testing::FakeClock;
use documents::DocumentsComponent;
use documents_contracts::DOCUMENT_READ;
use files_contract::testing::MemoryFiles;
use kernel_api::{AppError, ErrorKind};
use llm_contract::Role;
use llm_contract::testing::ScriptedLlm;
use log_contract::testing::MemoryLog;
use query::QueryComponent;
use serde_json::{Value, json};
use storage_sqlite::StorageSqlite;
use summary::SummaryComponent;
use summary_contracts::{Outcome, SUMMARIZE, SummarizeRequest};

fn ts(s: &str) -> i64 {
    s.parse::<jiff::Timestamp>().unwrap().as_millisecond()
}

struct Fixture {
    app: testkit::TestApp,
    files: MemoryFiles,
    llm: ScriptedLlm,
}

/// 「现在」是上海时间 2026-10-20 12:00；19 日有 40 分钟的记录。
async fn boot(summary: Value) -> Fixture {
    let (files, llm) = (
        MemoryFiles::default(),
        ScriptedLlm::replying("## 概览\n写了 lifelog。"),
    );
    let app = testkit::app()
        .with(StorageSqlite)
        .config("storage-sqlite", json!({ "path": ":memory:" }))
        .unwrap()
        .with(FakeClock::at(ts("2026-10-20T04:00:00Z"), 480))
        .with(MemoryLog::default())
        .with(ActivityComponent)
        .with(QueryComponent)
        .with(DocumentsComponent)
        .with(files.clone())
        .with(llm.clone())
        .with(SummaryComponent)
        .config("summary", summary)
        .unwrap()
        .boot()
        .await
        .unwrap();
    app.resolve(&ACTIVITY_WRITE)
        .unwrap()
        .submit(vec![
            Activity::new("focus", "window_focus", ts("2026-10-19T02:00:00Z"), 480)
                .with_end(ts("2026-10-19T02:40:00Z"))
                .with_app("ghostty")
                .with_title("nvim lifelog")
                .with_project("lifelog"),
            Activity::new("shell", "command", ts("2026-10-19T02:10:00Z"), 480)
                .with_title("cargo test --workspace")
                .with_project("lifelog"),
        ])
        .await
        .unwrap();
    Fixture { app, files, llm }
}

fn request(date: Option<&str>) -> SummarizeRequest {
    SummarizeRequest {
        date: date.map(str::to_owned),
        force: false,
        dry_run: false,
    }
}

impl Fixture {
    async fn run(&self, request: SummarizeRequest) -> Result<Vec<(String, Outcome)>, AppError> {
        let result = self
            .app
            .resolve(&SUMMARIZE)
            .unwrap()
            .summarize(request)
            .await?;
        Ok(result
            .days
            .into_iter()
            .map(|d| (d.date, d.outcome))
            .collect())
    }
}

fn day(date: &str, outcome: Outcome) -> (String, Outcome) {
    (date.to_owned(), outcome)
}

#[tokio::test]
async fn catching_up_writes_the_file_and_the_document_once() {
    let f = boot(json!({})).await;
    assert_eq!(
        f.run(request(None)).await.unwrap(),
        [
            day("2026-10-17", Outcome::TooLittle),
            day("2026-10-18", Outcome::TooLittle),
            day("2026-10-19", Outcome::Written),
        ]
    );
    let file = f.files.get("daily/2026-10-19.md").unwrap();
    assert!(
        file.contains("# 2026-10-19 周一\n\n## 概览\n写了 lifelog。"),
        "{file}"
    );
    let doc = f
        .app
        .resolve(&DOCUMENT_READ)
        .unwrap()
        .get("summary_daily:2026-10-19".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(doc.document.body, "## 概览\n写了 lifelog。");
    assert_eq!(doc.document.origin.as_deref(), Some("daily/2026-10-19.md"));

    // 发给模型的：说明 + 这一天的原料
    let seen = f.llm.seen();
    assert_eq!(seen.len(), 1, "没内容的两天不调模型");
    assert_eq!(seen[0].messages[0].role, Role::System);
    let material = &seen[0].messages[1].content;
    assert!(material.contains("lifelog：40 分，1 条命令"), "{material}");
    assert!(material.contains("cargo test --workspace"), "{material}");

    // 再跑一次：已经有了，不再调模型
    assert_eq!(
        f.run(request(None)).await.unwrap()[2],
        day("2026-10-19", Outcome::Exists)
    );
    assert_eq!(f.llm.seen().len(), 1);
}

#[tokio::test]
async fn hand_edits_are_kept_unless_forced() {
    let f = boot(json!({})).await;
    f.run(request(Some("yesterday"))).await.unwrap();
    let edited = f
        .files
        .get("daily/2026-10-19.md")
        .unwrap()
        .replace("写了", "写完了");
    f.files.put("daily/2026-10-19.md", &edited, 0);

    assert_eq!(
        f.run(request(Some("yesterday"))).await.unwrap(),
        [day("2026-10-19", Outcome::KeptEdited)]
    );
    assert_eq!(f.files.get("daily/2026-10-19.md").unwrap(), edited);

    let forced = SummarizeRequest {
        force: true,
        ..request(Some("yesterday"))
    };
    assert_eq!(
        f.run(forced).await.unwrap(),
        [day("2026-10-19", Outcome::Written)]
    );
    assert!(
        !f.files
            .get("daily/2026-10-19.md")
            .unwrap()
            .contains("写完了")
    );
}

#[tokio::test]
async fn a_dry_run_only_shows_the_material() {
    let f = boot(json!({ "hidden_sources": ["shell"] })).await;
    let result = f
        .app
        .resolve(&SUMMARIZE)
        .unwrap()
        .summarize(SummarizeRequest {
            dry_run: true,
            ..request(Some("2026-10-19"))
        })
        .await
        .unwrap();
    let only = &result.days[0];
    assert_eq!(only.outcome, Outcome::DryRun);
    let material = only.digest.as_deref().unwrap();
    assert!(material.contains("nvim lifelog"), "{material}");
    assert!(
        !material.contains("cargo test"),
        "隐藏的来源不发：{material}"
    );
    assert!(f.llm.seen().is_empty(), "不调模型");
    assert_eq!(f.files.get("daily/2026-10-19.md"), None, "不写文件");
}

#[tokio::test]
async fn model_errors_fail_the_run_and_write_nothing() {
    let f = boot(json!({})).await;
    f.llm.set(Err(AppError::unavailable("没有配置 key")));
    let err = f.run(request(Some("yesterday"))).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unavailable);
    assert_eq!(f.files.get("daily/2026-10-19.md"), None);
}

#[tokio::test]
async fn the_path_needs_a_date() {
    let err = testkit::app()
        .with(StorageSqlite)
        .config("storage-sqlite", json!({ "path": ":memory:" }))
        .unwrap()
        .with(FakeClock::at(0, 0))
        .with(MemoryLog::default())
        .with(ActivityComponent)
        .with(QueryComponent)
        .with(DocumentsComponent)
        .with(MemoryFiles::default())
        .with(ScriptedLlm::replying("x"))
        .with(SummaryComponent)
        .config("summary", json!({ "path": "daily.md" }))
        .unwrap()
        .boot()
        .await
        .err()
        .unwrap();
    assert_eq!(err.kind, ErrorKind::Config);
}
