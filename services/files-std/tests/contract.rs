#![allow(clippy::unwrap_used, clippy::expect_used)]

use files_contract::FILES;
use files_contract::testing::files_contract;
use files_std::FilesStd;
use kernel_api::ErrorKind;
use serde_json::json;

#[tokio::test]
async fn passes_the_contract_on_a_real_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("lifelog");
    let app = testkit::app()
        .with(FilesStd)
        .config("files-std", json!({ "root": root }))
        .unwrap()
        .boot()
        .await
        .unwrap();
    files_contract(app.resolve(&FILES).unwrap()).await.unwrap();
    let left: Vec<String> = std::fs::read_dir(root.join("daily"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp-"))
        .collect();
    assert!(left.is_empty(), "不留临时文件：{left:?}");
}

#[tokio::test]
async fn a_relative_root_is_a_config_error() {
    let err = testkit::app()
        .with(FilesStd)
        .config("files-std", json!({ "root": "lifelog" }))
        .unwrap()
        .boot()
        .await
        .err()
        .unwrap();
    assert_eq!(err.kind, ErrorKind::Config);
}
