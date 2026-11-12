//! 跨进程：在本地 socket 上导出契约，或者连到另一个进程导入契约。
//!
//! 消息格式在 `kernel-api`（按行分隔的 JSON）；这里只管 socket。每次调用一个连接：
//! 连上 → 写一行请求 → 读一行响应。调用不频繁（写命令），简单比快重要。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use kernel_api::{
    AppError, BoxFuture, Dispatcher, Transport, decode_request, decode_response, encode_request,
    encode_response,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio_util::sync::CancellationToken;

/// 这个进程把哪些契约导出到哪个 socket。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    pub socket: PathBuf,
    pub contracts: Vec<String>,
}

/// 进程里各服务契约的分派：契约名 → （版本，分派）。
pub(crate) type Dispatchers = Mutex<HashMap<&'static str, (u32, Dispatcher)>>;

/// Unix socket 的路径最长 107 字节（`sun_path` 是 108 字节，含结尾的 0）。
const MAX_SOCKET_PATH: usize = 107;

fn too_long(socket: &Path) -> Option<AppError> {
    let len = socket.as_os_str().len();
    (len > MAX_SOCKET_PATH).then(|| {
        AppError::config(format!(
            "socket 路径太长（{len} 字节，Unix socket 最多 {MAX_SOCKET_PATH} 字节）：{}。换一个短一点的 XDG_RUNTIME_DIR",
            socket.display()
        ))
    })
}

fn unavailable(socket: &Path, e: impl std::fmt::Display) -> AppError {
    AppError::unavailable(format!(
        "连不上 {}：{e}。提供这个功能的进程没在运行",
        socket.display()
    ))
}

/// 连到另一个进程的传输。
pub(crate) struct SocketTransport {
    pub(crate) socket: PathBuf,
}

impl Transport for SocketTransport {
    fn call(
        &self,
        contract: &'static str,
        version: u32,
        method: &'static str,
        params: String,
    ) -> BoxFuture<'static, Result<String, AppError>> {
        let socket = self.socket.clone();
        Box::pin(async move {
            if let Some(e) = too_long(&socket) {
                return Err(e);
            }
            let line = encode_request(contract, version, method, &params)?;
            let stream = UnixStream::connect(&socket)
                .await
                .map_err(|e| unavailable(&socket, e))?;
            let (read, mut write) = stream.into_split();
            write
                .write_all(format!("{line}\n").as_bytes())
                .await
                .map_err(|e| unavailable(&socket, e))?;
            let mut reply = String::new();
            let n = BufReader::new(read)
                .read_line(&mut reply)
                .await
                .map_err(|e| unavailable(&socket, e))?;
            if n == 0 {
                return Err(unavailable(&socket, "对方没回应就断开了"));
            }
            decode_response(reply.trim_end())
        })
    }
}

/// 处理一条请求。
async fn answer(line: &str, exported: &[String], dispatchers: &Dispatchers) -> String {
    let result = async {
        let request = decode_request(line)?;
        if !exported.contains(&request.contract) {
            return Err(AppError::not_found(format!(
                "这个进程没有导出契约 `{}`",
                request.contract
            )));
        }
        let found = dispatchers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(request.contract.as_str())
            .cloned();
        let Some((version, dispatch)) = found else {
            return Err(AppError::not_found(format!(
                "契约 `{}` 没有提供者",
                request.contract
            )));
        };
        if request.version != version {
            return Err(AppError::unavailable(format!(
                "契约 `{}` 的版本不一致：对方是 {}，这边是 {version}。两个程序要用同一次构建",
                request.contract, request.version
            )));
        }
        dispatch(request.method, request.params.to_string()).await
    }
    .await;
    encode_response(result)
}

async fn serve_connection(
    stream: UnixStream,
    exported: Arc<Vec<String>>,
    dispatchers: Arc<Dispatchers>,
) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let reply = answer(&line, &exported, &dispatchers).await;
        if write
            .write_all(format!("{reply}\n").as_bytes())
            .await
            .is_err()
        {
            break;
        }
    }
}

/// 绑定 socket：目录只有自己能进；旧进程留下的文件删掉；已经有活着的进程在用就报错。
pub(crate) async fn bind(socket: &Path) -> Result<UnixListener, AppError> {
    if let Some(e) = too_long(socket) {
        return Err(e);
    }
    let fail = |e: std::io::Error| {
        AppError::unavailable(format!("在 {} 上监听失败：{e}", socket.display()))
    };
    if let Some(dir) = socket.parent() {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(fail)?;
    }
    if socket.exists() {
        if UnixStream::connect(socket).await.is_ok() {
            return Err(AppError::config(format!(
                "已经有一个进程在 {} 上了：同一个程序不能同时跑两个",
                socket.display()
            )));
        }
        std::fs::remove_file(socket).map_err(fail)?;
    }
    UnixListener::bind(socket).map_err(fail)
}

/// 接受连接，直到被要求停止。
pub(crate) async fn serve(
    listener: UnixListener,
    exported: Vec<String>,
    dispatchers: Arc<Dispatchers>,
    stop: CancellationToken,
) {
    let exported = Arc::new(exported);
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                if let Ok((stream, _)) = accepted {
                    tokio::spawn(serve_connection(stream, exported.clone(), dispatchers.clone()));
                }
            }
            () = stop.cancelled() => break,
        }
    }
}
