//! Stateful linear resampler used by capture/WAV inputs when the source rate is
//! not 48 kHz. Linear interpolation is deliberately simple and documented as a
//! prototype-quality SRC (Windows endpoints are normally already 48 kHz; the
//! receiver side is expected to do asynchronous correction anyway).

pub struct LinearResampler {
    channels: usize,
    step: f64,
    pos: f64,
    prev: Vec<f32>,
    has_prev: bool,
}

impl LinearResampler {
    pub fn new(in_rate: u32, out_rate: u32, channels: usize) -> anyhow::Result<Self> {
        anyhow::ensure!(in_rate > 0 && out_rate > 0, "invalid resample rates");
        anyhow::ensure!(channels > 0, "channels must be > 0");
        Ok(Self {
            channels,
            step: in_rate as f64 / out_rate as f64,
            pos: 0.0,
            prev: vec![0.0; channels],
            has_prev: false,
        })
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Process interleaved input, returning interleaved output (possibly empty).
    /// Maintains fractional phase and previous frame across calls.
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        let ch = self.channels;
        let frames = input.len() / ch;
        let mut out = Vec::with_capacity(((frames as f64 / self.step) as usize + 2) * ch);
        for i in 0..frames {
            let cur = &input[i * ch..(i + 1) * ch];
            if !self.has_prev {
                self.prev.copy_from_slice(cur);
                self.has_prev = true;
                continue;
            }
            while self.pos < 1.0 {
                for c in 0..ch {
                    out.push(self.prev[c] + (cur[c] - self.prev[c]) * self.pos as f32);
                }
                self.pos += self.step;
            }
            self.pos -= 1.0;
            self.prev.copy_from_slice(cur);
        }
        out
    }
}

/// One-shot whole-buffer resample helper (interleaved).
pub fn resample_all(
    input: &[f32],
    in_rate: u32,
    out_rate: u32,
    channels: usize,
) -> anyhow::Result<Vec<f32>> {
    if in_rate == out_rate || input.is_empty() {
        return Ok(input.to_vec());
    }
    let mut rs = LinearResampler::new(in_rate, out_rate, channels)?;
    Ok(rs.process(input))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_rate_is_passthrough() {
        let input: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let out = resample_all(&input, 48_000, 48_000, 1).unwrap();
        assert_eq!(out, input);
    }

    #[test]
    fn upsample_441_to_48_preserves_dc() {
        let input = vec![0.75f32; 441 * 2];
        let out = resample_all(&input, 44_100, 48_000, 2).unwrap();
        // ~480 output frames minus one frame of initial latency, 2 ch
        assert!(out.len() >= 478 * 2, "out len {}", out.len());
        for v in &out {
            assert!((v - 0.75).abs() < 1e-6);
        }
    }

    #[test]
    fn downsample_96_to_48_keeps_shape() {
        let mut input = Vec::new();
        for i in 0..96 {
            let v = (i as f32 / 96.0).sin();
            input.push(v);
            input.push(v);
        }
        let mut rs = LinearResampler::new(96_000, 48_000, 2).unwrap();
        let out = rs.process(&input);
        assert!(!out.is_empty());
        assert_eq!(out.len() % 2, 0);
    }

    #[test]
    fn linear_resampler_state_continues_across_chunks() {
        let input: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.01).sin()).collect();
        let whole = resample_all(&input, 44_100, 48_000, 1).unwrap();
        let mut rs = LinearResampler::new(44_100, 48_000, 1).unwrap();
        let mut chunked = Vec::new();
        for chunk in input.chunks(97) {
            chunked.extend(rs.process(chunk));
        }
        assert_eq!(whole.len(), chunked.len());
        for (a, b) in whole.iter().zip(chunked.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }
}
