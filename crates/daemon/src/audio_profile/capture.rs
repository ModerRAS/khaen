//! ALSA capture device enumeration（R/IO：/api/device/inputs 的真实数据源）。
//!
//! 为什么只做“设备级枚举”而不直接给 analog/optical 通道表：
//! - ALSA 的 PCM hints / hw_params 只暴露设备名、方向、通道数范围，**不暴露每个通道是模拟还是光口**；
//! - 这个映射在 MOTU 上由设备型号/固件决定，CueMix 的 `kOpticalMode` 只说明光口模式（ADAT/TOSLINK），
//!   不能推出通道序号与角色的完整对应；
//! - 因此本模块只上报**真实枚举到的 capture PCM**（名字/方向/通道范围/打开错误），
//!   通道角色仍由 /api/device/inputs 标为不可用并给原因，绝不猜 analog/optical。
//!
//! Linux 实现直接使用 alsa crate；非 Linux 返回稳定错误（Windows 测试走纯函数/假数据）。
//! 打开 PCM 用 nonblock，只读 hw_params（`HwParams::any`）后立即 drop，不 start、不配置、
//! 不写设备；设备忙/打不开时把错误原样记在该设备条目里，不失败整个探测。

use serde_json::{json, Value};

/// 一个真实枚举到的 ALSA capture PCM。
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureDevice {
    /// PCM 设备名（e.g. `hw:CARD=UltraLitemk5,DEV=0` / `default`）。
    pub name: String,
    /// ALSA hint 的描述（可能为空）。
    pub description: Option<String>,
    /// card index（打开成功且 info 可读时）。
    pub card: Option<i32>,
    /// device index（打开成功且 info 可读时）。
    pub device: Option<u32>,
    /// hw_params 报告的通道数下限（null = 未打开/未知）。
    pub channels_min: Option<u32>,
    /// hw_params 报告的通道数上限（null = 未打开/未知）。
    pub channels_max: Option<u32>,
    /// hw_params 报告的采样率下限 Hz（null = 未打开/未知）。
    pub rate_min: Option<u32>,
    /// hw_params 报告的采样率上限 Hz（null = 未打开/未知）。
    pub rate_max: Option<u32>,
    /// 打开/查询失败原因（null = 成功）。
    pub open_error: Option<String>,
}

impl CaptureDevice {
    /// 纯函数 JSON 形状（供 API/tests 复用）。
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "card": self.card,
            "device": self.device,
            "channels_min": self.channels_min,
            "channels_max": self.channels_max,
            "rate_min": self.rate_min,
            "rate_max": self.rate_max,
            "open_error": self.open_error,
        })
    }
}

/// 枚举本机所有 capture PCM。
///
/// 返回 `Err(reason)` 表示枚举本身不可用（非 Linux / ALSA 库错误）；单个设备打不开时
/// 仍在 `Ok` 列表里带 `open_error`，不把“设备忙”误报成“没有输入设备”。
pub fn enumerate_capture() -> Result<Vec<CaptureDevice>, String> {
    platform_enumerate()
}

/// 纯函数：设备列表 -> JSON 数组（空列表是合法的“枚举成功但 0 个设备”）。
pub fn devices_json(devices: &[CaptureDevice]) -> Value {
    Value::Array(devices.iter().map(CaptureDevice::to_json).collect())
}

#[cfg(target_os = "linux")]
fn platform_enumerate() -> Result<Vec<CaptureDevice>, String> {
    use alsa::device_name::HintIter;
    use alsa::{pcm::HwParams, pcm::PCM, Direction};

    let hints = HintIter::new_str(None, "pcm")
        .map_err(|error| format!("ALSA device_name_hint failed: {error}"))?;
    let mut names: Vec<(String, Option<String>)> = Vec::new();
    for hint in hints {
        if hint.direction.is_some() && hint.direction != Some(Direction::Capture) {
            continue;
        }
        let Some(name) = hint.name else { continue };
        if names.iter().any(|(existing, _)| existing == &name) {
            continue;
        }
        names.push((name, hint.desc));
    }

    let mut devices = Vec::with_capacity(names.len());
    for (name, description) in names {
        let mut device = CaptureDevice {
            name: name.clone(),
            description,
            card: None,
            device: None,
            channels_min: None,
            channels_max: None,
            rate_min: None,
            rate_max: None,
            open_error: None,
        };
        match PCM::new(&name, Direction::Capture, true) {
            Ok(pcm) => {
                if let Ok(info) = pcm.info() {
                    device.card = Some(info.get_card());
                    device.device = Some(info.get_device());
                }
                if let Ok(params) = HwParams::any(&pcm) {
                    device.channels_min = params.get_channels_min().ok();
                    device.channels_max = params.get_channels_max().ok();
                    device.rate_min = params.get_rate_min().ok();
                    device.rate_max = params.get_rate_max().ok();
                }
                // 没 start 过，直接析构即可；显式 drop 让“这里不播放”在代码里可见。
                let _ = pcm.drop();
            }
            Err(error) => {
                device.open_error = Some(error.to_string());
            }
        }
        devices.push(device);
    }
    Ok(devices)
}

#[cfg(not(target_os = "linux"))]
fn platform_enumerate() -> Result<Vec<CaptureDevice>, String> {
    Err(
        "ALSA capture enumeration is Linux-only; this process is not on Linux and the channel list is not fabricated"
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_json_keeps_real_values_and_null_unknowns() {
        let devices = vec![CaptureDevice {
            name: "hw:CARD=UltraLitemk5,DEV=0".to_string(),
            description: Some("UltraLite-mk5".to_string()),
            card: Some(0),
            device: Some(0),
            channels_min: Some(2),
            channels_max: Some(18),
            rate_min: Some(44_100),
            rate_max: Some(96_000),
            open_error: None,
        }];
        let value = devices_json(&devices);
        assert_eq!(value[0]["name"], json!("hw:CARD=UltraLitemk5,DEV=0"));
        assert_eq!(value[0]["channels_max"], json!(18));
        assert_eq!(value[0]["rate_min"], json!(44_100));
        assert_eq!(value[0]["rate_max"], json!(96_000));
        assert!(value[0]["open_error"].is_null());
        assert_eq!(devices_json(&[]), json!([]));
    }

    #[test]
    fn capture_device_json_never_invents_channel_roles() {
        let value = CaptureDevice {
            name: "default".to_string(),
            description: None,
            card: None,
            device: None,
            channels_min: None,
            channels_max: None,
            rate_min: None,
            rate_max: None,
            open_error: Some("device busy".to_string()),
        }
        .to_json();
        assert_eq!(value["channels_min"], Value::Null);
        assert_eq!(value["channels_max"], Value::Null);
        assert_eq!(value["rate_min"], Value::Null);
        assert_eq!(value["rate_max"], Value::Null);
        assert_eq!(value["open_error"], json!("device busy"));
        assert!(value.get("kind").is_none());
        assert!(value.get("channels").is_none());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn non_linux_enumerator_returns_a_stable_reason() {
        let error = enumerate_capture().expect_err("non-Linux must not fabricate capture devices");
        assert!(error.contains("Linux-only"), "{error}");
    }
}
