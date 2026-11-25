//! 10 段模态 PEQ（T41；允许 IIR）——RBJ biquad 语义、shelf S/Q、系数导出与“原图比对”。
//!
//! 权威契约（只读输入）：
//! - rt-stack/spec/fir-toolbox.md §5（v1.2-draft）：数据模型/界限、RBJ 公式、f64/f32 精度、
//!   `tspi-modal-peq/v1` 导出 schema、“原图比对”§5.6、IIR 不硬截 FIR §5.7；
//! - calibration-spec/CONTRACT.md §3.4/§10（v1.1）：peaking `q`=Q，shelf `q`=RBJ slope S，
//!   shelf `slope=12*S`（CamillaDSP Slope 型映射）；
//! - specs/peq-optimizer/requirements.md 需求 5/6/8：模态检测（突出度、−3 dB 带宽反算 Q、
//!   `q_unresolved`、只切、合并、≤10 段）。
//!
//! 本文件是 T41 的 leaf 实现，**不是** RT-MODULE：设计器为离线 f64 工具，允许分配/Vec/日志。
//! 私有测试在 `crates/designer/tests/T41/modal_peq_cases.rs`，通过下面的 `include!` 在
//! `#[cfg(test)]` 下编译执行（见 tests/T41/README.md；不改 integrator 独占的 public_registration.rs）。
//!
//! 状态：T41 leaf 实现 + 真实 `cargo test -p khaen-designer modal_peq` 执行（见 evidence/T41/）。
//! 未做：板端/ALSA/RT 实测、真实房间数据、与 T64 daemon 的集成（NOT_RUN）。

use std::fmt;

/// 导出 JSON schema 名（spec §5.5）。
pub const MODAL_PEQ_SCHEMA: &str = "tspi-modal-peq/v1";

/// 每通道模态 peaking 段上限（“10 段模态 PEQ”定义；spec §5.2）。
pub const MAX_MODAL_PEAKINGS: usize = 10;
/// 每通道 biquad 总数硬上限（CONTRACT v1.1 §3.4/§10）。
pub const MAX_BIQUADS_PER_CHANNEL: usize = 32;
/// 生产端每个 shelf 类型约定上限（spec §5.2/I06；消费方不得据此假设）。
pub const MAX_SHELVES_PER_TYPE_PRODUCER: usize = 1;

/// 频带与增益/精度界限（spec §5.2/§5.4）。
pub const FREQ_MIN_HZ: f64 = 20.0;
pub const FREQ_MAX_HZ: f64 = 20000.0;
pub const SINGLE_BAND_GAIN_LIMIT_DB: f64 = 24.0;
pub const PEAK_Q_MIN: f64 = 0.1;
pub const SHELF_S_MIN: f64 = 0.1;
pub const SHELF_S_MAX: f64 = 1.0;
pub const POLE_RADIUS_MAX: f64 = 0.999999;

/// 模态段恒只切（spec §5.2/I05）：peaking 模态增益恒 ≤ 0。
pub const MODAL_BOOST_CAP_DB: f64 = 0.0;
/// 宽带/非模态段正增益默认上限与显式 boost 上限（spec §5.2；requirements 6 AC4）。
pub const WIDEBAND_BOOST_DEFAULT_CAP_DB: f64 = 0.0;
pub const WIDEBAND_BOOST_MAX_CAP_DB: f64 = 3.0;
/// 非模态正向总量另受 ≤12 dB 硬限、>6 dB warning（spec §5.2）。
pub const WIDEBAND_POSITIVE_TOTAL_LIMIT_DB: f64 = 12.0;
pub const WIDEBAND_POSITIVE_WARN_DB: f64 = 6.0;

/// §5.4 固定评估网格：20 Hz–20 kHz、2001 点 `f_k = 20*1000^(k/2000)`（约 1/200 oct）。
pub const EVAL_GRID_POINTS: usize = 2001;
/// §5.6.1 自洽网格：1/48 oct。
pub const SELF_CONSISTENCY_OCTAVE_RES: f64 = 1.0 / 48.0;
/// §5.4 f32 量化复算容差：超过则 `requires_f64=true`。
pub const F32_QUANTIZATION_TOLERANCE_DB: f64 = 0.1;

/// §5.7 FIR 尾长判据：tail_peak ≤ −100 dB re 总峰值、tail_energy ≤ −80 dB re 总能量。
pub const FIR_TAIL_PEAK_LIMIT_DB: f64 = -100.0;
pub const FIR_TAIL_ENERGY_LIMIT_DB: f64 = -80.0;
/// L_req 估计用幅度阈值 1e-5（spec §5.7：`L ≈ ceil(log(1e-5)/log(r))`）。
pub const FIR_TAIL_AMPLITUDE_EPS: f64 = 1e-5;
/// 尾窗定义：候选长度 L 的最后 1/8（文档化解释，spec 未给窗口宽度）。
pub const FIR_TAIL_WINDOW_FRACTION: f64 = 0.125;

/// 模态搜索带默认值（requirements 5 AC5：缺省 20–250 Hz）。
pub const DEFAULT_MODAL_BAND_HZ: [f64; 2] = [20.0, 250.0];
/// 模态检测：默认 1/24 oct 平滑、1/48 oct 分析网格、突出度 ≥3 dB、最大切 12 dB、目标余量 1 dB。
pub const DETECT_SMOOTH_OCT: f64 = 1.0 / 24.0;
pub const DETECT_ANALYSIS_OCT: f64 = 1.0 / 48.0;
pub const DETECT_MIN_PROMINENCE_DB: f64 = 3.0;
pub const DETECT_MAX_CUT_DB: f64 = 12.0;
pub const DETECT_TARGET_MARGIN_DB: f64 = 1.0;
pub const DETECT_MERGE_OCT: f64 = 1.0 / 6.0;
pub const DETECT_MIN_Q: f64 = 0.1;

// ---------------------------------------------------------------------------
// 错误码（spec §5 明确码 + T41 实现层补充码；补充码在 evidence/T41/MANIFEST.md 登记）
// ---------------------------------------------------------------------------

/// 未知 `type`（spec §5.2：整份拒绝，不降级）。
pub const E_PEQ_TYPE: &str = "E_PEQ_TYPE";
/// 极点半径 > 0.999999（spec §5.4）。
pub const E_PEQ_UNSTABLE: &str = "E_PEQ_UNSTABLE";
/// IIR 转 FIR 尾长不满足（spec §5.7：拒绝并给出所需长度估计）。
pub const E_PEQ_FIR_TAIL: &str = "E_PEQ_FIR_TAIL";
/// 频带非法（freq/fs/band_hz）。
pub const E_PEQ_FREQ: &str = "E_PEQ_FREQ";
/// 单段增益越过 ±24 dB。
pub const E_PEQ_GAIN: &str = "E_PEQ_GAIN";
/// Q/S 非法（peaking Q<0.1；shelf S ∉ [0.1,1.0]）。
pub const E_PEQ_Q: &str = "E_PEQ_Q";
/// 段数/通道数越界（≤10 模态、shelf≤1、总数≤32、channels≤32）。
pub const E_PEQ_LIMIT: &str = "E_PEQ_LIMIT";
/// 模态段正增益或宽带正增益超帽（只切/boost cap）。
pub const E_PEQ_BOOST: &str = "E_PEQ_BOOST";
/// 系数与 band 字段不一致（导出/装载自洽失败）。
pub const E_PEQ_COEFF: &str = "E_PEQ_COEFF";
/// 数值非有限或 fs 非契约采样率。
pub const E_PEQ_NUMERIC: &str = "E_PEQ_NUMERIC";
/// 采样率不在 CONTRACT 集合 {44100,48000,88200,96000}。
pub const E_PEQ_FS: &str = "E_PEQ_FS";
/// 通道名非法（CONTRACT §5 规则 3：`^[A-Z][A-Z0-9]*$`）。
pub const E_PEQ_NAME: &str = "E_PEQ_NAME";
/// 通道集合非法（空/重复 name/重复 index）。
pub const E_PEQ_CHANNELS: &str = "E_PEQ_CHANNELS";
/// 曲线非法（点数、排序、有限性、覆盖范围）——检测/原图比对的输入错误。
pub const E_PEQ_CURVE: &str = "E_PEQ_CURVE";
/// 设计带（band_hz）非法。
pub const E_PEQ_BAND: &str = "E_PEQ_BAND";
/// 导出 JSON 序列化/装载错误。
pub const E_PEQ_SCHEMA: &str = "E_PEQ_SCHEMA";

// ---------------------------------------------------------------------------
// 错误类型
// ---------------------------------------------------------------------------

/// 设计器错误：稳定错误码 + 可读消息。
#[derive(Debug, Clone, PartialEq)]
pub struct PeqError {
    pub code: &'static str,
    pub message: String,
}

impl PeqError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for PeqError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for PeqError {}

fn err<T>(code: &'static str, message: impl Into<String>) -> Result<T, PeqError> {
    Err(PeqError::new(code, message))
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

/// 契约允许的采样率集合（CONTRACT §5 规则 2）。
pub fn contract_sample_rates() -> [u32; 4] {
    [44100, 48000, 88200, 96000]
}

/// 契约采样率校验（E_PEQ_FS）。
pub fn validate_sample_rate(fs: u32) -> Result<(), PeqError> {
    if contract_sample_rates().contains(&fs) {
        Ok(())
    } else {
        err(
            E_PEQ_FS,
            format!("sample_rate {fs} 不在 CONTRACT 集合 {{44100,48000,88200,96000}}"),
        )
    }
}

/// 频率上界：`min(20000, fs/2 − 1)`（spec §5.2）。
pub fn freq_upper_hz(fs: f64) -> f64 {
    (fs / 2.0 - 1.0).min(FREQ_MAX_HZ)
}

/// 两个频率之间相差多少 octave（|log2(f2/f1)|）。
pub fn octave_distance(f1: f64, f2: f64) -> f64 {
    (f2 / f1).log2().abs()
}

/// 频率转 octave（相对 `f_ref`）。
pub fn hz_to_oct(f: f64, f_ref: f64) -> f64 {
    (f / f_ref).log2()
}

/// 对数频率网格：`lo` 起步、倍率 `2^resolution`，末点精确为 `hi`。
pub fn log_frequency_grid(lo_hz: f64, hi_hz: f64, octave_resolution: f64) -> Vec<f64> {
    let mut out = Vec::new();
    if !(lo_hz.is_finite() && hi_hz.is_finite()) || lo_hz <= 0.0 || hi_hz <= lo_hz {
        return out;
    }
    let res = octave_resolution.max(1e-6);
    let ratio = 2f64.powf(res);
    let mut f = lo_hz;
    while f < hi_hz * (1.0 - 1e-12) {
        out.push(f);
        f *= ratio;
    }
    out.push(hi_hz);
    out
}

/// spec §6.8 `tilt10` 内置目标：20 Hz 0 dB → 20 kHz −10 dB，log-f 线性（V8）。
pub fn tilt10_db(freq_hz: f64) -> f64 {
    -10.0 * (freq_hz / 20.0).log10() / 3.0
}

// ---------------------------------------------------------------------------
// 频响曲线
// ---------------------------------------------------------------------------

/// 一个频响点（Hz / dB）。输入曲线（measurement 或 target）按 freq 严格升序。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurvePoint {
    pub freq_hz: f64,
    pub mag_db: f64,
}

impl CurvePoint {
    pub fn new(freq_hz: f64, mag_db: f64) -> Self {
        Self { freq_hz, mag_db }
    }
}

/// 校验曲线：≥2 点、有限、freq>0 且严格升序（E_PEQ_CURVE）。
pub fn validate_curve(curve: &[CurvePoint]) -> Result<(), PeqError> {
    if curve.len() < 2 {
        return err(
            E_PEQ_CURVE,
            format!("曲线至少需要 2 点，实际 {}", curve.len()),
        );
    }
    let mut prev = f64::NEG_INFINITY;
    for (i, p) in curve.iter().enumerate() {
        if !p.freq_hz.is_finite() || !p.mag_db.is_finite() {
            return err(E_PEQ_CURVE, format!("曲线第 {i} 点含 NaN/Inf"));
        }
        if p.freq_hz <= 0.0 {
            return err(
                E_PEQ_CURVE,
                format!("曲线第 {i} 点 freq={} 非正", p.freq_hz),
            );
        }
        if !(p.freq_hz > prev) {
            return err(
                E_PEQ_CURVE,
                format!("曲线第 {i} 点 freq={} 未严格升序", p.freq_hz),
            );
        }
        prev = p.freq_hz;
    }
    Ok(())
}

/// 对数频率线性 dB 插值（端点外 hold；spec §6.7 的端点行为用于 target，测量曲线沿用同口径）。
pub fn interpolate_curve_db(curve: &[CurvePoint], freq_hz: f64) -> Result<f64, PeqError> {
    validate_curve(curve)?;
    if !freq_hz.is_finite() || freq_hz <= 0.0 {
        return err(E_PEQ_CURVE, format!("插值频率 {freq_hz} 非法"));
    }
    let n = curve.len();
    if freq_hz <= curve[0].freq_hz {
        return Ok(curve[0].mag_db);
    }
    if freq_hz >= curve[n - 1].freq_hz {
        return Ok(curve[n - 1].mag_db);
    }
    let mut lo = 0usize;
    let mut hi = n - 1;
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if curve[mid].freq_hz <= freq_hz {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let f0 = curve[lo].freq_hz;
    let f1 = curve[hi].freq_hz;
    let t = (freq_hz / f0).ln() / (f1 / f0).ln();
    Ok(curve[lo].mag_db + (curve[hi].mag_db - curve[lo].mag_db) * t)
}

/// 输入曲线在 `freq_hz` 处的局部网格步长（Hz）；越界用最近区间的步长。
pub fn local_grid_step_hz(curve: &[CurvePoint], freq_hz: f64) -> Result<f64, PeqError> {
    validate_curve(curve)?;
    let n = curve.len();
    if freq_hz <= curve[0].freq_hz {
        return Ok(curve[1].freq_hz - curve[0].freq_hz);
    }
    if freq_hz >= curve[n - 1].freq_hz {
        return Ok(curve[n - 1].freq_hz - curve[n - 2].freq_hz);
    }
    let mut lo = 0usize;
    let mut hi = n - 1;
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if curve[mid].freq_hz <= freq_hz {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Ok((curve[hi].freq_hz - curve[lo].freq_hz).max(f64::MIN_POSITIVE))
}

// ---------------------------------------------------------------------------
// RBJ alpha（spec §5.3 规范公式；公开供 T64/T55 复用）
// ---------------------------------------------------------------------------

/// Peaking：`alpha = sin(w0)/(2Q)`。
pub fn peaking_alpha(w0: f64, q: f64) -> f64 {
    w0.sin() / (2.0 * q)
}

/// Shelf：`alpha = sin(w0)/2 * sqrt((A + 1/A)*(1/S − 1) + 2)`，S 是 RBJ slope。
pub fn shelf_alpha(w0: f64, a_linear_gain: f64, s: f64) -> f64 {
    let inner = (a_linear_gain + 1.0 / a_linear_gain) * (1.0 / s - 1.0) + 2.0;
    w0.sin() / 2.0 * inner.sqrt()
}

/// `A = 10^(gain_db/40)`（spec §5.3）。
pub fn rbj_a(gain_db: f64) -> f64 {
    10f64.powf(gain_db / 40.0)
}

// ---------------------------------------------------------------------------
// Biquad
// ---------------------------------------------------------------------------

/// 归一化双二阶（`a[0] == 1`）：`y[n] = b0 x[n] + b1 x[n-1] + b2 x[n-2] − a1 y[n-1] − a2 y[n-2]`。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Biquad {
    pub b: [f64; 3],
    pub a: [f64; 3],
}

impl Biquad {
    pub fn identity() -> Self {
        Self {
            b: [1.0, 0.0, 0.0],
            a: [1.0, 0.0, 0.0],
        }
    }

    /// 从 RBJ 参数计算归一化系数（spec §5.3；含契约数值界限校验）。
    pub fn from_params(
        band_type: BandType,
        freq_hz: f64,
        q: f64,
        gain_db: f64,
        fs: f64,
    ) -> Result<Self, PeqError> {
        if !fs.is_finite() || fs <= 0.0 {
            return err(E_PEQ_FS, format!("fs={fs} 非法"));
        }
        if !freq_hz.is_finite() || !q.is_finite() || !gain_db.is_finite() {
            return err(E_PEQ_NUMERIC, "freq/q/gain 含 NaN/Inf");
        }
        let upper = freq_upper_hz(fs);
        if freq_hz < FREQ_MIN_HZ || freq_hz > upper {
            return err(
                E_PEQ_FREQ,
                format!("freq={freq_hz} Hz 越界 [{FREQ_MIN_HZ}, {upper}]（fs={fs}）"),
            );
        }
        if !(0.0 < q) {
            return err(E_PEQ_Q, format!("q/S={q} 非正"));
        }
        if gain_db.abs() > SINGLE_BAND_GAIN_LIMIT_DB {
            return err(
                E_PEQ_GAIN,
                format!("gain={gain_db} dB 越过 ±{SINGLE_BAND_GAIN_LIMIT_DB}"),
            );
        }
        match band_type {
            BandType::Peaking => {
                if q < PEAK_Q_MIN {
                    return err(E_PEQ_Q, format!("peaking q={q} < {PEAK_Q_MIN}"));
                }
            }
            BandType::LowShelf | BandType::HighShelf => {
                if !(SHELF_S_MIN..=SHELF_S_MAX).contains(&q) {
                    return err(
                        E_PEQ_Q,
                        format!("shelf S={q} 越界 [{SHELF_S_MIN}, {SHELF_S_MAX}]（S 不是 Q）"),
                    );
                }
            }
        }

        let w0 = 2.0 * std::f64::consts::PI * freq_hz / fs;
        let cw = w0.cos();
        let a = rbj_a(gain_db);
        let (raw_b, raw_a) = match band_type {
            BandType::Peaking => {
                let alpha = peaking_alpha(w0, q);
                (
                    [1.0 + alpha * a, -2.0 * cw, 1.0 - alpha * a],
                    [1.0 + alpha / a, -2.0 * cw, 1.0 - alpha / a],
                )
            }
            BandType::LowShelf => {
                let alpha = shelf_alpha(w0, a, q);
                let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
                (
                    [
                        a * ((a + 1.0) - (a - 1.0) * cw + two_sqrt_a_alpha),
                        2.0 * a * ((a - 1.0) - (a + 1.0) * cw),
                        a * ((a + 1.0) - (a - 1.0) * cw - two_sqrt_a_alpha),
                    ],
                    [
                        (a + 1.0) + (a - 1.0) * cw + two_sqrt_a_alpha,
                        -2.0 * ((a - 1.0) + (a + 1.0) * cw),
                        (a + 1.0) + (a - 1.0) * cw - two_sqrt_a_alpha,
                    ],
                )
            }
            BandType::HighShelf => {
                let alpha = shelf_alpha(w0, a, q);
                let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
                (
                    [
                        a * ((a + 1.0) + (a - 1.0) * cw + two_sqrt_a_alpha),
                        -2.0 * a * ((a - 1.0) + (a + 1.0) * cw),
                        a * ((a + 1.0) + (a - 1.0) * cw - two_sqrt_a_alpha),
                    ],
                    [
                        (a + 1.0) - (a - 1.0) * cw + two_sqrt_a_alpha,
                        2.0 * ((a - 1.0) - (a + 1.0) * cw),
                        (a + 1.0) - (a - 1.0) * cw - two_sqrt_a_alpha,
                    ],
                )
            }
        };
        let a0 = raw_a[0];
        if !a0.is_finite() || a0.abs() < 1e-300 {
            return err(E_PEQ_UNSTABLE, format!("a0={a0} 非法"));
        }
        let bq = Self {
            b: [raw_b[0] / a0, raw_b[1] / a0, raw_b[2] / a0],
            a: [1.0, raw_a[1] / a0, raw_a[2] / a0],
        };
        for v in bq.b.iter().chain(bq.a.iter()) {
            if !v.is_finite() {
                return err(E_PEQ_NUMERIC, "RBJ 系数含 NaN/Inf");
            }
        }
        if bq.max_pole_radius() > POLE_RADIUS_MAX {
            return err(
                E_PEQ_UNSTABLE,
                format!(
                    "极点半径 {:.9} > {POLE_RADIUS_MAX}（freq={freq_hz}, q={q}, gain={gain_db}）",
                    bq.max_pole_radius()
                ),
            );
        }
        Ok(bq)
    }

    /// 在频率 `f` 的复数响应 `(re, im)`。
    pub fn complex_response(&self, freq_hz: f64, fs: f64) -> (f64, f64) {
        let w = 2.0 * std::f64::consts::PI * freq_hz / fs;
        let (sw, cw) = w.sin_cos();
        let c2 = cw * cw - sw * sw;
        let s2 = 2.0 * sw * cw;
        let num_re = self.b[0] + self.b[1] * cw + self.b[2] * c2;
        let num_im = -(self.b[1] * sw + self.b[2] * s2);
        let den_re = self.a[0] + self.a[1] * cw + self.a[2] * c2;
        let den_im = -(self.a[1] * sw + self.a[2] * s2);
        let den2 = den_re * den_re + den_im * den_im;
        if den2 < 1e-300 {
            return (f64::INFINITY, 0.0);
        }
        (
            (num_re * den_re + num_im * den_im) / den2,
            (num_im * den_re - num_re * den_im) / den2,
        )
    }

    /// 幅度（dB）。
    pub fn magnitude_db(&self, freq_hz: f64, fs: f64) -> f64 {
        let (re, im) = self.complex_response(freq_hz, fs);
        20.0 * re.hypot(im).log10()
    }

    /// 相位（度，wrapped 到 (−180, 180]）。
    pub fn phase_deg(&self, freq_hz: f64, fs: f64) -> f64 {
        let (re, im) = self.complex_response(freq_hz, fs);
        im.atan2(re).to_degrees()
    }

    /// 分母极点（复数根；返回 (re, im) 对）。
    pub fn poles(&self) -> [(f64, f64); 2] {
        let a1 = self.a[1];
        let a2 = self.a[2];
        let disc = a1 * a1 - 4.0 * a2;
        if disc >= 0.0 {
            let s = disc.sqrt();
            [((-a1 + s) / 2.0, 0.0), ((-a1 - s) / 2.0, 0.0)]
        } else {
            let re = -a1 / 2.0;
            let im = (-disc).sqrt() / 2.0;
            [(re, im), (re, -im)]
        }
    }

    /// 最大极点半径 `max|p|`（spec §5.4）。
    pub fn max_pole_radius(&self) -> f64 {
        self.poles()
            .iter()
            .map(|(re, im)| re.hypot(*im))
            .fold(f64::NEG_INFINITY, f64::max)
    }

    /// 稳定性：`max|p| <= 0.999999`（spec §5.4）。
    pub fn is_stable(&self) -> bool {
        self.max_pole_radius() <= POLE_RADIUS_MAX
    }

    /// DC（z=1）增益（dB）。
    pub fn dc_gain_db(&self) -> f64 {
        let num = self.b[0] + self.b[1] + self.b[2];
        let den = self.a[0] + self.a[1] + self.a[2];
        gain_db_from(num / den)
    }

    /// Nyquist（z=−1）增益（dB）。
    pub fn nyquist_gain_db(&self) -> f64 {
        let num = self.b[0] - self.b[1] + self.b[2];
        let den = self.a[0] - self.a[1] + self.a[2];
        gain_db_from(num / den)
    }

    /// 冲激响应（长度 `len`，Direct Form I 数值递推）。
    pub fn impulse_response(&self, len: usize) -> Vec<f64> {
        let mut out = vec![0.0; len];
        let mut x1 = 0.0;
        let mut x2 = 0.0;
        let mut y1 = 0.0;
        let mut y2 = 0.0;
        for (n, slot) in out.iter_mut().enumerate() {
            let x = if n == 0 { 1.0 } else { 0.0 };
            let y =
                self.b[0] * x + self.b[1] * x1 + self.b[2] * x2 - self.a[1] * y1 - self.a[2] * y2;
            *slot = y;
            x2 = x1;
            x1 = x;
            y2 = y1;
            y1 = y;
        }
        out
    }

    /// f32 量化后的系数（供 §5.4 `requires_f64` 复算）。
    pub fn quantized_f32(&self) -> Self {
        Self {
            b: [
                self.b[0] as f32 as f64,
                self.b[1] as f32 as f64,
                self.b[2] as f32 as f64,
            ],
            a: [1.0, self.a[1] as f32 as f64, self.a[2] as f32 as f64],
        }
    }
}

fn gain_db_from(ratio: f64) -> f64 {
    if ratio.is_nan() {
        return f64::NAN;
    }
    if ratio == 0.0 {
        return f64::NEG_INFINITY;
    }
    20.0 * ratio.abs().log10()
}

// ---------------------------------------------------------------------------
// 频段类型与模态频段
// ---------------------------------------------------------------------------

/// 频段类型（CONTRACT v1.1 §3.4/§10；未知 type 整份拒绝）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BandType {
    Peaking,
    LowShelf,
    HighShelf,
}

impl BandType {
    pub fn as_str(self) -> &'static str {
        match self {
            BandType::Peaking => "peaking",
            BandType::LowShelf => "low_shelf",
            BandType::HighShelf => "high_shelf",
        }
    }

    /// 解析 type；未知 type 拒绝（E_PEQ_TYPE，禁止降级）。
    pub fn parse(s: &str) -> Result<Self, PeqError> {
        match s {
            "peaking" => Ok(BandType::Peaking),
            "low_shelf" => Ok(BandType::LowShelf),
            "high_shelf" => Ok(BandType::HighShelf),
            other => err(
                E_PEQ_TYPE,
                format!(
                    "未知 filter type {other:?}（只允许 peaking|low_shelf|high_shelf，禁止降级）"
                ),
            ),
        }
    }

    /// 消费语义：缺省（None/空）按 peaking；未知仍拒绝（E_PEQ_TYPE）。
    pub fn parse_optional(s: Option<&str>) -> Result<Self, PeqError> {
        match s {
            None | Some("") => Ok(BandType::Peaking),
            Some(v) => Self::parse(v),
        }
    }

    pub fn is_shelf(self) -> bool {
        matches!(self, BandType::LowShelf | BandType::HighShelf)
    }
}

/// 一个 RBJ 频段：参数 + 归一化系数（`a[0] == 1`）。shelf 的 `q` 字段是 slope S。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModalBand {
    pub band_type: BandType,
    pub freq_hz: f64,
    /// peaking：RBJ Q；shelf：RBJ slope S（[0.1,1.0]，不是 Q）。
    pub q: f64,
    pub gain_db: f64,
    pub b: [f64; 3],
    pub a: [f64; 3],
}

impl ModalBand {
    pub fn from_params(
        band_type: BandType,
        freq_hz: f64,
        q: f64,
        gain_db: f64,
        fs: f64,
    ) -> Result<Self, PeqError> {
        let biquad = Biquad::from_params(band_type, freq_hz, q, gain_db, fs)?;
        Ok(Self {
            band_type,
            freq_hz,
            q,
            gain_db,
            b: biquad.b,
            a: biquad.a,
        })
    }

    pub fn biquad(&self) -> Biquad {
        Biquad {
            b: self.b,
            a: self.a,
        }
    }

    /// 契约校验：字段重算系数并与已存系数自洽、数值/频率/增益/Q/S/稳定（E_PEQ_COEFF 等）。
    pub fn validate_contract(&self, fs: f64) -> Result<(), PeqError> {
        let recomputed =
            Biquad::from_params(self.band_type, self.freq_hz, self.q, self.gain_db, fs)?;
        for k in 0..3 {
            if (recomputed.b[k] - self.b[k]).abs() > 1e-12 {
                return err(
                    E_PEQ_COEFF,
                    format!(
                        "b[{k}] 与 band 字段重算不一致：{} vs {}",
                        self.b[k], recomputed.b[k]
                    ),
                );
            }
            if (recomputed.a[k] - self.a[k]).abs() > 1e-12 {
                return err(
                    E_PEQ_COEFF,
                    format!(
                        "a[{k}] 与 band 字段重算不一致：{} vs {}",
                        self.a[k], recomputed.a[k]
                    ),
                );
            }
        }
        if !self.biquad().is_stable() {
            return err(
                E_PEQ_UNSTABLE,
                format!("极点半径 {:.9} 越界", self.biquad().max_pole_radius()),
            );
        }
        Ok(())
    }

    /// 中心频率处的自身幅度修正（dB）。peaking 恒等于 gain_db；shelf 为半增益。
    pub fn correction_at_center_db(&self, fs: f64) -> f64 {
        self.biquad().magnitude_db(self.freq_hz, fs)
    }
}

// ---------------------------------------------------------------------------
// 预测
// ---------------------------------------------------------------------------

/// 每通道预测指标（spec §5.5 `predicted` + §5.4 报告 `max|a1|`）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeqPrediction {
    pub dc_gain_db: f64,
    pub nyquist_gain_db: f64,
    pub poles_radius_max: f64,
    pub max_a1: f64,
    pub requires_f64: bool,
}

// ---------------------------------------------------------------------------
// 通道与设计
// ---------------------------------------------------------------------------

/// 单通道的模态 PEQ 链。
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelPeq {
    pub name: String,
    pub index: u32,
    pub bands: Vec<ModalBand>,
}

impl ChannelPeq {
    pub fn new(name: impl Into<String>, index: u32) -> Result<Self, PeqError> {
        let name = name.into();
        validate_channel_name(&name)?;
        Ok(Self {
            name,
            index,
            bands: Vec::new(),
        })
    }

    pub fn push_band(&mut self, band: ModalBand) {
        self.bands.push(band);
    }

    pub fn is_empty(&self) -> bool {
        self.bands.is_empty()
    }

    pub fn modal_peaking_count(&self) -> usize {
        self.bands
            .iter()
            .filter(|b| b.band_type == BandType::Peaking)
            .count()
    }

    pub fn shelf_count(&self, band_type: BandType) -> usize {
        self.bands
            .iter()
            .filter(|b| b.band_type == band_type)
            .count()
    }

    /// 级联复数响应。
    pub fn cascade_complex(&self, freq_hz: f64, fs: f64) -> (f64, f64) {
        let mut re = 1.0;
        let mut im = 0.0;
        for band in &self.bands {
            let (r, i) = band.biquad().complex_response(freq_hz, fs);
            let nr = re * r - im * i;
            let ni = re * i + im * r;
            re = nr;
            im = ni;
        }
        (re, im)
    }

    /// 级联幅度（dB）。
    pub fn cascade_magnitude_db(&self, freq_hz: f64, fs: f64) -> f64 {
        let (re, im) = self.cascade_complex(freq_hz, fs);
        20.0 * re.hypot(im).log10()
    }

    /// 级联相位（度，wrapped）。
    pub fn cascade_phase_deg(&self, freq_hz: f64, fs: f64) -> f64 {
        let (re, im) = self.cascade_complex(freq_hz, fs);
        im.atan2(re).to_degrees()
    }

    /// 级联冲激响应（按段顺序逐 biquad 过滤）。
    pub fn impulse_response(&self, len: usize) -> Vec<f64> {
        let mut signal = vec![0.0; len];
        if len > 0 {
            signal[0] = 1.0;
        }
        for band in &self.bands {
            signal = filter_signal(&band.biquad(), &signal);
        }
        signal
    }

    pub fn max_pole_radius(&self) -> f64 {
        self.bands
            .iter()
            .map(|b| b.biquad().max_pole_radius())
            .fold(f64::NEG_INFINITY, f64::max)
    }

    pub fn is_stable(&self) -> bool {
        self.bands.iter().all(|b| b.biquad().is_stable())
    }

    /// §5.4 固定 2001 点网格上的 f32 量化最大偏差（dB）。
    pub fn f32_quantization_max_deviation_db(&self, fs: f64) -> f64 {
        let mut worst: f64 = 0.0;
        for f in fixed_eval_grid() {
            let exact = self.cascade_magnitude_db(f, fs);
            let mut re = 1.0;
            let mut im = 0.0;
            for band in &self.bands {
                let (r, i) = band.biquad().quantized_f32().complex_response(f, fs);
                let nr = re * r - im * i;
                let ni = re * i + im * r;
                re = nr;
                im = ni;
            }
            let quant = 20.0 * re.hypot(im).log10();
            let d = (exact - quant).abs();
            if d > worst {
                worst = d;
            }
        }
        worst
    }

    pub fn prediction(&self, fs: f64) -> PeqPrediction {
        let (re_dc, im_dc) = self.cascade_complex(0.0, fs);
        let (re_ny, im_ny) = self.cascade_complex(fs / 2.0, fs);
        let max_dev = self.f32_quantization_max_deviation_db(fs);
        let max_a1 = self
            .bands
            .iter()
            .map(|b| b.a[1].abs())
            .fold(0.0_f64, f64::max);
        PeqPrediction {
            dc_gain_db: 20.0 * re_dc.hypot(im_dc).log10(),
            nyquist_gain_db: 20.0 * re_ny.hypot(im_ny).log10(),
            poles_radius_max: self.max_pole_radius(),
            max_a1,
            requires_f64: max_dev > F32_QUANTIZATION_TOLERANCE_DB,
        }
    }

    /// 生产/消费校验（spec §5.2/§5.4；错误码见常量）。
    pub fn validate(&self, fs: u32, limits: &PeqLimits) -> Result<(), PeqError> {
        validate_sample_rate(fs)?;
        validate_channel_name(&self.name)?;
        let fs_f = fs as f64;
        if self.bands.len() > limits.max_biquads {
            return err(
                E_PEQ_LIMIT,
                format!(
                    "通道 {} band 数 {} > {}",
                    self.name,
                    self.bands.len(),
                    limits.max_biquads
                ),
            );
        }
        let modal_peakings = self.modal_peaking_count();
        if modal_peakings > limits.max_modal_peakings {
            return err(
                E_PEQ_LIMIT,
                format!(
                    "通道 {} 模态 peaking 段 {} > {}",
                    self.name, modal_peakings, limits.max_modal_peakings
                ),
            );
        }
        for band_type in [BandType::LowShelf, BandType::HighShelf] {
            if let Some(max) = limits.max_shelves_per_type {
                let n = self.shelf_count(band_type);
                if n > max {
                    return err(
                        E_PEQ_LIMIT,
                        format!("通道 {} {} 段 {n} > {max}", self.name, band_type.as_str()),
                    );
                }
            }
        }
        let mut positive_total = 0.0_f64;
        for (idx, band) in self.bands.iter().enumerate() {
            band.validate_contract(fs_f)?;
            if limits.enforce_producer_policy {
                let is_modal = band.band_type == BandType::Peaking
                    && band.freq_hz >= limits.modal_band_hz[0]
                    && band.freq_hz <= limits.modal_band_hz[1];
                if is_modal && band.gain_db > MODAL_BOOST_CAP_DB {
                    return err(
                        E_PEQ_BOOST,
                        format!(
                            "通道 {} 第 {idx} 段是模态 peaking 但 gain={} dB > 0（只切，allow_boost 对模态段无效）",
                            self.name, band.gain_db
                        ),
                    );
                }
                if band.gain_db > 0.0 && !is_modal {
                    positive_total += band.gain_db;
                }
            }
        }
        if limits.enforce_producer_policy {
            let cap = if limits.allow_boost {
                WIDEBAND_BOOST_MAX_CAP_DB
            } else {
                WIDEBAND_BOOST_DEFAULT_CAP_DB
            };
            if positive_total > cap + 1e-12 {
                return err(
                    E_PEQ_BOOST,
                    format!(
                        "通道 {} 宽带正增益总量 {positive_total:.3} dB > cap {cap} dB（allow_boost={}）",
                        self.name, limits.allow_boost
                    ),
                );
            }
            if positive_total > WIDEBAND_POSITIVE_TOTAL_LIMIT_DB + 1e-12 {
                return err(
                    E_PEQ_BOOST,
                    format!(
                        "通道 {} 宽带正增益总量 {positive_total:.3} dB 越过硬上限",
                        self.name
                    ),
                );
            }
        }
        Ok(())
    }
}

fn validate_channel_name(name: &str) -> Result<(), PeqError> {
    let ok = !name.is_empty()
        && name
            .bytes()
            .next()
            .map(|c| c.is_ascii_uppercase())
            .unwrap_or(false)
        && name
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    if ok {
        Ok(())
    } else {
        err(
            E_PEQ_NAME,
            format!("通道名 {name:?} 不匹配 ^[A-Z][A-Z0-9]*$（CONTRACT §5 规则 3）"),
        )
    }
}

fn filter_signal(biquad: &Biquad, input: &[f64]) -> Vec<f64> {
    let mut out = vec![0.0; input.len()];
    let mut x1 = 0.0;
    let mut x2 = 0.0;
    let mut y1 = 0.0;
    let mut y2 = 0.0;
    for (n, x) in input.iter().copied().enumerate() {
        let y = biquad.b[0] * x + biquad.b[1] * x1 + biquad.b[2] * x2
            - biquad.a[1] * y1
            - biquad.a[2] * y2;
        out[n] = y;
        x2 = x1;
        x1 = x;
        y2 = y1;
        y1 = y;
    }
    out
}

/// 通道校验限额（生产端 vs 消费端语义区分，spec §5.2/I06）。
#[derive(Clone, Debug, PartialEq)]
pub struct PeqLimits {
    pub max_modal_peakings: usize,
    pub max_shelves_per_type: Option<usize>,
    pub max_biquads: usize,
    pub modal_band_hz: [f64; 2],
    pub allow_boost: bool,
    pub enforce_producer_policy: bool,
}

impl PeqLimits {
    /// 生产端（T41/T64）：模态 peaking ≤10、每 shelf 类型 ≤1、总 ≤32、模态只切、boost 受限。
    pub fn producer(_mode: DesignMode, allow_boost: bool, modal_band_hz: [f64; 2]) -> Self {
        Self {
            max_modal_peakings: MAX_MODAL_PEAKINGS,
            max_shelves_per_type: Some(MAX_SHELVES_PER_TYPE_PRODUCER),
            max_biquads: MAX_BIQUADS_PER_CHANNEL,
            modal_band_hz,
            allow_boost,
            enforce_producer_policy: true,
        }
    }

    /// 消费端（T13/T23）：不假设 shelf 段数，接受已知 type 任意段，总数 ≤32。
    pub fn consumer() -> Self {
        Self {
            max_modal_peakings: MAX_BIQUADS_PER_CHANNEL,
            max_shelves_per_type: None,
            max_biquads: MAX_BIQUADS_PER_CHANNEL,
            modal_band_hz: [FREQ_MIN_HZ, FREQ_MAX_HZ],
            allow_boost: true,
            enforce_producer_policy: false,
        }
    }
}

/// 设计模式（spec §5.5 `design.mode`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesignMode {
    ModalOnly,
    ModalPlusWide,
}

impl DesignMode {
    pub fn as_str(self) -> &'static str {
        match self {
            DesignMode::ModalOnly => "modal_only",
            DesignMode::ModalPlusWide => "modal_plus_wide",
        }
    }

    pub fn parse(s: &str) -> Result<Self, PeqError> {
        match s {
            "modal_only" => Ok(DesignMode::ModalOnly),
            "modal_plus_wide" => Ok(DesignMode::ModalPlusWide),
            other => err(E_PEQ_SCHEMA, format!("未知 design.mode {other:?}")),
        }
    }
}

/// 导出 JSON `source` 块（spec §5.5；全部可选，未知/缺失不伪造）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PeqSource {
    pub measurement_sha256: Option<String>,
    pub curve_id: Option<String>,
    pub target_sha256: Option<String>,
    pub calibration_sha256: Option<String>,
}

/// 一次模态 PEQ 设计的完整产物（导出 schema `tspi-modal-peq/v1`）。
#[derive(Clone, Debug, PartialEq)]
pub struct PeqDesign {
    pub tool_name: String,
    pub tool_version: String,
    pub created_unix: u64,
    pub fs: u32,
    pub source: PeqSource,
    pub channels: Vec<ChannelPeq>,
    pub mode: DesignMode,
    pub allow_boost: bool,
    pub band_hz: [f64; 2],
    pub fit_report_sha256: Option<String>,
    pub warnings: Vec<String>,
}

impl PeqDesign {
    pub fn new(fs: u32, mode: DesignMode) -> Result<Self, PeqError> {
        validate_sample_rate(fs)?;
        Ok(Self {
            tool_name: "khaen-designer".to_string(),
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            created_unix: 0,
            fs,
            source: PeqSource::default(),
            channels: Vec::new(),
            mode,
            allow_boost: false,
            band_hz: DEFAULT_MODAL_BAND_HZ,
            fit_report_sha256: None,
            warnings: Vec::new(),
        })
    }

    /// 追加通道（重复 name/index 拒绝）。
    pub fn add_channel(&mut self, channel: ChannelPeq) -> Result<(), PeqError> {
        if self.channels.iter().any(|c| c.name == channel.name) {
            return err(E_PEQ_CHANNELS, format!("重复通道名 {}", channel.name));
        }
        if self.channels.iter().any(|c| c.index == channel.index) {
            return err(E_PEQ_CHANNELS, format!("重复通道 index {}", channel.index));
        }
        self.channels.push(channel);
        Ok(())
    }

    pub fn limits(&self) -> PeqLimits {
        PeqLimits::producer(self.mode, self.allow_boost, self.band_hz)
    }

    /// 设计级校验；返回生成的 warning 列表（设计器自报，不静默放行）。
    pub fn validate(&self) -> Result<Vec<String>, PeqError> {
        validate_sample_rate(self.fs)?;
        if self.channels.is_empty() {
            return err(E_PEQ_CHANNELS, "channels 为空");
        }
        if self.channels.len() > MAX_BIQUADS_PER_CHANNEL {
            return err(
                E_PEQ_CHANNELS,
                format!("channels {} > 32", self.channels.len()),
            );
        }
        let [lo, hi] = self.band_hz;
        let upper = freq_upper_hz(self.fs as f64);
        if !(lo.is_finite() && hi.is_finite()) || lo < FREQ_MIN_HZ || hi > upper || !(lo < hi) {
            return err(
                E_PEQ_BAND,
                format!(
                    "design.band_hz [{lo}, {hi}] 非法（须在 [{FREQ_MIN_HZ}, {upper}] 且 lo<hi）"
                ),
            );
        }
        let limits = self.limits();
        let mut warnings = Vec::new();
        for channel in &self.channels {
            channel.validate(self.fs, &limits)?;
            let positive_total: f64 = channel
                .bands
                .iter()
                .filter(|b| b.gain_db > 0.0)
                .map(|b| b.gain_db)
                .sum();
            if positive_total > WIDEBAND_POSITIVE_WARN_DB {
                warnings.push(format!(
                    "通道 {} 正增益总量 {positive_total:.3} dB > {WIDEBAND_POSITIVE_WARN_DB} dB（spec §5.2 warning）",
                    channel.name
                ));
            }
        }
        if self.mode == DesignMode::ModalPlusWide {
            let has_wide = self.channels.iter().any(|c| {
                c.bands.iter().any(|b| {
                    b.band_type.is_shelf()
                        || !(b.freq_hz >= self.band_hz[0] && b.freq_hz <= self.band_hz[1])
                })
            });
            if !has_wide {
                warnings.push("mode=modal_plus_wide 但没有任何宽带/非模态段".to_string());
            }
        }
        if self.allow_boost {
            warnings.push(format!(
                "allow_boost=true：宽带正增益上限 +{WIDEBAND_BOOST_MAX_CAP_DB} dB（>+{WIDEBAND_BOOST_MAX_CAP_DB} 由拟合层按上限执行）"
            ));
        }
        Ok(warnings)
    }

    /// 导出 `tspi-modal-peq/v1` JSON（确定性：字段顺序固定、数字至少 17 位有效数字）。
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        out.push('{');
        out.push_str(&format!("\"schema\":{},", json_string(MODAL_PEQ_SCHEMA)));
        out.push_str("\"tool\":{");
        out.push_str(&format!("\"name\":{},", json_string(&self.tool_name)));
        out.push_str(&format!("\"version\":{}", json_string(&self.tool_version)));
        out.push_str("},");
        out.push_str(&format!("\"created_unix\":{},", self.created_unix));
        out.push_str(&format!("\"fs\":{},", self.fs));
        out.push_str("\"source\":{");
        let mut first = true;
        for (key, value) in [
            ("measurement_sha256", &self.source.measurement_sha256),
            ("curve_id", &self.source.curve_id),
            ("target_sha256", &self.source.target_sha256),
            ("calibration_sha256", &self.source.calibration_sha256),
        ] {
            if let Some(v) = value {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&format!("{}:{}", json_string(key), json_string(v)));
            }
        }
        out.push_str("},");
        out.push_str("\"channels\":[");
        for (ci, channel) in self.channels.iter().enumerate() {
            if ci > 0 {
                out.push(',');
            }
            out.push('{');
            out.push_str(&format!("\"name\":{},", json_string(&channel.name)));
            out.push_str(&format!("\"index\":{},", channel.index));
            out.push_str("\"bands\":[");
            for (bi, band) in channel.bands.iter().enumerate() {
                if bi > 0 {
                    out.push(',');
                }
                out.push('{');
                out.push_str(&format!(
                    "\"type\":{},",
                    json_string(band.band_type.as_str())
                ));
                out.push_str(&format!("\"freq\":{},", json_number(band.freq_hz)));
                out.push_str(&format!("\"q\":{},", json_number(band.q)));
                out.push_str(&format!("\"gain_db\":{},", json_number(band.gain_db)));
                out.push_str("\"b\":[");
                out.push_str(&format!(
                    "{},{},{}",
                    json_number(band.b[0]),
                    json_number(band.b[1]),
                    json_number(band.b[2])
                ));
                out.push_str("],\"a\":[");
                out.push_str(&format!(
                    "{},{},{}",
                    json_number(band.a[0]),
                    json_number(band.a[1]),
                    json_number(band.a[2])
                ));
                out.push_str("]}");
            }
            out.push_str("],\"predicted\":{");
            let prediction = channel.prediction(self.fs as f64);
            out.push_str(&format!(
                "\"dc_gain_db\":{},",
                json_number(prediction.dc_gain_db)
            ));
            out.push_str(&format!(
                "\"nyquist_gain_db\":{},",
                json_number(prediction.nyquist_gain_db)
            ));
            out.push_str(&format!(
                "\"poles_radius_max\":{},",
                json_number(prediction.poles_radius_max)
            ));
            out.push_str(&format!("\"requires_f64\":{}", prediction.requires_f64));
            out.push_str("}}");
        }
        out.push_str("],");
        out.push_str("\"design\":{");
        out.push_str(&format!("\"mode\":{},", json_string(self.mode.as_str())));
        out.push_str(&format!("\"allow_boost\":{},", self.allow_boost));
        out.push_str(&format!(
            "\"band_hz\":[{},{}]",
            json_number(self.band_hz[0]),
            json_number(self.band_hz[1])
        ));
        if let Some(h) = &self.fit_report_sha256 {
            out.push_str(&format!(",\"fit_report_sha256\":{}", json_string(h)));
        }
        out.push_str("},");
        out.push_str("\"warnings\":[");
        for (i, w) in self.warnings.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&json_string(w));
        }
        out.push_str("]}");
        out
    }
}

fn json_number(x: f64) -> String {
    // 至少 17 位有效数字（spec §5.5）；18 位保证任意 f64 往返无损。
    format!("{x:.17e}")
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// 固定评估网格
// ---------------------------------------------------------------------------

/// §5.4 固定评估网格：2001 点，`f_k = 20*1000^(k/2000)`。
pub fn fixed_eval_grid() -> Vec<f64> {
    (0..EVAL_GRID_POINTS)
        .map(|k| 20.0 * 1000f64.powf(k as f64 / (EVAL_GRID_POINTS - 1) as f64))
        .collect()
}

// ---------------------------------------------------------------------------
// IIR 不硬截 FIR（spec §5.7）
// ---------------------------------------------------------------------------

/// 极点半径对应的保守 FIR 长度估计：`ceil(log(1e-5)/log(r))`（spec §5.7 公式）。
pub fn fir_required_len(pole_radius: f64) -> usize {
    if !pole_radius.is_finite() || pole_radius <= 0.0 {
        return 1;
    }
    if pole_radius >= 1.0 {
        return usize::MAX;
    }
    let len = FIR_TAIL_AMPLITUDE_EPS.ln() / pole_radius.ln();
    if !len.is_finite() || len > usize::MAX as f64 {
        usize::MAX
    } else if len <= 1.0 {
        1
    } else {
        len.ceil() as usize
    }
}

/// 单段尾长信息。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandFirTail {
    pub band_index: usize,
    pub pole_radius: f64,
    pub required_len: usize,
}

/// IIR→FIR 尾长报告（spec §5.7；`passed` 时可直接用该长度 FIR 近似）。
#[derive(Clone, Debug, PartialEq)]
pub struct FirTailReport {
    pub candidate_len: usize,
    pub tail_window_len: usize,
    pub tail_peak_db: f64,
    pub tail_energy_db: f64,
    pub passed: bool,
    pub bands: Vec<BandFirTail>,
    pub required_len: usize,
}

impl ChannelPeq {
    /// 数值计算尾长指标（不拒绝；拒绝用 `check_iir_to_fir`）。
    pub fn fir_tail_report(&self, candidate_len: usize) -> Result<FirTailReport, PeqError> {
        if candidate_len < 16 {
            return err(
                E_PEQ_FIR_TAIL,
                format!("candidate_len={candidate_len} < 16，长度估计无意义"),
            );
        }
        let h = self.impulse_response(candidate_len);
        let total_peak = h.iter().map(|x| x.abs()).fold(0.0_f64, f64::max);
        let total_energy: f64 = h.iter().map(|x| x * x).sum();
        let tail_window_len = ((candidate_len as f64 * FIR_TAIL_WINDOW_FRACTION).round() as usize)
            .clamp(1, candidate_len);
        let tail_start = candidate_len - tail_window_len;
        let tail_peak = h[tail_start..]
            .iter()
            .map(|x| x.abs())
            .fold(0.0_f64, f64::max);
        let tail_energy: f64 = h[tail_start..].iter().map(|x| x * x).sum();
        let tail_peak_db = if total_peak > 0.0 && tail_peak > 0.0 {
            20.0 * (tail_peak / total_peak).log10()
        } else if tail_peak == 0.0 {
            f64::NEG_INFINITY
        } else {
            f64::NAN
        };
        let tail_energy_db = if total_energy > 0.0 && tail_energy > 0.0 {
            10.0 * (tail_energy / total_energy).log10()
        } else if tail_energy == 0.0 {
            f64::NEG_INFINITY
        } else {
            f64::NAN
        };
        let mut bands = Vec::with_capacity(self.bands.len());
        let mut required_len = 1usize;
        for (idx, band) in self.bands.iter().enumerate() {
            let r = band.biquad().max_pole_radius();
            let req = fir_required_len(r);
            required_len = required_len.max(req);
            bands.push(BandFirTail {
                band_index: idx,
                pole_radius: r,
                required_len: req,
            });
        }
        let passed =
            tail_peak_db <= FIR_TAIL_PEAK_LIMIT_DB && tail_energy_db <= FIR_TAIL_ENERGY_LIMIT_DB;
        Ok(FirTailReport {
            candidate_len,
            tail_window_len,
            tail_peak_db,
            tail_energy_db,
            passed,
            bands,
            required_len,
        })
    }

    /// §5.7：若必须把该 IIR 转 FIR（例如运行 profile 只接受 FIR），候选长度不足时必须拒绝
    /// `E_PEQ_FIR_TAIL` 并给出所需长度估计；禁止静默截断。
    pub fn check_iir_to_fir(&self, candidate_len: usize) -> Result<FirTailReport, PeqError> {
        let report = self.fir_tail_report(candidate_len)?;
        if report.passed {
            return Ok(report);
        }
        let worst = report
            .bands
            .iter()
            .max_by(|a, b| a.pole_radius.partial_cmp(&b.pole_radius).unwrap())
            .copied();
        let detail = match worst {
            Some(b) => format!(
                "极点半径 {:.9}（第 {} 段）需 L≈{} samples；tail_peak={:.2} dB（限 {}）、tail_energy={:.2} dB（限 {}）",
                b.pole_radius,
                b.band_index,
                b.required_len,
                report.tail_peak_db,
                FIR_TAIL_PEAK_LIMIT_DB,
                report.tail_energy_db,
                FIR_TAIL_ENERGY_LIMIT_DB
            ),
            None => format!(
                "tail_peak={:.2} dB、tail_energy={:.2} dB",
                report.tail_peak_db, report.tail_energy_db
            ),
        };
        err(
            E_PEQ_FIR_TAIL,
            format!(
                "IIR→FIR 尾长不满足（candidate_len={candidate_len}, required_len={}）：{detail}；禁止静默截断",
                report.required_len
            ),
        )
    }
}

// ---------------------------------------------------------------------------
// “原图比对”（spec §5.6；2a 拟合单点 / 2b 多测点验收分开报告）
// ---------------------------------------------------------------------------

/// 每段修正报告：f0、peak/dip 修正量、该点残余。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandCorrectionReport {
    pub band_index: usize,
    pub band_type: BandType,
    pub freq_hz: f64,
    pub q: f64,
    pub gain_db: f64,
    /// 修正前 `original − target`（正=高出目标）。
    pub original_deviation_db: f64,
    /// 该段自身在 f0 的修正量（peaking≈gain_db；shelf 为半增益）。
    pub applied_correction_db: f64,
    /// 全部段修正后 `corrected − target`。
    pub residual_db: f64,
}

/// 模态带残余（修正前/后，RMS 与最大绝对值）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModalBandResidual {
    pub band_hz: [f64; 2],
    pub rms_before_db: f64,
    pub rms_after_db: f64,
    pub max_before_db: f64,
    pub max_after_db: f64,
}

/// §5.6.2a 规则（拟合阶段单点）：峰值高出目标 >+3 dB 或该点相对修正前变差 >3 dB → FAIL。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rule2aReport {
    pub passed: bool,
    pub limit_db: f64,
    pub peak_over_target_db: f64,
    pub worst_worsening_db: f64,
    pub worst_freq_hz: Option<f64>,
}

/// §5.6.2b 规则（多测点验收）：逐点恶化 ≤1 dB、1/3 oct ripple ≤3 dB、模态带峰 ≤target+3 dB、
/// 无新增 >6 dB 窄带缺口；并单列 1/24 oct 原始分辨率最大偏差（不得只看 1/3 oct 平滑）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rule2bLimits {
    pub point_worsening_db: f64,
    pub one_third_octave_ripple_db: f64,
    pub modal_band_peak_over_target_db: f64,
    pub new_narrow_notch_db: f64,
}

impl Default for Rule2bLimits {
    fn default() -> Self {
        Self {
            point_worsening_db: 1.0,
            one_third_octave_ripple_db: 3.0,
            modal_band_peak_over_target_db: 3.0,
            new_narrow_notch_db: 6.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rule2bReport {
    pub passed: bool,
    pub limits: Rule2bLimits,
    pub worst_point_worsening_db: f64,
    pub one_third_octave_ripple_db: f64,
    pub modal_band_peak_over_target_db: f64,
    pub new_narrow_notch_db: f64,
    /// 1/24 oct 原始分辨率最大偏差（spec §5.6.2b 第 5 条：必须另报）。
    pub raw_max_deviation_db_1_24_oct: f64,
    /// 输入曲线原始点上的最大偏差。
    pub raw_max_deviation_db_at_input_points: f64,
    /// 1/3 oct 平滑后的最大偏差（对照用）。
    pub one_third_octave_max_deviation_db: f64,
}

/// “原图比对”完整结果（2a/2b 分开，不混写）。
#[derive(Clone, Debug, PartialEq)]
pub struct OriginalComparison {
    pub modal_band_hz: [f64; 2],
    pub eval_grid_points: usize,
    pub band_corrections: Vec<BandCorrectionReport>,
    pub modal_band_residual: ModalBandResidual,
    pub rule_2a: Rule2aReport,
    pub rule_2b: Rule2bReport,
    pub warnings: Vec<String>,
}

impl OriginalComparison {
    /// 2a 与 2b 分别报告；overall 仅表示两者都通过（签收门由调用方/ACCEPTANCE 决定）。
    pub fn overall_pass(&self) -> bool {
        self.rule_2a.passed && self.rule_2b.passed
    }
}

/// spec §5.6：把设计（b/a 级联）叠加到修正前原始曲线上，与 target 在同一网格比较。
///
/// - `original`：修正前 measurement 幅度曲线（Hz/dB，升序）；
/// - `target`：期望曲线（Hz/dB，升序；flat 可传全 0）；端点行为 hold；
/// - `modal_band_hz`：模态带（默认 20–250 Hz）。
pub fn compare_design_to_original(
    channel: &ChannelPeq,
    fs: u32,
    original: &[CurvePoint],
    target: &[CurvePoint],
    modal_band_hz: [f64; 2],
) -> Result<OriginalComparison, PeqError> {
    validate_sample_rate(fs)?;
    validate_curve(original)?;
    validate_curve(target)?;
    let fs_f = fs as f64;
    let upper = freq_upper_hz(fs_f);
    let lo = FREQ_MIN_HZ.max(original[0].freq_hz).max(target[0].freq_hz);
    let hi = upper
        .min(original[original.len() - 1].freq_hz)
        .min(target[target.len() - 1].freq_hz);
    if !(hi > lo) {
        return err(
            E_PEQ_CURVE,
            format!("original/target 覆盖范围无交集或不足（lo={lo}, hi={hi}）"),
        );
    }
    let mb_lo = modal_band_hz[0].max(lo);
    let mb_hi = modal_band_hz[1].min(hi);
    if !(mb_hi > mb_lo) {
        return err(
            E_PEQ_BAND,
            format!(
                "模态带 [{}, {}] 与评估范围 [{lo}, {hi}] 无交集",
                modal_band_hz[0], modal_band_hz[1]
            ),
        );
    }

    let grid = log_frequency_grid(lo, hi, SELF_CONSISTENCY_OCTAVE_RES);
    let n = grid.len();
    let mut before = Vec::with_capacity(n);
    let mut target_db = Vec::with_capacity(n);
    let mut after = Vec::with_capacity(n);
    for &f in &grid {
        let o = interpolate_curve_db(original, f)?;
        let t = interpolate_curve_db(target, f)?;
        before.push(o);
        target_db.push(t);
        after.push(o + channel.cascade_magnitude_db(f, fs_f));
    }
    let residual_before: Vec<f64> = before.iter().zip(&target_db).map(|(o, t)| o - t).collect();
    let residual_after: Vec<f64> = after.iter().zip(&target_db).map(|(a, t)| a - t).collect();

    let band_idx: Vec<usize> = (0..n)
        .filter(|&i| grid[i] >= mb_lo && grid[i] <= mb_hi)
        .collect();
    if band_idx.is_empty() {
        return err(E_PEQ_BAND, "模态带内无评估点");
    }

    // 2a：峰值高出目标 / 相对修正前变差
    let mut peak_over = f64::NEG_INFINITY;
    let mut worst_worsening = f64::NEG_INFINITY;
    let mut worst_freq = None;
    for &i in &band_idx {
        let over = residual_after[i];
        if over > peak_over {
            peak_over = over;
        }
        let worsen = residual_after[i].abs() - residual_before[i].abs();
        if worsen > worst_worsening {
            worst_worsening = worsen;
            worst_freq = Some(grid[i]);
        }
    }
    let rule_2a = Rule2aReport {
        passed: peak_over <= 3.0 && worst_worsening <= 3.0,
        limit_db: 3.0,
        peak_over_target_db: peak_over,
        worst_worsening_db: worst_worsening,
        worst_freq_hz: worst_freq,
    };

    // 2b：全评估范围逐点恶化
    let mut worst_point_worsening = f64::NEG_INFINITY;
    for i in 0..n {
        let worsen = residual_after[i].abs() - residual_before[i].abs();
        if worsen > worst_point_worsening {
            worst_point_worsening = worsen;
        }
    }

    // 1/3 oct 平滑（±1/6 oct 半窗）后的残余 ripple / 最大偏差
    let half_third = ((1.0 / 6.0) / SELF_CONSISTENCY_OCTAVE_RES).round().max(1.0) as usize;
    let smoothed_after = moving_average(&residual_after, half_third);
    let smooth_band_start = band_idx[0];
    let band_len = band_idx[band_idx.len() - 1] - smooth_band_start + 1;
    let smoothed_band: Vec<f64> =
        smoothed_after[smooth_band_start..smooth_band_start + band_len].to_vec();
    let ripple = max_minus_min(&smoothed_band);
    let smoothed_dev = smoothed_after
        .iter()
        .map(|x| x.abs())
        .fold(0.0_f64, f64::max);

    // 新增窄带缺口
    let new_notch_db = detect_new_narrow_notch(&grid, &residual_before, &residual_after);

    // 1/24 oct 原始分辨率最大偏差
    let grid24 = log_frequency_grid(lo, hi, 1.0 / 24.0);
    let mut raw_1_24 = 0.0_f64;
    for f in grid24 {
        let o = interpolate_curve_db(original, f)?;
        let t = interpolate_curve_db(target, f)?;
        let a = o + channel.cascade_magnitude_db(f, fs_f);
        raw_1_24 = raw_1_24.max((a - t).abs());
    }
    let mut raw_input = 0.0_f64;
    for p in original {
        if p.freq_hz < lo || p.freq_hz > hi {
            continue;
        }
        let t = interpolate_curve_db(target, p.freq_hz)?;
        let a = p.mag_db + channel.cascade_magnitude_db(p.freq_hz, fs_f);
        raw_input = raw_input.max((a - t).abs());
    }

    let rule_2b_limits = Rule2bLimits::default();
    let passed_2b = worst_point_worsening <= rule_2b_limits.point_worsening_db
        && ripple <= rule_2b_limits.one_third_octave_ripple_db
        && peak_over <= rule_2b_limits.modal_band_peak_over_target_db
        && new_notch_db <= rule_2b_limits.new_narrow_notch_db;

    let modal_residual = {
        let before_band: Vec<f64> = band_idx.iter().map(|&i| residual_before[i]).collect();
        let after_band: Vec<f64> = band_idx.iter().map(|&i| residual_after[i]).collect();
        ModalBandResidual {
            band_hz: [mb_lo, mb_hi],
            rms_before_db: rms(&before_band),
            rms_after_db: rms(&after_band),
            max_before_db: before_band.iter().map(|x| x.abs()).fold(0.0_f64, f64::max),
            max_after_db: after_band.iter().map(|x| x.abs()).fold(0.0_f64, f64::max),
        }
    };

    let mut band_corrections = Vec::with_capacity(channel.bands.len());
    for (bidx, band) in channel.bands.iter().enumerate() {
        let o = interpolate_curve_db(original, band.freq_hz)?;
        let t = interpolate_curve_db(target, band.freq_hz)?;
        band_corrections.push(BandCorrectionReport {
            band_index: bidx,
            band_type: band.band_type,
            freq_hz: band.freq_hz,
            q: band.q,
            gain_db: band.gain_db,
            original_deviation_db: o - t,
            applied_correction_db: band.correction_at_center_db(fs_f),
            residual_db: o + channel.cascade_magnitude_db(band.freq_hz, fs_f) - t,
        });
    }

    let mut warnings = Vec::new();
    if lo > FREQ_MIN_HZ || hi < upper {
        warnings.push(format!(
            "original/target 覆盖 [{lo}, {hi}] 未覆盖 [{FREQ_MIN_HZ}, {upper}]；比对仅覆盖交集"
        ));
    }

    Ok(OriginalComparison {
        modal_band_hz: [mb_lo, mb_hi],
        eval_grid_points: n,
        band_corrections,
        modal_band_residual: modal_residual,
        rule_2a,
        rule_2b: Rule2bReport {
            passed: passed_2b,
            limits: rule_2b_limits,
            worst_point_worsening_db: worst_point_worsening,
            one_third_octave_ripple_db: ripple,
            modal_band_peak_over_target_db: peak_over,
            new_narrow_notch_db: new_notch_db,
            raw_max_deviation_db_1_24_oct: raw_1_24,
            raw_max_deviation_db_at_input_points: raw_input,
            one_third_octave_max_deviation_db: smoothed_dev,
        },
        warnings,
    })
}

fn moving_average(values: &[f64], half_window: usize) -> Vec<f64> {
    let n = values.len();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let start = i.saturating_sub(half_window);
        let end = (i + half_window + 1).min(n);
        let mut sum = 0.0;
        let mut count = 0usize;
        for v in &values[start..end] {
            sum += *v;
            count += 1;
        }
        out.push(if count > 0 { sum / count as f64 } else { 0.0 });
    }
    out
}

fn max_minus_min(values: &[f64]) -> f64 {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for v in values {
        if *v < lo {
            lo = *v;
        }
        if *v > hi {
            hi = *v;
        }
    }
    if lo.is_finite() && hi.is_finite() {
        hi - lo
    } else {
        0.0
    }
}

fn rms(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    (values.iter().map(|v| v * v).sum::<f64>() / values.len() as f64).sqrt()
}

fn detect_new_narrow_notch(grid: &[f64], residual_before: &[f64], residual_after: &[f64]) -> f64 {
    let n = grid.len();
    let mut worst = 0.0_f64;
    let mut i = 0usize;
    while i < n {
        if residual_after[i] > -6.0 {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && residual_after[i] <= -6.0 {
            i += 1;
        }
        let end = i - 1;
        let width_oct = (grid[end] / grid[start]).log2();
        let center = (start + end) / 2;
        let depth_after = -residual_after[center];
        let depth_before = -residual_before[center];
        if width_oct < 1.0 / 3.0 && depth_before < depth_after - 1.0 && depth_after > worst {
            worst = depth_after;
        }
    }
    worst
}

// ---------------------------------------------------------------------------
// 模态检测（spec §5.6 合成夹具；requirements 5 AC1–AC6）
// ---------------------------------------------------------------------------

/// 模态检测配置（缺省对应 requirements 5 的缺省口径）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModalDetectConfig {
    pub fs: f64,
    /// 模态搜索带（默认 [20,250] Hz）。
    pub band_hz: [f64; 2],
    pub max_bands: usize,
    pub min_prominence_db: f64,
    pub max_cut_db: f64,
    pub target_margin_db: f64,
    pub smooth_octaves: f64,
    pub analysis_octaves: f64,
    pub merge_octaves: f64,
}

impl Default for ModalDetectConfig {
    fn default() -> Self {
        Self {
            fs: 48000.0,
            band_hz: DEFAULT_MODAL_BAND_HZ,
            max_bands: MAX_MODAL_PEAKINGS,
            min_prominence_db: DETECT_MIN_PROMINENCE_DB,
            max_cut_db: DETECT_MAX_CUT_DB,
            target_margin_db: DETECT_TARGET_MARGIN_DB,
            smooth_octaves: DETECT_SMOOTH_OCT,
            analysis_octaves: DETECT_ANALYSIS_OCT,
            merge_octaves: DETECT_MERGE_OCT,
        }
    }
}

/// 一个检出模态。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DetectedMode {
    pub freq_hz: f64,
    /// RBJ Q（−3 dB 带宽反算；不夹到 10）。
    pub q: f64,
    /// 只切：恒 ≤ 0。
    pub gain_db: f64,
    pub prominence_db: f64,
    /// 带宽窄于频响网格分辨率时为 true，Q 取网格能支持的最细值。
    pub q_unresolved: bool,
    pub bandwidth_hz: f64,
    /// 输入曲线在 f0 处的局部网格步长。
    pub grid_step_hz: f64,
}

/// 一个检出峰的中间量（供合并/测试观察）。
#[derive(Clone, Copy, Debug, PartialEq)]
struct PeakCandidate {
    freq_hz: f64,
    q: f64,
    gain_db: f64,
    prominence_db: f64,
    q_unresolved: bool,
    bandwidth_hz: f64,
    grid_step_hz: f64,
}

/// 按 requirements 5 AC1–AC6 从频响曲线检测模态段（只切、不填谷、Q 不夹 10、合并/限段）。
pub fn detect_modal_modes(
    curve: &[CurvePoint],
    cfg: &ModalDetectConfig,
) -> Result<Vec<DetectedMode>, PeqError> {
    let candidates = detect_peak_candidates(curve, cfg)?;
    let mut merged: Vec<PeakCandidate> = Vec::new();
    let mut sorted = candidates;
    sorted.sort_by(|a, b| a.freq_hz.partial_cmp(&b.freq_hz).unwrap());
    for cand in sorted {
        match merged.last_mut() {
            Some(prev) if octave_distance(prev.freq_hz, cand.freq_hz) < cfg.merge_octaves => {
                let deeper_is_cand = cand.gain_db < prev.gain_db;
                *prev = PeakCandidate {
                    freq_hz: if deeper_is_cand {
                        cand.freq_hz
                    } else {
                        prev.freq_hz
                    },
                    q: prev.q.max(cand.q),
                    gain_db: prev.gain_db.min(cand.gain_db),
                    prominence_db: prev.prominence_db.max(cand.prominence_db),
                    q_unresolved: prev.q_unresolved || cand.q_unresolved,
                    bandwidth_hz: prev.bandwidth_hz.min(cand.bandwidth_hz),
                    grid_step_hz: prev.grid_step_hz.max(cand.grid_step_hz),
                };
            }
            _ => merged.push(cand),
        }
    }
    if merged.len() > cfg.max_bands {
        merged.sort_by(|a, b| a.gain_db.partial_cmp(&b.gain_db).unwrap());
        merged.truncate(cfg.max_bands);
        merged.sort_by(|a, b| a.freq_hz.partial_cmp(&b.freq_hz).unwrap());
    }
    Ok(merged
        .into_iter()
        .map(|c| DetectedMode {
            freq_hz: c.freq_hz,
            q: c.q,
            gain_db: c.gain_db,
            prominence_db: c.prominence_db,
            q_unresolved: c.q_unresolved,
            bandwidth_hz: c.bandwidth_hz,
            grid_step_hz: c.grid_step_hz,
        })
        .collect())
}

fn detect_peak_candidates(
    curve: &[CurvePoint],
    cfg: &ModalDetectConfig,
) -> Result<Vec<PeakCandidate>, PeqError> {
    validate_curve(curve)?;
    if !cfg.fs.is_finite() || cfg.fs <= 0.0 {
        return err(E_PEQ_FS, format!("检测 fs={} 非法", cfg.fs));
    }
    if !(cfg.band_hz[0].is_finite() && cfg.band_hz[1].is_finite())
        || cfg.band_hz[0] <= 0.0
        || cfg.band_hz[1] <= cfg.band_hz[0]
    {
        return err(E_PEQ_BAND, "检测 band_hz 非法");
    }
    if cfg.max_bands == 0 {
        return Ok(Vec::new());
    }
    if !(cfg.analysis_octaves > 0.0 && cfg.smooth_octaves > 0.0 && cfg.merge_octaves > 0.0) {
        return err(E_PEQ_BAND, "检测 octave 参数须 > 0");
    }
    let f_upper = freq_upper_hz(cfg.fs);
    let lo = cfg.band_hz[0].max(FREQ_MIN_HZ).max(curve[0].freq_hz);
    let hi = cfg.band_hz[1]
        .min(f_upper)
        .min(curve[curve.len() - 1].freq_hz);
    if !(hi > lo * 1.02) {
        return Ok(Vec::new());
    }
    let grid = log_frequency_grid(lo, hi, cfg.analysis_octaves);
    if grid.len() < 16 {
        return Ok(Vec::new());
    }
    let raw: Vec<f64> = grid
        .iter()
        .map(|&f| interpolate_curve_db(curve, f))
        .collect::<Result<Vec<_>, _>>()?;
    let smooth_half = ((cfg.smooth_octaves / 2.0) / cfg.analysis_octaves)
        .round()
        .max(1.0) as usize;
    let smooth = moving_average(&raw, smooth_half);
    let third_half = ((1.0 / 3.0) / cfg.analysis_octaves).round().max(1.0) as usize;

    let mut candidates: Vec<PeakCandidate> = Vec::new();
    for i in 1..grid.len() - 1 {
        if !(smooth[i] >= smooth[i - 1] && smooth[i] > smooth[i + 1]) {
            continue;
        }
        // 峰两侧需有足够的 ±1/3 oct 邻域（不足则跳过，避免边缘伪峰）
        if i < third_half / 2 || i + third_half / 2 >= grid.len() {
            continue;
        }
        let left_start = i.saturating_sub(third_half);
        let right_end = (i + third_half).min(grid.len() - 1);
        let mut left_floor = f64::INFINITY;
        let mut left_idx = i;
        for j in left_start..i {
            if smooth[j] < left_floor {
                left_floor = smooth[j];
                left_idx = j;
            }
        }
        let mut right_floor = f64::INFINITY;
        let mut right_idx = i;
        for j in (i + 1)..=right_end {
            if smooth[j] < right_floor {
                right_floor = smooth[j];
                right_idx = j;
            }
        }
        if !(left_floor.is_finite() && right_floor.is_finite()) {
            continue;
        }
        // requirements 5 AC1：突出度 = 峰值 − max(±1/3 oct 内左右最低点)
        let floor = left_floor.max(right_floor);
        let prominence = smooth[i] - floor;
        if prominence < cfg.min_prominence_db {
            continue;
        }
        // f0 精细定位：对数频率网格上的抛物线插值
        let y0 = smooth[i - 1];
        let y1 = smooth[i];
        let y2 = smooth[i + 1];
        let denom = y0 - 2.0 * y1 + y2;
        let mut delta = if denom.abs() > 1e-12 {
            0.5 * (y0 - y2) / denom
        } else {
            0.0
        };
        delta = delta.clamp(-0.5, 0.5);
        let freq_hz = grid[i] * 2f64.powf(delta * cfg.analysis_octaves);

        // −3 dB 带宽交点：baseline = 两肩最低点连成的 log-f 直线（AC1 邻域最低点法，不用中位数）
        let baseline = |f: f64| -> f64 {
            let x = (f / grid[left_idx]).log2() / (grid[right_idx] / grid[left_idx]).log2();
            left_floor + (right_floor - left_floor) * x
        };
        let peak_residual = interpolate_curve_db(curve, freq_hz)? - baseline(freq_hz);
        let half_level = peak_residual / 2.0;
        let residual_at =
            |f: f64| -> Result<f64, PeqError> { Ok(interpolate_curve_db(curve, f)? - baseline(f)) };
        let find_crossing =
            |start: usize, step: isize, limit: usize| -> Result<Option<f64>, PeqError> {
                let mut j = start as isize;
                let mut prev_f = grid[start];
                let mut prev_r = residual_at(prev_f)?;
                while j != limit as isize {
                    j += step;
                    let f = grid[j as usize];
                    let r = residual_at(f)?;
                    if r <= half_level {
                        let t = if (prev_r - r).abs() > 1e-12 {
                            (prev_r - half_level) / (prev_r - r)
                        } else {
                            0.0
                        };
                        let cross = prev_f * 2f64.powf(t.clamp(0.0, 1.0) * (f / prev_f).log2());
                        return Ok(Some(cross));
                    }
                    prev_f = f;
                    prev_r = r;
                }
                Ok(None)
            };
        let f1 = find_crossing(i, -1, left_idx)?;
        let f2 = find_crossing(i, 1, right_idx)?;
        let grid_step_hz = local_grid_step_hz(curve, freq_hz)?.max(f64::MIN_POSITIVE);
        let (bandwidth_hz, mut q, mut q_unresolved) = match (f1, f2) {
            (Some(a), Some(b)) if b > a => (b - a, freq_hz / (b - a), false),
            _ => (grid_step_hz, freq_hz / grid_step_hz, true),
        };
        // 带宽窄于频响网格分辨率 → q_unresolved，Q 取网格能支持的最细值（f0/网格步长）
        if bandwidth_hz < 2.0 * grid_step_hz {
            q_unresolved = true;
        }
        if q_unresolved {
            q = freq_hz / grid_step_hz;
        }
        let q = q.max(DETECT_MIN_Q);
        // 只切：g = −min(max_cut, prominence − target_margin)，恒 ≤ 0
        let gain_db = -((prominence - cfg.target_margin_db).min(cfg.max_cut_db)).max(0.0);
        candidates.push(PeakCandidate {
            freq_hz,
            q,
            gain_db,
            prominence_db: prominence,
            q_unresolved,
            bandwidth_hz,
            grid_step_hz,
        });
    }
    Ok(candidates)
}

/// 从曲线检测并直接产出通道级 Modal PEQ（band_type=peaking、gain≤0）。
pub fn detect_modal_peq_channel(
    name: &str,
    index: u32,
    curve: &[CurvePoint],
    cfg: &ModalDetectConfig,
) -> Result<ChannelPeq, PeqError> {
    let modes = detect_modal_modes(curve, cfg)?;
    let mut channel = ChannelPeq::new(name, index)?;
    for mode in modes {
        let band = ModalBand::from_params(
            BandType::Peaking,
            mode.freq_hz,
            mode.q,
            mode.gain_db,
            cfg.fs,
        )?;
        channel.push_band(band);
    }
    Ok(channel)
}

/// 便捷入口：曲线 → 单通道设计（modal_only）。
pub fn design_from_curve(
    fs: u32,
    name: &str,
    index: u32,
    curve: &[CurvePoint],
    cfg: &ModalDetectConfig,
) -> Result<PeqDesign, PeqError> {
    let channel = detect_modal_peq_channel(name, index, curve, cfg)?;
    let mut design = PeqDesign::new(fs, DesignMode::ModalOnly)?;
    design.band_hz = cfg.band_hz;
    design.add_channel(channel)?;
    Ok(design)
}

// ---------------------------------------------------------------------------
// 私有测试（tests/T41 通过 include! 进入本模块的 #[cfg(test)] 子模块）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod modal_peq_tests {
    use super::*;

    include!("../tests/T41/modal_peq_cases.rs");
}
