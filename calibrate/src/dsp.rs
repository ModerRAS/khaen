//! FFT、Farina 反卷积、分数延迟（相位斜率）、频响/相位提取。
use num_complex::Complex;
use rustfft::FftPlanner;

use crate::model::{FrPoint, PolarityEstimators};

pub fn next_pow2(n: usize) -> usize {
    let mut p = 1usize;
    while p < n.max(2) {
        p <<= 1;
    }
    p
}

fn fft(data: &mut Vec<Complex<f64>>, inverse: bool) {
    let n = data.len();
    let mut planner = FftPlanner::<f64>::new();
    let fft = if inverse {
        planner.plan_fft_inverse(n)
    } else {
        planner.plan_fft_forward(n)
    };
    fft.process(data);
    if inverse {
        let scale = 1.0 / n as f64;
        for v in data.iter_mut() {
            *v *= scale;
        }
    }
}

/// 互相关求粗延迟（正数表示采集里源从该样本开始）。
pub fn cross_correlation_delay(capture: &[f64], source: &[f64]) -> (i64, f64) {
    let n = next_pow2(capture.len() + source.len());
    let mut a = vec![Complex::new(0.0, 0.0); n];
    let mut b = vec![Complex::new(0.0, 0.0); n];
    for (i, &v) in capture.iter().enumerate() {
        a[i] = Complex::new(v, 0.0);
    }
    for (i, &v) in source.iter().enumerate() {
        b[i] = Complex::new(v, 0.0);
    }
    fft(&mut a, false);
    fft(&mut b, false);
    for i in 0..n {
        a[i] *= b[i].conj();
    }
    fft(&mut a, true);
    let mut best = 0usize;
    let mut best_v = 0.0f64;
    for (i, v) in a.iter().enumerate() {
        let m = v.norm();
        if m > best_v {
            best_v = m;
            best = i;
        }
    }
    let mut lag = best as i64;
    if lag > (n / 2) as i64 {
        lag -= n as i64;
    }
    (lag, best_v)
}

/// Farina 逆滤波：时间反转扫频 × 幅度补偿（对数扫频能量随时间降低）。
pub fn farina_inverse_filter(
    source: &[f64],
    sample_rate: f64,
    start_hz: f64,
    end_hz: f64,
) -> Vec<f64> {
    let n = source.len();
    if n == 0 {
        return Vec::new();
    }
    let t_len = n as f64 / sample_rate;
    let ratio = (end_hz / start_hz).max(1.0001);
    let decay = ratio.ln() / t_len.max(1e-9);
    let mut inv = vec![0.0f64; n];
    for i in 0..n {
        let t = i as f64 / sample_rate;
        // Farina: inv(t) = x(T-t) * exp(+decay*(T-t))  —— 包络 ∝ 1/f(t)，抵消扫频 -3dB/oct 频谱
        inv[i] = source[n - 1 - i] * (decay * (t_len - t)).exp();
    }
    let energy: f64 = inv.iter().map(|v| v * v).sum::<f64>().sqrt();
    if energy > 0.0 {
        for v in &mut inv {
            *v /= energy;
        }
    }
    inv
}

/// 从频域 H 的相位斜率求分数延迟（样本），频带 200–8000 Hz。
pub fn fractional_delay_samples(h: &[Complex<f64>], rate: f64, n: usize) -> f64 {
    let f_lo = 200.0f64;
    let f_hi = 8000.0f64.min(rate * 0.45);
    let mut xs: Vec<f64> = Vec::new();
    let mut ys: Vec<f64> = Vec::new();
    let mut prev: Option<f64> = None;
    let mut unwrapped = 0.0f64;
    let bin_lo = ((f_lo * n as f64 / rate).ceil() as usize).max(1);
    let bin_hi = ((f_hi * n as f64 / rate).floor() as usize).min(n / 2 - 1);
    if bin_hi <= bin_lo + 4 {
        return 0.0;
    }
    for k in bin_lo..=bin_hi {
        let mut ph = h[k].arg();
        if let Some(p) = prev {
            while ph - p > std::f64::consts::PI {
                ph -= 2.0 * std::f64::consts::PI;
            }
            while ph - p < -std::f64::consts::PI {
                ph += 2.0 * std::f64::consts::PI;
            }
            unwrapped += ph - p;
        }
        prev = Some(ph);
        xs.push(k as f64 * rate / n as f64);
        ys.push(unwrapped);
    }
    let mean_x = xs.iter().sum::<f64>() / xs.len() as f64;
    let mean_y = ys.iter().sum::<f64>() / ys.len() as f64;
    let mut num = 0.0;
    let mut den = 0.0;
    for i in 0..xs.len() {
        num += (xs[i] - mean_x) * (ys[i] - mean_y);
        den += (xs[i] - mean_x) * (xs[i] - mean_x);
    }
    if den.abs() < 1e-30 {
        return 0.0;
    }
    let slope = num / den; // rad/Hz
    -slope * rate / (2.0 * std::f64::consts::PI)
}

pub struct DeconvResult {
    pub delay_samples: f64,
    pub delay_coarse_samples: i64,
    /// 旧判据：IR 全局最大绝对值点的符号。不带 --robust-polarity / --repeats>1 时写入 analysis 的 polarity。
    pub polarity: i32,
    pub peak_idx: usize,
    /// 可靠估计器投票。SUB 带（截止 ≤200 Hz）且无近场时为 0。
    pub robust_polarity: i32,
    /// 投票符号，SUB 强制 unknown 之前的值。
    pub polarity_listening: i32,
    pub polarity_confidence: String,
    pub estimators: PolarityEstimators,
    pub peak_db: f64,
    pub phase_1k_deg: f64,
    /// 全带 SNR（旧口径，保持兼容）。
    pub snr_db: f64,
    /// 通道自身通带内的 SNR（P0-3；带限通道如 SUB 用 20-130 Hz）。
    pub snr_band_db: f64,
    /// snr_band_db 所用频带 (lo, hi)。
    pub snr_band_hz: (f64, f64),
    pub fr: Vec<FrPoint>,
    /// 1/48 oct 网格，给模态检测用。默认 `fr` 仍是 300 点。
    pub fr_modal: Vec<FrPoint>,
    pub impulse: Vec<f64>,
}

#[derive(Clone, Debug)]
pub struct PolarityDecision {
    pub robust: i32,
    pub listening: i32,
    pub confidence: String,
}

/// P0-3：SNR 统计频带 = 通道自身通带（20..min(end_hz, 0.45fs)）；
/// 过窄时退回 onset_band，保证 SUB/带限通道有足够的统计带宽。
fn snr_band_range(start_hz: f64, end_hz: f64, rate: u32) -> (f64, f64) {
    let fmax = (rate as f64 * 0.45).min(20_000.0);
    let lo = start_hz.max(20.0).min(fmax * 0.9);
    let hi = end_hz.min(fmax);
    if hi > lo * 1.05 {
        return (lo, hi);
    }
    let (blo, bhi) = onset_band(start_hz, end_hz);
    (blo.max(20.0).min(fmax * 0.9), bhi.min(fmax))
}

/// 通带内 SNR：扫频段（含直达声/房间）功率 vs 扫频结束后的尾部噪声功率。
/// 结果钳到 [-40, 120] dB；尾巴太短时退回全带 SNR（由调用方决定）。
fn snr_in_band(seg: &[f64], signal_len: usize, rate: f64, lo: f64, hi: f64) -> f64 {
    if seg.is_empty() || !(lo.is_finite() && hi.is_finite()) || hi <= lo * 1.01 {
        return 0.0;
    }
    let nyq = rate * 0.5;
    let b = bandpass(seg, rate, lo.min(nyq * 0.95), hi.min(nyq * 0.95));
    let n = signal_len.min(b.len());
    if n < 256 || b.len() < n + 256 {
        return 0.0;
    }
    let sig_pow = b[..n].iter().map(|v| v * v).sum::<f64>() / n as f64;
    // 噪声只取扫频结束后 100 ms（repeat-gap 通常 200 ms），避免把下一遍扫频算进噪声。
    let noise_end = (n + (0.1 * rate) as usize).min(b.len());
    if noise_end <= n + 64 {
        return 0.0;
    }
    let noise_pow = b[n..noise_end].iter().map(|v| v * v).sum::<f64>() / (noise_end - n) as f64;
    let clean = (sig_pow - noise_pow).max(0.0);
    let snr = if noise_pow > 1e-30 {
        10.0 * (clean.max(1e-30) / noise_pow).log10()
    } else {
        120.0
    };
    snr.clamp(-40.0, 120.0)
}

/// Farina 反卷积：粗延迟（互相关）→ 对齐段 → 逆滤波卷积 → IR 峰 → 分数延迟（相位斜率）+ 极性。
pub fn analyze_capture(
    capture: &[f64],
    source: &[f64],
    rate: u32,
    start_hz: f64,
    end_hz: f64,
    pre_samples: usize,
    fr_points: Option<usize>,
) -> Result<DeconvResult, String> {
    if capture.is_empty() || source.is_empty() {
        return Err("empty capture/source".into());
    }
    let (coarse, _) = cross_correlation_delay(capture, source);
    let start = if coarse >= 0 { coarse as usize } else { 0 };
    if start >= capture.len() {
        return Err(format!("coarse delay {coarse} outside capture"));
    }
    // 对齐段长度：源 + 0.5s 尾部
    let tail = (rate as usize) / 2;
    let seg_len = (source.len() + tail)
        .min(capture.len() - start)
        .max(source.len());
    let mut seg = vec![0.0f64; seg_len];
    let avail = (capture.len() - start).min(seg_len);
    seg[..avail].copy_from_slice(&capture[start..start + avail]);

    // Farina 逆滤波 + 频域卷积
    let inv = farina_inverse_filter(source, rate as f64, start_hz, end_hz);
    let n = next_pow2(seg_len + inv.len());
    let mut a = vec![Complex::new(0.0, 0.0); n];
    let mut b = vec![Complex::new(0.0, 0.0); n];
    for (i, &v) in seg.iter().enumerate() {
        a[i] = Complex::new(v, 0.0);
    }
    for (i, &v) in inv.iter().enumerate() {
        b[i] = Complex::new(v, 0.0);
    }
    fft(&mut a, false);
    fft(&mut b, false);
    for i in 0..n {
        a[i] *= b[i];
    }
    fft(&mut a, true);
    let impulse: Vec<f64> = a.iter().map(|c| c.re).collect();

    // IR 峰（对齐段内）
    let search = seg_len.min(impulse.len());
    let mut peak_idx = 0usize;
    let mut peak_abs = 0.0f64;
    for i in 0..search {
        if impulse[i].abs() > peak_abs {
            peak_abs = impulse[i].abs();
            peak_idx = i;
        }
    }
    let polarity = if impulse[peak_idx] < 0.0 { -1 } else { 1 };
    let peak_db = 20.0 * peak_abs.max(1e-15).log10();

    // 频响 H = Y * conj(X)/(|X|^2+eps)，用于 FR 与相位斜率
    let nf = next_pow2(seg_len.max(source.len() * 2));
    let mut x = vec![Complex::new(0.0, 0.0); nf];
    let mut y = vec![Complex::new(0.0, 0.0); nf];
    for (i, &v) in source.iter().enumerate() {
        x[i] = Complex::new(v, 0.0);
    }
    for (i, &v) in seg.iter().enumerate() {
        y[i] = Complex::new(v, 0.0);
    }
    fft(&mut x, false);
    fft(&mut y, false);
    let max_power = x.iter().map(|v| v.norm_sqr()).fold(0.0f64, f64::max);
    let eps = (max_power * 1e-8).max(1e-12);
    let mut h = vec![Complex::new(0.0, 0.0); nf];
    for i in 0..nf {
        h[i] = y[i] * x[i].conj() / (x[i].norm_sqr() + eps);
    }
    let frac = fractional_delay_samples(&h, rate as f64, nf);
    let delay_samples = coarse as f64 + frac - pre_samples as f64;
    let phase_1k = {
        let k = (1000.0 * nf as f64 / rate as f64).round() as usize;
        h[k.min(nf / 2 - 1)].arg().to_degrees()
    };
    let fr = match fr_points {
        Some(n) => fr_from_h_points(&h, rate, nf, n),
        None => fr_from_h(&h, rate, nf),
    };
    let f_max = (rate as f64 * 0.45).min(20_000.0);
    let fr_modal = fr_from_h_points(&h, rate, nf, fr_point_count(20.0, f_max, 48.0));
    // 粗略 SNR：IR 峰值能量 vs 尾部噪声
    let noise: f64 = impulse[search / 2..search]
        .iter()
        .map(|v| v * v)
        .sum::<f64>();
    let snr_db = if noise > 0.0 {
        10.0 * (peak_abs * peak_abs / (noise / (search / 2).max(1) as f64)).log10()
    } else {
        120.0
    };
    // P0-3：带限通道（如 SUB 20-130 Hz）只在自身通带内统计 SNR；
    // 全带 SNR 会被带外噪声/隆隆声压低，导致 SUB 被 25 dB 门限误排除。
    let (snr_lo, snr_hi) = snr_band_range(start_hz, end_hz, rate);
    let snr_band_db = snr_in_band(&seg, source.len(), rate as f64, snr_lo, snr_hi);
    let ir = &impulse[..search];
    let (lo, hi) = polarity_bands(start_hz, end_hz);
    let estimators = PolarityEstimators {
        ir_peak: polarity,
        direct_window: direct_window_sign(ir, rate as f64),
        phase_vote: phase_vote_sign(&h, rate, frac, start_hz, end_hz),
        lowband: lowband_sign(&seg, source, rate as f64, lo, hi),
        matched: matched_sign(&seg, source),
        reference_xcorr: None,
    };
    let sub_band = end_hz <= 200.0;
    let decision = decide_polarity(&estimators, sub_band);
    Ok(DeconvResult {
        delay_samples,
        delay_coarse_samples: coarse,
        polarity,
        peak_idx,
        robust_polarity: decision.robust,
        polarity_listening: decision.listening,
        polarity_confidence: decision.confidence,
        estimators,
        peak_db,
        phase_1k_deg: phase_1k,
        snr_db,
        snr_band_db,
        snr_band_hz: (snr_lo, snr_hi),
        fr,
        fr_modal,
        impulse,
    })
}

fn majority_known(votes: &[i32]) -> Option<(i32, bool)> {
    let known: Vec<i32> = votes.iter().copied().filter(|v| *v != 0).collect();
    if known.len() < 2 {
        return None;
    }
    let pos = known.iter().filter(|v| **v > 0).count();
    let neg = known.len() - pos;
    if pos == known.len() {
        Some((1, true))
    } else if neg == known.len() {
        Some((-1, true))
    } else if pos > neg {
        Some((1, false))
    } else if neg > pos {
        Some((-1, false))
    } else {
        None
    }
}

/// 主票是 direct_window / lowband / matched。三者一致即为 high，ir_peak 与 phase_vote 都不能否决。
/// 主票不齐时再把 phase_vote 算进去：全体一致才是 high，否则多数为 low，打平或不足 2 票为 unknown。
/// SUB 带（调用方 sub_listening_only）不给出可提交符号，listening 仍保留投票。
pub fn decide_polarity(est: &PolarityEstimators, sub_listening_only: bool) -> PolarityDecision {
    let primary = [est.direct_window, est.lowband, est.matched];
    let (listening, high) = if let Some((sign, unanimous)) = majority_known(&primary) {
        if unanimous {
            (sign, true)
        } else {
            (sign, false)
        }
    } else {
        let all = [est.direct_window, est.phase_vote, est.lowband, est.matched];
        match majority_known(&all) {
            Some((sign, unanimous)) => (sign, unanimous),
            None => (0, false),
        }
    };
    let conf = if listening == 0 {
        "unknown"
    } else if high {
        "high"
    } else {
        "low"
    };
    if sub_listening_only {
        PolarityDecision {
            robust: 0,
            listening,
            confidence: "unknown".into(),
        }
    } else {
        PolarityDecision {
            robust: listening,
            listening,
            confidence: conf.into(),
        }
    }
}

pub fn sign_of(v: f64) -> i32 {
    if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        0
    }
}

/// 旧判据，供测试对照。
pub fn legacy_peak_sign(impulse: &[f64]) -> (i32, usize) {
    let mut peak_idx = 0usize;
    let mut peak_abs = 0.0f64;
    for (i, v) in impulse.iter().enumerate() {
        if v.abs() > peak_abs {
            peak_abs = v.abs();
            peak_idx = i;
        }
    }
    if peak_abs == 0.0 {
        return (0, 0);
    }
    (if impulse[peak_idx] < 0.0 { -1 } else { 1 }, peak_idx)
}

/// 直达声窗：在全局峰之前找包络上第一个 ≥ 30% 峰值的点（首达），再对 [t0, t0+2 ms] 求和取符号。
pub fn direct_window_sign(impulse: &[f64], rate: f64) -> i32 {
    if impulse.is_empty() {
        return 0;
    }
    let (peak_sign, peak_idx) = legacy_peak_sign(impulse);
    if peak_sign == 0 {
        return 0;
    }
    let peak_abs = impulse[peak_idx].abs();
    let thresh = 0.30 * peak_abs;
    let mut t0 = peak_idx;
    for i in 0..=peak_idx {
        if impulse[i].abs() >= thresh {
            t0 = i;
            break;
        }
    }
    let win = ((0.002 * rate).round() as usize).max(1);
    let end = (t0 + win).min(impulse.len());
    let sum: f64 = impulse[t0..end].iter().sum();
    if sum.abs() < 1e-15 {
        return 0;
    }
    sign_of(sum)
}

fn polarity_bands(start_hz: f64, end_hz: f64) -> (f64, f64) {
    if end_hz >= 500.0 && start_hz <= 100.0 {
        (100.0, 500.0)
    } else {
        let lo = (start_hz * 1.15).max(1.0);
        let hi = (end_hz * 0.90).max(lo * 1.05);
        (lo, hi)
    }
}

/// 去掉分数延迟后，看 100/200/300/400 Hz 的 cos(phase)。带外或幅度过小的点弃权。
pub fn phase_vote_sign(
    h: &[Complex<f64>],
    rate: u32,
    frac_delay: f64,
    start_hz: f64,
    end_hz: f64,
) -> i32 {
    let mut score = 0i32;
    let mut n = 0i32;
    for f in [100.0, 200.0, 300.0, 400.0] {
        if f < start_hz * 1.2 || f > end_hz * 0.90 {
            continue;
        }
        let c = interp_complex(h, rate, f);
        if c.norm() < 1e-8 {
            continue;
        }
        let ph = c.arg() + 2.0 * std::f64::consts::PI * f * frac_delay / rate as f64;
        let cs = ph.cos();
        if cs.abs() < 0.05 {
            continue;
        }
        score += if cs > 0.0 { 1 } else { -1 };
        n += 1;
    }
    if n == 0 || score == 0 {
        0
    } else {
        score.signum()
    }
}

pub fn bandpass(x: &[f64], rate: f64, lo: f64, hi: f64) -> Vec<f64> {
    if x.is_empty() {
        return Vec::new();
    }
    let n = next_pow2(x.len());
    let mut a = vec![Complex::new(0.0, 0.0); n];
    for (i, &v) in x.iter().enumerate() {
        a[i] = Complex::new(v, 0.0);
    }
    fft(&mut a, false);
    for k in 0..n {
        let bin = if k <= n / 2 { k } else { n - k };
        let f = bin as f64 * rate / n as f64;
        if f < lo || f > hi {
            a[k] = Complex::new(0.0, 0.0);
        }
    }
    fft(&mut a, true);
    a.iter().take(x.len()).map(|c| c.re).collect()
}

#[derive(Clone, Debug)]
pub struct CorrPeak {
    pub lag: f64,
    pub signed: f64,
    pub mag: f64,
}

pub fn correlation_curve(capture: &[f64], source: &[f64]) -> Vec<f64> {
    let n = next_pow2(capture.len() + source.len());
    let mut a = vec![Complex::new(0.0, 0.0); n];
    let mut b = vec![Complex::new(0.0, 0.0); n];
    for (i, &v) in capture.iter().enumerate() {
        a[i] = Complex::new(v, 0.0);
    }
    for (i, &v) in source.iter().enumerate() {
        b[i] = Complex::new(v, 0.0);
    }
    fft(&mut a, false);
    fft(&mut b, false);
    for i in 0..n {
        a[i] *= b[i].conj();
    }
    fft(&mut a, true);
    a.into_iter().map(|c| c.re).collect()
}

fn parabolic_delta(left: f64, mid: f64, right: f64) -> f64 {
    let den = left - 2.0 * mid + right;
    if den.abs() < 1e-18 {
        return 0.0;
    }
    (0.5 * (left - right) / den).clamp(-0.5, 0.5)
}

/// 起点检测用的通带。全带扫频收进 80–8000 Hz，并且不超过扫频通带的 0.9 倍；
/// 窄带/SUB 不用固定的 20–130，按 start/end 收进 90% 带宽。
pub fn onset_band(start_hz: f64, end_hz: f64) -> (f64, f64) {
    let start_hz = start_hz.max(1.0);
    let end_hz = end_hz.max(start_hz * 1.01);
    let full = end_hz >= 1000.0 && end_hz / start_hz >= 8.0;
    if full {
        let lo = 80.0_f64.max(start_hz);
        let hi = 8000.0_f64.min(end_hz * 0.9);
        if hi > lo * 1.05 {
            return (lo, hi);
        }
    }
    let lo = start_hz;
    let hi = (start_hz + 0.9 * (end_hz - start_hz)).max(lo * 1.05);
    (lo, hi.min(end_hz).max(lo * 1.05))
}

/// 长度 `win` 的滑窗能量。窗口超出末尾时只累计还在缓冲里的样本。
fn sliding_energy(cap: &[f64], win: usize) -> Vec<f64> {
    let n = cap.len();
    let mut energy = vec![0.0; n];
    if n == 0 || win == 0 {
        return energy;
    }
    let w = win.min(n);
    let mut acc = 0.0;
    for v in cap.iter().take(w) {
        acc += v * v;
    }
    energy[0] = acc.max(0.0);
    for k in 1..n {
        acc -= cap[k - 1] * cap[k - 1];
        if k + w - 1 < n {
            let add = cap[k + w - 1];
            acc += add * add;
        }
        if acc < 0.0 {
            acc = 0.0;
        }
        energy[k] = acc;
    }
    energy
}

/// 在互相关上取若干峰。排序用归一化相关 `|c| / sqrt(Σseg² · Σsrc²)`，
/// `signed` 仍是未归一化的互相关，供极性符号使用。只搜索 [0, capture_len)。
/// `refine`：选中归一化峰之后，在 ±refine 内改用未归一化 |c| 的极大值当起点，
/// 避免滑窗能量把对齐点拉向更安静的一侧。极性投票传 0。
pub fn correlation_peaks(
    capture: &[f64],
    source: &[f64],
    count: usize,
    min_sep: usize,
    refine: usize,
) -> Vec<CorrPeak> {
    if capture.is_empty() || source.is_empty() || count == 0 {
        return Vec::new();
    }
    let curve = correlation_curve(capture, source);
    let n_fft = next_pow2(capture.len() + source.len());
    let src_energy: f64 = source.iter().map(|v| v * v).sum();
    let energy = sliding_energy(capture, source.len());
    let n = capture.len().min(curve.len());
    let mut score = vec![0.0f64; n];
    for i in 0..n {
        let seg_e = energy[i];
        if seg_e < 1e-18 || src_energy < 1e-18 {
            continue;
        }
        // correlation_curve 的逆变换除了 N，这里乘回去得到点积。
        score[i] = curve[i].abs() * n_fft as f64 / (seg_e * src_energy).sqrt();
    }
    let sep = min_sep.max(1);
    let mut peaks = Vec::new();
    for _ in 0..count {
        let mut best = 0usize;
        let mut best_v = 0.0f64;
        for (i, &m) in score.iter().enumerate() {
            if m > best_v {
                best_v = m;
                best = i;
            }
        }
        if best_v <= 0.0 {
            break;
        }
        let mut at = best;
        if refine > 0 {
            let lo_r = best.saturating_sub(refine);
            let hi_r = (best + refine + 1).min(n);
            let mut raw_v = curve[at].abs();
            for j in lo_r..hi_r {
                let m = curve[j].abs();
                if m > raw_v {
                    raw_v = m;
                    at = j;
                }
            }
        }
        let left = if at > 0 {
            curve[at - 1].abs()
        } else {
            curve[at].abs()
        };
        let right = if at + 1 < n {
            curve[at + 1].abs()
        } else {
            curve[at].abs()
        };
        let delta = parabolic_delta(left, curve[at].abs(), right);
        peaks.push(CorrPeak {
            lag: at as f64 + delta,
            signed: curve[at],
            mag: best_v,
        });
        let lo = best.saturating_sub(sep);
        let hi = (best + sep).min(score.len());
        for v in &mut score[lo..hi] {
            *v = 0.0;
        }
    }
    peaks.sort_by(|a, b| {
        a.lag
            .partial_cmp(&b.lag)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    peaks
}

/// 在未归一化互相关上，把起点收到 `radius` 以内、且不超过 `limit` 的最大 |c|。
/// 带通后的归一化峰会偏早，真正的对齐仍以全带相关为准。`limit` 用采集长度，避开 FFT 补零后的环绕。
/// 旧 pipeline 的 full-band snap 参考实现；v1.1 起 repeat 路径不再使用它
/// （保留仅为对照/回归，调用方只有测试）。
#[allow(dead_code)]
pub fn snap_correlation_lag(curve: &[f64], lag: f64, radius: usize, limit: usize) -> f64 {
    if curve.is_empty() || radius == 0 || limit == 0 {
        return lag;
    }
    let n = curve.len().min(limit);
    let center = lag.round().max(0.0) as usize;
    if center >= n {
        return lag;
    }
    let lo = center.saturating_sub(radius);
    let hi = (center + radius + 1).min(n);
    let mut at = center.min(n - 1);
    let mut best = curve[at].abs();
    for j in lo..hi {
        let m = curve[j].abs();
        if m > best {
            best = m;
            at = j;
        }
    }
    let left = if at > 0 {
        curve[at - 1].abs()
    } else {
        curve[at].abs()
    };
    let right = if at + 1 < n {
        curve[at + 1].abs()
    } else {
        curve[at].abs()
    };
    at as f64 + parabolic_delta(left, curve[at].abs(), right)
}

/// 先按扫频通带滤掉隆隆声，再做粗延迟。给 measure-multi 的第一遍搜索用。
pub fn bandpassed_delay(
    capture: &[f64],
    source: &[f64],
    rate: f64,
    start_hz: f64,
    end_hz: f64,
) -> (i64, f64) {
    let (lo, hi) = onset_band(start_hz, end_hz);
    let filtered_cap = bandpass(capture, rate, lo, hi);
    let filtered_src = bandpass(source, rate, lo, hi);
    cross_correlation_delay(&filtered_cap, &filtered_src)
}

pub fn matched_sign(seg: &[f64], source: &[f64]) -> i32 {
    let peaks = correlation_peaks(seg, source, 1, 1, 0);
    match peaks.first() {
        Some(p) if p.signed.abs() > 1e-15 => sign_of(p.signed),
        _ => 0,
    }
}

pub fn lowband_sign(seg: &[f64], source: &[f64], rate: f64, lo: f64, hi: f64) -> i32 {
    if hi <= lo {
        return 0;
    }
    let sb = bandpass(seg, rate, lo, hi);
    let src = bandpass(source, rate, lo, hi);
    matched_sign(&sb, &src)
}

/// 两段采集的相对极性（参考麦互相关）。正表示同号。
pub fn relative_polarity(a: &[f64], b: &[f64]) -> i32 {
    matched_sign(a, b)
}

pub fn median_f64(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut xs = values.to_vec();
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = xs.len();
    if n % 2 == 1 {
        xs[n / 2]
    } else {
        0.5 * (xs[n / 2 - 1] + xs[n / 2])
    }
}

pub fn majority_sign(values: &[i32]) -> i32 {
    let pos = values.iter().filter(|v| **v > 0).count();
    let neg = values.iter().filter(|v| **v < 0).count();
    if pos == 0 && neg == 0 {
        0
    } else if pos > neg {
        1
    } else if neg > pos {
        -1
    } else {
        0
    }
}

#[derive(Clone, Debug)]
pub struct ClockFit {
    /// raw ppm estimate; only meaningful when valid (== ppm_ok) is true.
    pub ppm: f64,
    /// 相邻差中位数（样本/重复间隔）。
    pub slope: f64,
    /// 稳健锚点（pass 0 的期望起点，样本）。
    pub intercept: f64,
    pub residual_rms: f64,
    /// ppm_ok：>=2 个被接受的相邻差、|ppm|<=max、且有限。
    pub valid: bool,
    /// true = 该遍起点不可用（逐遍判定，不再“一坏全坏”）。
    pub point_invalid: Vec<bool>,
    /// 与 slope 相同，保留字段名便于报告。
    pub spacing: f64,
    pub accepted_diffs: Vec<f64>,
    pub rejected_diffs: Vec<f64>,
    pub residuals: Vec<f64>,
    pub anchor: f64,
    pub ppm_source: String,
    /// A1：ppm 估计不确定度（1e6 * sqrt(SSE/(n-2)) / span，span=(kmax-kmin)*spacing）。
    pub uncertainty_ppm: f64,
    /// A1：|ppm| > 2*uncertainty_ppm 才认为漂移显著；--drift-correct 只在显著时启用。
    pub significant: bool,
    /// 有效起点少于 2 个（退化拟合，调用方应降级/告警）。
    pub insufficient_valid_passes: bool,
    /// 0 个有效遍时保留“最接近拟合”的两遍并置 true（绝不静默声称 clean）。
    pub fallback_kept_closest_two: bool,
}

/// 相邻差接受门限（样本）：|d - nominal_interval| <= 500 才参与间距/ppm 估计。
pub const CLOCK_DIFF_TOL: f64 = 500.0;
/// 逐遍残差门限（样本）：|onset - (anchor + k*spacing)| > 500 的遍标 invalid。
pub const CLOCK_RESID_TOL: f64 = 500.0;

/// 稳健时钟/栅格拟合（v1.1，替换旧的“0.25×interval + 端点回归”）。
///
/// 规则（见 scratch/lead/fr-repeat/REPORT.md）：
/// 1. 相邻差 d_k = o[k+1]-o[k]；只接受 |d_k - nominal_interval| <= CLOCK_DIFF_TOL 的差；
/// 2. spacing = median(accepted_diffs)（没有 accepted 时用 nominal_interval）；
/// 3. 只有 >=2 个 accepted 相邻差才估 ppm（且 |ppm|<=max_ppm）；否则 valid=false、ppm 无意义；
/// 4. anchor = median{ o[k]-k*spacing, o[k+1]-(k+1)*spacing | d_k accepted }；
///    accepted 为空时用调用方的 anchor_hint（单遍 actual_start），否则用全体中位数；
/// 5. 逐遍残差 r[k]=o[k]-(anchor+k*spacing)，|r|>CLOCK_RESID_TOL 只把该遍标 invalid；
/// 6. 坏点不参与 spacing/ppm；0 个有效遍时才退化保留最接近的 2 遍并告警。
pub fn fit_clock(
    points: &[(usize, f64)],
    nominal_interval: f64,
    max_ppm: f64,
    anchor_hint: Option<f64>,
) -> ClockFit {
    let n = points.len();
    let mut fit = ClockFit {
        ppm: 0.0,
        slope: nominal_interval,
        intercept: points.first().map(|p| p.1).unwrap_or(0.0),
        residual_rms: 0.0,
        valid: false,
        point_invalid: vec![n > 0; n],
        spacing: nominal_interval,
        accepted_diffs: vec![],
        rejected_diffs: vec![],
        residuals: vec![],
        anchor: points.first().map(|p| p.1).unwrap_or(0.0),
        ppm_source: "none".into(),
        uncertainty_ppm: f64::INFINITY,
        significant: false,
        insufficient_valid_passes: n < 2,
        fallback_kept_closest_two: false,
    };
    if n < 2 || !(nominal_interval.is_finite() && nominal_interval > 1.0) {
        return fit;
    }
    let diffs: Vec<(usize, f64)> = (0..n - 1)
        .map(|k| (k, points[k + 1].1 - points[k].1))
        .collect();
    let accepted: Vec<(usize, f64)> = diffs
        .iter()
        .copied()
        .filter(|(_, d)| (d - nominal_interval).abs() <= CLOCK_DIFF_TOL)
        .collect();
    fit.accepted_diffs = accepted.iter().map(|(_, d)| *d).collect();
    fit.rejected_diffs = diffs
        .iter()
        .filter(|(k, _)| !accepted.iter().any(|(ak, _)| ak == k))
        .map(|(_, d)| *d)
        .collect();
    let spacing = if accepted.is_empty() {
        nominal_interval
    } else {
        median_f64(&accepted.iter().map(|(_, d)| *d).collect::<Vec<_>>())
    };
    fit.spacing = spacing;
    fit.slope = spacing;
    let ppm_raw = (spacing / nominal_interval - 1.0) * 1e6;
    let ppm_ok = accepted.len() >= 2 && ppm_raw.abs() <= max_ppm && ppm_raw.is_finite();
    fit.ppm = if ppm_ok { ppm_raw } else { 0.0 };
    fit.valid = ppm_ok;
    fit.ppm_source = if ppm_ok {
        "median_adjacent_diffs".into()
    } else if accepted.is_empty() {
        "no_accepted_adjacent_diff".into()
    } else {
        "insufficient_accepted_adjacent_diffs".into()
    };
    let anchor = if !accepted.is_empty() {
        let mut anchors = Vec::with_capacity(accepted.len() * 2);
        for (k, _) in &accepted {
            anchors.push(points[*k].1 - *k as f64 * spacing);
            anchors.push(points[k + 1].1 - (k + 1) as f64 * spacing);
        }
        median_f64(&anchors)
    } else if let Some(hint) = anchor_hint {
        hint
    } else {
        median_f64(
            &points
                .iter()
                .map(|(k, y)| y - *k as f64 * spacing)
                .collect::<Vec<_>>(),
        )
    };
    fit.anchor = anchor;
    fit.intercept = anchor;
    let residuals: Vec<f64> = points
        .iter()
        .map(|(k, y)| y - (anchor + *k as f64 * spacing))
        .collect();
    let mut invalid: Vec<bool> = residuals
        .iter()
        .map(|r| r.abs() > CLOCK_RESID_TOL)
        .collect();
    let valid_count = invalid.iter().filter(|v| !**v).count();
    if valid_count == 0 {
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| {
            residuals[a]
                .abs()
                .partial_cmp(&residuals[b].abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let keep = order.iter().take(2.min(n)).copied().collect::<Vec<_>>();
        invalid = (0..n).map(|k| !keep.contains(&k)).collect();
        fit.fallback_kept_closest_two = true;
    }
    let valid_idx: Vec<usize> = (0..n).filter(|k| !invalid[*k]).collect();
    fit.insufficient_valid_passes = valid_idx.len() < 2;
    // 2 遍且没有 accepted 相邻差、也没有单遍 anchor_hint：无法判断哪一遍坏，
    // 诚实标记“证据不足”，不能声称 clean fit。
    if n == 2 && accepted.is_empty() && anchor_hint.is_none() {
        fit.insufficient_valid_passes = true;
    }
    fit.residual_rms = if valid_idx.is_empty() {
        0.0
    } else {
        (valid_idx
            .iter()
            .map(|k| residuals[*k] * residuals[*k])
            .sum::<f64>()
            / valid_idx.len() as f64)
            .sqrt()
    };
    // A1：ppm 不确定度由残差 + 跨度估计；只有 |ppm| > 2σ 才把漂移当物理量。
    let valid_ks: Vec<usize> = valid_idx.clone();
    if valid_ks.len() >= 2 && spacing.is_finite() && spacing > 0.0 {
        let kmin = *valid_ks.iter().min().unwrap();
        let kmax = *valid_ks.iter().max().unwrap();
        let span = (kmax - kmin) as f64 * spacing;
        let sse: f64 = valid_ks.iter().map(|k| residuals[*k] * residuals[*k]).sum();
        let dof = (valid_ks.len() as f64 - 2.0).max(1.0);
        if span > 1.0 {
            fit.uncertainty_ppm = 1e6 * (sse / dof).sqrt() / span;
            fit.significant = ppm_ok && fit.ppm.abs() > 2.0 * fit.uncertainty_ppm;
        }
    }
    fit.residuals = residuals;
    fit.point_invalid = invalid;
    fit
}

/// stretch>1 把信号拉长（特征间隔变大）。32 点 Hann 窗 sinc，stretch≈1 时直接拷贝。
pub fn resample_sinc(x: &[f64], stretch: f64) -> Vec<f64> {
    if x.is_empty() {
        return Vec::new();
    }
    if (stretch - 1.0).abs() < 1e-12 {
        return x.to_vec();
    }
    let stretch = stretch.max(1e-6);
    let n_out = ((x.len() as f64) * stretch).round().max(1.0) as usize;
    let taps = 32isize;
    let mut out = vec![0.0f64; n_out];
    for i in 0..n_out {
        let pos = i as f64 / stretch;
        let mut acc = 0.0;
        let center = pos.round() as isize;
        for k in (center - taps)..=(center + taps) {
            if k < 0 || k >= x.len() as isize {
                continue;
            }
            let t = pos - k as f64;
            let w = if t.abs() < 1e-8 {
                1.0
            } else if t.abs() >= taps as f64 {
                0.0
            } else {
                let sinc = (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t);
                let hann = 0.5 * (1.0 + (std::f64::consts::PI * t / taps as f64).cos());
                sinc * hann
            };
            acc += x[k as usize] * w;
        }
        out[i] = acc;
    }
    out
}

/// 多遍频响：按各自延迟对齐到中位数后再做复数平均，最后重新按 1 kHz 归一化。
pub fn average_fr(frs: &[Vec<FrPoint>], delays: &[f64], rate: f64) -> Vec<FrPoint> {
    if frs.is_empty() {
        return Vec::new();
    }
    let med = median_f64(delays);
    let n = frs[0].len();
    let mut raw: Vec<(f64, f64, f64)> = Vec::with_capacity(n);
    for i in 0..n {
        let f = frs[0][i].freq_hz;
        let mut re = 0.0;
        let mut im = 0.0;
        let mut cnt = 0.0;
        for (fr, d) in frs.iter().zip(delays.iter()) {
            if i >= fr.len() {
                continue;
            }
            let p = &fr[i];
            let mag = 10.0f64.powf(p.db / 20.0);
            let ph = p.phase_deg.to_radians() - 2.0 * std::f64::consts::PI * f * (d - med) / rate;
            re += mag * ph.cos();
            im += mag * ph.sin();
            cnt += 1.0;
        }
        if cnt > 0.0 {
            re /= cnt;
            im /= cnt;
        }
        let db = 20.0 * (re * re + im * im).sqrt().max(1e-12).log10();
        raw.push((f, db, im.atan2(re).to_degrees()));
    }
    let ref_db = {
        let mut best = raw[0];
        for row in &raw {
            if (row.0 - 1000.0).abs() < (best.0 - 1000.0).abs() {
                best = *row;
            }
        }
        best.1
    };
    let mut last = 0.0f64;
    let mut out = Vec::with_capacity(raw.len());
    for (i, (f, db, phase)) in raw.into_iter().enumerate() {
        let mut p = phase;
        if i > 0 {
            while p - last > 180.0 {
                p -= 360.0;
            }
            while p - last < -180.0 {
                p += 360.0;
            }
        }
        last = p;
        out.push(FrPoint {
            freq_hz: f,
            db: db - ref_db,
            phase_deg: p,
        });
    }
    out
}

fn interp_complex(bins: &[Complex<f64>], rate: u32, f: f64) -> Complex<f64> {
    let n = bins.len();
    let pos = f * n as f64 / rate as f64;
    let i0 = pos.floor().max(0.0) as usize;
    let i1 = (i0 + 1).min(n - 1);
    let t = (pos - i0 as f64).clamp(0.0, 1.0);
    bins[i0] * (1.0 - t) + bins[i1] * t
}

/// 从频域响应 H 生成对数频率网格上的幅度/相位（1 kHz 归一化）。默认 300 点。
pub fn fr_from_h(h: &[Complex<f64>], rate: u32, n: usize) -> Vec<FrPoint> {
    fr_from_h_points(h, rate, n, 300)
}

pub fn fr_point_count(f_min: f64, f_max: f64, per_oct: f64) -> usize {
    if !(f_max > f_min) || per_oct <= 0.0 {
        return 2;
    }
    let octaves = (f_max / f_min).ln() / std::f64::consts::LN_2;
    ((octaves * per_oct).round() as usize)
        .saturating_add(1)
        .max(2)
}

pub fn fr_from_h_points(h: &[Complex<f64>], rate: u32, n: usize, points: usize) -> Vec<FrPoint> {
    let points = points.max(2);
    let f_min = 20.0f64;
    let f_max = (rate as f64 * 0.45).min(20_000.0);
    let log_min = f_min.ln();
    let log_max = f_max.ln();
    let mut raw: Vec<(f64, f64, f64)> = Vec::with_capacity(points);
    for i in 0..points {
        let f = (log_min + (log_max - log_min) * i as f64 / (points - 1) as f64).exp();
        let c = interp_complex(&h[..n], rate, f);
        let db = 20.0 * (c.norm().max(1e-12)).log10();
        let phase = c.arg().to_degrees();
        raw.push((f, db, phase));
    }
    let ref_db = {
        let c = interp_complex(&h[..n], rate, 1000.0);
        20.0 * (c.norm().max(1e-12)).log10()
    };
    let mut last = 0.0f64;
    let mut out = Vec::with_capacity(points);
    for (i, (f, db, phase)) in raw.into_iter().enumerate() {
        let mut p = phase;
        if i > 0 {
            while p - last > 180.0 {
                p -= 360.0;
            }
            while p - last < -180.0 {
                p += 360.0;
            }
        }
        last = p;
        out.push(FrPoint {
            freq_hz: f,
            db: db - ref_db,
            phase_deg: p,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convolve(a: &[f64], b: &[f64]) -> Vec<f64> {
        let n = next_pow2(a.len() + b.len());
        let mut x = vec![Complex::new(0.0, 0.0); n];
        let mut y = vec![Complex::new(0.0, 0.0); n];
        for (i, &v) in a.iter().enumerate() {
            x[i] = Complex::new(v, 0.0);
        }
        for (i, &v) in b.iter().enumerate() {
            y[i] = Complex::new(v, 0.0);
        }
        fft(&mut x, false);
        fft(&mut y, false);
        for i in 0..n {
            x[i] *= y[i];
        }
        fft(&mut x, true);
        x.iter().map(|c| c.re).collect()
    }

    fn sweep_like(n: usize, fs: f64) -> Vec<f64> {
        let mut v = Vec::with_capacity(n);
        let f1 = 20.0f64;
        let f2 = 18_000.0f64;
        let t_len = n as f64 / fs;
        for i in 0..n {
            let t = i as f64 / fs;
            let phase = 2.0 * std::f64::consts::PI * f1 * t_len / (f2 / f1).ln()
                * ((f2 / f1).powf(t / t_len) - 1.0);
            v.push(0.01 * phase.sin());
        }
        v
    }

    #[test]
    fn farina_recovers_delay_and_polarity() {
        let fs = 48_000u32;
        let src = sweep_like(48_000, fs as f64); // 1s
                                                 // 延迟 37 样本 + 反相 + 简单低通（用一阶差分模拟）
        let mut path = vec![0.0f64; 64];
        path[37] = -1.0;
        let captured = convolve(&src, &path);
        // 前置静音 0.2s
        let pre = (0.2 * fs as f64) as usize;
        let mut cap = vec![0.0f64; pre];
        cap.extend_from_slice(&captured);
        cap.truncate(pre + src.len() + 4800);
        let r = analyze_capture(&cap, &src, fs, 20.0, 18_000.0, pre, None).unwrap();
        assert!(
            (r.delay_samples - 37.0).abs() < 0.75,
            "delay={}",
            r.delay_samples
        );
        assert_eq!(r.polarity, -1);
        assert!(r.fr.len() > 100);
    }

    #[test]
    fn reflection_five_ms_flips_peak_not_direct_window() {
        let mut ir = vec![0.0f64; 2000];
        ir[40] = -1.0;
        ir[40 + (0.005 * 48_000.0) as usize] = 1.05;
        let (legacy, _) = legacy_peak_sign(&ir);
        assert_eq!(legacy, 1, "旧单点判据被 5 ms 同幅度反射翻成正");
        assert_eq!(
            direct_window_sign(&ir, 48_000.0),
            -1,
            "直达声窗必须保持首达负号"
        );
    }

    #[test]
    fn ir_peak_negative_lowband_positive_stays_high() {
        let fs = 48_000u32;
        let src = sweep_like(24_000, fs as f64);
        let mut path = vec![0.0f64; 900];
        // 直达声每个样本都要超过负峰的 30%，否则首达窗会锁到后面的尖峰上。
        for i in 0..8 {
            path[60 + i] = 0.55;
        }
        path[60 + (0.005 * fs as f64) as usize] = -1.05;
        let captured = convolve(&src, &path);
        let r = analyze_capture(&captured, &src, fs, 20.0, 18_000.0, 0, None).unwrap();
        assert_eq!(
            r.polarity, -1,
            "legacy ir_peak should flip negative, peak_idx={}",
            r.peak_idx
        );
        assert!(r.estimators.lowband > 0, "lowband {}", r.estimators.lowband);
        assert!(
            r.estimators.direct_window > 0,
            "direct {}",
            r.estimators.direct_window
        );
        assert_eq!(r.robust_polarity, 1, "estimators={:?}", r.estimators);
        assert_eq!(
            r.polarity_confidence, "high",
            "estimators={:?}",
            r.estimators
        );
    }

    #[test]
    fn onset_band_full_sweep_and_sub() {
        let (lo, hi) = onset_band(20.0, 20_000.0);
        assert!((lo - 80.0).abs() < 1e-9, "lo={lo}");
        assert!((hi - 8000.0).abs() < 1e-9, "hi={hi}");
        let (sub_lo, sub_hi) = onset_band(20.0, 130.0);
        assert!(sub_lo >= 20.0 - 1e-9 && sub_lo < 40.0, "sub_lo={sub_lo}");
        assert!(
            sub_hi > sub_lo && sub_hi < 129.0,
            "sub band must sit inside the sweep, hi={sub_hi}"
        );
        let (other_lo, other_hi) = onset_band(25.0, 110.0);
        assert!(
            (other_hi - sub_hi).abs() > 1.0 && (other_lo - sub_lo).abs() > 1.0,
            "narrow band must follow start/end, got {other_lo}..{other_hi} vs {sub_lo}..{sub_hi}"
        );
    }

    #[test]
    fn clock_fit_needs_two_accepted_diffs() {
        let fit = fit_clock(&[(0usize, 0.0), (1, 1000.0)], 1000.0, 100.0, None);
        assert!(!fit.valid, "one interval must not produce a clock estimate");
        assert_eq!(fit.accepted_diffs.len(), 1);
        assert!(fit.insufficient_valid_passes == false); // 两点都可用，只是 ppm 不估
        let three = fit_clock(
            &[(0usize, 0.0), (1, 1000.0), (2, 2000.05)],
            1000.0,
            100.0,
            None,
        );
        assert!(three.valid, "ppm={}", three.ppm);
        assert!((three.ppm - 25.0).abs() < 2.0, "ppm={}", three.ppm);
    }

    #[test]
    fn clock_fit_drops_gross_outlier() {
        let pts = [(0usize, 0.0), (1, 1000.0), (2, 2000.0), (3, 9000.0)];
        let fit = fit_clock(&pts, 1000.0, 100.0, None);
        assert!(fit.valid, "ppm={} rms={}", fit.ppm, fit.residual_rms);
        assert!(fit.point_invalid[3]);
        assert!(!fit.point_invalid[0]);
        assert!((fit.slope - 1000.0).abs() < 1.0, "slope={}", fit.slope);
    }

    /// FR 事故回归：pass1 被 full-band snap 拉晚 +706，另两遍正常。
    /// 期望：只标 pass1 invalid，ppm 不估（仅 1 个 accepted 差），pass2/pass3 可用。
    #[test]
    fn clock_fit_marks_only_bad_first_pass() {
        let pts = [(0usize, 11152.47), (1, 260046.81), (2, 509646.07)];
        let fit = fit_clock(&pts, 249_600.0, 100.0, None);
        assert!(!fit.valid);
        assert_eq!(fit.point_invalid, vec![true, false, false]);
        assert_eq!(fit.accepted_diffs.len(), 1);
        assert_eq!(fit.rejected_diffs.len(), 1);
        assert!(
            (fit.spacing - 249_599.26).abs() < 0.1,
            "spacing={}",
            fit.spacing
        );
        assert!((fit.anchor - 10447.55).abs() < 0.1, "anchor={}", fit.anchor);
        assert!(
            (fit.residuals[0] - 704.92).abs() < 0.1,
            "r0={}",
            fit.residuals[0]
        );
        assert!(fit.residuals[1].abs() < 0.1 && fit.residuals[2].abs() < 0.1);
        assert!(!fit.insufficient_valid_passes);
        // delay_rel = median{o[k]-k*spacing | !invalid}，约 10448。
        let rels: Vec<f64> = pts
            .iter()
            .enumerate()
            .filter(|(i, _)| !fit.point_invalid[*i])
            .map(|(_, (k, y))| y - *k as f64 * fit.spacing)
            .collect();
        assert!(
            (median_f64(&rels) - 10448.0).abs() < 0.6,
            "delay_rel={rels:?}"
        );
    }

    /// §3 表：带通峰与 snap 峰分别送进新 fit_clock，ppm 必须落在报告数字上。
    #[test]
    fn clock_fit_matches_fr_opt_acceptance_table() {
        let i = 249_600.0;
        let cases = [
            (
                "FL snap",
                [(0usize, 11115.0), (1, 260701.0), (2, 510284.0)],
                -62.10,
                true,
            ),
            (
                "FL band",
                [(0, 10445.47), (1, 260045.26), (2, 509645.16)],
                -0.62,
                true,
            ),
            (
                "FR band",
                [(0, 10445.82), (1, 260046.08), (2, 509646.49)],
                1.34,
                true,
            ),
            (
                "SUB snap",
                [(0, 11185.0), (1, 260784.0), (2, 510384.0)],
                -2.00,
                true,
            ),
            (
                "SUB band",
                [(0, 11178.45), (1, 260777.11), (2, 510377.18)],
                -2.55,
                true,
            ),
        ];
        for (name, pts, ppm, valid) in cases {
            let fit = fit_clock(&pts, i, 100.0, None);
            assert_eq!(fit.valid, valid, "{name} valid ppm={}", fit.ppm);
            assert!(
                fit.point_invalid.iter().all(|v| !v),
                "{name} invalid={:?}",
                fit.point_invalid
            );
            assert!(
                (fit.ppm - ppm).abs() < 0.05,
                "{name} ppm={} expect {ppm}",
                fit.ppm
            );
        }
    }

    /// 只有 2 遍且坏 1 遍：没有冗余，必须用单遍 actual_start 作 anchor_hint。
    #[test]
    fn clock_fit_anchor_hint_resolves_two_pass_one_bad() {
        let pts = [(0usize, 11152.0), (1, 260046.0)];
        let no_hint = fit_clock(&pts, 249_600.0, 100.0, None);
        assert!(!no_hint.valid);
        let hinted = fit_clock(&pts, 249_600.0, 100.0, Some(10445.0));
        assert_eq!(hinted.point_invalid, vec![true, false]);
        assert!((hinted.anchor - 10445.0).abs() < 0.1);
    }

    /// A1：REF 型“ppm 与不确定度同量级”必须标 not significant（不得当漂移证据）。
    #[test]
    fn clock_fit_significance_rejects_noise_scale_ppm() {
        let i = 249_600.0f64;
        let ppm = 12.82;
        let d = i * ppm / 1e6;
        // 3 点跨度 2*I=10.4 s；单遍反射造成 ±6.4 样本的交替残差。
        let pts = [
            (0usize, 10_445.0),
            (1, 10_445.0 + i + d + 6.4),
            (2, 10_445.0 + 2.0 * (i + d)),
        ];
        let fit = fit_clock(&pts, i, 100.0, None);
        assert!(fit.valid, "ppm={} unc={}", fit.ppm, fit.uncertainty_ppm);
        assert!((fit.ppm - ppm).abs() < 2.0, "ppm={}", fit.ppm);
        assert!(
            !fit.significant,
            "ppm={:.2} unc={:.2} 2σ={:.2} must not be significant",
            fit.ppm,
            fit.uncertainty_ppm,
            2.0 * fit.uncertainty_ppm
        );
        // 真实 80 ppm（残差≈0）必须 significant。
        let real = [
            (0usize, 10_445.0),
            (1, 10_445.0 + i * 1.00008),
            (2, 10_445.0 + 2.0 * i * 1.00008),
        ];
        let fit2 = fit_clock(&real, i, 100.0, None);
        assert!(
            fit2.significant,
            "ppm={} unc={}",
            fit2.ppm, fit2.uncertainty_ppm
        );
    }

    /// 4 遍、末遍坏：仍应估出真实 +80 ppm，只标末遍 invalid。
    #[test]
    fn clock_fit_four_passes_last_bad_keeps_ppm() {
        let i = 249_600.0f64;
        let d = i * (1.0 + 80e-6);
        let pts = [
            (0usize, 10_445.0),
            (1, 10_445.0 + d),
            (2, 10_445.0 + 2.0 * d),
            (3, 10_445.0 + 3.0 * d + 700.0),
        ];
        let fit = fit_clock(&pts, i, 100.0, None);
        assert_eq!(fit.point_invalid, vec![false, false, false, true]);
        assert!(fit.valid, "ppm={}", fit.ppm);
        assert!((fit.ppm - 80.0).abs() < 2.0, "ppm={}", fit.ppm);
        assert_eq!(fit.accepted_diffs.len(), 2);
    }

    #[test]
    fn fractional_delay_is_finite() {
        let fs = 48_000u32;
        let src = sweep_like(24_000, fs as f64);
        let mut path = vec![0.0f64; 32];
        path[11] = 1.0;
        let captured = convolve(&src, &path);
        let r = analyze_capture(&captured, &src, fs, 20.0, 18_000.0, 0, None).unwrap();
        assert!(r.delay_samples.is_finite());
    }
}
