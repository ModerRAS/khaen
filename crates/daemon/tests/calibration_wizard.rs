//! T36 校准向导端到端测试（配置持久化 / 严格校验 / 8 步状态推导 / apply）。
//!
//! 覆盖：GET 默认值（真实常量 + reasons）、PUT 部分更新与未知字段 400、mic cal 必须已上传、
//! measurement 草稿持久化、重启后仍在（wizard.json 原子写）、鉴权/CSRF、
//! wizard/state（未接线灰态 / 已配置步骤 / fake job 推导）、wizard/apply 走控制面。

mod support;

use std::path::Path;

use serde_json::{json, Value};

use support::calibration_fake::FakeCalibration;
use support::plan_fixture::identity_plan;
use support::plan_fixture::TempPlan;

const WIZARD: &str = "/api/calibration/wizard";
const WIZARD_STATE: &str = "/api/calibration/wizard/state";
const WIZARD_APPLY: &str = "/api/calibration/wizard/apply";
const UPLOADS: &str = "/api/calibration/uploads";

fn assert_error_code(body: &Value, expected_code: &str) {
    assert_eq!(
        body["error"]["code"],
        json!(expected_code),
        "unexpected error body: {body}"
    );
    assert!(body["error"]["message"]
        .as_str()
        .is_some_and(|text| !text.is_empty()));
    assert!(body["error"]["details"].is_object());
}

async fn upload(server: &support::TestServer, bytes: &[u8]) -> String {
    let response =
        support::request_bytes(server, "POST", UPLOADS, "application/octet-stream", bytes).await;
    assert_eq!(response.status, 201, "{}", response.body_text());
    response.json()["upload_id"]
        .as_str()
        .expect("upload id")
        .to_string()
}

async fn get_wizard(server: &support::TestServer) -> Value {
    let response = support::request(server, "GET", WIZARD).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    response.json()
}

async fn put_wizard(server: &support::TestServer, body: &Value) -> support::HttpResponse {
    support::request_json(server, "PUT", WIZARD, &body.to_string()).await
}

fn wizard_file(state_dir: &Path) -> std::path::PathBuf {
    state_dir.join("wizard.json")
}

#[tokio::test(flavor = "current_thread")]
async fn get_returns_real_defaults_with_reasons() {
    let server = support::spawn().await;
    let body = get_wizard(&server).await;
    assert_eq!(body["schema"], json!("khaen-calibration-wizard/v1"));
    assert_eq!(body["saved"], json!(false));
    assert!(body["updated_at"].is_null());

    // 默认值来自真实常量（offline 闭环 CLI 默认 + designer BuiltinTarget）。
    assert_eq!(body["config"]["target_mode"], json!("flat"));
    assert!(body["config"]["mic_cal_sha256"].is_null());
    assert!(body["config"]["orientation"].is_null());
    let centers = body["config"]["single_point"]["centers_hz"]
        .as_array()
        .expect("centers");
    assert_eq!(centers.len(), 30);
    assert_eq!(centers[0], json!(25.0));
    assert_eq!(centers[29], json!(20000.0));
    assert_eq!(body["config"]["single_point"]["tolerance_db"], json!(2.0));
    assert_eq!(body["config"]["single_point"]["min_bands"], json!(3));
    assert_eq!(
        body["defaults"], body["config"],
        "未保存时 config 就是 defaults"
    );

    // 缺失字段都要给原因（不冒充已配置）。
    for path in [
        "mic_cal_sha256",
        "orientation",
        "target_mode",
        "single_point",
    ] {
        assert!(
            body["reasons"][path]
                .as_str()
                .is_some_and(|text| !text.is_empty()),
            "缺少 reason: {path} ({body})"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn put_persists_partial_config_atomically() {
    let server = support::spawn().await;
    let cal = upload(&server, b"umik-calibration-file").await;
    let baseline = upload(&server, b"baseline-wav").await;
    let applied = upload(&server, b"applied-wav").await;
    let body = json!({
        "mic_cal_sha256": cal,
        "mic_cal_filename": "umik-1-cal.txt",
        "orientation": "90deg",
        "target_mode": "tilt10",
        "single_point": {
            "centers_hz": [100.0, 1000.0, 10000.0],
            "tolerance_db": 1.5,
            "min_bands": 2,
        },
        "measurement": {
            "baseline_upload_id": baseline,
            "applied_upload_id": applied,
        },
    });
    let response = put_wizard(&server, &body).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["saved"], json!(true));
    assert!(body["updated_at"]
        .as_str()
        .is_some_and(|text| text.ends_with('Z')));
    assert_eq!(body["config"]["mic_cal_sha256"], json!(cal));
    assert_eq!(body["config"]["mic_cal_filename"], json!("umik-1-cal.txt"));
    assert_eq!(body["config"]["orientation"], json!("90deg"));
    assert_eq!(body["config"]["target_mode"], json!("tilt10"));
    assert_eq!(
        body["config"]["single_point"]["centers_hz"],
        json!([100.0, 1000.0, 10000.0])
    );
    assert_eq!(body["config"]["single_point"]["tolerance_db"], json!(1.5));
    assert_eq!(body["config"]["single_point"]["min_bands"], json!(2));
    assert!(body["set_fields"]
        .as_array()
        .is_some_and(|fields| fields.contains(&json!("orientation"))));
    // 有真实值的字段不该再有缺失原因。
    assert!(body["reasons"].get("orientation").is_none());
    assert!(body["reasons"].get("mic_cal_sha256").is_none());

    // 磁盘上真的写了 wizard.json（原子写 → 目录里没有临时文件）。
    let path = wizard_file(&server.state_dir);
    assert!(path.is_file());
    let stored: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read wizard")).expect("json");
    assert_eq!(stored["schema"], json!("khaen-calibration-wizard-store/v1"));
    assert_eq!(stored["measurement"]["baseline_upload_id"], json!(baseline));
    let leftovers: Vec<String> = std::fs::read_dir(&server.state_dir)
        .expect("state dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".tmp-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    // 再 GET：同一份配置（saved=true）。
    let body = get_wizard(&server).await;
    assert_eq!(body["saved"], json!(true));
    assert_eq!(body["config"]["orientation"], json!("90deg"));

    // 部分更新只改给定字段。
    let response = put_wizard(&server, &json!({"orientation": "0deg"})).await;
    let body = response.json();
    assert_eq!(body["config"]["orientation"], json!("0deg"));
    assert_eq!(body["config"]["target_mode"], json!("tilt10"));
    assert_eq!(body["config"]["single_point"]["min_bands"], json!(2));
}

#[tokio::test(flavor = "current_thread")]
async fn put_rejects_unknown_and_invalid_fields() {
    let server = support::spawn().await;
    let cases: Vec<(Value, &str, &str)> = vec![
        (json!({"nope": 1}), "invalid_request", "nope"),
        (
            json!({"orientation": "free"}),
            "invalid_request",
            "orientation",
        ),
        (
            json!({"target_mode": "harman"}),
            "invalid_request",
            "target_mode",
        ),
        (
            json!({"single_point": {"bogus": 1}}),
            "invalid_request",
            "single_point.bogus",
        ),
        (
            json!({"single_point": {"centers_hz": [1000.0, 100.0]}}),
            "invalid_request",
            "single_point.centers_hz",
        ),
        (
            json!({"single_point": {"tolerance_db": 0.0}}),
            "invalid_request",
            "single_point.tolerance_db",
        ),
        (
            json!({"single_point": {"min_bands": 0}}),
            "invalid_request",
            "single_point.min_bands",
        ),
        (
            json!({"mic_cal_sha256": "not-a-sha"}),
            "invalid_request",
            "mic_cal_sha256",
        ),
        (
            json!({"mic_cal_filename": "dir/cal.txt"}),
            "invalid_request",
            "mic_cal_filename",
        ),
        (
            json!({"mic_cal_filename": "cal.txt"}),
            "invalid_request",
            "mic_cal_filename",
        ),
        (
            json!({"measurement": {"bogus": 1}}),
            "invalid_request",
            "measurement.bogus",
        ),
        (
            json!({"mic_cal_sha256": "a".repeat(64)}),
            "upload_not_found",
            "mic_cal_sha256",
        ),
        (
            json!({"measurement": {"baseline_upload_id": "b".repeat(64)}}),
            "upload_not_found",
            "measurement.baseline_upload_id",
        ),
        (json!([1, 2, 3]), "invalid_request", "<body>"),
    ];
    for (body, code, field) in cases {
        let response = put_wizard(&server, &body).await;
        assert_eq!(
            response.status,
            if code == "upload_not_found" { 404 } else { 400 },
            "{body}: {}",
            response.body_text()
        );
        let body = response.json();
        assert_error_code(&body, code);
        assert_eq!(body["error"]["details"]["field"], json!(field));
        // 非法 PUT 不落盘：仍然是未保存状态。
        assert_eq!(get_wizard(&server).await["saved"], json!(false), "{body}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn wizard_survives_a_restart() {
    let dir = support::TempStateDir::new("wizard-restart");
    let cal = {
        let mut first = support::spawn_at(dir.path()).await;
        first.sign_in().await;
        let cal = upload(&first, b"cal-file").await;
        let response = put_wizard(
            &first,
            &json!({
                "mic_cal_sha256": cal,
                "orientation": "90deg",
            }),
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.body_text());
        cal
    };
    // 重启：同一个状态目录，新的进程内 DaemonState（auth.json + wizard.json 都从磁盘恢复）。
    let mut second = support::spawn_at(dir.path()).await;
    second.sign_in().await;
    let body = get_wizard(&second).await;
    assert_eq!(body["saved"], json!(true));
    assert_eq!(body["config"]["mic_cal_sha256"], json!(cal));
    assert_eq!(body["config"]["orientation"], json!("90deg"));
}

#[tokio::test(flavor = "current_thread")]
async fn wizard_requires_session_and_csrf() {
    let server = support::spawn().await;
    let anonymous = support::request_with(&server, "GET", WIZARD, &[], None).await;
    assert_eq!(anonymous.status, 401, "{}", anonymous.body_text());
    let session = server.session().expect("session").clone();
    let cookie_only = vec![(
        "Cookie".to_string(),
        format!("khaen_session={}", session.token),
    )];
    let forbidden = support::request_with(
        &server,
        "PUT",
        WIZARD,
        &cookie_only,
        Some(r#"{"orientation":"90deg"}"#),
    )
    .await;
    assert_eq!(forbidden.status, 403, "{}", forbidden.body_text());
    assert_error_code(&forbidden.json(), "forbidden");
}

#[tokio::test(flavor = "current_thread")]
async fn state_derives_real_gates_and_marks_unwired_steps_not_available() {
    let server = support::spawn().await;
    let response = support::request(&server, "GET", WIZARD_STATE).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["schema"], json!("khaen-calibration-wizard-state/v1"));
    let steps = body["steps"].as_array().expect("steps");
    assert_eq!(steps.len(), 8, "8 步向导语义（PLAN §10）");
    let keys: Vec<&str> = steps
        .iter()
        .map(|step| step["key"].as_str().expect("key"))
        .collect();
    assert_eq!(
        keys,
        [
            "device_check",
            "layout_channels",
            "mic_settings",
            "positions",
            "measurement",
            "analysis",
            "design",
            "review_apply"
        ]
    );
    // 没有 rt-host 状态文件 / 设备未接线：第 1 步 blocked。
    assert_eq!(body["steps"][0]["status"], json!("blocked"));
    assert!(body["steps"][0]["reason"]
        .as_str()
        .is_some_and(|text| text.contains("device")));
    // 第 2 步：/api/channels 已实现；status.format.channels 可读 → passed（只读，无 task）。
    assert_eq!(body["steps"][1]["status"], json!("passed"));
    assert_eq!(body["steps"][1]["task"], Value::Null);
    assert!(body["steps"][1]["reason"]
        .as_str()
        .is_some_and(|text| text.contains("readable")));
    // 第 3 步：向导配置没保存 → blocked。
    assert_eq!(body["steps"][2]["status"], json!("blocked"));
    // 第 4 步：单点支持，passed（多点/ART 在 reason 里明说 NOT_AVAILABLE）。
    assert_eq!(body["steps"][3]["status"], json!("passed"));
    // 第 5-8 步：测试服务未注入生产 backend（降级）→ not_available + reason，无任务号，不阻塞向导。
    for index in 4..8 {
        assert_eq!(body["steps"][index]["status"], json!("not_available"));
        assert_eq!(body["steps"][index]["task"], Value::Null);
        assert!(body["steps"][index]["reason"]
            .as_str()
            .is_some_and(|text| !text.is_empty()));
    }
    assert_eq!(body["current_step"], json!(1), "第一个 blocked 的步骤");
    assert!(body["active_job_id"].is_null());
    assert!(body["last_job_id"].is_null());
    assert!(body["snapshot"].is_null());
    assert!(body["reasons"]["snapshot"].is_string());
    assert!(body["measurement"]["baseline_upload_id"].is_null());
    assert!(body["reasons"]["measurement"].is_string());
    assert!(body["updated_at"]
        .as_str()
        .is_some_and(|text| text.ends_with('Z')));

    // 保存向导配置后：第 3 步通过；measurement 草稿进入状态（刷新可恢复）。
    let cal = upload(&server, b"cal-file").await;
    let baseline = upload(&server, b"baseline").await;
    let applied = upload(&server, b"applied").await;
    let response = put_wizard(
        &server,
        &json!({
            "mic_cal_sha256": cal,
            "orientation": "90deg",
            "measurement": {
                "baseline_upload_id": baseline,
                "applied_upload_id": applied,
            },
        }),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = support::request(&server, "GET", WIZARD_STATE).await.json();
    assert_eq!(body["steps"][2]["status"], json!("passed"));
    assert_eq!(body["measurement"]["baseline_upload_id"], json!(baseline));
    assert_eq!(body["measurement"]["baseline_sha256"], json!(baseline));
    assert_eq!(body["measurement"]["applied_upload_id"], json!(applied));
    assert_eq!(body["measurement"]["applied_sha256"], json!(applied));
}

#[tokio::test(flavor = "current_thread")]
async fn state_derives_job_steps_from_a_wired_backend() {
    let fake = FakeCalibration::new();
    let server = support::spawn_with_calibration(fake.clone()).await;
    let job_id = fake.seed_succeeded("PASS", &"a".repeat(64), "graph");
    let body = support::request(&server, "GET", WIZARD_STATE).await.json();
    assert_eq!(body["last_job_id"], json!(job_id));
    // 5 测量：job 的 inputs 就是测量来源 → passed。
    assert_eq!(body["steps"][4]["status"], json!("passed"));
    // 6 分析：job succeeded + verdict → passed。
    assert_eq!(body["steps"][5]["status"], json!("passed"));
    assert!(body["steps"][5]["reason"]
        .as_str()
        .is_some_and(|text| text.contains("PASS")));
    // 7 设计：result.plan 存在 → passed。
    assert_eq!(body["steps"][6]["status"], json!("passed"));
    // 8 回顾/应用：succeeded 但未 apply → blocked，reason 指向 apply 接口。
    assert_eq!(body["steps"][7]["status"], json!("blocked"));
    assert!(body["steps"][7]["reason"]
        .as_str()
        .is_some_and(|text| text.contains("/apply")));
    // 测量草稿来自 job inputs（当前 job 的 upload id 是 fake 造的 64 位 id）。
    assert!(body["measurement"]["baseline_upload_id"].is_string());
}

#[tokio::test(flavor = "current_thread")]
async fn wizard_apply_applies_the_last_succeeded_job_through_the_control_plane() {
    // 未接线：没有 job 且后端未接线 → 503 not_wired（不假装可修）。
    let not_wired = support::spawn().await;
    let response = support::request_json(&not_wired, "POST", WIZARD_APPLY, "{}").await;
    assert_eq!(response.status, 503, "{}", response.body_text());
    assert_error_code(&response.json(), "not_wired");

    // 已接线但没有 job → 409 no_applicable_job。
    let fake = FakeCalibration::new();
    let server = support::spawn_with_calibration(fake.clone()).await;
    let response = support::request_json(&server, "POST", WIZARD_APPLY, "{}").await;
    assert_eq!(response.status, 409, "{}", response.body_text());
    assert_error_code(&response.json(), "no_applicable_job");

    // 有成功 job + 登记 plan → 200，走真实 prepare/commit。
    let plan = TempPlan::new("wizard-apply", &identity_plan().encode());
    let job_id = fake.seed_succeeded("PASS", plan.sha256(), "graph");
    fake.set_apply_plan(&job_id, plan.path(), plan.sha256());
    let response = support::request_json(&server, "POST", WIZARD_APPLY, "{}").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["applied"], json!(true));
    assert_eq!(body["job_id"], json!(job_id));
    assert_eq!(body["applied_version"], json!(1));
    assert_eq!(body["sha256"], json!(plan.sha256()));
    assert!(body["applied_block"].is_number());
    assert!(fake
        .calls()
        .iter()
        .any(|call| call == &format!("apply_committed:{job_id}")));

    // 显式 job_id 也接受；未知 job → 404 job_not_found。
    let response = support::request_json(
        &server,
        "POST",
        WIZARD_APPLY,
        &json!({"job_id": job_id}).to_string(),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let response = support::request_json(
        &server,
        "POST",
        WIZARD_APPLY,
        &json!({"job_id": "job-9999"}).to_string(),
    )
    .await;
    assert_eq!(response.status, 404, "{}", response.body_text());
    assert_error_code(&response.json(), "job_not_found");
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn wizard_file_is_0600_on_unix() {
    use std::os::unix::fs::PermissionsExt;
    let server = support::spawn().await;
    let response = put_wizard(&server, &json!({"orientation": "0deg"})).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let metadata = std::fs::metadata(wizard_file(&server.state_dir)).expect("metadata");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
}
