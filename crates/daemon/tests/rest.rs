//! REST 端到端测试：真实 TCP 连接打真实 axum 服务，不用 mock。
//!
//! 覆盖：能力上报（wired/not_wired 真实）、方案 prepare/commit/abort/coeff 的真实行为与
//! 错误码 → HTTP 状态码映射（bad_request→400、admission_rejected→409、
//! engine_token_invalid→409、unknown_op→404）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use khaen_calibration::fir_plan::{compile_fir_plan, FirPlanConfig, LaneFir};
use khaen_calibration::{compile_peq_design, PlanCompileOptions};
use khaen_daemon::auth::{AuthConfig, SESSION_COOKIE};
use khaen_daemon::{router, DaemonState};
use khaen_designer::modal_peq::{BandType, ChannelPeq, DesignMode, ModalBand, PeqDesign};
use khaen_host::plan::{
    ContentSection, CrossoverFilter, CrossoverSection, Fold, MatrixSection, Plan, ProtectionEntry,
    ProtectionSection, RoutingSection,
};
use khaen_host::{sha256_hex, EngineConfig};
use serde_json::{json, Value};
use tokio::net::TcpListener;

const SAMPLE_RATE: u32 = 96_000;
const BLOCK_FRAMES: u32 = 128;
const CHANNELS: u16 = 2;
/// +6 dB 的线性增益（10^(6/20)）；断言容差 1e-6。
const GAIN_6DB: f32 = 1.995_262;

/// 测试固定密码（临时状态目录内使用；>=8 字符）。
const REST_PASSWORD: &str = "khaen-rest-test-password";

/// 测试用临时状态目录（auth.json 落这里；Drop 时递归删除）。
struct TempStateDir(PathBuf);

impl TempStateDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "khaen-daemon-rest-{}-{tag}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp state dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempStateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct TestServer {
    base: String,
    handle: tokio::task::JoinHandle<()>,
    _state_dir: TempStateDir,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// 已签发的会话凭据（按 host:port 记账，request() 自动带 Cookie/CSRF）。
#[derive(Clone)]
struct Credentials {
    token: String,
    csrf: String,
}

fn credentials() -> &'static Mutex<HashMap<String, Credentials>> {
    static CREDENTIALS: OnceLock<Mutex<HashMap<String, Credentials>>> = OnceLock::new();
    CREDENTIALS.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn spawn() -> (TestServer, Arc<DaemonState>) {
    let state_dir = TempStateDir::new("rest");
    // 先绑定再构造 state：/api/status 的 daemon.bind 用启动期解析的监听地址。
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let state = Arc::new(
        DaemonState::new_with_auth(
            EngineConfig::default(),
            AuthConfig::for_tests(state_dir.path()),
        )
        .expect("state")
        .with_bind(addr)
        // 测试固定注入一个不存在的 rt-host 状态文件：/api/status 必须可预测地退化。
        .with_rt_status_file(state_dir.path().join("rt-host-status.json"))
        // T70f-b：这些用例验证进程内引擎的真实数值行为，显式选 process 目标（响应带 target 标记）；
        // rt-host 转发语义由 tests/host_link.rs 覆盖。
        .with_host_link(khaen_daemon::host_link::HostLink::process()),
    );
    let app =
        router(Arc::clone(&state)).into_make_service_with_connect_info::<std::net::SocketAddr>();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let server = TestServer {
        base: format!("http://{addr}"),
        handle,
        _state_dir: state_dir,
    };
    let session = sign_in(&server.base).await;
    let (host, _) = split_url(&server.base);
    credentials()
        .lock()
        .expect("credentials")
        .insert(host.to_string(), session);
    (server, state)
}

/// setup 一个管理员密码并返回会话（每个 rest 测试服务只做一次）。
async fn sign_in(base: &str) -> Credentials {
    let url = format!("{base}/api/auth/setup");
    let body = format!(r#"{{"password":"{REST_PASSWORD}","confirm":"{REST_PASSWORD}"}}"#);
    let (status, json, headers) = request_with("POST", &url, Some(&body), &[]).await;
    assert_eq!(status, 201, "auth setup 必须成功: {json}");
    let csrf = json["csrf"].as_str().expect("csrf").to_string();
    let set_cookie = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
        .map(|(_, value)| value.clone())
        .expect("set-cookie");
    let token = set_cookie
        .split(';')
        .next()
        .and_then(|pair| pair.split_once('='))
        .filter(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, value)| value.trim().to_string())
        .expect("khaen_session cookie");
    Credentials { token, csrf }
}

/// 自动带会话认证的请求（M1：/api 全部要求会话；写方法带 CSRF）。
async fn request(method: &str, url: &str, body: Option<&str>) -> (u16, Value) {
    let (host, _) = split_url(url);
    let credentials = credentials()
        .lock()
        .expect("credentials")
        .get(host)
        .cloned();
    let mut headers: Vec<(String, String)> = Vec::new();
    if let Some(credentials) = credentials {
        headers.push((
            "Cookie".to_string(),
            format!("{SESSION_COOKIE}={}", credentials.token),
        ));
        if matches!(method, "POST" | "PUT" | "PATCH" | "DELETE") {
            headers.push(("X-KHAEN-CSRF".to_string(), credentials.csrf));
        }
    }
    let (status, json, _headers) = request_with(method, url, body, &headers).await;
    (status, json)
}

/// 原始请求：显式请求头，不自动加认证。
async fn request_with(
    method: &str,
    url: &str,
    body: Option<&str>,
    headers: &[(String, String)],
) -> (u16, Value, Vec<(String, String)>) {
    let (host, path) = split_url(url);
    let mut stream = tokio::net::TcpStream::connect(host).await.expect("connect");
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut text = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    for (name, value) in headers {
        text.push_str(&format!("{name}: {value}\r\n"));
    }
    if let Some(body) = body {
        if !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        {
            text.push_str("Content-Type: application/json\r\n");
        }
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
    let headers: Vec<(String, String)> = head
        .split("\r\n")
        .skip(1)
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_string(), value.trim().to_string()))
        })
        .collect();
    let json: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    (status, json, headers)
}

#[tokio::test(flavor = "current_thread")]
async fn surface_two_phase_over_http() {
    let (server, state) = spawn().await;

    // 没有已生效方案：surface 不能凭空打补丁 → 409 no_base_plan。
    let body = json!({
        "surface": "volume",
        "data": { "gains_db": [ {"lane": 0, "gain_db": 0.0} ] }
    })
    .to_string();
    let (status, response) =
        request("POST", &format!("{}/api/surface", server.base), Some(&body)).await;
    assert_eq!(status, 409);
    assert_eq!(response["error"], Value::String("no_base_plan".into()));

    // mode 已接通：非法输入源显式拒绝（400，不假装可选）。
    let body = json!({ "surface": "mode", "data": { "mode": "toslink" } }).to_string();
    let (status, response) =
        request("POST", &format!("{}/api/surface", server.base), Some(&body)).await;
    assert_eq!(status, 400);
    assert_eq!(response["error"], Value::String("bad_request".into()));
    assert!(response["message"]
        .as_str()
        .is_some_and(|message| message.contains("not wired")));

    // format 已接通：surface 入口委托执行器配置的两阶段（不要求已有方案）。
    let body = json!({ "surface": "format", "data": { "chunksize": 64 } }).to_string();
    let (status, draft) =
        request("POST", &format!("{}/api/surface", server.base), Some(&body)).await;
    assert_eq!(status, 200);
    assert_eq!(draft["changed"], Value::Bool(true));
    assert_eq!(draft["format"]["block_size"], Value::from(64));

    // 先准备 + 提交一份 identity 基线方案。
    let file = TempPlan::new("surface-base", &identity_plan().encode());
    let body = json!({
        "plan_path": file.path(),
        "plan_sha256": file.sha256,
    })
    .to_string();
    let (status, prepared) =
        request("POST", &format!("{}/api/prepare", server.base), Some(&body)).await;
    assert_eq!(status, 200);
    let token = prepared["engine_token"]
        .as_str()
        .expect("token")
        .to_string();
    let body = json!({ "engine_token": token }).to_string();
    let (status, _) = request("POST", &format!("{}/api/commit", server.base), Some(&body)).await;
    assert_eq!(status, 200);
    let input = signal(BLOCK_FRAMES as usize * CHANNELS as usize, 0x9e7);
    let _ = run_block(&state, &input);

    // 两阶段：surface 只准备补丁；commit 才在块边界生效。
    let body = json!({
        "surface": "volume",
        "data": { "gains_db": [ {"lane": 0, "gain_db": 6.0} ] }
    })
    .to_string();
    let (status, prepared) =
        request("POST", &format!("{}/api/surface", server.base), Some(&body)).await;
    assert_eq!(status, 200);
    assert_eq!(prepared["surface"], Value::String("volume".into()));
    let token = prepared["engine_token"]
        .as_str()
        .expect("surface token")
        .to_string();
    let body = json!({ "engine_token": token }).to_string();
    let (status, _) = request("POST", &format!("{}/api/commit", server.base), Some(&body)).await;
    assert_eq!(status, 200);

    let input2 = signal(BLOCK_FRAMES as usize * CHANNELS as usize, 0x9e8);
    let output = run_block(&state, &input2);
    for (index, (actual, sample)) in output.iter().zip(input2.iter()).enumerate() {
        let expected = if index % 2 == 0 {
            sample * GAIN_6DB
        } else {
            *sample
        };
        assert!(
            (actual - expected).abs() <= 1e-6,
            "sample {index}: {actual} vs {expected}"
        );
    }

    // 能力上报：M2 起 /api/status 是聚合 schema，能力清单看 /api/meta（同一份真实常量）。
    let (_, meta) = request("GET", &format!("{}/api/meta", server.base), None).await;
    let surfaces: Vec<&str> = meta["surfaces"]
        .as_array()
        .expect("surfaces 必须是数组")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for surface in ["volume", "routing", "layout", "format", "mode"] {
        assert!(
            surfaces.contains(&surface),
            "surfaces 缺 {surface}: {surfaces:?}"
        );
    }
}
fn split_url(url: &str) -> (&str, &str) {
    let rest = url.strip_prefix("http://").expect("scheme");
    match rest.split_once('/') {
        Some((host, _path)) => (host, &rest[host.len()..]),
        None => (rest, "/"),
    }
}

/// M2：/api/status 是 PLAN §4.4 的 `khaen-status/v1` 聚合；rt-host 状态文件缺失时
/// host_link 断开、rt-host 字段 null + reasons，daemon 自身字段照常返回，仍 200。
#[tokio::test(flavor = "current_thread")]
async fn status_reports_v1_aggregate_and_degrades_without_rt_host() {
    let (server, _state) = spawn().await;
    let (status, body) = request("GET", &format!("{}/api/status", server.base), None).await;
    assert_eq!(status, 200);
    assert_eq!(body["schema"], Value::String("khaen-status/v1".into()));

    // daemon 自身字段真实：版本来自进程、bind 来自启动时解析并注入的地址、格式来自 EngineConfig。
    assert_eq!(
        body["daemon"]["version"],
        Value::String(env!("CARGO_PKG_VERSION").into())
    );
    assert_eq!(
        body["daemon"]["bind"],
        Value::String(server.base.trim_start_matches("http://").into())
    );
    assert!(body["daemon"]["uptime_s"].is_u64());
    assert_eq!(
        body["format"]["block_frames"],
        Value::from(u64::from(BLOCK_FRAMES))
    );
    assert_eq!(body["format"]["channels"], Value::from(u64::from(CHANNELS)));

    // rt-host 状态文件不存在：connected=false，rt-host 字段 null + reasons，绝不 500。
    assert_eq!(body["host_link"]["connected"], Value::Bool(false));
    assert_eq!(body["host_link"]["socket"], Value::Null);
    assert_eq!(body["host_link"]["since"], Value::Null);
    assert_eq!(body["host_link"]["status_age_ms"], Value::Null);
    for path in [
        "host_link.connected",
        "state.value",
        "plan.graph_id",
        "format.plan_rate",
        "input.packets",
        "output.blocks",
        "device.rate_readback",
    ] {
        assert!(body["reasons"][path].is_string(), "缺原因: {path}");
    }
    assert_eq!(body["state"]["value"], Value::Null);
    assert_eq!(body["plan"]["graph_id"], Value::Null);
    // 校准 / 编码 / AES67 / 输出错误恒 null + 原因，不编数值。
    for path in [
        "calibration.active_job",
        "calibration.last_job_id",
        "calibration.last_verdict",
        "format.encoding",
        "format.aes67.pt",
        "output.out_errors",
        "device.id",
        "device.card",
    ] {
        assert!(body["reasons"][path].is_string(), "缺原因: {path}");
    }
    // not_wired 至少含控制通道条目（元素 {path, reason}）：本测试显式用 process 目标，
    // 所以控制通道如实标注为 host_link.control（不再写死 T70f 未实现）。
    let not_wired = body["not_wired"].as_array().expect("not_wired 必须是数组");
    let control = not_wired
        .iter()
        .find(|entry| entry["path"] == Value::String("host_link.control".into()))
        .expect("not_wired 必须含 host_link.control");
    assert!(control["reason"].as_str().is_some_and(|r| !r.is_empty()));
}

#[tokio::test(flavor = "current_thread")]
async fn device_endpoint_reports_not_wired_and_retry_is_harmless() {
    let (server, _state) = spawn().await;
    let (status, body) = request("GET", &format!("{}/api/device", server.base), None).await;
    assert_eq!(status, 200);
    assert_eq!(body["wired"], Value::Bool(false));
    assert_eq!(body["device_rate"], Value::Null);

    let (status, body) = request("POST", &format!("{}/api/device/retry", server.base), None).await;
    assert_eq!(status, 200);
    assert_eq!(body["wired"], Value::Bool(false));
}

#[tokio::test(flavor = "current_thread")]
async fn mute_is_block_boundary_and_idempotent() {
    let (server, _state) = spawn().await;
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
    let (server, _state) = spawn().await;
    let url = format!("{}/api/mute", server.base);
    let (status, body) = request("POST", &url, Some(r#"{"mute":"yes"}"#)).await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], Value::String("bad_request".into()));
}

#[tokio::test(flavor = "current_thread")]
async fn prepare_without_fields_is_400() {
    let (server, _state) = spawn().await;
    let url = format!("{}/api/prepare", server.base);
    let (status, body) = request("POST", &url, Some("{}")).await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], Value::String("bad_request".into()));
}

#[tokio::test(flavor = "current_thread")]
async fn prepare_with_bad_sha_or_missing_file_is_400() {
    let (server, _state) = spawn().await;
    let url = format!("{}/api/prepare", server.base);
    let plan_file = TempPlan::new("identity", &identity_plan().encode());

    // sha256 不符 → 400，点名。
    let body = json!({
        "plan_path": plan_file.path(),
        "plan_sha256": "0".repeat(64),
    })
    .to_string();
    let (status, response) = request("POST", &url, Some(&body)).await;
    assert_eq!(status, 400);
    assert_eq!(response["error"], Value::String("bad_request".into()));
    assert!(response["message"]
        .as_str()
        .is_some_and(|message| message.contains("sha256 mismatch")));

    // 文件不存在 → 400。
    let missing = std::env::temp_dir().join("khaen-t25b-rest-missing.plan");
    let body = json!({
        "plan_path": missing.display().to_string(),
        "plan_sha256": "00",
    })
    .to_string();
    let (status, response) = request("POST", &url, Some(&body)).await;
    assert_eq!(status, 400);
    assert!(response["message"]
        .as_str()
        .is_some_and(|message| message.contains("cannot read plan file")));
}

#[tokio::test(flavor = "current_thread")]
async fn commit_without_prepare_is_409() {
    let (server, _state) = spawn().await;
    let url = format!("{}/api/commit", server.base);
    let (status, body) = request("POST", &url, Some(r#"{"engine_token":"deadbeef"}"#)).await;
    assert_eq!(status, 409);
    assert_eq!(body["error"], Value::String("engine_token_invalid".into()));
}

#[tokio::test(flavor = "current_thread")]
async fn coeff_before_graph_is_null_with_reasons() {
    let (server, _state) = spawn().await;
    let (status, body) = request("GET", &format!("{}/api/coeff", server.base), None).await;
    assert_eq!(status, 200);
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(body["version"], Value::Null);
    assert_eq!(body["last_good_version"], Value::Null);
    assert_eq!(body["graph_id"], Value::Null);
    assert!(body["reasons"]["version"].is_string());
}

#[tokio::test(flavor = "current_thread")]
async fn prepare_commit_switches_graph_over_http() {
    let (server, state) = spawn().await;
    let identity = identity_plan();
    let identity_id = identity.graph_id_hex();
    let identity_file = TempPlan::new("identity", &identity.encode());

    // prepare → 200，返回确定性 token / 版本 / graph_id / 准入预算。
    let body = json!({
        "plan_path": identity_file.path(),
        "plan_sha256": identity_file.sha256,
    })
    .to_string();
    let (status, prepared) =
        request("POST", &format!("{}/api/prepare", server.base), Some(&body)).await;
    assert_eq!(status, 200);
    assert_eq!(prepared["ok"], Value::Bool(true));
    assert_eq!(prepared["planned_version"], Value::from(1));
    assert_eq!(prepared["graph_id"], Value::String(identity_id.clone()));
    assert_eq!(prepared["admission"]["accepted"], Value::Bool(true));
    let token = prepared["engine_token"]
        .as_str()
        .expect("token")
        .to_string();

    // commit → 200，块边界生效：applied_block = 下一块块号。
    let body = json!({ "engine_token": token }).to_string();
    let (status, committed) =
        request("POST", &format!("{}/api/commit", server.base), Some(&body)).await;
    assert_eq!(status, 200);
    assert_eq!(committed["version"], Value::from(1));
    assert_eq!(committed["applied_block"], Value::from(1));
    assert_eq!(committed["all_ack"], Value::Bool(true));
    assert_eq!(committed["graph_id"], Value::String(identity_id.clone()));

    // 下一块跑 identity 图：逐样本等于输入。
    let input = signal((BLOCK_FRAMES * u32::from(CHANNELS)) as usize, 7);
    let output = run_block(&state, &input);
    assert_eq!(output, input);

    // 已生效图/版本/生效块号在 /api/coeff（M2 起 /api/status 是 rt-host 聚合 schema，
    // 不再暴露 daemon 自己的 SlotEngine 图信息）。
    let (status, coefficients) = request("GET", &format!("{}/api/coeff", server.base), None).await;
    assert_eq!(status, 200);
    assert_eq!(coefficients["version"], Value::from(1));
    assert_eq!(coefficients["last_good_version"], Value::from(1));
    assert_eq!(coefficients["graph_id"], Value::String(identity_id));
    assert_eq!(coefficients["applied_block"], Value::from(1));

    // 第二张图：+6 dB；版本 2，下一块生效。
    let gain = gain_plan(6.0);
    let gain_id = gain.graph_id_hex();
    let gain_file = TempPlan::new("gain6", &gain.encode());
    let body = json!({
        "plan_path": gain_file.path(),
        "plan_sha256": gain_file.sha256,
    })
    .to_string();
    let (_, prepared) = request("POST", &format!("{}/api/prepare", server.base), Some(&body)).await;
    assert_eq!(prepared["planned_version"], Value::from(2));
    let token = prepared["engine_token"]
        .as_str()
        .expect("token")
        .to_string();
    let body = json!({ "engine_token": token }).to_string();
    let (_, committed) = request("POST", &format!("{}/api/commit", server.base), Some(&body)).await;
    assert_eq!(committed["applied_block"], Value::from(2));
    assert_eq!(committed["version"], Value::from(2));
    assert_eq!(committed["graph_id"], Value::String(gain_id.clone()));

    let input = signal((BLOCK_FRAMES * u32::from(CHANNELS)) as usize, 11);
    let output = run_block(&state, &input);
    for (index, (actual, sample)) in output.iter().zip(input.iter()).enumerate() {
        let expected = sample * GAIN_6DB;
        assert!(
            (actual - expected).abs() <= 1e-6,
            "sample {index}: {actual} vs {expected}"
        );
    }

    let (_, coefficients) = request("GET", &format!("{}/api/coeff", server.base), None).await;
    assert_eq!(coefficients["graph_id"], Value::String(gain_id.clone()));
    assert_eq!(coefficients["version"], Value::from(2));
}

#[tokio::test(flavor = "current_thread")]
async fn admission_rejection_is_409() {
    let (server, _state) = spawn().await;
    let mut plan = identity_plan();
    plan.matrix = None; // 实时图必须有矩阵段：准入拒绝
    let file = TempPlan::new("no-matrix", &plan.encode());
    let body = json!({
        "plan_path": file.path(),
        "plan_sha256": file.sha256,
    })
    .to_string();
    let (status, response) =
        request("POST", &format!("{}/api/prepare", server.base), Some(&body)).await;
    assert_eq!(status, 409);
    assert_eq!(
        response["error"],
        Value::String("admission_rejected".into())
    );
    assert!(response["message"]
        .as_str()
        .is_some_and(|message| message.contains("missing_matrix")));
}

#[tokio::test(flavor = "current_thread")]
async fn abort_discards_draft_over_http() {
    let (server, _state) = spawn().await;
    let file = TempPlan::new("identity", &identity_plan().encode());
    let body = json!({
        "plan_path": file.path(),
        "plan_sha256": file.sha256,
    })
    .to_string();
    let (_, prepared) = request("POST", &format!("{}/api/prepare", server.base), Some(&body)).await;
    let token = prepared["engine_token"]
        .as_str()
        .expect("token")
        .to_string();

    let body = json!({ "engine_token": token.clone() }).to_string();
    let (status, aborted) =
        request("POST", &format!("{}/api/abort", server.base), Some(&body)).await;
    assert_eq!(status, 200);
    assert_eq!(aborted["aborted"], Value::Bool(true));

    // 草稿已丢弃：commit 同一 token → 409。
    let (status, response) =
        request("POST", &format!("{}/api/commit", server.base), Some(&body)).await;
    assert_eq!(status, 409);
    assert_eq!(
        response["error"],
        Value::String("engine_token_invalid".into())
    );
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_path_is_404() {
    let (server, _state) = spawn().await;
    let (status, _) = request("GET", &format!("{}/api/nope", server.base), None).await;
    assert_eq!(status, 404);
}

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

fn run_block(state: &Arc<DaemonState>, input: &[f32]) -> Vec<f32> {
    let mut control = state.control();
    control
        .engine_mut()
        .run_block(input)
        .expect("block")
        .to_vec()
}

/// 确定性伪随机测试信号：值域 (-1, 1)。
fn signal(samples: usize, seed: u32) -> Vec<f32> {
    let mut state = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    let mut out = Vec::with_capacity(samples);
    for _ in 0..samples {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let unit = (state >> 8) as f32 / (1u32 << 24) as f32;
        out.push(unit * 2.0 - 1.0);
    }
    out
}

/// 全恒等方案：逐样本等于直通（identity 判据见 crates/host/tests/graph_executor.rs）。
fn identity_plan() -> Plan {
    let lanes = CHANNELS;
    let count = CHANNELS as usize;
    let mut matrix_coeffs = vec![0.0f32; count * count];
    for lane in 0..count {
        matrix_coeffs[lane * count + lane] = 1.0;
    }
    Plan {
        block_size: BLOCK_FRAMES,
        sample_rate: SAMPLE_RATE,
        content: Some(ContentSection {
            in_channels: CHANNELS,
            out_lanes: lanes,
            folds: (0..lanes)
                .map(|lane| Fold {
                    src: lane,
                    dst: lane,
                    gain: 1.0,
                })
                .collect(),
        }),
        crossover: Some(CrossoverSection {
            lanes,
            filters: (0..lanes)
                .map(|lane| CrossoverFilter {
                    lane,
                    coeffs: vec![1.0],
                })
                .collect(),
        }),
        matrix: Some(MatrixSection {
            rows: lanes,
            cols: lanes,
            decimation: 1,
            interpolation: 1,
            taps: 1,
            coeffs: matrix_coeffs,
        }),
        protection: Some(ProtectionSection {
            lanes,
            entries: (0..lanes)
                .map(|lane| ProtectionEntry {
                    lane,
                    delay: 0,
                    polarity: 1,
                    gain_db: 0.0,
                    peq: Vec::new(),
                })
                .collect(),
        }),
        routing: Some(RoutingSection {
            dac_channels: CHANNELS,
            routes: (0..lanes).map(|lane| (lane, lane)).collect(),
        }),
    }
}

/// identity 方案 + protection 每 lane 加 `gain_db` dB。
fn gain_plan(gain_db: f32) -> Plan {
    let mut plan = identity_plan();
    if let Some(protection) = &mut plan.protection {
        for entry in &mut protection.entries {
            entry.gain_db = gain_db;
        }
    }
    plan
}

/// 临时方案文件；Drop 时删除。
struct TempPlan {
    path: PathBuf,
    sha256: String,
}

impl TempPlan {
    fn new(tag: &str, bytes: &[u8]) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "khaen-t25b-rest-{}-{}-{}.plan",
            std::process::id(),
            tag,
            unique
        ));
        std::fs::write(&path, bytes).expect("write temp plan");
        Self {
            path,
            sha256: sha256_hex(bytes),
        }
    }

    fn path(&self) -> String {
        self.path.to_str().expect("temp path is utf-8").to_string()
    }
}

impl Drop for TempPlan {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn plan_inspect_is_read_only_and_reports_ready_plan() {
    let (server, _state) = spawn().await;
    let plan = identity_plan();
    let file = TempPlan::new("inspect", &plan.encode());
    let payload = format!(
        r#"{{"plan_path":{},"plan_sha256":{}}}"#,
        serde_json::to_string(&file.path()).expect("path json"),
        serde_json::to_string(&file.sha256).expect("sha json")
    );
    let (status, body) = request(
        "POST",
        &format!("{}/api/plan/inspect", server.base),
        Some(&payload),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(body["staged"], Value::Bool(false));
    assert_eq!(body["sha256"], Value::String(file.sha256.clone()));
    assert_eq!(body["graph_id"], Value::String(plan.graph_id_hex()));
    assert_eq!(body["admission"]["accepted"], Value::Bool(true));
    assert_eq!(body["compile"]["compiled"], Value::Bool(true));

    // 只读：没有草稿，commit 仍然 409。
    let (status, body) = request(
        "POST",
        &format!("{}/api/commit", server.base),
        Some(r#"{"engine_token":"00000000000000000000000000000000"}"#),
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(body["error"], Value::String("engine_token_invalid".into()));
}

#[tokio::test(flavor = "current_thread")]
async fn plan_inspect_bad_payloads_are_400() {
    let (server, _state) = spawn().await;
    let url = format!("{}/api/plan/inspect", server.base);
    let (status, body) = request("POST", &url, Some("{}")).await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], Value::String("bad_request".into()));

    let (status, _body) = request("POST", &url, Some(r#"{"plan_path":42}"#)).await;
    assert_eq!(status, 400);

    let (status, body) = request(
        "POST",
        &url,
        Some(r#"{"plan_path":"Z:/definitely/not/here.plan"}"#),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], Value::String("bad_request".into()));
}

#[tokio::test(flavor = "current_thread")]
async fn rollback_prepares_old_version_and_commits_over_http() {
    let (server, state) = spawn().await;
    // 没有历史：409 + no_history（如实报，不是 500）。
    let (status, body) =
        request("POST", &format!("{}/api/rollback", server.base), Some("{}")).await;
    assert_eq!(status, 409);
    assert_eq!(body["error"], Value::String("no_history".into()));

    // v1: identity
    let identity = identity_plan();
    let identity_id = identity.graph_id_hex();
    let file1 = TempPlan::new("rb-identity", &identity.encode());
    let body = json!({ "plan_path": file1.path(), "plan_sha256": file1.sha256 }).to_string();
    let (_, prepared) = request("POST", &format!("{}/api/prepare", server.base), Some(&body)).await;
    let token = prepared["engine_token"]
        .as_str()
        .expect("token")
        .to_string();
    let body = json!({ "engine_token": token }).to_string();
    let (status, _) = request("POST", &format!("{}/api/commit", server.base), Some(&body)).await;
    assert_eq!(status, 200);
    let input = signal((BLOCK_FRAMES * u32::from(CHANNELS)) as usize, 9);
    let output = run_block(&state, &input);
    assert_eq!(output, input);

    // v2: +6 dB
    let gain = gain_plan(6.0);
    let file2 = TempPlan::new("rb-gain", &gain.encode());
    let body = json!({ "plan_path": file2.path(), "plan_sha256": file2.sha256 }).to_string();
    let (_, prepared) = request("POST", &format!("{}/api/prepare", server.base), Some(&body)).await;
    let token = prepared["engine_token"]
        .as_str()
        .expect("token")
        .to_string();
    let body = json!({ "engine_token": token }).to_string();
    let (_, _) = request("POST", &format!("{}/api/commit", server.base), Some(&body)).await;
    let _ = run_block(&state, &input);

    // rollback 默认回到 v1，版本号递增到 3；commit 后输出恢复 identity。
    let (status, rolled) =
        request("POST", &format!("{}/api/rollback", server.base), Some("{}")).await;
    assert_eq!(status, 200);
    assert_eq!(rolled["rolled_back_to"], Value::from(1));
    assert_eq!(rolled["planned_version"], Value::from(3));
    assert_eq!(rolled["graph_id"], Value::String(identity_id.clone()));
    let token = rolled["engine_token"].as_str().expect("token").to_string();
    let body = json!({ "engine_token": token }).to_string();
    let (status, committed) =
        request("POST", &format!("{}/api/commit", server.base), Some(&body)).await;
    assert_eq!(status, 200);
    assert_eq!(committed["version"], Value::from(3));
    let output = run_block(&state, &input);
    assert_eq!(output, input);

    // 非法 target 类型：400 bad_request。
    let (status, body) = request(
        "POST",
        &format!("{}/api/rollback", server.base),
        Some(r#"{"target":"one"}"#),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], Value::String("bad_request".into()));
}

#[tokio::test(flavor = "current_thread")]
async fn format_two_phase_over_http() {
    let (server, state) = spawn().await;
    let (status, body) = request("GET", &format!("{}/api/status", server.base), None).await;
    assert_eq!(status, 200);
    assert_eq!(
        body["format"]["block_frames"],
        Value::from(u64::from(BLOCK_FRAMES))
    );
    assert_eq!(body["format"]["channels"], Value::from(u64::from(CHANNELS)));
    // 没有 rt-host 状态文件：plan_rate/device_rate 没有数据来源 → null + reason（不编数值）。
    assert_eq!(body["format"]["plan_rate"], Value::Null);
    assert!(body["reasons"]["format.plan_rate"].is_string());

    // 第一阶段：请求块长 64；commit 前不改配置。
    let (status, draft) = request(
        "POST",
        &format!("{}/api/format", server.base),
        Some(r#"{"chunksize":64}"#),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(draft["changed"], Value::Bool(true));
    assert_eq!(draft["format"]["block_size"], Value::from(64));
    assert_eq!(draft["device_restart_required"], Value::Bool(true));
    let token = draft["engine_token"].as_str().expect("token").to_string();
    let (_, before) = request("GET", &format!("{}/api/status", server.base), None).await;
    assert_eq!(
        before["format"]["block_frames"],
        Value::from(u64::from(BLOCK_FRAMES))
    );

    // 第二阶段：commit 重建执行器；状态如实显示新格式与“图被丢弃”。
    let body = json!({ "engine_token": token }).to_string();
    let (status, committed) =
        request("POST", &format!("{}/api/commit", server.base), Some(&body)).await;
    assert_eq!(status, 200);
    assert_eq!(committed["changed"], Value::Bool(true));
    assert_eq!(committed["graph_dropped"], Value::Bool(true));
    let (_, after) = request("GET", &format!("{}/api/status", server.base), None).await;
    assert_eq!(after["format"]["block_frames"], Value::from(64u64));
    // 图被丢弃（换格式重建执行器）在 /api/coeff 里如实为 null。
    let (_, coefficients) = request("GET", &format!("{}/api/coeff", server.base), None).await;
    assert_eq!(coefficients["graph_id"], Value::Null);

    // 非法请求：400 bad_request；空对象要有明确原因。
    let (status, body) = request("POST", &format!("{}/api/format", server.base), Some("{}")).await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], Value::String("bad_request".into()));
    assert!(body["message"]
        .as_str()
        .is_some_and(|message| message.contains("sample_rate") || message.contains("chunksize")));
    let (status, body) = request(
        "POST",
        &format!("{}/api/format", server.base),
        Some(r#"{"sample_rate":44100}"#),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], Value::String("bad_request".into()));
    let _ = state;
}

#[tokio::test(flavor = "current_thread")]
async fn format_draft_is_abortable_over_http() {
    let (server, _state) = spawn().await;
    let (_, draft) = request(
        "POST",
        &format!("{}/api/format", server.base),
        Some(r#"{"chunksize":64}"#),
    )
    .await;
    let token = draft["engine_token"].as_str().expect("token").to_string();
    let body = json!({ "engine_token": token }).to_string();
    let (status, aborted) =
        request("POST", &format!("{}/api/abort", server.base), Some(&body)).await;
    assert_eq!(status, 200);
    assert_eq!(aborted["aborted_surface"], Value::String("format".into()));
    // abort 后 commit 无效，格式没变。
    let (status, body) = request("POST", &format!("{}/api/commit", server.base), Some(&body)).await;
    assert_eq!(status, 409);
    assert_eq!(body["error"], Value::String("engine_token_invalid".into()));
    let (_, status_body) = request("GET", &format!("{}/api/status", server.base), None).await;
    assert_eq!(
        status_body["format"]["block_frames"],
        Value::from(u64::from(BLOCK_FRAMES))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn mode_off_and_on_over_http() {
    let (server, state) = spawn().await;
    let input = signal((BLOCK_FRAMES * u32::from(CHANNELS)) as usize, 0x31);
    let passthrough = run_block(&state, &input);
    assert_eq!(passthrough, input);

    // 两阶段：off 只准备；commit 后块边界静音。
    let body = json!({ "surface": "mode", "data": { "mode": "off" } }).to_string();
    let (status, draft) =
        request("POST", &format!("{}/api/surface", server.base), Some(&body)).await;
    assert_eq!(status, 200);
    assert_eq!(draft["changed"], Value::Bool(true));
    assert_eq!(draft["mode"], Value::String("off".into()));
    let token = draft["engine_token"].as_str().expect("token").to_string();

    let body = json!({ "engine_token": token }).to_string();
    let (status, committed) =
        request("POST", &format!("{}/api/commit", server.base), Some(&body)).await;
    assert_eq!(status, 200);
    assert_eq!(committed["mode"], Value::String("off".into()));
    let muted = run_block(&state, &input);
    assert!(muted.iter().all(|sample| sample.to_bits() == 0));

    // 切回 aes67：解除静音。
    let body = json!({ "surface": "mode", "data": { "mode": "aes67" } }).to_string();
    let (_, draft) = request("POST", &format!("{}/api/surface", server.base), Some(&body)).await;
    let token = draft["engine_token"].as_str().expect("token").to_string();
    let body = json!({ "engine_token": token }).to_string();
    let (status, _) = request("POST", &format!("{}/api/commit", server.base), Some(&body)).await;
    assert_eq!(status, 200);
    let unmuted = run_block(&state, &input);
    assert_eq!(unmuted, input);
}

#[tokio::test(flavor = "current_thread")]
async fn calibration_plan_roundtrips_through_control_plane() {
    let (server, state) = spawn().await;

    // 真实校准产物：两个通道各一条 +4 dB / 100 Hz 模态滤波，lane 增益 0 / -1 dB。
    let design = calibration_design();
    let options = PlanCompileOptions {
        sample_rate: SAMPLE_RATE,
        block_size: BLOCK_FRAMES,
        channels: CHANNELS,
        lane_gain_db: vec![0.0, -1.0],
        lane_delay: Vec::new(),
    };
    let compiled = compile_peq_design(&design, &options).expect("compile calibration plan");
    let decoded = Plan::decode(&compiled.bytes).expect("decode compiled plan");
    assert_eq!(compiled.sha256, sha256_hex(&compiled.bytes));
    assert_eq!(compiled.graph_id, decoded.graph_id_hex());
    // 同输入再编译一次：字节与 sha256 逐位相同（可复现 sha 是目标④的硬要求）。
    let again = compile_peq_design(&design, &options).expect("compile again");
    assert_eq!(compiled.bytes, again.bytes, "同输入必须逐字节可复现");
    assert_eq!(compiled.sha256, again.sha256);

    let file = TempPlan::new("calibration", &compiled.bytes);

    // 只读 inspect：不产生草稿、不改变正在跑的图。
    let body = json!({ "plan_path": file.path(), "plan_sha256": file.sha256 }).to_string();
    let (status, inspected) = request(
        "POST",
        &format!("{}/api/plan/inspect", server.base),
        Some(&body),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(inspected["staged"], Value::Bool(false));
    assert_eq!(inspected["admission"]["accepted"], Value::Bool(true));
    assert_eq!(
        inspected["graph_id"],
        Value::String(compiled.graph_id.clone())
    );

    // 错误的 sha256：400，不进入草稿。
    let bad = json!({ "plan_path": file.path(), "plan_sha256": "0".repeat(64) }).to_string();
    let (status, response) =
        request("POST", &format!("{}/api/prepare", server.base), Some(&bad)).await;
    assert_eq!(status, 400);
    assert_eq!(response["error"], Value::String("bad_request".into()));

    // 两阶段：prepare 只产生草稿，commit 才在块边界生效。
    let (status, prepared) =
        request("POST", &format!("{}/api/prepare", server.base), Some(&body)).await;
    assert_eq!(status, 200);
    assert_eq!(prepared["planned_version"], Value::from(1));
    assert_eq!(
        prepared["graph_id"],
        Value::String(compiled.graph_id.clone())
    );
    assert_eq!(prepared["admission"]["accepted"], Value::Bool(true));
    let token = prepared["engine_token"]
        .as_str()
        .expect("token")
        .to_string();
    let (_, before) = request("GET", &format!("{}/api/coeff", server.base), None).await;
    assert_eq!(
        before["version"],
        Value::Null,
        "还没提交过方案：coeff.version 如实为 null"
    );
    assert_eq!(before["applied_block"], Value::Null);
    let body = json!({ "engine_token": token }).to_string();
    let (status, committed) =
        request("POST", &format!("{}/api/commit", server.base), Some(&body)).await;
    assert_eq!(status, 200);
    assert_eq!(committed["version"], Value::from(1));
    assert_eq!(
        committed["graph_id"],
        Value::String(compiled.graph_id.clone())
    );

    // 图里确实有校准滤波器：逐 lane 用方案系数独立复算，输出必须一致。
    let input = signal((BLOCK_FRAMES * u32::from(CHANNELS)) as usize, 21);
    let calibrated = run_block(&state, &input);
    assert_ne!(calibrated, input, "方案含 +4 dB 模态滤波，输出不应等于直通");
    let protection = decoded.protection.as_ref().expect("protection section");
    for lane in 0..usize::from(CHANNELS) {
        let entry = protection
            .entries
            .iter()
            .find(|entry| usize::from(entry.lane) == lane)
            .expect("lane entry");
        let expected = expected_lane(&input, lane, entry, BLOCK_FRAMES as usize);
        for frame in 0..BLOCK_FRAMES as usize {
            let actual = calibrated[frame * usize::from(CHANNELS) + lane];
            assert!(
                (actual - expected[frame]).abs() <= 1e-4,
                "lane {lane} frame {frame}: {actual} vs {}",
                expected[frame]
            );
        }
    }

    // 换回恒等方案 v2，再 rollback 回校准方案 v1：版本递增，但图回到校准产物。
    let identity = TempPlan::new("calibration-identity", &identity_plan().encode());
    let body = json!({ "plan_path": identity.path(), "plan_sha256": identity.sha256 }).to_string();
    let (_, prepared) = request("POST", &format!("{}/api/prepare", server.base), Some(&body)).await;
    let token = prepared["engine_token"]
        .as_str()
        .expect("token")
        .to_string();
    let body = json!({ "engine_token": token }).to_string();
    let _ = request("POST", &format!("{}/api/commit", server.base), Some(&body)).await;
    let passthrough = run_block(&state, &input);
    assert_eq!(passthrough, input);

    let (status, rolled) =
        request("POST", &format!("{}/api/rollback", server.base), Some("{}")).await;
    assert_eq!(status, 200);
    assert_eq!(rolled["rolled_back_to"], Value::from(1));
    assert_eq!(rolled["planned_version"], Value::from(3));
    assert_eq!(rolled["graph_id"], Value::String(compiled.graph_id.clone()));
    let token = rolled["engine_token"].as_str().expect("token").to_string();
    let body = json!({ "engine_token": token }).to_string();
    let (status, committed) =
        request("POST", &format!("{}/api/commit", server.base), Some(&body)).await;
    assert_eq!(status, 200);
    assert_eq!(committed["version"], Value::from(3));
    let restored = run_block(&state, &input);
    assert_eq!(
        restored, calibrated,
        "rollback 后应与第一次下发的校准图逐样本一致"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn fir_plan_roundtrips_through_control_plane() {
    let (server, state) = spawn().await;

    // 真实尺寸 FIR：2 lane × 2048 taps；lane 0 = 延迟 1024 的冲激，lane 1 = 全静音。
    let taps = 2048usize;
    let mut lane0 = vec![0.0f64; taps];
    lane0[1024] = 1.0;
    let lanes = vec![
        LaneFir {
            lane: 0,
            taps: lane0,
        },
        LaneFir {
            lane: 1,
            taps: vec![0.0; taps],
        },
    ];
    let config = FirPlanConfig::new(BLOCK_FRAMES, SAMPLE_RATE, CHANNELS);
    let compiled = compile_fir_plan(&lanes, &config).expect("compile fir plan");
    assert_eq!(compiled.sha256(), sha256_hex(compiled.bytes()));
    // 同输入再编译一次：字节与 sha256 逐位相同（目标④的可复现 sha）。
    let again = compile_fir_plan(&lanes, &config).expect("compile again");
    assert_eq!(compiled.bytes(), again.bytes());
    assert_eq!(compiled.sha256(), again.sha256());

    let file = TempPlan::new("fir-calibration", compiled.bytes());
    let body = json!({ "plan_path": file.path(), "plan_sha256": file.sha256 }).to_string();

    // 只读 inspect：准入接受、不产生草稿。
    let (status, inspected) = request(
        "POST",
        &format!("{}/api/plan/inspect", server.base),
        Some(&body),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(inspected["staged"], Value::Bool(false));
    assert_eq!(inspected["admission"]["accepted"], Value::Bool(true));
    assert_eq!(
        inspected["graph_id"],
        Value::String(compiled.graph_id().into())
    );

    // 两阶段下发：prepare 草稿 -> commit 生效。
    let (status, prepared) =
        request("POST", &format!("{}/api/prepare", server.base), Some(&body)).await;
    assert_eq!(status, 200);
    assert_eq!(prepared["planned_version"], Value::from(1));
    assert_eq!(
        prepared["graph_id"],
        Value::String(compiled.graph_id().into())
    );
    let token = prepared["engine_token"]
        .as_str()
        .expect("token")
        .to_string();
    let (status, committed) = request(
        "POST",
        &format!("{}/api/commit", server.base),
        Some(&json!({ "engine_token": token }).to_string()),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(committed["version"], Value::from(1));
    assert_eq!(
        committed["graph_id"],
        Value::String(compiled.graph_id().into())
    );

    // 图内 FIR：单脉冲进，应在第 1024 帧出现、其余为 0（lane 1 全程静音）。
    let channels = usize::from(CHANNELS);
    let frames = BLOCK_FRAMES as usize;
    let mut impulse = vec![0.0f32; frames * channels];
    impulse[0] = 1.0;
    let zeros = vec![0.0f32; frames * channels];
    let mut output = Vec::new();
    for block in 0..9usize {
        let input = if block == 0 { &impulse } else { &zeros };
        output.extend(run_block(&state, input));
    }
    let impulse_frame = 1024usize;
    for frame in 0..output.len() / channels {
        for lane in 0..channels {
            let value = output[frame * channels + lane];
            if frame == impulse_frame && lane == 0 {
                assert!(
                    (value - 1.0).abs() <= 1e-6,
                    "impulse at frame {frame}: {value}"
                );
            } else {
                assert!(
                    value.abs() <= 1e-6,
                    "frame {frame} lane {lane} should be silent, got {value}"
                );
            }
        }
    }

    // 换回恒等方案 v2：直通。
    let identity_file = TempPlan::new("fir-identity", &identity_plan().encode());
    let identity_body =
        json!({ "plan_path": identity_file.path(), "plan_sha256": identity_file.sha256 })
            .to_string();
    let (_, prepared) = request(
        "POST",
        &format!("{}/api/prepare", server.base),
        Some(&identity_body),
    )
    .await;
    let token = prepared["engine_token"]
        .as_str()
        .expect("token")
        .to_string();
    let _ = request(
        "POST",
        &format!("{}/api/commit", server.base),
        Some(&json!({ "engine_token": token }).to_string()),
    )
    .await;
    let probe = signal(frames * channels, 31);
    assert_eq!(run_block(&state, &probe), probe);

    // rollback 回 v1 FIR 方案：图回到 FIR，脉冲延迟行为重新出现。
    let (status, rolled) =
        request("POST", &format!("{}/api/rollback", server.base), Some("{}")).await;
    assert_eq!(status, 200);
    assert_eq!(rolled["rolled_back_to"], Value::from(1));
    assert_eq!(
        rolled["graph_id"],
        Value::String(compiled.graph_id().into())
    );
    let token = rolled["engine_token"].as_str().expect("token").to_string();
    let _ = request(
        "POST",
        &format!("{}/api/commit", server.base),
        Some(&json!({ "engine_token": token }).to_string()),
    )
    .await;
    let mut restored = Vec::new();
    for block in 0..9usize {
        let input = if block == 0 { &impulse } else { &zeros };
        restored.extend(run_block(&state, input));
    }
    for frame in 0..restored.len() / channels {
        for lane in 0..channels {
            let value = restored[frame * channels + lane];
            if frame == impulse_frame && lane == 0 {
                assert!((value - 1.0).abs() <= 1e-6, "restored impulse: {value}");
            } else {
                assert!(
                    value.abs() <= 1e-6,
                    "restored frame {frame} lane {lane}: {value}"
                );
            }
        }
    }
}

/// 用方案里的保护项系数独立复算一条 lane 的期望输出（f64 计算，f32 系数）。
fn expected_lane(
    input: &[f32],
    lane: usize,
    entry: &khaen_host::plan::ProtectionEntry,
    frames: usize,
) -> Vec<f32> {
    assert_eq!(entry.peq.len(), 1, "夹具每条 lane 只有一段 PEQ");
    let section = entry.peq[0];
    let channels = usize::from(CHANNELS);
    let gain = f64::from(10f32.powf(entry.gain_db / 20.0));
    let (mut y1, mut y2, mut x1, mut x2) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    let mut out = Vec::with_capacity(frames);
    for frame in 0..frames {
        let x = f64::from(input[frame * channels + lane]);
        let y = f64::from(section.b0) * x + f64::from(section.b1) * x1 + f64::from(section.b2) * x2
            - f64::from(section.a1) * y1
            - f64::from(section.a2) * y2;
        out.push((y * gain) as f32);
        y2 = y1;
        y1 = y;
        x2 = x1;
        x1 = x;
    }
    out
}

/// 校准设计夹具：两个通道各一条 +4 dB / 100 Hz 模态滤波。
fn calibration_design() -> PeqDesign {
    let mut design = PeqDesign::new(SAMPLE_RATE, DesignMode::ModalOnly).expect("design");
    for (name, index) in [("FL", 0u32), ("FR", 1u32)] {
        let mut channel = ChannelPeq::new(name, index).expect("channel");
        channel.push_band(
            ModalBand::from_params(BandType::Peaking, 100.0, 4.0, 4.0, f64::from(SAMPLE_RATE))
                .expect("band"),
        );
        design.add_channel(channel).expect("add channel");
    }
    design
}
