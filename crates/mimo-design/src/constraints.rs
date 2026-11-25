//! 硬约束与缩放（契约 §6.6 + spec/mimo-objective-v1 §4 的声明式 headroom）。
//!
//! - 每源带内最大 |G_n(f)| ≤ +6 dB：超限则整源缩放并记录 applied_scale；
//! - headroom（声明式近似，见 REPORT/notes 的 spec 差异说明）：
//!   仿真无真实 playback level，不能把单位冲激最坏界 Σ|h| 当 −6 dBFS 峰值界
//!   （δ_D 直通都会让 Σ|h| ≥ 1）。按 spec §4 "预测峰值 ≤ −6 dBFS" 的等效声明：
//!   参考输入峰值 0.5（−6 dBFS）时预测输出峰值 = 0.5·max_n max_f |G_n(f)| ≤ 1.0，
//!   等价于 in-band max|G| ≤ +6 dB（与增益帽同一约束）。因此不做 0.5/Σ|h| 全局缩放；
//!   Σ|h| 仍作为 "peak_bound_per_source" 报告（信息量，不用于缩放）。
//! - 报告 E_effort、相对基线能量比、每源最大增益、预测峰值、总延迟。

use crate::fir;
use crate::solve::Cx;

/// FIR 变体约束报告。
#[derive(Clone, Debug)]
pub struct FirConstraintReport {
    pub per_source_max_gain_db: Vec<f64>,
    /// Σ_t |h_n[t]|（单位冲激最坏情况；仅报告，不作为缩放依据）
    pub peak_bound_per_source: Vec<f64>,
    /// 声明式 headroom：参考输入 0.5 峰值 × max|G| 的预测输出峰值（满刻度 1.0）
    pub predicted_peak_per_source: Vec<f64>,
    pub global_scale: f64,
    pub effort: f64,
    /// E_effort / E_effort_baseline，baseline = δ_D identity（每源 1 个 tap=1）→ E=N。
    pub effort_vs_baseline: f64,
    pub gain_cap_scale: Vec<f64>,
    pub gain_cap_triggered: bool,
    pub headroom_triggered: bool,
    pub headroom_ok: bool,
    pub total_delay_samples: usize,
    pub applied: bool,
}

pub const GAIN_CAP_DB: f64 = 6.0;
/// 声明式参考输入峰值（−6 dBFS）。
pub const REFERENCE_INPUT_PEAK: f64 = 0.5;
/// 预测输出峰值上限（满刻度 = 1.0）。
pub const PREDICTED_PEAK_LIMIT: f64 = 1.0;

fn db20(x: f64) -> f64 {
    20.0 * x.log10()
}

/// 对 FIR 施加增益帽（+6 dB），并计算声明式 headroom 与 effort 报告。
/// 返回缩放后的 h、实际 G（精确频点）与约束报告。
pub fn enforce_fir_constraints(
    h: &[Vec<f64>],
    freqs_hz: &[f64],
    fs: f64,
    gain_cap_db: f64,
    total_delay_samples: usize,
    applied: bool,
) -> Result<(Vec<Vec<f64>>, Vec<Vec<Cx>>, FirConstraintReport), String> {
    let n_src = h.len();
    let mut h = h.to_vec();
    let mut gain_cap_scale = vec![1.0f64; n_src];
    let mut gain_cap_triggered = false;
    for n in 0..n_src {
        let g = fir::fir_response(&h[n], freqs_hz, fs);
        let mut max_db = f64::NEG_INFINITY;
        for v in &g {
            let a = v.abs();
            if a > 0.0 {
                max_db = max_db.max(db20(a));
            }
        }
        if max_db > gain_cap_db {
            let scale = 10.0f64.powf((gain_cap_db - max_db) / 20.0);
            gain_cap_scale[n] = scale;
            gain_cap_triggered = true;
            for t in h[n].iter_mut() {
                *t *= scale;
            }
        }
    }
    let mut g_actual = Vec::with_capacity(n_src);
    let mut per_source_max_gain_db = Vec::with_capacity(n_src);
    let mut predicted_peak_per_source = Vec::with_capacity(n_src);
    for src in &h {
        let g = fir::fir_response(src, freqs_hz, fs);
        let mut max_db = f64::NEG_INFINITY;
        for v in &g {
            let a = v.abs();
            if a > 0.0 {
                max_db = max_db.max(db20(a));
            }
        }
        per_source_max_gain_db.push(max_db);
        predicted_peak_per_source.push(REFERENCE_INPUT_PEAK * 10.0f64.powf(max_db / 20.0));
        g_actual.push(g);
    }
    let headroom_ok = predicted_peak_per_source
        .iter()
        .all(|p| *p <= PREDICTED_PEAK_LIMIT + 1e-12);
    let peaks: Vec<f64> = h
        .iter()
        .map(|src| src.iter().map(|v| v.abs()).sum::<f64>())
        .collect();
    let effort: f64 = h.iter().flat_map(|s| s.iter()).map(|v| v * v).sum();
    Ok((
        h,
        g_actual,
        FirConstraintReport {
            per_source_max_gain_db,
            peak_bound_per_source: peaks,
            predicted_peak_per_source,
            global_scale: 1.0,
            effort,
            effort_vs_baseline: effort / n_src.max(1) as f64,
            gain_cap_scale,
            gain_cap_triggered,
            headroom_triggered: !headroom_ok,
            headroom_ok,
            total_delay_samples,
            applied,
        },
    ))
}

/// B0/B1 的约束报告（不做 FIR 缩放；如实给出标量参数本身）。
#[derive(Clone, Debug)]
pub struct ScalarConstraintReport {
    pub per_source_max_gain_db: Vec<f64>,
    /// B1 为单 tap：|h| = |gain|（单位峰值输入的峰值界）；B0 identity = 1。
    pub peak_bound_per_source: Vec<f64>,
    pub global_scale: f64,
    pub effort: f64,
    pub effort_vs_baseline: f64,
    pub total_delay_samples: usize,
    /// 10·log10(Σdesign evaluated 带内能量_after / Σdesign 同口径能量_B0)。
    pub identity_energy_ratio_db: f64,
    pub headroom_applied: bool,
}

/// B0 的约束报告：identity 参考（G=1）；不做任何缩放。
pub fn report_identity(n_sources: usize, identity_energy_ratio_db: f64) -> ScalarConstraintReport {
    ScalarConstraintReport {
        per_source_max_gain_db: vec![0.0; n_sources],
        peak_bound_per_source: vec![1.0; n_sources],
        global_scale: 1.0,
        effort: n_sources as f64,
        effort_vs_baseline: 1.0,
        total_delay_samples: 0,
        identity_energy_ratio_db,
        headroom_applied: false,
    }
}

/// B1 的约束报告：单 tap 幅度 = |gain|；delay 记为其自身对齐延迟（非公共 D）。
pub fn report_b1(
    params: &[crate::baseline::B1SourceParams],
    identity_energy_ratio_db: f64,
    level_tolerance_db: f64,
) -> Result<ScalarConstraintReport, String> {
    let n = params.len();
    let mut max_gain = Vec::with_capacity(n);
    let mut peaks = Vec::with_capacity(n);
    let mut effort = 0.0;
    let mut max_delay = 0usize;
    if !(identity_energy_ratio_db.is_finite()
        && identity_energy_ratio_db.abs() <= level_tolerance_db + 1e-9)
    {
        return Err(format!(
            "B1 电平约束违反: {} dB (|.| <= {})",
            identity_energy_ratio_db, level_tolerance_db
        ));
    }
    for p in params {
        let linear = p.linear_gain().abs();
        let db = 20.0 * linear.log10();
        if db > crate::baseline::B1_GAIN_CAP_DB + 1e-9 {
            return Err(format!("B1 参数增益 {} dB 超过 +6 dB 帽", db));
        }
        max_gain.push(db);
        peaks.push(linear);
        effort += linear * linear;
        max_delay = max_delay.max(p.delay_samples.max(0) as usize);
    }
    Ok(ScalarConstraintReport {
        per_source_max_gain_db: max_gain,
        peak_bound_per_source: peaks,
        global_scale: 1.0,
        effort,
        effort_vs_baseline: effort / n.max(1) as f64,
        total_delay_samples: max_delay,
        identity_energy_ratio_db,
        headroom_applied: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn impulse(n: usize) -> Vec<f64> {
        let mut h = vec![0.0; n];
        h[0] = 1.0;
        h
    }

    #[test]
    fn gain_cap_scales_source() {
        // 增益 4x (+12 dB) → 帽到 +6 dB（2x）；不再有 Σ|h| 全局缩放
        let mut h0 = impulse(16);
        h0[0] = 4.0;
        let freqs = [40.0, 80.0];
        let (h, g, rep) = enforce_fir_constraints(
            &[h0.clone(), impulse(16)],
            &freqs,
            48000.0,
            GAIN_CAP_DB,
            1024,
            true,
        )
        .expect("constraints");
        assert!(rep.gain_cap_triggered);
        let expected = 10.0f64.powf(6.0 / 20.0);
        assert!(
            (g[0][0].abs() - expected).abs() < 1e-9,
            "|G|={}",
            g[0][0].abs()
        );
        assert!((h[0][0] - expected).abs() < 1e-9);
        assert!((rep.global_scale - 1.0).abs() < 1e-15);
        assert!(rep.headroom_ok);
    }

    #[test]
    fn headroom_is_declarative_not_l1() {
        // 32 tap 全 0.05：Σ|h|=1.6，但 in-band max|G|≈1.6 < 2 → 不缩放、headroom_ok
        let h0 = vec![0.05f64; 32];
        let freqs = [40.0];
        let (_h, _g, rep) =
            enforce_fir_constraints(&[h0], &freqs, 48000.0, GAIN_CAP_DB, 1024, true)
                .expect("constraints");
        assert!(!rep.gain_cap_triggered);
        assert!(!rep.headroom_triggered);
        assert!(rep.headroom_ok);
        assert!((rep.peak_bound_per_source[0] - 1.6).abs() < 1e-12);
        assert!(rep.predicted_peak_per_source[0] <= 1.0);
        assert!((rep.global_scale - 1.0).abs() < 1e-15);
    }

    #[test]
    fn b1_report_rejects_out_of_cap() {
        use crate::baseline::B1SourceParams;
        let p = [B1SourceParams {
            delay_samples: 0,
            gain_db: 6.0,
            polarity: 1.0,
        }];
        let rep = report_b1(&p, 0.0, 1.0).expect("ok");
        assert!((rep.per_source_max_gain_db[0] - 6.0).abs() < 1e-9);
        assert!((rep.peak_bound_per_source[0] - 10.0f64.powf(6.0 / 20.0)).abs() < 1e-12);
        let bad = [B1SourceParams {
            delay_samples: 0,
            gain_db: 7.0,
            polarity: 1.0,
        }];
        assert!(report_b1(&bad, 0.0, 1.0).is_err());
        assert!(report_b1(&p, 2.0, 1.0).is_err());
    }
}
