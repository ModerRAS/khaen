//! 单点对角 P_post FIR 设计（48 kHz，T66 plan ABI v2 可表达）。
//!
//! 目的：把 T20 候选 B / 填谷的对角修正变成**全长带安全**的 48k P FIR：
//! - 20–129 Hz 用实测 H 与候选 B 目标（d=b+Σ(e^{-δT}-1)B_k b，δ 由实测 T20 锚定）；
//! - 129–150 Hz 平滑过渡；>150 Hz 直通（|P|=1），避免在 180 Hz 等处出现 +25…+55 dB 的
//!   “带外爆炸”（那会让 T66 D7 的编码 P 行 +3 dB 硬帽直接拒收，见 compile_apply.rs:2636）；
//! - 每 role 目标 |P| ≤ +3 dB（D7_SINGLE_BOOST_CAP_DB）；写 taps 2048/4096/8192 CSV。
//!
//! 状态：PREDICTED_ONLY / SINGLE_POINT_NOT_SPATIAL / NO_MEASUREMENT；未 apply、未实测。
use std::fs;
use std::path::{Path, PathBuf};

use khaen_mimo_design::decay::{
    bandpass_ir, candidate_b_target, fit_path_firs, ir_of_spectrum, t20_from_ir, BandSpec, Grid,
};
use khaen_mimo_design::fill::SinglePointH;
use khaen_mimo_design::solve::Cx;

const FS: f64 = 48_000.0;
const NFFT: usize = 131_072; // 2.73 s
const D_ALIGN: usize = 8192; // 48k 样本 = 170.7 ms 公共对齐（避免带限 IR 在 t≈0 回绕）
const D_ROLE: usize = 1024; // 21.3 ms 目标公共延迟（进入 P FIR 群延迟）
const TAPER_RISE_LO: f64 = 20.0;
const TAPER_RISE_HI: f64 = 24.0;
const TAPER_FALL_LO: f64 = 120.0;
const CAP_DB: f64 = 3.0; // D7 编码 P 行正峰硬帽
const PASS_LO_HZ: f64 = 129.0;
const PASS_HI_HZ: f64 = 150.0;

struct Args {
    h: PathBuf,
    out: PathBuf,
    taps: Vec<usize>,
    anchor_63_t20_s: f64,
    anchor_100_t20_s: f64,
    anchor_mode: String,
}

fn parse_args() -> Result<Args, String> {
    let mut h = None;
    let mut out = None;
    let mut taps = vec![2048usize, 4096, 8192];
    let mut anchor_63 = 0.5563;
    let mut anchor_100 = 0.405;
    let mut anchor_mode = "measured".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut take = |name: &str| args.next().ok_or_else(|| format!("{name} 缺参数"));
        match arg.as_str() {
            "--h" => h = Some(PathBuf::from(take("--h")?)),
            "--out" => out = Some(PathBuf::from(take("--out")?)),
            "--taps" => {
                taps = take("--taps")?
                    .split(',')
                    .map(|text| {
                        text.trim()
                            .parse::<usize>()
                            .map_err(|e| format!("--taps: {e}"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
            }
            "--anchor63" => {
                anchor_63 = take("--anchor63")?
                    .parse()
                    .map_err(|e| format!("anchor63: {e}"))?
            }
            "--anchor100" => {
                anchor_100 = take("--anchor100")?
                    .parse()
                    .map_err(|e| format!("anchor100: {e}"))?
            }
            "--anchor" => anchor_mode = take("--anchor")?,
            other => return Err(format!("未知参数 {other}")),
        }
    }
    Ok(Args {
        h: h.ok_or("缺 --h")?,
        out: out.ok_or("缺 --out")?,
        taps,
        anchor_63_t20_s: anchor_63,
        anchor_100_t20_s: anchor_100,
        anchor_mode,
    })
}

fn grid_freqs() -> Vec<f64> {
    (0..NFFT / 2 + 1)
        .map(|k| k as f64 * FS / NFFT as f64)
        .collect()
}

fn taper_weight(f: f64, data_lo: f64, data_hi: f64) -> f64 {
    if f < data_lo || f > data_hi {
        return 0.0;
    }
    let mut w = 1.0;
    if f < TAPER_RISE_HI {
        w = ((f - TAPER_RISE_LO) / (TAPER_RISE_HI - TAPER_RISE_LO)).clamp(0.0, 1.0);
    } else if f > TAPER_FALL_LO {
        w = ((data_hi - f) / (data_hi - TAPER_FALL_LO)).clamp(0.0, 1.0);
    }
    0.5 - 0.5 * (std::f64::consts::PI * w).cos()
}

fn interp_grid(host: &SinglePointH, source: usize, freqs: &[f64]) -> Vec<Cx> {
    let measured = &host.h[source];
    let mf = &host.freqs_hz;
    let data_lo = *mf.first().unwrap();
    let data_hi = *mf.last().unwrap();
    let lm: Vec<f64> = measured.iter().map(|v| v.abs().max(1e-30).ln()).collect();
    let phase: Vec<f64> = {
        let mut acc = 0.0;
        let mut prev = measured[0].im.atan2(measured[0].re);
        let mut out = Vec::with_capacity(measured.len());
        out.push(prev);
        for value in measured.iter().skip(1) {
            let mut p = value.im.atan2(value.re);
            while p - prev > std::f64::consts::PI {
                p -= std::f64::consts::TAU;
            }
            while p - prev < -std::f64::consts::PI {
                p += std::f64::consts::TAU;
            }
            acc += p - prev;
            prev = p;
            out.push(out[0] + acc);
        }
        out
    };
    let lf: Vec<f64> = mf.iter().map(|f| f.ln()).collect();
    let mut out = vec![Cx::zero(); freqs.len()];
    for (k, f) in freqs.iter().enumerate() {
        if *f < data_lo || *f > data_hi {
            continue;
        }
        let x = f.ln();
        let pos = lf.partition_point(|v| *v <= x).min(lf.len() - 1);
        let i0 = pos.saturating_sub(1);
        let i1 = pos;
        let t = if i1 > i0 {
            ((x - lf[i0]) / (lf[i1] - lf[i0])).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let mag = (lm[i0] + (lm[i1] - lm[i0]) * t).exp();
        let ph = phase[i0] + (phase[i1] - phase[i0]) * t;
        let w = taper_weight(*f, data_lo, data_hi);
        out[k] = Cx::new(mag * ph.cos() * w, mag * ph.sin() * w);
    }
    out
}

fn fir_response_at(coeffs: &[f64], fs: f64, freqs: &[f64]) -> Vec<Cx> {
    freqs
        .iter()
        .map(|f| {
            let w = std::f64::consts::TAU * f / fs;
            let mut re = 0.0;
            let mut im = 0.0;
            for (t, value) in coeffs.iter().enumerate() {
                let ph = w * t as f64;
                re += value * ph.cos();
                im -= value * ph.sin();
            }
            Cx::new(re, im)
        })
        .collect()
}

fn main() {
    if let Err(error) = run() {
        eprintln!("diag_p_fir: ERROR: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let host = SinglePointH::load_csv(&args.h)?;
    fs::create_dir_all(&args.out).map_err(|e| format!("mkdir: {e}"))?;
    let freqs = grid_freqs();
    let grid = Grid::new(FS, NFFT);
    let mut h_grid: Vec<Vec<Cx>> = Vec::new();
    for source in 0..host.n_sources() {
        h_grid.push(interp_grid(&host, source, &freqs));
    }
    let shift: Vec<Cx> = freqs
        .iter()
        .map(|f| Cx::exp_neg_j(std::f64::consts::TAU * f * D_ALIGN as f64 / FS))
        .collect();
    let baseline: Vec<Vec<f64>> = h_grid
        .iter()
        .map(|spec| {
            let delayed: Vec<Cx> = spec
                .iter()
                .zip(shift.iter())
                .map(|(h, s)| h.mul(*s))
                .collect();
            ir_of_spectrum(&delayed, NFFT)
        })
        .collect();
    let measured_anchor = |band: f64| -> f64 {
        if (band - 63.0).abs() < 1e-9 {
            3.0 * args.anchor_63_t20_s
        } else {
            3.0 * args.anchor_100_t20_s
        }
    };
    // 目标 D_i 谱：候选 B（只对 63/100 带，锚定实测 RT60）。
    let mut d_spec: Vec<Vec<Cx>> = Vec::new();
    let mut t20_rows: Vec<String> = Vec::new();
    let mut band_infos: Vec<String> = Vec::new();
    for (source, ir) in baseline.iter().enumerate() {
        let mut target = ir.clone();
        for band in [63.0f64, 100.0] {
            let component = bandpass_ir(ir, FS, band);
            let Some(estimate) = t20_from_ir(&component, FS) else {
                band_infos.push(format!(
                    "{{\"role\":{},\"band\":{band},\"valid\":false}}",
                    source
                ));
                continue;
            };
            let rt60 = if args.anchor_mode == "predicted" {
                3.0 * estimate.t20_s
            } else {
                measured_anchor(band)
            };
            let delta = 6.907_755 * (1.0 / (0.85 * rt60) - 1.0 / rt60);
            let t0 = component
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
                .map(|(i, _)| i)
                .unwrap_or(0);
            for index in t0..component.len() {
                let factor = (-delta * (index - t0) as f64 / FS).exp() - 1.0;
                target[index] += factor * component[index];
            }
            let after_est = t20_from_ir(&bandpass_ir(&target, FS, band), FS);
            band_infos.push(format!(
                "{{\"role\":{},\"band\":{band},\"t20_before\":{:.4},\"rt60_target\":{:.4},\"t20_target\":{:.4},\"valid\":true}}",
                source,
                estimate.t20_s,
                rt60,
                after_est.map(|e| e.t20_s).unwrap_or(f64::NAN)
            ));
        }
        d_spec.push(spectrum_of(&target));
    }
    let cap = 10.0f64.powf(CAP_DB / 20.0);
    let mut fir_sets: Vec<(usize, Vec<Vec<f64>>)> = Vec::new();
    for &taps in &args.taps {
        let mut coeffs: Vec<Vec<f64>> = vec![Vec::new(); host.n_sources()];
        for source in 0..host.n_sources() {
            let mut p_spec = vec![Cx::zero(); freqs.len()];
            for (k, f) in freqs.iter().enumerate() {
                let mut ratio = if *f <= PASS_LO_HZ {
                    let h = h_grid[source][k];
                    let d = d_spec[source][k];
                    if h.norm_sqr() <= 1e-24 {
                        Cx::one()
                    } else {
                        let raw = d.mul(Cx::new(h.re / h.norm_sqr(), -h.im / h.norm_sqr()));
                        // 去掉 D_ALIGN 分析平移（ratio 里含 e^{-jwD_ALIGN}），只保留 P 自己的 D_ROLE。
                        let undo = Cx::exp_pos_j(std::f64::consts::TAU * f * D_ALIGN as f64 / FS);
                        let raw = raw.mul(undo);
                        let mag = raw.abs();
                        if mag > cap {
                            raw.scale(cap / mag)
                        } else {
                            raw
                        }
                    }
                } else if *f < PASS_HI_HZ {
                    let t = ((f.ln() - PASS_LO_HZ.ln()) / (PASS_HI_HZ.ln() - PASS_LO_HZ.ln()))
                        .clamp(0.0, 1.0);
                    Cx::new(1.0 + (0.0 - 1.0) * t, 0.0)
                } else {
                    Cx::one()
                };
                let rot = Cx::exp_neg_j(std::f64::consts::TAU * f * D_ROLE as f64 / FS);
                ratio = ratio.mul(rot);
                p_spec[k] = ratio;
            }
            // Hermitian → 实 IR，取前 taps（频域采样，天然因果；不在带外/频点间振铃）。
            let ir = ir_of_spectrum(&p_spec, NFFT);
            coeffs[source] = ir[..taps.min(NFFT)].to_vec();
        }
        fir_sets.push((taps, coeffs));
    }
    let design_freqs: Vec<f64> = (0..160)
        .map(|i| 20.0 * (20_000.0f64 / 20.0).powf(i as f64 / 159.0))
        .collect();
    // 输出 + 指标
    let mut summary = String::from("{\n  \"schema\": \"khaen-mimo-diag-p-fir/v1\",\n  \"claim\": \"PREDICTED_ONLY / SINGLE_POINT_NOT_SPATIAL / NO_MEASUREMENT\",\n");
    summary.push_str(&format!(
        "  \"fs\": {FS}, \"nfft\": {NFFT}, \"d_align_samples\": {D_ALIGN}, \"d_role_samples\": {D_ROLE},\n  \"anchor_t20_63_s\": {:.4}, \"anchor_t20_100_s\": {:.4}, \"cap_db\": {CAP_DB},\n",
        args.anchor_63_t20_s, args.anchor_100_t20_s
    ));
    summary.push_str("  \"bands\": [");
    summary.push_str(&band_infos.join(", "));
    summary.push_str("],\n  \"sets\": [\n");
    let mut set_lines: Vec<String> = Vec::new();
    for (taps, coeffs) in &fir_sets {
        let directory = args.out.join(format!("taps{taps}"));
        fs::create_dir_all(&directory).map_err(|e| format!("mkdir taps: {e}"))?;
        let mut rows: Vec<String> = Vec::new();
        for (source, values) in coeffs.iter().enumerate() {
            let role = &host.roles[source];
            let mut text = String::from("tap,value\n");
            for (tap, value) in values.iter().enumerate() {
                text.push_str(&format!("{tap},{value:.17e}\n"));
            }
            fs::write(directory.join(format!("{role}.csv")), text)
                .map_err(|e| format!("write fir: {e}"))?;
            let response = fir_response_at(values, FS, &design_freqs);
            let max_db = response
                .iter()
                .map(|r| 20.0 * r.abs().max(1e-30).log10())
                .fold(f64::NEG_INFINITY, f64::max);
            let pass_db = response
                .iter()
                .zip(design_freqs.iter())
                .filter(|(_, f)| **f > PASS_HI_HZ)
                .map(|(r, _)| 20.0 * r.abs().max(1e-30).log10())
                .fold(f64::NEG_INFINITY, f64::max);
            let effort: f64 = values.iter().map(|v| v * v).sum();
            rows.push(format!(
                "{{\"role\":\"{role}\",\"taps\":{taps},\"fir_csv\":\"{role}.csv\",\"max_positive_db\":{max_db:.3},\"passband_max_db\":{pass_db:.3},\"effort\":{effort:.4},\"cap_ok\":{}}}",
                max_db <= CAP_DB + 1e-6
            ));
        }
        set_lines.push(format!(
            "    {{\"taps\": {taps}, \"roles\": [{}]}}",
            rows.join(", ")
        ));
    }
    summary.push_str(&set_lines.join(",\n"));
    summary.push_str("\n  ]\n}\n");
    fs::write(args.out.join("summary.json"), summary).map_err(|e| format!("write summary: {e}"))?;
    // 预测 T20（用实际 FIR 在 H 带内评估：先算 P actual 频率响应，Y=H*P，IFFT 得带限 IR）
    let mut report_rows: Vec<String> = Vec::new();
    for (taps, coeffs) in &fir_sets {
        for (source, values) in coeffs.iter().enumerate() {
            let p_resp = fir_spectrum(values, NFFT);
            let y_spec: Vec<Cx> = h_grid[source]
                .iter()
                .zip(p_resp.iter())
                .map(|(h, p)| h.mul(*p))
                .collect();
            let y_ir = ir_of_spectrum(&y_spec, NFFT);
            let mut row = format!("taps,{},{}", taps, host.roles[source]);
            for band in [63.0f64, 100.0] {
                let before = t20_from_ir(&bandpass_ir(&baseline[source], FS, band), FS);
                let after = t20_from_ir(&bandpass_ir(&y_ir, FS, band), FS);
                match (before, after) {
                    (Some(b), Some(a)) if b.valid && a.valid => {
                        row.push_str(&format!(
                            ",{band},{:.4},{:.4},{:.3}",
                            b.t20_s,
                            a.t20_s,
                            1.0 - a.t20_s / b.t20_s
                        ));
                    }
                    _ => row.push_str(&format!(",{band},NaN,NaN,NaN")),
                }
            }
            report_rows.push(row);
        }
    }
    let mut csv = String::from("kind,taps,role,band_hz,t20_before_s,t20_after_s,t20_drop\n");
    for row in &report_rows {
        csv.push_str(row);
        csv.push('\n');
    }
    fs::write(args.out.join("predicted_t20.csv"), csv).map_err(|e| format!("write csv: {e}"))?;
    println!("diag_p_fir: OK");
    println!("out: {}", args.out.display());
    for line in &report_rows {
        println!("{line}");
    }
    let _ = &t20_rows;
    Ok(())
}

fn spectrum_of(samples: &[f64]) -> Vec<Cx> {
    let mut re = samples.to_vec();
    let mut im = vec![0.0f64; samples.len()];
    khaen_mimo_design::fir::fft_in_place(&mut re, &mut im);
    (0..samples.len() / 2 + 1)
        .map(|k| Cx::new(re[k], im[k]))
        .collect()
}

fn fir_spectrum(coeffs: &[f64], nfft: usize) -> Vec<Cx> {
    let mut ir = vec![0.0f64; nfft];
    ir[..coeffs.len().min(nfft)].copy_from_slice(&coeffs[..coeffs.len().min(nfft)]);
    spectrum_of(&ir)
}
