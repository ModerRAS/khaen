//! 单点真实 H 上的 D7 填谷（null fill）离线设计实验（mimo-design 算法在 P=1 的扩展）。
//!
//! 用法：
//!   cargo run --release -p tspi-mimo-design --example single_point_fill -- \
//!       --h <H_complex.csv> [--gauge <gauge.json>] --out <evidence-dir> \
//!       [--taps 256,512,1024,2048,4096] [--primary-taps 2048]
//!
//! 产物（全部 SINGLE_POINT_NOT_SPATIAL / PREDICTED_ONLY）：
//! - fill_combined.csv / fill_per_channel.csv / fill_diag_self.csv / donor_criterion.csv / taps_requirements.csv
//! - fir/<gauge>/combined_<ROLE>.csv（联合填谷 FIR）
//! - summary.json（机读汇总 + 输入 sha256 + 参数）
//! - meta.json（命令行与 run 环境；证据目录另写 REPORT.md）
//!
//! 不打开任何音频设备、不 apply、不写 plan；纯离线。

use std::fs;
use std::path::{Path, PathBuf};

use tspi_mimo_design::fill::{
    band_indices, db_amp, donor_criterion, evaluate_bands, smooth_complex_power, taps_requirements,
    FillConfig, FillSolver, SinglePointH, DIAG_CAP_DB, SINGLE_POINT_CLAIM, SINGLE_POINT_FILL_SCHEMA,
};
use tspi_mimo_design::solve::Cx;

struct Args {
    h: PathBuf,
    gauge: Option<PathBuf>,
    out: PathBuf,
    taps: Vec<usize>,
    primary_taps: usize,
}

fn parse_args() -> Result<Args, String> {
    let mut h: Option<PathBuf> = None;
    let mut gauge: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut taps = vec![256usize, 512, 1024, 2048, 4096];
    let mut primary_taps = 2048usize;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let mut take = |name: &str| -> Result<String, String> {
            arguments
                .next()
                .ok_or_else(|| format!("{name} 缺少参数值"))
        };
        match argument.as_str() {
            "--h" => h = Some(PathBuf::from(take("--h")?)),
            "--gauge" => gauge = Some(PathBuf::from(take("--gauge")?)),
            "--out" => out = Some(PathBuf::from(take("--out")?)),
            "--taps" => {
                taps = take("--taps")?
                    .split(',')
                    .map(|text| {
                        text.trim()
                            .parse::<usize>()
                            .map_err(|error| format!("--taps {text:?} 非法：{error}"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
            }
            "--primary-taps" => {
                primary_taps = take("--primary-taps")?
                    .trim()
                    .parse::<usize>()
                    .map_err(|error| format!("--primary-taps 非法：{error}"))?;
            }
            other => return Err(format!("未知参数 {other:?}")),
        }
    }
    Ok(Args {
        h: h.ok_or_else(|| "缺少 --h".to_string())?,
        gauge,
        out: out.ok_or_else(|| "缺少 --out".to_string())?,
        taps,
        primary_taps,
    })
}

/// 读 gauge.json 的 relative_to_ref_db（peak_db 代理）；缺失返回 None。
fn load_gauge_relative(path: &Path) -> Result<Vec<(String, f64)>, String> {
    let document = tspi_room_sim::json::Json::parse_file(path)
        .map_err(|error| format!("gauge.json 解析失败: {error}"))?;
    let section = document
        .get("relative_to_ref_db")
        .ok_or_else(|| "gauge.json 缺 relative_to_ref_db".to_string())?;
    let entries = section
        .as_object()
        .ok_or_else(|| "gauge.json relative_to_ref_db 不是对象".to_string())?;
    let mut out = Vec::with_capacity(entries.len());
    for (role, value) in entries {
        let db = value
            .as_f64()
            .ok_or_else(|| format!("gauge.json {role} 不是数字"))?;
        out.push((role.clone(), db));
    }
    Ok(out)
}

fn scaled_host(host: &SinglePointH, relative_db: &[(String, f64)]) -> Result<SinglePointH, String> {
    let mut factors = Vec::with_capacity(host.roles.len());
    for role in &host.roles {
        let db = relative_db
            .iter()
            .find(|(name, _)| name == role)
            .map(|(_, value)| *value)
            .ok_or_else(|| format!("gauge.json 缺通道 {role}"))?;
        factors.push(10.0f64.powf(db / 20.0));
    }
    Ok(SinglePointH {
        freqs_hz: host.freqs_hz.clone(),
        roles: host.roles.clone(),
        h: host
            .h
            .iter()
            .enumerate()
            .map(|(index, source)| source.iter().map(|value| value.scale(factors[index])).collect())
            .collect(),
    })
}

fn write_csv(path: &Path, header: &str, rows: &[String]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("建目录失败: {error}"))?;
    }
    let mut text = String::with_capacity(header.len() + rows.iter().map(|row| row.len() + 1).sum::<usize>());
    text.push_str(header);
    text.push('\n');
    for row in rows {
        text.push_str(row);
        text.push('\n');
    }
    fs::write(path, text).map_err(|error| format!("写 {} 失败: {error}", path.display()))
}

fn band_row_columns() -> &'static str {
    "band_hz,n_points,before_rms_db,after_rms_db,delta_rms_db,valley_freq_hz,before_min_db,after_at_valley_db,fill_at_valley_db,target_fill_at_valley_db,depth_before_db,depth_after_db,depth_reduction_db"
}

fn band_row_values(rows: &[tspi_mimo_design::fill::BandFillRow]) -> Vec<String> {
    rows.iter()
        .map(|row| {
            format!(
                "{:.1},{},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3}",
                row.center_hz,
                row.n_points,
                row.before_rms_db,
                row.after_rms_db,
                row.delta_rms_db,
                row.valley_freq_hz,
                row.before_min_db,
                row.after_at_valley_db,
                row.fill_at_valley_db,
                row.target_fill_at_valley_db,
                row.depth_before_db,
                row.depth_after_db,
                row.depth_reduction_db
            )
        })
        .collect()
}

fn design_to_fir_csvs(out: &Path, gauge: &str, tag: &str, design: &tspi_mimo_design::fill::FillDesign) -> Result<(), String> {
    let directory = out.join("fir").join(gauge);
    fs::create_dir_all(&directory).map_err(|error| format!("建 fir 目录失败: {error}"))?;
    for (index, coefficients) in design.h.iter().enumerate() {
        let role = &design.h.len();
        let _ = role;
        let path = directory.join(format!("{tag}_source{index}.csv"));
        let mut text = String::with_capacity(coefficients.len() * 22 + 16);
        text.push_str("tap,value\n");
        for (tap, value) in coefficients.iter().enumerate() {
            text.push_str(&format!("{tap},{value:.17e}\n"));
        }
        fs::write(&path, text).map_err(|error| format!("写 {} 失败: {error}", path.display()))?;
    }
    Ok(())
}

fn json_string_list(values: &[String]) -> String {
    let mut out = String::from("[");
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(&value.replace('\\', "\\\\").replace('"', "\\\""));
        out.push('"');
    }
    out.push(']');
    out
}

fn main() {
    if let Err(error) = run() {
        eprintln!("mimo single-point fill: ERROR: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let host_base = SinglePointH::load_csv(&args.h)?;
    if host_base.n_sources() == 0 {
        return Err("H 没有通道".to_string());
    }
    let h_sha = tspi_room_sim::sha256::file_hex(&args.h)
        .map_err(|error| format!("H sha256 失败: {error}"))?;
    fs::create_dir_all(&args.out).map_err(|error| format!("建 out 目录失败: {error}"))?;
    let relative = match &args.gauge {
        Some(path) => load_gauge_relative(path)?,
        None => Vec::new(),
    };
    let mut gauges: Vec<(String, SinglePointH)> = vec![("asgiven".to_string(), host_base.clone())];
    if !relative.is_empty() {
        gauges.push(("peakproxy".to_string(), scaled_host(&host_base, &relative)?));
    }

    // ---- donor 判据 + taps 需求（按 gauge；donor 口径相关，taps 为同通道比值=口径无关）----
    let mut donor_rows: Vec<String> = Vec::new();
    for (gauge_name, host) in &gauges {
        for row in donor_criterion(host) {
            donor_rows.push(format!(
                "{gauge_name},{},{:.1},{:.3},{:.3},{:.3},{:.3},{:.3},{},{:.3},{:.3},{},{},{},{}",
                row.role,
                row.center_hz,
                row.valley_freq_hz,
                row.level_rms_db,
                row.valley_depth_db,
                row.diag_cap_fill_db,
                row.cross_path_needed_db,
                row.best_donor,
                row.best_donor_level_db,
                row.donor_margin_db,
                if row.donor_ok { "YES" } else { "NO" },
                row.headroom_gate,
                row.band_rms_rank,
                row.verdict
            ));
        }
    }
    let taps_rows_source = taps_requirements(&host_base);
    {
        let mut rows: Vec<String> = Vec::new();
        for row in &taps_rows_source {
            rows.push(format!(
                "{},{:.1},{:.3},{:.3},{:.3},{},{},{},{},{}",
                row.role,
                row.center_hz,
                row.valley_freq_hz,
                row.valley_depth_db,
                row.width_m3db_hz,
                row.taps_needed_pointwise,
                if row.pointwise_within_l_g_4096 { "YES" } else { "NO" },
                row.taps_needed_band_level,
                if row.band_level_within_l_g_4096 { "YES" } else { "NO" },
                if row.center_hz >= 63.0 { "covered" } else { "BLIND" }
            ));
        }
        write_csv(
            &args.out.join("taps_requirements.csv"),
            "role,band_hz,valley_freq_hz,valley_depth_db,width_m3db_hz,taps_needed_pointwise,pointwise_within_L_G_4096,taps_needed_band_level,band_level_within_L_G_4096,headroom_gate",
            &rows,
        )?;
    }
    write_csv(
        &args.out.join("donor_criterion.csv"),
        "gauge,role,band_hz,valley_freq_hz,level_rms_db,valley_depth_db,diag_cap_fill_db,cross_path_needed_db,best_donor,best_donor_level_db,donor_margin_db,donor_ok,headroom_gate,band_rms_rank,verdict",
        &donor_rows,
    )?;

    // ---- 主实验 ----
    let mut combined_rows: Vec<String> = Vec::new();
    let mut per_channel_rows: Vec<String> = Vec::new();
    let mut diag_self_rows: Vec<String> = Vec::new();
    let mut summary_combined: Vec<String> = Vec::new();
    let mut summary_per_channel: Vec<String> = Vec::new();
    let mut summary_diag: Vec<String> = Vec::new();
    let mut summary_headroom: Vec<String> = Vec::new();
    let mut fir_written: Vec<String> = Vec::new();

    for (gauge_name, host) in &gauges {
        let before = host.identity_pressure();
        let smooth_combined = smooth_complex_power(&before, &host.freqs_hz, 1.0 / 6.0);
        let mut primary_solver: Option<FillSolver> = None;
        for &taps in &args.taps {
            let cfg = FillConfig::new(taps);
            let solver = FillSolver::new(&host.freqs_hz, cfg.clone())?;
            let design = match solver.design(host, &smooth_combined) {
                Ok(design) => design,
                Err(error) => {
                    combined_rows.push(format!("{gauge_name},{taps},ERROR,{error}"));
                    continue;
                }
            };
            let after = host.pressure(&design.g);
            let rows = evaluate_bands(&host.freqs_hz, &before, &after, Some(&smooth_combined), &before);
            let values = band_row_values(&rows);
            for value in &values {
                combined_rows.push(format!("{gauge_name},{taps},{value}"));
            }
            let fill_max = rows.iter().map(|row| row.fill_at_valley_db).fold(f64::NEG_INFINITY, f64::max);
            let depth_reduction_max = rows
                .iter()
                .map(|row| row.depth_reduction_db)
                .fold(f64::NEG_INFINITY, f64::max);
            summary_combined.push(format!(
                "{{\"gauge\":\"{gauge_name}\",\"taps\":{taps},\"fill_at_valley_max_db\":{fill_max:.3},\"depth_reduction_max_db\":{depth_reduction_max:.3},\"max_gain_db\":{:.3},\"max_gain_gate_db\":{:.3},\"max_f_positive_db\":{:.3},\"effort\":{:.6},\"predicted_peak\":{:.4}}}",
                design.max_gain_db,
                design.max_gain_gate_db,
                design.max_f_positive_db,
                design.effort,
                design.predicted_peak
            ));
            if taps == args.primary_taps {
                design_to_fir_csvs(&args.out, gauge_name, "combined", &design)?;
                fir_written.push(format!("fir/{gauge_name}/combined_source*.csv"));
                primary_solver = Some(solver);
            }
        }
        // per-channel fill：只换通道 i 的 target，其它 preserve（联合求解，donor 参与）。
        if let Some(solver) = primary_solver {
            for (source_index, role) in host.roles.iter().enumerate() {
                let smooth_channel =
                    smooth_complex_power(&host.h[source_index], &host.freqs_hz, 1.0 / 6.0);
                let others = host.others_pressure(source_index);
                let target: Vec<Cx> = others
                    .iter()
                    .zip(smooth_channel.iter())
                    .map(|(other, channel)| other.add(*channel))
                    .collect();
                let design = match solver.design(host, &target) {
                    Ok(design) => design,
                    Err(error) => {
                        per_channel_rows.push(format!(
                            "{gauge_name},{},{role},ERROR,{error}",
                            args.primary_taps
                        ));
                        continue;
                    }
                };
                let after = host.pressure(&design.g);
                let rows = evaluate_bands(
                    &host.freqs_hz,
                    &before,
                    &after,
                    Some(&target),
                    &host.h[source_index],
                );
                let values = band_row_values(&rows);
                for value in &values {
                    per_channel_rows.push(format!(
                        "{gauge_name},{},{role},{value}",
                        args.primary_taps
                    ));
                }
                let worst = rows
                    .iter()
                    .filter(|row| row.depth_before_db > 6.0)
                    .max_by(|a, b| a.depth_before_db.partial_cmp(&b.depth_before_db).unwrap());
                if let Some(worst) = worst {
                    let ratio = if worst.target_fill_at_valley_db.abs() > 1e-9 {
                        worst.fill_at_valley_db / worst.target_fill_at_valley_db
                    } else {
                        f64::NAN
                    };
                    summary_per_channel.push(format!(
                        "{{\"gauge\":\"{gauge_name}\",\"role\":\"{role}\",\"band_hz\":{:.1},\"valley_freq_hz\":{:.3},\"depth_db\":{:.3},\"fill_at_valley_db\":{:.3},\"target_fill_db\":{:.3},\"fill_ratio\":{:.3},\"max_gain_db\":{:.3},\"max_gain_gate_db\":{:.3},\"effort\":{:.6}}}",
                        worst.center_hz,
                        worst.valley_freq_hz,
                        worst.depth_before_db,
                        worst.fill_at_valley_db,
                        worst.target_fill_at_valley_db,
                        ratio,
                        design.max_gain_db,
                        design.max_gain_gate_db,
                        design.effort
                    ));
                }
                // diag-only 理想上界（不做 FIR；只 F_i 自由）。
                let cap = 10.0f64.powf(DIAG_CAP_DB / 20.0);
                let mut f_ideal: Vec<Vec<Cx>> = vec![vec![Cx::one(); host.len()]; host.n_sources()];
                for k in 0..host.len() {
                    let h_i = host.h[source_index][k];
                    if h_i.norm_sqr() <= 1e-30 {
                        continue;
                    }
                    let ideal = smooth_channel[k].mul(Cx::new(
                        h_i.re / h_i.norm_sqr(),
                        -h_i.im / h_i.norm_sqr(),
                    ));
                    let magnitude = ideal.abs();
                    f_ideal[source_index][k] = if magnitude > cap {
                        ideal.scale(cap / magnitude)
                    } else {
                        ideal
                    };
                }
                let after_ideal = host.pressure(&f_ideal);
                let ideal_rows = evaluate_bands(
                    &host.freqs_hz,
                    &before,
                    &after_ideal,
                    None,
                    &host.h[source_index],
                );
                for row in &ideal_rows {
                    diag_self_rows.push(format!(
                        "{gauge_name},{},{role},{:.1},{},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3}",
                        args.primary_taps,
                        row.center_hz,
                        row.n_points,
                        row.depth_before_db,
                        row.fill_at_valley_db,
                        row.delta_rms_db,
                        row.before_min_db,
                        row.after_at_valley_db,
                        row.after_rms_db
                    ));
                }
                let worst_ideal = ideal_rows
                    .iter()
                    .filter(|row| row.depth_before_db > 6.0)
                    .max_by(|a, b| a.depth_before_db.partial_cmp(&b.depth_before_db).unwrap());
                if let Some(worst_ideal) = worst_ideal {
                    summary_diag.push(format!(
                        "{{\"gauge\":\"{gauge_name}\",\"role\":\"{role}\",\"band_hz\":{:.1},\"depth_db\":{:.3},\"ideal_diag_fill_db\":{:.3}}}",
                        worst_ideal.center_hz,
                        worst_ideal.depth_before_db,
                        worst_ideal.fill_at_valley_db
                    ));
                }
                summary_headroom.push(format!(
                    "{{\"gauge\":\"{gauge_name}\",\"role\":\"{role}\",\"max_gain_db\":{:.3},\"max_gain_gate_db\":{:.3},\"max_f_positive_db\":{:.3},\"cap_db\":{:.1},\"headroom_gate_pass\":{},\"target_fir_positive_cap_db\":3.0,\"target_fir_positive_pass\":{}}}",
                    design.max_gain_db,
                    design.max_gain_gate_db,
                    design.max_f_positive_db,
                    DIAG_CAP_DB,
                    design.max_gain_gate_db <= DIAG_CAP_DB + 1e-9,
                    design.max_f_positive_db <= 3.0 + 1e-9
                ));
            }
        }
    }

    write_csv(
        &args.out.join("fill_combined.csv"),
        &format!("gauge,taps,{}", band_row_columns()),
        &combined_rows,
    )?;
    write_csv(
        &args.out.join("fill_per_channel.csv"),
        &format!("gauge,taps,fill_role,{}", band_row_columns()),
        &per_channel_rows,
    )?;
    write_csv(
        &args.out.join("fill_diag_self.csv"),
        "gauge,taps,role,band_hz,n_points,depth_before_db,fill_at_valley_db,delta_rms_db,before_min_db,after_at_valley_db,after_rms_db",
        &diag_self_rows,
    )?;

    // ---- summary.json ----
    let command_line: Vec<String> = std::env::args().skip(1).collect();
    let summary = format!(
        "{{\n  \"schema\": \"{schema}\",\n  \"claim\": \"{claim}\",\n  \"input\": {{\"h_csv\": \"{h}\", \"h_sha256\": \"{h_sha}\", \"gauge_json\": {gauge_json}}},\n  \"config\": {{\"primary_taps\": {primary}, \"taps\": {taps_list}, \"cap_db\": {cap}, \"smooth_octave\": 0.16666666666666666, \"lambda_rel\": 0.000001}},\n  \"combined\": [{combined}],\n  \"per_channel\": [{per_channel}],\n  \"diag_self\": [{diag}],\n  \"headroom\": [{headroom}],\n  \"fir_files\": {fir_files},\n  \"command\": {command}\n}}\n",
        schema = SINGLE_POINT_FILL_SCHEMA,
        claim = SINGLE_POINT_CLAIM,
        h = args.h.display().to_string().replace('\\', "/"),
        h_sha = h_sha,
        gauge_json = match &args.gauge {
            Some(path) => format!("\"{}\"", path.display().to_string().replace('\\', "/")),
            None => "null".to_string(),
        },
        primary = args.primary_taps,
        taps_list = json_string_list(&args.taps.iter().map(|value| value.to_string()).collect::<Vec<_>>()),
        cap = DIAG_CAP_DB,
        combined = summary_combined.join(", "),
        per_channel = summary_per_channel.join(", "),
        diag = summary_diag.join(", "),
        headroom = summary_headroom.join(", "),
        fir_files = json_string_list(&fir_written),
        command = json_string_list(&command_line)
    );
    fs::write(args.out.join("summary.json"), summary)
        .map_err(|error| format!("写 summary.json 失败: {error}"))?;
    println!("mimo single-point fill: OK");
    println!("claim: {SINGLE_POINT_CLAIM}");
    println!("out: {}", args.out.display());
    for line in &summary_combined {
        println!("combined: {line}");
    }
    for line in &summary_per_channel {
        println!("per_channel: {line}");
    }
    for line in &summary_diag {
        println!("diag_self: {line}");
    }
    for line in &summary_headroom {
        println!("headroom: {line}");
    }
    let _ = band_indices;
    let _ = db_amp;
    Ok(())
}