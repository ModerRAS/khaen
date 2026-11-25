//! khaen 实测带域电平 CLI：两段 WAV（施加方案后的录音 / 基线参考录音）→ 逐带相对电平。
//!
//! 用法：
//!
//!   khaen-band-levels --measured applied.wav --reference baseline.wav --out levels.json \
//!       [--point P01] [--centers 25,31.5,40,...] [--window 4096]
//!
//! 输出：
//!
//! - out：khaen-closed-loop-levels/v1 JSON（一个测点，逐带 level_db = 10*log10(实测/参考)；
//!   任一侧该带无能量记 null），可直接作为 khaen-closed-loop --observed 的输入；
//! - stdout 一行 JSON：{bands, levels_sha256, missing, ok, point, tool}（serde_json 键序固定）。
//!
//! 退出码：
//!
//! - 0 = 成功
//! - 2 = 输入/用法错误（E_USAGE / E_IO / E_INPUT / E_BAND_LEVELS_*）
//! - 1 = 写输出失败（E_WRITE）
//!
//! WAV 支持：RIFF/WAVE，PCM 16/24/32 bit 或 IEEE float 32 bit；声道数 1..=8 按算术平均
//! 混成单声道；两段录音采样率必须一致。只读文件，不碰设备。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use khaen_calibration::band_levels::{relative_band_levels, BandLevelsConfig};
use khaen_calibration::plan_levels::DEFAULT_CENTERS_HZ;
use khaen_calibration::{levels_bytes, levels_sha256, LevelSet, PointLevels};

const EXIT_OK: u8 = 0;
const EXIT_WRITE: u8 = 1;
const EXIT_INPUT: u8 = 2;

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

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::from(EXIT_OK),
        Err(err) => {
            eprintln!("khaen-band-levels: {}: {}", err.code, err.message);
            ExitCode::from(if err.code == "E_WRITE" {
                EXIT_WRITE
            } else {
                EXIT_INPUT
            })
        }
    }
}

struct Options {
    measured: PathBuf,
    reference: PathBuf,
    out: PathBuf,
    point: String,
    centers: Vec<f64>,
    config: BandLevelsConfig,
}

fn run() -> Result<(), CliError> {
    let options = parse_args()?;
    let measured = read_wav(&options.measured)?;
    let reference = read_wav(&options.reference)?;
    if (measured.sample_rate_hz - reference.sample_rate_hz).abs() > 1e-9 {
        return Err(CliError::new(
            "E_BAND_LEVELS_ALIGNMENT",
            format!(
                "sample rate mismatch: measured {} Hz, reference {} Hz",
                measured.sample_rate_hz, reference.sample_rate_hz
            ),
        ));
    }
    let bands = relative_band_levels(
        &measured.samples,
        &reference.samples,
        measured.sample_rate_hz,
        &options.centers,
        &options.config,
    )
    .map_err(|err| CliError::new(err.code(), err.message().to_string()))?;
    let bands_count = bands.len();
    let missing = bands.iter().filter(|band| band.level_db.is_none()).count();
    let levels = LevelSet {
        sample_rate_hz: measured.sample_rate_hz,
        points: vec![PointLevels {
            name: options.point.clone(),
            bands,
        }],
    };
    let sha = levels_sha256(&levels);
    if options.out.exists() {
        return Err(CliError::new(
            "E_WRITE",
            format!("output file {} already exists", options.out.display()),
        ));
    }
    if let Some(parent) = options.out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|err| {
                CliError::new("E_WRITE", format!("create {}: {err}", parent.display()))
            })?;
        }
    }
    std::fs::write(&options.out, levels_bytes(&levels)).map_err(|err| {
        CliError::new("E_WRITE", format!("write {}: {err}", options.out.display()))
    })?;
    let summary = serde_json::json!({
        "ok": true,
        "tool": "khaen-band-levels",
        "point": options.point,
        "bands": bands_count,
        "missing": missing,
        "levels_sha256": sha,
    });
    println!("{summary}");
    Ok(())
}

fn parse_args() -> Result<Options, CliError> {
    let mut measured = None;
    let mut reference = None;
    let mut out = None;
    let mut point = None;
    let mut centers = None;
    let mut window = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--measured" => measured = Some(required_path(&mut args, "--measured")?),
            "--reference" => reference = Some(required_path(&mut args, "--reference")?),
            "--out" => out = Some(required_path(&mut args, "--out")?),
            "--point" => {
                point = Some(
                    args.next()
                        .ok_or_else(|| CliError::new("E_USAGE", "--point needs a name"))?,
                )
            }
            "--centers" => {
                let raw = args
                    .next()
                    .ok_or_else(|| CliError::new("E_USAGE", "--centers needs a comma list"))?;
                centers = Some(parse_centers(&raw)?);
            }
            "--window" => {
                let raw = args
                    .next()
                    .ok_or_else(|| CliError::new("E_USAGE", "--window needs an integer"))?;
                window = Some(raw.parse::<usize>().map_err(|_| {
                    CliError::new(
                        "E_USAGE",
                        format!("--window must be an integer, got {raw:?}"),
                    )
                })?);
            }
            other => {
                return Err(CliError::new(
                    "E_USAGE",
                    format!("unknown argument {other:?}"),
                ))
            }
        }
    }
    let measured = measured.ok_or_else(|| {
        CliError::new(
            "E_USAGE",
            "khaen-band-levels --measured FILE --reference FILE --out FILE [--point P01] [--centers ...] [--window 4096]",
        )
    })?;
    let reference = reference.ok_or_else(|| CliError::new("E_USAGE", "--reference is required"))?;
    let out = out.ok_or_else(|| CliError::new("E_USAGE", "--out is required"))?;
    let point = point.unwrap_or_else(|| "P01".to_string());
    if point.is_empty() {
        return Err(CliError::new("E_USAGE", "--point must not be empty"));
    }
    let centers = centers.unwrap_or_else(|| DEFAULT_CENTERS_HZ.to_vec());
    let config = BandLevelsConfig {
        window_len: window.unwrap_or(khaen_calibration::band_levels::DEFAULT_WINDOW_LEN),
    };
    Ok(Options {
        measured,
        reference,
        out,
        point,
        centers,
        config,
    })
}

fn required_path(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<PathBuf, CliError> {
    args.next()
        .map(PathBuf::from)
        .ok_or_else(|| CliError::new("E_USAGE", format!("{flag} needs a path")))
}

fn parse_centers(raw: &str) -> Result<Vec<f64>, CliError> {
    let mut centers = Vec::new();
    for item in raw.split(',') {
        let item = item.trim();
        if item.is_empty() {
            return Err(CliError::new("E_USAGE", "empty item in --centers"));
        }
        let value = item.parse::<f64>().map_err(|_| {
            CliError::new(
                "E_USAGE",
                format!("--centers item {item:?} is not a number"),
            )
        })?;
        centers.push(value);
    }
    Ok(centers)
}

struct Wav {
    sample_rate_hz: f64,
    samples: Vec<f64>,
}

fn read_wav(path: &Path) -> Result<Wav, CliError> {
    let bytes = std::fs::read(path)
        .map_err(|err| CliError::new("E_IO", format!("read {}: {err}", path.display())))?;
    parse_wav(&bytes)
        .map_err(|err| CliError::new(err.code, format!("{}: {}", path.display(), err.message)))
}

fn parse_wav(bytes: &[u8]) -> Result<Wav, CliError> {
    if bytes.len() < 44 {
        return Err(CliError::new("E_INPUT", "file is too short to be a WAV"));
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(CliError::new("E_INPUT", "not a RIFF/WAVE file"));
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
            .ok_or_else(|| CliError::new("E_INPUT", "WAV chunk size overflow"))?;
        if payload_end > bytes.len() {
            return Err(CliError::new(
                "E_INPUT",
                "WAV chunk extends past end of file",
            ));
        }
        if id == b"fmt " {
            if size < 16 {
                return Err(CliError::new("E_INPUT", "WAV fmt chunk is too short"));
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
        format.ok_or_else(|| CliError::new("E_INPUT", "WAV has no fmt chunk"))?;
    let (data_start, data_size) =
        data.ok_or_else(|| CliError::new("E_INPUT", "WAV has no data chunk"))?;
    let bytes_per_sample = match (format_tag, bits) {
        (1, 16) => 2usize,
        (1, 24) => 3usize,
        (1, 32) => 4usize,
        (3, 32) => 4usize,
        (tag, bits) => {
            return Err(CliError::new(
                "E_INPUT",
                format!(
                    "unsupported WAV format tag {tag} / {bits} bit (PCM 16/24/32 or float 32 only)"
                ),
            ))
        }
    };
    if !(1..=8).contains(&channels) {
        return Err(CliError::new(
            "E_INPUT",
            format!("unsupported channel count {channels} (1..=8)"),
        ));
    }
    if sample_rate == 0 {
        return Err(CliError::new("E_INPUT", "WAV sample rate is zero"));
    }
    let expected_align = u32::from(channels) * bytes_per_sample as u32;
    let block_align = if block_align == 0 {
        expected_align as u16
    } else {
        block_align
    };
    if u32::from(block_align) != expected_align {
        return Err(CliError::new(
            "E_INPUT",
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
