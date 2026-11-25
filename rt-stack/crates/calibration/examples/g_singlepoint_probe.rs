//! g-singlepoint-20261121 probe harness (OFFLINE, design-side only).
//!
//! 目的：用真实单点 5 执行器测量 H(f) 直接构造 tspi-modal-design 的 SolveProblem，
//! 离线跑 T51 solve / T52 regularize（λ 扫描 + 固定白名单），输出 G、predicted residual、
//! effort、L-curve 与适用边界；并演示 T54 artifact 状态门（INCOMPLETE 不得编译）。
//!
//! 纪律：
//! - 不改 solver 算法/参数默认值；只调用公开 API（solve / regularize / ArtifactBinding）。
//! - 不采音、不开设备、不写板端；示例程序，不进入生产 CLI。
//! - 单点 P=1 < M=5 ⇒ regularize 的 deliverable 恒为 false（贫穷边界），本程序如实输出。
//!
//! 用法（在 rt-stack/ 下）：
//!   cargo run --offline -p tspi-calibration --example g_singlepoint_probe -- solve \
//!       --h evidence/.../h/H_complex.csv --out .../solve/preserve_sub1pos --tag preserve_sub1pos
//!   cargo run ... -- solve --h ... --target .../target_probe_S2.csv --out ... --tag probe_s2
//!   cargo run ... -- artifact-gate --out ...
//!   cargo run ... -- measured-design --spec .../spec.json --out ...

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use tspi_calibration::compile_apply::ArtifactBinding;
use tspi_calibration::measured_design::{build_measured_design, load_measured_spec};
use tspi_modal_design::regularize::{
    regularize, RegularizeOptions, RegularizeOutcome, WhitelistKind,
};
use tspi_modal_design::solve::{
    solve, SolveMode, SolveOptions, SolveOutcome, SolvePosition, SolveProblem,
};
use tspi_modal_design::timebase::ComplexPoint;

const ROLES: [&str; 5] = ["FL", "FR", "SL", "SR", "SUB1"];

fn main() {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_default();
    let opts = parse_args(args.collect());
    let result = match command.as_str() {
        "solve" => run_solve(&opts),
        "artifact-gate" => run_artifact_gate(&opts),
        "measured-design" => run_measured_design(&opts),
        other => Err(format!("unknown subcommand {other:?} (solve|artifact-gate|measured-design)")),
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
    h_file: PathBuf,
    target_file: Option<PathBuf>,
    out_dir: PathBuf,
    tag: String,
    taps: usize,
    fit_lo: f64,
    fit_hi: f64,
    max_iterations: usize,
    spec: Option<PathBuf>,
    whitelists: Vec<WhitelistKind>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            h_file: PathBuf::new(),
            target_file: None,
            out_dir: PathBuf::new(),
            tag: "run".to_string(),
            taps: 256,
            fit_lo: 20.0,
            fit_hi: 129.5,
            max_iterations: 800,
            spec: None,
            whitelists: vec![WhitelistKind::DiagOnly, WhitelistKind::DiagAdjacent, WhitelistKind::DiagSub1, WhitelistKind::Full],
        }
    }
}

fn parse_args(items: Vec<String>) -> Args {
    let mut args = Args::default();
    let mut index = 0;
    while index < items.len() {
        let key = items[index].as_str();
        let mut next = || {
            index += 1;
            items.get(index).cloned().unwrap_or_default()
        };
        match key {
            "--h" => args.h_file = PathBuf::from(next()),
            "--target" => args.target_file = Some(PathBuf::from(next())),
            "--out" => args.out_dir = PathBuf::from(next()),
            "--tag" => args.tag = next(),
            "--taps" => args.taps = next().parse().unwrap_or(256),
            "--fit-lo" => args.fit_lo = next().parse().unwrap_or(20.0),
            "--fit-hi" => args.fit_hi = next().parse().unwrap_or(129.5),
            "--max-iterations" => args.max_iterations = next().parse().unwrap_or(800),
            "--spec" => args.spec = Some(PathBuf::from(next())),
            "--whitelists" => {
                let spec = next();
                let mut parsed = Vec::new();
                for item in spec.split(',') {
                    let kind = match item.trim() {
                        "diag_only" => WhitelistKind::DiagOnly,
                        "diag_adjacent" => WhitelistKind::DiagAdjacent,
                        "diag_sub1" => WhitelistKind::DiagSub1,
                        "full" => WhitelistKind::Full,
                        other => {
                            eprintln!("unknown whitelist {other}");
                            continue;
                        }
                    };
                    parsed.push(kind);
                }
                if !parsed.is_empty() {
                    args.whitelists = parsed;
                }
            }
            other => {
                eprintln!("ignoring unknown arg {other}");
            }
        }
        index += 1;
    }
    args
}

// ---------------------------------------------------------------------------
// CSV 读取：freq,<role>_re,<role>_im,...
// ---------------------------------------------------------------------------

struct Matrix {
    freq_hz: Vec<f64>,
    /// [freq][role] 复数
    values: Vec<[ComplexPoint; 5]>,
}

fn read_matrix(path: &Path) -> Result<Matrix, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let header = lines.next().ok_or("empty csv")?;
    let columns: Vec<&str> = header.split(',').collect();
    if columns.first().copied() != Some("freq_hz") {
        return Err(format!("{}: first column must be freq_hz", path.display()));
    }
    let mut freq_hz = Vec::new();
    let mut values: Vec<[ComplexPoint; 5]> = Vec::new();
    for (line_no, line) in lines.enumerate() {
        let fields: Vec<&str> = line.split(',').collect();
        if fields.len() != 1 + 2 * 5 {
            return Err(format!("{}:{}: expected {} fields, got {}", path.display(), line_no + 2, 11, fields.len()));
        }
        let freq: f64 = fields[0].parse().map_err(|e| format!("freq parse: {e}"))?;
        if let Some(last) = freq_hz.last() {
            if freq <= *last {
                return Err(format!("grid not strictly increasing at {}", line_no + 2));
            }
        }
        let mut row = [ComplexPoint { freq_hz: freq, re: 0.0, im: 0.0 }; 5];
        for (n, slot) in row.iter_mut().enumerate() {
            let re: f64 = fields[1 + 2 * n].parse().map_err(|e| format!("re parse: {e}"))?;
            let im: f64 = fields[2 + 2 * n].parse().map_err(|e| format!("im parse: {e}"))?;
            *slot = ComplexPoint { freq_hz: freq, re, im };
        }
        freq_hz.push(freq);
        values.push(row);
    }
    if freq_hz.len() < 8 {
        return Err(format!("grid has only {} points", freq_hz.len()));
    }
    Ok(Matrix { freq_hz, values })
}

// ---------------------------------------------------------------------------
// solve
// ---------------------------------------------------------------------------

fn run_solve(args: &Args) -> Result<(), String> {
    if args.h_file.as_os_str().is_empty() {
        return Err("solve needs --h FILE".to_string());
    }
    let h = read_matrix(&args.h_file)?;
    let target = match args.target_file.as_ref() {
        Some(path) => read_matrix(path)?,
        None => read_matrix(&args.h_file)?,
    };
    if target.freq_hz.len() != h.freq_hz.len() {
        return Err("target grid length differs from H".to_string());
    }

    // 标量归一：测量 H 用 1 kHz 相对 dB，绝对尺度任意；λ_scale=mean|H|^2 会因此达到 1e4 量级，
    // 撞 T51 MAX_SOLVE_RIDGE=100（真实数据上的 λ 网格可扩展问题，记入报告）。
    // 这里把 H 与 target 同乘 1/c（c=max|H|）：G 与相对残差 dB 不变，只把 λ 扫描拉回可用尺度。
    let mut scale = 0.0f64;
    for row in &h.values {
        for value in row {
            scale = scale.max(value.re.hypot(value.im));
        }
    }
    if !(scale > 0.0) {
        return Err("H is all zeros".to_string());
    }
    let h = {
        let mut m = Matrix { freq_hz: h.freq_hz.clone(), values: h.values.clone() };
        for row in m.values.iter_mut() {
            for value in row.iter_mut() {
                value.re /= scale;
                value.im /= scale;
            }
        }
        m
    };
    let target = {
        let mut m = Matrix { freq_hz: target.freq_hz.clone(), values: target.values.clone() };
        for row in m.values.iter_mut() {
            for value in row.iter_mut() {
                value.re /= scale;
                value.im /= scale;
            }
        }
        m
    };

    let mut h_eff: Vec<Vec<ComplexPoint>> = (0..5).map(|_| Vec::with_capacity(h.freq_hz.len())).collect();
    let mut target_rows: Vec<Vec<ComplexPoint>> =
        (0..5).map(|_| Vec::with_capacity(h.freq_hz.len())).collect();
    for (index, freq) in h.freq_hz.iter().copied().enumerate() {
        for n in 0..5 {
            h_eff[n].push(ComplexPoint { freq_hz: freq, re: h.values[index][n].re, im: h.values[index][n].im });
            target_rows[n].push(ComplexPoint {
                freq_hz: freq,
                re: target.values[index][n].re,
                im: target.values[index][n].im,
            });
        }
    }
    let problem = SolveProblem {
        roles: ROLES.iter().map(|role| role.to_string()).collect(),
        grid_hz: h.freq_hz.clone(),
        sample_rate_hz: 48_000,
        positions: vec![SolvePosition {
            label: "T4B_SINGLE_POINT".to_string(),
            weight: 1.0,
            h_eff,
            target: target_rows,
        }],
        // Custom：本 probe 的 target 由调用方显式给出（preserve 语义 = H_eff；probe = H_eff×|S2|），
        // 不伪造 T50 preserve/reduced target 身份。
        mode: SolveMode::Custom,
        target_position_variant: true,
        target_delay_samples: 0,
        provenance: None,
    };

    let mut solve_options = SolveOptions::default();
    solve_options.taps = args.taps;
    solve_options.fit_band_hz = (args.fit_lo, args.fit_hi);
    solve_options.max_iterations = args.max_iterations;
    solve_options.rel_tolerance = 1e-10;

    let base = solve(&problem, &solve_options).map_err(|e| format!("solve failed: {e}"))?;

    let mut reg_options = RegularizeOptions::default();
    reg_options.base = solve_options.clone();
    // λ 网格用 T52 固定默认 LAMBDA_GRID（8 点，0..1），白名单用固定 4 类。
    reg_options.whitelists = args.whitelists.clone();
    let reg = regularize(&problem, &reg_options).map_err(|e| format!("regularize failed: {e}"))?;

    fs::create_dir_all(&args.out_dir).map_err(|e| format!("mkdir: {e}"))?;
    write_solve_report(&args.out_dir, &base, &problem)?;
    write_regularize_report(&args.out_dir, &reg)?;
    if let Some(solution) = reg.solution.as_ref() {
        write_g(&args.out_dir, &solution.coeffs_f64, args.taps)?;
        write_predicted(&args.out_dir, &problem, &h, &target, &solution.coeffs_f64, args.taps)?;
    }
    write_meta(&args.out_dir, args, &problem, &base, &reg, scale)?;
    println!("[{}] solve status={} residual_max_db(fit)={:.3} reg deliverable={} selected={} lambda={}",
        args.tag,
        base.report.status.as_str(),
        base.report.predicted_fit_band_residual_max_db,
        reg.report.deliverable,
        reg.report.selection.selected_whitelist.as_str(),
        reg.report.selection.selected_lambda,
    );
    Ok(())
}

fn write_solve_report(dir: &Path, outcome: &SolveOutcome, problem: &SolveProblem) -> Result<(), String> {
    let report = &outcome.report;
    let mut text = String::new();
    text.push_str("{\n");
    let _ = writeln!(text, "  \"schema\": \"tspi-g-singlepoint-solve-report/v1\",");
    let _ = writeln!(text, "  \"scope\": \"SINGLE_POINT_NOT_SPATIAL\",");
    let _ = writeln!(text, "  \"mode\": \"{}\",", report.mode.as_str());
    let _ = writeln!(text, "  \"status\": \"{}\",", report.status.as_str());
    let _ = writeln!(text, "  \"m\": {},", report.m);
    let _ = writeln!(text, "  \"taps\": {},", report.taps);
    let _ = writeln!(text, "  \"delay_samples\": {},", report.delay_samples);
    let _ = writeln!(text, "  \"sample_rate_hz\": {},", report.sample_rate_hz);
    let _ = writeln!(text, "  \"fit_band_hz\": [{}, {}],", report.fit_band_hz.0, report.fit_band_hz.1);
    let _ = writeln!(text, "  \"band_points\": {},", report.band_points);
    let _ = writeln!(text, "  \"grid_points\": {},", report.grid_points);
    let _ = writeln!(text, "  \"design_positions\": {},", report.design_positions);
    let _ = writeln!(text, "  \"columns\": [");
    for (index, column) in report.columns.iter().enumerate() {
        let comma = if index + 1 == report.columns.len() { "" } else { "," };
        let _ = writeln!(text,
            "    {{\"input_dense\": {}, \"free_paths\": {}, \"unknowns\": {}, \"iterations\": {}, \"relative_residual\": {:.9}, \"converged\": {}, \"stop_reason\": \"{}\"}}{}",
            column.input_dense, column.free_paths, column.unknowns, column.iterations,
            column.relative_residual, column.converged, column.stop_reason, comma);
    }
    text.push_str("  ],\n");
    let _ = writeln!(text, "  \"relative_residual_max\": {:.9},", report.relative_residual_max);
    let _ = writeln!(text, "  \"predicted_design_residual_max_db\": {:.6},", report.predicted_design_residual_max_db);
    let _ = writeln!(text, "  \"predicted_fit_band_residual_max_db\": {:.6},", report.predicted_fit_band_residual_max_db);
    let _ = writeln!(text, "  \"improvement_band_residual_max_db\": {:.6},", report.predicted_design_residual_max_db);
    match &report.predicted_residual_worst {
        Some((label, role, db)) => {
            let _ = writeln!(text, "  \"predicted_residual_worst\": {{\"position\": \"{}\", \"role\": \"{}\", \"db\": {:.6}}},", label, role, db);
        }
        None => text.push_str("  \"predicted_residual_worst\": null,\n"),
    }
    let _ = writeln!(text, "  \"correction_coeff_max_db\": {},", report.correction_coeff_max_db.map(|v| format!("{v:.6}")).unwrap_or("null".to_string()));
    let _ = writeln!(text, "  \"offdiag_max_db\": {:.6},", report.offdiag_max_db);
    let _ = writeln!(text, "  \"diag_deviation_max_db\": {:.6},", report.diag_deviation_max_db);
    let _ = writeln!(text, "  \"preserve_expected_identity\": {},", report.preserve_expected_identity);
    let ident = &report.identifiability;
    let _ = writeln!(text, "  \"identifiability\": {{\"participants\": {}, \"design_positions\": {}, \"per_frequency_rank_min\": {}, \"per_frequency_rank_max\": {}, \"position_deficit\": {}, \"condition_max\": {}, \"fir_rank_bound\": {}, \"unknowns_max_per_input\": {}, \"potentially_full_column_rank\": {}, \"underdetermined\": {}}},",
        ident.participants, ident.design_positions, ident.per_frequency_rank_min, ident.per_frequency_rank_max,
        ident.position_deficit,
        ident.condition_max.map(|v| format!("{v:.6}")).unwrap_or("null".to_string()),
        ident.fir_rank_bound, ident.unknowns_max_per_input, ident.potentially_full_column_rank, ident.underdetermined);
    let _ = writeln!(text, "  \"resource\": {{\"within_limits\": {}, \"work_used\": {}, \"estimated_work\": {}}},",
        report.resource.within_limits, report.resource.work_used, report.resource.estimated_work);
    let _ = writeln!(text, "  \"claims\": \"{}\",", escape(report.claims));
    text.push_str("  \"warnings\": [");
    for (index, warning) in report.warnings.iter().enumerate() {
        if index > 0 { text.push_str(", "); }
        let _ = write!(text, "{{\"code\": \"{}\", \"detail\": \"{}\"}}", warning.code, escape(&warning.message));
    }
    text.push_str("],\n");
    let _ = writeln!(text, "  \"applied\": {},", report.applied);
    let _ = writeln!(text, "  \"post_apply_measurement\": \"{}\",", report.post_apply_measurement);
    let _ = writeln!(text, "  \"runtime_validation\": \"{}\",", report.runtime_validation);
    let _ = writeln!(text, "  \"solution_present\": {}", outcome.solution.is_some());
    text.push_str("}\n");
    fs::write(dir.join("solve_report.json"), text).map_err(|e| format!("write solve_report: {e}"))?;
    let _ = problem;
    Ok(())
}

fn write_regularize_report(dir: &Path, outcome: &RegularizeOutcome) -> Result<(), String> {
    let report = &outcome.report;
    let mut text = String::new();
    text.push_str("{\n");
    let _ = writeln!(text, "  \"schema\": \"tspi-g-singlepoint-regularize-report/v1\",");
    let _ = writeln!(text, "  \"scope\": \"SINGLE_POINT_NOT_SPATIAL\",");
    let _ = writeln!(text, "  \"mode\": \"{}\",", report.mode);
    let _ = writeln!(text, "  \"deliverable\": {},", report.deliverable);
    let _ = writeln!(text, "  \"deliverable_note\": \"P=1 < M=5: regularize marks applicability not_applicable_p_below_m; deliverable is false by contract (no spatial identifiability), G is minimum-norm/best-effort only\",");
    let _ = writeln!(text, "  \"applicability\": {{\"m\": {}, \"design_positions\": {}, \"p_ge_m\": {}, \"status\": \"{}\", \"cv_requires_p_gt_m\": {}}},",
        report.applicability.m, report.applicability.design_positions, report.applicability.p_ge_m,
        report.applicability.status, report.applicability.cv_requires_p_gt_m);
    let _ = writeln!(text, "  \"lambda_scale\": {:.9},", report.lambda_scale);
    let sel = &report.selection;
    let _ = writeln!(text, "  \"selection\": {{\"method\": \"{}\", \"selected_whitelist\": \"{}\", \"selected_lambda\": {}, \"selected_lambda_used\": {:.9}, \"cv_valid\": {}, \"cv_reason\": {}, \"fallback_used\": {}, \"l_curve_corner_lambda\": {}}},",
        sel.method, sel.selected_whitelist.as_str(), sel.selected_lambda, sel.selected_lambda_used,
        sel.cv_valid,
        match sel.cv_reason { Some(reason) => format!("\"{}\"", escape(reason)), None => "null".to_string() },
        sel.fallback_used,
        sel.l_curve_corner_lambda.map(|v| format!("{v}")).unwrap_or("null".to_string()));
    let scan = &report.lambda_scan;
    let _ = writeln!(text, "  \"lambda_scan_whitelist\": \"{}\",", scan.scan_whitelist.as_str());
    let _ = writeln!(text, "  \"lambda_scan\": [");
    for (index, point) in scan.points.iter().enumerate() {
        let comma = if index + 1 == scan.points.len() { "" } else { "," };
        let _ = writeln!(text,
            "    {{\"lambda\": {}, \"lambda_used\": {:.9}, \"design_residual_norm\": {:.9}, \"solution_norm\": {:.9}, \"fit_band_max_db\": {:.6}, \"improvement_band_max_db\": {:.6}, \"solve_status\": \"{}\", \"headroom_feasible\": {}}}{}",
            point.lambda, point.lambda_used, point.design_residual_norm, point.solution_norm,
            point.fit_band_max_db, point.improvement_band_max_db, point.solve_status,
            point.headroom_feasible, comma);
    }
    text.push_str("  ],\n");
    let _ = writeln!(text, "  \"candidates\": [");
    for (index, candidate) in report.candidates.iter().enumerate() {
        let comma = if index + 1 == report.candidates.len() { "" } else { "," };
        let _ = writeln!(text,
            "    {{\"whitelist\": \"{}\", \"free_paths\": {}, \"lambda\": {}, \"lambda_used\": {:.9}, \"cv_valid\": {}, \"cv_rank\": {}, \"per_frequency_rank_bound\": {}, \"fir_rank_bound\": {}, \"per_frequency_full_rank_necessary\": {}, \"fir_rank_necessary\": {}}}{}",
            candidate.whitelist.as_str(), candidate.free_paths, candidate.lambda, candidate.lambda_used,
            candidate.cv_valid,
            candidate.cv_rank.map(|v| format!("{v}")).unwrap_or("null".to_string()),
            candidate.applicability.per_frequency_rank_bound, candidate.applicability.fir_rank_bound,
            candidate.applicability.per_frequency_full_rank_necessary, candidate.applicability.fir_rank_necessary,
            comma);
    }
    text.push_str("  ],\n");
    let residual = &report.final_residual;
    let _ = writeln!(text, "  \"final_residual\": {{\"fit_band_max_db\": {:.6}, \"improvement_band_max_db\": {:.6}, \"residual_norm\": {:.9}, \"valid_points\": {}, \"near_zero_points\": {}}},",
        residual.fit_band_max_db, residual.improvement_band_max_db, residual.residual_norm,
        residual.valid_points, residual.near_zero_points);
    let headroom = &report.headroom;
    let _ = writeln!(text, "  \"headroom\": {{\"passed\": {}, \"worst_excess_db\": {:.6}, \"max_correction_gain_db\": {:.6}, \"max_diagonal_gain_db\": {:.6}, \"correction_cap_db\": {}, \"diagonal_cap_db\": {}, \"tolerance_db\": {}}},",
        headroom.passed, headroom.worst_excess_db, headroom.max_correction_gain_db,
        headroom.max_diagonal_gain_db, headroom.correction_cap_db, headroom.diagonal_cap_db,
        headroom.tolerance_db);
    let _ = writeln!(text, "  \"prior_kind\": \"{}\",", report.prior_kind);
    let _ = writeln!(text, "  \"prior_distance_norm\": {:.9},", report.prior_distance_norm);
    let _ = writeln!(text, "  \"final_solve_status\": \"{}\",", report.final_solve_status);
    let _ = writeln!(text, "  \"solution_hash\": {},", opt_str(&report.solution_hash));
    let _ = writeln!(text, "  \"coeffs_sha256\": {},", opt_str(&report.coeffs_sha256));
    let _ = writeln!(text, "  \"base_solve_hash\": {},", opt_str(&report.base_solve_hash));
    let _ = writeln!(text, "  \"applied\": {},", report.applied);
    let _ = writeln!(text, "  \"post_apply_measurement\": \"{}\",", report.post_apply_measurement);
    let _ = writeln!(text, "  \"runtime_validation\": \"{}\",", report.runtime_validation);
    let _ = writeln!(text, "  \"claims\": \"{}\",", escape(report.claims));
    text.push_str("  \"warnings\": [");
    for (index, warning) in report.warnings.iter().enumerate() {
        if index > 0 { text.push_str(", "); }
        let _ = write!(text, "{{\"code\": \"{}\", \"detail\": \"{}\"}}", warning.code, escape(&warning.message));
    }
    text.push_str("]\n");
    text.push_str("}\n");
    fs::write(dir.join("regularize_report.json"), text).map_err(|e| format!("write regularize_report: {e}"))?;
    Ok(())
}

fn opt_str(value: &Option<String>) -> String {
    match value {
        Some(text) => format!("\"{}\"", escape(text)),
        None => "null".to_string(),
    }
}

fn write_g(dir: &Path, coeffs: &[f64], taps: usize) -> Result<(), String> {
    let mut text = String::from("out_role,in_role,tap,coeff\n");
    let m = ROLES.len();
    if coeffs.len() != m * m * taps {
        return Err(format!("coeff layout mismatch: {} != {}", coeffs.len(), m * m * taps));
    }
    for out in 0..m {
        for input in 0..m {
            for tap in 0..taps {
                let _ = writeln!(text, "{},{},{},{}", ROLES[out], ROLES[input], tap, coeffs[(out * m + input) * taps + tap]);
            }
        }
    }
    fs::write(dir.join("G_coeffs.csv"), text).map_err(|e| format!("write G_coeffs: {e}"))?;
    Ok(())
}

/// 预测响应：对每个 input/freq，predicted = sum_out H[f,out] * G[out,in](f)；
/// 误差 dB = 20log10(|pred-target|/|target|)。
fn write_predicted(
    dir: &Path,
    problem: &SolveProblem,
    h: &Matrix,
    target: &Matrix,
    coeffs: &[f64],
    taps: usize,
) -> Result<(), String> {
    let m = ROLES.len();
    let mut text = String::from("freq_hz,in_role,pred_re,pred_im,target_re,target_im,err_db\n");
    for (freq_index, freq) in h.freq_hz.iter().copied().enumerate() {
        // 先算每个 (out,in) 的 G 复响应（直接 DFT，零延迟基线 D=0）
        let mut g = vec![(0.0f64, 0.0f64); m * m];
        for out in 0..m {
            for input in 0..m {
                let mut acc_re = 0.0;
                let mut acc_im = 0.0;
                for tap in 0..taps {
                    let coeff = coeffs[(out * m + input) * taps + tap];
                    let angle = -2.0 * std::f64::consts::PI * freq / 48_000.0 * tap as f64;
                    acc_re += coeff * angle.cos();
                    acc_im += coeff * angle.sin();
                }
                if out == input {
                    acc_re += 1.0; // delta_D=0 → 对角基线 1
                }
                g[out * m + input] = (acc_re, acc_im);
            }
        }
        for input in 0..m {
            let mut pred = (0.0f64, 0.0f64);
            for out in 0..m {
                let hv = h.values[freq_index][out];
                let gv = g[out * m + input];
                pred.0 += hv.re * gv.0 - hv.im * gv.1;
                pred.1 += hv.re * gv.1 + hv.im * gv.0;
            }
            let tv = target.values[freq_index][input];
            let err = ((pred.0 - tv.re).hypot(pred.1 - tv.im)) / tv.re.hypot(tv.im).max(1e-12);
            let err_db = 20.0 * err.max(1e-12).log10();
            let _ = writeln!(text, "{},{},{},{},{},{},{}", freq, ROLES[input], pred.0, pred.1, tv.re, tv.im, err_db);
        }
    }
    let _ = problem;
    fs::write(dir.join("predicted.csv"), text).map_err(|e| format!("write predicted: {e}"))?;
    Ok(())
}

fn write_meta(
    dir: &Path,
    args: &Args,
    problem: &SolveProblem,
    base: &SolveOutcome,
    reg: &RegularizeOutcome,
    input_scale: f64,
) -> Result<(), String> {
    let mut text = String::new();
    text.push_str("{\n");
    let _ = writeln!(text, "  \"schema\": \"tspi-g-singlepoint-meta/v1\",");
    let _ = writeln!(text, "  \"tag\": \"{}\",", escape(&args.tag));
    let _ = writeln!(text, "  \"scope\": \"SINGLE_POINT_NOT_SPATIAL\",");
    let _ = writeln!(text, "  \"h_file\": \"{}\",", escape(&args.h_file.display().to_string()));
    let _ = writeln!(text, "  \"target_file\": \"{}\",", escape(&args.target_file.as_ref().map(|p| p.display().to_string()).unwrap_or_default()));
    let _ = writeln!(text, "  \"roles\": [{}],", ROLES.iter().map(|r| format!("\"{r}\"")).collect::<Vec<_>>().join(", "));
    let _ = writeln!(text, "  \"design_positions\": {},", problem.positions.len());
    let _ = writeln!(text, "  \"m\": {},", problem.m());
    let _ = writeln!(text, "  \"taps\": {},", args.taps);
    let _ = writeln!(text, "  \"input_scale_c_max_H\": {:.9},", input_scale);
    let _ = writeln!(text, "  \"input_scale_note\": \"H and target divided by c=max|H|; G and relative residual dB are invariant to this scalar\",");
    let _ = writeln!(text, "  \"fit_band_hz\": [{}, {}],", args.fit_lo, args.fit_hi);
    let _ = writeln!(text, "  \"grid_points\": {},", problem.grid_hz.len());
    let _ = writeln!(text, "  \"target_semantics\": \"{}\",", if args.target_file.is_some() {
        "PROBE_TARGET = measured H_eff x |H_S2_PEQ(f)| (magnitude-only); NOT a T50 target; no decay claim"
    } else {
        "PRESERVE_SEMANTICS_TARGET = measured H_eff (target == H_eff, same as T50 preserve definition)"
    });
    let _ = writeln!(text, "  \"solve_status\": \"{}\",", base.report.status.as_str());
    let _ = writeln!(text, "  \"solve_predicted_fit_band_residual_max_db\": {:.6},", base.report.predicted_fit_band_residual_max_db);
    let _ = writeln!(text, "  \"solve_predicted_improvement_band_residual_max_db\": {:.6},", base.report.predicted_design_residual_max_db);
    let _ = writeln!(text, "  \"regularize_deliverable\": {},", reg.report.deliverable);
    let _ = writeln!(text, "  \"regularize_applicability\": \"{}\",", reg.report.applicability.status);
    let _ = writeln!(text, "  \"selected_whitelist\": \"{}\",", reg.report.selection.selected_whitelist.as_str());
    let _ = writeln!(text, "  \"selected_lambda\": {},", reg.report.selection.selected_lambda);
    let _ = writeln!(text, "  \"selected_lambda_used\": {:.9},", reg.report.selection.selected_lambda_used);
    let _ = writeln!(text, "  \"final_residual_fit_band_max_db\": {:.6},", reg.report.final_residual.fit_band_max_db);
    let _ = writeln!(text, "  \"final_residual_improvement_band_max_db\": {:.6},", reg.report.final_residual.improvement_band_max_db);
    let _ = writeln!(text, "  \"solution_norm_selected\": {:.9},", reg.report.prior_distance_norm);
    let _ = writeln!(text, "  \"coeffs_sha256\": {},", opt_str(&reg.report.coeffs_sha256));
    let _ = writeln!(text, "  \"solution_hash\": {},", opt_str(&reg.report.solution_hash));
    let _ = writeln!(text, "  \"applied\": false,");
    let _ = writeln!(text, "  \"measured\": false,");
    let _ = writeln!(text, "  \"post_apply_measurement\": \"not_requested\",");
    let _ = writeln!(text, "  \"runtime_validation\": \"not_run\",");
    let _ = writeln!(text, "  \"note\": \"P=1<M=5 underdetermined: G is one minimum-norm representative, NOT a spatial design; no T53/T54 artifact can be produced from a single point\"");
    text.push_str("}\n");
    fs::write(dir.join("meta.json"), text).map_err(|e| format!("write meta: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// artifact-gate：T53/T54 状态门的直接验证（不伪造 READY）
// ---------------------------------------------------------------------------

fn run_artifact_gate(args: &Args) -> Result<(), String> {
    let dir = if args.out_dir.as_os_str().is_empty() { PathBuf::from(".") } else { args.out_dir.clone() };
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    let hash = "a".repeat(64);
    let incomplete = ArtifactBinding::from_parts(
        hash.clone(), hash.clone(), hash.clone(), hash.clone(), hash.clone(),
        hash.clone(), hash.clone(), "INCOMPLETE",
    );
    let ready = ArtifactBinding::from_parts(
        hash.clone(), hash.clone(), hash.clone(), hash.clone(), hash.clone(),
        hash.clone(), hash.clone(), "READY",
    );
    let incomplete_result = incomplete.validate();
    let ready_result = ready.validate();
    let mut text = String::new();
    text.push_str("{\n");
    text.push_str("  \"schema\": \"tspi-g-singlepoint-artifact-gate-demo/v1\",\n");
    text.push_str("  \"purpose\": \"T54 artifact status gate is the first design-side blocker for a measured single-point path: status INCOMPLETE must be refused by compile_apply::compile_plan (CompileApplyRequest::validate -> artifact.validate)\",\n");
    let _ = writeln!(text, "  \"incomplete_status_refused\": {},", incomplete_result.is_err());
    match &incomplete_result {
        Ok(()) => text.push_str("  \"incomplete_error\": null,\n"),
        Err(error) => {
            let _ = writeln!(text, "  \"incomplete_error\": {{\"code\": \"{}\", \"field\": \"{}\", \"detail\": \"{}\"}},",
                error.code(), escape(error.field()), escape(error.detail()));
        }
    }
    let _ = writeln!(text, "  \"ready_status_accepted\": {},", ready_result.is_ok());
    match &ready_result {
        Ok(()) => text.push_str("  \"ready_error\": null,\n"),
        Err(error) => {
            let _ = writeln!(text, "  \"ready_error\": {{\"code\": \"{}\", \"detail\": \"{}\"}},", error.code(), escape(&error.detail().to_string()));
        }
    }
    text.push_str("  \"note\": \"READY here is only a status-literal control on synthetic hashes; this harness never writes/claims a real READY artifact and never runs compile for the measured design\"\n");
    text.push_str("}\n");
    fs::write(dir.join("artifact_gate.json"), text).map_err(|e| format!("write artifact_gate: {e}"))?;
    println!("artifact gate: INCOMPLETE refused={} READY accepted={}", incomplete_result.is_err(), ready_result.is_ok());
    Ok(())
}

// ---------------------------------------------------------------------------
// measured-design：走生产 measured spec 桥，验证 artifact 绑定状态
// ---------------------------------------------------------------------------

fn run_measured_design(args: &Args) -> Result<(), String> {
    let spec_path = args.spec.as_ref().ok_or("measured-design needs --spec FILE")?;
    let spec = load_measured_spec(spec_path).map_err(|e| format!("load_measured_spec: {e}"))?;
    let measured = build_measured_design(&spec).map_err(|e| format!("build_measured_design: {e}"))?;
    let dir = &args.out_dir;
    fs::create_dir_all(dir).map_err(|e| format!("mkdir: {e}"))?;
    fs::write(dir.join("measured_summary.json"), &measured.summary_json)
        .map_err(|e| format!("write measured_summary: {e}"))?;
    let design = &measured.design;
    let artifact_status = design.artifact.status.clone();
    let artifact_validate = design.artifact.validate();
    let diag_paths = design.g_paths.iter().filter(|path| path.diag_baseline).count();
    let mut text = String::new();
    text.push_str("{\n");
    text.push_str("  \"schema\": \"tspi-g-singlepoint-measured-design-probe/v1\",\n");
    let _ = writeln!(text, "  \"spec\": \"{}\",", escape(&spec_path.display().to_string()));
    let _ = writeln!(text, "  \"design_source\": \"{}\",", escape(&design.design_source));
    let _ = writeln!(text, "  \"measured\": {},", design.measured);
    let _ = writeln!(text, "  \"layout\": \"{}\",", escape(&spec.layout));
    let _ = writeln!(text, "  \"roles\": [{}],", spec.roles.iter().map(|r| format!("\"{}\"", escape(&r.role_id))).collect::<Vec<_>>().join(", "));
    let _ = writeln!(text, "  \"artifact_status\": \"{}\",", escape(&artifact_status));
    let _ = writeln!(text, "  \"artifact_ready\": {},", design.artifact.is_ready());
    let _ = writeln!(text, "  \"artifact_validate_error\": {},", match &artifact_validate {
        Ok(()) => "null".to_string(),
        Err(error) => format!("{{\"code\": \"{}\", \"detail\": \"{}\"}}", error.code(), escape(&error.detail().to_string())),
    });
    let _ = writeln!(text, "  \"g_paths\": {},", design.g_paths.len());
    let _ = writeln!(text, "  \"g_diagonal_paths\": {},", diag_paths);
    let _ = writeln!(text, "  \"compile_l_g\": {},", design.compile.l_g);
    let _ = writeln!(text, "  \"compile_l_p\": {},", design.compile.l_p);
    let _ = writeln!(text, "  \"compile_low_band_d\": {},", design.compile.low_band_d);
    let _ = writeln!(text, "  \"g_is_identity\": {},", design.g_paths.iter().all(|path| {
        let is_diag = path.diag_baseline;
        let head_ok = if is_diag { path.coeffs.first().copied() == Some(1.0) } else { path.coeffs.first().copied() == Some(0.0) };
        let rest_zero = path.coeffs.iter().skip(1).all(|value| *value == 0.0);
        (is_diag && head_ok && rest_zero) || (!is_diag && path.coeffs.iter().all(|value| *value == 0.0))
    }));
    let _ = writeln!(text, "  \"measurement_digest_in_summary\": true,");
    text.push_str("  \"notes\": [\"measured path 硬接 identity G + INCOMPLETE artifact（measured_design.rs:765-804）；本 probe 证明 G 注入入口 NOT_WIRED\"],\n");
    text.push_str("  \"g_injection_wired\": false\n");
    text.push_str("}\n");
    fs::write(dir.join("measured_probe.json"), text).map_err(|e| format!("write measured_probe: {e}"))?;
    println!("measured design: artifact_status={} artifact_ready={} g_paths={} l_g={} g_is_identity=unknown",
        artifact_status, design.artifact.is_ready(), design.g_paths.len(), design.compile.l_g);
    Ok(())
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}