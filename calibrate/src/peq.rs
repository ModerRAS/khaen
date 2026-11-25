//! RBJ 双二阶与校正拟合。目标是实测 + ΣH ≈ 目标曲线。
use num_complex::Complex;

use crate::modal;
use crate::model::{FrPoint, Peq};
use crate::target::Curve;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Peaking,
    LowShelf,
    HighShelf,
}

#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    pub kind: Kind,
    pub freq: f64,
    pub q: f64,
    pub gain_db: f64,
}

impl Biquad {
    pub fn new(kind: Kind, freq: f64, q: f64, gain_db: f64) -> Self {
        Self {
            kind,
            freq,
            q: if q.is_finite() { q.max(0.1) } else { 0.1 },
            gain_db: if gain_db.is_finite() {
                gain_db.clamp(-24.0, 24.0)
            } else {
                0.0
            },
        }
    }

    pub fn from_peq(p: &Peq) -> Self {
        let kind = match p.filter_type.as_str() {
            "low_shelf" => Kind::LowShelf,
            "high_shelf" => Kind::HighShelf,
            _ => Kind::Peaking,
        };
        Self::new(kind, p.freq, p.q, p.gain_db)
    }

    pub fn h_at(&self, f: f64, fs: f64) -> Complex<f64> {
        if self.gain_db.abs() < 1e-6 || f <= 0.0 || fs <= 0.0 {
            return Complex::new(1.0, 0.0);
        }
        let a = 10f64.powf(self.gain_db / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * self.freq / fs;
        let (b0, b1, b2, a0, a1, a2) = match self.kind {
            Kind::Peaking => {
                let alpha = w0.sin() / (2.0 * self.q);
                (
                    1.0 + alpha * a,
                    -2.0 * w0.cos(),
                    1.0 - alpha * a,
                    1.0 + alpha / a,
                    -2.0 * w0.cos(),
                    1.0 - alpha / a,
                )
            }
            Kind::LowShelf | Kind::HighShelf => {
                let alpha = shelf_alpha(w0, a, self.q);
                shelf_coeffs(self.kind, a, w0, alpha)
            }
        };
        eval_ba(b0, b1, b2, a0, a1, a2, f, fs)
    }

    pub fn db_at(&self, f: f64, fs: f64) -> f64 {
        20.0 * self.h_at(f, fs).norm().max(1e-12).log10()
    }
}

fn shelf_alpha(w0: f64, a: f64, q: f64) -> f64 {
    let q = q.max(0.1);
    let inside = (a + 1.0 / a) * (1.0 / q - 1.0) + 2.0;
    w0.sin() / 2.0 * inside.max(0.0).sqrt()
}

fn shelf_coeffs(kind: Kind, a: f64, w0: f64, alpha: f64) -> (f64, f64, f64, f64, f64, f64) {
    let cos = w0.cos();
    let sq = 2.0 * a.sqrt() * alpha;
    if kind == Kind::LowShelf {
        (
            a * ((a + 1.0) - (a - 1.0) * cos + sq),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
            a * ((a + 1.0) - (a - 1.0) * cos - sq),
            (a + 1.0) + (a - 1.0) * cos + sq,
            -2.0 * ((a - 1.0) + (a + 1.0) * cos),
            (a + 1.0) + (a - 1.0) * cos - sq,
        )
    } else {
        (
            a * ((a + 1.0) + (a - 1.0) * cos + sq),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
            a * ((a + 1.0) + (a - 1.0) * cos - sq),
            (a + 1.0) - (a - 1.0) * cos + sq,
            2.0 * ((a - 1.0) - (a + 1.0) * cos),
            (a + 1.0) - (a - 1.0) * cos - sq,
        )
    }
}

fn eval_ba(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64, f: f64, fs: f64) -> Complex<f64> {
    let w = 2.0 * std::f64::consts::PI * f / fs;
    let z1 = Complex::new(w.cos(), -w.sin());
    let z2 = z1 * z1;
    let num = b0 + b1 * z1 + b2 * z2;
    let den = a0 + a1 * z1 + a2 * z2;
    if den.norm() < 1e-18 {
        Complex::new(1.0, 0.0)
    } else {
        num / den
    }
}

pub fn combined_h(stages: &[Biquad], f: f64, fs: f64) -> Complex<f64> {
    let mut h = Complex::new(1.0, 0.0);
    for b in stages {
        h *= b.h_at(f, fs);
    }
    h
}

pub fn combined_db(stages: &[Biquad], f: f64, fs: f64) -> f64 {
    20.0 * combined_h(stages, f, fs).norm().max(1e-12).log10()
}

pub fn stages_from(filters: &[Peq]) -> Vec<Biquad> {
    filters.iter().map(Biquad::from_peq).collect()
}

/// 固定点数滑动平均。检测不用它；保留给旧调用。
pub fn smooth(fr: &[FrPoint], half_window: usize) -> Vec<FrPoint> {
    let mut out = Vec::with_capacity(fr.len());
    for i in 0..fr.len() {
        let lo = i.saturating_sub(half_window);
        let hi = (i + half_window + 1).min(fr.len());
        let mut db = 0.0;
        for p in &fr[lo..hi] {
            db += p.db;
        }
        db /= (hi - lo) as f64;
        out.push(FrPoint {
            freq_hz: fr[i].freq_hz,
            db,
            phase_deg: fr[i].phase_deg,
        });
    }
    out
}

#[derive(Clone, Debug)]
pub struct WeightedFr {
    pub name: String,
    pub weight: f64,
    pub fr: Vec<FrPoint>,
}

#[derive(Clone, Debug)]
pub struct OptimizeOpts {
    pub fs: f64,
    pub target: Curve,
    pub op_lo: f64,
    pub op_hi: f64,
    pub boost_cap_db: f64,
    pub modal_lo: f64,
    pub modal_hi: f64,
    pub wide_lo: f64,
    pub wide_hi: f64,
    /// P0-2：20–60 Hz 低频段是否可信（带内 SNR/噪声底足够）；false 时不生成低频模态滤波器。
    pub low_band_trusted: bool,
    /// D7：headroom 预测用的“节目峰值参考电平”（dBFS）。
    /// predicted_peak = headroom_reference_dbfs + 最大累计滤波增益（1 kHz 归一）。
    /// 默认 -18 dBFS：与 D7 总正向帽 12 dB 配套（-18+12=-6），并允许默认 +3 dB boost。
    pub headroom_reference_dbfs: f64,
}

impl OptimizeOpts {
    pub fn standard(fs: f64, target: Curve) -> Self {
        Self {
            fs,
            target,
            op_lo: 20.0,
            op_hi: 20_000.0,
            // D7（2026-11-17）：默认允许 +3 dB boost（opt-in cut-only 用 --boost-cap-db 0）
            boost_cap_db: 3.0,
            modal_lo: 20.0,
            modal_hi: 250.0,
            wide_lo: 80.0,
            wide_hi: 16_000.0,
            low_band_trusted: true,
            headroom_reference_dbfs: -18.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PredRow {
    pub freq_hz: f64,
    pub target_db: f64,
    pub predicted_db: f64,
    pub residual_db: f64,
}

#[derive(Clone, Debug)]
pub struct OptimizeReport {
    pub filters: Vec<Peq>,
    pub modal: Vec<Peq>,
    pub broadband: Vec<Peq>,
    pub rms: f64,
    pub max_abs: f64,
    pub warnings: Vec<String>,
    pub modal_confidence: String,
    pub rows: Vec<PredRow>,
    /// D7：正向增益滤波器（gain_db>0）。
    pub boost_filters: Vec<Peq>,
    /// 单段最大正向增益（应有 ≤3 dB 硬帽）。
    pub applied_boost_db: f64,
    /// 每通道正向增益总和（不含 channel trim）。
    pub total_boost_db: f64,
    /// 最大累计滤波增益（相对 1 kHz，dB）。
    pub max_predicted_gain_db: f64,
    /// 预测峰值（= headroom_reference_dbfs + max_predicted_gain_db，dBFS）。
    pub predicted_peak_dbfs: f64,
    /// 相对 0 dBFS 的余量：-predicted_peak_dbfs。
    pub headroom_db: f64,
    /// D7：headroom<6 dB / 单段>3 / 正向总和>12 任一成立即 true。
    pub clip_risk: bool,
    /// 预测所用节目峰值参考电平（dBFS，报告可追溯）。
    pub headroom_reference_dbfs: f64,
}

pub fn optimize(positions: &[WeightedFr], opts: &OptimizeOpts) -> OptimizeReport {
    let mut warnings = Vec::new();
    let mut boost_cap = opts.boost_cap_db;
    if boost_cap > 3.0 {
        warnings.push(format!(
            "boost-cap-db {boost_cap:.2} 超过 +3，按 +3 dB 执行"
        ));
        boost_cap = 3.0;
    }
    if boost_cap < 0.0 {
        boost_cap = 0.0;
    }
    let usable: Vec<&WeightedFr> = positions
        .iter()
        .filter(|p| p.weight > 0.0 && !p.fr.is_empty())
        .collect();
    if usable.is_empty() {
        warnings.push("没有可用测点".into());
        return OptimizeReport {
            filters: vec![],
            modal: vec![],
            broadband: vec![],
            rms: 0.0,
            max_abs: 0.0,
            warnings,
            modal_confidence: "low".into(),
            rows: vec![],
            boost_filters: vec![],
            applied_boost_db: 0.0,
            total_boost_db: 0.0,
            max_predicted_gain_db: 0.0,
            predicted_peak_dbfs: opts.headroom_reference_dbfs,
            headroom_db: -opts.headroom_reference_dbfs,
            clip_risk: false,
            headroom_reference_dbfs: opts.headroom_reference_dbfs,
        };
    }
    let modal_band = intersect(opts.modal_lo, opts.modal_hi, opts.op_lo, opts.op_hi);
    let wide_band = intersect(opts.wide_lo, opts.wide_hi, opts.op_lo, opts.op_hi);
    let confidence = if usable.len() < 2 { "low" } else { "high" };
    if usable.len() < 2 {
        warnings.push("只有一个测点，模态置信度 low".into());
    }

    let modal_src: Vec<(f64, &[FrPoint])> =
        usable.iter().map(|p| (p.weight, p.fr.as_slice())).collect();
    let modal_fr = average_fr(&modal_src, false, true);
    let pos_frs: Vec<Vec<FrPoint>> = usable.iter().map(|p| p.fr.clone()).collect();
    let modes = if let Some(band) = modal_band {
        modal::detect(&modal_fr, band, &pos_frs)
    } else {
        Vec::new()
    };
    let mut modal_filters = Vec::new();
    // P0-2：模态+宽带的总衰减（负增益之和）<= 18 dB，防止 10 个 -12 dB 段叠成过度修正；
    // 模态段数 <= 10。剩余预算不足以覆盖整段时按剩余预算部分施加，再停止。
    let mut cut_sum = 0.0f64;
    for m in &modes {
        if modal_filters.len() >= 10 {
            warnings.push("模态段数超过 10，截断".into());
            break;
        }
        if m.q_unresolved {
            // q_unresolved 一律不写滤波器：只报告，避免把“窄于 FR 网格”的伪 Q 写进校准。
            warnings.push(format!(
                "q_unresolved freq={:.2} q={:.2}（窄于频响网格，不写滤波器）",
                m.freq, m.q
            ));
            continue;
        }
        if m.freq <= 60.0 {
            if !opts.low_band_trusted {
                warnings.push(format!(
                    "20–60 Hz 频段不可信（带内 SNR/噪声底不足），跳过模态 {:.1} Hz",
                    m.freq
                ));
                continue;
            }
            // 多测点时必须可重现（>=2 个测点在该频率有 >=3 dB 峰）；
            // 单测点无法验证重现性，保留原行为（只受 low_band_trusted + prominence 约束）。
            if usable.len() >= 2 {
                let reproducible = usable
                    .iter()
                    .filter(|p| position_has_peak(&p.fr, m.freq, 3.0))
                    .count();
                if reproducible < 2 {
                    warnings.push(format!(
                        "20–60 Hz 模态 {:.1} Hz 不可重现（仅 {} 个测点有 >=3 dB 峰），不写滤波器",
                        m.freq, reproducible
                    ));
                    continue;
                }
            }
        }
        let q_clamped = m.q.clamp(0.1, 10.0);
        if (q_clamped - m.q).abs() > 1e-9 {
            warnings.push(format!(
                "模态 {:.1} Hz Q={:.2} 钳到 10（channelmap 上限）",
                m.freq, m.q
            ));
        }
        let remaining = (18.0 - cut_sum).max(0.0);
        if remaining < 0.2 {
            warnings.push("模态总 cut 已达 18 dB，停止追加模态段".into());
            break;
        }
        let g = m.gain_db.max(-remaining);
        cut_sum += -g;
        modal_filters.push(Peq::peaking(
            (m.freq * 10.0).round() / 10.0,
            (q_clamped * 100.0).round() / 100.0,
            (g * 100.0).round() / 100.0,
        ));
    }

    let wide_src: Vec<(f64, &[FrPoint])> = modal_src;
    let wide_fr = modal::smooth_oct(&average_fr(&wide_src, true, false), 1.0 / 6.0);
    let mut broadband = Vec::new();
    if let Some(band) = wide_band {
        broadband = fit_broadband(
            &wide_fr,
            opts,
            band,
            &modal_filters,
            boost_cap,
            &mut cut_sum,
            &mut warnings,
        );
    }
    let mut filters = modal_filters.clone();
    filters.extend(broadband.iter().cloned());
    filters.sort_by(|a, b| {
        a.freq
            .partial_cmp(&b.freq)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if filters.len() > 22 {
        warnings.push(format!("合并后 {} 段，截断到 22", filters.len()));
        filters.truncate(22);
    }
    let (rms, max_abs, rows) = predict(&wide_fr, &filters, opts);
    // D7：boost/headroom 统计与预测（以 1 kHz 为 0 dB 参考，参考节目峰值可追溯）
    let boost_filters: Vec<Peq> = filters
        .iter()
        .filter(|p| p.gain_db > 0.0)
        .cloned()
        .collect();
    let applied_boost_db = boost_filters
        .iter()
        .map(|p| p.gain_db)
        .fold(0.0f64, f64::max);
    let total_boost_db: f64 = boost_filters.iter().map(|p| p.gain_db).sum();
    let total_boost_db = if total_boost_db == 0.0 {
        0.0
    } else {
        total_boost_db
    }; // 避免 -0.0
    let bs = stages_from(&filters);
    let g_1k = combined_db(&bs, 1000.0, opts.fs);
    let mut max_gain: f64 = f64::NEG_INFINITY;
    let mut f = 20.0f64;
    while f <= 20_000.0 {
        max_gain = max_gain.max(combined_db(&bs, f, opts.fs) - g_1k);
        f *= 2.0f64.powf(1.0 / 24.0);
    }
    if !max_gain.is_finite() {
        max_gain = 0.0;
    }
    let predicted_peak_dbfs = opts.headroom_reference_dbfs + max_gain;
    let headroom_db = -predicted_peak_dbfs;
    let mut clip_risk = headroom_db < 6.0 - 1e-9;
    if applied_boost_db > 3.0 + 1e-9 {
        clip_risk = true;
        warnings.push(format!(
            "D7 违规：单段 boost {:.2} dB > +3 dB（producer 硬帽）",
            applied_boost_db
        ));
    }
    if total_boost_db > 12.0 + 1e-9 {
        clip_risk = true;
        warnings.push(format!(
            "D7 拒绝：每通道正向总和 {:.2} dB > 12 dB",
            total_boost_db
        ));
    } else if total_boost_db > 6.0 + 1e-9 {
        warnings.push(format!(
            "D7 warning：每通道正向总和 {:.2} dB > 6 dB",
            total_boost_db
        ));
    }
    if headroom_db < 6.0 - 1e-9 {
        warnings.push(format!(
            "D7 削波风险：预测峰值 {:.2} dBFS（参考 {:.1} dBFS + 最大累计增益 {:.2} dB），headroom {:.2} dB < 6 dB",
            predicted_peak_dbfs, opts.headroom_reference_dbfs, max_gain, headroom_db
        ));
    }
    OptimizeReport {
        filters,
        modal: modal_filters,
        broadband,
        rms,
        max_abs,
        warnings,
        modal_confidence: confidence.into(),
        rows,
        boost_filters,
        applied_boost_db,
        total_boost_db,
        max_predicted_gain_db: max_gain,
        predicted_peak_dbfs,
        headroom_db,
        clip_risk,
        headroom_reference_dbfs: opts.headroom_reference_dbfs,
    }
}

/// P0-2：测点在该频率附近是否有 >= min_prom 的峰（用于 20–60 Hz 可重现性门）。
fn position_has_peak(fr: &[FrPoint], freq: f64, min_prom: f64) -> bool {
    let lo = freq / 1.122; // ~1/6 oct
    let hi = freq * 1.122;
    let at = sample_db(fr, freq);
    let left = sample_db(fr, lo);
    let right = sample_db(fr, hi);
    at - left.max(right) >= min_prom
}

fn intersect(a0: f64, a1: f64, b0: f64, b1: f64) -> Option<(f64, f64)> {
    let lo = a0.max(b0);
    let hi = a1.min(b1);
    if hi > lo * 1.01 {
        Some((lo, hi))
    } else {
        None
    }
}

pub fn average_fr(
    parts: &[(f64, &[FrPoint])],
    power: bool,
    downweight_spread: bool,
) -> Vec<FrPoint> {
    let grid = parts
        .iter()
        .max_by_key(|(_, fr)| fr.len())
        .map(|(_, fr)| fr)
        .unwrap();
    let mut out = Vec::with_capacity(grid.len());
    for g in *grid {
        let mut wsum = 0.0;
        let mut acc = 0.0;
        let mut vals = Vec::new();
        let mut weights = Vec::new();
        for (w, fr) in parts {
            if *w <= 0.0 {
                continue;
            }
            let db = sample_db(fr, g.freq_hz);
            vals.push(db);
            weights.push(*w);
        }
        let spread = vals.iter().copied().fold(f64::MIN, f64::max)
            - vals.iter().copied().fold(f64::MAX, f64::min);
        for (db, w) in vals.iter().zip(weights.iter()) {
            let mut ww = *w;
            if downweight_spread && spread > 6.0 {
                ww *= 0.25;
            }
            let lin = if power {
                10f64.powf(db / 10.0)
            } else {
                10f64.powf(db / 20.0)
            };
            acc += ww * lin;
            wsum += ww;
        }
        let mean = if wsum > 0.0 { acc / wsum } else { 1e-12 };
        let db = if power {
            10.0 * mean.max(1e-18).log10()
        } else {
            20.0 * mean.max(1e-18).log10()
        };
        out.push(FrPoint {
            freq_hz: g.freq_hz,
            db,
            phase_deg: g.phase_deg,
        });
    }
    out
}

pub fn sample_db(fr: &[FrPoint], f: f64) -> f64 {
    if fr.is_empty() {
        return 0.0;
    }
    if f <= fr[0].freq_hz {
        return fr[0].db;
    }
    if f >= fr[fr.len() - 1].freq_hz {
        return fr[fr.len() - 1].db;
    }
    let mut lo = 0;
    let mut hi = fr.len() - 1;
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if fr[mid].freq_hz <= f {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let f0 = fr[lo].freq_hz.max(1e-9);
    let f1 = fr[hi].freq_hz.max(f0 * 1.0000001);
    let t = ((f.ln() - f0.ln()) / (f1.ln() - f0.ln())).clamp(0.0, 1.0);
    fr[lo].db + t * (fr[hi].db - fr[lo].db)
}

fn fit_broadband(
    fr: &[FrPoint],
    opts: &OptimizeOpts,
    band: (f64, f64),
    modal: &[Peq],
    boost_cap: f64,
    cut_sum: &mut f64,
    warnings: &mut Vec<String>,
) -> Vec<Peq> {
    let idx: Vec<usize> = fr
        .iter()
        .enumerate()
        .filter(|(_, p)| p.freq_hz >= band.0 && p.freq_hz <= band.1)
        .map(|(i, _)| i)
        .collect();
    if idx.is_empty() {
        return Vec::new();
    }
    let mut filters: Vec<Peq> = Vec::new();
    let mut current = vec![0.0; fr.len()];
    for m in modal {
        let b = Biquad::from_peq(m);
        for (i, p) in fr.iter().enumerate() {
            current[i] += b.db_at(p.freq_hz, opts.fs);
        }
    }
    let mut pos_sum = 0.0;
    maybe_shelves(
        fr,
        opts,
        band,
        boost_cap,
        &mut filters,
        &mut current,
        &mut pos_sum,
        cut_sum,
        warnings,
    );
    let qs = [0.3_f64, 0.5, 0.7, 1.0, 1.4, 2.0];
    let freqs = modal::log_grid(band.0, band.1, 12.0);
    for _ in 0..12 {
        if filters.len() >= 12 {
            break;
        }
        let rms = band_rms(fr, opts, &current, &idx);
        if rms < 0.5 {
            break;
        }
        let mut best: Option<(f64, f64, f64, f64)> = None;
        for &f in &freqs {
            if modal.iter().any(|m| oct_dist(m.freq, f) < 1.0 / 6.0) {
                continue;
            }
            if filters.iter().any(|p| oct_dist(p.freq, f) < 1.0 / 24.0) {
                continue;
            }
            let i = nearest(fr, f);
            let residual = opts.target.db_at(fr[i].freq_hz) - fr[i].db - current[i];
            let room = (boost_cap - pos_sum).max(0.0).min(6.0);
            let g_hi = room;
            // P0-2：宽带段与模态共享 18 dB 总衰减预算。
            let g_lo = (-(18.0 - *cut_sum)).clamp(-6.0, 0.0);
            let mut gains = vec![residual.clamp(g_lo, g_hi)];
            for scale in [0.5, 0.75, 1.25] {
                gains.push((residual * scale).clamp(g_lo, g_hi));
            }
            for q in qs {
                for g in &gains {
                    if g.abs() < 0.2 {
                        continue;
                    }
                    if *g > 0.0 && pos_sum + *g > boost_cap + 1e-6 {
                        continue;
                    }
                    let b = Biquad::new(Kind::Peaking, f, q, *g);
                    let err = idx
                        .iter()
                        .map(|&k| {
                            let e = opts.target.db_at(fr[k].freq_hz)
                                - fr[k].db
                                - current[k]
                                - b.db_at(fr[k].freq_hz, opts.fs);
                            e * e
                        })
                        .sum::<f64>()
                        / idx.len() as f64;
                    if best.map_or(true, |(_, _, _, be)| err < be) {
                        best = Some((f, q, *g, err));
                    }
                }
            }
        }
        let base = band_rms(fr, opts, &current, &idx).powi(2);
        match best {
            Some((f, q, g, err)) if err < base - 1e-4 && g.abs() >= 0.2 => {
                let b = Biquad::new(Kind::Peaking, f, q, g);
                for (i, p) in fr.iter().enumerate() {
                    current[i] += b.db_at(p.freq_hz, opts.fs);
                }
                if g > 0.0 {
                    pos_sum += g;
                } else {
                    *cut_sum += -g;
                }
                filters.push(Peq::peaking(
                    (f * 10.0).round() / 10.0,
                    (q * 100.0).round() / 100.0,
                    (g * 100.0).round() / 100.0,
                ));
            }
            _ => break,
        }
    }
    filters
}

fn maybe_shelves(
    fr: &[FrPoint],
    opts: &OptimizeOpts,
    band: (f64, f64),
    boost_cap: f64,
    filters: &mut Vec<Peq>,
    current: &mut [f64],
    pos_sum: &mut f64,
    cut_sum: &mut f64,
    _warnings: &mut Vec<String>,
) {
    let t_lo = opts.target.db_at(band.0);
    let t_hi = opts.target.db_at(band.1);
    let tilt = t_hi - t_lo;
    if tilt.abs() > 1.0 && filters.len() < 12 {
        let mut g = tilt.clamp(-6.0, 6.0);
        if g > boost_cap {
            g = boost_cap;
        }
        if g.abs() >= 0.2 {
            let f = (band.0 * band.1).sqrt();
            push_shelf(
                fr,
                opts.fs,
                filters,
                current,
                pos_sum,
                cut_sum,
                "high_shelf",
                f,
                g,
            );
        }
    }
    let low_delta = t_lo - opts.target.db_at(1000.0_f64.min(band.1));
    if band.0 < 800.0 && low_delta.abs() > 1.0 && filters.len() < 12 {
        let room = (boost_cap - *pos_sum).max(0.0);
        let mut g = low_delta.clamp(-6.0, 6.0);
        if g > room {
            g = room;
        }
        if g.abs() >= 0.2 {
            let f = (band.0 * 200.0_f64.min(band.1))
                .sqrt()
                .clamp(band.0, band.1);
            push_shelf(
                fr,
                opts.fs,
                filters,
                current,
                pos_sum,
                cut_sum,
                "low_shelf",
                f,
                g,
            );
        }
    }
}

fn push_shelf(
    fr: &[FrPoint],
    fs: f64,
    filters: &mut Vec<Peq>,
    current: &mut [f64],
    pos_sum: &mut f64,
    cut_sum: &mut f64,
    kind: &str,
    freq: f64,
    gain: f64,
) {
    // P0-2：负增益计入 18 dB 总 cut 预算，按剩余预算钳。
    let mut gain = gain;
    if gain < 0.0 {
        let room = (18.0 - *cut_sum).max(0.0);
        gain = gain.max(-room);
        if gain.abs() < 0.2 {
            return;
        }
        *cut_sum += -gain;
    }
    let bk = if kind == "low_shelf" {
        Kind::LowShelf
    } else {
        Kind::HighShelf
    };
    let b = Biquad::new(bk, freq, 0.7, gain);
    for (i, p) in fr.iter().enumerate() {
        current[i] += b.db_at(p.freq_hz, fs);
    }
    if gain > 0.0 {
        *pos_sum += gain;
    }
    filters.push(Peq::typed(
        kind,
        (freq * 10.0).round() / 10.0,
        0.7,
        (gain * 100.0).round() / 100.0,
    ));
}

fn band_rms(fr: &[FrPoint], opts: &OptimizeOpts, current: &[f64], idx: &[usize]) -> f64 {
    let mean = idx
        .iter()
        .map(|&i| {
            let e = opts.target.db_at(fr[i].freq_hz) - fr[i].db - current[i];
            e * e
        })
        .sum::<f64>()
        / idx.len().max(1) as f64;
    mean.sqrt()
}

fn nearest(fr: &[FrPoint], f: f64) -> usize {
    let mut bi = 0;
    let mut bd = f64::MAX;
    for (i, p) in fr.iter().enumerate() {
        let d = (p.freq_hz - f).abs();
        if d < bd {
            bd = d;
            bi = i;
        }
    }
    bi
}

fn oct_dist(a: f64, b: f64) -> f64 {
    (a.max(1e-9) / b.max(1e-9)).ln().abs() / std::f64::consts::LN_2
}

fn predict(fr: &[FrPoint], filters: &[Peq], opts: &OptimizeOpts) -> (f64, f64, Vec<PredRow>) {
    let bs = stages_from(filters);
    let mut rows = Vec::with_capacity(fr.len());
    let mut acc = 0.0;
    let mut max_abs = 0.0f64;
    let mut n = 0.0;
    for p in fr {
        if p.freq_hz < 30.0 || p.freq_hz > 16_000.0 {
            continue;
        }
        let filt = combined_db(&bs, p.freq_hz, opts.fs);
        let target = opts.target.db_at(p.freq_hz);
        let predicted = p.db + filt;
        let residual = predicted - target;
        acc += residual * residual;
        max_abs = max_abs.max(residual.abs());
        n += 1.0;
        rows.push(PredRow {
            freq_hz: p.freq_hz,
            target_db: target,
            predicted_db: predicted,
            residual_db: residual,
        });
    }
    let rms = if n > 0.0 { (acc / n).sqrt() } else { 0.0 };
    (rms, max_abs, rows)
}

pub fn smoothed_residual_rms(rows: &[PredRow], octaves: f64) -> f64 {
    if rows.is_empty() {
        return 0.0;
    }
    let half = octaves / 2.0;
    let ln2 = std::f64::consts::LN_2;
    let mut acc = 0.0;
    for r in rows {
        let mut s = 0.0;
        let mut n = 0.0;
        for q in rows {
            let d = (q.freq_hz.ln() - r.freq_hz.ln()).abs() / ln2;
            if d <= half {
                s += q.residual_db;
                n += 1.0;
            }
        }
        let e = if n > 0.0 { s / n } else { r.residual_db };
        acc += e * e;
    }
    (acc / rows.len() as f64).sqrt()
}

/// 匹配组：两条实测差过大时，用平均频响再拟合一次。
pub fn reconcile_pair(
    a: &WeightedFr,
    b: &WeightedFr,
    opts: &OptimizeOpts,
) -> (OptimizeReport, OptimizeReport, bool) {
    let ra = optimize(std::slice::from_ref(a), opts);
    let rb = optimize(std::slice::from_ref(b), opts);
    let delta = third_oct_delta(&ra, &rb);
    if delta <= 3.0 {
        return (ra, rb, false);
    }
    let avg = average_fr(
        &[(1.0, a.fr.as_slice()), (1.0, b.fr.as_slice())],
        true,
        false,
    );
    let shared_in = WeightedFr {
        name: format!("{}+{}", a.name, b.name),
        weight: 1.0,
        fr: avg,
    };
    let shared = optimize(std::slice::from_ref(&shared_in), opts);
    let (_, _, rows_a) = predict(&a.fr, &shared.filters, opts);
    let (_, _, rows_b) = predict(&b.fr, &shared.filters, opts);
    let still = smoothed_max_delta(&rows_a, &rows_b);
    let mut left = shared.clone();
    let mut right = shared;
    left.rows = rows_a;
    right.rows = rows_b;
    left.warnings
        .push(format!("匹配组 1/3 oct 差 {delta:.2} dB，已联合重拟合"));
    right.warnings.push("匹配组联合重拟合".into());
    if still > 3.0 {
        left.warnings
            .push(format!("联合重拟合后 1/3 oct 差仍为 {still:.2} dB"));
    }
    (left, right, true)
}

fn third_oct_delta(a: &OptimizeReport, b: &OptimizeReport) -> f64 {
    let mut max_d = 0.0f64;
    for ra in &a.rows {
        let db = sample_pred(&b.rows, ra.freq_hz);
        let d = (ra.predicted_db - db).abs();
        // 只在 1/3 oct 平滑后比较会更稳；这里先对预测做平滑差
        max_d = max_d.max(d);
    }
    // 用平滑后的最大差
    smoothed_max_delta(&a.rows, &b.rows)
}

fn smoothed_max_delta(a: &[PredRow], b: &[PredRow]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let half = 1.0 / 6.0;
    let ln2 = std::f64::consts::LN_2;
    let mut max_d = 0.0f64;
    for r in a {
        let sa = smooth_one(a, r.freq_hz, half, ln2, true);
        let sb = smooth_one(b, r.freq_hz, half, ln2, true);
        max_d = max_d.max((sa - sb).abs());
    }
    max_d
}

fn smooth_one(rows: &[PredRow], f: f64, half: f64, ln2: f64, predicted: bool) -> f64 {
    let mut s = 0.0;
    let mut n = 0.0;
    for q in rows {
        let d = (q.freq_hz.ln() - f.ln()).abs() / ln2;
        if d <= half {
            s += if predicted {
                q.predicted_db
            } else {
                q.residual_db
            };
            n += 1.0;
        }
    }
    if n > 0.0 {
        s / n
    } else {
        sample_pred(rows, f)
    }
}

fn sample_pred(rows: &[PredRow], f: f64) -> f64 {
    if rows.is_empty() {
        return 0.0;
    }
    let mut best = rows[0].predicted_db;
    let mut bd = f64::MAX;
    for r in rows {
        let d = (r.freq_hz - f).abs();
        if d < bd {
            bd = d;
            best = r.predicted_db;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modal::log_grid;

    fn grid_fr(map: impl Fn(f64) -> f64) -> Vec<FrPoint> {
        log_grid(20.0, 20_000.0, 48.0)
            .into_iter()
            .map(|f| FrPoint {
                freq_hz: f,
                db: map(f),
                phase_deg: 0.0,
            })
            .collect()
    }

    #[test]
    fn corrects_toward_flat_not_copy() {
        let fr = grid_fr(|f| {
            if (f - 1000.0).abs() < 1.0 {
                6.0
            } else {
                let x = (f / 1000.0).ln() / std::f64::consts::LN_2;
                6.0 / (1.0 + (x * 1.2).powi(2))
            }
        });
        let opts = OptimizeOpts::standard(48_000.0, Curve::flat());
        let rep = optimize(
            &[WeightedFr {
                name: "listening".into(),
                weight: 1.0,
                fr,
            }],
            &opts,
        );
        assert!(
            rep.filters.iter().all(|p| p.gain_db <= 0.05),
            "{:?}",
            rep.filters
        );
        let rms = smoothed_residual_rms(&rep.rows, 1.0 / 6.0);
        assert!(rms < 1.0, "rms={rms} stages={}", rep.filters.len());
    }

    #[test]
    fn dip_is_not_boosted_when_cap_is_zero() {
        let fr = grid_fr(|f| {
            let x = (f / 1000.0).ln() / std::f64::consts::LN_2;
            -8.0 / (1.0 + (x * 0.8).powi(2))
        });
        let mut opts = OptimizeOpts::standard(48_000.0, Curve::flat());
        opts.boost_cap_db = 0.0; // 本测试是 cut-only 语义（D7 默认 3 在 boost_default 测试覆盖）
        let rep = optimize(
            &[WeightedFr {
                name: "listening".into(),
                weight: 1.0,
                fr,
            }],
            &opts,
        );
        assert!(
            rep.filters.iter().all(|p| p.gain_db <= 0.05),
            "{:?}",
            rep.filters
        );
    }

    #[test]
    fn tilt_target_uses_negative_shelf() {
        let fr = grid_fr(|_| 0.0);
        let opts =
            OptimizeOpts::standard(48_000.0, crate::target::resolve(Some("tilt10")).unwrap());
        let rep = optimize(
            &[WeightedFr {
                name: "listening".into(),
                weight: 1.0,
                fr,
            }],
            &opts,
        );
        assert!(
            rep.filters
                .iter()
                .any(|p| p.filter_type == "high_shelf" && p.gain_db < 0.0),
            "{:?}",
            rep.filters
        );
        let rms = smoothed_residual_rms(&rep.rows, 1.0 / 6.0);
        // tilt10 斜率比旧 tilt 更陡；允许 22 段/±6 dB shelf 逼近后的残差不超过 3 dB
        assert!(rms < 3.0, "rms={rms}");
    }

    #[test]
    fn boost_default_is_3_and_clamped() {
        let opts = OptimizeOpts::standard(48_000.0, crate::target::resolve(None).unwrap());
        assert!((opts.boost_cap_db - 3.0).abs() < 1e-9);
        // 目标/实测有深谷时允许抬 +3 dB，且 D7 字段自洽
        let fr = grid_fr(|f| if (f - 200.0).abs() < 10.0 { -8.0 } else { 0.0 });
        let rep = optimize(
            &[WeightedFr {
                name: "listening".into(),
                weight: 1.0,
                fr,
            }],
            &opts,
        );
        assert!(
            rep.applied_boost_db <= 3.0 + 1e-9,
            "applied={}",
            rep.applied_boost_db
        );
        assert!(rep.total_boost_db <= 12.0 + 1e-9);
        assert!(rep.headroom_db.is_finite());
        assert_eq!(rep.headroom_reference_dbfs, -18.0);
        assert!(
            !rep.clip_risk,
            "default -12 dBFS ref + <=3 dB boost 应无 clip_risk"
        );
        let mut high = opts.clone();
        high.boost_cap_db = 9.0;
        let rep_high = optimize(
            &[WeightedFr {
                name: "listening".into(),
                weight: 1.0,
                fr: grid_fr(|_| 0.0),
            }],
            &high,
        );
        assert!(
            rep_high.warnings.iter().any(|w| w.contains("超过 +3")),
            "{:?}",
            rep_high.warnings
        );
        let mut neg = opts.clone();
        neg.boost_cap_db = -1.0;
        let rep_neg = optimize(
            &[WeightedFr {
                name: "listening".into(),
                weight: 1.0,
                fr: grid_fr(|f| if (f - 200.0).abs() < 10.0 { -8.0 } else { 0.0 }),
            }],
            &neg,
        );
        assert_eq!(
            rep_neg.applied_boost_db, 0.0,
            "boost<0 必须归 0（cut-only）"
        );
    }

    #[test]
    fn headroom_clip_risk_when_reference_is_hot() {
        let opts = OptimizeOpts {
            headroom_reference_dbfs: 0.0,
            ..OptimizeOpts::standard(48_000.0, crate::target::resolve(None).unwrap())
        };
        let rep = optimize(
            &[WeightedFr {
                name: "listening".into(),
                weight: 1.0,
                fr: grid_fr(|_| 0.0),
            }],
            &opts,
        );
        // 无 boost 时 predicted_peak=0 dBFS，headroom=0<6 → clip_risk
        assert!(rep.clip_risk, "headroom={}", rep.headroom_db);
        assert!(rep.headroom_db < 6.0);
    }

    #[test]
    fn eight_modes_q_and_frequency() {
        let specs = [
            (46.0, 12.0, 8.0),
            (55.0, 14.0, 7.0),
            (65.7, 11.0, 7.0),
            (78.5, 16.0, 9.0),
            (93.8, 13.0, 7.0),
            (112.1, 15.0, 8.0),
            (134.0, 18.0, 8.0),
            (160.1, 12.0, 7.0),
        ];
        let fr = grid_fr(|f| {
            let mut db = ((f / 1000.0).ln() * -0.4).clamp(-2.0, 2.0);
            for (f0, q, peak) in specs {
                let half = f0 / q / 2.0;
                let k = 10f64.powf(0.3) - 1.0;
                let atten = 10.0 * (1.0 + (f - f0).powi(2) / half.powi(2) * k).log10();
                db += (peak - atten).max(0.0);
            }
            db
        });
        // 1) 检测层：8 个模态都应被识别（原始 Q 不受 10 上限影响）。
        let detected = modal::detect(&fr, (20.0, 250.0), &[]);
        assert_eq!(detected.len(), 8, "detected={detected:?}");
        for (f0, q, _) in specs {
            let m = detected
                .iter()
                .find(|m| (m.freq - f0).abs() <= 3.0)
                .unwrap_or_else(|| panic!("missing {f0} in {detected:?}"));
            assert!((m.q - q).abs() / q < 0.30, "f={f0} q={} want {q}", m.q);
        }
        // 2) 发射层：optimize 写出的模态 Q 必须 <=10（channelmap 上限），
        //    且总 cut 受 18 dB 预算约束（8 个 -12 dB 段不会再全部写出）。
        let opts = OptimizeOpts::standard(48_000.0, Curve::flat());
        let rep = optimize(
            &[WeightedFr {
                name: "listening".into(),
                weight: 1.0,
                fr,
            }],
            &opts,
        );
        assert!(rep.modal.iter().all(|p| p.gain_db <= 0.0));
        assert!(
            rep.modal.iter().all(|p| p.q <= 10.0 + 1e-9),
            "emitted Q must be clamped: {:?}",
            rep.modal
        );
        assert!(
            rep.modal.iter().map(|p| -p.gain_db).sum::<f64>() <= 18.0 + 1e-6,
            "total modal cut must stay within 18 dB: {:?}",
            rep.modal
        );
        assert!(rep.filters.iter().all(|p| p.q <= 10.0 + 1e-9));
    }

    #[test]
    fn h_matches_check_module() {
        let samples = [
            (Kind::Peaking, 80.0, 4.0, -6.0, 70.0),
            (Kind::Peaking, 1000.0, 12.0, -8.0, 1000.0),
            (Kind::LowShelf, 120.0, 0.7, -2.5, 40.0),
            (Kind::HighShelf, 4000.0, 0.7, -3.0, 12_000.0),
        ];
        for (kind, freq, q, gain, f) in samples {
            let b = Biquad::new(kind, freq, q, gain);
            let kind_s = match kind {
                Kind::Peaking => "peaking",
                Kind::LowShelf => "low_shelf",
                Kind::HighShelf => "high_shelf",
            };
            let got = b.db_at(f, 48_000.0);
            let exp = crate::check::db_at(kind_s, freq, q, gain, f, 48_000.0);
            assert!((got - exp).abs() < 0.1, "{kind_s} got={got} exp={exp}");
        }
    }

    #[test]
    fn smooth_preserves_grid() {
        let fr: Vec<FrPoint> = (0..10)
            .map(|i| FrPoint {
                freq_hz: 20.0 + i as f64,
                db: i as f64,
                phase_deg: 0.0,
            })
            .collect();
        assert_eq!(smooth(&fr, 1).len(), fr.len());
    }

    #[test]
    fn fit_synthetic_peaking() {
        let truth = [
            Biquad::new(Kind::Peaking, 80.0, 1.2, -6.0),
            Biquad::new(Kind::Peaking, 1200.0, 2.0, 4.5),
            Biquad::new(Kind::Peaking, 6500.0, 1.5, -3.0),
        ];
        let fr: Vec<FrPoint> = (0..200)
            .map(|i| {
                let f = 20.0 * (1000.0f64).powf(i as f64 / 199.0);
                FrPoint {
                    freq_hz: f,
                    db: combined_db(&truth, f, 48000.0),
                    phase_deg: 0.0,
                }
            })
            .collect();
        let raw_peak = sample_db(&fr, 1200.0);
        let raw_dip = sample_db(&fr, 80.0);
        let mut opts = OptimizeOpts::standard(48_000.0, Curve::flat());
        opts.boost_cap_db = 0.0; // 本测试检查 cut-only 不填谷；D7 默认 +3 见 boost_default 测试
        let rep = optimize(
            &[WeightedFr {
                name: "listening".into(),
                weight: 1.0,
                fr,
            }],
            &opts,
        );
        assert!(
            rep.filters.iter().all(|p| p.gain_db <= 0.05),
            "校正不能去填谷 {:?}",
            rep.filters
        );
        let pred = |f: f64| -> f64 {
            rep.rows
                .iter()
                .min_by(|a, b| {
                    (a.freq_hz - f)
                        .abs()
                        .partial_cmp(&(b.freq_hz - f).abs())
                        .unwrap()
                })
                .map(|r| r.predicted_db)
                .unwrap_or(0.0)
        };
        assert!(
            pred(1200.0).abs() < raw_peak.abs() - 1.0,
            "1200 Hz 峰应被削低 raw={raw_peak} pred={}",
            pred(1200.0)
        );
        assert!(
            pred(80.0) <= raw_dip + 0.4,
            "80 Hz 谷不能被抬高 raw={raw_dip} pred={}",
            pred(80.0)
        );
    }

    fn bump_fr(specs: &[(f64, f64, f64)]) -> Vec<FrPoint> {
        grid_fr(|f| {
            let mut db = 0.0;
            for (f0, q, peak) in specs {
                let half = f0 / q / 2.0;
                let k = 10f64.powf(0.3) - 1.0;
                let atten = 10.0 * (1.0 + (f - f0).powi(2) / half.powi(2) * k).log10();
                db += (peak - atten).max(0.0);
            }
            db
        })
    }

    /// P0-2：q_unresolved 只报告、不写滤波器（窄于 FR 网格的 Q 不可信）。
    #[test]
    fn q_unresolved_is_reported_not_written() {
        // 模态检测只在 20–250 Hz 进行；用 1/12 oct 网格（df≈5.9 Hz@100）
        // 造一个 -3dB 宽度≈5 Hz 的峰：width < 1.5*df → q_from_bandwidth 判定 unresolved。
        let grid = modal::log_grid(20.0, 20_000.0, 12.0);
        let fr: Vec<FrPoint> = grid
            .iter()
            .map(|&f| {
                let half: f64 = 100.0 / 20.0 / 2.0;
                let k = 10f64.powf(0.3) - 1.0;
                let atten = 10.0 * (1.0 + (f - 100.0).powi(2) / half.powi(2) * k).log10();
                FrPoint {
                    freq_hz: f,
                    db: (10.0 - atten).max(0.0),
                    phase_deg: 0.0,
                }
            })
            .collect();
        let opts = OptimizeOpts::standard(48_000.0, Curve::flat());
        let rep = optimize(
            &[WeightedFr {
                name: "listening".into(),
                weight: 1.0,
                fr,
            }],
            &opts,
        );
        assert!(
            rep.warnings.iter().any(|w| w.contains("q_unresolved")),
            "warnings={:?}",
            rep.warnings
        );
        assert!(
            rep.modal.iter().all(|p| (p.freq - 100.0).abs() > 20.0),
            "q_unresolved mode must not be emitted: {:?}",
            rep.modal
        );
    }

    /// P0-2：20–60 Hz 不可信时不生成低频模态滤波器。
    #[test]
    fn low_band_gate_skips_untrusted_20_60() {
        let fr = bump_fr(&[(40.0, 12.0, 8.0)]);
        let mut opts = OptimizeOpts::standard(48_000.0, Curve::flat());
        opts.low_band_trusted = false;
        let rep = optimize(
            &[WeightedFr {
                name: "listening".into(),
                weight: 1.0,
                fr,
            }],
            &opts,
        );
        assert!(
            rep.warnings.iter().any(|w| w.contains("20–60 Hz")),
            "warnings={:?}",
            rep.warnings
        );
        assert!(
            rep.modal.iter().all(|p| (p.freq - 40.0).abs() > 5.0),
            "low-band mode must be skipped: {:?}",
            rep.modal
        );
    }

    /// P0-2：18 dB 是模态、宽带、shelf 共用的负增益之和，不是每个频点的合成衰减。
    #[test]
    fn total_cut_budget_is_capped() {
        let fr = bump_fr(&[
            (42.0, 3.0, 9.0),
            (46.0, 3.0, 9.0),
            (50.0, 3.0, 9.0),
            (54.0, 3.0, 9.0),
            (58.0, 3.0, 9.0),
        ]);
        let opts = OptimizeOpts::standard(48_000.0, Curve::flat());
        let rep = optimize(
            &[WeightedFr {
                name: "listening".into(),
                weight: 1.0,
                fr,
            }],
            &opts,
        );
        let cut = rep
            .filters
            .iter()
            .map(|p| (-p.gain_db).max(0.0))
            .sum::<f64>();
        assert!(
            cut <= 18.0 + 1e-6,
            "total cut={cut:.2} dB: {:?}",
            rep.filters
        );
        assert!(rep.modal.iter().map(|p| -p.gain_db).sum::<f64>() <= 18.0 + 1e-6);
    }
}
