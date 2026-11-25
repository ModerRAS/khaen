//! measured 多点（design>=2 + 独立 validation>=1）离线探针（EXPERIMENT；不采音/不碰设备）。
//!
//! 用途：为「生产多点窗口」预先准备并验证代码路径：
//! 1) fixtures：生成合成旧工具 analysis JSON（2 design + 1 独立 validation，FL/FR）
//!    与 spec v2（khaen-cal-measured-design/v2）；全部纯离线、确定性字节。
//! 2) report：读回 spec v2 -> 生产 measured 构建 -> T64 fit -> 写
//!    measured summary / T64 report / 多点证据（逐位置残差 + 座位间 spread + holdout 预测口径）。
//!
//! 纪律：
//! - 合成曲线不是测量：provenance 明写 synthetic_offline；产物 EXPERIMENT/NON_ACCEPTABLE；
//! - 真实多点测量窗口 / apply 后 holdout remeasure / T6 / T68 仍 NOT_RUN；
//! - 不写 READY/PASS/CERTIFIED。
//!
//! 用法（仓库根）：
//!   cargo run -p khaen-calibration --offline --example multipoint_probe -- fixtures --out DIR
//!   cargo run -p khaen-calibration --offline --example multipoint_probe -- report --spec DIR/spec.json --out DIR/report

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use khaen_calibration::job::sha256_hex;
use khaen_calibration::measured_design::{
    build_measured_design, load_measured_spec, multipoint_evidence_json,
    MEASURED_MULTIPOINT_SCHEMA, MEASURED_SPEC_SCHEMA_V2,
};
use khaen_calibration::peq_fit::{fit_peq, PeqFitRequest};

const FS_HZ: u32 = 48_000;

fn main() {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_default();
    let opts = parse_args(args.collect());
    let result = match command.as_str() {
        "fixtures" => write_fixtures(&opts),
        "report" => write_report(&opts),
        other => Err(format!("unknown subcommand {other:?} (fixtures|report)")),
    };
    match result {
        Ok(()) => {}
        Err(message) => {
            eprintln!("ERROR: {message}");
            std::process::exit(2);
        }
    }
}

struct Args {
    out_dir: PathBuf,
    spec: Option<PathBuf>,
}

fn parse_args(items: Vec<String>) -> Args {
    let mut args = Args {
        out_dir: PathBuf::new(),
        spec: None,
    };
    let mut index = 0;
    while index < items.len() {
        let key = items[index].as_str();
        let mut next = || {
            index += 1;
            items.get(index).cloned().unwrap_or_default()
        };
        match key {
            "--out" => args.out_dir = PathBuf::from(next()),
            "--spec" => args.spec = Some(PathBuf::from(next())),
            other => eprintln!("warning: unknown argument {other:?}"),
        }
        index += 1;
    }
    args
}

fn bump(freq_hz: f64, center_hz: f64, gain_db: f64, oct_width: f64) -> f64 {
    let distance = (freq_hz / center_hz).log2() / oct_width;
    gain_db * (-0.5 * distance * distance).exp()
}

fn design_a_db(freq_hz: f64) -> f64 {
    bump(freq_hz, 100.0, 6.0, 0.08)
        + bump(freq_hz, 160.0, 4.0, 0.10)
        + bump(freq_hz, 1_200.0, -1.5, 0.5)
}

fn design_b_db(freq_hz: f64) -> f64 {
    0.8 + bump(freq_hz, 100.0, 5.2, 0.08)
        + bump(freq_hz, 160.0, 3.6, 0.10)
        + bump(freq_hz, 300.0, 5.0, 0.06)
}

fn validation_db(freq_hz: f64) -> f64 {
    -0.5 + bump(freq_hz, 100.0, 2.0, 0.10) + bump(freq_hz, 1_000.0, -4.0, 0.30)
}

/// 旧工具 analysis JSON（schema khaen-channel-measurement/v1；1/24 oct 20–20k）。
///
/// 相位约定：工具 phase_deg=0（已按互相关对齐）；delay_samples 声明实测到达延迟，
/// measured 桥按 +2*pi*f*tau/fs 重建 T65 复响应。
fn analysis_json<F: Fn(f64) -> f64>(channel: u64, delay_samples: f64, shape: F) -> String {
    let mut fr = String::new();
    let mut index = 0;
    loop {
        let freq = 20.0 * 2f64.powf(index as f64 / 24.0);
        if freq > 20_000.0 {
            break;
        }
        if !fr.is_empty() {
            fr.push(',');
        }
        let _ = write!(
            fr,
            "{{\"freq_hz\":{freq:.7},\"db\":{:.7},\"phase_deg\":0.0}}",
            shape(freq)
        );
        index += 1;
    }
    format!(
        "{{\"schema\":\"khaen-channel-measurement/v1\",\"channel\":{channel},\"delay_samples\":{delay_samples:.6},\"snr_band_db\":31.0,\"polarity_confidence\":\"high\",\"meta\":{{\"sample_rate\":{FS_HZ},\"sweep_start_hz\":20.0,\"sweep_end_hz\":20000.0}},\"fr\":[{fr}]}}"
    )
}

fn write_analysis(dir: &Path, name: &str, body: &str) -> Result<PathBuf, String> {
    let path = dir.join(name);
    fs::write(&path, body).map_err(|error| format!("write {}: {error}", path.display()))?;
    Ok(path)
}

fn slash(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

fn position_json(label: &str, weight: Option<f64>, fl: &Path, fr: &Path) -> String {
    let weight = weight
        .map(|value| format!(",\"weight\":{value}"))
        .unwrap_or_default();
    format!(
        "{{\"label\":\"{label}\",\"mic_orientation\":\"90deg\"{weight},\"channels\":[{{\"name\":\"FL\",\"analysis\":\"{}\"}},{{\"name\":\"FR\",\"analysis\":\"{}\"}}]}}",
        slash(fl),
        slash(fr)
    )
}

fn write_fixtures(args: &Args) -> Result<(), String> {
    if args.out_dir.as_os_str().is_empty() {
        return Err("fixtures needs --out DIR".to_string());
    }
    let dir = &args.out_dir;
    fs::create_dir_all(dir).map_err(|error| format!("mkdir {}: {error}", dir.display()))?;
    let mut written: Vec<(String, String, usize)> = Vec::new();
    let positions: [(&str, fn(f64) -> f64, Option<f64>); 3] = [
        ("D1", design_a_db, Some(1.0)),
        ("D2", design_b_db, Some(0.8)),
        ("V1", validation_db, None),
    ];
    let mut position_jsons = Vec::new();
    for (label, shape, weight) in positions {
        let fl = write_analysis(
            dir,
            &format!("{label}-FL.analysis.json"),
            &analysis_json(0, 96.0, shape),
        )?;
        let fr = write_analysis(
            dir,
            &format!("{label}-FR.analysis.json"),
            &analysis_json(1, 120.0, shape),
        )?;
        for (path, body_name) in [(&fl, format!("{label}-FL")), (&fr, format!("{label}-FR"))] {
            let bytes =
                fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
            written.push((body_name, sha256_hex(&bytes), bytes.len()));
        }
        position_jsons.push(position_json(label, weight, &fl, &fr));
    }
    let spec = format!(
        r#"{{
  "schema": "{MEASURED_SPEC_SCHEMA_V2}",
  "session_id": "multipoint-pilot-20261122",
  "layout": "stereo-2.0",
  "sample_rate_hz": {FS_HZ},
  "target": "flat",
  "validation": "independent_validation_positions",
  "provenance": "synthetic_multipoint_fixture_offline_not_measured",
  "design_positions": [
    {},
    {}
  ],
  "validation_positions": [
    {}
  ],
  "channel_bands": [
    {{"channel": "FL", "band_hz": [80.0, 16000.0]}},
    {{"channel": "FR", "band_hz": [80.0, 16000.0]}}
  ],
  "dense_to_dac": [0, 1],
  "roles": [
    {{"role_index": 0, "role_id": "FL", "dac": 0, "outputs": [0]}},
    {{"role_index": 1, "role_id": "FR", "dac": 1, "outputs": [1]}}
  ],
  "container_channels": 8,
  "unused_outputs": [2, 3, 4, 5, 6, 7],
  "compile": {{"block_size": 128, "n_in": 2, "container_channels": 8, "low_band_d": 0, "l_g": 8, "l_p": 8}},
  "modal_artifact": null
}}
"#,
        position_jsons[0], position_jsons[1], position_jsons[2]
    );
    let spec_path = write_analysis(dir, "spec.json", &spec)?;
    let spec_bytes = spec.as_bytes();
    written.push((
        "spec.json".to_string(),
        sha256_hex(spec_bytes),
        spec_bytes.len(),
    ));
    let mut manifest = String::new();
    manifest.push_str("{\n  \"schema\": \"khaen-cal-multipoint-fixtures/v1\",\n");
    manifest.push_str("  \"note\": \"SYNTHETIC/OFFLINE fixtures only; not a real room measurement; 2 design + 1 independent validation positions\",\n");
    manifest.push_str("  \"files\": [\n");
    for (index, (name, sha, bytes)) in written.iter().enumerate() {
        manifest.push_str(&format!(
            "    {{\"name\": {:?}, \"sha256\": {:?}, \"bytes\": {}}}{}\n",
            name,
            sha,
            bytes,
            if index + 1 == written.len() { "" } else { "," }
        ));
    }
    manifest.push_str("  ]\n}\n");
    fs::write(dir.join("fixtures-manifest.json"), &manifest)
        .map_err(|error| format!("write manifest: {error}"))?;
    println!(
        "{{\"command\":\"fixtures\",\"out\":{:?},\"spec\":{:?},\"design_positions\":2,\"validation_positions\":1,\"synthetic\":true}}",
        slash(dir),
        slash(&spec_path)
    );
    Ok(())
}

fn write_report(args: &Args) -> Result<(), String> {
    let spec_path = args
        .spec
        .as_ref()
        .ok_or_else(|| "report needs --spec FILE".to_string())?;
    let out_dir = &args.out_dir;
    fs::create_dir_all(out_dir).map_err(|error| format!("mkdir {}: {error}", out_dir.display()))?;
    let spec = load_measured_spec(spec_path)?;
    let measured = build_measured_design(&spec)?;
    let design = measured.design;
    fs::write(
        out_dir.join("measured-summary.json"),
        &measured.summary_json,
    )
    .map_err(|error| format!("write measured summary: {error}"))?;
    // T64 fit：与 FitWiring stage_fitp 同入口/同选项（deterministic，created_unix=0）。
    let mut request = PeqFitRequest::new(&design.plan, &design.target, &design.curves)
        .with_options(design.peq_options.clone())
        .with_created_unix(design.created_unix);
    if !design.channel_bands.is_empty() {
        request = request.with_channel_bands(&design.channel_bands);
    }
    let peq = fit_peq(&request).map_err(|error| format!("T64 fit_peq: {error}"))?;
    let report_json = peq.report.to_json();
    fs::write(out_dir.join("fit-design-report.json"), &report_json)
        .map_err(|error| format!("write T64 report: {error}"))?;
    let evidence = multipoint_evidence_json(&design, &peq.report);
    fs::write(out_dir.join("multipoint-report.json"), &evidence)
        .map_err(|error| format!("write multipoint report: {error}"))?;
    let per_position: Vec<String> = peq
        .report
        .channels
        .iter()
        .flat_map(|channel| {
            channel
                .modal_residual_per_position
                .iter()
                .map(move |residual| {
                    format!(
                        "{{\"channel\":{:?},\"label\":{:?},\"role\":{:?},\"modal_rms_after_p_db\":{:.6}}}",
                        channel.name, residual.label, residual.role, residual.modal_rms_after_p_db
                    )
                })
        })
        .collect();
    let position_labels: Vec<&str> = design
        .plan
        .positions
        .iter()
        .map(|position| position.label.as_str())
        .collect();
    let fitp_summary = format!(
        "{{\n  \"schema\": \"khaen-cal-multipoint-fitp-probe/v1\",\n  \"spec\": {:?},\n  \"multipoint_report_schema\": {:?},\n  \"design_positions\": {},\n  \"validation_positions\": {},\n  \"position_labels\": {:?},\n  \"curves\": {},\n  \"phase_responses\": {},\n  \"measurement_sha256\": {:?},\n  \"p_sha256\": {:?},\n  \"t64_checks\": {{\"all_rule_2a_pass\": {}, \"all_rule_2b_pass\": {}, \"all_headroom_pass\": {}, \"all_boost_limits_pass\": {}}},\n  \"per_position_residual_after_p\": [{}],\n  \"verdict\": \"EXPERIMENT\",\n  \"acceptability\": \"NON_ACCEPTABLE\",\n  \"note\": \"synthetic/pilot multipoint code-path probe; measured-at-validation-positions predicted holdout only; real remeasure/T3/T6/T68 NOT_RUN\"\n}}\n",
        slash(spec_path),
        MEASURED_MULTIPOINT_SCHEMA,
        design.plan.design_count(),
        design.plan.validation_count(),
        position_labels,
        design.curves.len(),
        design.phase_responses.len(),
        peq.report.measurement_sha256,
        peq.report.p_sha256,
        peq.report.checks.all_rule_2a_pass,
        peq.report.checks.all_rule_2b_pass,
        peq.report.checks.all_headroom_pass,
        peq.report.checks.all_boost_limits_pass,
        per_position.join(", ")
    );
    fs::write(out_dir.join("fitp-summary.json"), &fitp_summary)
        .map_err(|error| format!("write fitp summary: {error}"))?;
    println!(
        "{{\"command\":\"report\",\"out\":{:?},\"design_positions\":{},\"validation_positions\":{},\"curves\":{},\"phase_responses\":{},\"t64_all_rule_2a_pass\":{},\"p_sha256\":{:?}}}",
        slash(out_dir),
        design.plan.design_count(),
        design.plan.validation_count(),
        design.curves.len(),
        design.phase_responses.len(),
        peq.report.checks.all_rule_2a_pass,
        peq.report.p_sha256
    );
    Ok(())
}
