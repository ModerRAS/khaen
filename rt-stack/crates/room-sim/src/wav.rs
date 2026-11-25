//! f32 单声道 RIFF/WAVE 读写（44 字节标准头）。读端容错未知 chunk、奇数字节对齐与
//! WAVE_FORMAT_EXTENSIBLE 的额外 fmt 字节；写端与 RoomForge AudioOptimizer.IO.WavFile.Write 的 float32
//! 路径一致（样本 clamp 到 [-1,1]）。所有失败都返回 Err(String)，不 panic、不返回半截数据。

use std::path::Path;

#[cfg_attr(not(test), allow(dead_code))]
const FORMAT_PCM: u16 = 1;
const FORMAT_IEEE_FLOAT: u16 = 3;
const FORMAT_EXTENSIBLE: u16 = 0xfffe;

/// Decoded WAV: sample rate, channel count and de-interleaved f32 samples in native [-1, 1] scale.
#[derive(Clone, Debug, PartialEq)]
pub struct WavData {
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
}

/// 读一个 f32 单声道 WAV 文件；失败（含截断/非 f32/多声道）返回 Err。
pub fn read_f32_mono(path: &Path) -> Result<WavData, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("cannot read WAV file {}: {error}", path.display()))?;
    decode_f32_mono(&bytes).map_err(|error| format!("invalid WAV file {}: {error}", path.display()))
}

/// 解析 RIFF/WAVE 字节；只接受 IEEE float 32-bit 单声道。
pub fn decode_f32_mono(bytes: &[u8]) -> Result<WavData, String> {
    if bytes.len() < 12 {
        return Err(format!(
            "not a WAV file: only {} bytes long (need at least 12)",
            bytes.len()
        ));
    }
    if &bytes[0..4] != b"RIFF" {
        return Err("not a RIFF file (missing 'RIFF' id)".to_string());
    }
    if &bytes[8..12] != b"WAVE" {
        return Err("not a WAVE file (missing 'WAVE' id)".to_string());
    }

    let mut position = 12usize;
    let mut format: Option<FormatChunk> = None;
    let mut data: Option<&[u8]> = None;

    while position + 8 <= bytes.len() {
        let chunk_id = &bytes[position..position + 4];
        let chunk_size = u32::from_le_bytes([
            bytes[position + 4],
            bytes[position + 5],
            bytes[position + 6],
            bytes[position + 7],
        ]) as usize;
        position += 8;
        let remaining = bytes.len() - position;
        if chunk_size > remaining {
            return Err(format!(
                "truncated WAV: chunk '{}' declares {chunk_size} bytes but only {remaining} remain",
                chunk_tag(chunk_id)
            ));
        }
        let body = &bytes[position..position + chunk_size];

        if chunk_id == b"fmt " {
            format = Some(parse_format_chunk(body)?);
        } else if chunk_id == b"data" {
            // 与 C# 读端一致：多个 data chunk 时最后一个生效（正常文件只有一个）。
            data = Some(body);
        }
        // 其他 chunk（LIST/fact/bext/JUNK…）一律跳过，不解释。

        position += chunk_size;
        if chunk_size % 2 == 1 {
            position += 1; // RIFF chunk 按 word 对齐
        }
        if position > bytes.len() {
            break;
        }
    }

    let format = format.ok_or_else(|| "no fmt chunk found".to_string())?;
    let data = data.ok_or_else(|| "no data chunk found".to_string())?;

    if format.channels == 0 {
        return Err("fmt chunk declares 0 channels".to_string());
    }
    if format.sample_rate_hz == 0 {
        return Err("fmt chunk declares 0 Hz sample rate".to_string());
    }
    if format.bits_per_sample != 32 {
        return Err(format!(
            "unsupported WAV sample width: {} bits (only 32-bit IEEE float is read)",
            format.bits_per_sample
        ));
    }
    if format.audio_format != FORMAT_IEEE_FLOAT {
        return Err(format!(
            "unsupported WAV format code {} (only IEEE float, code 3, is read; PCM 16/24-bit is not)",
            format.audio_format
        ));
    }
    if format.channels != 1 {
        return Err(format!(
            "unsupported channel count {} (only mono f32 is read)",
            format.channels
        ));
    }
    if data.len() % 4 != 0 {
        return Err(format!(
            "data chunk length {} is not a whole number of 4-byte f32 samples",
            data.len()
        ));
    }

    let mut samples = Vec::with_capacity(data.len() / 4);
    for chunk in data.chunks_exact(4) {
        samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }

    Ok(WavData {
        sample_rate_hz: format.sample_rate_hz,
        channels: 1,
        samples,
    })
}

/// 编码 44 字节标准头（RIFF/WAVE/fmt(16)/data）的 f32 单声道 WAV 字节。
///
/// # Panics
/// sample_rate_hz == 0 时 panic（无法写出合法的 fmt chunk）。
pub fn encode_f32_mono(samples: &[f32], sample_rate_hz: u32) -> Vec<u8> {
    assert!(sample_rate_hz > 0, "WAV sample rate must be > 0 Hz");
    let data_bytes = samples.len() * 4;
    let mut out = Vec::with_capacity(44 + data_bytes);

    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data_bytes) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&FORMAT_IEEE_FLOAT.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // channels
    out.extend_from_slice(&sample_rate_hz.to_le_bytes());
    out.extend_from_slice(&(sample_rate_hz * 4).to_le_bytes()); // byte rate
    out.extend_from_slice(&4u16.to_le_bytes()); // block align
    out.extend_from_slice(&32u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_bytes as u32).to_le_bytes());

    for sample in samples {
        // 与 RoomForge WavFile.Write 一致：clamp 到 [-1,1] 再写。
        out.extend_from_slice(&sample.clamp(-1.0, 1.0).to_le_bytes());
    }
    out
}

/// 写 44 字节标准头的 f32 单声道 WAV 文件。
pub fn write_f32_mono(path: &Path, samples: &[f32], sample_rate_hz: u32) -> Result<(), String> {
    if sample_rate_hz == 0 {
        return Err("WAV sample rate must be > 0 Hz".to_string());
    }
    let bytes = encode_f32_mono(samples, sample_rate_hz);
    std::fs::write(path, bytes)
        .map_err(|error| format!("cannot write WAV file {}: {error}", path.display()))
}

struct FormatChunk {
    audio_format: u16,
    channels: u16,
    sample_rate_hz: u32,
    bits_per_sample: u16,
}

fn parse_format_chunk(body: &[u8]) -> Result<FormatChunk, String> {
    if body.len() < 16 {
        return Err(format!(
            "fmt chunk is {} bytes; a PCM/float header needs at least 16",
            body.len()
        ));
    }
    let mut audio_format = u16::from_le_bytes([body[0], body[1]]);
    let channels = u16::from_le_bytes([body[2], body[3]]);
    let sample_rate_hz = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
    let bits_per_sample = u16::from_le_bytes([body[14], body[15]]);

    if audio_format == FORMAT_EXTENSIBLE {
        // WAVE_FORMAT_EXTENSIBLE: fmt chunk 40 字节，真正格式在 SubFormat GUID 前 2 字节。
        if body.len() < 26 {
            return Err(format!(
                "WAVE_FORMAT_EXTENSIBLE fmt chunk is {} bytes; need at least 26",
                body.len()
            ));
        }
        audio_format = u16::from_le_bytes([body[24], body[25]]);
    }

    Ok(FormatChunk {
        audio_format,
        channels,
        sample_rate_hz,
        bits_per_sample,
    })
}

fn chunk_tag(id: &[u8]) -> String {
    String::from_utf8_lossy(id).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// R:\Temp 在本机不可写；先探测 current_dir（cargo test 的包根），失败再退回 temp_dir。
    fn temp_path(name: &str) -> PathBuf {
        let file_name = format!(
            "tspi-room-sim-wav-{}-{}-{name}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        );
        let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let candidate = base.join(&file_name);
        match std::fs::write(&candidate, b"probe") {
            Ok(()) => {
                let _ = std::fs::remove_file(&candidate);
                candidate
            }
            Err(_) => std::env::temp_dir().join(file_name),
        }
    }

    #[test]
    fn write_then_read_roundtrips_exactly() {
        let samples: Vec<f32> = vec![0.0, 0.5, -0.25, 1.0, -1.0, 0.123_456_79, -1e-7];
        let path = temp_path("roundtrip");
        write_f32_mono(&path, &samples, 48_000).unwrap();
        let decoded = read_f32_mono(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(decoded.sample_rate_hz, 48_000);
        assert_eq!(decoded.channels, 1);
        assert_eq!(decoded.samples, samples);
        assert_eq!(decoded.samples.len(), 7);
    }

    #[test]
    fn encoded_header_is_canonical_44_bytes() {
        let samples = vec![0.25f32; 10];
        let bytes = encode_f32_mono(&samples, 48_000);
        assert_eq!(bytes.len(), 44 + 40);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(
            u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            76
        );
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[12..16], b"fmt ");
        assert_eq!(
            u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
            16
        );
        assert_eq!(u16::from_le_bytes([bytes[20], bytes[21]]), 3);
        assert_eq!(u16::from_le_bytes([bytes[22], bytes[23]]), 1);
        assert_eq!(
            u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]),
            48_000
        );
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(
            u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]),
            40
        );
    }

    #[test]
    fn samples_are_clamped_like_roomforge() {
        let bytes = encode_f32_mono(&[2.0, -3.0], 48_000);
        let decoded = decode_f32_mono(&bytes).unwrap();
        assert_eq!(decoded.samples, vec![1.0, -1.0]);
    }

    #[test]
    fn decode_tolerates_unknown_and_odd_sized_chunks() {
        let base = encode_f32_mono(&[0.1, -0.2, 0.3], 48_000);
        let mut custom = Vec::new();
        custom.extend_from_slice(&base[0..36]); // RIFF 头 + fmt chunk
        custom.extend_from_slice(b"JUNK"); // 奇数字节 chunk
        custom.extend_from_slice(&3u32.to_le_bytes());
        custom.extend_from_slice(&[0xaa, 0xbb, 0xcc]);
        custom.push(0x00); // word 对齐填充
        custom.extend_from_slice(b"LIST");
        custom.extend_from_slice(&0u32.to_le_bytes());
        custom.extend_from_slice(&base[36..]); // data chunk + 样本
        let decoded = decode_f32_mono(&custom).unwrap();
        assert_eq!(decoded.samples, vec![0.1f32, -0.2, 0.3]);
        assert_eq!(decoded.sample_rate_hz, 48_000);
    }

    #[test]
    fn decode_accepts_wave_format_extensible_float() {
        let samples = [0.5f32, -0.5, 0.25];
        let data_bytes = 12u32;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(4u32 + 8 + 40 + 8 + data_bytes).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&40u32.to_le_bytes());
        bytes.extend_from_slice(&FORMAT_EXTENSIBLE.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&48_000u32.to_le_bytes());
        bytes.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&32u16.to_le_bytes());
        bytes.extend_from_slice(&22u16.to_le_bytes()); // cbSize
        bytes.extend_from_slice(&32u16.to_le_bytes()); // valid bits
        bytes.extend_from_slice(&4u32.to_le_bytes()); // channel mask
        bytes.extend_from_slice(&FORMAT_IEEE_FLOAT.to_le_bytes()); // SubFormat GUID 前 2 字节
        bytes.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
        ]);
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_bytes.to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        let decoded = decode_f32_mono(&bytes).unwrap();
        assert_eq!(decoded.samples, samples.to_vec());
    }

    #[test]
    fn rejects_malformed_files() {
        assert!(decode_f32_mono(b"RIFF").is_err());
        assert!(decode_f32_mono(b"XXXX0000WAVE").is_err());
        // 只有头没有 chunk
        let mut only_header = Vec::new();
        only_header.extend_from_slice(b"RIFF");
        only_header.extend_from_slice(&0u32.to_le_bytes());
        only_header.extend_from_slice(b"WAVE");
        assert!(decode_f32_mono(&only_header).is_err());

        // data chunk 声明长度超过文件
        let mut truncated = encode_f32_mono(&[0.0; 4], 48_000);
        truncated[40] = 0xff; // 放大 data chunk size 的第一个字节（偏移 40 是 data size）
        assert!(decode_f32_mono(&truncated).is_err());

        // PCM 16-bit 拒绝
        let mut pcm = Vec::new();
        pcm.extend_from_slice(b"RIFF");
        pcm.extend_from_slice(&36u32.to_le_bytes());
        pcm.extend_from_slice(b"WAVE");
        pcm.extend_from_slice(b"fmt ");
        pcm.extend_from_slice(&16u32.to_le_bytes());
        pcm.extend_from_slice(&FORMAT_PCM.to_le_bytes());
        pcm.extend_from_slice(&1u16.to_le_bytes());
        pcm.extend_from_slice(&48_000u32.to_le_bytes());
        pcm.extend_from_slice(&96_000u32.to_le_bytes());
        pcm.extend_from_slice(&2u16.to_le_bytes());
        pcm.extend_from_slice(&16u16.to_le_bytes());
        pcm.extend_from_slice(b"data");
        pcm.extend_from_slice(&2u32.to_le_bytes());
        pcm.extend_from_slice(&[0, 0]);
        assert!(decode_f32_mono(&pcm).is_err());

        assert!(read_f32_mono(Path::new("definitely-missing-room-sim.wav")).is_err());
    }
}
