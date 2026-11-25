//! [NONRT] 极简 radix-2 FFT（f64，纯 std，无外部依赖）。
//!
//! 用途：goal-6 RT60/CSD 后处理里的 CSD（1024 点 STFT）与 sweep 反卷积
//! （最长 ~2^19–2^20 点）。只为离线分析，不进入 RT 路径；不追求最快，
//! 只要求确定性、可复现、无平台依赖。
//!
//! 约定：
//! - 所有变换长度必须是 2 的幂（next_pow2_at_least 负责取整）；
//! - rfft 返回 N/2+1 个频点（k = 0 到 N/2）；
//! - irfft 取 N/2+1 个频点，按共轭对称补全后做逆变换，结果已乘 1/N。

/// 复数（f64）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct C64 {
    /// 实部。
    pub re: f64,
    /// 虚部。
    pub im: f64,
}

impl C64 {
    /// 构造。
    pub const fn new(re: f64, im: f64) -> Self {
        C64 { re, im }
    }

    /// 共轭。
    pub fn conj(self) -> Self {
        C64::new(self.re, -self.im)
    }

    /// 幅值平方。
    pub fn norm_sqr(self) -> f64 {
        self.re * self.re + self.im * self.im
    }

    /// 幅值。
    pub fn abs(self) -> f64 {
        self.norm_sqr().sqrt()
    }
}

/// 大于等于 n 的最小 2 的幂（n=0 时返回 1）。
pub fn next_pow2_at_least(n: usize) -> usize {
    let mut p = 1usize;
    while p < n {
        p <<= 1;
    }
    p
}

/// 原位基 2 FFT（n 必须是 2 的幂；inverse=true 时含 1/n 归一化）。
pub fn fft_in_place(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    debug_assert_eq!(n, im.len());
    if n <= 1 {
        return;
    }
    debug_assert!(n.is_power_of_two());
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
        let half = len / 2;
        let angle = if inverse {
            2.0 * std::f64::consts::PI / len as f64
        } else {
            -2.0 * std::f64::consts::PI / len as f64
        };
        let w_re = angle.cos();
        let w_im = angle.sin();
        let mut start = 0usize;
        while start < n {
            let mut cur_re = 1.0f64;
            let mut cur_im = 0.0f64;
            for k in 0..half {
                let a = start + k;
                let b = a + half;
                let t_re = cur_re * re[b] - cur_im * im[b];
                let t_im = cur_re * im[b] + cur_im * re[b];
                re[b] = re[a] - t_re;
                im[b] = im[a] - t_im;
                re[a] += t_re;
                im[a] += t_im;
                let next_re = cur_re * w_re - cur_im * w_im;
                cur_im = cur_re * w_im + cur_im * w_re;
                cur_re = next_re;
            }
            start += len;
        }
        len <<= 1;
    }
    if inverse {
        let scale = 1.0 / n as f64;
        for i in 0..n {
            re[i] *= scale;
            im[i] *= scale;
        }
    }
}

/// 实数输入 FFT：x 自动补零到 n（n 必须 >= x.len() 且为 2 的幂），返回 N/2+1 频点。
pub fn rfft(x: &[f64], n: usize) -> Vec<C64> {
    debug_assert!(n.is_power_of_two());
    debug_assert!(x.len() <= n);
    let mut re = vec![0.0f64; n];
    let mut im = vec![0.0f64; n];
    re[..x.len()].copy_from_slice(x);
    fft_in_place(&mut re, &mut im, false);
    (0..=n / 2).map(|k| C64::new(re[k], im[k])).collect()
}

/// 实数输出逆 FFT：spec 为 N/2+1 频点，返回长度 n 的实信号（已乘 1/N）。
pub fn irfft(spec: &[C64], n: usize) -> Vec<f64> {
    debug_assert!(n.is_power_of_two());
    debug_assert_eq!(spec.len(), n / 2 + 1);
    let mut re = vec![0.0f64; n];
    let mut im = vec![0.0f64; n];
    for (k, s) in spec.iter().enumerate() {
        re[k] = s.re;
        im[k] = s.im;
    }
    // 共轭对称补全（0 与 N/2 是实数频点，保持原样）
    for k in 1..n / 2 {
        re[n - k] = spec[k].re;
        im[n - k] = -spec[k].im;
    }
    fft_in_place(&mut re, &mut im, true);
    re
}

impl std::fmt::Display for C64 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.6}{:+.6}j", self.re, self.im)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_dft(x: &[f64]) -> Vec<C64> {
        let n = x.len();
        let mut out = Vec::with_capacity(n);
        for k in 0..n {
            let mut sum_re = 0.0f64;
            let mut sum_im = 0.0f64;
            for (i, v) in x.iter().enumerate() {
                let angle = -2.0 * std::f64::consts::PI * (k * i) as f64 / n as f64;
                sum_re += v * angle.cos();
                sum_im += v * angle.sin();
            }
            out.push(C64::new(sum_re, sum_im));
        }
        out
    }

    #[test]
    fn rfft_matches_naive_dft() {
        let x = [1.0, -2.0, 0.5, 3.0, -1.5, 0.25, 2.0, -0.75];
        let got = rfft(&x, 8);
        let want = naive_dft(&x);
        for (g, w) in got.iter().zip(want.iter().take(5)) {
            assert!((g.re - w.re).abs() < 1e-12, "re {g} vs {w}");
            assert!((g.im - w.im).abs() < 1e-12, "im {g} vs {w}");
        }
    }

    #[test]
    fn irfft_roundtrip() {
        let x = [0.3, -1.2, 4.5, -0.7, 2.2, 0.0, -3.1, 0.9];
        let spec = rfft(&x, 8);
        let back = irfft(&spec, 8);
        for (a, b) in x.iter().zip(back.iter()) {
            assert!((a - b).abs() < 1e-12, "{a} vs {b}");
        }
    }

    #[test]
    fn convolution_theorem() {
        let a = [1.0, 2.0, -1.0, 0.5, 0.0, 0.0, 0.0, 0.0];
        let b = [0.5, -1.0, 0.25, 0.0, 0.0, 0.0, 0.0, 0.0];
        let fa = rfft(&a, 8);
        let fb = rfft(&b, 8);
        let prod: Vec<C64> = fa
            .iter()
            .zip(fb.iter())
            .map(|(x, y)| C64::new(x.re * y.re - x.im * y.im, x.re * y.im + x.im * y.re))
            .collect();
        let circ = irfft(&prod, 8);
        let linear = [0.5, 0.0, -2.25, 1.75];
        for (i, want) in linear.iter().enumerate() {
            assert!(
                (circ[i] - want).abs() < 1e-12,
                "i={i} got {} want {want}",
                circ[i]
            );
        }
    }
}
