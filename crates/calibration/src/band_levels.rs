//! 实测带域电平：把一段录音切成 Hann 窗做 Welch 平均频谱，按 1/3 oct 带积分成功率，
//! 再与参考录音逐带相除得到**相对电平**（dB）。
//!
//! # 为什么是相对电平
//!
//! 闭环复测的 predicted 侧是"方案相对无滤波的频响"（见 crate::plan_levels）。实测侧
//! 只有用同一参考归一化（施加方案后的录音 / 无方案基线的录音）才能逐带对账；绝对声压级
//! 需要麦克风灵敏度与声学校准，不在本工具范围内，也不假装有。
//!
//! # 算法（确定性）
//!
//! - 窗长默认 4096（2 的幂，可用配置改），Hann 窗，步长 = 窗长 / 2；
//! - 每段加窗后做实数 FFT，单边功率谱按窗能量归一化：
//!   `P[k] = c(k) * |X[k]|^2 / (N * sum_n w[n]^2)`，其中 k = 0 与 k = N/2 时 c = 1，其余 c = 2；
//! - 频带 [c * 2^(-1/6), c * 2^(+1/6)) 内的 bin 功率求和，再对全部分段取平均（功率域平均）；
//! - 相对电平 = 10 * log10(实测带功率 / 参考带功率)；任一侧带功率为 0 时记 None
//!   （缺数据，闭环判定算 missing，绝不算通过）。
//!
//! 本模块不读写文件、不碰设备：WAV 解析在命令行工具里，设备采集是 T42 真机流程（NOT_RUN）。

use crate::closed_loop::BandLevel;
use crate::microfft::rfft;

/// 默认窗长（样本，2 的幂）。
pub const DEFAULT_WINDOW_LEN: usize = 4096;
/// 窗长下限。
pub const MIN_WINDOW_LEN: usize = 256;
/// 窗长上限。
pub const MAX_WINDOW_LEN: usize = 65_536;

/// 输入错误：样本为空/非有限、录音长度不足、WAV 内容不合法。
pub const BAD_INPUT: &str = "E_BAND_LEVELS_INPUT";
/// 配置错误：窗长非法。
pub const CONFIG: &str = "E_BAND_LEVELS_CONFIG";
/// 对齐错误：采样率非法、频带列表非法。
pub const ALIGNMENT: &str = "E_BAND_LEVELS_ALIGNMENT";

/// 带域电平错误（稳定码 + 人读消息）。
#[derive(Clone, Debug, PartialEq)]
pub struct BandLevelsError {
    code: &'static str,
    message: String,
}

impl BandLevelsError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 稳定错误码。
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// 人读消息。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for BandLevelsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for BandLevelsError {}

/// 带域电平分析配置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BandLevelsConfig {
    /// 每段 FFT 窗长（样本，2 的幂，256..=65536）；步长固定为一半。
    pub window_len: usize,
}

impl Default for BandLevelsConfig {
    fn default() -> Self {
        Self {
            window_len: DEFAULT_WINDOW_LEN,
        }
    }
}

impl BandLevelsConfig {
    fn validate(&self) -> Result<(), BandLevelsError> {
        if !self.window_len.is_power_of_two() {
            return Err(BandLevelsError::new(
                CONFIG,
                format!("window_len {} is not a power of two", self.window_len),
            ));
        }
        if !(MIN_WINDOW_LEN..=MAX_WINDOW_LEN).contains(&self.window_len) {
            return Err(BandLevelsError::new(
                CONFIG,
                format!(
                    "window_len {} is outside [{MIN_WINDOW_LEN}, {MAX_WINDOW_LEN}]",
                    self.window_len
                ),
            ));
        }
        Ok(())
    }
}

/// 实测录音 vs 参考录音的逐带相对电平（dB），中心频率升序。
///
/// 两段录音采样率必须一致；频带中心必须非空、有限、严格升序、低于 Nyquist。
/// 任一侧带功率为 0 的带记 `level_db: None`（缺数据）。
pub fn relative_band_levels(
    measured: &[f64],
    reference: &[f64],
    sample_rate_hz: f64,
    centers_hz: &[f64],
    config: &BandLevelsConfig,
) -> Result<Vec<BandLevel>, BandLevelsError> {
    config.validate()?;
    validate_sample_rate(sample_rate_hz)?;
    validate_centers(centers_hz, sample_rate_hz)?;
    let measured_power = band_powers(measured, sample_rate_hz, centers_hz, config)?;
    let reference_power = band_powers(reference, sample_rate_hz, centers_hz, config)?;
    let mut levels = Vec::with_capacity(centers_hz.len());
    for ((&center_hz, &measured_power), &reference_power) in centers_hz
        .iter()
        .zip(measured_power.iter())
        .zip(reference_power.iter())
    {
        let level_db = if measured_power > 0.0 && reference_power > 0.0 {
            Some(10.0 * (measured_power / reference_power).log10())
        } else {
            None
        };
        levels.push(BandLevel {
            center_hz,
            level_db,
        });
    }
    Ok(levels)
}

/// 单段录音的逐带功率（均值，功率域）；供 [`relative_band_levels`] 与诊断使用。
pub fn band_powers(
    samples: &[f64],
    sample_rate_hz: f64,
    centers_hz: &[f64],
    config: &BandLevelsConfig,
) -> Result<Vec<f64>, BandLevelsError> {
    config.validate()?;
    validate_sample_rate(sample_rate_hz)?;
    validate_centers(centers_hz, sample_rate_hz)?;
    let window_len = config.window_len;
    if samples.len() < window_len {
        return Err(BandLevelsError::new(
            BAD_INPUT,
            format!(
                "recording has {} samples, need at least one full window of {window_len}",
                samples.len()
            ),
        ));
    }
    for (index, sample) in samples.iter().enumerate() {
        if !sample.is_finite() {
            return Err(BandLevelsError::new(
                BAD_INPUT,
                format!("sample {index} is not finite"),
            ));
        }
    }

    let window = hann_window(window_len);
    let window_energy: f64 = window.iter().map(|value| value * value).sum();
    let bin_factor = |bin: usize| -> f64 {
        if bin == 0 || bin == window_len / 2 {
            1.0
        } else {
            2.0
        }
    };
    let hop = window_len / 2;
    let mut accumulated = vec![0.0f64; centers_hz.len()];
    let mut segments = 0usize;
    let mut start = 0usize;
    while start + window_len <= samples.len() {
        let mut segment = Vec::with_capacity(window_len);
        for (sample, window_value) in samples[start..start + window_len].iter().zip(window.iter()) {
            segment.push(sample * window_value);
        }
        let spectrum = rfft(&segment, window_len);
        let scale = 1.0 / (window_len as f64 * window_energy);
        for (bin, value) in spectrum.iter().enumerate() {
            let power = bin_factor(bin) * value.norm_sqr() * scale;
            if power <= 0.0 {
                continue;
            }
            let frequency = bin as f64 * sample_rate_hz / window_len as f64;
            if let Some(index) = band_index(centers_hz, frequency) {
                accumulated[index] += power;
            }
        }
        segments += 1;
        start += hop;
    }
    if segments == 0 {
        return Err(BandLevelsError::new(
            BAD_INPUT,
            "no complete analysis window in recording",
        ));
    }
    let segments = segments as f64;
    Ok(accumulated
        .into_iter()
        .map(|power| power / segments)
        .collect())
}

fn hann_window(len: usize) -> Vec<f64> {
    (0..len)
        .map(|index| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * index as f64 / len as f64).cos())
        .collect()
}

/// 1/3 oct 带 [c / 2^(1/6), c * 2^(1/6)) 的索引；无匹配返回 None。
fn band_index(centers_hz: &[f64], frequency: f64) -> Option<usize> {
    let ratio = 2.0f64.powf(1.0 / 6.0);
    centers_hz
        .iter()
        .position(|&center| frequency >= center / ratio && frequency < center * ratio)
}

fn validate_sample_rate(sample_rate_hz: f64) -> Result<(), BandLevelsError> {
    if !sample_rate_hz.is_finite() || sample_rate_hz <= 0.0 {
        return Err(BandLevelsError::new(
            BAD_INPUT,
            format!("sample rate {sample_rate_hz} must be a finite value > 0"),
        ));
    }
    Ok(())
}

fn validate_centers(centers_hz: &[f64], sample_rate_hz: f64) -> Result<(), BandLevelsError> {
    if centers_hz.is_empty() {
        return Err(BandLevelsError::new(
            ALIGNMENT,
            "band centers must not be empty",
        ));
    }
    let nyquist = sample_rate_hz / 2.0;
    let mut previous: Option<f64> = None;
    for &center_hz in centers_hz {
        if !center_hz.is_finite() || center_hz <= 0.0 {
            return Err(BandLevelsError::new(
                ALIGNMENT,
                format!("band center {center_hz} Hz must be a finite value > 0"),
            ));
        }
        if center_hz >= nyquist {
            return Err(BandLevelsError::new(
                ALIGNMENT,
                format!("band center {center_hz} Hz must be below Nyquist {nyquist} Hz"),
            ));
        }
        if let Some(previous) = previous {
            if center_hz <= previous {
                return Err(BandLevelsError::new(
                    ALIGNMENT,
                    format!(
                        "band centers must be strictly ascending and unique: {} after {previous}",
                        center_hz
                    ),
                ));
            }
        }
        previous = Some(center_hz);
    }
    Ok(())
}
