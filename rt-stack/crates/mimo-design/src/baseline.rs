//! 基线变体（契约 §6）：
//! - B0 RAW：G_n(f) = 1；
//! - B1 DELAY/GAIN/POLARITY：每源 (delay_samples ∈ 0..=960, gain_db ∈ [-12,+6], polarity ∈ {±1})，
//!   目标 = evaluated spec 频带上的平均空间 std（dB），并约束 design 总能量相对 B0 变化 ≤ 1.0 dB；
//!   确定性坐标下降：4 样本/0.5 dB 粗扫 → ±3 样本/0.25 dB 细扫，2 轮。
//!
//! B1 是 legacy delay/gain 对齐的替身，**不是** spec 的 PEQ-only 基线（合成夹具无 PEQ 拟合数据）；
//! spec §6.1 的 δ=1.0 dB 判据只能作为 "对 delay/gain 基线的 predicted 对比"。

use crate::evaluate::BandPlan;
use crate::solve::Cx;

/// B1 每源参数。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct B1SourceParams {
    pub delay_samples: i32,
    pub gain_db: f64,
    pub polarity: f64, // +1.0 / -1.0
}

impl B1SourceParams {
    pub fn identity() -> Self {
        Self {
            delay_samples: 0,
            gain_db: 0.0,
            polarity: 1.0,
        }
    }
    pub fn linear_gain(&self) -> f64 {
        self.polarity * 10.0f64.powf(self.gain_db / 20.0)
    }
    pub fn gain_q(&self) -> i32 {
        (self.gain_db / 0.25).round() as i32
    }
    pub fn from_units(delay_samples: i32, gain_q: i32, polarity: f64) -> Self {
        Self {
            delay_samples: delay_samples.clamp(0, DELAY_MAX),
            gain_db: (gain_q.clamp(GAIN_MIN_Q, GAIN_MAX_Q) as f64) * 0.25,
            polarity: if polarity < 0.0 { -1.0 } else { 1.0 },
        }
    }
}

/// B1 搜索结果。
#[derive(Clone, Debug)]
pub struct B1Result {
    pub params: Vec<B1SourceParams>,
    /// 最终目标：evaluated spec 频带平均空间 std（dB）。
    pub objective_mean_spatial_std_db: f64,
    /// 10·log10(Σdesign evaluated 带内能量_B1after / Σdesign 同口径能量_B0)；约束 |.| ≤ 1.0 dB。
    pub level_ratio_db: f64,
    pub candidates_evaluated: u64,
}

pub const DELAY_MAX: i32 = 960;
pub const GAIN_MIN_Q: i32 = -48; // -12 dB，单位 0.25 dB
pub const GAIN_MAX_Q: i32 = 24; // +6 dB
/// B1 电平保持容差（契约 §6）。
pub const LEVEL_TOLERANCE_DB: f64 = 1.0;
/// B1 每源增益帽（契约 §6：gain_db <= +6 dB）。
pub const B1_GAIN_CAP_DB: f64 = 6.0;

/// B0：G_n(f) = 1。
pub fn b0_gains(n_sources: usize, n_bins: usize) -> Vec<Vec<Cx>> {
    vec![vec![Cx::one(); n_bins]; n_sources]
}

/// 由 B1 参数生成 G_n(f) = polarity·10^(gain/20)·exp(-j2πf·delay/fs)。
pub fn gains_from_params(params: &[B1SourceParams], freqs_hz: &[f64], fs: f64) -> Vec<Vec<Cx>> {
    params
        .iter()
        .map(|p| {
            let c = p.linear_gain();
            freqs_hz
                .iter()
                .map(|f| {
                    let w = std::f64::consts::TAU * f / fs;
                    Cx::exp_neg_j(w * p.delay_samples as f64).scale(c)
                })
                .collect()
        })
        .collect()
}

/// 一次候选的全量评估（为清晰起见逐候选重算；候选数 ~1.4e5、规模小，release 下 <1 s/场景）。
struct CandidateContext<'a> {
    /// h_n[mic][bin]：当前扫描源在每个 design mic/bin 的 H
    h_n: &'a [Vec<Cx>],
    /// base[mic][bin]：其他源在当前参数下的复压力和
    base: &'a [Vec<Cx>],
    /// phase[d][bin]
    phase: &'a [Vec<Cx>],
    band_bins: &'a [Vec<usize>],
    n_design: usize,
    e_b0_total: f64,
}

#[derive(Clone, Copy, Debug)]
struct CandEval {
    objective: f64,
    level_db: f64,
}

impl<'a> CandidateContext<'a> {
    fn evaluate(&self, trial: B1SourceParams) -> CandEval {
        let linear = trial.linear_gain();
        let ph = &self.phase[trial.delay_samples.clamp(0, DELAY_MAX) as usize];
        let plan_len = self.band_bins.len();
        let mut band_energy = vec![vec![0.0f64; plan_len]; self.n_design];
        let mut e_total = 0.0;
        for mic in 0..self.n_design {
            for band_idx in 0..plan_len {
                let mut e = 0.0;
                for &k in &self.band_bins[band_idx] {
                    let p = self.base[mic][k].add(self.h_n[mic][k].mul(ph[k]).scale(linear));
                    e += p.norm_sqr();
                }
                band_energy[mic][band_idx] = e;
                e_total += e;
            }
        }
        // 目标：每带 10log10(E_mk) 的空间 std（总体），对带平均
        let mut std_sum = 0.0;
        let mut cnt = 0usize;
        for band_idx in 0..plan_len {
            let mut levels = Vec::with_capacity(self.n_design);
            for m in 0..self.n_design {
                let e = band_energy[m][band_idx];
                if e > 0.0 {
                    levels.push(10.0 * e.log10());
                }
            }
            if levels.len() < 2 {
                continue;
            }
            let mean = levels.iter().sum::<f64>() / levels.len() as f64;
            let var =
                levels.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / levels.len() as f64;
            std_sum += var.sqrt();
            cnt += 1;
        }
        let objective = if cnt > 0 {
            std_sum / cnt as f64
        } else {
            f64::INFINITY
        };
        let level_db = if e_total > 0.0 && self.e_b0_total > 0.0 {
            10.0 * (e_total / self.e_b0_total).log10()
        } else {
            f64::INFINITY
        };
        CandEval {
            objective,
            level_db,
        }
    }
}

/// B1 坐标下降（确定性）。h_design: [design_mic][bin][source]；p_b0_design: 基线压力 [design_mic][bin]。
pub fn run_b1(
    h_design: &[Vec<Vec<Cx>>],
    freqs_hz: &[f64],
    fs: f64,
    plan: &BandPlan,
    p_b0_design: &[Vec<Cx>],
) -> B1Result {
    let n_design = h_design.len();
    let n_src = h_design
        .first()
        .and_then(|mic| mic.first())
        .map(|hk| hk.len())
        .unwrap_or(0);
    let n_bins = freqs_hz.len();
    let band_bins: Vec<Vec<usize>> = plan.bins.clone();
    // 每个源的 h_n[mic][bin]
    let h_src: Vec<Vec<Vec<Cx>>> = (0..n_src)
        .map(|n| {
            h_design
                .iter()
                .map(|mic| mic.iter().map(|hk| hk[n]).collect())
                .collect()
        })
        .collect();
    // 相位缓存 phase[d][bin]
    let omega: Vec<f64> = freqs_hz
        .iter()
        .map(|f| std::f64::consts::TAU * f / fs)
        .collect();
    let phase: Vec<Vec<Cx>> = (0..=DELAY_MAX)
        .map(|d| omega.iter().map(|w| Cx::exp_neg_j(w * d as f64)).collect())
        .collect();
    // B0 evaluated 带内 design 总能量
    let e_b0_total: f64 = {
        let mut e = 0.0;
        for mic in p_b0_design {
            for band_bins_item in &band_bins {
                for &k in band_bins_item {
                    e += mic[k].norm_sqr();
                }
            }
        }
        e
    };
    let mut params = vec![B1SourceParams::identity(); n_src];
    let mut candidates: u64 = 0;
    let mut best_objective = f64::NAN;
    let mut best_level = f64::NAN;
    for _round in 0..2 {
        for n in 0..n_src {
            // base = 其他源当前参数的复压力和
            let g_all = gains_from_params(&params, freqs_hz, fs);
            let mut base: Vec<Vec<Cx>> = vec![vec![Cx::zero(); n_bins]; n_design];
            for mic in 0..n_design {
                for k in 0..n_bins {
                    let mut p = Cx::zero();
                    for n2 in 0..n_src {
                        if n2 == n {
                            continue;
                        }
                        p = p.add(h_design[mic][k][n2].mul(g_all[n2][k]));
                    }
                    base[mic][k] = p;
                }
            }
            let ctx = CandidateContext {
                h_n: &h_src[n],
                base: &base,
                phase: &phase,
                band_bins: &band_bins,
                n_design,
                e_b0_total,
            };
            // 当前候选（上一轮结果）作为初始最好
            let mut best = params[n];
            let mut best_eval = ctx.evaluate(best);
            candidates += 1;
            // ---- 粗扫：4 样本步长 / 0.5 dB（= 2 个 0.25 dB 单位）----
            for d in (0..=DELAY_MAX).step_by(4) {
                for gain_q in (GAIN_MIN_Q..=GAIN_MAX_Q).step_by(2) {
                    for pol in [1.0f64, -1.0f64] {
                        let trial = B1SourceParams::from_units(d, gain_q, pol);
                        let ev = ctx.evaluate(trial);
                        candidates += 1;
                        if ev.level_db.abs() > LEVEL_TOLERANCE_DB + 1e-9 {
                            continue;
                        }
                        if ev.objective < best_eval.objective - 1e-12 {
                            best_eval = ev;
                            best = trial;
                        }
                    }
                }
            }
            // ---- 细扫：±3 样本 / 0.25 dB，极性沿用粗扫最优 ----
            let d0 = best.delay_samples;
            let g0 = best.gain_q();
            for d in (d0 - 3).max(0)..=(d0 + 3).min(DELAY_MAX) {
                for gain_q in (g0 - 2).max(GAIN_MIN_Q)..=(g0 + 2).min(GAIN_MAX_Q) {
                    let trial = B1SourceParams::from_units(d, gain_q, best.polarity);
                    let ev = ctx.evaluate(trial);
                    candidates += 1;
                    if ev.level_db.abs() > LEVEL_TOLERANCE_DB + 1e-9 {
                        continue;
                    }
                    if ev.objective < best_eval.objective - 1e-12 {
                        best_eval = ev;
                        best = trial;
                    }
                }
            }
            params[n] = best;
            best_objective = best_eval.objective;
            best_level = best_eval.level_db;
        }
    }
    B1Result {
        params,
        objective_mean_spatial_std_db: best_objective,
        level_ratio_db: best_level,
        candidates_evaluated: candidates,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b0_is_unity() {
        let g = b0_gains(3, 5);
        assert_eq!(g.len(), 3);
        for src in &g {
            assert_eq!(src.len(), 5);
            for v in src {
                assert_eq!(*v, Cx::one());
            }
        }
    }

    #[test]
    fn gains_from_params_amplitude_and_delay() {
        let freqs = [40.0f64];
        let params = vec![B1SourceParams {
            delay_samples: 300,
            gain_db: -6.0,
            polarity: -1.0,
        }];
        let g = gains_from_params(&params, &freqs, 48000.0);
        // |G| = 10^(-6/20) ≈ 0.5012；相位：pol=-1 再乘 e^{-jπ/2} → 总相位 +π/2
        assert!((g[0][0].abs() - 10.0f64.powf(-6.0 / 20.0)).abs() < 1e-12);
        // pol=-1 贡献 +π 相位；delay=300@40Hz 贡献 -π/2 → 合计 +π/2
        let expected = std::f64::consts::PI / 2.0;
        assert!((g[0][0].arg() - expected).abs() < 1e-9);
    }

    /// 合成用例：2 design mic / 2 source / 1 band。
    /// B0：P0=1.8, P1=0.2 → 空间 std 很大；B1 用 90° 延迟 (d=300 @40Hz) 后两 mic 幅度相等 → std≈0。
    #[test]
    fn b1_improves_spatial_std_and_keeps_level() {
        let freqs = [40.0f64];
        let h_design: Vec<Vec<Vec<Cx>>> = vec![
            vec![vec![Cx::new(1.0, 0.0), Cx::new(0.8, 0.0)]],
            vec![vec![Cx::new(1.0, 0.0), Cx::new(-0.8, 0.0)]],
        ];
        let plan = BandPlan::from_centers(&[40.0], &freqs);
        let p_b0 = vec![vec![Cx::new(1.8, 0.0)], vec![Cx::new(0.2, 0.0)]];
        let res = run_b1(&h_design, &freqs, 48000.0, &plan, &p_b0);
        assert_eq!(res.params.len(), 2);
        // 坐标下降先扫源 1：它可用 90° 相位 + 电平内增益把两 mic 幅度调平（合法解）；
        // 源 2 随后可能保持 identity。只断言“至少一个源偏离 identity 且目标显著改善”。
        assert!(
            res.params.iter().any(|p| *p != B1SourceParams::identity()),
            "params={:?}",
            res.params
        );
        assert!(
            res.objective_mean_spatial_std_db < 1.0,
            "objective={}",
            res.objective_mean_spatial_std_db
        );
        assert!(
            res.level_ratio_db.abs() <= LEVEL_TOLERANCE_DB + 1e-9,
            "level={}",
            res.level_ratio_db
        );
        assert!(res.candidates_evaluated > 0);
    }
}
