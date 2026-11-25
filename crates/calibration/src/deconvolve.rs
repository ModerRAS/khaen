//! Farina 式反卷积（原 `khaen-cal-dsp/src/deconvolve.rs`，迁入本 crate）。
//!
//! `ir_extract::deconvolve` 转调这里；与 v1 analyze_sweep.py 的 deconvolve 同口径。

use crate::microfft::{irfft, next_pow2_at_least, rfft, C64};

/// Farina 式反卷积（与 analyze_sweep.py deconvolve 同口径）。
pub fn deconvolve(seg: &[f64], sweep: &[f64], tail_frames: usize, reg: f64) -> Vec<f64> {
    let nfft = next_pow2_at_least(seg.len() + sweep.len());
    let y = rfft(seg, nfft);
    let x = rfft(sweep, nfft);
    let power: Vec<f64> = x.iter().map(|c| c.norm_sqr()).collect();
    let max_power = power.iter().cloned().fold(0.0f64, f64::max);
    let denom_add = reg.max(0.0) * max_power;
    let spec: Vec<C64> = y
        .iter()
        .zip(x.iter())
        .zip(power.iter())
        .map(|((y_k, x_k), p)| {
            let denom = *p + denom_add;
            if denom <= 0.0 {
                return C64::new(0.0, 0.0);
            }
            let numerator = C64::new(
                y_k.re * x_k.re + y_k.im * x_k.im,
                y_k.im * x_k.re - y_k.re * x_k.im,
            );
            C64::new(numerator.re / denom, numerator.im / denom)
        })
        .collect();
    let ir = irfft(&spec, nfft);
    let keep = (sweep.len() + tail_frames).min(ir.len());
    ir[..keep].to_vec()
}
