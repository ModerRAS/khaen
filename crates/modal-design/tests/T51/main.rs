//! T51 private integration tests — 因果有限 FIR 模态 solver（crate khaen-modal-design）。
//!
//! 运行（在 rt-stack/ 下）：cargo test -p khaen-modal-design solve
//! 文件位置：crates/modal-design/tests/T51/main.rs（leaf 私有测试；不写 public_registration.rs）。
//!
//! 覆盖（WBS T51 DoD）：
//! - 因果有限 FIR 最小算子：系数只有 t in [0,L_G)，完整 G=deltaD·I+g；自定义合成夹具
//!   精确恢复已知算子（可恢复满秩，闭式 A^-1，远优于 <=1% 要求）；
//! - 迭代解 CGLS：逐输入报告迭代/相对残差/收敛标记/停止原因；未收敛如实报告；
//! - 可恢复满秩夹具 + 欠定夹具：P=M 良态时精确恢复；P<M 时 Underdetermined + 警告，
//!   最小范数解不冒充可辨识；
//! - 未收敛/资源上界：ResourceLimited 返回 solution=None 的结构化报告（未知量/行/工作量
//!   三种超界），不 panic、不硬算；
//! - 与 T50 target/planner/内核接口对齐：Preserve 期望 G=deltaD·I（D=0）、Reduced 目标
//!   含声明 target delay（D 必须对齐）、完整 G 全部 cell、validate_solution 一致；
//! - 不写“消全部驻波”式承诺：claims 明确 predicted-only/no guarantee；静态守卫无 I/O。
//!
//! 本文件只构造内存夹具；不采音、不碰板端、不打开 ALSA、不写文件（独立 checker 的 dump
//! 走 stdout，由 evidence 脚本读取）。
// 迁移说明：本 crate 从 legacy/pre-rewrite 原样迁入，算法与数值路径不改写。下列 clippy 建议
// 属风格/数值改写建议（部分会改变 NaN/边界语义）；迁移期统一在 crate 级允许，避免为过 lint
// 改动算术路径——行为回归由 tests/ 的冻结数值测试保护。理由与 mimo-design 迁移先例一致。
#![allow(
    clippy::field_reassign_with_default,
    clippy::needless_range_loop,
    clippy::too_many_arguments,
    clippy::useless_format,
    clippy::useless_vec
)]

use khaen_modal_design::ingest::{sha256_hex, PositionRole};
use khaen_modal_design::solve::{
    solve, solve_from_target_plant, SolveError, SolveErrorKind, SolveMode, SolveOptions,
    SolvePosition, SolveProblem, SolveStatus, SolveWarning, MAX_SOLVE_TAPS, SOLVE_CLAIM,
    SOLVE_SCHEMA,
};
use khaen_modal_design::target::{
    build_stereo_strict_target, build_target_matrix, validate_solution,
    validate_stereo_strict_solution, BandDecay, CurveBinding, DecayTarget, EffectivePlant,
    FittedPostBaseline, FrozenPostBaseline, ModalParticipants, PostBaselineInput, PostRoleFit,
    RawPlant, RawPosition, RawResponse, TargetMatrix, TargetSpec, ToneApplicationProof,
    ToneBinding, P1_LAYOUT_4_1_2,
};
use khaen_modal_design::timebase::{CommonD0, ComplexPoint, PolaritySource, ResolvedPolarity};

// ===========================================================================
// 常量与夹具
// ===========================================================================

const FS: u32 = 48_000;
const D0_SAMPLES: i64 = 120;

/// 5x5 良态（对角占优）真实增益矩阵，用于 Reduced 精确可恢复夹具。
const GAINS_5: [[f64; 5]; 5] = [
    [1.00, 0.60, 0.40, 0.30, 0.20],
    [0.50, 1.10, 0.55, 0.35, 0.25],
    [0.35, 0.45, 1.20, 0.60, 0.40],
    [0.25, 0.30, 0.50, 1.30, 0.70],
    [0.20, 0.25, 0.35, 0.55, 1.40],
];

fn octave_grid(lo: f64, hi: f64) -> Vec<f64> {
    let mut freqs = Vec::new();
    let mut index = 0i32;
    loop {
        let freq = lo * 2f64.powf(index as f64 / 24.0);
        if freq > hi * (1.0 + 1.0e-12) {
            break;
        }
        freqs.push(freq);
        index += 1;
    }
    if let Some(last) = freqs.last() {
        if (*last - hi).abs() > 1.0e-9 * hi.max(1.0) {
            freqs.push(hi);
        }
    }
    freqs
}

fn grid() -> Vec<f64> {
    octave_grid(20.0, 2_000.0)
}

fn flat_points(grid: &[f64], gain_db: f64) -> Vec<ComplexPoint> {
    let magnitude = 10f64.powf(gain_db / 20.0);
    grid.iter()
        .map(|freq| ComplexPoint {
            freq_hz: *freq,
            re: magnitude,
            im: 0.0,
        })
        .collect()
}

fn gain_points(grid: &[f64], magnitude: f64) -> Vec<ComplexPoint> {
    grid.iter()
        .map(|freq| ComplexPoint {
            freq_hz: *freq,
            re: magnitude,
            im: 0.0,
        })
        .collect()
}

fn polarity(sign: i8) -> ResolvedPolarity {
    ResolvedPolarity::resolve(sign as i32, PolaritySource::Analysis).unwrap()
}

fn post_role(
    role_id: &str,
    out_index: u32,
    grid: &[f64],
    gain_db: f64,
    delay_samples: u32,
    sign: i8,
) -> PostRoleFit {
    PostRoleFit::analytic(
        role_id,
        out_index,
        FS,
        grid,
        None,
        delay_samples,
        sign,
        gain_db,
    )
    .unwrap()
}

fn make_baseline(grid: &[f64], participants: ModalParticipants) -> FrozenPostBaseline {
    let mut roles = Vec::new();
    for participant in participants.participants() {
        roles.push(post_role(
            &participant.role_id,
            participant.out_index,
            grid,
            0.0,
            0,
            1,
        ));
    }
    if participants.is_p1_explicit() {
        roles.push(post_role("HFL", 4, grid, 0.0, 0, 1));
        roles.push(post_role("HFR", 5, grid, 0.0, 0, 1));
    }
    let tone = ToneBinding::flat();
    let curve = CurveBinding::baseline_only(tone.curve_id()).unwrap();
    let input = PostBaselineInput {
        baseline_id: "t51-baseline".to_string(),
        sample_rate_hz: FS,
        participants,
        tone,
        curve,
        tone_applications: ToneApplicationProof::baseline_only(),
        fit_inputs_sha256: sha256_hex(b"t51-fit-inputs-v1"),
        common_d0: CommonD0::new(D0_SAMPLES).unwrap(),
        grid_hz: grid.to_vec(),
        roles,
        created_unix: None,
    };
    FittedPostBaseline::new(input).unwrap().freeze().unwrap()
}

fn normalized_response(role_id: &str, out_index: u32, points: Vec<ComplexPoint>) -> RawResponse {
    RawResponse::new(role_id, out_index, points)
        .unwrap()
        .normalize_polarity(polarity(1))
        .unwrap()
}

fn raw_position(
    label: &str,
    participants: &ModalParticipants,
    points_for: impl Fn(&str) -> Vec<ComplexPoint>,
) -> RawPosition {
    let mut responses = Vec::new();
    for participant in participants.participants() {
        responses.push(normalized_response(
            &participant.role_id,
            participant.out_index,
            points_for(&participant.role_id),
        ));
    }
    RawPosition::new(label, PositionRole::Design, 1.0, responses).unwrap()
}

fn make_plant(
    baseline: &FrozenPostBaseline,
    participants: &ModalParticipants,
    grid: &[f64],
    positions: Vec<RawPosition>,
) -> EffectivePlant {
    let raw = RawPlant::new(
        P1_LAYOUT_4_1_2,
        FS,
        participants.clone(),
        grid.to_vec(),
        positions,
    )
    .unwrap();
    EffectivePlant::build(baseline, &raw).unwrap()
}

fn preserve_decay() -> DecayTarget {
    DecayTarget::preserve(vec![
        BandDecay {
            band_hz: 63.0,
            baseline_t60_s: 0.40,
        },
        BandDecay {
            band_hz: 125.0,
            baseline_t60_s: 0.35,
        },
    ])
    .unwrap()
}

fn reduced_decay(ratio: f64) -> DecayTarget {
    DecayTarget::reduced(
        ratio,
        vec![
            BandDecay {
                band_hz: 63.0,
                baseline_t60_s: 0.40,
            },
            BandDecay {
                band_hz: 125.0,
                baseline_t60_s: 0.35,
            },
        ],
    )
    .unwrap()
}

fn build_preserve_target(baseline: &FrozenPostBaseline, plant: &EffectivePlant) -> TargetMatrix {
    let spec = TargetSpec::for_baseline(baseline, preserve_decay()).unwrap();
    build_target_matrix(baseline, plant, &spec).unwrap()
}

fn build_reduced_target(baseline: &FrozenPostBaseline, plant: &EffectivePlant) -> TargetMatrix {
    let spec = TargetSpec::for_baseline(baseline, reduced_decay(0.85)).unwrap();
    build_target_matrix(baseline, plant, &spec).unwrap()
}

fn build_reduced_target_with_delay(
    baseline: &FrozenPostBaseline,
    plant: &EffectivePlant,
    delay_samples: u32,
) -> TargetMatrix {
    let spec = TargetSpec::for_baseline(baseline, reduced_decay(0.85))
        .unwrap()
        .with_target_delay_samples(delay_samples);
    build_target_matrix(baseline, plant, &spec).unwrap()
}

fn options(taps: usize) -> SolveOptions {
    let mut options = SolveOptions::default();
    options.taps = taps;
    options.max_iterations = 600;
    options.rel_tolerance = 1.0e-10;
    options
}

fn gain_positions(
    participants: &ModalParticipants,
    grid: &[f64],
    position_count: usize,
) -> Vec<RawPosition> {
    let mut positions = Vec::new();
    for position in 0..position_count {
        let label = format!("P{:02}", position + 1);
        positions.push(raw_position(&label, participants, |role_id| {
            let dense = participants
                .participants()
                .iter()
                .position(|participant| participant.role_id == role_id)
                .unwrap();
            gain_points(grid, GAINS_5[position][dense])
        }));
    }
    positions
}

fn flat_positions(
    participants: &ModalParticipants,
    grid: &[f64],
    position_count: usize,
    gain_db: f64,
) -> Vec<RawPosition> {
    let mut positions = Vec::new();
    for position in 0..position_count {
        let label = format!("P{:02}", position + 1);
        positions.push(raw_position(&label, participants, |_| {
            flat_points(grid, gain_db)
        }));
    }
    positions
}

/// 高斯消元解 A x = b（n<=8；测试夹具用）。
fn solve_linear(matrix: &[f64], n: usize, rhs: &[f64]) -> Vec<f64> {
    let mut a = matrix.to_vec();
    let mut b = rhs.to_vec();
    for column in 0..n {
        let mut pivot = column;
        for row in (column + 1)..n {
            if a[row * n + column].abs() > a[pivot * n + column].abs() {
                pivot = row;
            }
        }
        if pivot != column {
            for k in 0..n {
                a.swap(column * n + k, pivot * n + k);
            }
            b.swap(column, pivot);
        }
        let diagonal = a[column * n + column];
        for k in column..n {
            a[column * n + k] /= diagonal;
        }
        b[column] /= diagonal;
        for row in 0..n {
            if row == column {
                continue;
            }
            let factor = a[row * n + column];
            if factor == 0.0 {
                continue;
            }
            for k in column..n {
                a[row * n + k] -= factor * a[column * n + k];
            }
            b[row] -= factor * b[column];
        }
    }
    b
}

fn mat_inverse(matrix: &[f64], n: usize) -> Vec<f64> {
    let mut inverse = vec![0.0f64; n * n];
    for column in 0..n {
        let mut rhs = vec![0.0f64; n];
        rhs[column] = 1.0;
        let solution = solve_linear(matrix, n, &rhs);
        for row in 0..n {
            inverse[row * n + column] = solution[row];
        }
    }
    inverse
}

fn expect_kind<T>(result: Result<T, SolveError>, kind: SolveErrorKind) -> SolveError {
    match result {
        Ok(_) => panic!("expected SolveError({kind:?}), got Ok"),
        Err(error) => {
            assert_eq!(error.kind(), kind, "unexpected error: {error}");
            error
        }
    }
}

fn custom_problem(
    m: usize,
    position_count: usize,
    grid: &[f64],
    h: &dyn Fn(usize, usize, f64) -> (f64, f64),
    target: &dyn Fn(usize, usize, f64) -> (f64, f64),
    mode: SolveMode,
    variant: bool,
    target_delay_samples: u32,
) -> SolveProblem {
    let role_pool = ["FL", "FR", "SL", "SR", "SUB1", "C", "SUB2", "SUB3"];
    let roles: Vec<String> = role_pool[..m].iter().map(|role| role.to_string()).collect();
    let mut positions = Vec::with_capacity(position_count);
    for position in 0..position_count {
        let mut h_eff = Vec::with_capacity(m);
        for output_dense in 0..m {
            let mut points = Vec::with_capacity(grid.len());
            for freq in grid {
                let (re, im) = h(position, output_dense, *freq);
                points.push(ComplexPoint {
                    freq_hz: *freq,
                    re,
                    im,
                });
            }
            h_eff.push(points);
        }
        let mut target_rows = Vec::with_capacity(m);
        for input_dense in 0..m {
            let mut points = Vec::with_capacity(grid.len());
            for freq in grid {
                let (re, im) = target(position, input_dense, *freq);
                points.push(ComplexPoint {
                    freq_hz: *freq,
                    re,
                    im,
                });
            }
            target_rows.push(points);
        }
        positions.push(SolvePosition {
            label: format!("P{:02}", position + 1),
            weight: 1.0,
            h_eff,
            target: target_rows,
        });
    }
    SolveProblem {
        roles,
        grid_hz: grid.to_vec(),
        sample_rate_hz: FS,
        positions,
        mode,
        target_position_variant: variant,
        target_delay_samples,
        provenance: None,
    }
}

fn delay_rotation(freq: f64, delay: f64) -> (f64, f64) {
    let angle = -2.0 * std::f64::consts::PI * freq * delay / FS as f64;
    (angle.cos(), angle.sin())
}

fn complex_mul(left: (f64, f64), right: (f64, f64)) -> (f64, f64) {
    (
        left.0 * right.0 - left.1 * right.1,
        left.0 * right.1 + left.1 * right.0,
    )
}

fn assert_close(actual: f64, expected: f64, tolerance: f64, context: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{context}: actual {actual} vs expected {expected} (tol {tolerance})"
    );
}

// ===========================================================================
// 1. Preserve：期望 G=deltaD·I
// ===========================================================================

#[test]
fn t51_solve_adapter_preserve_identity_baseline() {
    let grid = grid();
    let participants = ModalParticipants::p1_4_1_2();
    let baseline = make_baseline(&grid, participants.clone());
    let plant = make_plant(
        &baseline,
        &participants,
        &grid,
        flat_positions(&participants, &grid, 2, -6.0),
    );
    let target = build_preserve_target(&baseline, &plant);
    let outcome = solve_from_target_plant(&target, &plant, &options(8)).unwrap();

    assert_eq!(outcome.report.status, SolveStatus::PreserveBaseline);
    assert!(outcome.report.status.is_usable());
    assert!(outcome.report.preserve_expected_identity);
    assert_eq!(outcome.report.mode, SolveMode::Preserve);
    assert!(outcome
        .report
        .warnings
        .iter()
        .any(|warning| warning.code == SolveWarning::PRESERVE_IDENTITY));
    assert_eq!(outcome.report.predicted_design_residual_max_db, 0.0);
    let solution = outcome.solution.expect("preserve must produce identity G");
    assert!(solution.coeffs_f64.iter().all(|value| *value == 0.0));
    assert_eq!(solution.modal.m(), 5);
    assert_eq!(solution.modal.cells().len(), 25);
    for out_dense in 0..5u16 {
        for in_dense in 0..5u16 {
            let points = solution.response(out_dense, in_dense).unwrap();
            for point in points {
                if out_dense == in_dense {
                    assert_close(point.re, 1.0, 1.0e-12, "identity diag re");
                    assert_close(point.im, 0.0, 1.0e-12, "identity diag im");
                } else {
                    assert_close(point.re, 0.0, 1.0e-12, "identity offdiag re");
                    assert_close(point.im, 0.0, 1.0e-12, "identity offdiag im");
                }
            }
        }
    }
    let validation = validate_solution(&target, &plant, &solution.modal).unwrap();
    assert!(
        validation.passed,
        "preserve identity must validate: {validation:?}"
    );
    assert!(validation
        .residual_max_db
        .map(|residual| residual < 1.0e-9)
        .unwrap_or(true));
    assert_eq!(solution.hash.len(), 64);
    assert_eq!(solution.coeffs_sha256.len(), 64);
    assert!(!outcome.report.applied);
    assert!(!outcome.report.holdout_used_for_tuning);
}

#[test]
fn t51_solve_adapter_preserve_rejects_nonzero_delay() {
    let grid = grid();
    let participants = ModalParticipants::p1_4_1_2();
    let baseline = make_baseline(&grid, participants.clone());
    let plant = make_plant(
        &baseline,
        &participants,
        &grid,
        flat_positions(&participants, &grid, 2, -6.0),
    );
    let target = build_preserve_target(&baseline, &plant);
    let mut overridden = options(8);
    overridden.delay_samples = 1;
    let error = expect_kind(
        solve_from_target_plant(&target, &plant, &overridden),
        SolveErrorKind::Delay,
    );
    assert_eq!(error.code(), "E_SOLVE_DELAY_MISMATCH");
    // preserve target 自带 target delay 属非法输入（T50 preserve 不含声明延迟）。
    let baseline2 = make_baseline(&grid, participants.clone());
    let spec = TargetSpec::for_baseline(&baseline2, preserve_decay())
        .unwrap()
        .with_target_delay_samples(1);
    let delayed_target = build_target_matrix(&baseline2, &plant, &spec).unwrap();
    expect_kind(
        solve_from_target_plant(&delayed_target, &plant, &options(8)),
        SolveErrorKind::Delay,
    );
}

// ===========================================================================
// 2. Reduced：可恢复满秩（闭式 A^-1 对照）
// ===========================================================================

#[test]
fn t51_solve_adapter_reduced_exact_recoverable_full_rank() {
    let grid = grid();
    let participants = ModalParticipants::p1_4_1_2();
    let baseline = make_baseline(&grid, participants.clone());
    let plant = make_plant(
        &baseline,
        &participants,
        &grid,
        gain_positions(&participants, &grid, 5),
    );
    let target = build_reduced_target(&baseline, &plant);
    // 该夹具的 Reduced 目标在频域上是恒定（实增益）→ 精确 G 为常数矩阵，L_G=1 即可精确可恢复；
    // 多 tap 窄带基近奇异会只保证频响、不保证系数稀疏（另由 wideband 夹具覆盖）。
    let outcome = solve_from_target_plant(&target, &plant, &options(1)).unwrap();

    assert_eq!(outcome.report.status, SolveStatus::Converged);
    assert!(!outcome.report.identifiability.underdetermined);
    assert_eq!(outcome.report.identifiability.per_frequency_rank_min, 5);
    assert_eq!(outcome.report.identifiability.position_deficit, 0);
    assert!(outcome.report.identifiability.potentially_full_column_rank);
    assert!(
        outcome.report.predicted_fit_band_residual_max_db < 1.0e-3,
        "design-point fit must be exact: {} dB",
        outcome.report.predicted_fit_band_residual_max_db
    );
    assert!(outcome
        .report
        .columns
        .iter()
        .all(|column| column.converged && column.relative_residual < 1.0e-9));
    let solution = outcome.solution.expect("converged solution");
    let validation = validate_solution(&target, &plant, &solution.modal).unwrap();
    assert!(
        validation.passed,
        "reduced solution must validate: {validation:?}"
    );

    // 闭式对照：target_i = sqrt(mean_p H[p][i]^2)（T50 功率 RMS 聚合），
    // H * G_col_i = target_i * ones  =>  G_col_i = target_i * H^{-1} ones。
    let h_flat: Vec<f64> = GAINS_5.iter().flat_map(|row| row.iter().copied()).collect();
    let h_inverse = mat_inverse(&h_flat, 5);
    let once = [1.0f64; 5];
    let mut h_inv_ones = vec![0.0f64; 5];
    for row in 0..5 {
        for column in 0..5 {
            h_inv_ones[row] += h_inverse[row * 5 + column] * once[column];
        }
    }
    let target_levels: Vec<f64> = (0..5)
        .map(|input_dense| {
            (0..5)
                .map(|position| GAINS_5[position][input_dense].powi(2) / 5.0)
                .sum::<f64>()
                .sqrt()
        })
        .collect();
    let mut worst = 0.0f64;
    for out_dense in 0..5usize {
        for in_dense in 0..5usize {
            let expected = target_levels[in_dense] * h_inv_ones[out_dense];
            let points = solution
                .response(out_dense as u16, in_dense as u16)
                .unwrap();
            for point in points {
                let deviation = (point.re - expected).abs().max(point.im.abs());
                worst = worst.max(deviation);
            }
        }
    }
    assert!(
        worst < 1.0e-5,
        "recoverable full-rank operator must match closed form (<=1% requirement), worst={worst}"
    );
    assert!(outcome.report.identifiability.condition_max.unwrap() <= 100.0);
}

#[test]
fn t51_solve_adapter_reduced_position_deficit_underdetermined() {
    let grid = grid();
    let participants = ModalParticipants::p1_4_1_2();
    let baseline = make_baseline(&grid, participants.clone());
    // 仅 2 个 design position < M=5：每频点欠定（计划硬规则 P<M 不得宣称可辨识）。
    let plant = make_plant(
        &baseline,
        &participants,
        &grid,
        gain_positions(&participants, &grid, 2),
    );
    let target = build_reduced_target(&baseline, &plant);
    let outcome = solve_from_target_plant(&target, &plant, &options(8)).unwrap();
    assert_eq!(outcome.report.status, SolveStatus::Underdetermined);
    assert!(!outcome.report.status.is_usable());
    assert!(outcome.report.identifiability.underdetermined);
    assert_eq!(outcome.report.identifiability.position_deficit, 3);
    assert_eq!(outcome.report.identifiability.per_frequency_rank_min, 2);
    assert_eq!(outcome.report.identifiability.condition_max, None);
    assert!(outcome
        .report
        .warnings
        .iter()
        .any(|warning| warning.code == SolveWarning::UNDERDETERMINED));
    let solution = outcome.solution.expect("min-norm best-effort solution");
    assert!(solution.coeffs_f64.iter().all(|value| value.is_finite()));
}

// ===========================================================================
// 3. Custom 合成满秩夹具：精确恢复已知算子（F1）
// ===========================================================================

const F1_MATRIX: [[f64; 3]; 3] = [[2.0, 0.5, 0.2], [0.4, 1.8, 0.3], [0.2, 0.5, 2.2]];
const F1_DELAYS: [f64; 3] = [2.0, 5.0, 9.0];

fn f1_grid() -> Vec<f64> {
    octave_grid(25.0, 400.0)
}

fn f1_problem() -> SolveProblem {
    let grid = f1_grid();
    let matrix = F1_MATRIX;
    let delays = F1_DELAYS;
    custom_problem(
        3,
        3,
        &grid,
        &move |position, output_dense, freq| {
            let rotation = delay_rotation(freq, delays[position]);
            complex_mul(rotation, (matrix[position][output_dense], 0.0))
        },
        &move |position, input_dense, freq| {
            if position == input_dense {
                delay_rotation(freq, delays[position])
            } else {
                (0.0, 0.0)
            }
        },
        SolveMode::Custom,
        false,
        0,
    )
}

#[test]
fn t51_solve_custom_full_rank_recovers_known_operator() {
    let problem = f1_problem();
    let outcome = solve(&problem, &options(4)).unwrap();
    assert_eq!(outcome.report.status, SolveStatus::Converged);
    assert!(outcome.report.identifiability.potentially_full_column_rank);
    assert_eq!(outcome.report.identifiability.per_frequency_rank_min, 3);
    assert!(
        outcome.report.predicted_fit_band_residual_max_db < 0.05,
        "F1 design-point prediction error must be <=0.05 dB, got {} dB",
        outcome.report.predicted_fit_band_residual_max_db
    );
    // 近零目标行（本夹具位置 p != 输入 i 的目标为结构零）不参与相对残差统计，只计数。
    assert!(outcome.report.near_zero_target_points > 0);
    let solution = outcome.solution.expect("full-rank fixture must solve");
    let flat: Vec<f64> = F1_MATRIX
        .iter()
        .flat_map(|row| row.iter().copied())
        .collect();
    let inverse = mat_inverse(&flat, 3);
    // 计划 F1 判据：设计频点上 ||G_design - A^{-1}||/||A^{-1}|| <= 1%（本夹具更严）。
    let mut worst_relative = 0.0f64;
    let mut inverse_norm = 0.0f64;
    for row in &inverse {
        inverse_norm += row * row;
    }
    inverse_norm = inverse_norm.sqrt();
    for out_dense in 0..3usize {
        for in_dense in 0..3usize {
            let expected = inverse[out_dense * 3 + in_dense];
            let points = solution
                .response(out_dense as u16, in_dense as u16)
                .unwrap();
            for point in points {
                let deviation = (point.re - expected).abs().max(point.im.abs());
                worst_relative = worst_relative.max(deviation);
            }
        }
    }
    assert!(
        worst_relative / inverse_norm <= 1.0e-2,
        "F1 recoverable full-rank operator must be recovered within <=1%, worst={worst_relative}"
    );
    // 因果有限 FIR：所有系数有限；G=delta0*I+g 已在 RHS 扣除。
    assert!(solution.coeffs_f64.iter().all(|value| value.is_finite()));

    // 独立 checker dump：A、网格、完整 G 复响应（Python 侧独立验证 A*G=I 与预测）。
    let mut dump = String::new();
    dump.push_str(&format!("\"m\":3,\"taps\":4,"));
    dump.push_str(&format!("\"fs\":{FS},\"delays\":[2.0,5.0,9.0],"));
    dump.push_str("\"a\":[");
    for (index, value) in flat.iter().enumerate() {
        if index > 0 {
            dump.push(',');
        }
        dump.push_str(&format!("{:.17e}", *value));
    }
    dump.push_str("],\"grid\":[");
    for (index, freq) in problem.grid_hz.iter().enumerate() {
        if index > 0 {
            dump.push(',');
        }
        dump.push_str(&format!("{:.17e}", *freq));
    }
    dump.push_str("],\"g_re\":[");
    let mut first = true;
    for out_dense in 0..3usize {
        for in_dense in 0..3usize {
            for point in solution
                .response(out_dense as u16, in_dense as u16)
                .unwrap()
            {
                if !first {
                    dump.push(',');
                }
                first = false;
                dump.push_str(&format!("{:.17e}", point.re));
            }
        }
    }
    dump.push_str("],\"g_im\":[");
    let mut first = true;
    for out_dense in 0..3usize {
        for in_dense in 0..3usize {
            for point in solution
                .response(out_dense as u16, in_dense as u16)
                .unwrap()
            {
                if !first {
                    dump.push(',');
                }
                first = false;
                dump.push_str(&format!("{:.17e}", point.im));
            }
        }
    }
    dump.push_str("],\"residual_db\":");
    dump.push_str(&format!(
        "{:.17e}",
        outcome.report.predicted_fit_band_residual_max_db
    ));
    println!("T51_F1_DUMP {{{dump}}}");
}

#[test]
fn t51_solve_custom_delay_aligned_operator() {
    let grid = f1_grid();
    let matrix = F1_MATRIX;
    let delays = F1_DELAYS;
    let target_delay = 24.0;
    let problem = custom_problem(
        3,
        3,
        &grid,
        &move |position, output_dense, freq| {
            let rotation = delay_rotation(freq, delays[position]);
            complex_mul(rotation, (matrix[position][output_dense], 0.0))
        },
        &move |position, input_dense, freq| {
            if position == input_dense {
                delay_rotation(freq, delays[position] + target_delay)
            } else {
                (0.0, 0.0)
            }
        },
        SolveMode::Custom,
        false,
        24,
    );
    let mut solver_options = options(32);
    solver_options.delay_samples = 24;
    let outcome = solve(&problem, &solver_options).unwrap();
    assert_eq!(outcome.report.status, SolveStatus::Converged);
    assert_eq!(outcome.report.delay_samples, 24);
    let solution = outcome.solution.expect("delay-aligned solution");
    let flat: Vec<f64> = F1_MATRIX
        .iter()
        .flat_map(|row| row.iter().copied())
        .collect();
    let inverse = mat_inverse(&flat, 3);
    // 期望完整 G = A^{-1} * z^-24（delta_24·I 基线 + tap D 修正；因果有限 FIR）。
    let mut worst = 0.0f64;
    for out_dense in 0..3usize {
        for in_dense in 0..3usize {
            let expected = inverse[out_dense * 3 + in_dense];
            let points = solution
                .response(out_dense as u16, in_dense as u16)
                .unwrap();
            for (index, point) in points.iter().enumerate() {
                let rotation = delay_rotation(problem.grid_hz[index], target_delay);
                let expected_point = complex_mul(rotation, (expected, 0.0));
                worst = worst.max(
                    (point.re - expected_point.0)
                        .abs()
                        .max((point.im - expected_point.1).abs()),
                );
            }
        }
    }
    assert!(
        worst < 1.0e-3,
        "target-delay aligned G must equal A^-1*z^-24, worst={worst}"
    );
    assert!(
        outcome.report.predicted_fit_band_residual_max_db < 0.05,
        "delay-aligned fit residual too large: {} dB",
        outcome.report.predicted_fit_band_residual_max_db
    );
}

// ===========================================================================
// 4. 未收敛 / 资源上界
// ===========================================================================

#[test]
fn t51_solve_non_converged_reports_iterations() {
    let grid = octave_grid(20.0, 400.0);
    let m = 2usize;
    let problem = custom_problem(
        m,
        2,
        &grid,
        &move |position, output_dense, freq| {
            let rotation = delay_rotation(freq, 3.0 + 2.0 * position as f64 + 1.0);
            let gain = 1.0 + 0.15 * (((position * 2 + output_dense * 3) % 4) as f64);
            complex_mul(rotation, (gain, 0.0))
        },
        // 不可因果实现的任意目标（故意不一致）→ 未收敛。
        &move |position, input_dense, _freq| {
            let phase = 0.3 + 0.2 * position as f64 + 0.1 * input_dense as f64;
            (phase.cos() * (0.7 + 0.1 * input_dense as f64), phase.sin())
        },
        SolveMode::Custom,
        false,
        0,
    );
    let mut solver_options = options(4);
    solver_options.max_iterations = 1;
    solver_options.rel_tolerance = 1.0e-12;
    let outcome = solve(&problem, &solver_options).unwrap();
    assert_eq!(outcome.report.status, SolveStatus::NonConverged);
    assert!(!outcome.report.status.is_usable());
    assert!(!outcome.report.identifiability.underdetermined);
    assert!(outcome
        .report
        .warnings
        .iter()
        .any(|warning| warning.code == SolveWarning::NON_CONVERGED));
    assert!(outcome
        .report
        .columns
        .iter()
        .all(|column| column.iterations == 1 && !column.converged));
    let solution = outcome.solution.expect("best-effort solution retained");
    assert!(solution.coeffs_f64.iter().all(|value| value.is_finite()));
}

#[test]
fn t51_solve_resource_bound_reports_without_solution() {
    let problem = f1_problem();
    // 未知量超界。
    let mut too_many = options(4);
    too_many.max_unknowns = 3;
    let outcome = solve(&problem, &too_many).unwrap();
    assert_eq!(outcome.report.status, SolveStatus::ResourceLimited);
    assert!(outcome.solution.is_none());
    assert!(!outcome.report.resource.within_limits);
    assert_eq!(outcome.report.resource.limited_by, Some("unknowns"));
    assert!(outcome
        .report
        .warnings
        .iter()
        .any(|warning| warning.code == SolveWarning::RESOURCE_BOUND));
    let json = outcome.to_evidence_json();
    assert!(json.contains("\"solution\":null"));
    assert!(json.contains("\"within_limits\":false"));

    // 复数行超界。
    let mut too_rows = options(4);
    too_rows.max_rows = 1;
    let outcome = solve(&problem, &too_rows).unwrap();
    assert_eq!(outcome.report.resource.limited_by, Some("rows"));

    // 工作量超界。
    let mut too_work = options(4);
    too_work.max_work = 1;
    let outcome = solve(&problem, &too_work).unwrap();
    assert_eq!(outcome.report.resource.limited_by, Some("work"));
    assert!(outcome.report.resource.estimated_work > 1);
    println!("T51_RESOURCE_JSON {}", outcome.to_evidence_json());
}

// ===========================================================================
// 5. 选项/问题负向拒收
// ===========================================================================

#[test]
fn t51_solve_options_rejections() {
    let problem = f1_problem();
    let base = options(4);

    let cases: Vec<(&str, SolveOptions, SolveErrorKind)> = vec![
        (
            "taps zero",
            {
                let mut o = base.clone();
                o.taps = 0;
                o
            },
            SolveErrorKind::Options,
        ),
        (
            "taps over hard cap",
            {
                let mut o = base.clone();
                o.taps = MAX_SOLVE_TAPS + 1;
                o
            },
            SolveErrorKind::Options,
        ),
        (
            "delay beyond support",
            {
                let mut o = base.clone();
                o.delay_samples = 4;
                o
            },
            SolveErrorKind::Options,
        ),
        (
            "band inverted",
            {
                let mut o = base.clone();
                o.fit_band_hz = (300.0, 20.0);
                o
            },
            SolveErrorKind::Grid,
        ),
        (
            "band above nyquist",
            {
                let mut o = base.clone();
                o.fit_band_hz = (20.0, 30_000.0);
                o
            },
            SolveErrorKind::Grid,
        ),
        (
            "ridge negative",
            {
                let mut o = base.clone();
                o.ridge = -1.0;
                o
            },
            SolveErrorKind::Options,
        ),
        (
            "ridge NaN",
            {
                let mut o = base.clone();
                o.ridge = f64::NAN;
                o
            },
            SolveErrorKind::Options,
        ),
        (
            "iterations zero",
            {
                let mut o = base.clone();
                o.max_iterations = 0;
                o
            },
            SolveErrorKind::Options,
        ),
        (
            "iterations over cap",
            {
                let mut o = base.clone();
                o.max_iterations = 5_000;
                o
            },
            SolveErrorKind::Options,
        ),
        (
            "tolerance zero",
            {
                let mut o = base.clone();
                o.rel_tolerance = 0.0;
                o
            },
            SolveErrorKind::Options,
        ),
        (
            "tolerance too loose",
            {
                let mut o = base.clone();
                o.rel_tolerance = 1.0e-1;
                o
            },
            SolveErrorKind::Options,
        ),
        (
            "max_unknowns zero",
            {
                let mut o = base.clone();
                o.max_unknowns = 0;
                o
            },
            SolveErrorKind::Options,
        ),
        (
            "max_rows zero",
            {
                let mut o = base.clone();
                o.max_rows = 0;
                o
            },
            SolveErrorKind::Options,
        ),
        (
            "max_work zero",
            {
                let mut o = base.clone();
                o.max_work = 0;
                o
            },
            SolveErrorKind::Options,
        ),
        (
            "path mask wrong length",
            {
                let mut o = base.clone();
                o.path_mask = Some(vec![true; 5]);
                o
            },
            SolveErrorKind::Options,
        ),
        (
            "path mask all false",
            {
                let mut o = base.clone();
                o.path_mask = Some(vec![false; 9]);
                o
            },
            SolveErrorKind::Options,
        ),
        (
            "frequency weights wrong length",
            {
                let mut o = base.clone();
                o.frequency_weights = Some(vec![1.0; 3]);
                o
            },
            SolveErrorKind::Options,
        ),
        (
            "frequency weight zero",
            {
                let mut o = base.clone();
                o.frequency_weights = Some(vec![0.0; problem.grid_hz.len()]);
                o
            },
            SolveErrorKind::Range,
        ),
    ];
    for (name, solver_options, kind) in cases {
        let error = match solve(&problem, &solver_options) {
            Ok(_) => panic!("option case {name:?} unexpectedly solved"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), kind, "option case {name:?}: {error}");
    }
    // 拟合带内网格点不足 → Grid（而不是静默解空系统）。
    let sparse = custom_problem(
        2,
        2,
        &[40.0, 80.0, 125.0, 200.0],
        &|_, _, _| (1.0, 0.0),
        &|_, _, _| (1.0, 0.0),
        SolveMode::Custom,
        false,
        0,
    );
    let mut narrow = options(4);
    narrow.fit_band_hz = (20.0, 30.0);
    expect_kind(solve(&sparse, &narrow), SolveErrorKind::Grid);
}

#[test]
fn t51_solve_problem_rejections() {
    let base = f1_problem();
    // 空 roles。
    let mut empty_roles = base.clone();
    empty_roles.roles.clear();
    expect_kind(
        solve(&empty_roles, &options(4)),
        SolveErrorKind::Unsupported,
    );
    // 重复角色。
    let mut duplicate = base.clone();
    duplicate.roles = vec!["FL".into(), "FL".into(), "SL".into()];
    expect_kind(solve(&duplicate, &options(4)), SolveErrorKind::Participants);
    // 非法角色 token。
    let mut bad_role = base.clone();
    bad_role.roles = vec!["fl".into(), "FR".into(), "SL".into()];
    expect_kind(solve(&bad_role, &options(4)), SolveErrorKind::Participants);
    // 非法采样率。
    let mut bad_fs = base.clone();
    bad_fs.sample_rate_hz = 44_000;
    expect_kind(solve(&bad_fs, &options(4)), SolveErrorKind::SampleRate);
    // 网格 1 点。
    let mut tiny_grid = base.clone();
    tiny_grid.grid_hz = vec![100.0];
    tiny_grid.positions.clear();
    expect_kind(solve(&tiny_grid, &options(4)), SolveErrorKind::Grid);
    // 非升序网格。
    let mut unsorted = base.clone();
    unsorted.grid_hz.swap(3, 4);
    expect_kind(solve(&unsorted, &options(4)), SolveErrorKind::Grid);
    // 超 Nyquist。
    let mut over = base.clone();
    over.grid_hz.push(30_000.0);
    expect_kind(solve(&over, &options(4)), SolveErrorKind::Grid);
    // 无 position。
    let mut no_positions = base.clone();
    no_positions.positions.clear();
    expect_kind(solve(&no_positions, &options(4)), SolveErrorKind::Position);
    // 权重非法。
    let mut bad_weight = base.clone();
    bad_weight.positions[0].weight = f64::NAN;
    expect_kind(solve(&bad_weight, &options(4)), SolveErrorKind::Range);
    let mut zero_weight = base.clone();
    zero_weight.positions[0].weight = 0.0;
    expect_kind(solve(&zero_weight, &options(4)), SolveErrorKind::Range);
    // H_eff 行数错误。
    let mut bad_shape = base.clone();
    bad_shape.positions[0].h_eff.pop();
    expect_kind(solve(&bad_shape, &options(4)), SolveErrorKind::Position);
    // 点数不匹配。
    let mut bad_points = base.clone();
    bad_points.positions[0].h_eff[0].pop();
    expect_kind(solve(&bad_points, &options(4)), SolveErrorKind::Grid);
    // 频点不匹配。
    let mut mismatched = base.clone();
    mismatched.positions[0].h_eff[0][0].freq_hz = 21.0;
    expect_kind(solve(&mismatched, &options(4)), SolveErrorKind::Grid);
    // 非有限值。
    let mut nonfinite = base.clone();
    nonfinite.positions[0].h_eff[0][0].re = f64::INFINITY;
    expect_kind(solve(&nonfinite, &options(4)), SolveErrorKind::Range);
    // Preserve 问题目标必须等于 H_eff（custom 构造后手工标记 preserve）。
    let mut preserve_mismatch = base.clone();
    preserve_mismatch.mode = SolveMode::Preserve;
    preserve_mismatch.target_position_variant = true;
    expect_kind(
        solve(&preserve_mismatch, &options(4)),
        SolveErrorKind::Target,
    );
    // 来源 hash 非法。
    let mut bad_provenance = base.clone();
    bad_provenance.provenance = Some(khaen_modal_design::solve::SolveProvenance {
        baseline_hash: "not-a-hash".to_string(),
        plant_hash: "0".repeat(64),
        design_plant_hash: "0".repeat(64),
        target_sha256: "0".repeat(64),
        common_d0_samples: 0,
        target_status: "predicted_pass",
    });
    expect_kind(
        solve(&bad_provenance, &options(4)),
        SolveErrorKind::Baseline,
    );
}

// ===========================================================================
// 6. 掩码 / ridge / 确定性 / 布局
// ===========================================================================

#[test]
fn t51_solve_path_mask_whitelist_zero_offdiag() {
    let grid = grid();
    let participants = ModalParticipants::p1_4_1_2();
    let baseline = make_baseline(&grid, participants.clone());
    let plant = make_plant(
        &baseline,
        &participants,
        &grid,
        gain_positions(&participants, &grid, 5),
    );
    let target = build_reduced_target(&baseline, &plant);
    let mut solver_options = options(8);
    let mut mask = vec![false; 25];
    for dense in 0..5 {
        mask[dense * 5 + dense] = true;
    }
    solver_options.path_mask = Some(mask);
    let outcome = solve_from_target_plant(&target, &plant, &solver_options).unwrap();
    assert_eq!(outcome.report.unknowns_total, 5 * 8);
    assert_eq!(outcome.report.unknowns_max_per_input, 8);
    assert!(outcome
        .report
        .columns
        .iter()
        .all(|column| column.free_paths == 1 && column.unknowns == 8));
    let solution = outcome.solution.expect("diagonal whitelist solve");
    for out_dense in 0..5usize {
        for in_dense in 0..5usize {
            if out_dense == in_dense {
                continue;
            }
            for tap in 0..8 {
                assert_eq!(
                    solution.coeff(out_dense, in_dense, tap).unwrap(),
                    0.0,
                    "offdiag path must be structurally zero under mask"
                );
            }
        }
    }
    // 全部禁用不是一次求解。
    let mut empty_mask = options(8);
    empty_mask.path_mask = Some(vec![false; 25]);
    expect_kind(
        solve_from_target_plant(&target, &plant, &empty_mask),
        SolveErrorKind::Options,
    );
}

#[test]
fn t51_solve_ridge_biases_and_reports() {
    let problem = f1_problem();
    let plain = solve(&problem, &options(4)).unwrap();
    let plain_solution = plain.solution.unwrap();
    let plain_norm: f64 = plain_solution
        .coeffs_f64
        .iter()
        .map(|value| value * value)
        .sum();
    let mut regularized = options(4);
    regularized.ridge = 0.1;
    let outcome = solve(&problem, &regularized).unwrap();
    assert!(outcome
        .report
        .warnings
        .iter()
        .any(|warning| warning.code == SolveWarning::RIDGE_USED));
    assert_eq!(outcome.report.status, SolveStatus::Converged);
    let solution = outcome.solution.unwrap();
    let norm: f64 = solution.coeffs_f64.iter().map(|value| value * value).sum();
    assert!(
        norm <= plain_norm + 1.0e-18,
        "ridge must shrink the min-norm correction: {norm} vs {plain_norm}"
    );
    assert!(
        outcome.report.predicted_fit_band_residual_max_db
            >= plain.report.predicted_fit_band_residual_max_db - 1.0e-9
    );
}

#[test]
fn t51_solve_deterministic_hash_and_options() {
    let problem = f1_problem();
    let first = solve(&problem, &options(4)).unwrap();
    let second = solve(&problem, &options(4)).unwrap();
    assert_eq!(first.report.status, second.report.status);
    assert_eq!(
        first.solution.as_ref().unwrap().hash,
        second.solution.as_ref().unwrap().hash
    );
    assert_eq!(
        first.solution.as_ref().unwrap().coeffs_sha256,
        second.solution.as_ref().unwrap().coeffs_sha256
    );
    let mut different = options(5);
    different.delay_samples = 0;
    let third = solve(&problem, &different).unwrap();
    assert_ne!(
        first.solution.unwrap().hash,
        third.solution.unwrap().hash,
        "different taps must change the solution hash"
    );
    let mut modified = problem.clone();
    modified.positions[0].weight = 0.5;
    assert_ne!(problem.hash(), modified.hash());
}

#[test]
fn t51_solve_coeff_layout_and_f32() {
    // 宽带良态夹具（1-8 kHz，2 taps）：频域基条件数小，最小范数解即稀疏已知解，
    // 可独立核对 kernel 布局 g[out][in][tap] 与 f32 降位（而不是只核对频响）。
    let grid = octave_grid(1_000.0, 8_000.0);
    let matrix = [[1.5f64, 0.4], [0.3, 1.2]];
    let delays = [2.0f64, 7.0];
    let problem = custom_problem(
        2,
        2,
        &grid,
        &move |position, output_dense, freq| {
            let rotation = delay_rotation(freq, delays[position]);
            complex_mul(rotation, (matrix[position][output_dense], 0.0))
        },
        &move |position, input_dense, freq| {
            if position == input_dense {
                delay_rotation(freq, delays[position])
            } else {
                (0.0, 0.0)
            }
        },
        SolveMode::Custom,
        false,
        0,
    );
    let mut solver_options = options(2);
    solver_options.fit_band_hz = (1_000.0, 8_000.0);
    let outcome = solve(&problem, &solver_options).unwrap();
    assert_eq!(outcome.report.status, SolveStatus::Converged);
    let solution = outcome.solution.unwrap();
    assert_eq!(solution.coeffs_f64.len(), 2 * 2 * 2);
    assert_eq!(solution.coeffs_f32.len(), 2 * 2 * 2);
    let flat: Vec<f64> = matrix.iter().flat_map(|row| row.iter().copied()).collect();
    let inverse = mat_inverse(&flat, 2);
    for out_dense in 0..2usize {
        for in_dense in 0..2usize {
            for tap in 0..2usize {
                let f64_value = solution.coeff(out_dense, in_dense, tap).unwrap();
                let index = (out_dense * 2 + in_dense) * 2 + tap;
                assert_eq!(solution.coeffs_f64[index], f64_value);
                let f32_value = solution.coeffs_f32[index] as f64;
                assert_close(f32_value, f64_value, 1.0e-6, "f32 conversion");
                let expected = if tap == 0 {
                    inverse[out_dense * 2 + in_dense]
                        - if out_dense == in_dense { 1.0 } else { 0.0 }
                } else {
                    0.0
                };
                assert_close(f64_value, expected, 1.0e-5, "kernel layout g[out][in][tap]");
            }
        }
    }
    // 频响独立核对：完整 G=A^{-1}（常数），确认布局与响应一致。
    for out_dense in 0..2usize {
        for in_dense in 0..2usize {
            let expected = inverse[out_dense * 2 + in_dense];
            for point in solution
                .response(out_dense as u16, in_dense as u16)
                .unwrap()
            {
                assert_close(point.re, expected, 1.0e-5, "layout response re");
                assert_close(point.im, 0.0, 1.0e-5, "layout response im");
            }
        }
    }
    assert!(solution.coeff(2, 0, 0).is_none());
    assert!(solution.coeff(0, 0, 2).is_none());
}

// ===========================================================================
// 7. 证据 JSON / 静态守卫 / 错误码
// ===========================================================================

#[test]
fn t51_solve_evidence_json_honest() {
    let grid = grid();
    let participants = ModalParticipants::p1_4_1_2();
    let baseline = make_baseline(&grid, participants.clone());
    let plant = make_plant(
        &baseline,
        &participants,
        &grid,
        flat_positions(&participants, &grid, 2, -3.0),
    );
    let target = build_preserve_target(&baseline, &plant);
    let outcome = solve_from_target_plant(&target, &plant, &options(8)).unwrap();
    let json = outcome.to_evidence_json();
    assert!(json.contains("\"schema\":\"khaen-modal-solve/v1\""));
    assert!(json.contains("\"applied\":false"));
    assert!(json.contains("\"post_apply_measurement\":\"not_requested\""));
    assert!(json.contains("\"engine_compatibility\":\"not_validated\""));
    assert!(json.contains("\"runtime_validation\":\"not_run\""));
    assert!(json.contains("\"holdout_used_for_tuning\":false"));
    assert!(json.contains("\"preserve_expected_identity\":true"));
    assert!(json.contains("\"baseline_hash\":"));
    assert!(json.contains("\"target_sha256\":"));
    assert!(json.contains("\"solution\":{"));
    assert!(json.contains("no guarantee that all standing waves are eliminated"));
    assert!(json.contains("minimum-norm solution on rank-deficient systems is non-unique"));
    let open = json.matches('{').count();
    let close = json.matches('}').count();
    assert_eq!(open, close, "evidence JSON must be brace-balanced");
    assert_eq!(
        json,
        outcome.to_evidence_json(),
        "evidence JSON must be deterministic"
    );
    assert_eq!(SOLVE_SCHEMA, "khaen-modal-solve/v1");
    assert!(SOLVE_CLAIM.contains("no guarantee"));
    println!("T51_EVIDENCE_JSON {json}");
}

#[test]
fn t51_solve_static_guard_no_io_and_claim() {
    let source = khaen_modal_design::solve::solve_source_text_for_guards();
    for forbidden in [
        "std::fs",
        "std::process",
        "std::net",
        "std::thread",
        "std::env",
        "std::io",
        "serde",
    ] {
        assert!(
            !source.contains(forbidden),
            "solve.rs must not contain {forbidden}"
        );
    }
    assert!(!source.contains("unsafe"));
    for required in [
        "pub fn solve",
        "from_target_plant",
        "SolveMode::Preserve",
        "SolveStatus::ResourceLimited",
        "SolveStatus::Underdetermined",
        "deltaD",
        "causal",
        "no guarantee",
        "不保证",
        "hash",
        "include_str!",
    ] {
        assert!(
            source.contains(required),
            "solve.rs must document/implement {required}"
        );
    }
    // 只允许“不保证消除全部驻波”这类否定表述，不允许任何承诺式表述。
    assert!(source.contains("不保证消除全部驻波"));
    assert!(source.contains("不承诺消除全部驻波"));
    // 否定式“不保证/不承诺消除全部驻波”允许；承诺式表述必须是 0 条。
    assert_eq!(
        source.matches("保证消除全部驻波").count(),
        source.matches("不保证消除全部驻波").count(),
        "solve.rs may only mention standing-wave elimination in negated form"
    );
    assert_eq!(
        source.matches("承诺消除全部驻波").count(),
        source.matches("不承诺消除全部驻波").count(),
        "solve.rs may only mention standing-wave elimination in negated form"
    );
    assert!(!source.contains("消除所有驻波"));
}

#[test]
fn t51_solve_error_codes_stable() {
    let codes: Vec<&'static str> = SolveErrorKind::ALL.iter().map(|kind| kind.code()).collect();
    assert_eq!(codes.len(), 11);
    let unique: std::collections::BTreeSet<&str> = codes.iter().copied().collect();
    assert_eq!(unique.len(), codes.len(), "error codes must be unique");
    for code in &codes {
        assert!(code.starts_with("E_SOLVE_"));
    }
    let error = SolveError::new(SolveErrorKind::Delay, "solve.options.delay_samples", "demo");
    assert_eq!(error.kind(), SolveErrorKind::Delay);
    assert_eq!(error.field(), "solve.options.delay_samples");
    assert_eq!(error.detail(), "demo");
    assert!(error.to_string().contains("E_SOLVE_DELAY_MISMATCH"));
    assert!(SolveStatus::Converged.is_usable());
    assert!(SolveStatus::PreserveBaseline.is_usable());
    assert!(!SolveStatus::NonConverged.is_usable());
    assert!(!SolveStatus::Underdetermined.is_usable());
    assert!(!SolveStatus::ResourceLimited.is_usable());
    assert_eq!(SolveStatus::NonConverged.as_str(), "non_converged");
    assert_eq!(SolveMode::Reduced.as_str(), "reduced");
}

// ===========================================================================
// 8. 延迟对齐 / 零 RHS / stereo strict
// ===========================================================================

#[test]
fn t51_solve_adapter_reduced_target_delay_matches_baseline_delay() {
    // 宽带夹具（2-8 kHz、8 taps）：FIR 延迟基条件数小，可独立核对"声明 target delay 由
    // G 基线 delta_D 承担"，同时避免窄带多 tap 的近似稀疏问题。
    let grid = octave_grid(2_000.0, 8_000.0);
    let participants = ModalParticipants::p1_4_1_2();
    let baseline = make_baseline(&grid, participants.clone());
    let plant = make_plant(
        &baseline,
        &participants,
        &grid,
        gain_positions(&participants, &grid, 5),
    );
    let target = build_reduced_target_with_delay(&baseline, &plant, 4);
    let mut solver_options = options(8);
    solver_options.delay_samples = 4;
    solver_options.fit_band_hz = (2_000.0, 8_000.0);
    solver_options.rel_tolerance = 1.0e-12;
    solver_options.max_iterations = 2_000;
    let outcome = solve_from_target_plant(&target, &plant, &solver_options).unwrap();
    assert_eq!(outcome.report.status, SolveStatus::Converged);
    assert_eq!(outcome.report.delay_samples, 4);
    let solution = outcome.solution.expect("target-delay aligned solution");
    let validation = validate_solution(&target, &plant, &solution.modal).unwrap();
    assert!(
        validation.passed,
        "delayed target must validate: {validation:?}"
    );
    // 闭式对照：G(f) = target_i * H^{-1}1 * z^-4（声明 target delay 与 G 基线 D 对齐）。
    let h_flat: Vec<f64> = GAINS_5.iter().flat_map(|row| row.iter().copied()).collect();
    let h_inverse = mat_inverse(&h_flat, 5);
    let mut h_inv_ones = vec![0.0f64; 5];
    for row in 0..5 {
        for column in 0..5 {
            h_inv_ones[row] += h_inverse[row * 5 + column];
        }
    }
    let target_levels: Vec<f64> = (0..5)
        .map(|input_dense| {
            (0..5)
                .map(|position| GAINS_5[position][input_dense].powi(2) / 5.0)
                .sum::<f64>()
                .sqrt()
        })
        .collect();
    let mut worst = 0.0f64;
    for out_dense in 0..5usize {
        for in_dense in 0..5usize {
            let level = target_levels[in_dense] * h_inv_ones[out_dense];
            let points = solution
                .response(out_dense as u16, in_dense as u16)
                .unwrap();
            for (index, point) in points.iter().enumerate() {
                let rotation = delay_rotation(plant.grid_hz()[index], 4.0);
                let expected = complex_mul(rotation, (level, 0.0));
                worst = worst.max(
                    (point.re - expected.0)
                        .abs()
                        .max((point.im - expected.1).abs()),
                );
            }
        }
    }
    assert!(
        worst < 1.0e-3,
        "declared target delay must be carried by G baseline delta_4, worst={worst}"
    );
    // 因果性：修正能量集中落在 t=D（收敛门以内其它 tap 只剩迭代残差）。
    let mut tap_d_energy = 0.0f64;
    for out_dense in 0..5usize {
        for in_dense in 0..5usize {
            tap_d_energy += solution.coeff(out_dense, in_dense, 4).unwrap().abs();
            for tap in 0..8usize {
                if tap == 4 {
                    continue;
                }
                assert_close(
                    solution.coeff(out_dense, in_dense, tap).unwrap(),
                    0.0,
                    1.0e-3,
                    "declared target delay must sit at t=D (within convergence tolerance)",
                );
            }
        }
    }
    assert!(tap_d_energy > 1.0, "t=D correction must be non-trivial");
    // D 与 target delay 不一致 → 拒收（禁止事后平移）。
    let mut mismatched = options(8);
    mismatched.delay_samples = 0;
    expect_kind(
        solve_from_target_plant(&target, &plant, &mismatched),
        SolveErrorKind::Delay,
    );
}

#[test]
fn t51_solve_zero_rhs_is_identity_and_still_reports() {
    let grid = vec![40.0, 80.0, 125.0, 200.0, 260.0];
    let m = 2usize;
    let gains = [[1.0, 0.3], [0.4, 1.2]];
    let problem = custom_problem(
        m,
        2,
        &grid,
        &move |position, output_dense, _freq| (gains[position][output_dense], 0.0),
        // target 恰为 G=I 的预测（RHS 全零）：不应发明修正。
        &move |position, input_dense, _freq| (gains[position][input_dense], 0.0),
        SolveMode::Custom,
        false,
        0,
    );
    let outcome = solve(&problem, &options(1)).unwrap();
    assert_eq!(outcome.report.status, SolveStatus::Converged);
    let solution = outcome.solution.expect("zero-rhs solution");
    assert!(solution.coeffs_f64.iter().all(|value| *value == 0.0));
    assert_eq!(outcome.report.columns[0].stop_reason, "zero_rhs");
    assert!(outcome.report.predicted_fit_band_residual_max_db < 1.0e-9);
    assert!(outcome.report.correction_coeff_max_db.is_none());
}

#[test]
fn t51_solve_stereo_strict_profile() {
    let grid = grid();
    let participants = ModalParticipants::stereo_strict();
    let baseline = make_baseline(&grid, participants.clone());
    let gains = [[1.0, 0.5], [0.4, 1.2]];
    let mut positions = Vec::new();
    for position in 0..2usize {
        let label = format!("P{:02}", position + 1);
        positions.push(raw_position(&label, &participants, |role_id| {
            let dense = participants
                .participants()
                .iter()
                .position(|participant| participant.role_id == role_id)
                .unwrap();
            gain_points(&grid, gains[position][dense])
        }));
    }
    let plant = make_plant(&baseline, &participants, &grid, positions);
    let spec = TargetSpec::stereo_strict(&baseline, reduced_decay(0.85)).unwrap();
    let target = build_stereo_strict_target(&baseline, &plant, &spec).unwrap();
    let outcome = solve_from_target_plant(&target, &plant, &options(8)).unwrap();
    assert_eq!(outcome.report.status, SolveStatus::Converged);
    assert_eq!(outcome.report.m, 2);
    let solution = outcome.solution.expect("stereo strict solution");
    assert_eq!(solution.modal.m(), 2);
    let validation = validate_stereo_strict_solution(&target, &plant, &solution.modal).unwrap();
    assert!(
        validation.passed,
        "stereo strict must validate: {validation:?}"
    );
    assert!(outcome.report.predicted_fit_band_residual_max_db < 1.0e-3);
}

#[test]
fn t51_solve_adapter_requires_matching_baseline() {
    let grid = grid();
    let participants = ModalParticipants::p1_4_1_2();
    let baseline_a = make_baseline(&grid, participants.clone());
    let plant_a = make_plant(
        &baseline_a,
        &participants,
        &grid,
        gain_positions(&participants, &grid, 5),
    );
    let target_a = build_reduced_target(&baseline_a, &plant_a);
    // 另一条冻结 P 基线（不同 fit_inputs hash → 不同 baseline hash）的 plant。
    let mut input = PostBaselineInput {
        baseline_id: "t51-baseline-other".to_string(),
        sample_rate_hz: FS,
        participants: participants.clone(),
        tone: ToneBinding::flat(),
        curve: CurveBinding::baseline_only("flat_baseline").unwrap(),
        tone_applications: ToneApplicationProof::baseline_only(),
        fit_inputs_sha256: sha256_hex(b"t51-fit-inputs-other"),
        common_d0: CommonD0::new(D0_SAMPLES).unwrap(),
        grid_hz: grid.clone(),
        roles: Vec::new(),
        created_unix: None,
    };
    for participant in participants.participants() {
        input.roles.push(post_role(
            &participant.role_id,
            participant.out_index,
            &grid,
            0.0,
            0,
            1,
        ));
    }
    input.roles.push(post_role("HFL", 4, &grid, 0.0, 0, 1));
    input.roles.push(post_role("HFR", 5, &grid, 0.0, 0, 1));
    let baseline_b = FittedPostBaseline::new(input).unwrap().freeze().unwrap();
    let plant_b = make_plant(
        &baseline_b,
        &participants,
        &grid,
        flat_positions(&participants, &grid, 2, 0.0),
    );
    expect_kind(
        solve_from_target_plant(&target_a, &plant_b, &options(8)),
        SolveErrorKind::Baseline,
    );
    // T50 自身也拒绝跨基线（这里用 T50 builder 复核，防 solver 静默复用）。
    let spec_b = TargetSpec::for_baseline(&baseline_b, reduced_decay(0.85)).unwrap();
    assert!(build_target_matrix(&baseline_a, &plant_b, &spec_b).is_err());
}

// ===========================================================================
// 9. 已知常量/接口形状
// ===========================================================================

#[test]
fn t51_solve_constants_and_shapes() {
    assert_eq!(MAX_SOLVE_TAPS, 4096);
    assert_eq!(SolveProblem::SCHEMA, "khaen-modal-solve/v1");
    let problem = f1_problem();
    assert_eq!(problem.m(), 3);
    assert_eq!(problem.grid_points(), problem.grid_hz.len());
    let options = SolveOptions::default();
    assert!(options.taps <= MAX_SOLVE_TAPS);
    assert_eq!(options.ridge, 0.0);
    let fields = problem.canonical();
    assert!(fields.contains("khaen-modal-solve-problem/v1"));
    assert!(fields.contains("provenance=synthetic"));
    assert!(problem.hash().len() == 64);
    let mut options_a = options.clone();
    options_a.taps = 7;
    let mut options_b = options.clone();
    options_b.taps = 8;
    assert_ne!(options_a.canonical(&problem), options_b.canonical(&problem));
}
