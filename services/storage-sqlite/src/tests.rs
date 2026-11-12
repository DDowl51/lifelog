use std::path::Path;
use std::sync::Arc;

use kernel_api::{AppError, Component, ConfigSection, ErrorKind, Host, Kind, Manifest};
use serde_json::json;
use sql_contract::testing::sql_contract;
use sql_contract::{MIGRATIONS, SQL, SqlValue, Statement, StaticMigrations};
use testkit::TestApp;

use super::*;

/// 一个只有迁移的假功能。
struct Tables {
    id: &'static str,
    steps: Vec<(&'static str, &'static str)>,
}

impl Component for Tables {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: self.id,
            kind: Kind::Feature,
            summary: "测试用：只登记迁移",
            provides: vec![],
            requires: vec![SQL.id(), MIGRATIONS.id()],
            optional_requires: vec![],
        }
    }

    fn start(&self, _: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        host.contribute(
            &MIGRATIONS,
            Arc::new(StaticMigrations::new(self.id, &self.steps)),
        )
    }
}

const NOTES_V1: (&str, &str) = (
    "notes table",
    "CREATE TABLE notes_item (id INTEGER PRIMARY KEY, body TEXT NOT NULL) STRICT;
     CREATE INDEX notes_item_body ON notes_item(body);",
);
const NOTES_V2: (&str, &str) = (
    "pinned",
    "ALTER TABLE notes_item ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0",
);

fn notes(steps: &[(&'static str, &'static str)]) -> Tables {
    Tables {
        id: "notes",
        steps: steps.to_vec(),
    }
}

async fn boot(path: &str, read_only: bool, tables: Vec<Tables>) -> Result<TestApp, AppError> {
    let mut app = testkit::app().with(StorageSqlite).config(
        "storage-sqlite",
        json!({ "path": path, "read_only": read_only }),
    )?;
    for t in tables {
        app = app.with(t);
    }
    app.boot().await
}

async fn memory(tables: Vec<Tables>) -> Result<TestApp, AppError> {
    boot(":memory:", false, tables).await
}

fn file(dir: &tempfile::TempDir) -> String {
    dir.path()
        .join("sub/dir/test.db")
        .to_string_lossy()
        .into_owned()
}

async fn count(app: &TestApp, table: &str) -> i64 {
    let rows = app
        .resolve(&SQL)
        .unwrap()
        .query(Statement::new(format!("SELECT count(*) FROM {table}")))
        .await
        .unwrap();
    rows.rows[0][0].as_i64().unwrap()
}

#[tokio::test]
async fn obeys_the_sql_contract() {
    let app = memory(vec![]).await.unwrap();
    sql_contract(app.resolve(&SQL).unwrap()).await.unwrap();
}

#[tokio::test]
async fn obeys_the_sql_contract_on_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir);
    let app = boot(&path, false, vec![]).await.unwrap();
    sql_contract(app.resolve(&SQL).unwrap()).await.unwrap();
    // 文件数据库用 WAL：读的进程不会被写的进程挡住
    assert!(Path::new(&format!("{path}-wal")).exists());
}

#[tokio::test]
async fn runs_contributed_migrations_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir);

    let app = boot(&path, false, vec![notes(&[NOTES_V1])]).await.unwrap();
    app.resolve(&SQL)
        .unwrap()
        .execute(vec![Statement::new(
            "INSERT INTO notes_item (body) VALUES ('a')",
        )])
        .await
        .unwrap();
    app.stop().await;

    // 重开：已执行的不再执行，新增的版本接着执行，数据还在
    let app = boot(&path, false, vec![notes(&[NOTES_V1, NOTES_V2])])
        .await
        .unwrap();
    assert_eq!(count(&app, "notes_item").await, 1);
    let sql = app.resolve(&SQL).unwrap();
    let pinned = sql
        .query(Statement::new("SELECT pinned FROM notes_item"))
        .await
        .unwrap();
    assert_eq!(pinned.rows[0][0], SqlValue::Integer(0));
    let log = sql
        .query(Statement::new(
            "SELECT component, version, name FROM _migrations ORDER BY version",
        ))
        .await
        .unwrap();
    assert_eq!(log.rows.len(), 2);
    assert_eq!(log.rows[1][2], SqlValue::Text("pinned".to_owned()));
}

#[tokio::test]
async fn refuses_to_start_when_an_applied_migration_was_edited() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir);
    boot(&path, false, vec![notes(&[NOTES_V1])])
        .await
        .unwrap()
        .stop()
        .await;

    let edited = (
        "notes table",
        "CREATE TABLE notes_item (id INTEGER PRIMARY KEY, body TEXT) STRICT;",
    );
    let err = boot(&path, false, vec![notes(&[edited])])
        .await
        .err()
        .unwrap();
    assert_eq!(err.kind, ErrorKind::Config);
    assert!(
        err.message.contains("`notes`") && err.message.contains("被改动"),
        "{err}"
    );
}

#[tokio::test]
async fn refuses_a_database_newer_than_the_program() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir);
    boot(&path, false, vec![notes(&[NOTES_V1, NOTES_V2])])
        .await
        .unwrap()
        .stop()
        .await;
    let err = boot(&path, false, vec![notes(&[NOTES_V1])])
        .await
        .err()
        .unwrap();
    assert!(err.message.contains("数据库比程序新"), "{err}");
}

#[tokio::test]
async fn a_failed_migration_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir);
    let broken = (
        "broken",
        "CREATE TABLE notes_extra (id INTEGER); INSERT INTO notes_nope VALUES (1);",
    );
    let err = boot(&path, false, vec![notes(&[NOTES_V1, broken])])
        .await
        .err()
        .unwrap();
    assert!(err.message.contains("迁移 2（broken）"), "{err}");

    // 第 1 步留下了，第 2 步整个回滚
    let app = boot(&path, false, vec![notes(&[NOTES_V1])]).await.unwrap();
    let sql = app.resolve(&SQL).unwrap();
    let tables = sql
        .query(Statement::new(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'notes_%'",
        ))
        .await
        .unwrap();
    assert_eq!(tables.rows, [[SqlValue::Text("notes_item".to_owned())]]);
}

#[tokio::test]
async fn migrations_may_only_touch_their_own_prefix() {
    let foreign_table = ("oops", "CREATE TABLE items (id INTEGER)");
    let err = memory(vec![notes(&[foreign_table])]).await.err().unwrap();
    assert_eq!(err.kind, ErrorKind::Config);
    assert!(
        err.message.contains("`items`") && err.message.contains("`notes_`"),
        "{err}"
    );

    // 也不能动别的组件的表（`alpha` 排在 `notes` 前面，它的表这时已经建好）
    let other = Tables {
        id: "alpha",
        steps: vec![("t", "CREATE TABLE alpha_item (id INTEGER)")],
    };
    let meddling = ("meddle", "DROP TABLE alpha_item");
    let err = memory(vec![notes(&[NOTES_V1, meddling]), other])
        .await
        .err()
        .unwrap();
    assert!(err.message.contains("`alpha_item`"), "{err}");

    // UNIQUE 约束引出的自动索引不算越界；自己起名的索引要带前缀
    let unique = Tables {
        id: "uniq",
        steps: vec![("t", "CREATE TABLE uniq_item (k TEXT UNIQUE)")],
    };
    memory(vec![unique]).await.unwrap();
    let stray_index = ("idx", "CREATE INDEX by_body ON notes_item(body)");
    let err = memory(vec![notes(&[NOTES_V1, stray_index])])
        .await
        .err()
        .unwrap();
    assert!(err.message.contains("`by_body`"), "{err}");

    // id 里的连字符在前缀里换成下划线
    let dashed = Tables {
        id: "sync-dida",
        steps: vec![("t", "CREATE TABLE sync_dida_task (id INTEGER)")],
    };
    memory(vec![dashed]).await.unwrap();
}

#[tokio::test]
async fn versions_must_be_contiguous() {
    struct Gap;
    impl Component for Gap {
        fn manifest(&self) -> Manifest {
            Manifest {
                id: "gap",
                kind: Kind::Feature,
                summary: "测试用",
                provides: vec![],
                requires: vec![MIGRATIONS.id()],
                optional_requires: vec![],
            }
        }
        fn start(&self, _: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
            let mut set = StaticMigrations::new("gap", &[("a", "SELECT 1"), ("b", "SELECT 1")]);
            set.0.migrations[1].version = 3;
            host.contribute(&MIGRATIONS, Arc::new(set))
        }
    }
    let err = testkit::app()
        .with(StorageSqlite)
        .config("storage-sqlite", json!({ "path": ":memory:" }))
        .unwrap()
        .with(Gap)
        .boot()
        .await
        .err()
        .unwrap();
    assert!(err.message.contains("连续递增"), "{err}");
}

#[tokio::test]
async fn read_only_reads_but_cannot_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir);
    let writer = boot(&path, false, vec![notes(&[NOTES_V1])]).await.unwrap();
    writer
        .resolve(&SQL)
        .unwrap()
        .execute(vec![Statement::new(
            "INSERT INTO notes_item (body) VALUES ('a')",
        )])
        .await
        .unwrap();

    // 写者还开着的时候，只读的进程也能读
    let reader = boot(&path, true, vec![notes(&[NOTES_V1])]).await.unwrap();
    assert_eq!(count(&reader, "notes_item").await, 1);
    let err = reader
        .resolve(&SQL)
        .unwrap()
        .execute(vec![Statement::new("DELETE FROM notes_item")])
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unauthorized);
    assert_eq!(count(&writer, "notes_item").await, 1);
}

/// 只读进程里存储有问题时：进程照常启动，问题在查询时报出来。
async fn read_only_query_error(path: &str, tables: Vec<Tables>) -> AppError {
    let app = boot(path, true, tables).await.unwrap();
    app.resolve(&SQL)
        .unwrap()
        .query(Statement::new("SELECT 1"))
        .await
        .unwrap_err()
}

#[tokio::test]
async fn read_only_reports_problems_at_query_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir);

    // 文件还不存在
    let err = read_only_query_error(&path, vec![notes(&[NOTES_V1])]).await;
    assert_eq!(err.kind, ErrorKind::Unavailable);
    assert!(err.message.contains("先启动那个进程"), "{err}");
    assert!(!Path::new(&path).exists(), "只读不能把文件建出来");

    // 结构落后
    boot(&path, false, vec![notes(&[NOTES_V1])])
        .await
        .unwrap()
        .stop()
        .await;
    let err = read_only_query_error(&path, vec![notes(&[NOTES_V1, NOTES_V2])]).await;
    assert_eq!(err.kind, ErrorKind::Unavailable);
    assert!(
        err.message.contains("迁移 2（pinned）") && err.message.contains("只读"),
        "{err}"
    );

    // 只读也要对校验和
    let edited = ("notes table", "CREATE TABLE notes_item (id INTEGER)");
    let err = read_only_query_error(&path, vec![notes(&[edited])]).await;
    assert_eq!(err.kind, ErrorKind::Config);
}

#[tokio::test]
async fn a_writable_process_still_fails_at_startup() {
    let dir = tempfile::tempdir().unwrap();
    // 路径的上一级是个文件，建不了目录
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, "").unwrap();
    let path = blocker.join("x.db").to_string_lossy().into_owned();
    let err = boot(&path, false, vec![]).await.err().unwrap();
    assert_eq!(err.kind, ErrorKind::Unavailable);
}

#[test]
fn config_defaults_and_paths() {
    assert_eq!(
        ConfigSection::default().parse::<Config>().unwrap(),
        Config::default()
    );
    assert!(
        ConfigSection::from_value(json!({ "pth": "x" }))
            .unwrap()
            .parse::<Config>()
            .is_err()
    );
    assert_eq!(resolve_path(Some(":memory:")).unwrap(), ":memory:");
    assert_eq!(resolve_path(Some("/tmp/x.db")).unwrap(), "/tmp/x.db");
    let home = std::env::var("HOME").unwrap();
    assert_eq!(
        resolve_path(Some("~/x.db")).unwrap(),
        format!("{home}/x.db")
    );
    assert!(resolve_path(None).unwrap().ends_with("/lifelog/lifelog.db"));
}
