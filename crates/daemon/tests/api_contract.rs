//! daemon HTTP API 合同测试（docs/API.md v1.1）：端点 shape、稳定错误码、诚实降级语义。
//!
//! 覆盖：§1 认证/CSRF 与错误体、§3 状态与只读视图、§4 控制面两阶段、§5 /api/channels、
//! §8 系统 restart/password、§7 校准 not_wired 诚实语义。
//!
//! ## 错误体形状（合同 §1 / §11-A）
//!
//! - 新端点（auth/system/plan/channels/calibration 等）已是 M1 形状
//!   {error:{code,message,details}}，用 assert_m1_error 严格断言。
//! - 旧控制面端点（/api/mute、/api/prepare、/api/commit、/api/abort、/api/coeff、
//!   /api/surface、/api/format、/api/rollback、/api/device*，实现在 crates/daemon/src/lib.rs）
//!   仍返回 legacy 形状 {ok:false,error:<code>,message}。合同 §11-A 的目标是全站 M1，
//!   但本会话不迁移旧端点；这里用 assert_control_error 容忍两种形状并锁定 HTTP 状态与稳定码。
//! - host_link 转发的 rt-host 错误体是合同允许的第三种形状（{ok:false,error:<code>,...}），
//!   同样被 assert_control_error 接受。
//!
//! ## 已知合同偏差（不写红色测试、不改 src；差异清单见任务报告）
//!
//! 1. 旧控制面端点（见上）仍是非 M1 的 legacy 错误体；合同 §11-A 的收口（全站 M1）未完成。
//! 2. /api/device 未接线时 state 值为 null（键存在），reason 在 reason/source；这是诚实降级。

mod support;

use std::sync::Arc;

use khaen_daemon::DaemonState;
use serde_json::{json, Value};
use support::plan_fixture::{gain_plan, identity_plan, TempPlan};

const STATUS: &str = "/api/status";
const DEVICE: &str = "/api/device";
const COEFF: &str = "/api/coeff";
const HISTORY: &str = "/api/plan/history";
const INSPECT: &str = "/api/plan/inspect";
const MOTU: &str = "/api/motu";
const META: &str = "/api/meta";
const CHANNELS: &str = "/api/channels";
const WIZARD: &str = "/api/calibration/wizard";
const WIZARD_STATE: &str = "/api/calibration/wizard/state";
const JOBS: &str = "/api/calibration/jobs";
const UPLOADS: &str = "/api/calibration/uploads";
const PREPARE: &str = "/api/prepare";
const COMMIT: &str = "/api/commit";
const ABORT: &str = "/api/abort";
const ROLLBACK: &str = "/api/rollback";
const SURFACE: &str = "/api/surface";
const FORMAT: &str = "/api/format";
const MUTE: &str = "/api/mute";
const RESTART: &str = "/api/system/restart";
const PASSWORD: &str = "/api/system/password";

// ---------------------------------------------------------------------------
// 错误体断言（M1 / legacy / 转发三种形状）
// ---------------------------------------------------------------------------

/// 从 M1 或 legacy 错误体里提取稳定错误码。
fn error_code(body: &Value) -> Option<&str> {
    match body.get("error") {
        Some(Value::Object(object)) => object.get("code").and_then(Value::as_str),
        Some(Value::String(code)) => Some(code.as_str()),
        _ => None,
    }
}

/// 从 M1 或 legacy 错误体里提取人读 message。
fn error_message(body: &Value) -> Option<&str> {
    match body.get("error") {
        Some(Value::Object(object)) => object.get("message").and_then(Value::as_str),
        _ => body.get("message").and_then(Value::as_str),
    }
}

/// 严格断言 M1 错误体（合同 §1 / §11-A：新增 daemon 端点一律 M1）。
fn assert_m1_error(response: &support::HttpResponse, expected_status: u16, expected_code: &str) {
    assert_eq!(
        response.status,
        expected_status,
        "unexpected status, body: {}",
        response.body_text()
    );
    let body = response.json();
    let error = body
        .get("error")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("M1 错误体必须把 error 做成对象: {body}"));
    assert_eq!(
        error.get("code").and_then(Value::as_str),
        Some(expected_code),
        "unexpected error code: {body}"
    );
    assert!(
        error
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|message| !message.is_empty()),
        "M1 错误体 message 不能为空: {body}"
    );
    if let Some(details) = body.get("error").and_then(|error| error.get("details")) {
        assert!(details.is_object(), "M1 details 必须是对象: {body}");
    }
}

/// 断言控制面错误：HTTP 状态 + 稳定码（同时接受 M1 与 legacy 形状，见文件头）。
fn assert_control_error(
    response: &support::HttpResponse,
    expected_status: u16,
    expected_codes: &[&str],
) {
    assert_eq!(
        response.status,
        expected_status,
        "unexpected status, body: {}",
        response.body_text()
    );
    let body = response.json();
    let code = error_code(&body).unwrap_or_else(|| panic!("错误体缺少可提取的稳定码: {body}"));
    assert!(
        expected_codes.contains(&code),
        "unexpected error code {code:?}: {body}"
    );
    assert!(
        error_message(&body).is_some_and(|message| !message.is_empty()),
        "错误 message 不能为空: {body}"
    );
}

/// 只读端点的 ok 存在时必须是 true（/api/device/retry 历史无 ok，合同 §1 允许）。
fn assert_ok_if_present(body: &Value) {
    if let Some(ok) = body.get("ok") {
        assert_eq!(ok, &Value::Bool(true), "ok 存在时必须是 true: {body}");
    }
}

// ---------------------------------------------------------------------------
// 控制面两阶段辅助
// ---------------------------------------------------------------------------

/// 写一份临时方案文件、POST /api/prepare 并返回 (方案文件, prepare 回执)。
async fn prepare_identity_plan(server: &support::TestServer, tag: &str) -> (TempPlan, Value) {
    let plan = TempPlan::new(tag, &identity_plan().encode());
    let payload = json!({
        "plan_path": plan.path(),
        "plan_sha256": plan.sha256(),
    });
    let response = support::request_json(server, "POST", PREPARE, &payload.to_string()).await;
    assert_eq!(
        response.status,
        200,
        "prepare 必须 200: {}",
        response.body_text()
    );
    (plan, response.json())
}

/// prepare + commit 一份 identity 基线方案（surface 的 no_base_plan 前置条件）。
async fn commit_identity_plan(server: &support::TestServer) {
    let (plan, prepared) = prepare_identity_plan(server, "contract-base").await;
    let token = prepared["engine_token"]
        .as_str()
        .expect("prepare.engine_token")
        .to_string();
    let response = support::request_json(
        server,
        "POST",
        COMMIT,
        &json!({ "engine_token": token }).to_string(),
    )
    .await;
    assert_eq!(
        response.status,
        200,
        "commit 基线方案必须 200: {}",
        response.body_text()
    );
    drop(plan);
}

/// 上传一段字节到校准 uploads，返回 upload_id（内容 sha256）。
async fn upload_bytes(server: &support::TestServer, bytes: &[u8]) -> String {
    let response =
        support::request_bytes(server, "POST", UPLOADS, "application/octet-stream", bytes).await;
    assert_eq!(
        response.status,
        201,
        "calibration upload 必须 201: {}",
        response.body_text()
    );
    response.json()["upload_id"]
        .as_str()
        .expect("upload_id")
        .to_string()
}

/// 跑一个块，让 commit 的草稿在块边界真正生效（版本号/图才会推进）。
fn run_block(state: &Arc<DaemonState>, input: &[f32]) -> Vec<f32> {
    state
        .control()
        .engine_mut()
        .run_block(input)
        .expect("run block")
        .to_vec()
}

/// 恒定测试信号（默认引擎块长 128 × 2 声道）。
fn test_input() -> Vec<f32> {
    (0..128 * 2)
        .map(|index| (index as f32 * 0.01) - 1.0)
        .collect()
}

// ---------------------------------------------------------------------------
// §1 认证 / CSRF（错误体 M1）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn unauthenticated_read_is_401_m1() {
    let server = support::spawn().await;
    // 不带 Cookie / CSRF：/api 全局认证中间件必须 401 + M1 错误体。
    let response = support::request_with(&server, "GET", STATUS, &[], None).await;
    assert_m1_error(&response, 401, "unauthorized");
}

#[tokio::test(flavor = "current_thread")]
async fn write_without_csrf_is_forbidden_m1() {
    let server = support::spawn().await;
    let session = server.session().expect("session");
    // 只带会话 Cookie、缺 X-KHAEN-CSRF：写请求必须被拒绝。实测 403 forbidden（合同 §1 码表），
    // 合同允许 403/401，这里锁定实际语义（M1）。
    let headers = [(
        "Cookie".to_string(),
        format!("khaen_session={}", session.token),
    )];
    let response =
        support::request_with(&server, "POST", MUTE, &headers, Some(r#"{"mute":true}"#)).await;
    assert_m1_error(&response, 403, "forbidden");
    assert!(
        response.body_text().to_ascii_lowercase().contains("csrf"),
        "错误信息应说明 CSRF: {}",
        response.body_text()
    );
}

// ---------------------------------------------------------------------------
// §1 + §7 校准 not_wired 的诚实语义与 M1 错误体
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn calibration_not_wired_is_m1_and_job_list_is_honest() {
    let server = support::spawn().await;

    // 列表：200 + available=false + 非空 reason（不假装“有 0 个 job”）。
    let response = support::request(&server, "GET", JOBS).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(body["available"], Value::Bool(false));
    assert!(
        body["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "未接线必须给非空 reason: {body}"
    );
    assert!(body["jobs"].is_array(), "jobs 必须是数组: {body}");

    // 合法请求（上传引用真实存在）在 NotWired 后端上必须 503 not_wired + M1 错误体。
    let baseline = upload_bytes(&server, b"contract-test-baseline").await;
    let applied = upload_bytes(&server, b"contract-test-applied").await;
    let plan_sha = khaen_host::sha256_hex(&identity_plan().encode());
    let job = json!({
        "kind": "single_point_offline",
        "inputs": {
            "plan_sha": plan_sha,
            "baseline_upload_id": baseline,
            "applied_upload_id": applied,
            "centers_hz": [100.0, 1000.0, 10000.0],
            "tolerance_db": 2.0,
            "min_bands": 3,
        }
    });
    let response = support::request_json(&server, "POST", JOBS, &job.to_string()).await;
    assert_m1_error(&response, 503, "not_wired");

    // /api/meta 必须把 calibration.jobs 如实列进 not_wired（op + 非空 reason）。
    let meta = support::request(&server, "GET", META).await;
    assert_eq!(meta.status, 200);
    let meta = meta.json();
    assert_ok_if_present(&meta);
    let not_wired = meta["not_wired"].as_array().expect("meta.not_wired");
    let calibration_entry = not_wired
        .iter()
        .find(|entry| entry["op"] == json!("calibration.jobs"))
        .unwrap_or_else(|| panic!("/api/meta 必须列出 calibration.jobs: {meta}"));
    assert!(
        calibration_entry["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "not_wired 条目必须带非空 reason: {calibration_entry}"
    );
}

// ---------------------------------------------------------------------------
// §3 状态与只读视图
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn status_is_always_200_with_schema_and_honest_not_wired() {
    let server = support::spawn().await;
    let response = support::request(&server, "GET", STATUS).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert_eq!(response.header("content-type"), Some("application/json"));
    let body = response.json();

    // 合同 §3 的 schema 与字段骨架（ok 在合同里可缺省，但当前实现已带且必须为 true）。
    assert_eq!(body["schema"], json!("khaen-status/v1"));
    assert_eq!(body["ok"], Value::Bool(true));
    for key in [
        "daemon",
        "host_link",
        "format",
        "input",
        "output",
        "plan",
        "not_wired",
        "reasons",
    ] {
        assert!(
            body.get(key).is_some(),
            "/api/status 缺少合同字段 {key}: {body}"
        );
    }

    // daemon 自身字段来自真实进程。
    assert_eq!(body["daemon"]["version"], json!(env!("CARGO_PKG_VERSION")));
    assert_eq!(body["daemon"]["bind"], json!(server.addr));
    assert!(body["daemon"]["uptime_s"].is_u64());
    assert!(body["host_link"]["connected"].is_boolean());
    assert!(body["host_link"]["target"].is_string());

    // 运行时 mode（CONTRACTS B.1.5 / §9.7 G3 的加法投影）：process 目标下无 rt-host，
    // 必须是 null + 原因，不编造、也不拿 host_link.mode（传输模式）冒充。
    assert!(
        body["mode"].is_null() || body["mode"].is_string(),
        "mode 必须是 null 或字符串: {body}"
    );
    if body["mode"].is_null() {
        assert!(
            body["reasons"]["mode"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty()),
            "mode 为 null 时必须给原因: {body}"
        );
    }

    // 诚实降级：not_wired 每项恰好 {path, reason}，reason 非空。
    let not_wired = body["not_wired"].as_array().expect("not_wired 必须是数组");
    assert!(!not_wired.is_empty(), "process 目标下应有未接线项");
    for entry in not_wired {
        let object = entry.as_object().expect("not_wired 条目必须是对象");
        assert_eq!(
            object.len(),
            2,
            "not_wired 条目必须恰好 {{path, reason}}: {entry}"
        );
        assert!(object.get("path").is_some(), "缺少 path: {entry}");
        assert!(
            object
                .get("reason")
                .and_then(Value::as_str)
                .is_some_and(|reason| !reason.is_empty()),
            "not_wired.reason 不能为空: {entry}"
        );
    }
    assert!(body["reasons"].is_object());

    // 合同 §3 的 muted/safe/reconfigure：顶层键必须存在（拿不到时 null + reasons 给原因）。
    for key in ["muted", "safe", "reconfigure"] {
        assert!(
            body.get(key).is_some(),
            "/api/status 缺少合同字段 {key}: {body}"
        );
    }
    assert!(
        body["muted"].is_null() || body["muted"].is_boolean(),
        "muted 必须是 null 或 bool: {body}"
    );
    assert!(
        body["safe"].is_null() || body["safe"].is_boolean(),
        "safe 必须是 null 或 bool: {body}"
    );
    assert!(
        body["reconfigure"].is_null() || body["reconfigure"].is_object(),
        "reconfigure 必须是 null 或对象: {body}"
    );
    // state 块保留（muted/safe 的逐状态视图 + last_error）。
    assert!(body["state"].is_object(), "state 必须是对象: {body}");
}

#[tokio::test(flavor = "current_thread")]
async fn device_snapshot_has_wired_and_state() {
    let server = support::spawn().await;
    let response = support::request(&server, "GET", DEVICE).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    // 合同 §3：设备监督器快照 {wired, state, ...}，ok 可缺省。
    assert!(body["wired"].is_boolean(), "wired 必须是 bool: {body}");
    assert!(
        body.get("state").is_some(),
        "device 必须含 state 键: {body}"
    );
    if body["wired"] == Value::Bool(true) {
        assert!(
            !body["state"].is_null(),
            "已接线时 state 不能为 null: {body}"
        );
    } else {
        // 未接线：state=null + 非空原因（不编设备状态）。
        assert!(
            body["state"].is_null(),
            "未接线时 state 必须为 null: {body}"
        );
        assert!(
            body["reason"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty()),
            "未接线必须给非空 reason: {body}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn coeff_reports_ok_true() {
    let server = support::spawn().await;
    let response = support::request(&server, "GET", COEFF).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], Value::Bool(true));
    assert!(body.get("version").is_some(), "缺少 version: {body}");
    assert!(body.get("graph_id").is_some(), "缺少 graph_id: {body}");
}

#[tokio::test(flavor = "current_thread")]
async fn plan_history_reports_ok_and_versions_alias() {
    let server = support::spawn().await;
    let (plan, prepared) = prepare_identity_plan(&server, "contract-history").await;
    let token = prepared["engine_token"]
        .as_str()
        .expect("prepare.engine_token")
        .to_string();
    let commit = support::request_json(
        &server,
        "POST",
        COMMIT,
        &json!({ "engine_token": token }).to_string(),
    )
    .await;
    assert_eq!(commit.status, 200, "{}", commit.body_text());

    let response = support::request(&server, "GET", HISTORY).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(body["schema"], json!("khaen-plan-history/v1"));
    assert!(body["current"].is_object(), "current 必须是对象: {body}");
    let entries = body["entries"].as_array().expect("entries 必须是数组");
    assert!(!entries.is_empty(), "commit 后历史必须至少一条: {body}");
    // §11-D：versions 是 entries 的别名（不改旧字段），必须逐元素深度相等。
    let versions = body["versions"].as_array().expect("versions 必须是数组");
    assert_eq!(versions, entries, "versions 必须与 entries 深度相等");
    let entry = &entries[0];
    assert!(entry["version"]
        .as_u64()
        .is_some_and(|version| version >= 1));
    assert_eq!(
        entry["graph_id"].as_str().map(str::len),
        Some(64),
        "graph_id 应为 64 位 hex: {entry}"
    );
    assert_eq!(entry["sha256"], json!(plan.sha256()));
}

#[tokio::test(flavor = "current_thread")]
async fn plan_inspect_reports_ok_decoded_admission_graph_budget() {
    let server = support::spawn().await;
    let plan = TempPlan::new("contract-inspect", &identity_plan().encode());
    let payload = json!({ "plan_path": plan.path(), "plan_sha256": plan.sha256() });
    let response = support::request_json(&server, "POST", INSPECT, &payload.to_string()).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();

    // 合同 §3：{ok, decoded, admission, graph_id, budget}（只读试运行，不改运行态）。
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(
        body["graph_id"].as_str().map(str::len),
        Some(64),
        "graph_id 应为 64 位 hex: {body}"
    );
    let admission = body["admission"].as_object().expect("admission 必须是对象");
    assert_eq!(admission.get("accepted"), Some(&Value::Bool(true)));
    let decoded = body["decoded"].as_object().expect("decoded 必须是对象");
    assert_eq!(decoded.get("sha256"), Some(&json!(plan.sha256())));
    let decoded_plan = decoded.get("plan").expect("decoded.plan 必须存在");
    assert_eq!(decoded_plan["sample_rate"], json!(96_000u64));
    assert_eq!(decoded_plan["block_size"], json!(128u64));
    assert!(
        decoded.get("staged").is_some_and(Value::is_boolean),
        "decoded.staged 必须是 bool: {body}"
    );
    let budget = body["budget"].as_object().expect("budget 必须是对象");
    assert_eq!(
        admission.get("budget").and_then(Value::as_object),
        Some(budget),
        "budget 必须与 admission.budget 同一份数据"
    );
    assert!(
        budget
            .get("lanes")
            .and_then(Value::as_u64)
            .is_some_and(|lanes| lanes >= 1),
        "budget.lanes 必须是真实数值: {body}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn motu_view_has_schema_and_honest_readonly_fields() {
    let server = support::spawn().await;
    let response = support::request(&server, "GET", MOTU).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    // 合同 §3：schema 固定；ok 可缺省（只读快照）。
    assert_eq!(body["schema"], json!("khaen-motu/v1"));
    assert!(
        body["detected"].is_boolean(),
        "detected 必须是 bool: {body}"
    );
    assert!(body["not_wired"].is_array(), "not_wired 必须是数组: {body}");
    assert!(body["reasons"].is_object(), "reasons 必须是对象: {body}");
    if body["detected"] == Value::Bool(false) {
        assert!(
            body["reason"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty()),
            "未检测到设备必须给非空 reason: {body}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn calibration_wizard_and_state_shapes() {
    let server = support::spawn().await;

    let response = support::request(&server, "GET", WIZARD).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], Value::Bool(true));
    assert!(body["schema"].is_string(), "wizard 必须带 schema: {body}");
    assert!(body["config"].is_object(), "wizard 必须带 config: {body}");

    let response = support::request(&server, "GET", WIZARD_STATE).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], Value::Bool(true));
    assert!(
        body["current_step"].as_u64().is_some_and(|step| step >= 1),
        "current_step 必须是 >=1 的整数: {body}"
    );
    let steps = body["steps"].as_array().expect("steps 必须是数组");
    assert!(!steps.is_empty(), "steps 不能为空: {body}");
    for step in steps {
        assert!(step["step"].is_number(), "step.step 必须是数字: {step}");
        assert!(
            step["status"].is_string(),
            "step.status 必须是字符串: {step}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn system_info_services_shape() {
    let server = support::spawn().await;
    let response = support::request(&server, "GET", "/api/system/info").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    // 合同 §3 要求 ok:true（实现已补）。
    assert_eq!(body["ok"], Value::Bool(true));
    assert!(body["schema"].is_string(), "info 必须带 schema: {body}");
    assert!(
        body["crate_version"]
            .as_str()
            .is_some_and(|version| !version.is_empty()),
        "crate_version 不能为空: {body}"
    );
    for service in ["daemon", "rt_host"] {
        let object = body["services"][service]
            .as_object()
            .unwrap_or_else(|| panic!("services.{service} 必须是对象: {body}"));
        assert!(
            object.get("active").is_some(),
            "services.{service}.active 必须存在: {body}"
        );
        assert!(
            object.get("enabled").is_some(),
            "services.{service}.enabled 必须存在: {body}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn system_logs_reject_invalid_lines_with_400() {
    let server = support::spawn().await;
    for path in [
        "/api/system/logs?unit=daemon&lines=0",
        "/api/system/logs?unit=daemon&lines=abc",
        "/api/system/logs?unit=bogus",
    ] {
        let response = support::request(&server, "GET", path).await;
        assert_m1_error(&response, 400, "invalid_request");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn system_diagnostics_is_attachment_download() {
    let server = support::spawn().await;
    let response = support::request(&server, "GET", "/api/system/diagnostics").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    // 合同 §3：附件下载，Content-Disposition 带 attachment + filename。
    let disposition = response
        .header("content-disposition")
        .expect("diagnostics 必须带 Content-Disposition");
    assert!(
        disposition.contains("attachment"),
        "必须是附件下载: {disposition}"
    );
    assert!(
        disposition.contains("filename="),
        "附件必须带文件名: {disposition}"
    );
    assert_eq!(response.header("content-type"), Some("application/json"));
    let body = response.json();
    assert_eq!(body["schema"], json!("khaen-diagnostics/v1"));
}

// ---------------------------------------------------------------------------
// §4 控制面两阶段
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn mute_uses_mute_field_and_accepts_muted_alias() {
    let server = support::spawn().await;

    // canonical 字段 mute：回执同时回传 mute 与 muted（合同 §4）。
    let response = support::request_json(&server, "POST", MUTE, r#"{"mute":true}"#).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(body["mute"], Value::Bool(true));
    assert_eq!(body["muted"], Value::Bool(true));

    // 旧名 muted 作为别名接受。
    let response = support::request_json(&server, "POST", MUTE, r#"{"muted":false}"#).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(body["mute"], Value::Bool(false));

    // 两个字段冲突：400（旧控制面错误体，稳定码 bad_request；合同 §1 的 400 码）。
    let response =
        support::request_json(&server, "POST", MUTE, r#"{"mute":true,"muted":false}"#).await;
    assert_control_error(&response, 400, &["bad_request"]);
}

#[tokio::test(flavor = "current_thread")]
async fn prepare_commit_two_phase_consumes_token_once() {
    let server = support::spawn().await;

    // prepare 只生成草稿并返回一次性 token，不改变运行态。
    let (plan, prepared) = prepare_identity_plan(&server, "contract-commit").await;
    let token = prepared["engine_token"]
        .as_str()
        .expect("prepare.engine_token")
        .to_string();
    assert_eq!(prepared["ok"], Value::Bool(true));
    assert_eq!(
        prepared["graph_id"].as_str().map(str::len),
        Some(64),
        "prepare.graph_id 应为 64 位 hex: {prepared}"
    );

    // commit 在块边界生效并返回真实回执。
    let response = support::request_json(
        &server,
        "POST",
        COMMIT,
        &json!({ "engine_token": token }).to_string(),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], Value::Bool(true));
    assert!(
        body["version"].as_u64().is_some_and(|version| version >= 1),
        "commit.version 必须 >=1: {body}"
    );
    assert_eq!(
        body["graph_id"].as_str().map(str::len),
        Some(64),
        "commit.graph_id 应为 64 位 hex: {body}"
    );
    assert!(
        body["applied_block"].as_u64().is_some(),
        "commit.applied_block 必须是整数: {body}"
    );

    // token 只能消费一次。
    let response = support::request_json(
        &server,
        "POST",
        COMMIT,
        &json!({ "engine_token": token }).to_string(),
    )
    .await;
    assert_control_error(&response, 409, &["engine_token_invalid"]);

    // 未知 token 的 abort 同样 409。
    let response =
        support::request_json(&server, "POST", ABORT, r#"{"engine_token":"not-a-token"}"#).await;
    assert_control_error(&response, 409, &["engine_token_invalid"]);
    drop(plan);
}

#[tokio::test(flavor = "current_thread")]
async fn prepare_then_abort_cannot_be_committed() {
    let server = support::spawn().await;
    let (_plan, prepared) = prepare_identity_plan(&server, "contract-abort").await;
    let token = prepared["engine_token"]
        .as_str()
        .expect("prepare.engine_token")
        .to_string();

    let response = support::request_json(
        &server,
        "POST",
        ABORT,
        &json!({ "engine_token": token }).to_string(),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(body["aborted"], Value::Bool(true));

    // abort 后 token 已丢弃，commit 必须 409（不静默生效旧草稿）。
    let response = support::request_json(
        &server,
        "POST",
        COMMIT,
        &json!({ "engine_token": token }).to_string(),
    )
    .await;
    assert_control_error(&response, 409, &["engine_token_invalid"]);
}

#[tokio::test(flavor = "current_thread")]
async fn rollback_prepares_old_version_and_commit_applies_it() {
    let server = support::spawn().await;

    // 没有任何已提交历史：rollback 必须 409 no_history（不凭空造版本）。
    let response = support::request_json(&server, "POST", ROLLBACK, "{}").await;
    assert_control_error(&response, 409, &["no_history"]);

    // 提交两个不同的方案版本（identity v1、gain -3 dB v2）。
    let (base, prepared) = prepare_identity_plan(&server, "contract-rollback-base").await;
    let base_commit = support::request_json(
        &server,
        "POST",
        COMMIT,
        &json!({ "engine_token": prepared["engine_token"].as_str().expect("token") }).to_string(),
    )
    .await;
    assert_eq!(base_commit.status, 200, "{}", base_commit.body_text());
    assert_eq!(base_commit.json()["version"], json!(1u64));
    // 让 v1 在块边界生效（版本号推进到 1），下一次 prepare 才会是 v2。
    run_block(&server.state, &test_input());

    let gain = TempPlan::new("contract-rollback-gain", &gain_plan(-3.0).encode());
    let payload = json!({ "plan_path": gain.path(), "plan_sha256": gain.sha256() });
    let prepared = support::request_json(&server, "POST", PREPARE, &payload.to_string()).await;
    assert_eq!(prepared.status, 200, "{}", prepared.body_text());
    let gain_token = prepared.json()["engine_token"]
        .as_str()
        .expect("gain token")
        .to_string();
    let gain_commit = support::request_json(
        &server,
        "POST",
        COMMIT,
        &json!({ "engine_token": gain_token }).to_string(),
    )
    .await;
    assert_eq!(gain_commit.status, 200, "{}", gain_commit.body_text());
    assert_eq!(gain_commit.json()["version"], json!(2u64));
    run_block(&server.state, &test_input());

    // 回滚第一阶段：把历史 v1 重新准备成草稿（版本号递增，历史不改写），commit 后才生效。
    let response = support::request_json(&server, "POST", ROLLBACK, r#"{"target":1}"#).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(body["rolled_back_to"], json!(1u64));
    assert!(
        body["planned_version"]
            .as_u64()
            .is_some_and(|version| version >= 3),
        "回滚是前进式新提交，版本号必须递增: {body}"
    );
    let token = body["engine_token"]
        .as_str()
        .unwrap_or_else(|| panic!("rollback 必须返回 engine_token: {body}"))
        .to_string();

    // 真正生效仍走 /api/commit 的真实回执。
    let response = support::request_json(
        &server,
        "POST",
        COMMIT,
        &json!({ "engine_token": token }).to_string(),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], Value::Bool(true));
    assert!(
        body["version"].as_u64().is_some_and(|version| version >= 3),
        "回滚 commit 的版本号必须递增: {body}"
    );
    assert_eq!(
        body["graph_id"].as_str().map(str::len),
        Some(64),
        "commit.graph_id 应为 64 位 hex: {body}"
    );
    drop((base, gain));
}

#[tokio::test(flavor = "current_thread")]
async fn surface_volume_returns_engine_token_and_aborts() {
    let server = support::spawn().await;
    commit_identity_plan(&server).await;

    let payload = json!({
        "surface": "volume",
        "data": { "gains_db": [ { "lane": 0, "gain_db": -3.0 } ] }
    });
    let response = support::request_json(&server, "POST", SURFACE, &payload.to_string()).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], Value::Bool(true));
    let token = body["engine_token"]
        .as_str()
        .unwrap_or_else(|| panic!("surface 必须返回 engine_token: {body}"))
        .to_string();

    let response = support::request_json(
        &server,
        "POST",
        ABORT,
        &json!({ "engine_token": token }).to_string(),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(body["aborted"], Value::Bool(true));
}

#[tokio::test(flavor = "current_thread")]
async fn format_prepare_reports_ok_and_typed_format() {
    let server = support::spawn().await;
    let response = support::request_json(&server, "POST", FORMAT, r#"{"sample_rate":48000}"#).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], Value::Bool(true));
    assert!(body["changed"].is_boolean(), "changed 必须是 bool: {body}");
    // 目标格式是数值（不是字符串），并如实回显 48000。
    assert_eq!(body["format"]["sample_rate"], json!(48_000u64));
    assert!(
        body["engine_token"].is_string(),
        "format 两阶段必须返回 engine_token: {body}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn device_retry_returns_200() {
    let server = support::spawn().await;
    let response = support::request(&server, "POST", "/api/device/retry").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_ok_if_present(&body);
}

// ---------------------------------------------------------------------------
// §5 /api/channels（数据面热重配，两阶段）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn channels_get_shape_and_honest_hot_reconfigure() {
    let server = support::spawn().await;
    let response = support::request(&server, "GET", CHANNELS).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();

    // 合同 §5：恒 200 + ok + schema + 固定字段骨架；缺失数据给 null + reasons。
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(body["schema"], json!("khaen-channels/v1"));
    for key in [
        "current",
        "device",
        "format",
        "generation",
        "input",
        "output",
        "mapping",
        "plan",
        "hot_reconfigure",
        "not_wired",
        "reasons",
    ] {
        assert!(body.get(key).is_some(), "缺少合同字段 {key}: {body}");
    }
    // hot_reconfigure 必须如实说明是否接线：{wired, reason}。
    let hot = body["hot_reconfigure"]
        .as_object()
        .expect("hot_reconfigure 必须是对象");
    assert_eq!(
        hot.get("wired").and_then(Value::as_bool),
        Some(false),
        "process 目标没有数据面热重配: {body}"
    );
    assert!(
        hot.get("reason")
            .and_then(Value::as_str)
            .is_some_and(|reason| !reason.is_empty()),
        "hot_reconfigure.reason 说明原因: {body}"
    );
    // not_wired 条目形状与 /api/status 一致：{path, reason}。
    let not_wired = body["not_wired"].as_array().expect("not_wired 数组");
    for entry in not_wired {
        assert!(entry["path"].is_string(), "not_wired.path: {entry}");
        assert!(
            entry["reason"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty()),
            "not_wired.reason 非空: {entry}"
        );
    }
    assert!(body["reasons"].is_object());
}

#[tokio::test(flavor = "current_thread")]
async fn channels_process_target_is_501_host_link_unsupported() {
    let server = support::spawn().await;
    // process 目标没有数据面热重配：prepare 必须 501 + host_link_unsupported + 非空 reason，
    // 不能假成功、也不能发一个不可 commit 的 token。
    let response = support::request_json(
        &server,
        "PUT",
        CHANNELS,
        r#"{"stage":"prepare","channels":22}"#,
    )
    .await;
    assert_m1_error(&response, 501, "host_link_unsupported");
    let body = response.json();
    assert!(
        body["error"]["details"]["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "501 必须带非空 details.reason: {body}"
    );

    // 非法 stage 是参数错误 400 invalid_request（M1）。
    let response = support::request_json(&server, "PUT", CHANNELS, r#"{"stage":"bogus"}"#).await;
    assert_m1_error(&response, 400, "invalid_request");
}

// ---------------------------------------------------------------------------
// §8 系统 restart / password
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn system_restart_is_honest_about_capability() {
    let server = support::spawn().await;

    // daemon 自重启：501 unsupported（不假装已重启）。
    let response = support::request_json(&server, "POST", RESTART, r#"{"target":"daemon"}"#).await;
    assert_m1_error(&response, 501, "unsupported");

    // 非法 target：501 unsupported（合同 §8 裁决：未实现的 target 不是 400）。
    let response = support::request_json(&server, "POST", RESTART, r#"{"target":"weird"}"#).await;
    assert_m1_error(&response, 501, "unsupported");

    // 缺省 target 视为 rt-host：未启用 KHAEN_ALLOW_RESTART → 403 restart_disabled。
    for body in [r#"{}"#, r#"{"confirm":"RESTART"}"#] {
        let response = support::request_json(&server, "POST", RESTART, body).await;
        assert_m1_error(&response, 403, "restart_disabled");
        let body = response.json();
        assert!(
            body["error"]["details"]["reason"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty()),
            "403 必须带非空 details.reason: {body}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn system_password_rejects_wrong_current_password() {
    let server = support::spawn().await;
    // 合同 §8：{current,new}；已登录但旧密码错 → 401 invalid_credentials（M1）。
    let payload = json!({ "current": "wrong-password", "new": "new-password-123" });
    let response = support::request_json(&server, "POST", PASSWORD, &payload.to_string()).await;
    assert_m1_error(&response, 401, "invalid_credentials");

    // 新密码过短 → 400 invalid_password（稳定码，与旧密码错区分）。
    let payload = json!({ "current": support::TEST_PASSWORD, "new": "short" });
    let response = support::request_json(&server, "POST", PASSWORD, &payload.to_string()).await;
    assert_m1_error(&response, 400, "invalid_password");
}
