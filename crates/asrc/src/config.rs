//! ASRC 配置与校验。
//!
//! 所有字段在构造期固定：RT 路径（[crate::Asrc::process]）不再读取/改写配置，
//! 因此没有锁、没有原子、没有环境查询。

use crate::error::AsrcError;

/// 默认允许的最大比率修正（ppm）。±500 ppm 覆盖常见消费级/板载时钟偏差
/// （晶振通常 ±100 ppm 以内，加上温漂/老化后仍远小于该值）。
pub const DEFAULT_MAX_PPM: f64 = 500.0;
/// 默认抖动环目标水位（块数）：2 块 ≈ 5.3 ms @128 帧。
pub const DEFAULT_TARGET_BLOCKS: usize = 2;
/// fill 测量低通时间常数（秒）：滤掉收包节拍造成的锯齿。
pub const DEFAULT_FILL_LPF_SEC: f64 = 1.0;
/// 比例项增益（ppm / 帧）。
pub const DEFAULT_KP_PPM_PER_FRAME: f64 = 10.0;
/// 积分项增益（ppm / (帧·秒)）。
pub const DEFAULT_KI_PPM_PER_FRAME_SEC: f64 = 2.5;
/// 启动阶段块数：先只让 fill 低通收敛，不积分、不修正比率。
pub const DEFAULT_STARTUP_BLOCKS: u32 = 16;
/// sinc 备用模式的默认滤波器长度（样本）。
pub const DEFAULT_SINC_LEN: usize = 64;
/// sinc 备用模式的默认过采样因子。
pub const DEFAULT_SINC_OVERSAMPLING: usize = 64;
/// 单块 ASRC 支持的最大声道数（防御边界；AES67 每流 ≤ 8）。
pub const MAX_ASRC_CHANNELS: usize = 64;
/// 最大可配置修正范围（ppm）：超出即拒绝，避免构造出没有意义的比率范围。
pub const MAX_CONFIGURABLE_PPM: f64 = 10_000.0;

/// 重采样插值方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Interpolation {
    /// 多项式插值：无抗混叠滤波，速度最快；±500 ppm 的近 1:1 比率下已足够（见 README）。
    Poly,
    /// sinc 插值：带抗混叠滤波，质量最好、成本更高；作为高要求场景的备用。
    Sinc,
}

impl Interpolation {
    /// 稳定名字（环境变量/证据/日志用）。
    pub const fn name(self) -> &'static str {
        match self {
            Interpolation::Poly => "poly",
            Interpolation::Sinc => "sinc",
        }
    }

    /// 解析 poly / sinc（大小写不敏感，允许首尾空白）；其它返回 None。
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "poly" => Some(Interpolation::Poly),
            "sinc" => Some(Interpolation::Sinc),
            _ => None,
        }
    }
}

/// AES67 输入 ASRC 配置（构造期固定）。
///
/// [AsrcConfig::default] 给出 2 声道 / 128 帧块 / 48 kHz 的保守取值；
/// host 接线时按自己的 Aes67InputConfig 覆盖 channels/block_frames/sample_rate。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AsrcConfig {
    /// 是否启用 ASRC。host 在 host 侧把 off 映射成"不构造 Asrc"（本字段只做自描述）。
    pub enable: bool,
    /// 插值方式（poly 默认，sinc 备用）。
    pub interp: Interpolation,
    /// 声道数（= 每帧样本数；与抖动环 RTP 声道数一致）。
    pub channels: usize,
    /// 固定输出块帧数（= 引擎 block_frames；[crate::AsyncResampler] 输出恒为该值）。
    pub block_frames: usize,
    /// 媒体/本地名义采样率（Hz；控制器离散化用）。
    pub sample_rate: u32,
    /// 抖动环目标水位（帧）：控制器把 fill 稳定在该值附近。
    /// 必须 ≥ block_frames（至少留一块可消费数据）。
    pub target_fill_frames: usize,
    /// 比率修正限幅（ppm，正数；控制器输出被限制在 ±max_ppm）。
    pub max_ppm: f64,
    /// 比例增益（ppm / 帧）。
    pub kp_ppm_per_frame: f64,
    /// 积分增益（ppm / (帧·秒)）。
    pub ki_ppm_per_frame_sec: f64,
    /// fill 测量低通时间常数（秒）。
    pub fill_lpf_sec: f64,
    /// 启动阶段块数（只更新低通、不积分、不修正）。
    pub startup_blocks: u32,
    /// sinc 模式滤波器长度（样本；rubato 会向上取整到 8 的倍数）。
    pub sinc_len: usize,
    /// sinc 模式过采样因子。
    pub sinc_oversampling: usize,
}

impl Default for AsrcConfig {
    fn default() -> Self {
        Self {
            enable: true,
            interp: Interpolation::Poly,
            channels: 2,
            block_frames: 128,
            sample_rate: 48_000,
            target_fill_frames: DEFAULT_TARGET_BLOCKS * 128,
            max_ppm: DEFAULT_MAX_PPM,
            kp_ppm_per_frame: DEFAULT_KP_PPM_PER_FRAME,
            ki_ppm_per_frame_sec: DEFAULT_KI_PPM_PER_FRAME_SEC,
            fill_lpf_sec: DEFAULT_FILL_LPF_SEC,
            startup_blocks: DEFAULT_STARTUP_BLOCKS,
            sinc_len: DEFAULT_SINC_LEN,
            sinc_oversampling: DEFAULT_SINC_OVERSAMPLING,
        }
    }
}

impl AsrcConfig {
    /// 按 host 输入参数建立配置（其余字段取默认值）。
    pub fn new(channels: usize, block_frames: usize, sample_rate: u32) -> Self {
        Self {
            channels,
            block_frames,
            sample_rate,
            target_fill_frames: DEFAULT_TARGET_BLOCKS * block_frames,
            ..Self::default()
        }
    }

    /// 校验配置；失败返回 [AsrcError::BadConfig]（带点名原因，fail-closed）。
    pub fn validate(&self) -> Result<(), AsrcError> {
        if self.channels == 0 || self.channels > MAX_ASRC_CHANNELS {
            return Err(AsrcError::BadConfig(
                "channels must be in 1..=MAX_ASRC_CHANNELS",
            ));
        }
        if self.block_frames == 0 {
            return Err(AsrcError::BadConfig("block_frames must be positive"));
        }
        if self.sample_rate == 0 {
            return Err(AsrcError::BadConfig("sample_rate must be positive"));
        }
        if self.target_fill_frames < self.block_frames {
            return Err(AsrcError::BadConfig(
                "target_fill_frames must be >= block_frames",
            ));
        }
        if !self.max_ppm.is_finite() || self.max_ppm <= 0.0 {
            return Err(AsrcError::BadConfig("max_ppm must be finite and positive"));
        }
        if self.max_ppm > MAX_CONFIGURABLE_PPM {
            return Err(AsrcError::BadConfig("max_ppm exceeds MAX_CONFIGURABLE_PPM"));
        }
        if !self.kp_ppm_per_frame.is_finite() || self.kp_ppm_per_frame < 0.0 {
            return Err(AsrcError::BadConfig(
                "kp_ppm_per_frame must be finite and >= 0",
            ));
        }
        if !self.ki_ppm_per_frame_sec.is_finite() || self.ki_ppm_per_frame_sec < 0.0 {
            return Err(AsrcError::BadConfig(
                "ki_ppm_per_frame_sec must be finite and >= 0",
            ));
        }
        if self.kp_ppm_per_frame == 0.0 && self.ki_ppm_per_frame_sec == 0.0 {
            return Err(AsrcError::BadConfig(
                "at least one controller gain must be positive",
            ));
        }
        if !self.fill_lpf_sec.is_finite() || self.fill_lpf_sec <= 0.0 {
            return Err(AsrcError::BadConfig(
                "fill_lpf_sec must be finite and positive",
            ));
        }
        if self.startup_blocks > 1_000_000 {
            return Err(AsrcError::BadConfig("startup_blocks is unreasonably large"));
        }
        if self.interp == Interpolation::Sinc {
            if self.sinc_len < 8 {
                return Err(AsrcError::BadConfig("sinc_len must be >= 8"));
            }
            if self.sinc_oversampling == 0 {
                return Err(AsrcError::BadConfig("sinc_oversampling must be positive"));
            }
        }
        Ok(())
    }

    /// 每个输出块对应的名义时长（秒）。
    pub fn block_seconds(&self) -> f64 {
        self.block_frames as f64 / self.sample_rate as f64
    }
}
