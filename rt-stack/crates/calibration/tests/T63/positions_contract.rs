// T63（多点位置/重复质量/设计验证分组/job 恢复）私有测试。
//
// 本文件经 crates/calibration/src/positions.rs 末尾的
//   #[cfg(test)] mod t63_tests { include!("../tests/T63/positions_contract.rs"); }
// 编进 lib 单测（cargo 不自动发现 tests/<子目录>；公共注册属 integrator，不要重复注册）。
// 运行：cargo test -p tspi-calibration positions
//
// 离线：不打开 ALSA / 不碰板端；用合成度量夹具验证 label/role/weight 必填、不要求 xyz、
// design/validation 分组与质量门（缺 SNR/clock/phase 不继续）、plan/quality artifact 的
// 确定性与 job 重启恢复。

use super::*;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::job::{
    sha256_hex, CalibrationJobBackend, ExitDisposition, JobKind, JobManager, JobManagerConfig,
    JobRequest, JobStage, JobState, SafeOutputController, SafetyError, ARTIFACT_MANIFEST_FILE_NAME,
    CHECKPOINT_FILE_NAME,
};

const T63_SESSION: &str = "t63-session-2026-11-17";
// 听音位麦克风朝向（ACCEPTANCE §0.1.2：多声道听音位 90°；与本 crate T62 capture 常量同值）。
const T63_MIC_ORIENTATION: &str = "90deg";
const T63_PHASE_REF: &str =
    "aligned_segment(已去粗延迟); absolute_phase_deg = phase_deg - 360*f*delay_samples/fs";

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

fn t63_participants() -> Vec<String> {
    ["FL", "FR", "SL", "SR", "SUB1"]
        .iter()
        .map(|name| name.to_string())
        .collect()
}

fn t63_plan(design: usize, holdout: usize) -> MultipointPlan {
    let mut positions = Vec::new();
    for index in 0..design {
        positions.push(
            MultipointPosition::design(format!("D{index}"), 1.0)
                .unwrap()
                .with_mic_orientation(T63_MIC_ORIENTATION),
        );
    }
    for index in 0..holdout {
        positions.push(
            MultipointPosition::validation(format!("V{index}"), 1.0)
                .unwrap()
                .with_mic_orientation(T63_MIC_ORIENTATION),
        );
    }
    MultipointPlan::new(
        T63_SESSION,
        "4.1.2",
        48_000,
        t63_participants(),
        CommonD0::new(137).unwrap(),
        T63_PHASE_REF,
        positions,
    )
    .unwrap()
}

fn t63_grid() -> Vec<f64> {
    vec![
        100.0, 125.0, 160.0, 200.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0,
    ]
}

fn t63_magnitude_base() -> Vec<f64> {
    t63_grid()
        .iter()
        .map(|frequency| -6.0 - (frequency / 1_000.0).log10())
        .collect()
}

fn t63_good_repeat(repeat: u32) -> RepeatQualityInput {
    RepeatQualityInput {
        repeat,
        snr_db: Some(32.0),
        snr_lowband_db: Some(28.0),
        // |0.2 ppm| <= 2 * 0.3 ppm：不是显著漂移（ACCEPTANCE 0.2.2 口径）。
        clock_ppm: Some(0.2),
        clock_ppm_uncertainty_ppm: Some(0.3),
        clock_ppm_ok: Some(true),
        clock_drift_corrected: None,
        phase_ref: Some(T63_PHASE_REF.to_string()),
        phase_deg: Some(-12.0),
        delay_samples: Some(123.4),
        residual_phase_10k_deg: Some(4.0),
        residual_phase_span_seconds: Some(12.0),
        residual_phase_sigma_ppm: Some(0.2),
        common_d_residual_samples: Some(0.12),
        polarity: Some(1),
        magnitude_db: Some(t63_magnitude_base()),
        coherence: Some(0.96),
        onset_invalid: Some(false),
    }
}

fn t63_good_repeats(count: u32) -> Vec<RepeatQualityInput> {
    (0..count).map(t63_good_repeat).collect()
}

fn t63_quality_input(
    plan: &MultipointPlan,
    scope: QualityScope,
    repeats: Vec<RepeatQualityInput>,
    repeat_policy: RepeatPolicy,
) -> MultipointQualityInput {
    let mut input = MultipointQualityInput::new(scope, repeat_policy);
    let labels: Vec<String> = plan
        .positions
        .iter()
        .filter(|position| position.role == scope.role())
        .map(|position| position.label.clone())
        .collect();
    for label in labels {
        let mut position = PositionQualityInput::new(label);
        for speaker in &plan.participants {
            let mut measurement = MeasurementQualityInput::new(speaker.clone(), t63_grid());
            for repeat in &repeats {
                measurement.push_repeat(repeat.clone());
            }
            position.measurements.push(measurement);
        }
        input.positions.push(position);
    }
    input
}

fn t63_design_input(plan: &MultipointPlan) -> MultipointQualityInput {
    t63_quality_input(
        plan,
        QualityScope::Design,
        t63_good_repeats(MIN_DESIGN_REPEATS),
        RepeatPolicy::DesignOff,
    )
}

fn t63_validation_input(plan: &MultipointPlan) -> MultipointQualityInput {
    t63_quality_input(
        plan,
        QualityScope::Validation,
        t63_good_repeats(MIN_DESIGN_REPEATS),
        RepeatPolicy::DesignOff,
    )
}

fn t63_design_report(plan: &MultipointPlan) -> PositionQualityReport {
    evaluate_quality(plan, &t63_design_input(plan)).unwrap()
}

fn t63_validation_report(plan: &MultipointPlan) -> PositionQualityReport {
    evaluate_quality(plan, &t63_validation_input(plan)).unwrap()
}

fn t63_report_has(report: &PositionQualityReport, code: &str) -> bool {
    report.reasons.iter().any(|reason| reason == code)
}

fn t63_point<'a>(report: &'a PositionQualityReport, label: &str) -> &'a PositionQuality {
    report
        .points
        .iter()
        .find(|point| point.label == label)
        .unwrap()
}

fn t63_first_repeat_mut(input: &mut MultipointQualityInput) -> &mut RepeatQualityInput {
    &mut input.positions[0].measurements[0].repeats[0]
}

// ---------------------------------------------------------------------------
// plan：label/role/weight 必填，不要求 xyz
// ---------------------------------------------------------------------------

#[test]
fn t63_positions_plan_requires_label_role_weight_but_not_xyz() {
    // 坐标可选（P1 只要求 label/role/weight），但听音位朝向必须是 90deg。
    let plan = t63_plan(T6_MIN_DESIGN_POINTS, T6_MIN_HOLDOUT_POINTS);
    assert_eq!(plan.total_count(), T6_RECOMMENDED_POINTS);
    assert_eq!(plan.design_count(), T6_MIN_DESIGN_POINTS);
    assert_eq!(plan.validation_count(), T6_MIN_HOLDOUT_POINTS);
    assert!(plan
        .positions
        .iter()
        .all(|position| position.coordinates_m.is_none()));
    plan.require_t6_minimums().unwrap();
    assert!(plan
        .positions
        .iter()
        .all(|position| position.mic_orientation.as_deref() == Some(T63_MIC_ORIENTATION)));
    // 坐标在测点对象上可选：提供时合法即可（plan 里不要求 xyz）。
    let positioned = MultipointPosition::design("D-xyz", 1.5)
        .unwrap()
        .with_coordinates([1.2, 0.4, 1.1])
        .with_mic_orientation("90deg");
    assert_eq!(positioned.coordinates_m, Some([1.2, 0.4, 1.1]));
    assert_eq!(positioned.weight, 1.5);
    // label/role/weight 的非法值全部拒绝。
    assert_eq!(
        MultipointPosition::design("", 1.0).unwrap_err().code(),
        "E_POS_CONFIG"
    );
    assert_eq!(
        MultipointPosition::design(" bad", 1.0).unwrap_err().code(),
        "E_POS_CONFIG"
    );
    assert_eq!(
        MultipointPosition::design("bad\tlabel", 1.0)
            .unwrap_err()
            .code(),
        "E_POS_CONFIG"
    );
    assert_eq!(
        MultipointPosition::design("D0", 0.0).unwrap_err().code(),
        "E_POS_CONFIG"
    );
    assert_eq!(
        MultipointPosition::design("D0", f64::NAN)
            .unwrap_err()
            .code(),
        "E_POS_CONFIG"
    );
    let mut bad_coords = MultipointPosition::design("D0", 1.0).unwrap();
    bad_coords.coordinates_m = Some([0.0, f64::INFINITY, 0.0]);
    assert_eq!(bad_coords.validate().unwrap_err().code(), "E_POS_CONFIG");
}

#[test]
fn t63_positions_plan_rejects_invalid_fields_and_duplicate_labels() {
    let positions = vec![
        MultipointPosition::design("P1", 1.0)
            .unwrap()
            .with_mic_orientation(T63_MIC_ORIENTATION),
        MultipointPosition::validation("P1", 1.0)
            .unwrap()
            .with_mic_orientation(T63_MIC_ORIENTATION),
    ];
    let duplicate = MultipointPlan::new(
        T63_SESSION,
        "4.1.2",
        48_000,
        t63_participants(),
        CommonD0::new(0).unwrap(),
        T63_PHASE_REF,
        positions,
    )
    .unwrap_err();
    assert_eq!(duplicate.code(), "E_POS_PLAN_DUPLICATE_LABEL");
    // 采样率必须是 runtime 口径。
    let err = MultipointPlan::new(
        T63_SESSION,
        "4.1.2",
        44_100,
        t63_participants(),
        CommonD0::new(0).unwrap(),
        T63_PHASE_REF,
        vec![
            MultipointPosition::design("D0", 1.0).unwrap(),
            MultipointPosition::validation("V0", 1.0).unwrap(),
        ],
    )
    .unwrap_err();
    assert_eq!(err.code(), "E_POS_CONFIG");
    // participants 为空 / 重复 / 非法名。
    for participants in [
        Vec::<String>::new(),
        vec!["FL".to_string(), "FL".to_string()],
        vec!["fl".to_string()],
    ] {
        let err = MultipointPlan::new(
            T63_SESSION,
            "4.1.2",
            48_000,
            participants,
            CommonD0::new(0).unwrap(),
            T63_PHASE_REF,
            vec![
                MultipointPosition::design("D0", 1.0).unwrap(),
                MultipointPosition::validation("V0", 1.0).unwrap(),
            ],
        )
        .unwrap_err();
        assert_eq!(err.code(), "E_POS_CONFIG");
    }
    // phase_ref 缺失/不支持（delay_removed 无可机读延迟）。
    for phase_ref in ["", "delay_removed(1ms)", "unknown-semantics"] {
        let err = MultipointPlan::new(
            T63_SESSION,
            "4.1.2",
            48_000,
            t63_participants(),
            CommonD0::new(0).unwrap(),
            phase_ref,
            vec![
                MultipointPosition::design("D0", 1.0).unwrap(),
                MultipointPosition::validation("V0", 1.0).unwrap(),
            ],
        )
        .unwrap_err();
        assert_eq!(err.code(), "E_POS_PLAN_PHASE_REF");
    }
    // design/validation 两组都必须在预声明中出现。
    let err = MultipointPlan::new(
        T63_SESSION,
        "4.1.2",
        48_000,
        t63_participants(),
        CommonD0::new(0).unwrap(),
        T63_PHASE_REF,
        vec![MultipointPosition::design("D0", 1.0)
            .unwrap()
            .with_mic_orientation(T63_MIC_ORIENTATION)],
    )
    .unwrap_err();
    assert_eq!(err.code(), "E_POS_CONFIG");
}

#[test]
fn t63_positions_plan_groups_design_and_validation_without_overlap() {
    let mut positions: Vec<MultipointPosition> = (0..7)
        .map(|index| {
            MultipointPosition::design(format!("D{index}"), 0.5)
                .unwrap()
                .with_mic_orientation(T63_MIC_ORIENTATION)
        })
        .collect();
    positions.extend((0..4).map(|index| {
        MultipointPosition::validation(format!("V{index}"), 1.5)
            .unwrap()
            .with_mic_orientation(T63_MIC_ORIENTATION)
    }));
    let plan = MultipointPlan::new(
        T63_SESSION,
        "5.3.2",
        96_000,
        t63_participants(),
        CommonD0::new(-41).unwrap(),
        "absolute_phase_deg = phase_deg (total phase)",
        positions,
    )
    .unwrap();
    assert_eq!(plan.design_count(), 7);
    assert_eq!(plan.validation_count(), 4);
    let design: Vec<&str> = plan.design_positions().map(|p| p.label.as_str()).collect();
    let validation: Vec<&str> = plan
        .validation_positions()
        .map(|p| p.label.as_str())
        .collect();
    assert!(design.iter().all(|label| !validation.contains(label)));
    assert!((plan.design_weight_sum() - 3.5).abs() < 1e-12);
    assert_eq!(plan.phase_basis().unwrap(), PhaseBasis::Absolute);
    assert_eq!(plan.common_d0.samples(), -41);
}

#[test]
fn t63_positions_listening_session_requires_90deg_mic_orientation() {
    // T62/T63 共用同一朝向字面值（ACCEPTANCE §0.1.2）。
    assert_eq!(
        T63_MIC_ORIENTATION,
        crate::capture::UMIK_ORIENTATION_VERTICAL
    );
    // 全部 90deg -> plan 通过，JSON 记录 90deg。
    let plan = t63_plan(6, 3);
    assert!(plan
        .positions
        .iter()
        .all(|position| position.mic_orientation.as_deref() == Some(T63_MIC_ORIENTATION)));
    assert!(plan.to_json().contains("\"mic_orientation\":\"90deg\""));
    // 0deg/其它值/未声明 -> E_POS_MIC_ORIENTATION（听音位会话拒绝非 90°）。
    for orientation in [Some("0deg"), Some("90"), Some("vertical"), None] {
        let mut positions: Vec<MultipointPosition> = (0..6)
            .map(|index| {
                MultipointPosition::design(format!("D{index}"), 1.0)
                    .unwrap()
                    .with_mic_orientation(T63_MIC_ORIENTATION)
            })
            .collect();
        positions.extend((0..3).map(|index| {
            MultipointPosition::validation(format!("V{index}"), 1.0)
                .unwrap()
                .with_mic_orientation(T63_MIC_ORIENTATION)
        }));
        positions[2].mic_orientation = orientation.map(str::to_string);
        let error = MultipointPlan::new(
            T63_SESSION,
            "4.1.2",
            48_000,
            t63_participants(),
            CommonD0::new(137).unwrap(),
            T63_PHASE_REF,
            positions,
        )
        .unwrap_err();
        assert_eq!(error.code(), "E_POS_MIC_ORIENTATION");
        assert!(error.to_string().contains("90deg"));
    }
    // artifact 解析路径同样拒收（篡改 90deg -> 0deg；先于 plan hash 校验）。
    let json = t63_plan(6, 3).to_json();
    let tampered = json.replacen(
        "\"mic_orientation\":\"90deg\"",
        "\"mic_orientation\":\"0deg\"",
        1,
    );
    assert_ne!(tampered, json);
    let error = MultipointPlan::from_json(&tampered).unwrap_err();
    assert_eq!(error.code(), "E_POS_MIC_ORIENTATION");
}

#[test]
fn t63_positions_plan_requires_t6_minimums() {
    let pilot = t63_plan(5, 3);
    assert_eq!(
        pilot.require_t6_minimums().unwrap_err().code(),
        "E_POS_PLAN_DESIGN_POINTS"
    );
    let few_holdout = t63_plan(6, 2);
    assert_eq!(
        few_holdout.require_t6_minimums().unwrap_err().code(),
        "E_POS_PLAN_HOLDOUT_POINTS"
    );
    t63_plan(6, 3).require_t6_minimums().unwrap();
    t63_plan(9, 5).require_t6_minimums().unwrap();
}

#[test]
fn t63_positions_plan_json_roundtrip_is_deterministic_and_hash_bound() {
    let plan = t63_plan(6, 3);
    let json_a = plan.to_json();
    let json_b = plan.to_json();
    assert_eq!(json_a, json_b);
    assert!(json_a.contains("\"schema\":\"tspi-calibration-positions/v1\""));
    assert!(json_a.contains("\"design\""));
    assert!(json_a.contains("\"holdout\""));
    let parsed = MultipointPlan::from_json(&json_a).unwrap();
    assert_eq!(parsed, plan);
    assert_eq!(parsed.plan_sha256(), plan.plan_sha256());
    assert_eq!(plan.plan_sha256().len(), 64);
    // 篡改 label 后自 hash 必须失配（冻结/stale 保护）。
    let tampered = json_a.replace("\"label\":\"D0\"", "\"label\":\"D0x\"");
    assert_ne!(tampered, json_a);
    let error = MultipointPlan::from_json(&tampered).unwrap_err();
    assert_eq!(error.code(), "E_POS_ARTIFACT_HASH");
    // 篡改 plan_sha256 本身也拒绝。
    let wrong_hash = json_a.replace(&plan.plan_sha256(), &"0".repeat(64));
    let error = MultipointPlan::from_json(&wrong_hash).unwrap_err();
    assert_eq!(error.code(), "E_POS_ARTIFACT_HASH");
    // 非 JSON / schema 不符拒绝。
    assert_eq!(
        MultipointPlan::from_json("not-json").unwrap_err().code(),
        "E_POS_JSON"
    );
    let wrong_schema = json_a.replace(POSITION_PLAN_SCHEMA, "other/v1");
    assert_eq!(
        MultipointPlan::from_json(&wrong_schema).unwrap_err().code(),
        "E_POS_ARTIFACT_SCHEMA"
    );
    // 坐标是可选字段：含坐标的 plan 也能 roundtrip。
    let with_xyz = MultipointPlan::new(
        T63_SESSION,
        "4.1.2",
        48_000,
        t63_participants(),
        CommonD0::new(137).unwrap(),
        T63_PHASE_REF,
        vec![
            MultipointPosition::design("D0", 1.0)
                .unwrap()
                .with_coordinates([1.0, 2.0, 3.0])
                .with_mic_orientation(T63_MIC_ORIENTATION),
            MultipointPosition::validation("V0", 1.0)
                .unwrap()
                .with_mic_orientation(T63_MIC_ORIENTATION),
        ],
    )
    .unwrap();
    let xyz_json = with_xyz.to_json();
    assert!(xyz_json.contains("\"coordinates_m\":[1,2,3]"));
    assert_eq!(MultipointPlan::from_json(&xyz_json).unwrap(), with_xyz);
}

#[test]
fn t63_positions_plan_artifact_is_idempotent_for_job_resume() {
    let plan = t63_plan(6, 3);
    let (path_a, bytes_a, version_a) = plan_stage_artifact(&plan).unwrap();
    let (path_b, bytes_b, version_b) = plan_stage_artifact(&plan).unwrap();
    assert_eq!(path_a, PLAN_ARTIFACT_PATH);
    assert_eq!(path_b, path_a);
    assert_eq!(bytes_a, bytes_b);
    assert_eq!(version_a, version_b);
    assert_eq!(version_a, plan.plan_sha256());
    assert_eq!(sha256_hex(&bytes_a), sha256_hex(&bytes_b));
    let report = t63_design_report(&plan);
    let (quality_path, quality_bytes, quality_version) = quality_stage_artifact(&report).unwrap();
    assert_eq!(quality_path, QUALITY_DESIGN_ARTIFACT_PATH);
    assert!(quality_version.starts_with("design-"));
    assert_eq!(
        report.to_json().into_bytes(),
        quality_bytes,
        "quality artifact bytes must be deterministic"
    );
}

// ---------------------------------------------------------------------------
// quality：design/validation scope 通过路径
// ---------------------------------------------------------------------------

#[test]
fn t63_positions_quality_design_scope_passes_with_full_metrics() {
    let plan = t63_plan(6, 3);
    let report = t63_design_report(&plan);
    assert_eq!(report.scope, QualityScope::Design);
    assert_eq!(report.repeat_policy, RepeatPolicy::DesignOff);
    assert_eq!(report.min_repeats, MIN_DESIGN_REPEATS);
    assert_eq!(report.points_total, 6);
    assert_eq!(report.points_usable, 6);
    assert!(report.continue_allowed());
    assert_eq!(report.plan_sha256, plan.plan_sha256());
    assert_eq!(report.plan_design_total, 6);
    assert_eq!(report.plan_validation_total, 3);
    assert_eq!(report.required_usable_points(), T6_MIN_DESIGN_POINTS);
    assert!(report.reasons.is_empty());
    assert_eq!(quality_stage_failure(&report), None);
    let point = t63_point(&report, "D0");
    assert!(point.usable);
    assert_eq!(point.role, PositionRole::Design);
    assert_eq!(point.measurements.len(), plan.participants.len());
    let measurement = &point.measurements[0];
    assert!(measurement.usable);
    assert_eq!(measurement.speaker, "FL");
    assert_eq!(measurement.repeats_supplied, MIN_DESIGN_REPEATS);
    assert_eq!(measurement.repeats_valid, MIN_DESIGN_REPEATS);
    assert_eq!(measurement.snr_db_min, Some(32.0));
    assert_eq!(measurement.snr_lowband_db_min, Some(28.0));
    assert_eq!(measurement.polarity, Some(1));
    assert!((measurement.delay_spread_samples.unwrap()).abs() < 1e-12);
    assert!((measurement.magnitude_spread_db.unwrap()).abs() < 1e-12);
    assert!(measurement.coherence_available);
}

#[test]
fn t63_positions_quality_validation_scope_passes_three_holdout_points() {
    let plan = t63_plan(6, 3);
    let report = t63_validation_report(&plan);
    assert_eq!(report.scope, QualityScope::Validation);
    assert_eq!(report.points_total, T6_MIN_HOLDOUT_POINTS);
    assert_eq!(report.points_usable, T6_MIN_HOLDOUT_POINTS);
    assert!(report.continue_allowed());
    assert_eq!(report.required_usable_points(), T6_MIN_HOLDOUT_POINTS);
    assert_eq!(report.plan_design_total, 6);
    assert_eq!(report.plan_validation_total, 3);
    let labels: Vec<&str> = report
        .points
        .iter()
        .map(|point| point.label.as_str())
        .collect();
    assert_eq!(labels, vec!["V0", "V1", "V2"]);
    assert!(report
        .points
        .iter()
        .all(|point| point.role == PositionRole::Validation));
}

// ---------------------------------------------------------------------------
// quality：缺 SNR / clock / phase 直接停（DoD 核心）
// ---------------------------------------------------------------------------

#[test]
fn t63_positions_quality_missing_snr_stops() {
    let plan = t63_plan(6, 3);
    let mut input = t63_design_input(&plan);
    t63_first_repeat_mut(&mut input).snr_db = None;
    let error = evaluate_quality(&plan, &input).unwrap_err();
    assert_eq!(error.code(), E_SNR_MISSING);
    assert!(error.to_string().contains("snr_db"));
    let mut input = t63_design_input(&plan);
    t63_first_repeat_mut(&mut input).snr_lowband_db = None;
    let error = evaluate_quality(&plan, &input).unwrap_err();
    assert_eq!(error.code(), E_SNR_LOWBAND_MISSING);
    // 有值但非有限同样拒绝（不猜）。
    let mut input = t63_design_input(&plan);
    t63_first_repeat_mut(&mut input).snr_db = Some(f64::NAN);
    assert_eq!(
        evaluate_quality(&plan, &input).unwrap_err().code(),
        E_SNR_MISSING
    );
}

#[test]
fn t63_positions_quality_missing_clock_stops() {
    let plan = t63_plan(6, 3);
    for edit in [
        (|repeat: &mut RepeatQualityInput| repeat.clock_ppm = None) as fn(&mut RepeatQualityInput),
        |repeat: &mut RepeatQualityInput| repeat.clock_ppm_uncertainty_ppm = None,
        |repeat: &mut RepeatQualityInput| repeat.clock_ppm_ok = None,
    ] {
        let mut input = t63_design_input(&plan);
        edit(t63_first_repeat_mut(&mut input));
        let error = evaluate_quality(&plan, &input).unwrap_err();
        assert_eq!(error.code(), E_CLOCK_MISSING);
    }
    // 非正的 uncertainty 是非法度量（不是可继续的 0）。
    let mut input = t63_design_input(&plan);
    t63_first_repeat_mut(&mut input).clock_ppm_uncertainty_ppm = Some(0.0);
    assert_eq!(
        evaluate_quality(&plan, &input).unwrap_err().code(),
        E_CLOCK_MISSING
    );
}

#[test]
fn t63_positions_quality_missing_phase_stops() {
    let plan = t63_plan(6, 3);
    for edit in [
        (|repeat: &mut RepeatQualityInput| repeat.phase_ref = None) as fn(&mut RepeatQualityInput),
        |repeat: &mut RepeatQualityInput| repeat.phase_deg = None,
        |repeat: &mut RepeatQualityInput| repeat.delay_samples = None,
        |repeat: &mut RepeatQualityInput| repeat.residual_phase_10k_deg = None,
        |repeat: &mut RepeatQualityInput| repeat.residual_phase_span_seconds = None,
        |repeat: &mut RepeatQualityInput| repeat.residual_phase_sigma_ppm = None,
        |repeat: &mut RepeatQualityInput| repeat.common_d_residual_samples = None,
        |repeat: &mut RepeatQualityInput| repeat.polarity = None,
    ] {
        let mut input = t63_design_input(&plan);
        edit(t63_first_repeat_mut(&mut input));
        let error = evaluate_quality(&plan, &input).unwrap_err();
        assert!(
            matches!(
                error.code(),
                E_PHASE_MISSING
                    | E_DELAY_MISSING
                    | E_RESIDUAL_PHASE_MISSING
                    | E_COMMON_D_MISSING
                    | E_POLARITY_MISSING
            ),
            "unexpected code {}",
            error.code()
        );
    }
    // 不支持的 phase_ref 语义拒绝（delay_removed 无可机读延迟）。
    let mut input = t63_design_input(&plan);
    t63_first_repeat_mut(&mut input).phase_ref = Some("delay_removed(2.5ms)".to_string());
    assert_eq!(
        evaluate_quality(&plan, &input).unwrap_err().code(),
        E_PHASE_REF_INVALID
    );
}

#[test]
fn t63_positions_quality_unknown_polarity_never_guessed() {
    let plan = t63_plan(6, 3);
    let mut input = t63_design_input(&plan);
    t63_first_repeat_mut(&mut input).polarity = Some(0);
    assert_eq!(
        evaluate_quality(&plan, &input).unwrap_err().code(),
        E_POLARITY_UNKNOWN
    );
    let mut input = t63_design_input(&plan);
    t63_first_repeat_mut(&mut input).polarity = Some(2);
    assert_eq!(
        evaluate_quality(&plan, &input).unwrap_err().code(),
        E_POLARITY_MISSING
    );
}

#[test]
fn t63_positions_quality_repeat_count_and_duplicate_repeat_are_rejected() {
    let plan = t63_plan(6, 3);
    // design/off 要求 >=3：2 遍直接停。
    let input = t63_quality_input(
        &plan,
        QualityScope::Design,
        t63_good_repeats(2),
        RepeatPolicy::DesignOff,
    );
    assert_eq!(
        evaluate_quality(&plan, &input).unwrap_err().code(),
        "E_POS_REPEATS_INSUFFICIENT"
    );
    // on 政策允许 2 遍（但仍需 6 个可用 design 点）。
    let on_report = evaluate_quality(
        &plan,
        &t63_quality_input(
            &plan,
            QualityScope::Design,
            t63_good_repeats(2),
            RepeatPolicy::On,
        ),
    )
    .unwrap();
    assert_eq!(on_report.min_repeats, MIN_ON_REPEATS);
    assert!(on_report.continue_allowed());
    // 重复序号重复 -> 拒绝。
    let mut input = t63_design_input(&plan);
    input.positions[0].measurements[0].repeats[2].repeat = 1;
    assert_eq!(
        evaluate_quality(&plan, &input).unwrap_err().code(),
        "E_POS_REPEATS_DUPLICATE"
    );
    // 重复序号不连续（0,2,3）-> 拒绝。
    let mut input = t63_design_input(&plan);
    input.positions[0].measurements[0].repeats[1].repeat = 5;
    assert_eq!(
        evaluate_quality(&plan, &input).unwrap_err().code(),
        "E_POS_REPEATS_DUPLICATE"
    );
}

// ---------------------------------------------------------------------------
// quality：有值但超门 -> 报告不可用/不继续
// ---------------------------------------------------------------------------

#[test]
fn t63_positions_quality_repeat_spread_and_snr_thresholds_flag_unusable_points() {
    let plan = t63_plan(6, 3);
    let mut input = t63_design_input(&plan);
    input.positions[0].measurements[0].repeats[1].delay_samples = Some(124.0);
    let report = evaluate_quality(&plan, &input).unwrap();
    assert!(!report.continue_allowed());
    assert_eq!(report.points_usable, 5);
    assert!(t63_report_has(&report, Q_DELAY_SPREAD));
    assert!(t63_report_has(&report, Q_DESIGN_POINTS_USABLE));
    assert!(!t63_point(&report, "D0").usable);
    assert!(t63_point(&report, "D0").measurements[0]
        .reasons
        .iter()
        .any(|reason| reason == Q_DELAY_SPREAD));
    assert_eq!(
        quality_stage_failure(&report).unwrap().code,
        Q_QUALITY_INCOMPLETE
    );

    // 幅频极差 0.5 dB > 0.4 dB。
    let mut input = t63_design_input(&plan);
    let mut magnitude = t63_magnitude_base();
    magnitude[3] += 0.5;
    input.positions[1].measurements[2].repeats[1].magnitude_db = Some(magnitude);
    let report = evaluate_quality(&plan, &input).unwrap();
    assert!(!report.continue_allowed());
    assert!(t63_report_has(&report, Q_MAGNITUDE_SPREAD));

    // 全带 SNR / 低带 SNR 低于 25 dB。
    let mut input = t63_design_input(&plan);
    t63_first_repeat_mut(&mut input).snr_db = Some(20.0);
    let report = evaluate_quality(&plan, &input).unwrap();
    assert!(!report.continue_allowed());
    assert!(t63_report_has(&report, Q_SNR_LOW));
    let mut input = t63_design_input(&plan);
    t63_first_repeat_mut(&mut input).snr_lowband_db = Some(18.0);
    let report = evaluate_quality(&plan, &input).unwrap();
    assert!(t63_report_has(&report, Q_SNR_LOWBAND_LOW));

    // 残余相位 >15°、公共 D 残差 >0.5、相干性 <0.9、onset 无效。
    for (edit, code) in [
        (
            (|repeat: &mut RepeatQualityInput| repeat.residual_phase_10k_deg = Some(20.0))
                as fn(&mut RepeatQualityInput),
            Q_RESIDUAL_PHASE_HIGH,
        ),
        (
            |repeat: &mut RepeatQualityInput| repeat.common_d_residual_samples = Some(0.8),
            Q_COMMON_D_RESIDUAL,
        ),
        (
            |repeat: &mut RepeatQualityInput| repeat.coherence = Some(0.5),
            Q_COHERENCE_LOW,
        ),
        (
            |repeat: &mut RepeatQualityInput| repeat.onset_invalid = Some(true),
            Q_ONSET_INVALID,
        ),
    ] {
        let mut input = t63_design_input(&plan);
        edit(t63_first_repeat_mut(&mut input));
        let report = evaluate_quality(&plan, &input).unwrap();
        assert!(t63_report_has(&report, code), "missing {code}");
        assert!(!report.continue_allowed());
    }

    // 重复间极性不一致。
    let mut input = t63_design_input(&plan);
    input.positions[2].measurements[0].repeats[1].polarity = Some(-1);
    let report = evaluate_quality(&plan, &input).unwrap();
    assert!(t63_report_has(&report, Q_POLARITY_INCONSISTENT));
}

#[test]
fn t63_positions_quality_clock_drift_requires_declared_correction() {
    let plan = t63_plan(6, 3);
    // |5.0 ppm| > 2 * 0.1 ppm，未声明校正 -> 不可用。
    let mut input = t63_design_input(&plan);
    let repeat = t63_first_repeat_mut(&mut input);
    repeat.clock_ppm = Some(5.0);
    repeat.clock_ppm_uncertainty_ppm = Some(0.1);
    repeat.clock_drift_corrected = None;
    let report = evaluate_quality(&plan, &input).unwrap();
    assert!(t63_report_has(&report, Q_CLOCK_DRIFT_UNCORRECTED));
    assert!(!report.continue_allowed());
    // 声明已校正 -> 通过。
    let mut input = t63_design_input(&plan);
    let repeat = t63_first_repeat_mut(&mut input);
    repeat.clock_ppm = Some(5.0);
    repeat.clock_ppm_uncertainty_ppm = Some(0.1);
    repeat.clock_drift_corrected = Some(true);
    let report = evaluate_quality(&plan, &input).unwrap();
    assert!(report.continue_allowed());
    // clock_ppm_ok=false -> clock 无效，不可用。
    let mut input = t63_design_input(&plan);
    t63_first_repeat_mut(&mut input).clock_ppm_ok = Some(false);
    let report = evaluate_quality(&plan, &input).unwrap();
    assert!(t63_report_has(&report, Q_CLOCK_INVALID));
}

#[test]
fn t63_positions_quality_residual_phase_window_and_coherence_rules() {
    let plan = t63_plan(6, 3);
    // 3 遍、5 s、sigma 0.5 ppm：不满足 10.4s/sigma<=0.4、也不满足 >=5 遍或 >=60s -> 窗口不足。
    let mut input = t63_design_input(&plan);
    let repeat = t63_first_repeat_mut(&mut input);
    repeat.residual_phase_span_seconds = Some(5.0);
    repeat.residual_phase_sigma_ppm = Some(0.5);
    let report = evaluate_quality(&plan, &input).unwrap();
    assert!(t63_report_has(&report, Q_RESIDUAL_PHASE_WINDOW));
    // 同样 5 s 但 0.2 ppm（<=0.4）仍不足 10.4 s 跨度，除非 5 遍。
    let mut input = t63_design_input(&plan);
    let repeat = t63_first_repeat_mut(&mut input);
    repeat.residual_phase_span_seconds = Some(5.0);
    repeat.residual_phase_sigma_ppm = Some(0.2);
    let report = evaluate_quality(&plan, &input).unwrap();
    assert!(t63_report_has(&report, Q_RESIDUAL_PHASE_WINDOW));
    // 5 遍（policy On 允许）时 5 s 跨度可接受。
    let mut repeats = t63_good_repeats(5);
    for repeat in &mut repeats {
        repeat.residual_phase_span_seconds = Some(5.0);
        repeat.residual_phase_sigma_ppm = Some(0.2);
    }
    let report = evaluate_quality(
        &plan,
        &t63_quality_input(&plan, QualityScope::Design, repeats, RepeatPolicy::On),
    )
    .unwrap();
    assert!(report.continue_allowed());
    assert!(!t63_report_has(&report, Q_RESIDUAL_PHASE_WINDOW));
    // 相干性可选：缺省不算门失败，提供且 <0.9 才算。
    let mut input = t63_design_input(&plan);
    for repeat in &mut input.positions[0].measurements[0].repeats {
        repeat.coherence = None;
    }
    let report = evaluate_quality(&plan, &input).unwrap();
    assert!(report.continue_allowed());
    assert!(!t63_point(&report, "D0").measurements[0].coherence_available);
}

// ---------------------------------------------------------------------------
// design/validation 分组隔离（防 holdout 污染）
// ---------------------------------------------------------------------------

#[test]
fn t63_positions_quality_design_scope_rejects_holdout_inputs() {
    let plan = t63_plan(6, 3);
    // 把 validation 测点塞进 design scope：即使指标全好也必须拒收。
    let mut input = t63_design_input(&plan);
    input.positions[0].label = "V0".to_string();
    let error = evaluate_quality(&plan, &input).unwrap_err();
    assert_eq!(error.code(), "E_POS_GROUP_LEAK");
    assert!(error.to_string().contains("holdout contamination"));
    // 反向：design 测点进 validation scope 同样拒收。
    let mut input = t63_validation_input(&plan);
    input.positions[0].label = "D0".to_string();
    assert_eq!(
        evaluate_quality(&plan, &input).unwrap_err().code(),
        "E_POS_GROUP_LEAK"
    );
    // 不存在的 label 单独报 unknown（不是 group leak）。
    let mut input = t63_design_input(&plan);
    input.positions[0].label = "X9".to_string();
    assert_eq!(
        evaluate_quality(&plan, &input).unwrap_err().code(),
        "E_POS_POSITION_UNKNOWN"
    );
}

#[test]
fn t63_positions_quality_rejects_unknown_or_missing_positions_and_participants() {
    let plan = t63_plan(6, 3);
    // 缺测点。
    let mut input = t63_design_input(&plan);
    input.positions.pop();
    assert_eq!(
        evaluate_quality(&plan, &input).unwrap_err().code(),
        "E_POS_POSITION_MISSING"
    );
    // 重复测点。
    let mut input = t63_design_input(&plan);
    let duplicate = input.positions[0].clone();
    input.positions.push(duplicate);
    assert_eq!(
        evaluate_quality(&plan, &input).unwrap_err().code(),
        "E_POS_POSITION_DUPLICATE"
    );
    // 缺 participant measurement。
    let mut input = t63_design_input(&plan);
    input.positions[0].measurements.pop();
    assert_eq!(
        evaluate_quality(&plan, &input).unwrap_err().code(),
        "E_POS_PARTICIPANT_MISSING"
    );
    // 未知 participant。
    let mut input = t63_design_input(&plan);
    let mut measurement = MeasurementQualityInput::new("XX", t63_grid());
    for repeat in t63_good_repeats(MIN_DESIGN_REPEATS) {
        measurement.push_repeat(repeat);
    }
    input.positions[0].measurements.push(measurement);
    assert_eq!(
        evaluate_quality(&plan, &input).unwrap_err().code(),
        "E_POS_PARTICIPANT_UNKNOWN"
    );
    // 重复 participant。
    let mut input = t63_design_input(&plan);
    let duplicate = input.positions[0].measurements[0].clone();
    input.positions[0].measurements.push(duplicate);
    assert_eq!(
        evaluate_quality(&plan, &input).unwrap_err().code(),
        "E_POS_PARTICIPANT_DUPLICATE"
    );
}

// ---------------------------------------------------------------------------
// quality artifact / 重启恢复
// ---------------------------------------------------------------------------

#[test]
fn t63_positions_quality_json_is_deterministic_and_stale_plan_rejected() {
    let plan = t63_plan(6, 3);
    let report = t63_design_report(&plan);
    let json_a = report.to_json();
    let json_b = report.to_json();
    assert_eq!(json_a, json_b);
    assert!(json_a.contains("\"schema\":\"tspi-calibration-position-quality/v1\""));
    assert!(json_a.contains("\"scope\":\"design\""));
    assert!(json_a.contains("\"continue_allowed\":true"));
    let summary = QualityArtifactSummary::from_json(&json_a).unwrap();
    assert_eq!(summary.scope, QualityScope::Design);
    assert_eq!(summary.points_total, 6);
    assert_eq!(summary.points_usable, 6);
    assert!(summary.continue_allowed);
    summary.verify_against_plan(&plan).unwrap();
    // plan 变了 -> stale（先构造一个 label 不同的 plan）。
    let mut positions = Vec::new();
    for index in 0..6 {
        positions.push(
            MultipointPosition::design(format!("Q{index}"), 1.0)
                .unwrap()
                .with_mic_orientation(T63_MIC_ORIENTATION),
        );
    }
    for index in 0..3 {
        positions.push(
            MultipointPosition::validation(format!("W{index}"), 1.0)
                .unwrap()
                .with_mic_orientation(T63_MIC_ORIENTATION),
        );
    }
    let other_plan = MultipointPlan::new(
        T63_SESSION,
        "4.1.2",
        48_000,
        t63_participants(),
        CommonD0::new(137).unwrap(),
        T63_PHASE_REF,
        positions,
    )
    .unwrap();
    assert_ne!(other_plan.plan_sha256(), plan.plan_sha256());
    assert_eq!(
        summary.verify_against_plan(&other_plan).unwrap_err().code(),
        "E_POS_QUALITY_STALE"
    );
    // artifact 内 plan_sha256 被篡改 -> stale。
    let tampered = json_a.replace(&plan.plan_sha256(), &"f".repeat(64));
    let tampered_summary = QualityArtifactSummary::from_json(&tampered).unwrap();
    assert_eq!(
        tampered_summary
            .verify_against_plan(&plan)
            .unwrap_err()
            .code(),
        "E_POS_QUALITY_STALE"
    );
    // 内部计数篡改 -> artifact hash/一致性错误。
    let bad_counts = json_a.replace("\"points_usable\":6", "\"points_usable\":5");
    assert!(QualityArtifactSummary::from_json(&bad_counts).is_err());
    // 非本 schema 拒绝。
    assert_eq!(
        QualityArtifactSummary::from_json("{}").unwrap_err().code(),
        "E_POS_JSON"
    );
}

#[test]
fn t63_positions_resume_state_loads_artifacts_after_restart() {
    let plan = t63_plan(6, 3);
    let design = t63_design_report(&plan);
    let validation = t63_validation_report(&plan);
    let plan_json = plan.to_json();
    let design_json = design.to_json();
    let validation_json = validation.to_json();
    let state = load_resume_state(
        plan_json.as_bytes(),
        &[design_json.as_bytes(), validation_json.as_bytes()],
    )
    .unwrap();
    assert_eq!(state.plan, plan);
    assert_eq!(state.quality.len(), 2);
    assert!(state.all_scopes_passed());
    assert!(
        state
            .quality_for(QualityScope::Design)
            .unwrap()
            .continue_allowed
    );
    assert!(
        state
            .quality_for(QualityScope::Validation)
            .unwrap()
            .continue_allowed
    );
    // 只有 plan 也能恢复（quality 尚未产出：继续是否允许由上层决定）。
    let state = load_resume_state(plan_json.as_bytes(), &[]).unwrap();
    assert_eq!(state.plan, plan);
    assert!(!state.all_scopes_passed());
    // plan artifact 损坏 -> 恢复失败，不静默开始。
    assert!(load_resume_state(b"garbage", &[]).is_err());
    // scope 重复 -> 拒绝（无法判定哪一个有效）。
    assert_eq!(
        load_resume_state(
            plan_json.as_bytes(),
            &[design_json.as_bytes(), design_json.as_bytes()],
        )
        .unwrap_err()
        .code(),
        "E_POS_CONFIG"
    );
    // quality artifact 属于别的 plan -> stale。
    let other_plan = {
        let mut positions = Vec::new();
        for index in 0..6 {
            positions.push(
                MultipointPosition::design(format!("Z{index}"), 1.0)
                    .unwrap()
                    .with_mic_orientation(T63_MIC_ORIENTATION),
            );
        }
        for index in 0..3 {
            positions.push(
                MultipointPosition::validation(format!("Y{index}"), 1.0)
                    .unwrap()
                    .with_mic_orientation(T63_MIC_ORIENTATION),
            );
        }
        MultipointPlan::new(
            T63_SESSION,
            "4.1.2",
            48_000,
            t63_participants(),
            CommonD0::new(137).unwrap(),
            T63_PHASE_REF,
            positions,
        )
        .unwrap()
    };
    assert_eq!(
        load_resume_state(other_plan.to_json().as_bytes(), &[design_json.as_bytes()],)
            .unwrap_err()
            .code(),
        "E_POS_QUALITY_STALE"
    );
}

// ---------------------------------------------------------------------------
// job 可恢复：QualityCheck/Holdout stage 写 artifact + 重启读回
// ---------------------------------------------------------------------------

struct T63Safety;

impl SafeOutputController for T63Safety {
    fn acquire_calibration_owner(&self) -> Result<(), SafetyError> {
        Ok(())
    }

    fn release_calibration_owner(&self, _disposition: ExitDisposition) -> Result<(), SafetyError> {
        Ok(())
    }

    fn restore_last_good(&self) -> Result<String, SafetyError> {
        Ok("t63-last-good".to_string())
    }

    fn activate_safe_mute(&self) -> Result<(), SafetyError> {
        Ok(())
    }

    fn last_good_version(&self) -> Option<String> {
        Some("t63-last-good".to_string())
    }
}

struct T63Backend {
    plan: MultipointPlan,
    design: PositionQualityReport,
    validation: PositionQualityReport,
}

impl CalibrationJobBackend for T63Backend {
    fn run_stage(&mut self, stage: JobStage, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        match stage {
            JobStage::QualityCheck => {
                write_positions_stage_artifacts(
                    ctx,
                    &self.plan,
                    std::slice::from_ref(&self.design),
                )?;
            }
            JobStage::IndependentHoldout => {
                write_positions_stage_artifacts(
                    ctx,
                    &self.plan,
                    std::slice::from_ref(&self.validation),
                )?;
            }
            _ => {}
        }
        Ok(())
    }
}

fn t63_job_root(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let number = COUNTER.fetch_add(1, Ordering::SeqCst);
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/t63-job-tests")
        .join(format!("{tag}-{}-{number}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

fn t63_job_config(root: &Path) -> JobManagerConfig {
    let state_dir = root.join("state");
    fs::create_dir_all(&state_dir).unwrap();
    JobManagerConfig {
        artifacts_root: root.join("artifacts"),
        checkpoint_path: Some(state_dir.join(CHECKPOINT_FILE_NAME)),
        apply_confirm_timeout: None,
    }
}

fn t63_full_request() -> JobRequest {
    JobRequest {
        kind: JobKind::Full,
        label: "T63 multipoint".to_string(),
        layout: "4.1.2".to_string(),
        sample_rate: 48_000,
        design_points: T6_MIN_DESIGN_POINTS as u32,
        holdout_points: T6_MIN_HOLDOUT_POINTS as u32,
        repeats: MIN_DESIGN_REPEATS,
        drive_level_dbfs: -30.0,
        audible_sweep_authorized: true,
        auto_apply: true,
        expected_applied_version: None,
        inputs: Vec::new(),
    }
}

#[test]
fn t63_positions_job_stage_writes_artifacts_and_restart_resume() {
    let plan = t63_plan(6, 3);
    let design = t63_design_report(&plan);
    let validation = t63_validation_report(&plan);
    let root = t63_job_root("job");
    let safety: Arc<T63Safety> = Arc::new(T63Safety);
    let manager = JobManager::new(t63_job_config(&root), safety.clone()).unwrap();
    let job_id = manager
        .start(
            t63_full_request(),
            Box::new(T63Backend {
                plan: plan.clone(),
                design,
                validation,
            }),
        )
        .unwrap();
    let terminal = manager.wait_terminal(Duration::from_secs(20)).unwrap();
    assert_eq!(terminal.job_id, job_id);
    assert_eq!(terminal.state, JobState::Complete);
    let artifacts_dir = PathBuf::from(&terminal.artifacts_dir);
    for relative in [
        PLAN_ARTIFACT_PATH,
        QUALITY_DESIGN_ARTIFACT_PATH,
        QUALITY_VALIDATION_ARTIFACT_PATH,
    ] {
        assert!(
            artifacts_dir.join(relative).exists(),
            "missing artifact {relative}"
        );
    }
    let manifest = fs::read_to_string(artifacts_dir.join(ARTIFACT_MANIFEST_FILE_NAME)).unwrap();
    for artifact in &terminal.artifacts {
        assert!(manifest.contains(&artifact.sha256));
        let bytes = fs::read(artifacts_dir.join(&artifact.relative_path)).unwrap();
        assert_eq!(sha256_hex(&bytes), artifact.sha256);
    }
    // 模拟进程重启：新 manager 从 checkpoint 恢复完成态，artifact 从盘重建。
    let plan_bytes = fs::read(artifacts_dir.join(PLAN_ARTIFACT_PATH)).unwrap();
    let design_bytes = fs::read(artifacts_dir.join(QUALITY_DESIGN_ARTIFACT_PATH)).unwrap();
    let validation_bytes = fs::read(artifacts_dir.join(QUALITY_VALIDATION_ARTIFACT_PATH)).unwrap();
    manager.shutdown(Duration::from_secs(5)).unwrap();
    let restarted = JobManager::new(t63_job_config(&root), safety).unwrap();
    assert!(restarted.pending_resume().is_none());
    assert_eq!(
        restarted.last_finished().map(|snapshot| snapshot.state),
        Some(JobState::Complete)
    );
    let resume = load_resume_state(
        &plan_bytes,
        &[design_bytes.as_slice(), validation_bytes.as_slice()],
    )
    .unwrap();
    assert_eq!(resume.plan, plan);
    assert!(resume.all_scopes_passed());
    restarted.shutdown(Duration::from_secs(5)).unwrap();
    let _ = fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// 契约样例（设置 TSPI_T63_EVIDENCE_DIR 时才写盘；CI 默认无副作用）
// ---------------------------------------------------------------------------

#[test]
fn t63_positions_writes_evidence_examples_when_requested() {
    let Some(directory) = std::env::var_os("TSPI_T63_EVIDENCE_DIR").map(PathBuf::from) else {
        return;
    };
    fs::create_dir_all(&directory).unwrap();
    let plan = t63_plan(6, 3);
    let design = t63_design_report(&plan);
    let validation = t63_validation_report(&plan);
    let plan_json = plan.to_json();
    let design_json = design.to_json();
    let validation_json = validation.to_json();
    fs::write(directory.join("positions-plan-example.json"), &plan_json).unwrap();
    fs::write(
        directory.join("positions-quality-design-example.json"),
        &design_json,
    )
    .unwrap();
    fs::write(
        directory.join("positions-quality-validation-example.json"),
        &validation_json,
    )
    .unwrap();
    let resume = load_resume_state(
        plan_json.as_bytes(),
        &[design_json.as_bytes(), validation_json.as_bytes()],
    )
    .unwrap();
    assert!(resume.all_scopes_passed());
    let summary = format!(
        "{{\"schema\":\"tspi-calibration-positions-resume-example/v1\",\"plan_sha256\":\"{}\",\"design_continue_allowed\":{},\"validation_continue_allowed\":{},\"design_points\":{},\"validation_points\":{}}}",
        plan.plan_sha256(),
        resume
            .quality_for(QualityScope::Design)
            .map(|summary| summary.continue_allowed)
            .unwrap_or(false),
        resume
            .quality_for(QualityScope::Validation)
            .map(|summary| summary.continue_allowed)
            .unwrap_or(false),
        plan.design_count(),
        plan.validation_count()
    );
    fs::write(directory.join("positions-resume-summary.json"), summary).unwrap();
}
