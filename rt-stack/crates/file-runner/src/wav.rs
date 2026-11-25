//! [NONRT] 自有 RIFF/WAVE 解析/解包/写回（零第三方依赖）。
//!
//! T14 DoD 的“自有 file 解析/解包”入口。支持范围（显式白名单，未知一律拒收，不做隐式解释）：
//!
//! | WAVE audioFormat | 位深 | 解码（code -> f32） | 备注 |
//! | --- | --- | --- | --- |
//! | 1 PCM | 8 | `(code - 128) / 128` | 标准 8-bit 无符号偏移二进制；仅文件输入 |
//! | 1 PCM | 16 | `(i16 as f32) / 32768` | 与 T22 `rt-host::format` S16 逐位同语义 |
//! | 1 PCM | 24 | `(sign_extend24 as f32) / 8388608` | 3 字节 packed little-endian |
//! | 1 PCM | 32 | `(i32 as f32) / 2147483648` | **32-bit 全量程**，不是 24-in-32 |
//! | 3 IEEE float | 32 | `f32::from_bits(u32)` | WAV 系数文件/输入的主格式 |
//! | 3 IEEE float | 64 | `(f64 as f32)` | 单次正确舍入到 f32 |
//! | 0xFFFE EXTENSIBLE | 上述 | 仅当 SubFormat GUID = PCM / IEEE_FLOAT | valid_bits 必须 = bits（拒绝隐式移位） |
//!
//! 解码语义与 T22 冻结口径一致：nonfinite -> 0 并计 `nonfinite_in`；`|x| > 1` -> 饱和并计
//! `clipped`；输出恒有限（`nonfinite_out` 应为 0，>0 即不变量破坏）。不做 dither、
//! 不做归一化、不做 DC 去除。
//!
//! 校验：RIFF/WAVE 魔数、chunk 边界/奇数 padding、fmt 字段一致性（block_align == channels ×
//! bytes、byte_rate == fs × block_align）、data 长度必须整帧、fact 与实际帧数一致、通道 1..=32
//! （core §3.1 MAX_INPUT_CHANNELS）、采样率 1..=384000（runtime rate 由 session/compile 再校验）。
//!
//! 写回：[`encode_wav_f32`] 生成确定性 WAV（float32、16 B fmt、无额外 chunk、奇数 padding 规则），
//! 用于 runner 输出与 `khaen-fir/v1` 系数文件（spec/fir-toolbox.md §2.1：mono f32、长度 = taps、
//! 不做归一化）。

use crate::json::Json;
use crate::sha256::sha256_hex;
use crate::{RunnerError, RunnerResult};
use crate::{E_FILE_CHANNELS, E_FILE_FORMAT, E_FILE_FRAMES, E_FILE_RATE, E_FILE_WAV};

/// RIFF 头字节数。
pub const RIFF_HEADER_BYTES: usize = 12;
/// 最大输入通道数（core-v1 §3.1 MAX_INPUT_CHANNELS=32；本模块上限同源）。
pub const MAX_WAV_CHANNELS: u16 = 32;
/// 允许的最大采样率（文件解析层；runtime 只接受 48k/96k，由 compile/session 校验）。
pub const MAX_WAV_SAMPLE_RATE: u32 = 384_000;

/// 支持的文件采样格式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WavFormat {
    /// PCM 8-bit 无符号（偏移二进制）。
    PcmU8,
    /// PCM 16-bit little-endian 二补码。
    Pcm16,
    /// PCM 24-bit little-endian packed 二补码。
    Pcm24,
    /// PCM 32-bit little-endian 二补码（全 32-bit 量程）。
    Pcm32,
    /// IEEE-754 binary32 little-endian。
    Float32,
    /// IEEE-754 binary64 little-endian。
    Float64,
}

impl WavFormat {
    /// 每样本位深。
    pub const fn bits(self) -> u16 {
        match self {
            WavFormat::PcmU8 => 8,
            WavFormat::Pcm16 => 16,
            WavFormat::Pcm24 => 24,
            WavFormat::Pcm32 => 32,
            WavFormat::Float32 => 32,
            WavFormat::Float64 => 64,
        }
    }

    /// 每样本字节数。
    pub const fn bytes(self) -> usize {
        (self.bits() / 8) as usize
    }

    /// manifest/report 用的稳定字面值。
    pub const fn as_str(self) -> &'static str {
        match self {
            WavFormat::PcmU8 => "pcm_u8",
            WavFormat::Pcm16 => "pcm_s16le",
            WavFormat::Pcm24 => "pcm_s24le",
            WavFormat::Pcm32 => "pcm_s32le",
            WavFormat::Float32 => "f32",
            WavFormat::Float64 => "f64",
        }
    }

    /// 是否浮点格式。
    pub const fn is_float(self) -> bool {
        matches!(self, WavFormat::Float32 | WavFormat::Float64)
    }
}

/// 解析后的 WAV 信息（不含样本；data 区位置固定）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WavInfo {
    /// 采样格式。
    pub format: WavFormat,
    /// 通道数。
    pub channels: u16,
    /// 采样率（Hz）。
    pub sample_rate: u32,
    /// 帧数。
    pub frames: u64,
    /// data chunk 在文件中的起始字节。
    pub data_offset: usize,
    /// data chunk 字节数。
    pub data_len: usize,
    /// fact chunk 声明的帧数（若有；必须与 data 计算一致）。
    pub fact_frames: Option<u32>,
}

/// 解码统计（与 T22 `FormatStats` 同语义；报告必须保留）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecodeStats {
    /// 输入 nonfinite 样本数（已 sanitize 为 0）。
    pub nonfinite_in: u64,
    /// 输入幅度 |x| > 1.0 的样本数（已饱和）。
    pub clipped: u64,
    /// 输出 nonfinite 样本数（应为 0）。
    pub nonfinite_out: u64,
}

impl DecodeStats {
    /// 是否无 sanitize/clip/nonfinite 事件。
    pub const fn is_clean(&self) -> bool {
        self.nonfinite_in == 0 && self.clipped == 0 && self.nonfinite_out == 0
    }

    /// 累加（跨块/跨文件汇总）。
    pub fn merge(&mut self, other: DecodeStats) {
        self.nonfinite_in = self.nonfinite_in.saturating_add(other.nonfinite_in);
        self.clipped = self.clipped.saturating_add(other.clipped);
        self.nonfinite_out = self.nonfinite_out.saturating_add(other.nonfinite_out);
    }
}

/// 解码后的音频（planar f32：`planar[channel * frames + frame]`）。
#[derive(Clone, Debug)]
pub struct DecodedAudio {
    /// 解析信息。
    pub info: WavInfo,
    /// planar f32 样本。
    pub planar: Vec<f32>,
    /// 解码统计。
    pub stats: DecodeStats,
}

impl DecodedAudio {
    /// 通道数。
    pub fn channels(&self) -> usize {
        self.info.channels as usize
    }

    /// 帧数。
    pub fn frames(&self) -> usize {
        self.info.frames as usize
    }

    /// 取某通道样本切片。
    pub fn channel(&self, channel: usize) -> Option<&[f32]> {
        let frames = self.frames();
        self.planar.get(channel * frames..(channel + 1) * frames)
    }
}

/// 解析 WAV 头/chunk（不复制样本；拒绝未知格式与不一致字段）。
pub fn parse_wav(bytes: &[u8]) -> RunnerResult<WavInfo> {
    if bytes.len() < RIFF_HEADER_BYTES {
        return Err(wav_error("file shorter than RIFF header"));
    }
    if &bytes[0..4] != b"RIFF" {
        return Err(wav_error("missing RIFF magic"));
    }
    let riff_size = read_u32_le(bytes, 4)? as usize;
    let riff_end = match riff_size.checked_add(8) {
        Some(value) => value,
        None => return Err(wav_error("RIFF size overflows usize")),
    };
    if riff_end > bytes.len() {
        return Err(wav_error("RIFF size exceeds file length"));
    }
    if &bytes[8..12] != b"WAVE" {
        return Err(wav_error("missing WAVE form type"));
    }

    let mut pos = 12usize;
    let mut fmt: Option<FmtChunk> = None;
    let mut data: Option<(usize, usize)> = None;
    let mut fact_frames: Option<u32> = None;
    while pos + 8 <= riff_end {
        let id = &bytes[pos..pos + 4];
        let size = read_u32_le(bytes, pos + 4)? as usize;
        let body_start = pos + 8;
        let body_end = match body_start.checked_add(size) {
            Some(value) => value,
            None => return Err(wav_error("chunk size overflows usize")),
        };
        if body_end > riff_end {
            return Err(wav_error("chunk body exceeds RIFF end"));
        }
        match id {
            b"fmt " => {
                if fmt.is_some() {
                    return Err(wav_error("duplicate fmt chunk"));
                }
                fmt = Some(parse_fmt_chunk(bytes, body_start, size)?);
            }
            b"data" => {
                if data.is_some() {
                    return Err(wav_error("multiple data chunks are not supported"));
                }
                data = Some((body_start, size));
            }
            b"fact" => {
                if size < 4 {
                    return Err(wav_error("fact chunk shorter than 4 bytes"));
                }
                fact_frames = Some(read_u32_le(bytes, body_start)?);
            }
            _ => {}
        }
        pos = body_end + (size & 1);
    }

    let fmt = fmt.ok_or_else(|| wav_error("missing fmt chunk"))?;
    let (data_offset, data_len) = data.ok_or_else(|| wav_error("missing data chunk"))?;
    if fmt.channels == 0 || fmt.channels > MAX_WAV_CHANNELS {
        return Err(wav_error("channel count outside 1..=32"));
    }
    if fmt.sample_rate == 0 || fmt.sample_rate > MAX_WAV_SAMPLE_RATE {
        return Err(wav_error("sample rate outside 1..=384000"));
    }
    if fmt.block_align as usize != fmt.channels as usize * fmt.format.bytes() {
        return Err(wav_error("block_align != channels * bytes_per_sample"));
    }
    let expected_rate = (fmt.sample_rate as u64) * (fmt.block_align as u64);
    if fmt.byte_rate as u64 != expected_rate {
        return Err(wav_error("byte_rate != sample_rate * block_align"));
    }
    let frame_bytes = fmt.block_align as usize;
    if frame_bytes == 0 || data_len % frame_bytes != 0 {
        return Err(RunnerError::new(
            E_FILE_FRAMES,
            format!(
                "data length {} is not a multiple of frame bytes {}",
                data_len, frame_bytes
            ),
        ));
    }
    let frames = (data_len / frame_bytes) as u64;
    if let Some(fact) = fact_frames {
        if fact as u64 != frames {
            return Err(RunnerError::new(
                E_FILE_FRAMES,
                format!("fact frames {} != data frames {}", fact, frames),
            ));
        }
    }
    Ok(WavInfo {
        format: fmt.format,
        channels: fmt.channels,
        sample_rate: fmt.sample_rate,
        frames,
        data_offset,
        data_len,
        fact_frames,
    })
}

struct FmtChunk {
    format: WavFormat,
    channels: u16,
    sample_rate: u32,
    byte_rate: u32,
    block_align: u16,
}

fn parse_fmt_chunk(bytes: &[u8], offset: usize, size: usize) -> RunnerResult<FmtChunk> {
    if size < 16 {
        return Err(wav_error("fmt chunk shorter than 16 bytes"));
    }
    let tag = read_u16_le(bytes, offset)?;
    let channels = read_u16_le(bytes, offset + 2)?;
    let sample_rate = read_u32_le(bytes, offset + 4)?;
    let byte_rate = read_u32_le(bytes, offset + 8)?;
    let block_align = read_u16_le(bytes, offset + 12)?;
    let bits = read_u16_le(bytes, offset + 14)?;

    let (format, valid_bits) = match tag {
        1 => (pcm_format(bits)?, None),
        3 => (float_format(bits)?, None),
        0xFFFE => {
            if size < 40 {
                return Err(wav_error("extensible fmt chunk shorter than 40 bytes"));
            }
            let cb_size = read_u16_le(bytes, offset + 16)?;
            if cb_size < 22 {
                return Err(wav_error("extensible cbSize < 22"));
            }
            let valid = read_u16_le(bytes, offset + 18)?;
            let guid = &bytes[offset + 24..offset + 40];
            let guid_tail = [
                0x00u8, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
                0x71,
            ];
            if guid[2..] != guid_tail {
                return Err(RunnerError::new(
                    E_FILE_FORMAT,
                    "extensible subformat GUID is not PCM/IEEE_FLOAT",
                ));
            }
            let sub_tag = read_u16_le(bytes, offset + 24)?;
            let fmt = match sub_tag {
                1 => pcm_format(bits)?,
                3 => float_format(bits)?,
                other => {
                    return Err(RunnerError::new(
                        E_FILE_FORMAT,
                        format!("unsupported extensible subformat tag {}", other),
                    ))
                }
            };
            (fmt, Some(valid))
        }
        other => {
            return Err(RunnerError::new(
                E_FILE_FORMAT,
                format!("unsupported WAVE audioFormat {}", other),
            ))
        }
    };
    if let Some(valid) = valid_bits {
        if valid != 0 && valid != bits {
            return Err(RunnerError::new(
                E_FILE_FORMAT,
                format!(
                    "valid_bits {} != container bits {} (24-in-32 is not supported)",
                    valid, bits
                ),
            ));
        }
    }
    Ok(FmtChunk {
        format,
        channels,
        sample_rate,
        byte_rate,
        block_align,
    })
}

fn pcm_format(bits: u16) -> RunnerResult<WavFormat> {
    match bits {
        8 => Ok(WavFormat::PcmU8),
        16 => Ok(WavFormat::Pcm16),
        24 => Ok(WavFormat::Pcm24),
        32 => Ok(WavFormat::Pcm32),
        other => Err(RunnerError::new(
            E_FILE_FORMAT,
            format!("unsupported PCM bit depth {}", other),
        )),
    }
}

fn float_format(bits: u16) -> RunnerResult<WavFormat> {
    match bits {
        32 => Ok(WavFormat::Float32),
        64 => Ok(WavFormat::Float64),
        other => Err(RunnerError::new(
            E_FILE_FORMAT,
            format!("unsupported IEEE-float bit depth {}", other),
        )),
    }
}

/// 解析并解包为 planar f32（固定精度语义见模块文档）。
pub fn decode_wav(bytes: &[u8]) -> RunnerResult<DecodedAudio> {
    let info = parse_wav(bytes)?;
    let channels = info.channels as usize;
    let frames = info.frames as usize;
    let total = match channels.checked_mul(frames) {
        Some(value) => value,
        None => return Err(wav_error("sample count overflows usize")),
    };
    let mut planar = vec![0.0f32; total];
    let mut stats = DecodeStats::default();
    let bytes_per = info.format.bytes();
    let frame_bytes = channels * bytes_per;
    let body = &bytes[info.data_offset..info.data_offset + info.data_len];
    for frame in 0..frames {
        for channel in 0..channels {
            let offset = match frame
                .checked_mul(frame_bytes)
                .and_then(|value| value.checked_add(channel * bytes_per))
            {
                Some(value) => value,
                None => return Err(wav_error("sample offset overflow")),
            };
            let mut sample = decode_sample(info.format, body, offset)?;
            if !sample.is_finite() {
                stats.nonfinite_in = stats.nonfinite_in.saturating_add(1);
                sample = 0.0;
            }
            if sample > 1.0 {
                sample = 1.0;
                stats.clipped = stats.clipped.saturating_add(1);
            } else if sample < -1.0 {
                sample = -1.0;
                stats.clipped = stats.clipped.saturating_add(1);
            }
            if !sample.is_finite() {
                stats.nonfinite_out = stats.nonfinite_out.saturating_add(1);
                sample = 0.0;
            }
            planar[channel * frames + frame] = sample;
        }
    }
    Ok(DecodedAudio {
        info,
        planar,
        stats,
    })
}

fn decode_sample(format: WavFormat, body: &[u8], offset: usize) -> RunnerResult<f32> {
    Ok(match format {
        WavFormat::PcmU8 => (body[offset] as f32 - 128.0) / 128.0,
        WavFormat::Pcm16 => {
            (i16::from_le_bytes([body[offset], body[offset + 1]]) as f32) / 32_768.0
        }
        WavFormat::Pcm24 => {
            let raw = (body[offset] as u32)
                | ((body[offset + 1] as u32) << 8)
                | ((body[offset + 2] as u32) << 16);
            let signed = ((raw << 8) as i32) >> 8;
            (signed as f32) / 8_388_608.0
        }
        WavFormat::Pcm32 => (read_i32_le(body, offset)? as f32) / 2_147_483_648.0,
        WavFormat::Float32 => f32::from_bits(read_u32_le(body, offset)?),
        WavFormat::Float64 => {
            let bits = read_u64_le(body, offset)?;
            f64::from_bits(bits) as f32
        }
    })
}

/// 编码 planar f32 为确定性 WAV（WAVE_FORMAT_IEEE_FLOAT，32-bit，interleaved）。
///
/// 用于：runner 输出 WAV；spec/fir-toolbox.md §2.1 的 `wav_f32_mono` 系数文件（channels=1）。
/// 不写 fact/LIST 等额外 chunk；采样率 <= 384000、通道 1..=32、帧数 <= u32::MAX。
pub fn encode_wav_f32(planar: &[f32], channels: u16, sample_rate: u32) -> RunnerResult<Vec<u8>> {
    if channels == 0 || channels > MAX_WAV_CHANNELS {
        return Err(RunnerError::new(
            E_FILE_CHANNELS,
            format!("encode: channels {} outside 1..=32", channels),
        ));
    }
    if sample_rate == 0 || sample_rate > MAX_WAV_SAMPLE_RATE {
        return Err(RunnerError::new(
            E_FILE_RATE,
            format!("encode: sample rate {} outside 1..=384000", sample_rate),
        ));
    }
    let channels_usize = channels as usize;
    if planar.len() % channels_usize != 0 {
        return Err(RunnerError::new(
            E_FILE_FRAMES,
            format!(
                "encode: planar len {} is not a multiple of channels {}",
                planar.len(),
                channels
            ),
        ));
    }
    let frames = planar.len() / channels_usize;
    let frame_bytes = channels_usize * 4;
    let data_len = match frames.checked_mul(frame_bytes) {
        Some(value) => value,
        None => {
            return Err(RunnerError::new(
                E_FILE_FRAMES,
                "encode: data size overflow",
            ))
        }
    };
    if data_len > u32::MAX as usize {
        return Err(RunnerError::new(
            E_FILE_FRAMES,
            "encode: data chunk exceeds u32 size",
        ));
    }
    let block_align = frame_bytes as u16;
    let byte_rate = match (sample_rate as u64).checked_mul(block_align as u64) {
        Some(value) if value <= u32::MAX as u64 => value as u32,
        _ => return Err(RunnerError::new(E_FILE_RATE, "encode: byte_rate overflow")),
    };

    let fmt_chunk_len = 16usize;
    let pad = data_len & 1;
    let riff_payload = 4 + (8 + fmt_chunk_len) + (8 + data_len + pad);
    let total = 8 + riff_payload;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(riff_payload as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&(fmt_chunk_len as u32).to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for frame in 0..frames {
        for channel in 0..channels_usize {
            let sample = planar[channel * frames + frame];
            let bits = if sample.is_finite() {
                sample.to_bits()
            } else {
                0
            };
            out.extend_from_slice(&bits.to_le_bytes());
        }
    }
    if pad == 1 {
        out.push(0);
    }
    Ok(out)
}

/// 计算文件字节的 SHA-256（小写 hex；manifest/wav 校验用）。
pub fn file_sha256(bytes: &[u8]) -> String {
    sha256_hex(bytes)
}

/// 解码浮点 WAV 的原始样本（f32/f64；不做 T22 设备 clip/sanitize）。
///
/// 用于 `khaen-fir/v1` 系数文件（mono f32、值可 >1.0，不能被设备 clip 语义破坏）；返回
/// 未修改的样本值（非有限保留，由调用方拒绝）。仅接受 IEEE-float 格式。
pub fn decode_float32_samples(bytes: &[u8]) -> RunnerResult<(WavInfo, Vec<f32>)> {
    let info = parse_wav(bytes)?;
    if !info.format.is_float() {
        return Err(RunnerError::new(
            E_FILE_FORMAT,
            format!("expected IEEE-float WAV (got {})", info.format.as_str()),
        ));
    }
    let channels = info.channels as usize;
    let frames = info.frames as usize;
    let total = match channels.checked_mul(frames) {
        Some(value) => value,
        None => return Err(wav_error("sample count overflows usize")),
    };
    let bytes_per = info.format.bytes();
    let frame_bytes = channels * bytes_per;
    let body = &bytes[info.data_offset..info.data_offset + info.data_len];
    let mut samples = Vec::with_capacity(total);
    for frame in 0..frames {
        for channel in 0..channels {
            let offset = frame * frame_bytes + channel * bytes_per;
            samples.push(decode_sample(info.format, body, offset)?);
        }
    }
    Ok((info, samples))
}

/// 把 WavInfo 写成报告用 JSON 对象。
pub fn info_json(info: &WavInfo) -> Json {
    Json::from_entries(vec![
        ("format", Json::String(info.format.as_str().to_string())),
        ("bits_per_sample", Json::Number(info.format.bits() as f64)),
        ("channels", Json::Number(info.channels as f64)),
        ("sample_rate_hz", Json::Number(info.sample_rate as f64)),
        ("frames", Json::Number(info.frames as f64)),
    ])
}

fn wav_error(message: impl Into<String>) -> RunnerError {
    RunnerError::new(E_FILE_WAV, message)
}

fn read_u16_le(bytes: &[u8], offset: usize) -> RunnerResult<u16> {
    let slice = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| wav_error("truncated 16-bit field"))?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> RunnerResult<u32> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| wav_error("truncated 32-bit field"))?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_i32_le(bytes: &[u8], offset: usize) -> RunnerResult<i32> {
    Ok(read_u32_le(bytes, offset)? as i32)
}

fn read_u64_le(bytes: &[u8], offset: usize) -> RunnerResult<u64> {
    let slice = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| wav_error("truncated 64-bit field"))?;
    Ok(u64::from_le_bytes([
        slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
    ]))
}
