//! [NONRT] linear 相位 FIR 设计（WBS T10）—— T10 写域。
//!
//! 本模块**只**实现 linear 家族（常数群延迟 D=(L−1)/2 的有限窗设计），不把七家族藏进一个包：
//! min_fast/min_slow/apodizing/brickwall/corrected/nos/crossover/phase/modal_peq 各有独立 leaf。
//!
//! 权威契约：rt-stack/spec/fir-toolbox.md v1.2-draft §3.1（linear 参数/参考构造/规范性判据 1–10）、
//! §1.2（响应测量程序）、§1.3（支撑/延迟）、§2.5（E_FIR_* 失败码）、§9 V1–V4/V12/V13（手算向量）。
//!
//! 能力：
//! - 参数：taps、mode=lowpass|highpass、passband_hz、stopband_hz、ripple_db、attenuation_db、
//!   Kaiser/Blackman-Harris 有限窗、symmetric|antisymmetric（antisymmetric 仅 HP）、
//!   可选声明 declared_delay_samples（必须等于 (L−1)/2，否则 E_FIR_DELAY）。
//! - 构造：理想 LP/HP 幅度 + 有限窗（Kaiser beta 由 A_s 按标准公式选；V13 可行性式预检，不足即
//!   E_FIR_BUDGET，不静默截短 taps）；LP 以 DC 归一，HP 以通带几何中心归一，均声明 reference（0 dB）。
//! - 验证：**独立**于构造公式的直接 DTFT（复指数递推 + 每 64 点重锚）在规范测量网格
//!   （对数网格 20 Hz–min(20 kHz,0.499fs)，N >= max(8192, 8·L)）上复算幅度/群延迟（解析式 GD=Re(B/H)，
//!   非有限差分），逐条判 §3.1 判据 1–7，输出 LinearVerification。
//! - 手算/解析向量：V1（偶长对称 H(0.5fs)=0）、V2（D/ms 表）、V3/V4（3/5-tap 响应与 GD）、
//!   V12（延迟预算表）、V13（Kaiser L_req 表）。
//!
//! 明确非目标（不得由本模块冒充）：
//! - 不实现 least_squares 方法：规范 §3.1 的 window.type 列了它，但它不是有限窗；T10 DoD 是
//!   “有限窗 + 独立响应验证”。需要 LS/Remez 由后续任务/裁决另行实现，本模块不给出假系数。
//! - 不生成 WAV/manifest（tspi-fir/v1 产物与 20 Hz/20 kHz Endpoint 报告归 T55），不做 target 导入（T11）、
//!   不做 P/G 融合（T12）、不做 RT 运行/板端（本文件零 RT 路径、零板端动作）。
//! - 预算判定只报 delay_samples/delay_ms 事实；15/25/40 ms 整链预算与 profile 准入归 T55/T56，
//!   本模块不宣称“满足预算”。
//!
//! 裁决（Lead 2026-11-17，A 镜像读法）：§3.1 判据 5 的 HP 侧原文“最后一次离开 <= f_s”是 spec 笔误；
//! HP 应为“穿越点 >= f_s − tol”（衰减至少保持到 f_s，与 LP 的 <= f_s 镜像），且整段 [0, f_s] 阻带
//! 纹波峰值必须 < −A_s（不能靠 0.5 dB 余量掩盖反弹）。T04 已另派 errata 修正；本实现按 A 裁决。
//!
//! 精度/确定性：设计 f64；同一输入同一版本两次 design 系数逐位一致（无时间戳/随机数）。

use std::f64::consts::PI;
use std::fmt;

/// 规范 §3.1 判据 8：最小 taps。
pub const MIN_TAPS: usize = 8;
/// 规范 §2.1/§7.2：设计 taps 上限（>16384 只能由其它离线工具处理）。
pub const MAX_TAPS: usize = 16_384;
/// 规范 §7.2：常规 profile 每路由有效支撑上限（仅事实常量，准入归 T56）。
pub const REGULAR_PROFILE_TAPS_CAP: usize = 4_096;
/// 规范 §7.2：extended profile 上限（8192/16384 需准入；本模块不判定准入）。
pub const EXTENDED_PROFILE_TAPS_CAP: usize = 16_384;
/// 规范 §3.1 判据 8：最小过渡带宽度（Hz）。
pub const MIN_TRANSITION_HZ: f64 = 30.0;
/// 规范 §1.2：测量网格下界（Hz）。
pub const MEASUREMENT_LO_HZ: f64 = 20.0;
/// 规范 §1.2：测量网格上界之一（Hz）。
pub const MEASUREMENT_HI_HZ: f64 = 20_000.0;
/// 规范 §3.1 判据 8/§7.2：Nyquist 护栏比例 0.499·fs。
pub const NYQUIST_GUARD_FRACTION: f64 = 0.499;
/// 规范 §3.1：默认通带纹波（dB）。
pub const DEFAULT_RIPPLE_DB: f64 = 0.5;
/// 规范 §3.1：默认阻带衰减（dB）。
pub const DEFAULT_ATTENUATION_DB: f64 = 80.0;
/// 幅度下限（规范 §1.2：|H| 截到 1e-12，即 −240 dB）。
pub const MAGNITUDE_FLOOR: f64 = 1e-12;
/// linear 家族声明的参考增益（dB）；本模块不偷偷加整体增益。
pub const REFERENCE_GAIN_DB: f64 = 0.0;
/// Blackman-Harris 4 项窗的理论最高旁瓣（dB）；attenuation_db 超过它即 E_FIR_BUDGET。
pub const BLACKMAN_HARRIS_STOPBAND_DB: f64 = 92.0;
/// 规范 §9 V13：14.357078 = 2.285·2π（评审 I11 定值，不用截断 14.3567）。
pub const KAISER_TAPS_NUMERATOR: f64 = 14.357_078;
/// 规范 §3.1 判据 1：对称性容差（相对 max|h|）。
pub const SYMMETRY_TOLERANCE: f64 = 1e-7;
/// 规范 §3.1 判据 2：常数群延迟容差（samples）。
pub const GROUP_DELAY_TOLERANCE_SAMPLES: f64 = 0.01;
/// 规范 §3.1 判据 3：通带纹波允许的附加余量（dB）。
pub const PASSBAND_RIPPLE_MARGIN_DB: f64 = 0.05;
/// 规范 §3.1 判据 4：阻带衰减允许的附加余量（dB）。
pub const STOPBAND_MARGIN_DB: f64 = 0.5;
/// 规范 §3.1 判据 6：参考归一化容差（dB）。
pub const REFERENCE_TOLERANCE_DB: f64 = 0.01;

/// linear 家族的两个 mode（规范 §3.1）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinearMode {
    /// 低通：通带 [20, f_p]，阻带 [f_s, 0.499fs]。
    Lowpass,
    /// 高通：阻带 [20, f_s]，通带 [f_p, passband_hi]（passband_hi <= 0.5fs）。
    Highpass,
}

/// 有限窗类型（T10 只实现有限窗；least_squares 属非窗方法，见模块 docs 非目标）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowKind {
    /// Kaiser 窗，beta 由 attenuation_db 标准公式选取（或显式 beta）。
    Kaiser,
    /// Blackman-Harris 4 项窗（固定约 −92 dB 旁瓣）。
    BlackmanHarris,
}

/// 相位对称类型（规范 §3.1；antisymmetric 仅允许 highpass）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Symmetry {
    /// Type I/II：h[n] = +h[L−1−n]。
    Symmetric,
    /// Type III/IV：h[n] = −h[L−1−n]，H(0)=0（仅 HP）。
    Antisymmetric,
}

/// linear 设计参数（规范 §3.1 参数表；字段可直改，构造器给默认值）。
#[derive(Clone, Debug, PartialEq)]
pub struct LinearSpec {
    /// 运行采样率（Hz）；设计要求 fs 与实际运行 fs 一致（§7.2）。
    pub fs: f64,
    /// 支撑长度 L（taps），[MIN_TAPS, MAX_TAPS]。
    pub taps: usize,
    /// lowpass | highpass。
    pub mode: LinearMode,
    /// 通带：LP=[20, f_p]，HP=[f_p, passband_hi]。
    pub passband_hz: [f64; 2],
    /// 阻带：LP=[f_s, 0.499fs]，HP=[20, f_s]。
    pub stopband_hz: [f64; 2],
    /// 通带允许纹波（max−min，dB），判据为 <= ripple_db + 0.05。
    pub ripple_db: f64,
    /// 阻带要求衰减（dB），判据为 stopband max <= −(attenuation_db − 0.5)。
    pub attenuation_db: f64,
    /// 有限窗类型。
    pub window: WindowKind,
    /// Kaiser beta；None 时由 attenuation_db 推导。
    pub beta: Option<f64>,
    /// 对称类型；antisymmetric 仅 HP（LP 出现即 E_FIR_BAND）。
    pub symmetry: Symmetry,
    /// 可选声明体延迟 D（samples）；linear 家族必须等于 (L−1)/2，不一致即 E_FIR_DELAY。
    pub declared_delay_samples: Option<f64>,
}

impl LinearSpec {
    /// 低通参数构造器：通带 [20, passband_hi_hz]，阻带 [stopband_lo_hz, 0.499·fs]。
    ///
    /// 默认：Kaiser 窗（beta 由 attenuation_db=80 dB 推导）、symmetric、ripple 0.5 dB、无声明 D。
    pub fn lowpass(fs: f64, taps: usize, passband_hi_hz: f64, stopband_lo_hz: f64) -> Self {
        Self {
            fs,
            taps,
            mode: LinearMode::Lowpass,
            passband_hz: [MEASUREMENT_LO_HZ, passband_hi_hz],
            stopband_hz: [stopband_lo_hz, NYQUIST_GUARD_FRACTION * fs],
            ripple_db: DEFAULT_RIPPLE_DB,
            attenuation_db: DEFAULT_ATTENUATION_DB,
            window: WindowKind::Kaiser,
            beta: None,
            symmetry: Symmetry::Symmetric,
            declared_delay_samples: None,
        }
    }

    /// 高通参数构造器：阻带 [20, stopband_hi_hz]，通带 [passband_lo_hz, 0.499·fs]。
    ///
    /// 默认同 LinearSpec::lowpass（symmetric）。若需通带到 Nyquist（0.5fs），用
    /// LinearSpec::highpass_to_nyquist（Type I/IV 结构约束，见 §3.1 判据 9）。
    pub fn highpass(fs: f64, taps: usize, stopband_hi_hz: f64, passband_lo_hz: f64) -> Self {
        Self {
            fs,
            taps,
            mode: LinearMode::Highpass,
            passband_hz: [passband_lo_hz, NYQUIST_GUARD_FRACTION * fs],
            stopband_hz: [MEASUREMENT_LO_HZ, stopband_hi_hz],
            ripple_db: DEFAULT_RIPPLE_DB,
            attenuation_db: DEFAULT_ATTENUATION_DB,
            window: WindowKind::Kaiser,
            beta: None,
            symmetry: Symmetry::Symmetric,
            declared_delay_samples: None,
        }
    }

    /// 高通且通带上界取到 Nyquist（0.5·fs）：要求结构在 π 处不强制为零
    /// （symmetric 需奇数 taps=Type I；antisymmetric 需偶数 taps=Type IV），否则 E_FIR_HP_NYQ。
    pub fn highpass_to_nyquist(
        fs: f64,
        taps: usize,
        stopband_hi_hz: f64,
        passband_lo_hz: f64,
    ) -> Self {
        let mut spec = Self::highpass(fs, taps, stopband_hi_hz, passband_lo_hz);
        spec.passband_hz[1] = 0.5 * fs;
        spec
    }

    /// 设置 ripple_db。
    pub fn with_ripple(mut self, ripple_db: f64) -> Self {
        self.ripple_db = ripple_db;
        self
    }

    /// 设置 attenuation_db。
    pub fn with_attenuation(mut self, attenuation_db: f64) -> Self {
        self.attenuation_db = attenuation_db;
        self
    }

    /// 设置有限窗类型。
    pub fn with_window(mut self, window: WindowKind) -> Self {
        self.window = window;
        self
    }

    /// 显式设置 Kaiser beta（仅 Kaiser 窗；BlackmanHarris 不允许 beta）。
    pub fn with_kaiser_beta(mut self, beta: f64) -> Self {
        self.beta = Some(beta);
        self
    }

    /// 设置对称类型。
    pub fn with_symmetry(mut self, symmetry: Symmetry) -> Self {
        self.symmetry = symmetry;
        self
    }

    /// 声明体延迟 D（samples）；linear 必须 = (L−1)/2。
    pub fn with_declared_delay(mut self, delay_samples: f64) -> Self {
        self.declared_delay_samples = Some(delay_samples);
        self
    }

    /// 通带内侧边界（过渡带一侧）：LP=f_p（通带上界），HP=f_p（通带下界）。
    pub fn passband_edge_hz(&self) -> f64 {
        match self.mode {
            LinearMode::Lowpass => self.passband_hz[1],
            LinearMode::Highpass => self.passband_hz[0],
        }
    }

    /// 阻带内侧边界（过渡带一侧）：LP=f_s（阻带下界），HP=f_s（阻带上界）。
    pub fn stopband_edge_hz(&self) -> f64 {
        match self.mode {
            LinearMode::Lowpass => self.stopband_hz[0],
            LinearMode::Highpass => self.stopband_hz[1],
        }
    }
}

/// E_FIR_* 失败码（规范 §2.5）承载的错误；不发明规范外成功路径。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinearError {
    /// E_FIR_BAND：频带/参数非法（含 taps 越界、antisymmetric LP、过渡带 <30 Hz、band 结构非法）。
    Band(String),
    /// E_FIR_DELAY：声明 D 与 (L−1)/2 不一致或 delay_ms 不一致。
    Delay(String),
    /// E_FIR_TAPS：系数长度与声明 taps 不一致。
    Taps(String),
    /// E_FIR_BUDGET：可行性/支撑不足（Kaiser L_req > taps、BH 衰减超窗能力、判据不可达），不静默截短。
    Budget(String),
    /// E_FIR_NUMERIC：系数含 NaN/Inf 或 max|h|=0，或归一化失败。
    Numeric(String),
    /// E_FIR_PHASE_CLASS：linear 对称/常数 GD 不满足。
    PhaseClass(String),
    /// E_FIR_HP_NYQ：HP 要求通带到 fs/2 但结构在该处必为零（symmetric 偶长 / antisymmetric 奇长）。
    HpNyquist(String),
    /// E_FIR_REF：参考归一化不满足 ±0.01 dB。
    Reference(String),
}

impl LinearError {
    /// 规范 §2.5 的失败码字符串。
    pub fn code(&self) -> &'static str {
        match self {
            LinearError::Band(_) => "E_FIR_BAND",
            LinearError::Delay(_) => "E_FIR_DELAY",
            LinearError::Taps(_) => "E_FIR_TAPS",
            LinearError::Budget(_) => "E_FIR_BUDGET",
            LinearError::Numeric(_) => "E_FIR_NUMERIC",
            LinearError::PhaseClass(_) => "E_FIR_PHASE_CLASS",
            LinearError::HpNyquist(_) => "E_FIR_HP_NYQ",
            LinearError::Reference(_) => "E_FIR_REF",
        }
    }

    /// 人类可读细节。
    pub fn detail(&self) -> &str {
        match self {
            LinearError::Band(s)
            | LinearError::Delay(s)
            | LinearError::Taps(s)
            | LinearError::Budget(s)
            | LinearError::Numeric(s)
            | LinearError::PhaseClass(s)
            | LinearError::HpNyquist(s)
            | LinearError::Reference(s) => s,
        }
    }
}

impl fmt::Display for LinearError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.detail())
    }
}

impl std::error::Error for LinearError {}

/// 通过 validate 的完整设计参数（派生量集中在这里，构造与验证共用同一口径）。
#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedLinearSpec {
    /// 原参数（克隆）。
    pub spec: LinearSpec,
    /// 通带内侧边界（Hz）。
    pub passband_edge_hz: f64,
    /// 阻带内侧边界（Hz）。
    pub stopband_edge_hz: f64,
    /// 体延迟 D=(L−1)/2（samples）。
    pub delay_samples: f64,
    /// delay_ms = D/fs·1000。
    pub delay_ms: f64,
    /// 归一化参考频点：LP=0（DC），HP=sqrt(passband_lo·passband_hi)。
    pub reference_hz: f64,
    /// Kaiser 实际 beta（Kaiser 窗时 Some）。
    pub kaiser_beta: Option<f64>,
    /// Kaiser 可行性式所需 taps（attenuation_db>=40 时 Some）。
    pub kaiser_required_taps: Option<u64>,
}

/// 规范化 sinc：sin(πx)/(πx)，sinc(0)=1。
pub fn sinc_normalized(x: f64) -> f64 {
    if x == 0.0 {
        1.0
    } else {
        let px = PI * x;
        px.sin() / px
    }
}

/// 零阶第一类修正 Bessel 函数 I0(x)（级数，Kaiser 窗用；x 个位数量级足够）。
pub fn bessel_i0(x: f64) -> f64 {
    let ax = x.abs();
    let q = ax * ax / 4.0;
    let mut sum = 1.0_f64;
    let mut term = 1.0_f64;
    let mut k = 1_u32;
    while k < 10_000 {
        let kf = f64::from(k);
        term *= q / (kf * kf);
        sum += term;
        if term.abs() <= 1e-18 * sum.abs() {
            break;
        }
        k += 1;
    }
    sum
}

/// Kaiser beta 标准公式（规范 §3.1 参考构造：按 A_s 选 beta）：
/// A>50: 0.1102(A−8.7)；21<=A<=50: 0.5842(A−21)^0.4 + 0.07886(A−21)；A<21: 0。
pub fn kaiser_beta_for_attenuation(attenuation_db: f64) -> f64 {
    let a = if attenuation_db.is_finite() {
        attenuation_db.max(0.0)
    } else {
        0.0
    };
    if a > 50.0 {
        0.1102 * (a - 8.7)
    } else if a >= 21.0 {
        0.5842 * (a - 21.0).powf(0.4) + 0.07886 * (a - 21.0)
    } else {
        0.0
    }
}

/// 规范 §9 V13 Kaiser 可行性式：L_req = ceil(1 + (A_s−7.95)·fs/(14.357078·Δf))。
///
/// 仅对有限正 fs/Δf 定义；A_s>=40 时为规范给出的可行预检口径（V13 表），A_s<40 仅供参考。
pub fn kaiser_required_taps(attenuation_db: f64, fs: f64, transition_hz: f64) -> Option<u64> {
    if !fs.is_finite()
        || fs <= 0.0
        || !transition_hz.is_finite()
        || transition_hz <= 0.0
        || !attenuation_db.is_finite()
    {
        return None;
    }
    let raw = 1.0 + (attenuation_db - 7.95) * fs / (KAISER_TAPS_NUMERATOR * transition_hz);
    if !raw.is_finite() {
        return None;
    }
    if raw <= 1.0 {
        return Some(1);
    }
    Some(raw.ceil() as u64)
}

/// Kaiser 窗（长度 taps，居中、精确镜像对称）；taps=1 返回 [1]。
pub fn kaiser_window(taps: usize, beta: f64) -> Vec<f64> {
    let mut w = vec![0.0_f64; taps];
    if taps == 0 {
        return w;
    }
    if taps == 1 {
        w[0] = 1.0;
        return w;
    }
    let b = beta.abs();
    let denom = bessel_i0(b);
    let mid = (taps - 1) / 2;
    for i in 0..=mid {
        let t = 2.0 * i as f64 / (taps as f64 - 1.0) - 1.0;
        let val = bessel_i0(b * (1.0 - t * t).max(0.0).sqrt()) / denom;
        w[i] = val;
        w[taps - 1 - i] = val;
    }
    w
}

/// Blackman-Harris 4 项窗（长度 taps，居中、精确镜像对称）；taps=1 返回 [1]。
pub fn blackman_harris_window(taps: usize) -> Vec<f64> {
    const A0: f64 = 0.358_75;
    const A1: f64 = 0.488_29;
    const A2: f64 = 0.141_28;
    const A3: f64 = 0.011_68;
    let mut w = vec![0.0_f64; taps];
    if taps == 0 {
        return w;
    }
    if taps == 1 {
        w[0] = 1.0;
        return w;
    }
    let mid = (taps - 1) / 2;
    for i in 0..=mid {
        let x = 2.0 * PI * i as f64 / (taps as f64 - 1.0);
        let val = A0 - A1 * x.cos() + A2 * (2.0 * x).cos() - A3 * (3.0 * x).cos();
        w[i] = val;
        w[taps - 1 - i] = val;
    }
    w
}

/// linear 家族体延迟 D=(L−1)/2（samples；偶长 taps 时为半整数，不得取整隐藏）。
pub fn delay_samples(taps: usize) -> f64 {
    if taps == 0 {
        0.0
    } else {
        (taps - 1) as f64 / 2.0
    }
}

/// linear 家族 delay_ms = D/fs·1000。
pub fn delay_ms(taps: usize, fs: f64) -> f64 {
    if !fs.is_finite() || fs <= 0.0 {
        return f64::NAN;
    }
    delay_samples(taps) / fs * 1000.0
}

/// 复频响 H(f)（实部/虚部）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FreqResponse {
    /// 实部。
    pub re: f64,
    /// 虚部。
    pub im: f64,
}

impl FreqResponse {
    /// |H|。
    pub fn magnitude(&self) -> f64 {
        self.re.hypot(self.im)
    }

    /// 20·log10(max(|H|, 1e-12))（规范 §1.2 下限 −240 dB）。
    pub fn magnitude_db(&self) -> f64 {
        20.0 * self.magnitude().max(MAGNITUDE_FLOOR).log10()
    }

    /// wrap 到 (−180,180] 的相位（度）。
    pub fn phase_deg(&self) -> f64 {
        self.im.atan2(self.re) * 180.0 / PI
    }
}

/// 直接 DTFT 求 H(f)（O(L)；复指数递推 + 每 64 点重锚，独立于窗构造）。
///
/// fs<=0 或 f 非有限时返回 NaN 字段（不 panic）。
pub fn response_at(coeffs: &[f64], fs: f64, f_hz: f64) -> FreqResponse {
    if !fs.is_finite() || fs <= 0.0 || !f_hz.is_finite() {
        return FreqResponse {
            re: f64::NAN,
            im: f64::NAN,
        };
    }
    let (re, im, _) = eval_h_and_gd(coeffs, fs, f_hz);
    FreqResponse { re, im }
}

/// 20·log10|H(f)|（下限 −240 dB）；非法输入为 NaN。
pub fn magnitude_db_at(coeffs: &[f64], fs: f64, f_hz: f64) -> f64 {
    response_at(coeffs, fs, f_hz).magnitude_db()
}

/// 解析群延迟 GD(f)=Re(B/H)，B=Σ n·h[n]e^{−jωn}（不是有限差分；§1.2 允许中心差分，解析式更准）。
///
/// 非法输入或 |H| 过小时为 NaN。
pub fn group_delay_at(coeffs: &[f64], fs: f64, f_hz: f64) -> f64 {
    if !fs.is_finite() || fs <= 0.0 || !f_hz.is_finite() {
        return f64::NAN;
    }
    let (re, im, gd) = eval_h_and_gd(coeffs, fs, f_hz);
    let den = re * re + im * im;
    if den <= 0.0 || !den.is_finite() {
        return f64::NAN;
    }
    gd
}

/// 求 H 与 GD：返回 (re, im, gd)。
fn eval_h_and_gd(coeffs: &[f64], fs: f64, f_hz: f64) -> (f64, f64, f64) {
    let omega = 2.0 * PI * f_hz / fs;
    let step_re = (-omega).cos();
    let step_im = (-omega).sin();
    let mut cur_re = 1.0_f64;
    let mut cur_im = 0.0_f64;
    let mut h_re = 0.0_f64;
    let mut h_im = 0.0_f64;
    let mut b_re = 0.0_f64;
    let mut b_im = 0.0_f64;
    for (n, &h) in coeffs.iter().enumerate() {
        h_re += h * cur_re;
        h_im += h * cur_im;
        let nf = n as f64;
        b_re += nf * h * cur_re;
        b_im += nf * h * cur_im;
        let next_re = cur_re * step_re - cur_im * step_im;
        let next_im = cur_re * step_im + cur_im * step_re;
        cur_re = next_re;
        cur_im = next_im;
        if n % 64 == 63 {
            let angle = -omega * (nf + 1.0);
            cur_re = angle.cos();
            cur_im = angle.sin();
        }
    }
    let den = h_re * h_re + h_im * h_im;
    let gd = if den > 0.0 && den.is_finite() {
        (b_re * h_re + b_im * h_im) / den
    } else {
        f64::NAN
    };
    (h_re, h_im, gd)
}

/// 对称性误差 max|h[n] − s·h[L−1−n]|（规范 §3.1 判据 1 的绝对量；SPEC 容差=1e-7·max|h|）。
pub fn symmetry_error(coeffs: &[f64], symmetry: Symmetry) -> f64 {
    if coeffs.is_empty() {
        return f64::NAN;
    }
    let sign = match symmetry {
        Symmetry::Symmetric => 1.0,
        Symmetry::Antisymmetric => -1.0,
    };
    let l = coeffs.len();
    let mut max_err = 0.0_f64;
    for n in 0..l {
        let err = (coeffs[n] - sign * coeffs[l - 1 - n]).abs();
        max_err = max_err.max(err);
    }
    max_err
}

/// 有效支撑（规范 §1.3）：(first, last, effective_taps)；eps 为相对 max|h| 的门限。
///
/// 空数组或 max|h|=0 时返回 (0, 0, 0)。
pub fn effective_support(coeffs: &[f64], eps: f64) -> (usize, usize, usize) {
    let mut max_abs = 0.0_f64;
    for &h in coeffs {
        max_abs = max_abs.max(h.abs());
    }
    if coeffs.is_empty() || !max_abs.is_finite() || max_abs == 0.0 {
        return (0, 0, 0);
    }
    let threshold = eps.max(0.0) * max_abs;
    let first = coeffs.iter().position(|h| h.abs() > threshold);
    let last = coeffs.iter().rposition(|h| h.abs() > threshold);
    match (first, last) {
        (Some(fi), Some(la)) => (fi, la, la - fi + 1),
        _ => (0, 0, 0),
    }
}

/// 校验 linear 参数（§3.1 判据 8/参数约束），返回派生量；失败带 E_FIR_* 码。
pub fn validate(spec: &LinearSpec) -> Result<ValidatedLinearSpec, LinearError> {
    use LinearError as E;

    if !spec.fs.is_finite() || spec.fs <= 0.0 {
        return Err(E::Band(format!("fs 必须有限且 >0，实际 {}", spec.fs)));
    }
    if spec.taps < MIN_TAPS || spec.taps > MAX_TAPS {
        return Err(E::Band(format!(
            "taps 必须在 [{MIN_TAPS}, {MAX_TAPS}]，实际 {}",
            spec.taps
        )));
    }
    if !spec.ripple_db.is_finite() || spec.ripple_db < 0.0 {
        return Err(E::Band(format!(
            "ripple_db 必须有限且 >=0，实际 {}",
            spec.ripple_db
        )));
    }
    if !spec.attenuation_db.is_finite() || spec.attenuation_db <= 0.0 {
        return Err(E::Band(format!(
            "attenuation_db 必须有限且 >0，实际 {}",
            spec.attenuation_db
        )));
    }
    if spec.attenuation_db > 200.0 {
        return Err(E::Budget(format!(
            "attenuation_db={} 超出可行范围（>200 dB）",
            spec.attenuation_db
        )));
    }
    if !spec.passband_hz[0].is_finite()
        || !spec.passband_hz[1].is_finite()
        || !spec.stopband_hz[0].is_finite()
        || !spec.stopband_hz[1].is_finite()
    {
        return Err(E::Band("passband_hz/stopband_hz 含非有限值".to_string()));
    }

    let nyq = 0.5 * spec.fs;
    let guard = NYQUIST_GUARD_FRACTION * spec.fs;
    let pass_edge = spec.passband_edge_hz();
    let stop_edge = spec.stopband_edge_hz();
    let tol_hz = 1e-9 * (spec.fs.abs() + 1.0);

    match spec.mode {
        LinearMode::Lowpass => {
            if (spec.passband_hz[0] - MEASUREMENT_LO_HZ).abs() > 1e-9 {
                return Err(E::Band(format!(
                    "LP passband_hz[0] 必须=20 Hz（测量网格下界），实际 {}",
                    spec.passband_hz[0]
                )));
            }
            if !(MEASUREMENT_LO_HZ < pass_edge && pass_edge < stop_edge) {
                return Err(E::Band(format!(
                    "LP 要求 20 < f_p < f_s，实际 f_p={pass_edge} f_s={stop_edge}"
                )));
            }
            if stop_edge > guard + tol_hz {
                return Err(E::Band(format!(
                    "LP f_s 必须 <= 0.499fs={guard}，实际 {stop_edge}"
                )));
            }
            if stop_edge - pass_edge < MIN_TRANSITION_HZ - 1e-9 {
                return Err(E::Band(format!(
                    "LP 过渡带 f_s−f_p 必须 >= {MIN_TRANSITION_HZ} Hz，实际 {}",
                    stop_edge - pass_edge
                )));
            }
            if (spec.stopband_hz[1] - guard).abs() > tol_hz {
                return Err(E::Band(format!(
                    "LP stopband_hz[1] 必须=0.499fs={guard}，实际 {}",
                    spec.stopband_hz[1]
                )));
            }
            if spec.symmetry == Symmetry::Antisymmetric {
                return Err(E::Band(
                    "antisymmetric 仅允许 HP（Type III/IV 的 H(0)=0，LP 必须 DC 通带）".to_string(),
                ));
            }
        }
        LinearMode::Highpass => {
            if (spec.stopband_hz[0] - MEASUREMENT_LO_HZ).abs() > 1e-9 {
                return Err(E::Band(format!(
                    "HP stopband_hz[0] 必须=20 Hz（测量网格下界），实际 {}",
                    spec.stopband_hz[0]
                )));
            }
            if !(MEASUREMENT_LO_HZ < stop_edge && stop_edge < pass_edge) {
                return Err(E::Band(format!(
                    "HP 要求 20 < f_s < f_p，实际 f_s={stop_edge} f_p={pass_edge}"
                )));
            }
            if pass_edge > guard + tol_hz {
                return Err(E::Band(format!(
                    "HP f_p 必须 <= 0.499fs={guard}，实际 {pass_edge}"
                )));
            }
            if pass_edge - stop_edge < MIN_TRANSITION_HZ - 1e-9 {
                return Err(E::Band(format!(
                    "HP 过渡带 f_p−f_s 必须 >= {MIN_TRANSITION_HZ} Hz，实际 {}",
                    pass_edge - stop_edge
                )));
            }
            let pass_hi = spec.passband_hz[1];
            if !(pass_hi >= pass_edge && pass_hi <= nyq + tol_hz) {
                return Err(E::Band(format!(
                    "HP passband_hz[1] 必须在 [f_p, 0.5fs={nyq}]，实际 {pass_hi}"
                )));
            }
            if pass_hi >= nyq - tol_hz {
                // 规范 §3.1 判据 9：π 处被结构强制为零时不得声明通带到 fs/2。
                let forced_zero_at_pi = match spec.symmetry {
                    Symmetry::Symmetric => spec.taps % 2 == 0,     // Type II
                    Symmetry::Antisymmetric => spec.taps % 2 == 1, // Type III
                };
                if forced_zero_at_pi {
                    return Err(E::HpNyquist(format!(
                        "HP 通带包含 0.5fs 要求 H(0.5fs)!=0；taps={} 的 {:?} 结构在 π 必为零，需改 parity（Type I/IV）",
                        spec.taps, spec.symmetry
                    )));
                }
            }
        }
    }

    if spec.window == WindowKind::BlackmanHarris && spec.beta.is_some() {
        return Err(E::Band(
            "BlackmanHarris 窗不接受 beta（beta 仅 Kaiser）".to_string(),
        ));
    }
    let kaiser_beta = match spec.window {
        WindowKind::Kaiser => {
            let beta = spec
                .beta
                .unwrap_or_else(|| kaiser_beta_for_attenuation(spec.attenuation_db));
            if !beta.is_finite() || beta < 0.0 {
                return Err(E::Band(format!("Kaiser beta 必须有限且 >=0，实际 {beta}")));
            }
            Some(beta)
        }
        WindowKind::BlackmanHarris => None,
    };

    let d = delay_samples(spec.taps);
    if let Some(declared) = spec.declared_delay_samples {
        if !declared.is_finite() || (declared - d).abs() > 1e-9 {
            return Err(E::Delay(format!(
                "linear 声明 D 必须=(L−1)/2={d}，实际 {declared}"
            )));
        }
    }
    let d_ms = d / spec.fs * 1000.0;
    let reference_hz = match spec.mode {
        LinearMode::Lowpass => 0.0,
        LinearMode::Highpass => (spec.passband_hz[0] * spec.passband_hz[1]).sqrt(),
    };
    let transition_hz = (stop_edge - pass_edge).abs();
    let kaiser_required_taps = if spec.window == WindowKind::Kaiser && spec.attenuation_db >= 40.0 {
        kaiser_required_taps(spec.attenuation_db, spec.fs, transition_hz)
    } else {
        None
    };

    Ok(ValidatedLinearSpec {
        spec: spec.clone(),
        passband_edge_hz: pass_edge,
        stopband_edge_hz: stop_edge,
        delay_samples: d,
        delay_ms: d_ms,
        reference_hz,
        kaiser_beta,
        kaiser_required_taps,
    })
}

/// linear 设计产物（tspi-fir/v1 manifest 的系数源；WAV/manifest 归 T55）。
#[derive(Clone, Debug, PartialEq)]
pub struct LinearDesign {
    /// 原参数。
    pub spec: LinearSpec,
    /// f64 冲激响应（len=taps，因果）。
    pub coeffs: Vec<f64>,
    /// 实际使用的有限窗。
    pub window: WindowKind,
    /// 实际使用的 Kaiser beta（非 Kaiser 窗为 None）。
    pub kaiser_beta: Option<f64>,
    /// 对称类型。
    pub symmetry: Symmetry,
    /// D=(L−1)/2（samples）。
    pub delay_samples: f64,
    /// D/fs·1000（ms）。
    pub delay_ms: f64,
    /// 归一化参考频点（LP=DC，HP=通带几何中心）。
    pub reference_hz: f64,
    /// 声明的参考增益（dB，本模块固定 0 dB）。
    pub reference_gain_db: f64,
    /// max|h|。
    pub peak_abs: f64,
    /// Σh（LP≈1；HP≈0）。
    pub sum_coeffs: f64,
}

impl LinearDesign {
    /// taps = len(h)。
    pub fn taps(&self) -> usize {
        self.coeffs.len()
    }

    /// f32 导出（WAV/运行器用）。
    pub fn coeffs_f32(&self) -> Vec<f32> {
        self.coeffs.iter().map(|&h| h as f32).collect()
    }

    /// 有效支撑 (first, last, effective_taps)，eps=1e-6（规范 §1.3）。
    pub fn effective_support(&self) -> (usize, usize, usize) {
        effective_support(&self.coeffs, 1e-6)
    }
}

/// 构造 linear 系数（只构造 + 结构自检，不做全网格响应判定；推荐 design_checked）。
///
/// 失败码：E_FIR_BAND/E_FIR_DELAY/E_FIR_BUDGET/E_FIR_NUMERIC/E_FIR_PHASE_CLASS/E_FIR_REF/E_FIR_HP_NYQ。
pub fn design(spec: &LinearSpec) -> Result<LinearDesign, LinearError> {
    let v = validate(spec)?;

    // 可行性预检（不静默截短 taps）。
    match spec.window {
        WindowKind::Kaiser => {
            if let Some(required) = v.kaiser_required_taps {
                if spec.taps < required as usize {
                    return Err(LinearError::Budget(format!(
                        "Kaiser 可行性式要求 taps >= {required}（A_s={} dB, Δf={} Hz, fs={}），实际 {}；不得截短/降级冒充",
                        spec.attenuation_db,
                        (v.stopband_edge_hz - v.passband_edge_hz).abs(),
                        spec.fs,
                        spec.taps
                    )));
                }
            }
        }
        WindowKind::BlackmanHarris => {
            if spec.attenuation_db > BLACKMAN_HARRIS_STOPBAND_DB {
                return Err(LinearError::Budget(format!(
                    "BlackmanHarris 4 项窗旁瓣约 −{BLACKMAN_HARRIS_STOPBAND_DB} dB，无法满足 attenuation_db={}（不静默放宽）",
                    spec.attenuation_db
                )));
            }
        }
    }

    let coeffs = build_coefficients(spec, &v)?;

    let mut peak_abs = 0.0_f64;
    for &h in &coeffs {
        if !h.is_finite() {
            return Err(LinearError::Numeric("系数含 NaN/Inf".to_string()));
        }
        peak_abs = peak_abs.max(h.abs());
    }
    if peak_abs <= 0.0 || !peak_abs.is_finite() {
        return Err(LinearError::Numeric("max|h|=0 或非有限".to_string()));
    }
    let sym_err = symmetry_error(&coeffs, spec.symmetry);
    if !(sym_err <= 1e-9 * peak_abs) {
        return Err(LinearError::PhaseClass(format!(
            "linear 对称性构造失败：max|h−s·flip(h)|={sym_err} > 1e-9·max|h|"
        )));
    }
    let reference = response_at(&coeffs, spec.fs, v.reference_hz).magnitude_db();
    if !reference.is_finite() || reference.abs() > 1e-6 {
        return Err(LinearError::Reference(format!(
            "参考归一化失败：|H({} Hz)|={reference} dB（目标 0 dB）",
            v.reference_hz
        )));
    }
    let sum_coeffs: f64 = coeffs.iter().sum();
    Ok(LinearDesign {
        spec: spec.clone(),
        coeffs,
        window: spec.window,
        kaiser_beta: v.kaiser_beta,
        symmetry: spec.symmetry,
        delay_samples: v.delay_samples,
        delay_ms: v.delay_ms,
        reference_hz: v.reference_hz,
        reference_gain_db: REFERENCE_GAIN_DB,
        peak_abs,
        sum_coeffs,
    })
}

/// 构造系数（内部）：LP/HP × symmetric/antisymmetric 的有限窗合成 + 归一化。
fn build_coefficients(spec: &LinearSpec, v: &ValidatedLinearSpec) -> Result<Vec<f64>, LinearError> {
    let l = spec.taps;
    let fs = spec.fs;
    let d = v.delay_samples;
    let fc = 0.5 * (v.passband_edge_hz + v.stopband_edge_hz);
    let window = window_values(spec.window, l, v.kaiser_beta)?;
    let mut h = vec![0.0_f64; l];
    let mid = (l - 1) / 2;

    match (spec.mode, spec.symmetry) {
        (LinearMode::Lowpass, Symmetry::Symmetric) => {
            // 理想 LP：2fc/fs · sinc(2fc/fs·(n−D))，DC 归一。
            let r = 2.0 * fc / fs;
            for i in 0..=mid {
                let m = i as f64 - d;
                let val = r * sinc_normalized(r * m) * window[i];
                h[i] = val;
                h[l - 1 - i] = val;
            }
            let sum: f64 = h.iter().sum();
            if !sum.is_finite() || sum <= 0.0 {
                return Err(LinearError::Numeric(format!(
                    "LP DC 归一失败：Σh={sum}（应≈1）"
                )));
            }
            let scale = 1.0 / sum;
            for x in h.iter_mut() {
                *x *= scale;
            }
        }
        (LinearMode::Highpass, Symmetry::Symmetric) => {
            // 理想 HP：δ(n−D) − LP_ideal；减均值强制 DC=0；按通带几何中心幅值归一。
            let r = 2.0 * fc / fs;
            for i in 0..=mid {
                let m = i as f64 - d;
                let val = window[i] * (sinc_normalized(m) - r * sinc_normalized(r * m));
                h[i] = val;
                h[l - 1 - i] = val;
            }
            let mean = h.iter().sum::<f64>() / l as f64;
            for x in h.iter_mut() {
                *x -= mean;
            }
            normalize_to_reference(&mut h, fs, v.reference_hz)?;
        }
        (LinearMode::Highpass, Symmetry::Antisymmetric) => {
            // Type III/IV：h[D+m] = w·(cos(ωc·m) − cos(π·m))/(π·m)（m≠0），
            // 逐对取反对称值（精确 h[n] = −h[L−1−n]），中心恒 0。
            let wc = 2.0 * PI * fc / fs;
            for i in 0..(l / 2) {
                let m = i as f64 - d;
                let val = window[i] * antisymmetric_hp_ideal(m, wc);
                h[i] = val;
                h[l - 1 - i] = -val;
            }
            normalize_to_reference(&mut h, fs, v.reference_hz)?;
        }
        (LinearMode::Lowpass, Symmetry::Antisymmetric) => {
            return Err(LinearError::Band(
                "antisymmetric 仅允许 HP（LP 必须 DC 通带）".to_string(),
            ));
        }
    }
    Ok(h)
}

/// 反对称 HP 的理想（未加窗）冲激值：m≠0 时 (cos(ωc·m) − cos(π·m))/(π·m)；m=0 时 0（Type III）。
fn antisymmetric_hp_ideal(m: f64, wc: f64) -> f64 {
    if m == 0.0 {
        return 0.0;
    }
    ((wc * m).cos() - (PI * m).cos()) / (PI * m)
}

/// 按 |H(f_ref)|=1 缩放（正缩放，不改极性/对称性）。
fn normalize_to_reference(h: &mut [f64], fs: f64, f_ref: f64) -> Result<(), LinearError> {
    let mag = response_at(h, fs, f_ref).magnitude();
    if !mag.is_finite() || mag < MAGNITUDE_FLOOR {
        return Err(LinearError::Numeric(format!(
            "参考点 f_ref={f_ref} Hz 幅值过小/非有限（|H|={mag}），无法归一"
        )));
    }
    let scale = 1.0 / mag;
    for x in h.iter_mut() {
        *x *= scale;
    }
    Ok(())
}

/// 窗值（长度 taps，精确镜像对称）。
fn window_values(
    kind: WindowKind,
    taps: usize,
    beta: Option<f64>,
) -> Result<Vec<f64>, LinearError> {
    match kind {
        WindowKind::Kaiser => {
            let b = beta.ok_or_else(|| {
                LinearError::Band("Kaiser 窗缺少 beta（validate 应已给出）".to_string())
            })?;
            Ok(kaiser_window(taps, b))
        }
        WindowKind::BlackmanHarris => Ok(blackman_harris_window(taps)),
    }
}

/// 独立响应验证的网格/点数选项（默认按规范 §1.2；compact 便于快速自检，正式报告用 normative）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnalysisOptions {
    /// 主幅度网格点数（规范：>= max(8192, 8·L)）。
    pub mag_points: usize,
    /// 过渡带线性网格点数（规范：同 N）。
    pub transition_points: usize,
    /// 20 kHz/0.499fs 之外到 0.499fs（LP 阻带）或通带上界（HP）的扩展网格点数。
    pub extension_points: usize,
}

impl AnalysisOptions {
    /// 规范 §1.2 网格：N = max(8192, 8·L)；过渡带用同 N 点；扩展 256 点覆盖 20 kHz→0.499fs（LP）。
    pub fn normative(spec: &LinearSpec) -> Self {
        let n = (8 * spec.taps).max(8192);
        Self {
            mag_points: n,
            transition_points: n,
            extension_points: 256,
        }
    }

    /// 快速自检网格（测试/预检用；不用于正式报告，正式报告必须 normative）。
    pub fn compact(taps: usize) -> Self {
        let n = (2 * taps).clamp(1024, 4096);
        let t = (2 * taps).clamp(2048, 4096);
        Self {
            mag_points: n,
            transition_points: t,
            extension_points: 64,
        }
    }
}

/// 频带极值统计。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandStats {
    /// 下界（Hz）。
    pub lo_hz: f64,
    /// 上界（Hz）。
    pub hi_hz: f64,
    /// 最小幅度（dB）。
    pub min_db: f64,
    /// 最大幅度（dB）。
    pub max_db: f64,
    /// 参与统计的网格点数。
    pub points: usize,
}

impl BandStats {
    /// max−min（dB）。
    pub fn ripple_db(&self) -> f64 {
        self.max_db - self.min_db
    }
}

/// 群延迟统计（samples）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupDelayStats {
    /// 通带内最小值。
    pub min_samples: f64,
    /// 通带内最大值。
    pub max_samples: f64,
    /// 通带内均值。
    pub mean_samples: f64,
    /// 1 kHz 处 GD（规范 §2.2 group_delay.at_1khz_samples）。
    pub at_1khz_samples: f64,
    /// 是否满足常数 GD（判据 2）。
    pub class_constant: bool,
}

/// 过渡带判定结果。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransitionStats {
    /// LP：首次达到 −A_s 的频点；HP：首次离开 −A_s 的频点；None=搜索范围内未穿越。
    pub edge_hz: Option<f64>,
    /// 该 mode 规范上限（f_s）。
    pub limit_hz: f64,
    /// 过渡带搜索网格点数。
    pub points: usize,
}

/// 参考归一化结果。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReferenceStats {
    /// 参考频点（LP=DC，HP=通带几何中心）。
    pub hz: f64,
    /// manifest 声明增益（dB）。
    pub declared_gain_db: f64,
    /// 独立复算得到的 |H|（dB）。
    pub measured_gain_db: f64,
}

/// 逐条规范性判据结果（规范 §3.1 判据 1–7）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinearChecks {
    /// 有限且 max|h|>0。
    pub finite: bool,
    /// 判据 1：对称性。
    pub symmetry: bool,
    /// 判据 7：D=(L−1)/2、delay_ms 一致。
    pub delay: bool,
    /// 判据 2：通带常数 GD=(L−1)/2（±0.01 samples）。
    pub group_delay: bool,
    /// 判据 3：通带 max−min <= ripple_db+0.05。
    pub passband: bool,
    /// 判据 4：阻带 max <= −(A_s−0.5)。
    pub stopband: bool,
    /// 判据 5：LP 首次到 −A_s / HP 首次离开 −A_s 的频点 <= f_s。
    pub transition: bool,
    /// 判据 6：|H(reference)| 与声明增益差 <= 0.01 dB。
    pub reference: bool,
}

impl LinearChecks {
    /// 全部判据通过。
    pub fn passed(&self) -> bool {
        self.finite
            && self.symmetry
            && self.delay
            && self.group_delay
            && self.passband
            && self.stopband
            && self.transition
            && self.reference
    }
}

/// linear 独立响应验证报告（T55 报告器的输入；不含 WAV/manifest/Endpoint 等 T55 字段）。
#[derive(Clone, Debug, PartialEq)]
pub struct LinearVerification {
    /// 采样率。
    pub fs: f64,
    /// taps。
    pub taps: usize,
    /// D=(L−1)/2。
    pub delay_samples: f64,
    /// D/fs·1000。
    pub delay_ms: f64,
    /// 主网格点数（规范 §1.2 的 N）。
    pub grid_points: usize,
    /// max|h|。
    pub max_abs_h: f64,
    /// 对称性误差 max|h−s·flip(h)|（绝对值）。
    pub symmetry_max_abs_error: f64,
    /// 通带幅度统计。
    pub passband: BandStats,
    /// 阻带幅度统计。
    pub stopband: BandStats,
    /// 群延迟统计。
    pub group_delay: GroupDelayStats,
    /// 过渡带判定。
    pub transition: TransitionStats,
    /// 参考归一化。
    pub reference: ReferenceStats,
    /// H(0.5fs)（dB；LP 偶长对称期望 −240 dB 下限；HP 报告 H(π)）。
    pub nyquist_mag_db: f64,
    /// 逐条判据。
    pub checks: LinearChecks,
    /// 未通过判据的人类可读原因（空=全过）。
    pub failures: Vec<String>,
}

impl LinearVerification {
    /// 全部判据通过。
    pub fn passed(&self) -> bool {
        self.checks.passed()
    }
}

/// 单点测量（内部）。
#[derive(Clone, Copy, Debug)]
struct Sample {
    f_hz: f64,
    mag_db: f64,
    gd_samples: f64,
}

/// 独立响应验证（规范网格；正式报告口径）。等价 verify_linear_with + AnalysisOptions::normative。
pub fn verify_linear(spec: &LinearSpec, coeffs: &[f64]) -> Result<LinearVerification, LinearError> {
    verify_linear_with(spec, coeffs, &AnalysisOptions::normative(spec))
}

/// 独立响应验证（自定义网格/点数）。
pub fn verify_linear_with(
    spec: &LinearSpec,
    coeffs: &[f64],
    options: &AnalysisOptions,
) -> Result<LinearVerification, LinearError> {
    let v = validate(spec)?;
    if coeffs.len() != spec.taps {
        return Err(LinearError::Taps(format!(
            "系数长度 {} != 声明 taps {}",
            coeffs.len(),
            spec.taps
        )));
    }
    let mut max_abs_h = 0.0_f64;
    for &h in coeffs {
        if !h.is_finite() {
            return Err(LinearError::Numeric("系数含 NaN/Inf".to_string()));
        }
        max_abs_h = max_abs_h.max(h.abs());
    }
    if max_abs_h <= 0.0 {
        return Err(LinearError::Numeric(
            "max|h|=0（linear 家族不允许）".to_string(),
        ));
    }

    let fs = spec.fs;
    let l = spec.taps;
    let d = v.delay_samples;
    let report_hi = MEASUREMENT_HI_HZ.min(NYQUIST_GUARD_FRACTION * fs);
    let coverage_hi = match spec.mode {
        LinearMode::Lowpass => NYQUIST_GUARD_FRACTION * fs,
        LinearMode::Highpass => spec.passband_hz[1].max(report_hi),
    };
    if !(report_hi > MEASUREMENT_LO_HZ) {
        return Err(LinearError::Band(format!(
            "测量网格上界 {report_hi} Hz 必须 > 20 Hz（fs={fs}）"
        )));
    }

    let mag_n = options.mag_points.max(2);
    let mut samples: Vec<Sample> = Vec::with_capacity(mag_n + options.extension_points + 8);
    for f in log_grid(MEASUREMENT_LO_HZ, report_hi, mag_n) {
        samples.push(eval_sample(coeffs, fs, f));
    }
    if coverage_hi > report_hi * (1.0 + 1e-12) {
        let ext_n = options.extension_points.max(2);
        for (k, f) in log_grid(report_hi, coverage_hi, ext_n)
            .into_iter()
            .enumerate()
        {
            if k == 0 {
                continue; // 与主网格上界重复，跳过
            }
            samples.push(eval_sample(coeffs, fs, f));
        }
    }
    // 关键锚点（band 边界/1 kHz/参考/Nyquist），保证极值与参考不被网格漏掉。
    let mut anchor_hz: Vec<f64> = vec![
        spec.passband_hz[0],
        spec.passband_hz[1],
        spec.stopband_hz[0],
        spec.stopband_hz[1],
        1_000.0,
        0.5 * fs,
    ];
    if v.reference_hz > 0.0 {
        anchor_hz.push(v.reference_hz);
    }
    // 裁决 A：HP 阻带按 [0, f_s] 统计（含 DC 锚点）；LP 阻带仍为 [f_s, 0.499fs]。
    if spec.mode == LinearMode::Highpass {
        anchor_hz.push(0.0);
    }
    for f in anchor_hz {
        if f.is_finite() && f >= 0.0 && f <= 0.5 * fs {
            samples.push(eval_sample(coeffs, fs, f));
        }
    }

    let pass_lo = spec.passband_hz[0];
    let pass_hi = spec.passband_hz[1];
    let stop_lo = spec.stopband_hz[0];
    let stop_hi = spec.stopband_hz[1];
    let guard = NYQUIST_GUARD_FRACTION * fs;
    let eps_hz = 1e-9 * (fs + 1.0);

    let passband = stats_for(&samples, pass_lo, pass_hi);
    let stopband = match spec.mode {
        LinearMode::Lowpass => stats_for(&samples, stop_lo, guard),
        // 裁决 A：HP 阻带 [0, f_s]（含 DC；DC 由 h 求和精确给出）。
        LinearMode::Highpass => stats_for(&samples, 0.0, stop_hi),
    };

    // 群延迟：通带样本 |GD−D| 最大值（判据 2）。
    let mut gd_min = f64::INFINITY;
    let mut gd_max = f64::NEG_INFINITY;
    let mut gd_sum = 0.0_f64;
    let mut gd_count = 0_usize;
    let mut gd_err_max = 0.0_f64;
    for s in &samples {
        if s.f_hz >= pass_lo - eps_hz && s.f_hz <= pass_hi + eps_hz && s.gd_samples.is_finite() {
            gd_min = gd_min.min(s.gd_samples);
            gd_max = gd_max.max(s.gd_samples);
            gd_sum += s.gd_samples;
            gd_count += 1;
            gd_err_max = gd_err_max.max((s.gd_samples - d).abs());
        }
    }
    let (gd_min, gd_max, gd_mean) = if gd_count == 0 {
        (f64::NAN, f64::NAN, f64::NAN)
    } else {
        (gd_min, gd_max, gd_sum / gd_count as f64)
    };
    let gd_at_1k = group_delay_at(coeffs, fs, 1_000.0);
    let gd_ok =
        gd_count > 0 && gd_err_max.is_finite() && gd_err_max <= GROUP_DELAY_TOLERANCE_SAMPLES;

    // 过渡带：规范 §1.2 的线性网格（同 N 点，±0.2fs 夹到 [20 Hz, 0.5fs]）。
    let min_edge = v.passband_edge_hz.min(v.stopband_edge_hz);
    let max_edge = v.passband_edge_hz.max(v.stopband_edge_hz);
    let t_lo = (min_edge - 0.2 * fs).max(MEASUREMENT_LO_HZ);
    let t_hi = (max_edge + 0.2 * fs).min(0.5 * fs);
    let t_n = options.transition_points.max(2);
    let mut transition_edge = None;
    for f in linear_grid(t_lo, t_hi, t_n) {
        let db = magnitude_db_at(coeffs, fs, f);
        let crossed = match spec.mode {
            // LP：首次达到 −A_s；HP：首次离开 −A_s（进入通带）。
            LinearMode::Lowpass => db <= -spec.attenuation_db,
            LinearMode::Highpass => db > -spec.attenuation_db,
        };
        if crossed {
            transition_edge = Some(f);
            break;
        }
    }
    // 判据 5（§3.1）：LP 首次达到 −A_s 的频点必须 <= f_s（衰减在阻带边之前建立）；
    // HP 镜像口径：最后一次离开 −A_s 的频点必须 >= f_s（衰减保持到阻带边）。
    // 注：规范原文 HP 侧写作 "<= f_s"，与判据 4/通带定义自相矛盾（穿越必然发生在 (f_s,f_p) 内）；
    // 这里按 §3.5 LP 的镜像口径实现，并已在 evidence/T10 记录待 Lead 裁决（UNRULED），不静默放宽。
    // 裁决 A（Lead 2026-11-17）追加：HP 整段 [0,f_s] 阻带纹波峰值必须严格 < −A_s。
    let hp_stopband_below_att = stopband.points > 0
        && stopband.max_db.is_finite()
        && stopband.max_db < -spec.attenuation_db;
    let transition_ok = match spec.mode {
        LinearMode::Lowpass => transition_edge
            .map(|f| f <= v.stopband_edge_hz + eps_hz)
            .unwrap_or(false),
        LinearMode::Highpass => {
            transition_edge
                .map(|f| f >= v.stopband_edge_hz - eps_hz)
                .unwrap_or(false)
                && hp_stopband_below_att
        }
    };

    // 参考（判据 6）与 Nyquist。
    let ref_resp = response_at(coeffs, fs, v.reference_hz);
    let reference = ReferenceStats {
        hz: v.reference_hz,
        declared_gain_db: REFERENCE_GAIN_DB,
        measured_gain_db: ref_resp.magnitude_db(),
    };
    let reference_ok =
        (reference.measured_gain_db - REFERENCE_GAIN_DB).abs() <= REFERENCE_TOLERANCE_DB;
    let nyquist_mag_db = magnitude_db_at(coeffs, fs, 0.5 * fs);

    // 逐条判据。
    let symmetry_err = symmetry_error(coeffs, spec.symmetry);
    let finite = max_abs_h.is_finite() && max_abs_h > 0.0;
    let symmetry_ok = symmetry_err.is_finite() && symmetry_err <= SYMMETRY_TOLERANCE * max_abs_h;
    let delay_ok = coeffs.len() == l
        && (d - delay_samples(l)).abs() <= 0.0
        && (v.delay_ms - d / fs * 1000.0).abs() <= 1e-9;
    let passband_ok = passband.points > 0
        && passband.min_db.is_finite()
        && passband.max_db.is_finite()
        && passband.ripple_db() <= spec.ripple_db + PASSBAND_RIPPLE_MARGIN_DB;
    let stopband_ok = stopband.points > 0
        && stopband.max_db.is_finite()
        && stopband.max_db <= -(spec.attenuation_db - STOPBAND_MARGIN_DB);

    let mut failures = Vec::new();
    if !finite {
        failures.push("finite: max|h| 非有限或为 0".to_string());
    }
    if !symmetry_ok {
        failures.push(format!(
            "对称性: max|h−s·flip(h)|={symmetry_err:.6e} > {SYMMETRY_TOLERANCE}·max|h|"
        ));
    }
    if !delay_ok {
        failures.push("延迟: D=(L−1)/2/delay_ms 不一致".to_string());
    }
    if !gd_ok {
        failures.push(format!(
            "群延迟: 通带 max|GD−D|={gd_err_max:.6} samples > {GROUP_DELAY_TOLERANCE_SAMPLES}"
        ));
    }
    if !passband_ok {
        failures.push(format!(
            "通带: ripple={:.6} dB > declared {:.6}+{PASSBAND_RIPPLE_MARGIN_DB}",
            passband.ripple_db(),
            spec.ripple_db
        ));
    }
    if !stopband_ok {
        failures.push(format!(
            "阻带: max={:.6} dB > −(A_s−{STOPBAND_MARGIN_DB})={:.6}",
            stopband.max_db,
            -(spec.attenuation_db - STOPBAND_MARGIN_DB)
        ));
    }
    if !transition_ok {
        failures.push(format!(
            "过渡带: {:?} 穿越点 {:?} 未满足 {} 侧要求（f_s={}, HP 另需阻带峰值<−A_s: max={:.3} dB）",
            spec.mode,
            transition_edge,
            match spec.mode {
                LinearMode::Lowpass => "<=f_s",
                LinearMode::Highpass => ">=f_s",
            },
            v.stopband_edge_hz,
            stopband.max_db
        ));
    }
    if !reference_ok {
        failures.push(format!(
            "参考: |H({} Hz)|={:.6} dB 与声明 {REFERENCE_GAIN_DB} dB 差 > {REFERENCE_TOLERANCE_DB}",
            v.reference_hz, reference.measured_gain_db
        ));
    }

    let checks = LinearChecks {
        finite,
        symmetry: symmetry_ok,
        delay: delay_ok,
        group_delay: gd_ok,
        passband: passband_ok,
        stopband: stopband_ok,
        transition: transition_ok,
        reference: reference_ok,
    };
    Ok(LinearVerification {
        fs,
        taps: l,
        delay_samples: d,
        delay_ms: v.delay_ms,
        grid_points: mag_n,
        max_abs_h,
        symmetry_max_abs_error: symmetry_err,
        passband,
        stopband,
        group_delay: GroupDelayStats {
            min_samples: gd_min,
            max_samples: gd_max,
            mean_samples: gd_mean,
            at_1khz_samples: gd_at_1k,
            class_constant: gd_ok,
        },
        transition: TransitionStats {
            edge_hz: transition_edge,
            limit_hz: v.stopband_edge_hz,
            points: t_n,
        },
        reference,
        nyquist_mag_db,
        checks,
        failures,
    })
}

/// 对已构造设计做独立验证（等价 verify_linear(design.spec, design.coeffs)）。
pub fn verify_linear_design(design: &LinearDesign) -> Result<LinearVerification, LinearError> {
    verify_linear(&design.spec, &design.coeffs)
}

/// 构造 + 独立验证；任一判据失败返回 E_FIR_*（不返回“看起来成功”的坏设计）。
pub fn design_checked(spec: &LinearSpec) -> Result<LinearDesign, LinearError> {
    design_checked_with(spec, &AnalysisOptions::normative(spec))
}

/// 构造 + 独立验证（自定义网格/点数）。
pub fn design_checked_with(
    spec: &LinearSpec,
    options: &AnalysisOptions,
) -> Result<LinearDesign, LinearError> {
    let design = design(spec)?;
    let verification = verify_linear_with(spec, &design.coeffs, options)?;
    if verification.passed() {
        Ok(design)
    } else {
        Err(criteria_to_error(&verification))
    }
}

/// 把未通过判据分类到 E_FIR_* 码。
fn criteria_to_error(v: &LinearVerification) -> LinearError {
    let detail = v.failures.join("；");
    if !v.checks.symmetry || !v.checks.group_delay {
        LinearError::PhaseClass(detail)
    } else if !v.checks.reference {
        LinearError::Reference(detail)
    } else {
        LinearError::Budget(format!("设计在声明参数下不可行：{detail}"))
    }
}

/// 表格统计（按 [lo, hi] 过滤样本；含锚点）。
fn stats_for(samples: &[Sample], lo: f64, hi: f64) -> BandStats {
    let eps_hz = 1e-9 * (hi.abs() + lo.abs() + 1.0);
    let mut min_db = f64::INFINITY;
    let mut max_db = f64::NEG_INFINITY;
    let mut points = 0_usize;
    for s in samples {
        if s.f_hz >= lo - eps_hz && s.f_hz <= hi + eps_hz && s.mag_db.is_finite() {
            min_db = min_db.min(s.mag_db);
            max_db = max_db.max(s.mag_db);
            points += 1;
        }
    }
    if points == 0 {
        BandStats {
            lo_hz: lo,
            hi_hz: hi,
            min_db: f64::NAN,
            max_db: f64::NAN,
            points: 0,
        }
    } else {
        BandStats {
            lo_hz: lo,
            hi_hz: hi,
            min_db,
            max_db,
            points,
        }
    }
}

/// 单频点测量（内部）。
fn eval_sample(coeffs: &[f64], fs: f64, f_hz: f64) -> Sample {
    let (re, im, gd) = eval_h_and_gd(coeffs, fs, f_hz);
    let mag = re.hypot(im);
    Sample {
        f_hz,
        mag_db: 20.0 * mag.max(MAGNITUDE_FLOOR).log10(),
        gd_samples: gd,
    }
}

/// 对数频率网格（端点精确）。
fn log_grid(lo: f64, hi: f64, n: usize) -> Vec<f64> {
    if !(lo > 0.0 && hi > lo && n >= 2) {
        return vec![lo];
    }
    let ratio = hi / lo;
    let mut grid = Vec::with_capacity(n);
    for k in 0..n {
        let t = k as f64 / (n - 1) as f64;
        grid.push(lo * ratio.powf(t));
    }
    grid[0] = lo;
    grid[n - 1] = hi;
    grid
}

/// 线性频率网格（端点精确）。
fn linear_grid(lo: f64, hi: f64, n: usize) -> Vec<f64> {
    if !(hi > lo && n >= 2) {
        return vec![lo];
    }
    let mut grid = Vec::with_capacity(n);
    for k in 0..n {
        let t = k as f64 / (n - 1) as f64;
        grid.push(lo + (hi - lo) * t);
    }
    grid[0] = lo;
    grid[n - 1] = hi;
    grid
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lp_511() -> LinearSpec {
        LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0)
    }

    // HP 测试用 A_s=60（设计余量使整段阻带 < −A_s，满足判据 5 的阈值口径）。
    fn hp_513() -> LinearSpec {
        LinearSpec::highpass(48_000.0, 513, 5_000.0, 5_500.0).with_attenuation(60.0)
    }

    fn hp_512_antisymmetric() -> LinearSpec {
        LinearSpec::highpass(48_000.0, 512, 5_000.0, 5_500.0)
            .with_attenuation(60.0)
            .with_symmetry(Symmetry::Antisymmetric)
    }

    #[test]
    fn linear_hand_vector_v2_delay_table() {
        assert_eq!(delay_samples(1024), 511.5);
        assert_eq!(delay_samples(4096), 2047.5);
        assert!((delay_ms(1024, 48_000.0) - 10.656_25).abs() < 1e-12);
        assert!((delay_ms(2048, 48_000.0) - 21.322_916_666_666_66).abs() < 1e-9);
        assert!((delay_ms(2048, 96_000.0) - 10.661_458_333_333_33).abs() < 1e-9);
        assert!((delay_ms(4096, 48_000.0) - 42.656_25).abs() < 1e-9);
        assert!((delay_ms(8192, 48_000.0) - 85.322_916_666_666_66).abs() < 1e-9);
        assert!((delay_ms(16_384, 48_000.0) - 170.656_25).abs() < 1e-9);
    }

    #[test]
    fn linear_hand_vector_v3_three_tap() {
        let h = [0.25, 0.5, 0.25];
        let fs = 48_000.0;
        let dc = response_at(&h, fs, 0.0);
        assert!((dc.magnitude() - 1.0).abs() < 1e-15);
        let quarter = response_at(&h, fs, 0.25 * fs);
        assert!((quarter.magnitude_db() - (-6.020_599_913_279_624)).abs() < 1e-9);
        assert!((quarter.phase_deg() - (-90.0)).abs() < 1e-9);
        assert!(response_at(&h, fs, 0.5 * fs).magnitude_db() <= -240.0);
        assert!((group_delay_at(&h, fs, 12_000.0) - 1.0).abs() < 1e-9);
        assert!((group_delay_at(&h, fs, 1_000.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn linear_hand_vector_v4_five_tap() {
        let h = [1.0 / 9.0, 2.0 / 9.0, 3.0 / 9.0, 2.0 / 9.0, 1.0 / 9.0];
        let fs = 48_000.0;
        assert!((response_at(&h, fs, 0.0).magnitude() - 1.0).abs() < 1e-15);
        let nyq = response_at(&h, fs, 0.5 * fs);
        assert!((nyq.magnitude() - 1.0 / 9.0).abs() < 1e-15);
        assert!((nyq.magnitude_db() - 20.0 * (1.0_f64 / 9.0).log10()).abs() < 1e-9);
        assert!((group_delay_at(&h, fs, 437.0) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn linear_hand_vector_v1_even_symmetric_has_zero_at_nyquist() {
        let spec = LinearSpec::lowpass(48_000.0, 512, 18_000.0, 20_000.0);
        let design = design(&spec).expect("design ok");
        assert!(response_at(&design.coeffs, spec.fs, 0.5 * spec.fs).magnitude() <= 1e-12);
    }

    #[test]
    fn linear_hand_vector_v13_kaiser_taps_table() {
        let t = |a: f64, df: f64, fs: f64| kaiser_required_taps(a, fs, df).unwrap();
        assert_eq!(t(60.0, 1_000.0, 48_000.0), 176);
        assert_eq!(t(80.0, 1_000.0, 48_000.0), 242);
        assert_eq!(t(100.0, 1_000.0, 48_000.0), 309);
        assert_eq!(t(120.0, 1_000.0, 48_000.0), 376);
        assert_eq!(t(80.0, 500.0, 48_000.0), 483);
        assert_eq!(t(80.0, 1_000.0, 96_000.0), 483);
        assert_eq!(t(60.0, 2_000.0, 96_000.0), 176);
        assert!(kaiser_required_taps(80.0, 48_000.0, 0.0).is_none());
    }

    #[test]
    fn linear_kaiser_beta_and_window_sanity() {
        assert!((kaiser_beta_for_attenuation(80.0) - 0.1102 * 71.3).abs() < 1e-12);
        assert_eq!(kaiser_beta_for_attenuation(20.0), 0.0);
        let w = kaiser_window(511, 7.857_26);
        assert_eq!(w[0], w[510]);
        assert!((w[255] - 1.0).abs() < 1e-12);
        let bh = blackman_harris_window(512);
        assert_eq!(bh[0], bh[511]);
        assert!(bh.iter().all(|v| v.is_finite()));
        assert_eq!(symmetry_error(&w, Symmetry::Symmetric), 0.0);
    }

    #[test]
    fn linear_validate_rejects_bad_bands_and_flags() {
        let mut s = lp_511();
        s.passband_hz[1] = 20_100.0;
        assert_eq!(validate(&s).unwrap_err().code(), "E_FIR_BAND");
        let mut s = lp_511();
        s.stopband_hz[0] = 18_010.0;
        assert_eq!(validate(&s).unwrap_err().code(), "E_FIR_BAND");
        let mut s = lp_511();
        s.stopband_hz[1] = 23_000.0;
        assert_eq!(validate(&s).unwrap_err().code(), "E_FIR_BAND");
        let s = LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0)
            .with_symmetry(Symmetry::Antisymmetric);
        assert_eq!(validate(&s).unwrap_err().code(), "E_FIR_BAND");
        let s = LinearSpec::lowpass(48_000.0, 7, 18_000.0, 20_000.0);
        assert_eq!(validate(&s).unwrap_err().code(), "E_FIR_BAND");
        let s = LinearSpec::lowpass(48_000.0, 16_385, 18_000.0, 20_000.0);
        assert_eq!(validate(&s).unwrap_err().code(), "E_FIR_BAND");
        let s = LinearSpec::highpass(48_000.0, 512, 5_000.0, 5_500.0)
            .with_window(WindowKind::BlackmanHarris)
            .with_kaiser_beta(6.0);
        assert_eq!(validate(&s).unwrap_err().code(), "E_FIR_BAND");
    }

    #[test]
    fn linear_validate_hp_nyquist_parity() {
        let s = LinearSpec::highpass_to_nyquist(48_000.0, 512, 5_000.0, 5_500.0);
        assert_eq!(validate(&s).unwrap_err().code(), "E_FIR_HP_NYQ");
        let ok = LinearSpec::highpass_to_nyquist(48_000.0, 513, 5_000.0, 5_500.0);
        assert!(validate(&ok).is_ok());
        let ok4 = LinearSpec::highpass_to_nyquist(48_000.0, 512, 5_000.0, 5_500.0)
            .with_symmetry(Symmetry::Antisymmetric);
        assert!(validate(&ok4).is_ok());
        let bad3 = LinearSpec::highpass_to_nyquist(48_000.0, 513, 5_000.0, 5_500.0)
            .with_symmetry(Symmetry::Antisymmetric);
        assert_eq!(validate(&bad3).unwrap_err().code(), "E_FIR_HP_NYQ");
    }

    #[test]
    fn linear_validate_delay_and_budget() {
        let s = lp_511().with_declared_delay(256.0);
        assert_eq!(validate(&s).unwrap_err().code(), "E_FIR_DELAY");
        let ok = lp_511().with_declared_delay(255.0);
        assert!(validate(&ok).is_ok());
        let s = LinearSpec::lowpass(48_000.0, 120, 18_000.0, 20_000.0);
        assert_eq!(design(&s).unwrap_err().code(), "E_FIR_BUDGET");
        assert_eq!(
            kaiser_required_taps(80.0, 48_000.0, 2_000.0),
            Some(122),
            "A_s=80/Δf=2000 的 V13 L_req"
        );
        let s = LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0)
            .with_window(WindowKind::BlackmanHarris)
            .with_attenuation(100.0);
        assert_eq!(design(&s).unwrap_err().code(), "E_FIR_BUDGET");
    }

    #[test]
    fn linear_design_lowpass_compact_verify() {
        let spec = lp_511();
        let design = design_checked_with(&spec, &AnalysisOptions::compact(spec.taps))
            .expect("LP compact verify must pass");
        assert_eq!(design.taps(), 511);
        assert!((design.delay_samples - 255.0).abs() < 1e-12);
        assert!((design.delay_ms - 5.3125).abs() < 1e-12);
        assert!((design.sum_coeffs - 1.0).abs() < 1e-12);
        assert_eq!(design.spec, spec);
    }

    #[test]
    fn linear_design_highpass_symmetric_compact_verify() {
        let spec = hp_513();
        let design = design_checked_with(&spec, &AnalysisOptions::compact(spec.taps))
            .expect("HP symmetric compact verify must pass");
        assert!((design.delay_samples - 256.0).abs() < 1e-12);
        let ver = verify_linear_with(&spec, &design.coeffs, &AnalysisOptions::compact(spec.taps))
            .expect("verify");
        assert!(ver.passed(), "failures: {:?}", ver.failures);
        assert!(ver.passband.ripple_db() <= spec.ripple_db + PASSBAND_RIPPLE_MARGIN_DB);
        assert!(ver.stopband.max_db <= -(spec.attenuation_db - STOPBAND_MARGIN_DB));
    }

    #[test]
    fn linear_design_highpass_antisymmetric_even() {
        let spec = hp_512_antisymmetric();
        let design = design_checked_with(&spec, &AnalysisOptions::compact(spec.taps))
            .expect("antisymmetric HP compact verify must pass");
        assert_eq!(design.symmetry, Symmetry::Antisymmetric);
        assert!(design.sum_coeffs.abs() < 1e-12);
        assert_eq!(symmetry_error(&design.coeffs, Symmetry::Antisymmetric), 0.0);
    }

    #[test]
    fn linear_design_blackman_harris() {
        let spec = lp_511().with_window(WindowKind::BlackmanHarris);
        let d = design(&spec).expect("BH design");
        let ver = verify_linear_with(&spec, &d.coeffs, &AnalysisOptions::compact(spec.taps))
            .expect("verify");
        assert!(ver.passed(), "failures: {:?}", ver.failures);
        assert!(ver.stopband.max_db <= -(spec.attenuation_db - STOPBAND_MARGIN_DB));
    }

    #[test]
    fn linear_design_is_deterministic() {
        let a = design(&lp_511()).expect("a");
        let b = design(&lp_511()).expect("b");
        assert_eq!(a.coeffs, b.coeffs);
        assert_eq!(a.peak_abs, b.peak_abs);
    }

    #[test]
    fn linear_design_checked_rejects_infeasible() {
        let spec = LinearSpec::lowpass(48_000.0, 32, 18_000.0, 20_000.0).with_attenuation(30.0);
        let err = design_checked_with(&spec, &AnalysisOptions::compact(spec.taps)).unwrap_err();
        assert!(
            err.code() == "E_FIR_BUDGET" || err.code() == "E_FIR_PHASE_CLASS",
            "got {}",
            err.code()
        );
    }

    #[test]
    fn linear_f32_export_quantization() {
        let design = design(&lp_511()).expect("design");
        let f32s = design.coeffs_f32();
        assert_eq!(f32s.len(), design.coeffs.len());
        let mut max_err = 0.0_f64;
        for (a, b) in design.coeffs.iter().zip(f32s.iter()) {
            assert!(b.is_finite());
            max_err = max_err.max((f64::from(*b) - a).abs());
        }
        assert!(max_err <= 1e-7 * design.peak_abs);
    }

    #[test]
    fn linear_normative_grid_point_count() {
        let spec = lp_511();
        let opt = AnalysisOptions::normative(&spec);
        assert_eq!(opt.mag_points, 8192);
        assert_eq!(opt.transition_points, 8192);
        let big = LinearSpec::lowpass(48_000.0, 2048, 18_000.0, 20_000.0);
        assert_eq!(AnalysisOptions::normative(&big).mag_points, 16_384);
    }
}
