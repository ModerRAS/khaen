//! [NONRT] 有限 brickwall 近似族 FIR 设计（WBS T36）—— T36 写域。
//!
//! 本模块只实现 brickwall 家族（Kaiser 窗 sinc 有限近似 + 独立响应验证 + 预算拒绝），
//! 不把七家族藏进一个包：linear/min_fast/min_slow/apodizing/corrected/nos 各有独立 leaf。
//!
//! 权威契约：rt-stack/spec/fir-toolbox.md v1.2-draft §3.5（brickwall 参数/参考构造/规范性判据 1–8）、
//! §1.2（响应测量程序）、§1.4（振铃口径）、§2.5（E_FIR_* 失败码）、§3.8（家族自洽表）、
//! §7.2（运行准入边界：4096 / 8192 / 16384 / >16384 offline）、§9 V13（Kaiser L_req 表）、
//! evidence/T04/recompute-vectors.mjs V16（brickwall 振铃参考构造）。
//!
//! 能力：
//! - 参数：fs、taps、passband_hz=[20,f_p]、stopband_hz=[f_s,0.499fs]、ripple_db（默认 0.2）、
//!   attenuation_db（60..120）、method=kaiser_windowed_sinc（本 leaf 只交付该有限近似）、
//!   可选 kaiser_beta（显式复现参考构造；缺省按 §3.1 标准公式由 A_s 选取）、可选声明 D。
//! - 构造：fc=(f_p+f_s)/2、D=(L−1)/2、h[n]=(2fc/fs)·sinc(2fc/fs·(n−D))·w_kaiser[n]，DC 归一（reference 0 dB）。
//!   过渡带必须 > 0 且 >= 20 Hz；taps < L_req（Kaiser 可行性式）→ E_FIR_BUDGET 拒绝，不静默截短。
//! - 验证：独立直接 DTFT（复指数递推 + 每 64 点重锚，不复用构造公式）复算幅度/群延迟，逐条判
//!   §3.5 判据 1–4/6 与通用判据（finite/支撑/参考/端点），并给出预算分类与振铃信息。
//! - 预算拒绝（DoD）：① f_s−f_p <= 0 或 < 20 Hz → E_FIR_BAND；② A_s ∉ [60,120] → E_FIR_BAND；
//!   ③ taps < L_req（kaiser_windowed_sinc）→ E_FIR_BUDGET；④ taps > 16384 → E_FIR_BUDGET（§7.2
//!   >16384 仅 offline、runtime 不放行）；⑤ taps > 4096 标 extended（regular 不放行），仍可设计。
//!
//! 明确非目标（不得由本模块冒充）：
//! - 不实现 least_squares/remez 方法：method 字段保留全枚举，但非 kaiser_windowed_sinc 一律显式
//!   E_FIR_FAMILY 拒绝（不冒充等波纹），理由同 T10 对 linear 的 least_squares 处理：DoD 只要求
//!   有限近似，伪造未实现方法的系数等于静默降级。
//! - 不生成 WAV/manifest（tspi-fir/v1 产物与 T55 报告字段归 T55），不做 target 导入（T11）、
//!   P/G 融合（T12）与 RT 运行（本文件零 RT 路径、零板端动作）。
//! - 15/25/40 ms 整链延迟预算与 profile 准入归 T55/T56：本模块只报 delay_ms 事实与 taps profile 分类，
//!   不宣称“满足整链预算”。
//! - 振铃只报告（§3.5 判据 6 无上限）；brickwall 与 apodizing 的互斥判定（§3.8）由 T55 汇总，
//!   本模块给出 0.45fs 阻带值供其引用。
//!
//! 精度/确定性：设计 f64；同一输入同一版本两次 design 系数逐位一致（无时间戳/随机数）。
//!
//! 私有测试：tests/T36/brickwall_cases.rs 由本文件末尾 #[cfg(test)] mod brickwall_tests
//! 通过 include! 编译执行（cargo 不自动发现 tests/<Txx>/ 子目录；公共注册属 integrator）。

use std::f64::consts::PI;
use std::fmt;

// ---------------------------------------------------------------------------
// 常量（契约来源见各行注释）
// ---------------------------------------------------------------------------

/// taps 下界（§3.1 判据 8：L ∈ [8,16384]）。
pub const MIN_TAPS: usize = 8;

/// taps 上界（§2.1/§3.1：设计 16384 taps；>16384 仅 offline，见 §7.2）。
pub const MAX_TAPS: usize = 16_384;

/// 常规 profile 每路由有效支撑（§7.2）。
pub const REGULAR_PROFILE_TAPS_CAP: usize = 4_096;

/// extended profile 每路由有效支撑（§7.2）。
pub const EXTENDED_PROFILE_TAPS_CAP: usize = 16_384;

/// 最小过渡带（§3.5 判据 7：Δf=f_s−f_p >= 20 Hz；本模块另要求 >0）。
pub const MIN_TRANSITION_HZ: f64 = 20.0;

/// A_s 下界（§3.5 判据 7：A_s ∈ [60,120]）。
pub const MIN_ATTENUATION_DB: f64 = 60.0;

/// A_s 上界（§3.5 判据 7）。
pub const MAX_ATTENUATION_DB: f64 = 120.0;

/// 通带纹波默认（§3.5 参数表：ripple_db 默认 0.2）。
pub const DEFAULT_RIPPLE_DB: f64 = 0.2;

/// 衰减默认（§3.5 参考构造示例按 80 dB）。
pub const DEFAULT_ATTENUATION_DB: f64 = 80.0;

/// 参考构造的 Kaiser beta 参考值（§3.5：默认 8.96 ≈ 80 dB；T04 V16 用该值复算）。
pub const DEFAULT_KAISER_BETA: f64 = 8.96;

/// Kaiser 可行性式常数（§3.5 判据 5 / §9 V13：14.357078 = 2.285·2π，评审 I11 修正）。
pub const KAISER_TAPS_NUMERATOR: f64 = 14.357_078;

/// 阻带上界保护比例（§3.5 判据 3：阻带测到 0.499fs）。
pub const NYQUIST_GUARD_FRACTION: f64 = 0.499;

/// 测量网格下界（§1.2：20 Hz）。
pub const MEASUREMENT_LO_HZ: f64 = 20.0;

/// 测量网格上界（§1.2：min(20000, 0.499fs)）。
pub const MEASUREMENT_HI_HZ: f64 = 20_000.0;

/// 振铃排除窗比例（§1.4：W=ceil(0.000333·fs)，48k=16、96k=32）。
pub const RINGING_EXCLUSION_SECONDS: f64 = 0.000_333;

/// 振铃 duration 的衰减门（§3.5 判据 6：从主峰衰减到 −80 dB 的样本数）。
pub const RINGING_DECAY_DB: f64 = -80.0;

/// 线性相位对称性相对容差（§3.1 判据 1：1e-7·max|h|）。
pub const SYMMETRY_TOLERANCE_RELATIVE: f64 = 1e-7;

/// 常数群延迟容差（§3.1 判据 2：±0.01 samples）。
pub const GROUP_DELAY_TOLERANCE_SAMPLES: f64 = 0.01;

/// 通带纹波判定余量（§3.5 判据 2：ripple <= 声明 + 0.05 dB）。
pub const PASSBAND_RIPPLE_MARGIN_DB: f64 = 0.05;

/// 阻带判定余量（§3.5 判据 3：max <= −(A_s − 1)）。
pub const STOPBAND_MARGIN_DB: f64 = 1.0;

/// 参考归一化容差（§1.5/§3.0：±0.01 dB）。
pub const REFERENCE_TOLERANCE_DB: f64 = 0.01;

/// 过渡带穿越点容差比例（§3.5 判据 4：max(0.05·Δf, 20 Hz)）。
pub const TRANSITION_TOLERANCE_FRACTION: f64 = 0.05;

/// 过渡带穿越点容差下限（§3.5 判据 4：20 Hz）。
pub const TRANSITION_TOLERANCE_MIN_HZ: f64 = 20.0;

/// 幅度下限（§1.2：|H| 截到 1e-12 = −240 dB 避免 log0）。
pub const MAGNITUDE_FLOOR: f64 = 1e-12;

/// 参考增益（本家族 DC 归一后声明 0 dB）。
pub const REFERENCE_GAIN_DB: f64 = 0.0;

// ---------------------------------------------------------------------------
// 枚举
// ---------------------------------------------------------------------------

/// brickwall 构造方法（§3.5 参数 method）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrickwallMethod {
    /// Kaiser 窗 sinc（本 leaf 唯一实现；T04 V16 参考构造同法）。
    KaiserWindowedSinc,
    /// 最小二乘（§3.5 允许声明，但本 leaf 未实现；显式拒绝，不冒充）。
    LeastSquares,
    /// 等波纹 Remez（§3.5 允许声明，但本 leaf 未实现；显式拒绝，不冒充）。
    Remez,
}

impl BrickwallMethod {
    /// 规范字符串（manifest design.method 用）。
    pub fn as_str(&self) -> &'static str {
        match self {
            BrickwallMethod::KaiserWindowedSinc => "kaiser_windowed_sinc",
            BrickwallMethod::LeastSquares => "least_squares",
            BrickwallMethod::Remez => "remez",
        }
    }

    /// 本 leaf 是否已实现该 method。
    pub fn implemented(&self) -> bool {
        matches!(self, BrickwallMethod::KaiserWindowedSinc)
    }
}

impl fmt::Display for BrickwallMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// taps 运行准入分类（§7.2：常规 <=4096 / extended 8192 或 16384 / >16384 offline）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrickwallProfile {
    /// 常规 profile：<=4096 taps。
    Regular,
    /// extended profile：4097..=16384 taps，需 RAM/最坏 deadline/延迟/热切峰值准入（T56）。
    Extended,
    /// >16384：仅设计工具 offline 输出，runtime 不放行（本模块 design 直接 E_FIR_BUDGET）。
    OfflineOnly,
}

impl BrickwallProfile {
    /// 规范字符串（manifest limits.profile 用）。
    pub fn as_str(&self) -> &'static str {
        match self {
            BrickwallProfile::Regular => "regular",
            BrickwallProfile::Extended => "extended",
            BrickwallProfile::OfflineOnly => "offline_only",
        }
    }
}

impl fmt::Display for BrickwallProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 预算分类（§7.2；T55 manifest limits 的字段来源）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrickwallBudget {
    /// taps 所属 profile。
    pub profile: BrickwallProfile,
    /// 该 profile 的每路由 taps 上限（offline_only 记 0：无运行准入）。
    pub route_taps_cap: usize,
    /// 每路由 f32 系数内存 4·taps bytes（§7.2）。
    pub ram_bytes_f32: usize,
    /// 是否可进常规 profile（<=4096）。
    pub fits_regular_profile: bool,
    /// 是否可进 extended profile（<=16384）。
    pub fits_extended_profile: bool,
}

impl BrickwallBudget {
    /// 按 taps 分类（不 panic；越界如实标 OfflineOnly 且 fits_* 全 false）。
    pub fn for_taps(taps: usize) -> Self {
        if taps <= REGULAR_PROFILE_TAPS_CAP {
            BrickwallBudget {
                profile: BrickwallProfile::Regular,
                route_taps_cap: REGULAR_PROFILE_TAPS_CAP,
                ram_bytes_f32: 4usize.saturating_mul(taps),
                fits_regular_profile: true,
                fits_extended_profile: true,
            }
        } else if taps <= EXTENDED_PROFILE_TAPS_CAP {
            BrickwallBudget {
                profile: BrickwallProfile::Extended,
                route_taps_cap: EXTENDED_PROFILE_TAPS_CAP,
                ram_bytes_f32: 4usize.saturating_mul(taps),
                fits_regular_profile: false,
                fits_extended_profile: true,
            }
        } else {
            BrickwallBudget {
                profile: BrickwallProfile::OfflineOnly,
                route_taps_cap: 0,
                ram_bytes_f32: 4usize.saturating_mul(taps),
                fits_regular_profile: false,
                fits_extended_profile: false,
            }
        }
    }
}

/// 端点报告联合类型（§2.4；禁止裸字符串/省略）。
#[derive(Clone, Debug, PartialEq)]
pub enum Endpoint {
    /// 可测端点。
    Measured {
        /// 频点（Hz）。
        freq_hz: f64,
        /// |H|（dB）。
        mag_db: f64,
        /// 相位（度，wrap 到 (−180,180]）。
        phase_deg: f64,
    },
    /// 在运行 fs 下不可测（如 20 kHz > 0.499fs）。
    NotApplicableAtFs {
        /// 原因。
        reason: String,
    },
}

impl Endpoint {
    /// status 字段字符串（measured / not_applicable_at_fs）。
    pub fn status(&self) -> &'static str {
        match self {
            Endpoint::Measured { .. } => "measured",
            Endpoint::NotApplicableAtFs { .. } => "not_applicable_at_fs",
        }
    }

    /// 可测时返回 (freq_hz, mag_db, phase_deg)。
    pub fn measured(&self) -> Option<(f64, f64, f64)> {
        match self {
            Endpoint::Measured {
                freq_hz,
                mag_db,
                phase_deg,
            } => Some((*freq_hz, *mag_db, *phase_deg)),
            Endpoint::NotApplicableAtFs { .. } => None,
        }
    }
}

/// 20 Hz / 20 kHz 端点（§2.4；T55 response.endpoints 的字段来源）。
#[derive(Clone, Debug, PartialEq)]
pub struct BrickwallEndpoints {
    /// 20 Hz 端点。
    pub f20: Endpoint,
    /// 20 kHz 端点（fs 下不可测时为 not_applicable_at_fs）。
    pub f20k: Endpoint,
}

// ---------------------------------------------------------------------------
// 参数 / 错误 / 验证后参数
// ---------------------------------------------------------------------------

/// brickwall 设计参数（§3.5 参数表；字段可直改，构造器给默认值）。
#[derive(Clone, Debug, PartialEq)]
pub struct BrickwallSpec {
    /// 运行采样率（Hz）；FIR fs 必须等于运行 fs（§7.2，跨 fs 重设计）。
    pub fs: f64,
    /// 支撑长度 L（taps），[MIN_TAPS, MAX_TAPS]。
    pub taps: usize,
    /// 通带 [20, f_p]，f_p < f_s。
    pub passband_hz: [f64; 2],
    /// 阻带 [f_s, 0.499fs]，f_s−f_p >= 20 Hz。
    pub stopband_hz: [f64; 2],
    /// 通带允许纹波（max−min，dB），判据 <= ripple_db + 0.05。
    pub ripple_db: f64,
    /// 阻带要求衰减（dB），[60,120]；判据 stopband max <= −(A_s − 1)。
    pub attenuation_db: f64,
    /// 构造方法（本 leaf 仅 kaiser_windowed_sinc）。
    pub method: BrickwallMethod,
    /// Kaiser beta；None 时由 attenuation_db 标准公式推导（§3.5 参考构造默认 8.96 可显式给出）。
    pub kaiser_beta: Option<f64>,
    /// 可选声明体延迟 D（samples）；brickwall 必须 = (L−1)/2，不一致即 E_FIR_DELAY。
    pub declared_delay_samples: Option<f64>,
}

impl BrickwallSpec {
    /// 低通参数构造器：通带 [20, passband_hi_hz]，阻带 [stopband_lo_hz, 0.499·fs]。
    ///
    /// 默认：kaiser_windowed_sinc、beta 按 A_s=80 dB 标准公式推导、ripple 0.2 dB、无声明 D。
    pub fn lowpass(fs: f64, taps: usize, passband_hi_hz: f64, stopband_lo_hz: f64) -> Self {
        Self {
            fs,
            taps,
            passband_hz: [MEASUREMENT_LO_HZ, passband_hi_hz],
            stopband_hz: [stopband_lo_hz, NYQUIST_GUARD_FRACTION * fs],
            ripple_db: DEFAULT_RIPPLE_DB,
            attenuation_db: DEFAULT_ATTENUATION_DB,
            method: BrickwallMethod::KaiserWindowedSinc,
            kaiser_beta: None,
            declared_delay_samples: None,
        }
    }

    /// 设置通带 ripple_db。
    pub fn with_ripple(mut self, ripple_db: f64) -> Self {
        self.ripple_db = ripple_db;
        self
    }

    /// 设置 attenuation_db。
    pub fn with_attenuation(mut self, attenuation_db: f64) -> Self {
        self.attenuation_db = attenuation_db;
        self
    }

    /// 设置构造方法（非 kaiser_windowed_sinc 会在 validate 显式拒绝）。
    pub fn with_method(mut self, method: BrickwallMethod) -> Self {
        self.method = method;
        self
    }

    /// 显式设置 Kaiser beta（用于复现参考构造，如 T04 V16 的 8.96）。
    pub fn with_kaiser_beta(mut self, beta: f64) -> Self {
        self.kaiser_beta = Some(beta);
        self
    }

    /// 声明体延迟 D（samples）；brickwall 必须 = (L−1)/2。
    pub fn with_declared_delay(mut self, delay_samples: f64) -> Self {
        self.declared_delay_samples = Some(delay_samples);
        self
    }

    /// 通带上界 f_p（Hz）。
    pub fn passband_edge_hz(&self) -> f64 {
        self.passband_hz[1]
    }

    /// 阻带下界 f_s（Hz）。
    pub fn stopband_edge_hz(&self) -> f64 {
        self.stopband_hz[0]
    }

    /// 过渡带宽度 Δf=f_s−f_p（Hz）。
    pub fn transition_hz(&self) -> f64 {
        self.stopband_hz[0] - self.passband_hz[1]
    }
}

/// E_FIR_* 失败码（§2.5）承载的错误；不发明规范外成功路径。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrickwallError {
    /// E_FIR_BAND：频带/参数非法（含过渡带 <=0/<20 Hz、A_s 越界、taps < 下界、f_s > 0.499fs）。
    Band(String),
    /// E_FIR_DELAY：声明 D 与 (L−1)/2 不一致。
    Delay(String),
    /// E_FIR_TAPS：系数长度与声明 taps 不一致。
    Taps(String),
    /// E_FIR_BUDGET：可行性式不足/超出运行准入（L < L_req、taps > 16384），不静默截短/截断。
    Budget(String),
    /// E_FIR_NUMERIC：系数含 NaN/Inf 或 max|h|=0。
    Numeric(String),
    /// E_FIR_PHASE_CLASS：线性相位对称/常数 GD 不满足。
    PhaseClass(String),
    /// E_FIR_FAMILY：method 未实现（不冒充等波纹）等家族自洽失败。
    Family(String),
    /// E_FIR_REF：参考归一化不满足 ±0.01 dB。
    Reference(String),
}

impl BrickwallError {
    /// 规范 §2.5 的失败码字符串。
    pub fn code(&self) -> &'static str {
        match self {
            BrickwallError::Band(_) => "E_FIR_BAND",
            BrickwallError::Delay(_) => "E_FIR_DELAY",
            BrickwallError::Taps(_) => "E_FIR_TAPS",
            BrickwallError::Budget(_) => "E_FIR_BUDGET",
            BrickwallError::Numeric(_) => "E_FIR_NUMERIC",
            BrickwallError::PhaseClass(_) => "E_FIR_PHASE_CLASS",
            BrickwallError::Family(_) => "E_FIR_FAMILY",
            BrickwallError::Reference(_) => "E_FIR_REF",
        }
    }

    /// 人类可读细节。
    pub fn detail(&self) -> &str {
        match self {
            BrickwallError::Band(s)
            | BrickwallError::Delay(s)
            | BrickwallError::Taps(s)
            | BrickwallError::Budget(s)
            | BrickwallError::Numeric(s)
            | BrickwallError::PhaseClass(s)
            | BrickwallError::Family(s)
            | BrickwallError::Reference(s) => s,
        }
    }
}

impl fmt::Display for BrickwallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.detail())
    }
}

impl std::error::Error for BrickwallError {}

/// 通过 validate 的完整设计参数（派生量集中在这里，构造与验证共用同一口径）。
#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedBrickwallSpec {
    /// 原参数（克隆）。
    pub spec: BrickwallSpec,
    /// 通带上界 f_p（Hz）。
    pub passband_edge_hz: f64,
    /// 阻带下界 f_s（Hz）。
    pub stopband_edge_hz: f64,
    /// 过渡带 Δf=f_s−f_p（Hz）。
    pub transition_hz: f64,
    /// 理想幅度截止 fc=(f_p+f_s)/2（Hz）。
    pub center_hz: f64,
    /// 体延迟 D=(L−1)/2（samples）。
    pub delay_samples: f64,
    /// delay_ms = D/fs·1000。
    pub delay_ms: f64,
    /// 归一化参考频点（本家族 DC=0 Hz）。
    pub reference_hz: f64,
    /// 实际 Kaiser beta。
    pub kaiser_beta: f64,
    /// Kaiser 可行性式所需 taps（L_req；A_s>=40 时 Some）。
    pub required_taps: Option<u64>,
    /// taps 运行准入分类。
    pub budget: BrickwallBudget,
}

// ---------------------------------------------------------------------------
// 数学工具（自包含实现：构造与验证不共用同一实现，保证独立复算）
// ---------------------------------------------------------------------------

/// 规范化 sinc：sin(πx)/(πx)，|x|<1e-12 时 1（与 T04 V16 参考实现一致）。
pub fn sinc_normalized(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        let px = PI * x;
        px.sin() / px
    }
}

/// 零阶第一类修正 Bessel 函数 I0(x)（级数，Kaiser 窗用）。
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

/// Kaiser beta 标准公式（§3.1/§3.5 参考构造：按 A_s 选 beta）：
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

/// 规范 §3.5 判据 5 / §9 V13 的 Kaiser 可行性式：
/// L_req = ceil(1 + (A_s−7.95)·fs/(14.357078·Δf))（A_s>=40 dB 口径）。
pub fn brickwall_required_taps(attenuation_db: f64, fs: f64, transition_hz: f64) -> Option<u64> {
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

/// Kaiser 窗（长度 taps，居中、精确镜像对称；taps=1 返回 [1]）。
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

/// 线性相位体延迟 D=(L−1)/2（samples；L 偶数时为半整数）。
pub fn delay_samples(taps: usize) -> f64 {
    if taps == 0 {
        0.0
    } else {
        (taps - 1) as f64 / 2.0
    }
}

/// delay_ms = D/fs·1000。
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

    /// 20·log10(max(|H|, 1e-12))（§1.2 下限 −240 dB）。
    pub fn magnitude_db(&self) -> f64 {
        20.0 * self.magnitude().max(MAGNITUDE_FLOOR).log10()
    }

    /// wrap 到 (−180,180] 的相位（度）。
    pub fn phase_deg(&self) -> f64 {
        self.im.atan2(self.re) * 180.0 / PI
    }
}

/// 直接 DTFT 求 H(f)（O(L)；复指数递推 + 每 64 点重锚，独立于窗构造）。
pub fn response_at(coeffs: &[f64], fs: f64, f_hz: f64) -> FreqResponse {
    if !fs.is_finite() || fs <= 0.0 || !f_hz.is_finite() {
        return FreqResponse {
            re: f64::NAN,
            im: f64::NAN,
        };
    }
    let omega = 2.0 * PI * f_hz / fs;
    let step_re = (-omega).cos();
    let step_im = (-omega).sin();
    let mut cur_re = 1.0_f64;
    let mut cur_im = 0.0_f64;
    let mut h_re = 0.0_f64;
    let mut h_im = 0.0_f64;
    for (n, &h) in coeffs.iter().enumerate() {
        h_re += h * cur_re;
        h_im += h * cur_im;
        let next_re = cur_re * step_re - cur_im * step_im;
        let next_im = cur_re * step_im + cur_im * step_re;
        cur_re = next_re;
        cur_im = next_im;
        if n % 64 == 63 {
            let angle = -omega * (n as f64 + 1.0);
            cur_re = angle.cos();
            cur_im = angle.sin();
        }
    }
    FreqResponse { re: h_re, im: h_im }
}

/// 20·log10|H(f)|（下限 −240 dB）。
pub fn magnitude_db_at(coeffs: &[f64], fs: f64, f_hz: f64) -> f64 {
    response_at(coeffs, fs, f_hz).magnitude_db()
}

/// 解析群延迟 GD(f)=Re(B/H)，B=Σ n·h[n]e^{−jωn}（§1.2 允许中心差分；解析式更准）。
///
/// 非法输入或 |H| 过小时为 NaN。
pub fn group_delay_at(coeffs: &[f64], fs: f64, f_hz: f64) -> f64 {
    if !fs.is_finite() || fs <= 0.0 || !f_hz.is_finite() {
        return f64::NAN;
    }
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
    if den <= 0.0 || !den.is_finite() {
        return f64::NAN;
    }
    (b_re * h_re + b_im * h_im) / den
}

/// 线性相位对称性误差 max|h[n] − h[L−1−n]|（§3.1 判据 1 的绝对量）。
pub fn symmetry_error(coeffs: &[f64]) -> f64 {
    if coeffs.is_empty() {
        return f64::NAN;
    }
    let l = coeffs.len();
    let mut max_err = 0.0_f64;
    for n in 0..l {
        let err = (coeffs[n] - coeffs[l - 1 - n]).abs();
        max_err = max_err.max(err);
    }
    max_err
}

/// 有效支撑（§1.3）：(first, last, effective_taps)；eps 为相对 max|h| 的门限。
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

fn ratio_db(num: f64, den: f64) -> f64 {
    if num <= 0.0 || den <= 0.0 || !num.is_finite() || !den.is_finite() {
        return -240.0;
    }
    20.0 * (num / den).log10()
}

fn energy_ratio_db(num: f64, den: f64) -> f64 {
    if num <= 0.0 || den <= 0.0 || !num.is_finite() || !den.is_finite() {
        return -240.0;
    }
    10.0 * (num / den).log10()
}

// ---------------------------------------------------------------------------
// 振铃（§1.4 / §3.5 判据 6）
// ---------------------------------------------------------------------------

/// 振铃统计（§1.4 口径；W=ceil(0.000333·fs)；p=首个 |h| 主峰）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RingingStats {
    /// 排除窗 W（samples）。
    pub exclusion_samples: usize,
    /// pre_ring_db = 20log10(max_{n<p−W}|h| / max|h|)；不可用（支撑过短）为 None。
    pub pre_ring_db: Option<f64>,
    /// post_ring_db；不可用为 None。
    pub post_ring_db: Option<f64>,
    /// pre_ring_energy_db = 10log10(Σ_{n<p−W} h² / Σ h²)；不可用为 None。
    pub pre_ring_energy_db: Option<f64>,
    /// post_ring_energy_db；不可用为 None。
    pub post_ring_energy_db: Option<f64>,
    /// 主峰向左衰减到 −80 dB 的样本数（无非阈值以上样本记 0）。
    pub pre_decay_samples: usize,
    /// 主峰向右衰减到 −80 dB 的样本数。
    pub post_decay_samples: usize,
    /// max(pre, post)（§3.5 判据 6 的 duration）。
    pub decay_to_minus_80db_samples: usize,
}

impl RingingStats {
    /// 支撑过短导致两侧均 n/a。
    pub fn is_not_applicable(&self) -> bool {
        self.pre_ring_db.is_none() && self.post_ring_db.is_none()
    }
}

/// 计算振铃统计（§1.4：p=首个 argmax|h|；W=ceil(0.000333·fs)；零填充线性卷积系数）。
pub fn ringing_stats(coeffs: &[f64], fs: f64) -> RingingStats {
    let none = RingingStats {
        exclusion_samples: 0,
        pre_ring_db: None,
        post_ring_db: None,
        pre_ring_energy_db: None,
        post_ring_energy_db: None,
        pre_decay_samples: 0,
        post_decay_samples: 0,
        decay_to_minus_80db_samples: 0,
    };
    if coeffs.is_empty() || !fs.is_finite() || fs <= 0.0 {
        return none;
    }
    let l = coeffs.len();
    let w = (RINGING_EXCLUSION_SECONDS * fs).ceil() as usize;
    let mut peak = 0.0_f64;
    let mut p_int = 0_usize;
    for (n, &h) in coeffs.iter().enumerate() {
        let a = h.abs();
        if a > peak {
            peak = a;
            p_int = n;
        }
    }
    if peak <= 0.0 || !peak.is_finite() {
        return none;
    }
    // 主峰位置 p：§1.4 规定 argmax。偶长精确对称（brickwall 恒线性相位）时主峰被两个
    // 等高样本平分，物理中心 p=(L−1)/2 才是对称的 argmax；取分数中心使 pre/post 关于
    // 主峰对称，并与 T04 V16 参考实现（c=(L−1)/2）逐位一致。非对称/奇长时退回整数 argmax。
    let sym_err = symmetry_error(coeffs);
    let symmetric = sym_err.is_finite() && sym_err <= 1e-9 * peak;
    let p = if symmetric {
        (l as f64 - 1.0) / 2.0
    } else {
        p_int as f64
    };
    let total_energy: f64 = coeffs.iter().map(|h| h * h).sum();
    let decay_threshold = peak * 10f64.powf(RINGING_DECAY_DB / 20.0);

    let pre_limit = p - w as f64;
    let (pre_ring_db, pre_ring_energy_db) = if pre_limit <= 0.0 {
        (None, None)
    } else {
        let end = (pre_limit.ceil() as usize).min(l);
        let mut pre_max = 0.0_f64;
        let mut pre_energy = 0.0_f64;
        for &h in &coeffs[..end] {
            pre_max = pre_max.max(h.abs());
            pre_energy += h * h;
        }
        (
            Some(ratio_db(pre_max, peak)),
            Some(energy_ratio_db(pre_energy, total_energy)),
        )
    };
    let post_limit = p + w as f64;
    let (post_ring_db, post_ring_energy_db) = if post_limit >= l as f64 {
        (None, None)
    } else {
        let start = ((post_limit.floor() + 1.0) as usize).min(l);
        if start >= l {
            (None, None)
        } else {
            let mut post_max = 0.0_f64;
            let mut post_energy = 0.0_f64;
            for &h in &coeffs[start..] {
                post_max = post_max.max(h.abs());
                post_energy += h * h;
            }
            (
                Some(ratio_db(post_max, peak)),
                Some(energy_ratio_db(post_energy, total_energy)),
            )
        }
    };

    let mut pre_decay = 0_usize;
    for n in (0..p_int).rev() {
        if coeffs[n].abs() > decay_threshold {
            pre_decay = p_int - n;
            break;
        }
    }
    let mut post_decay = 0_usize;
    for (n, &h) in coeffs.iter().enumerate().skip(p_int + 1) {
        if h.abs() > decay_threshold {
            post_decay = n - p_int;
        }
    }
    RingingStats {
        exclusion_samples: w,
        pre_ring_db,
        post_ring_db,
        pre_ring_energy_db,
        post_ring_energy_db,
        pre_decay_samples: pre_decay,
        post_decay_samples: post_decay,
        decay_to_minus_80db_samples: pre_decay.max(post_decay),
    }
}

// ---------------------------------------------------------------------------
// 校验
// ---------------------------------------------------------------------------

/// 校验 brickwall 参数（§3.5 参数/判据 7 边界），返回派生量；失败带 E_FIR_* 码。
pub fn validate(spec: &BrickwallSpec) -> Result<ValidatedBrickwallSpec, BrickwallError> {
    use BrickwallError as E;

    if !spec.fs.is_finite() || spec.fs <= 0.0 {
        return Err(E::Band(format!("fs 必须有限且 >0，实际 {}", spec.fs)));
    }
    if spec.taps < MIN_TAPS {
        return Err(E::Band(format!(
            "taps 必须 >= {MIN_TAPS}，实际 {}",
            spec.taps
        )));
    }
    if spec.taps > MAX_TAPS {
        return Err(E::Budget(format!(
            "taps={} > {MAX_TAPS}：§7.2 规定 >16384 仅 offline 设计输出、runtime 不放行；本设计器上限 {MAX_TAPS}，不静默截断",
            spec.taps
        )));
    }
    if !spec.ripple_db.is_finite() || spec.ripple_db < 0.0 {
        return Err(E::Band(format!(
            "ripple_db 必须有限且 >=0，实际 {}",
            spec.ripple_db
        )));
    }
    if !spec.attenuation_db.is_finite() {
        return Err(E::Band(format!(
            "attenuation_db 必须有限，实际 {}",
            spec.attenuation_db
        )));
    }
    if spec.attenuation_db < MIN_ATTENUATION_DB || spec.attenuation_db > MAX_ATTENUATION_DB {
        return Err(E::Band(format!(
            "attenuation_db={} 必须在 [{MIN_ATTENUATION_DB}, {MAX_ATTENUATION_DB}] dB（§3.5 判据 7）",
            spec.attenuation_db
        )));
    }
    let pb = spec.passband_hz;
    let sb = spec.stopband_hz;
    if !pb[0].is_finite() || !pb[1].is_finite() || !sb[0].is_finite() || !sb[1].is_finite() {
        return Err(E::Band("passband_hz/stopband_hz 含非有限值".to_string()));
    }
    let tol_hz = 1e-9 * (spec.fs.abs() + 1.0);
    let guard = NYQUIST_GUARD_FRACTION * spec.fs;
    if (pb[0] - MEASUREMENT_LO_HZ).abs() > 1e-9 {
        return Err(E::Band(format!(
            "passband_hz[0] 必须={MEASUREMENT_LO_HZ} Hz（§1.2 测量网格下界），实际 {}",
            pb[0]
        )));
    }
    let f_p = pb[1];
    let f_s = sb[0];
    if !(MEASUREMENT_LO_HZ < f_p && f_p < f_s) {
        return Err(E::Band(format!(
            "要求 20 < f_p < f_s（过渡带必须 >0），实际 f_p={f_p} f_s={f_s}"
        )));
    }
    if f_s > guard + tol_hz {
        return Err(E::Band(format!(
            "f_s 必须 <= 0.499fs={guard}（§3.5 判据 3/7），实际 {f_s}"
        )));
    }
    if (sb[1] - guard).abs() > tol_hz {
        return Err(E::Band(format!(
            "stopband_hz[1] 必须=0.499fs={guard}，实际 {}",
            sb[1]
        )));
    }
    let transition_hz = f_s - f_p;
    if transition_hz < MIN_TRANSITION_HZ - 1e-9 {
        return Err(E::Band(format!(
            "过渡带 Δf=f_s−f_p 必须 >= {MIN_TRANSITION_HZ} Hz（§3.5 判据 7），实际 {transition_hz}"
        )));
    }
    if !spec.method.implemented() {
        return Err(E::Family(format!(
            "method={} 未实现：本 leaf 只交付 kaiser_windowed_sinc 有限近似；拒绝冒充 least_squares/remez（不静默降级）",
            spec.method
        )));
    }
    let kaiser_beta = spec
        .kaiser_beta
        .unwrap_or_else(|| kaiser_beta_for_attenuation(spec.attenuation_db));
    if !kaiser_beta.is_finite() || kaiser_beta < 0.0 {
        return Err(E::Band(format!(
            "kaiser_beta 必须有限且 >=0，实际 {kaiser_beta}"
        )));
    }
    let d = delay_samples(spec.taps);
    if let Some(declared) = spec.declared_delay_samples {
        if !declared.is_finite() || (declared - d).abs() > 1e-9 {
            return Err(E::Delay(format!(
                "brickwall 声明 D 必须=(L−1)/2={d}，实际 {declared}"
            )));
        }
    }
    let center_hz = 0.5 * (f_p + f_s);
    let required_taps = brickwall_required_taps(spec.attenuation_db, spec.fs, transition_hz);
    Ok(ValidatedBrickwallSpec {
        spec: spec.clone(),
        passband_edge_hz: f_p,
        stopband_edge_hz: f_s,
        transition_hz,
        center_hz,
        delay_samples: d,
        delay_ms: d / spec.fs * 1000.0,
        reference_hz: 0.0,
        kaiser_beta,
        required_taps,
        budget: BrickwallBudget::for_taps(spec.taps),
    })
}

// ---------------------------------------------------------------------------
// 设计
// ---------------------------------------------------------------------------

/// brickwall 设计产物（tspi-fir/v1 manifest 的系数源；WAV/manifest 归 T55）。
#[derive(Clone, Debug, PartialEq)]
pub struct BrickwallDesign {
    /// 原参数。
    pub spec: BrickwallSpec,
    /// f64 冲激响应（len=taps，因果、线性相位对称）。
    pub coeffs: Vec<f64>,
    /// 实际使用的方法。
    pub method: BrickwallMethod,
    /// 实际使用的 Kaiser beta。
    pub kaiser_beta: f64,
    /// 理想幅度截止 fc=(f_p+f_s)/2（Hz）。
    pub center_hz: f64,
    /// D=(L−1)/2（samples）。
    pub delay_samples: f64,
    /// D/fs·1000（ms）。
    pub delay_ms: f64,
    /// 归一化参考频点（0 Hz=DC）。
    pub reference_hz: f64,
    /// 声明的参考增益（dB，本模块固定 0 dB）。
    pub reference_gain_db: f64,
    /// max|h|。
    pub peak_abs: f64,
    /// Σh（DC 归一后 ≈1）。
    pub sum_coeffs: f64,
    /// taps 运行准入分类。
    pub budget: BrickwallBudget,
}

impl BrickwallDesign {
    /// taps = len(h)。
    pub fn taps(&self) -> usize {
        self.coeffs.len()
    }

    /// f32 导出（WAV/运行器用）。
    pub fn coeffs_f32(&self) -> Vec<f32> {
        self.coeffs.iter().map(|&h| h as f32).collect()
    }

    /// 有效支撑 (first, last, effective_taps)，eps=1e-6（§1.3）。
    pub fn effective_support(&self) -> (usize, usize, usize) {
        effective_support(&self.coeffs, 1e-6)
    }

    /// 振铃统计（§1.4）。
    pub fn ringing(&self) -> RingingStats {
        ringing_stats(&self.coeffs, self.spec.fs)
    }
}

/// 构造 brickwall 系数（只构造 + 结构自检；全网格响应判定用 design_checked）。
///
/// 失败码：E_FIR_BAND/E_FIR_FAMILY/E_FIR_DELAY/E_FIR_BUDGET/E_FIR_NUMERIC/E_FIR_PHASE_CLASS/E_FIR_REF。
pub fn design(spec: &BrickwallSpec) -> Result<BrickwallDesign, BrickwallError> {
    let v = validate(spec)?;

    // 可行性预检（§3.5 判据 5）：不足即 E_FIR_BUDGET，不静默截短 taps。
    if let Some(required) = v.required_taps {
        if (spec.taps as u64) < required {
            return Err(BrickwallError::Budget(format!(
                "Kaiser 可行性式要求 taps >= {required}（A_s={} dB, Δf={} Hz, fs={}），实际 {}；不得截短/降级冒充",
                spec.attenuation_db, v.transition_hz, spec.fs, spec.taps
            )));
        }
    }

    let l = spec.taps;
    let window = kaiser_window(l, v.kaiser_beta);
    let r = 2.0 * v.center_hz / spec.fs;
    let d = v.delay_samples;
    let mut h = vec![0.0_f64; l];
    let half = l / 2;
    for i in 0..half {
        let val = r * sinc_normalized(r * (i as f64 - d)) * window[i];
        h[i] = val;
        h[l - 1 - i] = val;
    }
    if l % 2 == 1 {
        let i = l / 2;
        h[i] = r * sinc_normalized(r * (i as f64 - d)) * window[i];
    }
    let sum: f64 = h.iter().sum();
    if !sum.is_finite() || sum <= 0.0 {
        return Err(BrickwallError::Numeric(format!(
            "DC 归一失败：Σh={sum}（应≈1）"
        )));
    }
    let scale = 1.0 / sum;
    for x in h.iter_mut() {
        *x *= scale;
    }

    let mut peak_abs = 0.0_f64;
    for &hv in &h {
        if !hv.is_finite() {
            return Err(BrickwallError::Numeric("系数含 NaN/Inf".to_string()));
        }
        peak_abs = peak_abs.max(hv.abs());
    }
    if peak_abs <= 0.0 || !peak_abs.is_finite() {
        return Err(BrickwallError::Numeric("max|h|=0 或非有限".to_string()));
    }
    let sym_err = symmetry_error(&h);
    if !(sym_err <= 1e-9 * peak_abs) {
        return Err(BrickwallError::PhaseClass(format!(
            "线性相位对称构造失败：max|h−flip(h)|={sym_err} > 1e-9·max|h|"
        )));
    }
    let reference = response_at(&h, spec.fs, v.reference_hz).magnitude_db();
    if !reference.is_finite() || reference.abs() > 1e-6 {
        return Err(BrickwallError::Reference(format!(
            "DC 参考归一化失败：|H(0)|={reference} dB（目标 0 dB）"
        )));
    }
    let sum_coeffs: f64 = h.iter().sum();
    Ok(BrickwallDesign {
        spec: spec.clone(),
        coeffs: h,
        method: spec.method,
        kaiser_beta: v.kaiser_beta,
        center_hz: v.center_hz,
        delay_samples: v.delay_samples,
        delay_ms: v.delay_ms,
        reference_hz: v.reference_hz,
        reference_gain_db: REFERENCE_GAIN_DB,
        peak_abs,
        sum_coeffs,
        budget: v.budget,
    })
}

// ---------------------------------------------------------------------------
// 独立响应验证
// ---------------------------------------------------------------------------

/// 独立响应验证的网格/点数选项（默认按 §1.2；compact 便于快速自检，正式报告用 normative）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrickwallAnalysisOptions {
    /// 主幅度网格点数（§1.2：>= max(8192, 8·L)）。
    pub mag_points: usize,
    /// 过渡带线性网格点数（§1.2：同 N）。
    pub transition_points: usize,
    /// 20 kHz→0.499fs 扩展网格点数（仅当 0.499fs > min(20000, 0.499fs) 时使用）。
    pub extension_points: usize,
}

impl BrickwallAnalysisOptions {
    /// 规范 §1.2 网格：N = max(8192, 8·L)。
    pub fn normative(spec: &BrickwallSpec) -> Self {
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
        Self {
            mag_points: n,
            transition_points: n,
            extension_points: 64,
        }
    }
}

/// 频带幅度统计（dB）。
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
    /// 1 kHz 处 GD（§2.2 group_delay.at_1khz_samples）。
    pub at_1khz_samples: f64,
    /// 是否满足常数 GD（§3.5 判据 1，±0.01 samples）。
    pub class_constant: bool,
}

/// 过渡带判定结果（§3.5 判据 4）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransitionStats {
    /// 首次达到 −A_s 的频点（None=搜索范围内未穿越）。
    pub edge_hz: Option<f64>,
    /// 规范上限 f_s（Hz）。
    pub limit_hz: f64,
    /// 判定容差 max(0.05·Δf, 20 Hz)（Hz）。
    pub tolerance_hz: f64,
    /// 搜索网格点数。
    pub points: usize,
}

/// 参考归一化结果（§1.5）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReferenceStats {
    /// 参考频点（本家族 DC=0 Hz）。
    pub hz: f64,
    /// manifest 声明增益（dB）。
    pub declared_gain_db: f64,
    /// 独立复算得到的 |H|（dB）。
    pub measured_gain_db: f64,
}

/// 逐条规范性判据结果（§3.5 判据 1–4/6 + 通用判据）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrickwallChecks {
    /// 有限且 max|h|>0。
    pub finite: bool,
    /// 判据 1：线性相位对称。
    pub symmetry: bool,
    /// D=(L−1)/2、delay_ms 一致。
    pub delay: bool,
    /// 判据 1：通带常数 GD=(L−1)/2（±0.01 samples）。
    pub group_delay: bool,
    /// 判据 2：通带 max−min <= ripple_db+0.05。
    pub passband: bool,
    /// 判据 3：阻带 [f_s,0.499fs] max <= −(A_s−1)。
    pub stopband: bool,
    /// 判据 4：−A_s 穿越点 <= f_s + max(0.05Δf,20 Hz)。
    pub transition: bool,
    /// §1.5：|H(reference)| 与声明增益差 <= 0.01 dB。
    pub reference: bool,
    /// 预算：taps >= L_req 且进 extended profile（不超 16384）。
    pub budget: bool,
}

impl BrickwallChecks {
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
            && self.budget
    }
}

/// brickwall 独立响应验证报告（T55 报告器输入；不含 WAV/manifest 等 T55 字段）。
#[derive(Clone, Debug, PartialEq)]
pub struct BrickwallVerification {
    /// 采样率。
    pub fs: f64,
    /// taps。
    pub taps: usize,
    /// 构造方法。
    pub method: BrickwallMethod,
    /// Kaiser beta。
    pub kaiser_beta: f64,
    /// fc=(f_p+f_s)/2（Hz）。
    pub center_hz: f64,
    /// D=(L−1)/2。
    pub delay_samples: f64,
    /// D/fs·1000。
    pub delay_ms: f64,
    /// §3.5 判据 5 的 L_req（A_s>=40 时 Some）。
    pub required_taps: Option<u64>,
    /// §1.2 主网格点数（>=max(8192,8L)；normative）。
    pub grid_points: usize,
    /// max|h|。
    pub max_abs_h: f64,
    /// 对称性误差 max|h−flip(h)|（绝对值）。
    pub symmetry_max_abs_error: f64,
    /// 通带幅度统计。
    pub passband: BandStats,
    /// 阻带幅度统计（[f_s, 0.499fs]）。
    pub stopband: BandStats,
    /// 群延迟统计。
    pub group_delay: GroupDelayStats,
    /// 过渡带判定。
    pub transition: TransitionStats,
    /// 参考归一化。
    pub reference: ReferenceStats,
    /// 20 Hz / 20 kHz 端点（§2.4）。
    pub endpoints: BrickwallEndpoints,
    /// 振铃（§1.4；§3.5 判据 6 只报告无上限）。
    pub ringing: RingingStats,
    /// taps 运行准入分类。
    pub budget: BrickwallBudget,
    /// 逐条判据。
    pub checks: BrickwallChecks,
    /// 未通过判据的人类可读原因（空=全过）。
    pub failures: Vec<String>,
}

impl BrickwallVerification {
    /// 全部判据通过。
    pub fn passed(&self) -> bool {
        self.checks.passed()
    }

    /// 0.45fs 处 |H|（dB；§3.8 apodizing vs brickwall 互斥判定用，T55 引用）。
    pub fn mag_db_at_045_fs(coeffs: &[f64], fs: f64) -> f64 {
        magnitude_db_at(coeffs, fs, 0.45 * fs)
    }
}

/// 单点幅度测量（内部）。
#[derive(Clone, Copy, Debug)]
struct MagSample {
    f_hz: f64,
    mag_db: f64,
}

fn eval_mag(coeffs: &[f64], fs: f64, f_hz: f64) -> f64 {
    // 仅幅度的一趟递推（与 response_at 同算法，省去 GD 的 b 累加）。
    let omega = 2.0 * PI * f_hz / fs;
    let step_re = (-omega).cos();
    let step_im = (-omega).sin();
    let mut cur_re = 1.0_f64;
    let mut cur_im = 0.0_f64;
    let mut h_re = 0.0_f64;
    let mut h_im = 0.0_f64;
    for (n, &h) in coeffs.iter().enumerate() {
        h_re += h * cur_re;
        h_im += h * cur_im;
        let next_re = cur_re * step_re - cur_im * step_im;
        let next_im = cur_re * step_im + cur_im * step_re;
        cur_re = next_re;
        cur_im = next_im;
        if n % 64 == 63 {
            let angle = -omega * (n as f64 + 1.0);
            cur_re = angle.cos();
            cur_im = angle.sin();
        }
    }
    let mag = h_re.hypot(h_im);
    20.0 * mag.max(MAGNITUDE_FLOOR).log10()
}

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

fn stats_for(samples: &[MagSample], lo: f64, hi: f64) -> BandStats {
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

fn band_stats(coeffs: &[f64], fs: f64, lo: f64, hi: f64, points: usize) -> BandStats {
    let n = points.max(2);
    let grid = linear_grid(lo, hi, n);
    let samples: Vec<MagSample> = grid
        .iter()
        .map(|&f| MagSample {
            f_hz: f,
            mag_db: eval_mag(coeffs, fs, f),
        })
        .collect();
    stats_for(&samples, lo, hi)
}

fn endpoint_at(coeffs: &[f64], fs: f64, freq_hz: f64) -> Endpoint {
    let guard = NYQUIST_GUARD_FRACTION * fs;
    if !freq_hz.is_finite() || freq_hz <= 0.0 || freq_hz > guard + 1e-9 {
        return Endpoint::NotApplicableAtFs {
            reason: format!("{freq_hz} Hz > 0.499*fs={guard} Hz：当前 fs 下不可测（§2.4）"),
        };
    }
    let resp = response_at(coeffs, fs, freq_hz);
    Endpoint::Measured {
        freq_hz,
        mag_db: resp.magnitude_db(),
        phase_deg: resp.phase_deg(),
    }
}

/// 独立响应验证（规范网格；正式报告口径）。等价 verify_brickwall_with + normative。
pub fn verify_brickwall(
    spec: &BrickwallSpec,
    coeffs: &[f64],
) -> Result<BrickwallVerification, BrickwallError> {
    verify_brickwall_with(spec, coeffs, &BrickwallAnalysisOptions::normative(spec))
}

/// 独立响应验证（自定义网格/点数）。
pub fn verify_brickwall_with(
    spec: &BrickwallSpec,
    coeffs: &[f64],
    options: &BrickwallAnalysisOptions,
) -> Result<BrickwallVerification, BrickwallError> {
    let v = validate(spec)?;
    let l = coeffs.len();
    if l != spec.taps {
        return Err(BrickwallError::Taps(format!(
            "系数长度 {l} != 声明 taps {}",
            spec.taps
        )));
    }
    let mut max_abs_h = 0.0_f64;
    for &h in coeffs {
        if !h.is_finite() {
            return Err(BrickwallError::Numeric("系数含 NaN/Inf".to_string()));
        }
        max_abs_h = max_abs_h.max(h.abs());
    }
    if max_abs_h <= 0.0 || !max_abs_h.is_finite() {
        return Err(BrickwallError::Numeric("max|h|=0 或非有限".to_string()));
    }

    let fs = spec.fs;
    let d = v.delay_samples;
    let guard = NYQUIST_GUARD_FRACTION * fs;

    // §1.2 主网格：对数 20 Hz..min(20000,0.499fs)，N>=max(8192,8L)（normative）；
    // band 统计另用更密的线性网格（比规范下限更严，避免漏掉最大纹波峰）。
    let mag_n = options.mag_points.max(2);
    let main_hi = guard.min(MEASUREMENT_HI_HZ);
    let mut grid_points = 0_usize;
    let mut main_finite = true;
    for f in log_grid(MEASUREMENT_LO_HZ, main_hi, mag_n) {
        if !eval_mag(coeffs, fs, f).is_finite() {
            main_finite = false;
        }
        grid_points += 1;
    }
    if guard > main_hi + 1e-9 * (fs + 1.0) {
        let ext_n = options.extension_points.max(2);
        for &f in linear_grid(main_hi, guard, ext_n).iter() {
            if !eval_mag(coeffs, fs, f).is_finite() {
                main_finite = false;
            }
            grid_points += 1;
        }
    }
    let checks_finite = main_finite;

    let passband = band_stats(
        coeffs,
        fs,
        MEASUREMENT_LO_HZ,
        v.passband_edge_hz,
        options.mag_points,
    );
    let stopband = band_stats(coeffs, fs, v.stopband_edge_hz, guard, options.mag_points);

    // 过渡带穿越搜索（§1.2/§3.5 判据 4）：[f_p−0.2fs, f_s+0.2fs] 截到测量域。
    let t_lo = (v.passband_edge_hz - 0.2 * fs).max(MEASUREMENT_LO_HZ);
    let t_hi = (v.stopband_edge_hz + 0.2 * fs).min(guard);
    let t_hi = t_hi.max(v.stopband_edge_hz);
    let t_n = options.transition_points.max(2);
    let mut transition_edge = None;
    for &f in linear_grid(t_lo, t_hi, t_n).iter() {
        if eval_mag(coeffs, fs, f) <= -spec.attenuation_db {
            transition_edge = Some(f);
            break;
        }
    }
    let tol_hz = (TRANSITION_TOLERANCE_FRACTION * v.transition_hz).max(TRANSITION_TOLERANCE_MIN_HZ);
    let transition_limit = v.stopband_edge_hz + tol_hz;
    let transition_ok = match transition_edge {
        Some(edge) => edge <= transition_limit + 1e-9 * (fs + 1.0),
        None => false,
    };

    // 群延迟（§3.1/§3.5 判据 1）：通带内线性网格 256 点，解析 GD。
    let gd_n = 256_usize.min(options.mag_points.max(2));
    let mut gd_min = f64::INFINITY;
    let mut gd_max = f64::NEG_INFINITY;
    let mut gd_sum = 0.0_f64;
    let mut gd_count = 0_usize;
    let mut gd_ok = true;
    for &f in linear_grid(MEASUREMENT_LO_HZ, v.passband_edge_hz, gd_n).iter() {
        let gd = group_delay_at(coeffs, fs, f);
        if !gd.is_finite() {
            gd_ok = false;
            continue;
        }
        gd_min = gd_min.min(gd);
        gd_max = gd_max.max(gd);
        gd_sum += gd;
        gd_count += 1;
        if (gd - d).abs() > GROUP_DELAY_TOLERANCE_SAMPLES {
            gd_ok = false;
        }
    }
    if gd_count == 0 {
        gd_ok = false;
        gd_min = f64::NAN;
        gd_max = f64::NAN;
    }
    let gd_mean = if gd_count > 0 {
        gd_sum / gd_count as f64
    } else {
        f64::NAN
    };
    let gd_at_1k = group_delay_at(coeffs, fs, 1000.0);

    // 参考归一化（§1.5）。
    let reference_measured = response_at(coeffs, fs, v.reference_hz).magnitude_db();

    // 端点（§2.4）。
    let endpoints = BrickwallEndpoints {
        f20: endpoint_at(coeffs, fs, MEASUREMENT_LO_HZ),
        f20k: endpoint_at(coeffs, fs, MEASUREMENT_HI_HZ),
    };

    // 判据。
    let mut failures: Vec<String> = Vec::new();
    let symmetry_err = symmetry_error(coeffs);
    let symmetry_ok = symmetry_err <= SYMMETRY_TOLERANCE_RELATIVE * max_abs_h;
    if !symmetry_ok {
        failures.push(format!(
            "判据1 线性相位对称失败：max|h−flip(h)|={symmetry_err:.3e} > {}·max|h|",
            SYMMETRY_TOLERANCE_RELATIVE
        ));
    }
    let delay_taps = delay_samples(l);
    let delay_ok = (delay_taps - d).abs() <= 1e-9
        && (v.delay_ms - d / fs * 1000.0).abs() <= 1e-12 * (v.delay_ms.abs() + 1.0);
    if !delay_ok {
        failures.push(format!(
            "D 判据失败：measured D={delay_taps} vs 声明 {d}，delay_ms={}",
            v.delay_ms
        ));
    }
    if !gd_ok {
        failures.push(format!(
            "判据1 常数 GD 失败：通带 GD∈[{gd_min}, {gd_max}] vs D={d}（±{GROUP_DELAY_TOLERANCE_SAMPLES} samples）"
        ));
    }
    let passband_ripple = passband.ripple_db();
    let passband_ok = passband.points > 0
        && passband_ripple.is_finite()
        && passband_ripple <= spec.ripple_db + PASSBAND_RIPPLE_MARGIN_DB;
    if !passband_ok {
        failures.push(format!(
            "判据2 通带纹波失败：max−min={passband_ripple:.4} dB > 声明 {}+{} dB（点数 {}）",
            spec.ripple_db, PASSBAND_RIPPLE_MARGIN_DB, passband.points
        ));
    }
    let stopband_limit_db = -(spec.attenuation_db - STOPBAND_MARGIN_DB);
    let stopband_ok =
        stopband.points > 0 && stopband.max_db.is_finite() && stopband.max_db <= stopband_limit_db;
    if !stopband_ok {
        failures.push(format!(
            "判据3 阻带衰减失败：max={:.4} dB > {:.4} dB（要求 <= −(A_s−{STOPBAND_MARGIN_DB})，点数 {}）",
            stopband.max_db, stopband_limit_db, stopband.points
        ));
    }
    if !transition_ok {
        match transition_edge {
            None => failures.push(format!(
                "判据4 过渡失败：搜索范围 [{t_lo:.3}, {t_hi:.3}] Hz 内未达到 −{} dB",
                spec.attenuation_db
            )),
            Some(edge) => failures.push(format!(
                "判据4 过渡失败：−{} dB 穿越点 {edge:.3} Hz > f_s+容差={transition_limit:.3} Hz",
                spec.attenuation_db
            )),
        }
    }
    let reference_ok = reference_measured.is_finite()
        && (reference_measured - REFERENCE_GAIN_DB).abs() <= REFERENCE_TOLERANCE_DB;
    if !reference_ok {
        failures.push(format!(
            "参考归一化失败：|H({})|={reference_measured:.6} dB vs 声明 {REFERENCE_GAIN_DB} dB（±{REFERENCE_TOLERANCE_DB}）",
            v.reference_hz
        ));
    }
    let required_ok = match v.required_taps {
        Some(required) => (l as u64) >= required,
        None => true,
    };
    let budget_ok = required_ok
        && v.budget.fits_extended_profile
        && v.budget.profile != BrickwallProfile::OfflineOnly;
    if !budget_ok {
        let required = v
            .required_taps
            .map(|r| r.to_string())
            .unwrap_or_else(|| "n/a".to_string());
        failures.push(format!(
            "预算失败：taps={l} < L_req={required} 或超出 extended profile（§7.2；不静默截短）"
        ));
    }

    let ringing = ringing_stats(coeffs, fs);
    let checks = BrickwallChecks {
        finite: checks_finite,
        symmetry: symmetry_ok,
        delay: delay_ok,
        group_delay: gd_ok,
        passband: passband_ok,
        stopband: stopband_ok,
        transition: transition_ok,
        reference: reference_ok,
        budget: budget_ok,
    };

    Ok(BrickwallVerification {
        fs,
        taps: l,
        method: spec.method,
        kaiser_beta: v.kaiser_beta,
        center_hz: v.center_hz,
        delay_samples: d,
        delay_ms: v.delay_ms,
        required_taps: v.required_taps,
        grid_points,
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
            tolerance_hz: tol_hz,
            points: t_n,
        },
        reference: ReferenceStats {
            hz: v.reference_hz,
            declared_gain_db: REFERENCE_GAIN_DB,
            measured_gain_db: reference_measured,
        },
        endpoints,
        ringing,
        budget: v.budget,
        checks,
        failures,
    })
}

/// 对已构造设计做独立验证（等价 verify_brickwall(design.spec, design.coeffs)）。
pub fn verify_brickwall_design(
    design: &BrickwallDesign,
) -> Result<BrickwallVerification, BrickwallError> {
    verify_brickwall(&design.spec, &design.coeffs)
}

/// 构造 + 独立验证；任一判据失败返回 E_FIR_*（不返回“看起来成功”的坏设计）。
pub fn design_checked(spec: &BrickwallSpec) -> Result<BrickwallDesign, BrickwallError> {
    design_checked_with(spec, &BrickwallAnalysisOptions::normative(spec))
}

/// 构造 + 独立验证（自定义网格/点数）。
pub fn design_checked_with(
    spec: &BrickwallSpec,
    options: &BrickwallAnalysisOptions,
) -> Result<BrickwallDesign, BrickwallError> {
    let design = design(spec)?;
    let verification = verify_brickwall_with(spec, &design.coeffs, options)?;
    if verification.passed() {
        Ok(design)
    } else {
        Err(criteria_to_error(&verification))
    }
}

/// 把未通过判据分类到 E_FIR_* 码（T55/上层消费；失败集非空时使用）。
pub fn criteria_to_error(v: &BrickwallVerification) -> BrickwallError {
    let detail = v.failures.join("；");
    if !v.checks.finite || !v.checks.symmetry || !v.checks.delay || !v.checks.group_delay {
        BrickwallError::PhaseClass(detail)
    } else if !v.checks.reference {
        BrickwallError::Reference(detail)
    } else if !v.checks.budget {
        BrickwallError::Budget(detail)
    } else {
        BrickwallError::Budget(format!("设计在声明参数下不可行：{detail}"))
    }
}

// ---------------------------------------------------------------------------
// 私有测试（tests/T36 通过 include! 进入本模块的 #[cfg(test)] 子模块）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod brickwall_tests {
    use super::*;

    include!("../tests/T36/brickwall_cases.rs");
}
