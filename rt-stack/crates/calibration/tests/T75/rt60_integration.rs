// T75：goal-6 RT60/CSD 接入 tspi-cal job 测量流水线（离线 fake；无板端/无声卡）。
//
// 注册方式：crates/calibration/src/stage_backend.rs 末尾
//   #[cfg(test)] mod t75_rt60_tests { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/T75/rt60_integration.rs")); }
// 运行：cargo test -p khaen-calibration t75

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::capture::UmikDeviceConfig;
use crate::job::{
    ExitDisposition, JobKind, JobManager, JobManagerConfig, JobRequest, JobSnapshot, JobState,
    SafeOutputController, SafetyError,
};
use crate::sweep::SweepOutput;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_path(tag: &str) -> std::path::PathBuf {
    let base = std::env::temp_dir();
    let id = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
    base.join(format!("tspi-t75-{tag}-{}-{id}", std::process::id()))
}

fn hex64() -> String {
    "0123456789abcdef".repeat(4)
}

fn acoustic_config() -> StageBackendConfig {
    let mut config = StageBackendConfig::default();
    config.mode = StageMeasurementMode::AcousticUmik;
    config.engine = EngineKind::Fake;
    config.mic = Some(UmikDeviceConfig::umik1(
        "hw:3,0",
        "718-7696",
        "umik-90deg.txt",
        hex64(),
        "90deg",
    ));
    config.outputs = vec![SweepOutput::new(0, "FL", 0)];
    config.sweep = StageSweepConfig::smoke();
    config
}

fn full_request() -> JobRequest {
    JobRequest {
        kind: JobKind::Full,
        label: "t75-rt60".to_string(),
        layout: "4.1.2".to_string(),
        sample_rate: 48_000,
        design_points: 6,
        holdout_points: 3,
        repeats: 3,
        drive_level_dbfs: -50.0,
        audible_sweep_authorized: true,
        auto_apply: false,
        expected_applied_version: None,
        inputs: Vec::new(),
    }
}

struct FakeSafety;

impl SafeOutputController for FakeSafety {
    fn acquire_calibration_owner(&self) -> Result<(), SafetyError> {
        Ok(())
    }
    fn release_calibration_owner(&self, _disposition: ExitDisposition) -> Result<(), SafetyError> {
        Ok(())
    }
    fn restore_last_good(&self) -> Result<String, SafetyError> {
        Ok("last-good-v1".to_string())
    }
    fn activate_safe_mute(&self) -> Result<(), SafetyError> {
        Ok(())
    }
    fn last_good_version(&self) -> Option<String> {
        Some("last-good-v1".to_string())
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

/// 跑一次 full job（fake 引擎）；返回 (manager 临时根目录, snapshot)。
fn run_job(
    config: StageBackendConfig,
    rt60: Rt60StageConfig,
) -> (std::path::PathBuf, JobSnapshot) {
    let artifacts_root = temp_path("job-artifacts");
    let manager = JobManager::new(
        JobManagerConfig {
            artifacts_root: artifacts_root.clone(),
            checkpoint_path: None,
            apply_confirm_timeout: None,
        },
        Arc::new(FakeSafety),
    )
    .expect("job manager");
    let backend = StageBackend::new(config, Box::new(fast_clean_engine()))
        .expect("backend")
        .with_rt60(rt60);
    manager
        .start(full_request(), Box::new(backend))
        .expect("start full job");
    let snapshot = manager
        .wait_terminal(Duration::from_secs(120))
        .expect("job terminal");
    let _ = manager.shutdown(Duration::from_secs(5));
    (artifacts_root, snapshot)
}

fn artifact(snapshot: &JobSnapshot, root: &std::path::Path, suffix: &str) -> Option<String> {
    let dir = root.join(snapshot.job_id.artifact_dir_name());
    for record in &snapshot.artifacts {
        if record.relative_path.ends_with(suffix) {
            let path = dir.join(&record.relative_path);
            if path.exists() {
                return std::fs::read_to_string(path).ok();
            }
        }
    }
    None
}

fn kinds(snapshot: &JobSnapshot) -> Vec<&str> {
    snapshot
        .artifacts
        .iter()
        .map(|artifact| artifact.kind.as_str())
        .collect()
}

fn cleanup(root: &std::path::Path) {
    let _ = std::fs::remove_dir_all(root);
}

// --- 测试用确定性合成 IR（0.5 s 衰减 + 直达声 + -45 dBFS 噪声，8 次平均） ---

struct Lcg {
    state: u64,
}

impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg { state: seed | 1 }
    }
    fn next(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state
    }
    fn uniform(&mut self) -> f64 {
        ((self.next() >> 11) as f64 + 1.0) / ((1u64 << 53) as f64)
    }
    fn normal(&mut self) -> f64 {
        let u1 = self.uniform();
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

fn synthetic_ir(rate: u32, rt_true_s: f64, seed: u64) -> Vec<f64> {
    let n = (1.6 * rate as f64) as usize;
    let mut rng = Lcg::new(seed);
    let noise_amp = 10.0f64.powf(-45.0 / 20.0);
    let mut acc = vec![0.0f64; n];
    for _ in 0..8 {
        let mut ir = vec![0.0f64; n];
        for (i, value) in ir.iter_mut().enumerate() {
            let t = i as f64 / rate as f64;
            *value = 0.20 * 10.0f64.powf(-3.0 * t / rt_true_s) * rng.normal();
        }
        ir[0] += 1.0;
        for value in ir.iter_mut() {
            *value += noise_amp * rng.normal();
        }
        let peak = ir.iter().fold(0.0f64, |a, v| a.max(v.abs()));
        for value in ir.iter_mut() {
            *value /= peak;
        }
        for (i, value) in acc.iter_mut().enumerate() {
            *value += ir[i];
        }
    }
    for value in acc.iter_mut() {
        *value /= 8.0;
    }
    acc
}

fn csv_rt60_for_band(csv: &str, label: &str, center_hz: f64) -> Option<(Option<f64>, bool)> {
    for line in csv.lines().skip(1) {
        let fields: Vec<&str> = line.split(',').collect();
        if fields.len() < 8 || fields[0] != label {
            continue;
        }
        let fc = fields[2].parse::<f64>().ok()?;
        if (fc - center_hz).abs() > 1e-6 {
            continue;
        }
        let rt = fields[4].parse::<f64>().ok();
        let valid = fields[6] == "true";
        return Some((rt, valid));
    }
    None
}

#[test]
fn t75_rt60_artifacts_are_written_by_default_and_do_not_block_pipeline() {
    let (root, snapshot) = run_job(acoustic_config(), Rt60StageConfig::default());
    // RT60 后处理不得改变 stage 结果：fake 引擎测量段通过，FitP 仍如实 NOT_WIRED 失败。
    assert_eq!(snapshot.state, JobState::Failed);
    let failure = snapshot.failure.clone().expect("failure");
    assert_eq!(failure.code, E_STAGE_NOT_WIRED);
    assert_eq!(failure.stage, Some(JobStage::FitP));
    let kind_list = kinds(&snapshot);
    assert!(
        kind_list.contains(&"rt60_summary"),
        "artifact kinds: {kind_list:?}"
    );
    assert!(kind_list.contains(&"rt60_csv"), "kinds: {kind_list:?}");
    assert!(kind_list.contains(&"rt60_csd"), "kinds: {kind_list:?}");
    assert!(kind_list.contains(&"rt60_ir"), "kinds: {kind_list:?}");
    let summary = artifact(&snapshot, &root, "rt60/summary.json").expect("rt60 summary");
    assert!(summary.contains("\"schema\":\"tspi-rt60/v1\""));
    assert!(summary.contains("\"kind\":\"deconvolved_capture\""));
    assert!(summary.contains("\"simulated\":true"));
    assert!(summary.contains("\"real_measurement\":\"NOT_RUN\""));
    assert!(summary.contains("\"engine\":\"fake-synth\""));
    // fake 引擎没有房间衰减：RT60 必须如实不可测/无效，而不是编造数字。
    assert!(
        summary.contains("\"valid\":false"),
        "fake-engine IR must not claim valid RT60"
    );
    let csd = artifact(&snapshot, &root, "csd/FL.json").expect("csd artifact");
    assert!(csd.contains("\"schema\":\"tspi-rt60-csd/v1\""));
    assert!(csd.contains("\"nperseg\":1024"));
    let csv = artifact(&snapshot, &root, "rt60/rt60.csv").expect("rt60 csv");
    assert!(csv.starts_with("label,ir,center_hz,method,rt60_s"));
    cleanup(&root);
}

#[test]
fn t75_no_rt60_disables_postprocessing_but_records_status() {
    let (root, snapshot) = run_job(acoustic_config(), Rt60StageConfig::disabled());
    let kind_list = kinds(&snapshot);
    assert!(kind_list.contains(&"rt60_status"), "kinds: {kind_list:?}");
    assert!(!kind_list.contains(&"rt60_summary"), "kinds: {kind_list:?}");
    let status = artifact(&snapshot, &root, "rt60/status.json").expect("status artifact");
    assert!(status.contains("\"enabled\":false"));
    assert!(status.contains("\"status\":\"DISABLED\""));
    assert!(
        artifact(&snapshot, &root, "rt60/rt60.csv").is_none(),
        "disabled must not emit RT60 csv"
    );
    cleanup(&root);
}

#[test]
fn t75_external_real_ir_produces_valid_rt60_and_csd_artifacts() {
    let dir = temp_path("ir-input");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let ir_path = dir.join("ir-FL.wav");
    let ir = synthetic_ir(48_000, 0.5, 0x5EED_0075);
    crate::rt60::write_wav_pcm24(&ir_path, &ir, 48_000).expect("write wav");
    let mut rt60 = Rt60StageConfig::default();
    rt60.external_irs = vec![Rt60ExternalIr {
        label: "FL".to_string(),
        path: ir_path.clone(),
    }];
    let (root, snapshot) = run_job(acoustic_config(), rt60);
    let summary = artifact(&snapshot, &root, "rt60/summary.json").expect("rt60 summary");
    assert!(summary.contains("\"kind\":\"external_ir\""));
    assert!(summary.contains("\"valid\":true"), "summary={summary}");
    assert!(summary.contains("ir-FL.wav"));
    let csv = artifact(&snapshot, &root, "rt60/rt60.csv").expect("rt60 csv");
    let (rt, valid) = csv_rt60_for_band(&csv, "FL", 1000.0).expect("1000 Hz row");
    assert!(valid, "1 kHz should be valid: {csv}");
    let rt = rt.expect("1 kHz rt60");
    assert!(
        (rt - 0.5).abs() <= 0.06,
        "1 kHz RT60 = {rt} (true 0.5 s, tolerance 0.06)"
    );
    assert!(artifact(&snapshot, &root, "csd/FL.json").is_some());
    cleanup(&root);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn t75_rt60_failure_is_nonblocking_and_records_warning() {
    let dir = temp_path("bad-ir");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let bad = dir.join("bad.wav");
    std::fs::write(&bad, b"this is not a wav file").expect("write bad");
    let mut rt60 = Rt60StageConfig::default();
    rt60.external_irs = vec![Rt60ExternalIr {
        label: "BROKEN".to_string(),
        path: bad.clone(),
    }];
    let (root, snapshot) = run_job(acoustic_config(), rt60);
    // 主流程不受影响：测量通过，仍走到 FitP 的 JSON not-wired 失败。
    let failure = snapshot.failure.clone().expect("failure");
    assert_eq!(failure.code, E_STAGE_NOT_WIRED);
    assert_eq!(failure.stage, Some(JobStage::FitP));
    let warning = artifact(&snapshot, &root, "rt60/warning.json").expect("warning artifact");
    assert!(warning.contains("\"status\":\"WARNING\""));
    assert!(warning.contains("\"blocking\":\"false\""));
    assert!(warning.contains("BROKEN"));
    cleanup(&root);
    let _ = std::fs::remove_dir_all(&dir);
}
