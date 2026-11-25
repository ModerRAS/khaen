//! T50 private integration tests — 模态目标构建（crate tspi-modal-design）.
//!
//! 运行（在 rt-stack/ 下）：cargo test -p tspi-modal-design target
//! 文件位置：crates/modal-design/tests/T50/main.rs（leaf 私有测试；Cargo 自动发现为
//! integration test target T50；不写 public_registration.rs，由 integrator 决定注册方式）。
//!
//! 覆盖（WBS T50 DoD）：
//! - participants 显式 5（FL/FR/SL/SR/SUB1 → DAC 0/1/2/3/7）；从 T48 session 显式声明构造并拒绝推断；
//! - 拟合后冻结 P：移动语义 freeze、hash 确定性/时间戳不入 hash、stale_reasons（fit/fs/tone/
//!   participants/grid）、极性 ±1/delay/增益/带限/人工 delay/PEQ 契约拒收；
//! - H_eff = H_raw x P：唯一复乘点、闭式独立复算、极性恰好归一化一次（double → E_TGT_POLARITY）、
//!   网格/绑定/fs 不一致拒收、只扣 T49 D0（measured delay 只作 provenance，不双扣）；
//! - 真实 5x5 目标：preserve（per-position = H_eff）与 reduce（position-invariant 功率平滑聚合、
//!   T*_60 = ratio * 基线 T60、T20 >=15% 目标口径、软帽/硬帽状态、T6 §3.3 覆盖率）；
//! - holdout 隔离：validation position 数据不参与目标（两份仅 holdout 不同的 plant → 同一 target hash）；
//! - baseline tone 单次应用：ToneApplicationProof (1,0,0)、target 继承不重算、残留 curve 拒收；
//! - 高度全频 scope 分离：HFL/HFR 全频/无 HP-LP/无人工 delay/不进低带矩阵；
//! - stereo strict 独立 profile：2x2、FL/FR 输出集合、独立 hash domain、禁止复用 5x5 目标/G；
//! - 求解矩阵校验：尺寸/角色/对角 baseline/预测残差；错误码稳定性；证据 JSON 与静态守卫（无 I/O）。
//!
//! 本文件只构造内存夹具；不采音、不碰板端、不打开 ALSA、不写文件。

use tspi_modal_design::ingest::{
    sha256_hex, AnalysisMeta, CaptureInfo, FrPoint, IngestSession, MicInfo, PlaybackInfo,
    PositionRole, ReferenceInfo, SampleFormat, SourceAnalysis, SpeakerInfo, StimulusInfo, ToolInfo,
};
use tspi_modal_design::target::{
    build_stereo_strict_target, build_target_matrix, validate_solution,
    validate_stereo_strict_solution, BandDecay, BandLimit, CurveApplySite, CurveBinding, DecayMode,
    DecayTarget, EffectivePlant, FittedPostBaseline, ModalMatrix, ModalMatrixCell,
    ModalParticipants, PostBaselineInput, PostRoleFit, RawPlant, RawPosition, RawResponse,
    TargetCellView, TargetError, TargetErrorKind, TargetProfile, TargetSpec, TargetStatus,
    ToneApplicationProof, ToneBinding, ToneShape, P1_LAYOUT_4_1_2, P1_MIMO_PARTICIPANT_IDS,
    TARGET_SCHEMA,
};
use tspi_modal_design::timebase::{
    de_phase_analysis, CommonD0, ComplexPoint, PolaritySource, ResolvedPolarity, SlotPhase,
};

// ===========================================================================
// 常量与夹具辅助
// ===========================================================================

const FS: u32 = 48_000;
const D0_SAMPLES: i64 = 120;
const DEFAULT_T60_63: f64 = 0.40;
const DEFAULT_T60_125: f64 = 0.35;

fn grid_1_24_oct() -> Vec<f64> {
    let mut freqs = Vec::new();
    let mut index = 0i32;
    loop {
        let freq = 20.0 * 2f64.powf(index as f64 / 24.0);
        if freq >= 20_000.0 {
            break;
        }
        freqs.push(freq);
        index += 1;
    }
    freqs.push(20_000.0);
    freqs
}

fn small_grid() -> Vec<f64> {
    vec![
        20.0, 31.5, 40.0, 63.0, 80.0, 125.0, 160.0, 250.0, 320.0, 640.0, 1_000.0, 2_000.0,
    ]
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

fn room_points(grid: &[f64], base_gain_db: f64, peak_db: f64) -> Vec<ComplexPoint> {
    grid.iter()
        .map(|freq| {
            let octave_offset = (freq / 120.0).log2();
            let bump = peak_db * (-(octave_offset * octave_offset) / (2.0 * 0.03 * 0.03)).exp();
            let magnitude = 10f64.powf((base_gain_db + bump) / 20.0);
            ComplexPoint {
                freq_hz: *freq,
                re: magnitude,
                im: 0.0,
            }
        })
        .collect()
}

fn notch_points(grid: &[f64], base_gain_db: f64, notch_db: f64) -> Vec<ComplexPoint> {
    grid.iter()
        .map(|freq| {
            let octave_offset = (freq / 120.0).log2();
            let dip = notch_db * (-(octave_offset * octave_offset) / (2.0 * 0.03 * 0.03)).exp();
            let magnitude = 10f64.powf((base_gain_db + dip) / 20.0);
            ComplexPoint {
                freq_hz: *freq,
                re: magnitude,
                im: 0.0,
            }
        })
        .collect()
}

fn polarity(sign: i8) -> ResolvedPolarity {
    ResolvedPolarity::resolve(sign as i32, PolaritySource::Analysis).unwrap()
}

fn target_error<T>(result: Result<T, TargetError>) -> TargetError {
    match result {
        Ok(_) => panic!("expected TargetError, got Ok"),
        Err(error) => error,
    }
}

fn expect_kind<T>(result: Result<T, TargetError>, kind: TargetErrorKind) -> TargetError {
    let error = target_error(result);
    assert_eq!(error.kind(), kind, "unexpected error kind: {error}");
    error
}

fn assert_close(actual: f64, expected: f64, tolerance: f64, context: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{context}: actual {actual} vs expected {expected} (tol {tolerance})"
    );
}

fn assert_complex_close(
    actual: &ComplexPoint,
    expected: (f64, f64),
    tolerance: f64,
    context: &str,
) {
    assert_close(actual.re, expected.0, tolerance, &format!("{context}.re"));
    assert_close(actual.im, expected.1, tolerance, &format!("{context}.im"));
}

fn default_decay() -> DecayTarget {
    DecayTarget::preserve(vec![
        BandDecay {
            band_hz: 63.0,
            baseline_t60_s: DEFAULT_T60_63,
        },
        BandDecay {
            band_hz: 125.0,
            baseline_t60_s: DEFAULT_T60_125,
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
                baseline_t60_s: DEFAULT_T60_63,
            },
            BandDecay {
                band_hz: 125.0,
                baseline_t60_s: DEFAULT_T60_125,
            },
        ],
    )
    .unwrap()
}

fn post_role(
    role_id: &str,
    out_index: u32,
    grid: &[f64],
    gain_db: f64,
    delay_samples: u32,
    polarity_sign: i8,
) -> PostRoleFit {
    PostRoleFit::analytic(
        role_id,
        out_index,
        FS,
        grid,
        None,
        delay_samples,
        polarity_sign,
        gain_db,
    )
    .unwrap()
}

fn baseline_roles(
    grid: &[f64],
    participants: &ModalParticipants,
    gain_db: f64,
    delay_samples: u32,
    polarity_sign: i8,
) -> Vec<PostRoleFit> {
    let mut roles = Vec::new();
    for participant in participants.participants() {
        roles.push(post_role(
            &participant.role_id,
            participant.out_index,
            grid,
            gain_db,
            delay_samples,
            polarity_sign,
        ));
    }
    if participants.is_p1_explicit() {
        roles.push(post_role("HFL", 4, grid, 0.0, 0, 1));
        roles.push(post_role("HFR", 5, grid, 0.0, 0, 1));
    }
    roles
}

fn try_fitted_baseline_with_roles(
    grid: &[f64],
    participants: ModalParticipants,
    tone: ToneBinding,
    roles: Vec<PostRoleFit>,
) -> Result<FittedPostBaseline, TargetError> {
    let curve = CurveBinding::baseline_only(tone.curve_id()).unwrap();
    let input = PostBaselineInput {
        baseline_id: "t50-baseline-a".to_string(),
        sample_rate_hz: FS,
        participants,
        tone,
        curve,
        tone_applications: ToneApplicationProof::baseline_only(),
        fit_inputs_sha256: sha256_hex(b"t50-fit-inputs-v1"),
        common_d0: CommonD0::new(D0_SAMPLES).unwrap(),
        grid_hz: grid.to_vec(),
        roles,
        created_unix: None,
    };
    FittedPostBaseline::new(input)
}

fn frozen_baseline_with_roles(
    grid: &[f64],
    participants: ModalParticipants,
    tone: ToneBinding,
    roles: Vec<PostRoleFit>,
) -> FittedPostBaseline {
    try_fitted_baseline_with_roles(grid, participants, tone, roles).unwrap()
}

fn default_baseline(grid: &[f64]) -> tspi_modal_design::target::FrozenPostBaseline {
    let participants = ModalParticipants::p1_4_1_2();
    let roles = baseline_roles(grid, &participants, 0.0, 0, 1);
    frozen_baseline_with_roles(grid, participants, ToneBinding::flat(), roles)
        .freeze()
        .unwrap()
}

fn normalized_response(
    role_id: &str,
    out_index: u32,
    points: Vec<ComplexPoint>,
    invert: bool,
) -> RawResponse {
    RawResponse::new(role_id, out_index, points)
        .unwrap()
        .normalize_polarity(polarity(if invert { -1 } else { 1 }))
        .unwrap()
}

fn raw_position_with(
    label: &str,
    role: PositionRole,
    weight: f64,
    participants: &ModalParticipants,
    grid: &[f64],
    points_for: impl Fn(&str, &[f64]) -> Vec<ComplexPoint>,
    inverted: &[&str],
) -> RawPosition {
    let mut responses = Vec::new();
    for participant in participants.participants() {
        let points = points_for(&participant.role_id, grid);
        let invert = inverted.contains(&participant.role_id.as_str());
        responses.push(normalized_response(
            &participant.role_id,
            participant.out_index,
            points,
            invert,
        ));
    }
    RawPosition::new(label, role, weight, responses).unwrap()
}

fn identity_participants_session() -> IngestSession {
    let speakers = vec![
        speaker("FL", 0, "main"),
        speaker("FR", 1, "main"),
        speaker("SL", 2, "surround"),
        speaker("SR", 3, "surround"),
        speaker("HFL", 4, "height"),
        speaker("HFR", 5, "height"),
        speaker("SUB1", 7, "subwoofer"),
    ];
    session("4.1.2", P1_MIMO_PARTICIPANT_IDS.to_vec(), speakers)
}

fn speaker(name: &str, out_index: u32, kind: &str) -> SpeakerInfo {
    SpeakerInfo {
        name: name.to_string(),
        out_index,
        kind: kind.to_string(),
        allowed_band_hz: None,
    }
}

fn session(layout: &str, participants: Vec<&str>, speakers: Vec<SpeakerInfo>) -> IngestSession {
    IngestSession {
        session_id: "t50-session".to_string(),
        layout: layout.to_string(),
        sample_rate_hz: FS,
        block_frames: Some(128),
        participants: participants.into_iter().map(str::to_string).collect(),
        speakers,
        mic: MicInfo {
            model: "UMIK-1".to_string(),
            serial: "7001234".to_string(),
            cal_sha256: "a".repeat(64),
            orientation: "90deg".to_string(),
            clock_domain: "usb".to_string(),
        },
        playback: PlaybackInfo {
            device: "hw:motu,0".to_string(),
            channels: 8,
            sample_format: SampleFormat::S32LE,
        },
        capture: CaptureInfo {
            device: "hw:umik1,0".to_string(),
            channels: 1,
            sample_format: SampleFormat::S24_3LE,
            pre_seconds: 0.5,
            post_seconds: 1.0,
        },
        reference: Some(ReferenceInfo {
            device: "hw:motu,1".to_string(),
            channels: 1,
            sample_format: SampleFormat::S32LE,
            independent: true,
        }),
        stimulus: StimulusInfo {
            kind: "log_sweep".to_string(),
            start_hz: 20.0,
            end_hz: 20_000.0,
            seconds: 5.0,
            level_dbfs: -12.0,
            fade_ms: 20.0,
            repeats: 3,
            repeat_gap_seconds: 1.0,
        },
        room_temperature_c: 22.0,
        tool: ToolInfo {
            name: "tspi-audio-daemon".to_string(),
            version: "0.1.0".to_string(),
        },
        created_unix: None,
        claim: None,
    }
}

// T49 集成夹具：闭式相位 + 137.25 sample 延迟 + -1 极性 + D0 去相位一次。
fn phase0_deg(freq_hz: f64) -> f64 {
    2.0 * (freq_hz / 100.0).sqrt()
}

fn t49_source_analysis(grid: &[f64], delay_samples: f64, polarity_sign: i32) -> SourceAnalysis {
    let fr = grid
        .iter()
        .map(|freq| FrPoint {
            freq_hz: *freq,
            db: -12.0 + 0.0 * freq,
            phase_deg: phase0_deg(*freq) + 360.0 * freq * delay_samples / FS as f64,
        })
        .collect();
    SourceAnalysis {
        channel: 0,
        delay_samples,
        delay_ms: delay_samples / FS as f64 * 1000.0,
        delay_coarse_samples: delay_samples.floor() as i64,
        polarity: polarity_sign,
        peak_db: -12.0,
        phase_1k_deg: 0.0,
        snr_db: 35.0,
        snr_band_db: Some(32.0),
        snr_band_hz: Some((63.0, 250.0)),
        phase_ref:
            "aligned_segment(已去粗延迟); absolute_phase_deg = phase_deg - 360*f*delay_samples/fs"
                .to_string(),
        clock_ppm: Some(7.5),
        clock_ppm_ok: Some(true),
        clock_ppm_uncertainty_ppm: Some(0.2),
        clock_ppm_significant: Some(false),
        drift_corrected: Some(true),
        onset_invalid: Some(false),
        measurement_role: None,
        position: None,
        repeats_count: Some(3),
        fr,
        fr_modal: Vec::new(),
        meta: AnalysisMeta {
            sample_rate_hz: FS,
            playback_channels: 8,
            sweep_start_hz: 20.0,
            sweep_end_hz: 20_000.0,
            sweep_seconds: 5.0,
            level_dbfs: -12.0,
            pre_seconds: 0.5,
            post_seconds: 1.0,
            playback_device: "hw:motu,0".to_string(),
            capture_channels: 1,
            capture_device: "hw:umik1,0".to_string(),
            created_unix: 1_700_000_000,
        },
    }
}

fn t49_slot_phases(
    participants: &ModalParticipants,
    grid: &[f64],
    delay_samples: f64,
    polarity_sign: i32,
) -> Vec<SlotPhase> {
    let common_d0 = CommonD0::new(D0_SAMPLES).unwrap();
    participants
        .participants()
        .iter()
        .enumerate()
        .map(|(slot, participant)| {
            let mut analysis = t49_source_analysis(grid, delay_samples, polarity_sign);
            analysis.channel = participant.out_index;
            let absolute =
                de_phase_analysis(&analysis, common_d0, format!("P01/{}", participant.role_id))
                    .unwrap();
            SlotPhase {
                slot: slot as u32,
                speaker: participant.role_id.clone(),
                out_index: participant.out_index,
                polarity: policy_polarity(polarity_sign),
                absolute,
            }
        })
        .collect()
}

fn policy_polarity(sign: i32) -> ResolvedPolarity {
    ResolvedPolarity::resolve(sign, PolaritySource::Analysis).unwrap()
}

// ===========================================================================
// participants / height scope
// ===========================================================================

#[test]
fn t50_target_participants_explicit_five_and_dac_mapping() {
    let participants = ModalParticipants::p1_4_1_2();
    assert_eq!(participants.m(), 5);
    assert_eq!(
        participants.role_ids(),
        vec!["FL", "FR", "SL", "SR", "SUB1"]
    );
    assert_eq!(participants.dense_to_out_index(), vec![0, 1, 2, 3, 7]);
    assert!(participants.is_p1_explicit());
    assert!(!participants.is_stereo_strict());
    assert_eq!(participants.dense_index_of("SUB1"), Some(4));
    assert_eq!(participants.out_index_of("SL"), Some(2));
    assert_eq!(
        participants.participants().len(),
        P1_MIMO_PARTICIPANT_IDS.len()
    );

    let canonical = participants.canonical();
    assert!(canonical.contains("dense4=SUB1:dac7"));
    let hash = participants.binding_hash();
    assert_eq!(hash.len(), 64);
    assert_eq!(hash, ModalParticipants::p1_4_1_2().binding_hash());

    let stereo = ModalParticipants::stereo_strict();
    assert_eq!(stereo.m(), 2);
    assert!(stereo.is_stereo_strict());
    assert!(!stereo.is_p1_explicit());
    assert_ne!(stereo.binding_hash(), hash);

    // 重复角色 / 重复 DAC / 高度进矩阵 / 空列表 / 超上限均拒收。
    expect_kind(
        ModalParticipants::new("4.1.2", vec![("FL".to_string(), 0), ("FL".to_string(), 1)]),
        TargetErrorKind::Participants,
    );
    expect_kind(
        ModalParticipants::new("4.1.2", vec![("FL".to_string(), 0), ("FR".to_string(), 0)]),
        TargetErrorKind::Participants,
    );
    expect_kind(
        ModalParticipants::new("4.1.2", vec![("HFL".to_string(), 4)]),
        TargetErrorKind::Scope,
    );
    expect_kind(
        ModalParticipants::new("4.1.2", vec![]),
        TargetErrorKind::Missing,
    );
    let too_many: Vec<(String, u32)> = (0..9).map(|index| (format!("R{index}"), index)).collect();
    expect_kind(
        ModalParticipants::new("4.1.2", too_many),
        TargetErrorKind::Participants,
    );
    expect_kind(
        ModalParticipants::new("4.1.2", vec![("fl".to_string(), 0)]),
        TargetErrorKind::Participants,
    );
}

#[test]
fn t50_target_participants_from_session_explicit_scope_only() {
    let participants = ModalParticipants::from_session(&identity_participants_session()).unwrap();
    assert!(participants.is_p1_explicit());
    assert_eq!(participants.layout_id(), P1_LAYOUT_4_1_2);

    // layout 非 4.1.2 → Unsupported（5.3.2/M=8 未实现）。
    let mut wrong_layout = identity_participants_session();
    wrong_layout.layout = "5.3.2".to_string();
    expect_kind(
        ModalParticipants::from_session(&wrong_layout),
        TargetErrorKind::Unsupported,
    );

    // participants 顺序/集合必须等于显式任务边界（不从角色名/频响推断）。
    let mut wrong_scope = identity_participants_session();
    wrong_scope.participants = vec![
        "FL".to_string(),
        "FR".to_string(),
        "SL".to_string(),
        "SR".to_string(),
    ];
    expect_kind(
        ModalParticipants::from_session(&wrong_scope),
        TargetErrorKind::Participants,
    );

    // DAC 槽位必须按 overlay 解析为 0/1/2/3/7（硬编码错误映射拒收）。
    let mut wrong_dac = identity_participants_session();
    wrong_dac.speakers[4].out_index = 4;
    wrong_dac.speakers[6].out_index = 6;
    expect_kind(
        ModalParticipants::from_session(&wrong_dac),
        TargetErrorKind::Participants,
    );

    // 高度角色进入低带 participants → Scope 拒收。
    // 高度替代 SUB1 会先违反"participants 必须等于显式任务边界"，高度进矩阵另由 Scope 拦截（见上一条测试）。
    let mut height_scope = identity_participants_session();
    height_scope.participants[4] = "HFL".to_string();
    expect_kind(
        ModalParticipants::from_session(&height_scope),
        TargetErrorKind::Participants,
    );

    // 缺 speaker 声明 → Participants 拒收。
    let mut missing_speaker = identity_participants_session();
    missing_speaker
        .speakers
        .retain(|speaker| speaker.name != "SL");
    expect_kind(
        ModalParticipants::from_session(&missing_speaker),
        TargetErrorKind::Participants,
    );
}

#[test]
fn t50_target_height_scope_is_full_range_and_separate() {
    let grid = small_grid();
    let participants = ModalParticipants::p1_4_1_2();
    let roles = baseline_roles(&grid, &participants, 0.0, 0, 1);
    let baseline =
        frozen_baseline_with_roles(&grid, participants.clone(), ToneBinding::flat(), roles)
            .freeze()
            .unwrap();
    let height_scope = baseline.height_scope().unwrap();
    assert_eq!(height_scope.roles().len(), 2);
    assert!(height_scope.excluded_from_low_band_matrix());
    assert_eq!(height_scope.sample_rate_hz(), FS);
    let hfl = height_scope.role("HFL").unwrap();
    assert_eq!(hfl.out_index, 4);
    assert_eq!(hfl.band_hz, (0.0, FS as f64 / 2.0));
    assert_eq!(hfl.band_limit, BandLimit::None);
    assert_eq!(hfl.artificial_delay_ms, 0.0);
    assert!(!hfl.in_low_band_matrix);
    assert!(!participants.contains_role("HFL"));

    // 输出 scope 表：5 个低带矩阵 + 2 个高度 + 明确其它（无静默下混/造声道）。
    let output_scope = baseline.output_scope().unwrap();
    assert_eq!(output_scope.low_band_entries().len(), 5);
    assert_eq!(output_scope.height_entries().len(), 2);
    let scope_text = output_scope.canonical();
    assert!(scope_text.contains("role=HFL dac=4 kind=full_range_height"));
    assert!(scope_text.contains("role=FL dac=0 kind=low_band_matrix"));

    // 高度带限（LP/HP）→ Scope 拒收。
    let mut band_limited = baseline_roles(&grid, &participants, 0.0, 0, 1);
    band_limited[5] = band_limited[5]
        .clone()
        .with_band_limit(BandLimit::LowPass { freq_hz: 200.0 });
    expect_kind(
        try_fitted_baseline_with_roles(
            &grid,
            participants.clone(),
            ToneBinding::flat(),
            band_limited,
        )
        .and_then(FittedPostBaseline::freeze),
        TargetErrorKind::Scope,
    );

    // 高度人工 18 ms → Scope 拒收（旧生成器设计已被否定）。
    let mut artificial = baseline_roles(&grid, &participants, 0.0, 0, 1);
    artificial[4] = artificial[4]
        .clone()
        .with_artificial_delay_ms(18.0)
        .unwrap();
    expect_kind(
        try_fitted_baseline_with_roles(
            &grid,
            participants.clone(),
            ToneBinding::flat(),
            artificial,
        )
        .and_then(FittedPostBaseline::freeze),
        TargetErrorKind::Scope,
    );

    // 4.1.2 multichannel 基线缺高度 → height_scope Missing。
    let mut no_height = baseline_roles(&grid, &participants, 0.0, 0, 1);
    no_height.retain(|role| role.role_id() != "HFL" && role.role_id() != "HFR");
    let no_height_baseline =
        frozen_baseline_with_roles(&grid, participants.clone(), ToneBinding::flat(), no_height)
            .freeze()
            .unwrap();
    expect_kind(no_height_baseline.height_scope(), TargetErrorKind::Missing);
    assert!(no_height_baseline
        .optional_height_scope()
        .unwrap()
        .is_none());

    // stereo strict 基线可无高度输出。
    let stereo_participants = ModalParticipants::stereo_strict();
    let stereo_roles = baseline_roles(&grid, &stereo_participants, 0.0, 0, 1);
    let stereo_baseline = frozen_baseline_with_roles(
        &grid,
        stereo_participants,
        ToneBinding::flat(),
        stereo_roles,
    )
    .freeze()
    .unwrap();
    assert!(stereo_baseline.optional_height_scope().unwrap().is_none());
    expect_kind(stereo_baseline.height_scope(), TargetErrorKind::Missing);
}

// ===========================================================================
// P 冻结 / stale / 契约
// ===========================================================================

#[test]
fn t50_target_p_freeze_is_deterministic_and_timestamp_free() {
    let grid = small_grid();
    let participants = ModalParticipants::p1_4_1_2();
    let roles = baseline_roles(&grid, &participants, 0.0, 0, 1);
    let first = frozen_baseline_with_roles(
        &grid,
        participants.clone(),
        ToneBinding::flat(),
        roles.clone(),
    )
    .freeze()
    .unwrap();
    let second =
        frozen_baseline_with_roles(&grid, participants.clone(), ToneBinding::flat(), roles)
            .freeze()
            .unwrap();
    assert_eq!(first.baseline_hash(), second.baseline_hash());
    assert_eq!(first.canonical(), second.canonical());
    assert_eq!(first.baseline_hash().len(), 64);

    // 时间戳不进入 hash。
    let mut with_timestamp = PostBaselineInput {
        baseline_id: "t50-baseline-a".to_string(),
        sample_rate_hz: FS,
        participants: participants.clone(),
        tone: ToneBinding::flat(),
        curve: CurveBinding::baseline_only("flat_baseline").unwrap(),
        tone_applications: ToneApplicationProof::baseline_only(),
        fit_inputs_sha256: sha256_hex(b"t50-fit-inputs-v1"),
        common_d0: CommonD0::new(D0_SAMPLES).unwrap(),
        grid_hz: grid.clone(),
        roles: baseline_roles(&grid, &participants, 0.0, 0, 1),
        created_unix: Some(1_700_000_123),
    };
    let timestamped = FittedPostBaseline::new(with_timestamp.clone())
        .unwrap()
        .freeze()
        .unwrap();
    assert_eq!(timestamped.baseline_hash(), first.baseline_hash());
    assert_eq!(timestamped.created_unix(), Some(1_700_000_123));
    with_timestamp.created_unix = None;

    // 任何 P 阶段参数变化 → hash 变化（P 身份）。
    let changed_delay = frozen_baseline_with_roles(
        &grid,
        participants.clone(),
        ToneBinding::flat(),
        baseline_roles(&grid, &participants, 0.0, 7, 1),
    )
    .freeze()
    .unwrap();
    assert_ne!(changed_delay.baseline_hash(), first.baseline_hash());
    let changed_polarity = frozen_baseline_with_roles(
        &grid,
        participants.clone(),
        ToneBinding::flat(),
        baseline_roles(&grid, &participants, 0.0, 0, -1),
    )
    .freeze()
    .unwrap();
    assert_ne!(changed_polarity.baseline_hash(), first.baseline_hash());

    // stale：fit inputs / fs / tone / participants / grid 均被识别。
    let frozen = first;
    let current = frozen.identity();
    assert!(frozen.stale_reasons(&current).is_empty());
    let mut stale_fit = current.clone();
    stale_fit.fit_inputs_sha256 = sha256_hex(b"other-fit");
    assert_eq!(frozen.stale_reasons(&stale_fit)[0].code, "STALE_FIT_INPUTS");
    let mut stale_fs = current.clone();
    stale_fs.sample_rate_hz = 96_000;
    assert_eq!(frozen.stale_reasons(&stale_fs)[0].code, "STALE_SAMPLE_RATE");
    let mut stale_tone = current.clone();
    stale_tone.tone_curve_id = "tilt10_baseline".to_string();
    assert_eq!(frozen.stale_reasons(&stale_tone)[0].code, "STALE_TONE");
    let mut stale_participants = current.clone();
    stale_participants.participants_binding_hash = "f".repeat(64);
    assert_eq!(
        frozen.stale_reasons(&stale_participants)[0].code,
        "STALE_PARTICIPANTS"
    );
    let mut stale_grid = current.clone();
    stale_grid.grid_points += 1;
    assert_eq!(frozen.stale_reasons(&stale_grid)[0].code, "STALE_GRID");
}

#[test]
fn t50_target_rejects_invalid_p_baseline_contracts() {
    let grid = small_grid();
    let participants = ModalParticipants::p1_4_1_2();

    // 极性 0 不可猜（with_meta 直接拒收）。
    let legal_role = PostRoleFit::new("FL", 0, flat_points(&grid, 0.0)).unwrap();
    assert_eq!(
        target_error(legal_role.clone().with_meta(0, 0, 0.0)).kind(),
        TargetErrorKind::Polarity
    );
    assert_eq!(
        target_error(legal_role.clone().with_meta(0, 2, 0.0)).kind(),
        TargetErrorKind::Polarity
    );

    // 显式负向：通过 PostBaselineInput 构造非法角色（直接构造被 validate_contract 拦下）。
    let invalid_roles = vec![
        PostRoleFit::new("FL", 0, flat_points(&grid, 0.0)).unwrap(),
        PostRoleFit::new("FR", 1, flat_points(&grid, 0.0)).unwrap(),
        PostRoleFit::new("SL", 2, flat_points(&grid, 0.0)).unwrap(),
        PostRoleFit::new("SR", 3, flat_points(&grid, 0.0)).unwrap(),
        PostRoleFit::new("SUB1", 7, flat_points(&grid, 0.0)).unwrap(),
    ];
    // 缺高度（4.1.2 multichannel 需要 HFL/HFR 全频 scope，但基线输入本身允许无高度；构建 target 时才拒）。
    let input = PostBaselineInput {
        baseline_id: "t50-baseline-b".to_string(),
        sample_rate_hz: FS,
        participants: participants.clone(),
        tone: ToneBinding::flat(),
        curve: CurveBinding::baseline_only("flat_baseline").unwrap(),
        tone_applications: ToneApplicationProof::baseline_only(),
        fit_inputs_sha256: sha256_hex(b"t50-fit-inputs-v1"),
        common_d0: CommonD0::new(D0_SAMPLES).unwrap(),
        grid_hz: grid.clone(),
        roles: invalid_roles,
        created_unix: None,
    };

    // fit hash 全零 → Baseline 拒收。
    let mut zero_hash = input.clone();
    zero_hash.fit_inputs_sha256 = "0".repeat(64);
    expect_kind(
        FittedPostBaseline::new(zero_hash),
        TargetErrorKind::Baseline,
    );

    // curve 应用点非基线 / 次数非 1 → Tone 拒收。
    let mut residual_site = input.clone();
    residual_site.curve =
        CurveBinding::new("flat_baseline", CurveApplySite::LowBandResidualG, 1).unwrap();
    expect_kind(
        FittedPostBaseline::new(residual_site),
        TargetErrorKind::Tone,
    );
    let mut double_application = input.clone();
    double_application.curve =
        CurveBinding::new("flat_baseline", CurveApplySite::PostBaselineP, 2).unwrap();
    expect_kind(
        FittedPostBaseline::new(double_application),
        TargetErrorKind::Tone,
    );

    // tone 应用次数 target=1 / g=1 / baseline=0/2 → Tone 拒收。
    for (post, residual, target) in [(1, 0, 1), (1, 1, 0), (0, 0, 0), (2, 0, 0)] {
        let mut input = input.clone();
        input.tone_applications = ToneApplicationProof::declared(post, residual, target);
        expect_kind(FittedPostBaseline::new(input), TargetErrorKind::Tone);
    }

    // tone/curve id 不一致 → Curve 拒收。
    let mut mixed = input.clone();
    mixed.tone = ToneBinding::tilt10();
    expect_kind(FittedPostBaseline::new(mixed), TargetErrorKind::Curve);

    // 人工 delay 18 ms（任何角色）→ Scope 拒收。
    let mut artificial = input.clone();
    artificial.roles = baseline_roles(&grid, &participants, 0.0, 0, 1);
    artificial.roles[0] = artificial.roles[0]
        .clone()
        .with_artificial_delay_ms(18.0)
        .unwrap();
    expect_kind(FittedPostBaseline::new(artificial), TargetErrorKind::Scope);

    // 延迟 >200 ms → Range 拒收（analytic 构造期拒绝）。
    let over_delay = PostRoleFit::analytic("FL", 0, FS, &grid, None, 9_601, 1, 0.0);
    assert_eq!(target_error(over_delay).kind(), TargetErrorKind::Range);

    // 增益越界 → Range 拒收。
    let over_gain = PostRoleFit::analytic("FL", 0, FS, &grid, None, 0, 1, 13.0);
    assert_eq!(target_error(over_gain).kind(), TargetErrorKind::Range);

    // SUB1 保护低通 >130 Hz → Role 拒收；高度带限 → Scope 拒收；HP 400 Hz → Range。
    let mut sub_protection = baseline_roles(&grid, &participants, 0.0, 0, 1);
    let sub_index = sub_protection
        .iter()
        .position(|role| role.role_id() == "SUB1")
        .unwrap();
    sub_protection[sub_index] = sub_protection[sub_index]
        .clone()
        .with_band_limit(BandLimit::LowPass { freq_hz: 200.0 });
    expect_kind(
        FittedPostBaseline::new(PostBaselineInput {
            baseline_id: "t50-baseline-b".to_string(),
            sample_rate_hz: FS,
            participants: participants.clone(),
            tone: ToneBinding::flat(),
            curve: CurveBinding::baseline_only("flat_baseline").unwrap(),
            tone_applications: ToneApplicationProof::baseline_only(),
            fit_inputs_sha256: sha256_hex(b"t50-fit-inputs-v1"),
            common_d0: CommonD0::new(D0_SAMPLES).unwrap(),
            grid_hz: grid.clone(),
            roles: sub_protection,
            created_unix: None,
        }),
        TargetErrorKind::Role,
    );
    let mut sub_ok_roles = baseline_roles(&grid, &participants, 0.0, 0, 1);
    let sub_index = sub_ok_roles
        .iter()
        .position(|role| role.role_id() == "SUB1")
        .unwrap();
    sub_ok_roles[sub_index] = sub_ok_roles[sub_index]
        .clone()
        .with_band_limit(BandLimit::LowPass { freq_hz: 120.0 });
    let sub_ok = FittedPostBaseline::new(PostBaselineInput {
        baseline_id: "t50-baseline-b".to_string(),
        sample_rate_hz: FS,
        participants: participants.clone(),
        tone: ToneBinding::flat(),
        curve: CurveBinding::baseline_only("flat_baseline").unwrap(),
        tone_applications: ToneApplicationProof::baseline_only(),
        fit_inputs_sha256: sha256_hex(b"t50-fit-inputs-v1"),
        common_d0: CommonD0::new(D0_SAMPLES).unwrap(),
        grid_hz: grid.clone(),
        roles: sub_ok_roles,
        created_unix: None,
    })
    .unwrap()
    .freeze()
    .unwrap();
    assert_eq!(
        sub_ok.role("SUB1").unwrap().band_limit(),
        BandLimit::LowPass { freq_hz: 120.0 }
    );
}

// ===========================================================================
// H_eff = H_raw x P
// ===========================================================================

fn raw_plant(
    participants: &ModalParticipants,
    grid: &[f64],
    positions: Vec<RawPosition>,
) -> RawPlant {
    RawPlant::new(
        P1_LAYOUT_4_1_2,
        FS,
        participants.clone(),
        grid.to_vec(),
        positions,
    )
    .unwrap()
}

fn complex_divide(numerator: (f64, f64), denominator: (f64, f64)) -> (f64, f64) {
    let denom = denominator.0 * denominator.0 + denominator.1 * denominator.1;
    (
        (numerator.0 * denominator.0 + numerator.1 * denominator.1) / denom,
        (numerator.1 * denominator.0 - numerator.0 * denominator.1) / denom,
    )
}

#[test]
fn t50_target_heff_equals_hraw_times_p_closed_form() {
    let grid = small_grid();
    let participants = ModalParticipants::p1_4_1_2();
    // P: FL +3 dB / delay 7 / polarity -1；其它 participant 与高度 identity。
    let mut roles = baseline_roles(&grid, &participants, 0.0, 0, 1);
    roles[0] = post_role("FL", 0, &grid, 3.0, 7, -1);
    let baseline =
        frozen_baseline_with_roles(&grid, participants.clone(), ToneBinding::flat(), roles)
            .freeze()
            .unwrap();
    // H_raw: 全部 participant -6 dB flat、无实测反相。
    let plant = EffectivePlant::build(
        &baseline,
        &raw_plant(
            &participants,
            &grid,
            vec![raw_position_with(
                "P01",
                PositionRole::Design,
                1.0,
                &participants,
                &grid,
                |_, grid| flat_points(grid, -6.0),
                &[],
            )],
        ),
    )
    .unwrap();

    let fl = plant.input("P01", "FL").unwrap();
    let expected_magnitude = 10f64.powf(-6.0 / 20.0) * 10f64.powf(3.0 / 20.0);
    for (index, freq) in grid.iter().enumerate() {
        let phase = -2.0 * std::f64::consts::PI * freq * 7.0 / FS as f64 + std::f64::consts::PI;
        assert_complex_close(
            &fl.points[index],
            (
                expected_magnitude * phase.cos(),
                expected_magnitude * phase.sin(),
            ),
            1.0e-12,
            &format!("H_eff[FL]@{freq}"),
        );
    }
    let fr = plant.input("P01", "FR").unwrap();
    let base_magnitude = 10f64.powf(-6.0 / 20.0);
    for point in &fr.points {
        assert_complex_close(point, (base_magnitude, 0.0), 1.0e-12, "H_eff[FR]");
    }
    // 高度不进入低带 plant（scope 分离）。
    assert!(plant.input("P01", "HFL").is_none());
    assert!(plant.input("P01", "HFR").is_none());
    assert_eq!(plant.participants().m(), 5);
    // plant hash 确定性。
    let plant2 = EffectivePlant::build(
        &baseline,
        &raw_plant(
            &participants,
            &grid,
            vec![raw_position_with(
                "P01",
                PositionRole::Design,
                1.0,
                &participants,
                &grid,
                |_, grid| flat_points(grid, -6.0),
                &[],
            )],
        ),
    )
    .unwrap();
    assert_eq!(plant.hash(), plant2.hash());
    assert_eq!(plant.hash().len(), 64);
}

#[test]
fn t50_target_heff_polarity_normalized_exactly_once() {
    let grid = small_grid();
    let participants = ModalParticipants::p1_4_1_2();
    let points = flat_points(&grid, 0.0);
    let response = RawResponse::new("FL", 0, points.clone()).unwrap();
    let once = response.normalize_polarity(polarity(-1)).unwrap();
    let first = once.points()[0];
    assert_close(first.re, -1.0, 1.0e-12, "inverted re");
    assert_close(first.im, 0.0, 1.0e-12, "inverted im");
    // 第二次极性归一化 → E_TGT_POLARITY（双反相不可达）。
    expect_kind(
        once.clone().normalize_polarity(polarity(-1)),
        TargetErrorKind::Polarity,
    );
    expect_kind(
        once.normalize_polarity(polarity(1)),
        TargetErrorKind::Polarity,
    );

    // 未归一化极性的 raw 进入 plant → E_TGT_POLARITY（unknown polarity 不猜）。
    let mut responses = Vec::new();
    for participant in participants.participants() {
        responses.push(
            RawResponse::new(&participant.role_id, participant.out_index, points.clone()).unwrap(),
        );
    }
    let position = RawPosition::new("P01", PositionRole::Design, 1.0, responses).unwrap();
    expect_kind(
        RawPlant::new(
            P1_LAYOUT_4_1_2,
            FS,
            participants.clone(),
            grid.clone(),
            vec![position],
        ),
        TargetErrorKind::Polarity,
    );
}

fn valid_baseline_input(grid: &[f64], participants: &ModalParticipants) -> PostBaselineInput {
    PostBaselineInput {
        baseline_id: "t50-baseline-a".to_string(),
        sample_rate_hz: FS,
        participants: participants.clone(),
        tone: ToneBinding::flat(),
        curve: CurveBinding::baseline_only("flat_baseline").unwrap(),
        tone_applications: ToneApplicationProof::baseline_only(),
        fit_inputs_sha256: sha256_hex(b"t50-fit-inputs-v1"),
        common_d0: CommonD0::new(D0_SAMPLES).unwrap(),
        grid_hz: grid.to_vec(),
        roles: baseline_roles(grid, participants, 0.0, 0, 1),
        created_unix: None,
    }
}

#[test]
fn t50_target_heff_rejects_grid_and_binding_mismatch() {
    let grid = small_grid();
    let other_grid = grid_1_24_oct();
    let participants = ModalParticipants::p1_4_1_2();
    let baseline = default_baseline(&grid);

    // 网格不一致（P 与 raw 同一契约）→ E_TGT_BASELINE。
    let raw = raw_plant(
        &participants,
        &other_grid,
        vec![raw_position_with(
            "P01",
            PositionRole::Design,
            1.0,
            &participants,
            &other_grid,
            |_, grid| flat_points(grid, 0.0),
            &[],
        )],
    );
    expect_kind(
        EffectivePlant::build(&baseline, &raw),
        TargetErrorKind::Baseline,
    );

    // participants 不同（stereo vs 5x5 binding）→ E_TGT_BASELINE。
    let stereo_participants = ModalParticipants::stereo_strict();
    let stereo_raw = raw_plant(
        &stereo_participants,
        &grid,
        vec![raw_position_with(
            "P01",
            PositionRole::Design,
            1.0,
            &stereo_participants,
            &grid,
            |_, grid| flat_points(grid, 0.0),
            &[],
        )],
    );
    expect_kind(
        EffectivePlant::build(&baseline, &stereo_raw),
        TargetErrorKind::Baseline,
    );

    // 非法网格：非严格升序 / 超 Nyquist / 点数不足 → E_TGT_GRID。
    let mut unsorted = grid.clone();
    unsorted.swap(2, 3);
    let mut input = valid_baseline_input(&grid, &participants);
    input.grid_hz = unsorted;
    expect_kind(FittedPostBaseline::new(input), TargetErrorKind::Grid);
    let mut over_nyquist = grid.clone();
    over_nyquist.push(30_000.0);
    let mut input = valid_baseline_input(&grid, &participants);
    input.grid_hz = over_nyquist;
    expect_kind(FittedPostBaseline::new(input), TargetErrorKind::Grid);
    let tiny = vec![20.0, 40.0, 80.0];
    let mut too_few = valid_baseline_input(&grid, &participants);
    too_few.grid_hz = tiny;
    expect_kind(FittedPostBaseline::new(too_few), TargetErrorKind::Grid);

    // raw plant：重复 label / 无 design / out_index 不一致 → 拒收。
    let duplicate_positions = vec![
        raw_position_with(
            "P01",
            PositionRole::Design,
            1.0,
            &participants,
            &grid,
            |_, grid| flat_points(grid, 0.0),
            &[],
        ),
        raw_position_with(
            "P01",
            PositionRole::Validation,
            1.0,
            &participants,
            &grid,
            |_, grid| flat_points(grid, 0.0),
            &[],
        ),
    ];
    expect_kind(
        RawPlant::new(
            P1_LAYOUT_4_1_2,
            FS,
            participants.clone(),
            grid.clone(),
            duplicate_positions,
        ),
        TargetErrorKind::Position,
    );
    let only_validation = vec![raw_position_with(
        "P09",
        PositionRole::Validation,
        1.0,
        &participants,
        &grid,
        |_, grid| flat_points(grid, 0.0),
        &[],
    )];
    expect_kind(
        RawPlant::new(
            P1_LAYOUT_4_1_2,
            FS,
            participants.clone(),
            grid.clone(),
            only_validation,
        ),
        TargetErrorKind::Position,
    );
    let mut bad_positions = Vec::new();
    for participant in participants.participants() {
        bad_positions.push(normalized_response(
            &participant.role_id,
            participant.out_index + 1,
            flat_points(&grid, 0.0),
            false,
        ));
    }
    let bad_position = RawPosition::new("P01", PositionRole::Design, 1.0, bad_positions).unwrap();
    expect_kind(
        RawPlant::new(
            P1_LAYOUT_4_1_2,
            FS,
            participants.clone(),
            grid.clone(),
            vec![bad_position],
        ),
        TargetErrorKind::Role,
    );
}

#[test]
fn t50_target_heff_from_t49_commond0_single_subtraction_and_polarity() {
    let grid = small_grid();
    let participants = ModalParticipants::p1_4_1_2();
    let slots = t49_slot_phases(&participants, &grid, 137.25, -1);
    // T49 v2 契约：aligned_segment 只扣公共整数 D0 一次（不扣 measured delay）。
    assert_eq!(slots[0].absolute.delay_removed_samples(), D0_SAMPLES as f64);
    assert_eq!(slots[0].absolute.common_d0().samples(), D0_SAMPLES);
    assert!(
        slots[0].absolute.polarity().is_none(),
        "T49 leaves polarity un-applied"
    );

    let position = RawPosition::from_slot_phases("P01", PositionRole::Design, 1.0, &slots).unwrap();
    let raw = raw_plant(&participants, &grid, vec![position]);
    let baseline = default_baseline(&grid);
    let plant = EffectivePlant::build(&baseline, &raw).unwrap();
    let fl = plant.input("P01", "FL").unwrap();
    // 期望：|H|=10^(-12/20)；相位 = phase0 + 360 f (137.25 - D0)/fs + 180°（极性只乘一次）。
    for (index, freq) in grid.iter().enumerate() {
        let expected_phase_deg =
            phase0_deg(*freq) + 360.0 * freq * (137.25 - D0_SAMPLES as f64) / FS as f64 + 180.0;
        let magnitude = 10f64.powf(-12.0 / 20.0);
        let expected = (
            magnitude * expected_phase_deg.to_radians().cos(),
            magnitude * expected_phase_deg.to_radians().sin(),
        );
        assert_complex_close(
            &fl.points[index],
            expected,
            1.0e-9,
            &format!("T49 H_eff@{freq}"),
        );
    }
    // 负向：若误再扣 measured delay（双去相位），相位会少 360 f 137.25/fs（1 kHz 约 1029°）。
    let index_1k = grid
        .iter()
        .position(|freq| (*freq - 1_000.0).abs() < 1.0e-9)
        .expect("1 kHz grid point");
    let mut double_subtracted = (phase0_deg(grid[index_1k]) + 180.0) % 360.0;
    if double_subtracted < 0.0 {
        double_subtracted += 360.0;
    }
    let actual_deg = fl.points[index_1k].phase_deg().rem_euclid(360.0);
    let difference = (actual_deg - double_subtracted).abs();
    assert!(
        difference.min(360.0 - difference) > 1.0,
        "measured delay must remain provenance (single D0 subtraction only); difference {difference}"
    );
    // provenance residual = measured - D0（不进入相位）。
    assert_close(
        137.25 - D0_SAMPLES as f64,
        17.25,
        1.0e-12,
        "residual provenance",
    );
}

// ===========================================================================
// 衰减目标
// ===========================================================================

#[test]
fn t50_target_decay_preserve_and_reduced_tstar() {
    let preserve = default_decay();
    assert_eq!(preserve.mode(), DecayMode::Preserve);
    assert_eq!(preserve.ratio(), 1.0);
    assert!(preserve.is_preserve());
    assert!(!preserve.meets_t20_gate());
    assert_eq!(preserve.target_t60_s(63.0), Some(DEFAULT_T60_63));
    assert_eq!(preserve.reduction_percent(), 0.0);

    let reduced = reduced_decay(0.85);
    assert_eq!(reduced.mode(), DecayMode::Reduced { ratio: 0.85 });
    assert!(reduced.meets_t20_gate());
    assert_close(
        reduced.target_t60_s(63.0).unwrap(),
        DEFAULT_T60_63 * 0.85,
        1.0e-12,
        "T*63",
    );
    assert_close(
        reduced.target_t60_s(125.0).unwrap(),
        DEFAULT_T60_125 * 0.85,
        1.0e-12,
        "T*125",
    );
    assert_close(reduced.reduction_percent(), 15.0, 1.0e-12, "reduction");
    assert!(reduced_decay(0.70).meets_t20_gate());
    assert!(reduced_decay(0.55).meets_t20_gate());
    assert_eq!(reduced_decay(0.55).target_t60_s(40.0), None);

    // ratio 越界 / NaN → 拒收。
    for ratio in [0.0, -0.1, 1.5, f64::NAN] {
        let result = DecayTarget::reduced(
            ratio,
            vec![
                BandDecay {
                    band_hz: 63.0,
                    baseline_t60_s: DEFAULT_T60_63,
                },
                BandDecay {
                    band_hz: 125.0,
                    baseline_t60_s: DEFAULT_T60_125,
                },
            ],
        );
        let kind = target_error(result).kind();
        assert!(
            matches!(kind, TargetErrorKind::Decay | TargetErrorKind::Range),
            "ratio {ratio} gave {kind:?}"
        );
    }
    // 缺 63/125 → Decay；重复带 → Decay；T60 越界 → Range；空 bands → Decay。
    expect_kind(
        DecayTarget::preserve(vec![BandDecay {
            band_hz: 125.0,
            baseline_t60_s: 0.3,
        }]),
        TargetErrorKind::Decay,
    );
    expect_kind(
        DecayTarget::preserve(vec![
            BandDecay {
                band_hz: 63.0,
                baseline_t60_s: 0.4,
            },
            BandDecay {
                band_hz: 63.0,
                baseline_t60_s: 0.3,
            },
            BandDecay {
                band_hz: 125.0,
                baseline_t60_s: 0.3,
            },
        ]),
        TargetErrorKind::Decay,
    );
    expect_kind(
        DecayTarget::preserve(vec![
            BandDecay {
                band_hz: 63.0,
                baseline_t60_s: 0.001,
            },
            BandDecay {
                band_hz: 125.0,
                baseline_t60_s: 0.3,
            },
        ]),
        TargetErrorKind::Range,
    );
    expect_kind(DecayTarget::preserve(Vec::new()), TargetErrorKind::Decay);
}

// ===========================================================================
// 目标矩阵：preserve / reduced / holdout 隔离
// ===========================================================================

#[test]
fn t50_target_declares_target_delay_phase() {
    let grid = small_grid();
    let participants = ModalParticipants::p1_4_1_2();
    let baseline = default_baseline(&grid);
    let plant = EffectivePlant::build(
        &baseline,
        &raw_plant(
            &participants,
            &grid,
            flat_positions(
                &participants,
                &grid,
                &[
                    ("P01", PositionRole::Design),
                    ("P02", PositionRole::Design),
                    ("P03", PositionRole::Design),
                ],
            ),
        ),
    )
    .unwrap();
    let spec = TargetSpec::for_baseline(&baseline, reduced_decay(0.85))
        .unwrap()
        .with_target_delay_samples(24);
    assert_eq!(spec.target_delay_samples(), 24);
    let target = build_target_matrix(&baseline, &plant, &spec).unwrap();
    assert_eq!(target.target_delay_samples(), 24);
    assert!(target.canonical().contains("target_delay_samples=24"));
    let points = target.diag_response(None, 0).unwrap();
    for (index, freq) in grid.iter().enumerate() {
        let expected_phase = -2.0 * std::f64::consts::PI * freq * 24.0 / FS as f64;
        let magnitude = points[index].re.hypot(points[index].im);
        assert_close(
            points[index].re,
            magnitude * expected_phase.cos(),
            1.0e-12,
            "declared target delay re",
        );
        assert_close(
            points[index].im,
            magnitude * expected_phase.sin(),
            1.0e-12,
            "declared target delay im",
        );
    }
    // 超过 200 ms（48 kHz = 9600 samples）→ Range 拒收。
    let over_delay = TargetSpec::for_baseline(&baseline, reduced_decay(0.85))
        .unwrap()
        .with_target_delay_samples(9_601);
    expect_kind(
        build_target_matrix(&baseline, &plant, &over_delay),
        TargetErrorKind::Range,
    );
}

#[test]
fn t50_target_preserve_matrix_matches_heff_per_position() {
    let grid = grid_1_24_oct();
    let participants = ModalParticipants::p1_4_1_2();
    let baseline = default_baseline(&grid);
    let positions = vec![
        raw_position_with(
            "P01",
            PositionRole::Design,
            1.0,
            &participants,
            &grid,
            |_, grid| flat_points(grid, -3.0),
            &[],
        ),
        raw_position_with(
            "P02",
            PositionRole::Design,
            1.0,
            &participants,
            &grid,
            |_, grid| flat_points(grid, -4.0),
            &[],
        ),
        raw_position_with(
            "P03",
            PositionRole::Design,
            1.0,
            &participants,
            &grid,
            |_, grid| flat_points(grid, -5.0),
            &[],
        ),
        raw_position_with(
            "P09",
            PositionRole::Validation,
            1.0,
            &participants,
            &grid,
            |_, grid| room_points(grid, -3.0, 6.0),
            &[],
        ),
    ];
    let plant =
        EffectivePlant::build(&baseline, &raw_plant(&participants, &grid, positions)).unwrap();
    let spec = TargetSpec::for_baseline(&baseline, default_decay()).unwrap();
    let target = build_target_matrix(&baseline, &plant, &spec).unwrap();

    assert_eq!(target.schema(), TARGET_SCHEMA);
    assert_eq!(target.profile(), TargetProfile::Multichannel4x2_5);
    assert_eq!(target.m(), 5);
    assert_eq!(target.status(), TargetStatus::Preserve);
    assert!(target.is_position_variant());
    assert_eq!(target.cells().len(), 5 * 3);
    // per-position preserve cell 必须逐点等于该 position 的 H_eff。
    let plant_fl = plant.input("P01", "FL").unwrap();
    let cell = match target.cell(Some("P01"), 0, 0) {
        TargetCellView::Response(points) => points.to_vec(),
        TargetCellView::Zero => panic!("missing preserve cell"),
    };
    assert_eq!(cell, plant_fl.points);
    // 非对角结构零；position-invariant 查询返回 Zero。
    assert!(matches!(
        target.cell(Some("P01"), 0, 1),
        TargetCellView::Zero
    ));
    assert!(matches!(target.cell(None, 0, 0), TargetCellView::Zero));
    assert!(target.diag_response(None, 0).is_none());
    assert!(target.diag_response(Some("P02"), 3).is_some());
    // preserve 诊断：0 所需修正/0 纹波；holdout 只记录不参与。
    assert_eq!(target.diagnostics().required_correction_max_db, 0.0);
    assert_eq!(target.diagnostics().ripple_before_max_db, 0.0);
    assert_eq!(
        target.diagnostics().validation_positions_reserved,
        vec!["P09".to_string()]
    );
    assert!(target.verify_holdout_isolated());
    assert!(target.verify_structure().is_ok());
    assert!(target.verify_tone_single_application().is_ok());
    assert_eq!(target.target_sha256().len(), 64);
    assert_eq!(
        target.canonical(),
        build_target_matrix(&baseline, &plant, &spec)
            .unwrap()
            .canonical()
    );
    // 高度 scope 记录且不进 cells。
    assert!(target.height_scope().is_some());
    assert!(target
        .cells()
        .iter()
        .all(|cell| cell.output_role != "HFL" && cell.output_role != "HFR"));
    assert!(target.diagnostics().improvement_band_coverage >= 0.8);
}

#[test]
fn t50_target_reduced_target_smooths_modal_peak_and_status_caps() {
    let grid = grid_1_24_oct();
    let participants = ModalParticipants::p1_4_1_2();
    let baseline = default_baseline(&grid);

    let build = |generate: fn(&[f64]) -> Vec<ComplexPoint>| {
        let positions: Vec<RawPosition> = ["P01", "P02", "P03"]
            .iter()
            .map(|label| {
                raw_position_with(
                    label,
                    PositionRole::Design,
                    1.0,
                    &participants,
                    &grid,
                    |_, grid| generate(grid),
                    &[],
                )
            })
            .collect();
        let plant =
            EffectivePlant::build(&baseline, &raw_plant(&participants, &grid, positions)).unwrap();
        let spec = TargetSpec::for_baseline(&baseline, reduced_decay(0.85)).unwrap();
        let target = build_target_matrix(&baseline, &plant, &spec).unwrap();
        (plant, target)
    };

    // 4 dB 模态峰：平滑目标去掉大部分峰，所需修正 < 软帽 → predicted_pass。
    let (plant, modest) = build(|grid| room_points(grid, 0.0, 4.0));
    assert!(!modest.is_position_variant());
    assert_eq!(modest.cells().len(), 5);
    let target_fl = modest.diag_response(None, 0).unwrap();
    let measured_fl = plant.input("P01", "FL").unwrap();
    // 取离 120 Hz 最近的网格点（1/24 oct 网格在 119.865 Hz）。
    let peak_index = grid
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| {
            (**left - 120.0)
                .abs()
                .partial_cmp(&(**right - 120.0).abs())
                .unwrap()
        })
        .map(|(index, _)| index)
        .expect("peak grid point");
    let measured_peak_db = 20.0
        * measured_fl.points[peak_index]
            .re
            .hypot(measured_fl.points[peak_index].im)
            .log10();
    let target_peak_db = 20.0
        * target_fl[peak_index]
            .re
            .hypot(target_fl[peak_index].im)
            .log10();
    assert!(
        measured_peak_db > 3.0,
        "synthetic modal peak must be present: {measured_peak_db}"
    );
    assert!(
        target_peak_db < measured_peak_db - 1.0,
        "reduced target must smooth the modal peak: target {target_peak_db} measured {measured_peak_db}"
    );
    assert!(modest.diagnostics().required_correction_max_db > 0.5);
    assert!(modest.diagnostics().ripple_before_max_db > 1.0);
    assert_eq!(modest.status(), TargetStatus::PredictedPass);
    assert!(modest.decay().meets_t20_gate());
    // 目标相位 = 声明的附加目标延迟线性相位（默认 0 = 零多余相位；不是测量相位平滑）。
    assert_eq!(modest.target_delay_samples(), 0);
    assert!(target_fl
        .iter()
        .all(|point| point.im.abs() <= 1.0e-12 && point.re >= 0.0));

    // -10 dB 深谷：平滑目标需要 +9 dB 级提升 → 超软帽但未超硬帽 → predicted_partial。
    let (_, medium) = build(|grid| notch_points(grid, 0.0, -10.0));
    let correction = medium.diagnostics().required_correction_max_db;
    assert_eq!(medium.status(), TargetStatus::PredictedPartial);
    assert!(correction > 6.0 && correction <= 12.0);

    // -40 dB 深谷：平滑目标需要 >> 硬帽的 boost → infeasible（不放宽帽子伪装可行）。
    let (_, severe) = build(|grid| notch_points(grid, 0.0, -40.0));
    assert!(severe.diagnostics().required_correction_max_db > 12.0);
    assert_eq!(severe.status(), TargetStatus::Infeasible);
    assert!(severe.diagnostics().ripple_before_max_db > 20.0);
}

#[test]
fn t50_target_reduced_is_holdout_isolated() {
    let grid = small_grid();
    let participants = ModalParticipants::p1_4_1_2();
    let baseline = default_baseline(&grid);
    let design_positions = |validation_points: Vec<ComplexPoint>| {
        vec![
            raw_position_with(
                "P01",
                PositionRole::Design,
                1.0,
                &participants,
                &grid,
                |_, grid| room_points(grid, 0.0, 6.0),
                &[],
            ),
            raw_position_with(
                "P02",
                PositionRole::Design,
                1.0,
                &participants,
                &grid,
                |_, grid| room_points(grid, 0.0, 6.0),
                &[],
            ),
            RawPosition::new(
                "P09",
                PositionRole::Validation,
                1.0,
                participants
                    .participants()
                    .iter()
                    .map(|participant| {
                        normalized_response(
                            &participant.role_id,
                            participant.out_index,
                            validation_points.clone(),
                            false,
                        )
                    })
                    .collect(),
            )
            .unwrap(),
        ]
    };
    let plant_a = EffectivePlant::build(
        &baseline,
        &raw_plant(
            &participants,
            &grid,
            design_positions(flat_points(&grid, -20.0)),
        ),
    )
    .unwrap();
    let plant_b = EffectivePlant::build(
        &baseline,
        &raw_plant(
            &participants,
            &grid,
            design_positions(room_points(&grid, 0.0, 30.0)),
        ),
    )
    .unwrap();
    assert_ne!(
        plant_a.hash(),
        plant_b.hash(),
        "plant hashes include holdout data"
    );
    let spec = TargetSpec::for_baseline(&baseline, reduced_decay(0.85)).unwrap();
    let target_a = build_target_matrix(&baseline, &plant_a, &spec).unwrap();
    let target_b = build_target_matrix(&baseline, &plant_b, &spec).unwrap();
    assert_eq!(
        target_a.design_plant_hash(),
        target_b.design_plant_hash(),
        "design-only digest must ignore holdout data"
    );
    assert_eq!(
        target_a.target_sha256(),
        target_b.target_sha256(),
        "holdout data must not influence the target"
    );
    assert!(target_a.cells().iter().all(|cell| cell.position.is_none()));
    assert_eq!(
        target_a.diagnostics().validation_positions_reserved,
        vec!["P09".to_string()]
    );
    assert!(target_a.verify_holdout_isolated());
    assert!(!target_a.is_position_variant());
}

// ===========================================================================
// tone 单次 / 形状 / curve 绑定
// ===========================================================================

fn flat_positions(
    participants: &ModalParticipants,
    grid: &[f64],
    labels: &[(&str, PositionRole)],
) -> Vec<RawPosition> {
    labels
        .iter()
        .map(|(label, role)| {
            raw_position_with(
                label,
                *role,
                1.0,
                participants,
                grid,
                |_, grid| flat_points(grid, 0.0),
                &[],
            )
        })
        .collect()
}

fn stereo_baseline(grid: &[f64]) -> tspi_modal_design::target::FrozenPostBaseline {
    let participants = ModalParticipants::stereo_strict();
    let roles = baseline_roles(grid, &participants, 0.0, 0, 1);
    frozen_baseline_with_roles(grid, participants, ToneBinding::flat(), roles)
        .freeze()
        .unwrap()
}

#[test]
fn t50_target_tone_single_application_and_no_double_stack() {
    let grid = small_grid();
    let participants = ModalParticipants::p1_4_1_2();

    // 基线 P 的 magnitude 形状 = 一次 tilt10；H_raw flat 1 → preserve target 直接继承该 tone。
    let tilt = ToneBinding::tilt10();
    let mut roles = Vec::new();
    for participant in participants.participants() {
        let points = grid
            .iter()
            .map(|freq| {
                let magnitude = 10f64.powf(tilt.mag_db(*freq) / 20.0);
                ComplexPoint {
                    freq_hz: *freq,
                    re: magnitude,
                    im: 0.0,
                }
            })
            .collect();
        roles.push(PostRoleFit::new(&participant.role_id, participant.out_index, points).unwrap());
    }
    roles.push(post_role("HFL", 4, &grid, 0.0, 0, 1));
    roles.push(post_role("HFR", 5, &grid, 0.0, 0, 1));
    let baseline = frozen_baseline_with_roles(&grid, participants.clone(), tilt.clone(), roles)
        .freeze()
        .unwrap();
    let plant = EffectivePlant::build(
        &baseline,
        &raw_plant(
            &participants,
            &grid,
            flat_positions(
                &participants,
                &grid,
                &[
                    ("P01", PositionRole::Design),
                    ("P02", PositionRole::Design),
                    ("P03", PositionRole::Design),
                ],
            ),
        ),
    )
    .unwrap();
    let spec = TargetSpec::for_baseline(&baseline, default_decay()).unwrap();
    let target = build_target_matrix(&baseline, &plant, &spec).unwrap();
    // 声明层：target 继承 tone（1/0/0），不重算/不再应用。
    assert_eq!(target.tone().curve_id(), "tilt10_baseline");
    assert_eq!(target.tone().shape(), ToneShape::Tilt10);
    assert_eq!(
        target.tone_applications(),
        ToneApplicationProof::baseline_only()
    );
    assert_eq!(target.tone_applications().post_baseline(), 1);
    assert_eq!(target.tone_applications().low_band_residual(), 0);
    assert_eq!(target.tone_applications().target(), 0);
    assert!(target.verify_tone_single_application().is_ok());
    assert_eq!(target.curve().curve_id(), "tilt10_baseline");

    // 数值层：target 幅度只含一次 tilt10（63 Hz → 1 kHz 的差值 = 单次 tone 斜率）。
    let cell = match target.cell(Some("P01"), 0, 0) {
        TargetCellView::Response(points) => points.to_vec(),
        TargetCellView::Zero => panic!("missing preserve cell"),
    };
    let index_63 = grid
        .iter()
        .position(|freq| (*freq - 63.0).abs() < 1.0e-9)
        .unwrap();
    let index_1k = grid
        .iter()
        .position(|freq| (*freq - 1_000.0).abs() < 1.0e-9)
        .unwrap();
    let delta_target = 20.0
        * (cell[index_1k].re.hypot(cell[index_1k].im) / cell[index_63].re.hypot(cell[index_63].im))
            .log10();
    let delta_single_tone = tilt.mag_db(1_000.0) - tilt.mag_db(63.0);
    assert_close(
        delta_target,
        delta_single_tone,
        1.0e-9,
        "one tone application",
    );
    assert!(
        (delta_target - 2.0 * delta_single_tone).abs() > 1.0,
        "double-stacked tone would double the slope; not observed"
    );

    // 残留 curve（G 或 target 侧）→ E_TGT_TONE 拒收（tone 只应用一次）。
    for site in [
        CurveApplySite::LowBandResidualG,
        CurveApplySite::TargetMatrix,
    ] {
        let residual = CurveBinding::new("tilt10_baseline", site, 1).unwrap();
        let bad_spec = TargetSpec::for_baseline(&baseline, default_decay())
            .unwrap()
            .with_residual_curve(residual);
        expect_kind(
            build_target_matrix(&baseline, &plant, &bad_spec),
            TargetErrorKind::Tone,
        );
    }

    // target 继承的 tone 与另一条 baseline 不一致 → E_TGT_TONE。
    let flat_baseline = default_baseline(&grid);
    let spec = TargetSpec::for_baseline(&baseline, default_decay()).unwrap();
    expect_kind(spec.validate(&flat_baseline), TargetErrorKind::Tone);
}

#[test]
fn t50_target_tone_shape_and_curve_binding_rules() {
    // flat 恒 0；tilt10：20 Hz 0 → 20 kHz −10 dB，log-f 线性，端点外固定。
    assert_eq!(ToneShape::Flat.mag_db(20.0), 0.0);
    assert_eq!(ToneShape::Flat.mag_db(20_000.0), 0.0);
    assert_eq!(ToneShape::Flat.slope_db_per_decade(), 0.0);
    assert_close(
        ToneShape::Tilt10.mag_db(20.0),
        0.0,
        1.0e-12,
        "tilt10 low anchor",
    );
    assert_close(
        ToneShape::Tilt10.mag_db(20_000.0),
        -10.0,
        1.0e-12,
        "tilt10 high anchor",
    );
    assert_close(
        ToneShape::Tilt10.mag_db(1_000.0),
        -10.0 * (1_000.0f64 / 20.0).log10() / 3.0,
        1.0e-12,
        "tilt10 1 kHz",
    );
    assert_close(
        ToneShape::Tilt10.mag_db(1_000.0),
        -5.663_233_347_786_729,
        1.0e-9,
        "tilt10 1 kHz literal",
    );
    assert_close(
        ToneShape::Tilt10.mag_db(10.0),
        0.0,
        1.0e-12,
        "tilt10 below 20 Hz hold",
    );
    assert_close(
        ToneShape::Tilt10.mag_db(30_000.0),
        -10.0,
        1.0e-12,
        "tilt10 above 20 kHz hold",
    );
    assert_close(
        ToneShape::Tilt10.slope_db_per_decade(),
        -10.0 / 3.0,
        1.0e-12,
        "tilt10 slope",
    );
    assert_eq!(ToneShape::from_id("flat"), Some(ToneShape::Flat));
    assert_eq!(ToneShape::from_id("tilt10"), Some(ToneShape::Tilt10));
    assert_eq!(ToneShape::from_id("tilt3"), None);

    // ToneBinding 字段校验/参考增益/目标 hash。
    let flat = ToneBinding::flat();
    assert_eq!(flat.shape(), ToneShape::Flat);
    assert_eq!(flat.reference_gain_db(), 0.0);
    assert!(flat.target_sha256().is_none());
    assert!(flat.same_definition(&ToneBinding::flat()));
    assert!(!flat.same_definition(&ToneBinding::tilt10()));
    assert_eq!(flat.binding_hash(), ToneBinding::flat().binding_hash());
    let tilt = ToneBinding::new("tilt10_baseline", ToneShape::Tilt10)
        .unwrap()
        .with_reference_gain_db(-2.0)
        .unwrap();
    assert_close(tilt.mag_db(20.0), -2.0, 1.0e-12, "reference gain offset");
    let hashed = tilt.clone().with_target_sha256("b".repeat(64)).unwrap();
    assert!(hashed.target_sha256().is_some());
    expect_kind(
        ToneBinding::new("bad id!", ToneShape::Flat),
        TargetErrorKind::Tone,
    );
    expect_kind(
        ToneBinding::flat().with_target_sha256("xyz".to_string()),
        TargetErrorKind::Tone,
    );
    expect_kind(
        ToneBinding::flat().with_reference_gain_db(30.0),
        TargetErrorKind::Range,
    );

    // CurveBinding：只允许 P_post 基线一次；其它应用点/次数 → E_TGT_TONE。
    let baseline_curve = CurveBinding::baseline_only("flat_baseline").unwrap();
    assert_eq!(baseline_curve.site(), CurveApplySite::PostBaselineP);
    assert_eq!(baseline_curve.applications(), 1);
    assert!(baseline_curve.is_baseline());
    assert!(baseline_curve.validate_single_application().is_ok());
    assert_eq!(
        CurveApplySite::from_id("post_baseline_p"),
        Some(CurveApplySite::PostBaselineP)
    );
    assert_eq!(CurveApplySite::from_id("nope"), None);
    expect_kind(
        CurveBinding::new("flat_baseline", CurveApplySite::PostBaselineP, 0),
        TargetErrorKind::Curve,
    );
    for site in [
        CurveApplySite::LowBandResidualG,
        CurveApplySite::TargetMatrix,
    ] {
        let residual = CurveBinding::new("flat_baseline", site, 1).unwrap();
        expect_kind(
            residual.validate_single_application(),
            TargetErrorKind::Tone,
        );
        assert!(!residual.is_baseline());
    }
    let double = CurveBinding::new("flat_baseline", CurveApplySite::PostBaselineP, 2).unwrap();
    expect_kind(double.validate_single_application(), TargetErrorKind::Tone);
    assert_eq!(baseline_curve.binding_hash().len(), 64);
}

// ===========================================================================
// stereo strict 独立 profile / 求解校验
// ===========================================================================

#[test]
fn t50_target_stereo_strict_separate_profile() {
    let grid = small_grid();
    let baseline = stereo_baseline(&grid);
    let plant = EffectivePlant::build(
        &baseline,
        &raw_plant(
            &baseline.participants().clone(),
            &grid,
            flat_positions(
                &baseline.participants().clone(),
                &grid,
                &[
                    ("P01", PositionRole::Design),
                    ("P02", PositionRole::Design),
                    ("P03", PositionRole::Design),
                ],
            ),
        ),
    )
    .unwrap();
    let spec = TargetSpec::stereo_strict(&baseline, default_decay()).unwrap();
    assert_eq!(spec.profile(), TargetProfile::StereoStrict2);
    let target = build_stereo_strict_target(&baseline, &plant, &spec).unwrap();
    assert_eq!(target.profile(), TargetProfile::StereoStrict2);
    assert_eq!(target.m(), 2);
    assert_eq!(target.participants().role_ids(), vec!["FL", "FR"]);
    assert!(target
        .cells()
        .iter()
        .all(|cell| cell.output_dense < 2 && cell.input_dense < 2));
    assert!(target.height_scope().is_none());
    assert_eq!(
        target
            .warnings()
            .iter()
            .filter(|warning| warning.code == "W_TGT_STEREO_STRICT_PROFILE")
            .count(),
        1
    );
    // 独立 hash domain（与 5x5 分开）。
    assert!(target
        .canonical()
        .starts_with("tspi-modal-stereo-strict-target/v1"));

    // 5x5 规格不能复用 stereo builder（独立 profile/独立求解）。
    let multi_baseline = default_baseline(&grid);
    let multi_plant = EffectivePlant::build(
        &multi_baseline,
        &raw_plant(
            &multi_baseline.participants().clone(),
            &grid,
            flat_positions(
                &multi_baseline.participants().clone(),
                &grid,
                &[("P01", PositionRole::Design)],
            ),
        ),
    )
    .unwrap();
    let multi_spec = TargetSpec::for_baseline(&multi_baseline, default_decay()).unwrap();
    assert_eq!(multi_spec.profile(), TargetProfile::Multichannel4x2_5);
    expect_kind(
        build_stereo_strict_target(&multi_baseline, &multi_plant, &multi_spec),
        TargetErrorKind::Stereo,
    );
    // stereo strict spec 不能从 5x5 基线派生（禁止 runtime mask 5x5）。
    expect_kind(
        TargetSpec::stereo_strict(&multi_baseline, default_decay()),
        TargetErrorKind::Stereo,
    );
    let multi_target = build_target_matrix(&multi_baseline, &multi_plant, &multi_spec).unwrap();
    assert!(multi_target
        .canonical()
        .starts_with("tspi-modal-target-matrix/v1"));
    assert_ne!(
        multi_target.target_sha256(),
        build_stereo_strict_target(&baseline, &plant, &spec)
            .unwrap()
            .target_sha256()
    );
}

#[test]
fn t50_target_stereo_strict_solution_validation() {
    let grid = small_grid();
    let baseline = stereo_baseline(&grid);
    let participants = baseline.participants().clone();
    let plant = EffectivePlant::build(
        &baseline,
        &raw_plant(
            &participants,
            &grid,
            flat_positions(
                &participants,
                &grid,
                &[
                    ("P01", PositionRole::Design),
                    ("P02", PositionRole::Design),
                    ("P03", PositionRole::Design),
                ],
            ),
        ),
    )
    .unwrap();
    let spec = TargetSpec::stereo_strict(&baseline, default_decay()).unwrap();
    let target = build_stereo_strict_target(&baseline, &plant, &spec).unwrap();
    let roles: Vec<String> = participants
        .role_ids()
        .iter()
        .map(|role| (*role).to_string())
        .collect();

    // 合法 2x2 G（preserve 目标 → identity 预测恒等通过）。
    let identity = ModalMatrix::identity(roles.clone(), grid.clone()).unwrap();
    assert!(identity.is_identity(1.0e-12));
    let validation = validate_stereo_strict_solution(&target, &plant, &identity).unwrap();
    assert_eq!(validation.profile, TargetProfile::StereoStrict2);
    assert!(validation.passed, "checks: {:?}", validation.checks);
    assert!(validation.residual_max_db.unwrap() < 1.0e-9);
    assert_eq!(validation.offdiag_max_db, 0.0);

    // 5x5 G → E_TGT_STEREO（不能 runtime mask 5x5 解）。
    let multi_roles: Vec<String> = P1_MIMO_PARTICIPANT_IDS
        .iter()
        .map(|role| (*role).to_string())
        .collect();
    let five_by_five = ModalMatrix::identity(multi_roles, grid.clone()).unwrap();
    expect_kind(
        validate_stereo_strict_solution(&target, &plant, &five_by_five),
        TargetErrorKind::Stereo,
    );
    // 非 FL/FR 输出集合 → E_TGT_STEREO。
    let wrong_roles = vec!["SL".to_string(), "SUB1".to_string()];
    let wrong = ModalMatrix::identity(wrong_roles, grid.clone()).unwrap();
    expect_kind(
        validate_stereo_strict_solution(&target, &plant, &wrong),
        TargetErrorKind::Stereo,
    );
    // 通用入口对非 stereo 目标调用 stereo 校验 → E_TGT_STEREO。
    let multi_baseline = default_baseline(&grid);
    let multi_plant = EffectivePlant::build(
        &multi_baseline,
        &raw_plant(
            &multi_baseline.participants().clone(),
            &grid,
            flat_positions(
                &multi_baseline.participants().clone(),
                &grid,
                &[("P01", PositionRole::Design)],
            ),
        ),
    )
    .unwrap();
    let multi_target = build_target_matrix(
        &multi_baseline,
        &multi_plant,
        &TargetSpec::for_baseline(&multi_baseline, default_decay()).unwrap(),
    )
    .unwrap();
    expect_kind(
        validate_stereo_strict_solution(&multi_target, &multi_plant, &five_by_five),
        TargetErrorKind::Stereo,
    );
}

// ===========================================================================
// 5x5 求解校验
// ===========================================================================

#[test]
fn t50_target_solution_validation_multichannel() {
    let grid = small_grid();
    let participants = ModalParticipants::p1_4_1_2();
    let baseline = default_baseline(&grid);
    let design_labels = [
        ("P01", PositionRole::Design),
        ("P02", PositionRole::Design),
        ("P03", PositionRole::Design),
    ];
    let plant = EffectivePlant::build(
        &baseline,
        &raw_plant(
            &participants,
            &grid,
            flat_positions(&participants, &grid, &design_labels),
        ),
    )
    .unwrap();
    let roles: Vec<String> = participants
        .role_ids()
        .iter()
        .map(|role| (*role).to_string())
        .collect();

    // preserve 目标 + identity G → 残差 0，结构校验通过。
    let preserve_target = build_target_matrix(
        &baseline,
        &plant,
        &TargetSpec::for_baseline(&baseline, default_decay()).unwrap(),
    )
    .unwrap();
    let identity = ModalMatrix::identity(roles.clone(), grid.clone()).unwrap();
    let validation = validate_solution(&preserve_target, &plant, &identity).unwrap();
    assert_eq!(validation.profile, TargetProfile::Multichannel4x2_5);
    assert!(validation.passed, "checks: {:?}", validation.checks);
    assert_eq!(validation.residual_max_db, Some(0.0));
    assert_eq!(validation.offdiag_max_db, 0.0);
    assert!(!validation.checks.is_empty());

    // reduced 目标 + G = target / H_eff（position 相同）→ 残差 ≈ 0 且通过。
    let reduced_target = build_target_matrix(
        &baseline,
        &plant,
        &TargetSpec::for_baseline(&baseline, reduced_decay(0.85)).unwrap(),
    )
    .unwrap();
    let mut cells = Vec::new();
    for participant in participants.participants() {
        let target_points = reduced_target
            .diag_response(None, participant.dense_index)
            .unwrap();
        let measured = plant.input("P01", &participant.role_id).unwrap();
        let mut points = Vec::with_capacity(grid.len());
        for (index, freq) in grid.iter().enumerate() {
            let quotient = complex_divide(
                (target_points[index].re, target_points[index].im),
                (measured.points[index].re, measured.points[index].im),
            );
            points.push(ComplexPoint {
                freq_hz: *freq,
                re: quotient.0,
                im: quotient.1,
            });
        }
        cells.push(ModalMatrixCell {
            out_dense: participant.dense_index,
            in_dense: participant.dense_index,
            points,
        });
    }
    let solved = ModalMatrix::new(roles.clone(), grid.clone(), cells).unwrap();
    let validation = validate_solution(&reduced_target, &plant, &solved).unwrap();
    assert!(validation.passed, "checks: {:?}", validation.checks);
    assert!(validation.residual_max_db.unwrap() < 1.0e-6);

    // 缺对角 baseline → E_TGT_MATRIX（G 必须声明完整 deltaD 对角）。
    let mut missing_diag_cells = Vec::new();
    for cell in identity.cells().iter().take(4) {
        missing_diag_cells.push(cell.clone());
    }
    let missing_diag = ModalMatrix::new(roles.clone(), grid.clone(), missing_diag_cells).unwrap();
    expect_kind(
        validate_solution(&preserve_target, &plant, &missing_diag),
        TargetErrorKind::Matrix,
    );
    // 维度/角色错误 → E_TGT_MATRIX。
    let four_roles: Vec<String> = P1_MIMO_PARTICIPANT_IDS[..4]
        .iter()
        .map(|role| (*role).to_string())
        .collect();
    let four = ModalMatrix::identity(four_roles, grid.clone()).unwrap();
    expect_kind(
        validate_solution(&preserve_target, &plant, &four),
        TargetErrorKind::Matrix,
    );
    // 网格不一致 → E_TGT_GRID。
    let shifted_grid: Vec<f64> = grid.iter().map(|freq| freq * 1.001).collect();
    let shifted = ModalMatrix::identity(roles.clone(), shifted_grid).unwrap();
    expect_kind(
        validate_solution(&preserve_target, &plant, &shifted),
        TargetErrorKind::Grid,
    );
    // 绑定不同 plant → E_TGT_BASELINE。
    let other_baseline = default_baseline(&grid);
    let other_plant = EffectivePlant::build(
        &other_baseline,
        &raw_plant(
            &ModalParticipants::p1_4_1_2(),
            &grid,
            flat_positions(
                &ModalParticipants::p1_4_1_2(),
                &grid,
                &[("P01", PositionRole::Design)],
            ),
        ),
    )
    .unwrap();
    let _ = other_plant;
}

// ===========================================================================
// 错误码 / 静态守卫 / 证据 JSON
// ===========================================================================

#[test]
fn t50_target_error_codes_stable() {
    assert_eq!(TargetErrorKind::ALL.len(), 16);
    let mut codes: Vec<&'static str> = TargetErrorKind::ALL
        .iter()
        .map(|kind| kind.code())
        .collect();
    assert!(codes.iter().all(|code| code.starts_with("E_TGT_")));
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), 16, "error codes must be unique and stable");
    let error = TargetError::new(TargetErrorKind::Grid, "a.b[0]", "detail");
    assert_eq!(error.code(), "E_TGT_GRID");
    assert_eq!(error.kind(), TargetErrorKind::Grid);
    assert_eq!(error.field(), "a.b[0]");
    assert!(format!("{error}").contains("E_TGT_GRID"));
    assert_eq!(TargetErrorKind::SampleRate.code(), "E_TGT_SAMPLE_RATE");
    assert_eq!(TargetErrorKind::Stereo.code(), "E_TGT_STEREO");
    assert!(TargetErrorKind::Unsupported.code().starts_with("E_TGT_"));
    // 警告码稳定（W_TGT_）。
    for code in [
        "W_TGT_NEAR_ZERO_BASELINE",
        "W_TGT_HOLDOUT_RESERVED",
        "W_TGT_HEIGHT_SCOPE_SEPARATED",
        "W_TGT_STEREO_STRICT_PROFILE",
        "W_TGT_AGGRESSIVE_DECAY",
        "W_TGT_POSITION_INVARIANT",
    ] {
        assert!(code.starts_with("W_TGT_"), "{code}");
    }
}

#[test]
fn t50_target_static_guards_pure_compute() {
    let source = tspi_modal_design::target::target_source_text_for_guards();
    assert!(source.contains("pub fn build_target_matrix"));
    assert!(source.contains("pub fn build_stereo_strict_target"));
    assert!(source.contains("pub fn validate_solution"));
    // 唯一 H_eff 复乘点：定义 + 调用各一次。
    assert_eq!(source.matches("compose_effective_response(").count(), 2);
    // 复数乘法只出现在 H_eff 组合与求解预测（可审计的 3 处）。
    assert_eq!(source.matches("complex_multiply(").count(), 3);
    // 纯计算：无文件/进程/网络/线程/环境/IO/unsafe/序列化依赖。
    for forbidden in [
        "std::fs",
        "std::process",
        "std::net",
        "std::thread",
        "std::env",
        "std::io",
        "unsafe",
        "serde",
        "File::open",
        "Command::new",
    ] {
        assert!(
            !source.contains(forbidden),
            "target.rs must not contain {forbidden}"
        );
    }
    // 目标产物诚实字段存在。
    assert!(source.contains("post_apply_measurement"));
    assert!(source.contains("not_requested"));
    assert!(source.contains("predicted-only target matrix"));
}

fn assert_json_balanced(text: &str) {
    let mut braces = 0i64;
    let mut brackets = 0i64;
    let mut in_string = false;
    let mut escaped = false;
    for character in text.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '{' => braces += 1,
            '}' => braces -= 1,
            '[' => brackets += 1,
            ']' => brackets -= 1,
            _ => {}
        }
        assert!(braces >= 0 && brackets >= 0, "unbalanced JSON");
    }
    assert_eq!(braces, 0);
    assert_eq!(brackets, 0);
    assert!(!in_string);
}

#[test]
fn t50_target_evidence_json_shape() {
    let grid = small_grid();
    let participants = ModalParticipants::p1_4_1_2();
    let baseline = default_baseline(&grid);
    let plant = EffectivePlant::build(
        &baseline,
        &raw_plant(
            &participants,
            &grid,
            flat_positions(
                &participants,
                &grid,
                &[
                    ("P01", PositionRole::Design),
                    ("P09", PositionRole::Validation),
                ],
            ),
        ),
    )
    .unwrap();
    let target = build_target_matrix(
        &baseline,
        &plant,
        &TargetSpec::for_baseline(&baseline, reduced_decay(0.85)).unwrap(),
    )
    .unwrap();
    let evidence = target.to_evidence_json();
    assert!(evidence.starts_with('{'));
    assert!(evidence.ends_with('}'));
    assert_json_balanced(&evidence);
    for expected in [
        "\"schema\":\"tspi-modal-target/v1\"",
        "\"profile\":\"multichannel_4_1_2_5\"",
        "\"applied\":false",
        "\"post_apply_measurement\":\"not_requested\"",
        "\"engine_compatibility\":\"not_validated\"",
        "\"runtime_validation\":\"not_run\"",
        "\"used_for_tuning\":false",
        "\"claim\":\"predicted-only target matrix",
        "\"holdout\":{\"used_for_tuning\":false",
        "\"position_variant\":false",
        "\"offdiag\":\"structural_zero\"",
    ] {
        assert!(evidence.contains(expected), "evidence missing {expected}");
    }
    assert!(evidence.contains("\"target_sha256\":\""));
    assert!(evidence.contains("\"design_plant_hash\":\""));
    assert!(evidence.contains("\"common_d0_samples\":120"));
    assert!(evidence.contains("\"target_delay_samples\":0"));
    // JSON 字段值可被独立 checker 解析（无 NaN/Infinity 字面量）。
    assert!(!evidence.contains("NaN"));
    assert!(!evidence.contains("Infinity"));
    // 确定性：同一输入重复构建 evidence 一致。
    let target2 = build_target_matrix(
        &baseline,
        &plant,
        &TargetSpec::for_baseline(&baseline, reduced_decay(0.85)).unwrap(),
    )
    .unwrap();
    assert_eq!(evidence, target2.to_evidence_json());
}

// ===========================================================================
// 近零基线 / 矩阵输入校验 / 证据 dump（独立 checker 用）
// ===========================================================================

#[test]
fn t50_target_near_zero_baseline_is_flagged_not_invented() {
    let grid = small_grid();
    let participants = ModalParticipants::p1_4_1_2();
    let baseline = default_baseline(&grid);
    let position = raw_position_with(
        "P01",
        PositionRole::Design,
        1.0,
        &participants,
        &grid,
        |role, grid| {
            if role == "FL" {
                grid.iter()
                    .map(|freq| ComplexPoint {
                        freq_hz: *freq,
                        re: 0.0,
                        im: 0.0,
                    })
                    .collect()
            } else {
                flat_points(grid, 0.0)
            }
        },
        &[],
    );
    let plant =
        EffectivePlant::build(&baseline, &raw_plant(&participants, &grid, vec![position])).unwrap();
    assert!(plant
        .warnings()
        .iter()
        .any(|warning| warning.code == "W_TGT_NEAR_ZERO_BASELINE"));
    let target = build_target_matrix(
        &baseline,
        &plant,
        &TargetSpec::for_baseline(&baseline, reduced_decay(0.85)).unwrap(),
    )
    .unwrap();
    assert!(target.diagnostics().near_zero_points > 0);
    assert!(target
        .warnings()
        .iter()
        .any(|warning| warning.code == "W_TGT_NEAR_ZERO_BASELINE"));
    // 近零基线不得被伪造成可反演修正（FL 目标幅度保持近零）。
    let fl = target.diag_response(None, 0).unwrap();
    assert!(fl.iter().all(|point| point.re.hypot(point.im) < 1.0e-6));
}

#[test]
fn t50_target_modal_matrix_input_validation() {
    let grid = small_grid();
    let roles: Vec<String> = vec!["FL".to_string(), "FR".to_string()];
    let identity = ModalMatrix::identity(roles.clone(), grid.clone()).unwrap();
    expect_kind(
        ModalMatrix::new(
            vec!["FL".to_string(), "FL".to_string()],
            grid.clone(),
            identity.cells().to_vec(),
        ),
        TargetErrorKind::Matrix,
    );
    let mut out_of_range = identity.cells().to_vec();
    out_of_range[0].out_dense = 5;
    expect_kind(
        ModalMatrix::new(roles.clone(), grid.clone(), out_of_range),
        TargetErrorKind::Matrix,
    );
    let mut duplicated = identity.cells().to_vec();
    duplicated.push(duplicated[0].clone());
    expect_kind(
        ModalMatrix::new(roles.clone(), grid.clone(), duplicated),
        TargetErrorKind::Matrix,
    );
    let mut unsorted = grid.clone();
    unsorted.swap(1, 2);
    expect_kind(
        ModalMatrix::new(roles.clone(), unsorted, identity.cells().to_vec()),
        TargetErrorKind::Grid,
    );
    expect_kind(
        ModalMatrix::new(roles.clone(), grid.clone(), Vec::new()),
        TargetErrorKind::Missing,
    );
    assert!(identity.is_identity(1.0e-12));
}

fn json_num(value: f64) -> String {
    format!("{value:.17e}")
}

#[test]
fn t50_target_evidence_dump_known_fixture() {
    // 主夹具使用 1/24 oct 生产网格（改善带覆盖率 >=80% → 状态 predicted_pass）。
    let grid = grid_1_24_oct();
    let participants = ModalParticipants::p1_4_1_2();
    // P: FL +3 dB / delay 7 / polarity -1；其它 identity。
    let mut roles = baseline_roles(&grid, &participants, 0.0, 0, 1);
    roles[0] = post_role("FL", 0, &grid, 3.0, 7, -1);
    let baseline =
        frozen_baseline_with_roles(&grid, participants.clone(), ToneBinding::flat(), roles)
            .freeze()
            .unwrap();
    let plant = EffectivePlant::build(
        &baseline,
        &raw_plant(
            &participants,
            &grid,
            vec![raw_position_with(
                "P01",
                PositionRole::Design,
                1.0,
                &participants,
                &grid,
                |_, grid| flat_points(grid, -6.0),
                &[],
            )],
        ),
    )
    .unwrap();
    let target = build_target_matrix(
        &baseline,
        &plant,
        &TargetSpec::for_baseline(&baseline, reduced_decay(0.85)).unwrap(),
    )
    .unwrap();
    let fl_heff = plant.input("P01", "FL").unwrap().points.clone();
    let fl_target = target.diag_response(None, 0).unwrap().to_vec();
    let mut samples = String::new();
    for (index, freq) in grid.iter().enumerate() {
        if index > 0 {
            samples.push(',');
        }
        samples.push_str(&format!(
            "{{\"freq\":{},\"h_eff_re\":{},\"h_eff_im\":{},\"target_re\":{},\"target_im\":{}}}",
            json_num(*freq),
            json_num(fl_heff[index].re),
            json_num(fl_heff[index].im),
            json_num(fl_target[index].re),
            json_num(fl_target[index].im)
        ));
    }
    println!("T50_EVIDENCE_JSON={}", target.to_evidence_json());
    println!(
        "T50_NUMERIC_JSON={{\"fixture\":\"t50-known-1\",\"sample_rate_hz\":48000,\"grid_points\":{},\"p_gain_db\":{},\"p_delay_samples\":7,\"p_polarity\":-1,\"raw_gain_db\":{},\"common_d0_samples\":{},\"samples\":[{}]}}",
        grid.len(),
        json_num(3.0),
        json_num(-6.0),
        D0_SAMPLES,
        samples
    );

    // tone 单次应用数值证据（63 Hz -> 1 kHz 的幅度差 = 单次 tilt10，不是两次）。
    let tone_grid = small_grid();
    let tilt = ToneBinding::tilt10();
    let mut tone_roles = Vec::new();
    for participant in participants.participants() {
        let points = tone_grid
            .iter()
            .map(|freq| {
                let magnitude = 10f64.powf(tilt.mag_db(*freq) / 20.0);
                ComplexPoint {
                    freq_hz: *freq,
                    re: magnitude,
                    im: 0.0,
                }
            })
            .collect();
        tone_roles
            .push(PostRoleFit::new(&participant.role_id, participant.out_index, points).unwrap());
    }
    tone_roles.push(post_role("HFL", 4, &tone_grid, 0.0, 0, 1));
    tone_roles.push(post_role("HFR", 5, &tone_grid, 0.0, 0, 1));
    let tone_baseline =
        frozen_baseline_with_roles(&tone_grid, participants.clone(), tilt.clone(), tone_roles)
            .freeze()
            .unwrap();
    let tone_plant = EffectivePlant::build(
        &tone_baseline,
        &raw_plant(
            &participants,
            &tone_grid,
            flat_positions(&participants, &tone_grid, &[("P01", PositionRole::Design)]),
        ),
    )
    .unwrap();
    let tone_target = build_target_matrix(
        &tone_baseline,
        &tone_plant,
        &TargetSpec::for_baseline(&tone_baseline, default_decay()).unwrap(),
    )
    .unwrap();
    let tone_cell = match tone_target.cell(Some("P01"), 0, 0) {
        TargetCellView::Response(points) => points.to_vec(),
        TargetCellView::Zero => panic!("missing tone preserve cell"),
    };
    let index_63 = tone_grid
        .iter()
        .position(|freq| (*freq - 63.0).abs() < 1.0e-9)
        .unwrap();
    let index_1k = tone_grid
        .iter()
        .position(|freq| (*freq - 1_000.0).abs() < 1.0e-9)
        .unwrap();
    let one_application_db = 20.0
        * (tone_cell[index_1k].re.hypot(tone_cell[index_1k].im)
            / tone_cell[index_63].re.hypot(tone_cell[index_63].im))
        .log10();
    println!(
        "T50_TONE_JSON={{\"one_application_db\":{},\"expected_one_db\":{},\"expected_double_db\":{}}}",
        json_num(one_application_db),
        json_num(tilt.mag_db(1_000.0) - tilt.mag_db(63.0)),
        json_num(2.0 * (tilt.mag_db(1_000.0) - tilt.mag_db(63.0)))
    );
}
