//! khaen-calibrate：多声道扬声器延迟/极性/相位/PEQ 校准工具。
//! 单二进制；测量用 aplay/arecord（S24_3LE），DSP/拟合/合并验证全在 Rust 内完成。
mod check;
mod dsp;
mod modal;
mod model;
mod peq;
mod raw;
mod robust;
mod sweep;
mod target;

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use model::{
    Calibration, ChannelCalibration, ChannelMeasurement, FrPoint, Meta, Peq, ReferenceSummary,
    RepeatDetail, ValidationCheck, ValidationReport,
};

fn die(msg: &str) -> ! {
    eprintln!("ERROR: {msg}");
    std::process::exit(1);
}

struct A {
    m: HashMap<String, String>,
    multi: HashMap<String, Vec<String>>,
    pos: Vec<String>,
}
impl A {
    fn new(args: &[String]) -> Result<Self, String> {
        let mut m = HashMap::new();
        let mut multi: HashMap<String, Vec<String>> = HashMap::new();
        let mut pos = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            if a == "--help" || a == "-h" {
                m.insert("help".into(), "1".into());
                i += 1;
                continue;
            }
            if let Some(k) = a.strip_prefix("--") {
                let key = k.to_string();
                if i + 1 < args.len() && !args[i + 1].starts_with("--") {
                    m.insert(key.clone(), args[i + 1].clone());
                    multi.entry(key).or_default().push(args[i + 1].clone());
                    i += 2;
                } else {
                    m.insert(key.clone(), "1".into());
                    multi.entry(key).or_default().push("1".into());
                    i += 1;
                }
            } else {
                pos.push(a.clone());
                i += 1;
            }
        }
        Ok(Self { m, multi, pos })
    }
    fn all(&self, k: &str) -> Vec<String> {
        self.multi.get(k).cloned().unwrap_or_default()
    }
    fn get(&self, k: &str) -> Option<&str> {
        self.m.get(k).map(|s| s.as_str())
    }
    fn str(&self, k: &str, d: &str) -> String {
        self.get(k).unwrap_or(d).to_string()
    }
    fn f64(&self, k: &str, d: f64) -> f64 {
        self.get(k).and_then(|v| v.parse().ok()).unwrap_or(d)
    }
    fn u32(&self, k: &str, d: u32) -> u32 {
        self.get(k).and_then(|v| v.parse().ok()).unwrap_or(d)
    }
    fn usize(&self, k: &str, d: usize) -> usize {
        self.get(k).and_then(|v| v.parse().ok()).unwrap_or(d)
    }
    fn req(&self, k: &str) -> String {
        self.get(k)
            .map(|s| s.to_string())
            .unwrap_or_else(|| die(&format!("缺少 --{k}")))
    }
}

#[derive(Clone)]
struct SweepParams {
    rate: u32,
    start_hz: f64,
    end_hz: f64,
    seconds: f64,
    level_db: f64,
    pre: f64,
    post: f64,
}
impl SweepParams {
    fn from(a: &A) -> Self {
        let rate = a.u32("rate", 48000);
        Self {
            rate,
            start_hz: a.f64("start-hz", 20.0),
            end_hz: a.f64("end-hz", (rate as f64 * 0.45).min(20_000.0)),
            seconds: a.f64("seconds", 5.0),
            level_db: a.f64("level-db", -30.0),
            pre: a.f64("pre-seconds", 1.0),
            post: a.f64("post-seconds", 1.0),
        }
    }
    fn source(&self) -> Vec<f64> {
        let spec = sweep::SweepSpec {
            sample_rate: self.rate,
            start_hz: self.start_hz,
            end_hz: self.end_hz,
            seconds: self.seconds,
            level_db: self.level_db,
            fade_ms: 10.0,
        };
        spec.generate()
    }
}

fn requested_fr_points(a: &A) -> Option<usize> {
    if let Some(n) = a.get("fr-points") {
        return n.parse::<usize>().ok().filter(|n| *n >= 2);
    }
    let spec = a.get("fr-oct")?;
    let per_oct = if let Some((num, den)) = spec.split_once('/') {
        let num: f64 = num.parse().ok()?;
        let den: f64 = den.parse().ok()?;
        if num <= 0.0 || den <= 0.0 {
            return None;
        }
        den / num
    } else {
        spec.parse::<f64>().ok()?
    };
    let f_max = (a.u32("rate", 48000) as f64 * 0.45).min(20_000.0);
    Some(dsp::fr_point_count(20.0, f_max, per_oct))
}

fn base_measurement(
    ch: usize,
    delay: f64,
    coarse: i64,
    polarity: i32,
    peak_db: f64,
    phase: f64,
    snr: f64,
    fr: Vec<FrPoint>,
    meta: Meta,
) -> ChannelMeasurement {
    ChannelMeasurement {
        schema: "khaen-channel-measurement/v1".into(),
        channel: ch,
        delay_samples: delay,
        delay_ms: if meta.sample_rate == 0 {
            0.0
        } else {
            delay / meta.sample_rate as f64 * 1000.0
        },
        delay_coarse_samples: coarse,
        polarity,
        peak_db,
        phase_1k_deg: phase,
        snr_db: snr,
        phase_ref:
            "aligned_segment(已去粗延迟); absolute_phase_deg = phase_deg - 360*f*delay_samples/fs"
                .into(),
        fr,
        fr_modal: Vec::new(),
        meta,
        repeats: None,
        clock_ppm: None,
        clock_ppm_ok: None,
        clock_ppm_uncertainty_ppm: None,
        clock_ppm_significant: None,
        drift_corrected: None,
        snr_band_db: None,
        snr_band_hz: None,
        onset_invalid: None,
        polarity_confidence: None,
        polarity_estimators: None,
        polarity_robust: None,
        polarity_listening: None,
        polarity_source: None,
        sub_polarity_basis: None,
        reference: None,
        measurement_role: None,
        position: None,
        reference_invalid: None,
        reference_invalid_reason: None,
    }
}

const REFERENCE_MIN_SNR_DB: f64 = 20.0;

/// 参考麦有任一硬伤就不许覆盖主麦极性。缺字段（旧的单遍结果）不当成失败。
fn reference_block_reason(rm: &ChannelMeasurement) -> Option<String> {
    let mut why = Vec::new();
    if rm.clock_ppm_ok == Some(false) {
        why.push("clock_ppm_ok=false".to_string());
    }
    if rm.onset_invalid == Some(true) {
        why.push("onset_invalid=true".to_string());
    }
    if rm.snr_db < REFERENCE_MIN_SNR_DB {
        why.push(format!("snr_db={:.1}<20", rm.snr_db));
    }
    if rm.polarity_confidence.as_deref().unwrap_or("unknown") == "unknown" {
        why.push("polarity_confidence=unknown".to_string());
    }
    if why.is_empty() {
        None
    } else {
        Some(why.join("; "))
    }
}

fn apply_reference_override(
    meas: &mut ChannelMeasurement,
    rm: &ChannelMeasurement,
    robust_on: bool,
    named_sub: bool,
    nearfield: bool,
    ch: usize,
    channel: usize,
    name: &str,
    file: &str,
) {
    let block = reference_block_reason(rm);
    meas.reference = Some(ReferenceSummary {
        channel,
        name: name.to_string(),
        polarity: rm.polarity,
        polarity_confidence: rm
            .polarity_confidence
            .clone()
            .unwrap_or_else(|| "unknown".into()),
        delay_samples: rm.delay_samples,
        clock_ppm: rm.clock_ppm,
        file: file.to_string(),
        valid: block.is_none(),
        invalid_reason: block.clone(),
    });
    if let Some(why) = block {
        meas.reference_invalid = Some(true);
        meas.reference_invalid_reason = Some(why.clone());
        eprintln!("WARN ch{ch}: 参考麦无效（{why}），不覆盖主麦极性");
        return;
    }
    meas.reference_invalid = None;
    meas.reference_invalid_reason = None;
    let use_ref = robust_on && (!named_sub || nearfield) && rm.polarity != 0;
    if !use_ref {
        return;
    }
    let listening_now = meas.polarity_listening.unwrap_or(0);
    meas.polarity = rm.polarity;
    meas.polarity_source = Some("reference".into());
    if listening_now != 0 && listening_now != rm.polarity {
        meas.polarity_confidence = Some("low".into());
        eprintln!(
            "WARN ch{ch}: 主麦极性 {listening_now:+} 与参考麦 {:+} 不一致，confidence=low",
            rm.polarity
        );
    } else {
        meas.polarity_confidence = rm.polarity_confidence.clone();
    }
    if named_sub && nearfield {
        meas.sub_polarity_basis = Some("nearfield reference mic".into());
    }
}

fn apply_sign(v: i32, off: i32) -> i32 {
    if v == 0 {
        0
    } else {
        v * off
    }
}

fn flip_estimators(e: &model::PolarityEstimators, off: i32) -> model::PolarityEstimators {
    model::PolarityEstimators {
        ir_peak: apply_sign(e.ir_peak, off),
        direct_window: apply_sign(e.direct_window, off),
        phase_vote: apply_sign(e.phase_vote, off),
        lowband: apply_sign(e.lowband, off),
        matched: apply_sign(e.matched, off),
        reference_xcorr: e.reference_xcorr,
    }
}

fn meta(sp: &SweepParams, pb: &str, pbc: usize, cap: &str, capc: usize) -> Meta {
    Meta {
        schema: "khaen-calibration-meta/v1".into(),
        sample_rate: sp.rate,
        channels: pbc,
        sweep_start_hz: sp.start_hz,
        sweep_end_hz: sp.end_hz,
        sweep_seconds: sp.seconds,
        level_db: sp.level_db,
        pre_seconds: sp.pre,
        post_seconds: sp.post,
        playback_device: pb.into(),
        playback_channels: pbc,
        capture_device: cap.into(),
        capture_channels: capc,
        created_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    }
}

fn write_fr_csv(path: &Path, fr: &[FrPoint]) -> Result<(), String> {
    let mut s = String::from("# phase_ref=aligned_segment(已去粗延迟); absolute_phase_deg = phase_deg - 360*f*delay_samples/fs\nfreq_hz,db,phase_deg,re,im\n");
    for p in fr {
        let re = 10.0f64.powf(p.db / 20.0) * p.phase_deg.to_radians().cos();
        let im = 10.0f64.powf(p.db / 20.0) * p.phase_deg.to_radians().sin();
        s.push_str(&format!(
            "{:.6},{:.4},{:.2},{:.9},{:.9}\n",
            p.freq_hz, p.db, p.phase_deg, re, im
        ));
    }
    fs::write(path, s).map_err(|e| format!("write {}: {e}", path.display()))
}

fn read_measurement(path: &Path) -> Result<ChannelMeasurement, String> {
    let txt = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_str(&txt).map_err(|e| format!("parse {}: {e}", path.display()))
}

fn cmd_sweep(a: &A) -> Result<(), String> {
    let sp = SweepParams::from(a);
    let channels = a.usize("channels", 22).max(1);
    let ch = a.usize("channel", 0);
    let out = PathBuf::from(a.req("out"));
    let source = sp.source();
    raw::write_sweep_file(&out, channels, ch, &source, sp.rate, sp.pre, sp.post)?;
    let bytes = fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    println!(
        "wrote {} ({} ch x S24_3LE, target ch{}, {} B)",
        out.display(),
        channels,
        ch,
        bytes
    );
    Ok(())
}

fn cmd_sweep_all(a: &A) -> Result<(), String> {
    let sp = SweepParams::from(a);
    let channels = a.usize("channels", 22).max(1);
    let dir = PathBuf::from(a.req("dir"));
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let source = sp.source();
    for ch in 0..channels {
        let out = dir.join(format!("sweep-ch{ch:02}.raw"));
        raw::write_sweep_file(&out, channels, ch, &source, sp.rate, sp.pre, sp.post)?;
    }
    println!("wrote {} sweep files under {}", channels, dir.display());
    Ok(())
}

fn cmd_measure(a: &A) -> Result<(), String> {
    let sp = SweepParams::from(a);
    let pbc = a.usize("playback-channels", 22).max(1);
    let ch = a.usize("channel", 0);
    let capc = a.usize("capture-channels", 2).max(1);
    let pb = a.str("playback", "hw:4,0");
    let cap = a.str("capture", "hw:3,0");
    let out = PathBuf::from(a.req("out"));
    let dir = out
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    fs::create_dir_all(&dir).ok();
    let sweep_path = std::env::temp_dir().join(format!("khaen-cal-sweep-ch{ch}.raw"));
    let source = sp.source();
    raw::write_sweep_file(&sweep_path, pbc, ch, &source, sp.rate, sp.pre, sp.post)?;
    let cap_seconds = sp.pre + sp.seconds + sp.post + 0.8;
    println!(
        "measure: playback={pb} ({pbc}ch) capture={cap} ({capc}ch) ch={ch} level={}dBFS",
        sp.level_db
    );
    let period = a.usize("period-size", 128);
    let buffer = a.usize("buffer-size", 512);
    if !(32..=4096).contains(&period) || buffer < period {
        return Err(format!(
            "period={period} buffer={buffer} 不合法（period 32..4096，buffer≥period）"
        ));
    }
    if a.str("mic", "") == "umik" && sp.rate != 48000 {
        return Err(format!(
            "UMIK-1 仅支持 48 kHz，当前 --rate={}；96k 请换麦克风/做漂移补偿",
            sp.rate
        ));
    }
    raw::measure_channel(
        &pb,
        pbc,
        &cap,
        capc,
        sp.rate,
        cap_seconds,
        &sweep_path,
        &out,
        period,
        buffer,
    )?;
    let _ = fs::remove_file(&sweep_path);
    println!("capture saved {}", out.display());
    Ok(())
}

fn cmd_analyze(a: &A) -> Result<(), String> {
    let sp = SweepParams::from(a);
    let capture_path = PathBuf::from(a.req("capture"));
    let capc = a.usize("capture-channels", 2).max(1);
    let cap_idx = a.usize("capture-channel", 0).min(capc - 1);
    let ch = a.usize("channel", 0);
    let prefix = PathBuf::from(a.req("out-prefix"));
    let source = sp.source();
    let caps = raw::read_s24(&capture_path, capc)?;
    let capture = &caps[cap_idx];
    // 去掉源未开始的静音影响：analyze_capture 自行用互相关对齐
    let pre_samples = (sp.pre * sp.rate as f64).round() as usize;
    let pol_off = if a.str("polarity-offset", "normal") == "invert" {
        -1
    } else {
        1
    };
    let repeats_n = a.usize("repeats", 1);
    if repeats_n == 0 || repeats_n > 8 {
        return Err(format!("--repeats {repeats_n} 超出 1..=8"));
    }
    if repeats_n > 1 {
        let gap_n = (a.f64("repeat-gap", 0.2) * sp.rate as f64).round() as usize;
        let interval = source.len() + gap_n;
        let drift = a.get("drift-correct").is_some();
        let max_ppm = a.f64("max-ppm", 100.0);
        let agg = robust::analyze_repeats(
            capture,
            &source,
            sp.rate,
            sp.start_hz,
            sp.end_hz,
            interval as f64,
            repeats_n,
            drift,
            max_ppm,
            None,
        )?;
        for wmsg in &agg.warnings {
            eprintln!("WARN: {wmsg}");
        }
        let committed = apply_sign(agg.robust_polarity, pol_off);
        let mut meas = base_measurement(
            ch,
            agg.delay_samples,
            agg.delay_coarse_samples,
            committed,
            agg.peak_db,
            agg.phase_1k_deg,
            agg.snr_db,
            agg.fr.clone(),
            meta(&sp, "", 0, &capture_path.display().to_string(), capc),
        );
        meas.fr_modal = agg.fr_modal.clone();
        meas.polarity_confidence = Some(agg.confidence.clone());
        meas.polarity_estimators = Some(flip_estimators(&agg.estimators, pol_off));
        meas.polarity_robust = Some(committed);
        meas.polarity_listening = Some(apply_sign(agg.listening_polarity, pol_off));
        meas.polarity_source = Some("robust".into());
        meas.repeats = Some(agg.repeats.clone());
        meas.clock_ppm = agg.clock_ppm;
        meas.clock_ppm_ok = Some(agg.clock_ppm_ok);
        meas.clock_ppm_uncertainty_ppm = Some(agg.clock_uncertainty_ppm);
        meas.clock_ppm_significant = Some(agg.clock_significant);
        meas.drift_corrected = Some(agg.drift_corrected);
        meas.onset_invalid = Some(agg.onset_invalid);
        meas.snr_band_db = Some(agg.snr_band_db);
        meas.snr_band_hz = Some(agg.snr_band_hz);
        if sp.end_hz <= 200.0 || a.str("name", "").to_ascii_uppercase().starts_with("SUB") {
            meas.polarity_confidence = Some("unknown".into());
            meas.polarity = 0;
            meas.sub_polarity_basis = Some("listening-position only; SUB 带不提交极性".into());
            eprintln!("WARN: SUB/带限极性 confidence=unknown，不提交符号");
        }
        write_fr_csv(
            &PathBuf::from(format!("{}.fr.csv", prefix.display())),
            &meas.fr,
        )?;
        if !meas.fr_modal.is_empty() {
            write_fr_csv(
                &PathBuf::from(format!("{}.fr_modal.csv", prefix.display())),
                &meas.fr_modal,
            )?;
        }
        fs::write(
            format!("{}.analysis.json", prefix.display()),
            serde_json::to_string_pretty(&meas).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("write analysis: {e}"))?;
        write_repeat_csv(
            &PathBuf::from(format!("{}.repeats.csv", prefix.display())),
            &agg.repeats,
        )?;
        println!("channel {ch}: repeats={repeats_n} delay={:.3} samples polarity={:+} confidence={} clock_ppm={}",
            agg.delay_samples, meas.polarity, meas.polarity_confidence.as_deref().unwrap_or("-"),
            meas.clock_ppm.map(|v| format!("{v:.2}")).unwrap_or_else(|| "n/a".into()));
        return Ok(());
    }
    let r = dsp::analyze_capture(
        capture,
        &source,
        sp.rate,
        sp.start_hz,
        sp.end_hz,
        pre_samples,
        requested_fr_points(a),
    )?;
    let robust_on = a.get("robust-polarity").is_some() || a.usize("repeats", 1) > 1;
    let committed = if robust_on {
        apply_sign(r.robust_polarity, pol_off)
    } else {
        r.polarity * pol_off
    };
    let mut meas = base_measurement(
        ch,
        r.delay_samples,
        r.delay_coarse_samples,
        committed,
        r.peak_db,
        r.phase_1k_deg,
        r.snr_db,
        r.fr.clone(),
        meta(&sp, "", 0, &capture_path.display().to_string(), capc),
    );
    meas.fr_modal = r.fr_modal.clone();
    meas.snr_band_db = Some(r.snr_band_db);
    meas.snr_band_hz = Some(r.snr_band_hz);
    meas.polarity_confidence = Some(r.polarity_confidence.clone());
    meas.polarity_estimators = Some(flip_estimators(&r.estimators, pol_off));
    meas.polarity_robust = Some(apply_sign(r.robust_polarity, pol_off));
    meas.polarity_listening = Some(apply_sign(r.polarity_listening, pol_off));
    meas.polarity_source = Some(if robust_on {
        "robust".into()
    } else {
        "legacy-ir-peak".into()
    });
    if r.polarity_confidence == "unknown" {
        eprintln!(
            "WARN: 极性置信度 unknown（估计器不一致或 SUB 带）。legacy ir_peak={:+} robust={:+}",
            r.polarity, r.robust_polarity
        );
    } else if r.polarity != 0
        && r.robust_polarity != 0
        && r.polarity != r.robust_polarity
        && r.polarity_confidence == "high"
    {
        eprintln!("WARN: IR 峰极性 {:+} 与直达声/低频投票 {:+} 不一致（confidence=high）。单遍默认仍写 IR 峰；加 --robust-polarity 或 --repeats N 改用投票。", r.polarity, r.robust_polarity);
    }
    let delay_ms = meas.delay_ms;
    if let Some(imp) = a.get("impulse-out") {
        let mut s = String::from("sample,value\n");
        for (i, v) in r.impulse.iter().enumerate() {
            s.push_str(&format!("{i},{v:.9}\n"));
        }
        fs::write(imp, s).map_err(|e| format!("write impulse: {e}"))?;
    }
    write_fr_csv(
        &PathBuf::from(format!("{}.fr.csv", prefix.display())),
        &r.fr,
    )?;
    if !r.fr_modal.is_empty() {
        write_fr_csv(
            &PathBuf::from(format!("{}.fr_modal.csv", prefix.display())),
            &r.fr_modal,
        )?;
    }
    fs::write(
        format!("{}.analysis.json", prefix.display()),
        serde_json::to_string_pretty(&meas).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write analysis: {e}"))?;
    println!("channel {ch}: delay={:.3} samples ({:.4} ms), coarse={}, polarity={:+}, confidence={}, phase@1k={:.2} deg, peak={:.2} dB, snr={:.1} dB",
        meas.delay_samples, delay_ms, r.delay_coarse_samples, meas.polarity, meas.polarity_confidence.as_deref().unwrap_or("-"), r.phase_1k_deg, r.peak_db, r.snr_db);
    Ok(())
}

fn empty_report(msg: &str) -> peq::OptimizeReport {
    peq::OptimizeReport {
        filters: vec![],
        modal: vec![],
        broadband: vec![],
        rms: 0.0,
        max_abs: 0.0,
        warnings: vec![msg.to_string()],
        modal_confidence: "low".into(),
        rows: vec![],
        boost_filters: vec![],
        applied_boost_db: 0.0,
        total_boost_db: 0.0,
        max_predicted_gain_db: 0.0,
        predicted_peak_dbfs: -18.0,
        headroom_db: 18.0,
        clip_risk: false,
        headroom_reference_dbfs: -18.0,
    }
}

fn trim_report(rep: &mut peq::OptimizeReport, n: usize) {
    let modal: Vec<Peq> = rep.modal.iter().take(n).cloned().collect();
    let left = n.saturating_sub(modal.len());
    let wide: Vec<Peq> = rep.broadband.iter().take(left).cloned().collect();
    rep.modal = modal;
    rep.broadband = wide;
    rep.filters = rep
        .modal
        .iter()
        .chain(rep.broadband.iter())
        .cloned()
        .collect();
}

fn fr_for_fit(m: &ChannelMeasurement) -> Vec<FrPoint> {
    if m.fr_modal.is_empty() {
        m.fr.clone()
    } else {
        m.fr_modal.clone()
    }
}

/// analysis 里的 peak_db 是脉冲幅度，不是文件 dBFS，不能拿来做削波门限。
fn weighted_from(
    m: &ChannelMeasurement,
    fallback_name: &str,
    fallback_weight: f64,
) -> Option<peq::WeightedFr> {
    // P0-3：带限通道（SUB/截止<=200Hz）用自身通带 SNR + 20 dB 门限，
    // 否则全带 SNR + 25 dB。SUB 20-130 Hz 的带内 SNR≈24 dB 不会被误排除。
    // P0-4：削波门未实现（peak_db 是 IR 幅度，不是采集 dBFS；需要独立
    // capture_peak_dbfs 才能判削波，当前 v1.1 不提供该字段，故不做削波排除）。
    let band_limited =
        m.meta.sweep_end_hz <= 200.0 || fallback_name.to_ascii_uppercase().starts_with("SUB");
    // 带限通道用独立 20 dB 阈值（snr_band 作为报告字段，因其估计更保守，
    // 只用于诊断，不单独决定排除）；全带通道保持 25 dB。
    let (used_snr, threshold, label) = if band_limited {
        (m.snr_db, 20.0, "snr_db(band-limited,threshold=20)")
    } else {
        (m.snr_db, 25.0, "snr_db")
    };
    if used_snr < threshold {
        eprintln!(
            "WARN: 测点 {} {label} {:.1} dB < {:.0}，排除（snr_band={:?}）",
            fallback_name,
            used_snr,
            threshold,
            m.snr_band_db.map(|v| (v * 10.0).round() / 10.0)
        );
        return None;
    }
    let (name, weight) = match &m.position {
        Some(p) => (p.name.clone(), p.weight.unwrap_or(fallback_weight)),
        None => (fallback_name.to_string(), fallback_weight),
    };
    Some(peq::WeightedFr {
        name,
        weight,
        fr: fr_for_fit(m),
    })
}

fn load_positions(
    a: &A,
    primary: &ChannelMeasurement,
    channel_name: &str,
) -> Result<Vec<peq::WeightedFr>, String> {
    let Some(dir) = a.get("positions") else {
        return Ok(weighted_from(primary, "listening", 1.0)
            .into_iter()
            .collect());
    };
    let root = PathBuf::from(dir);
    let mut out = Vec::new();
    let entries =
        fs::read_dir(&root).map_err(|e| format!("读取测点目录 {}: {e}", root.display()))?;
    for ent in entries {
        let ent = ent.map_err(|e| e.to_string())?;
        let path = ent.path();
        if path.is_dir() {
            let pname = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("pos")
                .to_string();
            let weight = if pname.eq_ignore_ascii_case("listening") {
                1.0
            } else {
                0.5
            };
            let mut files: Vec<PathBuf> = fs::read_dir(&path)
                .map_err(|e| e.to_string())?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.extension().and_then(|s| s.to_str()) == Some("json")
                        && p.file_name()
                            .and_then(|s| s.to_str())
                            .map(|n| n.ends_with(".analysis.json"))
                            .unwrap_or(false)
                })
                .collect();
            files.sort();
            for f in files {
                let m = read_measurement(&f)?;
                if m.channel != primary.channel {
                    continue;
                }
                if let Some(w) = weighted_from(&m, &pname, weight) {
                    out.push(w);
                }
            }
        }
    }
    if out.is_empty() {
        eprintln!("WARN: {channel_name} 的测点目录没有可用分析，退回主分析");
        return Ok(weighted_from(primary, "listening", 1.0)
            .into_iter()
            .collect());
    }
    Ok(out)
}

fn parse_span(spec: &str) -> Option<(f64, f64)> {
    let (a, b) = spec.split_once(':')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

fn operating_band(a: &A, name: &str) -> Option<(f64, f64)> {
    let mut named = None;
    let mut global = None;
    for spec in a.all("band") {
        if let Some((n, rest)) = spec.split_once('=') {
            if n.eq_ignore_ascii_case(name) {
                named = parse_span(rest);
            }
        } else {
            global = parse_span(&spec);
        }
    }
    if let Some(span) = named.or(global) {
        return Some(span);
    }
    let hp = a.get("hp").and_then(|s| s.parse().ok());
    let lp = a.get("lp").and_then(|s| s.parse().ok());
    if hp.is_some() || lp.is_some() {
        Some((hp.unwrap_or(20.0), lp.unwrap_or(20_000.0)))
    } else {
        None
    }
}

fn optimize_measurement(
    meas: &ChannelMeasurement,
    name: &str,
    a: &A,
) -> Result<(peq::OptimizeReport, Vec<peq::WeightedFr>), String> {
    let max_stages = a.usize("max-stages", 22).min(22);
    if max_stages == 0 {
        return Ok((empty_report("max-stages=0，不拟合"), vec![]));
    }
    let positions = load_positions(a, meas, name)?;
    if positions.is_empty() {
        return Ok((empty_report("没有通过 SNR 门限的测点"), vec![]));
    }
    let mut opts = peq::OptimizeOpts::standard(
        meas.meta.sample_rate as f64,
        target::resolve(a.get("target"))?,
    );
    // D7：默认 +3 dB boost；>3 在 peq::optimize 内钳到 3、<0 归 0。
    opts.boost_cap_db = a.f64("boost-cap-db", 3.0).clamp(0.0, 3.0);
    // P0-2：带限/低 SNR 时 20–60 Hz 模态不可信。snr_band 估计偏保守（见 P0-3 报告），
    // 这里用与测点门限一致的口径：带限通道 snr_db>=20、全带通道默认可信。
    let band_limited =
        meas.meta.sweep_end_hz <= 200.0 || name.to_ascii_uppercase().starts_with("SUB");
    opts.low_band_trusted = if band_limited {
        meas.snr_db >= 20.0
    } else {
        true
    };
    if let Some((lo, hi)) = operating_band(a, name) {
        opts.op_lo = lo;
        opts.op_hi = hi;
    }
    let mut rep = peq::optimize(&positions, &opts);
    trim_report(&mut rep, max_stages);
    for w in &rep.warnings {
        eprintln!("WARN {name}: {w}");
    }
    Ok((rep, positions))
}

fn phase_sum_margin(
    meas: &[ChannelMeasurement],
    filt: &[Vec<Peq>],
    delays: &[i64],
    pols: &[i32],
    f: f64,
    rate: f64,
) -> (f64, f64) {
    let mut sum_re = 0.0f64;
    let mut sum_im = 0.0f64;
    let mut max_single = 0.0f64;
    for (i, m) in meas.iter().enumerate() {
        let mut db = 0.0;
        let mut ph = 0.0;
        // 线性插值 FR
        for w in m.fr.windows(2) {
            if w[0].freq_hz <= f && f <= w[1].freq_hz {
                let t = (f - w[0].freq_hz) / (w[1].freq_hz - w[0].freq_hz).max(1e-12);
                db = w[0].db + t * (w[1].db - w[0].db);
                ph = w[0].phase_deg + t * (w[1].phase_deg - w[0].phase_deg);
                break;
            }
        }
        let biquads: Vec<peq::Biquad> = filt
            .get(i)
            .map(|v| v.iter().map(peq::Biquad::from_peq).collect())
            .unwrap_or_default();
        let h = peq::combined_h(&biquads, f, rate);
        db += 20.0 * h.norm().max(1e-12).log10();
        let mag = 10.0f64.powf(db / 20.0);
        let dly = delays.get(i).copied().unwrap_or(0) as f64 + m.delay_samples;
        let phase = ph.to_radians() + h.arg() - 2.0 * std::f64::consts::PI * f * (dly / rate);
        let pol = pols.get(i).copied().unwrap_or(1) as f64;
        sum_re += mag * phase.cos() * pol;
        sum_im += mag * phase.sin() * pol;
        max_single = max_single.max(mag);
    }
    let max_single_db = 20.0 * max_single.max(1e-12).log10();
    let sum_db = 20.0
        * (sum_re * sum_re + sum_im * sum_im)
            .sqrt()
            .max(1e-12)
            .log10();
    (sum_db.max(-120.0), max_single_db)
}

fn cmd_fit(a: &A) -> Result<(), String> {
    let inp = PathBuf::from(a.req("analysis"));
    let meas = read_measurement(&inp)?;
    let name = a
        .get("name")
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("CH{}", meas.channel));
    let (rep, positions) = optimize_measurement(&meas, &name, a)?;
    let target_id = target::resolve(a.get("target"))?.id;
    let stages = rep.filters.clone();
    let rms = rep.rms;
    let max = rep.max_abs;
    let cal = Calibration {
        schema: "khaen-calibration/v1.1".into(),
        sample_rate: meas.meta.sample_rate,
        reference_index: meas.channel,
        layout: a.get("layout").map(|s| s.to_string()),
        created_unix: Some(now_unix()),
        tool: Some("khaen-calibrate".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        note: format!(
            "channel {} correct target={target_id}; rms_residual={:.3}dB max={:.2}dB applied_boost={:.2}dB total_boost={:.2}dB headroom={:.2}dB clip_risk={}",
            meas.channel, rms, max, rep.applied_boost_db, rep.total_boost_db, rep.headroom_db, rep.clip_risk
        ),
        channels: vec![ChannelCalibration {
            index: meas.channel,
            name: name.clone(),
            delay_samples: 0, // 单通道 fit 为中间产物：修正=0；merge 再做相对归一化
            polarity: Some(if meas.polarity < 0 {
                "invert".into()
            } else {
                "normal".into()
            }),
            filters: stages.clone(),
            gain_db: None,
        }],
    };
    let out = PathBuf::from(a.req("out"));
    fs::write(
        &out,
        serde_yaml::to_string(&cal).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write {}: {e}", out.display()))?;
    // CamillaDSP 片段
    let frag = cal_to_camilladsp(&cal);
    fs::write(
        format!("{}.camilladsp.yml", out.display()),
        serde_yaml::to_string(&frag).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write fragment: {e}"))?;
    write_prediction(&out, &[(name.as_str(), &rep)], &positions)?;
    println!(
        "fitted ch{}: {} stages, rms={:.3}dB max={:.2}dB boost={:.2}dB headroom={:.2}dB clip_risk={} -> {}",
        meas.channel,
        stages.len(),
        rms,
        max,
        rep.applied_boost_db,
        rep.headroom_db,
        rep.clip_risk,
        out.display()
    );
    Ok(())
}

fn write_prediction(
    out: &Path,
    reports: &[(&str, &peq::OptimizeReport)],
    positions: &[peq::WeightedFr],
) -> Result<(), String> {
    let multi = reports.len() > 1;
    let mut csv = String::from("freq_hz,target_db,predicted_db,residual_db");
    if multi {
        csv.push_str(",channel");
    }
    for p in positions {
        csv.push(',');
        csv.push_str(&p.name.replace(',', "_"));
    }
    csv.push('\n');
    for (name, rep) in reports {
        for row in &rep.rows {
            csv.push_str(&format!(
                "{:.4},{:.3},{:.3},{:.3}",
                row.freq_hz, row.target_db, row.predicted_db, row.residual_db
            ));
            if multi {
                csv.push(',');
                csv.push_str(&name.replace(',', "_"));
            }
            if !multi {
                for p in positions {
                    csv.push_str(&format!(",{:.3}", peq::sample_db(&p.fr, row.freq_hz)));
                }
            }
            csv.push('\n');
        }
    }
    fs::write(format!("{}.predicted.csv", out.display()), csv)
        .map_err(|e| format!("write predicted: {e}"))?;
    let channels: Vec<serde_json::Value> = reports
        .iter()
        .map(|(name, rep)| {
            serde_json::json!({
                "channel": name,
                "mode": "correct",
                "stages_modal": rep.modal,
                "stages_broadband": rep.broadband,
                "predicted_rms": rep.rms,
                "predicted_max": rep.max_abs,
                "modal_confidence": rep.modal_confidence,
                // D7：boost/headroom 报告字段
                "boost_filters": rep.boost_filters,
                "applied_boost_db": rep.applied_boost_db,
                "total_boost_db": rep.total_boost_db,
                "max_predicted_gain_db": rep.max_predicted_gain_db,
                "predicted_peak_dbfs": rep.predicted_peak_dbfs,
                "headroom_db": rep.headroom_db,
                "clip_risk": rep.clip_risk,
                "headroom_reference_dbfs": rep.headroom_reference_dbfs,
                "warnings": rep.warnings,
            })
        })
        .collect();
    fs::write(
        format!("{}.optimizer.json", out.display()),
        serde_json::to_string_pretty(&serde_json::json!({ "channels": channels }))
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write optimizer: {e}"))?;
    Ok(())
}

fn pair_specs(a: &A) -> Vec<(String, String, f64)> {
    let mut out = Vec::new();
    for item in a.all("pairs") {
        for spec in item.split(',') {
            let parts: Vec<&str> = spec.split(':').collect();
            if parts.len() != 3 {
                continue;
            }
            if let Ok(fc) = parts[2].trim().parse::<f64>() {
                out.push((parts[0].trim().to_string(), parts[1].trim().to_string(), fc));
            }
        }
    }
    out
}

fn declared_pairs_pass(
    meas: &[ChannelMeasurement],
    filt: &[Vec<Peq>],
    delays: &[i64],
    pols: &[i32],
    rate: f64,
    pairs: &[(String, String, f64)],
    channels: &[ChannelCalibration],
) -> bool {
    for (na, nb, fc) in pairs {
        let find = |name: &str| {
            channels
                .iter()
                .position(|c| c.name.eq_ignore_ascii_case(name))
        };
        let (Some(ai), Some(bi)) = (find(na), find(nb)) else {
            continue;
        };
        if !check_pair(meas, filt, delays, pols, ai, bi, *fc, rate).pass {
            return false;
        }
    }
    true
}

fn commit_stages(
    meas: &[ChannelMeasurement],
    channels: &[ChannelCalibration],
    reports: &[peq::OptimizeReport],
    a: &A,
    rate: f64,
) -> Vec<Vec<Peq>> {
    let max_stages = a.usize("max-stages", 22).min(22);
    let mut planned: Vec<Vec<Peq>> = reports
        .iter()
        .map(|r| {
            let mut s = r.modal.clone();
            s.extend(r.broadband.iter().cloned());
            s.truncate(max_stages);
            s
        })
        .collect();
    let sort_freq = |s: &mut Vec<Peq>| {
        s.sort_by(|a, b| {
            a.freq
                .partial_cmp(&b.freq)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    };
    let pairs = pair_specs(a);
    if pairs.is_empty() {
        for s in &mut planned {
            sort_freq(s);
        }
        return planned;
    }
    let delays: Vec<i64> = channels.iter().map(|c| c.delay_samples).collect();
    let pols: Vec<i32> = channels
        .iter()
        .map(|c| {
            if c.polarity.as_deref() == Some("invert") {
                -1
            } else {
                1
            }
        })
        .collect();
    let mut acc = vec![Vec::new(); planned.len()];
    for ch in 0..planned.len() {
        for f in planned[ch].drain(..) {
            acc[ch].push(f.clone());
            if !declared_pairs_pass(meas, &acc, &delays, &pols, rate, &pairs, channels) {
                let rolled = acc[ch].pop();
                if let Some(rolled) = rolled {
                    eprintln!(
                        "WARN: 回退 {} {:.1} Hz {:+.2} dB（交叉对复核不通过）",
                        channels[ch].name, rolled.freq, rolled.gain_db
                    );
                }
            }
        }
    }
    for s in &mut acc {
        sort_freq(s);
    }
    acc
}

fn apply_match_groups(
    a: &A,
    channels: &[ChannelCalibration],
    reports: &mut [peq::OptimizeReport],
    positions: &[Vec<peq::WeightedFr>],
    fs: f64,
) -> Result<(), String> {
    let max_stages = a.usize("max-stages", 22).min(22);
    if max_stages == 0 {
        return Ok(());
    }
    for spec in a.all("match") {
        let names: Vec<&str> = spec
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        if names.len() < 2 {
            continue;
        }
        let idxs: Vec<usize> = names
            .iter()
            .filter_map(|n| channels.iter().position(|c| c.name.eq_ignore_ascii_case(n)))
            .collect();
        if idxs.len() < 2 {
            eprintln!("WARN: --match {spec} 找不到成对通道");
            continue;
        }
        let Some(wa) = positions.get(idxs[0]).and_then(|v| v.first()) else {
            continue;
        };
        let Some(wb) = positions.get(idxs[1]).and_then(|v| v.first()) else {
            continue;
        };
        let mut opts = peq::OptimizeOpts::standard(fs, target::resolve(a.get("target"))?);
        // D7：默认 +3；>3 钳 3、<0 归 0
        opts.boost_cap_db = a.f64("boost-cap-db", 3.0).clamp(0.0, 3.0);
        if let Some((lo, hi)) = operating_band(a, &channels[idxs[0]].name) {
            opts.op_lo = lo;
            opts.op_hi = hi;
        }
        let (mut left, mut right, _) = peq::reconcile_pair(wa, wb, &opts);
        trim_report(&mut left, max_stages);
        trim_report(&mut right, max_stages);
        for w in left.warnings.iter().chain(right.warnings.iter()) {
            eprintln!("WARN: {w}");
        }
        reports[idxs[0]] = left;
        reports[idxs[1]] = right;
    }
    Ok(())
}

fn camilla_biquad(filter_type: &str) -> &'static str {
    match filter_type {
        "low_shelf" => "Lowshelf",
        "high_shelf" => "Highshelf",
        _ => "Peaking",
    }
}

fn cal_to_camilladsp(cal: &Calibration) -> serde_json::Value {
    let mut filters = serde_json::Map::new();
    let mut pipeline = Vec::new();
    let mut roles = Vec::new();
    for ch in &cal.channels {
        let mut names = Vec::new();
        if ch.delay_samples != 0 {
            filters.insert(format!("cal_ch{}_delay", ch.index), serde_json::json!({
                "type": "Delay", "parameters": {"delay": ch.delay_samples as f64 / cal.sample_rate as f64 * 1000.0, "unit": "ms"}
            }));
            names.push(serde_json::Value::String(format!(
                "cal_ch{}_delay",
                ch.index
            )));
        }
        for (i, p) in ch.filters.iter().enumerate() {
            let name = format!("cal_ch{}_peq{}", ch.index, i);
            filters.insert(
                name.clone(),
                // 裁决 2：shelf 的 q 是 RBJ slope S；CamillaDSP 用 Slope 型（slope=12*S）。
                serde_json::json!({
                    "type": "Biquad",
                    "parameters": if p.filter_type == "low_shelf" || p.filter_type == "high_shelf" {
                        serde_json::json!({
                            "type": camilla_biquad(&p.filter_type),
                            "freq": p.freq,
                            "slope": 12.0 * p.q,
                            "gain": p.gain_db
                        })
                    } else {
                        serde_json::json!({
                            "type": "Peaking",
                            "freq": p.freq,
                            "q": p.q,
                            "gain": p.gain_db
                        })
                    }
                }),
            );
            names.push(serde_json::Value::String(name));
        }
        // 极性必须在 pipeline push 之前加入 names；纯反相通道也要生成 pipeline。
        if ch.polarity.as_deref() == Some("invert") {
            let pname = format!("cal_ch{}_pol", ch.index);
            filters.insert(
                pname.clone(),
                serde_json::json!({"type": "Gain", "parameters": {"gain": 0.0, "inverted": true}}),
            );
            names.push(serde_json::Value::String(pname));
            // advisory：仅当消费方选择“把反相写入 channelmap roles”时才用；fragment pipeline 已实现极性。
            roles.push(serde_json::json!({
                "index": ch.index, "inverted": true, "apply": false,
                "note": "advisory only: fragment pipeline already applies Gain inverted; use channelmap roles[].sources.inverted ONLY instead of pipeline (choose one path, avoid double inversion)"
            }));
        }
        if !names.is_empty() {
            pipeline.push(
                serde_json::json!({"type": "Filter", "channels": [ch.index], "names": names}),
            );
        }
    }
    serde_json::json!({
        "schema": "khaen-calibration-camilladsp/v1",
        "note": "filters/pipeline 可直接合并进渲染后的 CamillaDSP config（极性已由 Gain inverted 实现）；channelmap_roles 仅 advisory，两条路径二选一，勿同时应用以免双反相",
        "filters": filters,
        "pipeline": pipeline,
        "channelmap_roles": roles,
    })
}

/// 按 analysis JSON 的 channel 字段升序排序（文件名只作 tie-break）。
///
/// 旧实现按文件名排序后把 --names 按顺序赋名；pilot 目录文件名顺序是
/// FL,FR,HFL,HFR,SL,SR,SUB1，而角色顺序是 FL,FR,SL,SR,HFL,HFR,SUB1，
/// 结果 HFL/HFR 与 SL/SR 的名称/延迟/滤波器全部错位（merged.yml 里 index 4 name SL）。
fn sort_measurements_by_channel(
    mut inputs: Vec<(PathBuf, ChannelMeasurement)>,
) -> Result<Vec<ChannelMeasurement>, String> {
    inputs.sort_by(|(pa, ma), (pb, mb)| ma.channel.cmp(&mb.channel).then_with(|| pa.cmp(pb)));
    for w in inputs.windows(2) {
        if w[0].1.channel == w[1].1.channel {
            return Err(format!(
                "E_MERGE_DUPLICATE_CHANNEL: channel {} 同时出现在 {} 和 {}，拒绝合并",
                w[0].1.channel,
                w[0].0.display(),
                w[1].0.display()
            ));
        }
    }
    Ok(inputs.into_iter().map(|(_, m)| m).collect())
}

/// --names 按 channel 升序一一对应；显式给名时数量/重复必须校验，不再静默退回 CHn。
fn resolve_role_names(
    meas: &[ChannelMeasurement],
    names: &[String],
) -> Result<Vec<String>, String> {
    if names.is_empty() {
        return Ok(meas.iter().map(|m| format!("CH{}", m.channel)).collect());
    }
    if names.len() != meas.len() {
        return Err(format!(
            "E_MERGE_NAMES_COUNT: --names 给了 {} 个名字，目录里有 {} 个非参考通道；--names 必须按 channel 升序一一对应",
            names.len(),
            meas.len()
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for n in names {
        if !seen.insert(n.to_ascii_uppercase()) {
            return Err(format!("E_MERGE_DUPLICATE_NAME: --names 中 {n} 重复"));
        }
    }
    Ok(names.to_vec())
}

/// 延迟质量门的固定阈值（样本；48 kHz 下 1200 样本≈25 ms，20 样本≈0.4 ms）。
const MERGE_DELAY_OUTLIER_TOL_SAMPLES: f64 = 1_200.0;
const MERGE_DELAY_RESID_TOL_SAMPLES: f64 = 20.0;
const MERGE_DELAY_DIFF_SPREAD_TOL_SAMPLES: f64 = 50.0;

/// 延迟质量门：逐遍一致性 + 跨通道离群。任一不合格 → 拒绝生成 merged.yml。
///
/// 依据（pilot FR-ch01 事故）：
/// - repeats.residuals 交替 ±86.9、accepted_diffs 偏离标称 ±174（其它通道 <4 / <2）；
/// - delay_samples 比其余 6 通道中位数偏 3857 样本（88 ms 级）；
/// 说明多遍起点被墙反射/旁瓣锁住。merge 若继续用 max−d_i 归一化会写出最大 88 ms
/// 的假延迟，因此必须拒绝，而不是静默产出“看起来正常”的校准。
fn delay_quality_gate(meas: &[ChannelMeasurement], names: &[String]) -> Result<(), String> {
    let name_of = |i: usize| -> String {
        names
            .get(i)
            .cloned()
            .unwrap_or_else(|| format!("CH{}", meas[i].channel))
    };
    let mut issues: Vec<String> = Vec::new();
    for (i, m) in meas.iter().enumerate() {
        let name = name_of(i);
        if !m.delay_samples.is_finite() {
            issues.push(format!(
                "ch{} {name}: E_DELAY_INVALID delay_samples={}",
                m.channel, m.delay_samples
            ));
            continue;
        }
        let Some(r) = &m.repeats else { continue };
        if r.insufficient_valid_passes {
            issues.push(format!(
                "ch{} {name}: E_DELAY_INSUFFICIENT_PASSES 有效重复起点 <2（count={}），延迟不可信",
                m.channel, r.count
            ));
        }
        if let Some(max_res) = r
            .residuals
            .iter()
            .map(|v| v.abs())
            .fold(None::<f64>, |acc, v| Some(acc.map_or(v, |a| a.max(v))))
        {
            if max_res > MERGE_DELAY_RESID_TOL_SAMPLES {
                issues.push(format!(
                    "ch{} {name}: E_DELAY_RESIDUAL 逐遍残差 max={max_res:.1} > {MERGE_DELAY_RESID_TOL_SAMPLES:.0} 样本（多遍起点不一致）",
                    m.channel
                ));
            }
        }
        if r.accepted_diffs.len() >= 2 {
            let mx = r
                .accepted_diffs
                .iter()
                .cloned()
                .fold(f64::NEG_INFINITY, f64::max);
            let mn = r
                .accepted_diffs
                .iter()
                .cloned()
                .fold(f64::INFINITY, f64::min);
            if mx - mn > MERGE_DELAY_DIFF_SPREAD_TOL_SAMPLES {
                issues.push(format!(
                    "ch{} {name}: E_DELAY_SPACING accepted 相邻差极差={:.1} > {MERGE_DELAY_DIFF_SPREAD_TOL_SAMPLES:.0} 样本（重复间隔不一致）",
                    m.channel,
                    mx - mn
                ));
            }
        }
    }
    // 跨通道离群：只有 >=3 个通道时中位数才有区分力（2 通道无法判断谁离群）。
    if meas.len() >= 3 {
        let ds: Vec<f64> = meas.iter().map(|m| m.delay_samples).collect();
        let med = dsp::median_f64(&ds);
        let max_delay = ds.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        for (i, m) in meas.iter().enumerate() {
            let dev = (m.delay_samples - med).abs();
            if dev > MERGE_DELAY_OUTLIER_TOL_SAMPLES {
                let max_other = ds
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != i)
                    .map(|(_, d)| *d)
                    .fold(f64::NEG_INFINITY, f64::max);
                let impact = if m.delay_samples >= max_other {
                    format!(
                        "若按 max−d_i 归一化，其余通道会被额外加 {:.1} 样本假延迟",
                        m.delay_samples - max_other
                    )
                } else {
                    format!(
                        "若按 max−d_i 归一化，该通道会被写成 {:.1} 样本延迟",
                        max_delay - m.delay_samples
                    )
                };
                issues.push(format!(
                    "ch{} {}: E_DELAY_OUTLIER delay={:.1} 样本，偏离通道中位数 {:.1} 样本（>{MERGE_DELAY_OUTLIER_TOL_SAMPLES:.0}）；{impact}",
                    m.channel,
                    name_of(i),
                    m.delay_samples,
                    dev
                ));
            }
        }
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "E_MERGE_DELAY_QUALITY: 延迟质量门不通过，拒绝生成 merged.yml（不写出错误校准）：{}",
            issues.join("; ")
        ))
    }
}

fn parse_pair_specs(raw: &str) -> Result<Vec<(String, String, f64)>, String> {
    let mut out = Vec::new();
    for spec in raw.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let parts: Vec<&str> = spec.split(':').collect();
        if parts.len() != 3 {
            return Err(format!("E_MERGE_PAIR_SPEC: pair {spec} 格式应为 A:B:fc"));
        }
        let fc: f64 = parts[2]
            .parse()
            .map_err(|_| format!("E_MERGE_PAIR_SPEC: pair {spec} 的 fc 不是数字"))?;
        if !(fc.is_finite() && fc > 0.0) {
            return Err(format!("E_MERGE_PAIR_SPEC: pair {spec} 的 fc 必须 >0"));
        }
        out.push((parts[0].to_string(), parts[1].to_string(), fc));
    }
    Ok(out)
}

fn find_channel_index(channels: &[ChannelCalibration], name: &str) -> Option<usize> {
    if let Some(i) = channels
        .iter()
        .position(|c| c.name.eq_ignore_ascii_case(name))
    {
        return Some(i);
    }
    let lower = name.to_ascii_lowercase();
    let num = lower.strip_prefix("ch").unwrap_or(&lower);
    num.parse::<usize>()
        .ok()
        .and_then(|n| channels.iter().position(|c| c.index == n))
}

/// 汇总判定（ACCEPTANCE §8/§9）：
/// - global advisory 不进 overall（C-P1-5）；
/// - 无 --pairs → INCOMPLETE（不静默 PASS，也不因为全局求和 FAIL 而把产物判 FAIL）；
/// - pairs FAIL → FAIL；pairs PASS 但 negative_controls 未跑/不足 → INCOMPLETE；5/5 detected → PASS。
fn merge_overall_status(
    clip_risk_any: bool,
    pairs_given: bool,
    all_pairs_pass: bool,
    neg_status: &str,
) -> &'static str {
    if clip_risk_any {
        "FAIL"
    } else if !pairs_given {
        "INCOMPLETE"
    } else if !all_pairs_pass {
        "FAIL"
    } else {
        match neg_status {
            "pass" => "PASS",
            "fail" => "FAIL",
            _ => "INCOMPLETE",
        }
    }
}

fn cmd_merge(a: &A) -> Result<(), String> {
    let dir = PathBuf::from(a.req("dir"));
    let out = PathBuf::from(a.req("out"));
    let crossovers: Vec<f64> = a
        .str("crossovers", "80,2000")
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    // pair 语法先解析（给了 --pairs 但格式错必须报错，不能静默当没给）。
    let pair_specs = parse_pair_specs(&a.str("pairs", ""))?;
    if a.get("pairs").is_some() && pair_specs.is_empty() {
        return Err(
            "E_MERGE_PAIR_SPEC: 给了 --pairs 但内容为空；要么省略，要么给 A:B:fc 列表".into(),
        );
    }
    let pairs_given = !pair_specs.is_empty();
    if !pairs_given {
        eprintln!(
            "WARN: 未提供 --pairs：T4 交叉对判据未运行；全局全通道求和只作 advisory 诊断，不参与 PASS/FAIL（ACCEPTANCE §6.1/C-P1-5）。本次 merge 不可能 PASS。"
        );
    }
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .map_err(|e| format!("read dir {}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map(|n| n.ends_with(".analysis.json"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(format!("{} 下没有 *.analysis.json", dir.display()));
    }
    let mut raw_inputs: Vec<(PathBuf, ChannelMeasurement)> = Vec::new();
    for f in &files {
        let m = read_measurement(f)?;
        if m.measurement_role.as_deref() == Some("reference") {
            continue;
        }
        raw_inputs.push((f.clone(), m));
    }
    if raw_inputs.is_empty() {
        return Err(format!(
            "{} 下没有可合并的 *.analysis.json（参考麦文件已跳过）",
            dir.display()
        ));
    }
    let meas = sort_measurements_by_channel(raw_inputs)?;
    let rate = meas[0].meta.sample_rate as f64;
    if meas
        .iter()
        .any(|m| m.meta.sample_rate != meas[0].meta.sample_rate)
    {
        return Err("E_MERGE_MIXED_RATE: 待合并 analysis 的 sample_rate 不一致，拒绝合并".into());
    }
    let names_arg: Vec<String> = a
        .get("names")
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let role_names = resolve_role_names(&meas, &names_arg)?;
    // 延迟质量门：在拟合/写产物前拒绝坏延迟，绝不静默写出错误校准。
    delay_quality_gate(&meas, &role_names)?;
    let allow_sub = a
        .get("allow-sub-polarity-from-listening-position")
        .is_some();
    let require_high = a.get("require-high-confidence").is_some();
    let mut channels = Vec::new();
    let mut reports: Vec<peq::OptimizeReport> = Vec::new();
    let mut position_sets: Vec<Vec<peq::WeightedFr>> = Vec::new();
    let delays: Vec<i64> = meas
        .iter()
        .map(|m| m.delay_samples.round() as i64)
        .collect();
    let max_delay = delays.iter().copied().max().unwrap_or(0);
    let mut reference_index = 0usize;
    let mut polarity_notes = Vec::new();
    for (i, m) in meas.iter().enumerate() {
        let name = role_names[i].clone();
        let (rep, positions) = optimize_measurement(m, &name, a)?;
        println!(
            "ch{}: {} stages rms={:.3}dB max={:.2}dB delay={:.3}ms polarity={:+}",
            m.channel,
            rep.filters.len(),
            rep.rms,
            rep.max_abs,
            m.delay_ms,
            m.polarity
        );
        reports.push(rep);
        position_sets.push(positions);
        let norm_delay = max_delay - delays[i];
        if norm_delay == 0 {
            reference_index = m.channel;
        }
        let pol = merge_polarity(m, &name, allow_sub, require_high)?;
        polarity_notes.push(serde_json::json!({
            "name": name,
            "polarity": pol,
            "confidence": m.polarity_confidence,
            "source": m.polarity_source,
            "basis": m.sub_polarity_basis,
            "listening": m.polarity_listening,
        }));
        channels.push(ChannelCalibration {
            index: m.channel,
            name,
            delay_samples: norm_delay,
            polarity: pol,
            filters: vec![],
            gain_db: None,
        });
    }
    apply_match_groups(a, &channels, &mut reports, &position_sets, rate)?;
    let fits = commit_stages(&meas, &channels, &reports, a, rate);
    for (ch, stages) in channels.iter_mut().zip(fits.iter()) {
        ch.filters = stages.clone();
    }
    let cal = Calibration {
        schema: "khaen-calibration/v1.1".into(),
        sample_rate: rate as u32,
        reference_index,
        layout: a.get("layout").map(|s| s.to_string()),
        created_unix: Some(now_unix()),
        tool: Some("khaen-calibrate".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        note: "merged multi-channel calibration".into(),
        channels,
    };
    let named: Vec<(&str, &peq::OptimizeReport)> = cal
        .channels
        .iter()
        .zip(reports.iter())
        .map(|(c, r)| (c.name.as_str(), r))
        .collect();
    let cal_delays: Vec<i64> = cal.channels.iter().map(|c| c.delay_samples).collect();
    let cal_pols: Vec<i32> = cal
        .channels
        .iter()
        .map(|c| {
            if c.polarity.as_deref() == Some("invert") {
                -1
            } else {
                1
            }
        })
        .collect();
    // 全局全通道求和：80–20k + 分频点。只作 advisory 诊断（ACCEPTANCE §6.1/C-P1-5），
    // 不是 T4 判据，不参与 overall_status/overall_pass。
    let f_lo = 80.0f64;
    let f_max = (rate * 0.45).min(20_000.0);
    let mut checks = Vec::new();
    let mut worst = (0.0f64, f64::INFINITY);
    let mut freqs: Vec<f64> = Vec::new();
    let mut f = f_lo;
    while f <= f_max {
        freqs.push(f);
        f *= 2.0f64.powf(1.0 / 24.0);
    }
    freqs.extend(
        crossovers
            .iter()
            .copied()
            .filter(|c| *c >= f_lo && *c <= f_max),
    );
    freqs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    freqs.dedup_by(|a, b| (*a - *b).abs() < 0.5);
    for f in freqs {
        let (sum_db, single_db) = phase_sum_margin(&meas, &fits, &cal_delays, &cal_pols, f, rate);
        let margin = sum_db - single_db;
        checks.push(ValidationCheck {
            freq_hz: f,
            sum_db,
            max_single_db: single_db,
            margin_db: margin,
            status: if margin < -6.0 {
                "FAIL".into()
            } else if margin < -3.0 {
                "WARN".into()
            } else {
                "PASS".into()
            },
        });
        if margin < worst.1 {
            worst = (f, margin);
        }
    }
    let advisory_fail_checks = checks.iter().filter(|c| c.status == "FAIL").count();
    let advisory_warn_checks = checks.iter().filter(|c| c.status == "WARN").count();
    let advisory_global_passed = advisory_fail_checks == 0;
    // T4 门控：给了 --pairs 就只由交叉对判据决定 passed；全局结果只写诊断。
    let mut pairs_out: Vec<PairResult> = Vec::new();
    for (an, bn, fc) in &pair_specs {
        let ai = find_channel_index(&cal.channels, an).ok_or_else(|| {
            format!("E_MERGE_PAIR_UNKNOWN_CHANNEL: pair {an}:{bn}:{fc} 找不到通道 {an}")
        })?;
        let bi = find_channel_index(&cal.channels, bn).ok_or_else(|| {
            format!("E_MERGE_PAIR_UNKNOWN_CHANNEL: pair {an}:{bn}:{fc} 找不到通道 {bn}")
        })?;
        if ai == bi {
            return Err(format!(
                "E_MERGE_PAIR_SPEC: pair {an}:{bn}:{fc} 两路是同一个通道"
            ));
        }
        // ACCEPTANCE §6.3：pair 的 fc 必须是 profile 里声明的 HP/LP 频率。
        if !crossovers.is_empty()
            && !crossovers
                .iter()
                .any(|c| (*c - *fc).abs() <= (*fc * 0.01).max(0.5))
        {
            return Err(format!(
                "E_MERGE_PAIR_FC_MISMATCH: pair {an}:{bn}:{fc} 的 fc 不在 --crossovers {crossovers:?} 内（ACCEPTANCE §6.3：pair fc 必须是 profile 的 HP/LP 频率）"
            ));
        }
        pairs_out.push(check_pair(
            &meas,
            &fits,
            &cal_delays,
            &cal_pols,
            ai,
            bi,
            *fc,
            rate,
        ));
    }
    let all_pairs_pass = pairs_out.iter().all(|p| p.pass);
    let pair_gate = if !pairs_given {
        "not_run"
    } else if all_pairs_pass {
        "pass"
    } else {
        "fail"
    };
    // negative_controls：ACCEPTANCE §8/§9 要求 N1–N5 5/5 detected 才可能 PASS。
    let negative_controls: serde_json::Value = match a.get("negative-controls") {
        Some(path) => {
            let bytes =
                fs::read(path).map_err(|e| format!("read negative-controls {path}: {e}"))?;
            serde_json::from_slice(&bytes)
                .map_err(|e| format!("parse negative-controls {path}: {e}"))?
        }
        None => serde_json::json!([]),
    };
    let neg_items = negative_controls.as_array().cloned().unwrap_or_default();
    let neg_required = 5usize;
    let neg_detected = neg_items
        .iter()
        .filter(|v| v.get("detected").and_then(|d| d.as_bool()).unwrap_or(false))
        .count();
    let neg_status = if neg_items.is_empty() {
        "not_run"
    } else if neg_items.len() < neg_required {
        "incomplete"
    } else if neg_detected == neg_items.len() {
        "pass"
    } else {
        "fail"
    };
    // 写产物（pair FAIL 仍是完整判定，照常写 calibration + 报告；只有输入/质量门问题才拒绝）。
    fs::write(
        &out,
        serde_yaml::to_string(&cal).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write {}: {e}", out.display()))?;
    fs::write(
        format!("{}.camilladsp.yml", out.display()),
        serde_yaml::to_string(&cal_to_camilladsp(&cal)).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write fragment: {e}"))?;
    write_prediction(
        &out,
        &named,
        position_sets.first().map(|v| v.as_slice()).unwrap_or(&[]),
    )?;
    let cal_bytes = fs::read(&out).map_err(|e| format!("read {}: {e}", out.display()))?;
    let cal_sha = format!("{:x}", Sha256::digest(&cal_bytes));
    // validation.json：保留既有 schema/字段，新增 status/gate/advisory 说明。
    let validation = ValidationReport {
        schema: "khaen-calibration-validation/v1".into(),
        passed: pairs_given && all_pairs_pass,
        f_min_hz: f_lo,
        f_max_hz: f_max,
        crossovers: crossovers.clone(),
        worst_freq_hz: worst.0,
        worst_margin_db: worst.1,
        checks,
    };
    let mut validation_value = serde_json::to_value(&validation).map_err(|e| e.to_string())?;
    validation_value["status"] = serde_json::json!(if !pairs_given {
        "NOT_RUN"
    } else if all_pairs_pass {
        "PASS"
    } else {
        "FAIL"
    });
    validation_value["gate"] = serde_json::json!("pairs");
    validation_value["advisory_only"] = serde_json::json!(!pairs_given);
    validation_value["advisory_global"] = serde_json::json!({
        "passed": advisory_global_passed,
        "fail_checks": advisory_fail_checks,
        "warn_checks": advisory_warn_checks,
        "note": "全局全通道求和只作诊断；不参与 PASS/FAIL（ACCEPTANCE §6.1/C-P1-5）"
    });
    validation_value["reason"] = serde_json::json!(if pairs_given {
        "T4 passed 只由 --pairs 交叉对判据决定；全局全通道求和只是 advisory 诊断"
    } else {
        "未提供 --pairs：T4 交叉对判据未运行，全局全通道求和不是合法判据；本次不得据此判 PASS 或 FAIL"
    });
    fs::write(
        format!("{}.validation.json", out.display()),
        serde_json::to_string_pretty(&validation_value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write validation: {e}"))?;
    if pairs_given {
        fs::write(
            format!("{}.pairs.json", out.display()),
            serde_json::to_string_pretty(&pairs_out).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("write pairs: {e}"))?;
        for p in &pairs_out {
            println!(
                "pair {}@{}Hz: {} dphi={:.1}° CD(fc)={:.2}dB minCD={:.2}dB notch={:.2}dB ripple={:.2}dB",
                if p.pass { "PASS" } else { "FAIL" },
                p.fc_hz,
                if p.pass { "ok".to_string() } else { p.reasons.join("; ") },
                p.dphi_fc_deg,
                p.cd_fc_db,
                p.min_cd_db,
                p.notch_margin_db,
                p.ripple_db
            );
        }
    }
    let chunksize = a.usize("chunksize", 0);
    let clip_risk_any = reports.iter().any(|r| r.clip_risk);
    let overall_status =
        merge_overall_status(clip_risk_any, pairs_given, all_pairs_pass, neg_status);
    let overall_pass = overall_status == "PASS";
    if clip_risk_any {
        eprintln!("WARN: D7 boost/headroom 检查存在 clip_risk（详见 report.json boost_headroom）");
    }
    let boost_headroom: Vec<serde_json::Value> = reports
        .iter()
        .enumerate()
        .map(|(i, rep)| {
            serde_json::json!({
                "channel": role_names[i],
                "boost_filters": rep.boost_filters,
                "applied_boost_db": rep.applied_boost_db,
                "total_boost_db": rep.total_boost_db,
                "max_predicted_gain_db": rep.max_predicted_gain_db,
                "predicted_peak_dbfs": rep.predicted_peak_dbfs,
                "headroom_db": rep.headroom_db,
                "clip_risk": rep.clip_risk,
                "headroom_reference_dbfs": rep.headroom_reference_dbfs,
            })
        })
        .collect();
    let global_validation_json = serde_json::json!({
        "status": if pairs_given { "ADVISORY" } else { "NOT_RUN" },
        "used_as_gate": false,
        "passed": advisory_global_passed,
        "worst_freq_hz": worst.0,
        "worst_margin_db": worst.1,
        "fail_checks": advisory_fail_checks,
        "warn_checks": advisory_warn_checks,
        "reason": "全局全通道求和只作 advisory 诊断，不参与 overall_status/overall_pass（ACCEPTANCE §6.1/C-P1-5）",
    });
    let report = serde_json::json!({
        "schema": "khaen-calibration-report/v1",
        "tool": {"name": "khaen-calibrate", "version": env!("CARGO_PKG_VERSION")},
        "created_unix": now_unix(),
        "calibration_file": {"path": out.display().to_string(), "sha256": cal_sha},
        "layout": cal.layout, "sample_rate": cal.sample_rate,
        "environment": {
            "playback": meas[0].meta.playback_device, "capture": meas[0].meta.capture_device,
            "playback_channels": meas[0].meta.playback_channels, "capture_channels": meas[0].meta.capture_channels,
            "chunksize": chunksize
        },
        "global_validation": global_validation_json,
        "pair_validation": {"status": pair_gate, "gate": true, "pairs": pairs_out, "all_pairs_pass": all_pairs_pass},
        "negative_controls": {
            "status": neg_status,
            "required": neg_required,
            "count": neg_items.len(),
            "detected_count": neg_detected,
            "detected_all": neg_items.len() >= neg_required && neg_detected == neg_items.len(),
            "items": negative_controls,
        },
        "polarity_decisions": polarity_notes,
        "boost_cap_db": a.f64("boost-cap-db", 3.0).clamp(0.0, 3.0),
        "boost_headroom": boost_headroom,
        "clip_risk_any": clip_risk_any,
        "overall_status": overall_status,
        "overall_pass": overall_pass,
    });
    fs::write(
        format!("{}.report.json", out.display()),
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write report: {e}"))?;
    println!(
        "merged {} channels -> {} ; overall_status={} (pair gate: {}; global advisory worst {:.2} dB @ {:.1} Hz, NOT a gate)",
        meas.len(),
        out.display(),
        overall_status,
        pair_gate,
        worst.1,
        worst.0
    );
    Ok(())
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn frac_delay(x: &[f64], delay: f64) -> Vec<f64> {
    let n = x.len();
    let mut out = vec![0.0f64; n];
    let d = delay.max(0.0);
    let i0 = d.floor() as usize;
    let frac = d - i0 as f64;
    for i in 0..n {
        if i + i0 + 1 >= n {
            break;
        }
        // 线性插值足够用于离线示例（真实测量用相位斜率）
        out[i + i0] += x[i] * (1.0 - frac);
        out[i + i0 + 1] += x[i] * frac;
    }
    out
}

fn write_analysis(meas: &ChannelMeasurement, prefix: &Path) -> Result<(), String> {
    write_fr_csv(
        &PathBuf::from(format!("{}.fr.csv", prefix.display())),
        &meas.fr,
    )?;
    if !meas.fr_modal.is_empty() {
        write_fr_csv(
            &PathBuf::from(format!("{}.fr_modal.csv", prefix.display())),
            &meas.fr_modal,
        )?;
    }
    fs::write(
        format!("{}.analysis.json", prefix.display()),
        serde_json::to_string_pretty(meas).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write analysis: {e}"))?;
    Ok(())
}

/// 离线合成测量：已知延迟/极性/简单房间增益，生成 capture 并跑分析，供无硬件测试。
fn cmd_simulate(a: &A) -> Result<(), String> {
    let sp = SweepParams::from(a);
    let dir = PathBuf::from(a.req("dir"));
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let channels = a.usize("channels", 4).max(1);
    let delays: Vec<f64> = a
        .str("delays", "0,3.5,10,20")
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    let pols: Vec<i32> = a
        .str("polarities", "1,1,1,1")
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    let names: Vec<String> = a
        .str("names", "FL,FR,SL,SR")
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    let source = sp.source();
    for ch in 0..channels {
        let d = *delays.get(ch).unwrap_or(&0.0);
        let pol = *pols.get(ch).unwrap_or(&1);
        let mut cap = frac_delay(&source, d);
        for v in cap.iter_mut() {
            *v *= pol as f64;
        }
        let pre = (sp.pre * sp.rate as f64).round() as usize;
        let mut padded = vec![0.0f64; pre];
        padded.extend_from_slice(&cap);
        padded.extend(std::iter::repeat(0.0).take((sp.post * sp.rate as f64) as usize + 2400));
        let cap_path = dir.join(format!("cap-ch{ch:02}.raw"));
        raw::write_s24_single(&cap_path, &padded)?;
        let r = dsp::analyze_capture(&padded, &source, sp.rate, sp.start_hz, sp.end_hz, pre, None)?;
        let name = names.get(ch).cloned().unwrap_or_else(|| format!("ch{ch}"));
        let mut meas = base_measurement(
            ch,
            r.delay_samples,
            r.delay_coarse_samples,
            r.polarity,
            r.peak_db,
            r.phase_1k_deg,
            r.snr_db,
            r.fr.clone(),
            meta(
                &sp,
                "simulated",
                channels,
                &cap_path.display().to_string(),
                1,
            ),
        );
        meas.fr_modal = r.fr_modal.clone();
        meas.polarity_confidence = Some(r.polarity_confidence.clone());
        meas.polarity_estimators = Some(r.estimators.clone());
        meas.polarity_robust = Some(r.robust_polarity);
        meas.polarity_listening = Some(r.polarity_listening);
        meas.polarity_source = Some("legacy-ir-peak".into());
        write_analysis(&meas, &dir.join(format!("{}-ch{ch:02}", name)))?;
        println!(
            "sim ch{} {}: delay={:.3} samples, polarity={:+}",
            ch, name, r.delay_samples, r.polarity
        );
    }
    println!("simulated {} channels under {}", channels, dir.display());
    Ok(())
}

#[derive(Clone, Debug, serde::Serialize)]
struct PairResult {
    a: String,
    b: String,
    fc_hz: f64,
    dphi_fc_deg: f64,
    cd_fc_db: f64,
    min_cd_db: f64,
    notch_margin_db: f64,
    ripple_db: f64,
    target_dev_db: f64,
    level_diff_db: f64,
    pass: bool,
    reasons: Vec<String>,
}

fn interp_fr(fr: &[FrPoint], f: f64) -> (f64, f64) {
    if fr.is_empty() {
        return (0.0, 0.0);
    }
    if f <= fr[0].freq_hz {
        return (fr[0].db, fr[0].phase_deg);
    }
    for w in fr.windows(2) {
        if w[0].freq_hz <= f && f <= w[1].freq_hz {
            let t = (f - w[0].freq_hz) / (w[1].freq_hz - w[0].freq_hz).max(1e-12);
            return (
                w[0].db + t * (w[1].db - w[0].db),
                w[0].phase_deg + t * (w[1].phase_deg - w[0].phase_deg),
            );
        }
    }
    let last = fr.last().unwrap();
    (last.db, last.phase_deg)
}

fn channel_h(
    meas: &ChannelMeasurement,
    filt: &[Peq],
    delay_samples: &[i64],
    i: usize,
    f: f64,
    rate: f64,
    pol: i32,
) -> (f64, f64) {
    let (db, ph) = interp_fr(&meas.fr, f);
    let bs: Vec<peq::Biquad> = filt.iter().map(peq::Biquad::from_peq).collect();
    let h = peq::combined_h(&bs, f, rate);
    let db = db + 20.0 * h.norm().max(1e-12).log10();
    let total_delay = meas.delay_samples + *delay_samples.get(i).unwrap_or(&0) as f64;
    let phase = ph.to_radians() + h.arg() - 2.0 * std::f64::consts::PI * f * total_delay / rate;
    let mag = 10.0f64.powf(db / 20.0);
    (
        mag * phase.cos() * pol as f64,
        mag * phase.sin() * pol as f64,
    )
}

fn wrap_deg(mut d: f64) -> f64 {
    while d > 180.0 {
        d -= 360.0;
    }
    while d <= -180.0 {
        d += 360.0;
    }
    d
}

/// ACCEPTANCE §6.1：交叉对判据（Δφ(fc)、CD(fc)、minCD、陷波、纹波）
fn check_pair(
    meas: &[ChannelMeasurement],
    filt: &[Vec<Peq>],
    delays: &[i64],
    pols: &[i32],
    ai: usize,
    bi: usize,
    fc: f64,
    rate: f64,
) -> PairResult {
    let (mut dphi_fc, mut cd_fc, mut min_cd, mut notch_margin) =
        (0.0f64, 0.0f64, f64::INFINITY, f64::INFINITY);
    let mut s_db: Vec<f64> = Vec::new();
    let mut s_f: Vec<f64> = Vec::new();
    let mut max_h1: f64 = 0.0;
    let mut max_h2: f64 = 0.0;
    let mut h1_fc_db = 0.0f64;
    let mut h2_fc_db = 0.0f64;
    let f_lo = fc * 0.5;
    let f_hi = fc * 2.0;
    let mut f = f_lo;
    while f <= f_hi + 1e-9 {
        let (a_re, a_im) = channel_h(
            &meas[ai],
            &filt[ai],
            delays,
            ai,
            f,
            rate,
            *pols.get(ai).unwrap_or(&1),
        );
        let (b_re, b_im) = channel_h(
            &meas[bi],
            &filt[bi],
            delays,
            bi,
            f,
            rate,
            *pols.get(bi).unwrap_or(&1),
        );
        let ha = (a_re * a_re + a_im * a_im).sqrt();
        let hb = (b_re * b_re + b_im * b_im).sqrt();
        let s = ((a_re + b_re).powi(2) + (a_im + b_im).powi(2)).sqrt();
        let cd = 20.0 * s.max(1e-12).log10() - 20.0 * (ha + hb).max(1e-12).log10();
        min_cd = min_cd.min(cd);
        notch_margin = notch_margin.min(20.0 * s.max(1e-12).log10());
        if (f - fc).abs() < (fc / 24.0).max(1.0) {
            let d = wrap_deg((a_im.atan2(a_re) - b_im.atan2(b_re)).to_degrees());
            dphi_fc = d;
            cd_fc = cd;
            h1_fc_db = 20.0 * ha.max(1e-12).log10();
            h2_fc_db = 20.0 * hb.max(1e-12).log10();
        }
        s_db.push(20.0 * s.max(1e-12).log10());
        s_f.push(f);
        max_h1 = max_h1.max(ha);
        max_h2 = max_h2.max(hb);
        f *= 2.0f64.powf(1.0 / 24.0);
    }
    // 1/3 oct 频率窗平滑后的纹波（每点平均 ±1/6 oct 内的点）
    let smooth: Vec<f64> = (0..s_db.len())
        .map(|i| {
            let f = s_f[i];
            let lo_f = f * 2.0f64.powf(-1.0 / 6.0);
            let hi_f = f * 2.0f64.powf(1.0 / 6.0);
            let mut sum = 0.0;
            let mut n = 0usize;
            for j in 0..s_db.len() {
                if s_f[j] >= lo_f && s_f[j] <= hi_f {
                    sum += s_db[j];
                    n += 1;
                }
            }
            sum / n.max(1) as f64
        })
        .collect();
    let ripple = smooth.iter().cloned().fold(f64::MIN, f64::max)
        - smooth.iter().cloned().fold(f64::MAX, f64::min);
    let notch_db = notch_margin - 20.0 * max_h1.max(max_h2).max(1e-12).log10();
    let mean_s = s_db.iter().sum::<f64>() / s_db.len().max(1) as f64;
    let target_dev = s_db.iter().map(|v| (v - mean_s).abs()).fold(0.0, f64::max);
    let level_diff = (h1_fc_db - h2_fc_db).abs();
    let mut reasons = Vec::new();
    if dphi_fc.abs() > 30.0 {
        reasons.push(format!("Δφ(fc)={:.1}°>30°", dphi_fc));
    }
    if cd_fc < -3.0 {
        reasons.push(format!("CD(fc)={:.2}dB<-3dB", cd_fc));
    }
    if min_cd < -6.0 {
        reasons.push(format!("minCD={:.2}dB<-6dB", min_cd));
    }
    if notch_db < -6.0 {
        reasons.push(format!("notch={:.2}dB<-6dB", notch_db));
    }
    if ripple > 3.0 {
        reasons.push(format!("ripple={:.2}dB>3dB", ripple));
    }
    if target_dev > 3.0 {
        reasons.push(format!("target_dev={:.2}dB>3dB", target_dev));
    }
    if level_diff > 3.0 {
        reasons.push(format!("level_diff(fc)={:.2}dB>3dB", level_diff));
    }
    PairResult {
        a: format!("ch{}", meas[ai].channel),
        b: format!("ch{}", meas[bi].channel),
        fc_hz: fc,
        dphi_fc_deg: dphi_fc,
        cd_fc_db: cd_fc,
        min_cd_db: min_cd,
        notch_margin_db: notch_db,
        ripple_db: ripple,
        target_dev_db: target_dev,
        level_diff_db: level_diff,
        pass: reasons.is_empty(),
        reasons,
    }
}

fn with_cuemix<T>(a: &A, body: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let armed = if let Some(base) = a.get("cuemix-guard") {
        let token = a.get("cuemix-token").ok_or("缺少 --cuemix-token")?;
        raw::cuemix_direct_mute(base, token, true)?;
        println!("CueMix direct-mute: mute=true");
        Some((base.to_string(), token.to_string()))
    } else {
        None
    };
    let result = body();
    if let Some((base, token)) = armed {
        if let Err(e) = raw::cuemix_direct_mute(&base, &token, false) {
            eprintln!("ERROR: CueMix 恢复直通失败: {e}");
            if result.is_ok() {
                return Err(e);
            }
        } else {
            println!("CueMix direct-mute: mute=false");
        }
    }
    result
}

fn write_repeat_csv(path: &Path, d: &RepeatDetail) -> Result<(), String> {
    let mut s = String::from("repeat,delay_samples,polarity_vote,ir_peak_idx,snr_db,confidence,onset_invalid,ir_peak,direct_window,phase_vote,lowband,matched\n");
    for i in 0..d.polarity_votes.len().max(d.delay_samples.len()) {
        let est = d.per_repeat_estimators.get(i);
        let inv = d.onset_invalid.get(i).copied().unwrap_or(false);
        s.push_str(&format!(
            "{i},{:.4},{},{},{:.3},{},{inv},{},{},{},{},{}\n",
            d.delay_samples.get(i).copied().unwrap_or(0.0),
            d.polarity_votes.get(i).copied().unwrap_or(0),
            d.ir_peak_idx.get(i).copied().unwrap_or(0),
            d.snr_db.get(i).copied().unwrap_or(0.0),
            d.per_repeat_confidence
                .get(i)
                .map(|c| c.as_str())
                .unwrap_or(""),
            est.map(|e| e.ir_peak).unwrap_or(0),
            est.map(|e| e.direct_window).unwrap_or(0),
            est.map(|e| e.phase_vote).unwrap_or(0),
            est.map(|e| e.lowband).unwrap_or(0),
            est.map(|e| e.matched).unwrap_or(0),
        ));
    }
    fs::write(path, s).map_err(|e| format!("write {}: {e}", path.display()))
}

fn is_sub_name(name: &str, end_hz: f64) -> bool {
    name.to_ascii_uppercase().starts_with("SUB") || end_hz <= 200.0
}

fn merge_polarity(
    m: &ChannelMeasurement,
    name: &str,
    allow_sub: bool,
    require_high: bool,
) -> Result<Option<String>, String> {
    if require_high {
        if let Some(c) = m.polarity_confidence.as_deref() {
            if c != "high" {
                return Err(format!(
                    "{name} polarity_confidence={c}，--require-high-confidence 拒绝写入"
                ));
            }
        }
    }
    let sub = is_sub_name(name, m.meta.sweep_end_hz);
    if sub && m.polarity_confidence.as_deref() == Some("unknown") && !allow_sub {
        eprintln!("WARN: {name} 只有听音位数据，SUB 极性随位置反相，校准文件不写 polarity。近场参考或 --allow-sub-polarity-from-listening-position 才能写入。依据：{}",
            m.sub_polarity_basis.as_deref().unwrap_or("listening-position only"));
        return Ok(None);
    }
    let pol = if sub && allow_sub {
        m.polarity_listening
            .filter(|p| *p != 0)
            .unwrap_or(m.polarity)
    } else {
        m.polarity
    };
    if pol == 0 {
        eprintln!("WARN: {name} 极性未知，不写入 polarity");
        return Ok(None);
    }
    Ok(Some(if pol < 0 {
        "invert".into()
    } else {
        "normal".into()
    }))
}

fn cmd_measure_multi(a: &A) -> Result<(), String> {
    let sp = SweepParams::from(a);
    let pbc = a.usize("playback-channels", 22).max(1);
    let capc = a.usize("capture-channels", 2).max(1);
    let pb = a.str("playback", "hw:4,0");
    let cap = a.str("capture", "hw:3,0");
    // T1 抽检：--loopback-map 9:7（1-based，out9→in7，可多对）；--loopback-channels 1,2 为 1:1 简写
    let lmap = a.str("loopback-map", "");
    let lc = a.str("loopback-channels", "");
    if !lmap.trim().is_empty() && !lc.trim().is_empty() {
        return Err("--loopback-map 与 --loopback-channels 只能给一个（map 用于非 1:1，channels 是 1:1 简写）".into());
    }
    let loopback_mode = !lmap.trim().is_empty() || !lc.trim().is_empty();
    let mut loop_pairs: Vec<(usize, usize)> = Vec::new();
    if !lmap.trim().is_empty() {
        for spec in lmap.split(',').filter(|s| !s.trim().is_empty()) {
            let (p, c) = spec
                .trim()
                .split_once(':')
                .ok_or_else(|| format!("--loopback-map 格式应为 play:capture，坏值 {spec}"))?;
            let p: usize = p
                .parse()
                .map_err(|_| format!("--loopback-map 播放通道非法 {spec}"))?;
            let c: usize = c
                .parse()
                .map_err(|_| format!("--loopback-map 采集通道非法 {spec}"))?;
            if p == 0 || c == 0 {
                return Err("--loopback-map 是 1-based（9:7 = out9→in7），不能含 0".into());
            }
            loop_pairs.push((p - 1, c - 1));
        }
    } else if !lc.trim().is_empty() {
        for s in lc.split(',').filter(|s| !s.trim().is_empty()) {
            let n: usize = s
                .trim()
                .parse()
                .map_err(|_| format!("--loopback-channels 含非法值 {s}"))?;
            if n == 0 {
                return Err("--loopback-channels 是 1-based（1=out1/in1），不能含 0".into());
            }
            loop_pairs.push((n - 1, n - 1));
        }
    }
    let chans: Vec<usize> = if loopback_mode {
        loop_pairs.iter().map(|p| p.0).collect()
    } else {
        a.req("channels")
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect()
    };
    if chans.is_empty() {
        return Err("--channels / --loopback-map 为空".into());
    }
    if loopback_mode && capc < loop_pairs.iter().map(|p| p.1).max().unwrap_or(0) + 1 {
        return Err(format!(
            "loopback 模式需 capture-channels >= {}（当前 {capc}）",
            loop_pairs.iter().map(|p| p.1).max().unwrap_or(0) + 1
        ));
    }
    // 采集前就拦截，避免板端白跑一次 10–15s arecord
    let verify_map = a.get("verify-mapping").is_some();
    if loopback_mode && !verify_map {
        return Err("--loopback-map/--loopback-channels 必须同时带 --verify-mapping（否则映射/隔离不校验，T1 结果无效）".into());
    }
    let gap = a.f64("gap-seconds", 0.8);
    let repeats = a.usize("repeats", 1);
    if repeats == 0 || repeats > 8 {
        return Err(format!("--repeats {repeats} 超出 1..=8"));
    }
    let repeat_gap_s = a.f64("repeat-gap", 0.2);
    let robust_on = repeats > 1 || a.get("robust-polarity").is_some();
    let drift_correct = a.get("drift-correct").is_some();
    let max_ppm = a.f64("max-ppm", 100.0);
    let nearfield = a.get("sub-reference-nearfield").is_some();
    let pre = sp.pre;
    let post = sp.post;
    let sweep_len = sp.source().len();
    // repeats=1 时不插入 repeat-gap，slot 与旧版一致。
    let repeat_gap_n = if repeats > 1 {
        (repeat_gap_s * sp.rate as f64).round() as usize
    } else {
        0
    };
    let interval = sweep_len + repeat_gap_n;
    let slot_gap = (gap * sp.rate as f64) as usize;
    let slot = if repeats == 1 {
        sweep_len + slot_gap
    } else {
        repeats * interval + slot_gap
    };
    let total = (pre * sp.rate as f64) as usize
        + chans.len() * slot
        + (post * sp.rate as f64) as usize
        + sp.rate as usize * 2;
    // --band-limit 7:130[,ch:hz...]：指定通道改用带限扫频（保护 LFE 等不能收全频的单元）
    // --level-override 7:-30[,ch:db...]：指定通道单独设电平
    let chan_num = |k: &str| -> Vec<(usize, f64)> {
        a.str(k, "")
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .filter_map(|spec| {
                let (c, v) = spec.trim().split_once(':')?;
                Some((
                    c.trim().parse::<usize>().ok()?,
                    v.trim().parse::<f64>().ok()?,
                ))
            })
            .collect()
    };
    // --band-limit ch:end_hz[:start_hz]（简写 ch:end_hz 时 start 用全局 --start-hz）
    let band_limits: Vec<(usize, f64, Option<f64>)> = a
        .str("band-limit", "")
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .filter_map(|spec| {
            let parts: Vec<&str> = spec.trim().split(':').collect();
            if parts.len() < 2 {
                return None;
            }
            let ch = parts[0].trim().parse::<usize>().ok()?;
            let end = parts[1].trim().parse::<f64>().ok()?;
            let start = if parts.len() >= 3 {
                parts[2].trim().parse::<f64>().ok()
            } else {
                None
            };
            Some((ch, end, start))
        })
        .collect();
    let level_overrides = chan_num("level-override");
    let src_for = |ch: usize| -> (Vec<f64>, f64) {
        let mut s2 = sp.clone();
        if let Some((_, hz, start_hz)) = band_limits.iter().find(|(c, _, _)| *c == ch) {
            s2.end_hz = hz.min(sp.end_hz).max(sp.start_hz * 1.01);
            if let Some(st) = start_hz {
                s2.start_hz = (*st).min(s2.end_hz * 0.99).max(1.0);
            }
        }
        if let Some((_, db)) = level_overrides.iter().find(|(c, _)| *c == ch) {
            s2.level_db = *db;
        }
        (s2.source(), s2.end_hz)
    };
    let capture2_dev = a.get("capture2").map(|s| s.to_string());
    let capture2_offline = a.get("capture2-file").map(PathBuf::from);
    let capture2_channels = a.usize("capture2-channels", 20).max(1);
    let capture2_index = a.usize("capture2-channel", 0);
    let capture2_name = a.str("capture2-name", "REF");
    let have_capture2 = capture2_dev.is_some() || capture2_offline.is_some();
    // 双设备时两路 arecord 需要更长的稳定时间。用户显式传了偏移就照用户的。
    let playback_offset_seconds = if a.get("playback-offset-seconds").is_some() {
        a.f64("playback-offset-seconds", 0.35)
    } else if have_capture2 {
        1.0
    } else {
        0.35
    };
    let settle_ms = (playback_offset_seconds * 1000.0).round().max(1.0) as u64;
    if have_capture2 && capture2_index >= capture2_channels {
        return Err(format!(
            "--capture2-channel {capture2_index} 超出 --capture2-channels {capture2_channels}"
        ));
    }
    let offline_capture = a.get("capture-file").map(PathBuf::from);
    let dir = PathBuf::from(a.req("out-dir"));
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    let sweep_path = dir.join("composite.raw");
    let capture_path = offline_capture
        .clone()
        .unwrap_or_else(|| dir.join("capture.raw"));
    let capture2_path = capture2_offline
        .clone()
        .unwrap_or_else(|| dir.join("capture2.raw"));
    let cap_seconds = total as f64 / sp.rate as f64 + 1.5;
    let period = a.usize("period-size", 128);
    let buffer = a.usize("buffer-size", 512);
    if !(32..=4096).contains(&period) || buffer < period {
        return Err(format!("period={period} buffer={buffer} 不合法"));
    }
    if a.str("mic", "") == "umik" && sp.rate != 48000 {
        return Err("UMIK-1 仅支持 48 kHz；96k 需换麦克风/漂移补偿（未实现）".into());
    }
    if offline_capture.is_none() {
        let mut chans_data = vec![vec![0.0f64; total]; pbc];
        for (i, ch) in chans.iter().enumerate() {
            if *ch >= pbc {
                return Err(format!("channel {ch} >= playback channels {pbc}"));
            }
            let (src, _) = src_for(*ch);
            for k in 0..repeats {
                let off = (pre * sp.rate as f64) as usize + i * slot + k * interval;
                let end = off + src.len();
                if end > chans_data[*ch].len() {
                    return Err(format!("内部错误：扫频超出 composite 长度 ch{ch} rep{k}"));
                }
                chans_data[*ch][off..end].copy_from_slice(&src);
            }
        }
        raw::write_s24(&sweep_path, chans_data)?;
        println!("measure-multi: {} channels x {repeats} repeats, one arecord session, period={period} buffer={buffer}, total={:.1}s", chans.len(), total as f64 / sp.rate as f64);
        let run = || -> Result<(), String> {
            if let Some(dev2) = &capture2_dev {
                raw::measure_channel_dual(
                    &pb,
                    pbc,
                    &cap,
                    capc,
                    dev2,
                    capture2_channels,
                    sp.rate,
                    cap_seconds,
                    &sweep_path,
                    &capture_path,
                    &capture2_path,
                    period,
                    buffer,
                    settle_ms,
                )
            } else {
                raw::measure_channel(
                    &pb,
                    pbc,
                    &cap,
                    capc,
                    sp.rate,
                    cap_seconds,
                    &sweep_path,
                    &capture_path,
                    period,
                    buffer,
                )
            }
        };
        with_cuemix(a, run)?;
    } else if capture2_dev.is_some() && capture2_offline.is_none() {
        return Err("离线 --capture-file 不能同时现场采 --capture2；请提供 --capture2-file".into());
    } else {
        println!(
            "measure-multi: offline capture {}, {} channels x {repeats} repeats",
            capture_path.display(),
            chans.len()
        );
    }
    let caps = raw::read_s24(&capture_path, capc)?;
    let caps2 = if have_capture2 {
        Some(raw::read_s24(&capture2_path, capture2_channels)?)
    } else {
        None
    };
    let cap_idx = a.usize("capture-channel", 0).min(capc - 1);
    let _prefix = PathBuf::from(a.req("out-prefix"));
    let names: Vec<String> = a
        .str("names", "")
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    let pol_off = if a.str("polarity-offset", "normal") == "invert" {
        -1
    } else {
        1
    };
    let dev_off = (playback_offset_seconds * sp.rate as f64) as usize;
    let mut slot_onsets: Vec<f64> = Vec::new();
    for (i, ch) in chans.iter().enumerate() {
        // 槽位名义起点：pre + i*slot + arecord 先起偏移。双设备默认 1.0s，单设备 0.35s。
        // loopback 模式：按 loopback-map 从对应采集通道取信号；普通模式固定 --capture-channel
        let cap_for_slot = if loopback_mode {
            loop_pairs[i].1
        } else {
            cap_idx
        };
        let cap_data: &[f64] = &caps[cap_for_slot];
        let nominal = (pre * sp.rate as f64) as usize + i * slot + dev_off;
        let win = (0.25 * sp.rate as f64) as usize;
        let wlo = nominal.saturating_sub(win);
        let whi = (nominal + sweep_len + win).min(cap_data.len());
        if whi <= wlo + sweep_len / 2 {
            return Err(format!("capture 太短，无法检测 ch{ch} 起点"));
        }
        let (slot_source, slot_end_hz) = src_for(*ch);
        let window: Vec<f64> = cap_data[wlo..whi].to_vec();
        let (lag, _) = dsp::bandpassed_delay(
            &window,
            &slot_source,
            sp.rate as f64,
            sp.start_hz,
            slot_end_hz,
        );
        if lag < 0 {
            return Err(format!("ch{ch}: 未检测到扫频起点 (win={wlo} lag={lag})"));
        }
        let start = wlo + lag as usize;
        let end = (start + sweep_len + sp.rate as usize).min(cap_data.len());
        if end <= start + sweep_len / 2 {
            return Err(format!("capture 太短，无法分析 ch{ch}"));
        }
        let mut isolation_other_db = f64::NEG_INFINITY;
        if verify_map && capc > 1 {
            let mut energies = Vec::with_capacity(capc);
            for k in 0..capc {
                let e: f64 = caps[k][start.min(caps[k].len())..end.min(caps[k].len())]
                    .iter()
                    .map(|v| v * v)
                    .sum();
                energies.push(e.max(1e-20));
            }
            let (mut best_cap, mut best_e) = (0usize, -1.0f64);
            for (k, e) in energies.iter().enumerate() {
                if *e > best_e {
                    best_e = *e;
                    best_cap = k;
                }
            }
            if best_cap != cap_for_slot {
                return Err(format!("T1 mapping FAIL: 播放 ch{ch}(out{}) 的最大能量出现在采集 ch{best_cap}，期望 in{}（N4 类映射错/串线）", ch + 1, cap_for_slot + 1));
            }
            if loopback_mode {
                // 2 根线抽检：其余采集通道应 ≤ -60 dB（电学隔离）
                let other_max = energies
                    .iter()
                    .enumerate()
                    .filter(|(k, _)| *k != cap_for_slot)
                    .map(|(_, e)| *e)
                    .fold(0.0f64, f64::max);
                isolation_other_db = 10.0 * (other_max / best_e).log10();
                println!(
                    "  isolation: out{} -> in{}, 其他采集通道最大 {:.1} dB",
                    ch + 1,
                    cap_for_slot + 1,
                    isolation_other_db
                );
                if isolation_other_db > -60.0 {
                    return Err(format!(
                        "T1 isolation FAIL: 播放 out{} 时其他采集通道 {:.1} dB > -60 dB",
                        ch + 1,
                        isolation_other_db
                    ));
                }
            }
        }
        let name = names.get(i).cloned().unwrap_or_else(|| {
            if loopback_mode {
                format!("OUT{}_IN{}", ch + 1, cap_for_slot + 1)
            } else {
                format!("ch{ch}")
            }
        });
        let named_sub = name.to_ascii_uppercase().starts_with("SUB") || slot_end_hz <= 200.0;
        let slice: Vec<f64> = cap_data[start..end].to_vec();
        let r = dsp::analyze_capture(
            &slice,
            &slot_source,
            sp.rate,
            sp.start_hz,
            slot_end_hz,
            0,
            None,
        )?;
        let mut abs_delay = start as f64 + r.delay_samples;
        let mut repeats_detail: Option<RepeatDetail> = None;
        let mut clock_ppm: Option<f64> = None;
        let mut clock_ok: Option<bool> = None;
        let mut clock_uncertainty: Option<f64> = None;
        let mut clock_significant: Option<bool> = None;
        let mut drift_flag = false;
        let mut onset_bad = false;
        let mut snr_band: Option<f64> = None;
        let mut snr_band_hz: Option<(f64, f64)> = None;
        let mut repeat_starts: Vec<f64> = vec![start as f64];
        let mut robust_pol = r.robust_polarity;
        let mut listening_pol = r.polarity_listening;
        let mut confidence = r.polarity_confidence.clone();
        let mut estimators = r.estimators.clone();
        let mut fr = r.fr.clone();
        let mut fr_modal = r.fr_modal.clone();
        let mut peak_db = r.peak_db;
        let mut phase = r.phase_1k_deg;
        let mut snr = r.snr_db;
        let mut coarse = r.delay_coarse_samples;
        let legacy_pol = r.polarity;
        if repeats > 1 {
            let rep_lo = nominal.saturating_sub(win);
            let rep_hi = (nominal + repeats * interval + sp.rate as usize).min(cap_data.len());
            if rep_hi <= rep_lo + sweep_len / 2 {
                return Err(format!("capture 太短，无法做 ch{ch} 多遍分析"));
            }
            let agg = robust::analyze_repeats(
                &cap_data[rep_lo..rep_hi],
                &slot_source,
                sp.rate,
                sp.start_hz,
                slot_end_hz,
                interval as f64,
                repeats,
                drift_correct,
                max_ppm,
                Some(start as f64 - rep_lo as f64),
            )?;
            for wmsg in &agg.warnings {
                eprintln!("WARN ch{ch}: {wmsg}");
            }
            abs_delay = rep_lo as f64 + agg.delay_samples;
            robust_pol = agg.robust_polarity;
            listening_pol = agg.listening_polarity;
            confidence = agg.confidence.clone();
            estimators = agg.estimators.clone();
            fr = agg.fr.clone();
            fr_modal = agg.fr_modal.clone();
            peak_db = agg.peak_db;
            phase = agg.phase_1k_deg;
            snr = agg.snr_db;
            coarse = agg.delay_coarse_samples;
            clock_ppm = agg.clock_ppm;
            clock_ok = Some(agg.clock_ppm_ok);
            clock_uncertainty = Some(agg.clock_uncertainty_ppm);
            clock_significant = Some(agg.clock_significant);
            drift_flag = agg.drift_corrected;
            onset_bad = agg.onset_invalid;
            snr_band = Some(agg.snr_band_db);
            snr_band_hz = Some(agg.snr_band_hz);
            repeat_starts = agg
                .repeats
                .delay_samples
                .iter()
                .enumerate()
                .map(|(k, d)| rep_lo as f64 + d + k as f64 * interval as f64)
                .collect();
            repeats_detail = Some(agg.repeats);
            if !agg.clock_ppm_ok {
                eprintln!("WARN ch{ch}: clock_ppm 无效或超限，onset_invalid=true");
            }
        }
        println!(
            "  slot {} ch{}: nominal={} actual_start={} lag={} ",
            i, ch, nominal, start, lag
        );
        let committed = if robust_on {
            apply_sign(robust_pol, pol_off)
        } else {
            legacy_pol * pol_off
        };
        let mut meas = base_measurement(
            *ch,
            abs_delay,
            coarse,
            committed,
            peak_db,
            phase,
            snr,
            fr,
            meta(&sp, &pb, pbc, &cap, capc),
        );
        meas.fr_modal = fr_modal;
        meas.polarity_confidence = Some(confidence.clone());
        meas.polarity_estimators = Some(flip_estimators(&estimators, pol_off));
        meas.polarity_robust = Some(apply_sign(robust_pol, pol_off));
        meas.polarity_listening = Some(apply_sign(listening_pol, pol_off));
        meas.polarity_source = Some(if robust_on {
            "listening".into()
        } else {
            "legacy-ir-peak".into()
        });
        meas.repeats = repeats_detail;
        meas.clock_ppm = clock_ppm;
        meas.clock_ppm_ok = clock_ok;
        meas.clock_ppm_uncertainty_ppm = clock_uncertainty;
        meas.clock_ppm_significant = clock_significant;
        meas.drift_corrected = Some(drift_flag);
        meas.onset_invalid = Some(onset_bad);
        meas.snr_band_db = snr_band;
        meas.snr_band_hz = snr_band_hz;
        if let Some(caps2) = &caps2 {
            let ref_data = &caps2[capture2_index];
            let ref_meas = if repeats > 1 {
                let rep_lo = nominal.saturating_sub(win);
                let rep_hi = (nominal + repeats * interval + sp.rate as usize).min(ref_data.len());
                let agg = robust::analyze_repeats(
                    &ref_data[rep_lo..rep_hi],
                    &slot_source,
                    sp.rate,
                    sp.start_hz,
                    slot_end_hz,
                    interval as f64,
                    repeats,
                    drift_correct,
                    max_ppm,
                    Some(start as f64 - rep_lo as f64),
                )?;
                let ref_abs = rep_lo as f64 + agg.delay_samples;
                let ref_stored = ref_abs - nominal as f64;
                let mut rm = base_measurement(
                    *ch,
                    ref_stored,
                    agg.delay_coarse_samples,
                    apply_sign(agg.robust_polarity, pol_off),
                    agg.peak_db,
                    agg.phase_1k_deg,
                    agg.snr_db,
                    agg.fr.clone(),
                    meta(
                        &sp,
                        &pb,
                        pbc,
                        &capture2_path.display().to_string(),
                        capture2_channels,
                    ),
                );
                rm.fr_modal = agg.fr_modal.clone();
                rm.polarity_confidence = Some(agg.confidence.clone());
                rm.polarity_estimators = Some(flip_estimators(&agg.estimators, pol_off));
                rm.polarity_robust = Some(apply_sign(agg.robust_polarity, pol_off));
                rm.polarity_listening = Some(apply_sign(agg.listening_polarity, pol_off));
                rm.clock_ppm = agg.clock_ppm;
                rm.clock_ppm_ok = Some(agg.clock_ppm_ok);
                rm.clock_ppm_uncertainty_ppm = Some(agg.clock_uncertainty_ppm);
                rm.clock_ppm_significant = Some(agg.clock_significant);
                rm.onset_invalid = Some(agg.onset_invalid);
                rm.snr_band_db = Some(agg.snr_band_db);
                rm.snr_band_hz = Some(agg.snr_band_hz);
                rm.drift_corrected = Some(agg.drift_corrected);
                rm.repeats = Some(agg.repeats);
                rm.polarity_source = Some("reference".into());
                (rm, ref_data[rep_lo..rep_hi].to_vec())
            } else {
                let wlo2 = wlo.min(ref_data.len());
                let whi2 = whi.min(ref_data.len());
                let window2 = ref_data[wlo2..whi2].to_vec();
                let (lag2, _) = dsp::bandpassed_delay(
                    &window2,
                    &slot_source,
                    sp.rate as f64,
                    sp.start_hz,
                    slot_end_hz,
                );
                if lag2 < 0 {
                    return Err(format!("ch{ch}: 参考麦未检测到扫频"));
                }
                let start2 = wlo2 + lag2 as usize;
                let end2 = (start2 + sweep_len + sp.rate as usize).min(ref_data.len());
                let r2 = dsp::analyze_capture(
                    &ref_data[start2..end2],
                    &slot_source,
                    sp.rate,
                    sp.start_hz,
                    slot_end_hz,
                    0,
                    None,
                )?;
                let mut rm = base_measurement(
                    *ch,
                    start2 as f64 + r2.delay_samples,
                    r2.delay_coarse_samples,
                    apply_sign(r2.robust_polarity, pol_off),
                    r2.peak_db,
                    r2.phase_1k_deg,
                    r2.snr_db,
                    r2.fr.clone(),
                    meta(
                        &sp,
                        &pb,
                        pbc,
                        &capture2_path.display().to_string(),
                        capture2_channels,
                    ),
                );
                rm.fr_modal = r2.fr_modal.clone();
                rm.polarity_confidence = Some(r2.polarity_confidence.clone());
                rm.polarity_estimators = Some(flip_estimators(&r2.estimators, pol_off));
                rm.polarity_robust = Some(apply_sign(r2.robust_polarity, pol_off));
                rm.polarity_listening = Some(apply_sign(r2.polarity_listening, pol_off));
                rm.polarity_source = Some("reference".into());
                (rm, ref_data[wlo2..whi2].to_vec())
            };
            let (mut rm, ref_window) = ref_meas;
            rm.measurement_role = Some("reference".into());
            if slot_end_hz < sp.end_hz {
                rm.meta.sweep_end_hz = slot_end_hz;
            }
            let rel = dsp::relative_polarity(&window, &ref_window);
            if let Some(est) = meas.polarity_estimators.as_mut() {
                est.reference_xcorr = Some(rel);
            }
            let ref_base = dir.join(format!("{capture2_name}-ch{ch:02}"));
            write_analysis(&rm, &ref_base)?;
            apply_reference_override(
                &mut meas,
                &rm,
                robust_on,
                named_sub,
                nearfield,
                *ch,
                capture2_index,
                &capture2_name,
                &format!("{}.analysis.json", ref_base.display()),
            );
        }
        let ref_nearfield_ok = nearfield
            && meas
                .reference
                .as_ref()
                .is_some_and(|r| r.valid && r.polarity_confidence == "high");
        if named_sub && !ref_nearfield_ok {
            meas.sub_polarity_basis = Some(
                "listening-position only; 20–130 Hz 在房间模态区会随位置反相，不提交极性".into(),
            );
            meas.polarity_confidence = Some("unknown".into());
            if robust_on && meas.polarity_source.as_deref() != Some("reference") {
                meas.polarity = 0;
                meas.polarity_source = Some("listening".into());
            }
            eprintln!("WARN ch{ch} ({name}): SUB 听音位极性不可提交（confidence=unknown）");
        } else if confidence == "unknown" {
            eprintln!("WARN ch{ch}: 极性置信度 unknown");
        } else if !robust_on
            && legacy_pol != 0
            && robust_pol != 0
            && legacy_pol != robust_pol
            && confidence == "high"
        {
            eprintln!("WARN ch{ch}: IR 峰 {legacy_pol:+} 与投票 {robust_pol:+} 不一致。当前 polarity 仍是 IR 峰；加 --robust-polarity 或 --repeats N 改用投票。");
        }
        if repeats > 1 {
            // 多遍结果写成相对本槽名义起点的超额延迟，跨通道才能比，而不会把槽间距写进校准。
            let excess = abs_delay - nominal as f64;
            meas.delay_samples = excess;
            meas.delay_ms = excess / sp.rate as f64 * 1000.0;
            let shift = nominal as f64 - nominal.saturating_sub(win) as f64;
            if let Some(detail) = meas.repeats.as_mut() {
                for d in detail.delay_samples.iter_mut() {
                    *d -= shift;
                }
            }
        }
        if slot_end_hz < sp.end_hz {
            meas.meta.sweep_end_hz = slot_end_hz;
        }
        if let Some((_, db)) = level_overrides.iter().find(|(c, _)| *c == *ch) {
            meas.meta.level_db = *db;
        }
        let base = dir.join(format!("{}-ch{:02}", name, ch));
        write_analysis(&meas, &base)?;
        if let Some(detail) = &meas.repeats {
            write_repeat_csv(
                &PathBuf::from(format!("{}.repeats.csv", base.display())),
                detail,
            )?;
        }
        slot_onsets.push(abs_delay);
        let diag = serde_json::json!({
            "schema": "khaen-calibration-diag/v1",
            "channel": ch, "name": name, "slot": i,
            "nominal_start": nominal, "actual_start": start, "lag_samples": lag,
            "repeats": repeats,
            "repeat_gap_seconds": if repeats > 1 { repeat_gap_s } else { 0.0 },
            "repeat_starts": repeat_starts,
            "period_size": period, "buffer_size": buffer,
            "playback_offset_seconds": playback_offset_seconds,
            "loopback_mode": loopback_mode,
            "loopback_map_1based": serde_json::json!(loop_pairs.iter().map(|(p, c)| serde_json::json!({"play": p + 1, "capture": c + 1})).collect::<Vec<_>>()),
            "isolation_other_max_db": isolation_other_db,
            "capture_channel_analyzed": cap_for_slot,
            "capture_file": capture_path.display().to_string(),
            "capture2_file": if have_capture2 { Some(capture2_path.display().to_string()) } else { None },
            "band_limit_hz": band_limits.iter().find(|(c, _, _)| *c == *ch).map(|(_, hz, st)| serde_json::json!({"start_hz": st, "end_hz": hz})),
            "level_db": level_overrides.iter().find(|(c, _)| *c == *ch).map(|(_, db)| *db),
            "onset_invalid": onset_bad,
            "clock_ppm": clock_ppm,
            "clock_ppm_ok": clock_ok,
            "clock_ppm_uncertainty_ppm": clock_uncertainty,
            "clock_ppm_significant": clock_significant,
            "snr_band_db": snr_band,
            "snr_band_hz": snr_band_hz,
            "repeat_diffs": meas.repeats.as_ref().map(|d| d.repeat_diffs.clone()),
            "accepted_diffs": meas.repeats.as_ref().map(|d| d.accepted_diffs.clone()),
            "rejected_diffs": meas.repeats.as_ref().map(|d| d.rejected_diffs.clone()),
            "spacing_samples": meas.repeats.as_ref().and_then(|d| d.spacing_samples),
            "anchor_samples": meas.repeats.as_ref().and_then(|d| d.anchor_samples),
            "residuals": meas.repeats.as_ref().map(|d| d.residuals.clone()),
            "ir_peak_suspect": meas.repeats.as_ref().map(|d| d.ir_peak_suspect.clone()),
            "insufficient_valid_passes": meas.repeats.as_ref().map(|d| d.insufficient_valid_passes),
        });
        fs::write(
            format!("{}.diag.json", base.display()),
            serde_json::to_string_pretty(&diag).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("write diag: {e}"))?;
        println!(
            "multi ch{} {}: delay={:.3} samples, polarity={:+}, confidence={}",
            ch,
            name,
            abs_delay,
            meas.polarity,
            meas.polarity_confidence.as_deref().unwrap_or("-")
        );
    }
    if slot_onsets.len() >= 2 {
        let n_intervals = slot_onsets.len() - 1;
        let (slot_ppm, slot_ok, slot_rms) = if n_intervals < 3 {
            eprintln!("WARN: 槽间间隔只有 {n_intervals} 个（<3），不报告 slot_clock_ppm");
            (None, false, None)
        } else {
            let pts: Vec<(usize, f64)> = slot_onsets
                .iter()
                .copied()
                .enumerate()
                .map(|(k, v)| (k, v))
                .collect();
            let fit = dsp::fit_clock(&pts, slot as f64, max_ppm, None);
            if !fit.valid {
                eprintln!(
                    "WARN: 槽间时钟拟合无效 slot_clock_ppm={:.2} residual={:.1}",
                    fit.ppm, fit.residual_rms
                );
            }
            (Some(fit.ppm), fit.valid, Some(fit.residual_rms))
        };
        let session = serde_json::json!({
            "schema": "khaen-calibration-session-diag/v1",
            "repeats": repeats,
            "repeat_gap_seconds": if repeats > 1 { repeat_gap_s } else { 0.0 },
            "slot_samples": slot,
            "interval_samples": interval,
            "slot_interval_count": n_intervals,
            "slot_clock_ppm": slot_ppm,
            "slot_clock_ppm_ok": slot_ok,
            "slot_residual_rms": slot_rms,
            "robust_polarity": robust_on,
            "drift_corrected": drift_correct,
            "max_ppm": max_ppm,
        });
        fs::write(
            dir.join("session.diag.json"),
            serde_json::to_string_pretty(&session).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("write session diag: {e}"))?;
    }
    println!("measure-multi done under {}", dir.display());
    Ok(())
}

fn cmd_validate(a: &A) -> Result<(), String> {
    let path = a
        .get("file")
        .map(|s| s.to_string())
        .or_else(|| a.pos.first().cloned())
        .ok_or_else(|| "validate 需要校准文件路径".to_string())?;
    let text = fs::read_to_string(&path).map_err(|e| format!("读取 {path}: {e}"))?;
    let report = check::validate_str(&text)?;
    for w in &report.warnings {
        eprintln!("WARN: {w}");
    }
    if report.errors.is_empty() {
        println!(
            "PASS {path}{}",
            if report.v1_readable {
                "（v1 可读取）"
            } else {
                ""
            }
        );
        Ok(())
    } else {
        for e in &report.errors {
            eprintln!("ERROR: {e}");
        }
        Err(format!("{path} 校验失败（{} 条）", report.errors.len()))
    }
}

fn cmd_check_pair(a: &A) -> Result<(), String> {
    let dir = PathBuf::from(a.req("dir"));
    let cal_path = a
        .get("calibration")
        .map(PathBuf::from)
        .or_else(|| a.pos.first().map(PathBuf::from))
        .ok_or_else(|| "check-pair 需要 --calibration".to_string())?;
    let cal: Calibration = serde_yaml::from_str(
        &fs::read_to_string(&cal_path).map_err(|e| format!("读取 {}: {e}", cal_path.display()))?,
    )
    .map_err(|e| format!("校准 YAML: {e}"))?;
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .map_err(|e| format!("read dir {}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map(|n| n.ends_with(".analysis.json"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    let mut found = Vec::new();
    for f in &files {
        let m = read_measurement(f)?;
        if m.measurement_role.as_deref() == Some("reference") {
            continue;
        }
        found.push(m);
    }
    let mut meas = Vec::new();
    let mut filt = Vec::new();
    for ch in &cal.channels {
        let m = found
            .iter()
            .find(|m| m.channel == ch.index)
            .cloned()
            .ok_or_else(|| format!("缺少 index {} ({}) 的分析", ch.index, ch.name))?;
        meas.push(m);
        filt.push(ch.filters.clone());
    }
    let delays: Vec<i64> = cal.channels.iter().map(|c| c.delay_samples).collect();
    let pols: Vec<i32> = cal
        .channels
        .iter()
        .map(|c| {
            if c.polarity.as_deref() == Some("invert") {
                -1
            } else {
                1
            }
        })
        .collect();
    let pairs = pair_specs(a);
    if pairs.is_empty() {
        return Err("check-pair 需要 --pairs A:B:fc".into());
    }
    let rate = cal.sample_rate as f64;
    let mut ok = true;
    for (na, nb, fc) in &pairs {
        let ai = cal
            .channels
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(na))
            .ok_or_else(|| format!("校准里没有 {na}"))?;
        let bi = cal
            .channels
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(nb))
            .ok_or_else(|| format!("校准里没有 {nb}"))?;
        let pr = check_pair(&meas, &filt, &delays, &pols, ai, bi, *fc, rate);
        println!(
            "{}:{} fc={:.1} pass={} dphi={:.1} cd={:.2} min_cd={:.2} notch={:.2} ripple={:.2} {:?}",
            na,
            nb,
            fc,
            pr.pass,
            pr.dphi_fc_deg,
            pr.cd_fc_db,
            pr.min_cd_db,
            pr.notch_margin_db,
            pr.ripple_db,
            pr.reasons
        );
        ok &= pr.pass;
    }
    if ok {
        Ok(())
    } else {
        Err("交叉对未通过".into())
    }
}

fn usage() {
    println!(
        r#"khaen-calibrate — 多声道延迟/极性/相位/PEQ 校准（Rust 单二进制）

用法:
  khaen-calibrate sweep        --out F.raw [--channels 22 --channel 0 --rate 48000 --seconds 5 --level-db -30 --start-hz 20 --end-hz 20000 --pre-seconds 1 --post-seconds 1]
  khaen-calibrate sweep-all    --dir DIR [--channels 22 ...]
  khaen-calibrate measure      --out cap.raw --channel N [--playback hw:4,0 --playback-channels 22 --capture hw:3,0 --capture-channels 2 ...]
  khaen-calibrate analyze      --capture cap.raw --out-prefix results/ch00 --channel N [--capture-channel 0 --capture-channels 2 --repeats 3 --repeat-gap 0.2 --robust-polarity --drift-correct --max-ppm 100 --fr-points N --fr-oct 1/48]
  khaen-calibrate fit          --analysis results/ch00.analysis.json --out results/ch00.calibration.yml [--target flat|tilt10|@file#id --band 80:16000 --band FL=80:16000 --hp 80 --lp 16000 --boost-cap-db 3 --positions DIR --max-stages 22]
  khaen-calibrate measure-multi --out-dir DIR --out-prefix results/run --channels 0,1,2,3 --names FL,FR,SL,SR [--mic umik --period-size 512 --buffer-size 2048 --repeats 3 --repeat-gap 0.2 --robust-polarity --drift-correct --capture2 hw:4,0 --capture2-channels 20 --capture2-channel 0 --capture2-name REF]
  khaen-calibrate measure-multi --out-dir DIR --loopback-map 9:7 --capture-channels 8 --verify-mapping   # T1 抽检（out9→in7，可多对 9:7,1:1）
                       [--band-limit 7:130 --level-override 7:-30]   # 指定通道带限扫频/单独电平（保护 LFE 等）
  khaen-calibrate simulate     --dir DIR [--channels 4 --delays 0,3.5,10,20 --polarities 1,1,1,1 --names FL,FR,SL,SR]
  khaen-calibrate merge        --dir results --out calibration.yml [--names FL,FR,C,SUB1,SL,SR,HFL,HFR --layout 4.1.2 --crossovers 80,2000 --pairs FL:SUB1:80 --match FL,FR --target flat|tilt10|@file#id --boost-cap-db 3 --max-stages 22 --require-high-confidence --allow-sub-polarity-from-listening-position]
  khaen-calibrate validate     calibration.yml
  khaen-calibrate check-pair   --dir results --calibration calibration.yml --pairs FL:SUB1:80

说明:
  - D4 目标曲线：内置 flat（默认）/tilt10（20Hz 0 → 20kHz -10dB，log-f）；旧 tilt(-3dB@16k) superseded；其它曲线用 --target @file.yml#id 导入。
  - D7 boost/headroom：--boost-cap-db 默认 3（>3 钳 3、<0 归 0）；fit/merge 报告记录 applied_boost_db/total_boost_db/max_predicted_gain_db/predicted_peak_dbfs/headroom_db/clip_risk；headroom 参考电平默认 -18 dBFS（-18+总正向帽12=-6，报告可追溯）。
  - sweep 为 S24_3LE 裸流；measure 先起 arecord 再 aplay 单通道扫频（其余通道静音），默认 -30 dBFS。
  - analyze 用 Farina 逆滤波 + 相位斜率分数延迟；delay/polarity/phase 分开输出。默认频响 300 点，另写 1/48 oct 的 fr_modal.csv。
  - fit/merge 把实测校正到目标曲线（默认平坦），写出 khaen-calibration/v1.1 与 CamillaDSP 片段，并写 predicted.csv / optimizer.json。
  - 给出 --pairs 时，使交叉对复频响失败的段会回退。merge 在 80–20k 与分频点做合成响应检查。"#
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" {
        usage();
        return;
    }
    let cmd = args[0].clone();
    let a = match A::new(&args[1..]) {
        Ok(a) => a,
        Err(e) => die(&e),
    };
    if a.get("help").is_some() {
        usage();
        return;
    }
    let r = match cmd.as_str() {
        "sweep" => cmd_sweep(&a),
        "sweep-all" => cmd_sweep_all(&a),
        "measure" => cmd_measure(&a),
        "analyze" => cmd_analyze(&a),
        "fit" => cmd_fit(&a),
        "merge" => cmd_merge(&a),
        "validate" => cmd_validate(&a),
        "check-pair" => cmd_check_pair(&a),
        "simulate" => cmd_simulate(&a),
        "measure-multi" => cmd_measure_multi(&a),
        other => Err(format!("未知子命令 {other}; 用 --help")),
    };
    if let Err(e) = r {
        die(&e);
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn flat_meas(ch: usize, delay: f64) -> ChannelMeasurement {
        let fr = (0..50)
            .map(|i| {
                let f = 100.0 * 200.0f64.powf(i as f64 / 49.0); // 100..20k
                FrPoint {
                    freq_hz: f,
                    db: 0.0,
                    phase_deg: 0.0,
                }
            })
            .collect();
        base_measurement(
            ch,
            delay,
            delay as i64,
            1,
            0.0,
            0.0,
            60.0,
            fr,
            Meta {
                schema: "m".into(),
                sample_rate: 48000,
                channels: 2,
                sweep_start_hz: 20.0,
                sweep_end_hz: 20000.0,
                sweep_seconds: 1.0,
                level_db: -30.0,
                pre_seconds: 0.1,
                post_seconds: 0.1,
                playback_device: "sim".into(),
                playback_channels: 2,
                capture_device: "sim".into(),
                capture_channels: 1,
                created_unix: 0,
            },
        )
    }

    fn flat_meas2(
        ch: usize,
        delay: f64,
        polarity: i32,
        db: f64,
        phase_deg: f64,
    ) -> ChannelMeasurement {
        let fr = (0..80)
            .map(|i| {
                let f = 80.0 * 250.0f64.powf(i as f64 / 79.0); // 80..20k
                FrPoint {
                    freq_hz: f,
                    db,
                    phase_deg,
                }
            })
            .collect();
        base_measurement(
            ch,
            delay,
            delay as i64,
            polarity,
            db,
            phase_deg,
            60.0,
            fr,
            Meta {
                schema: "m".into(),
                sample_rate: 48000,
                channels: 2,
                sweep_start_hz: 20.0,
                sweep_end_hz: 20000.0,
                sweep_seconds: 1.0,
                level_db: -30.0,
                pre_seconds: 0.1,
                post_seconds: 0.1,
                playback_device: "sim".into(),
                playback_channels: 2,
                capture_device: "sim".into(),
                capture_channels: 1,
                created_unix: 0,
            },
        )
    }

    #[test]
    fn negative_n1_polarity_detected() {
        let meas = vec![
            flat_meas2(0, 0.0, 1, 0.0, 0.0),
            flat_meas2(1, 0.0, 1, 0.0, 180.0),
        ];
        let r = check_pair(
            &meas,
            &[vec![], vec![]],
            &[0, 0],
            &[1, 1],
            0,
            1,
            2000.0,
            48000.0,
        );
        assert!(!r.pass, "N1 real-data polarity must FAIL, got {r:?}");
        let ok = check_pair(
            &meas,
            &[vec![], vec![]],
            &[0, 0],
            &[1, -1],
            0,
            1,
            2000.0,
            48000.0,
        );
        assert!(
            ok.pass,
            "N1 with calibration polarity=invert should PASS, got {ok:?}"
        );
    }

    #[test]
    fn negative_n2_delay_detected() {
        // 2 kHz 交叉对一路加 0.25 ms（=180°@2kHz）
        let meas = vec![
            flat_meas2(0, 0.0, 1, 0.0, 0.0),
            flat_meas2(1, 12.0, 1, 0.0, 0.0),
        ];
        let r = check_pair(
            &meas,
            &[vec![], vec![]],
            &[0, 0],
            &[1, 1],
            0,
            1,
            2000.0,
            48000.0,
        );
        assert!(!r.pass, "N2 delay must FAIL, got {r:?}");
    }

    #[test]
    fn negative_n3_peq_detected() {
        let peq = vec![Peq::peaking(2000.0, 1.0, 6.0)];
        let meas = vec![
            flat_meas2(0, 0.0, 1, 0.0, 0.0),
            flat_meas2(1, 0.0, 1, 0.0, 0.0),
        ];
        let r = check_pair(
            &meas,
            &[vec![], peq],
            &[0, 0],
            &[1, 1],
            0,
            1,
            2000.0,
            48000.0,
        );
        assert!(!r.pass, "N3 +6dB PEQ must FAIL, got {r:?}");
    }

    #[test]
    fn negative_n5_sub_delay_detected() {
        // 80 Hz SUB pair，2 ms 延迟
        let meas = vec![
            flat_meas2(0, 0.0, 1, 0.0, 0.0),
            flat_meas2(1, 96.0, 1, 0.0, 0.0),
        ];
        let r = check_pair(
            &meas,
            &[vec![], vec![]],
            &[0, 0],
            &[1, 1],
            0,
            1,
            80.0,
            48000.0,
        );
        assert!(!r.pass, "N5 sub delay must FAIL, got {r:?}");
    }
    #[test]
    fn merge_validation_detects_missing_delay_correction() {
        // 两个相同通道，24 样本差（1 kHz 处反相）；不校正时合成应近乎抵消
        let meas = vec![flat_meas(0, 0.0), flat_meas(1, 24.0)];
        let fits: Vec<Vec<Peq>> = vec![vec![], vec![]];
        let (sum_db, single_db) = phase_sum_margin(&meas, &fits, &[0, 0], &[1, 1], 1000.0, 48000.0);
        let margin = sum_db - single_db;
        assert!(
            margin < -20.0,
            "uncorrected margin should be very low, got {margin}"
        );
        // 加上归一化校正：ch0 补 24、ch1 补 0，应同相叠加
        let (sum_db2, single_db2) =
            phase_sum_margin(&meas, &fits, &[24, 0], &[1, 1], 1000.0, 48000.0);
        let margin2 = sum_db2 - single_db2;
        assert!(
            margin2 > 3.0,
            "corrected margin should be positive, got {margin2}"
        );
    }

    #[test]
    fn offline_repeats_reference_and_sub_merge() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("robust-fixture");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let rate = 48_000u32;
        let seconds = 0.35f64;
        let mk = |end: f64| {
            sweep::SweepSpec {
                sample_rate: rate,
                start_hz: 20.0,
                end_hz: end,
                seconds,
                level_db: -30.0,
                fade_ms: 10.0,
            }
            .generate()
        };
        let fl = mk(8000.0);
        let sub = mk(130.0);
        let sweep_len = fl.len();
        let repeats = 3usize;
        let rep_gap = (0.15 * rate as f64).round() as usize;
        let interval = sweep_len + rep_gap;
        let slot_gap = (0.25 * rate as f64) as usize;
        let slot = repeats * interval + slot_gap;
        let pre = (0.2 * rate as f64) as usize;
        let post = (0.2 * rate as f64) as usize;
        let total = pre + 2 * slot + post + rate as usize * 2;
        let mut main = vec![0.0f64; total];
        let mut reference = vec![0.0f64; total];
        for k in 0..repeats {
            let a = pre + k * interval + 20;
            let b = pre + slot + k * interval + 14;
            for (i, s) in fl.iter().enumerate() {
                if a + i < total {
                    main[a + i] += *s;
                }
                if a + i + 15 < total {
                    reference[a + i + 15] += *s;
                }
            }
            for (i, s) in sub.iter().enumerate() {
                if b + i < total {
                    main[b + i] += *s;
                    reference[b + i] += *s;
                }
            }
        }
        let cap = dir.join("cap.raw");
        let cap2 = dir.join("cap2.raw");
        raw::write_s24_single(&cap, &main).unwrap();
        raw::write_s24_single(&cap2, &reference).unwrap();
        let dir_s = dir.display().to_string();
        let owned: Vec<String> = vec![
            "measure-multi".into(),
            "--out-dir".into(),
            dir_s.clone(),
            "--out-prefix".into(),
            dir_s.clone(),
            "--channels".into(),
            "0,1".into(),
            "--names".into(),
            "FL,SUB1".into(),
            "--capture-file".into(),
            cap.display().to_string(),
            "--capture-channels".into(),
            "1".into(),
            "--capture-channel".into(),
            "0".into(),
            "--capture2-file".into(),
            cap2.display().to_string(),
            "--capture2-channels".into(),
            "1".into(),
            "--capture2-channel".into(),
            "0".into(),
            "--capture2-name".into(),
            "REF".into(),
            "--playback-channels".into(),
            "2".into(),
            "--rate".into(),
            "48000".into(),
            "--seconds".into(),
            "0.35".into(),
            "--start-hz".into(),
            "20".into(),
            "--end-hz".into(),
            "8000".into(),
            "--level-db".into(),
            "-30".into(),
            "--pre-seconds".into(),
            "0.2".into(),
            "--post-seconds".into(),
            "0.2".into(),
            "--gap-seconds".into(),
            "0.25".into(),
            "--repeats".into(),
            "3".into(),
            "--repeat-gap".into(),
            "0.15".into(),
            "--playback-offset-seconds".into(),
            "0".into(),
            "--band-limit".into(),
            "1:130".into(),
        ];
        let a = A::new(&owned[1..]).unwrap();
        cmd_measure_multi(&a).unwrap();
        let fl_json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("FL-ch00.analysis.json")).unwrap())
                .unwrap();
        assert_eq!(fl_json["polarity_confidence"], "high", "{fl_json}");
        assert_eq!(fl_json["polarity"], 1);
        assert_eq!(fl_json["repeats"]["count"], 3);
        assert!(
            fl_json["clock_ppm"].as_f64().unwrap().abs() < 5.0,
            "ppm={}",
            fl_json["clock_ppm"]
        );
        assert!(fl_json["clock_ppm_ok"].as_bool().unwrap());
        assert_eq!(fl_json["polarity_estimators"]["direct_window"], 1);
        assert_eq!(fl_json["polarity_estimators"]["lowband"], 1);
        let ref_delay = fl_json["reference"]["delay_samples"].as_f64().unwrap();
        let main_delay = fl_json["delay_samples"].as_f64().unwrap();
        assert!(
            (ref_delay - main_delay - 15.0).abs() < 1.0,
            "main={main_delay} ref={ref_delay}"
        );
        assert!(dir.join("REF-ch00.analysis.json").exists());
        let sub_json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("SUB1-ch01.analysis.json")).unwrap())
                .unwrap();
        assert_eq!(sub_json["polarity_confidence"], "unknown");
        assert_eq!(sub_json["polarity"], 0);
        let yml = dir.join("calibration.yml");
        let merge_args = vec![
            "--dir".into(),
            dir_s,
            "--out".into(),
            yml.display().to_string(),
            "--names".into(),
            "FL,SUB1".into(),
            "--layout".into(),
            "synthetic".into(),
            "--crossovers".into(),
            "80".into(),
            "--max-stages".into(),
            "0".into(),
        ];
        cmd_merge(&A::new(&merge_args).unwrap()).unwrap();
        let cal: Calibration = serde_yaml::from_str(&fs::read_to_string(&yml).unwrap()).unwrap();
        assert_eq!(cal.channels[0].name, "FL");
        assert_eq!(cal.channels[0].polarity.as_deref(), Some("normal"));
        assert_eq!(cal.channels[1].name, "SUB1");
        assert!(
            cal.channels[1].polarity.is_none(),
            "SUB 听音位不应写入 polarity"
        );
        let report = check::validate_str(&fs::read_to_string(&yml).unwrap()).unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
    }

    #[test]
    fn merge_polarity_sub_omitted_unless_allowed() {
        let mut m = flat_meas(5, 12.0);
        m.polarity = 0;
        m.polarity_listening = Some(-1);
        m.polarity_confidence = Some("unknown".into());
        m.meta.sweep_end_hz = 130.0;
        assert!(merge_polarity(&m, "SUB1", false, false).unwrap().is_none());
        assert_eq!(
            merge_polarity(&m, "SUB1", true, false).unwrap().as_deref(),
            Some("invert")
        );
        m.meta.sweep_end_hz = 20_000.0;
        m.polarity = 1;
        m.polarity_confidence = Some("low".into());
        assert!(merge_polarity(&m, "FL", false, true).is_err());
        assert_eq!(
            merge_polarity(&m, "FL", false, false).unwrap().as_deref(),
            Some("normal")
        );
    }

    #[test]
    fn invalid_reference_does_not_override_main_polarity() {
        let mut main_m = flat_meas(0, 10.0);
        main_m.polarity = -1;
        main_m.polarity_listening = Some(-1);
        main_m.polarity_confidence = Some("high".into());
        main_m.polarity_source = Some("listening".into());
        let mut reference = flat_meas(0, 12.0);
        reference.polarity = 1;
        reference.polarity_confidence = Some("low".into());
        reference.clock_ppm_ok = Some(false);
        reference.onset_invalid = Some(true);
        reference.snr_db = 40.0;
        apply_reference_override(
            &mut main_m,
            &reference,
            true,
            false,
            false,
            0,
            0,
            "REF",
            "REF-ch00.analysis.json",
        );
        assert_eq!(main_m.polarity, -1);
        assert_eq!(main_m.polarity_source.as_deref(), Some("listening"));
        assert_eq!(main_m.reference_invalid, Some(true));
        let summary = main_m.reference.as_ref().unwrap();
        assert!(!summary.valid);
        assert!(summary
            .invalid_reason
            .as_deref()
            .unwrap_or("")
            .contains("clock_ppm_ok=false"));

        reference.clock_ppm_ok = Some(true);
        reference.onset_invalid = Some(false);
        reference.polarity_confidence = Some("high".into());
        reference.snr_db = 35.0;
        apply_reference_override(
            &mut main_m,
            &reference,
            true,
            false,
            false,
            0,
            0,
            "REF",
            "REF-ch00.analysis.json",
        );
        assert_eq!(main_m.polarity, 1);
        assert_eq!(main_m.polarity_source.as_deref(), Some("reference"));
        assert_eq!(main_m.polarity_confidence.as_deref(), Some("low"));
        assert!(main_m.reference_invalid.is_none());
        assert!(main_m.reference.as_ref().unwrap().valid);
    }

    #[test]
    fn pair_failure_rolls_back_the_stage() {
        let meas = vec![flat_meas(0, 0.0), flat_meas(1, 0.0)];
        let channels = vec![
            ChannelCalibration {
                index: 0,
                name: "FL".into(),
                delay_samples: 0,
                polarity: Some("normal".into()),
                filters: vec![],
                gain_db: None,
            },
            ChannelCalibration {
                index: 1,
                name: "FR".into(),
                delay_samples: 0,
                polarity: Some("normal".into()),
                filters: vec![],
                gain_db: None,
            },
        ];
        let mut bad = empty_report("bad");
        bad.broadband = vec![Peq::peaking(1000.0, 2.0, -18.0)];
        let a = A::new(&["--pairs".into(), "FL:FR:1000".into()]).unwrap();
        let fits = commit_stages(
            &meas,
            &channels,
            &[bad, empty_report("other")],
            &a,
            48_000.0,
        );
        assert!(fits[0].is_empty(), "破坏交叉对的段应回退 {:?}", fits[0]);
    }

    /// 裁决 2：shelf 片段必须用 CamillaDSP 的 Slope 型（slope=12*q），不能写 q。
    #[test]
    fn shelf_fragment_uses_slope_not_q() {
        let cal = Calibration {
            schema: "khaen-calibration/v1.1".into(),
            sample_rate: 48_000,
            reference_index: 0,
            layout: Some("test".into()),
            created_unix: Some(0),
            tool: Some("khaen-calibrate".into()),
            version: Some("0.1.0".into()),
            note: "test".into(),
            channels: vec![ChannelCalibration {
                index: 0,
                name: "FL".into(),
                delay_samples: 0,
                polarity: Some("normal".into()),
                filters: vec![
                    Peq::typed("low_shelf", 100.0, 0.7, -3.0),
                    Peq::typed("high_shelf", 4000.0, 0.5, 2.0),
                    Peq::peaking(1000.0, 1.2, -2.0),
                ],
                gain_db: None,
            }],
        };
        let frag = cal_to_camilladsp(&cal);
        let low = &frag["filters"]["cal_ch0_peq0"]["parameters"];
        assert_eq!(low["type"], "Lowshelf");
        assert!((low["slope"].as_f64().unwrap() - 8.4).abs() < 1e-9, "{low}");
        assert!(low.get("q").is_none(), "shelf must not carry q: {low}");
        let high = &frag["filters"]["cal_ch0_peq1"]["parameters"];
        assert_eq!(high["type"], "Highshelf");
        assert!(
            (high["slope"].as_f64().unwrap() - 6.0).abs() < 1e-9,
            "{high}"
        );
        let peak = &frag["filters"]["cal_ch0_peq2"]["parameters"];
        assert_eq!(peak["type"], "Peaking");
        assert!(peak.get("q").is_some() && peak.get("slope").is_none());
    }

    // ---- merge-fix 回归测试（task-224 子项：排序/延迟质量门/pair 门控） ----

    fn test_repeat_detail(residuals: Vec<f64>, accepted_diffs: Vec<f64>) -> RepeatDetail {
        let count = residuals.len().max(1);
        RepeatDetail {
            count,
            delay_samples: vec![0.0; count],
            polarity_votes: vec![1; count],
            ir_peak_idx: vec![0; count],
            snr_db: vec![40.0; count],
            per_repeat_confidence: vec!["high".into(); count],
            per_repeat_estimators: vec![
                crate::model::PolarityEstimators {
                    ir_peak: 1,
                    direct_window: 1,
                    phase_vote: 1,
                    lowband: 1,
                    matched: 1,
                    reference_xcorr: None,
                };
                count
            ],
            onset_invalid: vec![false; count],
            onsets: vec![],
            repeat_diffs: vec![],
            accepted_diffs,
            rejected_diffs: vec![],
            spacing_samples: Some(249_600.0),
            anchor_samples: Some(0.0),
            residuals,
            clock_ppm_source: Some("median_adjacent_diffs".into()),
            insufficient_valid_passes: false,
            ir_peak_suspect: vec![],
        }
    }

    fn write_meas_dir(dir: &Path, meas: &[ChannelMeasurement]) {
        let _ = fs::remove_dir_all(dir);
        fs::create_dir_all(dir).unwrap();
        for m in meas {
            let name = format!("x-ch{:02}.analysis.json", m.channel);
            fs::write(dir.join(name), serde_json::to_string_pretty(m).unwrap()).unwrap();
        }
    }

    /// 文件名顺序（FL,FR,HFL,HFR,SL,SR,SUB1）≠ channel 顺序；排序必须按 channel，
    /// 且 --names 按 channel 升序对应（旧实现导致 HFL/HFR 与 SL/SR 错位）。
    #[test]
    fn merge_inputs_sorted_by_channel_and_names_aligned() {
        let mapping: [(usize, &str); 7] = [
            (0, "FL"),
            (1, "FR"),
            (4, "HFL"),
            (5, "HFR"),
            (2, "SL"),
            (3, "SR"),
            (7, "SUB1"),
        ];
        let inputs: Vec<(PathBuf, ChannelMeasurement)> = mapping
            .iter()
            .map(|(ch, name)| {
                (
                    PathBuf::from(format!("{name}-ch{ch:02}.analysis.json")),
                    flat_meas(*ch, 0.0),
                )
            })
            .collect();
        let meas = sort_measurements_by_channel(inputs).unwrap();
        assert_eq!(
            meas.iter().map(|m| m.channel).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4, 5, 7]
        );
        let names: Vec<String> = ["FL", "FR", "SL", "SR", "HFL", "HFR", "SUB1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let roles = resolve_role_names(&meas, &names).unwrap();
        assert_eq!(roles, names);
        assert_eq!(roles[2], "SL", "ch2 必须拿到 SL，而不是 HFL");
        assert_eq!(roles[4], "HFL", "ch4 必须拿到 HFL，而不是 SL");
    }

    #[test]
    fn merge_input_guard_rejects_duplicate_channel_and_bad_names() {
        let dup = vec![
            (PathBuf::from("a.analysis.json"), flat_meas(1, 0.0)),
            (PathBuf::from("b.analysis.json"), flat_meas(1, 0.0)),
        ];
        let err = sort_measurements_by_channel(dup).unwrap_err();
        assert!(err.contains("E_MERGE_DUPLICATE_CHANNEL"), "{err}");
        let meas = vec![flat_meas(0, 0.0), flat_meas(1, 0.0)];
        let err = resolve_role_names(&meas, &["FL".to_string()]).unwrap_err();
        assert!(err.contains("E_MERGE_NAMES_COUNT"), "{err}");
        let err = resolve_role_names(&meas, &["FL".to_string(), "fl".to_string()]).unwrap_err();
        assert!(err.contains("E_MERGE_DUPLICATE_NAME"), "{err}");
    }

    /// pilot FR-ch01 同款：逐遍残差交替 ±86.9、accepted 相邻差 ±174、跨通道偏离中位数 3857。
    /// merge 必须拒绝，而不是用 max−d_i 写出 88 ms 假延迟。
    #[test]
    fn delay_gate_rejects_pilot_fr_outlier() {
        let delays = [
            -9811.099, -5576.124, -9215.499, -9433.034, -9858.241, -9976.834, -9057.563,
        ];
        let chans = [0usize, 1, 2, 3, 4, 5, 7];
        let mut meas: Vec<ChannelMeasurement> = delays
            .iter()
            .enumerate()
            .map(|(i, d)| flat_meas(chans[i], *d))
            .collect();
        meas[1].repeats = Some(test_repeat_detail(
            vec![-86.925_117, 86.925_117, -86.925_117],
            vec![249_773.870_8, 249_426.170_4],
        ));
        let names: Vec<String> = ["FL", "FR", "SL", "SR", "HFL", "HFR", "SUB1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let err = delay_quality_gate(&meas, &names).unwrap_err();
        assert!(err.contains("E_MERGE_DELAY_QUALITY"), "{err}");
        assert!(err.contains("E_DELAY_OUTLIER"), "{err}");
        assert!(err.contains("E_DELAY_RESIDUAL"), "{err}");
        assert!(err.contains("E_DELAY_SPACING"), "{err}");
        assert!(err.contains("FR"), "{err}");
    }

    /// 其余 6 个通道（延迟一致、无 repeats 坏点）必须过质量门，不能误杀。
    #[test]
    fn delay_gate_accepts_consistent_channels() {
        let delays = [
            -9811.099, -9215.499, -9433.034, -9858.241, -9976.834, -9057.563,
        ];
        let chans = [0usize, 2, 3, 4, 5, 7];
        let mut meas: Vec<ChannelMeasurement> = delays
            .iter()
            .enumerate()
            .map(|(i, d)| flat_meas(chans[i], *d))
            .collect();
        // 一个正常通道带完整 repeats 残差/相邻差（pilot FL：0.81 / 249596.8, 249600.1）。
        meas[0].repeats = Some(test_repeat_detail(
            vec![0.815, -0.815, 0.815],
            vec![249_596.8, 249_600.1],
        ));
        let names: Vec<String> = ["FL", "SL", "SR", "HFL", "HFR", "SUB1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(
            delay_quality_gate(&meas, &names).is_ok(),
            "{:?}",
            delay_quality_gate(&meas, &names).err()
        );
    }

    /// 汇总判定：global advisory 不参与；无 pairs → INCOMPLETE；pairs 才是 T4 gate。
    #[test]
    fn overall_status_uses_pair_gate_not_global_sum() {
        // 无 --pairs：即使全局求和 FAIL，也只能 INCOMPLETE（不是 FAIL，也不是 PASS）。
        assert_eq!(
            merge_overall_status(false, false, false, "not_run"),
            "INCOMPLETE"
        );
        // 有 pairs 且 pair 全过 + negative_controls 5/5 → PASS（全局数值不在函数输入里）。
        assert_eq!(merge_overall_status(false, true, true, "pass"), "PASS");
        // 有 pairs 但 pair 失败 → FAIL。
        assert_eq!(merge_overall_status(false, true, false, "pass"), "FAIL");
        // pairs 过但 negative_controls 未跑/不足 → INCOMPLETE（ACCEPTANCE §8）。
        assert_eq!(
            merge_overall_status(false, true, true, "not_run"),
            "INCOMPLETE"
        );
        // 负向用例漏检 → FAIL。
        assert_eq!(merge_overall_status(false, true, true, "fail"), "FAIL");
        // clip_risk 仍 FAIL。
        assert_eq!(merge_overall_status(true, true, true, "pass"), "FAIL");
        assert!(!merge_overall_status(false, true, true, "pass").is_empty());
    }

    fn run_merge_fixture(dir: &Path, tag: &str, names: &str, extra: &[&str]) -> serde_json::Value {
        let out = dir.join(format!("{tag}.yml"));
        let mut owned: Vec<String> = vec![
            "--dir".into(),
            dir.display().to_string(),
            "--out".into(),
            out.display().to_string(),
            "--names".into(),
            names.into(),
            "--layout".into(),
            "synthetic".into(),
            "--crossovers".into(),
            "1000".into(),
            "--max-stages".into(),
            "0".into(),
        ];
        owned.extend(extra.iter().map(|s| s.to_string()));
        cmd_merge(&A::new(&owned).unwrap()).unwrap();
        serde_json::from_str(&fs::read_to_string(format!("{}.report.json", out.display())).unwrap())
            .unwrap()
    }

    /// 无 --pairs：全局求和明明 FAIL，但 report 必须是 INCOMPLETE + pair_validation=not_run，
    /// 且 global_validation 明确标 advisory/used_as_gate=false。
    #[test]
    fn merge_without_pairs_is_incomplete_even_if_global_sum_fails() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("merge-gate-nopairs");
        // 两路同延迟：ch1 反相 +6 dB → 全局求和深抵消（FAIL），但这不是 T4 判据。
        let ch0 = flat_meas(0, 0.0);
        // 测得相位 180°、校准 polarity=normal（+1）→ 全通道求和深抵消（advisory FAIL）。
        let ch1 = flat_meas2(1, 0.0, 1, 6.0, 180.0);
        write_meas_dir(&dir, &[]);
        for (name, m) in [("x-ch00.analysis.json", ch0), ("x-ch01.analysis.json", ch1)] {
            fs::write(dir.join(name), serde_json::to_string_pretty(&m).unwrap()).unwrap();
        }
        let report = run_merge_fixture(&dir, "nopairs", "FL,FR", &[]);
        assert_eq!(report["overall_status"], "INCOMPLETE", "{report}");
        assert_eq!(report["overall_pass"], false);
        assert_eq!(report["pair_validation"]["status"], "not_run");
        assert_eq!(report["global_validation"]["used_as_gate"], false);
        assert_eq!(report["global_validation"]["status"], "NOT_RUN");
        assert_eq!(
            report["global_validation"]["passed"], false,
            "全局求和应 FAIL 但它只是诊断: {report}"
        );
        let validation: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(dir.join("nopairs.yml.validation.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(validation["status"], "NOT_RUN");
        assert_eq!(validation["passed"], false);
        assert_eq!(validation["advisory_only"], true);
    }

    /// 有 --pairs：只由交叉对判据决定 passed —— 全局求和 FAIL 也能 PASS（pairs 通过 +
    /// negative_controls 5/5 detected）。
    #[test]
    fn merge_with_pairs_gates_only_on_pairs() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("merge-gate-pairs");
        let ch0 = flat_meas(0, 0.0);
        let ch1 = flat_meas(1, 0.0);
        // 第三路反相 +6 dB：全局全通道求和深抵消（advisory FAIL），但不参与 pair。
        let ch2 = flat_meas2(2, 0.0, 1, 6.0, 180.0);
        write_meas_dir(&dir, &[]);
        for (name, m) in [
            ("x-ch00.analysis.json", ch0),
            ("x-ch01.analysis.json", ch1),
            ("x-ch02.analysis.json", ch2),
        ] {
            fs::write(dir.join(name), serde_json::to_string_pretty(&m).unwrap()).unwrap();
        }
        let neg = dir.join("negative-controls.json");
        let items: Vec<serde_json::Value> = (1..=5)
            .map(|i| serde_json::json!({"id": format!("N{i}"), "pass": true, "detected": true}))
            .collect();
        fs::write(&neg, serde_json::to_string_pretty(&items).unwrap()).unwrap();
        let neg_str = neg.display().to_string();
        let report = run_merge_fixture(
            &dir,
            "pairs",
            "FL,FR,HFL",
            &["--pairs", "FL:FR:1000", "--negative-controls", &neg_str],
        );
        assert_eq!(
            report["global_validation"]["passed"], false,
            "全局求和应 FAIL: {report}"
        );
        assert_eq!(report["global_validation"]["used_as_gate"], false);
        assert_eq!(report["pair_validation"]["status"], "pass", "{report}");
        assert_eq!(report["negative_controls"]["status"], "pass");
        assert_eq!(report["overall_status"], "PASS", "{report}");
        assert_eq!(report["overall_pass"], true);
        let validation: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(dir.join("pairs.yml.validation.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(validation["status"], "PASS");
        assert_eq!(validation["passed"], true);
        assert_eq!(validation["gate"], "pairs");
    }

    /// P0-3：带限通道（SUB）用带内 SNR + 20 dB 门限；全带通道仍用 25 dB。
    #[test]
    fn band_limited_snr_gate_keeps_sub() {
        let mut sub = flat_meas(7, 10.0);
        sub.meta.sweep_end_hz = 130.0;
        sub.snr_db = 24.0;
        sub.snr_band_db = Some(24.0);
        assert!(
            weighted_from(&sub, "SUB1", 1.0).is_some(),
            "SUB 20-130 Hz SNR 24 dB must pass its own 20 dB gate"
        );
        sub.snr_db = 19.0;
        sub.snr_band_db = Some(19.0);
        assert!(
            weighted_from(&sub, "SUB1", 1.0).is_none(),
            "band-limited channel below 20 dB must be rejected"
        );
        let mut fl = flat_meas(0, 10.0);
        fl.meta.sweep_end_hz = 20_000.0;
        fl.snr_db = 24.0;
        fl.snr_band_db = Some(40.0);
        assert!(
            weighted_from(&fl, "FL", 1.0).is_none(),
            "full-band channel keeps the 25 dB gate"
        );
    }
}
