//! 热重配（M3b）协议门禁：channels_status / channels_prepare / channels_commit /
//! channels_abort（Windows 可跑；假块线程 + 假只读探测，真机 ALSA/网卡路径 NOT_RUN）。
//!
//! 覆盖：能力上报/未接线、prepare 校验（map 长度/PT/支持速率/准入/未知字段/探测失败）、
//! 幂等与 abort、单活动 busy、正常提交（块边界/PID/gap/generation/静音保持/音频继续）、
//! 无变化 no-op、块线程失败回滚（generation 不前进/如实 rolled_back）、设备 absent 拒绝、
//! 与校准模式互斥（cal_busy / reconfiguring）、ack 丢失后的 channels_busy 与对账重放。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use khaen_calibration::capture::UmikDeviceConfig;
use khaen_host::control::error as host_error;
use khaen_host::runtime::{HwReadback, ReconfigureFailure};
use khaen_host::AdmissionLimits;
use serde_json::{json, Value};

use crate::control::cal::engine::FakeCalFactory;
use crate::control::cal::{CalEngineKind, CalibrationConfig, CalibrationSetup};
use crate::control::command::ControlCommand;
use crate::control::core::{ControlCore, ControlCoreConfig};
use crate::control::reconfigure::error as hot_error;
use crate::control::reconfigure::CHANNELS_OPS;
use crate::control::reconfigure::{
    shared_tracker, Aes67InputSource, AppliedReconfigure, ChannelsConfig, ChannelsState,
    ChannelsView,
};
use crate::control::snapshot::SharedSnapshot;
use crate::control::test_support::{fake_runtime_config, FakeProbe, FakeProbeState, Harness};

// ===========================================================================
// 夹具
// ===========================================================================

#[test]
fn channels_status_reports_aes67_input_source_provenance() {
    let sdp = ChannelsState::new(
        fake_runtime_config(),
        Some(Aes67InputSource::Sdp {
            sha256: "0123456789abcdef".to_string(),
        }),
        Arc::new(FakeProbe::new(FakeProbeState::default())),
        shared_tracker(),
        AdmissionLimits::default(),
    );
    let status = sdp.status_json(None);
    assert_eq!(status["input_source"]["source"], json!("sdp"));
    assert_eq!(
        status["input_source"]["sdp_sha256"],
        json!("0123456789abcdef")
    );

    let manual = ChannelsState::new(
        fake_runtime_config(),
        Some(Aes67InputSource::Manual),
        Arc::new(FakeProbe::new(FakeProbeState::default())),
        shared_tracker(),
        AdmissionLimits::default(),
    );
    let status = manual.status_json(None);
    assert_eq!(status["input_source"]["source"], json!("manual"));
    assert!(status["input_source"]["sdp_sha256"].is_null());

    let unknown = ChannelsState::new(
        fake_runtime_config(),
        None,
        Arc::new(FakeProbe::new(FakeProbeState::default())),
        shared_tracker(),
        AdmissionLimits::default(),
    );
    assert!(unknown.status_json(None)["input_source"].is_null());
}

/// 临时校准产物目录（Drop 删除；文件名带进程号 + 原子计数）。
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "khaen-rt-host-hot-{}-{tag}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn fake_cal_sha() -> String {
    "ab".repeat(32)
}

/// 离线合成校准装配（cal 互斥测试用；不需要真 UMIK）。
fn fake_calibration_setup(dir: &Path) -> CalibrationSetup {
    let config = CalibrationConfig {
        artifact_dir: dir.to_path_buf(),
        umik_device: "hw:0,0".to_string(),
        capture_channels: 2,
        capture_channel: Some(0),
        mic: Some(UmikDeviceConfig::umik1(
            "hw:0,0",
            "718-7696",
            "umik1-90deg.txt",
            fake_cal_sha(),
            "90deg",
        )),
        mic_missing_reason: None,
        layout: "stereo-2.0".to_string(),
        channels: vec![0, 1],
        level_dbfs: -12.0,
        engine: CalEngineKind::Fake,
    };
    CalibrationSetup {
        config,
        factory: Box::new(FakeCalFactory::new()),
    }
}

fn call(harness: &mut Harness, request: Value) -> Value {
    harness.call_json(&request)
}

fn assert_error(value: &Value, code: &str) {
    assert_eq!(value["ok"], json!(false), "expected error {code}: {value}");
    assert_eq!(value["error"], json!(code), "unexpected error: {value}");
}

fn prepare(harness: &mut Harness, fields: Value) -> Value {
    let mut request = fields;
    request["op"] = json!("channels_prepare");
    call(harness, request)
}

fn commit(harness: &mut Harness, token: &str) -> Value {
    call(harness, json!({ "op": "channels_commit", "token": token }))
}

fn token_of(response: &Value) -> String {
    response["token"]
        .as_str()
        .unwrap_or_else(|| panic!("channels_prepare must return a token: {response}"))
        .to_string()
}

// ===========================================================================
// 能力上报 / 未接线
// ===========================================================================

#[test]
fn channels_status_reports_current_configuration_and_generation() {
    let mut harness = Harness::with_channels();
    harness.publish();
    let status = call(&mut harness, json!({ "op": "channels_status" }));
    assert_eq!(status["ok"], json!(true), "{status}");
    assert_eq!(status["generation"], json!(0));
    assert_eq!(status["current"]["channels"], json!(2));
    assert_eq!(status["current"]["sample_rate"], json!(48_000));
    assert_eq!(status["current"]["aes67_port"], json!(5_004));
    assert_eq!(status["current"]["aes67_payload_type"], json!(96));
    assert_eq!(status["current"]["aes67_map"], json!([0, 1]));
    assert_eq!(status["device"]["state"], json!("present"));
    assert_eq!(status["device"]["probe"]["available"], json!(true));
    assert_eq!(status["busy"], json!(false));
    assert!(status["last_result"].is_null());

    // 全局 status：wired 含 channels_*，reconfigure 增量字段存在。
    let global = call(&mut harness, json!({ "op": "status" }));
    let wired = global["wired"].as_array().expect("wired array");
    for op in CHANNELS_OPS {
        assert!(
            wired.iter().any(|value| value.as_str() == Some(op)),
            "channels op {op} must be wired: {global}"
        );
    }
    assert_eq!(global["reconfigure"]["generation"], json!(0));
    assert_eq!(global["reconfigure"]["in_flight"], json!(false));
}

#[test]
fn channels_ops_are_not_wired_without_channel_context() {
    let mut harness = Harness::new();
    harness.publish();
    let status = call(&mut harness, json!({ "op": "channels_status" }));
    assert_error(&status, host_error::UNSUPPORTED);
    let global = call(&mut harness, json!({ "op": "status" }));
    let wired = global["wired"].as_array().expect("wired");
    let not_wired = global["not_wired"].as_array().expect("not_wired");
    for op in CHANNELS_OPS {
        assert!(!wired.iter().any(|value| value.as_str() == Some(op)));
        assert!(not_wired
            .iter()
            .any(|entry| entry["op"].as_str() == Some(op)));
    }
}

// ===========================================================================
// channels_prepare：校验 / 幂等 / abort
// ===========================================================================

#[test]
fn channels_prepare_returns_token_diff_and_reports_graph_action() {
    let mut harness = Harness::with_channels();
    harness.publish();
    let response = prepare(
        &mut harness,
        json!({ "channels": 4, "aes67_channels": 4, "aes67_map": [0, 1, 2, 3] }),
    );
    assert_eq!(response["ok"], json!(true), "{response}");
    assert_eq!(response["changed"], json!(true));
    assert_eq!(response["planned_generation"], json!(1));
    assert_eq!(response["token"], response["engine_token"]);
    assert_eq!(response["target"]["channels"], json!(4));
    assert_eq!(response["diff"]["channels"]["from"], json!(2));
    assert_eq!(response["diff"]["channels"]["to"], json!(4));
    assert_eq!(response["graph"]["action"], json!("dropped"));
    assert_eq!(response["admission"]["accepted"], json!(true));
    assert_eq!(response["device"]["probe"]["available"], json!(true));
    let token = token_of(&response);
    assert!(!token.is_empty());

    // 只换 AES67 输入（引擎格式不变）：图保持。
    let kept = prepare(&mut harness, json!({ "aes67_port": 5_006 }));
    assert_eq!(kept["ok"], json!(true));
    assert_eq!(kept["graph"]["action"], json!("kept"));
    assert_eq!(kept["diff"]["aes67_port"]["to"], json!(5_006));

    // 幂等：同一目标重复 prepare 原样返回同一 token/响应。
    let repeated = prepare(&mut harness, json!({ "aes67_port": 5_006 }));
    assert_eq!(token_of(&repeated), token_of(&kept));
    assert_eq!(repeated, kept);

    // status.pending 暴露草稿。
    let status = call(&mut harness, json!({ "op": "channels_status" }));
    assert_eq!(status["pending"]["engine_token"], json!(token_of(&kept)));
    assert_eq!(status["pending"]["planned_generation"], json!(1));
    assert_eq!(status["busy"], json!(false));
}

#[test]
fn channels_prepare_validates_targets_fail_closed() {
    let mut harness = Harness::with_channels();
    harness.publish();

    // 映射长度必须等于 AES67（RTP）通道数。
    let response = prepare(
        &mut harness,
        json!({ "aes67_channels": 3, "aes67_map": [0, 1, 2, 3] }),
    );
    assert_error(&response, host_error::BAD_REQUEST);
    assert!(response["message"]
        .as_str()
        .expect("message")
        .contains("map length"));

    // 输出槽必须 < 输出通道数；映射必须单射。
    let response = prepare(&mut harness, json!({ "aes67_map": [0, 5] }));
    assert_error(&response, host_error::BAD_REQUEST);
    let response = prepare(&mut harness, json!({ "aes67_map": [0, 0] }));
    assert_error(&response, host_error::BAD_REQUEST);

    // PT 必须 96..=127。
    let response = prepare(&mut harness, json!({ "aes67_payload_type": 95 }));
    assert_error(&response, host_error::BAD_REQUEST);
    let response = prepare(&mut harness, json!({ "aes67_payload_type": 128 }));
    assert_error(&response, host_error::BAD_REQUEST);

    // 采样率必须属于支持集合。
    let response = prepare(&mut harness, json!({ "sample_rate": 44_100 }));
    assert_error(&response, host_error::BAD_REQUEST);

    // 通道数受准入限制（plan ABI 输出通道上限 22）。
    let response = prepare(&mut harness, json!({ "channels": 23 }));
    assert_error(&response, host_error::ADMISSION_REJECTED);
    let response = prepare(&mut harness, json!({ "channels": 0 }));
    assert_error(&response, host_error::ADMISSION_REJECTED);

    // 端口 0 / 非组播地址。
    let response = prepare(&mut harness, json!({ "aes67_port": 0 }));
    assert_error(&response, host_error::BAD_REQUEST);
    let response = prepare(&mut harness, json!({ "aes67_group": "10.0.0.1" }));
    assert_error(&response, host_error::BAD_REQUEST);

    // 未知字段（拼错不能静默变成"什么都没改"）/ 空补丁。
    let response = prepare(&mut harness, json!({ "chanels": 4 }));
    assert_error(&response, host_error::BAD_REQUEST);
    let response = prepare(&mut harness, json!({}));
    assert_error(&response, host_error::BAD_REQUEST);

    // 只读探测失败 -> device_unavailable（不先改配置）。
    harness
        .probe()
        .expect("channels probe")
        .set_available(false);
    let response = prepare(&mut harness, json!({ "aes67_port": 5_008 }));
    assert_error(&response, hot_error::DEVICE_UNAVAILABLE);
    harness.probe().expect("channels probe").set_available(true);

    // 校验失败不推进代际、不留草稿。
    let status = call(&mut harness, json!({ "op": "channels_status" }));
    assert_eq!(status["generation"], json!(0));
    assert!(status["pending"].is_null());
}

#[test]
fn channels_prepare_keeps_prepared_draft_until_replaced_and_abort_semantics() {
    let mut harness = Harness::with_channels();
    harness.publish();
    let first = prepare(&mut harness, json!({ "aes67_port": 5_006 }));
    let first_token = token_of(&first);
    // 新 prepare 覆盖旧草稿（与方案 prepare 一致）。
    let second = prepare(&mut harness, json!({ "aes67_port": 5_007 }));
    assert_ne!(token_of(&second), first_token);
    // 用旧 token abort 不行（草稿已换），且草稿不丢。
    let response = call(
        &mut harness,
        json!({ "op": "channels_abort", "token": first_token }),
    );
    assert_error(&response, host_error::ENGINE_TOKEN_INVALID);
    // 正确 token abort。
    let second_token = token_of(&second);
    let response = call(
        &mut harness,
        json!({ "op": "channels_abort", "token": second_token }),
    );
    assert_eq!(response["ok"], json!(true), "{response}");
    assert_eq!(response["aborted"], json!(true));
    // 已经没有草稿：commit/abort 都如实报 no_channels_draft。
    assert_error(
        &commit(&mut harness, &second_token),
        hot_error::NO_CHANNELS_DRAFT,
    );
    assert_error(
        &call(
            &mut harness,
            json!({ "op": "channels_abort", "token": "x" }),
        ),
        hot_error::NO_CHANNELS_DRAFT,
    );
}

// ===========================================================================
// channels_commit：成功路径
// ===========================================================================

#[test]
fn channels_commit_applies_at_block_boundary_keeps_pid_and_audio() {
    let mut harness = Harness::with_channels();
    harness.publish();
    let pid_before = std::process::id();
    harness.run_blocks(2);
    // 用户静音请求要在重配前后原样恢复。
    let mute = call(&mut harness, json!({ "op": "mute", "mute": true }));
    assert_eq!(mute["ok"], json!(true), "{mute}");
    assert!(harness.user_mute_requested());
    let blocks_before = harness.blocks();

    let prepared = prepare(
        &mut harness,
        json!({ "channels": 4, "aes67_channels": 4, "aes67_map": [0, 1, 2, 3] }),
    );
    let token = token_of(&prepared);
    let response = commit(&mut harness, &token);
    assert_eq!(response["ok"], json!(true), "{response}");
    assert_eq!(response["applied"], json!(true));
    assert_eq!(response["changed"], json!(true));
    assert_eq!(response["idempotent"], json!(false));
    assert_eq!(response["rolled_back"], json!(false));
    assert_eq!(response["generation"], json!(1));
    assert_eq!(
        response["pid"],
        json!(pid_before),
        "the process/PID must not change"
    );
    let gap = response["gap_ms"].as_u64().expect("gap_ms");
    assert!(
        gap <= 5_000,
        "measured gap must be a plausible measurement: {gap}"
    );
    assert_eq!(response["format"]["channels"], json!(4));
    assert_eq!(response["format"]["sample_rate"], json!(48_000));
    assert_eq!(response["channels"]["aes67_map"], json!([0, 1, 2, 3]));
    assert_eq!(response["pcm_reconfigured"], json!(true));
    assert_eq!(response["device_restarted"], json!(true));
    assert_eq!(response["engine_rebuilt"], json!(true));
    assert_eq!(response["graph_dropped"], json!(true));
    assert_eq!(response["hw"]["channels"], json!(4));
    // 用户静音在提交后保持（块线程内部临时静音已恢复）。
    assert!(harness.user_mute_requested());
    // 块循环继续（同一进程内）：块计数连续（热重配不重置）、引擎格式已换。
    assert_eq!(
        harness.blocks(),
        blocks_before,
        "hot reconfiguration must keep the block counter continuous"
    );
    harness.run_blocks(2);
    assert_eq!(harness.blocks(), blocks_before + 2);
    assert_eq!(harness.current_format().output_channels, 4);
    // generation/last_gap 进追踪器与 status。
    assert_eq!(harness.generation(), 1);
    let status = call(&mut harness, json!({ "op": "channels_status" }));
    assert_eq!(status["generation"], json!(1));
    assert_eq!(status["current"]["channels"], json!(4));
    assert_eq!(status["last_gap_ms"], json!(gap));
    assert_eq!(status["last_result"]["applied"], json!(true));
    // 全局 status 的 format 跟随新数据面格式。
    let global = call(&mut harness, json!({ "op": "status" }));
    assert_eq!(global["format"]["channels"], json!(4));
    assert_eq!(global["reconfigure"]["generation"], json!(1));
    assert_eq!(global["reconfigure"]["last_gap_ms"], json!(gap));
}

#[test]
fn channels_commit_replays_success_and_rejects_bad_tokens() {
    let mut harness = Harness::with_channels();
    harness.publish();
    let prepared = prepare(&mut harness, json!({ "aes67_port": 5_006 }));
    let token = token_of(&prepared);
    let first = commit(&mut harness, &token);
    assert_eq!(first["ok"], json!(true), "{first}");
    // 同 token 再提交：幂等重放（不重复生效）。
    let replay = commit(&mut harness, &token);
    assert_eq!(replay["ok"], json!(true), "{replay}");
    assert_eq!(replay["generation"], first["generation"]);
    assert_eq!(harness.reconfigure_calls(), 1);
    // 草稿已消费：未知 token 如实报 no_channels_draft（不假装有草稿可提交）。
    assert_error(
        &commit(&mut harness, "not-a-token"),
        hot_error::NO_CHANNELS_DRAFT,
    );
    // token 字段缺失/类型错。
    assert_error(
        &call(&mut harness, json!({ "op": "channels_commit" })),
        host_error::BAD_REQUEST,
    );
    assert_error(
        &call(&mut harness, json!({ "op": "channels_commit", "token": 7 })),
        host_error::BAD_REQUEST,
    );
}

#[test]
fn channels_commit_noop_keeps_generation_and_does_not_touch_the_device() {
    let mut harness = Harness::with_channels();
    harness.publish();
    let prepared = prepare(&mut harness, json!({ "channels": 2 }));
    assert_eq!(prepared["changed"], json!(false));
    let response = commit(&mut harness, &token_of(&prepared));
    assert_eq!(response["ok"], json!(true), "{response}");
    assert_eq!(response["applied"], json!(false));
    assert_eq!(response["changed"], json!(false));
    assert_eq!(response["generation"], json!(0));
    assert_eq!(response["gap_ms"], json!(0));
    assert_eq!(
        harness.reconfigure_calls(),
        0,
        "no-op must not touch the data plane"
    );
}

// ===========================================================================
// channels_commit：失败 / 回滚 / absent
// ===========================================================================

#[test]
fn channels_commit_failure_rolls_back_and_keeps_generation() {
    let mut harness = Harness::with_channels();
    harness.publish();
    let _ = call(&mut harness, json!({ "op": "mute", "mute": false }));
    let blocks_before = harness.blocks();
    let prepared = prepare(&mut harness, json!({ "channels": 4 }));
    let token = token_of(&prepared);
    harness.fail_next_reconfigure(ReconfigureFailure {
        code: "ERR_RECONFIGURE_DEVICE",
        stage: "device_open",
        message: "fake new device could not be opened".to_string(),
        rolled_back: true,
        device_started: true,
        receiver_running: true,
    });
    let response = commit(&mut harness, &token);
    assert_eq!(response["ok"], json!(false), "{response}");
    assert_eq!(response["error"], json!("ERR_RECONFIGURE_DEVICE"));
    assert_eq!(response["stage"], json!("device_open"));
    assert_eq!(response["rolled_back"], json!(true));
    assert_eq!(response["applied"], json!(false));
    assert_eq!(
        response["generation"],
        json!(0),
        "a failed commit must not advance the generation"
    );
    assert!(response["gap_ms"].as_u64().is_some());
    assert!(response["message"]
        .as_str()
        .expect("message")
        .contains("fake new device"));
    // 旧配置保持：块循环继续、格式不变、用户静音保持。
    assert_eq!(harness.current_format().output_channels, 2);
    assert!(
        !harness.user_mute_requested(),
        "the user mute request must be restored"
    );
    harness.run_blocks(1);
    assert_eq!(harness.blocks(), blocks_before + 1);
    let status = call(&mut harness, json!({ "op": "channels_status" }));
    assert_eq!(status["generation"], json!(0));
    assert_eq!(status["current"]["channels"], json!(2));
    assert_eq!(status["busy"], json!(false));
    assert_eq!(
        status["last_result"]["error"],
        json!("ERR_RECONFIGURE_DEVICE")
    );
    // 失败后可以重新 prepare + commit。
    let retry = prepare(&mut harness, json!({ "channels": 4 }));
    let response = commit(&mut harness, &token_of(&retry));
    assert_eq!(response["ok"], json!(true), "{response}");
    assert_eq!(harness.generation(), 1);
}

#[test]
fn channels_commit_failed_rollback_marks_device_absent_fail_closed() {
    let mut harness = Harness::with_channels();
    harness.publish();
    let prepared = prepare(&mut harness, json!({ "channels": 4 }));
    let token = token_of(&prepared);
    harness.fail_next_reconfigure(ReconfigureFailure {
        code: "ERR_RECONFIGURE_ROLLBACK",
        stage: "device_open",
        message: "neither the new nor the previous device could be reopened".to_string(),
        rolled_back: false,
        device_started: false,
        receiver_running: true,
    });
    let response = commit(&mut harness, &token);
    assert_eq!(response["ok"], json!(false), "{response}");
    assert_eq!(response["rolled_back"], json!(false));
    assert_eq!(response["device_started"], json!(false));
    // 块线程如实登记设备缺失；之后提交按 device_absent fail-closed 拒绝。
    assert!(harness.lifecycle_mut().is_absent());
    harness.publish();
    let prepared = prepare(&mut harness, json!({ "aes67_port": 5_009 }));
    let response = commit(&mut harness, &token_of(&prepared));
    assert_eq!(response["ok"], json!(false), "{response}");
    assert_eq!(response["error"], json!("device_absent"));
    assert_eq!(response["rolled_back"], json!(true));
    assert_eq!(harness.generation(), 0);
}

#[test]
fn channels_commit_absent_device_is_rejected_fail_closed() {
    let mut harness = Harness::with_channels();
    harness
        .lifecycle_mut()
        .note_absent(1, "fake device unplugged");
    harness.publish();
    let prepared = prepare(&mut harness, json!({ "channels": 4 }));
    let response = commit(&mut harness, &token_of(&prepared));
    assert_eq!(response["ok"], json!(false), "{response}");
    assert_eq!(response["error"], json!("device_absent"));
    assert_eq!(response["stage"], json!("preflight"));
    assert_eq!(response["rolled_back"], json!(true));
    assert_eq!(
        harness.reconfigure_calls(),
        0,
        "absent device: nothing may be touched"
    );
    // 设备恢复后重新 prepare + commit（absent 拒绝发生在块线程，草稿已消费）。
    harness.lifecycle_mut().note_recovered(2);
    harness.publish();
    let retry = prepare(&mut harness, json!({ "channels": 4 }));
    let response = commit(&mut harness, &token_of(&retry));
    assert_eq!(response["ok"], json!(true), "{response}");
    assert_eq!(harness.generation(), 1);
}

// ===========================================================================
// 与校准模式互斥
// ===========================================================================

#[test]
fn channels_commit_rejected_while_calibration_mode_is_active() {
    let dir = TempDir::new("cal-busy");
    let mut harness = Harness::with_channels_and_calibration(fake_calibration_setup(dir.path()));
    harness.publish();
    let entered = call(&mut harness, json!({ "op": "cal_enter", "audible": true }));
    assert_eq!(entered["ok"], json!(true), "{entered}");
    assert_eq!(entered["mode"], json!("calibration"));
    let prepared = prepare(&mut harness, json!({ "channels": 4 }));
    let token = token_of(&prepared);
    let response = commit(&mut harness, &token);
    assert_error(&response, "cal_busy");
    assert_eq!(harness.generation(), 0);
    // 校准结束后同一草稿仍可提交（拒绝发生在取草稿之前）。
    let left = call(&mut harness, json!({ "op": "cal_leave" }));
    assert_eq!(left["ok"], json!(true), "{left}");
    let response = commit(&mut harness, &token);
    assert_eq!(response["ok"], json!(true), "{response}");
    assert_eq!(harness.generation(), 1);
}

#[test]
fn cal_enter_is_rejected_while_a_reconfiguration_is_in_flight() {
    let dir = TempDir::new("reconfiguring");
    let mut harness = Harness::with_channels_and_calibration(fake_calibration_setup(dir.path()));
    harness.publish();
    let gate = { harness.core().lock().expect("core lock").reconfigure_gate() };
    assert!(gate.enter(), "gate must be free before the test");
    let response = call(&mut harness, json!({ "op": "cal_enter", "audible": true }));
    assert_error(&response, hot_error::RECONFIGURING);
    gate.leave();
    let response = call(&mut harness, json!({ "op": "cal_enter", "audible": true }));
    assert_eq!(response["ok"], json!(true), "{response}");
}

// ===========================================================================
// ack 丢失：channels_busy 与追踪器对账（不重复生效）
// ===========================================================================

fn call_core(core: &mut ControlCore, request: Value) -> Value {
    serde_json::from_str(&core.handle_line(&request.to_string()))
        .unwrap_or_else(|_| panic!("response is not JSON: {request}"))
}

#[test]
fn channels_commit_ack_lost_stays_busy_then_reconciles_instead_of_reapplying() {
    let (command_tx, _command_rx) = mpsc::channel::<ControlCommand>();
    let snapshot: SharedSnapshot = Arc::new(Mutex::new(None));
    let tracker = shared_tracker();
    let mut core = ControlCore::new(ControlCoreConfig {
        data_plane_format: crate::control::test_support::test_config(),
        limits: AdmissionLimits::default(),
        commands: command_tx,
        snapshot,
        mode: khaen_host::control::RuntimeMode::Aes67,
        initial_plan: None,
        initial_version: 0,
        command_timeout: Duration::from_millis(50),
        calibration: None,
        channels: Some(ChannelsConfig {
            config: fake_runtime_config(),
            probe: Arc::new(FakeProbe::new(FakeProbeState::default())),
            input_source: None,
        }),
        reconfigure: Some(Arc::clone(&tracker)),
    });
    let prepared = call_core(
        &mut core,
        json!({ "op": "channels_prepare", "channels": 4 }),
    );
    let token = token_of(&prepared);
    // 块线程不消费命令：ack 超时 -> not_applied，但结果未知（在飞）。
    let response = call_core(
        &mut core,
        json!({ "op": "channels_commit", "token": token }),
    );
    assert_error(&response, host_error::NOT_APPLIED);
    // 再次提交：单活动 -> channels_busy（不重复下发）。
    let busy = call_core(
        &mut core,
        json!({ "op": "channels_commit", "token": token }),
    );
    assert_error(&busy, hot_error::CHANNELS_BUSY);
    let status = call_core(&mut core, json!({ "op": "channels_status" }));
    assert_eq!(status["busy"], json!(true));

    // 模拟块线程稍后执行并记录结果（ack 丢失但已生效）：控制面对账，不重复生效。
    let mut target = fake_runtime_config();
    target.engine.output_channels = 4;
    target.device.output_channels = 4;
    target.input.channels = 4;
    target.input.output_channels = 4;
    target.input.channels_map = vec![0, 1, 2, 3];
    let view = ChannelsView::from_config(&target);
    tracker.lock().expect("tracker").record_success(
        AppliedReconfigure {
            token: token.clone(),
            gap_ms: 12,
            target: view,
            format: target.engine.clone(),
            engine_changed: true,
            device_was_absent: false,
            graph_dropped: true,
        },
        12,
        json!({ "ok": true, "applied": true, "gap_ms": 12 }),
    );
    let status = call_core(&mut core, json!({ "op": "channels_status" }));
    assert_eq!(status["ok"], json!(true));
    assert_eq!(status["busy"], json!(false));
    assert_eq!(status["generation"], json!(1));
    assert_eq!(status["current"]["channels"], json!(4));
    assert_eq!(status["last_gap_ms"], json!(12));
    // 同一 token 再提交：重放已生效结果（idempotent/reconciled），不再下发。
    let replay = call_core(
        &mut core,
        json!({ "op": "channels_commit", "token": token }),
    );
    assert_eq!(replay["ok"], json!(true), "{replay}");
    assert_eq!(replay["reconciled"], json!(true));
    assert_eq!(replay["idempotent"], json!(true));
    assert_eq!(replay["generation"], json!(1));
}

// ===========================================================================
// 回读类型（避免 unused import；HwReadback 也由 host 层测试覆盖）
// ===========================================================================

#[test]
fn prepared_response_reports_probe_readback() {
    let mut harness = Harness::with_channels();
    harness.publish();
    let probe = harness.probe().expect("channels probe");
    probe.set_readback(48_000, 2);
    let response = prepare(&mut harness, json!({ "aes67_port": 5_011 }));
    assert_eq!(
        response["device"]["probe"],
        json!({ "available": true, "sample_rate": 48_000, "channels": 2 })
    );
    let _: HwReadback = HwReadback {
        sample_rate: Some(48_000),
        channels: Some(2),
    };
}
