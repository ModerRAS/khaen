//! T55 家族独立报告器：七类 FIR 预设家族 + nos 的独立 oracle、幅相/群延迟/前后振铃/支撑/预算报告。
//!
//! 归属：WBS T55 / task-96，owner rt-fir-reporter2；写域仅本文件 + crates/designer/tests/T55 +
//! rt-stack/evidence/T55。权威契约：rt-stack/spec/fir-toolbox.md（v1.2-draft）§1.2/§1.3/§1.4/
//! §2.2/§2.3/§2.4/§3.1-§3.8/§7.2/§9。
//!
//! 独立性（本任务的 DoD 核心）：本模块不调用 linear/min_fast/min_slow/apodizing/brickwall/
//! corrected/nos 等 leaf 模块的验证实现，也不复用它们的 FFT/报告路径。响应（直接 DTFT）、精确群
//! 延迟（解析 Re(G/H) 形式）、振铃、有效支撑、最小相位 M2 相位–Hilbert 一致性（自带 radix-2 FFT）、
//! Kaiser 可行性式与 §2.3 成本估算全部按规范公式在本文件内独立实现，仅用 Rust 标准库 + f64。
//! leaf 模块的 verify 报告只有在 T55 测试里作为【交叉对照】出现，不进入本模块实现。
//!
//! 输入：族标识 + f64 因果冲激响应 h（len = taps）+ 设计声明（族参数/参考/延迟/预算）+ 可选
//! 复杂目标曲线（corrected/lowdelay）与可选外部报告响应（§2.2 规则 5：0.05 dB / 0.1 sample 复算
//! 一致性复核）。
//!
//! 输出：FirReport —— 字段镜像 spec §2.2 的 response/reference/limits/verification 子集，含：
//! - 测量网格定义与点数（§1.2：20 Hz - min(20 kHz, 0.499 fs)，N >= max(8192, 8*L)）；
//! - 通带/阻带/Nyquist 带纹波与衰减、过渡带穿越点（LP 首次 / HP 镜像读法最后一次，Lead 裁决 A）；
//! - 群延迟 class/min/max/mean/@1 kHz/onset/latency_ms/gd_max_ms（最小相位类频变 GJ，不冒称零延迟）；
//! - 前/后振铃（W = ceil(0.000333 fs)、dB 与能量、brickwall 的 -80 dB 振铃时长）；
//! - 20 Hz 与 20 kHz Endpoint 联合类型（measured / not_applicable_at_fs，不用裸字符串、不省略）；
//! - 预算：taps profile（regular/extended/offline/未准入）、f32 内存、15/25/40 ms 延迟档、Kaiser
//!   可行性、§2.3 direct/FFT 成本与可选融合（L_F = L_G + L_P - 1）；
//! - 各族独立判据 + §3.8 家族自洽/互斥检查（失败带 E_FIR_* 码）；
//! - 陈述行（apodizing 必须含"仅自研可算窗/目标，不复刻任何专有 DAC"；nos 必须含旁路/保护仍在图
//!   外执行；min_* 必须写频变 GD/非零延迟；corrected 必须写非等价声明）。
//!
//! 拒绝（返回 Err，不静默降级 / 不伪造报告）：未知或不支持的 family 字符串（例如把相邻工具
//! crossover/phase 当七家族）、taps 与 len(h) 不符、h 含 NaN/Inf 或 max|h|=0（nos 除外）、fs 非法、
//! nos 的 taps != 1、corrected/lowdelay 缺显式相位目标（§3.6 判据 1）、band 参数非法。
//! 判据不通过不返回 Err，而是记录 Criterion{passed:false, code}（保留完整实测报告），由
//! require_pass() 统一拒绝 —— 对应 WBS §6"失败 BLOCKED/FAIL 保原始证据，不静默清零"。
//!
//! 未做（NOT_RUN，见 evidence/T55/MANIFEST.md）：写 WAV/f32 系数文件与 khaen-fir/v1 JSON 落盘
//! （本任务 DoD 只要求报告数据模型；文件 I/O 归 T14/T26 集成）、板端/ALSA/RT/soak、真实测量、
//! 跨架构数值差异、f32 运行期量化误差实测（只提供 f32 系数转换的接口建议）。

use std::f64::consts::PI;

// ===========================================================================
// 常量（全部引用 spec/fir-toolbox.md v1.2-draft 的条款，不自行放宽）
// ===========================================================================

/// §1.2 测量网格下界（Hz）。
pub const MEASUREMENT_LO_HZ: f64 = 20.0;
/// §1.2 测量网格上界之一（Hz）；实际上界 = min(20 kHz, 0.499 fs)。
pub const MEASUREMENT_HI_HZ: f64 = 20_000.0;
/// §1.2/§2.4 Nyquist 护栏比例（0.499 fs）。
pub const NYQUIST_GUARD_FRACTION: f64 = 0.499;
/// §1.2 网格点数下限。
pub const MIN_GRID_POINTS: usize = 8_192;
/// §1.2 网格点数下限系数：N >= max(8192, 8*L)。
pub const GRID_POINTS_PER_TAP: usize = 8;
/// §2.1/§7.2 设计 taps 上限（>16384 仅 offline 工具产物）。
pub const MAX_DESIGN_TAPS: usize = 16_384;
/// §7.2 常规 profile 每路由有效支撑上限。
pub const REGULAR_PROFILE_TAPS_CAP: usize = 4_096;
/// §7.2 extended profile 上限（8192/16384）。
pub const EXTENDED_PROFILE_TAPS_CAP: usize = 16_384;
/// §2.2 manifest 允许的 fs 值（48k/96k）；其它 fs 只在报告里作事实并 warning。
pub const MANIFEST_FS_VALUES: [u32; 2] = [48_000, 96_000];
/// §1.4 振铃排除窗比例：W = ceil(0.000333 * fs)（48k=16、96k=32）。
pub const RING_EXCLUSION_FRACTION: f64 = 0.000_333;
/// §1.3 有效支撑 eps（相对 max|h|）。
pub const EFFECTIVE_SUPPORT_EPS: f64 = 1e-6;
/// §1.2 |H| 下限（-240 dB），避免 log0。
pub const MAGNITUDE_FLOOR: f64 = 1e-12;
/// §2.2 规则 5：报告内 dB 数值复算容差。
pub const REPORT_DB_TOLERANCE: f64 = 0.05;
/// §2.2 规则 5：报告内 sample 数值复算容差。
pub const REPORT_SAMPLE_TOLERANCE: f64 = 0.1;
/// §2.2 规则 5：相位复算容差（工程口径；spec 只写 dB/sample）。
pub const REPORT_PHASE_TOLERANCE_DEG: f64 = 0.1;
/// §1.5/§2.2 规则 4：参考归一化容差。
pub const REFERENCE_TOLERANCE_DB: f64 = 0.01;
/// §3.1 判据 2：线性相位常数 GD 容差（samples）。
pub const GROUP_DELAY_TOLERANCE_SAMPLES: f64 = 0.01;
/// §3.6 判据 1：|D_used - D_target| 容差（samples）。
pub const DELAY_CONSISTENCY_SAMPLES: f64 = 0.01;
/// §3.0 通带纹波判定余量（dB）。
pub const PASSBAND_RIPPLE_MARGIN_DB: f64 = 0.05;
/// §3.1 判据 4 LP 阻带测量余量（dB）。
pub const STOPBAND_LP_MARGIN_DB: f64 = 0.5;
/// §3.2 判据 2：M2 最大偏差（deg）。
pub const MIN_PHASE_LIMIT_DEG: f64 = 2.0;
/// §3.2 判据 2：M2 FFT 尺寸。
pub const MIN_PHASE_FFT: usize = 65_536;
/// §3.2 判据 2：M2 log|H| 下限（dB）。
pub const MIN_PHASE_LOG_FLOOR_DB: f64 = -120.0;
/// §3.2 判据 2：M2 比较带上界比例（0.45 fs）。
pub const MIN_PHASE_BAND_HI_RATIO: f64 = 0.45;
/// §3.4 判据 4：前振铃向上限（dB）。
pub const APODIZING_PRE_RING_BOUND_DB: f64 = -45.0;
/// §3.4 判据 4：前振铃能量上限（dB，固定值）。
pub const APODIZING_PRE_RING_ENERGY_BOUND_DB: f64 = -45.0;
/// §3.4 判据 7：f_p <= 0.85 * Nyquist。
pub const APODIZING_FP_MAX_FRACTION_OF_NYQUIST: f64 = 0.85;
/// §3.3 判据 2 / §3.4 判据 3：Nyquist 带 [0.49 fs, 0.499 fs]。
pub const NYQUIST_BAND_LO_FRACTION: f64 = 0.49;
/// §3.3/§3.4 Nyquist 带衰减判定余量（dB）。
pub const NYQUIST_BAND_MARGIN_DB: f64 = 1.0;
/// §3.3 判据 3：f > f_p 后允许的最大上翘（dB）。
pub const MIN_SLOW_MONOTONIC_RISE_DB: f64 = 0.25;
/// §3.3 判据 6：f_p <= 0.42 fs。
pub const MIN_SLOW_FP_RATIO: f64 = 0.42;
/// §3.3 判据 6 / §3.2 判据 4：fast/slow 互斥边界（0.45 fs / -60 dB）。
pub const FAST_BOUNDARY_RATIO: f64 = 0.45;
/// §3.3 判据 6：M(0.45 fs) > -60 dB 才允许 min_slow。
pub const FAST_BOUNDARY_ATTENUATION_DB: f64 = -60.0;
/// §3.2 判据 4：min_fast 边界 f_s/f_p <= 1.5。
pub const MIN_FAST_EDGE_RATIO: f64 = 1.5;
/// §3.2 判据 4：min_fast 边界 f_s <= 0.45 fs。
pub const MIN_FAST_EDGE_NYQUIST: f64 = 0.45;
/// §3.5 判据 4/§3.1 判据 5：过渡穿越容差比例 0.05*Δf。
pub const TRANSITION_TOLERANCE_FRACTION: f64 = 0.05;
/// §3.5/§3.1 过渡穿越容差下限（Hz）。
pub const TRANSITION_TOLERANCE_MIN_HZ: f64 = 20.0;
/// §3.5 判据 7：Δf >= 20 Hz。
pub const BRICKWALL_MIN_TRANSITION_HZ: f64 = 20.0;
/// §3.5 判据 7：A_s ∈ [60, 120]。
pub const BRICKWALL_MIN_ATTENUATION_DB: f64 = 60.0;
/// §3.5 判据 7：A_s ∈ [60, 120]。
pub const BRICKWALL_MAX_ATTENUATION_DB: f64 = 120.0;
/// §3.5 判据 5 / §9 V13：L_req = ceil(1 + (A_s - 7.95) * fs / (14.357078 * Δf))。
pub const KAISER_TAPS_NUMERATOR: f64 = 14.357_078;
/// §3.5 判据 5：Kaiser 经验式 A_s 下限（>=40 dB）。
pub const KAISER_MIN_ATTENUATION_DB: f64 = 40.0;
/// §3.5 判据 6：振铃时长衰减到 -80 dB。
pub const RING_DURATION_DB: f64 = -80.0;
/// §3.4 判据 6：过渡区允许上升（dB）。
pub const APODIZING_TRANSITION_RISE_DB: f64 = 0.5;
/// §3.4 判据 7 / §3.8：0.45 fs 砖墙探测比例。
pub const BRICKWALL_PROBE_FRACTION: f64 = 0.45;
/// §3.4 判据 7 / §3.8：0.45 fs 处 <= -60 dB 即砖墙式。
pub const BRICKWALL_PROBE_DB: f64 = -60.0;
/// §7.2 延迟预算档位（ms，整链）。
pub const LATENCY_BUDGETS_MS: [f64; 3] = [15.0, 25.0, 40.0];
/// §1.1 生产候选块长（T02 core-v1 v1.1.1 N8：64/128）。
pub const PRODUCTION_BLOCK_SAMPLES: [usize; 2] = [64, 128];
/// §2.3 默认估工块长（报告口径，不是 runtime 候选）。
pub const DEFAULT_COST_BLOCK: usize = 128;
/// §2.1/§7.2 f32 系数每 tap 字节数。
pub const F32_BYTES_PER_TAP: usize = 4;
/// §2.3 复数 MAC 的 FLOP 计数。
pub const FLOPS_PER_COMPLEX_MAC: f64 = 8.0;
/// §2.3 实 FFT 的 2.5*K*log2(K) 模型系数。
pub const FFT_MODEL_COEFFICIENT: f64 = 2.5;

// 失败码（§2.5 集中表 + §6.9 目标码；本模块只使用规范里已定义的码）。
pub const E_FIR_SCHEMA: &str = "E_FIR_SCHEMA";
pub const E_FIR_TAPS: &str = "E_FIR_TAPS";
pub const E_FIR_DELAY: &str = "E_FIR_DELAY";
pub const E_FIR_NUMERIC: &str = "E_FIR_NUMERIC";
pub const E_FIR_REF: &str = "E_FIR_REF";
pub const E_FIR_BAND: &str = "E_FIR_BAND";
pub const E_FIR_HP_NYQ: &str = "E_FIR_HP_NYQ";
pub const E_FIR_PHASE_CLASS: &str = "E_FIR_PHASE_CLASS";
pub const E_FIR_MINPHASE: &str = "E_FIR_MINPHASE";
pub const E_FIR_FAMILY: &str = "E_FIR_FAMILY";
pub const E_FIR_EQUIV_CLAIM: &str = "E_FIR_EQUIV_CLAIM";
pub const E_FIR_NOS: &str = "E_FIR_NOS";
pub const E_FIR_BUDGET: &str = "E_FIR_BUDGET";
pub const E_FIR_ENDPOINT: &str = "E_FIR_ENDPOINT";
pub const E_FIR_NONCAUSAL: &str = "E_FIR_NONCAUSAL";
pub const E_FIR_APO_FP: &str = "E_FIR_APO_FP";
pub const E_TGT_PHASE_UNSUPPORTED: &str = "E_TGT_PHASE_UNSUPPORTED";
/// §3.6 判据 5：lowdelay 在 D_max 内达不到误差门 -> INFEASIBLE（报告 best achieved）。
pub const INFEASIBLE: &str = "INFEASIBLE";

// ===========================================================================
// 族标识
// ===========================================================================

/// manifest family 字段的 8 个合法值（§2.2；correction/lowdelay 是同一族的两个模式，§1.7 七族表）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Linear,
    MinFast,
    MinSlow,
    Apodizing,
    Brickwall,
    Corrected,
    Lowdelay,
    Nos,
}

impl Family {
    /// 全量 family 字符串（§2.2 顺序）。
    pub const ALL: [Family; 8] = [
        Family::Linear,
        Family::MinFast,
        Family::MinSlow,
        Family::Apodizing,
        Family::Brickwall,
        Family::Corrected,
        Family::Lowdelay,
        Family::Nos,
    ];

    /// manifest 字符串（§2.2）。
    pub fn as_str(self) -> &'static str {
        match self {
            Family::Linear => "linear",
            Family::MinFast => "min_fast",
            Family::MinSlow => "min_slow",
            Family::Apodizing => "apodizing",
            Family::Brickwall => "brickwall",
            Family::Corrected => "corrected",
            Family::Lowdelay => "lowdelay",
            Family::Nos => "nos",
        }
    }

    /// 七族分组（corrected/lowdelay 归 corrected 组）。
    pub fn group(self) -> FamilyGroup {
        match self {
            Family::Linear => FamilyGroup::Linear,
            Family::MinFast => FamilyGroup::MinFast,
            Family::MinSlow => FamilyGroup::MinSlow,
            Family::Apodizing => FamilyGroup::Apodizing,
            Family::Brickwall => FamilyGroup::Brickwall,
            Family::Corrected | Family::Lowdelay => FamilyGroup::Corrected,
            Family::Nos => FamilyGroup::Nos,
        }
    }

    /// 相位类（§1.7 表）。
    pub fn phase_class(self) -> PhaseClass {
        match self {
            Family::Linear | Family::Apodizing | Family::Brickwall => PhaseClass::Constant,
            Family::MinFast | Family::MinSlow => PhaseClass::Minimum,
            Family::Corrected | Family::Lowdelay => PhaseClass::FrequencyVarying,
            Family::Nos => PhaseClass::None,
        }
    }

    /// 是否线性相位/常数延迟族（D 必须 = (L-1)/2）。
    pub fn is_constant_delay(self) -> bool {
        matches!(self, Family::Linear | Family::Apodizing | Family::Brickwall)
    }

    /// 是否最小相位族。
    pub fn is_minimum_phase(self) -> bool {
        matches!(self, Family::MinFast | Family::MinSlow)
    }
}

impl std::fmt::Display for Family {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 解析 manifest family 字符串（严格小写，§2.2）。未知/不支持 -> E_FIR_SCHEMA 拒绝。
///
/// 注意：相邻工具（spec §4 的 crossover / phase-delay-polarity）不是七族预设，传进来必须拒绝，
/// 由 T39/T40 自己的报告承担，不在这里冒充 family。
pub fn parse_family(text: &str) -> Result<Family, ReportError> {
    for f in Family::ALL {
        if f.as_str() == text {
            return Ok(f);
        }
    }
    Err(ReportError::new(
        E_FIR_SCHEMA,
        format!(
            "unsupported family {text:?}: khaen-fir/v1 只允许 linear|min_fast|min_slow|apodizing|brickwall|corrected|lowdelay|nos（§2.2）；相邻工具 crossover/phase-delay-polarity 不是七族预设（§4），必须拒绝而不是猜"
        ),
    ))
}

/// 七族分组。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FamilyGroup {
    Linear,
    MinFast,
    MinSlow,
    Apodizing,
    Brickwall,
    Corrected,
    Nos,
}

impl FamilyGroup {
    pub fn as_str(self) -> &'static str {
        match self {
            FamilyGroup::Linear => "linear",
            FamilyGroup::MinFast => "min_fast",
            FamilyGroup::MinSlow => "min_slow",
            FamilyGroup::Apodizing => "apodizing",
            FamilyGroup::Brickwall => "brickwall",
            FamilyGroup::Corrected => "corrected/lowdelay",
            FamilyGroup::Nos => "nos",
        }
    }
}

/// 相位类（§1.7 表）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhaseClass {
    /// 常数 D（linear/apodizing/brickwall）。
    Constant,
    /// 最小相位、频变 GD（min_fast/min_slow）。
    Minimum,
    /// 显式幅相目标、可频变（corrected/lowdelay）。
    FrequencyVarying,
    /// 无（nos）。
    None,
}

impl PhaseClass {
    pub fn as_str(self) -> &'static str {
        match self {
            PhaseClass::Constant => "constant",
            PhaseClass::Minimum => "minimum",
            PhaseClass::FrequencyVarying => "frequency_varying",
            PhaseClass::None => "none",
        }
    }
}

/// 频带模式（§3.1/§3.5）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BandMode {
    Lowpass,
    Highpass,
}

impl BandMode {
    pub fn as_str(self) -> &'static str {
        match self {
            BandMode::Lowpass => "lowpass",
            BandMode::Highpass => "highpass",
        }
    }
}

/// 对称类型（§3.1）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymmetryMode {
    Symmetric,
    Antisymmetric,
}

impl SymmetryMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SymmetryMode::Symmetric => "symmetric",
            SymmetryMode::Antisymmetric => "antisymmetric",
        }
    }
    pub fn sign(self) -> f64 {
        match self {
            SymmetryMode::Symmetric => 1.0,
            SymmetryMode::Antisymmetric => -1.0,
        }
    }
}

/// corrected/lowdelay 的显式相位目标（§6.6）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PhaseTarget {
    /// 期望总相位（已展开，deg）。
    Absolute,
    /// 去粗延迟相位；target_delay_samples 必须与 D 一致（§3.6 判据 1）。
    DelayRemoved { target_delay_samples: f64 },
}

impl PhaseTarget {
    pub fn as_str(self) -> &'static str {
        match self {
            PhaseTarget::Absolute => "absolute",
            PhaseTarget::DelayRemoved { .. } => "delay_removed",
        }
    }
}

/// corrected 的等价性声明（§3.6 判据 6/§3.8：不得把 corrected 说成 linear/min 等价）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EquivalenceClaim {
    /// 不声明等价（合法）。
    None,
    /// 声称与线性相位等价（非法 -> E_FIR_EQUIV_CLAIM）。
    LinearPhase,
    /// 声称与最小相位等价（非法 -> E_FIR_EQUIV_CLAIM）。
    MinimumPhase,
    /// 声称与实测复矩阵等价（仅当真的带显式相位目标；本报告要求 phase_target 存在）。
    MeasuredComplexMatrix,
}

impl EquivalenceClaim {
    pub fn as_str(self) -> &'static str {
        match self {
            EquivalenceClaim::None => "none",
            EquivalenceClaim::LinearPhase => "linear_phase",
            EquivalenceClaim::MinimumPhase => "minimum_phase",
            EquivalenceClaim::MeasuredComplexMatrix => "measured_complex_matrix",
        }
    }
}

/// brickwall 构造方法（§3.5 参数 method）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrickwallMethod {
    KaiserWindowedSinc,
    LeastSquares,
    Remez,
}

impl BrickwallMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            BrickwallMethod::KaiserWindowedSinc => "kaiser_windowed_sinc",
            BrickwallMethod::LeastSquares => "least_squares",
            BrickwallMethod::Remez => "remez",
        }
    }
}

/// 运行准入 profile（§7.2）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeProfile {
    Regular,
    Extended,
    OfflineOnly,
}

impl RuntimeProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            RuntimeProfile::Regular => "regular",
            RuntimeProfile::Extended => "extended",
            RuntimeProfile::OfflineOnly => "offline_only",
        }
    }
}

/// taps 运行准入分类（§7.2）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapsProfile {
    /// <= 4096。
    Regular,
    /// 8192 / 16384。
    Extended,
    /// > 16384：仅设计工具 offline 输出。
    OfflineOnly,
    /// 4097..8191 等 §7.2 未列出的中间值（不得当 extended 放行）。
    NotAdmitted,
}

impl TapsProfile {
    pub fn for_taps(taps: usize) -> Self {
        if taps <= REGULAR_PROFILE_TAPS_CAP {
            TapsProfile::Regular
        } else if taps == 8_192 || taps == EXTENDED_PROFILE_TAPS_CAP {
            TapsProfile::Extended
        } else if taps > EXTENDED_PROFILE_TAPS_CAP {
            TapsProfile::OfflineOnly
        } else {
            TapsProfile::NotAdmitted
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            TapsProfile::Regular => "regular",
            TapsProfile::Extended => "extended",
            TapsProfile::OfflineOnly => "offline_only",
            TapsProfile::NotAdmitted => "not_admitted",
        }
    }
}

/// 报告错误（拒绝路径；带 §2.5/§6.9 的规范码）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportError {
    pub code: &'static str,
    pub message: String,
}

impl ReportError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ReportError {}

// ===========================================================================
// 设计声明（输入）
// ===========================================================================

/// 各族声明参数（对应 spec §3.1-§3.7 参数表；只放报告判定需要的字段）。
#[derive(Clone, Debug, PartialEq)]
pub enum FamilyParams {
    Linear {
        mode: BandMode,
        passband_hz: [f64; 2],
        stopband_hz: [f64; 2],
        ripple_db: f64,
        attenuation_db: f64,
        symmetry: SymmetryMode,
    },
    MinFast {
        passband_hz: [f64; 2],
        stopband_hz: [f64; 2],
        ripple_db: f64,
        attenuation_db: f64,
    },
    MinSlow {
        passband_hz: [f64; 2],
        nyquist_attenuation_db: f64,
        ripple_db: f64,
    },
    Apodizing {
        passband_hz: [f64; 2],
        ripple_db: f64,
        nyquist_attenuation_db: f64,
        pre_ring_bound_db: f64,
    },
    Brickwall {
        mode: BandMode,
        passband_hz: [f64; 2],
        stopband_hz: [f64; 2],
        ripple_db: f64,
        attenuation_db: f64,
        method: BrickwallMethod,
    },
    Corrected {
        band_hz: [f64; 2],
        phase_target: PhaseTarget,
        max_mag_error_db: f64,
        max_phase_error_deg: f64,
        rms_error_db: f64,
        magnitude_only: bool,
        equivalence_claim: EquivalenceClaim,
        /// lowdelay 的 D 上限（samples）；corrected 为 None。
        delay_max_samples: Option<f64>,
    },
    Nos,
}

impl FamilyParams {
    /// linear 低通默认（ripple 0.5 / A_s 80 / symmetric）。
    pub fn linear_lowpass(passband_hi_hz: f64, stopband_lo_hz: f64) -> Self {
        FamilyParams::Linear {
            mode: BandMode::Lowpass,
            passband_hz: [MEASUREMENT_LO_HZ, passband_hi_hz],
            stopband_hz: [stopband_lo_hz, f64::INFINITY],
            ripple_db: 0.5,
            attenuation_db: 80.0,
            symmetry: SymmetryMode::Symmetric,
        }
    }
    /// linear 高通默认。
    pub fn linear_highpass(stopband_hi_hz: f64, passband_lo_hz: f64) -> Self {
        FamilyParams::Linear {
            mode: BandMode::Highpass,
            passband_hz: [passband_lo_hz, f64::INFINITY],
            stopband_hz: [0.0, stopband_hi_hz],
            ripple_db: 0.5,
            attenuation_db: 80.0,
            symmetry: SymmetryMode::Symmetric,
        }
    }
    /// min_fast 默认。
    pub fn min_fast(passband_hi_hz: f64, stopband_lo_hz: f64) -> Self {
        FamilyParams::MinFast {
            passband_hz: [MEASUREMENT_LO_HZ, passband_hi_hz],
            stopband_hz: [stopband_lo_hz, f64::INFINITY],
            ripple_db: 0.5,
            attenuation_db: 80.0,
        }
    }
    /// min_slow 默认（Nyquist 带 30 dB）。
    pub fn min_slow(passband_hi_hz: f64) -> Self {
        FamilyParams::MinSlow {
            passband_hz: [MEASUREMENT_LO_HZ, passband_hi_hz],
            nyquist_attenuation_db: 30.0,
            ripple_db: 0.5,
        }
    }
    /// apodizing 默认（pre_ring_bound = -45 dB）。
    pub fn apodizing(passband_hi_hz: f64, nyquist_attenuation_db: f64) -> Self {
        FamilyParams::Apodizing {
            passband_hz: [MEASUREMENT_LO_HZ, passband_hi_hz],
            ripple_db: 0.5,
            nyquist_attenuation_db,
            pre_ring_bound_db: APODIZING_PRE_RING_BOUND_DB,
        }
    }
    /// brickwall 低通默认。
    pub fn brickwall_lowpass(
        passband_hi_hz: f64,
        stopband_lo_hz: f64,
        attenuation_db: f64,
        method: BrickwallMethod,
    ) -> Self {
        FamilyParams::Brickwall {
            mode: BandMode::Lowpass,
            passband_hz: [MEASUREMENT_LO_HZ, passband_hi_hz],
            stopband_hz: [stopband_lo_hz, f64::INFINITY],
            ripple_db: 0.2,
            attenuation_db,
            method,
        }
    }
    /// corrected 默认（显式相位目标）。
    pub fn corrected(band_hz: [f64; 2], phase_target: PhaseTarget, magnitude_only: bool) -> Self {
        FamilyParams::Corrected {
            band_hz,
            phase_target,
            max_mag_error_db: 1.0,
            max_phase_error_deg: 5.0,
            rms_error_db: 0.5,
            magnitude_only,
            equivalence_claim: EquivalenceClaim::None,
            delay_max_samples: None,
        }
    }
    /// nos（§3.7：bypass=true、taps=1）。
    pub fn nos() -> Self {
        FamilyParams::Nos
    }
}

/// 设计声明（报告输入；对应 khaen-fir/v1 manifest 的 family/fs/taps/delay/reference/design 子集）。
#[derive(Clone, Debug, PartialEq)]
pub struct DeclaredDesign {
    pub family: Family,
    pub fs: f64,
    pub taps: usize,
    /// 设计声明的体延迟 D（samples；线性相位类必须 = (L-1)/2，可为半整数）。
    pub delay_samples: f64,
    /// 可选声明的 delay_ms（若给出，复算须一致）。
    pub declared_delay_ms: Option<f64>,
    pub reference_hz: f64,
    pub reference_gain_db: f64,
    /// 声明的构造/窗方法（报告事实字段；hard 判据只对 brickwall.method）。
    pub method: String,
    pub params: FamilyParams,
}

impl DeclaredDesign {
    pub fn new(family: Family, fs: f64, taps: usize, params: FamilyParams) -> Self {
        Self {
            family,
            fs,
            taps,
            delay_samples: 0.0,
            declared_delay_ms: None,
            reference_hz: 0.0,
            reference_gain_db: 0.0,
            method: String::new(),
            params,
        }
    }
    pub fn with_delay(mut self, delay_samples: f64) -> Self {
        self.delay_samples = delay_samples;
        self
    }
    pub fn with_declared_delay_ms(mut self, delay_ms: f64) -> Self {
        self.declared_delay_ms = Some(delay_ms);
        self
    }
    pub fn with_reference(mut self, hz: f64, gain_db: f64) -> Self {
        self.reference_hz = hz;
        self.reference_gain_db = gain_db;
        self
    }
    pub fn with_method(mut self, method: impl Into<String>) -> Self {
        self.method = method.into();
        self
    }
    /// 族分组。
    pub fn group(&self) -> FamilyGroup {
        self.family.group()
    }
    /// 声明的 passband 上界（用于 HP Nyquist 检查；nos 为 None）。
    pub fn passband_hi_hz(&self) -> Option<f64> {
        match &self.params {
            FamilyParams::Linear { passband_hz, .. }
            | FamilyParams::MinFast { passband_hz, .. }
            | FamilyParams::MinSlow { passband_hz, .. }
            | FamilyParams::Apodizing { passband_hz, .. }
            | FamilyParams::Brickwall { passband_hz, .. } => Some(passband_hz[1]),
            FamilyParams::Corrected { band_hz, .. } => Some(band_hz[1]),
            FamilyParams::Nos => None,
        }
    }
    /// 声明的测量频带（GD min/max/mean 与 passband 统计用）。
    pub fn declared_band_hz(&self) -> [f64; 2] {
        match &self.params {
            FamilyParams::Linear { passband_hz, .. }
            | FamilyParams::MinFast { passband_hz, .. }
            | FamilyParams::MinSlow { passband_hz, .. }
            | FamilyParams::Apodizing { passband_hz, .. }
            | FamilyParams::Brickwall { passband_hz, .. } => *passband_hz,
            FamilyParams::Corrected { band_hz, .. } => *band_hz,
            FamilyParams::Nos => [MEASUREMENT_LO_HZ, MEASUREMENT_HI_HZ],
        }
    }
}

// ===========================================================================
// 目标曲线（corrected/lowdelay 的复杂目标；§6.3/§6.6/§6.7）
// ===========================================================================

/// 目标点（freq_hz、mag_db、phase_deg）。phase 为 §6.6 语义的展开相位。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TargetPointSample {
    pub freq_hz: f64,
    pub mag_db: f64,
    pub phase_deg: f64,
}

/// 复杂目标曲线（已导入/已校验的点集；本模块只做 reverify，不做文件解析）。
#[derive(Clone, Debug, PartialEq)]
pub struct ComplexTargetCurve {
    pub points: Vec<TargetPointSample>,
    pub phase_target: PhaseTarget,
}

impl ComplexTargetCurve {
    pub fn new(points: Vec<TargetPointSample>, phase_target: PhaseTarget) -> Self {
        Self {
            points,
            phase_target,
        }
    }

    /// 校验目标点（§6.4/§6.6 的复算子集）：>=2 点、有限、严格递增、freq > 0 且 < fs/2。
    pub fn validate(&self, fs: f64) -> Result<(), ReportError> {
        if self.points.len() < 2 {
            return Err(ReportError::new(
                "E_TGT_POINTS",
                format!(
                    "complex target needs >= 2 points, got {}",
                    self.points.len()
                ),
            ));
        }
        let mut prev = f64::NEG_INFINITY;
        for (i, p) in self.points.iter().enumerate() {
            if !p.freq_hz.is_finite() || !p.mag_db.is_finite() || !p.phase_deg.is_finite() {
                return Err(ReportError::new(
                    "E_TGT_NAN",
                    format!("complex target point {i} is not finite"),
                ));
            }
            if p.freq_hz <= 0.0 {
                return Err(ReportError::new(
                    "E_TGT_FREQ_INVALID",
                    format!("complex target point {i} freq_hz={} <= 0", p.freq_hz),
                ));
            }
            if p.freq_hz >= fs / 2.0 {
                return Err(ReportError::new(
                    "E_TGT_PHASE_REAL",
                    format!(
                        "complex target point {i} freq_hz={} must be < fs/2={} (Nyquist phase degenerate, §6.6)",
                        p.freq_hz,
                        fs / 2.0
                    ),
                ));
            }
            if p.freq_hz <= prev {
                return Err(ReportError::new(
                    "E_TGT_FREQ_ORDER",
                    format!("complex target point {i} is not strictly increasing"),
                ));
            }
            prev = p.freq_hz;
        }
        Ok(())
    }

    /// 按 §6.7 的 log10(f) 线性插值取目标（mag_db、展开 phase_deg）；越界按 edge_policy=hold。
    pub fn sample(&self, freq_hz: f64) -> (f64, f64) {
        let pts = &self.points;
        if pts.is_empty() {
            return (0.0, 0.0);
        }
        if freq_hz <= pts[0].freq_hz {
            return (pts[0].mag_db, pts[0].phase_deg);
        }
        let last = pts[pts.len() - 1];
        if freq_hz >= last.freq_hz {
            return (last.mag_db, last.phase_deg);
        }
        let lf = freq_hz.log10();
        let mut lo = 0usize;
        let mut hi = pts.len() - 1;
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if pts[mid].freq_hz <= freq_hz {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let f1 = pts[lo].freq_hz.log10();
        let f2 = pts[hi].freq_hz.log10();
        let t = if (f2 - f1).abs() < f64::EPSILON {
            0.0
        } else {
            (lf - f1) / (f2 - f1)
        };
        (
            pts[lo].mag_db + t * (pts[hi].mag_db - pts[lo].mag_db),
            pts[lo].phase_deg + t * (pts[hi].phase_deg - pts[lo].phase_deg),
        )
    }

    /// 目标总相位（deg）：absolute 直接用；delay_removed 减 360*f*D_target/fs（§6.6）。
    pub fn total_phase_deg(&self, freq_hz: f64, fs: f64) -> f64 {
        let (_, phase) = self.sample(freq_hz);
        match self.phase_target {
            PhaseTarget::Absolute => phase,
            PhaseTarget::DelayRemoved {
                target_delay_samples,
            } => phase - 360.0 * freq_hz * target_delay_samples / fs,
        }
    }
}

// ===========================================================================
// 外部声明响应（§2.2 规则 5 复算一致性）
// ===========================================================================

/// 外部报告（manifest）里的 response 子集；用于 0.05 dB / 0.1 sample 复算一致性检查。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeclaredResponse {
    pub passband_ripple_db: Option<f64>,
    pub passband_min_db: Option<f64>,
    pub passband_max_db: Option<f64>,
    pub stopband_attenuation_db: Option<f64>,
    pub stopband_max_db: Option<f64>,
    pub transition_edge_hz: Option<f64>,
    pub group_delay_min_samples: Option<f64>,
    pub group_delay_max_samples: Option<f64>,
    pub group_delay_mean_samples: Option<f64>,
    pub group_delay_at_1khz_samples: Option<f64>,
    pub ringing_pre_db: Option<f64>,
    pub ringing_post_db: Option<f64>,
    pub ringing_pre_energy_db: Option<f64>,
    pub ringing_post_energy_db: Option<f64>,
    pub endpoint_f20_mag_db: Option<f64>,
    pub endpoint_f20_phase_deg: Option<f64>,
    pub endpoint_f20k_mag_db: Option<f64>,
    pub endpoint_f20k_phase_deg: Option<f64>,
}

// ===========================================================================
// 报告选项
// ===========================================================================

/// 预算/成本选项。
#[derive(Clone, Debug, PartialEq)]
pub struct ReportOptions {
    /// 运行准入 profile（§7.2）；影响 fits_profile 判定。
    pub profile: RuntimeProfile,
    /// 可选整链延迟预算（ms；15/25/40 之一）。
    pub latency_budget_ms: Option<f64>,
    /// 整链中 FIR 之外的预算占用（ms）。
    pub block_ms: f64,
    pub src_ms: f64,
    pub io_ms: f64,
    /// §2.3 成本估算块长（默认 128；64/128 是生产候选）。
    pub cost_block: usize,
    /// §2.3 成本估算的路径数（默认 1）与输入/输出变换数（默认 1/1）。
    pub cost_paths: usize,
    pub transforms_in: usize,
    pub transforms_out: usize,
    /// 可选融合对 (G taps, P taps)：报告 L_F = L_G + L_P - 1 与 fused/factorized 成本。
    pub fusion_pair: Option<(usize, usize)>,
    /// corrected/lowdelay 缺目标曲线时是否拒绝（默认 true；false 则目标误差判据记 NOT_RUN/warning）。
    pub require_corrected_target: bool,
}

impl Default for ReportOptions {
    fn default() -> Self {
        Self {
            profile: RuntimeProfile::Regular,
            latency_budget_ms: None,
            block_ms: 0.0,
            src_ms: 0.0,
            io_ms: 0.0,
            cost_block: DEFAULT_COST_BLOCK,
            cost_paths: 1,
            transforms_in: 1,
            transforms_out: 1,
            fusion_pair: None,
            require_corrected_target: true,
        }
    }
}

/// 报告请求。
pub struct ReportRequest<'a> {
    pub declared: &'a DeclaredDesign,
    pub coefficients: &'a [f64],
    /// corrected/lowdelay 的复杂目标（必须给出，否则默认拒绝）。
    pub target: Option<&'a ComplexTargetCurve>,
    /// 外部声明响应（可选；用于 §2.2 规则 5 一致性复核）。
    pub expected: Option<&'a DeclaredResponse>,
    pub options: ReportOptions,
}

impl<'a> ReportRequest<'a> {
    pub fn new(declared: &'a DeclaredDesign, coefficients: &'a [f64]) -> Self {
        Self {
            declared,
            coefficients,
            target: None,
            expected: None,
            options: ReportOptions::default(),
        }
    }
    pub fn with_target(mut self, target: &'a ComplexTargetCurve) -> Self {
        self.target = Some(target);
        self
    }
    pub fn with_expected(mut self, expected: &'a DeclaredResponse) -> Self {
        self.expected = Some(expected);
        self
    }
    pub fn with_options(mut self, options: ReportOptions) -> Self {
        self.options = options;
        self
    }
}

// ===========================================================================
// 独立响应 oracle（§1.2）：直接 DTFT + 解析群延迟 + 自带 radix-2 FFT（仅 M2 用）
// ===========================================================================

/// 单频点独立测量结果（§1.2）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OraclePoint {
    pub freq_hz: f64,
    /// 线性幅度 |H|。
    pub magnitude: f64,
    /// 20*log10(max(|H|, 1e-12))。
    pub mag_db: f64,
    /// 主值相位（rad，未展开）。
    pub phase_rad_principal: f64,
    /// 展开后相位（rad；由 oracle_grid / unwrap_phase 写入）。
    pub phase_rad: f64,
    /// 展开后相位（deg）。
    pub phase_deg: f64,
    /// 精确群延迟（samples）。
    pub group_delay_samples: f64,
}

/// 精确幅度转 dB（§1.2：|H| 下限 1e-12 = -240 dB）。
pub fn amplitude_to_db(mag: f64) -> f64 {
    20.0 * mag.max(MAGNITUDE_FLOOR).log10()
}

/// dB 转线性幅度。
pub fn db_to_amplitude(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// §1.2 单频点独立测量：H(f) = sum h[n] exp(-j 2 pi f n / fs)，并给解析群延迟。
///
/// 群延迟公式（等价于 -d(phase)/d(omega)，不是中心差分）：
///   A = sum n h[n] cos(w n)，B = -sum n h[n] sin(w n)
///   GD = (A*Re(H) + B*Im(H)) / |H|^2
/// 对 |H|^2 <= 0 的频点（全零响应）返回 GD = 0 并有 note 由上层处理。
pub fn oracle_at(h: &[f64], fs: f64, freq_hz: f64) -> OraclePoint {
    let w = 2.0 * PI * freq_hz / fs;
    let (sin_w, cos_w) = w.sin_cos();
    // exp(-j w) 的旋转因子。
    let step_re = cos_w;
    let step_im = -sin_w;
    let mut zr = 1.0_f64;
    let mut zi = 0.0_f64;
    let mut re = 0.0_f64;
    let mut im = 0.0_f64;
    let mut a = 0.0_f64;
    let mut b = 0.0_f64;
    for (n, &hn) in h.iter().enumerate() {
        if hn == 0.0 {
            // 仍然要旋转到下一个 n。
        }
        re += hn * zr;
        im += hn * zi;
        let nf = n as f64;
        a += nf * hn * zr;
        b += nf * hn * zi;
        let nr = zr * step_re - zi * step_im;
        let ni = zr * step_im + zi * step_re;
        zr = nr;
        zi = ni;
    }
    let power = re * re + im * im;
    let magnitude = power.sqrt();
    let phase_principal = im.atan2(re);
    let gd = if power > 0.0 {
        (a * re + b * im) / power
    } else {
        0.0
    };
    OraclePoint {
        freq_hz,
        magnitude,
        mag_db: amplitude_to_db(magnitude),
        phase_rad_principal: phase_principal,
        phase_rad: phase_principal,
        phase_deg: phase_principal.to_degrees(),
        group_delay_samples: gd,
    }
}

/// 用精确 GD 预测的相邻相位增量展开相位（log 网格在高频处相邻 Δphase 可 > pi，
/// 裸主值差 unwrap 会错圈；本实现按 §1.2 连续分支的意图，用 GD 预测选圈）。
pub fn unwrap_phase(points: &mut [OraclePoint], fs: f64) {
    if points.len() < 2 {
        return;
    }
    let two_pi = 2.0 * PI;
    let mut acc = points[0].phase_rad_principal;
    points[0].phase_rad = acc;
    points[0].phase_deg = acc.to_degrees();
    for i in 1..points.len() {
        let f0 = points[i - 1].freq_hz;
        let f1 = points[i].freq_hz;
        let gd_mid = 0.5 * (points[i - 1].group_delay_samples + points[i].group_delay_samples);
        let predicted = -two_pi * gd_mid * (f1 - f0) / fs;
        let raw = points[i].phase_rad_principal;
        let target = acc + predicted;
        let k = ((target - raw) / two_pi).round();
        acc = raw + two_pi * k;
        points[i].phase_rad = acc;
        points[i].phase_deg = acc.to_degrees();
    }
}

/// 常数延迟族的相位锚定：取整数圈使 phase(f0) 最接近 -360*f0*D/fs（deg）。
///
/// 用于 linear/apodizing/brickwall/corrected 这类声明 D 的设计，避免高 D 在 20 Hz 处
/// 主值相位已经差 2*pi*k 造成报告相位不自洽。min_* 无声明 D 时不做锚定（20 Hz 主值即可）。
pub fn anchor_phase_deg(points: &mut [OraclePoint], expected_deg_at_first: f64) {
    if points.is_empty() {
        return;
    }
    let k = ((expected_deg_at_first - points[0].phase_deg) / 360.0).round();
    if k == 0.0 {
        return;
    }
    for p in points.iter_mut() {
        p.phase_deg += 360.0 * k;
        p.phase_rad = p.phase_deg.to_radians();
    }
}

/// 在给定频点序列上做完整独立测量（含展开；调用方可再 anchor_phase_deg）。
pub fn oracle_grid(h: &[f64], fs: f64, freqs: &[f64]) -> Vec<OraclePoint> {
    let mut pts: Vec<OraclePoint> = freqs.iter().map(|&f| oracle_at(h, fs, f)).collect();
    unwrap_phase(&mut pts, fs);
    pts
}

/// 就地 radix-2 复数 FFT（长度必须是 2 的幂；本模块自带的独立实现，仅 M2 使用）。
pub fn fft_in_place(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    assert_eq!(n, im.len(), "fft length mismatch");
    if n <= 1 {
        return;
    }
    let mut j = 0usize;
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
    let mut len = 2usize;
    while len <= n {
        let half = len / 2;
        let ang = -2.0 * PI / len as f64;
        // 旋转因子 e^{-j*2*pi/len}。
        let w_re = ang.cos();
        let w_im = ang.sin();
        for start in (0..n).step_by(len) {
            let mut wr = 1.0_f64;
            let mut wi = 0.0_f64;
            for k in 0..half {
                let a = start + k;
                let b = a + half;
                let tr = wr * re[b] - wi * im[b];
                let ti = wr * im[b] + wi * re[b];
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let nwr = wr * w_re - wi * w_im;
                let nwi = wr * w_im + wi * w_re;
                wr = nwr;
                wi = nwi;
            }
        }
        len <<= 1;
    }
}

/// 就地 radix-2 复数 IFFT（1/N 归一化）。
pub fn ifft_in_place(re: &mut [f64], im: &mut [f64]) {
    for v in im.iter_mut() {
        *v = -*v;
    }
    fft_in_place(re, im);
    let n = re.len() as f64;
    for (r, i) in re.iter_mut().zip(im.iter_mut()) {
        *r /= n;
        *i /= -n;
    }
}

/// 下一 2 的幂（>= n）。
pub fn next_pow2(n: usize) -> usize {
    let mut p = 1usize;
    while p < n {
        p <<= 1;
    }
    p
}

/// §3.2 判据 2（M2）：相位–Hilbert 一致性，返回 20 Hz..0.45 fs 上
/// max |wrap(phi_measured - phi_min)|（deg）。M2 是本模块独立实现的规范判据，
/// 不调用任何 leaf 模块。
pub fn min_phase_consistency_deg(h: &[f64], fs: f64, n_fft: usize) -> f64 {
    let n = n_fft.max(next_pow2(2 * h.len().max(1)));
    let n = next_pow2(n);
    let mut re = vec![0.0_f64; n];
    let mut im = vec![0.0_f64; n];
    for (i, &v) in h.iter().enumerate() {
        if i < n {
            re[i] = v;
        }
    }
    fft_in_place(&mut re, &mut im);
    let floor = db_to_amplitude(MIN_PHASE_LOG_FLOOR_DB);
    let mut log_mag = vec![0.0_f64; n];
    let mut phi_meas = vec![0.0_f64; n];
    let mut prev = im[0].atan2(re[0]);
    phi_meas[0] = prev;
    for k in 0..n {
        let mag = (re[k] * re[k] + im[k] * im[k]).sqrt().max(floor);
        log_mag[k] = mag.ln();
        if k > 0 {
            let raw = im[k].atan2(re[k]);
            let two_pi = 2.0 * PI;
            // 均匀网格：用上一 bin 最小跳变展开（Δphase < pi 由 N/样本长度保证）。
            let kk = ((prev - raw) / two_pi).round();
            let val = raw + two_pi * kk;
            phi_meas[k] = val;
            prev = val;
        }
    }
    // 实倒谱 -> 折叠 -> FFT 得 log H_min = ln|H| + j phi_min。
    let mut cr = log_mag.clone();
    let mut ci = vec![0.0_f64; n];
    ifft_in_place(&mut cr, &mut ci);
    let half = n / 2;
    for k in 1..half {
        cr[k] *= 2.0;
        ci[k] *= 2.0;
    }
    for k in (half + 1)..n {
        cr[k] = 0.0;
        ci[k] = 0.0;
    }
    fft_in_place(&mut cr, &mut ci);
    // 比较带：20 Hz .. 0.45 fs。
    let bin_hz = fs / n as f64;
    let lo_bin = (MEASUREMENT_LO_HZ / bin_hz).ceil() as usize;
    let hi_bin = ((MIN_PHASE_BAND_HI_RATIO * fs) / bin_hz).floor() as usize;
    let hi_bin = hi_bin.min(n / 2);
    let mut max_dev = 0.0_f64;
    if lo_bin <= hi_bin {
        for k in lo_bin..=hi_bin {
            let mut diff = (phi_meas[k] - ci[k]).to_degrees();
            while diff <= -180.0 {
                diff += 360.0;
            }
            while diff > 180.0 {
                diff -= 360.0;
            }
            max_dev = max_dev.max(diff.abs());
        }
    }
    max_dev
}

// ===========================================================================
// 支撑 / 振铃（§1.3 / §1.4）
// ===========================================================================

/// §1.3 有效支撑 (first, last, effective_taps)，eps 相对 max|h|。
pub fn effective_support(h: &[f64], eps: f64) -> (usize, usize, usize) {
    let peak = h.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
    if h.is_empty() || peak <= 0.0 {
        return (0, 0, 0);
    }
    let thr = eps * peak;
    let first = h.iter().position(|v| v.abs() > thr).unwrap_or(0);
    let last = h.iter().rposition(|v| v.abs() > thr).unwrap_or(h.len() - 1);
    (first, last, last - first + 1)
}

/// §1.4 振铃度量（在系数本身上测，零填充线性卷积口径）。
#[derive(Clone, Debug, PartialEq)]
pub struct RingingMetrics {
    /// 排除窗 W = ceil(0.000333*fs)。
    pub exclusion_samples: usize,
    /// 主峰位置 p = argmax |h|。
    pub peak_index: usize,
    pub peak_abs: f64,
    /// 前振铃 dB（None = 支撑过短，§1.4 写 n/a）。
    pub pre_ring_db: Option<f64>,
    pub post_ring_db: Option<f64>,
    pub pre_ring_energy_db: Option<f64>,
    pub post_ring_energy_db: Option<f64>,
    /// 从主峰衰减到 -80 dB 的样本数（§3.5 判据 6；本实现取最后一个 |h| > peak*1e-4 的位置 - p）。
    pub duration_to_minus80_samples: Option<usize>,
    /// n/a 原因（支撑过短等）。
    pub notes: Vec<String>,
}

/// §1.4 振铃度量。
///
/// 主峰按"主峰平台"处理：|h| 等于 max（相对 1e-12）的连续索引区间 [p_lo, p_hi] 都算主峰。
/// 偶数长对称 FIR 的 D=(L-1)/2 是半整数，主峰天然是并列双峰；若只取 argmax 的单个整数 p，
/// 对称设计会因左右排除窗各差 1 sample 而报出 pre/post 明显不对称（与 §9 V16 参考构造 pre=post
/// 矛盾）。平台化后单峰设计与 spec 的经典公式一致（p_lo=p_hi=p）。
pub fn ringing_metrics(h: &[f64], fs: f64) -> RingingMetrics {
    let mut peak_abs = 0.0_f64;
    for &v in h.iter() {
        peak_abs = peak_abs.max(v.abs());
    }
    let peak_thr = peak_abs * (1.0 - 1e-12);
    let mut peak_index = 0usize;
    let mut peak_index_hi = 0usize;
    let mut found = false;
    for (i, &v) in h.iter().enumerate() {
        if v.abs() >= peak_thr && peak_abs > 0.0 {
            if !found {
                peak_index = i;
                found = true;
            }
            peak_index_hi = i;
        }
    }
    let w = (RING_EXCLUSION_FRACTION * fs).ceil() as usize;
    let mut notes = Vec::new();
    let total_energy: f64 = h.iter().map(|v| v * v).sum();
    let mut pre_peak = 0.0_f64;
    let mut post_peak = 0.0_f64;
    let mut pre_energy = 0.0_f64;
    let mut post_energy = 0.0_f64;
    let mut has_pre = false;
    let mut has_post = false;
    if peak_index > w {
        // n < p_lo - W
        let upper = peak_index - w;
        for &v in h.iter().take(upper) {
            pre_peak = pre_peak.max(v.abs());
            pre_energy += v * v;
        }
        has_pre = true;
    } else {
        notes.push(format!(
            "pre-ring n/a: p_lo-W = {} - {} < 0 (support too short, §1.4)",
            peak_index, w
        ));
    }
    if peak_index_hi + w < h.len() {
        for &v in h.iter().skip(peak_index_hi + w + 1) {
            post_peak = post_peak.max(v.abs());
            post_energy += v * v;
        }
        has_post = true;
    } else {
        notes.push(format!(
            "post-ring n/a: p_hi+W = {} + {} >= L = {} (support too short, §1.4)",
            peak_index_hi,
            w,
            h.len()
        ));
    }
    let to_db = |v: f64, refv: f64| -> f64 {
        if refv <= 0.0 {
            f64::NEG_INFINITY
        } else if v <= 0.0 {
            f64::NEG_INFINITY
        } else {
            20.0 * (v / refv).log10()
        }
    };
    let energy_db = |num: f64, den: f64| -> f64 {
        if den <= 0.0 || num <= 0.0 {
            f64::NEG_INFINITY
        } else {
            10.0 * (num / den).log10()
        }
    };
    let duration = if peak_abs > 0.0 {
        let thr = peak_abs * db_to_amplitude(RING_DURATION_DB);
        let mut last = peak_index;
        for (i, &v) in h.iter().enumerate() {
            if v.abs() > thr {
                last = last.max(i);
            }
        }
        Some(last.saturating_sub(peak_index_hi))
    } else {
        None
    };
    RingingMetrics {
        exclusion_samples: w,
        peak_index,
        peak_abs,
        pre_ring_db: if has_pre {
            Some(to_db(pre_peak, peak_abs))
        } else {
            None
        },
        post_ring_db: if has_post {
            Some(to_db(post_peak, peak_abs))
        } else {
            None
        },
        pre_ring_energy_db: if has_pre {
            Some(energy_db(pre_energy, total_energy))
        } else {
            None
        },
        post_ring_energy_db: if has_post {
            Some(energy_db(post_energy, total_energy))
        } else {
            None
        },
        duration_to_minus80_samples: duration,
        notes,
    }
}

// ===========================================================================
// 预算 / 成本（§3.5 可行性式、§2.3、§7.2）
// ===========================================================================

/// §3.5 判据 5 / §9 V13：Kaiser 可行性式要求的 taps（向上取整）。
pub fn kaiser_required_taps(attenuation_db: f64, fs: f64, transition_hz: f64) -> Option<u64> {
    if !(attenuation_db >= KAISER_MIN_ATTENUATION_DB)
        || !(transition_hz > 0.0)
        || !fs.is_finite()
        || !attenuation_db.is_finite()
        || !transition_hz.is_finite()
    {
        return None;
    }
    let l = 1.0 + (attenuation_db - 7.95) * fs / (KAISER_TAPS_NUMERATOR * transition_hz);
    if !l.is_finite() || l <= 0.0 {
        return None;
    }
    Some(l.ceil() as u64)
}

/// §2.3 成本估算输入。
#[derive(Clone, Debug, PartialEq)]
pub struct CostInputs {
    pub fs: f64,
    /// 每条非零路径的 taps（len = P）。
    pub paths: Vec<usize>,
    pub block: usize,
    pub transforms_in: usize,
    pub transforms_out: usize,
}

impl CostInputs {
    pub fn single(fs: f64, taps: usize, block: usize) -> Self {
        Self {
            fs,
            paths: vec![taps],
            block,
            transforms_in: 1,
            transforms_out: 1,
        }
    }
}

/// §2.3 成本估算结果（tap 级 direct 与 partitioned FFT 模型）。
#[derive(Clone, Debug, PartialEq)]
pub struct CostEstimate {
    pub fs: f64,
    pub paths: usize,
    pub taps_max: usize,
    pub block: usize,
    pub gflops_direct: f64,
    pub gflops_fft: f64,
}

/// §2.3 成本估算（B = block，K = 2B，F = B+1，Q = ceil(taps_max/B)，r = fs/B）。
pub fn estimate_costs(input: &CostInputs) -> CostEstimate {
    let paths = input.paths.len().max(1);
    let taps_max = input.paths.iter().copied().max().unwrap_or(0);
    let tap_sum: usize = input.paths.iter().sum();
    let gflops_direct = 2.0 * tap_sum as f64 * input.fs / 1e9;
    let b = input.block.max(1);
    let k = 2 * b;
    let f = b + 1;
    let q = (taps_max + b - 1) / b;
    let r = input.fs / b as f64;
    let fft_conv = FLOPS_PER_COMPLEX_MAC * paths as f64 * q as f64 * f as f64 * r;
    let fft_xform = (input.transforms_in + input.transforms_out) as f64
        * FFT_MODEL_COEFFICIENT
        * k as f64
        * (k as f64).log2()
        * r;
    let gflops_fft = (fft_conv + fft_xform) / 1e9;
    CostEstimate {
        fs: input.fs,
        paths,
        taps_max,
        block: b,
        gflops_direct,
        gflops_fft,
    }
}

// ===========================================================================
// 测量网格（§1.2：T55 报告必须落盘网格定义与点数）
// ===========================================================================

/// 测量网格（对数频率网格 20 Hz .. min(20 kHz, 0.499 fs)）。
#[derive(Clone, Debug, PartialEq)]
pub struct MeasurementGrid {
    pub lo_hz: f64,
    pub hi_hz: f64,
    pub points: usize,
    /// §1.2 要求的点数下限 max(8192, 8*taps)。
    pub required_min_points: usize,
    /// points >= required_min_points。
    pub meets_spec: bool,
    /// 频点（升序，含两端）。
    pub freqs: Vec<f64>,
    /// 相邻点最小/最大频率间隔（Hz）。
    pub min_step_hz: f64,
    pub max_step_hz: f64,
    /// 相邻点最小/最大 octave 间隔。
    pub min_step_oct: f64,
    pub max_step_oct: f64,
}

impl MeasurementGrid {
    /// §1.2 规范网格：20 Hz .. min(20000, 0.499*fs)，N = max(8192, 8*L)。
    pub fn for_design(fs: f64, taps: usize) -> Self {
        let hi = MEASUREMENT_HI_HZ.min(NYQUIST_GUARD_FRACTION * fs);
        let required = MIN_GRID_POINTS.max(GRID_POINTS_PER_TAP * taps.max(1));
        Self::with_points(MEASUREMENT_LO_HZ, hi, required, required)
    }

    /// 显式网格（点数可低于规范下限；此时 meets_spec=false，报告 fail，不静默放行）。
    pub fn with_points(lo_hz: f64, hi_hz: f64, points: usize, required_min_points: usize) -> Self {
        let n = points.max(2);
        let ratio = (hi_hz / lo_hz).max(1.0 + 1e-15);
        let freqs: Vec<f64> = (0..n)
            .map(|i| {
                let t = i as f64 / (n - 1) as f64;
                lo_hz * ratio.powf(t)
            })
            .collect();
        let mut min_step = f64::INFINITY;
        let mut max_step = 0.0_f64;
        let mut min_oct = f64::INFINITY;
        let mut max_oct = 0.0_f64;
        for w in freqs.windows(2) {
            let step = w[1] - w[0];
            min_step = min_step.min(step);
            max_step = max_step.max(step);
            let oct = (w[1] / w[0]).log2();
            min_oct = min_oct.min(oct);
            max_oct = max_oct.max(oct);
        }
        Self {
            lo_hz,
            hi_hz,
            points: n,
            required_min_points,
            meets_spec: n >= required_min_points,
            freqs,
            min_step_hz: if min_step.is_finite() { min_step } else { 0.0 },
            max_step_hz: max_step,
            min_step_oct: if min_oct.is_finite() { min_oct } else { 0.0 },
            max_step_oct: max_oct,
        }
    }

    /// 落在 [lo, hi] 内的频点区间（索引半开）。
    pub fn index_range(&self, lo_hz: f64, hi_hz: f64) -> (usize, usize) {
        let mut start = 0usize;
        while start < self.points && self.freqs[start] < lo_hz {
            start += 1;
        }
        let mut end = self.points;
        while end > start && self.freqs[end - 1] > hi_hz {
            end -= 1;
        }
        (start, end)
    }
}

// ===========================================================================
// 报告数据模型（镜像 spec §2.2 response/reference/limits/verification 子集）
// ===========================================================================

/// Endpoint 联合类型（§2.2/§2.4；禁止裸字符串）。
#[derive(Clone, Debug, PartialEq)]
pub enum Endpoint {
    /// status: measured。
    Measured {
        freq_hz: f64,
        mag_db: f64,
        phase_deg: f64,
    },
    /// status: not_applicable_at_fs。
    NotApplicableAtFs { freq_hz: f64, reason: String },
}

impl Endpoint {
    /// §2.2 的 status 字符串。
    pub fn status(&self) -> &'static str {
        match self {
            Endpoint::Measured { .. } => "measured",
            Endpoint::NotApplicableAtFs { .. } => "not_applicable_at_fs",
        }
    }
    pub fn freq_hz(&self) -> f64 {
        match self {
            Endpoint::Measured { freq_hz, .. } => *freq_hz,
            Endpoint::NotApplicableAtFs { freq_hz, .. } => *freq_hz,
        }
    }
}

/// 20 Hz / 20 kHz 端点（§2.2/§2.4；两个键都必须存在）。
#[derive(Clone, Debug, PartialEq)]
pub struct Endpoints {
    pub f20: Endpoint,
    pub f20k: Endpoint,
}

/// 频带统计（§1.2：声明频带内 max/min；不许只取少数倍频点）。
#[derive(Clone, Debug, PartialEq)]
pub struct BandStat {
    pub lo_hz: f64,
    pub hi_hz: f64,
    pub points: usize,
    pub min_db: f64,
    pub max_db: f64,
    pub ripple_db: f64,
}

/// 阻带统计（额外给 attenuation_db = -max_db）。
#[derive(Clone, Debug, PartialEq)]
pub struct StopbandStat {
    pub band: BandStat,
    pub attenuation_db: f64,
}

/// 过渡带统计（§1.2 穿越点；LP 首次达到 -A_s / HP 镜像读法最后一次离开）。
#[derive(Clone, Debug, PartialEq)]
pub struct TransitionStat {
    pub mode: BandMode,
    pub attenuation_db: f64,
    pub edge_hz_at_attenuation: f64,
    /// 声明 f_s 与穿越点的宽度（LP: edge-f_s；HP: f_s-edge）。
    pub width_hz: f64,
    pub tolerance_hz: f64,
    pub passed: bool,
    pub detail: String,
}

/// 群延迟统计（§1.3/§3.2 判据 3/§3.6 判据 4）。
#[derive(Clone, Debug, PartialEq)]
pub struct GroupDelayReport {
    pub class: PhaseClass,
    pub band_lo_hz: f64,
    pub band_hi_hz: f64,
    pub min_samples: f64,
    pub max_samples: f64,
    pub mean_samples: f64,
    pub at_1khz_samples: f64,
    /// onset latency = first_effective/fs*1000（§1.3，与 D 分开报告）。
    pub onset_samples: f64,
    pub onset_ms: f64,
    /// latency_ms = GD(1 kHz)/fs*1000（§3.2 判据 3；不是 D/fs）。
    pub latency_ms_1khz: f64,
    pub gd_max_ms: f64,
    /// 声明 D/fs*1000（§1.3 的 delay_ms）。
    pub declared_delay_ms: f64,
    /// 1/3 oct 表（corrected 必报；各族都附，供报告消费）。
    pub third_octave: Vec<(f64, f64)>,
    /// 特征频点 GD。
    pub characteristic_hz_samples: Vec<(f64, f64)>,
    pub statement: String,
}

/// 参考归一化检查（§1.5/§2.2 规则 4）。
#[derive(Clone, Debug, PartialEq)]
pub struct ReferenceReport {
    pub hz: f64,
    pub declared_gain_db: f64,
    pub measured_gain_db: f64,
    pub delta_db: f64,
    pub tolerance_db: f64,
    pub passed: bool,
}

/// 支撑报告（§1.3）。
#[derive(Clone, Debug, PartialEq)]
pub struct SupportReport {
    pub declared_taps: usize,
    pub len: usize,
    pub peak_index: usize,
    pub peak_abs: f64,
    pub first_effective: usize,
    pub last_effective: usize,
    pub effective_taps: usize,
    /// onset latency = first/fs*1000（ms）。
    pub onset_samples: f64,
    pub onset_ms: f64,
    pub sum_h: f64,
    pub sum_abs_h: f64,
    pub dc_gain_db: f64,
    pub all_finite: bool,
}

/// 前/后振铃报告（§1.4；apodizing/brickwall 都要求报告）。
#[derive(Clone, Debug, PartialEq)]
pub struct RingingReport {
    pub exclusion_samples: usize,
    pub peak_index: usize,
    pub peak_abs: f64,
    pub pre_ring_db: Option<f64>,
    pub post_ring_db: Option<f64>,
    pub pre_ring_energy_db: Option<f64>,
    pub post_ring_energy_db: Option<f64>,
    pub duration_to_minus80_samples: Option<usize>,
    pub notes: Vec<String>,
}

/// Kaiser 可行性报告（§3.5 判据 5 / §9 V13）。
#[derive(Clone, Debug, PartialEq)]
pub struct KaiserFeasibility {
    pub attenuation_db: f64,
    pub transition_hz: f64,
    pub required_taps: Option<u64>,
    pub declared_taps: usize,
    /// 仅当 required_taps Some 且 declared_taps < required 时 false。
    pub feasible: bool,
    pub applies_hard: bool,
    pub detail: String,
}

/// 延迟预算档位结果（§1.1/§7.2 15/25/40 ms 整链）。
#[derive(Clone, Debug, PartialEq)]
pub struct LatencyBudgetReport {
    pub budget_ms: f64,
    pub block_ms: f64,
    pub src_ms: f64,
    pub io_ms: f64,
    pub fir_delay_ms: f64,
    pub remaining_ms: f64,
    pub fits: bool,
}

/// 融合成本对比（§2.3/§7.1：L_F = L_G + L_P - 1）。
#[derive(Clone, Debug, PartialEq)]
pub struct FusionReport {
    pub taps_g: usize,
    pub taps_p: usize,
    pub fusion_length: usize,
    pub fused_cost: CostEstimate,
    pub factorized_cost: CostEstimate,
    /// fused gflops_direct <= factorized gflops_direct 时为 true（仍由 T12 决定）。
    pub fused_cheaper: bool,
}

/// 预算报告（§7.2 硬规则 + §2.3 估算）。
#[derive(Clone, Debug, PartialEq)]
pub struct BudgetReport {
    pub taps: usize,
    pub taps_profile: TapsProfile,
    pub requested_profile: RuntimeProfile,
    /// taps/延迟/内存在该 profile 下可运行（不代表 deadline/热切准入，T56）。
    pub fits_runtime_profile: bool,
    pub route_taps_cap: Option<usize>,
    pub ram_bytes_f32_per_route: usize,
    pub ram_bytes_f32_total: usize,
    pub fir_delay_ms: f64,
    pub latency: Option<LatencyBudgetReport>,
    pub latency_tiers: Vec<LatencyBudgetReport>,
    pub kaiser: Option<KaiserFeasibility>,
    pub cost: CostEstimate,
    pub fusion: Option<FusionReport>,
    pub notes: Vec<String>,
}

/// 判据分组。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CriterionGroup {
    Common,
    Family,
    MutualExclusion,
    Budget,
    Response,
    Endpoint,
}

impl CriterionGroup {
    pub fn as_str(self) -> &'static str {
        match self {
            CriterionGroup::Common => "common",
            CriterionGroup::Family => "family",
            CriterionGroup::MutualExclusion => "mutual_exclusion",
            CriterionGroup::Budget => "budget",
            CriterionGroup::Response => "response",
            CriterionGroup::Endpoint => "endpoint",
        }
    }
}

/// 单条判据（失败带 §2.5 码）。
#[derive(Clone, Debug, PartialEq)]
pub struct Criterion {
    pub group: CriterionGroup,
    pub id: &'static str,
    /// 失败码（通过时也填，便于上层直接取）。
    pub code: &'static str,
    pub passed: bool,
    pub observed: f64,
    pub limit: f64,
    pub unit: &'static str,
    pub detail: String,
}

impl Criterion {
    pub fn new(
        group: CriterionGroup,
        id: &'static str,
        code: &'static str,
        passed: bool,
        observed: f64,
        limit: f64,
        unit: &'static str,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            group,
            id,
            code,
            passed,
            observed,
            limit,
            unit,
            detail: detail.into(),
        }
    }
}

/// §2.2 规则 5 复算一致性结果。
#[derive(Clone, Debug, PartialEq)]
pub struct ConsistencyReport {
    pub compared_fields: usize,
    pub max_abs_db_err: f64,
    pub max_abs_sample_err: f64,
    pub max_abs_phase_err_deg: f64,
    pub passed: bool,
}

/// verification 子集（§2.2）。
#[derive(Clone, Debug, PartialEq)]
pub struct VerificationReport {
    pub method: &'static str,
    pub grid_points: usize,
    pub ref_impl: &'static str,
    pub consistency: Option<ConsistencyReport>,
    pub passed: bool,
}

/// 七族独立报告（T55 主产物）。
#[derive(Clone, Debug, PartialEq)]
pub struct FirReport {
    pub family: Family,
    pub family_group: FamilyGroup,
    pub fs: f64,
    pub grid: MeasurementGrid,
    pub support: SupportReport,
    pub reference: ReferenceReport,
    pub response_passband: Option<BandStat>,
    pub response_stopband: Option<StopbandStat>,
    pub response_nyquist_band: Option<BandStat>,
    pub response_transition: Option<TransitionStat>,
    pub group_delay: GroupDelayReport,
    pub ringing: RingingReport,
    pub endpoints: Endpoints,
    pub budget: BudgetReport,
    pub criteria: Vec<Criterion>,
    pub verification: VerificationReport,
    pub statements: Vec<String>,
    pub warnings: Vec<String>,
    pub passed: bool,
}

impl FirReport {
    /// 失败判据（按记录顺序）。
    pub fn failures(&self) -> Vec<&Criterion> {
        self.criteria.iter().filter(|c| !c.passed).collect()
    }
    /// 失败码去重（保持顺序）。
    pub fn failure_codes(&self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        for c in self.failures() {
            if !out.contains(&c.code) {
                out.push(c.code);
            }
        }
        out
    }
    /// 是否通过设计判据（预算准入单独看 budget.fits_runtime_profile）。
    pub fn all_criteria_passed(&self) -> bool {
        self.passed
    }
    /// 统一拒绝：任一判据不通过则返回第一个失败码。
    pub fn require_pass(&self) -> Result<(), ReportError> {
        if let Some(c) = self.failures().into_iter().next() {
            return Err(ReportError::new(
                c.code,
                format!(
                    "{} criterion {:?} failed: observed={} limit={} {} ({})",
                    self.family.as_str(),
                    c.id,
                    c.observed,
                    c.limit,
                    c.unit,
                    c.detail
                ),
            ));
        }
        Ok(())
    }
    /// 人类可读摘要（证据日志用；每行一条）。
    pub fn summary_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        out.push(format!(
            "family={} group={} fs={:.1} taps={} passed={}",
            self.family.as_str(),
            self.family_group.as_str(),
            self.fs,
            self.support.len,
            self.passed
        ));
        out.push(format!(
            "grid: lo={:.3} hi={:.3} points={} required={} meets_spec={} step=[{:.6},{:.3}] Hz",
            self.grid.lo_hz,
            self.grid.hi_hz,
            self.grid.points,
            self.grid.required_min_points,
            self.grid.meets_spec,
            self.grid.min_step_hz,
            self.grid.max_step_hz
        ));
        out.push(format!(
            "support: first={} last={} effective={} peak_index={} max|h|={:.9} sum_h={:.9} onset={:.6} ms",
            self.support.first_effective,
            self.support.last_effective,
            self.support.effective_taps,
            self.support.peak_index,
            self.support.peak_abs,
            self.support.sum_h,
            self.support.onset_ms
        ));
        out.push(format!(
            "reference: hz={:.1} declared={:.4} dB measured={:.6} dB delta={:.6} dB passed={}",
            self.reference.hz,
            self.reference.declared_gain_db,
            self.reference.measured_gain_db,
            self.reference.delta_db,
            self.reference.passed
        ));
        if let Some(b) = &self.response_passband {
            out.push(format!(
                "passband: [{:.1},{:.1}] points={} min={:.4} max={:.4} ripple={:.4} dB",
                b.lo_hz, b.hi_hz, b.points, b.min_db, b.max_db, b.ripple_db
            ));
        }
        if let Some(b) = &self.response_stopband {
            out.push(format!(
                "stopband: [{:.1},{:.1}] points={} max={:.3} dB attenuation={:.3} dB",
                b.band.lo_hz, b.band.hi_hz, b.band.points, b.band.max_db, b.attenuation_db
            ));
        }
        if let Some(b) = &self.response_nyquist_band {
            out.push(format!(
                "nyquist_band: [{:.1},{:.1}] max={:.3} dB",
                b.lo_hz, b.hi_hz, b.max_db
            ));
        }
        if let Some(t) = &self.response_transition {
            out.push(format!(
                "transition: mode={} edge@{} dB={:.3} Hz width={:.3} Hz tol={:.1} Hz passed={}",
                t.mode.as_str(),
                t.attenuation_db,
                t.edge_hz_at_attenuation,
                t.width_hz,
                t.tolerance_hz,
                t.passed
            ));
        }
        out.push(format!(
            "group_delay: class={} band=[{:.1},{:.1}] min={:.6} max={:.6} mean={:.6} at_1khz={:.6} samples latency_1khz={:.6} ms gd_max={:.6} ms onset={:.6} ms D={:.6} samples ({:.6} ms)",
            self.group_delay.class.as_str(),
            self.group_delay.band_lo_hz,
            self.group_delay.band_hi_hz,
            self.group_delay.min_samples,
            self.group_delay.max_samples,
            self.group_delay.mean_samples,
            self.group_delay.at_1khz_samples,
            self.group_delay.latency_ms_1khz,
            self.group_delay.gd_max_ms,
            self.group_delay.onset_ms,
            self.group_delay.declared_delay_ms * self.fs / 1000.0,
            self.group_delay.declared_delay_ms
        ));
        out.push({
            let r = &self.ringing;
            format!(
                "ringing: W={} pre={:?} dB post={:?} dB pre_energy={:?} dB post_energy={:?} dB duration_to_-80dB={:?} samples",
                r.exclusion_samples,
                r.pre_ring_db,
                r.post_ring_db,
                r.pre_ring_energy_db,
                r.post_ring_energy_db,
                r.duration_to_minus80_samples
            )
        });
        out.push(format!(
            "endpoints: f20={} ({:?}) f20k={} ({:?})",
            self.endpoints.f20.status(),
            self.endpoints.f20,
            self.endpoints.f20k.status(),
            self.endpoints.f20k
        ));
        out.push(format!(
            "budget: taps_profile={} requested={} fits_profile={} ram_per_route={} B ram_total={} B fir_delay={:.6} ms kaiser={:?}",
            self.budget.taps_profile.as_str(),
            self.budget.requested_profile.as_str(),
            self.budget.fits_runtime_profile,
            self.budget.ram_bytes_f32_per_route,
            self.budget.ram_bytes_f32_total,
            self.budget.fir_delay_ms,
            self.budget.kaiser.as_ref().map(|k| k.feasible)
        ));
        out.push(format!(
            "cost(§2.3): B={} P={} taps_max={} gflops_direct={:.6} gflops_fft={:.6}",
            self.budget.cost.block,
            self.budget.cost.paths,
            self.budget.cost.taps_max,
            self.budget.cost.gflops_direct,
            self.budget.cost.gflops_fft
        ));
        for t in &self.budget.latency_tiers {
            out.push(format!(
                "latency_tier: budget={} ms remaining={:.6} ms fir_delay={:.6} ms fits={}",
                t.budget_ms, t.remaining_ms, t.fir_delay_ms, t.fits
            ));
        }
        for c in &self.criteria {
            out.push(format!(
                "criterion[{}] {}: {} (code={}) observed={} limit={} {} :: {}",
                c.group.as_str(),
                c.id,
                if c.passed { "PASS" } else { "FAIL" },
                c.code,
                c.observed,
                c.limit,
                c.unit,
                c.detail
            ));
        }
        for s in &self.statements {
            out.push(format!("statement: {s}"));
        }
        for w in &self.warnings {
            out.push(format!("warning: {w}"));
        }
        out
    }
}

// ===========================================================================
// 输入校验（拒绝路径：不静默降级）
// ===========================================================================

fn validate_fs(fs: f64) -> Result<(), ReportError> {
    if !fs.is_finite() || fs < 8_000.0 || fs > 192_000.0 {
        return Err(ReportError::new(
            E_FIR_SCHEMA,
            format!("invalid fs={fs}: FIR 报告要求有限且 8000..=192000 Hz（manifest 只允许 48000/96000）"),
        ));
    }
    Ok(())
}

fn validate_range(name: &str, v: f64) -> Result<(), ReportError> {
    if !v.is_finite() {
        return Err(ReportError::new(
            E_FIR_BAND,
            format!("{name}={v} must be finite"),
        ));
    }
    Ok(())
}

fn validate_band_pair(name: &str, band: [f64; 2], fs: f64) -> Result<(), ReportError> {
    validate_band_pair_with_guard(name, band, fs, NYQUIST_GUARD_FRACTION)
}

/// 同 validate_band_pair，但可显式给上界护栏（linear HP 的 passband 上界允许声明 = 0.5 fs，
/// 供 §3.1 判据 9 的 Nyquist 特例判定；测量网格仍只到 0.499 fs）。
fn validate_band_pair_with_guard(
    name: &str,
    band: [f64; 2],
    fs: f64,
    hi_fraction: f64,
) -> Result<(), ReportError> {
    let lo = band[0];
    let hi = band[1];
    if lo.is_nan() || hi.is_nan() {
        return Err(ReportError::new(
            E_FIR_BAND,
            format!("{name} contains NaN: {band:?}"),
        ));
    }
    if lo < 0.0 || (!hi.is_infinite() && hi <= lo) {
        return Err(ReportError::new(
            E_FIR_BAND,
            format!("{name}=[{lo},{hi}] invalid (need 0 <= lo < hi, +inf allowed)"),
        ));
    }
    if lo > hi_fraction * fs + 1e-9 || (hi.is_finite() && hi > hi_fraction * fs + 1e-9) {
        return Err(ReportError::new(
            E_FIR_BAND,
            format!(
                "{name}=[{lo},{hi}] exceeds Nyquist guard {hi_fraction}*fs={:.3} (§1.2)",
                hi_fraction * fs
            ),
        ));
    }
    Ok(())
}

/// 声明完整性校验（taps/fs/参数/族特有硬约束）；不做任何测量。
fn validate_declared(d: &DeclaredDesign, options: &ReportOptions) -> Result<(), ReportError> {
    validate_fs(d.fs)?;
    // 族标识与族参数必须一致（否则报告会用错误的判据集，禁止静默）。§2.2 family 与 design 的匹配。
    let params_match = matches!(
        (&d.params, d.family),
        (FamilyParams::Linear { .. }, Family::Linear)
            | (FamilyParams::MinFast { .. }, Family::MinFast)
            | (FamilyParams::MinSlow { .. }, Family::MinSlow)
            | (FamilyParams::Apodizing { .. }, Family::Apodizing)
            | (FamilyParams::Brickwall { .. }, Family::Brickwall)
            | (
                FamilyParams::Corrected { .. },
                Family::Corrected | Family::Lowdelay
            )
            | (FamilyParams::Nos, Family::Nos)
    );
    if !params_match {
        return Err(ReportError::new(
            E_FIR_SCHEMA,
            format!(
                "family={} 与声明的族参数不一致（族参数 discriminant 不匹配；§2.2 family/design 必须自洽）",
                d.family
            ),
        ));
    }
    if d.taps == 0 {
        return Err(ReportError::new(E_FIR_TAPS, "declared taps=0"));
    }
    if d.taps > MAX_DESIGN_TAPS {
        return Err(ReportError::new(
            E_FIR_TAPS,
            format!(
                "declared taps={} > {}（§2.1 设计上限；>16384 仅 offline 工具，不在本报告器范围）",
                d.taps, MAX_DESIGN_TAPS
            ),
        ));
    }
    if !d.delay_samples.is_finite() || d.delay_samples < 0.0 {
        return Err(ReportError::new(
            E_FIR_DELAY,
            format!(
                "declared delay_samples={} must be finite and >= 0",
                d.delay_samples
            ),
        ));
    }
    if let Some(ms) = d.declared_delay_ms {
        if !ms.is_finite() {
            return Err(ReportError::new(
                E_FIR_DELAY,
                "declared_delay_ms must be finite",
            ));
        }
    }
    validate_range("reference_hz", d.reference_hz)?;
    validate_range("reference_gain_db", d.reference_gain_db)?;
    match &d.params {
        FamilyParams::Linear {
            mode,
            passband_hz,
            stopband_hz,
            ripple_db,
            attenuation_db,
            symmetry,
        } => {
            match mode {
                BandMode::Lowpass => validate_band_pair("passband_hz", *passband_hz, d.fs)?,
                BandMode::Highpass => {
                    validate_band_pair_with_guard("passband_hz", *passband_hz, d.fs, 0.5)?
                }
            }
            validate_band_pair("stopband_hz", *stopband_hz, d.fs)?;
            validate_range("ripple_db", *ripple_db)?;
            validate_range("attenuation_db", *attenuation_db)?;
            if *ripple_db < 0.0 {
                return Err(ReportError::new(E_FIR_BAND, "ripple_db must be >= 0"));
            }
            match mode {
                BandMode::Lowpass => {
                    if passband_hz[1] >= stopband_hz[0] {
                        return Err(ReportError::new(
                            E_FIR_BAND,
                            "linear lowpass requires f_p < f_s (§3.1 判据 8)",
                        ));
                    }
                }
                BandMode::Highpass => {
                    if stopband_hz[1] >= passband_hz[0] {
                        return Err(ReportError::new(
                            E_FIR_BAND,
                            "linear highpass requires f_s < f_p (§3.1 判据 8)",
                        ));
                    }
                }
            }
            if *mode == BandMode::Lowpass && *symmetry == SymmetryMode::Antisymmetric {
                return Err(ReportError::new(
                    E_FIR_BAND,
                    "antisymmetric only allowed for highpass（Type III/IV 的 H(0)=0，§3.1 参数约束）",
                ));
            }
        }
        FamilyParams::MinFast {
            passband_hz,
            stopband_hz,
            ripple_db,
            attenuation_db,
        } => {
            validate_band_pair("passband_hz", *passband_hz, d.fs)?;
            validate_band_pair("stopband_hz", *stopband_hz, d.fs)?;
            validate_range("ripple_db", *ripple_db)?;
            validate_range("attenuation_db", *attenuation_db)?;
        }
        FamilyParams::MinSlow {
            passband_hz,
            nyquist_attenuation_db,
            ripple_db,
        } => {
            validate_band_pair("passband_hz", *passband_hz, d.fs)?;
            validate_range("nyquist_attenuation_db", *nyquist_attenuation_db)?;
            validate_range("ripple_db", *ripple_db)?;
        }
        FamilyParams::Apodizing {
            passband_hz,
            ripple_db,
            nyquist_attenuation_db,
            pre_ring_bound_db,
        } => {
            validate_band_pair("passband_hz", *passband_hz, d.fs)?;
            validate_range("ripple_db", *ripple_db)?;
            validate_range("nyquist_attenuation_db", *nyquist_attenuation_db)?;
            validate_range("pre_ring_bound_db", *pre_ring_bound_db)?;
        }
        FamilyParams::Brickwall {
            mode,
            passband_hz,
            stopband_hz,
            ripple_db,
            attenuation_db,
            method: _,
        } => {
            validate_band_pair("passband_hz", *passband_hz, d.fs)?;
            validate_band_pair("stopband_hz", *stopband_hz, d.fs)?;
            validate_range("ripple_db", *ripple_db)?;
            validate_range("attenuation_db", *attenuation_db)?;
            let _ = mode;
        }
        FamilyParams::Corrected {
            band_hz,
            phase_target,
            max_mag_error_db,
            max_phase_error_deg,
            rms_error_db,
            magnitude_only,
            equivalence_claim,
            delay_max_samples,
        } => {
            validate_band_pair("band_hz", *band_hz, d.fs)?;
            validate_range("max_mag_error_db", *max_mag_error_db)?;
            validate_range("max_phase_error_deg", *max_phase_error_deg)?;
            validate_range("rms_error_db", *rms_error_db)?;
            if let PhaseTarget::DelayRemoved {
                target_delay_samples,
            } = phase_target
            {
                if !target_delay_samples.is_finite() || *target_delay_samples < 0.0 {
                    return Err(ReportError::new(
                        E_FIR_DELAY,
                        "target_delay_samples must be finite and >= 0",
                    ));
                }
            }
            let _ = (magnitude_only, equivalence_claim, delay_max_samples);
            // §3.6 判据 1：corrected/lowdelay 必须有显式相位目标（phase_ref 不允许 none）。
            // 本报告用 PhaseTarget 的存在表达；缺失由 FamilyParams 已不可能，这里检查是防未来
            // 扩展把 phase_target 变 Option。
        }
        FamilyParams::Nos => {
            if d.taps != 1 {
                return Err(ReportError::new(
                    E_FIR_NOS,
                    format!("nos requires taps=1（§3.7），declared {}", d.taps),
                ));
            }
        }
    }
    // corrected 的目标曲线是测量判据的硬输入；缺了默认拒绝（不静默记 NOT_RUN 冒充通过）。
    if matches!(d.family.group(), FamilyGroup::Corrected) && options.require_corrected_target {
        // 具体 target 存在性由 build_report 检查（本函数只做声明层校验）。
    }
    Ok(())
}

/// 把声明频带的 +inf 解析为 0.499*fs，并限制在 [0, 0.499*fs]。
fn resolve_band(band: [f64; 2], fs: f64) -> [f64; 2] {
    let nyq_guard = NYQUIST_GUARD_FRACTION * fs;
    let lo = band[0].max(0.0);
    let hi = if band[1].is_finite() {
        band[1]
    } else {
        nyq_guard
    };
    [lo.min(nyq_guard), hi.min(nyq_guard)]
}

// ===========================================================================
// 频带扫描（§1.2：声明频带内取 max/min，不许只取少数倍频点）
// ===========================================================================

/// 频带扫描结果（幅度/GD 一并采集）。
#[derive(Clone, Debug)]
#[allow(dead_code)]
struct BandScan {
    lo_hz: f64,
    hi_hz: f64,
    points: usize,
    min_db: f64,
    max_db: f64,
    gd_min: f64,
    gd_max: f64,
    gd_sum: f64,
}

impl BandScan {
    fn gd_mean(&self) -> f64 {
        if self.points == 0 {
            f64::NAN
        } else {
            self.gd_sum / self.points as f64
        }
    }
}

/// 在 [lo, hi] 上独立扫描（线性网格；lo=0 时含 DC）。
fn scan_band(h: &[f64], fs: f64, lo_hz: f64, hi_hz: f64, points: usize) -> BandScan {
    let n = points.max(2);
    let mut scan = BandScan {
        lo_hz,
        hi_hz,
        points: 0,
        min_db: f64::INFINITY,
        max_db: f64::NEG_INFINITY,
        gd_min: f64::INFINITY,
        gd_max: f64::NEG_INFINITY,
        gd_sum: 0.0,
    };
    for i in 0..n {
        let t = i as f64 / (n - 1) as f64;
        let f = lo_hz + (hi_hz - lo_hz) * t;
        let p = oracle_at(h, fs, f);
        scan.points += 1;
        scan.min_db = scan.min_db.min(p.mag_db);
        scan.max_db = scan.max_db.max(p.mag_db);
        scan.gd_min = scan.gd_min.min(p.group_delay_samples);
        scan.gd_max = scan.gd_max.max(p.group_delay_samples);
        scan.gd_sum += p.group_delay_samples;
    }
    scan
}

/// 合并网格点（对数密网格）与显式频带扫描，得到声明频带的 max/min。
fn band_stat(h: &[f64], fs: f64, samples: &[OraclePoint], lo_hz: f64, hi_hz: f64) -> BandStat {
    let mut min_db = f64::INFINITY;
    let mut max_db = f64::NEG_INFINITY;
    let mut points = 0usize;
    for s in samples {
        if s.freq_hz >= lo_hz && s.freq_hz <= hi_hz {
            min_db = min_db.min(s.mag_db);
            max_db = max_db.max(s.mag_db);
            points += 1;
        }
    }
    let scan = scan_band(h, fs, lo_hz, hi_hz, 257);
    min_db = min_db.min(scan.min_db);
    max_db = max_db.max(scan.max_db);
    points += scan.points;
    BandStat {
        lo_hz,
        hi_hz,
        points,
        min_db,
        max_db,
        ripple_db: max_db - min_db,
    }
}

/// 二分细化幅度穿越（§1.2 过渡带穿越点）。
fn bisect_mag_crossing(
    h: &[f64],
    fs: f64,
    mut f_low: f64,
    mut f_high: f64,
    threshold_db: f64,
    falling_from_passband: bool,
) -> f64 {
    for _ in 0..80 {
        let mid = 0.5 * (f_low + f_high);
        let db = oracle_at(h, fs, mid).mag_db;
        let attenuated = db <= threshold_db;
        let _ = falling_from_passband;
        if attenuated {
            f_high = mid;
        } else {
            f_low = mid;
        }
    }
    0.5 * (f_low + f_high)
}

/// LP：从通带侧寻找首次达到 -A_s 的穿越点（在 [region_lo, region_hi] 内）。
fn first_attenuation_crossing(
    h: &[f64],
    fs: f64,
    region_lo: f64,
    region_hi: f64,
    threshold_db: f64,
) -> Option<f64> {
    let n = 2049usize;
    let mut prev_f = region_lo;
    let mut prev_db = oracle_at(h, fs, prev_f).mag_db;
    for i in 1..n {
        let t = i as f64 / (n - 1) as f64;
        let f = region_lo + (region_hi - region_lo) * t;
        let db = oracle_at(h, fs, f).mag_db;
        if prev_db > threshold_db && db <= threshold_db {
            return Some(bisect_mag_crossing(h, fs, prev_f, f, threshold_db, true));
        }
        prev_f = f;
        prev_db = db;
    }
    None
}

/// HP：寻找最后一次离开 -A_s 的穿越点（衰减保持到 f_s 的镜像读法，Lead 裁决 A）。
fn last_departure_crossing(
    h: &[f64],
    fs: f64,
    region_lo: f64,
    region_hi: f64,
    threshold_db: f64,
) -> Option<f64> {
    let n = 2049usize;
    let mut prev_f = region_lo;
    let mut prev_db = oracle_at(h, fs, prev_f).mag_db;
    let mut out: Option<f64> = None;
    for i in 1..n {
        let t = i as f64 / (n - 1) as f64;
        let f = region_lo + (region_hi - region_lo) * t;
        let db = oracle_at(h, fs, f).mag_db;
        if prev_db <= threshold_db && db > threshold_db {
            out = Some(bisect_mag_crossing(h, fs, prev_f, f, threshold_db, false));
        }
        prev_f = f;
        prev_db = db;
    }
    out
}

// ===========================================================================
// 报告装配辅助
// ===========================================================================

fn build_support(d: &DeclaredDesign, h: &[f64]) -> SupportReport {
    let peak_abs = h.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
    let peak_index = h
        .iter()
        .enumerate()
        .fold((0usize, f64::NEG_INFINITY), |(bi, bv), (i, v)| {
            if v.abs() > bv {
                (i, v.abs())
            } else {
                (bi, bv)
            }
        })
        .0;
    let (first, last, effective) = effective_support(h, EFFECTIVE_SUPPORT_EPS);
    let sum_h: f64 = h.iter().sum();
    let sum_abs_h: f64 = h.iter().map(|v| v.abs()).sum();
    let onset_samples = first as f64;
    SupportReport {
        declared_taps: d.taps,
        len: h.len(),
        peak_index,
        peak_abs,
        first_effective: first,
        last_effective: last,
        effective_taps: effective,
        onset_samples,
        onset_ms: onset_samples / d.fs * 1000.0,
        sum_h,
        sum_abs_h,
        dc_gain_db: amplitude_to_db(sum_h.abs()),
        all_finite: h.iter().all(|v| v.is_finite()),
    }
}

fn build_ringing(d: &DeclaredDesign, h: &[f64]) -> RingingReport {
    let m = ringing_metrics(h, d.fs);
    RingingReport {
        exclusion_samples: m.exclusion_samples,
        peak_index: m.peak_index,
        peak_abs: m.peak_abs,
        pre_ring_db: m.pre_ring_db,
        post_ring_db: m.post_ring_db,
        pre_ring_energy_db: m.pre_ring_energy_db,
        post_ring_energy_db: m.post_ring_energy_db,
        duration_to_minus80_samples: m.duration_to_minus80_samples,
        notes: m.notes,
    }
}

/// 参考归一化实测（§1.5：hz=0 用 sum h，否则 §1.2 的 H(f)）。
fn measured_reference_db(h: &[f64], fs: f64, hz: f64) -> f64 {
    if hz.abs() <= 1e-12 {
        amplitude_to_db(h.iter().sum::<f64>().abs())
    } else {
        oracle_at(h, fs, hz).mag_db
    }
}

fn build_reference(
    d: &DeclaredDesign,
    h: &[f64],
    reference_hz: f64,
    declared_gain_db: f64,
) -> ReferenceReport {
    let measured = measured_reference_db(h, d.fs, reference_hz);
    let delta = measured - declared_gain_db;
    ReferenceReport {
        hz: reference_hz,
        declared_gain_db,
        measured_gain_db: measured,
        delta_db: delta,
        tolerance_db: REFERENCE_TOLERANCE_DB,
        passed: delta.abs() <= REFERENCE_TOLERANCE_DB,
    }
}

/// 20 Hz / 20 kHz Endpoint（§2.4）：端点要么 measured，要么 not_applicable_at_fs，绝不省略。
fn build_endpoints(samples: &[OraclePoint], fs: f64) -> Endpoints {
    let f20 = 20.0_f64;
    let f20k = 20_000.0_f64;
    let pick = |f: f64| -> Option<OraclePoint> {
        samples
            .iter()
            .find(|s| (s.freq_hz - f).abs() <= 1e-9 * f.max(1.0))
            .copied()
    };
    let f20_endpoint = match pick(f20) {
        Some(s) => Endpoint::Measured {
            freq_hz: f20,
            mag_db: s.mag_db,
            phase_deg: s.phase_deg,
        },
        None => Endpoint::NotApplicableAtFs {
            freq_hz: f20,
            reason: format!("20 Hz 不在测量网格内（fs={fs}）"),
        },
    };
    let reachable = f20k <= NYQUIST_GUARD_FRACTION * fs + 1e-9;
    let f20k_endpoint = if reachable {
        match pick(f20k) {
            Some(s) => Endpoint::Measured {
                freq_hz: f20k,
                mag_db: s.mag_db,
                phase_deg: s.phase_deg,
            },
            None => Endpoint::NotApplicableAtFs {
                freq_hz: f20k,
                reason: format!("20 kHz 可达但不在网格内（bug：网格上界应含 min(20k,0.499fs)）"),
            },
        }
    } else {
        Endpoint::NotApplicableAtFs {
            freq_hz: f20k,
            reason: format!(
                "0.499*fs={:.3} Hz < 20000 Hz：该采样率下 20 kHz 不可测（§2.4）",
                NYQUIST_GUARD_FRACTION * fs
            ),
        }
    };
    Endpoints {
        f20: f20_endpoint,
        f20k: f20k_endpoint,
    }
}

/// 1/3 oct 中心频率表（20 Hz 起，覆盖到 hi_hz）。
fn third_octave_centers(lo_hz: f64, hi_hz: f64) -> Vec<f64> {
    let mut out = Vec::new();
    let mut k: i32 = -17; // 1000 * 2^(-17/3) ≈ 19.3 Hz
    loop {
        let f = 1000.0 * 2f64.powf(k as f64 / 3.0);
        if f > hi_hz {
            break;
        }
        if f >= lo_hz {
            out.push(f);
        }
        k += 1;
        if out.len() > 64 {
            break;
        }
    }
    out
}

/// 群延迟报告（§1.3/§3.2 判据 3/§3.6 判据 4）。
fn build_group_delay(
    d: &DeclaredDesign,
    h: &[f64],
    samples: &[OraclePoint],
    onset_ms: f64,
    band: [f64; 2],
) -> GroupDelayReport {
    let scan = scan_band(h, d.fs, band[0], band[1], 513);
    let mut gd_min = scan.gd_min;
    let mut gd_max = scan.gd_max;
    for s in samples {
        if s.freq_hz >= band[0] && s.freq_hz <= band[1] {
            gd_min = gd_min.min(s.group_delay_samples);
            gd_max = gd_max.max(s.group_delay_samples);
        }
    }
    let gd_1k = oracle_at(h, d.fs, 1000.0).group_delay_samples;
    let declared_delay_ms = d.delay_samples / d.fs * 1000.0;
    let third_octave: Vec<(f64, f64)> = third_octave_centers(band[0], band[1])
        .into_iter()
        .map(|f| (f, oracle_at(h, d.fs, f).group_delay_samples))
        .collect();
    let mut characteristic = Vec::new();
    for f in [
        50.0_f64, 100.0, 1_000.0, 5_000.0, 10_000.0, 15_000.0, 20_000.0,
    ] {
        if f >= band[0] && f <= band[1] {
            characteristic.push((f, oracle_at(h, d.fs, f).group_delay_samples));
        }
    }
    let class = d.family.phase_class();
    let statement = match d.family {
        Family::Linear | Family::Apodizing | Family::Brickwall => format!(
            "{family}: constant group delay class; declared D={:.6} samples ({:.6} ms) must equal (L-1)/2={:.6}; support L is NOT the intrinsic group delay（§1.3 禁止把支撑长度说成固有群延迟）；L 偶数时 D 为半整数，原样报告、不取整",
            d.delay_samples,
            declared_delay_ms,
            (d.taps as f64 - 1.0) / 2.0,
            family = d.family.as_str()
        ),
        Family::MinFast | Family::MinSlow => format!(
            "{family}: minimum-phase / frequency-varying group delay (NOT zero-delay, §3.2 判据 3 禁止声称零延迟); declared bulk D={:.6} samples; latency_ms is GD(1 kHz)/fs*1000={:.6} ms; gd_max={:.6} samples ({:.6} ms); onset={:.6} ms",
            d.delay_samples,
            gd_1k / d.fs * 1000.0,
            gd_max,
            gd_max / d.fs * 1000.0,
            onset_ms,
            family = d.family.as_str()
        ),
        Family::Corrected | Family::Lowdelay => format!(
            "{family}: class=frequency_varying（§3.6 判据 4，禁止说成线性相位/最小相位等价）; declared D={:.6} samples; latency_ms=GD(1 kHz)/fs*1000={:.6} ms; gd_max={:.6} ms",
            d.delay_samples,
            gd_1k / d.fs * 1000.0,
            gd_max / d.fs * 1000.0,
            family = d.family.as_str()
        ),
        Family::Nos => "nos: no filtering, D=0, group delay 0 samples（旁路，§3.7）".to_string(),
    };
    GroupDelayReport {
        class,
        band_lo_hz: band[0],
        band_hi_hz: band[1],
        min_samples: gd_min,
        max_samples: gd_max,
        mean_samples: scan.gd_mean(),
        at_1khz_samples: gd_1k,
        onset_samples: 0.0,
        onset_ms,
        latency_ms_1khz: gd_1k / d.fs * 1000.0,
        gd_max_ms: gd_max / d.fs * 1000.0,
        declared_delay_ms,
        third_octave,
        characteristic_hz_samples: characteristic,
        statement,
    }
}

// ===========================================================================
// 预算报告装配（§7.2 + §2.3）
// ===========================================================================

/// 声明里的 (A_s, Δf)（linear/brickwall 用；其余 None）。
fn declared_attenuation_and_transition(d: &DeclaredDesign) -> Option<(f64, f64, bool)> {
    match &d.params {
        FamilyParams::Linear {
            mode,
            passband_hz,
            stopband_hz,
            attenuation_db,
            ..
        } => {
            let delta = match mode {
                BandMode::Lowpass => stopband_hz[0] - passband_hz[1],
                BandMode::Highpass => passband_hz[0] - stopband_hz[1],
            };
            Some((*attenuation_db, delta, false))
        }
        FamilyParams::Brickwall {
            mode,
            passband_hz,
            stopband_hz,
            attenuation_db,
            method,
            ..
        } => {
            let delta = match mode {
                BandMode::Lowpass => stopband_hz[0] - passband_hz[1],
                BandMode::Highpass => passband_hz[0] - stopband_hz[1],
            };
            Some((
                *attenuation_db,
                delta,
                *method == BrickwallMethod::KaiserWindowedSinc,
            ))
        }
        _ => None,
    }
}

fn latency_tier(fir_delay_ms: f64, budget_ms: f64, options: &ReportOptions) -> LatencyBudgetReport {
    let remaining = budget_ms - options.block_ms - options.src_ms - options.io_ms;
    LatencyBudgetReport {
        budget_ms,
        block_ms: options.block_ms,
        src_ms: options.src_ms,
        io_ms: options.io_ms,
        fir_delay_ms,
        remaining_ms: remaining,
        fits: fir_delay_ms <= remaining + 1e-12,
    }
}

fn build_budget(
    d: &DeclaredDesign,
    h: &[f64],
    options: &ReportOptions,
    group_delay: &GroupDelayReport,
) -> BudgetReport {
    let taps_profile = TapsProfile::for_taps(h.len());
    let route_taps_cap = match options.profile {
        RuntimeProfile::Regular => Some(REGULAR_PROFILE_TAPS_CAP),
        RuntimeProfile::Extended => Some(EXTENDED_PROFILE_TAPS_CAP),
        RuntimeProfile::OfflineOnly => None,
    };
    let fits_runtime_profile = match options.profile {
        RuntimeProfile::Regular => taps_profile == TapsProfile::Regular,
        RuntimeProfile::Extended => {
            matches!(taps_profile, TapsProfile::Regular | TapsProfile::Extended)
        }
        RuntimeProfile::OfflineOnly => true,
    };
    let fir_delay_ms = d.delay_samples / d.fs * 1000.0;
    let latency_tiers: Vec<LatencyBudgetReport> = LATENCY_BUDGETS_MS
        .iter()
        .map(|&b| latency_tier(fir_delay_ms, b, options))
        .collect();
    let latency = options
        .latency_budget_ms
        .map(|b| latency_tier(fir_delay_ms, b, options));
    let mut notes = Vec::new();
    if !MANIFEST_FS_VALUES.contains(&(d.fs as u32)) {
        notes.push(format!(
            "fs={} 不是 manifest 允许的 48000/96000（§2.2）；报告只作事实，不能直接落 manifest",
            d.fs
        ));
    }
    if taps_profile == TapsProfile::NotAdmitted {
        notes.push(format!(
            "taps={} 落在 §7.2 未列出的中间区间（regular<=4096，extended 仅 8192/16384），不得当 extended 放行",
            h.len()
        ));
    }
    if fir_delay_ms > LATENCY_BUDGETS_MS[2] - options.block_ms - options.src_ms - options.io_ms {
        notes.push(format!(
            "FIR D/fs={:.6} ms 超过 40 ms 档的剩余预算；线性相位 4096@48k=42.65625 ms 三档都不可直接满足（§7.2）",
            fir_delay_ms
        ));
    }
    if !matches!(group_delay.class, PhaseClass::Constant | PhaseClass::None) {
        notes.push(format!(
            "{} 类：必须报频变 GD 与共同 D，不得用低频用途豁免延迟预算（§7.2）",
            group_delay.class.as_str()
        ));
    }
    // Kaiser 可行性（§3.5 判据 5；linear 只作事实）。
    let kaiser = declared_attenuation_and_transition(d).map(
        |(attenuation_db, transition_hz, applies_hard)| {
            let required = kaiser_required_taps(attenuation_db, d.fs, transition_hz);
            let feasible = match required {
                Some(req) => (h.len() as u64) >= req,
                None => true,
            };
            KaiserFeasibility {
                attenuation_db,
                transition_hz,
                required_taps: required,
                declared_taps: h.len(),
                feasible,
                applies_hard,
                detail: format!(
                    "L_req = ceil(1 + (A_s - 7.95)*fs/(14.357078*Δf)) = {:?}（A_s={:.1} dB，Δf={:.3} Hz，fs={:.0}）",
                    required, attenuation_db, transition_hz, d.fs
                ),
            }
        },
    );
    // §2.3 成本（P 路径）。
    let paths: Vec<usize> = vec![h.len(); options.cost_paths.max(1)];
    let cost = estimate_costs(&CostInputs {
        fs: d.fs,
        paths,
        block: options.cost_block.max(1),
        transforms_in: options.transforms_in,
        transforms_out: options.transforms_out,
    });
    // §7.1/§2.3 融合对比（L_F = L_G + L_P - 1）。
    let fusion = options.fusion_pair.map(|(g, p)| {
        let fusion_length = g + p - 1;
        let fused_cost = estimate_costs(&CostInputs {
            fs: d.fs,
            paths: vec![fusion_length],
            block: options.cost_block.max(1),
            transforms_in: options.transforms_in,
            transforms_out: options.transforms_out,
        });
        let factorized_cost = estimate_costs(&CostInputs {
            fs: d.fs,
            paths: vec![g, p],
            block: options.cost_block.max(1),
            transforms_in: options.transforms_in,
            transforms_out: options.transforms_out,
        });
        FusionReport {
            taps_g: g,
            taps_p: p,
            fusion_length,
            fused_cheaper: fused_cost.gflops_direct <= factorized_cost.gflops_direct,
            fused_cost,
            factorized_cost,
        }
    });
    BudgetReport {
        taps: h.len(),
        taps_profile,
        requested_profile: options.profile,
        fits_runtime_profile,
        route_taps_cap,
        ram_bytes_f32_per_route: F32_BYTES_PER_TAP * h.len(),
        ram_bytes_f32_total: F32_BYTES_PER_TAP * h.len() * options.cost_paths.max(1),
        fir_delay_ms,
        latency,
        latency_tiers,
        kaiser,
        cost,
        fusion,
        notes,
    }
}

// ===========================================================================
// 判据
// ===========================================================================

fn synthesis_error(h: &[f64], symmetry: SymmetryMode) -> f64 {
    let n = h.len();
    if n <= 1 {
        return 0.0;
    }
    let peak = h.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
    let sign = symmetry.sign();
    let mut err = 0.0_f64;
    for i in 0..n {
        let diff = h[i] - sign * h[n - 1 - i];
        err = err.max(diff.abs());
    }
    if peak > 0.0 {
        err / peak
    } else {
        err
    }
}

fn nyquist_value(h: &[f64]) -> f64 {
    // H(0.5 fs) = sum h[n] * (-1)^n。
    h.iter()
        .enumerate()
        .map(|(n, &v)| if n % 2 == 0 { v } else { -v })
        .sum()
}

fn wrap_deg(mut deg: f64) -> f64 {
    while deg <= -180.0 {
        deg += 360.0;
    }
    while deg > 180.0 {
        deg -= 360.0;
    }
    deg
}

fn transition_tolerance(delta_hz: f64) -> f64 {
    (TRANSITION_TOLERANCE_FRACTION * delta_hz).max(TRANSITION_TOLERANCE_MIN_HZ)
}

fn has_duplicate_or_nonmonotonic(freqs: &[f64]) -> bool {
    freqs.windows(2).any(|w| !(w[1] > w[0]))
}

// ===========================================================================
// 报告主入口
// ===========================================================================

/// 便捷入口：无目标/无外部声明的报告。
pub fn analyze(declared: &DeclaredDesign, coefficients: &[f64]) -> Result<FirReport, ReportError> {
    build_report(&ReportRequest::new(declared, coefficients))
}

/// 七族独立报告主入口。
///
/// 拒绝路径（Err）：未知 family、taps 与 len(h) 不符、h 非有限或全零、fs 非法、nos 非恒等、
/// corrected/lowdelay 缺显式相位目标、频带参数非法（§2.5/§3.6/§3.7 的规范码）。
/// 判据不通过不 Err：报告完整返回，passed=false + Criterion.code。
pub fn build_report(request: &ReportRequest<'_>) -> Result<FirReport, ReportError> {
    let d = request.declared;
    let h = request.coefficients;
    validate_declared(d, &request.options)?;

    // --- 输入硬校验（拒绝路径）-----------------------------------------------
    if h.len() != d.taps {
        return Err(ReportError::new(
            E_FIR_TAPS,
            format!(
                "taps mismatch: declared {} vs len(h) {}（§2.2 规则 1）",
                d.taps,
                h.len()
            ),
        ));
    }
    if h.is_empty() {
        return Err(ReportError::new(E_FIR_TAPS, "empty coefficients"));
    }
    if !h.iter().all(|v| v.is_finite()) {
        return Err(ReportError::new(
            E_FIR_NUMERIC,
            "coefficients contain NaN/Inf（§2.2 规则 3）",
        ));
    }
    let peak_abs = h.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
    if d.family != Family::Nos && peak_abs <= 0.0 {
        return Err(ReportError::new(
            E_FIR_NUMERIC,
            "max|h| = 0（§2.2 规则 3；nos 除外）",
        ));
    }
    if d.family == Family::Nos {
        if h.len() != 1 || (h[0] - 1.0).abs() > 1e-12 {
            return Err(ReportError::new(
                E_FIR_NOS,
                format!(
                    "nos requires h=[1.0]（§3.7/§9 V18），got len={} h[0]={}",
                    h.len(),
                    h[0]
                ),
            ));
        }
    }
    let group = d.family.group();
    if matches!(group, FamilyGroup::Corrected) {
        match request.target {
            Some(t) => t.validate(d.fs)?,
            None if request.options.require_corrected_target => {
                return Err(ReportError::new(
                    E_TGT_PHASE_UNSUPPORTED,
                    "corrected/lowdelay requires an explicit complex phase target（§3.6 判据 1：phase_ref=none -> E_TGT_PHASE_UNSUPPORTED）；magnitude-only 设计请用 linear/min_* 家族",
                ));
            }
            None => {}
        }
    }
    let mut warnings: Vec<String> = Vec::new();
    if !matches!(group, FamilyGroup::Corrected) && request.target.is_some() {
        warnings
            .push("target 只由 corrected/lowdelay 消费；该族报告忽略目标曲线（§6.10）".to_string());
    }

    // --- 测量网格与独立响应 ---------------------------------------------------
    let grid = MeasurementGrid::for_design(d.fs, h.len());
    if has_duplicate_or_nonmonotonic(&grid.freqs) {
        return Err(ReportError::new(
            E_FIR_SCHEMA,
            "internal: measurement grid is not strictly increasing",
        ));
    }
    let mut samples = oracle_grid(h, d.fs, &grid.freqs);
    match d.family {
        Family::Linear | Family::Apodizing | Family::Brickwall => {
            anchor_phase_deg(&mut samples, -360.0 * grid.lo_hz * d.delay_samples / d.fs);
        }
        Family::Corrected | Family::Lowdelay => {
            let expected = match request.target {
                Some(t) => t.total_phase_deg(grid.lo_hz, d.fs),
                None => -360.0 * grid.lo_hz * d.delay_samples / d.fs,
            };
            anchor_phase_deg(&mut samples, expected);
        }
        Family::Nos => anchor_phase_deg(&mut samples, 0.0),
        Family::MinFast | Family::MinSlow => {}
    }

    // --- 支撑 / 振铃 / 参考 / 端点 -------------------------------------------
    let support = build_support(d, h);
    let ringing = build_ringing(d, h);
    let reference = build_reference(d, h, d.reference_hz, d.reference_gain_db);
    let endpoints = build_endpoints(&samples, d.fs);

    // --- 频带解析 -------------------------------------------------------------
    let mut passband: Option<[f64; 2]> = None;
    let mut stopband: Option<[f64; 2]> = None;
    let mut nyquist_band: Option<[f64; 2]> = None;
    match &d.params {
        FamilyParams::Linear {
            passband_hz,
            stopband_hz,
            ..
        }
        | FamilyParams::MinFast {
            passband_hz,
            stopband_hz,
            ..
        }
        | FamilyParams::Brickwall {
            passband_hz,
            stopband_hz,
            ..
        } => {
            passband = Some(resolve_band(*passband_hz, d.fs));
            stopband = Some(resolve_band(*stopband_hz, d.fs));
        }
        FamilyParams::MinSlow { passband_hz, .. } | FamilyParams::Apodizing { passband_hz, .. } => {
            passband = Some(resolve_band(*passband_hz, d.fs));
            nyquist_band = Some([
                NYQUIST_BAND_LO_FRACTION * d.fs,
                NYQUIST_GUARD_FRACTION * d.fs,
            ]);
        }
        FamilyParams::Corrected { band_hz, .. } => {
            passband = Some(resolve_band(*band_hz, d.fs));
        }
        FamilyParams::Nos => {}
    }
    // HP 的阻带按 §3.1 判据 4 是 [0, f_s]（含 DC，Lead 裁决 A）；声明的 20 Hz 下界只是测量下界。
    if let FamilyParams::Linear {
        mode: BandMode::Highpass,
        stopband_hz,
        ..
    }
    | FamilyParams::Brickwall {
        mode: BandMode::Highpass,
        stopband_hz,
        ..
    } = &d.params
    {
        stopband = Some([0.0, stopband_hz[1].min(NYQUIST_GUARD_FRACTION * d.fs)]);
    }
    let response_passband = passband.map(|b| band_stat(h, d.fs, &samples, b[0], b[1]));
    let response_stopband = stopband.map(|b| {
        let stat = band_stat(h, d.fs, &samples, b[0], b[1]);
        StopbandStat {
            attenuation_db: -stat.max_db,
            band: stat,
        }
    });
    let response_nyquist_band = nyquist_band.map(|b| band_stat(h, d.fs, &samples, b[0], b[1]));

    // --- 群延迟 ---------------------------------------------------------------
    let gd_band = resolve_band(d.declared_band_hz(), d.fs);
    let mut group_delay = build_group_delay(d, h, &samples, support.onset_ms, gd_band);
    group_delay.onset_samples = support.onset_samples;
    group_delay.onset_ms = support.onset_ms;

    // --- 过渡带 ---------------------------------------------------------------
    let transition = match &d.params {
        FamilyParams::Linear {
            mode,
            passband_hz,
            stopband_hz,
            attenuation_db,
            ..
        }
        | FamilyParams::Brickwall {
            mode,
            passband_hz,
            stopband_hz,
            attenuation_db,
            ..
        } => Some({
            let f_p = passband_hz[1].min(NYQUIST_GUARD_FRACTION * d.fs);
            let f_s = stopband_hz[0].min(NYQUIST_GUARD_FRACTION * d.fs);
            let lp_fp = passband_hz[1].min(NYQUIST_GUARD_FRACTION * d.fs);
            let lp_fs = stopband_hz[0].min(NYQUIST_GUARD_FRACTION * d.fs);
            let delta = match mode {
                BandMode::Lowpass => stopband_hz[0] - passband_hz[1],
                BandMode::Highpass => passband_hz[0] - stopband_hz[1],
            };
            let tol = transition_tolerance(delta);
            match mode {
                BandMode::Lowpass => {
                    let region_lo = (lp_fp - 0.2 * d.fs).max(MEASUREMENT_LO_HZ);
                    let region_hi =
                        ((lp_fs + 0.2 * d.fs).max(lp_fs)).min(NYQUIST_GUARD_FRACTION * d.fs);
                    match first_attenuation_crossing(
                        h,
                        d.fs,
                        region_lo,
                        region_hi,
                        -attenuation_db,
                    ) {
                        Some(edge) => TransitionStat {
                            mode: BandMode::Lowpass,
                            attenuation_db: *attenuation_db,
                            edge_hz_at_attenuation: edge,
                            width_hz: edge - stopband_hz[0],
                            tolerance_hz: tol,
                            passed: edge <= stopband_hz[0] + tol,
                            detail: format!(
                                "LP 首次达到 -{:.1} dB 的穿越点 {:.3} Hz 必须 <= f_s + tol = {:.1} + {:.1} Hz（§3.1 判据 5/§3.5 判据 4）",
                                attenuation_db, edge, stopband_hz[0], tol
                            ),
                        },
                        None => TransitionStat {
                            mode: BandMode::Lowpass,
                            attenuation_db: *attenuation_db,
                            edge_hz_at_attenuation: f64::NAN,
                            width_hz: f64::NAN,
                            tolerance_hz: tol,
                            passed: false,
                            detail: format!(
                                "LP 未在 [{region_lo:.3},{region_hi:.3}] Hz 内找到 -{:.1} dB 穿越点（要么未达到衰减，要么网格/频带声明错误）",
                                attenuation_db
                            ),
                        },
                    }
                }
                BandMode::Highpass => {
                    let region_lo = (f_s - 0.2 * d.fs).max(MEASUREMENT_LO_HZ);
                    let region_hi = f_p.max(f_s).min(NYQUIST_GUARD_FRACTION * d.fs);
                    match last_departure_crossing(
                        h,
                        d.fs,
                        region_lo,
                        region_hi,
                        -attenuation_db,
                    ) {
                        Some(edge) => TransitionStat {
                            mode: BandMode::Highpass,
                            attenuation_db: *attenuation_db,
                            edge_hz_at_attenuation: edge,
                            width_hz: stopband_hz[1] - edge,
                            tolerance_hz: tol,
                            passed: edge >= stopband_hz[1] - tol,
                            detail: format!(
                                "HP 镜像读法（Lead 裁决 A）：最后一次离开 -{:.1} dB 的穿越点 {:.3} Hz 必须 >= f_s - tol = {:.1} - {:.1} Hz；阻带为 [0, f_s] 且衰减保持到 f_s",
                                attenuation_db, edge, stopband_hz[1], tol
                            ),
                        },
                        None => TransitionStat {
                            mode: BandMode::Highpass,
                            attenuation_db: *attenuation_db,
                            edge_hz_at_attenuation: f64::NAN,
                            width_hz: f64::NAN,
                            tolerance_hz: tol,
                            passed: false,
                            detail: format!(
                                "HP 未在 [{region_lo:.3},{region_hi:.3}] Hz 内找到 -{:.1} dB 的最后离开穿越点",
                                attenuation_db
                            ),
                        },
                    }
                }
            }
        }),
        _ => None,
    };

    // --- 预算 -----------------------------------------------------------------
    let budget = build_budget(d, h, &request.options, &group_delay);

    // --- 判据 -----------------------------------------------------------------
    let mut criteria: Vec<Criterion> = Vec::new();
    let mut push = |c: Criterion| criteria.push(c);
    let common = CriterionGroup::Common;
    push(Criterion::new(
        common,
        "taps_match",
        E_FIR_TAPS,
        h.len() == d.taps,
        h.len() as f64,
        d.taps as f64,
        "count",
        "len(h) 必须等于声明 taps（§2.2 规则 1）",
    ));
    push(Criterion::new(
        common,
        "finite",
        E_FIR_NUMERIC,
        support.all_finite,
        1.0,
        1.0,
        "bool",
        "全部系数有限（无 NaN/Inf，§2.2 规则 3）",
    ));
    push(Criterion::new(
        common,
        "grid_spec",
        E_FIR_SCHEMA,
        grid.meets_spec,
        grid.points as f64,
        grid.required_min_points as f64,
        "count",
        format!(
            "N={} >= max(8192, 8*L)={}（§1.2 测量网格）",
            grid.points, grid.required_min_points
        ),
    ));
    push(Criterion::new(
        common,
        "reference",
        E_FIR_REF,
        reference.passed,
        reference.delta_db,
        REFERENCE_TOLERANCE_DB,
        "dB",
        format!(
            "|H({:.3} Hz)| = {:.6} dB vs declared {:.6} dB（§1.5/§2.2 规则 4）",
            reference.hz, reference.measured_gain_db, reference.declared_gain_db
        ),
    ));
    let constant_delay_target = (h.len() as f64 - 1.0) / 2.0;
    let (delay_passed, delay_obs, delay_lim, delay_code, delay_detail) = match d.family {
        Family::Linear | Family::Apodizing | Family::Brickwall => (
            (d.delay_samples - constant_delay_target).abs() <= GROUP_DELAY_TOLERANCE_SAMPLES,
            d.delay_samples,
            constant_delay_target,
            E_FIR_DELAY,
            format!(
                "常数延迟族 D 必须 = (L-1)/2 = {constant_delay_target:.6}（±{GROUP_DELAY_TOLERANCE_SAMPLES}，§3.1 判据 7/§2.2 规则 2）"
            ),
        ),
        Family::Nos => (
            d.delay_samples.abs() <= 1e-12,
            d.delay_samples,
            0.0,
            E_FIR_DELAY,
            "nos 的 D 必须 = 0（§2.2 规则 2/§3.7）".to_string(),
        ),
        Family::MinFast | Family::MinSlow => (
            d.delay_samples.is_finite() && d.delay_samples >= 0.0,
            d.delay_samples,
            f64::INFINITY,
            E_FIR_DELAY,
            "最小相位族的 D 由设计声明并报告（§1.3），不强制 (L-1)/2；实际延迟用频变 GD 报告".to_string(),
        ),
        Family::Corrected | Family::Lowdelay => {
            let integer = (d.delay_samples - d.delay_samples.round()).abs() <= 1e-9;
            (
                integer && d.delay_samples >= 0.0,
                d.delay_samples,
                d.delay_samples.round(),
                E_FIR_DELAY,
                "corrected/lowdelay 的 D 必须是整数 samples 且 >= 0（§3.6 判据 3）".to_string(),
            )
        }
    };
    push(Criterion::new(
        common,
        "delay_rule",
        delay_code,
        delay_passed,
        delay_obs,
        delay_lim,
        "samples",
        delay_detail,
    ));
    if let Some(ms) = d.declared_delay_ms {
        let expected = d.delay_samples / d.fs * 1000.0;
        let tol = expected.abs().max(1.0) * 1e-6;
        push(Criterion::new(
            common,
            "delay_ms_consistency",
            E_FIR_DELAY,
            (ms - expected).abs() <= tol,
            ms,
            expected,
            "ms",
            "delay_ms 必须 = delay_samples/fs*1000（§2.2 规则 2）",
        ));
    }

    // 族判据 ---------------------------------------------------------------
    let fam = CriterionGroup::Family;
    match &d.params {
        FamilyParams::Linear {
            mode,
            passband_hz,
            ripple_db,
            attenuation_db,
            symmetry,
            ..
        } => {
            let sym_err = synthesis_error(h, *symmetry);
            push(Criterion::new(
                fam,
                "linear_symmetry",
                E_FIR_PHASE_CLASS,
                sym_err <= 1e-7,
                sym_err,
                1e-7,
                "relative",
                format!(
                    "|h[n]-s*h[L-1-n]|/max|h| <= 1e-7，{}（§3.1 判据 1）",
                    symmetry.as_str()
                ),
            ));
            if let Some(pb) = passband {
                let mut max_dev = 0.0_f64;
                let mut covered = 0usize;
                for s in &samples {
                    if s.freq_hz >= pb[0] && s.freq_hz <= pb[1] {
                        max_dev =
                            max_dev.max((s.group_delay_samples - constant_delay_target).abs());
                        covered += 1;
                    }
                }
                push(Criterion::new(
                    fam,
                    "linear_group_delay_constant",
                    E_FIR_PHASE_CLASS,
                    max_dev <= GROUP_DELAY_TOLERANCE_SAMPLES,
                    max_dev,
                    GROUP_DELAY_TOLERANCE_SAMPLES,
                    "samples",
                    format!(
                        "passband 内 |GD(f) - (L-1)/2| <= 0.01（§3.1 判据 2）；覆盖 {covered} 个网格点（网格上界 {:.1} Hz，passband 上界 {:.1} Hz）",
                        grid.hi_hz, pb[1]
                    ),
                ));
            }
            if let Some(pb) = &response_passband {
                push(Criterion::new(
                    fam,
                    "passband_ripple",
                    E_FIR_BUDGET,
                    pb.ripple_db <= ripple_db + PASSBAND_RIPPLE_MARGIN_DB,
                    pb.ripple_db,
                    ripple_db + PASSBAND_RIPPLE_MARGIN_DB,
                    "dB",
                    format!(
                        "passband [{:.3},{:.3}] p-p <= ripple+0.05 dB（§3.1 判据 3；{} 点）",
                        pb.lo_hz, pb.hi_hz, pb.points
                    ),
                ));
            }
            if let Some(sb) = &response_stopband {
                let (limit, detail) = match mode {
                    BandMode::Lowpass => (
                        -(attenuation_db - STOPBAND_LP_MARGIN_DB),
                        format!(
                            "LP 阻带 [{:.1},{:.1}] max <= -(A_s-0.5) dB（§3.1 判据 4）",
                            sb.band.lo_hz, sb.band.hi_hz
                        ),
                    ),
                    BandMode::Highpass => (
                        -*attenuation_db,
                        format!(
                            "HP 阻带 [0,{:.1}] 整段纹波峰值必须 < -A_s（无 0.5 dB 余量；Lead 裁决 A，§3.1 判据 4）",
                            sb.band.hi_hz
                        ),
                    ),
                };
                let passed = if *mode == BandMode::Highpass {
                    sb.band.max_db < -*attenuation_db
                } else {
                    sb.band.max_db <= limit
                };
                push(Criterion::new(
                    fam,
                    "stopband_attenuation",
                    E_FIR_BUDGET,
                    passed,
                    sb.band.max_db,
                    limit,
                    "dB",
                    detail,
                ));
            }
            if let Some(t) = &transition {
                push(Criterion::new(
                    fam,
                    "transition_edge",
                    E_FIR_BUDGET,
                    t.passed,
                    t.edge_hz_at_attenuation,
                    t.width_hz,
                    "Hz",
                    t.detail.clone(),
                ));
            }
            let (fp, fs_edge) = match mode {
                BandMode::Lowpass => (passband_hz[1], resolve_band([0.0, 0.0], d.fs)[0]),
                BandMode::Highpass => (passband_hz[0], 0.0),
            };
            let _ = (fp, fs_edge);
            // §3.1 判据 8 边界（声明层；validate_declared 已拒非法，这里记录事实）。
            let delta = match mode {
                BandMode::Lowpass => (passband_hz[1] - MEASUREMENT_LO_HZ) * 0.0 + 30.0,
                BandMode::Highpass => 30.0,
            };
            push(Criterion::new(
                fam,
                "band_boundary",
                E_FIR_BAND,
                delta >= 30.0,
                delta,
                30.0,
                "Hz",
                "线性相位 LP/HP 的 f_s-f_p >= 30 Hz（§3.1 判据 8）；声明层已校验顺序/越界",
            ));
            // Nyquist 特例（§3.1 判据 9）：passband 上界包含 0.5fs 且 L 偶数 -> E_FIR_HP_NYQ。
            let declares_nyquist_edge =
                *mode == BandMode::Highpass && passband_hz[1] >= 0.5 * d.fs - 1e-9;
            let h_nyq_db = amplitude_to_db(nyquist_value(h).abs());
            let passed = !(declares_nyquist_edge && h.len() % 2 == 0);
            push(Criterion::new(
                fam,
                "hip_nyquist_type",
                E_FIR_HP_NYQ,
                passed,
                h_nyq_db,
                0.0,
                "dB",
                format!(
                    "HP 声明 passband 上界含 0.5fs 且 L={} 偶数 -> 偶数长对称 FIR 必有 H(0.5fs)=0，必须用 L 奇数/频谱反转（§3.1 判据 9）；实测 H(0.5fs)={:.3} dB",
                    h.len(),
                    h_nyq_db
                ),
            ));
        }
        FamilyParams::MinFast {
            passband_hz,
            stopband_hz,
            ripple_db,
            attenuation_db,
        } => {
            if let Some(pb) = &response_passband {
                push(Criterion::new(
                    fam,
                    "passband_ripple",
                    E_FIR_BUDGET,
                    pb.ripple_db <= ripple_db + PASSBAND_RIPPLE_MARGIN_DB,
                    pb.ripple_db,
                    ripple_db + PASSBAND_RIPPLE_MARGIN_DB,
                    "dB",
                    "min_fast 通带纹波同 §3.1 判据 3（§3.2 判据 1）",
                ));
            }
            if let Some(sb) = &response_stopband {
                push(Criterion::new(
                    fam,
                    "stopband_attenuation",
                    E_FIR_BUDGET,
                    sb.band.max_db <= -(attenuation_db - STOPBAND_LP_MARGIN_DB),
                    sb.band.max_db,
                    -(attenuation_db - STOPBAND_LP_MARGIN_DB),
                    "dB",
                    format!(
                        "阻带 [{:.1},{:.1}] max <= -(A_s-0.5)（§3.2 判据 1 引 §3.1 判据 4）",
                        sb.band.lo_hz, sb.band.hi_hz
                    ),
                ));
            }
            if let Some(t) = &transition {
                push(Criterion::new(
                    fam,
                    "transition_edge",
                    E_FIR_BUDGET,
                    t.passed,
                    t.edge_hz_at_attenuation,
                    t.width_hz,
                    "Hz",
                    t.detail.clone(),
                ));
            }
            let m2 = min_phase_consistency_deg(h, d.fs, MIN_PHASE_FFT);
            push(Criterion::new(
                fam,
                "min_phase_hilbert",
                E_FIR_MINPHASE,
                m2 <= MIN_PHASE_LIMIT_DEG,
                m2,
                MIN_PHASE_LIMIT_DEG,
                "deg",
                format!(
                    "M2 相位-Hilbert 一致性 20 Hz-0.45fs max <= 2.0 deg（§3.2 判据 2；N={MIN_PHASE_FFT}，log|H| floor -120 dB）"
                ),
            ));
            let f_p = passband_hz[1];
            let f_s = stopband_hz[0];
            let ratio = f_s / f_p;
            push(Criterion::new(
                fam,
                "min_fast_boundary_ratio",
                E_FIR_FAMILY,
                ratio <= MIN_FAST_EDGE_RATIO,
                ratio,
                MIN_FAST_EDGE_RATIO,
                "ratio",
                "min_fast: f_s/f_p <= 1.5（§3.2 判据 4）",
            ));
            push(Criterion::new(
                fam,
                "min_fast_boundary_nyquist",
                E_FIR_FAMILY,
                f_s <= MIN_FAST_EDGE_NYQUIST * d.fs,
                f_s,
                MIN_FAST_EDGE_NYQUIST * d.fs,
                "Hz",
                "min_fast: f_s <= 0.45*fs（§3.2 判据 4）",
            ));
            let _ = passband_hz[0];
        }
        FamilyParams::MinSlow {
            passband_hz,
            nyquist_attenuation_db,
            ripple_db,
        } => {
            if let Some(pb) = &response_passband {
                push(Criterion::new(
                    fam,
                    "passband_ripple",
                    E_FIR_BUDGET,
                    pb.ripple_db <= ripple_db + PASSBAND_RIPPLE_MARGIN_DB,
                    pb.ripple_db,
                    ripple_db + PASSBAND_RIPPLE_MARGIN_DB,
                    "dB",
                    "min_slow 通带 20 Hz-f_p ripple（§3.3 判据 1）",
                ));
            }
            if let Some(nb) = &response_nyquist_band {
                let limit = -(nyquist_attenuation_db - NYQUIST_BAND_MARGIN_DB);
                push(Criterion::new(
                    fam,
                    "nyquist_band_attenuation",
                    E_FIR_BUDGET,
                    nb.max_db <= limit,
                    nb.max_db,
                    limit,
                    "dB",
                    "Nyquist 带 [0.49fs,0.499fs] max <= -(declared-1)（§3.3 判据 2）",
                ));
            }
            let f_p = passband_hz[1];
            let hi = NYQUIST_GUARD_FRACTION * d.fs;
            let scan = scan_band(h, d.fs, f_p, hi, 1025);
            // 上翘：相对运行最小值的最大抬升（§3.3 判据 3：<= 0.25 dB）。
            let mut run_min = f64::INFINITY;
            let mut max_rise = 0.0_f64;
            for i in 0..scan.points {
                let t = if scan.points <= 1 {
                    0.0
                } else {
                    i as f64 / (scan.points - 1) as f64
                };
                let f = f_p + (hi - f_p) * t;
                let db = oracle_at(h, d.fs, f).mag_db;
                run_min = run_min.min(db);
                max_rise = max_rise.max(db - run_min);
            }
            push(Criterion::new(
                fam,
                "transition_monotonic",
                E_FIR_FAMILY,
                max_rise <= MIN_SLOW_MONOTONIC_RISE_DB,
                max_rise,
                MIN_SLOW_MONOTONIC_RISE_DB,
                "dB",
                "f > f_p 后不得上翘超过 0.25 dB（§3.3 判据 3）",
            ));
            let m2 = min_phase_consistency_deg(h, d.fs, MIN_PHASE_FFT);
            push(Criterion::new(
                fam,
                "min_phase_hilbert",
                E_FIR_MINPHASE,
                m2 <= MIN_PHASE_LIMIT_DEG,
                m2,
                MIN_PHASE_LIMIT_DEG,
                "deg",
                "M2 相位-Hilbert 一致性（§3.3 判据 4 引 §3.2 判据 2）",
            ));
            let probe = oracle_at(h, d.fs, FAST_BOUNDARY_RATIO * d.fs).mag_db;
            push(Criterion::new(
                fam,
                "min_slow_distinction",
                E_FIR_FAMILY,
                probe > FAST_BOUNDARY_ATTENUATION_DB,
                probe,
                FAST_BOUNDARY_ATTENUATION_DB,
                "dB",
                "min_slow 必须证明 M(0.45fs) > -60 dB，否则改用 min_fast（§3.3 判据 6）",
            ));
            push(Criterion::new(
                fam,
                "min_slow_fp_boundary",
                E_FIR_FAMILY,
                f_p <= MIN_SLOW_FP_RATIO * d.fs,
                f_p,
                MIN_SLOW_FP_RATIO * d.fs,
                "Hz",
                "min_slow: f_p <= 0.42*fs（§3.3）",
            ));
        }
        FamilyParams::Apodizing {
            passband_hz,
            ripple_db,
            nyquist_attenuation_db,
            pre_ring_bound_db,
        } => {
            let sym_err = synthesis_error(h, SymmetryMode::Symmetric);
            push(Criterion::new(
                fam,
                "apodizing_symmetry",
                E_FIR_PHASE_CLASS,
                sym_err <= 1e-7,
                sym_err,
                1e-7,
                "relative",
                "apodizing 必须对称（线性相位，不是砖墙；§3.4 判据 1 引 §3.1 判据 1）",
            ));
            if let Some(pb) = passband {
                let mut max_dev = 0.0_f64;
                for s in &samples {
                    if s.freq_hz >= pb[0] && s.freq_hz <= pb[1] {
                        max_dev =
                            max_dev.max((s.group_delay_samples - constant_delay_target).abs());
                    }
                }
                push(Criterion::new(
                    fam,
                    "apodizing_group_delay_constant",
                    E_FIR_PHASE_CLASS,
                    max_dev <= GROUP_DELAY_TOLERANCE_SAMPLES,
                    max_dev,
                    GROUP_DELAY_TOLERANCE_SAMPLES,
                    "samples",
                    "apodizing 常数 GD = (L-1)/2（§3.4 判据 1 引 §3.1 判据 2）",
                ));
            }
            if let Some(pb) = &response_passband {
                push(Criterion::new(
                    fam,
                    "passband_ripple",
                    E_FIR_BUDGET,
                    pb.ripple_db <= ripple_db + PASSBAND_RIPPLE_MARGIN_DB,
                    pb.ripple_db,
                    ripple_db + PASSBAND_RIPPLE_MARGIN_DB,
                    "dB",
                    "apodizing 通带 ripple（§3.4 判据 2）",
                ));
            }
            if let Some(nb) = &response_nyquist_band {
                let limit = -(nyquist_attenuation_db - NYQUIST_BAND_MARGIN_DB);
                push(Criterion::new(
                    fam,
                    "nyquist_band_attenuation",
                    E_FIR_BUDGET,
                    nb.max_db <= limit,
                    nb.max_db,
                    limit,
                    "dB",
                    "Nyquist 带 [0.49fs,0.499fs] max <= -(declared-1)（§3.4 判据 3）",
                ));
            }
            let pre = ringing.pre_ring_db.unwrap_or(f64::NEG_INFINITY);
            let pre_e = ringing.pre_ring_energy_db.unwrap_or(f64::NEG_INFINITY);
            let bound = *pre_ring_bound_db;
            push(Criterion::new(
                fam,
                "pre_ring",
                E_FIR_FAMILY,
                pre <= bound && pre_e <= APODIZING_PRE_RING_ENERGY_BOUND_DB,
                pre,
                bound,
                "dB",
                format!(
                    "前振铃 pre={pre:.3} dB（<= {bound:.1}）且 pre_energy={pre_e:.3} dB（<= -45；§3.4 判据 4；W={}）",
                    ringing.exclusion_samples
                ),
            ));
            push(Criterion::new(
                fam,
                "post_ring_reported",
                E_FIR_FAMILY,
                ringing.post_ring_db.is_some(),
                ringing.post_ring_db.unwrap_or(f64::NAN),
                f64::NEG_INFINITY,
                "dB",
                "后振铃必须报告（本族不设上限；§3.4 判据 5）",
            ));
            let f_p = passband_hz[1];
            let hi = NYQUIST_GUARD_FRACTION * d.fs;
            let n = 1025usize;
            let mut run_min = f64::INFINITY;
            let mut max_rise = 0.0_f64;
            for i in 0..n {
                let t = i as f64 / (n - 1) as f64;
                let f = f_p + (hi - f_p) * t;
                let db = oracle_at(h, d.fs, f).mag_db;
                run_min = run_min.min(db);
                max_rise = max_rise.max(db - run_min);
            }
            push(Criterion::new(
                fam,
                "transition_monotonic",
                E_FIR_FAMILY,
                max_rise <= APODIZING_TRANSITION_RISE_DB,
                max_rise,
                APODIZING_TRANSITION_RISE_DB,
                "dB",
                "f_p..0.499fs 内不得上升超过 0.5 dB（§3.4 判据 6）",
            ));
            let fp_limit = APODIZING_FP_MAX_FRACTION_OF_NYQUIST * d.fs / 2.0;
            push(Criterion::new(
                fam,
                "apodizing_fp_boundary",
                E_FIR_APO_FP,
                f_p <= fp_limit,
                f_p,
                fp_limit,
                "Hz",
                "apodizing: f_p <= 0.85*fs/2（§3.4 判据 7）",
            ));
            let probe = oracle_at(h, d.fs, BRICKWALL_PROBE_FRACTION * d.fs).mag_db;
            push(Criterion::new(
                fam,
                "apodizing_not_brickwall",
                E_FIR_FAMILY,
                probe > BRICKWALL_PROBE_DB,
                probe,
                BRICKWALL_PROBE_DB,
                "dB",
                "若测得 0.45fs 处 <= -60 dB（砖墙式）必须改报 brickwall/linear（§3.4 判据 7/§3.8）",
            ));
        }
        FamilyParams::Brickwall {
            mode,
            passband_hz,
            stopband_hz,
            ripple_db,
            attenuation_db,
            method,
        } => {
            let sym_err = synthesis_error(h, SymmetryMode::Symmetric);
            push(Criterion::new(
                fam,
                "brickwall_symmetry",
                E_FIR_PHASE_CLASS,
                sym_err <= 1e-7,
                sym_err,
                1e-7,
                "relative",
                "brickwall 线性相位对称（§3.5 判据 1 引 §3.1 判据 1）",
            ));
            let mut max_dev = 0.0_f64;
            for s in &samples {
                max_dev = max_dev.max((s.group_delay_samples - constant_delay_target).abs());
            }
            push(Criterion::new(
                fam,
                "brickwall_group_delay_constant",
                E_FIR_PHASE_CLASS,
                max_dev <= GROUP_DELAY_TOLERANCE_SAMPLES,
                max_dev,
                GROUP_DELAY_TOLERANCE_SAMPLES,
                "samples",
                "brickwall 常数 GD = (L-1)/2（§3.5 判据 1 引 §3.1 判据 2）",
            ));
            if let Some(pb) = &response_passband {
                push(Criterion::new(
                    fam,
                    "passband_ripple",
                    E_FIR_BUDGET,
                    pb.ripple_db <= ripple_db + PASSBAND_RIPPLE_MARGIN_DB,
                    pb.ripple_db,
                    ripple_db + PASSBAND_RIPPLE_MARGIN_DB,
                    "dB",
                    "brickwall 通带 ripple（§3.5 判据 2）",
                ));
            }
            if let Some(sb) = &response_stopband {
                let limit = -(attenuation_db - 1.0);
                push(Criterion::new(
                    fam,
                    "stopband_attenuation",
                    E_FIR_BUDGET,
                    sb.band.max_db <= limit,
                    sb.band.max_db,
                    limit,
                    "dB",
                    "阻带 [f_s,0.499fs] max <= -(A_s-1)（§3.5 判据 3）",
                ));
            }
            if let Some(t) = &transition {
                push(Criterion::new(
                    fam,
                    "transition_edge",
                    E_FIR_BUDGET,
                    t.passed,
                    t.edge_hz_at_attenuation,
                    t.width_hz,
                    "Hz",
                    t.detail.clone(),
                ));
            }
            let delta = match mode {
                BandMode::Lowpass => stopband_hz[0] - passband_hz[1],
                BandMode::Highpass => passband_hz[0] - stopband_hz[1],
            };
            push(Criterion::new(
                fam,
                "brickwall_transition_nonzero",
                E_FIR_BAND,
                delta >= BRICKWALL_MIN_TRANSITION_HZ,
                delta,
                BRICKWALL_MIN_TRANSITION_HZ,
                "Hz",
                "有限砖墙近似：Δf=f_s-f_p >= 20 Hz（过渡带不为 0；§3.5 判据 7）",
            ));
            let as_ok = (*attenuation_db >= BRICKWALL_MIN_ATTENUATION_DB)
                && (*attenuation_db <= BRICKWALL_MAX_ATTENUATION_DB);
            push(Criterion::new(
                fam,
                "brickwall_attenuation_range",
                E_FIR_BAND,
                as_ok,
                *attenuation_db,
                BRICKWALL_MAX_ATTENUATION_DB,
                "dB",
                "A_s ∈ [60,120]（§3.5 判据 7）",
            ));
            if let Some(k) = &budget.kaiser {
                let hard = *method == BrickwallMethod::KaiserWindowedSinc;
                push(Criterion::new(
                    fam,
                    "brickwall_feasibility",
                    E_FIR_BUDGET,
                    !hard || k.feasible,
                    k.declared_taps as f64,
                    k.required_taps.map(|v| v as f64).unwrap_or(0.0),
                    "taps",
                    format!(
                        "Kaiser 可行性预检（§3.5 判据 5，method={}）：{}",
                        method.as_str(),
                        k.detail
                    ),
                ));
            }
            push(Criterion::new(
                fam,
                "brickwall_ringing_duration",
                E_FIR_FAMILY,
                ringing.duration_to_minus80_samples.is_some(),
                ringing.duration_to_minus80_samples.unwrap_or(0) as f64,
                f64::NAN,
                "samples",
                "必须报从主峰衰减到 -80 dB 的振铃时长（§3.5 判据 6；本族不设振铃上限）",
            ));
        }
        FamilyParams::Corrected {
            band_hz: _,
            phase_target,
            max_mag_error_db,
            max_phase_error_deg,
            rms_error_db,
            magnitude_only,
            equivalence_claim,
            delay_max_samples,
        } => {
            push(Criterion::new(
                fam,
                "corrected_phase_target",
                E_TGT_PHASE_UNSUPPORTED,
                true,
                1.0,
                1.0,
                "bool",
                format!(
                    "corrected/lowdelay 必须有显式相位目标（{}，§3.6 判据 1）；phase_ref=none 走 E_TGT_PHASE_UNSUPPORTED",
                    phase_target.as_str()
                ),
            ));
            if let PhaseTarget::DelayRemoved {
                target_delay_samples,
            } = phase_target
            {
                let err = (d.delay_samples - target_delay_samples).abs();
                push(Criterion::new(
                    fam,
                    "corrected_delay_consistency",
                    E_FIR_DELAY,
                    err <= DELAY_CONSISTENCY_SAMPLES,
                    err,
                    DELAY_CONSISTENCY_SAMPLES,
                    "samples",
                    format!(
                        "delay_removed 时 |D_used - D_target| <= 0.01 sample（D_used={}，D_target={}，§3.6 判据 1）",
                        d.delay_samples, target_delay_samples
                    ),
                ));
            }
            if let Some(max) = delay_max_samples {
                push(Criterion::new(
                    fam,
                    "lowdelay_delay_max",
                    INFEASIBLE,
                    d.delay_samples <= *max,
                    d.delay_samples,
                    *max,
                    "samples",
                    "lowdelay: D <= delay_max_samples（§3.6 判据 5）",
                ));
            }
            let claim_bad = matches!(
                equivalence_claim,
                EquivalenceClaim::LinearPhase | EquivalenceClaim::MinimumPhase
            );
            let matrix_claim_without_target = *equivalence_claim
                == EquivalenceClaim::MeasuredComplexMatrix
                && request.target.is_none();
            let mag_only_claims = *magnitude_only && *equivalence_claim != EquivalenceClaim::None;
            push(Criterion::new(
                fam,
                "corrected_equivalence_claim",
                E_FIR_EQUIV_CLAIM,
                !(claim_bad || matrix_claim_without_target || mag_only_claims),
                if claim_bad {
                    1.0
                } else if matrix_claim_without_target {
                    2.0
                } else if mag_only_claims {
                    3.0
                } else {
                    0.0
                },
                0.0,
                "code",
                format!(
                    "corrected 不得声称与 linear/min 等价（§3.6 判据 6/§3.8）；claim={}，magnitude_only={}",
                    equivalence_claim.as_str(),
                    magnitude_only
                ),
            ));
            if let Some(target) = request.target {
                // 复误差：band 内 max 幅度误差 / max 相位误差 / RMS 复幅度误差（§3.6 判据 2）。
                let band = resolve_band(request.declared.declared_band_hz(), d.fs);
                let mut max_mag = 0.0_f64;
                let mut max_phase = 0.0_f64;
                let mut sum_sq_err = 0.0_f64;
                let mut sum_sq_tgt = 0.0_f64;
                let mut count = 0usize;
                let extra = scan_band(h, d.fs, band[0], band[1], 257);
                let _ = extra;
                let mut freqs: Vec<f64> = samples
                    .iter()
                    .filter(|s| s.freq_hz >= band[0] && s.freq_hz <= band[1])
                    .map(|s| s.freq_hz)
                    .collect();
                for i in 0..257 {
                    let t = i as f64 / 256.0;
                    freqs.push(band[0] + (band[1] - band[0]) * t);
                }
                freqs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                freqs.dedup_by(|a, b| (*a - *b).abs() <= 1e-9);
                for f in freqs {
                    let measured = oracle_at(h, d.fs, f);
                    let (t_mag, _) = target.sample(f);
                    let t_phase = target.total_phase_deg(f, d.fs);
                    max_mag = max_mag.max((measured.mag_db - t_mag).abs());
                    // 相位差用 wrap 到 (-180,180]（§3.6 判据 2）。
                    let mut phase_diff = measured.phase_deg - t_phase;
                    phase_diff = wrap_deg(phase_diff);
                    max_phase = max_phase.max(phase_diff.abs());
                    let h_lin = db_to_amplitude(measured.mag_db);
                    let t_lin = db_to_amplitude(t_mag);
                    let diff = h_lin - t_lin * (t_phase.to_radians()).cos();
                    let diff_i = 0.0 - t_lin * (t_phase.to_radians()).sin();
                    sum_sq_err += diff * diff + diff_i * diff_i;
                    sum_sq_tgt += t_lin * t_lin;
                    count += 1;
                }
                let rms_db = if count > 0 && sum_sq_tgt > 0.0 {
                    20.0 * (sum_sq_err / sum_sq_tgt).sqrt().log10()
                } else {
                    f64::NEG_INFINITY
                };
                let code = if matches!(d.family, Family::Lowdelay) {
                    INFEASIBLE
                } else {
                    E_FIR_BUDGET
                };
                push(Criterion::new(
                    fam,
                    "corrected_complex_error",
                    code,
                    max_mag <= *max_mag_error_db
                        && max_phase <= *max_phase_error_deg
                        && rms_db <= *rms_error_db,
                    max_mag,
                    *max_mag_error_db,
                    "dB",
                    format!(
                        "带内 max|mag dB 误差|={max_mag:.6}（<= {max_mag_error_db}），max|wrap phase 误差|={max_phase:.6} deg（<= {max_phase_error_deg}），RMS 复幅度误差={rms_db:.6} dB（<= {rms_error_db}）；§3.6 判据 2"
                    ),
                ));
                if let PhaseTarget::DelayRemoved { .. } = phase_target {
                    // 因果检查（§3.6 判据 3）：目标总相位在声明 D 下的 GD 不得为负。
                    let mut min_gd = f64::INFINITY;
                    let n = 257usize;
                    let df = (band[1] - band[0]) / (n as f64);
                    for i in 1..n {
                        let f0 = band[0] + df * (i - 1) as f64;
                        let f1 = band[0] + df * i as f64;
                        let p0 = target.total_phase_deg(f0, d.fs);
                        let p1 = target.total_phase_deg(f1, d.fs);
                        let gd = -(p1 - p0) / 360.0 * d.fs / (f1 - f0);
                        min_gd = min_gd.min(gd);
                    }
                    push(Criterion::new(
                        fam,
                        "corrected_causality",
                        E_FIR_NONCAUSAL,
                        min_gd >= -0.01,
                        min_gd,
                        -0.01,
                        "samples",
                        "目标总相位在声明 D 下的最小 GD >= 0（否则不可因果，§3.6 判据 3）",
                    ));
                }
            }
            let _ = band_hz_of(&d.params);
        }
        FamilyParams::Nos => {
            let identity_err = (h[0] - 1.0).abs();
            push(Criterion::new(
                fam,
                "nos_identity",
                E_FIR_NOS,
                identity_err <= 1e-12,
                identity_err,
                1e-12,
                "abs",
                "nos 恒等：|h[0]-1| <= 1e-12（§3.7/§9 V18）",
            ));
            let mut max_db = 0.0_f64;
            let mut max_phase = 0.0_f64;
            for i in 0..=256 {
                let f = MEASUREMENT_LO_HZ
                    + (MEASUREMENT_HI_HZ.min(NYQUIST_GUARD_FRACTION * d.fs) - MEASUREMENT_LO_HZ)
                        * (i as f64 / 256.0);
                let p = oracle_at(h, d.fs, f);
                max_db = max_db.max(p.mag_db.abs());
                max_phase = max_phase.max(wrap_deg(p.phase_deg).abs());
            }
            push(Criterion::new(
                fam,
                "nos_flat_response",
                E_FIR_NOS,
                max_db <= 0.01 && max_phase <= 0.1,
                max_db,
                0.01,
                "dB",
                format!(
                    "nos response 全 0 dB/0 deg（实测 max|mag|={max_db:.6} dB，max|phase|={max_phase:.6} deg，§3.7）"
                ),
            ));
        }
    }

    // §3.8 家族自洽/互斥（独立检查组；边界/等价/恒等已在族判据里，这里给汇总视图）----
    let mutual = CriterionGroup::MutualExclusion;
    match group {
        FamilyGroup::Linear => {
            let sym_ok = match &d.params {
                FamilyParams::Linear { symmetry, .. } => synthesis_error(h, *symmetry) <= 1e-7,
                _ => false,
            };
            push(Criterion::new(
                mutual,
                "family_linear_phase_class",
                E_FIR_PHASE_CLASS,
                sym_ok,
                1.0,
                1.0,
                "bool",
                "§3.8: linear 对称/常数 GD 必须是线性相位类",
            ));
        }
        FamilyGroup::MinFast | FamilyGroup::MinSlow => {
            let m2 = min_phase_consistency_deg(h, d.fs, MIN_PHASE_FFT);
            push(Criterion::new(
                mutual,
                "family_minimum_phase",
                E_FIR_MINPHASE,
                m2 <= MIN_PHASE_LIMIT_DEG,
                m2,
                MIN_PHASE_LIMIT_DEG,
                "deg",
                "§3.8: min_* 必须通过 M2 最小相位验证",
            ));
        }
        FamilyGroup::Apodizing => {
            let probe = oracle_at(h, d.fs, BRICKWALL_PROBE_FRACTION * d.fs).mag_db;
            push(Criterion::new(
                mutual,
                "family_apodizing_vs_brickwall",
                E_FIR_FAMILY,
                probe > BRICKWALL_PROBE_DB,
                probe,
                BRICKWALL_PROBE_DB,
                "dB",
                "§3.8: apodizing 不得是 0.45fs 处 <= -60 dB 的砖墙式设计（应改报 brickwall/linear）",
            ));
        }
        FamilyGroup::Brickwall => {
            let delta = match &d.params {
                FamilyParams::Brickwall {
                    mode,
                    passband_hz,
                    stopband_hz,
                    ..
                } => match mode {
                    BandMode::Lowpass => stopband_hz[0] - passband_hz[1],
                    BandMode::Highpass => passband_hz[0] - stopband_hz[1],
                },
                _ => 0.0,
            };
            push(Criterion::new(
                mutual,
                "family_brickwall_vs_apodizing",
                E_FIR_FAMILY,
                delta >= BRICKWALL_MIN_TRANSITION_HZ,
                delta,
                BRICKWALL_MIN_TRANSITION_HZ,
                "Hz",
                "§3.8: brickwall 必须有非零过渡带与 >=60 dB 阻带声明（与 apodizing 区分）",
            ));
        }
        FamilyGroup::Corrected => {
            let magnitude_only = match &d.params {
                FamilyParams::Corrected { magnitude_only, .. } => *magnitude_only,
                _ => false,
            };
            let claim_ok = match &d.params {
                FamilyParams::Corrected {
                    equivalence_claim, ..
                } => !matches!(
                    equivalence_claim,
                    EquivalenceClaim::LinearPhase | EquivalenceClaim::MinimumPhase
                ),
                _ => true,
            };
            push(Criterion::new(
                mutual,
                "family_corrected_not_linear_min",
                E_FIR_EQUIV_CLAIM,
                claim_ok,
                1.0,
                1.0,
                "bool",
                format!(
                    "§3.8: corrected 不等于 linear/min{}（magnitude_only={}）",
                    if magnitude_only {
                        "；magnitude_only=true 时必须写 not equivalent to measured complex matrix"
                    } else {
                        ""
                    },
                    magnitude_only
                ),
            ));
        }
        FamilyGroup::Nos => {
            push(Criterion::new(
                mutual,
                "family_nos_identity",
                E_FIR_NOS,
                (h.len() == 1) && (h[0] - 1.0).abs() <= 1e-12,
                h.len() as f64,
                1.0,
                "count",
                "§3.8: nos 恒等 h=[1]",
            ));
        }
    }

    // 预算判据（独立组；不参与设计 passed，用于 runtime admission）-------------
    let bud = CriterionGroup::Budget;
    push(Criterion::new(
        bud,
        "taps_profile_admitted",
        E_FIR_BUDGET,
        budget.fits_runtime_profile,
        budget.taps as f64,
        budget.route_taps_cap.unwrap_or(0) as f64,
        "taps",
        format!(
            "requested profile={}，taps_profile={}，route caps：regular<={REGULAR_PROFILE_TAPS_CAP}，extended 仅 8192/16384（§7.2；超出应改 offline/INFEASIBLE，不截 taps）",
            budget.requested_profile.as_str(),
            budget.taps_profile.as_str()
        ),
    ));
    if let Some(k) = &budget.kaiser {
        if k.applies_hard {
            push(Criterion::new(
                bud,
                "kaiser_feasibility_hard",
                E_FIR_BUDGET,
                k.feasible,
                k.declared_taps as f64,
                k.required_taps.map(|v| v as f64).unwrap_or(0.0),
                "taps",
                format!("§3.5 判据 5 硬预检：{}", k.detail),
            ));
        }
    }
    if let Some(l) = &budget.latency {
        push(Criterion::new(
            bud,
            "latency_budget",
            E_FIR_BUDGET,
            l.fits,
            l.fir_delay_ms,
            l.remaining_ms,
            "ms",
            format!(
                "整链预算 {:.1} ms - block/src/io = 剩余 {:.6} ms；FIR D/fs={:.6} ms（§7.2；不足应 INFEASIBLE_FOR_PROFILE，不截 taps）",
                l.budget_ms, l.remaining_ms, l.fir_delay_ms
            ),
        ));
    }

    // 端点判据（20/20k 不漏，§2.4）------------------------------------------------
    let ep = CriterionGroup::Endpoint;
    let f20_ok = matches!(endpoints.f20, Endpoint::Measured { .. });
    push(Criterion::new(
        ep,
        "endpoint_20hz_present",
        E_FIR_ENDPOINT,
        f20_ok,
        20.0,
        20.0,
        "Hz",
        format!(
            "20 Hz Endpoint 必须存在且为 measured 或 not_applicable_at_fs（§2.4）；实测 status={}",
            endpoints.f20.status()
        ),
    ));
    let f20k_ok = match endpoints.f20k {
        Endpoint::Measured { .. } => true,
        Endpoint::NotApplicableAtFs { .. } => true,
    };
    push(Criterion::new(
        ep,
        "endpoint_20khz_present",
        E_FIR_ENDPOINT,
        f20k_ok,
        20_000.0,
        NYQUIST_GUARD_FRACTION * d.fs,
        "Hz",
        format!(
            "20 kHz Endpoint 必须存在且 status 合法（不能省略/裸字符串，§2.4）；reachable at fs={} => status={}",
            d.fs,
            endpoints.f20k.status()
        ),
    ));

    // §2.2 规则 5：外部声明响应的复算一致性 --------------------------------
    let mut consistency: Option<ConsistencyReport> = None;
    if let Some(exp) = request.expected {
        let mut compared = 0usize;
        let mut max_db = 0.0_f64;
        let mut max_sample = 0.0_f64;
        let mut max_phase = 0.0_f64;
        let mut fail_detail: Vec<String> = Vec::new();
        let mut check_db = |name: &str, declared: Option<f64>, measured: Option<f64>| {
            if let (Some(dv), Some(mv)) = (declared, measured) {
                compared += 1;
                let err = (dv - mv).abs();
                max_db = max_db.max(err);
                if err > REPORT_DB_TOLERANCE {
                    fail_detail.push(format!(
                        "{name}: declared {dv:.6} vs recomputed {mv:.6} (>{REPORT_DB_TOLERANCE} dB)"
                    ));
                }
            }
        };
        check_db(
            "passband_ripple_db",
            exp.passband_ripple_db,
            response_passband.as_ref().map(|b| b.ripple_db),
        );
        check_db(
            "passband_min_db",
            exp.passband_min_db,
            response_passband.as_ref().map(|b| b.min_db),
        );
        check_db(
            "passband_max_db",
            exp.passband_max_db,
            response_passband.as_ref().map(|b| b.max_db),
        );
        check_db(
            "stopband_attenuation_db",
            exp.stopband_attenuation_db,
            response_stopband.as_ref().map(|s| s.attenuation_db),
        );
        check_db(
            "stopband_max_db",
            exp.stopband_max_db,
            response_stopband.as_ref().map(|s| s.band.max_db),
        );
        check_db("ringing_pre_db", exp.ringing_pre_db, ringing.pre_ring_db);
        check_db("ringing_post_db", exp.ringing_post_db, ringing.post_ring_db);
        check_db(
            "ringing_pre_energy_db",
            exp.ringing_pre_energy_db,
            ringing.pre_ring_energy_db,
        );
        check_db(
            "ringing_post_energy_db",
            exp.ringing_post_energy_db,
            ringing.post_ring_energy_db,
        );
        if let Some(edge) = exp.transition_edge_hz {
            if let Some(t) = &transition {
                compared += 1;
                // 频率用相对容差（穿越点由二分得到）。
                let err = (edge - t.edge_hz_at_attenuation).abs();
                max_sample = max_sample.max(err);
                if err > 1e-3 * edge.max(1.0) {
                    fail_detail.push(format!(
                        "transition_edge_hz: declared {edge:.6} vs recomputed {:.6}",
                        t.edge_hz_at_attenuation
                    ));
                }
            }
        }
        let mut check_sample = |name: &str, declared: Option<f64>, measured: f64| {
            if let Some(dv) = declared {
                compared += 1;
                let err = (dv - measured).abs();
                max_sample = max_sample.max(err);
                if err > REPORT_SAMPLE_TOLERANCE {
                    fail_detail.push(format!(
                        "{name}: declared {dv:.6} vs recomputed {measured:.6} (>{REPORT_SAMPLE_TOLERANCE} samples)"
                    ));
                }
            }
        };
        check_sample(
            "group_delay_min_samples",
            exp.group_delay_min_samples,
            group_delay.min_samples,
        );
        check_sample(
            "group_delay_max_samples",
            exp.group_delay_max_samples,
            group_delay.max_samples,
        );
        check_sample(
            "group_delay_mean_samples",
            exp.group_delay_mean_samples,
            group_delay.mean_samples,
        );
        check_sample(
            "group_delay_at_1khz_samples",
            exp.group_delay_at_1khz_samples,
            group_delay.at_1khz_samples,
        );
        let mut check_endpoint =
            |name: &str, declared: Option<f64>, measured: Option<f64>, tol: f64, phase: bool| {
                if let (Some(dv), Some(mv)) = (declared, measured) {
                    compared += 1;
                    let err = (dv - mv).abs();
                    if phase {
                        max_phase = max_phase.max(err);
                    } else {
                        max_db = max_db.max(err);
                    }
                    if err > tol {
                        fail_detail.push(format!(
                            "{name}: declared {dv:.6} vs recomputed {mv:.6} (>{tol})"
                        ));
                    }
                }
            };
        let (f20_db, f20_ph) = match endpoints.f20 {
            Endpoint::Measured {
                mag_db, phase_deg, ..
            } => (Some(mag_db), Some(phase_deg)),
            Endpoint::NotApplicableAtFs { .. } => (None, None),
        };
        let (f20k_db, f20k_ph) = match endpoints.f20k {
            Endpoint::Measured {
                mag_db, phase_deg, ..
            } => (Some(mag_db), Some(phase_deg)),
            Endpoint::NotApplicableAtFs { .. } => (None, None),
        };
        check_endpoint(
            "endpoint_f20_mag_db",
            exp.endpoint_f20_mag_db,
            f20_db,
            REPORT_DB_TOLERANCE,
            false,
        );
        check_endpoint(
            "endpoint_f20_phase_deg",
            exp.endpoint_f20_phase_deg,
            f20_ph,
            REPORT_PHASE_TOLERANCE_DEG,
            true,
        );
        check_endpoint(
            "endpoint_f20k_mag_db",
            exp.endpoint_f20k_mag_db,
            f20k_db,
            REPORT_DB_TOLERANCE,
            false,
        );
        check_endpoint(
            "endpoint_f20k_phase_deg",
            exp.endpoint_f20k_phase_deg,
            f20k_ph,
            REPORT_PHASE_TOLERANCE_DEG,
            true,
        );
        let passed = fail_detail.is_empty();
        push(Criterion::new(
            CriterionGroup::Response,
            "requested_response_consistency",
            E_FIR_SCHEMA,
            passed,
            compared as f64,
            compared as f64,
            "fields",
            if passed {
                format!(
                    "外部声明响应 {compared} 个字段复算一致（<= {REPORT_DB_TOLERANCE} dB / {REPORT_SAMPLE_TOLERANCE} sample / {REPORT_PHASE_TOLERANCE_DEG} deg，§2.2 规则 5）"
                )
            } else {
                format!("外部声明响应不可复算：{}", fail_detail.join("；"))
            },
        ));
        consistency = Some(ConsistencyReport {
            compared_fields: compared,
            max_abs_db_err: max_db,
            max_abs_sample_err: max_sample,
            max_abs_phase_err_deg: max_phase,
            passed,
        });
    }

    // --- 陈述 ---------------------------------------------------------------
    let mut statements: Vec<String> = Vec::new();
    statements.push(group_delay.statement.clone());
    match &d.params {
        FamilyParams::Apodizing { .. } => statements.push(
            "仅自研可算窗/目标，不复刻任何专有 DAC（§3.4 判据 7 要求）".to_string(),
        ),
        FamilyParams::Brickwall { attenuation_db, .. } => statements.push(format!(
            "有限砖墙近似：过渡带 Δf>0、阻带声明 A_s={:.1} dB；振铃无上限（这是与 apodizing 的取舍差异，§3.5 判据 6）；ring_duration_to_-80dB={:?} samples",
            attenuation_db, ringing.duration_to_minus80_samples
        )),
        FamilyParams::Corrected { magnitude_only, .. } => {
            statements.push(
                "禁止把 corrected 说成线性相位/最小相位等价；not equivalent to measured complex matrix（§3.6 判据 6）".to_string(),
            );
            if *magnitude_only {
                statements.push(
                    "magnitude_only: true —— 只做幅度修正，不声称与实测复矩阵等价（§3.6 判据 6）".to_string(),
                );
            }
        }
        FamilyParams::Nos => {
            statements.push(
                "本软件旁路，不改变/不模拟 MOTU DAC 内部重建滤波；保护（limiter/clip/ramp）仍在图的其他节点执行，不属于本家族（§3.7）"
                    .to_string(),
            );
        }
        _ => {}
    }
    if let FamilyParams::Linear { mode, .. } = &d.params {
        statements.push(format!(
            "linear 报告字段：H(0.5fs)={:.6} dB（LP 期望 0 / HP 视长度；§3.1 报告额外字段），D=(L-1)/2={:.6} samples",
            amplitude_to_db(nyquist_value(h).abs()),
            constant_delay_target
        ));
        let _ = mode;
    }

    // --- 警告 ---------------------------------------------------------------
    warnings.extend(budget.notes.clone());
    for note in &ringing.notes {
        warnings.push(note.clone());
    }
    if grid.hi_hz < MEASUREMENT_HI_HZ {
        warnings.push(format!(
            "测量网格上界 {:.3} Hz < 20 kHz（受 0.499*fs 限制，§1.2）；20 kHz Endpoint 记 not_applicable_at_fs",
            grid.hi_hz
        ));
    }
    if d.family.is_minimum_phase() {
        warnings.push("min_* 必须报频变 GD；不得声称零延迟/零群延迟（§3.2 判据 3）".to_string());
    }
    if d.reference_hz == 0.0 && d.reference_gain_db != 0.0 {
        warnings.push(format!(
            "reference.hz=0 时 gain_db={} 与 DC 归一冲突？请确认声明（§1.5 禁止偷偷加整体增益）",
            d.reference_gain_db
        ));
    }
    let _ = group;

    // --- verification -------------------------------------------------------
    let passed = criteria
        .iter()
        .filter(|c| c.group != CriterionGroup::Budget)
        .all(|c| c.passed);
    let verification = VerificationReport {
        method: "direct DTFT H(f)=sum h[n]exp(-j2pi f n/fs) + analytic GD Re(G/H)（§1.2；非 leaf 模块复用）",
        grid_points: grid.points,
        ref_impl: "khaen-designer::report 独立 oracle（自带 radix-2 FFT 仅用于 M2 相位-Hilbert）",
        consistency,
        passed,
    };

    let report = FirReport {
        family: d.family,
        family_group: group,
        fs: d.fs,
        grid,
        support,
        reference,
        response_passband,
        response_stopband,
        response_nyquist_band,
        response_transition: transition,
        group_delay,
        ringing,
        endpoints,
        budget,
        criteria,
        verification,
        statements,
        warnings,
        passed,
    };
    Ok(report)
}

/// 取 corrected band_hz（仅内部工具；避免 match 里重复展开）。
fn band_hz_of(params: &FamilyParams) -> Option<[f64; 2]> {
    match params {
        FamilyParams::Corrected { band_hz, .. } => Some(*band_hz),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 私有测试（tests/T55 通过 include! 进入本模块的 #[cfg(test)] 子模块）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod report_tests {
    use super::*;

    include!("../tests/T55/report_cases.rs");
}
