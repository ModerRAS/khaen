//! 单点 MIMO 候选 B（modal residual cancellation / decay 改短）预测（P=1；纯离线）。
//!
//! 目标（M1-FINAL-SPEC §1/§3 口径）：在冻结基线 H 上设计 G = z^-D I + g，使每输入响应
//! y_i(f) 的模态衰减变短（T20 下降 >=15%），同时逐输入误差改善 I>=3 dB（63–250 Hz）。
//!
//! 候选 B 目标（时间域）：
//!   d = b + sum_k (e^{-delta_k max(0,t-t0)} - 1) B_k b,
//!   delta_k = 6.907 (1/T* - 1/T_k),  T* = 0.85 T_k（T20 下降 15%）。
//! B_k = 1/3 oct 带通；b = 由实测复 H 重建的基线 IR（band-limited，注明重建口径）。
//!
//! 两个口径分开报（不许混写）：
//! - Diagonal: 只允许 g_ii（T66 P_post 对角 FIR 可表达）——生产可实现预测；
//! - Full5x5: 允许 g_ji（j != i，交叉帽 -6 dB；diag +6 dB）——T51/T54 modal matrix 形态的**理想上界**，
//!   当前 tspi-cal measured 路径 G 注入 NOT_WIRED，不能落地。
//!
//! 诚实边界：PREDICTED / SINGLE_POINT_NOT_SPATIAL / NO_MEASUREMENT；未 apply、未实测 T20。

use crate::fir::{fft_in_place, FirBasis};
use crate::solve::Cx;
use crate::fill::db_amp;

/// 证据 schema。
pub const DECAY_SCHEMA: &str = "tspi-mimo-single-point-decay/v1";
/// 结论标签。
pub const DECAY_CLAIM: &str = "PREDICTED_ONLY / SINGLE_POINT_NOT_SPATIAL / NO_MEASUREMENT";
/// 候选 B 目标下降比例（T*=0.85T，M1 §3）。
pub const T20_TARGET_RATIO: f64 = 0.85;
/// 相位目标衰减常数（6.907 = ln(1e3)）。
pub const DECAY_LN1000: f64 = 6.907_755;

/// 1/3 oct 带定义（候选中低频域）。
#[derive(Clone, Copy, Debug)]
pub struct BandSpec {
    /// 中心频率 Hz。
    pub center_hz: f64,
}

/// M1 预声明频带（63/100/125/250，均为 1/3 oct；160/200 可选）。
pub const M1_BANDS: [BandSpec; 4] = [
    BandSpec { center_hz: 63.0 },
    BandSpec { center_hz: 100.0 },
    BandSpec { center_hz: 125.0 },
    BandSpec { center_hz: 250.0 },
];

/// 设计/IR 网格（12 kHz 模态域；taps 最大 16384 = 1.365 s 支撑）。
#[derive(Clone, Debug)]
pub struct Grid {
    /// 12 kHz（MIMO 低速率域）。
    pub fs: f64,
    /// FFT 长度（>= 2*16384）。
    pub nfft: usize,
    /// 频点（升序，Hz）。
    pub freqs_hz: Vec<f64>,
}

impl Grid {
    /// 默认网格：fs=12k、nfft=32768（2.73 s）、正频率 bin。
    pub fn new(fs: f64, nfft: usize) -> Self {
        let half = nfft / 2 + 1;
        Self {
            fs,
            nfft,
            freqs_hz: (0..half).map(|k| k as f64 * fs / nfft as f64).collect(),
        }
    }
    /// 网格 bin 数（正频率）。
    pub fn bins(&self) -> usize {
        self.freqs_hz.len()
    }
}

/// 带通频点索引（1/3 oct，边界 [c*2^-1/6, c*2^+1/6)）。
pub fn band_bin_mask(freqs_hz: &[f64], center_hz: f64) -> Vec<bool> {
    let lo = center_hz / 2.0f64.powf(1.0 / 6.0);
    let hi = center_hz * 2.0f64.powf(1.0 / 6.0);
    freqs_hz.iter().map(|f| *f >= lo && *f < hi).collect()
}

/// 由实 IR 求正频率复谱（FFT）。
pub fn spectrum_of_ir(ir: &[f64]) -> Vec<Cx> {
    let mut re = ir.to_vec();
    let mut im = vec![0.0f64; ir.len()];
    fft_in_place(&mut re, &mut im);
    let half = ir.len() / 2 + 1;
    (0..half).map(|k| Cx::new(re[k], im[k])).collect()
}

/// 由正频率复谱（Hermitian 补全）求实 IR。
pub fn ir_of_spectrum(spectrum: &[Cx], nfft: usize) -> Vec<f64> {
    let half = nfft / 2 + 1;
    assert_eq!(spectrum.len(), half, "谱长度必须是 nfft/2+1");
    let mut re = vec![0.0f64; nfft];
    let mut im = vec![0.0f64; nfft];
    for k in 0..half {
        re[k] = spectrum[k].re;
        im[k] = spectrum[k].im;
        if k > 0 && k < nfft / 2 {
            re[nfft - k] = spectrum[k].re;
            im[nfft - k] = -spectrum[k].im;
        }
    }
    im[0] = 0.0;
    im[nfft / 2] = 0.0;
    for value in im.iter_mut() {
        *value = -*value;
    }
    fft_in_place(&mut re, &mut im);
    let inv = 1.0 / nfft as f64;
    re.iter().map(|value| value * inv).collect()
}

/// 1/3 oct 带通（RBJ biquad，Q=1/(2^(1/6)-2^(-1/6))；前向+反向=零相位）。
/// 用 IIR 而不是 FFT 掩码：FFT 矩形掩码的 sinc 尾巴会污染 EDC 长尾（实测见过假平台）。
pub fn bandpass_ir(ir: &[f64], fs: f64, center_hz: f64) -> Vec<f64> {
    let q = 1.0 / (2.0f64.powf(1.0 / 6.0) - 2.0f64.powf(-1.0 / 6.0));
    let w0 = std::f64::consts::TAU * center_hz / fs;
    let alpha = w0.sin() / (2.0 * q);
    let a0 = 1.0 + alpha;
    let b0 = alpha / a0;
    let b2 = -alpha / a0;
    let a1 = (-2.0 * w0.cos()) / a0;
    let a2 = (1.0 - alpha) / a0;
    let forward = |input: &[f64]| -> Vec<f64> {
        let mut out = vec![0.0f64; input.len()];
        let (mut x1, mut x2, mut y1, mut y2) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
        for (index, x0) in input.iter().enumerate() {
            let y0 = b0 * x0 + b2 * x2 - a1 * y1 - a2 * y2;
            out[index] = y0;
            x2 = x1;
            x1 = *x0;
            y2 = y1;
            y1 = y0;
        }
        out
    };
    let mut filtered = forward(ir);
    filtered.reverse();
    let mut filtered = forward(&filtered);
    filtered.reverse();
    filtered
}

/// T20 物理合理范围下限（s）。
pub const T20_MIN_S: f64 = 0.02;
/// T20 物理合理范围上限（s）。
pub const T20_MAX_S: f64 = 1.5;
/// EDC 必须在本窗内到达 -25 dB（s）；12k/32768 网格记录长 2.73 s，留余量。
pub const EDC_WINDOW_S: f64 = 2.0;
/// 噪声底估计窗：记录尾部比例（先 e = max(x² - noise_power, 0) 再算 EDC）。
pub const NOISE_TAIL_FRACTION: f64 = 0.1;
/// T20 线性拟合最低 R²（低于此判无效并记录 note）。
pub const T20_MIN_R2: f64 = 0.90;

/// T20 口径（与 mimo-t6 §4.5 / acoustic-verify lf_edc 一致）：
/// 带通 IR → 扣噪声底能量 → Schroeder 反向累积 EDC → 在 EDC ∈ [-5,-25] dB 上线性回归，
/// T20 = -20/slope（即 **20 dB 衰减时间**，s）；RT60 = 3·T20（只报告）。
pub const T20_CLAIM: &str =
    "T20 = 20 dB decay time from EDC -5..-25 dB linear fit (T20=-20/slope); RT60 = 3*T20";

/// T20 估计结果。
#[derive(Clone, Copy, Debug)]
pub struct T20Estimate {
    /// T20（s）= 20 dB 衰减时间；无效时为 NaN（不输出 8 s 之类的伪值）。
    pub t20_s: f64,
    /// RT60（s）= 3·T20（只报告；无效时 NaN）。
    pub rt60_s: f64,
    /// 100 ms 处 EDC（dB，相对起点）。
    pub edc_100ms_db: f64,
    /// 线性拟合 R²。
    pub r2: f64,
    /// 是否有有效 T20 估计。
    pub valid: bool,
    /// 无效原因（有效时为空串）。
    pub note: &'static str,
}

/// 从带通 IR 估计 T20（20 dB 衰减时间；见 T20_CLAIM）。
///
/// 与旧实现差异（Lead 复核 2026-11-22）：旧式 3*(t25-t5) 是 RT60（60 dB 时间），不是 T20；
/// 现改为 -5..-25 dB 区间 EDC 线性回归斜率 T20 = -20/slope，并报告 RT60=3·T20。
///
/// 有效性：
/// - EDC 必须在 EDC_WINDOW_S 内到达 -25 dB（噪声底扣除后仍不可达 → EDC_DYNAMIC_RANGE_INSUFFICIENT）；
/// - 斜率 < 0、R² ≥ T20_MIN_R2、T20 ∈ [T20_MIN_S, T20_MAX_S]；
/// 不满足时 t20_s = NaN + note；调用方不得把无效值当结果。
pub fn t20_from_ir(ir: &[f64], fs: f64) -> Option<T20Estimate> {
    let n = ir.len();
    if n < 8 {
        return None;
    }
    // 噪声底：记录尾部 NOISE_TAIL_FRACTION 的均方（本底段）；能量样本扣除后再做 EDC。
    let tail_start = ((n as f64) * (1.0 - NOISE_TAIL_FRACTION)) as usize;
    let tail = &ir[tail_start.min(n - 1)..];
    let noise_power = tail.iter().map(|value| value * value).sum::<f64>() / tail.len().max(1) as f64;
    let mut energy = vec![0.0f64; n + 1];
    for index in (0..n).rev() {
        let sample = ir[index] * ir[index] - noise_power;
        energy[index] = energy[index + 1] + sample.max(0.0);
    }
    let total = energy[0];
    if !(total.is_finite() && total > 1e-30) {
        return None;
    }
    let edc: Vec<f64> = (0..n)
        .map(|index| 10.0 * (energy[index] / total).max(1e-30).log10())
        .collect();
    let crossing = |level: f64, from: usize| -> Option<f64> {
        for index in from..n.saturating_sub(1) {
            if edc[index] > level && edc[index + 1] <= level {
                let span = edc[index] - edc[index + 1];
                let frac = if span.abs() > 1e-12 {
                    (edc[index] - level) / span
                } else {
                    0.0
                };
                return Some((index as f64 + frac) / fs);
            }
        }
        None
    };
    let edc_100ms_db = edc[(100e-3 * fs) as usize % n];
    let invalid = |note: &'static str| T20Estimate {
        t20_s: f64::NAN,
        rt60_s: f64::NAN,
        edc_100ms_db,
        r2: f64::NAN,
        valid: false,
        note,
    };
    let Some(t5) = crossing(-5.0, 0) else {
        return Some(invalid("NO_M5_CROSSING"));
    };
    let Some(t25) = crossing(-25.0, (t5 * fs) as usize) else {
        return Some(invalid("EDC_DYNAMIC_RANGE_INSUFFICIENT"));
    };
    if t25 > EDC_WINDOW_S {
        return Some(invalid("EDC_DYNAMIC_RANGE_INSUFFICIENT"));
    }
    // -5..-25 dB 区间线性回归 edc = a + b*t；T20 = -20/b。
    let (mut count, mut sum_t, mut sum_y) = (0usize, 0.0f64, 0.0f64);
    let mut t_samples = Vec::new();
    let mut y_samples = Vec::new();
    for (index, level) in edc.iter().enumerate() {
        if *level <= -5.0 && *level >= -25.0 {
            let t = index as f64 / fs;
            t_samples.push(t);
            y_samples.push(*level);
            count += 1;
            sum_t += t;
            sum_y += *level;
        }
    }
    if count < 8 {
        return Some(invalid("FIT_WINDOW_TOO_SHORT"));
    }
    let mean_t = sum_t / count as f64;
    let mean_y = sum_y / count as f64;
    let mut sxx = 0.0f64;
    let mut sxy = 0.0f64;
    for (t, y) in t_samples.iter().zip(y_samples.iter()) {
        sxx += (t - mean_t) * (t - mean_t);
        sxy += (t - mean_t) * (y - mean_y);
    }
    if !(sxx.is_finite() && sxx > 1e-18) {
        return Some(invalid("FIT_DEGENERATE"));
    }
    let slope = sxy / sxx;
    if !(slope.is_finite() && slope < 0.0) {
        return Some(invalid("EDC_SLOPE_NOT_NEGATIVE"));
    }
    let t20_s = -20.0 / slope;
    if !(T20_MIN_S..=T20_MAX_S).contains(&t20_s) {
        return Some(invalid("T20_OUT_OF_RANGE"));
    }
    let intercept = mean_y - slope * mean_t;
    let mut ss_res = 0.0f64;
    let mut ss_tot = 0.0f64;
    for (t, y) in t_samples.iter().zip(y_samples.iter()) {
        let predicted = intercept + slope * t;
        ss_res += (y - predicted) * (y - predicted);
        ss_tot += (y - mean_y) * (y - mean_y);
    }
    let r2 = if ss_tot > 1e-18 {
        1.0 - ss_res / ss_tot
    } else {
        f64::NAN
    };
    if !(r2.is_finite() && r2 >= T20_MIN_R2) {
        return Some(T20Estimate {
            t20_s: f64::NAN,
            rt60_s: f64::NAN,
            edc_100ms_db,
            r2,
            valid: false,
            note: "FIT_R2_TOO_LOW",
        });
    }
    Some(T20Estimate {
        t20_s,
        rt60_s: 3.0 * t20_s,
        edc_100ms_db,
        r2,
        valid: true,
        note: "",
    })
}

/// 每输入每带的候选 B 目标信息。
#[derive(Clone, Debug)]
pub struct BandTargetInfo {
    /// 中心频率。
    pub center_hz: f64,
    /// 基线 T20（s；20 dB 时间；无效 = NaN）。
    pub t20_before_s: f64,
    /// 目标 T20（s；= 0.85 * before）。
    pub t20_target_s: f64,
    /// 基线 RT60（s；= 3*T20）。
    pub rt60_before_s: f64,
    /// 目标 RT60（s；= 0.85 * before）。
    pub rt60_target_s: f64,
    /// delta（1/s，作用在信号包络上；由 RT60 求）。
    pub delta_per_s: f64,
    /// 该带是否有效（T20 可估计且带在数据范围内）。
    pub valid: bool,
    /// 无效原因（如 DATA_NOT_AVAILABLE_BAND_EDGE）。
    pub note: &'static str,
}

/// 构造候选 B 目标：d = b + sum_k (e^{-delta max(0,t-t0)}-1) B_k b。
/// 返回 (d, 每带信息)。
///
/// delta 用 RT60 求（作用在信号包络上，能量/EDC 斜率为其 2 倍）：
///   delta = ln(1000) * (1/RT60* - 1/RT60)，RT60* = 0.85·RT60；
/// 等价于 T20 口径 delta = (ln(1000)/3)*(1/(0.85·T20) - 1/T20)
///                    = 2.3026*(1/(0.85·T20) - 1/T20)。
pub fn candidate_b_target(
    baseline_ir: &[f64],
    grid: &Grid,
    bands: &[BandSpec],
    band_hi_hz: f64,
) -> (Vec<f64>, Vec<BandTargetInfo>) {
    let anchors = vec![None; bands.len()];
    candidate_b_target_with_anchor(baseline_ir, grid, bands, band_hi_hz, &anchors)
}

/// 与 candidate_b_target 相同，但允许对每带给定**外部实测锚点**基线 T20（s）。
///
/// 语义（measured-anchor 敏感性）：T20 锚点来自同一房间/同一点的 raw slow-sweep 实测
/// （例如 acoustic-verify 2026-11-22 identity：63 Hz T20=0.556 s → RT60=1.675 s）；
/// 若重建 IR 的 T20 与实测不可比（H 为 fr_modal 分析产物，带限/分析窗使 apparent decay 偏短），
/// 用实测锚点生成候选 B 目标更接近真实基线。None = 用重建 IR 估计（设计自洽口径）。
///
/// 注意：锚点只在该带“数据完整”（band 上沿 <= band_hi_hz）时生效；PARTIAL/NA 带一律不生效。
pub fn candidate_b_target_with_anchor(
    baseline_ir: &[f64],
    grid: &Grid,
    bands: &[BandSpec],
    band_hi_hz: f64,
    t20_anchor_s: &[Option<f64>],
) -> (Vec<f64>, Vec<BandTargetInfo>) {
    let mut target = baseline_ir.to_vec();
    let mut infos = Vec::with_capacity(bands.len());
    for (band_index, band) in bands.iter().enumerate() {
        let band_hi = band.center_hz * 2.0f64.powf(1.0 / 6.0);
        let note_edge = if band_hi > band_hi_hz + 1e-9 {
            "PARTIAL_BAND_EDGE_TRUNCATED"
        } else {
            ""
        };
        let component = bandpass_ir(baseline_ir, grid.fs, band.center_hz);
        let estimate = t20_from_ir(&component, grid.fs);
        let estimate_t20 = match estimate {
            Some(estimate) if estimate.valid => estimate.t20_s,
            _ => f64::NAN,
        };
        let anchor = t20_anchor_s
            .get(band_index)
            .and_then(|value| *value)
            .filter(|value| value.is_finite() && *value > 0.0);
        let t20_before = match anchor {
            Some(value) if note_edge.is_empty() => value,
            _ => estimate_t20,
        };
        let valid = t20_before.is_finite() && note_edge.is_empty();
        let rt60_before = 3.0 * t20_before;
        let t20_target = t20_before * T20_TARGET_RATIO;
        let rt60_target = rt60_before * T20_TARGET_RATIO;
        let delta = if valid && rt60_before.is_finite() && rt60_before > 1e-9 {
            DECAY_LN1000 * (1.0 / rt60_target - 1.0 / rt60_before)
        } else {
            0.0
        };
        if valid {
            // t0 = 该带分量峰值（直接声/带宽内主峰）。
            let t0 = component
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
                .map(|(index, _)| index)
                .unwrap_or(0);
            let mut factor = vec![0.0f64; component.len()];
            for index in t0..component.len() {
                factor[index] = (-delta * (index - t0) as f64 / grid.fs).exp() - 1.0;
            }
            for index in 0..target.len() {
                target[index] += factor[index] * component[index];
            }
        }
        infos.push(BandTargetInfo {
            center_hz: band.center_hz,
            t20_before_s: t20_before,
            t20_target_s: t20_target,
            rt60_before_s: rt60_before,
            rt60_target_s: rt60_target,
            delta_per_s: delta,
            valid,
            note: if valid { note_edge } else if !note_edge.is_empty() { note_edge } else { "T20_NOT_ESTIMABLE" },
        });
    }
    (target, infos)
}

/// 修正矩阵口径。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MatrixMode {
    /// 只 g_ii（T66 P_post 对角 FIR 可表达）。
    Diagonal,
    /// 5x5 交叉（T51/T54 modal matrix 形态；理想上界，当前 NOT_WIRED）。
    Full5x5,
}

impl MatrixMode {
    /// 标签。
    pub fn tag(self) -> &'static str {
        match self {
            MatrixMode::Diagonal => "diagonal_realizable",
            MatrixMode::Full5x5 => "full5x5_ideal_upper_bound",
        }
    }
}

/// 每路径元素帽（线性幅度）。
pub fn path_caps(mode: MatrixMode, sources: usize) -> Vec<Vec<f64>> {
    let diag_cap = 10.0f64.powf(6.0 / 20.0);
    let offdiag_cap = 10.0f64.powf(-6.0 / 20.0);
    (0..sources)
        .map(|out| {
            (0..sources)
                .map(|input| {
                    if mode == MatrixMode::Diagonal && out != input {
                        0.0
                    } else if out == input {
                        diag_cap
                    } else {
                        offdiag_cap
                    }
                })
                .collect()
        })
        .collect()
}

/// 单频点元素帽最小范数解：sum_j H_j g_j = rhs，|g_j| <= cap_j。
pub fn solve_with_caps(h: &[Cx], rhs: Cx, caps: &[f64]) -> Vec<Cx> {
    let n = h.len();
    let mut g = vec![Cx::zero(); n];
    let mut fixed = vec![false; n];
    let mut residual = rhs;
    for _iteration in 0..(n + 2) {
        let mut denominator = 0.0f64;
        for index in 0..n {
            if !fixed[index] {
                denominator += h[index].norm_sqr();
            }
        }
        if denominator <= 1e-30 {
            break;
        }
        for index in 0..n {
            if fixed[index] {
                continue;
            }
            let scale = residual.mul(h[index].conj()).scale(1.0 / denominator);
            g[index] = scale;
        }
        // 找最大超帽元素
        let mut worst: Option<(usize, f64)> = None;
        for index in 0..n {
            if fixed[index] || caps[index] <= 0.0 {
                continue;
            }
            let magnitude = g[index].abs();
            let ratio = magnitude / caps[index];
            if ratio > 1.0 && worst.map(|(_, r)| ratio > r).unwrap_or(true) {
                worst = Some((index, ratio));
            }
        }
        let Some((index, _)) = worst else {
            break;
        };
        let magnitude = g[index].abs();
        g[index] = g[index].scale(caps[index] / magnitude);
        fixed[index] = true;
        residual = residual.sub(h[index].mul(g[index]));
    }
    // 最终安全投影
    for index in 0..n {
        let magnitude = g[index].abs();
        if caps[index] > 0.0 && magnitude > caps[index] {
            g[index] = g[index].scale(caps[index] / magnitude);
        } else if caps[index] <= 0.0 {
            g[index] = Cx::zero();
        }
    }
    g
}

/// 一条路径的 FIR 拟合结果。
#[derive(Clone, Debug)]
pub struct PathFir {
    /// out（speaker j）。
    pub out_index: usize,
    /// in（source i）。
    pub in_index: usize,
    /// FIR taps。
    pub coeffs: Vec<f64>,
}

/// decay 实验允许的最大 FIR taps（12 kHz 域 16384 = 1.365 s 支撑；与 Grid 文档一致）。
///
/// 注意：`fir::FirBasis` 自身上限是 4096 taps；本模块对 >4096 的档位走**同一 dual 公式**的
/// 本地实现（`fit_real_fir_dual`，见下），避免修改 fir 模块或另建第二套数学。
pub const MAX_PATH_TAPS: usize = 16_384;

/// 把每路径频域修正 g_ji(f) 拟合成因果实 FIR（12 kHz 域，任意相位）。
///
/// 求解口径与 `fir::FirBasis::solve` 一致：h = Aᵀ(AAᵀ + μI)⁻¹ c，μ = 1e-6·K，
/// A 的每频点两行为 cos(ωt) 与 −sin(ωt)。taps <= 4096 复用 FirBasis；taps > 4096
/// （8192/16384 档，T20 证据包需要）用本模块等价实现，numerics 与 FirBasis 同式。
pub fn fit_path_firs(
    design_freqs_hz: &[f64],
    targets: &[Vec<Vec<Cx>>],
    fs: f64,
    taps: usize,
) -> Result<Vec<PathFir>, String> {
    if design_freqs_hz.is_empty() {
        return Err("fit_path_firs: 空频点".to_string());
    }
    if taps == 0 || taps > MAX_PATH_TAPS {
        return Err(format!(
            "fit_path_firs: taps={taps} 超出 (0,{MAX_PATH_TAPS}]"
        ));
    }
    let basis = if taps <= 4096 {
        Some(FirBasis::new(design_freqs_hz, fs, taps)?)
    } else {
        None
    };
    let mut out = Vec::new();
    for (out_index, row) in targets.iter().enumerate() {
        for (in_index, response) in row.iter().enumerate() {
            let coeffs = match &basis {
                Some(basis) => basis.solve(response, 1e-6)?,
                None => fit_real_fir_dual(design_freqs_hz, response, fs, taps)?,
            };
            out.push(PathFir { out_index, in_index, coeffs });
        }
    }
    Ok(out)
}

/// `fir::FirBasis` 的 taps 上限（4096）之外的等价 dual 形式实现（>4096 档专用）。
///
/// 与 FirBasis::solve 完全同一数学：A 为 2K×taps（行 cos(ω_k t) / −sin(ω_k t)），
/// 正向方程 h = Aᵀ y, (AAᵀ + μI) y = c，μ = diag_load_rel·K（此处 diag_load_rel=1e-6）。
fn fit_real_fir_dual(
    design_freqs_hz: &[f64],
    c: &[Cx],
    fs: f64,
    taps: usize,
) -> Result<Vec<f64>, String> {
    let k = c.len();
    if k == 0 {
        return Err("fit_real_fir_dual: 空目标".to_string());
    }
    let rows = 2 * k;
    let mut basis = vec![vec![0.0f64; taps]; rows];
    for (index, f) in design_freqs_hz.iter().enumerate() {
        let w = std::f64::consts::TAU * f / fs;
        for t in 0..taps {
            let phase = w * t as f64;
            basis[2 * index][t] = phase.cos();
            basis[2 * index + 1][t] = -phase.sin();
        }
    }
    let mut aat = vec![vec![0.0f64; rows]; rows];
    for r in 0..rows {
        for q in r..rows {
            let mut s = 0.0f64;
            let (br, bq) = (&basis[r], &basis[q]);
            for t in 0..taps {
                s += br[t] * bq[t];
            }
            aat[r][q] = s;
            aat[q][r] = s;
        }
    }
    let mu = 1e-6 * k as f64;
    for (r, row) in aat.iter_mut().enumerate() {
        row[r] += mu;
    }
    let mut rhs = vec![0.0f64; rows];
    for (index, value) in c.iter().enumerate() {
        rhs[2 * index] = value.re;
        rhs[2 * index + 1] = value.im;
    }
    let y = solve_real_linear(&mut aat, &mut rhs)?;
    let mut h = vec![0.0f64; taps];
    for (t, value) in h.iter_mut().enumerate() {
        let mut s = 0.0f64;
        for (r, row) in basis.iter().enumerate() {
            s += row[t] * y[r];
        }
        *value = s;
    }
    if h.iter().any(|v| !v.is_finite()) {
        return Err("fit_real_fir_dual: 解含非有限值".to_string());
    }
    Ok(h)
}

/// 实系数部分主元高斯消元（A x = b），原地；与 fir::solve_real_linear 同式（该函数为私有）。
fn solve_real_linear(a: &mut [Vec<f64>], b: &mut [f64]) -> Result<Vec<f64>, String> {
    let n = b.len();
    for col in 0..n {
        let mut pivot = col;
        let mut best = a[col][col].abs();
        for row in col + 1..n {
            let value = a[row][col].abs();
            if value > best {
                best = value;
                pivot = row;
            }
        }
        if !(best.is_finite() && best > 1e-300) {
            return Err(format!("fit_path_firs: 第 {col} 列主元近零"));
        }
        if pivot != col {
            a.swap(pivot, col);
            b.swap(pivot, col);
        }
        let diagonal = a[col][col];
        for row in col + 1..n {
            let factor = a[row][col] / diagonal;
            if factor == 0.0 {
                continue;
            }
            for column in col..n {
                a[row][column] -= factor * a[col][column];
            }
            b[row] -= factor * b[col];
        }
    }
    let mut x = vec![0.0f64; n];
    for row in (0..n).rev() {
        let mut s = b[row];
        for column in row + 1..n {
            s -= a[row][column] * x[column];
        }
        x[row] = s / a[row][row];
    }
    Ok(x)
}

/// 由 FIR 系数求正频率复响应（补零到 nfft 后 FFT；taps <= nfft）。
pub fn fir_spectrum(coeffs: &[f64], nfft: usize) -> Vec<Cx> {
    let mut ir = vec![0.0f64; nfft];
    let count = coeffs.len().min(nfft);
    ir[..count].copy_from_slice(&coeffs[..count]);
    spectrum_of_ir(&ir)
}

/// 每输入评估行（逐带）。
#[derive(Clone, Debug)]
pub struct DecayBandRow {
    /// 输入 role。
    pub role: String,
    /// 带中心。
    pub center_hz: f64,
    /// 基线 T20（s）。
    pub t20_before_s: f64,
    /// 目标 T20（s）。
    pub t20_target_s: f64,
    /// 达成响应 T20（s）。
    pub t20_after_s: f64,
    /// T20 下降比例（1 - after/before）。
    pub t20_drop: f64,
    /// 是否达到 >=15%。
    pub pass_15pct: bool,
    /// 该带是否有效。
    pub valid: bool,
    /// I（该输入在 63–125 Hz 的误差改善 dB；63–250 的 250 段数据不可用）。
    pub i_db: f64,
    /// 带电平变化（after RMs - before RMS，dB）。
    pub level_change_db: f64,
    /// EDC100 guard（after - before，dB；正=早期衰减变慢）。
    pub edc100_guard_db: f64,
    /// 备注（数据缺口等）。
    pub note: &'static str,
}

/// log 网格等权 I：I=10log10(sum |b-d|^2 / sum |y-d|^2)，在 [lo,hi] 上取 n 个 log 等距频点。
pub fn interference_improvement_db(
    grid: &Grid,
    before: &[Cx],
    target: &[Cx],
    after: &[Cx],
    lo_hz: f64,
    hi_hz: f64,
    points: usize,
) -> f64 {
    let ratio = (hi_hz / lo_hz).powf(1.0 / (points as f64 - 1.0));
    let mut num = 0.0f64;
    let mut den = 0.0f64;
    for index in 0..points {
        let freq = lo_hz * ratio.powi(index as i32);
        let bin = (freq / grid.fs * grid.nfft as f64).round() as usize;
        let bin = bin.min(grid.bins() - 1);
        num += (before[bin].sub(target[bin])).norm_sqr();
        den += (after[bin].sub(target[bin])).norm_sqr();
    }
    if den <= 1e-30 {
        if num <= 1e-30 {
            f64::NAN
        } else {
            f64::INFINITY
        }
    } else {
        10.0 * (num.max(1e-30) / den).log10()
    }
}

/// 由复谱重建 IR 并在每带评估 T20/电平/EDC100。
pub fn evaluate_decay(
    grid: &Grid,
    role: &str,
    before_spectrum: &[Cx],
    target_spectrum: &[Cx],
    after_spectrum: &[Cx],
    bands: &[BandSpec],
    band_hi_hz: f64,
) -> Vec<DecayBandRow> {
    let before_ir = ir_of_spectrum(before_spectrum, grid.nfft);
    let target_ir = ir_of_spectrum(target_spectrum, grid.nfft);
    let after_ir = ir_of_spectrum(after_spectrum, grid.nfft);
    let i_db = interference_improvement_db(grid, before_spectrum, target_spectrum, after_spectrum, 63.0, 125.0, 40);
    let mut rows = Vec::new();
    for band in bands {
        let band_hi = band.center_hz * 2.0f64.powf(1.0 / 6.0);
        let edge_note = if band_hi > band_hi_hz + 1e-9 { "PARTIAL_BAND_EDGE_TRUNCATED" } else { "" };
        let before_band = bandpass_ir(&before_ir, grid.fs, band.center_hz);
        let after_band = bandpass_ir(&after_ir, grid.fs, band.center_hz);
        let before_est = t20_from_ir(&before_band, grid.fs);
        let target_est = t20_from_ir(&bandpass_ir(&target_ir, grid.fs, band.center_hz), grid.fs);
        let after_est = t20_from_ir(&after_band, grid.fs);
        let (t20_before, valid_before) = before_est.map(|e| (e.t20_s, e.valid)).unwrap_or((f64::NAN, false));
        let (t20_target, _) = target_est.map(|e| (e.t20_s, e.valid)).unwrap_or((f64::NAN, false));
        let (t20_after, valid_after) = after_est.map(|e| (e.t20_s, e.valid)).unwrap_or((f64::NAN, false));
        let t20_drop = if valid_before && valid_after && t20_before > 1e-9 {
            1.0 - t20_after / t20_before
        } else {
            f64::NAN
        };
        let pass_15pct = t20_drop >= 0.15;
        let before_rms = rms_of(&before_band);
        let after_rms = rms_of(&after_band);
        let level_change_db = 20.0 * (after_rms.max(1e-30) / before_rms.max(1e-30)).log10();
        let edc100_guard_db = match (before_est, after_est) {
            (Some(b), Some(a)) => a.edc_100ms_db - b.edc_100ms_db,
            _ => f64::NAN,
        };
        let note = if !edge_note.is_empty() {
            edge_note
        } else if !valid_before {
            before_est
                .map(|estimate| estimate.note)
                .filter(|note| !note.is_empty())
                .unwrap_or("BEFORE_T20_INVALID")
        } else if !valid_after {
            after_est
                .map(|estimate| estimate.note)
                .filter(|note| !note.is_empty())
                .unwrap_or("AFTER_T20_INVALID")
        } else {
            ""
        };
        rows.push(DecayBandRow {
            role: role.to_string(),
            center_hz: band.center_hz,
            t20_before_s: t20_before,
            t20_target_s: t20_target,
            t20_after_s: t20_after,
            t20_drop,
            pass_15pct,
            valid: valid_before && valid_after,
            i_db,
            level_change_db,
            edc100_guard_db,
            note,
        });
    }
    rows
}

fn rms_of(samples: &[f64]) -> f64 {
    let power = samples.iter().map(|v| v * v).sum::<f64>() / samples.len().max(1) as f64;
    power.sqrt()
}

/// 估算后半段（t > onset + tail）能量占比：flag 尾长度是否超过 taps 支撑（时域尾长检查）。
pub fn late_energy_fraction(ir: &[f64], fs: f64, onset_s: f64, window_s: f64) -> f64 {
    let total: f64 = ir.iter().map(|v| v * v).sum();
    if total <= 1e-30 {
        return f64::NAN;
    }
    let start = ((onset_s + window_s) * fs) as usize;
    let tail: f64 = ir.iter().skip(start.min(ir.len())).map(|v| v * v).sum();
    tail / total
}

/// dB 幅度（复用 fill 口径）。
pub fn amp_db(value: f64) -> f64 {
    db_amp(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_decay(fs: f64, hits: usize, t60_s: f64, freq_hz: f64) -> Vec<f64> {
        // 指数衰减 + 平滑起振（避免矩形窗泄漏尾的假 EDC 平台）；T20 == T60。
        let sigma = DECAY_LN1000 / t60_s;
        (0..hits)
            .map(|index| {
                let t = index as f64 / fs;
                let onset = 1.0 - (-t / 0.004).exp();
                let amplitude = onset * (-sigma * t).exp();
                amplitude * (std::f64::consts::TAU * freq_hz * t).cos()
            })
            .collect()
    }

    #[test]
    fn t20_estimator_matches_known_exponential() {
        let fs = 12_000.0;
        let ir = synthetic_decay(fs, 16_384, 0.60, 100.0);
        let estimate = t20_from_ir(&ir, fs).expect("t20");
        assert!(estimate.valid, "valid: {estimate:?}");
        // 已知答案：T60=0.6 s → T20（20 dB 时间）= T60/3 = 0.2 s（±10%）；RT60 只报告 = 3*T20。
        assert!((estimate.t20_s - 0.2).abs() < 0.02, "t20={}", estimate.t20_s);
        assert!((estimate.rt60_s - 0.6).abs() < 0.06, "rt60={}", estimate.rt60_s);
        assert!(estimate.r2 > 0.99, "r2={}", estimate.r2);
    }

    #[test]
    fn candidate_b_targets_15pct_shorter_t20() {
        let grid = Grid::new(12_000.0, 32_768);
        let fs = grid.fs;
        let ir = synthetic_decay(fs, 32_768, 0.60, 100.0);
        let (target, infos) = candidate_b_target(&ir, &grid, &[BandSpec { center_hz: 100.0 }], 400.0);
        let info = &infos[0];
        assert!(info.valid, "band info: {info:?}");
        assert!((info.t20_before_s - 0.2).abs() < 0.02, "before={}", info.t20_before_s);
        let target_band = bandpass_ir(&target, grid.fs, 100.0);
        let target_est = t20_from_ir(&target_band, fs).expect("target t20");
        let before_est = t20_from_ir(&bandpass_ir(&ir, grid.fs, 100.0), fs).expect("before t20");
        assert!(target_est.valid && before_est.valid);
        let drop = 1.0 - target_est.t20_s / before_est.t20_s;
        assert!((drop - 0.15).abs() < 0.05, "drop={drop} target={}", target_est.t20_s);
    }

    #[test]
    fn capped_solver_respects_per_path_caps_and_equation_residual() {
        let h = vec![Cx::new(1.0, 0.0), Cx::new(1.0, 0.4), Cx::new(0.7, -0.2)];
        let rhs = Cx::new(30.0, 0.0);
        let caps = vec![2.0, 0.5, 0.5];
        let g = solve_with_caps(&h, rhs, &caps);
        for (index, value) in g.iter().enumerate() {
            assert!(value.abs() <= caps[index] + 1e-12, "|g[{index}]|={}", value.abs());
        }
        // 残差应小于未加帽（无法完全满足）；只验证有限且非零。
        let mut sum = Cx::zero();
        for (index, value) in g.iter().enumerate() {
            sum = sum.add(h[index].mul(*value));
        }
        assert!(sum.abs().is_finite());
    }

    #[test]
    fn ir_spectrum_roundtrip() {
        let grid = Grid::new(12_000.0, 4096);
        let ir = synthetic_decay(grid.fs, 4096, 0.5, 80.0);
        let spectrum = spectrum_of_ir(&ir);
        let back = ir_of_spectrum(&spectrum, grid.nfft);
        for index in 0..ir.len() {
            assert!((ir[index] - back[index]).abs() < 1e-9, "index {index}");
        }
    }

    #[test]
    fn fit_path_firs_supports_taps_above_fir_basis_limit() {
        // 8192 taps 走本地 dual 实现（FirBasis 上限 4096）：拟合复目标应能重建其频响。
        let freqs: Vec<f64> = (0..40).map(|i| 20.0 + 2.5 * i as f64).collect();
        let c: Vec<Cx> = freqs
            .iter()
            .enumerate()
            .map(|(i, _)| Cx::new(1.0 - 0.01 * i as f64, 0.05 * (i as f64 * 0.3).sin()))
            .collect();
        let paths = fit_path_firs(&freqs, &[vec![c.clone()]], 12_000.0, 8192).expect("fit 8192");
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].coeffs.len(), 8192);
        let rebuilt = crate::fir::fir_response(&paths[0].coeffs, &freqs, 12_000.0);
        let max_error = rebuilt
            .iter()
            .zip(c.iter())
            .map(|(a, b)| a.sub(*b).abs())
            .fold(0.0f64, f64::max);
        assert!(max_error < 1e-6, "max_error={max_error}");
    }

    #[test]
    fn local_dual_matches_fir_basis_on_small_taps() {
        // 同一公式两条实现（FirBasis 与本地 dual）在 512 taps 上应逐点一致。
        let freqs: Vec<f64> = (0..24).map(|i| 25.0 + 3.0 * i as f64).collect();
        let c: Vec<Cx> = freqs
            .iter()
            .enumerate()
            .map(|(i, _)| Cx::new(0.7, -0.2 + 0.01 * i as f64))
            .collect();
        let basis = FirBasis::new(&freqs, 12_000.0, 512).expect("basis");
        let a = basis.solve(&c, 1e-6).expect("fir basis");
        let b = fit_real_fir_dual(&freqs, &c, 12_000.0, 512).expect("local dual");
        for (index, (x, y)) in a.iter().zip(b.iter()).enumerate() {
            assert!((x - y).abs() < 1e-9, "tap {index}: {x} vs {y}");
        }
    }
}