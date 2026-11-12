//! 复杂档的验收：`Sql` 的契约测试经过跨进程代理再跑一遍。
//!
//! 一个 kernel 导出存储服务，另一个 kernel 用 `Import` 导入，中间是真的 Unix socket。
//! 契约测试拿到的是代理，不知道对面在另一个「进程」里。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use kernel_api::Import;
use serde_json::json;
use sql_contract::SQL;
use sql_contract::testing::sql_contract;
use storage_sqlite::StorageSqlite;

#[tokio::test]
async fn obeys_the_sql_contract_through_a_remote_proxy() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("daemon.sock");
    let daemon = testkit::app()
        .with(StorageSqlite)
        .config("storage-sqlite", json!({ "path": ":memory:" }))
        .unwrap()
        .export(&socket, &["sql"])
        .boot()
        .await
        .unwrap();
    let cli = testkit::app()
        .with(Import::new("sql-remote", "转给另一个进程", &SQL))
        .connect(&socket)
        .boot()
        .await
        .unwrap();
    sql_contract(cli.resolve(&SQL).unwrap()).await.unwrap();
    cli.stop().await;
    daemon.stop().await;
}
