//! 测试共享工具：真实 TCP 连接打真实 axum 服务（不 mock），返回原始响应字节。
//!
//! 请求行里的 path 原样发送，不经过客户端规范化——这样才能测试 ..、百分号编码等
//! 穿越路径在服务端的真实行为。
//!
//! M1 起 /api 除 auth 三个公开端点外都要求会话：spawn() 用临时状态目录自动完成
//! setup（或已配置时 login），request() 自动带 Cookie、写方法自动带 X-KHAEN-CSRF。
//! 需要匿名/未配置/自定义头的场景用 spawn_at()（不自动登录）与 request_with()（不自动加头）。

#![allow(dead_code)] // 不同的测试二进制只用到其中一部分工具

pub mod calibration_fake;
pub mod host_link_fake;
pub mod plan_fixture;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use khaen_daemon::auth::{AuthConfig, SESSION_COOKIE};
use khaen_daemon::calibration::service::CalibrationService;
use khaen_daemon::host_link::{HostLink, HostLinkConfig, Transport};
use khaen_daemon::system::{BuildInfo, RestartPolicy, SystemProbe};
use khaen_daemon::{router, DaemonState};
use khaen_host::EngineConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// 测试用固定密码（满足 >=8 字符；只在临时状态目录里使用）。
pub const TEST_PASSWORD: &str = "khaen-test-password";

/// 进程内目标（仅测试/离线演示）：控制 op 作用于 daemon 进程内引擎，响应标 `target: daemon-process`。
///
/// 默认测试服务都用它——旧 REST 测试的语义（真跑进程内引擎）保持不变，同时按 T70f-b 的契约
/// 显式标注目标；rt-host 转发语义由注入假传输的测试单独覆盖。
pub fn process_link() -> Arc<HostLink> {
    HostLink::process()
}

/// rt-host 目标 + 注入传输（内存假实现或 TCP 回环），2 s 超时（并行跑测试时也不至于误超时）。
///
/// 泛型入参让调用方直接传 `Arc<FakeTransport>`（在函数调用点做 unsize 转换，
/// 不用写 `as Arc<dyn Transport>`）。
pub fn injected_link<T: Transport + 'static>(transport: Arc<T>) -> Arc<HostLink> {
    injected_link_with_timeout(transport, Duration::from_secs(2))
}

/// 同 [`injected_link`]，但显式给往返超时（超时降级测试用短超时）。
pub fn injected_link_with_timeout<T: Transport + 'static>(
    transport: Arc<T>,
    timeout: Duration,
) -> Arc<HostLink> {
    let transport: Arc<dyn Transport> = transport;
    HostLink::with_transport(
        HostLinkConfig::rt_host("/tmp/khaen-test-rt-host.sock").with_timeout(timeout),
        transport,
    )
}

/// 测试用临时状态目录；Drop 时递归删除。
pub struct TempStateDir {
    path: PathBuf,
}

impl TempStateDir {
    /// 在系统临时目录下创建唯一目录。
    pub fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "khaen-daemon-test-{}-{tag}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp state dir");
        Self { path }
    }

    /// 目录路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// auth.json 路径。
    pub fn auth_path(&self) -> PathBuf {
        self.path.join("auth.json")
    }
}

impl Drop for TempStateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// 已签发的会话凭据（Cookie 值 = 会话 token）。
#[derive(Debug, Clone)]
pub struct TestSession {
    /// 会话 token（写进 Cookie）。
    pub token: String,
    /// 绑定会话的 CSRF。
    pub csrf: String,
}

/// 一个真实监听随机端口的 daemon 服务；drop 时终止服务任务。
pub struct TestServer {
    pub addr: String,
    pub state: Arc<DaemonState>,
    pub state_dir: PathBuf,
    session: Option<TestSession>,
    handle: tokio::task::JoinHandle<()>,
    owned_dir: Option<Arc<TempStateDir>>,
}

impl TestServer {
    /// 服务根 URL（http://host:port）。
    pub fn base(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// 自动签发的会话；spawn_at() 起的未认证服务为 None。
    pub fn session(&self) -> Option<&TestSession> {
        self.session.as_ref()
    }

    /// 未自动登录的服务：完成 setup/login 并把会话挂到本测试服务上（M2 状态聚合测试用）。
    pub async fn sign_in(&mut self) {
        self.session = Some(bootstrap_session(self).await);
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// 启动真实 axum 服务（与 main 用同一个 router + connect info），自动 setup/login。
pub async fn spawn() -> TestServer {
    let dir = Arc::new(TempStateDir::new("support"));
    let mut server = spawn_at(dir.path()).await;
    server.owned_dir = Some(Arc::clone(&dir));
    server.session = Some(bootstrap_session(&server).await);
    server
}

/// 用指定状态目录启动服务；不自动登录（调用方自己 setup/login）。
pub async fn spawn_at(state_dir: &Path) -> TestServer {
    spawn_at_with_config(state_dir, AuthConfig::for_tests(state_dir.to_path_buf())).await
}

/// 用指定状态目录 + 自定义认证配置（如短会话 TTL）启动服务；不自动登录。
pub async fn spawn_at_with_config(state_dir: &Path, config: AuthConfig) -> TestServer {
    spawn_custom(state_dir, config, None, None, None).await
}

/// 同 spawn_at_with_config，但注入 rt-host 状态文件路径（测试 M2 聚合的 fixture）。
pub async fn spawn_at_with_rt_status_file(state_dir: &Path, rt_status_file: PathBuf) -> TestServer {
    spawn_custom(
        state_dir,
        AuthConfig::for_tests(state_dir.to_path_buf()),
        Some(rt_status_file),
        None,
        None,
    )
    .await
}

/// 用指定状态目录 + 注入控制通道启动服务；可选 rt-host 状态文件路径；不自动登录。
pub async fn spawn_at_with_link(
    state_dir: &Path,
    link: Arc<HostLink>,
    rt_status_file: Option<PathBuf>,
) -> TestServer {
    spawn_custom(
        state_dir,
        AuthConfig::for_tests(state_dir.to_path_buf()),
        rt_status_file,
        None,
        Some(link),
    )
    .await
}

/// 临时状态目录 + 注入控制通道（可选状态文件），自动 setup/login。
pub async fn spawn_with_link(link: Arc<HostLink>, rt_status_file: Option<PathBuf>) -> TestServer {
    let dir = Arc::new(TempStateDir::new("host-link"));
    let mut server = spawn_at_with_link(dir.path(), link, rt_status_file).await;
    server.owned_dir = Some(Arc::clone(&dir));
    server.session = Some(bootstrap_session(&server).await);
    server
}

/// 用临时状态目录 + 注入系统采集/构建信息/重启能力启动服务（T46 系统页测试；自动 setup/login）。
pub async fn spawn_with_system(
    probe: Arc<dyn SystemProbe>,
    build: BuildInfo,
    restart: RestartPolicy,
) -> TestServer {
    let dir = Arc::new(TempStateDir::new("system"));
    let mut server = spawn_at_with_system(dir.path(), probe, build, restart).await;
    server.owned_dir = Some(Arc::clone(&dir));
    server.session = Some(bootstrap_session(&server).await);
    server
}

/// 同 spawn_at_with_system，但不自动登录（匿名 /setup 前状态测试用）。
pub async fn spawn_at_with_system(
    state_dir: &Path,
    probe: Arc<dyn SystemProbe>,
    build: BuildInfo,
    restart: RestartPolicy,
) -> TestServer {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let state = DaemonState::new_with_auth(
        EngineConfig::default(),
        AuthConfig::for_tests(state_dir.to_path_buf()),
    )
    .expect("daemon state")
    .with_bind(addr)
    .with_rt_status_file(state_dir.join("rt-host-status.json"))
    .with_host_link(process_link())
    .with_system_probe(probe)
    .with_build_info(build)
    .with_restart_policy(restart);
    let state = Arc::new(state);
    let app = router(Arc::clone(&state)).into_make_service_with_connect_info::<SocketAddr>();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    TestServer {
        addr: addr.to_string(),
        state,
        state_dir: state_dir.to_path_buf(),
        session: None,
        handle,
        owned_dir: None,
    }
}

/// 用临时状态目录 + 注入校准 job 服务启动服务（自动 setup/login）。
pub async fn spawn_with_calibration(service: Arc<dyn CalibrationService>) -> TestServer {
    let dir = Arc::new(TempStateDir::new("calibration"));
    let mut server = spawn_at_with_service(dir.path(), service).await;
    server.owned_dir = Some(Arc::clone(&dir));
    server.session = Some(bootstrap_session(&server).await);
    server
}

/// 用指定状态目录 + 注入校准服务启动服务；不自动登录。
pub async fn spawn_at_with_service(
    state_dir: &Path,
    service: Arc<dyn CalibrationService>,
) -> TestServer {
    spawn_custom(
        state_dir,
        AuthConfig::for_tests(state_dir.to_path_buf()),
        None,
        Some(service),
        None,
    )
    .await
}

/// 真实监听随机端口；启动时解析的监听地址注入 DaemonState（/api/status 的 daemon.bind 用它）。
///
/// `link` 为 None 时用 [`process_link`]（旧测试语义）；转发语义测试显式注入 rt-host 链路。
async fn spawn_custom(
    state_dir: &Path,
    config: AuthConfig,
    rt_status_file: Option<PathBuf>,
    calibration: Option<Arc<dyn CalibrationService>>,
    link: Option<Arc<HostLink>>,
) -> TestServer {
    // 先绑定端口，再构造 state：daemon.bind 必须是启动时解析的真实监听地址。
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let mut state = DaemonState::new_with_auth(EngineConfig::default(), config)
        .expect("daemon state")
        .with_bind(addr);
    if let Some(rt_status_file) = rt_status_file {
        state = state.with_rt_status_file(rt_status_file);
    }
    if let Some(calibration) = calibration {
        state = state.with_calibration(calibration);
    }
    state = state.with_host_link(link.unwrap_or_else(process_link));
    let state = Arc::new(state);
    let app = router(Arc::clone(&state)).into_make_service_with_connect_info::<SocketAddr>();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    TestServer {
        addr: addr.to_string(),
        state,
        state_dir: state_dir.to_path_buf(),
        session: None,
        handle,
        owned_dir: None,
    }
}

/// 未配置 → setup；已配置 → login。返回可用会话。
pub async fn bootstrap_session(server: &TestServer) -> TestSession {
    let status = request_with(server, "GET", "/api/auth/status", &[], None).await;
    assert_eq!(status.status, 200, "auth status: {}", status.body_text());
    let configured = status.json()["configured"].as_bool().unwrap_or(false);
    let body = format!(r#"{{"password":"{TEST_PASSWORD}","confirm":"{TEST_PASSWORD}"}}"#);
    let response = if configured {
        request_with(
            server,
            "POST",
            "/api/auth/login",
            &[],
            Some(&format!(r#"{{"password":"{TEST_PASSWORD}"}}"#)),
        )
        .await
    } else {
        request_with(server, "POST", "/api/auth/setup", &[], Some(&body)).await
    };
    let expected = if configured { 200 } else { 201 };
    assert_eq!(
        response.status,
        expected,
        "auth bootstrap {}: {}",
        if configured { "login" } else { "setup" },
        response.body_text()
    );
    let csrf = response.json()["csrf"]
        .as_str()
        .expect("csrf in auth response")
        .to_string();
    let token = response
        .session_token()
        .expect("khaen_session cookie in auth response");
    TestSession { token, csrf }
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

    /// body 按 JSON 解析（非 JSON 时返回 Null）。
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }

    /// Set-Cookie 头里的 khaen_session 值（会话 token）。
    pub fn session_token(&self) -> Option<String> {
        let header = self.header("set-cookie")?;
        let pair = header.split(';').next()?.trim();
        let (name, value) = pair.split_once('=')?;
        if name.trim() == SESSION_COOKIE {
            Some(value.trim().to_string())
        } else {
            None
        }
    }
}

/// 发一个自动带认证的请求（GET/HEAD 带 Cookie；写方法再带 CSRF）。
pub async fn request(server: &TestServer, method: &str, path: &str) -> HttpResponse {
    request_authed(server, method, path, None).await
}

/// 同 request，但带 JSON body。
pub async fn request_json(
    server: &TestServer,
    method: &str,
    path: &str,
    body: &str,
) -> HttpResponse {
    request_authed(server, method, path, Some(body)).await
}

fn auth_headers(server: &TestServer, method: &str) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = Vec::new();
    if let Some(session) = server.session() {
        headers.push((
            "Cookie".to_string(),
            format!("{SESSION_COOKIE}={}", session.token),
        ));
        let upper = method.to_ascii_uppercase();
        if matches!(upper.as_str(), "POST" | "PUT" | "PATCH" | "DELETE") {
            headers.push(("X-KHAEN-CSRF".to_string(), session.csrf.clone()));
        }
    }
    headers
}

async fn request_authed(
    server: &TestServer,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> HttpResponse {
    let headers = auth_headers(server, method);
    request_with_raw(server, method, path, &headers, body.map(str::as_bytes)).await
}

/// 发一个自动带认证的原始字节请求（upload / 二进制 body）。
pub async fn request_bytes(
    server: &TestServer,
    method: &str,
    path: &str,
    content_type: &str,
    body: &[u8],
) -> HttpResponse {
    let mut headers = auth_headers(server, method);
    headers.push(("Content-Type".to_string(), content_type.to_string()));
    request_with_raw(server, method, path, &headers, Some(body)).await
}

/// 发一个不带自动认证头的原始字节请求（未鉴权 / 缺 CSRF / 自定义 Content-Type 场景）。
pub async fn request_bytes_with(
    server: &TestServer,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
) -> HttpResponse {
    request_with_raw(server, method, path, headers, body).await
}

/// 发一个不带自动认证头的 HTTP/1.1 请求，返回完整响应。
pub async fn request_with(
    server: &TestServer,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: Option<&str>,
) -> HttpResponse {
    request_with_raw(server, method, path, headers, body.map(str::as_bytes)).await
}

/// 原始字节版本的请求构造（request_with / request_bytes 共用）。
pub async fn request_with_raw(
    server: &TestServer,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
) -> HttpResponse {
    let mut stream = tokio::net::TcpStream::connect(&server.addr)
        .await
        .expect("connect");
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
        server.addr
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if let Some(body) = body {
        if !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        {
            head.push_str("Content-Type: application/json\r\n");
        }
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    let mut text = head.into_bytes();
    if let Some(body) = body {
        text.extend_from_slice(body);
    }
    stream.write_all(&text).await.expect("write");
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
