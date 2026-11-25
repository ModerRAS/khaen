//! ALSA 声卡（MOTU UltraLite-mk5）与 USB 设备状态、测试音。
//!
//! 数据来源：/proc/asound/cards、/proc/asound/card*/pcm*/info|sub*/hw_params、
//! /sys/bus/usb/devices；测试音用内置 WAV + \`aplay -D plughw:X,Y\`。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;

use crate::cmd::CommandRunner;
use crate::config::AlsaConfig;
use crate::error::{AppError, AppResult};

/// 内置测试音（1 kHz，48 kHz，16-bit，1 s，带淡入淡出）。
pub const TEST_TONE_WAV: &[u8] = include_bytes!("../assets/test-tone.wav");

#[derive(Debug, Clone, Serialize, Default)]
pub struct AlsaCard {
    pub index: u32,
    pub id: String,
    pub driver: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PcmEntry {
    pub card: u32,
    pub card_id: String,
    pub device: u32,
    pub pcm_type: String,
    pub name: String,
    pub subdevices: Option<u32>,
    pub subdevices_available: Option<u32>,
    pub active: bool,
    /// 设备支持的最大通道数（来自 /proc/asound/cardX/stream0，未知为 None）。
    pub channels: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct HwParamsEntry {
    pub card: u32,
    pub device: u32,
    pub pcm_type: String,
    pub subdevice: u32,
    pub format: Option<String>,
    pub channels: Option<u32>,
    pub rate: Option<u32>,
    pub period_size: Option<u32>,
    pub buffer_size: Option<u32>,
    pub raw: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct UsbDevice {
    pub bus: String,
    pub device: String,
    pub id: String,
    pub product: String,
    pub manufacturer: String,
    pub speed: String,
    pub is_audio: bool,
    pub is_motu: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct AlsaView {
    pub cards: Vec<AlsaCard>,
    pub pcm: Vec<PcmEntry>,
    pub hw_params: Vec<HwParamsEntry>,
    pub usb_devices: Vec<UsbDevice>,
    pub motu_present: bool,
    pub raw_cards: String,
    pub raw_aplay_l: String,
    pub warnings: Vec<String>,
    pub mock: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ToneResult {
    pub device: String,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub message: String,
}

#[async_trait]
pub trait AlsaBackend: Send + Sync {
    async fn view(&self) -> AlsaView;
    async fn test_tone(&self, device: Option<&str>) -> AppResult<ToneResult>;
}

/// 解析 /proc/asound/cards。
pub fn parse_asound_cards(text: &str) -> Vec<AlsaCard> {
    let mut cards: Vec<AlsaCard> = Vec::new();
    for line in text.lines() {
        let t = line.trim_end();
        if t.trim().is_empty() {
            continue;
        }
        if t.contains('[') && t.contains(']') && t.contains(':') {
            let Some(open) = t.find('[') else { continue };
            let Some(close) = t.find(']') else { continue };
            let index = t[..open].trim().parse::<u32>().unwrap_or(0);
            let id = t[open + 1..close].trim().to_string();
            let rest = t[close + 1..].trim_start_matches(':').trim();
            let (driver, name) = match rest.split_once(" - ") {
                Some((d, n)) => (d.trim().to_string(), n.trim().to_string()),
                None => (String::new(), rest.to_string()),
            };
            cards.push(AlsaCard {
                index,
                id,
                driver,
                name,
            });
        } else if let Some(card) = cards.last_mut() {
            // 第二行是更长的描述
            if card.name.is_empty() {
                card.name = t.trim().to_string();
            }
        }
    }
    cards
}

/// /proc/asound 的 pcm info 解析结果：(card, device, stream, name, 子设备总数, 可用数)。
pub type PcmInfo = (u32, u32, String, String, Option<u32>, Option<u32>);

/// hw_params 解析结果：(format, channels, rate, period_size, buffer_size)。
pub type HwParamsFields = (
    Option<String>,
    Option<u32>,
    Option<u32>,
    Option<u32>,
    Option<u32>,
);

/// 解析 /proc/asound/cardX/pcmYp/info。
pub fn parse_pcm_info(text: &str) -> Option<PcmInfo> {
    let mut card = None;
    let mut device = None;
    let mut stream = String::new();
    let mut name = String::new();
    let mut count = None;
    let mut avail = None;
    for line in text.lines() {
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim();
            let v = v.trim();
            match k {
                "card" => card = v.parse::<u32>().ok(),
                "device" => device = v.parse::<u32>().ok(),
                "stream" => stream = v.to_string(),
                "name" => name = v.to_string(),
                "subdevices_count" => count = v.parse::<u32>().ok(),
                "subdevices_avail" => avail = v.parse::<u32>().ok(),
                _ => {}
            }
        }
    }
    Some((
        card?,
        device?,
        stream.to_ascii_lowercase(),
        name,
        count,
        avail,
    ))
}

/// 解析 hw_params 文本为结构化字段。
/// 校验 ALSA 设备名（防止把奇怪字符传进 aplay；虽然不经过 shell，仍做白名单）。
pub fn validate_device_arg(dev: &str) -> Result<(), String> {
    let d = dev.trim();
    if d.is_empty() || d.len() > 64 {
        return Err("设备名为空或过长".into());
    }
    if !d
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | ',' | '=' | '_' | '-' | '.'))
    {
        return Err("设备名包含非法字符".into());
    }
    Ok(())
}

/// 解析 /proc/asound/cardX/stream0 的 Playback/Capture 最大通道数。
/// 返回 (playback_max_channels, capture_max_channels)。
pub fn parse_stream0(text: &str) -> (Option<u32>, Option<u32>) {
    let mut section = String::new();
    let mut playback = None;
    let mut capture = None;
    for line in text.lines() {
        let t = line.trim();
        if t.eq_ignore_ascii_case("Playback:") {
            section = "playback".into();
            continue;
        }
        if t.eq_ignore_ascii_case("Capture:") {
            section = "capture".into();
            continue;
        }
        if let Some(rest) = t.strip_prefix("Channels:") {
            if let Ok(n) = rest.trim().parse::<u32>() {
                match section.as_str() {
                    "playback" => playback = Some(playback.map_or(n, |m: u32| m.max(n))),
                    "capture" => capture = Some(capture.map_or(n, |m: u32| m.max(n))),
                    _ => {}
                }
            }
        }
    }
    (playback, capture)
}

/// 生成物理输出通道显示名（MOTU 风格：Main1/2 + Line3..N；其他设备：ChN）。
pub fn default_output_channel_names(channels: u32) -> Vec<String> {
    let n = channels.max(1);
    let mut names = Vec::with_capacity(n as usize);
    for idx in 0..n {
        let name = match idx {
            0 => "Main1".to_string(),
            1 => "Main2".to_string(),
            _ => format!("Line{}", idx + 1),
        };
        names.push(name);
    }
    names
}

pub fn parse_hw_params(text: &str) -> HwParamsFields {
    let mut format = None;
    let mut channels = None;
    let mut rate = None;
    let mut period = None;
    let mut buffer = None;
    for line in text.lines() {
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim();
            let v = v.trim();
            match k {
                "format" => format = Some(v.to_string()),
                "channels" => channels = v.parse().ok(),
                "rate" => rate = v.split_whitespace().next().and_then(|x| x.parse().ok()),
                "period_size" => period = v.parse().ok(),
                "buffer_size" => buffer = v.parse().ok(),
                _ => {}
            }
        }
    }
    (format, channels, rate, period, buffer)
}

pub struct RealAlsa {
    cfg: AlsaConfig,
    runner: Arc<dyn CommandRunner>,
}

impl RealAlsa {
    pub fn new(cfg: AlsaConfig, runner: Arc<dyn CommandRunner>) -> Self {
        Self { cfg, runner }
    }

    async fn scan(&self) -> AlsaView {
        let mut view = AlsaView {
            mock: false,
            ..Default::default()
        };
        match tokio::fs::read_to_string("/proc/asound/cards").await {
            Ok(text) => {
                view.raw_cards = text.clone();
                view.cards = parse_asound_cards(&text);
            }
            Err(e) => view
                .warnings
                .push(format!("/proc/asound/cards 不可读: {e}")),
        }
        if let Ok(out) = self.runner.run(&self.cfg.aplay_bin, &["-l"]).await {
            view.raw_aplay_l = out.stdout.clone();
            if !out.success {
                view.warnings
                    .push("aplay -l 返回非零（ALSA 工具可能未安装）".into());
            }
        } else {
            view.warnings
                .push("无法执行 aplay -l（alsa-utils 未安装？）".into());
        }

        let card_ids: std::collections::HashMap<u32, String> =
            view.cards.iter().map(|c| (c.index, c.id.clone())).collect();

        // 每个声卡的最大 Playback/Capture 通道数（/proc/asound/cardX/stream0）
        let mut card_channels: std::collections::HashMap<u32, (Option<u32>, Option<u32>)> =
            std::collections::HashMap::new();
        for card in &view.cards {
            let stream_path = format!("/proc/asound/card{}/stream0", card.index);
            if let Ok(text) = tokio::fs::read_to_string(&stream_path).await {
                card_channels.insert(card.index, parse_stream0(&text));
            }
        }

        // 扫描 /proc/asound/card*/pcm*{p,c}
        if let Ok(mut dir) = tokio::fs::read_dir("/proc/asound").await {
            while let Ok(Some(entry)) = dir.next_entry().await {
                let name = entry.file_name().to_string_lossy().to_string();
                let Some(rest) = name.strip_prefix("card") else {
                    continue;
                };
                if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                if let Ok(mut subdir) = tokio::fs::read_dir(entry.path()).await {
                    while let Ok(Some(pcm_entry)) = subdir.next_entry().await {
                        let pname = pcm_entry.file_name().to_string_lossy().to_string();
                        let Some(pcm_rest) = pname.strip_prefix("pcm") else {
                            continue;
                        };
                        let mut chars = pcm_rest.chars();
                        let digits: String =
                            chars.by_ref().take_while(|c| c.is_ascii_digit()).collect();
                        let dir_char = chars.next().unwrap_or('p');
                        if digits.is_empty() || (dir_char != 'p' && dir_char != 'c') {
                            continue;
                        }
                        let Ok(info_text) =
                            tokio::fs::read_to_string(pcm_entry.path().join("info")).await
                        else {
                            continue;
                        };
                        let Some((card, device, stream, pcm_name, count, avail)) =
                            parse_pcm_info(&info_text)
                        else {
                            continue;
                        };
                        let pcm_type = if stream.is_empty() {
                            if dir_char == 'p' {
                                "playback".to_string()
                            } else {
                                "capture".to_string()
                            }
                        } else {
                            stream
                        };
                        let mut active = false;
                        let sub_count = count.unwrap_or(1).min(8);
                        for sub in 0..sub_count {
                            let hw_path =
                                pcm_entry.path().join(format!("sub{sub}")).join("hw_params");
                            let Ok(hw) = tokio::fs::read_to_string(&hw_path).await else {
                                continue;
                            };
                            if hw.trim().is_empty() {
                                continue;
                            }
                            active = true;
                            let (format, channels, rate, period, buffer) = parse_hw_params(&hw);
                            view.hw_params.push(HwParamsEntry {
                                card,
                                device,
                                pcm_type: pcm_type.clone(),
                                subdevice: sub,
                                format,
                                channels,
                                rate,
                                period_size: period,
                                buffer_size: buffer,
                                raw: hw.trim().to_string(),
                            });
                        }
                        let channels = card_channels.get(&card).and_then(|(p, c)| {
                            if dir_char == 'p' {
                                *p
                            } else {
                                *c
                            }
                        });
                        view.pcm.push(PcmEntry {
                            card,
                            card_id: card_ids.get(&card).cloned().unwrap_or_default(),
                            device,
                            pcm_type,
                            name: pcm_name,
                            subdevices: count,
                            subdevices_available: avail,
                            active,
                            channels,
                        });
                    }
                }
            }
        }
        view.pcm
            .sort_by_key(|p| (p.card, p.device, p.pcm_type.clone()));

        // USB 设备
        if let Ok(mut dir) = tokio::fs::read_dir("/sys/bus/usb/devices").await {
            while let Ok(Some(entry)) = dir.next_entry().await {
                let path = entry.path();
                let Ok(vendor) = tokio::fs::read_to_string(path.join("idVendor")).await else {
                    continue;
                };
                let vendor = vendor.trim().to_string();
                let product_id = tokio::fs::read_to_string(path.join("idProduct"))
                    .await
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                let product = tokio::fs::read_to_string(path.join("product"))
                    .await
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                let manufacturer = tokio::fs::read_to_string(path.join("manufacturer"))
                    .await
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                let speed = tokio::fs::read_to_string(path.join("speed"))
                    .await
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                let bus = tokio::fs::read_to_string(path.join("busnum"))
                    .await
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                let dev = tokio::fs::read_to_string(path.join("devnum"))
                    .await
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                let is_motu = vendor.eq_ignore_ascii_case("07fd")
                    || product.to_ascii_lowercase().contains("motu")
                    || product.to_ascii_lowercase().contains("ultralite");
                let lower = format!("{product} {manufacturer}").to_ascii_lowercase();
                let is_audio = is_motu
                    || lower.contains("audio")
                    || lower.contains("dac")
                    || lower.contains("motu");
                if !is_audio && product.is_empty() && manufacturer.is_empty() {
                    continue;
                }
                view.usb_devices.push(UsbDevice {
                    bus,
                    device: dev,
                    id: format!("{vendor}:{product_id}"),
                    product,
                    manufacturer,
                    speed,
                    is_audio,
                    is_motu,
                });
            }
        }
        view.usb_devices
            .sort_by(|a, b| a.bus.cmp(&b.bus).then(a.device.cmp(&b.device)));
        view.motu_present = view.usb_devices.iter().any(|d| d.is_motu);
        view
    }

    fn pick_device(&self, view: &AlsaView, requested: Option<&str>) -> AppResult<String> {
        if let Some(dev) = requested.map(str::trim).filter(|d| !d.is_empty()) {
            validate_device_arg(dev).map_err(AppError::bad_request)?;
            return Ok(dev.to_string());
        }
        if !self.cfg.default_device.trim().is_empty() {
            return Ok(self.cfg.default_device.trim().to_string());
        }
        let playback: Vec<&PcmEntry> = view
            .pcm
            .iter()
            .filter(|p| p.pcm_type == "playback")
            .collect();
        // 优先 MOTU
        if let Some(p) = playback.iter().find(|p| {
            p.card_id.to_ascii_lowercase().contains("ultralite")
                || p.card_id.to_ascii_lowercase().contains("motu")
        }) {
            return Ok(format!("plughw:{},{}", p.card, p.device));
        }
        if let Some(p) = playback.first() {
            return Ok(format!("plughw:{},{}", p.card, p.device));
        }
        Err(AppError::bad_request("未找到可用的播放设备"))
    }
}

#[async_trait]
impl AlsaBackend for RealAlsa {
    async fn view(&self) -> AlsaView {
        self.scan().await
    }

    async fn test_tone(&self, device: Option<&str>) -> AppResult<ToneResult> {
        let view = self.scan().await;
        let device = self.pick_device(&view, device)?;
        let path = std::env::temp_dir().join("khaen-audio-web-test-tone.wav");
        tokio::fs::write(&path, TEST_TONE_WAV)
            .await
            .map_err(|e| AppError::internal(format!("写入测试音文件失败: {e}")))?;
        let out = self
            .runner
            .run_with_timeout(
                &self.cfg.aplay_bin,
                &["-q", "-D", &device, &path.to_string_lossy()],
                Duration::from_secs(20),
            )
            .await
            .map_err(|e| AppError::unavailable(format!("执行 aplay 失败: {e}")))?;
        let mut result = ToneResult {
            device: device.clone(),
            success: out.success,
            stdout: out.stdout.trim().to_string(),
            stderr: out.stderr.trim().to_string(),
            message: if out.success {
                format!("已在 {device} 播放 1 kHz 测试音（1 秒）")
            } else {
                format!("aplay 播放失败（{device}）")
            },
        };
        if out.success {
            result.message.push_str("；请确认 MOTU 输出通道与音量");
        }
        Ok(result)
    }
}

/// mock 后端。
pub struct MockAlsa {
    cfg: AlsaConfig,
}

impl MockAlsa {
    pub fn new(cfg: AlsaConfig) -> Self {
        Self { cfg }
    }
}

impl Default for MockAlsa {
    fn default() -> Self {
        Self::new(AlsaConfig::default())
    }
}

#[async_trait]
impl AlsaBackend for MockAlsa {
    async fn view(&self) -> AlsaView {
        AlsaView {
            cards: vec![
                AlsaCard {
                    index: 0,
                    id: "PCH".into(),
                    driver: "HDA-Intel".into(),
                    name: "HDA Intel PCH".into(),
                },
                AlsaCard {
                    index: 1,
                    id: "UltraLite".into(),
                    driver: "USB-Audio".into(),
                    name: "MOTU UltraLite-mk5".into(),
                },
            ],
            pcm: vec![
                PcmEntry {
                    card: 0,
                    card_id: "PCH".into(),
                    device: 0,
                    pcm_type: "playback".into(),
                    name: "HDA Intel PCH".into(),
                    subdevices: Some(1),
                    subdevices_available: Some(1),
                    active: false,
                    channels: Some(2),
                },
                PcmEntry {
                    card: 1,
                    card_id: "UltraLite".into(),
                    device: 0,
                    pcm_type: "playback".into(),
                    name: "USB Audio".into(),
                    subdevices: Some(1),
                    subdevices_available: Some(1),
                    active: true,
                    channels: Some(22),
                },
                PcmEntry {
                    card: 1,
                    card_id: "UltraLite".into(),
                    device: 0,
                    pcm_type: "capture".into(),
                    name: "USB Audio".into(),
                    subdevices: Some(1),
                    subdevices_available: Some(1),
                    active: true,
                    channels: Some(22),
                },
            ],
            hw_params: vec![
                HwParamsEntry {
                    card: 1,
                    device: 0,
                    pcm_type: "playback".into(),
                    subdevice: 0,
                    format: Some("S32_LE".into()),
                    channels: Some(24),
                    rate: Some(48000),
                    period_size: Some(1024),
                    buffer_size: Some(8192),
                    raw: "access: MMAP_INTERLEAVED
format: S32_LE
subformat: STD
channels: 24
rate: 48000 (48000/1)
period_size: 1024
buffer_size: 8192"
                        .into(),
                },
                HwParamsEntry {
                    card: 1,
                    device: 0,
                    pcm_type: "capture".into(),
                    subdevice: 0,
                    format: Some("S32_LE".into()),
                    channels: Some(2),
                    rate: Some(48000),
                    period_size: Some(1024),
                    buffer_size: Some(8192),
                    raw: "format: S32_LE\nchannels: 2\nrate: 48000".into(),
                },
            ],
            usb_devices: vec![UsbDevice {
                bus: "1".into(),
                device: "3".into(),
                id: "07fd:0008".into(),
                product: "MOTU UltraLite-mk5".into(),
                manufacturer: "MOTU".into(),
                speed: "480".into(),
                is_audio: true,
                is_motu: true,
            }],
            motu_present: true,
            raw_cards: " 0 [PCH            ]: HDA-Intel - HDA Intel PCH
 1 [UltraLite      ]: USB-Audio - MOTU UltraLite-mk5"
                .into(),
            raw_aplay_l: "**** List of PLAYBACK Hardware Devices ****
card 1: UltraLite [MOTU UltraLite-mk5], device 0: USB Audio [USB Audio]"
                .into(),
            warnings: vec![],
            mock: true,
        }
    }

    async fn test_tone(&self, device: Option<&str>) -> AppResult<ToneResult> {
        let dev = match device.map(str::trim).filter(|d| !d.is_empty()) {
            Some(d) => {
                validate_device_arg(d).map_err(AppError::bad_request)?;
                d.to_string()
            }
            None => {
                if self.cfg.default_device.trim().is_empty() {
                    "plughw:1,0".to_string()
                } else {
                    self.cfg.default_device.trim().to_string()
                }
            }
        };
        Ok(ToneResult {
            device: dev.clone(),
            success: true,
            stdout: String::new(),
            stderr: String::new(),
            message: format!("mock：已在 {dev} 模拟播放 1 kHz 测试音（未真正出声）"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asound_cards_parse() {
        let text = " 0 [PCH            ]: HDA-Intel - HDA Intel PCH
                      HDA Intel PCH at 0xf7f00000 irq 120
 1 [UltraLite      ]: USB-Audio - MOTU UltraLite-mk5
                      MOTU MOTU UltraLite-mk5 at usb-0000:00:14.0-1, high speed
";
        let cards = parse_asound_cards(text);
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[1].index, 1);
        assert_eq!(cards[1].id, "UltraLite");
        assert_eq!(cards[1].driver, "USB-Audio");
        assert_eq!(cards[1].name, "MOTU UltraLite-mk5");
        assert_eq!(cards[0].name, "HDA Intel PCH");
    }

    #[test]
    fn pcm_info_parse() {
        let text = "card: 1
device: 0
subdevice: 0
stream: PLAYBACK
id: USB Audio
name: USB Audio
subdevices_count: 1
subdevices_avail: 1
";
        let (card, device, stream, name, count, avail) = parse_pcm_info(text).unwrap();
        assert_eq!(card, 1);
        assert_eq!(device, 0);
        assert_eq!(stream, "playback");
        assert_eq!(name, "USB Audio");
        assert_eq!(count, Some(1));
        assert_eq!(avail, Some(1));
    }

    #[test]
    fn hw_params_parse() {
        let text = "access: MMAP_INTERLEAVED
format: S32_LE
channels: 24
rate: 48000 (48000/1)
period_size: 1024
buffer_size: 8192";
        let (format, channels, rate, period, buffer) = parse_hw_params(text);
        assert_eq!(format.as_deref(), Some("S32_LE"));
        assert_eq!(channels, Some(24));
        assert_eq!(rate, Some(48000));
        assert_eq!(period, Some(1024));
        assert_eq!(buffer, Some(8192));
    }

    #[test]
    fn stream0_channels_parse() {
        let text = "MOTU UltraLite-mk5 at usb-0000:00:14.0-1, high speed : USB Audio\n\nPlayback:\n  Status: Stop\n  Interface 1\n    Altset 1\n    Format: S32_LE\n    Channels: 22\n  Interface 1\n    Altset 2\n    Format: S16_LE\n    Channels: 24\nCapture:\n  Status: Stop\n  Interface 2\n    Altset 1\n    Channels: 2\n";
        let (p, c) = parse_stream0(text);
        assert_eq!(p, Some(24));
        assert_eq!(c, Some(2));
        assert_eq!(
            default_output_channel_names(4),
            vec!["Main1", "Main2", "Line3", "Line4"]
        );
    }

    #[tokio::test]
    async fn mock_view_has_motu() {
        let alsa = MockAlsa::default();
        let view = alsa.view().await;
        assert!(view.motu_present);
        assert!(view.cards.iter().any(|c| c.name.contains("MOTU")));
        assert!(view.hw_params.iter().any(|h| h.pcm_type == "playback"));
        assert!(view.mock);
    }

    #[tokio::test]
    async fn mock_test_tone() {
        let alsa = MockAlsa::default();
        let r = alsa.test_tone(None).await.unwrap();
        assert!(r.success);
        assert_eq!(r.device, "plughw:1,0");
        let r2 = alsa.test_tone(Some("plughw:1,1")).await.unwrap();
        assert_eq!(r2.device, "plughw:1,1");
        assert!(alsa.test_tone(Some("bad device;rm -rf")).await.is_err());
    }

    #[tokio::test]
    async fn real_pick_device_prefers_motu() {
        let runner = Arc::new(crate::cmd::MockCommandRunner);
        let alsa = RealAlsa::new(AlsaConfig::default(), runner);
        let view = MockAlsa::default().view().await;
        let dev = alsa.pick_device(&view, None).unwrap();
        assert_eq!(dev, "plughw:1,0");
    }
}
