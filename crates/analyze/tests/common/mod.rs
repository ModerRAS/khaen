//! 分析工具链测试共用：合成 WAV、临时目录、跑 CLI、读 JSON。
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::Value;

/// 测试统一采样率。
pub const FS: u32 = 48_000;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// 新建一个空临时目录（同名旧目录先删）。
pub fn temp_dir(tag: &str) -> PathBuf {
    let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "khaen-analyze-{}-{}-{}",
        std::process::id(),
        tag,
        unique
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// 单声道正弦（幅度线性、帧数 samples）。
pub fn sine(freq_hz: f64, amplitude: f64, frames: usize, sample_rate: u32) -> Vec<f64> {
    (0..frames)
        .map(|index| {
            amplitude
                * (2.0 * std::f64::consts::PI * freq_hz * index as f64 / f64::from(sample_rate))
                    .sin()
        })
        .collect()
}

/// 正弦 + [start, end) 帧替换成另一幅度/频率的正弦（造掉音或低电平块）。
pub fn sine_with_block(
    freq_hz: f64,
    amplitude: f64,
    frames: usize,
    sample_rate: u32,
    block: (usize, usize, f64),
) -> Vec<f64> {
    let (start, end, block_amplitude) = block;
    let mut samples = sine(freq_hz, amplitude, frames, sample_rate);
    for (index, sample) in samples
        .iter_mut()
        .enumerate()
        .take(end.min(frames))
        .skip(start)
    {
        *sample = block_amplitude
            * (2.0 * std::f64::consts::PI * freq_hz * index as f64 / f64::from(sample_rate)).sin();
    }
    samples
}

/// 构造 RIFF/WAVE 字节：交错样本 + 指定 format tag / 位数。
/// 支持 PCM 16/24/32 与 IEEE float 32（与 khaen-calibration 的 WAV 解码器一致）。
pub fn wav_bytes(
    sample_rate: u32,
    channels: u16,
    format_tag: u16,
    bits: u16,
    interleaved: &[f64],
) -> Vec<u8> {
    assert_eq!(
        interleaved.len() % usize::from(channels),
        0,
        "interleaved frames"
    );
    let bytes_per_sample = usize::from(bits / 8);
    let data_len = interleaved.len() * bytes_per_sample;
    let block_align = usize::from(channels) * bytes_per_sample;
    let mut bytes = Vec::with_capacity(44 + data_len);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(
        &u32::try_from(36 + data_len)
            .expect("wav fits u32")
            .to_le_bytes(),
    );
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&format_tag.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * block_align as u32).to_le_bytes());
    bytes.extend_from_slice(
        &u16::try_from(block_align)
            .expect("block align")
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&bits.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(
        &u32::try_from(data_len)
            .expect("data fits u32")
            .to_le_bytes(),
    );
    for &sample in interleaved {
        match (format_tag, bits) {
            (1, 16) => {
                let value = (sample * 32_767.0).round().clamp(-32_768.0, 32_767.0) as i16;
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            (1, 24) => {
                let value = (sample * 8_388_607.0)
                    .round()
                    .clamp(-8_388_608.0, 8_388_607.0) as i32;
                bytes.extend_from_slice(&value.to_le_bytes()[..3]);
            }
            (1, 32) => {
                let value = (sample * 2_147_483_647.0)
                    .round()
                    .clamp(-2_147_483_648.0, 2_147_483_647.0) as i32;
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            (3, 32) => bytes.extend_from_slice(&(sample as f32).to_le_bytes()),
            (tag, bits) => panic!("unsupported wav format tag {tag} / {bits} bit"),
        }
    }
    bytes
}

/// 写 16-bit PCM WAV 文件。
pub fn write_pcm16(path: &Path, sample_rate: u32, channels: u16, interleaved: &[f64]) {
    std::fs::write(path, wav_bytes(sample_rate, channels, 1, 16, interleaved)).expect("write wav");
}

/// 写 24-bit PCM WAV 文件。
pub fn write_pcm24(path: &Path, sample_rate: u32, channels: u16, interleaved: &[f64]) {
    std::fs::write(path, wav_bytes(sample_rate, channels, 1, 24, interleaved)).expect("write wav");
}

/// 写 32-bit PCM WAV 文件。
pub fn write_pcm32(path: &Path, sample_rate: u32, channels: u16, interleaved: &[f64]) {
    std::fs::write(path, wav_bytes(sample_rate, channels, 1, 32, interleaved)).expect("write wav");
}

/// 写 32-bit float WAV 文件。
pub fn write_float32(path: &Path, sample_rate: u32, channels: u16, interleaved: &[f64]) {
    std::fs::write(path, wav_bytes(sample_rate, channels, 3, 32, interleaved)).expect("write wav");
}

/// CLI 一次运行的输出。
pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    /// stdout 的一行 JSON 摘要。
    pub fn summary(&self) -> Value {
        serde_json::from_str(self.stdout.trim()).expect("stdout summary is JSON")
    }
}

fn run_command(args: &[&str]) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_khaen-analyze"))
        .args(args)
        .output()
        .expect("spawn khaen-analyze");
    Run {
        code: output.status.code().expect("exit code"),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// 跑 khaen-analyze（参数为字符串切片）。
pub fn run_bin(args: &[&str]) -> Run {
    run_command(args)
}

/// 跑 khaen-analyze（参数为 String 向量）。
pub fn run_args(args: &[String]) -> Run {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_command(&refs)
}

/// 读 JSON 文件。
pub fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).expect("read json")).expect("parse json")
}

/// 文件字节的 sha256（复用 khaen-calibration 的纯 Rust 实现）。
pub fn sha256_file(path: &Path) -> String {
    khaen_calibration::job::sha256_hex(&std::fs::read(path).expect("read for sha256"))
}

/// 从 JSON 文档的 bands 数组按中心频率取某字段（null -> None）。
pub fn band_field(document: &Value, center_hz: f64, field: &str) -> Option<f64> {
    document["bands"]
        .as_array()
        .expect("bands array")
        .iter()
        .find(|band| (band["center_hz"].as_f64().expect("center_hz") - center_hz).abs() < 1e-6)
        .and_then(|band| {
            let value = &band[field];
            if value.is_null() {
                None
            } else {
                Some(value.as_f64().expect("numeric band field"))
            }
        })
}
