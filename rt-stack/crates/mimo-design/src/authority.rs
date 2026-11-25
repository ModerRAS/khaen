//! 4.4 权威加权：正则项按频点加权 `Γ_s(f) = Γ0,s / |H_s(f)|²`（按该音箱通带均值归一化）。
//!
//! 语义：**在该频点本来就弱的音箱自动少被派活**。岭正则项
//! `λ Σ_s Γ_s(f)·|f_s|²` 中，弱频点的 Γ 大 → 该源在该频点被压得更狠；强频点 Γ 小 →
//! 允许它多出力。归一化用**该源通带均值** |H_s|² 作 Γ0，使 Γ 通带平均 ≈ 1，不携带量纲
//! （否则不同夹具/电平的 |H|² 绝对值会直接改变 λ 的有效大小，等于偷改正则强度）。
//!
//! 口径：
//! - `|H_s(f)|²` 取 **design 测点空间均值** `mean_m |H[m][n][f]|²`（单点 = 该点）；
//! - 通带 `passband_hz` 默认取设计频点的全带（[AuthorityConfig]）；
//! - 分母下界 `eps_rel · Γ0,s` 防除 0；Γ 上限 `max_gamma`（默认 1e4）防止"完全没有
//!   权威"的频点把解冻成 0；
//! - 默认**关闭**：关闭时求解器走旧的标量 λ 路径，结果与旧版**逐位一致**。
//!
//! 与 [crate::quality] 的分工：quality 判"这条测量能不能用"（Unknown 排除拟合）；
//! 本模块处理"测量合格、但该频点该箱本来就弱"（能力不足）。两者不互相替代。

use crate::report::Json;
use crate::solve::Cx;

/// |H|² 相对下界（相对该源通带均值的比例）。
pub const DEFAULT_EPS_REL: f64 = 1e-6;
/// Γ 上限（默认 +60 dB 的"少派活"压力，可配）。
pub const DEFAULT_MAX_GAMMA: f64 = 1e4;

/// 权威加权配置（RunConfig.authority；默认关闭）。
#[derive(Clone, Debug)]
pub struct AuthorityConfig {
    pub enabled: bool,
    /// 归一化通带（Hz）；None = 用全部设计频点。
    pub passband_hz: Option<(f64, f64)>,
    /// |H|² 相对下界（> 0）
    pub eps_rel: f64,
    /// Γ 上限（>= 1）
    pub max_gamma: f64,
}

impl Default for AuthorityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            passband_hz: None,
            eps_rel: DEFAULT_EPS_REL,
            max_gamma: DEFAULT_MAX_GAMMA,
        }
    }
}

impl AuthorityConfig {
    /// 启用（通带 = 全部设计频点）。
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }
}

/// 权威权重结果。
#[derive(Clone, Debug)]
pub struct AuthorityWeights {
    /// Γ_s(f)：[bin][source]（= 1 表示不加权）。
    pub gamma: Vec<Vec<f64>>,
    /// |H_s(f)|²（design 点空间均值）：[bin][source]。
    pub mean_sq: Vec<Vec<f64>>,
    /// 每源通带均值 |H_s|²（= Γ0,s）：[source]。
    pub passband_mean_sq: Vec<f64>,
    /// 被 max_gamma 截断的 (bin, source)。
    pub clipped: Vec<(usize, usize)>,
    /// 通带内 |H|² 恒为 0 的源（无权威）。
    pub empty_sources: Vec<usize>,
}

impl AuthorityWeights {
    /// 某频点的 Γ 行（按 source）。
    pub fn gamma_at(&self, bin: usize) -> Option<&[f64]> {
        self.gamma.get(bin).map(|row| row.as_slice())
    }

    /// 每源 Γ 的最小/最大值（报告用）。
    pub fn gamma_range(&self, source: usize) -> Option<(f64, f64)> {
        let mut min = f64::INFINITY;
        let mut max = 0.0f64;
        let mut any = false;
        for bin in &self.gamma {
            if let Some(g) = bin.get(source) {
                min = min.min(*g);
                max = max.max(*g);
                any = true;
            }
        }
        if any {
            Some((min, max))
        } else {
            None
        }
    }

    /// 权威加权 JSON（报告用）。
    pub fn json(&self, cfg: &AuthorityConfig, source_ids: &[String]) -> Json {
        let per_source: Vec<(String, Json)> = (0..self.passband_mean_sq.len())
            .map(|n| {
                let (min_g, max_g) = self.gamma_range(n).unwrap_or((1.0, 1.0));
                let clipped = self.clipped.iter().filter(|(_, s)| *s == n).count();
                (
                    source_ids
                        .get(n)
                        .cloned()
                        .unwrap_or_else(|| format!("src{n}")),
                    Json::Obj(vec![
                        (
                            "passband_mean_sq".to_string(),
                            Json::num(self.passband_mean_sq[n]),
                        ),
                        ("gamma_min".to_string(), Json::num(min_g)),
                        ("gamma_max".to_string(), Json::num(max_g)),
                        ("clipped_bins".to_string(), Json::Num(clipped as f64)),
                        (
                            "empty".to_string(),
                            Json::Bool(self.empty_sources.contains(&n)),
                        ),
                    ]),
                )
            })
            .collect();
        let mut fields = vec![
            (
                "form".to_string(),
                Json::str("gamma_s(f) = passband_mean_sq_s / max(mean_m|H_ms(f)|^2, eps_rel*passband_mean_sq_s), clamp max_gamma"),
            ),
            ("eps_rel".to_string(), Json::num(cfg.eps_rel)),
            ("max_gamma".to_string(), Json::num(cfg.max_gamma)),
            (
                "passband_hz".to_string(),
                match cfg.passband_hz {
                    Some((lo, hi)) => Json::Arr(vec![Json::num(lo), Json::num(hi)]),
                    None => Json::Null,
                },
            ),
            (
                "clipped_count".to_string(),
                Json::Num(self.clipped.len() as f64),
            ),
            ("per_source".to_string(), Json::Obj(per_source)),
        ];
        if !self.empty_sources.is_empty() {
            fields.push((
                "empty_sources".to_string(),
                Json::Arr(
                    self.empty_sources
                        .iter()
                        .map(|n| Json::str(source_ids.get(*n).cloned().unwrap_or_default()))
                        .collect(),
                ),
            ));
        }
        Json::Obj(fields)
    }
}

fn validate(cfg: &AuthorityConfig) -> Result<(), String> {
    if !(cfg.eps_rel.is_finite() && cfg.eps_rel > 0.0 && cfg.eps_rel < 1.0) {
        return Err(format!("authority: eps_rel={} 必须在 (0,1)", cfg.eps_rel));
    }
    if !(cfg.max_gamma.is_finite() && cfg.max_gamma >= 1.0) {
        return Err(format!("authority: max_gamma={} 必须 >= 1", cfg.max_gamma));
    }
    if let Some((lo, hi)) = cfg.passband_hz {
        if !(lo.is_finite() && hi.is_finite() && hi > lo) {
            return Err(format!("authority: 通带 {lo}..{hi} 非法"));
        }
    }
    Ok(())
}

/// 计算权威权重 Γ（纯函数；enabled 由调用方判断，本函数总是给出一套权重）。
///
/// - h_design: [design mic][bin][source]；
/// - freqs_hz: 长度 = bin 数；
/// - 返回 [bin][source] 的 Γ，通带均值 ≈ 1。
pub fn authority_weights(
    h_design: &[Vec<Vec<Cx>>],
    freqs_hz: &[f64],
    cfg: &AuthorityConfig,
) -> Result<AuthorityWeights, String> {
    validate(cfg)?;
    if h_design.is_empty() {
        return Err("authority: 空 H".to_string());
    }
    let k = freqs_hz.len();
    if k == 0 {
        return Err("authority: 空频点".to_string());
    }
    let m = h_design.len();
    if h_design.iter().any(|mic| mic.len() != k) {
        return Err("authority: H 的 bin 数与频点数不符".to_string());
    }
    let n_src = h_design[0][0].len();
    if n_src == 0 {
        return Err("authority: 无源".to_string());
    }
    if h_design
        .iter()
        .any(|mic| mic.iter().any(|hk| hk.len() != n_src))
    {
        return Err("authority: 各频点源数不一致".to_string());
    }
    // |H_s(f)|² = mean_m |H[m][n][f]|²
    let mut mean_sq = vec![vec![0.0f64; n_src]; k];
    for (ki, row) in mean_sq.iter_mut().enumerate() {
        for (n, value) in row.iter_mut().enumerate() {
            let mut sum = 0.0;
            for mic in h_design {
                sum += mic[ki][n].norm_sqr();
            }
            *value = sum / m as f64;
        }
    }
    // 通带内每源均值 Γ0
    let in_passband = |f: f64| match cfg.passband_hz {
        Some((lo, hi)) => f >= lo && f < hi,
        None => true,
    };
    let mut passband_mean_sq = vec![0.0f64; n_src];
    let mut empty_sources = Vec::new();
    for n in 0..n_src {
        let mut sum = 0.0;
        let mut count = 0usize;
        for (ki, f) in freqs_hz.iter().enumerate() {
            if in_passband(*f) {
                sum += mean_sq[ki][n];
                count += 1;
            }
        }
        if count == 0 {
            return Err(format!("authority: 通带 {:?} 内没有频点", cfg.passband_hz));
        }
        passband_mean_sq[n] = sum / count as f64;
        if !(passband_mean_sq[n].is_finite() && passband_mean_sq[n] > 0.0) {
            empty_sources.push(n);
            passband_mean_sq[n] = 0.0;
        }
    }
    // Γ_s(f)
    let mut gamma = vec![vec![1.0f64; n_src]; k];
    let mut clipped = Vec::new();
    let lower = 1.0 / cfg.max_gamma;
    for n in 0..n_src {
        if passband_mean_sq[n] <= 0.0 {
            continue;
        }
        let floor = cfg.eps_rel * passband_mean_sq[n];
        for ki in 0..k {
            let denom = mean_sq[ki][n].max(floor);
            let raw = passband_mean_sq[n] / denom;
            if raw > cfg.max_gamma {
                clipped.push((ki, n));
            }
            gamma[ki][n] = raw.clamp(lower, cfg.max_gamma);
        }
    }
    Ok(AuthorityWeights {
        gamma,
        mean_sq,
        passband_mean_sq,
        clipped,
        empty_sources,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cx(re: f64) -> Cx {
        Cx::new(re, 0.0)
    }

    fn h_two_sources(mic_rows: &[&[(f64, f64)]]) -> Vec<Vec<Vec<Cx>>> {
        mic_rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|(a, b)| vec![cx(*a), cx(*b)])
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    #[test]
    fn gamma_inverts_squared_magnitude() {
        // 2 bins 通带；源0 恒 1；源1 bin0=1 bin1=0.1 → 通带均值 0.505
        let h = h_two_sources(&[&[(1.0, 1.0), (1.0, 0.1)]]);
        let cfg = AuthorityConfig::enabled();
        let w = authority_weights(&h, &[63.0, 100.0], &cfg).expect("weights");
        assert!((w.passband_mean_sq[0] - 1.0).abs() < 1e-12);
        assert!((w.passband_mean_sq[1] - 0.505).abs() < 1e-12);
        assert!((w.gamma[0][0] - 1.0).abs() < 1e-12);
        // 弱频点 Γ 大：0.505 / 0.01 = 50.5
        assert!((w.gamma[1][1] - 50.5).abs() < 1e-9, "{:?}", w.gamma);
        // 弱频点的 Γ 必须远大于强频点
        assert!(w.gamma[1][1] > w.gamma[0][1] * 10.0);
    }

    #[test]
    fn empty_source_stays_neutral_and_is_reported() {
        // 源1 全为零 → 无权威，Γ=1（H 列本身为 0 已经让它不参与；不造无穷权重）
        let h = h_two_sources(&[&[(1.0, 0.0), (1.0, 0.0)]]);
        let w = authority_weights(&h, &[63.0, 100.0], &AuthorityConfig::enabled()).expect("w");
        assert_eq!(w.empty_sources, vec![1]);
        assert!((w.gamma[0][1] - 1.0).abs() < 1e-15);
        assert!((w.gamma[1][1] - 1.0).abs() < 1e-15);
        let text = w
            .json(&AuthorityConfig::enabled(), &["SUB1".into(), "SUB2".into()])
            .to_compact();
        assert!(text.contains(r#""empty_sources":["SUB2"]"#), "{text}");
    }

    #[test]
    fn max_gamma_clips_and_is_reported() {
        // 全带都弱 → 归一化抵消，Γ≈1，不截断
        let h = h_two_sources(&[&[(1.0, 1e-6), (1.0, 1e-6)]]);
        let cfg = AuthorityConfig {
            max_gamma: 1e3,
            ..AuthorityConfig::enabled()
        };
        let w = authority_weights(&h, &[63.0, 100.0], &cfg).expect("w");
        assert!(w.clipped.is_empty(), "{:?}", w.clipped);
        // 单频点弱、另一频点强 → 截断到 max_gamma
        let h = h_two_sources(&[&[(1.0, 1.0), (1.0, 1e-6)]]);
        let w = authority_weights(&h, &[63.0, 100.0], &cfg).expect("w");
        assert_eq!(w.clipped, vec![(1, 1)]);
        assert!((w.gamma[1][1] - 1e3).abs() < 1e-9);
    }

    #[test]
    fn passband_restricts_normalization() {
        // 通带只含 63 Hz：Γ0 只用 bin0；100 Hz 处 Γ 允许偏离 1
        let h = h_two_sources(&[&[(1.0, 1.0), (1.0, 0.5)]]);
        let cfg = AuthorityConfig {
            passband_hz: Some((50.0, 70.0)),
            ..AuthorityConfig::enabled()
        };
        let w = authority_weights(&h, &[63.0, 100.0], &cfg).expect("w");
        assert!((w.passband_mean_sq[1] - 1.0).abs() < 1e-12);
        assert!((w.gamma[1][1] - 4.0).abs() < 1e-12);
    }

    /// ② 空权威（该频点本来就弱）的音箱在谷点不被派活：Γ 加权后驱动量骤降。
    ///
    /// 2 个频点、2 源：源2 在 bin0 强（|H|=1）、bin1 是谷（|H|=0.1）。
    /// 只看 bin1：不加权时 LS 给 f2≈0.098，加权后 Γ2=50.5 → f2≈0.002（< 1/20）。
    #[test]
    fn authority_gamma_suppresses_drive_at_a_valley() {
        use crate::solve::{weighted_ridge_ls_complex, weighted_ridge_ls_complex_with_ridge, Cx};
        let freqs = [63.0, 100.0];
        // h[mic][bin][source]：1 个测点
        let h = vec![vec![
            vec![Cx::new(1.0, 0.0), Cx::new(1.0, 0.0)],
            vec![Cx::new(1.0, 0.0), Cx::new(0.1, 0.0)],
        ]];
        let w = authority_weights(&h, &freqs, &AuthorityConfig::enabled()).expect("weights");
        assert!(w.gamma[1][1] > 40.0, "谷点 Γ 应远大于 1: {}", w.gamma[1][1]);
        let lambda = 0.01;
        let target = Cx::new(1.0, 0.0);
        let one_mic_bin1 = vec![vec![Cx::new(1.0, 0.0), Cx::new(0.1, 0.0)]];
        let f_off = weighted_ridge_ls_complex(&one_mic_bin1, target, &[1.0], lambda).expect("off");
        let f_on = weighted_ridge_ls_complex_with_ridge(
            &one_mic_bin1,
            target,
            &[1.0],
            lambda,
            Some(&w.gamma[1]),
        )
        .expect("on");
        println!("off f1={} f2={}", f_off[0].re, f_off[1].re);
        assert!(
            f_off[1].re > 0.05,
            "不加权时谷点驱动应明显: {}",
            f_off[1].re
        );
        assert!(
            f_on[1].re < f_off[1].re * 0.05,
            "加权后谷点驱动未被压下去: on={} off={}",
            f_on[1].re,
            f_off[1].re
        );
        assert!(
            f_on[1].re.abs() < 0.01,
            "加权后 f2={} 应 < 0.01",
            f_on[1].re
        );
        // 强源仍承担主要驱动
        assert!(f_on[0].re > 0.9, "强源驱动 f1={}", f_on[0].re);
    }

    #[test]
    fn validation_rejects_bad_config() {
        let h = h_two_sources(&[&[(1.0, 1.0)]]);
        let mut cfg = AuthorityConfig::enabled();
        cfg.eps_rel = 0.0;
        assert!(authority_weights(&h, &[63.0], &cfg).is_err());
        let cfg = AuthorityConfig {
            max_gamma: 0.5,
            ..AuthorityConfig::enabled()
        };
        assert!(authority_weights(&h, &[63.0], &cfg).is_err());
        let cfg = AuthorityConfig {
            passband_hz: Some((200.0, 300.0)),
            ..AuthorityConfig::enabled()
        };
        assert!(authority_weights(&h, &[63.0], &cfg).is_err());
    }
}
