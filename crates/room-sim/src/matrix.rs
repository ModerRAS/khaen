//! H(f) 矩阵：每 (mic, source, frequency bin) 的复数声压传递函数。
//! 布局与契约 §7 一致：h[m·N·K + n·K + k]（N = n_sources，K = n_bins）。

use crate::complex::Complex;

/// 源到测点的复数传递函数矩阵。
#[derive(Clone, Debug, PartialEq)]
pub struct HMatrix {
    pub freqs_hz: Vec<f64>,
    pub n_sources: usize,
    pub n_mics: usize,
    /// 长度 n_mics · n_sources · n_bins，索引 m·N·K + n·K + k。
    pub h: Vec<Complex>,
}

impl HMatrix {
    pub fn new(freqs_hz: Vec<f64>, n_sources: usize, n_mics: usize) -> HMatrix {
        let bins = freqs_hz.len();
        HMatrix {
            freqs_hz,
            n_sources,
            n_mics,
            h: vec![Complex::ZERO; n_mics * n_sources * bins],
        }
    }

    pub fn n_bins(&self) -> usize {
        self.freqs_hz.len()
    }

    /// Flat index m·N·K + n·K + k。
    pub fn index(&self, mic: usize, source: usize, bin: usize) -> usize {
        assert!(
            mic < self.n_mics,
            "mic index {mic} out of range ({})",
            self.n_mics
        );
        assert!(
            source < self.n_sources,
            "source index {source} out of range ({})",
            self.n_sources
        );
        assert!(
            bin < self.n_bins(),
            "bin index {bin} out of range ({})",
            self.n_bins()
        );
        mic * self.n_sources * self.n_bins() + source * self.n_bins() + bin
    }

    pub fn at(&self, mic: usize, source: usize, bin: usize) -> Complex {
        self.h[self.index(mic, source, bin)]
    }

    pub fn set(&mut self, mic: usize, source: usize, bin: usize, value: Complex) {
        let flat = self.index(mic, source, bin);
        self.h[flat] = value;
    }

    /// P_m(f_k) = Σ_n H[m][n][k]·G_n(f_k)，返回 [mic][bin]；gains 是 [source][bin]。
    pub fn pressure(&self, gains: &[Vec<Complex>]) -> Vec<Vec<Complex>> {
        assert_eq!(
            gains.len(),
            self.n_sources,
            "gains has {} sources, matrix has {}",
            gains.len(),
            self.n_sources
        );
        let bins = self.n_bins();
        for (source, row) in gains.iter().enumerate() {
            assert_eq!(
                row.len(),
                bins,
                "gains[{source}] has {} bins, matrix has {bins}",
                row.len()
            );
        }

        let mut result = vec![vec![Complex::ZERO; bins]; self.n_mics];
        for mic in 0..self.n_mics {
            for source in 0..self.n_sources {
                let base = self.index(mic, source, 0);
                for bin in 0..bins {
                    result[mic][bin] += self.h[base + bin] * gains[source][bin];
                }
            }
        }
        result
    }

    /// 单个频点的 P_m(f_k)，返回 [mic]。
    pub fn pressure_at(&self, gains: &[Vec<Complex>], bin: usize) -> Vec<Complex> {
        assert!(bin < self.n_bins(), "bin index {bin} out of range");
        let mut result = vec![Complex::ZERO; self.n_mics];
        for mic in 0..self.n_mics {
            for source in 0..self.n_sources {
                result[mic] += self.at(mic, source, bin) * gains[source][bin];
            }
        }
        result
    }

    /// 基准 G_n(f) = 1（B0 RAW）。
    pub fn unity_gains(&self) -> Vec<Vec<Complex>> {
        vec![vec![Complex::ONE; self.n_bins()]; self.n_sources]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_matrix() -> HMatrix {
        // 2 mics × 2 sources × 3 bins，手算可复核。
        let mut matrix = HMatrix::new(vec![10.0, 20.0, 30.0], 2, 2);
        let values = [
            (0usize, 0usize, 0usize, Complex::new(1.0, 0.0)),
            (0, 1, 0, Complex::new(0.0, 1.0)),
            (0, 0, 1, Complex::new(2.0, 0.0)),
            (0, 1, 1, Complex::new(0.0, 2.0)),
            (0, 0, 2, Complex::new(3.0, 0.0)),
            (0, 1, 2, Complex::new(0.0, 3.0)),
            (1, 0, 0, Complex::new(0.5, 0.0)),
            (1, 1, 0, Complex::new(0.5, 0.0)),
            (1, 0, 1, Complex::new(1.0, 0.0)),
            (1, 1, 1, Complex::new(-1.0, 0.0)),
            (1, 0, 2, Complex::new(0.0, 0.0)),
            (1, 1, 2, Complex::new(0.0, 0.0)),
        ];
        for (mic, source, bin, value) in values {
            matrix.set(mic, source, bin, value);
        }
        matrix
    }

    #[test]
    fn layout_is_mic_major_source_then_bin() {
        let matrix = sample_matrix();
        assert_eq!(matrix.n_bins(), 3);
        assert_eq!(matrix.index(0, 0, 0), 0);
        assert_eq!(matrix.index(0, 1, 0), 3);
        assert_eq!(matrix.index(0, 0, 2), 2);
        assert_eq!(matrix.index(1, 0, 0), 6);
        assert_eq!(matrix.h.len(), 12);
        assert_eq!(matrix.at(1, 1, 1), Complex::new(-1.0, 0.0));
    }

    #[test]
    fn pressure_sums_sources_per_bin() {
        let matrix = sample_matrix();
        let gains = vec![vec![Complex::ONE; 3], vec![Complex::from(2.0); 3]];
        let pressure = matrix.pressure(&gains);
        assert_eq!(pressure.len(), 2);
        // mic0 bin0: 1·1 + i·2 = 1 + 2i
        assert!((pressure[0][0] - Complex::new(1.0, 2.0)).abs() < 1e-15);
        // mic0 bin1: 2·1 + 2i·2 = 2 + 4i
        assert!((pressure[0][1] - Complex::new(2.0, 4.0)).abs() < 1e-15);
        // mic1 bin1: 1·1 + (−1)·2 = −1
        assert!((pressure[1][1] - Complex::new(-1.0, 0.0)).abs() < 1e-15);
        let pressure_bin2 = matrix.pressure_at(&gains, 2);
        assert!((pressure_bin2[0] - Complex::new(3.0, 6.0)).abs() < 1e-15);
        assert_eq!(pressure_bin2[1], Complex::ZERO);
        assert_eq!(matrix.unity_gains().len(), 2);
        assert_eq!(matrix.unity_gains()[0][0], Complex::ONE);
    }
}
