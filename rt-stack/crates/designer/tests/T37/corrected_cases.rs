// T37 私有测试：corrected/lowdelay 显式幅相目标 + 因果约束 + 独立响应验证。
//
// 本文件由 crates/designer/src/corrected.rs 的 #[cfg(test)] 子模块通过 include! 编译执行
// （不改 integrator 独占的 public_registration.rs；见 tests/T37/README.md）。
// 运行：cargo test -p tspi-fir-designer corrected
//
// 独立 oracle 说明：
// - 目标构造用 T11 import_json_bytes 真实导入（不是 mock 目标结构）；
// - 响应验证调用 corrected::verify（直接 DTFT + 解析 GD），不复用 LS 中间量；
// - min-phase 替代反例用 min_phase_from_magnitude_table 独立构造，再用 verify 复算；
// - 数值期望来自 evidence/T37（Python 原型与 Rust 实测交叉记录，非自证）。

use crate::targets::{import_json_bytes, ImportOptions, TargetCurve};

const FS: f64 = 48_000.0;
const N_POINTS: usize = 480;

// ---------------------------------------------------------------------------
// 目标构造小工具
// ---------------------------------------------------------------------------

fn flat(_f: f64) -> f64 {
    0.0
}

fn shelf(f: f64) -> f64 {
    -6.0 / (1.0 + (f / 300.0).powi(2)).sqrt()
}

// 相位起伏用 f 的正弦（GD 有界，保证目标因果）：GD = -(fs/360)·dφ/df
// A=10/25/100 deg、P=4800 Hz 时 GD 起伏约 ±1.75/±4.36/±17.45 samples。
fn wave10(f: f64) -> f64 {
    10.0 * (2.0 * PI * f / 4_800.0).sin()
}

fn wave25(f: f64) -> f64 {
    25.0 * (2.0 * PI * f / 4_800.0).sin()
}

fn wave100(f: f64) -> f64 {
    100.0 * (2.0 * PI * f / 4_800.0).sin()
}

fn wrap180(mut deg: f64) -> f64 {
    while deg > 180.0 {
        deg -= 360.0;
    }
    while deg <= -180.0 {
        deg += 360.0;
    }
    deg
}

fn log_freqs(n: usize) -> Vec<f64> {
    (0..n)
        .map(|index| 20.0 * 1000.0f64.powf(index as f64 / (n - 1) as f64))
        .collect()
}

/// 构造 tspi-target-draft/v1 单 JSON（480 点，20 Hz–20 kHz，log-f 网格）。
///
/// phase_ref: None=仅幅度；Some("absolute")=phase_fn 返回总相位；
/// Some("delay_removed")=phase_fn 返回 φ_ex，d_target 写 target_delay_samples。
fn build_target_json(
    fs: f64,
    mag_fn: &dyn Fn(f64) -> f64,
    phase_ref: Option<&str>,
    d_target: Option<u32>,
    phase_fn: Option<&dyn Fn(f64) -> f64>,
) -> String {
    let freqs = log_freqs(N_POINTS);
    let mut points = String::from("[");
    for (index, freq) in freqs.iter().enumerate() {
        if index > 0 {
            points.push(',');
        }
        let mag = mag_fn(*freq);
        if phase_ref.is_some() {
            let raw = phase_fn.expect("有 phase 时必须给 phase_fn")(*freq);
            points.push_str(&format!(
                "{{\"freq_hz\":{:.12},\"mag_db\":{:.12},\"phase_deg\":{:.12}}}",
                freq,
                mag,
                wrap180(raw)
            ));
        } else {
            points.push_str(&format!(
                "{{\"freq_hz\":{:.12},\"mag_db\":{:.12}}}",
                freq, mag
            ));
        }
    }
    points.push(']');
    let mut json = String::from("{");
    json.push_str("\"schema\":\"tspi-target-draft/v1\",");
    json.push_str("\"frequency_unit\":\"Hz\",");
    json.push_str("\"magnitude_unit\":\"dB\",");
    match phase_ref {
        None => {}
        Some(reference) => {
            json.push_str("\"phase_unit\":\"deg\",");
            json.push_str(&format!("\"phase_ref\":\"{reference}\","));
            json.push_str("\"unwrap_policy\":\"auto\",");
            if reference == "delay_removed" {
                let delay = d_target.expect("delay_removed 必须给 d_target");
                json.push_str(&format!("\"target_delay_samples\":{delay},"));
            }
        }
    }
    json.push_str(&format!("\"runtime_rate\":{},", fs as u64));
    json.push_str("\"normalization\":\"none\",");
    json.push_str("\"interpolation\":\"log_frequency_linear\",");
    json.push_str("\"edge_policy\":\"hold\",");
    json.push_str(&format!("\"points\":{points}"));
    json.push('}');
    json
}

fn import_target(json: &str, fs: f64) -> TargetCurve {
    import_json_bytes(json.as_bytes(), &ImportOptions::new(fs)).expect("T11 目标导入应成功")
}

fn design_compact(
    spec: &CorrectedSpec,
    target: &TargetCurve,
) -> Result<CorrectedDesign, CorrectedError> {
    design_with(spec, target, &DesignOptions::compact(spec.taps))
}

fn delay_removed_flat_target(d: u32) -> TargetCurve {
    let phase = move |f: f64| -> f64 { 0.0 * f + 0.0 };
    let json = build_target_json(FS, &flat, Some("delay_removed"), Some(d), Some(&phase));
    import_target(&json, FS)
}

fn complex_target_delay_removed(
    mag_fn: &dyn Fn(f64) -> f64,
    phase_fn: &dyn Fn(f64) -> f64,
    d: u32,
) -> TargetCurve {
    let json = build_target_json(FS, mag_fn, Some("delay_removed"), Some(d), Some(phase_fn));
    import_target(&json, FS)
}

// ---------------------------------------------------------------------------
// 1) 目标必须带显式相位 / magnitude-only 降级
// ---------------------------------------------------------------------------

#[test]
fn t37_rejects_magnitude_only_target_by_default() {
    let json = build_target_json(FS, &flat, None, None, None);
    let target = import_target(&json, FS);
    let spec = CorrectedSpec::corrected(FS, 128, 64, [20.0, 20_000.0]);
    let error = design_compact(&spec, &target).unwrap_err();
    assert_eq!(error.code, "E_TGT_PHASE_UNSUPPORTED", "{}", error);
    assert!(
        error.message.contains("必须有相位目标"),
        "{}",
        error.message
    );
}

#[test]
fn t37_magnitude_only_allowed_reports_not_equivalent() {
    let json = build_target_json(FS, &flat, None, None, None);
    let target = import_target(&json, FS);
    let spec =
        CorrectedSpec::corrected(FS, 128, 64, [20.0, 20_000.0]).with_allow_magnitude_only(true);
    let design = design_compact(&spec, &target).expect("显式允许幅度-only 应可设计");
    assert!(design.magnitude_only);
    assert_eq!(
        design.not_equivalent_statement,
        NOT_EQUIVALENT_MAGNITUDE_ONLY
    );
    assert_eq!(design.target_phase_class.as_str(), "none");
    assert!(design.verification.max_phase_error_deg.is_none());
    assert!(design.verification.checks.phase_error);
    assert!(
        design.verification.passed(),
        "{:?}",
        design.verification.failures
    );
    assert!(design.accepted());
    assert!(!design.equivalence.minimum_phase_equivalent);
    assert!(!design.equivalence.measured_complex_matrix_equivalent);
    assert!(design
        .equivalence
        .note
        .contains("not equivalent to measured complex matrix"));
}

// ---------------------------------------------------------------------------
// 2) absolute 相位目标 + 纯延迟：显式幅相拟合
// ---------------------------------------------------------------------------

#[test]
fn t37_absolute_phase_pure_delay_design_meets_limits() {
    let phase: &dyn Fn(f64) -> f64 = &|f: f64| -360.0 * f * 64.0 / FS;
    let json = build_target_json(FS, &flat, Some("absolute"), None, Some(phase));
    let target = import_target(&json, FS);
    let spec = CorrectedSpec::corrected(FS, 128, 64, [20.0, 20_000.0]);
    let design = design_compact(&spec, &target).expect("design");
    assert!(design.accepted(), "{:?}", design.verification.failures);
    assert_eq!(design.target_phase_class.as_str(), "absolute");
    assert_eq!(design.family, "corrected");
    assert!(design.verification.max_mag_error_db < 0.05);
    let phase_error = design
        .verification
        .max_phase_error_deg
        .expect("phase error");
    assert!(phase_error < 0.5, "phase error = {phase_error}");
    assert!(design.verification.checks.passed());
    let gd = design.verification.target_gd.expect("target gd");
    assert!((gd.min_samples - 64.0).abs() < 0.5, "target gd {gd:?}");
    assert!((gd.max_samples - 64.0).abs() < 0.5, "target gd {gd:?}");
    assert_eq!(design.verification.delay_samples, 64);
    assert!((design.delay_ms() - 1.333_333).abs() < 1e-5);
    // 严格入口在可达时应返回 Ok。
    let strict = design_strict(&spec, &target).expect("strict design");
    assert!(strict.accepted());
}

// ---------------------------------------------------------------------------
// 3) delay_removed：D 必须等于 target_delay_samples（§3.6 判据 1）
// ---------------------------------------------------------------------------

#[test]
fn t37_delay_removed_requires_exact_d() {
    let target = delay_removed_flat_target(64);
    let mismatch = CorrectedSpec::corrected(FS, 128, 32, [20.0, 20_000.0]);
    let error = design_compact(&mismatch, &target).unwrap_err();
    assert_eq!(error.code, "E_TGT_DELAY", "{}", error);
    assert!(
        error.message.contains("target_delay_samples=64"),
        "{}",
        error.message
    );

    let spec = CorrectedSpec::corrected(FS, 128, 64, [20.0, 20_000.0]);
    let design = design_compact(&spec, &target).expect("matching D");
    assert!(design.accepted(), "{:?}", design.verification.failures);
    assert_eq!(design.applied_delay_samples, 64);
    assert_eq!(design.target_audit.target_delay_samples, Some(64));
    assert_eq!(design.verification.delay_samples, 64);
    assert!(design.verification.checks.delay_consistency);
    assert!(design.verification.target_gd.expect("gd").min_samples > 60.0);
}

// ---------------------------------------------------------------------------
// 4) 因果约束：目标在声明 D 下 GD 为负 -> E_FIR_NONCAUSAL
// ---------------------------------------------------------------------------

#[test]
fn t37_non_causal_target_rejected() {
    // absolute 目标：φ = +360·f·K/fs，GD = −K samples，无法因果。
    let phase: &dyn Fn(f64) -> f64 = &|f: f64| 360.0 * f * 10.0 / FS;
    let json = build_target_json(FS, &flat, Some("absolute"), None, Some(phase));
    let target = import_target(&json, FS);
    let spec = CorrectedSpec::corrected(FS, 128, 0, [20.0, 20_000.0]);
    let error = design_compact(&spec, &target).unwrap_err();
    assert_eq!(error.code, "E_FIR_NONCAUSAL", "{}", error);
    assert!(error.message.contains("不可因果"), "{}", error.message);

    // delay_removed 目标：φ_ex 正斜率（GD_ex<0）且 D=0 -> 总 GD 仍为负。
    let phase_ex: &dyn Fn(f64) -> f64 = &|f: f64| 360.0 * f * 10.0 / FS;
    let json = build_target_json(FS, &flat, Some("delay_removed"), Some(0), Some(phase_ex));
    let target = import_target(&json, FS);
    let spec = CorrectedSpec::corrected(FS, 128, 0, [20.0, 20_000.0]);
    let error = design_compact(&spec, &target).unwrap_err();
    assert_eq!(error.code, "E_FIR_NONCAUSAL", "{}", error);
}

// ---------------------------------------------------------------------------
// 5) 不得冒充 linear/min 等价（§3.6 判据 6 / §3.8）
// ---------------------------------------------------------------------------

#[test]
fn t37_equivalence_claims_rejected() {
    let target = delay_removed_flat_target(64);
    for claim in [
        EquivalenceClaim::LinearPhase,
        EquivalenceClaim::MinimumPhase,
    ] {
        let spec =
            CorrectedSpec::corrected(FS, 128, 64, [20.0, 20_000.0]).with_equivalence_claim(claim);
        let error = validate_spec(&spec).unwrap_err();
        assert_eq!(error.code, "E_FIR_EQUIV_CLAIM", "claim {claim:?}: {error}");
        // design 也必须在接触目标前拒收（错误发生顺序稳定）。
        let error = design_compact(&spec, &target).unwrap_err();
        assert_eq!(error.code, "E_FIR_EQUIV_CLAIM");
    }
}

#[test]
fn t37_min_phase_substitution_fails_phase_where_corrected_passes() {
    let target = delay_removed_flat_target(64);
    let spec = CorrectedSpec::corrected(FS, 128, 64, [20.0, 20_000.0]);
    let design = design_compact(&spec, &target).expect("corrected design");
    assert!(design.accepted(), "{:?}", design.verification.failures);
    let corrected_phase_error = design.verification.max_phase_error_deg.expect("phase");
    assert!(
        corrected_phase_error < 0.5,
        "corrected phase error {corrected_phase_error}"
    );

    // 用目标幅度构造最小相位替代（诊断/oracle），相位与显式延迟目标完全不同。
    let freqs = log_freqs(N_POINTS);
    let mags: Vec<f64> = freqs.iter().map(|f| flat(*f)).collect();
    let min_phase = min_phase_from_magnitude_table(&freqs, &mags, FS, 128, 8_192)
        .expect("min-phase diagnostic");
    let verification = verify(&spec, &target, &min_phase).expect("verify min-phase");
    assert!(!verification.checks.phase_error);
    let min_phase_error = verification.max_phase_error_deg.expect("phase");
    assert!(
        min_phase_error > 30.0,
        "min-phase phase error = {min_phase_error}"
    );
    assert!(verification
        .failures
        .iter()
        .any(|failure| failure.contains("phase_error")));
    // corrected 设计本身不是最小相位（纯延迟的相位与 min-phase 不一致），且报告不声明等价。
    assert!(!design.equivalence.minimum_phase_equivalent);
    assert!(!design.equivalence.measured_complex_matrix_equivalent);
    assert!(
        design
            .equivalence
            .minimum_phase_consistency_deg
            .expect("consistency")
            > 2.0
    );
    assert!(design
        .equivalence
        .note
        .contains("not a minimum-phase or linear-phase reformulation"));
}

// ---------------------------------------------------------------------------
// 6) 群延迟事实报告（频变）与 ringing/support/out-of-band
// ---------------------------------------------------------------------------

#[test]
fn t37_group_delay_reported_frequency_varying() {
    let target = complex_target_delay_removed(&shelf, &wave10, 64);
    let spec = CorrectedSpec::corrected(FS, 256, 64, [20.0, 20_000.0]);
    let design = design_compact(&spec, &target).expect("design");
    assert!(design.accepted(), "{:?}", design.verification.failures);
    let gd = &design.verification.group_delay;
    assert!(gd.min_samples.is_finite() && gd.max_samples.is_finite());
    assert!(gd.at_1khz_samples.is_finite());
    assert_eq!(gd.class, GroupDelayClass::FrequencyVarying, "gd {gd:?}");
    assert_eq!(gd.declared_class, GroupDelayClass::FrequencyVarying);
    assert!(gd.max_samples - gd.min_samples > 0.01);
    assert!(!gd.table_1_3_oct.is_empty());
    assert_eq!(gd.table_1_3_oct.len(), 10);
    assert!(gd
        .table_1_3_oct
        .iter()
        .all(|point| point.gd_samples.is_finite()));
    assert!(design.verification.target_gd.is_some());
    assert!(design.verification.checks.causal_target);
}

#[test]
fn t37_ringing_support_and_out_of_band() {
    let target = delay_removed_flat_target(64);
    let spec = CorrectedSpec::corrected(FS, 128, 64, [200.0, 18_000.0]);
    let design = design_compact(&spec, &target).expect("design");
    assert!(design.accepted(), "{:?}", design.verification.failures);
    let ringing = design.verification.ringing;
    assert_eq!(ringing.exclusion_samples, 16);
    assert!(
        ringing.peak_index >= 48 && ringing.peak_index <= 80,
        "{ringing:?}"
    );
    assert!(ringing.pre_db.is_some() && ringing.post_db.is_some());
    assert!(ringing.pre_energy_db.is_some() && ringing.post_energy_db.is_some());
    let (first, last, effective) = design.effective_support;
    assert!(first <= last && effective >= 1);
    assert!(
        last < design.taps(),
        "有限窗/有效支撑应短于 taps: ({first},{last})"
    );
    assert!(design.onset_ms >= 0.0 && design.onset_ms <= design.delay_ms() + 1.0);
    assert!(
        ringing.peak_index >= 48 && ringing.peak_index <= 80,
        "{ringing:?}"
    );
    assert!(
        design.verification.out_of_band.points > 0,
        "{:?}",
        design.verification.out_of_band
    );
    assert!(design.verification.out_of_band.min_db.is_finite());
}

// ---------------------------------------------------------------------------
// 7) lowdelay：误差-延迟折中扫描 / D 上限 / INFEASIBLE 诚实报告
// ---------------------------------------------------------------------------

#[test]
fn t37_lowdelay_scan_reports_tradeoff() {
    let target = complex_target_delay_removed(&shelf, &wave100, 64);
    let spec = CorrectedSpec::lowdelay(FS, 256, 64, [20.0, 20_000.0]);
    let design = design_compact(&spec, &target).expect("lowdelay design");
    assert!(design.accepted(), "{:?}", design.verification.failures);
    assert_eq!(design.family, "lowdelay");
    assert_eq!(design.applied_delay_samples, 64);
    assert!(design.delay_scan.len() >= 2, "{:?}", design.delay_scan);
    let applied = design
        .delay_scan
        .iter()
        .find(|entry| entry.applied)
        .expect("applied entry");
    assert_eq!(applied.d_samples, 64);
    assert!(applied.meets_limits, "{applied:?}");
    assert!(applied.max_phase_error_deg.expect("phase") <= 5.0);
    let zero = design
        .delay_scan
        .iter()
        .find(|entry| entry.d_samples == 0)
        .expect("D=0 diagnostic entry");
    assert!(!zero.meets_limits, "D=0 should not meet limits: {zero:?}");
    assert!(zero.max_mag_error_db > 1.0);
    // 折中方向：应用 D 的相位误差应不大于 D=0 的相位误差。
    assert!(
        applied.max_phase_error_deg.expect("phase") <= zero.max_phase_error_deg.expect("phase"),
        "applied {applied:?} zero {zero:?}"
    );
    assert!(design.verification.checks.lowdelay_budget);
    // 证据用：--nocapture 时打印误差-延迟折中表。
    println!("--- T37 lowdelay delay_scan (evidence) ---");
    for entry in &design.delay_scan {
        println!(
            "delay_scan D={} applied={} within_delay_max={} max_mag_error={:.6} phase_error={} rms_error={:.6} meets_limits={}",
            entry.d_samples,
            entry.applied,
            entry.within_delay_max,
            entry.max_mag_error_db,
            match entry.max_phase_error_deg {
                Some(value) => format!("{value:.6} deg"),
                None => "n/a".to_string(),
            },
            entry.rms_error_db,
            entry.meets_limits
        );
    }
}

#[test]
fn t37_lowdelay_applied_d_over_max_rejected() {
    let spec = CorrectedSpec::lowdelay(FS, 256, 32, [20.0, 20_000.0]).with_delay(64);
    let error = validate_spec(&spec).unwrap_err();
    assert_eq!(error.code, "E_FIR_DELAY", "{}", error);
    assert!(
        error.message.contains("delay_max_samples=32"),
        "{}",
        error.message
    );

    let mut missing_max = CorrectedSpec::lowdelay(FS, 128, 64, [20.0, 20_000.0]);
    missing_max.delay_max_samples = None;
    let error = validate_spec(&missing_max).unwrap_err();
    assert_eq!(error.code, "E_FIR_DELAY");
}

#[test]
fn t37_lowdelay_infeasible_reports_best_achieved() {
    let target = complex_target_delay_removed(&shelf, &wave25, 64);
    let spec = CorrectedSpec::lowdelay(FS, 64, 64, [20.0, 20_000.0]);
    let design = design_compact(&spec, &target).expect("design returns honest report");
    assert!(!design.accepted(), "64 taps 不可能达到默认门");
    assert!(!design.verification.failures.is_empty());
    assert!(!design.verification.checks.magnitude_error);
    let applied = design
        .delay_scan
        .iter()
        .find(|entry| entry.applied)
        .expect("applied entry");
    assert!(!applied.meets_limits);
    // 证据用：打印 INFEASIBLE 的 best achieved 摘要。
    println!("--- T37 lowdelay INFEASIBLE summary (evidence) ---");
    for line in design.summary_lines() {
        if line.starts_with("delay_scan")
            || line.starts_with("failure")
            || line.starts_with("max_mag_error")
        {
            println!("{line}");
        }
    }
    let error = design_strict(&spec, &target).unwrap_err();
    assert_eq!(error.code, "E_FIR_BUDGET", "{}", error);
    assert!(error.message.contains("INFEASIBLE"), "{}", error.message);
    assert!(error.message.contains("best achieved"), "{}", error.message);
}

#[test]
fn t37_corrected_infeasible_reports_best_achieved() {
    let target = complex_target_delay_removed(&shelf, &wave25, 64);
    let spec = CorrectedSpec::corrected(FS, 64, 64, [20.0, 20_000.0]);
    let design = design_compact(&spec, &target).expect("design returns honest report");
    assert!(!design.accepted());
    assert!(design
        .verification
        .failures
        .iter()
        .any(|failure| failure.contains("magnitude_error")));
    assert_eq!(design.delay_scan.len(), 1);
    assert!(!design.delay_scan[0].meets_limits);
    let error = design_strict(&spec, &target).unwrap_err();
    assert_eq!(error.code, "E_FIR_BUDGET");
    assert!(error.message.contains("INFEASIBLE"));
}

// ---------------------------------------------------------------------------
// 8) 确定性、参考归一化、端点
// ---------------------------------------------------------------------------

#[test]
fn t37_determinism_and_no_hidden_gain() {
    let target = complex_target_delay_removed(&shelf, &wave10, 64);
    let spec = CorrectedSpec::corrected(FS, 128, 64, [20.0, 20_000.0]);
    let first = design_compact(&spec, &target).expect("design 1");
    let second = design_compact(&spec, &target).expect("design 2");
    assert_eq!(first.coeffs, second.coeffs, "同一输入两次设计必须逐位一致");
    assert_eq!(first.target_sha256, second.target_sha256);
    assert_eq!(first.ls.cg_iterations, second.ls.cg_iterations);
    let reference = first.verification.reference;
    assert_eq!(reference.declared_gain_db, reference.measured_gain_db);
    assert!(reference.tolerance_ok);
    assert_eq!(reference.hz, 1_000.0);
    assert!((reference.measured_gain_db - reference.target_gain_db).abs() <= 1.0);
    assert!(first.peak_abs > 0.0 && first.sum_coeffs.is_finite());
    assert!(first.verification.passed());
}

#[test]
fn t37_reference_and_endpoints() {
    let target = delay_removed_flat_target(64);
    let spec = CorrectedSpec::corrected(FS, 128, 64, [20.0, 20_000.0]);
    let design = design_compact(&spec, &target).expect("design");
    let endpoints = design.verification.endpoints;
    assert!(endpoints.f20.is_measured());
    assert!(endpoints.f20k.is_measured(), "{:?}", endpoints.f20k);
    assert_eq!(endpoints.f20.status_str(), "measured");
    assert_eq!(endpoints.f20k.status_str(), "measured");
    assert!(design.verification.reference.tolerance_ok);
    assert_eq!(design.verification.reference.hz, 1_000.0);
}

// ---------------------------------------------------------------------------
// 9) 参数/目标拒收码
// ---------------------------------------------------------------------------

#[test]
fn t37_param_rejections_codes() {
    let expected = |result: Result<ValidatedCorrectedSpec, CorrectedError>, code: &str| {
        let error = result.unwrap_err();
        assert_eq!(error.code, code, "{}", error);
    };
    let base = CorrectedSpec::corrected(FS, 128, 64, [20.0, 20_000.0]);

    expected(
        validate_spec(&CorrectedSpec {
            taps: 7,
            ..base.clone()
        }),
        "E_FIR_TAPS",
    );
    expected(
        validate_spec(&CorrectedSpec {
            taps: 16_385,
            ..base.clone()
        }),
        "E_FIR_TAPS",
    );
    expected(
        validate_spec(&CorrectedSpec {
            fs: f64::NAN,
            ..base.clone()
        }),
        "E_FIR_BAND",
    );
    expected(
        validate_spec(&CorrectedSpec {
            band_hz: [10.0, 1_000.0],
            ..base.clone()
        }),
        "E_FIR_BAND",
    );
    expected(
        validate_spec(&CorrectedSpec {
            band_hz: [1_000.0, 1_000.0],
            ..base.clone()
        }),
        "E_FIR_BAND",
    );
    expected(
        validate_spec(&CorrectedSpec {
            band_hz: [20.0, 20_001.0],
            ..base.clone()
        }),
        "E_FIR_BAND",
    );
    expected(
        validate_spec(&CorrectedSpec {
            fs: 40_000.0,
            ..base.clone()
        }),
        "E_FIR_BAND",
    );
    expected(
        validate_spec(&CorrectedSpec {
            delay_samples: 9_601,
            ..base.clone()
        }),
        "E_FIR_DELAY",
    );
    expected(
        validate_spec(&CorrectedSpec {
            delay_max_samples: Some(32),
            ..base.clone()
        }),
        "E_FIR_FAMILY",
    );
    expected(
        validate_spec(&CorrectedSpec {
            weights: Weighting::TargetSha256,
            ..base.clone()
        }),
        "E_FIR_FAMILY",
    );
    expected(
        validate_spec(&CorrectedSpec {
            equivalence_claim: EquivalenceClaim::MinimumPhase,
            ..base.clone()
        }),
        "E_FIR_EQUIV_CLAIM",
    );
    expected(
        validate_spec(&CorrectedSpec {
            max_mag_error_db: 0.0,
            ..base.clone()
        }),
        "E_FIR_BAND",
    );
    expected(
        validate_spec(&CorrectedSpec {
            rms_error_db: -1.0,
            ..base.clone()
        }),
        "E_FIR_BAND",
    );
    expected(
        validate_spec(&CorrectedSpec {
            ridge: -1.0,
            ..base.clone()
        }),
        "E_FIR_BAND",
    );
    // 合法参数不应报错。
    validate_spec(&base).expect("base spec valid");
}

#[test]
fn t37_target_rate_mismatch_rejected() {
    let phase: &dyn Fn(f64) -> f64 = &|_f: f64| 0.0;
    let json = build_target_json(
        96_000.0,
        &flat,
        Some("delay_removed"),
        Some(64),
        Some(phase),
    );
    let target = import_target(&json, 96_000.0);
    let spec = CorrectedSpec::corrected(FS, 128, 64, [20.0, 20_000.0]);
    let error = design_compact(&spec, &target).unwrap_err();
    assert_eq!(error.code, "E_TGT_RATE", "{}", error);
}

// ---------------------------------------------------------------------------
// 10) 96 kHz、相位 wrap、FFT/CG 报告、normative 网格
// ---------------------------------------------------------------------------

#[test]
fn t37_96k_delay_scaling() {
    let phase: &dyn Fn(f64) -> f64 = &|f: f64| -360.0 * f * 64.0 / 96_000.0;
    let json_96 = build_target_json(96_000.0, &flat, Some("absolute"), None, Some(phase));
    let target_96 = import_target(&json_96, 96_000.0);
    let spec_96 = CorrectedSpec::corrected(96_000.0, 128, 64, [20.0, 20_000.0]);
    let design_96 = design_compact(&spec_96, &target_96).expect("96k design");
    assert!(
        design_96.accepted(),
        "{:?}",
        design_96.verification.failures
    );

    let phase_48: &dyn Fn(f64) -> f64 = &|f: f64| -360.0 * f * 64.0 / FS;
    let json_48 = build_target_json(FS, &flat, Some("absolute"), None, Some(phase_48));
    let target_48 = import_target(&json_48, FS);
    let spec_48 = CorrectedSpec::corrected(FS, 128, 64, [20.0, 20_000.0]);
    let design_48 = design_compact(&spec_48, &target_48).expect("48k design");
    assert!(
        design_48.accepted(),
        "{:?}",
        design_48.verification.failures
    );

    assert!((design_96.delay_ms() - 0.666_667).abs() < 1e-5);
    assert!((design_48.delay_ms() - 1.333_333).abs() < 1e-5);
    assert!((design_96.delay_ms() * 2.0 - design_48.delay_ms()).abs() < 1e-9);
}

#[test]
fn t37_phase_wrap_handling() {
    // 20 kHz 处目标总相位约 −9600 deg：必须按连续相位拟合，wrap 到 (−180,180] 比较。
    let phase: &dyn Fn(f64) -> f64 = &|f: f64| -360.0 * f * 64.0 / FS;
    let json = build_target_json(FS, &flat, Some("absolute"), None, Some(phase));
    let target = import_target(&json, FS);
    let total = target.phase_total_deg().expect("phase total");
    assert!(
        *total.last().expect("last phase") < -1_000.0,
        "目标相位应已展开到连续分支：{:?}",
        total.last()
    );
    let spec = CorrectedSpec::corrected(FS, 128, 64, [20.0, 20_000.0]);
    let design = design_compact(&spec, &target).expect("design");
    assert!(design.accepted(), "{:?}", design.verification.failures);
    let phase_error = design.verification.max_phase_error_deg.expect("phase");
    assert!(
        phase_error < 1.0,
        "wrap 处理错误会使误差接近 180 deg: {phase_error}"
    );
}

#[test]
fn t37_fft_and_cg_report() {
    let target = delay_removed_flat_target(64);
    let spec = CorrectedSpec::corrected(FS, 128, 64, [20.0, 20_000.0]);
    let design = design_compact(&spec, &target).expect("design");
    assert_eq!(design.ls.n_fft, 4_096);
    assert!(design.ls.bins_in_band > 4 * design.taps());
    assert_eq!(design.ls.ridge_used, DEFAULT_RIDGE);
    assert!(design.ls.cg_iterations > 0);
    assert!(design.ls.cg_iterations <= CG_MAX_ITERATIONS);
    assert!(design.ls.cg_relative_residual < 1e-8, "{:?}", design.ls);
    assert!(design.ls.converged);
}

#[test]
fn t37_normative_grid_verification() {
    let target = delay_removed_flat_target(64);
    let spec = CorrectedSpec::corrected(FS, 128, 64, [20.0, 20_000.0]);
    let design = design(&spec, &target).expect("normative design");
    assert!(design.accepted(), "{:?}", design.verification.failures);
    assert!(design.verification.grid_points >= 8_192);
    assert_eq!(design.ls.n_fft, 4_096);
}

// ---------------------------------------------------------------------------
// 11) 预算事实与摘要
// ---------------------------------------------------------------------------

#[test]
fn t37_budget_facts_profile_caps() {
    let regular = budget_facts(&CorrectedSpec::corrected(FS, 1_024, 64, [20.0, 20_000.0]))
        .expect("regular budget");
    assert_eq!(regular.profile, "regular");
    assert!(regular.fits_regular && regular.fits_extended);
    assert_eq!(regular.memory_bytes_f32, 4_096);
    assert_eq!(regular.delay_ms, regular.delay_samples as f64 / FS * 1000.0);
    assert!(!regular.latency_budget_evaluated);

    let extended = budget_facts(&CorrectedSpec::corrected(FS, 8_192, 64, [20.0, 20_000.0]))
        .expect("extended budget");
    assert_eq!(extended.profile, "extended");
    assert!(!extended.fits_regular && extended.fits_extended);
    assert_eq!(extended.memory_bytes_f32, 32_768);

    let max = budget_facts(&CorrectedSpec::corrected(FS, 16_384, 64, [20.0, 20_000.0]))
        .expect("max budget");
    assert_eq!(max.profile, "extended");
    assert!(max.fits_extended);
}

#[test]
fn t37_narrow_band_grid_infeasible() {
    let target = delay_removed_flat_target(64);
    let spec = CorrectedSpec::corrected(FS, 512, 64, [1_000.0, 1_010.0]);
    let error = design_compact(&spec, &target).unwrap_err();
    assert_eq!(error.code, "E_FIR_BUDGET", "{}", error);
    assert!(error.message.contains("INFEASIBLE"), "{}", error.message);
    assert!(error.message.contains("频点"), "{}", error.message);
}

#[test]
fn t37_summary_lines_contain_required_facts() {
    let target = complex_target_delay_removed(&shelf, &wave10, 64);
    let spec = CorrectedSpec::corrected(FS, 128, 64, [20.0, 20_000.0]);
    let design = design_compact(&spec, &target).expect("design");
    let summary = design.summary_lines().join("\n");
    assert!(summary.contains("family=corrected"), "{summary}");
    assert!(summary.contains("target_sha256="), "{summary}");
    assert!(summary.contains("checks passed=true"), "{summary}");
    assert!(
        summary.contains("not_equivalent=explicit amplitude+phase target"),
        "{summary}"
    );
    assert!(
        summary.contains("delay_scan D=64 applied=true"),
        "{summary}"
    );
    assert!(summary.contains("ls n_fft=4096"), "{summary}");
    assert!(summary.contains("group_delay"), "{summary}");
    assert!(summary.contains("gd_1_3oct hz=1000.0"), "{summary}");
    assert!(
        summary.contains("declared_class=frequency_varying"),
        "{summary}"
    );
    assert!(summary.contains("endpoints f20=measured"), "{summary}");
    assert!(
        !summary.contains("failure "),
        "通过设计不应有 failure: {summary}"
    );
    // 证据用：--nocapture 时打印实测摘要（不是断言，不影响 PASS/FAIL）。
    println!("--- T37 summary_lines (evidence) ---");
    for line in design.summary_lines() {
        println!("{line}");
    }
}
