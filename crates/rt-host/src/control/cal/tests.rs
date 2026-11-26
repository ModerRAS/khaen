//! T70f-c 校准模式测试（Windows 本机必须全跑；真机路径只做 Linux cfg 编译/探针测试）。
//!
//! 覆盖点：
//! - 配置/环境解析（默认值来自旧 cal_control/cal_umik 常量、mic 声明、非法值点名）；
//! - 协议/模式状态机（未进入 → not_in_calibration、audible 授权、幂等 enter、leave）；
//! - 设备 absent / mic 声明缺失 / UMIK probe 失败回滚；
//! - fake 引擎 enter → measure → remeasure(delta) → solve → leave 全流程（无真机）；
//! - 设备 absent 时测量被拒（不产生假测量）；并发 measure 拒绝（cal_busy）；
//! - 测量失败如实带 stage code + error.json，不写假数据；
//! - Runtime 校准播放 seam（扫频块/收尾静音/安全静音/进度/图不被改）；
//! - 控制命令通道 CalPlaybackBegin/End 的 ack；
//! - 非 Linux 的 auto 引擎如实降级 fake（simulated=true）；Linux 真机引擎工厂编译/探针。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use khaen_calibration::capture::UmikDeviceConfig;
use khaen_calibration::stage_backend::FakeEngineConfig;
#[cfg(target_os = "linux")]
use khaen_calibration::stage_backend::StageDeviceConfig;
use khaen_host::runtime::{CalibrationPlayback, CalibrationProgress};
use serde_json::json;

#[cfg(target_os = "linux")]
use crate::control::cal::engine::CalProgress;
use crate::control::cal::engine::{build_factory, CalEngineFactory, FakeCalFactory};
use crate::control::cal::{
    error, CalEngineKind, CalibrationConfig, CalibrationSetup, DEFAULT_CHANNELS,
};
use crate::control::command::{execute_command_isolated, CommandReply, ControlCommand};
use crate::control::test_support::{fake_runtime, Harness};

// ===========================================================================
// 夹具
// ===========================================================================

/// 临时产物目录（Drop 删除；文件名带进程号 + 原子计数，测试并行安全）。
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "khaen-rt-host-cal-{}-{tag}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp calibration dir");
        TempDir { path }
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

/// 合法的 64 hex mic 校准文件 sha256（声明只用，不读文件）。
fn fake_cal_sha() -> String {
    "ab".repeat(32)
}

/// 离线合成引擎的校准配置（2 通道运行时；mic 声明齐全）。
fn fake_config(dir: &Path) -> CalibrationConfig {
    CalibrationConfig {
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
        level_dbfs: super::DEFAULT_LEVEL_DBFS,
        engine: CalEngineKind::Fake,
    }
}

fn fake_harness(dir: &Path) -> Harness {
    Harness::with_calibration(CalibrationSetup {
        config: fake_config(dir),
        factory: Box::new(FakeCalFactory::new()),
    })
}

fn fake_harness_with(dir: &Path, factory: Box<dyn CalEngineFactory>) -> Harness {
    Harness::with_calibration(CalibrationSetup {
        config: fake_config(dir),
        factory,
    })
}

fn assert_err(value: &serde_json::Value, code: &str) {
    assert_eq!(value["ok"], json!(false), "expected error {code}: {value}");
    assert_eq!(value["error"], json!(code), "unexpected error: {value}");
}

fn call_cal_enter(harness: &mut Harness, audible: bool) -> serde_json::Value {
    harness.call_json(&json!({ "op": "cal_enter", "audible": audible }))
}

fn publish_snapshot(harness: &mut Harness) {
    harness.publish();
}

/// 轮询 cal_status 直到测量不再 running（测试用；超时 panic）。
fn wait_until_measure_finished(harness: &mut Harness, timeout: Duration) -> serde_json::Value {
    let deadline = Instant::now() + timeout;
    loop {
        let status = harness.call_json(&json!({ "op": "cal_status" }));
        let state = status["measure"]["state"]
            .as_str()
            .unwrap_or("")
            .to_string();
        if state != "measuring" {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "measurement did not finish within {timeout:?}: {status}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ===========================================================================
// 配置/环境解析
// ===========================================================================

#[test]
fn config_defaults_come_from_legacy_constants() {
    let config = CalibrationConfig::parse(
        None, None, None, None, None, None, None, None, None, None, None, None,
    )
    .expect("parse defaults")
    .expect("calibration enabled by default");
    // 旧 cal_control status: layout 4.1.2 / channels [0,1,2,3,4,5,7] / level -12。
    assert_eq!(config.layout, super::DEFAULT_LAYOUT);
    assert_eq!(config.channels, DEFAULT_CHANNELS.to_vec());
    assert_eq!(config.level_dbfs, super::DEFAULT_LEVEL_DBFS);
    assert_eq!(config.umik_device, super::DEFAULT_UMIK_DEVICE);
    assert_eq!(config.capture_channels, 2);
    assert_eq!(config.capture_channel, Some(0));
    assert_eq!(config.engine, CalEngineKind::Auto);
    // 没有声明时不能猜 mic：mic=None + 原因点名环境变量。
    assert!(config.mic.is_none());
    let reason = config.mic_missing_reason.expect("reason");
    for env in [
        super::CAL_UMIK_SERIAL_ENV,
        super::CAL_UMIK_CAL_FILE_ENV,
        super::CAL_UMIK_CAL_SHA_ENV,
    ] {
        assert!(reason.contains(env), "missing {env} in reason: {reason}");
    }
}

#[test]
fn config_parse_off_and_invalid_values_name_the_variable() {
    let off = CalibrationConfig::parse(
        Some("off".to_string()),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .expect("off is valid");
    assert!(off.is_none());
    for value in ["maybe", "2"] {
        let error = CalibrationConfig::parse(
            Some(value.to_string()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect_err("invalid switch");
        assert!(error.message().contains(super::CAL_ENV), "{error}");
    }
    let bad_engine = CalibrationConfig::parse(
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some("turbo".to_string()),
    )
    .expect_err("invalid engine");
    assert!(bad_engine.message().contains(super::CAL_ENGINE_ENV));
    let bad_channels = CalibrationConfig::parse(
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some("0,x".to_string()),
        None,
        None,
        None,
    )
    .expect_err("invalid channels");
    assert!(bad_channels.message().contains(super::CAL_CHANNELS_ENV));
    let bad_level = CalibrationConfig::parse(
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some("loud".to_string()),
        None,
    )
    .expect_err("invalid level");
    assert!(bad_level.message().contains(super::CAL_LEVEL_ENV));
}

#[test]
fn config_parse_mic_declaration_checks_sha_when_file_exists() {
    let dir = TempDir::new("mic");
    let cal_file = dir.path().join("umik.txt");
    std::fs::write(&cal_file, b"calibration data").expect("write cal file");
    let sha = khaen_host::sha256_hex(b"calibration data");
    let declared = CalibrationConfig::parse(
        None,
        Some(dir.path().display().to_string()),
        None,
        Some("718-7696".to_string()),
        Some(cal_file.display().to_string()),
        Some(sha.clone()),
        Some("90deg".to_string()),
        None,
        None,
        None,
        None,
        Some("fake".to_string()),
    )
    .expect("parse declaration")
    .expect("enabled");
    assert!(declared.mic.is_some(), "declaration must be accepted");
    assert!(declared.mic_missing_reason.is_none());

    let mismatch = CalibrationConfig::parse(
        None,
        None,
        None,
        Some("718-7696".to_string()),
        Some(cal_file.display().to_string()),
        Some("cd".repeat(32)),
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .expect("parse mismatch")
    .expect("enabled");
    assert!(mismatch.mic.is_none());
    assert!(mismatch
        .mic_missing_reason
        .expect("reason")
        .contains("sha256 mismatch"));
}

// ===========================================================================
// 协议/模式状态机
// ===========================================================================

#[test]
fn enter_requires_audible_and_reports_calibration_mode_and_wiring() {
    let dir = TempDir::new("enter");
    let mut harness = fake_harness(dir.path());
    publish_snapshot(&mut harness);

    assert_err(
        &call_cal_enter(&mut harness, false),
        error::AUDIBLE_AUTH_REQUIRED,
    );
    let status = harness.call_json(&json!({ "op": "cal_status" }));
    assert_eq!(status["mode"], json!("playback"));
    assert_eq!(status["umik"]["open"], json!(false));

    let entered = call_cal_enter(&mut harness, true);
    assert_eq!(entered["ok"], json!(true), "{entered}");
    assert_eq!(entered["mode"], json!("calibration"));
    assert_eq!(entered["capture_source"], json!("cal"));
    assert_eq!(entered["program_input_reaches_dsp"], json!(false));
    assert_eq!(entered["umik"]["open"], json!(true));
    assert_eq!(entered["umik"]["declared"], json!(true));
    assert_eq!(entered["simulated"], json!(true));
    assert_eq!(entered["real_capture"], json!(false));

    // 幂等：再次 enter 不重复打开，如实说 already。
    let again = call_cal_enter(&mut harness, true);
    assert_eq!(again["ok"], json!(true));
    assert_eq!(again["entered"], json!(false));

    // 能力上报：进入校准后 wired 才含 cal_*。
    let global = harness.call_json(&json!({ "op": "status" }));
    let wired = global["wired"].as_array().expect("wired");
    for op in super::CAL_OPS {
        assert!(
            wired.iter().any(|value| value.as_str() == Some(op)),
            "cal op {op} must be wired after cal_enter: {global}"
        );
    }
    assert_eq!(global["calibration"]["mode"], json!("calibration"));

    let left = harness.call_json(&json!({ "op": "cal_leave" }));
    assert_eq!(left["ok"], json!(true), "{left}");
    assert_eq!(left["left"], json!(true));
    assert_eq!(left["mode"], json!("playback"));
    assert_eq!(left["playback_restored"], json!(true));
    assert_eq!(left["umik"]["open"], json!(false));
}

#[test]
fn measure_before_enter_is_rejected_with_not_in_calibration() {
    let dir = TempDir::new("not-in-cal");
    let mut harness = fake_harness(dir.path());
    publish_snapshot(&mut harness);
    let response = harness.call_json(&json!({ "op": "cal_measure", "audible": true }));
    assert_err(&response, error::NOT_IN_CALIBRATION);
    let solve = harness.call_json(&json!({ "op": "cal_solve" }));
    assert_err(&solve, error::NO_MEASUREMENT);
}

#[test]
fn enter_rejects_absent_device_and_missing_mic_declaration() {
    let dir = TempDir::new("enter-guards");
    let mut harness = fake_harness(dir.path());
    publish_snapshot(&mut harness);

    // 设备 absent：如实 device_absent，模式保持 playback。
    harness
        .lifecycle_mut()
        .note_absent(1_000, "ALSA device unplugged");
    publish_snapshot(&mut harness);
    let absent = call_cal_enter(&mut harness, true);
    assert_err(&absent, error::DEVICE_ABSENT);
    assert_eq!(
        harness.call_json(&json!({ "op": "cal_status" }))["mode"],
        json!("playback")
    );
    harness.lifecycle_mut().note_recovered(2_000);

    // mic 声明缺失：拒绝并点名原因（不猜设备/不伪造声明）。
    let mut config = fake_config(dir.path());
    config.mic = None;
    config.mic_missing_reason = Some("UMIK declaration missing: set KHAEN_UMIK_SERIAL".to_string());
    let mut harness = Harness::with_calibration(CalibrationSetup {
        config,
        factory: Box::new(FakeCalFactory::new()),
    });
    publish_snapshot(&mut harness);
    let missing = call_cal_enter(&mut harness, true);
    assert_err(&missing, error::MIC_DECLARATION_MISSING);
    assert!(missing["message"]
        .as_str()
        .expect("message")
        .contains("KHAEN_UMIK_SERIAL"));
}

#[test]
fn enter_rolls_back_to_playback_when_umik_probe_fails() {
    let dir = TempDir::new("probe-fail");
    let factory_config = FakeEngineConfig {
        force_unavailable: true,
        ..FakeEngineConfig::default()
    };
    let mut harness = fake_harness_with(
        dir.path(),
        Box::new(FakeCalFactory::with_config(factory_config)),
    );
    publish_snapshot(&mut harness);
    let response = call_cal_enter(&mut harness, true);
    assert_err(&response, error::DEVICE_ABSENT);
    let status = harness.call_json(&json!({ "op": "cal_status" }));
    assert_eq!(status["mode"], json!("playback"));
    assert_eq!(status["umik"]["open"], json!(false));
    // 回滚后还能正常进入（引擎没被留在半开状态）。
    let mut harness = fake_harness(dir.path());
    publish_snapshot(&mut harness);
    assert_eq!(
        call_cal_enter(&mut harness, true)["mode"],
        json!("calibration")
    );
}

// ===========================================================================
// 全流程（fake 引擎；无真机）
// ===========================================================================

#[test]
fn fake_measure_remeasure_solve_and_leave_flow() {
    let dir = TempDir::new("flow");
    let mut harness = fake_harness(dir.path());
    publish_snapshot(&mut harness);
    // 只测 1 个通道（enter 固定会话几何）：时间轴更短，链路与多通道完全一致。
    let entered = harness.call_json(&json!({
        "op": "cal_enter",
        "audible": true,
        "channels": [0],
        "layout": "mono-1.0",
    }));
    assert_eq!(entered["mode"], json!("calibration"), "{entered}");
    let measure = harness.call_json(&json!({
        "op": "cal_measure",
        "audible": true,
        "wait_ms": 300_000,
    }));
    assert_eq!(measure["ok"], json!(true), "measure failed: {measure}");
    assert_eq!(measure["state"], json!("done"));
    assert_eq!(measure["simulated"], json!(true));
    assert_eq!(measure["verdict"], json!("SIMULATED_MEASURED"));
    let quality = &measure["quality"];
    let snr = quality["min_snr_db"].as_f64().expect("min_snr_db");
    assert!(
        snr > 25.0,
        "fake engine SNR must clear the 25 dB gate: {snr}"
    );
    assert_eq!(quality["clipped_samples"], json!(0));
    assert_eq!(quality["all_onsets_valid"], json!(true));
    // UMIK 独立 USB 时钟：clock ppm + 不确定度必须有来源（T62 时基），不编数。
    assert!(quality["clock_ppm"].is_number(), "clock_ppm: {quality}");
    assert!(
        quality["clock_ppm_uncertainty_ppm"].is_number(),
        "clock_ppm_uncertainty_ppm: {quality}"
    );

    // 产物：WAV / raw / 报告 / IR + sha256。
    let artifacts = measure["artifacts"].as_array().expect("artifacts");
    for role in [
        "capture_raw",
        "capture_wav",
        "timeline",
        "capture_report",
        "summary",
        "ir",
    ] {
        assert!(
            artifacts
                .iter()
                .any(|artifact| artifact["role"] == json!(role)),
            "missing artifact role {role}: {measure}"
        );
    }
    for artifact in artifacts {
        let path = artifact["path"].as_str().expect("artifact path");
        let bytes = std::fs::read(path).expect("artifact file exists");
        assert_eq!(artifact["bytes"], json!(bytes.len() as u64));
        assert_eq!(
            artifact["sha256"],
            json!(khaen_host::sha256_hex(&bytes)),
            "sha256 mismatch for {path}"
        );
    }
    let irs = measure["ir"].as_array().expect("ir list");
    assert_eq!(irs.len(), 1, "one channel -> one IR: {measure}");
    assert!(irs[0]["samples"].as_u64().expect("ir samples") > 100);

    // cal_status 摘要：最近测量/求解状态。
    let status = harness.call_json(&json!({ "op": "cal_status" }));
    assert_eq!(status["measure"]["state"], json!("done"));
    assert_eq!(status["solve"]["available"], json!(true));
    let measure_id = status["measure"]["measure_id"]
        .as_u64()
        .expect("measure id");

    // remeasure：需要前一次结果，报告 delta。
    let remeasure = harness.call_json(&json!({
        "op": "cal_remeasure",
        "audible": true,
        "wait_ms": 300_000,
    }));
    assert_eq!(
        remeasure["ok"],
        json!(true),
        "remeasure failed: {remeasure}"
    );
    assert_eq!(remeasure["state"], json!("done"));
    assert_eq!(remeasure["pass"], json!(2));
    let delta = &remeasure["delta_vs_previous"];
    assert_eq!(delta["previous_measure_id"], json!(measure_id));
    assert!(
        delta["snr_db_delta"].is_number(),
        "delta must be reported for remeasure: {remeasure}"
    );

    // solve：产出 design JSON + 建议 plan（写 artifact，不提交、不改运行图）。
    let solve = harness.call_json(&json!({ "op": "cal_solve" }));
    assert_eq!(solve["ok"], json!(true), "solve failed: {solve}");
    assert_eq!(solve["applied"], json!(false));
    assert_eq!(solve["verdict"], json!("PREDICTED_ONLY"));
    let design = &solve["design"];
    assert_eq!(design["schema"], json!(super::solve::DESIGN_SCHEMA));
    assert_eq!(design["applied"], json!(false));
    assert_eq!(design["admission"]["accepted"], json!(true));
    assert_eq!(design["graph_compile"], json!("ok"));
    let plan_path = design["plan"]["path"].as_str().expect("plan path");
    let plan_bytes = std::fs::read(plan_path).expect("plan file exists");
    assert_eq!(design["plan"]["bytes"], json!(plan_bytes.len() as u64));
    assert_eq!(
        design["plan"]["sha256"],
        json!(khaen_host::sha256_hex(&plan_bytes))
    );
    // 运行图未被 solve 改动。
    let graph = harness.graph_info();
    assert!(
        graph.version.is_none(),
        "solve must not commit a plan: {graph:?}"
    );

    // leave：回播放模式，测量结果仍可读（不丢证据）。
    let left = harness.call_json(&json!({ "op": "cal_leave" }));
    assert_eq!(left["mode"], json!("playback"));
    let status = harness.call_json(&json!({ "op": "cal_status" }));
    assert_eq!(status["measure"]["state"], json!("done"));
    assert_eq!(status["solve"]["available"], json!(true));
}

#[test]
fn measure_concurrent_start_is_rejected_with_cal_busy() {
    let dir = TempDir::new("busy");
    let mut harness = fake_harness(dir.path());
    publish_snapshot(&mut harness);
    call_cal_enter(&mut harness, true);

    let started = harness.call_json(&json!({
        "op": "cal_measure",
        "audible": true,
        "wait_ms": 0,
    }));
    assert_eq!(started["ok"], json!(true), "{started}");
    assert_eq!(started["state"], json!("measuring"));
    let second = harness.call_json(&json!({
        "op": "cal_measure",
        "audible": true,
        "wait_ms": 0,
    }));
    assert_err(&second, error::BUSY);

    let status = wait_until_measure_finished(&mut harness, Duration::from_secs(300));
    assert_eq!(status["measure"]["state"], json!("done"), "{status}");
    // 结束后可以再测（job 已被回收）。
    let again = harness.call_json(&json!({
        "op": "cal_measure",
        "audible": true,
        "wait_ms": 300_000,
    }));
    assert_eq!(again["ok"], json!(true), "{again}");
    assert_eq!(again["state"], json!("done"));
}

#[test]
fn measure_with_device_absent_is_rejected_without_fake_data() {
    let dir = TempDir::new("absent-measure");
    let mut harness = fake_harness(dir.path());
    publish_snapshot(&mut harness);
    call_cal_enter(&mut harness, true);
    harness
        .lifecycle_mut()
        .note_absent(5_000, "device disappeared after enter");
    publish_snapshot(&mut harness);

    let response = harness.call_json(&json!({
        "op": "cal_measure",
        "audible": true,
    }));
    assert_err(&response, error::DEVICE_ABSENT);
    let status = harness.call_json(&json!({ "op": "cal_status" }));
    assert_eq!(status["measure"]["state"], json!("idle"));
    assert_eq!(status["measure"]["result"], serde_json::Value::Null);
    assert_eq!(status["solve"]["available"], json!(false));
}

#[test]
fn measure_failure_reports_stage_code_and_writes_error_artifact_only() {
    let dir = TempDir::new("measure-fail");
    let factory_config = FakeEngineConfig {
        force_timeout: true,
        ..FakeEngineConfig::default()
    };
    let mut harness = fake_harness_with(
        dir.path(),
        Box::new(FakeCalFactory::with_config(factory_config)),
    );
    publish_snapshot(&mut harness);
    call_cal_enter(&mut harness, true);
    let response = harness.call_json(&json!({
        "op": "cal_measure",
        "audible": true,
        "wait_ms": 300_000,
    }));
    assert_err(&response, error::MEASURE_FAILED);
    assert!(
        response["message"]
            .as_str()
            .expect("message")
            .contains("E_STAGE_CAPTURE_TIMEOUT"),
        "failure must carry the exact stage code: {response}"
    );
    let status = harness.call_json(&json!({ "op": "cal_status" }));
    assert_eq!(status["measure"]["state"], json!("failed"));
    let failure = &status["measure"]["error"];
    assert_eq!(failure["stage_code"], json!("E_STAGE_CAPTURE_TIMEOUT"));
    assert_eq!(status["solve"]["available"], json!(false));
    // 失败会话目录只有 error.json，没有假 WAV/IR。
    let session = status["measure"]["session_id"]
        .as_str()
        .expect("session id");
    let session_dir = dir.path().join(session);
    assert!(
        session_dir.join("error.json").is_file(),
        "error artifact missing in {}",
        session_dir.display()
    );
    assert!(!session_dir.join("capture.wav").exists());
}

// ===========================================================================
// Runtime 校准播放 seam（host::runtime；Windows 可跑）
// ===========================================================================

#[test]
fn runtime_calibration_playback_writes_sweep_then_silence_and_keeps_graph() {
    let mut runtime = fake_runtime();
    let block_frames = runtime.config().engine.block_frames;
    let channels = runtime.config().engine.channels;
    let block_samples = block_frames * channels;
    let progress = Arc::new(CalibrationProgress::new(2));
    let mut data = vec![0.0f32; block_samples * 2];
    data[..block_samples].fill(0.5);
    data[block_samples..].fill(-0.25);
    let playback = CalibrationPlayback::new(data, block_frames, channels, Arc::clone(&progress))
        .expect("playback buffer");
    assert_eq!(
        runtime.begin_calibration_playback(playback).expect("begin"),
        2
    );
    assert!(runtime.calibration_playback_active());
    let graph_before = runtime.status().graph.clone();
    let blocks_before = runtime.blocks();

    let first = runtime
        .run_calibration_block()
        .expect("first block")
        .to_vec();
    assert_eq!(first.len(), block_samples);
    assert!(first.iter().all(|sample| (*sample - 0.5).abs() < 1e-6));
    assert_eq!(runtime.calibration_playback_progress(), Some((1, 2)));
    let second = runtime
        .run_calibration_block()
        .expect("second block")
        .to_vec();
    assert!(second.iter().all(|sample| (*sample + 0.25).abs() < 1e-6));
    assert!(progress.finished(), "progress must report finished");
    let tail = runtime
        .run_calibration_block()
        .expect("tail block")
        .to_vec();
    assert!(
        tail.iter().all(|sample| *sample == 0.0),
        "after the sweep the seam writes silence"
    );
    assert_eq!(runtime.calibration_playback_progress(), Some((2, 2)));
    // 图没有被校准播放推进/改动。
    assert_eq!(runtime.status().graph.version, graph_before.version);
    assert_eq!(runtime.status().graph.epoch, graph_before.epoch);
    assert!(runtime.blocks() > blocks_before);
    assert!(runtime.end_calibration_playback());
    assert!(!runtime.calibration_playback_active());
    // 结束后回到节目块路径（未装载方案 -> 既有 unloaded 策略，不影响本测试目标）。
    runtime
        .run_block()
        .expect("program block after calibration");

    // 安全静音：校准播放也受安全状态机约束（整块清零）。
    let progress = Arc::new(CalibrationProgress::new(1));
    let playback = CalibrationPlayback::new(
        vec![0.5f32; block_samples],
        block_frames,
        channels,
        progress,
    )
    .expect("playback buffer");
    runtime.begin_calibration_playback(playback).expect("begin");
    runtime.request_mute(true);
    let muted = runtime
        .run_calibration_block()
        .expect("muted block")
        .to_vec();
    assert!(
        muted.iter().all(|sample| *sample == 0.0),
        "safety mute must zero the calibration playback"
    );
    runtime.request_mute(false);
    assert!(runtime.end_calibration_playback());
}

#[test]
fn calibration_playback_commands_are_acked_by_runtime_target() {
    let mut runtime = fake_runtime();
    let block_frames = runtime.config().engine.block_frames;
    let channels = runtime.config().engine.channels;
    let progress = Arc::new(CalibrationProgress::new(1));
    let playback = CalibrationPlayback::new(
        vec![0.0f32; block_frames * channels],
        block_frames,
        channels,
        progress,
    )
    .expect("playback buffer");
    let (ack, replies) = mpsc::channel();
    execute_command_isolated(
        ControlCommand::CalPlaybackBegin { playback, ack },
        &mut runtime,
    );
    match replies
        .recv_timeout(Duration::from_secs(2))
        .expect("begin ack")
    {
        CommandReply::CalPlaybackStarted { total_blocks } => assert_eq!(total_blocks, 1),
        other => panic!("unexpected begin reply: {other:?}"),
    }
    let (ack, replies) = mpsc::channel();
    execute_command_isolated(ControlCommand::CalPlaybackEnd { ack }, &mut runtime);
    match replies
        .recv_timeout(Duration::from_secs(2))
        .expect("end ack")
    {
        CommandReply::CalPlaybackEnded { was_active } => assert!(was_active),
        other => panic!("unexpected end reply: {other:?}"),
    }
}

// ===========================================================================
// 引擎工厂平台语义
// ===========================================================================

#[test]
fn auto_factory_is_honest_about_platform_capture_support() {
    let dir = TempDir::new("factory");
    let mut config = fake_config(dir.path());
    config.engine = CalEngineKind::Auto;
    let (commands, _receiver) = mpsc::channel();
    let factory = build_factory(&config, commands);
    #[cfg(target_os = "linux")]
    {
        assert_eq!(factory.kind(), CalEngineKind::InProcess);
        assert!(!factory.simulated());
        assert_eq!(factory.engine_name(), "rt-host-in-process-alsa");
    }
    #[cfg(not(target_os = "linux"))]
    {
        // 非 Linux 没有真实采集路径：auto 如实降级为 fake 并标 simulated。
        assert_eq!(factory.kind(), CalEngineKind::Fake);
        assert!(factory.simulated());
        assert_eq!(factory.engine_name(), "fake-synth");
    }
}

/// Linux 真机引擎工厂：编译/探针路径（本机无 UMIK 时 probe 如实 DEVICE_UNAVAILABLE；
/// 有设备时 accept 也合法）。真实采集测量不在 Windows 本机跑（CI/板端覆盖）。
#[cfg(target_os = "linux")]
#[test]
fn in_process_engine_factory_probe_path_compiles() {
    let (commands, _receiver) = mpsc::channel();
    let factory = super::engine::InProcessCalFactory::new(commands);
    assert_eq!(factory.engine_name(), "rt-host-in-process-alsa");
    assert!(!factory.simulated());
    let stage = super::StageBackendConfig {
        devices: StageDeviceConfig::default(),
        outputs: vec![khaen_calibration::sweep::SweepOutput::new(0, "C0", 0)],
        ..super::StageBackendConfig::default()
    };
    let progress = Arc::new(CalProgress::new());
    let mut engine = factory.create(&stage, progress).expect("create engine");
    assert_eq!(engine.name(), "rt-host-in-process-alsa");
    match engine.probe(&stage.devices) {
        Ok(probe) => assert!(probe.capture_ok),
        Err(error) => assert!(
            !error.code.is_empty() && !error.message.is_empty(),
            "probe failure must be a stable StageError: {error}"
        ),
    }
}

/// Windows 本机：GSD 缺少设备时也不允许假测量（阴性用例已由 fake force_* 覆盖）。
#[cfg(not(target_os = "linux"))]
#[test]
fn non_linux_has_no_fake_real_capture_path() {
    let dir = TempDir::new("non-linux");
    let config = fake_config(dir.path());
    let (commands, _receiver) = mpsc::channel();
    let factory = build_factory(&config, commands);
    assert_eq!(factory.kind(), CalEngineKind::Fake);
    assert!(factory.simulated());
}
