//! 输出质量测试（T41 交付 3c）：THD+N / 杂散阈值（poly 与 sinc 分别给阈值）。
//!
//! 两个层面分开量：
//! 1. **插值本征失真**：直接驱动 [khaen_asrc::AsyncResampler]、比率固定，
//!    测的是 poly/sinc 插值器本身的失真 + f32 量化（poly ≤ -110 dB，sinc ≤ -130 dB）；
//! 2. **整环输出 THD+N**：抖动环 + 漂移控制 + 重采样整链（漂移 ±200 ppm、1 s 输出），
//!    阈值较松（poly/sinc ≤ -72/-73 dB）：残差由亚赫兹级的比率慢抖动主导
//!    （±30 ppm、~2 Hz 的不可闻音高抖动，见 README 误差预算），不是插值失真。
mod support;

use khaen_asrc::{AsrcConfig, AsyncResampler, Interpolation};
use support::*;

/// 整环 THD+N 阈值（dB，越负越好）。
const POLY_LOOP_THRESHOLD_DB: f64 = -72.0;
const SINC_LOOP_THRESHOLD_DB: f64 = -73.0;
/// 固定比率插值本征 THD+N 阈值（dB）。
const POLY_INTRINSIC_THRESHOLD_DB: f64 = -110.0;
const SINC_INTRINSIC_THRESHOLD_DB: f64 = -130.0;
/// 整环仿真参数。
const LOOP_DRIFT_PPM: f64 = 200.0;
const LOOP_SECONDS: f64 = 30.0;
const LOOP_WARMUP_SECONDS: f64 = 20.0;
const LOOP_COLLECT_SECONDS: f64 = 1.0;
const TARGET_FILL: usize = 256;

/// 整环输出（收敛后 1 s）的 10 ms 分窗 THD+N。
fn loop_thd_n(interp: Interpolation) -> f64 {
    let sim = SimConfig {
        run_seconds: LOOP_SECONDS,
        warmup_seconds: LOOP_WARMUP_SECONDS,
        collect_seconds: LOOP_COLLECT_SECONDS,
        ..SimConfig::default()
    };
    let config = asrc_config(&sim, interp, TARGET_FILL);
    let outcome = run_with_asrc(&sim, &config, LOOP_DRIFT_PPM);
    assert_eq!(outcome.underruns, 0, "{interp:?}: 质量测量窗口内出现欠载");
    assert!(
        outcome.output.len() >= 480 * sim.channels,
        "{interp:?}: 收集到的输出太短"
    );
    // 发送端快 200 ppm → 本地输出频率 = 1000 × (1 + 200e-6)；按该频率拟合基波。
    let nominal = TONE_HZ * (1.0 + LOOP_DRIFT_PPM * 1e-6);
    tone_thd_n_db(&outcome.output, sim.channels, sim.sample_rate, nominal)
}

#[test]
fn measurement_metric_rejects_a_dirty_signal() {
    // 自检：纯 f32 正弦的 THD+N 必须极低（否则指标本身不可信）。
    let mut reference = Vec::new();
    for index in 0..4800usize {
        let phase = std::f64::consts::TAU * TONE_HZ / f64::from(SAMPLE_RATE) * index as f64;
        let value = (phase.sin() * f64::from(TONE_AMPLITUDE)) as f32;
        for _ in 0..2 {
            reference.push(value);
        }
    }
    let thd = tone_thd_n_db(&reference, 2, SAMPLE_RATE, TONE_HZ);
    assert!(thd < -140.0, "纯正弦的 THD+N 指标异常：{thd:.2} dB");
    // 反例：混入 1% 的三次谐波必须被指标抓到（≈ -40 dB）。
    let mut dirty = reference.clone();
    for frame in 0..reference.len() / 2 {
        let phase = std::f64::consts::TAU * TONE_HZ / f64::from(SAMPLE_RATE) * frame as f64;
        let harmonic = (phase * 3.0).sin() as f32 * 0.005;
        dirty[frame * 2] += harmonic;
        dirty[frame * 2 + 1] += harmonic;
    }
    let dirty_thd = tone_thd_n_db(&dirty, 2, SAMPLE_RATE, TONE_HZ);
    assert!(
        dirty_thd > -60.0 && dirty_thd < -30.0,
        "含 1% 三次谐波的 THD+N 不应为 {dirty_thd:.2} dB"
    );
}

#[test]
fn loop_output_thd_n_poly_below_threshold() {
    let thd = loop_thd_n(Interpolation::Poly);
    assert!(
        thd <= POLY_LOOP_THRESHOLD_DB,
        "poly 整环 THD+N = {thd:.2} dB，阈值 {POLY_LOOP_THRESHOLD_DB:.1} dB"
    );
}

#[test]
fn loop_output_thd_n_sinc_below_threshold() {
    let thd = loop_thd_n(Interpolation::Sinc);
    assert!(
        thd <= SINC_LOOP_THRESHOLD_DB,
        "sinc 整环 THD+N = {thd:.2} dB，阈值 {SINC_LOOP_THRESHOLD_DB:.1} dB"
    );
}

/// 直接驱动重采样器（比率固定）测插值本征失真：不经过抖动环/控制器。
fn intrinsic_thd_n(interp: Interpolation, ratio_ppm: f64) -> f64 {
    let mut config = AsrcConfig::new(2, 128, SAMPLE_RATE);
    config.interp = interp;
    let mut resampler = AsyncResampler::new(&config).expect("resampler");
    resampler.set_ratio_ppm(ratio_ppm).expect("set ratio");
    let omega = std::f64::consts::TAU * TONE_HZ / f64::from(SAMPLE_RATE);
    let mut media_frame = 0u64;
    let mut output = Vec::new();
    let blocks = 2000usize;
    let collect_from = 1900usize;
    for block in 0..blocks {
        let need = resampler.input_frames_next();
        {
            let input = resampler.input_mut();
            for frame in 0..need {
                let value = (omega * media_frame as f64).sin() as f32 * TONE_AMPLITUDE;
                for channel in 0..2 {
                    input.channel_mut(channel)[frame] = value;
                }
                media_frame += 1;
            }
        }
        resampler.process().expect("process");
        if block >= collect_from {
            let planar = resampler.output();
            for frame in 0..config.block_frames {
                for channel in 0..2 {
                    output.push(planar.channel(channel)[frame]);
                }
            }
        }
    }
    // 比率 r = 1 + ppm·1e-6（输出/输入），1 kHz 输入对应的输出频率 = 1000 / r。
    let nominal = TONE_HZ / (1.0 + ratio_ppm * 1e-6);
    tone_thd_n_db(&output, 2, SAMPLE_RATE, nominal)
}

#[test]
fn intrinsic_interpolation_thd_n_meets_thresholds() {
    for ratio_ppm in [0.0f64, -200.0, 500.0] {
        let poly = intrinsic_thd_n(Interpolation::Poly, ratio_ppm);
        assert!(
            poly <= POLY_INTRINSIC_THRESHOLD_DB,
            "poly 固定比率 {ratio_ppm:+.0} ppm 的插值失真 THD+N = {poly:.2} dB，阈值 {POLY_INTRINSIC_THRESHOLD_DB:.1} dB"
        );
        let sinc = intrinsic_thd_n(Interpolation::Sinc, ratio_ppm);
        assert!(
            sinc <= SINC_INTRINSIC_THRESHOLD_DB,
            "sinc 固定比率 {ratio_ppm:+.0} ppm 的插值失真 THD+N = {sinc:.2} dB，阈值 {SINC_INTRINSIC_THRESHOLD_DB:.1} dB"
        );
    }
}
