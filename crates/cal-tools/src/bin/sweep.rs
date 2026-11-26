//! khaen-sweep：确定性测量信号生成器（纯 Rust，不依赖 numpy）。
//!
//! 为单点闭环复测生成指数扫频 WAV：可把总时长拆成多段（例如"全频扫频 + 低频专用扫频"），
//! 每段独立起始/终止频率与电平，段间插入静音。输出 24-bit PCM WAV，同参数逐字节可复现。
//!
//! 用法：
//!
//!   khaen-sweep --out FILE [--rate 48000] [--channels 2] [--amplitude-db -12]
//!       [--lead 1.5] [--gap 2.0] [--tail 1.5]
//!       --segment SECONDS:START_HZ:END_HZ[:AMPLITUDE_DB] [--segment ...]
//!
//! 默认（不写 --segment）：9:20:20000，即整段 9 s 全频扫频。
//!
//! 例：全频 + 低频专用（LF 积分时间 ×8，用于 25–63 Hz 噪声受限时提高信噪比）
//!
//!   khaen-sweep --out sweep-full-lf.wav --lead 1.5 --gap 2 --tail 1.5 \
//!       --segment 9:20:20000 --segment 9:20:200:-9
//!
//! 输出：
//! - stdout 一行 JSON 摘要：{ok,tool,out,sha256,frames,duration_s,segments:[...]}
//! - 退出码：0 成功 | 2 输入/用法错误（E_USAGE/E_INPUT） | 1 写输出失败（E_WRITE）
//!
//! 只写文件、不碰设备；生成的 WAV 只用于测量播放（真机流程见 docs/CLOSED-LOOP.md）。

use std::path::PathBuf;
use std::process::ExitCode;

const TOOL: &str = "khaen-sweep";
const EXIT_OK: u8 = 0;
const EXIT_WRITE: u8 = 1;
const EXIT_INPUT: u8 = 2;
const FADE_SECONDS: f64 = 0.010;
const MAX_CHANNELS: u16 = 8;

struct CliError {
    code: &'static str,
    message: String,
}

impl CliError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Segment {
    seconds: f64,
    start_hz: f64,
    end_hz: f64,
    amplitude_db: f64,
}

struct Options {
    out: PathBuf,
    rate: u32,
    channels: u16,
    lead: f64,
    gap: f64,
    tail: f64,
    segments: Vec<Segment>,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::from(EXIT_OK),
        Err(err) => {
            eprintln!("{TOOL}: {}: {}", err.code, err.message);
            ExitCode::from(if err.code == "E_WRITE" {
                EXIT_WRITE
            } else {
                EXIT_INPUT
            })
        }
    }
}

fn run() -> Result<(), CliError> {
    let options = parse_args()?;
    let mono = render(&options);
    let samples: Vec<i32> = mono
        .iter()
        .map(|value| {
            let scaled = (value.clamp(-1.0, 1.0) * 8_388_607.0).round();
            scaled as i32
        })
        .collect();
    let bytes = encode_wav_24(&samples, options.channels, options.rate);
    std::fs::write(&options.out, &bytes).map_err(|err| {
        CliError::new("E_WRITE", format!("write {}: {err}", options.out.display()))
    })?;
    let frames = samples.len() as f64;
    let summary = serde_json::json!({
        "ok": true,
        "tool": TOOL,
        "out": options.out.display().to_string(),
        "sha256": sha256_hex(&bytes),
        "frames": frames as u64,
        "duration_s": frames / f64::from(options.rate),
        "rate": options.rate,
        "channels": options.channels,
        "segments": options.segments.iter().map(|segment| serde_json::json!({
            "seconds": segment.seconds,
            "start_hz": segment.start_hz,
            "end_hz": segment.end_hz,
            "amplitude_db": segment.amplitude_db,
        })).collect::<Vec<_>>(),
    });
    println!("{summary}");
    Ok(())
}

/// 渲染单声道样本：lead 静音 +（段间 gap 静音 + 扫频段）× N + tail 静音。
fn render(options: &Options) -> Vec<f64> {
    let rate = f64::from(options.rate);
    let mut out = Vec::new();
    push_silence(&mut out, options.lead, rate);
    for (index, segment) in options.segments.iter().enumerate() {
        if index > 0 {
            push_silence(&mut out, options.gap, rate);
        }
        let frames = (segment.seconds * rate).round() as usize;
        let amplitude = 10f64.powf(segment.amplitude_db / 20.0);
        let ln_ratio = (segment.end_hz / segment.start_hz).ln();
        let k = segment.seconds / ln_ratio;
        let phase_scale = 2.0 * std::f64::consts::PI * segment.start_hz * k;
        let fade_frames = ((FADE_SECONDS * rate).round() as usize).min(frames / 2);
        for frame in 0..frames {
            let t = frame as f64 / rate;
            let phase = phase_scale * ((t / k).exp() - 1.0);
            let mut gain = 1.0;
            if fade_frames > 1 {
                if frame < fade_frames {
                    let x = frame as f64 / fade_frames as f64;
                    gain = 0.5 - 0.5 * (std::f64::consts::PI * x).cos();
                } else if frame + fade_frames > frames {
                    let x = (frames - frame) as f64 / fade_frames as f64;
                    gain = 0.5 - 0.5 * (std::f64::consts::PI * x).cos();
                }
            }
            out.push(amplitude * gain * phase.sin());
        }
    }
    push_silence(&mut out, options.tail, rate);
    out
}

fn push_silence(out: &mut Vec<f64>, seconds: f64, rate: f64) {
    let frames = (seconds * rate).round() as usize;
    out.extend(std::iter::repeat_n(0.0, frames));
}

fn parse_args() -> Result<Options, CliError> {
    let mut out = None;
    let mut rate = 48_000u32;
    let mut channels = 2u16;
    let mut amplitude_db = -12.0;
    let mut lead = 1.5;
    let mut gap = 2.0;
    let mut tail = 1.5;
    let mut segments = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out = Some(PathBuf::from(required_value(&mut args, "--out")?)),
            "--rate" => {
                rate = required_value(&mut args, "--rate")?
                    .parse()
                    .map_err(|_| CliError::new("E_USAGE", "--rate must be an integer"))?;
            }
            "--channels" => {
                channels = required_value(&mut args, "--channels")?
                    .parse()
                    .map_err(|_| CliError::new("E_USAGE", "--channels must be an integer"))?;
            }
            "--amplitude-db" => {
                amplitude_db = required_value(&mut args, "--amplitude-db")?
                    .parse()
                    .map_err(|_| CliError::new("E_USAGE", "--amplitude-db must be a number"))?;
            }
            "--lead" => {
                lead = non_negative(
                    "--lead",
                    required_value(&mut args, "--lead")?
                        .parse()
                        .map_err(|_| CliError::new("E_USAGE", "--lead must be a number"))?,
                )?;
            }
            "--gap" => {
                gap = non_negative(
                    "--gap",
                    required_value(&mut args, "--gap")?
                        .parse()
                        .map_err(|_| CliError::new("E_USAGE", "--gap must be a number"))?,
                )?;
            }
            "--tail" => {
                tail = non_negative(
                    "--tail",
                    required_value(&mut args, "--tail")?
                        .parse()
                        .map_err(|_| CliError::new("E_USAGE", "--tail must be a number"))?,
                )?;
            }
            "--segment" => segments.push(parse_segment(&required_value(&mut args, "--segment")?)?),
            other => {
                return Err(CliError::new(
                    "E_USAGE",
                    format!("unknown argument {other:?}; see khaen-sweep --help"),
                ))
            }
        }
    }
    if args.next().is_some() {
        return Err(CliError::new("E_USAGE", "--help is not supported"));
    }
    let out = out.ok_or_else(|| CliError::new("E_USAGE", "--out is required"))?;
    if segments.is_empty() {
        segments.push(Segment {
            seconds: 9.0,
            start_hz: 20.0,
            end_hz: 20_000.0,
            amplitude_db,
        });
    }
    if !(8_000..=384_000).contains(&rate) {
        return Err(CliError::new(
            "E_USAGE",
            format!("--rate {rate} is outside [8000, 384000]"),
        ));
    }
    if !(1..=MAX_CHANNELS).contains(&channels) {
        return Err(CliError::new(
            "E_USAGE",
            format!("--channels {channels} is outside [1, {MAX_CHANNELS}]"),
        ));
    }
    if !(-120.0..=0.0).contains(&amplitude_db) {
        return Err(CliError::new(
            "E_USAGE",
            format!("--amplitude-db {amplitude_db} is outside [-120, 0]"),
        ));
    }
    let nyquist = f64::from(rate) / 2.0;
    for segment in &segments {
        if segment.seconds <= 0.0 {
            return Err(CliError::new("E_USAGE", "segment seconds must be > 0"));
        }
        if !(segment.start_hz > 0.0 && segment.start_hz < segment.end_hz) {
            return Err(CliError::new(
                "E_USAGE",
                "segment frequencies must satisfy 0 < start_hz < end_hz",
            ));
        }
        if segment.end_hz >= nyquist {
            return Err(CliError::new(
                "E_USAGE",
                format!(
                    "segment end_hz {} must be below Nyquist {}",
                    segment.end_hz, nyquist
                ),
            ));
        }
        if !(-120.0..=0.0).contains(&segment.amplitude_db) {
            return Err(CliError::new(
                "E_USAGE",
                "segment amplitude_db must be within [-120, 0]",
            ));
        }
    }
    Ok(Options {
        out,
        rate,
        channels,
        lead,
        gap,
        tail,
        segments,
    })
}

fn required_value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, CliError> {
    args.next()
        .ok_or_else(|| CliError::new("E_USAGE", format!("{flag} needs a value")))
}

fn non_negative(flag: &str, value: f64) -> Result<f64, CliError> {
    if value.is_finite() && value >= 0.0 {
        Ok(value)
    } else {
        Err(CliError::new(
            "E_USAGE",
            format!("{flag} must be a finite non-negative number"),
        ))
    }
}

fn parse_segment(raw: &str) -> Result<Segment, CliError> {
    let parts: Vec<&str> = raw.split(':').collect();
    if parts.len() != 3 && parts.len() != 4 {
        return Err(CliError::new(
            "E_USAGE",
            "--segment must be SECONDS:START_HZ:END_HZ[:AMPLITUDE_DB]",
        ));
    }
    let number = |index: usize, name: &str| -> Result<f64, CliError> {
        parts[index]
            .parse::<f64>()
            .map_err(|_| CliError::new("E_USAGE", format!("segment {name} is not a number")))
    };
    let seconds = number(0, "seconds")?;
    let start_hz = number(1, "start_hz")?;
    let end_hz = number(2, "end_hz")?;
    let amplitude_db = if parts.len() == 4 {
        number(3, "amplitude_db")?
    } else {
        -12.0
    };
    Ok(Segment {
        seconds,
        start_hz,
        end_hz,
        amplitude_db,
    })
}

/// 24-bit little-endian PCM WAV（data 在前、fmt 在后之前的常见 44 字节头）。
fn encode_wav_24(samples: &[i32], channels: u16, rate: u32) -> Vec<u8> {
    let data_len = samples.len() * usize::from(channels) * 3;
    let byte_rate = rate * u32::from(channels) * 3;
    let block_align = channels * 3;
    let mut out = Vec::with_capacity(44 + data_len);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for sample in samples {
        let raw = (*sample).clamp(-8_388_608, 8_388_607);
        let bytes = raw.to_le_bytes();
        for _ in 0..channels {
            out.extend_from_slice(&bytes[0..3]);
        }
    }
    debug_assert_eq!(out.len(), 44 + data_len);
    out
}

fn sha256_hex(bytes: &[u8]) -> String {
    khaen_core::sha256::sha256_hex(bytes)
}
