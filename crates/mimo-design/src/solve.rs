//! 逐频点加权岭最小二乘（契约 §6.2/§6.3）。
//!
//! 每个频点 k 解
//!
//!   F(f) = argmin_f  sum_{m∈D} w_m |sum_n H_mn(f) f_n - A(f)|² + λ sum_n |f_n|²
//!        => (H^H W H + λI) f = H^H W a
//!
//! N <= 4，用带部分主元（复数模最大）的高斯消元。IRLS：用当前 FIR 的实际 design
//! 残差更新 w_m ∝ clamp((r_m/mean r)^0.5, 0.25, 4)，共 2 次外迭代。
//!
//! 本模块零 room-sim 依赖，便于用手算小矩阵单元测试。

use crate::fir;
use crate::target;

/// 内部复数值类型（f64 实部/虚部）。只在 mimo-design 内部使用；
/// CLI 与 room-sim 的 Complex 之间做显式转换。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cx {
    pub re: f64,
    pub im: f64,
}

impl Cx {
    pub const fn new(re: f64, im: f64) -> Self {
        Self { re, im }
    }
    pub const fn zero() -> Self {
        Self { re: 0.0, im: 0.0 }
    }
    pub const fn one() -> Self {
        Self { re: 1.0, im: 0.0 }
    }
    #[inline]
    pub fn add(self, o: Self) -> Self {
        Self::new(self.re + o.re, self.im + o.im)
    }
    #[inline]
    pub fn sub(self, o: Self) -> Self {
        Self::new(self.re - o.re, self.im - o.im)
    }
    #[inline]
    pub fn mul(self, o: Self) -> Self {
        Self::new(
            self.re * o.re - self.im * o.im,
            self.re * o.im + self.im * o.re,
        )
    }
    #[inline]
    pub fn scale(self, s: f64) -> Self {
        Self::new(self.re * s, self.im * s)
    }
    #[inline]
    pub fn conj(self) -> Self {
        Self::new(self.re, -self.im)
    }
    #[inline]
    pub fn norm_sqr(self) -> f64 {
        self.re * self.re + self.im * self.im
    }
    #[inline]
    pub fn abs(self) -> f64 {
        self.norm_sqr().sqrt()
    }
    #[inline]
    pub fn recip(self) -> Self {
        let d = self.norm_sqr();
        Self::new(self.re / d, -self.im / d)
    }
    /// 复角（rad）：atan2(im, re)。
    #[inline]
    pub fn arg(self) -> f64 {
        self.im.atan2(self.re)
    }
    /// 纯相位旋转 exp(-jθ)。
    #[inline]
    pub fn exp_neg_j(theta: f64) -> Self {
        Self::new(theta.cos(), -theta.sin())
    }
    /// 纯相位旋转 exp(+jθ)。
    #[inline]
    pub fn exp_pos_j(theta: f64) -> Self {
        Self::new(theta.cos(), theta.sin())
    }
    #[inline]
    fn key(self) -> f64 {
        self.norm_sqr()
    }
}

/// 解复线性方程组 A x = b（A 为 n×n，原地消元 + 回代），部分主元（模最大）。
pub fn solve_linear(mut a: Vec<Vec<Cx>>, mut b: Vec<Cx>) -> Result<Vec<Cx>, String> {
    let n = b.len();
    if a.len() != n || a.iter().any(|row| row.len() != n) {
        return Err("solve_linear: 维数不匹配".to_string());
    }
    for col in 0..n {
        let mut piv = col;
        let mut best = a[col][col].key();
        for r in col + 1..n {
            let v = a[r][col].key();
            if v > best {
                best = v;
                piv = r;
            }
        }
        if !(best.is_finite() && best > 1e-28) {
            return Err(format!("solve_linear: 第 {} 列主元近零（{:e}）", col, best));
        }
        if piv != col {
            a.swap(piv, col);
            b.swap(piv, col);
        }
        let inv = a[col][col].recip();
        for r in col + 1..n {
            let factor = a[r][col].mul(inv);
            if factor.key() == 0.0 {
                a[r][col] = Cx::zero();
                continue;
            }
            for c in col..n {
                let t = factor.mul(a[col][c]);
                a[r][c] = a[r][c].sub(t);
            }
            let t = factor.mul(b[col]);
            b[r] = b[r].sub(t);
            a[r][col] = Cx::zero();
        }
    }
    let mut x = vec![Cx::zero(); n];
    for i in (0..n).rev() {
        let mut s = b[i];
        for j in i + 1..n {
            s = s.sub(a[i][j].mul(x[j]));
        }
        x[i] = s.mul(a[i][i].recip());
    }
    Ok(x)
}

/// 单频点加权岭最小二乘（实数目标 A(f)，相位 0）。
pub fn weighted_ridge_ls(
    h: &[Vec<Cx>],
    target: f64,
    weights: &[f64],
    lambda: f64,
) -> Result<Vec<Cx>, String> {
    weighted_ridge_ls_complex(h, Cx::new(target, 0.0), weights, lambda)
}

/// 单频点加权岭最小二乘（复数目标）。
///
/// - h: h[mic][source]（只含 design 点；mic 顺序与 weights 一致）；
/// - target: 复数目标 C；
/// - weights: 每 mic 权重 w_m；
/// - lambda: 岭正则 λ（绝对，非相对值）。
pub fn weighted_ridge_ls_complex(
    h: &[Vec<Cx>],
    target: Cx,
    weights: &[f64],
    lambda: f64,
) -> Result<Vec<Cx>, String> {
    weighted_ridge_ls_complex_with_ridge(h, target, weights, lambda, None)
}

/// 每源每频点复响应幅值帽（+6 dB，契约 §6.6 的带内增益帽在频域解里前馈执行）。
pub const F_MAG_CAP_DB: f64 = 6.0;

/// 单频点加权岭最小二乘（每 mic 独立复数目标 target_m）。
pub fn weighted_ridge_ls_targets(
    h: &[Vec<Cx>],
    targets: &[Cx],
    weights: &[f64],
    lambda: f64,
) -> Result<Vec<Cx>, String> {
    weighted_ridge_ls_targets_with_ridge(h, targets, weights, lambda, None)
}

/// 单频点求解并投影到每源幅值帽 |F_n| <= cap（保留相位）。
///
/// 实现（确定性 active-set）：先解无约束岭 LS；对越界源投影到 cap 并固定；用
/// "每 mic 目标 A − 固定源在该 mic 的贡献" 对剩余源重解；重复至多 3 次。
/// 目的：禁止深 null 处 F 爆到极大（throat：会让 FIR 系数爆炸 → headroom 缩放 → 静音式假改善）。
pub fn weighted_ridge_ls_capped(
    h: &[Vec<Cx>],
    target: f64,
    weights: &[f64],
    lambda: f64,
    cap: f64,
) -> Result<Vec<Cx>, String> {
    weighted_ridge_ls_capped_with_ridge(h, target, weights, lambda, cap, None)
}

/// 由目标幅度 A(f) 设计一轮 MIMO FIR（per-(mic,bin) 权重版，供 OBJECTIVES-V2 的 IRLS 使用）。
#[allow(clippy::too_many_arguments)]
pub fn design_mimo_once_w(
    freqs_hz: &[f64],
    fs: f64,
    h_design: &[Vec<Vec<Cx>>],
    a_target: &[f64],
    weights: &[Vec<f64>],
    lambda: f64,
    taps: usize,
    delay_samples: usize,
    diag_load_rel: f64,
    nfft: usize,
) -> Result<FirDesign, String> {
    design_mimo_once_w_with(
        freqs_hz,
        fs,
        h_design,
        a_target,
        weights,
        lambda,
        taps,
        delay_samples,
        diag_load_rel,
        nfft,
        DesignOptions::default(),
    )
}

/// 单频点求解并投影到每源幅值帽（每 mic 独立复数目标，OBJECTIVES-V2 的 phase-fixed 幅度目标）。
pub fn weighted_ridge_ls_capped_targets(
    h: &[Vec<Cx>],
    targets: &[Cx],
    weights: &[f64],
    lambda: f64,
    cap: f64,
) -> Result<Vec<Cx>, String> {
    weighted_ridge_ls_capped_targets_with_ridge(h, targets, weights, lambda, cap, None)
}

/// 每频点目标不同（targets[mic][bin]）的 MIMO FIR 一轮设计（OBJECTIVES-V2）。
#[allow(clippy::too_many_arguments)]
pub fn design_mimo_once_multi_targets(
    freqs_hz: &[f64],
    fs: f64,
    h_design: &[Vec<Vec<Cx>>],
    targets: &[Vec<Cx>],
    weights: &[Vec<f64>],
    lambda: f64,
    taps: usize,
    delay_samples: usize,
    diag_load_rel: f64,
    nfft: usize,
) -> Result<FirDesign, String> {
    design_mimo_once_multi_targets_with(
        freqs_hz,
        fs,
        h_design,
        targets,
        weights,
        lambda,
        taps,
        delay_samples,
        diag_load_rel,
        nfft,
        DesignOptions::default(),
    )
}

/// 一轮 MIMO FIR 设计结果（B2/B3 内部）。
#[derive(Clone, Debug)]
pub struct FirDesign {
    pub taps: usize,
    /// h[source][tap]（实系数，因果，支撑 [0, taps-1]）
    pub h: Vec<Vec<f64>>,
    /// 用精确频点求得的实际复响应 G_n(f_k)（不是理想 F）
    pub g_actual: Vec<Vec<Cx>>,
    /// 该轮最终使用的 design 点权重 w_m（长度 = design 点数）
    pub weights: Vec<f64>,
}

/// 由目标幅度 A(f) 与 λ 设计一轮 MIMO FIR（不含 IRLS 外迭代）。
#[allow(clippy::too_many_arguments)]
pub fn design_mimo_once(
    freqs_hz: &[f64],
    fs: f64,
    h_design: &[Vec<Vec<Cx>>],
    a_target: &[f64],
    weights: &[f64],
    lambda: f64,
    taps: usize,
    delay_samples: usize,
    diag_load_rel: f64,
    nfft: usize,
) -> Result<FirDesign, String> {
    design_mimo_once_with(
        freqs_hz,
        fs,
        h_design,
        a_target,
        weights,
        lambda,
        taps,
        delay_samples,
        diag_load_rel,
        nfft,
        DesignOptions::default(),
    )
}

/// IRLS 加权 MIMO FIR 设计（契约 §6.3：2 次外迭代）。
///
/// - 权重初值全 1；
/// - 每轮：按当前权重解 F→FIR→实际 G，计算 design 点残差
///   r_m = sqrt(mean_k |sum_n H_mn(f_k)·G_n(f_k)·e^{+jω_k D} - A(f_k)|²)；
/// - 权重更新 w_m ∝ clamp((r_m/mean r)^0.5, 0.25, 4)；
/// - 共 irls_iterations 次外迭代，最后一轮用更新后的权重再设计一次并返回。
#[allow(clippy::too_many_arguments)]
pub fn design_mimo_irls(
    freqs_hz: &[f64],
    fs: f64,
    h_design: &[Vec<Vec<Cx>>],
    a_target: &[f64],
    lambda: f64,
    taps: usize,
    delay_samples: usize,
    diag_load_rel: f64,
    irls_iterations: usize,
    nfft: usize,
) -> Result<FirDesign, String> {
    design_mimo_irls_with(
        freqs_hz,
        fs,
        h_design,
        a_target,
        lambda,
        taps,
        delay_samples,
        diag_load_rel,
        irls_iterations,
        nfft,
        DesignOptions::default(),
    )
}

/// 用给定 G（[source][bin]）计算压力：P_m(f_k) = sum_n H[m][n][f_k]·G_n(f_k)。
/// h: h[mic][bin][source]；返回 [mic][bin]。
pub fn pressure(h: &[Vec<Vec<Cx>>], g: &[Vec<Cx>]) -> Vec<Vec<Cx>> {
    let mut out = Vec::with_capacity(h.len());
    for mic in h {
        let mut row = Vec::with_capacity(mic.len());
        for (ki, hk) in mic.iter().enumerate() {
            let mut acc = Cx::zero();
            for (ni, hv) in hk.iter().enumerate() {
                acc = acc.add(hv.mul(g[ni][ki]));
            }
            row.push(acc);
        }
        out.push(row);
    }
    out
}

/// 基线 design 空间 RMS 功率（每频点）→ 目标构建用（A² 的空间均值）。
pub fn design_rms_sq(p_design: &[Vec<Cx>]) -> Vec<f64> {
    target::spatial_mean_power(p_design)
}

// ===========================================================================
// 4.1/4.4 设计选项：拟合掩码（质量门 Unknown 频段排除拟合）+ 正则项权重（权威加权）
// ===========================================================================

/// 设计选项（默认 None/None = 旧路径**逐位一致**）。
///
/// - `fit_mask`：per-bin 拟合掩码，false 的频点不参与拟合、且该频点修正量强制为
///   恒等 F=1（质量门把 SNR 不足/无证据的 1/3 oct 带判 Unknown 后就用它排除）；
/// - `ridge`：per-bin per-source 正则项权重 Γ_s(f)，正则项变成
///   `λ Σ_s Γ_s(f)|f_s|²`（权威加权：弱频点 Γ 大 → 该源少被派活）。
#[derive(Clone, Copy, Debug, Default)]
pub struct DesignOptions<'a> {
    pub fit_mask: Option<&'a [bool]>,
    pub ridge: Option<&'a [Vec<f64>]>,
}

impl<'a> DesignOptions<'a> {
    /// 该频点是否参与拟合（无掩码 → true）。
    pub fn fits(&self, ki: usize) -> bool {
        self.fit_mask.map(|m| m[ki]).unwrap_or(true)
    }

    /// 该频点的正则项权重（无权重 → None，走标量 λ）。
    pub fn ridge_at(&self, ki: usize) -> Option<&'a [f64]> {
        self.ridge.map(|r| r[ki].as_slice())
    }
}

/// 单频点加权岭最小二乘（复数目标 + 幂等正则项权重 Γ_s）。
///
/// ridge = None → 与 [weighted_ridge_ls_complex] 逐位一致（同一个加法）。
pub fn weighted_ridge_ls_complex_with_ridge(
    h: &[Vec<Cx>],
    target: Cx,
    weights: &[f64],
    lambda: f64,
    ridge: Option<&[f64]>,
) -> Result<Vec<Cx>, String> {
    let m = h.len();
    if m == 0 {
        return Err("weighted_ridge_ls: 空 H".to_string());
    }
    let n = h[0].len();
    if n == 0 || h.iter().any(|row| row.len() != n) {
        return Err("weighted_ridge_ls: 列数不一致/为 0".to_string());
    }
    if weights.len() != m {
        return Err(format!(
            "weighted_ridge_ls: weights.len()={} != m={}",
            weights.len(),
            m
        ));
    }
    if !lambda.is_finite() || lambda < 0.0 {
        return Err(format!("weighted_ridge_ls: λ={} 非法", lambda));
    }
    if let Some(g) = ridge {
        if g.len() != n || g.iter().any(|v| !v.is_finite() || *v <= 0.0) {
            return Err("weighted_ridge_ls: ridge 权重长度非法或非正/非有限".to_string());
        }
    }
    let k = n;
    let mut normal = vec![vec![Cx::zero(); k]; k];
    let mut rhs = vec![Cx::zero(); k];
    for mi in 0..m {
        let w = weights[mi];
        for i in 0..k {
            let whi = h[mi][i].conj().scale(w);
            rhs[i] = rhs[i].add(whi.mul(target));
            for j in 0..k {
                normal[i][j] = normal[i][j].add(whi.mul(h[mi][j]));
            }
        }
    }
    for i in 0..k {
        match ridge {
            Some(g) => normal[i][i].re += lambda * g[i],
            None => normal[i][i].re += lambda,
        }
    }
    solve_linear(normal, rhs)
}

/// 单频点加权岭最小二乘（每 mic 独立复数目标 + 正则项权重 Γ_s）。
pub fn weighted_ridge_ls_targets_with_ridge(
    h: &[Vec<Cx>],
    targets: &[Cx],
    weights: &[f64],
    lambda: f64,
    ridge: Option<&[f64]>,
) -> Result<Vec<Cx>, String> {
    let m = h.len();
    if m == 0 || targets.len() != m {
        return Err("weighted_ridge_ls_targets: H 行数与目标数不匹配".to_string());
    }
    let n = h[0].len();
    if let Some(g) = ridge {
        if g.len() != n || g.iter().any(|v| !v.is_finite() || *v <= 0.0) {
            return Err("weighted_ridge_ls_targets: ridge 权重长度非法或非正/非有限".to_string());
        }
    }
    let mut normal = vec![vec![Cx::zero(); n]; n];
    let mut rhs = vec![Cx::zero(); n];
    for mi in 0..m {
        let w = weights[mi];
        for i in 0..n {
            let whi = h[mi][i].conj().scale(w);
            rhs[i] = rhs[i].add(whi.mul(targets[mi]));
            for j in 0..n {
                normal[i][j] = normal[i][j].add(whi.mul(h[mi][j]));
            }
        }
    }
    for i in 0..n {
        match ridge {
            Some(g) => normal[i][i].re += lambda * g[i],
            None => normal[i][i].re += lambda,
        }
    }
    solve_linear(normal, rhs)
}

/// 单频点求解并投影到每源幅值帽（实数目标 + 正则项权重 Γ_s）。
pub fn weighted_ridge_ls_capped_with_ridge(
    h: &[Vec<Cx>],
    target: f64,
    weights: &[f64],
    lambda: f64,
    cap: f64,
    ridge: Option<&[f64]>,
) -> Result<Vec<Cx>, String> {
    let n = h.first().map(|r| r.len()).unwrap_or(0);
    if n == 0 {
        return Err("weighted_ridge_ls_capped: 空 H".to_string());
    }
    let base = Cx::new(target, 0.0);
    let mut f = weighted_ridge_ls_complex_with_ridge(h, base, weights, lambda, ridge)?;
    let mut fixed = vec![false; n];
    for _pass in 0..3 {
        let mut any_fixed = false;
        for i in 0..n {
            if fixed[i] {
                continue;
            }
            let a = f[i].abs();
            if cap > 0.0 && a > cap {
                f[i] = f[i].scale(cap / a);
                fixed[i] = true;
                any_fixed = true;
            }
        }
        if !any_fixed {
            break;
        }
        let free: Vec<usize> = (0..n).filter(|i| !fixed[*i]).collect();
        if free.is_empty() {
            break;
        }
        let targets: Vec<Cx> = h
            .iter()
            .map(|row| {
                let mut t = base;
                for i in 0..n {
                    if fixed[i] {
                        t = t.sub(row[i].mul(f[i]));
                    }
                }
                t
            })
            .collect();
        let h_red: Vec<Vec<Cx>> = h
            .iter()
            .map(|row| free.iter().map(|i| row[*i]).collect())
            .collect();
        let ridge_free: Option<Vec<f64>> =
            ridge.map(|g| free.iter().map(|i| g[*i]).collect::<Vec<f64>>());
        let f_red = weighted_ridge_ls_targets_with_ridge(
            &h_red,
            &targets,
            weights,
            lambda,
            ridge_free.as_deref(),
        )?;
        for (j, i) in free.iter().enumerate() {
            f[*i] = f_red[j];
        }
    }
    for i in 0..n {
        let a = f[i].abs();
        if cap > 0.0 && a > cap {
            f[i] = f[i].scale(cap / a);
        }
    }
    Ok(f)
}

/// 单频点求解并投影到每源幅值帽（每 mic 独立复数目标 + 正则项权重 Γ_s）。
pub fn weighted_ridge_ls_capped_targets_with_ridge(
    h: &[Vec<Cx>],
    targets: &[Cx],
    weights: &[f64],
    lambda: f64,
    cap: f64,
    ridge: Option<&[f64]>,
) -> Result<Vec<Cx>, String> {
    let n = h.first().map(|r| r.len()).unwrap_or(0);
    if n == 0 || targets.len() != h.len() {
        return Err("weighted_ridge_ls_capped_targets: 维度不匹配".to_string());
    }
    let mut f = weighted_ridge_ls_targets_with_ridge(h, targets, weights, lambda, ridge)?;
    let mut fixed = vec![false; n];
    for _pass in 0..3 {
        let mut any_fixed = false;
        for i in 0..n {
            if fixed[i] {
                continue;
            }
            let a = f[i].abs();
            if cap > 0.0 && a > cap {
                f[i] = f[i].scale(cap / a);
                fixed[i] = true;
                any_fixed = true;
            }
        }
        if !any_fixed {
            break;
        }
        let free: Vec<usize> = (0..n).filter(|i| !fixed[*i]).collect();
        if free.is_empty() {
            break;
        }
        let adjusted: Vec<Cx> = h
            .iter()
            .enumerate()
            .map(|(mi, row)| {
                let mut t = targets[mi];
                for i in 0..n {
                    if fixed[i] {
                        t = t.sub(row[i].mul(f[i]));
                    }
                }
                t
            })
            .collect();
        let h_red: Vec<Vec<Cx>> = h
            .iter()
            .map(|row| free.iter().map(|i| row[*i]).collect())
            .collect();
        let ridge_free: Option<Vec<f64>> =
            ridge.map(|g| free.iter().map(|i| g[*i]).collect::<Vec<f64>>());
        let f_red = weighted_ridge_ls_targets_with_ridge(
            &h_red,
            &adjusted,
            weights,
            lambda,
            ridge_free.as_deref(),
        )?;
        for (j, i) in free.iter().enumerate() {
            f[*i] = f_red[j];
        }
    }
    for i in 0..n {
        let a = f[i].abs();
        if cap > 0.0 && a > cap {
            f[i] = f[i].scale(cap / a);
        }
    }
    Ok(f)
}

/// 由目标幅度 A(f) 设计一轮 MIMO FIR（per-(mic,bin) 权重版 + 设计选项）。
#[allow(clippy::too_many_arguments)]
pub fn design_mimo_once_w_with(
    freqs_hz: &[f64],
    fs: f64,
    h_design: &[Vec<Vec<Cx>>],
    a_target: &[f64],
    weights: &[Vec<f64>],
    lambda: f64,
    taps: usize,
    delay_samples: usize,
    diag_load_rel: f64,
    nfft: usize,
    opts: DesignOptions<'_>,
) -> Result<FirDesign, String> {
    let k = freqs_hz.len();
    if h_design.is_empty() {
        return Err("design_mimo_once_w: 空 H".to_string());
    }
    if weights.len() != h_design.len() {
        return Err("design_mimo_once_w: weights 行数 != design 点数".to_string());
    }
    if weights.iter().any(|row| row.len() != k) {
        return Err("design_mimo_once_w: weights 列数 != 频点数".to_string());
    }
    if let Some(mask) = opts.fit_mask {
        if mask.len() != k {
            return Err("design_mimo_once_w: fit_mask 长度 != 频点数".to_string());
        }
    }
    if let Some(ridge) = opts.ridge {
        if ridge.len() != k || ridge.iter().any(|row| row.len() != h_design[0][0].len()) {
            return Err("design_mimo_once_w: ridge 维度不符".to_string());
        }
    }
    let n_src = h_design[0].first().map(|hk| hk.len()).unwrap_or(0);
    let cap = 10.0f64.powf(F_MAG_CAP_DB / 20.0);
    let mut f_resp: Vec<Vec<Cx>> = vec![Vec::with_capacity(n_src); k];
    for ki in 0..k {
        if !opts.fits(ki) {
            // 质量门排除：该频点不做任何修正（恒等 F=1），压力保持 B0
            f_resp[ki] = vec![Cx::one(); n_src];
            continue;
        }
        let hk: Vec<Vec<Cx>> = h_design.iter().map(|mic| mic[ki].clone()).collect();
        let wbin: Vec<f64> = (0..hk.len()).map(|mi| weights[mi][ki]).collect();
        let fk = weighted_ridge_ls_capped_with_ridge(
            &hk,
            a_target[ki],
            &wbin,
            lambda,
            cap,
            opts.ridge_at(ki),
        )?;
        f_resp[ki] = fk;
    }
    let mut c: Vec<Vec<Cx>> = vec![Vec::with_capacity(k); n_src];
    for ki in 0..k {
        let w = std::f64::consts::TAU * freqs_hz[ki] / fs;
        let rot = Cx::exp_neg_j(w * delay_samples as f64);
        for ni in 0..n_src {
            c[ni].push(f_resp[ki][ni].mul(rot));
        }
    }
    let mut hs: Vec<Vec<f64>> = Vec::with_capacity(n_src);
    let mut gs: Vec<Vec<Cx>> = Vec::with_capacity(n_src);
    for ni in 0..n_src {
        let h =
            fir::design_fir_by_frequency_sampling(freqs_hz, fs, &c[ni], taps, delay_samples, nfft)?;
        let g = fir::fir_response(&h, freqs_hz, fs);
        hs.push(h);
        gs.push(g);
    }
    let _ = diag_load_rel;
    Ok(FirDesign {
        taps,
        h: hs,
        g_actual: gs,
        weights: vec![],
    })
}

/// 每频点目标不同（targets[mic][bin]）+ 设计选项的 MIMO FIR 一轮设计。
#[allow(clippy::too_many_arguments)]
pub fn design_mimo_once_multi_targets_with(
    freqs_hz: &[f64],
    fs: f64,
    h_design: &[Vec<Vec<Cx>>],
    targets: &[Vec<Cx>],
    weights: &[Vec<f64>],
    lambda: f64,
    taps: usize,
    delay_samples: usize,
    diag_load_rel: f64,
    nfft: usize,
    opts: DesignOptions<'_>,
) -> Result<FirDesign, String> {
    let k = freqs_hz.len();
    if h_design.is_empty() {
        return Err("design_mimo_once_multi_targets: 空 H".to_string());
    }
    if targets.len() != h_design.len() || weights.len() != h_design.len() {
        return Err(
            "design_mimo_once_multi_targets: targets/weights 行数 != design 点数".to_string(),
        );
    }
    if targets.iter().any(|r| r.len() != k) || weights.iter().any(|r| r.len() != k) {
        return Err("design_mimo_once_multi_targets: 列数 != 频点数".to_string());
    }
    if let Some(mask) = opts.fit_mask {
        if mask.len() != k {
            return Err("design_mimo_once_multi_targets: fit_mask 长度 != 频点数".to_string());
        }
    }
    if let Some(ridge) = opts.ridge {
        if ridge.len() != k || ridge.iter().any(|row| row.len() != h_design[0][0].len()) {
            return Err("design_mimo_once_multi_targets: ridge 维度不符".to_string());
        }
    }
    let n_src = h_design[0].first().map(|hk| hk.len()).unwrap_or(0);
    let cap = 10.0f64.powf(F_MAG_CAP_DB / 20.0);
    let mut f_resp: Vec<Vec<Cx>> = vec![Vec::with_capacity(n_src); k];
    for ki in 0..k {
        if !opts.fits(ki) {
            f_resp[ki] = vec![Cx::one(); n_src];
            continue;
        }
        let hk: Vec<Vec<Cx>> = h_design.iter().map(|mic| mic[ki].clone()).collect();
        let tk: Vec<Cx> = targets.iter().map(|row| row[ki]).collect();
        let wk: Vec<f64> = weights.iter().map(|row| row[ki]).collect();
        let fk = weighted_ridge_ls_capped_targets_with_ridge(
            &hk,
            &tk,
            &wk,
            lambda,
            cap,
            opts.ridge_at(ki),
        )?;
        f_resp[ki] = fk;
    }
    let mut c: Vec<Vec<Cx>> = vec![Vec::with_capacity(k); n_src];
    for ki in 0..k {
        let w = std::f64::consts::TAU * freqs_hz[ki] / fs;
        let rot = Cx::exp_neg_j(w * delay_samples as f64);
        for ni in 0..n_src {
            c[ni].push(f_resp[ki][ni].mul(rot));
        }
    }
    let mut hs: Vec<Vec<f64>> = Vec::with_capacity(n_src);
    let mut gs: Vec<Vec<Cx>> = Vec::with_capacity(n_src);
    for ni in 0..n_src {
        let h =
            fir::design_fir_by_frequency_sampling(freqs_hz, fs, &c[ni], taps, delay_samples, nfft)?;
        let g = fir::fir_response(&h, freqs_hz, fs);
        hs.push(h);
        gs.push(g);
    }
    let _ = diag_load_rel;
    Ok(FirDesign {
        taps,
        h: hs,
        g_actual: gs,
        weights: vec![],
    })
}

/// 由目标幅度 A(f) 与 λ 设计一轮 MIMO FIR（不含 IRLS 外迭代）+ 设计选项。
#[allow(clippy::too_many_arguments)]
pub fn design_mimo_once_with(
    freqs_hz: &[f64],
    fs: f64,
    h_design: &[Vec<Vec<Cx>>],
    a_target: &[f64],
    weights: &[f64],
    lambda: f64,
    taps: usize,
    delay_samples: usize,
    diag_load_rel: f64,
    nfft: usize,
    opts: DesignOptions<'_>,
) -> Result<FirDesign, String> {
    let k = freqs_hz.len();
    if h_design.is_empty() {
        return Err("design_mimo_once: 空 H".to_string());
    }
    let n_src = h_design
        .first()
        .and_then(|mic| mic.first())
        .map(|hk| hk.len())
        .unwrap_or(0);
    if h_design.len() != weights.len() {
        return Err("design_mimo_once: H 行数与 weights 不一致".to_string());
    }
    if a_target.len() != k {
        return Err("design_mimo_once: A(f) 长度与频点不一致".to_string());
    }
    if let Some(mask) = opts.fit_mask {
        if mask.len() != k {
            return Err("design_mimo_once: fit_mask 长度 != 频点数".to_string());
        }
    }
    if let Some(ridge) = opts.ridge {
        if ridge.len() != k || ridge.iter().any(|row| row.len() != n_src) {
            return Err("design_mimo_once: ridge 维度不符".to_string());
        }
    }
    let cap = 10.0f64.powf(F_MAG_CAP_DB / 20.0);
    let mut f_resp: Vec<Vec<Cx>> = vec![Vec::with_capacity(n_src); k];
    for ki in 0..k {
        if !opts.fits(ki) {
            f_resp[ki] = vec![Cx::one(); n_src];
            continue;
        }
        let hk: Vec<Vec<Cx>> = h_design.iter().map(|mic| mic[ki].clone()).collect();
        let fk = weighted_ridge_ls_capped_with_ridge(
            &hk,
            a_target[ki],
            weights,
            lambda,
            cap,
            opts.ridge_at(ki),
        )?;
        f_resp[ki] = fk;
    }
    let mut c: Vec<Vec<Cx>> = vec![Vec::with_capacity(k); n_src];
    for ki in 0..k {
        let w = std::f64::consts::TAU * freqs_hz[ki] / fs;
        let rot = Cx::exp_neg_j(w * delay_samples as f64);
        for ni in 0..n_src {
            c[ni].push(f_resp[ki][ni].mul(rot));
        }
    }
    let mut hs: Vec<Vec<f64>> = Vec::with_capacity(n_src);
    let mut gs: Vec<Vec<Cx>> = Vec::with_capacity(n_src);
    for ni in 0..n_src {
        let h =
            fir::design_fir_by_frequency_sampling(freqs_hz, fs, &c[ni], taps, delay_samples, nfft)?;
        let g = fir::fir_response(&h, freqs_hz, fs);
        hs.push(h);
        gs.push(g);
    }
    let _ = diag_load_rel;
    Ok(FirDesign {
        taps,
        h: hs,
        g_actual: gs,
        weights: weights.to_vec(),
    })
}

/// IRLS 加权 MIMO FIR 设计（契约 §6.3：2 次外迭代）+ 设计选项。
#[allow(clippy::too_many_arguments)]
pub fn design_mimo_irls_with(
    freqs_hz: &[f64],
    fs: f64,
    h_design: &[Vec<Vec<Cx>>],
    a_target: &[f64],
    lambda: f64,
    taps: usize,
    delay_samples: usize,
    diag_load_rel: f64,
    irls_iterations: usize,
    nfft: usize,
    opts: DesignOptions<'_>,
) -> Result<FirDesign, String> {
    let k = freqs_hz.len();
    let m = h_design.len();
    let mut weights = vec![1.0f64; m];
    let mut current: Option<FirDesign> = None;
    for _ in 0..irls_iterations {
        let design = design_mimo_once_with(
            freqs_hz,
            fs,
            h_design,
            a_target,
            &weights,
            lambda,
            taps,
            delay_samples,
            diag_load_rel,
            nfft,
            opts,
        )?;
        // 实际 G 去公共延迟 D 后与零相位目标 A 比较，得到每 mic 残差
        let mut r = vec![0.0f64; m];
        for (mi, hi) in h_design.iter().enumerate() {
            let mut acc = 0.0;
            for ki in 0..k {
                if !opts.fits(ki) {
                    continue;
                }
                let w = std::f64::consts::TAU * freqs_hz[ki] / fs;
                let un_rot = crate::solve::Cx::exp_pos_j(w * delay_samples as f64);
                let mut p = Cx::zero();
                for ni in 0..hi[ki].len() {
                    let g = design.g_actual[ni][ki].mul(un_rot);
                    p = p.add(hi[ki][ni].mul(g));
                }
                let d = p.sub(Cx::new(a_target[ki], 0.0));
                acc += d.norm_sqr();
            }
            r[mi] = (acc / k as f64).sqrt();
        }
        let mean_r = r.iter().sum::<f64>() / m as f64;
        if !(mean_r.is_finite() && mean_r > 0.0) {
            break;
        }
        for mi in 0..m {
            let ratio = r[mi] / mean_r;
            weights[mi] = ratio.sqrt().clamp(0.25, 4.0);
        }
        current = Some(design);
    }
    let _ = current;
    design_mimo_once_with(
        freqs_hz,
        fs,
        h_design,
        a_target,
        &weights,
        lambda,
        taps,
        delay_samples,
        diag_load_rel,
        nfft,
        opts,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cx_ops() {
        let a = Cx::new(1.0, 2.0);
        let b = Cx::new(3.0, -1.0);
        let p = a.mul(b);
        assert!((p.re - 5.0).abs() < 1e-15 && (p.im - 5.0).abs() < 1e-15);
        assert!((a.norm_sqr() - 5.0).abs() < 1e-15);
        let q = a.mul(a.recip());
        assert!((q.re - 1.0).abs() < 1e-15 && q.im.abs() < 1e-15);
    }

    /// 手算 2x2 复线性系统：[[1, i],[i, 2]] x = [1+i, 2] → x = [2/3, 1 - i/3]。
    #[test]
    fn gauss_matches_hand_computed_2x2() {
        let i = Cx::new(0.0, 1.0);
        let a = vec![vec![Cx::new(1.0, 0.0), i], vec![i, Cx::new(2.0, 0.0)]];
        let b = vec![Cx::new(1.0, 1.0), Cx::new(2.0, 0.0)];
        let x = solve_linear(a, b).expect("solve");
        assert!((x[0].re - 2.0 / 3.0).abs() < 1e-15 && x[0].im.abs() < 1e-15);
        assert!((x[1].re - 1.0).abs() < 1e-15 && (x[1].im + 1.0 / 3.0).abs() < 1e-15);
    }

    /// 单源单 mic：H=1，目标 A=2 → F = 2H/(|H|²+λ)；λ=0 → F=2；λ=3 → 0.5。
    #[test]
    fn ridge_ls_single_source_analytic() {
        let h = vec![vec![Cx::new(1.0, 0.0)]];
        let f = weighted_ridge_ls(&h, 2.0, &[1.0], 0.0).expect("ls");
        assert!((f[0].re - 2.0).abs() < 1e-15 && f[0].im.abs() < 1e-15);
        let f2 = weighted_ridge_ls(&h, 2.0, &[1.0], 3.0).expect("ls");
        assert!((f2[0].re - 0.5).abs() < 1e-15);
    }

    #[test]
    fn ridge_ls_weighted_two_mics() {
        let h = vec![vec![Cx::new(1.0, 0.0)], vec![Cx::new(2.0, 0.0)]];
        // (1+4)F = 3 → F=0.6
        let f = weighted_ridge_ls(&h, 1.0, &[1.0, 1.0], 0.0).expect("ls");
        assert!((f[0].re - 0.6).abs() < 1e-15);
        // (2*1+1*4)F = 2*1+1*2 → 6F=4 → 2/3
        let f = weighted_ridge_ls(&h, 1.0, &[2.0, 1.0], 0.0).expect("ls");
        assert!((f[0].re - 2.0 / 3.0).abs() < 1e-15);
    }

    #[test]
    fn pressure_sums_sources() {
        let h = vec![vec![
            vec![Cx::new(1.0, 0.0), Cx::new(0.0, 1.0)],
            vec![Cx::new(2.0, 0.0), Cx::new(0.0, -1.0)],
        ]];
        let g = vec![
            vec![Cx::new(1.0, 0.0), Cx::new(1.0, 0.0)],
            vec![Cx::new(1.0, 0.0), Cx::new(1.0, 0.0)],
        ];
        let p = pressure(&h, &g);
        // bin0: 1 + i*1 → (1, 1)；bin1: 2 + (-i)*1 → (2, -1)
        assert!((p[0][0].re - 1.0).abs() < 1e-15 && (p[0][0].im - 1.0).abs() < 1e-15);
        assert!((p[0][1].re - 2.0).abs() < 1e-15 && (p[0][1].im + 1.0).abs() < 1e-15);
    }

    /// 权威加权的默认路径：ridge = None 必须与原函数**逐位一致**。
    #[test]
    fn ridge_none_is_bit_identical_to_legacy() {
        let h = vec![
            vec![Cx::new(1.0, 0.2), Cx::new(0.3, -0.1)],
            vec![Cx::new(0.5, 0.0), Cx::new(1.2, 0.4)],
        ];
        let weights = [1.0, 0.7];
        let a = weighted_ridge_ls_complex(&h, Cx::new(1.0, 0.0), &weights, 1e-3).expect("legacy");
        let b = weighted_ridge_ls_complex_with_ridge(&h, Cx::new(1.0, 0.0), &weights, 1e-3, None)
            .expect("with");
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.re.to_bits(), y.re.to_bits(), "实部不是逐位一致");
            assert_eq!(x.im.to_bits(), y.im.to_bits(), "虚部不是逐位一致");
        }
        // Γ 全 1 也应与标量 λ 逐位一致（lambda*1.0 == lambda）
        let ones = [1.0f64, 1.0];
        let c = weighted_ridge_ls_complex_with_ridge(
            &h,
            Cx::new(1.0, 0.0),
            &weights,
            1e-3,
            Some(&ones),
        )
        .expect("ones");
        for (x, y) in a.iter().zip(c.iter()) {
            assert_eq!(x.re.to_bits(), y.re.to_bits());
            assert_eq!(x.im.to_bits(), y.im.to_bits());
        }
    }

    /// 质量门掩码：被排除频点的目标不参与拟合（改它不影响输出）。
    #[test]
    fn fit_mask_excludes_bin_from_fitting() {
        let freqs: Vec<f64> = (0..4)
            .map(|i| (i as f64 + 2.0) * 48000.0 / 8192.0)
            .collect();
        let h: Vec<Vec<Vec<Cx>>> = (0..2)
            .map(|m| {
                (0..4)
                    .map(|ki| {
                        vec![
                            Cx::new(1.0 + 0.1 * m as f64, 0.05 * ki as f64),
                            Cx::new(0.3, -0.1 * m as f64),
                        ]
                    })
                    .collect()
            })
            .collect();
        let weights = vec![vec![1.0f64; 4]; 2];
        let a1 = vec![1.0f64; 4];
        let mut a2 = a1.clone();
        a2[2] = 3.0; // 被掩码频点的目标差异
        let mask = [true, true, false, true];
        let opts = DesignOptions {
            fit_mask: Some(&mask),
            ridge: None,
        };
        let d1 = design_mimo_once_w_with(
            &freqs, 48000.0, &h, &a1, &weights, 1e-6, 32, 16, 1e-9, 8192, opts,
        )
        .expect("d1");
        let d2 = design_mimo_once_w_with(
            &freqs, 48000.0, &h, &a2, &weights, 1e-6, 32, 16, 1e-9, 8192, opts,
        )
        .expect("d2");
        assert_eq!(d1.h, d2.h, "掩码频点的目标变化不应影响拟合");
        // 对照：不掩码时目标变化必须改变 FIR
        let d3 = design_mimo_once_w_with(
            &freqs,
            48000.0,
            &h,
            &a2,
            &weights,
            1e-6,
            32,
            16,
            1e-9,
            8192,
            DesignOptions::default(),
        )
        .expect("d3");
        assert_ne!(d1.h, d3.h, "未掩码时目标变化应改变 FIR");
    }

    #[test]
    fn irls_produces_finite_design() {
        // 2 design mic / 2 source / 8 bins 小合成 H；频点取 65536 网格精确 bin
        let freqs: Vec<f64> = (0..8)
            .map(|i| (60 + 23 * i) as f64 * 48000.0 / 65536.0)
            .collect();
        // 结构必须是 [design_mic][bin][source]：2 mic × 8 bin × 2 source
        let h: Vec<Vec<Vec<Cx>>> = (0..2)
            .map(|m| {
                (0..8)
                    .map(|_| {
                        vec![
                            Cx::new(1.0 + 0.1 * m as f64, 0.1),
                            Cx::new(0.2 + 0.05 * m as f64, -0.3),
                        ]
                    })
                    .collect()
            })
            .collect();
        let a: Vec<f64> = vec![1.0; 8];
        let d =
            design_mimo_irls(&freqs, 48000.0, &h, &a, 1e-4, 32, 16, 1e-9, 2, 65536).expect("irls");
        assert_eq!(d.h.len(), 2);
        assert!(d
            .h
            .iter()
            .all(|h| h.len() == 32 && h.iter().all(|v| v.is_finite())));
    }
}
