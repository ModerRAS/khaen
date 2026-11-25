//! 频带指标（契约 §5 / spec/mimo-objective-v1）与 RoomForge 风格空间统计。
//!
//! 所有函数都是确定性的纯函数：输入 P: &[Vec<Complex>]（[mic][bin]）与点集/频带，输出 f64。
//! 公式逐条对应契约 §5，注释里给出对应条目。

use std::cmp::Ordering;

use crate::complex::Complex;

/// R_k > 1.05 ⇒ ENERGY_MIGRATION（spec ε = 0.05）。
pub const ENERGY_MIGRATION_RATIO: f64 = 1.05;
/// 空间一致性覆盖容差：|level − mean| ≤ 3 dB。
pub const COVERAGE_TOLERANCE_DB: f64 = 3.0;
/// holdout 严重反向恶化阈值：任一 holdout 点任一带 Δ > +3.0 dB。
pub const HOLDOUT_SEVERE_REGRESSION_DB: f64 = 3.0;
/// holdout 一致性门槛：C ≥ 0.50 为「初步空间泛化」（不得称统计显著）。
pub const HOLDOUT_CONSISTENCY_THRESHOLD: f64 = 0.50;
/// 带权重 clamp 下界/上界。
pub const ALPHA_CLAMP_MIN: f64 = 0.5;
pub const ALPHA_CLAMP_MAX: f64 = 2.0;
/// 分母近零判定：before < 1e-12 · max(before) → N/A。
pub const NA_DENOMINATOR_FLOOR: f64 = 1e-12;
/// 1/6 octave 边界指数（1/3 oct 带宽）。
const SIXTH_OCTAVE_EXPONENT: f64 = 1.0 / 6.0;

/// 一个 1/3 oct 频带：中心 c，边界 [c·2^(−1/6), c·2^(+1/6))。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    pub center_hz: f64,
    pub low_hz: f64,
    pub high_hz: f64,
}

impl Band {
    pub fn one_third_octave(center_hz: f64) -> Band {
        Band {
            center_hz,
            low_hz: center_hz * 2f64.powf(-SIXTH_OCTAVE_EXPONENT),
            high_hz: center_hz * 2f64.powf(SIXTH_OCTAVE_EXPONENT),
        }
    }

    /// 下边界含、上边界不含（契约 §3）。
    pub fn contains(&self, frequency_hz: f64) -> bool {
        frequency_hz >= self.low_hz && frequency_hz < self.high_hz
    }
}

/// ground-truth.json 的 bands 字段（spec / evaluated / N/A / extended）。
#[derive(Clone, Debug, PartialEq)]
pub struct BandPlan {
    pub spec_centers_hz: Vec<f64>,
    pub evaluated_spec_centers_hz: Vec<f64>,
    pub na_spec_centers_hz: Vec<f64>,
    pub extended_centers_hz: Vec<f64>,
    pub band_edges_sixth_oct: bool,
}

impl BandPlan {
    pub fn new(
        spec_centers_hz: Vec<f64>,
        evaluated_spec_centers_hz: Vec<f64>,
        na_spec_centers_hz: Vec<f64>,
        extended_centers_hz: Vec<f64>,
        band_edges_sixth_oct: bool,
    ) -> BandPlan {
        BandPlan {
            spec_centers_hz,
            evaluated_spec_centers_hz,
            na_spec_centers_hz,
            extended_centers_hz,
            band_edges_sixth_oct,
        }
    }

    /// spec 全集（含 N/A 的 160/200 Hz）。
    pub fn spec_bands(&self) -> Vec<Band> {
        self.spec_centers_hz
            .iter()
            .map(|center| Band::one_third_octave(*center))
            .collect()
    }

    /// 实际评估的 spec 频带（40–125 Hz，6 带）。
    pub fn evaluated_bands(&self) -> Vec<Band> {
        self.evaluated_spec_centers_hz
            .iter()
            .map(|center| Band::one_third_octave(*center))
            .collect()
    }

    /// 补充低频观察的 extended 频带（25/31.5 + evaluated）。
    pub fn extended_bands(&self) -> Vec<Band> {
        self.extended_centers_hz
            .iter()
            .map(|center| Band::one_third_octave(*center))
            .collect()
    }

    pub fn n_evaluated(&self) -> usize {
        self.evaluated_spec_centers_hz.len()
    }
}

/// 频带内的 bins（升序）：freqs_hz[k] ∈ [band.low_hz, band.high_hz)。
pub fn bins_in_band(freqs_hz: &[f64], band: &Band) -> Vec<usize> {
    freqs_hz
        .iter()
        .enumerate()
        .filter(|(_, frequency)| band.contains(**frequency))
        .map(|(index, _)| index)
        .collect()
}

fn check_pressure(p: &[Vec<Complex>], mics: &[usize], freqs_hz: &[f64]) {
    for &mic in mics {
        assert!(
            mic < p.len(),
            "mic index {mic} out of range (P has {} mics)",
            p.len()
        );
        assert_eq!(
            p[mic].len(),
            freqs_hz.len(),
            "P[{mic}] has {} bins, frequency grid has {}",
            p[mic].len(),
            freqs_hz.len()
        );
    }
}

/// E_k = Σ_{m∈mics} Σ_{f∈B_k} |P_m(f)|²（契约 §5 基线/残余能量）。
pub fn band_energies(
    p: &[Vec<Complex>],
    mics: &[usize],
    freqs_hz: &[f64],
    bands: &[Band],
) -> Vec<f64> {
    check_pressure(p, mics, freqs_hz);
    bands
        .iter()
        .map(|band| {
            let bins = bins_in_band(freqs_hz, band);
            let mut energy = 0.0;
            for &mic in mics {
                for &bin in &bins {
                    energy += p[mic][bin].norm_sqr();
                }
            }
            energy
        })
        .collect()
}

/// 每个测点每带的 E_m,k（返回顺序同 mics 参数；band 顺序同 bands 参数）。
pub fn band_energies_per_mic(
    p: &[Vec<Complex>],
    mics: &[usize],
    freqs_hz: &[f64],
    bands: &[Band],
) -> Vec<Vec<f64>> {
    check_pressure(p, mics, freqs_hz);
    mics.iter()
        .map(|&mic| {
            bands
                .iter()
                .map(|band| {
                    let bins = bins_in_band(freqs_hz, band);
                    bins.iter().map(|&bin| p[mic][bin].norm_sqr()).sum()
                })
                .collect()
        })
        .collect()
}

/// 单个测点每带的 E_m,k。
pub fn point_band_energies(
    p: &[Vec<Complex>],
    mic: usize,
    freqs_hz: &[f64],
    bands: &[Band],
) -> Vec<f64> {
    check_pressure(p, &[mic], freqs_hz);
    bands
        .iter()
        .map(|band| {
            let bins = bins_in_band(freqs_hz, band);
            bins.iter().map(|&bin| p[mic][bin].norm_sqr()).sum()
        })
        .collect()
}

/// 带权重 α（契约 §5）：
/// x_k = E_k^0 / mean_k(E_k^0)；a_k = clamp(x_k, 0.5, 2.0)；α_k = a_k·K_b/Σ_j a_j（Σα = K_b）。
pub fn alpha_weights(base_band_energies: &[f64]) -> Vec<f64> {
    let count = base_band_energies.len();
    if count == 0 {
        return Vec::new();
    }
    let mean: f64 = base_band_energies.iter().sum::<f64>() / count as f64;
    let mut a: Vec<f64> = base_band_energies
        .iter()
        .map(|energy| {
            let x = if mean > 0.0 { energy / mean } else { 0.0 };
            x.clamp(ALPHA_CLAMP_MIN, ALPHA_CLAMP_MAX)
        })
        .collect();
    let sum_a: f64 = a.iter().sum();
    if sum_a > 0.0 {
        for value in a.iter_mut() {
            *value *= count as f64 / sum_a;
        }
    }
    a
}

/// R_k = after_k / before_k；分母 < 1e-12·max(before) → None（N/A，不参与求和）。
pub fn residual_ratios(design_before: &[f64], design_after: &[f64]) -> Vec<Option<f64>> {
    assert_eq!(
        design_before.len(),
        design_after.len(),
        "before/after band count mismatch: {} vs {}",
        design_before.len(),
        design_after.len()
    );
    let maximum = design_before.iter().cloned().fold(0.0f64, f64::max);
    let threshold = NA_DENOMINATOR_FLOOR * maximum;
    design_before
        .iter()
        .zip(design_after.iter())
        .map(|(&before, &after)| {
            if before > threshold {
                Some(after / before)
            } else {
                None
            }
        })
        .collect()
}

/// E_modal 及配套量（契约 §5）。
#[derive(Clone, Debug, PartialEq)]
pub struct EModalReport {
    pub alpha: Vec<f64>,
    pub r_k: Vec<Option<f64>>,
    pub na_bands: Vec<usize>,
    pub e_modal: f64,
    pub e_modal_db: f64,
    pub e_modal_eq: f64,
    pub r_max: Option<f64>,
    pub argmax_k: Option<usize>,
    pub energy_migration: Vec<usize>,
}

/// E_modal = Σ_{valid} α_k R_k；E_modal_dB = 10·log10(E_modal)；等权对照 Σ_{valid} R_k；
/// R_max = max_k R_k（仅 valid 带）；R_k > 1.05 的 band 列为 ENERGY_MIGRATION。
pub fn emodal(design_before: &[f64], design_after: &[f64]) -> EModalReport {
    let alpha = alpha_weights(design_before);
    let r_k = residual_ratios(design_before, design_after);

    let mut na_bands = Vec::new();
    let mut e_modal = 0.0;
    let mut e_modal_eq = 0.0;
    let mut r_max: Option<f64> = None;
    let mut argmax_k: Option<usize> = None;
    let mut energy_migration = Vec::new();

    for (band_index, ratio) in r_k.iter().enumerate() {
        match ratio {
            None => na_bands.push(band_index),
            Some(ratio) => {
                e_modal += alpha[band_index] * ratio;
                e_modal_eq += ratio;
                if r_max.map_or(true, |current| *ratio > current) {
                    r_max = Some(*ratio);
                    argmax_k = Some(band_index);
                }
                if *ratio > ENERGY_MIGRATION_RATIO {
                    energy_migration.push(band_index);
                }
            }
        }
    }

    EModalReport {
        alpha,
        r_k,
        na_bands,
        e_modal,
        e_modal_db: 10.0 * e_modal.log10(),
        e_modal_eq,
        r_max,
        argmax_k,
        energy_migration,
    }
}

/// 能量转 dB：10·log10(E)；E=0 → −∞，E<0 → NaN。
pub fn energy_to_db(energy: f64) -> f64 {
    10.0 * energy.log10()
}

/// 每带相对变化 Δ = 10·log10(after/before)；before=0（after>0）→ +∞；两者皆 0 → NaN；after=0 → −∞。
pub fn delta_db(before_energy: f64, after_energy: f64) -> f64 {
    if before_energy > 0.0 {
        10.0 * (after_energy / before_energy).log10()
    } else if after_energy > 0.0 {
        f64::INFINITY
    } else {
        f64::NAN
    }
}

/// 每个（点, 带）的 Δ_mk：mics 顺序外、bands 顺序内。
pub fn point_band_deltas_db(
    p_before: &[Vec<Complex>],
    p_after: &[Vec<Complex>],
    mics: &[usize],
    freqs_hz: &[f64],
    bands: &[Band],
) -> Vec<Vec<f64>> {
    check_pressure(p_before, mics, freqs_hz);
    check_pressure(p_after, mics, freqs_hz);
    mics.iter()
        .map(|&mic| {
            let before = point_band_energies(p_before, mic, freqs_hz, bands);
            let after = point_band_energies(p_after, mic, freqs_hz, bands);
            before
                .iter()
                .zip(after.iter())
                .map(|(&b, &a)| delta_db(b, a))
                .collect()
        })
        .collect()
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

/// 最近秩百分位（无插值）：升序序列的第 ceil(p/100·N) − 1 个（clamp 到 [0,N−1]）。
pub fn percentile_nearest_rank(sorted_ascending: &[f64], percentile: f64) -> f64 {
    assert!(
        !sorted_ascending.is_empty(),
        "a percentile needs at least one value"
    );
    assert!(
        percentile > 0.0 && percentile <= 100.0,
        "percentile must be in (0, 100], got {percentile}"
    );
    let count = sorted_ascending.len();
    let rank = (percentile / 100.0 * count as f64).ceil() as i64;
    let index = (rank - 1).clamp(0, count as i64 - 1) as usize;
    sorted_ascending[index]
}

fn sort_ascending(values: &[f64]) -> Vec<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    sorted
}

/// 中位数：奇数取中间序统计量，偶数取两中间值算术平均（与 RoomForge SpatialMetrics 一致）。
pub fn median(values: &[f64]) -> f64 {
    assert!(!values.is_empty(), "a median needs at least one value");
    let sorted = sort_ascending(values);
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[middle]
    } else {
        (sorted[middle - 1] + sorted[middle]) * 0.5
    }
}

/// 一个频带的空间统计（对给定点集）。
#[derive(Clone, Debug, PartialEq)]
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

/// 每带空间统计（契约 §5 "RoomForge 风格空间统计"）：
/// - 每点带能量 E_m,k = Σ_{f∈B_k}|P_m(f)|²，level_m = 10·log10(E_m,k)；
/// - mean/std（总体 σ，÷N）/range/P10/P90（最近秩）/p90−p10；
/// - coverage = |level − mean| ≤ 3 dB 的比例；
/// - 逐频点空间均值 L̄(f) = mean_m 20·log10|P_m(f)|，
///   worst_null = min_{f,m}(20·log10|P_m| − L̄)，max_peak_above_mean = max_{f,m}(…)；
/// - interference_null = 10·log10(min_m E_m,k / mean_m E_m,k)。
pub fn spatial_stats(
    p: &[Vec<Complex>],
    mics: &[usize],
    freqs_hz: &[f64],
    band: &Band,
) -> SpatialStats {
    check_pressure(p, mics, freqs_hz);
    assert!(
        !mics.is_empty(),
        "a spatial summary needs at least one point"
    );
    let bins = bins_in_band(freqs_hz, band);
    assert!(
        !bins.is_empty(),
        "band centred at {} Hz has no bins in the frequency grid",
        band.center_hz
    );

    let count = mics.len();
    let mut energies = Vec::with_capacity(count);
    for &mic in mics {
        let energy: f64 = bins.iter().map(|&bin| p[mic][bin].norm_sqr()).sum();
        energies.push(energy);
    }
    let levels: Vec<f64> = energies
        .iter()
        .map(|&energy| energy_to_db(energy))
        .collect();
    let mean_db = mean(&levels);
    let variance = levels
        .iter()
        .map(|level| {
            let deviation = level - mean_db;
            deviation * deviation
        })
        .sum::<f64>()
        / count as f64;
    let sorted = sort_ascending(&levels);
    let p10_db = percentile_nearest_rank(&sorted, 10.0);
    let p90_db = percentile_nearest_rank(&sorted, 90.0);
    let coverage_within_3db = levels
        .iter()
        .filter(|level| (*level - mean_db).abs() <= COVERAGE_TOLERANCE_DB)
        .count() as f64
        / count as f64;

    let mut worst_null_db = f64::INFINITY;
    let mut max_peak_above_mean_db = f64::NEG_INFINITY;
    for &bin in &bins {
        let mut bin_levels = Vec::with_capacity(count);
        for &mic in mics {
            let magnitude_db = 20.0 * p[mic][bin].abs().log10();
            bin_levels.push(magnitude_db);
        }
        let bin_mean_db = mean(&bin_levels);
        for level in &bin_levels {
            let deviation = *level - bin_mean_db;
            if deviation < worst_null_db {
                worst_null_db = deviation;
            }
            if deviation > max_peak_above_mean_db {
                max_peak_above_mean_db = deviation;
            }
        }
    }

    let minimum_energy = energies.iter().cloned().fold(f64::INFINITY, f64::min);
    let mean_energy = energies.iter().sum::<f64>() / count as f64;
    let ratio = if mean_energy > 0.0 {
        minimum_energy / mean_energy
    } else {
        f64::NAN
    };

    SpatialStats {
        center_hz: band.center_hz,
        n_points: count,
        mean_db,
        std_db: variance.sqrt(),
        range_db: sorted[count - 1] - sorted[0],
        p10_db,
        p90_db,
        p90_p10_db: p90_db - p10_db,
        coverage_within_3db,
        worst_null_db,
        max_peak_above_mean_db,
        interference_null_db: 10.0 * ratio.log10(),
    }
}

/// 对一组频带逐带求空间统计。
pub fn spatial_stats_all(
    p: &[Vec<Complex>],
    mics: &[usize],
    freqs_hz: &[f64],
    bands: &[Band],
) -> Vec<SpatialStats> {
    bands
        .iter()
        .map(|band| spatial_stats(p, mics, freqs_hz, band))
        .collect()
}

/// 一个 holdout 点的评估结果。
#[derive(Clone, Debug, PartialEq)]
pub struct HoldoutPointResult {
    pub mic_index: usize,
    /// 每带 Δ_mk（dB）。
    pub band_delta_db: Vec<f64>,
    /// Σα_k Δ_k / Σα_k（仅非 NaN 的带）。
    pub weighted_mean_delta_db: f64,
    /// 有效改善：加权平均 Δ ≤ 0 dB。
    pub improved: bool,
    /// 至少一个带可加权。
    pub valid: bool,
}

/// 一个严重的 holdout 反向恶化事件。
#[derive(Clone, Debug, PartialEq)]
pub struct SevereRegression {
    pub mic_index: usize,
    pub band_index: usize,
    pub delta_db: f64,
}

/// holdout 评估（契约 §5）：
/// - 每点每带 Δ_mk；
/// - 点有效改善 = α 加权平均 Δ ≤ 0 dB；
/// - 一致性 C = 有效改善点数 / 有效 holdout 数，C ≥ 0.50 ⇒ 初步空间泛化（不是统计显著）；
/// - 任一 holdout 点任一带 Δ > +3.0 dB ⇒ HOLDOUT_SEVERE_REGRESSION。
#[derive(Clone, Debug, PartialEq)]
pub struct HoldoutEvaluation {
    pub points: Vec<HoldoutPointResult>,
    pub valid_points: usize,
    pub improved_points: usize,
    pub consistency: f64,
    pub consistency_pass: bool,
    pub severe_regression: bool,
    pub severe_cases: Vec<SevereRegression>,
}

pub fn holdout_evaluate(
    p_before: &[Vec<Complex>],
    p_after: &[Vec<Complex>],
    holdout_mics: &[usize],
    freqs_hz: &[f64],
    bands: &[Band],
    alpha: &[f64],
) -> HoldoutEvaluation {
    assert_eq!(
        alpha.len(),
        bands.len(),
        "alpha has {} bands, band set has {}",
        alpha.len(),
        bands.len()
    );
    check_pressure(p_before, holdout_mics, freqs_hz);
    check_pressure(p_after, holdout_mics, freqs_hz);

    let mut points = Vec::with_capacity(holdout_mics.len());
    let mut severe_cases = Vec::new();

    for &mic in holdout_mics {
        let before = point_band_energies(p_before, mic, freqs_hz, bands);
        let after = point_band_energies(p_after, mic, freqs_hz, bands);
        let band_delta_db: Vec<f64> = before
            .iter()
            .zip(after.iter())
            .map(|(&b, &a)| delta_db(b, a))
            .collect();

        let mut weighted_sum = 0.0;
        let mut weight_sum = 0.0;
        for (band_index, delta) in band_delta_db.iter().enumerate() {
            if delta.is_nan() {
                continue;
            }
            weighted_sum += alpha[band_index] * delta;
            weight_sum += alpha[band_index];
            if *delta > HOLDOUT_SEVERE_REGRESSION_DB {
                severe_cases.push(SevereRegression {
                    mic_index: mic,
                    band_index,
                    delta_db: *delta,
                });
            }
        }

        let valid = weight_sum > 0.0;
        let weighted_mean_delta_db = if valid {
            weighted_sum / weight_sum
        } else {
            f64::NAN
        };
        let improved = valid && weighted_mean_delta_db <= 0.0;
        points.push(HoldoutPointResult {
            mic_index: mic,
            band_delta_db,
            weighted_mean_delta_db,
            improved,
            valid,
        });
    }

    let valid_points = points.iter().filter(|point| point.valid).count();
    let improved_points = points.iter().filter(|point| point.improved).count();
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
        consistency_pass: consistency >= HOLDOUT_CONSISTENCY_THRESHOLD,
        severe_regression: !severe_cases.is_empty(),
        severe_cases,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixth_octave_edges_and_containment() {
        let band = Band::one_third_octave(100.0);
        assert!((band.low_hz - 100.0 * 2f64.powf(-1.0 / 6.0)).abs() < 1e-12);
        assert!((band.high_hz - 100.0 * 2f64.powf(1.0 / 6.0)).abs() < 1e-12);
        assert!((band.low_hz - 89.089_871_814_033_93).abs() < 1e-12);
        assert!((band.high_hz - 112.246_204_830_937_3).abs() < 1e-12);
        assert!(band.contains(89.089_871_814_033_93));
        assert!(band.contains(112.24));
        assert!(!band.contains(band.high_hz));
        assert!(!band.contains(89.0));
    }

    #[test]
    fn bins_in_band_respects_half_open_edges() {
        let freqs = vec![
            80.0,
            89.0,
            89.089_871_814_033_93,
            90.0,
            100.0,
            112.24,
            112.246_204_830_937_3,
            120.0,
        ];
        let band = Band::one_third_octave(100.0);
        let bins = bins_in_band(&freqs, &band);
        assert_eq!(bins, vec![2, 3, 4, 5]);
    }

    #[test]
    fn band_energies_sum_over_points_and_bins() {
        // 3 bins，2 个 band：band0 覆盖 bin0+bin1，band1 覆盖 bin2。
        let band0 = Band {
            center_hz: 10.0,
            low_hz: 0.0,
            high_hz: 20.0,
        };
        let band1 = Band {
            center_hz: 30.0,
            low_hz: 20.0,
            high_hz: 40.0,
        };
        let freqs = vec![10.0, 15.0, 30.0];
        let p = vec![
            vec![
                Complex::new(1.0, 0.0),
                Complex::new(0.0, 2.0),
                Complex::new(3.0, 0.0),
            ],
            vec![
                Complex::new(2.0, 0.0),
                Complex::new(1.0, 0.0),
                Complex::new(0.0, 1.0),
            ],
        ];
        let energies = band_energies(&p, &[0, 1], &freqs, &[band0, band1]);
        // band0: mic0 1²+2²=5, mic1 2²+1²=5 → 10；band1: mic0 3²=9, mic1 1²=1 → 10。
        assert!((energies[0] - 10.0).abs() < 1e-15);
        assert!((energies[1] - 10.0).abs() < 1e-15);
        let per_mic = band_energies_per_mic(&p, &[0, 1], &freqs, &[band0, band1]);
        assert_eq!(per_mic, vec![vec![5.0, 9.0], vec![5.0, 1.0]]);
        assert_eq!(
            point_band_energies(&p, 1, &freqs, &[band0, band1]),
            vec![5.0, 1.0]
        );
    }

    #[test]
    fn alpha_weights_clamp_and_normalise() {
        let base = [1.0, 2.0, 0.2, 0.4, 4.0, 8.0];
        let alpha = alpha_weights(&base);
        let count = base.len() as f64;
        let mean = base.iter().sum::<f64>() / count;
        let mut expected_a = Vec::new();
        for energy in base {
            expected_a.push((energy / mean).clamp(0.5, 2.0));
        }
        let sum_a: f64 = expected_a.iter().sum();
        let expected: Vec<f64> = expected_a.iter().map(|a| a * count / sum_a).collect();
        assert_eq!(alpha.len(), 6);
        for (actual, reference) in alpha.iter().zip(expected.iter()) {
            assert!((actual - reference).abs() < 1e-15);
        }
        let total: f64 = alpha.iter().sum();
        assert!((total - count).abs() < 1e-12, "sum alpha = {total}");
        // clamp 生效：最小 band 的 α 不低于 0.5·K/Σa，最大 band 的 α 由 2.0 封顶。
        assert!(expected_a[0] == 0.5);
        assert!(expected_a[5] == 2.0);
    }

    #[test]
    fn residual_ratios_marks_near_zero_denominator_na() {
        let before = [1.0, 0.0, 1e-15, 100.0];
        let after = [2.0, 5.0, 3.0, 50.0];
        let ratios = residual_ratios(&before, &after);
        assert_eq!(ratios[0], Some(2.0));
        assert_eq!(ratios[1], None);
        assert_eq!(ratios[2], None); // 1e-15 < 1e-12·100
        assert_eq!(ratios[3], Some(0.5));
    }

    #[test]
    fn emodal_and_energy_migration() {
        let before = [1.0, 2.0, 3.0, 4.0];
        let after = [0.5, 1.0, 3.0, 6.0];
        let report = emodal(&before, &after);
        let alpha = alpha_weights(&before);
        assert_eq!(report.alpha, alpha);
        assert_eq!(report.r_k, vec![Some(0.5), Some(0.5), Some(1.0), Some(1.5)]);
        assert!(report.na_bands.is_empty());
        let expected_modal: f64 = alpha
            .iter()
            .zip([0.5, 0.5, 1.0, 1.5].iter())
            .map(|(a, r)| a * r)
            .sum();
        assert!((report.e_modal - expected_modal).abs() < 1e-15);
        assert!((report.e_modal_db - 10.0 * expected_modal.log10()).abs() < 1e-15);
        assert!((report.e_modal_eq - 3.5).abs() < 1e-15);
        assert_eq!(report.r_max, Some(1.5));
        assert_eq!(report.argmax_k, Some(3));
        assert_eq!(report.energy_migration, vec![3]);
    }

    #[test]
    fn delta_db_handles_zero_energy() {
        assert!((delta_db(1.0, 2.0) - 10.0 * 2f64.log10()).abs() < 1e-15);
        assert!((delta_db(2.0, 1.0) + 10.0 * 2f64.log10()).abs() < 1e-15);
        assert_eq!(delta_db(0.0, 1.0), f64::INFINITY);
        assert_eq!(delta_db(1.0, 0.0), f64::NEG_INFINITY);
        assert!(delta_db(0.0, 0.0).is_nan());
    }

    #[test]
    fn nearest_rank_percentile_matches_csharp_hand_check() {
        // RoomForge SpatialMetrics 文档里的手算例子：−6, −2, 0, 3, 9 → P10 = −6, P90 = 9。
        let sorted = [-6.0, -2.0, 0.0, 3.0, 9.0];
        assert_eq!(percentile_nearest_rank(&sorted, 10.0), -6.0);
        assert_eq!(percentile_nearest_rank(&sorted, 90.0), 9.0);
        assert_eq!(percentile_nearest_rank(&sorted, 50.0), 0.0);
        assert_eq!(percentile_nearest_rank(&[1.0], 10.0), 1.0);
        assert_eq!(percentile_nearest_rank(&[1.0, 2.0], 100.0), 2.0);
        assert_eq!(median(&sorted), 0.0);
        assert_eq!(median(&[1.0, 2.0, 3.0, 4.0]), 2.5);
    }

    #[test]
    fn spatial_stats_hand_checked() {
        // 单 bin band；五个测点带内能量 10^(dB/10) 对应 level −6, −2, 0, 3, 9 dB。
        let band = Band {
            center_hz: 100.0,
            low_hz: 50.0,
            high_hz: 150.0,
        };
        let freqs = vec![100.0];
        let levels_db = [-6.0, -2.0, 0.0, 3.0, 9.0];
        let p: Vec<Vec<Complex>> = levels_db
            .iter()
            .map(|level| vec![Complex::new((10f64.powf(level / 10.0)).sqrt(), 0.0)])
            .collect();
        let mics = [0usize, 1, 2, 3, 4];
        let stats = spatial_stats(&p, &mics, &freqs, &band);
        assert_eq!(stats.n_points, 5);
        assert!((stats.mean_db - 0.8).abs() < 1e-12);
        let expected_std = (126.8f64 / 5.0).sqrt();
        assert!((stats.std_db - expected_std).abs() < 1e-12);
        assert!((stats.range_db - 15.0).abs() < 1e-12);
        assert!((stats.p10_db + 6.0).abs() < 1e-12);
        assert!((stats.p90_db - 9.0).abs() < 1e-12);
        assert!((stats.p90_p10_db - 15.0).abs() < 1e-12);
        assert!((stats.coverage_within_3db - 0.6).abs() < 1e-15);
        assert!((stats.worst_null_db + 6.8).abs() < 1e-12);
        assert!((stats.max_peak_above_mean_db - 8.2).abs() < 1e-12);
        let energies: Vec<f64> = levels_db
            .iter()
            .map(|level| 10f64.powf(level / 10.0))
            .collect();
        let min_energy = energies.iter().cloned().fold(f64::INFINITY, f64::min);
        let mean_energy = energies.iter().sum::<f64>() / 5.0;
        let expected_interference = 10.0 * (min_energy / mean_energy).log10();
        assert!((stats.interference_null_db - expected_interference).abs() < 1e-12);
    }

    #[test]
    fn holdout_consistency_and_severe_regression() {
        // 2 个 holdout 点、2 个带；点 A 两带都改善，点 B 一带 +6 dB 恶化（严重）。
        let band0 = Band {
            center_hz: 10.0,
            low_hz: 0.0,
            high_hz: 20.0,
        };
        let band1 = Band {
            center_hz: 30.0,
            low_hz: 20.0,
            high_hz: 40.0,
        };
        let freqs = vec![10.0, 30.0];
        // 能量：每点每带 1.0 baseline。
        let before = vec![
            vec![Complex::ONE, Complex::ONE],
            vec![Complex::ONE, Complex::ONE],
        ];
        let mut after = before.clone();
        // 点 A：带0 −3 dB（能量 0.5），带1 −1 dB（能量约 0.7943）。
        after[0][0] = Complex::new(0.5f64.sqrt(), 0.0);
        after[0][1] = Complex::new((10f64.powf(-0.1)).sqrt(), 0.0);
        // 点 B：带0 +6 dB（能量 4.0），带1 −1 dB。
        after[1][0] = Complex::new(2.0, 0.0);
        after[1][1] = Complex::new((10f64.powf(-0.1)).sqrt(), 0.0);

        let alpha = [1.0, 1.0];
        let evaluation =
            holdout_evaluate(&before, &after, &[0, 1], &freqs, &[band0, band1], &alpha);
        assert_eq!(evaluation.points.len(), 2);
        assert_eq!(evaluation.valid_points, 2);
        assert_eq!(evaluation.improved_points, 1);
        assert!((evaluation.consistency - 0.5).abs() < 1e-15);
        assert!(evaluation.consistency_pass);
        assert!(evaluation.severe_regression);
        assert_eq!(evaluation.severe_cases.len(), 1);
        assert_eq!(evaluation.severe_cases[0].mic_index, 1);
        assert_eq!(evaluation.severe_cases[0].band_index, 0);
        assert!(evaluation.points[0].improved);
        assert!(!evaluation.points[1].improved);
    }
}
