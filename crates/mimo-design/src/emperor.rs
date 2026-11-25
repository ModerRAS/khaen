//! 皇帝位单麦 focus 求解（离线）。
//!
//! 侧路系数是加在直通延迟上的修正：`p = h_p ∗ (δ_D + g_pp) + Σ h_s ∗ g_sp`。
//! `g` 里不再放一个 `δ_D`。声学复测不在本模块里宣称通过。

use crate::fir::fft_in_place;

/// 缺省与设计文档第 2、5 节一致。
#[derive(Clone, Debug)]
pub struct EmperorParams {
    /// 低速率，播放侧路是 12 kHz。
    pub fs: f64,
    /// 支援频段上沿，缺省 150，大于 150 拒绝。
    pub f_hi: f64,
    /// 低于此频率的基不加。缺省 0，从直流算起。
    pub f_lo: f64,
    /// 主峰之后保留的时间（秒）。
    pub t_keep_s: f64,
    /// 保留时间之后的误差权重。
    pub w_tail: f64,
    /// 从喇叭惩罚，按该喇叭能量归一。
    pub lambda0: f64,
    /// 12 kHz 预延迟，对应直通在低频模型里的 `δ_D`。
    pub d12: usize,
    /// 48 kHz 块长，用来算分区数。
    pub block48: usize,
    /// 当前侧路抽头上限。超过则 `engine_playable = false`，系数仍可给离线渲染。
    pub max_taps: usize,
    /// 分区上限。
    pub max_partitions: usize,
    /// 全局预衰减的下限（dB）。正增益配等量的负预衰减；再低就拒绝，不削系数。
    pub pre_gain_floor_db: f64,
    /// 抽取低通抽头（奇数）。
    pub resamp_taps: usize,
}

impl Default for EmperorParams {
    fn default() -> Self {
        Self {
            fs: 12_000.0,
            f_hi: 150.0,
            f_lo: 0.0,
            t_keep_s: 0.020,
            w_tail: 10.0,
            lambda0: 0.01,
            d12: 120,
            block48: 128,
            max_taps: 24_000,
            max_partitions: 1500,
            pre_gain_floor_db: -20.0,
            resamp_taps: 95,
        }
    }
}

/// 衰减曲线能看到的深度。只决定复测看 T20 还是 T30，不决定抽头。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecayMetric {
    /// 曲线至少落到 −35 dB。
    T30,
    /// 曲线落到 −25 dB，但不到 −35 dB。
    T20,
}

/// 一次声道设计。`g_primary` 是修正，不是 `δ_D + g`。
#[derive(Clone, Debug)]
pub struct EmperorDesign {
    /// 预定的复测指标。
    pub metric: DecayMetric,
    /// 主峰到噪声底的时间（秒）。
    pub tail_s: f64,
    /// 能量衰减曲线从主峰落下的 dB。
    pub descent_db: f64,
    /// 抽头数。
    pub taps: usize,
    /// 分区数。
    pub partitions: usize,
    /// 当前 `MIMO12K_MAX_TAPS` / 分区上限能不能播。
    pub engine_playable: bool,
    /// 可测尾巴长于本次抽头能盖住的跨度。
    pub tail_truncated: bool,
    /// 主喇叭修正，长度 `taps`。
    pub g_primary: Vec<f64>,
    /// 从喇叭修正，顺序与输入一致。
    pub g_support: Vec<Vec<f64>>,
    /// 控制频段窗内预测：尾巴能量 / 未修正尾巴能量。不是声学通过。
    pub predicted_tail_ratio: f64,
    /// 听感延迟，48 kHz 样本。
    pub delta_sys_48: u32,
    /// 这条设计自己需要的预衰减（dB）。一套音箱播放时要再合成一个共用值。
    pub pre_gain_db: f64,
}

/// 一套音箱共用一个预衰减，等于其中最高正增益的相反数。
/// 每一只音箱都乘这个数，相对声压不变。任一路径超过下限则整套拒绝，不削系数。
pub fn shared_pre_gain_db(peak_db: &[f64], floor_db: f64) -> Result<f64, String> {
    let Some(hottest) = peak_db.iter().copied().reduce(f64::max) else {
        return Ok(0.0);
    };
    pre_gain_db(hottest, floor_db)
}

/// 正增益配等量的全局预衰减。没有正增益时为 0。超过 `-floor_db` 则拒绝，不削系数。
pub fn pre_gain_db(peak_db: f64, floor_db: f64) -> Result<f64, String> {
    if !peak_db.is_finite() || !floor_db.is_finite() || floor_db > 0.0 {
        return Err("预衰减参数不合法".to_string());
    }
    if peak_db <= 0.0 {
        return Ok(0.0);
    }
    let limit = -floor_db;
    if peak_db > limit + 1e-4 {
        return Err(format!(
            "预测增益 {peak_db:.2} dB 超过 +{limit:.1} dB，拒绝而不是削顶。全局预衰减最低要到 {floor_db:.1} dB，盖不住这次增益"
        ));
    }
    Ok(-peak_db)
}

/// 与 `mimo12k` 相同的对称低通：48 kHz 上截止 6 kHz，DC 增益 1。
pub fn decim_lowpass(taps: usize) -> Result<Vec<f64>, String> {
    if taps < 3 || taps.is_multiple_of(2) || taps > 255 {
        return Err(format!("抽取低通抽头 {taps} 必须是 3..=255 的奇数"));
    }
    fir_lowpass(taps, 0.125)
}

/// 对称窗函数低通。`cutoff` 是相对采样率的归一化频率，DC 增益 1。
fn fir_lowpass(taps: usize, cutoff: f64) -> Result<Vec<f64>, String> {
    if taps < 3 || taps.is_multiple_of(2) || taps > 4095 {
        return Err(format!("低通抽头 {taps} 必须是 3..=4095 的奇数"));
    }
    if !(0.0 < cutoff && cutoff < 0.5) {
        return Err(format!("低通截止 {cutoff} 不在 (0, 0.5)"));
    }
    let mid = (taps - 1) as f64 / 2.0;
    let mut raw = vec![0.0; taps];
    let mut sum = 0.0;
    for (k, slot) in raw.iter_mut().enumerate() {
        let t = k as f64 - mid;
        let x = std::f64::consts::TAU * cutoff * t;
        let sinc = if x.abs() < 1e-12 { 1.0 } else { x.sin() / x };
        let angle = std::f64::consts::TAU * k as f64 / (taps - 1) as f64;
        let window = 0.42 - 0.5 * angle.cos() + 0.08 * (2.0 * angle).cos();
        *slot = 2.0 * cutoff * sinc * window;
        sum += *slot;
    }
    for v in &mut raw {
        *v /= sum;
    }
    Ok(raw)
}

/// 代价和尾巴能量只留下下沿到上沿。零相位带通把脉冲放在缓冲中间，
/// 振铃留在两侧空白里，不折进尾巴。
fn limit_to_control_band(x: &[f64], fs: f64, f_lo: f64, f_hi: f64) -> Result<Vec<f64>, String> {
    if x.is_empty() || fs <= 0.0 || f_hi <= f_lo.max(0.0) {
        return Err("控制频段为空".to_string());
    }
    if f_lo < 20.0 {
        let filtered = conv(&fir_lowpass(191, (f_hi / fs).clamp(0.001, 0.45))?, x);
        if filtered.is_empty() {
            return Err("控制频段滤波后没有样本".to_string());
        }
        return Ok(filtered);
    }
    let tw = ((f_hi - f_lo) / 4.0).clamp(8.0, 20.0);
    let pad = (fs.round() as usize).max(8_192);
    let n = (x.len() + pad.saturating_mul(2)).next_power_of_two();
    let start = (n - x.len()) / 2;
    let mut re = vec![0.0; n];
    let mut im = vec![0.0; n];
    re[start..start + x.len()].copy_from_slice(x);
    fft_in_place(&mut re, &mut im);
    for k in 0..n {
        let bin = if k <= n / 2 { k } else { n - k };
        let freq = bin as f64 * fs / n as f64;
        let gain = control_band_gain(freq, f_lo, f_hi, tw);
        re[k] *= gain;
        im[k] *= gain;
    }
    ifft(&mut re, &mut im);
    Ok(re[start..start + x.len()].to_vec())
}

fn control_band_gain(freq: f64, f_lo: f64, f_hi: f64, tw: f64) -> f64 {
    let rise = if freq <= f_lo - tw {
        0.0
    } else if freq >= f_lo {
        1.0
    } else {
        let t = (freq - (f_lo - tw)) / tw;
        0.5 - 0.5 * (std::f64::consts::PI * t).cos()
    };
    let fall = if freq <= f_hi {
        1.0
    } else if freq >= f_hi + tw {
        0.0
    } else {
        let t = (freq - f_hi) / tw;
        0.5 + 0.5 * (std::f64::consts::PI * t).cos()
    };
    rise * fall
}

/// 低频峰相对这段脉冲开头的样本。开头应是该音箱自己的直达声。
pub fn bass_arrival_sample(x: &[f64], fs: f64) -> Option<usize> {
    if x.len() < 32 || fs <= 0.0 {
        return None;
    }
    let hi = 150.0_f64.min(fs * 0.45);
    if hi <= 40.0 {
        return None;
    }
    let band = limit_to_control_band(x, fs, 40.0, hi).ok()?;
    Some(
        band.iter()
            .enumerate()
            .max_by(|a, b| {
                a.1.abs()
                    .partial_cmp(&b.1.abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(index, _)| index)
            .unwrap_or(0),
    )
}

/// 把若干到达样本对齐到最晚的那个。音箱之间的播放延迟用路程差，不用这里的低频建立时间。
pub fn bass_align_delays(arrivals: &[usize]) -> Vec<usize> {
    let Some(latest) = arrivals.iter().copied().max() else {
        return Vec::new();
    };
    arrivals.iter().map(|sample| latest - sample).collect()
}

/// 相对 200–400 Hz 的 1/3 oct 中位数，从低频往上第一次进入 −3 dB 的频率。
pub fn low_extension_hz(ir: &[f64], fs: f64) -> Option<f64> {
    if ir.len() < 32 || fs <= 0.0 {
        return None;
    }
    let n = ir.len().next_power_of_two().max(2);
    let mut re = vec![0.0; n];
    let mut im = vec![0.0; n];
    re[..ir.len()].copy_from_slice(ir);
    fft_in_place(&mut re, &mut im);
    let power: Vec<f64> = re.iter().zip(&im).map(|(a, b)| a * a + b * b).collect();
    let third = |hz: f64| -> f64 {
        let lo = ((hz / 2f64.powf(1.0 / 6.0) / fs) * n as f64).round() as usize;
        let hi = ((hz * 2f64.powf(1.0 / 6.0) / fs) * n as f64).round() as usize;
        let lo = lo.max(1);
        let hi = hi.min(n / 2);
        if hi <= lo {
            return -200.0;
        }
        let mean = power[lo..=hi].iter().sum::<f64>() / (hi - lo + 1) as f64;
        10.0 * mean.max(1e-30).log10()
    };
    let mut refs: Vec<f64> = Vec::new();
    let mut f = 200.0;
    while f <= 400.0 {
        refs.push(third(f));
        f += 2.0;
    }
    if refs.is_empty() {
        return None;
    }
    refs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let reference = refs[refs.len() / 2];
    let mut hz = 20.0;
    while hz <= 200.0 {
        if third(hz) >= reference - 3.0 {
            return Some(hz);
        }
        hz += 1.0;
    }
    None
}

/// 48 kHz → 12 kHz。先过 `decim_lowpass`，再每 4 点取 1 点。
pub fn decimate_48_to_12(x: &[f64], resamp_taps: usize) -> Result<Vec<f64>, String> {
    let h = decim_lowpass(resamp_taps)?;
    let y = conv(&h, x);
    let delay = (resamp_taps - 1) / 2;
    let mut out = Vec::new();
    let mut n = delay;
    while n < y.len() {
        out.push(y[n]);
        n += 4;
    }
    if out.is_empty() {
        return Err("抽取后没有样本".to_string());
    }
    Ok(out)
}

/// 听感延迟：两段重采样群延迟 + `4·D12`，单位是 48 kHz 样本。
pub fn delta_sys_48(resamp_taps: usize, d12: usize) -> u32 {
    (resamp_taps - 1) as u32 + 4 * d12 as u32
}

/// 座位对消半径，按本次上沿的 λ/10 估计。声速 343 m/s。
pub fn seat_radius_m(f_hi: f64) -> f64 {
    343.0 / (10.0 * f_hi)
}

/// 旧幅度填谷系数不能标成皇帝位衰减。
pub fn refuse_legacy_fill_sha(sha: &str) -> Result<(), String> {
    let hex = sha.trim().trim_start_matches("0x").to_ascii_lowercase();
    if hex.starts_with("b56b3426") {
        return Err("系数 sha b56b3426… 是幅度填谷，拒绝标成皇帝位衰减设计".to_string());
    }
    Ok(())
}

/// 由 12 kHz 主喇叭脉冲响应求解。`supports` 为空则只修正主喇叭自己的尾巴。
pub fn design_channel(
    primary: &[f64],
    supports: &[Vec<f64>],
    params: &EmperorParams,
) -> Result<EmperorDesign, String> {
    if params.f_hi > 150.0 + 1e-9 {
        return Err(format!(
            "支援频段上沿 {:.3} Hz 高于 150 Hz，拒绝",
            params.f_hi
        ));
    }
    if params.f_hi < 20.0 || params.fs <= 0.0 || params.t_keep_s < 0.0 || params.w_tail < 1.0 {
        return Err("求解参数不合法".to_string());
    }
    if primary.iter().any(|v| !v.is_finite()) {
        return Err("主喇叭脉冲响应含非有限值".to_string());
    }
    for support in supports {
        if support.len() != primary.len() {
            return Err("从喇叭与主喇叭的脉冲响应长度不一致".to_string());
        }
    }
    let primary_band = limit_to_control_band(primary, params.fs, params.f_lo, params.f_hi)?;
    let mut support_band = Vec::with_capacity(supports.len());
    for support in supports {
        support_band.push(limit_to_control_band(
            support,
            params.fs,
            params.f_lo,
            params.f_hi,
        )?);
    }
    let class = classify_decay(&primary_band, params.fs)?;
    let taps = params.d12 + ((class.tail_s * params.fs).ceil() as usize).max(8);
    let low_block = params.block48 / 4;
    if low_block == 0 {
        return Err("块长不能被 4 整除".to_string());
    }
    let partitions = taps.div_ceil(low_block);
    let engine_playable = taps <= params.max_taps && partitions <= params.max_partitions;

    let solved = solve_correction(&primary_band, &support_band, taps, params)?;
    let mut hottest = peak_gain_db(&solved.g_primary);
    for g in &solved.g_support {
        hottest = hottest.max(peak_gain_db(g));
    }
    let global_pre = pre_gain_db(hottest, params.pre_gain_floor_db)?;
    Ok(EmperorDesign {
        metric: class.metric,
        tail_s: class.tail_s,
        descent_db: class.descent_db,
        taps,
        partitions,
        engine_playable,
        tail_truncated: false,
        g_primary: solved.g_primary,
        g_support: solved.g_support,
        predicted_tail_ratio: solved.tail_ratio,
        delta_sys_48: delta_sys_48(params.resamp_taps, params.d12),
        pre_gain_db: global_pre,
    })
}

fn peak_gain_db(g: &[f64]) -> f64 {
    20.0 * max_dft_gain(g).max(1e-20).log10()
}

struct DecayClass {
    metric: DecayMetric,
    tail_s: f64,
    descent_db: f64,
}

fn classify_decay(x: &[f64], fs: f64) -> Result<DecayClass, String> {
    if x.len() < 8 {
        return Err("脉冲响应太短，衰减曲线无法判断".to_string());
    }
    let peak = x
        .iter()
        .enumerate()
        .max_by(|a, b| {
            a.1.abs()
                .partial_cmp(&b.1.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
        .unwrap_or(0);
    let mut rev = vec![0.0; x.len() - peak];
    let mut acc = 0.0;
    for (i, v) in x[peak..].iter().enumerate().rev() {
        acc += v * v;
        rev[i] = acc;
    }
    let e0 = rev[0].max(1e-30);
    let noise_n = (x.len() / 10).max(1);
    let noise_ms = x[x.len() - noise_n..].iter().map(|v| v * v).sum::<f64>() / noise_n as f64;
    let mut knee = rev.len() - 1;
    for (i, e) in rev.iter().enumerate() {
        let remain = (rev.len() - i) as f64;
        if *e <= 2.0 * noise_ms * remain && i > 0 {
            knee = i;
            break;
        }
    }
    let descent_db = -10.0 * (rev[knee].max(1e-30) / e0).log10();
    if !descent_db.is_finite() || descent_db < 25.0 {
        return Err(format!(
            "能量衰减曲线只落下 {:.1} dB，到不了 −25 dB，拒绝这次测量",
            descent_db.max(0.0)
        ));
    }
    let metric = if descent_db >= 35.0 {
        DecayMetric::T30
    } else {
        DecayMetric::T20
    };
    let tail_s = knee as f64 / fs;
    Ok(DecayClass {
        metric,
        tail_s: tail_s.max(params_min_tail(fs)),
        descent_db,
    })
}

fn params_min_tail(fs: f64) -> f64 {
    8.0 / fs
}

struct Solved {
    g_primary: Vec<f64>,
    g_support: Vec<Vec<f64>>,
    tail_ratio: f64,
}

/// 尾巴窗的控制频段谱。单边幅度按能量归一，正频率计两次，使全频和等于加窗后的时域能量。
fn tail_control_spectrum(slice: &[f64], fs: f64, f_lo: f64, f_hi: f64) -> (Vec<f64>, Vec<f64>) {
    let m = slice.len();
    if m < 8 || fs <= 0.0 || f_hi <= f_lo.max(0.0) {
        return (Vec::new(), Vec::new());
    }
    let n = m.next_power_of_two();
    let mut re = vec![0.0; n];
    let mut im = vec![0.0; n];
    let denom = (m - 1) as f64;
    for (index, value) in slice.iter().enumerate() {
        let window = 0.5 - 0.5 * (std::f64::consts::TAU * index as f64 / denom).cos();
        re[index] = value * window;
    }
    fft_in_place(&mut re, &mut im);
    let mut out_re = Vec::new();
    let mut out_im = Vec::new();
    for k in 0..=n / 2 {
        let freq = k as f64 * fs / n as f64;
        if freq + 1e-9 < f_lo || freq > f_hi + 1e-9 {
            continue;
        }
        let scale = if k == 0 || k == n / 2 {
            1.0 / (n as f64).sqrt()
        } else {
            (2.0 / n as f64).sqrt()
        };
        out_re.push(re[k] * scale);
        out_im.push(im[k] * scale);
    }
    (out_re, out_im)
}

fn tail_control_energy(slice: &[f64], fs: f64, f_lo: f64, f_hi: f64) -> f64 {
    let (re, im) = tail_control_spectrum(slice, fs, f_lo, f_hi);
    re.iter().zip(im).map(|(a, b)| a * a + b * b).sum()
}

fn solve_correction(
    primary: &[f64],
    supports: &[Vec<f64>],
    taps: usize,
    params: &EmperorParams,
) -> Result<Solved, String> {
    let n_pos = ((params.f_hi / params.fs) * taps as f64).floor() as usize;
    let basis: Vec<Vec<f64>> = basis_bank(taps, n_pos)
        .into_iter()
        .enumerate()
        .filter(|(index, _)| basis_hz(*index, taps, params.fs) + 1e-9 >= params.f_lo)
        .map(|(_, column)| column)
        .collect();
    let n_basis = basis.len();
    let n_filters = 1 + supports.len();
    let n_unk = n_basis * n_filters;
    if n_basis == 0 || n_unk > 2000 {
        return Err(format!("未知数 {n_unk} 超出求解容量"));
    }
    let mut plants = Vec::with_capacity(n_filters);
    plants.push(primary.to_vec());
    for s in supports {
        if s.len() != primary.len() {
            return Err("从喇叭与主喇叭的脉冲响应长度不一致".to_string());
        }
        plants.push(s.clone());
    }
    let conv_len = primary.len() + taps - 1;
    let mut cols = Vec::with_capacity(n_unk);
    for plant in &plants {
        for b in &basis {
            let mut c = conv(plant, b);
            c.resize(conv_len, 0.0);
            cols.push(c);
        }
    }
    let mut dry = vec![0.0; conv_len];
    if params.d12 + primary.len() <= conv_len {
        dry[params.d12..params.d12 + primary.len()].copy_from_slice(primary);
    } else {
        return Err("预延迟长于滤波器".to_string());
    }
    let peak = dry
        .iter()
        .enumerate()
        .max_by(|a, b| {
            a.1.abs()
                .partial_cmp(&b.1.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
        .unwrap_or(0);
    let keep_end = peak + ((params.t_keep_s * params.fs).round() as usize);
    let tail_end = (peak
        + ((classify_decay(primary, params.fs)?.tail_s * params.fs).round() as usize)
        + params.d12)
        .min(conv_len.saturating_sub(1));
    let mut target = dry.clone();
    for v in target.iter_mut().skip(keep_end) {
        *v = 0.0;
    }
    let head_end = keep_end.min(conv_len.saturating_sub(1));
    let tail_from = head_end.saturating_add(1).min(conv_len);
    let tail_to = tail_end.saturating_add(1).min(conv_len);
    let mut ata = vec![0.0; n_unk * n_unk];
    let mut atb = vec![0.0; n_unk];
    for i in 0..n_unk {
        for j in i..n_unk {
            let mut acc = 0.0;
            for n in 0..=head_end {
                acc += cols[i][n] * cols[j][n];
            }
            ata[i * n_unk + j] = acc;
            ata[j * n_unk + i] = acc;
        }
        let mut acc = 0.0;
        for n in 0..=head_end {
            acc += cols[i][n] * (target[n] - dry[n]);
        }
        atb[i] = acc;
    }
    if tail_to > tail_from {
        let spectra: Vec<(Vec<f64>, Vec<f64>)> = cols
            .iter()
            .map(|column| {
                tail_control_spectrum(
                    &column[tail_from..tail_to],
                    params.fs,
                    params.f_lo,
                    params.f_hi,
                )
            })
            .collect();
        let (dry_re, dry_im) = tail_control_spectrum(
            &dry[tail_from..tail_to],
            params.fs,
            params.f_lo,
            params.f_hi,
        );
        if !dry_re.is_empty() && spectra.iter().all(|(re, _)| re.len() == dry_re.len()) {
            let ww = params.w_tail * params.w_tail;
            for i in 0..n_unk {
                for j in i..n_unk {
                    let mut acc = 0.0;
                    for k in 0..dry_re.len() {
                        acc +=
                            spectra[i].0[k] * spectra[j].0[k] + spectra[i].1[k] * spectra[j].1[k];
                    }
                    ata[i * n_unk + j] += ww * acc;
                    if i != j {
                        ata[j * n_unk + i] += ww * acc;
                    }
                }
                let mut acc = 0.0;
                for k in 0..dry_re.len() {
                    acc += spectra[i].0[k] * (-dry_re[k]) + spectra[i].1[k] * (-dry_im[k]);
                }
                atb[i] += ww * acc;
            }
        }
    }
    // 从喇叭惩罚。主修正不加这项。
    for s in 0..supports.len() {
        let energy = supports[s].iter().map(|v| v * v).sum::<f64>().max(1e-12);
        let scale = params.lambda0 * energy;
        let base = (1 + s) * n_basis;
        for i in 0..n_basis {
            for j in 0..n_basis {
                let gram = basis_inner(&basis[i], &basis[j]);
                ata[(base + i) * n_unk + (base + j)] += scale * gram;
            }
        }
    }
    let ridge = 1e-8;
    for i in 0..n_unk {
        ata[i * n_unk + i] += ridge * ata[i * n_unk + i].abs().max(1.0);
    }
    cholesky_solve(&mut ata, &mut atb, n_unk)?;
    let mut filters = Vec::with_capacity(n_filters);
    for f in 0..n_filters {
        let coeff = &atb[f * n_basis..(f + 1) * n_basis];
        let mut g = vec![0.0; taps];
        for (c, b) in coeff.iter().zip(basis.iter()) {
            for (slot, bv) in g.iter_mut().zip(b.iter()) {
                *slot += c * bv;
            }
        }
        filters.push(g);
    }
    let g_primary = filters.remove(0);
    let mut predicted = dry.clone();
    add_conv(&mut predicted, primary, &g_primary);
    for (h, g) in supports.iter().zip(filters.iter()) {
        add_conv(&mut predicted, h, g);
    }
    let tail_ratio = if tail_to > tail_from {
        let before = tail_control_energy(
            &dry[tail_from..tail_to],
            params.fs,
            params.f_lo,
            params.f_hi,
        );
        let after = tail_control_energy(
            &predicted[tail_from..tail_to],
            params.fs,
            params.f_lo,
            params.f_hi,
        );
        if before <= 1e-20 {
            1.0
        } else {
            after / before
        }
    } else {
        1.0
    };
    Ok(Solved {
        g_primary,
        g_support: filters,
        tail_ratio,
    })
}

fn basis_hz(index: usize, taps: usize, fs: f64) -> f64 {
    if index == 0 || taps == 0 {
        0.0
    } else {
        index.div_ceil(2) as f64 * fs / taps as f64
    }
}

fn basis_bank(taps: usize, n_pos: usize) -> Vec<Vec<f64>> {
    let mut out = Vec::with_capacity(1 + 2 * n_pos);
    out.push(vec![1.0; taps]);
    for k in 1..=n_pos {
        let mut c = vec![0.0; taps];
        let mut s = vec![0.0; taps];
        for n in 0..taps {
            let th = std::f64::consts::TAU * (k * n) as f64 / taps as f64;
            c[n] = th.cos();
            s[n] = th.sin();
        }
        out.push(c);
        out.push(s);
    }
    out
}

fn basis_inner(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn add_conv(dst: &mut [f64], h: &[f64], g: &[f64]) {
    let y = conv(h, g);
    for (d, v) in dst.iter_mut().zip(y.iter()) {
        *d += *v;
    }
}

fn conv(a: &[f64], b: &[f64]) -> Vec<f64> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let n_out = a.len() + b.len() - 1;
    let n = n_out.next_power_of_two();
    let mut ar = vec![0.0; n];
    let mut ai = vec![0.0; n];
    let mut br = vec![0.0; n];
    let mut bi = vec![0.0; n];
    ar[..a.len()].copy_from_slice(a);
    br[..b.len()].copy_from_slice(b);
    fft_in_place(&mut ar, &mut ai);
    fft_in_place(&mut br, &mut bi);
    for i in 0..n {
        let re = ar[i] * br[i] - ai[i] * bi[i];
        let im = ar[i] * bi[i] + ai[i] * br[i];
        ar[i] = re;
        ai[i] = im;
    }
    ifft(&mut ar, &mut ai);
    ar.truncate(n_out);
    ar
}

fn ifft(re: &mut [f64], im: &mut [f64]) {
    for v in im.iter_mut() {
        *v = -*v;
    }
    fft_in_place(re, im);
    let n = re.len() as f64;
    for i in 0..re.len() {
        re[i] /= n;
        im[i] = -im[i] / n;
    }
}

fn max_dft_gain(g: &[f64]) -> f64 {
    let n = g.len().next_power_of_two();
    let mut re = vec![0.0; n];
    let mut im = vec![0.0; n];
    re[..g.len()].copy_from_slice(g);
    fft_in_place(&mut re, &mut im);
    re.iter()
        .zip(im.iter())
        .map(|(a, b)| (a * a + b * b).sqrt())
        .fold(0.0, f64::max)
}

fn cholesky_solve(a: &mut [f64], b: &mut [f64], n: usize) -> Result<(), String> {
    for i in 0..n {
        for j in 0..=i {
            let mut sum = a[i * n + j];
            for k in 0..j {
                sum -= a[i * n + k] * a[j * n + k];
            }
            if i == j {
                if sum <= 1e-18 || !sum.is_finite() {
                    return Err("正规方程不是正定的，拒绝这次设计".to_string());
                }
                a[i * n + j] = sum.sqrt();
            } else {
                a[i * n + j] = sum / a[j * n + j];
            }
        }
    }
    for i in 0..n {
        let mut sum = b[i];
        for k in 0..i {
            sum -= a[i * n + k] * b[k];
        }
        b[i] = sum / a[i * n + i];
    }
    for i in (0..n).rev() {
        let mut sum = b[i];
        for k in i + 1..n {
            sum -= a[k * n + i] * b[k];
        }
        b[i] = sum / a[i * n + i];
    }
    Ok(())
}

/// 把 12 kHz 修正渲染成 48 kHz 多通道。主通道 = 延迟后的干声 + 插值修正。
pub fn render_48k(
    sweep: &[f64],
    primary_g12: &[f64],
    support_g12: &[Vec<f64>],
    params: &EmperorParams,
) -> Result<Vec<Vec<f64>>, String> {
    let low = decimate_48_to_12(sweep, params.resamp_taps)?;
    let h_dec = decim_lowpass(params.resamp_taps)?;
    let mut h_int = h_dec.clone();
    for v in &mut h_int {
        *v *= 4.0;
    }
    let delay = delta_sys_48(params.resamp_taps, params.d12) as usize;
    let mut channels = Vec::with_capacity(1 + support_g12.len());
    let primary_corr = upsample4(&conv(&low, primary_g12), &h_int);
    let mut primary = vec![0.0; sweep.len() + delay + primary_corr.len()];
    for (i, v) in sweep.iter().enumerate() {
        primary[i + delay] += *v;
    }
    for (i, v) in primary_corr.iter().enumerate() {
        if i < primary.len() {
            primary[i] += *v;
        }
    }
    channels.push(primary);
    for g in support_g12 {
        let corr = upsample4(&conv(&low, g), &h_int);
        channels.push(corr);
    }
    let n = channels.iter().map(|c| c.len()).max().unwrap_or(0);
    for c in &mut channels {
        c.resize(n, 0.0);
    }
    Ok(channels)
}

fn upsample4(low: &[f64], interp: &[f64]) -> Vec<f64> {
    let mut stuffed = vec![0.0; low.len().saturating_mul(4)];
    for (i, v) in low.iter().enumerate() {
        stuffed[i * 4] = *v;
    }
    conv(interp, &stuffed)
}

/// 从脉冲响应估计 T20 或 T30 的外推秒数。曲线不够就拒绝。
pub fn score_decay(x: &[f64], fs: f64) -> Result<(DecayMetric, f64), String> {
    let class = classify_decay(x, fs)?;
    let peak = x
        .iter()
        .enumerate()
        .max_by(|a, b| {
            a.1.abs()
                .partial_cmp(&b.1.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
        .unwrap_or(0);
    let mut energy = vec![0.0; x.len() - peak];
    let mut acc = 0.0;
    for (i, v) in x[peak..].iter().enumerate().rev() {
        acc += v * v;
        energy[i] = acc;
    }
    let e0 = energy[0].max(1e-30);
    let db: Vec<f64> = energy
        .iter()
        .map(|e| 10.0 * (e.max(1e-30) / e0).log10())
        .collect();
    let (lo, hi) = if class.metric == DecayMetric::T30 {
        (-5.0, -35.0)
    } else {
        (-5.0, -25.0)
    };
    let mut t_lo = None;
    let mut t_hi = None;
    for (i, v) in db.iter().enumerate() {
        if t_lo.is_none() && *v <= lo {
            t_lo = Some(i as f64 / fs);
        }
        if *v <= hi {
            t_hi = Some(i as f64 / fs);
            break;
        }
    }
    let (Some(a), Some(b)) = (t_lo, t_hi) else {
        return Err("衰减曲线没有同时经过评分区间的两端".to_string());
    };
    if b <= a {
        return Err("衰减区间长度为 0".to_string());
    }
    let span_db = lo - hi;
    let seconds = span_db / (span_db / (b - a)) * (60.0 / span_db);
    Ok((class.metric, seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 仓库外证据根目录：REW 房间冲击响应 / 低段扫频 .f32 不由仓库分发。
    ///
    /// 需要跑这些证据测试时设置 KHAEN_EMPEROR_EVIDENCE_DIR（或 KHAEN_EVIDENCE_DIR）
    /// 指向本地证据目录；未设置或文件缺失时测试跳过，源码不写死仓库外路径。
    fn evidence_root() -> Option<std::path::PathBuf> {
        std::env::var_os("KHAEN_EMPEROR_EVIDENCE_DIR")
            .or_else(|| std::env::var_os("KHAEN_EVIDENCE_DIR"))
            .map(std::path::PathBuf::from)
    }

    fn mode(fs: f64, hz: f64, tau_s: f64, seconds: f64, noise: f64) -> Vec<f64> {
        let n = (fs * seconds) as usize;
        let mut y = vec![0.0; n];
        for (i, v) in y.iter_mut().enumerate() {
            let t = i as f64 / fs;
            *v = (-t / tau_s).exp() * (std::f64::consts::TAU * hz * t).sin();
        }
        let mut s: u32 = 0x1234_5678;
        for (i, v) in y.iter_mut().enumerate() {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            let u = (s >> 8) as f64 / (1u32 << 24) as f64 - 0.5;
            if i as f64 > fs * (seconds - 0.25) {
                *v = noise * u;
            } else {
                *v += noise * 1e-3 * u;
            }
        }
        y
    }

    fn params_fast() -> EmperorParams {
        let mut p = EmperorParams::default();
        p.d12 = 32;
        p
    }

    fn band_peak_db(g: &[f64], fs: f64, f0: f64, f1: f64) -> f64 {
        let n = g.len().next_power_of_two().max(2);
        let mut re = vec![0.0; n];
        let mut im = vec![0.0; n];
        re[..g.len()].copy_from_slice(g);
        fft_in_place(&mut re, &mut im);
        let k0 = ((f0 / fs) * n as f64).ceil() as usize;
        let k1 = (((f1 / fs) * n as f64).floor() as usize).min(n / 2);
        let mut best = 0.0;
        if k0 <= k1 {
            for k in k0..=k1 {
                let mag = (re[k] * re[k] + im[k] * im[k]).sqrt();
                if mag > best {
                    best = mag;
                }
            }
        }
        20.0 * best.max(1e-20).log10()
    }

    fn peak_bin(g: &[f64], fs: f64) -> (f64, f64) {
        let n = g.len().next_power_of_two().max(2);
        let mut re = vec![0.0; n];
        let mut im = vec![0.0; n];
        re[..g.len()].copy_from_slice(g);
        fft_in_place(&mut re, &mut im);
        let mut best_k = 0usize;
        let mut best = 0.0;
        for k in 0..=n / 2 {
            let mag = (re[k] * re[k] + im[k] * im[k]).sqrt();
            if mag > best {
                best = mag;
                best_k = k;
            }
        }
        (best_k as f64 * fs / n as f64, best)
    }

    fn bin_mag(g: &[f64], hz: f64, fs: f64) -> f64 {
        let n = g.len().next_power_of_two();
        let mut re = vec![0.0; n];
        let mut im = vec![0.0; n];
        re[..g.len()].copy_from_slice(g);
        fft_in_place(&mut re, &mut im);
        let k = ((hz / fs) * n as f64) as usize;
        (re[k] * re[k] + im[k] * im[k]).sqrt()
    }

    #[test]
    fn rejects_f_hi_above_150() {
        let mut p = params_fast();
        p.f_hi = 151.0;
        let h = mode(12_000.0, 63.0, 0.08, 0.6, 1e-4);
        let err = design_channel(&h, &[], &p).unwrap_err();
        assert!(err.contains("150"), "{err}");
    }

    #[test]
    fn primary_correction_shortens_tail_and_is_not_a_second_identity() {
        let p = params_fast();
        let h = mode(12_000.0, 63.0, 0.08, 0.8, 1e-5);
        let d = design_channel(&h, &[], &p).unwrap();
        assert!(
            d.predicted_tail_ratio < 0.5,
            "尾巴能量比 {}",
            d.predicted_tail_ratio
        );
        assert!(d.g_primary.iter().all(|v| v.is_finite()));
        let mut ident = vec![0.0; d.taps];
        ident[0] = 1.0;
        let diff: f64 = d
            .g_primary
            .iter()
            .zip(ident.iter())
            .map(|(a, b)| (a - b).abs())
            .sum();
        assert!(diff > 0.5, "导出的主修正不应再是一个单位脉冲");
        assert!(
            (d.g_primary[0] - 1.0).abs() > 0.2,
            "首点 {}",
            d.g_primary[0]
        );
        let gain = max_dft_gain(&d.g_primary);
        let above = bin_mag(&d.g_primary, 400.0, p.fs);
        assert!(
            above < gain * 0.05 + 1e-3,
            "400 Hz 处修正应远小于通带，实际 {above}"
        );
        assert_eq!(d.taps, p.d12 + ((d.tail_s * p.fs).ceil() as usize).max(8));
        assert!(!d.tail_truncated);
    }

    #[test]
    fn solves_at_80_hz_and_stays_below_that_edge() {
        let mut p = params_fast();
        p.f_hi = 80.0;
        let h = mode(12_000.0, 63.0, 0.08, 0.8, 1e-5);
        let d = design_channel(&h, &[], &p).unwrap();
        let gain = max_dft_gain(&d.g_primary);
        let above = bin_mag(&d.g_primary, 200.0, p.fs);
        assert!(above < gain * 0.05 + 1e-3, "200 Hz {above} 通带 {gain}");
        assert!((seat_radius_m(80.0) - 0.429).abs() < 0.01);
    }

    #[test]
    fn support_path_stays_inside_band_and_empty_support_exports_nothing() {
        let p = params_fast();
        let h = mode(12_000.0, 63.0, 0.08, 0.8, 1e-5);
        let mut other = mode(12_000.0, 80.0, 0.09, 0.8, 1e-5);
        other.truncate(h.len());
        other.resize(h.len(), 0.0);
        let d = design_channel(&h, &[other], &p).unwrap();
        assert_eq!(d.g_support.len(), 1);
        assert_eq!(d.g_support[0].len(), d.g_primary.len());
        let gain = max_dft_gain(&d.g_support[0]).max(max_dft_gain(&d.g_primary));
        assert!(bin_mag(&d.g_support[0], 400.0, p.fs) < gain * 0.05 + 1e-3);
        assert!(bin_mag(&d.g_primary, 400.0, p.fs) < gain * 0.05 + 1e-3);
        let alone = design_channel(&h, &[], &p).unwrap();
        assert!(alone.g_support.is_empty());
    }

    #[test]
    fn pre_gain_matches_the_boost_and_stops_at_minus_20() {
        assert!((pre_gain_db(6.0, -20.0).unwrap() + 6.0).abs() < 1e-9);
        assert!((pre_gain_db(14.6, -20.0).unwrap() + 14.6).abs() < 1e-9);
        assert!((pre_gain_db(20.0, -20.0).unwrap() + 20.0).abs() < 1e-6);
        assert_eq!(pre_gain_db(0.0, -20.0).unwrap(), 0.0);
        assert_eq!(pre_gain_db(-4.0, -20.0).unwrap(), 0.0);
        let err = pre_gain_db(20.1, -20.0).unwrap_err();
        assert!(err.contains("削顶") && err.contains("20"), "{err}");
        let err = pre_gain_db(63.0, -20.0).unwrap_err();
        assert!(err.contains("全局预衰减"), "{err}");

        let p = params_fast();
        let h = mode(12_000.0, 63.0, 0.08, 0.8, 1e-5);
        let d = design_channel(&h, &[], &p).unwrap();
        let db = peak_gain_db(&d.g_primary);
        let expect = if db > 0.0 { -db } else { 0.0 };
        assert!(
            (d.pre_gain_db - expect).abs() < 1e-6,
            "增益 {db} 预衰减 {}",
            d.pre_gain_db
        );
        assert!(d.pre_gain_db >= p.pre_gain_floor_db - 1e-6);
        let untouched = peak_gain_db(&d.g_primary);
        assert!((untouched - db).abs() < 1e-9, "预衰减不得改系数");
    }

    #[test]
    fn speakers_share_one_pre_gain() {
        let peaks = [8.6, -1.9, 8.7, 7.0, -2.7, 1.3, 11.0];
        let shared = shared_pre_gain_db(&peaks, -20.0).unwrap();
        assert!((shared + 11.0).abs() < 1e-9, "{shared}");
        let per_speaker: Vec<f64> = peaks
            .iter()
            .map(|peak| if *peak > 0.0 { -peak } else { 0.0 })
            .collect();
        assert!(per_speaker.iter().all(|gain| *gain + 1e-9 >= shared));
        assert!(per_speaker.iter().any(|gain| (*gain - shared).abs() > 1.0));
        let err = shared_pre_gain_db(&[8.6, 20.2, 3.0], -20.0).unwrap_err();
        assert!(err.contains("削顶"), "{err}");
    }

    #[test]
    fn engine_cap_marks_unplayable_without_dropping_the_span() {
        let mut p = params_fast();
        p.max_taps = 64;
        let h = mode(12_000.0, 63.0, 0.12, 1.0, 1e-5);
        let d = design_channel(&h, &[], &p).unwrap();
        assert!(!d.engine_playable);
        assert!(
            d.taps > 64,
            "抽头 {} 不应被 64 点上限截成可播放设计",
            d.taps
        );
    }

    fn stopped_envelope(drop_db: f64) -> Vec<f64> {
        let k = 1600usize;
        let n = 4000usize;
        let amp = 10f64.powf(-drop_db / 20.0);
        let tau = -(k as f64) / amp.ln();
        let mut y = vec![0.0; n];
        let mut s = 1u32;
        for i in 0..n {
            if i < k {
                y[i] = (-(i as f64) / tau).exp();
            } else {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                let u = (s >> 8) as f64 / (1u32 << 24) as f64 - 0.5;
                y[i] = amp * u;
            }
        }
        y
    }

    #[test]
    fn decay_metric_follows_how_far_the_curve_falls() {
        let mid = stopped_envelope(30.0);
        let mid_class = classify_decay(&mid, 12_000.0).unwrap();
        assert_eq!(
            mid_class.metric,
            DecayMetric::T20,
            "落下 {:.1} dB",
            mid_class.descent_db
        );
        assert!((25.0..35.0).contains(&mid_class.descent_db));

        let deep = stopped_envelope(50.0);
        let deep_class = classify_decay(&deep, 12_000.0).unwrap();
        assert_eq!(
            deep_class.metric,
            DecayMetric::T30,
            "落下 {:.1} dB",
            deep_class.descent_db
        );
        let (scored, seconds) = score_decay(&deep, 12_000.0).unwrap();
        assert_eq!(scored, DecayMetric::T30);
        assert!(seconds.is_finite() && seconds > 0.0);
        let from_tail = ((deep_class.tail_s * 12_000.0).ceil() as usize).max(8);
        let from_score = (seconds * 12_000.0).ceil() as usize;
        assert_ne!(from_tail, from_score, "抽头用可测尾巴，不用外推秒数");
    }

    #[test]
    fn listening_delay_does_not_follow_tap_length() {
        assert_eq!(delta_sys_48(95, 120), 574);
        let p = EmperorParams::default();
        let h = mode(12_000.0, 63.0, 0.04, 0.4, 1e-6);
        let d = design_channel(&h, &[], &p).unwrap();
        assert_eq!(d.delta_sys_48, 574);
        assert!(d.taps > 4096 || d.tail_s * p.fs + p.d12 as f64 > 64.0);
        assert_eq!(d.delta_sys_48, delta_sys_48(p.resamp_taps, p.d12));
    }

    #[test]
    fn legacy_fill_sha_is_not_this_design() {
        let err = refuse_legacy_fill_sha(
            "b56b34263c47c692e0ad2823da47e03320884141a79b4b4448941ad5fe10dbb3",
        )
        .unwrap_err();
        assert!(err.contains("填谷"), "{err}");
        assert!(refuse_legacy_fill_sha("aaaaaaaaaaaaaaaa").is_ok());
    }

    #[test]
    fn noisy_ir_is_refused() {
        let p = params_fast();
        let h = mode(12_000.0, 63.0, 0.02, 0.5, 0.5);
        let err = design_channel(&h, &[], &p).unwrap_err();
        assert!(
            err.contains("−25") || err.contains("-25") || err.contains("25"),
            "{err}"
        );
    }

    #[test]
    fn control_band_tail_shrinks_when_hf_energy_dominates() {
        let p = params_fast();
        let mut h = mode(12_000.0, 63.0, 0.12, 0.55, 1e-5);
        let fs = 12_000.0;
        for (i, sample) in h.iter_mut().enumerate() {
            let t = i as f64 / fs;
            if t < 0.30 {
                *sample += 4.0 * (-t / 0.12).exp() * (std::f64::consts::TAU * 2_000.0 * t).sin();
            }
        }
        let d = design_channel(&h, &[], &p).unwrap();
        let db = 10.0 * d.predicted_tail_ratio.max(1e-20).log10();
        assert!(
            d.predicted_tail_ratio < 0.5,
            "控制频段尾巴应短于直通，比值 {} ({db:.1} dB)",
            d.predicted_tail_ratio
        );
        assert!(d.pre_gain_db >= p.pre_gain_floor_db - 1e-6);
    }

    fn read_f32(path: &std::path::Path) -> Vec<f64> {
        let bytes = std::fs::read(path).unwrap_or_default();
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as f64)
            .collect()
    }

    fn window_at_peak(ir: &[f64], keep: usize) -> Vec<f64> {
        let peak = ir
            .iter()
            .enumerate()
            .max_by(|a, b| {
                a.1.abs()
                    .partial_cmp(&b.1.abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
            .unwrap_or(0);
        let end = (peak + keep).min(ir.len());
        ir[peak..end].to_vec()
    }

    /// 用存下来的低段扫频走和主机一样的抽取、开窗、求解。文件不在时跳过。
    /// 导出的正增益配等量全局预衰减；超过 +20 dB 拒绝，不到 −25 dB 也拒绝。
    ///
    /// 证据（7 个低段 .f32 扫频）不随仓库分发；见 evidence_root()。
    #[ignore = "requires repo-external evidence; not shipped"]
    #[test]
    fn saved_low_sweeps_stay_under_the_gain_cap() {
        let Some(root) = evidence_root().map(|root| root.join("emperor-sweeps-20261123")) else {
            eprintln!("skip saved_low_sweeps: KHAEN_EMPEROR_EVIDENCE_DIR 未设置");
            return;
        };
        let names = ["FL", "FR", "SL", "SR", "HFL", "HFR", "SUB1"];
        let params = EmperorParams::default();
        let mut saw = 0;
        let mut failures = Vec::new();
        for name in names {
            let ir48 = read_f32(&root.join(format!("{name}-low-ir48.f32")));
            if ir48.len() < 8 {
                continue;
            }
            saw += 1;
            let peak = ir48.iter().fold(0.0_f64, |acc, v| acc.max(v.abs()));
            let extension = if peak < 1e-4 {
                None
            } else {
                low_extension_hz(&ir48, 48_000.0)
            };
            println!(
                "{name} extension_hz={} peak={:.6}",
                extension
                    .map(|hz| format!("{hz:.0}"))
                    .unwrap_or_else(|| "噪声".to_string()),
                peak
            );
            let decimated = decimate_48_to_12(&ir48, params.resamp_taps).unwrap();
            let primary = window_at_peak(&decimated, 24_000);
            let class = classify_decay(&primary, params.fs);
            let descent = class.as_ref().map(|c| c.descent_db);
            if let Ok(class) = class {
                let taps = params.d12 + ((class.tail_s * params.fs).ceil() as usize).max(8);
                match solve_correction(&primary, &[], taps, &params) {
                    Ok(solved) => {
                        let (hz, mag) = peak_bin(&solved.g_primary, params.fs);
                        let db = 20.0 * mag.max(1e-20).log10();
                        let below50 = band_peak_db(&solved.g_primary, params.fs, 0.0, 50.0);
                        let band50 = band_peak_db(&solved.g_primary, params.fs, 50.0, 150.0);
                        let below74 = band_peak_db(&solved.g_primary, params.fs, 0.0, 74.0);
                        let band74 = band_peak_db(&solved.g_primary, params.fs, 74.0, 150.0);
                        let spots: Vec<String> = [20.0, 40.0, 50.0, 74.0, 100.0, 125.0, 150.0]
                            .into_iter()
                            .map(|f| {
                                format!(
                                    "{f:.0}:{:.1}",
                                    20.0 * bin_mag(&solved.g_primary, f, params.fs)
                                        .max(1e-20)
                                        .log10()
                                )
                            })
                            .collect();
                        println!(
                            "{name} SOLVED descent={:.1} taps={taps} peak={db:.2}dB@{hz:.1}Hz <50={below50:.1} 50-150={band50:.1} <74={below74:.1} 74-150={band74:.1} spots={spots:?}",
                            class.descent_db
                        );
                    }
                    Err(err) => println!("{name} SOLVE_ERR {err}"),
                }
                if let Some(f_lo) = extension {
                    let mut limited = params.clone();
                    limited.f_lo = f_lo;
                    match solve_correction(&primary, &[], taps, &limited) {
                        Ok(solved) => {
                            let (hz, mag) = peak_bin(&solved.g_primary, params.fs);
                            let db = 20.0 * mag.max(1e-20).log10();
                            let below = band_peak_db(&solved.g_primary, params.fs, 0.0, f_lo);
                            let inside = band_peak_db(&solved.g_primary, params.fs, f_lo, 150.0);
                            println!(
                                "{name} LIMITED f_lo={f_lo:.0} peak={db:.1}dB@{hz:.1}Hz below={below:.1} in_band={inside:.1}"
                            );
                        }
                        Err(err) => println!("{name} LIMITED_ERR {err}"),
                    }
                }
            } else {
                println!("{name} NO_SOLVE descent={descent:?} ir12={}", primary.len());
            }
            let mut used = params.clone();
            if let Some(hz) = extension {
                if hz < used.f_hi {
                    used.f_lo = hz;
                }
            }
            match design_channel(&primary, &[], &used) {
                Ok(d) => {
                    let db = peak_gain_db(&d.g_primary);
                    let expect = if db > 0.0 { -db } else { 0.0 };
                    let tail_db = 10.0 * d.predicted_tail_ratio.max(1e-20).log10();
                    println!(
                        "{name} CONTROL f_lo={:.0} descent={:.1} tail={tail_db:.2}dB peak={db:.1} pre={:.1}",
                        used.f_lo, d.descent_db, d.pre_gain_db
                    );
                    if (d.pre_gain_db - expect).abs() > 0.05
                        || d.pre_gain_db < used.pre_gain_floor_db - 0.05
                    {
                        failures.push(format!("{name} 增益 {db:.2} dB 预衰减 {}", d.pre_gain_db));
                    }
                    if d.predicted_tail_ratio >= 1.0 {
                        failures.push(format!("{name} 控制频段尾巴没有变短 {tail_db:.2} dB"));
                    }
                }
                Err(err) if err.contains("削顶") || err.contains("25") => {}
                Err(err) => failures.push(format!("{name}: {err}")),
            }
        }
        if saw == 0 {
            return;
        }
        assert!(failures.is_empty(), "{failures:?}");
    }

    /// 需要仓库外 REW 房间冲击响应（14 个 .f32）；证据不随仓分发。
    #[ignore = "requires repo-external evidence; not shipped"]
    #[test]
    fn raw_r_channel_keeps_the_control_band_cut() {
        let Some(root) = evidence_root().map(|root| root.join("rew-room-20261123")) else {
            eprintln!("skip raw_r_channel: KHAEN_EMPEROR_EVIDENCE_DIR 未设置");
            return;
        };
        for name in [
            "L-full",
            "R-full",
            "SL-full",
            "SR-full",
            "HFL-full-24",
            "HFR-full-24",
            "LFE-full",
            "L-b",
            "R-b",
            "SL-b",
            "SR-b",
            "HFL-b-24",
            "HFR-b-24",
            "LFE-b",
        ] {
            let ir48 = read_f32(&root.join(format!("{name}.f32")));
            if ir48.len() < 8 {
                continue;
            }
            let params = EmperorParams::default();
            let decimated = decimate_48_to_12(&ir48, params.resamp_taps).unwrap();
            let primary = window_at_peak(&decimated, 24_000);
            let mut used = params.clone();
            if let Some(hz) = low_extension_hz(&ir48, 48_000.0) {
                if hz < used.f_hi {
                    used.f_lo = hz;
                }
            }
            let designed = match design_channel(&primary, &[], &used) {
                Ok(design) => design,
                Err(err) => {
                    println!("{name} 拒绝 {err}");
                    continue;
                }
            };
            let band = limit_to_control_band(&primary, used.fs, used.f_lo, used.f_hi).unwrap();
            let on_band = residual_tail_ratio(&band, &designed.g_primary, used.d12);
            let on_raw = residual_tail_ratio(&primary, &designed.g_primary, used.d12);
            let raw_band = limit_to_control_band(&on_raw.1, used.fs, used.f_lo, used.f_hi).unwrap();
            let raw_dry = limit_to_control_band(&on_raw.2, used.fs, used.f_lo, used.f_hi).unwrap();
            let bass_peak = peak_index(&raw_dry);
            let rect = dft_tail_energy(
                &on_raw.1,
                used.fs,
                used.f_lo,
                used.f_hi,
                Some(bass_peak),
                false,
            ) / dft_tail_energy(
                &on_raw.2,
                used.fs,
                used.f_lo,
                used.f_hi,
                Some(bass_peak),
                false,
            )
            .max(1e-30);
            let hann = dft_tail_energy(
                &on_raw.1,
                used.fs,
                used.f_lo,
                used.f_hi,
                Some(bass_peak),
                true,
            ) / dft_tail_energy(
                &on_raw.2,
                used.fs,
                used.f_lo,
                used.f_hi,
                Some(bass_peak),
                true,
            )
            .max(1e-30);
            let filtered = dft_tail_energy(
                &raw_band,
                used.fs,
                used.f_lo,
                used.f_hi,
                Some(bass_peak),
                true,
            ) / dft_tail_energy(
                &raw_dry,
                used.fs,
                used.f_lo,
                used.f_hi,
                Some(bass_peak),
                true,
            )
            .max(1e-30);
            println!(
                "{name} rect {:.2} dB hann {:.2} dB filtered-hann {:.2} dB band-window {:.2} dB predicted {:.2} dB tail {:.3} s",
                10.0 * rect.max(1e-20).log10(),
                10.0 * hann.max(1e-20).log10(),
                10.0 * filtered.max(1e-20).log10(),
                10.0 * on_band.0.max(1e-20).log10(),
                10.0 * designed.predicted_tail_ratio.max(1e-20).log10(),
                designed.tail_s
            );
            if name == "R-full" {
                assert!(hann < 0.5, "右声道控制频段尾巴没有被削短");
            }
        }
    }

    fn residual_tail_ratio(plant: &[f64], g: &[f64], d12: usize) -> (f64, Vec<f64>, Vec<f64>) {
        let conv_len = plant.len() + g.len() - 1;
        let mut dry = vec![0.0; conv_len];
        let end = (d12 + plant.len()).min(conv_len);
        dry[d12..end].copy_from_slice(&plant[..end - d12]);
        let mut predicted = dry.clone();
        let mixed = conv(plant, g);
        for (slot, value) in predicted.iter_mut().zip(mixed.iter()) {
            *slot += *value;
        }
        let peak = dry
            .iter()
            .enumerate()
            .max_by(|a, b| {
                a.1.abs()
                    .partial_cmp(&b.1.abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(index, _)| index)
            .unwrap_or(0);
        let start = (peak + 240).min(conv_len);
        let stop = (start + 6_000).min(conv_len);
        let before = dry[start..stop].iter().map(|v| v * v).sum::<f64>();
        let after = predicted[start..stop].iter().map(|v| v * v).sum::<f64>();
        (after / before.max(1e-30), predicted, dry)
    }

    fn peak_index(x: &[f64]) -> usize {
        x.iter()
            .enumerate()
            .max_by(|a, b| {
                a.1.abs()
                    .partial_cmp(&b.1.abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(index, _)| index)
            .unwrap_or(0)
    }

    fn dft_tail_energy(
        x: &[f64],
        fs: f64,
        f_lo: f64,
        f_hi: f64,
        peak_at: Option<usize>,
        hann: bool,
    ) -> f64 {
        let peak = peak_at.unwrap_or_else(|| peak_index(x));
        let start = (peak + 240).min(x.len());
        let stop = (start + 6_000).min(x.len());
        let slice = &x[start..stop];
        if slice.len() < 64 {
            return 0.0;
        }
        let mut energy = 0.0;
        let mut bin = ((f_lo / fs) * slice.len() as f64).ceil() as usize;
        let last = ((f_hi / fs) * slice.len() as f64).floor() as usize;
        let n = slice.len();
        while bin <= last && bin < n / 2 {
            let mut re = 0.0;
            let mut im = 0.0;
            for (index, value) in slice.iter().enumerate() {
                let window = if hann {
                    0.5 - 0.5 * (std::f64::consts::TAU * index as f64 / (n - 1) as f64).cos()
                } else {
                    1.0
                };
                let phase = std::f64::consts::TAU * bin as f64 * index as f64 / n as f64;
                let shaped = value * window;
                re += shaped * phase.cos();
                im -= shaped * phase.sin();
            }
            energy += re * re + im * im;
            bin += 1;
        }
        energy
    }

    #[test]
    fn bass_align_delays_are_relative_to_latest_peak() {
        assert_eq!(bass_align_delays(&[10, 40, 25]), vec![30, 0, 15]);
        assert_eq!(bass_align_delays(&[7]), vec![0]);
        assert!(bass_align_delays(&[]).is_empty());
    }

    /// 需要仓库外 REW 房间冲击响应（7 个 .f32）；证据不随仓分发。
    #[ignore = "requires repo-external evidence; not shipped"]
    #[test]
    fn bass_align_delays_on_rew_room_ir() {
        let Some(root) = evidence_root().map(|root| root.join("rew-room-20261123")) else {
            eprintln!("skip bass_align_delays: KHAEN_EMPEROR_EVIDENCE_DIR 未设置");
            return;
        };
        let names = [
            "L-full",
            "R-full",
            "SL-full",
            "SR-full",
            "HFL-full-24",
            "HFR-full-24",
            "LFE-full",
        ];
        let mut arrivals = Vec::new();
        for name in names {
            let ir48 = read_f32(&root.join(format!("{name}.f32")));
            if ir48.len() < 32 {
                continue;
            }
            let decimated = decimate_48_to_12(&ir48, EmperorParams::default().resamp_taps).unwrap();
            let primary = window_at_peak(&decimated, 24_000);
            let Some(sample) = bass_arrival_sample(&primary, 12_000.0) else {
                continue;
            };
            arrivals.push(sample);
            println!("{name} bass +{:.1} ms", sample as f64 / 12.0);
        }
        if arrivals.len() == names.len() {
            let buildup = bass_align_delays(&arrivals);
            let buildup_ms = buildup.iter().copied().max().unwrap_or(0) as f64 / 12.0;
            println!("低频建立差 {buildup_ms:.1} ms");
            assert!(
                buildup_ms > 10.0,
                "低频建立差短到几毫秒时，才和座位尺寸同一量级"
            );
        }
    }

    /// 全场 REW 冲击响应。文件不在时跳过。控制频段尾巴要比直通短，带外增益被下潜截掉。
    ///
    /// 证据不随仓库分发，按 evidence_root() 跳过；此前遗留的"known failure"结论在
    /// HANDOFF 2026-11-25（仓库外）记录，本次迁移不改变该测试的断言。
    #[ignore = "requires repo-external evidence; not shipped"]
    #[test]
    fn rew_room_control_band_tail_is_shorter_than_dry() {
        let Some(root) = evidence_root().map(|root| root.join("rew-room-20261123")) else {
            eprintln!("skip rew_room_control_band_tail: KHAEN_EMPEROR_EVIDENCE_DIR 未设置");
            return;
        };
        let names = [
            "L-full",
            "R-full",
            "SL-full",
            "SR-full",
            "HFL-full-24",
            "HFR-full-24",
            "LFE-full",
            "L-b",
            "R-b",
            "SL-b",
            "SR-b",
            "HFL-b-24",
            "HFR-b-24",
            "LFE-b",
        ];
        let params = EmperorParams::default();
        let mut saw = 0;
        let mut shortened = 0;
        let mut failures = Vec::new();
        let mut peaks = [Vec::new(), Vec::new()];
        let mut group_over = [false, false];
        for name in names {
            let ir48 = read_f32(&root.join(format!("{name}.f32")));
            if ir48.len() < 8 {
                continue;
            }
            saw += 1;
            let group = usize::from(saw > 7);
            let extension = low_extension_hz(&ir48, 48_000.0);
            let decimated = decimate_48_to_12(&ir48, params.resamp_taps).unwrap();
            let primary = window_at_peak(&decimated, 24_000);
            let mut used = params.clone();
            if let Some(hz) = extension {
                if hz < used.f_hi {
                    used.f_lo = hz;
                }
            }
            match design_channel(&primary, &[], &used) {
                Ok(d) => {
                    let db = peak_gain_db(&d.g_primary);
                    let tail_db = 10.0 * d.predicted_tail_ratio.max(1e-20).log10();
                    let skirt = (used.f_lo - 10.0).max(0.0);
                    let below = band_peak_db(&d.g_primary, params.fs, 0.0, skirt);
                    println!(
                        "{name} f_lo={:.0} descent={:.1} tail={tail_db:.2}dB peak={db:.1} pre={:.1} below{skirt:.0}={below:.1}",
                        used.f_lo, d.descent_db, d.pre_gain_db
                    );
                    if d.predicted_tail_ratio >= 1.0 {
                        failures.push(format!("{name} 控制频段尾巴没有变短 {tail_db:.2} dB"));
                    } else {
                        shortened += 1;
                    }
                    if db > 20.05 {
                        failures.push(format!("{name} 导出了 {db:.1} dB，超过 +20 dB"));
                    }
                    if used.f_lo > 15.0 && below > 0.0 {
                        failures.push(format!("{name} 下沿 10 Hz 以下仍有 {below:.1} dB"));
                    }
                    peaks[group].push(db);
                }
                Err(err) if err.contains("削顶") => {
                    group_over[group] = true;
                    println!("{name} REFUSED {err}");
                }
                Err(err) if err.contains("25") => {
                    println!("{name} REFUSED {err}");
                }
                Err(err) => failures.push(format!("{name}: {err}")),
            }
        }
        if saw == 0 {
            return;
        }
        for (group, group_peaks) in peaks.iter().enumerate() {
            if group_over[group] {
                failures.push(format!(
                    "第 {} 组有音箱超过 +20 dB，整组一起拒绝",
                    group + 1
                ));
                continue;
            }
            if group_peaks.is_empty() {
                continue;
            }
            match shared_pre_gain_db(group_peaks, params.pre_gain_floor_db) {
                Ok(shared) => {
                    let hottest = group_peaks
                        .iter()
                        .copied()
                        .fold(f64::NEG_INFINITY, f64::max);
                    println!(
                        "GROUP {} shared={shared:.2}dB hottest={hottest:.1}dB n={}",
                        group + 1,
                        group_peaks.len()
                    );
                    let expect = if hottest > 0.0 { -hottest } else { 0.0 };
                    if (shared - expect).abs() > 1e-6 {
                        failures.push(format!(
                            "第 {} 组预衰减 {shared:.2} 没有跟着最高增益 {hottest:.1}",
                            group + 1
                        ));
                    }
                }
                Err(err) => failures.push(format!("第 {} 组: {err}", group + 1)),
            }
        }
        assert!(shortened > 0, "没有任何一条测量在控制频段把尾巴变短");
        assert!(failures.is_empty(), "{failures:?}");
    }

    #[test]
    fn decimate_keeps_a_low_frequency_tone() {
        let fs = 48_000.0;
        let n = 4800;
        let mut x = vec![0.0; n];
        for (i, v) in x.iter_mut().enumerate() {
            *v = (std::f64::consts::TAU * 100.0 * i as f64 / fs).sin();
        }
        let y = decimate_48_to_12(&x, 95).unwrap();
        let mut dots = 0.0;
        let mut energy = 0.0;
        for (i, v) in y.iter().enumerate().skip(40).take(200) {
            let r = (std::f64::consts::TAU * 100.0 * (i * 4) as f64 / fs).sin();
            dots += v * r;
            energy += r * r;
        }
        assert!(dots / energy > 0.2, "抽取后 100 Hz 相关 {}", dots / energy);
    }
}
