//! Welch 功率谱与 1/3 倍频程频带电平。
//!
//! # 算法（与 khaen-calibration::band_levels 完全同一口径）
//!
//! - Hann 窗 w[n] = 0.5 - 0.5*cos(2*pi*n/N)，步长 = 窗长/2，功率域平均；
//! - 单边 bin 功率 P[k] = c(k)*|X[k]|^2 / (N*sum(w^2))，c(0) = c(N/2) = 1，其余 c = 2；
//! - 单边 PSD S[k] = P[k]/df = P[k]*N/fs（单位 满幅^2/Hz，df = fs/N）；
//! - 1/3 oct 带 [c*2^(-1/6), c*2^(+1/6)) 内 bin 求和 = 带功率；带电平 = 10*log10(带功率)。
//!
//! # dBFS 口径
//!
//! 满幅幅度 = 1.0（数字满幅）；参考功率 = 1.0（满幅方波均方）。因此**幅度 1.0 的正弦读
//! -3.01 dBFS**（即它的 RMS），与通常「0 dBFS 满幅正弦」的 dBFS 定义一致。
//!
//! 带功率**精确为 0** 的带输出 null（缺数据，绝不编数值）。注意：实际录音里无信号的带通常
//! 不是精确 0，而是 FFT 舍入产生的极低电平（约 -130 dBFS 级，随幅度/窗长变化）；工具如实
//! 报出该数值、不设人为门限（与 khaen-band-levels 的 power > 0 口径一致）。判读时不要把低于
//! 约 -100 dBFS 的带当真实信号。
//!
//! # 与 khaen-band-levels 的关系
//!
//! 带功率与相对电平直接调用 khaen-calibration 的实现（同一份 Hann/Welch/频带积分代码）；
//! 本模块只在其上做 dB 换算与 PSD 数组输出，并用单元测试锁定「PSD 按 bin 带宽积分 =
//! 带功率」，防止两条路径悄悄分叉。

use std::f64::consts::PI;

use khaen_calibration::band_levels::BandLevelsConfig;
use khaen_calibration::{band_powers, BandLevel};
use realfft::num_complex::Complex;
use realfft::RealFftPlanner;
use serde_json::Value;

use crate::json;
use crate::wav::Wav;
use crate::{codes, AnalyzeError, SPECTRUM_SCHEMA, TOOL_NAME};

pub use khaen_calibration::band_levels::{DEFAULT_WINDOW_LEN, MAX_WINDOW_LEN, MIN_WINDOW_LEN};
pub use khaen_calibration::plan_levels::DEFAULT_CENTERS_HZ;

/// 窗函数名（写入报告，便于口径审计）。
pub const WINDOW_NAME: &str = "hann";

/// 单边 Welch 功率谱密度（满幅^2/Hz）。
#[derive(Clone, Debug, PartialEq)]
pub struct Psd {
    /// 采样率（Hz）。
    pub sample_rate_hz: f64,
    /// 窗长（样本）。
    pub window_len: usize,
    /// bin 带宽（Hz）= fs/window_len。
    pub bin_hz: f64,
    /// 参与平均的分段数。
    pub segments: usize,
    /// 单边 PSD，长度 = window_len/2 + 1；下标 k 的频率 = k*bin_hz。
    pub power: Vec<f64>,
}

/// 频谱分析配置。
#[derive(Clone, Debug, PartialEq)]
pub struct SpectrumConfig {
    /// 每段 FFT 窗长（样本，2 的幂，256..=65536）；步长固定为一半。
    pub window_len: usize,
    /// 1/3 oct 带中心频率（Hz，严格升序、低于 Nyquist）。
    pub centers_hz: Vec<f64>,
    /// 是否附加输出原始单边 PSD。
    pub include_psd: bool,
}

impl Default for SpectrumConfig {
    fn default() -> Self {
        Self {
            window_len: DEFAULT_WINDOW_LEN,
            centers_hz: DEFAULT_CENTERS_HZ.to_vec(),
            include_psd: false,
        }
    }
}

/// 一次频谱分析结果。
#[derive(Clone, Debug, PartialEq)]
pub struct SpectrumReport {
    /// 采样率（Hz）。
    pub sample_rate_hz: f64,
    /// 源 WAV 声道数。
    pub channels: u16,
    /// 样本数（已混单声道）。
    pub samples: usize,
    /// 窗长（样本）。
    pub window_len: usize,
    /// 参与平均的分段数。
    pub segments: usize,
    /// 逐带电平（dBFS；中心频率升序；无数据为 None）。
    pub bands: Vec<BandLevel>,
    /// 原始单边 PSD（未请求时为 None）。
    pub psd: Option<Psd>,
}

/// 分析一段 WAV：1/3 oct 带电平（+ 可选原始 PSD）。
pub fn analyze_wav(wav: &Wav, config: &SpectrumConfig) -> Result<SpectrumReport, AnalyzeError> {
    let bands = band_levels(
        &wav.samples,
        wav.sample_rate_hz,
        &config.centers_hz,
        config.window_len,
    )?;
    let psd = if config.include_psd {
        Some(welch_psd(
            &wav.samples,
            wav.sample_rate_hz,
            config.window_len,
        )?)
    } else {
        None
    };
    let segments = match &psd {
        Some(psd) => psd.segments,
        None => segment_count(wav.samples.len(), config.window_len),
    };
    Ok(SpectrumReport {
        sample_rate_hz: wav.sample_rate_hz,
        channels: wav.channels,
        samples: wav.samples.len(),
        window_len: config.window_len,
        segments,
        bands,
        psd,
    })
}

/// 单段录音的逐带绝对电平（dBFS，中心频率升序）。
///
/// 带功率直接调用 khaen-calibration 的 band_powers（与 khaen-band-levels 同一份实现）。
pub fn band_levels(
    samples: &[f64],
    sample_rate_hz: f64,
    centers_hz: &[f64],
    window_len: usize,
) -> Result<Vec<BandLevel>, AnalyzeError> {
    let config = BandLevelsConfig { window_len };
    let powers = band_powers(samples, sample_rate_hz, centers_hz, &config)
        .map_err(|err| AnalyzeError::new(err.code(), err.message().to_string()))?;
    Ok(centers_hz
        .iter()
        .copied()
        .zip(powers)
        .map(|(center_hz, power)| BandLevel {
            center_hz,
            level_db: power_to_db(power),
        })
        .collect())
}

/// 单边 Welch PSD（满幅^2/Hz）；Hann 窗、步长一半、功率域平均。
pub fn welch_psd(
    samples: &[f64],
    sample_rate_hz: f64,
    window_len: usize,
) -> Result<Psd, AnalyzeError> {
    validate_window(window_len)?;
    if !sample_rate_hz.is_finite() || sample_rate_hz <= 0.0 {
        return Err(AnalyzeError::new(
            codes::BAND_LEVELS_BAD_INPUT,
            format!("sample rate {sample_rate_hz} must be a finite value > 0"),
        ));
    }
    if samples.len() < window_len {
        return Err(AnalyzeError::new(
            codes::BAND_LEVELS_BAD_INPUT,
            format!(
                "recording has {} samples, need at least one full window of {window_len}",
                samples.len()
            ),
        ));
    }
    if let Some(index) = samples.iter().position(|sample| !sample.is_finite()) {
        return Err(AnalyzeError::new(
            codes::BAND_LEVELS_BAD_INPUT,
            format!("sample {index} is not finite"),
        ));
    }

    let window = hann_window(window_len);
    let window_energy: f64 = window.iter().map(|value| value * value).sum();
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(window_len);
    let mut buffer = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let mut scratch = vec![Complex::new(0.0, 0.0); fft.get_scratch_len()];
    let bins = window_len / 2 + 1;
    let mut accumulated = vec![0.0f64; bins];
    let hop = window_len / 2;
    let mut segments = 0usize;
    let mut start = 0usize;
    while start + window_len <= samples.len() {
        for (index, value) in buffer.iter_mut().enumerate() {
            *value = samples[start + index] * window[index];
        }
        fft.process_with_scratch(&mut buffer, &mut spectrum, &mut scratch)
            .map_err(|err| {
                AnalyzeError::new(
                    codes::ANALYZE_CONFIG,
                    format!("realfft failed for window {window_len}: {err}"),
                )
            })?;
        for (bin, value) in spectrum.iter().enumerate() {
            let factor = if bin == 0 || bin == window_len / 2 {
                1.0
            } else {
                2.0
            };
            accumulated[bin] += factor * (value.re * value.re + value.im * value.im);
        }
        segments += 1;
        start += hop;
    }
    if segments == 0 {
        return Err(AnalyzeError::new(
            codes::BAND_LEVELS_BAD_INPUT,
            "no complete analysis window in recording",
        ));
    }
    // PSD[k] = (1/segments) * sum(c*|X|^2) / (sum(w^2) * fs)：即 bin 功率除以 df = fs/N。
    let scale = 1.0 / (segments as f64 * window_energy * sample_rate_hz);
    Ok(Psd {
        sample_rate_hz,
        window_len,
        bin_hz: sample_rate_hz / window_len as f64,
        segments,
        power: accumulated.iter().map(|&value| value * scale).collect(),
    })
}

/// 频谱子命令的确定性 JSON 文档。
pub fn document(wav_path: &str, report: &SpectrumReport) -> Value {
    let bands: Vec<Value> = report
        .bands
        .iter()
        .map(|band| {
            json::object([
                ("center_hz", json::number(band.center_hz)),
                ("level_dbfs", json::opt_number(band.level_db)),
            ])
        })
        .collect();
    let mut entries = vec![
        ("schema", Value::from(SPECTRUM_SCHEMA)),
        ("tool", Value::from(TOOL_NAME)),
        ("tool_version", Value::from(env!("CARGO_PKG_VERSION"))),
        ("command", Value::from("spectrum")),
        ("wav", Value::from(wav_path)),
        ("sample_rate_hz", json::number(report.sample_rate_hz)),
        ("channels", json::count(u64::from(report.channels))),
        ("samples", json::count(report.samples as u64)),
        (
            "duration_s",
            json::number(report.samples as f64 / report.sample_rate_hz),
        ),
        ("window", Value::from(WINDOW_NAME)),
        ("window_len", json::count(report.window_len as u64)),
        ("hop", json::count((report.window_len / 2) as u64)),
        ("segments", json::count(report.segments as u64)),
        ("level_unit", Value::from("dBFS")),
        ("bands", Value::Array(bands)),
    ];
    if let Some(psd) = report.psd.as_ref() {
        entries.push((
            "psd",
            json::object([
                ("bin_hz", json::number(psd.bin_hz)),
                ("segments", json::count(psd.segments as u64)),
                ("window_len", json::count(psd.window_len as u64)),
                ("unit", Value::from("full_scale_squared_per_hz")),
                ("values", json::numbers(&psd.power)),
            ]),
        ));
    }
    json::object(entries)
}

/// 1/3 oct 带内 bin 功率求和（把 PSD 按 bin 带宽积分）。
///
/// 私有实现，仅供单元测试对拍「PSD 积分 = band_powers」；边界包含规则与
/// khaen-calibration::band_levels 一致（[c/2^(1/6), c*2^(1/6))，按中心列表首个匹配）。
#[cfg(test)]
fn psd_band_powers(psd: &Psd, centers_hz: &[f64]) -> Vec<f64> {
    let ratio = 2.0f64.powf(1.0 / 6.0);
    let mut powers = vec![0.0f64; centers_hz.len()];
    for (bin, &density) in psd.power.iter().enumerate() {
        let frequency = bin as f64 * psd.bin_hz;
        if let Some(index) = centers_hz
            .iter()
            .position(|&center| frequency >= center / ratio && frequency < center * ratio)
        {
            powers[index] += density * psd.bin_hz;
        }
    }
    powers
}

pub(crate) fn segment_count(samples: usize, window_len: usize) -> usize {
    if samples < window_len || window_len < 2 {
        0
    } else {
        (samples - window_len) / (window_len / 2) + 1
    }
}

fn validate_window(window_len: usize) -> Result<(), AnalyzeError> {
    if !window_len.is_power_of_two() {
        return Err(AnalyzeError::new(
            codes::BAND_LEVELS_CONFIG,
            format!("window_len {window_len} is not a power of two"),
        ));
    }
    if !(MIN_WINDOW_LEN..=MAX_WINDOW_LEN).contains(&window_len) {
        return Err(AnalyzeError::new(
            codes::BAND_LEVELS_CONFIG,
            format!("window_len {window_len} is outside [{MIN_WINDOW_LEN}, {MAX_WINDOW_LEN}]"),
        ));
    }
    Ok(())
}

fn hann_window(len: usize) -> Vec<f64> {
    (0..len)
        .map(|index| 0.5 - 0.5 * (2.0 * PI * index as f64 / len as f64).cos())
        .collect()
}

fn power_to_db(power: f64) -> Option<f64> {
    if power.is_finite() && power > 0.0 {
        Some(10.0 * power.log10())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq_hz: f64, amplitude: f64, frames: usize, sample_rate_hz: f64) -> Vec<f64> {
        (0..frames)
            .map(|index| amplitude * (2.0 * PI * freq_hz * index as f64 / sample_rate_hz).sin())
            .collect()
    }

    #[test]
    fn psd_integrates_to_calibration_band_powers() {
        let sample_rate_hz = 48_000.0;
        let samples = sine(1_000.0, 0.5, 48_000, sample_rate_hz);
        for window_len in [256usize, 1024, 4096, 8192] {
            let config = BandLevelsConfig { window_len };
            let expected = band_powers(&samples, sample_rate_hz, &DEFAULT_CENTERS_HZ, &config)
                .expect("band powers");
            let psd = welch_psd(&samples, sample_rate_hz, window_len).expect("psd");
            assert_eq!(psd.power.len(), window_len / 2 + 1);
            let got = psd_band_powers(&psd, &DEFAULT_CENTERS_HZ);
            let max_band = expected.iter().copied().fold(0.0f64, f64::max);
            for (index, (&got, &want)) in got.iter().zip(expected.iter()).enumerate() {
                if want > max_band * 1e-12 {
                    let tolerance = want * 1e-9;
                    assert!(
                        (got - want).abs() <= tolerance,
                        "window {window_len} band {index}: psd integral {got} vs band_powers {want}"
                    );
                } else {
                    assert!(
                        got <= max_band * 1e-12,
                        "window {window_len} band {index}: unexpected leakage {got}"
                    );
                }
            }
        }
    }

    #[test]
    fn full_scale_sine_reads_minus_3_dbfs() {
        let sample_rate_hz = 48_000.0;
        let samples = sine(1_000.0, 1.0, 48_000, sample_rate_hz);
        let levels = band_levels(
            &samples,
            sample_rate_hz,
            &DEFAULT_CENTERS_HZ,
            DEFAULT_WINDOW_LEN,
        )
        .expect("levels");
        let level = levels
            .iter()
            .find(|band| (band.center_hz - 1_000.0).abs() < 1e-9)
            .and_then(|band| band.level_db)
            .expect("1 kHz band");
        assert!((level - -3.0103).abs() < 0.01, "level {level}");
    }

    #[test]
    fn window_validation_uses_stable_codes() {
        let samples = sine(1_000.0, 0.5, 48_000, 48_000.0);
        let error = welch_psd(&samples, 48_000.0, 1_000).expect_err("not power of two");
        assert_eq!(error.code(), codes::BAND_LEVELS_CONFIG);
        let error = welch_psd(&samples, 48_000.0, 128).expect_err("below minimum");
        assert_eq!(error.code(), codes::BAND_LEVELS_CONFIG);
        let error = welch_psd(&samples[..100], 48_000.0, 256).expect_err("too short");
        assert_eq!(error.code(), codes::BAND_LEVELS_BAD_INPUT);
    }

    #[test]
    fn psd_segments_match_band_power_segments() {
        let samples = sine(1_000.0, 0.5, 20_000, 48_000.0);
        let psd = welch_psd(&samples, 48_000.0, 4096).expect("psd");
        assert_eq!(psd.segments, segment_count(samples.len(), 4096));
        assert_eq!(psd.segments, (20_000 - 4096) / 2048 + 1);
    }
}
