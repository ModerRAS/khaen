// NOTE(port): T70h-b 私有集成测试（`crate::offline_job` 生产 backend）。
//
// 注册方式：crates/calibration/src/offline_job.rs 末尾
//   #[cfg(test)] mod t70h_tests { include!("../tests/T70h/offline_job.rs"); }
// 运行：cargo test -p khaen-calibration t70h_
//
// 覆盖：JobManager 全链路（成功/FAIL 判定/INCOMPLETE/取消/失败稳定码/断点 resume/不跳步）、
// 两段 WAV 已知电平的数值断言、两次运行逐字节确定性、设计 JSON 编译路径。
// 全部离线（合成 WAV + ABI v3 方案），不依赖设备/网络。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use khaen_designer::modal_peq::{BandType, ChannelPeq, DesignMode, ModalBand, PeqDesign};

use crate::job::{
    sha256_hex, CalibrationJobBackend, ExitDisposition, FileCheckpointStore, InputHash,
    JobCheckpoint, JobKind, JobManager, JobManagerConfig, JobRequest, JobSnapshot, JobStage,
    JobState, SafeOutputController, SafetyError, StageCtx, StageFailure,
};
use crate::offline_job::{
    error, OfflineJobBackend, OfflineJobConfig, BUNDLE_DIR, CLOSED_LOOP_REPORT_FILE,
    CLOSED_LOOP_SUMMARY_FILE, COMPILE_REPORT_FILE, INPUTS_FILE, PLAN_FILE,
};
use crate::plan_compile::{compile_peq_design, PlanCompileOptions};

const SAMPLE_RATE: u32 = 48_000;
const BLOCK_SIZE: u32 = 128;
const TONE_HZ: f64 = 1_000.0;
const TONE_SECONDS: f64 = 1.0;

// ---------------------------------------------------------------------------
// 合成输入
// ---------------------------------------------------------------------------

/// 44B RIFF/WAVE 头 + 单声道 PCM16（确定性小端）。
fn write_wav_bytes(samples: &[f64], sample_rate: u32) -> Vec<u8> {
    let data_bytes = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    for sample in samples {
        let clamped = sample.clamp(-1.0, 1.0);
        let scaled = (clamped * 32767.0).round() as i32;
        out.extend_from_slice(&(scaled as i16).to_le_bytes());
    }
    out
}

fn tone(amplitude: f64, sample_rate: u32, seconds: f64) -> Vec<f64> {
    let count = (sample_rate as f64 * seconds).round() as usize;
    (0..count)
        .map(|index| {
            amplitude
                * (2.0 * std::f64::consts::PI * TONE_HZ * index as f64 / sample_rate as f64).sin()
        })
        .collect()
}

fn write_inputs(root: &Path, applied_gain_db: f64) -> (PathBuf, PathBuf) {
    let baseline_samples = tone(0.5, SAMPLE_RATE, TONE_SECONDS);
    let gain = 10f64.powf(applied_gain_db / 20.0);
    let applied_samples: Vec<f64> = baseline_samples.iter().map(|value| value * gain).collect();
    let baseline = root.join("baseline.wav");
    let applied = root.join("applied.wav");
    fs::write(&baseline, write_wav_bytes(&baseline_samples, SAMPLE_RATE)).unwrap();
    fs::write(&applied, write_wav_bytes(&applied_samples, SAMPLE_RATE)).unwrap();
    (baseline, applied)
}

/// 恒等方案：两 lane、无 PEQ（预测相对电平 = 0 dB）。
fn identity_plan_bytes() -> Vec<u8> {
    let mut design = PeqDesign::new(SAMPLE_RATE, DesignMode::ModalOnly).expect("design");
    design
        .add_channel(ChannelPeq::new("L", 0).expect("channel"))
        .expect("add channel");
    let compiled = compile_peq_design(
        &design,
        &PlanCompileOptions {
            sample_rate: SAMPLE_RATE,
            block_size: BLOCK_SIZE,
            channels: 2,
            lane_gain_db: Vec::new(),
            lane_delay: Vec::new(),
        },
    )
    .expect("compile identity plan");
    compiled.bytes
}

fn write_plan(root: &Path) -> PathBuf {
    let path = root.join("plan.bin");
    fs::write(&path, identity_plan_bytes()).unwrap();
    path
}

fn offline_config(root: &Path, applied_gain_db: f64) -> OfflineJobConfig {
    let (baseline, applied) = write_inputs(root, applied_gain_db);
    let mut config = OfflineJobConfig::new(baseline, applied);
    config.plan_bin = Some(write_plan(root));
    config.point = "P01".to_string();
    config.lane = 0;
    config.centers_hz = vec![TONE_HZ];
    config.tolerance_db = 2.0;
    config.min_compared_bands = 1;
    config.window_len = 4096;
    config
}

fn offline_request(sample_rate: u32) -> JobRequest {
    JobRequest {
        kind: JobKind::SinglePointOffline,
        label: "T70h offline job".to_string(),
        layout: "2.0".to_string(),
        sample_rate,
        design_points: 1,
        holdout_points: 1,
        repeats: 1,
        drive_level_dbfs: -50.0,
        audible_sweep_authorized: true,
        auto_apply: false,
        expected_applied_version: None,
        inputs: Vec::new(),
    }
}

fn sha_entry(name: &str, bytes: &[u8]) -> InputHash {
    InputHash {
        name: name.to_string(),
        sha256: sha256_hex(bytes),
    }
}

// ---------------------------------------------------------------------------
// 测试支架
// ---------------------------------------------------------------------------

struct TestSafety {
    calls: Mutex<Vec<String>>,
}

impl TestSafety {
    fn new() -> Arc<Self> {
        Arc::new(TestSafety {
            calls: Mutex::new(Vec::new()),
        })
    }

    fn record(&self, call: &str) {
        self.calls.lock().unwrap().push(call.to_string());
    }
}

impl SafeOutputController for TestSafety {
    fn acquire_calibration_owner(&self) -> Result<(), SafetyError> {
        self.record("acquire");
        Ok(())
    }

    fn release_calibration_owner(&self, disposition: ExitDisposition) -> Result<(), SafetyError> {
        self.record(&format!("release:{}", disposition.as_str()));
        Ok(())
    }

    fn restore_last_good(&self) -> Result<String, SafetyError> {
        self.record("restore_last_good");
        Ok("test-last-good".to_string())
    }

    fn activate_safe_mute(&self) -> Result<(), SafetyError> {
        self.record("activate_safe_mute");
        Ok(())
    }

    fn last_good_version(&self) -> Option<String> {
        Some("test-last-good".to_string())
    }
}

fn manager_config(root: &Path) -> JobManagerConfig {
    JobManagerConfig {
        artifacts_root: root.join("artifacts"),
        checkpoint_path: Some(root.join("checkpoint")),
        apply_confirm_timeout: None,
    }
}

fn run_to_terminal(root: &Path, config: OfflineJobConfig, request: JobRequest) -> JobSnapshot {
    let manager = JobManager::new(manager_config(root), TestSafety::new()).expect("manager");
    manager
        .start(request, Box::new(OfflineJobBackend::new(config)))
        .expect("start job");
    manager
        .wait_terminal(Duration::from_secs(30))
        .expect("job terminal")
}

fn read_text(path: &Path) -> String {
    String::from_utf8(fs::read(path).unwrap()).unwrap()
}

fn artifact_bytes(snapshot: &JobSnapshot, relative: &str) -> Vec<u8> {
    fs::read(Path::new(&snapshot.artifacts_dir).join(relative)).unwrap()
}

fn assert_artifacts_integrity(snapshot: &JobSnapshot) {
    assert!(!snapshot.artifacts.is_empty());
    for record in &snapshot.artifacts {
        let path = Path::new(&snapshot.artifacts_dir).join(&record.relative_path);
        let bytes = fs::read(&path).unwrap_or_else(|err| {
            panic!("artifact {} unreadable: {err}", record.relative_path)
        });
        assert_eq!(
            bytes.len() as u64,
            record.bytes,
            "artifact {} size mismatch",
            record.relative_path
        );
        assert_eq!(
            sha256_hex(&bytes),
            record.sha256,
            "artifact {} sha256 mismatch",
            record.relative_path
        );
    }
}

struct SlowBackend {
    inner: OfflineJobBackend,
    delay: Duration,
}

impl CalibrationJobBackend for SlowBackend {
    fn run_stage(&mut self, stage: JobStage, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        std::thread::sleep(self.delay);
        self.inner.run_stage(stage, ctx)
    }
}

// ---------------------------------------------------------------------------
// 1) 成功链路：5 个 stage 全跑、证据齐全、PASS 判定
// ---------------------------------------------------------------------------

#[test]
fn t70h_offline_job_full_pipeline_completes_with_evidence() {
    let root = temp_root("full");
    let config = offline_config(&root, 0.0);
    let request = offline_request(SAMPLE_RATE);
    let snapshot = run_to_terminal(&root, config, request);

    assert_eq!(snapshot.state, JobState::Complete, "{:?}", snapshot.failure);
    assert_eq!(snapshot.stage_index, 5, "all five stages must run");
    assert_eq!(snapshot.stage_count, 5);
    assert_eq!(
        JobKind::SinglePointOffline.plan(),
        &[
            JobStage::Validate,
            JobStage::Compile,
            JobStage::Levels,
            JobStage::ClosedLoop,
            JobStage::Bundle,
        ]
    );

    // 每个 stage 的产物都在磁盘上，且 artifact 记录 sha256 与文件一致。
    for relative in [
        INPUTS_FILE,
        PLAN_FILE,
        COMPILE_REPORT_FILE,
        "levels/predicted.json",
        "levels/observed.json",
        CLOSED_LOOP_REPORT_FILE,
        CLOSED_LOOP_SUMMARY_FILE,
        "bundle/predicted.json",
        "bundle/observed.json",
        "bundle/report.json",
        "bundle/manifest.json",
    ] {
        assert!(
            Path::new(&snapshot.artifacts_dir).join(relative).is_file(),
            "missing artifact {relative}"
        );
    }
    assert_artifacts_integrity(&snapshot);
    assert!(
        snapshot.artifacts.len() >= 11,
        "expected >= 11 artifacts, got {}",
        snapshot.artifacts.len()
    );

    // verdict = PASS（float 实际电平 0 dB，预测 0 dB，容差 2 dB）。
    let report = read_text(&Path::new(&snapshot.artifacts_dir).join(CLOSED_LOOP_REPORT_FILE));
    assert!(report.contains("\"verdict\":\"PASS\""), "report: {report}");
    let summary = read_text(&Path::new(&snapshot.artifacts_dir).join(CLOSED_LOOP_SUMMARY_FILE));
    assert!(summary.contains("\"verdict\":\"PASS\""), "summary: {summary}");
    assert!(summary.contains("\"compared_bands\":1"), "summary: {summary}");

    // 编译报告与方案互相印证（ABI v3、sha256 稳定）。
    let compile_report = read_text(&Path::new(&snapshot.artifacts_dir).join(COMPILE_REPORT_FILE));
    let plan_bytes = artifact_bytes(&snapshot, PLAN_FILE);
    assert_eq!(plan_bytes, identity_plan_bytes(), "plan.bin must be deterministic");
    assert!(
        compile_report.contains(&format!("\"plan_sha256\":\"{}\"", sha256_hex(&plan_bytes))),
        "compile report sha mismatch: {compile_report}"
    );
    assert!(compile_report.contains("\"abi\":\"khaen-plan/v3\""));
    assert!(compile_report.contains("\"source\":\"input_plan\""));

    // 四件套 bundle manifest 记录两段 WAV 的实际 sha256（不是路径）。
    let manifest = read_text(&Path::new(&snapshot.artifacts_dir).join("bundle/manifest.json"));
    let baseline = fs::read(root.join("baseline.wav")).unwrap();
    let applied = fs::read(root.join("applied.wav")).unwrap();
    assert!(manifest.contains(&format!("\"baseline_wav_sha256\":\"{}\"", sha256_hex(&baseline))));
    assert!(manifest.contains(&format!("\"applied_wav_sha256\":\"{}\"", sha256_hex(&applied))));
    assert!(!manifest.contains(root.to_string_lossy().as_ref()), "artifact must not embed absolute paths");
}

// ---------------------------------------------------------------------------
// 2) FAIL 判定：+6 dB 实测超出 2 dB 容差；job 仍 complete（verdict 是结果不是 stage 失败）
// ---------------------------------------------------------------------------

#[test]
fn t70h_offline_job_reports_fail_verdict_without_failing_stage() {
    let root = temp_root("fail-verdict");
    let config = offline_config(&root, 6.0);
    let snapshot = run_to_terminal(&root, config, offline_request(SAMPLE_RATE));
    assert_eq!(snapshot.state, JobState::Complete, "{:?}", snapshot.failure);
    assert_eq!(snapshot.stage_index, 5);

    let observed = read_text(&Path::new(&snapshot.artifacts_dir).join("levels/observed.json"));
    // 数值断言：1 kHz 带实测相对电平 ≈ +6.02 dB（PCM16 量化误差 < 0.01 dB）。
    let level = first_level_db(&observed).expect("observed level");
    assert!(
        (level - 6.0206).abs() < 0.2,
        "observed level {level} dB not ~ +6 dB"
    );
    let report = read_text(&Path::new(&snapshot.artifacts_dir).join(CLOSED_LOOP_REPORT_FILE));
    assert!(report.contains("\"verdict\":\"FAIL\""), "report: {report}");
    let summary = read_text(&Path::new(&snapshot.artifacts_dir).join(CLOSED_LOOP_SUMMARY_FILE));
    assert!(summary.contains("\"verdict\":\"FAIL\""));
    assert!(summary.contains("\"violations\":1"), "summary: {summary}");
}

fn first_level_db(levels_json: &str) -> Option<f64> {
    // observed.json 里 first band 的 level_db 是数字；轻量扫描足够（格式由本 crate 生成）。
    let index = levels_json.find("\"level_db\":")?;
    let rest = &levels_json[index + "\"level_db\":".len()..];
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    rest[..end].trim().parse::<f64>().ok()
}

// ---------------------------------------------------------------------------
// 3) INCOMPLETE：基线静音 → 无可比较带，不冒充 PASS
// ---------------------------------------------------------------------------

#[test]
fn t70h_offline_job_reports_incomplete_when_reference_is_silent() {
    let root = temp_root("incomplete");
    let (baseline, applied) = write_inputs(&root, 0.0);
    fs::write(&baseline, write_wav_bytes(&vec![0.0; 48_000], SAMPLE_RATE)).unwrap();
    let mut config = OfflineJobConfig::new(baseline, applied);
    config.plan_bin = Some(write_plan(&root));
    config.centers_hz = vec![TONE_HZ];
    config.min_compared_bands = 1;
    let snapshot = run_to_terminal(&root, config, offline_request(SAMPLE_RATE));
    assert_eq!(snapshot.state, JobState::Complete);
    let summary = read_text(&Path::new(&snapshot.artifacts_dir).join(CLOSED_LOOP_SUMMARY_FILE));
    assert!(
        summary.contains("\"verdict\":\"INCOMPLETE\""),
        "summary: {summary}"
    );
    assert!(summary.contains("\"compared_bands\":0"), "summary: {summary}");
}

// ---------------------------------------------------------------------------
// 4) 确定性：同输入两次运行证据字节逐字节相同
// ---------------------------------------------------------------------------

#[test]
fn t70h_offline_job_is_byte_deterministic_across_runs() {
    let first_root = temp_root("determinism-a");
    let second_root = temp_root("determinism-b");
    let first = run_to_terminal(
        &first_root,
        offline_config(&first_root, 0.0),
        offline_request(SAMPLE_RATE),
    );
    let second = run_to_terminal(
        &second_root,
        offline_config(&second_root, 0.0),
        offline_request(SAMPLE_RATE),
    );
    assert_eq!(first.state, JobState::Complete);
    assert_eq!(second.state, JobState::Complete);
    for relative in [
        PLAN_FILE,
        COMPILE_REPORT_FILE,
        "levels/predicted.json",
        "levels/observed.json",
        CLOSED_LOOP_REPORT_FILE,
        CLOSED_LOOP_SUMMARY_FILE,
        "bundle/predicted.json",
        "bundle/observed.json",
        "bundle/report.json",
        "bundle/manifest.json",
    ] {
        let left = artifact_bytes(&first, relative);
        let right = artifact_bytes(&second, relative);
        assert_eq!(left, right, "artifact {relative} differs between runs");
    }
}

// ---------------------------------------------------------------------------
// 5) 非法输入：稳定错误码，且在 validate stage 失败（不进入 compile）
// ---------------------------------------------------------------------------

#[test]
fn t70h_offline_job_invalid_wav_fails_validate_with_stable_code() {
    let root = temp_root("invalid-wav");
    let (baseline, applied) = write_inputs(&root, 0.0);
    fs::write(&applied, b"not a wav file").unwrap();
    let mut config = OfflineJobConfig::new(baseline, applied);
    config.plan_bin = Some(write_plan(&root));
    let snapshot = run_to_terminal(&root, config, offline_request(SAMPLE_RATE));
    assert_eq!(snapshot.state, JobState::Failed);
    let failure = snapshot.failure.expect("failure");
    assert_eq!(failure.code, error::INPUT);
    assert_eq!(failure.stage, Some(JobStage::Validate));
    assert!(!Path::new(&snapshot.artifacts_dir).join(PLAN_FILE).is_file());
}

#[test]
fn t70h_offline_job_stale_input_sha_is_rejected() {
    let root = temp_root("stale");
    let config = offline_config(&root, 0.0);
    let mut request = offline_request(SAMPLE_RATE);
    request.inputs = vec![sha_entry("baseline", b"stale-bytes")];
    let snapshot = run_to_terminal(&root, config, request);
    assert_eq!(snapshot.state, JobState::Failed);
    let failure = snapshot.failure.expect("failure");
    assert_eq!(failure.code, error::STALE);
    assert_eq!(failure.stage, Some(JobStage::Validate));
}

#[test]
fn t70h_offline_job_sample_rate_mismatch_has_alignment_code() {
    let root = temp_root("alignment");
    let baseline_samples = tone(0.5, SAMPLE_RATE, 0.5);
    let applied_samples = tone(0.5, 44_100, 0.5);
    let baseline = root.join("baseline.wav");
    let applied = root.join("applied.wav");
    fs::write(&baseline, write_wav_bytes(&baseline_samples, SAMPLE_RATE)).unwrap();
    fs::write(&applied, write_wav_bytes(&applied_samples, 44_100)).unwrap();
    let mut config = OfflineJobConfig::new(baseline, applied);
    config.plan_bin = Some(write_plan(&root));
    let snapshot = run_to_terminal(&root, config, offline_request(SAMPLE_RATE));
    assert_eq!(snapshot.state, JobState::Failed);
    assert_eq!(
        snapshot.failure.expect("failure").code,
        error::ALIGNMENT
    );
}

#[test]
fn t70h_offline_job_without_plan_or_design_is_rejected() {
    let root = temp_root("no-plan");
    let (baseline, applied) = write_inputs(&root, 0.0);
    let config = OfflineJobConfig::new(baseline, applied);
    let snapshot = run_to_terminal(&root, config, offline_request(SAMPLE_RATE));
    assert_eq!(snapshot.state, JobState::Failed);
    assert_eq!(snapshot.failure.expect("failure").code, error::CONFIG);
}

// ---------------------------------------------------------------------------
// 6) 设计 JSON → plan_compile 路径
// ---------------------------------------------------------------------------

#[test]
fn t70h_offline_job_design_json_compiles_plan() {
    let root = temp_root("design-json");
    let (baseline, applied) = write_inputs(&root, 0.0);
    let mut design = PeqDesign::new(SAMPLE_RATE, DesignMode::ModalOnly).expect("design");
    let mut channel = ChannelPeq::new("L", 0).expect("channel");
    channel.push_band(
        ModalBand::from_params(BandType::Peaking, 2_000.0, 4.0, -3.0, f64::from(SAMPLE_RATE))
            .expect("band"),
    );
    design.add_channel(channel).expect("add channel");
    let design_path = root.join("design.json");
    fs::write(&design_path, design.to_json()).unwrap();

    let mut config = OfflineJobConfig::new(baseline, applied);
    config.design_json = Some(design_path);
    config.centers_hz = vec![TONE_HZ];
    config.min_compared_bands = 1;
    let snapshot = run_to_terminal(&root, config, offline_request(SAMPLE_RATE));
    assert_eq!(snapshot.state, JobState::Complete, "{:?}", snapshot.failure);

    let plan_bytes = artifact_bytes(&snapshot, PLAN_FILE);
    let plan = khaen_core::plan::Plan::decode(&plan_bytes).expect("plan decodes");
    assert_eq!(plan.sample_rate, SAMPLE_RATE);
    let compile_report = read_text(&Path::new(&snapshot.artifacts_dir).join(COMPILE_REPORT_FILE));
    assert!(compile_report.contains("\"source\":\"design_params\""));
    assert!(compile_report.contains("\"channels\":1"));
    // 设计 JSON 的坏格式有稳定错误码（不是 panic/假成功）。
    let mut bad = OfflineJobConfig::new(root.join("baseline.wav"), root.join("applied.wav"));
    let bad_path = root.join("bad-design.json");
    fs::write(&bad_path, "{not json").unwrap();
    bad.design_json = Some(bad_path);
    let bad_snapshot = run_to_terminal(&root, bad, offline_request(SAMPLE_RATE));
    assert_eq!(bad_snapshot.state, JobState::Failed);
    assert_eq!(bad_snapshot.failure.expect("failure").code, error::DESIGN);
}

// ---------------------------------------------------------------------------
// 7) 取消：job 映射为 cancelled 终态
// ---------------------------------------------------------------------------

#[test]
fn t70h_offline_job_cancel_maps_to_cancelled_terminal_state() {
    let root = temp_root("cancel");
    let manager = JobManager::new(manager_config(&root), TestSafety::new()).expect("manager");
    let backend = SlowBackend {
        inner: OfflineJobBackend::new(offline_config(&root, 0.0)),
        delay: Duration::from_millis(40),
    };
    manager
        .start(offline_request(SAMPLE_RATE), Box::new(backend))
        .expect("start");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = manager.snapshot().expect("snapshot");
        if snapshot.stage.is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "job never entered a stage");
        std::thread::sleep(Duration::from_millis(5));
    }
    manager.cancel().expect("cancel");
    let snapshot = manager
        .wait_terminal(Duration::from_secs(30))
        .expect("terminal");
    assert_eq!(snapshot.state, JobState::Cancelled);
    assert!(snapshot.cancel_requested);
}

// ---------------------------------------------------------------------------
// 8) 断点 resume：从 checkpoint 续跑剩余 stage
// ---------------------------------------------------------------------------

fn checkpoint_at(
    root: &Path,
    kind: JobKind,
    state: JobState,
    request: JobRequest,
) -> JobCheckpoint {
    let stage_index = state
        .active_stage()
        .and_then(|stage| kind.plan().iter().position(|candidate| *candidate == stage))
        .map(|position| position as u32)
        .unwrap_or(0);
    let artifacts_dir = root.join("artifacts").join("job-0001");
    fs::create_dir_all(&artifacts_dir).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    JobCheckpoint {
        job_id: crate::job::JobId(1),
        kind,
        state,
        stage_index,
        plan: kind.plan().to_vec(),
        request,
        artifacts_dir,
        device_held: false,
        awaiting_apply: false,
        applied_by_job: false,
        applied_version: None,
        last_good_version: None,
        created_unix: now,
        updated_unix: now,
        started_unix: Some(now),
        finished_unix: None,
        artifacts: Vec::new(),
        recovery: None,
        failure: None,
        terminal_reason: None,
        evidence_errors: Vec::new(),
    }
}

#[test]
fn t70h_offline_job_resumes_from_checkpoint_and_completes() {
    let root = temp_root("resume");
    let config = offline_config(&root, 0.0);
    let request = offline_request(SAMPLE_RATE);
    let checkpoint = checkpoint_at(
        &root,
        JobKind::SinglePointOffline,
        JobState::Active(JobStage::Compile),
        request,
    );
    FileCheckpointStore::new(root.join("checkpoint"))
        .save(&checkpoint)
        .expect("save checkpoint");
    let manager = JobManager::new(manager_config(&root), TestSafety::new()).expect("manager");
    let pending = manager.pending_resume().expect("pending resume");
    assert_eq!(pending.kind, JobKind::SinglePointOffline);
    assert_eq!(pending.stage, Some(JobStage::Compile));
    manager
        .resume(Box::new(OfflineJobBackend::new(config)))
        .expect("resume");
    let snapshot = manager
        .wait_terminal(Duration::from_secs(30))
        .expect("terminal");
    assert_eq!(snapshot.state, JobState::Complete, "{:?}", snapshot.failure);
    assert_eq!(snapshot.stage_index, 5);
    assert!(Path::new(&snapshot.artifacts_dir).join(PLAN_FILE).is_file());
    assert!(Path::new(&snapshot.artifacts_dir)
        .join(BUNDLE_DIR)
        .join("manifest.json")
        .is_file());
}

#[test]
fn t70h_offline_job_resume_does_not_skip_missing_artifact_stage() {
    let root = temp_root("resume-no-skip");
    // checkpoint 停在 levels：plan.bin 不存在，必须按 E_OFFLINE_JOB_ARTIFACT 失败而不是重编/跳步。
    let config = offline_config(&root, 0.0);
    let request = offline_request(SAMPLE_RATE);
    let checkpoint = checkpoint_at(
        &root,
        JobKind::SinglePointOffline,
        JobState::Active(JobStage::Levels),
        request,
    );
    FileCheckpointStore::new(root.join("checkpoint"))
        .save(&checkpoint)
        .expect("save checkpoint");
    let manager = JobManager::new(manager_config(&root), TestSafety::new()).expect("manager");
    manager
        .resume(Box::new(OfflineJobBackend::new(config)))
        .expect("resume");
    let snapshot = manager
        .wait_terminal(Duration::from_secs(30))
        .expect("terminal");
    assert_eq!(snapshot.state, JobState::Failed);
    let failure = snapshot.failure.expect("failure");
    assert_eq!(failure.code, error::ARTIFACT);
    assert_eq!(failure.stage, Some(JobStage::Levels));
}

// ---------------------------------------------------------------------------
// 9) 状态机契约：kind/stage 机器名可解析（REST/checkpoint 往返）
// ---------------------------------------------------------------------------

#[test]
fn t70h_single_point_offline_plan_and_names_are_frozen() {
    assert_eq!(JobKind::SinglePointOffline.as_str(), "single_point_offline");
    assert_eq!(
        JobKind::parse("single_point_offline"),
        Some(JobKind::SinglePointOffline)
    );
    let names: Vec<&str> = JobKind::SinglePointOffline
        .plan()
        .iter()
        .map(|stage| stage.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["validate", "compile", "levels", "closed_loop", "bundle"]
    );
    for stage in JobKind::SinglePointOffline.plan() {
        assert_eq!(JobStage::parse(stage.as_str()), Some(*stage));
        assert_eq!(JobState::parse(stage.as_str()), Some(JobState::Active(*stage)));
    }
    assert_eq!(JobKind::SinglePointOffline.plan().len(), 5);
}

fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "khaen-t70h-{tag}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}
