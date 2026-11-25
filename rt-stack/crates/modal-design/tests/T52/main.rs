//! T52 private integration tests — 模态正则（crate tspi-modal-design）。
//!
//! 运行（在 rt-stack/ 下）：cargo test -p tspi-modal-design regularize
//! 文件位置：crates/modal-design/tests/T52/main.rs（leaf 私有测试；不写 public_registration.rs）。
//!
//! 覆盖（WBS T52 DoD）：
//! - design 内 CV：LOO/k 折只使用 design positions，留出 design position 上预测残差；
//!   P==M 时折训练集欠定 → CV 无效 → 明确回退；P<M 标记 not_applicable 且 deliverable=false；
//! - 固定 3-4 白名单：DiagOnly/DiagAdjacent/DiagSub1/Full 结构定义与数据无关（同一 roles
//!   在任意 H 上掩码相同；DiagSub1 缺 SUB1 拒收）；
//! - lambda 扫描 / L-curve：尺度归一 lambda_used=lambda*scale、残差/解范数单调性、拐点与
//!   不可辨告警；L-curve 只作诊断（CV 有效时选择仍由 CV 决定）；
//! - gain headroom 与先验：PR-3 每路径最大增益（修正帽/对角帽/容差）、headroom 不可行时有界
//!   回退、全部不可行时 best-effort + deliverable=false；冻结先验通过等价 target 变换收缩；
//! - P>=M 适用边界标记；holdout/validation label 与 design label 冲突拒收、from_target_plant
//!   自动把 validation labels 加入 forbidden_tuning_labels；
//! - 诚实边界：applied=false/post_apply_measurement=not_requested/holdout_used_for_tuning=false；
//!   证据 JSON schema 合法（自带最小 JSON 语法校验器）；确定性 hash；静态守卫无 I/O。
//!
//! 本文件只构造内存夹具；不采音、不碰板端、不打开 ALSA、不写文件（独立 checker 的 dump 走
//! stdout，由 evidence 脚本读取）。

use tspi_modal_design::ingest::{sha256_hex, PositionRole};
use tspi_modal_design::regularize::{
    regularize, regularize_from_target_plant, HeadroomSpec, PriorKind, RegularizationPrior,
    RegularizeError, RegularizeErrorKind, RegularizeOptions, RegularizeWarning, WhitelistKind,
    APPLICABILITY_APPLICABLE, APPLICABILITY_BOUNDARY, APPLICABILITY_NOT_APPLICABLE,
    CORRECTION_GAIN_CAP_MAX_DB, CORRECTION_GAIN_CAP_MIN_DB, LAMBDA_GRID, MAX_LAMBDA,
    REGULARIZE_SCHEMA, SELECTION_METHOD_CV, SELECTION_METHOD_L_CURVE, SELECTION_METHOD_PRESERVE,
};
use tspi_modal_design::solve::{solve, SolveMode, SolveOptions, SolveProblem, SolveStatus};
use tspi_modal_design::target::{
    build_target_matrix, validate_solution, BandDecay, CurveBinding, DecayTarget, EffectivePlant,
    FittedPostBaseline, FrozenPostBaseline, ModalParticipants, PostBaselineInput, PostRoleFit,
    RawPlant, RawPosition, RawResponse, TargetMatrix, TargetSpec, ToneApplicationProof,
    ToneBinding, P1_LAYOUT_4_1_2,
};
use tspi_modal_design::timebase::{CommonD0, ComplexPoint, PolaritySource, ResolvedPolarity};

// ===========================================================================
// 常量与通用夹具
// ===========================================================================

const FS: u32 = 48_000;
const D0_SAMPLES: i64 = 120;

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
    octave_grid(20.0, 300.0, 6)
}

fn role_pool() -> [&'static str; 8] {
    ["FL", "FR", "SL", "SR", "SUB1", "C", "SUB2", "SUB3"]
}

/// 良态对角占优 3x3/5x5 混合矩阵（对角 1.0+0.05*i，非对角 <=0.15）。
fn matrix_a(m: usize) -> Vec<f64> {
    let mut matrix = vec![0.0f64; m * m];
    for row in 0..m {
        for column in 0..m {
            matrix[row * m + column] = if row == column {
                1.0 + 0.05 * row as f64
            } else {
                0.15 / (1.0 + (row as f64 - column as f64).abs())
            };
        }
    }
    matrix
}

/// 目标 G 矩阵：对角 1.0、相邻非对角 offdiag、远处 0.25*offdiag。
fn gt_matrix(m: usize, offdiag: f64) -> Vec<f64> {
    let mut matrix = vec![0.0f64; m * m];
    for row in 0..m {
        for column in 0..m {
            matrix[row * m + column] = if row == column {
                1.0
            } else if row.abs_diff(column) == 1 {
                offdiag
            } else {
                0.25 * offdiag
            };
        }
    }
    matrix
}

/// 构造 Custom 合成问题：H_{p,j}(f) 为常数实混合矩阵（任意 >=M 个 position 的行满秩；
/// 同 base 行加 position 相关扰动以避免 P>M 时 LOO 子集秩亏），target d_{p,i}=Sum_j H_{p,j}*GT[j][i]。
fn make_problem_with_scale(
    m: usize,
    positions: usize,
    grid: &[f64],
    gt: &[f64],
    h_scale: f64,
) -> SolveProblem {
    let roles: Vec<String> = role_pool()[..m]
        .iter()
        .map(|role| role.to_string())
        .collect();
    let base = matrix_a(m);
    let mut solve_positions = Vec::with_capacity(positions);
    for position in 0..positions {
        let scale = h_scale * (1.0 + 0.05 * position as f64);
        let mut mixing = vec![0.0f64; m];
        for participant in 0..m {
            let perturbation = 0.05 * ((((position + 1) * (participant + 2)) % 7) as f64) / 7.0;
            mixing[participant] = base[(position % m) * m + participant] + perturbation;
        }
        let mut h_eff = Vec::with_capacity(m);
        for participant in 0..m {
            let value = mixing[participant] * scale;
            h_eff.push(
                grid.iter()
                    .map(|freq| ComplexPoint {
                        freq_hz: *freq,
                        re: value,
                        im: 0.0,
                    })
                    .collect::<Vec<ComplexPoint>>(),
            );
        }
        let mut target = Vec::with_capacity(m);
        for input_dense in 0..m {
            let mut value = 0.0f64;
            for participant in 0..m {
                value += mixing[participant] * scale * gt[participant * m + input_dense];
            }
            target.push(
                grid.iter()
                    .map(|freq| ComplexPoint {
                        freq_hz: *freq,
                        re: value,
                        im: 0.0,
                    })
                    .collect::<Vec<ComplexPoint>>(),
            );
        }
        solve_positions.push(tspi_modal_design::solve::SolvePosition {
            label: format!("P{:02}", position + 1),
            weight: 1.0,
            h_eff,
            target,
        });
    }
    SolveProblem {
        roles,
        grid_hz: grid.to_vec(),
        sample_rate_hz: FS,
        positions: solve_positions,
        mode: SolveMode::Custom,
        target_position_variant: false,
        target_delay_samples: 0,
        provenance: None,
    }
}

fn make_problem(m: usize, positions: usize, grid: &[f64], gt: &[f64]) -> SolveProblem {
    make_problem_with_scale(m, positions, grid, gt, 1.0)
}

/// Preserve 问题：target 逐 position 恰为 H_eff（T51 Preserve 语义）。
fn make_preserve_problem(m: usize, positions: usize, grid: &[f64]) -> SolveProblem {
    let mut problem = make_problem(m, positions, grid, &gt_matrix(m, 0.2));
    problem.mode = SolveMode::Preserve;
    problem.target_position_variant = true;
    for position in &mut problem.positions {
        position.target = position.h_eff.clone();
    }
    problem
}

fn relaxed_headroom() -> HeadroomSpec {
    HeadroomSpec {
        correction_cap_db: CORRECTION_GAIN_CAP_MAX_DB,
        diagonal_cap_db: 6.0,
        tolerance_db: 2.0,
    }
}

fn base_options(taps: usize) -> RegularizeOptions {
    let mut options = RegularizeOptions::default();
    options.base.taps = taps;
    options.base.max_iterations = 64;
    options.base.rel_tolerance = 1.0e-8;
    options.lambda_grid = vec![0.0, 1.0e-4, 1.0e-2, 1.0];
    options.whitelists = vec![WhitelistKind::DiagOnly, WhitelistKind::Full];
    options.cv_folds = 0;
    options.headroom = relaxed_headroom();
    options
}

fn assert_close(actual: f64, expected: f64, tolerance: f64, context: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{context}: actual {actual} vs expected {expected} (tol {tolerance})"
    );
}

fn expect_kind<T>(
    result: Result<T, RegularizeError>,
    kind: RegularizeErrorKind,
) -> RegularizeError {
    match result {
        Ok(_) => panic!("expected RegularizeError({kind:?}), got Ok"),
        Err(error) => {
            assert_eq!(error.kind(), kind, "unexpected error: {error}");
            error
        }
    }
}

fn warning_codes(outcome: &tspi_modal_design::regularize::RegularizeOutcome) -> Vec<&'static str> {
    outcome
        .report
        .warnings
        .iter()
        .map(|warning| warning.code)
        .collect()
}

fn solution_offdiag_max_db(outcome: &tspi_modal_design::regularize::RegularizeOutcome) -> f64 {
    let solution = outcome.solution.as_ref().expect("solution required");
    let m = solution.modal.m();
    let mut maximum = f64::NEG_INFINITY;
    for out_dense in 0..m {
        for in_dense in 0..m {
            if out_dense == in_dense {
                continue;
            }
            let points = solution
                .response(out_dense as u16, in_dense as u16)
                .unwrap();
            for point in points {
                let magnitude = point.re.hypot(point.im).max(1.0e-300);
                maximum = maximum.max(20.0 * magnitude.log10());
            }
        }
    }
    maximum
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
                Some(b',') => {
                    self.position += 1;
                }
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
                Some(b',') => {
                    self.position += 1;
                }
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
        loop {
            match self.peek() {
                None => return Err("unterminated string".to_string()),
                Some(b'"') => {
                    self.position += 1;
                    return Ok(());
                }
                Some(b'\\') => {
                    self.position += 1;
                    match self.peek() {
                        Some(b'"') | Some(b'\\') | Some(b'/') | Some(b'b') | Some(b'f')
                        | Some(b'n') | Some(b'r') | Some(b't') => {
                            self.position += 1;
                        }
                        Some(b'u') => {
                            self.position += 1;
                            for _ in 0..4 {
                                match self.peek() {
                                    Some(byte) if byte.is_ascii_hexdigit() => {
                                        self.position += 1;
                                    }
                                    other => {
                                        return Err(format!(
                                            "bad unicode escape {other:?} at byte {}",
                                            self.position
                                        ))
                                    }
                                }
                            }
                        }
                        other => {
                            return Err(format!("bad escape {other:?} at byte {}", self.position))
                        }
                    }
                }
                Some(byte) if byte < 0x20 => {
                    return Err(format!("control byte in string at byte {}", self.position))
                }
                Some(_) => {
                    self.position += 1;
                }
            }
        }
    }

    fn number(&mut self) -> Result<(), String> {
        if self.peek() == Some(b'-') {
            self.position += 1;
        }
        match self.peek() {
            Some(b'0') => self.position += 1,
            Some(byte) if byte.is_ascii_digit() => {
                while matches!(self.peek(), Some(byte) if byte.is_ascii_digit()) {
                    self.position += 1;
                }
            }
            other => return Err(format!("bad number {other:?} at byte {}", self.position)),
        }
        if self.peek() == Some(b'.') {
            self.position += 1;
            if !matches!(self.peek(), Some(byte) if byte.is_ascii_digit()) {
                return Err(format!("bad fraction at byte {}", self.position));
            }
            while matches!(self.peek(), Some(byte) if byte.is_ascii_digit()) {
                self.position += 1;
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            self.position += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.position += 1;
            }
            if !matches!(self.peek(), Some(byte) if byte.is_ascii_digit()) {
                return Err(format!("bad exponent at byte {}", self.position));
            }
            while matches!(self.peek(), Some(byte) if byte.is_ascii_digit()) {
                self.position += 1;
            }
        }
        Ok(())
    }
}

// ===========================================================================
// T50 夹具（from_target_plant 桥接；合成数据，非实测）
// ===========================================================================

const GAINS_5: [[f64; 5]; 5] = [
    [1.00, 0.60, 0.40, 0.30, 0.20],
    [0.50, 1.10, 0.55, 0.35, 0.25],
    [0.35, 0.45, 1.20, 0.60, 0.40],
    [0.25, 0.30, 0.50, 1.30, 0.70],
    [0.20, 0.25, 0.35, 0.55, 1.40],
];

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
        baseline_id: "t52-baseline".to_string(),
        sample_rate_hz: FS,
        participants,
        tone,
        curve,
        tone_applications: ToneApplicationProof::baseline_only(),
        fit_inputs_sha256: sha256_hex(b"t52-fit-inputs-v1"),
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

fn gain_points(grid: &[f64], magnitude: f64) -> Vec<ComplexPoint> {
    grid.iter()
        .map(|freq| ComplexPoint {
            freq_hz: *freq,
            re: magnitude,
            im: 0.0,
        })
        .collect()
}

fn raw_position(
    label: &str,
    role: PositionRole,
    participants: &ModalParticipants,
    grid: &[f64],
    position_index: usize,
) -> RawPosition {
    let mut responses = Vec::new();
    for participant in participants.participants() {
        let dense = participant.dense_index as usize;
        // 设计位置用 GAINS_5 行（良态 5x5）；第 6 个位置用行 0 的循环移位
        // （与任何 4 行子集线性无关，保证 LOO 训练集满秩）。
        let magnitude = if position_index < 5 {
            GAINS_5[position_index][dense]
        } else {
            GAINS_5[0][(dense + 1) % 5]
        };
        responses.push(normalized_response(
            &participant.role_id,
            participant.out_index,
            gain_points(grid, magnitude),
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

fn build_reduced_target(baseline: &FrozenPostBaseline, plant: &EffectivePlant) -> TargetMatrix {
    let spec = TargetSpec::for_baseline(baseline, reduced_decay(0.85)).unwrap();
    build_target_matrix(baseline, plant, &spec).unwrap()
}

// ===========================================================================
// 1. 固定白名单（结构固定、与数据无关）
// ===========================================================================

#[test]
fn t52_regularize_fixed_whitelists_structural() {
    let roles: Vec<String> = role_pool()[..5].iter().map(|r| r.to_string()).collect();
    let counts: Vec<usize> = WhitelistKind::ALL
        .iter()
        .map(|whitelist| whitelist.free_path_count(5, &roles).unwrap())
        .collect();
    assert_eq!(counts, vec![5, 13, 13, 25]);
    assert_eq!(WhitelistKind::ALL.len(), 4);
    assert_eq!(WhitelistKind::DiagOnly.as_str(), "diag_only");
    assert_eq!(WhitelistKind::DiagAdjacent.as_str(), "diag_adjacent");
    assert_eq!(WhitelistKind::DiagSub1.as_str(), "diag_sub1");
    assert_eq!(WhitelistKind::Full.as_str(), "full");
    let diag = WhitelistKind::DiagOnly.mask(5, &roles).unwrap();
    for out_dense in 0..5 {
        for in_dense in 0..5 {
            assert_eq!(diag[out_dense * 5 + in_dense], out_dense == in_dense);
        }
    }
    let adjacent = WhitelistKind::DiagAdjacent.mask(5, &roles).unwrap();
    for out_dense in 0..5 {
        for in_dense in 0..5 {
            assert_eq!(
                adjacent[out_dense * 5 + in_dense],
                adjacent[in_dense * 5 + out_dense],
                "adjacent whitelist must be symmetric"
            );
        }
    }
    let sub1 = WhitelistKind::DiagSub1.mask(5, &roles).unwrap();
    for dense in 0..5 {
        assert!(sub1[4 * 5 + dense]);
        assert!(sub1[dense * 5 + 4]);
    }
    // 结构定义不读测量数据：两个 H 完全不同的 problem 掩码逐位一致。
    let grid = bass_grid();
    let gt = gt_matrix(5, 0.2);
    let problem_a = make_problem_with_scale(5, 6, &grid, &gt, 1.0);
    let problem_b = make_problem_with_scale(5, 6, &grid, &gt, 1000.0);
    for whitelist in WhitelistKind::ALL {
        assert_eq!(
            whitelist.mask(5, &problem_a.roles).unwrap(),
            whitelist.mask(5, &problem_b.roles).unwrap(),
            "whitelist masks must be structural, not data-driven"
        );
        assert!(!whitelist.definition().is_empty());
        assert!(whitelist.order() < 4);
    }
    // 2 角色：diag_sub1 需要 SUB1，缺则拒收（不静默退化）。
    let roles_two: Vec<String> = vec!["FL".to_string(), "FR".to_string()];
    expect_kind(
        WhitelistKind::DiagSub1.mask(2, &roles_two),
        RegularizeErrorKind::Whitelist,
    );
    let roles_sub: Vec<String> = vec!["FL".to_string(), "SUB1".to_string()];
    assert_eq!(
        WhitelistKind::DiagSub1
            .free_path_count(2, &roles_sub)
            .unwrap(),
        4
    );
    assert_eq!(
        WhitelistKind::DiagAdjacent
            .free_path_count(2, &roles_sub)
            .unwrap(),
        4
    );
    expect_kind(
        WhitelistKind::Full.mask(3, &roles_two),
        RegularizeErrorKind::Whitelist,
    );
}

// ===========================================================================
// 2. design 内 CV：选择、折平衡、只使用 design positions
// ===========================================================================

#[test]
fn t52_regularize_cv_selects_full_whitelist() {
    let grid = bass_grid();
    let gt = gt_matrix(3, 0.2);
    let problem = make_problem(3, 4, &grid, &gt);
    let options = base_options(1);
    let outcome = regularize(&problem, &options).unwrap();
    let report = &outcome.report;
    assert_eq!(report.schema, REGULARIZE_SCHEMA);
    assert_eq!(report.applicability.status, APPLICABILITY_APPLICABLE);
    assert!(report.applicability.p_ge_m);
    assert!(report.selection.cv_valid, "P>M LOO CV must be valid");
    assert_eq!(report.selection.method, SELECTION_METHOD_CV);
    assert_eq!(report.candidates.len(), 2 * 4);
    assert_eq!(report.selection.selected_whitelist, WhitelistKind::Full);
    assert!(report.selection.selected_lambda >= 0.0);
    assert_close(
        report.selection.selected_lambda_used,
        report.selection.selected_lambda * report.lambda_scale,
        1.0e-15,
        "lambda_used = lambda * lambda_scale",
    );
    let candidate = report
        .candidates
        .iter()
        .find(|candidate| {
            candidate.whitelist == report.selection.selected_whitelist
                && candidate.lambda == report.selection.selected_lambda
        })
        .expect("selected candidate must be reported");
    assert_eq!(candidate.folds.len(), 4);
    let mut held_out: Vec<String> = Vec::new();
    for fold in &candidate.folds {
        assert_eq!(fold.training_positions, 3);
        assert_eq!(fold.held_out_labels.len(), 1);
        assert!(fold.usable, "fold must be usable: {fold:?}");
        assert!(!fold.underdetermined, "fold must be identifiable: {fold:?}");
        assert!(fold.mean_residual_db.is_finite());
        held_out.extend(fold.held_out_labels.iter().cloned());
    }
    held_out.sort();
    held_out.dedup();
    assert_eq!(held_out.len(), 4, "every design position held out once");
    assert!(candidate.cv_mean_db.is_some());
    assert!(candidate.cv_rank.is_some());
    assert!(candidate.applicability.fir_rank_necessary);
    assert_eq!(
        candidate.applicability.per_frequency_rank_bound, 4,
        "min(P=4, free_paths=9) = 4"
    );
    assert!(!candidate.applicability.per_frequency_full_rank_necessary);
    let solution = outcome.solution.as_ref().expect("solution");
    assert_eq!(solution.coeffs_f64.len(), 3 * 3 * 1);
    assert_eq!(solution.coeffs_f32.len(), solution.coeffs_f64.len());
    assert_eq!(solution.modal.cells().len(), 9);
    for (index, value) in solution.coeffs_f64.iter().enumerate() {
        assert_eq!(solution.coeffs_f32[index], *value as f32);
    }
    assert_eq!(solution.hash.len(), 64);
    assert_eq!(solution.coeffs_sha256.len(), 64);
    assert!(solution.base_solve_hash.is_some());
    // 选中的全 5x5 解应接近已知 G_target（CV 选择合理性的闭式对照）。
    let mut worst = 0.0f64;
    for out_dense in 0..3usize {
        for in_dense in 0..3usize {
            let expected = gt[out_dense * 3 + in_dense];
            let points = solution
                .response(out_dense as u16, in_dense as u16)
                .unwrap();
            for point in points {
                worst = worst.max((point.re - expected).abs().max(point.im.abs()));
            }
        }
    }
    assert!(
        worst < 1.0e-2,
        "selected full-whitelist solution must recover known G_target, worst={worst}"
    );
    assert!(!report.holdout_used_for_tuning);
    assert!(report.cv_uses_only_design_positions);
    assert!(!report.applied);
    assert_eq!(report.post_apply_measurement, "not_requested");
    assert_eq!(report.engine_compatibility, "not_validated");
    assert_eq!(report.runtime_validation, "not_run");
    assert!(report.deliverable);
}

#[test]
fn t52_regularize_cv_folds_balance_and_explicit_k() {
    let grid = bass_grid();
    let gt = gt_matrix(3, 0.2);
    let problem = make_problem(3, 7, &grid, &gt);
    let mut options = base_options(1);
    options.whitelists = vec![WhitelistKind::DiagOnly];
    options.lambda_grid = vec![0.0, 1.0e-2];
    options.cv_folds = 3;
    let outcome = regularize(&problem, &options).unwrap();
    let report = &outcome.report;
    assert!(report.selection.cv_valid);
    let candidate = &report.candidates[0];
    assert_eq!(candidate.folds.len(), 3);
    let mut held_sizes: Vec<usize> = candidate
        .folds
        .iter()
        .map(|fold| fold.held_out_labels.len())
        .collect();
    held_sizes.sort();
    assert_eq!(
        held_sizes,
        vec![2, 2, 3],
        "7 positions round-robin into 3 folds"
    );
    let mut seen: Vec<String> = Vec::new();
    for fold in &candidate.folds {
        assert_eq!(
            fold.training_positions + fold.held_out_labels.len(),
            7,
            "held-out + training must partition the design positions"
        );
        seen.extend(fold.held_out_labels.iter().cloned());
    }
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 7);
    let mut bad = base_options(1);
    bad.cv_folds = 1;
    expect_kind(regularize(&problem, &bad), RegularizeErrorKind::Fold);
    let mut too_many = base_options(1);
    too_many.cv_folds = 8;
    expect_kind(regularize(&problem, &too_many), RegularizeErrorKind::Fold);
}

// ===========================================================================
// 3. lambda 扫描 / L-curve
// ===========================================================================

#[test]
fn t52_regularize_lambda_scan_and_l_curve() {
    let grid = bass_grid();
    let gt = gt_matrix(3, 0.2);
    let problem = make_problem(3, 4, &grid, &gt);
    let mut options = base_options(1);
    options.whitelists = vec![WhitelistKind::Full];
    options.lambda_grid = vec![1.0e-2, 1.0e-1, 1.0, 10.0, 100.0];
    let outcome = regularize(&problem, &options).unwrap();
    let report = &outcome.report;
    let scan = &report.lambda_scan;
    assert_eq!(scan.points.len(), options.lambda_grid.len());
    assert_close(scan.lambda_scale, report.lambda_scale, 1.0e-15, "scale");
    let mut previous_solution_norm = f64::INFINITY;
    let mut previous_residual = f64::NEG_INFINITY;
    for (index, point) in scan.points.iter().enumerate() {
        assert_eq!(point.lambda, options.lambda_grid[index]);
        assert_close(
            point.lambda_used,
            point.lambda * scan.lambda_scale,
            1.0e-15,
            "lambda_used",
        );
        assert!(point.solution_norm.is_finite());
        assert!(point.design_residual_norm.is_finite());
        assert!(
            point.solution_norm <= previous_solution_norm * (1.0 + 1.0e-9) + 1.0e-12,
            "solution norm must be non-increasing along the lambda path"
        );
        assert!(
            point.design_residual_norm >= previous_residual * (1.0 - 1.0e-9) - 1.0e-12,
            "design residual must be non-decreasing along the lambda path"
        );
        previous_solution_norm = point.solution_norm;
        previous_residual = point.design_residual_norm;
    }
    assert_eq!(scan.points[0].curvature, None);
    assert_eq!(scan.points[scan.points.len() - 1].curvature, None);
    assert!(
        scan.points.iter().any(|point| point.curvature.is_some()),
        "interior L-curve curvature must be computed"
    );
    assert!(scan.l_curve_corner_index.is_some());
    let corner = scan.l_curve_corner_index.unwrap();
    assert!(corner > 0 && corner + 1 < scan.points.len());
    assert_eq!(scan.l_curve_corner_lambda, Some(scan.points[corner].lambda));
    assert!(scan.l_curve_corner_lambda.unwrap() > 0.0);
    assert!(scan.note.contains("diagnostic"));
    assert!(report.selection.cv_valid);
    assert_eq!(report.selection.method, SELECTION_METHOD_CV);
    assert_eq!(
        report.selection.l_curve_corner_lambda,
        scan.l_curve_corner_lambda
    );
}

#[test]
fn t52_regularize_l_curve_unavailable_single_lambda() {
    let grid = bass_grid();
    let gt = gt_matrix(3, 0.2);
    let problem = make_problem(3, 4, &grid, &gt);
    let mut options = base_options(1);
    options.whitelists = vec![WhitelistKind::Full];
    options.lambda_grid = vec![0.0];
    let outcome = regularize(&problem, &options).unwrap();
    assert_eq!(outcome.report.lambda_scan.points.len(), 1);
    assert_eq!(outcome.report.lambda_scan.l_curve_corner_index, None);
    assert!(warning_codes(&outcome).contains(&RegularizeWarning::L_CURVE_UNAVAILABLE));
    assert!(outcome.report.selection.cv_valid);
}

#[test]
fn t52_regularize_lambda_scale_normalization() {
    let grid = bass_grid();
    let gt = gt_matrix(3, 0.2);
    let problem = make_problem_with_scale(3, 4, &grid, &gt, 1.0);
    let problem_loud = make_problem_with_scale(3, 4, &grid, &gt, 10.0);
    let mut options = base_options(1);
    options.whitelists = vec![WhitelistKind::Full];
    options.lambda_grid = vec![1.0e-3, 1.0e-2];
    let quiet = regularize(&problem, &options).unwrap();
    let loud = regularize(&problem_loud, &options).unwrap();
    let ratio = loud.report.lambda_scale / quiet.report.lambda_scale;
    assert_close(ratio, 100.0, 1.0e-6, "H x10 must scale mean|H|^2 by 100");
    for point in &loud.report.lambda_scan.points {
        assert_close(
            point.lambda_used,
            point.lambda * loud.report.lambda_scale,
            1.0e-15,
            "lambda scaling is plant-scale normalized",
        );
    }
    // 超界 lambda_used 拒收（不是静默截断）。
    let mut over = base_options(1);
    over.whitelists = vec![WhitelistKind::Full];
    over.lambda_grid = vec![10.0];
    expect_kind(
        regularize(&problem_loud, &over),
        RegularizeErrorKind::Lambda,
    );
}

// ===========================================================================
// 4. gain headroom（PR-3）
// ===========================================================================

#[test]
fn t52_regularize_headroom_fallback_to_simple_whitelist() {
    let grid = bass_grid();
    let hot = gt_matrix(3, 0.9);
    let problem = make_problem(3, 4, &grid, &hot);
    let mut options = base_options(1);
    options.headroom = HeadroomSpec::default();
    let outcome = regularize(&problem, &options).unwrap();
    let report = &outcome.report;
    assert!(
        report.selection.headroom_fallback_attempts > 0,
        "hot off-diagonal fixture must trigger bounded headroom fallback: {report:?}"
    );
    assert!(warning_codes(&outcome).contains(&RegularizeWarning::HEADROOM_FALLBACK));
    assert_eq!(
        report.selection.selected_whitelist,
        WhitelistKind::DiagOnly,
        "diagonal-only candidate has zero off-diagonal gain and must pass PR-3 headroom"
    );
    assert!(report.headroom.passed);
    assert_eq!(report.headroom.worst_excess_db, 0.0);
    assert!(report.deliverable);
    let offdiag = solution_offdiag_max_db(&outcome);
    assert!(
        offdiag < -200.0,
        "diagonal-only solution must have zero off-diagonal response, got {offdiag} dB"
    );
    assert_eq!(report.headroom.correction_cap_db, -6.0);
    assert_eq!(report.headroom.tolerance_db, 2.0);
    assert_eq!(
        report.headroom.band_hz,
        tspi_modal_design::target::IMPROVEMENT_BAND_HZ
    );
}

#[test]
fn t52_regularize_headroom_violation_best_effort() {
    let grid = bass_grid();
    let hot = gt_matrix(3, 0.9);
    let problem = make_problem(3, 4, &grid, &hot);
    let mut options = base_options(1);
    options.whitelists = vec![WhitelistKind::Full];
    options.headroom = HeadroomSpec {
        correction_cap_db: CORRECTION_GAIN_CAP_MIN_DB,
        diagonal_cap_db: 6.0,
        tolerance_db: 2.0,
    };
    let outcome = regularize(&problem, &options).unwrap();
    let report = &outcome.report;
    assert!(!report.headroom.passed, "declared cap must be violated");
    assert!(report.headroom.worst_excess_db > 0.0);
    assert!(
        !report.deliverable,
        "violating headroom must not be deliverable"
    );
    assert!(warning_codes(&outcome).contains(&RegularizeWarning::HEADROOM_VIOLATION));
    assert!(
        outcome.solution.is_some(),
        "best-effort solution is returned"
    );
    let mut relaxed = options.clone();
    relaxed.headroom.correction_cap_db = CORRECTION_GAIN_CAP_MAX_DB;
    let relaxed_outcome = regularize(&problem, &relaxed).unwrap();
    assert!(relaxed_outcome.report.headroom.passed);
    assert!(relaxed_outcome.report.deliverable);
}

#[test]
fn t52_regularize_headroom_path_table_math() {
    let grid = bass_grid();
    let gt = gt_matrix(3, 0.2);
    let problem = make_problem(3, 4, &grid, &gt);
    let mut options = base_options(1);
    options.whitelists = vec![WhitelistKind::Full];
    options.lambda_grid = vec![0.0];
    let outcome = regularize(&problem, &options).unwrap();
    let report = &outcome.report;
    let headroom = &report.headroom;
    assert_eq!(headroom.paths.len(), 9);
    let mut max_correction = f64::NEG_INFINITY;
    let mut max_diagonal = f64::NEG_INFINITY;
    for path in &headroom.paths {
        let expected_cap = if path.is_correction {
            headroom.correction_cap_db
        } else {
            headroom.diagonal_cap_db
        };
        assert_eq!(path.cap_db, expected_cap);
        assert_eq!(
            path.margin_db,
            expected_cap + headroom.tolerance_db - path.max_gain_db
        );
        assert_eq!(path.passed, path.margin_db >= 0.0);
        assert!((path.out_dense as usize) < 3 && (path.in_dense as usize) < 3);
        if path.is_correction {
            max_correction = max_correction.max(path.max_gain_db);
        } else {
            max_diagonal = max_diagonal.max(path.max_gain_db);
        }
    }
    assert_close(
        headroom.max_correction_gain_db,
        max_correction,
        1.0e-12,
        "max correction gain",
    );
    assert_close(
        headroom.max_diagonal_gain_db,
        max_diagonal,
        1.0e-12,
        "max diagonal gain",
    );
    assert_close(max_diagonal, 0.0, 1.0e-2, "diagonal gain ~0 dB");
    assert_close(max_correction, 20.0 * 0.2f64.log10(), 0.05, "offdiag 0.2");
    assert!(headroom.note.contains("PR-3"));
}

// ===========================================================================
// 5. 先验（零修正 / 冻结系数）
// ===========================================================================

#[test]
fn t52_regularize_prior_frozen_shrinks_toward_prior() {
    let grid = bass_grid();
    let gt = gt_matrix(3, 0.2);
    let problem = make_problem(3, 4, &grid, &gt);
    let mut prior_coeffs = vec![0.0f64; 3 * 3 * 1];
    for dense in 0..3 {
        prior_coeffs[dense * 3 + dense] = 0.3;
    }
    let prior = RegularizationPrior::frozen(
        problem.roles.clone(),
        problem.grid_hz.clone(),
        1,
        prior_coeffs,
    )
    .unwrap();
    assert_eq!(prior.kind(), PriorKind::FrozenCoefficients);
    assert_eq!(prior.hash().len(), 64);
    let mut zero_options = base_options(1);
    zero_options.whitelists = vec![WhitelistKind::Full];
    zero_options.lambda_grid = vec![0.0];
    let zero_outcome = regularize(&problem, &zero_options).unwrap();
    let mut frozen_options = zero_options.clone();
    frozen_options.prior = prior.clone();
    frozen_options.lambda_grid = vec![0.0, 100.0];
    let frozen_outcome = regularize(&problem, &frozen_options).unwrap();
    assert_eq!(
        frozen_outcome.report.prior_kind,
        PriorKind::FrozenCoefficients.as_str()
    );
    assert!(warning_codes(&frozen_outcome).contains(&RegularizeWarning::PRIOR_FROZEN));
    let distance = |solution: &tspi_modal_design::regularize::RegularizedSolution| {
        let mut distance = 0.0f64;
        for (index, value) in solution.coeffs_f64.iter().enumerate() {
            distance += (value - prior.coeffs()[index]).powi(2);
        }
        distance.sqrt()
    };
    let distance_lambda0 = distance(frozen_outcome.solution.as_ref().unwrap());
    let mut stronger = frozen_options.clone();
    stronger.lambda_grid = vec![100.0];
    let stronger_outcome = regularize(&problem, &stronger).unwrap();
    let distance_stronger = distance(stronger_outcome.solution.as_ref().unwrap());
    assert!(
        distance_stronger < distance_lambda0,
        "larger lambda must shrink the solution toward the frozen prior: {distance_stronger} vs {distance_lambda0}"
    );
    let mut zero_strong = zero_options.clone();
    zero_strong.lambda_grid = vec![100.0];
    let zero_strong_outcome = regularize(&problem, &zero_strong).unwrap();
    let zero_distance = distance(zero_strong_outcome.solution.as_ref().unwrap());
    assert!(
        distance_stronger < zero_distance,
        "frozen prior must bias the solution toward g_prior (frozen {distance_stronger} vs zero {zero_distance})"
    );
    // 冻结先验不影响 lambda=0 的纯最小二乘解（目标变换是等价的）。
    let mut lambda0 = frozen_options.clone();
    lambda0.lambda_grid = vec![0.0];
    let frozen_lambda0 = regularize(&problem, &lambda0).unwrap();
    let plain = zero_outcome.solution.as_ref().unwrap();
    let with_prior = frozen_lambda0.solution.as_ref().unwrap();
    let mut worst = 0.0f64;
    for (left, right) in plain.coeffs_f64.iter().zip(with_prior.coeffs_f64.iter()) {
        worst = worst.max((left - right).abs());
    }
    assert!(
        worst < 1.0e-6,
        "lambda=0 with frozen prior must reproduce plain least squares, worst={worst}"
    );
}

#[test]
fn t52_regularize_prior_from_solution_roundtrip() {
    let grid = bass_grid();
    let gt = gt_matrix(3, 0.2);
    let problem = make_problem(3, 4, &grid, &gt);
    let mut solve_options = SolveOptions::default();
    solve_options.taps = 1;
    solve_options.max_iterations = 64;
    solve_options.rel_tolerance = 1.0e-8;
    let solved = solve(&problem, &solve_options).unwrap();
    let base_solution = solved.solution.as_ref().expect("T51 solution");
    let prior = RegularizationPrior::from_solution(base_solution, &problem).unwrap();
    assert_eq!(prior.kind(), PriorKind::FrozenCoefficients);
    assert_eq!(prior.taps(), 1);
    assert_eq!(prior.roles().len(), 3);
    assert_eq!(prior.hash().len(), 64);
    let mut options = base_options(1);
    options.whitelists = vec![WhitelistKind::Full];
    options.lambda_grid = vec![0.0];
    options.prior = prior.clone();
    let outcome = regularize(&problem, &options).unwrap();
    let regularized = outcome.solution.as_ref().unwrap();
    let prior_again = RegularizationPrior::from_regularized(regularized, &problem).unwrap();
    assert_eq!(prior_again.hash().len(), 64);
    let solution = outcome.solution.as_ref().unwrap();
    let mut worst = 0.0f64;
    for (index, value) in solution.coeffs_f64.iter().enumerate() {
        worst = worst.max((value - prior.coeffs()[index]).abs());
    }
    assert!(
        worst < 1.0e-5,
        "prior equal to upstream solution must keep the solution at the prior, worst={worst}"
    );
}

#[test]
fn t52_regularize_prior_rejected_in_preserve() {
    let grid = bass_grid();
    let problem = make_preserve_problem(3, 4, &grid);
    let mut options = base_options(1);
    options.prior = RegularizationPrior::frozen(
        problem.roles.clone(),
        problem.grid_hz.clone(),
        1,
        vec![0.0; 3 * 3 * 1],
    )
    .unwrap();
    let error = expect_kind(regularize(&problem, &options), RegularizeErrorKind::Prior);
    assert_eq!(error.code(), "E_REG_PRIOR");
}

// ===========================================================================
// 6. P>=M 适用边界
// ===========================================================================

#[test]
fn t52_regularize_p_below_m_boundary() {
    let grid = bass_grid();
    let gt = gt_matrix(5, 0.2);
    let problem = make_problem(5, 3, &grid, &gt);
    let mut options = base_options(1);
    options.lambda_grid = vec![0.0, 1.0e-2];
    let outcome = regularize(&problem, &options).unwrap();
    let report = &outcome.report;
    assert_eq!(report.applicability.status, APPLICABILITY_NOT_APPLICABLE);
    assert!(!report.applicability.p_ge_m);
    assert!(report.applicability.cv_requires_p_gt_m);
    assert!(!report.selection.cv_valid);
    assert_eq!(report.selection.cv_reason, Some("p_below_m"));
    assert_eq!(report.selection.method, SELECTION_METHOD_L_CURVE);
    assert!(!report.deliverable, "P<M must not be deliverable");
    assert!(warning_codes(&outcome).contains(&RegularizeWarning::UNDERDETERMINED));
    assert!(warning_codes(&outcome).contains(&RegularizeWarning::CV_INVALID));
    assert!(!status_usable(&report.final_solve_status));
    assert_eq!(report.final_solve_status, "underdetermined");
    assert!(outcome.solution.is_some());
}

#[test]
fn t52_regularize_p_equals_m_boundary() {
    let grid = bass_grid();
    let gt = gt_matrix(3, 0.2);
    let problem = make_problem(3, 3, &grid, &gt);
    let mut options = base_options(1);
    options.lambda_grid = vec![0.0, 1.0e-2];
    let outcome = regularize(&problem, &options).unwrap();
    let report = &outcome.report;
    assert_eq!(report.applicability.status, APPLICABILITY_BOUNDARY);
    assert!(report.applicability.p_ge_m);
    assert!(
        !report.selection.cv_valid,
        "P==M LOO folds are underdetermined"
    );
    assert_eq!(report.selection.cv_reason, Some("p_equals_m_boundary"));
    assert_eq!(report.selection.method, SELECTION_METHOD_L_CURVE);
    assert!(warning_codes(&outcome).contains(&RegularizeWarning::CV_BOUNDARY));
    assert!(status_usable(&report.final_solve_status));
    assert!(report.deliverable);
    assert!(outcome.report.selection.fallback_used);
}

fn status_usable(status: &str) -> bool {
    status == SolveStatus::Converged.as_str() || status == SolveStatus::PreserveBaseline.as_str()
}

// ===========================================================================
// 7. holdout 不调参
// ===========================================================================

#[test]
fn t52_regularize_holdout_labels_rejected() {
    let grid = bass_grid();
    let gt = gt_matrix(3, 0.2);
    let problem = make_problem(3, 4, &grid, &gt);
    let mut options = base_options(1);
    options.forbidden_tuning_labels = vec!["P01".to_string()];
    let error = expect_kind(regularize(&problem, &options), RegularizeErrorKind::Holdout);
    assert_eq!(error.code(), "E_REG_HOLDOUT");
    let mut bad = base_options(1);
    bad.forbidden_tuning_labels = vec!["-bad".to_string()];
    expect_kind(regularize(&problem, &bad), RegularizeErrorKind::Holdout);
    let mut ok = base_options(1);
    ok.whitelists = vec![WhitelistKind::DiagOnly];
    ok.lambda_grid = vec![0.0];
    ok.forbidden_tuning_labels = vec!["V01".to_string(), "V02".to_string()];
    let outcome = regularize(&problem, &ok).unwrap();
    assert_eq!(
        outcome.report.forbidden_tuning_labels,
        vec!["V01".to_string(), "V02".to_string()]
    );
    assert!(!outcome.report.holdout_used_for_tuning);
    assert!(outcome.report.cv_uses_only_design_positions);
}

// ===========================================================================
// 8. Preserve 基线
// ===========================================================================

#[test]
fn t52_regularize_preserve_baseline() {
    let grid = bass_grid();
    let problem = make_preserve_problem(3, 4, &grid);
    let options = base_options(1);
    let outcome = regularize(&problem, &options).unwrap();
    let report = &outcome.report;
    assert_eq!(report.mode, "preserve");
    assert_eq!(report.selection.method, SELECTION_METHOD_PRESERVE);
    assert!(!report.selection.cv_valid);
    assert!(report.candidates.is_empty());
    assert!(report.lambda_scan.points.is_empty());
    assert!(warning_codes(&outcome).contains(&RegularizeWarning::PRESERVE_NO_TUNING));
    assert_eq!(report.final_solve_status, "preserve_baseline");
    let solution = outcome.solution.as_ref().unwrap();
    assert!(
        solution.coeffs_f64.iter().all(|value| *value == 0.0),
        "preserve baseline carries no modal correction"
    );
    for out_dense in 0..3u16 {
        for in_dense in 0..3u16 {
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
    assert!(report.headroom.passed);
    assert!(report.deliverable);
}

// ===========================================================================
// 9. T50 桥接：from_target_plant（含 holdout label 自动加入、validate_solution）
// ===========================================================================

#[test]
fn t52_regularize_from_target_plant_aligns_with_t50() {
    let grid = bass_grid();
    let participants = ModalParticipants::p1_4_1_2();
    let baseline = make_baseline(&grid, participants.clone());
    let mut positions = Vec::new();
    for position in 0..6usize {
        positions.push(raw_position(
            &format!("P{:02}", position + 1),
            PositionRole::Design,
            &participants,
            &grid,
            position,
        ));
    }
    for position in 0..3usize {
        positions.push(raw_position(
            &format!("V{:02}", position + 1),
            PositionRole::Validation,
            &participants,
            &grid,
            position,
        ));
    }
    let plant = make_plant(&baseline, &participants, &grid, positions);
    let target = build_reduced_target(&baseline, &plant);
    assert!(target.verify_holdout_isolated());
    assert_eq!(plant.validation_labels().len(), 3);
    let mut options = base_options(1);
    options.whitelists = vec![WhitelistKind::DiagOnly, WhitelistKind::Full];
    options.lambda_grid = vec![0.0, 1.0e-2];
    let outcome = regularize_from_target_plant(&target, &plant, &options).unwrap();
    let report = &outcome.report;
    assert_eq!(report.mode, "reduced");
    assert_eq!(report.m, 5);
    assert_eq!(report.design_positions, 6);
    assert_eq!(report.applicability.status, APPLICABILITY_APPLICABLE);
    assert!(report.selection.cv_valid, "P=6>M=5 LOO CV must be valid");
    assert_eq!(report.selection.method, SELECTION_METHOD_CV);
    for label in ["V01", "V02", "V03"] {
        assert!(
            report
                .forbidden_tuning_labels
                .iter()
                .any(|item| item == label),
            "validation label {label} must be forbidden for tuning"
        );
    }
    for candidate in &report.candidates {
        for fold in &candidate.folds {
            for label in &fold.held_out_labels {
                assert!(
                    label.starts_with('P'),
                    "CV must hold out design positions only, got {label}"
                );
            }
        }
    }
    let validation = report
        .solution_validation
        .as_ref()
        .expect("from_target_plant must attach validate_solution summary");
    assert!(
        validation.passed,
        "final G must pass T50 validate_solution: {validation:?}"
    );
    assert_eq!(validation.profile, "multichannel_4_1_2_5");
    assert!(validation.residual_max_db.unwrap() < 12.0);
    let solution = outcome.solution.as_ref().unwrap();
    let direct = validate_solution(&target, &plant, &solution.modal).unwrap();
    assert!(direct.passed);
    assert_eq!(report.selection.selected_whitelist, WhitelistKind::Full);
    assert!(report.deliverable);
    assert_eq!(solution.hash.len(), 64);
    assert_eq!(solution.coeffs_sha256.len(), 64);
    assert_eq!(report.prior_hash.as_ref().unwrap().len(), 64);
}

// ===========================================================================
// 10. 选项/先验负向拒收
// ===========================================================================

#[test]
fn t52_regularize_options_rejected() {
    let grid = bass_grid();
    let gt = gt_matrix(3, 0.2);
    let problem = make_problem(3, 4, &grid, &gt);
    let expect = |options: RegularizeOptions, kind: RegularizeErrorKind| {
        expect_kind(regularize(&problem, &options), kind);
    };
    let mut options = base_options(1);
    options.base.ridge = 1.0e-3;
    expect(options, RegularizeErrorKind::Options);
    let mut options = base_options(1);
    options.base.path_mask = Some(vec![true; 9]);
    expect(options, RegularizeErrorKind::Options);
    let mut options = base_options(1);
    options.lambda_grid = Vec::new();
    expect(options, RegularizeErrorKind::Lambda);
    let mut options = base_options(1);
    options.lambda_grid = vec![1.0e-2, 0.0];
    expect(options, RegularizeErrorKind::Lambda);
    let mut options = base_options(1);
    options.lambda_grid = vec![-1.0e-3];
    expect(options, RegularizeErrorKind::Lambda);
    let mut options = base_options(1);
    options.lambda_grid = (0..17).map(|index| index as f64 * 1.0e-3).collect();
    expect(options, RegularizeErrorKind::Lambda);
    let mut options = base_options(1);
    options.whitelists = Vec::new();
    expect(options, RegularizeErrorKind::Whitelist);
    let mut options = base_options(1);
    options.whitelists = vec![WhitelistKind::Full, WhitelistKind::Full];
    expect(options, RegularizeErrorKind::Whitelist);
    let mut options = base_options(1);
    options.selection_tolerance_db = 100.0;
    expect(options, RegularizeErrorKind::Options);
    let mut options = base_options(1);
    options.headroom.correction_cap_db = -30.0;
    expect(options, RegularizeErrorKind::Headroom);
    let mut options = base_options(1);
    options.headroom.diagonal_cap_db = -1.0;
    expect(options, RegularizeErrorKind::Headroom);
    let mut options = base_options(1);
    options.headroom.tolerance_db = 10.0;
    expect(options, RegularizeErrorKind::Headroom);
    let mut options = base_options(1);
    options.prior = RegularizationPrior::frozen(
        problem.roles.clone(),
        problem.grid_hz.clone(),
        8,
        vec![0.0; 3 * 3 * 8],
    )
    .unwrap();
    expect(options, RegularizeErrorKind::Prior);
    let mut options = base_options(1);
    options.prior = RegularizationPrior::frozen(
        vec!["FL".to_string(), "FR".to_string()],
        problem.grid_hz.clone(),
        4,
        vec![0.0; 2 * 2 * 4],
    )
    .unwrap();
    expect(options, RegularizeErrorKind::Prior);
    expect_kind(
        RegularizationPrior::frozen(
            problem.roles.clone(),
            problem.grid_hz.clone(),
            4,
            vec![f64::NAN; 3 * 3 * 4],
        ),
        RegularizeErrorKind::Range,
    );
    expect_kind(
        RegularizationPrior::frozen(
            problem.roles.clone(),
            problem.grid_hz.clone(),
            4,
            vec![0.0; 3],
        ),
        RegularizeErrorKind::Prior,
    );
    let mut options = base_options(1);
    options.forbidden_tuning_labels = vec!["P02".to_string()];
    expect(options, RegularizeErrorKind::Holdout);
}

// ===========================================================================
// 11. 确定性 / 证据 JSON / 零 plant
// ===========================================================================

#[test]
fn t52_regularize_deterministic_hashes() {
    let grid = bass_grid();
    let gt = gt_matrix(3, 0.2);
    let problem = make_problem(3, 4, &grid, &gt);
    let options = base_options(1);
    let first = regularize(&problem, &options).unwrap();
    let second = regularize(&problem, &options).unwrap();
    assert_eq!(first.to_evidence_json(), second.to_evidence_json());
    assert_eq!(
        first.solution.as_ref().unwrap().hash,
        second.solution.as_ref().unwrap().hash
    );
    assert_eq!(
        first.solution.as_ref().unwrap().coeffs_sha256,
        second.solution.as_ref().unwrap().coeffs_sha256
    );
    let mut changed = options.clone();
    changed.lambda_grid = vec![0.0, 1.0e-2, 1.0];
    let third = regularize(&problem, &changed).unwrap();
    assert_ne!(first.to_evidence_json(), third.to_evidence_json());
    assert_eq!(third.report.lambda_scan.points.len(), 3);
    let prior = RegularizationPrior::frozen(
        problem.roles.clone(),
        problem.grid_hz.clone(),
        1,
        vec![0.01; 3 * 3 * 1],
    )
    .unwrap();
    let mut with_prior = options.clone();
    with_prior.prior = prior;
    let fourth = regularize(&problem, &with_prior).unwrap();
    assert_ne!(
        first.solution.as_ref().unwrap().hash,
        fourth.solution.as_ref().unwrap().hash
    );
    assert_eq!(fourth.report.prior_kind, "frozen_coefficients");
}

#[test]
fn t52_regularize_evidence_json_contract() {
    let grid = bass_grid();
    let gt = gt_matrix(3, 0.2);
    let problem = make_problem(3, 4, &grid, &gt);
    let options = base_options(1);
    let outcome = regularize(&problem, &options).unwrap();
    let json = outcome.to_evidence_json();
    JsonSyntax::validate(&json).expect("evidence JSON must be syntactically valid");
    assert!(json.contains("\"schema\":\"tspi-modal-regularize/v1\""));
    assert!(json.contains("\"mode\":\"custom\""));
    assert!(json.contains("\"holdout_used_for_tuning\":false"));
    assert!(json.contains("\"cv_uses_only_design_positions\":true"));
    assert!(json.contains("\"applied\":false"));
    assert!(json.contains("\"post_apply_measurement\":\"not_requested\""));
    assert!(json.contains("\"engine_compatibility\":\"not_validated\""));
    assert!(json.contains("\"runtime_validation\":\"not_run\""));
    assert!(json.contains("\"deliverable\":true"));
    assert!(json.contains("\"method\":\"cv\""));
    assert!(json.contains("\"status\":\"applicable\""));
    assert!(json.contains("\"l_curve_corner_index\":"));
    let candidate_count = json.matches("\"whitelist\":").count();
    assert_eq!(candidate_count, 2 * 4);
    let fold_count = json.matches("\"fold_index\":").count();
    assert_eq!(fold_count, 2 * 4 * 4);
    let path_count = json.matches("\"is_correction\":").count();
    assert_eq!(path_count, 9);
    let lambda_points = json.matches("\"design_residual_norm\":").count();
    assert_eq!(lambda_points, 4);
    assert!(json.contains("no guarantee that all standing waves are eliminated"));
    assert!(json.contains("\"prior\":{\"kind\":\"zero_correction\""));
}

#[test]
fn t52_regularize_zero_plant_degenerate() {
    let grid = bass_grid();
    let gt = gt_matrix(3, 0.2);
    let problem = make_problem_with_scale(3, 4, &grid, &gt, 0.0);
    let options = base_options(1);
    let outcome = regularize(&problem, &options).unwrap();
    let report = &outcome.report;
    assert_eq!(
        report.lambda_scale, 1.0,
        "degenerate scale falls back to 1.0"
    );
    assert!(warning_codes(&outcome).contains(&RegularizeWarning::DEGENERATE_PLANT));
    assert!(!report.selection.cv_valid);
    assert!(!report.deliverable);
    assert_eq!(report.final_solve_status, "underdetermined");
    assert!(outcome.solution.is_some(), "best-effort solution returned");
}

// ===========================================================================
// 12. 静态守卫 / 错误码
// ===========================================================================

#[test]
fn t52_regularize_static_guards() {
    let source = tspi_modal_design::regularize::regularize_source_text_for_guards();
    for required in [
        "tspi-modal-regularize/v1",
        "WhitelistKind",
        "DiagOnly",
        "DiagAdjacent",
        "DiagSub1",
        "Full",
        "design 内 CV",
        "lambda_scan",
        "l_curve_corner",
        "gain headroom",
        "P>=M",
        "not_applicable_p_below_m",
        "boundary_p_equals_m",
        "holdout",
        "FrozenCoefficients",
        "PR-3",
        "include_str!",
        "hash",
    ] {
        assert!(
            source.contains(required),
            "regularize.rs must document/implement {required}"
        );
    }
    assert_eq!(
        source.matches("保证消除全部驻波").count(),
        source.matches("不保证消除全部驻波").count(),
        "regularize.rs may only mention standing-wave elimination in negated form"
    );
    assert_eq!(
        source.matches("承诺消除全部驻波").count(),
        source.matches("不承诺消除全部驻波").count(),
        "regularize.rs may only mention standing-wave elimination in negated form"
    );
    assert!(!source.contains("消除所有驻波"));
    for forbidden in [
        "std::fs",
        "std::process",
        "std::net",
        "std::thread",
        "std::env",
        "std::io",
        "unsafe",
        "serde",
        "File::",
        "Command::",
        "TcpStream",
        "thread::spawn",
    ] {
        assert!(
            !source.contains(forbidden),
            "regularize.rs must stay pure computation; found {forbidden}"
        );
    }
    assert!(source.contains("固定白名单候选子集"));
}

#[test]
fn t52_regularize_error_codes_stable() {
    let codes: Vec<&'static str> = RegularizeErrorKind::ALL
        .iter()
        .map(|kind| kind.code())
        .collect();
    assert_eq!(codes.len(), 11);
    let unique: std::collections::BTreeSet<&str> = codes.iter().copied().collect();
    assert_eq!(unique.len(), codes.len());
    for code in &codes {
        assert!(code.starts_with("E_REG_"), "bad code {code}");
    }
    let error = RegularizeError::new(
        RegularizeErrorKind::Holdout,
        "regularize.options.forbidden_tuning_labels",
        "demo",
    );
    assert_eq!(error.kind(), RegularizeErrorKind::Holdout);
    assert_eq!(error.code(), "E_REG_HOLDOUT");
    assert_eq!(error.field(), "regularize.options.forbidden_tuning_labels");
    assert_eq!(error.detail(), "demo");
    assert!(error.to_string().contains("E_REG_HOLDOUT"));
    assert_eq!(LAMBDA_GRID.len(), 8);
    assert_eq!(LAMBDA_GRID[0], 0.0);
    assert!(LAMBDA_GRID.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(MAX_LAMBDA, 100.0);
    assert_eq!(CORRECTION_GAIN_CAP_MIN_DB, -24.0);
    assert_eq!(CORRECTION_GAIN_CAP_MAX_DB, 6.0);
    assert_eq!(PriorKind::ZeroCorrection.as_str(), "zero_correction");
    assert_eq!(
        PriorKind::FrozenCoefficients.as_str(),
        "frozen_coefficients"
    );
    assert_eq!(
        RegularizeOptions::default().lambda_grid,
        LAMBDA_GRID.to_vec()
    );
    assert_eq!(
        RegularizeOptions::default().whitelists,
        WhitelistKind::ALL.to_vec()
    );
    assert!(RegularizeOptions::default().prior.is_zero());
    assert_eq!(RegularizeOptions::default().cv_folds, 0);
    assert_eq!(RegularizeOptions::default().base.ridge, 0.0);
    assert!(RegularizeOptions::default().base.path_mask.is_none());
}

// ===========================================================================
// 13. 已知夹具 dump（供 evidence/T52/check_regularize_fixture.py 独立复算）
// ===========================================================================

fn json_number(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.17e}")
    } else {
        "null".to_string()
    }
}

fn json_bool(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

#[test]
fn t52_regularize_known_fixture_dump() {
    let grid = bass_grid();
    let gt = gt_matrix(3, 0.2);
    let problem = make_problem(3, 4, &grid, &gt);
    let mut options = base_options(1);
    options.whitelists = vec![WhitelistKind::DiagOnly, WhitelistKind::Full];
    options.lambda_grid = vec![0.0, 1.0e-4, 1.0e-2, 1.0];
    options.headroom = HeadroomSpec::default();
    let outcome = regularize(&problem, &options).unwrap();
    let report = &outcome.report;
    let solution = outcome.solution.as_ref().expect("solution");
    let m = report.m;
    let grid_len = report.grid_points;
    let mut json = String::new();
    json.push('{');
    json.push_str(&format!(
        "\"schema\":\"tspi-t52-fixture/v1\",\"m\":{m},\"taps\":{},\"fs\":{},\"grid_points\":{grid_len},",
        report.taps, report.sample_rate_hz
    ));
    json.push_str("\"grid\":[");
    for (index, freq) in grid.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&json_number(*freq));
    }
    json.push_str("],\"roles\":[");
    for (index, role) in problem.roles.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&format!("\"{role}\""));
    }
    json.push_str("],");
    json.push_str("\"h\":[");
    for (position_index, position) in problem.positions.iter().enumerate() {
        if position_index > 0 {
            json.push(',');
        }
        json.push('[');
        for (participant, points) in position.h_eff.iter().enumerate() {
            if participant > 0 {
                json.push(',');
            }
            json.push('[');
            for (index, point) in points.iter().enumerate() {
                if index > 0 {
                    json.push(',');
                }
                json.push_str(&json_number(point.re));
            }
            json.push(']');
        }
        json.push(']');
    }
    json.push_str("],");
    json.push_str("\"target\":[");
    for (position_index, position) in problem.positions.iter().enumerate() {
        if position_index > 0 {
            json.push(',');
        }
        json.push('[');
        for (input_dense, points) in position.target.iter().enumerate() {
            if input_dense > 0 {
                json.push(',');
            }
            json.push('[');
            for (index, point) in points.iter().enumerate() {
                if index > 0 {
                    json.push(',');
                }
                json.push_str(&json_number(point.re));
            }
            json.push(']');
        }
        json.push(']');
    }
    json.push_str("],");
    json.push_str("\"gt\":[");
    for (index, value) in gt.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&json_number(*value));
    }
    json.push_str("],");
    json.push_str(&format!(
        "\"options\":{{\"cv_folds\":{},\"fit_band_hz\":[{},{}],\"improvement_band_hz\":[{},{}],\"headroom\":{{\"correction_cap_db\":{},\"diagonal_cap_db\":{},\"tolerance_db\":{}}},\"lambda_grid\":[",
        options.cv_folds,
        json_number(options.base.fit_band_hz.0),
        json_number(options.base.fit_band_hz.1),
        json_number(tspi_modal_design::target::IMPROVEMENT_BAND_HZ.0),
        json_number(tspi_modal_design::target::IMPROVEMENT_BAND_HZ.1),
        json_number(options.headroom.correction_cap_db),
        json_number(options.headroom.diagonal_cap_db),
        json_number(options.headroom.tolerance_db)
    ));
    for (index, lambda) in options.lambda_grid.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&json_number(*lambda));
    }
    json.push_str("],\"whitelists\":[");
    for (index, whitelist) in options.whitelists.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&format!("\"{}\"", whitelist.as_str()));
    }
    json.push_str("]},");
    json.push_str(&format!(
        "\"applicability\":{{\"p_ge_m\":{},\"status\":\"{}\",\"design_positions\":{}}},\"lambda_scale\":{},",
        json_bool(report.applicability.p_ge_m),
        report.applicability.status,
        report.applicability.design_positions,
        json_number(report.lambda_scale)
    ));
    let selection = &report.selection;
    json.push_str(&format!(
        "\"selection\":{{\"method\":\"{}\",\"selected_whitelist\":\"{}\",\"selected_lambda\":{},\"selected_lambda_used\":{},\"cv_valid\":{},\"cv_reason\":{},\"fallback_used\":{},\"tie_break_used\":{},\"l_curve_corner_lambda\":{},\"headroom_fallback_attempts\":{}}},",
        selection.method,
        selection.selected_whitelist.as_str(),
        json_number(selection.selected_lambda),
        json_number(selection.selected_lambda_used),
        json_bool(selection.cv_valid),
        match selection.cv_reason {
            Some(reason) => format!("\"{reason}\""),
            None => "null".to_string(),
        },
        json_bool(selection.fallback_used),
        json_bool(selection.tie_break_used),
        match selection.l_curve_corner_lambda {
            Some(value) => json_number(value),
            None => "null".to_string(),
        },
        selection.headroom_fallback_attempts
    ));
    json.push_str("\"candidates\":[");
    for (candidate_index, candidate) in report.candidates.iter().enumerate() {
        if candidate_index > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            "{{\"whitelist\":\"{}\",\"free_paths\":{},\"lambda\":{},\"lambda_used\":{},\"cv_mean_db\":{},\"cv_worst_db\":{},\"cv_valid\":{},\"cv_rank\":{},\"fold_underdetermined\":{},\"folds\":[",
            candidate.whitelist.as_str(),
            candidate.free_paths,
            json_number(candidate.lambda),
            json_number(candidate.lambda_used),
            match candidate.cv_mean_db {
                Some(value) => json_number(value),
                None => "null".to_string(),
            },
            match candidate.cv_worst_db {
                Some(value) => json_number(value),
                None => "null".to_string(),
            },
            json_bool(candidate.cv_valid),
            match candidate.cv_rank {
                Some(rank) => rank.to_string(),
                None => "null".to_string(),
            },
            json_bool(candidate.fold_underdetermined)
        ));
        for (fold_index, fold) in candidate.folds.iter().enumerate() {
            if fold_index > 0 {
                json.push(',');
            }
            json.push_str(&format!(
                "{{\"fold_index\":{},\"training_positions\":{},\"held_out_labels\":[",
                fold.fold_index, fold.training_positions
            ));
            for (label_index, label) in fold.held_out_labels.iter().enumerate() {
                if label_index > 0 {
                    json.push(',');
                }
                json.push_str(&format!("\"{label}\""));
            }
            json.push_str(&format!(
                "],\"solve_status\":\"{}\",\"usable\":{},\"underdetermined\":{},\"mean_residual_db\":{},\"worst_residual_db\":{}}}",
                fold.solve_status,
                json_bool(fold.usable),
                json_bool(fold.underdetermined),
                json_number(fold.mean_residual_db),
                json_number(fold.worst_residual_db)
            ));
        }
        json.push_str("]}");
    }
    json.push_str("],");
    let scan = &report.lambda_scan;
    json.push_str(&format!(
        "\"lambda_scan\":{{\"scan_whitelist\":\"{}\",\"l_curve_corner_index\":{},\"points\":[",
        scan.scan_whitelist.as_str(),
        match scan.l_curve_corner_index {
            Some(index) => index.to_string(),
            None => "null".to_string(),
        }
    ));
    for (index, point) in scan.points.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            "{{\"lambda\":{},\"lambda_used\":{},\"design_residual_norm\":{},\"solution_norm\":{},\"fit_band_max_db\":{},\"improvement_band_max_db\":{},\"curvature\":{}}}",
            json_number(point.lambda),
            json_number(point.lambda_used),
            json_number(point.design_residual_norm),
            json_number(point.solution_norm),
            json_number(point.fit_band_max_db),
            json_number(point.improvement_band_max_db),
            match point.curvature {
                Some(value) => json_number(value),
                None => "null".to_string(),
            }
        ));
    }
    json.push_str("]},");
    let headroom = &report.headroom;
    json.push_str(&format!(
        "\"headroom\":{{\"correction_cap_db\":{},\"diagonal_cap_db\":{},\"tolerance_db\":{},\"worst_excess_db\":{},\"passed\":{},\"paths\":[",
        json_number(headroom.correction_cap_db),
        json_number(headroom.diagonal_cap_db),
        json_number(headroom.tolerance_db),
        json_number(headroom.worst_excess_db),
        json_bool(headroom.passed)
    ));
    for (index, path) in headroom.paths.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            "{{\"out_dense\":{},\"in_dense\":{},\"is_correction\":{},\"max_gain_db\":{},\"cap_db\":{},\"margin_db\":{},\"passed\":{}}}",
            path.out_dense,
            path.in_dense,
            json_bool(path.is_correction),
            json_number(path.max_gain_db),
            json_number(path.cap_db),
            json_number(path.margin_db),
            json_bool(path.passed)
        ));
    }
    json.push_str("]},");
    json.push_str("\"final_g_re\":[");
    let mut first = true;
    for out_dense in 0..m {
        for in_dense in 0..m {
            for point in solution
                .response(out_dense as u16, in_dense as u16)
                .unwrap()
            {
                if !first {
                    json.push(',');
                }
                first = false;
                json.push_str(&json_number(point.re));
            }
        }
    }
    json.push_str("],\"final_g_im\":[");
    let mut first = true;
    for out_dense in 0..m {
        for in_dense in 0..m {
            for point in solution
                .response(out_dense as u16, in_dense as u16)
                .unwrap()
            {
                if !first {
                    json.push(',');
                }
                first = false;
                json.push_str(&json_number(point.im));
            }
        }
    }
    json.push_str("],\"coeffs\":[");
    for (index, value) in solution.coeffs_f64.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&json_number(*value));
    }
    json.push_str("],");
    json.push_str(&format!(
        "\"report\":{{\"deliverable\":{},\"final_solve_status\":\"{}\",\"final_improvement_band_max_db\":{},\"holdout_used_for_tuning\":{},\"cv_uses_only_design_positions\":{},\"applied\":{},\"prior_kind\":\"{}\",\"prior_hash\":\"{}\",\"solution_hash\":\"{}\",\"coeffs_sha256\":\"{}\",\"base_solve_hash\":\"{}\"}}",
        json_bool(report.deliverable),
        report.final_solve_status,
        json_number(report.final_residual.improvement_band_max_db),
        json_bool(report.holdout_used_for_tuning),
        json_bool(report.cv_uses_only_design_positions),
        json_bool(report.applied),
        report.prior_kind,
        report.prior_hash.clone().unwrap_or_default(),
        report.solution_hash.clone().unwrap_or_default(),
        report.coeffs_sha256.clone().unwrap_or_default(),
        report.base_solve_hash.clone().unwrap_or_default()
    ));
    json.push('}');
    println!("T52_FIXTURE_JSON {json}");
}
// ===========================================================================
// 14. 容差带 tie-break：选中项必须先吃 headroom（lead review P2-1 回归）
// ===========================================================================

#[test]
fn t52_regularize_cv_tie_break_selects_simpler_candidate_first() {
    let grid = bass_grid();
    // 构造：GT 近对角 + 1e-12 级相邻 off-diagonal。full 白名单可精确拟合（CV 残差落在数值底），
    // diag_only 因缺相邻路径略差数 dB；selection_tolerance_db=6 dB 让二者落入同一容差带。
    // 严格 cv_mean 最小候选是 full，但容差带按“更少自由路径 → 更大 lambda_used”偏好 diag_only λ=1；
    // 该 tie-break 选中项必须成为 headroom 首个尝试项并真正成为最终解（否则 tie_break_used 必须为 false）。
    let gt = gt_matrix(3, 1.0e-12);
    let problem = make_problem(3, 4, &grid, &gt);
    let mut options = base_options(1);
    options.selection_tolerance_db = 6.0;
    let outcome = regularize(&problem, &options).unwrap();
    let report = &outcome.report;
    assert!(report.selection.cv_valid);
    assert!(
        report.selection.tie_break_used,
        "tiny off-diagonal fixture must hit the CV tolerance-band tie-break: {report:?}"
    );
    assert!(warning_codes(&outcome).contains(&RegularizeWarning::CV_TIEBREAK));
    let strict_best = report
        .candidates
        .iter()
        .find(|candidate| candidate.cv_rank == Some(1))
        .expect("strict CV minimum must be ranked");
    assert_eq!(
        strict_best.whitelist,
        WhitelistKind::Full,
        "strict cv_mean minimum must be the full whitelist"
    );
    let selected = report
        .candidates
        .iter()
        .find(|candidate| {
            candidate.whitelist == report.selection.selected_whitelist
                && candidate.lambda == report.selection.selected_lambda
        })
        .expect("selected candidate must be reported");
    assert!(
        strict_best.cv_mean_db.unwrap() <= selected.cv_mean_db.unwrap(),
        "tie-break selection must not beat the strict CV minimum"
    );
    assert_eq!(
        report.selection.selected_whitelist,
        WhitelistKind::DiagOnly,
        "tie-break must select the simpler whitelist and actually win headroom"
    );
    assert_eq!(report.selection.selected_lambda, 1.0);
    assert_eq!(report.selection.headroom_fallback_attempts, 0);
    assert_eq!(
        report.selection.selected_cv_rank, selected.cv_rank,
        "selected_cv_rank must describe the final selected candidate"
    );
    assert!(report.selection.selected_cv_rank.unwrap() > 1);
    assert_eq!(
        report.lambda_scan.scan_whitelist, report.selection.selected_whitelist,
        "lambda_scan.scan_whitelist must match the final selected whitelist"
    );
    assert_eq!(report.lambda_scan.points.len(), options.lambda_grid.len());
    assert!(report.deliverable);
}

// ===========================================================================
// 15. taps>1 可用域：欠定折 → CV 回退 + 不可交付（lead review P3-2 回归）
// ===========================================================================

#[test]
fn t52_regularize_underdetermined_taps_fallback_not_deliverable() {
    let grid = bass_grid();
    let gt = gt_matrix(5, 0.2);
    // T6 正式 P=6 > M=5，默认 taps=256（T51 DEFAULT_TAPS）。full 每 input 5*256=1280 未知量，
    // 而每折（P-1=5）实方程 2*5*25=250 < 1280 → T51 判欠定 → 全部折不可用 → CV 无效；
    // 回退 L-curve 后全设计仍欠定（2*6*25=300 < 1280）→ deliverable=false，只返回 best-effort。
    let problem = make_problem(5, 6, &grid, &gt);
    let mut options = base_options(256);
    options.whitelists = vec![WhitelistKind::Full];
    let outcome = regularize(&problem, &options).unwrap();
    let report = &outcome.report;
    assert!(report.applicability.p_ge_m);
    assert_eq!(report.applicability.status, APPLICABILITY_APPLICABLE);
    assert!(!report.selection.cv_valid);
    assert_eq!(report.selection.cv_reason, Some("no_valid_fold"));
    assert_eq!(report.selection.method, SELECTION_METHOD_L_CURVE);
    assert!(report.selection.fallback_used);
    assert!(
        !report.deliverable,
        "underdetermined ridge solution must not be reported as deliverable"
    );
    assert_eq!(report.final_solve_status, "underdetermined");
    assert!(
        outcome.solution.is_some(),
        "best-effort solution must still be returned for inspection"
    );
    let candidate = report
        .candidates
        .iter()
        .find(|candidate| candidate.whitelist == WhitelistKind::Full)
        .expect("full candidate must be reported");
    assert!(!candidate.cv_valid);
    assert!(candidate.fold_underdetermined);
    assert!(!candidate.applicability.fir_rank_necessary);
    assert_eq!(candidate.free_paths, 25);
    assert_eq!(candidate.applicability.fir_rank_bound, 300);
    assert_eq!(report.lambda_scan.scan_whitelist, WhitelistKind::Full);
    assert_eq!(
        report.selection.selected_whitelist,
        WhitelistKind::Full,
        "headroom fallback must stay on the only configured whitelist"
    );
}
