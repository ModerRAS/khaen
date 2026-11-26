//! WAV 解码（复用 khaen-calibration）：16/24/32-bit PCM、float32、多声道混单声道、错误码。

mod common;

use std::path::Path;

use common::*;
use khaen_analyze::wav::{parse_wav, read_wav_file};

fn peak(samples: &[f64]) -> f64 {
    samples
        .iter()
        .fold(0.0f64, |peak, &value| peak.max(value.abs()))
}

#[test]
fn parses_pcm16_mono() {
    let dir = temp_dir("wav-pcm16");
    let path = dir.join("mono.wav");
    write_pcm16(&path, FS, 1, &sine(1_000.0, 0.5, 4_800, FS));
    let wav = read_wav_file(&path).expect("read wav");
    assert_eq!(wav.channels, 1);
    assert_eq!(wav.samples.len(), 4_800);
    assert!((wav.sample_rate_hz - 48_000.0).abs() < 1e-9);
    assert!((peak(&wav.samples) - 0.5).abs() < 2e-3);
}

#[test]
fn mixes_stereo_to_mono_by_arithmetic_mean() {
    let frames = 1_000;
    let left = sine(1_000.0, 0.5, frames, FS);
    let mut interleaved = Vec::with_capacity(frames * 2);
    for sample in &left {
        interleaved.push(*sample);
        interleaved.push(0.0);
    }
    let wav = parse_wav(&wav_bytes(FS, 2, 1, 16, &interleaved)).expect("parse stereo");
    assert_eq!(wav.channels, 2);
    assert_eq!(wav.samples.len(), frames);
    assert!((peak(&wav.samples) - 0.25).abs() < 2e-3);
}

#[test]
fn parses_pcm24_pcm32_and_float32() {
    for (format_tag, bits) in [(1u16, 24u16), (1, 32), (3, 32)] {
        let samples = sine(1_000.0, 1.0, 480, FS);
        let wav = parse_wav(&wav_bytes(FS, 1, format_tag, bits, &samples)).expect("parse format");
        assert_eq!(wav.samples.len(), 480);
        assert!(
            (peak(&wav.samples) - 1.0).abs() < 2e-3,
            "format {format_tag}/{bits} peak {}",
            peak(&wav.samples)
        );
    }
}

#[test]
fn rejects_non_wav_bytes() {
    let error = parse_wav(b"this is definitely not a RIFF/WAVE file").expect_err("reject");
    assert_eq!(error.code(), "E_WAV_INPUT");
    let error = parse_wav(&[0u8; 10]).expect_err("too short");
    assert_eq!(error.code(), "E_WAV_INPUT");
}

#[test]
fn rejects_unsupported_sample_format() {
    // 8-bit PCM：同一份 RIFF 头，bits = 8、block align = 1、每样本 1 字节。
    let data = [0u8; 8];
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&FS.to_le_bytes());
    bytes.extend_from_slice(&FS.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&8u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&data);
    let error = parse_wav(&bytes).expect_err("8-bit PCM unsupported");
    assert_eq!(error.code(), "E_WAV_INPUT");
}

#[test]
fn rejects_zero_sample_rate_and_too_many_channels() {
    let samples = sine(1_000.0, 0.5, 96, FS);
    let error = parse_wav(&wav_bytes(0, 1, 1, 16, &samples)).expect_err("zero rate");
    assert_eq!(error.code(), "E_WAV_INPUT");
    let channels = 9u16;
    let interleaved: Vec<f64> = vec![0.0; 96 * usize::from(channels)];
    let error = parse_wav(&wav_bytes(FS, channels, 1, 16, &interleaved)).expect_err("9 channels");
    assert_eq!(error.code(), "E_WAV_INPUT");
}

#[test]
fn missing_file_reports_io_code() {
    let path = Path::new("this-file-does-not-exist-khaen-analyze.wav");
    let error = read_wav_file(path).expect_err("missing file");
    assert_eq!(error.code(), "E_WAV_IO");
}
