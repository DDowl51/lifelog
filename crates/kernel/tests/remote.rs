//! 跨进程调用：一个 kernel 导出契约，另一个 kernel 用 `Import` 导入，中间是真的 Unix socket。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use kernel::{Delivery, Export, Options, Running};
use kernel_api::{
    AppError, Component, ConfigSection, ErrorKind, Host, Import, Kind, Manifest, async_trait,
    contract,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Greeting {
    pub text: String,
    pub times: u32,
}

contract! {
    pub trait Greeter as GREETER = service("greeter", 1) {
        async fn greet(&self, name: String, times: u32) -> Result<Greeting, AppError>;
        async fn fail(&self, kind: ErrorKind) -> Result<(), AppError>;
        async fn count(&self) -> Result<u32, AppError>;
    }
}

/// 同名、版本不同的契约（另一次构建的程序）。
mod v2 {
    use kernel_api::{AppError, contract};

    contract! {
        pub trait Greeter as GREETER = service("greeter", 2) {
            async fn count(&self) -> Result<u32, AppError>;
        }
    }
}

contract! {
    pub trait Other as OTHER = service("other", 1) {
        async fn ping(&self) -> Result<String, AppError>;
    }
}

struct Real;

#[async_trait]
impl Greeter for Real {
    async fn greet(&self, name: String, times: u32) -> Result<Greeting, AppError> {
        Ok(Greeting {
            text: format!("你好，{name}"),
            times,
        })
    }

    async fn fail(&self, kind: ErrorKind) -> Result<(), AppError> {
        Err(AppError::new(kind, "故意的"))
    }

    async fn count(&self) -> Result<u32, AppError> {
        Ok(42)
    }
}

#[async_trait]
impl Other for Real {
    async fn ping(&self) -> Result<String, AppError> {
        Ok("pong".to_owned())
    }
}

/// 提供 `greeter` 和 `other` 的组件。
struct Provider;

impl Component for Provider {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: "greeter-real",
            kind: Kind::Feature,
            summary: "测试用",
            provides: vec![GREETER.id(), OTHER.id()],
            requires: vec![],
            optional_requires: vec![],
        }
    }

    fn start(&self, _: &ConfigSection, host: &mut Host<'_>) -> Result<(), AppError> {
        host.provide(&GREETER, Arc::new(Real))?;
        host.provide(&OTHER, Arc::new(Real))
    }
}

fn options(export: Option<Export>, connect: Option<&Path>) -> Options {
    Options {
        trace: None,
        faults: None,
        delivery: Delivery::Background,
        queue_capacity: 16,
        stop_timeout: Duration::from_secs(5),
        export,
        connect: connect.map(Path::to_path_buf),
    }
}

async fn server(socket: &Path, contracts: &[&str]) -> Result<Running, AppError> {
    let export = Export {
        socket: socket.to_path_buf(),
        contracts: contracts.iter().map(|c| (*c).to_owned()).collect(),
    };
    kernel::start(
        vec![Arc::new(Provider)],
        &HashMap::new(),
        options(Some(export), None),
    )
    .await
}

async fn client(socket: &Path, components: Vec<Arc<dyn Component>>) -> Running {
    kernel::start(components, &HashMap::new(), options(None, Some(socket)))
        .await
        .unwrap()
}

fn import() -> Arc<dyn Component> {
    Arc::new(Import::new("greeter-remote", "转给另一个进程", &GREETER))
}

#[tokio::test]
async fn calls_go_through_the_socket_and_back() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("run/daemon.sock");
    let daemon = server(&socket, &["greeter"]).await.unwrap();
    let cli = client(&socket, vec![import()]).await;
    let greeter = cli.resolve(&GREETER).unwrap();

    assert_eq!(
        greeter.greet("气象站".to_owned(), 3).await.unwrap(),
        Real.greet("气象站".to_owned(), 3).await.unwrap(),
        "和直接调用一样"
    );
    assert_eq!(greeter.count().await.unwrap(), 42, "没有参数的方法");
    let err = greeter.fail(ErrorKind::NotFound).await.unwrap_err();
    assert_eq!(
        (err.kind, err.message.as_str()),
        (ErrorKind::NotFound, "故意的"),
        "错误原样回来"
    );

    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(socket.parent().unwrap())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700, "socket 所在的目录只有自己能进");

    daemon.stop().await;
    assert!(!socket.exists(), "停止时删掉 socket");
    let err = greeter.count().await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unavailable);
    assert!(err.message.contains("没在运行"), "{err}");
    cli.stop().await;
}

#[tokio::test]
async fn only_exported_contracts_and_matching_versions_are_served() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("daemon.sock");
    let daemon = server(&socket, &["greeter"]).await.unwrap();

    let other = client(
        &socket,
        vec![Arc::new(Import::new("other-remote", "测试", &OTHER))],
    )
    .await;
    let err = other.resolve(&OTHER).unwrap().ping().await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound, "提供了但没导出：{err}");

    let newer = client(
        &socket,
        vec![Arc::new(Import::new("greeter-v2", "测试", &v2::GREETER))],
    )
    .await;
    let err = newer
        .resolve(&v2::GREETER)
        .unwrap()
        .count()
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unavailable);
    assert!(err.message.contains("版本不一致"), "{err}");

    other.stop().await;
    newer.stop().await;
    daemon.stop().await;
}

#[tokio::test]
async fn exporting_skips_contracts_without_a_provider_and_needs_a_free_socket() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("daemon.sock");
    // 提供者被停用了：照样起来，调用方得到 not_found
    let daemon = server(&socket, &["greeter", "nothing"]).await.unwrap();
    let cli = client(
        &socket,
        vec![Arc::new(Import::new("other-remote", "测试", &OTHER))],
    )
    .await;
    let err = cli.resolve(&OTHER).unwrap().ping().await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotFound);
    cli.stop().await;
    daemon.stop().await;

    let first = server(&socket, &["greeter"]).await.unwrap();
    let err = server(&socket, &["greeter"]).await.err().unwrap();
    assert!(err.message.contains("已经有一个进程"), "{err}");
    first.stop().await;

    // 上一个进程崩溃留下的 socket 文件：照样能起来
    std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let again = server(&socket, &["greeter"]).await.unwrap();
    again.stop().await;
}

#[tokio::test]
async fn importing_needs_somewhere_to_connect() {
    let err = kernel::start(vec![import()], &HashMap::new(), options(None, None))
        .await
        .err()
        .unwrap();
    assert_eq!(err.kind, ErrorKind::Config);
    assert!(err.message.contains("greeter-remote"), "{err}");
}

#[tokio::test]
async fn a_socket_path_that_is_too_long_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("x".repeat(120)).join("daemon.sock");
    let err = server(&socket, &["greeter"]).await.err().unwrap();
    assert_eq!(err.kind, ErrorKind::Config);
    assert!(err.message.contains("socket 路径太长"), "{err}");
    let cli = client(&socket, vec![import()]).await;
    let err = cli.resolve(&GREETER).unwrap().count().await.unwrap_err();
    assert!(
        err.message.contains("socket 路径太长"),
        "不说「没在运行」：{err}"
    );
}
