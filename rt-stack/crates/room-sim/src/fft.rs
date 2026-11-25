//! radix-2 复数 FFT（就地，Cooley–Tukey，长度必须是 2 的幂）与零填充 rfft。
//! 零外部依赖；用于把 RIR 变到频域取 H(f) 频点。

use crate::complex::Complex;
use std::f64::consts::PI;

/// 就地 radix-2 复数 FFT。n = data.len() 必须是 2 的幂；n <= 1 时不做任何事。
///
/// # Panics
/// 长度不是 2 的幂时 panic（调用方错误不可静默传播）。
pub fn fft_in_place(data: &mut [Complex]) {
    let n = data.len();
    assert!(
        n.is_power_of_two(),
        "FFT length must be a power of two, got {n}"
    );
    if n <= 1 {
        return;
    }

    // 位反序置换。
    let mut reversed = 0usize;
    for index in 1..n {
        let mut bit = n >> 1;
        while reversed & bit != 0 {
            reversed ^= bit;
            bit >>= 1;
        }
        reversed ^= bit;
        if index < reversed {
            data.swap(index, reversed);
        }
    }

    // 逐级蝶形。
    let mut length = 2usize;
    while length <= n {
        let angle = -2.0 * PI / length as f64;
        let twiddle_step = Complex::new(angle.cos(), angle.sin());
        let half = length >> 1;
        let mut start = 0usize;
        while start < n {
            let mut twiddle = Complex::ONE;
            for offset in 0..half {
                let even = data[start + offset];
                let odd = data[start + offset + half] * twiddle;
                data[start + offset] = even + odd;
                data[start + offset + half] = even - odd;
                twiddle *= twiddle_step;
            }
            start += length;
        }
        length <<= 1;
    }
}

/// 一次性复数 FFT（复制输入）。
pub fn fft(data: &[Complex]) -> Vec<Complex> {
    let mut buffer = data.to_vec();
    fft_in_place(&mut buffer);
    buffer
}

/// 实数信号零填充/截断到 n 点后做 rfft，返回 n/2 + 1 个 bins（bin k 对应频率 k·fs/n）。
/// n 必须是 2 的幂；样本多于 n 时只取前 n 个，少于时补 0。
pub fn rfft(samples: &[f64], n: usize) -> Vec<Complex> {
    assert!(
        n.is_power_of_two() && n >= 1,
        "rfft size must be a power of two >= 1, got {n}"
    );
    let mut buffer = vec![Complex::ZERO; n];
    let copied = samples.len().min(n);
    for index in 0..copied {
        buffer[index] = Complex::new(samples[index], 0.0);
    }
    fft_in_place(&mut buffer);
    buffer.truncate(n / 2 + 1);
    buffer
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn naive_dft(samples: &[Complex]) -> Vec<Complex> {
        let n = samples.len();
        let mut out = vec![Complex::ZERO; n];
        for k in 0..n {
            let mut sum = Complex::ZERO;
            for (t, sample) in samples.iter().enumerate() {
                let angle = -2.0 * PI * ((k * t) % n) as f64 / n as f64;
                sum += *sample * Complex::from_polar(1.0, angle);
            }
            out[k] = sum;
        }
        out
    }

    #[test]
    fn impulse_transforms_to_all_ones() {
        let mut data = vec![Complex::ZERO; 32];
        data[0] = Complex::ONE;
        fft_in_place(&mut data);
        for bin in &data {
            assert!((*bin - Complex::ONE).abs() < 1e-15);
        }
    }

    #[test]
    fn matches_naive_dft() {
        let n = 64usize;
        let samples: Vec<Complex> = (0..n)
            .map(|i| {
                let t = i as f64 / n as f64;
                Complex::new(
                    (2.0 * PI * 5.0 * t).sin() + 0.5 * (2.0 * PI * 11.0 * t).cos() + 0.25,
                    0.3 * (2.0 * PI * 3.0 * t).sin(),
                )
            })
            .collect();
        let expected = naive_dft(&samples);
        let mut actual = samples.clone();
        fft_in_place(&mut actual);
        for (bin, reference) in actual.iter().zip(expected.iter()) {
            assert!(
                (*bin - *reference).abs() <= 1e-10,
                "bin mismatch: {bin} vs {reference}"
            );
        }
    }

    #[test]
    fn rfft_matches_naive_dft_and_returns_half_spectrum() {
        let n = 128usize;
        let samples: Vec<f64> = (0..n)
            .map(|i| {
                let t = i as f64 / n as f64;
                (2.0 * PI * 7.0 * t).sin() + 0.4 * (2.0 * PI * 19.0 * t).cos()
            })
            .collect();
        let bins = rfft(&samples, n);
        assert_eq!(bins.len(), n / 2 + 1);
        let complex_samples: Vec<Complex> = samples
            .iter()
            .map(|value| Complex::new(*value, 0.0))
            .collect();
        let expected = naive_dft(&complex_samples);
        for (bin, reference) in bins.iter().zip(expected.iter()) {
            assert!((*bin - *reference).abs() <= 1e-10);
        }
    }

    #[test]
    fn rfft_zero_pads_and_has_real_dc_and_nyquist() {
        let bins = rfft(&[1.0], 8);
        assert_eq!(bins.len(), 5);
        for bin in &bins {
            assert!((bin.re - 1.0).abs() < 1e-15);
            assert!(bin.im.abs() < 1e-15);
        }
        let samples = [0.5, -0.5, 0.5];
        let bins = rfft(&samples, 8);
        assert!(bins[0].im.abs() < 1e-15);
        assert!(bins[4].im.abs() < 1e-15);
        assert!((bins[0].re - 0.5).abs() < 1e-15);
    }

    #[test]
    fn paresval_energy_is_preserved() {
        let n = 64usize;
        let samples: Vec<f64> = (0..n)
            .map(|i| (2.0 * PI * ((i * 9) % n) as f64 / n as f64).sin())
            .collect();
        let bins = rfft(&samples, n);
        let time_energy: f64 = samples.iter().map(|value| value * value).sum();
        let spectrum_energy: f64 = bins
            .iter()
            .enumerate()
            .map(|(index, bin)| {
                let weight = if index == 0 || index == n / 2 {
                    1.0
                } else {
                    2.0
                };
                weight * bin.norm_sqr()
            })
            .sum::<f64>()
            / n as f64;
        assert!((time_energy - spectrum_energy).abs() <= 1e-10);
    }
}
