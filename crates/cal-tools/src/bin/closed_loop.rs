//! khaen 闭环复测判定 CLI：预测 vs 实测带域电平 → PASS/FAIL/INCOMPLETE + 可复现报告。
//!
//! 用法：
//!
//!   khaen-closed-loop --predicted predicted.json --observed observed.json --out out/ \
//!       [--tolerance 2.0] [--min-bands 3]
//!
//! 输出：
//!
//! - out/report.json：确定性报告（键序固定、10 位有效数字、末尾换行）；
//! - stdout 一行 JSON：{ok, verdict, report_sha256, report_path, compared_bands, violations}。
//!
//! 退出码：
//!
//! - 0 = PASS（所有可比较带都在容差内且数量足够）
//! - 3 = FAIL（至少一个带超容差）
//! - 4 = INCOMPLETE（可比较带不足，数据不够）
//! - 2 = 输入/用法错误（E_USAGE / E_INPUT / E_ALIGNMENT / E_CONFIG）
//! - 1 = 写报告失败（E_WRITE）
//!
//! 本工具**不做测量、不碰设备**；实测电平由真机采集流程提供（T42 设备侧，NOT_RUN）。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use khaen_calibration::{
    compare, report_bytes, report_sha256, BandLevel, ClosedLoopConfig, ClosedLoopError, LevelSet,
    PointLevels, Verdict, CLOSED_LOOP_LEVELS_SCHEMA,
};
use serde_json::Value;

const EXIT_PASS: u8 = 0;
const EXIT_WRITE: u8 = 1;
const EXIT_INPUT: u8 = 2;
const EXIT_FAIL: u8 = 3;
const EXIT_INCOMPLETE: u8 = 4;

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

    fn usage(message: impl Into<String>) -> Self {
        Self::new("E_USAGE", message)
    }

    fn input(message: impl Into<String>) -> Self {
        Self::new("E_INPUT", message)
    }
}

fn main() -> ExitCode {
    match parse_args() {
        Ok(options) => match execute(&options) {
            Ok(exit_code) => ExitCode::from(exit_code),
            Err(err) => {
                eprintln!("khaen-closed-loop: {}: {}", err.code, err.message);
                ExitCode::from(exit_code_for_error(&err))
            }
        },
        Err(err) => {
            eprintln!("khaen-closed-loop: {}: {}", err.code, err.message);
            ExitCode::from(exit_code_for_error(&err))
        }
    }
}

fn exit_code_for_error(err: &CliError) -> u8 {
    if err.code == "E_WRITE" {
        EXIT_WRITE
    } else {
        EXIT_INPUT
    }
}

struct Options {
    predicted: PathBuf,
    observed: PathBuf,
    out: PathBuf,
    config: ClosedLoopConfig,
}

fn parse_args() -> Result<Options, CliError> {
    let mut predicted = None;
    let mut observed = None;
    let mut out = None;
    let mut tolerance = None;
    let mut min_bands = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--predicted" => {
                predicted = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| CliError::usage("--predicted needs a path"))?,
                ))
            }
            "--observed" => {
                observed = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| CliError::usage("--observed needs a path"))?,
                ))
            }
            "--out" => {
                out = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| CliError::usage("--out needs a directory"))?,
                ))
            }
            "--tolerance" => {
                let value = args
                    .next()
                    .ok_or_else(|| CliError::usage("--tolerance needs a number"))?;
                tolerance = Some(value.parse::<f64>().map_err(|_| {
                    CliError::usage(format!("--tolerance must be a number, got {value:?}"))
                })?);
            }
            "--min-bands" => {
                let value = args
                    .next()
                    .ok_or_else(|| CliError::usage("--min-bands needs an integer"))?;
                min_bands = Some(value.parse::<usize>().map_err(|_| {
                    CliError::usage(format!("--min-bands must be an integer, got {value:?}"))
                })?);
            }
            other => {
                return Err(CliError::usage(format!(
                    "unknown argument {other:?} (expected --predicted/--observed/--out/--tolerance/--min-bands)"
                )))
            }
        }
    }
    let mut config = ClosedLoopConfig::default();
    if let Some(tolerance) = tolerance {
        config.tolerance_db = tolerance;
    }
    if let Some(min_bands) = min_bands {
        config.min_compared_bands = min_bands;
    }
    Ok(Options {
        predicted: predicted.ok_or_else(|| CliError::usage("missing --predicted"))?,
        observed: observed.ok_or_else(|| CliError::usage("missing --observed"))?,
        out: out.ok_or_else(|| CliError::usage("missing --out"))?,
        config,
    })
}

fn execute(options: &Options) -> Result<u8, CliError> {
    let predicted = load_levels(&options.predicted)?;
    let observed = load_levels(&options.observed)?;
    let report = compare(&predicted, &observed, &options.config)
        .map_err(|err: ClosedLoopError| CliError::new(err.code(), err.message().to_string()))?;
    let bytes = report_bytes(&report);
    let sha = report_sha256(&report);
    ensure_empty_or_absent(&options.out)?;
    std::fs::create_dir_all(&options.out).map_err(|err| {
        CliError::new(
            "E_WRITE",
            format!("cannot create {}: {err}", options.out.display()),
        )
    })?;
    let report_path = options.out.join("report.json");
    std::fs::write(&report_path, &bytes).map_err(|err| {
        CliError::new(
            "E_WRITE",
            format!("cannot write {}: {err}", report_path.display()),
        )
    })?;
    println!(
        "{{\"ok\":true,\"verdict\":\"{}\",\"report_sha256\":\"{}\",\"report_path\":\"{}\",\"compared_bands\":{},\"violations\":{}}}",
        report.verdict.as_str(),
        sha,
        report_path.display(),
        report.compared_bands,
        report.violations
    );
    Ok(match report.verdict {
        Verdict::Pass => EXIT_PASS,
        Verdict::Fail => EXIT_FAIL,
        Verdict::Incomplete => EXIT_INCOMPLETE,
    })
}

fn load_levels(path: &Path) -> Result<LevelSet, CliError> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| CliError::input(format!("cannot read {}: {err}", path.display())))?;
    let doc: Value = serde_json::from_str(&text)
        .map_err(|err| CliError::input(format!("{} is not valid JSON: {err}", path.display())))?;
    parse_levels(&doc)
        .map_err(|err| CliError::input(format!("{}: {}", path.display(), err.message)))
}

fn parse_levels(doc: &Value) -> Result<LevelSet, CliError> {
    let object = doc
        .as_object()
        .ok_or_else(|| CliError::input("document must be a JSON object"))?;
    let schema = object
        .get("schema")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::input("missing string field: schema"))?;
    if schema != CLOSED_LOOP_LEVELS_SCHEMA {
        return Err(CliError::input(format!(
            "schema must be {CLOSED_LOOP_LEVELS_SCHEMA}, got {schema:?}"
        )));
    }
    let sample_rate_hz = object
        .get("sample_rate_hz")
        .and_then(Value::as_f64)
        .ok_or_else(|| CliError::input("field sample_rate_hz must be a number"))?;
    let points = object
        .get("points")
        .and_then(Value::as_array)
        .ok_or_else(|| CliError::input("field points must be an array"))?;
    let mut parsed_points = Vec::with_capacity(points.len());
    for point in points {
        let point = point
            .as_object()
            .ok_or_else(|| CliError::input("points entries must be objects"))?;
        let name = point
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| CliError::input("point.name must be a string"))?
            .to_string();
        let bands = point
            .get("bands")
            .and_then(Value::as_array)
            .ok_or_else(|| CliError::input(format!("point {name}: bands must be an array")))?;
        let mut parsed_bands = Vec::with_capacity(bands.len());
        for band in bands {
            let band = band.as_object().ok_or_else(|| {
                CliError::input(format!("point {name}: band entries must be objects"))
            })?;
            let center_hz = band
                .get("center_hz")
                .and_then(Value::as_f64)
                .ok_or_else(|| {
                    CliError::input(format!("point {name}: band center_hz must be a number"))
                })?;
            let level_db = match band.get("level_db") {
                None | Some(Value::Null) => None,
                Some(Value::Number(number)) => Some(number.as_f64().ok_or_else(|| {
                    CliError::input(format!("point {name}: band level_db must be finite"))
                })?),
                Some(_) => {
                    return Err(CliError::input(format!(
                        "point {name}: band level_db must be a number or null"
                    )))
                }
            };
            parsed_bands.push(BandLevel {
                center_hz,
                level_db,
            });
        }
        parsed_points.push(PointLevels {
            name,
            bands: parsed_bands,
        });
    }
    Ok(LevelSet {
        sample_rate_hz,
        points: parsed_points,
    })
}

fn ensure_empty_or_absent(dir: &Path) -> Result<(), CliError> {
    match std::fs::read_dir(dir) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(CliError::new(
            "E_WRITE",
            format!("cannot inspect {}: {err}", dir.display()),
        )),
        Ok(mut entries) => {
            if entries.next().is_some() {
                Err(CliError::new(
                    "E_WRITE",
                    format!(
                        "output directory {} is not empty; refusing to mix reports",
                        dir.display()
                    ),
                ))
            } else {
                Ok(())
            }
        }
    }
}
