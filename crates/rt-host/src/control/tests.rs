//! 控制通道行为测试：协议核心 + 假块线程/真 SlotEngine + 内存双工/回环/Unix socket。
//!
//! 覆盖点（对应 T70f-a 验收）：
//! - 协议错误码：坏 JSON / 未知 op / 字段缺失与类型 / 超长行 / 未接通界面；
//! - 能力上报：status.wired / not_wired / surfaces / reasons 与快照 age；
//! - mute 块边界生效与幂等；prepare -> commit 在块边界换图（真 SlotEngine 验证）；
//! - surface volume/routing/layout/mode 补丁的接收形状与真实生效（冲激响应）；
//! - rollback 前进式提交与历史有限；format 两阶段（control-plane only，如实 device_restart_required）；
//! - inspect 只读；coeff 真实版本；abort 不销毁错 token 草稿；
//! - 传输：内存双工（平台无关）、TCP 回环（多连接串行化）、Unix socket 端到端（cfg(unix)）；
//! - Runtime 适配层：ControlTarget for Runtime + 命令回执 + 格式不匹配拒绝 + 快照发布。

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::test_support::*;
use super::transport::serve_connection;
use super::{ControlBridge, ControlConfig, ControlCore, ControlCoreConfig};
use crate::control::command::{CommandReply, ControlCommand};
use crate::recovery::DeviceLifecycle;
use khaen_host::control::{error, RuntimeMode, WIRED_OPS};
use khaen_host::runtime::compile_plan_draft;

fn assert_ok(value: &Value) {
    assert_eq!(
        value["ok"],
        Value::Bool(true),
        "expected ok response, got: {value}"
    );
}

fn assert_error(value: &Value, code: &str) {
    assert_eq!(
        value["ok"],
        Value::Bool(false),
        "expected error {code}, got: {value}"
    );
    assert_eq!(
        value["error"],
        Value::String(code.to_string()),
        "unexpected error code in: {value}"
    );
    assert!(
        value["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "error response must carry a message: {value}"
    );
}

/// 校准未装配时（KHAEN_CAL=off / 测试夹具默认）：cal_* 必须如实进 not_wired，
/// status.calibration 如实报 enabled=false，且 wired 不含 cal_*。
fn assert_calibration_disabled(response: &Value) {
    let wired = response["wired"].as_array().expect("wired array");
    for op in crate::control::cal::CAL_OPS {
        assert!(
            !wired.iter().any(|value| value.as_str() == Some(op)),
            "cal op {op} must not be reported wired when calibration is off: {response}"
        );
    }
    let not_wired = response["not_wired"].as_array().expect("not_wired array");
    for op in crate::control::cal::CAL_OPS {
        assert!(
            not_wired
                .iter()
                .any(|entry| entry["op"].as_str() == Some(op)),
            "cal op {op} must be listed in not_wired when calibration is off: {response}"
        );
    }
    assert_eq!(response["calibration"]["enabled"], json!(false));
    assert_eq!(response["calibration"]["mode"], json!("playback"));
    assert_eq!(response["calibration"]["real_capture"], json!(false));
}

fn text(value: &Value) -> String {
    value["engine_token"]
        .as_str()
        .expect("engine_token string")
        .to_string()
}

fn commit_token(harness: &mut Harness, token: &str) -> Value {
    harness.call_json(&json!({ "op": "commit", "engine_token": token }))
}

fn prepare_plan(harness: &mut Harness, plan: &std::path::Path, sha: &str) -> Value {
    harness.call_json(&json!({
        "op": "prepare",
        "plan_path": plan.display().to_string(),
        "plan_sha256": sha,
    }))
}

fn prepare_commit_identity(harness: &mut Harness) -> Value {
    let plan = identity_plan();
    let bytes = encode_plan(&plan);
    let file = TempPlan::new("prepare-commit", &bytes);
    harness.publish();
    let prepared = prepare_plan(harness, file.path(), &file.sha256());
    assert_ok(&prepared);
    let committed = commit_token(harness, &text(&prepared));
    assert_ok(&committed);
    harness.run_blocks(1);
    committed
}

// ===========================================================================
// 协议层：错误码与能力上报
// ===========================================================================

#[test]
fn malformed_requests_get_stable_error_codes() {
    let mut harness = Harness::new();
    for (request, code) in [
        (r#"not json at all"#, error::BAD_REQUEST),
        ("", error::BAD_REQUEST),
        ("[]", error::BAD_REQUEST),
        ("{}", error::BAD_REQUEST),
        (r#"{"op":42}"#, error::BAD_REQUEST),
        (r#"{"op":"frobnicate"}"#, error::UNKNOWN_OP),
        (r#"{"op":"mute"}"#, error::BAD_REQUEST),
        (r#"{"op":"mute","mute":"yes"}"#, error::BAD_REQUEST),
        (r#"{"op":"prepare"}"#, error::BAD_REQUEST),
        (
            r#"{"op":"prepare","plan_path":"x.plan"}"#,
            error::BAD_REQUEST,
        ),
        (
            r#"{"op":"prepare","plan_path":42,"plan_sha256":"00"}"#,
            error::BAD_REQUEST,
        ),
        (r#"{"op":"commit"}"#, error::BAD_REQUEST),
        (r#"{"op":"commit","engine_token":42}"#, error::BAD_REQUEST),
        (r#"{"op":"abort","engine_token":null}"#, error::BAD_REQUEST),
        (r#"{"op":"surface"}"#, error::BAD_REQUEST),
        (
            r#"{"op":"surface","surface":"volume","data":[]}"#,
            error::NO_BASE_PLAN,
        ),
        (
            r#"{"op":"surface","surface":"frobnicate","data":{}}"#,
            error::BAD_REQUEST,
        ),
        (r#"{"op":"rollback"}"#, error::NO_HISTORY),
        (r#"{"op":"format"}"#, error::BAD_REQUEST),
    ] {
        let response = harness.call(request);
        assert_error(&response, code);
    }

    // 单行超过上限：控制核心自己也做长度检查（传输层还会在更前面丢弃并回错误）。
    let huge = format!(r#"{{"op":"status","pad":"{}"}}"#, "x".repeat(70_000));
    let response = harness.call(&huge);
    assert_error(&response, error::BAD_REQUEST);
    assert!(response["message"]
        .as_str()
        .expect("message")
        .contains("exceeds"));
}

#[test]
fn status_before_first_snapshot_is_honest() {
    let mut harness = Harness::new();
    let response = harness.call_json(&json!({"op": "status"}));
    assert_ok(&response);
    assert_eq!(response["runtime_state"], json!("starting"));
    assert_eq!(response["blocks"], Value::Null);
    assert_eq!(response["age_ms"], Value::Null);
    assert!(
        response["reasons"]["snapshot"].is_string(),
        "pending snapshot must have a reason: {response}"
    );
    // 能力列表不依赖快照：wired 完整、surfaces 全 true。
    assert_eq!(response["wired"], json!(WIRED_OPS));
    for surface in ["volume", "routing", "layout", "format", "mode"] {
        assert_eq!(
            response["surfaces"][surface],
            json!(true),
            "surface {surface}"
        );
    }
    assert!(response["surface_reasons"]
        .as_object()
        .expect("surface_reasons object")
        .is_empty());
    assert_calibration_disabled(&response);
}

#[test]
fn status_reports_snapshot_fields_capabilities_and_age() {
    let mut harness = Harness::new();
    harness.publish();
    let response = harness.call_json(&json!({"op": "status"}));
    assert_ok(&response);
    assert_eq!(
        response["protocol_version"],
        json!(khaen_host::control::PROTOCOL_VERSION)
    );
    assert_eq!(response["runtime_state"], json!("running"));
    assert_eq!(response["blocks"], json!(0));
    assert_eq!(response["mute"], json!(false));
    assert_eq!(response["silent"], json!(false));
    assert_eq!(response["safe_mode"], json!(false));
    assert_eq!(response["mode"], json!("aes67"));
    assert_eq!(response["wired"], json!(WIRED_OPS));
    assert_calibration_disabled(&response);
    // 没有已生效方案：图字段为 null 且必须给原因，不允许编数。
    assert_eq!(response["graph_id"], Value::Null);
    assert_eq!(response["actual_version"], Value::Null);
    assert_eq!(response["epoch"], Value::Null);
    for field in ["graph_id", "actual_version", "epoch"] {
        assert!(
            response["reasons"][field].is_string(),
            "field {field} must have a reason: {response}"
        );
    }
    // 格式与设备字段来自真实快照。
    assert_eq!(response["format"]["sample_rate"], json!(SAMPLE_RATE));
    assert_eq!(response["format"]["block_size"], json!(BLOCK_FRAMES));
    assert_eq!(response["format"]["channels"], json!(CHANNELS));
    assert_eq!(response["control_format"], response["format"]);
    assert_eq!(response["format_mismatch"], json!(false));
    assert_eq!(response["device_restart_required"], json!(false));
    assert_eq!(response["device_state"], json!("present"));
    assert!(
        response["age_ms"].as_u64().is_some(),
        "age_ms must be a number"
    );
}

#[test]
fn status_age_grows_and_marks_stale_snapshots() {
    let mut harness = Harness::new();
    harness.publish_with_age(Duration::from_millis(5_000));
    let response = harness.call_json(&json!({"op": "status"}));
    assert_ok(&response);
    let age = response["age_ms"].as_u64().expect("age_ms");
    assert!(age >= 5_000, "age_ms must reflect capture time, got {age}");
    assert_eq!(response["stale"], json!(true));
    assert_eq!(response["snapshot_interval_ms"], json!(100));
}

#[test]
fn status_reports_absent_device_fail_closed() {
    let mut harness = Harness::new();
    harness.publish();
    harness
        .lifecycle_mut()
        .note_absent(1_234, "alsa pcm open failed: device absent");
    harness.publish();
    let response = harness.call_json(&json!({"op": "status"}));
    assert_ok(&response);
    assert_eq!(response["device_state"], json!("absent"));
    assert_eq!(response["absent_since"], json!(1_234));
    assert_eq!(response["safe_mode"], json!(true));
    assert_eq!(response["silent"], json!(true));
    assert!(response["reasons"]["safe_mode"].is_string());
}

// ===========================================================================
// mute / mode
// ===========================================================================

#[test]
fn mute_takes_effect_at_block_boundary_and_is_idempotent() {
    let mut harness = Harness::new();
    harness.publish();

    let first = harness.call_json(&json!({"op": "mute", "mute": true}));
    assert_ok(&first);
    assert_eq!(first["mute"], json!(true));
    assert_eq!(first["applied_block"], json!(1));
    assert_eq!(first["idempotent"], json!(false));

    // 块边界前状态诚实地还是旧的。
    let status = harness.call_json(&json!({"op": "status"}));
    assert_eq!(status["mute"], json!(false));
    assert_eq!(status["blocks"], json!(0));

    harness.run_blocks(1);
    let status = harness.call_json(&json!({"op": "status"}));
    assert_eq!(status["mute"], json!(true));
    assert_eq!(status["silent"], json!(true));
    assert_eq!(status["blocks"], json!(1));

    let repeat = harness.call_json(&json!({"op": "mute", "mute": true}));
    assert_eq!(repeat["idempotent"], json!(true));
    assert_eq!(repeat["applied_block"], json!(1));

    let unmute = harness.call_json(&json!({"op": "mute", "mute": false}));
    assert_ok(&unmute);
    assert_eq!(unmute["idempotent"], json!(false));
    assert_eq!(unmute["applied_block"], json!(2));
    harness.run_blocks(1);
    let status = harness.call_json(&json!({"op": "status"}));
    assert_eq!(status["mute"], json!(false));
}

#[test]
fn mode_surface_switches_output_enable_at_block_boundary() {
    let mut harness = Harness::new();
    harness.publish();

    let prepared = harness.call_json(&json!({
        "op": "surface",
        "surface": "mode",
        "data": {"mode": "off"},
    }));
    assert_ok(&prepared);
    assert_eq!(prepared["surface"], json!("mode"));
    assert_eq!(prepared["mode"], json!("off"));
    assert_eq!(prepared["changed"], json!(true));
    assert_eq!(
        prepared["applies_to"],
        json!("control-plane engine (output enable)")
    );
    let token = text(&prepared);
    let committed = commit_token(&mut harness, &token);
    assert_ok(&committed);
    assert_eq!(committed["changed"], json!(true));
    assert_eq!(committed["applied_block"], json!(1));
    harness.run_blocks(1);
    let status = harness.call_json(&json!({"op": "status"}));
    assert_eq!(status["mode"], json!("off"));
    assert_eq!(status["mute"], json!(true));

    // 回到 aes67：解除静音。同值重复 -> changed=false、commit 为空操作。
    let prepared = harness.call_json(&json!({
        "op": "surface",
        "surface": "mode",
        "data": {"mode": "aes67"},
    }));
    assert_ok(&prepared);
    assert_eq!(prepared["changed"], json!(true));
    let committed = commit_token(&mut harness, &text(&prepared));
    assert_eq!(committed["applied_block"], json!(2));
    harness.run_blocks(1);
    let status = harness.call_json(&json!({"op": "status"}));
    assert_eq!(status["mode"], json!("aes67"));
    assert_eq!(status["mute"], json!(false));

    let same = harness.call_json(&json!({
        "op": "surface",
        "surface": "mode",
        "data": {"mode": "aes67"},
    }));
    assert_eq!(same["changed"], json!(false));
    let noop = commit_token(&mut harness, &text(&same));
    assert_eq!(noop["changed"], json!(false));
    assert_eq!(noop["applied_block"], Value::Null);

    // 未实现的采集源显式拒绝，不假装可选。
    let unsupported = harness.call_json(&json!({
        "op": "surface",
        "surface": "mode",
        "data": {"mode": "toslink"},
    }));
    assert_error(&unsupported, error::BAD_REQUEST);
    assert!(unsupported["message"]
        .as_str()
        .expect("message")
        .contains("not wired"));
}

// ===========================================================================
// prepare / commit / abort / inspect / coeff
// ===========================================================================

#[test]
fn prepare_then_commit_switches_graph_at_block_boundary() {
    let mut harness = Harness::new();
    let plan = identity_plan();
    let bytes = encode_plan(&plan);
    let file = TempPlan::new("identity", &bytes);
    harness.publish();

    let prepared = prepare_plan(&mut harness, file.path(), &file.sha256());
    assert_ok(&prepared);
    let token = text(&prepared);
    assert_eq!(token.len(), 32, "engine_token is 16+16 hex chars");
    assert!(token.chars().all(|character| character.is_ascii_hexdigit()));
    assert_eq!(prepared["planned_version"], json!(1));
    assert_eq!(prepared["graph_id"], json!(plan.graph_id_hex()));
    assert_eq!(prepared["admission"]["accepted"], json!(true));
    assert_eq!(
        prepared["admission"]["budget"]["plan_bytes"],
        json!(bytes.len())
    );

    let committed = commit_token(&mut harness, &token);
    assert_ok(&committed);
    assert_eq!(committed["applied_block"], json!(1));
    assert_eq!(committed["version"], json!(1));
    assert_eq!(committed["all_ack"], json!(true));
    assert_eq!(committed["graph_id"], prepared["graph_id"]);

    // 块边界前：图还没生效（status 如实报 null + switch_pending）。
    harness.publish();
    let status = harness.call_json(&json!({"op": "status"}));
    assert_eq!(status["actual_version"], Value::Null);
    assert_eq!(status["switch_pending"], json!(true));

    // 跑一块 -> 图在本块入口生效。
    harness.run_blocks(1);
    let status = harness.call_json(&json!({"op": "status"}));
    assert_eq!(status["actual_version"], json!(1));
    assert_eq!(status["graph_id"], json!(plan.graph_id_hex()));
    assert_eq!(status["epoch"], json!(1));
    assert_eq!(status["blocks"], json!(1));
    assert_eq!(status["switch_pending"], json!(false));

    // coeff 报真实版本与可回滚列表。
    let coeff = harness.call_json(&json!({"op": "coeff"}));
    assert_ok(&coeff);
    assert_eq!(coeff["version"], json!(1));
    assert_eq!(coeff["last_good_version"], json!(1));
    assert_eq!(coeff["graph_id"], json!(plan.graph_id_hex()));
    assert_eq!(coeff["applied_block"], json!(1));
    assert_eq!(coeff["available_versions"], json!([1]));
}

#[test]
fn prepare_rejects_bad_files_sha_and_admission() {
    let mut harness = Harness::new();

    let missing = std::env::temp_dir().join("khaen-rt-host-control-definitely-missing.plan");
    let response = prepare_plan(&mut harness, &missing, "00");
    assert_error(&response, error::BAD_REQUEST);
    assert!(response["message"]
        .as_str()
        .expect("message")
        .contains("cannot read plan file"));

    let plan = identity_plan();
    let bytes = encode_plan(&plan);
    let file = TempPlan::new("sha-mismatch", &bytes);
    let response = prepare_plan(&mut harness, file.path(), &"0".repeat(64));
    assert_error(&response, error::BAD_REQUEST);
    assert!(response["message"]
        .as_str()
        .expect("message")
        .contains("sha256 mismatch"));

    let garbage = TempPlan::new("garbage", b"not an ABI v3 plan");
    let response = prepare_plan(&mut harness, garbage.path(), &garbage.sha256());
    assert_error(&response, error::BAD_REQUEST);
    assert!(response["message"]
        .as_str()
        .expect("message")
        .contains("cannot decode"));

    // 33 lane 超准入默认上限 32 -> admission_rejected（fail-closed）。
    let wide = wide_plan(33);
    let wide_file = TempPlan::new("wide", &encode_plan(&wide));
    let response = prepare_plan(&mut harness, wide_file.path(), &wide_file.sha256());
    assert_error(&response, error::ADMISSION_REJECTED);
    assert!(response["message"]
        .as_str()
        .expect("message")
        .contains("admission"));
}

#[test]
fn prepare_is_idempotent_for_same_plan_token() {
    let mut harness = Harness::new();
    let plan = identity_plan();
    let bytes = encode_plan(&plan);
    let file = TempPlan::new("idempotent", &bytes);

    let first = prepare_plan(&mut harness, file.path(), &file.sha256());
    assert_ok(&first);
    let second = prepare_plan(&mut harness, file.path(), &file.sha256());
    assert_eq!(first, second, "same token must return the same response");

    // 不同内容 -> 不同 token，但仍是同一个 planned_version（commit 前不消耗版本号）。
    let other = TempPlan::new("idempotent-other", &encode_plan(&variant_plan()));
    let third = prepare_plan(&mut harness, other.path(), &other.sha256());
    assert_ok(&third);
    assert_ne!(text(&third), text(&first));
    assert_eq!(third["planned_version"], first["planned_version"]);
}

#[test]
fn abort_and_commit_token_semantics() {
    let mut harness = Harness::new();
    // 没有草稿：commit/abort 都回 engine_token_invalid。
    assert_error(
        &commit_token(&mut harness, "deadbeef"),
        error::ENGINE_TOKEN_INVALID,
    );
    assert_error(
        &harness.call_json(&json!({"op": "abort", "engine_token": "deadbeef"})),
        error::ENGINE_TOKEN_INVALID,
    );

    let file = TempPlan::new("abort", &encode_plan(&identity_plan()));
    let prepared = prepare_plan(&mut harness, file.path(), &file.sha256());
    let token = text(&prepared);

    // 错 token 的 abort 不销毁草稿：之后用正确 token 还能 commit。
    let wrong = harness.call_json(&json!({"op": "abort", "engine_token": "beef"}));
    assert_error(&wrong, error::ENGINE_TOKEN_INVALID);
    let wrong_commit = commit_token(&mut harness, "beef");
    assert_error(&wrong_commit, error::ENGINE_TOKEN_INVALID);

    let aborted = harness.call_json(&json!({"op": "abort", "engine_token": token}));
    assert_ok(&aborted);
    assert_eq!(aborted["aborted"], json!(true));
    assert_error(
        &commit_token(&mut harness, &token),
        error::ENGINE_TOKEN_INVALID,
    );
    assert_error(
        &harness.call_json(&json!({"op": "abort", "engine_token": token})),
        error::ENGINE_TOKEN_INVALID,
    );
}

#[test]
fn coeff_before_any_plan_is_null_with_reasons() {
    let mut harness = Harness::new();
    harness.publish();
    let response = harness.call_json(&json!({"op": "coeff"}));
    assert_ok(&response);
    for field in ["version", "last_good_version", "graph_id", "applied_block"] {
        assert_eq!(response[field], Value::Null, "field {field}");
        assert!(
            response["reasons"][field].is_string(),
            "field {field} must carry a reason: {response}"
        );
    }
    assert_eq!(response["available_versions"], json!([]));
}

#[test]
fn inspect_is_read_only_and_reports_admission_and_compile() {
    let mut harness = Harness::new();
    harness.publish();
    let plan = identity_plan();
    let bytes = encode_plan(&plan);
    let file = TempPlan::new("inspect", &bytes);

    let response = harness.call_json(&json!({
        "op": "inspect",
        "plan_path": file.path().display().to_string(),
        "plan_sha256": file.sha256(),
    }));
    assert_ok(&response);
    assert_eq!(response["staged"], json!(false));
    assert_eq!(response["sha256"], json!(file.sha256()));
    assert_eq!(response["graph_id"], json!(plan.graph_id_hex()));
    assert_eq!(response["plan"]["block_size"], json!(BLOCK_FRAMES));
    assert_eq!(response["plan"]["sample_rate"], json!(SAMPLE_RATE));
    assert_eq!(response["admission"]["accepted"], json!(true));
    assert_eq!(response["compile"]["compiled"], json!(true));
    assert!(response["compile"]["latency_samples"].is_u64());

    // 只读：没有草稿（commit 回 token invalid），状态也没变。
    assert_error(
        &commit_token(&mut harness, "deadbeef"),
        error::ENGINE_TOKEN_INVALID,
    );
    let status = harness.call_json(&json!({"op": "status"}));
    assert_eq!(status["actual_version"], Value::Null);

    // sha 不符 / 不可解码 -> bad_request。
    let response = harness.call_json(&json!({
        "op": "inspect",
        "plan_path": file.path().display().to_string(),
        "plan_sha256": "0".repeat(64),
    }));
    assert_error(&response, error::BAD_REQUEST);
    let garbage = TempPlan::new("inspect-garbage", b"garbage");
    let response = harness.call_json(&json!({
        "op": "inspect",
        "plan_path": garbage.path().display().to_string(),
    }));
    assert_error(&response, error::BAD_REQUEST);

    // 准入拒绝仍然 ok:true（一次看到多个问题），但 accepted/compiled 如实为 false。
    let wide_file = TempPlan::new("inspect-wide", &encode_plan(&wide_plan(33)));
    let response = harness.call_json(&json!({
        "op": "inspect",
        "plan_path": wide_file.path().display().to_string(),
    }));
    assert_ok(&response);
    assert_eq!(response["admission"]["accepted"], json!(false));
    assert_eq!(response["compile"]["compiled"], json!(false));
    assert_eq!(
        response["compile"]["error_code"],
        json!(error::ADMISSION_REJECTED)
    );
    assert!(!response["admission"]["rejections"]
        .as_array()
        .expect("rejections")
        .is_empty());
}

// ===========================================================================
// surface：volume / routing / layout
// ===========================================================================

#[test]
fn surface_requires_a_committed_base_plan() {
    let mut harness = Harness::new();
    for request in [
        json!({"op": "surface", "surface": "volume", "data": {"gains_db": [{"lane": 0, "gain_db": -3.0}]}}),
        json!({"op": "surface", "surface": "routing", "data": {"routes": [[0, 0], [1, 1]]}}),
        json!({"op": "surface", "surface": "layout", "data": {"in_channels": 2, "out_lanes": 2, "folds": [[0, 0, 1.0], [1, 1, 1.0]]}}),
    ] {
        let response = harness.call_json(&request);
        assert_error(&response, error::NO_BASE_PLAN);
    }
}

#[test]
fn surface_volume_patch_applies_gain_at_block_boundary() {
    let mut harness = Harness::new();
    prepare_commit_identity(&mut harness);

    // 恒等方案的冲激响应：输出 = 输入（图执行器 identity 判据）。
    let size = BLOCK_FRAMES * CHANNELS;
    let mut impulse = vec![0.0f32; size];
    impulse[0] = 1.0;
    harness.run_block_input(&impulse);
    let output = harness.last_output();
    assert!(
        (output[0] - 1.0).abs() < 1e-6,
        "identity plan must pass lane 0"
    );
    assert!(output[1].abs() < 1e-6);

    // -6.0206 dB = 0.5 倍。
    let prepared = harness.call_json(&json!({
        "op": "surface",
        "surface": "volume",
        "data": {"gains_db": [{"lane": 0, "gain_db": -6.020_599_9}]},
    }));
    assert_ok(&prepared);
    assert_eq!(prepared["surface"], json!("volume"));
    assert_eq!(prepared["planned_version"], json!(2));
    let token = text(&prepared);
    let committed = commit_token(&mut harness, &token);
    assert_ok(&committed);
    // 前面已跑过一块（冲激响应），所以新图将在第 3 块入口生效。
    assert_eq!(committed["applied_block"], json!(3));
    harness.run_block_input(&impulse);
    let output = harness.last_output();
    assert!(
        (output[0] - 0.5).abs() < 0.01,
        "volume patch must scale lane 0 by 0.5, got {}",
        output[0]
    );
    harness.publish();
    let status = harness.call_json(&json!({"op": "status"}));
    assert_eq!(status["actual_version"], json!(2));
    assert_eq!(status["blocks"], json!(3));

    // 非法补丁：空表 / 未知 lane / 非有限值（JSON 里用字符串表达类型错误）。
    for data in [
        json!({"gains_db": []}),
        json!({"gains_db": [{"lane": 9, "gain_db": -3.0}]}),
        json!({"gains_db": [{"lane": 0, "gain_db": "loud"}]}),
        json!({"gains_db": [{"lane": -1, "gain_db": -3.0}]}),
    ] {
        let response =
            harness.call_json(&json!({"op": "surface", "surface": "volume", "data": data}));
        assert_error(&response, error::BAD_REQUEST);
    }
}

#[test]
fn surface_routing_and_layout_patches_change_the_running_graph() {
    let mut harness = Harness::new();
    prepare_commit_identity(&mut harness);
    let size = BLOCK_FRAMES * CHANNELS;
    let mut impulse = vec![0.0f32; size];
    impulse[0] = 1.0;

    // routing: lane 0 -> dac 1, lane 1 -> dac 0（交换）。
    let prepared = harness.call_json(&json!({
        "op": "surface",
        "surface": "routing",
        "data": {"routes": [[0, 1], [1, 0]], "dac_channels": 2},
    }));
    assert_ok(&prepared);
    assert_eq!(prepared["planned_version"], json!(2));
    let committed = commit_token(
        &mut harness,
        prepared["engine_token"].as_str().expect("token"),
    );
    assert_ok(&committed);
    harness.run_block_input(&impulse);
    let output = harness.last_output();
    assert!(output[0].abs() < 1e-6, "dac 0 must be silent after swap");
    assert!((output[1] - 1.0).abs() < 1e-6, "lane 0 must reach dac 1");

    // layout: 交换 content 折叠（in 0 -> lane 1），叠加在已交换的 routing 上 =
    // 冲激从 dac 0 出来。
    let prepared = harness.call_json(&json!({
        "op": "surface",
        "surface": "layout",
        "data": {
            "in_channels": 2,
            "out_lanes": 2,
            "folds": [[1, 0, 1.0], [0, 1, 1.0]],
        },
    }));
    assert_ok(&prepared);
    assert_eq!(prepared["planned_version"], json!(3));
    let committed = commit_token(
        &mut harness,
        prepared["engine_token"].as_str().expect("token"),
    );
    assert_ok(&committed);
    harness.run_block_input(&impulse);
    let output = harness.last_output();
    assert!(
        (output[0] - 1.0).abs() < 1e-6,
        "double swap must route lane 1 back to dac 0"
    );

    // routing 越界：dac 必须 < dac_channels；空表拒绝。
    for data in [
        json!({"routes": [[0, 2]], "dac_channels": 2}),
        json!({"routes": []}),
        json!({"routes": [[0]]}),
    ] {
        let response =
            harness.call_json(&json!({"op": "surface", "surface": "routing", "data": data}));
        assert_error(&response, error::BAD_REQUEST);
    }
}

// ===========================================================================
// rollback
// ===========================================================================

#[test]
fn rollback_requires_history_and_rolls_forward_with_a_new_version() {
    let mut harness = Harness::new();
    assert_error(
        &harness.call_json(&json!({"op": "rollback"})),
        error::NO_HISTORY,
    );

    // v1: 恒等；v2: lane0 -3 dB。
    let v1_plan = identity_plan();
    let v1_file = TempPlan::new("rollback-v1", &encode_plan(&v1_plan));
    harness.publish();
    let prepared = prepare_plan(&mut harness, v1_file.path(), &v1_file.sha256());
    assert_eq!(
        commit_token(&mut harness, &text(&prepared))["version"],
        json!(1)
    );
    harness.run_blocks(1);
    let v1_graph = v1_plan.graph_id_hex();

    let v2_plan = variant_plan();
    let v2_file = TempPlan::new("rollback-v2", &encode_plan(&v2_plan));
    let prepared = prepare_plan(&mut harness, v2_file.path(), &v2_file.sha256());
    assert_eq!(
        commit_token(&mut harness, &text(&prepared))["version"],
        json!(2)
    );
    harness.run_blocks(1);
    let coeff = harness.call_json(&json!({"op": "coeff"}));
    assert_eq!(coeff["available_versions"], json!([1, 2]));

    // 回滚 = 前进式提交：版本 3，历史不改写，内容来自版本 1。
    let rolled = harness.call_json(&json!({"op": "rollback"}));
    assert_ok(&rolled);
    assert_eq!(rolled["rolled_back_to"], json!(1));
    assert_eq!(rolled["planned_version"], json!(3));
    assert_eq!(rolled["graph_id"], json!(v1_graph));
    let committed = commit_token(&mut harness, &text(&rolled));
    assert_ok(&committed);
    assert_eq!(committed["version"], json!(3));
    harness.run_blocks(1);
    let status = harness.call_json(&json!({"op": "status"}));
    assert_eq!(status["actual_version"], json!(3));
    assert_eq!(status["graph_id"], json!(v1_graph));
    let coeff = harness.call_json(&json!({"op": "coeff"}));
    assert_eq!(coeff["available_versions"], json!([1, 2, 3]));

    // target = 当前版本 / 不存在版本 / 唯一版本时缺省回滚 -> bad_request。
    for request in [
        json!({"op": "rollback", "target": 3}),
        json!({"op": "rollback", "target": 99}),
    ] {
        assert_error(&harness.call_json(&request), error::BAD_REQUEST);
    }
}

#[test]
fn rollback_with_single_version_and_default_target_is_rejected() {
    let mut harness = Harness::new();
    let file = TempPlan::new("rollback-single", &encode_plan(&identity_plan()));
    harness.publish();
    let prepared = prepare_plan(&mut harness, file.path(), &file.sha256());
    commit_token(&mut harness, &text(&prepared));
    let response = harness.call_json(&json!({"op": "rollback"}));
    assert_error(&response, error::BAD_REQUEST);
    assert!(response["message"]
        .as_str()
        .expect("message")
        .contains("nothing to roll back"));
}

// ===========================================================================
// format
// ===========================================================================

#[test]
fn format_noop_and_control_plane_only_change_are_honest() {
    let mut harness = Harness::new();
    harness.publish();

    // 同格式：changed=false，commit 空操作。
    let same = harness.call_json(&json!({"op": "format", "sample_rate": SAMPLE_RATE}));
    assert_ok(&same);
    assert_eq!(same["changed"], json!(false));
    assert_eq!(same["device_restart_required"], json!(false));
    let noop = commit_token(&mut harness, &text(&same));
    assert_ok(&noop);
    assert_eq!(noop["changed"], json!(false));
    assert_eq!(noop["applied_block"], Value::Null);

    // 目标格式变更：两阶段只改控制面编译格式，数据面不动，如实报 device_restart_required。
    let changed = harness.call_json(&json!({"op": "format", "sample_rate": 96_000}));
    assert_ok(&changed);
    assert_eq!(changed["changed"], json!(true));
    assert_eq!(changed["device_restart_required"], json!(true));
    assert_eq!(changed["format"]["sample_rate"], json!(96_000));
    let committed = commit_token(&mut harness, &text(&changed));
    assert_ok(&committed);
    assert_eq!(committed["changed"], json!(true));
    assert_eq!(
        committed["applied_block"],
        Value::Null,
        "no data-plane block action happened"
    );
    assert_eq!(committed["pcm_reconfigured"], json!(false));
    assert_eq!(committed["device_restart_required"], json!(true));
    assert_eq!(committed["applies_to"], json!("control-plane engine"));

    harness.publish();
    let status = harness.call_json(&json!({"op": "status"}));
    assert_eq!(
        status["format"]["sample_rate"],
        json!(SAMPLE_RATE),
        "running data plane keeps its format"
    );
    assert_eq!(status["control_format"]["sample_rate"], json!(96_000));
    assert_eq!(status["format_mismatch"], json!(true));
    assert_eq!(status["device_restart_required"], json!(true));
    assert!(status["reasons"]["device_restart_required"].is_string());

    // 控制面按 96 kHz 编译：48 kHz 方案现在编译不过（bad_request，不静默接受）。
    let mismatched = TempPlan::new("format-48k", &encode_plan(&identity_plan()));
    let response = prepare_plan(&mut harness, mismatched.path(), &mismatched.sha256());
    assert_error(&response, error::BAD_REQUEST);

    // 96 kHz 方案可以准备，但 commit 必须如实回 not_applied（数据面格式不匹配）。
    let mut plan96 = identity_plan();
    plan96.sample_rate = 96_000;
    let file96 = TempPlan::new("format-96k", &encode_plan(&plan96));
    let prepared = prepare_plan(&mut harness, file96.path(), &file96.sha256());
    assert_ok(&prepared);
    let token = text(&prepared);
    let rejected = commit_token(&mut harness, &token);
    assert_error(&rejected, error::NOT_APPLIED);
    assert!(rejected["message"]
        .as_str()
        .expect("message")
        .contains("restart"));
    // 草稿仍在：可 abort（错 token 不销毁的同一语义）。
    let aborted = harness.call_json(&json!({"op": "abort", "engine_token": token}));
    assert_ok(&aborted);

    // 把控制面格式改回数据面格式：mismatch 清零，正常准备/提交换图恢复可用。
    let back = harness.call_json(&json!({"op": "format", "sample_rate": SAMPLE_RATE}));
    assert_eq!(back["changed"], json!(true));
    let committed = commit_token(&mut harness, &text(&back));
    assert_ok(&committed);
    assert_eq!(committed["device_restart_required"], json!(false));
    harness.publish();
    let status = harness.call_json(&json!({"op": "status"}));
    assert_eq!(status["format_mismatch"], json!(false));

    let file = TempPlan::new("format-recovered", &encode_plan(&identity_plan()));
    let prepared = prepare_plan(&mut harness, file.path(), &file.sha256());
    assert_ok(&prepared);
    assert_ok(&commit_token(&mut harness, &text(&prepared)));
    harness.run_blocks(1);
    let status = harness.call_json(&json!({"op": "status"}));
    // 版本号单调递增、不复用：两次 format commit 已消耗版本 1/2，这份方案是版本 3。
    assert_eq!(status["actual_version"], json!(3));
}

#[test]
fn format_rejects_invalid_targets() {
    let mut harness = Harness::new();
    for request in [
        json!({"op": "format", "sample_rate": 44_100}),
        json!({"op": "format", "chunksize": 999}),
        json!({"op": "format"}),
        json!({"op": "format", "sample_rate": "fast"}),
        json!({"op": "format", "sample_rate": -1}),
        json!({"op": "format", "chunksize": 70_000}),
    ] {
        assert_error(&harness.call_json(&request), error::BAD_REQUEST);
    }
}

// ===========================================================================
// 跨线程语义：命令超时 / 通道关闭 / bridge 空操作
// ===========================================================================

#[test]
fn command_without_block_loop_returns_not_applied() {
    // 接收端存在但没人消费：等 ack 超时 -> not_applied（不得假报成功）。
    let (command_tx, _command_rx) = std::sync::mpsc::channel();
    let snapshot = std::sync::Arc::new(std::sync::Mutex::new(None));
    let mut core = ControlCore::new(ControlCoreConfig {
        data_plane_format: test_config(),
        limits: khaen_host::AdmissionLimits::default(),
        commands: command_tx,
        snapshot,
        mode: RuntimeMode::Aes67,
        initial_plan: None,
        initial_version: 0,
        command_timeout: Duration::from_millis(100),
        calibration: None,
        channels: None,
        reconfigure: None,
    });
    let started = Instant::now();
    let response: Value =
        serde_json::from_str(&core.handle_line(r#"{"op":"mute","mute":true}"#)).expect("json");
    assert_error(&response, error::NOT_APPLIED);
    assert!(
        started.elapsed() >= Duration::from_millis(90),
        "must actually wait for the ack before giving up"
    );

    // 接收端已经关闭：立即回 not_applied，消息说明块循环不在跑。
    let (command_tx, command_rx) = std::sync::mpsc::channel();
    drop(command_rx);
    let snapshot = std::sync::Arc::new(std::sync::Mutex::new(None));
    let mut core = ControlCore::new(ControlCoreConfig {
        data_plane_format: test_config(),
        limits: khaen_host::AdmissionLimits::default(),
        commands: command_tx,
        snapshot,
        mode: RuntimeMode::Aes67,
        initial_plan: None,
        initial_version: 0,
        command_timeout: Duration::from_millis(100),
        calibration: None,
        channels: None,
        reconfigure: None,
    });
    let response: Value =
        serde_json::from_str(&core.handle_line(r#"{"op":"mute","mute":true}"#)).expect("json");
    assert_error(&response, error::NOT_APPLIED);
    assert!(response["message"]
        .as_str()
        .expect("message")
        .contains("not running"));
}

#[test]
fn disabled_bridge_is_a_no_op() {
    let mut runtime = fake_runtime();
    let mut bridge = ControlBridge::disabled();
    assert!(!bridge.enabled());
    let mut lifecycle = DeviceLifecycle::new();
    assert_eq!(bridge.poll_commands(&mut runtime, &mut lifecycle, 0), 0);
    bridge.maybe_publish(&runtime, &lifecycle);
    // 不 panic、不改变运行期状态即可。
    assert_eq!(runtime.blocks(), 0);
}

// ===========================================================================
// Runtime 适配层（ControlTarget for Runtime<P>）
// ===========================================================================

#[test]
fn control_target_runtime_applies_commands_and_rejects_format_mismatch() {
    let mut runtime = fake_runtime();
    let (command_tx, command_rx) = std::sync::mpsc::channel();
    let snapshot = std::sync::Arc::new(std::sync::Mutex::new(None));
    let mut bridge = ControlBridge::new(command_rx, std::sync::Arc::clone(&snapshot), 1);
    let mut lifecycle = DeviceLifecycle::new();
    bridge.publish(&runtime, &lifecycle);

    // mute：回执 applied_block=1，块边界后才生效。
    let (ack_tx, ack_rx) = std::sync::mpsc::channel();
    command_tx
        .send(ControlCommand::Mute {
            mute: true,
            ack: ack_tx,
        })
        .expect("queue mute");
    assert_eq!(bridge.poll_commands(&mut runtime, &mut lifecycle, 0), 1);
    match ack_rx.recv().expect("mute ack") {
        CommandReply::Mute {
            mute,
            applied_block,
            idempotent,
        } => {
            assert!(mute);
            assert_eq!(applied_block, 1);
            assert!(!idempotent);
        }
        other => panic!("unexpected reply: {other:?}"),
    }
    assert!(!runtime.muted(), "mute must not be applied before a block");
    runtime.run_block().expect("block");
    assert!(runtime.muted());

    // commit：控制线程编译草稿 -> 命令 -> 块边界换图。
    let plan = identity_plan();
    let draft = compile_plan_draft(
        &plan,
        &runtime.config().engine,
        &khaen_host::AdmissionLimits::default(),
        1,
    )
    .expect("compile draft");
    let (ack_tx, ack_rx) = std::sync::mpsc::channel();
    command_tx
        .send(ControlCommand::CommitPlan {
            draft,
            format: runtime.config().engine.clone(),
            ack: ack_tx,
        })
        .expect("queue commit");
    assert_eq!(bridge.poll_commands(&mut runtime, &mut lifecycle, 0), 1);
    match ack_rx.recv().expect("commit ack") {
        CommandReply::PlanApplied { applied_block } => assert_eq!(applied_block, 2),
        other => panic!("unexpected reply: {other:?}"),
    }
    runtime.run_block().expect("block");
    assert_eq!(runtime.graph_info().version, Some(1));
    assert_eq!(runtime.graph_info().applied_block, Some(2));

    // 快照反映真实引擎状态（bridge 命令后已发布）。
    bridge.publish(&runtime, &lifecycle);
    let snapshot = snapshot
        .lock()
        .expect("snapshot lock")
        .clone()
        .expect("snapshot");
    assert_eq!(snapshot.status.graph.version, Some(1));
    assert_eq!(snapshot.status.blocks, 2);
    assert_eq!(snapshot.format, test_config());

    // 格式不匹配（96 kHz 草稿）必须被块线程如实拒绝：不 stage、不改变正在跑的图。
    let mut plan96 = identity_plan();
    plan96.sample_rate = 96_000;
    let config96 = khaen_host::engine::EngineConfig {
        sample_rate: 96_000,
        ..test_config()
    };
    let draft96 = compile_plan_draft(
        &plan96,
        &config96,
        &khaen_host::AdmissionLimits::default(),
        2,
    )
    .expect("compile 96k draft");
    let (ack_tx, ack_rx) = std::sync::mpsc::channel();
    command_tx
        .send(ControlCommand::CommitPlan {
            draft: draft96,
            format: config96,
            ack: ack_tx,
        })
        .expect("queue mismatched commit");
    assert_eq!(bridge.poll_commands(&mut runtime, &mut lifecycle, 0), 1);
    match ack_rx.recv().expect("reject ack") {
        CommandReply::Rejected { code, message } => {
            assert_eq!(code, error::NOT_APPLIED);
            assert!(message.contains("not staged"));
            assert!(message.contains("channels_prepare"));
        }
        other => panic!("unexpected reply: {other:?}"),
    }
    assert_eq!(runtime.graph_info().version, Some(1));
}

// ===========================================================================
// 传输：内存双工（所有平台）
// ===========================================================================

#[test]
fn serve_connection_answers_one_line_per_request() {
    let mut harness = Harness::new();
    harness.publish();
    let core = harness.core();
    let responses = serve_in_memory(
        &core,
        b"{\"op\":\"status\"}\nnot json at all\n{\"op\":\"frobnicate\"}\n",
    );
    assert_eq!(responses.len(), 3);
    let status: Value = serde_json::from_str(&responses[0]).expect("status json");
    assert_eq!(status["ok"], json!(true));
    let bad: Value = serde_json::from_str(&responses[1]).expect("bad json");
    assert_eq!(bad["error"], json!(error::BAD_REQUEST));
    let unknown: Value = serde_json::from_str(&responses[2]).expect("unknown json");
    assert_eq!(unknown["error"], json!(error::UNKNOWN_OP));
    for response in &responses {
        assert!(!response.starts_with('\u{feff}'), "no BOM allowed");
        assert!(!response.contains('\n'), "one JSON object per line");
    }
}

#[test]
fn serve_connection_rejects_oversized_line_and_resyncs() {
    let mut harness = Harness::new();
    harness.publish();
    let core = harness.core();
    let mut input = Vec::new();
    input.extend_from_slice(
        format!(r#"{{"op":"status","pad":"{}"}}"#, "x".repeat(70_000)).as_bytes(),
    );
    input.push(b'\n');
    input.extend_from_slice(b"{\"op\":\"status\"}\n");
    let responses = serve_in_memory(&core, &input);
    assert_eq!(
        responses.len(),
        2,
        "oversized line -> one error, then resync"
    );
    let error_response: Value = serde_json::from_str(&responses[0]).expect("error json");
    assert_eq!(error_response["error"], json!(error::BAD_REQUEST));
    let status: Value = serde_json::from_str(&responses[1]).expect("status json");
    assert_eq!(status["ok"], json!(true));
}

#[test]
fn serve_connection_handles_final_line_without_newline() {
    let mut harness = Harness::new();
    harness.publish();
    let core = harness.core();
    let responses = serve_in_memory(&core, b"{\"op\":\"status\"}");
    assert_eq!(responses.len(), 1);
    let status: Value = serde_json::from_str(&responses[0]).expect("status json");
    assert_eq!(status["ok"], json!(true));
}

// ===========================================================================
// 传输：TCP 回环（所有平台；验证每连接一个处理线程 + 共享核心串行化）
// ===========================================================================

#[test]
fn tcp_loopback_serves_concurrent_clients_with_serialized_core() {
    use std::io::{BufRead, BufReader, BufWriter, Write};
    use std::net::{TcpListener, TcpStream};

    let mut harness = Harness::new();
    harness.publish();
    let core = harness.core();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let address = listener.local_addr().expect("local addr");
    let server_core = std::sync::Arc::clone(&core);
    let server = std::thread::spawn(move || {
        let mut handlers = Vec::new();
        for _ in 0..2 {
            let (stream, _) = listener.accept().expect("accept");
            let connection_core = std::sync::Arc::clone(&server_core);
            handlers.push(std::thread::spawn(move || {
                let write = stream.try_clone().expect("clone stream");
                let mut reader = BufReader::new(stream);
                let mut writer = BufWriter::new(write);
                serve_connection(&mut reader, &mut writer, &connection_core)
                    .expect("serve connection");
            }));
        }
        for handler in handlers {
            handler.join().expect("handler join");
        }
    });

    let clients: Vec<TcpStream> = (0..2)
        .map(|_| TcpStream::connect(address).expect("connect"))
        .collect();
    let mut client_threads = Vec::new();
    for client in clients {
        client_threads.push(std::thread::spawn(move || {
            let mut writer = client.try_clone().expect("clone client");
            let mut reader = BufReader::new(client);
            for _ in 0..3 {
                writer
                    .write_all(b"{\"op\":\"status\"}\n")
                    .expect("write request");
                writer.flush().expect("flush request");
                let mut line = String::new();
                reader.read_line(&mut line).expect("read response");
                let response: Value = serde_json::from_str(line.trim_end()).expect("response json");
                assert_eq!(response["ok"], json!(true), "response: {response}");
            }
        }));
    }
    for thread in client_threads {
        thread.join().expect("client join");
    }
    server.join().expect("server join");
}

// ===========================================================================
// 传输：Unix domain socket 端到端（cfg(unix)；Linux CI 覆盖）
// ===========================================================================

#[cfg(unix)]
fn unique_socket_path(tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "khaen-rt-host-control-{}-{tag}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    path
}

#[cfg(unix)]
#[test]
fn unix_socket_end_to_end_status_mute_prepare_commit() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let mut harness = Harness::new();
    harness.publish();
    let path = unique_socket_path("e2e");
    let server = super::start(
        &ControlConfig {
            enabled: true,
            socket_path: path.clone(),
        },
        harness.core(),
    )
    .expect("start unix control server");
    assert_eq!(server.socket_path(), Some(path.as_path()));

    let stream = UnixStream::connect(&path).expect("connect unix socket");
    let mut writer = stream.try_clone().expect("clone stream");
    let mut reader = BufReader::new(stream);
    let mut call = |request: Value| -> Value {
        let line = request.to_string();
        writer.write_all(line.as_bytes()).expect("write request");
        writer.write_all(b"\n").expect("write newline");
        writer.flush().expect("flush request");
        let mut response = String::new();
        reader.read_line(&mut response).expect("read response");
        serde_json::from_str(response.trim_end()).expect("response json")
    };

    assert_ok(&call(json!({"op": "status"})));

    let mute = call(json!({"op": "mute", "mute": true}));
    assert_ok(&mute);
    assert_eq!(mute["applied_block"], json!(1));

    let plan = identity_plan();
    let bytes = encode_plan(&plan);
    let file = TempPlan::new("unix-e2e", &bytes);
    let prepared = call(json!({
        "op": "prepare",
        "plan_path": file.path().display().to_string(),
        "plan_sha256": file.sha256(),
    }));
    assert_ok(&prepared);
    let token = prepared["engine_token"]
        .as_str()
        .expect("engine token")
        .to_string();
    let committed = call(json!({"op": "commit", "engine_token": token}));
    assert_ok(&committed);
    assert_eq!(committed["version"], json!(1));

    // 块线程侧（本测试的假块线程）跑一块，图在块边界生效。
    harness.run_blocks(1);
    let status = call(json!({"op": "status"}));
    assert_eq!(status["actual_version"], json!(1));
    assert_eq!(status["graph_id"], json!(plan.graph_id_hex()));
    assert_eq!(status["blocks"], json!(1));
    assert_eq!(status["mute"], json!(true));

    // 错误请求不中断连接：坏 JSON 之后还能继续用同一连接。
    let bad = call(json!({"op": "frobnicate"}));
    assert_error(&bad, error::UNKNOWN_OP);
    assert_ok(&call(json!({"op": "status"})));

    drop(reader);
    drop(writer);
    drop(server);
    // Drop 时监听线程退出并移除 socket 文件。
    assert!(
        !path.exists(),
        "control socket file must be removed on shutdown"
    );
}

#[cfg(unix)]
#[test]
fn unix_socket_replaces_stale_file_and_refuses_live_listener() {
    use std::os::unix::net::{UnixListener, UnixStream};

    let mut harness = Harness::new();
    harness.publish();

    // 陈旧 socket 文件（没有监听者）：删掉重建，不该让会话起不来。
    let path = unique_socket_path("stale");
    std::fs::write(&path, b"stale socket file").expect("write stale file");
    let server = super::start(
        &ControlConfig {
            enabled: true,
            socket_path: path.clone(),
        },
        harness.core(),
    )
    .expect("stale socket file is replaced");
    let stream = UnixStream::connect(&path).expect("connect after stale replacement");
    drop(stream);
    drop(server);
    let _ = std::fs::remove_file(&path);

    // 活着的监听者：拒绝启动（避免两个 rt-host 抢同一个控制面）。
    let live_path = unique_socket_path("live");
    let live = UnixListener::bind(&live_path).expect("bind live listener");
    let err = super::start(
        &ControlConfig {
            enabled: true,
            socket_path: live_path.clone(),
        },
        harness.core(),
    )
    .expect_err("live listener must be refused");
    assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
    drop(live);
    let _ = std::fs::remove_file(&live_path);
}

// ===========================================================================
// 配置解析
// ===========================================================================

#[test]
fn control_config_parses_env_values() {
    assert_eq!(
        ControlConfig::parse(None, None).expect("default"),
        ControlConfig::default()
    );
    for off in ["off", "OFF", "0", "false", "False"] {
        let config = ControlConfig::parse(Some(off.to_string()), None).expect("off value");
        assert!(!config.enabled, "{off} must disable the control channel");
    }
    for on in ["on", "1", "true", "ON"] {
        let config = ControlConfig::parse(Some(on.to_string()), None).expect("on value");
        assert!(config.enabled, "{on} must enable the control channel");
    }
    let error = ControlConfig::parse(Some("maybe".to_string()), None).expect_err("bogus value");
    assert!(error.message().contains("KHAEN_CONTROL"));
    let error = ControlConfig::parse(None, Some(String::new())).expect_err("empty socket path");
    assert!(error.message().contains("KHAEN_CONTROL_SOCKET"));
    let custom = ControlConfig::parse(None, Some("/tmp/khaen-test.sock".to_string()))
        .expect("custom socket path");
    assert_eq!(
        custom.socket_path,
        std::path::PathBuf::from("/tmp/khaen-test.sock")
    );
}
