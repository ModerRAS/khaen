//! Lightweight signal analysis for self-check (tone frequency / level / clipping).

/// RMS of a block of samples.
pub fn rms(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|s| (*s as f64) * (*s as f64)).sum();
    (sum / samples.len() as f64).sqrt()
}

/// 20*log10(rms) with a floor at -200 dB for silence.
pub fn rms_db(samples: &[f32]) -> f64 {
    let r = rms(samples);
    if r <= 1e-10 {
        -200.0
    } else {
        20.0 * r.log10()
    }
}

pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

/// Goertzel single-bin amplitude estimate (full-scale == 1.0).
pub fn goertzel_amplitude(samples: &[f32], rate: f64, freq: f64) -> f64 {
    let n = samples.len();
    if n == 0 || rate <= 0.0 {
        return 0.0;
    }
    let w = 2.0 * std::f64::consts::PI * freq / rate;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for x in samples {
        let s0 = *x as f64 + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let power = s1 * s1 + s2 * s2 - coeff * s1 * s2;
    // For a real sinusoid of amplitude A, |X(f)| = A*N/2.
    (2.0 * power.max(0.0).sqrt() / n as f64).min(10.0)
}

/// Estimate a tone's frequency *and* peak amplitude near `center_hz`.
///
/// A plain Goertzel evaluation at the nominal frequency is extremely sensitive to
/// tiny clock offsets over multi-second windows (virtual-cable SRC shifts tones by
/// ~0.05%, which nulls a 2 kHz Goertzel over 3 s). Two-stage scan (coarse then a
/// fine scan one coarse-bin wide) + parabolic refinement gives sub-0.1 Hz accuracy
/// and a peak magnitude close to the true tone amplitude.
pub fn tone_estimate(
    samples: &[f32],
    rate: f64,
    center_hz: f64,
    range_pct: f64,
    bins: usize,
) -> (f64, f64) {
    if samples.is_empty() || center_hz <= 0.0 || rate <= 0.0 || bins < 2 {
        return (0.0, 0.0);
    }
    let span = (center_hz * range_pct.max(0.001)).max(1.0);
    let lo = (center_hz - span).max(1.0);
    let hi = center_hz + span;
    let step = (hi - lo) / bins as f64;

    let scan = |lo: f64, hi: f64, bins: usize| -> (f64, f64, usize, f64) {
        let st = (hi - lo) / bins as f64;
        let mut best = 0usize;
        let mut best_amp = f64::NEG_INFINITY;
        for i in 0..=bins {
            let f = lo + st * i as f64;
            let p = goertzel_amplitude(samples, rate, f);
            if p > best_amp {
                best_amp = p;
                best = i;
            }
        }
        (lo + st * best as f64, best_amp, best, st)
    };

    let (coarse_f, _, _, _) = scan(lo, hi, bins);
    let (fine_lo, fine_hi) = (coarse_f - step, coarse_f + step);
    let (fine_f, mut peak_amp, fine_idx, fine_step) = scan(fine_lo.max(1.0), fine_hi, bins);
    let mut freq = fine_f;
    if fine_idx > 0 && fine_idx < bins {
        let y1 = goertzel_amplitude(samples, rate, freq - fine_step);
        let y2 = goertzel_amplitude(samples, rate, freq);
        let y3 = goertzel_amplitude(samples, rate, freq + fine_step);
        let denom = y1 - 2.0 * y2 + y3;
        if denom.abs() > 1e-15 {
            let delta = 0.5 * (y1 - y3) / denom;
            if delta.abs() < 0.5 {
                freq += fine_step * delta;
                let vertex = y2 - 0.25 * (y1 - y3) * delta;
                peak_amp = vertex.max(peak_amp);
            }
        }
    }
    (freq, peak_amp.max(0.0))
}

/// Refine the dominant frequency near `center_hz` using a Goertzel scan.
/// Robust for finite captures with leading/trailing silence (unlike zero-crossing
/// counting, which is sensitive to partial cycles at block edges).
pub fn estimate_frequency_near(
    samples: &[f32],
    rate: f64,
    center_hz: f64,
    range_pct: f64,
    bins: usize,
) -> f64 {
    if samples.is_empty() || center_hz <= 0.0 || rate <= 0.0 || bins == 0 {
        return 0.0;
    }
    let span = (center_hz * range_pct.max(0.001)).max(1.0);
    let lo = (center_hz - span).max(1.0);
    let hi = center_hz + span;
    let mut best_f = center_hz;
    let mut best_p = -1.0f64;
    for i in 0..=bins {
        let f = lo + (hi - lo) * (i as f64) / (bins as f64);
        let p = goertzel_amplitude(samples, rate, f);
        if p > best_p {
            best_p = p;
            best_f = f;
        }
    }
    best_f
}

/// Estimate the dominant frequency of a near-sinusoidal signal via interpolated
/// zero crossings (positive-going). Good for tone self-check; not a general FFT.
pub fn estimate_frequency(samples: &[f32], rate: f64) -> f64 {
    if samples.len() < 4 || rate <= 0.0 {
        return 0.0;
    }
    let mut crossings = 0u64;
    let mut first_pos: Option<f64> = None;
    let mut last_pos: Option<f64> = None;
    for i in 1..samples.len() {
        let a = samples[i - 1];
        let b = samples[i];
        if a < 0.0 && b >= 0.0 {
            let frac = (-a as f64) / ((b - a) as f64);
            let pos = (i - 1) as f64 + frac;
            if first_pos.is_none() {
                first_pos = Some(pos);
            }
            last_pos = Some(pos);
            if last_pos != first_pos {
                crossings += 1;
            }
        }
    }
    match (first_pos, last_pos) {
        (Some(f), Some(l)) if crossings > 0 && l > f => (crossings as f64) * rate / (l - f),
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(n: usize, freq: f64, rate: f64, amp: f64) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / rate).sin() * amp)
            .map(|v| v as f32)
            .collect()
    }

    #[test]
    fn rms_matches_sine_amplitude() {
        let s = sine(48_000, 1000.0, 48_000.0, 0.5);
        assert!((rms_db(&s) - (-9.03)).abs() < 0.05, "rms_db={}", rms_db(&s));
        assert!((rms(&s) - 0.35355).abs() < 1e-3);
        assert!((peak(&s) - 0.5).abs() < 1e-3);
    }

    #[test]
    fn goertzel_detects_tone_and_ignores_other() {
        let s = sine(48_000, 1000.0, 48_000.0, 0.5);
        let a = goertzel_amplitude(&s, 48_000.0, 1000.0);
        let b = goertzel_amplitude(&s, 48_000.0, 1300.0);
        assert!((a - 0.5).abs() < 0.01, "amp at 1k = {a}");
        assert!(b < 0.05, "amp at 1.3k = {b}");
    }

    #[test]
    fn tone_estimate_refines_frequency_and_amplitude() {
        for (f, amp) in [(1000.0, 0.5), (1999.5, 0.4), (440.0, 0.25)] {
            let s = sine(48_000 * 3, f, 48_000.0, amp);
            let (est_f, est_a) = tone_estimate(&s, 48_000.0, f, 0.05, 101);
            assert!((est_f - f).abs() < 0.15, "f={f} est={est_f}");
            assert!((est_a - amp).abs() < 0.02, "amp={amp} est={est_a}");
        }
    }

    #[test]
    fn goertzel_near_handles_leading_silence() {
        let mut s = vec![0.0f32; 48_000 / 2]; // 0.5 s silence
        s.extend(sine(48_000, 1000.0, 48_000.0, 0.5));
        // Half a cycle may be clipped at the boundary; the estimate must still be close.
        let est = estimate_frequency_near(&s, 48_000.0, 1000.0, 0.05, 101);
        assert!((est - 1000.0).abs() < 1.0, "est={est}");
        assert!(estimate_frequency(&s, 48_000.0) > 0.0);
    }

    #[test]
    fn zero_crossing_frequency_is_close() {
        for f in [440.0, 1000.0, 2000.0, 5000.0] {
            let s = sine(9_600, f, 48_000.0, 0.5);
            let est = estimate_frequency(&s, 48_000.0);
            assert!((est - f).abs() / f < 0.01, "f={f} est={est}");
        }
    }

    #[test]
    fn silence_is_quiet() {
        let s = vec![0.0f32; 1000];
        assert!(rms_db(&s) < -100.0);
        assert!((goertzel_amplitude(&s, 48_000.0, 1000.0)).abs() < 1e-9);
    }
}
