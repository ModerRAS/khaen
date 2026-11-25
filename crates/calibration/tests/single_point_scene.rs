//! T66v5 单点场景作业桥验收：合成场景跑批 → 摘要透传 → artifact 落盘/登记。
//!
//! 全部离线（Windows 本机可跑），不碰设备；真实采集/apply/闭环复测属 NOT_RUN。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use khaen_calibration::job::{
    sha256_hex, CalibrationJobBackend, ExitDisposition, InputHash, JobKind, JobManager,
    JobManagerConfig, JobRequest, JobStage, SafeOutputController, SafetyError, StageCtx,
    StageFailure, CHECKPOINT_FILE_NAME,
};
use khaen_calibration::single_point_scene::{
    error, fir_artifact_path, run_single_point_scene, write_scene_artifacts, METRICS_ARTIFACT_PATH,
    REPORT_ARTIFACT_PATH,
};
use khaen_mimo_design::quality::QualityConfig;
use khaen_mimo_design::{Cx, RunConfig, SceneInput};

const NFFT: usize = 8192;

fn freqs_on_grid(bins: &[usize]) -> Vec<f64> {
    bins.iter()
        .map(|k| *k as f64 * 48_000.0 / NFFT as f64)
        .collect()
}

fn config() -> RunConfig {
    RunConfig {
        taps: 64,
        delay_samples: 16,
        nfft: NFFT,
        ..RunConfig::default()
    }
}

/// 合成场景：3 design + 2 holdout，H 为解析构造（与 mimo-design 集成测试同构）。
fn scene() -> SceneInput {
    let freqs = freqs_on_grid(&[7, 9, 11, 13, 17, 21]);
    let k = freqs.len();
    let n_mics = 5;
    let design_mics = vec![0usize, 1, 2];
    let holdout_mics = vec![3usize, 4];
    let mut h = Vec::with_capacity(n_mics);
    for m in 0..n_mics {
        let hmic: Vec<Vec<Cx>> = (0..k)
            .map(|kk| {
                let a = 0.15 * kk as f64;
                let ph2 = a + 0.8;
                vec![
                    Cx::new(a.cos(), a.sin()),
                    Cx::new(0.6 * ph2.cos(), 0.6 * ph2.sin()),
                ]
            })
            .collect();
        let _ = m;
        h.push(hmic);
    }
    SceneInput {
        scene_id: "synthetic-scene-bridge".to_string(),
        fixture_dir: "synthetic".to_string(),
        ground_truth_sha256: "0".repeat(64),
        seed: 7,
        rir_scale: 0.5,
        sample_rate_hz: 48_000.0,
        freqs_hz: freqs,
        source_ids: vec!["SUB1".to_string(), "SUB2".to_string()],
        mic_ids: vec![
            "P01".to_string(),
            "P02".to_string(),
            "P03".to_string(),
            "P04".to_string(),
            "P05".to_string(),
        ],
        design_mics,
        holdout_mics,
        h,
        band_centers_hz: vec![40.0, 50.0, 63.0, 80.0, 100.0, 125.0],
        extended_centers_hz: vec![25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0],
    }
}

#[test]
fn bridge_透出三态并产出三类_artifact() {
    let mut cfg = config();
    cfg.safety.enabled = true;
    let artifacts = run_single_point_scene(&scene(), &cfg).expect("scene run");
    let summary = &artifacts.summary;

    // 三态来自报告，不在桥里重新判定。
    let outcome = summary
        .outcome
        .as_deref()
        .expect("safety enabled => outcome");
    assert!(
        matches!(outcome, "FULL" | "PARTIAL" | "NONE"),
        "unexpected outcome: {outcome}"
    );
    assert_eq!(summary.outcome_variant.as_deref(), Some("B3"));
    assert!(!summary.variant_outcomes.is_empty());
    assert!(summary
        .variant_outcomes
        .iter()
        .any(|(key, state)| key == "B2" && matches!(state.as_str(), "FULL" | "PARTIAL" | "NONE")));
    assert!(summary
        .variant_outcomes
        .iter()
        .any(|(key, state)| key == "B3" && matches!(state.as_str(), "FULL" | "PARTIAL" | "NONE")));

    // 报告是合法 JSON，且桥的摘要与报告一致（透传，不修饰）。
    let report: serde_json::Value =
        serde_json::from_str(&artifacts.report_json).expect("report must be valid JSON");
    assert_eq!(report["outcome"], serde_json::json!(outcome));
    assert_eq!(report["judgement"]["measured"], serde_json::json!(false));
    assert_eq!(report["judgement"]["applied"], serde_json::json!(false));
    assert!(!summary.measured);
    assert!(!summary.applied);
    assert!(summary.claim.contains("synthetic"));

    // 三类 artifact 都在。
    assert!(!artifacts.metrics_csv.is_empty());
    assert!(!artifacts.report_json.is_empty());
    assert!(!artifacts.fir_files.is_empty());
    for (path, csv) in &artifacts.fir_files {
        assert!(path.starts_with("single-point/fir/"), "{path}");
        assert!(!csv.is_empty(), "{path}");
    }
}

#[test]
fn 质量门_unknown_频带被透传() {
    let mut cfg = config();
    cfg.safety.enabled = true;
    // 31 个 1/3 oct 中心；前 3 个给 None（证据缺失 → Unknown），其余 30 dB。
    let centers = khaen_mimo_design::quality::THIRD_OCTAVE_CENTERS_HZ_20_20K;
    let mut snr = vec![Some(30.0f64); centers.len()];
    snr[0] = None;
    snr[1] = None;
    snr[2] = None;
    cfg.quality = QualityConfig::from_band_snr(snr);
    let artifacts = run_single_point_scene(&scene(), &cfg).expect("scene run");
    let summary = &artifacts.summary;
    assert_eq!(
        summary.excluded_bands,
        Some(3),
        "unknown bands must be reported, not hidden"
    );
    let report: serde_json::Value =
        serde_json::from_str(&artifacts.report_json).expect("report json");
    let bands = report["quality"]["bands"]
        .as_array()
        .expect("quality bands");
    let unknown = bands
        .iter()
        .filter(|band| band["state"] == serde_json::json!("unknown"))
        .count();
    assert_eq!(unknown, 3);
    // Unknown != 失败：安全钳位结果仍然存在（是否 FULL 由限值决定，不由质量门直接决定）。
    assert!(summary.outcome.is_some());
}

#[test]
fn 同一输入两次运行逐字节相同() {
    let mut cfg = config();
    cfg.safety.enabled = true;
    let first = run_single_point_scene(&scene(), &cfg).expect("first run");
    let second = run_single_point_scene(&scene(), &cfg).expect("second run");
    assert_eq!(first.report_json, second.report_json);
    assert_eq!(first.metrics_csv, second.metrics_csv);
    assert_eq!(first.fir_files, second.fir_files);
    assert_eq!(first.summary, second.summary);
}

#[test]
fn 未启用质量门与钳位时_报告不出现对应块() {
    let artifacts = run_single_point_scene(&scene(), &config()).expect("scene run");
    let summary = &artifacts.summary;
    assert_eq!(summary.outcome, None);
    assert_eq!(summary.outcome_variant, None);
    assert!(summary.variant_outcomes.is_empty());
    assert_eq!(summary.excluded_bands, None);
    let report: serde_json::Value =
        serde_json::from_str(&artifacts.report_json).expect("report json");
    assert!(report.get("outcome").is_none());
    assert!(report.get("quality").is_none());
}

// ===========================================================================
// job 证据链：Compile 阶段跑场景 + 登记三类 artifact；Prepared 阶段从磁盘复核
// ===========================================================================

const VERSION: &str = "single-point-scene-v1";

struct TestSafety {
    released: AtomicBool,
}

impl TestSafety {
    fn new() -> Arc<Self> {
        Arc::new(TestSafety {
            released: AtomicBool::new(false),
        })
    }
}

impl SafeOutputController for TestSafety {
    fn acquire_calibration_owner(&self) -> Result<(), SafetyError> {
        Ok(())
    }

    fn release_calibration_owner(&self, _disposition: ExitDisposition) -> Result<(), SafetyError> {
        self.released.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn restore_last_good(&self) -> Result<String, SafetyError> {
        Ok("last-good".to_string())
    }

    fn activate_safe_mute(&self) -> Result<(), SafetyError> {
        Ok(())
    }

    fn last_good_version(&self) -> Option<String> {
        Some("last-good".to_string())
    }
}

struct SceneBackend {
    cfg: RunConfig,
    outcome: Arc<Mutex<Option<String>>>,
    prepared_ok: Arc<Mutex<bool>>,
}

impl SceneBackend {
    fn new(cfg: RunConfig) -> Self {
        Self {
            cfg,
            outcome: Arc::new(Mutex::new(None)),
            prepared_ok: Arc::new(Mutex::new(false)),
        }
    }
}

impl CalibrationJobBackend for SceneBackend {
    fn run_stage(&mut self, stage: JobStage, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        match stage {
            JobStage::Compile => {
                let artifacts = run_single_point_scene(&scene(), &self.cfg)
                    .map_err(|err| StageFailure::failed(err.code(), err.message()))?;
                *self.outcome.lock().unwrap() = artifacts.summary.outcome.clone();
                let records = write_scene_artifacts(ctx, &artifacts, VERSION)?;
                if records.len() != artifacts.fir_files.len() + 2 {
                    return Err(StageFailure::failed(
                        "E_SINGLE_POINT_SCENE_TEST",
                        "unexpected artifact record count",
                    ));
                }
            }
            JobStage::Prepared => {
                let dir = ctx.artifacts_dir();
                let report = fs::read(dir.join(REPORT_ARTIFACT_PATH)).expect("report on disk");
                let metrics = fs::read(dir.join(METRICS_ARTIFACT_PATH)).expect("metrics on disk");
                if report.is_empty() || metrics.is_empty() {
                    return Err(StageFailure::failed(
                        "E_SINGLE_POINT_SCENE_TEST",
                        "scene artifacts are empty on disk",
                    ));
                }
                // 报告确实是 JSON，且 outcome 与 Compile 阶段透出的一致。
                let report_json: serde_json::Value =
                    serde_json::from_slice(&report).expect("report json on disk");
                let outcome = self.outcome.lock().unwrap().clone();
                if let Some(outcome) = outcome {
                    assert_eq!(report_json["outcome"], serde_json::json!(outcome));
                }
                let fir_dir = dir.join("single-point/fir");
                assert!(fir_dir.is_dir(), "FIR artifacts must be on disk");
                assert!(fs::read_dir(&fir_dir).unwrap().next().is_some());
                *self.prepared_ok.lock().unwrap() = true;
            }
            _ => {}
        }
        Ok(())
    }
}

fn temp_root(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let number = COUNTER.fetch_add(1, Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!(
        "khaen-single-point-scene-{}-{}-{}",
        std::process::id(),
        tag,
        number
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

fn manager_config(root: &Path) -> JobManagerConfig {
    let state_dir = root.join("state");
    fs::create_dir_all(&state_dir).unwrap();
    JobManagerConfig {
        artifacts_root: root.join("artifacts"),
        checkpoint_path: Some(state_dir.join(CHECKPOINT_FILE_NAME)),
        apply_confirm_timeout: None,
    }
}

fn request() -> JobRequest {
    JobRequest {
        kind: JobKind::Full,
        label: "single point scene".to_string(),
        layout: "4.1.2".to_string(),
        sample_rate: 48_000,
        design_points: 3,
        holdout_points: 2,
        repeats: 1,
        drive_level_dbfs: -30.0,
        audible_sweep_authorized: true,
        auto_apply: true,
        expected_applied_version: None,
        inputs: vec![InputHash {
            name: "synthetic-scene".to_string(),
            sha256: sha256_hex(b"scene"),
        }],
    }
}

#[test]
fn 场景产物进_证据链_并在_prepared_阶段复核() {
    let root = temp_root("job");
    let safety = TestSafety::new();
    let manager = JobManager::new(manager_config(&root), safety).unwrap();
    let mut cfg = config();
    cfg.safety.enabled = true;
    let backend = SceneBackend::new(cfg);
    let outcome = Arc::clone(&backend.outcome);
    let prepared_ok = Arc::clone(&backend.prepared_ok);
    manager
        .start(request(), Box::new(backend))
        .expect("start job");
    let terminal = manager.wait_terminal(Duration::from_secs(10)).unwrap();
    assert_eq!(terminal.state, khaen_calibration::job::JobState::Complete);
    assert!(
        *prepared_ok.lock().unwrap(),
        "Prepared must verify from disk"
    );
    let outcome = outcome.lock().unwrap().clone().expect("outcome透传");
    assert!(matches!(outcome.as_str(), "FULL" | "PARTIAL" | "NONE"));

    let kinds: Vec<String> = terminal
        .artifacts
        .iter()
        .map(|artifact| artifact.kind.as_str().to_string())
        .collect();
    assert!(
        kinds.contains(&"single_point_scene_report".to_string()),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&"single_point_scene_metrics".to_string()),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&"single_point_scene_fir".to_string()),
        "{kinds:?}"
    );
    // manifest 落盘。
    let manifest = root
        .join("artifacts")
        .join("job-0001")
        .join(khaen_calibration::job::ARTIFACT_MANIFEST_FILE_NAME);
    assert!(manifest.is_file(), "artifacts manifest must exist");
}

#[test]
fn 不安全的_fir_文件名被拒绝() {
    for name in [
        "../escape.csv",
        "/abs.csv",
        "a//b.csv",
        "with space.csv",
        "",
        "x\ty",
        "a/../b.csv",
    ] {
        let err = fir_artifact_path(name).expect_err("must be rejected");
        assert_eq!(err.code(), error::FIR_PATH, "{name:?}");
        assert!(err.message().contains("safe artifact name"));
    }
    assert_eq!(
        fir_artifact_path("fir-B2-P01.csv").expect("safe name"),
        "single-point/fir-B2-P01.csv"
    );
    assert_eq!(
        fir_artifact_path("fir/B2_SUB1.csv").expect("safe nested name"),
        "single-point/fir/B2_SUB1.csv"
    );
}
