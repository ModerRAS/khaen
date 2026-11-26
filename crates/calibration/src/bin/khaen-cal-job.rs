//! `khaen-cal-job`：离线闭环校准 job 的命令行入口（T70h-b）。
//!
//! 走的是与 daemon REST 完全相同的 `JobManager + OfflineJobBackend` 路径：
//! `validate -> compile -> levels -> closed_loop -> bundle`，证据包落 `--out`。
//!
//! 用法：
//! ```text
//! khaen-cal-job --baseline baseline.wav --applied applied.wav --plan plan.bin --out DIR
//!               [--point P01] [--lane 0] [--centers 1000,2000] [--tolerance 2.0]
//!               [--min-bands 3] [--window 4096]
//! khaen-cal-job --baseline b.wav --applied a.wav --design design.json --out DIR
//! ```
//!
//! 退出码（与 `khaen-closed-loop-run` 的三态口径一致）：0=PASS/完成，3=FAIL，
//! 4=INCOMPLETE，2=输入/参数错误，1=job 运行失败（稳定码见 job 快照）。

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use khaen_calibration::job::{
    ExitDisposition, JobKind, JobManager, JobManagerConfig, JobRequest, JobStage, JobState,
    SafeOutputController, SafetyError,
};
use khaen_calibration::offline_job::{
    OfflineJobBackend, OfflineJobConfig, CLOSED_LOOP_SUMMARY_FILE,
};
use khaen_calibration::plan_levels::DEFAULT_CENTERS_HZ;

const EXIT_PASS: u8 = 0;
const EXIT_RUN_FAILED: u8 = 1;
const EXIT_BAD_INPUT: u8 = 2;
const EXIT_FAIL: u8 = 3;
const EXIT_INCOMPLETE: u8 = 4;

/// 离线 CLI 不触碰设备：安全控制面只做占位（真实 apply/rollback 属 daemon/设备任务）。
struct HeadlessSafety;

impl SafeOutputController for HeadlessSafety {
    fn acquire_calibration_owner(&self) -> Result<(), SafetyError> {
        Ok(())
    }

    fn release_calibration_owner(&self, _disposition: ExitDisposition) -> Result<(), SafetyError> {
        Ok(())
    }

    fn restore_last_good(&self) -> Result<String, SafetyError> {
        Err(SafetyError::new(
            "LAST_GOOD_UNAVAILABLE",
            "offline CLI has no playback graph to restore",
        ))
    }

    fn activate_safe_mute(&self) -> Result<(), SafetyError> {
        Err(SafetyError::new(
            "SAFE_MUTE_UNAVAILABLE",
            "offline CLI has no device to mute",
        ))
    }

    fn last_good_version(&self) -> Option<String> {
        None
    }
}

#[derive(Default)]
struct Args {
    baseline: Option<PathBuf>,
    applied: Option<PathBuf>,
    plan: Option<PathBuf>,
    design: Option<PathBuf>,
    out: Option<PathBuf>,
    point: Option<String>,
    lane: Option<u16>,
    centers: Option<Vec<f64>>,
    tolerance: Option<f64>,
    min_bands: Option<usize>,
    window: Option<usize>,
}

fn parse_args(arguments: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut args = Args::default();
    let mut queue: VecDeque<String> = arguments.collect();
    while let Some(flag) = queue.pop_front() {
        let mut value = || {
            queue
                .pop_front()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--baseline" => args.baseline = Some(PathBuf::from(value()?)),
            "--applied" => args.applied = Some(PathBuf::from(value()?)),
            "--plan" => args.plan = Some(PathBuf::from(value()?)),
            "--design" => args.design = Some(PathBuf::from(value()?)),
            "--out" => args.out = Some(PathBuf::from(value()?)),
            "--point" => args.point = Some(value()?),
            "--lane" => {
                args.lane = Some(value()?.parse().map_err(|_| "--lane must be u16")?);
            }
            "--centers" => {
                let text = value()?;
                let mut centers = Vec::new();
                for part in text.split(',') {
                    centers.push(
                        part.trim()
                            .parse::<f64>()
                            .map_err(|_| format!("--centers has bad number {part:?}"))?,
                    );
                }
                args.centers = Some(centers);
            }
            "--tolerance" => {
                args.tolerance = Some(value()?.parse().map_err(|_| "--tolerance must be f64")?);
            }
            "--min-bands" => {
                args.min_bands = Some(value()?.parse().map_err(|_| "--min-bands must be usize")?);
            }
            "--window" => {
                args.window = Some(value()?.parse().map_err(|_| "--window must be usize")?);
            }
            "--help" | "-h" => return Err(String::new()),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(args)
}

fn usage() -> &'static str {
    "usage: khaen-cal-job --baseline a.wav --applied b.wav (--plan p.bin | --design d.json) --out DIR \
        [--point P01] [--lane 0] [--centers 1000.0,2000.0] [--tolerance 2.0] [--min-bands 3] [--window 4096]"
}

fn build_config(args: &Args) -> Result<OfflineJobConfig, String> {
    let baseline = args.baseline.clone().ok_or("--baseline is required")?;
    let applied = args.applied.clone().ok_or("--applied is required")?;
    if args.plan.is_none() && args.design.is_none() {
        return Err("either --plan (ABI v3) or --design (modal-peq/json) is required".to_string());
    }
    let mut config = OfflineJobConfig::new(baseline, applied);
    config.plan_bin = args.plan.clone();
    config.design_json = args.design.clone();
    if let Some(point) = &args.point {
        config.point = point.clone();
    }
    if let Some(lane) = args.lane {
        config.lane = lane;
    }
    if let Some(centers) = &args.centers {
        config.centers_hz = centers.clone();
    } else {
        config.centers_hz = DEFAULT_CENTERS_HZ.to_vec();
    }
    if let Some(tolerance) = args.tolerance {
        config.tolerance_db = tolerance;
    }
    if let Some(min_bands) = args.min_bands {
        config.min_compared_bands = min_bands;
    }
    if let Some(window) = args.window {
        config.window_len = window;
    }
    Ok(config)
}

fn request() -> JobRequest {
    JobRequest {
        kind: JobKind::SinglePointOffline,
        label: "khaen-cal-job offline".to_string(),
        layout: "2.0".to_string(),
        sample_rate: 48_000,
        design_points: 1,
        holdout_points: 1,
        repeats: 1,
        drive_level_dbfs: -50.0,
        audible_sweep_authorized: true,
        auto_apply: false,
        expected_applied_version: None,
        inputs: Vec::new(),
    }
}

fn verdict_exit(snapshot: &khaen_calibration::job::JobSnapshot) -> u8 {
    let summary = PathBuf::from(&snapshot.artifacts_dir).join(CLOSED_LOOP_SUMMARY_FILE);
    let Ok(text) = std::fs::read_to_string(&summary) else {
        return EXIT_RUN_FAILED;
    };
    if text.contains("\"verdict\":\"PASS\"") {
        EXIT_PASS
    } else if text.contains("\"verdict\":\"FAIL\"") {
        EXIT_FAIL
    } else if text.contains("\"verdict\":\"INCOMPLETE\"") {
        EXIT_INCOMPLETE
    } else {
        EXIT_RUN_FAILED
    }
}

fn run(arguments: impl Iterator<Item = String>) -> u8 {
    let args = match parse_args(arguments) {
        Ok(args) => args,
        Err(message) => {
            if !message.is_empty() {
                eprintln!("error: {message}");
            }
            eprintln!("{}", usage());
            return EXIT_BAD_INPUT;
        }
    };
    let config = match build_config(&args) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("error: {message}");
            eprintln!("{}", usage());
            return EXIT_BAD_INPUT;
        }
    };
    let out = match &args.out {
        Some(out) => out.clone(),
        None => {
            eprintln!("error: --out is required");
            return EXIT_BAD_INPUT;
        }
    };
    if let Err(message) = config.validate() {
        eprintln!("error: {message}");
        return EXIT_BAD_INPUT;
    }
    let manager = match JobManager::new(
        JobManagerConfig {
            artifacts_root: out,
            checkpoint_path: None,
            apply_confirm_timeout: None,
        },
        Arc::new(HeadlessSafety),
    ) {
        Ok(manager) => manager,
        Err(error) => {
            eprintln!("error: {error}");
            return EXIT_RUN_FAILED;
        }
    };
    if let Err(error) = manager.start(request(), Box::new(OfflineJobBackend::new(config))) {
        eprintln!("error: {error}");
        return EXIT_BAD_INPUT;
    }
    let snapshot = match manager.wait_terminal(Duration::from_secs(600)) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            eprintln!("error: {error}");
            return EXIT_RUN_FAILED;
        }
    };
    println!("{}", snapshot.to_json());
    if snapshot.state != JobState::Complete {
        if let Some(failure) = &snapshot.failure {
            eprintln!(
                "job failed at {}: {}",
                failure
                    .stage
                    .map(JobStage::as_str)
                    .unwrap_or("(unknown stage)"),
                failure.code
            );
        }
        return EXIT_RUN_FAILED;
    }
    verdict_exit(&snapshot)
}

fn main() -> ExitCode {
    ExitCode::from(run(std::env::args().skip(1)))
}
