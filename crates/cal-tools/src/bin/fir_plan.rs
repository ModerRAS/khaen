//! khaen FIR -> 方案 CLI：把 mimo 单点 FIR（时域冲激响应 CSV）编译成 plan ABI v3 方案文件。
//!
//! 用法：
//!
//!   khaen-fir-plan --fir lane0.csv [--fir lane1.csv ...] --out plan.bin \
//!       --sample-rate 48000 [--block-size 128] [--channels N]
//!
//! 第 i 个 `--fir` 对应 lane i（从 0 开始）；未给出的 lane 在方案里保持恒等直通。
//! CSV：空行与以 `#` 开头的注释行跳过，其余每行一个抽头（与 mimo `fir_csv` 格式一致）。
//!
//! 退出码：
//!
//! - 0 = 成功（写方案文件并打印 sha256 / graph_id）
//! - 2 = 输入/用法/配置错误（E_USAGE / E_IO / E_INPUT / E_FIR_PLAN_*）
//! - 1 = 写输出失败（E_WRITE）
//!
//! 只读 CSV、只写方案文件；不碰设备（真机测量/下发是 T42 设备侧流程，NOT_RUN）。

use std::path::PathBuf;
use std::process::ExitCode;

use khaen_calibration::fir_plan::{compile_fir_plan, FirPlanConfig, LaneFir};

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
            eprintln!("khaen-fir-plan: {}: {}", err.code, err.message);
            ExitCode::from(if err.code == "E_WRITE" {
                EXIT_WRITE
            } else {
                EXIT_INPUT
            })
        }
    }
}

struct Options {
    firs: Vec<PathBuf>,
    out: PathBuf,
    config: FirPlanConfig,
}

fn run() -> Result<(), CliError> {
    let options = parse_args()?;
    let mut lanes = Vec::with_capacity(options.firs.len());
    for (index, path) in options.firs.iter().enumerate() {
        let taps = read_fir_csv(path)?;
        lanes.push(LaneFir {
            lane: index as u16,
            taps,
        });
    }
    let compiled = compile_fir_plan(&lanes, &options.config)
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
        "tool": "khaen-fir-plan",
        "lanes": lanes.len(),
        "taps": taps_total,
        "channels": options.config.channels,
        "sample_rate": options.config.sample_rate,
        "block_size": options.config.block_size,
        "sha256": compiled.sha256(),
        "graph_id": compiled.graph_id(),
    });
    println!("{summary}");
    Ok(())
}

fn read_fir_csv(path: &std::path::Path) -> Result<Vec<f64>, CliError> {
    khaen_cal_tools::fir_csv::read_fir_csv(path).map_err(|err| {
        let code = if err.code() == khaen_cal_tools::fir_csv::IO {
            "E_IO"
        } else {
            "E_INPUT"
        };
        CliError::new(code, err.message().to_string())
    })
}
fn parse_args() -> Result<Options, CliError> {
    let mut firs = Vec::new();
    let mut out = None;
    let mut sample_rate = None;
    let mut block_size = 128u32;
    let mut channels = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--fir" => {
                let path = args
                    .next()
                    .ok_or_else(|| CliError::new("E_USAGE", "--fir needs a path"))?;
                firs.push(PathBuf::from(path));
            }
            "--out" => {
                out =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        CliError::new("E_USAGE", "--out needs a path")
                    })?));
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
    if firs.is_empty() {
        return Err(CliError::new(
            "E_USAGE",
            "khaen-fir-plan --fir lane0.csv [--fir ...] --out plan.bin --sample-rate 48000 [--block-size 128] [--channels N]",
        ));
    }
    let out = out.ok_or_else(|| CliError::new("E_USAGE", "--out is required"))?;
    let sample_rate = sample_rate
        .ok_or_else(|| CliError::new("E_USAGE", "--sample-rate is required (48000 or 96000)"))?;
    if firs.len() > usize::from(u16::MAX) {
        return Err(CliError::new("E_USAGE", "too many --fir files"));
    }
    let channels = channels.unwrap_or(firs.len() as u16);
    Ok(Options {
        firs,
        out,
        config: FirPlanConfig::new(block_size, sample_rate, channels),
    })
}
