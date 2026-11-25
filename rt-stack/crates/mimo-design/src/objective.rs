//! 目标函数扩展 v2（OBJECTIVES-V2.md）：emodal | seat-std | null-depth | combined。
//!
//! 全部只用 design 点；holdout 绝不参与选择。SIMULATED / UNPROVEN。
//!
//! 口径：
//! - 目标幅度（seat-std/null-depth/combined）：每 evaluated 1/3 oct 带常数
//!   A_b = beta_level · mean_{m∈D} sqrt(mean_{f∈B_b}|P0_m(f)|²)（功率均值再开方）；
//!   频带外 bin 用最近带常数外推。
//! - emodal 保持 v1 逐频点 target（build_target，beta=0.70），本模块只提供附加指标；
//! - 指标：e_modal（v1 同口径）、seat_std_mean_db / seat_std_max_db（带内 10log10(E_m,b) 的总体 std）、
//!   max_dev_db（相对 B0 的逐点逐带 |ΔdB| 最大）、null_depth_mean_db / worst
//!   （10log10(min_m E_m,b / mean_m E_m,b)，越接近 0 越好）；
//! - IRLS 权重（per-(mic,freq)）：
//!   seat-std: r=||P_m(f)|-A(f)|，w=clamp((r/r̄)^p, w_min, w_max)（r̄ 为该频点跨 mic 均值，尺度无关），
//!            再按频点归一化 mean_m w = 1（保持 λ 口径不变）；
//!   null-depth: |P|<A 时 w=1+k_null·((A-|P|)/A)²，否则 1，再归一化 mean=1；
//!   combined: w = w_std·seat_w + w_null·null_w，再归一化 mean=1。

use crate::evaluate::{delta_db, residual_ratios, BandPlan, ENERGY_MIGRATION_RATIO};
use crate::solve::Cx;

/// 目标类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectiveKind {
    EModal,
    SeatStd,
    NullDepth,
    Combined,
}

impl ObjectiveKind {
    pub fn parse(s: &str) -> Option<ObjectiveKind> {
        match s {
            "emodal" => Some(ObjectiveKind::EModal),
            "seat-std" | "seat_std" => Some(ObjectiveKind::SeatStd),
            "null-depth" | "null_depth" => Some(ObjectiveKind::NullDepth),
            "combined" => Some(ObjectiveKind::Combined),
            _ => None,
        }
    }
    pub fn tag(self) -> &'static str {
        match self {
            ObjectiveKind::EModal => "emodal",
            ObjectiveKind::SeatStd => "seat-std",
            ObjectiveKind::NullDepth => "null-depth",
            ObjectiveKind::Combined => "combined",
        }
    }
    /// 名义 beta_level（OBJECTIVES-V2 §1）。
    pub fn default_beta(self) -> f64 {
        match self {
            ObjectiveKind::EModal => 0.70,
            ObjectiveKind::SeatStd => 1.00,
            ObjectiveKind::NullDepth => 1.00,
            ObjectiveKind::Combined => 0.85,
        }
    }
    /// emodal 之外的幅度目标；seat-std 用 Lp/minimax 幅度残差。
    pub fn is_band_target(self) -> bool {
        !matches!(self, ObjectiveKind::EModal)
    }
}

/// 目标配置（CLI 可覆盖）。
#[derive(Clone, Debug)]
pub struct ObjectiveConfig {
    pub kind: ObjectiveKind,
    /// 名义目标电平（emodal 的 beta，或 seat-std/null-depth/combined 的 beta_level）
    pub beta: f64,
    /// seat-std 的 Lp 指数
    pub p_norm: f64,
    pub weight_min: f64,
    pub weight_max: f64,
    /// null-depth 非对称权重系数
    pub k_null: f64,
    pub w_std: f64,
    pub w_null: f64,
    pub w_e: f64,
    /// holdout-safe 信任域：任一 design/CV-heldout 点任一 evaluated 带 rise > 该值即不可行
    pub max_db_rise_db: f64,
    /// seat_std 目标里 max_dev_db 的权重（默认 0.1；0 = 纯 seat std）
    pub maxdev_weight: f64,
    /// 是否启用 target_level 候选阶梯（emodal 默认关闭以复现 v1）
    pub ladder_enabled: bool,
    /// 阶梯下界
    pub ladder_min: f64,
    /// IRLS 外迭代次数（<=4）
    pub irls_iterations: usize,
}

impl Default for ObjectiveConfig {
    fn default() -> Self {
        Self {
            kind: ObjectiveKind::EModal,
            beta: 0.70,
            p_norm: 6.0,
            weight_min: 0.25,
            weight_max: 4.0,
            k_null: 4.0,
            w_std: 1.0,
            w_null: 1.0,
            w_e: 1.0,
            max_db_rise_db: 2.0,
            maxdev_weight: 0.1,
            ladder_enabled: false,
            ladder_min: 0.55,
            irls_iterations: 2,
        }
    }
}

impl ObjectiveConfig {
    /// 按目标类别构造默认配置；beta_override 覆盖名义 beta。
    pub fn for_kind(kind: ObjectiveKind, beta_override: Option<f64>) -> ObjectiveConfig {
        let mut cfg = ObjectiveConfig::default();
        cfg.kind = kind;
        cfg.beta = beta_override.unwrap_or_else(|| kind.default_beta());
        cfg.ladder_enabled = kind.is_band_target();
        cfg
    }

    /// target_level 候选阶梯：{beta, beta-0.1, beta-0.15? ...} 这里按 0.05 步长从 beta 降到 ladder_min。
    pub fn ladder_levels(&self) -> Vec<f64> {
        if !self.ladder_enabled {
            return vec![self.beta];
        }
        let mut levels = Vec::new();
        let mut lv = self.beta;
        let mut guard = 0;
        while lv >= self.ladder_min - 1e-12 && guard < 64 {
            levels.push((lv * 1e6).round() / 1e6);
            lv -= 0.05;
            guard += 1;
        }
        if levels.is_empty() {
            levels.push(self.beta);
        }
        levels
    }

    /// 目标函数值（越小越好）。
    pub fn objective_value(&self, m: &ObjMetrics) -> f64 {
        match self.kind {
            ObjectiveKind::EModal => m.e_modal_db,
            ObjectiveKind::SeatStd => m.seat_std_mean_db + self.maxdev_weight * m.max_dev_db,
            ObjectiveKind::NullDepth => m.null_depth_mean_db,
            ObjectiveKind::Combined => {
                self.w_e * m.e_modal_db
                    + self.w_std * m.seat_std_mean_db
                    + self.w_null * (-m.null_depth_mean_db)
            }
        }
    }
}

/// 每带目标幅度（带内常数）。
pub fn band_targets(p_b0_points: &[Vec<Cx>], plan: &BandPlan, beta: f64) -> Vec<f64> {
    let mut targets = Vec::with_capacity(plan.len());
    for band in 0..plan.len() {
        let bins = &plan.bins[band];
        if bins.is_empty() {
            targets.push(0.0);
            continue;
        }
        let mut sum_mic = 0.0f64;
        for mic in p_b0_points {
            let mut e = 0.0;
            for &k in bins {
                e += mic[k].norm_sqr();
            }
            sum_mic += (e / bins.len() as f64).sqrt();
        }
        targets.push(beta * sum_mic / p_b0_points.len().max(1) as f64);
    }
    targets
}

/// 把带常数目标展开到频点（带外取最近带）。
pub fn target_per_bin(freqs_hz: &[f64], plan: &BandPlan, band_a: &[f64]) -> Vec<f64> {
    let mut out = Vec::with_capacity(freqs_hz.len());
    for f in freqs_hz {
        let mut idx = None;
        for b in 0..plan.len() {
            if plan.bands[b].contains(*f) {
                idx = Some(b);
                break;
            }
        }
        let b = match idx {
            Some(b) => b,
            None => {
                if plan.is_empty() {
                    0
                } else if *f < plan.bands[0].low_hz {
                    0
                } else {
                    plan.len() - 1
                }
            }
        };
        out.push(band_a.get(b).copied().unwrap_or(0.0));
    }
    out
}

/// 目标指标（同时覆盖 design / holdout / CV 折）。
#[derive(Clone, Debug, Default)]
pub struct ObjMetrics {
    pub e_modal: f64,
    pub e_modal_db: f64,
    pub e_modal_eq_db: f64,
    pub r_k: Vec<Option<f64>>,
    pub na_band_indices: Vec<usize>,
    pub r_max: Option<f64>,
    pub argmax_band: Option<usize>,
    pub band_delta_db: Vec<Option<f64>>,
    pub energy_migration: bool,
    pub seat_std_mean_db: f64,
    pub seat_std_max_db: f64,
    pub max_dev_db: f64,
    pub null_depth_mean_db: f64,
    pub null_depth_worst_db: f64,
    pub per_point_band_db: Vec<Vec<Option<f64>>>,
}

fn energy_db(e: f64) -> f64 {
    if e > 0.0 {
        10.0 * e.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// 给定点集（mics 为 p_after/p_before 的索引）计算目标指标。
///
/// - denom_before: 每带 "before" 能量（design 用 B0(design) E_k0，holdout 用 B0(holdout)）；
/// - alpha: E_modal 带权重（design 冻结 α，所有集合共用）。
pub fn metrics_for_points(
    p_after: &[Vec<Cx>],
    p_before: &[Vec<Cx>],
    mics: &[usize],
    plan: &BandPlan,
    denom_before: &[f64],
    alpha: &[f64],
) -> ObjMetrics {
    let nb = plan.len();
    let mut after_band = vec![0.0f64; nb];
    let mut before_band = vec![0.0f64; nb];
    let mut after_mic_band = vec![vec![0.0f64; nb]; mics.len()];
    let mut before_mic_band = vec![vec![0.0f64; nb]; mics.len()];
    for (mi, &m) in mics.iter().enumerate() {
        for b in 0..nb {
            let mut ea = 0.0;
            let mut eb = 0.0;
            for &k in &plan.bins[b] {
                ea += p_after[m][k].norm_sqr();
                eb += p_before[m][k].norm_sqr();
            }
            after_mic_band[mi][b] = ea;
            before_mic_band[mi][b] = eb;
            after_band[b] += ea;
            before_band[b] += eb;
        }
    }
    let mut out = ObjMetrics::default();
    out.r_k = residual_ratios(denom_before, &after_band);
    let mut e_modal = 0.0;
    let mut alpha_sum = 0.0;
    let mut e_modal_eq = 0.0;
    let mut n_valid = 0usize;
    let mut r_max: Option<f64> = None;
    let mut argmax: Option<usize> = None;
    for (b, r) in out.r_k.iter().enumerate() {
        match r {
            Some(v) => {
                let a = alpha.get(b).copied().unwrap_or(1.0);
                e_modal += a * *v;
                alpha_sum += a;
                e_modal_eq += *v;
                n_valid += 1;
                if r_max.map(|m| *v > m).unwrap_or(true) {
                    r_max = Some(*v);
                    argmax = Some(b);
                }
                if *v > ENERGY_MIGRATION_RATIO {
                    out.energy_migration = true;
                }
            }
            None => out.na_band_indices.push(b),
        }
    }
    out.e_modal = e_modal;
    out.e_modal_db = if e_modal > 0.0 && alpha_sum > 0.0 {
        10.0 * (e_modal / alpha_sum).log10()
    } else {
        f64::NAN
    };
    out.e_modal_eq_db = if e_modal_eq > 0.0 && n_valid > 0 {
        10.0 * (e_modal_eq / n_valid as f64).log10()
    } else {
        f64::NAN
    };
    out.r_max = r_max;
    out.argmax_band = argmax;
    out.band_delta_db = (0..nb)
        .map(|b| {
            if denom_before[b] > 0.0 {
                Some(delta_db(denom_before[b], after_band[b]))
            } else {
                None
            }
        })
        .collect();
    // 座位离散度：seat_std = std_b(10log10 E_m,b)；max_dev_db = max_{b,m}|level_{m,b} - mean_b|
    let mut stds = Vec::new();
    let mut max_dev = 0.0f64;
    for b in 0..nb {
        let mut levels = Vec::new();
        for mi in 0..mics.len() {
            let ea = after_mic_band[mi][b];
            if ea > 0.0 {
                levels.push(energy_db(ea));
            }
        }
        if levels.len() >= 2 {
            let mean = levels.iter().sum::<f64>() / levels.len() as f64;
            let var =
                levels.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / levels.len() as f64;
            stds.push(var.sqrt());
            for v in &levels {
                max_dev = max_dev.max((v - mean).abs());
            }
        } else if levels.len() == 1 {
            // 单点集合无离散度，取 0
        }
        let _ = &before_mic_band;
    }
    out.seat_std_mean_db = if stds.is_empty() {
        f64::NAN
    } else {
        stds.iter().sum::<f64>() / stds.len() as f64
    };
    out.seat_std_max_db = stds.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    out.max_dev_db = max_dev;
    // 源间干涉 null
    let mut nulls = Vec::new();
    for b in 0..nb {
        let energies: Vec<f64> = (0..mics.len())
            .map(|mi| after_mic_band[mi][b])
            .filter(|e| *e > 0.0)
            .collect();
        if energies.is_empty() {
            continue;
        }
        let min_e = energies.iter().cloned().fold(f64::INFINITY, f64::min);
        let mean_e = energies.iter().sum::<f64>() / energies.len() as f64;
        nulls.push(10.0 * (min_e / mean_e).log10());
    }
    out.null_depth_mean_db = if nulls.is_empty() {
        f64::NAN
    } else {
        nulls.iter().sum::<f64>() / nulls.len() as f64
    };
    out.null_depth_worst_db = nulls.iter().cloned().fold(f64::INFINITY, f64::min);
    // 逐点逐带 dB 变化（before = 该点自身 B0）
    out.per_point_band_db = (0..mics.len())
        .map(|mi| {
            (0..nb)
                .map(|b| {
                    let eb = before_mic_band[mi][b];
                    let ea = after_mic_band[mi][b];
                    if eb > 0.0 {
                        Some(delta_db(eb, ea))
                    } else {
                        None
                    }
                })
                .collect()
        })
        .collect();
    out
}

/// 由当前迭代的压力计算 per-(mic,bin) IRLS 权重（design 点，长度 [n_design][K]）。
pub fn irls_weights(
    kind: ObjectiveKind,
    cfg: &ObjectiveConfig,
    p_after_design: &[Vec<Cx>],
    target_per_bin: &[f64],
    _plan: &BandPlan,
) -> Vec<Vec<f64>> {
    let n = p_after_design.len();
    let k = target_per_bin.len();
    let mut weights = vec![vec![1.0f64; k]; n];
    match kind {
        ObjectiveKind::EModal => weights,
        ObjectiveKind::SeatStd => {
            for kj in 0..k {
                let a = target_per_bin[kj];
                let mut r = vec![0.0f64; n];
                let mut mean = 0.0;
                for mi in 0..n {
                    r[mi] = (p_after_design[mi][kj].abs() - a).abs();
                    mean += r[mi];
                }
                mean /= n.max(1) as f64;
                let mut sum = 0.0;
                for mi in 0..n {
                    let rn = if mean > 1e-30 { r[mi] / mean } else { 0.0 };
                    let w = rn.powf(cfg.p_norm).clamp(cfg.weight_min, cfg.weight_max);
                    weights[mi][kj] = w;
                    sum += w;
                }
                let norm = if sum > 0.0 { n as f64 / sum } else { 1.0 };
                for mi in 0..n {
                    weights[mi][kj] *= norm;
                }
            }
            weights
        }
        ObjectiveKind::NullDepth => {
            for kj in 0..k {
                let a = target_per_bin[kj];
                let mut sum = 0.0;
                for mi in 0..n {
                    let mag = p_after_design[mi][kj].abs();
                    let w = if a > 1e-30 && mag < a {
                        1.0 + cfg.k_null * ((a - mag) / a).powi(2)
                    } else {
                        1.0
                    };
                    weights[mi][kj] = w;
                    sum += w;
                }
                let norm = if sum > 0.0 { n as f64 / sum } else { 1.0 };
                for mi in 0..n {
                    weights[mi][kj] *= norm;
                }
            }
            weights
        }
        ObjectiveKind::Combined => {
            let w_seat = irls_weights(
                ObjectiveKind::SeatStd,
                cfg,
                p_after_design,
                target_per_bin,
                _plan,
            );
            let w_null = irls_weights(
                ObjectiveKind::NullDepth,
                cfg,
                p_after_design,
                target_per_bin,
                _plan,
            );
            for kj in 0..k {
                let mut sum = 0.0;
                for mi in 0..n {
                    let w = cfg.w_std * w_seat[mi][kj] + cfg.w_null * w_null[mi][kj];
                    weights[mi][kj] = w.max(1e-6);
                    sum += weights[mi][kj];
                }
                let norm = if sum > 0.0 { n as f64 / sum } else { 1.0 };
                for mi in 0..n {
                    weights[mi][kj] *= norm;
                }
            }
            weights
        }
    }
}

/// 逐点逐带 rise 检查结果（holdout-safe 信任域）。
#[derive(Clone, Debug, Default)]
pub struct RiseCheck {
    pub max_rise_db: f64,
    pub violations: Vec<RiseViolation>,
}

#[derive(Clone, Debug, Default)]
pub struct RiseViolation {
    pub mic_index: usize,
    pub band_index: usize,
    pub band_center_hz: f64,
    pub rise_db: f64,
}

/// 计算给定点集的逐带 rise（10log10(E_after/E_B0)），返回最大值与超门限列表。
pub fn rise_check(
    p_after: &[Vec<Cx>],
    p_before: &[Vec<Cx>],
    mics: &[usize],
    plan: &BandPlan,
    max_db_rise_db: f64,
) -> RiseCheck {
    let nb = plan.len();
    let mut out = RiseCheck {
        max_rise_db: f64::NEG_INFINITY,
        violations: Vec::new(),
    };
    for &m in mics {
        for b in 0..nb {
            let mut ea = 0.0;
            let mut eb = 0.0;
            for &k in &plan.bins[b] {
                ea += p_after[m][k].norm_sqr();
                eb += p_before[m][k].norm_sqr();
            }
            if eb <= 0.0 {
                continue;
            }
            let rise = delta_db(eb, ea);
            if rise.is_finite() && rise > out.max_rise_db {
                out.max_rise_db = rise;
            }
            if rise > max_db_rise_db {
                out.violations.push(RiseViolation {
                    mic_index: m,
                    band_index: b,
                    band_center_hz: plan.centers_hz[b],
                    rise_db: rise,
                });
            }
        }
    }
    if !out.max_rise_db.is_finite() {
        out.max_rise_db = f64::NEG_INFINITY;
    }
    out
}

/// amplitude-only（幅度目标）的 phase-fixed 复目标：
/// target_m(k) = A(k) * P_m(k)/|P_m(k)|（保持当前相位，只把幅度拉向 A；|P|~0 时退化为相位 0）。
pub fn phase_fixed_targets(target_per_bin: &[f64], p_current: &[Vec<Cx>]) -> Vec<Vec<Cx>> {
    let mut out = Vec::with_capacity(p_current.len());
    for mic in p_current {
        let mut row = Vec::with_capacity(target_per_bin.len());
        for (k, a) in target_per_bin.iter().enumerate() {
            let mag = mic[k].abs();
            let ph = if mag > 1e-30 {
                Cx::new(mic[k].re / mag, mic[k].im / mag)
            } else {
                Cx::one()
            };
            row.push(ph.scale(*a));
        }
        out.push(row);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluate::BandPlan;

    fn cx(re: f64, im: f64) -> Cx {
        Cx::new(re, im)
    }

    fn plan_two_bands() -> BandPlan {
        // 40Hz 带 [35.63,44.91) 与 100Hz 带 [89.81,111.36)；频点 40、100 各一条
        BandPlan::from_centers(&[40.0, 100.0], &[40.0, 100.0])
    }

    #[test]
    fn band_targets_hand_computed() {
        // mic0: |P|=1 at both bins, mic1: |P|=3 → 带 RMS = sqrt(1)=1 和 sqrt(9)=3 → mean=2
        let p = vec![
            vec![cx(1.0, 0.0), cx(1.0, 0.0)],
            vec![cx(3.0, 0.0), cx(3.0, 0.0)],
        ];
        let plan = plan_two_bands();
        let t = band_targets(&p, &plan, 1.0);
        assert!((t[0] - 2.0).abs() < 1e-12);
        assert!((t[1] - 2.0).abs() < 1e-12);
        let t2 = band_targets(&p, &plan, 0.5);
        assert!((t2[0] - 1.0).abs() < 1e-12);
        // 展开到频点：带外频点 20Hz 取最近带（40Hz 带）常数
        let expanded = target_per_bin(&[20.0, 40.0, 100.0, 130.0], &plan, &[2.0, 2.0]);
        assert_eq!(expanded, vec![2.0, 2.0, 2.0, 2.0]);
    }

    #[test]
    fn seat_std_and_max_dev_hand_computed() {
        // 单带，2 点；B0 每点每带能量 1；after 幅度 1 与 10^(1/2)：
        // levels: 0 dB, +10 dB；mean 5, 总体 std 5
        let plan = BandPlan::from_centers(&[40.0], &[40.0]);
        let p_before = vec![vec![cx(1.0, 0.0)], vec![cx(1.0, 0.0)]];
        let p_after = vec![vec![cx(1.0, 0.0)], vec![cx(10.0f64.sqrt(), 0.0)]];
        let denom = vec![2.0];
        let m = metrics_for_points(&p_after, &p_before, &[0, 1], &plan, &denom, &[1.0]);
        assert!(
            (m.seat_std_mean_db - 5.0).abs() < 1e-9,
            "std={}",
            m.seat_std_mean_db
        );
        assert!((m.seat_std_max_db - 5.0).abs() < 1e-9);
        // max_dev = max_m |level - mean_b| = |0-5|=5 与 |10-5|=5 → 5
        assert!(
            (m.max_dev_db - 5.0).abs() < 1e-9,
            "max_dev={}",
            m.max_dev_db
        );
        // R_k = (1+10)/(1+1)=5.5 → e_modal_db=10log10(5.5)
        assert!((m.e_modal_db - 10.0 * 5.5f64.log10()).abs() < 1e-12);
    }

    #[test]
    fn null_depth_hand_computed() {
        // 单带 2 点：after 能量 1 与 9 → min/mean = 1/5 → 10log10(0.2) = -6.9897 dB
        let plan = BandPlan::from_centers(&[40.0], &[40.0]);
        let p_before = vec![vec![cx(1.0, 0.0)], vec![cx(1.0, 0.0)]];
        let p_after = vec![vec![cx(1.0, 0.0)], vec![cx(3.0, 0.0)]];
        let m = metrics_for_points(&p_after, &p_before, &[0, 1], &plan, &[2.0], &[1.0]);
        assert!(
            (m.null_depth_mean_db - 10.0 * (1.0f64 / 5.0).log10()).abs() < 1e-12,
            "null={}",
            m.null_depth_mean_db
        );
        assert!((m.null_depth_worst_db - m.null_depth_mean_db).abs() < 1e-12);
    }

    #[test]
    fn irls_weights_monotone_and_normalized() {
        let plan = BandPlan::from_centers(&[40.0], &[40.0]);
        let cfg = ObjectiveConfig::for_kind(ObjectiveKind::SeatStd, None);
        // 三个 design 点幅度与目标 1 的偏差依次 0.1, 0.4, 0.5 → 权重应单调递增
        let p = vec![vec![cx(0.9, 0.0)], vec![cx(0.6, 0.0)], vec![cx(1.5, 0.0)]];
        let w = irls_weights(ObjectiveKind::SeatStd, &cfg, &p, &[1.0], &plan);
        assert!(w[0][0] < w[1][0] && w[1][0] < w[2][0], "weights={:?}", w);
        let mean: f64 = (w[0][0] + w[1][0] + w[2][0]) / 3.0;
        assert!((mean - 1.0).abs() < 1e-12, "mean={}", mean);
        // null-depth：低于目标的点权重更高
        let p2 = vec![vec![cx(0.2, 0.0)], vec![cx(1.0, 0.0)]];
        let wn = irls_weights(ObjectiveKind::NullDepth, &cfg, &p2, &[1.0], &plan);
        assert!(wn[0][0] > wn[1][0], "null weights={:?}", wn);
    }

    #[test]
    fn rise_check_rejects_and_accepts() {
        let plan = BandPlan::from_centers(&[40.0, 100.0], &[40.0, 100.0]);
        let p_before = vec![vec![cx(1.0, 0.0), cx(1.0, 0.0)]];
        // 40Hz +3dB（≈2.0 能量），100Hz 不变 → 超 2.0 dB 门限
        let amp = 10.0f64.powf(3.0 / 20.0);
        let bad = vec![vec![cx(amp, 0.0), cx(1.0, 0.0)]];
        let chk = rise_check(&bad, &p_before, &[0], &plan, 2.0);
        assert_eq!(chk.violations.len(), 1);
        assert!((chk.max_rise_db - 3.0).abs() < 1e-9);
        // 40Hz +1dB → 通过
        let amp_ok = 10.0f64.powf(1.0 / 20.0);
        let ok = vec![vec![cx(amp_ok, 0.0), cx(1.0, 0.0)]];
        let chk = rise_check(&ok, &p_before, &[0], &plan, 2.0);
        assert!(chk.violations.is_empty());
        assert!((chk.max_rise_db - 1.0).abs() < 1e-9);
    }

    #[test]
    fn objective_values_shape() {
        let cfg = ObjectiveConfig::for_kind(ObjectiveKind::Combined, None);
        let m = ObjMetrics {
            e_modal_db: -3.0,
            seat_std_mean_db: 2.0,
            max_dev_db: 5.0,
            null_depth_mean_db: -4.0,
            ..Default::default()
        };
        // combined = 1*(-3) + 1*2 + 1*(+4) = 3
        assert!((cfg.objective_value(&m) - 3.0).abs() < 1e-12);
        let seat = ObjectiveConfig::for_kind(ObjectiveKind::SeatStd, None);
        // seat = 2 + 0.1*5 = 2.5
        assert!((seat.objective_value(&m) - 2.5).abs() < 1e-12);
    }

    #[test]
    fn ladder_levels_step_005() {
        let cfg = ObjectiveConfig::for_kind(ObjectiveKind::SeatStd, None);
        let lv = cfg.ladder_levels();
        assert!((lv[0] - 1.0).abs() < 1e-12);
        assert!(
            lv.iter().any(|v| (v - 0.55).abs() < 1e-9),
            "levels={:?}",
            lv
        );
        for w in lv.windows(2) {
            assert!((w[0] - w[1] - 0.05).abs() < 1e-9);
        }
    }
}
