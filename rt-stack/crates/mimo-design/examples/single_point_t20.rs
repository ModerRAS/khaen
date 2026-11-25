//! 单点 MIMO 候选 B（T20 衰减改短）预测：真实 t4b 单点 5 执行器 H 上的离线设计实验。
//!
//! 用法（纯离线；不碰声卡/设备/板端，不 apply、不出声）：
//!   cargo run --offline --release -p khaen-mimo-design --example single_point_t20 -- \
//!       --h <H_complex.csv> [--out <evidence-dir>] [--taps 2048,4096,8192,16384] [--design-points 100]
//!
//! 流程：
//! 1) 把每通道实测 H(f) 插值到 12 kHz / NFFT=32768 网格（ln f 上对 ln|H| 与 unwrap 相位线性插值，
//!    共同测量带 20–129.019 Hz 之外置 0，不外推），得到 baseline 谱与 baseline IR；
//! 2) 每通道构造候选 B 目标 d = b + sum_k (e^{-delta_k max(0,t-t0)}-1) B_k b（T*=0.85T，decay.rs）；
//! 3) 对 taps 档 {2048,4096,8192,16384}、两个口径各算理想修正：
//!    (a) Diagonal: g_ii(f)=e^{-jwD}(D_i-H_i)/H_i，clip |g|<=+6 dB（T66 P_post 对角 FIR 可表达）；
//!    (b) Full5x5: 逐频点 solve_with_caps(H_vec, e^{-jwD}(D_i-H_i), caps diag=2.0/offdiag=0.5)
//!        （T51/T54 modal matrix 形态的理想上界；当前 G 注入 NOT_WIRED，不可落地）；
//!    再 fit_path_firs 拟合每路径、fir_spectrum 得到实际 ĝ，after_spec[i] = e^{-jwD}H_i + sum_j H_j ĝ_ji；
//!    另用理想 g（不拟合 FIR）直接算 after 作为上界；
//! 4) 每输入每 1/3 oct 带（63/100/125/250）用 decay::evaluate_decay 求 T20/电平/EDC100 与 I；
//! 5) 输出 t20_bands.csv / t20_summary.csv / summary.json / COMMANDS.md。
//!
//! 诚实边界：全部产物 PREDICTED_ONLY / SINGLE_POINT_NOT_SPATIAL / NO_MEASUREMENT；
//! 250 Hz = DATA_NOT_AVAILABLE（H 共同带止于 129.019 Hz），125 Hz = PARTIAL（带上沿截断）；
//! 不外推、不写 PASS（离线预测无法证明实测改善）。

use std::fs;
use std::path::{Path, PathBuf};

use khaen_mimo_design::decay::{
    candidate_b_target_with_anchor, evaluate_decay, fir_spectrum, fit_path_firs, ir_of_spectrum,
    path_caps, solve_with_caps, spectrum_of_ir, Grid, MatrixMode, PathFir, M1_BANDS,
    T20_TARGET_RATIO,
};
use khaen_mimo_design::fill::{db_amp, SinglePointH};
use khaen_mimo_design::fir::fir_response;
use khaen_mimo_design::solve::Cx;

const FS: f64 = 12_000.0;
const NFFT: usize = 32_768;
const DELAY_SAMPLES: usize = 256;
const DEFAULT_TAPS: [usize; 4] = [2048, 4096, 8192, 16_384];
const DEFAULT_DESIGN_POINTS: usize = 100;
const T20_DROP_TARGET: f64 = 0.15;
const I_TARGET_DB: f64 = 3.0;
const REPORT_I_CAP_DB: f64 = 60.0;
const SCHEMA: &str = "tspi-mimo-single-point-t20/v1";
const CLAIM: &str = "PREDICTED_ONLY / SINGLE_POINT_NOT_SPATIAL / NO_MEASUREMENT";
const DEFAULT_OUT: &str = "evidence/calib-chain-20261121/mimo-fill-solver-20261122/t20";
const MODES: [MatrixMode; 2] = [MatrixMode::Diagonal, MatrixMode::Full5x5];
/// 测量 H 是 fr_modal aligned_segment、D0=0 口径：20–129 Hz 带限重建的 IR 有 ~±10 ms 预振铃，
/// 在 t=0 处会发生圆周回绕（实测 FL/SUB1 峰落在 32765/32719）。声明一个**公共** 2048 样本（171 ms）
/// 对齐延迟后再重建，使所有通道 IR 完整落在记录内；T20/I 对公共延迟不变。
const ALIGN_SAMPLES: usize = 2048;
/// 频带两端 raised-cosine 锥（20→24 Hz 上升、120→129.019 Hz 下降）：避免硬截断 sinc 长尾
/// 把 EDC 到 -25 dB 的时刻拖到记录末端（Lead 复核要求）。
const TAPER_RISE_LO_HZ: f64 = 20.0;
const TAPER_RISE_HI_HZ: f64 = 24.0;
const TAPER_FALL_LO_HZ: f64 = 120.0;
/// 对角路径正则化：g = rhs·conj(H)/(|H|² + ε)，ε = 1e-6·mean|H|²（避免 |H|≈0 处除零爆炸）。
const DIAG_EPS_REL: f64 = 1e-6;
/// level_change guard：带电平变化超过 ±3 dB 视为数值爆炸 → INFEASIBLE，不给数当结果。
const LEVEL_GUARD_DB: f64 = 3.0;
/// 当前 12k 引擎（MIMO12K_MAX_TAPS=4096, Q=256）可交付的 taps 上限；超过即 NOT_FEASIBLE_CURRENT_ENGINE。
const ENGINE_MAX_TAPS: usize = 4096;
/// I 评估带（M1 §3：63–250；250 DATA_NOT_AVAILABLE → 实际 63–125）与 log 点数。
const I_FREQ_LO_HZ: f64 = 63.0;
const I_FREQ_HI_HZ: f64 = 125.0;
const I_POINTS: usize = 40;

struct Args {
    h: PathBuf,
    out: PathBuf,
    taps: Vec<usize>,
    design_points: usize,
    /// 基线 T20 口径："predicted"（重建 IR 估计，设计自洽）或
    /// "measured63"（63 Hz 用 acoustic-verify 实测锚点 T20=0.556 s；measured-anchor 敏感性）。
    anchor: String,
}

fn parse_args() -> Result<Args, String> {
    let mut h: Option<PathBuf> = None;
    let mut out = PathBuf::from(DEFAULT_OUT);
    let mut taps = DEFAULT_TAPS.to_vec();
    let mut design_points = DEFAULT_DESIGN_POINTS;
    let mut anchor = "predicted".to_string();
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let mut take = |name: &str| -> Result<String, String> {
            arguments
                .next()
                .ok_or_else(|| format!("{name} 缺少参数值"))
        };
        match argument.as_str() {
            "--h" => h = Some(PathBuf::from(take("--h")?)),
            "--out" => out = PathBuf::from(take("--out")?),
            "--taps" => {
                taps = take("--taps")?
                    .split(',')
                    .map(|text| {
                        text.trim()
                            .parse::<usize>()
                            .map_err(|error| format!("--taps {text:?} 非法：{error}"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if taps.is_empty() {
                    return Err("--taps 为空".to_string());
                }
            }
            "--anchor" => {
                anchor = take("--anchor")?;
                if anchor != "predicted" && anchor != "measured63" {
                    return Err(format!("--anchor {anchor:?} 只支持 predicted|measured63"));
                }
            }
            "--design-points" => {
                design_points = take("--design-points")?
                    .trim()
                    .parse::<usize>()
                    .map_err(|error| format!("--design-points 非法：{error}"))?;
                if design_points < 8 {
                    return Err("--design-points 至少 8".to_string());
                }
            }
            other => return Err(format!("未知参数 {other:?}")),
        }
    }
    Ok(Args {
        h: h.ok_or_else(|| "缺少 --h".to_string())?,
        out,
        taps,
        design_points,
        anchor,
    })
}

// ---------------------------------------------------------------------------
// 频率插值（ln f 上对 ln|H| 与 unwrap 相位线性插值；带外=0，不外推）
// ---------------------------------------------------------------------------

struct ChannelCurve {
    ln_freqs: Vec<f64>,
    ln_mag: Vec<f64>,
    phase: Vec<f64>,
    data_hi_hz: f64,
}

impl ChannelCurve {
    fn from_host(host: &SinglePointH, source: usize) -> Result<Self, String> {
        let h = host
            .h
            .get(source)
            .ok_or_else(|| format!("H 缺通道 {source}"))?;
        if h.is_empty() {
            return Err("H 通道为空".to_string());
        }
        let mut phase = Vec::with_capacity(h.len());
        let mut previous = h[0].arg();
        phase.push(previous);
        for value in &h[1..] {
            let mut current = value.arg();
            while current - previous > std::f64::consts::PI {
                current -= std::f64::consts::TAU;
            }
            while previous - current > std::f64::consts::PI {
                current += std::f64::consts::TAU;
            }
            phase.push(current);
            previous = current;
        }
        Ok(Self {
            ln_freqs: host.freqs_hz.iter().map(|f| f.ln()).collect(),
            ln_mag: h.iter().map(|v| v.abs().max(1e-30).ln()).collect(),
            phase,
            data_hi_hz: *host.freqs_hz.last().unwrap(),
        })
    }

    /// 频带两端 raised-cosine 锥（20→24 Hz 上升、120→129.019 Hz 下降）；带内 1，边界 0。
    fn taper_weight(&self, freq_hz: f64) -> f64 {
        let mut weight = 1.0f64;
        if freq_hz < TAPER_RISE_HI_HZ {
            let t = ((freq_hz - TAPER_RISE_LO_HZ) / (TAPER_RISE_HI_HZ - TAPER_RISE_LO_HZ))
                .clamp(0.0, 1.0);
            weight = weight.min(0.5 * (1.0 - (std::f64::consts::PI * t).cos()));
        }
        if freq_hz > TAPER_FALL_LO_HZ {
            let t = ((self.data_hi_hz - freq_hz) / (self.data_hi_hz - TAPER_FALL_LO_HZ))
                .clamp(0.0, 1.0);
            weight = weight.min(0.5 * (1.0 - (std::f64::consts::PI * t).cos()));
        }
        weight
    }

    /// 只在测量带内插值；带外返回 None（调用方置 0，禁止外推）。
    fn sample_inside(&self, freq_hz: f64) -> Option<Cx> {
        if !(freq_hz.is_finite() && freq_hz > 0.0) {
            return None;
        }
        let ln_f = freq_hz.ln();
        let first = self.ln_freqs[0];
        let last = *self.ln_freqs.last().unwrap();
        if ln_f < first - 1e-12 || ln_f > last + 1e-12 {
            return None;
        }
        let weight = self.taper_weight(freq_hz);
        if weight <= 0.0 {
            return Some(Cx::zero());
        }
        if ln_f <= first {
            return Some(mag_phase(self.ln_mag[0], self.phase[0]).scale(weight));
        }
        if ln_f >= last {
            let index = self.ln_freqs.len() - 1;
            return Some(mag_phase(self.ln_mag[index], self.phase[index]).scale(weight));
        }
        let high = self
            .ln_freqs
            .partition_point(|value| *value <= ln_f)
            .min(self.ln_freqs.len() - 1);
        let low = high - 1;
        let span = self.ln_freqs[high] - self.ln_freqs[low];
        let t = if span.abs() > 1e-15 {
            (ln_f - self.ln_freqs[low]) / span
        } else {
            0.0
        };
        let ln_mag = (1.0 - t) * self.ln_mag[low] + t * self.ln_mag[high];
        let phase = (1.0 - t) * self.phase[low] + t * self.phase[high];
        Some(mag_phase(ln_mag, phase).scale(weight))
    }
}

fn mag_phase(ln_mag: f64, phase: f64) -> Cx {
    let magnitude = ln_mag.exp();
    Cx::new(magnitude * phase.cos(), magnitude * phase.sin())
}

fn sample_channel(curve: &ChannelCurve, freqs_hz: &[f64]) -> Vec<Cx> {
    freqs_hz
        .iter()
        .map(|f| curve.sample_inside(*f).unwrap_or_else(Cx::zero))
        .collect()
}

/// 由实 IR 在给定频点求复响应（递推相量，避免 O(n) 次 sin/cos）。
fn dft_at(ir: &[f64], freqs_hz: &[f64], fs: f64) -> Vec<Cx> {
    let mut out = Vec::with_capacity(freqs_hz.len());
    for freq in freqs_hz {
        let rotation = Cx::exp_neg_j(std::f64::consts::TAU * freq / fs);
        let mut phasor = Cx::one();
        let mut acc = Cx::zero();
        for value in ir {
            acc = acc.add(phasor.scale(*value));
            phasor = phasor.mul(rotation);
        }
        out.push(acc);
    }
    out
}

// ---------------------------------------------------------------------------
// 1/3 oct 带数据可用性（H 共同带止于 band_hi_hz；不外推）
// ---------------------------------------------------------------------------

fn band_data_note(center_hz: f64, data_hi_hz: f64) -> &'static str {
    let low = center_hz / 2.0f64.powf(1.0 / 6.0);
    let high = center_hz * 2.0f64.powf(1.0 / 6.0);
    if low > data_hi_hz + 1e-9 {
        "DATA_NOT_AVAILABLE"
    } else if high > data_hi_hz + 1e-9 {
        "PARTIAL_BAND_EDGE_TRUNCATED"
    } else {
        ""
    }
}

// ---------------------------------------------------------------------------
// 评估行
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct BandResult {
    center_hz: f64,
    t20_before_s: f64,
    t20_target_s: f64,
    t20_after_s: f64,
    /// T20 下降比例（= 1 - after/before；与 RT60 下降比例相同）。
    t20_drop: f64,
    rt60_before_s: f64,
    rt60_target_s: f64,
    rt60_after_s: f64,
    pass_15pct: bool,
    valid: bool,
    i_db: f64,
    level_change_db: f64,
    edc100_guard_db: f64,
    note: String,
}

fn band_results(
    grid: &Grid,
    role: &str,
    before_spectrum: &[Cx],
    target_spectrum: &[Cx],
    after_spectrum: &[Cx],
    data_hi_hz: f64,
) -> Vec<BandResult> {
    let rows = evaluate_decay(
        grid,
        role,
        before_spectrum,
        target_spectrum,
        after_spectrum,
        &M1_BANDS,
        data_hi_hz,
    );
    rows.into_iter()
        .map(|row| {
            let data_note = band_data_note(row.center_hz, data_hi_hz);
            let available = data_note.is_empty();
            let valid = row.valid && available;
            let pass_15pct = valid && row.t20_drop.is_finite() && row.t20_drop >= T20_DROP_TARGET;
            let note = if !data_note.is_empty() {
                data_note.to_string()
            } else if !row.valid {
                if row.note.is_empty() {
                    "T20_NOT_ESTIMABLE".to_string()
                } else {
                    row.note.to_string()
                }
            } else {
                String::new()
            };
            BandResult {
                center_hz: row.center_hz,
                t20_before_s: row.t20_before_s,
                t20_target_s: row.t20_target_s,
                t20_after_s: row.t20_after_s,
                t20_drop: row.t20_drop,
                // RT60 = 3*T20（只报告；T20 为 EDC -5..-25 dB 线性拟合的 20 dB 衰减时间）。
                rt60_before_s: 3.0 * row.t20_before_s,
                rt60_target_s: 3.0 * row.t20_target_s,
                rt60_after_s: 3.0 * row.t20_after_s,
                pass_15pct,
                valid,
                i_db: row.i_db,
                level_change_db: row.level_change_db,
                edc100_guard_db: row.edc100_guard_db,
                note,
            }
        })
        .collect()
}

/// 逐输入 I（M1 §4.3 口径的可用段版本）：I = 10log10(E_off/E_on)，在 63–125 Hz 的 40 个
/// log 等距点、且该输入 |H|² > ε（不近零）的点上计算；E_off 近零（目标=基线）→ N/A。
#[derive(Clone, Copy, Debug)]
struct IMetric {
    i_db: f64,
    points_used: usize,
    e_off_rel: f64,
}

fn i_metrics(
    grid: &Grid,
    before: &[Vec<Cx>],
    target: &[Vec<Cx>],
    after: &[Vec<Cx>],
    diag_eps: &[f64],
) -> Vec<IMetric> {
    let m = before.len();
    let ratio = (I_FREQ_HI_HZ / I_FREQ_LO_HZ).powf(1.0 / (I_POINTS as f64 - 1.0));
    let bins: Vec<usize> = (0..I_POINTS)
        .map(|index| {
            let freq = I_FREQ_LO_HZ * ratio.powi(index as i32);
            ((freq / grid.fs * grid.nfft as f64).round() as usize).min(grid.bins() - 1)
        })
        .collect();
    (0..m)
        .map(|input| {
            let mut e_off = 0.0f64;
            let mut e_on = 0.0f64;
            let mut scale = 0.0f64;
            let mut used = 0usize;
            for bin in &bins {
                // before/target/after 均为 [input][bin] 布局。
                let h2 = before[input][*bin].norm_sqr();
                if h2 <= diag_eps[input] {
                    continue; // |H| 近零点不参与 I（避免除零/拟合爆炸）
                }
                e_off += before[input][*bin].sub(target[input][*bin]).norm_sqr();
                e_on += after[input][*bin].sub(target[input][*bin]).norm_sqr();
                scale += h2;
                used += 1;
            }
            let e_off_rel = if scale > 0.0 { e_off / scale } else { f64::NAN };
            let i_db = if used == 0 || !(scale > 0.0) || e_off <= 1e-12 * scale {
                f64::NAN // N/A：目标≈基线（E_off 近零），不给无限/伪负值
            } else if e_on <= 1e-30 * scale {
                f64::INFINITY
            } else {
                let value = 10.0 * (e_off / e_on.max(1e-300)).log10();
                value.min(REPORT_I_CAP_DB)
            };
            IMetric {
                i_db,
                points_used: used,
                e_off_rel,
            }
        })
        .collect()
}

fn append_note(note: &mut String, extra: &str) {
    if note.is_empty() {
        note.push_str(extra);
    } else {
        note.push(';');
        note.push_str(extra);
    }
}

/// level_change guard：任何带 |after/before RMS dB| > LEVEL_GUARD_DB 视为数值爆炸 → 不可交付。
fn apply_level_guard(rows: &mut [BandResult]) -> bool {
    let mut ok = true;
    for row in rows.iter_mut() {
        if !(row.level_change_db.is_finite() && row.level_change_db.abs() <= LEVEL_GUARD_DB) {
            ok = false;
            append_note(&mut row.note, "LEVEL_CHANGE_GUARD");
        }
    }
    ok
}

fn cap_i_db(value: f64) -> f64 {
    if value.is_finite() {
        value.min(REPORT_I_CAP_DB)
    } else if value == f64::INFINITY {
        REPORT_I_CAP_DB
    } else {
        f64::NAN
    }
}

fn clip_mag(value: Cx, cap: f64) -> Cx {
    let magnitude = value.abs();
    if magnitude > cap && magnitude > 0.0 {
        value.scale(cap / magnitude)
    } else {
        value
    }
}

// ---------------------------------------------------------------------------
// 理想修正（逐频点）：设计频点与全网格两用
// ---------------------------------------------------------------------------

fn design_g_paths(
    m: usize,
    mode: MatrixMode,
    h_design: &[Vec<Cx>],
    d_design: &[Vec<Cx>],
    rot_design: &[Cx],
    caps: &[Vec<f64>],
    diag_eps: &[f64],
) -> Vec<Vec<Vec<Cx>>> {
    let k = rot_design.len();
    let mut paths: Vec<Vec<Vec<Cx>>> = vec![vec![vec![Cx::zero(); k]; m]; m];
    match mode {
        MatrixMode::Diagonal => {
            let cap = 10.0f64.powf(6.0 / 20.0);
            for input in 0..m {
                for bin in 0..k {
                    let h = h_design[input][bin];
                    let rhs = rot_design[bin].mul(d_design[input][bin].sub(h));
                    // 正则化逆：g = rhs·conj(H)/(|H|²+ε)，ε=1e-6·mean|H|²；之后再做 +6 dB 幅值帽。
                    let denominator = h.norm_sqr() + diag_eps[input];
                    let g = if denominator > 0.0 {
                        rhs.mul(h.conj()).scale(1.0 / denominator)
                    } else {
                        Cx::zero()
                    };
                    paths[input][input][bin] = clip_mag(g, cap);
                }
            }
        }
        MatrixMode::Full5x5 => {
            for input in 0..m {
                for bin in 0..k {
                    let h_vec: Vec<Cx> = (0..m).map(|source| h_design[source][bin]).collect();
                    let rhs = rot_design[bin].mul(d_design[input][bin].sub(h_vec[input]));
                    let caps_row: Vec<f64> = (0..m).map(|source| caps[source][input]).collect();
                    let g = solve_with_caps(&h_vec, rhs, &caps_row);
                    for source in 0..m {
                        paths[source][input][bin] = g[source];
                    }
                }
            }
        }
    }
    paths
}

fn cap_stats(
    m: usize,
    paths: &[Vec<Vec<Cx>>],
    caps: &[Vec<f64>],
) -> (f64, usize) {
    let mut max_magnitude = 0.0f64;
    let mut hits = 0usize;
    for source in 0..m {
        for input in 0..m {
            let cap = caps[source][input];
            if cap <= 0.0 {
                continue;
            }
            for value in &paths[source][input] {
                let magnitude = value.abs();
                max_magnitude = max_magnitude.max(magnitude);
                if magnitude > cap + 1e-9 {
                    hits += 1;
                }
            }
        }
    }
    (db_amp(max_magnitude), hits)
}

fn fit_g_paths(
    m: usize,
    mode: MatrixMode,
    paths: &[Vec<Vec<Cx>>],
    design_freqs_hz: &[f64],
    taps: usize,
) -> Result<Vec<PathFir>, String> {
    match mode {
        MatrixMode::Diagonal => {
            let mut out = Vec::with_capacity(m);
            for input in 0..m {
                let single = vec![vec![paths[input][input].clone()]];
                let mut fitted = fit_path_firs(design_freqs_hz, &single, FS, taps)?;
                let mut path = fitted
                    .pop()
                    .ok_or_else(|| "fit_path_firs 未返回路径".to_string())?;
                path.out_index = input;
                path.in_index = input;
                out.push(path);
            }
            Ok(out)
        }
        MatrixMode::Full5x5 => fit_path_firs(design_freqs_hz, paths, FS, taps),
    }
}

fn ideal_after_grid(
    m: usize,
    bins: usize,
    mode: MatrixMode,
    baseline_spec: &[Vec<Cx>],
    target_spec: &[Vec<Cx>],
    rot_grid: &[Cx],
    caps: &[Vec<f64>],
    diag_eps: &[f64],
) -> Vec<Vec<Cx>> {
    let mut after = vec![vec![Cx::zero(); bins]; m];
    match mode {
        MatrixMode::Diagonal => {
            let cap = 10.0f64.powf(6.0 / 20.0);
            for input in 0..m {
                for bin in 0..bins {
                    let h = baseline_spec[input][bin];
                    let rhs = rot_grid[bin].mul(target_spec[input][bin].sub(h));
                    let denominator = h.norm_sqr() + diag_eps[input];
                    let g = if denominator > 0.0 {
                        clip_mag(rhs.mul(h.conj()).scale(1.0 / denominator), cap)
                    } else {
                        Cx::zero()
                    };
                    after[input][bin] = rot_grid[bin].mul(h).add(h.mul(g));
                }
            }
        }
        MatrixMode::Full5x5 => {
            for input in 0..m {
                for bin in 0..bins {
                    let h_vec: Vec<Cx> = (0..m).map(|source| baseline_spec[source][bin]).collect();
                    let rhs = rot_grid[bin].mul(target_spec[input][bin].sub(h_vec[input]));
                    let caps_row: Vec<f64> = (0..m).map(|source| caps[source][input]).collect();
                    let g = solve_with_caps(&h_vec, rhs, &caps_row);
                    let mut value = rot_grid[bin].mul(h_vec[input]);
                    for source in 0..m {
                        value = value.add(h_vec[source].mul(g[source]));
                    }
                    after[input][bin] = value;
                }
            }
        }
    }
    after
}

fn after_from_path_firs(
    m: usize,
    bins: usize,
    baseline_spec: &[Vec<Cx>],
    g_hat: &[Vec<Vec<Cx>>],
    rot_grid: &[Cx],
) -> Vec<Vec<Cx>> {
    let mut after = vec![vec![Cx::zero(); bins]; m];
    for input in 0..m {
        for bin in 0..bins {
            let mut value = rot_grid[bin].mul(baseline_spec[input][bin]);
            for source in 0..m {
                let g = g_hat[source][input].get(bin).copied().unwrap_or_else(Cx::zero);
                value = value.add(baseline_spec[source][bin].mul(g));
            }
            after[input][bin] = value;
        }
    }
    after
}

// ---------------------------------------------------------------------------
// 输出辅助
// ---------------------------------------------------------------------------

fn write_text(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("建目录失败: {error}"))?;
    }
    fs::write(path, text).map_err(|error| format!("写 {} 失败: {error}", path.display()))
}

fn write_csv(path: &Path, header: &str, rows: &[String]) -> Result<(), String> {
    let mut text = String::new();
    text.push_str(header);
    text.push('\n');
    for row in rows {
        text.push_str(row);
        text.push('\n');
    }
    write_text(path, &text)
}

fn js(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn jf(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.6}")
    } else {
        "null".to_string()
    }
}

fn jb(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

fn band_json(row: &BandResult) -> String {
    format!(
        "{{\"band_hz\":{:.1},\"t20_before_s\":{},\"t20_target_s\":{},\"t20_after_s\":{},\"t20_drop\":{},\"rt60_before_s\":{},\"rt60_target_s\":{},\"rt60_after_s\":{},\"pass_15pct\":{},\"valid\":{},\"i_db\":{},\"level_change_db\":{},\"edc100_guard_db\":{},\"note\":{}}}",
        row.center_hz,
        jf(row.t20_before_s),
        jf(row.t20_target_s),
        jf(row.t20_after_s),
        jf(row.t20_drop),
        jf(row.rt60_before_s),
        jf(row.rt60_target_s),
        jf(row.rt60_after_s),
        jb(row.pass_15pct),
        jb(row.valid),
        jf(cap_i_db(row.i_db)),
        jf(row.level_change_db),
        jf(row.edc100_guard_db),
        js(&row.note)
    )
}

fn bands_json(rows: &[BandResult]) -> String {
    format!(
        "[{}]",
        rows.iter().map(band_json).collect::<Vec<_>>().join(",")
    )
}

fn band_csv_row(mode: &str, taps: usize, role: &str, row: &BandResult) -> String {
    format!(
        "{},{},{},{:.1},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{},{},{:.3},{:.3},{:.3},{}",
        mode,
        taps,
        role,
        row.center_hz,
        row.t20_before_s,
        row.t20_target_s,
        row.t20_after_s,
        row.t20_drop,
        row.rt60_before_s,
        row.rt60_target_s,
        row.rt60_after_s,
        row.pass_15pct,
        row.valid,
        row.i_db,
        row.level_change_db,
        row.edc100_guard_db,
        row.note
    )
}

#[derive(Clone, Debug)]
struct Cost {
    paths: usize,
    plan_bytes: usize,
    mac_per_sec: u64,
    support_ms: f64,
}

fn cost_of(mode: MatrixMode, taps: usize, sources: usize) -> Cost {
    let paths = match mode {
        MatrixMode::Diagonal => sources,
        MatrixMode::Full5x5 => sources * sources,
    };
    Cost {
        paths,
        plan_bytes: taps * paths * 4,
        mac_per_sec: paths as u64 * taps as u64 * FS as u64,
        support_ms: taps as f64 / FS * 1000.0,
    }
}

/// 逐 (mode,taps,role) 状态；只允许 PREDICTED_* / NOT_FEASIBLE_*，不写 PASS。
fn family_status(
    engine_feasible: bool,
    numeric_ok: bool,
    valid_bands: usize,
    pass: bool,
    i_db: f64,
) -> &'static str {
    if !engine_feasible {
        "PREDICTED_NOT_FEASIBLE_CURRENT_ENGINE"
    } else if !numeric_ok {
        "PREDICTED_INFEASIBLE_NUMERIC"
    } else if valid_bands == 0 {
        "PREDICTED_DATA_INSUFFICIENT"
    } else if pass && i_db.is_finite() && i_db >= I_TARGET_DB {
        "PREDICTED_PARTIAL"
    } else {
        "PREDICTED_INCOMPLETE"
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("mimo single-point t20: ERROR: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let host = SinglePointH::load_csv(&args.h)?;
    let m = host.n_sources();
    if m == 0 {
        return Err("H 没有通道".to_string());
    }
    let h_sha = khaen_room_sim::sha256::file_hex(&args.h)
        .map_err(|error| format!("H sha256 失败: {error}"))?;
    // measured-anchor 敏感性：63 Hz 用 acoustic-verify 2026-11-22 identity 实测 T20（同房间/同点；
    // 3 次 0.556/0.558/0.561 s，取 0.556 s → RT60=1.668 s）。其它带锚点不可用（100 无实测锚，
    // 125 PARTIAL/250 DATA_NOT_AVAILABLE）→ 仍用重建 IR 估计。
    let measured_anchor = args.anchor == "measured63";
    let measured_anchor_t20_s: Option<f64> = if measured_anchor { Some(0.556) } else { None };
    let t20_anchors: Vec<Option<f64>> = M1_BANDS
        .iter()
        .map(|band| {
            if (band.center_hz - 63.0).abs() < 1e-9 {
                measured_anchor_t20_s
            } else {
                None
            }
        })
        .collect();
    let out_dir = if measured_anchor {
        args.out.join("anchor-measured63")
    } else {
        args.out.clone()
    };
    fs::create_dir_all(&out_dir).map_err(|error| format!("建 out 目录失败: {error}"))?;

    let data_lo_hz = *host.freqs_hz.first().unwrap();
    let data_hi_hz = *host.freqs_hz.last().unwrap();
    let grid = Grid::new(FS, NFFT);
    let bins = grid.bins();
    let d_ms = DELAY_SAMPLES as f64 / FS * 1000.0;

    // ---- 1) H → 12k/32768 网格 + baseline IR ----
    // 两端 raised-cosine 锥 + 公共对齐延迟 ALIGN_SAMPLES：硬截断 sinc 长尾与 t=0 圆周回绕都会
    // 伪造 T20（Lead 复核：曾出现 8.0 s / 峰落在记录末尾），必须先做这两步再重建 IR。
    let curves: Vec<ChannelCurve> = (0..m)
        .map(|source| ChannelCurve::from_host(&host, source))
        .collect::<Result<Vec<_>, _>>()?;
    let align_rot_grid: Vec<Cx> = grid
        .freqs_hz
        .iter()
        .map(|f| Cx::exp_neg_j(std::f64::consts::TAU * f * ALIGN_SAMPLES as f64 / FS))
        .collect();
    let baseline_spec: Vec<Vec<Cx>> = curves
        .iter()
        .map(|curve| {
            sample_channel(curve, &grid.freqs_hz)
                .iter()
                .zip(align_rot_grid.iter())
                .map(|(value, rot)| rot.mul(*value))
                .collect()
        })
        .collect();
    let baseline_ir: Vec<Vec<f64>> = baseline_spec
        .iter()
        .map(|spectrum| ir_of_spectrum(spectrum, NFFT))
        .collect();

    // ---- 2) 候选 B 目标（decay.rs；T*=0.85T）----
    let mut target_ir: Vec<Vec<f64>> = Vec::with_capacity(m);
    let mut target_infos = Vec::with_capacity(m);
    for source in 0..m {
        let (ir, infos) = candidate_b_target_with_anchor(
            &baseline_ir[source],
            &grid,
            &M1_BANDS,
            data_hi_hz,
            &t20_anchors,
        );
        target_ir.push(ir);
        target_infos.push(infos);
    }
    let target_spec: Vec<Vec<Cx>> = target_ir.iter().map(|ir| spectrum_of_ir(ir)).collect();

    // ---- 3) 设计频点（20–129 Hz 上 log 等距）与设计频点响应 ----
    let design_points = args.design_points.max(2);
    let design_freqs: Vec<f64> = (0..design_points)
        .map(|index| {
            let ratio = (data_hi_hz / data_lo_hz).powf(index as f64 / (design_points - 1) as f64);
            data_lo_hz * ratio
        })
        .collect();
    let align_rot_design: Vec<Cx> = design_freqs
        .iter()
        .map(|f| Cx::exp_neg_j(std::f64::consts::TAU * f * ALIGN_SAMPLES as f64 / FS))
        .collect();
    let h_design: Vec<Vec<Cx>> = curves
        .iter()
        .map(|curve| {
            sample_channel(curve, &design_freqs)
                .iter()
                .zip(align_rot_design.iter())
                .map(|(value, rot)| rot.mul(*value))
                .collect()
        })
        .collect();
    // 对角路径正则化 ε_i = 1e-6·mean|H_i|²（设计频点上；Lead 复核要求避免 |H|≈0 除零）。
    let diag_eps: Vec<f64> = (0..m)
        .map(|source| {
            let mean = h_design[source]
                .iter()
                .map(|value| value.norm_sqr())
                .sum::<f64>()
                / design_freqs.len().max(1) as f64;
            DIAG_EPS_REL * mean.max(1e-30)
        })
        .collect();
    let d_design: Vec<Vec<Cx>> = target_ir
        .iter()
        .map(|ir| dft_at(ir, &design_freqs, FS))
        .collect();
    let rot_design: Vec<Cx> = design_freqs
        .iter()
        .map(|f| Cx::exp_neg_j(std::f64::consts::TAU * f * DELAY_SAMPLES as f64 / FS))
        .collect();
    let rot_grid: Vec<Cx> = grid
        .freqs_hz
        .iter()
        .map(|f| Cx::exp_neg_j(std::f64::consts::TAU * f * DELAY_SAMPLES as f64 / FS))
        .collect();
    // 评估用公共延迟帧：before/target 乘 e^{-jwD}，与物理 after 同帧（I 只差公共单位相位，T20 平移不变）。
    let before_eval: Vec<Vec<Cx>> = baseline_spec
        .iter()
        .map(|spectrum| {
            spectrum
                .iter()
                .zip(rot_grid.iter())
                .map(|(value, rot)| rot.mul(*value))
                .collect()
        })
        .collect();
    let target_eval: Vec<Vec<Cx>> = target_spec
        .iter()
        .map(|spectrum| {
            spectrum
                .iter()
                .zip(rot_grid.iter())
                .map(|(value, rot)| rot.mul(*value))
                .collect()
        })
        .collect();

    // ---- 诊断（stdout；不写证据数字）----
    println!("mimo single-point t20: {CLAIM}");
    println!(
        "H: {} channels={m} points={} {:.3}-{:.3} Hz sha256={h_sha}",
        args.h.display(),
        host.len(),
        data_lo_hz,
        data_hi_hz
    );
    println!(
        "grid: fs={FS} nfft={NFFT} bins={bins} duration={:.3}s; D={DELAY_SAMPLES} ({d_ms:.3} ms); design_points={design_points}",
        NFFT as f64 / FS
    );
    for source in 0..m {
        let ir = &baseline_ir[source];
        let peak = ir
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .map(|(index, _)| index)
            .unwrap_or(0);
        let early: f64 = ir
            .iter()
            .take(4096)
            .map(|v| v * v)
            .sum::<f64>();
        let total: f64 = ir.iter().map(|v| v * v).sum::<f64>();
        let before_bands: Vec<String> = target_infos[source]
            .iter()
            .map(|info| {
                let note = band_data_note(info.center_hz, data_hi_hz);
                format!(
                    "{}Hz T20={:.4}s valid={}{}",
                    info.center_hz,
                    info.t20_before_s,
                    info.valid,
                    if note.is_empty() {
                        String::new()
                    } else {
                        format!(" [{note}]")
                    }
                )
            })
            .collect();
        println!(
            "baseline {}: ir_peak_idx={peak} ({:.3}s) early_energy={:.4} | {}",
            host.roles[source],
            peak as f64 / FS,
            if total > 0.0 { early / total } else { f64::NAN },
            before_bands.join("; ")
        );
    }

    // ---- baseline 行（每通道一次；t20_before/t20_target 口径）----
    let baseline_rows: Vec<Vec<BandResult>> = (0..m)
        .map(|input| {
            band_results(
                &grid,
                &host.roles[input],
                &before_eval[input],
                &target_eval[input],
                &before_eval[input],
                data_hi_hz,
            )
        })
        .collect();

    // ---- 4) 两口径 × taps 档 ----
    let header_bands = "mode,taps,role,band_hz,t20_before_s,t20_target_s,t20_after_s,t20_drop,rt60_before_s,rt60_target_s,rt60_after_s,pass_15pct,valid,i_db,level_change_db,edc100_guard_db,note";
    let header_summary = "mode,taps,role,valid_bands,min_t20_drop,max_i_db,cost_plan_bytes,mac_per_sec,status,d_samples,d_ms,support_ms,worst_level_change_db,worst_edc100_guard_db";
    let mut band_csv: Vec<String> = Vec::new();
    let mut summary_csv: Vec<String> = Vec::new();
    let mut run_json: Vec<String> = Vec::new();
    let mut ideal_json: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    // mode,taps,valid_band_rows,pass15_band_rows,min_drop,min_i,max_i,numeric_ok,worst_level,worst_edc,engine_feasible
    let mut combo_rows: Vec<(String, usize, usize, usize, f64, f64, f64, bool, f64, f64, bool)> = Vec::new();

    let mut ideal_rows_by_mode: Vec<Vec<Vec<BandResult>>> = Vec::new();
    let mut ideal_i_by_mode: Vec<Vec<f64>> = Vec::new();
    let mut ideal_json_per_mode: Vec<String> = Vec::new();

    for (mode_index, mode) in MODES.iter().enumerate() {
        let mode_tag = mode.tag();
        let caps = path_caps(*mode, m);
        let ideal_after = ideal_after_grid(
            m,
            bins,
            *mode,
            &baseline_spec,
            &target_spec,
            &rot_grid,
            &caps,
            &diag_eps,
        );
        let ideal_i_rows = i_metrics(&grid, &before_eval, &target_eval, &ideal_after, &diag_eps);
        let mut mode_ideal_rows: Vec<Vec<BandResult>> = Vec::with_capacity(m);
        let mut mode_ideal_i: Vec<f64> = Vec::with_capacity(m);
        let mut mode_ideal_json: Vec<String> = Vec::with_capacity(m);
        for input in 0..m {
            let mut rows = band_results(
                &grid,
                &host.roles[input],
                &before_eval[input],
                &target_eval[input],
                &ideal_after[input],
                data_hi_hz,
            );
            let i_metric = ideal_i_rows[input];
            for row in rows.iter_mut() {
                row.i_db = i_metric.i_db;
            }
            mode_ideal_i.push(i_metric.i_db);
            mode_ideal_rows.push(rows.clone());
            mode_ideal_json.push(format!(
                "{{\"role\":{},\"i_db\":{},\"i_points_used\":{},\"e_off_rel\":{},\"bands\":{}}}",
                js(&host.roles[input]),
                jf(cap_i_db(i_metric.i_db)),
                i_metric.points_used,
                jf(i_metric.e_off_rel),
                bands_json(&rows)
            ));
        }
        // ideal = 目标由理想 g 直接达成的自证上界（SELF_REFERENCE）：只作上界一列，不是结果。
        ideal_json_per_mode.push(format!(
            "{{\"mode\":{},\"note\":\"SELF_REFERENCE ideal_g_no_fir_upper_bound (目标由理想 g 直接达成，不是可交付结果)\",\"roles\":[{}]}}",
            js(mode_tag),
            mode_ideal_json.join(",")
        ));
        ideal_rows_by_mode.push(mode_ideal_rows);
        ideal_i_by_mode.push(mode_ideal_i);

        for taps in &args.taps {
            let g_paths = design_g_paths(
                m,
                *mode,
                &h_design,
                &d_design,
                &rot_design,
                &caps,
                &diag_eps,
            );
            let (max_ideal_gain_db, cap_hits) = cap_stats(m, &g_paths, &caps);
            let firs = match fit_g_paths(m, *mode, &g_paths, &design_freqs, *taps) {
                Ok(firs) => firs,
                Err(error) => {
                    let message = format!("mode={mode_tag} taps={taps}: fit_path_firs 失败: {error}");
                    println!("DESIGN_FAILED: {message}");
                    failures.push(message);
                    continue;
                }
            };
            // 拟合质量：实际 FIR 在设计频点 vs 理想 g（相对误差 dB）。
            let mut fit_num = 0.0f64;
            let mut fit_den = 0.0f64;
            for path in &firs {
                let ideal = &g_paths[path.out_index][path.in_index];
                let actual = fir_response(&path.coeffs, &design_freqs, FS);
                for bin in 0..design_freqs.len() {
                    fit_num += actual[bin].sub(ideal[bin]).norm_sqr();
                    fit_den += ideal[bin].norm_sqr();
                }
            }
            let fit_error_db = if fit_den > 1e-30 {
                10.0 * (fit_num.max(1e-30) / fit_den).log10()
            } else {
                f64::NAN
            };
            let mut g_hat: Vec<Vec<Vec<Cx>>> = vec![vec![Vec::new(); m]; m];
            for path in &firs {
                g_hat[path.out_index][path.in_index] = fir_spectrum(&path.coeffs, NFFT);
            }
            let after_real = after_from_path_firs(m, bins, &baseline_spec, &g_hat, &rot_grid);
            let cost = cost_of(*mode, *taps, m);
            let mut valid_total = 0usize;
            let mut pass_total = 0usize;
            let mut min_drop_total = f64::INFINITY;
            let mut min_i_total = f64::INFINITY;
            let mut max_i_total = f64::NEG_INFINITY;
            let mut numeric_ok_total = true;
            let mut worst_level_total = 0.0f64;
            let mut worst_edc_total = 0.0f64;
            let engine_feasible = *taps <= ENGINE_MAX_TAPS;
            let i_rows = i_metrics(&grid, &before_eval, &target_eval, &after_real, &diag_eps);
            for input in 0..m {
                let role = &host.roles[input];
                let mut rows = band_results(
                    &grid,
                    role,
                    &before_eval[input],
                    &target_eval[input],
                    &after_real[input],
                    data_hi_hz,
                );
                let i_metric = i_rows[input];
                for row in rows.iter_mut() {
                    row.i_db = i_metric.i_db;
                }
                let numeric_ok = apply_level_guard(&mut rows);
                let worst_level = rows
                    .iter()
                    .map(|row| {
                        if row.level_change_db.is_finite() {
                            row.level_change_db.abs()
                        } else {
                            f64::INFINITY
                        }
                    })
                    .fold(0.0f64, f64::max);
                let worst_edc = rows
                    .iter()
                    .map(|row| {
                        if row.edc100_guard_db.is_finite() {
                            row.edc100_guard_db.abs()
                        } else {
                            f64::INFINITY
                        }
                    })
                    .fold(0.0f64, f64::max);
                numeric_ok_total &= numeric_ok;
                worst_level_total = worst_level_total.max(worst_level);
                worst_edc_total = worst_edc_total.max(worst_edc);
                let i_db = i_metric.i_db;
                let valid_bands = rows.iter().filter(|row| row.valid).count();
                let pass_bands = rows.iter().filter(|row| row.pass_15pct).count();
                let min_drop = rows
                    .iter()
                    .filter(|row| row.valid)
                    .map(|row| row.t20_drop)
                    .fold(f64::INFINITY, f64::min);
                let min_drop = if min_drop.is_finite() { min_drop } else { f64::NAN };
                let pass = valid_bands > 0
                    && rows
                        .iter()
                        .filter(|row| row.valid)
                        .all(|row| row.t20_drop.is_finite() && row.t20_drop >= T20_DROP_TARGET);
                let status = family_status(engine_feasible, numeric_ok, valid_bands, pass, i_db);
                valid_total += valid_bands;
                pass_total += pass_bands;
                min_drop_total = min_drop_total.min(min_drop);
                if i_db.is_finite() {
                    min_i_total = min_i_total.min(i_db);
                    max_i_total = max_i_total.max(i_db);
                }
                for row in &rows {
                    band_csv.push(band_csv_row(mode_tag, *taps, role, row));
                }
                summary_csv.push(format!(
                    "{},{},{},{},{:.6},{:.3},{},{},{},{},{:.3},{:.3},{:.3},{:.3}",
                    mode_tag,
                    taps,
                    role,
                    valid_bands,
                    min_drop,
                    cap_i_db(i_db),
                    cost.plan_bytes,
                    cost.mac_per_sec,
                    status,
                    DELAY_SAMPLES,
                    d_ms,
                    cost.support_ms,
                    worst_level,
                    worst_edc
                ));
                run_json.push(format!(
                    "{{\"mode\":{},\"taps\":{},\"role\":{},\"i_db\":{},\"i_points_used\":{},\"e_off_rel\":{},\"valid_bands\":{},\"min_t20_drop\":{},\"status\":{},\"engine_feasible\":{},\"numeric_ok\":{},\"fit_error_db\":{},\"max_ideal_gain_db\":{},\"cap_hits\":{},\"worst_level_change_db\":{},\"worst_edc100_guard_db\":{},\"cost\":{{\"paths\":{},\"plan_bytes\":{},\"mac_per_sec\":{},\"d_samples\":{},\"d_ms\":{},\"support_ms\":{}}},\"bands\":{},\"ideal_bands_SELF_REFERENCE\":{},\"ideal_i_db_SELF_REFERENCE\":{}}}",
                    js(mode_tag),
                    taps,
                    js(role),
                    jf(cap_i_db(i_db)),
                    i_metric.points_used,
                    jf(i_metric.e_off_rel),
                    valid_bands,
                    jf(min_drop),
                    js(status),
                    jb(engine_feasible),
                    jb(numeric_ok),
                    jf(fit_error_db),
                    jf(max_ideal_gain_db),
                    cap_hits,
                    jf(worst_level),
                    jf(worst_edc),
                    cost.paths,
                    cost.plan_bytes,
                    cost.mac_per_sec,
                    DELAY_SAMPLES,
                    jf(d_ms),
                    jf(cost.support_ms),
                    bands_json(&rows),
                    bands_json(&ideal_rows_by_mode[mode_index][input]),
                    jf(cap_i_db(ideal_i_by_mode[mode_index][input]))
                ));
            }
            let min_drop_total = if min_drop_total.is_finite() {
                min_drop_total
            } else {
                f64::NAN
            };
            let min_i_total = if min_i_total.is_finite() {
                min_i_total
            } else {
                f64::NAN
            };
            let max_i_total = if max_i_total.is_finite() {
                max_i_total
            } else {
                f64::NAN
            };
            combo_rows.push((
                mode_tag.to_string(),
                *taps,
                valid_total,
                pass_total,
                min_drop_total,
                min_i_total,
                max_i_total,
                numeric_ok_total,
                worst_level_total,
                worst_edc_total,
                engine_feasible,
            ));
        }
    }
    for entry in ideal_json_per_mode {
        ideal_json.push(entry);
    }

    // ---- 5) 计数与判定（只 PREDICTED_PARTIAL / PREDICTED_INCOMPLETE；不写 PASS）----
    let role_runs = 2 * args.taps.len() * m;
    let band_rows_total = role_runs * M1_BANDS.len();
    let valid_band_rows = band_csv
        .iter()
        .filter(|row| row.split(',').nth(12) == Some("true"))
        .count();
    let pass_band_rows = band_csv
        .iter()
        .filter(|row| row.split(',').nth(11) == Some("true"))
        .count();
    let mut i_ge3_role_runs = 0usize;
    for row in &summary_csv {
        let fields: Vec<&str> = row.split(',').collect();
        if let Some(value) = fields.get(5).and_then(|text| text.parse::<f64>().ok()) {
            if value.is_finite() && value >= I_TARGET_DB {
                i_ge3_role_runs += 1;
            }
        }
    }
    let any_pass = pass_band_rows > 0;
    let any_i = i_ge3_role_runs > 0;
    let mut n_partial = 0usize;
    let mut n_incomplete = 0usize;
    let mut n_data_insufficient = 0usize;
    let mut n_infeasible = 0usize;
    let mut n_not_feasible_engine = 0usize;
    for row in &summary_csv {
        let fields: Vec<&str> = row.split(',').collect();
        match fields.get(8).copied() {
            Some("PREDICTED_PARTIAL") => n_partial += 1,
            Some("PREDICTED_DATA_INSUFFICIENT") => n_data_insufficient += 1,
            Some("PREDICTED_INFEASIBLE_NUMERIC") => n_infeasible += 1,
            Some("PREDICTED_NOT_FEASIBLE_CURRENT_ENGINE") => n_not_feasible_engine += 1,
            _ => n_incomplete += 1,
        }
    }
    let verdict = if n_partial > 0 {
        "PREDICTED_PARTIAL"
    } else {
        "PREDICTED_INCOMPLETE"
    };
    let verdict_detail = format!(
        "role_runs={role_runs}: PREDICTED_PARTIAL={n_partial}, PREDICTED_INCOMPLETE={n_incomplete}, PREDICTED_DATA_INSUFFICIENT={n_data_insufficient}, PREDICTED_INFEASIBLE_NUMERIC={n_infeasible}, PREDICTED_NOT_FEASIBLE_CURRENT_ENGINE={n_not_feasible_engine}；离线预测（无实测/无 apply/无空间），不写 PASS"
    );
    println!(
        "verdict: {verdict} band_rows={band_rows_total} valid_band_rows={valid_band_rows} pass15_band_rows={pass_band_rows} i>=3dB_role_runs={i_ge3_role_runs}/{role_runs}"
    );
    println!("status_counts: {verdict_detail}");

    // ---- 6) 写 CSV ----
    write_csv(
        &out_dir.join("t20_bands.csv"),
        header_bands,
        &band_csv,
    )?;
    write_csv(
        &out_dir.join("t20_summary.csv"),
        header_summary,
        &summary_csv,
    )?;

    // ---- 7) summary.json ----
    let baseline_json: String = (0..m)
        .map(|input| {
            format!(
                "{{\"role\":{},\"bands\":{}}}",
                js(&host.roles[input]),
                bands_json(&baseline_rows[input])
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let combo_json: String = combo_rows
        .iter()
        .map(
            |(mode, taps, valid, pass, min_drop, min_i, max_i, numeric_ok, worst_level, worst_edc, engine)| {
                format!(
                    "{{\"mode\":{},\"taps\":{},\"valid_band_rows\":{},\"pass15_band_rows\":{},\"min_t20_drop\":{},\"min_i_db\":{},\"max_i_db\":{},\"numeric_ok\":{},\"worst_level_change_db\":{},\"worst_edc100_guard_db\":{},\"engine_feasible\":{}}}",
                    js(mode),
                    taps,
                    valid,
                    pass,
                    jf(*min_drop),
                    jf(cap_i_db(*min_i)),
                    jf(cap_i_db(*max_i)),
                    jb(*numeric_ok),
                    jf(*worst_level),
                    jf(*worst_edc),
                    jb(*engine)
                )
            },
        )
        .collect::<Vec<_>>()
        .join(",");
    let anchor_tag = if measured_anchor {
        "measured_anchor_63"
    } else {
        "predicted_baseline"
    };
    let anchor_t20 = match measured_anchor_t20_s {
        Some(value) => jf(value),
        None => "null".to_string(),
    };
    let config_json = format!(
        "{{\"fs_hz\":{FS},\"nfft\":{NFFT},\"bins\":{bins},\"delay_samples\":{DELAY_SAMPLES},\"delay_ms\":{:.6},\"align_samples\":{ALIGN_SAMPLES},\"align_ms\":{:.6},\"taps\":[{}],\"design_points\":{design_points},\"design_freq_lo_hz\":{:.6},\"design_freq_hi_hz\":{:.6},\"bands_hz\":[63.0,100.0,125.0,250.0],\"data_lo_hz\":{:.6},\"data_hi_hz\":{:.6},\"anchor\":\"{anchor_tag}\",\"anchor_measured_t20_s\":{anchor_t20},\"taper\":\"raised-cosine 20-24 Hz rise, 120-data_hi fall (avoid hard-truncation sinc tail)\",\"t20_definition\":\"20 dB decay time from EDC -5..-25 dB linear fit (T20=-20/slope); RT60=3*T20\",\"t20_valid_s\":[0.02,1.5],\"edc_window_s\":2.0,\"t20_target_ratio\":{},\"t20_drop_target\":{},\"i_target_db\":{},\"i_band_hz\":[63.0,125.0],\"i_points\":{I_POINTS},\"i_report_cap_db\":{},\"i_gate\":\"per-input |H|^2 > 1e-6*mean|H_i|^2 (design freqs)\",\"diag_cap_db\":6.0,\"offdiag_cap_db\":-6.0,\"diag_eps_rel\":{DIAG_EPS_REL},\"level_guard_db\":{LEVEL_GUARD_DB},\"engine_max_taps\":{ENGINE_MAX_TAPS},\"interp\":\"ln-f linear in ln|H| and unwrapped phase; zero outside common band (no extrapolation)\",\"phase_reference\":\"fr_modal aligned_segment D0=0 as-given; declared common ALIGN_SAMPLES delay applied before IR reconstruction so band-limited IRs are causal\",\"candidate_b_note\":\"target band T20 measured after reconstruction may deviate from nominal 0.85T (candidate B is a time-domain decay subtraction, not an exact T20 scaling)\"}}",
        d_ms,
        ALIGN_SAMPLES as f64 / FS * 1000.0,
        args.taps
            .iter()
            .map(|taps| taps.to_string())
            .collect::<Vec<_>>()
            .join(","),
        data_lo_hz,
        data_hi_hz,
        data_lo_hz,
        data_hi_hz,
        T20_TARGET_RATIO,
        T20_DROP_TARGET,
        I_TARGET_DB,
        REPORT_I_CAP_DB
    );
    let counts_json = format!(
        "{{\"role_runs\":{role_runs},\"band_rows\":{band_rows_total},\"valid_band_rows\":{valid_band_rows},\"pass15_band_rows\":{pass_band_rows},\"i_ge3_role_runs\":{i_ge3_role_runs},\"status_partial\":{n_partial},\"status_incomplete\":{n_incomplete},\"status_data_insufficient\":{n_data_insufficient},\"status_infeasible_numeric\":{n_infeasible},\"status_not_feasible_engine\":{n_not_feasible_engine},\"i_target_met\":{}}}",
        jb(any_i)
    );
    let command_line: Vec<String> = std::env::args().skip(1).collect();
    let summary = format!(
        "{{\n  \"schema\": {},\n  \"claim\": {},\n  \"anchor\": {},\n  \"anchor_measured_t20_s\": {},\n  \"verdict\": {},\n  \"verdict_detail\": {},\n  \"input\": {{\"h_csv\": {}, \"h_sha256\": {}, \"channels\": {}, \"roles\": [{}], \"points\": {}, \"freq_lo_hz\": {:.6}, \"freq_hi_hz\": {:.6}}},\n  \"grid\": {{\"fs_hz\": {FS}, \"nfft\": {NFFT}, \"bins\": {bins}, \"duration_s\": {:.6}}},\n  \"config\": {},\n  \"modes\": {{\"diagonal_realizable\": \"T66 P_post diagonal FIR (production-expressible)\", \"full5x5_ideal_upper_bound\": \"T51/T54 modal matrix form; T54 G injection NOT_WIRED (not deployable)\"}},\n  \"baseline\": [{}],\n  \"ideal_SELF_REFERENCE\": [{}],\n  \"runs\": [{}],\n  \"combos\": [{}],\n  \"counts\": {},\n  \"failures\": [{}],\n  \"command\": [{}]\n}}\n",
        js(SCHEMA),
        js(CLAIM),
        js(anchor_tag),
        anchor_t20,
        js(verdict),
        js(&verdict_detail),
        js(&args.h.display().to_string().replace('\\', "/")),
        js(&h_sha),
        m,
        host.roles.iter().map(|role| js(role)).collect::<Vec<_>>().join(","),
        host.len(),
        data_lo_hz,
        data_hi_hz,
        NFFT as f64 / FS,
        config_json,
        baseline_json,
        ideal_json.join(","),
        run_json.join(","),
        combo_json,
        counts_json,
        failures
            .iter()
            .map(|message| js(message))
            .collect::<Vec<_>>()
            .join(","),
        command_line
            .iter()
            .map(|argument| js(argument))
            .collect::<Vec<_>>()
            .join(",")
    );
    write_text(&out_dir.join("summary.json"), &summary)?;

    // ---- 8) COMMANDS.md ----
    let commands = format!(
        "# 复现命令（单点 MIMO 候选 B T20 预测；纯离线）\n\n\
         \x20   cd C:/WorkSpace/Claw/projects/tspi-audio-dsp/rt-stack\n\n\
         \x20   # 1) 单测（decay 模块；含 taps>4096 的 fit_path_firs 回退路径与 T20 定义）\n\
         \x20   cargo test --offline -p khaen-mimo-design --lib\n\n\
         \x20   # 2) 构建 CLI\n\
         \x20   cargo build --offline --release -p khaen-mimo-design --example single_point_t20\n\n\
         \x20   # 3) 跑真实 t4b 单点 H：predicted-baseline 口径（设计自洽）→ t20/\n\
         \x20   ./target/release/examples/single_point_t20.exe --h evidence/calib-chain-20261121/mimo-fill-solver-20261122/h/H_complex.csv --out evidence/calib-chain-20261121/mimo-fill-solver-20261122/t20 --anchor predicted\n\n\
         \x20   # 4) measured-anchor 敏感性：63 Hz 用 acoustic-verify 实测 T20=0.556 s → t20/anchor-measured63/\n\
         \x20   ./target/release/examples/single_point_t20.exe --h evidence/calib-chain-20261121/mimo-fill-solver-20261122/h/H_complex.csv --out evidence/calib-chain-20261121/mimo-fill-solver-20261122/t20 --anchor measured63\n\n\
         说明：\n\
         - 输入 H sha256 = {}\n\
         - 纯离线：不打开设备、不写 plan、不 apply、不出声；无实测、无 holdout、无空间信息。\n\
         - 全部产物 CLAIM = {}\n\
         - T20 定义（mimo-t6 §4.5 / acoustic-verify lf_edc 一致）：带通 IR 扣噪声底 → EDC，在 EDC ∈ [-5,-25] dB 线性拟合，\n\
         T20 = -20/slope（20 dB 衰减时间，s）；RT60 = 3×T20（只报告）。旧式 3*(t25-t5) 是 RT60，已改。\n\
         - 数据带 20–129.019 Hz（H 共同有效带）；250 Hz = DATA_NOT_AVAILABLE；125 Hz 带 111.4–140.3 被截断 → PARTIAL_BAND_EDGE_TRUNCATED，不计通过；主判带 63/100 Hz。\n\
         - 重建：ln-f 插值 + 两端 raised-cosine 锥（20–24 Hz 上升 / 120–{:.3} Hz 下降）+ 声明公共对齐延迟 2048 样本（171 ms）；\n\
         测量 H 是 fr_modal aligned_segment、D0=0，带限重建 IR 在 t≈0 会圆周回绕（实测 FL/SUB1 峰 32765/32719），必须先对齐。\n\
         - T20 有效性：EDC 需在 ≤2.0 s 内到 -25 dB、R²≥0.90、T20∈[0.02,1.5] s，否则 NaN + EDC_DYNAMIC_RANGE_INSUFFICIENT/FIT_R2_TOO_LOW（不输出 8 s 伪值）。\n\
         - 对角 g 用正则化逆 g=rhs·conj(H)/(|H|²+ε)、ε=1e-6·mean|H|²，再 +6 dB 幅值帽；I 只在 63–125 Hz 且 |H| 不近零的 40 个 log 点上算；level_change 超 ±3 dB 判 INFEASIBLE。\n\
         - Diagonal = T66 P_post 对角 FIR（生产可表达）；Full5x5 = T51/T54 modal matrix 理想上界（T54 G 注入 NOT_WIRED，不可落地）。\n\
         - 8192/16384 taps：当前 12k 引擎（MIMO12K_MAX_TAPS=4096，Q=256）不可交付 → NOT_FEASIBLE_CURRENT_ENGINE（12k 下 16384 taps=1.365 s 支撑）。\n\
         - decay.rs 最小修复：fit_path_firs 增补 >4096 档同一 dual 公式本地实现（h=Aᵀ(AAᵀ+μI)⁻¹c）；t20_from_ir 改为线性拟合 20 dB 时间 + 噪声扣除 + 有效性。\n\
         - ideal 是 SELF_REFERENCE（目标由理想 g 直接达成）上界，不是可交付结果；表只报 FIR-realized。\n\
         - predicted baseline 与实测 baseline 不可直接比较（t4b H 为 fr_modal 分析产物，apparent decay 偏短；63 Hz 约差 2.6×）→ 绝对 T20 不外推，drop 只作相对预测。\n",
        h_sha,
        CLAIM,
        data_hi_hz
    );
    write_text(&out_dir.join("COMMANDS.md"), &commands)?;

    // ---- 9) stdout 关键数字 ----
    for (mode, taps, valid, pass, min_drop, min_i, max_i, numeric_ok, worst_level, worst_edc, engine) in &combo_rows {
        println!(
            "combo {mode} taps={taps}: engine_feasible={engine} numeric_ok={numeric_ok} valid_band_rows={valid} pass15_band_rows={pass} min_t20_drop={:.4} min_i_db={:.3} max_i_db={:.3} worst_level_db={:.3} worst_edc_guard_db={:.3}",
            min_drop, min_i, max_i, worst_level, worst_edc
        );
    }
    println!("out: {}", out_dir.display());
    println!("files: t20_bands.csv t20_summary.csv summary.json COMMANDS.md");
    println!("next: REPORT-T20.md（人工撰写；表从 CSV/JSON 转录，不汇总掩盖最差点）");
    Ok(())
}
