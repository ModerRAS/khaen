//! 皇帝位求解命令行。
//!
//! 自检：`emperor_focus --self-check`
//! 求解：`emperor_focus solve --fs 48000 --primary a.f32 --support b.f32 --out-dir DIR`
//! 反卷积：`emperor_focus deconv --rec rec.f32 --sweep sweep.f32 --out ir.f32`
//! 渲染：`emperor_focus render --sweep sweep.f32 --g-primary g.f32 --out play.f32`
//!
//! f32 文件是小端单声道。渲染输出按通道首尾相接。

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use khaen_mimo_design::emperor::{
    decimate_48_to_12, design_channel, low_extension_hz, refuse_legacy_fill_sha, render_48k,
    score_decay, seat_radius_m, EmperorParams,
};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "--self-check") {
        return match self_check() {
            Ok(()) => {
                println!("SELF_CHECK_OK predicted_only");
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("SELF_CHECK_FAIL {err}");
                ExitCode::from(1)
            }
        };
    }
    let cmd = args[0].as_str();
    let result = match cmd {
        "solve" => cmd_solve(&args[1..]),
        "deconv" => cmd_deconv(&args[1..]),
        "render" => cmd_render(&args[1..]),
        "sweep" => cmd_sweep(&args[1..]),
        "score" => cmd_score(&args[1..]),
        "claim" => cmd_claim(&args[1..]),
        _ => Err(format!("未知命令 {cmd}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::from(2)
        }
    }
}

fn self_check() -> Result<(), String> {
    let mut p = EmperorParams::default();
    p.d12 = 32;
    let h = synth_mode(12_000.0, 63.0, 0.08, 0.8, 1e-5);
    let d = design_channel(&h, &[], &p)?;
    if d.predicted_tail_ratio >= 0.5 {
        return Err(format!("尾巴没有变短，比值 {}", d.predicted_tail_ratio));
    }
    p.f_hi = 151.0;
    if design_channel(&h, &[], &p).is_ok() {
        return Err("151 Hz 应被拒绝".to_string());
    }
    Ok(())
}

fn cmd_solve(args: &[String]) -> Result<(), String> {
    let fs: f64 = arg_f64(args, "--fs").unwrap_or(48_000.0);
    let f_hi = arg_f64(args, "--f-hi").unwrap_or(150.0);
    let primary = read_f32(&req(args, "--primary")?)?;
    let mut supports = Vec::new();
    let mut support_paths = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--support" {
            let path = args.get(i + 1).ok_or("--support 缺少路径")?;
            supports.push(read_f32(path)?);
            support_paths.push(path.clone());
            i += 2;
            continue;
        }
        i += 1;
    }
    if let Some(sha) = arg_str(args, "--sha") {
        refuse_legacy_fill_sha(&sha)?;
    }
    let out_dir = PathBuf::from(req(args, "--out-dir")?);
    fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let mut params = EmperorParams::default();
    params.f_hi = f_hi;
    let (primary12, supports12) = if (fs - 48_000.0).abs() < 1.0 {
        let p = decimate_48_to_12(&primary, params.resamp_taps)?;
        let mut s = Vec::new();
        for h in &supports {
            s.push(decimate_48_to_12(h, params.resamp_taps)?);
        }
        let n = p
            .len()
            .min(s.iter().map(|v| v.len()).min().unwrap_or(p.len()));
        let p = p[..n].to_vec();
        let s = s.into_iter().map(|v| v[..n].to_vec()).collect();
        (p, s)
    } else if (fs - 12_000.0).abs() < 1.0 {
        (primary, supports)
    } else {
        return Err("采样率只接受 48000 或 12000".to_string());
    };
    if params.f_lo <= 0.0 {
        if let Some(hz) = low_extension_hz(&primary12, params.fs) {
            if hz < params.f_hi {
                params.f_lo = hz;
            }
        }
    }
    let f_lo = params.f_lo;
    match design_channel(&primary12, &supports12, &params) {
        Ok(d) => {
            write_f32(&out_dir.join("g_primary.f32"), &d.g_primary)?;
            for (i, g) in d.g_support.iter().enumerate() {
                write_f32(&out_dir.join(format!("g_support_{i}.f32")), g)?;
            }
            let metric = match d.metric {
                khaen_mimo_design::emperor::DecayMetric::T30 => "T30",
                khaen_mimo_design::emperor::DecayMetric::T20 => "T20",
            };
            let supports_json = json_arr(&support_paths);
            let report = format!(
                "{{\"claim\":\"PREDICTED_ONLY\",\"single_seat\":true,\"f_hi\":{f_hi},\"f_lo\":{f_lo:.1},\"t_keep_s\":{:.3},\"w_tail\":{:.1},\"lambda0\":{:.4},\"supports\":{supports_json},\"tail_s\":{:.6},\"taps\":{},\"partitions\":{},\"delta_sys_48\":{},\"metric\":\"{metric}\",\"engine_playable\":{},\"radius_m\":{:.4},\"descent_db\":{:.3},\"pre_gain_db\":{:.3},\"predicted_tail_ratio\":{:.6}}}\n",
                params.t_keep_s,
                params.w_tail,
                params.lambda0,
                d.tail_s,
                d.taps,
                d.partitions,
                d.delta_sys_48,
                d.engine_playable,
                seat_radius_m(f_hi),
                d.descent_db,
                d.pre_gain_db,
                d.predicted_tail_ratio,
            );
            fs::write(out_dir.join("report.json"), report).map_err(|e| e.to_string())?;
            println!(
                "SOLVE_OK metric={metric} taps={} engine_playable={} predicted_tail_ratio={:.4}",
                d.taps, d.engine_playable, d.predicted_tail_ratio
            );
            Ok(())
        }
        Err(err) => {
            let report = format!("{{\"claim\":\"REFUSED\",\"reason\":{}}}\n", json_str(&err));
            fs::write(out_dir.join("report.json"), report).map_err(|e| e.to_string())?;
            println!("SOLVE_REFUSED {err}");
            Err(err)
        }
    }
}

fn cmd_deconv(args: &[String]) -> Result<(), String> {
    let rec = read_f32(&req(args, "--rec")?)?;
    let sweep = read_f32(&req(args, "--sweep")?)?;
    let ir = deconvolve(&rec, &sweep)?;
    write_f32(&req(args, "--out")?, &ir)?;
    println!("DECONV_OK samples={}", ir.len());
    Ok(())
}

fn cmd_render(args: &[String]) -> Result<(), String> {
    let sweep = read_f32(&req(args, "--sweep")?)?;
    let g_primary = read_f32(&req(args, "--g-primary")?)?;
    let mut supports = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--g-support" {
            let path = args.get(i + 1).ok_or("--g-support 缺少路径")?;
            supports.push(read_f32(path)?);
            i += 2;
            continue;
        }
        i += 1;
    }
    let params = EmperorParams::default();
    let ch = render_48k(&sweep, &g_primary, &supports, &params)?;
    let mut cat = Vec::new();
    for c in &ch {
        cat.extend_from_slice(c);
    }
    write_f32(&req(args, "--out")?, &cat)?;
    println!(
        "RENDER_OK channels={} frames={}",
        ch.len(),
        ch.first().map(|c| c.len()).unwrap_or(0)
    );
    Ok(())
}

fn cmd_claim(args: &[String]) -> Result<(), String> {
    let sha = req(args, "--sha")?;
    match refuse_legacy_fill_sha(&sha) {
        Ok(()) => {
            println!("CLAIM_OK");
            Ok(())
        }
        Err(err) => Err(format!("CLAIM_REFUSED {err}")),
    }
}

fn cmd_score(args: &[String]) -> Result<(), String> {
    let fs = arg_f64(args, "--fs").unwrap_or(48_000.0);
    let ir = read_f32(&req(args, "--ir")?)?;
    let (metric, seconds) = score_decay(&ir, fs)?;
    let name = match metric {
        khaen_mimo_design::emperor::DecayMetric::T30 => "T30",
        khaen_mimo_design::emperor::DecayMetric::T20 => "T20",
    };
    println!("SCORE_OK metric={name} seconds={seconds:.4}");
    Ok(())
}

fn cmd_sweep(args: &[String]) -> Result<(), String> {
    let seconds = arg_f64(args, "--seconds").unwrap_or(4.0);
    let y = exp_sweep(48_000.0, 20.0, 500.0, seconds);
    write_f32(&req(args, "--out")?, &y)?;
    println!("SWEEP_OK samples={}", y.len());
    Ok(())
}

fn deconvolve(rec: &[f64], sweep: &[f64]) -> Result<Vec<f64>, String> {
    let n = (rec.len().max(sweep.len()) * 2).next_power_of_two();
    let mut rr = vec![0.0; n];
    let mut ri = vec![0.0; n];
    let mut sr = vec![0.0; n];
    let mut si = vec![0.0; n];
    rr[..rec.len()].copy_from_slice(rec);
    sr[..sweep.len()].copy_from_slice(sweep);
    khaen_mimo_design::fir::fft_in_place(&mut rr, &mut ri);
    khaen_mimo_design::fir::fft_in_place(&mut sr, &mut si);
    let max_s = sr
        .iter()
        .zip(si.iter())
        .map(|(a, b)| a * a + b * b)
        .fold(0.0, f64::max);
    let reg = 1e-3 * max_s.max(1e-12);
    for i in 0..n {
        let den = sr[i] * sr[i] + si[i] * si[i] + reg;
        let re = (rr[i] * sr[i] + ri[i] * si[i]) / den;
        let im = (ri[i] * sr[i] - rr[i] * si[i]) / den;
        rr[i] = re;
        ri[i] = im;
    }
    for v in ri.iter_mut() {
        *v = -*v;
    }
    khaen_mimo_design::fir::fft_in_place(&mut rr, &mut ri);
    let scale = n as f64;
    let keep = rec.len().min(n);
    Ok(rr[..keep].iter().map(|v| v / scale).collect())
}

fn exp_sweep(fs: f64, f0: f64, f1: f64, seconds: f64) -> Vec<f64> {
    let n = (fs * seconds) as usize;
    let ln = (f1 / f0).ln();
    let mut y = vec![0.0; n];
    for i in 0..n {
        let t = i as f64 / fs;
        let phase = std::f64::consts::TAU * f0 * seconds / ln * ((t / seconds * ln).exp() - 1.0);
        let fade =
            ((i as f64 / 256.0).min(1.0) * ((n - i) as f64 / 256.0).min(1.0)).clamp(0.0, 1.0);
        y[i] = phase.sin() * fade;
    }
    y
}

fn synth_mode(fs: f64, hz: f64, tau: f64, seconds: f64, noise: f64) -> Vec<f64> {
    let n = (fs * seconds) as usize;
    let mut y = vec![0.0; n];
    let mut s = 0x1234_5678u32;
    for (i, v) in y.iter_mut().enumerate() {
        let t = i as f64 / fs;
        *v = (-t / tau).exp() * (std::f64::consts::TAU * hz * t).sin();
        s = s.wrapping_mul(1664525).wrapping_add(1013904223);
        let u = (s >> 8) as f64 / (1u32 << 24) as f64 - 0.5;
        if t > seconds - 0.25 {
            *v = noise * u;
        }
    }
    y
}

fn req(args: &[String], key: &str) -> Result<String, String> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1).cloned())
        .ok_or_else(|| format!("缺少 {key}"))
}

fn arg_str(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1).cloned())
}

fn json_arr(items: &[String]) -> String {
    let body = items
        .iter()
        .map(|s| json_str(s))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{body}]")
}

fn arg_f64(args: &[String], key: &str) -> Option<f64> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
}

fn read_f32(path: &str) -> Result<Vec<f64>, String> {
    let bytes = fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    if bytes.len() % 4 != 0 {
        return Err(format!("{path} 长度不是 4 的倍数"));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64)
        .collect())
}

fn write_f32(path: impl AsRef<std::path::Path>, x: &[f64]) -> Result<(), String> {
    let mut bytes = Vec::with_capacity(x.len() * 4);
    for v in x {
        bytes.extend_from_slice(&(*v as f32).to_le_bytes());
    }
    fs::write(path, bytes).map_err(|e| e.to_string())
}

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
