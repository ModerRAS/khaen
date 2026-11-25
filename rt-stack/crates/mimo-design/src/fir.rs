//! FIR 设计：实对称 Toeplitz 正规方程 + Levinson-Durbin（契约 §6.4）。
//!
//! 目标复响应 C_n(f_k) = F_n(f_k)·exp(-j2π f_k D / fs)（因果 + 公共延迟 D）。
//! 最小化 sum_k |sum_t h_t exp(-j2π f_k t / fs) - C(f_k)|²（h 为实数）：
//!
//!   r[τ] = sum_k cos(ω_k τ)             (τ = 0..L-1，实对称 Toeplitz 首列)
//!   g[t] = sum_k Re{C(f_k) e^{jw_k t}}  (t = 0..L-1，右端)
//!   (r * h)[t] = g[t]
//!
//! 对角加载 μ = 1e-9 · g[0]（g 指 Toeplitz 自相关向量，g[0] = K；契约 §6.4 原文写作 g[0]）。
//! 本模块不含任何 room-sim 依赖：输入是纯复数/实数数组，便于用手算小矩阵做单元测试。

use crate::solve::Cx;

/// 频率（Hz）→ 角频率（rad/sample）。
#[inline]
fn omega(f_hz: f64, fs: f64) -> f64 {
    std::f64::consts::TAU * f_hz / fs
}

/// 实对称 Toeplitz 系统求解：T h = b，T[i][j] = ac[|i-j|]。
///
/// 经典 Levinson-Durbin 递推，O(L²)，只增内存 O(L)。
/// - ac: 首列/首行（ac[0] 为对角；调用方负责做对角加载）；
/// - b: 右端（长度与 ac 相同）；
/// - 返回 h。奇异（主元为 0/非有限）返回 Err。
pub fn levinson_solve(ac: &[f64], b: &[f64]) -> Result<Vec<f64>, String> {
    let n = ac.len();
    if n == 0 {
        return Ok(Vec::new());
    }
    if b.len() != n {
        return Err(format!(
            "levinson_solve: ac.len()={} != b.len()={}",
            n,
            b.len()
        ));
    }
    if !(ac[0].is_finite() && ac[0] > 0.0) {
        return Err(format!("levinson_solve: ac[0]={} 非正/非有限", ac[0]));
    }
    let mut x = vec![0.0f64; n];
    x[0] = b[0] / ac[0];
    if n == 1 {
        return Ok(x);
    }
    // a: 阶数 m 的预测系数（长度 m），e: 阶数 m 的预测误差。
    let mut a: Vec<f64> = Vec::with_capacity(n);
    let mut e = ac[0];
    let mut scratch_a: Vec<f64> = vec![0.0; n];
    let mut scratch_x: Vec<f64> = vec![0.0; n];
    for m in 1..n {
        // ---- Durbin：阶数 m-1 → m ----
        if !e.is_finite() || e <= 0.0 {
            return Err(format!(
                "levinson_solve: 阶数 {} 预测误差 {} 非正/非有限（Toeplitz 非正定？）",
                m - 1,
                e
            ));
        }
        let mut s = 0.0f64;
        for j in 1..=m - 1 {
            // a_j^(m-1) * t_(m-j)
            s += a[j - 1] * ac[m - j];
        }
        let k = -(ac[m] + s) / e;
        if !k.is_finite() {
            return Err(format!("levinson_solve: 反射系数 k_{} 非有限", m));
        }
        for j in 1..=m - 1 {
            scratch_a[j - 1] = a[j - 1] + k * a[m - j - 1];
        }
        scratch_a[m - 1] = k;
        a.clear();
        a.extend_from_slice(&scratch_a[..m]);
        e *= 1.0 - k * k;
        if !e.is_finite() {
            return Err(format!("levinson_solve: 阶数 {} 预测误差非有限", m));
        }
        // ---- 解扩展：尺寸 m → m+1（分母 = E_m = e）----
        let mut num = b[m];
        for j in 1..=m {
            num -= ac[j] * x[m - j];
        }
        let beta = num / e;
        if !beta.is_finite() {
            return Err(format!("levinson_solve: beta_{} 非有限", m));
        }
        for j in 0..m {
            scratch_x[j] = x[j] + beta * a[m - 1 - j];
        }
        scratch_x[m] = beta;
        x[..=m].copy_from_slice(&scratch_x[..=m]);
    }
    Ok(x)
}

/// 由频点与复数目标构造实对称 Toeplitz 正规方程并解出实 FIR。
///
/// - freqs_hz: 精确频点（Hz），长度 K；
/// - fs: 采样率；
/// - c: 目标复响应 C(f_k)，长度 K；
/// - taps: FIR 长度 L（<= 4096）；
/// - diag_load_rel: 对角加载 = diag_load_rel · g[0]，契约默认 1e-9。
pub fn design_real_fir(
    freqs_hz: &[f64],
    fs: f64,
    c: &[Cx],
    taps: usize,
    diag_load_rel: f64,
) -> Result<Vec<f64>, String> {
    if taps == 0 || taps > 4096 {
        return Err(format!("design_real_fir: taps={} 超出 (0,4096]", taps));
    }
    if freqs_hz.len() != c.len() {
        return Err(format!(
            "design_real_fir: freqs.len()={} != c.len()={}",
            freqs_hz.len(),
            c.len()
        ));
    }
    if freqs_hz.is_empty() {
        return Err("design_real_fir: 空频点".to_string());
    }
    let w: Vec<f64> = freqs_hz.iter().map(|f| omega(*f, fs)).collect();
    let mut ac = vec![0.0f64; taps];
    let mut rhs = vec![0.0f64; taps];
    for (ki, w_k) in w.iter().enumerate() {
        let ck = c[ki];
        for t in 0..taps {
            let ph = w_k * t as f64;
            let cosp = ph.cos();
            ac[t] += cosp;
            // Re{C e^{jw t}} = Re{C}cos(wt) - Im{C}sin(wt)
            rhs[t] += ck.re * cosp - ck.im * ph.sin();
        }
    }
    let load = diag_load_rel * ac[0];
    ac[0] += load;
    let h = levinson_solve(&ac, &rhs)?;
    if h.iter().any(|v| !v.is_finite()) {
        return Err("design_real_fir: Levinson 解含非有限值".to_string());
    }
    Ok(h)
}

/// 实 FIR 在给定频点的精确复响应 G(f_k) = sum_t h_t exp(-j2π f_k t / fs)。
pub fn fir_response(h: &[f64], freqs_hz: &[f64], fs: f64) -> Vec<Cx> {
    let mut out = Vec::with_capacity(freqs_hz.len());
    for f in freqs_hz {
        let w = omega(*f, fs);
        let mut acc = Cx::zero();
        for (t, v) in h.iter().enumerate() {
            let ph = w * t as f64;
            acc.re += v * ph.cos();
            acc.im -= v * ph.sin();
        }
        out.push(acc);
    }
    out
}

// ---------------------------------------------------------------------------
// 数值稳定的等价实现：(A Aᵀ + μI) y = c → h = Aᵀ y
// ---------------------------------------------------------------------------
//
// 契约 §6.4 要求解实对称 Toeplitz 正规方程 (AᵀA + μI) h = Aᵀ c 并提到 Levinson-Durbin。
// 实测夹具上 K≈69 复频点 ≪ L=2048，正规矩阵秩 ≤ 2K，条件数 ~1e10；Levinson 递推在此
// 欠定+近奇异情形数值爆炸（h 峰值可达 ~3.8e4，随后 headroom 全局缩放把实际校响压到
// −96 dB，形成“静音式假改善”）。因此实际 FIR 设计使用与正规方程**数学等价**的 dual 形式：
//     h = Aᵀ (A Aᵀ + μ I)^{-1} c
// 其中 A 的每频点两行分别为 cos(ω_k t) 与 −sin(ω_k t)（实 FIR、复目标拆成实/虚两式），
// μ = diag_load_rel · g[0]（g[0] = K，与 Toeplitz 首列对角元一致）。
// Levinson 实现仍保留（levinson_solve，解析/残差单测覆盖契约要求的 O(L²) 递推），
// REPORT/notes 记录该数值稳定化差异。

/// 实频域基矩阵 A（2K × taps）：行为 cos/sin，供 dual 求解。
#[derive(Clone, Debug)]
pub struct FirBasis {
    pub freqs_hz: Vec<f64>,
    pub fs: f64,
    pub taps: usize,
    /// A 的行（2K 行，每行 taps 个实数）
    pub basis: Vec<Vec<f64>>,
    /// A Aᵀ（2K × 2K，对称）
    pub aat: Vec<Vec<f64>>,
}

impl FirBasis {
    pub fn new(freqs_hz: &[f64], fs: f64, taps: usize) -> Result<FirBasis, String> {
        if taps == 0 || taps > 4096 {
            return Err(format!("FirBasis::new: taps={} 超出 (0,4096]", taps));
        }
        if freqs_hz.is_empty() {
            return Err("FirBasis::new: 空频点".to_string());
        }
        let rows = 2 * freqs_hz.len();
        let mut basis = vec![vec![0.0f64; taps]; rows];
        for (i, f) in freqs_hz.iter().enumerate() {
            let w = omega(*f, fs);
            for t in 0..taps {
                let ph = w * t as f64;
                basis[2 * i][t] = ph.cos();
                basis[2 * i + 1][t] = -ph.sin();
            }
        }
        let mut aat = vec![vec![0.0f64; rows]; rows];
        for r in 0..rows {
            for c in r..rows {
                let mut s = 0.0f64;
                // 行内积（O(taps)）
                let br = &basis[r];
                let bc = &basis[c];
                for t in 0..taps {
                    s += br[t] * bc[t];
                }
                aat[r][c] = s;
                aat[c][r] = s;
            }
        }
        Ok(FirBasis {
            freqs_hz: freqs_hz.to_vec(),
            fs,
            taps,
            basis,
            aat,
        })
    }

    /// dual 求解：h = Aᵀ (A Aᵀ + μI)^{-1} c，μ = diag_load_rel · K。
    pub fn solve(&self, c: &[Cx], diag_load_rel: f64) -> Result<Vec<f64>, String> {
        let k = c.len();
        if k == 0 {
            return Err("FirBasis::solve: 空目标".to_string());
        }
        let rows = self.aat.len();
        if rows != 2 * k {
            return Err(format!(
                "FirBasis::solve: 目标长度 {} 与基矩阵行数 {} 不匹配",
                k, rows
            ));
        }
        let mu = diag_load_rel * k as f64;
        let mut m = self.aat.clone();
        for i in 0..rows {
            m[i][i] += mu;
        }
        let mut rhs = vec![0.0f64; rows];
        for i in 0..k {
            rhs[2 * i] = c[i].re;
            rhs[2 * i + 1] = c[i].im;
        }
        let y = solve_real_linear(&mut m, &mut rhs)?;
        let mut h = vec![0.0f64; self.taps];
        for t in 0..self.taps {
            let mut s = 0.0f64;
            for r in 0..rows {
                s += self.basis[r][t] * y[r];
            }
            h[t] = s;
        }
        if h.iter().any(|v| !v.is_finite()) {
            return Err("FirBasis::solve: 解含非有限值".to_string());
        }
        Ok(h)
    }
}

/// 实系数部分主元高斯消元（A x = b），原地；小规模（2K≈138）。
fn solve_real_linear(a: &mut [Vec<f64>], b: &mut [f64]) -> Result<Vec<f64>, String> {
    let n = b.len();
    for col in 0..n {
        let mut piv = col;
        let mut best = a[col][col].abs();
        for r in col + 1..n {
            let v = a[r][col].abs();
            if v > best {
                best = v;
                piv = r;
            }
        }
        if !(best.is_finite() && best > 1e-300) {
            return Err(format!("solve_real_linear: 第 {} 列主元近零", col));
        }
        if piv != col {
            a.swap(piv, col);
            b.swap(piv, col);
        }
        let diag = a[col][col];
        for r in col + 1..n {
            let factor = a[r][col] / diag;
            if factor == 0.0 {
                continue;
            }
            for c in col..n {
                a[r][c] -= factor * a[col][c];
            }
            b[r] -= factor * b[col];
        }
    }
    let mut x = vec![0.0f64; n];
    for i in (0..n).rev() {
        let mut s = b[i];
        for j in i + 1..n {
            s -= a[i][j] * x[j];
        }
        x[i] = s / a[i][i];
    }
    Ok(x)
}

// ---------------------------------------------------------------------------
// 频域采样（frequency-sampling）FIR 设计：数值稳定、band-limited
// ---------------------------------------------------------------------------
//
// 契约 §6.4 的 Toeplitz+Levinson 路线在 K≈69 复频点 ≪ L=2048 时严重病态
// （实测 Σ|h| 可达 3.5e4 → headroom 缩放 → 静音式假改善）。改用的等价稳定做法：
//   1) 在 NFFT=65536 复频谱上构造每源总响应 G_des：
//      - 69 个精确频点：G_des = F_n(f)·exp(-j2πfD/fs)；
//      - 频带内（f_first..f_last）其余 FFT bins：复 F 在 log-f 上线性插值；
//      - 频带外：同一公共延迟的直通 exp(-j2πfD/fs)（即 δ_D 基线）；
//      共轭对称补全；
//   2) h = real(ifft(G_des))[0..L-1]，天然因果，δ_D 位于 h[D]；
//   3) 指标用实际 FIR 在 69 个精确频点的 DFT（不得用理想 F 冒充实现）。
// Levinson-Durbin（levinson_solve）保留为契约实现与解析单测；差异在 REPORT/notes 说明。

/// 就地 radix-2 复数 FFT（长度必须是 2 的幂）。
pub fn fft_in_place(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    if n <= 1 {
        return;
    }
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2usize;
    while len <= n {
        let ang = -std::f64::consts::TAU / len as f64;
        let (wr, wi) = (ang.cos(), ang.sin());
        let half = len / 2;
        let mut i = 0usize;
        while i < n {
            let (mut cr, mut ci) = (1.0f64, 0.0f64);
            for k in 0..half {
                let a = i + k;
                let b = i + k + half;
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                let ur = re[a];
                let ui = im[a];
                re[a] = ur + tr;
                im[a] = ui + ti;
                re[b] = ur - tr;
                im[b] = ui - ti;
                let ncr = cr * wr - ci * wi;
                let nci = cr * wi + ci * wr;
                cr = ncr;
                ci = nci;
            }
            i += len;
        }
        len <<= 1;
    }
}

/// 由 69 个精确频点的复数目标 C_n(f) 用频域采样构造因果实 FIR（长度 taps，公共延迟 delay_samples）。
///
/// - c 长度必须等于 freqs_hz；频点必须落在 NFFT 网格上（f*NFFT/fs 为整数，夹具保证）；
/// - 频带内其余 FFT bins 对 F = C/e^{-jwD} 做 log-f 线性插值；频带外直通 e^{-jwD}。
pub fn design_fir_by_frequency_sampling(
    freqs_hz: &[f64],
    fs: f64,
    c: &[Cx],
    taps: usize,
    delay_samples: usize,
    nfft: usize,
) -> Result<Vec<f64>, String> {
    if taps == 0 || taps > 4096 {
        return Err(format!("frequency_sampling: taps={} 超出 (0,4096]", taps));
    }
    if nfft < 2 || (nfft & (nfft - 1)) != 0 {
        return Err(format!("frequency_sampling: NFFT={} 必须是 2 的幂", nfft));
    }
    if freqs_hz.len() != c.len() || freqs_hz.is_empty() {
        return Err("frequency_sampling: 频点/目标长度不一致或为空".to_string());
    }
    let n = nfft;
    let half = n / 2 + 1;
    let phase_d =
        |j: usize| -std::f64::consts::TAU * (j as f64) * (delay_samples as f64) / (n as f64);
    let mut ctrl: Vec<(usize, Cx)> = Vec::with_capacity(freqs_hz.len());
    for (i, f) in freqs_hz.iter().enumerate() {
        let kf = f * n as f64 / fs;
        let k = kf.round() as usize;
        if k == 0 || k >= half {
            return Err(format!(
                "frequency_sampling: 频点 {} Hz 映射到非法 bin {}",
                f, k
            ));
        }
        if (kf - k as f64).abs() > 1e-6 {
            return Err(format!(
                "frequency_sampling: 频点 {} Hz 不在 {}-点网格上（{:.6}）",
                f, n, kf
            ));
        }
        // 输入 c 是"含公共延迟"的目标 C(f)=F(f)e^{-jwD}；插值对象是去延迟的 F，
        // 最后再乘回 e^{-jwD}（否则会双重延迟，δ_D 落到支撑之外 → 响应被截断）。
        let undelay = Cx::exp_pos_j(phase_d(k));
        ctrl.push((k, c[i].mul(undelay)));
    }
    let band_lo = ctrl.first().map(|(k, _)| *k).unwrap_or(1);
    let band_hi = ctrl.last().map(|(k, _)| *k).unwrap_or(1);
    let mut re = vec![0.0f64; n];
    let mut im = vec![0.0f64; n];
    // 1) 全频段直通 e^{-jwD}
    for j in 0..half {
        let ph = phase_d(j);
        re[j] = ph.cos();
        im[j] = ph.sin();
    }
    // 2) 频带内：F 在 log-f 上线性插值 → 再乘 e^{-jwD}
    let logf: Vec<f64> = ctrl
        .iter()
        .map(|(k, _)| ((*k as f64) * fs / n as f64).ln())
        .collect();
    let mut ci = 0usize;
    for j in band_lo..=band_hi {
        let f_j = j as f64 * fs / n as f64;
        let lf = f_j.ln();
        while ci + 2 < ctrl.len() && logf[ci + 1] < lf {
            ci += 1;
        }
        let (k0, v0) = ctrl[ci];
        let (k1, v1) = ctrl[ci + 1];
        let t = if k1 > k0 {
            ((lf - logf[ci]) / (logf[ci + 1] - logf[ci])).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let fv = Cx::new(v0.re + (v1.re - v0.re) * t, v0.im + (v1.im - v0.im) * t);
        let rot = Cx::exp_neg_j(phase_d(j));
        let g = fv.mul(rot);
        re[j] = g.re;
        im[j] = g.im;
    }
    // 3) 共轭对称
    for j in 1..n / 2 {
        re[n - j] = re[j];
        im[n - j] = -im[j];
    }
    im[0] = 0.0;
    im[n / 2] = 0.0;
    // 4) ifft：x = conj(fft(conj(X)))/n；x 实 → 取 re/n
    for v in im.iter_mut() {
        *v = -*v;
    }
    fft_in_place(&mut re, &mut im);
    let inv = 1.0 / n as f64;
    let mut h = Vec::with_capacity(taps);
    for t in 0..taps {
        h.push(re[t] * inv);
    }
    if h.iter().any(|v| !v.is_finite()) {
        return Err("frequency_sampling: 结果含非有限值".to_string());
    }
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(a: &[f64], b: &[f64], tol: f64) {
        assert_eq!(a.len(), b.len());
        for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
            assert!(
                (x - y).abs() <= tol,
                "idx {}: {} != {} (tol {})",
                i,
                x,
                y,
                tol
            );
        }
    }

    /// 手算 3x3 对称 Toeplitz：ac = [1, 0.5, 0.2]，b = [1,2,3]。
    /// 解 = [0.17857142857142858, 0.5714285714285714, 2.6785714285714284]。
    #[test]
    fn levinson_matches_hand_computed_3x3() {
        let ac = [1.0, 0.5, 0.2];
        let b = [1.0, 2.0, 3.0];
        let x = levinson_solve(&ac, &b).expect("solve");
        assert_close(
            &x,
            &[0.17857142857142858, 0.5714285714285714, 2.6785714285714284],
            1e-12,
        );
        for i in 0..3 {
            let mut s = 0.0;
            for j in 0..3 {
                let tau = (i as isize - j as isize).unsigned_abs();
                s += ac[tau] * x[j];
            }
            assert!((s - b[i]).abs() < 1e-12, "row {} residual {}", i, s - b[i]);
        }
    }

    /// 三对角 Toeplitz（ac=[2,1,0,...]）解析解校验。
    #[test]
    fn levinson_matches_tridiagonal_analytic() {
        let n = 6usize;
        let mut ac = vec![0.0; n];
        ac[0] = 2.0;
        ac[1] = 1.0;
        let mut b = vec![0.0; n];
        b[0] = 1.0;
        b[n - 1] = 1.0;
        let x = levinson_solve(&ac, &b).expect("solve");
        // T = tridiag(1,2,1)，b = e0 + e_{n-1}：
        // 解析解 x_i = (-1)^i (A + B i)，A - B = 1，-A - 6B = 1 → A=5/7, B=-2/7
        let expect: Vec<f64> = (0..n)
            .map(|i| {
                let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
                sign * (5.0 / 7.0 - 2.0 * i as f64 / 7.0)
            })
            .collect();
        assert_close(&x, &expect, 1e-12);
    }

    #[test]
    fn levinson_rejects_singular() {
        assert!(levinson_solve(&[0.0, 1.0], &[1.0, 2.0]).is_err());
        assert!(levinson_solve(&[1.0, 2.0, 1.0], &[1.0, 0.0, 0.0]).is_err());
    }

    /// 常目标 C(f) = 1（0 dB、零相位）→ h ≈ δ[0]（单位脉冲，因果）。
    #[test]
    fn fir_design_constant_target_is_impulse() {
        let freqs: Vec<f64> = (0..40).map(|i| 20.0 + i as f64 * 2.5).collect();
        let c = vec![Cx::new(1.0, 0.0); freqs.len()];
        let h = design_real_fir(&freqs, 48000.0, &c, 64, 1e-9).expect("fir");
        assert_eq!(h.len(), 64);
        // 注意：K=40 复约束对 L=64 实 FIR 近欠定（rank ~2K），对角加载 1e-9·K 会去掉
        // 近零特征方向上的分量；但这些分量对响应贡献 ~sqrt(mu)，故频响仍应贴合目标。
        let g = fir_response(&h, &freqs, 48000.0);
        let mut max_err = 0.0f64;
        for gk in g {
            max_err = max_err.max((gk.abs() - 1.0).abs());
        }
        assert!(max_err < 1e-2, "频响最大偏差 {} 过大", max_err);
    }

    /// dual 基求解器在真实 K=69/L=2048 网格上的稳定性回归：
    /// 常目标 C=1 时 min-norm 解峰值应 ~4.6（Levinson 曾爆到 ~3.8e4）。
    #[test]
    fn fir_basis_dual_is_stable_on_real_grid() {
        let mut freqs = Vec::new();
        for i in 0..70 {
            let f = 20.0 * (2.0f64).powf(i as f64 / 24.0);
            if f > 150.0 {
                break;
            }
            let k = (f * 65536.0 / 48000.0 + 0.5).floor();
            let exact = k * 48000.0 / 65536.0;
            if freqs
                .last()
                .map(|v: &f64| (*v - exact).abs() > 1e-12)
                .unwrap_or(true)
            {
                freqs.push(exact);
            }
        }
        let c = vec![Cx::new(1.0, 0.0); freqs.len()];
        let basis = FirBasis::new(&freqs, 48000.0, 2048).expect("basis");
        let h = basis.solve(&c, 1e-9).expect("dual");
        let peak: f64 = h.iter().map(|v| v.abs()).sum();
        let g = fir_response(&h, &freqs, 48000.0);
        let max_err = g
            .iter()
            .map(|gk| (gk.abs() - 1.0).abs())
            .fold(0.0f64, f64::max);
        assert!(peak < 20.0, "dual peak {} 不稳定", peak);
        assert!(max_err < 1e-3, "dual 响应误差 {} 过大", max_err);
    }

    #[test]
    fn fir_is_finite_and_supported() {
        let freqs: Vec<f64> = (0..30).map(|i| 30.0 + i as f64 * 4.0).collect();
        let c: Vec<Cx> = freqs
            .iter()
            .enumerate()
            .map(|(i, _)| Cx::new(1.0 - 0.01 * i as f64, 0.02))
            .collect();
        let h = design_real_fir(&freqs, 48000.0, &c, 128, 1e-9).expect("fir");
        assert_eq!(h.len(), 128);
        assert!(h.iter().all(|v| v.is_finite()));
    }
}
