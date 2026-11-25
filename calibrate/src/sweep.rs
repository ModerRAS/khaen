//! 对数扫频信号生成。
#[derive(Clone, Debug)]
pub struct SweepSpec {
    pub sample_rate: u32,
    pub start_hz: f64,
    pub end_hz: f64,
    pub seconds: f64,
    pub level_db: f64,
    pub fade_ms: f64,
}

impl SweepSpec {
    pub fn new(sample_rate: u32, seconds: f64, level_db: f64) -> Self {
        Self {
            sample_rate,
            start_hz: 20.0,
            end_hz: 20_000.0_f64.min(sample_rate as f64 * 0.45),
            seconds,
            level_db,
            fade_ms: 10.0,
        }
    }

    pub fn len(&self) -> usize {
        (self.seconds * self.sample_rate as f64).round() as usize
    }

    pub fn generate(&self) -> Vec<f64> {
        let n = self.len();
        let mut out = vec![0.0f64; n];
        if n == 0 {
            return out;
        }
        let t_len = n as f64 / self.sample_rate as f64;
        let k = (self.end_hz / self.start_hz).ln();
        let amp = 10.0f64.powf(self.level_db / 20.0);
        let fade = ((self.fade_ms / 1000.0) * self.sample_rate as f64)
            .round()
            .max(1.0) as usize;
        for i in 0..n {
            let t = i as f64 / self.sample_rate as f64;
            let phase = 2.0 * std::f64::consts::PI * self.start_hz * t_len / k
                * ((self.end_hz / self.start_hz).powf(t / t_len) - 1.0);
            let mut w = 1.0;
            if i < fade {
                w = 0.5 - 0.5 * (std::f64::consts::PI * i as f64 / fade as f64).cos();
            }
            if i + fade >= n {
                let j = n - 1 - i;
                w = w.min(0.5 - 0.5 * (std::f64::consts::PI * j as f64 / fade as f64).cos());
            }
            out[i] = amp * phase.sin() * w;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn length_and_range() {
        let s = SweepSpec::new(48000, 1.0, -30.0);
        let x = s.generate();
        assert_eq!(x.len(), 48000);
        assert!(x.iter().all(|v| v.is_finite()));
        assert!(x.iter().map(|v| v.abs()).fold(0.0, f64::max) <= 0.04);
    }
    #[test]
    fn fades_are_zero_ish_at_edges() {
        let s = SweepSpec::new(48000, 0.5, -20.0);
        let x = s.generate();
        assert!(x[0].abs() < 1e-12);
        assert!(x[x.len() - 1].abs() < 1e-12);
    }
}
