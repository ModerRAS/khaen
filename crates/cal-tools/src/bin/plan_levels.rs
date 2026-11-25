//! khaen 预测电平 CLI：从方案 ABI v3 推出每个 lane 的预测带域电平（闭环复测的 predicted 侧）。
//!
//! 用法：
//!
//!   khaen-plan-levels --plan plan.bin --out levels.json [--centers 25,31.5,40,...]
//!
//! 输出：
//!
//! - out：确定性电平 JSON（khaen-closed-loop-levels/v1，键序固定、10 位有效数字、末尾换行），
//!   可直接作为 khaen-closed-loop --predicted 的输入；
//! - stdout 一行 JSON：{ok, tool, plan_sha256, levels_sha256, points, bands}。
//!
//! 退出码：
//!
//! - 0 = 成功
//! - 2 = 输入/用法/方案错误（E_USAGE / E_IO / E_PLAN / E_PLAN_LEVELS_INPUT / E_PLAN_LEVELS_CONFIG / E_PLAN_LEVELS_PLAN）
//! - 1 = 写输出失败（E_WRITE）
//!
//! 诚实边界：预测是保护段（PEQ + 每 lane 增益）的**相对频响**，不含房间/单元/麦克风；
//! 实测侧必须用同一参考归一化。内容/分频/矩阵/路由非恒等时直接报错，不静默少算。

use std::path::PathBuf;
use std::process::ExitCode;

use khaen_calibration::plan_levels::{predicted_levels, DEFAULT_CENTERS_HZ};
use khaen_calibration::{levels_bytes, levels_sha256};
use khaen_core::plan::Plan;

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
            eprintln!("khaen-plan-levels: {}: {}", err.code, err.message);
            ExitCode::from(if err.code == "E_WRITE" {
                EXIT_WRITE
            } else {
                EXIT_INPUT
            })
        }
    }
}

fn run() -> Result<(), CliError> {
    let plan_path = required_path("--plan")?;
    let out_path = required_path("--out")?;
    let centers = parse_centers()?;

    let plan_bytes = std::fs::read(&plan_path).map_err(|err| {
        CliError::new("E_IO", format!("read plan {}: {err}", plan_path.display()))
    })?;
    let plan = Plan::decode(&plan_bytes).map_err(|err| {
        CliError::new(
            "E_PLAN",
            format!("decode plan {}: {err}", plan_path.display()),
        )
    })?;
    let levels = predicted_levels(&plan, &centers)
        .map_err(|err| CliError::new(err.code(), err.message().to_string()))?;
    let bytes = levels_bytes(&levels);
    let sha = levels_sha256(&levels);

    if out_path.exists() {
        return Err(CliError::new(
            "E_WRITE",
            format!("output file {} already exists", out_path.display()),
        ));
    }
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|err| {
                CliError::new("E_WRITE", format!("create {}: {err}", parent.display()))
            })?;
        }
    }
    std::fs::write(&out_path, &bytes)
        .map_err(|err| CliError::new("E_WRITE", format!("write {}: {err}", out_path.display())))?;

    let plan_sha = khaen_core::sha256::sha256_hex(&plan_bytes);
    let bands = levels
        .points
        .first()
        .map(|point| point.bands.len())
        .unwrap_or(0);
    println!(
        "{{\"ok\":true,\"tool\":\"khaen-plan-levels\",\"plan_sha256\":\"{plan_sha}\",\"levels_sha256\":\"{sha}\",\"points\":{},\"bands\":{bands}}}",
        levels.points.len()
    );
    Ok(())
}

fn required_path(flag: &str) -> Result<PathBuf, CliError> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == flag {
            return args
                .next()
                .map(PathBuf::from)
                .ok_or_else(|| CliError::new("E_USAGE", format!("{flag} needs a path")));
        }
    }
    Err(CliError::new(
        "E_USAGE",
        format!("{flag} is required (khaen-plan-levels --plan FILE --out FILE [--centers ...])"),
    ))
}

fn parse_centers() -> Result<Vec<f64>, CliError> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--centers" {
            let raw = args
                .next()
                .ok_or_else(|| CliError::new("E_USAGE", "--centers needs a comma list"))?;
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
            return Ok(centers);
        }
    }
    Ok(DEFAULT_CENTERS_HZ.to_vec())
}
