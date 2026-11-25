//! khaen 场景方案 CLI：mimo 单点报告 + FIR CSV 目录 -> plan ABI v3 方案文件。
//!
//! 与手动指定每个 --fir 的 khaen-fir-plan 相比，本工具按**报告里选定的变体**取 FIR：
//! mimo 一次运行会同时写出 B2/B3 两套 FIR，直接按文件名排序会把同一源的两套变体当成两条
//! lane。这里从 report.json 读 outcome_variant（可用 --variant 覆盖），只取该变体列出的
//! fir_files，按报告给的顺序映射成 lane 0..N-1。
//!
//! 用法：
//!
//!   khaen-scene-plan --report out/single-point/scene-report.json \
//!       --fir-dir out/single-point/fir --out plan.bin --sample-rate 48000 \
//!       [--variant B3] [--block-size 128] [--channels N]
//!
//! 退出码：0 成功 / 2 输入或配置错误（E_USAGE / E_IO / E_INPUT / E_FIR_PLAN_*） / 1 写失败。
//! 只读报告与 CSV、只写方案文件；不碰设备（真机测量/下发是 T42 设备侧流程，NOT_RUN）。

use std::path::PathBuf;
use std::process::ExitCode;

use khaen_cal_tools::fir_csv::{read_fir_csv, IO as FIR_CSV_IO};
use khaen_calibration::fir_plan::{compile_fir_plan, FirPlanConfig, LaneFir};
use serde_json::Value;

const EXIT_OK: u8 = 0;
const EXIT_WRITE: u8 = 1;
const EXIT_INPUT: u8 = 2;

const REPORT_FORMAT: &str = "khaen-mimo-sim-report/v1";

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
            eprintln!("khaen-scene-plan: {}: {}", err.code, err.message);
            ExitCode::from(if err.code == "E_WRITE" {
                EXIT_WRITE
            } else {
                EXIT_INPUT
            })
        }
    }
}

struct Options {
    report: PathBuf,
    fir_dir: PathBuf,
    out: PathBuf,
    variant: Option<String>,
    block_size: u32,
    sample_rate: u32,
    channels: Option<u16>,
}

struct SceneFir {
    source: String,
    file: PathBuf,
}

fn run() -> Result<(), CliError> {
    let options = parse_args()?;
    let report_bytes = std::fs::read(&options.report).map_err(|err| {
        CliError::new(
            "E_IO",
            format!("read report {}: {err}", options.report.display()),
        )
    })?;
    let report: Value = serde_json::from_slice(&report_bytes).map_err(|err| {
        CliError::new(
            "E_INPUT",
            format!(
                "report {} is not valid JSON: {err}",
                options.report.display()
            ),
        )
    })?;
    let format = report["format"].as_str().unwrap_or("");
    if format != REPORT_FORMAT {
        return Err(CliError::new(
            "E_INPUT",
            format!("report format {format:?} is not {REPORT_FORMAT} (not a mimo scene report)"),
        ));
    }
    let variant = match options.variant.as_deref() {
        Some(variant) => variant.to_string(),
        None => report["outcome_variant"]
            .as_str()
            .ok_or_else(|| {
                CliError::new(
                    "E_INPUT",
                    "report has no outcome_variant; pass --variant explicitly",
                )
            })?
            .to_string(),
    };
    let variant_obj = report
        .get("variants")
        .and_then(|variants| variants.get(&variant))
        .ok_or_else(|| {
            CliError::new(
                "E_INPUT",
                format!("report has no variant {variant:?} (known variants: B0..B3)"),
            )
        })?;
    let files = variant_obj
        .get("fir_files")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            CliError::new(
                "E_INPUT",
                format!("variant {variant:?} has no fir_files array"),
            )
        })?;
    if files.is_empty() {
        return Err(CliError::new(
            "E_INPUT",
            format!("variant {variant:?} lists no FIR files"),
        ));
    }

    let mut scene_firs = Vec::with_capacity(files.len());
    let mut sources = Vec::with_capacity(files.len());
    for (index, entry) in files.iter().enumerate() {
        let path = entry.get("path").and_then(Value::as_str).ok_or_else(|| {
            CliError::new(
                "E_INPUT",
                format!("variant {variant:?} FIR entry {index} has no path"),
            )
        })?;
        let file_name = path.rsplit('/').next().filter(|name| !name.is_empty());
        let file_name = file_name.ok_or_else(|| {
            CliError::new(
                "E_INPUT",
                format!("variant {variant:?} FIR entry {index} path {path:?} has no file name"),
            )
        })?;
        let source = entry
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or(file_name)
            .to_string();
        sources.push(source.clone());
        scene_firs.push(SceneFir {
            source,
            file: options.fir_dir.join(file_name),
        });
    }

    let mut lanes = Vec::with_capacity(scene_firs.len());
    for (index, scene_fir) in scene_firs.iter().enumerate() {
        let taps = read_fir_csv(&scene_fir.file).map_err(|err| {
            let code = if err.code() == FIR_CSV_IO {
                "E_IO"
            } else {
                "E_INPUT"
            };
            CliError::new(
                code,
                format!(
                    "source {} (lane {index}) FIR {}: {}",
                    scene_fir.source,
                    scene_fir.file.display(),
                    err.message()
                ),
            )
        })?;
        lanes.push(LaneFir {
            lane: index as u16,
            taps,
        });
    }

    if lanes.len() > usize::from(u16::MAX) {
        return Err(CliError::new("E_INPUT", "too many FIR lanes"));
    }
    let channels = options.channels.unwrap_or(lanes.len() as u16);
    let config = FirPlanConfig::new(options.block_size, options.sample_rate, channels);
    let compiled = compile_fir_plan(&lanes, &config)
        .map_err(|err| CliError::new(err.code(), err.message().to_string()))?;
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
    std::fs::write(&options.out, compiled.bytes()).map_err(|err| {
        CliError::new("E_WRITE", format!("write {}: {err}", options.out.display()))
    })?;
    let taps_total: usize = lanes.iter().map(|lane| lane.taps.len()).sum();
    let summary = serde_json::json!({
        "ok": true,
        "tool": "khaen-scene-plan",
        "variant": variant,
        "lanes": lanes.len(),
        "taps": taps_total,
        "sources": sources,
        "channels": channels,
        "sample_rate": options.sample_rate,
        "block_size": options.block_size,
        "sha256": compiled.sha256(),
        "graph_id": compiled.graph_id(),
    });
    println!("{summary}");
    Ok(())
}

fn parse_args() -> Result<Options, CliError> {
    let mut report = None;
    let mut fir_dir = None;
    let mut out = None;
    let mut variant = None;
    let mut sample_rate = None;
    let mut block_size = 128u32;
    let mut channels = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--report" => report = Some(required_path(&mut args, "--report")?),
            "--fir-dir" => fir_dir = Some(required_path(&mut args, "--fir-dir")?),
            "--out" => out = Some(required_path(&mut args, "--out")?),
            "--variant" => {
                variant = Some(
                    args.next()
                        .ok_or_else(|| CliError::new("E_USAGE", "--variant needs a name"))?,
                )
            }
            "--sample-rate" => {
                let raw = args
                    .next()
                    .ok_or_else(|| CliError::new("E_USAGE", "--sample-rate needs an integer"))?;
                sample_rate = Some(raw.parse::<u32>().map_err(|_| {
                    CliError::new(
                        "E_USAGE",
                        format!("--sample-rate must be an integer, got {raw:?}"),
                    )
                })?);
            }
            "--block-size" => {
                let raw = args
                    .next()
                    .ok_or_else(|| CliError::new("E_USAGE", "--block-size needs an integer"))?;
                block_size = raw.parse::<u32>().map_err(|_| {
                    CliError::new(
                        "E_USAGE",
                        format!("--block-size must be an integer, got {raw:?}"),
                    )
                })?;
            }
            "--channels" => {
                let raw = args
                    .next()
                    .ok_or_else(|| CliError::new("E_USAGE", "--channels needs an integer"))?;
                channels = Some(raw.parse::<u16>().map_err(|_| {
                    CliError::new(
                        "E_USAGE",
                        format!("--channels must be an integer, got {raw:?}"),
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
    let report = report.ok_or_else(|| {
        CliError::new(
            "E_USAGE",
            "khaen-scene-plan --report REPORT --fir-dir DIR --out FILE --sample-rate 48000 [--variant B3] [--block-size 128] [--channels N]",
        )
    })?;
    let fir_dir = fir_dir.ok_or_else(|| CliError::new("E_USAGE", "--fir-dir is required"))?;
    let out = out.ok_or_else(|| CliError::new("E_USAGE", "--out is required"))?;
    let sample_rate = sample_rate
        .ok_or_else(|| CliError::new("E_USAGE", "--sample-rate is required (48000 or 96000)"))?;
    Ok(Options {
        report,
        fir_dir,
        out,
        variant,
        block_size,
        sample_rate,
        channels,
    })
}

fn required_path(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<PathBuf, CliError> {
    args.next()
        .map(PathBuf::from)
        .ok_or_else(|| CliError::new("E_USAGE", format!("{flag} needs a path")))
}
