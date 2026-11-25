//! 校准数据模型：测量元数据、频响点、每通道分析结果、PEQ 与校准文件。
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meta {
    pub schema: String,
    pub sample_rate: u32,
    pub channels: usize,
    pub sweep_start_hz: f64,
    pub sweep_end_hz: f64,
    pub sweep_seconds: f64,
    pub level_db: f64,
    pub pre_seconds: f64,
    pub post_seconds: f64,
    pub playback_device: String,
    pub playback_channels: usize,
    pub capture_device: String,
    pub capture_channels: usize,
    pub created_unix: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FrPoint {
    pub freq_hz: f64,
    pub db: f64,
    pub phase_deg: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelAnalysis {
    pub index: usize,
    pub delay_samples: i64,
    pub polarity: i32,
    pub peak_db: f64,
    pub fr_csv: String,
}

fn default_filter_type() -> String {
    "peaking".into()
}

fn is_peaking_type(s: &String) -> bool {
    s == "peaking"
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Peq {
    pub freq: f64,
    pub q: f64,
    pub gain_db: f64,
    /// peaking | low_shelf | high_shelf。缺省 peaking，写出时省略。
    #[serde(
        default = "default_filter_type",
        rename = "type",
        skip_serializing_if = "is_peaking_type"
    )]
    pub filter_type: String,
}

impl Peq {
    pub fn peaking(freq: f64, q: f64, gain_db: f64) -> Self {
        Self {
            freq,
            q,
            gain_db,
            filter_type: "peaking".into(),
        }
    }

    pub fn typed(filter_type: &str, freq: f64, q: f64, gain_db: f64) -> Self {
        Self {
            freq,
            q,
            gain_db,
            filter_type: filter_type.into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelCalibration {
    /// 播放设备通道号（保留）。
    pub index: usize,
    /// 角色 id（FL/FR/C/SUB1/...），v1 必填且唯一。
    pub name: String,
    /// 相对延迟（整数样本，min=0 归一化）。
    pub delay_samples: i64,
    /// "normal" | "invert"（旧 ±1 作为别名接受）。
    /// SUB 在只有听音位、估计器不可信时省略（契约校验器对此只告警）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polarity: Option<String>,
    pub filters: Vec<Peq>,
    /// 可选通道增益（dB）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain_db: Option<f64>,
}

/// 三路（加低频/全带相关）极性估计。ir_peak 只记录，不参与否决。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PolarityEstimators {
    pub ir_peak: i32,
    pub direct_window: i32,
    pub phase_vote: i32,
    pub lowband: i32,
    pub matched: i32,
    /// 与参考麦波形互相关的符号；没有参考麦时省略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_xcorr: Option<i32>,
}

/// 多遍扫频的逐遍记录。delay_samples 是去掉重复间隔后的相对延迟。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepeatDetail {
    pub count: usize,
    pub delay_samples: Vec<f64>,
    pub polarity_votes: Vec<i32>,
    pub ir_peak_idx: Vec<i64>,
    pub snr_db: Vec<f64>,
    /// 每一遍的置信度（第二层投票之前）。
    pub per_repeat_confidence: Vec<String>,
    pub per_repeat_estimators: Vec<PolarityEstimators>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub onset_invalid: Vec<bool>,
    /// v1.1：逐遍起点（去掉重复间隔前的原始 onset，样本，相对重复窗口起点）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub onsets: Vec<f64>,
    /// 相邻差 o[k+1]-o[k]。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repeat_diffs: Vec<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepted_diffs: Vec<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejected_diffs: Vec<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spacing_samples: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_samples: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub residuals: Vec<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_ppm_source: Option<String>,
    #[serde(default)]
    pub insufficient_valid_passes: bool,
    /// A3：跨遍 IR 峰一致性门（|peak-median|>200 样本）标 suspect，不进 ppm/延迟主聚合。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ir_peak_suspect: Vec<bool>,
}

/// 参考麦（通常是 MOTU 同源时钟）的摘要。不进入 khaen-calibration/v1 的 channels。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReferenceSummary {
    pub channel: usize,
    pub name: String,
    pub polarity: i32,
    pub polarity_confidence: String,
    pub delay_samples: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_ppm: Option<f64>,
    pub file: String,
    /// 起点和极性都可信时才允许覆盖主麦。
    #[serde(default)]
    pub valid: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Calibration {
    pub schema: String,
    pub sample_rate: u32,
    /// 参考通道（delay=0 的那路）。
    pub reference_index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub note: String,
    pub channels: Vec<ChannelCalibration>,
}
/// 单通道测量/分析结果（analysis JSON）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelMeasurement {
    pub schema: String,
    pub channel: usize,
    pub delay_samples: f64,
    pub delay_ms: f64,
    pub delay_coarse_samples: i64,
    pub polarity: i32,
    pub peak_db: f64,
    pub phase_1k_deg: f64,
    pub snr_db: f64,
    /// 相位参考：aligned_segment 表示已去掉粗延迟；绝对相位 = phase_deg - 360*f*delay_samples/fs
    #[serde(default)]
    pub phase_ref: String,
    pub fr: Vec<FrPoint>,
    /// 1/48 oct 频响。旧分析文件没有该字段时，拟合退回 `fr`。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fr_modal: Vec<FrPoint>,
    pub meta: Meta,
    /// 多遍扫频明细。旧分析文件没有该字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeats: Option<RepeatDetail>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_ppm: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_ppm_ok: Option<bool>,
    /// A1：ppm 不确定度（ppm）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_ppm_uncertainty_ppm: Option<f64>,
    /// A1：|ppm| > 2*uncertainty 才为 true；--drift-correct 只在 true 时启用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_ppm_significant: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drift_corrected: Option<bool>,
    /// P0-3：通道通带内 SNR（带限通道如 SUB 用自身通带）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snr_band_db: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snr_band_hz: Option<(f64, f64)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub onset_invalid: Option<bool>,
    /// high | low | unknown。旧文件没有该字段时，merge 视作历史结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polarity_confidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polarity_estimators: Option<PolarityEstimators>,
    /// 投票结果（SUB 听音位不可信时仍保留，供 --allow-sub-polarity-from-listening-position）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polarity_robust: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polarity_listening: Option<i32>,
    /// listening | reference | legacy-ir-peak
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polarity_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_polarity_basis: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<ReferenceSummary>,
    /// "reference" 的分析文件会被 merge 跳过，避免参考麦进入校准通道。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measurement_role: Option<String>,
    /// 测点。旧分析文件没有该字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<PositionMeta>,
    /// 参考麦起点无效时为 true，此时 polarity 仍是主麦自己的结论。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_invalid: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_invalid_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PositionMeta {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<f64>,
}

/// 合并验证报告（相位抵消检查）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidationReport {
    pub schema: String,
    pub passed: bool,
    pub f_min_hz: f64,
    pub f_max_hz: f64,
    pub crossovers: Vec<f64>,
    pub worst_freq_hz: f64,
    pub worst_margin_db: f64,
    pub checks: Vec<ValidationCheck>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidationCheck {
    pub freq_hz: f64,
    pub sum_db: f64,
    pub max_single_db: f64,
    pub margin_db: f64,
    pub status: String,
}
