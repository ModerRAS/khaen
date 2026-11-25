//! WAV 读取（RIFF/WAVE）：给实测电平与闭环复测运行包共用。
//!
//! 支持 PCM 16/24/32 bit 与 IEEE float 32 bit；声道数 1..=8 按算术平均混成单声道
//! （相对电平与混音的固定比例无关）。只读文件，不做重采样、不猜测格式。

use std::path::Path;

/// 输入内容错误（不是合法 WAV / 不支持的格式）。
pub const BAD_INPUT: &str = "E_WAV_INPUT";
/// 文件读取错误。
pub const IO: &str = "E_WAV_IO";

/// WAV 读取错误（稳定码 + 人读消息）。
#[derive(Clone, Debug, PartialEq)]
pub struct WavError {
    code: &'static str,
    message: String,
}

impl WavError {
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

impl std::fmt::Display for WavError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for WavError {}

/// 解码后的 WAV：采样率、源声道数与单声道样本。
#[derive(Clone, Debug, PartialEq)]
pub struct Wav {
    /// 采样率（Hz）。
    pub sample_rate_hz: f64,
    /// 源文件声道数（1..=8）。
    pub channels: u16,
    /// 单声道样本（多声道已算术平均）。
    pub samples: Vec<f64>,
}

/// 读文件并解析。
pub fn read_wav_file(path: &Path) -> Result<Wav, WavError> {
    let bytes = std::fs::read(path)
        .map_err(|err| WavError::new(IO, format!("read {}: {err}", path.display())))?;
    parse_wav(&bytes)
        .map_err(|err| WavError::new(err.code, format!("{}: {}", path.display(), err.message)))
}

/// 解析 RIFF/WAVE 字节。
pub fn parse_wav(bytes: &[u8]) -> Result<Wav, WavError> {
    if bytes.len() < 44 {
        return Err(WavError::new(BAD_INPUT, "file is too short to be a WAV"));
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(WavError::new(BAD_INPUT, "not a RIFF/WAVE file"));
    }
    let mut format: Option<(u16, u16, u32, u16, u16)> = None;
    let mut data: Option<(usize, usize)> = None;
    let mut offset = 12usize;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = read_u32(bytes, offset + 4) as usize;
        let payload_start = offset + 8;
        let payload_end = payload_start
            .checked_add(size)
            .ok_or_else(|| WavError::new(BAD_INPUT, "WAV chunk size overflow"))?;
        if payload_end > bytes.len() {
            return Err(WavError::new(
                BAD_INPUT,
                "WAV chunk extends past end of file",
            ));
        }
        if id == b"fmt " {
            if size < 16 {
                return Err(WavError::new(BAD_INPUT, "WAV fmt chunk is too short"));
            }
            let format_tag = read_u16(bytes, payload_start);
            let channels = read_u16(bytes, payload_start + 2);
            let sample_rate = read_u32(bytes, payload_start + 4);
            let block_align = read_u16(bytes, payload_start + 12);
            let bits = read_u16(bytes, payload_start + 14);
            format = Some((format_tag, channels, sample_rate, block_align, bits));
        } else if id == b"data" {
            data = Some((payload_start, size));
        }
        offset = payload_end + (size % 2);
    }
    let (format_tag, channels, sample_rate, block_align, bits) =
        format.ok_or_else(|| WavError::new(BAD_INPUT, "WAV has no fmt chunk"))?;
    let (data_start, data_size) =
        data.ok_or_else(|| WavError::new(BAD_INPUT, "WAV has no data chunk"))?;
    let bytes_per_sample = match (format_tag, bits) {
        (1, 16) => 2usize,
        (1, 24) => 3usize,
        (1, 32) => 4usize,
        (3, 32) => 4usize,
        (tag, bits) => {
            return Err(WavError::new(
                BAD_INPUT,
                format!(
                    "unsupported WAV format tag {tag} / {bits} bit (PCM 16/24/32 or float 32 only)"
                ),
            ))
        }
    };
    if !(1..=8).contains(&channels) {
        return Err(WavError::new(
            BAD_INPUT,
            format!("unsupported channel count {channels} (1..=8)"),
        ));
    }
    if sample_rate == 0 {
        return Err(WavError::new(BAD_INPUT, "WAV sample rate is zero"));
    }
    let expected_align = u32::from(channels) * bytes_per_sample as u32;
    let block_align = if block_align == 0 {
        expected_align as u16
    } else {
        block_align
    };
    if u32::from(block_align) != expected_align {
        return Err(WavError::new(
            BAD_INPUT,
            format!("WAV block align {block_align} does not match {channels} channels x {bytes_per_sample} bytes"),
        ));
    }
    let frames = data_size / usize::from(block_align);
    let mut samples = Vec::with_capacity(frames);
    for frame in 0..frames {
        let mut sum = 0.0f64;
        for channel in 0..usize::from(channels) {
            let sample_start =
                data_start + frame * usize::from(block_align) + channel * bytes_per_sample;
            sum += decode_sample(
                &bytes[sample_start..sample_start + bytes_per_sample],
                format_tag,
                bits,
            );
        }
        samples.push(sum / f64::from(channels));
    }
    Ok(Wav {
        sample_rate_hz: f64::from(sample_rate),
        channels,
        samples,
    })
}

fn decode_sample(bytes: &[u8], format_tag: u16, bits: u16) -> f64 {
    match (format_tag, bits) {
        (1, 16) => f64::from(i16::from_le_bytes([bytes[0], bytes[1]])) / 32_768.0,
        (1, 24) => {
            let raw =
                i32::from(bytes[0]) | (i32::from(bytes[1]) << 8) | (i32::from(bytes[2]) << 16);
            let signed = if raw & 0x0080_0000 != 0 {
                raw | !0x00FF_FFFF
            } else {
                raw
            };
            f64::from(signed) / 8_388_608.0
        }
        (1, 32) => {
            f64::from(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                / 2_147_483_648.0
        }
        (3, 32) => f64::from(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
        _ => 0.0,
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}
