//! rt-check CLI 入口（WBS T06）。
//!
//! 退出码：0 = PASS；1 = 检查失败；2 = 用法/未知命令；3 = 子命令尚未实现（有明确 owner）。
//! 工具离线运行、无外部依赖、不参与 RT 路径。

use std::path::PathBuf;
use std::process::ExitCode;

use khaen_rt_check::{abi_layout, drop_audit, forbid, Report, TOOL_VERSION};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("abi-layout") => run_report("abi-layout", abi_layout::run()),
        Some("forbid") => match root_arg(&args[2..]) {
            Ok(root) => match forbid::scan_path(&root) {
                Ok(report) => run_report("forbid", report),
                Err(err) => fail_io("forbid", err),
            },
            Err(code) => code,
        },
        Some("drop-audit") => {
            let mut report = drop_audit::audit_types();
            match root_arg(&args[2..]) {
                Ok(root) => match drop_audit::scan_path(&root) {
                    Ok(scanned) => report.merge(scanned),
                    Err(err) => return fail_io("drop-audit", err),
                },
                Err(code) => return code,
            }
            run_report("drop-audit", report)
        }
        Some("plan-hash") => not_implemented("plan-hash", "T12/T13（plan 打包与 graph_id 规范化）"),
        Some("state-machine") => not_implemented("state-machine", "T21/T26（状态机模型测试）"),
        Some("-h") | Some("--help") => {
            print_help();
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("rt-check: unknown command: {other}");
            print_help();
            ExitCode::from(2)
        }
        None => {
            print_help();
            ExitCode::from(2)
        }
    }
}

fn root_arg(args: &[String]) -> Result<PathBuf, ExitCode> {
    let mut root: Option<PathBuf> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--root" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    eprintln!("rt-check: --root 需要路径参数");
                    return Err(ExitCode::from(2));
                };
                root = Some(PathBuf::from(value));
            }
            other if other.starts_with("--") => {
                eprintln!("rt-check: 未知参数: {other}");
                return Err(ExitCode::from(2));
            }
            other => root = Some(PathBuf::from(other)),
        }
        index += 1;
    }
    Ok(root.unwrap_or_else(|| PathBuf::from("crates")))
}

fn run_report(kind: &str, report: Report) -> ExitCode {
    if report.ok() {
        println!("PASS {kind} scanned={}", report.scanned);
        ExitCode::SUCCESS
    } else {
        for violation in &report.violations {
            eprintln!(
                "FAIL {}:{} [{}] {}",
                violation.file, violation.line, violation.rule, violation.detail
            );
        }
        eprintln!(
            "FAIL {kind} scanned={} violations={}",
            report.scanned,
            report.violations.len()
        );
        ExitCode::from(1)
    }
}

fn fail_io(kind: &str, err: std::io::Error) -> ExitCode {
    eprintln!("FAIL {kind}: io error: {err}");
    ExitCode::from(2)
}

fn not_implemented(kind: &str, owner: &str) -> ExitCode {
    eprintln!("NOT_IMPLEMENTED {kind}: T06 scaffold 只提供入口；实现 owner = {owner}");
    ExitCode::from(3)
}

fn print_help() {
    println!(
        "rt-check {} — KHAEN 离线契约检查（WBS T06 scaffold）",
        TOOL_VERSION
    );
    println!("用法:");
    println!("  rt-check abi-layout                  # core-v1 §11 ABI 布局 + §3 时限/容量向量");
    println!("  rt-check forbid [--root PATH]        # RT-MODULE 文件 F-01..F-44 子集扫描（默认 crates）");
    println!("  rt-check drop-audit [--root PATH]    # §13 无 RT 析构类型清单 + RT 模块扫描（默认 crates）");
    println!("  rt-check plan-hash                   # NOT_IMPLEMENTED（owner T12/T13）");
    println!("  rt-check state-machine               # NOT_IMPLEMENTED（owner T21/T26）");
    println!("退出码: 0=PASS 1=检查失败 2=用法/IO 3=尚未实现");
}
