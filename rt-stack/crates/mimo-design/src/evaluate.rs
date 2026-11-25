//! 契约 §5 / spec/mimo-objective-v1 指标实现（design 与 holdout 分开，holdout 不参与选择）。
//!
//! 口径（已与 roomforge-mimo 冻结）：
//! - design 块：所有 variant 的 R_k 分母统一 = B0(design) 的 E_k0，α 也用 B0(design)；
//! - holdout 块：before = B0 在 holdout 点集上的每带能量（独立分母，R_k(B0)=1）；
//! - α_k = clamp(E_k0/mean(E_k0), 0.5, 2.0) · K / Σ clamp；E_modal = Σ_{valid} α_k R_k；
//! - 空间统计：总体 σ（÷N），P10/P90 最近秩，worst_null/max_peak 逐频点空间均值；
//! - holdout 一致性 C = 有效改善点数 / 有效 holdout 数，Δ>+3.0 dB 任一（点,带）即严重恶化。
//!
//! 本模块不依赖 room-sim；CLI 会用 room-sim::metrics 做独立复算交叉校验（见 tools/mimo-opt）。

use crate::solve::Cx;

/// 1/3 oct 频带（边界 [c·2^(-1/6), c·2^(+1/6))）。
#[derive(Clone, Debug)]
pub struct Band {
    pub center_hz: f64,
    pub low_hz: f64,
    pub high_hz: f64,
}

impl Band {
    pub fn one_third_octave(center_hz: f64) -> Band {
        let f = (2.0f64).powf(1.0 / 6.0);
        Band {
            center_hz,
            low_hz: center_hz / f,
            high_hz: center_hz * f,
        }
    }
    pub fn contains(&self, f: f64) -> bool {
        f >= self.low_hz && f < self.high_hz
    }
}

/// 频带计划：evaluated spec 频带（报告/优化口径）。
#[derive(Clone, Debug)]
pub struct BandPlan {
    pub centers_hz: Vec<f64>,
    pub bands: Vec<Band>,
    /// 每带内的频点索引（构造时按给定 freqs 计算）
    pub bins: Vec<Vec<usize>>,
}

impl BandPlan {
    pub fn from_centers(centers_hz: &[f64], freqs_hz: &[f64]) -> BandPlan {
        let bands: Vec<Band> = centers_hz
            .iter()
            .map(|c| Band::one_third_octave(*c))
            .collect();
        let bins: Vec<Vec<usize>> = bands
            .iter()
            .map(|b| {
                freqs_hz
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| b.contains(**f))
                    .map(|(i, _)| i)
                    .collect()
            })
            .collect();
        BandPlan {
            centers_hz: centers_hz.to_vec(),
            bands,
            bins,
        }
    }
    pub fn len(&self) -> usize {
        self.centers_hz.len()
    }
    pub fn is_empty(&self) -> bool {
        self.centers_hz.is_empty()
    }
}

/// 频带能量 E_k = Σ_{m∈mics} Σ_{f∈B_k} |P_m(f)|²。
pub fn band_energies(p: &[Vec<Cx>], mics: &[usize], plan: &BandPlan) -> Vec<f64> {
    let mut out = vec![0.0f64; plan.len()];
    for band_idx in 0..plan.len() {
        let mut e = 0.0;
        for &m in mics {
            for &k in &plan.bins[band_idx] {
                e += p[m][k].norm_sqr();
            }
        }
        out[band_idx] = e;
    }
    out
}

/// 每 mic 每带能量（返回顺序同 mics）。
pub fn band_energies_per_mic(p: &[Vec<Cx>], mics: &[usize], plan: &BandPlan) -> Vec<Vec<f64>> {
    let mut out = Vec::with_capacity(mics.len());
    for &m in mics {
        let mut row = vec![0.0f64; plan.len()];
        for band_idx in 0..plan.len() {
            let mut e = 0.0;
            for &k in &plan.bins[band_idx] {
                e += p[m][k].norm_sqr();
            }
            row[band_idx] = e;
        }
        out.push(row);
    }
    out
}

/// 某点集在 evaluated 带内的总能量（B1 电平保持约束用）。
pub fn total_energy_in_bands(p: &[Vec<Cx>], mics: &[usize], plan: &BandPlan) -> f64 {
    let mut e = 0.0;
    for &m in mics {
        for band_idx in 0..plan.len() {
            for &k in &plan.bins[band_idx] {
                e += p[m][k].norm_sqr();
            }
        }
    }
    e
}

/// α 权重：x_k = E_k0/mean(E_k0)；a_k = clamp(x_k, 0.5, 2.0)；α_k = a_k·K/Σa。
pub fn alpha_weights(base_band_energies: &[f64]) -> Vec<f64> {
    let k = base_band_energies.len();
    if k == 0 {
        return Vec::new();
    }
    let mean = base_band_energies.iter().sum::<f64>() / k as f64;
    if !(mean.is_finite() && mean > 0.0) {
        // 退化：等权（Σα = K）
        return vec![1.0; k];
    }
    let a: Vec<f64> = base_band_energies
        .iter()
        .map(|e| (e / mean).clamp(0.5, 2.0))
        .collect();
    let sum: f64 = a.iter().sum();
    a.iter().map(|v| v * k as f64 / sum).collect()
}

/// R_k = after_k / before_k；before 近零（< 1e-12·max(before)）→ None（N/A）。
pub fn residual_ratios(before: &[f64], after: &[f64]) -> Vec<Option<f64>> {
    debug_assert_eq!(before.len(), after.len());
    let max_before = before.iter().cloned().fold(0.0f64, f64::max);
    let threshold = 1e-12 * max_before;
    before
        .iter()
        .zip(after.iter())
        .map(|(b, a)| {
            if *b > threshold && *b > 0.0 {
                Some(*a / *b)
            } else {
                None
            }
        })
        .collect()
}

/// 能量迁移阈值（spec ε = 0.05）。
pub const ENERGY_MIGRATION_RATIO: f64 = 1.05;
/// holdout 严重反向恶化阈值（spec：> +3.0 dB 任一（点,带））。
pub const SEVERE_REGRESSION_DB: f64 = 3.0;
/// holdout 一致性门槛（spec C ≥ 0.50）。
pub const CONSISTENCY_MIN: f64 = 0.50;

/// 10·log10(after/before)；before<=0 → +inf（after>0）/NaN；after==0 → -inf。
pub fn delta_db(before: f64, after: f64) -> f64 {
    if before <= 0.0 {
        if after > 0.0 {
            f64::INFINITY
        } else {
            f64::NAN
        }
    } else if after <= 0.0 {
        f64::NEG_INFINITY
    } else {
        10.0 * (after / before).log10()
    }
}

/// E_modal 汇总（design 或 holdout 通用，由调用方决定 before/after 点集与分母）。
#[derive(Clone, Debug, Default)]
pub struct EModal {
    pub alpha: Vec<f64>,
    pub r_k: Vec<Option<f64>>,
    pub na_band_indices: Vec<usize>,
    pub e_modal: f64,
    pub e_modal_db: f64,
    pub e_modal_eq: f64,
    pub e_modal_eq_db: f64,
    pub r_max: Option<f64>,
    pub argmax_band: Option<usize>,
    pub energy_migration: bool,
}

/// 由 before/after 每带能量 + α 计算 E_modal 汇总。
pub fn emodal_from_energies(before: &[f64], after: &[f64], alpha: &[f64]) -> EModal {
    let r_k = residual_ratios(before, after);
    let mut e_modal = 0.0;
    let mut e_modal_eq = 0.0;
    let mut alpha_sum = 0.0f64;
    let mut n_valid = 0usize;
    let mut na = Vec::new();
    let mut r_max: Option<f64> = None;
    let mut argmax: Option<usize> = None;
    let mut migration = false;
    for (i, r) in r_k.iter().enumerate() {
        match r {
            Some(v) => {
                let a = alpha.get(i).copied().unwrap_or(1.0);
                e_modal += a * *v;
                alpha_sum += a;
                e_modal_eq += *v;
                n_valid += 1;
                if r_max.map(|m| *v > m).unwrap_or(true) {
                    r_max = Some(*v);
                    argmax = Some(i);
                }
                if *v > ENERGY_MIGRATION_RATIO {
                    migration = true;
                }
            }
            None => na.push(i),
        }
    }
    // 归一化（CONTRACT v2 §5）：B0 必须为 0 dB。
    // e_modal_db = 10log10( E_modal / Σ_valid α_k )，e_modal_eq_db = 10log10( (Σ_valid R_k) / K_valid )
    let e_modal_db = if e_modal > 0.0 && alpha_sum > 0.0 {
        10.0 * (e_modal / alpha_sum).log10()
    } else {
        f64::NEG_INFINITY
    };
    let e_modal_eq_db = if e_modal_eq > 0.0 && n_valid > 0 {
        10.0 * (e_modal_eq / n_valid as f64).log10()
    } else {
        f64::NEG_INFINITY
    };
    EModal {
        alpha: alpha.to_vec(),
        r_k,
        na_band_indices: na,
        e_modal,
        e_modal_db,
        e_modal_eq,
        e_modal_eq_db,
        r_max,
        argmax_band: argmax,
        energy_migration: migration,
    }
}

/// 最近秩百分位（输入需升序；p ∈ [0,100]）。空返回 NaN。
pub fn percentile_nearest_rank(sorted_ascending: &[f64], p: f64) -> f64 {
    let n = sorted_ascending.len();
    if n == 0 {
        return f64::NAN;
    }
    let rank = ((p / 100.0) * n as f64).ceil() as isize; // 1-based
    let idx = rank.clamp(1, n as isize) as usize - 1;
    sorted_ascending[idx]
}

/// RoomForge 风格空间统计（每带，对给定点集）。
#[derive(Clone, Debug, Default)]
pub struct SpatialStats {
    pub center_hz: f64,
    pub n_points: usize,
    pub mean_db: f64,
    pub std_db: f64,
    pub range_db: f64,
    pub p10_db: f64,
    pub p90_db: f64,
    pub p90_p10_db: f64,
    pub coverage_within_3db: f64,
    pub worst_null_db: f64,
    pub max_peak_above_mean_db: f64,
    pub interference_null_db: f64,
}

fn to_db_energy(e: f64) -> f64 {
    if e > 0.0 {
        10.0 * e.log10()
    } else {
        f64::NEG_INFINITY
    }
}

fn to_db_magnitude(c: Cx) -> f64 {
    let a = c.abs();
    if a > 0.0 {
        20.0 * a.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// 单带空间统计。p: [mic][bin]；mics: 点索引；plan 提供该带 bins。
pub fn spatial_stats_band(
    p: &[Vec<Cx>],
    mics: &[usize],
    plan: &BandPlan,
    band_idx: usize,
) -> SpatialStats {
    let band = &plan.bands[band_idx];
    let bins = &plan.bins[band_idx];
    let mut levels = Vec::with_capacity(mics.len());
    let mut per_mic_energy = Vec::with_capacity(mics.len());
    for &m in mics {
        let mut e = 0.0;
        for &k in bins {
            e += p[m][k].norm_sqr();
        }
        per_mic_energy.push(e);
        levels.push(to_db_energy(e));
    }
    let finite_levels: Vec<f64> = levels.iter().cloned().filter(|v| v.is_finite()).collect();
    let n = finite_levels.len();
    let mean = if n > 0 {
        finite_levels.iter().sum::<f64>() / n as f64
    } else {
        f64::NAN
    };
    let std_db = if n > 0 {
        (finite_levels
            .iter()
            .map(|v| (v - mean) * (v - mean))
            .sum::<f64>()
            / n as f64)
            .sqrt()
    } else {
        f64::NAN
    };
    let mut sorted = finite_levels.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let range_db = if n > 0 {
        sorted[n - 1] - sorted[0]
    } else {
        f64::NAN
    };
    let p10 = percentile_nearest_rank(&sorted, 10.0);
    let p90 = percentile_nearest_rank(&sorted, 90.0);
    let coverage = if n > 0 && mean.is_finite() {
        finite_levels
            .iter()
            .filter(|v| (*v - mean).abs() <= 3.0)
            .count() as f64
            / n as f64
    } else {
        f64::NAN
    };
    // worst_null / max_peak：逐频点空间均值 L̄(f)，遍历 (m,f) 取偏差极值
    let mut worst_null = f64::INFINITY;
    let mut max_peak = f64::NEG_INFINITY;
    for &k in bins {
        let mut sum = 0.0;
        let mut cnt = 0usize;
        for &m in mics {
            let d = to_db_magnitude(p[m][k]);
            if d.is_finite() {
                sum += d;
                cnt += 1;
            }
        }
        if cnt == 0 {
            continue;
        }
        let mean_k = sum / cnt as f64;
        for &m in mics {
            let d = to_db_magnitude(p[m][k]);
            if d.is_finite() {
                let dev = d - mean_k;
                if dev < worst_null {
                    worst_null = dev;
                }
                if dev > max_peak {
                    max_peak = dev;
                }
            }
        }
    }
    // 干涉 null：10log10(min_m E_mk / mean_m E_mk)
    let interference = {
        let positive: Vec<f64> = per_mic_energy
            .iter()
            .cloned()
            .filter(|e| *e > 0.0)
            .collect();
        if positive.is_empty() {
            f64::NAN
        } else {
            let min_e = positive.iter().cloned().fold(f64::INFINITY, f64::min);
            let mean_e = positive.iter().sum::<f64>() / positive.len() as f64;
            10.0 * (min_e / mean_e).log10()
        }
    };
    SpatialStats {
        center_hz: band.center_hz,
        n_points: mics.len(),
        mean_db: mean,
        std_db,
        range_db,
        p10_db: p10,
        p90_db: p90,
        p90_p10_db: p90 - p10,
        coverage_within_3db: coverage,
        worst_null_db: if worst_null.is_finite() {
            worst_null
        } else {
            f64::NAN
        },
        max_peak_above_mean_db: if max_peak.is_finite() {
            max_peak
        } else {
            f64::NAN
        },
        interference_null_db: interference,
    }
}

pub fn spatial_stats_all(p: &[Vec<Cx>], mics: &[usize], plan: &BandPlan) -> Vec<SpatialStats> {
    (0..plan.len())
        .map(|b| spatial_stats_band(p, mics, plan, b))
        .collect()
}

/// 每 band 的空间 std 的平均（B1 目标：evaluated spec 频带平均空间 std，dB）。
pub fn mean_spatial_std_db(p: &[Vec<Cx>], mics: &[usize], plan: &BandPlan) -> f64 {
    if plan.is_empty() {
        return f64::NAN;
    }
    let stats = spatial_stats_all(p, mics, plan);
    let vals: Vec<f64> = stats
        .iter()
        .map(|s| s.std_db)
        .filter(|v| v.is_finite())
        .collect();
    if vals.is_empty() {
        f64::NAN
    } else {
        vals.iter().sum::<f64>() / vals.len() as f64
    }
}

/// 每个（mic, band）的 after/before 能量比 → dB。
pub fn point_band_deltas_db(
    p_before: &[Vec<Cx>],
    p_after: &[Vec<Cx>],
    mics: &[usize],
    plan: &BandPlan,
) -> Vec<Vec<Option<f64>>> {
    let before = band_energies_per_mic(p_before, mics, plan);
    let after = band_energies_per_mic(p_after, mics, plan);
    let mut out = Vec::with_capacity(mics.len());
    for (mi, _) in mics.iter().enumerate() {
        let mut row = Vec::with_capacity(plan.len());
        for b in 0..plan.len() {
            if before[mi][b] > 0.0 {
                row.push(Some(delta_db(before[mi][b], after[mi][b])));
            } else {
                row.push(None);
            }
        }
        out.push(row);
    }
    out
}

/// holdout 点结果（与 room-sim metrics::HoldoutPointResult 同口径）。
#[derive(Clone, Debug, Default)]
pub struct HoldoutPointResult {
    pub mic_index: usize,
    pub band_delta_db: Vec<Option<f64>>,
    pub weighted_mean_delta_db: f64,
    pub improved: bool,
    pub valid: bool,
}

#[derive(Clone, Debug, Default)]
pub struct HoldoutEvaluation {
    pub points: Vec<HoldoutPointResult>,
    pub valid_points: usize,
    pub improved_points: usize,
    pub consistency: f64,
    pub consistency_pass: bool,
    pub severe_regression: bool,
    pub severe_cases: Vec<(usize, usize, f64)>, // (mic_index, band_index, delta_db)
}

/// holdout 评估：per-point before = B0，权重用 design α。
pub fn holdout_evaluate(
    p_before: &[Vec<Cx>],
    p_after: &[Vec<Cx>],
    holdout_mics: &[usize],
    plan: &BandPlan,
    alpha: &[f64],
) -> HoldoutEvaluation {
    let deltas = point_band_deltas_db(p_before, p_after, holdout_mics, plan);
    let mut points = Vec::with_capacity(holdout_mics.len());
    let mut valid_points = 0usize;
    let mut improved_points = 0usize;
    let mut severe = false;
    let mut severe_cases = Vec::new();
    for (mi, &mic_index) in holdout_mics.iter().enumerate() {
        let row = &deltas[mi];
        let mut wsum = 0.0;
        let mut dsum = 0.0;
        let mut any = false;
        for (b, d) in row.iter().enumerate() {
            if let Some(v) = d {
                if v.is_finite() {
                    let w = alpha.get(b).copied().unwrap_or(1.0);
                    wsum += w;
                    dsum += w * *v;
                    any = true;
                    if *v > SEVERE_REGRESSION_DB {
                        severe = true;
                        severe_cases.push((mic_index, b, *v));
                    }
                }
            }
        }
        let valid = any && wsum > 0.0;
        let weighted = if valid { dsum / wsum } else { f64::NAN };
        let improved = valid && weighted <= 0.0;
        if valid {
            valid_points += 1;
        }
        if improved {
            improved_points += 1;
        }
        points.push(HoldoutPointResult {
            mic_index,
            band_delta_db: row.clone(),
            weighted_mean_delta_db: weighted,
            improved,
            valid,
        });
    }
    let consistency = if valid_points > 0 {
        improved_points as f64 / valid_points as f64
    } else {
        f64::NAN
    };
    HoldoutEvaluation {
        points,
        valid_points,
        improved_points,
        consistency,
        consistency_pass: consistency.is_finite() && consistency >= CONSISTENCY_MIN,
        severe_regression: severe,
        severe_cases,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solve::Cx;

    fn cx(re: f64, im: f64) -> Cx {
        Cx::new(re, im)
    }

    #[test]
    fn bands_and_bins() {
        let b = Band::one_third_octave(40.0);
        assert!(b.contains(40.0));
        assert!(!b.contains(56.0)); // 56 > 40*2^(1/6)=44.9
        let centers = [40.0, 50.0, 63.0, 80.0];
        let freqs = [39.0, 40.0, 45.0, 50.0, 63.0, 80.0];
        let plan = BandPlan::from_centers(&centers, &freqs);
        // 39 与 40 都落在 40 Hz 带 [35.63, 44.91) 内；45 不在
        assert_eq!(plan.bins[0], vec![0, 1]);
    }

    #[test]
    fn alpha_weights_clamped() {
        // E = [1, 100, 4] → mean 35；x = [0.0286, 2.857, 0.114] → clamp [0.5, 2.0, 0.5]
        let e = [1.0, 100.0, 4.0];
        let a = alpha_weights(&e);
        let k = 3.0;
        let sum_a: f64 = [0.5, 2.0, 0.5].iter().sum();
        assert!((a[0] - 0.5 * k / sum_a).abs() < 1e-12);
        assert!((a[1] - 2.0 * k / sum_a).abs() < 1e-12);
        assert!((a[2] - 0.5 * k / sum_a).abs() < 1e-12);
        assert!((a.iter().sum::<f64>() - k).abs() < 1e-12);
    }

    #[test]
    fn residual_ratios_and_na() {
        let before = [1.0, 0.0, 4.0];
        let after = [0.5, 1.0, 4.0];
        let r = residual_ratios(&before, &after);
        assert_eq!(r[0], Some(0.5));
        assert_eq!(r[1], None);
        assert_eq!(r[2], Some(1.0));
    }

    #[test]
    fn emodal_weighted_sum() {
        let before = [1.0, 1.0, 1.0];
        let after = [0.5, 1.0, 2.0];
        let alpha = vec![1.0, 1.0, 1.0];
        let e = emodal_from_energies(&before, &after, &alpha);
        assert!((e.e_modal - 3.5).abs() < 1e-12);
        assert!((e.e_modal_eq - 3.5).abs() < 1e-12);
        // 归一化后 = 10log10(3.5/3)（B0 为 0 dB 的口径）
        assert!((e.e_modal_db - 10.0 * (3.5f64 / 3.0).log10()).abs() < 1e-12);
        assert!((e.e_modal_eq_db - 10.0 * (3.5f64 / 3.0).log10()).abs() < 1e-12);
        assert_eq!(e.r_max, Some(2.0));
        assert_eq!(e.argmax_band, Some(2));
        assert!(e.energy_migration); // 2.0 > 1.05
    }

    #[test]
    fn spatial_stats_hand_computed() {
        // 2 mic，单带单 bin：|P| = 1, 10 → 20log10 = 0, 20 dB；mean 10, std 10
        let plan = BandPlan::from_centers(&[40.0], &[40.0]);
        let p = vec![vec![cx(1.0, 0.0)], vec![cx(10.0, 0.0)]];
        let s = spatial_stats_band(&p, &[0, 1], &plan, 0);
        assert!((s.mean_db - 10.0).abs() < 1e-12);
        assert!((s.std_db - 10.0).abs() < 1e-12);
        assert!((s.range_db - 20.0).abs() < 1e-12);
        assert!((s.worst_null_db - (-10.0)).abs() < 1e-12);
        assert!((s.max_peak_above_mean_db - 10.0).abs() < 1e-12);
        let e1: f64 = 1.0;
        let mean_e: f64 = 50.5;
        assert!((s.interference_null_db - 10.0 * (e1 / mean_e).log10()).abs() < 1e-12);
        // coverage: |0-10|=10 >3, |20-10|=10 >3 → 0
        assert!((s.coverage_within_3db - 0.0).abs() < 1e-12);
    }

    #[test]
    fn holdout_consistency_and_severe() {
        let plan = BandPlan::from_centers(&[40.0, 50.0], &[40.0, 50.0]);
        // before: 每点每带能量 1
        let p_before = vec![
            vec![cx(1.0, 0.0), cx(1.0, 0.0)],
            vec![cx(1.0, 0.0), cx(1.0, 0.0)],
            vec![cx(1.0, 0.0), cx(1.0, 0.0)],
        ];
        let p_after = vec![
            // 点0：两带都降 3dB（改善）
            vec![cx(0.7071, 0.0), cx(0.7071, 0.0)],
            // 点1：一带不变、一带升 1dB（加权平均 ≤? 1dB/2=0.5 >0 → 未改善）
            vec![cx(1.0, 0.0), cx(1.122, 0.0)],
            // 点2：严重恶化 +5dB（0.7071→2.0 相对 1? 用 1.778 = +5dB）
            vec![cx(1.0, 0.0), cx(1.778, 0.0)],
        ];
        let alpha = vec![1.0, 1.0];
        let ev = holdout_evaluate(&p_before, &p_after, &[0, 1, 2], &plan, &alpha);
        assert_eq!(ev.valid_points, 3);
        assert_eq!(ev.improved_points, 1);
        assert!((ev.consistency - 1.0 / 3.0).abs() < 1e-12);
        assert!(ev.severe_regression);
        assert_eq!(ev.severe_cases.len(), 1);
        assert_eq!(ev.severe_cases[0].0, 2);
    }

    #[test]
    fn delta_db_edges() {
        assert!((delta_db(1.0, 10.0) - 10.0).abs() < 1e-12);
        assert!(delta_db(0.0, 1.0).is_infinite() && delta_db(0.0, 1.0) > 0.0);
        assert!(delta_db(1.0, 0.0).is_infinite() && delta_db(1.0, 0.0) < 0.0);
        assert!(delta_db(0.0, 0.0).is_nan());
    }

    #[test]
    fn percentile_nearest_rank_hand() {
        let v = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        assert_eq!(percentile_nearest_rank(&v, 10.0), 1.0);
        assert_eq!(percentile_nearest_rank(&v, 90.0), 9.0);
        assert_eq!(percentile_nearest_rank(&v, 100.0), 10.0);
    }
}
