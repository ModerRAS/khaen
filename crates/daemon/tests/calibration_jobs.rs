//! T36 校准 job REST 端到端测试：真实 TCP + 真实 axum 服务。
//!
//! 两种服务：
//! - 默认 NotWired：列表 200 available=false，job 相关接口 503 not_wired；
//! - 注入 tests/support/calibration_fake：创建/列表/详情/取消/apply/rollback 全链路，
//!   apply/rollback 走**真实控制面**（prepare→commit / rollback→commit）。
//!
//! 覆盖：校验错误码（kind/inputs/上传引用）、202 创建、单 active job 409、分页、状态机回显、
//! apply/rollback 与控制面版本、/api/status 的 calibration 字段（含 JobSink 事件兜底）。

mod support;

use std::sync::Arc;

use serde_json::{json, Value};

use khaen_daemon::DaemonState;
use khaen_host::plan::Plan;
use khaen_host::sha256_hex;
use support::calibration_fake::FakeCalibration;
use support::plan_fixture::{gain_plan, identity_plan, TempPlan};

const JOBS: &str = "/api/calibration/jobs";
const UPLOADS: &str = "/api/calibration/uploads";

fn assert_error_code(body: &Value, expected_code: &str) {
    assert_eq!(
        body["error"]["code"],
        json!(expected_code),
        "unexpected error body: {body}"
    );
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "error.message must be non-empty: {body}"
    );
    assert!(body["error"]["details"].is_object(), "{body}");
}

async fn upload_bytes(server: &support::TestServer, bytes: &[u8]) -> String {
    let response =
        support::request_bytes(server, "POST", UPLOADS, "application/octet-stream", bytes).await;
    assert_eq!(
        response.status,
        201,
        "upload failed: {}",
        response.body_text()
    );
    response.json()["upload_id"]
        .as_str()
        .expect("upload_id")
        .to_string()
}

fn job_payload(plan_sha: &str, baseline: &str, applied: &str) -> Value {
    json!({
        "kind": "single_point_offline",
        "inputs": {
            "plan_sha": plan_sha,
            "baseline_upload_id": baseline,
            "applied_upload_id": applied,
            "centers_hz": [100.0, 1000.0, 10000.0],
            "tolerance_db": 2.0,
            "min_bands": 3,
        }
    })
}

async fn valid_payload(server: &support::TestServer, plan: &TempPlan) -> (Value, String) {
    let baseline = upload_bytes(server, b"baseline-wav").await;
    let applied = upload_bytes(server, b"applied-wav").await;
    (
        job_payload(plan.sha256(), &baseline, &applied),
        baseline.clone(),
    )
}

async fn create_job(server: &support::TestServer, payload: &Value) -> support::HttpResponse {
    support::request_json(server, "POST", JOBS, &payload.to_string()).await
}

/// 跑一个块，让 commit 的草稿在块边界真正生效（版本号/图才会更新）。
fn run_block(state: &Arc<DaemonState>, input: &[f32]) -> Vec<f32> {
    state
        .control()
        .engine_mut()
        .run_block(input)
        .expect("run block")
        .to_vec()
}

/// identity 方案的恒定测试信号（块长 128 × 2 声道）。
fn test_input() -> Vec<f32> {
    (0..128 * 2)
        .map(|index| (index as f32 * 0.01) - 1.0)
        .collect()
}

fn plan_with_graph() -> (Plan, String) {
    let plan = identity_plan();
    let graph_id = plan.graph_id_hex();
    (plan, graph_id)
}

// ---------------------------------------------------------------------------
// 默认 NotWired 服务
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn not_wired_job_list_is_empty_with_available_false() {
    let server = support::spawn().await;
    let response = support::request(&server, "GET", JOBS).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["schema"], json!("khaen-calibration-jobs/v1"));
    assert_eq!(body["available"], json!(false));
    assert_eq!(body["jobs"], json!([]));
    assert_eq!(body["total"], json!(0));
    assert_eq!(body["limit"], json!(20));
    assert_eq!(body["offset"], json!(0));
    assert!(body["active_job_id"].is_null());
    assert!(body["engine"].is_null());
    assert!(
        body["reason"].as_str().is_some_and(|text| !text.is_empty()),
        "未接线必须给 reason: {body}"
    );
    assert!(
        body["reasons"]["jobs"].is_string(),
        "jobs 缺失要有原因: {body}"
    );
    assert!(body["reasons"]["engine"].is_string());
    assert!(body["reasons"]["active_job_id"].is_string());
}

#[tokio::test(flavor = "current_thread")]
async fn not_wired_job_endpoints_are_503_and_get_detail_too() {
    let server = support::spawn().await;
    let plan = TempPlan::new("not-wired", &identity_plan().encode());
    let (payload, _) = valid_payload(&server, &plan).await;

    // 校验通过后由服务返回 503（不是 200 假 job，也不是 400）。
    let response = create_job(&server, &payload).await;
    assert_eq!(response.status, 503, "{}", response.body_text());
    assert_error_code(&response.json(), "not_wired");

    for (method, path) in [
        ("GET", format!("{JOBS}/job-0001")),
        ("POST", format!("{JOBS}/job-0001/cancel")),
        ("POST", format!("{JOBS}/job-0001/apply")),
        ("POST", format!("{JOBS}/job-0001/rollback")),
        ("GET", format!("{JOBS}/job-0001/artifacts/report.json")),
    ] {
        let response = support::request_json(&server, method, &path, "{}").await;
        assert_eq!(
            response.status,
            503,
            "{method} {path}: {}",
            response.body_text()
        );
        assert_error_code(&response.json(), "not_wired");
    }
    // 向导应用没有 job 且后端未接线：同样 503（不假装 409 可修）。
    let response =
        support::request_json(&server, "POST", "/api/calibration/wizard/apply", "{}").await;
    assert_eq!(response.status, 503, "{}", response.body_text());
    assert_error_code(&response.json(), "not_wired");
}

#[tokio::test(flavor = "current_thread")]
async fn create_job_validates_kind_inputs_and_upload_references() {
    let server = support::spawn().await;
    let plan = TempPlan::new("validate", &identity_plan().encode());
    let (valid, baseline) = valid_payload(&server, &plan).await;
    let applied = valid["inputs"]["applied_upload_id"]
        .as_str()
        .expect("applied id")
        .to_string();

    let cases: Vec<(Value, u16, &str, &str)> = vec![
        (json!([]), 400, "invalid_request", "<body>"),
        (
            json!({"inputs": valid["inputs"].clone()}),
            400,
            "invalid_request",
            "kind",
        ),
        (
            json!({"kind": "full", "inputs": valid["inputs"].clone()}),
            400,
            "invalid_request",
            "kind",
        ),
        (
            json!({"kind": "single_point_offline"}),
            400,
            "invalid_request",
            "inputs",
        ),
        (
            json!({"kind": "single_point_offline", "inputs": valid["inputs"].clone(), "x": 1}),
            400,
            "invalid_request",
            "x",
        ),
        (
            json!({"kind": "single_point_offline", "inputs": {"plan_sha": plan.sha256(), "baseline_upload_id": baseline, "applied_upload_id": applied, "centers_hz": [], "tolerance_db": 2.0, "min_bands": 1}}),
            400,
            "invalid_request",
            "inputs.centers_hz",
        ),
        (
            json!({"kind": "single_point_offline", "inputs": {"plan_sha": plan.sha256(), "baseline_upload_id": baseline, "applied_upload_id": applied, "centers_hz": [1000.0, 100.0], "tolerance_db": 2.0, "min_bands": 1}}),
            400,
            "invalid_request",
            "inputs.centers_hz",
        ),
        (
            json!({"kind": "single_point_offline", "inputs": {"plan_sha": plan.sha256(), "baseline_upload_id": baseline, "applied_upload_id": applied, "centers_hz": [100.0], "tolerance_db": 0.0, "min_bands": 1}}),
            400,
            "invalid_request",
            "inputs.tolerance_db",
        ),
        (
            json!({"kind": "single_point_offline", "inputs": {"plan_sha": plan.sha256(), "baseline_upload_id": baseline, "applied_upload_id": applied, "centers_hz": [100.0], "tolerance_db": 2.0, "min_bands": 0}}),
            400,
            "invalid_request",
            "inputs.min_bands",
        ),
        (
            json!({"kind": "single_point_offline", "inputs": {"plan_sha": "zz", "baseline_upload_id": baseline, "applied_upload_id": applied, "centers_hz": [100.0], "tolerance_db": 2.0, "min_bands": 1}}),
            400,
            "invalid_request",
            "inputs.plan_sha",
        ),
        (
            json!({"kind": "single_point_offline", "inputs": {"plan_sha": plan.sha256(), "baseline_upload_id": "../../etc/passwd", "applied_upload_id": applied, "centers_hz": [100.0], "tolerance_db": 2.0, "min_bands": 1}}),
            400,
            "invalid_upload",
            "inputs.baseline_upload_id",
        ),
        (
            json!({"kind": "single_point_offline", "inputs": {"plan_sha": plan.sha256(), "baseline_upload_id": "a".repeat(64), "applied_upload_id": applied, "centers_hz": [100.0], "tolerance_db": 2.0, "min_bands": 1}}),
            404,
            "upload_not_found",
            "inputs.baseline_upload_id",
        ),
        (
            json!({"kind": "single_point_offline", "inputs": {"plan_sha": plan.sha256(), "baseline_upload_id": baseline, "applied_upload_id": applied, "centers_hz": [100.0], "tolerance_db": 2.0, "min_bands": 2}}),
            400,
            "invalid_request",
            "inputs.min_bands",
        ),
    ];
    for (payload, status, code, field) in cases {
        let response = create_job(&server, &payload).await;
        assert_eq!(
            response.status,
            status,
            "{payload}: {}",
            response.body_text()
        );
        let body = response.json();
        assert_error_code(&body, code);
        assert_eq!(
            body["error"]["details"]["field"],
            json!(field),
            "{payload}: {body}"
        );
    }
    // 校验通过的合法请求最终是 503（NotWired），说明校验顺序在服务调用之前。
    let response = create_job(&server, &valid).await;
    assert_eq!(response.status, 503, "{}", response.body_text());
}

#[tokio::test(flavor = "current_thread")]
async fn job_routes_require_session_and_csrf() {
    let server = support::spawn().await;
    let anonymous = support::request_with(&server, "GET", JOBS, &[], None).await;
    assert_eq!(anonymous.status, 401, "{}", anonymous.body_text());
    let session = server.session().expect("session").clone();
    let cookie_only = vec![(
        "Cookie".to_string(),
        format!("khaen_session={}", session.token),
    )];
    let forbidden = support::request_with(&server, "POST", JOBS, &cookie_only, Some("{}")).await;
    assert_eq!(forbidden.status, 403, "{}", forbidden.body_text());
    assert_error_code(&forbidden.json(), "forbidden");
}

#[tokio::test(flavor = "current_thread")]
async fn list_query_params_are_validated() {
    let server = support::spawn().await;
    for (query, field) in [
        ("?limit=0", "limit"),
        ("?limit=101", "limit"),
        ("?limit=x", "limit"),
        ("?offset=-1", "offset"),
        ("?page=1", "page"),
    ] {
        let path = format!("{JOBS}{query}");
        let response = support::request(&server, "GET", &path).await;
        assert_eq!(response.status, 400, "{path}: {}", response.body_text());
        let body = response.json();
        assert_error_code(&body, "invalid_request");
        assert_eq!(body["error"]["details"]["field"], json!(field), "{path}");
    }
}

// ---------------------------------------------------------------------------
// 注入 fake 服务（真实控制面 apply/rollback）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn create_list_and_detail_reflect_the_backend_state_machine() {
    let fake = FakeCalibration::new();
    let server = support::spawn_with_calibration(fake.clone()).await;
    let plan = TempPlan::new("create", &identity_plan().encode());
    let (payload, _) = valid_payload(&server, &plan).await;

    let response = create_job(&server, &payload).await;
    assert_eq!(response.status, 202, "{}", response.body_text());
    let body = response.json();
    let job_id = body["job_id"].as_str().expect("job_id").to_string();
    assert_eq!(
        body["location"],
        json!(format!("/api/calibration/jobs/{job_id}"))
    );

    let list = support::request(&server, "GET", JOBS).await;
    assert_eq!(list.status, 200);
    let list = list.json();
    assert_eq!(list["available"], json!(true));
    assert!(list["reason"].is_null());
    assert_eq!(list["active_job_id"], json!(job_id));
    assert_eq!(list["total"], json!(1));
    assert_eq!(list["jobs"][0]["id"], json!(job_id));
    assert_eq!(list["jobs"][0]["status"], json!("running"));
    assert!(list["jobs"][0]["verdict"].is_null());
    assert!(list["jobs"][0]["reason"].is_null());

    let detail = support::request(&server, "GET", &format!("{JOBS}/{job_id}")).await;
    assert_eq!(detail.status, 200, "{}", detail.body_text());
    let detail = detail.json();
    assert_eq!(detail["schema"], json!("khaen-calibration-job/v1"));
    let job = &detail["job"];
    assert_eq!(job["id"], json!(job_id));
    assert_eq!(job["kind"], json!("single_point_offline"));
    assert_eq!(job["status"], json!("running"));
    assert_eq!(job["step"], json!("validate"));
    assert_eq!(job["apply_state"], json!("not_applied"));
    let steps = job["steps"].as_array().expect("steps");
    assert_eq!(steps.len(), 5);
    assert_eq!(steps[0]["name"], json!("validate"));
    assert_eq!(steps[0]["status"], json!("running"));
    assert_eq!(steps[1]["status"], json!("pending"));
    assert_eq!(steps[0]["sha256"], Value::Null);
    assert_eq!(job["inputs"]["min_bands"], json!(3));
    assert!(detail["reasons"]["verdict"].is_string());
    assert!(detail["reasons"]["result"].is_string());

    // 单 active job：第二个创建 → 409 switch_busy（带 active_job_id）。
    let second = create_job(&server, &payload).await;
    assert_eq!(second.status, 409, "{}", second.body_text());
    let second = second.json();
    assert_error_code(&second, "switch_busy");
    assert_eq!(second["error"]["details"]["active_job_id"], json!(job_id));

    // 取消后列表可见 cancelled，且可以再开新 job。
    let cancel =
        support::request_json(&server, "POST", &format!("{JOBS}/{job_id}/cancel"), "{}").await;
    assert_eq!(cancel.status, 200, "{}", cancel.body_text());
    assert_eq!(cancel.json()["status"], json!("cancelled"));
    assert!(fake
        .calls()
        .iter()
        .any(|call| call == &format!("cancel:{job_id}")));
    let list = support::request(&server, "GET", JOBS).await.json();
    assert!(list["active_job_id"].is_null());
    assert_eq!(list["jobs"][0]["status"], json!("cancelled"));
    assert_eq!(
        list["jobs"][0]["finished_at"],
        json!("2026-11-26T00:00:00Z")
    );
    let again = create_job(&server, &payload).await;
    assert_eq!(again.status, 202, "{}", again.body_text());
}

#[tokio::test(flavor = "current_thread")]
async fn pagination_returns_newest_first_and_total() {
    let fake = FakeCalibration::new();
    let server = support::spawn_with_calibration(fake.clone()).await;
    let mut ids = Vec::new();
    for index in 0..3 {
        ids.push(fake.seed_succeeded("PASS", &format!("{:064x}", index + 1), "graph"));
    }
    let response = support::request(&server, "GET", &format!("{JOBS}?limit=2")).await;
    let body = response.json();
    assert_eq!(body["total"], json!(3));
    assert_eq!(body["limit"], json!(2));
    assert_eq!(body["jobs"].as_array().expect("jobs").len(), 2);
    assert_eq!(body["jobs"][0]["id"], json!(ids[2]));
    assert_eq!(body["jobs"][1]["id"], json!(ids[1]));
    let response = support::request(&server, "GET", &format!("{JOBS}?limit=2&offset=2")).await;
    let body = response.json();
    assert_eq!(body["jobs"].as_array().expect("jobs").len(), 1);
    assert_eq!(body["jobs"][0]["id"], json!(ids[0]));
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_job_is_404_but_invalid_id_is_400() {
    let fake = FakeCalibration::new();
    let server = support::spawn_with_calibration(fake.clone()).await;
    let response = support::request(&server, "GET", &format!("{JOBS}/job-9999")).await;
    assert_eq!(response.status, 404, "{}", response.body_text());
    assert_error_code(&response.json(), "job_not_found");
    let response = support::request(&server, "GET", &format!("{JOBS}/..")).await;
    assert_eq!(response.status, 400, "{}", response.body_text());
    assert_error_code(&response.json(), "invalid_request");
}

#[tokio::test(flavor = "current_thread")]
async fn apply_runs_the_real_control_plane_and_records_the_version() {
    let fake = FakeCalibration::new();
    let server = support::spawn_with_calibration(fake.clone()).await;
    let (plan_value, expected_graph) = plan_with_graph();
    let plan = TempPlan::new("apply", &plan_value.encode());
    let job_id = fake.seed_succeeded("PASS", plan.sha256(), "fake-graph");
    fake.set_apply_plan(&job_id, plan.path(), plan.sha256());

    let response =
        support::request_json(&server, "POST", &format!("{JOBS}/{job_id}/apply"), "{}").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["applied"], json!(true));
    assert_eq!(body["job_id"], json!(job_id));
    assert_eq!(body["applied_version"], json!(1));
    assert_eq!(body["graph_id"], json!(expected_graph));
    // applied_block = 下一块块号：commit 只在块边界生效。
    assert_eq!(body["applied_block"], json!(1));
    assert_eq!(body["sha256"], json!(plan.sha256()));
    assert!(
        body["snapshot"]["version"].is_null(),
        "apply 前没有运行版本"
    );
    assert!(body["snapshot"]["sha256"].is_null());
    assert!(body["reasons"]["snapshot.sha256"].is_string());
    assert_eq!(body["ack"], json!({"synthetic": false, "actual": true}));
    // commit 后、块边界前：readback 如实显示还没生效（actual_version=null + 原因）。
    assert!(body["readback"]["actual_version"].is_null());
    assert!(body["readback"]["reasons"]["actual_version"].is_string());
    assert!(
        body["applied_at"]
            .as_str()
            .is_some_and(|text| text.ends_with('Z')),
        "applied_at 必须是 RFC3339 UTC: {body}"
    );
    assert_eq!(body["job"]["job"]["apply_state"], json!("applied"));

    // 控制面真的生效了（真实 prepare→commit，块边界后 actual_version/graph_id 可回读）。
    let input = test_input();
    let output = run_block(&server.state, &input);
    assert_eq!(output, input, "identity 方案应逐样本直通");
    let status = server.state.control().status_json();
    assert_eq!(status["actual_version"], json!(1));
    assert_eq!(status["graph_id"], json!(expected_graph));
    assert!(fake
        .calls()
        .iter()
        .any(|call| call == &format!("apply_committed:{job_id}")));

    // 详情接口回写 apply 状态。
    let detail = support::request(&server, "GET", &format!("{JOBS}/{job_id}"))
        .await
        .json();
    assert_eq!(detail["job"]["apply_state"], json!("applied"));
    assert_eq!(detail["job"]["applied_version"], json!(1));
    assert_eq!(detail["job"]["apply"]["plan_sha256"], json!(plan.sha256()));
    assert_eq!(detail["job"]["result"]["verdict"], json!("PASS"));
}

#[tokio::test(flavor = "current_thread")]
async fn apply_requires_succeeded_job_and_registered_plan() {
    let fake = FakeCalibration::new();
    let server = support::spawn_with_calibration(fake.clone()).await;
    let plan = TempPlan::new("apply-not-succeeded", &identity_plan().encode());
    let (payload, _) = valid_payload(&server, &plan).await;
    let created = create_job(&server, &payload).await.json();
    let job_id = created["job_id"].as_str().expect("job id");
    let response =
        support::request_json(&server, "POST", &format!("{JOBS}/{job_id}/apply"), "{}").await;
    assert_eq!(response.status, 409, "{}", response.body_text());
    assert_error_code(&response.json(), "job_not_succeeded");

    // 成功 job 但没登记 plan → 409 plan_unavailable。
    let job_id = fake.seed_succeeded("PASS", plan.sha256(), "graph");
    let response =
        support::request_json(&server, "POST", &format!("{JOBS}/{job_id}/apply"), "{}").await;
    assert_eq!(response.status, 409, "{}", response.body_text());
    assert_error_code(&response.json(), "plan_unavailable");
}

#[tokio::test(flavor = "current_thread")]
async fn rollback_restores_the_previous_version_through_the_control_plane() {
    let fake = FakeCalibration::new();
    let server = support::spawn_with_calibration(fake.clone()).await;
    let input = test_input();
    let first = TempPlan::new("rollback-first", &identity_plan().encode());
    let gain = gain_plan(6.0);
    let gain_graph = gain.graph_id_hex();
    let second = TempPlan::new("rollback-second", &gain.encode());
    let job1 = fake.seed_succeeded("PASS", first.sha256(), "graph-1");
    fake.set_apply_plan(&job1, first.path(), first.sha256());
    let response =
        support::request_json(&server, "POST", &format!("{JOBS}/{job1}/apply"), "{}").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert_eq!(response.json()["applied_version"], json!(1));
    assert_eq!(run_block(&server.state, &input), input, "v1 = identity");

    let job2 = fake.seed_succeeded("PASS", second.sha256(), "graph-2");
    fake.set_apply_plan(&job2, second.path(), second.sha256());
    let response =
        support::request_json(&server, "POST", &format!("{JOBS}/{job2}/apply"), "{}").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let applied = response.json();
    assert_eq!(applied["applied_version"], json!(2));
    assert_eq!(applied["snapshot"]["version"], json!(1), "v1 已在跑");
    assert_eq!(applied["graph_id"], json!(gain_graph));
    // v2 = +6 dB：块边界后输出放大 1.995262 倍。
    let output = run_block(&server.state, &input);
    for (out, reference) in output.iter().zip(input.iter()) {
        assert!(
            (out - reference * 1.995_262).abs() < 1e-4,
            "{out} vs {reference}"
        );
    }

    let response =
        support::request_json(&server, "POST", &format!("{JOBS}/{job2}/rollback"), "{}").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["rolled_back"], json!(true));
    assert_eq!(body["restored_version"], json!(1));
    assert_eq!(body["applied_version"], json!(3));
    assert_eq!(body["sha256"], Value::Null);
    assert!(body["reasons"]["sha256"].is_string());
    // 回滚 commit 后、块边界前：readback 仍是回滚前正在跑的 v2。
    assert_eq!(body["readback"]["actual_version"], json!(2));
    assert!(body["rolled_back_at"]
        .as_str()
        .is_some_and(|text| text.ends_with('Z')));
    assert_eq!(body["job"]["job"]["apply_state"], json!("rolled_back"));
    assert!(fake
        .calls()
        .iter()
        .any(|call| call == &format!("rollback_committed:{job2}")));
    // 回滚在块边界生效：输出恢复 identity。
    assert_eq!(run_block(&server.state, &input), input);
    assert_eq!(
        server.state.control().status_json()["actual_version"],
        json!(3)
    );

    // 没应用过的 job 回滚 → 409 rollback_unavailable。
    let job3 = fake.seed_succeeded("PASS", first.sha256(), "graph-3");
    let response =
        support::request_json(&server, "POST", &format!("{JOBS}/{job3}/rollback"), "{}").await;
    assert_eq!(response.status, 409, "{}", response.body_text());
    assert_error_code(&response.json(), "rollback_unavailable");
}

#[tokio::test(flavor = "current_thread")]
async fn status_endpoint_uses_service_status_and_job_sink_events() {
    let fake = FakeCalibration::new();
    let server = support::spawn_with_calibration(fake.clone()).await;
    // 服务状态：已接线、没有 job。
    let body = support::request(&server, "GET", "/api/status").await.json();
    assert_eq!(body["calibration"]["active_job"], Value::Null);
    assert!(body["reasons"]["calibration.active_job"].is_string());
    let not_wired = body["not_wired"].as_array().expect("not_wired");
    assert!(not_wired
        .iter()
        .all(|entry| entry["path"] != json!("calibration.jobs")));

    // 事件汇（JobSink）：事件一到，/api/status 立即反映 active job。
    fake.publish(
        "job-0001",
        khaen_daemon::calibration::service::JobState::Running,
        None,
    );
    let body = support::request(&server, "GET", "/api/status").await.json();
    assert_eq!(body["calibration"]["active_job"], json!("job-0001"));
    assert!(body["reasons"].get("calibration.active_job").is_none());

    // 服务 status() 坏掉时：事件缓存兜底 + 降级原因，绝不 500。
    fake.fail_status("fake status failure");
    fake.publish(
        "job-0001",
        khaen_daemon::calibration::service::JobState::Succeeded,
        Some("PASS".into()),
    );
    let response = support::request(&server, "GET", "/api/status").await;
    assert_eq!(response.status, 200);
    let body = response.json();
    assert_eq!(body["calibration"]["active_job"], Value::Null);
    assert_eq!(body["calibration"]["last_job_id"], json!("job-0001"));
    assert_eq!(body["calibration"]["last_verdict"], json!("PASS"));
    assert!(body["reasons"]["calibration.active_job"].is_string());
    assert!(body["reasons"].get("calibration.last_job_id").is_none());
    let not_wired = body["not_wired"].as_array().expect("not_wired");
    assert!(not_wired
        .iter()
        .any(|entry| entry["path"] == json!("calibration.jobs")));
}

#[tokio::test(flavor = "current_thread")]
async fn upload_id_is_the_content_sha256() {
    // 同一份字节：REST 返回的 upload_id 就是 sha256（内容寻址），不另造 id。
    let server = support::spawn().await;
    let bytes = b"same-bytes-same-id";
    let id = upload_bytes(&server, bytes).await;
    assert_eq!(id, sha256_hex(bytes));
}
