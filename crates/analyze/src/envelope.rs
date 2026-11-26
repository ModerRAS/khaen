//! 短时 RMS 包络与掉音（dropout）检测。
//!
//! 背景：2026-11-26 真机闭环复测首次基线录音在 4.3-4.9 s 中途掉音（覆盖扫频 630-1000 Hz），
//! 30 带全频判定 FAIL（最大 delta 30.7 dB）；该录音作废重录后才回到 +-0.5 dB。本模块把这类
//! 问题变成复测前的自动检查：先 envelope 确认无掉音，再做 khaen-band-levels /
//! khaen-closed-loop-run 判定。
//!
//! # 算法（确定性、无状态）
//!
//! 1. **短时电平**：矩形窗（不加重叠窗函数），窗长 window_ms（默认 50 ms），步长 = 窗长/2；
//!    每窗电平 20*log10(rms) dBFS（满幅幅度 1.0 = 0 dBFS 峰值；满幅正弦读 -3.01 dBFS）；
//!    整窗全零的窗记 null（理论 -inf dB，JSON 不编大负数）。
//! 2. **活动区（signal region）**：真机录音的“静音”不是数字零，而是约 -70 dBFS 的本底噪声；
//!    若直接对全体窗取中位数，静音占比 >50% 时参考会落在静音上（T63 复查发现的漏检根因）。
//!    因此先估计底噪并划出活动区：
//!    - 底噪 = 全窗电平的 p10（最近秩；null 窗按 -inf 计入）；
//!    - 峰值 = 最大窗电平；若 峰值 - 底噪 <= 10 dB，认为没有明显静音段，全部有信号窗都算活动；
//!    - 否则活动阈值 = max(峰值 - 40 dB, 峰值与底噪的 dB 中点)，高于阈值的窗才算活动；
//!    - signal_start_s / signal_end_s = 首/末活动窗的覆盖区间（边界静音不算活动区）。
//! 3. **参考电平** = 活动窗（不是全体窗、也不是活动区内的所有窗）的电平中位数。
//! 4. **掉音** = 活动区内电平 < 参考 + dropout_db（默认 -20 dB，负值）的窗；连续掉音窗数
//!    >= min_windows（默认 3）记一个 gap；越出活动区的头尾静音不算 gap。
//! 5. **gap 起止秒** = 首/末掉音窗的覆盖区间（first*hop/fs 到 (last*hop + window)/fs）。
//!
//! # 边界诚实说明
//!
//! - 窗只有**完全落在掉音段内**才会被判掉音，所以报告的 gap 比真实掉音段**向内收缩**
//!   （最多缩一个窗长/步长）；不会把边界往外扩。
//! - 活动区需要“底噪明显低于信号”才可分辨：峰值与底噪差 <= 10 dB 时不做区分（全部窗活动）；
//!   若底噪只比信号低十几 dB，按中点阈值仍可能把部分安静信号窗算进活动区/参考。
//! - 若掉音紧贴扫频起点/终点、与头尾静音连成一片，会落进边界静音区而**漏检**；用
//!   signal_start_s / signal_end_s 与 levels_dbfs 人工核对。
//! - 全程无任何有信号窗时 verdict = silent、参考电平为 null，不假装通过。

use serde_json::Value;

use crate::json;
use crate::wav::Wav;
use crate::{codes, AnalyzeError, ENVELOPE_SCHEMA, TOOL_NAME};

/// 默认窗长（毫秒）。
pub const DEFAULT_WINDOW_MS: f64 = 50.0;
/// 默认掉音阈值（相对活动区中位电平，dB）。
pub const DEFAULT_DROPOUT_DB: f64 = -20.0;
/// 默认判定为一个 gap 所需的最少连续掉音窗数。
pub const DEFAULT_MIN_WINDOWS: usize = 3;
/// 活动区：窗电平至少比峰值低这么多 dB 才算活动（峰值 - 40 dB 的上界）。
pub const ACTIVE_RANGE_DB: f64 = 40.0;
/// 活动区：峰值与估计底噪差 <= 此值视为没有明显静音段（全部有信号窗都算活动）。
pub const ACTIVE_MIN_SPAN_DB: f64 = 10.0;
/// 底噪估计分位（最近秩，0..1；全零窗按 -inf 计入）。
pub const NOISE_FLOOR_PERCENTILE: f64 = 0.10;

/// 包络判定三态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// 活动区内未检出达到门限的掉音。
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
    /// 掉音阈值（相对活动区中位电平的 dB，必须 < 0）。
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
    /// 相对活动区中位电平的深度（dB，负值）；无参考或无有限电平时为 None。
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
    /// 估计底噪（dBFS，p10；全零窗按 -inf 计，估不出时为 None）。
    pub noise_floor_dbfs: Option<f64>,
    /// 活动阈值（dBFS）；无明显静音段、全部窗活动时为 None。
    pub active_threshold_dbfs: Option<f64>,
    /// 活动窗数。
    pub active_windows: usize,
    /// 活动区中位电平（dBFS）；无活动窗时 None。
    pub reference_median_dbfs: Option<f64>,
    /// 首个活动窗的起点（秒）；无活动窗为 None。
    pub signal_start_s: Option<f64>,
    /// 末个活动窗的终点（秒）；无活动窗为 None。
    pub signal_end_s: Option<f64>,
    /// 掉音阈值（相对活动区中位电平，dB）。
    pub dropout_db: f64,
    /// 最少连续掉音窗数。
    pub min_windows: usize,
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
                "dropout_db {} must be finite and < 0 (level relative to the active-region median)",
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

    let evaluation = evaluate_levels(&levels, sample_rate_hz, hop_samples, window_samples, config);
    Ok(EnvelopeReport {
        sample_rate_hz,
        channels: wav.channels,
        samples: wav.samples.len(),
        window_ms: window_samples as f64 * 1000.0 / sample_rate_hz,
        window_samples,
        hop_samples,
        windows: levels.len(),
        noise_floor_dbfs: evaluation.noise_floor_dbfs,
        active_threshold_dbfs: evaluation.active_threshold_dbfs,
        active_windows: evaluation.active_windows,
        reference_median_dbfs: evaluation.reference_median_dbfs,
        signal_start_s: evaluation.signal_start_s,
        signal_end_s: evaluation.signal_end_s,
        dropout_db: config.dropout_db,
        min_windows: config.min_windows,
        verdict: evaluation.verdict,
        gaps: evaluation.gaps,
        max_gap_s: evaluation.max_gap_s,
        levels_dbfs: levels,
    })
}

/// 活动区 + 掉音判定的中间结果（analyze_wav 与单元测试共用）。
struct EnvelopeEvaluation {
    noise_floor_dbfs: Option<f64>,
    active_threshold_dbfs: Option<f64>,
    active_windows: usize,
    reference_median_dbfs: Option<f64>,
    signal_start_s: Option<f64>,
    signal_end_s: Option<f64>,
    verdict: Verdict,
    gaps: Vec<DropoutGap>,
    max_gap_s: Option<f64>,
}

fn evaluate_levels(
    levels: &[Option<f64>],
    sample_rate_hz: f64,
    hop_samples: usize,
    window_samples: usize,
    config: &EnvelopeConfig,
) -> EnvelopeEvaluation {
    let finite_count = levels.iter().filter(|level| level.is_some()).count();
    if finite_count == 0 {
        return EnvelopeEvaluation {
            noise_floor_dbfs: None,
            active_threshold_dbfs: None,
            active_windows: 0,
            reference_median_dbfs: None,
            signal_start_s: None,
            signal_end_s: None,
            verdict: Verdict::Silent,
            gaps: Vec::new(),
            max_gap_s: None,
        };
    }
    let peak = levels
        .iter()
        .filter_map(|level| *level)
        .fold(f64::NEG_INFINITY, f64::max);
    let floor = noise_floor_dbfs(levels);
    let span = peak - floor;
    let active_threshold = if span <= ACTIVE_MIN_SPAN_DB {
        f64::NEG_INFINITY
    } else {
        (peak - ACTIVE_RANGE_DB).max((peak + floor) / 2.0)
    };
    let is_active =
        |level: &Option<f64>| -> bool { level.is_some_and(|value| value >= active_threshold) };
    let first_active = levels.iter().position(is_active);
    let last_active = levels.iter().rposition(is_active);
    let active_levels: Vec<f64> = levels
        .iter()
        .filter(|level| is_active(level))
        .filter_map(|level| *level)
        .collect();
    let reference_median_dbfs = median_values(&active_levels);
    let (signal_start_s, signal_end_s) = match (first_active, last_active) {
        (Some(first), Some(last)) => (
            Some((first * hop_samples) as f64 / sample_rate_hz),
            Some(((last * hop_samples + window_samples) as f64) / sample_rate_hz),
        ),
        _ => (None, None),
    };

    let mut gaps = Vec::new();
    if let (Some(first), Some(last), Some(reference)) =
        (first_active, last_active, reference_median_dbfs)
    {
        let dropout_threshold = reference + config.dropout_db;
        let is_dropout = |level: &Option<f64>| -> bool {
            match level {
                Some(value) => *value < dropout_threshold,
                None => true,
            }
        };
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
    let verdict = if gaps.is_empty() {
        Verdict::Clean
    } else {
        Verdict::Dropout
    };
    EnvelopeEvaluation {
        noise_floor_dbfs: floor.is_finite().then_some(floor),
        active_threshold_dbfs: active_threshold.is_finite().then_some(active_threshold),
        active_windows: active_levels.len(),
        reference_median_dbfs,
        signal_start_s,
        signal_end_s,
        verdict,
        gaps,
        max_gap_s,
    }
}

/// 底噪估计：全窗电平（null 按 -inf 计）的 p10（最近秩）。
fn noise_floor_dbfs(levels: &[Option<f64>]) -> f64 {
    let mut values: Vec<f64> = levels
        .iter()
        .map(|level| level.unwrap_or(f64::NEG_INFINITY))
        .collect();
    if values.is_empty() {
        return f64::NEG_INFINITY;
    }
    values.sort_by(f64::total_cmp);
    let last = values.len() - 1;
    let index = ((last as f64) * NOISE_FLOOR_PERCENTILE).round() as usize;
    values[index.min(last)]
}

/// 中位数（空集返回 None）。
fn median_values(values: &[f64]) -> Option<f64> {
    let mut sorted = values.to_vec();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        Some(0.5 * (sorted[middle - 1] + sorted[middle]))
    } else {
        Some(sorted[middle])
    }
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
            "noise_floor_dbfs",
            json::opt_number(report.noise_floor_dbfs),
        ),
        (
            "active_threshold_dbfs",
            json::opt_number(report.active_threshold_dbfs),
        ),
        ("active_windows", json::count(report.active_windows as u64)),
        (
            "reference_median_dbfs",
            json::opt_number(report.reference_median_dbfs),
        ),
        ("signal_start_s", json::opt_number(report.signal_start_s)),
        ("signal_end_s", json::opt_number(report.signal_end_s)),
        ("dropout_db", json::number(report.dropout_db)),
        ("min_windows", json::count(report.min_windows as u64)),
        ("verdict", Value::from(report.verdict.as_str())),
        ("gap_count", json::count(report.gaps.len() as u64)),
        ("max_gap_s", json::opt_number(report.max_gap_s)),
        ("gaps", Value::Array(gaps)),
        ("levels_dbfs", json::opt_numbers(&report.levels_dbfs)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq_hz: f64, amplitude: f64, frames: usize, sample_rate_hz: f64) -> Vec<f64> {
        (0..frames)
            .map(|index| {
                amplitude
                    * (2.0 * std::f64::consts::PI * freq_hz * index as f64 / sample_rate_hz).sin()
            })
            .collect()
    }

    fn wav_from(samples: Vec<f64>, sample_rate_hz: f64) -> Wav {
        Wav {
            sample_rate_hz,
            channels: 1,
            samples,
        }
    }

    fn evaluate(levels: &[Option<f64>]) -> EnvelopeEvaluation {
        evaluate_levels(levels, 48_000.0, 1_200, 2_400, &EnvelopeConfig::default())
    }

    #[test]
    fn majority_silence_uses_active_region_reference() {
        // 60 窗 -70 dB 本底 + 40 窗 -15 dB 扫频（静音 60%），扫频中间 5 窗掉到 -72 dB。
        let mut levels = vec![Some(-70.0); 60];
        levels.extend(vec![Some(-15.0); 40]);
        for level in levels.iter_mut().take(80).skip(75) {
            *level = Some(-72.0);
        }
        let evaluation = evaluate(&levels);
        assert!((evaluation.noise_floor_dbfs.expect("floor") - -70.0).abs() < 1e-9);
        let threshold = evaluation.active_threshold_dbfs.expect("threshold");
        assert!((threshold - -42.5).abs() < 1e-9, "threshold {threshold}");
        // 掉音的 5 个窗不算活动：40 - 5 = 35。
        assert_eq!(evaluation.active_windows, 35);
        assert!((evaluation.reference_median_dbfs.expect("reference") - -15.0).abs() < 1e-9);
        assert_eq!(evaluation.verdict, Verdict::Dropout);
        assert_eq!(evaluation.gaps.len(), 1);
        let gap = &evaluation.gaps[0];
        assert_eq!(gap.windows, 5);
        assert!((gap.depth_db.expect("depth") - -57.0).abs() < 1e-9);
        // 活动区 = 窗 60..99（掉音窗嵌在中间）。
        assert!((evaluation.signal_start_s.expect("start") - 1.5).abs() < 1e-9);
        assert!(
            (evaluation.signal_end_s.expect("end") - (99.0 * 1200.0 + 2400.0) / 48_000.0).abs()
                < 1e-9
        );
    }

    #[test]
    fn continuous_signal_marks_all_windows_active() {
        // 峰值与底噪几乎一样（无静音段）时不做活动区分，参考 = 全体中位。
        let levels: Vec<Option<f64>> = (0..60).map(|_| Some(-15.0)).collect();
        let evaluation = evaluate(&levels);
        assert!(evaluation.active_threshold_dbfs.is_none());
        assert_eq!(evaluation.active_windows, 60);
        assert!((evaluation.reference_median_dbfs.expect("reference") - -15.0).abs() < 1e-9);
        assert_eq!(evaluation.verdict, Verdict::Clean);
    }

    #[test]
    fn all_silent_is_silent_not_clean() {
        let levels = vec![None; 40];
        let evaluation = evaluate(&levels);
        assert_eq!(evaluation.verdict, Verdict::Silent);
        assert!(evaluation.reference_median_dbfs.is_none());
        assert_eq!(evaluation.active_windows, 0);
        assert!(evaluation.gaps.is_empty());
    }

    #[test]
    fn median_handles_even_and_odd_and_empty() {
        assert_eq!(median_values(&[]), None);
        assert_eq!(median_values(&[1.0]), Some(1.0));
        assert_eq!(median_values(&[4.0, 1.0, 3.0]), Some(3.0));
        assert_eq!(median_values(&[4.0, 1.0, 3.0, 2.0]), Some(2.5));
    }

    #[test]
    fn detects_gap_position_and_depth() {
        let sample_rate_hz = 48_000.0;
        let mut samples = sine(1_000.0, 0.5, 96_000, sample_rate_hz);
        // 0.8 s - 1.0 s 注入 -63 dBFS 级别的低电平（模拟真实掉音的本底，不是数字零）。
        for sample in samples.iter_mut().skip(38_400).take(9_600) {
            *sample = 0.0007;
        }
        let report = analyze_wav(
            &wav_from(samples, sample_rate_hz),
            &EnvelopeConfig::default(),
        )
        .expect("envelope");
        assert_eq!(report.verdict, Verdict::Dropout);
        assert_eq!(report.gaps.len(), 1);
        let gap = &report.gaps[0];
        assert!(
            (0.8 - 1e-9..=0.8 + 0.026).contains(&gap.start_s),
            "start {}",
            gap.start_s
        );
        assert!(
            (1.0 - 0.051..=1.0 + 1e-9).contains(&gap.end_s),
            "end {}",
            gap.end_s
        );
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
    fn all_silent_recording_marks_silent_not_clean() {
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
