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

fn read_wav(path: &Path) -> Result<khaen_calibration::Wav, CliError> {
    khaen_calibration::wav::read_wav_file(path).map_err(|err| {
        let code = if err.code() == khaen_calibration::wav::IO {
            "E_IO"
        } else {
            "E_INPUT"
        };
        CliError::new(code, err.message().to_string())
    })
}
