//! T66v4 单点 artifact 包接进校准 job 证据链：登记/不可变/校验/篡改检测。
//!
//! 全部离线（Windows 本机可跑），不碰设备；真实采集/apply 属 v1 遗留（NOT_RUN）。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use khaen_calibration::job::{
    sha256_hex, ArtifactKind, CalibrationJobBackend, ExitDisposition, InputHash, JobKind,
    JobManager, JobManagerConfig, JobRequest, JobSnapshot, JobStage, SafeOutputController,
    SafetyError, StageCtx, StageFailure, ARTIFACT_MANIFEST_FILE_NAME, CHECKPOINT_FILE_NAME,
};
use khaen_calibration::plan_compile::PlanCompileOptions;
use khaen_calibration::single_point_bundle::{build_bundle, Provenance};
use khaen_calibration::single_point_job::{
    artifact_kind_for, error, register_bundle, verify_registered,
};
use khaen_designer::modal_peq::{BandType, ChannelPeq, DesignMode, ModalBand, PeqDesign};

const SAMPLE_RATE: u32 = 48_000;
const BLOCK_SIZE: u32 = 128;
const CHANNELS: u16 = 2;
const VERSION: &str = "single-point-v1";

fn band(kind: BandType, freq_hz: f64, q: f64, gain_db: f64) -> ModalBand {
    ModalBand::from_params(kind, freq_hz, q, gain_db, f64::from(SAMPLE_RATE)).expect("band")
}

fn design() -> PeqDesign {
    let mut design = PeqDesign::new(SAMPLE_RATE, DesignMode::ModalOnly).expect("design");
    let mut left = ChannelPeq::new("L", 0).expect("channel");
    left.push_band(band(BandType::Peaking, 63.0, 4.0, -6.0));
    let mut right = ChannelPeq::new("R", 1).expect("channel");
    right.push_band(band(BandType::Peaking, 80.0, 5.0, -4.5));
    design.channels.push(left);
    design.channels.push(right);
    design
}

fn options() -> PlanCompileOptions {
    PlanCompileOptions {
        sample_rate: SAMPLE_RATE,
        block_size: BLOCK_SIZE,
        channels: CHANNELS,
        lane_gain_db: vec![0.0, 2.5],
        lane_delay: vec![0, 16],
    }
}

/// 最小安全控制器（只记录调用，不碰设备）。
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

/// 在 Compile stage 打包+登记，在 Prepared stage 校验；可选篡改计划文件。
struct BundleBackend {
    tamper: bool,
    prepared_ok: Arc<Mutex<bool>>,
    verify_code: Arc<Mutex<Option<String>>>,
}

impl BundleBackend {
    fn new(tamper: bool) -> Self {
        Self {
            tamper,
            prepared_ok: Arc::new(Mutex::new(false)),
            verify_code: Arc::new(Mutex::new(None)),
        }
    }
}

impl CalibrationJobBackend for BundleBackend {
    fn run_stage(&mut self, stage: JobStage, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        match stage {
            JobStage::Compile => {
                let bundle = build_bundle(&design(), &options(), Provenance::Synthetic, None)
                    .map_err(|err| StageFailure::failed(err.code(), err.message()))?;
                let records = register_bundle(ctx, &bundle, VERSION)?;
                assert_eq!(records.len(), 4, "params/plan/report/manifest");
            }
            JobStage::Prepared => {
                let bundle = build_bundle(&design(), &options(), Provenance::Synthetic, None)
                    .map_err(|err| StageFailure::failed(err.code(), err.message()))?;
                if self.tamper {
                    let plan_path = ctx
                        .artifacts_dir()
                        .join(khaen_calibration::plan_compile::PLAN_ARTIFACT_PATH);
                    fs::write(&plan_path, b"tampered").expect("tamper plan");
                }
                match verify_registered(ctx, &bundle) {
                    Ok(()) => {
                        *self.prepared_ok.lock().unwrap() = true;
                    }
                    Err(err) => {
                        *self.verify_code.lock().unwrap() = Some(err.code.clone());
                    }
                }
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
        "khaen-single-point-job-{}-{}-{}",
        std::process::id(),
        tag,
        number
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

fn config(root: &Path) -> JobManagerConfig {
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
        label: "single point bundle".to_string(),
        layout: "4.1.2".to_string(),
        sample_rate: SAMPLE_RATE,
        design_points: 1,
        holdout_points: 1,
        repeats: 1,
        drive_level_dbfs: -30.0,
        audible_sweep_authorized: true,
        // 测试关注 artifact 证据链，不在这里验证人工确认流程：自动 apply 让 job 走完。
        auto_apply: true,
        expected_applied_version: None,
        inputs: vec![InputHash {
            name: "layout-presets".to_string(),
            sha256: sha256_hex(b"layout"),
        }],
    }
}

fn wait_terminal(manager: &JobManager) -> JobSnapshot {
    manager.wait_terminal(Duration::from_secs(10)).unwrap()
}

#[test]
fn artifact_kind_mapping_is_stable() {
    assert_eq!(
        artifact_kind_for(khaen_calibration::single_point_bundle::PARAMS_FILE_NAME),
        Some(ArtifactKind::Other("single_point_params".to_string()))
    );
    assert_eq!(
        artifact_kind_for(khaen_calibration::plan_compile::PLAN_ARTIFACT_PATH),
        Some(ArtifactKind::CalibrationDraft)
    );
    assert_eq!(
        artifact_kind_for(khaen_calibration::single_point_bundle::REPORT_FILE_NAME),
        Some(ArtifactKind::Other("single_point_report".to_string()))
    );
    assert_eq!(artifact_kind_for("unknown/path.bin"), None);
}

#[test]
fn bundle_is_registered_and_verified_inside_job() {
    let root = temp_root("register");
    let safety = TestSafety::new();
    let manager = JobManager::new(config(&root), safety).unwrap();
    let backend = BundleBackend::new(false);
    let prepared_ok = Arc::clone(&backend.prepared_ok);
    let verify_code = Arc::clone(&backend.verify_code);
    manager
        .start(request(), Box::new(backend))
        .expect("start job");
    let terminal = wait_terminal(&manager);
    assert_eq!(terminal.state, khaen_calibration::job::JobState::Complete);
    assert!(*prepared_ok.lock().unwrap(), "verify must pass in Prepared");
    assert!(verify_code.lock().unwrap().is_none());

    let kinds: Vec<String> = terminal
        .artifacts
        .iter()
        .map(|artifact| artifact.kind.as_str().to_string())
        .collect();
    assert!(
        kinds.contains(&"single_point_params".to_string()),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&"calibration_draft".to_string()),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&"single_point_report".to_string()),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&"single_point_manifest".to_string()),
        "{kinds:?}"
    );

    // 文件真实落盘，manifest 也写进了 job 的终态清单。
    let artifacts_dir = PathBuf::from(&terminal.artifacts_dir);
    for path in khaen_calibration::single_point_bundle::bundle_paths() {
        assert!(artifacts_dir.join(&path).is_file(), "missing {path:?}");
    }
    let manifest = fs::read_to_string(artifacts_dir.join(ARTIFACT_MANIFEST_FILE_NAME)).unwrap();
    assert!(manifest.contains("single_point_manifest"), "{manifest}");
}

#[test]
fn tampered_bundle_fails_verification_inside_job() {
    let root = temp_root("tamper");
    let safety = TestSafety::new();
    let manager = JobManager::new(config(&root), safety).unwrap();
    let backend = BundleBackend::new(true);
    let prepared_ok = Arc::clone(&backend.prepared_ok);
    let verify_code = Arc::clone(&backend.verify_code);
    manager
        .start(request(), Box::new(backend))
        .expect("start job");
    let _ = wait_terminal(&manager);
    assert!(
        !*prepared_ok.lock().unwrap(),
        "verify must fail after tamper"
    );
    assert_eq!(
        verify_code.lock().unwrap().as_deref(),
        Some(error::VERIFY),
        "stable verify error code"
    );
}
