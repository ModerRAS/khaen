//! 逐带 SNR 诊断：把一段录音切成信号段/噪声段，分别算 1/3 oct 带电平，给出每带 SNR
//! 与是否可测标记。
//!
//! 背景（2026-11-26 真机闭环复测）：30 带判定在 25/31.5/50/63 Hz 超差，离线诊断发现
//! baseline 录音在 25/31.5 Hz 的 SNR 只有约 0/1 dB —— 这两带里没有可测信号、只有噪声，
//! 拿它去和施加后的信号相除得到的 delta 不是传递函数。khaen-band-levels /
//! khaen-closed-loop-run 目前只有带功率为 0 记 missing 的机制，没有逐带 SNR 门，
//! 因此会把不可测带当可比较带判定。本命令把哪一带可测、哪一带不可测变成显式证据，
//! 供人工/交接判定使用；它不改任何既有判定工具的口径。
//!
//! # 算法（确定性）
//!
//! 1. 用 envelope 模块（同一份活动区逻辑）找 signal_start_s / signal_end_s；
//! 2. 噪声段：信号段之前的 [margin, signal_start - margin]，长度 >= min_noise_s；
//!    信号从录音开头就活动时，退回信号段之后的 [signal_end + margin, duration - margin]；
//!    两段都不够 -> E_ANALYZE_INPUT（不猜噪声）；
//! 3. 信号段 = [signal_start_s, signal_end_s]；
//! 4. 两段分别调用 spectrum::band_levels（与 spectrum/compare 同一份带功率实现），
//!    SNR = 信号带电平 - 噪声带电平；
//! 5. measurable = SNR 有限且 >= min_snr_db（默认 10 dB）。任一侧带功率为 0 时该带记 null
//!    （缺数据，不算 measurable）。
//!
//! 只读 WAV、只写 JSON；不碰设备。

use crate::envelope::{self, EnvelopeConfig};
use crate::json;
use crate::spectrum;
use crate::wav::Wav;
use crate::{codes, AnalyzeError, BAND_SNR_SCHEMA, TOOL_NAME};

/// 默认噪声段与信号段之间的留白（秒）。
pub const DEFAULT_NOISE_MARGIN_S: f64 = 0.100;
/// 噪声段最短时长（秒）。
pub const DEFAULT_MIN_NOISE_S: f64 = 0.300;
/// 默认可测门限（dB）。
pub const DEFAULT_MIN_SNR_DB: f64 = 10.0;
/// 默认带功率窗长（与 khaen-band-levels 默认一致）。
pub const DEFAULT_WINDOW_LEN: usize = khaen_calibration::band_levels::DEFAULT_WINDOW_LEN;

/// 逐带 SNR 配置。
#[derive(Clone, Debug, PartialEq)]
pub struct BandSnrConfig {
    /// 带功率窗长（样本，2 的幂）。
    pub window_len: usize,
    /// 1/3 oct 中心频率（Hz，升序）。
    pub centers_hz: Vec<f64>,
    /// 包络窗长（毫秒）。
    pub envelope_window_ms: f64,
    /// 噪声段最短时长（秒）。
    pub min_noise_s: f64,
    /// 噪声段与信号段之间的留白（秒）。
    pub noise_margin_s: f64,
    /// 可测门限（dB）。
    pub min_snr_db: f64,
}

impl Default for BandSnrConfig {
    fn default() -> Self {
        Self {
            window_len: DEFAULT_WINDOW_LEN,
            centers_hz: khaen_calibration::plan_levels::DEFAULT_CENTERS_HZ.to_vec(),
            envelope_window_ms: envelope::DEFAULT_WINDOW_MS,
            min_noise_s: DEFAULT_MIN_NOISE_S,
            noise_margin_s: DEFAULT_NOISE_MARGIN_S,
            min_snr_db: DEFAULT_MIN_SNR_DB,
        }
    }
}

/// 单带 SNR 结果。
#[derive(Clone, Debug, PartialEq)]
pub struct BandSnr {
    /// 中心频率（Hz）。
    pub center_hz: f64,
    /// 信号段带电平（dBFS）；无信号侧带功率时为 None。
    pub signal_dbfs: Option<f64>,
    /// 噪声段带电平（dBFS）；无噪声侧带功率时为 None。
    pub noise_dbfs: Option<f64>,
    /// SNR（dB）；任一侧 None 时为 None。
    pub snr_db: Option<f64>,
    /// 是否达到可测门限；SNR 为 None 时也是 None（缺数据，不冒充可测/不可测）。
    pub measurable: Option<bool>,
}

/// 逐带 SNR 报告。
#[derive(Clone, Debug, PartialEq)]
pub struct BandSnrReport {
    /// 采样率（Hz）。
    pub sample_rate_hz: f64,
    /// 源 WAV 声道数。
    pub channels: u16,
    /// 录音总时长（秒）。
    pub duration_s: f64,
    /// 包络窗长（毫秒）。
    pub envelope_window_ms: f64,
    /// 信号段起点（秒）。
    pub signal_start_s: f64,
    /// 信号段终点（秒）。
    pub signal_end_s: f64,
    /// 噪声段起点（秒）。
    pub noise_start_s: f64,
    /// 噪声段终点（秒）。
    pub noise_end_s: f64,
    /// 可测门限（dB）。
    pub min_snr_db: f64,
    /// 逐带结果（中心频率升序）。
    pub bands: Vec<BandSnr>,
}

impl BandSnrReport {
    /// 有 SNR 的带数（可比较）。
    pub fn comparable_bands(&self) -> usize {
        self.bands
            .iter()
            .filter(|band| band.snr_db.is_some())
            .count()
    }

    /// 达到门限的带数。
    pub fn measurable_bands(&self) -> usize {
        self.bands
            .iter()
            .filter(|band| band.measurable == Some(true))
            .count()
    }

    /// 低于门限的带数（SNR 有限但不够）。
    pub fn unmeasurable_bands(&self) -> usize {
        self.bands
            .iter()
            .filter(|band| band.measurable == Some(false))
            .count()
    }

    /// 任一侧缺数据的带数。
    pub fn missing_bands(&self) -> usize {
        self.bands
            .iter()
            .filter(|band| band.snr_db.is_none())
            .count()
    }
}

/// 分析一段 WAV 的逐带 SNR。
pub fn analyze_wav(wav: &Wav, config: &BandSnrConfig) -> Result<BandSnrReport, AnalyzeError> {
    validate(config)?;
    let duration_s = wav.samples.len() as f64 / wav.sample_rate_hz;
    if wav.samples.is_empty() || !wav.samples.iter().all(|sample| sample.is_finite()) {
        return Err(AnalyzeError::new(
            codes::ANALYZE_INPUT,
            "WAV samples must be non-empty and finite",
        ));
    }
    let envelope_config = EnvelopeConfig {
        window_ms: config.envelope_window_ms,
        ..EnvelopeConfig::default()
    };
    let envelope_report = envelope::analyze_wav(wav, &envelope_config)?;
    let Some(signal_start_s) = envelope_report.signal_start_s else {
        return Err(AnalyzeError::new(
            codes::ANALYZE_INPUT,
            "recording has no active signal region: envelope verdict is silent, so no band SNR can be estimated",
        ));
    };
    let Some(signal_end_s) = envelope_report.signal_end_s else {
        return Err(AnalyzeError::new(
            codes::ANALYZE_INPUT,
            "recording has no active signal end: envelope report is inconsistent",
        ));
    };
    let margin = config.noise_margin_s;
    let before_start = margin;
    let before_end = signal_start_s - margin;
    let after_start = signal_end_s + margin;
    let after_end = duration_s - margin;
    let (noise_start_s, noise_end_s) = if before_end - before_start >= config.min_noise_s {
        (before_start, before_end)
    } else if after_end - after_start >= config.min_noise_s {
        (after_start, after_end)
    } else {
        return Err(AnalyzeError::new(
            codes::ANALYZE_INPUT,
            format!(
                "recording has no quiet region of at least {:.3} s before or after the active signal (start {:.3} s, end {:.3} s, duration {:.3} s); cannot estimate a noise floor",
                config.min_noise_s, signal_start_s, signal_end_s, duration_s
            ),
        ));
    };
    let noise_samples = slice(wav, noise_start_s, noise_end_s)?;
    let signal_samples = slice(wav, signal_start_s, signal_end_s)?;
    if noise_samples.len() < config.window_len || signal_samples.len() < config.window_len {
        return Err(AnalyzeError::new(
            codes::ANALYZE_INPUT,
            format!(
                "noise or signal segment is shorter than one analysis window ({} samples)",
                config.window_len
            ),
        ));
    }
    let noise_levels = spectrum::band_levels(
        noise_samples,
        wav.sample_rate_hz,
        &config.centers_hz,
        config.window_len,
    )?;
    let signal_levels = spectrum::band_levels(
        signal_samples,
        wav.sample_rate_hz,
        &config.centers_hz,
        config.window_len,
    )?;
    let mut bands = Vec::with_capacity(config.centers_hz.len());
    for (noise, signal) in noise_levels.iter().zip(signal_levels.iter()) {
        let snr_db = match (signal.level_db, noise.level_db) {
            (Some(signal_db), Some(noise_db)) => Some(signal_db - noise_db),
            _ => None,
        };
        let measurable = snr_db.map(|snr| snr >= config.min_snr_db);
        bands.push(BandSnr {
            center_hz: signal.center_hz,
            signal_dbfs: signal.level_db,
            noise_dbfs: noise.level_db,
            snr_db,
            measurable,
        });
    }
    Ok(BandSnrReport {
        sample_rate_hz: wav.sample_rate_hz,
        channels: wav.channels,
        duration_s,
        envelope_window_ms: config.envelope_window_ms,
        signal_start_s,
        signal_end_s,
        noise_start_s,
        noise_end_s,
        min_snr_db: config.min_snr_db,
        bands,
    })
}

fn validate(config: &BandSnrConfig) -> Result<(), AnalyzeError> {
    if !config.envelope_window_ms.is_finite() || config.envelope_window_ms <= 0.0 {
        return Err(AnalyzeError::new(
            codes::ANALYZE_CONFIG,
            format!(
                "envelope window_ms {} must be a finite value > 0",
                config.envelope_window_ms
            ),
        ));
    }
    if !config.min_noise_s.is_finite() || config.min_noise_s <= 0.0 {
        return Err(AnalyzeError::new(
            codes::ANALYZE_CONFIG,
            format!(
                "min_noise_s {} must be a finite value > 0",
                config.min_noise_s
            ),
        ));
    }
    if !config.noise_margin_s.is_finite() || config.noise_margin_s < 0.0 {
        return Err(AnalyzeError::new(
            codes::ANALYZE_CONFIG,
            format!(
                "noise_margin_s {} must be a finite value >= 0",
                config.noise_margin_s
            ),
        ));
    }
    if !config.min_snr_db.is_finite() {
        return Err(AnalyzeError::new(
            codes::ANALYZE_CONFIG,
            "min_snr_db must be finite",
        ));
    }
    if config.centers_hz.is_empty() {
        return Err(AnalyzeError::new(
            codes::ANALYZE_CONFIG,
            "centers_hz must not be empty",
        ));
    }
    Ok(())
}

fn slice(wav: &Wav, start_s: f64, end_s: f64) -> Result<&[f64], AnalyzeError> {
    let start = (start_s * wav.sample_rate_hz).round() as usize;
    let end = (end_s * wav.sample_rate_hz).round() as usize;
    if end <= start || start >= wav.samples.len() {
        return Err(AnalyzeError::new(
            codes::ANALYZE_INPUT,
            format!("slice {start_s:.3}..{end_s:.3} s is empty"),
        ));
    }
    let end = end.min(wav.samples.len());
    Ok(&wav.samples[start..end])
}

/// 报告 JSON（键字典序；数值只来自输入 + 固定配置）。
pub fn document(wav_path: &str, report: &BandSnrReport) -> serde_json::Value {
    let bands = report
        .bands
        .iter()
        .map(|band| {
            json::object([
                ("center_hz", json::number(band.center_hz)),
                (
                    "measurable",
                    match band.measurable {
                        Some(true) => serde_json::Value::from(true),
                        Some(false) => serde_json::Value::from(false),
                        None => serde_json::Value::Null,
                    },
                ),
                ("noise_dbfs", json::opt_number(band.noise_dbfs)),
                ("signal_dbfs", json::opt_number(band.signal_dbfs)),
                ("snr_db", json::opt_number(band.snr_db)),
            ])
        })
        .collect::<Vec<_>>();
    json::object([
        ("bands", serde_json::Value::Array(bands)),
        ("channels", json::count(u64::from(report.channels))),
        ("command", serde_json::Value::from("band-snr")),
        ("duration_s", json::number(report.duration_s)),
        (
            "envelope_window_ms",
            json::number(report.envelope_window_ms),
        ),
        ("min_snr_db", json::number(report.min_snr_db)),
        ("noise_end_s", json::number(report.noise_end_s)),
        ("noise_start_s", json::number(report.noise_start_s)),
        ("sample_rate_hz", json::number(report.sample_rate_hz)),
        ("schema", serde_json::Value::from(BAND_SNR_SCHEMA)),
        ("signal_end_s", json::number(report.signal_end_s)),
        ("signal_start_s", json::number(report.signal_start_s)),
        ("tool", serde_json::Value::from(TOOL_NAME)),
        (
            "tool_version",
            serde_json::Value::from(env!("CARGO_PKG_VERSION")),
        ),
        ("wav", serde_json::Value::from(wav_path)),
    ])
}
