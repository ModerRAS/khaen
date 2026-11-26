//! M0 测试共享工具：真实 TCP 连接打真实 axum 服务（不 mock），返回原始响应字节。
//!
//! 请求行里的 `path` 原样发送，不经过客户端规范化——这样才能测试 `..`、百分号编码等
//! 穿越路径在服务端的真实行为。

#![allow(dead_code)] // 不同的测试二进制只用到其中一部分工具

use std::sync::Arc;

use khaen_daemon::{router, DaemonState};
use khaen_host::EngineConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// 一个真实监听随机端口的 daemon 服务；drop 时终止服务任务。
pub struct TestServer {
    pub addr: String,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// 启动真实 axum 服务（与 `main` 用同一个 `router`）。
pub async fn spawn() -> TestServer {
    let state = Arc::new(DaemonState::new(EngineConfig::default()).expect("state"));
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let addr = listener.local_addr().expect("local addr").to_string();
    let app = router(Arc::clone(&state));
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    TestServer { addr, handle }
}

/// 原始 HTTP 响应。
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// 大小写不敏感的响应头查找。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// body 按 UTF-8 有损解码（断言错误信息用）。
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// body 按 JSON 解析（非 JSON 时返回 `Null`）。
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }
}

/// 发一个 HTTP/1.1 请求（GET/HEAD/POST/DELETE 均可，不带请求体），返回完整响应。
pub async fn request(server: &TestServer, method: &str, path: &str) -> HttpResponse {
    let mut stream = tokio::net::TcpStream::connect(&server.addr)
        .await
        .expect("connect");
    let text = format!(
        "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        server.addr
    );
    stream.write_all(text.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read");

    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("http response head");
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let body = raw[split + 4..].to_vec();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .expect("status line")
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("status code");
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_string(), value.trim().to_string()))
        })
        .collect();
    HttpResponse {
        status,
        headers,
        body,
    }
}
