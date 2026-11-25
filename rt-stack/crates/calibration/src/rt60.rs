//! [NONRT] RT60 (EDT/T15/T20/T30) + CSD 后处理（goal 6：把瀑布图/混响时间接入测量流水线）。
//!
//! 本模块是 tools/analysis/rt60_waterfall.py v2-20261122 的**纯 Rust 移植**：
//! 同一算法、同一门限、同一 flags/valid/confidence 判定，便于在 khaen-cal job
//! artifact 里离线产出 RT60/CSD，不依赖 Python/numpy（板端也可用）。
//!
//! 算法（与 Python v2 逐项一致；差异只在浮点实现）：
//!  1. 峰值归一化 + 直达声 onset（IR 前 100 ms 绝对峰）；
//!  2. 每倍频带 3 阶 Butterworth 带通（SOS，前后向零相位；两端零填充 >=0.3 s 或
//!     10 个周期以消除窄带启动瞬态）；
//!  3. 25 ms 块能量中值滤波；从 IR 尾部“平坦块能量平台”估计带内噪声底
//!     （25 ms 块、跨度 <=4 dB、>=0.4 s、低于带峰 >=15 dB；取最高的平台，
//!     避免把 FFT 反卷积的数值零地板当噪声）；
//!  4. 衰减门（平滑块能量连续 5 块 <= 噪声+3 dB）→ 截断窗口（默认 900 ms）
//!     → Chu/Lundeby 噪声修正 Schroeder EDC → 最小二乘拟合 EDT/T15/T20/T30；
//!  5. 质量门：EDC 噪声平台 <= -27 dB（T20）、拟合 span/R^2、噪声剩余比；
//!     并用平滑块包络 -5..-25 dB 斜率做交叉校验/回退；
//!  6. 每频带输出 method/rt60/r2/valid/confidence/flags（拒绝编造不可测值）。
//!
//! CSD：21.3 ms Hann STFT（nperseg=1024、noverlap=768、boundary=None），
//! 30 Hz–16 kHz、0–600 ms，dB re 全局峰值（与 Python 工具 plot_csd 相同口径）。
//!
//! 输出为确定性 JSON/CSV 字符串；WAV 读写用于“外部真实 IR 输入 / 反卷积 IR 证据”。
//! 证据纪律：这是**观察量**（报告/前后对比），不是 apply 通过/失败门槛；
//! 离线 fake 引擎产物一律 simulated，真实房间测量仍 NOT_RUN。

use std::path::Path;

use crate::microfft::{rfft, C64};

/// 工具版本（写进 summary.json，与 Python v2 区分）。
pub const RT60_TOOL_VERSION: &str = "rust-v1-20261122";
/// summary schema。
pub const RT60_SCHEMA: &str = "khaen-rt60/v1";
/// CSD schema。
pub const RT60_CSD_SCHEMA: &str = "khaen-rt60-csd/v1";
/// warning schema（RT60 失败但主流程继续）。
pub const RT60_WARNING_SCHEMA: &str = "khaen-rt60-warning/v1";
/// status schema（--no-rt60 或不适用的显式记录）。
pub const RT60_STATUS_SCHEMA: &str = "khaen-rt60-status/v1";

/// 默认倍频带中心频率（与 Python DEFAULT_BANDS 一致；31.5 Hz–16 kHz）。
pub const DEFAULT_BANDS_HZ: [f64; 10] = [
    31.5, 63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];
/// 块能量块长（ms）。
pub const BLOCK_MS: f64 = 25.0;
/// 块能量中值滤波块数。
pub const SMOOTH_BLOCKS: usize = 3;
/// 噪声平台最短时长（s）。
pub const NOISE_MIN_DUR_S: f64 = 0.4;
/// 噪声平台最大跨度（dB）。
pub const NOISE_MAX_SPREAD_DB: f64 = 4.0;
/// 噪声平台至少低于带峰多少 dB。
pub const NOISE_MIN_BELOW_PEAK_DB: f64 = 15.0;
/// 衰减门之后的余量（s）。
pub const GATE_MARGIN_S: f64 = 0.04;
/// 默认分析窗口（ms；sweep 反卷积保留 tail_s = 0.9 s）。
pub const DEFAULT_WINDOW_MS: f64 = 900.0;
/// 默认 CSD 窗口（ms）。
pub const DEFAULT_CSD_WINDOW_MS: f64 = 600.0;

/// RT60 分析配置。
#[derive(Clone, Debug, PartialEq)]
pub struct Rt60Config {
    /// 倍频带中心频率（Hz；升序、互不相同）。
    pub bands_hz: Vec<f64>,
    /// 最大分析窗口（ms，自直达声起）。
    pub window_ms: f64,
    /// CSD 窗口（ms，自直达声起）。
    pub csd_window_ms: f64,
}

impl Default for Rt60Config {
    fn default() -> Self {
        Rt60Config {
            bands_hz: DEFAULT_BANDS_HZ.to_vec(),
            window_ms: DEFAULT_WINDOW_MS,
            csd_window_ms: DEFAULT_CSD_WINDOW_MS,
        }
    }
}

impl Rt60Config {
    /// 严格校验（band 有限且 >0、窗口有限且合理）。
    pub fn validate(&self) -> Result<(), String> {
        if self.bands_hz.is_empty() {
            return Err("rt60: bands_hz must not be empty".to_string());
        }
        for fc in &self.bands_hz {
            if !fc.is_finite() || *fc <= 0.0 {
                return Err(format!("rt60: invalid band center {fc}"));
            }
        }
        if !self.window_ms.is_finite() || !(100.0..=5000.0).contains(&self.window_ms) {
            return Err("rt60: window_ms must be finite and in 100..=5000".to_string());
        }
        if !self.csd_window_ms.is_finite() || !(50.0..=5000.0).contains(&self.csd_window_ms) {
            return Err("rt60: csd_window_ms must be finite and in 50..=5000".to_string());
        }
        Ok(())
    }
}

/// EDC/包络最小二乘拟合结果。
#[derive(Clone, Debug, PartialEq)]
pub struct Rt60DecayFit {
    /// 斜率（dB/s）。
    pub slope_db_s: f64,
    /// 截距（dB）。
    pub intercept_db: f64,
    /// 拟合起点（s）。
    pub t0_s: f64,
    /// 拟合终点（s）。
    pub t1_s: f64,
    /// 拟合跨度（s）。
    pub span_s: f64,
    /// 决定系数。
    pub r2: f64,
    /// RT60 外推（s；斜率非负时 None）。
    pub rt60_s: Option<f64>,
    /// 起点样本下标。
    pub i0: usize,
    /// 终点样本下标。
    pub i1: usize,
}

/// 包络拟合结果（交叉校验用，无 i0/i1）。
#[derive(Clone, Debug, PartialEq)]
pub struct Rt60EnvFit {
    /// 斜率（dB/s）。
    pub slope_db_s: f64,
    /// 拟合起点（s）。
    pub t0_s: f64,
    /// 拟合终点（s）。
    pub t1_s: f64,
    /// 拟合跨度（s）。
    pub span_s: f64,
    /// 决定系数。
    pub r2: f64,
    /// RT60 外推（s；斜率非负时 None）。
    pub rt60_s: Option<f64>,
}

/// 块包络拟合族（env20 / env15）。
#[derive(Clone, Debug, PartialEq)]
pub struct Rt60Envelope {
    /// -5..-25 dB 拟合。
    pub env20: Option<Rt60EnvFit>,
    /// -5..-20 dB 拟合。
    pub env15: Option<Rt60EnvFit>,
    /// 最后一个高于噪声门限的块时间（s）。
    pub last_above_s: f64,
}

/// 单频带 RT60 结果。
#[derive(Clone, Debug, PartialEq)]
pub struct Rt60Band {
    /// 中心频率（Hz）。
    pub center_hz: f64,
    /// 带内噪声底（dB re 带峰）。
    pub noise_floor_db: f64,
    /// 噪声平台起止（s）。
    pub noise_plateau_start_s: f64,
    /// 噪声平台终点（s）。
    pub noise_plateau_end_s: f64,
    /// 噪声平台跨度（dB，p90/p10；无平台时 None）。
    pub noise_spread_db: Option<f64>,
    /// 噪声置信（ok/low）。
    pub noise_confidence: &'static str,
    /// 衰减门位置（s）。
    pub decay_end_s: f64,
    /// 实际 EDC 窗长（s）。
    pub window_s: f64,
    /// EDC 噪声平台（dB re 总能量）。
    pub edc_plateau_db: f64,
    /// EDT 拟合。
    pub edt: Option<Rt60DecayFit>,
    /// T15 拟合。
    pub t15: Option<Rt60DecayFit>,
    /// T20 拟合。
    pub t20: Option<Rt60DecayFit>,
    /// T30 拟合。
    pub t30: Option<Rt60DecayFit>,
    /// 包络交叉校验。
    pub env: Option<Rt60Envelope>,
    /// T20 噪声剩余比。
    pub noise_ratio_t20: Option<f64>,
    /// T15 噪声剩余比。
    pub noise_ratio_t15: Option<f64>,
    /// 选用方法（T20/T15x4/ENV20/ENV15x1.33/EDT_x6）。
    pub method: Option<&'static str>,
    /// 报出的 RT60（s；仅当 method 给出）。
    pub rt60_s: Option<f64>,
    /// 报出方法的 r2。
    pub r2: Option<f64>,
    /// 包络法 RT60（s）。
    pub env_rt60_s: Option<f64>,
    /// 包络法 r2。
    pub env_r2: Option<f64>,
    /// 交叉校验相对差。
    pub cross_check_rel: Option<f64>,
    /// 质量/追溯 flags（分号连接写 CSV）。
    pub flags: Vec<&'static str>,
    /// 是否 valid（可用于报告）。
    pub valid: bool,
    /// 置信度（high/medium/low/none）。
    pub confidence: &'static str,
    /// 带外/不可分析（band_out_of_range）。
    pub band_out_of_range: bool,
}

/// CSD 矩阵（频率 x 时间，dB re 全局峰值）。
#[derive(Clone, Debug, PartialEq)]
pub struct CsdMatrix {
    /// 频率轴（Hz，升序；已按 30–16k 裁剪）。
    pub freqs_hz: Vec<f64>,
    /// 时间轴（ms，自直达声起；scipy stft boundary=None 口径）。
    pub times_ms: Vec<f64>,
    /// 幅值 [频点][时间帧]（dB re 全局峰值）。
    pub db_re_peak: Vec<Vec<f64>>,
    /// STFT 参数：nperseg。
    pub nperseg: usize,
    /// STFT 参数：noverlap。
    pub noverlap: usize,
}

/// 单个 IR 的完整分析结果。
#[derive(Clone, Debug, PartialEq)]
pub struct Rt60IrAnalysis {
    /// 标签（通道/测量点）。
    pub label: String,
    /// 采样率（Hz）。
    pub sample_rate_hz: u32,
    /// 样本数。
    pub samples: usize,
    /// 直达声 onset 样本下标。
    pub onset_sample: usize,
    /// 直达声 onset（ms）。
    pub onset_ms: f64,
    /// 输入峰值（dBFS）。
    pub peak_dbfs: f64,
    /// 每频带结果（与配置 bands 对齐；带外为 band_out_of_range）。
    pub bands: Vec<Rt60Band>,
    /// CSD 数据。
    pub csd: CsdMatrix,
}

// ---------------------------------------------------------------------------
// 数值原语（与 numpy/scipy 口径一致）
// ---------------------------------------------------------------------------

/// 10*log10(max(x, 1e-300))（与 Python db() / numpy 口径一致）。
fn db10(x: f64) -> f64 {
    10.0 * x.max(1e-300).log10()
}

/// 中位数（偶数长度取中间两数均值，与 np.median 一致）。
fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        0.5 * (values[n / 2 - 1] + values[n / 2])
    }
}

/// 3 点中值滤波（边界复制；与 np.pad(edge) + np.median 一致）。
fn medfilt3(x: &[f64]) -> Vec<f64> {
    let n = x.len();
    if n < SMOOTH_BLOCKS {
        return x.to_vec();
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let a = x[i.saturating_sub(1)];
        let b = x[i];
        let c = x[(i + 1).min(n - 1)];
        let mut w = [a, b, c];
        out.push(median(&mut w));
    }
    out
}

/// 线性插值分位数（与 np.percentile 默认 linear 一致）。
fn percentile_linear(values: &[f64], q: f64) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    let pos = (n as f64 - 1.0) * q / 100.0;
    let lo = pos.floor() as usize;
    let hi = (lo + 1).min(n - 1);
    let frac = pos - lo as f64;
    v[lo] * (1.0 - frac) + v[hi] * frac
}

/// 最小二乘直线拟合，返回 (slope, intercept, r2)。
fn linear_fit(xs: &[f64], ys: &[f64]) -> Option<(f64, f64, f64)> {
    let n = xs.len();
    if n < 2 || ys.len() != n {
        return None;
    }
    let mut sx = 0.0;
    let mut sy = 0.0;
    let mut sxx = 0.0;
    let mut sxy = 0.0;
    for i in 0..n {
        sx += xs[i];
        sy += ys[i];
        sxx += xs[i] * xs[i];
        sxy += xs[i] * ys[i];
    }
    let den = n as f64 * sxx - sx * sx;
    if den.abs() < 1e-300 {
        return None;
    }
    let slope = (n as f64 * sxy - sx * sy) / den;
    let intercept = (sy - slope * sx) / n as f64;
    let mean_y = sy / n as f64;
    let mut ss_res = 0.0;
    let mut ss_tot = 0.0;
    for i in 0..n {
        let pred = intercept + slope * xs[i];
        ss_res += (ys[i] - pred) * (ys[i] - pred);
        ss_tot += (ys[i] - mean_y) * (ys[i] - mean_y);
    }
    let r2 = if ss_tot > 0.0 {
        1.0 - ss_res / ss_tot
    } else {
        0.0
    };
    Some((slope, intercept, r2))
}

// ---------------------------------------------------------------------------
// 倍频带 Butterworth 带通（3 阶，SOS，前后向零相位）
// ---------------------------------------------------------------------------

/// 一个二阶节（b0,b1,b2,a1,a2；a0=1）。
#[derive(Clone, Copy, Debug)]
struct SosSection {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

/// 复数 sqrt（主值分支）。
fn csqrt(c: C64) -> C64 {
    let r = c.re.hypot(c.im);
    let re = (0.5 * (r + c.re)).max(0.0).sqrt();
    let im = (0.5 * (r - c.re)).max(0.0).sqrt();
    C64::new(re, if c.im < 0.0 { -im } else { im })
}

impl SosSection {
    /// 前向直接 II 型（转置）滤波；状态 (z1,z2) 连续。
    fn run(&self, x: &[f64], out: &mut [f64]) {
        let mut z1 = 0.0f64;
        let mut z2 = 0.0f64;
        for (i, &v) in x.iter().enumerate() {
            let y = self.b0 * v + z1;
            z1 = self.b1 * v - self.a1 * y + z2;
            z2 = self.b2 * v - self.a2 * y;
            out[i] = y;
        }
    }
}

/// 设计 scipy 口径的 3 阶 Butterworth 带通（[fc/sqrt2, fc*sqrt2]，上限 0.98*Nyquist）。
/// 返回 None 表示该带超出可用范围（与 Python band_signal 的 None 语义一致）。
fn butter_bp_sos(fc: f64, sample_rate: f64) -> Option<Vec<SosSection>> {
    const ORDER: usize = 3;
    let nyq = sample_rate / 2.0 * 0.98;
    let lo = fc / std::f64::consts::SQRT_2;
    let hi = (fc * std::f64::consts::SQRT_2).min(nyq);
    if lo >= nyq * 0.99 {
        return None;
    }
    // scipy 预畸变：Wn 归一化到 Nyquist 后 fs=2，warped = 4*tan(pi*Wn/2) = 4*tan(pi*f/fs)
    let w1 = 4.0 * (std::f64::consts::PI * lo / sample_rate).tan();
    let w2 = 4.0 * (std::f64::consts::PI * hi / sample_rate).tan();
    let wo = (w1 * w2).sqrt();
    let bw = w2 - w1;
    // Butterworth 模拟低通原型极点（单位圆左半平面）
    let mut p_lp = Vec::with_capacity(ORDER);
    for k in 0..ORDER {
        let angle = std::f64::consts::PI * (2.0 * k as f64 + 1.0) / (2.0 * ORDER as f64)
            + std::f64::consts::PI / 2.0;
        p_lp.push(C64::new(angle.cos(), angle.sin()));
    }
    // 低通 -> 带通（scipy lp2bp_zpk）：p_bp = p_lp*bw/2 ± sqrt((p_lp*bw/2)^2 - wo^2)
    let mut p_bp = Vec::with_capacity(ORDER * 2);
    for p in &p_lp {
        let scaled = C64::new(p.re * bw / 2.0, p.im * bw / 2.0);
        let disc = C64::new(
            scaled.re * scaled.re - scaled.im * scaled.im - wo * wo,
            2.0 * scaled.re * scaled.im,
        );
        let root = csqrt(disc);
        p_bp.push(C64::new(scaled.re + root.re, scaled.im + root.im));
        p_bp.push(C64::new(scaled.re - root.re, scaled.im - root.im));
    }
    // 双线性变换（scipy bilinear_zpk，fs2 = 4；LP 的 3 个无穷远零点 -> 原点零点 -> z=+1）
    let fs2 = 4.0f64;
    let mut p_d = Vec::with_capacity(p_bp.len());
    for p in &p_bp {
        let num = C64::new(fs2 + p.re, p.im);
        let den = C64::new(fs2 - p.re, -p.im);
        let den_norm = den.norm_sqr();
        p_d.push(C64::new(
            (num.re * den.re + num.im * den.im) / den_norm,
            (num.im * den.re - num.re * den.im) / den_norm,
        ));
    }
    // 增益：k = bw^N * Re(prod(fs2 - z) / prod(fs2 - p_bp))；LP 的 3 个无穷远零点
    // 在双线性后变 z=-1（已由每节 b=(1,-0,-1) 承担 1 个 +1 与 1 个 -1 零点）。
    let num_prod_re = fs2.powi(ORDER as i32);
    let mut den_prod = C64::new(1.0, 0.0);
    for p in &p_bp {
        let d = C64::new(fs2 - p.re, -p.im);
        den_prod = C64::new(
            den_prod.re * d.re - den_prod.im * d.im,
            den_prod.re * d.im + den_prod.im * d.re,
        );
    }
    let den_norm = den_prod.norm_sqr();
    let k = bw.powi(ORDER as i32) * ((num_prod_re * den_prod.re) / den_norm);
    // 配对：复极点取最近共轭对；实极点两两配对（3 阶带通共 6 极点 = 2 实 + 2 复共轭对）
    let mut sections: Vec<SosSection> = Vec::with_capacity(3);
    let mut complex_poles: Vec<C64> = Vec::new();
    let mut real_poles: Vec<f64> = Vec::new();
    for p in &p_d {
        if p.im.abs() > 1e-9 {
            complex_poles.push(*p);
        } else {
            real_poles.push(p.re);
        }
    }
    let mut used = vec![false; complex_poles.len()];
    for i in 0..complex_poles.len() {
        if used[i] {
            continue;
        }
        let conj = complex_poles[i].conj();
        let mut best = usize::MAX;
        let mut best_dist = f64::INFINITY;
        for j in 0..complex_poles.len() {
            if j == i || used[j] {
                continue;
            }
            let dist = (complex_poles[j].re - conj.re).hypot(complex_poles[j].im - conj.im);
            if dist < best_dist {
                best_dist = dist;
                best = j;
            }
        }
        if best == usize::MAX || best_dist > 1e-6 {
            return None;
        }
        used[i] = true;
        used[best] = true;
        let a = complex_poles[i];
        let b = complex_poles[best];
        sections.push(SosSection {
            b0: 1.0,
            b1: 0.0,
            b2: -1.0,
            a1: -(a.re + b.re),
            a2: (a.re * b.re - a.im * b.im),
        });
    }
    if real_poles.len() % 2 != 0 {
        return None;
    }
    for chunk in real_poles.chunks(2) {
        let a = chunk[0];
        let b = chunk[1];
        sections.push(SosSection {
            b0: 1.0,
            b1: 0.0,
            b2: -1.0,
            a1: -(a + b),
            a2: a * b,
        });
    }
    if sections.is_empty() {
        return None;
    }
    // 增益均摊到各节（整体传递函数不变，动态范围可控）
    let section_count = sections.len() as f64;
    let kc = k.abs().powf(1.0 / section_count);
    for section in &mut sections {
        section.b0 *= kc;
        section.b2 *= kc;
    }
    Some(sections)
}

/// 零相位带通（前向-反向滤波；等价于 scipy.sosfiltfilt 在零填充信号上的行为）。
fn band_signal(x: &[f64], fc: f64, sample_rate: f64) -> Option<Vec<f64>> {
    let sections = butter_bp_sos(fc, sample_rate)?;
    // 两端零填充：>=0.3 s 或 10 个周期（Python band_signal 同口径）
    let pad_frames = (10.0 / fc).max(0.3).min(1.0);
    let pad = (pad_frames * sample_rate) as usize;
    let padded_len = x.len() + 2 * pad;
    let mut buf = vec![0.0f64; padded_len];
    buf[pad..pad + x.len()].copy_from_slice(x);
    let mut tmp = vec![0.0f64; padded_len];
    for section in &sections {
        section.run(&buf, &mut tmp);
        buf.copy_from_slice(&tmp);
    }
    // 反向再滤一遍
    buf.reverse();
    for section in &sections {
        section.run(&buf, &mut tmp);
        buf.copy_from_slice(&tmp);
    }
    buf.reverse();
    Some(buf[pad..pad + x.len()].to_vec())
}

/// 25 ms 块平均功率。
fn block_power(y: &[f64], sample_rate: f64) -> (Vec<f64>, f64) {
    let blk = ((sample_rate * BLOCK_MS / 1000.0) as usize).max(1);
    let n = y.len() / blk;
    let dt = blk as f64 / sample_rate;
    if n < 4 {
        return (Vec::new(), dt);
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let mut sum = 0.0;
        for j in 0..blk {
            let v = y[i * blk + j];
            sum += v * v;
        }
        out.push(sum / blk as f64);
    }
    (out, dt)
}

// ---------------------------------------------------------------------------
// 噪声平台 / 衰减门 / Schroeder EDC / 拟合
// ---------------------------------------------------------------------------

/// 带内噪声平台：返回 (起块, 止块, 电平 dB, 时长 s)。与 Python find_noise_plateau 一致。
fn find_noise_plateau(pblock: &[f64], dt: f64, peak_db: f64) -> Option<(usize, usize, f64, f64)> {
    let n = pblock.len();
    if n < 8 {
        return None;
    }
    let dbv: Vec<f64> = pblock.iter().map(|p| db10(*p)).collect();
    let sm = medfilt3(&dbv);
    let win = ((NOISE_MIN_DUR_S / dt) as usize).max(4);
    let mut runs: Vec<(usize, usize, f64, f64)> = Vec::new();
    let mut i = 0usize;
    while i + win <= n {
        let seg = &sm[i..i + win];
        let (mut lo, mut hi) = (seg[0], seg[0]);
        for v in seg {
            lo = lo.min(*v);
            hi = hi.max(*v);
        }
        let mut med_buf = seg.to_vec();
        let med = median(&mut med_buf);
        if hi - lo <= NOISE_MAX_SPREAD_DB && med <= peak_db - NOISE_MIN_BELOW_PEAK_DB {
            let mut j = i + win;
            while j < n {
                let s2 = &sm[i..j + 1];
                let (mut lo2, mut hi2) = (s2[0], s2[0]);
                for v in s2 {
                    lo2 = lo2.min(*v);
                    hi2 = hi2.max(*v);
                }
                if hi2 - lo2 > NOISE_MAX_SPREAD_DB {
                    break;
                }
                j += 1;
            }
            let mut run_buf = sm[i..j].to_vec();
            let level = median(&mut run_buf);
            runs.push((i, j - 1, level, (j - i) as f64 * dt));
            i = j;
        } else {
            i += 1;
        }
    }
    if runs.is_empty() {
        return None;
    }
    let long: Vec<(usize, usize, f64, f64)> = {
        let filtered: Vec<(usize, usize, f64, f64)> = runs
            .iter()
            .copied()
            .filter(|r| r.3 >= NOISE_MIN_DUR_S)
            .collect();
        if filtered.is_empty() {
            runs.clone()
        } else {
            filtered
        }
    };
    let mut best = long[0];
    for run in long.iter().skip(1) {
        if run.2 > best.2 {
            best = *run;
        }
    }
    Some(best)
}

/// 衰减门：平滑块能量连续 5 块 <= 噪声+3 dB 的首个块（Python decay_gate_index）。
fn decay_gate_index(pblock: &[f64], nf_db: f64, peak_idx: usize) -> Option<usize> {
    let n = pblock.len();
    if n < 6 {
        return None;
    }
    let sm: Vec<f64> = medfilt3(pblock).iter().map(|p| db10(*p)).collect();
    let gate = nf_db + 3.0;
    let start = (peak_idx + 1).max(1);
    if start >= n.saturating_sub(4) {
        return None;
    }
    for i in start..n - 4 {
        if (0..5).all(|k| sm[i + k] <= gate) {
            return Some(i);
        }
    }
    None
}

/// 最后一个高于噪声门限的块（Python last_above_gate）。
fn last_above_gate(pblock: &[f64], nf_db: f64) -> usize {
    let gate = nf_db + 3.0;
    let mut last = 0usize;
    for (i, p) in pblock.iter().enumerate() {
        if db10(*p) > gate {
            last = i;
        }
    }
    last
}

/// Chu/Lundeby 噪声修正 Schroeder EDC：返回 (t, edc_db_corrected, e0)。
fn schroeder_edc(seg: &[f64], sample_rate: f64, noise_power: f64) -> (Vec<f64>, Vec<f64>, f64) {
    let n = seg.len();
    let mut energy = vec![0.0f64; n];
    let mut acc = 0.0;
    for i in (0..n).rev() {
        acc += seg[i] * seg[i];
        energy[i] = acc;
    }
    let e0 = energy.first().copied().unwrap_or(0.0);
    let t: Vec<f64> = (0..n).map(|i| i as f64 / sample_rate).collect();
    if e0 <= 0.0 {
        return (t, vec![0.0; n], 0.0);
    }
    let ref_db = 10.0 * e0.log10();
    let corr_db: Vec<f64> = (0..n)
        .map(|i| {
            let remaining = (n - i) as f64;
            let v = energy[i] - noise_power * remaining;
            10.0 * v.max(1e-300).log10() - ref_db
        })
        .collect();
    (t, corr_db, e0)
}

/// 第一个满足 y <= threshold 的下标（Python np.argmax(y <= threshold) 的等价物，无解 None）。
fn first_index_le(y: &[f64], threshold: f64) -> Option<usize> {
    y.iter().position(|v| *v <= threshold)
}

/// EDC 最小二乘拟合（Python fit_decay）。
fn fit_decay(
    t: &[f64],
    y: &[f64],
    start_db: f64,
    end_db: f64,
    min_points: usize,
) -> Option<Rt60DecayFit> {
    let ia = first_index_le(y, start_db)?;
    let ib = first_index_le(y, end_db)?;
    if ib <= ia || ib >= y.len() || ib >= t.len() {
        return None;
    }
    let xs = &t[ia..=ib];
    let ys = &y[ia..=ib];
    if ys.len() < min_points || xs[xs.len() - 1] <= xs[0] {
        return None;
    }
    let (slope, intercept, r2) = linear_fit(xs, ys)?;
    Some(Rt60DecayFit {
        slope_db_s: slope,
        intercept_db: intercept,
        t0_s: xs[0],
        t1_s: xs[xs.len() - 1],
        span_s: xs[xs.len() - 1] - xs[0],
        r2,
        rt60_s: if slope < -1e-9 {
            Some(-60.0 / slope)
        } else {
            None
        },
        i0: ia,
        i1: ib,
    })
}

/// 块包络拟合（Python fit_envelope）。
fn fit_envelope(pblock: &[f64], dt: f64, peak_db: f64, nf_db: f64) -> Option<Rt60Envelope> {
    let dbv: Vec<f64> = pblock.iter().map(|p| db10(*p)).collect();
    let last = last_above_gate(pblock, nf_db);
    let pk = argmax(&dbv);
    if last <= pk + 4 {
        return None;
    }
    let fit = |a_db: f64, b_db: f64| -> Option<Rt60EnvFit> {
        let i0 = (pk..=last).find(|i| dbv[*i] <= peak_db - a_db)?;
        let i1 = (i0..=last).find(|i| dbv[*i] <= peak_db - b_db)?;
        if i1 < i0 + 4 {
            return None;
        }
        let xs: Vec<f64> = (i0..=i1).map(|i| i as f64 * dt).collect();
        let ys = &dbv[i0..=i1];
        let (slope, _intercept, r2) = linear_fit(&xs, ys)?;
        Some(Rt60EnvFit {
            slope_db_s: slope,
            t0_s: xs[0],
            t1_s: xs[xs.len() - 1],
            span_s: xs[xs.len() - 1] - xs[0],
            r2,
            rt60_s: if slope < -1e-9 {
                Some(-60.0 / slope)
            } else {
                None
            },
        })
    };
    Some(Rt60Envelope {
        env20: fit(5.0, 25.0),
        env15: fit(5.0, 20.0),
        last_above_s: last as f64 * dt,
    })
}

/// 第一个最大值的下标（Python np.argmax：并列取首个）。
fn argmax(x: &[f64]) -> usize {
    let mut best = 0usize;
    let mut best_val = f64::NEG_INFINITY;
    for (i, v) in x.iter().enumerate() {
        if *v > best_val {
            best_val = *v;
            best = i;
        }
    }
    best
}

/// 绝对值最大下标（Python np.argmax(np.abs(x))）。
fn argmax_abs(x: &[f64]) -> usize {
    let mut best = 0usize;
    let mut best_val = f64::NEG_INFINITY;
    for (i, v) in x.iter().enumerate() {
        let a = v.abs();
        if a > best_val {
            best_val = a;
            best = i;
        }
    }
    best
}

// ---------------------------------------------------------------------------
// 单频带分析
// ---------------------------------------------------------------------------

/// 不可分析频带的占位结果（band_out_of_range）。
fn out_of_range_band(center_hz: f64) -> Rt60Band {
    Rt60Band {
        center_hz,
        noise_floor_db: f64::NAN,
        noise_plateau_start_s: f64::NAN,
        noise_plateau_end_s: f64::NAN,
        noise_spread_db: None,
        noise_confidence: "none",
        decay_end_s: f64::NAN,
        window_s: f64::NAN,
        edc_plateau_db: f64::NAN,
        edt: None,
        t15: None,
        t20: None,
        t30: None,
        env: None,
        noise_ratio_t20: None,
        noise_ratio_t15: None,
        method: None,
        rt60_s: None,
        r2: None,
        env_rt60_s: None,
        env_r2: None,
        cross_check_rel: None,
        flags: vec!["band_out_of_range"],
        valid: false,
        confidence: "none",
        band_out_of_range: true,
    }
}

/// 噪声剩余比（fit 结束处剩余噪声能量 / 修正信号能量）。
fn noise_ratio(fit: Option<&Rt60DecayFit>, nseg: usize, nf_power: f64, e0: f64) -> Option<f64> {
    let fit = fit?;
    let rem = (nseg.saturating_sub(fit.i1)).max(1) as f64;
    let noise_rem = nf_power * rem;
    let pred_db = fit.intercept_db + fit.slope_db_s * fit.t1_s;
    let sig = (e0 * 10.0f64.powf(pred_db / 10.0)).max(1e-300);
    Some(noise_rem / sig)
}

/// 单倍频带 RT60（Python analyze_band 的逐行移植）。
fn analyze_band(
    x: &[f64],
    sample_rate: f64,
    onset: usize,
    center_hz: f64,
    config: &Rt60Config,
) -> Rt60Band {
    let Some(y) = band_signal(&x[onset..], center_hz, sample_rate) else {
        return out_of_range_band(center_hz);
    };
    if y.len() < (0.2 * sample_rate) as usize {
        return out_of_range_band(center_hz);
    }
    let (pblock, dt) = block_power(&y, sample_rate);
    if pblock.len() < 8 {
        return out_of_range_band(center_hz);
    }
    let pk_db = db10(pblock.iter().cloned().fold(f64::NEG_INFINITY, f64::max));
    let (noise, noise_confidence) = match find_noise_plateau(&pblock, dt, pk_db) {
        Some(noise) => (noise, "ok"),
        None => {
            let nb = ((pblock.len() as f64 * 0.2) as usize)
                .max(4)
                .min(pblock.len());
            let mut tail = pblock[pblock.len() - nb..].to_vec();
            let nf = median(&mut tail);
            (
                (
                    pblock.len() - nb,
                    pblock.len() - 1,
                    db10(nf),
                    nb as f64 * dt,
                ),
                "low",
            )
        }
    };
    let nf_db = noise.2;
    let nf_power = 10.0f64.powf(nf_db / 10.0);
    let mut nf_spread: Option<f64> = None;
    let run = &pblock[noise.0..=noise.1];
    if run.len() >= 2 {
        let p90 = percentile_linear(run, 90.0);
        let p10 = percentile_linear(run, 10.0);
        nf_spread = Some(10.0 * ((p90 + 1e-300) / (p10 + 1e-300)).log10());
    }
    let peak_idx = argmax(&pblock);
    let (decay_end_s, window_limited) = match decay_gate_index(&pblock, nf_db, peak_idx) {
        Some(gate_idx) => (gate_idx as f64 * dt, false),
        None => (y.len() as f64 / sample_rate, true),
    };
    let mut win_s = (y.len() as f64 / sample_rate).min(config.window_ms / 1000.0);
    if !window_limited {
        win_s = win_s.min(decay_end_s + GATE_MARGIN_S);
    }
    let win_s = win_s.max(0.12);
    let nseg = y.len().min((win_s * sample_rate) as usize);
    let (t, edc, e0) = schroeder_edc(&y[..nseg], sample_rate, nf_power);
    let plateau_db = if e0 > 0.0 {
        db10(nf_power * nseg as f64 / e0)
    } else {
        -300.0
    };
    let edt = fit_decay(&t, &edc, 0.0, -10.0, 4);
    let t15 = fit_decay(&t, &edc, -5.0, -20.0, 4);
    let t20 = fit_decay(&t, &edc, -5.0, -25.0, 4);
    let t30 = fit_decay(&t, &edc, -5.0, -35.0, 4);
    let env = fit_envelope(&pblock, dt, pk_db, nf_db);
    let env20 = env.as_ref().and_then(|e| e.env20.as_ref());
    let env15 = env.as_ref().and_then(|e| e.env15.as_ref());
    let nr_t20 = noise_ratio(t20.as_ref(), nseg, nf_power, e0);
    let nr_t15 = noise_ratio(t15.as_ref(), nseg, nf_power, e0);

    let ok = |fit: Option<&Rt60DecayFit>, min_span: f64, min_r2: f64, max_ratio: f64| -> bool {
        let Some(fit) = fit else { return false };
        if fit.rt60_s.is_none() || fit.span_s < min_span || fit.r2 < min_r2 {
            return false;
        }
        match noise_ratio(Some(fit), nseg, nf_power, e0) {
            Some(ratio) => ratio <= max_ratio,
            None => false,
        }
    };

    let mut flags: Vec<&'static str> = Vec::new();
    let mut method: Option<&'static str> = None;
    let mut rt60: Option<f64> = None;
    let mut r2: Option<f64> = None;
    if ok(t20.as_ref(), 0.06, 0.99, 2.0) {
        method = Some("T20");
        rt60 = t20.as_ref().and_then(|f| f.rt60_s);
        r2 = t20.as_ref().map(|f| f.r2);
    } else if ok(t15.as_ref(), 0.05, 0.985, 3.0) {
        method = Some("T15x4");
        rt60 = t15.as_ref().and_then(|f| f.rt60_s).map(|v| v * 4.0 / 3.0);
        r2 = t15.as_ref().map(|f| f.r2);
        flags.push("T20_noise_limited");
    } else if let Some(env20) = env20 {
        let passes = env20
            .rt60_s
            .map(|_| env20.span_s >= 0.05 && env20.r2 >= 0.97)
            .unwrap_or(false);
        if passes {
            method = Some("ENV20");
            rt60 = env20.rt60_s;
            r2 = Some(env20.r2);
            flags.push("schroeder_limited");
        }
    }
    if method.is_none() {
        if let Some(env15) = env15 {
            let passes = env15
                .rt60_s
                .map(|_| env15.span_s >= 0.04 && env15.r2 >= 0.96)
                .unwrap_or(false);
            if passes {
                method = Some("ENV15x1.33");
                rt60 = env15.rt60_s.map(|v| v * 20.0 / 15.0);
                r2 = Some(env15.r2);
                flags.push("schroeder_limited");
                flags.push("envelope_limited");
            }
        }
    }
    if method.is_none() && ok(edt.as_ref(), 0.03, 0.98, 4.0) {
        method = Some("EDT_x6");
        rt60 = edt.as_ref().and_then(|f| f.rt60_s).map(|v| v * 6.0);
        r2 = edt.as_ref().map(|f| f.r2);
        flags.push("schroeder_limited");
        flags.push("edt_only_low_confidence");
    }
    if method.is_none() {
        flags.push("unmeasurable");
    }
    if window_limited {
        flags.push("decay_exceeds_window");
    }
    if noise_confidence != "ok" {
        flags.push("noise_confidence_low");
    }
    if let Some(spread) = nf_spread {
        if spread > 8.0 {
            flags.push("noise_nonstationary");
        }
    }
    if plateau_db > -27.0 {
        flags.push("edc_noise_plateau_high");
    }
    let mut cross: Option<f64> = None;
    if let Some(value) = rt60 {
        let mut others: Vec<f64> = Vec::new();
        if method.map(|m| m.starts_with('T')).unwrap_or(false) {
            if let Some(env20) = env20 {
                if let Some(v) = env20.rt60_s {
                    others.push(v);
                }
            }
        } else if let Some(t20) = t20.as_ref() {
            if let Some(v) = t20.rt60_s {
                others.push(v);
            }
        }
        let others: Vec<f64> = others.into_iter().filter(|v| *v > 1e-3).collect();
        if let Some(reference) = others.first() {
            let rel = (value - reference).abs() / reference.max(1e-6);
            cross = Some(rel);
            if rel > 0.15 {
                flags.push("cross_check_mismatch");
            }
        }
    }
    if let Some(value) = rt60 {
        if value < 0.1 {
            flags.push("rt_short_suspect");
        } else if value > 1.2 {
            flags.push("rt_long_suspect");
        } else if !(0.2..=0.8).contains(&value) {
            flags.push("outside_typical_residential");
        }
        if decay_end_s >= config.window_ms / 1000.0 * 0.95 {
            flags.push("gate_at_window_edge");
        }
    }
    let disqualified = [
        "unmeasurable",
        "rt_long_suspect",
        "rt_short_suspect",
        "cross_check_mismatch",
        "decay_exceeds_window",
        "noise_confidence_low",
    ];
    let valid = rt60.is_some()
        && matches!(method, Some("T20") | Some("T15x4") | Some("ENV20"))
        && !flags.iter().any(|f| disqualified.contains(f));
    let confidence = if method == Some("T20") && valid && cross.map(|c| c <= 0.10).unwrap_or(true) {
        "high"
    } else if valid {
        "medium"
    } else if rt60.is_some() {
        "low"
    } else {
        "none"
    };
    let env_rt60_s = env20.and_then(|e| e.rt60_s);
    let env_r2 = env20.map(|e| e.r2);
    Rt60Band {
        center_hz,
        noise_floor_db: nf_db,
        noise_plateau_start_s: noise.0 as f64 * dt,
        noise_plateau_end_s: noise.1 as f64 * dt,
        noise_spread_db: nf_spread,
        noise_confidence,
        decay_end_s,
        window_s: nseg as f64 / sample_rate,
        edc_plateau_db: plateau_db,
        edt,
        t15,
        t20,
        t30,
        env,
        noise_ratio_t20: nr_t20,
        noise_ratio_t15: nr_t15,
        method,
        rt60_s: rt60,
        r2,
        env_rt60_s,
        env_r2,
        cross_check_rel: cross,
        flags,
        valid,
        confidence,
        band_out_of_range: false,
    }
}

// ---------------------------------------------------------------------------
// CSD（STFT，与 Python plot_csd 同口径）
// ---------------------------------------------------------------------------

/// CSD：21.3 ms Hann STFT（nperseg=1024，noverlap=768，boundary=None），30 Hz–16 kHz。
fn csd_matrix(x: &[f64], sample_rate: f64, onset: usize, window_ms: f64) -> CsdMatrix {
    const NPERS: usize = 1024;
    const NOVERLAP: usize = 768;
    let step = NPERS - NOVERLAP;
    let want = (window_ms / 1000.0 * sample_rate) as usize;
    let start = onset.min(x.len());
    let available = x.len() - start;
    let seg_len = want.min(available);
    let seg = &x[start..start + seg_len];
    let frame_count = if seg.len() >= NPERS {
        (seg.len() - NPERS) / step + 1
    } else {
        0
    };
    let mut win = vec![0.0f64; NPERS];
    for (n, w) in win.iter_mut().enumerate() {
        *w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / NPERS as f64).cos();
    }
    let mut frames: Vec<Vec<f64>> = Vec::with_capacity(frame_count);
    let mut global_peak = 0.0f64;
    for f in 0..frame_count {
        let offset = f * step;
        let mut windowed = vec![0.0f64; NPERS];
        for n in 0..NPERS {
            windowed[n] = seg[offset + n] * win[n];
        }
        let spec = rfft(&windowed, NPERS);
        let abs: Vec<f64> = spec.iter().map(|c| c.abs()).collect();
        for a in &abs {
            if *a > global_peak {
                global_peak = *a;
            }
        }
        frames.push(abs);
    }
    let mut freqs_hz: Vec<f64> = Vec::new();
    let mut db_re_peak: Vec<Vec<f64>> = Vec::new();
    let peak_db = 20.0 * global_peak.max(1e-12).log10();
    for k in 0..=NPERS / 2 {
        let freq = k as f64 * sample_rate / NPERS as f64;
        if !(30.0..=16000.0).contains(&freq) {
            continue;
        }
        freqs_hz.push(freq);
        let row: Vec<f64> = frames
            .iter()
            .map(|abs| 20.0 * abs[k].max(1e-12).log10() - peak_db)
            .collect();
        db_re_peak.push(row);
    }
    let times_ms: Vec<f64> = (0..frame_count)
        .map(|f| (NPERS / 2 + f * step) as f64 / sample_rate * 1000.0)
        .collect();
    CsdMatrix {
        freqs_hz,
        times_ms,
        db_re_peak,
        nperseg: NPERS,
        noverlap: NOVERLAP,
    }
}

// ---------------------------------------------------------------------------
// 单个 IR 分析
// ---------------------------------------------------------------------------

/// 分析一个（已反卷积的）IR：峰值归一化 + onset + 每频带 RT60 + CSD。
///
/// 输入为单通道时域 IR（任意量纲；内部按峰值归一化）。
pub fn analyze_ir(
    samples: &[f64],
    sample_rate_hz: u32,
    label: &str,
    config: &Rt60Config,
) -> Result<Rt60IrAnalysis, String> {
    config.validate()?;
    if samples.is_empty() {
        return Err(format!("rt60: {label}: empty IR"));
    }
    if sample_rate_hz == 0 {
        return Err(format!("rt60: {label}: zero sample rate"));
    }
    let fs = sample_rate_hz as f64;
    let peak = samples.iter().fold(0.0f64, |acc, v| acc.max(v.abs()));
    if !(peak > 0.0) || !peak.is_finite() {
        return Err(format!("rt60: {label}: all-zero or non-finite IR"));
    }
    let x: Vec<f64> = samples.iter().map(|v| v / peak).collect();
    let lim = x.len().min((0.1 * fs) as usize).max(1);
    let onset = argmax_abs(&x[..lim]);
    let mut bands = Vec::with_capacity(config.bands_hz.len());
    for fc in &config.bands_hz {
        bands.push(analyze_band(&x, fs, onset, *fc, config));
    }
    let csd = csd_matrix(&x, fs, onset, config.csd_window_ms);
    Ok(Rt60IrAnalysis {
        label: label.to_string(),
        sample_rate_hz,
        samples: samples.len(),
        onset_sample: onset,
        onset_ms: onset as f64 / fs * 1000.0,
        peak_dbfs: 20.0 * peak.log10(),
        bands,
        csd,
    })
}

// ---------------------------------------------------------------------------
// WAV 读写（PCM16/24/32 读；24-bit 写）
// ---------------------------------------------------------------------------

/// 读取的 PCM WAV 数据（只取第 0 声道，与 Python read_wav 一致）。
#[derive(Clone, Debug, PartialEq)]
pub struct WavPcm {
    /// 第 0 声道样本（归一化到 [-1,1)）。
    pub samples: Vec<f64>,
    /// 采样率（Hz）。
    pub sample_rate_hz: u32,
    /// 声道数。
    pub channels: u16,
    /// 位深。
    pub bits_per_sample: u16,
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let slice = bytes.get(offset..offset + 2)?;
    Some(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let slice = bytes.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

/// 读 PCM WAV（16/24/32-bit；支持 WAVE_FORMAT_EXTENSIBLE 的 PCM 子格式）。
pub fn read_wav_pcm(path: &Path) -> Result<WavPcm, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read wav {}: {e}", path.display()))?;
    parse_wav_pcm(&bytes).map_err(|e| format!("parse wav {}: {e}", path.display()))
}

/// 解析 PCM WAV 字节流。
pub fn parse_wav_pcm(bytes: &[u8]) -> Result<WavPcm, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".to_string());
    }
    let mut offset = 12usize;
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    let mut data: Option<&[u8]> = None;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size =
            read_u32(bytes, offset + 4).ok_or_else(|| "truncated chunk size".to_string())? as usize;
        let body_start = offset + 8;
        let body_end = body_start
            .checked_add(size)
            .ok_or_else(|| "chunk size overflow".to_string())?;
        if body_end > bytes.len() {
            return Err(format!("truncated chunk {}", String::from_utf8_lossy(id)));
        }
        if id == b"fmt " {
            if size < 16 {
                return Err("fmt chunk too short".to_string());
            }
            let format = read_u16(bytes, body_start).ok_or("fmt format")?;
            let channels = read_u16(bytes, body_start + 2).ok_or("fmt channels")?;
            let rate = read_u32(bytes, body_start + 4).ok_or("fmt rate")?;
            let bits = read_u16(bytes, body_start + 14).ok_or("fmt bits")?;
            if format != 1 && format != 0xFFFE {
                return Err(format!(
                    "unsupported WAVE format {format} (only PCM/EXTENSIBLE-PCM accepted)"
                ));
            }
            fmt = Some((format, channels, rate, bits));
        } else if id == b"data" {
            data = Some(&bytes[body_start..body_end]);
        }
        offset = body_end + (size & 1);
    }
    let (_format, channels, sample_rate_hz, bits) = fmt.ok_or("missing fmt chunk")?;
    let data = data.ok_or("missing data chunk")?;
    if channels == 0 {
        return Err("zero channels".to_string());
    }
    if sample_rate_hz == 0 {
        return Err("zero sample rate".to_string());
    }
    let bytes_per_sample = (bits as usize).div_ceil(8);
    let block_align = bytes_per_sample
        .checked_mul(channels as usize)
        .ok_or("block align overflow")?;
    if block_align == 0 {
        return Err("zero block align".to_string());
    }
    let frames = data.len() / block_align;
    let mut samples = Vec::with_capacity(frames);
    for frame in 0..frames {
        let base = frame * block_align;
        let v = match bits {
            16 => {
                let raw = read_u16(data, base).ok_or("truncated sample")? as i16;
                raw as f64 / 32768.0
            }
            24 => {
                let b0 = *data.get(base).ok_or("truncated sample")? as i32;
                let b1 = *data.get(base + 1).ok_or("truncated sample")? as i32;
                let b2 = *data.get(base + 2).ok_or("truncated sample")? as i32;
                let raw = b0 | (b1 << 8) | (b2 << 16);
                let signed = if raw & 0x800000 != 0 {
                    raw - 0x100_0000
                } else {
                    raw
                };
                signed as f64 / 8_388_608.0
            }
            32 => {
                let raw = read_u32(data, base).ok_or("truncated sample")? as i32;
                raw as f64 / 2_147_483_648.0
            }
            other => return Err(format!("unsupported bits_per_sample {other}")),
        };
        samples.push(v);
    }
    Ok(WavPcm {
        samples,
        sample_rate_hz,
        channels,
        bits_per_sample: bits,
    })
}

/// 编码 24-bit PCM 单声道 WAV 字节（峰值归一化到 0.9，防削波）。
pub fn encode_wav_pcm24(samples: &[f64], sample_rate_hz: u32) -> Vec<u8> {
    let peak = samples.iter().fold(0.0f64, |acc, v| acc.max(v.abs()));
    let scale = if peak > 0.0 { 0.9 / peak } else { 0.0 };
    let mut data = Vec::with_capacity(samples.len() * 3);
    for sample in samples {
        let value = (sample * scale).clamp(-1.0, 1.0);
        let mut int = (value * 8_388_608.0).round() as i64;
        int = int.clamp(-8_388_608, 8_388_607);
        let raw = int as i32;
        data.push((raw & 0xff) as u8);
        data.push(((raw >> 8) & 0xff) as u8);
        data.push(((raw >> 16) & 0xff) as u8);
    }
    let mut out = Vec::with_capacity(44 + data.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data.len()) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&sample_rate_hz.to_le_bytes());
    out.extend_from_slice(&(sample_rate_hz * 3).to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    out
}

/// 写 24-bit PCM WAV 文件（峰值归一化到 0.9）。
pub fn write_wav_pcm24(path: &Path, samples: &[f64], sample_rate_hz: u32) -> Result<(), String> {
    let bytes = encode_wav_pcm24(samples, sample_rate_hz);
    std::fs::write(path, bytes).map_err(|e| format!("write wav {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// 报告 / JSON / CSV（job artifact 内容）
// ---------------------------------------------------------------------------

/// RT60 报告来源（诚实边界；离线 fake 一律 simulated + 真实测量 NOT_RUN）。
#[derive(Clone, Debug, PartialEq)]
pub struct Rt60Source {
    /// 数据来源：external_ir（外部真实 IR 回放）| deconvolved_capture（job 采集反卷积）。
    pub kind: &'static str,
    /// 触发 stage（sweep_capture / remeasure / holdout / offline_cli）。
    pub stage: String,
    /// 引擎名（fake-synth / alsa-tools / offline）。
    pub engine: String,
    /// 是否离线合成流水线。
    pub simulated: bool,
    /// 真实房间测量状态（NOT_RUN / EXECUTED_ON_HOST）。
    pub real_measurement: &'static str,
    /// IR 来源说明（人读）。
    pub ir_origin: String,
    /// job 采集 raw 的 sha256（反卷积路径有）。
    pub capture_sha256: Option<String>,
    /// sweep 起始频率（Hz）。
    pub sweep_start_hz: Option<f64>,
    /// sweep 结束频率（Hz）。
    pub sweep_end_hz: Option<f64>,
    /// sweep 单遍时长（s）。
    pub sweep_seconds: Option<f64>,
}

/// 一个已分析 IR 及其溯源信息。
#[derive(Clone, Debug, PartialEq)]
pub struct Rt60AnalyzedIr {
    /// 分析结果。
    pub analysis: Rt60IrAnalysis,
    /// 输入 WAV 路径（外部 IR）。
    pub source_path: Option<String>,
    /// 输入 WAV 的 sha256。
    pub ir_sha256: Option<String>,
    /// 反卷积 IR 写出的 job artifact 相对路径（自动路径）。
    pub ir_wav_path: Option<String>,
    /// 平均的重复遍数（1 = 单遍）。
    pub repeats_averaged: u32,
}

/// RT60 报告（summary.json + rt60.csv 的输入）。
#[derive(Clone, Debug, PartialEq)]
pub struct Rt60Report {
    /// 分析配置。
    pub config: Rt60Config,
    /// 来源。
    pub source: Rt60Source,
    /// 生成时间（unix 秒）。
    pub generated_unix: u64,
    /// 已分析 IR。
    pub irs: Vec<Rt60AnalyzedIr>,
    /// 逐 IR 错误（部分失败仍写出其余结果）。
    pub errors: Vec<String>,
}

/// 把标签安全化为 artifact 文件名（仅 [A-Za-z0-9._-]；最长 48 字符）。
pub fn safe_artifact_label(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    for c in label.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "ir".to_string()
    } else if trimmed.chars().count() > 48 {
        trimmed.chars().take(48).collect()
    } else {
        trimmed
    }
}

/// 当前 unix 秒。
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn push_json_escaped(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '"' => {
                out.push('\\');
                out.push('"');
            }
            '\\' => {
                out.push('\\');
                out.push('\\');
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
}

fn push_json_str_field(out: &mut String, key: &str, value: &str) {
    out.push('"');
    push_json_escaped(out, key);
    out.push_str("\":\"");
    push_json_escaped(out, value);
    out.push('"');
}

fn fmt_num(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.6}")
    } else {
        "null".to_string()
    }
}

fn fmt_num4(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.4}")
    } else {
        "null".to_string()
    }
}

fn push_json_num_field(out: &mut String, key: &str, value: f64) {
    out.push('"');
    push_json_escaped(out, key);
    out.push_str("\":");
    out.push_str(&fmt_num(value));
}

fn push_json_opt_num_field(out: &mut String, key: &str, value: Option<f64>) {
    out.push('"');
    push_json_escaped(out, key);
    out.push_str("\":");
    match value {
        Some(v) if v.is_finite() => out.push_str(&fmt_num(v)),
        _ => out.push_str("null"),
    }
}

fn push_json_opt_num4_field(out: &mut String, key: &str, value: Option<f64>) {
    out.push('"');
    push_json_escaped(out, key);
    out.push_str("\":");
    match value {
        Some(v) if v.is_finite() => out.push_str(&fmt_num4(v)),
        _ => out.push_str("null"),
    }
}

fn push_json_bool_field(out: &mut String, key: &str, value: bool) {
    out.push('"');
    push_json_escaped(out, key);
    out.push_str(if value { "\":true" } else { "\":false" });
}

fn push_decay_fit_json(out: &mut String, key: &str, fit: Option<&Rt60DecayFit>) {
    out.push('"');
    push_json_escaped(out, key);
    out.push_str("\":");
    let Some(fit) = fit else {
        out.push_str("null");
        return;
    };
    out.push('{');
    push_json_num_field(out, "slope_db_s", fit.slope_db_s);
    out.push(',');
    push_json_num_field(out, "intercept_db", fit.intercept_db);
    out.push(',');
    push_json_num_field(out, "t0_s", fit.t0_s);
    out.push(',');
    push_json_num_field(out, "t1_s", fit.t1_s);
    out.push(',');
    push_json_num_field(out, "span_s", fit.span_s);
    out.push(',');
    push_json_num_field(out, "r2", fit.r2);
    out.push(',');
    push_json_opt_num_field(out, "rt60_s", fit.rt60_s);
    out.push('}');
}

fn push_env_fit_json(out: &mut String, key: &str, fit: Option<&Rt60EnvFit>) {
    out.push('"');
    push_json_escaped(out, key);
    out.push_str("\":");
    let Some(fit) = fit else {
        out.push_str("null");
        return;
    };
    out.push('{');
    push_json_num_field(out, "slope_db_s", fit.slope_db_s);
    out.push(',');
    push_json_num_field(out, "t0_s", fit.t0_s);
    out.push(',');
    push_json_num_field(out, "t1_s", fit.t1_s);
    out.push(',');
    push_json_num_field(out, "span_s", fit.span_s);
    out.push(',');
    push_json_num_field(out, "r2", fit.r2);
    out.push(',');
    push_json_opt_num_field(out, "rt60_s", fit.rt60_s);
    out.push('}');
}

fn push_envelope_json(out: &mut String, env: Option<&Rt60Envelope>) {
    out.push('"');
    push_json_escaped(out, "env");
    out.push_str("\":");
    let Some(env) = env else {
        out.push_str("null");
        return;
    };
    out.push('{');
    push_env_fit_json(out, "env20", env.env20.as_ref());
    out.push(',');
    push_env_fit_json(out, "env15", env.env15.as_ref());
    out.push(',');
    push_json_num_field(out, "last_above_s", env.last_above_s);
    out.push('}');
}

fn push_flags_json(out: &mut String, flags: &[&'static str]) {
    out.push('"');
    push_json_escaped(out, "flags");
    out.push_str("\":[");
    for (i, flag) in flags.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        push_json_escaped(out, flag);
        out.push('"');
    }
    out.push(']');
}

fn push_band_json(out: &mut String, band: &Rt60Band) {
    out.push('{');
    push_json_num_field(out, "center_hz", band.center_hz);
    out.push(',');
    push_json_opt_num_field(out, "noise_floor_db", Some(band.noise_floor_db));
    out.push(',');
    push_json_opt_num_field(
        out,
        "noise_plateau_start_s",
        Some(band.noise_plateau_start_s),
    );
    out.push(',');
    push_json_opt_num_field(out, "noise_plateau_end_s", Some(band.noise_plateau_end_s));
    out.push(',');
    push_json_opt_num_field(out, "noise_spread_db", band.noise_spread_db);
    out.push(',');
    push_json_str_field(out, "noise_confidence", band.noise_confidence);
    out.push(',');
    push_json_opt_num_field(out, "decay_end_s", Some(band.decay_end_s));
    out.push(',');
    push_json_opt_num_field(out, "window_s", Some(band.window_s));
    out.push(',');
    push_json_opt_num_field(out, "edc_plateau_db", Some(band.edc_plateau_db));
    out.push(',');
    push_decay_fit_json(out, "edt", band.edt.as_ref());
    out.push(',');
    push_decay_fit_json(out, "t15", band.t15.as_ref());
    out.push(',');
    push_decay_fit_json(out, "t20", band.t20.as_ref());
    out.push(',');
    push_decay_fit_json(out, "t30", band.t30.as_ref());
    out.push(',');
    push_envelope_json(out, band.env.as_ref());
    out.push(',');
    push_json_opt_num_field(out, "noise_ratio_t20", band.noise_ratio_t20);
    out.push(',');
    push_json_opt_num_field(out, "noise_ratio_t15", band.noise_ratio_t15);
    out.push(',');
    match band.method {
        Some(method) => push_json_str_field(out, "method", method),
        None => {
            out.push('"');
            push_json_escaped(out, "method");
            out.push_str("\":null");
        }
    }
    out.push(',');
    push_json_opt_num4_field(out, "rt60_s", band.rt60_s);
    out.push(',');
    push_json_opt_num4_field(out, "r2", band.r2);
    out.push(',');
    push_json_opt_num4_field(out, "env_rt60_s", band.env_rt60_s);
    out.push(',');
    push_json_opt_num4_field(out, "env_r2", band.env_r2);
    out.push(',');
    push_json_opt_num4_field(out, "cross_check_rel", band.cross_check_rel);
    out.push(',');
    push_flags_json(out, &band.flags);
    out.push(',');
    push_json_bool_field(out, "valid", band.valid);
    out.push(',');
    push_json_str_field(out, "confidence", band.confidence);
    out.push(',');
    push_json_bool_field(out, "band_out_of_range", band.band_out_of_range);
    out.push('}');
}

impl Rt60Report {
    /// 机读 summary JSON（schema khaen-rt60/v1）。
    pub fn summary_json(&self) -> String {
        let mut out = String::with_capacity(8192);
        out.push('{');
        push_json_str_field(&mut out, "schema", RT60_SCHEMA);
        out.push(',');
        out.push_str("\"tool\":{");
        push_json_str_field(&mut out, "name", "khaen-cal rt60 (rust)");
        out.push(',');
        push_json_str_field(&mut out, "version", RT60_TOOL_VERSION);
        out.push(',');
        push_json_str_field(
            &mut out,
            "python_reference",
            "rt60_waterfall.py v2-20261122",
        );
        out.push('}');
        out.push(',');
        out.push_str(&format!("\"generated_unix\":{},", self.generated_unix));
        out.push_str("\"config\":{");
        out.push_str("\"bands_hz\":[");
        for (i, band) in self.config.bands_hz.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&fmt_num(*band));
        }
        out.push_str("],");
        push_json_num_field(&mut out, "window_ms", self.config.window_ms);
        out.push(',');
        push_json_num_field(&mut out, "csd_window_ms", self.config.csd_window_ms);
        out.push(',');
        push_json_num_field(&mut out, "block_ms", BLOCK_MS);
        out.push(',');
        push_json_num_field(&mut out, "noise_min_duration_s", NOISE_MIN_DUR_S);
        out.push(',');
        push_json_num_field(&mut out, "noise_max_spread_db", NOISE_MAX_SPREAD_DB);
        out.push(',');
        push_json_num_field(&mut out, "noise_min_below_peak_db", NOISE_MIN_BELOW_PEAK_DB);
        out.push(',');
        push_json_num_field(&mut out, "gate_margin_s", GATE_MARGIN_S);
        out.push('}');
        out.push(',');
        out.push_str("\"source\":{");
        push_json_str_field(&mut out, "kind", self.source.kind);
        out.push(',');
        push_json_str_field(&mut out, "stage", &self.source.stage);
        out.push(',');
        push_json_str_field(&mut out, "engine", &self.source.engine);
        out.push(',');
        push_json_bool_field(&mut out, "simulated", self.source.simulated);
        out.push(',');
        push_json_str_field(&mut out, "real_measurement", self.source.real_measurement);
        out.push(',');
        push_json_str_field(&mut out, "ir_origin", &self.source.ir_origin);
        out.push(',');
        match self.source.capture_sha256.as_deref() {
            Some(sha) => push_json_str_field(&mut out, "capture_sha256", sha),
            None => {
                out.push_str("\"capture_sha256\":null");
            }
        }
        out.push(',');
        push_json_opt_num_field(&mut out, "sweep_start_hz", self.source.sweep_start_hz);
        out.push(',');
        push_json_opt_num_field(&mut out, "sweep_end_hz", self.source.sweep_end_hz);
        out.push(',');
        push_json_opt_num_field(&mut out, "sweep_seconds", self.source.sweep_seconds);
        out.push('}');
        out.push(',');
        out.push_str("\"irs\":[");
        for (i, entry) in self.irs.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let analysis = &entry.analysis;
            out.push('{');
            push_json_str_field(&mut out, "label", &analysis.label);
            out.push(',');
            out.push_str(&format!("\"sample_rate_hz\":{},", analysis.sample_rate_hz));
            out.push_str(&format!("\"samples\":{},", analysis.samples));
            out.push_str(&format!("\"onset_sample\":{},", analysis.onset_sample));
            push_json_num_field(&mut out, "onset_ms", analysis.onset_ms);
            out.push(',');
            push_json_num_field(&mut out, "peak_dbfs", analysis.peak_dbfs);
            out.push(',');
            match entry.source_path.as_deref() {
                Some(path) => push_json_str_field(&mut out, "source_path", path),
                None => out.push_str("\"source_path\":null"),
            }
            out.push(',');
            match entry.ir_sha256.as_deref() {
                Some(sha) => push_json_str_field(&mut out, "ir_sha256", sha),
                None => out.push_str("\"ir_sha256\":null"),
            }
            out.push(',');
            match entry.ir_wav_path.as_deref() {
                Some(path) => push_json_str_field(&mut out, "ir_wav_path", path),
                None => out.push_str("\"ir_wav_path\":null"),
            }
            out.push(',');
            out.push_str(&format!("\"repeats_averaged\":{}", entry.repeats_averaged));
            out.push('}');
        }
        out.push(']');
        out.push(',');
        out.push_str("\"rows\":[");
        let mut first = true;
        for entry in &self.irs {
            for band in &entry.analysis.bands {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push('{');
                push_json_str_field(&mut out, "label", &entry.analysis.label);
                out.push(',');
                push_json_num_field(&mut out, "center_hz", band.center_hz);
                out.push(',');
                match band.method {
                    Some(method) => push_json_str_field(&mut out, "method", method),
                    None => out.push_str("\"method\":null"),
                }
                out.push(',');
                push_json_opt_num4_field(&mut out, "rt60_s", band.rt60_s);
                out.push(',');
                push_json_opt_num4_field(&mut out, "r2", band.r2);
                out.push(',');
                push_json_bool_field(&mut out, "valid", band.valid);
                out.push(',');
                push_json_str_field(&mut out, "confidence", band.confidence);
                out.push(',');
                push_json_str_field(&mut out, "flags", &band.flags.join(";"));
                out.push('}');
            }
        }
        out.push(']');
        out.push(',');
        out.push_str("\"bands\":[");
        let mut first = true;
        for entry in &self.irs {
            for band in &entry.analysis.bands {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push('{');
                out.push('"');
                push_json_escaped(&mut out, "label");
                out.push_str("\":\"");
                push_json_escaped(&mut out, &entry.analysis.label);
                out.push_str("\",");
                // 复用 band 内容（先写 label 字段后紧跟 band 字段）
                let mut band_text = String::new();
                push_band_json(&mut band_text, band);
                // 去掉外层 { 与 }，拼接 label
                let inner = band_text.trim_start_matches('{').trim_end_matches('}');
                out.push_str(inner);
                out.push('}');
            }
        }
        out.push(']');
        out.push(',');
        out.push_str("\"errors\":[");
        for (i, error) in self.errors.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('"');
            push_json_escaped(&mut out, error);
            out.push('"');
        }
        out.push_str("],");
        push_json_str_field(
            &mut out,
            "note",
            "RT60/CSD 为观察量（报告/前后对比），不是 apply 通过门槛；valid=false 的行只作追溯。",
        );
        out.push('}');
        out
    }

    /// 机读 rt60.csv（列与 Python rt60_waterfall.py CSV_FIELDS 一致）。
    pub fn rt60_csv(&self) -> String {
        let mut out = String::with_capacity(4096);
        out.push_str("label,ir,center_hz,method,rt60_s,r2,valid,confidence,env_rt60_s,env_r2,cross_check_rel,noise_floor_db,noise_spread_db,noise_confidence,noise_plateau_start_s,noise_plateau_end_s,decay_end_s,window_s,edc_plateau_db,edt_rt60_s,t20_s,t20_r2,t30_s,flags\n");
        for entry in &self.irs {
            let ir = entry
                .source_path
                .clone()
                .or_else(|| entry.ir_wav_path.clone())
                .unwrap_or_default();
            for band in &entry.analysis.bands {
                let fields: Vec<String> = vec![
                    entry.analysis.label.clone(),
                    ir.clone(),
                    fmt_num(band.center_hz),
                    band.method.unwrap_or("").to_string(),
                    opt_num(band.rt60_s),
                    opt_num(band.r2),
                    band.valid.to_string(),
                    band.confidence.to_string(),
                    opt_num(band.env_rt60_s),
                    opt_num(band.env_r2),
                    opt_num(band.cross_check_rel),
                    opt_num(Some(band.noise_floor_db)),
                    opt_num(band.noise_spread_db),
                    band.noise_confidence.to_string(),
                    opt_num(Some(band.noise_plateau_start_s)),
                    opt_num(Some(band.noise_plateau_end_s)),
                    opt_num(Some(band.decay_end_s)),
                    opt_num(Some(band.window_s)),
                    opt_num(Some(band.edc_plateau_db)),
                    band.edt
                        .as_ref()
                        .and_then(|f| f.rt60_s)
                        .map(fmt_num)
                        .unwrap_or_default(),
                    band.t20
                        .as_ref()
                        .and_then(|f| f.rt60_s)
                        .map(fmt_num)
                        .unwrap_or_default(),
                    band.t20.as_ref().map(|f| fmt_num(f.r2)).unwrap_or_default(),
                    band.t30
                        .as_ref()
                        .and_then(|f| f.rt60_s)
                        .map(fmt_num)
                        .unwrap_or_default(),
                    band.flags.join(";"),
                ];
                let escaped: Vec<String> = fields.iter().map(|f| csv_escape(f)).collect();
                out.push_str(&escaped.join(","));
                out.push('\n');
            }
        }
        out
    }
}

fn opt_num(value: Option<f64>) -> String {
    match value {
        Some(v) if v.is_finite() => fmt_num(v),
        _ => String::new(),
    }
}

fn csv_escape(text: &str) -> String {
    if text.contains(',') || text.contains('"') || text.contains('\n') || text.contains('\r') {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_string()
    }
}

/// CSD 单 IR JSON（频率 x 时间矩阵；schema khaen-rt60-csd/v1）。
pub fn csd_json(entry: &Rt60AnalyzedIr, window_ms: f64) -> String {
    let analysis = &entry.analysis;
    let csd = &analysis.csd;
    let mut out = String::with_capacity(64 * 1024);
    out.push('{');
    push_json_str_field(&mut out, "schema", RT60_CSD_SCHEMA);
    out.push(',');
    push_json_str_field(&mut out, "label", &analysis.label);
    out.push(',');
    out.push_str(&format!("\"sample_rate_hz\":{},", analysis.sample_rate_hz));
    out.push_str(&format!("\"onset_sample\":{},", analysis.onset_sample));
    push_json_num_field(&mut out, "window_ms", window_ms);
    out.push(',');
    out.push_str(&format!("\"nperseg\":{},", csd.nperseg));
    out.push_str(&format!("\"noverlap\":{},", csd.noverlap));
    out.push_str("\"times_ms\":[");
    for (i, t) in csd.times_ms.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!("{t:.4}"));
    }
    out.push_str("],\"freqs_hz\":[");
    for (i, f) in csd.freqs_hz.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!("{f:.4}"));
    }
    out.push_str("],\"db_re_peak\":[");
    for (i, row) in csd.db_re_peak.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('[');
        for (j, v) in row.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            out.push_str(&format!("{v:.2}"));
        }
        out.push(']');
    }
    out.push(']');
    out.push(',');
    push_json_str_field(
        &mut out,
        "note",
        "CSD = 21.3 ms Hann STFT, dB re global peak; observation only (not an acceptance gate).",
    );
    out.push('}');
    out
}

/// 禁用/不适用时的显式 status artifact（--no-rt60 也要留痕）。
pub fn status_json(enabled: bool, stage: &str, reason: &str) -> String {
    let mut out = String::new();
    out.push('{');
    push_json_str_field(&mut out, "schema", RT60_STATUS_SCHEMA);
    out.push(',');
    push_json_bool_field(&mut out, "enabled", enabled);
    out.push(',');
    push_json_str_field(
        &mut out,
        "status",
        if enabled { "ACTIVE" } else { "DISABLED" },
    );
    out.push(',');
    push_json_str_field(&mut out, "stage", stage);
    out.push(',');
    push_json_str_field(&mut out, "reason", reason);
    out.push(',');
    push_json_str_field(&mut out, "real_measurement", "NOT_RUN");
    out.push(',');
    push_json_str_field(
        &mut out,
        "note",
        "RT60/CSD 后处理为观察量；disabled 表示本次 job 未产出 RT60/CSD。",
    );
    out.push('}');
    out
}

/// RT60 后处理失败时的 warning artifact（主流程继续，不阻断 job）。
pub fn warning_json(stage: &str, message: &str, errors: &[String]) -> String {
    let mut out = String::new();
    out.push('{');
    push_json_str_field(&mut out, "schema", RT60_WARNING_SCHEMA);
    out.push(',');
    push_json_str_field(&mut out, "status", "WARNING");
    out.push(',');
    push_json_str_field(&mut out, "stage", stage);
    out.push(',');
    push_json_str_field(&mut out, "message", message);
    out.push(',');
    out.push_str("\"errors\":[");
    for (i, error) in errors.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        push_json_escaped(&mut out, error);
        out.push('"');
    }
    out.push_str("],");
    push_json_str_field(&mut out, "blocking", "false");
    out.push(',');
    push_json_str_field(&mut out, "real_measurement", "NOT_RUN");
    out.push(',');
    push_json_str_field(
        &mut out,
        "note",
        "RT60 后处理失败不阻断测量/拟合主流程；本 artifact 如实记录失败原因。",
    );
    out.push('}');
    out
}

/// 读 WAV + 分析（外部 IR 路径；返回分析结果和原始字节用于 sha256/证据）。
pub fn analyze_wav_path(
    path: &Path,
    label: &str,
    config: &Rt60Config,
) -> Result<(Rt60IrAnalysis, Vec<u8>), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read wav {}: {e}", path.display()))?;
    let wav = parse_wav_pcm(&bytes).map_err(|e| format!("parse wav {}: {e}", path.display()))?;
    let analysis = analyze_ir(&wav.samples, wav.sample_rate_hz, label, config)?;
    Ok((analysis, bytes))
}

/// 确定性 LCG + Box-Muller（测试用；不用于生产）。
#[cfg(test)]
struct Lcg {
    state: u64,
}

#[cfg(test)]
impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg { state: seed | 1 }
    }
    fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.state
    }
    fn uniform(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 1.0) / ((1u64 << 53) as f64)
    }
    fn normal(&mut self) -> f64 {
        let u1 = self.uniform();
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_ir(rate: u32, rt_true_s: f64, seed: u64) -> Vec<f64> {
        let n = (1.6 * rate as f64) as usize;
        let mut rng = Lcg::new(seed);
        let noise_amp = 10.0f64.powf(-45.0 / 20.0);
        let mut acc = vec![0.0f64; n];
        for _ in 0..8 {
            let mut ir = vec![0.0f64; n];
            for (i, v) in ir.iter_mut().enumerate() {
                let t = i as f64 / rate as f64;
                *v = 0.20 * 10.0f64.powf(-3.0 * t / rt_true_s) * rng.normal();
            }
            ir[0] += 1.0;
            for v in ir.iter_mut() {
                *v += noise_amp * rng.normal();
            }
            let peak = ir.iter().fold(0.0f64, |a, v| a.max(v.abs()));
            for v in ir.iter_mut() {
                *v /= peak;
            }
            for i in 0..n {
                acc[i] += ir[i];
            }
        }
        for v in acc.iter_mut() {
            *v /= 8.0;
        }
        acc
    }

    #[test]
    fn synthetic_rt60_selftest_matches_python_selftest() {
        // 与 Python rt60_waterfall.py --selftest 同构造：0.30/0.50/0.80 s x 1k/2k/4k，
        // 8 次独立实现平均 + -45 dBFS 噪声；容差与 Python 工具一致：
        // |got - true| <= max(0.05, 0.12*true)。
        let rate = 48_000u32;
        let mut config = Rt60Config::default();
        config.bands_hz = vec![1000.0, 2000.0, 4000.0];
        let mut failures = Vec::new();
        for (case, rt_true) in [0.30f64, 0.50, 0.80].iter().enumerate() {
            let ir = synthetic_ir(rate, *rt_true, 0x5EED_2026 + case as u64);
            let analysis = analyze_ir(&ir, rate, "synthetic", &config).expect("analyze");
            for band in &analysis.bands {
                let got = band.rt60_s;
                let tol = 0.05f64.max(0.12 * rt_true);
                let pass = got.map(|v| (v - rt_true).abs() <= tol).unwrap_or(false);
                let got_text = got
                    .map(|v| format!("{v:.3}"))
                    .unwrap_or_else(|| "None".to_string());
                eprintln!(
                    "rust-selftest rt_true={rt_true:.2} fc={:7.1} -> rt={got_text} method={:?} {}",
                    band.center_hz,
                    band.method,
                    if pass { "PASS" } else { "FAIL" }
                );
                if !pass {
                    failures.push(format!(
                        "rt_true={rt_true:.2} fc={:.1} got={:?} method={:?} valid={} flags={:?}",
                        band.center_hz, got, band.method, band.valid, band.flags
                    ));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "synthetic selftest failures: {failures:#?}"
        );
    }

    #[test]
    fn noise_floor_only_is_never_valid_rt60() {
        let rate = 48_000u32;
        let n = rate as usize * 2;
        let mut rng = Lcg::new(0xC0FF_EE01);
        let mut noise = vec![0.0f64; n];
        for v in noise.iter_mut() {
            *v = 10.0f64.powf(-35.0 / 20.0) * rng.normal();
        }
        let analysis = analyze_ir(&noise, rate, "noise", &Rt60Config::default()).expect("analyze");
        assert!(
            analysis.bands.iter().all(|b| !b.valid),
            "pure noise must not yield a valid RT60: {:?}",
            analysis
                .bands
                .iter()
                .map(|b| (b.center_hz, b.rt60_s, b.flags.clone()))
                .collect::<Vec<_>>()
        );
        // 中高频带应显式拒绝（unmeasurable / noise_confidence_low / plateau 高）。
        for band in analysis.bands.iter().filter(|b| b.center_hz >= 500.0) {
            assert!(
                band.rt60_s.is_none() || !band.valid,
                "fc={} rt60={:?} flags={:?}",
                band.center_hz,
                band.rt60_s,
                band.flags
            );
        }
    }

    #[test]
    fn flat_impulse_is_not_reported_as_valid_rt60() {
        let rate = 48_000u32;
        let mut ir = vec![0.0f64; rate as usize];
        ir[0] = 1.0;
        let analysis = analyze_ir(&ir, rate, "impulse", &Rt60Config::default()).expect("analyze");
        assert!(analysis.bands.iter().all(|b| !b.valid));
    }

    #[test]
    fn wav_pcm24_roundtrip_preserves_samples() {
        let rate = 48_000u32;
        let samples: Vec<f64> = (0..256)
            .map(|i| {
                let t = i as f64 / rate as f64;
                (2.0 * std::f64::consts::PI * 1000.0 * t).sin() * 0.5
            })
            .collect();
        let bytes = encode_wav_pcm24(&samples, rate);
        let wav = parse_wav_pcm(&bytes).expect("parse wav");
        assert_eq!(wav.sample_rate_hz, rate);
        assert_eq!(wav.channels, 1);
        assert_eq!(wav.bits_per_sample, 24);
        assert_eq!(wav.samples.len(), samples.len());
        let peak = samples.iter().fold(0.0f64, |a, v| a.max(v.abs()));
        let scaled: Vec<f64> = samples.iter().map(|v| v * 0.9 / peak).collect();
        for (got, want) in wav.samples.iter().zip(scaled.iter()) {
            assert!(
                (got - want).abs() <= 1.5 / 8_388_608.0,
                "wav roundtrip {got} vs {want}"
            );
        }
    }

    #[test]
    fn csd_shape_and_peak_normalisation() {
        let rate = 48_000u32;
        let ir = synthetic_ir(rate, 0.5, 0xCAFE_0001);
        let analysis = analyze_ir(&ir, rate, "csd", &Rt60Config::default()).expect("analyze");
        let csd = &analysis.csd;
        assert_eq!(csd.nperseg, 1024);
        assert_eq!(csd.noverlap, 768);
        // 48000 Hz、600 ms -> 28800 样本 -> (28800-1024)/256+1 = 109 帧
        assert_eq!(csd.times_ms.len(), 109);
        assert_eq!(csd.db_re_peak.len(), csd.freqs_hz.len());
        assert!(csd.freqs_hz.first().copied().unwrap() >= 30.0);
        assert!(csd.freqs_hz.last().copied().unwrap() <= 16000.0);
        assert_eq!(csd.db_re_peak[0].len(), csd.times_ms.len());
        let max_db = csd
            .db_re_peak
            .iter()
            .flat_map(|row| row.iter())
            .fold(f64::NEG_INFINITY, |a, v| a.max(*v));
        assert!((max_db - 0.0).abs() < 1e-9, "CSD max dB = {max_db}");
        // scipy stft boundary=None 口径：首帧时间 = nperseg/2 / fs
        assert!((csd.times_ms[0] - 512.0 / 48.0 / 1000.0 * 1000.0).abs() < 1e-9);
    }

    #[test]
    fn invalid_inputs_are_rejected() {
        let config = Rt60Config::default();
        assert!(analyze_ir(&[], 48_000, "empty", &config).is_err());
        assert!(analyze_ir(&[0.0; 1024], 48_000, "zeros", &config).is_err());
        assert!(analyze_ir(&[1.0], 0, "rate", &config).is_err());
        let mut bad = Rt60Config::default();
        bad.window_ms = 10.0;
        assert!(bad.validate().is_err());
        let mut bad = Rt60Config::default();
        bad.bands_hz.clear();
        assert!(bad.validate().is_err());
    }

    #[test]
    fn report_json_and_csv_are_wellformed() {
        let rate = 48_000u32;
        let ir = synthetic_ir(rate, 0.5, 0x1234_5678);
        let analysis = analyze_ir(&ir, rate, "FL", &Rt60Config::default()).expect("analyze");
        let report = Rt60Report {
            config: Rt60Config::default(),
            source: Rt60Source {
                kind: "external_ir",
                stage: "unit_test".to_string(),
                engine: "offline".to_string(),
                simulated: true,
                real_measurement: "NOT_RUN",
                ir_origin: "unit test synthetic".to_string(),
                capture_sha256: None,
                sweep_start_hz: None,
                sweep_end_hz: None,
                sweep_seconds: None,
            },
            generated_unix: 0,
            irs: vec![Rt60AnalyzedIr {
                analysis,
                source_path: Some("synthetic.wav".to_string()),
                ir_sha256: None,
                ir_wav_path: None,
                repeats_averaged: 1,
            }],
            errors: Vec::new(),
        };
        let json = report.summary_json();
        assert!(json.starts_with('{') && json.ends_with('}'));
        assert!(json.contains("\"schema\":\"khaen-rt60/v1\""));
        assert!(json.contains("\"real_measurement\":\"NOT_RUN\""));
        assert!(json.contains("\"labels\"") == false);
        let dump = std::env::temp_dir().join("khaen-rt60-summary-unit-test.json");
        let _ = std::fs::write(&dump, &json);
        // 简易平衡检查：引号数为偶数（不含转义内部复杂度，仅粗检）
        assert!(
            json.matches('{').count() == json.matches('}').count(),
            "unbalanced braces in {}",
            dump.display()
        );
        assert!(json.matches('[').count() == json.matches(']').count());
        let csv = report.rt60_csv();
        assert!(csv.starts_with("label,ir,center_hz,method,rt60_s"));
        assert!(csv.lines().count() >= 2);
        let status = status_json(false, "sweep_capture", "disabled by --no-rt60");
        assert!(status.contains("\"enabled\":false"));
        let warning = warning_json("sweep_capture", "boom", &["e1".to_string()]);
        assert!(warning.contains("\"status\":\"WARNING\""));
        assert!(warning.contains("\"blocking\":\"false\""));
        let csd = csd_json(&report.irs[0], 600.0);
        assert!(csd.contains("\"schema\":\"khaen-rt60-csd/v1\""));
    }
}
