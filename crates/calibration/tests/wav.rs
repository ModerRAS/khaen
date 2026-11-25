//! WAV 解析：位深/声道/坏文件/奇数块填充。

use khaen_calibration::wav::{parse_wav, BAD_INPUT};

fn wav_bytes(sample_rate: u32, channels: u16, bits: u16, format_tag: u16, data: &[u8]) -> Vec<u8> {
    let block_align = channels * bits / 8;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&format_tag.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    let byte_rate = sample_rate * u32::from(block_align);
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&bits.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(data);
    bytes
}

fn pcm16(samples: &[f64]) -> Vec<u8> {
    let mut data = Vec::new();
    for sample in samples {
        let value = (sample * 32_767.0).round().clamp(-32_768.0, 32_767.0) as i16;
        data.extend_from_slice(&value.to_le_bytes());
    }
    data
}

fn pcm24(samples: &[f64]) -> Vec<u8> {
    let mut data = Vec::new();
    for sample in samples {
        let value = (sample * 8_388_607.0)
            .round()
            .clamp(-8_388_608.0, 8_388_607.0) as i32;
        data.extend_from_slice(&value.to_le_bytes()[..3]);
    }
    data
}

fn pcm32(samples: &[f64]) -> Vec<u8> {
    let mut data = Vec::new();
    for sample in samples {
        let value = (sample * 2_147_483_647.0)
            .round()
            .clamp(-2_147_483_648.0, 2_147_483_647.0) as i32;
        data.extend_from_slice(&value.to_le_bytes());
    }
    data
}

fn float32(samples: &[f64]) -> Vec<u8> {
    let mut data = Vec::new();
    for sample in samples {
        data.extend_from_slice(&(*sample as f32).to_le_bytes());
    }
    data
}

#[test]
fn parses_mono_16_bit() {
    let data = pcm16(&[0.0, 0.5, -0.5]);
    let bytes = wav_bytes(48_000, 1, 16, 1, &data);
    let wav = parse_wav(&bytes).expect("wav");
    assert_eq!(wav.sample_rate_hz, 48_000.0);
    assert_eq!(wav.channels, 1);
    assert_eq!(wav.samples.len(), 3);
    assert!((wav.samples[1] - 0.5).abs() <= 1e-4);
    assert!((wav.samples[2] + 0.5).abs() <= 1e-4);
}

#[test]
fn parses_24_and_32_bit_pcm() {
    let bytes = wav_bytes(96_000, 1, 24, 1, &pcm24(&[0.25, -0.25]));
    let wav = parse_wav(&bytes).expect("24 bit");
    assert_eq!(wav.sample_rate_hz, 96_000.0);
    assert!((wav.samples[0] - 0.25).abs() <= 1e-5);
    assert!((wav.samples[1] + 0.25).abs() <= 1e-5);

    let bytes = wav_bytes(48_000, 1, 32, 1, &pcm32(&[0.5, -1.0]));
    let wav = parse_wav(&bytes).expect("32 bit");
    assert!((wav.samples[0] - 0.5).abs() <= 1e-6);
    assert!((wav.samples[1] + 1.0).abs() <= 1e-6);
}

#[test]
fn parses_float32() {
    let bytes = wav_bytes(48_000, 1, 32, 3, &float32(&[0.125, -0.75]));
    let wav = parse_wav(&bytes).expect("float");
    assert_eq!(wav.samples, vec![0.125, -0.75]);
}

#[test]
fn stereo_is_averaged_to_mono() {
    let bytes = wav_bytes(48_000, 2, 16, 1, &pcm16(&[0.5, 1.0, -0.5, 0.5]));
    let wav = parse_wav(&bytes).expect("stereo");
    assert_eq!(wav.channels, 2);
    assert_eq!(wav.samples.len(), 2);
    assert!((wav.samples[0] - 0.75).abs() <= 1e-4);
    assert!((wav.samples[1] - 0.0).abs() <= 1e-4);
}

#[test]
fn rejects_bad_headers_and_unsupported_formats() {
    assert_eq!(parse_wav(b"short").expect_err("short").code(), BAD_INPUT);
    let mut bytes = wav_bytes(48_000, 1, 16, 1, &pcm16(&[0.0]));
    bytes[0] = b'X';
    assert_eq!(parse_wav(&bytes).expect_err("magic").code(), BAD_INPUT);

    let bytes = wav_bytes(48_000, 1, 8, 1, &[0u8; 4]);
    assert_eq!(parse_wav(&bytes).expect_err("8 bit").code(), BAD_INPUT);

    let bytes = wav_bytes(48_000, 0, 16, 1, &pcm16(&[0.0]));
    assert_eq!(
        parse_wav(&bytes).expect_err("zero channels").code(),
        BAD_INPUT
    );

    // 把 data 块的 size 改成超过文件长度。
    let mut bytes = wav_bytes(48_000, 1, 16, 1, &pcm16(&[0.0, 0.0]));
    let data_offset = bytes
        .windows(4)
        .position(|window| window == b"data")
        .expect("data chunk");
    bytes[data_offset + 4..data_offset + 8].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(parse_wav(&bytes).expect_err("truncated").code(), BAD_INPUT);
}

#[test]
fn skips_unknown_chunks_including_odd_padding() {
    let data = pcm16(&[0.5, -0.5]);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(0u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&48_000u32.to_le_bytes());
    bytes.extend_from_slice(&96_000u32.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    // 奇数长度的未知块（3 字节 + 1 字节填充）。
    bytes.extend_from_slice(b"LIST");
    bytes.extend_from_slice(&3u32.to_le_bytes());
    bytes.extend_from_slice(b"abc");
    bytes.push(0);
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&data);
    let wav = parse_wav(&bytes).expect("odd chunk padding");
    assert_eq!(wav.samples.len(), 2);
    assert!((wav.samples[0] - 0.5).abs() <= 1e-4);
}
