//! T53 private integration tests — 模态 holdout 预测校验 + T20 质量窗（crate khaen-modal-design）。
//!
//! 运行（在 rt-stack/ 下）：cargo test -p khaen-modal-design holdout
//! 文件位置：crates/modal-design/tests/T53/main.rs（leaf 私有测试；不写 public_registration.rs）。
//!
//! 覆盖（WBS T53 DoD）：
//! - 设计内与未见 holdout 分开：只评估 independent holdout plant 的 validation 测点；
//!   tuning labels 含 holdout → F-07；seal 不可证 → I-04；holdout hash 不符 → F-07；
//! - 独立 plant 预测误差：63-250 Hz 改善带 E_off/E_on/I_raw/I_use、最差点/每 input/每测点最小值；
//! - 近零基线 N/A（E_off ≤ 4·E_noise）与 N/A 比例规则；
//! - σ 定义：重复对 σ²_c、E_noise 经 G 传播、delta-method σ_I、重复次数/传播单调性；
//! - T20 有效窗 V1-V4：Schroeder EDC、[-25,-5] dB 回归、R²/残差/噪声底/时长/物理范围；
//! - T20 门：median 下降 ≥15% 且无测点恶化 >5%（F-02）；IR 不足 → I-06；
//! - 判定优先级 FAIL > INCOMPLETE > NOT_RUN；PREDICTED_PASS 恒带诚实字段；
//! - 输入契约错误（绑错 plant/holdout labels/IR band/重复角色等）拒绝；
//! - 确定性 hash / 证据 JSON 语法 / 静态守卫（无 I/O/线程/unsafe/serde）。
//!
//! 本文件只构造内存夹具；不采音、不碰板端、不打开 ALSA、不写文件。
// 迁移说明：本 crate 从 legacy/pre-rewrite 原样迁入，算法与数值路径不改写。下列 clippy 建议
// 属风格/数值改写建议（部分会改变 NaN/边界语义）；迁移期统一在 crate 级允许，避免为过 lint
// 改动算术路径——行为回归由 tests/ 的冻结数值测试保护。理由与 mimo-design 迁移先例一致。
#![allow(clippy::approx_constant, clippy::field_reassign_with_default)]

use khaen_modal_design::ingest::sha256_hex;
use khaen_modal_design::solve::{solve_from_target_plant, SolveOptions};
use khaen_modal_design::target::{
    build_target_matrix, BandDecay, CurveBinding, DecayTarget, EffectivePlant, FittedPostBaseline,
    FrozenPostBaseline, ModalMatrix, ModalParticipants, PostBaselineInput, PostRoleFit, RawPlant,
    RawPosition, RawResponse, TargetMatrix, TargetSpec, ToneApplicationProof, ToneBinding,
    P1_LAYOUT_4_1_2,
};
use khaen_modal_design::timebase::{CommonD0, ComplexPoint, PolaritySource, ResolvedPolarity};
use khaen_modal_design::validate::{
    band_filter_ir, estimate_t20_window, expected_1_24_oct_points, holdout_labels_sha256,
    min_improvement_band_points, validate_holdout, BandIr, GateStatus, HoldoutRequest,
    HoldoutVerdict, IrOrigin, IrState, NoiseRepeats, PointStatus, RoleRepeats, TuningDeclaration,
    DECLARED_GATES, IMPROVEMENT_GATE_DB, MIN_HOLDOUT_POSITIONS, T20_REDUCTION_GATE_RATIO,
    VALIDATE_CLAIM, VALIDATE_REPORT_SCHEMA,
};

// ===========================================================================
// 夹具常量
// ===========================================================================

const FS: u32 = 48_000;
const D0_SAMPLES: i64 = 120;
const TUNING_INPUTS_SHA256: &str =
    "1111111111111111111111111111111111111111111111111111111111111111";
const SECONDS_IR: f64 = 1.5;

/// 6 个 design position 的 H 行（每行是 5 个输出角色的转移幅度；行和都 = 4.0，
/// 使 ones 向量落在 H 的列空间，目标矩阵存在精确可达 G）。
const DESIGN_ROWS: [[f64; 5]; 6] = [
    [1.0, 0.9, 0.8, 0.7, 0.6],
    [0.6, 0.7, 0.8, 0.9, 1.0],
    [0.8, 1.0, 0.6, 1.0, 0.6],
    [0.7, 0.7, 0.9, 0.9, 0.8],
    [0.9, 0.8, 0.7, 0.8, 0.8],
    [1.0, 0.8, 0.8, 0.8, 0.6],
];

/// 3 个 holdout position（与部分 design 行同值；合成夹具，非实测）。
const HOLDOUT_ROWS: [[f64; 5]; 3] = [
    [1.0, 0.9, 0.8, 0.7, 0.6],
    [0.6, 0.7, 0.8, 0.9, 1.0],
    [0.8, 1.0, 0.6, 1.0, 0.6],
];

const HOLDOUT_LABELS: [&str; 3] = ["H01", "H02", "H03"];

// ===========================================================================
// 公共夹具
// ===========================================================================

fn octave_grid(lo: f64, hi: f64, divisions: i32) -> Vec<f64> {
    let mut freqs = Vec::new();
    let mut index = 0i32;
    loop {
        let freq = lo * 2f64.powf(index as f64 / divisions as f64);
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

fn bass_grid() -> Vec<f64> {
    octave_grid(20.0, 300.0, 24)
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

fn make_baseline(
    grid: &[f64],
    participants: ModalParticipants,
    baseline_id: &str,
    fit_inputs: &[u8],
) -> FrozenPostBaseline {
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
        baseline_id: baseline_id.to_string(),
        sample_rate_hz: FS,
        participants,
        tone,
        curve,
        tone_applications: ToneApplicationProof::baseline_only(),
        fit_inputs_sha256: sha256_hex(fit_inputs),
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

fn flat_points(grid: &[f64], magnitude: f64) -> Vec<ComplexPoint> {
    grid.iter()
        .map(|freq| ComplexPoint {
            freq_hz: *freq,
            re: magnitude,
            im: 0.0,
        })
        .collect()
}

fn raw_position_row(
    label: &str,
    role: khaen_modal_design::ingest::PositionRole,
    participants: &ModalParticipants,
    grid: &[f64],
    row: &[f64; 5],
) -> RawPosition {
    let mut responses = Vec::new();
    for participant in participants.participants() {
        responses.push(normalized_response(
            &participant.role_id,
            participant.out_index,
            flat_points(grid, row[participant.dense_index as usize]),
        ));
    }
    RawPosition::new(label, role, 1.0, responses).unwrap()
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

fn make_design_plant(
    baseline: &FrozenPostBaseline,
    participants: &ModalParticipants,
    grid: &[f64],
) -> EffectivePlant {
    let positions = DESIGN_ROWS
        .iter()
        .enumerate()
        .map(|(index, row)| {
            raw_position_row(
                &format!("P{:02}", index + 1),
                khaen_modal_design::ingest::PositionRole::Design,
                participants,
                grid,
                row,
            )
        })
        .collect();
    make_plant(baseline, participants, grid, positions)
}

fn make_holdout_plant(
    baseline: &FrozenPostBaseline,
    participants: &ModalParticipants,
    grid: &[f64],
    rows: &[[f64; 5]],
) -> EffectivePlant {
    let mut positions = vec![raw_position_row(
        "PH00",
        khaen_modal_design::ingest::PositionRole::Design,
        participants,
        grid,
        &[1.0, 0.9, 0.8, 0.7, 0.6],
    )];
    for (index, row) in rows.iter().enumerate() {
        positions.push(raw_position_row(
            &format!("H{:02}", index + 1),
            khaen_modal_design::ingest::PositionRole::Validation,
            participants,
            grid,
            row,
        ));
    }
    make_plant(baseline, participants, grid, positions)
}

fn measured_decay_bands() -> Vec<BandDecay> {
    vec![
        BandDecay {
            band_hz: 63.0,
            baseline_t60_s: 0.40,
        },
        BandDecay {
            band_hz: 125.0,
            baseline_t60_s: 0.35,
        },
    ]
}

fn reduced_decay(ratio: f64) -> DecayTarget {
    DecayTarget::reduced(ratio, measured_decay_bands()).unwrap()
}

fn preserve_decay() -> DecayTarget {
    DecayTarget::preserve(measured_decay_bands()).unwrap()
}

fn build_target(
    baseline: &FrozenPostBaseline,
    plant: &EffectivePlant,
    decay: DecayTarget,
) -> TargetMatrix {
    let spec = TargetSpec::for_baseline(baseline, decay).unwrap();
    build_target_matrix(baseline, plant, &spec).unwrap()
}

fn make_declaration(
    design_plant: &EffectivePlant,
    holdout_labels: &[&str],
    sealed_before_design: bool,
) -> TuningDeclaration {
    let design_labels: Vec<String> = design_plant
        .design_positions()
        .map(|position| position.label.clone())
        .collect();
    TuningDeclaration::new(
        design_labels.clone(),
        holdout_labels
            .iter()
            .map(|label| (*label).to_string())
            .collect(),
    )
    .with_tuning(design_labels, TUNING_INPUTS_SHA256)
    .with_seal(sealed_before_design)
}

fn make_repeats(
    participants: &ModalParticipants,
    grid: &[f64],
    labels: &[&str],
    delta: f64,
) -> Vec<NoiseRepeats> {
    labels
        .iter()
        .map(|label| NoiseRepeats {
            position_label: (*label).to_string(),
            roles: participants
                .participants()
                .iter()
                .map(|participant| RoleRepeats {
                    role_id: participant.role_id.clone(),
                    series: vec![flat_points(grid, 1.0), flat_points(grid, 1.0 + delta)],
                })
                .collect(),
        })
        .collect()
}

fn exponential_decay(t60_s: f64, seconds: f64) -> Vec<f64> {
    let sample_count = (FS as f64 * seconds).round() as usize;
    let alpha = std::f64::consts::LN_10 * 3.0 / t60_s;
    (0..sample_count)
        .map(|index| (-alpha * index as f64 / FS as f64).exp())
        .collect()
}

fn make_band_ir(labels: &[&str], off_t60_s: f64, on_t60_s: f64, seconds: f64) -> Vec<BandIr> {
    let off = exponential_decay(off_t60_s, seconds);
    let on = exponential_decay(on_t60_s, seconds);
    let mut irs = Vec::new();
    for label in labels {
        for band_hz in [63.0, 125.0] {
            for (state, samples) in [(IrState::Off, &off), (IrState::On, &on)] {
                irs.push(BandIr {
                    position_label: (*label).to_string(),
                    band_hz,
                    state,
                    samples: samples.clone(),
                    snr_db: 40.0,
                    clipped: false,
                    xrun: false,
                    deadline_miss: false,
                    lost_lock: false,
                    dropped_blocks: 0,
                });
            }
        }
    }
    irs
}

// ===========================================================================
// 最小 JSON 语法校验器（证据 JSON 必须可被独立 checker 解析；不引入第三方依赖）
// ===========================================================================

struct JsonSyntax<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> JsonSyntax<'a> {
    fn validate(text: &'a str) -> Result<(), String> {
        let mut parser = JsonSyntax {
            bytes: text.as_bytes(),
            position: 0,
        };
        parser.skip_whitespace();
        parser.value()?;
        parser.skip_whitespace();
        if parser.position != parser.bytes.len() {
            return Err(format!("trailing characters at byte {}", parser.position));
        }
        Ok(())
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn skip_whitespace(&mut self) {
        while let Some(byte) = self.peek() {
            if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
                self.position += 1;
            } else {
                break;
            }
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), String> {
        if self.peek() == Some(byte) {
            self.position += 1;
            Ok(())
        } else {
            Err(format!(
                "expected {:?} at byte {}",
                byte as char, self.position
            ))
        }
    }

    fn value(&mut self) -> Result<(), String> {
        match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => self.string(),
            Some(b't') => self.literal(b"true"),
            Some(b'f') => self.literal(b"false"),
            Some(b'n') => self.literal(b"null"),
            Some(byte) if byte == b'-' || byte.is_ascii_digit() => self.number(),
            other => Err(format!(
                "unexpected value start {other:?} at byte {}",
                self.position
            )),
        }
    }

    fn literal(&mut self, expected: &[u8]) -> Result<(), String> {
        if self.bytes[self.position..].starts_with(expected) {
            self.position += expected.len();
            Ok(())
        } else {
            Err(format!("bad literal at byte {}", self.position))
        }
    }

    fn object(&mut self) -> Result<(), String> {
        self.expect(b'{')?;
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.position += 1;
            return Ok(());
        }
        loop {
            self.skip_whitespace();
            self.string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_whitespace();
            self.value()?;
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.position += 1,
                Some(b'}') => {
                    self.position += 1;
                    return Ok(());
                }
                other => {
                    return Err(format!(
                        "expected ',' or '}}' in object, got {other:?} at byte {}",
                        self.position
                    ))
                }
            }
        }
    }

    fn array(&mut self) -> Result<(), String> {
        self.expect(b'[')?;
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.position += 1;
            return Ok(());
        }
        loop {
            self.skip_whitespace();
            self.value()?;
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.position += 1,
                Some(b']') => {
                    self.position += 1;
                    return Ok(());
                }
                other => {
                    return Err(format!(
                        "expected ',' or ']' in array, got {other:?} at byte {}",
                        self.position
                    ))
                }
            }
        }
    }

    fn string(&mut self) -> Result<(), String> {
        self.expect(b'"')?;
        'outer: loop {
            match self.peek() {
                None => return Err("unterminated string".to_string()),
                Some(b'"') => {
                    self.position += 1;
                    break 'outer;
                }
                Some(b'\\') => {
                    self.position += 1;
                    match self.peek() {
                        Some(b'"') | Some(b'\\') | Some(b'/') | Some(b'b') | Some(b'f')
                        | Some(b'n') | Some(b'r') | Some(b't') => self.position += 1,
                        Some(b'u') => {
                            self.position += 1;
                            for _ in 0..4 {
                                match self.peek() {
                                    Some(byte) if byte.is_ascii_hexdigit() => self.position += 1,
                                    _ => return Err("bad unicode escape".to_string()),
                                }
                            }
                        }
                        _ => return Err("bad escape".to_string()),
                    }
                }
                Some(byte) if byte < 0x20 => return Err("control character in string".to_string()),
                Some(_) => self.position += 1,
            }
        }
        Ok(())
    }

    fn number(&mut self) -> Result<(), String> {
        if self.peek() == Some(b'-') {
            self.position += 1;
        }
        let mut digits = 0usize;
        while let Some(byte) = self.peek() {
            if byte.is_ascii_digit() {
                self.position += 1;
                digits += 1;
            } else {
                break;
            }
        }
        if digits == 0 {
            return Err(format!("number without digits at byte {}", self.position));
        }
        if self.peek() == Some(b'.') {
            self.position += 1;
            let mut fraction = 0usize;
            while let Some(byte) = self.peek() {
                if byte.is_ascii_digit() {
                    self.position += 1;
                    fraction += 1;
                } else {
                    break;
                }
            }
            if fraction == 0 {
                return Err("fraction without digits".to_string());
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            self.position += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.position += 1;
            }
            let mut exponent = 0usize;
            while let Some(byte) = self.peek() {
                if byte.is_ascii_digit() {
                    self.position += 1;
                    exponent += 1;
                } else {
                    break;
                }
            }
            if exponent == 0 {
                return Err("exponent without digits".to_string());
            }
        }
        Ok(())
    }
}

// ===========================================================================
// 夹具自检（smoke）：先证明 T50/T51 夹具可用
// ===========================================================================

#[test]
fn t53_holdout_fixture_smoke() {
    let grid = bass_grid();
    let participants = ModalParticipants::p1_4_1_2();
    let baseline = make_baseline(&grid, participants.clone(), "t53-baseline", b"t53-fixture");
    let design_plant = make_design_plant(&baseline, &participants, &grid);
    assert_eq!(design_plant.design_positions().count(), 6);
    let target = build_target(&baseline, &design_plant, reduced_decay(0.85));
    assert!(!target.is_position_variant());
    assert_eq!(target.participants().m(), 5);
    // 网格覆盖 63-250 Hz 的 1/24 oct 点数必须 >= 39（T6 §3.3）。
    let band_points = target
        .grid_hz()
        .iter()
        .filter(|freq| **freq >= 63.0 && **freq <= 250.0)
        .count();
    assert_eq!(expected_1_24_oct_points(63.0, 250.0), 48);
    assert!(band_points >= min_improvement_band_points());
    // T51 精确可达夹具：taps=1、D=0，CGLS 应收敛且设计残差很小。
    let mut options = SolveOptions::default();
    options.taps = 1;
    options.delay_samples = 0;
    options.fit_band_hz = (20.0, 300.0);
    options.max_iterations = 64;
    options.rel_tolerance = 1.0e-8;
    let outcome = solve_from_target_plant(&target, &design_plant, &options).unwrap();
    let solution = outcome.solution.expect("solve must return a solution");
    assert_eq!(solution.modal.m(), 5);
    let holdout_plant = make_holdout_plant(&baseline, &participants, &grid, &HOLDOUT_ROWS);
    assert_eq!(holdout_plant.validation_labels(), HOLDOUT_LABELS.to_vec());
    // 独立 holdout 的预测误差应远小于基线（合成夹具属性）。
    assert!(solution.modal.m() > 0 && holdout_plant.validation_positions().count() == 3);
}

// ===========================================================================
// 完整夹具（含 T51 解）
// ===========================================================================

struct Fixture {
    grid: Vec<f64>,
    participants: ModalParticipants,
    baseline: FrozenPostBaseline,
    design_plant: EffectivePlant,
    target: TargetMatrix,
    solution: ModalMatrix,
    holdout_plant: EffectivePlant,
}

fn solve_exact_g(target: &TargetMatrix, design_plant: &EffectivePlant) -> ModalMatrix {
    let mut options = SolveOptions::default();
    options.taps = 1;
    options.delay_samples = 0;
    options.fit_band_hz = (20.0, 300.0);
    options.max_iterations = 64;
    options.rel_tolerance = 1.0e-8;
    let outcome = solve_from_target_plant(target, design_plant, &options).unwrap();
    outcome
        .solution
        .expect("solve must return a solution")
        .modal
}

fn pass_fixture() -> Fixture {
    let grid = bass_grid();
    let participants = ModalParticipants::p1_4_1_2();
    let baseline = make_baseline(&grid, participants.clone(), "t53-baseline", b"t53-fixture");
    let design_plant = make_design_plant(&baseline, &participants, &grid);
    let target = build_target(&baseline, &design_plant, reduced_decay(0.85));
    let solution = solve_exact_g(&target, &design_plant);
    let holdout_plant = make_holdout_plant(&baseline, &participants, &grid, &HOLDOUT_ROWS);
    Fixture {
        grid,
        participants,
        baseline,
        design_plant,
        target,
        solution,
        holdout_plant,
    }
}

fn full_request(
    design_plant: &EffectivePlant,
    repeats: Vec<NoiseRepeats>,
    irs: Vec<BandIr>,
) -> HoldoutRequest {
    HoldoutRequest::new(make_declaration(design_plant, &HOLDOUT_LABELS, true))
        .with_repeats(repeats)
        .with_ir(irs, IrOrigin::PredictedModel)
}

fn point_by_role<'a>(
    report: &'a khaen_modal_design::validate::HoldoutReport,
    position: &str,
    role: &str,
) -> &'a khaen_modal_design::validate::PointPrediction {
    report
        .prediction
        .points
        .iter()
        .find(|point| point.position_label == position && point.input_role == role)
        .expect("point must exist")
}

// ===========================================================================
// 1. 预声明门固定
// ===========================================================================

#[test]
fn t53_holdout_gates_are_fixed_and_predeclared() {
    assert_eq!(DECLARED_GATES.improvement_gate_db, 3.0);
    assert_eq!(IMPROVEMENT_GATE_DB, 3.0);
    assert_eq!(DECLARED_GATES.t20_reduction_gate_ratio, 0.85);
    assert_eq!(T20_REDUCTION_GATE_RATIO, 0.85);
    assert_eq!(DECLARED_GATES.t20_position_worsen_ratio, 1.05);
    assert_eq!(DECLARED_GATES.near_zero_e_off_over_noise, 4.0);
    assert_eq!(DECLARED_GATES.min_holdout_positions, 3);
    assert_eq!(MIN_HOLDOUT_POSITIONS, 3);
    assert_eq!(DECLARED_GATES.improvement_band_hz, (63.0, 250.0));
    assert_eq!(DECLARED_GATES.improvement_grid_min_coverage, 0.8);
    assert_eq!(DECLARED_GATES.t20_noise_margin_db, 10.0);
    assert_eq!(DECLARED_GATES.t20_fit_window_db, (-5.0, -25.0));
    assert_eq!(DECLARED_GATES.t20_min_r2, 0.99);
    assert_eq!(DECLARED_GATES.t20_max_residual_rms_db, 1.0);
    assert_eq!(DECLARED_GATES.t20_range_s, (0.05, 2.0));
    assert_eq!(DECLARED_GATES.t20_min_snr_db, 25.0);
    assert!(DECLARED_GATES.authority.contains("mimo-t6"));
    // 门值只能来自冻结常量；HoldoutRequest 没有任何 gate override 字段。
    let request = HoldoutRequest::new(make_declaration(
        &pass_fixture().design_plant,
        &HOLDOUT_LABELS,
        true,
    ));
    assert!(request.repeats.is_empty());
    assert!(request.ir.is_empty());
}

// ===========================================================================
// 2. PREDICTED_PASS（完整证据）+ 诚实字段
// ===========================================================================

#[test]
fn t53_holdout_predicted_pass_with_independent_holdout() {
    let fixture = pass_fixture();
    let repeats = make_repeats(
        &fixture.participants,
        &fixture.grid,
        &HOLDOUT_LABELS,
        1.0e-3,
    );
    let irs = make_band_ir(&HOLDOUT_LABELS, 0.40, 0.32, SECONDS_IR);
    let request = full_request(&fixture.design_plant, repeats, irs);
    let report = validate_holdout(
        &fixture.target,
        &fixture.design_plant,
        &fixture.holdout_plant,
        &fixture.solution,
        &request,
    )
    .unwrap();
    assert_eq!(report.schema, VALIDATE_REPORT_SCHEMA);
    assert_eq!(report.verdict, HoldoutVerdict::PredictedPass);
    assert_eq!(report.verdict_code, "PREDICTED_PASS");
    // 诚实字段：predicted-only，永不冒充实测/可替代复测。
    assert!(!report.applied);
    assert!(!report.measured);
    assert_eq!(report.post_apply_measurement, "not_run");
    assert!(!report.holdout_used_for_tuning);
    assert!(!report.can_substitute_real_remeasurement);
    assert!(report.requires_post_apply_remeasurement);
    assert_eq!(report.claims, VALIDATE_CLAIM);
    assert!(report.warnings.contains(&"predicted_only_not_measured"));
    assert!(report
        .warnings
        .contains(&"not_a_substitute_for_post_apply_remeasurement"));
    // 隔离通过、设计绑定一致、holdout plant 独立。
    assert_eq!(report.isolation.gate, GateStatus::Pass);
    assert!(report.isolation.labels_hash_match);
    assert_eq!(
        report.identity.design_plant_hash,
        fixture.design_plant.hash()
    );
    assert_eq!(
        report.identity.design_plant_hash_observed,
        fixture.design_plant.hash()
    );
    assert_eq!(
        report.identity.holdout_plant_hash,
        fixture.holdout_plant.hash()
    );
    // 预测门：15 个候选点全部 valid；最差点 ≥3 dB；每 input 最小值 ≥3 dB。
    assert_eq!(report.prediction.gate, GateStatus::Pass);
    assert_eq!(report.prediction.candidate_points, 15);
    assert_eq!(report.prediction.valid_points, 15);
    assert_eq!(report.prediction.near_zero_points, 0);
    let worst = report.prediction.worst_i_use.as_ref().unwrap();
    assert!(worst.i_use_db >= IMPROVEMENT_GATE_DB);
    assert!(report.prediction.median_i_use_db.unwrap() >= IMPROVEMENT_GATE_DB);
    for (role, min) in &report.prediction.per_input_min_db {
        assert!(
            min.map(|value| value >= IMPROVEMENT_GATE_DB)
                .unwrap_or(false),
            "input {role} min {min:?} below gate"
        );
    }
    // σ 定义与 E_noise 传播。
    let sigma = &report.prediction.sigma;
    assert!(sigma.definition.contains("sigma_squared_complex_c"));
    assert!(sigma.formula_e_noise.contains("E_noise(p,i)"));
    assert!(sigma.formula_sigma_i.contains("delta-method"));
    assert!(sigma.repeat_pair_count >= 15);
    let first_point = point_by_role(&report, "H01", "FL");
    assert!(first_point.e_noise.unwrap() > 0.0);
    assert!(first_point.e_noise_off.unwrap() > 0.0);
    assert!(first_point.sigma_i_db.unwrap() >= 0.0);
    // T20 门：两个必需 band 都 Pass，且 T20 估计落在合成值附近。
    assert_eq!(report.t20.gate, GateStatus::Pass);
    assert_eq!(report.t20.bands.len(), 2);
    for band in &report.t20.bands {
        assert_eq!(band.gate, GateStatus::Pass);
        assert_eq!(band.valid_positions, 3);
        let off = band.median_off_s.unwrap();
        let on = band.median_on_s.unwrap();
        assert!((off - 0.40).abs() < 0.02, "off T20 {off}");
        assert!((on - 0.32).abs() < 0.02, "on T20 {on}");
        assert!(band.median_ratio.unwrap() <= T20_REDUCTION_GATE_RATIO);
        for position in &band.positions {
            assert!(position.valid);
            assert!(position.off.as_ref().unwrap().valid);
            assert!(position.on.as_ref().unwrap().valid);
            let t20_sigma = position.on.as_ref().unwrap().t20_sigma_s.unwrap();
            assert!(t20_sigma.is_finite() && t20_sigma >= 0.0);
        }
    }
    // design 侧预测摘要只报 design positions，不作 holdout 门。
    assert!(report
        .design_prediction
        .scope
        .contains("design_positions_only"));
    assert!(report.design_prediction.passed);
    assert!(report.design_prediction.predicted_residual_max_db.unwrap() < 3.0);
    // 证据 JSON 语法合法 + hash 自洽。
    JsonSyntax::validate(&report.to_evidence_json())
        .expect("evidence JSON must be syntactically valid");
    assert_eq!(report.content_hash(), report.report_hash);
    assert_eq!(report.report_hash.len(), 64);
}

// ===========================================================================
// 3. identity 负向对照（NC-01 精神）：I_use≈0 → F-01，不得 PASS
// ===========================================================================

#[test]
fn t53_holdout_identity_negative_control_fails() {
    let fixture = pass_fixture();
    let roles: Vec<String> = fixture
        .participants
        .participants()
        .iter()
        .map(|participant| participant.role_id.clone())
        .collect();
    let identity = ModalMatrix::identity(roles, fixture.grid.clone()).unwrap();
    let repeats = make_repeats(
        &fixture.participants,
        &fixture.grid,
        &HOLDOUT_LABELS,
        1.0e-3,
    );
    let irs = make_band_ir(&HOLDOUT_LABELS, 0.40, 0.32, SECONDS_IR);
    let request = full_request(&fixture.design_plant, repeats, irs);
    let report = validate_holdout(
        &fixture.target,
        &fixture.design_plant,
        &fixture.holdout_plant,
        &identity,
        &request,
    )
    .unwrap();
    assert_eq!(report.verdict, HoldoutVerdict::PredictedFail);
    assert_eq!(report.verdict_code, "F-01");
    assert_eq!(report.prediction.gate, GateStatus::Fail);
    let worst = report.prediction.worst_i_use.as_ref().unwrap();
    assert!(worst.i_use_db < IMPROVEMENT_GATE_DB);
    // identity 的 I_use 应接近 0 dB（不是 N/A、不是 PASS）。
    assert!(
        worst.i_use_db.abs() <= 1.0,
        "identity I_use {}",
        worst.i_use_db
    );
}

// ===========================================================================
// 4. 最差点：单点不达标必须被逐点门检出（汇总不得遮盖）
// ===========================================================================

#[test]
fn t53_holdout_worst_point_reported() {
    let fixture = pass_fixture();
    let bad_rows: [[f64; 5]; 3] = [HOLDOUT_ROWS[0], HOLDOUT_ROWS[1], [2.4, 3.0, 1.8, 3.0, 1.8]];
    let holdout_plant = make_holdout_plant(
        &fixture.baseline,
        &fixture.participants,
        &fixture.grid,
        &bad_rows,
    );
    let repeats = make_repeats(
        &fixture.participants,
        &fixture.grid,
        &HOLDOUT_LABELS,
        1.0e-3,
    );
    let irs = make_band_ir(&HOLDOUT_LABELS, 0.40, 0.32, SECONDS_IR);
    let request = full_request(&fixture.design_plant, repeats, irs);
    let report = validate_holdout(
        &fixture.target,
        &fixture.design_plant,
        &holdout_plant,
        &fixture.solution,
        &request,
    )
    .unwrap();
    assert_eq!(report.verdict, HoldoutVerdict::PredictedFail);
    assert_eq!(report.prediction.gate, GateStatus::Fail);
    let worst = report.prediction.worst_i_use.as_ref().unwrap();
    assert_eq!(worst.position_label, "H03");
    assert!(worst.i_use_db < IMPROVEMENT_GATE_DB);
    // 逐点门：H03 的 5 个 input 全部低于门；其余两个测点仍高。
    for participant in fixture.participants.participants() {
        let bad = point_by_role(&report, "H03", &participant.role_id);
        assert_eq!(bad.status, PointStatus::Valid);
        assert!(bad.i_use_db.unwrap() < IMPROVEMENT_GATE_DB);
        assert!(bad.e_off.unwrap() > 0.0);
        assert!(bad.e_on.unwrap() > 0.0);
    }
    let good = point_by_role(&report, "H01", "FL");
    assert!(good.i_use_db.unwrap() >= IMPROVEMENT_GATE_DB);
    // 每 input 最小值必须落在 H03 上，且被记录。
    for (_, min) in &report.prediction.per_input_min_db {
        assert!(min.unwrap() < IMPROVEMENT_GATE_DB);
    }
}

// ===========================================================================
// 5. 近零基线 N/A（§4.4）：E_off ≤ 4·E_noise → N/A；100% N/A → INCOMPLETE
// ===========================================================================

#[test]
fn t53_holdout_near_zero_na_rule() {
    let fixture = pass_fixture();
    // holdout 测点的 H_i 恰等于目标 D_i ⇒ G=I 基线误差为 0 ⇒ 全部 N/A。
    let mut target_magnitudes = [0.0f64; 5];
    for participant in fixture.participants.participants() {
        let response = fixture
            .target
            .diag_response(None, participant.dense_index)
            .expect("target diag response");
        target_magnitudes[participant.dense_index as usize] = response[0].re.hypot(response[0].im);
    }
    let rows = [target_magnitudes, target_magnitudes, target_magnitudes];
    let holdout_plant = make_holdout_plant(
        &fixture.baseline,
        &fixture.participants,
        &fixture.grid,
        &rows,
    );
    let repeats = make_repeats(
        &fixture.participants,
        &fixture.grid,
        &HOLDOUT_LABELS,
        1.0e-3,
    );
    let request = full_request(&fixture.design_plant, repeats, Vec::new());
    let report = validate_holdout(
        &fixture.target,
        &fixture.design_plant,
        &holdout_plant,
        &fixture.solution,
        &request,
    )
    .unwrap();
    assert_eq!(report.prediction.near_zero_points, 15);
    assert_eq!(report.prediction.valid_points, 0);
    assert!(report.prediction.near_zero_fraction > 0.20);
    assert_eq!(report.prediction.gate, GateStatus::Incomplete);
    assert_eq!(report.prediction.gate_code, "I-07");
    assert_ne!(report.verdict, HoldoutVerdict::PredictedPass);
    assert_eq!(report.verdict, HoldoutVerdict::Incomplete);
    for point in &report.prediction.points {
        assert_eq!(point.status, PointStatus::NearZero);
        assert_eq!(point.status_reason, Some("e_off_le_4x_e_noise"));
        assert!(point.e_off.unwrap() >= 0.0);
        assert!(point.e_off.unwrap() <= 4.0 * point.e_noise.unwrap());
    }
}

// ===========================================================================
// 6. holdout 不调参：tuning labels 含 holdout → F-07
// ===========================================================================

#[test]
fn t53_holdout_tuning_pollution_fails() {
    let fixture = pass_fixture();
    let repeats = make_repeats(
        &fixture.participants,
        &fixture.grid,
        &HOLDOUT_LABELS,
        1.0e-3,
    );
    let irs = make_band_ir(&HOLDOUT_LABELS, 0.40, 0.32, SECONDS_IR);
    let mut declaration = make_declaration(&fixture.design_plant, &HOLDOUT_LABELS, true);
    let mut tuned = declaration.tuning_labels.clone();
    tuned.push("H02".to_string());
    declaration.tuning_labels = tuned;
    let request = HoldoutRequest::new(declaration)
        .with_repeats(repeats)
        .with_ir(irs, IrOrigin::PredictedModel);
    let report = validate_holdout(
        &fixture.target,
        &fixture.design_plant,
        &fixture.holdout_plant,
        &fixture.solution,
        &request,
    )
    .unwrap();
    assert_eq!(report.isolation.gate, GateStatus::Fail);
    assert_eq!(report.isolation.gate_code, "F-07");
    assert!(report
        .isolation
        .tuning_holdout_overlap
        .contains(&"H02".to_string()));
    assert_eq!(report.verdict, HoldoutVerdict::PredictedFail);
    assert_eq!(report.verdict_code, "F-07");
    // 即使预测/T20 门本身通过，污染也必须 FAIL（不得只看结果）。
    assert_eq!(report.prediction.gate, GateStatus::Pass);
    assert_eq!(report.t20.gate, GateStatus::Pass);
}

// ===========================================================================
// 7. seal 不可证 → I-04 INCOMPLETE
// ===========================================================================

#[test]
fn t53_holdout_seal_not_provable_incomplete() {
    let fixture = pass_fixture();
    let repeats = make_repeats(
        &fixture.participants,
        &fixture.grid,
        &HOLDOUT_LABELS,
        1.0e-3,
    );
    let irs = make_band_ir(&HOLDOUT_LABELS, 0.40, 0.32, SECONDS_IR);
    let mut declaration = make_declaration(&fixture.design_plant, &HOLDOUT_LABELS, false);
    declaration.holdout_sealed_before_design = false;
    let request = HoldoutRequest::new(declaration)
        .with_repeats(repeats)
        .with_ir(irs, IrOrigin::PredictedModel);
    let report = validate_holdout(
        &fixture.target,
        &fixture.design_plant,
        &fixture.holdout_plant,
        &fixture.solution,
        &request,
    )
    .unwrap();
    assert_eq!(report.isolation.gate, GateStatus::Incomplete);
    assert_eq!(report.isolation.gate_code, "I-04");
    assert_eq!(report.verdict, HoldoutVerdict::Incomplete);
    assert_eq!(report.verdict_code, "I-04");
    assert!(report
        .isolation
        .reasons
        .contains(&"holdout_seal_not_provable"));
}

// ===========================================================================
// 8. holdout 清单 hash 不符（事后替换/篡改）→ F-07
// ===========================================================================

#[test]
fn t53_holdout_hash_mismatch_fails() {
    let fixture = pass_fixture();
    let repeats = make_repeats(
        &fixture.participants,
        &fixture.grid,
        &HOLDOUT_LABELS,
        1.0e-3,
    );
    let irs = make_band_ir(&HOLDOUT_LABELS, 0.40, 0.32, SECONDS_IR);
    let mut declaration = make_declaration(&fixture.design_plant, &HOLDOUT_LABELS, true);
    declaration.holdout_labels_sha256 = "0".repeat(64);
    let request = HoldoutRequest::new(declaration)
        .with_repeats(repeats)
        .with_ir(irs, IrOrigin::PredictedModel);
    let report = validate_holdout(
        &fixture.target,
        &fixture.design_plant,
        &fixture.holdout_plant,
        &fixture.solution,
        &request,
    )
    .unwrap();
    assert!(!report.isolation.labels_hash_match);
    assert_eq!(report.isolation.gate, GateStatus::Fail);
    assert_eq!(report.verdict, HoldoutVerdict::PredictedFail);
    assert_eq!(report.verdict_code, "F-07");
    assert!(report
        .isolation
        .reasons
        .contains(&"holdout_labels_sha256_mismatch"));
}

// ===========================================================================
// 9-11. 缺证据：noise / IR / 都没有
// ===========================================================================

#[test]
fn t53_holdout_missing_noise_incomplete() {
    let fixture = pass_fixture();
    let irs = make_band_ir(&HOLDOUT_LABELS, 0.40, 0.32, SECONDS_IR);
    let request = full_request(&fixture.design_plant, Vec::new(), irs);
    let report = validate_holdout(
        &fixture.target,
        &fixture.design_plant,
        &fixture.holdout_plant,
        &fixture.solution,
        &request,
    )
    .unwrap();
    assert_eq!(report.prediction.gate, GateStatus::NotRun);
    assert_eq!(report.t20.gate, GateStatus::Pass);
    assert_eq!(report.verdict, HoldoutVerdict::Incomplete);
    assert!(report.warnings.contains(&"no_repeat_noise_provided"));
    for point in &report.prediction.points {
        assert!(
            matches!(
                point.status,
                PointStatus::NoNoiseData | PointStatus::NearZero
            ),
            "unexpected status {:?}",
            point.status
        );
    }
}

#[test]
fn t53_holdout_missing_ir_incomplete() {
    let fixture = pass_fixture();
    let repeats = make_repeats(
        &fixture.participants,
        &fixture.grid,
        &HOLDOUT_LABELS,
        1.0e-3,
    );
    let request = full_request(&fixture.design_plant, repeats, Vec::new());
    let report = validate_holdout(
        &fixture.target,
        &fixture.design_plant,
        &fixture.holdout_plant,
        &fixture.solution,
        &request,
    )
    .unwrap();
    assert_eq!(report.t20.gate, GateStatus::NotRun);
    assert_eq!(report.prediction.gate, GateStatus::Pass);
    assert_eq!(report.verdict, HoldoutVerdict::Incomplete);
    assert!(report.warnings.contains(&"no_t20_ir_provided"));
}

#[test]
fn t53_holdout_both_missing_not_run() {
    let fixture = pass_fixture();
    let request = full_request(&fixture.design_plant, Vec::new(), Vec::new());
    let report = validate_holdout(
        &fixture.target,
        &fixture.design_plant,
        &fixture.holdout_plant,
        &fixture.solution,
        &request,
    )
    .unwrap();
    assert_eq!(report.prediction.gate, GateStatus::NotRun);
    assert_eq!(report.t20.gate, GateStatus::NotRun);
    assert_eq!(report.verdict, HoldoutVerdict::NotRun);
    assert_eq!(report.verdict_code, "not_run");
}

// ===========================================================================
// 12. σ 定义：E_noise 随重复差平方增长；σ_I 记录
// ===========================================================================

#[test]
fn t53_holdout_sigma_definition_and_monotonic() {
    let fixture = pass_fixture();
    let irs = make_band_ir(&HOLDOUT_LABELS, 0.40, 0.32, SECONDS_IR);
    let run = |delta: f64| -> khaen_modal_design::validate::HoldoutReport {
        let repeats = make_repeats(&fixture.participants, &fixture.grid, &HOLDOUT_LABELS, delta);
        let request = full_request(&fixture.design_plant, repeats, irs.clone());
        validate_holdout(
            &fixture.target,
            &fixture.design_plant,
            &fixture.holdout_plant,
            &fixture.solution,
            &request,
        )
        .unwrap()
    };
    let small = run(1.0e-3);
    let large = run(2.0e-3);
    let sigma_small = small.prediction.sigma.clone();
    let sigma_large = large.prediction.sigma.clone();
    assert_eq!(sigma_small.repeat_pair_count, 15); // 3 positions × 5 roles × 1 pair
    assert_eq!(sigma_large.repeat_pair_count, 15);
    assert!(sigma_small
        .note
        .contains("not a measured confidence interval"));
    let point_small = point_by_role(&small, "H01", "FL");
    let point_large = point_by_role(&large, "H01", "FL");
    let ratio = point_large.e_noise.unwrap() / point_small.e_noise.unwrap();
    assert!(
        (ratio - 4.0).abs() < 1.0e-6,
        "doubling repeat difference must quadruple E_noise, got {ratio}"
    );
    assert!(point_small.sigma_i_db.unwrap() > 0.0);
    assert!(point_large.sigma_abs_rms.unwrap() > point_small.sigma_abs_rms.unwrap());
    // 噪声增大 → I_use 变小（I_use 用 max(E_on,E_noise) 保守化）。
    assert!(point_large.i_use_db.unwrap() < point_small.i_use_db.unwrap());
}

// ===========================================================================
// 13. holdout 点数不足 → I-03（不得用 design 点替补）
// ===========================================================================

#[test]
fn t53_holdout_too_few_positions_incomplete() {
    let fixture = pass_fixture();
    let rows = [HOLDOUT_ROWS[0], HOLDOUT_ROWS[1]];
    let holdout_plant = make_holdout_plant(
        &fixture.baseline,
        &fixture.participants,
        &fixture.grid,
        &rows,
    );
    let labels = ["H01", "H02"];
    let repeats = make_repeats(&fixture.participants, &fixture.grid, &labels, 1.0e-3);
    let irs = make_band_ir(&labels, 0.40, 0.32, SECONDS_IR);
    let request = HoldoutRequest::new(make_declaration(&fixture.design_plant, &labels, true))
        .with_repeats(repeats)
        .with_ir(irs, IrOrigin::PredictedModel);
    let report = validate_holdout(
        &fixture.target,
        &fixture.design_plant,
        &holdout_plant,
        &fixture.solution,
        &request,
    )
    .unwrap();
    assert_eq!(report.prediction.gate, GateStatus::Incomplete);
    assert_eq!(report.prediction.gate_code, "I-03");
    assert!(report
        .prediction
        .reasons
        .contains(&"holdout_positions_below_min"));
    assert_eq!(report.verdict, HoldoutVerdict::Incomplete);
    assert!(report
        .warnings
        .contains(&"holdout_positions_below_formal_minimum"));
}

// ===========================================================================
// 14. preserve target 未在 holdout 冻结 → I-09
// ===========================================================================

#[test]
fn t53_holdout_preserve_target_incomplete() {
    let fixture = pass_fixture();
    let preserve_target = build_target(&fixture.baseline, &fixture.design_plant, preserve_decay());
    let repeat_plant = make_design_plant(&fixture.baseline, &fixture.participants, &fixture.grid);
    let identity = {
        let roles: Vec<String> = fixture
            .participants
            .participants()
            .iter()
            .map(|participant| participant.role_id.clone())
            .collect();
        ModalMatrix::identity(roles, fixture.grid.clone()).unwrap()
    };
    let repeats = make_repeats(
        &fixture.participants,
        &fixture.grid,
        &HOLDOUT_LABELS,
        1.0e-3,
    );
    let irs = make_band_ir(&HOLDOUT_LABELS, 0.40, 0.32, SECONDS_IR);
    let request = full_request(&fixture.design_plant, repeats, irs);
    let report = validate_holdout(
        &preserve_target,
        &repeat_plant,
        &fixture.holdout_plant,
        &identity,
        &request,
    )
    .unwrap();
    assert_eq!(report.prediction.gate, GateStatus::Incomplete);
    assert_eq!(report.prediction.gate_code, "I-09");
    assert!(report
        .warnings
        .contains(&"preserve_target_not_holdout_frozen"));
    assert_ne!(report.verdict, HoldoutVerdict::PredictedPass);
}

// ===========================================================================
// 15. T20 有效窗：估计精度（合成指数衰减，T20 = T60）
// ===========================================================================

#[test]
fn t53_holdout_t20_window_accuracy() {
    let ir = exponential_decay(0.40, SECONDS_IR);
    let fit = estimate_t20_window(&ir, FS, 63.0).unwrap();
    assert!(fit.valid, "window must be valid, reasons {:?}", fit.reasons);
    let t20 = fit.t20_s.unwrap();
    assert!((t20 - 0.40).abs() < 0.008, "T20 estimate {t20}");
    assert!(fit.r_squared.unwrap() > 0.9999);
    assert!(fit.residual_rms_db.unwrap() < 0.01);
    assert!(fit.noise_margin_db.unwrap() >= 10.0);
    assert!(fit.fit_points >= 16);
    let t25 = fit.t_minus_25_s.unwrap();
    assert!((t25 - 25.0 / (8.685_889_000_000_001 * 17.269_386_72)).abs() < 0.01);
    assert!(fit.t20_sigma_s.unwrap() >= 0.0);
    assert!(fit.slope_db_per_s.unwrap() < 0.0);
    assert!(fit.duration_s >= 1.2 * t25);
}

// ===========================================================================
// 16. T20 有效窗：采集时长不够 → 无效（不得当 T20=0 或混入统计）
// ===========================================================================

#[test]
fn t53_holdout_t20_window_short_record() {
    // 截断到 0.1 s 的 0.4 s 指数衰减：ECDC 尾部还没到噪声底、采集时长不足 V2；
    // 这样的窗口必须 N/A，不得把截断斜率当有效 T20。
    let short = exponential_decay(0.40, 0.10);
    let fit = estimate_t20_window(&short, FS, 63.0).unwrap();
    assert!(
        !fit.valid,
        "truncated record must be invalid: {:?}",
        fit.reasons
    );
    assert!(
        fit.reasons.contains(&"v2_duration"),
        "expected V2 duration failure, reasons {:?}, t25={:?}, duration={}",
        fit.reasons,
        fit.t_minus_25_s,
        fit.duration_s
    );
    // 全零 IR 必须被拒为 zero_energy，不能当 T20=0。
    let zeros = vec![0.0f64; 4800];
    let zero_fit = estimate_t20_window(&zeros, FS, 63.0).unwrap();
    assert!(!zero_fit.valid);
    assert!(zero_fit.reasons.contains(&"zero_energy"));
    // 空 IR 同样无效。
    let empty_fit = estimate_t20_window(&[], FS, 63.0).unwrap();
    assert!(!empty_fit.valid);
    assert!(empty_fit.reasons.contains(&"no_samples"));
}

// ===========================================================================
// 17. T20 有效窗：噪声底余量不足（V1）→ 无效
// ===========================================================================

#[test]
fn t53_holdout_t20_window_noise_floor() {
    let mut noisy = exponential_decay(0.40, SECONDS_IR);
    let sigma = 4.4e-3;
    for (index, sample) in noisy.iter_mut().enumerate() {
        *sample += sigma * ((index as f64) * 1.234_567).sin();
    }
    let fit = estimate_t20_window(&noisy, FS, 63.0).unwrap();
    assert!(
        !fit.valid,
        "noisy window must be invalid: {:?}",
        fit.reasons
    );
    assert!(
        fit.reasons.contains(&"v1_noise_margin"),
        "expected V1 failure, reasons {:?}, noise_floor {:?}",
        fit.reasons,
        fit.noise_floor_db
    );
    assert!(fit.noise_margin_db.unwrap() < 10.0);
}

// ===========================================================================
// 18. T20 有效窗：物理范围（V4）→ 无效
// ===========================================================================

#[test]
fn t53_holdout_t20_window_out_of_range() {
    let slow = exponential_decay(3.0, 2.5);
    let fit = estimate_t20_window(&slow, FS, 63.0).unwrap();
    assert!(!fit.valid, "T20=3 s must be invalid: {:?}", fit.reasons);
    assert!(
        fit.reasons.contains(&"v4_t20_range"),
        "reasons {:?}",
        fit.reasons
    );
    assert!(fit.t20_s.unwrap() > 2.0);
}

// ===========================================================================
// 19. T20 门失败（on 无改善）→ F-02（不是 PASS/INCOMPLETE）
// ===========================================================================

#[test]
fn t53_holdout_t20_gate_fail() {
    let fixture = pass_fixture();
    let repeats = make_repeats(
        &fixture.participants,
        &fixture.grid,
        &HOLDOUT_LABELS,
        1.0e-3,
    );
    let irs = make_band_ir(&HOLDOUT_LABELS, 0.40, 0.39, SECONDS_IR);
    let request = full_request(&fixture.design_plant, repeats, irs);
    let report = validate_holdout(
        &fixture.target,
        &fixture.design_plant,
        &fixture.holdout_plant,
        &fixture.solution,
        &request,
    )
    .unwrap();
    assert_eq!(report.prediction.gate, GateStatus::Pass);
    assert_eq!(report.t20.gate, GateStatus::Fail);
    assert_eq!(report.t20.gate_code, "F-02");
    for band in &report.t20.bands {
        assert_eq!(band.gate, GateStatus::Fail);
        assert!(band.median_ratio.unwrap() > T20_REDUCTION_GATE_RATIO);
    }
    assert_eq!(report.verdict, HoldoutVerdict::PredictedFail);
    assert_eq!(report.verdict_code, "F-02");
}

// ===========================================================================
// 20. T20 证据不足（缺测点 IR / snr 不足）→ I-06 INCOMPLETE
// ===========================================================================

#[test]
fn t53_holdout_t20_incomplete_positions() {
    let fixture = pass_fixture();
    let repeats = make_repeats(
        &fixture.participants,
        &fixture.grid,
        &HOLDOUT_LABELS,
        1.0e-3,
    );
    let labels = ["H01", "H02"];
    let irs = make_band_ir(&labels, 0.40, 0.32, SECONDS_IR);
    let request = full_request(&fixture.design_plant, repeats, irs);
    let report = validate_holdout(
        &fixture.target,
        &fixture.design_plant,
        &fixture.holdout_plant,
        &fixture.solution,
        &request,
    )
    .unwrap();
    assert_eq!(report.prediction.gate, GateStatus::Pass);
    assert_eq!(report.t20.gate, GateStatus::Incomplete);
    assert_eq!(report.t20.gate_code, "I-06");
    for band in &report.t20.bands {
        assert_eq!(band.gate, GateStatus::Incomplete);
        assert_eq!(band.valid_positions, 2);
        assert!(band.reasons.contains(&"t20_valid_positions_insufficient"));
    }
    assert_eq!(report.verdict, HoldoutVerdict::Incomplete);
}

#[test]
fn t53_holdout_t20_snr_below_min_incomplete() {
    let fixture = pass_fixture();
    let repeats = make_repeats(
        &fixture.participants,
        &fixture.grid,
        &HOLDOUT_LABELS,
        1.0e-3,
    );
    let mut irs = make_band_ir(&HOLDOUT_LABELS, 0.40, 0.32, SECONDS_IR);
    for ir in irs.iter_mut() {
        ir.snr_db = 20.0;
    }
    let request = full_request(&fixture.design_plant, repeats, irs);
    let report = validate_holdout(
        &fixture.target,
        &fixture.design_plant,
        &fixture.holdout_plant,
        &fixture.solution,
        &request,
    )
    .unwrap();
    assert_ne!(report.t20.gate, GateStatus::Pass);
    assert_ne!(report.verdict, HoldoutVerdict::PredictedPass);
    let mut saw_snr_reason = false;
    for band in &report.t20.bands {
        for position in &band.positions {
            if let Some(off) = &position.off {
                if off.reasons.contains(&"v5_snr_below_min") {
                    saw_snr_reason = true;
                }
            }
        }
    }
    assert!(saw_snr_reason, "V5 SNR failure must be reported");
}

// ===========================================================================
// 21. 1/3 oct 带限滤波器（raw IR 预处理的公开实现）
// ===========================================================================

#[test]
fn t53_holdout_band_filter() {
    let tone = |freq_hz: f64| -> Vec<f64> {
        (0..FS as usize / 2)
            .map(|index| (2.0 * std::f64::consts::PI * freq_hz * index as f64 / FS as f64).sin())
            .collect()
    };
    let input = tone(63.0);
    let filtered = band_filter_ir(&input, FS, 63.0).unwrap();
    assert_eq!(filtered.len(), input.len());
    assert!(filtered.iter().all(|sample| sample.is_finite()));
    let start = filtered.len() * 8 / 10;
    let rms = (filtered[start..].iter().map(|s| s * s).sum::<f64>()
        / (filtered.len() - start) as f64)
        .sqrt();
    assert!((rms - 0.707_106).abs() < 0.08, "63 Hz passband RMS {rms}");
    let high_input = tone(1000.0);
    let high_filtered = band_filter_ir(&high_input, FS, 63.0).unwrap();
    let high_rms = (high_filtered[start..].iter().map(|s| s * s).sum::<f64>()
        / (high_filtered.len() - start) as f64)
        .sqrt();
    assert!(high_rms < 0.05, "1 kHz must be attenuated, RMS {high_rms}");
    // 空/非有限输入必须拒收。
    assert!(band_filter_ir(&[], FS, 63.0).is_err());
    assert!(band_filter_ir(&[0.0, f64::NAN], FS, 63.0).is_err());
}

// ===========================================================================
// 22. 绑定/输入契约错误
// ===========================================================================

fn expect_error_with_code(
    result: Result<
        khaen_modal_design::validate::HoldoutReport,
        khaen_modal_design::validate::ValidateError,
    >,
    code: &str,
) {
    match result {
        Ok(report) => panic!(
            "expected {code}, got verdict {} ({})",
            report.verdict.as_str(),
            report.verdict_code
        ),
        Err(error) => assert_eq!(error.code(), code, "unexpected error: {error}"),
    }
}

#[test]
fn t53_holdout_plant_binding_errors() {
    let fixture = pass_fixture();
    let request = full_request(&fixture.design_plant, Vec::new(), Vec::new());
    // design plant 不是构建 target 的那一份 ⇒ E_VAL_BASELINE。
    let mut changed_rows = DESIGN_ROWS;
    changed_rows[0][0] = 1.2;
    let changed_plant = make_plant(
        &fixture.baseline,
        &fixture.participants,
        &fixture.grid,
        changed_rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                raw_position_row(
                    &format!("P{:02}", index + 1),
                    khaen_modal_design::ingest::PositionRole::Design,
                    &fixture.participants,
                    &fixture.grid,
                    row,
                )
            })
            .collect(),
    );
    expect_error_with_code(
        validate_holdout(
            &fixture.target,
            &changed_plant,
            &fixture.holdout_plant,
            &fixture.solution,
            &request,
        ),
        "E_VAL_BASELINE",
    );
    // holdout plant 来自另一份冻结基线 ⇒ E_VAL_BASELINE。
    let other_baseline = make_baseline(
        &fixture.grid,
        fixture.participants.clone(),
        "t53-baseline-other",
        b"t53-fixture-other",
    );
    let other_holdout = make_holdout_plant(
        &other_baseline,
        &fixture.participants,
        &fixture.grid,
        &HOLDOUT_ROWS,
    );
    expect_error_with_code(
        validate_holdout(
            &fixture.target,
            &fixture.design_plant,
            &other_holdout,
            &fixture.solution,
            &request,
        ),
        "E_VAL_BASELINE",
    );
}

#[test]
fn t53_holdout_bad_request_inputs() {
    let fixture = pass_fixture();
    let good_repeats = make_repeats(
        &fixture.participants,
        &fixture.grid,
        &HOLDOUT_LABELS,
        1.0e-3,
    );
    let good_ir = make_band_ir(&HOLDOUT_LABELS, 0.40, 0.32, SECONDS_IR);
    let run = |request: &HoldoutRequest| {
        validate_holdout(
            &fixture.target,
            &fixture.design_plant,
            &fixture.holdout_plant,
            &fixture.solution,
            request,
        )
    };
    let base = || make_declaration(&fixture.design_plant, &HOLDOUT_LABELS, true);

    // 缺调参输入 hash。
    let mut declaration = base();
    declaration.tuning_inputs_sha256 = String::new();
    let request = HoldoutRequest::new(declaration)
        .with_repeats(good_repeats.clone())
        .with_ir(good_ir.clone(), IrOrigin::PredictedModel);
    expect_error_with_code(run(&request), "E_VAL_HOLDOUT");

    // design labels 声明与 design plant 不一致。
    let mut declaration = base();
    declaration.design_labels.push("PX".to_string());
    let request = HoldoutRequest::new(declaration).with_repeats(good_repeats.clone());
    expect_error_with_code(run(&request), "E_VAL_HOLDOUT");

    // holdout labels 与 holdout plant 不一致。
    let mut declaration = base();
    declaration.holdout_labels = vec!["H01".to_string(), "H99".to_string()];
    let request = HoldoutRequest::new(declaration).with_repeats(good_repeats.clone());
    expect_error_with_code(run(&request), "E_VAL_HOLDOUT");

    // 重复噪声测点不在 holdout 清单。
    let mut repeats = good_repeats.clone();
    repeats[0].position_label = "H99".to_string();
    let request = HoldoutRequest::new(base()).with_repeats(repeats);
    expect_error_with_code(run(&request), "E_VAL_REPEATS");

    // 重复噪声缺角色。
    let mut repeats = good_repeats.clone();
    repeats[0].roles.pop();
    let request = HoldoutRequest::new(base()).with_repeats(repeats);
    expect_error_with_code(run(&request), "E_VAL_REPEATS");

    // 重复噪声只有 1 条序列（不能估噪声）。
    let mut repeats = good_repeats.clone();
    repeats[0].roles[0].series.truncate(1);
    let request = HoldoutRequest::new(base()).with_repeats(repeats);
    expect_error_with_code(run(&request), "E_VAL_REPEATS");

    // IR band 非 T6 必需/报告频带。
    let mut irs = good_ir.clone();
    irs[0].band_hz = 100.0;
    let request = HoldoutRequest::new(base()).with_ir(irs, IrOrigin::PredictedModel);
    expect_error_with_code(run(&request), "E_VAL_T20");

    // 重复 IR。
    let mut irs = good_ir.clone();
    irs.push(irs[0].clone());
    let request = HoldoutRequest::new(base()).with_ir(irs, IrOrigin::PredictedModel);
    expect_error_with_code(run(&request), "E_VAL_T20");

    // IR 测点不在 holdout 清单。
    let mut irs = good_ir.clone();
    irs[0].position_label = "H99".to_string();
    let request = HoldoutRequest::new(base()).with_ir(irs, IrOrigin::PredictedModel);
    expect_error_with_code(run(&request), "E_VAL_T20");
}

// ===========================================================================
// 23. 确定性 hash / 证据 JSON 契约 / holdout label hash
// ===========================================================================

#[test]
fn t53_holdout_hash_deterministic_and_json_contract() {
    let fixture = pass_fixture();
    let build = |delta: f64| {
        let repeats = make_repeats(&fixture.participants, &fixture.grid, &HOLDOUT_LABELS, delta);
        let irs = make_band_ir(&HOLDOUT_LABELS, 0.40, 0.32, SECONDS_IR);
        let request = full_request(&fixture.design_plant, repeats, irs);
        validate_holdout(
            &fixture.target,
            &fixture.design_plant,
            &fixture.holdout_plant,
            &fixture.solution,
            &request,
        )
        .unwrap()
    };
    let first = build(1.0e-3);
    let second = build(1.0e-3);
    assert_eq!(first.report_hash, second.report_hash);
    assert_eq!(first.to_evidence_json(), second.to_evidence_json());
    let changed = build(2.0e-3);
    assert_ne!(first.report_hash, changed.report_hash);
    assert_eq!(first.report_hash.len(), 64);
    assert_eq!(first.content_hash(), first.report_hash);
    let json = first.to_evidence_json();
    JsonSyntax::validate(&json).expect("evidence JSON must be valid");
    assert!(json.contains("\"schema\":\"khaen-modal-holdout-report/v1\""));
    assert!(json.contains("\"verdict\":\"PREDICTED_PASS\""));
    assert!(json.contains("\"improvement_gate_db\":3."));
    assert!(json.contains("\"applied\":false"));
    assert!(json.contains("\"measured\":false"));
    assert!(json.contains("\"can_substitute_real_remeasurement\":false"));
    assert!(json.contains("\"requires_post_apply_remeasurement\":true"));
    assert!(json.contains("\"predicted_only_not_measured\""));
    assert!(json.contains("\"sigma_squared_complex_c"));
    assert!(json.contains("\"report_hash\":\""));
    // holdout labels hash 是排序后的规范 hash（与输入顺序无关）。
    let ordered = vec!["H01".to_string(), "H02".to_string(), "H03".to_string()];
    let shuffled = vec!["H03".to_string(), "H01".to_string(), "H02".to_string()];
    assert_eq!(
        holdout_labels_sha256(&ordered),
        holdout_labels_sha256(&shuffled)
    );
    assert_ne!(
        holdout_labels_sha256(&ordered),
        holdout_labels_sha256(&["H01".to_string(), "H02".to_string()])
    );
}

// ===========================================================================
// 24. 静态守卫：无 I/O/线程/环境/序列化/unsafe
// ===========================================================================

#[test]
fn t53_holdout_static_guards() {
    let source = khaen_modal_design::validate::validate_source_text_for_guards();
    assert!(source.contains("pub fn validate_holdout"));
    assert!(source.contains("DECLARED_GATES"));
    assert!(source.contains("sigma_squared_complex_c"));
    assert!(source.contains("Schroeder"));
    for forbidden in [
        "std::fs",
        "std::process",
        "std::thread",
        "std::env",
        "std::net",
        "std::io",
        "File::",
        "Command::",
        "TcpStream",
        "thread::spawn",
        "unsafe {",
        "serde",
    ] {
        assert!(
            !source.contains(forbidden),
            "validate.rs must not contain forbidden pattern {forbidden}"
        );
    }
}

// ===========================================================================
// 25. 已知夹具 dump（供 evidence/T53 独立复算；stdout 由证据脚本收集）
// ===========================================================================

#[test]
fn t53_holdout_known_fixture_dump() {
    let fixture = pass_fixture();
    let repeats = make_repeats(
        &fixture.participants,
        &fixture.grid,
        &HOLDOUT_LABELS,
        1.0e-3,
    );
    let irs = make_band_ir(&HOLDOUT_LABELS, 0.40, 0.32, SECONDS_IR);
    let request = full_request(&fixture.design_plant, repeats, irs);
    let report = validate_holdout(
        &fixture.target,
        &fixture.design_plant,
        &fixture.holdout_plant,
        &fixture.solution,
        &request,
    )
    .unwrap();
    // 独立 checker 需要的输入夹具（最小可复算集：网格/目标/plant/G/噪声/IR 参数）。
    let target_diag: Vec<f64> = fixture
        .participants
        .participants()
        .iter()
        .map(|participant| {
            let response = fixture
                .target
                .diag_response(None, participant.dense_index)
                .expect("target diag");
            response[0].re.hypot(response[0].im)
        })
        .collect();
    let mut g_matrix: Vec<Vec<f64>> = Vec::new();
    for out_dense in 0..5u16 {
        let mut row = Vec::new();
        for in_dense in 0..5u16 {
            let points = fixture
                .solution
                .cell(out_dense, in_dense)
                .expect("complete G fixture");
            row.push(points[0].re);
        }
        g_matrix.push(row);
    }
    let mut inputs = String::from("{\"schema\":\"khaen-modal-holdout-fixture-inputs/v1\",");
    inputs.push_str("\"fs\":48000,\"grid\":[");
    inputs.push_str(
        &fixture
            .grid
            .iter()
            .map(|freq| format!("{freq:.17e}"))
            .collect::<Vec<String>>()
            .join(","),
    );
    inputs.push_str("],\"design_rows\":[");
    inputs.push_str(
        &DESIGN_ROWS
            .iter()
            .map(|row| {
                format!(
                    "[{}]",
                    row.iter()
                        .map(|value| format!("{value:.17e}"))
                        .collect::<Vec<String>>()
                        .join(",")
                )
            })
            .collect::<Vec<String>>()
            .join(","),
    );
    inputs.push_str("],\"holdout_labels\":[\"H01\",\"H02\",\"H03\"],\"holdout_rows\":[");
    inputs.push_str(
        &HOLDOUT_ROWS
            .iter()
            .map(|row| {
                format!(
                    "[{}]",
                    row.iter()
                        .map(|value| format!("{value:.17e}"))
                        .collect::<Vec<String>>()
                        .join(",")
                )
            })
            .collect::<Vec<String>>()
            .join(","),
    );
    inputs.push_str("],\"target_diag_magnitude\":[");
    inputs.push_str(
        &target_diag
            .iter()
            .map(|value| format!("{value:.17e}"))
            .collect::<Vec<String>>()
            .join(","),
    );
    inputs.push_str("],\"g_matrix\":[");
    inputs.push_str(
        &g_matrix
            .iter()
            .map(|row| {
                format!(
                    "[{}]",
                    row.iter()
                        .map(|value| format!("{value:.17e}"))
                        .collect::<Vec<String>>()
                        .join(",")
                )
            })
            .collect::<Vec<String>>()
            .join(","),
    );
    inputs.push_str("],\"repeat_delta\":1.00000000000000000e-3,\"ir\":{\"off_t60_s\":4.00000000000000000e-1,\"on_t60_s\":3.20000000000000000e-1,\"seconds\":1.50000000000000000e0},");
    inputs.push_str("\"gates\":{\"improvement_gate_db\":3.0,\"t20_reduction_gate_ratio\":0.85,\"t20_position_worsen_ratio\":1.05,\"near_zero_ratio\":4.0,\"improvement_band_hz\":[63.0,250.0],\"t20_noise_margin_db\":10.0,\"t20_min_r2\":0.99,\"t20_max_residual_rms_db\":1.0,\"t20_range_s\":[0.05,2.0],\"t20_min_snr_db\":25.0,\"min_holdout_positions\":3,\"min_valid_point_fraction\":0.80,\"max_na_fraction\":0.20}}");
    println!("T53_HOLDOUT_FIXTURE_BEGIN");
    println!(
        "T53_HOLDOUT_FIXTURE_SUMMARY verdict={} code={} pred_gate={} t20_gate={} valid={} candidate={}",
        report.verdict.as_str(),
        report.verdict_code,
        report.prediction.gate.as_str(),
        report.t20.gate.as_str(),
        report.prediction.valid_points,
        report.prediction.candidate_points
    );
    println!("T53_HOLDOUT_FIXTURE_INPUTS_JSON={inputs}");
    println!("T53_HOLDOUT_FIXTURE_JSON={}", report.to_evidence_json());
    println!("T53_HOLDOUT_FIXTURE_END");
    JsonSyntax::validate(&inputs).unwrap();
    JsonSyntax::validate(&report.to_evidence_json()).unwrap();
}

// ===========================================================================
// 26. validate/holdout 契约复核（任务卡 cargo test ... validate 关键字 + T50 预测摘要一致性）
// ===========================================================================

#[test]
fn t53_validate_holdout_manifest_contract() {
    let fixture = pass_fixture();
    let repeats = make_repeats(
        &fixture.participants,
        &fixture.grid,
        &HOLDOUT_LABELS,
        1.0e-3,
    );
    let irs = make_band_ir(&HOLDOUT_LABELS, 0.40, 0.32, SECONDS_IR);
    let request = full_request(&fixture.design_plant, repeats, irs);
    let report = validate_holdout(
        &fixture.target,
        &fixture.design_plant,
        &fixture.holdout_plant,
        &fixture.solution,
        &request,
    )
    .unwrap();
    assert_eq!(report.schema, VALIDATE_REPORT_SCHEMA);
    assert_eq!(report.verdict, HoldoutVerdict::PredictedPass);
    // design 预测摘要必须与 T50 validate_solution 独立调用一致（只是摘要，不作 holdout 门）。
    let design = khaen_modal_design::target::validate_solution(
        &fixture.target,
        &fixture.design_plant,
        &fixture.solution,
    )
    .unwrap();
    assert_eq!(report.design_prediction.passed, design.passed);
    assert_eq!(
        report.design_prediction.predicted_residual_max_db,
        design.residual_max_db
    );
    assert_eq!(
        report.design_prediction.predicted_residual_worst,
        design.residual_worst
    );
    assert_eq!(report.design_prediction.checks.len(), design.checks.len());
    assert!(report
        .design_prediction
        .scope
        .contains("not a holdout gate"));
    // report hash/JSON 契约。
    assert_eq!(report.content_hash(), report.report_hash);
    JsonSyntax::validate(&report.to_evidence_json()).expect("evidence JSON must be valid");
    assert!(report
        .to_evidence_json()
        .contains("\"schema\":\"khaen-modal-holdout-report/v1\""));
    assert!(report
        .to_evidence_json()
        .contains("\"verdict\":\"PREDICTED_PASS\""));
    assert!(!report.can_substitute_real_remeasurement);
    assert!(report.requires_post_apply_remeasurement);
}

// ===========================================================================
// 27. I_raw 60 dB 报告上限截断（lead review P3-1 回归）
// ===========================================================================

#[test]
fn t53_holdout_i_raw_report_cap_truncation() {
    let fixture = pass_fixture();
    let irs = make_band_ir(&HOLDOUT_LABELS, 0.40, 0.32, SECONDS_IR);
    let run = |delta: f64| -> khaen_modal_design::validate::HoldoutReport {
        let repeats = make_repeats(&fixture.participants, &fixture.grid, &HOLDOUT_LABELS, delta);
        let request = full_request(&fixture.design_plant, repeats, irs.clone());
        validate_holdout(
            &fixture.target,
            &fixture.design_plant,
            &fixture.holdout_plant,
            &fixture.solution,
            &request,
        )
        .unwrap()
    };
    assert_eq!(DECLARED_GATES.numeric_floor_i_db, 60.0);
    // δ=1e-3（正常夹具噪声）：on 预测落在数值底 → I_raw 远超 60 dB，必须截断并标注；
    // I_use 用 max(E_on,E_noise) 保守化（≈53 dB < 60 dB）→ 不截断。
    let normal = run(1.0e-3);
    let point = point_by_role(&normal, "H01", "FL");
    assert!(
        point.e_on.unwrap() < 1.0e-12,
        "fixture on-prediction must sit on the numeric floor"
    );
    assert_eq!(point.i_raw_db, Some(DECLARED_GATES.numeric_floor_i_db));
    assert!(point.i_raw_truncated);
    assert!(!point.numeric_floor);
    assert!(point.i_use_db.unwrap() < DECLARED_GATES.numeric_floor_i_db);
    assert!(!point.i_use_truncated);
    let json = normal.to_evidence_json();
    assert!(json.contains("\"i_report_cap_db\":6.00000000000000000e1"));
    assert!(json.contains("\"i_raw_truncated\":true"));
    // δ=1e-9（噪声可忽略）：I_raw 与 I_use 都超过 60 dB → 两者都截断并标注，不出现 100+ dB 报告值。
    let quiet = run(1.0e-9);
    let quiet_point = point_by_role(&quiet, "H01", "FL");
    assert_eq!(
        quiet_point.i_raw_db,
        Some(DECLARED_GATES.numeric_floor_i_db)
    );
    assert_eq!(
        quiet_point.i_use_db,
        Some(DECLARED_GATES.numeric_floor_i_db)
    );
    assert!(quiet_point.i_raw_truncated);
    assert!(quiet_point.i_use_truncated);
    assert!(!quiet_point.numeric_floor);
    assert!(quiet
        .to_evidence_json()
        .contains("\"i_use_truncated\":true"));
}

// ===========================================================================
// 28. T20 band 级“有 IR 但全部窗无效” → INCOMPLETE（lead review O-3 回归）
// ===========================================================================

#[test]
fn t53_holdout_t20_band_incomplete_without_valid_windows() {
    let fixture = pass_fixture();
    let repeats = make_repeats(
        &fixture.participants,
        &fixture.grid,
        &HOLDOUT_LABELS,
        1.0e-3,
    );
    // 0.10 s 截断 IR：V2 时长/噪声底不足 → 每个窗都无效；IR 本身已提供（不是 NOT_RUN）。
    let irs = make_band_ir(&HOLDOUT_LABELS, 0.40, 0.32, 0.10);
    let request = full_request(&fixture.design_plant, repeats, irs);
    let report = validate_holdout(
        &fixture.target,
        &fixture.design_plant,
        &fixture.holdout_plant,
        &fixture.solution,
        &request,
    )
    .unwrap();
    assert_eq!(report.t20.gate, GateStatus::Incomplete);
    assert_eq!(report.t20.gate_code, "I-06");
    assert!(report.t20.reasons.contains(&"t20_evidence_incomplete"));
    assert_eq!(report.t20.bands.len(), 2);
    for band in &report.t20.bands {
        assert_eq!(
            band.gate,
            GateStatus::Incomplete,
            "band with zero valid windows is INCOMPLETE (evidence missing), not NOT_RUN"
        );
        assert_eq!(band.gate_code, "I-06");
        assert_eq!(band.valid_positions, 0);
        assert!(band.reasons.contains(&"no_valid_t20_windows"));
    }
    assert_ne!(report.t20.gate, GateStatus::NotRun);
    assert_eq!(report.verdict, HoldoutVerdict::Incomplete);
    assert_ne!(report.verdict, HoldoutVerdict::PredictedPass);
}
