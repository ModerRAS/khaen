//! REST 端到端测试：真实 TCP 连接打真实 axum 服务，不用 mock。
//!
//! 覆盖：能力上报（wired/not_wired 真实）、方案 prepare/commit/abort/coeff 的真实行为与
//! 错误码 → HTTP 状态码映射（bad_request→400、admission_rejected→409、
//! engine_token_invalid→409、unknown_op→404）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use khaen_daemon::{router, DaemonState};
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

struct TestServer {
    base: String,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn spawn() -> (TestServer, Arc<DaemonState>) {
    let state = Arc::new(DaemonState::new(EngineConfig::default()).expect("state"));
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let app = router(Arc::clone(&state));
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (
        TestServer {
            base: format!("http://{addr}"),
            handle,
        },
        state,
    )
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

    // 未接通界面：501 + 原因，不返回假成功。
    let body = json!({ "surface": "mode", "data": {} }).to_string();
    let (status, response) =
        request("POST", &format!("{}/api/surface", server.base), Some(&body)).await;
    assert_eq!(status, 501);
    assert_eq!(response["error"], Value::String("unsupported".into()));

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

    // 能力上报：接通/未接通界面逐项如实。
    let (_, status_json) = request("GET", &format!("{}/api/status", server.base), None).await;
    for surface in ["volume", "routing", "layout", "format"] {
        assert_eq!(
            status_json["surfaces"][surface],
            Value::Bool(true),
            "{surface}"
        );
    }
    assert_eq!(status_json["surfaces"]["mode"], Value::Bool(false));
    assert!(status_json["surface_reasons"]["mode"]
        .as_str()
        .is_some_and(|reason| !reason.is_empty()));
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
    let (server, _state) = spawn().await;
    let (status, body) = request("GET", &format!("{}/api/status", server.base), None).await;
    assert_eq!(status, 200);
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(body["runtime_state"], Value::String("idle".into()));
    // 全部 op 已接通：wired 完整、not_wired 为空。
    let wired: Vec<&str> = body["wired"]
        .as_array()
        .expect("wired")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for op in ["status", "mute", "prepare", "commit", "abort", "coeff"] {
        assert!(wired.contains(&op), "wired 缺 {op}: {wired:?}");
    }
    assert!(body["not_wired"]
        .as_array()
        .is_some_and(|ops| ops.is_empty()));
    assert_eq!(body["graph_id"], Value::Null);
    assert_eq!(body["epoch"], Value::Null);
    assert!(body["reasons"]["graph_id"].is_string());
    // 设备监督器默认未接线：如实上报 wired=false + 原因，不编造 device_rate。
    assert_eq!(body["device"]["wired"], Value::Bool(false));
    assert!(body["device"]["reason"]
        .as_str()
        .is_some_and(|r| !r.is_empty()));
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

    let (status, body) = request("GET", &format!("{}/api/status", server.base), None).await;
    assert_eq!(status, 200);
    assert_eq!(body["graph_id"], Value::String(identity_id.clone()));
    assert_eq!(body["epoch"], Value::from(1));
    assert_eq!(body["actual_version"], Value::from(1));
    assert_eq!(body["blocks"], Value::from(1));

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

    let (_, body) = request("GET", &format!("{}/api/status", server.base), None).await;
    assert_eq!(body["graph_id"], Value::String(gain_id.clone()));
    assert_eq!(body["epoch"], Value::from(2));
    assert_eq!(body["actual_version"], Value::from(2));
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
    assert_eq!(body["format"]["block_size"], Value::from(BLOCK_FRAMES));
    assert_eq!(body["format"]["sample_rate"], Value::from(SAMPLE_RATE));
    assert_eq!(body["format"]["channels"], Value::from(CHANNELS));

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
    assert_eq!(before["format"]["block_size"], Value::from(BLOCK_FRAMES));

    // 第二阶段：commit 重建执行器；状态如实显示新格式与“图被丢弃”。
    let body = json!({ "engine_token": token }).to_string();
    let (status, committed) =
        request("POST", &format!("{}/api/commit", server.base), Some(&body)).await;
    assert_eq!(status, 200);
    assert_eq!(committed["changed"], Value::Bool(true));
    assert_eq!(committed["graph_dropped"], Value::Bool(true));
    let (_, after) = request("GET", &format!("{}/api/status", server.base), None).await;
    assert_eq!(after["format"]["block_size"], Value::from(64));
    assert_eq!(after["graph_id"], Value::Null);

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
        status_body["format"]["block_size"],
        Value::from(BLOCK_FRAMES)
    );
}
