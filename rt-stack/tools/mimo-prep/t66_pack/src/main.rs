//! task-232 / MIMO pilot: REAL T66 compiler hook (non-identity G matrix).
//!
//! Reads a plain-text design input (exported by prep/dryrun/export_t66_input.py),
//! builds the T66 CompileApplyRequest (G path FIRs from the MIMO design + identity
//! P rows with T65/T64-shaped evidence) and calls the real T66 compiler
//! (khaen_calibration::compile_apply::compile_plan) used by khaen-cal run.
//! Writes plan.bin + compile-report.json manifest accepted by khaen-rt-host
//! plan-arena (plan_sha256 / host_graph_sha256 binding).
//!
//! Honesty: T65/T64 shape evidence is derived from the pilot design (identity P,
//! zero boosts, program peak reference -18 dBFS); the manifest carries
//! t66_compiler_invoked=true and pilot_3mic=true. No board, no sound.
//!
//! Usage: mimo_t66_pack --input design.t66.txt --out DIR
use std::fmt::Write as _;
use std::path::PathBuf;

use khaen_calibration::compile_apply::{
    ArtifactBinding, CompileApplyOptions, CompileApplyRequest, DelayPolarityEncoding,
    GPathInput, HeadroomEvidence, MappingBinding, PhaseEvidence, RoleBoostRecord,
    RoleCompileInput, RoleDacSlot, PEQ_FIT_SCHEMA, compile_plan,
};
use khaen_calibration::job::sha256_hex;
use khaen_calibration::phase_fit::{PhaseChannelOp, PhaseFitChecks};
use khaen_designer::phase::Polarity;

const PROGRAM_PEAK_DBFS: f64 = -18.0;

struct Input {
    fs: u32,
    block: u16,
    n_in: u16,
    container: u16,
    l_g: u32,
    l_p: u32,
    d: u32,
    unused: Vec<u16>,
    roles: Vec<(u16, String, u16, u16)>,
    participants: Vec<(u16, u16)>,
    artifact_sha: Vec<String>,
    cells: Vec<(u16, u16, Vec<f64>)>,
}

fn parse_input(text: &str) -> Result<Input, String> {
    let mut input = Input {
        fs: 0, block: 0, n_in: 0, container: 0, l_g: 0, l_p: 0, d: 0,
        unused: Vec::new(), roles: Vec::new(), participants: Vec::new(),
        artifact_sha: Vec::new(), cells: Vec::new(),
    };
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let p: Vec<&str> = line.split_whitespace().collect();
        let err = |msg: String| format!("line {}: {}", lineno + 1, msg);
        match p[0] {
            "fs" => input.fs = p[1].parse().map_err(|e| err(format!("fs: {e}")))?,
            "block" => input.block = p[1].parse().map_err(|e| err(format!("block: {e}")))?,
            "n_in" => input.n_in = p[1].parse().map_err(|e| err(format!("n_in: {e}")))?,
            "container" => input.container = p[1].parse().map_err(|e| err(format!("container: {e}")))?,
            "l_g" => input.l_g = p[1].parse().map_err(|e| err(format!("l_g: {e}")))?,
            "l_p" => input.l_p = p[1].parse().map_err(|e| err(format!("l_p: {e}")))?,
            "d" => input.d = p[1].parse().map_err(|e| err(format!("d: {e}")))?,
            "unused" => {
                input.unused = p[1]
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(|s| s.parse::<u16>().map_err(|e| err(format!("unused: {e}"))))
                    .collect::<Result<Vec<_>, _>>()?;
            }
            "role" => input.roles.push((
                p[1].parse().map_err(|e| err(format!("role index: {e}")))?,
                p[2].to_string(),
                p[3].parse().map_err(|e| err(format!("role dac: {e}")))?,
                p[4].parse().map_err(|e| err(format!("role source: {e}")))?,
            )),
            "participant" => input.participants.push((
                p[1].parse().map_err(|e| err(format!("participant dense: {e}")))?,
                p[2].parse().map_err(|e| err(format!("participant role: {e}")))?,
            )),
            "artifact_sha" => {
                input.artifact_sha = p[1..].iter().map(|s| s.to_string()).collect();
            }
            "cell" => {
                let out: u16 = p[1].parse().map_err(|e| err(format!("cell out: {e}")))?;
                let in_: u16 = p[2].parse().map_err(|e| err(format!("cell in: {e}")))?;
                let coeffs: Vec<f64> = p[4..]
                    .iter()
                    .map(|s| s.parse::<f64>().map_err(|e| err(format!("cell coeff: {e}"))))
                    .collect::<Result<Vec<_>, _>>()?;
                if coeffs.len() != input.l_g as usize {
                    return Err(err(format!("cell coeffs {} != l_g {}", coeffs.len(), input.l_g)));
                }
                input.cells.push((out, in_, coeffs));
            }
            other => return Err(err(format!("unknown key {other}"))),
        }
    }
    if input.artifact_sha.len() != 7 {
        return Err("artifact_sha needs 7 sha256 values".into());
    }
    Ok(input)
}

fn hex32(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for b in bytes {
        let _ = write!(out, "{:02x}", b);
    }
    out
}

fn t64_report_bytes(role_ids: &[String]) -> Vec<u8> {
    let mut channels = String::new();
    for (i, id) in role_ids.iter().enumerate() {
        if i > 0 {
            channels.push(',');
        }
        let _ = write!(
            channels,
            r#"{{"name":"{}","band_count":0,"headroom":{{"program_peak_reference_dbfs":{},"predicted_peak_before_trim_dbfs":{},"trim_db":0.0,"predicted_peak_dbfs":{},"headroom_db":{},"pass":true}},"boost":{{"single_filter_max_db":0.0,"filter_positive_sum_db":0.0,"limits_pass":true}},"target_fir":{{"max_positive_db":0.0}}}}"#,
            id, PROGRAM_PEAK_DBFS, PROGRAM_PEAK_DBFS, PROGRAM_PEAK_DBFS, -PROGRAM_PEAK_DBFS
        );
    }
    format!(r#"{{"schema":"{}","channels":[{}]}}"#, PEQ_FIT_SCHEMA, channels).into_bytes()
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
    let text = std::fs::read_to_string(&input_path)
        .map_err(|e| format!("read {}: {e}", input_path.display()))?;
    let input = parse_input(&text)?;
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("mkdir {}: {e}", out_dir.display()))?;

    let mut role_slots = Vec::new();
    let mut roles = Vec::new();
    let mut phase_ops = Vec::new();
    let mut role_ids = Vec::new();
    for (idx, (role_index, role_id, dac, source)) in input.roles.iter().enumerate() {
        if *role_index as usize != idx {
            return Err("role_index must be 0..N-1 in order".into());
        }
        role_slots.push(
            RoleDacSlot::new(*role_index, role_id.clone(), *dac, vec![*dac])
                .map_err(|e| format!("RoleDacSlot {role_id}: {e:?}"))?,
        );
        roles.push(
            RoleCompileInput::new(*role_index, role_id.clone(), *dac, Some(*source), 0, 1, 0.0, vec![1.0], 0)
                .map_err(|e| format!("RoleCompileInput {role_id}: {e:?}"))?,
        );
        phase_ops.push(PhaseChannelOp {
            channel: role_id.clone(),
            index: Some(*role_index as u32),
            delay_samples: 0,
            delay_ms: 0.0,
            polarity: Polarity::Normal,
        });
        role_ids.push(role_id.clone());
    }
    let dense_to_dac: Vec<u16> = input
        .participants
        .iter()
        .map(|(_dense, role)| input.roles[*role as usize].2)
        .collect();
    let mapping = MappingBinding::new(
        "4.1.2",
        input.artifact_sha[0].clone(),
        input.artifact_sha[1].clone(),
        input.artifact_sha[2].clone(),
        input.artifact_sha[3].clone(),
        input.container,
        dense_to_dac.clone(),
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
        input.artifact_sha[4].clone(),
        input.artifact_sha[5].clone(),
        role_ids[0].clone(),
        0.0,
    );

    let report_bytes = t64_report_bytes(&role_ids);
    let report_sha = sha256_hex(&report_bytes);
    let mut records = Vec::new();
    for id in &role_ids {
        records.push(
            RoleBoostRecord::new(id.clone(), 0.0, 0.0, PROGRAM_PEAK_DBFS, -PROGRAM_PEAK_DBFS, true)
                .map_err(|e| format!("RoleBoostRecord: {e:?}"))?,
        );
    }
    let headroom = HeadroomEvidence::new(PEQ_FIT_SCHEMA, report_sha, records)
        .with_t64_report_bytes(report_bytes);

    let artifact = ArtifactBinding::from_parts(
        input.artifact_sha[0].clone(),
        input.artifact_sha[1].clone(),
        input.artifact_sha[2].clone(),
        input.artifact_sha[3].clone(),
        input.artifact_sha[4].clone(),
        input.artifact_sha[5].clone(),
        input.artifact_sha[6].clone(),
        "READY",
    );

    let mut paths = Vec::with_capacity(input.cells.len());
    for (out_dense, in_dense, coeffs) in &input.cells {
        let diag = out_dense == in_dense;
        let path = GPathInput::new(
            *out_dense,
            *in_dense,
            coeffs.clone(),
            diag,
            false,
            input.artifact_sha[(*out_dense as usize) % 7].clone(),
        )
        .map_err(|e| format!("GPathInput [{out_dense}][{in_dense}]: {e:?}"))?
        .with_source(*in_dense);
        paths.push(path);
    }

    let options = CompileApplyOptions { max_taps: 4096, ..Default::default() };
    let request = CompileApplyRequest::new(
        input.fs, input.block, input.n_in, input.container, input.d, input.l_g, input.l_p,
        &mapping, &phase, &headroom, &artifact, &paths, &roles,
        DelayPolarityEncoding::RoleMetadata, options,
    );
    let compiled = compile_plan(&request).map_err(|e| format!("T66 compile_plan: {e:?}"))?;
    std::fs::write(out_dir.join("plan.bin"), &compiled.plan_bytes)
        .map_err(|e| format!("write plan: {e}"))?;
    let plan_sha = sha256_hex(&compiled.plan_bytes);
    let graph_hex = hex32(&compiled.graph_id);
    let header = compiled.header;
    let offdiag = compiled.report.paths.iter().filter(|p| !p.diag_baseline).count();
    let max_offdiag_peak = compiled
        .report
        .paths
        .iter()
        .filter(|p| !p.diag_baseline)
        .map(|p| p.peak_abs)
        .fold(0.0_f64, f64::max);

    let mut manifest = String::new();
    let _ = writeln!(manifest, "{{");
    let _ = writeln!(manifest, "  \"schema\": \"khaen-cal-compile-apply/v1\",");
    let _ = writeln!(manifest, "  \"tool\": {{\"name\":\"khaen-mimo-t66-pack\",\"version\":\"task232-1\"}},");
    let _ = writeln!(manifest, "  \"t66_compiler_invoked\": true,");
    let _ = writeln!(manifest, "  \"pilot_3mic\": true,");
    let _ = writeln!(manifest, "  \"honesty\": \"G FIR from 3-mic pilot reference design; T65/T64-shape evidence derived from identity P (no real T3/T67 acceptance)\",");
    let _ = writeln!(manifest, "  \"compiled\": true,");
    let _ = writeln!(manifest, "  \"environment\": {{\"fs\": {}, \"block_size\": {}, \"n_in\": {}, \"container_channels\": {}, \"workers\": 4}},", input.fs, input.block, input.n_in, input.container);
    let _ = writeln!(manifest, "  \"plan\": {{\"graph_id\": \"{}\", \"plan_sha256\": \"{}\", \"plan_version\": 0, \"path_count\": {}, \"diag_count\": {}, \"role_count\": {}, \"low_band_count\": {}, \"d_samples\": {}, \"l_g\": {}, \"l_p\": {}, \"max_taps\": {}, \"total_coeff_bytes\": {}, \"chosen_form\": \"{}\"}},", graph_hex, plan_sha, compiled.paths.len(), compiled.diags.len(), compiled.roles.len(), header.low_band_count, input.d, input.l_g, input.l_p, header.max_taps, compiled.coeffs.len(), compiled.report.facts.chosen_form);
    let _ = writeln!(manifest, "  \"host_graph_sha256\": \"{}\",", graph_hex);
    let _ = writeln!(manifest, "  \"artifact\": {{\"host_graph_sha256\": \"{}\", \"status\": \"READY\", \"pilot_3mic\": true}},", graph_hex);
    let _ = writeln!(manifest, "  \"d7_warnings\": {:?}", compiled.d7_warnings);
    let _ = writeln!(manifest, "}}");
    std::fs::write(out_dir.join("compile-report.json"), manifest)
        .map_err(|e| format!("write manifest: {e}"))?;

    let summary = format!(
        "{{\"plan_bytes\": {}, \"plan_sha256\": \"{}\", \"graph_id\": \"{}\", \"paths\": {}, \"offdiag_paths\": {}, \"roles\": {}, \"max_offdiag_peak_abs\": {:.6}, \"form\": \"{}\", \"t66_compiler_invoked\": true, \"pilot_3mic\": true}}\n",
        compiled.plan_bytes.len(), plan_sha, graph_hex, compiled.paths.len(), offdiag,
        compiled.roles.len(), max_offdiag_peak, compiled.report.facts.chosen_form
    );
    std::fs::write(out_dir.join("pack-summary.json"), &summary)
        .map_err(|e| format!("write summary: {e}"))?;
    print!("{}", summary);
    Ok(())
}

fn main() {
    match main_entry() {
        Ok(()) => {}
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(2);
        }
    }
}
