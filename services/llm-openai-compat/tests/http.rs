//! 对着一个本地的假接口测：发出去的请求长什么样、各种响应变成什么。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

use kernel_api::ErrorKind;
use llm_contract::{ChatMessage, CompletionRequest, LLM};
use llm_openai_compat::LlmOpenAiCompat;
use serde_json::{Value, json};

/// 收到的请求：（路径，Authorization 头，JSON 体）。
type Seen = (String, String, Value);

/// 起一个只回一次的 HTTP 服务。
fn fake(status: u16, body: &str) -> (String, mpsc::Receiver<Seen>) {
    fake_times(status, body, 1)
}

/// 起一个回 `times` 次的 HTTP 服务。
fn fake_times(status: u16, body: &str, times: usize) -> (String, mpsc::Receiver<Seen>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    let body = body.to_owned();
    std::thread::spawn(move || {
        for _ in 0..times {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
            let (mut auth, mut length) = (String::new(), 0usize);
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                let header = header.trim_end();
                if header.is_empty() {
                    break;
                }
                let (name, value) = header.split_once(':').unwrap();
                match name.to_ascii_lowercase().as_str() {
                    "authorization" => auth = value.trim().to_owned(),
                    "content-length" => length = value.trim().parse().unwrap(),
                    _ => {}
                }
            }
            let mut request = vec![0; length];
            reader.read_exact(&mut request).unwrap();
            tx.send((path, auth, serde_json::from_slice(&request).unwrap()))
                .unwrap();
            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    (url, rx)
}

async fn boot(config: Value) -> testkit::TestApp {
    testkit::app()
        .with(LlmOpenAiCompat)
        .config("llm-openai-compat", config)
        .unwrap()
        .boot()
        .await
        .unwrap()
}

fn request() -> CompletionRequest {
    CompletionRequest {
        messages: vec![
            ChatMessage::system("你是助手"),
            ChatMessage::user("总结一下"),
        ],
        max_tokens: Some(100),
    }
}

#[tokio::test]
async fn sends_a_chat_completion_and_reads_the_reply() {
    let (url, seen) = fake(
        200,
        r#"{"model":"deepseek-chat","choices":[{"message":{"role":"assistant","content":"今天写了 lifelog。"}}],"usage":{"prompt_tokens":12,"completion_tokens":7}}"#,
    );
    let app =
        boot(json!({ "base_url": url, "model": "deepseek-chat", "api_key": "sk-test\n" })).await;
    let reply = app
        .resolve(&LLM)
        .unwrap()
        .complete(request())
        .await
        .unwrap();
    assert_eq!(reply.text, "今天写了 lifelog。");
    assert_eq!(
        (
            reply.model.as_str(),
            reply.prompt_tokens,
            reply.completion_tokens
        ),
        ("deepseek-chat", Some(12), Some(7))
    );

    let (path, auth, body) = seen.recv().unwrap();
    assert_eq!(path, "/v1/chat/completions");
    assert_eq!(
        auth, "Bearer sk-test",
        "key 两边的空白去掉（命令的输出带换行）"
    );
    assert_eq!(body["model"], "deepseek-chat");
    assert_eq!(body["max_tokens"], 100);
    assert_eq!(body["stream"], false);
    assert_eq!(
        body["messages"],
        json!([{ "role": "system", "content": "你是助手" }, { "role": "user", "content": "总结一下" }])
    );
}

#[tokio::test]
async fn http_errors_become_error_kinds() {
    for (status, kind) in [
        (401, ErrorKind::Unauthorized),
        (429, ErrorKind::TempFail),
        (400, ErrorKind::Validation),
        (503, ErrorKind::Unavailable),
    ] {
        let (url, _seen) = fake(status, r#"{"error":{"message":"服务商说的"}}"#);
        let app = boot(json!({ "base_url": url, "model": "m", "api_key": "k" })).await;
        let err = app
            .resolve(&LLM)
            .unwrap()
            .complete(request())
            .await
            .unwrap_err();
        assert_eq!(err.kind, kind, "{status}");
        assert!(err.message.contains("服务商说的"), "{err}");
    }
}

#[tokio::test]
async fn missing_settings_fail_at_call_time_not_at_startup() {
    let app = boot(json!({})).await;
    let err = app
        .resolve(&LLM)
        .unwrap()
        .complete(request())
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unavailable);
    assert!(err.message.contains("model"), "{err}");

    let app = boot(json!({ "model": "m", "api_key_env": "LIFELOG_TEST_NO_SUCH_VAR" })).await;
    let err = app
        .resolve(&LLM)
        .unwrap()
        .complete(request())
        .await
        .unwrap_err();
    assert!(err.message.contains("key_command"), "{err}");
}

#[tokio::test]
async fn the_key_command_runs_on_first_use_only() {
    let dir = tempfile::tempdir().unwrap();
    let counter = dir.path().join("runs");
    let (url, seen) = fake_times(200, r#"{"choices":[{"message":{"content":"好"}}]}"#, 2);
    let command = format!("echo x >> {}; printf 'sk-lazy\\n'", counter.display());
    let app = boot(json!({ "base_url": url, "model": "m", "key_command": command })).await;
    assert!(!counter.exists(), "启动时不运行");
    let llm = app.resolve(&LLM).unwrap();
    llm.complete(request()).await.unwrap();
    llm.complete(request()).await.unwrap();
    assert_eq!(seen.recv().unwrap().1, "Bearer sk-lazy");
    assert_eq!(seen.recv().unwrap().1, "Bearer sk-lazy");
    assert_eq!(
        std::fs::read_to_string(&counter).unwrap().lines().count(),
        1,
        "调了两次，命令只运行一次"
    );

    let failing =
        boot(json!({ "model": "m", "key_command": "echo 'gpg: 解锁失败' >&2; exit 2" })).await;
    let err = failing
        .resolve(&LLM)
        .unwrap()
        .complete(request())
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unavailable);
    assert!(err.message.contains("解锁失败"), "{err}");
}

#[tokio::test]
async fn nobody_listening_is_unavailable() {
    let app =
        boot(json!({ "base_url": "http://127.0.0.1:9/v1", "model": "m", "api_key": "k" })).await;
    let err = app
        .resolve(&LLM)
        .unwrap()
        .complete(request())
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unavailable);
}
