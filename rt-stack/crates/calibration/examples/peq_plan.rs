//! S2（PEQ-only）plan 生成器（plan ABI v2 per-role IIR；GAP-T66-3）。
//!
//! 用法：peq_plan [--out DIR] [--f0 HZ] [--gain-db DB] [--q Q]
//! 产物：DIR/plan.bin（T66 compile/plan.bin 同布局）、DIR/peq-report.json（解析响应/校验）。
//!
//! 生成 factorized identity A/G/P + 单 role PEQ 的 plan ABI v2 字节，供 khaen-rt-host
//! graph=plan-arena（backend=fake，[sink]/[capture] 离线）做数字域验证。不打开设备、不出声。

use std::process::ExitCode;

use khaen_calibration::peq_plan::{
    compile_peq_plan, PeqBiquad, PeqPlanRequest, PeqRolePlanInput, PEQ_PLAN_SCHEMA,
};

fn hex32(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn main() -> ExitCode {
    let mut out_dir = "/tmp/peq-plan".to_string();
    let mut f0 = 100.0f64;
    let mut gain_db = -6.0f64;
    let mut q = 1.0f64;
    let mut no_peq = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => match args.next() {
                Some(value) => out_dir = value,
                None => return fail("--out 需要目录参数"),
            },
            "--f0" => match args.next().and_then(|value| value.parse::<f64>().ok()) {
                Some(value) => f0 = value,
                None => return fail("--f0 需要数字"),
            },
            "--gain-db" => match args.next().and_then(|value| value.parse::<f64>().ok()) {
                Some(value) => gain_db = value,
                None => return fail("--gain-db 需要数字"),
            },
            "--q" => match args.next().and_then(|value| value.parse::<f64>().ok()) {
                Some(value) => q = value,
                None => return fail("--q 需要数字"),
            },
            "--no-peq" => no_peq = true,
            other => return fail(&format!("未知参数 {other}")),
        }
    }
    let section = match PeqBiquad::peaking_eq(48_000.0, f0, gain_db, q) {
        Ok(section) => section,
        Err(detail) => return fail(&detail),
    };
    let request = PeqPlanRequest {
        sample_rate_hz: 48_000,
        block_size: 128,
        n_in: 2,
        container_channels: 2,
        workers: 4,
        roles: if no_peq {
            vec![
                PeqRolePlanInput::identity(0, "FL", 0),
                PeqRolePlanInput::identity(1, "FR", 1),
            ]
        } else {
            vec![
                PeqRolePlanInput::identity(0, "FL", 0).with_peq(section),
                PeqRolePlanInput::identity(1, "FR", 1),
            ]
        },
    };
    let artifact = match compile_peq_plan(&request) {
        Ok(artifact) => artifact,
        Err(detail) => return fail(&detail),
    };
    let dir = std::path::Path::new(&out_dir);
    if let Err(error) = std::fs::create_dir_all(dir) {
        return fail(&format!("创建输出目录失败 {out_dir}: {error}"));
    }
    let plan_path = dir.join("plan.bin");
    if let Err(error) = std::fs::write(&plan_path, &artifact.plan_bytes) {
        return fail(&format!("写 plan.bin 失败: {error}"));
    }
    let mut grid: Vec<String> = Vec::new();
    let mut freq = 20.0f64;
    while freq <= 20_000.0 {
        grid.push(format!(
            "{{\"hz\":{freq:.3},\"expected_db\":{:.6}}}",
            artifact.role_response_db(0, freq)
        ));
        freq *= 2.0f64.powf(1.0 / 12.0);
    }
    let report = format!(
        "{{\"schema\":\"{}\",\"plan_bytes\":{},\"plan_sha256_hex_requested\":false,\"graph_id\":\"{}\",\"abi_version\":{},\"role_iir_roles\":{},\"role_iir_sections\":{},\"peq\":{{\"f0_hz\":{f0},\"gain_db\":{gain_db},\"q\":{q}}},\"role0_response\":[{}]}}\n",
        PEQ_PLAN_SCHEMA,
        artifact.plan_bytes.len(),
        hex32(&artifact.graph_id),
        artifact.header.abi_version,
        artifact.iir_entries.len(),
        artifact.section_count(),
        grid.join(",")
    );
    let report_path = dir.join("peq-report.json");
    if let Err(error) = std::fs::write(&report_path, report) {
        return fail(&format!("写 peq-report.json 失败: {error}"));
    }
    println!(
        "{{\"plan\":\"{}\",\"report\":\"{}\",\"graph_id\":\"{}\",\"plan_bytes\":{},\"abi_version\":{},\"sections\":{}}}",
        plan_path.display(),
        report_path.display(),
        hex32(&artifact.graph_id),
        artifact.plan_bytes.len(),
        artifact.header.abi_version,
        artifact.section_count()
    );
    ExitCode::SUCCESS
}

fn fail(message: &str) -> ExitCode {
    eprintln!("peq_plan: {message}");
    ExitCode::FAILURE
}
