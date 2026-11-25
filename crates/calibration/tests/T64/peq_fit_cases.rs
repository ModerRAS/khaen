// T64 PEQ fit 私有单测（经 src/peq_fit.rs 的 #[cfg(test)] include! 进入单测构建）。
//
// 覆盖：约 10 段模态 PEQ / flat+tilt10 目标 / 目标 FIR / trim+headroom(D7) /
// 基线 P 与 modal 残差单 owner / T63 多点 plan 与质量门 / 契约错误码 / artifact 发布。

use super::*;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use khaen_modal_design::timebase::CommonD0;

use khaen_designer::targets::{import_json_bytes, ImportOptions};

use crate::positions::{
    MultipointPlan, MultipointPosition, PositionQualityReport, QualityScope, RepeatPolicy,
    POSITION_QUALITY_SCHEMA,
};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

/// 8 个已知模态（f0 Hz, Q, gain dB）；其中两个 Q > 10（requirements 8 AC1）。
const FIXTURE_MODES: [(f64, f64, f64); 8] = [
    (24.0, 12.0, 12.0),
    (32.0, 14.0, 11.0),
    (42.0, 10.0, 10.0),
    (55.0, 18.0, 10.0),
    (72.0, 9.0, 9.0),
    (95.0, 8.0, 9.0),
    (130.0, 7.0, 8.0),
    (180.0, 6.0, 7.0),
];

fn plan_6_3(fs: u32) -> MultipointPlan {
    let mut positions = Vec::new();
    // 听音位多点 plan：麦克风朝向必须是 90deg（ACCEPTANCE §0.1.2；T63 plan 校验）。
    for index in 1..=6 {
        positions.push(
            MultipointPosition::design(format!("D{index}"), 1.0)
                .expect("design")
                .with_mic_orientation("90deg"),
        );
    }
    for index in 1..=3 {
        positions.push(
            MultipointPosition::validation(format!("H{index}"), 1.0)
                .expect("holdout")
                .with_mic_orientation("90deg"),
        );
    }
    MultipointPlan::new(
        "t64-session",
        "4.1.2",
        fs,
        vec!["FL".to_string(), "FR".to_string()],
        CommonD0::zero(),
        "aligned_segment",
        positions,
    )
    .expect("plan")
}

fn quality_pass(plan: &MultipointPlan, points_usable: usize) -> PositionQualityReport {
    PositionQualityReport {
        schema: POSITION_QUALITY_SCHEMA,
        plan_sha256: plan.plan_sha256(),
        scope: QualityScope::Design,
        repeat_policy: RepeatPolicy::DesignOff,
        min_repeats: RepeatPolicy::DesignOff.min_repeats(),
        plan_design_total: plan.design_count(),
        plan_validation_total: plan.validation_count(),
        points_total: plan.design_count(),
        points_usable,
        continue_allowed: true,
        reasons: Vec::new(),
        points: Vec::new(),
    }
}

fn quality_blocked(plan: &MultipointPlan) -> PositionQualityReport {
    let mut report = quality_pass(plan, 3);
    report.continue_allowed = false;
    report.reasons = vec!["E_POS_SNR_LOW".to_string()];
    report
}

fn flat_target(fs: u32) -> TargetCurve {
    builtin_target(BuiltinTarget::Flat, &BuiltinOptions::new(fs as f64)).expect("flat")
}

fn tilt10_target(fs: u32) -> TargetCurve {
    builtin_target(BuiltinTarget::Tilt10, &BuiltinOptions::new(fs as f64)).expect("tilt10")
}

/// 非内置目标（F-15 夹具）：20 Hz–20 kHz 61 点 log 网格，1 kHz 附近 +0.75 dB 隆起。
/// sha256 与内置 flat/tilt10 都不同，但曲线本身是合法的幅度目标（应被 E_FIT_TARGET 拒收）。
fn custom_target(fs: u32) -> TargetCurve {
    let count = 61usize;
    let mut points = String::from("[");
    for index in 0..count {
        let ratio = index as f64 / (count as f64 - 1.0);
        let freq = 20.0 * 1000f64.powf(ratio);
        let bump = 0.75 * (-((freq / 1_000.0).log2() / 0.5).powi(2)).exp();
        if index > 0 {
            points.push(',');
        }
        points.push_str(&format!(
            "{{\"freq_hz\":{freq:.6},\"mag_db\":{bump:.9}}}"
        ));
    }
    points.push(']');
    let document = format!(
        "{{\"schema\":\"khaen-target-draft/v1\",\"frequency_unit\":\"Hz\",\
         \"magnitude_unit\":\"dB\",\"runtime_rate\":{fs},\"normalization\":\"none\",\
         \"interpolation\":\"log_frequency_linear\",\"edge_policy\":\"hold\",\
         \"points\":{points}}}"
    );
    import_json_bytes(document.as_bytes(), &ImportOptions::new(fs as f64))
        .expect("custom target 导入应成功")
}

fn synthetic_curve(
    position_index: usize,
    channel_variant: usize,
    fs: f64,
    include_dip: bool,
) -> Vec<CurvePoint> {
    let grid = log_frequency_grid(20.0, 20_000.0, 1.0 / 48.0);
    let mut points = Vec::with_capacity(grid.len());
    for (grid_index, freq) in grid.iter().enumerate() {
        let mut db = 0.0_f64;
        for (mode_index, (f0, q, gain)) in FIXTURE_MODES.iter().enumerate() {
            let key = (position_index + channel_variant + mode_index) % 5;
            let scale = 0.9 + 0.05 * key as f64;
            let detune = 1.0 + 0.004 * (((position_index + mode_index) % 3) as f64 - 1.0);
            let band = ModalBand::from_params(BandType::Peaking, f0 * detune, *q, gain * scale, fs)
                .expect("fixture mode");
            db += band.biquad().magnitude_db(*freq, fs);
        }
        // 宽带隆起（拟合器应能压平）
        db += 2.0 * (-(((*freq / 1_500.0).log2()) / 0.8).powi(2)).exp();
        // 确定性细纹波
        let phase =
            grid_index as f64 * 0.37 + position_index as f64 * 1.1 + channel_variant as f64 * 2.3;
        db += 0.25 * phase.sin() + 0.15 * (phase * 2.1).cos();
        // 位置间小差异
        db += 0.2 * (position_index as f64 - 4.0) * ((*freq / 300.0).ln()).sin();
        if include_dip {
            // 500 Hz 深谷（默认 boost cap 0 时不可填，进入 unfilled_dips）
            let distance = (*freq / 500.0).log2();
            db -= 10.0 * (-(distance * distance) / (2.0 * 0.03 * 0.03)).exp();
        }
        points.push(CurvePoint::new(*freq, db));
    }
    points
}

fn curves_for(plan: &MultipointPlan, channels: &[&str], include_dip: bool) -> Vec<PositionCurve> {
    let mut curves = Vec::new();
    for (channel_index, channel) in channels.iter().enumerate() {
        for (position_index, position) in plan.positions.iter().enumerate() {
            let points = synthetic_curve(
                position_index,
                channel_index,
                plan.sample_rate_hz as f64,
                include_dip,
            );
            curves.push(PositionCurve::new(
                position.label.clone(),
                (*channel).to_string(),
                points,
            ));
        }
    }
    curves
}

fn fast_options() -> PeqFitOptions {
    let mut options = PeqFitOptions::default();
    options.target_fir.taps = 255;
    options
}

fn run_fit<'a>(
    plan: &'a MultipointPlan,
    target: &'a TargetCurve,
    curves: &'a [PositionCurve],
    quality: Option<&'a PositionQualityReport>,
    options: PeqFitOptions,
    created_unix: u64,
) -> Result<PeqFitOutput, PeqFitError> {
    let mut request = PeqFitRequest::new(plan, target, curves)
        .with_options(options)
        .with_created_unix(created_unix);
    if let Some(quality) = quality {
        request = request.with_design_quality(quality);
    }
    fit_peq(&request)
}

fn channel<'a>(report: &'a PeqFitReport, name: &str) -> &'a ChannelFitReport {
    report
        .channels
        .iter()
        .find(|channel| channel.name == name)
        .expect("channel report")
}

fn modal_bands(report: &ChannelFitReport) -> Vec<&BandReport> {
    report.bands.iter().filter(|band| band.modal).collect()
}

fn approx(a: f64, b: f64, tolerance: f64) {
    assert!(
        (a - b).abs() <= tolerance,
        "expected {a} ~= {b} (tolerance {tolerance})"
    );
}

// ---------------------------------------------------------------------------
// 1) flat 目标：约 10 段模态 PEQ、检出与只切
// ---------------------------------------------------------------------------

#[test]
fn t64_peq_fit_flat_modal_detection_and_cut_only() {
    let fs = 48_000;
    let plan = plan_6_3(fs);
    let quality = quality_pass(&plan, 6);
    let target = flat_target(fs);
    let curves = curves_for(&plan, &["FL"], false);
    let output = run_fit(
        &plan,
        &target,
        &curves,
        Some(&quality),
        fast_options(),
        1_700_000_000,
    )
    .expect("fit");
    let report = &output.report;
    assert_eq!(report.target.id, "flat");
    approx(report.target.anchor_db, 0.0, 1e-9);
    let channel = channel(report, "FL");
    let bands = modal_bands(channel);
    assert!(
        bands.len() >= 6 && bands.len() <= 10,
        "modal bands = {}",
        bands.len()
    );
    // 已知模态：检出频率 ±3 Hz（检出集合是 8 个真模态的子集；欠检出只影响残差报告）。
    for band in &bands {
        assert!(
            FIXTURE_MODES
                .iter()
                .any(|(f0, _, _)| (band.freq_hz - f0).abs() <= 3.0),
            "detected band {:.2} Hz 不匹配任何已知模态",
            band.freq_hz
        );
    }
    // 两个 Q>10 的模态（32/55 Hz）必须在检出集合里且不被夹到 10。
    for f0 in [32.0_f64, 55.0] {
        assert!(
            bands.iter().any(|band| (band.freq_hz - f0).abs() <= 3.0),
            "missing Q>10 modal band near {f0} Hz"
        );
    }
    for band in &bands {
        assert!(band.gain_db <= 0.0, "modal band must be cut-only: {band:?}");
        assert!(band.gain_db >= -24.0);
    }
    assert!(
        bands.iter().any(|band| band.q > 10.0),
        "Q>10 模态不得被夹到 10：{:?}",
        bands.iter().map(|band| band.q).collect::<Vec<_>>()
    );
    // 模态残差下降（PEQ 后应明显低于修正前）。
    let residual = channel.modal_residual;
    assert!(
        residual.rms_after_db < residual.rms_before_db - 1.5,
        "modal residual {residual:?}"
    );
    // 只切：无正 boost。
    approx(channel.filter_positive_sum_db, 0.0, 1e-9);
    approx(channel.single_filter_max_boost_db, 0.0, 1e-9);
    // 只切：PEQ 侧无正增益；目标 FIR 允许实现 ripple 的微小正部（cap=0 时 |H| 可能 ~0.02 dB）。
    assert!(
        channel.applied_boost_db <= 0.1,
        "applied boost {:.3}",
        channel.applied_boost_db
    );
    // P 链基线：p_sha256 是 64 hex。
    assert_eq!(report.p_sha256.len(), 64);
    assert!(report.p_sha256.chars().all(|c| c.is_ascii_hexdigit()));
    // 模态残差单 owner：G 残余曲线在模态带内、频率升序。
    assert!(channel.g_residual_curve.len() >= 40);
    let mut previous = 0.0;
    for pair in &channel.g_residual_curve {
        assert!(pair[0] > previous);
        previous = pair[0];
        assert!(pair[0] >= 20.0 && pair[0] <= 250.0 + 1e-6);
    }
}

#[test]
fn t64_peq_fit_flat_residual_meets_wide_band_goal() {
    let fs = 48_000;
    let plan = plan_6_3(fs);
    let quality = quality_pass(&plan, 6);
    let target = flat_target(fs);
    let curves = curves_for(&plan, &["FL"], false);
    let mut options = fast_options();
    options.target_fir.taps = 255;
    let output = run_fit(&plan, &target, &curves, Some(&quality), options, 7).expect("fit");
    let channel = channel(&output.report, "FL");
    let wide = channel.wide_residual_after_p.expect("wide residual");
    assert!(
        wide.rms_after_db <= 1.2,
        "wide residual after P = {:.3} dB (before {:.3})",
        wide.rms_after_db,
        wide.rms_before_db
    );
    assert!(wide.rms_after_db < wide.rms_before_db);
    // 模态带内 P 后残差不应比 PEQ 后显著变差。
    assert!(
        channel.modal_residual_after_p.max_after_db <= channel.modal_residual.max_after_db + 0.75
    );
    // 2a/2b 在合成夹具上应通过（拟合质量门；失败时报告必须诚实标 false）。
    assert!(
        channel.rule_2a_pass,
        "rule 2a failed: {:?}",
        channel.warnings
    );
    assert!(
        channel.rule_2a_peak_over_target_db <= RULE_2A_LIMIT_DB + 1e-6,
        "peak over target {:.3}",
        channel.rule_2a_peak_over_target_db
    );
}

// ---------------------------------------------------------------------------
// 2) tilt10 目标绑定
// ---------------------------------------------------------------------------

#[test]
fn t64_peq_fit_tilt10_target_is_bound_and_reported() {
    let fs = 48_000;
    let plan = plan_6_3(fs);
    let quality = quality_pass(&plan, 6);
    let target = tilt10_target(fs);
    let curves = curves_for(&plan, &["FL"], false);
    let mut options = fast_options();
    options.boost_cap_db = 3.0; // tilt10 低带需要 +boost 才能靠近目标（D7 允许）
    let output = run_fit(&plan, &target, &curves, Some(&quality), options, 8).expect("fit");
    let report = &output.report;
    assert_eq!(report.target.id, "tilt10");
    // tilt10 在 1 kHz 为 -5.663233 dB；fit 以 1 kHz 归一（anchor 记录）。
    approx(report.target.anchor_db, -5.663233, 1e-3);
    assert_eq!(report.target.reference_gain_db, 0.0);
    let channel = channel(report, "FL");
    // 目标倾斜需要宽带/负增益段（shelf 或 peaking），且仍在模态带外。
    assert!(
        channel
            .bands
            .iter()
            .any(|band| !band.modal && band.gain_db < 0.0),
        "tilt10 应有宽带负增益段：{:?}",
        channel.bands
    );
    assert!(
        channel.target_fir_taps == 255,
        "target FIR taps = {}",
        channel.target_fir_taps
    );
    // 拟合后宽带残差相对未修正应有明显改善（boost cap +3 允许低带 shelf 抬升；
    // 单段 |gain|<=6 的 requirements 6 AC4 上限下 tilt 不能被一个 shelf 完全吃掉）。
    let wide = channel.wide_residual_after_p.expect("wide residual");
    assert!(
        wide.rms_after_db <= wide.rms_before_db - 0.8,
        "tilt wide residual {wide:?}"
    );
    assert!(
        wide.max_after_db <= wide.max_before_db - 1.0,
        "tilt wide max residual {wide:?}"
    );
    // D7 单段/总和帽在 tilt 拟合里同样生效。
    assert!(channel.single_filter_max_boost_db <= MAX_BOOST_CAP_DB + 1e-6);
    assert!(channel.filter_positive_sum_db <= D7_POSITIVE_SUM_WARN_DB + 1e-6);
}

// ---------------------------------------------------------------------------
// 3) D7 boost 帽 + unfilled_dips
// ---------------------------------------------------------------------------

#[test]
fn t64_peq_fit_boost_cap_is_clamped_and_dips_are_reported() {
    let fs = 48_000;
    let plan = plan_6_3(fs);
    let quality = quality_pass(&plan, 6);
    let target = flat_target(fs);
    let curves = curves_for(&plan, &["FL"], true);
    let mut options = fast_options();
    options.boost_cap_db = 5.0; // > +3 -> 按 +3 执行
    let output = run_fit(&plan, &target, &curves, Some(&quality), options, 9).expect("fit");
    let report = &output.report;
    assert!(
        report.warnings.iter().any(|line| line.contains("按 +3")),
        "warnings = {:?}",
        report.warnings
    );
    let channel = channel(report, "FL");
    assert_eq!(channel.boost_cap_db, MAX_BOOST_CAP_DB);
    assert!(
        channel.single_filter_max_boost_db <= MAX_BOOST_CAP_DB + 1e-6,
        "single boost {:.3}",
        channel.single_filter_max_boost_db
    );
    assert!(
        channel.filter_positive_sum_db <= D7_POSITIVE_SUM_WARN_DB + 1e-6,
        "positive sum {:.3}",
        channel.filter_positive_sum_db
    );
    assert!(channel.boost_limits_pass);
    // 500 Hz -10 dB 深谷：+3 dB 帽填不满 -> unfilled_dips 有记录。
    assert!(
        channel
            .unfilled_dips
            .iter()
            .any(|pair| (pair[0] - 500.0).abs() < 120.0 && pair[1] < -3.0),
        "unfilled dips = {:?}",
        channel.unfilled_dips
    );
}

// ---------------------------------------------------------------------------
// 4) trim / headroom（D7）
// ---------------------------------------------------------------------------

#[test]
fn t64_peq_fit_trim_reaches_headroom_floor() {
    let fs = 48_000;
    let plan = plan_6_3(fs);
    let quality = quality_pass(&plan, 6);
    let target = flat_target(fs);
    let curves = curves_for(&plan, &["FL"], true);
    let mut options = fast_options();
    options.boost_cap_db = 3.0;
    // 节目峰值参考 0 dBFS：+boost 后必须靠负 trim 拉回 <= -6 dBFS。
    options.program_peak_reference_dbfs = 0.0;
    let output = run_fit(&plan, &target, &curves, Some(&quality), options, 10).expect("fit");
    let channel = channel(&output.report, "FL");
    assert!(
        channel.predicted_peak_dbfs <= D7_PREDICTED_PEAK_LIMIT_DBFS + 1e-6,
        "predicted peak {:.3} dBFS",
        channel.predicted_peak_dbfs
    );
    assert!(channel.headroom_db >= D7_MIN_HEADROOM_DB - 1e-6);
    assert!(channel.headroom_pass);
    assert!(channel.trim_db <= 0.0);
    assert!(channel.trim_db >= TRIM_MIN_DB - 1e-9);
    assert!(channel.predicted_peak_before_trim_dbfs > channel.predicted_peak_dbfs);
}

#[test]
fn t64_peq_fit_headroom_infeasible_is_rejected() {
    let fs = 48_000;
    let plan = plan_6_3(fs);
    let quality = quality_pass(&plan, 6);
    let target = flat_target(fs);
    let curves = curves_for(&plan, &["FL"], true);
    let mut options = fast_options();
    options.boost_cap_db = 3.0;
    // +10 dBFS 参考峰值 + 正 boost：trim 下界 -12 dB 也不够 -> E_FIT_HEADROOM。
    options.program_peak_reference_dbfs = 10.0;
    let error =
        run_fit(&plan, &target, &curves, Some(&quality), options, 11).expect_err("must reject");
    assert_eq!(error.code(), E_FIT_HEADROOM, "{error}");
}

// ---------------------------------------------------------------------------
// 5) 目标 FIR：线性相位 / D / 系数产物 / 正增益帽
// ---------------------------------------------------------------------------

#[test]
fn t64_peq_fit_target_fir_is_linear_phase_and_reported() {
    let fs = 48_000;
    let plan = plan_6_3(fs);
    let quality = quality_pass(&plan, 6);
    let target = flat_target(fs);
    let curves = curves_for(&plan, &["FL"], false);
    let mut options = fast_options();
    options.target_fir.taps = 255;
    let output = run_fit(&plan, &target, &curves, Some(&quality), options, 12).expect("fit");
    let channel = channel(&output.report, "FL");
    assert_eq!(channel.target_fir_taps, 255);
    approx(channel.target_fir_group_delay_samples, 127.0, 1e-9);
    approx(
        channel.target_fir_group_delay_ms,
        127.0 * 1000.0 / fs as f64,
        1e-9,
    );
    let fir = &output.target_firs[0];
    assert_eq!(fir.coeffs.len(), 255);
    // 线性相位：h[n] == h[N-1-n]（对称）。
    for index in 0..fir.coeffs.len() / 2 {
        approx(
            fir.coeffs[index],
            fir.coeffs[fir.coeffs.len() - 1 - index],
            1e-9,
        );
    }
    // 默认只切：FIR 正增益不得超过 0 dB 附近。
    assert!(
        channel.target_fir_max_positive_db <= 0.5,
        "FIR positive {:.3}",
        channel.target_fir_max_positive_db
    );
    assert_eq!(channel.target_fir_coeff_sha256.len(), 64);
    // 频带内实现偏差应可报告且有限。
    assert!(channel.target_fir_max_deviation_db.is_finite());
    assert!(channel.target_fir_max_deviation_db <= 2.0);
    assert!(channel.target_fir_band_hz[1] > channel.target_fir_band_hz[0]);
}

#[test]
fn t64_peq_fit_target_fir_can_be_disabled() {
    let fs = 48_000;
    let plan = plan_6_3(fs);
    let quality = quality_pass(&plan, 6);
    let target = flat_target(fs);
    let curves = curves_for(&plan, &["FL"], false);
    let mut options = fast_options();
    options.target_fir.enabled = false;
    let output = run_fit(&plan, &target, &curves, Some(&quality), options, 13).expect("fit");
    let channel = channel(&output.report, "FL");
    assert_eq!(channel.target_fir_taps, 0);
    assert_eq!(channel.target_fir_group_delay_samples, 0.0);
    assert_eq!(channel.target_fir_max_positive_db, 0.0);
    let fir = &output.target_firs[0];
    assert!(fir.coeffs.is_empty());
}

#[test]
fn t64_peq_fit_target_fir_group_delay_is_taps_minus_one_over_two() {
    // F-16 唯一延迟定义：线性相位 target FIR 的固有群延迟 GD=(taps-1)/2，只由系数承载；
    // 奇 taps -> 整数，偶 taps -> 半整数（半整数同样不能被忽略/四舍五入）。
    let fs = 48_000;
    let plan = plan_6_3(fs);
    let quality = quality_pass(&plan, 6);
    let target = flat_target(fs);
    let curves = curves_for(&plan, &["FL"], false);
    for (case, taps, expected) in [("odd", 255usize, 127.0_f64), ("even", 256usize, 127.5)] {
        let mut options = fast_options();
        options.target_fir.taps = taps;
        let output = run_fit(&plan, &target, &curves, Some(&quality), options, 20 + taps as u64)
            .unwrap_or_else(|error| panic!("{case} / {taps} taps fit 失败：{error:?}"));
        let channel = channel(&output.report, "FL");
        assert_eq!(channel.target_fir_taps, taps);
        approx(channel.target_fir_group_delay_samples, expected, 1e-12);
        approx(
            channel.target_fir_group_delay_ms,
            expected * 1000.0 / fs as f64,
            1e-12,
        );
        let fir = &output.target_firs[0];
        assert_eq!(fir.taps, taps);
        assert_eq!(fir.coeffs.len(), taps);
        assert_eq!(fir.group_delay_samples, expected);
        // 线性相位 h[n]=h[taps-1-n]：群延迟已含在系数里，不存在额外延迟通道。
        for index in 0..taps / 2 {
            approx(fir.coeffs[index], fir.coeffs[taps - 1 - index], 1e-9);
        }
    }
}

// ---------------------------------------------------------------------------
// 6) T63 多点 plan：design / validation 分离（holdout 不参与拟合）
// ---------------------------------------------------------------------------

#[test]
fn t64_peq_fit_holdout_does_not_change_baseline_p() {
    let fs = 48_000;
    let plan = plan_6_3(fs);
    let quality = quality_pass(&plan, 6);
    let target = flat_target(fs);
    let curves = curves_for(&plan, &["FL"], false);
    let output_a =
        run_fit(&plan, &target, &curves, Some(&quality), fast_options(), 14).expect("fit a");
    // 改动 validation（holdout）测点曲线：基线 P（报告 p_sha256 / 段表）必须不变。
    let mut curves_b = curves.clone();
    for curve in curves_b.iter_mut() {
        if curve.points.len() > 0 && curve.position.starts_with('H') {
            for point in curve.points.iter_mut() {
                point.mag_db += 2.5;
            }
        }
    }
    let output_b = run_fit(
        &plan,
        &target,
        &curves_b,
        Some(&quality),
        fast_options(),
        14,
    )
    .expect("fit b");
    assert_eq!(output_a.report.p_sha256, output_b.report.p_sha256);
    assert_eq!(
        output_a.report.channels[0].bands,
        output_b.report.channels[0].bands
    );
    // validation 残差确实不同（说明 holdout 数据被读入但只用于报告）。
    let holdout_a = output_a.report.channels[0]
        .modal_residual_per_position
        .iter()
        .find(|position| position.label == "H1")
        .expect("H1 a")
        .modal_rms_after_p_db;
    let holdout_b = output_b.report.channels[0]
        .modal_residual_per_position
        .iter()
        .find(|position| position.label == "H1")
        .expect("H1 b")
        .modal_rms_after_p_db;
    assert!((holdout_a - holdout_b).abs() > 0.1);
    // 逐测点表覆盖 6 design + 3 validation。
    assert_eq!(
        output_a.report.channels[0]
            .modal_residual_per_position
            .len(),
        9
    );
}

// ---------------------------------------------------------------------------
// 7) T63 质量门
// ---------------------------------------------------------------------------

#[test]
fn t64_peq_fit_quality_gates_are_enforced() {
    let fs = 48_000;
    let plan = plan_6_3(fs);
    let target = flat_target(fs);
    let curves = curves_for(&plan, &["FL"], false);

    // 缺质量报告 -> E_FIT_QUALITY_MISSING（默认 require_design_quality=true）。
    let error = run_fit(&plan, &target, &curves, None, fast_options(), 15).expect_err("missing");
    assert_eq!(error.code(), E_FIT_QUALITY_MISSING);

    // continue_allowed=false -> E_FIT_QUALITY_BLOCKED。
    let blocked = quality_blocked(&plan);
    let error =
        run_fit(&plan, &target, &curves, Some(&blocked), fast_options(), 15).expect_err("blocked");
    assert_eq!(error.code(), E_FIT_QUALITY_BLOCKED);

    // stale plan hash -> E_FIT_QUALITY_STALE。
    let mut stale = quality_pass(&plan, 6);
    stale.plan_sha256 = "0".repeat(64);
    let error =
        run_fit(&plan, &target, &curves, Some(&stale), fast_options(), 15).expect_err("stale");
    assert_eq!(error.code(), E_FIT_QUALITY_STALE);

    // 显式允许缺质量（仅工具/合成夹具路径）时才能继续。
    let mut options = fast_options();
    options.require_design_quality = false;
    let output = run_fit(&plan, &target, &curves, None, options, 15).expect("explicit waiver");
    assert!(!output.report.quality.present);
    assert!(!output.report.quality.required || output.report.quality.present == false);
}

// ---------------------------------------------------------------------------
// 8) 契约错误码
// ---------------------------------------------------------------------------

#[test]
fn t64_peq_fit_rejects_invalid_inputs() {
    let fs = 48_000;
    let plan = plan_6_3(fs);
    let quality = quality_pass(&plan, 6);
    let target = flat_target(fs);
    let curves = curves_for(&plan, &["FL"], false);

    // 未知测点。
    let mut bad = curves.clone();
    bad[0].position = "UNKNOWN".to_string();
    let error = run_fit(&plan, &target, &bad, Some(&quality), fast_options(), 16)
        .expect_err("unknown position");
    assert_eq!(error.code(), E_FIT_UNKNOWN_POSITION);

    // 缺某通道的曲线。
    let partial: Vec<PositionCurve> = curves.iter().take(5).cloned().collect();
    let error = run_fit(&plan, &target, &partial, Some(&quality), fast_options(), 16)
        .expect_err("missing curves");
    assert_eq!(error.code(), E_FIT_MISSING_POSITION);

    // 重复曲线。
    let mut duplicate = curves.clone();
    duplicate.push(curves[0].clone());
    let error = run_fit(
        &plan,
        &target,
        &duplicate,
        Some(&quality),
        fast_options(),
        16,
    )
    .expect_err("duplicate");
    assert_eq!(error.code(), E_FIT_DUPLICATE);

    // 非法通道名。
    let mut bad_name = curves.clone();
    for curve in bad_name.iter_mut() {
        curve.channel = "fl".to_string();
    }
    let error = run_fit(
        &plan,
        &target,
        &bad_name,
        Some(&quality),
        fast_options(),
        16,
    )
    .expect_err("bad channel");
    assert_eq!(error.code(), E_FIT_CHANNEL);

    // 曲线频率非递增。
    let mut bad_curve = curves.clone();
    bad_curve[0].points.swap(3, 7);
    let error = run_fit(
        &plan,
        &target,
        &bad_curve,
        Some(&quality),
        fast_options(),
        16,
    )
    .expect_err("bad curve");
    assert_eq!(error.code(), E_FIT_CURVE);

    // target runtime_rate 与 profile fs 不一致。
    let other_target = flat_target(96_000);
    let error = run_fit(
        &plan,
        &other_target,
        &curves,
        Some(&quality),
        fast_options(),
        16,
    )
    .expect_err("target rate");
    assert_eq!(error.code(), E_FIT_TARGET);
}

#[test]
fn t64_peq_fit_rejects_custom_target_and_allows_only_flat_tilt10() {
    // F-15（用户裁决）：现行目标只有 flat 与 tilt10。其它 sha256 不再降级为 `custom`
    // 继续拟合，而是稳定返回 E_FIT_TARGET（prepare_target 单元级 + fit 入口集成级）。
    let fs = 48_000;
    let plan = plan_6_3(fs);
    let quality = quality_pass(&plan, 6);
    let curves = curves_for(&plan, &["FL"], false);
    let custom = custom_target(fs);

    let error = match prepare_target(&custom, fs, REFERENCE_FREQ_HZ) {
        Ok(_) => panic!("custom target 必须被 prepare_target 拒收"),
        Err(error) => error,
    };
    assert_eq!(error.code(), E_FIT_TARGET);
    assert!(
        error.message.contains("flat/tilt10"),
        "错误信息应说明只允许 flat/tilt10：{}",
        error.message
    );

    // 两条内置目标仍可反查 id 并通过（防误杀）。
    let flat = flat_target(fs);
    let shape = prepare_target(&flat, fs, REFERENCE_FREQ_HZ).expect("flat target");
    assert_eq!(shape.id, "flat");
    let tilt = tilt10_target(fs);
    let shape = prepare_target(&tilt, fs, REFERENCE_FREQ_HZ).expect("tilt10 target");
    assert_eq!(shape.id, "tilt10");

    // 集成级：fit_peq 入口同样拒收（prepare_context 第一道门，不写出任何 artifact）。
    let error = run_fit(&plan, &custom, &curves, Some(&quality), fast_options(), 16)
        .expect_err("custom target fit 必须拒收");
    assert_eq!(error.code(), E_FIT_TARGET);
}

#[test]
fn t64_peq_fit_options_validation_rules() {
    let mut options = PeqFitOptions::default();
    options.target_fir.taps = 8_192; // > 4096
    assert_eq!(
        options.validate(48_000).expect_err("taps").code(),
        E_FIT_OPTIONS
    );
    let mut options = PeqFitOptions::default();
    options.trim_range_db = [-20.0, 0.0];
    assert_eq!(
        options.validate(48_000).expect_err("trim").code(),
        E_FIT_OPTIONS
    );
    let mut options = PeqFitOptions::default();
    options.target_fir.allow_positive_db = 4.0;
    let warnings = options.validate(48_000).expect("clamp warning");
    assert!(warnings.iter().any(|line| line.contains("按 +3")));
    assert!((options.effective_fir_positive_cap_db() - 3.0).abs() < 1e-12);
    let options = PeqFitOptions::default();
    assert_eq!(options.effective_boost_cap_db(), 0.0);
}

// ---------------------------------------------------------------------------
// 9) 确定性 / artifact 发布
// ---------------------------------------------------------------------------

#[test]
fn t64_peq_fit_is_deterministic_and_artifacts_bind_report_hash() {
    let fs = 48_000;
    let plan = plan_6_3(fs);
    let quality = quality_pass(&plan, 6);
    let target = flat_target(fs);
    let curves = curves_for(&plan, &["FL"], false);
    let output_a =
        run_fit(&plan, &target, &curves, Some(&quality), fast_options(), 17).expect("fit a");
    let output_b =
        run_fit(&plan, &target, &curves, Some(&quality), fast_options(), 17).expect("fit b");
    assert_eq!(output_a.report_json(), output_b.report_json());
    assert_eq!(output_a.report.p_sha256, output_b.report.p_sha256);

    // 不同 created_unix：报告的 p_sha256 不变（P 内容相同），报告字节不同。
    let output_c =
        run_fit(&plan, &target, &curves, Some(&quality), fast_options(), 18).expect("fit c");
    assert_eq!(output_a.report.p_sha256, output_c.report.p_sha256);
    assert_ne!(output_a.report_json(), output_c.report_json());

    let artifacts = output_a.artifacts();
    assert_eq!(artifacts.entries.len(), 3);
    let report_json = output_a.report_json();
    assert_eq!(artifacts.report_sha256, sha256_hex(report_json.as_bytes()));
    let modal_peq = artifacts
        .entries
        .iter()
        .find(|entry| entry.path == FIT_MODAL_PEQ_ARTIFACT_PATH)
        .expect("modal peq artifact");
    let modal_peq_text = String::from_utf8(modal_peq.bytes.clone()).expect("utf8");
    assert!(modal_peq_text.contains("khaen-modal-peq/v1"));
    assert!(modal_peq_text.contains(&artifacts.report_sha256));
    let target_fir = artifacts
        .entries
        .iter()
        .find(|entry| entry.path == FIT_TARGET_FIR_ARTIFACT_PATH)
        .expect("target fir artifact");
    let target_fir_text = String::from_utf8(target_fir.bytes.clone()).expect("utf8");
    assert!(target_fir_text.contains(TARGET_FIR_SCHEMA));
    assert!(target_fir_text.contains(&artifacts.report_sha256));
    let report_entry = artifacts
        .entries
        .iter()
        .find(|entry| entry.path == FIT_REPORT_ARTIFACT_PATH)
        .expect("report artifact");
    assert_eq!(report_entry.sha256, artifacts.report_sha256);
    assert!(String::from_utf8(report_entry.bytes.clone())
        .expect("utf8")
        .contains(PEQ_FIT_SCHEMA));

    // 写盘（直接目录）并读回核对字节/hash。
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/t64-artifacts")
        .join(format!("t64-peq-fit-{}-{}", std::process::id(), counter));
    if dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    let written = artifacts.write_to_dir(&dir).expect("write artifacts");
    assert_eq!(written.len(), 3);
    for entry in &artifacts.entries {
        let path = dir.join(&entry.path);
        let bytes = std::fs::read(&path).expect("read back");
        assert_eq!(sha256_hex(&bytes), entry.sha256);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 10) 契约上限 / 单 owner / 低带可信开关
// ---------------------------------------------------------------------------

#[test]
fn t64_peq_fit_respects_contract_limits_and_shelf_quota() {
    let fs = 48_000;
    let plan = plan_6_3(fs);
    let quality = quality_pass(&plan, 6);
    let target = tilt10_target(fs);
    let curves = curves_for(&plan, &["FL"], false);
    let mut options = fast_options();
    options.boost_cap_db = 3.0;
    let output = run_fit(&plan, &target, &curves, Some(&quality), options, 19).expect("fit");
    let channel = channel(&output.report, "FL");
    assert!(channel.band_count <= 22);
    assert!(channel.band_count <= 32);
    assert!(channel.modal_band_count <= 10);
    assert!(channel.shelf_count <= 2);
    let mut previous = f64::NEG_INFINITY;
    for band in &channel.bands {
        assert!(band.freq_hz >= 20.0 && band.freq_hz <= 20_000.0);
        assert!(band.freq_hz > previous, "bands 须按频率升序");
        previous = band.freq_hz;
        if !band.modal {
            assert!(band.gain_db.abs() <= 24.0);
        }
    }
    // 模态残差单 owner：report 里同时给 PEQ 后与 P 后残差。
    assert!(channel.modal_residual_after_p.rms_after_db.is_finite());
    assert!(channel.modal_residual_per_position.len() == 9);
}

#[test]
fn t64_peq_fit_low_band_trusted_off_skips_below_60hz() {
    let fs = 48_000;
    let plan = plan_6_3(fs);
    let quality = quality_pass(&plan, 6);
    let target = flat_target(fs);
    let curves = curves_for(&plan, &["FL"], false);
    let mut options = fast_options();
    options.low_band_trusted = false;
    let output = run_fit(&plan, &target, &curves, Some(&quality), options, 20).expect("fit");
    let channel = channel(&output.report, "FL");
    assert!(
        channel
            .warnings
            .iter()
            .any(|line| line.contains("20-60 Hz")),
        "warnings = {:?}",
        channel.warnings
    );
    for band in modal_bands(channel) {
        assert!(band.freq_hz > 60.0, "band {band:?} below 60 Hz");
    }
}

// ---------------------------------------------------------------------------
// 11) 逐通道工作带（HP 以上才放段）
// ---------------------------------------------------------------------------

#[test]
fn t64_peq_fit_channel_work_band_limits_modal_and_wide_bands() {
    let fs = 48_000;
    let plan = plan_6_3(fs);
    let quality = quality_pass(&plan, 6);
    let target = flat_target(fs);
    let curves = curves_for(&plan, &["FL"], false);
    let bands = [ChannelBand::new("FL", [80.0, 16_000.0])];
    let request = PeqFitRequest::new(&plan, &target, &curves)
        .with_options(fast_options())
        .with_channel_bands(&bands)
        .with_design_quality(&quality)
        .with_created_unix(21);
    let output = fit_peq(&request).expect("fit");
    let channel = channel(&output.report, "FL");
    for band in &channel.bands {
        assert!(
            band.freq_hz >= 80.0 - 1e-9 && band.freq_hz <= 16_000.0 + 1e-9,
            "band {band:?} outside work band"
        );
    }
    assert_eq!(channel.work_band_hz, [80.0, 16_000.0]);
    // 工作带以上/以下不应有模态段。
    for band in modal_bands(channel) {
        assert!(band.freq_hz >= 80.0 - 1e-9);
    }
}

// ---------------------------------------------------------------------------
// 12) 2a 无法满足时诚实报告（不静默放行、不伪造 PASS）
// ---------------------------------------------------------------------------

#[test]
fn t64_peq_fit_rule_2a_failure_is_reported_honestly() {
    let fs = 48_000;
    let plan = plan_6_3(fs);
    let quality = quality_pass(&plan, 6);
    let target = flat_target(fs);
    // 20 dB 单模态：max_cut 12 不够，2a 峰高出目标 > +3 dB，必须报 false。
    let mut curves = curves_for(&plan, &["FL"], false);
    for curve in curves.iter_mut() {
        let grid = log_frequency_grid(20.0, 20_000.0, 1.0 / 48.0);
        let band = ModalBand::from_params(BandType::Peaking, 100.0, 8.0, 20.0, fs as f64)
            .expect("huge mode");
        for (index, point) in curve.points.iter_mut().enumerate() {
            point.mag_db += band.biquad().magnitude_db(grid[index], fs as f64);
        }
    }
    let output = run_fit(&plan, &target, &curves, Some(&quality), fast_options(), 22).expect("fit");
    let channel = channel(&output.report, "FL");
    assert!(!channel.rule_2a_pass, "must report 2a failure");
    assert!(channel.rule_2a_peak_over_target_db > RULE_2A_LIMIT_DB);
    assert!(
        channel.warnings.iter().any(|line| line.contains("2a")),
        "warnings = {:?}",
        channel.warnings
    );
    assert!(!output.report.checks.all_rule_2a_pass);
}

// ---------------------------------------------------------------------------
// 12b) 2a/2b pass 位只由 design 决定；holdout 单列（F-39）
// ---------------------------------------------------------------------------

/// 合成规则聚合输入：design/holdout 各自可选"坏"（100 Hz 处 +20 dB 模态，2a/2b 必 FAIL）。
fn rule_fixture(fs: u32, design_bad: bool, holdout_bad: bool) -> (FitContext, ChannelInput, ChannelPeq) {
    let grid = log_frequency_grid(FIT_FREQ_MIN_HZ, freq_upper_hz(fs as f64), FIT_GRID_OCTAVE);
    let target_db = vec![0.0_f64; grid.len()];
    let target = TargetShape {
        grid: grid.clone(),
        values_db: target_db.clone(),
        anchor_db: 0.0,
        id: "flat".to_string(),
        sha256: "a".repeat(64),
        normalization: "flat".to_string(),
        reference_gain_db: 0.0,
        point_count: grid.len(),
    };
    let context = FitContext {
        fs,
        fs_f: fs as f64,
        upper_hz: freq_upper_hz(fs as f64),
        grid: grid.clone(),
        target_db,
        target,
        plan_sha256: "b".repeat(64),
        measurement_sha256: "c".repeat(64),
        phase_basis: "aligned_segment".to_string(),
        common_d0_samples: 0,
        channels: Vec::new(),
    };
    let mode =
        ModalBand::from_params(BandType::Peaking, 100.0, 8.0, 20.0, fs as f64).expect("mode");
    let bad: Vec<f64> = grid
        .iter()
        .map(|frequency| mode.biquad().magnitude_db(*frequency, fs as f64))
        .collect();
    let flat = vec![0.0_f64; grid.len()];
    let position = |label: String, role: &'static str, db: &[f64]| PreparedPosition {
        label,
        role,
        weight: 1.0,
        db: db.to_vec(),
    };
    let design_db: &[f64] = if design_bad { &bad } else { &flat };
    let holdout_db: &[f64] = if holdout_bad { &bad } else { &flat };
    let input = ChannelInput {
        name: "FL".to_string(),
        index: 0,
        work_band_hz: [FIT_FREQ_MIN_HZ, freq_upper_hz(fs as f64)],
        design_positions: (0..2)
            .map(|index| position(format!("D{index}"), "design", design_db))
            .collect(),
        validation_positions: (0..2)
            .map(|index| position(format!("H{index}"), "validation", holdout_db))
            .collect(),
        modal_avg_db: flat.clone(),
        wide_avg_db: flat,
    };
    let channel = ChannelPeq::new("FL", 0).expect("empty channel");
    (context, input, channel)
}

#[test]
fn t64_peq_fit_rule_pass_bits_are_design_only_and_holdout_is_separate() {
    // design pass + holdout 差：design pass 位不受 holdout 影响，holdout 单列 false。
    let (context, input, channel) = rule_fixture(48_000, false, true);
    let mut warnings = Vec::new();
    let aggregate = evaluate_rules(&context, &input, &channel, Some([20.0, 250.0]), &mut warnings)
        .expect("rules");
    assert!(aggregate.rule_2a_pass, "design 2a must stay pass");
    assert!(aggregate.rule_2b_pass, "design 2b must stay pass");
    assert!(!aggregate.holdout_rule_2a_pass);
    assert!(!aggregate.holdout_rule_2b_pass);
    assert!(aggregate.holdout_peak_over_target_db > RULE_2A_LIMIT_DB);
    assert!(
        warnings.iter().any(|line| line.contains("holdout 2a")),
        "warnings = {warnings:?}"
    );
    // 反例：design 差 + holdout pass —— design pass 位 false，holdout 单列 true。
    let (context, input, channel) = rule_fixture(48_000, true, false);
    let mut warnings = Vec::new();
    let aggregate = evaluate_rules(&context, &input, &channel, Some([20.0, 250.0]), &mut warnings)
        .expect("rules");
    assert!(!aggregate.rule_2a_pass);
    assert!(!aggregate.rule_2b_pass);
    assert!(aggregate.holdout_rule_2a_pass);
    assert!(aggregate.holdout_rule_2b_pass);
    assert!(warnings.iter().any(|line| line.contains("design 2a")), "warnings = {warnings:?}");
}

#[test]
fn t64_peq_fit_report_keeps_holdout_rules_separate_from_design_pass() {
    let fs = 48_000;
    let plan = plan_6_3(fs);
    let quality = quality_pass(&plan, 6);
    let target = flat_target(fs);
    let mut curves = curves_for(&plan, &["FL"], false);
    // 只把 holdout 曲线搞差（100 Hz +20 dB）：design 拟合与 design pass 位不得受影响。
    let grid = log_frequency_grid(20.0, 20_000.0, 1.0 / 48.0);
    let mode =
        ModalBand::from_params(BandType::Peaking, 100.0, 8.0, 20.0, fs as f64).expect("mode");
    for curve in curves.iter_mut() {
        if curve.position.starts_with('H') {
            for (index, point) in curve.points.iter_mut().enumerate() {
                point.mag_db += mode.biquad().magnitude_db(grid[index], fs as f64);
            }
        }
    }
    let output = run_fit(&plan, &target, &curves, Some(&quality), fast_options(), 23)
        .expect("fit with bad holdout");
    let channel = channel(&output.report, "FL");
    assert!(
        channel.rule_2a_pass,
        "design pass must be unaffected by holdout: {:?}",
        channel.warnings
    );
    assert!(
        !channel.holdout_rule_2a_pass,
        "holdout 2a must be reported separately: {:?}",
        channel.warnings
    );
    assert!(channel.holdout_rule_2a_peak_over_target_db > RULE_2A_LIMIT_DB);
    assert!(channel
        .warnings
        .iter()
        .any(|line| line.contains("holdout 2a")));
    let json = output.report.to_json();
    assert!(json.contains("\"rule_2a_pass\":true"), "{json}");
    assert!(json.contains("\"holdout_rules\":"), "{json}");
    assert!(json.contains("\"holdout_rule_2a_pass\":false"), "{json}");
}

// ---------------------------------------------------------------------------
// 13) 证据样例（opt-in：KHAEN_T64_EVIDENCE_DIR 指向 evidence/T64 时写真实产物）
// ---------------------------------------------------------------------------

#[test]
fn t64_peq_fit_writes_evidence_examples_when_requested() {
    let Ok(dir) = std::env::var("KHAEN_T64_EVIDENCE_DIR") else {
        return;
    };
    let fs = 48_000;
    let plan = plan_6_3(fs);
    let quality = quality_pass(&plan, 6);
    let target = flat_target(fs);
    let curves = curves_for(&plan, &["FL"], false);
    let output = run_fit(
        &plan,
        &target,
        &curves,
        Some(&quality),
        fast_options(),
        1_700_000_000,
    )
    .expect("fit");
    let artifacts = output.artifacts();
    let root = PathBuf::from(dir);
    let written = artifacts.write_to_dir(&root).expect("write artifacts");
    println!("evidence artifacts: {written:?}");
    println!("report_sha256={}", artifacts.report_sha256);
    println!("p_sha256={}", output.report.p_sha256);
    println!(
        "FL modal_bands={} band_count={} applied_boost={:.3} headroom={:.3}",
        output.report.channels[0].modal_band_count,
        output.report.channels[0].band_count,
        output.report.channels[0].applied_boost_db,
        output.report.channels[0].headroom_db
    );
}
