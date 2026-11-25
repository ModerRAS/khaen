// T67 前置：真实 stage backend 单测（离线 fake 抽象；无板端/无声卡）。
//
// 注册方式：crates/calibration/src/stage_backend.rs 末尾
//   #[cfg(test)] mod t67_tests { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/T67/stage_backend.rs")); }
// 运行：cargo test -p khaen-calibration stage_backend

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::capture::UmikDeviceConfig;
use crate::job::{
    ExitDisposition, JobKind, JobManager, JobManagerConfig, JobRequest, JobStage, JobState,
    SafeOutputController, SafetyError,
};
use crate::sweep::SweepOutput;

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_path(tag: &str) -> std::path::PathBuf {
    let base = std::env::temp_dir();
    let id = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
    base.join(format!("tspi-t67-{tag}-{}-{id}", std::process::id()))
}

fn hex64() -> String {
    "0123456789abcdef".repeat(4)
}

fn mic_declaration(device: &str) -> UmikDeviceConfig {
    UmikDeviceConfig::umik1(device, "718-7696", "umik-90deg.txt", hex64(), "90deg")
}

fn acoustic_config() -> StageBackendConfig {
    let mut config = StageBackendConfig::default();
    config.mode = StageMeasurementMode::AcousticUmik;
    config.engine = EngineKind::Fake;
    config.mic = Some(mic_declaration("hw:3,0"));
    config.outputs = vec![SweepOutput::new(0, "FL", 0)];
    config.sweep = StageSweepConfig::smoke();
    config
}

fn loopback_config() -> StageBackendConfig {
    let mut config = StageBackendConfig::default();
    config.mode = StageMeasurementMode::ElectricalLoopback;
    config.engine = EngineKind::Fake;
    config.mic = None;
    config.devices.capture_device = "hw:4,0".to_string();
    config.devices.capture_channels = 20;
    config.devices.capture_channel = Some(6);
    config.outputs = vec![SweepOutput::new(0, "OUT9", 8)];
    config.sweep = StageSweepConfig::smoke();
    config
}

fn full_request(level_dbfs: f32) -> JobRequest {
    JobRequest {
        kind: JobKind::Full,
        label: "t67-stage-backend".to_string(),
        layout: "4.1.2".to_string(),
        sample_rate: 48_000,
        design_points: 6,
        holdout_points: 3,
        repeats: 3,
        drive_level_dbfs: level_dbfs,
        audible_sweep_authorized: true,
        auto_apply: false,
        expected_applied_version: None,
        inputs: Vec::new(),
    }
}

struct FakeSafety {
    version: String,
}

impl FakeSafety {
    fn new() -> Self {
        FakeSafety {
            version: "last-good-v1".to_string(),
        }
    }
}

impl SafeOutputController for FakeSafety {
    fn acquire_calibration_owner(&self) -> Result<(), SafetyError> {
        Ok(())
    }

    fn release_calibration_owner(&self, _disposition: ExitDisposition) -> Result<(), SafetyError> {
        Ok(())
    }

    fn restore_last_good(&self) -> Result<String, SafetyError> {
        Ok(self.version.clone())
    }

    fn activate_safe_mute(&self) -> Result<(), SafetyError> {
        Ok(())
    }

    fn last_good_version(&self) -> Option<String> {
        Some(self.version.clone())
    }
}

fn fast_clean_engine() -> FakeEngine {
    FakeEngine::new(FakeEngineConfig {
        preroll_frames: 2_400,
        tail_frames: 2_400,
        gain_db: 0.0,
        noise_dbfs: -90.0,
        ..FakeEngineConfig::default()
    })
}

// ---------------------------------------------------------------------------
// 默认值 / 采样率 / 电平 / 配置校验
// ---------------------------------------------------------------------------

#[test]
fn t67_default_level_is_minus_50_and_hot_levels_need_authorization() {
    assert_eq!(STAGE_DEFAULT_DRIVE_LEVEL_DBFS, -50.0);
    let level = StageLevelConfig::default();
    assert_eq!(level.default_level(), -50.0);
    assert_eq!(level.resolve(-50.0).unwrap(), -50.0);
    assert_eq!(level.resolve(-60.0).unwrap(), -60.0);
    // 更大的音量（> -30 dBFS）默认拒绝。
    let failure = level.resolve(-20.0).unwrap_err();
    assert_eq!(failure.code, E_LEVEL_NOT_AUTHORIZED);
    // 显式授权后允许（仍受 -1 dBFS 上限约束）。
    let mut authorized = level;
    authorized.hot_level_authorized = true;
    assert_eq!(authorized.resolve(-20.0).unwrap(), -20.0);
    assert_eq!(authorized.resolve(-1.0).unwrap(), -1.0);
    assert_eq!(authorized.resolve(0.0).unwrap_err().code, E_LEVEL_UNSAFE);
    assert_eq!(authorized.resolve(-70.0).unwrap_err().code, E_LEVEL_UNSAFE);
    assert_eq!(
        authorized.resolve(f64::NAN).unwrap_err().code,
        E_LEVEL_UNSAFE
    );
}

#[test]
fn t67_rate_rules_forbid_192k_and_keep_umik_to_48k() {
    let acoustic = acoustic_config();
    assert_eq!(
        acoustic.validate_rate(192_000).unwrap_err().code,
        E_RATE_192K_FORBIDDEN
    );
    assert_eq!(
        acoustic.validate_rate(96_000).unwrap_err().code,
        E_RATE_UMIK_48K_ONLY
    );
    assert_eq!(
        acoustic.validate_rate(44_100).unwrap_err().code,
        E_RATE_UMIK_48K_ONLY
    );
    assert!(acoustic.validate_rate(48_000).is_ok());

    let loopback = loopback_config();
    assert_eq!(
        loopback.validate_rate(192_000).unwrap_err().code,
        E_RATE_192K_FORBIDDEN
    );
    assert!(loopback.validate_rate(96_000).is_ok());
    assert!(loopback.validate_rate(48_000).is_ok());
    assert_eq!(
        loopback.validate_rate(44_100).unwrap_err().code,
        E_RATE_UNSUPPORTED
    );
}

#[test]
fn t67_config_validation_rejects_bad_geometry_and_mic_declaration() {
    let mut config = acoustic_config();
    config.outputs = Vec::new();
    assert_eq!(config.validate().unwrap_err().code, E_CONFIG_INVALID);

    let mut config = acoustic_config();
    config.outputs = vec![SweepOutput::new(0, "FL", 0), SweepOutput::new(1, "FR", 0)];
    assert_eq!(config.validate().unwrap_err().code, E_CONFIG_INVALID);

    let mut config = acoustic_config();
    config.outputs = vec![SweepOutput::new(0, "FL", 22)];
    assert_eq!(config.validate().unwrap_err().code, E_CONFIG_INVALID);

    let mut config = acoustic_config();
    config.mic = None;
    assert_eq!(config.validate().unwrap_err().code, E_MIC_DECLARATION);

    // 声学 UMIK 1ch：capture_channels != 1 拒绝。
    let mut config = acoustic_config();
    config.devices.capture_channels = 2;
    assert_eq!(config.validate().unwrap_err().code, E_CONFIG_INVALID);

    // 环回必须声明 MOTU 参考输入通道；且不能声明 UMIK。
    let mut config = loopback_config();
    config.devices.capture_channel = None;
    assert_eq!(config.validate().unwrap_err().code, E_CONFIG_INVALID);
    let mut config = loopback_config();
    config.mic = Some(mic_declaration("hw:3,0"));
    assert_eq!(config.validate().unwrap_err().code, E_CONFIG_INVALID);
    let mut config = loopback_config();
    config.devices.capture_channel = Some(20);
    assert_eq!(config.validate().unwrap_err().code, E_CONFIG_INVALID);

    // graph id / epoch / block / buffer / format 非法一律拒绝。
    let mut config = acoustic_config();
    config.graph_id = "0".repeat(64);
    assert_eq!(config.validate().unwrap_err().code, E_CONFIG_INVALID);
    let mut config = acoustic_config();
    config.devices.block_frames = 100;
    assert_eq!(config.validate().unwrap_err().code, E_CONFIG_INVALID);
    let mut config = acoustic_config();
    config.devices.buffer_frames = 100;
    assert_eq!(config.validate().unwrap_err().code, E_CONFIG_INVALID);
    let mut config = acoustic_config();
    config.devices.sample_format = sample_format::S16_LE_INTERLEAVED;
    assert_eq!(config.validate().unwrap_err().code, E_CONFIG_INVALID);
}

// ---------------------------------------------------------------------------
// 测量核心（fake 设备抽象；真实 T61/T62 分析路径）
// ---------------------------------------------------------------------------

#[test]
fn t67_fake_acoustic_measurement_passes_protection_and_produces_evidence() {
    let config = acoustic_config();
    let mut engine = fast_clean_engine();
    let cancel = CancelToken::new();
    let output = run_acoustic_measurement(
        &config,
        &mut engine,
        "t67-acoustic-1",
        "4.1.2",
        STAGE_DEFAULT_DRIVE_LEVEL_DBFS,
        &cancel,
        true,
    )
    .expect("healthy fake acoustic capture must pass protection gates");
    assert_eq!(output.summary.engine, "fake-synth");
    assert_eq!(output.summary.mode, StageMeasurementMode::AcousticUmik);
    assert!(output.summary.capture_peak_dbfs <= STAGE_PEAK_CEILING_DBFS);
    assert_eq!(output.summary.clipped_samples, 0);
    assert_eq!(output.summary.xruns, 0);
    assert!(output.summary.snr_db.unwrap() >= STAGE_MIN_SNR_DB);
    assert_eq!(output.summary.total_slots, 1);
    assert_eq!(output.summary.valid_slots, 1);
    assert!(output.summary.all_onsets_valid);
    assert!(!output.raw.is_empty());
    assert!(output.timeline_json.contains("tspi-sweep-timeline"));
    assert!(output.capture_report_json.contains("tspi-umik-capture"));
    assert_eq!(output.raw_relative_path, ACOUSTIC_CAPTURE_RELATIVE_PATH);
    assert!(output.summary.capture_frames >= output.summary.timeline_frames);
    // 驱动电平按 request 生效（默认 -50 dBFS；不是 T61 常量 -30）。
    assert_eq!(output.summary.drive_level_dbfs, -50.0);
}

#[test]
fn t67_fake_loopback_measurement_uses_motu_reference_channel() {
    let config = loopback_config();
    let mut engine = fast_clean_engine();
    let cancel = CancelToken::new();
    let output = run_loopback_measurement(
        &config,
        &mut engine,
        "t67-loopback-1",
        "4.1.2",
        -12.0,
        &cancel,
        true,
    )
    .expect("healthy fake MOTU loopback capture must pass protection gates");
    assert_eq!(
        output.summary.mode,
        StageMeasurementMode::ElectricalLoopback
    );
    assert_eq!(output.summary.capture_channels, 20);
    assert_eq!(output.summary.drive_level_dbfs, -12.0);
    assert!(output.capture_report_json.contains("electrical_loopback"));
    assert_eq!(output.raw_relative_path, LOOPBACK_CAPTURE_RELATIVE_PATH);
}

#[test]
fn t67_clip_xrun_snr_timeout_and_unavailable_map_to_stable_codes() {
    let config = acoustic_config();
    let cancel = CancelToken::new();

    let mut clip = FakeEngine::new(FakeEngineConfig {
        force_clip: true,
        ..FakeEngineConfig::default()
    });
    let failure = run_acoustic_measurement(
        &config, &mut clip, "t67-clip", "4.1.2", -50.0, &cancel, true,
    )
    .unwrap_err();
    assert_eq!(failure.code, E_CAPTURE_CLIP);

    let mut xrun = FakeEngine::new(FakeEngineConfig {
        xruns: 1,
        ..FakeEngineConfig::default()
    });
    let failure = run_acoustic_measurement(
        &config, &mut xrun, "t67-xrun", "4.1.2", -50.0, &cancel, true,
    )
    .unwrap_err();
    assert_eq!(failure.code, E_CAPTURE_XRUN);

    let mut noisy = FakeEngine::new(FakeEngineConfig {
        noise_dbfs: -20.0,
        ..FakeEngineConfig::default()
    });
    let failure = run_acoustic_measurement(
        &config, &mut noisy, "t67-snr", "4.1.2", -50.0, &cancel, true,
    )
    .unwrap_err();
    assert_eq!(failure.code, E_SNR_BELOW_MIN);

    let mut timeout = FakeEngine::new(FakeEngineConfig {
        force_timeout: true,
        ..FakeEngineConfig::default()
    });
    let failure = run_acoustic_measurement(
        &config,
        &mut timeout,
        "t67-timeout",
        "4.1.2",
        -50.0,
        &cancel,
        true,
    )
    .unwrap_err();
    assert_eq!(failure.code, E_CAPTURE_TIMEOUT);

    let mut unavailable = FakeEngine::new(FakeEngineConfig {
        force_unavailable: true,
        ..FakeEngineConfig::default()
    });
    let failure = run_acoustic_measurement(
        &config,
        &mut unavailable,
        "t67-unavailable",
        "4.1.2",
        -50.0,
        &cancel,
        true,
    )
    .unwrap_err();
    assert_eq!(failure.code, E_DEVICE_UNAVAILABLE);
}

#[test]
fn t67_cancel_before_playback_maps_to_cancelled_stage_failure() {
    let config = acoustic_config();
    let mut engine = fast_clean_engine();
    let cancel = CancelToken::new();
    cancel.request();
    let failure = run_acoustic_measurement(
        &config,
        &mut engine,
        "t67-cancel",
        "4.1.2",
        -50.0,
        &cancel,
        true,
    )
    .unwrap_err();
    assert!(
        failure.cancelled,
        "cancel must map to cancelled=true: {failure:?}"
    );
    assert_eq!(failure.code, E_CANCELLED);
}

#[test]
fn t67_hot_level_unverified_is_rejected_before_any_device_access() {
    let config = acoustic_config();
    let mut engine = fast_clean_engine();
    let cancel = CancelToken::new();
    let failure = run_acoustic_measurement(
        &config,
        &mut engine,
        "t67-hot",
        "4.1.2",
        -20.0,
        &cancel,
        true,
    )
    .unwrap_err();
    assert_eq!(failure.code, E_LEVEL_NOT_AUTHORIZED);
}

#[test]
fn t67_snr_estimate_uses_pre_silence_noise_floor() {
    let config = acoustic_config();
    let mut engine = fast_clean_engine();
    let cancel = CancelToken::new();
    let output = run_acoustic_measurement(
        &config,
        &mut engine,
        "t67-snr-estimate",
        "4.1.2",
        -50.0,
        &cancel,
        false,
    )
    .unwrap();
    let snr = output.summary.snr_db.expect("snr must be estimable");
    // 健康合成信号：约 30+ dB（带限 T63 门另算）。
    assert!(snr.is_finite() && snr >= 25.0, "unexpected snr {snr}");
    assert!(output.summary.noise_floor_dbfs.is_some());
}

#[test]
fn t67_protection_gates_are_explicit_and_ordered() {
    let config = acoustic_config();
    let summary = MeasurementSummary {
        schema: STAGE_BACKEND_SCHEMA,
        mode: StageMeasurementMode::AcousticUmik,
        session_id: "gates".to_string(),
        sample_rate: 48_000,
        block_frames: 128,
        playback_channels: 22,
        capture_channels: 1,
        drive_level_dbfs: -50.0,
        k_main_trim_db: 0.0,
        capture_peak_dbfs: 1.0,
        clipped_samples: 10,
        nonfinite_samples: 0,
        capture_frames: 10,
        capture_seconds: 0.0002,
        timeline_frames: 100_000,
        noise_floor_dbfs: Some(-90.0),
        snr_db: Some(1.0),
        snr: None,
        xruns: 3,
        underruns: 1,
        seed_render_frame: 0.0,
        valid_slots: 0,
        total_slots: 1,
        all_onsets_valid: false,
        engine: "fake-synth".to_string(),
        wall_seconds: 0.1,
        notes: "bad".to_string(),
    };
    let gates = protection_gates(&summary, &config);
    let names: Vec<&str> = gates.iter().map(|gate| gate.name).collect();
    assert_eq!(
        names,
        vec!["xrun", "capture_peak", "capture_complete", "onsets", "snr"]
    );
    assert!(gates.iter().all(|gate| !gate.passed));
    let json = gates_json(&gates);
    assert!(json.starts_with('[') && json.ends_with(']'));
    assert!(json.contains(E_CAPTURE_XRUN));
    let failure = evaluate_protection(&summary, &config).unwrap_err();
    assert_eq!(failure.code, E_CAPTURE_XRUN);
}

#[test]
fn t67_retry_path_keeps_extension() {
    assert_eq!(retry_path("capture/umik.raw", 2), "capture/umik.r2.raw");
    assert_eq!(retry_path("summary", 3), "summary.r3");
}

// ---------------------------------------------------------------------------
// 板端 smoke 一步式入口（fake 干跑证据包）
// ---------------------------------------------------------------------------

#[test]
fn t67_board_measurement_writes_hashed_evidence_and_marks_simulated() {
    let mut config = acoustic_config();
    config.engine = EngineKind::Fake;
    let out_dir = temp_path("board-evidence");
    let evidence =
        run_board_measurement(&config, &out_dir).expect("fake dry-run evidence must be produced");
    assert!(evidence.passed);
    assert_eq!(evidence.engine, "fake-synth");
    assert!(evidence.simulated, "fake engine must be marked simulated");
    let manifest_path = out_dir.join("manifest.json");
    let manifest = std::fs::read_to_string(&manifest_path).expect("manifest");
    assert!(manifest.contains("\"simulated\":true"));
    assert!(manifest.contains("NOT_RUN"));
    assert!(manifest.contains("\"status\":\"PASS\""));
    for (relative, bytes, digest) in &evidence.files {
        let data = std::fs::read(out_dir.join(relative)).expect("evidence file");
        assert_eq!(data.len() as u64, *bytes);
        assert_eq!(&sha256_hex(&data), digest);
    }
    assert!(out_dir.join("capture/umik.raw").exists());
    assert!(out_dir.join("quality/report.json").exists());
    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn t67_board_measurement_failure_keeps_evidence_and_returns_gate_code() {
    let mut config = acoustic_config();
    config.engine = EngineKind::Fake;
    // 注入“强制削波”引擎：走 run_board_measurement_with_engine。
    let mut engine = FakeEngine::new(FakeEngineConfig {
        force_clip: true,
        ..FakeEngineConfig::default()
    });
    let out_dir = temp_path("board-failure");
    let failure = run_board_measurement_with_engine(&config, &mut engine, &out_dir).unwrap_err();
    assert_eq!(failure.code, E_CAPTURE_CLIP);
    // 失败也必须保留 raw/quality 证据（不静默丢弃）。
    assert!(out_dir.join("quality/report.json").exists());
    let quality = std::fs::read_to_string(out_dir.join("quality/report.json")).expect("quality");
    assert!(quality.contains("FAIL"));
    assert!(out_dir.join("capture/summary.json").exists());
    let _ = std::fs::remove_dir_all(&out_dir);
}

// ---------------------------------------------------------------------------
// T60 JobManager 接线（真实 backend + fake 引擎；fake 测试保持不变）
// ---------------------------------------------------------------------------

#[test]
fn t67_job_manager_runs_measurement_stages_then_reports_not_wired_fit() {
    let artifacts_root = temp_path("job-artifacts");
    let manager = JobManager::new(
        JobManagerConfig {
            artifacts_root: artifacts_root.clone(),
            checkpoint_path: None,
            apply_confirm_timeout: None,
        },
        Arc::new(FakeSafety::new()),
    )
    .expect("job manager");
    let config = acoustic_config();
    let backend = StageBackend::new(config, Box::new(fast_clean_engine())).expect("backend");
    let job_id = manager
        .start(full_request(-50.0), Box::new(backend))
        .expect("start full job");
    let snapshot = manager
        .wait_terminal(Duration::from_secs(120))
        .expect("job terminal");
    assert_eq!(snapshot.job_id, job_id);
    // 测量三段真实执行后，FitP 未接线必须如实失败（不得 fake PASS）。
    assert_eq!(snapshot.state, JobState::Failed);
    let failure = snapshot.failure.expect("failure");
    assert_eq!(failure.code, E_STAGE_NOT_WIRED);
    assert_eq!(failure.stage, Some(JobStage::FitP));
    // 测量 artifact 必须已登记（preflight / timeline / raw capture / capture report / quality）。
    let kinds: Vec<&str> = snapshot
        .artifacts
        .iter()
        .map(|artifact| artifact.kind.as_str())
        .collect();
    assert!(kinds.contains(&"preflight_report"), "kinds={kinds:?}");
    assert!(kinds.contains(&"sweep_timeline"), "kinds={kinds:?}");
    assert!(kinds.contains(&"raw_capture"), "kinds={kinds:?}");
    assert!(kinds.contains(&"capture_report"), "kinds={kinds:?}");
    assert!(kinds.contains(&"quality_report"), "kinds={kinds:?}");
    for artifact in &snapshot.artifacts {
        let path = artifacts_root
            .join(job_id.artifact_dir_name())
            .join(&artifact.relative_path);
        assert!(path.exists(), "artifact missing: {}", path.display());
    }
    let _ = manager.shutdown(Duration::from_secs(5));
    let _ = std::fs::remove_dir_all(&artifacts_root);
}

#[test]
fn t67_job_manager_sweep_failure_reports_gate_code_at_sweep_stage() {
    let artifacts_root = temp_path("job-artifacts-fail");
    let manager = JobManager::new(
        JobManagerConfig {
            artifacts_root: artifacts_root.clone(),
            checkpoint_path: None,
            apply_confirm_timeout: None,
        },
        Arc::new(FakeSafety::new()),
    )
    .expect("job manager");
    let config = acoustic_config();
    let noisy = FakeEngine::new(FakeEngineConfig {
        noise_dbfs: -20.0,
        ..FakeEngineConfig::default()
    });
    let backend = StageBackend::new(config, Box::new(noisy)).expect("backend");
    manager
        .start(full_request(-50.0), Box::new(backend))
        .expect("start");
    let snapshot = manager
        .wait_terminal(Duration::from_secs(120))
        .expect("job terminal");
    assert_eq!(snapshot.state, JobState::Failed);
    let failure = snapshot.failure.expect("failure");
    assert_eq!(failure.code, E_SNR_BELOW_MIN);
    assert_eq!(failure.stage, Some(JobStage::SweepCapture));
    let _ = manager.shutdown(Duration::from_secs(5));
    let _ = std::fs::remove_dir_all(&artifacts_root);
}
