//! 输出 FIR crossover（T39） ——LR/BW 分频配置、有限幅相近似、两支合并/极性/相对 D 检查。
//!
//! 权威契约：rt-stack/spec/fir-toolbox.md v1.2-draft §4.1（输出 FIR 分频，T39）与 §3.1（linear 家族）、
//! reference/SPEC.md §2.5（P_post 系数级语义）、WBS T39。
//!
//! # 本模块做什么
//!
//! 1. LR2/LR4/LR6/LR8 与 BW1..BW8 的**解析**分频目标：LRn = 两级 Butterworth n/2 级联（幅度平方），
//!    BWn = Butterworth 总阶 n；给出 LP/HP 两支的复频响（含相位），支持任意显式未知 type 拒收。
//! 2. 把解析目标近似成**有限 FIR**（f64）：两种显式方法
//!    - CrossoverMethod::AnalyticPhase（默认）：解析复频响 + 公共体延迟 + Nyquist fade 后 IFFT，
//!      矩形截断，近似**幅度与相位**；
//!    - CrossoverMethod::LinearPhaseMagnitude：只按解析 |LP|/|HP| 幅度目标做线性相位（§3.1）合成，
//!      相位由结构固定；**非规范对照**（Lead 裁决 2026-11-17 选定 AnalyticPhase 为规范构造，
//!      线性相位只约束支路 FIR 自身设计，不作为两支求和验收依据）。
//!    两种方法都报告幅度/相位近似误差、群延迟、前/后振铃、有效支撑。
//! 3. **两支合并检查**：S = H_LP + H_HP，在 [fc/2, 2fc] 内 |S| 相对 max 偏差 ≤ 0.5 dB（§4.1）。
//! 4. **极性检查（家族相对极性，RULED 2026-11-17）**：
//!    - LR 偶数阶（LR2/LR4/LR6/LR8）：两支在 fc **同相（相对 0°）**、求和幅频平坦；
//!      LR2/LR6 解析相差 180° 故令 HP 反相回到 0°；LR4/LR8 同相（§4.1 LR4 两支同极性）。
//!    - BW 奇数阶（BW1/3/5/7）：允许/要求**一支反相**（极性关系 180°）；解析和 (1∓s^n)/D
//!      对奇数 n 仍全通，故反相后幅频仍平坦；fc 相对**相位**按解析目标（±90°），**不强制 0°**、
//!      也不要求线性相位。
//!    同时测量相反极性作为诊断；**禁止靠听感/role 名判极性**。
//! 5. **相对 D 检查**：两支同 L、同方法、同一 D=(taps−1)/2；相对 D 必须为整数 samples 并报告
//!    （本实现恒为 0；D 本身可为整数/半整数，§1.3）。
//! 6. **无默认角色 HP**：本模块**不提供任何 role 名 → HP/LP 映射**；plan_outputs 只接受显式
//!    OutputAssignment，未赋值的输出是直通（bypass/full-range），不是 HP。高度默认全频
//!    （无 HP/LP/人工 18 ms，§4.1），不得按 role 名注入。
//!
//! # 判据实现口径（诚实报告，不静默放行）
//!
//! - 参数边界：spec §4.1 参数域 fc ∈ [20, 0.45·fs]；**可设计范围按裁决收敛**：生产 ≤0.2fs、
//!   实验 ≤0.225fs（显式 flag）、>0.225fs INFEASIBLE；taps ∈ [8, 16384]；未知 type 由
//!   CrossoverType::parse 拒收（E_FIR_XOVER_TYPE）。
//! - **错误码唯一性（终审 errata C，F-22）**：crossover 参数域（fs/fc/taps/attenuation/线性相位
//!   方法奇数 taps）与其它 FIR 家族共用唯一码 **E_FIR_BAND**（历史 `E_FIR_XOVER_BAND` 已撤销，
//!   alias→E_FIR_BAND，本模块不输出该名）；输出赋值布局/拓扑非法用独立码
//!   **E_FIR_XOVER_LAYOUT**（spec §2.5 已正式登记）。
//! - 合并平坦：|S(f)| 在 [fc/2, 2fc] 内 max |dB − max_dB| ≤ 0.5 dB（§4.1 原文）。
//! - 交叉点相对相位（RULED）：LR 偶数阶目标 0°（家族极性应用后同相和）；BW 奇数阶按解析目标
//!   （±90°，一支反相 180° 极性，不强制 0°），容差 5°；**BW 偶数阶不定义相位验收**：家族极性/
//!   相位@fc 两项只报告并标 NOT_APPLICABLE（不计 pass、不得以相位门放行或拒收），拒收只走合并
//!   平坦判据 → E_FIR_XOVER_SUM（终审 errata C，F-24；奇数阶行为不变）。
//! - 幅度近似误差在 crossover 带内 ≤ 0.5 dB；相位近似误差（仅 AnalyticPhase 方法）≤ 5°；
//!   这两个 0.5 dB / 5° 是**本实现的报告门**（spec §4.1 只要求“报告 FIR 近似误差”，未给数值），
//!   不改写任何 spec 判据。
//! - LR4 两支同极性（§4.1）单列为判据；BW 奇数阶“需报告使用极性”由报告字段与
//!   polarity_source = "analytic_filter_relation" 满足。
//!
//! # Lead 裁决（RULED 2026-11-17；全文见 evidence/T39/RULED-T39-2026-11-17.md）
//!
//! 1. **生产硬上限 fc ≤ 0.2·fs（=0.2fs 允许）**：0.2–0.225·fs 仅显式 experimental flag 可设计
//!    （warning + 非生产，判据 xover_production_cap 失败，**不得 accepted**）；fc > 0.225·fs
//!    → E_FIR_XOVER_FC_INFEASIBLE（INFEASIBLE）。默认 experimental=false 时 fc>0.2·fs 直接
//!    E_FIR_XOVER_FC_LIMIT 拒绝。
//! 2. **§4.1 判据按家族相对极性**（见上文第 4 条）：LR 偶数阶 fc 同相 0° 求和幅频平坦；
//!    BW 奇数阶一支反相（180° 极性）、不强制线性相位或 0° 求和；**线性相位只约束支路 FIR
//!    自身设计**，不约束两支求和。
//! 3. **规范构造二选一 → 选 AnalyticPhase**（自洽：解析复频响 + 公共体延迟 + Nyquist fade，
//!    能对 LR 全阶与 BW 奇数阶给出与家族极性判据一致的有限幅相近似）。LinearPhaseMagnitude
//!    保留为**非规范对照**（判据 xover_normative_construction 失败、warning 明示），不得用于
//!    §4.1 验收。此前“请裁决”的 spec 措辞矛盾随本裁决关闭；spec §4.1 文本更新属 T04/spec owner
//!    写域，不在本 leaf 修改。
//!
//! BW 偶数阶解析和本身不平坦（@fc +3.0103 dB，解析可证）：仍如实 INFEASIBLE（sum_flat 失败），
//! 不因裁决而放宽 0.5 dB，也不冒充已裁决项。**任何失败都会在 criteria/warnings 与 evidence/T39
//! 中保留原始测量值，不静默 PASS。** BW 偶数阶的 `xover_family_polarity`/`xover_phase_at_fc`
//! 为 NOT_APPLICABLE（只报告、不计 pass）；`design_strict` 对偶数阶只映射 E_FIR_XOVER_SUM
//! （终审 errata C，F-24）。
//!
//! # AnalyticPhase 的具体构造与限制
//!
//! 解析复频响 H(j·f/fc) 乘公共体延迟 e^{-j2πfD/fs}（D=(taps−1)/2）再乘 **Nyquist 护栏 fade**
//! （0.45fs→0.5fs 升余弦到 0），IFFT 后取 n=0..taps−1 矩形截断。fade 的作用是消除连续时间
//! LR/BW 在 fs/2 复相位（一般非 0/180°）与实 FIR 必须实 Nyquist 值之间的跳变——否则 IDFT 出现
//! ~1/n 长尾，短 taps 时 1–3 dB 级误差。fade 使目标频谱在 Nyquist 连续、长尾消失；0.45fs 以下
//! 幅度/相位完全按解析目标，含 20 kHz 规范测量带与常规分频带。
//!
//! 已知限制（诚实报告，不静默放行）：Nyquist fade 从 0.45fs 起，因此 fc 越大 crossover 带上界
//! （min(2fc, 0.499fs)）越接近 fade 区。Lead 裁决 2026-11-17 已把可设计范围收敛为：生产
//! fc ≤ 0.2fs（带的上界 ≤ 0.4fs，完全在 fade 外）、实验 ≤ 0.225fs（上界 ≤ 0.45fs，fade 边界）、
//! > 0.225fs INFEASIBLE。§4.1 的参数域上限 0.45fs 仅用于 E_FIR_BAND 消息，不再等于可设计范围。
//!
//! # 未测项（NOT_RUN，不得当已验证）
//!
//! 无板端/ALSA/CueMix/实时/soak；无 tspi-fir/v1 WAV/manifest 落盘（T55）；无与 T12 融合/T23 图
//! 节点/T56 资源计划的集成；f32 运行期量化误差复算归 T55/T22；本文件只做 f64 离线设计。
//!
//! 测试（私有，include! 内联，integrator 无需注册）：
//! cargo test -p tspi-fir-designer crossover。

use crate::linear::{group_delay_at, response_at, FreqResponse, WindowKind, MAGNITUDE_FLOOR};
use std::fmt;

// ---------------------------------------------------------------------------
// 常量与错误码
// ---------------------------------------------------------------------------

/// 家族标识（与 fir-toolbox family 表并列的“工具”类，不冒充七类预设家族）。
pub const FAMILY: &str = "crossover";

/// 系数/manifest schema（T55 报告器消费，本模块不落盘）。
pub const FIR_SCHEMA: &str = "tspi-fir/v1";

/// 默认运行采样率（spec §1.1）。
pub const DEFAULT_FS: f64 = 48_000.0;

/// 设计工具允许的 fs 下限（Hz）。
pub const MIN_FS_HZ: f64 = 8_000.0;

/// 设计工具允许的 fs 上限（Hz）。
pub const MAX_FS_HZ: f64 = 192_000.0;

/// 交叉点下限（Hz，§4.1 边界）。
pub const MIN_FC_HZ: f64 = 20.0;

/// 交叉点上限（相对 fs 的比例，§4.1 参数域 fc ≤ 0.45·fs）。
pub const MAX_FC_FS_FRACTION: f64 = 0.45;

/// **生产硬上限**：fc ≤ 0.2·fs（含 0.2fs 本身）——Lead 裁决 2026-11-17（RULED）。
pub const FC_PRODUCTION_MAX_FRACTION: f64 = 0.2;

/// 实验模式上限：0.2·fs < fc ≤ 0.225·fs 仅显式 experimental flag + warning 可设计（非生产）；
/// fc > 0.225·fs → INFEASIBLE。
pub const FC_EXPERIMENTAL_MAX_FRACTION: f64 = 0.225;

/// 支撑长度下限（taps）。
pub const MIN_TAPS: usize = 8;

/// 支撑长度上限（taps，设计工具 16384）。
pub const MAX_TAPS: usize = 16_384;

/// 常规 profile 每路由支撑上限（spec §7.2；超出只 warning，由 T56 准入判定）。
pub const REGULAR_PROFILE_TAPS_CAP: usize = 4_096;

/// 默认窗设计衰减（dB）。
pub const DEFAULT_ATTENUATION_DB: f64 = 80.0;

/// 窗设计衰减下限（dB）。
pub const MIN_ATTENUATION_DB: f64 = 40.0;

/// 窗设计衰减上限（dB）。
pub const MAX_ATTENUATION_DB: f64 = 120.0;

/// 合并平坦判据：|S| 在 [fc/2, 2fc] 内相对 max 偏差上限（dB，§4.1）。
pub const SUM_DEVIATION_LIMIT_DB: f64 = 0.5;

/// 幅度近似误差报告门（dB，crossover 带；本实现门，spec 只要求报告）。
pub const MAG_APPROX_LIMIT_DB: f64 = 0.5;

/// 相位近似误差报告门（度，仅 AnalyticPhase；本实现门）。
pub const PHASE_APPROX_LIMIT_DEG: f64 = 5.0;

/// LR 交叉点相对相位容差（度；目标 0°）。
pub const LR_PHASE_FC_LIMIT_DEG: f64 = 0.5;

/// BW 交叉点相对相位容差（度；对解析目标）。
pub const BW_PHASE_FC_LIMIT_DEG: f64 = 5.0;

/// 相位近似误差只在解析幅度 ≥ 该值（dB）的频点计入（弱幅度处相位无意义）。
pub const PHASE_WEIGHT_FLOOR_DB: f64 = -40.0;

/// 测量带下限（Hz，spec §1.2）。
pub const MEASUREMENT_LO_HZ: f64 = 20.0;

/// 测量带上限参考（Hz，spec §1.2；实际取 min(20 kHz, 0.499·fs)）。
pub const MEASUREMENT_HI_HZ: f64 = 20_000.0;

/// Nyquist 保护比例（spec §1.2/§3.1）。
pub const NYQUIST_GUARD_FRACTION: f64 = 0.499;

/// 振铃排除窗（秒，spec §1.4：0.000333·fs）。
pub const RING_WINDOW_SEC: f64 = 0.000_333;

/// 有效支撑 eps（spec §1.3）。
pub const EFFECTIVE_SUPPORT_EPS: f64 = 1e-6;

/// 设计自检网格点数（crossover 带线性网格；T55 规范网格另算）。
pub const GRID_POINTS: usize = 256;

/// 全带报告网格点数（对数网格；T55 规范网格另算）。
pub const FULL_GRID_POINTS: usize = 256;

/// 全带幅度近似误差只统计解析幅度 >= 该值（dB）的频点：深阻带里相对 dB 误差无意义。
pub const FULL_BAND_MAG_FLOOR_DB: f64 = -40.0;

/// FFT 合成的基尺寸（不足 8·taps 时翻倍）。
pub const BASE_FFT_SIZE: usize = 65_536;

/// LR 交叉点解析幅度（dB，= 20·log10(0.5)）。
pub const LR_AT_FC_DB: f64 = -6.020_599_913_279_624;

/// BW 交叉点解析幅度（dB，= 20·log10(1/√2)）。
pub const BW_AT_FC_DB: f64 = -3.010_299_956_639_811_6;

/// BW 偶数阶解析和在 fc 的抬高（dB，= 20·log10(√2)）。
pub const BW_EVEN_SUM_BUMP_DB: f64 = 3.010_299_956_639_811_6;

/// 未知分频类型（spec §4.1：必须显式拒绝未知 type）。
pub const E_FIR_XOVER_TYPE: &str = "E_FIR_XOVER_TYPE";

/// 输出赋值布局/拓扑非法（output_count=0、output_index 越界、同一输出重复赋值/未显式）。
///
/// spec fir-toolbox.md §2.5 已正式登记（终审 errata C，F-22）：触发域 = 布局/拓扑，不属参数域；
/// 参数域错误一律 E_FIR_BAND。
pub const E_FIR_XOVER_LAYOUT: &str = "E_FIR_XOVER_LAYOUT";

/// 合并/近似判据不可达 → INFEASIBLE（设计仍返回完整报告，strict 才 Err）。
pub const E_FIR_XOVER_SUM: &str = "E_FIR_XOVER_SUM";

/// 交叉点相对相位不满足解析目标。
pub const E_FIR_XOVER_PHASE: &str = "E_FIR_XOVER_PHASE";

/// fc 超过生产硬上限（0.2·fs）且未显式开启实验模式（Lead 裁决 2026-11-17，默认拒绝）。
pub const E_FIR_XOVER_FC_LIMIT: &str = "E_FIR_XOVER_FC_LIMIT";

/// fc 超过实验上限（0.225·fs）→ INFEASIBLE（Nyquist fade 护栏范围外）。
pub const E_FIR_XOVER_FC_INFEASIBLE: &str = "E_FIR_XOVER_FC_INFEASIBLE";

/// 支撑/参数越界（沿用 spec §2.5 码）。
pub const E_FIR_TAPS: &str = "E_FIR_TAPS";

/// 频带/采样率参数非法（spec §2.5 唯一参数域码）。
///
/// crossover 的 fs/fc/taps/attenuation/线性相位方法奇数 taps 参数域错误也走本码（与其它 FIR 家族
/// 共用，一个参数域错误只对应一个码；终审 errata C，F-22）。历史 `E_FIR_XOVER_BAND` 已撤销
/// （alias→本码），本模块不输出该名。
pub const E_FIR_BAND: &str = "E_FIR_BAND";

/// 数值非法（NaN/Inf/全零，沿用 spec §2.5 码）。
pub const E_FIR_NUMERIC: &str = "E_FIR_NUMERIC";

/// 极性来源说明：只来自 LR/BW 解析相位关系，不来自 role/听感。
pub const POLARITY_SOURCE_ANALYTIC: &str = "analytic_filter_relation";

/// 极性来源说明：由实测合并偏差选出的对照结果（仅诊断，不用于放行）。
pub const POLARITY_SOURCE_MEASURED: &str = "measured_flat_sum_comparison";

/// 设计/赋值错误（稳定错误码 + 可复算消息）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrossoverError {
    code: &'static str,
    detail: String,
}

impl CrossoverError {
    /// 构造错误。
    pub fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }

    /// 稳定错误码（spec §2.5 或本模块 E_FIR_XOVER_*）。
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// 人类可读细节。
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for CrossoverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for CrossoverError {}

fn err<T>(code: &'static str, detail: impl Into<String>) -> Result<T, CrossoverError> {
    Err(CrossoverError::new(code, detail))
}

// ---------------------------------------------------------------------------
// 类型 / 支路 / 极性 / 方法
// ---------------------------------------------------------------------------

/// 分频类型：LR2/LR4/LR6/LR8、BW1..BW8（阶数指总阶，§4.1）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CrossoverType {
    /// Linkwitz-Riley 2 阶（两级 1 阶 Butterworth，幅度平方）。
    Lr2,
    /// Linkwitz-Riley 4 阶。
    Lr4,
    /// Linkwitz-Riley 6 阶。
    Lr6,
    /// Linkwitz-Riley 8 阶。
    Lr8,
    /// Butterworth 1 阶。
    Bw1,
    /// Butterworth 2 阶。
    Bw2,
    /// Butterworth 3 阶。
    Bw3,
    /// Butterworth 4 阶。
    Bw4,
    /// Butterworth 5 阶。
    Bw5,
    /// Butterworth 6 阶。
    Bw6,
    /// Butterworth 7 阶。
    Bw7,
    /// Butterworth 8 阶。
    Bw8,
}

impl CrossoverType {
    /// 全部合法类型（顺序固定，便于证据枚举）。
    pub const ALL: [CrossoverType; 12] = [
        CrossoverType::Lr2,
        CrossoverType::Lr4,
        CrossoverType::Lr6,
        CrossoverType::Lr8,
        CrossoverType::Bw1,
        CrossoverType::Bw2,
        CrossoverType::Bw3,
        CrossoverType::Bw4,
        CrossoverType::Bw5,
        CrossoverType::Bw6,
        CrossoverType::Bw7,
        CrossoverType::Bw8,
    ];

    /// 总阶数（LR2 = 2、BW3 = 3 …）。
    pub fn order(self) -> usize {
        match self {
            CrossoverType::Lr2 => 2,
            CrossoverType::Lr4 => 4,
            CrossoverType::Lr6 => 6,
            CrossoverType::Lr8 => 8,
            CrossoverType::Bw1 => 1,
            CrossoverType::Bw2 => 2,
            CrossoverType::Bw3 => 3,
            CrossoverType::Bw4 => 4,
            CrossoverType::Bw5 => 5,
            CrossoverType::Bw6 => 6,
            CrossoverType::Bw7 => 7,
            CrossoverType::Bw8 => 8,
        }
    }

    /// 是否 Linkwitz-Riley。
    pub fn is_linkwitz_riley(self) -> bool {
        matches!(
            self,
            CrossoverType::Lr2 | CrossoverType::Lr4 | CrossoverType::Lr6 | CrossoverType::Lr8
        )
    }

    /// 是否 Butterworth。
    pub fn is_butterworth(self) -> bool {
        !self.is_linkwitz_riley()
    }

    /// 规范字符串（"LR4"/"BW3"）。
    pub fn as_str(self) -> &'static str {
        match self {
            CrossoverType::Lr2 => "LR2",
            CrossoverType::Lr4 => "LR4",
            CrossoverType::Lr6 => "LR6",
            CrossoverType::Lr8 => "LR8",
            CrossoverType::Bw1 => "BW1",
            CrossoverType::Bw2 => "BW2",
            CrossoverType::Bw3 => "BW3",
            CrossoverType::Bw4 => "BW4",
            CrossoverType::Bw5 => "BW5",
            CrossoverType::Bw6 => "BW6",
            CrossoverType::Bw7 => "BW7",
            CrossoverType::Bw8 => "BW8",
        }
    }

    /// 解析 type 字符串（大小写不敏感、去空白）；未知 type 一律 E_FIR_XOVER_TYPE。
    pub fn parse(text: &str) -> Result<Self, CrossoverError> {
        let norm = text.trim().to_ascii_uppercase();
        for kind in Self::ALL {
            if kind.as_str() == norm {
                return Ok(kind);
            }
        }
        err(
            E_FIR_XOVER_TYPE,
            format!("未知 crossover 类型 {text:?}；允许 LR2/LR4/LR6/LR8/BW1..BW8（§4.1 显式拒绝未知 type）"),
        )
    }

    /// 交叉点解析相对相位目标（度，wrap 到 (−180,180]；HP/LP 的解析 ∠ 关系，**未含极性**）。
    ///
    /// 推导：HP/LP = s^order，s = j 时 = j^order，故 = 90°·order (mod 360°)。
    pub fn analytic_relative_phase_raw_deg(self) -> f64 {
        wrap_deg(90.0 * self.order() as f64)
    }

    /// 家族相对极性（Lead 裁决 2026-11-17，RULED；只由类型解析关系决定，与 role 名/听感无关）：
    ///
    /// - **LR 偶数阶**（LR2/LR4/LR6/LR8）：两支在 fc **同相（相对 0°）**求和幅频平坦。
    ///   LR2/LR6 的解析 HP/LP 相差 180°，令 HP 反相后相对相位回到 0°；LR4/LR8 本就同相。
    /// - **BW 奇数阶**（BW1/3/5/7）：允许/要求**一支反相**（极性关系 180°）。解析和
    ///   (1∓s^n)/D 对奇数 n 仍为全通，故反相后两项和幅频仍平坦；fc 相对**相位**按解析目标
    ///   （±90°），**不强制 0°**、也不要求线性相位。
    /// - BW 偶数阶：解析和本身不平坦（@fc +3.0103 dB），极性仅用于相位对齐（order%4==2 反相）；
    ///   相位/极性判据为 NOT_APPLICABLE（只报告、不计 pass，拒收只走 SUM；终审 F-24）。
    pub fn analytic_polarity(self) -> BranchPolarity {
        let invert = if self.is_butterworth() && self.order() % 2 == 1 {
            true // BW 奇数阶：一支反相（家族极性 180°）
        } else {
            self.order() % 4 == 2 // LR2/LR6、BW2/BW6
        };
        if invert {
            BranchPolarity {
                lowpass: Polarity::Normal,
                highpass: Polarity::Inverted,
            }
        } else {
            BranchPolarity::SAME
        }
    }

    /// 家族相对极性（度）：两支同极性 = 0°，一支反相 = 180°（只描述极性关系，不含相位）。
    pub fn family_relative_polarity_deg(self) -> f64 {
        if self.analytic_polarity().is_same() {
            0.0
        } else {
            180.0
        }
    }

    /// 家族 fc 相位验收规则说明（RULED 2026-11-17）。
    pub fn family_phase_rule(self) -> &'static str {
        if self.is_linkwitz_riley() {
            "LR even order: two branches in phase at fc (relative 0 deg, after family polarity); magnitude-flat sum"
        } else if self.order() % 2 == 1 {
            "BW odd order: one branch inverted (180 deg polarity) allowed/required; fc phase follows analytic target (+-90 deg), NOT forced to 0 deg; no linear-phase requirement"
        } else {
            "BW even order (F-24): phase criteria NOT_APPLICABLE (report-only, never counted as pass); analytic LP+HP sum is not flat, rejection only via E_FIR_XOVER_SUM; no in-phase/flat acceptance"
        }
    }

    /// 解析复数和是否恒为全通（|S| ≡ 1）：LR 全部成立；BW 仅奇数阶成立（偶数阶 @fc +3.01 dB）。
    pub fn analytic_sum_is_flat(self) -> bool {
        self.is_linkwitz_riley() || self.order() % 2 == 1
    }

    /// 解析和在 fc 的理论抬高（dB；偶数阶 BW = 3.0103，其余 0）。
    pub fn analytic_sum_bump_at_fc_db(self) -> f64 {
        if self.is_butterworth() && self.order() % 2 == 0 {
            BW_EVEN_SUM_BUMP_DB
        } else {
            0.0
        }
    }

    /// 解析交叉点幅度（dB；LR = −6.0206，BW = −3.0103）。
    pub fn analytic_magnitude_at_fc_db(self) -> f64 {
        if self.is_linkwitz_riley() {
            LR_AT_FC_DB
        } else {
            BW_AT_FC_DB
        }
    }
}

/// 支路：LP = 低通支，HP = 高通支。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Branch {
    /// 低通支（woofer/sub）。
    Lowpass,
    /// 高通支（mid/tweeter）。
    Highpass,
}

impl Branch {
    /// 规范字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Branch::Lowpass => "lowpass",
            Branch::Highpass => "highpass",
        }
    }

    /// 另一支。
    pub fn other(self) -> Branch {
        match self {
            Branch::Lowpass => Branch::Highpass,
            Branch::Highpass => Branch::Lowpass,
        }
    }
}

/// 支路极性（系数整体符号；§4.2：invert = 系数乘 −1）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Polarity {
    /// 正极性。
    Normal,
    /// 反相（系数乘 −1）。
    Inverted,
}

impl Polarity {
    /// 规范字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Polarity::Normal => "normal",
            Polarity::Inverted => "inverted",
        }
    }

    /// 符号（+1/−1）。
    pub fn sign(self) -> f64 {
        match self {
            Polarity::Normal => 1.0,
            Polarity::Inverted => -1.0,
        }
    }

    /// 翻转。
    pub fn flipped(self) -> Polarity {
        match self {
            Polarity::Normal => Polarity::Inverted,
            Polarity::Inverted => Polarity::Normal,
        }
    }
}

/// 两支极性组合（LP 固定 Normal，HP 可 Normal/Inverted）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BranchPolarity {
    /// 低通支极性。
    pub lowpass: Polarity,
    /// 高通支极性。
    pub highpass: Polarity,
}

impl BranchPolarity {
    /// 两支同极性。
    pub const SAME: BranchPolarity = BranchPolarity {
        lowpass: Polarity::Normal,
        highpass: Polarity::Normal,
    };

    /// 指定支路极性。
    pub fn for_branch(self, branch: Branch) -> Polarity {
        match branch {
            Branch::Lowpass => self.lowpass,
            Branch::Highpass => self.highpass,
        }
    }

    /// HP 反相后的组合（对照）。
    pub fn flipped_highpass(self) -> BranchPolarity {
        BranchPolarity {
            lowpass: self.lowpass,
            highpass: self.highpass.flipped(),
        }
    }

    /// 两支是否同极性。
    pub fn is_same(self) -> bool {
        self.lowpass == self.highpass
    }
}

/// FIR 近似方法（必须显式；无“自动挑舒服的”）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrossoverMethod {
    /// 解析复频响 + 公共体延迟 + Nyquist fade 后 IFFT 矩形截断：有限近似解析**幅度与相位**（默认）。
    AnalyticPhase,
    /// 只按解析 |LP|/|HP| 幅度目标做线性相位（§3.1 合成）；相位由结构固定（0°/180°）。
    /// **非规范对照**：Lead 裁决 2026-11-17 选定 AnalyticPhase 为 §4.1 规范构造；本法仅用于
    /// 量化“线性相位幅度构造 vs BW 相位判据”的冲突，判据 xover_normative_construction 失败。
    LinearPhaseMagnitude,
}

impl CrossoverMethod {
    /// 规范字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            CrossoverMethod::AnalyticPhase => "analytic_phase",
            CrossoverMethod::LinearPhaseMagnitude => "linear_phase_magnitude",
        }
    }
}

// ---------------------------------------------------------------------------
// 解析模型（LR / BW 复频响）
// ---------------------------------------------------------------------------

/// 内部复数量（仅本模块解析模型使用；公开频响用 FreqResponse）。
#[derive(Clone, Copy, Debug, PartialEq)]
struct Cplx {
    re: f64,
    im: f64,
}

impl Cplx {
    const ONE: Cplx = Cplx { re: 1.0, im: 0.0 };

    const fn new(re: f64, im: f64) -> Self {
        Self { re, im }
    }

    fn sub(self, o: Self) -> Self {
        Self::new(self.re - o.re, self.im - o.im)
    }

    fn mul(self, o: Self) -> Self {
        Self::new(
            self.re * o.re - self.im * o.im,
            self.re * o.im + self.im * o.re,
        )
    }

    fn div(self, o: Self) -> Self {
        let den = o.re * o.re + o.im * o.im;
        Self::new(
            (self.re * o.re + self.im * o.im) / den,
            (self.im * o.re - self.re * o.im) / den,
        )
    }

    fn powi(self, mut n: usize) -> Self {
        let mut acc = Self::ONE;
        let mut base = self;
        while n > 0 {
            if n & 1 == 1 {
                acc = acc.mul(base);
            }
            base = base.mul(base);
            n >>= 1;
        }
        acc
    }
}

/// Butterworth n 阶分母 D_n(s) = Π(s − p_k)，p_k = exp(jπ(2k+n+1)/(2n))。
fn butterworth_denominator(order: usize, s: Cplx) -> Cplx {
    let mut d = Cplx::ONE;
    for k in 0..order {
        let angle =
            std::f64::consts::PI * (2.0 * k as f64 + order as f64 + 1.0) / (2.0 * order as f64);
        let pole = Cplx::new(angle.cos(), angle.sin());
        d = d.mul(s.sub(pole));
    }
    d
}

/// 解析支路复频响 H(j·f/fc)（LRn = [BW(n/2)]²，幅度平方；BWn = BW(n)；§4.1）。
///
/// fc_hz 必须 > 0。
pub fn analytic_response(
    kind: CrossoverType,
    branch: Branch,
    fc_hz: f64,
    f_hz: f64,
) -> FreqResponse {
    let order = kind.order();
    let s = Cplx::new(0.0, f_hz / fc_hz);
    let h = if kind.is_linkwitz_riley() {
        let d = butterworth_denominator(order / 2, s);
        let den = d.mul(d);
        match branch {
            Branch::Lowpass => Cplx::ONE.div(den),
            Branch::Highpass => s.powi(order).div(den),
        }
    } else {
        let den = butterworth_denominator(order, s);
        match branch {
            Branch::Lowpass => Cplx::ONE.div(den),
            Branch::Highpass => s.powi(order).div(den),
        }
    };
    FreqResponse { re: h.re, im: h.im }
}

/// 交叉点解析相对相位（度，已含解析极性；LR 全阶 = 0°，BW1/3/5/7 = ±90°，BW2/6 = 0°）。
pub fn analytic_relative_phase_at_fc_deg(kind: CrossoverType) -> f64 {
    let fc = 1_000.0;
    let lp = analytic_response(kind, Branch::Lowpass, fc, fc).phase_deg();
    let hp = analytic_response(kind, Branch::Highpass, fc, fc).phase_deg();
    // 极性 invert = 系数乘 −1 → 相位 +180°（不是把相位取负）
    let hp_applied = if kind.analytic_polarity().highpass == Polarity::Inverted {
        hp + 180.0
    } else {
        hp
    };
    wrap_deg(hp_applied - lp)
}

/// 解析群延迟（samples，中心差分；step = max(0.01 Hz, f·1e−4)，不超过 spec §1.2 上限）。
fn analytic_group_delay_samples(
    kind: CrossoverType,
    branch: Branch,
    fc_hz: f64,
    fs: f64,
    f_hz: f64,
) -> f64 {
    let step = (f_hz * 1e-4).max(0.01);
    let lo = (f_hz - step).max(0.0);
    let hi = f_hz + step;
    let phi_lo = analytic_response(kind, branch, fc_hz, lo).phase_deg();
    let phi_hi = analytic_response(kind, branch, fc_hz, hi).phase_deg();
    let dphi_rad = wrap_deg(phi_hi - phi_lo).to_radians();
    -dphi_rad * fs / (4.0 * std::f64::consts::PI * step)
}

/// wrap 到 (−180, 180] 的度。
pub fn wrap_deg(deg: f64) -> f64 {
    let mut x = deg % 360.0;
    if x <= -180.0 {
        x += 360.0;
    } else if x > 180.0 {
        x -= 360.0;
    }
    x
}

// ---------------------------------------------------------------------------
// 设计参数与校验
// ---------------------------------------------------------------------------

/// crossover 设计参数（全部显式；无 role 输入字段）。
#[derive(Clone, Debug, PartialEq)]
pub struct CrossoverSpec {
    /// 运行采样率（Hz；必须等于实际运行 fs，§7.2）。
    pub fs: f64,
    /// 分频类型。
    pub kind: CrossoverType,
    /// 交叉点 fc（Hz；[20, 0.45·fs]）。
    pub fc_hz: f64,
    /// 两支支撑长度（同 L；[8, 16384]）。
    pub taps: usize,
    /// 窗设计衰减（dB；T39 两种方法均为矩形截断，本字段只登记/校验，不参与系数构造）。
    pub attenuation_db: f64,
    /// 有限窗类型（与 T10 linear 同一枚举；仅 LinearPhaseMagnitude 生效）。
    pub window: WindowKind,
    /// FIR 近似方法。
    pub method: CrossoverMethod,
    /// 实验模式开关：false（默认）时 fc>0.2·fs 直接拒绝；true 时 0.2–0.225·fs 可设计，
    /// 但报告/判据标 EXPERIMENTAL 且**不得被生产接受**（Lead 裁决 2026-11-17）。
    pub experimental: bool,
}

impl CrossoverSpec {
    /// 基本构造：Kaiser（登记）、80 dB、CrossoverMethod::AnalyticPhase。
    pub fn new(fs: f64, kind: CrossoverType, fc_hz: f64, taps: usize) -> Self {
        Self {
            fs,
            kind,
            fc_hz,
            taps,
            attenuation_db: DEFAULT_ATTENUATION_DB,
            window: WindowKind::Kaiser,
            method: CrossoverMethod::AnalyticPhase,
            experimental: false,
        }
    }

    /// 设置近似方法。
    pub fn with_method(mut self, method: CrossoverMethod) -> Self {
        self.method = method;
        self
    }

    /// 设置窗。
    pub fn with_window(mut self, window: WindowKind) -> Self {
        self.window = window;
        self
    }

    /// 设置窗设计衰减。
    pub fn with_attenuation(mut self, attenuation_db: f64) -> Self {
        self.attenuation_db = attenuation_db;
        self
    }

    /// 显式开启/关闭实验模式（默认 false = 生产口径；fc>0.2·fs 必须显式开启）。
    pub fn with_experimental(mut self, experimental: bool) -> Self {
        self.experimental = experimental;
        self
    }

    /// 校验参数并返回内部校验结果。
    pub fn validate(&self) -> Result<ValidatedCrossoverSpec, CrossoverError> {
        if !self.fs.is_finite() || self.fs < MIN_FS_HZ || self.fs > MAX_FS_HZ {
            return err(
                E_FIR_BAND,
                format!(
                    "fs={} 非法；允许 [{MIN_FS_HZ}, {MAX_FS_HZ}] Hz（FIR fs 必须 == 运行 fs，§7.2）",
                    self.fs
                ),
            );
        }
        if !self.fc_hz.is_finite() || self.fc_hz < MIN_FC_HZ {
            return err(
                E_FIR_BAND,
                format!("fc={} 非法；下限 {MIN_FC_HZ} Hz（§4.1）", self.fc_hz),
            );
        }
        let fc_max = MAX_FC_FS_FRACTION * self.fs;
        if self.fc_hz > fc_max {
            return err(
                E_FIR_BAND,
                format!(
                    "fc={} > 0.45·fs={fc_max}（§4.1 参数域 fc ∈ [20, 0.45fs]）",
                    self.fc_hz
                ),
            );
        }
        // Lead 裁决 2026-11-17（RULED）：>0.225·fs → INFEASIBLE（Nyquist fade 护栏范围外）
        let fc_experimental_max = FC_EXPERIMENTAL_MAX_FRACTION * self.fs;
        if self.fc_hz > fc_experimental_max {
            return err(
                E_FIR_XOVER_FC_INFEASIBLE,
                format!(
                    "INFEASIBLE: fc={} > 0.225·fs={fc_experimental_max}（Nyquist fade 护栏 0.45fs–0.5fs 之外，\
                     Lead 裁决 2026-11-17 上限 0.225fs；生产上限 0.2fs）",
                    self.fc_hz
                ),
            );
        }
        // 生产硬上限 0.2·fs（=0.2fs 允许）；0.2–0.225fs 仅显式 experimental flag（默认拒绝）
        let fc_production_max = FC_PRODUCTION_MAX_FRACTION * self.fs;
        if self.fc_hz > fc_production_max && !self.experimental {
            return err(
                E_FIR_XOVER_FC_LIMIT,
                format!(
                    "fc={} > 生产硬上限 0.2·fs={fc_production_max}（Lead 裁决 2026-11-17）；\
                     0.2–0.225fs 仅实验模式，需 CrossoverSpec::with_experimental(true) 显式开启且报告标 EXPERIMENTAL（不得生产接受）",
                    self.fc_hz
                ),
            );
        }
        if self.taps < MIN_TAPS || self.taps > MAX_TAPS {
            return err(
                E_FIR_TAPS,
                format!(
                    "taps={} 越界；允许 [{MIN_TAPS}, {MAX_TAPS}]（§2.1/§7.2）",
                    self.taps
                ),
            );
        }
        if self.method == CrossoverMethod::LinearPhaseMagnitude && self.taps % 2 == 0 {
            return err(
                E_FIR_TAPS,
                format!(
                    "LinearPhaseMagnitude 要求奇数 taps={}：偶数长对称 FIR（Type II）强制 H(0.5fs)=0，\
                     无法承载 HP 支在 Nyquist 的增益；请用奇数 taps 或 AnalyticPhase 方法",
                    self.taps
                ),
            );
        }
        if !self.attenuation_db.is_finite()
            || self.attenuation_db < MIN_ATTENUATION_DB
            || self.attenuation_db > MAX_ATTENUATION_DB
        {
            return err(
                E_FIR_BAND,
                format!(
                    "attenuation_db={} 越界；允许 [{MIN_ATTENUATION_DB}, {MAX_ATTENUATION_DB}] dB",
                    self.attenuation_db
                ),
            );
        }
        let mut fft_size = BASE_FFT_SIZE;
        while fft_size < self.taps.saturating_mul(8) {
            fft_size <<= 1;
        }
        Ok(ValidatedCrossoverSpec {
            spec: self.clone(),
            fft_size,
        })
    }
}

/// 校验后的设计参数（内部使用；FFT 尺寸与窗 beta 已解析）。
#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedCrossoverSpec {
    /// 原始参数。
    pub spec: CrossoverSpec,
    /// 合成用 FFT 尺寸（>= 8·taps，2 的幂）。
    pub fft_size: usize,
}

impl ValidatedCrossoverSpec {
    /// 采样率。
    pub fn fs(&self) -> f64 {
        self.spec.fs
    }

    /// 交叉点。
    pub fn fc_hz(&self) -> f64 {
        self.spec.fc_hz
    }

    /// 支撑长度。
    pub fn taps(&self) -> usize {
        self.spec.taps
    }
}

// ---------------------------------------------------------------------------
// FIR 合成（FFT 频率采样 + 公共体延迟 + Nyquist fade + 矩形截断）
// ---------------------------------------------------------------------------

/// in-place radix-2 FFT（f64；inverse 时含 1/N 归一化）。
fn fft_in_place(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    debug_assert_eq!(n, im.len());
    debug_assert!(n.is_power_of_two());
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let half = n / 2;
    let mut tw_re = vec![0.0f64; half.max(1)];
    let mut tw_im = vec![0.0f64; half.max(1)];
    for k in 0..half {
        let angle = -2.0 * std::f64::consts::PI * k as f64 / n as f64;
        let (s, c) = angle.sin_cos();
        tw_re[k] = c;
        tw_im[k] = s;
    }
    let mut len = 2usize;
    while len <= n {
        let half_len = len / 2;
        let stride = n / len;
        let mut start = 0usize;
        while start < n {
            for k in 0..half_len {
                let t = k * stride;
                let (wr, wi) = if inverse {
                    (tw_re[t], -tw_im[t])
                } else {
                    (tw_re[t], tw_im[t])
                };
                let u = start + k;
                let v = u + half_len;
                let xr = re[v] * wr - im[v] * wi;
                let xi = re[v] * wi + im[v] * wr;
                re[v] = re[u] - xr;
                im[v] = im[u] - xi;
                re[u] += xr;
                im[u] += xi;
            }
            start += len;
        }
        len <<= 1;
    }
    if inverse {
        let scale = 1.0 / n as f64;
        for x in re.iter_mut() {
            *x *= scale;
        }
        for x in im.iter_mut() {
            *x *= scale;
        }
    }
}

/// Nyquist 护栏 fade（AnalyticPhase）：[0, 0.45fs] 内 =1，[0.45fs, 0.5fs] 内升余弦到 0。
///
/// 目的：连续时间 LR/BW 在 fs/2 处的复相位一般不是 0/180°（例如 LR4 HP 在 fs/2 的相位可达
/// 十几度），而实数 FIR 在 Nyquist 的响应必须为实数；若直接把复数 Nyquist 值取实部，会在周期性
/// 频谱上形成跳变，IDFT 出现 ~1/n 长尾，短 taps 时频带误差可达数 dB。对 0.45fs 以上做平滑 fade
/// 到 0 使目标频谱在 Nyquist 连续（fade=0），长尾消失；0.45fs 以下（含 20 kHz 规范测量带与
/// 常规分频带）不受影响。代价：fc > 0.225·fs 的 crossover 带上界进入 fade 区，近似误差按判据
/// 如实报告（INFEASIBLE），不静默放行。
pub fn nyquist_fade(fs: f64, f_hz: f64) -> f64 {
    let lo = 0.45 * fs;
    let hi = 0.5 * fs;
    if f_hz <= lo {
        1.0
    } else if f_hz >= hi {
        0.0
    } else {
        let u = (f_hz - lo) / (hi - lo);
        0.5 * (1.0 + (std::f64::consts::PI * u).cos())
    }
}

/// 合成一支的有限 FIR 系数（未应用极性）。
///
/// - AnalyticPhase（默认）：解析复频响 H(j·f/fc) 乘 Nyquist 护栏 fade（见 nyquist_fade）并加
///   公共体延迟 e^{-j2πfD/fs}（D=(taps−1)/2），IFFT 得实系数、取 n=0..taps−1 的矩形截断
///   （fade 保证目标在 Nyquist 连续，长尾已消失，无需再加 taper）。
/// - LinearPhaseMagnitude：按解析 |H| 频域采样 → IFFT 得零相位核，循环移位 D=(taps−1)/2 并乘对称窗；
///   Type I 线性相位（要求奇数 taps）。
///
/// 两种方法两支共用同一 taps/方法/D，保证相对 D=0（D 本身可为整数/半整数，§1.3）。
fn synthesize_branch(spec: &ValidatedCrossoverSpec, branch: Branch) -> Vec<f64> {
    let n_fft = spec.fft_size;
    let half = n_fft / 2;
    let taps = spec.spec.taps;
    let delay = declared_delay_samples(spec.spec.method, taps);
    let mut re = vec![0.0f64; n_fft];
    let mut im = vec![0.0f64; n_fft];
    for k in 0..=half {
        let f = k as f64 * spec.spec.fs / n_fft as f64;
        let h = analytic_response(spec.spec.kind, branch, spec.spec.fc_hz, f);
        match spec.spec.method {
            CrossoverMethod::AnalyticPhase => {
                let phase = -2.0 * std::f64::consts::PI * f * delay / spec.spec.fs;
                let (sin, cos) = phase.sin_cos();
                let fade = nyquist_fade(spec.spec.fs, f);
                let t_re = (h.re * cos - h.im * sin) * fade;
                let t_im = (h.re * sin + h.im * cos) * fade;
                re[k] = t_re;
                im[k] = if k == half { 0.0 } else { t_im };
                if k > 0 && k < half {
                    re[n_fft - k] = t_re;
                    im[n_fft - k] = -t_im;
                }
            }
            CrossoverMethod::LinearPhaseMagnitude => {
                let a = h.magnitude();
                re[k] = a;
                if k > 0 && k < half {
                    re[n_fft - k] = a;
                }
            }
        }
    }
    fft_in_place(&mut re, &mut im, true);
    let mut out = vec![0.0f64; taps];
    match spec.spec.method {
        CrossoverMethod::AnalyticPhase => {
            for (n, item) in out.iter_mut().enumerate() {
                *item = re[n];
            }
        }
        CrossoverMethod::LinearPhaseMagnitude => {
            // 零相位对称核循环移位 D 后矩形截断：目标幅度函数平滑时矩形截断的主瓣比
            // Kaiser(β=8.96) 更窄（后者会以 ~117 Hz 主瓣卷积 fc 附近的过渡，低 fc 时造成
            // 数 dB 误差）；通带/阻带纹波由目标平滑性与 taps 控制，measurement 报告实际误差。
            let delay_int = (taps - 1) / 2;
            for (n, item) in out.iter_mut().enumerate() {
                let idx = (n as isize - delay_int as isize).rem_euclid(n_fft as isize) as usize;
                *item = re[idx];
            }
        }
    }
    out
}

/// 各方法的声明体延迟 D（samples；两支相同，相对 D=0；可为整数/半整数，§1.3）。
pub fn declared_delay_samples(_method: CrossoverMethod, taps: usize) -> f64 {
    (taps.saturating_sub(1)) as f64 / 2.0
}

// ---------------------------------------------------------------------------
// 网格与通用数值工具
// ---------------------------------------------------------------------------

fn linear_grid(lo: f64, hi: f64, points: usize) -> Vec<f64> {
    if points == 0 {
        return Vec::new();
    }
    if points == 1 || (hi - lo).abs() <= f64::EPSILON {
        return vec![lo];
    }
    (0..points)
        .map(|i| lo + (hi - lo) * i as f64 / (points - 1) as f64)
        .collect()
}

fn log_grid(lo: f64, hi: f64, points: usize) -> Vec<f64> {
    if points == 0 || hi <= lo || lo <= 0.0 {
        return Vec::new();
    }
    if points == 1 {
        return vec![lo];
    }
    let ratio = hi / lo;
    (0..points)
        .map(|i| lo * ratio.powf(i as f64 / (points - 1) as f64))
        .collect()
}

fn db_from_mag(mag: f64) -> f64 {
    20.0 * mag.max(MAGNITUDE_FLOOR).log10()
}

/// crossover 测量带 [max(20, fc/2), min(2·fc, 0.499·fs)]（§4.1 合并判据带）。
pub fn crossover_band(fc_hz: f64, fs: f64) -> [f64; 2] {
    let lo = (fc_hz / 2.0).max(MEASUREMENT_LO_HZ);
    let hi = (fc_hz * 2.0).min(NYQUIST_GUARD_FRACTION * fs);
    [lo, hi]
}

fn full_report_band(fs: f64) -> [f64; 2] {
    [
        MEASUREMENT_LO_HZ,
        MEASUREMENT_HI_HZ.min(NYQUIST_GUARD_FRACTION * fs),
    ]
}

fn max_abs(values: &[f64]) -> f64 {
    values.iter().fold(0.0f64, |m, v| m.max(v.abs()))
}

// ---------------------------------------------------------------------------
// 报告结构
// ---------------------------------------------------------------------------

/// 有效支撑（spec §1.3）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SupportReport {
    /// 首个超过 eps·max|h| 的样本下标。
    pub first_effective: usize,
    /// 最后一个超过 eps·max|h| 的样本下标。
    pub last_effective: usize,
    /// 有效支撑长度。
    pub effective_taps: usize,
    /// max|h|。
    pub max_abs: f64,
    /// 主峰下标。
    pub peak_index: usize,
}

/// 前/后振铃（spec §1.4；None = n/a，主峰贴近支撑边界）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RingingReport {
    /// 排除窗 W = ceil(0.000333·fs)。
    pub exclusion_samples: usize,
    /// 前振铃（dB，相对 max|h|）。
    pub pre_db: Option<f64>,
    /// 后振铃（dB）。
    pub post_db: Option<f64>,
    /// 前振铃能量占比（dB）。
    pub pre_energy_db: Option<f64>,
    /// 后振铃能量占比（dB）。
    pub post_energy_db: Option<f64>,
}

/// 幅度近似误差（FIR vs 解析）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MagnitudeApproxReport {
    /// crossover 测量带 [fc/2, 2fc]。
    pub band_hz: [f64; 2],
    /// crossover 带网格点数。
    pub grid_points: usize,
    /// crossover 带 max |Δmag|（dB）。
    pub max_abs_err_db: f64,
    /// crossover 带平均 |Δmag|（dB）。
    pub mean_abs_err_db: f64,
    /// 最差频点（Hz）。
    pub worst_hz: f64,
    /// fc 处误差（dB）。
    pub at_fc_err_db: f64,
    /// 全测量带 max |Δmag|（dB）。
    pub full_band_max_abs_err_db: f64,
    /// 全测量带最差频点（Hz）。
    pub full_band_worst_hz: f64,
    /// 解析幅度 @fc（dB）。
    pub analytic_at_fc_db: f64,
    /// FIR 幅度 @fc（dB）。
    pub measured_at_fc_db: f64,
}

/// 相位近似误差（FIR vs 解析；按声明 D 去掉体延迟后比较）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhaseApproxReport {
    /// 测量带。
    pub band_hz: [f64; 2],
    /// 网格点数。
    pub grid_points: usize,
    /// 计入误差的频点数（解析幅度 ≥ PHASE_WEIGHT_FLOOR_DB）。
    pub considered_points: usize,
    /// 弱幅度门（dB）。
    pub weight_floor_db: f64,
    /// max |Δphase|（度）。
    pub max_abs_err_deg: f64,
    /// 平均 |Δphase|（度）。
    pub mean_abs_err_deg: f64,
    /// 最差频点（Hz）。
    pub worst_hz: f64,
    /// fc 处相位误差（度）。
    pub at_fc_err_deg: f64,
    /// 方法说明（相位是否被设计目标）。
    pub method_note: &'static str,
}

/// 群延迟报告（FIR 实测 vs 解析）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupDelayReport {
    /// 测量带。
    pub band_hz: [f64; 2],
    /// 网格点数。
    pub grid_points: usize,
    /// 声明体延迟（samples；本实现为整数）。
    pub declared_delay_samples: f64,
    /// 声明体延迟（ms）。
    pub declared_delay_ms: f64,
    /// 实测 GD 最小值（samples）。
    pub min_samples: f64,
    /// 实测 GD 最大值（samples）。
    pub max_samples: f64,
    /// 实测 GD 平均（samples）。
    pub mean_samples: f64,
    /// fc 处 GD（samples）。
    pub at_fc_samples: f64,
    /// 1 kHz 处 GD（samples）。
    pub at_1khz_samples: f64,
    /// 物理 GD @fc（实测总 GD − 声明体延迟 D，samples）。
    pub physical_at_fc_samples: f64,
    /// 解析 GD @fc（samples）。
    pub analytic_at_fc_samples: f64,
    /// 与解析 GD 的 max |Δ|（samples，crossover 带）。
    pub max_abs_err_samples: f64,
}

/// 单支设计产物（系数已应用极性）。
#[derive(Clone, Debug, PartialEq)]
pub struct BranchDesign {
    /// 支路。
    pub branch: Branch,
    /// 支撑长度。
    pub taps: usize,
    /// 已应用极性。
    pub applied_polarity: Polarity,
    /// 声明体延迟（samples）。
    pub declared_delay_samples: f64,
    /// 声明体延迟（ms）。
    pub declared_delay_ms: f64,
    /// FIR 系数（f64，长度 = taps；已乘极性）。
    pub coefficients: Vec<f64>,
    /// 有效支撑。
    pub support: SupportReport,
    /// 幅度近似。
    pub magnitude: MagnitudeApproxReport,
    /// 相位近似。
    pub phase: PhaseApproxReport,
    /// 群延迟。
    pub group_delay: GroupDelayReport,
    /// 振铃。
    pub ringing: RingingReport,
    /// DC 增益（Σh，线性）。
    pub dc_gain: f64,
    /// DC 增益（dB）。
    pub dc_gain_db: f64,
    /// |H(0.5fs)|（dB）。
    pub nyquist_mag_db: f64,
    /// max|h|。
    pub max_abs_h: f64,
    /// Σ|h|（峰值余量参考，spec §1.5）。
    pub sum_abs_h: f64,
}

/// 两支合并报告（S = H_LP + H_HP）。
#[derive(Clone, Debug, PartialEq)]
pub struct MergeReport {
    /// 测量带 [fc/2, 2fc]。
    pub band_hz: [f64; 2],
    /// 网格点数。
    pub grid_points: usize,
    /// 实际使用的极性。
    pub polarity: BranchPolarity,
    /// 极性来源（恒为 POLARITY_SOURCE_ANALYTIC，禁止听感）。
    pub polarity_source: &'static str,
    /// 家族相对极性（度）：0 = 两支同极性；180 = 一支反相（只描述极性，不含相位）。
    pub family_relative_polarity_deg: f64,
    /// 家族 fc 相位验收规则（RULED 2026-11-17；LR 同相 0° / BW 奇数阶不强制 0°）。
    pub family_phase_rule: &'static str,
    /// 实测偏差更小的极性（对照诊断，不用于放行）。
    pub measured_best_polarity: BranchPolarity,
    /// 实测 |S| 相对 max 偏差（dB，使用极性）。
    pub flat_max_deviation_db: f64,
    /// 判据上限（dB）。
    pub flat_limit_db: f64,
    /// 是否满足合并平坦判据。
    pub flat_pass: bool,
    /// 相反极性下的实测偏差（dB）。
    pub opposite_polarity_max_deviation_db: f64,
    /// |S| 最大值（dB）。
    pub sum_max_db: f64,
    /// |S| 最小值（dB）。
    pub sum_min_db: f64,
    /// 解析复数和 |S_ideal| 相对 max 偏差（dB，同一带）。
    pub analytic_sum_max_deviation_db: f64,
    /// 解析复数和是否恒全通（LR 全阶 / BW 奇数阶）。
    pub analytic_sum_flat: bool,
    /// 解析交叉点相对相位（度，已含解析极性）。
    pub analytic_relative_phase_at_fc_deg: f64,
    /// 实测交叉点相对相位（度，已含应用极性）。
    pub measured_relative_phase_at_fc_deg: f64,
    /// 实测 − 解析（度）。
    pub relative_phase_error_deg: f64,
    /// 两支声明体延迟之差（samples；必须为整数 0）。
    pub relative_delay_samples: f64,
    /// 相对延迟是否为整数 samples。
    pub relative_delay_is_integer: bool,
    /// 合并和 @fc 的群延迟（samples）。
    pub sum_group_delay_at_fc_samples: f64,
    /// 解析和的理论说明。
    pub theoretical_sum: &'static str,
}

/// 单条判据（与 T33/T55 报告口径一致）。
#[derive(Clone, Debug, PartialEq)]
pub struct Criterion {
    /// 稳定 id。
    pub id: &'static str,
    /// 判据说明。
    pub statement: &'static str,
    /// 实测值。
    pub measured: Option<f64>,
    /// 限值（None = 仅报告/report-only）。
    pub limit: Option<f64>,
    /// 单位。
    pub unit: &'static str,
    /// 是否通过。
    pub pass: bool,
}

/// 判据汇总。
#[derive(Clone, Debug, PartialEq, Default)]
pub struct NormativeReport {
    /// 判据列表。
    pub criteria: Vec<Criterion>,
}

impl NormativeReport {
    /// 全部通过。
    pub fn all_pass(&self) -> bool {
        self.criteria.iter().all(|c| c.pass)
    }

    /// 失败判据 id。
    pub fn failed_ids(&self) -> Vec<&'static str> {
        self.criteria
            .iter()
            .filter(|c| !c.pass)
            .map(|c| c.id)
            .collect()
    }

    /// 按 id 取判据。
    pub fn get(&self, id: &str) -> Option<&Criterion> {
        self.criteria.iter().find(|c| c.id == id)
    }
}

/// crossover 设计产物（系数 + 判据 + 测量报告；WAV/manifest 导出归 T55）。
#[derive(Clone, Debug, PartialEq)]
pub struct CrossoverDesign {
    /// 原始参数。
    pub spec: CrossoverSpec,
    /// 设计方法。
    pub method: CrossoverMethod,
    /// fc 档位（RULED 2026-11-17）："production"（fc ≤ 0.2fs）/ "experimental"（0.2–0.225fs，显式 flag）。
    pub fc_tier: &'static str,
    /// 分频类型。
    pub kind: CrossoverType,
    /// 低通支。
    pub lowpass: BranchDesign,
    /// 高通支。
    pub highpass: BranchDesign,
    /// 合并报告。
    pub merge: MergeReport,
    /// 判据。
    pub criteria: NormativeReport,
    /// 警告（不静默隐藏近似/冲突）。
    pub warnings: Vec<String>,
    /// 静态说明（口径/未测项）。
    pub notes: Vec<&'static str>,
}

impl CrossoverDesign {
    /// 家族标识。
    pub fn family(&self) -> &'static str {
        FAMILY
    }

    /// 是否满足全部判据（失败 = 该参数/方法组合 INFEASIBLE，不是静默放行）。
    ///
    /// BW 偶数阶的相位/极性项为 NOT_APPLICABLE（只报告、不计 pass），其拒收码固定为
    /// E_FIR_XOVER_SUM（见 design_strict；终审 F-24）。
    pub fn accepted(&self) -> bool {
        self.criteria.all_pass()
    }

    /// 失败判据 id。
    pub fn failed_criteria(&self) -> Vec<&'static str> {
        self.criteria.failed_ids()
    }

    /// 取一支的系数。
    pub fn coeffs(&self, branch: Branch) -> &[f64] {
        match branch {
            Branch::Lowpass => &self.lowpass.coefficients,
            Branch::Highpass => &self.highpass.coefficients,
        }
    }

    /// 取一支的设计报告。
    pub fn branch(&self, branch: Branch) -> &BranchDesign {
        match branch {
            Branch::Lowpass => &self.lowpass,
            Branch::Highpass => &self.highpass,
        }
    }

    /// 声明体延迟（两支相同；相对 D=0）。
    pub fn declared_delay_samples(&self) -> f64 {
        self.lowpass.declared_delay_samples
    }

    /// 人类可读摘要（证据日志/报告用；每行一条）。
    pub fn summary_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push(format!(
            "crossover: kind={} method={} fs={} fc={} taps={} fc_tier={} experimental={} window={:?} attenuation={}dB",
            self.kind.as_str(),
            self.method.as_str(),
            self.spec.fs,
            self.spec.fc_hz,
            self.spec.taps,
            self.fc_tier,
            self.spec.experimental,
            self.spec.window,
            self.spec.attenuation_db
        ));
        lines.push(format!(
            "crossover: declared D lp={:.6} hp={:.6} samples (relative {:.6} integer={}) polarity lp:{} hp:{} family_rel={:.1}deg source={}",
            self.lowpass.declared_delay_samples,
            self.highpass.declared_delay_samples,
            self.merge.relative_delay_samples,
            self.merge.relative_delay_is_integer,
            self.merge.polarity.lowpass.as_str(),
            self.merge.polarity.highpass.as_str(),
            self.merge.family_relative_polarity_deg,
            self.merge.polarity_source,
        ));
        lines.push(format!(
            "crossover: sum |S| band [{:.3}, {:.3}] Hz dev={:.6} dB limit={:.3} flat={} opposite_polarity_dev={:.6} dB",
            self.merge.band_hz[0],
            self.merge.band_hz[1],
            self.merge.flat_max_deviation_db,
            self.merge.flat_limit_db,
            self.merge.flat_pass,
            self.merge.opposite_polarity_max_deviation_db,
        ));
        lines.push(format!(
            "crossover: analytic sum flat={} dev={:.6} dB bump@fc={:.4} dB; relative phase @fc analytic={:.6} deg measured={:.6} deg err={:.6} deg",
            self.merge.analytic_sum_flat,
            self.merge.analytic_sum_max_deviation_db,
            self.kind.analytic_sum_bump_at_fc_db(),
            self.merge.analytic_relative_phase_at_fc_deg,
            self.merge.measured_relative_phase_at_fc_deg,
            self.merge.relative_phase_error_deg,
        ));
        for b in [&self.lowpass, &self.highpass] {
            lines.push(format!(
                "crossover: {} mag_err max={:.6} dB mean={:.6} dB @ {:.3} Hz (at_fc {:.6} dB); phase_err max={:.6} deg @ {:.3} Hz (at_fc {:.6} deg); GD@fc={:.6} samples analytic={:.6}",
                b.branch.as_str(),
                b.magnitude.max_abs_err_db,
                b.magnitude.mean_abs_err_db,
                b.magnitude.worst_hz,
                b.magnitude.at_fc_err_db,
                b.phase.max_abs_err_deg,
                b.phase.worst_hz,
                b.phase.at_fc_err_deg,
                b.group_delay.at_fc_samples,
                b.group_delay.analytic_at_fc_samples,
            ));
            lines.push(format!(
                "crossover: {} support eff={} peak={} max|h|={:.6} sum|h|={:.6} ring pre={:?} dB post={:?} dB",
                b.branch.as_str(),
                b.support.effective_taps,
                b.support.peak_index,
                b.max_abs_h,
                b.sum_abs_h,
                b.ringing.pre_db,
                b.ringing.post_db,
            ));
        }
        lines.push(format!(
            "crossover: criteria {} ({}/{}); failed={:?}",
            if self.accepted() { "PASS" } else { "FAIL" },
            self.criteria.criteria.iter().filter(|c| c.pass).count(),
            self.criteria.criteria.len(),
            self.failed_criteria()
        ));
        for w in &self.warnings {
            lines.push(format!("crossover: WARNING {w}"));
        }
        lines
    }
}

// ---------------------------------------------------------------------------
// 测量实现
// ---------------------------------------------------------------------------

fn support_report(coeffs: &[f64], eps: f64) -> SupportReport {
    let max_abs_value = max_abs(coeffs);
    let mut first = coeffs.len();
    let mut last = 0usize;
    let mut peak = 0usize;
    let mut peak_val = 0.0f64;
    let threshold = eps * max_abs_value;
    for (n, &h) in coeffs.iter().enumerate() {
        let a = h.abs();
        if a > threshold {
            if n < first {
                first = n;
            }
            last = n;
        }
        if a > peak_val {
            peak_val = a;
            peak = n;
        }
    }
    if first == coeffs.len() {
        first = 0;
    }
    SupportReport {
        first_effective: first,
        last_effective: last,
        effective_taps: last.saturating_sub(first) + 1,
        max_abs: max_abs_value,
        peak_index: peak,
    }
}

fn ringing_report(coeffs: &[f64], fs: f64, support: &SupportReport) -> RingingReport {
    let taps = coeffs.len();
    let exclusion = (RING_WINDOW_SEC * fs).ceil() as usize;
    let p = support.peak_index;
    let max_abs_value = support.max_abs;
    let mut pre_db = None;
    let mut post_db = None;
    let mut pre_energy_db = None;
    let mut post_energy_db = None;
    if max_abs_value > 0.0 && p < taps {
        let total_energy: f64 = coeffs.iter().map(|h| h * h).sum();
        if p >= exclusion {
            let mut m = 0.0f64;
            let mut e = 0.0f64;
            for &h in &coeffs[..p - exclusion] {
                m = m.max(h.abs());
                e += h * h;
            }
            pre_db = Some(db_from_mag(m / max_abs_value));
            pre_energy_db = Some(10.0 * (e / total_energy).max(1e-300).log10());
        }
        if p + exclusion < taps {
            let mut m = 0.0f64;
            let mut e = 0.0f64;
            for &h in &coeffs[p + exclusion + 1..] {
                m = m.max(h.abs());
                e += h * h;
            }
            post_db = Some(db_from_mag(m / max_abs_value));
            post_energy_db = Some(10.0 * (e / total_energy).max(1e-300).log10());
        }
    }
    RingingReport {
        exclusion_samples: exclusion,
        pre_db,
        post_db,
        pre_energy_db,
        post_energy_db,
    }
}

fn magnitude_report(
    coeffs: &[f64],
    spec: &ValidatedCrossoverSpec,
    branch: Branch,
    band_hz: [f64; 2],
    cross_grid: &[f64],
    full_grid: &[f64],
) -> MagnitudeApproxReport {
    let mut max_abs_err = 0.0f64;
    let mut sum_abs = 0.0f64;
    let mut worst = spec.fc_hz();
    for &f in cross_grid {
        let measured = response_at(coeffs, spec.fs(), f).magnitude_db();
        let analytic = analytic_response(spec.spec.kind, branch, spec.fc_hz(), f).magnitude_db();
        let e = (measured - analytic).abs();
        sum_abs += e;
        if e > max_abs_err {
            max_abs_err = e;
            worst = f;
        }
    }
    let count = cross_grid.len().max(1);
    let at_fc_err = response_at(coeffs, spec.fs(), spec.fc_hz()).magnitude_db()
        - analytic_response(spec.spec.kind, branch, spec.fc_hz(), spec.fc_hz()).magnitude_db();
    let mut full_max = 0.0f64;
    let mut full_worst = spec.fc_hz();
    for &f in full_grid {
        let measured = response_at(coeffs, spec.fs(), f).magnitude_db();
        let analytic = analytic_response(spec.spec.kind, branch, spec.fc_hz(), f).magnitude_db();
        let e = (measured - analytic).abs();
        if e > full_max {
            full_max = e;
            full_worst = f;
        }
    }
    MagnitudeApproxReport {
        band_hz,
        grid_points: cross_grid.len(),
        max_abs_err_db: max_abs_err,
        mean_abs_err_db: sum_abs / count as f64,
        worst_hz: worst,
        at_fc_err_db: at_fc_err,
        full_band_max_abs_err_db: full_max,
        full_band_worst_hz: full_worst,
        analytic_at_fc_db: analytic_response(spec.spec.kind, branch, spec.fc_hz(), spec.fc_hz())
            .magnitude_db(),
        measured_at_fc_db: response_at(coeffs, spec.fs(), spec.fc_hz()).magnitude_db(),
    }
}

fn phase_report(
    coeffs: &[f64],
    spec: &ValidatedCrossoverSpec,
    branch: Branch,
    applied_polarity: Polarity,
    declared_delay: f64,
    band_hz: [f64; 2],
    cross_grid: &[f64],
) -> PhaseApproxReport {
    let sign = applied_polarity.sign();
    let analytic_phase_with_polarity = |f: f64| -> f64 {
        let h = analytic_response(spec.spec.kind, branch, spec.fc_hz(), f);
        (sign * h.im).atan2(sign * h.re).to_degrees()
    };
    let weight_floor_mag = 10f64.powf(PHASE_WEIGHT_FLOOR_DB / 20.0);
    let mut max_abs_err = 0.0f64;
    let mut sum_abs = 0.0f64;
    let mut worst = spec.fc_hz();
    let mut considered = 0usize;
    for &f in cross_grid {
        let analytic = analytic_response(spec.spec.kind, branch, spec.fc_hz(), f);
        if analytic.magnitude() < weight_floor_mag {
            continue;
        }
        let measured = response_at(coeffs, spec.fs(), f).phase_deg();
        let expected = analytic_phase_with_polarity(f) - 360.0 * f * declared_delay / spec.fs();
        let e = wrap_deg(measured - expected).abs();
        sum_abs += e;
        considered += 1;
        if e > max_abs_err {
            max_abs_err = e;
            worst = f;
        }
    }
    let at_fc_err = {
        let measured = response_at(coeffs, spec.fs(), spec.fc_hz()).phase_deg();
        let expected = analytic_phase_with_polarity(spec.fc_hz())
            - 360.0 * spec.fc_hz() * declared_delay / spec.fs();
        wrap_deg(measured - expected)
    };
    let method_note = match spec.spec.method {
        CrossoverMethod::AnalyticPhase => "complex analytic impulse truncation (magnitude+phase targeted)",
        CrossoverMethod::LinearPhaseMagnitude => {
            "linear-phase magnitude-only construction (phase not targeted, reported for transparency)"
        }
    };
    PhaseApproxReport {
        band_hz,
        grid_points: cross_grid.len(),
        considered_points: considered,
        weight_floor_db: PHASE_WEIGHT_FLOOR_DB,
        max_abs_err_deg: max_abs_err,
        mean_abs_err_deg: if considered == 0 {
            f64::NAN
        } else {
            sum_abs / considered as f64
        },
        worst_hz: worst,
        at_fc_err_deg: at_fc_err,
        method_note,
    }
}

fn group_delay_report(
    coeffs: &[f64],
    spec: &ValidatedCrossoverSpec,
    branch: Branch,
    declared_delay: f64,
    band_hz: [f64; 2],
    cross_grid: &[f64],
) -> GroupDelayReport {
    let mut min_samples = f64::INFINITY;
    let mut max_samples = f64::NEG_INFINITY;
    let mut sum = 0.0f64;
    let mut count = 0usize;
    let mut max_err = 0.0f64;
    for &f in cross_grid {
        let gd = group_delay_at(coeffs, spec.fs(), f);
        if !gd.is_finite() {
            continue;
        }
        min_samples = min_samples.min(gd);
        max_samples = max_samples.max(gd);
        sum += gd;
        count += 1;
        let analytic =
            analytic_group_delay_samples(spec.spec.kind, branch, spec.fc_hz(), spec.fs(), f);
        if analytic.is_finite() {
            // 实测 GD 含声明体延迟 D；解析目标不含，比较前减去 D
            max_err = max_err.max(((gd - declared_delay) - analytic).abs());
        }
    }
    GroupDelayReport {
        band_hz,
        grid_points: cross_grid.len(),
        declared_delay_samples: declared_delay,
        declared_delay_ms: declared_delay / spec.fs() * 1000.0,
        min_samples: if count == 0 { f64::NAN } else { min_samples },
        max_samples: if count == 0 { f64::NAN } else { max_samples },
        mean_samples: if count == 0 {
            f64::NAN
        } else {
            sum / count as f64
        },
        at_fc_samples: group_delay_at(coeffs, spec.fs(), spec.fc_hz()),
        physical_at_fc_samples: group_delay_at(coeffs, spec.fs(), spec.fc_hz()) - declared_delay,
        at_1khz_samples: group_delay_at(coeffs, spec.fs(), 1_000.0),
        analytic_at_fc_samples: analytic_group_delay_samples(
            spec.spec.kind,
            branch,
            spec.fc_hz(),
            spec.fs(),
            spec.fc_hz(),
        ),
        max_abs_err_samples: max_err,
    }
}

fn build_branch(
    coeffs: Vec<f64>,
    spec: &ValidatedCrossoverSpec,
    branch: Branch,
    applied_polarity: Polarity,
    band_hz: [f64; 2],
    cross_grid: &[f64],
    full_grid: &[f64],
) -> BranchDesign {
    let declared_delay = declared_delay_samples(spec.spec.method, spec.spec.taps);
    let support = support_report(&coeffs, EFFECTIVE_SUPPORT_EPS);
    let ringing = ringing_report(&coeffs, spec.fs(), &support);
    let magnitude = magnitude_report(&coeffs, spec, branch, band_hz, cross_grid, full_grid);
    let phase = phase_report(
        &coeffs,
        spec,
        branch,
        applied_polarity,
        declared_delay,
        band_hz,
        cross_grid,
    );
    let group_delay =
        group_delay_report(&coeffs, spec, branch, declared_delay, band_hz, cross_grid);
    let dc_gain: f64 = coeffs.iter().sum();
    let c = &coeffs;
    BranchDesign {
        branch,
        taps: coeffs.len(),
        applied_polarity,
        declared_delay_samples: declared_delay,
        declared_delay_ms: declared_delay / spec.fs() * 1000.0,
        support,
        magnitude,
        phase,
        group_delay,
        ringing,
        dc_gain,
        dc_gain_db: db_from_mag(dc_gain.abs()),
        nyquist_mag_db: response_at(c, spec.fs(), spec.fs() * 0.5).magnitude_db(),
        max_abs_h: support.max_abs,
        sum_abs_h: c.iter().map(|h| h.abs()).sum(),
        coefficients: coeffs,
    }
}

/// 合并支路和 (H_LP + H_HP) 的 |S| 统计：返回 (max_db, min_db, 相对 max 偏差)。
fn sum_magnitude_stats(coefficients: &[f64], grid: &[f64], fs: f64) -> (f64, f64, f64) {
    let mut max_db = f64::NEG_INFINITY;
    let mut min_db = f64::INFINITY;
    let mut values = Vec::with_capacity(grid.len());
    for &f in grid {
        let v = response_at(coefficients, fs, f).magnitude_db();
        values.push(v);
        max_db = max_db.max(v);
        min_db = min_db.min(v);
    }
    let deviation = values.iter().fold(0.0f64, |m, v| m.max((v - max_db).abs()));
    (max_db, min_db, deviation)
}

fn analytic_sum_stats(
    kind: CrossoverType,
    fc_hz: f64,
    polarity: BranchPolarity,
    grid: &[f64],
) -> (f64, f64, f64) {
    let mut max_db = f64::NEG_INFINITY;
    let mut min_db = f64::INFINITY;
    let mut values = Vec::with_capacity(grid.len());
    for &f in grid {
        let lp = analytic_response(kind, Branch::Lowpass, fc_hz, f);
        let hp = analytic_response(kind, Branch::Highpass, fc_hz, f);
        let sign = polarity.highpass.sign();
        let re = lp.re + sign * hp.re;
        let im = lp.im + sign * hp.im;
        let v = db_from_mag(re.hypot(im));
        values.push(v);
        max_db = max_db.max(v);
        min_db = min_db.min(v);
    }
    let deviation = values.iter().fold(0.0f64, |m, v| m.max((v - max_db).abs()));
    (max_db, min_db, deviation)
}

// ---------------------------------------------------------------------------
// 主设计入口
// ---------------------------------------------------------------------------

/// 设计 LR/BW 输出 FIR 分频（有限幅相近似 + 合并/极性/相对 D 检查）。
///
/// 返回 Ok(design) 表示参数合法且构造完成；是否满足全部判据看
/// CrossoverDesign::accepted / CrossoverDesign::failed_criteria（失败 = 该组合 INFEASIBLE，
/// 不是静默放行）。要求“必须通过”的调用方用 design_strict。
pub fn design(spec: &CrossoverSpec) -> Result<CrossoverDesign, CrossoverError> {
    let validated = spec.validate()?;
    let kind = spec.kind;
    let mut warnings = Vec::new();

    // 1) 同步率提醒：FIR fs 必须 == 运行 fs（§7.2）
    if (spec.fs - 48_000.0).abs() > 1e-9 && (spec.fs - 96_000.0).abs() > 1e-9 {
        warnings.push(format!(
            "fs={} 不是 48 kHz/96 kHz：允许工具设计，但运行 fs 必须与系数 fs 一致（§7.2 禁止重采样复用）",
            spec.fs
        ));
    }
    if spec.taps > REGULAR_PROFILE_TAPS_CAP {
        warnings.push(format!(
            "taps={} > 常规 profile 每路由 4096（§7.2）：需要 extended 准入（T56），本报告不代做 RAM/deadline 判定",
            spec.taps
        ));
    }
    warnings.push(format!(
        "T39 两种方法都采用矩形截断（AnalyticPhase 另加 Nyquist fade；目标函数平滑时矩形主瓣比 Kaiser β=8.96 更窄，低 fc 更准）；window={:?} attenuation_db={} 只登记/参与参数校验，不参与本实现系数构造（需要窗函数设计另行裁决）",
        spec.window, spec.attenuation_db
    ));

    // 2) fc 分档（Lead 裁决 2026-11-17）：≤0.2fs 生产；0.2–0.225fs 仅显式 experimental
    let fc_production_max = FC_PRODUCTION_MAX_FRACTION * spec.fs;
    let fc_tier = if spec.fc_hz <= fc_production_max {
        "production"
    } else {
        "experimental"
    };
    if fc_tier == "experimental" {
        warnings.push(format!(
            "EXPERIMENTAL（非生产）：fc={} > 0.2·fs={fc_production_max}（Lead 裁决 2026-11-17）；\
             仅显式 experimental flag 可设计，判据 xover_production_cap 失败，结果不得作为生产验收",
            spec.fc_hz
        ));
    }

    // 3) 解析目标与极性（唯一来源：LR/BW 解析相位关系）
    let polarity = kind.analytic_polarity();

    // 4) 合成两支 FIR 并应用极性
    let lowpass_coeffs = synthesize_branch(&validated, Branch::Lowpass);
    let mut highpass_coeffs = synthesize_branch(&validated, Branch::Highpass);
    if polarity.highpass == Polarity::Inverted {
        for h in highpass_coeffs.iter_mut() {
            *h = -*h;
        }
    }
    if lowpass_coeffs.len() != spec.taps || highpass_coeffs.len() != spec.taps {
        return err(
            E_FIR_TAPS,
            format!(
                "系数长度 lp={} hp={} != taps={}",
                lowpass_coeffs.len(),
                highpass_coeffs.len(),
                spec.taps
            ),
        );
    }
    if !lowpass_coeffs.iter().all(|h| h.is_finite())
        || !highpass_coeffs.iter().all(|h| h.is_finite())
    {
        return err(E_FIR_NUMERIC, "合成系数含 NaN/Inf");
    }
    if max_abs(&lowpass_coeffs) <= 0.0 || max_abs(&highpass_coeffs) <= 0.0 {
        return err(E_FIR_NUMERIC, "合成系数全零");
    }

    // 5) 测量网格
    let cross = crossover_band(spec.fc_hz, spec.fs);
    let cross_grid = linear_grid(cross[0], cross[1], GRID_POINTS);
    let full = full_report_band(spec.fs);
    let full_grid = log_grid(full[0], full[1], FULL_GRID_POINTS);

    // 6) 分支报告（系数已应用极性）
    let lowpass = build_branch(
        lowpass_coeffs,
        &validated,
        Branch::Lowpass,
        polarity.lowpass,
        cross,
        &cross_grid,
        &full_grid,
    );
    let highpass = build_branch(
        highpass_coeffs,
        &validated,
        Branch::Highpass,
        polarity.highpass,
        cross,
        &cross_grid,
        &full_grid,
    );

    // 7) 合并检查：实测和（使用极性）与相反极性对照、解析复数和对照
    let mut sum_applied = vec![0.0f64; spec.taps];
    let mut sum_opposite = vec![0.0f64; spec.taps];
    for n in 0..spec.taps {
        sum_applied[n] = lowpass.coefficients[n] + highpass.coefficients[n];
        sum_opposite[n] = lowpass.coefficients[n] - highpass.coefficients[n];
    }
    let (sum_max_db, sum_min_db, flat_dev) =
        sum_magnitude_stats(&sum_applied, &cross_grid, spec.fs);
    let (_, _, opposite_dev) = sum_magnitude_stats(&sum_opposite, &cross_grid, spec.fs);
    let (_, _, analytic_dev) = analytic_sum_stats(kind, spec.fc_hz, polarity, &cross_grid);
    let measured_best = if opposite_dev < flat_dev {
        polarity.flipped_highpass()
    } else {
        polarity
    };
    let lp_phase = response_at(&lowpass.coefficients, spec.fs, spec.fc_hz).phase_deg();
    let hp_phase = response_at(&highpass.coefficients, spec.fs, spec.fc_hz).phase_deg();
    let measured_rel_phase = wrap_deg(hp_phase - lp_phase);
    let analytic_rel_phase = analytic_relative_phase_at_fc_deg(kind);
    let merge = MergeReport {
        band_hz: cross,
        grid_points: cross_grid.len(),
        polarity,
        polarity_source: POLARITY_SOURCE_ANALYTIC,
        family_relative_polarity_deg: kind.family_relative_polarity_deg(),
        family_phase_rule: kind.family_phase_rule(),
        measured_best_polarity: measured_best,
        flat_max_deviation_db: flat_dev,
        flat_limit_db: SUM_DEVIATION_LIMIT_DB,
        flat_pass: flat_dev <= SUM_DEVIATION_LIMIT_DB,
        opposite_polarity_max_deviation_db: opposite_dev,
        sum_max_db,
        sum_min_db,
        analytic_sum_max_deviation_db: analytic_dev,
        analytic_sum_flat: kind.analytic_sum_is_flat(),
        analytic_relative_phase_at_fc_deg: analytic_rel_phase,
        measured_relative_phase_at_fc_deg: measured_rel_phase,
        relative_phase_error_deg: wrap_deg(measured_rel_phase - analytic_rel_phase),
        relative_delay_samples: lowpass.declared_delay_samples - highpass.declared_delay_samples,
        relative_delay_is_integer: {
            let rel = lowpass.declared_delay_samples - highpass.declared_delay_samples;
            rel.is_finite() && (rel - rel.round()).abs() <= 1e-12
        },
        sum_group_delay_at_fc_samples: group_delay_at(&sum_applied, spec.fs, spec.fc_hz),
        theoretical_sum: if kind.analytic_sum_is_flat() {
            "analytic LP+HP complex sum is allpass (flat magnitude)"
        } else {
            "analytic even-order BW LP+HP complex sum is NOT flat (+3.0103 dB at fc); fir-toolbox §4.1 flat-sum claim holds only for odd BW order"
        },
    };

    // 8) 判据
    let criteria = build_criteria(spec, kind, &lowpass, &highpass, &merge);

    // 9) 裁决/失败/诊断 warnings（不静默隐藏）
    if !merge.analytic_sum_flat {
        warnings.push(format!(
            "RULED（2026-11-17）：{} 解析 LP+HP 复数和本身不平坦（@fc +{:.4} dB，解析可证）；spec §4.1 “BW 和仍应为全通”只对奇数阶成立。本实现如实报告实测/解析偏差，不放宽 0.5 dB 判据（sum_flat 失败属数学性质，非数值误差）",
            kind.as_str(),
            BW_EVEN_SUM_BUMP_DB
        ));
    }
    if spec.method == CrossoverMethod::LinearPhaseMagnitude {
        warnings.push(
            "LinearPhaseMagnitude 为非规范对照方法（Lead 裁决 2026-11-17：§4.1 采用 AnalyticPhase 为规范构造）；\
             线性相位只约束支路 FIR 自身设计，不作为两支求和验收依据，判据 xover_normative_construction 失败"
                .to_string(),
        );
    }
    if spec.method == CrossoverMethod::LinearPhaseMagnitude && kind.is_butterworth() {
        warnings.push(format!(
            "LinearPhaseMagnitude 无法复现 {} 解析和的 90°/270° 相对相位（同 L/同 D 线性相位对只能 0°/180°）：实测 |S| 偏差与相位目标均不通过；该法为非规范对照，BW/§4.1 验收请用 AnalyticPhase（默认，RULED 2026-11-17）",
            kind.as_str()
        ));
    }
    if spec.method == CrossoverMethod::LinearPhaseMagnitude
        && kind.is_linkwitz_riley()
        && kind.order() % 4 == 2
    {
        warnings.push(format!(
            "LinearPhaseMagnitude + {}：解析要求 HP 反相（已按解析规则应用），但线性相位同 L/同 D 构造下该极性造成合并凹陷；{} 请用 AnalyticPhase（默认）",
            kind.as_str(),
            kind.as_str()
        ));
    }
    if measured_best != polarity {
        let best_dev = if measured_best == polarity {
            flat_dev
        } else {
            opposite_dev
        };
        warnings.push(format!(
            "实测合并偏差更小的极性为 lp:{} hp:{}（{:.6} dB）而非解析极性 lp:{} hp:{}（{:.6} dB）；本实现按解析关系报告/应用极性（禁止靠听感判极性），此差异仅诊断",
            measured_best.lowpass.as_str(),
            measured_best.highpass.as_str(),
            best_dev,
            polarity.lowpass.as_str(),
            polarity.highpass.as_str(),
            flat_dev,
        ));
    }
    if !criteria.all_pass() {
        warnings.push(format!(
            "判据失败（INFEASIBLE）：{:?}；原始测量值见报告，未静默放行",
            criteria.failed_ids()
        ));
    }
    if spec.method == CrossoverMethod::AnalyticPhase
        && kind.is_butterworth()
        && !kind.analytic_sum_is_flat()
    {
        warnings.push(
            "解析偶数阶 BW 的 LP+HP 复数和不是全通：合并判据失败是数学性质而非数值误差".to_string(),
        );
    }

    let notes = vec![
        "无默认角色 HP/LP：分频必须显式配置；未赋值输出为 bypass/full-range（高度默认全频，§4.1）",
        "极性来自 LR/BW 解析相位关系（order%4==2 反相：LR2/LR6/BW2/BW6；其余同相：LR4/LR8/BW1/3/5/7），禁止靠听感判极性（§4.1）",
        "两支同 L/同方法/同一 D，相对 D=0 整数 samples（§4.1）；不许人为相位错位",
        "合并判据：|S| 在 [fc/2, 2fc] 相对 max 偏差 <= 0.5 dB（§4.1）",
        "P_post 语义：分频相位由系数本身决定（reference/SPEC §2.5）；本模块 FIR 已含声明体延迟 D=(taps−1)/2（两支相同，相对 D=0），不再额外加延迟",
        "AnalyticPhase 用公共体延迟 + Nyquist 护栏 fade（0.45fs–0.5fs 升余弦到 0）后矩形截断：fade 消除 fs/2 复相位跳变引起的 1/n 长尾；两种方法均不用 window/attenuation_db 构造系数（只登记/校验）",
        "NOT_RUN：无板端/ALSA/CueMix/RT/soak；无 WAV/manifest 落盘（T55）；f32 运行期量化误差复算归 T55/T22",
    ];

    Ok(CrossoverDesign {
        spec: spec.clone(),
        method: spec.method,
        fc_tier,
        kind,
        lowpass,
        highpass,
        merge,
        criteria,
        warnings,
        notes,
    })
}

fn criterion(
    id: &'static str,
    statement: &'static str,
    measured: Option<f64>,
    limit: Option<f64>,
    unit: &'static str,
    pass: bool,
) -> Criterion {
    Criterion {
        id,
        statement,
        measured,
        limit,
        unit,
        pass,
    }
}

fn build_criteria(
    spec: &CrossoverSpec,
    kind: CrossoverType,
    lowpass: &BranchDesign,
    highpass: &BranchDesign,
    merge: &MergeReport,
) -> NormativeReport {
    let mut criteria = Vec::new();

    criteria.push(criterion(
        "xover_type",
        "LR/BW 类型在 LR2/LR4/LR6/LR8、BW1..BW8 内；未知 type 显式拒收（§4.1）",
        None,
        None,
        "",
        true,
    ));
    criteria.push(criterion(
        "xover_band",
        "fc ∈ [20, 0.45·fs]（§4.1 边界）",
        Some(spec.fc_hz),
        Some(MAX_FC_FS_FRACTION * spec.fs),
        "Hz",
        spec.fc_hz >= MIN_FC_HZ && spec.fc_hz <= MAX_FC_FS_FRACTION * spec.fs,
    ));
    let fc_production_max = FC_PRODUCTION_MAX_FRACTION * spec.fs;
    criteria.push(criterion(
        "xover_production_cap",
        "生产硬上限 fc <= 0.2·fs（=0.2fs 允许；0.2–0.225fs 仅实验模式且不得生产接受；Lead 裁决 2026-11-17）",
        Some(spec.fc_hz / spec.fs),
        Some(FC_PRODUCTION_MAX_FRACTION),
        "fs",
        spec.fc_hz <= fc_production_max,
    ));
    criteria.push(criterion(
        "xover_same_length_delay",
        "两支同 L、同 D；相对 D 为整数 samples（本实现 0，§4.1）",
        Some(merge.relative_delay_samples),
        Some(0.0),
        "samples",
        lowpass.taps == highpass.taps
            && (lowpass.declared_delay_samples - highpass.declared_delay_samples).abs() <= 1e-12
            && merge.relative_delay_is_integer,
    ));
    criteria.push(criterion(
        "xover_polarity_declared",
        "极性由解析 LR/BW 关系决定并报告（禁止听感判极性，§4.1）",
        None,
        None,
        "",
        merge.polarity_source == POLARITY_SOURCE_ANALYTIC
            && merge.polarity.lowpass == Polarity::Normal,
    ));
    criteria.push(criterion(
        "xover_normative_construction",
        "§4.1 规范构造 = AnalyticPhase（Lead 裁决 2026-11-17）；LinearPhaseMagnitude 仅对照，不参与验收",
        None,
        None,
        "",
        spec.method == CrossoverMethod::AnalyticPhase,
    ));
    let lr4_same_polarity_ok =
        kind != CrossoverType::Lr4 || merge.polarity.highpass == Polarity::Normal;
    criteria.push(criterion(
        "xover_lr4_polarity_same",
        "LR4 两支同极性（§4.1）",
        None,
        None,
        "",
        lr4_same_polarity_ok,
    ));
    // 相位验收适用性（终审 F-24）：LR 全阶与 BW 奇数阶适用；BW 偶数阶不定义相位验收，
    // 相位/极性两项只报告并标 NOT_APPLICABLE（不计 pass），拒收只走 xover_sum_flat → E_FIR_XOVER_SUM。
    let phase_criteria_applicable = kind.is_linkwitz_riley() || kind.order() % 2 == 1;
    let (family_measured, family_limit, family_unit, family_ok) = if kind.is_linkwitz_riley() {
        (
            merge.measured_relative_phase_at_fc_deg.abs(),
            Some(LR_PHASE_FC_LIMIT_DEG),
            "deg",
            merge.measured_relative_phase_at_fc_deg.abs() <= LR_PHASE_FC_LIMIT_DEG,
        )
    } else if kind.order() % 2 == 1 {
        (
            merge.family_relative_polarity_deg,
            Some(180.0),
            "deg-polarity",
            merge.polarity.highpass == Polarity::Inverted,
        )
    } else {
        // BW 偶数阶：NOT_APPLICABLE —— 只报告，不计 pass（不得以相位门放行或拒收）。
        (
            merge.family_relative_polarity_deg,
            None,
            "deg-polarity",
            false,
        )
    };
    criteria.push(criterion(
        "xover_family_polarity",
        "家族相对极性（RULED 2026-11-17）：LR 偶数阶 fc 同相（相对 0°，求和幅频平坦）；BW 奇数阶允许/要求一支反相（极性 180°），不强制线性相位或 0°；BW 偶数阶 NOT_APPLICABLE：不定义同相/180°验收，只报告、不计 pass（拒收只走合并平坦判据 → E_FIR_XOVER_SUM，终审 F-24）",
        Some(family_measured),
        family_limit,
        family_unit,
        family_ok,
    ));
    let phase_limit = if kind.is_linkwitz_riley() {
        Some(LR_PHASE_FC_LIMIT_DEG)
    } else if kind.order() % 2 == 1 {
        Some(BW_PHASE_FC_LIMIT_DEG)
    } else {
        None
    };
    let phase_ok = phase_criteria_applicable
        && match phase_limit {
            Some(limit) => merge.relative_phase_error_deg.abs() <= limit,
            None => true,
        };
    criteria.push(criterion(
        "xover_phase_at_fc",
        "交叉点相对相位 vs 解析目标：LR 偶数阶目标 0°（同相和）；BW 奇数阶按解析目标（±90°，一支反相 180° 极性，不强制 0°）；BW 偶数阶 NOT_APPLICABLE：只报告、不计 pass（拒收只走合并平坦判据 → E_FIR_XOVER_SUM，终审 F-24）",
        Some(merge.relative_phase_error_deg.abs()),
        phase_limit,
        "deg",
        phase_ok,
    ));
    criteria.push(criterion(
        "xover_sum_flat",
        "|S| 在 [fc/2, 2fc] 相对 max 偏差 <= 0.5 dB（§4.1）",
        Some(merge.flat_max_deviation_db),
        Some(SUM_DEVIATION_LIMIT_DB),
        "dB",
        merge.flat_pass,
    ));
    let mag_err = lowpass
        .magnitude
        .max_abs_err_db
        .max(highpass.magnitude.max_abs_err_db);
    criteria.push(criterion(
        "xover_mag_approx",
        "两支 FIR 幅度近似误差（crossover 带）；报告门 0.5 dB（spec 只要求报告 FIR 近似误差）",
        Some(mag_err),
        Some(MAG_APPROX_LIMIT_DB),
        "dB",
        mag_err <= MAG_APPROX_LIMIT_DB,
    ));
    let phase_err = lowpass
        .phase
        .max_abs_err_deg
        .max(highpass.phase.max_abs_err_deg);
    let phase_pass = match spec.method {
        CrossoverMethod::AnalyticPhase => phase_err <= PHASE_APPROX_LIMIT_DEG,
        CrossoverMethod::LinearPhaseMagnitude => true,
    };
    criteria.push(criterion(
        "xover_phase_approx",
        match spec.method {
            CrossoverMethod::AnalyticPhase => {
                "两支 FIR 相位近似误差（crossover 带）；报告门 5°（自定义，spec 只要求报告）"
            }
            CrossoverMethod::LinearPhaseMagnitude => {
                "线性相位幅度构造不把解析相位当设计目标：report-only（不判 PASS/FAIL）"
            }
        },
        Some(phase_err),
        match spec.method {
            CrossoverMethod::AnalyticPhase => Some(PHASE_APPROX_LIMIT_DEG),
            CrossoverMethod::LinearPhaseMagnitude => None,
        },
        "deg",
        phase_pass,
    ));
    let finite_ok = lowpass.coefficients.iter().all(|h| h.is_finite())
        && highpass.coefficients.iter().all(|h| h.is_finite())
        && lowpass.max_abs_h > 0.0
        && highpass.max_abs_h > 0.0;
    criteria.push(criterion(
        "xover_finite",
        "系数有限、max|h|>0、无 NaN/Inf（§2.2 规则 3）",
        None,
        None,
        "",
        finite_ok && spec.taps == lowpass.taps && spec.taps == highpass.taps,
    ));

    NormativeReport { criteria }
}

/// 严格设计：任何判据失败 → Err（E_FIR_XOVER_SUM / E_FIR_XOVER_PHASE），消息含 INFEASIBLE 与实测值。
///
/// BW 偶数阶的相位项为 NOT_APPLICABLE（只报告、不计 pass），拒收只映射 E_FIR_XOVER_SUM
/// （终审 errata C，F-24；LR 与 BW 奇数阶行为不变）。
pub fn design_strict(spec: &CrossoverSpec) -> Result<CrossoverDesign, CrossoverError> {
    let design = design(spec)?;
    if design.accepted() {
        return Ok(design);
    }
    let failed = design.failed_criteria();
    let phase_rejection_applicable =
        design.kind.is_linkwitz_riley() || design.kind.order() % 2 == 1;
    let code = if failed.iter().any(|id| *id == "xover_production_cap") {
        E_FIR_XOVER_FC_LIMIT
    } else if phase_rejection_applicable && failed.iter().any(|id| *id == "xover_phase_at_fc") {
        E_FIR_XOVER_PHASE
    } else {
        E_FIR_XOVER_SUM
    };
    Err(CrossoverError::new(
        code,
        format!(
            "INFEASIBLE: {} method={} fs={} fc={} taps={}；failed={:?}；merge_dev={:.6} dB (limit {:.3})；analytic_sum_flat={} analytic_dev={:.6} dB；phase_err={:.6} deg",
            spec.kind.as_str(),
            spec.method.as_str(),
            spec.fs,
            spec.fc_hz,
            spec.taps,
            failed,
            design.merge.flat_max_deviation_db,
            design.merge.flat_limit_db,
            design.merge.analytic_sum_flat,
            design.merge.analytic_sum_max_deviation_db,
            design.merge.relative_phase_error_deg,
        ),
    ))
}

// ---------------------------------------------------------------------------
// 输出赋值（显式；无默认角色 HP）
// ---------------------------------------------------------------------------

/// 单个输出的显式分频赋值（必须点名 output_index 与 branch）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputAssignment {
    /// 输出通道下标（0-based，设备/矩阵行号）。
    pub output_index: usize,
    /// 显式支路（Lowpass/Highpass；bypass 用“不赋值”表达）。
    pub branch: Branch,
}

impl OutputAssignment {
    /// 显式低通赋值。
    pub fn lowpass(output_index: usize) -> Self {
        Self {
            output_index,
            branch: Branch::Lowpass,
        }
    }

    /// 显式高通赋值。
    pub fn highpass(output_index: usize) -> Self {
        Self {
            output_index,
            branch: Branch::Highpass,
        }
    }
}

/// 每输出一行的编译结果：None = 直通（full-range bypass），不是默认 HP。
#[derive(Clone, Debug, PartialEq)]
pub struct OutputPlan {
    /// 输出通道数。
    pub output_count: usize,
    /// 每输出行：None=bypass；Some(coefficients)=对应支路 FIR。
    pub per_output: Vec<Option<Vec<f64>>>,
}

impl OutputPlan {
    /// 取某输出的 FIR（None = 直通）。
    pub fn filter(&self, output_index: usize) -> Option<&[f64]> {
        self.per_output.get(output_index).and_then(|v| v.as_deref())
    }

    /// 是否直通。
    pub fn is_bypass(&self, output_index: usize) -> bool {
        self.filter(output_index).is_none()
    }

    /// 被显式赋值的输出下标。
    pub fn assigned_outputs(&self) -> Vec<usize> {
        self.per_output
            .iter()
            .enumerate()
            .filter(|(_, v)| v.is_some())
            .map(|(i, _)| i)
            .collect()
    }

    /// 直通输出下标。
    pub fn bypass_outputs(&self) -> Vec<usize> {
        self.per_output
            .iter()
            .enumerate()
            .filter(|(_, v)| v.is_none())
            .map(|(i, _)| i)
            .collect()
    }
}

/// 把显式赋值编译成每输出行 FIR；**未赋值的输出一律直通（bypass），绝不默认 HP**。
///
/// 规则（§4.1“无默认角色 HP”）：分频必须显式来自 profile/校准，不得按 role 名注入；高度默认全频。
pub fn plan_outputs(
    design: &CrossoverDesign,
    output_count: usize,
    assignments: &[OutputAssignment],
) -> Result<OutputPlan, CrossoverError> {
    if output_count == 0 {
        return err(E_FIR_XOVER_LAYOUT, "output_count 必须 > 0");
    }
    let mut per_output: Vec<Option<Vec<f64>>> = vec![None; output_count];
    let mut seen = vec![false; output_count];
    for a in assignments {
        if a.output_index >= output_count {
            return err(
                E_FIR_XOVER_LAYOUT,
                format!(
                    "output_index={} 越界（output_count={output_count}）",
                    a.output_index
                ),
            );
        }
        if seen[a.output_index] {
            return err(
                E_FIR_XOVER_LAYOUT,
                format!(
                    "output_index={} 重复赋值（每条输出最多一个 branch）",
                    a.output_index
                ),
            );
        }
        seen[a.output_index] = true;
        per_output[a.output_index] = Some(design.coeffs(a.branch).to_vec());
    }
    Ok(OutputPlan {
        output_count,
        per_output,
    })
}

// ---------------------------------------------------------------------------
// 私有测试（include! 内联；integrator 无需在 public_registration.rs 注册）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod crossover_tests {
    use super::*;

    include!("../tests/T39/crossover_cases.rs");
}
