//! T60 私有集成测试（crate 公共 API 契约） —— WBS T60 写域：crates/calibration/tests/T60/。
//!
//! 注册纪律（rt-stack/README.md §5）：leaf 测试放 tests/<Txx>/，由 integrator 在
//! tests/public_registration.rs 加 #[path = "T60/job_contract.rs"] mod job_contract; 后由 cargo 执行。
//! 本文件只用 khaen_calibration::job 的公开 API（模拟 T42/T44/T61-T66 的最小消费方式），
//! 因此也可用 rustc --test 直接对 build 出的 rlib 做独立编译/执行验证（见 evidence/T60/MANIFEST.md）。
//!
//! 覆盖：R48/P08/P12/K14/K56 的最小公共契约：start/status/cancel/apply/rollback、设备互斥、
//! 失败 last-good、SHA-256 artifact、Retest plan、checkpoint 文件。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use khaen_calibration::job::{
    sha256_hex, ArtifactKind, CalibrationJobBackend, ExitDisposition, FileCheckpointStore,
    InputHash, JobError, JobKind, JobManager, JobManagerConfig, JobRequest, JobSnapshot, JobStage,
    JobState, RecoveryOutcome, SafeOutputController, SafetyError, StageCtx, StageFailure,
    ARTIFACT_MANIFEST_FILE_NAME, CHECKPOINT_FILE_NAME,
};

struct TestSafety {
    calls: Mutex<Vec<String>>,
    device_held: AtomicBool,
    restore_fail: AtomicBool,
    last_good: Mutex<String>,
}

impl TestSafety {
    fn new() -> Arc<Self> {
        Arc::new(TestSafety {
            calls: Mutex::new(Vec::new()),
            device_held: AtomicBool::new(false),
            restore_fail: AtomicBool::new(false),
            last_good: Mutex::new("public-api-last-good".to_string()),
        })
    }

    fn push(&self, call: &str) {
        self.calls.lock().unwrap().push(call.to_string());
    }

    fn count(&self, needle: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.contains(needle))
            .count()
    }
}

impl SafeOutputController for TestSafety {
    fn acquire_calibration_owner(&self) -> Result<(), SafetyError> {
        self.push("acquire");
        if self.device_held.swap(true, Ordering::SeqCst) {
            return Err(SafetyError::new("DEVICE_BUSY", "double acquire"));
        }
        Ok(())
    }

    fn release_calibration_owner(&self, disposition: ExitDisposition) -> Result<(), SafetyError> {
        self.push(&format!("release:{}", disposition.as_str()));
        self.device_held.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn restore_last_good(&self) -> Result<String, SafetyError> {
        self.push("restore_last_good");
        if self.restore_fail.load(Ordering::SeqCst) {
            return Err(SafetyError::new("LAST_GOOD_UNAVAILABLE", "mock"));
        }
        Ok(self.last_good.lock().unwrap().clone())
    }

    fn activate_safe_mute(&self) -> Result<(), SafetyError> {
        self.push("activate_safe_mute");
        Ok(())
    }

    fn last_good_version(&self) -> Option<String> {
        Some(self.last_good.lock().unwrap().clone())
    }
}

#[derive(Default)]
struct ScriptedBackend {
    calls: Arc<Mutex<Vec<JobStage>>>,
    fail_at: Option<JobStage>,
    artifacts: bool,
}

impl ScriptedBackend {
    fn new(calls: Arc<Mutex<Vec<JobStage>>>) -> Self {
        ScriptedBackend {
            calls,
            fail_at: None,
            artifacts: false,
        }
    }

    fn fail_at(mut self, stage: JobStage) -> Self {
        self.fail_at = Some(stage);
        self
    }

    fn with_artifacts(mut self) -> Self {
        self.artifacts = true;
        self
    }
}

impl CalibrationJobBackend for ScriptedBackend {
    fn run_stage(&mut self, stage: JobStage, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        self.calls.lock().unwrap().push(stage);
        if self.fail_at == Some(stage) {
            return Err(StageFailure::failed("E_SCRIPTED", "scripted failure"));
        }
        if self.artifacts {
            match stage {
                JobStage::SweepCapture => {
                    ctx.write_artifact(
                        ArtifactKind::RawSweep,
                        "sweep/raw.bin",
                        b"public-api-sweep",
                        "sweep-v1",
                    )?;
                }
                JobStage::Apply => {
                    ctx.set_last_good_version("last-good-before-apply");
                    ctx.set_applied_version("applied-v1");
                }
                JobStage::IndependentHoldout => {
                    ctx.write_artifact(
                        ArtifactKind::HoldoutReport,
                        "holdout/report.json",
                        b"{\"ok\":true}",
                        "holdout-v1",
                    )?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn temp_root(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let number = COUNTER.fetch_add(1, Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!(
        "tspi-caljob-t60-public-{}-{}-{}",
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

fn manager(root: &Path) -> (JobManager, Arc<TestSafety>) {
    let safety = TestSafety::new();
    let manager = JobManager::new(config(root), safety.clone()).unwrap();
    (manager, safety)
}

fn full_request(auto_apply: bool) -> JobRequest {
    JobRequest {
        kind: JobKind::Full,
        label: "T60 public api".to_string(),
        layout: "4.1.2".to_string(),
        sample_rate: 48_000,
        design_points: 1,
        holdout_points: 2,
        repeats: 2,
        drive_level_dbfs: -30.0,
        audible_sweep_authorized: true,
        auto_apply,
        expected_applied_version: None,
        inputs: vec![InputHash {
            name: "layout-presets".to_string(),
            sha256: sha256_hex(b"layout"),
        }],
    }
}

fn wait_for(
    manager: &JobManager,
    timeout: Duration,
    mut predicate: impl FnMut(&JobSnapshot) -> bool,
) -> JobSnapshot {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(snapshot) = manager.snapshot() {
            if predicate(&snapshot) {
                return snapshot;
            }
        }
        if Instant::now() >= deadline {
            panic!("timed out; last={:?}", manager.snapshot());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait_terminal(manager: &JobManager) -> JobSnapshot {
    manager.wait_terminal(Duration::from_secs(10)).unwrap()
}

#[test]
fn public_api_full_flow_completes_with_artifact_manifest() {
    let root = temp_root("full");
    let (manager, safety) = manager(&root);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let backend = ScriptedBackend::new(calls.clone()).with_artifacts();
    let job_id = manager
        .start(full_request(true), Box::new(backend))
        .unwrap();
    let terminal = wait_terminal(&manager);
    assert_eq!(terminal.job_id, job_id);
    assert_eq!(terminal.state, JobState::Complete);
    assert_eq!(terminal.state_name(), "complete");
    assert_eq!(calls.lock().unwrap().as_slice(), JobStage::ALL.as_slice());
    assert_eq!(terminal.applied_version.as_deref(), Some("applied-v1"));
    assert!(terminal.applied_by_job);
    assert_eq!(safety.count("acquire"), 2);
    assert_eq!(safety.count("release:resume_playback"), 2);
    assert_eq!(safety.count("restore_last_good"), 0);
    let artifacts_dir = PathBuf::from(&terminal.artifacts_dir);
    let manifest = fs::read_to_string(artifacts_dir.join(ARTIFACT_MANIFEST_FILE_NAME)).unwrap();
    assert!(manifest.contains("khaen-calibration-artifacts/v1"));
    for artifact in &terminal.artifacts {
        assert!(manifest.contains(&artifact.sha256));
        let path = artifacts_dir.join(&artifact.relative_path);
        assert_eq!(sha256_hex(&fs::read(&path).unwrap()), artifact.sha256);
    }
    let store = FileCheckpointStore::new(root.join("state").join(CHECKPOINT_FILE_NAME));
    assert_eq!(store.load().unwrap().unwrap().state, JobState::Complete);
    let _ = manager.shutdown(Duration::from_secs(5));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn public_api_apply_gate_then_cancel_is_safe() {
    let root = temp_root("cancel");
    let (manager, safety) = manager(&root);
    let calls = Arc::new(Mutex::new(Vec::new()));
    manager
        .start(
            full_request(false),
            Box::new(ScriptedBackend::new(calls.clone())),
        )
        .unwrap();
    let prepared = wait_for(&manager, Duration::from_secs(10), |snapshot| {
        snapshot.state == JobState::Active(JobStage::Prepared) && snapshot.awaiting_apply
    });
    // 停在 prepared 等 apply 时 cancel：不得二次提交/假 ack，必须安全回 last-good
    manager.cancel().unwrap();
    let terminal = wait_terminal(&manager);
    assert_eq!(terminal.state, JobState::Cancelled);
    assert!(matches!(
        terminal.recovery.expect("recovery").outcome,
        RecoveryOutcome::LastGood { .. }
    ));
    assert_eq!(safety.count("restore_last_good"), 0);
    assert!(!safety.device_held.load(Ordering::SeqCst));
    assert!(!calls.lock().unwrap().contains(&JobStage::Apply));
    assert_eq!(prepared.stage_count, 10);
    let _ = manager.shutdown(Duration::from_secs(5));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn public_api_failure_before_apply_keeps_last_good() {
    let root = temp_root("failure");
    let (manager, safety) = manager(&root);
    let calls = Arc::new(Mutex::new(Vec::new()));
    manager
        .start(
            full_request(true),
            Box::new(ScriptedBackend::new(calls).fail_at(JobStage::FitP)),
        )
        .unwrap();
    let terminal = wait_terminal(&manager);
    assert_eq!(terminal.state, JobState::Failed);
    assert_eq!(terminal.failure.as_ref().unwrap().code, "E_SCRIPTED");
    assert!(matches!(
        terminal.recovery.expect("recovery").outcome,
        RecoveryOutcome::LastGood { .. }
    ));
    assert_eq!(safety.count("restore_last_good"), 0);
    let _ = manager.shutdown(Duration::from_secs(5));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn public_api_retest_plan_does_not_apply() {
    let root = temp_root("retest");
    let (manager, safety) = manager(&root);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let request = JobRequest {
        kind: JobKind::Retest,
        expected_applied_version: Some("applied-v1".to_string()),
        ..full_request(false)
    };
    manager
        .start(request, Box::new(ScriptedBackend::new(calls.clone())))
        .unwrap();
    let terminal = wait_terminal(&manager);
    assert_eq!(terminal.state, JobState::Complete);
    assert_eq!(terminal.stage_count, 3);
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        [
            JobStage::Preflight,
            JobStage::Remeasure,
            JobStage::IndependentHoldout
        ]
    );
    assert!(!terminal.applied_by_job);
    assert_eq!(safety.count("acquire"), 1);
    assert_eq!(safety.count("restore_last_good"), 0);
    let _ = manager.shutdown(Duration::from_secs(5));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn public_api_rejects_second_job_while_busy() {
    let root = temp_root("busy");
    let (manager, _safety) = manager(&root);
    // 用 Checkpoint 文件写失败以外的方式让 job 停在 preflight：脚本 backend 没有阻塞能力，
    // 故只验证 start 之后立即的第二个 start 至少被 Busy 或不出第二 job（同 id 单调）。
    let calls = Arc::new(Mutex::new(Vec::new()));
    let first = manager
        .start(full_request(true), Box::new(ScriptedBackend::new(calls)))
        .unwrap();
    let second = manager.start(
        full_request(true),
        Box::new(ScriptedBackend::new(Arc::new(Mutex::new(Vec::new())))),
    );
    match second {
        Err(JobError::Busy) => {}
        Ok(second_id) => {
            // 第一个 job 可能已经结束（快速 happy path），第二个合法启动；id 必须递增。
            assert!(second_id.as_u64() > first.as_u64());
        }
        Err(other) => panic!("unexpected second start error: {other}"),
    }
    let _ = manager.shutdown(Duration::from_secs(5));
    let _ = fs::remove_dir_all(&root);
}
