//! GET/PUT /api/audio-profile + GET /api/device/inputs + POST /api/audio-profile/plan 合同测试
//! （docs/API.md §12，v1.2）。
//!
//! 全离线：真实 TCP 打真实 axum 服务；rt-host 控制通道用脚本化假传输（tests/support/host_link_fake.rs），
//! MOTU 用假 CueMix（tests/support/motu_fake.rs），状态目录用临时目录。

mod support;

use khaen_daemon::host_link::TransportError;
use serde_json::{json, Value};

use support::host_link_fake::FakeTransport;
use support::motu_fake::FakeCueMix;

const PROFILE: &str = "/api/audio-profile";
const PLAN: &str = "/api/audio-profile/plan";
const DEVICE_INPUTS: &str = "/api/device/inputs";

fn default_profile_json() -> Value {
    khaen_daemon::audio_profile::AudioProfile::default().to_json()
}

/// 断言 M1 错误体 {error:{code,message,details}}。
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
        "M1 message 不能为空: {body}"
    );
}

async fn prepare(server: &support::TestServer, profile: &Value) -> support::HttpResponse {
    support::request_json(
        server,
        "PUT",
        PROFILE,
        &json!({ "stage": "prepare", "profile": profile }).to_string(),
    )
    .await
}

async fn commit(server: &support::TestServer, token: &str) -> support::HttpResponse {
    support::request_json(
        server,
        "PUT",
        PROFILE,
        &json!({ "stage": "commit", "token": token }).to_string(),
    )
    .await
}

// ---------------------------------------------------------------------------
// GET /api/audio-profile
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_profile_is_read_only_and_reports_the_default_2_0_layout() {
    let server = support::spawn().await;
    let response = support::request(&server, "GET", PROFILE).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["schema"], json!("khaen-audio-profile/v1"));
    // 首次运行默认 2.0（GET 不写盘）。
    assert_eq!(body["profile"]["device"]["channels"], json!(22));
    assert_eq!(
        body["profile"]["outputs"]
            .as_array()
            .expect("outputs")
            .len(),
        22
    );
    assert_eq!(body["profile"]["outputs"][0]["role"], json!("L"));
    assert_eq!(body["profile"]["outputs"][1]["role"], json!("R"));
    assert_eq!(body["profile"]["outputs"][2]["enabled"], json!(false));
    assert_eq!(
        body["profile"]["inputs"]["sources"][0]["kind"],
        json!("aes67")
    );
    assert_eq!(body["validation"]["ok"], json!(true));
    assert_eq!(body["validation"]["errors"], json!([]));
    assert_eq!(body["plan"]["lanes"], json!(2));
    assert_eq!(body["plan"]["enabled_outputs"], json!(2));
    assert_eq!(body["plan"]["dac_channels"], json!(22));
    assert_eq!(body["revision"], json!(0));
    assert_eq!(body["saved"], json!(false));
    // capability 段永远存在；process 目标下 AES67 拿不到 -> null + reasons。
    assert!(body["capability"]["aes67"].is_object());
    assert!(body["capability"]["device_inputs"].is_object());
    assert!(body["reasons"].is_object());
    assert!(
        !server.state_dir.join("audio-profile.json").exists(),
        "GET 不能写盘"
    );
}

#[tokio::test]
async fn get_profile_derives_aes67_capability_and_graph_id_from_the_rt_host() {
    let transport = FakeTransport::with_rt_status_and_channels(
        support::host_link_fake::rt_status_document(),
        support::host_link_fake::channels_status_document(),
    );
    let server = support::spawn_with_link(support::injected_link(transport), None).await;
    let response = support::request(&server, "GET", PROFILE).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["capability"]["aes67"]["channels"], json!(2));
    assert_eq!(body["capability"]["aes67"]["map"], json!([0, 1]));
    assert_eq!(
        body["capability"]["aes67"]["channel_order_source"],
        json!("none")
    );
    assert_eq!(body["capability"]["aes67"]["channel_order"], Value::Null);
    // socket status 报 graph_id。
    assert_eq!(body["plan"]["graph_id"], json!("cafebabe0011"));
    // 没有启用输入 -> 2 条直通 fold 的方案仍可摘要。
    assert_eq!(body["plan"]["lanes"], json!(2));
}

#[tokio::test]
async fn get_profile_degrades_to_null_reasons_when_the_saved_file_is_corrupt() {
    let server = support::spawn().await;
    std::fs::write(server.state_dir.join("audio-profile.json"), b"{ broken").expect("write");
    let response = support::request(&server, "GET", PROFILE).await;
    assert_eq!(
        response.status,
        200,
        "GET 永远 200: {}",
        response.body_text()
    );
    let body = response.json();
    assert_eq!(body["profile"], Value::Null);
    assert_eq!(body["validation"], Value::Null);
    assert_eq!(body["plan"]["lanes"], Value::Null);
    assert_eq!(body["plan"]["graph_id"], Value::Null);
    assert!(body["reasons"]["profile"]
        .as_str()
        .is_some_and(|r| !r.is_empty()));
}

// ---------------------------------------------------------------------------
// PUT /api/audio-profile：prepare / commit / abort
// ---------------------------------------------------------------------------

#[tokio::test]
async fn prepare_commit_abort_roundtrip_and_get_reflects_the_saved_profile() {
    let server = support::spawn().await;
    // 把 DAC2 打开为中置（C），AES67 ch2 标为 C。
    let mut profile = default_profile_json();
    profile["outputs"][2]["enabled"] = json!(true);
    profile["outputs"][2]["role"] = json!("C");
    profile["inputs"]["sources"][0]["channels"]
        .as_array_mut()
        .expect("channels")
        .push(json!({ "channel": 2, "role": "C" }));

    let prepared = prepare(&server, &profile).await;
    assert_eq!(prepared.status, 200, "{}", prepared.body_text());
    let prepared_body = prepared.json();
    assert_eq!(prepared_body["ok"], json!(true));
    assert_eq!(prepared_body["stage"], json!("prepare"));
    let token = prepared_body["token"].as_str().expect("token").to_string();
    assert_eq!(token.len(), 16);
    assert_eq!(prepared_body["expires_in_s"], json!(120));
    assert_eq!(prepared_body["validation"]["ok"], json!(true));
    // 相对默认 2.0 的 diff。
    assert_eq!(prepared_body["diff"]["outputs_changed"], json!(true));
    assert_eq!(prepared_body["diff"]["inputs_changed"], json!(true));
    assert_eq!(prepared_body["diff"]["downmix_changed"], json!(false));
    assert_eq!(prepared_body["diff"]["fanout_changed"], json!(false));
    // prepare 不碰引擎、不写盘。
    assert!(!server.state_dir.join("audio-profile.json").exists());

    let committed = commit(&server, &token).await;
    assert_eq!(committed.status, 200, "{}", committed.body_text());
    let committed_body = committed.json();
    assert_eq!(committed_body["ok"], json!(true));
    assert_eq!(committed_body["saved"], json!(true));
    assert_eq!(committed_body["applied"], json!(true));
    assert_eq!(committed_body["revision"], json!(1));
    assert!(server.state_dir.join("audio-profile.json").exists());

    // GET 反映已保存 profile。
    let after = support::request(&server, "GET", PROFILE).await.json();
    assert_eq!(after["profile"]["outputs"][2]["enabled"], json!(true));
    assert_eq!(after["profile"]["outputs"][2]["role"], json!("C"));
    assert_eq!(
        after["profile"]["inputs"]["sources"][0]["channels"]
            .as_array()
            .expect("channels")
            .len(),
        3
    );
    assert_eq!(after["saved"], json!(true));
    assert_eq!(after["revision"], json!(1));
    assert_eq!(after["plan"]["lanes"], json!(3));
    // 第二次保存 revision 递增。
    let prepared = prepare(&server, &default_profile_json()).await;
    let token = prepared.json()["token"]
        .as_str()
        .expect("token")
        .to_string();
    let committed = commit(&server, &token).await.json();
    assert_eq!(committed["revision"], json!(2));
    assert_eq!(
        support::request(&server, "GET", PROFILE).await.json()["revision"],
        json!(2)
    );
}

#[tokio::test]
async fn abort_discards_the_draft_and_the_token_cannot_be_committed() {
    let server = support::spawn().await;
    let prepared = prepare(&server, &default_profile_json()).await.json();
    let token = prepared["token"].as_str().expect("token").to_string();
    let aborted = support::request_json(
        &server,
        "PUT",
        PROFILE,
        &json!({ "stage": "abort", "token": token }).to_string(),
    )
    .await;
    assert_eq!(aborted.status, 200, "{}", aborted.body_text());
    assert_eq!(aborted.json()["aborted"], json!(true));
    let commit_after_abort = commit(&server, &token).await;
    assert_m1_error(&commit_after_abort, 409, "engine_token_invalid");
}

#[tokio::test]
async fn commit_rejects_unknown_and_consumed_tokens() {
    let server = support::spawn().await;
    assert_m1_error(
        &commit(&server, "0123456789abcdef").await,
        409,
        "engine_token_invalid",
    );
    let prepared = prepare(&server, &default_profile_json()).await.json();
    let token = prepared["token"].as_str().expect("token").to_string();
    assert_eq!(commit(&server, &token).await.status, 200);
    // 一次性 token 不能复用。
    assert_m1_error(&commit(&server, &token).await, 409, "engine_token_invalid");
    // abort 的 token 也必须匹配。
    let aborted = support::request_json(
        &server,
        "PUT",
        PROFILE,
        &json!({ "stage": "abort", "token": "ffffffffffffffff" }).to_string(),
    )
    .await;
    assert_m1_error(&aborted, 409, "engine_token_invalid");
}

#[tokio::test]
async fn prepare_rejects_the_four_structural_errors_with_m1_bad_request() {
    let server = support::spawn().await;

    // 1) 启用但角色 None。
    let mut profile = default_profile_json();
    profile["outputs"][2]["enabled"] = json!(true);
    profile["outputs"][2]["role"] = json!("None");
    let response = prepare(&server, &profile).await;
    assert_m1_error(&response, 400, "bad_request");
    let body = response.json();
    assert!(body["error"]["details"]["validation"]["errors"]
        .as_array()
        .expect("errors")
        .iter()
        .any(|issue| issue["code"] == json!("enabled_without_role")));

    // 2) dac 越界。
    let mut profile = default_profile_json();
    profile["outputs"][2]["dac"] = json!(99);
    profile["outputs"][2]["enabled"] = json!(true);
    profile["outputs"][2]["role"] = json!("C");
    let response = prepare(&server, &profile).await;
    assert_m1_error(&response, 400, "bad_request");
    assert!(response.json()["error"]["details"]["validation"]["errors"]
        .as_array()
        .expect("errors")
        .iter()
        .any(|issue| issue["code"] == json!("dac_out_of_range")));

    // 3) 全部禁用。
    let mut profile = default_profile_json();
    for row in profile["outputs"].as_array_mut().expect("outputs") {
        row["enabled"] = json!(false);
        row["role"] = json!("None");
    }
    let response = prepare(&server, &profile).await;
    assert_m1_error(&response, 400, "bad_request");
    assert!(response.json()["error"]["details"]["validation"]["errors"]
        .as_array()
        .expect("errors")
        .iter()
        .any(|issue| issue["code"] == json!("no_enabled_outputs")));

    // 4) Custom 无 custom_label。
    let mut profile = default_profile_json();
    profile["outputs"][2]["enabled"] = json!(true);
    profile["outputs"][2]["role"] = json!("Custom");
    let response = prepare(&server, &profile).await;
    assert_m1_error(&response, 400, "bad_request");
    assert!(response.json()["error"]["details"]["validation"]["errors"]
        .as_array()
        .expect("errors")
        .iter()
        .any(|issue| issue["code"] == json!("custom_without_label")));
}

#[tokio::test]
async fn prepare_reports_request_shape_errors() {
    let server = support::spawn().await;
    // 缺 profile 字段。
    let response = support::request_json(
        &server,
        "PUT",
        PROFILE,
        &json!({ "stage": "prepare" }).to_string(),
    )
    .await;
    assert_m1_error(&response, 400, "bad_request");
    assert_eq!(
        response.json()["error"]["details"]["field"],
        json!("profile")
    );
    // 非 JSON body。
    let response = support::request_json(&server, "PUT", PROFILE, "{ nope").await;
    assert_m1_error(&response, 400, "bad_request");
    // 未知 stage。
    let response = support::request_json(
        &server,
        "PUT",
        PROFILE,
        &json!({ "stage": "rollback" }).to_string(),
    )
    .await;
    assert_m1_error(&response, 400, "bad_request");
    assert_eq!(response.json()["error"]["details"]["field"], json!("stage"));
    // 非法角色。
    let mut profile = default_profile_json();
    profile["outputs"][0]["role"] = json!("Left");
    let response = prepare(&server, &profile).await;
    assert_m1_error(&response, 400, "bad_request");
}

#[tokio::test]
async fn prepare_reports_duplicate_roles_as_valid() {
    let server = support::spawn().await;
    let mut profile = default_profile_json();
    profile["outputs"][2]["enabled"] = json!(true);
    profile["outputs"][2]["role"] = json!("L");
    let prepared = prepare(&server, &profile).await;
    assert_eq!(prepared.status, 200, "{}", prepared.body_text());
    let body = prepared.json();
    assert_eq!(body["validation"]["ok"], json!(true));
    // 重复角色不是错误；另外 L 有两路但 R 只有一路也不是错误。
    assert_eq!(body["profile"]["outputs"][2]["role"], json!("L"));
}

#[tokio::test]
async fn get_profile_validation_warnings_are_informational_only() {
    let server = support::spawn().await;
    // 只启用 L/R（默认）但 AES67 标了 C/LFE：缺角色只 warning，可保存。
    let mut profile = default_profile_json();
    for (index, role) in [(2, "C"), (3, "LFE")] {
        profile["inputs"]["sources"][0]["channels"]
            .as_array_mut()
            .expect("channels")
            .push(json!({ "channel": index, "role": role }));
    }
    let prepared = prepare(&server, &profile).await;
    assert_eq!(prepared.status, 200, "{}", prepared.body_text());
    let body = prepared.json();
    let warnings = body["validation"]["warnings"].as_array().expect("warnings");
    assert!(warnings
        .iter()
        .any(|issue| issue["code"] == json!("downmix_center")));
    assert!(warnings
        .iter()
        .any(|issue| issue["code"] == json!("downmix_lfe")));
    // GET 也是同样的 warning（read-only 反映当前保存/默认 profile，不阻塞）。
    let get = support::request(&server, "GET", PROFILE).await.json();
    assert_eq!(get["validation"]["ok"], json!(true));
}

// ---------------------------------------------------------------------------
// POST /api/audio-profile/plan
// ---------------------------------------------------------------------------

#[tokio::test]
async fn plan_endpoint_generates_an_abi_v3_file_and_runs_inspect() {
    let transport = FakeTransport::new();
    let server = support::spawn_with_link(support::injected_link(transport), None).await;
    let response = support::request_json(&server, "POST", PLAN, "{}").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["lane_count"], json!(2));
    assert_eq!(body["filter_source"], json!("identity"));
    let plan_path = body["plan_path"].as_str().expect("plan_path").to_string();
    assert!(plan_path.ends_with(".plan"), "{plan_path}");
    assert!(
        std::path::Path::new(&plan_path).exists(),
        "方案文件必须落盘"
    );
    let sha = body["sha256"].as_str().expect("sha256").to_string();
    assert_eq!(sha.len(), 64);
    let bytes = std::fs::read(&plan_path).expect("read plan");
    assert_eq!(khaen_host::sha256_hex(&bytes), sha);
    assert_eq!(
        std::path::Path::new(&plan_path)
            .file_name()
            .and_then(|name| name.to_str()),
        Some(format!("{sha}.plan").as_str())
    );
    // folds 预览：2 条直通 fold，无下混。
    let folds = body["folds_preview"].as_array().expect("folds_preview");
    assert_eq!(folds.len(), 2);
    assert_eq!(folds[0]["source"], json!("aes67"));
    assert_eq!(folds[0]["channel"], json!(0));
    assert_eq!(folds[0]["role"], json!("L"));
    assert_eq!(folds[0]["lane"], json!(0));
    assert_eq!(body["downmix_applied"], json!([]));
    // 假 rt-host 的 inspect 回执原样带出（不自动 prepare/commit）。
    assert_eq!(body["inspect"]["ok"], json!(true));
    assert_eq!(body["inspect"]["graph_id"], json!("graph-2"));

    // 生成的方案可以走既有 /api/prepare（plan_path + sha256 配对）。
    let prepared = support::request_json(
        &server,
        "POST",
        "/api/prepare",
        &json!({ "plan_path": plan_path, "plan_sha256": sha }).to_string(),
    )
    .await;
    assert_eq!(prepared.status, 200, "{}", prepared.body_text());
    // 方案生成不自动 prepare/commit：POST /api/audio-profile/plan 的回执里没有 token。
    assert!(
        body.get("token").is_none(),
        "plan 端点不能自动 prepare（不得签发引擎 token）: {body}"
    );
}

#[tokio::test]
async fn plan_endpoint_reports_downmix_preview_for_missing_roles() {
    let transport = FakeTransport::new();
    let server = support::spawn_with_link(support::injected_link(transport), None).await;
    let mut profile = default_profile_json();
    for (index, role) in [(2, "C"), (3, "LFE")] {
        profile["inputs"]["sources"][0]["channels"]
            .as_array_mut()
            .expect("channels")
            .push(json!({ "channel": index, "role": role }));
    }
    let prepared = prepare(&server, &profile).await.json();
    let token = prepared["token"].as_str().expect("token").to_string();
    assert_eq!(commit(&server, &token).await.status, 200);
    let body = support::request_json(&server, "POST", PLAN, "{}")
        .await
        .json();
    let downmix = body["downmix_applied"].as_array().expect("downmix_applied");
    assert_eq!(downmix.len(), 2);
    assert_eq!(downmix[0]["kind"], json!("center"));
    assert_eq!(downmix[0]["gain_db"], json!(-3.0));
    assert_eq!(downmix[0]["lanes"], json!([0, 1]));
    assert_eq!(downmix[1]["kind"], json!("lfe"));
    // 下混 fold 在预览里带标记。
    let folds = body["folds_preview"].as_array().expect("folds");
    assert_eq!(folds.len(), 6, "{folds:?}");
    assert!(folds.iter().any(|fold| fold["downmix"] == json!("center")));
    assert!(folds.iter().any(|fold| fold["downmix"] == json!("lfe")));
}

#[tokio::test]
async fn plan_endpoint_degrades_inspect_to_null_when_the_link_is_down() {
    let transport = FakeTransport::new();
    transport.push_transport_error(TransportError::Connect("down".to_string()));
    transport.push_transport_error(TransportError::Connect("down".to_string()));
    let server = support::spawn_with_link(support::injected_link(transport), None).await;
    // 第一次 status 探测（GET 不在本测试里）——直接 POST plan：inspect 失败 -> null + reason。
    let response = support::request_json(&server, "POST", PLAN, "{}").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["inspect"], Value::Null);
    assert!(body["reasons"]["inspect"]
        .as_str()
        .is_some_and(|reason| !reason.is_empty()));
}

// ---------------------------------------------------------------------------
// GET /api/device/inputs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn device_inputs_is_read_only_and_honest_when_no_probe_exists() {
    let server = support::spawn().await;
    let response = support::request(&server, "GET", DEVICE_INPUTS).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], json!(true));
    // daemon 没有采集通道枚举：available:false + reason，不编通道列表。
    assert_eq!(body["available"], json!(false));
    assert_eq!(body["optical_mode"], Value::Null);
    assert_eq!(body["channels"], Value::Null);
    assert_eq!(body["source"], json!("motu-alsa"));
    assert!(body["reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("capture")));
}

#[tokio::test]
async fn device_inputs_reports_the_motu_sample_rate_when_the_control_surface_is_ready() {
    let server = support::spawn_with_motu(FakeCueMix::ready()).await;
    let response = support::request(&server, "GET", DEVICE_INPUTS).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["available"], json!(false));
    assert_eq!(body["sample_rate"], json!(48_000));
    assert!(body["reason"]
        .as_str()
        .is_some_and(|reason| !reason.is_empty()));
}

// ---------------------------------------------------------------------------
// 通道角色加法（/api/channels speaker_roles / input_roles）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn channels_exposes_speaker_and_input_roles_without_breaking_the_shape() {
    let server = support::spawn().await;
    let body = support::request(&server, "GET", "/api/channels")
        .await
        .json();
    assert_eq!(body["ok"], json!(true));
    // 既有形状仍在。
    assert!(body.get("mapping").is_some());
    assert!(body.get("format").is_some());
    let speaker_roles = body["speaker_roles"].as_array().expect("speaker_roles");
    assert_eq!(speaker_roles.len(), 22);
    assert_eq!(speaker_roles[0]["dac"], json!(0));
    assert_eq!(speaker_roles[0]["role"], json!("L"));
    assert_eq!(speaker_roles[0]["enabled"], json!(true));
    assert_eq!(speaker_roles[2]["role"], json!("None"));
    let input_roles = body["input_roles"].as_array().expect("input_roles");
    assert_eq!(input_roles.len(), 2);
    assert_eq!(input_roles[0]["source"], json!("aes67"));
    assert_eq!(input_roles[0]["channel"], json!(0));
    assert_eq!(input_roles[0]["role"], json!("L"));
}
