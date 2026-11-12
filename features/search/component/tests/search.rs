//! `search` 和真实的 `activity`、SQLite（内存库）、分词器装在一起。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use activity::ActivityComponent;
use activity_contracts::{ACTIVITY_WRITE, Activity, payload_keys};
use clock_contract::testing::FakeClock;
use kernel_api::ErrorKind;
use log_contract::testing::MemoryLog;
use search::SearchComponent;
use search_contracts::{SEARCH, SearchQuery};
use serde_json::{Value, json};
use storage_sqlite::StorageSqlite;
use timer_contract::testing::ManualTimer;
use tokenizer_cjk::TokenizerCjk;

struct Fixture {
    app: testkit::TestApp,
    timer: ManualTimer,
}

async fn boot(config: Value) -> Fixture {
    let timer = ManualTimer::default();
    let app = testkit::app()
        .with(StorageSqlite)
        .config("storage-sqlite", json!({ "path": ":memory:" }))
        .unwrap()
        .with(FakeClock::at(1_000_000, 480))
        .with(MemoryLog::default())
        .with(timer.clone())
        .with(ActivityComponent)
        .with(TokenizerCjk)
        .with(SearchComponent)
        .config("search", config)
        .unwrap()
        .boot()
        .await
        .unwrap();
    Fixture { app, timer }
}

impl Fixture {
    async fn add(&self, activities: Vec<Activity>) {
        self.app
            .resolve(&ACTIVITY_WRITE)
            .unwrap()
            .submit(activities)
            .await
            .unwrap();
    }

    async fn index(&self) {
        self.timer.fire_all().await.unwrap();
        self.app.settle().await;
    }

    async fn find(&self, text: &str) -> Vec<String> {
        self.find_with(text, |_| {}).await
    }

    async fn find_with(&self, text: &str, tweak: impl FnOnce(&mut SearchQuery)) -> Vec<String> {
        let mut query = SearchQuery {
            text: text.to_owned(),
            since: None,
            until: None,
            sources: vec![],
            exclude_sources: vec![],
            limit: 10,
        };
        tweak(&mut query);
        let result = self
            .app
            .resolve(&SEARCH)
            .unwrap()
            .search(query)
            .await
            .unwrap();
        result
            .hits
            .into_iter()
            .map(|h| format!("{}:{}x{}", h.source, h.title.unwrap_or_default(), h.count))
            .collect()
    }
}

fn window(at: i64, title: &str) -> Activity {
    Activity::new("focus", "window_focus", at, 480)
        .with_end(at + 1_000)
        .with_title(title)
}

fn data() -> Vec<Activity> {
    vec![
        window(1_000, "jiff - Rust"),
        window(2_000, "气象站固件调试"),
        window(3_000, "jiff - Rust"),
        Activity::new("shell", "command", 4_000, 480).with_title("cargo add jiff"),
        Activity::new("browsing", "visit", 5_000, 480)
            .with_title("Timestamp in jiff")
            .with_payload(
                payload_keys::URL,
                "https://docs.rs/jiff/latest/jiff/struct.Timestamp.html",
            ),
        Activity::new("afk", "away", 6_000, 480).with_end(7_000),
    ]
}

#[tokio::test]
async fn finds_across_sources_newest_first_and_merges_repeats() {
    let f = boot(json!({})).await;
    f.add(data()).await;
    assert!(f.find("jiff").await.is_empty(), "建索引之前什么也找不到");
    f.index().await;
    assert_eq!(
        f.find("jiff").await,
        [
            "browsing:Timestamp in jiffx1",
            "shell:cargo add jiffx1",
            "focus:jiff - Rustx2",
        ]
    );
    assert_eq!(f.find("气象").await, ["focus:气象站固件调试x1"]);
    assert_eq!(
        f.find("站").await,
        ["focus:气象站固件调试x1"],
        "一个字也能找"
    );
    assert_eq!(f.find("ji").await.len(), 3, "最后一个词按前缀");
    assert_eq!(f.find("timestamp html").await.len(), 1, "网址里的词也算");
    assert!(f.find("jiff 气象").await.is_empty(), "每个词都要出现");
}

#[tokio::test]
async fn filters_by_time_and_source() {
    let f = boot(json!({})).await;
    f.add(data()).await;
    f.index().await;
    let shell_only = f
        .find_with("jiff", |q| q.sources = vec!["shell".to_owned()])
        .await;
    assert_eq!(shell_only, ["shell:cargo add jiffx1"]);
    let hidden = f
        .find_with("jiff", |q| {
            q.exclude_sources = vec!["focus".to_owned(), "browsing".to_owned()]
        })
        .await;
    assert_eq!(hidden, ["shell:cargo add jiffx1"]);
    let early = f
        .find_with("jiff", |q| {
            q.since = Some(0);
            q.until = Some(2_500);
        })
        .await;
    assert_eq!(early, ["focus:jiff - Rustx1"], "范围外的那一次不算");
}

#[tokio::test]
async fn indexes_in_batches_and_picks_up_new_activities() {
    let f = boot(json!({ "batch": 2, "max_batches": 1 })).await;
    f.add(data()).await;
    f.index().await;
    assert_eq!(
        f.find("jiff").await,
        ["focus:jiff - Rustx1"],
        "一轮只做了一批"
    );
    f.index().await;
    f.index().await;
    assert_eq!(f.find("jiff").await.len(), 3);
    f.add(vec![window(9_000, "jiff 新的")]).await;
    f.index().await;
    assert_eq!(f.find("新的").await, ["focus:jiff 新的x1"]);
}

#[tokio::test]
async fn many_repeats_do_not_crowd_out_other_hits() {
    let f = boot(json!({})).await;
    let mut many: Vec<Activity> = (0..1_200)
        .map(|i| window(10_000 + i, "jiff 窗口"))
        .collect();
    many.insert(0, window(1_000, "jiff 更早的"));
    f.add(many).await;
    f.index().await;
    let hits = f.find_with("jiff", |q| q.limit = 2).await;
    assert_eq!(hits, ["focus:jiff 窗口x1200", "focus:jiff 更早的x1"]);
}

#[tokio::test]
async fn nothing_to_search_for_is_a_validation_error() {
    let f = boot(json!({})).await;
    let err = f
        .app
        .resolve(&SEARCH)
        .unwrap()
        .search(SearchQuery {
            text: " — ".to_owned(),
            since: None,
            until: None,
            sources: vec![],
            exclude_sources: vec![],
            limit: 10,
        })
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Validation);
}

#[tokio::test]
async fn without_a_timer_it_only_searches() {
    // 只读的命令行进程里没有定时器
    let app = testkit::app()
        .with(StorageSqlite)
        .config("storage-sqlite", json!({ "path": ":memory:" }))
        .unwrap()
        .with(FakeClock::at(0, 0))
        .with(MemoryLog::default())
        .with(ActivityComponent)
        .with(TokenizerCjk)
        .with(SearchComponent)
        .boot()
        .await
        .unwrap();
    assert!(app.resolve(&SEARCH).is_ok());
}

// ---------- 文档（摘要、笔记） ----------

mod documents_and_vectors {
    use std::sync::{Arc, Mutex};

    use documents::DocumentsComponent;
    use documents_contracts::{DOCUMENT_WRITE, Document};
    use kernel_api::{AppError, Component, ConfigSection, Host, Kind, Manifest, async_trait};
    use vectors_contracts::{VECTOR_SEARCH, VectorHit, VectorQuery, VectorSearch};

    use super::*;

    /// 假的向量检索：按设定好的结果回答；`None` 表示出错。
    #[derive(Clone, Default)]
    struct FakeVectors(Arc<Mutex<Option<Vec<VectorHit>>>>);

    #[async_trait]
    impl VectorSearch for FakeVectors {
        async fn search(&self, _query: VectorQuery) -> Result<Vec<VectorHit>, AppError> {
            self.0
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| AppError::unavailable("常驻进程没在运行"))
        }
    }

    impl Component for FakeVectors {
        fn manifest(&self) -> Manifest {
            Manifest {
                id: "vectors-fake",
                kind: Kind::Feature,
                summary: "测试用",
                provides: vec![VECTOR_SEARCH.id()],
                requires: vec![],
                optional_requires: vec![],
            }
        }

        fn start(&self, _: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
            host.provide(&VECTOR_SEARCH, Arc::new(self.clone()))
        }
    }

    struct Docs {
        app: testkit::TestApp,
        timer: ManualTimer,
        vectors: FakeVectors,
    }

    async fn boot_docs() -> Docs {
        let (timer, vectors) = (ManualTimer::default(), FakeVectors::default());
        *vectors.0.lock().unwrap() = Some(vec![]);
        let app = testkit::app()
            .with(StorageSqlite)
            .config("storage-sqlite", json!({ "path": ":memory:" }))
            .unwrap()
            .with(FakeClock::at(1_000_000, 480))
            .with(MemoryLog::default())
            .with(timer.clone())
            .with(ActivityComponent)
            .with(DocumentsComponent)
            .with(vectors.clone())
            .with(TokenizerCjk)
            .with(SearchComponent)
            .boot()
            .await
            .unwrap();
        Docs {
            app,
            timer,
            vectors,
        }
    }

    fn summary(date: &str, body: &str) -> Document {
        Document {
            key: format!("summary_daily:{date}"),
            kind: "summary_daily".to_owned(),
            date: Some(date.to_owned()),
            title: format!("{date} 摘要"),
            body: body.to_owned(),
            origin: None,
        }
    }

    impl Docs {
        async fn put(&self, doc: Document) {
            self.app
                .resolve(&DOCUMENT_WRITE)
                .unwrap()
                .put(doc)
                .await
                .unwrap();
        }

        async fn index(&self) {
            self.timer.fire_all().await.unwrap();
            self.app.settle().await;
        }

        async fn find(
            &self,
            text: &str,
            tweak: impl FnOnce(&mut SearchQuery),
        ) -> Vec<(String, String)> {
            let mut query = SearchQuery {
                text: text.to_owned(),
                since: None,
                until: None,
                sources: vec![],
                exclude_sources: vec![],
                limit: 10,
            };
            tweak(&mut query);
            self.app
                .resolve(&SEARCH)
                .unwrap()
                .search(query)
                .await
                .unwrap()
                .documents
                .into_iter()
                .map(|d| (d.key, d.snippet))
                .collect()
        }
    }

    fn key(date: &str) -> String {
        format!("summary_daily:{date}")
    }

    #[tokio::test]
    async fn summaries_are_found_with_a_snippet_and_follow_changes() {
        let d = boot_docs().await;
        d.put(summary(
            "2026-11-12",
            "## 概览\n调试气象站固件，加入了 uucp 组。",
        ))
        .await;
        d.put(summary("2026-11-11", "## 概览\n写 lifelog 的检索。"))
            .await;
        d.index().await;
        let found = d.find("气象站", |_| {}).await;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, key("2026-11-12"));
        assert!(found[0].1.contains("气象站固件"), "{found:?}");

        // 改了：重新索引
        d.put(summary("2026-11-12", "## 概览\n其实在写 Kakoune 配置。"))
            .await;
        d.index().await;
        assert!(d.find("气象站", |_| {}).await.is_empty());
        assert_eq!(d.find("kakoune", |_| {}).await[0].0, key("2026-11-12"));

        // 删了：去掉
        d.app
            .resolve(&DOCUMENT_WRITE)
            .unwrap()
            .remove(key("2026-11-11"))
            .await
            .unwrap();
        d.index().await;
        assert!(d.find("lifelog", |_| {}).await.is_empty());

        // 来源过滤和隐藏
        assert!(
            d.find("kakoune", |q| q.sources = vec!["shell".to_owned()])
                .await
                .is_empty()
        );
        assert!(
            d.find("kakoune", |q| q.exclude_sources =
                vec!["documents".to_owned()])
                .await
                .is_empty()
        );
        assert_eq!(
            d.find("kakoune", |q| q.sources = vec!["documents".to_owned()])
                .await
                .len(),
            1
        );
        // 按日期过滤
        assert!(
            d.find("kakoune", |q| q.since = Some(1_794_528_000_000))
                .await
                .is_empty(),
            "11-13 起"
        );
    }

    #[tokio::test]
    async fn meaning_and_words_are_merged_and_vector_errors_fall_back() {
        let d = boot_docs().await;
        d.put(summary("2026-11-12", "## 概览\n调试气象站固件。"))
            .await;
        d.put(summary("2026-11-10", "## 概览\n研究 tokio 的任务调度。"))
            .await;
        d.index().await;
        // 「异步运行时」这几个字两篇都没有；向量说 11-10 那篇意思最近
        *d.vectors.0.lock().unwrap() = Some(vec![VectorHit {
            key: key("2026-11-10"),
            score: 0.9,
            chunk: "研究 tokio 的任务调度。".to_owned(),
        }]);
        let found = d.find("异步运行时", |_| {}).await;
        assert_eq!(
            found,
            [(key("2026-11-10"), "研究 tokio 的任务调度。".to_owned())]
        );

        // 向量检索出错：只按全文，不失败
        *d.vectors.0.lock().unwrap() = None;
        assert!(d.find("异步运行时", |_| {}).await.is_empty());
        assert_eq!(d.find("气象站", |_| {}).await.len(), 1);
    }
}
