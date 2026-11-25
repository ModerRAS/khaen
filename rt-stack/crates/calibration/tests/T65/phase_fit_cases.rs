// T65（delay/polarity 实测估计 / 整数样本契约 / FIR 执行 phase 方向）私有测试。
//
// 本文件经 crates/calibration/src/phase_fit.rs 末尾的
//   #[cfg(test)] mod t65_tests { include!("../tests/T65/phase_fit_cases.rs"); }
// 编进 lib 单测（cargo 不自动发现 tests/<子目录>；公共注册属 integrator，不要重复注册）。
// 运行：cargo test -p khaen-calibration phase_fit
//
// 离线：不打开 ALSA / 不碰板端；全部为解析合成复响应夹具（T49 口径 X=|X|e^{+jφ}，
// 残余到达延迟 τ 贡献 +2πfτ/fs）。覆盖：相对整数契约（D=round(max t − t)、min=0、D=0=最晚）、
// 极性实测/margin、未知不猜（极性歧义/质量门/搜索边界/超 200ms 上限）、T63 质量门、
// 参考测点解析、CommonD0 只扣一次（aligned/absolute + 不匹配拒绝）、T40 FIR 执行方向
// （正确方向 conj(H) 对齐 / 错误方向必须失败）、确定性 JSON 与 artifact 写盘、证据样例。

use std::fs;
use std::path::{Path, PathBuf};

use crate::positions::{
    MeasurementQuality, MultipointPosition, PositionQuality, PositionQualityReport, RepeatPolicy,
    POSITION_QUALITY_SCHEMA,
};

const T65_FS: u32 = 48_000;
const T65_D0: i64 = 137;
const T65_SESSION: &str = "t65-session-2026-11-17";
const T65_PHASE_REF: &str =
    "aligned_segment(已去粗延迟); absolute_phase_deg = phase_deg - 360*f*delay_samples/fs";
const T65_PARTICIPANTS: [&str; 5] = ["FL", "FR", "SL", "SR", "SUB1"];

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

fn t65_participants() -> Vec<String> {
    T65_PARTICIPANTS
        .iter()
        .map(|name| name.to_string())
        .collect()
}

fn t65_positions(design: usize, holdout: usize) -> Vec<MultipointPosition> {
    let mut positions = Vec::new();
    // 听音位多点 plan：麦克风朝向必须是 90deg（ACCEPTANCE §0.1.2；T63 plan 校验）。
    for index in 0..design {
        positions.push(
            MultipointPosition::design(format!("D{index}"), 1.0)
                .unwrap()
                .with_mic_orientation("90deg"),
        );
    }
    for index in 0..holdout {
        positions.push(
            MultipointPosition::validation(format!("V{index}"), 1.0)
                .unwrap()
                .with_mic_orientation("90deg"),
        );
    }
    positions
}

fn t65_plan(design: usize, holdout: usize) -> MultipointPlan {
    t65_plan_with(design, holdout, T65_D0, T65_PHASE_REF, t65_participants())
}

fn t65_plan_with(
    design: usize,
    holdout: usize,
    common_d0: i64,
    phase_ref: &str,
    participants: Vec<String>,
) -> MultipointPlan {
    MultipointPlan::new(
        T65_SESSION,
        "4.1.2",
        T65_FS,
        participants,
        CommonD0::new(common_d0).unwrap(),
        phase_ref,
        t65_positions(design, holdout),
    )
    .unwrap()
}

/// 1/24 oct 对数网格（20 Hz–20 kHz；mimo-t6 §3.3 分析网格）。
fn t65_grid() -> Vec<f64> {
    let ratio = 2.0_f64.powf(1.0 / 24.0);
    let mut grid = Vec::new();
    let mut frequency = 20.0_f64;
    while frequency <= 20_000.0 {
        grid.push(frequency);
        frequency *= ratio;
    }
    grid
}

fn t65_flat_magnitude(_frequency: f64) -> f64 {
    1.0
}

fn t65_zero_phase(_frequency: f64) -> f64 {
    0.0
}

/// 合成 T49 口径原始点：X = pol·mag(f)·exp(+j(2π f(d0+τ)/fs + phase0(f)))。
fn t65_raw_points_on(
    grid: &[f64],
    fs: u32,
    d0: i64,
    tau: f64,
    polarity: i32,
    magnitude: &dyn Fn(f64) -> f64,
    phase0: &dyn Fn(f64) -> f64,
) -> Vec<PhaseFitPoint> {
    grid.iter()
        .map(|frequency| {
            let phase = 2.0 * std::f64::consts::PI * frequency * (d0 as f64 + tau) / fs as f64
                + phase0(*frequency);
            let gain = polarity as f64 * magnitude(*frequency);
            PhaseFitPoint::new(*frequency, gain * phase.cos(), gain * phase.sin())
        })
        .collect()
}

fn t65_raw_points(fs: u32, d0: i64, tau: f64, polarity: i32) -> Vec<PhaseFitPoint> {
    t65_raw_points_on(
        &t65_grid(),
        fs,
        d0,
        tau,
        polarity,
        &t65_flat_magnitude,
        &t65_zero_phase,
    )
}

fn t65_response(
    plan: &MultipointPlan,
    position: &str,
    channel: &str,
    tau: f64,
    polarity: i32,
) -> DePhasedResponse {
    let raw = t65_raw_points(plan.sample_rate_hz, plan.common_d0.samples(), tau, polarity);
    DePhasedResponse::from_aligned_segment(
        position,
        channel,
        plan.sample_rate_hz,
        raw,
        plan.common_d0,
    )
    .unwrap()
}

fn t65_responses(
    plan: &MultipointPlan,
    position: &str,
    delays: &[f64],
    polarities: &[i32],
) -> Vec<DePhasedResponse> {
    plan.participants
        .iter()
        .enumerate()
        .map(|(index, channel)| {
            t65_response(plan, position, channel, delays[index], polarities[index])
        })
        .collect()
}

fn t65_responses_all(
    plan: &MultipointPlan,
    delays_by_position: &[Vec<f64>],
    polarities: &[i32],
) -> Vec<DePhasedResponse> {
    let labels: Vec<String> = plan
        .design_positions()
        .map(|position| position.label.clone())
        .collect();
    let mut responses = Vec::new();
    for (position_index, label) in labels.iter().enumerate() {
        responses.extend(t65_responses(
            plan,
            label,
            &delays_by_position[position_index],
            polarities,
        ));
    }
    responses
}

fn t65_options(reference_position: Option<&str>) -> PhaseFitOptions {
    PhaseFitOptions {
        reference_position: reference_position.map(|label| label.to_string()),
        require_design_quality: false,
        ..PhaseFitOptions::default()
    }
}

fn t65_quality(plan: &MultipointPlan, continue_allowed: bool) -> PositionQualityReport {
    PositionQualityReport {
        schema: POSITION_QUALITY_SCHEMA,
        plan_sha256: plan.plan_sha256(),
        scope: QualityScope::Design,
        repeat_policy: RepeatPolicy::DesignOff,
        min_repeats: 3,
        plan_design_total: plan.design_count(),
        plan_validation_total: plan.validation_count(),
        points_total: plan.design_count(),
        points_usable: if continue_allowed {
            plan.design_count()
        } else {
            0
        },
        continue_allowed,
        reasons: Vec::new(),
        points: Vec::new(),
    }
}

fn t65_fit(
    plan: &MultipointPlan,
    responses: &[DePhasedResponse],
    options: PhaseFitOptions,
) -> Result<PhaseFitOutput, PhaseFitError> {
    let request = PhaseFitRequest::new(plan, responses).with_options(options);
    fit_phase(&request)
}

fn t65_fit_with_quality(
    plan: &MultipointPlan,
    responses: &[DePhasedResponse],
    quality: &PositionQualityReport,
    options: PhaseFitOptions,
) -> Result<PhaseFitOutput, PhaseFitError> {
    let request = PhaseFitRequest::new(plan, responses)
        .with_options(options)
        .with_design_quality(quality);
    fit_phase(&request)
}

fn t65_expected_delays(taus: &[f64]) -> Vec<u32> {
    let max = taus.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    taus.iter().map(|tau| (max - tau).round() as u32).collect()
}

fn t65_channel<'a>(output: &'a PhaseFitOutput, channel: &str) -> &'a PhaseChannelReport {
    output
        .report
        .channels
        .iter()
        .find(|report| report.channel == channel)
        .unwrap()
}

fn t65_design_label(plan: &MultipointPlan, index: usize) -> String {
    plan.design_positions()
        .nth(index)
        .map(|position| position.label.clone())
        .unwrap()
}

fn t65_target_tmp(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/tmp")
        .join("T65")
        .join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn t65_assert_ok(output: &PhaseFitOutput) {
    assert!(
        output.report.checks.all_pass,
        "checks must all pass: {:?}",
        output.report.checks
    );
    assert_eq!(output.report.schema, PHASE_FIT_SCHEMA);
    assert_eq!(output.ops.len(), output.report.channels.len());
    assert_eq!(output.firs.len(), output.report.channels.len());
    let min = output
        .report
        .channels
        .iter()
        .map(|channel| channel.delay_samples)
        .min()
        .unwrap();
    assert_eq!(min, 0, "CONTRACT min(delay_samples) must be 0");
    let reference = output
        .report
        .channels
        .iter()
        .find(|channel| channel.reference)
        .expect("exactly one reference channel");
    assert_eq!(
        reference.delay_samples, 0,
        "reference channel must have D=0"
    );
}

// ---------------------------------------------------------------------------
// 1) 相对整数契约：D_i = round(max t − t_i)，min=0，D=0=最晚 = 参考通道
// ---------------------------------------------------------------------------

#[test]
fn t65_phase_fit_estimates_relative_delays_and_integer_contract() {
    let plan = t65_plan(1, 1);
    let label = t65_design_label(&plan, 0);
    let taus = [100.0, 90.0, 120.0, 110.0, 130.0];
    let polarities = [1, 1, 1, 1, 1];
    let responses = t65_responses(&plan, &label, &taus, &polarities);
    let output = t65_fit(&plan, &responses, t65_options(None)).expect("fit must succeed");
    t65_assert_ok(&output);
    let expected = t65_expected_delays(&taus);
    assert_eq!(expected, vec![30, 40, 10, 20, 0]);
    assert_eq!(output.report.reference_channel, "SUB1");
    assert_eq!(output.report.reference_position, label);
    assert!(!output.report.reference_ambiguous);
    let margin = output.report.reference_tie_margin_samples.unwrap();
    assert!(
        (margin - 10.0).abs() < 0.2,
        "tie margin {margin} should be ~10"
    );
    for (channel_index, channel) in T65_PARTICIPANTS.iter().enumerate() {
        let report = t65_channel(&output, channel);
        assert_eq!(report.delay_samples, expected[channel_index], "{channel} D");
        assert!(
            (report.delay_raw_samples - expected[channel_index] as f64).abs() < 0.05,
            "{channel} raw delay {} vs {} (estimator sign/scale)",
            report.delay_raw_samples,
            expected[channel_index]
        );
        assert!(report.fractional_residual_samples.abs() <= 0.5 + 1e-12);
        assert!(report.delay_confidence >= 0.5, "{channel} delay confidence");
        assert_eq!(report.polarity, Polarity::Normal);
        assert!(report.fir.verification_passed);
        assert!(report.execution.within_tolerance);
        assert!(report.execution.direction_ok);
    }
    assert_eq!(output.ops.len(), expected.len());
    assert!(output
        .report
        .warnings
        .iter()
        .any(|warning| warning.contains("channel_indices")));
}

// ---------------------------------------------------------------------------
// 2) 极性：实测反相 -> calibration invert；参考通道自身反相也能补偿
// ---------------------------------------------------------------------------

#[test]
fn t65_phase_fit_polarity_detects_invert_and_compensates() {
    let plan = t65_plan(1, 1);
    let label = t65_design_label(&plan, 0);
    let taus = [50.0, 60.0, 70.0, 80.0, 90.0];
    // FL 与参考通道 SUB1 都反相：两者都需要 calibration invert。
    let polarities = [-1, 1, 1, 1, -1];
    let responses = t65_responses(&plan, &label, &taus, &polarities);
    let output = t65_fit(&plan, &responses, t65_options(None)).expect("fit must succeed");
    t65_assert_ok(&output);
    let expected = t65_expected_delays(&taus);
    for (channel_index, channel) in T65_PARTICIPANTS.iter().enumerate() {
        let report = t65_channel(&output, channel);
        assert_eq!(report.delay_samples, expected[channel_index]);
        assert_eq!(report.measured_polarity_sign, polarities[channel_index]);
        let expected_polarity = if polarities[channel_index] < 0 {
            Polarity::Invert
        } else {
            Polarity::Normal
        };
        assert_eq!(
            report.polarity, expected_polarity,
            "{channel} correction polarity"
        );
        assert!(
            report.polarity_confidence > 0.9,
            "{channel} polarity confidence"
        );
        assert_eq!(report.execution.compensated_polarity_sign, Some(1));
        assert!(report.execution.direction_ok);
    }
    let fl = t65_channel(&output, "FL");
    assert_eq!(fl.execution.delta_samples.abs() <= 0.5, true);
}

// ---------------------------------------------------------------------------
// 3) 未知不猜：极性正交（real part 抵消）-> E_PHF_POLARITY_UNKNOWN
// ---------------------------------------------------------------------------

#[test]
fn t65_phase_fit_unknown_polarity_never_guessed() {
    let plan = t65_plan(1, 1);
    let label = t65_design_label(&plan, 0);
    let taus = [40.0, 50.0, 60.0, 70.0, 80.0];
    let polarities = [1, 1, 1, 1, 1];
    let mut responses = t65_responses(&plan, &label, &taus, &polarities);
    // FR：延迟严格线性（delay 估计正常），但去延迟后响应整体 +90°，实部/相干和 = 0 -> 极性不可判。
    let fr_raw = t65_raw_points_on(
        &t65_grid(),
        plan.sample_rate_hz,
        plan.common_d0.samples(),
        50.0,
        1,
        &t65_flat_magnitude,
        &|_| std::f64::consts::FRAC_PI_2,
    );
    let fr = DePhasedResponse::from_aligned_segment(
        &label,
        "FR",
        plan.sample_rate_hz,
        fr_raw,
        plan.common_d0,
    )
    .unwrap();
    let fr_index = responses
        .iter()
        .position(|response| response.channel() == "FR")
        .unwrap();
    responses[fr_index] = fr;
    let error = t65_fit(&plan, &responses, t65_options(None))
        .expect_err("ambiguous polarity must not be guessed");
    assert_eq!(error.code(), "E_PHF_POLARITY_UNKNOWN");
}

// ---------------------------------------------------------------------------
// 4) T63 design scope 质量门：缺/受阻/stale/错 scope 都不继续
// ---------------------------------------------------------------------------

#[test]
fn t65_phase_fit_design_quality_gate() {
    let plan = t65_plan(1, 1);
    let label = t65_design_label(&plan, 0);
    let taus = [10.0, 20.0, 30.0, 40.0, 50.0];
    let responses = t65_responses(&plan, &label, &taus, &[1, 1, 1, 1, 1]);
    let strict = PhaseFitOptions {
        reference_position: None,
        ..PhaseFitOptions::default()
    };
    // 缺质量报告：默认 require_design_quality=true -> 不继续。
    let error = t65_fit(&plan, &responses, strict.clone()).expect_err("missing quality must stop");
    assert_eq!(error.code(), "E_PHF_QUALITY_MISSING");
    // 正常质量报告 -> 通过。
    let quality = t65_quality(&plan, true);
    t65_fit_with_quality(&plan, &responses, &quality, strict.clone()).expect("quality gate pass");
    // 不允许继续 -> 不继续。
    let blocked = t65_quality(&plan, false);
    let error = t65_fit_with_quality(&plan, &responses, &blocked, strict.clone())
        .expect_err("continue_allowed=false must stop");
    assert_eq!(error.code(), "E_PHF_QUALITY_BLOCKED");
    // plan 变更后 stale -> 不继续。
    let mut stale = t65_quality(&plan, true);
    stale.plan_sha256 = "deadbeef".to_string();
    let error = t65_fit_with_quality(&plan, &responses, &stale, strict.clone())
        .expect_err("stale quality must stop");
    assert_eq!(error.code(), "E_PHF_QUALITY_STALE");
    // scope 不是 design -> 不继续。
    let mut validation_scope = t65_quality(&plan, true);
    validation_scope.scope = QualityScope::Validation;
    let error = t65_fit_with_quality(&plan, &responses, &validation_scope, strict)
        .expect_err("validation scope quality must not drive the fit");
    assert_eq!(error.code(), "E_PHF_QUALITY_BLOCKED");
}

// ---------------------------------------------------------------------------
// 5) holdout 不进拟合 / 未知测点或角色拒绝 / 缺失或重复响应拒绝
// ---------------------------------------------------------------------------

#[test]
fn t65_phase_fit_rejects_holdout_and_unknown_responses() {
    let plan = t65_plan(1, 1);
    let label = t65_design_label(&plan, 0);
    let taus = [10.0, 20.0, 30.0, 40.0, 50.0];
    let mut responses = t65_responses(&plan, &label, &taus, &[1, 1, 1, 1, 1]);
    // holdout 响应 -> 拒绝（holdout 不得用于调参）。
    let mut with_holdout = responses.clone();
    with_holdout.push(t65_response(&plan, "V0", "FL", 999.0, 1));
    let error =
        t65_fit(&plan, &with_holdout, t65_options(None)).expect_err("holdout must be rejected");
    assert_eq!(error.code(), "E_PHF_HOLDOUT_CONTAMINATION");
    // 未知测点 -> 拒绝。
    let mut unknown_position = responses.clone();
    unknown_position.push(t65_response(&plan, "NOPE", "FL", 0.0, 1));
    let error = t65_fit(&plan, &unknown_position, t65_options(None)).expect_err("unknown position");
    assert_eq!(error.code(), "E_PHF_RESPONSE_UNKNOWN");
    // 未知角色 -> 拒绝。
    let mut unknown_channel = responses.clone();
    unknown_channel.push(t65_response(&plan, &label, "HFL", 0.0, 1));
    let error = t65_fit(&plan, &unknown_channel, t65_options(None)).expect_err("unknown channel");
    assert_eq!(error.code(), "E_PHF_RESPONSE_UNKNOWN");
    // 缺一个响应 -> 拒绝。
    responses.pop();
    let error = t65_fit(&plan, &responses, t65_options(None)).expect_err("missing response");
    assert_eq!(error.code(), "E_PHF_RESPONSE_MISSING");
    // 重复响应 -> 拒绝。
    let taus = [10.0, 20.0, 30.0, 40.0, 50.0];
    let mut duplicated = t65_responses(&plan, &label, &taus, &[1, 1, 1, 1, 1]);
    duplicated.push(duplicated[0].clone());
    let error = t65_fit(&plan, &duplicated, t65_options(None)).expect_err("duplicate response");
    assert_eq!(error.code(), "E_PHF_RESPONSE_DUPLICATE");
}

// ---------------------------------------------------------------------------
// 6) 参考测点：多 design 测点必须显式；相对契约与公共平移无关
// ---------------------------------------------------------------------------

#[test]
fn t65_phase_fit_reference_position_must_be_explicit_and_relative_delays_invariant() {
    let plan = t65_plan(2, 1);
    let label0 = t65_design_label(&plan, 0);
    let label1 = t65_design_label(&plan, 1);
    let taus0 = [100.0, 90.0, 120.0, 110.0, 130.0];
    // 第二个测点整体到达晚 3.7 samples（距离差），通道间相对结构保持不变。
    let taus1: Vec<f64> = taus0.iter().map(|tau| tau + 3.7).collect();
    let polarities = [1, 1, 1, 1, 1];
    let responses = t65_responses_all(&plan, &[taus0.to_vec(), taus1], &polarities);
    // 多测点未声明参考测点 -> 拒绝（不猜座位）。
    let error =
        t65_fit(&plan, &responses, t65_options(None)).expect_err("reference position required");
    assert_eq!(error.code(), "E_PHF_REFERENCE_POSITION");
    // 指定不存在/非 design 测点 -> 拒绝。
    let error = t65_fit(&plan, &responses, t65_options(Some("V0")))
        .expect_err("holdout cannot be reference");
    assert_eq!(error.code(), "E_PHF_REFERENCE_POSITION");
    // 两个 design 测点分别作为参考：相对 D 必须一致（公共到达差被相对契约定量抵消）。
    let output0 = t65_fit(&plan, &responses, t65_options(Some(&label0))).expect("reference D0");
    let output1 = t65_fit(&plan, &responses, t65_options(Some(&label1))).expect("reference D1");
    assert_eq!(output0.report.reference_position, label0);
    assert_eq!(output1.report.reference_position, label1);
    assert_eq!(output0.report.reference_channel, "SUB1");
    assert_eq!(output1.report.reference_channel, "SUB1");
    let expected = t65_expected_delays(&taus0);
    for (channel_index, channel) in T65_PARTICIPANTS.iter().enumerate() {
        assert_eq!(
            t65_channel(&output0, channel).delay_samples,
            expected[channel_index]
        );
        assert_eq!(
            t65_channel(&output1, channel).delay_samples,
            expected[channel_index]
        );
    }
    // 两测点各自报告自己的原始到达（差 3.7 samples），证明没有把测点差异当校准量。
    let d0_fl = t65_channel(&output0, "FL");
    let d1_fl = t65_channel(&output1, "FL");
    let delta = d1_fl.positions[1].delay_raw_samples - d0_fl.positions[0].delay_raw_samples;
    assert!(
        (delta - 3.7).abs() < 0.2,
        "per-position raw delay delta {delta} ~ 3.7"
    );
}

// ---------------------------------------------------------------------------
// 7) 单通道频带覆盖（channel_bands）：低频专用通道用窄带，未知/重复拒绝
// ---------------------------------------------------------------------------

#[test]
fn t65_phase_fit_channel_bands_override_and_validation() {
    let plan = t65_plan(1, 1);
    let label = t65_design_label(&plan, 0);
    let taus = [100.0, 100.0, 60.0, 60.0, 80.0];
    let polarities = [1, 1, 1, 1, 1];
    let mut responses = t65_responses(&plan, &label, &taus, &polarities);
    // SUB1 只在 40–160 Hz 内是真实响应（τ=80）；其余频段是另一条相干路径（τ=500）。
    let sub_raw: Vec<PhaseFitPoint> = t65_grid()
        .iter()
        .map(|frequency| {
            let tau = if *frequency >= 40.0 && *frequency <= 160.0 {
                80.0
            } else {
                500.0
            };
            let phase =
                2.0 * std::f64::consts::PI * frequency * (plan.common_d0.samples() as f64 + tau)
                    / plan.sample_rate_hz as f64;
            PhaseFitPoint::new(*frequency, phase.cos(), phase.sin())
        })
        .collect();
    let sub = DePhasedResponse::from_aligned_segment(
        &label,
        "SUB1",
        plan.sample_rate_hz,
        sub_raw,
        plan.common_d0,
    )
    .unwrap();
    let sub_index = responses
        .iter()
        .position(|response| response.channel() == "SUB1")
        .unwrap();
    responses[sub_index] = sub;
    // 默认频带 [200,4000]：SUB1 被估计成 τ=500 -> 参考通道错成 SUB1（说明必须显式声明窄带）。
    let without_override =
        t65_fit(&plan, &responses, t65_options(None)).expect("fit works but wrong reference");
    assert_eq!(without_override.report.reference_channel, "SUB1");
    // 显式 SUB1 窄带 [40,160]：τ=80，参考通道回到 FL（100），D 正确。
    let channel_bands = [PhaseChannelBand::new("SUB1", [40.0, 160.0])];
    let request = PhaseFitRequest::new(&plan, &responses)
        .with_options(t65_options(None))
        .with_channel_bands(&channel_bands);
    let output = fit_phase(&request).expect("channel band override");
    t65_assert_ok(&output);
    assert_eq!(output.report.reference_channel, "FL");
    let expected = t65_expected_delays(&taus);
    for (channel_index, channel) in T65_PARTICIPANTS.iter().enumerate() {
        assert_eq!(
            t65_channel(&output, channel).delay_samples,
            expected[channel_index]
        );
    }
    // 未知通道/重复条目 -> 配置拒绝。
    let unknown = [PhaseChannelBand::new("HFL", [40.0, 160.0])];
    let request = PhaseFitRequest::new(&plan, &responses)
        .with_options(t65_options(None))
        .with_channel_bands(&unknown);
    assert_eq!(fit_phase(&request).unwrap_err().code(), "E_PHF_CONFIG");
    let duplicate = [
        PhaseChannelBand::new("SUB1", [40.0, 160.0]),
        PhaseChannelBand::new("SUB1", [50.0, 150.0]),
    ];
    let request = PhaseFitRequest::new(&plan, &responses)
        .with_options(t65_options(None))
        .with_channel_bands(&duplicate);
    assert_eq!(fit_phase(&request).unwrap_err().code(), "E_PHF_CONFIG");
}

// ---------------------------------------------------------------------------
// 8) CommonD0 只扣一次（aligned_segment 去相位一次 / absolute 不再扣 / 不匹配拒绝）
// ---------------------------------------------------------------------------

#[test]
fn t65_phase_fit_common_d0_removed_exactly_once() {
    // aligned_segment：raw 相位 = ω(d0+τ)，去相位一次后残余相位 = ωτ。
    let simple_grid = vec![
        100.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 12_000.0, 16_000.0,
    ];
    let d0 = CommonD0::new(T65_D0).unwrap();
    let raw = t65_raw_points_on(
        &simple_grid,
        T65_FS,
        T65_D0,
        0.0,
        1,
        &t65_flat_magnitude,
        &t65_zero_phase,
    );
    let removed = DePhasedResponse::from_aligned_segment("D0", "FL", T65_FS, raw, d0).unwrap();
    assert_eq!(removed.basis(), PhaseBasis::AlignedSegment);
    assert_eq!(removed.delay_removed_samples(), T65_D0 as f64);
    for point in removed.points() {
        assert!(
            point.phase_deg().abs() < 1e-9,
            "d0 must be removed exactly once @ {} Hz",
            point.freq_hz
        );
    }
    let raw = t65_raw_points_on(
        &simple_grid,
        T65_FS,
        T65_D0,
        3.25,
        1,
        &t65_flat_magnitude,
        &t65_zero_phase,
    );
    let removed = DePhasedResponse::from_aligned_segment("D0", "FL", T65_FS, raw, d0).unwrap();
    for (point, frequency) in removed.points().iter().zip(simple_grid.iter()) {
        let expected = t65_wrap_deg(360.0 * frequency * 3.25 / T65_FS as f64);
        assert!(
            (t65_wrap_deg(point.phase_deg()) - expected).abs() < 1e-6,
            "residual phase must be exactly +360*f*tau/fs at {frequency} Hz",
        );
    }
    // plan 与响应 D0 不一致 -> 拒绝（双扣/漏扣风险，不猜）。
    let plan = t65_plan(1, 1);
    let label = t65_design_label(&plan, 0);
    let mismatched_d0 = CommonD0::new(100).unwrap();
    let mut responses = Vec::new();
    for channel in &plan.participants {
        let raw = t65_raw_points(plan.sample_rate_hz, plan.common_d0.samples(), 10.0, 1);
        responses.push(
            DePhasedResponse::from_aligned_segment(
                &label,
                channel,
                plan.sample_rate_hz,
                raw,
                mismatched_d0,
            )
            .unwrap(),
        );
    }
    let error = t65_fit(&plan, &responses, t65_options(None)).expect_err("D0 mismatch");
    assert_eq!(error.code(), "E_PHF_COMMON_D0_MISMATCH");
    // absolute 响应不能塞进 aligned_segment plan（会混淆「是否已扣」）。
    let mut responses = t65_responses(
        &plan,
        &label,
        &[10.0, 20.0, 30.0, 40.0, 50.0],
        &[1, 1, 1, 1, 1],
    );
    let absolute = DePhasedResponse::from_absolute(
        &label,
        "FL",
        plan.sample_rate_hz,
        t65_raw_points(plan.sample_rate_hz, 0, 10.0, 1),
    )
    .unwrap();
    responses[0] = absolute;
    let error = t65_fit(&plan, &responses, t65_options(None)).expect_err("basis mismatch");
    assert_eq!(error.code(), "E_PHF_PHASE_BASIS");
    // absolute plan + absolute 响应：不再扣任何 delay，相对契约仍然正确。
    let absolute_plan = t65_plan_with(
        1,
        1,
        0,
        "absolute_phase_deg (already total phase)",
        t65_participants(),
    );
    let absolute_label = t65_design_label(&absolute_plan, 0);
    let taus = [500.0, 520.0, 490.0, 530.0, 510.0];
    let mut responses = Vec::new();
    for (channel_index, channel) in absolute_plan.participants.iter().enumerate() {
        let raw = t65_raw_points(absolute_plan.sample_rate_hz, 0, taus[channel_index], 1);
        responses.push(
            DePhasedResponse::from_absolute(
                &absolute_label,
                channel,
                absolute_plan.sample_rate_hz,
                raw,
            )
            .unwrap(),
        );
    }
    let output =
        t65_fit(&absolute_plan, &responses, t65_options(None)).expect("absolute basis fit");
    t65_assert_ok(&output);
    assert_eq!(output.report.phase_basis, "absolute");
    let expected = t65_expected_delays(&taus);
    for (channel_index, channel) in absolute_plan.participants.iter().enumerate() {
        assert_eq!(
            t65_channel(&output, channel).delay_samples,
            expected[channel_index]
        );
    }
    // 非 P1 采样率拒绝。
    let raw = vec![PhaseFitPoint::new(100.0, 1.0, 0.0)];
    let error = DePhasedResponse::from_aligned_segment("D0", "FL", 44_100, raw, d0).unwrap_err();
    assert_eq!(error.code(), "E_PHF_FS");
}

// ---------------------------------------------------------------------------
// 9) 契约上限：D > floor(0.2*fs) 报错而不是截断
// ---------------------------------------------------------------------------

#[test]
fn t65_phase_fit_contract_rejects_over_200ms_delay() {
    let plan = t65_plan(1, 1);
    let label = t65_design_label(&plan, 0);
    // FL 比最晚通道早 9800 samples -> D=9800 > 9600@48k（200 ms）。
    let taus = [-9_700.0, 100.0, 50.0, 0.0, -50.0];
    let responses = t65_responses(&plan, &label, &taus, &[1, 1, 1, 1, 1]);
    let options = PhaseFitOptions {
        search_range_ms: 300.0,
        ..t65_options(None)
    };
    let error = t65_fit(&plan, &responses, options).expect_err("D must not be truncated");
    assert_eq!(error.code(), "E_PHF_DELAY_LIMIT");
}

// ---------------------------------------------------------------------------
// 10) 搜索窗边界：峰落在边界 -> 报错要求扩大搜索窗（不猜）
// ---------------------------------------------------------------------------

#[test]
fn t65_phase_fit_search_boundary_rejects_instead_of_guessing() {
    let plan = t65_plan(1, 1);
    let label = t65_design_label(&plan, 0);
    // 默认搜索 ±50 ms = ±2400 samples；真值 -2403 恰在窗口外但主瓣仍触及边界 -> 峰落边界。
    let taus = [-2_403.0, 0.0, 0.0, 0.0, 0.0];
    let responses = t65_responses(&plan, &label, &taus, &[1, 1, 1, 1, 1]);
    let error = t65_fit(&plan, &responses, t65_options(None)).expect_err("search boundary");
    assert_eq!(error.code(), "E_PHF_DELAY_SEARCH");
    // 远在窗外（-5000 samples）时匹配峰落到旁瓣：按「置信不足」拒绝，同样不猜。
    let taus = [-5_000.0, 0.0, 0.0, 0.0, 0.0];
    let responses = t65_responses(&plan, &label, &taus, &[1, 1, 1, 1, 1]);
    let error = t65_fit(&plan, &responses, t65_options(None)).expect_err("out of search window");
    assert!(
        error.code() == "E_PHF_DELAY_SEARCH" || error.code() == "E_PHF_DELAY_LOW_CONFIDENCE",
        "out-of-window delay must not be guessed (got {:?})",
        error.code()
    );
}

// ---------------------------------------------------------------------------
// 11) FIR 执行 phase 方向：conj(H) 正确对齐；乘 H（错误方向）必须放大错位
// ---------------------------------------------------------------------------

fn t65_wrong_direction_apply(points: &[PhaseFitPoint], design: &PhaseDesign) -> Vec<PhaseFitPoint> {
    points
        .iter()
        .map(|point| {
            let response = design.response_at(point.freq_hz);
            PhaseFitPoint::new(
                point.freq_hz,
                point.re * response.re - point.im * response.im,
                point.re * response.im + point.im * response.re,
            )
        })
        .collect()
}

fn t65_residual_delay(points: &[PhaseFitPoint], options: &PhaseFitOptions) -> f64 {
    estimate_delay(points, T65_FS as f64, DEFAULT_DELAY_BAND_HZ, options)
        .expect("residual delay estimate")
        .delay_samples
}

#[test]
fn t65_phase_fit_fir_execution_direction_is_correct() {
    let plan = t65_plan(1, 1);
    let label = t65_design_label(&plan, 0);
    let taus = [100.0, 120.0, 90.0, 130.0, 110.0];
    let polarities = [1, -1, 1, 1, -1];
    let responses = t65_responses(&plan, &label, &taus, &polarities);
    let options = t65_options(None);
    let output = t65_fit(&plan, &responses, options.clone()).expect("fit must succeed");
    t65_assert_ok(&output);
    let reference_index = T65_PARTICIPANTS
        .iter()
        .position(|channel| *channel == output.report.reference_channel)
        .unwrap();
    let reference_fir = &output.firs[reference_index];
    let reference_response = responses
        .iter()
        .find(|response| response.channel() == output.report.reference_channel)
        .unwrap();
    let reference_compensated =
        apply_measured_domain_fir(reference_response.points(), &reference_fir.design);
    let reference_residual = t65_residual_delay(&reference_compensated, &options);
    for (channel_index, channel) in T65_PARTICIPANTS.iter().enumerate() {
        let design = &output.firs[channel_index].design;
        // T40 系数本身：h[n]=sign·delta[n-D]，最小因果支撑 D+1。
        let delay = output.ops[channel_index].delay_samples as usize;
        assert_eq!(design.taps, delay + 1);
        assert_eq!(
            design.coefficients[delay],
            output.ops[channel_index].polarity.sign()
        );
        let response = responses
            .iter()
            .find(|response| response.channel() == *channel)
            .unwrap();
        // 正确方向：乘 conj(H) -> 相对到达差 ≈ 0（<=0.5 sample）。
        let compensated = apply_measured_domain_fir(response.points(), design);
        let residual = t65_residual_delay(&compensated, &options);
        assert!(
            (residual - reference_residual).abs() <= 0.5,
            "{channel}: conj(H) direction residual {} vs reference {} must align",
            residual,
            reference_residual
        );
        // 错误方向：乘 H -> 相对差 = -2·D_i，必须明显失败。
        let wrong = t65_wrong_direction_apply(response.points(), design);
        let wrong_residual = t65_residual_delay(&wrong, &options);
        let wrong_delta = wrong_residual - reference_residual;
        let expected_wrong = -2.0 * delay as f64;
        assert!(
            (wrong_delta - expected_wrong).abs() <= 0.5,
            "{channel}: wrong-direction delta {wrong_delta} should be -2*D={expected_wrong}",
        );
        if delay > 0 {
            assert!(
                wrong_delta.abs() > options.execution_tolerance_samples,
                "{channel}: wrong direction must fail the execution tolerance",
            );
        } else {
            assert!(
                wrong_delta.abs() <= options.execution_tolerance_samples,
                "{channel}: reference D=0 is invariant under either direction",
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 12) 分数样本残差与正相位斜率符号（延迟 >0 = 更晚到达）
// ---------------------------------------------------------------------------

#[test]
fn t65_phase_fit_fractional_residual_and_sign_convention() {
    let plan = t65_plan(1, 1);
    let label = t65_design_label(&plan, 0);
    let taus = [100.4, 100.0, 100.6, 100.2, 100.8];
    let responses = t65_responses(&plan, &label, &taus, &[1, 1, 1, 1, 1]);
    let output = t65_fit(&plan, &responses, t65_options(None)).expect("fit");
    t65_assert_ok(&output);
    let expected_d = t65_expected_delays(&taus);
    assert_eq!(expected_d, vec![0, 1, 0, 1, 0]);
    for (channel_index, channel) in T65_PARTICIPANTS.iter().enumerate() {
        let report = t65_channel(&output, channel);
        assert_eq!(report.delay_samples, expected_d[channel_index]);
        let true_raw = 100.8 - taus[channel_index];
        assert!(
            (report.delay_raw_samples - true_raw).abs() < 0.05,
            "{channel} raw {} vs {true_raw}",
            report.delay_raw_samples
        );
        assert!(report.fractional_residual_samples.abs() <= 0.5 + 1e-12);
    }
    // 正相位斜率 = 更晚到达：单独一个 +250 samples 的响应估计应为正 250。
    let single_plan = t65_plan_with(
        1,
        1,
        0,
        "absolute_phase_deg (already total phase)",
        vec!["FL".to_string()],
    );
    let single_label = t65_design_label(&single_plan, 0);
    let raw = t65_raw_points(single_plan.sample_rate_hz, 0, 250.0, 1);
    let response =
        DePhasedResponse::from_absolute(&single_label, "FL", single_plan.sample_rate_hz, raw)
            .unwrap();
    let output = t65_fit(&single_plan, &[response], t65_options(None)).expect("single channel fit");
    t65_assert_ok(&output);
    let report = t65_channel(&output, "FL");
    assert!(
        (report.delay_raw_samples - 0.0).abs() < 0.05,
        "single channel has no relative delay"
    );
    let estimate = output.report.channels[0].positions[0].delay_raw_samples;
    assert!(
        (estimate - 250.0).abs() < 0.05,
        "raw residual delay must be +250 (sign convention): {estimate}"
    );
}

// ---------------------------------------------------------------------------
// 13) 确定性 JSON / artifact sha256 / 目录写盘
// ---------------------------------------------------------------------------

#[test]
fn t65_phase_fit_deterministic_json_and_artifact_write() {
    let plan = t65_plan(1, 1);
    let label = t65_design_label(&plan, 0);
    let taus = [100.0, 90.0, 120.0, 110.0, 130.0];
    let responses = t65_responses(&plan, &label, &taus, &[1, 1, 1, 1, 1]);
    let first = t65_fit(&plan, &responses, t65_options(None)).expect("fit 1");
    let second = t65_fit(&plan, &responses, t65_options(None)).expect("fit 2");
    let first_json = first.report_json();
    let second_json = second.report_json();
    assert_eq!(first_json, second_json, "report JSON must be deterministic");
    assert_eq!(
        first.ops_json(),
        second.ops_json(),
        "ops JSON must be deterministic"
    );
    assert!(first_json.contains("\"schema\":\"tspi-phase-fit/v1\""));
    assert!(first_json.contains("\"all_pass\":true"));
    assert!(first
        .ops_json()
        .contains("\"schema\":\"tspi-phase-ops/v1\""));
    assert_eq!(first.artifacts.entries.len(), 2);
    assert_eq!(
        first.artifacts.report_sha256,
        sha256_hex(first_json.as_bytes()),
        "report artifact sha256"
    );
    let dir = t65_target_tmp("deterministic");
    let written = first.artifacts.write_to_dir(&dir).expect("write artifacts");
    assert_eq!(written.len(), 2);
    for (entry, path) in first.artifacts.entries.iter().zip(written.iter()) {
        let bytes = fs::read(path).unwrap();
        assert_eq!(bytes, entry.bytes);
        assert_eq!(sha256_hex(&bytes), entry.sha256);
        assert!(path.ends_with(&entry.path));
    }
    let _ = fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 14) 选项/响应点非法输入全部拒绝
// ---------------------------------------------------------------------------

#[test]
fn t65_phase_fit_rejects_bad_options_and_points() {
    let plan = t65_plan(1, 1);
    let label = t65_design_label(&plan, 0);
    let responses = t65_responses(
        &plan,
        &label,
        &[10.0, 20.0, 30.0, 40.0, 50.0],
        &[1, 1, 1, 1, 1],
    );
    for bad in [
        PhaseFitOptions {
            delay_band_hz: [10.0, 4_000.0],
            ..t65_options(None)
        },
        PhaseFitOptions {
            polarity_band_hz: [200.0, 100.0],
            ..t65_options(None)
        },
        PhaseFitOptions {
            max_delay_ms: 300.0,
            ..t65_options(None)
        },
        PhaseFitOptions {
            search_range_ms: 0.0,
            ..t65_options(None)
        },
        PhaseFitOptions {
            min_points: 2,
            ..t65_options(None)
        },
        PhaseFitOptions {
            execution_tolerance_samples: 0.0,
            ..t65_options(None)
        },
    ] {
        let error = t65_fit(&plan, &responses, bad).expect_err("bad options must be rejected");
        assert_eq!(error.code(), "E_PHF_CONFIG");
    }
    // 响应点：空/非有限/非递增/越 fs/2。
    let error = DePhasedResponse::from_absolute("D0", "FL", T65_FS, Vec::new()).unwrap_err();
    assert_eq!(error.code(), "E_PHF_RESPONSE");
    let error = DePhasedResponse::from_absolute(
        "D0",
        "FL",
        T65_FS,
        vec![PhaseFitPoint::new(100.0, f64::NAN, 0.0)],
    )
    .unwrap_err();
    assert_eq!(error.code(), "E_PHF_RESPONSE");
    let error = DePhasedResponse::from_absolute(
        "D0",
        "FL",
        T65_FS,
        vec![
            PhaseFitPoint::new(1_000.0, 1.0, 0.0),
            PhaseFitPoint::new(100.0, 1.0, 0.0),
        ],
    )
    .unwrap_err();
    assert_eq!(error.code(), "E_PHF_RESPONSE");
    let error = DePhasedResponse::from_absolute(
        "D0",
        "FL",
        T65_FS,
        vec![PhaseFitPoint::new(T65_FS as f64 / 2.0, 1.0, 0.0)],
    )
    .unwrap_err();
    assert_eq!(error.code(), "E_PHF_RESPONSE");
}

// ---------------------------------------------------------------------------
// 15) 频带内有效点不足 -> E_PHF_BAND（不猜）
// ---------------------------------------------------------------------------

#[test]
fn t65_phase_fit_band_insufficient_rejects() {
    let plan = t65_plan(1, 1);
    let label = t65_design_label(&plan, 0);
    let mut responses = t65_responses(
        &plan,
        &label,
        &[10.0, 20.0, 30.0, 40.0, 50.0],
        &[1, 1, 1, 1, 1],
    );
    let low_grid: Vec<f64> = t65_grid()
        .into_iter()
        .filter(|frequency| *frequency <= 100.0)
        .collect();
    let raw = t65_raw_points_on(
        &low_grid,
        plan.sample_rate_hz,
        plan.common_d0.samples(),
        30.0,
        1,
        &t65_flat_magnitude,
        &t65_zero_phase,
    );
    let fl = DePhasedResponse::from_aligned_segment(
        &label,
        "FL",
        plan.sample_rate_hz,
        raw,
        plan.common_d0,
    )
    .unwrap();
    responses[0] = fl;
    let error = t65_fit(&plan, &responses, t65_options(None)).expect_err("band insufficient");
    assert_eq!(error.code(), "E_PHF_BAND");
}

// ---------------------------------------------------------------------------
// 16) 与 T63 质量报告极性交叉核对（矛盾 -> 拒绝；一致 -> 报告标记）
// ---------------------------------------------------------------------------

fn t65_quality_with_polarity(
    plan: &MultipointPlan,
    position: &str,
    channel: &str,
    polarity: i32,
) -> PositionQualityReport {
    let mut report = t65_quality(plan, true);
    let measurement = MeasurementQuality {
        speaker: channel.to_string(),
        repeats_supplied: 3,
        repeats_valid: 3,
        usable: true,
        snr_db_min: Some(35.0),
        snr_lowband_db_min: Some(30.0),
        clock_ppm: Some(0.1),
        clock_ppm_uncertainty_ppm: Some(0.2),
        delay_spread_samples: Some(0.05),
        magnitude_spread_db: Some(0.1),
        polarity: Some(polarity),
        coherence_min: Some(0.98),
        coherence_available: true,
        reasons: Vec::new(),
    };
    report.points.push(PositionQuality {
        label: position.to_string(),
        role: PositionRole::Design,
        weight: 1.0,
        usable: true,
        reasons: Vec::new(),
        measurements: vec![measurement],
    });
    report
}

#[test]
fn t65_phase_fit_t63_polarity_cross_check() {
    let plan = t65_plan(1, 1);
    let label = t65_design_label(&plan, 0);
    let taus = [10.0, 20.0, 30.0, 40.0, 50.0];
    let responses = t65_responses(&plan, &label, &taus, &[1, 1, 1, 1, 1]);
    let strict = PhaseFitOptions {
        reference_position: None,
        ..PhaseFitOptions::default()
    };
    // T63 生产极性 -1 与实测 +1 矛盾 -> 拒绝（不静默采用任一方）。
    let conflict = t65_quality_with_polarity(&plan, &label, "FL", -1);
    let error = t65_fit_with_quality(&plan, &responses, &conflict, strict.clone())
        .expect_err("polarity conflict");
    assert_eq!(error.code(), "E_PHF_POLARITY_CONFLICT");
    // 一致 -> 通过并写 producer_polarity_agrees=true。
    let agree = t65_quality_with_polarity(&plan, &label, "FL", 1);
    let output = t65_fit_with_quality(&plan, &responses, &agree, strict).expect("agreement fit");
    t65_assert_ok(&output);
    assert_eq!(
        t65_channel(&output, "FL").producer_polarity_agrees,
        Some(true)
    );
    assert_eq!(t65_channel(&output, "FR").producer_polarity_agrees, None);
}

// ---------------------------------------------------------------------------
// 17) 单通道 plan：D=0、参考自身、无相对极性交叉验证（warning）
// ---------------------------------------------------------------------------

#[test]
fn t65_phase_fit_single_channel_plan() {
    let plan = t65_plan_with(1, 1, T65_D0, T65_PHASE_REF, vec!["FL".to_string()]);
    let label = t65_design_label(&plan, 0);
    let responses = t65_responses(&plan, &label, &[120.0], &[1]);
    let output = t65_fit(&plan, &responses, t65_options(None)).expect("single channel");
    t65_assert_ok(&output);
    assert_eq!(output.report.reference_channel, "FL");
    assert_eq!(t65_channel(&output, "FL").delay_samples, 0);
    assert_eq!(t65_channel(&output, "FL").delay_raw_samples, 0.0);
    assert!(output
        .report
        .warnings
        .iter()
        .any(|warning| warning.contains("单通道")));
}

// ---------------------------------------------------------------------------
// 18) 角色→设备通道号映射（CONTRACT index / reference_index）
// ---------------------------------------------------------------------------

#[test]
fn t65_phase_fit_channel_index_mapping() {
    let plan = t65_plan(1, 1);
    let label = t65_design_label(&plan, 0);
    let taus = [100.0, 90.0, 120.0, 110.0, 130.0];
    let responses = t65_responses(&plan, &label, &taus, &[1, 1, 1, 1, 1]);
    let indices = [
        PhaseChannelIndex::new("FL", 0),
        PhaseChannelIndex::new("FR", 1),
        PhaseChannelIndex::new("SL", 2),
        PhaseChannelIndex::new("SR", 3),
        PhaseChannelIndex::new("SUB1", 7),
    ];
    let request = PhaseFitRequest::new(&plan, &responses)
        .with_options(t65_options(None))
        .with_channel_indices(&indices);
    let output = fit_phase(&request).expect("index mapping");
    assert_eq!(output.report.reference_index, Some(7));
    assert_eq!(output.report.reference_channel, "SUB1");
    assert_eq!(t65_channel(&output, "SUB1").index, Some(7));
    assert_eq!(output.ops[4].index, Some(7));
    // 重复 index / 未知通道 -> 拒绝。
    let duplicates = [
        PhaseChannelIndex::new("FL", 0),
        PhaseChannelIndex::new("FR", 0),
        PhaseChannelIndex::new("SL", 2),
        PhaseChannelIndex::new("SR", 3),
        PhaseChannelIndex::new("SUB1", 7),
    ];
    let request = PhaseFitRequest::new(&plan, &responses)
        .with_options(t65_options(None))
        .with_channel_indices(&duplicates);
    assert_eq!(fit_phase(&request).unwrap_err().code(), "E_PHF_CONFIG");
    let unknown = [PhaseChannelIndex::new("HFL", 4)];
    let request = PhaseFitRequest::new(&plan, &responses)
        .with_options(t65_options(None))
        .with_channel_indices(&unknown);
    assert_eq!(fit_phase(&request).unwrap_err().code(), "E_PHF_CONFIG");
}

// ---------------------------------------------------------------------------
// 19) 证据样例（TSPI_T65_EVIDENCE_DIR 设置时写 evidence/T65 JSON）
// ---------------------------------------------------------------------------

#[test]
fn t65_phase_fit_writes_evidence_examples_when_requested() {
    let evidence_dir = match std::env::var("TSPI_T65_EVIDENCE_DIR") {
        Ok(value) if !value.trim().is_empty() => PathBuf::from(value),
        _ => return,
    };
    fs::create_dir_all(&evidence_dir).unwrap();
    let plan = t65_plan(2, 1);
    let label0 = t65_design_label(&plan, 0);
    let _label1 = t65_design_label(&plan, 1);
    let taus0 = [100.3, 90.2, 120.7, 110.1, 130.5];
    let taus1: Vec<f64> = taus0.iter().map(|tau| tau + 1.5).collect();
    // SR 物理反相（calibration invert 样例）。
    let polarities = [1, 1, 1, -1, 1];
    let responses = t65_responses_all(&plan, &[taus0.to_vec(), taus1], &polarities);
    let indices = [
        PhaseChannelIndex::new("FL", 0),
        PhaseChannelIndex::new("FR", 1),
        PhaseChannelIndex::new("SL", 2),
        PhaseChannelIndex::new("SR", 3),
        PhaseChannelIndex::new("SUB1", 7),
    ];
    let quality = t65_quality(&plan, true);
    let request = PhaseFitRequest::new(&plan, &responses)
        .with_options(PhaseFitOptions {
            reference_position: Some(label0.clone()),
            ..PhaseFitOptions::default()
        })
        .with_channel_indices(&indices)
        .with_design_quality(&quality)
        .with_created_unix(1_762_000_000);
    let output = fit_phase(&request).expect("evidence fit");
    t65_assert_ok(&output);
    let report_path = evidence_dir.join("phase-fit-example.json");
    let ops_path = evidence_dir.join("phase-ops-example.json");
    fs::write(&report_path, output.report_json()).unwrap();
    fs::write(&ops_path, output.ops_json()).unwrap();
    assert!(report_path.exists());
    assert!(ops_path.exists());
    assert!(fs::read_to_string(&report_path)
        .unwrap()
        .contains("tspi-phase-fit/v1"));
    assert!(fs::read_to_string(&ops_path)
        .unwrap()
        .contains("tspi-phase-ops/v1"));
}

// ---------------------------------------------------------------------------
// 20) 时域卷积 vs 测量域 conj(H)：真实信号级验证 FIR 方向
// ---------------------------------------------------------------------------

fn t65_dft(signal: &[f64], fs: f64, frequency: f64) -> (f64, f64) {
    let omega = 2.0 * std::f64::consts::PI * frequency / fs;
    let mut re = 0.0_f64;
    let mut im = 0.0_f64;
    for (index, value) in signal.iter().enumerate() {
        let (sin, cos) = (omega * index as f64).sin_cos();
        re += value * cos;
        im -= value * sin;
    }
    (re, im)
}

/// T49 口径测量响应：X_meas(f) = conj(Y(f)/X(f)) = conj(Y)·X/|X|²。
fn t65_measured_response(
    signal: &[f64],
    reference: &[f64],
    fs: f64,
    grid: &[f64],
) -> Vec<PhaseFitPoint> {
    grid.iter()
        .map(|frequency| {
            let (y_re, y_im) = t65_dft(signal, fs, *frequency);
            let (x_re, x_im) = t65_dft(reference, fs, *frequency);
            let denom = x_re * x_re + x_im * x_im;
            let quotient_re = (y_re * x_re + y_im * x_im) / denom;
            let quotient_im = (y_im * x_re - y_re * x_im) / denom;
            PhaseFitPoint::new(*frequency, quotient_re, -quotient_im)
        })
        .collect()
}

fn t65_shifted(signal: &[f64], delay: usize) -> Vec<f64> {
    let mut out = vec![0.0_f64; signal.len()];
    for index in delay..signal.len() {
        out[index] = signal[index - delay];
    }
    out
}

#[test]
fn t65_phase_fit_time_domain_convolution_matches_measured_domain_conj() {
    // 确定性宽带白噪激励 + 整数样本延迟 + 反相：物理信号级夹具。
    let mut state = 0x5eed_1234_5678_9abc_u64;
    let mut excitation = vec![0.0_f64; 1024];
    // 前 400 点非零 + 尾部零填充：移位不丢样点（Z 变换上 Y=X·e^{-jωτ} 精确成立）。
    for value in excitation.iter_mut().take(400) {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let unit = ((state >> 11) as f64) / ((1_u64 << 53) as f64);
        *value = unit - 0.5;
    }
    let fs = T65_FS as f64;
    let mut grid = Vec::new();
    let ratio = 2.0_f64.powf(1.0 / 48.0);
    let mut frequency = 200.0_f64;
    while frequency <= 4_000.0 {
        grid.push(frequency);
        frequency *= ratio;
    }
    let delays = [40_usize, 65_usize];
    let inversions = [1_i32, -1_i32];
    let mut signals = Vec::new();
    for (index, delay) in delays.iter().enumerate() {
        let shifted = t65_shifted(&excitation, *delay);
        let signal = if inversions[index] < 0 {
            shifted.into_iter().map(|value| -value).collect()
        } else {
            shifted
        };
        signals.push(signal);
    }
    let measured: Vec<Vec<PhaseFitPoint>> = signals
        .iter()
        .map(|signal| t65_measured_response(signal, &excitation, fs, &grid))
        .collect();
    // absolute plan：测量响应已是总相位（不再扣 D0/delay）。
    let plan = t65_plan_with(
        1,
        1,
        0,
        "absolute_phase_deg (already total phase)",
        vec!["FL".to_string(), "FR".to_string()],
    );
    let label = t65_design_label(&plan, 0);
    let responses: Vec<DePhasedResponse> = measured
        .iter()
        .enumerate()
        .map(|(index, points)| {
            DePhasedResponse::from_absolute(&label, ["FL", "FR"][index], T65_FS, points.clone())
                .unwrap()
        })
        .collect();
    let output = t65_fit(&plan, &responses, t65_options(None)).expect("time-domain fit");
    t65_assert_ok(&output);
    // FL 比 FR 早 25 samples；FR 是参考（D=0）。
    assert_eq!(t65_channel(&output, "FL").delay_samples, 25);
    assert_eq!(t65_channel(&output, "FR").delay_samples, 0);
    // 时域卷积 h 后的测量响应 == 测量域乘 conj(H)（证明 FIR 方向与 T49 共轭域一致）。
    for (channel_index, channel) in ["FL", "FR"].iter().enumerate() {
        let design = &output.firs[channel_index].design;
        let convolved = khaen_designer::phase::convolve_linear(
            &signals[channel_index],
            &design.coefficients,
        );
        let measured_after = t65_measured_response(&convolved, &excitation, fs, &grid);
        let measured_domain = apply_measured_domain_fir(&measured[channel_index], design);
        assert_eq!(measured_after.len(), measured_domain.len());
        for (time_point, domain_point) in measured_after.iter().zip(measured_domain.iter()) {
            assert!(
                (time_point.re - domain_point.re).abs() < 1e-9
                    && (time_point.im - domain_point.im).abs() < 1e-9,
                "{channel}: conj(H) measured-domain application must equal real convolution @ {} Hz",
                domain_point.freq_hz
            );
        }
    }
}
fn t65_wrap_deg(deg: f64) -> f64 {
    let mut value = deg.rem_euclid(360.0);
    if value > 180.0 {
        value -= 360.0;
    }
    value
}
