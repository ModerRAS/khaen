// NOTE(port): legacy/pre-rewrite rt-stack/crates/calibration/tests/meas_fix/robust_snr.rs —— 原样迁入；无改动。
// meas-fix（T3 remeasure SNR 断崖）合成单测。
//
// 注册方式：crates/calibration/src/capture.rs 末尾
//   #[cfg(test)] mod meas_fix_tests { include!("tests/meas_fix/robust_snr.rs"); }
// 运行：cargo test -p khaen-calibration meas_fix
//
// **合成数据，不是真实板端证据**：按 g0-g1 snr-cliff.json 的实测参数构造
//   - 采集长度 731008 帧（48 kHz, 15.229 s）
//   - 真实扫频 onset t=3.15 s（aplay/设备启动比 seed+pre 的 2.695 s 晚 ~0.45 s）
//   - 噪声本底 −74 dBFS；单遍扫频 RMS −57 dBFS；apply-host 退出流切换 pop：
//     t≈6 ms、峰值 −31.57 dBFS、τ=50 ms 衰减
// 断言：旧固定前静音窗把 SNR 判成负值（FAIL），稳健估计器判回 ~17 dB（PASS）；
// 真噪声抬升（−74→−69 dBFS）负例仍 FAIL。

use super::{
    seed_from_stream, CaptureStreamRole, CapturedStream, RepeatGrid, RepeatSlot, SampleFormat,
};
use crate::noise_est::{
    estimate, legacy_fixed_pre_silence_snr_db, OnsetSource, RobustSnrConfig, SILENCE_DBFS,
};
use crate::stage_backend::{
    compute_snr_db, snr_estimate, SnrGateReport, StageBackendConfig, DEFAULT_APPLY_SETTLE_MS,
    MIN_APPLY_SETTLE_MS,
};

const FS: u32 = 48_000;
const CAPTURE_FRAMES: usize = 731_008;
const TRUE_ONSET_S: f64 = 3.15;
const SWEEP_SECONDS: f64 = 5.0;
const GAP_SECONDS: f64 = 0.5;
const REPEATS: u32 = 2;
const PRE_FRAMES: u64 = 72_064; // 1.5013 s（timeline pre_frames）
const SEED_RENDER_FRAME: f64 = 57_286.449_504; // engine 实测 seed（采集先起 => 正）
const NOISE_DBFS: f64 = -74.0;
const SWEEP_RMS_DBFS: f64 = -57.0;
const POP_PEAK_DBFS: f64 = -31.57;
const POP_TAU_SECONDS: f64 = 0.050;
const T3_GATE_DB: f64 = 15.0; // 14:30 复测冻结门值的合成对照值

fn fake_grid() -> RepeatGrid {
    let sweep = (SWEEP_SECONDS * FS as f64) as u64;
    let gap = (GAP_SECONDS * FS as f64) as u64;
    let first = PRE_FRAMES;
    RepeatGrid {
        session_id: "meas-fix-synthetic".to_string(),
        layout: "4.1.2".to_string(),
        epoch_id: 1,
        graph_id: "0123456789abcdef".repeat(4),
        sample_rate_hz: FS,
        block_frames: 128,
        start_hz: 20.0,
        end_hz: 20_000.0,
        sweep_frames: sweep,
        gap_frames: gap,
        fade_frames: 480,
        pre_frames: PRE_FRAMES,
        post_frames: PRE_FRAMES,
        total_frames: PRE_FRAMES * 2 + sweep * REPEATS as u64 + gap,
        level_dbfs: -25.0,
        peak_linear: 10.0f64.powf(-25.0 / 20.0),
        repeats: REPEATS,
        slots: vec![RepeatSlot {
            container_slot: 0,
            speaker: "FL".to_string(),
            channel: 0,
            render_frame_index: first,
            repeat_render_frames: vec![first, first + sweep + gap],
        }],
    }
}

/// 确定性 LCG（[-1,1) 伪噪声；测试不需要严格白噪声谱）。
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed)
    }
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }
}

fn noise_amplitude(dbfs: f64) -> f64 {
    10.0f64.powf(dbfs / 20.0) * 3.0f64.sqrt() // uniform [-1,1] rms = 1/sqrt(3)
}

fn sweep_sample_amplitude(rms_dbfs: f64) -> f64 {
    10.0f64.powf(rms_dbfs / 20.0) * std::f64::consts::SQRT_2
}

/// 对数扫频（与 T62 同相位公式）；rms 目标 = 单遍扫频正弦 RMS。
fn log_sweep_sample(grid: &RepeatGrid, frames_from_onset: f64, rms_dbfs: f64) -> f64 {
    let sweep_frames = grid.sweep_frames as f64;
    if !(0.0..sweep_frames).contains(&frames_from_onset) {
        return 0.0;
    }
    let duration = sweep_frames / FS as f64;
    let log_ratio = (grid.end_hz / grid.start_hz).ln();
    let time = frames_from_onset / FS as f64;
    let phase = 2.0 * std::f64::consts::PI * grid.start_hz * duration / log_ratio
        * ((time / duration * log_ratio).exp() - 1.0);
    let fade = grid.fade_frames as f64;
    let envelope = if frames_from_onset < fade {
        0.5 * (1.0 - (std::f64::consts::PI * frames_from_onset / (fade - 1.0)).cos())
    } else if frames_from_onset + fade >= sweep_frames {
        let local = frames_from_onset - (sweep_frames - fade);
        0.5 * (1.0 + (std::f64::consts::PI * local / (fade - 1.0)).cos())
    } else {
        1.0
    };
    sweep_sample_amplitude(rms_dbfs) * envelope * phase.sin()
}

/// 合成采集：pop（可选）+ 2 遍真扫频 + 噪声底。
fn synthetic_capture(noise_dbfs: f64, with_pop: bool) -> Vec<f32> {
    let grid = fake_grid();
    let mut rng = Lcg::new(0x5EED_5EED);
    let noise = noise_amplitude(noise_dbfs);
    let onset = (TRUE_ONSET_S * FS as f64).round() as usize;
    let pop_peak = 10.0f64.powf(POP_PEAK_DBFS / 20.0);
    let mut samples = vec![0.0f32; CAPTURE_FRAMES];
    for (index, sample) in samples.iter_mut().enumerate() {
        let mut value = noise * rng.next();
        if with_pop && index < (1.0 * FS as f64) as usize {
            let t = index as f64 / FS as f64;
            let envelope = (-t / POP_TAU_SECONDS).exp();
            value += pop_peak * envelope * (2.0 * std::f64::consts::PI * 1_000.0 * t).sin();
        }
        *sample = value as f32;
    }
    // 两遍扫频（sweep1 起点 = TRUE_ONSET）。
    let sweep = grid.sweep_frames as usize;
    let gap = grid.gap_frames as usize;
    for (offset, frame) in [0usize, sweep + gap].into_iter().enumerate() {
        let start = onset + frame;
        for k in 0..sweep {
            let index = start + k;
            if index >= samples.len() {
                break;
            }
            samples[index] += log_sweep_sample(&grid, k as f64, SWEEP_RMS_DBFS) as f32;
        }
        let _ = offset;
    }
    samples
}

fn gate_passes(snr_db: Option<f64>, gate_db: f64) -> bool {
    snr_db.map(|snr| snr >= gate_db).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// 1) seed 符号/语义：直接返回 "render frame 0 对应的采集 sample 索引"
// ---------------------------------------------------------------------------

fn stream_with_seed(seed: f64) -> CapturedStream {
    CapturedStream {
        role: CaptureStreamRole::UmikAdc,
        device: "hw:0,0".to_string(),
        session_id: "meas-fix-seed".to_string(),
        epoch_id: 1,
        graph_id: "0123456789abcdef".repeat(4),
        epoch_start_mono_ns: 0,
        capture_start_render_frame: Some(seed),
        sample_format: SampleFormat::S24_3LE,
        sample_rate_hz: FS,
        chunk_frames: 128,
        raw: Vec::new(),
        samples: Vec::new(),
        blocks: Vec::new(),
        peak_abs: 0.0,
        clipped_samples: 0,
        nonfinite_samples: 0,
        frames: 0,
        reference_channel: None,
    }
}

#[test]
fn meas_fix_seed_from_stream_keeps_engine_sign() {
    // 采集先起（settle/预滚）=> seed 为正；修复前这里返回 −seed，噪声窗被 clamp 到 0。
    let stream = stream_with_seed(SEED_RENDER_FRAME);
    let seed = seed_from_stream(&stream).expect("seed");
    assert!(
        (seed - SEED_RENDER_FRAME).abs() < 1e-9,
        "seed must keep engine sign: got {seed}"
    );
    assert!(seed > 0.0);
    // 采集晚于 render 0 => seed 为负（合法时基，窗口侧必须 clamp 而不是把 onset 当成正）。
    let late = stream_with_seed(-1_234.0);
    assert!((seed_from_stream(&late).expect("seed") - (-1_234.0)).abs() < 1e-9);
}

// ---------------------------------------------------------------------------
// 2) 正例：pop 在 t=6 ms + 正常扫频 → 稳健估计判正常，旧逻辑判失败
// ---------------------------------------------------------------------------

#[test]
fn meas_fix_pop_at_6ms_old_logic_fails_new_estimator_passes() {
    let samples = synthetic_capture(NOISE_DBFS, true);
    let grid = fake_grid();
    let legacy = legacy_fixed_pre_silence_snr_db(
        &samples,
        grid.sweep_frames,
        grid.first_render_frame().expect("first frame"),
        SEED_RENDER_FRAME,
    )
    .expect("legacy estimate");
    // 旧口径：噪声窗 [0, 2.695 s) 含 pop → SNR 判死（远低于门）。
    assert!(
        !gate_passes(Some(legacy), T3_GATE_DB),
        "pre-fix fixed pre-silence window must fail on the stream-close pop, got {legacy} dB"
    );
    assert!(legacy < 10.0, "legacy SNR should collapse, got {legacy} dB");

    let expected_onset = SEED_RENDER_FRAME + grid.first_render_frame().unwrap() as f64;
    let robust = estimate(
        &samples,
        FS,
        grid.sweep_frames,
        Some(expected_onset),
        &RobustSnrConfig::default(),
    )
    .expect("robust estimate");
    // onset 必须跟随真实起点（3.15 s），而不是锚点 2.695 s。
    assert_eq!(robust.onset_source, OnsetSource::Detected);
    let detected_s = robust.onset_sample / FS as f64;
    assert!(
        (detected_s - TRUE_ONSET_S).abs() <= 0.030,
        "onset should track true 3.15 s, got {detected_s:.3} s"
    );
    // 噪声不得被 pop 污染（−74 dBFS ± 2 dB）。
    assert!(
        (robust.noise_rms_dbfs - NOISE_DBFS).abs() <= 2.0,
        "robust noise {:?} dBFS must stay near {} dBFS",
        robust.noise_rms_dbfs,
        NOISE_DBFS
    );
    assert!(
        robust.transient_rejected,
        "pop must be flagged as transient"
    );
    assert!(
        (robust.snr_db - 17.0).abs() <= 2.5,
        "robust SNR should recover ~17 dB, got {} dB",
        robust.snr_db
    );
    // 端到端（stage backend 公开入口）与稳健估计一致。
    let end_to_end = compute_snr_db(&samples, &grid, SEED_RENDER_FRAME).expect("snr");
    assert!(
        (end_to_end - robust.snr_db).abs() < 1e-9,
        "compute_snr_db must delegate to the robust estimator"
    );
    assert!(
        gate_passes(Some(end_to_end), T3_GATE_DB),
        "post-fix SNR {} dB must pass the frozen T3 gate {T3_GATE_DB} dB",
        end_to_end
    );
    assert!(end_to_end - legacy > 10.0);
}

// ---------------------------------------------------------------------------
// 3) 负例：真噪声抬升（−74 → −69 dBFS，无 pop）→ 仍然 FAIL
// ---------------------------------------------------------------------------

#[test]
fn meas_fix_true_noise_rise_still_fails() {
    let samples = synthetic_capture(-69.0, false);
    let grid = fake_grid();
    let expected_onset = SEED_RENDER_FRAME + grid.first_render_frame().unwrap() as f64;
    let robust = estimate(
        &samples,
        FS,
        grid.sweep_frames,
        Some(expected_onset),
        &RobustSnrConfig::default(),
    )
    .expect("robust estimate");
    assert_eq!(robust.onset_source, OnsetSource::Detected);
    assert!(
        robust.snr_db < T3_GATE_DB,
        "true noise rise must still fail the {T3_GATE_DB} dB gate, got {} dB",
        robust.snr_db
    );
    assert!(
        !gate_passes(Some(robust.snr_db), T3_GATE_DB),
        "gate must FAIL on real noise rise"
    );
    assert!(
        (robust.noise_rms_dbfs - (-69.0)).abs() <= 2.0,
        "robust noise should follow the real rise, got {:?} dBFS",
        robust.noise_rms_dbfs
    );
    // 端到端同样 FAIL（不能修成永远通过）。
    assert!(!gate_passes(
        compute_snr_db(&samples, &grid, SEED_RENDER_FRAME),
        T3_GATE_DB
    ));
}

// ---------------------------------------------------------------------------
// 4) 兼容：干净采集（无 pop）新旧口径一致（健康路径不能被改坏）
// ---------------------------------------------------------------------------

#[test]
fn meas_fix_clean_capture_matches_legacy_estimate() {
    let samples = synthetic_capture(NOISE_DBFS, false);
    let grid = fake_grid();
    let legacy = legacy_fixed_pre_silence_snr_db(
        &samples,
        grid.sweep_frames,
        grid.first_render_frame().unwrap(),
        SEED_RENDER_FRAME,
    )
    .expect("legacy");
    let robust = compute_snr_db(&samples, &grid, SEED_RENDER_FRAME).expect("robust");
    assert!((legacy - 17.0).abs() <= 2.5, "legacy clean {legacy} dB");
    assert!(
        (robust - legacy).abs() <= 2.0,
        "clean capture: robust {robust} dB vs legacy {legacy} dB"
    );
    assert!(gate_passes(Some(robust), T3_GATE_DB));
}

// ---------------------------------------------------------------------------
// 5) 纯静音 / 无法估计：返回 None 而不是伪造 PASS
// ---------------------------------------------------------------------------

#[test]
fn meas_fix_silent_capture_is_not_estimable() {
    let grid = fake_grid();
    let samples = vec![0.0f32; CAPTURE_FRAMES];
    let robust = estimate(
        &samples,
        FS,
        grid.sweep_frames,
        Some(SEED_RENDER_FRAME + grid.first_render_frame().unwrap() as f64),
        &RobustSnrConfig::default(),
    );
    match robust {
        None => {}
        Some(estimate) => {
            assert!(estimate.snr_db <= SILENCE_DBFS || estimate.signal_rms_dbfs <= SILENCE_DBFS);
            assert!(!gate_passes(Some(estimate.snr_db), T3_GATE_DB));
        }
    }
    assert!(!gate_passes(
        compute_snr_db(&samples, &grid, SEED_RENDER_FRAME),
        T3_GATE_DB
    ));
}

// ---------------------------------------------------------------------------
// 6) apply 后 settle 配置（≥500 ms；默认 1 s；门值冻结进证据）
// ---------------------------------------------------------------------------

#[test]
fn meas_fix_apply_settle_config_is_parameterized_and_validated() {
    assert_eq!(StageBackendConfig::default().apply_settle_ms, 1_000);
    assert_eq!(DEFAULT_APPLY_SETTLE_MS, 1_000);
    assert_eq!(MIN_APPLY_SETTLE_MS, 500);
    assert!(StageBackendConfig::validate_apply_settle(0).is_ok());
    assert!(StageBackendConfig::validate_apply_settle(500).is_ok());
    assert!(StageBackendConfig::validate_apply_settle(1_000).is_ok());
    assert!(StageBackendConfig::validate_apply_settle(5_000).is_ok());
    assert!(StageBackendConfig::validate_apply_settle(100).is_err());
    assert!(StageBackendConfig::validate_apply_settle(5_001).is_err());
}

#[test]
fn meas_fix_snr_gate_report_freezes_estimator_and_gate_value() {
    let samples = synthetic_capture(NOISE_DBFS, true);
    let grid = fake_grid();
    let estimate = snr_estimate(&samples, &grid, SEED_RENDER_FRAME).expect("estimate");
    let report = SnrGateReport::from_estimate(&estimate, FS, 18.0);
    assert_eq!(report.mode, "robust-broadband");
    assert_eq!(report.gate_min_snr_db, 18.0);
    assert_eq!(report.onset_source, "detected");
    // 17.4 dB < 18 dB → 门如实 FAIL（门值可配、判据不注水）。
    assert!(!report.passed);
    let gate15 = SnrGateReport::from_estimate(&estimate, FS, 15.0);
    assert!(gate15.passed);
    let json = gate15.to_json();
    assert!(json.contains("\"gate_min_snr_db\":15"));
    assert!(json.contains("\"mode\":\"robust-broadband\""));
    assert!(json.contains("\"onset_source\":\"detected\""));
    assert!(json.contains("\"schema\":\"khaen-robust-snr/v1\""));
}
