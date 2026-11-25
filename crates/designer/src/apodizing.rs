//! [NONRT] apodizing 低前振铃族 FIR 设计（WBS T35）—— T35 写域。
//!
//! 本模块**只**实现 apodizing 家族：可算目标（raised-cosine 幅度目标）+ 可算窗（Kaiser/Hann²）
//! 的线性相位对称合成，以及前后振铃/“阻带”（Nyquist 带）/相位量化（常数群延迟 + f32 量化误差复算）
//! 的独立测量报告；不仿任何专有 DAC。
//!
//! 权威契约：
//! - `rt-stack/spec/fir-toolbox.md` v1.2-draft §3.4（apodizing 参数/参考构造/判据 1–8）、
//!   §3.8（与 brickwall 的家族互斥）、§1.2（响应测量程序）、§1.3（支撑/延迟）、
//!   §1.4（前/后振铃排除窗）、§1.5（参考归一化）、§1.6（f32 量化误差必须复算）、§2.5（E_FIR_* 失败码）；
//! - Lead fs/2 裁决 2026-11-17（`evidence/T04/RULING-fs2-2026-11-17.md`）：
//!   magnitude-only 目标允许 `freq <= fs/2`；带 phase 的目标点才禁止等于 fs/2。
//!   本模块的幅度目标是 **magnitude-only**（无 phase 列），所以合成时 Nyquist bin 取幅度下限
//!   `floor=10^(−120/20)=1e-6`（与 T04 V16 参考构造一致）；相位/群延迟只在 `f < fs/2` 的报告网格上测量。
//!
//! 能力：
//! - 参数：`taps`、`passband_hz=[20, f_p]`（`f_p <= 0.85*fs/2`，违反 → `E_FIR_APO_FP`）、
//!   `ripple_db`（默认 0.5）、`nyquist_attenuation_db`（>=30，默认 30）、
//!   `apodizing_window: kaiser{beta?}|hann_squared`、`pre_ring_bound_db`（默认 −45，只允许更严，不得放宽）。
//! - 参考构造（§3.4，可复算，T04 `evidence/T04/recompute-vectors.mjs` V16）：幅度目标 |H|=1 (f<=f_p)、
//!   f_p..fs/2 升余弦 1→floor（−120 dB）；对称合成（N=32768、D=(L−1)/2）后乘 apodizing 窗
//!   （Kaiser 默认 beta=8.96 / Hann²），DC 归一到 1；线性相位，不是砖墙，不做专有 DAC 复刻。
//! - 独立验证（§1.2 网格）：对称/常数 GD、通带纹波、Nyquist 带（“阻带”）衰减、前后振铃 dB/能量、
//!   过渡单调、0.45fs 砖墙探测（≤−60 dB → 家族拒收 `E_FIR_FAMILY`）、参考 0 dB、20 Hz/20 kHz 端点、
//!   f32 系数量化后的幅度/相位/群延迟偏差复算（§1.6）。
//! - 报告必须包含句：**仅自研可算窗/目标，不复刻任何专有 DAC**（`NO_PROPRIETARY_DAC_STATEMENT`，
//!   由 `ApodizingVerification::family` 携带）。
//!
//! 明确非目标（不得由本模块冒充）：
//! - 不生成 WAV/manifest（`khaen-fir/v1` 产物与 20/20k Endpoint 落盘归 T55）；不做 target 文件导入（T11）；
//!   不做 P/G 融合/运行准入（T12/T56）；不做 RT/板端（本文件零 RT 路径、零板端动作）。
//! - 不复刻任何专有 DAC 的 apodizing 重建滤波器；本模块只给“可算窗/可算目标”的可复现构造。
//! - 不把 apodizing 说成砖墙：0.45fs 处 ≤ −60 dB 的构造按 §3.4 判据 7/§3.8 必须改报 brickwall/linear。
//!
//! 精度/确定性：设计 f64；同一输入同一版本两次 design 的系数逐位一致（无随机数/时间戳）。

use std::f64::consts::PI;
use std::fmt;

// ---------------------------------------------------------------------------
// 常量（规范 §3.4 / §1.2–§1.6 / §2.5）
// ---------------------------------------------------------------------------

/// 规范 §3.1 判据 8 的公共下限：最小 taps（apodizing 沿用公共支撑约束）。
pub const MIN_TAPS: usize = 8;
/// 规范 §2.1/§7.2：设计 taps 上限（>16384 只能由其它离线工具处理）。
pub const MAX_TAPS: usize = 16_384;
/// 规范 §1.2：幅度/相位测量网格下界（Hz）。
pub const MEASUREMENT_LO_HZ: f64 = 20.0;
/// 规范 §1.2：测量网格上界之一（Hz）。
pub const MEASUREMENT_HI_HZ: f64 = 20_000.0;
/// 规范 §1.2：Nyquist 护栏比例 0.499·fs（测量网格上界）。
pub const NYQUIST_GUARD_FRACTION: f64 = 0.499;
/// 规范 §3.4：`f_p <= 0.85*fs/2` 的家族边界比例。
pub const FP_MAX_FRACTION_OF_NYQUIST: f64 = 0.85;
/// 规范 §3.4：默认通带纹波（dB）。
pub const DEFAULT_RIPPLE_DB: f64 = 0.5;
/// 规范 §3.4 判据 2：通带纹波允许的附加余量（dB）。
pub const PASSBAND_RIPPLE_MARGIN_DB: f64 = 0.05;
/// 规范 §3.4：默认 Nyquist 带衰减要求（dB，>=30）。
pub const DEFAULT_NYQUIST_ATTENUATION_DB: f64 = 30.0;
/// 规范 §3.4 判据 3：Nyquist 带判定余量（dB）。
pub const NYQUIST_BAND_MARGIN_DB: f64 = 1.0;
/// 规范 §3.4 判据 3 的 Nyquist 带下界比例（0.49·fs）。
pub const NYQUIST_BAND_LO_FRACTION: f64 = 0.49;
/// 规范 §3.4 参考构造默认 Kaiser beta（与 T04 V16 一致）。
pub const DEFAULT_KAISER_BETA: f64 = 8.96;
/// 规范 §3.4 参考构造目标下限（dB）：floor = 10^(−120/20)。
pub const MAG_FLOOR_DB: f64 = -120.0;
/// 规范 §3.4 参考构造目标下限幅度：`10^(−120/20) = 1e-06`。
pub const MAG_FLOOR_AMPLITUDE: f64 = 1e-6;
/// 规范 §3.4 参考构造的对称合成点数 N（T04 V16 固定值）。
pub const SYNTHESIS_POINTS: usize = 32_768;
/// 合成点数工程上限（防误用；>16384 taps 的离线工具另有准入）。
pub const MAX_SYNTHESIS_POINTS: usize = 1 << 20;
/// 规范 §3.4 判据 4 默认前振铃上限（dB）。
pub const PRE_RING_BOUND_DB: f64 = -45.0;
/// 规范 §3.4 判据 4：前振铃**能量**上限（dB；固定值，不做参数放宽）。
pub const PRE_RING_ENERGY_BOUND_DB: f64 = -45.0;
/// 规范 §3.4 判据 7 / §3.8：砖墙探测频点比例（0.45·fs）。
pub const BRICKWALL_PROBE_FRACTION: f64 = 0.45;
/// 规范 §3.4 判据 7 / §3.8：0.45fs 处 ≤ −60 dB 即“砖墙式”，必须改报 brickwall/linear。
pub const BRICKWALL_PROBE_DB: f64 = -60.0;
/// 规范 §3.4 判据 6：过渡区（f_p..0.499fs）允许的最大上升（dB）。
pub const TRANSITION_RISE_TOLERANCE_DB: f64 = 0.5;
/// 规范 §1.4：振铃排除窗比例（W=ceil(0.000333·fs)，48k=16、96k=32）。
pub const RING_EXCLUSION_FRACTION: f64 = 0.000333;
/// 规范 §1.2：|H| 截断下限（−240 dB），避免 log0。
pub const MAGNITUDE_FLOOR: f64 = 1e-12;
/// 规范 §3.1 判据 1（apodizing §3.4 判据 1 引用）：对称性容差（相对 max|h|）。
pub const SYMMETRY_TOLERANCE: f64 = 1e-7;
/// 规范 §3.1 判据 2（apodizing §3.4 判据 1 引用）：常数群延迟容差（samples）。
pub const GROUP_DELAY_TOLERANCE_SAMPLES: f64 = 0.01;
/// 规范 §1.5/§3.4 判据 6：参考归一化容差（dB）。
pub const REFERENCE_TOLERANCE_DB: f64 = 0.01;
/// 相位线性度（相对 −2πfD/fs 的 wrap 偏差）工程容差（度）。
pub const LINEAR_PHASE_DEVIATION_TOLERANCE_DEG: f64 = 0.1;
/// 规范 §1.6：f32 系数量化后幅度偏差工程容差（dB；报告口径，非 spec 判据）。
pub const F32_MAGNITUDE_DEVIATION_TOLERANCE_DB: f64 = 0.01;
/// 规范 §1.6：f32 系数量化后相位偏差工程容差（度；报告口径）。
pub const F32_PHASE_DEVIATION_TOLERANCE_DEG: f64 = 0.1;
/// 规范 §1.6：f32 系数量化后群延迟偏差工程容差（samples；报告口径）。
pub const F32_GROUP_DELAY_DEVIATION_TOLERANCE_SAMPLES: f64 = 0.01;
/// 规范 §1.6：f32 系数相对 L2 误差工程容差（报告口径）。
pub const F32_RELATIVE_L2_TOLERANCE: f64 = 1e-6;
/// 设计模块工程采样率范围下限（Hz；典型 48k/96k，8k 为语音域下限）。
pub const FS_ENGINEERING_MIN_HZ: f64 = 8_000.0;
/// 设计模块工程采样率范围上限（Hz）。
pub const FS_ENGINEERING_MAX_HZ: f64 = 384_000.0;
/// 规范 §3.4 判据 7：报告必须含的“不复刻专有 DAC”声明。
pub const NO_PROPRIETARY_DAC_STATEMENT: &str = "仅自研可算窗/目标，不复刻任何专有 DAC";

// ---------------------------------------------------------------------------
// 参数 / 错误
// ---------------------------------------------------------------------------

/// apodizing 窗类型（规范 §3.4 `apodizing_window.type`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApodizingWindowKind {
    /// Kaiser 窗，beta 默认 `DEFAULT_KAISER_BETA`（8.96，与 T04 V16 一致）。
    Kaiser,
    /// Hann² 窗：`w[n] = (0.5 − 0.5·cos(2πn/(L−1)))²`（对称、两端为 0）。
    HannSquared,
}

impl ApodizingWindowKind {
    /// 规范 §3.4 的窗类型字符串（manifest/schema 用）。
    pub fn schema_name(self) -> &'static str {
        match self {
            ApodizingWindowKind::Kaiser => "kaiser",
            ApodizingWindowKind::HannSquared => "hann_squared",
        }
    }

    /// 解析规范 §3.4 的窗类型字符串；未知类型返回 `None`（调用方应报 E_FIR_BAND）。
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "kaiser" => Some(ApodizingWindowKind::Kaiser),
            "hann_squared" => Some(ApodizingWindowKind::HannSquared),
            _ => None,
        }
    }
}

/// apodizing 设计参数（规范 §3.4 参数表；字段可直改，构造器给默认值）。
#[derive(Clone, Debug, PartialEq)]
pub struct ApodizingSpec {
    /// 运行采样率（Hz）。
    pub fs: f64,
    /// 支撑长度 L（taps），`[MIN_TAPS, MAX_TAPS]`。
    pub taps: usize,
    /// 通带 `[20, f_p]`（Hz）；`f_p <= 0.85*fs/2`。
    pub passband_hz: [f64; 2],
    /// 通带允许纹波（max−min，dB），判据为 <= ripple_db + 0.05。
    pub ripple_db: f64,
    /// Nyquist 带衰减要求（dB，>=30）：[0.49fs,0.499fs] max|H|dB <= −(该值−1)。
    pub nyquist_attenuation_db: f64,
    /// apodizing 窗类型。
    pub window: ApodizingWindowKind,
    /// Kaiser beta；`None` 时取默认 8.96；Hann² 不允许 beta。
    pub kaiser_beta: Option<f64>,
    /// 前振铃上限（dB，默认 −45；只允许更严，不得放宽）。
    pub pre_ring_bound_db: f64,
}

impl ApodizingSpec {
    /// 构造参数：`fs`、`taps`、通带上界 `f_p`；其余取 §3.4 默认值
    /// （ripple 0.5 dB、nyquist_attenuation 30 dB、Kaiser 窗 beta 8.96、pre_ring_bound −45 dB）。
    pub fn new(fs: f64, taps: usize, passband_hi_hz: f64) -> Self {
        Self {
            fs,
            taps,
            passband_hz: [MEASUREMENT_LO_HZ, passband_hi_hz],
            ripple_db: DEFAULT_RIPPLE_DB,
            nyquist_attenuation_db: DEFAULT_NYQUIST_ATTENUATION_DB,
            window: ApodizingWindowKind::Kaiser,
            kaiser_beta: None,
            pre_ring_bound_db: PRE_RING_BOUND_DB,
        }
    }

    /// §3.4 参考构造预设（48k/1024 taps/f_p=18 kHz/Kaiser beta 8.96），与 T04 V16 完全一致。
    pub fn reference_preset_48k() -> Self {
        Self::new(48_000.0, 1_024, 18_000.0)
    }

    /// 通用参考预设：`f_p = 0.375·fs = 0.75·fs/2`（48k 时即 18 kHz，保持 V16 相对位置）。
    pub fn reference_preset(fs: f64, taps: usize) -> Self {
        Self::new(fs, taps, 0.375 * fs)
    }

    /// 设置通带纹波（dB）。
    pub fn with_ripple(mut self, ripple_db: f64) -> Self {
        self.ripple_db = ripple_db;
        self
    }

    /// 设置 Nyquist 带衰减要求（dB）。
    pub fn with_nyquist_attenuation(mut self, nyquist_attenuation_db: f64) -> Self {
        self.nyquist_attenuation_db = nyquist_attenuation_db;
        self
    }

    /// 设置 apodizing 窗类型。
    pub fn with_window(mut self, window: ApodizingWindowKind) -> Self {
        self.window = window;
        self
    }

    /// 显式设置 Kaiser beta（仅 Kaiser 窗；Hann² 不允许）。
    pub fn with_kaiser_beta(mut self, beta: f64) -> Self {
        self.kaiser_beta = Some(beta);
        self
    }

    /// 设置前振铃上限（dB；只允许 ≤ −45，更严可以）。
    pub fn with_pre_ring_bound(mut self, pre_ring_bound_db: f64) -> Self {
        self.pre_ring_bound_db = pre_ring_bound_db;
        self
    }

    /// 通带上界 f_p（Hz）。
    pub fn passband_edge_hz(&self) -> f64 {
        self.passband_hz[1]
    }

    /// Nyquist 频率 fs/2（Hz）。
    pub fn nyquist_hz(&self) -> f64 {
        0.5 * self.fs
    }

    /// §3.4 家族边界 `0.85*fs/2`（Hz）。
    pub fn fp_limit_hz(&self) -> f64 {
        FP_MAX_FRACTION_OF_NYQUIST * self.nyquist_hz()
    }

    /// 幅度目标（magnitude-only；fs/2 处为 floor，符合 Lead fs/2 裁决）。
    pub fn target(&self) -> ApodizingTarget {
        ApodizingTarget::new(self.fs, self.passband_edge_hz())
    }
}

/// apodizing 失败码承载的错误（规范 §2.5；不发明规范外成功路径）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApodizingError {
    /// E_FIR_BAND：参数/频带非法（fs/taps/纹波/衰减/窗/beta/pre_ring 放宽等）。
    Band(String),
    /// E_FIR_TAPS：系数长度与声明 taps 不一致。
    Taps(String),
    /// E_FIR_NUMERIC：系数含 NaN/Inf 或 max|h|=0，或归一化失败。
    Numeric(String),
    /// E_FIR_REF：参考归一化不满足 ±0.01 dB。
    Reference(String),
    /// E_FIR_PHASE_CLASS：线性相位（对称/常数 GD）不满足。
    PhaseClass(String),
    /// E_FIR_FAMILY：家族自洽/互斥失败（前振铃/能量、砖墙式、过渡非单调）。
    Family(String),
    /// E_FIR_BUDGET：可行/支撑不足（Nyquist 带、通带纹波、合成点数等；不静默截短）。
    Budget(String),
    /// E_FIR_APO_FP：`f_p > 0.85*fs/2`。
    ApoFp(String),
}

impl ApodizingError {
    /// 规范 §2.5 的失败码字符串。
    pub fn code(&self) -> &'static str {
        match self {
            ApodizingError::Band(_) => "E_FIR_BAND",
            ApodizingError::Taps(_) => "E_FIR_TAPS",
            ApodizingError::Numeric(_) => "E_FIR_NUMERIC",
            ApodizingError::Reference(_) => "E_FIR_REF",
            ApodizingError::PhaseClass(_) => "E_FIR_PHASE_CLASS",
            ApodizingError::Family(_) => "E_FIR_FAMILY",
            ApodizingError::Budget(_) => "E_FIR_BUDGET",
            ApodizingError::ApoFp(_) => "E_FIR_APO_FP",
        }
    }

    /// 人类可读细节。
    pub fn detail(&self) -> &str {
        match self {
            ApodizingError::Band(s)
            | ApodizingError::Taps(s)
            | ApodizingError::Numeric(s)
            | ApodizingError::Reference(s)
            | ApodizingError::PhaseClass(s)
            | ApodizingError::Family(s)
            | ApodizingError::Budget(s)
            | ApodizingError::ApoFp(s) => s,
        }
    }
}

impl fmt::Display for ApodizingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.detail())
    }
}

impl std::error::Error for ApodizingError {}

// ---------------------------------------------------------------------------
// 可算目标
// ---------------------------------------------------------------------------

/// apodizing 幅度目标（magnitude-only，规范 §3.4 参考构造）：
/// `|H|=1` for `f <= f_p`；`f_p..fs/2` 升余弦 1→floor（−120 dB）；`f=fs/2` 取 floor。
///
/// **fs/2 说明（Lead 2026-11-17 裁决）**：本目标没有 phase 列，属 magnitude-only，允许包含
/// `freq = fs/2`（合成时 Nyquist bin 取 floor）；带 phase 的目标点等于 fs/2 才必须拒收（T11 负责）。
/// 相位/群延迟报告只在 `f < fs/2` 的测量网格上做（Nyquist 处相位退化）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ApodizingTarget {
    /// 采样率（Hz）。
    pub fs: f64,
    /// 通带上界 f_p（Hz）。
    pub passband_edge_hz: f64,
    /// 目标下限幅度 `10^(−120/20)=1e-6`。
    pub floor_amplitude: f64,
}

impl ApodizingTarget {
    /// 由 fs 与 f_p 构造 §3.4 参考目标（floor 固定 −120 dB）。
    pub fn new(fs: f64, passband_edge_hz: f64) -> Self {
        Self {
            fs,
            passband_edge_hz,
            floor_amplitude: MAG_FLOOR_AMPLITUDE,
        }
    }

    /// 由完整参数构造目标。
    pub fn from_spec(spec: &ApodizingSpec) -> Self {
        Self::new(spec.fs, spec.passband_edge_hz())
    }

    /// Nyquist 频率（Hz）。
    pub fn nyquist_hz(&self) -> f64 {
        0.5 * self.fs
    }

    /// 目标幅度 `|T(f)|`（对 f 取绝对值；非法输入 NaN）。
    ///
    /// 与 T04 V16 `magRaisedCosine` 逐点同式：`f<=f_p → 1`；`f>=fs/2 → floor`；
    /// 之间 `max(floor, 0.5*(1+cos(π(f−f_p)/(fs/2−f_p))))`。
    pub fn amplitude_at(&self, f_hz: f64) -> f64 {
        if !f_hz.is_finite() || !self.fs.is_finite() || self.fs <= 0.0 {
            return f64::NAN;
        }
        let af = f_hz.abs();
        let nyq = self.nyquist_hz();
        if af <= self.passband_edge_hz {
            1.0
        } else if af >= nyq {
            self.floor_amplitude
        } else {
            let frac = (af - self.passband_edge_hz) / (nyq - self.passband_edge_hz);
            let value = 0.5 * (1.0 + (PI * frac).cos());
            value.max(self.floor_amplitude)
        }
    }

    /// 目标幅度（dB，截到 −240 dB 下限）。
    pub fn magnitude_db_at(&self, f_hz: f64) -> f64 {
        20.0 * self.amplitude_at(f_hz).max(MAGNITUDE_FLOOR).log10()
    }

    /// 目标合法性（供 `synthesize_apodizing` 防御；正常路径已由 `validate` 保证）。
    fn validate(&self) -> Result<(), ApodizingError> {
        if !self.fs.is_finite() || self.fs <= 0.0 {
            return Err(ApodizingError::Band(format!(
                "目标 fs 必须有限且 >0，实际 {}",
                self.fs
            )));
        }
        if !self.passband_edge_hz.is_finite()
            || self.passband_edge_hz <= 0.0
            || self.passband_edge_hz >= self.nyquist_hz()
        {
            return Err(ApodizingError::Band(format!(
                "目标 f_p 必须满足 0 < f_p < fs/2={}，实际 {}",
                self.nyquist_hz(),
                self.passband_edge_hz
            )));
        }
        if !self.floor_amplitude.is_finite()
            || self.floor_amplitude <= 0.0
            || self.floor_amplitude > 1.0
        {
            return Err(ApodizingError::Band(format!(
                "目标 floor_amplitude 必须在 (0,1]，实际 {}",
                self.floor_amplitude
            )));
        }
        Ok(())
    }
}

/// 通过 `validate` 的完整参数（派生量集中在这里，构造与验证共用同一口径）。
#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedApodizingSpec {
    /// 原参数（克隆）。
    pub spec: ApodizingSpec,
    /// fs/2（Hz）。
    pub nyquist_hz: f64,
    /// 家族边界 0.85·fs/2（Hz）。
    pub fp_limit_hz: f64,
    /// 体延迟 D=(L−1)/2（samples）。
    pub delay_samples: f64,
    /// delay_ms = D/fs·1000。
    pub delay_ms: f64,
    /// 实际 Kaiser beta（Kaiser 窗时 Some）。
    pub kaiser_beta: Option<f64>,
    /// 可算幅度目标。
    pub target: ApodizingTarget,
}

/// 校验 apodizing 参数（§3.4 参数约束 + §3.8 家族边界），返回派生量。
pub fn validate(spec: &ApodizingSpec) -> Result<ValidatedApodizingSpec, ApodizingError> {
    use ApodizingError as E;

    if !spec.fs.is_finite() || spec.fs < FS_ENGINEERING_MIN_HZ || spec.fs > FS_ENGINEERING_MAX_HZ {
        return Err(E::Band(format!(
            "fs 必须在工程范围 [{FS_ENGINEERING_MIN_HZ}, {FS_ENGINEERING_MAX_HZ}] Hz（典型 48k/96k），实际 {}",
            spec.fs
        )));
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
    if !spec.nyquist_attenuation_db.is_finite() || spec.nyquist_attenuation_db < 30.0 {
        return Err(E::Band(format!(
            "nyquist_attenuation_db 必须有限且 >=30，实际 {}",
            spec.nyquist_attenuation_db
        )));
    }
    if !spec.passband_hz[0].is_finite() || !spec.passband_hz[1].is_finite() {
        return Err(E::Band("passband_hz 含非有限值".to_string()));
    }
    if (spec.passband_hz[0] - MEASUREMENT_LO_HZ).abs() > 1e-9 {
        return Err(E::Band(format!(
            "passband_hz[0] 必须=20 Hz（测量网格下界），实际 {}",
            spec.passband_hz[0]
        )));
    }
    let fp = spec.passband_hz[1];
    let nyquist_hz = 0.5 * spec.fs;
    let fp_limit_hz = FP_MAX_FRACTION_OF_NYQUIST * nyquist_hz;
    if !(fp > MEASUREMENT_LO_HZ) {
        return Err(E::Band(format!("通带上界 f_p 必须 > 20 Hz，实际 {fp}")));
    }
    if fp > fp_limit_hz + 1e-9 {
        return Err(E::ApoFp(format!(
            "apodizing 要求 f_p <= 0.85*fs/2={fp_limit_hz} Hz，实际 f_p={fp} Hz（§3.4 判据 7）"
        )));
    }
    if !spec.pre_ring_bound_db.is_finite() {
        return Err(E::Band(format!(
            "pre_ring_bound_db 必须有限，实际 {}",
            spec.pre_ring_bound_db
        )));
    }
    if spec.pre_ring_bound_db > PRE_RING_BOUND_DB + 1e-12 {
        return Err(E::Band(format!(
            "pre_ring_bound_db={} dB 放宽了 §3.4 判据 4 的工程默认 {PRE_RING_BOUND_DB} dB；不得静默放宽（只允许更严）",
            spec.pre_ring_bound_db
        )));
    }

    let kaiser_beta = match spec.window {
        ApodizingWindowKind::Kaiser => {
            let beta = spec.kaiser_beta.unwrap_or(DEFAULT_KAISER_BETA);
            if !beta.is_finite() || beta < 0.0 {
                return Err(E::Band(format!("Kaiser beta 必须有限且 >=0，实际 {beta}")));
            }
            Some(beta)
        }
        ApodizingWindowKind::HannSquared => {
            if spec.kaiser_beta.is_some() {
                return Err(E::Band(
                    "hann_squared 窗不接受 beta（beta 仅 Kaiser）".to_string(),
                ));
            }
            None
        }
    };

    let delay_samples = (spec.taps - 1) as f64 / 2.0;
    let delay_ms = delay_samples / spec.fs * 1000.0;
    let target = ApodizingTarget::new(spec.fs, fp);
    Ok(ValidatedApodizingSpec {
        spec: spec.clone(),
        nyquist_hz,
        fp_limit_hz,
        delay_samples,
        delay_ms,
        kaiser_beta,
        target,
    })
}

// ---------------------------------------------------------------------------
// 可算窗 / 对称合成
// ---------------------------------------------------------------------------

/// 归一化 sinc：sin(πx)/(πx)，sinc(0)=1。
pub fn sinc_normalized(x: f64) -> f64 {
    if x == 0.0 {
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

/// Kaiser 窗（长度 taps，居中、精确镜像对称）；taps=1 返回 [1]。
pub fn kaiser_window_values(taps: usize, beta: f64) -> Vec<f64> {
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

/// Hann² 窗（长度 taps，居中、精确镜像对称）：`w[n] = (0.5 − 0.5·cos(2πn/(L−1)))²`。
pub fn hann_squared_window(taps: usize) -> Vec<f64> {
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
        let hann = 0.5 - 0.5 * x.cos();
        let val = hann * hann;
        w[i] = val;
        w[taps - 1 - i] = val;
    }
    w
}

/// 按 §3.4 参数取 apodizing 窗值（长度 taps，镜像对称）。
pub fn apodizing_window_values(
    kind: ApodizingWindowKind,
    taps: usize,
    kaiser_beta: Option<f64>,
) -> Result<Vec<f64>, ApodizingError> {
    if taps == 0 || taps > MAX_TAPS {
        return Err(ApodizingError::Taps(format!(
            "窗长度 taps 必须在 [1, {MAX_TAPS}]，实际 {taps}"
        )));
    }
    match kind {
        ApodizingWindowKind::Kaiser => {
            let beta = kaiser_beta.unwrap_or(DEFAULT_KAISER_BETA);
            if !beta.is_finite() || beta < 0.0 {
                return Err(ApodizingError::Band(format!(
                    "Kaiser beta 必须有限且 >=0，实际 {beta}"
                )));
            }
            Ok(kaiser_window_values(taps, beta))
        }
        ApodizingWindowKind::HannSquared => {
            if kaiser_beta.is_some() {
                return Err(ApodizingError::Band(
                    "hann_squared 窗不接受 beta（beta 仅 Kaiser）".to_string(),
                ));
            }
            Ok(hann_squared_window(taps))
        }
    }
}

/// 原地基 2 复数 FFT（长度须为 2 的幂；`inverse=true` 为 IFFT，含 1/N）。
///
/// 只用于 §3.4 对称合成：把 N 点幅度谱 + 线性相位 D 变成 `h[n] = (1/N)Σ M_k e^(j2πk(n−D)/N)`，
/// 与规范参考构造的直接求和式数学等价（测试 `t35_synthesis_matches_direct_formula` 对小 N 逐点对照）。
fn fft_in_place(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    if n <= 1 || n != im.len() || !n.is_power_of_two() {
        return;
    }
    // 位反转置换
    let mut j = 0_usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    // 蝶形
    let mut len = 2_usize;
    while len <= n {
        let ang = if inverse {
            2.0 * PI / len as f64
        } else {
            -2.0 * PI / len as f64
        };
        let wlen_re = ang.cos();
        let wlen_im = ang.sin();
        let half = len / 2;
        let mut base = 0_usize;
        while base < n {
            let mut w_re = 1.0_f64;
            let mut w_im = 0.0_f64;
            for k in 0..half {
                let a = base + k;
                let b = base + k + half;
                let v_re = re[b] * w_re - im[b] * w_im;
                let v_im = re[b] * w_im + im[b] * w_re;
                let u_re = re[a];
                let u_im = im[a];
                re[a] = u_re + v_re;
                im[a] = u_im + v_im;
                re[b] = u_re - v_re;
                im[b] = u_im - v_im;
                let next_re = w_re * wlen_re - w_im * wlen_im;
                w_im = w_re * wlen_im + w_im * wlen_re;
                w_re = next_re;
            }
            base += len;
        }
        len <<= 1;
    }
    if inverse {
        let scale = 1.0 / n as f64;
        for v in re.iter_mut() {
            *v *= scale;
        }
        for v in im.iter_mut() {
            *v *= scale;
        }
    }
}

/// §3.4 参考构造的“对称合成 + 可算窗 + DC 归一”（纯幅度目标，线性相位）。
///
/// 步骤（与 T04 `recompute-vectors.mjs` 的 `synthSymmetric` 同式）：
/// 1. 在 N 点网格上取幅度目标 `M_k = |T(k·fs/N)|`（Nyquist bin 取 floor，符合 fs/2 裁决）；
/// 2. 乘线性相位 `e^(−j2πkD/N)`（D=(L−1)/2，可为半整数）做 N 点 IFFT，取前 L 点为对称冲激响应；
/// 3. 逐点乘 apodizing 窗（长度必须 = L）；
/// 4. 以 Σh 归一（DC 增益 1，等价 §1.5 reference hz=0/gain=0 dB）。
///
/// `synthesis_points` 必须为 2 的幂、>= 1024 且 >= 2·taps（不足报 `E_FIR_BUDGET`，不静默降质）。
pub fn synthesize_apodizing(
    target: &ApodizingTarget,
    taps: usize,
    window: &[f64],
    synthesis_points: usize,
) -> Result<Vec<f64>, ApodizingError> {
    target.validate()?;
    if taps == 0 || taps > MAX_TAPS {
        return Err(ApodizingError::Taps(format!(
            "taps 必须在 [1, {MAX_TAPS}]，实际 {taps}"
        )));
    }
    if window.len() != taps {
        return Err(ApodizingError::Taps(format!(
            "apodizing 窗长度 {} != taps {}",
            window.len(),
            taps
        )));
    }
    if synthesis_points < 1_024
        || !synthesis_points.is_power_of_two()
        || synthesis_points < 2 * taps
        || synthesis_points > MAX_SYNTHESIS_POINTS
    {
        return Err(ApodizingError::Budget(format!(
            "synthesis_points={synthesis_points} 必须为 2 的幂且满足 1024 <= N <= {MAX_SYNTHESIS_POINTS}、N >= 2·taps={}（§3.4 参考构造 N=32768）",
            2 * taps
        )));
    }

    let n = synthesis_points;
    let fs = target.fs;
    let delay = (taps - 1) as f64 / 2.0;
    let mut re = vec![0.0_f64; n];
    let mut im = vec![0.0_f64; n];
    for k in 0..=n / 2 {
        let freq = k as f64 * fs / n as f64;
        let mag = target.amplitude_at(freq);
        let phase = -2.0 * PI * (k as f64) * delay / (n as f64);
        let (sin_p, cos_p) = phase.sin_cos();
        re[k] = mag * cos_p;
        im[k] = mag * sin_p;
        if k != 0 && k != n / 2 {
            let mirror = n - k;
            re[mirror] = mag * cos_p;
            im[mirror] = -mag * sin_p;
        }
    }
    fft_in_place(&mut re, &mut im, true);
    let mut coeffs: Vec<f64> = re[..taps].to_vec();
    for (h, w) in coeffs.iter_mut().zip(window.iter()) {
        *h *= *w;
    }
    let sum: f64 = coeffs.iter().sum();
    if !sum.is_finite() || sum.abs() <= 0.0 {
        return Err(ApodizingError::Numeric(format!(
            "DC 归一失败：Σh={sum}（应≈1）"
        )));
    }
    let scale = 1.0 / sum;
    for h in coeffs.iter_mut() {
        *h *= scale;
    }
    if coeffs.iter().any(|h| !h.is_finite()) {
        return Err(ApodizingError::Numeric("合成系数含 NaN/Inf".to_string()));
    }
    Ok(coeffs)
}

// ---------------------------------------------------------------------------
// 设计产物
// ---------------------------------------------------------------------------

/// apodizing 设计产物（`khaen-fir/v1` manifest 的系数源；WAV/manifest 归 T55）。
#[derive(Clone, Debug, PartialEq)]
pub struct ApodizingDesign {
    /// 原参数。
    pub spec: ApodizingSpec,
    /// f64 冲激响应（len=taps，因果、对称）。
    pub coeffs: Vec<f64>,
    /// 实际使用的幅度目标。
    pub target: ApodizingTarget,
    /// 实际使用的 apodizing 窗。
    pub window: ApodizingWindowKind,
    /// 实际使用的 Kaiser beta（Hann² 时为 None）。
    pub kaiser_beta: Option<f64>,
    /// 对称合成点数 N。
    pub synthesis_points: usize,
    /// D=(L−1)/2（samples）。
    pub delay_samples: f64,
    /// D/fs·1000（ms）。
    pub delay_ms: f64,
    /// 归一化参考频点（本家族为 DC=0 Hz）。
    pub reference_hz: f64,
    /// 声明的参考增益（dB，本模块固定 0 dB）。
    pub reference_gain_db: f64,
    /// max|h|。
    pub peak_abs: f64,
    /// Σh（DC 归一后≈1）。
    pub sum_coeffs: f64,
}

impl ApodizingDesign {
    /// taps = len(h)。
    pub fn taps(&self) -> usize {
        self.coeffs.len()
    }

    /// f32 导出（WAV/运行器用；量化误差由验证报告复算，§1.6）。
    pub fn coeffs_f32(&self) -> Vec<f32> {
        self.coeffs.iter().map(|&h| h as f32).collect()
    }

    /// 有效支撑 (first, last, effective_taps)，eps=1e-6（规范 §1.3）。
    pub fn effective_support(&self) -> (usize, usize, usize) {
        effective_support(&self.coeffs, 1e-6)
    }
}

/// 设计选项（合成网格；默认按 §3.4 N=32768）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DesignOptions {
    /// 对称合成点数 N（2 的幂，>=max(1024, 2·taps)）。
    pub synthesis_points: usize,
}

impl DesignOptions {
    /// 规范 §3.4 参考构造：N=32768。
    pub fn normative() -> Self {
        Self {
            synthesis_points: SYNTHESIS_POINTS,
        }
    }

    /// 显式指定合成点数（测试/快速复算用；正式产物用 normative）。
    pub fn with_synthesis_points(synthesis_points: usize) -> Self {
        Self { synthesis_points }
    }
}

/// 构造 apodizing 系数（只构造 + 结构自检；全网格判定用 `design_checked`）。
///
/// 失败码：E_FIR_BAND/E_FIR_TAPS/E_FIR_NUMERIC/E_FIR_REF/E_FIR_PHASE_CLASS/E_FIR_BUDGET/E_FIR_APO_FP。
pub fn design(spec: &ApodizingSpec) -> Result<ApodizingDesign, ApodizingError> {
    design_with_options(spec, &DesignOptions::normative())
}

/// 构造 apodizing 系数（自定义合成点数）。
pub fn design_with_options(
    spec: &ApodizingSpec,
    options: &DesignOptions,
) -> Result<ApodizingDesign, ApodizingError> {
    let validated = validate(spec)?;
    let window = apodizing_window_values(spec.window, spec.taps, validated.kaiser_beta)?;
    let coeffs = synthesize_apodizing(
        &validated.target,
        spec.taps,
        &window,
        options.synthesis_points,
    )?;

    // 结构自检（不静默返回看起来成功的坏设计）。
    let mut peak_abs = 0.0_f64;
    for &h in &coeffs {
        if !h.is_finite() {
            return Err(ApodizingError::Numeric("系数含 NaN/Inf".to_string()));
        }
        peak_abs = peak_abs.max(h.abs());
    }
    if peak_abs <= 0.0 || !peak_abs.is_finite() {
        return Err(ApodizingError::Numeric("max|h|=0 或非有限".to_string()));
    }
    let symmetry_err = symmetry_error(&coeffs);
    if !(symmetry_err <= 1e-9 * peak_abs) {
        return Err(ApodizingError::PhaseClass(format!(
            "apodizing 对称性构造失败：max|h−flip(h)|={symmetry_err} > 1e-9·max|h|"
        )));
    }
    let reference = response_at(&coeffs, spec.fs, 0.0).magnitude_db();
    if !reference.is_finite() || reference.abs() > 1e-6 {
        return Err(ApodizingError::Reference(format!(
            "参考归一化失败：|H(0)|={reference} dB（目标 0 dB）"
        )));
    }
    let sum_coeffs: f64 = coeffs.iter().sum();
    Ok(ApodizingDesign {
        spec: spec.clone(),
        coeffs,
        target: validated.target,
        window: spec.window,
        kaiser_beta: validated.kaiser_beta,
        synthesis_points: options.synthesis_points,
        delay_samples: validated.delay_samples,
        delay_ms: validated.delay_ms,
        reference_hz: 0.0,
        reference_gain_db: 0.0,
        peak_abs,
        sum_coeffs,
    })
}

// ---------------------------------------------------------------------------
// 独立响应测量（§1.2 / §1.3 / §1.4）
// ---------------------------------------------------------------------------

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

/// 直接 DTFT 求 H(f)（O(L)；复指数递推 + 每 64 点重锚，独立于窗/合成构造）。
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

/// 解析群延迟 GD(f)=Re(B/H)，B=Σ n·h[n]e^(−jωn)（§1.2 允许中心差分；解析式更准）。
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

/// 对称性误差 max|h[n] − h[L−1−n]|（规范 §3.4 判据 1 → §3.1 判据 1）。
pub fn symmetry_error(coeffs: &[f64]) -> f64 {
    if coeffs.is_empty() {
        return f64::NAN;
    }
    let l = coeffs.len();
    let mut max_err = 0.0_f64;
    for n in 0..l {
        max_err = max_err.max((coeffs[n] - coeffs[l - 1 - n]).abs());
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

/// 前/后振铃测量结果（规范 §1.4 统一口径）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RingMetrics {
    /// 排除窗 W=ceil(0.000333·fs)（samples）。
    pub exclusion_samples: usize,
    /// argmax_n |h[n]|（首个最大峰；偶长对称设计有两个相等中心峰）。
    pub peak_index: usize,
    /// 主峰中心 (L−1)/2（偶长时半整数；排除窗以它为中心，保持两侧对称）。
    pub center_samples: f64,
    /// pre_ring_db = 20log10(max_{n<c−W}|h[n]|/max|h|)；None=n/a（支撑过短）。
    pub pre_db: Option<f64>,
    /// post_ring_db 同上（n>c+W）。
    pub post_db: Option<f64>,
    /// pre_ring_energy_db = 10log10(Σ_{n<c−W}h²/Σh²)；None=n/a。
    pub pre_energy_db: Option<f64>,
    /// post_ring_energy_db；None=n/a。
    pub post_energy_db: Option<f64>,
}

impl RingMetrics {
    /// pre 侧是否可测（规范 §1.4：p−W<0 写 n/a）。
    pub fn pre_applicable(&self) -> bool {
        self.pre_db.is_some()
    }

    /// post 侧是否可测（规范 §1.4：p+W>=L 写 n/a）。
    pub fn post_applicable(&self) -> bool {
        self.post_db.is_some()
    }
}

/// 前/后振铃测量（§1.4：排除窗 W=ceil(0.000333·fs) samples，零填充线性卷积系数上测）。
///
/// 对偶长对称 FIR，argmax|h| 有两个相等中心峰；为对齐 §1.4「主峰两侧」的对称排除意图
/// 与 T04 V16 参考构造（`ringMetrics` 用 c=(L−1)/2），本函数以中心 c=(L−1)/2 计算排除窗，
/// 并单列 `peak_index`（首个 argmax）供审计。若 c−W<0 / c+W>=L 则对应项写 None（n/a）。
pub fn ring_metrics(coeffs: &[f64], fs: f64) -> RingMetrics {
    let l = coeffs.len();
    let exclusion_samples = if fs.is_finite() && fs > 0.0 {
        (RING_EXCLUSION_FRACTION * fs).ceil() as usize
    } else {
        0
    };
    if l == 0 {
        return RingMetrics {
            exclusion_samples,
            peak_index: 0,
            center_samples: f64::NAN,
            pre_db: None,
            post_db: None,
            pre_energy_db: None,
            post_energy_db: None,
        };
    }
    let center = (l - 1) as f64 / 2.0;
    let mut peak = 0.0_f64;
    let mut peak_index = 0_usize;
    for (n, &h) in coeffs.iter().enumerate() {
        let a = h.abs();
        if a > peak {
            peak = a;
            peak_index = n;
        }
    }
    if !peak.is_finite() || peak <= 0.0 {
        return RingMetrics {
            exclusion_samples,
            peak_index,
            center_samples: center,
            pre_db: None,
            post_db: None,
            pre_energy_db: None,
            post_energy_db: None,
        };
    }
    let total_energy: f64 = coeffs.iter().map(|h| h * h).sum();
    let pre_applicable = center - exclusion_samples as f64 >= 0.0;
    let post_applicable = center + (exclusion_samples as f64) < l as f64;
    let mut pre_max = 0.0_f64;
    let mut post_max = 0.0_f64;
    let mut pre_energy = 0.0_f64;
    let mut post_energy = 0.0_f64;
    for (n, &h) in coeffs.iter().enumerate() {
        let nf = n as f64;
        if pre_applicable && nf < center - exclusion_samples as f64 {
            pre_max = pre_max.max(h.abs());
            pre_energy += h * h;
        }
        if post_applicable && nf > center + exclusion_samples as f64 {
            post_max = post_max.max(h.abs());
            post_energy += h * h;
        }
    }
    let pre_db = if pre_applicable {
        Some(20.0 * (pre_max / peak).max(MAGNITUDE_FLOOR).log10())
    } else {
        None
    };
    let post_db = if post_applicable {
        Some(20.0 * (post_max / peak).max(MAGNITUDE_FLOOR).log10())
    } else {
        None
    };
    let energy_floor = MAGNITUDE_FLOOR * MAGNITUDE_FLOOR;
    let pre_energy_db = if pre_applicable && total_energy > 0.0 {
        Some(10.0 * (pre_energy / total_energy).max(energy_floor).log10())
    } else {
        None
    };
    let post_energy_db = if post_applicable && total_energy > 0.0 {
        Some(10.0 * (post_energy / total_energy).max(energy_floor).log10())
    } else {
        None
    };
    RingMetrics {
        exclusion_samples,
        peak_index,
        center_samples: center,
        pre_db,
        post_db,
        pre_energy_db,
        post_energy_db,
    }
}

// ---------------------------------------------------------------------------
// 独立验证报告（§3.4 判据 1–8 / §1.2–§1.6 / §2.4 端点）
// ---------------------------------------------------------------------------

/// 分析网格/点数选项（默认按规范 §1.2；compact 便于快速自检，正式报告用 normative）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnalysisOptions {
    /// 通带幅度/相位/GD 主网格点数（规范：>= max(8192, 8·L)）。
    pub passband_points: usize,
    /// 过渡区 f_p..0.499fs 线性网格点数（规范：同 N 点口径）。
    pub transition_points: usize,
    /// Nyquist 带 [0.49fs, 0.499fs] 线性网格点数。
    pub stopband_points: usize,
    /// f32 量化复算网格点数（§1.6）。
    pub quantization_points: usize,
}

impl AnalysisOptions {
    /// 规范 §1.2 网格：N=max(8192, 8·L)；Nyquist 带 4096 点；f32 复算 2048 点。
    pub fn normative(spec: &ApodizingSpec) -> Self {
        let n = (8 * spec.taps).max(8192);
        Self {
            passband_points: n,
            transition_points: n,
            stopband_points: 4096,
            quantization_points: 2048,
        }
    }

    /// 快速自检网格（测试/预检用；**不用于**正式报告，正式报告必须 normative）。
    pub fn compact(taps: usize) -> Self {
        let passband = (taps / 2).clamp(512, 2048);
        let transition = taps.clamp(1024, 2048);
        Self {
            passband_points: passband,
            transition_points: transition,
            stopband_points: 256,
            quantization_points: 128,
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

/// 过渡区统计（f_p..0.499fs）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransitionStats {
    /// 下界（Hz，=f_p）。
    pub lo_hz: f64,
    /// 上界（Hz，=0.499fs）。
    pub hi_hz: f64,
    /// 最小幅度（dB）。
    pub min_db: f64,
    /// 最大幅度（dB）。
    pub max_db: f64,
    /// 相邻网格点最大上升量（dB；判据 6 要求 <= 0.5）。
    pub max_rise_db: f64,
    /// 网格点数。
    pub points: usize,
}

impl TransitionStats {
    /// 判据 6：过渡单调（不上升超过 0.5 dB）。
    pub fn monotonic(&self) -> bool {
        self.max_rise_db.is_finite() && self.max_rise_db <= TRANSITION_RISE_TOLERANCE_DB
    }
}

/// “阻带”统计：apodizing 的阻带即 Nyquist 带 [0.49fs, 0.499fs]（§3.4 判据 3）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StopbandStats {
    /// 下界（Hz，=0.49fs）。
    pub lo_hz: f64,
    /// 上界（Hz，=0.499fs）。
    pub hi_hz: f64,
    /// 最大幅度（dB）。
    pub max_db: f64,
    /// 衰减 = −max_db（dB，正值）。
    pub attenuation_db: f64,
    /// 网格点数。
    pub points: usize,
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
    /// 通带内 max|GD−D|（§3.1 判据 2 口径）。
    pub max_deviation_samples: f64,
    /// 判据 2：常数 GD（max|GD−D| <= 0.01 samples）。
    pub class_constant: bool,
}

/// 参考归一化结果（§1.5；本家族 reference hz=0、gain=0 dB）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReferenceStats {
    /// 参考频点（0=DC）。
    pub hz: f64,
    /// manifest 声明增益（dB）。
    pub declared_gain_db: f64,
    /// 独立复算 |H|（dB）。
    pub measured_gain_db: f64,
}

/// Endpoint 状态（§2.4）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointStatus {
    /// 可测。
    Measured,
    /// 该 fs 不可测（20 kHz > 0.499fs）。
    NotApplicableAtFs,
}

impl EndpointStatus {
    /// 状态字符串（manifest Endpoint 联合类型口径）。
    pub fn schema_name(self) -> &'static str {
        match self {
            EndpointStatus::Measured => "measured",
            EndpointStatus::NotApplicableAtFs => "not_applicable_at_fs",
        }
    }
}

/// 单端点测量（§2.4 Endpoint 联合类型的模块内表示；落盘 JSON 归 T55）。
#[derive(Clone, Debug, PartialEq)]
pub struct Endpoint {
    /// 频点（Hz）。
    pub freq_hz: f64,
    /// 状态。
    pub status: EndpointStatus,
    /// 幅度（dB；不可测时 NaN）。
    pub mag_db: f64,
    /// 相位（度；不可测时 NaN）。
    pub phase_deg: f64,
    /// 不可测原因（Measured 时 None）。
    pub reason: Option<String>,
}

impl Endpoint {
    /// 是否可测。
    pub fn is_measured(&self) -> bool {
        self.status == EndpointStatus::Measured
    }
}

/// 20 Hz / 20 kHz 端点报告（§2.4：两个都必须存在，20k 不可测写 N/A 原因）。
#[derive(Clone, Debug, PartialEq)]
pub struct Endpoints {
    /// 20 Hz 端点。
    pub f20: Endpoint,
    /// 20 kHz 端点。
    pub f20k: Endpoint,
}

impl Endpoints {
    /// 判据：20 Hz 必须可测；20 kHz 可测或带原因 N/A（不得省略/裸字符串）。
    pub fn ok(&self) -> bool {
        self.f20.is_measured()
            && (self.f20k.is_measured()
                || (self.f20k.status == EndpointStatus::NotApplicableAtFs
                    && self.f20k.reason.is_some()))
    }
}

/// 相位量化报告（§3.4 判据 1 常数 GD + §1.6 f32 量化误差复算）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhaseQuantizationReport {
    /// 相位类是否线性（apodizing 必须 true）。
    pub linear_phase: bool,
    /// D=(L−1)/2（samples）。
    pub delay_samples: f64,
    /// 通带内 max|wrap(phase(f)+2πfD/fs)|（度；相对线性相位模型的偏差）。
    pub linear_phase_max_dev_deg: f64,
    /// 1 kHz 测量相位（度）。
    pub phase_at_1khz_deg: f64,
    /// f32 系数 vs f64 系数的最大幅度偏差（dB）。
    pub f32_max_magnitude_dev_db: f64,
    /// f32 系数 vs f64 系数的最大相位偏差（度）。
    pub f32_max_phase_dev_deg: f64,
    /// f32 系数 vs f64 系数的最大群延迟偏差（samples）。
    pub f32_max_group_delay_dev_samples: f64,
    /// f32 系数相对 L2 量化误差。
    pub f32_relative_l2_error: f64,
    /// §1.6 复算是否在工程容差内（报告口径，非 spec 门）。
    pub f32_quantization_ok: bool,
}

/// 家族自洽/互斥报告（§3.4 判据 7 / §3.8 apodizing vs brickwall）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FamilyReport {
    /// f_p（Hz）。
    pub fp_hz: f64,
    /// 家族边界 0.85·fs/2（Hz）。
    pub fp_limit_hz: f64,
    /// `f_p <= 0.85*fs/2`。
    pub fp_ok: bool,
    /// 砖墙探测频点 0.45·fs（Hz）。
    pub brickwall_probe_hz: f64,
    /// 0.45·fs 处实测幅度（dB）。
    pub brickwall_probe_db: f64,
    /// 不是砖墙式：0.45·fs 处 > −60 dB。
    pub not_brickwall: bool,
    /// 报告必须携带的“仅自研可算窗/目标，不复刻任何专有 DAC”声明。
    pub statement: &'static str,
}

impl FamilyReport {
    /// 家族检查通过（边界 + 非砖墙 + 声明存在）。
    pub fn ok(&self) -> bool {
        self.fp_ok && self.not_brickwall && !self.statement.is_empty()
    }
}

/// 有效支撑统计（§1.3）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SupportStats {
    /// 首个 |h|>eps·max|h| 的样本下标。
    pub first: usize,
    /// 最后 |h|>eps·max|h| 的样本下标。
    pub last: usize,
    /// 有效支撑长度 last−first+1。
    pub effective_taps: usize,
    /// 使用的相对门限 eps 以 1e-6 的整数倍记（1 = 1e-6；保持 Eq 语义）。
    pub eps_micro: u32,
}

/// 逐条规范性判据结果（§3.4 判据 1–8 + §1.2–§1.6 公共判据）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApodizingChecks {
    /// 系数有限且 max|h|>0。
    pub finite: bool,
    /// 系数长度 = 声明 taps。
    pub taps: bool,
    /// 判据 1：对称。
    pub symmetry: bool,
    /// 判据 1：D=(L−1)/2、delay_ms 一致。
    pub delay: bool,
    /// 判据 1：通带常数 GD（max|GD−D| <= 0.01 samples）。
    pub group_delay: bool,
    /// 判据 2：通带 max−min <= ripple_db+0.05。
    pub passband_ripple: bool,
    /// 判据 3：“阻带”Nyquist 带 max <= −(nyquist_attenuation_db−1)。
    pub stopband: bool,
    /// 判据 4：pre_ring_db <= pre_ring_bound_db（可测时）。
    pub pre_ring: bool,
    /// 判据 4：pre_ring_energy_db <= −45 dB（可测时）。
    pub pre_ring_energy: bool,
    /// 判据 5：post_ring_db/post_ring_energy_db 已报告（无上限）。
    pub post_ring_reported: bool,
    /// 判据 6：过渡区不上升超过 0.5 dB。
    pub transition_monotonic: bool,
    /// 判据 6（§1.5）：参考 |H(0)|=0 dB ±0.01。
    pub reference: bool,
    /// §2.4：20 Hz/20 kHz 端点。
    pub endpoints: bool,
    /// 判据 7/§3.8：家族边界 + 非砖墙式。
    pub family: bool,
    /// 判据 7：`f_p <= 0.85*fs/2`。
    pub fp_boundary: bool,
    /// §1.6 f32 量化复算在工程容差内（报告口径，非 spec 门）。
    pub f32_quantization: bool,
}

impl ApodizingChecks {
    /// 全部 **spec** 判据通过（f32 量化属 §1.6 报告口径，不计入 spec 门）。
    pub fn passed(&self) -> bool {
        self.finite
            && self.taps
            && self.symmetry
            && self.delay
            && self.group_delay
            && self.passband_ripple
            && self.stopband
            && self.pre_ring
            && self.pre_ring_energy
            && self.post_ring_reported
            && self.transition_monotonic
            && self.reference
            && self.endpoints
            && self.family
            && self.fp_boundary
    }
}

/// apodizing 独立响应验证报告（T55 报告器的输入；不含 WAV/manifest 落盘字段）。
#[derive(Clone, Debug, PartialEq)]
pub struct ApodizingVerification {
    /// 采样率。
    pub fs: f64,
    /// taps。
    pub taps: usize,
    /// D=(L−1)/2。
    pub delay_samples: f64,
    /// D/fs·1000。
    pub delay_ms: f64,
    /// max|h|。
    pub max_abs_h: f64,
    /// 对称性误差 max|h−flip(h)|（绝对值）。
    pub symmetry_max_abs_error: f64,
    /// 有效支撑（§1.3）。
    pub effective_support: SupportStats,
    /// 通带幅度统计 [20,f_p]。
    pub passband: BandStats,
    /// 过渡区统计 [f_p,0.499fs]。
    pub transition: TransitionStats,
    /// “阻带”Nyquist 带统计 [0.49fs,0.499fs]。
    pub stopband: StopbandStats,
    /// 前后振铃（§1.4）。
    pub ringing: RingMetrics,
    /// 群延迟统计（§3.4 判据 1）。
    pub group_delay: GroupDelayStats,
    /// 相位量化报告（常数 GD + f32 量化）。
    pub phase_quantization: PhaseQuantizationReport,
    /// 参考归一化（§1.5）。
    pub reference: ReferenceStats,
    /// 20 Hz / 20 kHz 端点（§2.4）。
    pub endpoints: Endpoints,
    /// 家族自洽/互斥（§3.4 判据 7/§3.8）。
    pub family: FamilyReport,
    /// 逐条判据。
    pub checks: ApodizingChecks,
    /// 未通过 spec 判据的人类可读原因（空=全过）。
    pub failures: Vec<String>,
    /// 非 spec 门的诊断/提示（f32 量化、端点 N/A 原因等）。
    pub warnings: Vec<String>,
}

impl ApodizingVerification {
    /// 全部 spec 判据通过。
    pub fn passed(&self) -> bool {
        self.checks.passed()
    }
}

/// 单点测量（内部）。
#[derive(Clone, Copy, Debug)]
struct Sample {
    f_hz: f64,
    mag_db: f64,
    phase_deg: f64,
    gd_samples: f64,
}

/// 独立响应验证（规范网格；正式报告口径）。等价 verify_apodizing_with + AnalysisOptions::normative。
pub fn verify_apodizing(
    spec: &ApodizingSpec,
    coeffs: &[f64],
) -> Result<ApodizingVerification, ApodizingError> {
    verify_apodizing_with(spec, coeffs, &AnalysisOptions::normative(spec))
}

/// 独立响应验证（自定义网格/点数）。
pub fn verify_apodizing_with(
    spec: &ApodizingSpec,
    coeffs: &[f64],
    options: &AnalysisOptions,
) -> Result<ApodizingVerification, ApodizingError> {
    use ApodizingError as E;

    let validated = validate(spec)?;
    let fs = spec.fs;
    let l = spec.taps;
    if coeffs.len() != l {
        return Err(E::Taps(format!(
            "系数长度 {} != 声明 taps {l}",
            coeffs.len()
        )));
    }
    let mut max_abs_h = 0.0_f64;
    for &h in coeffs {
        if !h.is_finite() {
            return Err(E::Numeric("系数含 NaN/Inf".to_string()));
        }
        max_abs_h = max_abs_h.max(h.abs());
    }
    if max_abs_h <= 0.0 {
        return Err(E::Numeric("max|h|=0（apodizing 家族不允许）".to_string()));
    }

    let d = validated.delay_samples;
    let pb_lo = spec.passband_hz[0];
    let pb_hi = spec.passband_hz[1];
    let measurement_hi = MEASUREMENT_HI_HZ.min(NYQUIST_GUARD_FRACTION * fs);
    if !(measurement_hi > MEASUREMENT_LO_HZ) {
        return Err(E::Band(format!(
            "测量网格上界 {measurement_hi} Hz 必须 > 20 Hz（fs={fs}）"
        )));
    }

    // ---- 通带：幅度/相位/GD（§1.2 对数网格） ----
    let pb_n = options.passband_points.max(2);
    let mut passband_samples: Vec<Sample> = Vec::with_capacity(pb_n + 4);
    for f in log_grid(pb_lo, pb_hi, pb_n) {
        passband_samples.push(eval_sample(coeffs, fs, f));
    }
    let passband = stats_from_samples(&passband_samples, pb_lo, pb_hi);

    // 群延迟统计（判据 1；解析式 GD）。
    let mut gd_min = f64::INFINITY;
    let mut gd_max = f64::NEG_INFINITY;
    let mut gd_sum = 0.0_f64;
    let mut gd_count = 0_usize;
    let mut gd_err_max = 0.0_f64;
    for s in &passband_samples {
        if s.gd_samples.is_finite() {
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

    // 相位线性度（相对 −2πfD/fs 的 wrap 偏差）。
    let mut linear_phase_max_dev_deg = 0.0_f64;
    for s in &passband_samples {
        if s.f_hz.is_finite() && s.phase_deg.is_finite() {
            let model = -360.0 * s.f_hz * d / fs;
            linear_phase_max_dev_deg =
                linear_phase_max_dev_deg.max(wrap_deg_diff(s.phase_deg, model));
        }
    }
    let phase_at_1k = response_at(coeffs, fs, 1_000.0).phase_deg();
    let linear_phase = linear_phase_max_dev_deg.is_finite()
        && linear_phase_max_dev_deg <= LINEAR_PHASE_DEVIATION_TOLERANCE_DEG;

    // ---- 过渡区（判据 6）与“阻带”Nyquist 带（判据 3） ----
    let t_lo = pb_hi;
    let t_hi = NYQUIST_GUARD_FRACTION * fs;
    let t_n = options.transition_points.max(2);
    let mut t_min = f64::INFINITY;
    let mut t_max = f64::NEG_INFINITY;
    let mut t_max_rise = f64::NEG_INFINITY;
    let mut prev: Option<f64> = None;
    for f in linear_grid(t_lo, t_hi, t_n) {
        let db = magnitude_db_at(coeffs, fs, f);
        t_min = t_min.min(db);
        t_max = t_max.max(db);
        if let Some(p) = prev {
            t_max_rise = t_max_rise.max(db - p);
        }
        prev = Some(db);
    }
    let transition = TransitionStats {
        lo_hz: t_lo,
        hi_hz: t_hi,
        min_db: t_min,
        max_db: t_max,
        max_rise_db: t_max_rise,
        points: t_n,
    };

    let s_lo = NYQUIST_BAND_LO_FRACTION * fs;
    let s_hi = NYQUIST_GUARD_FRACTION * fs;
    let s_n = options.stopband_points.max(2);
    let mut s_max = f64::NEG_INFINITY;
    for f in linear_grid(s_lo, s_hi, s_n) {
        s_max = s_max.max(magnitude_db_at(coeffs, fs, f));
    }
    let stopband = StopbandStats {
        lo_hz: s_lo,
        hi_hz: s_hi,
        max_db: s_max,
        attenuation_db: -s_max,
        points: s_n,
    };

    // ---- 0.45fs 砖墙探测（判据 7/§3.8） ----
    let brickwall_probe_hz = BRICKWALL_PROBE_FRACTION * fs;
    let brickwall_probe_db = magnitude_db_at(coeffs, fs, brickwall_probe_hz);
    let fp_ok = spec.passband_hz[1] <= validated.fp_limit_hz + 1e-9;
    let not_brickwall = brickwall_probe_db.is_finite() && brickwall_probe_db > BRICKWALL_PROBE_DB;
    let family = FamilyReport {
        fp_hz: spec.passband_hz[1],
        fp_limit_hz: validated.fp_limit_hz,
        fp_ok,
        brickwall_probe_hz,
        brickwall_probe_db,
        not_brickwall,
        statement: NO_PROPRIETARY_DAC_STATEMENT,
    };

    // ---- 前后振铃（判据 4/5；§1.4） ----
    let ringing = ring_metrics(coeffs, fs);

    // ---- 参考（判据 6/§1.5） ----
    let reference = ReferenceStats {
        hz: 0.0,
        declared_gain_db: 0.0,
        measured_gain_db: response_at(coeffs, fs, 0.0).magnitude_db(),
    };

    // ---- 20 Hz / 20 kHz 端点（§2.4） ----
    let endpoint_20 = measured_endpoint(coeffs, fs, 20.0);
    let endpoint_20k = if MEASUREMENT_HI_HZ <= NYQUIST_GUARD_FRACTION * fs {
        measured_endpoint(coeffs, fs, MEASUREMENT_HI_HZ)
    } else {
        Endpoint {
            freq_hz: MEASUREMENT_HI_HZ,
            status: EndpointStatus::NotApplicableAtFs,
            mag_db: f64::NAN,
            phase_deg: f64::NAN,
            reason: Some(format!(
                "20 kHz > 0.499*fs = {} Hz（fs={fs}），按 §2.4 记 not_applicable_at_fs",
                NYQUIST_GUARD_FRACTION * fs
            )),
        }
    };
    let endpoints = Endpoints {
        f20: endpoint_20,
        f20k: endpoint_20k,
    };

    // ---- f32 量化复算（§1.6） ----
    let quantized: Vec<f64> = coeffs.iter().map(|&h| f64::from(h as f32)).collect();
    let q_n = options.quantization_points.max(2);
    let mut f32_max_mag_dev = 0.0_f64;
    let mut f32_max_phase_dev = 0.0_f64;
    for f in log_grid(MEASUREMENT_LO_HZ, measurement_hi, q_n) {
        let a = response_at(coeffs, fs, f);
        let b = response_at(&quantized, fs, f);
        f32_max_mag_dev = f32_max_mag_dev.max((a.magnitude_db() - b.magnitude_db()).abs());
        f32_max_phase_dev = f32_max_phase_dev.max(wrap_deg_diff(a.phase_deg(), b.phase_deg()));
    }
    // 群延迟偏差只在通带内复算（判据 1 的声明域）。
    let mut f32_max_gd_dev = 0.0_f64;
    for f in log_grid(pb_lo, pb_hi, pb_n) {
        let gd_a = group_delay_at(coeffs, fs, f);
        let gd_b = group_delay_at(&quantized, fs, f);
        if gd_a.is_finite() && gd_b.is_finite() {
            f32_max_gd_dev = f32_max_gd_dev.max((gd_a - gd_b).abs());
        }
    }
    let mut signal_energy = 0.0_f64;
    let mut error_energy = 0.0_f64;
    for (a, b) in coeffs.iter().zip(quantized.iter()) {
        signal_energy += a * a;
        let e = a - b;
        error_energy += e * e;
    }
    let f32_relative_l2_error = if signal_energy > 0.0 {
        (error_energy / signal_energy).sqrt()
    } else {
        f64::NAN
    };
    let f32_quantization_ok = f32_max_mag_dev <= F32_MAGNITUDE_DEVIATION_TOLERANCE_DB
        && f32_max_phase_dev <= F32_PHASE_DEVIATION_TOLERANCE_DEG
        && f32_max_gd_dev <= F32_GROUP_DELAY_DEVIATION_TOLERANCE_SAMPLES
        && f32_relative_l2_error <= F32_RELATIVE_L2_TOLERANCE;

    // ---- 判据汇总 ----
    let symmetry_err = symmetry_error(coeffs);
    let (first, last, effective_taps) = effective_support(coeffs, 1e-6);
    let finite = max_abs_h.is_finite() && max_abs_h > 0.0;
    let taps_ok = coeffs.len() == l;
    let symmetry_ok = symmetry_err.is_finite() && symmetry_err <= SYMMETRY_TOLERANCE * max_abs_h;
    let delay_ok = (d - (l - 1) as f64 / 2.0).abs() == 0.0
        && (validated.delay_ms - d / fs * 1000.0).abs() <= 1e-9;
    let passband_ok = passband.points > 0
        && passband.min_db.is_finite()
        && passband.max_db.is_finite()
        && passband.ripple_db() <= spec.ripple_db + PASSBAND_RIPPLE_MARGIN_DB;
    let stopband_ok = stopband.max_db.is_finite()
        && stopband.max_db <= -(spec.nyquist_attenuation_db - NYQUIST_BAND_MARGIN_DB);
    let pre_ring_ok = ringing
        .pre_db
        .map(|v| v <= spec.pre_ring_bound_db)
        .unwrap_or(false);
    let pre_ring_energy_ok = ringing
        .pre_energy_db
        .map(|v| v <= PRE_RING_ENERGY_BOUND_DB)
        .unwrap_or(false);
    let post_ring_reported = ringing.post_db.is_some() && ringing.post_energy_db.is_some();
    let transition_ok = transition.monotonic();
    let reference_ok =
        (reference.measured_gain_db - reference.declared_gain_db).abs() <= REFERENCE_TOLERANCE_DB;
    let endpoints_ok = endpoints.ok();
    let checks = ApodizingChecks {
        finite,
        taps: taps_ok,
        symmetry: symmetry_ok,
        delay: delay_ok,
        group_delay: gd_ok,
        passband_ripple: passband_ok,
        stopband: stopband_ok,
        pre_ring: pre_ring_ok,
        pre_ring_energy: pre_ring_energy_ok,
        post_ring_reported,
        transition_monotonic: transition_ok,
        reference: reference_ok,
        endpoints: endpoints_ok,
        family: family.ok(),
        fp_boundary: fp_ok,
        f32_quantization: f32_quantization_ok,
    };

    let mut failures = Vec::new();
    if !finite {
        failures.push("finite: max|h| 非有限或为 0".to_string());
    }
    if !taps_ok {
        failures.push("taps: 系数长度与声明不符".to_string());
    }
    if !symmetry_ok {
        failures.push(format!(
            "对称性: max|h−flip(h)|={symmetry_err:.6e} > {SYMMETRY_TOLERANCE}·max|h|"
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
            "Nyquist 带: max={:.6} dB > −(nyq_att−{NYQUIST_BAND_MARGIN_DB})={:.6}",
            stopband.max_db,
            -(spec.nyquist_attenuation_db - NYQUIST_BAND_MARGIN_DB)
        ));
    }
    match (ringing.pre_db, spec.pre_ring_bound_db) {
        (Some(v), bound) if v <= bound => {}
        (Some(v), bound) => failures.push(format!(
            "前振铃: pre_ring_db={v:.6} > bound {bound} dB（§3.4 判据 4）"
        )),
        (None, _) => {
            failures.push("前振铃: 支撑过短（p−W<0）无法证明 <= bound（§1.4 n/a）".to_string())
        }
    }
    match ringing.pre_energy_db {
        Some(v) if v <= PRE_RING_ENERGY_BOUND_DB => {}
        Some(v) => failures.push(format!(
            "前振铃能量: pre_energy_db={v:.6} > {PRE_RING_ENERGY_BOUND_DB} dB（§3.4 判据 4）"
        )),
        None => failures.push("前振铃能量: n/a（支撑过短）".to_string()),
    }
    if !post_ring_reported {
        failures.push("后振铃: 未报告（§3.4 判据 5 必须报告）".to_string());
    }
    if !transition_ok {
        failures.push(format!(
            "过渡单调: max rise={:.6} dB > {TRANSITION_RISE_TOLERANCE_DB} dB（§3.4 判据 6）",
            transition.max_rise_db
        ));
    }
    if !reference_ok {
        failures.push(format!(
            "参考: |H(0)|={:.6} dB 与声明 0 dB 差 > {REFERENCE_TOLERANCE_DB}",
            reference.measured_gain_db
        ));
    }
    if !endpoints_ok {
        failures.push("端点: 20 Hz/20 kHz Endpoint 缺失或 status 非法（§2.4）".to_string());
    }
    if !fp_ok {
        failures.push(format!(
            "家族边界: f_p={} > 0.85*fs/2={}（§3.4 判据 7）",
            spec.passband_hz[1], validated.fp_limit_hz
        ));
    }
    if !not_brickwall {
        failures.push(format!(
            "家族互斥: |H(0.45fs={} Hz)|={brickwall_probe_db:.6} dB <= {BRICKWALL_PROBE_DB} dB（砖墙式，必须改报 brickwall/linear；§3.4 判据 7/§3.8）",
            brickwall_probe_hz
        ));
    }

    let mut warnings = Vec::new();
    if !f32_quantization_ok {
        warnings.push(format!(
            "f32 量化: max|Δmag|={f32_max_mag_dev:.3e} dB / max|Δphase|={f32_max_phase_dev:.3e}° / max|ΔGD|={f32_max_gd_dev:.3e} samples / relL2={f32_relative_l2_error:.3e}（§1.6 报告口径）"
        ));
    }
    if !endpoints.f20k.is_measured() {
        if let Some(reason) = &endpoints.f20k.reason {
            warnings.push(format!("20 kHz 端点: not_applicable_at_fs（{reason}）"));
        }
    }

    Ok(ApodizingVerification {
        fs,
        taps: l,
        delay_samples: d,
        delay_ms: validated.delay_ms,
        max_abs_h,
        symmetry_max_abs_error: symmetry_err,
        effective_support: SupportStats {
            first,
            last,
            effective_taps,
            eps_micro: 1,
        },
        passband,
        transition,
        stopband,
        ringing,
        group_delay: GroupDelayStats {
            min_samples: gd_min,
            max_samples: gd_max,
            mean_samples: gd_mean,
            at_1khz_samples: gd_at_1k,
            max_deviation_samples: gd_err_max,
            class_constant: gd_ok,
        },
        phase_quantization: PhaseQuantizationReport {
            linear_phase,
            delay_samples: d,
            linear_phase_max_dev_deg,
            phase_at_1khz_deg: phase_at_1k,
            f32_max_magnitude_dev_db: f32_max_mag_dev,
            f32_max_phase_dev_deg: f32_max_phase_dev,
            f32_max_group_delay_dev_samples: f32_max_gd_dev,
            f32_relative_l2_error,
            f32_quantization_ok,
        },
        reference,
        endpoints,
        family,
        checks,
        failures,
        warnings,
    })
}

/// 对已构造设计做独立验证（等价 verify_apodizing(&design.spec, &design.coeffs)）。
pub fn verify_apodizing_design(
    design: &ApodizingDesign,
) -> Result<ApodizingVerification, ApodizingError> {
    verify_apodizing(&design.spec, &design.coeffs)
}

/// 构造 + 独立验证；任一 spec 判据失败返回 E_FIR_*（不返回“看起来成功”的坏设计）。
pub fn design_checked(spec: &ApodizingSpec) -> Result<ApodizingDesign, ApodizingError> {
    design_checked_with(spec, &AnalysisOptions::normative(spec))
}

/// 构造 + 独立验证（自定义分析网格）。
pub fn design_checked_with(
    spec: &ApodizingSpec,
    options: &AnalysisOptions,
) -> Result<ApodizingDesign, ApodizingError> {
    let design = design(spec)?;
    let verification = verify_apodizing_with(spec, &design.coeffs, options)?;
    if verification.passed() {
        Ok(design)
    } else {
        Err(criteria_to_error(&verification))
    }
}

/// 验证任意系数（如砖墙构造）并执行 spec 判据；失败返回分类后的 E_FIR_*。
///
/// 用途：§3.8 家族互斥——把 brickwall 式系数按 apodizing 报告会得到 E_FIR_FAMILY 而不是假通过。
pub fn verify_apodizing_checked(
    spec: &ApodizingSpec,
    coeffs: &[f64],
) -> Result<ApodizingVerification, ApodizingError> {
    let verification = verify_apodizing(spec, coeffs)?;
    if verification.passed() {
        Ok(verification)
    } else {
        Err(criteria_to_error(&verification))
    }
}

/// 把未通过判据分类到 E_FIR_* 码。
fn criteria_to_error(v: &ApodizingVerification) -> ApodizingError {
    let detail = v.failures.join("；");
    if !v.checks.taps {
        ApodizingError::Taps(detail)
    } else if !v.checks.finite {
        ApodizingError::Numeric(detail)
    } else if !v.checks.symmetry || !v.checks.group_delay {
        ApodizingError::PhaseClass(detail)
    } else if !v.checks.reference {
        ApodizingError::Reference(detail)
    } else if !v.checks.family || !v.checks.fp_boundary {
        ApodizingError::Family(detail)
    } else if v.ringing.pre_applicable() && (!v.checks.pre_ring || !v.checks.pre_ring_energy) {
        ApodizingError::Family(detail)
    } else {
        ApodizingError::Budget(detail)
    }
}

// ---------------------------------------------------------------------------
// 内部小工具
// ---------------------------------------------------------------------------

fn measured_endpoint(coeffs: &[f64], fs: f64, freq_hz: f64) -> Endpoint {
    let response = response_at(coeffs, fs, freq_hz);
    Endpoint {
        freq_hz,
        status: EndpointStatus::Measured,
        mag_db: response.magnitude_db(),
        phase_deg: response.phase_deg(),
        reason: None,
    }
}

fn eval_sample(coeffs: &[f64], fs: f64, f_hz: f64) -> Sample {
    let (re, im, gd) = eval_h_and_gd(coeffs, fs, f_hz);
    let mag = re.hypot(im);
    Sample {
        f_hz,
        mag_db: 20.0 * mag.max(MAGNITUDE_FLOOR).log10(),
        phase_deg: im.atan2(re) * 180.0 / PI,
        gd_samples: gd,
    }
}

fn stats_from_samples(samples: &[Sample], lo: f64, hi: f64) -> BandStats {
    let eps = 1e-9 * (lo.abs() + hi.abs() + 1.0);
    let mut min_db = f64::INFINITY;
    let mut max_db = f64::NEG_INFINITY;
    let mut points = 0_usize;
    for s in samples {
        if s.f_hz >= lo - eps && s.f_hz <= hi + eps && s.mag_db.is_finite() {
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

/// wrap 相位差到 (−180,180]（度）并取绝对值。
fn wrap_deg_diff(a_deg: f64, b_deg: f64) -> f64 {
    if !a_deg.is_finite() || !b_deg.is_finite() {
        return f64::NAN;
    }
    let mut diff = a_deg - b_deg;
    while diff > 180.0 {
        diff -= 360.0;
    }
    while diff <= -180.0 {
        diff += 360.0;
    }
    diff.abs()
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
mod apodizing_tests {
    use super::*;
    include!("../tests/T35/apodizing_cases.rs");
}
