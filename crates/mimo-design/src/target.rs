//! 目标函数 A(f)（契约 §6.1）：design 基线空间 RMS 幅度 + 功率域 1/12 oct 平滑，相位 0。
//!
//! A(f_k) = beta · sqrt( smooth_1/12oct( mean_{m∈D} |P_m^0(f_k)|² ) )
//! - 只用 design 点（holdout 绝不参与）；
//! - 目标在设计中冻结；
//! - beta 默认 0.70（契约 §8 config.target_beta）：目标电平比基线低约 3.1 dB，用于给
//!   因果 FIR 拟合留出 headroom/正则化余量；不是测量结论。

use crate::solve::Cx;

/// 每频点的 design 空间平均功率 mean_m |P_m(f)|²。
pub fn spatial_mean_power(p_design: &[Vec<Cx>]) -> Vec<f64> {
    if p_design.is_empty() {
        return Vec::new();
    }
    let k = p_design[0].len();
    let mut out = vec![0.0f64; k];
    for p in p_design {
        for (ki, v) in p.iter().enumerate() {
            out[ki] += v.norm_sqr();
        }
    }
    let inv = 1.0 / p_design.len() as f64;
    for v in out.iter_mut() {
        *v *= inv;
    }
    out
}

/// 功率域 1/12 oct 平滑（每频点窗口 = [f·2^(-1/24), f·2^(+1/24)]，等权平均）。
/// 频点按升序；窗口按频率比判定，不假设均匀格点。
pub fn smooth_power_one_twelfth_oct(freqs_hz: &[f64], power: &[f64]) -> Vec<f64> {
    let n = freqs_hz.len();
    if n == 0 {
        return Vec::new();
    }
    debug_assert_eq!(n, power.len());
    let half = (2.0f64).powf(1.0 / 24.0);
    let mut out = vec![0.0f64; n];
    let mut lo = 0usize;
    let mut hi = 0usize;
    for i in 0..n {
        let f = freqs_hz[i];
        let low = f / half;
        let high = f * half;
        while lo < n && freqs_hz[lo] < low {
            lo += 1;
        }
        if hi < lo {
            hi = lo;
        }
        while hi < n && freqs_hz[hi] <= high {
            hi += 1;
        }
        if hi > lo {
            let mut sum = 0.0;
            for v in &power[lo..hi] {
                sum += *v;
            }
            out[i] = sum / (hi - lo) as f64;
        } else {
            out[i] = power[i];
        }
    }
    out
}

/// 构建目标幅度 A(f_k)（实数、相位 0），长度 = 频点数。
pub fn build_target(freqs_hz: &[f64], p0_design: &[Vec<Cx>], beta: f64) -> Vec<f64> {
    let mean_pow = spatial_mean_power(p0_design);
    let smoothed = smooth_power_one_twelfth_oct(freqs_hz, &mean_pow);
    smoothed.iter().map(|v| beta * v.max(0.0).sqrt()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cx(re: f64, im: f64) -> Cx {
        Cx::new(re, im)
    }

    #[test]
    fn spatial_mean_power_two_mics() {
        // m1: |1+2j|²=5, m2: |3|²=9 → 均值 7
        let p = vec![
            vec![cx(1.0, 2.0), cx(3.0, 0.0)],
            vec![cx(3.0, 0.0), cx(0.0, 1.0)],
        ];
        let pow = spatial_mean_power(&p);
        assert!((pow[0] - 7.0).abs() < 1e-15);
        assert!((pow[1] - 5.0).abs() < 1e-15);
    }

    #[test]
    fn smoothing_averages_neighbours() {
        // 1/24 oct 网格：25.0, 25.0*2^(1/24), 25*2^(2/24), 25*2^(3/24)
        let f0 = 25.0f64;
        let freqs: Vec<f64> = (0..4)
            .map(|i| f0 * (2.0f64).powf(i as f64 / 24.0))
            .collect();
        let power = vec![1.0, 9.0, 9.0, 1.0];
        let s = smooth_power_one_twelfth_oct(&freqs, &power);
        // 中间点窗口含相邻 1/24 oct 邻居（±1/24 oct 恰好含相邻格点）
        assert!(
            (s[1] - (1.0 + 9.0 + 9.0) / 3.0).abs() < 1e-12,
            "s1={}",
            s[1]
        );
        assert!((s[0] - (1.0 + 9.0) / 2.0).abs() < 1e-12, "s0={}", s[0]);
        assert!((s[3] - (9.0 + 1.0) / 2.0).abs() < 1e-12, "s3={}", s[3]);
    }

    #[test]
    fn target_applies_beta() {
        let freqs = vec![25.0, 50.0, 100.0];
        let p = vec![vec![cx(2.0, 0.0); 3], vec![cx(0.0, 2.0); 3]];
        let a = build_target(&freqs, &p, 0.7);
        for v in a {
            assert!((v - 0.7 * 2.0).abs() < 1e-12, "A={}", v);
        }
    }
}
