//! T49 private integration tests — 公共时基/相位/ppm 重建（crate khaen-modal-design）.
//!
//! 运行（在 rt-stack/ 下）：cargo test -p khaen-modal-design timebase
//! 文件位置：crates/modal-design/tests/T49/main.rs（leaf 私有测试；Cargo 自动发现为
//! integration test target T49；由 integrator 决定是否在 public_registration.rs 另注册）。
//!
//! 覆盖（T49 DoD）：
//! - 已知夹具（delay=137.25 samples、ppm=+7.5、polarity=-1）独立解析：输入只有 T48
//!   primitives（render_frame_index/nominal_start/actual_start/repeat_starts/
//!   analysis.delay/polarity/phase_ref/fr），断言由闭式解析量 e^{-j2πfτ/fs}、
//!   1+ppm·1e-6、sign(-1) 独立导出，不把 producer 的答案当输入；
//! - T6 FC-04 公共整数 D0（[CommonD0]）：去相位**只扣 D0 一次**，绝不扣 T48
//!   measured delay；137.25 + 非零 D0（0/120）双扣夹具、per-participant residual
//!   provenance、D0 相对相位不变性；P3：delay_coarse/delay_ms 与 delay_samples 自洽复校。
//! - coarse/frac 分解（137 + 0.25）与 τ=0/137/411 线性；23 Hz–10 kHz 复响应 ≤1e-9；
//! - 禁止双去相位：phase_ref=aligned 只减一次；phase_ref=absolute 一律不再减
//!   （delay_removed_samples=0 + 反向夹具证明没有减）；delay_removed v1 不支持 → 拒；
//! - clock_ppm 三独立估计（render/nominal LSQ、onset-delay LSQ、repeat 间隔）交叉一致，
//!   与 diag 声明对拍；resample 残差、显著漂移未校正、声明/估计冲突均拒；
//! - 单次极性归一（第二次 → E_TB_DOUBLE_POLARITY）；unknown 极性拒；
//! - MOTU 电学 reference 独立通道（device/independent/raw 声明一致性）与相对响应只除一次；
//! - slot overlap/spacing/repeat spacing 非致命诊断；
//! - 两 position 公共 session/epoch/graph + participants 一致性；
//! - 真实 file-backed T48 ingest 端到端（JSON/SHA256SUMS/read_bundle）→ reconstruct_bundle；
//! - 静态守卫：本模块无 I/O/进程/网络/线程；去相位旋转只有一处。
//!
//! 本文件只构造内存夹具与临时目录（测试二进制旁，可重建，不在仓库内）；不采音、不碰板端。

use std::fs;
use std::path::{Path, PathBuf};

use khaen_modal_design::ingest::{
    read_bundle, sha256_hex, AnalysisMeta, CaptureFile, CaptureInfo, DriftCorrection, FrPoint,
    IngestBundle, IngestMeasurement, IngestPosition, IngestSession, MicClock, MicInfo,
    PlaybackInfo, PositionMeta, PositionRole, ReferenceInfo, SampleFormat, SourceAnalysis,
    SpeakerInfo, StimulusInfo, TimebaseDiag, ToolInfo, CAPTURE_FILE, POSITION_FILE, REFERENCE_FILE,
    SESSION_FILE, SHA256SUMS_FILE,
};
use khaen_modal_design::timebase::{
    de_phase_analysis, de_phase_reference, reconstruct_bundle, reconstruct_common_phase,
    reconstruct_position, ClockMethod, CommonD0, ComplexPoint, DelaySplit, PhaseBasis,
    PolaritySource, ResolvedPolarity, TimebaseError, TimebaseErrorKind, TimebaseSummary,
    TimebaseWarning, COMMON_D0_SANITY_SAMPLES, DELAY_MS_ABSOLUTE_TOLERANCE_MS,
    DELAY_ORIGIN_SPREAD_MAX_SAMPLES, TIMEBASE_SCHEMA,
};

// ===========================================================================
// 常量
// ===========================================================================

const FS: u32 = 48_000;
const SESSION_ID: &str = "mimo-4.1.2-20261120-a";
const EPOCH_ID: u64 = 7;
const GRAPH_ID: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
const SWEEP_SECONDS: f64 = 0.1;
const GAP_SECONDS: f64 = 0.02;
const PRE_SECONDS: f64 = 0.01;
const POST_SECONDS: f64 = 0.01;
const RENDER0: u64 = 1_000_000;
const CAPTURE0: f64 = 480.0;
const CAPTURE_FRAMES: u64 = 200_000;
const PARTICIPANTS: [(&str, u32); 5] = [("FL", 0), ("FR", 1), ("SL", 2), ("SR", 3), ("SUB1", 7)];
const PHASE_REF_ALIGNED: &str =
    "aligned_segment(已去粗延迟); absolute_phase_deg = phase_deg - 360*f*delay_samples/fs";

/// 已知夹具真值（T49 DoD 明确给出）：137.25 samples / +7.5 ppm / polarity -1。
const KNOWN_DELAY: f64 = 137.25;
const KNOWN_PPM: f64 = 7.5;
const DECLARED_SIGMA: f64 = 4.0;

// ===========================================================================
// 基础 helper
// ===========================================================================

fn fake_sha(seed: u32) -> String {
    format!("{seed:064x}")
}

fn sppf(ppm: f64) -> f64 {
    1.0 + ppm / 1.0e6
}

fn slot_interval_frames(repeats: u32) -> f64 {
    repeats as f64 * (SWEEP_SECONDS + GAP_SECONDS) * FS as f64
}

fn repeat_interval_samples(ppm: f64) -> f64 {
    (SWEEP_SECONDS + GAP_SECONDS) * FS as f64 * sppf(ppm)
}

/// 独立解析用的已知系统相位（平滑、非平凡）；测试期望值直接由它闭式算出。
fn phase0_deg(freq_hz: f64) -> f64 {
    2.0 * (freq_hz / 100.0).sqrt()
}

fn reference_phase0_deg(freq_hz: f64) -> f64 {
    -1.0 * (freq_hz / 1000.0).sqrt()
}

/// FR 测试网格：20 Hz–10 kHz，1/6 oct（约 55 点；覆盖低频判据与高频去相位灵敏度）。
fn fr_freqs() -> Vec<f64> {
    let mut out = Vec::new();
    let mut index = 0usize;
    loop {
        let freq = 20.0 * 2.0f64.powf(index as f64 / 6.0);
        if freq > 10_000.0 {
            break;
        }
        out.push(freq);
        index += 1;
    }
    out
}

fn expect_error<T>(result: Result<T, TimebaseError>, kind: TimebaseErrorKind, code: &str) {
    match result {
        Ok(_) => panic!("expected {code} but reconstruction succeeded"),
        Err(error) => {
            assert_eq!(error.kind(), kind, "unexpected error kind: {error}");
            assert_eq!(error.code(), code, "unexpected error code: {error}");
        }
    }
}

fn assert_close(actual: f64, expected: f64, tolerance: f64, context: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{context}: actual {actual} vs expected {expected} (tolerance {tolerance})"
    );
}

fn expected_phase0(analysis: &SourceAnalysis) -> Vec<ComplexPoint> {
    analysis
        .fr
        .iter()
        .map(|point| {
            ComplexPoint::from_polar(point.freq_hz, point.db, phase0_deg(point.freq_hz))
                .expect("analytic point")
        })
        .collect()
}

/// 独立解析：按公共整数 D0 去相位**一次**后的期望复响应。
///
/// 只使用 DoD 已知夹具真值（phase0_deg 闭式 + analysis.delay_samples provenance）与 D0，
/// 不读取 producer 的 phase_deg 结果，因此可独立验证"只扣 D0、不扣 measured delay"。
fn expected_after_d0(analysis: &SourceAnalysis, d0: CommonD0) -> Vec<ComplexPoint> {
    let fs = analysis.meta.sample_rate_hz as f64;
    let residual = analysis.delay_samples - d0.as_f64();
    analysis
        .fr
        .iter()
        .map(|point| {
            ComplexPoint::from_polar(
                point.freq_hz,
                point.db,
                phase0_deg(point.freq_hz) + 360.0 * point.freq_hz * residual / fs,
            )
            .expect("analytic point")
        })
        .collect()
}

fn assert_complex_close(
    actual: &[ComplexPoint],
    expected: &[ComplexPoint],
    tolerance: f64,
    context: &str,
) {
    assert_eq!(actual.len(), expected.len(), "{context}: length");
    for (index, (value, want)) in actual.iter().zip(expected.iter()).enumerate() {
        assert_close(
            value.re,
            want.re,
            tolerance,
            &format!("{context}[{index}].re"),
        );
        assert_close(
            value.im,
            want.im,
            tolerance,
            &format!("{context}[{index}].im"),
        );
    }
}

// ===========================================================================
// 夹具 spec / builders（直接构造 T48 强类型，不经过 JSON）
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BasisKind {
    Aligned,
    Absolute,
    Unknown,
    DelayRemoved,
}

impl BasisKind {
    fn text(self) -> &'static str {
        match self {
            BasisKind::Aligned => PHASE_REF_ALIGNED,
            BasisKind::Absolute => "absolute_phase_deg (already total phase)",
            BasisKind::Unknown => "mystery_phase_ref_v9",
            BasisKind::DelayRemoved => "delay_removed(D=120 samples)",
        }
    }
}

/// 生成相位时是否把已知延迟写进 phase_deg（模拟 producer 的 aligned 产物）；
/// 与 phase_ref 标签独立，用来构造"标签/数据不一致"的反向夹具。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PhaseSource {
    WithDelay,
    WithoutDelay,
}

#[derive(Debug, Clone, Copy)]
struct Spec {
    repeats: u32,
    ppm: f64,
    declared_ppm: f64,
    declared_sigma: f64,
    correction: DriftCorrection,
    delay: f64,
    origin_offset: f64,
    polarity: i32,
    d0: i64,
    basis: BasisKind,
    phase_source: PhaseSource,
    slot_interval_frames: Option<f64>,
    capture0: f64,
    render0: u64,
    reference: bool,
}

impl Spec {
    fn known() -> Self {
        Spec {
            repeats: 3,
            ppm: KNOWN_PPM,
            declared_ppm: KNOWN_PPM,
            declared_sigma: DECLARED_SIGMA,
            correction: DriftCorrection::None,
            delay: KNOWN_DELAY,
            origin_offset: 0.0,
            polarity: -1,
            d0: 0,
            basis: BasisKind::Aligned,
            phase_source: PhaseSource::WithDelay,
            slot_interval_frames: None,
            capture0: CAPTURE0,
            render0: RENDER0,
            reference: false,
        }
    }
}

fn generated_phase_deg(freq_hz: f64, spec: &Spec) -> f64 {
    match spec.phase_source {
        PhaseSource::WithDelay => phase0_deg(freq_hz) + 360.0 * freq_hz * spec.delay / FS as f64,
        PhaseSource::WithoutDelay => phase0_deg(freq_hz),
    }
}

fn d0(spec: &Spec) -> CommonD0 {
    CommonD0::new(spec.d0).expect("declared common D0")
}

fn fr_points(spec: &Spec) -> Vec<FrPoint> {
    fr_freqs()
        .iter()
        .enumerate()
        .map(|(index, freq_hz)| FrPoint {
            freq_hz: *freq_hz,
            db: -1.0 - 0.01 * index as f64,
            phase_deg: generated_phase_deg(*freq_hz, spec),
        })
        .collect()
}

fn test_meta() -> AnalysisMeta {
    AnalysisMeta {
        sample_rate_hz: FS,
        playback_channels: 22,
        sweep_start_hz: 15.0,
        sweep_end_hz: 600.0,
        sweep_seconds: SWEEP_SECONDS,
        level_dbfs: -30.0,
        pre_seconds: PRE_SECONDS,
        post_seconds: POST_SECONDS,
        playback_device: "hw:motu,0".to_string(),
        capture_channels: 1,
        capture_device: "hw:umik1,0".to_string(),
        created_unix: 1_794_725_050,
    }
}

fn source_analysis(out_index: u32, spec: &Spec) -> SourceAnalysis {
    SourceAnalysis {
        channel: out_index,
        delay_samples: spec.delay,
        delay_ms: spec.delay / FS as f64 * 1000.0,
        delay_coarse_samples: spec.delay.floor() as i64,
        polarity: spec.polarity,
        peak_db: -22.0,
        phase_1k_deg: generated_phase_deg(1000.0, spec),
        snr_db: 35.0,
        snr_band_db: None,
        snr_band_hz: None,
        phase_ref: spec.basis.text().to_string(),
        clock_ppm: None,
        clock_ppm_ok: None,
        clock_ppm_uncertainty_ppm: None,
        clock_ppm_significant: None,
        drift_corrected: None,
        onset_invalid: None,
        measurement_role: None,
        position: None,
        repeats_count: None,
        fr: fr_points(spec),
        fr_modal: Vec::new(),
        meta: test_meta(),
    }
}

fn timebase_diag(speaker: &str, out_index: u32, slot: u32, spec: &Spec) -> TimebaseDiag {
    let slot_interval = spec
        .slot_interval_frames
        .unwrap_or_else(|| slot_interval_frames(spec.repeats));
    let render = spec.render0 + (slot as f64 * slot_interval).round() as u64;
    let nominal = spec.capture0 + (render as f64 - spec.render0 as f64) * sppf(spec.ppm);
    let lag = spec.origin_offset + spec.delay;
    let actual = nominal + lag;
    let repeat_interval = repeat_interval_samples(spec.ppm);
    let repeat_starts = (0..spec.repeats)
        .map(|index| actual + index as f64 * repeat_interval)
        .collect();
    let drift_significant = spec.declared_ppm.abs() > 2.0 * spec.declared_sigma;
    TimebaseDiag {
        session_id: SESSION_ID.to_string(),
        epoch_id: EPOCH_ID,
        graph_id: GRAPH_ID.to_string(),
        speaker: speaker.to_string(),
        channel: out_index,
        slot,
        capture_file: CAPTURE_FILE.to_string(),
        render_frame_index: render,
        capture_sample_index: actual.round() as u64,
        nominal_start: nominal,
        actual_start: actual,
        lag_samples: lag,
        repeat_starts,
        repeats: spec.repeats,
        clock: MicClock {
            clock_ppm: spec.declared_ppm,
            clock_ppm_uncertainty_ppm: spec.declared_sigma,
            drift_significant,
            clock_ppm_ok: true,
            correction: spec.correction,
        },
        onset_invalid: false,
    }
}

fn measurement(label: &str, slot: usize, spec: &Spec) -> IngestMeasurement {
    let (speaker, out_index) = PARTICIPANTS[slot];
    IngestMeasurement {
        speaker: speaker.to_string(),
        out_index,
        slot: slot as u32,
        analysis_relative_path: format!(
            "positions/{label}/{speaker}-ch{out_index:02}.analysis.json"
        ),
        analysis_sha256: fake_sha(10 + slot as u32),
        diag_relative_path: format!("positions/{label}/{speaker}-ch{out_index:02}.diag.json"),
        diag_sha256: fake_sha(20 + slot as u32),
        analysis: source_analysis(out_index, spec),
        diag: timebase_diag(speaker, out_index, slot as u32, spec),
    }
}

fn capture_file(
    _spec: &Spec,
    channels: u16,
    sample_format: SampleFormat,
    path: String,
) -> CaptureFile {
    let bytes_per_sample = sample_format.bytes_per_sample();
    CaptureFile {
        relative_path: path,
        sha256: fake_sha(0xC0FFEE),
        bytes: CAPTURE_FRAMES * channels as u64 * bytes_per_sample,
        frames: CAPTURE_FRAMES,
        channels,
        sample_rate_hz: FS,
        sample_format,
    }
}

fn position_with(label: &str, role: PositionRole, weight: f64, spec: &Spec) -> IngestPosition {
    let measurements = (0..PARTICIPANTS.len())
        .map(|slot| measurement(label, slot, spec))
        .collect();
    let reference = if spec.reference {
        Some(capture_file(
            spec,
            2,
            SampleFormat::S32LE,
            format!("positions/{label}/{REFERENCE_FILE}"),
        ))
    } else {
        None
    };
    IngestPosition {
        dir: label.to_string(),
        meta: PositionMeta {
            name: label.to_string(),
            role,
            weight,
            coordinates_m: None,
            mic_orientation: Some("90deg".to_string()),
            created_unix: Some(1_794_725_100),
        },
        meta_sha256: fake_sha(3),
        capture: capture_file(
            spec,
            1,
            SampleFormat::S24_3LE,
            format!("positions/{label}/{CAPTURE_FILE}"),
        ),
        reference,
        measurements,
    }
}

fn make_position(spec: &Spec) -> IngestPosition {
    position_with("p01-mlp", PositionRole::Design, 1.0, spec)
}

fn make_session(spec: &Spec) -> IngestSession {
    IngestSession {
        session_id: SESSION_ID.to_string(),
        layout: "4.1.2".to_string(),
        sample_rate_hz: FS,
        block_frames: Some(128),
        participants: PARTICIPANTS
            .iter()
            .map(|(name, _)| name.to_string())
            .collect(),
        speakers: PARTICIPANTS
            .iter()
            .map(|(name, out_index)| SpeakerInfo {
                name: name.to_string(),
                out_index: *out_index,
                kind: if *name == "SUB1" {
                    "subwoofer".to_string()
                } else {
                    "main".to_string()
                },
                allowed_band_hz: None,
            })
            .collect(),
        mic: MicInfo {
            model: "UMIK-1".to_string(),
            serial: "718-7696".to_string(),
            cal_sha256: fake_sha(0xAA),
            orientation: "90deg".to_string(),
            clock_domain: "usb".to_string(),
        },
        playback: PlaybackInfo {
            device: "hw:motu,0".to_string(),
            channels: 22,
            sample_format: SampleFormat::S32LE,
        },
        capture: CaptureInfo {
            device: "hw:umik1,0".to_string(),
            channels: 1,
            sample_format: SampleFormat::S24_3LE,
            pre_seconds: PRE_SECONDS,
            post_seconds: POST_SECONDS,
        },
        reference: if spec.reference {
            Some(ReferenceInfo {
                device: "hw:motu,0".to_string(),
                channels: 2,
                sample_format: SampleFormat::S32LE,
                independent: true,
            })
        } else {
            None
        },
        stimulus: StimulusInfo {
            kind: "log_sweep".to_string(),
            start_hz: 15.0,
            end_hz: 600.0,
            seconds: SWEEP_SECONDS,
            level_dbfs: -30.0,
            fade_ms: 10.0,
            repeats: spec.repeats,
            repeat_gap_seconds: GAP_SECONDS,
        },
        room_temperature_c: 22.0,
        tool: ToolInfo {
            name: "tspi-audio-daemon".to_string(),
            version: "0.1.0".to_string(),
        },
        created_unix: Some(1_794_725_000),
        claim: None,
    }
}

fn bundle(session: IngestSession, positions: Vec<IngestPosition>) -> IngestBundle {
    IngestBundle {
        root: PathBuf::from("C:/t49-direct-fixtures"),
        session,
        session_sha256: fake_sha(99),
        positions,
        files: Vec::new(),
        warnings: Vec::new(),
    }
}

// ===========================================================================
// 夹具变异 helper
// ===========================================================================

fn set_lag_shift(position: &mut IngestPosition, slot: usize, delta_lag: f64) {
    let diag = &mut position.measurements[slot].diag;
    let lag = diag.lag_samples + delta_lag;
    let actual = diag.nominal_start + lag;
    diag.lag_samples = lag;
    diag.actual_start = actual;
    diag.capture_sample_index = actual.round() as u64;
    for start in &mut diag.repeat_starts {
        *start += delta_lag;
    }
}

fn stretch_repeats(position: &mut IngestPosition, factors: &[f64]) {
    for (index, measurement) in position.measurements.iter_mut().enumerate() {
        let factor = factors[index % factors.len()];
        let diag = &mut measurement.diag;
        let interval = diag.repeat_starts[1] - diag.repeat_starts[0];
        let base = diag.repeat_starts[0];
        for repeat in 1..diag.repeat_starts.len() {
            diag.repeat_starts[repeat] = base + repeat as f64 * interval * factor;
        }
    }
}

fn reference_analysis(spec: &Spec) -> SourceAnalysis {
    let delay = 50.0;
    let mut analysis = source_analysis(0, spec);
    analysis.measurement_role = Some("reference".to_string());
    analysis.delay_samples = delay;
    analysis.delay_ms = delay / FS as f64 * 1000.0;
    analysis.delay_coarse_samples = 50;
    analysis.polarity = 1;
    analysis.phase_ref = PHASE_REF_ALIGNED.to_string();
    analysis.phase_1k_deg = reference_phase0_deg(1000.0) + 360.0 * 1000.0 * delay / FS as f64;
    analysis.fr = fr_freqs()
        .iter()
        .enumerate()
        .map(|(index, freq_hz)| FrPoint {
            freq_hz: *freq_hz,
            db: -2.0 - 0.005 * index as f64,
            phase_deg: reference_phase0_deg(*freq_hz) + 360.0 * freq_hz * delay / FS as f64,
        })
        .collect();
    analysis
}

// ===========================================================================
// 1) 已知夹具 137.25 / +7.5 ppm / -1 极性：独立解析
// ===========================================================================

#[test]
fn t49_timebase_known_fixture_137_25_delay_ppm_polarity() {
    let spec = Spec::known();
    let session = make_session(&spec);
    let position = make_position(&spec);
    let timebase =
        reconstruct_position(&position, &session, d0(&spec)).expect("known fixture timebase");

    assert_eq!(timebase.label, "p01-mlp");
    assert_eq!(timebase.role, PositionRole::Design);
    assert_eq!(timebase.session_id, SESSION_ID);
    assert_eq!(timebase.epoch_id, EPOCH_ID);
    assert_eq!(timebase.graph_id, GRAPH_ID);
    assert_eq!(timebase.sample_rate_hz, FS);
    assert_eq!(
        timebase.participant_names(),
        vec!["FL", "FR", "SL", "SR", "SUB1"]
    );

    // --- clock：三独立估计 + 声明对拍（独立解析 1+ppm*1e-6） ---
    assert_close(
        timebase.clock.residual_ppm,
        KNOWN_PPM,
        1.0e-6,
        "residual ppm",
    );
    assert_close(
        timebase.clock.declared_ppm,
        KNOWN_PPM,
        1.0e-12,
        "declared ppm",
    );
    assert_eq!(timebase.clock.method, ClockMethod::Combined);
    assert_eq!(timebase.clock.estimates.len(), 3);
    let methods: Vec<ClockMethod> = timebase.clock.estimates.iter().map(|e| e.method).collect();
    assert!(methods.contains(&ClockMethod::RenderGridLsq));
    assert!(methods.contains(&ClockMethod::OnsetDelayLsq));
    assert!(methods.contains(&ClockMethod::RepeatInterval));
    for estimate in &timebase.clock.estimates {
        assert_close(estimate.ppm, KNOWN_PPM, 1.0e-6, estimate.method.as_str());
    }
    assert_eq!(timebase.clock.correction, DriftCorrection::None);
    assert!(!timebase.clock.residual_drift_significant);
    assert_close(
        timebase.clock.samples_per_frame,
        sppf(KNOWN_PPM),
        1.0e-12,
        "samples_per_frame",
    );

    // --- coarse/frac：137 + 0.25（从 diag primitives 独立恢复，也核对 analysis 分解） ---
    for slot in &timebase.slots {
        assert_eq!(
            slot.lag_split.coarse_samples, 137,
            "slot {} coarse",
            slot.slot
        );
        assert_close(slot.lag_split.frac_samples, 0.25, 1.0e-12, "slot frac");
        assert_eq!(slot.analysis_split.coarse_samples, 137);
        assert_close(
            slot.analysis_split.frac_samples,
            0.25,
            1.0e-12,
            "analysis frac",
        );
        assert_close(
            slot.analysis_frac_samples,
            0.25,
            1.0e-12,
            "analysis field frac",
        );
        assert_close(slot.lag_samples, KNOWN_DELAY, 1.0e-12, "lag");
        assert_close(
            slot.analysis_delay_samples,
            KNOWN_DELAY,
            1.0e-12,
            "analysis delay",
        );
        assert_close(
            slot.delay_origin_residual_samples,
            0.0,
            1.0e-12,
            "origin residual",
        );
        assert_eq!(slot.polarity.as_i8(), -1);
        assert_eq!(slot.phase_basis, PhaseBasis::AlignedSegment);
        assert_eq!(slot.repeats, 3);
        assert_eq!(slot.repeat_render_frames.len(), 3);
    }
    assert_close(
        timebase.delay_origin_offset_samples,
        0.0,
        1.0e-12,
        "origin offset",
    );
    assert_close(
        timebase.delay_origin_spread_samples,
        0.0,
        1.0e-12,
        "origin spread",
    );

    // --- render<->capture 映射：独立解析 nominal = capture0 + (render-render0)*sppf ---
    for slot in &timebase.slots {
        let mapped = timebase
            .clock
            .capture_of_render(slot.render_frame_index as f64);
        assert_close(mapped, slot.nominal_start, 1.0e-6, "capture_of_render");
        let back = timebase.clock.render_of_capture(slot.actual_start);
        assert_close(
            back,
            slot.render_frame_index as f64 + KNOWN_DELAY / sppf(KNOWN_PPM),
            1.0e-3,
            "render_of_capture",
        );
    }

    // --- slot/repeat 诊断：无 overlap/spacing/repeat 告警 ---
    assert!(
        timebase
            .warnings
            .iter()
            .all(|warning| warning.code != TimebaseWarning::SLOT_OVERLAP
                && warning.code != TimebaseWarning::SLOT_SPACING
                && warning.code != TimebaseWarning::REPEAT_SPACING),
        "unexpected warnings: {:?}",
        timebase.warnings
    );
    assert_close(
        timebase.slot_interval_frames_median,
        slot_interval_frames(3),
        1.0e-9,
        "slot interval median",
    );

    // --- 去相位只扣公共整数 D0（本夹具 D0=0）：measured delay 保留为 per-participant residual ---
    assert_eq!(timebase.common_d0, d0(&spec));
    for slot in &timebase.slots {
        assert_close(
            slot.per_participant_residual_delay_samples,
            KNOWN_DELAY - d0(&spec).as_f64(),
            1.0e-12,
            "per-participant residual delay",
        );
        assert_eq!(slot.per_participant_residual_split.coarse_samples, 137);
        assert_close(
            slot.per_participant_residual_split.frac_samples,
            0.25,
            1.0e-12,
            "per-participant residual frac",
        );
    }
    let fl = position.measurement("FL").expect("FL measurement");
    let absolute = de_phase_analysis(&fl.analysis, d0(&spec), "P01/FL").expect("de-phase");
    assert_eq!(absolute.basis(), PhaseBasis::AlignedSegment);
    assert_eq!(absolute.common_d0(), d0(&spec));
    assert_close(
        absolute.delay_removed_samples(),
        d0(&spec).as_f64(),
        1.0e-12,
        "removed delay = declared D0",
    );
    assert!(
        absolute.polarity().is_none(),
        "polarity must not be pre-applied"
    );
    assert_complex_close(
        absolute.points(),
        &expected_after_d0(&fl.analysis, d0(&spec)),
        1.0e-9,
        "absolute after D0",
    );

    // --- 极性只乘一次（-1），第二次拒绝 ---
    let polarity = ResolvedPolarity::resolve(-1, PolaritySource::Analysis).expect("polarity");
    let raw: Vec<ComplexPoint> = absolute.points().to_vec();
    let mut normalized = absolute.clone();
    normalized
        .normalize_polarity(polarity)
        .expect("first normalize");
    for (after, before) in normalized.points().iter().zip(raw.iter()) {
        assert_close(after.re, -before.re, 1.0e-12, "normalized re");
        assert_close(after.im, -before.im, 1.0e-12, "normalized im");
    }
    assert_eq!(normalized.polarity().expect("stamp").as_i8(), -1);
    let second = normalized
        .normalize_polarity(polarity)
        .expect_err("second normalize");
    assert_eq!(second.code(), "E_TB_DOUBLE_POLARITY");

    // --- 摘要（T50/T54/T63/T66 消费字段） ---
    let summary = timebase.summary();
    assert_eq!(summary.label, "p01-mlp");
    assert_eq!(summary.role, "design");
    assert_eq!(summary.clock.method, "combined");
    assert!(summary.clock.independent);
    assert_eq!(summary.slots.len(), 5);
    assert_eq!(summary.slots[4].coarse_samples, 137);
    assert_close(summary.slots[4].frac_samples, 0.25, 1.0e-12, "summary frac");
    assert_eq!(summary.common_d0_samples, 0);
    assert_close(
        summary.slots[4].per_participant_residual_delay_samples,
        KNOWN_DELAY,
        1.0e-12,
        "summary residual delay",
    );
    assert_eq!(
        summary.slots[4].per_participant_residual_coarse_samples,
        137
    );
    assert_close(
        summary.slots[4].per_participant_residual_frac_samples,
        0.25,
        1.0e-12,
        "summary residual frac",
    );

    // --- 单 position bundle 也能重建 ---
    let rebuilt = reconstruct_bundle(&bundle(session, vec![position]), d0(&spec)).expect("bundle");
    assert_eq!(rebuilt.session_id, SESSION_ID);
    assert_eq!(rebuilt.positions.len(), 1);
}

// ===========================================================================
// 2) τ = 0 / 137 / 411 / 137.25 线性 + 去相位符号
// ===========================================================================

#[test]
fn t49_timebase_fractional_delay_linearity_and_sign() {
    for delay in [0.0_f64, 137.0, 411.0, KNOWN_DELAY] {
        let mut spec = Spec::known();
        spec.delay = delay;
        spec.origin_offset = 3.0; // 非零常数原点（pre-guard 类），不影响 frac 恢复
        let session = make_session(&spec);
        let position = make_position(&spec);
        let timebase = reconstruct_position(&position, &session, d0(&spec)).expect("timebase");
        let lag = spec.origin_offset + delay;
        for slot in &timebase.slots {
            assert_eq!(
                slot.lag_split.coarse_samples,
                lag.floor() as i64,
                "delay {delay} coarse"
            );
            assert_close(
                slot.lag_split.frac_samples,
                lag - lag.floor(),
                1.0e-12,
                "frac",
            );
            assert_close(
                slot.delay_origin_residual_samples,
                0.0,
                1.0e-12,
                "constant origin cancels",
            );
        }
        let fl = position.measurement("FL").expect("FL");
        let absolute = de_phase_analysis(&fl.analysis, d0(&spec), "linearity").expect("de-phase");
        // D0=0：去相位 0 次；measured delay 仍以线性相位漂移留在响应里（residual）。
        assert_close(
            absolute.delay_removed_samples(),
            0.0,
            1.0e-12,
            "D0=0 removes nothing",
        );
        assert_complex_close(
            absolute.points(),
            &expected_after_d0(&fl.analysis, d0(&spec)),
            1.0e-9,
            &format!("delay {delay}"),
        );
    }
}

// ===========================================================================
// 3) 禁止双去相位：absolute 基准不再扣；标签/数据不一致的两种反向夹具
// ===========================================================================

#[test]
fn t49_timebase_absolute_basis_never_double_dephases() {
    // (a) 标签 absolute + 数据不含延迟：模块必须不扣（扣了就是双去相位）。
    //     声明 D0=120 仍不得扣：absolute 基准下 D0 只作 provenance。
    let mut spec = Spec::known();
    spec.basis = BasisKind::Absolute;
    spec.phase_source = PhaseSource::WithoutDelay;
    spec.d0 = 120;
    let session = make_session(&spec);
    let position = make_position(&spec);
    let timebase = reconstruct_position(&position, &session, d0(&spec)).expect("absolute timebase");
    assert_eq!(timebase.common_d0, d0(&spec));
    for slot in &timebase.slots {
        assert_eq!(slot.phase_basis, PhaseBasis::Absolute);
        assert_close(
            slot.analysis_delay_samples,
            KNOWN_DELAY,
            1.0e-12,
            "provenance delay kept",
        );
        assert_close(
            slot.per_participant_residual_delay_samples,
            KNOWN_DELAY - d0(&spec).as_f64(),
            1.0e-12,
            "residual provenance kept",
        );
    }
    let fl = position.measurement("FL").expect("FL");
    let absolute = de_phase_analysis(&fl.analysis, d0(&spec), "absolute").expect("de-phase");
    assert_eq!(absolute.basis(), PhaseBasis::Absolute);
    assert_eq!(absolute.common_d0(), d0(&spec));
    assert_eq!(absolute.delay_removed_samples(), 0.0);
    assert_complex_close(
        absolute.points(),
        &expected_phase0(&fl.analysis),
        1.0e-9,
        "absolute (no D0 subtraction)",
    );

    // (b) 标签 aligned + 数据不含延迟：模块必须恰好扣一次**公共 D0**（结果 = phase0 - 360 f D0/fs），
    //     且不得改扣 measured delay（137.25）。
    let mut spec = Spec::known();
    spec.phase_source = PhaseSource::WithoutDelay;
    spec.d0 = 120;
    let position2 = make_position(&spec);
    let fl2 = position2.measurement("FL").expect("FL");
    let absolute2 =
        de_phase_analysis(&fl2.analysis, d0(&spec), "aligned-no-delay").expect("de-phase");
    assert_close(
        absolute2.delay_removed_samples(),
        d0(&spec).as_f64(),
        1.0e-12,
        "one D0 subtraction",
    );
    let expected: Vec<ComplexPoint> = fl2
        .analysis
        .fr
        .iter()
        .map(|point| {
            ComplexPoint::from_polar(
                point.freq_hz,
                point.db,
                phase0_deg(point.freq_hz) - 360.0 * point.freq_hz * d0(&spec).as_f64() / FS as f64,
            )
            .expect("analytic")
        })
        .collect();
    assert_complex_close(
        absolute2.points(),
        &expected,
        1.0e-9,
        "exactly one D0 subtraction",
    );
    // 反向：若实现忽略标签改扣 measured delay（137.25），结果会等于 phase0；若扣两次 D0 也偏。
    let phase0_only = expected_phase0(&fl2.analysis);
    let wrong_measured = phase0_only[5];
    assert!(
        (absolute2.points()[5].re - wrong_measured.re).abs() > 1.0e-3
            || (absolute2.points()[5].im - wrong_measured.im).abs() > 1.0e-3,
        "aligned basis must subtract D0, not the measured delay"
    );

    // (c) 标签 absolute + 数据含延迟：模块信任标签、不再扣（证明绝不会二次扣 D0/delay）；
    //     若实现忽略标签硬扣，则结果会等于 phase0（D0 扣）或 phase0-360f·137.25/fs（delay 扣），
    //     被下面的正反断言抓住。
    let mut spec = Spec::known();
    spec.basis = BasisKind::Absolute;
    spec.phase_source = PhaseSource::WithDelay;
    spec.d0 = 120;
    let position3 = make_position(&spec);
    let fl3 = position3.measurement("FL").expect("FL");
    let absolute3 =
        de_phase_analysis(&fl3.analysis, d0(&spec), "absolute-aligned-data").expect("de-phase");
    assert_eq!(absolute3.basis(), PhaseBasis::Absolute);
    assert_eq!(absolute3.delay_removed_samples(), 0.0);
    // 正向：absolute = producer 原始相位（phase0 + 360f·137.25/fs）——既不扣 D0 也不扣 delay。
    let raw_expected: Vec<ComplexPoint> = fl3
        .analysis
        .fr
        .iter()
        .map(|point| {
            ComplexPoint::from_polar(
                point.freq_hz,
                point.db,
                phase0_deg(point.freq_hz)
                    + 360.0 * point.freq_hz * fl3.analysis.delay_samples / FS as f64,
            )
            .expect("analytic")
        })
        .collect();
    assert_complex_close(
        absolute3.points(),
        &raw_expected,
        1.0e-9,
        "absolute raw phase",
    );
    let phase0_like: Vec<ComplexPoint> = expected_phase0(&fl3.analysis);
    let first = absolute3.points()[10];
    let wrong = phase0_like[10];
    assert!(
        (first.re - wrong.re).abs() > 1.0e-3 || (first.im - wrong.im).abs() > 1.0e-3,
        "absolute basis must NOT subtract D0/measured delay (double de-phase detected)"
    );
}

// ===========================================================================
// 4) phase_ref 语义：unknown / delay_removed 拒
// ===========================================================================

#[test]
fn t49_timebase_rejects_unknown_phase_ref() {
    let mut spec = Spec::known();
    spec.basis = BasisKind::Unknown;
    let session = make_session(&spec);
    let position = make_position(&spec);
    expect_error(
        reconstruct_position(&position, &session, d0(&spec)),
        TimebaseErrorKind::PhaseReference,
        "E_TB_PHASE_REF",
    );
    expect_error(
        de_phase_analysis(&position.measurements[0].analysis, d0(&spec), "unknown"),
        TimebaseErrorKind::PhaseReference,
        "E_TB_PHASE_REF",
    );
    assert_eq!(TimebaseErrorKind::PhaseReference.code(), "E_TB_PHASE_REF");
}

#[test]
fn t49_timebase_rejects_delay_removed_without_declared_delay() {
    assert_eq!(
        PhaseBasis::parse(BasisKind::DelayRemoved.text()).expect("parse"),
        PhaseBasis::DelayRemoved
    );
    let mut spec = Spec::known();
    spec.basis = BasisKind::DelayRemoved;
    let session = make_session(&spec);
    let position = make_position(&spec);
    expect_error(
        reconstruct_position(&position, &session, d0(&spec)),
        TimebaseErrorKind::PhaseReference,
        "E_TB_PHASE_REF",
    );
    expect_error(
        de_phase_analysis(
            &position.measurements[0].analysis,
            d0(&spec),
            "delay_removed",
        ),
        TimebaseErrorKind::PhaseReference,
        "E_TB_PHASE_REF",
    );
}

// ===========================================================================
// 5) clock：声明/估计冲突
// ===========================================================================

#[test]
fn t49_timebase_rejects_inconsistent_declared_clock_across_slots() {
    let mut spec = Spec::known();
    spec.ppm = 0.0;
    spec.declared_ppm = 50.0;
    spec.declared_sigma = 1.0;
    spec.correction = DriftCorrection::Resample;
    let session = make_session(&spec);
    let mut position = make_position(&spec);
    // 单个 slot 的声明 ppm 与其它 slot 差 150 ppm（各自内部 2σ 自洽）-> 同 position 冲突。
    position.measurements[2].diag.clock.clock_ppm = 200.0;
    expect_error(
        reconstruct_position(&position, &session, d0(&spec)),
        TimebaseErrorKind::ClockConflict,
        "E_TB_CLOCK_CONFLICT",
    );
}

#[test]
fn t49_timebase_rejects_declared_clock_conflict() {
    let mut spec = Spec::known();
    spec.declared_ppm = -3.0;
    spec.declared_sigma = 2.0; // |-3| <= 2*2 -> 不显著，correction=none 合法
    let session = make_session(&spec);
    let position = make_position(&spec);
    expect_error(
        reconstruct_position(&position, &session, d0(&spec)),
        TimebaseErrorKind::ClockConflict,
        "E_TB_CLOCK_CONFLICT",
    );
}

#[test]
fn t49_timebase_rejects_onset_vs_grid_clock_conflict() {
    let spec = Spec::known();
    let session = make_session(&spec);
    let mut position = make_position(&spec);
    // 逐 slot 给 lag 加斜坡：nominal 栅格仍 7.5 ppm，但 actual_start 蕴含 ~11 ppm。
    for slot in 0..5usize {
        set_lag_shift(&mut position, slot, 0.06 * slot as f64);
    }
    expect_error(
        reconstruct_position(&position, &session, d0(&spec)),
        TimebaseErrorKind::ClockConflict,
        "E_TB_CLOCK_CONFLICT",
    );
}

#[test]
fn t49_timebase_rejects_repeat_interval_clock_conflict() {
    let spec = Spec::known();
    let session = make_session(&spec);
    let mut position = make_position(&spec);
    stretch_repeats(&mut position, &[1.0002]);
    expect_error(
        reconstruct_position(&position, &session, d0(&spec)),
        TimebaseErrorKind::ClockConflict,
        "E_TB_CLOCK_CONFLICT",
    );
}

#[test]
fn t49_timebase_rejects_significant_drift_without_correction() {
    let mut spec = Spec::known();
    spec.declared_ppm = 50.0;
    spec.declared_sigma = 1.0;
    spec.correction = DriftCorrection::None; // 显著漂移却 none -> 拒
    let session = make_session(&spec);
    let position = make_position(&spec);
    expect_error(
        reconstruct_position(&position, &session, d0(&spec)),
        TimebaseErrorKind::ClockDrift,
        "E_TB_CLOCK_DRIFT",
    );
}

#[test]
fn t49_timebase_resample_clears_residual_ppm() {
    let mut spec = Spec::known();
    // 声明 phys 漂移 50 ppm 且已校正 -> 记录坐标不应再漂移。
    spec.ppm = 0.0;
    spec.declared_ppm = 50.0;
    spec.declared_sigma = 1.0;
    spec.correction = DriftCorrection::Resample;
    let session = make_session(&spec);
    let position = make_position(&spec);
    let timebase =
        reconstruct_position(&position, &session, d0(&spec)).expect("resampled timebase");
    assert_close(
        timebase.clock.residual_ppm,
        0.0,
        1.0e-6,
        "residual ppm after resample",
    );
    assert_close(
        timebase.clock.declared_ppm,
        50.0,
        1.0e-12,
        "declared ppm kept",
    );
    assert_eq!(timebase.clock.correction, DriftCorrection::Resample);
    assert!(!timebase.clock.residual_drift_significant);
}

#[test]
fn t49_timebase_rejects_residual_ppm_after_resample() {
    let mut spec = Spec::known();
    spec.ppm = 30.0; // 坐标里仍残留 30 ppm，但声明已 resample -> 拒
    spec.declared_ppm = 50.0;
    spec.declared_sigma = 1.0;
    spec.correction = DriftCorrection::Resample;
    let session = make_session(&spec);
    let position = make_position(&spec);
    expect_error(
        reconstruct_position(&position, &session, d0(&spec)),
        TimebaseErrorKind::ClockDrift,
        "E_TB_CLOCK_DRIFT",
    );
}

#[test]
fn t49_timebase_declared_only_clock_warns() {
    let mut spec = Spec::known();
    spec.repeats = 1; // 单遍 -> 无 repeat 估计
    let session = make_session(&spec);
    let mut position = make_position(&spec);
    position.measurements.truncate(1); // 单 slot -> 无 LSQ
    let timebase =
        reconstruct_position(&position, &session, d0(&spec)).expect("declared-only clock");
    assert_eq!(timebase.clock.method, ClockMethod::DeclaredOnly);
    assert!(!timebase.clock.independent());
    assert_close(
        timebase.clock.residual_ppm,
        KNOWN_PPM,
        1.0e-12,
        "declared residual",
    );
    assert!(
        timebase
            .warnings
            .iter()
            .any(|warning| warning.code == TimebaseWarning::DECLARED_ONLY_CLOCK),
        "expected W_TB_DECLARED_ONLY_CLOCK: {:?}",
        timebase.warnings
    );
}

// ===========================================================================
// 6) 延迟原点一致性 / 极性 / 无效几何
// ===========================================================================

#[test]
fn t49_timebase_rejects_delay_origin_inconsistency() {
    let spec = Spec::known();
    let session = make_session(&spec);
    let mut position = make_position(&spec);
    // 只改一个 slot 的 analysis delay（三字段同步改，先通过字段自洽复校）：
    // diag lag 与 analysis delay 的原点不再常数。
    position.measurements[3].analysis.delay_samples += 3.0;
    position.measurements[3].analysis.delay_coarse_samples += 3;
    position.measurements[3].analysis.delay_ms += 3.0 / FS as f64 * 1000.0;
    expect_error(
        reconstruct_position(&position, &session, d0(&spec)),
        TimebaseErrorKind::DelayOrigin,
        "E_TB_DELAY_ORIGIN",
    );
    assert!(DELAY_ORIGIN_SPREAD_MAX_SAMPLES > 0.0);
}

#[test]
fn t49_timebase_rejects_unknown_polarity() {
    let mut spec = Spec::known();
    spec.polarity = 0;
    let session = make_session(&spec);
    let position = make_position(&spec);
    expect_error(
        reconstruct_position(&position, &session, d0(&spec)),
        TimebaseErrorKind::PolarityUnknown,
        "E_TB_POLARITY_UNKNOWN",
    );
    // 相位重建本身不依赖极性字段：de_phase_analysis 仍可工作。
    let absolute = de_phase_analysis(&position.measurements[0].analysis, d0(&spec), "no-polarity")
        .expect("phase does not need polarity");
    assert!(absolute.polarity().is_none());
}

#[test]
fn t49_timebase_rejects_invalid_session_shape() {
    let spec = Spec::known();
    let session = make_session(&spec);
    let position = make_position(&spec);
    let mut bad_rate = session.clone();
    bad_rate.sample_rate_hz = 44_100;
    expect_error(
        reconstruct_position(&position, &bad_rate, d0(&spec)),
        TimebaseErrorKind::Unsupported,
        "E_TB_UNSUPPORTED",
    );
    let mut bad_capture = position.clone();
    bad_capture.capture.channels = 2;
    expect_error(
        reconstruct_position(&bad_capture, &session, d0(&spec)),
        TimebaseErrorKind::Unsupported,
        "E_TB_UNSUPPORTED",
    );
}

#[test]
fn t49_timebase_delay_split_semantics() {
    let split = DelaySplit::new(KNOWN_DELAY).expect("split");
    assert_eq!(split.coarse_samples, 137);
    assert_close(split.frac_samples, 0.25, 1.0e-12, "frac");
    let negative = DelaySplit::new(-0.25).expect("negative split");
    assert_eq!(negative.coarse_samples, -1);
    assert_close(negative.frac_samples, 0.75, 1.0e-12, "negative frac");
    assert_close(negative.centered_fraction(), -0.25, 1.0e-12, "centered");
}

// ===========================================================================
// 7) MOTU 电学 reference 独立通道 + 公共相位
// ===========================================================================

#[test]
fn t49_timebase_reference_channel_and_relative_phase() {
    let mut spec = Spec::known();
    spec.reference = true;
    spec.d0 = 120; // 非零公共 D0：reference 与 participant 都只扣这一个值。
    let session = make_session(&spec);
    let position = make_position(&spec);
    let timebase = reconstruct_position(&position, &session, d0(&spec)).expect("timebase");
    let reference = reference_analysis(&spec);

    let channel =
        de_phase_reference(&session, &position, d0(&spec), &reference).expect("reference channel");
    assert_eq!(channel.device, "hw:motu,0");
    assert_eq!(channel.channels, 2);
    assert_eq!(channel.sample_format, SampleFormat::S32LE);
    assert!(channel.independent);
    assert_eq!(channel.common_d0, d0(&spec));
    assert_close(channel.delay_samples, 50.0, 1.0e-12, "reference delay");
    assert_close(
        channel.residual_delay_samples,
        50.0 - d0(&spec).as_f64(),
        1.0e-12,
        "reference residual delay",
    );
    assert_eq!(channel.polarity.as_i8(), 1);
    assert_eq!(channel.response.basis(), PhaseBasis::AlignedSegment);
    assert_close(
        channel.response.delay_removed_samples(),
        d0(&spec).as_f64(),
        1.0e-12,
        "ref removed = declared D0 (not measured 50)",
    );

    let common =
        reconstruct_common_phase(&timebase, &position, &session, Some(&reference)).expect("common");
    assert_eq!(common.position, "p01-mlp");
    assert_eq!(common.basis, PhaseBasis::AlignedSegment);
    assert_eq!(common.common_d0, d0(&spec));
    assert!(common.reference.is_some());
    assert_eq!(common.relative.len(), 5);
    assert_eq!(common.slots.len(), 5);

    // 相对响应 = participant_abs / reference_abs（只除一次）：D0 在两侧都只扣一次、相消，
    // 相对相位只由实测传播延迟差决定（D0 不应出现在相对响应里）。
    let fl = common.slot("FL").expect("FL slot");
    let fl_meas = position.measurement("FL").expect("FL measurement");
    assert_close(
        fl.absolute.delay_removed_samples(),
        d0(&spec).as_f64(),
        1.0e-12,
        "participant removed D0",
    );
    let relative = &common.relative[0];
    assert_eq!(relative.label(), "p01-mlp/FL");
    assert_eq!(relative.reference_label(), "p01-mlp/reference");
    let delay_difference = fl_meas.analysis.delay_samples - reference.delay_samples;
    for (index, point) in relative.points().iter().enumerate() {
        let db_part = fl_meas.analysis.fr[index].db;
        let db_ref = reference.fr[index].db;
        let mag_part = 10.0f64.powf(db_part / 20.0);
        let mag_ref = 10.0f64.powf(db_ref / 20.0);
        let phase = phase0_deg(point.freq_hz) - reference_phase0_deg(point.freq_hz)
            + 360.0 * point.freq_hz * delay_difference / FS as f64;
        let expected =
            ComplexPoint::from_polar(point.freq_hz, 20.0 * (mag_part / mag_ref).log10(), phase)
                .expect("expected relative");
        assert_close(point.re, expected.re, 1.0e-9, "relative re");
        assert_close(point.im, expected.im, 1.0e-9, "relative im");
    }
    assert!(fl.absolute.polarity().is_none());
    assert!(common
        .warnings
        .iter()
        .all(|warning| warning.code != "W_TB_ABSOLUTE_PHASE_ONLY"));

    // T6 FC-04 关键性质：相对相位与公共 D0 无关（D0=0 vs 120 的相对响应逐点一致）。
    let mut spec_zero = spec;
    spec_zero.d0 = 0;
    let timebase_zero =
        reconstruct_position(&position, &session, d0(&spec_zero)).expect("D0=0 timebase");
    let common_zero =
        reconstruct_common_phase(&timebase_zero, &position, &session, Some(&reference))
            .expect("D0=0 common");
    for (a, b) in common_zero.relative[0]
        .points()
        .iter()
        .zip(relative.points().iter())
    {
        assert_close(a.re, b.re, 1.0e-12, "relative D0-invariant re");
        assert_close(a.im, b.im, 1.0e-12, "relative D0-invariant im");
    }
}

#[test]
fn t49_timebase_reference_rejections() {
    let mut spec = Spec::known();
    spec.reference = true;
    let session = make_session(&spec);
    let position = make_position(&spec);
    let reference = reference_analysis(&spec);

    // reference 声明但 position 缺 reference.raw
    let mut no_raw = position.clone();
    no_raw.reference = None;
    expect_error(
        de_phase_reference(&session, &no_raw, d0(&spec), &reference),
        TimebaseErrorKind::Reference,
        "E_TB_REFERENCE",
    );

    // reference.raw 通道/格式与声明不符
    let mut bad_raw = position.clone();
    bad_raw.reference.as_mut().expect("reference").channels = 1;
    expect_error(
        de_phase_reference(&session, &bad_raw, d0(&spec), &reference),
        TimebaseErrorKind::Reference,
        "E_TB_REFERENCE",
    );

    // 未声明 reference 却给 reference analysis
    let mut no_reference = session.clone();
    no_reference.reference = None;
    expect_error(
        de_phase_reference(&no_reference, &position, d0(&spec), &reference),
        TimebaseErrorKind::Reference,
        "E_TB_REFERENCE",
    );

    // device 与 UMIK 相同 -> reject
    let mut same_device = session.clone();
    same_device.reference.as_mut().expect("declared").device = same_device.capture.device.clone();
    expect_error(
        reconstruct_position(&position, &same_device, d0(&spec)),
        TimebaseErrorKind::Reference,
        "E_TB_REFERENCE",
    );

    // independent=false -> reject
    let mut not_independent = session.clone();
    not_independent
        .reference
        .as_mut()
        .expect("declared")
        .independent = false;
    expect_error(
        reconstruct_position(&position, &not_independent, d0(&spec)),
        TimebaseErrorKind::Reference,
        "E_TB_REFERENCE",
    );

    // session 声明 reference，但公共相位重建未提供 reference analysis -> reject
    let timebase = reconstruct_position(&position, &session, d0(&spec)).expect("timebase");
    expect_error(
        reconstruct_common_phase(&timebase, &position, &session, None),
        TimebaseErrorKind::Reference,
        "E_TB_REFERENCE",
    );
}

// ===========================================================================
// 7b) 公共整数 D0（T6 FC-04）：137.25 + 非零 D0 双扣夹具 / 声明界 / 延迟字段复校
// ===========================================================================

#[test]
fn t49_timebase_common_d0_nonzero_137_25_no_double_subtraction() {
    // 同一输入（measured delay = 137.25）在 D0=0 与 D0=120 下：只扣 D0 一次，
    // residual = measured − D0；绝不把 measured delay 再扣一次（双扣）。
    let spec = Spec::known();
    let session = make_session(&spec);
    let position = make_position(&spec);
    let fl = position.measurement("FL").expect("FL measurement");
    let d0_zero = CommonD0::zero();
    let d0_120 = CommonD0::new(120).expect("D0=120");

    let tb_zero = reconstruct_position(&position, &session, d0_zero).expect("D0=0 timebase");
    let tb_120 = reconstruct_position(&position, &session, d0_120).expect("D0=120 timebase");
    assert_eq!(tb_zero.common_d0.samples(), 0);
    assert_eq!(tb_120.common_d0.samples(), 120);
    assert_eq!(tb_zero.summary().common_d0_samples, 0);
    assert_eq!(tb_120.summary().common_d0_samples, 120);
    for slot in &tb_zero.slots {
        assert_close(
            slot.per_participant_residual_delay_samples,
            KNOWN_DELAY,
            1.0e-12,
            "D0=0 residual = measured",
        );
        assert_eq!(slot.per_participant_residual_split.coarse_samples, 137);
    }
    for slot in &tb_120.slots {
        assert_close(
            slot.per_participant_residual_delay_samples,
            KNOWN_DELAY - 120.0,
            1.0e-12,
            "D0=120 residual = measured - D0",
        );
        assert_eq!(slot.per_participant_residual_split.coarse_samples, 17);
        assert_close(
            slot.per_participant_residual_split.frac_samples,
            0.25,
            1.0e-12,
            "D0=120 residual frac",
        );
    }

    let abs_zero = de_phase_analysis(&fl.analysis, d0_zero, "P01/FL@D0=0").expect("abs D0=0");
    let abs_120 = de_phase_analysis(&fl.analysis, d0_120, "P01/FL@D0=120").expect("abs D0=120");
    assert_close(
        abs_zero.delay_removed_samples(),
        0.0,
        1.0e-12,
        "D0=0 removes nothing",
    );
    assert_close(
        abs_120.delay_removed_samples(),
        120.0,
        1.0e-12,
        "D0=120 removes exactly D0",
    );
    assert_complex_close(
        abs_zero.points(),
        &expected_after_d0(&fl.analysis, d0_zero),
        1.0e-9,
        "D0=0 response",
    );
    assert_complex_close(
        abs_120.points(),
        &expected_after_d0(&fl.analysis, d0_120),
        1.0e-9,
        "D0=120 response",
    );

    // 负对照 1：若在 D0 之外又扣 measured delay（= phase0），偏离。
    let phase0_like = expected_phase0(&fl.analysis);
    // 负对照 2：若 D0 被扣两次（measured − 2·D0），也偏离。
    let double_d0 = expected_after_d0(&fl.analysis, CommonD0::new(240).expect("D0=240"));
    for (index, point) in abs_120.points().iter().enumerate() {
        let wrong_phase0 = phase0_like[index];
        assert!(
            (point.re - wrong_phase0.re).abs() > 1.0e-3
                || (point.im - wrong_phase0.im).abs() > 1.0e-3,
            "must NOT subtract the T48 measured delay on top of D0 (double de-phase)"
        );
        let wrong_double = double_d0[index];
        assert!(
            (point.re - wrong_double.re).abs() > 1.0e-3
                || (point.im - wrong_double.im).abs() > 1.0e-3,
            "must NOT subtract D0 twice"
        );
    }

    // 独立线性相位证据：abs(D0=0) 与 abs(D0=120) 的逐点相位差 = 360·f·120/fs（取主值）。
    for (zero, minus120) in abs_zero.points().iter().zip(abs_120.points().iter()) {
        let diff = wrap_deg(minus120.phase_deg() - zero.phase_deg());
        let expected = wrap_deg(-360.0 * zero.freq_hz * 120.0 / FS as f64);
        assert!(
            (diff - expected).abs() < 1.0e-6,
            "D0 subtraction must be exactly 120 samples at {} Hz: diff {diff} vs {expected}",
            zero.freq_hz
        );
    }
}

fn wrap_deg(deg: f64) -> f64 {
    (deg + 180.0).rem_euclid(360.0) - 180.0
}

#[test]
fn t49_timebase_common_d0_declaration_bounds() {
    assert_eq!(CommonD0::zero().samples(), 0);
    assert_eq!(CommonD0::default().samples(), 0);
    assert_eq!(CommonD0::new(120).expect("ok").as_f64(), 120.0);
    assert_eq!(CommonD0::new(-120).expect("negative ok").samples(), -120);
    assert_eq!(
        CommonD0::new(COMMON_D0_SANITY_SAMPLES)
            .expect("edge ok")
            .samples(),
        COMMON_D0_SANITY_SAMPLES
    );
    assert_eq!(
        CommonD0::new(-COMMON_D0_SANITY_SAMPLES)
            .expect("negative edge ok")
            .samples(),
        -COMMON_D0_SANITY_SAMPLES
    );
    expect_error(
        CommonD0::new(COMMON_D0_SANITY_SAMPLES + 1),
        TimebaseErrorKind::CommonDelay,
        "E_TB_COMMON_D",
    );
    expect_error(
        CommonD0::new(-COMMON_D0_SANITY_SAMPLES - 1),
        TimebaseErrorKind::CommonDelay,
        "E_TB_COMMON_D",
    );
    assert!(CommonD0::new(i64::MIN).is_err(), "i64::MIN must not panic");
    assert_close(
        CommonD0::new(120).expect("ok").residual_of(137.25),
        17.25,
        1.0e-12,
        "residual_of",
    );
    assert_eq!(
        format!("{}", CommonD0::new(120).expect("ok")),
        "D0=120 samples@fs"
    );
}

#[test]
fn t49_timebase_rejects_inconsistent_delay_fields() {
    let spec = Spec::known();
    let session = make_session(&spec);

    // delay_coarse_samples != floor(delay_samples)：reconstruct 与 de_phase 均复校。
    let mut coarse = make_position(&spec);
    coarse.measurements[0].analysis.delay_coarse_samples += 1;
    expect_error(
        reconstruct_position(&coarse, &session, d0(&spec)),
        TimebaseErrorKind::Range,
        "E_TB_RANGE",
    );
    expect_error(
        de_phase_analysis(&coarse.measurements[0].analysis, d0(&spec), "bad-coarse"),
        TimebaseErrorKind::Range,
        "E_TB_RANGE",
    );

    // delay_ms 与 delay_samples/fs*1000 不一致。
    let mut ms = make_position(&spec);
    ms.measurements[0].analysis.delay_ms += 0.5;
    expect_error(
        reconstruct_position(&ms, &session, d0(&spec)),
        TimebaseErrorKind::Range,
        "E_TB_RANGE",
    );

    // 按错误采样率换算的 delay_ms。
    let mut wrong_rate = make_position(&spec);
    wrong_rate.measurements[0].analysis.delay_ms =
        wrong_rate.measurements[0].analysis.delay_samples / 44_100.0 * 1000.0;
    expect_error(
        reconstruct_position(&wrong_rate, &session, d0(&spec)),
        TimebaseErrorKind::Range,
        "E_TB_RANGE",
    );

    // 6 位小数 ms 量化仍在容差内（不误杀 producer 记录）。
    let mut rounded = make_position(&spec);
    let exact = rounded.measurements[0].analysis.delay_samples / FS as f64 * 1000.0;
    rounded.measurements[0].analysis.delay_ms = (exact * 1.0e6).round() / 1.0e6;
    assert!(
        reconstruct_position(&rounded, &session, d0(&spec)).is_ok(),
        "6-decimal ms rounding must stay accepted"
    );
    assert!(DELAY_MS_ABSOLUTE_TOLERANCE_MS > 0.0);

    // analysis 的 fs 与 session 不一致（T49 不允许跨 rate）。
    let mut fs_mismatch = make_position(&spec);
    fs_mismatch.measurements[0].analysis.meta.sample_rate_hz = 96_000;
    expect_error(
        reconstruct_position(&fs_mismatch, &session, d0(&spec)),
        TimebaseErrorKind::Unsupported,
        "E_TB_UNSUPPORTED",
    );
}

// ===========================================================================
// 8) slot 诊断（overlap / spacing / repeat spacing）与非致命 warning
// ===========================================================================

#[test]
fn t49_timebase_slot_diagnostics_warn_without_failing() {
    // slot 间隔 = 单遍间隔 -> 相邻 slot 的 repeat 窗重叠且 slot 间隔 != 顺序预期。
    let mut spec = Spec::known();
    spec.slot_interval_frames = Some((SWEEP_SECONDS + GAP_SECONDS) * FS as f64);
    let session = make_session(&spec);
    let position = make_position(&spec);
    let timebase = reconstruct_position(&position, &session, d0(&spec)).expect("diagnostics only");
    assert!(timebase
        .warnings
        .iter()
        .any(|warning| warning.code == TimebaseWarning::SLOT_OVERLAP));
    assert!(timebase
        .warnings
        .iter()
        .any(|warning| warning.code == TimebaseWarning::SLOT_SPACING));

    // repeat 间隔交替 ±0.4%：repeat 估计不确定度大（不触发 ppm 冲突），但残差 > 2 samples
    // -> REPEAT_SPACING 非致命告警。
    let spec2 = Spec::known();
    let session2 = make_session(&spec2);
    let mut position2 = make_position(&spec2);
    stretch_repeats(&mut position2, &[1.004, 0.996, 1.0, 1.004, 0.996]);
    let timebase2 = reconstruct_position(&position2, &session2, d0(&spec2)).expect("warn only");
    assert!(timebase2
        .warnings
        .iter()
        .any(|warning| warning.code == TimebaseWarning::REPEAT_SPACING));
}

// ===========================================================================
// 9) 跨 position 公共基准 / participants 一致性
// ===========================================================================

#[test]
fn t49_timebase_bundle_common_basis_and_participants() {
    let spec_a = Spec::known();
    let mut spec_b = Spec::known();
    spec_b.ppm = -3.0;
    spec_b.declared_ppm = -3.0;
    spec_b.capture0 = 960.0;
    let session = make_session(&spec_a);
    let p01 = position_with("p01-mlp", PositionRole::Design, 1.0, &spec_a);
    let mut p02 = position_with("p02-sofa", PositionRole::Validation, 0.6, &spec_b);
    // p02 的路径/目录名与 p01 不同，但时间基独立；公共 epoch/graph/session 必须一致。
    for measurement in &mut p02.measurements {
        measurement.speaker = measurement.speaker.clone();
    }
    let bundle = bundle(session.clone(), vec![p01.clone(), p02.clone()]);
    let timebase = reconstruct_bundle(&bundle, d0(&spec_a)).expect("bundle");
    assert_eq!(timebase.positions.len(), 2);
    assert_eq!(timebase.design_positions().count(), 1);
    assert_eq!(timebase.validation_positions().count(), 1);
    let design = timebase.position("p01-mlp").expect("p01");
    let validation = timebase.position("p02-sofa").expect("p02");
    assert_close(design.clock.residual_ppm, KNOWN_PPM, 1.0e-6, "design ppm");
    assert_close(
        validation.clock.residual_ppm,
        -3.0,
        1.0e-6,
        "validation ppm",
    );
    assert_eq!(design.epoch_id, validation.epoch_id);
    assert_eq!(design.graph_id, validation.graph_id);

    let summary: TimebaseSummary = timebase.summary();
    assert_eq!(summary.schema, TIMEBASE_SCHEMA);
    assert_eq!(summary.positions.len(), 2);
    assert_eq!(summary.session_id, SESSION_ID);
    assert_eq!(summary.common_d0_samples, d0(&spec_a).samples());
    for position in &summary.positions {
        assert_eq!(position.common_d0_samples, d0(&spec_a).samples());
        for slot in &position.slots {
            assert_close(
                slot.per_participant_residual_delay_samples,
                KNOWN_DELAY - d0(&spec_a).as_f64(),
                1.0e-12,
                "bundle residual delay",
            );
            assert_eq!(slot.per_participant_residual_coarse_samples, 137);
            assert_close(
                slot.per_participant_residual_frac_samples,
                0.25,
                1.0e-12,
                "bundle residual frac",
            );
        }
    }

    // graph_id 跨 position 不一致 -> slot order 错误（公共 render frame 基准破裂）。
    let mut broken = bundle.clone();
    broken.positions[1].measurements[0].diag.graph_id = "ff".repeat(32);
    expect_error(
        reconstruct_bundle(&broken, d0(&spec_a)),
        TimebaseErrorKind::SlotOrder,
        "E_TB_SLOT_ORDER",
    );

    // participants 跨 position 不一致 -> slot order 错误。
    let mut broken2 = bundle.clone();
    broken2.positions[1].measurements.truncate(4);
    expect_error(
        reconstruct_bundle(&broken2, d0(&spec_a)),
        TimebaseErrorKind::SlotOrder,
        "E_TB_SLOT_ORDER",
    );
}

#[test]
fn t49_timebase_rejects_slot_order_violations() {
    let spec = Spec::known();
    let session = make_session(&spec);

    // slot 覆盖错（重复 slot 1）
    let mut duplicated = make_position(&spec);
    duplicated.measurements[2].slot = 1;
    expect_error(
        reconstruct_position(&duplicated, &session, d0(&spec)),
        TimebaseErrorKind::SlotOrder,
        "E_TB_SLOT_ORDER",
    );

    // render frame 非严格递增
    let mut reordered = make_position(&spec);
    reordered.measurements[3].diag.render_frame_index =
        reordered.measurements[2].diag.render_frame_index;
    expect_error(
        reconstruct_position(&reordered, &session, d0(&spec)),
        TimebaseErrorKind::SlotOrder,
        "E_TB_SLOT_ORDER",
    );

    // out_index 与 session.speakers 声明不一致
    let mut bad_index = make_position(&spec);
    bad_index.measurements[0].out_index = 9;
    bad_index.measurements[0].diag.channel = 9;
    bad_index.measurements[0].analysis.channel = 9;
    expect_error(
        reconstruct_position(&bad_index, &session, d0(&spec)),
        TimebaseErrorKind::SlotOrder,
        "E_TB_SLOT_ORDER",
    );
}

// ===========================================================================
// 10) 已知夹具 JSON 证据（供独立 Python checker 从日志复算；不参与生产逻辑）
// ===========================================================================

#[test]
fn t49_timebase_known_fixture_json_evidence() {
    let spec = Spec::known();
    let session = make_session(&spec);
    let position = make_position(&spec);
    let fl = position.measurement("FL").expect("FL");
    let variants = [CommonD0::zero(), CommonD0::new(120).expect("D0=120")];
    let mut json = String::new();
    // 夹具声明（DoD 已知真值）+ 独立复算所需的 producer 输入点（raw phase/db），
    // 供 evidence/T49/check_known_fixture.py 只从本 JSON 复算"只扣 D0 一次"，不读 Rust 源码。
    json.push_str(
        r#"{"fixture":{"delay_samples":137.25,"clock_ppm":7.5,"polarity":-1,"sample_rate_hz":48000},"raw_fr":["#,
    );
    for (index, point) in fl.analysis.fr.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            r#"{{"freq_hz":{:.17e},"db":{:.17e},"phase_deg":{:.17e}}}"#,
            point.freq_hz, point.db, point.phase_deg
        ));
    }
    json.push_str(r#"],"variants":["#);
    let mut removed_all: Vec<f64> = Vec::new();
    let mut residual_all: Vec<f64> = Vec::new();
    for (variant_index, declared) in variants.iter().enumerate() {
        let timebase = reconstruct_position(&position, &session, *declared).expect("timebase");
        let absolute = de_phase_analysis(&fl.analysis, *declared, "P01/FL").expect("de-phase");
        let slot = timebase.slots.first().expect("slot");
        if variant_index > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            r#"{{"d0_samples":{},"reconstructed":{{"common_d0_samples":{},"coarse_samples":{},"frac_samples":{},"residual_ppm":{},"declared_ppm":{},"samples_per_frame":{},"delay_removed_samples":{},"basis":"{}","polarity_field":{},"slot_interval_frames_median":{},"expected_slot_interval_frames":{},"delay_origin_offset_samples":{},"delay_origin_spread_samples":{},"per_participant_residual_delay_samples":{},"per_participant_residual_coarse_samples":{},"per_participant_residual_frac_samples":{},"fr_points":["#,
            declared.samples(),
            timebase.common_d0.samples(),
            slot.lag_split.coarse_samples,
            slot.lag_split.frac_samples,
            timebase.clock.residual_ppm,
            timebase.clock.declared_ppm,
            timebase.clock.samples_per_frame,
            absolute.delay_removed_samples(),
            absolute.basis().as_str(),
            slot.polarity.as_i8(),
            timebase.slot_interval_frames_median,
            timebase.expected_slot_interval_frames,
            timebase.delay_origin_offset_samples,
            timebase.delay_origin_spread_samples,
            slot.per_participant_residual_delay_samples,
            slot.per_participant_residual_split.coarse_samples,
            slot.per_participant_residual_split.frac_samples,
        ));
        for (index, point) in absolute.points().iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            json.push_str(&format!(
                r#"{{"freq_hz":{:.17e},"re":{:.17e},"im":{:.17e}}}"#,
                point.freq_hz, point.re, point.im
            ));
        }
        json.push_str("]}}");
        removed_all.push(absolute.delay_removed_samples());
        residual_all.push(slot.per_participant_residual_delay_samples);
    }
    json.push(']');
    json.push('}');
    // 人工复核用：只有 --nocapture 时才出现在日志里；本行不参与生产/报告。
    println!("T49_EVIDENCE_JSON {json}");
    assert_eq!(removed_all, vec![0.0, 120.0], "D0 removal per variant");
    assert_eq!(residual_all, vec![137.25, 17.25], "residual per variant");
}

#[test]
fn t49_timebase_static_guards_no_io_and_single_dephase_site() {
    let source = khaen_modal_design::timebase::timebase_source_text_for_guards();
    for forbidden in [
        "std::fs",
        "File::open",
        "File::create",
        "OpenOptions",
        "std::process",
        "Command::new",
        "std::net",
        "std::thread",
        "std::env",
        "std::io",
        "serde",
        "unsafe {",
    ] {
        assert!(
            !source.contains(forbidden),
            "timebase.rs must not contain {forbidden:?} (pure non-RT computation, no I/O)"
        );
    }
    assert!(source.contains("#![forbid(unsafe_code)]"));
    // 去相位旋转必须只有一个实现点（双去相位结构性防线）。
    assert_eq!(
        source.matches("point.re * cos - point.im * sin").count(),
        1,
        "exactly one de-phase rotation site expected"
    );
    // 该唯一旋转点必须只扣公共整数 D0，不得再扣 T48 measured delay（FC-04）。
    assert_eq!(
        source
            .matches("point.freq_hz * d0_samples / sample_rate")
            .count(),
        1,
        "de-phase rotation must use the common D0 only"
    );
    assert!(
        !source.contains("point.freq_hz * delay_samples / sample_rate"),
        "de-phase must NOT use the T48 measured delay"
    );
    assert!(
        !source.contains("point.freq_hz * analysis_delay_samples / sample_rate"),
        "de-phase must NOT use analysis_delay_samples"
    );
    assert!(source.contains("common_d0.as_f64()"));
    assert!(source.contains("common_d0.residual_of(delay)"));
    assert!(source.contains("pub struct CommonD0"));
    assert!(source.contains("pub fn de_phase_analysis"));
    assert!(source.contains("pub fn reconstruct_bundle"));
}

// ===========================================================================
// 12) file-backed T48 ingest 端到端（JSON + SHA256SUMS + read_bundle）
// ===========================================================================

fn json_session(session: &IngestSession) -> String {
    let participants = session
        .participants
        .iter()
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(",");
    let speakers = session
        .speakers
        .iter()
        .map(|speaker| {
            format!(
                "{{\"name\":\"{}\",\"out_index\":{},\"kind\":\"{}\"}}",
                speaker.name, speaker.out_index, speaker.kind
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let reference = match &session.reference {
        Some(reference) => format!(
            ",\"reference\":{{\"device\":\"{}\",\"channels\":{},\"sample_format\":\"{}\",\"independent\":{}}}",
            reference.device,
            reference.channels,
            reference.sample_format.as_str(),
            reference.independent
        ),
        None => String::new(),
    };
    format!(
        "{{\"schema\":\"tspi-mimo-measurement/v1\",\"session_id\":\"{}\",\"layout\":\"{}\",\"sample_rate_hz\":{},\"block_frames\":128,\"participants\":[{}],\"speakers\":[{}],\"mic\":{{\"model\":\"{}\",\"serial\":\"{}\",\"cal_sha256\":\"{}\",\"orientation\":\"{}\",\"clock_domain\":\"{}\"}},\"playback\":{{\"device\":\"{}\",\"channels\":{},\"sample_format\":\"{}\"}},\"capture\":{{\"device\":\"{}\",\"channels\":{},\"sample_format\":\"{}\",\"pre_seconds\":{},\"post_seconds\":{}}},\"stimulus\":{{\"kind\":\"{}\",\"start_hz\":{},\"end_hz\":{},\"seconds\":{},\"level_dbfs\":{},\"fade_ms\":{},\"repeats\":{},\"repeat_gap_seconds\":{}}},\"room_temperature_c\":{},\"tool\":{{\"name\":\"{}\",\"version\":\"{}\"}},\"created_unix\":{}{}}}",
        session.session_id,
        session.layout,
        session.sample_rate_hz,
        participants,
        speakers,
        session.mic.model,
        session.mic.serial,
        session.mic.cal_sha256,
        session.mic.orientation,
        session.mic.clock_domain,
        session.playback.device,
        session.playback.channels,
        session.playback.sample_format.as_str(),
        session.capture.device,
        session.capture.channels,
        session.capture.sample_format.as_str(),
        session.capture.pre_seconds,
        session.capture.post_seconds,
        session.stimulus.kind,
        session.stimulus.start_hz,
        session.stimulus.end_hz,
        session.stimulus.seconds,
        session.stimulus.level_dbfs,
        session.stimulus.fade_ms,
        session.stimulus.repeats,
        session.stimulus.repeat_gap_seconds,
        session.room_temperature_c,
        session.tool.name,
        session.tool.version,
        session.created_unix.unwrap_or(0),
        reference
    )
}

fn json_position(position: &IngestPosition) -> String {
    format!(
        "{{\"schema\":\"tspi-mimo-position/v1\",\"name\":\"{}\",\"role\":\"{}\",\"weight\":{},\"mic_orientation\":\"90deg\"}}",
        position.meta.name,
        position.meta.role.as_str(),
        position.meta.weight
    )
}

fn json_diag(diag: &TimebaseDiag) -> String {
    let repeat_starts = diag
        .repeat_starts
        .iter()
        .map(|value| format!("{value}"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"schema\":\"tspi-mimo-diag/v1\",\"session_id\":\"{}\",\"epoch_id\":{},\"graph_id\":\"{}\",\"speaker\":\"{}\",\"channel\":{},\"slot\":{},\"capture_file\":\"{}\",\"render_frame_index\":{},\"capture_sample_index\":{},\"nominal_start\":{},\"actual_start\":{},\"lag_samples\":{},\"repeat_starts\":[{}],\"repeats\":{},\"clock\":{{\"clock_ppm\":{},\"clock_ppm_uncertainty_ppm\":{},\"drift_significant\":{},\"clock_ppm_ok\":{},\"correction\":\"{}\"}},\"onset_invalid\":{}}}",
        diag.session_id,
        diag.epoch_id,
        diag.graph_id,
        diag.speaker,
        diag.channel,
        diag.slot,
        diag.capture_file,
        diag.render_frame_index,
        diag.capture_sample_index,
        diag.nominal_start,
        diag.actual_start,
        diag.lag_samples,
        repeat_starts,
        diag.repeats,
        diag.clock.clock_ppm,
        diag.clock.clock_ppm_uncertainty_ppm,
        diag.clock.drift_significant,
        diag.clock.clock_ppm_ok,
        diag.clock.correction.as_str(),
        diag.onset_invalid
    )
}

fn json_analysis(analysis: &SourceAnalysis) -> String {
    let fr = analysis
        .fr
        .iter()
        .map(|point| {
            format!(
                "{{\"freq_hz\":{},\"db\":{},\"phase_deg\":{}}}",
                point.freq_hz, point.db, point.phase_deg
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"schema\":\"tspi-channel-measurement/v1\",\"channel\":{},\"delay_samples\":{},\"delay_ms\":{},\"delay_coarse_samples\":{},\"polarity\":{},\"peak_db\":{},\"phase_1k_deg\":{},\"snr_db\":{},\"phase_ref\":\"{}\",\"fr\":[{}],\"meta\":{{\"schema\":\"khaen-calibration-meta/v1\",\"sample_rate\":{},\"channels\":{},\"sweep_start_hz\":{},\"sweep_end_hz\":{},\"sweep_seconds\":{},\"level_db\":{},\"pre_seconds\":{},\"post_seconds\":{},\"playback_device\":\"{}\",\"playback_channels\":{},\"capture_device\":\"{}\",\"capture_channels\":{},\"created_unix\":{}}}}}",
        analysis.channel,
        analysis.delay_samples,
        analysis.delay_ms,
        analysis.delay_coarse_samples,
        analysis.polarity,
        analysis.peak_db,
        analysis.phase_1k_deg,
        analysis.snr_db,
        analysis.phase_ref,
        fr,
        analysis.meta.sample_rate_hz,
        analysis.meta.playback_channels,
        analysis.meta.sweep_start_hz,
        analysis.meta.sweep_end_hz,
        analysis.meta.sweep_seconds,
        analysis.meta.level_dbfs,
        analysis.meta.pre_seconds,
        analysis.meta.post_seconds,
        analysis.meta.playback_device,
        analysis.meta.playback_channels,
        analysis.meta.capture_device,
        analysis.meta.capture_channels,
        analysis.meta.created_unix
    )
}

fn write_text_file(root: &Path, relative: &str, content: &str) -> (String, String) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dir");
    }
    fs::write(&path, content.as_bytes()).expect("write fixture file");
    (relative.to_string(), sha256_hex(content.as_bytes()))
}

fn materialize_bundle(
    session: &IngestSession,
    positions: &[IngestPosition],
) -> (PathBuf, Vec<(String, String)>) {
    let base = std::env::current_exe()
        .ok()
        .and_then(|exe| {
            exe.parent()
                .map(|parent| parent.join("t49-timebase-fixtures"))
        })
        .unwrap_or_else(|| std::env::temp_dir().join("t49-timebase-fixtures"));
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let root = base.join(format!("t49-e2e-{}-{unique}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("create fixture root");
    let mut files: Vec<(String, String)> = Vec::new();
    files.push(write_text_file(&root, SESSION_FILE, &json_session(session)));
    for position in positions {
        let position_dir = format!("positions/{}", position.dir);
        files.push(write_text_file(
            &root,
            &format!("{position_dir}/{POSITION_FILE}"),
            &json_position(position),
        ));
        let capture = vec![0u8; position.capture.bytes as usize];
        let path = root.join(&position.capture.relative_path);
        fs::write(&path, &capture).expect("write capture.raw");
        files.push((position.capture.relative_path.clone(), sha256_hex(&capture)));
        if let Some(reference) = &position.reference {
            let raw = vec![0u8; reference.bytes as usize];
            let path = root.join(&reference.relative_path);
            fs::write(&path, &raw).expect("write reference.raw");
            files.push((reference.relative_path.clone(), sha256_hex(&raw)));
        }
        for measurement in &position.measurements {
            files.push(write_text_file(
                &root,
                &measurement.analysis_relative_path,
                &json_analysis(&measurement.analysis),
            ));
            files.push(write_text_file(
                &root,
                &measurement.diag_relative_path,
                &json_diag(&measurement.diag),
            ));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut sums = String::new();
    for (path, hash) in &files {
        sums.push_str(hash);
        sums.push_str("  ");
        sums.push_str(path);
        sums.push('\n');
    }
    fs::write(root.join(SHA256SUMS_FILE), sums.as_bytes()).expect("write SHA256SUMS");
    (root, files)
}

#[test]
fn t49_timebase_end_to_end_ingest_bundle() {
    let spec = Spec::known();
    let session = make_session(&spec);
    let mut spec_b = Spec::known();
    spec_b.ppm = -3.0;
    spec_b.declared_ppm = -3.0;
    spec_b.capture0 = 960.0;
    let p01 = position_with("p01-mlp", PositionRole::Design, 1.0, &spec);
    let p02 = position_with("p02-sofa", PositionRole::Validation, 0.6, &spec_b);
    let (root, _files) = materialize_bundle(&session, &[p01, p02]);
    let bundle = read_bundle(&root).expect("real ingest read_bundle");
    assert_eq!(bundle.positions.len(), 2);
    assert_eq!(bundle.design_count(), 1);
    assert_eq!(bundle.validation_count(), 1);

    let timebase =
        reconstruct_bundle(&bundle, d0(&spec)).expect("timebase from real ingest bundle");
    assert_eq!(timebase.positions.len(), 2);
    assert_close(
        timebase
            .position("p01-mlp")
            .expect("p01")
            .clock
            .residual_ppm,
        KNOWN_PPM,
        1.0e-6,
        "p01 ppm",
    );
    assert_close(
        timebase
            .position("p02-sofa")
            .expect("p02")
            .clock
            .residual_ppm,
        -3.0,
        1.0e-6,
        "p02 ppm",
    );
    let summary = timebase.summary();
    assert_eq!(summary.schema, TIMEBASE_SCHEMA);
    assert_eq!(summary.warnings.len(), 0);

    // 公共相位（无 reference）端到端。
    let p01_ingested = bundle.position("p01-mlp").expect("p01 ingested");
    let p01_timebase = timebase.position("p01-mlp").expect("p01 timebase");
    let common = reconstruct_common_phase(p01_timebase, p01_ingested, &bundle.session, None)
        .expect("common phase");
    assert_eq!(common.slots.len(), 5);
    assert!(common.reference.is_none());
    assert!(common.relative.is_empty());
    assert!(common
        .warnings
        .iter()
        .any(|warning| warning.code == "W_TB_ABSOLUTE_PHASE_ONLY"));
    let fl = common.slot("FL").expect("FL");
    assert_complex_close(
        fl.absolute.points(),
        &expected_after_d0(
            &p01_ingested.measurement("FL").expect("FL").analysis,
            d0(&spec),
        ),
        1.0e-9,
        "e2e absolute",
    );

    let _ = fs::remove_dir_all(&root);
}
