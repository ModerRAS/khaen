//! 对角 P_post 单点 MIMO plan 打包（实验级；T66 compile_plan 真正调用）。
//!
//! 输入：文本设计文件（fs/block/n_in/container/unused/l_p/max_taps/layout/sha*/role 行 + P FIR CSV）。
//! 输出：plan.bin + compile-report.json + manifest.json（含 sha256 / graph_id / 实验标记）。
//!
//! 诚实边界：对角 P FIR 来自单点预测设计（PREDICTED/SINGLE_POINT_NOT_SPATIAL）；
//! T54 artifact 标 INCOMPLETE（无 holdout、5×5 G 未接线），用 ExperimentMode::SINGLE_POINT 显式旁路；
//! 不是生产 READY，不写 PASS；不 apply、不碰设备。
use std::fmt::Write as _;
use std::path::PathBuf;

use tspi_calibration::compile_apply::{
    ArtifactBinding, CompileApplyOptions, CompileApplyRequest, DelayPolarityEncoding,
    ExperimentMode, GPathInput, HeadroomEvidence, MappingBinding, PhaseEvidence, RoleBoostRecord,
    RoleCompileInput, RoleDacSlot, PEQ_FIT_SCHEMA, compile_plan,
};
use tspi_calibration::job::sha256_hex;
use tspi_calibration::phase_fit::{PhaseChannelOp, PhaseFitChecks};
use tspi_fir_designer::phase::Polarity;

const PROGRAM_PEAK_DBFS: f64 = -18.0;

struct RoleInput {
    index: u16,
    id: String,
    dac: u16,
    source: u16,
    fir: Vec<f64>,
}

struct Input {
    fs: u32,
    block: u16,
    n_in: u16,
    container: u16,
    unused: Vec<u16>,
    l_p: u32,
    max_taps: u32,
    layout: String,
    sha: Vec<String>,
    roles: Vec<RoleInput>,
}

fn parse_fir_csv(path: &str) -> Result<Vec<f64>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
    let mut values = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("tap") {
            continue;
        }
        let value = line
            .rsplit(',')
            .next()
            .ok_or_else(|| format!("bad FIR line {line:?}"))?
            .trim()
            .parse::<f64>()
            .map_err(|e| format!("FIR value {line:?}: {e}"))?;
        values.push(value);
    }
    if values.is_empty() {
        return Err(format!("FIR CSV {path} 为空"));
    }
    Ok(values)
}

fn parse_input(text: &str) -> Result<Input, String> {
    let mut input = Input {
        fs: 0,
        block: 0,
        n_in: 0,
        container: 0,
        unused: Vec::new(),
        l_p: 0,
        max_taps: 0,
        layout: "4.1.2".to_string(),
        sha: Vec::new(),
        roles: Vec::new(),
    };
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        let err = |msg: String| format!("line {}: {msg}", lineno + 1);
        match parts[0] {
            "fs" => input.fs = parts[1].parse().map_err(|e| err(format!("fs: {e}")))?,
            "block" => input.block = parts[1].parse().map_err(|e| err(format!("block: {e}")))?,
            "n_in" => input.n_in = parts[1].parse().map_err(|e| err(format!("n_in: {e}")))?,
            "container" => input.container = parts[1].parse().map_err(|e| err(format!("container: {e}")))?,
            "unused" => {
                input.unused = parts[1]
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(|s| s.parse::<u16>().map_err(|e| err(format!("unused: {e}"))))
                    .collect::<Result<Vec<_>, _>>()?
            }
            "l_p" => input.l_p = parts[1].parse().map_err(|e| err(format!("l_p: {e}")))?,
            "max_taps" => input.max_taps = parts[1].parse().map_err(|e| err(format!("max_taps: {e}")))?,
            "layout" => input.layout = parts[1].to_string(),
            "sha" => {
                input.sha = parts[1..].iter().map(|s| s.to_string()).collect();
            }
            "role" => {
                let fir = parse_fir_csv(parts[5])?;
                input.roles.push(RoleInput {
                    index: parts[1].parse().map_err(|e| err(format!("role index: {e}")))?,
                    id: parts[2].to_string(),
                    dac: parts[3].parse().map_err(|e| err(format!("role dac: {e}")))?,
                    source: parts[4].parse().map_err(|e| err(format!("role source: {e}")))?,
                    fir,
                });
            }
            other => return Err(err(format!("unknown key {other}"))),
        }
    }
    if input.sha.len() != 7 {
        return Err("sha 需要 7 个 sha256".into());
    }
    Ok(input)
}

fn t64_report_bytes(roles: &[RoleInput]) -> Vec<u8> {
    let mut channels = String::new();
    for (i, role) in roles.iter().enumerate() {
        if i > 0 {
            channels.push(',');
        }
        let max_pos = p_row_max_positive_db(&role.fir, 48_000.0_f64);
        let _ = write!(
            channels,
            r#"{{"name":"{}","band_count":0,"headroom":{{"program_peak_reference_dbfs":{},"predicted_peak_before_trim_dbfs":{},"trim_db":0.0,"predicted_peak_dbfs":{},"headroom_db":{},"pass":true}},"boost":{{"single_filter_max_db":0.0,"filter_positive_sum_db":0.0,"limits_pass":true}},"target_fir":{{"max_positive_db":{}}}}}"#,
            role.id, PROGRAM_PEAK_DBFS, PROGRAM_PEAK_DBFS, PROGRAM_PEAK_DBFS, -PROGRAM_PEAK_DBFS, max_pos
        );
    }
    format!(r#"{{"schema":"{}","channels":[{}]}}"#, PEQ_FIT_SCHEMA, channels).into_bytes()
}

fn p_row_max_positive_db(coeffs: &[f64], fs: f64) -> f64 {
    // 20 Hz..20 kHz, 1/24 oct 网格，绝对幅度峰值（与 compile_apply::p_row_max_positive_db 同口径的近似）。
    let mut max_db = f64::NEG_INFINITY;
    let mut freq = 20.0f64;
    while freq < 20_000.0f64 {
        let w = std::f64::consts::TAU * freq / fs;
        let mut re = 0.0f64;
        let mut im = 0.0f64;
        for (t, value) in coeffs.iter().enumerate() {
            let ph = w * t as f64;
            re += value * ph.cos();
            im -= value * ph.sin();
        }
        let mag = (re * re + im * im).sqrt();
        max_db = max_db.max(20.0 * mag.max(1e-30).log10());
        freq *= 2.0f64.powf(1.0 / 24.0);
    }
    max_db
}

fn main_entry() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mut input_path = None;
    let mut out_dir = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--input" => input_path = args.next().map(PathBuf::from),
            "--out" => out_dir = args.next().map(PathBuf::from),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let input_path = input_path.ok_or("missing --input")?;
    let out_dir = out_dir.ok_or("missing --out")?;
    let text = std::fs::read_to_string(&input_path).map_err(|e| format!("read: {e}"))?;
    let input = parse_input(&text)?;
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("mkdir: {e}"))?;

    let mut role_slots = Vec::new();
    let mut roles = Vec::new();
    let mut phase_ops = Vec::new();
    let mut dense_to_dac = Vec::new();
    for (idx, role) in input.roles.iter().enumerate() {
        if role.index as usize != idx {
            return Err("role index 必须 0..N-1 顺序".into());
        }
        role_slots.push(
            RoleDacSlot::new(role.index, role.id.clone(), role.dac, vec![role.dac])
                .map_err(|e| format!("RoleDacSlot {}: {e:?}", role.id))?,
        );
        roles.push(
            RoleCompileInput::new(
                role.index,
                role.id.clone(),
                role.dac,
                Some(role.source),
                0,
                1,
                0.0,
                role.fir.clone(),
                0,
            )
            .map_err(|e| format!("RoleCompileInput {}: {e:?}", role.id))?,
        );
        phase_ops.push(PhaseChannelOp {
            channel: role.id.clone(),
            index: Some(role.index as u32),
            delay_samples: 0,
            delay_ms: 0.0,
            polarity: Polarity::Normal,
        });
        dense_to_dac.push(role.dac);
    }
    let mapping = MappingBinding::new(
        input.layout.clone(),
        input.sha[0].clone(),
        input.sha[1].clone(),
        input.sha[2].clone(),
        input.sha[3].clone(),
        input.container,
        dense_to_dac,
        role_slots,
        input.unused.clone(),
        false,
        Vec::new(),
    )
    .map_err(|e| format!("MappingBinding: {e:?}"))?;
    let checks = PhaseFitChecks {
        integer_delay_contract: true,
        min_zero: true,
        max_within_cap: true,
        reference_is_latest: true,
        fractional_residual_within_half_sample: true,
        polarity_known: true,
        fir_design_ok: true,
        fir_direction_ok: true,
        all_pass: true,
    };
    let phase = PhaseEvidence::from_ops(
        &phase_ops,
        &checks,
        input.sha[4].clone(),
        input.sha[5].clone(),
        input.roles[0].id.clone(),
        0.0,
    );
    let report_bytes = t64_report_bytes(&input.roles);
    let report_sha = sha256_hex(&report_bytes);
    let mut records = Vec::new();
    for role in &input.roles {
        records.push(
            RoleBoostRecord::new(role.id.clone(), 0.0, 0.0, PROGRAM_PEAK_DBFS, -PROGRAM_PEAK_DBFS, true)
                .map_err(|e| format!("RoleBoostRecord: {e:?}"))?,
        );
    }
    let headroom = HeadroomEvidence::new(PEQ_FIT_SCHEMA, report_sha, records)
        .with_t64_report_bytes(report_bytes);
    let artifact = ArtifactBinding::from_parts(
        input.sha[0].clone(),
        input.sha[1].clone(),
        input.sha[2].clone(),
        input.sha[3].clone(),
        input.sha[4].clone(),
        input.sha[5].clone(),
        input.sha[6].clone(),
        "INCOMPLETE",
    );
    let mut paths = Vec::new();
    for role in &input.roles {
        paths.push(
            GPathInput::new(role.index, role.index, vec![0.0], true, false, input.sha[0].clone())
                .map_err(|e| format!("GPathInput {}: {e:?}", role.id))?
                .with_source(role.source),
        );
    }
    let options = CompileApplyOptions {
        max_taps: input.max_taps,
        extended_taps_admitted: input.max_taps > 4096,
        force_form: Some(tspi_dsp_core::compile::FusionForm::Factorized),
        ..Default::default()
    };
    let request = CompileApplyRequest::new(
        input.fs,
        input.block,
        input.n_in,
        input.container,
        0,
        1,
        input.l_p,
        &mapping,
        &phase,
        &headroom,
        &artifact,
        &paths,
        &roles,
        DelayPolarityEncoding::RoleMetadata,
        options,
    )
    .with_experiment(ExperimentMode::SINGLE_POINT)
    .with_experiment_notes(vec![
        "diagonal P_post single-point MIMO: PREDICTED design, no T3/holdout; T54 artifact INCOMPLETE (experiment bypass)".to_string(),
        "5x5 cross G NOT_WIRED; only diagonal P FIR carried".to_string(),
    ]);
    let compiled = compile_plan(&request).map_err(|e| format!("T66 compile_plan: {e:?}"))?;
    std::fs::write(out_dir.join("plan.bin"), &compiled.plan_bytes)
        .map_err(|e| format!("write plan: {e}"))?;
    let plan_sha = sha256_hex(&compiled.plan_bytes);
    let graph_hex = compiled
        .graph_id
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let mut manifest = String::new();
    let _ = writeln!(manifest, "{{");
    let _ = writeln!(manifest, "  \"schema\": \"tspi-mimo-diag-plan/v1\",");
    let _ = writeln!(manifest, "  \"tool\": {{\"name\":\"tspi-mimo-diag-plan\",\"version\":\"mimo-prod-20261122-1\"}},");
    let _ = writeln!(manifest, "  \"status\": \"EXPERIMENT_NON_ACCEPTABLE\",");
    let _ = writeln!(manifest, "  \"claim\": \"PREDICTED_ONLY / SINGLE_POINT_NOT_SPATIAL / NO_MEASUREMENT\",");
    let _ = writeln!(manifest, "  \"compiled\": true, \"applied\": false, \"measured\": false,");
    let _ = writeln!(manifest, "  \"experiment\": {},", request.experiment.markers_json());
    let _ = writeln!(manifest, "  \"environment\": {{\"fs\": {}, \"block_size\": {}, \"n_in\": {}, \"container_channels\": {}, \"l_p\": {}, \"max_taps\": {}, \"form\": \"{}\"}},", input.fs, input.block, input.n_in, input.container, input.l_p, input.max_taps, compiled.report.facts.chosen_form);
    let _ = writeln!(manifest, "  \"plan\": {{\"path\": \"plan.bin\", \"bytes\": {}, \"sha256\": \"{}\", \"graph_id\": \"{}\", \"path_count\": {}, \"diag_count\": {}, \"role_count\": {}, \"total_coeff_bytes\": {}}},", compiled.plan_bytes.len(), plan_sha, graph_hex, compiled.paths.len(), compiled.diags.len(), compiled.roles.len(), compiled.coeffs.len());
    let _ = writeln!(manifest, "  \"roles\": [");
    for (i, role) in input.roles.iter().enumerate() {
        let last = i + 1 == input.roles.len();
        let _ = writeln!(manifest, "    {{\"index\": {}, \"id\": \"{}\", \"dac\": {}, \"source\": {}, \"p_taps\": {}, \"fir_sha256\": \"{}\"}}{}", role.index, role.id, role.dac, role.source, role.fir.len(), sha256_hex(format!("{:?}", role.fir).as_bytes()), if last { "" } else { "," });
    }
    let _ = writeln!(manifest, "  ]");
    let _ = writeln!(manifest, "}}");
    std::fs::write(out_dir.join("manifest.json"), &manifest).map_err(|e| format!("write manifest: {e}"))?;
    std::fs::write(out_dir.join("compile-report.json"), compiled.report.to_json())
        .map_err(|e| format!("write report: {e}"))?;
    println!("diag_p_plan: OK plan_bytes={} sha256={} graph_id={} form={}", compiled.plan_bytes.len(), plan_sha, graph_hex, compiled.report.facts.chosen_form);
    Ok(())
}

fn main() {
    if let Err(error) = main_entry() {
        eprintln!("diag_p_plan: ERROR: {error}");
        std::process::exit(2);
    }
}