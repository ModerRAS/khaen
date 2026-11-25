//! REST 端到端测试：真实 TCP 连接打真实 axum 服务，不用 mock。

use std::sync::Arc;

use khaen_daemon::{router, DaemonState};
use khaen_host::EngineConfig;
use serde_json::Value;
use tokio::net::TcpListener;

struct TestServer {
    base: String,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn spawn() -> TestServer {
    let state = Arc::new(DaemonState::new(EngineConfig::default()).expect("state"));
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let app = router(state);
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    TestServer {
        base: format!("http://{addr}"),
        handle,
    }
}

async fn request(method: &str, url: &str, body: Option<&str>) -> (u16, Value) {
    let (host, path) = split_url(url);
    let mut stream = tokio::net::TcpStream::connect(host).await.expect("connect");
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut text = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    if let Some(body) = body {
        text.push_str("Content-Type: application/json\r\n");
        text.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    text.push_str("\r\n");
    if let Some(body) = body {
        text.push_str(body);
    }
    stream.write_all(text.as_bytes()).await.expect("write");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.expect("read");
    let response = String::from_utf8_lossy(&response).into_owned();
    let (head, body) = response.split_once("\r\n\r\n").expect("http response");
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("status code");
    let json: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    (status, json)
}

fn split_url(url: &str) -> (&str, &str) {
    let rest = url.strip_prefix("http://").expect("scheme");
    match rest.split_once('/') {
        Some((host, _path)) => (host, &rest[host.len()..]),
        None => (rest, "/"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn status_reports_capabilities_over_http() {
    let server = spawn().await;
    let (status, body) = request("GET", &format!("{}/api/status", server.base), None).await;
    assert_eq!(status, 200);
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(body["runtime_state"], Value::String("idle".into()));
    assert_eq!(body["wired"][0], Value::String("status".into()));
    assert!(body["not_wired"]
        .as_array()
        .is_some_and(|ops| !ops.is_empty()));
}

#[tokio::test(flavor = "current_thread")]
async fn mute_is_block_boundary_and_idempotent() {
    let server = spawn().await;
    let url = format!("{}/api/mute", server.base);
    let (status, body) = request("POST", &url, Some(r#"{"mute":true}"#)).await;
    assert_eq!(status, 200);
    assert_eq!(body["mute"], Value::Bool(true));
    assert_eq!(body["applied_block"], Value::from(1));
    assert_eq!(body["idempotent"], Value::Bool(false));

    let (status, body) = request("POST", &url, Some(r#"{"mute":true}"#)).await;
    assert_eq!(status, 200);
    assert_eq!(body["idempotent"], Value::Bool(true));
}

#[tokio::test(flavor = "current_thread")]
async fn bad_mute_payload_is_400() {
    let server = spawn().await;
    let url = format!("{}/api/mute", server.base);
    let (status, body) = request("POST", &url, Some(r#"{"mute":"yes"}"#)).await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], Value::String("bad_request".into()));
}

#[tokio::test(flavor = "current_thread")]
async fn unwired_ops_are_501_with_reason() {
    let server = spawn().await;
    for op in ["prepare", "commit", "abort"] {
        let url = format!("{}/api/{op}", server.base);
        let (status, body) = request("POST", &url, Some("{}")).await;
        assert_eq!(status, 501, "op {op}");
        assert_eq!(body["error"], Value::String("unsupported".into()));
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|text| text.contains("not wired")),
            "op {op} 必须给出原因"
        );
    }
    let (status, body) = request("GET", &format!("{}/api/coeff", server.base), None).await;
    assert_eq!(status, 501);
    assert_eq!(body["error"], Value::String("unsupported".into()));
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_path_is_404() {
    let server = spawn().await;
    let (status, _) = request("GET", &format!("{}/api/nope", server.base), None).await;
    assert_eq!(status, 404);
}
