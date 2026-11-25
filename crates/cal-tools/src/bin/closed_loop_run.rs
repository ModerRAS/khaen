//! khaen 闭环复测运行包 CLI：方案 + 基线录音 + 施加录音 → 四份可审计产物。
//!
//! 用法：
//!
//!   khaen-closed-loop-run --plan plan.bin --baseline baseline.wav --applied applied.wav \
//!       --out dir/ [--point P01] [--lane 0] [--centers 25,31.5,40,...] [--window 4096] \
//!       [--tolerance 2.0] [--min-bands 3]
//!
//! `--lane` 指定这次测量对应方案里的哪条 lane（默认 0）；预测侧取该 lane 的电平，
//! 与两段录音算出的实测电平按点对账。
//!
//! 产物（out 目录内，全部确定性）：predicted.json / observed.json / report.json / manifest.json。
//! 清单里记录方案与两段录音的 sha256、三份输出的 sha256、判定与配置，便于交接与复现。
//!
//! 退出码：
//!
//! - 0 = PASS（所有可比较带在容差内且数量足够）
//! - 3 = FAIL（至少一个带超容差）
//! - 4 = INCOMPLETE（可比较带不足）
//! - 2 = 输入/用法/方案错误（E_USAGE / E_IO / E_INPUT / E_PLAN / E_CLOSED_LOOP_RUN_*）
//! - 1 = 写输出失败（E_WRITE）
//!
//! 只读文件、只写输出目录；不碰设备（真机采集是 T42 设备侧流程，NOT_RUN）。

use std::path::PathBuf;
use std::process::ExitCode;

use khaen_calibration::closed_loop_run::build_run_bundle;
use khaen_calibration::plan_levels::DEFAULT_CENTERS_HZ;
use khaen_calibration::wav::{parse_wav, IO as WAV_IO};
use khaen_calibration::{RunConfig, RunInputs, Verdict};
use khaen_core::plan::Plan;

const EXIT_OK: u8 = 0;
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
}

fn main() -> ExitCode {
    match run() {
        Ok(exit_code) => ExitCode::from(exit_code),
        Err(err) => {
            eprintln!("khaen-closed-loop-run: {}: {}", err.code, err.message);
            ExitCode::from(if err.code == "E_WRITE" {
                EXIT_WRITE
            } else {
                EXIT_INPUT
            })
        }
    }
}

struct Options {
    plan: PathBuf,
    baseline: PathBuf,
    applied: PathBuf,
    out: PathBuf,
    config: RunConfig,
}

fn run() -> Result<u8, CliError> {
    let options = parse_args()?;
    let plan_bytes = std::fs::read(&options.plan).map_err(|err| {
        CliError::new(
            "E_IO",
            format!("read plan {}: {err}", options.plan.display()),
        )
    })?;
    let plan = Plan::decode(&plan_bytes).map_err(|err| {
        CliError::new(
            "E_PLAN",
            format!("decode plan {}: {err}", options.plan.display()),
        )
    })?;
    let plan_sha256 = khaen_core::sha256::sha256_hex(&plan_bytes);

    let baseline_bytes = std::fs::read(&options.baseline).map_err(|err| {
        CliError::new(
            "E_IO",
            format!("read baseline {}: {err}", options.baseline.display()),
        )
    })?;
    let baseline = parse_wav(&baseline_bytes).map_err(|err| {
        CliError::new(
            wav_code(&err),
            format!("baseline {}: {}", options.baseline.display(), err.message()),
        )
    })?;
    let baseline_sha256 = khaen_core::sha256::sha256_hex(&baseline_bytes);

    let applied_bytes = std::fs::read(&options.applied).map_err(|err| {
        CliError::new(
            "E_IO",
            format!("read applied {}: {err}", options.applied.display()),
        )
    })?;
    let applied = parse_wav(&applied_bytes).map_err(|err| {
        CliError::new(
            wav_code(&err),
            format!("applied {}: {}", options.applied.display(), err.message()),
        )
    })?;
    let applied_sha256 = khaen_core::sha256::sha256_hex(&applied_bytes);

    if (baseline.sample_rate_hz - applied.sample_rate_hz).abs() > 1e-9 {
        return Err(CliError::new(
            "E_CLOSED_LOOP_RUN_ALIGNMENT",
            format!(
                "baseline {} Hz and applied {} Hz sample rates differ",
                baseline.sample_rate_hz, applied.sample_rate_hz
            ),
        ));
    }
    let inputs = RunInputs {
        plan: &plan,
        plan_sha256: &plan_sha256,
        baseline_samples: &baseline.samples,
        baseline_wav_sha256: &baseline_sha256,
        applied_samples: &applied.samples,
        applied_wav_sha256: &applied_sha256,
        sample_rate_hz: baseline.sample_rate_hz,
    };
    let bundle = build_run_bundle(&inputs, &options.config)
        .map_err(|err| CliError::new(err.code(), err.message().to_string()))?;

    let verdict = bundle.verdict();
    let report_sha256 = bundle.report_sha256().to_string();
    let manifest_sha256 = bundle.manifest_sha256();
    let report = bundle.report();
    let summary = serde_json::json!({
        "ok": true,
        "tool": "khaen-closed-loop-run",
        "verdict": verdict.as_str(),
        "point": options.config.point,
        "bands": options.config.centers_hz.len(),
        "compared_bands": report.compared_bands,
        "missing_bands": report.missing_bands,
        "violations": report.violations,
        "report_sha256": report_sha256,
        "manifest_sha256": manifest_sha256,
    });

    if options.out.exists() {
        let mut entries = std::fs::read_dir(&options.out).map_err(|err| {
            CliError::new("E_WRITE", format!("read {}: {err}", options.out.display()))
        })?;
        if entries.next().is_some() {
            return Err(CliError::new(
                "E_WRITE",
                format!("output directory {} is not empty", options.out.display()),
            ));
        }
    }
    std::fs::create_dir_all(&options.out).map_err(|err| {
        CliError::new(
            "E_WRITE",
            format!("create {}: {err}", options.out.display()),
        )
    })?;
    for (name, bytes) in bundle.files() {
        let path = options.out.join(name);
        std::fs::write(&path, bytes)
            .map_err(|err| CliError::new("E_WRITE", format!("write {}: {err}", path.display())))?;
    }
    println!("{summary}");
    Ok(match verdict {
        Verdict::Pass => EXIT_OK,
        Verdict::Fail => EXIT_FAIL,
        Verdict::Incomplete => EXIT_INCOMPLETE,
    })
}

fn wav_code(err: &khaen_calibration::WavError) -> &'static str {
    if err.code() == WAV_IO {
        "E_IO"
    } else {
        "E_INPUT"
    }
}

fn parse_args() -> Result<Options, CliError> {
    let mut plan = None;
    let mut baseline = None;
    let mut applied = None;
    let mut out = None;
    let mut config = RunConfig::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--plan" => plan = Some(required_path(&mut args, "--plan")?),
            "--baseline" => baseline = Some(required_path(&mut args, "--baseline")?),
            "--applied" => applied = Some(required_path(&mut args, "--applied")?),
            "--out" => out = Some(required_path(&mut args, "--out")?),
            "--lane" => {
                let raw = args
                    .next()
                    .ok_or_else(|| CliError::new("E_USAGE", "--lane needs an integer"))?;
                config.lane = raw.parse::<u16>().map_err(|_| {
                    CliError::new("E_USAGE", format!("--lane must be an integer, got {raw:?}"))
                })?;
            }
            "--point" => {
                config.point = args
                    .next()
                    .ok_or_else(|| CliError::new("E_USAGE", "--point needs a name"))?;
            }
            "--centers" => {
                let raw = args
                    .next()
                    .ok_or_else(|| CliError::new("E_USAGE", "--centers needs a comma list"))?;
                config.centers_hz = parse_centers(&raw)?;
            }
            "--window" => {
                let raw = args
                    .next()
                    .ok_or_else(|| CliError::new("E_USAGE", "--window needs an integer"))?;
                config.window_len = raw.parse::<usize>().map_err(|_| {
                    CliError::new(
                        "E_USAGE",
                        format!("--window must be an integer, got {raw:?}"),
                    )
                })?;
            }
            "--tolerance" => {
                let raw = args
                    .next()
                    .ok_or_else(|| CliError::new("E_USAGE", "--tolerance needs a number"))?;
                config.tolerance_db = raw.parse::<f64>().map_err(|_| {
                    CliError::new(
                        "E_USAGE",
                        format!("--tolerance must be a number, got {raw:?}"),
                    )
                })?;
            }
            "--min-bands" => {
                let raw = args
                    .next()
                    .ok_or_else(|| CliError::new("E_USAGE", "--min-bands needs an integer"))?;
                config.min_compared_bands = raw.parse::<usize>().map_err(|_| {
                    CliError::new(
                        "E_USAGE",
                        format!("--min-bands must be an integer, got {raw:?}"),
                    )
                })?;
            }
            other => {
                return Err(CliError::new(
                    "E_USAGE",
                    format!("unknown argument {other:?}"),
                ))
            }
        }
    }
    let plan = plan.ok_or_else(|| {
        CliError::new(
            "E_USAGE",
            "khaen-closed-loop-run --plan FILE --baseline WAV --applied WAV --out DIR [--point P01] [--lane 0] [--centers ...] [--window 4096] [--tolerance 2.0] [--min-bands 3]",
        )
    })?;
    let baseline = baseline.ok_or_else(|| CliError::new("E_USAGE", "--baseline is required"))?;
    let applied = applied.ok_or_else(|| CliError::new("E_USAGE", "--applied is required"))?;
    let out = out.ok_or_else(|| CliError::new("E_USAGE", "--out is required"))?;
    if config.centers_hz.is_empty() {
        config.centers_hz = DEFAULT_CENTERS_HZ.to_vec();
    }
    Ok(Options {
        plan,
        baseline,
        applied,
        out,
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
