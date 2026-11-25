//! T54 private integration tests -- per-path modal artifact (WAV/manifest/hash/role/stale) --
//! crate khaen-modal-design (WBS T54; task-150; owner rt-artifact2).
//!
//! Run (from rt-stack/): cargo test -p khaen-modal-design artifact
//! Location: crates/modal-design/tests/T54/main.rs (leaf private tests; public_registration.rs untouched).
//!
//! Coverage (WBS T54 DoD):
//! - per-path WAV: mono IEEE float32, fs = design fs, length = taps, no normalization, byte-reproducible;
//!   decoded f32 coefficients must be bit-identical to T51/T52 solution coeffs_f32 (f64 design -> f32
//!   export quantization error is recomputed and recorded);
//! - manifest: schema/version/hash-algo, status + INCOMPLETE/FAIL codes, tool, identity hash chain
//!   (target/baseline/P/plant/solution/holdout/provenance), 5x5 participants dense->DAC, stereo mask,
//!   overlay DAC binding (source sha256 + ch6 empty + unused outputs), role_hash, P binding, fs/precision,
//!   D, decay/T20 target, tone/curve single application, height scope, per-path sha256;
//! - determinism: same input -> byte-identical files; created_unix only in manifest, not in artifact hash;
//! - stale: any change of fs/layout/participants/role_hash/overlay/P/profile/target/solution/prior/taps/D/
//!   holdout -> fixed-order stale reasons;
//! - INCOMPLETE/FAIL: missing solution/not deliverable/missing P/missing holdout/holdout INCOMPLETE/NOT_RUN/
//!   PREDICTED_FAIL/missing provenance are recorded honestly (missing real data never becomes READY);
//! - negative controls: holdout binding mismatch/solution hash tamper/WAV tamper/manifest tamper/missing
//!   file/overlay mapping error/fs/taps/D mismatch are rejected;
//! - stereo strict independent 2x2 profile: stereo_strict=true and mask exactly FL/FR (5x5 not mixed in);
//! - static guards: no unsafe/threads/process/env/net/third-party serialization, no unwrap/expect/panic
//!   (NONRT pure in-memory build; write_to_dir is the only file I/O).
//!
//! Note: fixtures are synthetic engineering fixtures (not measurements); this test file does not write
//! repository files (write_to_dir uses the system temp dir and cleans up), does not touch board/ALSA/
//! CueMix/RT, does not read real sessions, and does not run git.

use std::fs;

use khaen_modal_design::artifact::{
    build_artifact, decode_wav_f32_mono, encode_wav_f32_mono, ArtifactBundle, ArtifactCurrent,
    ArtifactErrorKind, ArtifactInput, ArtifactProvenance, ArtifactSolution, ArtifactStaleReason,
    ArtifactStatus, ArtifactTool, OverlayDacBinding, OverlayRole, ARTIFACT_CLAIM,
    ARTIFACT_HASH_ALGO, ARTIFACT_MANIFEST_FILE, ARTIFACT_SCHEMA, ARTIFACT_VERSION,
    ARTIFACT_WAV_FORMAT, F_HOLDOUT_PREDICTED_FAIL, R_CALIBRATION_PROVENANCE_MISSING,
    R_HOLDOUT_INCOMPLETE, R_HOLDOUT_MISSING, R_MEASUREMENT_PROVENANCE_MISSING,
    R_POST_BASELINE_MISSING, R_SOLUTION_MISSING, R_SOLUTION_NOT_DELIVERABLE, STATUS_FAIL,
    STATUS_INCOMPLETE, STATUS_READY, W_ART_DESIGN_ONLY,
};
use khaen_modal_design::ingest::{sha256_hex, PositionRole};
use khaen_modal_design::regularize::{
    regularize_from_target_plant, HeadroomSpec, RegularizeOptions, WhitelistKind,
    CORRECTION_GAIN_CAP_MAX_DB,
};
use khaen_modal_design::solve::{solve_from_target_plant, SolveOptions, SolveStatus};
use khaen_modal_design::target::{
    build_stereo_strict_target, build_target_matrix, BandDecay, CurveBinding, DecayTarget,
    EffectivePlant, FittedPostBaseline, FrozenPostBaseline, ModalParticipants, PostBaselineInput,
    PostRoleFit, RawPlant, RawPosition, RawResponse, TargetMatrix, TargetSpec,
    ToneApplicationProof, ToneBinding, HEIGHT_ROLE_IDS, MODAL_CORRECTION_BAND_HZ, P1_LAYOUT_4_1_2,
    STEREO_STRICT_OUTPUT_ROLE_IDS,
};
use khaen_modal_design::timebase::{CommonD0, ComplexPoint, PolaritySource, ResolvedPolarity};
use khaen_modal_design::validate::{
    validate_holdout, BandIr, HoldoutReport, HoldoutRequest, HoldoutVerdict, IrOrigin, IrState,
    NoiseRepeats, RoleRepeats, TuningDeclaration,
};

// ===========================================================================
// fixture constants (synthetic; not measurements)
// ===========================================================================

const FS: u32 = 48_000;
const D0_SAMPLES: i64 = 120;
const TUNING_INPUTS_SHA256: &str =
    "1111111111111111111111111111111111111111111111111111111111111111";
const SECONDS_IR: f64 = 1.5;
const HOLDOUT_LABELS: [&str; 3] = ["H01", "H02", "H03"];

/// 6 design positions; each H row sums to 4.0 so the ones vector is in the column space
/// (the reduced target is exactly reachable by a 1-tap G fixture).
const DESIGN_ROWS: [[f64; 5]; 6] = [
    [1.0, 0.9, 0.8, 0.7, 0.6],
    [0.6, 0.7, 0.8, 0.9, 1.0],
    [0.8, 1.0, 0.6, 1.0, 0.6],
    [0.7, 0.7, 0.9, 0.9, 0.8],
    [0.9, 0.8, 0.7, 0.8, 0.8],
    [1.0, 0.8, 0.8, 0.8, 0.6],
];

/// 3 independent holdout positions (synthetic fixture, not measurements).
const HOLDOUT_ROWS: [[f64; 5]; 3] = [
    [1.0, 0.9, 0.8, 0.7, 0.6],
    [0.6, 0.7, 0.8, 0.9, 1.0],
    [0.8, 1.0, 0.6, 1.0, 0.6],
];

/// stereo strict 2x2 design rows (each row sums to 1.0 -> reachable target).
const STEREO_ROWS: [[f64; 2]; 3] = [[0.6, 0.4], [0.5, 0.5], [0.7, 0.3]];

// ===========================================================================
// fixture helpers (T50/T51/T52/T53 + overlay/tool/provenance)
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
        fit_inputs_sha256: sha256_hex(b"t54-fit-inputs-v1"),
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
    role: PositionRole,
    participants: &ModalParticipants,
    grid: &[f64],
    row: &[f64],
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
                PositionRole::Design,
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
) -> EffectivePlant {
    let mut positions = vec![raw_position_row(
        "PH00",
        PositionRole::Design,
        participants,
        grid,
        &[1.0, 0.9, 0.8, 0.7, 0.6],
    )];
    for (index, row) in HOLDOUT_ROWS.iter().enumerate() {
        positions.push(raw_position_row(
            &format!("H{:02}", index + 1),
            PositionRole::Validation,
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

fn build_target(baseline: &FrozenPostBaseline, plant: &EffectivePlant) -> TargetMatrix {
    let spec = TargetSpec::for_baseline(baseline, reduced_decay(0.85)).unwrap();
    build_target_matrix(baseline, plant, &spec).unwrap()
}

fn solve_outcome(
    target: &TargetMatrix,
    plant: &EffectivePlant,
) -> khaen_modal_design::solve::SolveOutcome {
    let mut options = SolveOptions::default();
    options.taps = 1;
    options.delay_samples = 0;
    options.fit_band_hz = (20.0, 300.0);
    options.max_iterations = 64;
    options.rel_tolerance = 1.0e-8;
    solve_from_target_plant(target, plant, &options).unwrap()
}

fn regularize_options() -> RegularizeOptions {
    let mut options = RegularizeOptions::default();
    options.base.taps = 1;
    options.base.delay_samples = 0;
    options.base.fit_band_hz = (20.0, 300.0);
    options.base.max_iterations = 64;
    options.base.rel_tolerance = 1.0e-8;
    options.lambda_grid = vec![0.0, 1.0e-4, 1.0e-2, 1.0];
    options.whitelists = vec![WhitelistKind::DiagOnly, WhitelistKind::Full];
    options.cv_folds = 0;
    options.headroom = HeadroomSpec {
        correction_cap_db: CORRECTION_GAIN_CAP_MAX_DB,
        diagonal_cap_db: 6.0,
        tolerance_db: 2.0,
    };
    options
}

fn make_declaration(
    design_plant: &EffectivePlant,
    sealed_before_design: bool,
) -> TuningDeclaration {
    let design_labels: Vec<String> = design_plant
        .design_positions()
        .map(|position| position.label.clone())
        .collect();
    TuningDeclaration::new(
        design_labels.clone(),
        HOLDOUT_LABELS
            .iter()
            .map(|label| (*label).to_string())
            .collect(),
    )
    .with_tuning(design_labels, TUNING_INPUTS_SHA256)
    .with_seal(sealed_before_design)
}

fn make_repeats(participants: &ModalParticipants, grid: &[f64], delta: f64) -> Vec<NoiseRepeats> {
    HOLDOUT_LABELS
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

fn make_band_ir(off_t60_s: f64, on_t60_s: f64) -> Vec<BandIr> {
    let off = exponential_decay(off_t60_s, SECONDS_IR);
    let on = exponential_decay(on_t60_s, SECONDS_IR);
    let mut irs = Vec::new();
    for label in HOLDOUT_LABELS {
        for band_hz in [63.0, 125.0] {
            for (state, samples) in [(IrState::Off, &off), (IrState::On, &on)] {
                irs.push(BandIr {
                    position_label: label.to_string(),
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

fn holdout_report(
    target: &TargetMatrix,
    design_plant: &EffectivePlant,
    holdout_plant: &EffectivePlant,
    solution: &khaen_modal_design::target::ModalMatrix,
    participants: &ModalParticipants,
    grid: &[f64],
) -> HoldoutReport {
    let request = HoldoutRequest::new(make_declaration(design_plant, true))
        .with_repeats(make_repeats(participants, grid, 1.0e-3))
        .with_ir(make_band_ir(0.40, 0.32), IrOrigin::PredictedModel);
    validate_holdout(target, design_plant, holdout_plant, solution, &request).unwrap()
}

/// Resolved overlay binding (synthetic stand-in for the T57 parse result of the board overlay;
/// source hash is a fixture declaration).
fn overlay_binding() -> OverlayDacBinding {
    let roles = vec![
        OverlayRole::new("FL", vec![0]).unwrap(),
        OverlayRole::new("FR", vec![1]).unwrap(),
        OverlayRole::new("SL", vec![2]).unwrap(),
        OverlayRole::new("SR", vec![3]).unwrap(),
        OverlayRole::new("HFL", vec![4]).unwrap(),
        OverlayRole::new("HFR", vec![5]).unwrap(),
        OverlayRole::new("SUB1", vec![7]).unwrap(),
    ];
    OverlayDacBinding::new(
        P1_LAYOUT_4_1_2,
        sha256_hex(b"t54-overlay-layout-presets-fixture-v1"),
        8,
        roles,
    )
    .unwrap()
}

fn tool() -> ArtifactTool {
    ArtifactTool::new("khaen-modal-design", "t54-tests").unwrap()
}

fn provenance() -> ArtifactProvenance {
    ArtifactProvenance::new(
        sha256_hex(b"t54-profile-binding"),
        Some(sha256_hex(b"t54-measurement-session")),
        Some(sha256_hex(b"t54-calibration-ir")),
    )
    .with_tuning_inputs(TUNING_INPUTS_SHA256)
}

struct Fixture {
    baseline: FrozenPostBaseline,
    target: TargetMatrix,
    design_plant_hash: String,
    solve: khaen_modal_design::solve::SolveOutcome,
    regularize: khaen_modal_design::regularize::RegularizeOutcome,
    holdout: HoldoutReport,
    overlay: OverlayDacBinding,
    tool: ArtifactTool,
    provenance: ArtifactProvenance,
}

fn fixture() -> Fixture {
    let grid = bass_grid();
    let participants = ModalParticipants::p1_4_1_2();
    let baseline = make_baseline(&grid, participants.clone(), "t54-baseline");
    let design_plant = make_design_plant(&baseline, &participants, &grid);
    let target = build_target(&baseline, &design_plant);
    let solve = solve_outcome(&target, &design_plant);
    let regularize =
        regularize_from_target_plant(&target, &design_plant, &regularize_options()).unwrap();
    let holdout_plant = make_holdout_plant(&baseline, &participants, &grid);
    let solution = solve
        .solution
        .as_ref()
        .expect("solve fixture must return a solution");
    let holdout = holdout_report(
        &target,
        &design_plant,
        &holdout_plant,
        &solution.modal,
        &participants,
        &grid,
    );
    Fixture {
        baseline,
        target,
        design_plant_hash: design_plant.hash(),
        solve,
        regularize,
        holdout,
        overlay: overlay_binding(),
        tool: tool(),
        provenance: provenance(),
    }
}

fn regularized_artifact(fx: &Fixture) -> ArtifactBundle {
    let input = ArtifactInput::new(
        &fx.target,
        ArtifactSolution::Regularized(&fx.regularize),
        &fx.overlay,
        fx.tool.clone(),
        fx.provenance.clone(),
    )
    .with_post(&fx.baseline)
    .with_holdout(&fx.holdout)
    .with_design_plant_hash(fx.design_plant_hash.as_str());
    build_artifact(&input).expect("regularized artifact must build")
}

fn solved_artifact(fx: &Fixture) -> ArtifactBundle {
    let input = ArtifactInput::new(
        &fx.target,
        ArtifactSolution::Solve(&fx.solve),
        &fx.overlay,
        fx.tool.clone(),
        fx.provenance.clone(),
    )
    .with_post(&fx.baseline)
    .with_holdout(&fx.holdout)
    .with_design_plant_hash(fx.design_plant_hash.as_str());
    build_artifact(&input).expect("solve artifact must build")
}

fn input_without_post_holdout(fx: &Fixture) -> ArtifactInput<'_> {
    ArtifactInput::new(
        &fx.target,
        ArtifactSolution::Regularized(&fx.regularize),
        &fx.overlay,
        fx.tool.clone(),
        fx.provenance.clone(),
    )
    .with_design_plant_hash(fx.design_plant_hash.as_str())
}

fn regularized_input<'a>(fx: &'a Fixture, holdout: &'a HoldoutReport) -> ArtifactInput<'a> {
    ArtifactInput::new(
        &fx.target,
        ArtifactSolution::Regularized(&fx.regularize),
        &fx.overlay,
        fx.tool.clone(),
        fx.provenance.clone(),
    )
    .with_post(&fx.baseline)
    .with_holdout(holdout)
    .with_design_plant_hash(fx.design_plant_hash.as_str())
}

fn input_with_overlay<'a>(fx: &'a Fixture, overlay: &'a OverlayDacBinding) -> ArtifactInput<'a> {
    ArtifactInput::new(
        &fx.target,
        ArtifactSolution::Regularized(&fx.regularize),
        overlay,
        fx.tool.clone(),
        fx.provenance.clone(),
    )
    .with_post(&fx.baseline)
    .with_holdout(&fx.holdout)
    .with_design_plant_hash(fx.design_plant_hash.as_str())
}

// ===========================================================================
// minimal JSON syntax validator (evidence JSON must parse without third-party crates)
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
        self.skip_whitespace();
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
        self.skip_whitespace();
        match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => self.string(),
            Some(b't') => self.literal(b"true"),
            Some(b'f') => self.literal(b"false"),
            Some(b'n') => self.literal(b"null"),
            Some(byte) if byte == b'-' || byte.is_ascii_digit() => self.number(),
            Some(byte) => Err(format!(
                "unexpected byte {:?} at {}",
                byte as char, self.position
            )),
            None => Err("unexpected end of JSON".to_string()),
        }
    }

    fn literal(&mut self, expected: &[u8]) -> Result<(), String> {
        if self.bytes[self.position..].starts_with(expected) {
            self.position += expected.len();
            Ok(())
        } else {
            Err(format!("invalid literal at byte {}", self.position))
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
            self.expect(b':')?;
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
                _ => return Err(format!("expected , or }} at byte {}", self.position)),
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
                _ => return Err(format!("expected , or ] at byte {}", self.position)),
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
                                    _ => return Err("invalid \\u escape".to_string()),
                                }
                            }
                        }
                        _ => return Err("invalid escape".to_string()),
                    }
                }
                Some(byte) if byte < 0x20 => {
                    return Err(format!("control character at byte {}", self.position));
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
        let mut digits = 0usize;
        while let Some(byte) = self.peek() {
            if byte.is_ascii_digit() {
                digits += 1;
                self.position += 1;
            } else {
                break;
            }
        }
        if digits == 0 {
            return Err(format!("invalid number at byte {}", self.position));
        }
        if self.peek() == Some(b'.') {
            self.position += 1;
            let mut fraction = 0usize;
            while let Some(byte) = self.peek() {
                if byte.is_ascii_digit() {
                    fraction += 1;
                    self.position += 1;
                } else {
                    break;
                }
            }
            if fraction == 0 {
                return Err(format!("invalid fraction at byte {}", self.position));
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
                    exponent += 1;
                    self.position += 1;
                } else {
                    break;
                }
            }
            if exponent == 0 {
                return Err(format!("invalid exponent at byte {}", self.position));
            }
        }
        Ok(())
    }
}

// ===========================================================================
// 1. fixture smoke + READY manifest (main T54 path)
// ===========================================================================

#[test]
fn t54_artifact_fixture_smoke() {
    let fx = fixture();
    assert_eq!(fx.target.m(), 5);
    assert_eq!(fx.target.sample_rate_hz(), FS);
    assert_eq!(fx.target.target_delay_samples(), 0);
    let solution = fx.solve.solution.as_ref().expect("solve solution");
    assert_eq!(solution.modal.m(), 5);
    assert_eq!(solution.coeffs_f64.len(), 5 * 5 * 1);
    assert_eq!(fx.holdout.verdict, HoldoutVerdict::PredictedPass);
    assert!(
        fx.regularize.solution.is_some(),
        "regularize must return a solution"
    );
    // explicit 5x5 low-band participants DAC 0/1/2/3/7; height roles stay outside the matrix.
    assert_eq!(
        fx.target.participants().dense_to_out_index(),
        vec![0, 1, 2, 3, 7]
    );
    for height in HEIGHT_ROLE_IDS {
        assert!(!fx.target.participants().contains_role(height));
    }
}

#[test]
fn t54_artifact_ready_manifest_contract() {
    let fx = fixture();
    let bundle = regularized_artifact(&fx);
    assert_eq!(bundle.schema, ARTIFACT_SCHEMA);
    assert_eq!(bundle.artifact_version, ARTIFACT_VERSION);
    assert_eq!(bundle.hash_algo, ARTIFACT_HASH_ALGO);
    assert_eq!(
        bundle.status,
        ArtifactStatus::Ready,
        "regularized fixture must be READY (design-side); codes={:?}",
        bundle.status_codes
    );
    assert!(bundle.status_codes.is_empty());
    assert_eq!(bundle.status.as_str(), STATUS_READY);
    assert_eq!(bundle.artifact_hash.len(), 64);
    assert_eq!(bundle.manifest_sha256.len(), 64);
    // honesty: READY is still not applied/measured/compiled and requires post-apply remeasurement.
    assert!(!bundle.honesty.applied);
    assert!(!bundle.honesty.measured);
    assert!(!bundle.honesty.compiled);
    assert_eq!(bundle.honesty.post_apply_measurement, "not_run");
    assert!(!bundle.honesty.holdout_used_for_tuning);
    assert!(!bundle.honesty.can_substitute_real_remeasurement);
    assert!(bundle.honesty.requires_post_apply_remeasurement);
    assert_eq!(bundle.honesty.claims, ARTIFACT_CLAIM);
    assert!(bundle.warnings.contains(&W_ART_DESIGN_ONLY));
    // 5x5 participants / dense->DAC / overlay / stereo mask (4.1.2 is not stereo strict).
    assert_eq!(bundle.scope.m, 5);
    assert_eq!(bundle.scope.fs, FS);
    assert_eq!(bundle.scope.dense_to_dac, vec![0, 1, 2, 3, 7]);
    assert_eq!(bundle.scope.layout_id, P1_LAYOUT_4_1_2);
    assert_eq!(bundle.scope.container_channels, 8);
    assert!(!bundle.scope.stereo_strict);
    assert!(bundle.scope.stereo_mask.is_empty());
    assert_eq!(
        bundle.scope.overlay.unused_outputs,
        vec![6],
        "4.1.2 ch6 must be empty"
    );
    assert_eq!(bundle.scope.role_hash.len(), 64);
    // files = manifest + 25 path WAVs.
    assert_eq!(bundle.files.len(), 1 + 25);
    assert_eq!(bundle.files[0].rel_path, ARTIFACT_MANIFEST_FILE);
    assert_eq!(bundle.paths.len(), 25);
    // manifest JSON syntax + no NaN/Inf text.
    let manifest = bundle.to_manifest_json();
    JsonSyntax::validate(&manifest).expect("manifest must be valid JSON");
    assert!(!manifest.contains("NaN"), "manifest must not contain NaN");
    assert!(!manifest.contains("inf"), "manifest must not contain inf");
    assert!(manifest.contains("\"status\":\"READY\""));
    assert!(manifest.contains("\"schema\":\"khaen-modal-artifact/v1\""));
    bundle.verify().expect("READY artifact must self-verify");
}

// ===========================================================================
// 2. per-path WAV: format/length/fs/no normalization/bit-exact f32
// ===========================================================================

#[test]
fn t54_artifact_wav_per_path_roundtrip() {
    let fx = fixture();
    let bundle = regularized_artifact(&fx);
    let solution = fx
        .regularize
        .solution
        .as_ref()
        .expect("regularized solution");
    let m = fx.target.m();
    let taps = bundle.design.taps as usize;
    assert_eq!(bundle.design.taps, 1);
    assert_eq!(bundle.design.delay_samples, 0);
    for out_dense in 0..m {
        for in_dense in 0..m {
            let path = bundle
                .paths
                .iter()
                .find(|path| path.out_dense == out_dense as u16 && path.in_dense == in_dense as u16)
                .expect("path record must exist");
            assert_eq!(path.format, ARTIFACT_WAV_FORMAT);
            assert_eq!(path.fs, FS);
            assert_eq!(path.taps, taps as u32);
            assert_eq!(path.band_hz, MODAL_CORRECTION_BAND_HZ);
            let expected_name = format!("coeff/p_{}__{}.wav", path.in_role, path.out_role);
            assert_eq!(path.file, expected_name);
            let file = bundle.file(&path.file).expect("WAV file must exist");
            let wav = decode_wav_f32_mono(&file.bytes).expect("canonical WAV must decode");
            assert_eq!(wav.sample_rate_hz, FS);
            assert_eq!(wav.samples.len(), taps);
            assert_eq!(wav.samples.len() as u64 * 4 + 44, file.bytes.len() as u64);
            assert_eq!(file.bytes.len() as u64, path.bytes);
            assert_eq!(sha256_hex(&file.bytes), path.sha256);
            // WAV f32 must be bit-identical to the T52 solution coeffs_f32.
            let offset = (out_dense * m + in_dense) * taps;
            for (index, sample) in wav.samples.iter().enumerate() {
                assert_eq!(
                    sample.to_bits(),
                    solution.coeffs_f32[offset + index].to_bits(),
                    "path ({out_dense},{in_dense}) sample {index} f32 bits differ"
                );
            }
            // diagonal declares delta_D baseline (D carried by manifest/kernel), off-diagonal none.
            let diag = out_dense == in_dense;
            assert_eq!(path.diag_baseline, diag);
            assert_eq!(
                path.delay_samples,
                if diag { bundle.design.delay_samples } else { 0 }
            );
            assert_eq!(path.delay_q16, 0);
            // no normalization: recorded peak equals the actual coefficient peak.
            let mut peak = 0.0f64;
            for sample in &wav.samples {
                peak = peak.max(f64::from(sample.abs()));
            }
            assert_eq!(peak, path.peak_abs);
        }
    }
}

// ===========================================================================
// 3. fs/precision and f32 quantization recomputation (WBS T54 "fs precision")
// ===========================================================================

#[test]
fn t54_artifact_fs_precision_and_quantization() {
    let fx = fixture();
    let bundle = regularized_artifact(&fx);
    let solution = fx.regularize.solution.as_ref().expect("solution");
    assert_eq!(bundle.quantization.sample_format, ARTIFACT_WAV_FORMAT);
    assert_eq!(bundle.quantization.bits_per_sample, 32);
    assert_eq!(
        bundle.quantization.coeffs_f64_sha256,
        solution.coeffs_sha256
    );
    assert_eq!(bundle.quantization.coeffs_f32_sha256.len(), 64);
    assert_eq!(bundle.quantization.wav_aggregate_sha256.len(), 64);
    assert!(bundle.quantization.max_abs_f32_error.is_finite());
    assert!(bundle.quantization.max_abs_f32_error >= 0.0);
    assert!(
        bundle.quantization.max_abs_f32_error < 1.0e-6,
        "f32 quantization error {} unexpectedly large for this fixture",
        bundle.quantization.max_abs_f32_error
    );
    // recompute the f32 aggregate hash from decoded WAV bytes (independent of internal path).
    let mut all: Vec<f32> = Vec::new();
    for path in &bundle.paths {
        let file = bundle.file(&path.file).expect("WAV");
        let wav = decode_wav_f32_mono(&file.bytes).expect("decode");
        all.extend_from_slice(&wav.samples);
    }
    let mut text = String::new();
    for sample in &all {
        text.push_str(&format!("{:08x}\n", sample.to_bits()));
    }
    assert_eq!(
        sha256_hex(text.as_bytes()),
        bundle.quantization.coeffs_f32_sha256
    );
    let solved = solved_artifact(&fx);
    let solve_solution = fx.solve.solution.as_ref().expect("solve solution");
    assert_eq!(
        solved.quantization.coeffs_f64_sha256,
        solve_solution.coeffs_sha256
    );
    assert_eq!(solved.quantization.bits_per_sample, 32);
}

// ===========================================================================
// 4. role hash / overlay DAC / stereo mask (multichannel)
// ===========================================================================

#[test]
fn t54_artifact_role_hash_and_overlay_dac() {
    let fx = fixture();
    let bundle = regularized_artifact(&fx);
    assert_eq!(bundle.scope.participants.len(), 5);
    let expected_roles = ["FL", "FR", "SL", "SR", "SUB1"];
    let expected_dac = [0u32, 1, 2, 3, 7];
    for (index, participant) in bundle.scope.participants.iter().enumerate() {
        assert_eq!(participant.dense as usize, index);
        assert_eq!(participant.role_id, expected_roles[index]);
        assert_eq!(participant.dac, expected_dac[index]);
    }
    // overlay binding hash must come from the resolved binding (source sha256 + role outputs).
    assert_eq!(
        bundle.scope.overlay.source_sha256,
        fx.overlay.source_sha256()
    );
    assert_eq!(bundle.scope.overlay.binding_hash, fx.overlay.binding_hash());
    assert_eq!(bundle.scope.overlay.roles.len(), 7);
    assert_eq!(bundle.scope.overlay.unused_outputs, vec![6]);
    for height in HEIGHT_ROLE_IDS {
        assert!(!bundle
            .scope
            .participants
            .iter()
            .any(|participant| participant.role_id == height));
    }
    assert_eq!(bundle.scope.role_hash.len(), 64);
    // changing overlay source hash -> role_hash must change (stale semantics computable).
    let roles = fx.overlay.roles().to_vec();
    let mutated_overlay = OverlayDacBinding::new(
        P1_LAYOUT_4_1_2,
        sha256_hex(b"t54-overlay-layout-presets-fixture-v2"),
        8,
        roles,
    )
    .unwrap();
    let mutated = build_artifact(&input_with_overlay(&fx, &mutated_overlay))
        .expect("mutated overlay artifact");
    assert_ne!(mutated.scope.role_hash, bundle.scope.role_hash);
    assert_ne!(
        mutated.scope.overlay.binding_hash,
        bundle.scope.overlay.binding_hash
    );
}

// ===========================================================================
// 5. required manifest bindings (T50 section 9 list + P/decay/tone/curve/height/D)
// ===========================================================================

#[test]
fn t54_artifact_manifest_required_bindings() {
    let fx = fixture();
    let bundle = regularized_artifact(&fx);
    let solution = fx.regularize.solution.as_ref().expect("solution");
    assert_eq!(bundle.identity.target_schema, fx.target.schema());
    assert_eq!(bundle.identity.target_id, fx.target.target_id());
    assert_eq!(bundle.identity.target_sha256, fx.target.target_sha256());
    assert_eq!(bundle.identity.baseline_hash, fx.target.baseline_hash());
    assert_eq!(
        bundle.identity.design_plant_hash,
        fx.target.design_plant_hash()
    );
    assert_eq!(bundle.identity.plant_hash, fx.target.plant_hash());
    assert_eq!(
        bundle.identity.solution_hash.as_deref(),
        Some(solution.hash.as_str())
    );
    assert_eq!(bundle.identity.solution_kind, "regularized");
    assert_eq!(
        bundle.identity.holdout_report_hash.as_deref(),
        Some(fx.holdout.report_hash.as_str())
    );
    // P binding: baseline hash matches and role metadata is complete (participants + HFL/HFR).
    assert!(bundle.post.present);
    assert_eq!(bundle.post.baseline_hash, fx.target.baseline_hash());
    assert_eq!(bundle.post.sample_rate_hz, FS);
    assert_eq!(bundle.post.roles.len(), 7);
    for role in &bundle.post.roles {
        assert_eq!(role.delay_samples, 0);
        assert_eq!(role.polarity, 1);
        assert_eq!(role.gain_db, 0.0);
        assert!(!role.modal_peq);
        assert_eq!(role.artificial_delay_ms, 0.0);
    }
    // decay target (ratio .85 -> T60 0.34/0.2975); a declaration, not a measurement.
    assert_eq!(bundle.design.decay.mode, "reduced");
    assert_eq!(bundle.design.decay.ratio, 0.85);
    assert!(bundle.design.decay.meets_t20_gate);
    assert_eq!(bundle.design.decay.bands.len(), 2);
    let band63 = bundle
        .design
        .decay
        .bands
        .iter()
        .find(|band| band.band_hz == 63.0)
        .expect("63 Hz band");
    assert!((band63.baseline_t60_s - 0.40).abs() < 1.0e-12);
    assert!((band63.target_t60_s - 0.34).abs() < 1.0e-9);
    // tone once (post baseline 1 / low-band residual 0 / target 0); curve not double-applied.
    assert_eq!(bundle.design.tone.post_baseline_applications, 1);
    assert_eq!(bundle.design.tone.low_band_residual_applications, 0);
    assert_eq!(bundle.design.tone.target_applications, 0);
    assert_eq!(bundle.design.curve.applications, 1);
    assert!(bundle.design.curve.single_application);
    // height scope: HFL/HFR full range, no HP/LP, no artificial delay, not in the low-band matrix.
    assert!(bundle.design.height_scope.present);
    assert!(bundle.design.height_scope.excluded_from_low_band_matrix);
    assert_eq!(bundle.design.height_scope.roles.len(), 2);
    for role in &bundle.design.height_scope.roles {
        assert!(HEIGHT_ROLE_IDS.contains(&role.role_id.as_str()));
        assert_eq!(role.band_limit, "none");
        assert_eq!(role.artificial_delay_ms, 0.0);
        assert!(!role.in_low_band_matrix);
        assert!(role.dac == 4 || role.dac == 5);
    }
    assert_eq!(
        bundle.design.delay_samples,
        fx.target.target_delay_samples()
    );
    assert_eq!(bundle.design.delay_q16, 0);
    assert_eq!(bundle.design.fs, FS);
    assert_eq!(bundle.design.grid_points, fx.target.grid_hz().len());
    assert_eq!(bundle.design.common_d0_samples, D0_SAMPLES);
    let low_band = bundle
        .design
        .output_scope
        .iter()
        .filter(|entry| entry.kind == "low_band_matrix")
        .count();
    assert_eq!(low_band, 5);
}

// ===========================================================================
// 6. determinism: same input -> byte-identical build
// ===========================================================================

#[test]
fn t54_artifact_deterministic_bytes() {
    let fx = fixture();
    let first = regularized_artifact(&fx);
    let second = regularized_artifact(&fx);
    assert_eq!(first.artifact_hash, second.artifact_hash);
    assert_eq!(first.manifest_sha256, second.manifest_sha256);
    assert_eq!(first.files.len(), second.files.len());
    for (left, right) in first.files.iter().zip(second.files.iter()) {
        assert_eq!(left.rel_path, right.rel_path);
        assert_eq!(
            left.bytes, right.bytes,
            "file {} not deterministic",
            left.rel_path
        );
    }
    assert_eq!(first.canonical(), second.canonical());
    assert_eq!(first.to_manifest_json(), second.to_manifest_json());
    let solved_first = solved_artifact(&fx);
    let solved_second = solved_artifact(&fx);
    assert_eq!(solved_first.artifact_hash, solved_second.artifact_hash);
    assert_eq!(solved_first.files, solved_second.files);
}

// ===========================================================================
// 7. created_unix only in manifest, not in artifact hash
// ===========================================================================

#[test]
fn t54_artifact_created_unix_not_hashed() {
    let fx = fixture();
    let base = ArtifactInput::new(
        &fx.target,
        ArtifactSolution::Regularized(&fx.regularize),
        &fx.overlay,
        fx.tool.clone(),
        fx.provenance.clone(),
    )
    .with_post(&fx.baseline)
    .with_holdout(&fx.holdout)
    .with_design_plant_hash(fx.design_plant_hash.as_str());
    let plain = build_artifact(&base).expect("plain artifact");
    let stamped_input = ArtifactInput::new(
        &fx.target,
        ArtifactSolution::Regularized(&fx.regularize),
        &fx.overlay,
        fx.tool.clone(),
        fx.provenance.clone(),
    )
    .with_post(&fx.baseline)
    .with_holdout(&fx.holdout)
    .with_design_plant_hash(fx.design_plant_hash.as_str())
    .with_created_unix(1_700_000_000);
    let stamped = build_artifact(&stamped_input).expect("stamped artifact");
    assert_eq!(plain.created_unix, None);
    assert_eq!(stamped.created_unix, Some(1_700_000_000));
    assert_eq!(
        plain.artifact_hash, stamped.artifact_hash,
        "created_unix must not participate in artifact hash"
    );
    assert_eq!(plain.canonical(), stamped.canonical());
    assert_ne!(
        plain.manifest_sha256, stamped.manifest_sha256,
        "created_unix changes manifest bytes"
    );
    assert!(stamped
        .to_manifest_json()
        .contains("\"created_unix\":1700000000"));
    plain.verify().expect("plain verify");
    stamped.verify().expect("stamped verify");
}

// ===========================================================================
// 8. solve / regularized upstream variants
// ===========================================================================

#[test]
fn t54_artifact_solve_and_regularized_variants() {
    let fx = fixture();
    let solved = solved_artifact(&fx);
    let solve_solution = fx.solve.solution.as_ref().expect("solve solution");
    assert_eq!(solved.solution.kind, "solve");
    assert_eq!(
        solved.solution.hash.as_deref(),
        Some(solve_solution.hash.as_str())
    );
    assert!(solved.solution.selection.is_none());
    assert!(solved.solution.headroom.is_none());
    assert_eq!(solved.identity.solution_kind, "solve");
    assert_eq!(solved.status, ArtifactStatus::Ready);
    let regularized = regularized_artifact(&fx);
    let reg_solution = fx
        .regularize
        .solution
        .as_ref()
        .expect("regularized solution");
    assert_eq!(regularized.solution.kind, "regularized");
    assert_eq!(
        regularized.solution.hash.as_deref(),
        Some(reg_solution.hash.as_str())
    );
    assert_eq!(
        regularized.solution.base_solve_hash.as_deref(),
        reg_solution.base_solve_hash.as_deref()
    );
    assert_eq!(
        regularized.solution.prior_hash.as_deref(),
        Some(reg_solution.prior_hash.as_str())
    );
    let selection = regularized.solution.selection.as_ref().expect("selection");
    // M=5 with 6 design positions: design-internal CV may fall back to the L-curve at the
    // P=M boundary; the selection is reported honestly (method + cv_reason), never hidden.
    assert!(matches!(
        selection.method,
        "cv" | "l_curve_fallback" | "preserve_baseline"
    ));
    if selection.cv_valid {
        assert_eq!(selection.method, "cv");
    } else {
        assert!(selection.cv_reason.is_some());
    }
    assert!(!selection.selected_whitelist.is_empty());
    assert!(regularized
        .solution
        .report_status
        .starts_with("regularized_"));
    let headroom = regularized.solution.headroom.as_ref().expect("headroom");
    assert!(headroom.passed, "relaxed headroom fixture must pass");
    assert!(headroom.violated_paths.is_empty());
}

// ===========================================================================
// 9. INCOMPLETE: missing P baseline / missing holdout report
// ===========================================================================

#[test]
fn t54_artifact_incomplete_missing_post_and_holdout() {
    let fx = fixture();
    let input = input_without_post_holdout(&fx);
    let bundle = build_artifact(&input).expect("incomplete artifact must still build");
    assert_eq!(bundle.status, ArtifactStatus::Incomplete);
    assert_eq!(bundle.status.as_str(), STATUS_INCOMPLETE);
    assert!(bundle.status_codes.contains(&R_POST_BASELINE_MISSING));
    assert!(bundle.status_codes.contains(&R_HOLDOUT_MISSING));
    assert!(!bundle.post.present);
    assert!(!bundle.holdout.present);
    assert_eq!(
        bundle.paths.len(),
        25,
        "solution coefficients are still recorded"
    );
    bundle
        .verify()
        .expect("INCOMPLETE artifact must self-verify");
}

// ===========================================================================
// 10. INCOMPLETE: no solution / not deliverable
// ===========================================================================

#[test]
fn t54_artifact_incomplete_missing_solution() {
    let mut fx = fixture();
    fx.solve.solution = None;
    let bundle = solved_artifact(&fx);
    assert_eq!(bundle.status, ArtifactStatus::Incomplete);
    assert!(bundle.status_codes.contains(&R_SOLUTION_MISSING));
    assert!(bundle.status_codes.contains(&R_SOLUTION_NOT_DELIVERABLE));
    assert!(!bundle.solution.present);
    assert!(bundle.paths.is_empty());
    assert_eq!(bundle.files.len(), 1);
    bundle
        .verify()
        .expect("no-solution artifact must self-verify");
}

#[test]
fn t54_artifact_incomplete_not_deliverable() {
    let mut fx = fixture();
    fx.solve.report.status = SolveStatus::NonConverged;
    let bundle = solved_artifact(&fx);
    assert_eq!(bundle.status, ArtifactStatus::Incomplete);
    assert!(bundle.status_codes.contains(&R_SOLUTION_NOT_DELIVERABLE));
    assert!(!bundle.solution.deliverable);
    bundle
        .verify()
        .expect("best-effort artifact must self-verify");
}

// ===========================================================================
// 11. INCOMPLETE: provenance (measurement/calibration) missing
// ===========================================================================

#[test]
fn t54_artifact_incomplete_missing_provenance() {
    let fx = fixture();
    let bare = ArtifactProvenance::new(sha256_hex(b"t54-profile-binding"), None, None);
    let input = ArtifactInput::new(
        &fx.target,
        ArtifactSolution::Regularized(&fx.regularize),
        &fx.overlay,
        fx.tool.clone(),
        bare,
    )
    .with_post(&fx.baseline)
    .with_holdout(&fx.holdout)
    .with_design_plant_hash(fx.design_plant_hash.as_str());
    let bundle = build_artifact(&input).expect("incomplete provenance artifact");
    assert_eq!(bundle.status, ArtifactStatus::Incomplete);
    assert!(bundle
        .status_codes
        .contains(&R_MEASUREMENT_PROVENANCE_MISSING));
    assert!(bundle
        .status_codes
        .contains(&R_CALIBRATION_PROVENANCE_MISSING));
    assert!(bundle.identity.measurement_sha256.is_none());
    assert!(bundle.identity.calibration_sha256.is_none());
    bundle
        .verify()
        .expect("provenance-incomplete artifact must self-verify");
}

// ===========================================================================
// 12. holdout verdict -> FAIL / INCOMPLETE codes (recorded as-is, no fake PASS)
// ===========================================================================

#[test]
fn t54_artifact_holdout_verdict_codes() {
    let fx = fixture();
    let mut failed = fx.holdout.clone();
    failed.verdict = HoldoutVerdict::PredictedFail;
    let failed_bundle = build_artifact(&regularized_input(&fx, &failed)).expect("fail bundle");
    assert_eq!(failed_bundle.status, ArtifactStatus::Fail);
    assert_eq!(failed_bundle.status.as_str(), STATUS_FAIL);
    assert!(failed_bundle
        .status_codes
        .contains(&F_HOLDOUT_PREDICTED_FAIL));
    assert_eq!(failed_bundle.holdout.verdict, "PREDICTED_FAIL");
    failed_bundle
        .verify()
        .expect("FAIL artifact must self-verify");

    let mut incomplete = fx.holdout.clone();
    incomplete.verdict = HoldoutVerdict::Incomplete;
    let incomplete_bundle =
        build_artifact(&regularized_input(&fx, &incomplete)).expect("incomplete bundle");
    assert_eq!(incomplete_bundle.status, ArtifactStatus::Incomplete);
    assert!(incomplete_bundle
        .status_codes
        .contains(&R_HOLDOUT_INCOMPLETE));

    let mut not_run = fx.holdout.clone();
    not_run.verdict = HoldoutVerdict::NotRun;
    let not_run_bundle = build_artifact(&regularized_input(&fx, &not_run)).expect("not-run bundle");
    assert_eq!(not_run_bundle.status, ArtifactStatus::Incomplete);
    assert!(
        not_run_bundle.status_codes.contains(&R_HOLDOUT_INCOMPLETE)
            || not_run_bundle.status_codes.contains(&"R_HOLDOUT_NOT_RUN")
    );
}

// ===========================================================================
// 13. holdout binding mismatch / contract violation -> error (never silent)
// ===========================================================================

#[test]
fn t54_artifact_holdout_binding_mismatch_rejected() {
    let fx = fixture();
    let mut wrong_target = fx.holdout.clone();
    wrong_target.identity.target_sha256 = "0".repeat(64);
    let error = build_artifact(&regularized_input(&fx, &wrong_target))
        .expect_err("wrong target must be rejected");
    assert_eq!(error.kind(), ArtifactErrorKind::Holdout);
    assert_eq!(error.code(), "E_ART_HOLDOUT");

    let mut wrong_observed = fx.holdout.clone();
    wrong_observed.identity.design_plant_hash_observed = "1".repeat(64);
    let error = build_artifact(&regularized_input(&fx, &wrong_observed))
        .expect_err("wrong observed design plant hash must be rejected");
    assert_eq!(error.kind(), ArtifactErrorKind::Holdout);

    let mut contaminated = fx.holdout.clone();
    contaminated.applied = true;
    let error = build_artifact(&regularized_input(&fx, &contaminated))
        .expect_err("applied holdout report must be rejected");
    assert_eq!(error.kind(), ArtifactErrorKind::Status);

    let mut unsealed = fx.holdout.clone();
    unsealed.holdout_used_for_tuning = true;
    let error = build_artifact(&regularized_input(&fx, &unsealed))
        .expect_err("holdout-used-for-tuning report must be rejected");
    assert_eq!(error.kind(), ArtifactErrorKind::Holdout);
}

// ===========================================================================
// 14. solution / report tamper -> error (hash chain cannot pass silently)
// ===========================================================================

#[test]
fn t54_artifact_solution_tamper_rejected() {
    // f32 coefficient is not the canonical cast of f64 -> hash error.
    let mut fx = fixture();
    fx.solve.solution.as_mut().expect("solution").coeffs_f32[0] += 1.0;
    let error = build_artifact(&ArtifactInput::new(
        &fx.target,
        ArtifactSolution::Solve(&fx.solve),
        &fx.overlay,
        fx.tool.clone(),
        fx.provenance.clone(),
    ))
    .expect_err("tampered f32 coefficient must be rejected");
    assert_eq!(error.kind(), ArtifactErrorKind::Hash);

    // tampered coefficient canonical sha.
    let mut fx = fixture();
    fx.solve.solution.as_mut().expect("solution").coeffs_sha256 = "2".repeat(64);
    let error = build_artifact(&ArtifactInput::new(
        &fx.target,
        ArtifactSolution::Solve(&fx.solve),
        &fx.overlay,
        fx.tool.clone(),
        fx.provenance.clone(),
    ))
    .expect_err("tampered coeffs sha must be rejected");
    assert_eq!(error.kind(), ArtifactErrorKind::Hash);

    // D mismatch with target delay / D+1 > taps.
    let mut fx = fixture();
    fx.solve.report.delay_samples = 3;
    let error = build_artifact(&ArtifactInput::new(
        &fx.target,
        ArtifactSolution::Solve(&fx.solve),
        &fx.overlay,
        fx.tool.clone(),
        fx.provenance.clone(),
    ))
    .expect_err("D mismatch must be rejected");
    assert_eq!(error.kind(), ArtifactErrorKind::Delay);

    // taps mismatch with coefficient length.
    let mut fx = fixture();
    fx.solve.report.taps = 2;
    let error = build_artifact(&ArtifactInput::new(
        &fx.target,
        ArtifactSolution::Solve(&fx.solve),
        &fx.overlay,
        fx.tool.clone(),
        fx.provenance.clone(),
    ))
    .expect_err("taps mismatch must be rejected");
    assert_eq!(error.kind(), ArtifactErrorKind::Taps);

    // fs not a P1 design rate.
    let mut fx = fixture();
    fx.solve.report.sample_rate_hz = 44_100;
    let error = build_artifact(&ArtifactInput::new(
        &fx.target,
        ArtifactSolution::Solve(&fx.solve),
        &fx.overlay,
        fx.tool.clone(),
        fx.provenance.clone(),
    ))
    .expect_err("unsupported fs must be rejected");
    assert_eq!(error.kind(), ArtifactErrorKind::Fs);

    // malformed solution hash shape.
    let mut fx = fixture();
    fx.solve.solution.as_mut().expect("solution").hash = "not-a-hash".to_string();
    let error = build_artifact(&ArtifactInput::new(
        &fx.target,
        ArtifactSolution::Solve(&fx.solve),
        &fx.overlay,
        fx.tool.clone(),
        fx.provenance.clone(),
    ))
    .expect_err("bad solution hash must be rejected");
    assert_eq!(error.kind(), ArtifactErrorKind::Hash);
}

// ===========================================================================
// 15. overlay mapping errors -> reject (no hardcoded/silent mapping changes)
// ===========================================================================

#[test]
fn t54_artifact_overlay_mismatch_rejected() {
    let fx = fixture();

    // missing SUB1.
    let missing = OverlayDacBinding::new(
        P1_LAYOUT_4_1_2,
        sha256_hex(b"t54-overlay-missing"),
        8,
        vec![
            OverlayRole::new("FL", vec![0]).unwrap(),
            OverlayRole::new("FR", vec![1]).unwrap(),
            OverlayRole::new("SL", vec![2]).unwrap(),
            OverlayRole::new("SR", vec![3]).unwrap(),
            OverlayRole::new("HFL", vec![4]).unwrap(),
            OverlayRole::new("HFR", vec![5]).unwrap(),
        ],
    )
    .unwrap();
    let error = build_artifact(&input_with_overlay(&fx, &missing))
        .expect_err("missing SUB1 must be rejected");
    assert_eq!(error.kind(), ArtifactErrorKind::Overlay);

    // overlay first output differs from participant DAC (FL declared as 2).
    let swapped = OverlayDacBinding::new(
        P1_LAYOUT_4_1_2,
        sha256_hex(b"t54-overlay-swapped"),
        8,
        vec![
            OverlayRole::new("FL", vec![2]).unwrap(),
            OverlayRole::new("FR", vec![1]).unwrap(),
            OverlayRole::new("SL", vec![0]).unwrap(),
            OverlayRole::new("SR", vec![3]).unwrap(),
            OverlayRole::new("HFL", vec![4]).unwrap(),
            OverlayRole::new("HFR", vec![5]).unwrap(),
            OverlayRole::new("SUB1", vec![7]).unwrap(),
        ],
    )
    .unwrap();
    let error = build_artifact(&input_with_overlay(&fx, &swapped))
        .expect_err("swapped DAC must be rejected");
    assert_eq!(error.kind(), ArtifactErrorKind::Overlay);

    // ch6 used by an extra role -> 4.1.2 must reject instead of silently remapping.
    let ch6_used = OverlayDacBinding::new(
        P1_LAYOUT_4_1_2,
        sha256_hex(b"t54-overlay-ch6"),
        8,
        vec![
            OverlayRole::new("FL", vec![0]).unwrap(),
            OverlayRole::new("FR", vec![1]).unwrap(),
            OverlayRole::new("SL", vec![2]).unwrap(),
            OverlayRole::new("SR", vec![3]).unwrap(),
            OverlayRole::new("HFL", vec![4]).unwrap(),
            OverlayRole::new("HFR", vec![5]).unwrap(),
            OverlayRole::new("SUB1", vec![7]).unwrap(),
            OverlayRole::new("AUX", vec![6]).unwrap(),
        ],
    )
    .unwrap();
    let error =
        build_artifact(&input_with_overlay(&fx, &ch6_used)).expect_err("ch6 used must be rejected");
    assert_eq!(error.kind(), ArtifactErrorKind::Overlay);

    // layout mismatch.
    let wrong_layout = OverlayDacBinding::new(
        "5.3.2",
        sha256_hex(b"t54-overlay-layout"),
        8,
        vec![
            OverlayRole::new("FL", vec![0]).unwrap(),
            OverlayRole::new("FR", vec![1]).unwrap(),
            OverlayRole::new("SL", vec![2]).unwrap(),
            OverlayRole::new("SR", vec![3]).unwrap(),
            OverlayRole::new("HFL", vec![4]).unwrap(),
            OverlayRole::new("HFR", vec![5]).unwrap(),
            OverlayRole::new("SUB1", vec![7]).unwrap(),
        ],
    )
    .unwrap();
    let error = build_artifact(&input_with_overlay(&fx, &wrong_layout))
        .expect_err("layout mismatch must be rejected");
    assert_eq!(error.kind(), ArtifactErrorKind::Layout);

    // invalid overlay source sha256 (rejected at construction).
    let bad = OverlayDacBinding::new(
        P1_LAYOUT_4_1_2,
        "not-a-sha256",
        8,
        vec![OverlayRole::new("FL", vec![0]).unwrap()],
    )
    .expect_err("invalid overlay sha must be rejected");
    assert_eq!(bad.kind(), ArtifactErrorKind::Overlay);
}

// ===========================================================================
// 16. invalid provenance -> reject
// ===========================================================================

#[test]
fn t54_artifact_provenance_invalid_rejected() {
    let fx = fixture();
    let bad_profile = ArtifactProvenance::new("bad", None, None);
    let error = build_artifact(&ArtifactInput::new(
        &fx.target,
        ArtifactSolution::Regularized(&fx.regularize),
        &fx.overlay,
        fx.tool.clone(),
        bad_profile,
    ))
    .expect_err("bad profile hash must be rejected");
    assert_eq!(error.kind(), ArtifactErrorKind::Provenance);

    let bad_measurement =
        ArtifactProvenance::new(sha256_hex(b"t54-profile"), Some("XYZ".to_string()), None);
    let error = build_artifact(&ArtifactInput::new(
        &fx.target,
        ArtifactSolution::Regularized(&fx.regularize),
        &fx.overlay,
        fx.tool.clone(),
        bad_measurement,
    ))
    .expect_err("bad measurement hash must be rejected");
    assert_eq!(error.kind(), ArtifactErrorKind::Provenance);
}

// ===========================================================================
// 17. stale: per-field reasons + fixed order
// ===========================================================================

#[test]
fn t54_artifact_stale_reasons() {
    let fx = fixture();
    let bundle = regularized_artifact(&fx);
    let current = bundle.current_binding();
    assert!(bundle.stale_reasons(&current).is_empty());
    assert!(!bundle.is_stale(&current));

    fn expect_single(
        bundle: &ArtifactBundle,
        current: &ArtifactCurrent,
        mutate: impl FnOnce(&mut ArtifactCurrent),
        expected: ArtifactStaleReason,
    ) {
        let mut changed = current.clone();
        mutate(&mut changed);
        assert_eq!(
            bundle.stale_reasons(&changed),
            vec![expected],
            "expected {:?}",
            expected
        );
        assert!(bundle.is_stale(&changed));
    }

    expect_single(
        &bundle,
        &current,
        |value| value.sample_rate_hz = 96_000,
        ArtifactStaleReason::Fs,
    );
    expect_single(
        &bundle,
        &current,
        |value| value.layout_id = "5.3.2".to_string(),
        ArtifactStaleReason::Layout,
    );
    expect_single(
        &bundle,
        &current,
        |value| value.participants_binding_hash = "0".repeat(64),
        ArtifactStaleReason::Participants,
    );
    expect_single(
        &bundle,
        &current,
        |value| value.role_hash = "1".repeat(64),
        ArtifactStaleReason::Roles,
    );
    expect_single(
        &bundle,
        &current,
        |value| value.overlay_source_sha256 = "2".repeat(64),
        ArtifactStaleReason::Overlay,
    );
    expect_single(
        &bundle,
        &current,
        |value| value.post_baseline_hash = "3".repeat(64),
        ArtifactStaleReason::PostBaseline,
    );
    expect_single(
        &bundle,
        &current,
        |value| value.profile_sha256 = "4".repeat(64),
        ArtifactStaleReason::Profile,
    );
    expect_single(
        &bundle,
        &current,
        |value| value.target_sha256 = "5".repeat(64),
        ArtifactStaleReason::Target,
    );
    expect_single(
        &bundle,
        &current,
        |value| value.solution_hash = "6".repeat(64),
        ArtifactStaleReason::Solution,
    );
    expect_single(
        &bundle,
        &current,
        |value| value.prior_hash = Some("7".repeat(64)),
        ArtifactStaleReason::Prior,
    );
    expect_single(
        &bundle,
        &current,
        |value| value.taps += 1,
        ArtifactStaleReason::Taps,
    );
    expect_single(
        &bundle,
        &current,
        |value| value.delay_samples += 1,
        ArtifactStaleReason::Delay,
    );
    expect_single(
        &bundle,
        &current,
        |value| value.holdout_report_hash = Some("8".repeat(64)),
        ArtifactStaleReason::Holdout,
    );

    // multiple changes -> fixed order (Fs/Layout/.../Target).
    let mut changed = current.clone();
    changed.sample_rate_hz = 96_000;
    changed.target_sha256 = "9".repeat(64);
    changed.taps += 1;
    assert_eq!(
        bundle.stale_reasons(&changed),
        vec![
            ArtifactStaleReason::Fs,
            ArtifactStaleReason::Target,
            ArtifactStaleReason::Taps
        ]
    );
    assert_eq!(ArtifactStaleReason::ALL.len(), 13);
}

// ===========================================================================
// 18. write_to_dir (temp dir; read-back verification; cleanup)
// ===========================================================================

#[test]
fn t54_artifact_write_to_dir() {
    let fx = fixture();
    let bundle = regularized_artifact(&fx);
    // R:\Temp may be read-only in this workspace; fall back to the build tree when the system
    // temp dir is not writable. Neither location is inside the repository source tree.
    let name = format!("khaen-t54-artifact-{}", std::process::id());
    let temp_candidate = std::env::temp_dir().join(&name);
    let dir = if fs::create_dir_all(&temp_candidate).is_ok() {
        temp_candidate
    } else {
        let fallback = std::env::current_dir()
            .expect("current dir")
            .join("target")
            .join(&name);
        fs::create_dir_all(&fallback).expect("fallback test dir");
        fallback
    };
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("clean temp dir");
    }
    let written = bundle.write_to_dir(&dir).expect("write artifact");
    assert_eq!(written.files.len(), bundle.files.len());
    assert_eq!(written.manifest_path, dir.join(ARTIFACT_MANIFEST_FILE));
    assert_eq!(
        written.total_bytes,
        bundle
            .files
            .iter()
            .map(|file| file.bytes.len() as u64)
            .sum::<u64>()
    );
    for file in &bundle.files {
        let path = dir.join(&file.rel_path);
        let from_disk = fs::read(&path).expect("written file must exist");
        assert_eq!(
            from_disk, file.bytes,
            "disk bytes differ for {}",
            file.rel_path
        );
    }
    assert!(written.manifest_path.exists());
    let second = dir.join("second");
    let written_second = bundle.write_to_dir(&second).expect("second write");
    assert_eq!(written_second.total_bytes, written.total_bytes);
    for file in &bundle.files {
        let first_bytes = fs::read(dir.join(&file.rel_path)).expect("first");
        let second_bytes = fs::read(second.join(&file.rel_path)).expect("second");
        assert_eq!(first_bytes, second_bytes);
    }
    fs::remove_dir_all(&dir).expect("cleanup temp dir");
}

// ===========================================================================
// 19. verify negative controls: tampering must be detected
// ===========================================================================

#[test]
fn t54_artifact_verify_negative_controls() {
    let fx = fixture();
    let bundle = regularized_artifact(&fx);
    bundle.verify().expect("baseline verify");

    let mut tampered_wav = bundle.clone();
    tampered_wav.files[1].bytes[44] ^= 0xff;
    let error = tampered_wav
        .verify()
        .expect_err("tampered WAV must fail verify");
    assert_eq!(error.kind(), ArtifactErrorKind::Hash);

    let mut tampered_manifest = bundle.clone();
    tampered_manifest.files[0].bytes.push(b' ');
    let error = tampered_manifest
        .verify()
        .expect_err("tampered manifest must fail verify");
    assert_eq!(error.kind(), ArtifactErrorKind::Hash);

    let mut missing_file = bundle.clone();
    missing_file.files.pop();
    let error = missing_file
        .verify()
        .expect_err("missing coefficient file must fail verify");
    assert_eq!(error.kind(), ArtifactErrorKind::Hash);

    let mut dirty_hash = bundle.clone();
    dirty_hash.artifact_hash = "f".repeat(64);
    let error = dirty_hash
        .verify()
        .expect_err("tampered artifact hash must fail verify");
    assert_eq!(error.kind(), ArtifactErrorKind::Hash);

    // honesty tamper with recomputed hashes/manifest: verify must still reject (status check).
    let mut dishonest = bundle.clone();
    dishonest.honesty.applied = true;
    dishonest.artifact_hash = sha256_hex(dishonest.canonical().as_bytes());
    let manifest = dishonest.to_manifest_json().into_bytes();
    dishonest.manifest_sha256 = sha256_hex(&manifest);
    dishonest.files[0].bytes = manifest;
    let error = dishonest
        .verify()
        .expect_err("applied honesty must fail verify");
    assert_eq!(error.kind(), ArtifactErrorKind::Status);
}

// ===========================================================================
// 20. WAV codec contract (f32 mono / 44B header / strict rejects)
// ===========================================================================

#[test]
fn t54_wav_codec_contract() {
    let samples = vec![1.0f32, -0.5, 0.0, 0.25];
    let bytes = encode_wav_f32_mono(48_000, &samples).expect("encode");
    assert_eq!(bytes.len(), 44 + 16);
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(&bytes[12..16], b"fmt ");
    assert_eq!(&bytes[36..40], b"data");
    let decoded = decode_wav_f32_mono(&bytes).expect("decode");
    assert_eq!(decoded.sample_rate_hz, 48_000);
    assert_eq!(decoded.samples.len(), samples.len());
    for (left, right) in decoded.samples.iter().zip(samples.iter()) {
        assert_eq!(left.to_bits(), right.to_bits());
    }
    assert_eq!(
        bytes,
        encode_wav_f32_mono(48_000, &samples).expect("encode again")
    );
    let error = encode_wav_f32_mono(48_000, &[f32::NAN]).expect_err("NaN must be rejected");
    assert_eq!(error.kind(), ArtifactErrorKind::Wav);
    let error = encode_wav_f32_mono(48_000, &[]).expect_err("empty must be rejected");
    assert_eq!(error.kind(), ArtifactErrorKind::Wav);
    let mut pcm = bytes.clone();
    pcm[20] = 1;
    pcm[21] = 0;
    let error = decode_wav_f32_mono(&pcm).expect_err("PCM int must be rejected");
    assert_eq!(error.kind(), ArtifactErrorKind::Wav);
    let error = decode_wav_f32_mono(&bytes[..40]).expect_err("truncated must be rejected");
    assert_eq!(error.kind(), ArtifactErrorKind::Wav);
    let mut short_data = bytes.clone();
    short_data[40] -= 4;
    let error = decode_wav_f32_mono(&short_data).expect_err("data size mismatch must be rejected");
    assert_eq!(error.kind(), ArtifactErrorKind::Wav);
    let mut nan_bytes = encode_wav_f32_mono(48_000, &[0.0]).expect("encode");
    nan_bytes[44..48].copy_from_slice(&f32::NAN.to_le_bytes());
    let error = decode_wav_f32_mono(&nan_bytes).expect_err("NaN sample must be rejected");
    assert_eq!(error.kind(), ArtifactErrorKind::Wav);
}

// ===========================================================================
// 21. stereo strict independent 2x2 profile: stereo mask exactly FL/FR
// ===========================================================================

struct StereoFixture {
    baseline: FrozenPostBaseline,
    design_plant_hash: String,
    target: TargetMatrix,
    solve: khaen_modal_design::solve::SolveOutcome,
    holdout: HoldoutReport,
    overlay: OverlayDacBinding,
    tool: ArtifactTool,
    provenance: ArtifactProvenance,
}

fn stereo_fixture() -> StereoFixture {
    let grid = bass_grid();
    let participants = ModalParticipants::stereo_strict();
    let baseline = make_baseline(&grid, participants.clone(), "t54-stereo-baseline");
    let design_positions: Vec<RawPosition> = STEREO_ROWS
        .iter()
        .enumerate()
        .map(|(index, row)| {
            raw_position_row(
                &format!("S{:02}", index + 1),
                PositionRole::Design,
                &participants,
                &grid,
                &row[..],
            )
        })
        .collect();
    let plant = make_plant(&baseline, &participants, &grid, design_positions);
    let spec = TargetSpec::stereo_strict(&baseline, reduced_decay(0.85)).unwrap();
    let target = build_stereo_strict_target(&baseline, &plant, &spec).unwrap();
    let solve = solve_outcome(&target, &plant);
    let mut holdout_positions = vec![raw_position_row(
        "SPH00",
        PositionRole::Design,
        &participants,
        &grid,
        &[0.6, 0.4],
    )];
    let holdout_rows: [[f64; 2]; 3] = [[0.5, 0.5], [0.7, 0.3], [0.6, 0.4]];
    for (index, row) in holdout_rows.iter().enumerate() {
        holdout_positions.push(raw_position_row(
            &format!("H{:02}", index + 1),
            PositionRole::Validation,
            &participants,
            &grid,
            &row[..],
        ));
    }
    let holdout_plant = make_plant(&baseline, &participants, &grid, holdout_positions);
    let solution = solve.solution.as_ref().expect("stereo solve solution");
    let declaration_labels: Vec<String> = plant
        .design_positions()
        .map(|position| position.label.clone())
        .collect();
    let request = HoldoutRequest::new(
        TuningDeclaration::new(
            declaration_labels.clone(),
            HOLDOUT_LABELS
                .iter()
                .map(|label| (*label).to_string())
                .collect(),
        )
        .with_tuning(declaration_labels, TUNING_INPUTS_SHA256)
        .with_seal(true),
    )
    .with_repeats(make_repeats(&participants, &grid, 1.0e-3))
    .with_ir(make_band_ir(0.40, 0.32), IrOrigin::PredictedModel);
    let holdout =
        validate_holdout(&target, &plant, &holdout_plant, &solution.modal, &request).unwrap();
    StereoFixture {
        baseline,
        design_plant_hash: plant.hash(),
        target,
        solve,
        holdout,
        overlay: overlay_binding(),
        tool: tool(),
        provenance: provenance(),
    }
}

#[test]
fn t54_artifact_stereo_strict_mask() {
    let fx = stereo_fixture();
    assert_eq!(fx.target.m(), 2);
    assert!(fx.target.participants().is_stereo_strict());
    let input = ArtifactInput::new(
        &fx.target,
        ArtifactSolution::Solve(&fx.solve),
        &fx.overlay,
        fx.tool.clone(),
        fx.provenance.clone(),
    )
    .with_post(&fx.baseline)
    .with_holdout(&fx.holdout)
    .with_design_plant_hash(fx.design_plant_hash.as_str());
    let bundle = build_artifact(&input).expect("stereo strict artifact must build");
    assert_eq!(
        bundle.scope.stereo_mask,
        STEREO_STRICT_OUTPUT_ROLE_IDS
            .iter()
            .map(|role| (*role).to_string())
            .collect::<Vec<String>>()
    );
    assert!(bundle.scope.stereo_strict);
    assert_eq!(bundle.scope.m, 2);
    assert_eq!(bundle.scope.dense_to_dac, vec![0, 1]);
    assert_eq!(bundle.paths.len(), 4);
    assert_eq!(bundle.scope.participants.len(), 2);
    for participant in &bundle.scope.participants {
        assert!(STEREO_STRICT_OUTPUT_ROLE_IDS.contains(&participant.role_id.as_str()));
    }
    assert!(!bundle.design.height_scope.present);
    assert_eq!(bundle.identity.solution_kind, "solve");
    bundle
        .verify()
        .expect("stereo strict artifact must self-verify");
    // 5x5 main fixture has an empty stereo mask (the two profiles are not mixed).
    let fx5 = fixture();
    let bundle5 = regularized_artifact(&fx5);
    assert!(!bundle5.scope.stereo_strict);
    assert!(bundle5.scope.stereo_mask.is_empty());
}

// ===========================================================================
// 22. static guards (NONRT in-memory build; no threads/process/env/net/unsafe)
// ===========================================================================

#[test]
fn t54_artifact_static_guards() {
    let source = include_str!("../../src/artifact.rs");
    for required in [
        "pub fn build_artifact",
        "pub fn encode_wav_f32_mono",
        "pub fn decode_wav_f32_mono",
        "pub fn to_manifest_json",
        "pub fn verify",
        "pub fn write_to_dir",
        "role_hash",
        "stale_reasons",
        "INCOMPLETE",
        "PREDICTED_FAIL",
        "holdout_used_for_tuning",
        "wav_f32_mono",
        "stereo_mask",
        "overlay",
        "ARTIFACT_SCHEMA",
    ] {
        assert!(
            source.contains(required),
            "artifact.rs missing required semantic probe {required}"
        );
    }
    for forbidden in [
        "unsafe {",
        "std::thread",
        "std::process",
        "std::net",
        "std::env",
        "std::time",
        "serde",
        "rand::",
        "panic!",
        "unwrap()",
        ".expect(",
        "println!",
        "dbg!",
    ] {
        assert!(
            !source.contains(forbidden),
            "artifact.rs must not contain forbidden probe {forbidden}"
        );
    }
    assert!(source.contains("pub fn write_to_dir"));
    assert!(source.contains("std::fs"));
}

// ===========================================================================
// 23. known fixture dump (evidence audit; run with --nocapture)
// ===========================================================================

#[test]
fn t54_artifact_known_fixture_dump() {
    let fx = fixture();
    let bundle = regularized_artifact(&fx);
    let solved = solved_artifact(&fx);
    let manifest = bundle.to_manifest_json();
    JsonSyntax::validate(&manifest).expect("dump manifest JSON");
    println!("T54_ARTIFACT_FIXTURE_BEGIN");
    println!(
        "SUMMARY status={} codes={:?} artifact_hash={} manifest_sha256={} paths={} files={} wav_bytes={}",
        bundle.status.as_str(),
        bundle.status_codes,
        bundle.artifact_hash,
        bundle.manifest_sha256,
        bundle.paths.len(),
        bundle.files.len(),
        bundle.quantization.wav_aggregate_sha256
    );
    println!(
        "INPUTS target_sha256={} baseline_hash={} solution_hash={} holdout_report_hash={} role_hash={} overlay_source_sha256={}",
        bundle.identity.target_sha256,
        bundle.identity.baseline_hash,
        bundle.identity.solution_hash.as_deref().unwrap_or("none"),
        bundle
            .identity
            .holdout_report_hash
            .as_deref()
            .unwrap_or("none"),
        bundle.scope.role_hash,
        bundle.scope.overlay.source_sha256
    );
    println!(
        "PARTICIPANTS dense_to_dac={:?} fs={} taps={} D={} stereo_strict={} stereo_mask={:?}",
        bundle.scope.dense_to_dac,
        bundle.design.fs,
        bundle.design.taps,
        bundle.design.delay_samples,
        bundle.scope.stereo_strict,
        bundle.scope.stereo_mask
    );
    for path in &bundle.paths {
        println!(
            "PATH out={} in={} {} sha256={} bytes={} diag={} delay={} zero={} peak={}",
            path.out_dense,
            path.in_dense,
            path.file,
            path.sha256,
            path.bytes,
            path.diag_baseline,
            path.delay_samples,
            path.zero,
            path.peak_abs
        );
    }
    println!("SOLVE_ARTIFACT_HASH {}", solved.artifact_hash);
    println!("T54_ARTIFACT_FIXTURE_END");
}
