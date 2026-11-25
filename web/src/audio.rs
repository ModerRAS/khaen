//! 音频格式与缓冲参数（采样率 / 位深 / 声道 / chunksize / target_level）展示与有限编辑。

use serde::Serialize;

use crate::camilla::{scan_xrun, DeviceFields, XrunInfo};
use crate::config::{AppConfig, ModeProfile, RunMode};
use crate::error::{AppError, AppResult};

/// 允许的采样率（页面下拉 + 服务端校验）：产品仅支持 ≤96 kHz。
///
/// 硬约束：禁止 176400/192000 Hz（MOTU 空闲 192k 根因报告 §2.3；默认 48k，96k 可用）。
pub const SAMPLE_RATES: [u32; 4] = [44100, 48000, 88200, 96000];
/// 明确禁用的采样率：即使未来白名单被误改也必须拒绝（PUT 返回稳定错误）。
pub const FORBIDDEN_SAMPLE_RATES: [u32; 2] = [176400, 192000];
/// 允许的 chunksize（32 帧可选但属高风险，页面会提示）。
pub const CHUNKSIZES: [u32; 6] = [32, 64, 128, 256, 512, 1024];
pub const TARGET_LEVEL_MIN: u32 = 0;
pub const TARGET_LEVEL_MAX: u32 = 65536;
pub const BIT_DEPTHS: [u32; 2] = [16, 24];

#[derive(Debug, Clone, Serialize)]
pub struct FormatLimits {
    pub sample_rates: Vec<u32>,
    pub chunksizes: Vec<u32>,
    pub target_level_min: u32,
    pub target_level_max: u32,
    pub bit_depths: Vec<u32>,
}

impl Default for FormatLimits {
    fn default() -> Self {
        Self {
            sample_rates: SAMPLE_RATES.to_vec(),
            chunksizes: CHUNKSIZES.to_vec(),
            target_level_min: TARGET_LEVEL_MIN,
            target_level_max: TARGET_LEVEL_MAX,
            bit_depths: BIT_DEPTHS.to_vec(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FormatProfile {
    pub mode: String,
    pub label: String,
    pub input_format: String,
    pub output_format: String,
    pub default_chunksize: u32,
    pub default_target_level: u32,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct AudioFormatView {
    pub mode: String,
    pub mode_label: String,
    pub source: String,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u32>,
    pub channels: Option<u32>,
    pub chunksize: Option<u32>,
    pub target_level: Option<u32>,
    pub input_format: String,
    pub output_format: String,
    pub config_path: Option<String>,
    pub limits: FormatLimits,
    pub profiles: Vec<FormatProfile>,
    pub warnings: Vec<String>,
    pub xrun: XrunInfo,
    pub mock: bool,
}

fn fmt_rate(rate: u32) -> String {
    format!("{} kHz", rate as f64 / 1000.0)
}

fn format_profile(p: &ModeProfile) -> (String, String) {
    (
        format!(
            "{} / {}-bit / {} ch",
            fmt_rate(p.input_sample_rate),
            p.input_bit_depth,
            p.channels
        ),
        format!(
            "MOTU {} / {}-bit / {} ch",
            fmt_rate(p.output_sample_rate),
            p.output_bit_depth,
            p.channels
        ),
    )
}

pub fn profile_views(config: &AppConfig) -> Vec<FormatProfile> {
    [RunMode::Dante, RunMode::Aes67]
        .iter()
        .filter_map(|m| config.mode.profile(*m).map(|p| (*m, p)))
        .map(|(m, p)| {
            let (input_format, output_format) = format_profile(p);
            FormatProfile {
                mode: m.as_str().to_string(),
                label: p.label.clone(),
                input_format,
                output_format,
                default_chunksize: p.default_chunksize,
                default_target_level: p.default_target_level,
            }
        })
        .collect()
}

/// 服务端校验格式修改请求。
pub fn validate_update(
    sample_rate: Option<u32>,
    chunksize: Option<u32>,
    target_level: Option<u32>,
) -> AppResult<()> {
    if sample_rate.is_none() && chunksize.is_none() && target_level.is_none() {
        return Err(AppError::bad_request(
            "至少提供 sample_rate / chunksize / target_level 之一",
        ));
    }
    if let Some(sr) = sample_rate {
        if FORBIDDEN_SAMPLE_RATES.contains(&sr) {
            return Err(AppError::bad_request(format!(
                "采样率 {sr} Hz 已禁用（产品硬约束：禁止 176400/192000 Hz；可选: {:?}）",
                SAMPLE_RATES
            )));
        }
        if !SAMPLE_RATES.contains(&sr) {
            return Err(AppError::bad_request(format!(
                "不支持的采样率 {sr} Hz（可选: {:?}）",
                SAMPLE_RATES
            )));
        }
    }
    if let Some(cs) = chunksize {
        if !CHUNKSIZES.contains(&cs) {
            return Err(AppError::bad_request(format!(
                "不支持的 chunksize {cs}（可选: {:?}）",
                CHUNKSIZES
            )));
        }
    }
    if let Some(tl) = target_level {
        if !(TARGET_LEVEL_MIN..=TARGET_LEVEL_MAX).contains(&tl) {
            return Err(AppError::bad_request(format!(
                "target_level 必须在 {TARGET_LEVEL_MIN}~{TARGET_LEVEL_MAX}"
            )));
        }
    }
    Ok(())
}

/// chunksize=32 的高风险提示（依赖 FIR/声道/模式，需实测 xrun）。
pub fn chunksize_warning(chunksize: Option<u32>) -> Option<String> {
    if chunksize == Some(32) {
        Some(
            "chunksize=32 属于高风险档位：是否稳定取决于 FIR 长度、声道数与运行模式，必须实测 xrun；默认建议 128"
                .into(),
        )
    } else {
        None
    }
}

#[allow(clippy::too_many_arguments)]
pub fn build_view(
    config: &AppConfig,
    mode: RunMode,
    fields: Option<DeviceFields>,
    source: &str,
    config_path: Option<String>,
    xrun: XrunInfo,
    mock: bool,
) -> AudioFormatView {
    let profile = config.mode.profile(mode);
    // 关闭模式下退回 AES67 的展示默认值（页面仍显示模式默认格式）
    let fallback = profile.or_else(|| config.mode.profile(RunMode::Aes67));
    let f = fields.clone().unwrap_or_default();

    let sample_rate = f.sample_rate.or(fallback.map(|p| p.input_sample_rate));
    let bit_depth = fallback.map(|p| p.input_bit_depth);
    let channels = f.capture_channels.or(fallback.map(|p| p.channels));
    let chunksize = f.chunksize.or(fallback.map(|p| p.default_chunksize));
    let target_level = f.target_level.or(fallback.map(|p| p.default_target_level));

    let playback_channels = f.playback_channels.or(channels);
    let out_rate = f.sample_rate.or(fallback.map(|p| p.output_sample_rate));
    let out_bit = fallback.map(|p| p.output_bit_depth);

    let input_format = match (sample_rate, bit_depth, channels) {
        (Some(sr), Some(bd), Some(ch)) => format!("{} / {}-bit / {} ch", fmt_rate(sr), bd, ch),
        _ => "未知".to_string(),
    };
    let output_format = match (out_rate, out_bit, playback_channels) {
        (Some(sr), Some(bd), Some(ch)) => {
            format!("MOTU {} / {}-bit / {} ch", fmt_rate(sr), bd, ch)
        }
        _ => "未知".to_string(),
    };

    let mut warnings = Vec::new();
    if mode == RunMode::Off {
        warnings.push("当前未启用运行模式，格式取自 CamillaDSP 当前配置 / 模式默认值".into());
    }
    if let Some(w) = chunksize_warning(chunksize) {
        warnings.push(w);
    }
    if let (Some(fields_sr), Some(p)) = (f.sample_rate, profile) {
        if fields_sr != p.input_sample_rate {
            warnings.push(format!(
                "当前配置采样率 {} 与模式默认 {} 不一致（如非有意配置请检查）",
                fmt_rate(fields_sr),
                fmt_rate(p.input_sample_rate)
            ));
        }
    }
    if mode == RunMode::Aes67 {
        warnings.push(
            "AES67 默认 48 kHz / 24-bit（输入兼容 16-bit）；输出 MOTU 24-bit，采样率默认跟随输入"
                .into(),
        );
    }
    if mode == RunMode::Dante {
        warnings.push("Dante 默认 96 kHz / 24-bit（输入兼容 16-bit）；输出 MOTU 24-bit".into());
    }

    AudioFormatView {
        mode: mode.as_str().to_string(),
        mode_label: profile
            .map(|p| p.label.clone())
            .unwrap_or_else(|| "关闭（Off）".into()),
        source: source.to_string(),
        sample_rate,
        bit_depth,
        channels,
        chunksize,
        target_level,
        input_format,
        output_format,
        config_path,
        limits: FormatLimits::default(),
        profiles: profile_views(config),
        warnings,
        xrun,
        mock,
    }
}

/// 从服务日志统计 xrun（供路由复用）。
pub fn xrun_from_logs(logs: &str) -> XrunInfo {
    scan_xrun(logs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_config() -> AppConfig {
        AppConfig::mock_defaults()
    }

    #[test]
    fn validate_accepts_known_values() {
        assert!(validate_update(Some(44100), Some(128), Some(2048)).is_ok());
        assert!(validate_update(Some(48000), Some(128), Some(2048)).is_ok());
        assert!(validate_update(Some(88200), None, None).is_ok());
        assert!(validate_update(Some(96000), None, None).is_ok());
        assert!(validate_update(None, Some(32), None).is_ok());
    }

    #[test]
    fn sample_rates_are_capped_at_96k() {
        assert_eq!(SAMPLE_RATES, [44100, 48000, 88200, 96000]);
        assert!(!SAMPLE_RATES.contains(&176400));
        assert!(!SAMPLE_RATES.contains(&192000));
        assert_eq!(FORBIDDEN_SAMPLE_RATES, [176400, 192000]);
    }

    #[test]
    fn validate_rejects_192k_with_stable_error() {
        for rate in FORBIDDEN_SAMPLE_RATES {
            let err = validate_update(Some(rate), None, None).expect_err("192k/176.4k 必须被拒绝");
            let message = err.message();
            assert!(message.contains(&format!("{rate} Hz 已禁用")), "{message}");
            assert!(message.contains("禁止 176400/192000 Hz"), "{message}");
            assert_eq!(err.code(), "bad_request");
        }
    }

    #[test]
    fn validate_rejects_bad_values() {
        assert!(validate_update(None, None, None).is_err());
        assert!(validate_update(Some(12345), None, None).is_err());
        assert!(validate_update(None, Some(100), None).is_err());
        assert!(validate_update(None, None, Some(999_999)).is_err());
    }

    #[test]
    fn chunksize_32_warns() {
        assert!(chunksize_warning(Some(32)).unwrap().contains("高风险"));
        assert!(chunksize_warning(Some(128)).is_none());
    }

    #[test]
    fn view_uses_mode_defaults_when_off() {
        let cfg = base_config();
        let view = build_view(
            &cfg,
            RunMode::Off,
            None,
            "模式默认值",
            None,
            XrunInfo::default(),
            true,
        );
        assert_eq!(view.sample_rate, Some(48000));
        assert_eq!(view.bit_depth, Some(24));
        assert_eq!(view.channels, Some(2));
        assert!(view.input_format.contains("48 kHz"));
        assert!(view.output_format.contains("MOTU"));
        assert_eq!(view.profiles.len(), 2);
    }

    #[test]
    fn view_prefers_live_config_fields() {
        let cfg = base_config();
        let fields = DeviceFields {
            sample_rate: Some(96000),
            chunksize: Some(256),
            target_level: Some(1024),
            capture_channels: Some(2),
            playback_channels: Some(24),
            capture_device: Some("hw:Loopback,0,0".into()),
            playback_device: Some("hw:MOTU,0,0".into()),
        };
        let view = build_view(
            &cfg,
            RunMode::Dante,
            Some(fields),
            "camilladsp 当前激活配置",
            Some("/etc/camilladsp/configs/dante.yml".into()),
            XrunInfo::default(),
            true,
        );
        assert_eq!(view.sample_rate, Some(96000));
        assert_eq!(view.chunksize, Some(256));
        assert_eq!(view.target_level, Some(1024));
        assert!(view.output_format.contains("24 ch"));
        assert!(view.warnings.iter().any(|w| w.contains("96 kHz / 24-bit")));
    }
}
