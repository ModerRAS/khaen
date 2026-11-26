//! khaen-analyze：仓库内离线分析工具链 CLI（纯 Rust；只读 WAV、只写 JSON）。
//!
//! 子命令：
//! - spectrum：Welch 功率谱 -> 1/3 oct 带电平（dBFS），可选附加原始 PSD；
//! - envelope：短时 RMS 包络 + 掉音（dropout）检测（复测前的质量门）；
//! - compare：两段录音的相对频带电平与 delta（与 khaen-band-levels 同一口径）。
//!
//! 退出码：0 成功 / 2 输入或用法错误 / 1 写输出失败。
//! stdout 一行 JSON 摘要（键字典序，含输出文件 sha256）；错误写 stderr 一行。
//! 输出文件已存在会拒绝覆盖（E_WRITE），除非先自行删除。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use khaen_analyze::compare;
use khaen_analyze::envelope::{self, EnvelopeConfig};
use khaen_analyze::json;
use khaen_analyze::spectrum::{self, SpectrumConfig, DEFAULT_CENTERS_HZ, DEFAULT_WINDOW_LEN};
use khaen_analyze::wav::{read_wav_file, Wav};
use khaen_analyze::{codes, AnalyzeError, TOOL_NAME};
use serde_json::Value;

const EXIT_OK: u8 = 0;
const EXIT_WRITE: u8 = 1;
const EXIT_INPUT: u8 = 2;

const USAGE: &str = "\
khaen-analyze：离线分析工具链（纯 Rust；只读 WAV、只写 JSON）

用法:
  khaen-analyze spectrum --wav FILE --out FILE [--window 4096] [--centers 25,31.5,...] [--psd 0|1]
  khaen-analyze envelope --wav FILE --out FILE [--window-ms 50] [--dropout-db -20] [--min-windows 3]
  khaen-analyze compare --reference FILE --measured FILE --out FILE [--window 4096] [--centers ...]

退出码: 0 成功 | 2 输入/用法错误 | 1 写输出失败
stdout: 一行 JSON 摘要（含输出文件 sha256）；错误: stderr 一行 <CODE>: <message>
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::from(EXIT_OK),
        Err(err) => {
            eprintln!("{TOOL_NAME}: {}: {}", err.code(), err.message());
            ExitCode::from(exit_code(err.code()))
        }
    }
}

fn exit_code(code: &str) -> u8 {
    if code == codes::WRITE {
        EXIT_WRITE
    } else {
        EXIT_INPUT
    }
}

fn run(args: &[String]) -> Result<(), AnalyzeError> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err(usage("missing subcommand (spectrum | envelope | compare)"));
    };
    let rest = &args[1..];
    match command {
        "spectrum" => run_spectrum(rest),
        "envelope" => run_envelope(rest),
        "compare" => run_compare(rest),
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            Ok(())
        }
        other => Err(usage(format!(
            "unknown subcommand {other:?} (spectrum | envelope | compare)"
        ))),
    }
}

fn run_spectrum(args: &[String]) -> Result<(), AnalyzeError> {
    let mut wav = None;
    let mut out = None;
    let mut window = None;
    let mut centers = None;
    let mut psd = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--wav" => wav = Some(next_path(&mut iter, "--wav")?),
            "--out" => out = Some(next_path(&mut iter, "--out")?),
            "--window" => {
                window = Some(parse_usize(next_value(&mut iter, "--window")?, "--window")?)
            }
            "--centers" => centers = Some(parse_centers(next_value(&mut iter, "--centers")?)?),
            "--psd" => psd = Some(parse_flag01(next_value(&mut iter, "--psd")?, "--psd")?),
            other => return Err(usage(format!("unknown argument {other:?} for spectrum"))),
        }
    }
    let wav_path = wav.ok_or_else(|| {
        usage("spectrum --wav FILE --out FILE [--window 4096] [--centers 25,31.5,...] [--psd 0|1]")
    })?;
    let out_path = out.ok_or_else(|| usage("spectrum needs --out FILE"))?;
    let config = SpectrumConfig {
        window_len: window.unwrap_or(DEFAULT_WINDOW_LEN),
        centers_hz: centers.unwrap_or_else(|| DEFAULT_CENTERS_HZ.to_vec()),
        include_psd: psd.unwrap_or(false),
    };
    let wav = read_wav(&wav_path)?;
    let report = spectrum::analyze_wav(&wav, &config)?;
    let document = spectrum::document(&path_text(&wav_path), &report);
    let bytes = json::write_document(&out_path, &document)?;
    let missing = report
        .bands
        .iter()
        .filter(|band| band.level_db.is_none())
        .count();
    let summary = json::object([
        ("ok", Value::from(true)),
        ("tool", Value::from(TOOL_NAME)),
        ("tool_version", Value::from(env!("CARGO_PKG_VERSION"))),
        ("command", Value::from("spectrum")),
        ("bands", json::count(report.bands.len() as u64)),
        ("missing", json::count(missing as u64)),
        ("segments", json::count(report.segments as u64)),
        ("psd", Value::from(config.include_psd)),
        ("levels_sha256", Value::from(sha256_hex(&bytes))),
        ("out", Value::from(path_text(&out_path))),
    ]);
    println!("{}", json::to_line(&summary)?);
    Ok(())
}

fn run_envelope(args: &[String]) -> Result<(), AnalyzeError> {
    let mut wav = None;
    let mut out = None;
    let mut window_ms = None;
    let mut dropout_db = None;
    let mut min_windows = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--wav" => wav = Some(next_path(&mut iter, "--wav")?),
            "--out" => out = Some(next_path(&mut iter, "--out")?),
            "--window-ms" => {
                window_ms = Some(parse_f64(
                    next_value(&mut iter, "--window-ms")?,
                    "--window-ms",
                )?)
            }
            "--dropout-db" => {
                dropout_db = Some(parse_f64(
                    next_value(&mut iter, "--dropout-db")?,
                    "--dropout-db",
                )?)
            }
            "--min-windows" => {
                min_windows = Some(parse_usize(
                    next_value(&mut iter, "--min-windows")?,
                    "--min-windows",
                )?)
            }
            other => return Err(usage(format!("unknown argument {other:?} for envelope"))),
        }
    }
    let wav_path = wav.ok_or_else(|| {
        usage(
            "envelope --wav FILE --out FILE [--window-ms 50] [--dropout-db -20] [--min-windows 3]",
        )
    })?;
    let out_path = out.ok_or_else(|| usage("envelope needs --out FILE"))?;
    let config = EnvelopeConfig {
        window_ms: window_ms.unwrap_or(envelope::DEFAULT_WINDOW_MS),
        dropout_db: dropout_db.unwrap_or(envelope::DEFAULT_DROPOUT_DB),
        min_windows: min_windows.unwrap_or(envelope::DEFAULT_MIN_WINDOWS),
    };
    let wav = read_wav(&wav_path)?;
    let report = envelope::analyze_wav(&wav, &config)?;
    let document = envelope::document(&path_text(&wav_path), &report);
    let bytes = json::write_document(&out_path, &document)?;
    let summary = json::object([
        ("ok", Value::from(true)),
        ("tool", Value::from(TOOL_NAME)),
        ("tool_version", Value::from(env!("CARGO_PKG_VERSION"))),
        ("command", Value::from("envelope")),
        ("windows", json::count(report.windows as u64)),
        ("gaps", json::count(report.gaps.len() as u64)),
        ("verdict", Value::from(report.verdict.as_str())),
        (
            "reference_median_dbfs",
            json::opt_number(report.reference_median_dbfs),
        ),
        ("levels_sha256", Value::from(sha256_hex(&bytes))),
        ("out", Value::from(path_text(&out_path))),
    ]);
    println!("{}", json::to_line(&summary)?);
    Ok(())
}

fn run_compare(args: &[String]) -> Result<(), AnalyzeError> {
    let mut reference = None;
    let mut measured = None;
    let mut out = None;
    let mut window = None;
    let mut centers = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--reference" => reference = Some(next_path(&mut iter, "--reference")?),
            "--measured" => measured = Some(next_path(&mut iter, "--measured")?),
            "--out" => out = Some(next_path(&mut iter, "--out")?),
            "--window" => {
                window = Some(parse_usize(next_value(&mut iter, "--window")?, "--window")?)
            }
            "--centers" => centers = Some(parse_centers(next_value(&mut iter, "--centers")?)?),
            other => return Err(usage(format!("unknown argument {other:?} for compare"))),
        }
    }
    let reference_path = reference.ok_or_else(|| {
        usage("compare --reference FILE --measured FILE --out FILE [--window 4096] [--centers ...]")
    })?;
    let measured_path = measured.ok_or_else(|| usage("compare needs --measured FILE"))?;
    let out_path = out.ok_or_else(|| usage("compare needs --out FILE"))?;
    let window_len = window.unwrap_or(DEFAULT_WINDOW_LEN);
    let centers_hz = centers.unwrap_or_else(|| DEFAULT_CENTERS_HZ.to_vec());
    let reference_wav = read_wav(&reference_path)?;
    let measured_wav = read_wav(&measured_path)?;
    let report = compare::compare_wavs(&reference_wav, &measured_wav, &centers_hz, window_len)?;
    let document = compare::document(
        &path_text(&reference_path),
        &path_text(&measured_path),
        &report,
    );
    let bytes = json::write_document(&out_path, &document)?;
    let summary = json::object([
        ("ok", Value::from(true)),
        ("tool", Value::from(TOOL_NAME)),
        ("tool_version", Value::from(env!("CARGO_PKG_VERSION"))),
        ("command", Value::from("compare")),
        ("bands", json::count(report.bands.len() as u64)),
        ("compared_bands", json::count(report.compared_bands as u64)),
        ("missing_bands", json::count(report.missing_bands as u64)),
        (
            "max_abs_delta_db",
            json::opt_number(report.max_abs_delta_db),
        ),
        ("levels_sha256", Value::from(sha256_hex(&bytes))),
        ("out", Value::from(path_text(&out_path))),
    ]);
    println!("{}", json::to_line(&summary)?);
    Ok(())
}

fn usage(message: impl Into<String>) -> AnalyzeError {
    AnalyzeError::new(codes::USAGE, format!("{} (try --help)", message.into()))
}

fn read_wav(path: &Path) -> Result<Wav, AnalyzeError> {
    read_wav_file(path).map_err(|err| AnalyzeError::new(err.code(), err.message().to_string()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    khaen_calibration::job::sha256_hex(bytes)
}

fn path_text(path: &Path) -> String {
    path.display().to_string()
}

fn next_value<'a>(
    iter: &mut std::slice::Iter<'a, String>,
    flag: &str,
) -> Result<&'a str, AnalyzeError> {
    iter.next()
        .map(String::as_str)
        .ok_or_else(|| usage(format!("{flag} needs a value")))
}

fn next_path(iter: &mut std::slice::Iter<'_, String>, flag: &str) -> Result<PathBuf, AnalyzeError> {
    next_value(iter, flag).map(PathBuf::from)
}

fn parse_usize(raw: &str, flag: &str) -> Result<usize, AnalyzeError> {
    raw.parse::<usize>()
        .map_err(|_| usage(format!("{flag} must be an integer, got {raw:?}")))
}

fn parse_f64(raw: &str, flag: &str) -> Result<f64, AnalyzeError> {
    raw.parse::<f64>()
        .map_err(|_| usage(format!("{flag} must be a number, got {raw:?}")))
}

fn parse_flag01(raw: &str, flag: &str) -> Result<bool, AnalyzeError> {
    match raw {
        "0" => Ok(false),
        "1" => Ok(true),
        other => Err(usage(format!("{flag} must be 0 or 1, got {other:?}"))),
    }
}

fn parse_centers(raw: &str) -> Result<Vec<f64>, AnalyzeError> {
    let mut centers = Vec::new();
    for item in raw.split(',') {
        let item = item.trim();
        if item.is_empty() {
            return Err(usage("empty item in --centers"));
        }
        centers.push(parse_f64(item, "--centers item")?);
    }
    Ok(centers)
}
