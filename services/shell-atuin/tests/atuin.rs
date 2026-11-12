//! 对着一个按 atuin 的表结构建的临时库测。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use clock_contract::testing::FakeClock;
use kernel_api::ErrorKind;
use rusqlite::{Connection, params};
use serde_json::json;
use shell_atuin::ShellAtuin;
use shell_contract::testing::shell_history_contract;
use shell_contract::{SHELL_HISTORY, ShellCommand};

/// atuin 18 的 history 表。
const SCHEMA: &str = "
    CREATE TABLE history (
        id TEXT PRIMARY KEY,
        timestamp INTEGER NOT NULL,
        duration INTEGER NOT NULL,
        exit INTEGER NOT NULL,
        command TEXT NOT NULL,
        cwd TEXT NOT NULL,
        session TEXT NOT NULL,
        hostname TEXT NOT NULL,
        deleted_at INTEGER,
        UNIQUE(timestamp, cwd, command)
    );";

fn host() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .unwrap()
        .trim()
        .to_owned()
}

fn insert(db: &Path, c: &ShellCommand, hostname: &str) {
    let conn = Connection::open(db).unwrap();
    conn.execute(
        "INSERT INTO history (id, timestamp, duration, exit, command, cwd, session, hostname)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            c.id,
            c.started_at * 1_000_000,
            c.duration_ms.map_or(-1, |d| d * 1_000_000),
            c.exit.unwrap_or(-1),
            c.command,
            c.cwd.clone().unwrap_or_else(|| "unknown".to_owned()),
            c.session.clone().unwrap_or_default(),
            hostname,
        ],
    )
    .unwrap();
}

async fn boot(db: &Path, now_ms: i64) -> testkit::TestApp {
    testkit::app()
        .with(FakeClock::at(now_ms, 540))
        .with(ShellAtuin)
        .config("shell-atuin", json!({ "path": db }))
        .unwrap()
        .boot()
        .await
        .unwrap()
}

fn fresh() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("history.db");
    Connection::open(&db)
        .unwrap()
        .execute_batch(SCHEMA)
        .unwrap();
    (dir, db)
}

#[tokio::test]
async fn passes_the_contract() {
    let (_dir, db) = fresh();
    let app = boot(&db, 1_000_000).await;
    let history = app.resolve(&SHELL_HISTORY).unwrap();
    let me = format!("{}:me", host());
    shell_history_contract(history, |commands| {
        for c in &commands {
            insert(&db, c, &me);
        }
        async {}
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn other_hosts_deleted_and_running_commands_are_left_out() {
    let (_dir, db) = fresh();
    let me = format!("{}:me", host());
    let finished = shell_contract::testing::finished;
    insert(&db, &finished("mine", 1_000, "ls"), &me);
    insert(&db, &finished("theirs", 2_000, "ls"), "other-box:me");
    insert(&db, &finished("gone", 3_000, "secret"), &me);
    Connection::open(&db)
        .unwrap()
        .execute("UPDATE history SET deleted_at = 1 WHERE id = 'gone'", [])
        .unwrap();
    let running = ShellCommand {
        duration_ms: None,
        exit: None,
        ..finished("running", 4_000, "cargo build")
    };
    insert(&db, &running, &me);

    let app = boot(&db, 5_000).await;
    let history = app.resolve(&SHELL_HISTORY).unwrap();
    let page = history.read(None, 10).await.unwrap();
    let ids: Vec<&str> = page.commands.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["mine"]);
    // 游标停在还在执行的命令前面：它结束后能读到
    Connection::open(&db)
        .unwrap()
        .execute(
            "UPDATE history SET duration = 7000000000, exit = 0 WHERE id = 'running'",
            [],
        )
        .unwrap();
    let later = history.read(Some(page.next), 10).await.unwrap();
    let ids: Vec<&str> = later.commands.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["running"]);
    assert_eq!(later.commands[0].duration_ms, Some(7_000));
}

#[tokio::test]
async fn a_missing_database_is_unavailable_not_a_startup_error() {
    let dir = tempfile::tempdir().unwrap();
    let app = boot(&dir.path().join("nope.db"), 0).await;
    let err = app
        .resolve(&SHELL_HISTORY)
        .unwrap()
        .read(None, 10)
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unavailable);
    assert!(
        err.message.contains("nope.db") && err.message.contains("path"),
        "{err}"
    );
}

#[tokio::test]
async fn commands_sharing_a_timestamp_are_not_lost_between_pages() {
    let (_dir, db) = fresh();
    let me = format!("{}:me", host());
    // 从别的历史导入的：时间戳全一样
    for i in 0..7 {
        insert(
            &db,
            &shell_contract::testing::finished(&format!("id{i}"), 5_000, &format!("cmd {i}")),
            &me,
        );
    }
    let app = boot(&db, 10_000).await;
    let history = app.resolve(&SHELL_HISTORY).unwrap();
    let mut cursor = None;
    let mut seen = Vec::new();
    loop {
        let page = history.read(cursor.clone(), 3).await.unwrap();
        seen.extend(page.commands.into_iter().map(|c| c.id));
        cursor = Some(page.next);
        if !page.more {
            break;
        }
    }
    assert_eq!(seen, (0..7).map(|i| format!("id{i}")).collect::<Vec<_>>());
}
