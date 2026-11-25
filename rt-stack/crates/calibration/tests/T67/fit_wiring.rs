// T67 前置 fit-wiring 私有单测（经 src/fit_wiring.rs 末尾的 #[cfg(test)] mod { include!(...) } 进入
// lib 单测构建；cargo 不自动发现 tests/<Txx>/ 子目录；公共注册属 integrator，勿写入
// public_registration.rs）。全部测试名 t67_wiring_ 前缀。
//
// 运行：cargo test -p khaen-calibration t67_wiring_
// 覆盖（任务卡要求）：合法链路 / 门失败 / 坏 plan / 回滚路径，全部 fake 数据（离线合成引擎 +
// synthetic_offline 设计输入）。无板端/无声卡/无真实 apply；NOT_RUN 见 evidence/cursor-fix-fitwiring/。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;

use khaen_compat::ws::json_parse;

use crate::compile_apply::{compile_plan, CompileApplyRequest, CompiledPlan};
use crate::job::{
    ExitDisposition, JobKind, JobManager, JobManagerConfig, JobRequest, JobSnapshot, JobStage,
    JobState, SafeOutputController, SafetyError,
};
use crate::peq_fit::{fit_peq, PeqFitRequest};
use crate::phase_fit::{fit_phase, PhaseFitOutput, PhaseFitRequest};
use crate::stage_backend::{
    EngineKind, StageBackend, StageBackendConfig, StageDeviceConfig, StageLevelConfig,
    StageMeasurementMode, StageSweepConfig,
};
use crate::sweep::SweepOutput;

static UNIQUE: AtomicU64 = AtomicU64::new(0);

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let id = UNIQUE.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("tspi-fit-wiring-{tag}-{}-{id}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create test dir");
    dir
}

#[derive(Debug, Default)]
struct OfflineSafety;

impl SafeOutputController for OfflineSafety {
    fn acquire_calibration_owner(&self) -> Result<(), SafetyError> {
        Ok(())
    }
    fn release_calibration_owner(&self, _disposition: ExitDisposition) -> Result<(), SafetyError> {
        Ok(())
    }
    fn restore_last_good(&self) -> Result<String, SafetyError> {
        Ok("last-good-v0".to_string())
    }
    fn activate_safe_mute(&self) -> Result<(), SafetyError> {
        Ok(())
    }
    fn last_good_version(&self) -> Option<String> {
        Some("last-good-v0".to_string())
    }
}

fn fake_stage_config() -> StageBackendConfig {
    let mut config = StageBackendConfig::default();
    config.mode = StageMeasurementMode::AcousticUmik;
    config.engine = EngineKind::Fake;
    config.devices = StageDeviceConfig {
        playback_device: "hw:0,0".to_string(),
        playback_channels: 2,
        capture_device: "hw:1,0".to_string(),
        capture_channels: 1,
        capture_channel: None,
        sample_rate: 48_000,
        block_frames: 128,
        buffer_frames: 512,
        settle_ms: 10,
        sample_format: khaen_core::abi::sample_format::S24_3LE_INTERLEAVED,
    };
    config.mic = Some(StageBackendConfig::umik1_declaration(
        "hw:1,0",
        "SIM-0001",
        "synthetic-cal.txt",
        super::fixture_hash("umik-cal"),
        "90deg",
    ));
    config.level = StageLevelConfig::default();
    config.sweep = StageSweepConfig::smoke();
    config.outputs = vec![SweepOutput::new(0, "FL", 0), SweepOutput::new(1, "FR", 1)];
    config.session_label = "fit-wiring-test".to_string();
    config.graph_id = super::fixture_hash("host-graph");
    config.epoch_id = 1;
    config
}

fn full_request(auto_apply: bool) -> JobRequest {
    JobRequest {
        kind: JobKind::Full,
        label: "t67-wiring-dryrun".to_string(),
        layout: "stereo-2.0".to_string(),
        sample_rate: 48_000,
        design_points: 1,
        holdout_points: 1,
        repeats: 1,
        drive_level_dbfs: -50.0,
        audible_sweep_authorized: true,
        auto_apply,
        expected_applied_version: None,
        inputs: Vec::new(),
    }
}

fn manager_with(root: &std::path::Path) -> JobManager {
    JobManager::new(
        JobManagerConfig {
            artifacts_root: root.join("jobs"),
            checkpoint_path: None,
            apply_confirm_timeout: None,
        },
        Arc::new(OfflineSafety),
    )
    .expect("job manager")
}

fn start_job(manager: &JobManager, auto_apply: bool, wiring: FitWiring) -> crate::job::JobId {
    let backend = StageBackend::from_config(fake_stage_config())
        .expect("stage backend")
        .with_delegate(Box::new(wiring));
    manager
        .start(full_request(auto_apply), Box::new(backend))
        .expect("start job")
}

fn wait_terminal(manager: &JobManager) -> JobSnapshot {
    manager
        .wait_terminal(Duration::from_secs(120))
        .expect("job must reach terminal state")
}

fn wait_until(
    manager: &JobManager,
    what: &str,
    predicate: impl Fn(&JobSnapshot) -> bool,
) -> JobSnapshot {
    for _ in 0..40_000 {
        let snapshot = manager.snapshot().expect("job snapshot");
        if predicate(&snapshot) {
            return snapshot;
        }
        if snapshot.state.is_terminal() {
            panic!(
                "job reached terminal {} before condition {what}: {:?}",
                snapshot.state.as_str(),
                snapshot.failure
            );
        }
        sleep(Duration::from_millis(2));
    }
    panic!("timeout waiting for {what}");
}

fn artifact_paths(snapshot: &JobSnapshot) -> Vec<String> {
    snapshot
        .artifacts
        .iter()
        .map(|record| record.relative_path.clone())
        .collect()
}

fn artifact_bytes(snapshot: &JobSnapshot, relative_path: &str) -> Vec<u8> {
    let root = std::path::Path::new(&snapshot.artifacts_dir);
    std::fs::read(root.join(relative_path))
        .unwrap_or_else(|error| panic!("read artifact {relative_path}: {error}"))
}

fn artifact_text(snapshot: &JobSnapshot, relative_path: &str) -> String {
    String::from_utf8(artifact_bytes(snapshot, relative_path)).expect("artifact is UTF-8")
}

fn json_field<'a>(
    document: &'a khaen_compat::ws::JsonValue,
    field: &str,
) -> &'a khaen_compat::ws::JsonValue {
    document
        .get(field)
        .unwrap_or_else(|| panic!("missing json field {field}"))
}

fn try_compile_design(
    design: &FitWiringDesign,
) -> Result<CompiledPlan, crate::compile_apply::CompileApplyError> {
    let quality = design.design_quality.as_ref().expect("quality");
    let peq = fit_peq(
        &PeqFitRequest::new(&design.plan, &design.target, &design.curves)
            .with_options(design.peq_options.clone())
            .with_design_quality(quality)
            .with_created_unix(design.created_unix),
    )
    .expect("fit_peq for prebuilt plan");
    let phase = fit_phase(
        &PhaseFitRequest::new(&design.plan, &design.phase_responses)
            .with_options(design.phase_options.clone())
            .with_created_unix(design.created_unix)
            .with_channel_indices(&design.phase_channel_indices)
            .with_design_quality(quality),
    )
    .expect("fit_phase for prebuilt plan");
    let peq_artifacts = peq.artifacts();
    let report_bytes = peq_artifacts
        .entries
        .iter()
        .find(|entry| entry.path == crate::peq_fit::FIT_REPORT_ARTIFACT_PATH)
        .map(|entry| entry.bytes.clone())
        .expect("T64 report bytes");
    let headroom = super::build_headroom(design, &peq, &report_bytes).expect("headroom bridge");
    let phase_artifacts = phase.artifacts.clone();
    let phase_evidence =
        super::build_phase_evidence(design, &phase, &phase_artifacts).expect("phase bridge");
    let roles = super::build_roles(design, &peq, &phase).expect("roles bridge");
    let request = CompileApplyRequest::new(
        design.plan.sample_rate_hz,
        design.compile.block_size,
        design.compile.n_in,
        design.compile.container_channels,
        design.compile.low_band_d,
        design.compile.l_g,
        design.compile.l_p,
        &design.mapping,
        &phase_evidence,
        &headroom,
        &design.artifact,
        &design.g_paths,
        &roles,
        design.compile.encoding,
        design.compile.options,
    );
    // 与生产 FitWiring::stage_compile 一致：experiment 必须一路带到 T66（产物打 EXPERIMENT）。
    let request = if design.compile.experiment.is_active() {
        request.with_experiment(design.compile.experiment)
    } else {
        request
    };
    compile_plan(&request)
}

fn compile_design(design: &FitWiringDesign) -> CompiledPlan {
    try_compile_design(design).expect("prebuilt plan compiles")
}

fn phase_ops(design: &FitWiringDesign) -> PhaseFitOutput {
    let mut request = PhaseFitRequest::new(&design.plan, &design.phase_responses)
        .with_options(design.phase_options.clone())
        .with_created_unix(design.created_unix)
        .with_channel_indices(&design.phase_channel_indices);
    if let Some(quality) = design.design_quality.as_ref() {
        request = request.with_design_quality(quality);
    }
    fit_phase(&request).expect("phase fit")
}

// ---------------------------------------------------------------------------
// 0) 夹具/链路前置：T64 band_count=0、T65 非零 D_i 与参考方向（真实执行）
// ---------------------------------------------------------------------------

#[test]
fn t67_wiring_synthetic_design_fit_and_phase_are_real() {
    let design = super::synthetic_design().expect("synthetic design");
    let peq = fit_peq(
        &PeqFitRequest::new(&design.plan, &design.target, &design.curves)
            .with_options(design.peq_options.clone())
            .with_design_quality(design.design_quality.as_ref().expect("quality"))
            .with_created_unix(design.created_unix),
    )
    .expect("T64 fit");
    for channel in &peq.report.channels {
        assert_eq!(
            channel.band_count, 0,
            "flat fixture must be FIR-only (F-37: T66 plan ABI 不能承载 IIR 段)"
        );
        assert!(channel.headroom_pass, "headroom must pass");
        assert!(channel.boost_limits_pass, "boost limits must pass");
    }
    assert!(peq.report.checks.all_rule_2a_pass);
    assert!(peq.report.checks.all_headroom_pass);
    assert!(peq.report.checks.all_boost_limits_pass);
    assert!(
        peq.target_firs.iter().all(|fir| fir.coeffs.is_empty()),
        "target FIR disabled in fixture -> P=identity, D_i 由 T65 承载（F-16 无双扣）"
    );

    let phase = phase_ops(&design);
    assert!(
        phase.report.checks.all_pass,
        "T65 checks: {:?}",
        phase.report
    );
    assert!(phase.report.checks.fir_direction_ok);
    assert_eq!(
        phase.report.reference_channel, "FR",
        "FR 到达最晚 = D=0 参考"
    );
    let ops: Vec<(String, u32, i8)> = phase
        .ops
        .iter()
        .map(|op| {
            (
                op.channel.clone(),
                op.delay_samples,
                op.polarity.sign() as i8,
            )
        })
        .collect();
    assert_eq!(
        ops,
        vec![("FL".to_string(), 3u32, 1i8), ("FR".to_string(), 0u32, 1i8)],
        "D_i = round(max tau - tau_i)：FL 早到 3 samples -> D_FL=3，FR=参考 D=0"
    );
    assert!(
        phase
            .report
            .channels
            .iter()
            .all(|channel| channel.execution.direction_ok),
        "T65 FIR 执行方向（conj(H)）必须逐通道通过"
    );
}

// ---------------------------------------------------------------------------
// 1) 合法链路：Full job -> FitP -> SolveG -> Compile -> Prepared -> Apply -> 复测/holdout
// ---------------------------------------------------------------------------

#[test]
fn t67_wiring_full_job_reaches_apply_with_artifacts() {
    let root = temp_dir("full");
    let design = super::synthetic_design().expect("synthetic design");
    let wiring = FitWiring::new(design).expect("wiring");
    let manager = manager_with(&root);
    start_job(&manager, true, wiring);
    let snapshot = wait_terminal(&manager);
    assert_eq!(
        snapshot.state,
        JobState::Complete,
        "full job must complete: {:?}",
        snapshot.failure
    );
    assert!(
        snapshot.failure.is_none(),
        "no failure: {:?}",
        snapshot.failure
    );
    let paths = artifact_paths(&snapshot);
    for expected in [
        "fit/design-report.json",
        "fit/modal-peq.json",
        "fit/target-fir.json",
        "phase/design-report.json",
        "phase/ops.json",
        "solve/g-source.json",
        "compile/plan.bin",
        "compile/compile-report.json",
        "prepared/prepared-plan.json",
        "apply/apply-ack.json",
        "wiring/fitp-summary.json",
        "wiring/compile-summary.json",
        "wiring/apply-summary.json",
    ] {
        assert!(
            paths.iter().any(|path| path == expected),
            "missing artifact {expected}; got {paths:?}"
        );
    }
    // apply ack：applied=true，但 T3 未测 -> APPLIED_NOT_CONFIRMED（不得 CONFIRMED/PASS）。
    let ack_text = artifact_text(&snapshot, "apply/apply-ack.json");
    let ack = json_parse(&ack_text).expect("apply ack json");
    assert_eq!(json_field(&ack, "applied").as_bool(), Some(true));
    assert_eq!(
        json_field(&ack, "overall_status").as_str(),
        Some("APPLIED_NOT_CONFIRMED"),
        "T3 applied-after remeasure is NOT_RUN -> INCOMPLETE, 不得 CONFIRMED"
    );
    assert_eq!(json_field(&ack, "state").as_str(), Some("applied"));
    assert_eq!(json_field(&ack, "t3_required").as_bool(), Some(true));
    assert_eq!(json_field(&ack, "camilla_involved").as_bool(), Some(false));
    // plan.bin 是真实 plan 字节（非空、header size 128B、以 TSPI magic 开头）。
    let plan = artifact_bytes(&snapshot, "compile/plan.bin");
    assert!(plan.len() > 128, "plan.bin too small: {} bytes", plan.len());
    assert_eq!(
        u32::from_le_bytes([plan[0], plan[1], plan[2], plan[3]]),
        khaen_core::abi::TSPI_PLAN_MAGIC,
        "plan.bin 必须以 TSPI_PLAN_MAGIC 开头"
    );
    // snapshot 的 applied version 来自接线 set_applied_version（ack 后）。
    assert!(
        snapshot
            .applied_version
            .as_deref()
            .map(|version| version.starts_with("pv-1:"))
            .unwrap_or(false),
        "applied_version = {:?}",
        snapshot.applied_version
    );
    assert!(snapshot.applied_by_job);
    // FitP 摘要显式标注离线口径。
    let summary = json_parse(&artifact_text(&snapshot, "wiring/fitp-summary.json"))
        .expect("fitp summary json");
    assert_eq!(
        json_field(&summary, "design_source").as_str(),
        Some("synthetic_offline")
    );
    assert_eq!(json_field(&summary, "measured").as_bool(), Some(false));
    assert_eq!(json_field(&summary, "simulated").as_bool(), Some(true));
    assert_eq!(json_field(&summary, "board_run").as_str(), Some("NOT_RUN"));
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// 2) 回滚路径 A：prepared 等 apply 时 rollback（aborted before commit，job 级）
// ---------------------------------------------------------------------------

#[test]
fn t67_wiring_prepared_gate_rollback_aborts_before_commit() {
    let root = temp_dir("prepared-rollback");
    let design = super::synthetic_design().expect("synthetic design");
    let wiring = FitWiring::new(design).expect("wiring");
    let manager = manager_with(&root);
    start_job(&manager, false, wiring);
    let waiting = wait_until(&manager, "awaiting_apply", |snapshot| {
        snapshot.awaiting_apply
    });
    assert_eq!(waiting.state, JobState::Active(JobStage::Prepared));
    manager.rollback().expect("rollback request");
    let snapshot = wait_terminal(&manager);
    assert_eq!(
        snapshot.state,
        JobState::RolledBack,
        "prepared rollback must be rolled_back: {:?}",
        snapshot.failure
    );
    assert_eq!(
        snapshot.terminal_reason.as_deref(),
        Some("rollback_requested_before_apply")
    );
    assert!(
        snapshot.applied_version.is_none(),
        "不得在 apply 前写 applied version"
    );
    let paths = artifact_paths(&snapshot);
    assert!(paths.iter().any(|path| path == "compile/plan.bin"));
    assert!(paths
        .iter()
        .any(|path| path == "prepared/prepared-plan.json"));
    assert!(
        !paths.iter().any(|path| path == "apply/apply-ack.json"),
        "rollback before commit 不得有 apply ack：{paths:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// 3) 门失败：T63 质量门 blocked -> FitP 如实失败（不进入 compile）
// ---------------------------------------------------------------------------

#[test]
fn t67_wiring_fit_gate_failure_reports_quality_code_at_fitp() {
    let root = temp_dir("fit-gate");
    let mut design = super::synthetic_design().expect("synthetic design");
    let quality = design.design_quality.as_mut().expect("quality");
    quality.continue_allowed = false;
    quality.reasons = vec!["E_POS_SNR_LOW".to_string()];
    let wiring = FitWiring::new(design).expect("wiring");
    let manager = manager_with(&root);
    start_job(&manager, true, wiring);
    let snapshot = wait_terminal(&manager);
    assert_eq!(snapshot.state, JobState::Failed);
    let failure = snapshot.failure.clone().expect("failure");
    assert_eq!(failure.stage, Some(JobStage::FitP), "must fail at FitP");
    assert_eq!(failure.code, "E_FIT_QUALITY_BLOCKED", "T64 门错误码透传");
    let paths = artifact_paths(&snapshot);
    assert!(
        !paths.iter().any(|path| path == "compile/plan.bin"),
        "门失败不得产出 plan：{paths:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// 4) 坏输入/坏 plan：T54 artifact 状态非 READY -> Compile 如实拒收
// ---------------------------------------------------------------------------

#[test]
fn t67_wiring_compile_rejects_non_ready_artifact() {
    let root = temp_dir("bad-artifact");
    let mut design = super::synthetic_design().expect("synthetic design");
    design.artifact.status = "INCOMPLETE".to_string();
    let wiring = FitWiring::new(design).expect("wiring");
    let manager = manager_with(&root);
    start_job(&manager, true, wiring);
    let snapshot = wait_terminal(&manager);
    assert_eq!(snapshot.state, JobState::Failed);
    let failure = snapshot.failure.clone().expect("failure");
    assert_eq!(failure.stage, Some(JobStage::Compile));
    assert_eq!(
        failure.code, "E_CA_ARTIFACT_STATUS",
        "T66 artifact 状态门错误码透传；不得静默 compile"
    );
    assert!(!artifact_paths(&snapshot)
        .iter()
        .any(|path| path == "compile/plan.bin"));
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// 5) 坏 plan 字节：Prepared 的 T66 verify 拒收（离线 artifact 路径）
// ---------------------------------------------------------------------------

#[test]
fn t67_wiring_prepared_rejects_tampered_prebuilt_plan() {
    let root = temp_dir("bad-plan");
    let design = super::synthetic_design().expect("synthetic design");
    let mut plan = compile_design(&design);
    let index = plan.plan_bytes.len() / 2;
    plan.plan_bytes[index] ^= 0xFF;
    let wiring = FitWiring::new(design)
        .expect("wiring")
        .with_prebuilt_plan(plan);
    let manager = manager_with(&root);
    start_job(&manager, true, wiring);
    let snapshot = wait_terminal(&manager);
    assert_eq!(snapshot.state, JobState::Failed);
    let failure = snapshot.failure.clone().expect("failure");
    assert_eq!(
        failure.stage,
        Some(JobStage::Prepared),
        "must fail at Prepared"
    );
    assert_eq!(
        failure.code, "E_CA_PLAN_VERIFY",
        "篡改 plan 字节必须被 T66 verify 拒收（不静默应用）"
    );
    assert!(snapshot.applied_version.is_none());
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// 6) 回滚路径 B：apply all-ack 未达 -> T66 rollback（无 last-good -> SafetyMute）
// ---------------------------------------------------------------------------

#[test]
fn t67_wiring_apply_ack_timeout_rolls_back_to_safety_mute() {
    let root = temp_dir("ack-timeout");
    let design = super::synthetic_design().expect("synthetic design");
    let wiring = FitWiring::new(design)
        .expect("wiring")
        .with_ack_source(Box::new(super::WithheldAck));
    let manager = manager_with(&root);
    start_job(&manager, true, wiring);
    let snapshot = wait_terminal(&manager);
    assert_eq!(snapshot.state, JobState::Failed);
    let failure = snapshot.failure.clone().expect("failure");
    assert_eq!(failure.stage, Some(JobStage::Apply));
    assert_eq!(failure.code, "E_CA_ACK_TIMEOUT", "ack 未达必须如实失败");
    assert!(
        snapshot.applied_version.is_none(),
        "未 all-ack 不得 set_applied_version"
    );
    assert!(!snapshot.applied_by_job);
    let paths = artifact_paths(&snapshot);
    assert!(
        paths.iter().any(|path| path == "apply/rollback.json"),
        "rollback 证据缺失：{paths:?}"
    );
    assert!(
        !paths.iter().any(|path| path == "apply/apply-ack.json"),
        "未 applied 不得写 apply ack：{paths:?}"
    );
    let rollback =
        json_parse(&artifact_text(&snapshot, "apply/rollback.json")).expect("rollback json");
    assert_eq!(
        json_field(&rollback, "mode").as_str(),
        Some("safety_mute"),
        "首轮无 last-good -> 保持 SafetyMute（不输出半图）"
    );
    assert_eq!(json_field(&rollback, "safety_mute").as_bool(), Some(true));
    assert_eq!(json_field(&rollback, "rolled_back").as_bool(), Some(true));
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// 7) JobManager cancel 在 Prepared 等 apply：cancelled 终态 + 无 apply ack
// ---------------------------------------------------------------------------

#[test]
fn t67_wiring_cancel_while_awaiting_apply_is_cancelled() {
    let root = temp_dir("cancel");
    let design = super::synthetic_design().expect("synthetic design");
    let wiring = FitWiring::new(design).expect("wiring");
    let manager = manager_with(&root);
    start_job(&manager, false, wiring);
    let _waiting = wait_until(&manager, "awaiting_apply", |snapshot| {
        snapshot.awaiting_apply
    });
    manager.cancel().expect("cancel request");
    let snapshot = wait_terminal(&manager);
    assert_eq!(snapshot.state, JobState::Cancelled);
    assert_eq!(
        snapshot.terminal_reason.as_deref(),
        Some("cancelled_while_awaiting_apply")
    );
    assert!(!artifact_paths(&snapshot)
        .iter()
        .any(|path| path == "apply/apply-ack.json"));
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// 8) 接线输入结构校验（坏映射在 start 前拒绝，不静默）
// ---------------------------------------------------------------------------

#[test]
fn t67_wiring_rejects_inconsistent_design_inputs() {
    let mut design = super::synthetic_design().expect("synthetic design");
    design.compile.container_channels = 4; // != mapping.container_channels (8)
    let error = match FitWiring::new(design) {
        Ok(_) => panic!("inconsistent container_channels must be rejected"),
        Err(error) => error,
    };
    assert!(error.contains("container"), "unexpected error: {error}");

    let mut design = super::synthetic_design().expect("synthetic design");
    design.plan.participants = vec!["FL".to_string(), "SL".to_string()];
    let error = match FitWiring::new(design) {
        Ok(_) => panic!("participants mismatch must be rejected"),
        Err(error) => error,
    };
    assert!(error.contains("participants"), "unexpected error: {error}");

    let mut design = super::synthetic_design().expect("synthetic design");
    design.g_paths.truncate(3);
    let error = match FitWiring::new(design) {
        Ok(_) => panic!("incomplete G paths must be rejected"),
        Err(error) => error,
    };
    assert!(error.contains("g_paths"), "unexpected error: {error}");
}

// ---------------------------------------------------------------------------
// T67 补测（2026-11-22，calib-continue）：measured -> P_post 目标曲线 FIR ->
// factorized plan 的实验路径。覆盖目标 3/4 的离线解锁逻辑；不碰设备、
// 不产生测量结论。真实 ALSA apply + T3 + holdout 仍是 NOT_RUN（见
// evidence/calib-chain-20261121/measured-pfir-20261122/RUNBOOK.md）。
// ---------------------------------------------------------------------------

#[test]
fn t67_wiring_target_fir_p_post_enable_syncs_plan_abi() {
    let mut design = super::synthetic_design().expect("synthetic design");
    // 生产 dry-run 默认：target FIR 关闭、role 整数延迟保留、L_P=8（P=identity）。
    assert!(!design.peq_options.target_fir.enabled);
    assert!(!design.zero_phase_delay);
    assert_eq!(design.compile.l_p, 8);

    super::enable_target_fir_p_post(&mut design, 64).expect("enable target FIR");
    assert!(design.peq_options.target_fir.enabled);
    assert_eq!(design.peq_options.target_fir.taps, 64);
    assert!(
        design.zero_phase_delay,
        "线性相位 target FIR 必须清 role 整数延迟（F-16）"
    );
    assert_eq!(
        design.compile.l_p, 64,
        "L_P 必须 >= P FIR 长度（否则 E_CA_TAPS）"
    );
}

#[test]
fn t67_wiring_target_fir_p_post_rejects_out_of_range_without_mutation() {
    let mut design = super::synthetic_design().expect("synthetic design");
    for taps in [0usize, 7, 4097] {
        let error = super::enable_target_fir_p_post(&mut design, taps)
            .expect_err("out-of-range taps must be rejected");
        assert!(
            error.contains(super::E_WIRING_TARGET_FIR_EXPERIMENT),
            "error must carry stable code, got {error}"
        );
        assert!(
            !design.peq_options.target_fir.enabled,
            "rejected call must not mutate design"
        );
        assert_eq!(design.compile.l_p, 8);
        assert!(!design.zero_phase_delay);
    }
}

#[test]
fn t67_wiring_measured_target_fir_experiment_requires_single_point() {
    let mut design = super::synthetic_design().expect("synthetic design");
    let error = super::apply_measured_target_fir_experiment(
        &mut design,
        64,
        crate::compile_apply::ExperimentMode::OFF,
    )
    .expect_err("production experiment mode must be rejected");
    assert!(
        error.contains(super::E_WIRING_TARGET_FIR_EXPERIMENT),
        "got {error}"
    );
    assert!(
        !design.peq_options.target_fir.enabled,
        "guard runs before mutation"
    );

    let outcome = super::apply_measured_target_fir_experiment(
        &mut design,
        64,
        crate::compile_apply::ExperimentMode {
            single_point: true,
            multipoint_pilot: false,
            allow_incomplete_artifact: false,
        },
    )
    .expect("experiment enable");
    assert_eq!(outcome.taps, 64);
    assert!(outcome.zero_phase_delay);
    assert_eq!(
        outcome.force_form,
        khaen_core::compile::FusionForm::Factorized
    );
    assert_eq!(
        design.compile.options.force_form,
        Some(khaen_core::compile::FusionForm::Factorized),
        "host plan-arena 只接受 factorized，实验必须显式强制"
    );
    assert!(
        design.compile.experiment.is_active(),
        "experiment 标记必须带到 compile"
    );
}

#[test]
fn t67_wiring_measured_target_fir_experiment_plan_is_factorized_p_rows() {
    let mut design = super::synthetic_design().expect("synthetic design");
    super::apply_measured_target_fir_experiment(
        &mut design,
        64,
        crate::compile_apply::ExperimentMode {
            single_point: true,
            multipoint_pilot: false,
            allow_incomplete_artifact: true,
        },
    )
    .expect("experiment enable");

    let plan = compile_design(&design);
    assert_eq!(
        plan.diags.iter().map(|diag| diag.taps).collect::<Vec<_>>(),
        vec![64, 64],
        "plan P diag 行必须承载 target FIR 全长（stereo fixture: 2 roles）"
    );
    assert_eq!(
        plan.report.facts.chosen_form, "factorized",
        "实验强制 factorized"
    );
    assert_eq!(plan.roles.len(), 2, "stereo fixture: 2 roles");
    assert_eq!(
        plan.diags.len(),
        plan.roles.len(),
        "factorized 形态每个 role 一行 P diag（P_post 目标 FIR）"
    );
    assert!(plan.experiment.is_active(), "plan 必须携带 experiment 模式");
    let envelope = plan
        .experiment_artifact()
        .expect("experiment envelope artifact");
    assert!(
        envelope
            .bytes
            .windows(b"EXPERIMENT".len())
            .any(|w| w == b"EXPERIMENT"),
        "envelope 必须标 EXPERIMENT"
    );
    assert!(
        envelope
            .bytes
            .windows(b"NON_ACCEPTABLE".len())
            .any(|w| w == b"NON_ACCEPTABLE"),
        "envelope 必须标 NON_ACCEPTABLE"
    );
}

#[test]
fn t67_wiring_target_fir_without_zero_delay_is_double_count_rejected() {
    let mut design = super::synthetic_design().expect("synthetic design");
    super::apply_measured_target_fir_experiment(
        &mut design,
        64,
        crate::compile_apply::ExperimentMode {
            single_point: true,
            multipoint_pilot: false,
            allow_incomplete_artifact: true,
        },
    )
    .expect("experiment enable");
    // 模拟“忘了清 role 延迟”的旧 hack：线性相位 P FIR GD + D_i 并存 -> F-16 双扣。
    design.zero_phase_delay = false;
    let error = try_compile_design(&design).expect_err("double count must be rejected");
    assert_eq!(error.code(), crate::compile_apply::E_CA_DOUBLE_COUNT);
}
