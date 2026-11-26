//! 短时 RMS 包络与掉音（dropout）检测。
//!
//! 背景：2026-11-26 真机闭环复测首次基线录音在 4.3-4.9 s 中途掉音（覆盖扫频 630-1000 Hz），
//! 30 带全频判定 FAIL（最大 delta 30.7 dB）；该录音作废重录后才回到 +-0.5 dB。本模块把这类
//! 问题变成复测前的自动检查：先 envelope 确认无掉音，再做 khaen-band-levels /
//! khaen-closed-loop-run 判定。
//!
//! # 算法（确定性、无状态）
//!
//! - 矩形窗（不加重叠窗函数），窗长 window_ms（默认 50 ms），步长 = 窗长/2；
//! - 每窗电平 20*log10(rms) dBFS（满幅幅度 1.0 = 0 dBFS 峰值；满幅正弦读 -3.01 dBFS）；
//!   整窗全零的窗记 null（理论 -inf dB，JSON 不编大负数）；
//! - 参考电平 = 全部有信号窗（非 null）的 dB 中位数（即「中位扫频电平」）；
//! - 掉音窗 = 电平 < 参考 + dropout_db（默认 -20 dB，负值）；连续掉音窗数 >= min_windows
//!   （默认 3）记一个 gap；
//! - 只在**有信号区间**（第一个到最后一个非 null 窗）内找 gap：文件头/尾的静音是闭环
//!   复测协议的前后 1.5 s 静音，属于边界而不是掉音，不计 gap（但电平仍在 levels_dbfs 里）；
//! - gap 起止秒 = 首/末掉音窗的覆盖区间（first*hop/fs 到 (last*hop + window)/fs）。
//!
//! # 边界诚实说明
//!
//! - 窗只有**完全落在掉音段内**才会被判掉音，所以报告的 gap 比真实掉音段**向内收缩**
//!   （最多缩一个窗长/步长）；不会把边界往外扩。
//! - 参考电平取中位数：对闭环复测的 9 s 录音（1.5 s 静音 + 5 s 扫频 + 1.5 s 静音）就是
//!   扫频中位电平；若静音占比超过一半，中位数会被静音拉低（报告里给出
//!   reference_median_dbfs，可人工核对）。
//! - 全部窗都无信号时 verdict = silent、参考电平为 null，不假装通过。
//! - 已知边界：如果掉音紧贴扫频起点/终点、与头尾静音连成一片，会落进边界静音区而**漏检**
//!   （工具不把边界静音当掉音）；这种情况报告里 signal_start_s / signal_end_s 与
//!   levels_dbfs 可用于人工核对。

use serde_json::Value;

use crate::json;
use crate::wav::Wav;
use crate::{codes, AnalyzeError, ENVELOPE_SCHEMA, TOOL_NAME};

/// 默认窗长（毫秒）。
pub const DEFAULT_WINDOW_MS: f64 = 50.0;
/// 默认掉音阈值（相对中位电平，dB）。
pub const DEFAULT_DROPOUT_DB: f64 = -20.0;
/// 默认判定为一个 gap 所需的最少连续掉音窗数。
pub const DEFAULT_MIN_WINDOWS: usize = 3;

/// 包络判定三态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// 未检出达到门限的掉音。
    Clean,
    /// 检出至少一个掉音 gap。
    Dropout,
    /// 全部窗都无信号（参考电平不存在）。
    Silent,
}

impl Verdict {
    /// 报告里的字符串（固定、稳定）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Dropout => "dropout",
            Self::Silent => "silent",
        }
    }
}

/// 包络/掉音检测配置。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnvelopeConfig {
    /// 窗长（毫秒，> 0）。
    pub window_ms: f64,
    /// 掉音阈值（相对中位电平的 dB，必须 < 0）。
    pub dropout_db: f64,
    /// 判定为一个 gap 所需的最少连续掉音窗数（>= 1）。
    pub min_windows: usize,
}

impl Default for EnvelopeConfig {
    fn default() -> Self {
        Self {
            window_ms: DEFAULT_WINDOW_MS,
            dropout_db: DEFAULT_DROPOUT_DB,
            min_windows: DEFAULT_MIN_WINDOWS,
        }
    }
}

/// 一个掉音 gap（起止秒 + 深度）。
#[derive(Clone, Debug, PartialEq)]
pub struct DropoutGap {
    /// 首窗起点（秒）。
    pub start_s: f64,
    /// 末窗终点（秒）。
    pub end_s: f64,
    /// 连续掉音窗数。
    pub windows: usize,
    /// 窗内最差电平（dBFS）；窗全静音时为 None。
    pub worst_dbfs: Option<f64>,
    /// 相对中位电平的深度（dB，负值）；无参考或无有限电平时为 None。
    pub depth_db: Option<f64>,
}

/// 一次包络分析结果。
#[derive(Clone, Debug, PartialEq)]
pub struct EnvelopeReport {
    /// 采样率（Hz）。
    pub sample_rate_hz: f64,
    /// 源 WAV 声道数。
    pub channels: u16,
    /// 样本数（已混单声道）。
    pub samples: usize,
    /// 窗长（毫秒，按样本数反推以保持自洽）。
    pub window_ms: f64,
    /// 窗长（样本）。
    pub window_samples: usize,
    /// 步长（样本，= 窗长/2）。
    pub hop_samples: usize,
    /// 窗数。
    pub windows: usize,
    /// 参考中位电平（dBFS）；全静音时 None。
    pub reference_median_dbfs: Option<f64>,
    /// 掉音阈值（相对中位电平，dB）。
    pub dropout_db: f64,
    /// 最少连续掉音窗数。
    pub min_windows: usize,
    /// 首个有信号窗的起点（秒）；全静音为 None。
    pub signal_start_s: Option<f64>,
    /// 末个有信号窗的终点（秒）；全静音为 None。
    pub signal_end_s: Option<f64>,
    /// 判定。
    pub verdict: Verdict,
    /// 检出的 gap（按时间升序；只含 >= min_windows 的连续段）。
    pub gaps: Vec<DropoutGap>,
    /// 最长 gap 时长（秒）；无 gap 时 None。
    pub max_gap_s: Option<f64>,
    /// 每窗电平（dBFS；整窗全零为 None），下标 i 起点 = i*hop_samples。
    pub levels_dbfs: Vec<Option<f64>>,
}

/// 分析一段 WAV 的短时包络并检测掉音。
pub fn analyze_wav(wav: &Wav, config: &EnvelopeConfig) -> Result<EnvelopeReport, AnalyzeError> {
    if !config.window_ms.is_finite() || config.window_ms <= 0.0 {
        return Err(AnalyzeError::new(
            codes::ANALYZE_CONFIG,
            format!("window_ms {} must be a finite value > 0", config.window_ms),
        ));
    }
    if !config.dropout_db.is_finite() || config.dropout_db >= 0.0 {
        return Err(AnalyzeError::new(
            codes::ANALYZE_CONFIG,
            format!(
                "dropout_db {} must be finite and < 0 (level relative to the median)",
                config.dropout_db
            ),
        ));
    }
    if config.min_windows == 0 {
        return Err(AnalyzeError::new(
            codes::ANALYZE_CONFIG,
            "min_windows must be >= 1",
        ));
    }
    let sample_rate_hz = wav.sample_rate_hz;
    if !sample_rate_hz.is_finite() || sample_rate_hz <= 0.0 {
        return Err(AnalyzeError::new(
            codes::ANALYZE_INPUT,
            format!("sample rate {sample_rate_hz} must be a finite value > 0"),
        ));
    }
    let exact_window = config.window_ms / 1000.0 * sample_rate_hz;
    if exact_window < 2.0 {
        return Err(AnalyzeError::new(
            codes::ANALYZE_CONFIG,
            format!(
                "window_ms {} is shorter than 2 samples at {sample_rate_hz} Hz",
                config.window_ms
            ),
        ));
    }
    if exact_window > wav.samples.len() as f64 {
        return Err(AnalyzeError::new(
            codes::ANALYZE_INPUT,
            format!(
                "recording has {} samples, shorter than one window of {exact_window} samples",
                wav.samples.len()
            ),
        ));
    }
    if let Some(index) = wav.samples.iter().position(|sample| !sample.is_finite()) {
        return Err(AnalyzeError::new(
            codes::ANALYZE_INPUT,
            format!("sample {index} is not finite"),
        ));
    }
    let window_samples = exact_window.round() as usize;
    if window_samples < 2 {
        return Err(AnalyzeError::new(
            codes::ANALYZE_CONFIG,
            format!("window {window_samples} sample is too short"),
        ));
    }
    let hop_samples = window_samples / 2;

    let mut levels = Vec::with_capacity(wav.samples.len() / hop_samples + 1);
    let mut start = 0usize;
    while start + window_samples <= wav.samples.len() {
        let segment = &wav.samples[start..start + window_samples];
        let mean_square =
            segment.iter().map(|&value| value * value).sum::<f64>() / window_samples as f64;
        if !mean_square.is_finite() {
            return Err(AnalyzeError::new(
                codes::ANALYZE_INPUT,
                format!("window at sample {start} has non-finite mean square"),
            ));
        }
        levels.push(if mean_square > 0.0 {
            Some(10.0 * mean_square.log10())
        } else {
            None
        });
        start += hop_samples;
    }
    if levels.is_empty() {
        return Err(AnalyzeError::new(
            codes::ANALYZE_INPUT,
            "no complete analysis window in recording",
        ));
    }

    let reference_median_dbfs = median_of(&levels);
    let threshold = reference_median_dbfs.map(|reference| reference + config.dropout_db);
    let is_dropout = |level: &Option<f64>| -> bool {
        match level {
            Some(level) => match threshold {
                Some(threshold) => *level < threshold,
                None => false,
            },
            None => true,
        }
    };

    // 只判定有信号区间内部的掉音：头尾静音是闭环协议的边界，不是掉音。
    let first_signal_window = levels.iter().position(Option::is_some);
    let last_signal_window = levels.iter().rposition(Option::is_some);
    let (signal_start_s, signal_end_s) = match (first_signal_window, last_signal_window) {
        (Some(first), Some(last)) => (
            Some((first * hop_samples) as f64 / sample_rate_hz),
            Some(((last * hop_samples + window_samples) as f64) / sample_rate_hz),
        ),
        _ => (None, None),
    };
    let mut gaps = Vec::new();
    if let (Some(first), Some(last)) = (first_signal_window, last_signal_window) {
        let mut index = first;
        while index <= last {
            if !is_dropout(&levels[index]) {
                index += 1;
                continue;
            }
            let run_start = index;
            while index <= last && is_dropout(&levels[index]) {
                index += 1;
            }
            let run_windows = index - run_start;
            if run_windows < config.min_windows {
                continue;
            }
            let worst_dbfs = levels[run_start..index]
                .iter()
                .filter_map(|level| *level)
                .fold(None, |worst: Option<f64>, level| {
                    Some(worst.map_or(level, |worst| worst.min(level)))
                });
            gaps.push(DropoutGap {
                start_s: (run_start * hop_samples) as f64 / sample_rate_hz,
                end_s: ((index - 1) * hop_samples + window_samples) as f64 / sample_rate_hz,
                windows: run_windows,
                worst_dbfs,
                depth_db: match (worst_dbfs, reference_median_dbfs) {
                    (Some(worst), Some(reference)) => Some(worst - reference),
                    _ => None,
                },
            });
        }
    }
    let max_gap_s = gaps.iter().map(|gap| gap.end_s - gap.start_s).fold(
        None,
        |longest: Option<f64>, duration| {
            Some(longest.map_or(duration, |longest| longest.max(duration)))
        },
    );
    let verdict = if reference_median_dbfs.is_none() {
        Verdict::Silent
    } else if gaps.is_empty() {
        Verdict::Clean
    } else {
        Verdict::Dropout
    };

    Ok(EnvelopeReport {
        sample_rate_hz,
        channels: wav.channels,
        samples: wav.samples.len(),
        window_ms: window_samples as f64 * 1000.0 / sample_rate_hz,
        window_samples,
        hop_samples,
        windows: levels.len(),
        reference_median_dbfs,
        signal_start_s,
        signal_end_s,
        dropout_db: config.dropout_db,
        min_windows: config.min_windows,
        verdict,
        gaps,
        max_gap_s,
        levels_dbfs: levels,
    })
}

/// envelope 子命令的确定性 JSON 文档。
pub fn document(wav_path: &str, report: &EnvelopeReport) -> Value {
    let gaps: Vec<Value> = report
        .gaps
        .iter()
        .map(|gap| {
            json::object([
                ("start_s", json::number(gap.start_s)),
                ("end_s", json::number(gap.end_s)),
                ("windows", json::count(gap.windows as u64)),
                ("worst_dbfs", json::opt_number(gap.worst_dbfs)),
                ("depth_db", json::opt_number(gap.depth_db)),
            ])
        })
        .collect();
    json::object([
        ("schema", Value::from(ENVELOPE_SCHEMA)),
        ("tool", Value::from(TOOL_NAME)),
        ("tool_version", Value::from(env!("CARGO_PKG_VERSION"))),
        ("command", Value::from("envelope")),
        ("wav", Value::from(wav_path)),
        ("sample_rate_hz", json::number(report.sample_rate_hz)),
        ("channels", json::count(u64::from(report.channels))),
        ("samples", json::count(report.samples as u64)),
        (
            "duration_s",
            json::number(report.samples as f64 / report.sample_rate_hz),
        ),
        ("window", Value::from("rect")),
        ("level_unit", Value::from("dBFS")),
        ("window_ms", json::number(report.window_ms)),
        ("window_samples", json::count(report.window_samples as u64)),
        ("hop_samples", json::count(report.hop_samples as u64)),
        ("windows", json::count(report.windows as u64)),
        (
            "reference_median_dbfs",
            json::opt_number(report.reference_median_dbfs),
        ),
        ("dropout_db", json::number(report.dropout_db)),
        ("min_windows", json::count(report.min_windows as u64)),
        ("signal_start_s", json::opt_number(report.signal_start_s)),
        ("signal_end_s", json::opt_number(report.signal_end_s)),
        ("verdict", Value::from(report.verdict.as_str())),
        ("gap_count", json::count(report.gaps.len() as u64)),
        ("max_gap_s", json::opt_number(report.max_gap_s)),
        ("gaps", Value::Array(gaps)),
        ("levels_dbfs", json::opt_numbers(&report.levels_dbfs)),
    ])
}

/// 中位数（只取有信号的窗；空集返回 None）。
fn median_of(levels: &[Option<f64>]) -> Option<f64> {
    let mut values: Vec<f64> = levels.iter().filter_map(|level| *level).collect();
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        Some(0.5 * (values[middle - 1] + values[middle]))
    } else {
        Some(values[middle])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav_from(samples: Vec<f64>, sample_rate_hz: f64) -> Wav {
        Wav {
            sample_rate_hz,
            channels: 1,
            samples,
        }
    }

    fn sine(freq_hz: f64, amplitude: f64, frames: usize, sample_rate_hz: f64) -> Vec<f64> {
        (0..frames)
            .map(|index| {
                amplitude
                    * (2.0 * std::f64::consts::PI * freq_hz * index as f64 / sample_rate_hz).sin()
            })
            .collect()
    }

    #[test]
    fn median_handles_even_and_odd_and_none() {
        assert_eq!(median_of(&[Some(1.0)]), Some(1.0));
        assert_eq!(median_of(&[Some(4.0), Some(1.0), Some(3.0)]), Some(3.0));
        assert_eq!(
            median_of(&[Some(4.0), Some(1.0), Some(3.0), Some(2.0)]),
            Some(2.5)
        );
        assert_eq!(median_of(&[None, None]), None);
        assert_eq!(median_of(&[None, Some(5.0)]), Some(5.0));
    }

    #[test]
    fn detects_gap_position_and_depth() {
        let sample_rate_hz = 48_000.0;
        let mut samples = sine(1_000.0, 0.5, 96_000, sample_rate_hz);
        // 0.8 s - 1.0 s 注入 -60 dBFS 级别的低电平（模拟真实掉音的本底，不是数字零）。
        for sample in samples.iter_mut().skip(38_400).take(9_600) {
            *sample = 0.001 * (*sample).signum().max(0.0);
        }
        let report = analyze_wav(
            &wav_from(samples, sample_rate_hz),
            &EnvelopeConfig::default(),
        )
        .expect("envelope");
        assert_eq!(report.verdict, Verdict::Dropout);
        assert_eq!(report.gaps.len(), 1);
        let gap = &report.gaps[0];
        assert!(gap.start_s >= 0.8 - 1e-9 && gap.start_s <= 0.8 + 0.026);
        assert!(gap.end_s <= 1.0 + 1e-9 && gap.end_s >= 1.0 - 0.051);
        let depth = gap.depth_db.expect("depth");
        assert!(depth < -20.0, "depth {depth}");
    }

    #[test]
    fn disappears_when_below_min_windows() {
        let sample_rate_hz = 48_000.0;
        let mut samples = sine(1_000.0, 0.5, 96_000, sample_rate_hz);
        for sample in samples.iter_mut().skip(38_400).take(2_400) {
            *sample = 0.0;
        }
        let config = EnvelopeConfig {
            min_windows: 10,
            ..EnvelopeConfig::default()
        };
        let report = analyze_wav(&wav_from(samples, sample_rate_hz), &config).expect("envelope");
        assert_eq!(report.verdict, Verdict::Clean);
        assert!(report.gaps.is_empty());
    }

    #[test]
    fn all_silent_marks_silent_not_clean() {
        let report = analyze_wav(
            &wav_from(vec![0.0; 48_000], 48_000.0),
            &EnvelopeConfig::default(),
        )
        .expect("envelope");
        assert_eq!(report.verdict, Verdict::Silent);
        assert!(report.reference_median_dbfs.is_none());
        assert!(report.gaps.is_empty());
        assert!(report.signal_start_s.is_none());
        assert!(report.levels_dbfs.iter().all(Option::is_none));
    }

    #[test]
    fn config_validation_uses_stable_codes() {
        let wav = wav_from(sine(1_000.0, 0.5, 48_000, 48_000.0), 48_000.0);
        for config in [
            EnvelopeConfig {
                window_ms: 0.0,
                ..EnvelopeConfig::default()
            },
            EnvelopeConfig {
                dropout_db: 0.0,
                ..EnvelopeConfig::default()
            },
            EnvelopeConfig {
                min_windows: 0,
                ..EnvelopeConfig::default()
            },
        ] {
            let error = analyze_wav(&wav, &config).expect_err("invalid config");
            assert_eq!(error.code(), codes::ANALYZE_CONFIG);
        }
        let error = analyze_wav(
            &wav,
            &EnvelopeConfig {
                window_ms: 5_000.0,
                ..EnvelopeConfig::default()
            },
        )
        .expect_err("window longer than recording");
        assert_eq!(error.code(), codes::ANALYZE_INPUT);
    }
}
