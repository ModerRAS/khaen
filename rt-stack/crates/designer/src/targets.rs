//! T11：目标曲线导入（`tspi-target-draft/v1`）。
//!
//! 本模块是 rt-stack 设计器 leaf `targets`（WBS T11，deps T10/T04）。
//! 目标曲线是**期望响应**（设计输入），不是实测 CSV；CSV 是 `freq_hz,mag_db[,phase_deg]`，
//! sidecar/单 JSON 提供 schema/单位/采样率/相位/归一化/插值/端点/哈希元数据。
//!
//! 权威契约（只读）：
//! - `rt-stack/spec/fir-toolbox.md` §6（tspi-target-draft/v1：§6.2 文件形式、§6.3 字段、
//!   §6.4 频点约束、§6.5 normalization/reference_gain、§6.6 相位语义、§6.7 插值/端点、
//!   §6.8 内置目标、§6.9 28 个 E_TGT_* 拒收码、§6.9.1 唯一码优先级、§6.10 消费映射）——
//!   现行 v1.2-draft；fs/2 边界按 Lead 裁决（`evidence/T04/RULING-fs2-2026-11-17.md`）：
//!   magnitude-only 允许 `freq <= fs/2`；带 phase 必须 `< fs/2`（`= fs/2` → E_TGT_PHASE_REAL）。
//! - `scratch/lead/planner/DSP-HOST-DECISION.md` §2.4：tilt10 = 20 Hz 0 dB → 20 kHz −10 dB（log-f），
//!   reference gain 独立。
//! - `calibration-spec/CONTRACT.md`：目标曲线不是校准应用字段（只进拟合/报告）；本模块不写 CONTRACT 字段。
//!
//! 旧 `calibrate/target-curves.yml` 的 `tilt`（−3 dB @ 16 kHz 且 16–20 kHz 平 −3 dB）已由用户口径
//! **superseded**：不作为内置目标、不是默认、不参与任何设计默认值（见 `SUPERSEDED_LEGACY_TILT_ID` 与
//! `builtin_target` 的断言）。本模块内置目标只有 `BuiltinTarget::Flat`（默认）与 `BuiltinTarget::Tilt10`。
//!
//! # 范围
//! - 形式 A：CSV + sidecar（`target.csv` + `target.csv.json`，sidecar 也可显式路径）；
//! - 形式 B：单 JSON（元数据 + `points`）；
//! - 内置：`flat` / `tilt10`（20–20 kHz、480 点几何网格，f_k = 20·1000^(k/479)）；
//! - 全部 28 个 E_TGT_* 拒收码与 §6.9.1 唯一码优先级；
//! - 归一化（none/ref_1k/peak）+ reference gain + boost 检查（warning/拒收/硬上限）；
//! - 相位：none/absolute/delay_removed、unwrapped/auto、target_delay_samples 整数与 200 ms 界；
//! - log10(f) 线性插值、hold/reject 端点、20 Hz/20 kHz 端点取值来源报告；
//! - 非模态 solver 读取接口 `NonModalTargetSource`：任意频率采样（幅度/相位）+ 原生网格 + 采样来源，
//!   供 T37/T55/T64（PEQ fit）/报告器消费；本模块不做模态 solver、不做 FIR/PEQ 设计。
//!
//! # 非目标
//! - 不读测量 CSV、不把测量去延迟公式套到 target（§6.1/§6.11）；不写 CONTRACT 校准字段；
//! - 不做七家族设计（T10/T33–T41）、不做融合（T12）、不做报告 manifest（T55）；
//! - 不实现 sha256 之外的密码学；不新增第三方依赖（纯 std，离线构建）；
//! - 不碰板端/ALSA/CueMix/服务/git。
//!
//! # 可判性口径（实现解释，见 evidence/T11/MANIFEST.md §7）
//! - 多错并存的单码优先级按 §6.9.1 映射矩阵（逐 stage 由低到高）：schema > missing > unknown >
//!   unit > rate > csv_format > sidecar_mix > point_field > points > freq > nan > mag > phase…
//!   （§6.3 与矩阵在 `phase_unit`/`phase_ref` 缺失上冲突时以矩阵的 `E_TGT_PHASE_UNIT`/`E_TGT_PHASE_REF` 为准，
//!   无专用码的字段（`unwrap_policy` 缺失/非法、`note` 超长、点内缺 `freq_hz`、JSON 处
//!   非数值类型）按矩阵语义就近映射并逐条登记，不静默放行。
//! - `E_TGT_PHASE_REAL` 责任层拆分（终审 errata C，F-23；spec §6.6/§6.9 #19/§6.9.1）：
//!   (a) 带 phase 的 freq = fs/2（Nyquist 相位退化）由**本静态导入层**判定（stage 19）。实系数 FIR
//!       在 fs/2 的响应必为实数 H(fs/2)=Σh[n]·(−1)^n，故 fs/2 处的任意相位目标都不可由实系数 FIR
//!       表示——这是唯一可在静态层判定的实系数相位反例；Lead 裁决进一步把该频点任何 phase 目标整体拒收。
//!   (b) 「其余频点相位能否由实系数因果 FIR 表示」**静态层不可判**：f<fs/2 的任意有限相位点可由
//!       足够长度实系数 FIR 表示，可表示性依赖 taps/D/预算，属设计期 → 由 T37 corrected
//!       （E_FIR_NONCAUSAL / 相位误差判据 INCOMPLETE）与 T55 报告器（corrected_phase_target/
//!       corrected_complex_error 判据，联合 E_TGT_BUDGET）承担；测试见 tests/T37/corrected_cases.rs
//!       与 tests/T55/report_cases.rs。本模块不猜测、不静默放行。
//! - JSON 形式 `source_hash` 的规范序列化按 §6.3 第 1–4 条实现（ECMAScript Number::toString
//!   风格最短往返十进制、键序固定、无空格）；`source_file_sha256` 仍按原始字节校验。
//!
//! # RT / 板端
//! 本模块是设计期一次性导入路径（读文件、分配 Vec、字符串格式化），**不属于 RT-MODULE**，不被
//! `rt-check forbid/drop-audit` 扫描；不得被 RT 线程调用。
//!
//! 状态：**实现 + 真实 `cargo test -p khaen-designer targets`**；未上板、未做真实测量/校准、
//! 未与 T37/T55/T64 联调（接口按 §6 提供）。见 evidence/T11/。

use std::fmt;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// 常量（§6）
// ---------------------------------------------------------------------------

/// 目标文件 schema（§6.3）。
pub const TARGET_SCHEMA: &str = "tspi-target-draft/v1";
/// CSV 表头：仅有幅度（§6.2）。
pub const CSV_HEADER_MAG: &str = "freq_hz,mag_db";
/// CSV 表头：幅度 + 相位（§6.2）。
pub const CSV_HEADER_PHASE: &str = "freq_hz,mag_db,phase_deg";
/// 默认运行采样率（profile fs；48 kHz）。
pub const DEFAULT_RUNTIME_RATE_HZ: f64 = 48_000.0;
/// 频点数量下界（§6.4 判据 4）。
pub const MIN_POINTS: usize = 2;
/// 频点数量上界（§6.4 判据 4）。
pub const MAX_POINTS: usize = 20_000;
/// ref_1k 归一化参考频率（§6.5）。
pub const REFERENCE_FREQ_HZ: f64 = 1_000.0;
/// 覆盖范围下界（§6.7 / §6.8）。
pub const COVERAGE_LO_HZ: f64 = 20.0;
/// 覆盖范围上界（§6.7 / §6.8）。
pub const COVERAGE_HI_HZ: f64 = 20_000.0;
/// reference_gain_db 下界（§6.5）。
pub const REFERENCE_GAIN_MIN_DB: f64 = -24.0;
/// reference_gain_db 上界（§6.5）。
pub const REFERENCE_GAIN_MAX_DB: f64 = 24.0;
/// boost 警告阈值（§6.5）。
pub const BOOST_WARN_DB: f64 = 6.0;
/// boost 拒收阈值（未显式 allow_boost；§6.5）。
pub const BOOST_REJECT_DB: f64 = 12.0;
/// boost 硬上限（§6.5）。
pub const BOOST_HARD_LIMIT_DB: f64 = 18.0;
/// target_delay_samples 上界（毫秒，§6.3）。
pub const TARGET_DELAY_MAX_MS: f64 = 200.0;
/// 内置目标网格点数（§6.8：480 点几何网格）。
pub const BUILTIN_GRID_POINTS: usize = 480;
/// 低频段（≤200 Hz）建议最大 octave 步长 1/48 oct（§6.4 判据 5，warning）。
pub const GRID_MAX_OCTAVE_STEP_LOW: f64 = 1.0 / 48.0;
/// 1–20 kHz 建议最大 octave 步长 1/12 oct（§6.4 判据 5，warning）。
pub const GRID_MAX_OCTAVE_STEP_MID: f64 = 1.0 / 12.0;
/// 低频段/中高频段的分界（§6.4 判据 5 的 200 Hz）。
pub const GRID_LOW_BAND_HZ: f64 = 200.0;
/// 相位 `phase_ref=delay_removed` 时 D_used 与 D_target 的允许偏差（sample，§6.6）。
pub const TARGET_DELAY_CONSISTENCY_SAMPLES: f64 = 0.01;
/// 相位相邻点比较的浮点容差（deg）。
const PHASE_JUMP_EPS_DEG: f64 = 1e-9;
/// 内置 tilt10 的 −10 dB 参考（20 kHz）。
pub const TILT10_HI_HZ: f64 = 20_000.0;
/// 内置 tilt10 的 0 dB 参考（20 Hz）。
pub const TILT10_LO_HZ: f64 = 20.0;
/// tilt10 斜率：−10 dB / log10(1000) = −10/3 dB per decade（§6.8）。
pub const TILT10_DB_PER_DECADE: f64 = 10.0 / 3.0;

/// 旧 `calibrate/target-curves.yml` 的 `tilt` 预设 id：−3 dB @ 16 kHz（16–20 kHz 平 −3 dB）。
///
/// **superseded**（用户口径：tilt10 = 20 Hz 0 dB → 20 kHz −10 dB log-f，见 DSP-HOST-DECISION §2.4）。
/// 本常量只用于追溯/迁移检查；**不得**作为默认目标、不得作为 `builtin_target` 的输入、
/// 不得参与设计默认值。历史文件 `calibrate/target-curves.yml` 不在本模块写域内，未改。
pub const SUPERSEDED_LEGACY_TILT_ID: &str = "legacy_tilt_minus3db_at_16k_superseded";
/// superseded 旧 tilt 的说明文本（报告/日志用；不是目标定义）。
pub const SUPERSEDED_LEGACY_TILT_NOTE: &str = "旧 calibrate/target-curves.yml tilt=[(20,0),(1000,0),(16000,-3),(20000,-3)] 已 superseded by tilt10；不得作为默认或内置目标";

/// 规范点数序列化算法标识（§6.3 第 1–4 条）。
pub const CANONICAL_POINTS_HASH_ALGO: &str = "es-number-shortest-canonical-points-v1";

// ---------------------------------------------------------------------------
// 拒收码（§6.9：28 码；§6.9.1：唯一码优先级）
// ---------------------------------------------------------------------------

/// 目标导入拒收码（§6.9 全 28 码）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TargetErrorCode {
    /// 1 schema 缺失/不等于 `tspi-target-draft/v1`（含 JSON 语法/非对象根）。
    Schema,
    /// 2 顶层未知非 `x_` 字段。
    FieldUnknown,
    /// 3 必填字段缺失。
    MissingField,
    /// 4 frequency/magnitude/phase 单位缺失或非 Hz/dB/deg。
    Unit,
    /// 5 runtime_rate 非法或 != profile fs。
    Rate,
    /// 6 CSV 表头/字段数/注释行/数字 token 非法/空字段。
    CsvFormat,
    /// 7 sidecar 携带 points、CSV+JSON 混用。
    SidecarMix,
    /// 8 points 元素出现未知字段（`x_` 除外）。
    PointField,
    /// 9 点数 < 2 或 > 20000。
    Points,
    /// 10 freq <= 0 或非有限。
    FreqInvalid,
    /// 11 freq 非严格递增/重复。
    FreqOrder,
    /// 12 freq > fs/2（magnitude-only 允许 = fs/2）。
    FreqRange,
    /// 13 mag/phase NaN/Inf。
    Nan,
    /// 14 某点缺 mag。
    MagMissing,
    /// 15 部分点有 phase 部分没有。
    PhaseMixed,
    /// 16 无 phase 却给 phase_unit，或有 phase 却没给。
    PhaseUnit,
    /// 17 有 phase 时 phase_ref 缺失/非法；delay_removed 缺 target_delay_samples。
    PhaseRef,
    /// 18 unwrapped 模式相邻 |Δφ| > 180°（auto 展开后仍越界）。
    PhaseJump,
    /// 19 带 phase 目标含 freq = fs/2（Nyquist 退化；静态层可判，见 stage 19）；或相位无法由
    /// 实系数因果 FIR 表示（静态层不可判，由 T37/T55 设计期承担，见 spec §6.9 #19）。
    PhaseReal,
    /// 20 所选族不接受 phase（或 corrected/lowdelay 缺 phase）。
    PhaseUnsupported,
    /// 21 target_delay_samples 非整数/越界/与 D_used 不一致。
    Delay,
    /// 22 interpolation 非 log_frequency_linear。
    Interp,
    /// 23 edge_policy 非法或 reject 无法覆盖 20 Hz/20 kHz。
    Edge,
    /// 24 normalization 非法。
    Norm,
    /// 25 reference_gain_db 超出 [-24, +24]。
    GainRange,
    /// 26 超过 +12 dB 提升且未 allow_boost（硬上限 +18）。
    Boost,
    /// 27 source_hash 缺失/不符/无法复算。
    Hash,
    /// 28 目标网格/相位要求超出 taps/延迟/预算（与设计方联合；INCOMPLETE 而非静默截断）。
    Budget,
}

impl TargetErrorCode {
    /// 全部 28 个码（§6.9 码表顺序 #1–#28）。
    pub const ALL: [TargetErrorCode; 28] = [
        TargetErrorCode::Schema,
        TargetErrorCode::FieldUnknown,
        TargetErrorCode::MissingField,
        TargetErrorCode::Unit,
        TargetErrorCode::Rate,
        TargetErrorCode::CsvFormat,
        TargetErrorCode::SidecarMix,
        TargetErrorCode::PointField,
        TargetErrorCode::Points,
        TargetErrorCode::FreqInvalid,
        TargetErrorCode::FreqOrder,
        TargetErrorCode::FreqRange,
        TargetErrorCode::Nan,
        TargetErrorCode::MagMissing,
        TargetErrorCode::PhaseMixed,
        TargetErrorCode::PhaseUnit,
        TargetErrorCode::PhaseRef,
        TargetErrorCode::PhaseJump,
        TargetErrorCode::PhaseReal,
        TargetErrorCode::PhaseUnsupported,
        TargetErrorCode::Delay,
        TargetErrorCode::Interp,
        TargetErrorCode::Edge,
        TargetErrorCode::Norm,
        TargetErrorCode::GainRange,
        TargetErrorCode::Boost,
        TargetErrorCode::Hash,
        TargetErrorCode::Budget,
    ];

    /// `E_TGT_*` 码字符串（§6.9 码表字面值）。
    pub fn as_str(self) -> &'static str {
        match self {
            TargetErrorCode::Schema => "E_TGT_SCHEMA",
            TargetErrorCode::FieldUnknown => "E_TGT_FIELD_UNKNOWN",
            TargetErrorCode::MissingField => "E_TGT_MISSING_FIELD",
            TargetErrorCode::Unit => "E_TGT_UNIT",
            TargetErrorCode::Rate => "E_TGT_RATE",
            TargetErrorCode::CsvFormat => "E_TGT_CSV_FORMAT",
            TargetErrorCode::SidecarMix => "E_TGT_SIDECAR_MIX",
            TargetErrorCode::PointField => "E_TGT_POINT_FIELD",
            TargetErrorCode::Points => "E_TGT_POINTS",
            TargetErrorCode::FreqInvalid => "E_TGT_FREQ_INVALID",
            TargetErrorCode::FreqOrder => "E_TGT_FREQ_ORDER",
            TargetErrorCode::FreqRange => "E_TGT_FREQ_RANGE",
            TargetErrorCode::Nan => "E_TGT_NAN",
            TargetErrorCode::MagMissing => "E_TGT_MAG_MISSING",
            TargetErrorCode::PhaseMixed => "E_TGT_PHASE_MIXED",
            TargetErrorCode::PhaseUnit => "E_TGT_PHASE_UNIT",
            TargetErrorCode::PhaseRef => "E_TGT_PHASE_REF",
            TargetErrorCode::PhaseJump => "E_TGT_PHASE_JUMP",
            TargetErrorCode::PhaseReal => "E_TGT_PHASE_REAL",
            TargetErrorCode::PhaseUnsupported => "E_TGT_PHASE_UNSUPPORTED",
            TargetErrorCode::Delay => "E_TGT_DELAY",
            TargetErrorCode::Interp => "E_TGT_INTERP",
            TargetErrorCode::Edge => "E_TGT_EDGE",
            TargetErrorCode::Norm => "E_TGT_NORM",
            TargetErrorCode::GainRange => "E_TGT_GAIN_RANGE",
            TargetErrorCode::Boost => "E_TGT_BOOST",
            TargetErrorCode::Hash => "E_TGT_HASH",
            TargetErrorCode::Budget => "E_TGT_BUDGET",
        }
    }

    /// 唯一码优先级 stage（§6.9.1 映射矩阵从上到下；数字越小越先命中）。
    pub fn stage(self) -> u8 {
        self.to_index() as u8 + 1
    }

    fn to_index(self) -> usize {
        TargetErrorCode::ALL
            .iter()
            .position(|code| *code == self)
            .expect("TargetErrorCode::ALL 覆盖全部变体")
    }
}

/// 目标导入错误（含 `kind=error` 语义：调用方把 `TargetErrorCode::as_str` 原样透传）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetError {
    /// 拒收码（§6.9）。
    pub code: TargetErrorCode,
    /// 字段路径（如 `points[3].freq_hz`、`sidecar.normalization`），无则为 `None`。
    pub field: Option<String>,
    /// 人读细节（含期望/实际值）。
    pub detail: String,
}

impl TargetError {
    /// 构造错误。
    pub fn new(code: TargetErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            field: None,
            detail: detail.into(),
        }
    }

    /// 带字段路径构造错误。
    pub fn with_field(
        code: TargetErrorCode,
        field: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            code,
            field: Some(field.into()),
            detail: detail.into(),
        }
    }

    /// 码字符串（`E_TGT_*`）。
    pub fn code_str(&self) -> &'static str {
        self.code.as_str()
    }
}

impl fmt::Display for TargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.field {
            Some(field) => write!(f, "{} [{}]: {}", self.code.as_str(), field, self.detail),
            None => write!(f, "{}: {}", self.code.as_str(), self.detail),
        }
    }
}

impl std::error::Error for TargetError {}

fn err_field(
    code: TargetErrorCode,
    field: impl Into<String>,
    detail: impl Into<String>,
) -> TargetError {
    TargetError::with_field(code, field, detail)
}

// ---------------------------------------------------------------------------
// 枚举（§6.3/§6.5/§6.6/§6.7）
// ---------------------------------------------------------------------------

/// normalization（§6.5）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Normalization {
    /// 按给定 mag_db 使用（producer 已锚定）。
    None,
    /// 整体平移使 1 kHz（插值）为 0 dB。
    Ref1k,
    /// 整体平移使 max(mag) = 0 dB。
    Peak,
}

impl Normalization {
    /// 从 sidecar/JSON 字段值解析。
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "none" => Some(Normalization::None),
            "ref_1k" => Some(Normalization::Ref1k),
            "peak" => Some(Normalization::Peak),
            _ => None,
        }
    }

    /// 字段字面值。
    pub fn as_str(self) -> &'static str {
        match self {
            Normalization::None => "none",
            Normalization::Ref1k => "ref_1k",
            Normalization::Peak => "peak",
        }
    }
}

/// 相位参考语义（§6.6）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhaseRef {
    /// 只有幅度目标；不得把缺 phase 当零相位。
    None,
    /// 期望的**总相位**（已展开，deg；含 −360·f·D/fs）。
    Absolute,
    /// 去粗延迟 φ_ex；总相位 φ_tot = φ_ex − 360·f·D/fs。
    DelayRemoved,
}

impl PhaseRef {
    /// 从字段值解析。
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "none" => Some(PhaseRef::None),
            "absolute" => Some(PhaseRef::Absolute),
            "delay_removed" => Some(PhaseRef::DelayRemoved),
            _ => None,
        }
    }

    /// 字段字面值。
    pub fn as_str(self) -> &'static str {
        match self {
            PhaseRef::None => "none",
            PhaseRef::Absolute => "absolute",
            PhaseRef::DelayRemoved => "delay_removed",
        }
    }

    /// 是否需要 phase 列（absolute/delay_removed）。
    pub fn has_phase(self) -> bool {
        !matches!(self, PhaseRef::None)
    }
}

/// 相位展开策略（§6.6）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnwrapPolicy {
    /// 已展开：要求相邻 |Δφ| <= 180°，否则 E_TGT_PHASE_JUMP。
    Unwrapped,
    /// 按最小跳变 ±360k 自动展开并报告 wrap 次数。
    Auto,
}

impl UnwrapPolicy {
    /// 从字段值解析。
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "unwrapped" => Some(UnwrapPolicy::Unwrapped),
            "auto" => Some(UnwrapPolicy::Auto),
            _ => None,
        }
    }

    /// 字段字面值。
    pub fn as_str(self) -> &'static str {
        match self {
            UnwrapPolicy::Unwrapped => "unwrapped",
            UnwrapPolicy::Auto => "auto",
        }
    }
}

/// 端点行为（§6.7）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgePolicy {
    /// 网格外取端点值（报告 hold 的频段）。
    Hold,
    /// 设计需覆盖 20 Hz/20 kHz 而网格未包含 → E_TGT_EDGE。
    Reject,
}

impl EdgePolicy {
    /// 从字段值解析。
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "hold" => Some(EdgePolicy::Hold),
            "reject" => Some(EdgePolicy::Reject),
            _ => None,
        }
    }

    /// 字段字面值。
    pub fn as_str(self) -> &'static str {
        match self {
            EdgePolicy::Hold => "hold",
            EdgePolicy::Reject => "reject",
        }
    }
}

/// 插值方式（§6.7：本规范只允许 log_frequency_linear）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interpolation {
    /// 按 log10(f) 线性插值 dB 与展开后的 phase。
    LogFrequencyLinear,
}

impl Interpolation {
    /// 从字段值解析。
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "log_frequency_linear" => Some(Interpolation::LogFrequencyLinear),
            _ => None,
        }
    }

    /// 字段字面值。
    pub fn as_str(self) -> &'static str {
        "log_frequency_linear"
    }
}

/// 导入形式（报告字段；`Builtin` 为模块内构造，不来自文件）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetForm {
    /// CSV + sidecar。
    Csv,
    /// 单 JSON。
    Json,
    /// 内置目标（flat/tilt10）。
    Builtin,
}

impl TargetForm {
    /// 报告字面值。
    pub fn as_str(self) -> &'static str {
        match self {
            TargetForm::Csv => "csv",
            TargetForm::Json => "json",
            TargetForm::Builtin => "builtin",
        }
    }
}

/// 采样点来源（报告 20 Hz/20 kHz 取值来源，§6.7）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoverageSource {
    /// 精确命中网格点（index）。
    GridPoint(usize),
    /// 两个网格点之间按 log-f 线性插值。
    Interpolated,
    /// 低于网格下界，按 edge_policy=hold 取下端点。
    HeldLow,
    /// 高于网格上界，按 edge_policy=hold 取上端点。
    HeldHigh,
    /// 网格外且 edge_policy=reject（未取到值）。
    OutsideGrid,
}

impl CoverageSource {
    /// 报告字面值（无 index 的部分）。
    pub fn as_str(self) -> &'static str {
        match self {
            CoverageSource::GridPoint(_) => "grid_point",
            CoverageSource::Interpolated => "interpolated",
            CoverageSource::HeldLow => "held_low",
            CoverageSource::HeldHigh => "held_high",
            CoverageSource::OutsideGrid => "outside_grid",
        }
    }

    fn index(self) -> Option<usize> {
        match self {
            CoverageSource::GridPoint(index) => Some(index),
            _ => None,
        }
    }
}

/// 目标消费族（§6.10 消费映射：谁接受 phase）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetFamily {
    /// linear：幅度家族，不允许 phase。
    Linear,
    /// min_fast：幅度家族，不允许 phase。
    MinFast,
    /// min_slow：幅度家族，不允许 phase。
    MinSlow,
    /// apodizing：幅度家族，不允许 phase。
    Apodizing,
    /// brickwall：幅度家族，不允许 phase。
    Brickwall,
    /// corrected：复误差族，必须 phase（absolute/delay_removed）。
    Corrected,
    /// lowdelay：复误差族，必须 phase。
    LowDelay,
    /// nos：旁路，phase 仅记录、不参与设计。
    Nos,
}

impl TargetFamily {
    /// 从族 id 解析（小写，见 §1.7/§6.10）。
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "linear" => Some(TargetFamily::Linear),
            "min_fast" => Some(TargetFamily::MinFast),
            "min_slow" => Some(TargetFamily::MinSlow),
            "apodizing" => Some(TargetFamily::Apodizing),
            "brickwall" => Some(TargetFamily::Brickwall),
            "corrected" => Some(TargetFamily::Corrected),
            "lowdelay" => Some(TargetFamily::LowDelay),
            "nos" => Some(TargetFamily::Nos),
            _ => None,
        }
    }

    /// 族 id。
    pub fn as_str(self) -> &'static str {
        match self {
            TargetFamily::Linear => "linear",
            TargetFamily::MinFast => "min_fast",
            TargetFamily::MinSlow => "min_slow",
            TargetFamily::Apodizing => "apodizing",
            TargetFamily::Brickwall => "brickwall",
            TargetFamily::Corrected => "corrected",
            TargetFamily::LowDelay => "lowdelay",
            TargetFamily::Nos => "nos",
        }
    }

    /// 是否**必须**有相位目标（corrected/lowdelay）。
    pub fn requires_phase(self) -> bool {
        matches!(self, TargetFamily::Corrected | TargetFamily::LowDelay)
    }

    /// 是否**不接受**相位目标（phase_ref=absolute/delay_removed 默认拒收）。
    pub fn forbids_phase(self) -> bool {
        matches!(
            self,
            TargetFamily::Linear
                | TargetFamily::MinFast
                | TargetFamily::MinSlow
                | TargetFamily::Apodizing
                | TargetFamily::Brickwall
        )
    }
}

/// 报告中的 `phase_target` 类别（§6.6：none/absolute/delay_removed/ignored）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhaseTargetClass {
    /// 只有幅度目标（缺 phase 必须由调用方显式选择设计家族，不得当零相位）。
    None,
    /// 期望总相位。
    Absolute,
    /// 去粗延迟相位（设计时用 φ_tot）。
    DelayRemoved,
    /// 调用者显式允许忽略 phase（仅幅度族；报告写 `ignored`）。
    Ignored,
}

impl PhaseTargetClass {
    /// 报告字面值。
    pub fn as_str(self) -> &'static str {
        match self {
            PhaseTargetClass::None => "none",
            PhaseTargetClass::Absolute => "absolute",
            PhaseTargetClass::DelayRemoved => "delay_removed",
            PhaseTargetClass::Ignored => "ignored",
        }
    }
}

/// 内置目标（§6.8）。默认 `BuiltinTarget::Flat`；旧 −3 dB@16k tilt 为 superseded，不在枚举内。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinTarget {
    /// flat：全带 0 dB。
    Flat,
    /// tilt10：20 Hz 0 dB → 20 kHz −10 dB，log-f。
    Tilt10,
}

impl BuiltinTarget {
    /// 全部内置目标（仅 flat/tilt10；旧 tilt 已 superseded 不在此表）。
    pub const ALL: [BuiltinTarget; 2] = [BuiltinTarget::Flat, BuiltinTarget::Tilt10];
    /// 默认目标：flat（不是旧 −3 dB@16k tilt）。
    pub const DEFAULT: BuiltinTarget = BuiltinTarget::Flat;

    /// 目标 id（`flat`/`tilt10`）。
    pub fn id(self) -> &'static str {
        match self {
            BuiltinTarget::Flat => "flat",
            BuiltinTarget::Tilt10 => "tilt10",
        }
    }

    /// 从 id 解析；**不接受** superseded 旧 tilt id。
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "flat" => Some(BuiltinTarget::Flat),
            "tilt10" => Some(BuiltinTarget::Tilt10),
            _ => None,
        }
    }

    /// 人读名称。
    pub fn display_name(self) -> &'static str {
        match self {
            BuiltinTarget::Flat => "flat（全带 0 dB）",
            BuiltinTarget::Tilt10 => "tilt10（20 Hz 0 dB → 20 kHz −10 dB，log-f）",
        }
    }
}

/// 警告码（不拒收；§6.4 判据 5 / §6.3 x_ 字段 / §6.5 boost）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetWarningCode {
    /// 低频段（≤200 Hz）网格步长粗于 1/48 oct（建议值，不拒收）。
    GridResolutionLow,
    /// 1–20 kHz 网格步长粗于 1/12 oct（建议值，不拒收）。
    GridResolutionHigh,
    /// max_boost_db > 6 dB（未到拒收阈值）。
    Boost,
    /// 顶层 `x_` 扩展字段（warning，不参与语义）。
    XFieldTopLevel,
    /// points 元素 `x_` 扩展字段（warning，不参与语义）。
    XFieldPoint,
    /// `note` 超过 500 字符（人读字段，warning）。
    NoteTooLong,
    /// `note` 非字符串（人读字段，warning 并忽略）。
    NoteNotString,
    /// 顶层字段重复（保留第一次出现的值，warning）。
    DuplicateField,
    /// 无 phase 时给了 unwrap_policy（warning 并忽略）。
    PhaseMetaIgnored,
    /// edge_policy=hold 且设计采样超出网格（warning，值取端点）。
    EdgeHeld,
    /// 单文件 JSON/Builtin 的 source_file_sha256 无法自引用校验（只登记）。
    SourceFileHashNotVerifiable,
}

impl TargetWarningCode {
    /// 报告字面值。
    pub fn as_str(self) -> &'static str {
        match self {
            TargetWarningCode::GridResolutionLow => "W_TGT_GRID_RESOLUTION_LOW",
            TargetWarningCode::GridResolutionHigh => "W_TGT_GRID_RESOLUTION_HIGH",
            TargetWarningCode::Boost => "W_TGT_BOOST",
            TargetWarningCode::XFieldTopLevel => "W_TGT_X_FIELD_TOP",
            TargetWarningCode::XFieldPoint => "W_TGT_X_FIELD_POINT",
            TargetWarningCode::NoteTooLong => "W_TGT_NOTE_TOO_LONG",
            TargetWarningCode::NoteNotString => "W_TGT_NOTE_NOT_STRING",
            TargetWarningCode::DuplicateField => "W_TGT_DUPLICATE_FIELD",
            TargetWarningCode::PhaseMetaIgnored => "W_TGT_PHASE_META_IGNORED",
            TargetWarningCode::EdgeHeld => "W_TGT_EDGE_HELD",
            TargetWarningCode::SourceFileHashNotVerifiable => "W_TGT_SOURCE_FILE_HASH_UNVERIFIABLE",
        }
    }
}

/// 一条导入警告。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetWarning {
    /// 警告码。
    pub code: TargetWarningCode,
    /// 人读细节。
    pub detail: String,
}

impl TargetWarning {
    fn new(code: TargetWarningCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// 数据结构
// ---------------------------------------------------------------------------

/// 目标曲线单点（原始导入值，未归一化）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TargetPoint {
    /// 频率（Hz，> 0，<= fs/2；带 phase 时 < fs/2）。
    pub freq_hz: f64,
    /// 幅度（dB，有限）。
    pub mag_db: f64,
    /// 相位（deg，有限；`None` = 无 phase）。
    pub phase_deg: Option<f64>,
}

impl TargetPoint {
    /// 仅幅度点。
    pub fn new(freq_hz: f64, mag_db: f64) -> Self {
        Self {
            freq_hz,
            mag_db,
            phase_deg: None,
        }
    }

    /// 加相位。
    pub fn with_phase(mut self, phase_deg: f64) -> Self {
        self.phase_deg = Some(phase_deg);
        self
    }
}

/// 目标元数据（§6.3；已通过单位/取值校验）。
#[derive(Clone, Debug)]
pub struct TargetMeta {
    /// schema（= `TARGET_SCHEMA`）。
    pub schema: String,
    /// frequency_unit（= "Hz"）。
    pub frequency_unit: String,
    /// magnitude_unit（= "dB"）。
    pub magnitude_unit: String,
    /// phase_unit（有 phase 时 = "deg"；否则 None）。
    pub phase_unit: Option<String>,
    /// runtime_rate（Hz；必须 == profile fs）。
    pub runtime_rate: f64,
    /// normalization（§6.5）。
    pub normalization: Normalization,
    /// reference_gain_db（归一化之后整体相加；§6.5）。
    pub reference_gain_db: f64,
    /// 相位参考语义（§6.6）。
    pub phase_ref: PhaseRef,
    /// 展开策略（有 phase 时 Some）。
    pub unwrap_policy: Option<UnwrapPolicy>,
    /// `phase_ref=delay_removed` 的 D_target（整数 sample，0..=200 ms）。
    pub target_delay_samples: Option<u32>,
    /// 插值（§6.7）。
    pub interpolation: Interpolation,
    /// 端点行为（§6.7）。
    pub edge_policy: EdgePolicy,
    /// CSV 形式必填 / JSON 可选（§6.3）。
    pub source_hash: Option<String>,
    /// 原始文件字节哈希（可选，可选校验；§6.3）。
    pub source_file_sha256: Option<String>,
    /// 人读备注（<=500 字符；超长为 warning）。
    pub note: Option<String>,
}

/// 归一化/参考增益/端点/哈希报告（§6.5/§6.7）。
#[derive(Clone, Debug)]
pub struct TargetImportReport {
    /// 导入形式。
    pub form: TargetForm,
    /// 点数。
    pub point_count: usize,
    /// 是否有相位目标。
    pub has_phase: bool,
    /// `phase_target` 类别（§6.6）。
    pub phase_target: PhaseTargetClass,
    /// 调用方声明/选择的族（None 表示未声明）。
    pub phase_family_used: Option<TargetFamily>,
    /// normalization。
    pub normalization: Normalization,
    /// normalization 实际平移量（dB；ref_1k/peak 计算值，none 为 0）。
    pub normalization_offset_db: f64,
    /// reference_gain_db（§6.5）。
    pub reference_gain_db: f64,
    /// 实际总平移 = normalization_offset_db + reference_gain_db（报告分别记录三项）。
    pub applied_offset_db: f64,
    /// max_boost_db = max(mag_norm + ref) − (mag_norm(1 kHz) + ref)（§6.5，与 ref 无关）。
    pub max_boost_db: f64,
    /// max_boost_db > 6 dB。
    pub boost_warning: bool,
    /// 调用方是否显式 allow_boost。
    pub allow_boost: bool,
    /// 网格最小频点（Hz）。
    pub min_freq_hz: f64,
    /// 网格最大频点（Hz）。
    pub max_freq_hz: f64,
    /// 相邻点最小间隔（Hz）。
    pub min_interval_hz: f64,
    /// 相邻点最大间隔（Hz）。
    pub max_interval_hz: f64,
    /// 相邻点最小 octave 间隔。
    pub min_octave_interval: f64,
    /// 相邻点最大 octave 间隔。
    pub max_octave_interval: f64,
    /// 网格是否覆盖 20 Hz–20 kHz。
    pub covers_20_20k: bool,
    /// 20 Hz 处取值与来源（§6.7 必须报告）。
    pub at_20hz: CoverageValue,
    /// 20 kHz 处取值与来源（§6.7 必须报告）。
    pub at_20khz: CoverageValue,
    /// 相位展开报告（有 phase 时）。
    pub unwrap: Option<UnwrapReport>,
    /// target_delay_samples（delay_removed 时）。
    pub target_delay_samples: Option<u32>,
    /// 规范点序列 sha256（§6.3）。
    pub canonical_points_sha256: String,
    /// 声明的 source_hash（如给出/要求）。
    pub source_hash: Option<String>,
    /// source_hash 是否已按规范复算通过。
    pub source_hash_verified: bool,
    /// 声明的 source_file_sha256（如给出）。
    pub source_file_sha256: Option<String>,
    /// source_file_sha256 是否与实际字节一致。
    pub source_file_sha256_verified: bool,
    /// 规范点哈希算法标识。
    pub canonical_points_hash_algo: &'static str,
    /// 警告（不拒收）。
    pub warnings: Vec<TargetWarning>,
    /// 信息/说明（含 superseded 说明与 phase_target=none 的族声明要求）。
    pub notes: Vec<String>,
}

/// 端点/覆盖取值（§6.7）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoverageValue {
    /// 值（`None` = 网格外且 reject 未取值）。
    pub value: Option<f64>,
    /// 取值来源。
    pub source: CoverageSource,
}

/// 相位展开报告（§6.6）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UnwrapReport {
    /// 策略。
    pub policy: UnwrapPolicy,
    /// auto 模式下应用的 ±360° 展开次数。
    pub wraps_applied: usize,
    /// 展开后相邻最大 |Δφ|（deg）。
    pub max_abs_delta_deg: f64,
    /// 原始值相邻最大 |Δφ|（deg；用于说明 unwrapped 检查依据）。
    pub raw_max_abs_delta_deg: f64,
}

/// 单个频率采样结果（非模态 solver 接口返回类型）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TargetSample {
    /// 采样频率（Hz）。
    pub freq_hz: f64,
    /// 最终幅度（归一化 + reference_gain_db 之后）。
    pub mag_db: f64,
    /// 形状幅度（归一化之后、reference_gain_db 之前）。
    pub mag_shape_db: f64,
    /// 目标总相位（deg；absolute 为给定值，delay_removed 已扣 360·f·D/fs；无 phase 为 None）。
    pub phase_total_deg: Option<f64>,
    /// 去粗延迟相位 φ_ex（deg；无 phase 为 None）。
    pub phase_ex_deg: Option<f64>,
    /// 幅度取值来源。
    pub mag_source: CoverageSource,
    /// 相位取值来源。
    pub phase_source: Option<CoverageSource>,
}

/// 设计预算（§6.9 码 28：与设计方联合判定目标网格/相位是否超 taps/延迟/预算）。
#[derive(Clone, Debug, Default)]
pub struct TargetBudget {
    /// 允许的最大点数（None 不检查）。
    pub max_points: Option<usize>,
    /// 允许的延迟预算（sample；对 delay_removed 目标检查 D_target）。
    pub delay_budget_samples: Option<f64>,
    /// profile 允许的 taps（None 不检查；phase 目标要求 >= 8）。
    pub max_taps: Option<usize>,
}

/// 预算检查报告。
#[derive(Clone, Debug)]
pub struct BudgetReport {
    /// 点数。
    pub point_count: usize,
    /// delay_removed 目标所需延迟（sample）；其它为 None。
    pub required_delay_samples: Option<f64>,
    /// 说明（不可静态判定项登记在此）。
    pub notes: Vec<String>,
}

/// 导入选项（不来自文件；调用方/设计器提供）。
#[derive(Clone, Debug)]
pub struct ImportOptions {
    /// 目标 profile 采样率（Hz）；runtime_rate 必须严格相等（§6.3 E_TGT_RATE）。
    pub profile_rate_hz: f64,
    /// 是否显式允许 > +12 dB 提升（硬上限 +18 仍拒收；§6.5）。
    pub allow_boost: bool,
    /// edge_policy=reject 时是否要求网格覆盖 20 Hz–20 kHz（§6.7；默认 true）。
    pub require_20_20k_coverage: bool,
    /// 调用方目标族（§6.10：phase 接受性检查；None = 不检查）。
    pub family: Option<TargetFamily>,
    /// 仅幅度族时是否显式请求忽略 phase（默认 false：E_TGT_PHASE_UNSUPPORTED）。
    pub allow_phase_ignored: bool,
    /// 设计预算（§6.9 码 28；None = 不检查）。
    pub budget: Option<TargetBudget>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self::new(DEFAULT_RUNTIME_RATE_HZ)
    }
}

impl ImportOptions {
    /// 以 profile 采样率构造默认选项。
    pub fn new(profile_rate_hz: f64) -> Self {
        Self {
            profile_rate_hz,
            allow_boost: false,
            require_20_20k_coverage: true,
            family: None,
            allow_phase_ignored: false,
            budget: None,
        }
    }

    /// 声明目标族（§6.10）。
    pub fn with_family(mut self, family: TargetFamily) -> Self {
        self.family = Some(family);
        self
    }

    /// 显式允许 boost > 12 dB（仍受硬上限 +18）。
    pub fn with_allow_boost(mut self, allow: bool) -> Self {
        self.allow_boost = allow;
        self
    }

    /// 是否要求 20 Hz–20 kHz 覆盖。
    pub fn with_require_20_20k_coverage(mut self, require: bool) -> Self {
        self.require_20_20k_coverage = require;
        self
    }

    /// 仅幅度族时显式忽略 phase。
    pub fn with_allow_phase_ignored(mut self, allow: bool) -> Self {
        self.allow_phase_ignored = allow;
        self
    }

    /// 设计预算。
    pub fn with_budget(mut self, budget: TargetBudget) -> Self {
        self.budget = Some(budget);
        self
    }

    fn validate(&self) -> Result<(), TargetError> {
        if !self.profile_rate_hz.is_finite() || self.profile_rate_hz <= 0.0 {
            return Err(err_field(
                TargetErrorCode::Rate,
                "options.profile_rate_hz",
                format!("profile fs 必须是有限正数，实际 {}", self.profile_rate_hz),
            ));
        }
        Ok(())
    }
}

/// 内置目标构造选项。
#[derive(Clone, Debug)]
pub struct BuiltinOptions {
    /// 运行采样率（Hz；20 kHz 必须 < fs/2）。
    pub fs: f64,
    /// reference gain（归一化之后整体相加；flat/tilt10 默认 0，§6.8）。
    pub reference_gain_db: f64,
}

impl BuiltinOptions {
    /// 默认 reference_gain_db = 0。
    pub fn new(fs: f64) -> Self {
        Self {
            fs,
            reference_gain_db: 0.0,
        }
    }

    /// 设置 reference gain（独立于曲线形状）。
    pub fn with_reference_gain_db(mut self, reference_gain_db: f64) -> Self {
        self.reference_gain_db = reference_gain_db;
        self
    }
}

/// 导入后的目标曲线（归一化/相位已解析，可采样给非模态 solver）。
#[derive(Clone, Debug)]
pub struct TargetCurve {
    meta: TargetMeta,
    points: Vec<TargetPoint>,
    mag_normalized_db: Vec<f64>,
    mag_final_db: Vec<f64>,
    phase_ex_unwrapped_deg: Option<Vec<f64>>,
    phase_total_deg: Option<Vec<f64>>,
    canonical_points_sha256: String,
    report: TargetImportReport,
}

// ---------------------------------------------------------------------------
// sha256（纯 std；§6.3 source_hash 校验）
// ---------------------------------------------------------------------------

/// SHA-256（FIPS 180-4）；返回 32 字节。
pub fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let mut message = Vec::with_capacity(data.len() + 72);
    message.extend_from_slice(data);
    let bit_len = (data.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    let mut w = [0u32; 64];
    for chunk in message.chunks_exact(64) {
        for (i, word) in w.iter_mut().take(16).enumerate() {
            let base = i * 4;
            *word = u32::from_be_bytes([
                chunk[base],
                chunk[base + 1],
                chunk[base + 2],
                chunk[base + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut hh = h[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// SHA-256 小写十六进制（§6.3 `sha256_hex`）。
pub fn sha256_hex(data: &[u8]) -> String {
    let digest = sha256(data);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(char::from_digit((byte >> 4) as u32, 16).expect("hex"));
        out.push(char::from_digit((byte & 0x0f) as u32, 16).expect("hex"));
    }
    out
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn hex_eq_ignore_case(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

// ---------------------------------------------------------------------------
// 规范数字序列化（§6.3 第 1–4 条：ECMAScript Number::toString 风格最短往返）
// ---------------------------------------------------------------------------

/// 把有限 f64 序列化为 §6.3 规范十进制（最短往返；整数不带 `.0`；指数用 `e+`/`e-`）。
///
/// 采用 ECMAScript `Number::toString` 的格式规则（k/n 规则），数字本身用 Rust 最短往返
/// `{:e}` 取得。非有限值返回 `None`。
pub fn canonical_number(value: f64) -> Option<String> {
    if !value.is_finite() {
        return None;
    }
    if value == 0.0 {
        return Some("0".to_string());
    }
    let neg = value.is_sign_negative();
    let abs = value.abs();
    let exp_form = format!("{:e}", abs);
    let (mantissa, exp_str) = exp_form.split_once('e').expect("LowerExp 总是包含 e");
    let exp: i32 = exp_str.parse().expect("LowerExp 指数为整数");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exp + 1;
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if k <= n && n <= 21 {
        out.push_str(&digits);
        for _ in 0..(n - k) {
            out.push('0');
        }
    } else if 0 < n && n <= 21 {
        let split = n as usize;
        out.push_str(&digits[..split]);
        out.push('.');
        out.push_str(&digits[split..]);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        for _ in 0..(-n) {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        let exponent = n - 1;
        if exponent >= 0 {
            out.push('+');
        }
        out.push_str(&exponent.to_string());
    }
    Some(out)
}

/// 目标点的规范序列化（§6.3：键序 freq_hz/mag_db/phase_deg、无空格、无 phase 省略键）。
pub fn canonical_points_json(points: &[TargetPoint]) -> Result<String, TargetError> {
    let mut out = String::from("[");
    for (index, point) in points.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let freq = canonical_number(point.freq_hz).ok_or_else(|| {
            err_field(
                TargetErrorCode::Nan,
                format!("points[{index}].freq_hz"),
                "freq_hz 非有限，无法规范序列化",
            )
        })?;
        let mag = canonical_number(point.mag_db).ok_or_else(|| {
            err_field(
                TargetErrorCode::Nan,
                format!("points[{index}].mag_db"),
                "mag_db 非有限，无法规范序列化",
            )
        })?;
        out.push_str("{\"freq_hz\":");
        out.push_str(&freq);
        out.push_str(",\"mag_db\":");
        out.push_str(&mag);
        if let Some(phase) = point.phase_deg {
            let phase = canonical_number(phase).ok_or_else(|| {
                err_field(
                    TargetErrorCode::Nan,
                    format!("points[{index}].phase_deg"),
                    "phase_deg 非有限，无法规范序列化",
                )
            })?;
            out.push_str(",\"phase_deg\":");
            out.push_str(&phase);
        }
        out.push('}');
    }
    out.push(']');
    Ok(out)
}

/// 规范点序列哈希（§6.3 `source_hash` 算法）。
pub fn canonical_points_sha256(points: &[TargetPoint]) -> Result<String, TargetError> {
    let text = canonical_points_json(points)?;
    Ok(sha256_hex(text.as_bytes()))
}

// ---------------------------------------------------------------------------
// 最小 JSON 解析器（纯 std；离线构建无 serde）
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn as_obj(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Obj(fields) => Some(fields),
            _ => None,
        }
    }

    fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(items) => Some(items),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct JsonParseError {
    position: usize,
    message: String,
}

struct JsonParser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> JsonParser<'a> {
    fn parse_document(bytes: &'a [u8]) -> Result<Json, JsonParseError> {
        let mut parser = JsonParser { bytes, pos: 0 };
        parser.skip_whitespace();
        let value = parser.parse_value(0)?;
        parser.skip_whitespace();
        if parser.pos != bytes.len() {
            return Err(parser.error("文档末尾存在多余内容"));
        }
        Ok(value)
    }

    fn error(&self, message: impl Into<String>) -> JsonParseError {
        JsonParseError {
            position: self.pos,
            message: message.into(),
        }
    }

    fn skip_whitespace(&mut self) {
        while let Some(&byte) = self.bytes.get(self.pos) {
            if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn parse_value(&mut self, depth: usize) -> Result<Json, JsonParseError> {
        if depth > 64 {
            return Err(self.error("JSON 嵌套过深（>64）"));
        }
        self.skip_whitespace();
        match self.peek() {
            Some(b'{') => self.parse_object(depth),
            Some(b'[') => self.parse_array(depth),
            Some(b'"') => Ok(Json::Str(self.parse_string()?)),
            Some(b't') => {
                self.expect_literal("true")?;
                Ok(Json::Bool(true))
            }
            Some(b'f') => {
                self.expect_literal("false")?;
                Ok(Json::Bool(false))
            }
            Some(b'n') => {
                self.expect_literal("null")?;
                Ok(Json::Null)
            }
            Some(b'-') | Some(b'0'..=b'9') => self.parse_number(),
            Some(_) => Err(self.error("非法 JSON 值起始字符")),
            None => Err(self.error("JSON 提前结束")),
        }
    }

    fn expect_literal(&mut self, literal: &str) -> Result<(), JsonParseError> {
        let end = self.pos + literal.len();
        if self.bytes.len() >= end && &self.bytes[self.pos..end] == literal.as_bytes() {
            self.pos = end;
            Ok(())
        } else {
            Err(self.error(format!("期望字面量 {literal}")))
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<Json, JsonParseError> {
        self.pos += 1; // {
        let mut fields = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Obj(fields));
        }
        loop {
            self.skip_whitespace();
            if self.peek() != Some(b'"') {
                return Err(self.error("对象键必须是字符串"));
            }
            let key = self.parse_string()?;
            self.skip_whitespace();
            if self.peek() != Some(b':') {
                return Err(self.error("对象键后缺冒号"));
            }
            self.pos += 1;
            let value = self.parse_value(depth + 1)?;
            fields.push((key, value));
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(self.error("对象成员间缺逗号或右花括号")),
            }
        }
        Ok(Json::Obj(fields))
    }

    fn parse_array(&mut self, depth: usize) -> Result<Json, JsonParseError> {
        self.pos += 1; // [
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            items.push(self.parse_value(depth + 1)?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(self.error("数组元素间缺逗号或右方括号")),
            }
        }
        Ok(Json::Arr(items))
    }

    fn parse_string(&mut self) -> Result<String, JsonParseError> {
        debug_assert_eq!(self.peek(), Some(b'"'));
        self.pos += 1;
        let mut out = String::new();
        loop {
            let start = self.pos;
            while let Some(&byte) = self.bytes.get(self.pos) {
                if byte == b'"' || byte == b'\\' {
                    break;
                }
                if byte < 0x20 {
                    return Err(self.error("字符串中出现未转义控制字符"));
                }
                self.pos += 1;
            }
            let chunk = &self.bytes[start..self.pos];
            match std::str::from_utf8(chunk) {
                Ok(text) => out.push_str(text),
                Err(_) => return Err(self.error("字符串不是合法 UTF-8")),
            }
            match self.peek() {
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    self.parse_escape(&mut out)?;
                }
                _ => return Err(self.error("字符串提前结束")),
            }
        }
    }

    fn parse_escape(&mut self, out: &mut String) -> Result<(), JsonParseError> {
        let escape = match self.peek() {
            Some(byte) => byte,
            None => return Err(self.error("转义序列提前结束")),
        };
        self.pos += 1;
        match escape {
            b'"' => out.push('"'),
            b'\\' => out.push('\\'),
            b'/' => out.push('/'),
            b'b' => out.push('\u{0008}'),
            b'f' => out.push('\u{000c}'),
            b'n' => out.push('\n'),
            b'r' => out.push('\r'),
            b't' => out.push('\t'),
            b'u' => {
                let high = self.parse_hex4()?;
                let code = if (0xd800..=0xdbff).contains(&high) {
                    if self.peek() != Some(b'\\') {
                        return Err(self.error("代理对缺低位转义"));
                    }
                    self.pos += 1;
                    if self.peek() != Some(b'u') {
                        return Err(self.error("代理对低位必须以 \\u 开始"));
                    }
                    self.pos += 1;
                    let low = self.parse_hex4()?;
                    if !(0xdc00..=0xdfff).contains(&low) {
                        return Err(self.error("代理对低位不在 U+DC00..U+DFFF"));
                    }
                    0x10000 + ((high as u32 - 0xd800) << 10) + (low as u32 - 0xdc00)
                } else if (0xdc00..=0xdfff).contains(&high) {
                    return Err(self.error("孤立代理项"));
                } else {
                    high as u32
                };
                match char::from_u32(code) {
                    Some(ch) => out.push(ch),
                    None => return Err(self.error("非法 unicode 标量")),
                }
            }
            _ => return Err(self.error("未知转义字符")),
        }
        Ok(())
    }

    fn parse_hex4(&mut self) -> Result<u16, JsonParseError> {
        if self.pos + 4 > self.bytes.len() {
            return Err(self.error("\\u 转义字符不足"));
        }
        let text = std::str::from_utf8(&self.bytes[self.pos..self.pos + 4])
            .map_err(|_| self.error("\\u 转义不是 ASCII"))?;
        let value = u16::from_str_radix(text, 16).map_err(|_| self.error("\\u 转义非十六进制"))?;
        self.pos += 4;
        Ok(value)
    }

    fn parse_number(&mut self) -> Result<Json, JsonParseError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return Err(self.error("JSON 数字不允许前导零"));
                }
            }
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => return Err(self.error("JSON 数字缺整数部分")),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("JSON 数字小数点后缺数字"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("JSON 数字指数部分缺数字"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        let token = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| self.error("JSON 数字不是 ASCII"))?;
        let value = token
            .parse::<f64>()
            .map_err(|_| self.error("JSON 数字无法解析为 f64"))?;
        Ok(Json::Num(value))
    }
}

// ---------------------------------------------------------------------------
// CSV 解析（§6.2）
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default)]
struct RawPoint {
    freq_hz: Option<f64>,
    mag_db: Option<f64>,
    phase_deg: Option<f64>,
    has_phase_key: bool,
    freq_invalid_type: bool,
    mag_invalid_type: bool,
    phase_invalid_type: bool,
}

struct CsvData {
    points: Vec<RawPoint>,
    has_phase: bool,
}

fn parse_csv(csv_bytes: &[u8]) -> Result<CsvData, TargetError> {
    if csv_bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        return Err(err_field(
            TargetErrorCode::CsvFormat,
            "csv",
            "CSV 含 UTF-8 BOM；表头必须精确为 freq_hz,mag_db 或 freq_hz,mag_db,phase_deg",
        ));
    }
    let text = std::str::from_utf8(csv_bytes)
        .map_err(|_| err_field(TargetErrorCode::CsvFormat, "csv", "CSV 不是合法 UTF-8"))?;
    let mut lines: Vec<&str> = text.split('\n').collect();
    // 去掉文件末尾的空行（trailing newline 合法），仅允许末尾。
    while matches!(lines.last(), Some(line) if line.trim().is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return Err(err_field(
            TargetErrorCode::CsvFormat,
            "csv",
            "CSV 为空（至少需要表头 + 2 个点）",
        ));
    }
    let header_raw = lines[0].trim_end_matches('\r').trim();
    let (expected_fields, has_phase) = if header_raw == CSV_HEADER_MAG {
        (2usize, false)
    } else if header_raw == CSV_HEADER_PHASE {
        (3usize, true)
    } else {
        return Err(err_field(
            TargetErrorCode::CsvFormat,
            "csv.header",
            format!("表头必须是 {CSV_HEADER_MAG} 或 {CSV_HEADER_PHASE}，实际 \"{header_raw}\""),
        ));
    };

    let mut points = Vec::with_capacity(lines.len().saturating_sub(1));
    for (index, raw_line) in lines.iter().enumerate().skip(1) {
        let line_no = index + 1;
        let line = raw_line.trim_end_matches('\r');
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Err(err_field(
                TargetErrorCode::CsvFormat,
                format!("csv.line{line_no}"),
                "CSV 中间出现空行",
            ));
        }
        if trimmed.starts_with('#') {
            return Err(err_field(
                TargetErrorCode::CsvFormat,
                format!("csv.line{line_no}"),
                "CSV 不允许注释行（# 行）",
            ));
        }
        let mut fields = Vec::with_capacity(expected_fields);
        for cell in line.split(',') {
            let cell = cell.trim();
            if cell.is_empty() {
                return Err(err_field(
                    TargetErrorCode::CsvFormat,
                    format!("csv.line{line_no}"),
                    "CSV 出现空字段",
                ));
            }
            fields.push(cell);
        }
        if fields.len() != expected_fields {
            return Err(err_field(
                TargetErrorCode::CsvFormat,
                format!("csv.line{line_no}"),
                format!(
                    "字段数与表头不符：表头 {expected_fields} 列，实际 {} 列",
                    fields.len()
                ),
            ));
        }
        let mut point = RawPoint::default();
        match parse_csv_number(fields[0], line_no, "freq_hz") {
            Ok(value) => point.freq_hz = Some(value),
            Err(error) => return Err(error),
        }
        match parse_csv_number(fields[1], line_no, "mag_db") {
            Ok(value) => point.mag_db = Some(value),
            Err(error) => return Err(error),
        }
        if has_phase {
            match parse_csv_number(fields[2], line_no, "phase_deg") {
                Ok(value) => {
                    point.phase_deg = Some(value);
                    point.has_phase_key = true;
                }
                Err(error) => return Err(error),
            }
        }
        points.push(point);
    }

    Ok(CsvData { points, has_phase })
}

fn parse_csv_number(token: &str, line_no: usize, field: &str) -> Result<f64, TargetError> {
    token.parse::<f64>().map_err(|_| {
        err_field(
            TargetErrorCode::CsvFormat,
            format!("csv.line{line_no}.{field}"),
            format!("数字 token 非法: \"{token}\""),
        )
    })
}

// ---------------------------------------------------------------------------
// sidecar / 单 JSON 元数据解析（§6.3）
// ---------------------------------------------------------------------------

const KNOWN_TOP_LEVEL_FIELDS: [&str; 16] = [
    "schema",
    "frequency_unit",
    "magnitude_unit",
    "phase_unit",
    "runtime_rate",
    "normalization",
    "reference_gain_db",
    "phase_ref",
    "unwrap_policy",
    "target_delay_samples",
    "interpolation",
    "edge_policy",
    "source_hash",
    "source_file_sha256",
    "note",
    "points",
];

#[derive(Default)]
struct RawMeta {
    schema: Option<String>,
    schema_invalid: bool,
    frequency_unit: Option<String>,
    frequency_unit_invalid: bool,
    magnitude_unit: Option<String>,
    magnitude_unit_invalid: bool,
    phase_unit: Option<String>,
    phase_unit_invalid: bool,
    runtime_rate: Option<f64>,
    runtime_rate_invalid: bool,
    normalization: Option<String>,
    normalization_invalid: bool,
    reference_gain_db: Option<f64>,
    reference_gain_invalid: bool,
    phase_ref: Option<String>,
    phase_ref_invalid: bool,
    unwrap_policy: Option<String>,
    unwrap_policy_invalid: bool,
    target_delay_samples: Option<f64>,
    target_delay_invalid: bool,
    interpolation: Option<String>,
    interpolation_invalid: bool,
    edge_policy: Option<String>,
    edge_policy_invalid: bool,
    source_hash: Option<String>,
    source_hash_invalid: bool,
    source_file_sha256: Option<String>,
    source_file_sha256_invalid: bool,
    note: Option<String>,
    has_points_field: bool,
    unknown_fields: Vec<String>,
    x_fields: Vec<String>,
    duplicate_fields: Vec<String>,
    warnings: Vec<TargetWarning>,
}

fn parse_raw_meta(object: &[(String, Json)]) -> RawMeta {
    let mut meta = RawMeta::default();
    let mut seen: Vec<&str> = Vec::with_capacity(object.len());
    for (key, value) in object {
        if seen.contains(&key.as_str()) {
            meta.duplicate_fields.push(key.clone());
            continue;
        }
        seen.push(key.as_str());
        if key.starts_with("x_") {
            meta.x_fields.push(key.clone());
            continue;
        }
        if !KNOWN_TOP_LEVEL_FIELDS.contains(&key.as_str()) {
            meta.unknown_fields.push(key.clone());
            continue;
        }
        match key.as_str() {
            "schema" => match value {
                Json::Str(text) => meta.schema = Some(text.clone()),
                _ => meta.schema_invalid = true,
            },
            "frequency_unit" => match value {
                Json::Str(text) => meta.frequency_unit = Some(text.clone()),
                _ => meta.frequency_unit_invalid = true,
            },
            "magnitude_unit" => match value {
                Json::Str(text) => meta.magnitude_unit = Some(text.clone()),
                _ => meta.magnitude_unit_invalid = true,
            },
            "phase_unit" => match value {
                Json::Str(text) => meta.phase_unit = Some(text.clone()),
                Json::Null => {}
                _ => meta.phase_unit_invalid = true,
            },
            "runtime_rate" => match value {
                Json::Num(number) => meta.runtime_rate = Some(*number),
                _ => meta.runtime_rate_invalid = true,
            },
            "normalization" => match value {
                Json::Str(text) => meta.normalization = Some(text.clone()),
                _ => meta.normalization_invalid = true,
            },
            "reference_gain_db" => match value {
                Json::Num(number) => meta.reference_gain_db = Some(*number),
                Json::Null => {}
                _ => meta.reference_gain_invalid = true,
            },
            "phase_ref" => match value {
                Json::Str(text) => meta.phase_ref = Some(text.clone()),
                Json::Null => {}
                _ => meta.phase_ref_invalid = true,
            },
            "unwrap_policy" => match value {
                Json::Str(text) => meta.unwrap_policy = Some(text.clone()),
                Json::Null => {}
                _ => meta.unwrap_policy_invalid = true,
            },
            "target_delay_samples" => match value {
                Json::Num(number) => meta.target_delay_samples = Some(*number),
                Json::Null => {}
                _ => meta.target_delay_invalid = true,
            },
            "interpolation" => match value {
                Json::Str(text) => meta.interpolation = Some(text.clone()),
                Json::Null => {}
                _ => meta.interpolation_invalid = true,
            },
            "edge_policy" => match value {
                Json::Str(text) => meta.edge_policy = Some(text.clone()),
                Json::Null => {}
                _ => meta.edge_policy_invalid = true,
            },
            "source_hash" => match value {
                Json::Str(text) => meta.source_hash = Some(text.clone()),
                Json::Null => {}
                _ => meta.source_hash_invalid = true,
            },
            "source_file_sha256" => match value {
                Json::Str(text) => meta.source_file_sha256 = Some(text.clone()),
                Json::Null => {}
                _ => meta.source_file_sha256_invalid = true,
            },
            "note" => match value {
                Json::Str(text) => meta.note = Some(text.clone()),
                Json::Null => {}
                _ => meta.warnings.push(TargetWarning::new(
                    TargetWarningCode::NoteNotString,
                    "note 不是字符串，已忽略",
                )),
            },
            "points" => meta.has_points_field = true,
            _ => unreachable!("KNOWN_TOP_LEVEL_FIELDS 与 match 分支必须同步"),
        }
    }
    meta
}

struct JsonPoints {
    points: Vec<RawPoint>,
    field_errors: Vec<String>,
    x_field_warnings: Vec<String>,
    not_array: bool,
}

fn parse_json_points(value: &Json) -> JsonPoints {
    let mut result = JsonPoints {
        points: Vec::new(),
        field_errors: Vec::new(),
        x_field_warnings: Vec::new(),
        not_array: false,
    };
    let items = match value.as_array() {
        Some(items) => items,
        None => {
            result.not_array = true;
            return result;
        }
    };
    for (index, item) in items.iter().enumerate() {
        let fields = match item.as_obj() {
            Some(fields) => fields,
            None => {
                result
                    .field_errors
                    .push(format!("points[{index}] 不是对象"));
                continue;
            }
        };
        let mut point = RawPoint::default();
        for (key, value) in fields {
            if key.starts_with("x_") {
                result
                    .x_field_warnings
                    .push(format!("points[{index}].{key}"));
                continue;
            }
            match key.as_str() {
                "freq_hz" => match value {
                    Json::Num(number) => point.freq_hz = Some(*number),
                    Json::Null => {}
                    _ => point.freq_invalid_type = true,
                },
                "mag_db" => match value {
                    Json::Num(number) => point.mag_db = Some(*number),
                    Json::Null => {}
                    _ => point.mag_invalid_type = true,
                },
                "phase_deg" => {
                    point.has_phase_key = true;
                    match value {
                        Json::Num(number) => point.phase_deg = Some(*number),
                        Json::Null => {}
                        _ => point.phase_invalid_type = true,
                    }
                }
                _ => result.field_errors.push(format!("points[{index}].{key}")),
            }
        }
        result.points.push(point);
    }
    result
}

// ---------------------------------------------------------------------------
// 文档中间表示
// ---------------------------------------------------------------------------

struct ParsedDocument {
    form: TargetForm,
    meta: RawMeta,
    points: Vec<RawPoint>,
    /// CSV 解析错误延迟到 §6.9.1 stage 6 抛出（schema/missing/unknown/unit/rate 优先）。
    csv_error: Option<TargetError>,
    /// CSV 头声明的 has_phase（None = 由点内 phase 键推断）。
    csv_has_phase: Option<bool>,
    /// 原始文件字节（CSV 形式 = CSV 字节；JSON 形式 = JSON 字节；builtin 为空）。
    raw_file_bytes: Vec<u8>,
}

// ---------------------------------------------------------------------------
// 插值 / 端点 / 网格统计 / 预算 辅助
// ---------------------------------------------------------------------------

fn max_adjacent_abs_delta(values: &[f64]) -> f64 {
    let mut max = 0.0f64;
    for pair in values.windows(2) {
        let delta = (pair[1] - pair[0]).abs();
        if delta > max {
            max = delta;
        }
    }
    max
}

/// auto 展开：把每个相位按最小跳变 ±360k 折到与上一点最近的值；返回 (展开值, wrap 次数)。
fn unwrap_phase_auto(raw_phases: &[f64]) -> (Vec<f64>, usize) {
    let mut out = Vec::with_capacity(raw_phases.len());
    let mut wraps = 0usize;
    out.push(raw_phases[0]);
    for index in 1..raw_phases.len() {
        let previous = out[index - 1];
        let delta = raw_phases[index] - previous;
        let k = (delta / 360.0).round();
        let adjusted = raw_phases[index] - 360.0 * k;
        if k != 0.0 {
            wraps += 1;
        }
        out.push(adjusted);
    }
    (out, wraps)
}

/// log10(f) 线性插值（§6.7）；返回 (值, 来源)。
fn interpolate_log_f(
    freqs: &[f64],
    values: &[f64],
    freq_hz: f64,
    edge_policy: EdgePolicy,
) -> Result<(f64, CoverageSource), TargetError> {
    debug_assert_eq!(freqs.len(), values.len());
    if !freq_hz.is_finite() || freq_hz <= 0.0 {
        return Err(err_field(
            TargetErrorCode::FreqInvalid,
            "sample.freq_hz",
            format!("采样频率必须为正有限数，实际 {freq_hz}"),
        ));
    }
    let last = freqs.len() - 1;
    if freq_hz < freqs[0] {
        return match edge_policy {
            EdgePolicy::Hold => Ok((values[0], CoverageSource::HeldLow)),
            EdgePolicy::Reject => Err(err_field(
                TargetErrorCode::Edge,
                "sample.freq_hz",
                format!(
                    "采样频率 {freq_hz} Hz 低于网格下界 {} Hz 且 edge_policy=reject",
                    freqs[0]
                ),
            )),
        };
    }
    if freq_hz > freqs[last] {
        return match edge_policy {
            EdgePolicy::Hold => Ok((values[last], CoverageSource::HeldHigh)),
            EdgePolicy::Reject => Err(err_field(
                TargetErrorCode::Edge,
                "sample.freq_hz",
                format!(
                    "采样频率 {freq_hz} Hz 高于网格上界 {} Hz 且 edge_policy=reject",
                    freqs[last]
                ),
            )),
        };
    }
    if freq_hz == freqs[0] {
        return Ok((values[0], CoverageSource::GridPoint(0)));
    }
    if freq_hz == freqs[last] {
        return Ok((values[last], CoverageSource::GridPoint(last)));
    }
    let mut low = 0usize;
    let mut high = last;
    while high - low > 1 {
        let mid = (low + high) / 2;
        if freqs[mid] <= freq_hz {
            low = mid;
        } else {
            high = mid;
        }
    }
    if freq_hz == freqs[low] {
        return Ok((values[low], CoverageSource::GridPoint(low)));
    }
    let t = (freq_hz.log10() - freqs[low].log10()) / (freqs[high].log10() - freqs[low].log10());
    Ok((
        values[low] + t * (values[high] - values[low]),
        CoverageSource::Interpolated,
    ))
}

fn coverage_value(
    freqs: &[f64],
    values: &[f64],
    freq_hz: f64,
    edge_policy: EdgePolicy,
) -> CoverageValue {
    match interpolate_log_f(freqs, values, freq_hz, edge_policy) {
        Ok((value, source)) => CoverageValue {
            value: Some(value),
            source,
        },
        Err(_) => CoverageValue {
            value: None,
            source: CoverageSource::OutsideGrid,
        },
    }
}

fn grid_stats(freqs: &[f64]) -> (f64, f64, f64, f64) {
    let mut min_interval = f64::INFINITY;
    let mut max_interval = 0.0f64;
    let mut min_octave = f64::INFINITY;
    let mut max_octave = 0.0f64;
    for pair in freqs.windows(2) {
        let interval = pair[1] - pair[0];
        min_interval = min_interval.min(interval);
        max_interval = max_interval.max(interval);
        let octave = (pair[1] / pair[0]).log2();
        min_octave = min_octave.min(octave);
        max_octave = max_octave.max(octave);
    }
    (min_interval, max_interval, min_octave, max_octave)
}

fn push_grid_resolution_warnings(freqs: &[f64], warnings: &mut Vec<TargetWarning>) {
    let mut low_warn = 0usize;
    let mut high_warn = 0usize;
    let mut low_max = 0.0f64;
    let mut high_max = 0.0f64;
    for pair in freqs.windows(2) {
        let octave = (pair[1] / pair[0]).log2();
        let mid = (pair[0] * pair[1]).sqrt();
        if mid <= GRID_LOW_BAND_HZ {
            if octave > GRID_MAX_OCTAVE_STEP_LOW + 1e-12 {
                low_warn += 1;
                low_max = low_max.max(octave);
            }
        } else if octave > GRID_MAX_OCTAVE_STEP_MID + 1e-12 {
            high_warn += 1;
            high_max = high_max.max(octave);
        }
    }
    if low_warn > 0 {
        warnings.push(TargetWarning::new(
            TargetWarningCode::GridResolutionLow,
            format!(
                "{low_warn} 个 <={GRID_LOW_BAND_HZ} Hz 相邻点步长粗于 1/48 oct（最大 {low_max:.6} oct）；建议值，不拒收"
            ),
        ));
    }
    if high_warn > 0 {
        warnings.push(TargetWarning::new(
            TargetWarningCode::GridResolutionHigh,
            format!(
                "{high_warn} 个 >{GRID_LOW_BAND_HZ} Hz 相邻点步长粗于 1/12 oct（最大 {high_max:.6} oct）；建议值，不拒收"
            ),
        ));
    }
}

fn check_budget_inner(
    freqs: &[f64],
    target_delay_samples: Option<u32>,
    budget: &TargetBudget,
) -> Result<BudgetReport, TargetError> {
    if let Some(max_points) = budget.max_points {
        if freqs.len() > max_points {
            return Err(err_field(
                TargetErrorCode::Budget,
                "points",
                format!("点数 {} 超过预算 {max_points}", freqs.len()),
            ));
        }
    }
    let mut notes = Vec::new();
    let mut required_delay_samples = None;
    if let Some(delay) = target_delay_samples {
        required_delay_samples = Some(delay as f64);
        if let Some(delay_budget) = budget.delay_budget_samples {
            if delay as f64 > delay_budget {
                return Err(err_field(
                    TargetErrorCode::Budget,
                    "target_delay_samples",
                    format!(
                        "delay_removed 目标 D_target={delay} sample 超过延迟预算 {delay_budget} sample（INCOMPLETE，不得静默截断）"
                    ),
                ));
            }
        } else {
            notes.push("delay 预算未声明（delay_budget_samples=None），未做延迟准入".to_string());
        }
    } else {
        notes.push(
            "phase_ref!=delay_removed：目标没有整数 D_target，延迟准入由设计器（T55/T56）联合判定"
                .to_string(),
        );
    }
    if let Some(max_taps) = budget.max_taps {
        if max_taps < 8 {
            return Err(err_field(
                TargetErrorCode::Budget,
                "options.budget.max_taps",
                format!("max_taps={max_taps} 小于最小 FIR 长度 8，无法承载目标"),
            ));
        }
        notes.push(format!(
            "taps 预算 {max_taps}：目标本身不直接决定 taps，频响逼近可行性由设计器判定（INCOMPLETE 时不得静默截断）"
        ));
    }
    Ok(BudgetReport {
        point_count: freqs.len(),
        required_delay_samples,
        notes,
    })
}

// ---------------------------------------------------------------------------
// 导入管线（§6.9.1 唯一码优先级；逐 stage 命中即返回）
// ---------------------------------------------------------------------------

fn import_document(
    document: ParsedDocument,
    options: &ImportOptions,
) -> Result<TargetCurve, TargetError> {
    options.validate()?;
    let form = document.form;
    let raw = document.meta;
    let fs = options.profile_rate_hz;

    // ---- stage 1: schema ----
    if raw.schema_invalid || raw.schema.as_deref() != Some(TARGET_SCHEMA) {
        let detail = match (&raw.schema, raw.schema_invalid) {
            (Some(value), _) => format!("schema 必须是 {TARGET_SCHEMA}，实际 \"{value}\""),
            (None, true) => "schema 不是字符串".to_string(),
            (None, false) => "schema 缺失".to_string(),
        };
        return Err(err_field(TargetErrorCode::Schema, "schema", detail));
    }

    // ---- stage 2: 必填字段缺失（phase 专属字段按 §6.3/§6.9.1 在 stage 16/17 处理）----
    macro_rules! require_string {
        ($field:expr, $value:expr, $invalid:expr) => {{
            if $value.is_none() {
                let detail = if $invalid {
                    format!("必填字段 {} 缺失或不是字符串", $field)
                } else {
                    format!("必填字段 {} 缺失", $field)
                };
                return Err(err_field(TargetErrorCode::MissingField, $field, detail));
            }
        }};
    }
    require_string!(
        "normalization",
        raw.normalization.as_ref(),
        raw.normalization_invalid
    );
    require_string!(
        "interpolation",
        raw.interpolation.as_ref(),
        raw.interpolation_invalid
    );
    require_string!(
        "edge_policy",
        raw.edge_policy.as_ref(),
        raw.edge_policy_invalid
    );
    if form == TargetForm::Json && !raw.has_points_field {
        return Err(err_field(
            TargetErrorCode::MissingField,
            "points",
            "JSON 形式缺少 points 数组",
        ));
    }

    // ---- stage 3: 顶层未知字段（x_ 前缀为 warning）----
    if let Some(field) = raw.unknown_fields.first() {
        return Err(err_field(
            TargetErrorCode::FieldUnknown,
            field,
            format!(
                "顶层未知字段 {}（扩展字段请用 x_ 前缀；未知语义必须拒收）",
                field
            ),
        ));
    }

    // ---- stage 4: 单位 ----
    let check_unit = |field: &str, value: &Option<String>, invalid: bool, expected: &str| {
        if invalid {
            return Err(err_field(
                TargetErrorCode::Unit,
                field,
                format!("{field} 不是字符串（只允许 {expected}）"),
            ));
        }
        match value.as_deref() {
            Some(actual) if actual == expected => Ok(()),
            Some(actual) => Err(err_field(
                TargetErrorCode::Unit,
                field,
                format!("{field} 只允许 {expected}，实际 \"{actual}\"（单位不明不猜）"),
            )),
            None => Err(err_field(
                TargetErrorCode::Unit,
                field,
                format!("{field} 必填且只允许 {expected}（缺失不得默认/猜测）"),
            )),
        }
    };
    check_unit(
        "frequency_unit",
        &raw.frequency_unit,
        raw.frequency_unit_invalid,
        "Hz",
    )?;
    check_unit(
        "magnitude_unit",
        &raw.magnitude_unit,
        raw.magnitude_unit_invalid,
        "dB",
    )?;
    if raw.phase_unit_invalid {
        return Err(err_field(
            TargetErrorCode::Unit,
            "phase_unit",
            "phase_unit 不是字符串（只允许 deg）",
        ));
    }
    if let Some(actual) = raw.phase_unit.as_deref() {
        if actual != "deg" {
            return Err(err_field(
                TargetErrorCode::Unit,
                "phase_unit",
                format!("phase_unit 只允许 deg，实际 \"{actual}\"（单位不明不猜）"),
            ));
        }
    }

    // ---- stage 5: runtime_rate ----
    let runtime_rate = match raw.runtime_rate {
        Some(value) => value,
        None => {
            return Err(err_field(
                TargetErrorCode::Rate,
                "runtime_rate",
                if raw.runtime_rate_invalid {
                    "runtime_rate 缺失或不是数字"
                } else {
                    "runtime_rate 缺失"
                },
            ))
        }
    };
    if !runtime_rate.is_finite() || runtime_rate <= 0.0 || runtime_rate != fs {
        return Err(err_field(
            TargetErrorCode::Rate,
            "runtime_rate",
            format!(
                "runtime_rate 非法或与 profile fs 不一致：runtime_rate={runtime_rate}, profile fs={fs}"
            ),
        ));
    }
    let half_fs = fs / 2.0;

    // ---- stage 6: CSV 格式（延迟错误；优先于 stage 7+）----
    if let Some(error) = document.csv_error {
        return Err(error);
    }

    // ---- stage 7: sidecar 混用 ----
    if form == TargetForm::Csv && raw.has_points_field {
        return Err(err_field(
            TargetErrorCode::SidecarMix,
            "points",
            "CSV 形式的 sidecar 不得携带 points（CSV 表与 JSON points 不得混用）",
        ));
    }

    // stage 8（points 内未知字段）由 JSON 导入入口在构造文档前按 §6.9.1 先行检查；
    // CSV 形式不存在点内未知字段。

    // ---- stage 9: 点数 ----
    let point_count = document.points.len();
    if !(MIN_POINTS..=MAX_POINTS).contains(&point_count) {
        return Err(err_field(
            TargetErrorCode::Points,
            "points",
            format!("点数必须在 {MIN_POINTS}..={MAX_POINTS}，实际 {point_count}"),
        ));
    }

    // ---- stage 10: freq <= 0 / 非有限 / 缺失 ----
    for (index, point) in document.points.iter().enumerate() {
        let invalid = point.freq_invalid_type
            || match point.freq_hz {
                Some(freq) => !freq.is_finite() || freq <= 0.0,
                None => true,
            };
        if invalid {
            let detail = match point.freq_hz {
                Some(freq) => format!("freq_hz 必须为正有限数，实际 {freq}"),
                None => "freq_hz 缺失或类型非法".to_string(),
            };
            return Err(err_field(
                TargetErrorCode::FreqInvalid,
                format!("points[{index}].freq_hz"),
                detail,
            ));
        }
    }

    // ---- stage 11: 严格递增/唯一 ----
    for index in 1..document.points.len() {
        let previous = document.points[index - 1].freq_hz.expect("stage 10 已确认");
        let current = document.points[index].freq_hz.expect("stage 10 已确认");
        if current <= previous {
            return Err(err_field(
                TargetErrorCode::FreqOrder,
                format!("points[{index}].freq_hz"),
                format!(
                    "freq 必须严格递增且唯一：points[{}].freq_hz={previous}，points[{index}].freq_hz={current}",
                    index - 1
                ),
            ));
        }
    }

    // ---- stage 12: freq > fs/2 ----
    for (index, point) in document.points.iter().enumerate() {
        let freq = point.freq_hz.expect("stage 10 已确认");
        if freq > half_fs {
            return Err(err_field(
                TargetErrorCode::FreqRange,
                format!("points[{index}].freq_hz"),
                format!("freq_hz={freq} 超过 fs/2={half_fs}"),
            ));
        }
    }

    // ---- stage 13: mag/phase NaN/Inf（含 JSON 非数值类型）----
    for (index, point) in document.points.iter().enumerate() {
        if point.mag_invalid_type || point.phase_invalid_type {
            return Err(err_field(
                TargetErrorCode::Nan,
                format!("points[{index}]"),
                "mag_db/phase_deg 必须是有限数值（JSON 非数值类型无法表示有限数）",
            ));
        }
        if let Some(mag) = point.mag_db {
            if !mag.is_finite() {
                return Err(err_field(
                    TargetErrorCode::Nan,
                    format!("points[{index}].mag_db"),
                    format!("mag_db 非有限: {mag}"),
                ));
            }
        }
        if let Some(phase) = point.phase_deg {
            if !phase.is_finite() {
                return Err(err_field(
                    TargetErrorCode::Nan,
                    format!("points[{index}].phase_deg"),
                    format!("phase_deg 非有限: {phase}"),
                ));
            }
        }
    }

    // ---- stage 14: 缺 amp ----
    for (index, point) in document.points.iter().enumerate() {
        if point.mag_db.is_none() {
            return Err(err_field(
                TargetErrorCode::MagMissing,
                format!("points[{index}].mag_db"),
                "每点必须有 mag_db",
            ));
        }
    }

    // ---- stage 15: phase 混合 ----
    let declared_has_phase = match document.csv_has_phase {
        Some(has_phase) => has_phase,
        None => document
            .points
            .iter()
            .any(|point| point.has_phase_key || point.phase_deg.is_some()),
    };
    if declared_has_phase {
        let all_values = document
            .points
            .iter()
            .all(|point| point.phase_deg.is_some());
        let all_keys = document
            .points
            .iter()
            .all(|point| point.has_phase_key || point.phase_deg.is_some());
        if !all_values || !all_keys {
            return Err(err_field(
                TargetErrorCode::PhaseMixed,
                "points[].phase_deg",
                "phase 列必须全有或全无（不得部分点有 phase）",
            ));
        }
    }
    let has_phase = declared_has_phase;

    // ---- stage 16: phase_unit 缺失/多余 ----
    if has_phase && raw.phase_unit.is_none() {
        return Err(err_field(
            TargetErrorCode::PhaseUnit,
            "phase_unit",
            "有 phase 时 phase_unit 必填且只允许 deg",
        ));
    }
    if !has_phase && raw.phase_unit.is_some() {
        return Err(err_field(
            TargetErrorCode::PhaseUnit,
            "phase_unit",
            "无 phase 时不得给 phase_unit（必须缺省）",
        ));
    }

    // ---- stage 17: phase_ref / delay_removed 的 target_delay_samples 语义 ----
    let phase_ref = if raw.phase_ref_invalid {
        return Err(err_field(
            TargetErrorCode::PhaseRef,
            "phase_ref",
            "phase_ref 不是字符串",
        ));
    } else {
        match raw.phase_ref.as_deref() {
            Some(text) => PhaseRef::from_id(text).ok_or_else(|| {
                err_field(
                    TargetErrorCode::PhaseRef,
                    "phase_ref",
                    format!("phase_ref 非法：\"{text}\"（只允许 none|absolute|delay_removed）"),
                )
            })?,
            None => {
                if has_phase {
                    return Err(err_field(
                        TargetErrorCode::PhaseRef,
                        "phase_ref",
                        "有 phase 时 phase_ref 必填（none|absolute|delay_removed）",
                    ));
                }
                PhaseRef::None
            }
        }
    };
    if has_phase && !phase_ref.has_phase() {
        return Err(err_field(
            TargetErrorCode::PhaseRef,
            "phase_ref",
            "有 phase 列却声明 phase_ref=none（语义自相矛盾；缺 phase 应删 phase 列）",
        ));
    }
    if !has_phase && phase_ref.has_phase() {
        return Err(err_field(
            TargetErrorCode::PhaseRef,
            "phase_ref",
            format!(
                "无 phase 列却声明 phase_ref={}（需要 phase 列）",
                phase_ref.as_str()
            ),
        ));
    }
    if phase_ref == PhaseRef::DelayRemoved && raw.target_delay_samples.is_none() {
        return Err(err_field(
            TargetErrorCode::PhaseRef,
            "target_delay_samples",
            "phase_ref=delay_removed 时 target_delay_samples 必填（整数 sample）",
        ));
    }

    // ---- stage 18: unwrap_policy + phase 跳变 ----
    let mut unwrap_report: Option<UnwrapReport> = None;
    let mut phase_ex_unwrapped: Option<Vec<f64>> = None;
    if has_phase {
        let policy = if raw.unwrap_policy_invalid {
            return Err(err_field(
                TargetErrorCode::MissingField,
                "unwrap_policy",
                "unwrap_policy 缺失或不是字符串（有 phase 时必填：unwrapped|auto）",
            ));
        } else {
            match raw.unwrap_policy.as_deref() {
                Some(text) => UnwrapPolicy::from_id(text).ok_or_else(|| {
                    err_field(
                        TargetErrorCode::MissingField,
                        "unwrap_policy",
                        format!("unwrap_policy 非法：\"{text}\"（只允许 unwrapped|auto）"),
                    )
                })?,
                None => {
                    return Err(err_field(
                        TargetErrorCode::MissingField,
                        "unwrap_policy",
                        "有 phase 时 unwrap_policy 必填（unwrapped|auto）",
                    ))
                }
            }
        };
        let raw_phases: Vec<f64> = document
            .points
            .iter()
            .map(|point| point.phase_deg.expect("stage 15 已确认全有"))
            .collect();
        let raw_max_delta = max_adjacent_abs_delta(&raw_phases);
        let (unwrapped, wraps) = match policy {
            UnwrapPolicy::Unwrapped => {
                if raw_max_delta > 180.0 + PHASE_JUMP_EPS_DEG {
                    return Err(err_field(
                        TargetErrorCode::PhaseJump,
                        "points[].phase_deg",
                        format!(
                            "unwrap_policy=unwrapped 但相邻 |Δφ| 最大值 {raw_max_delta:.6}° > 180°；请用 auto 或先展开"
                        ),
                    ));
                }
                (raw_phases.clone(), 0usize)
            }
            UnwrapPolicy::Auto => unwrap_phase_auto(&raw_phases),
        };
        let max_delta = max_adjacent_abs_delta(&unwrapped);
        if max_delta > 180.0 + PHASE_JUMP_EPS_DEG {
            return Err(err_field(
                TargetErrorCode::PhaseJump,
                "points[].phase_deg",
                format!("auto 展开后相邻 |Δφ| 最大值 {max_delta:.6}° 仍 > 180°"),
            ));
        }
        unwrap_report = Some(UnwrapReport {
            policy,
            wraps_applied: wraps,
            max_abs_delta_deg: max_delta,
            raw_max_abs_delta_deg: raw_max_delta,
        });
        phase_ex_unwrapped = Some(unwrapped);
    }

    // ---- stage 19: 带 phase 的 freq = fs/2（Nyquist 相位退化）----
    // F-23(a)：这是 E_TGT_PHASE_REAL 唯一静态可判子条件——实系数 FIR 在 fs/2 的响应
    // H(fs/2)=Σh[n]·(−1)^n 必为实数，相位只能是 0°/180°；Lead 裁决把该频点任何 phase 目标整体拒收。
    // F-23(b) 的「任意相位可否由实系数因果 FIR 表示」静态层不可判（依赖 taps/D/预算），
    // 由 T37/T55 设计期判定，不在本模块猜测。
    if has_phase {
        for (index, point) in document.points.iter().enumerate() {
            let freq = point.freq_hz.expect("stage 10 已确认");
            if freq == half_fs {
                return Err(err_field(
                    TargetErrorCode::PhaseReal,
                    format!("points[{index}].freq_hz"),
                    format!(
                        "带 phase 的目标点不得等于 fs/2={half_fs}（Nyquist 相位退化：实系数 FIR 在 fs/2 的响应必为实数、相位只能 0°/180°，故 phase 目标整体拒收；magnitude-only 允许 = fs/2；E_TGT_PHASE_REAL 静态可判子条件，F-23）"
                    ),
                ));
            }
        }
    }

    // ---- stage 20: 族相位接受性（§6.10）----
    let mut phase_target = match phase_ref {
        PhaseRef::None => PhaseTargetClass::None,
        PhaseRef::Absolute => PhaseTargetClass::Absolute,
        PhaseRef::DelayRemoved => PhaseTargetClass::DelayRemoved,
    };
    if let Some(family) = options.family {
        if family.forbids_phase() && has_phase {
            if options.allow_phase_ignored {
                phase_target = PhaseTargetClass::Ignored;
            } else {
                return Err(err_field(
                    TargetErrorCode::PhaseUnsupported,
                    "phase_ref",
                    format!(
                        "族 {} 不接受 phase 目标（phase_ref={}）；建议 corrected/lowdelay，或调用方显式 allow_phase_ignored 并在报告写 phase_target=ignored",
                        family.as_str(),
                        phase_ref.as_str()
                    ),
                ));
            }
        }
        if family.requires_phase() && !has_phase {
            return Err(err_field(
                TargetErrorCode::PhaseUnsupported,
                "phase_ref",
                format!(
                    "族 {} 必须收到相位目标（absolute/delay_removed），当前目标缺 phase",
                    family.as_str()
                ),
            ));
        }
    }

    // ---- stage 21: target_delay_samples 合法性 ----
    let mut target_delay_samples: Option<u32> = None;
    if raw.target_delay_invalid {
        return Err(err_field(
            TargetErrorCode::Delay,
            "target_delay_samples",
            "target_delay_samples 不是数字",
        ));
    }
    if let Some(delay) = raw.target_delay_samples {
        if phase_ref != PhaseRef::DelayRemoved {
            return Err(err_field(
                TargetErrorCode::Delay,
                "target_delay_samples",
                format!(
                    "phase_ref={} 时 target_delay_samples 必须缺省（只允许 delay_removed）",
                    phase_ref.as_str()
                ),
            ));
        }
        if !delay.is_finite() || delay < 0.0 || delay.fract() != 0.0 {
            return Err(err_field(
                TargetErrorCode::Delay,
                "target_delay_samples",
                format!("target_delay_samples 必须是 >=0 的整数 sample，实际 {delay}"),
            ));
        }
        let max_delay = TARGET_DELAY_MAX_MS * fs / 1000.0;
        if delay > max_delay {
            return Err(err_field(
                TargetErrorCode::Delay,
                "target_delay_samples",
                format!("target_delay_samples={delay} 超过 200 ms 上界 {max_delay}"),
            ));
        }
        target_delay_samples = Some(delay as u32);
    }

    // ---- stage 22: interpolation ----
    let interpolation = if raw.interpolation_invalid {
        return Err(err_field(
            TargetErrorCode::Interp,
            "interpolation",
            "interpolation 不是字符串",
        ));
    } else {
        match raw.interpolation.as_deref() {
            Some(text) => Interpolation::from_id(text).ok_or_else(|| {
                err_field(
                    TargetErrorCode::Interp,
                    "interpolation",
                    format!("interpolation 只允许 log_frequency_linear，实际 \"{text}\""),
                )
            })?,
            None => unreachable!("stage 2 已确认 interpolation 存在"),
        }
    };

    // ---- stage 23: edge_policy ----
    let edge_policy = if raw.edge_policy_invalid {
        return Err(err_field(
            TargetErrorCode::Edge,
            "edge_policy",
            "edge_policy 不是字符串",
        ));
    } else {
        match raw.edge_policy.as_deref() {
            Some(text) => EdgePolicy::from_id(text).ok_or_else(|| {
                err_field(
                    TargetErrorCode::Edge,
                    "edge_policy",
                    format!("edge_policy 只允许 hold|reject，实际 \"{text}\""),
                )
            })?,
            None => unreachable!("stage 2 已确认 edge_policy 存在"),
        }
    };

    // ---- stage 24: normalization ----
    let normalization = if raw.normalization_invalid {
        return Err(err_field(
            TargetErrorCode::Norm,
            "normalization",
            "normalization 不是字符串",
        ));
    } else {
        match raw.normalization.as_deref() {
            Some(text) => Normalization::from_id(text).ok_or_else(|| {
                err_field(
                    TargetErrorCode::Norm,
                    "normalization",
                    format!("normalization 只允许 none|ref_1k|peak，实际 \"{text}\""),
                )
            })?,
            None => unreachable!("stage 2 已确认 normalization 存在"),
        }
    };

    // ---- stage 25: reference_gain_db ----
    let reference_gain_db = if raw.reference_gain_invalid {
        return Err(err_field(
            TargetErrorCode::GainRange,
            "reference_gain_db",
            "reference_gain_db 不是数字",
        ));
    } else {
        raw.reference_gain_db.unwrap_or(0.0)
    };
    if !reference_gain_db.is_finite()
        || !(REFERENCE_GAIN_MIN_DB..=REFERENCE_GAIN_MAX_DB).contains(&reference_gain_db)
    {
        return Err(err_field(
            TargetErrorCode::GainRange,
            "reference_gain_db",
            format!(
                "reference_gain_db 必须在 [{REFERENCE_GAIN_MIN_DB}, {REFERENCE_GAIN_MAX_DB}] dB，实际 {reference_gain_db}"
            ),
        ));
    }

    // ---- 构造点（此时所有点级检查已通过）----
    let points: Vec<TargetPoint> = document
        .points
        .iter()
        .map(|point| TargetPoint {
            freq_hz: point.freq_hz.expect("stage 10 已确认"),
            mag_db: point.mag_db.expect("stage 14 已确认"),
            phase_deg: point.phase_deg,
        })
        .collect();
    let freqs: Vec<f64> = points.iter().map(|point| point.freq_hz).collect();
    let raw_mags: Vec<f64> = points.iter().map(|point| point.mag_db).collect();

    // ---- 归一化平移（§6.5；在 stage 23 覆盖检查之前先算，offset 需要 1 kHz 插值）----
    let normalization_offset_db = match normalization {
        Normalization::None => 0.0,
        Normalization::Ref1k => {
            let (value, _) = interpolate_log_f(&freqs, &raw_mags, REFERENCE_FREQ_HZ, edge_policy)?;
            -value
        }
        Normalization::Peak => {
            let max = raw_mags.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            -max
        }
    };
    let mag_normalized_db: Vec<f64> = raw_mags
        .iter()
        .map(|mag| mag + normalization_offset_db)
        .collect();
    let mag_final_db: Vec<f64> = mag_normalized_db
        .iter()
        .map(|mag| mag + reference_gain_db)
        .collect();

    // ---- stage 23 后半：edge=reject 的 20 Hz/20 kHz 覆盖 ----
    let covers_20_20k = freqs[0] <= COVERAGE_LO_HZ && freqs[freqs.len() - 1] >= COVERAGE_HI_HZ;
    if edge_policy == EdgePolicy::Reject && options.require_20_20k_coverage && !covers_20_20k {
        return Err(err_field(
            TargetErrorCode::Edge,
            "edge_policy",
            format!(
                "edge_policy=reject 且设计要求覆盖 20 Hz–20 kHz，但网格仅覆盖 {} Hz–{} Hz",
                freqs[0],
                freqs[freqs.len() - 1]
            ),
        ));
    }

    // ---- stage 26: boost（§6.5）----
    let (mag_at_1k_shape, _) =
        interpolate_log_f(&freqs, &mag_normalized_db, REFERENCE_FREQ_HZ, edge_policy)?;
    let max_shape = mag_normalized_db
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    let max_boost_db = max_shape - mag_at_1k_shape;
    let boost_warning = max_boost_db > BOOST_WARN_DB;
    if max_boost_db > BOOST_HARD_LIMIT_DB {
        return Err(err_field(
            TargetErrorCode::Boost,
            "points[].mag_db",
            format!(
                "max_boost_db={max_boost_db:.6} dB 超过硬上限 +{BOOST_HARD_LIMIT_DB} dB（不得静默抬低频）"
            ),
        ));
    }
    if max_boost_db > BOOST_REJECT_DB && !options.allow_boost {
        return Err(err_field(
            TargetErrorCode::Boost,
            "points[].mag_db",
            format!(
                "max_boost_db={max_boost_db:.6} dB > +{BOOST_REJECT_DB} dB 且未显式 allow_boost（硬上限 +{BOOST_HARD_LIMIT_DB}）"
            ),
        ));
    }

    // ---- 相位 total（delay_removed：φ_tot = φ_ex − 360·f·D/fs）----
    let phase_total_deg: Option<Vec<f64>> = match (&phase_ex_unwrapped, phase_ref) {
        (Some(phases), PhaseRef::DelayRemoved) => {
            let delay = target_delay_samples.expect("stage 17 已确认") as f64;
            Some(
                phases
                    .iter()
                    .zip(freqs.iter())
                    .map(|(phase, freq)| phase - 360.0 * freq * delay / fs)
                    .collect(),
            )
        }
        (Some(phases), _) => Some(phases.clone()),
        (None, _) => None,
    };

    // ---- stage 27: source_hash / source_file_sha256（§6.3）----
    let canonical_points_sha256 = canonical_points_sha256(&points)?;
    let raw_file_sha256 = sha256_hex(&document.raw_file_bytes);
    let mut source_hash_verified = false;
    if raw.source_hash_invalid {
        return Err(err_field(
            TargetErrorCode::Hash,
            "source_hash",
            "source_hash 不是字符串",
        ));
    }
    if raw.source_file_sha256_invalid {
        return Err(err_field(
            TargetErrorCode::Hash,
            "source_file_sha256",
            "source_file_sha256 不是字符串",
        ));
    }
    match form {
        TargetForm::Csv => {
            let declared = raw.source_hash.as_deref().ok_or_else(|| {
                err_field(
                    TargetErrorCode::Hash,
                    "source_hash",
                    "CSV 形式 source_hash 必填（sidecar 中给出 = CSV 文件字节 sha256_hex）",
                )
            })?;
            if !is_sha256_hex(declared) {
                return Err(err_field(
                    TargetErrorCode::Hash,
                    "source_hash",
                    format!("source_hash 必须是 64 位 hex，实际 \"{declared}\""),
                ));
            }
            if !hex_eq_ignore_case(declared, &raw_file_sha256) {
                return Err(err_field(
                    TargetErrorCode::Hash,
                    "source_hash",
                    format!(
                        "source_hash 与 CSV 字节 sha256 不符：声明 {declared}，实际 {raw_file_sha256}"
                    ),
                ));
            }
            source_hash_verified = true;
        }
        TargetForm::Json => {
            if let Some(declared) = raw.source_hash.as_deref() {
                if !is_sha256_hex(declared) {
                    return Err(err_field(
                        TargetErrorCode::Hash,
                        "source_hash",
                        format!("source_hash 必须是 64 位 hex，实际 \"{declared}\""),
                    ));
                }
                if !hex_eq_ignore_case(declared, &canonical_points_sha256) {
                    return Err(err_field(
                        TargetErrorCode::Hash,
                        "source_hash",
                        format!(
                            "source_hash 与规范点序列 sha256 不符：声明 {declared}，实际 {canonical_points_sha256}"
                        ),
                    ));
                }
                source_hash_verified = true;
            }
        }
        TargetForm::Builtin => {}
    }
    let mut source_file_sha256_verified = false;
    let mut source_file_sha256_unverifiable = false;
    if let Some(declared) = raw.source_file_sha256.as_deref() {
        if !is_sha256_hex(declared) {
            return Err(err_field(
                TargetErrorCode::Hash,
                "source_file_sha256",
                format!("source_file_sha256 必须是 64 位 hex，实际 \"{declared}\""),
            ));
        }
        match form {
            TargetForm::Csv => {
                if !hex_eq_ignore_case(declared, &raw_file_sha256) {
                    return Err(err_field(
                        TargetErrorCode::Hash,
                        "source_file_sha256",
                        format!(
                            "source_file_sha256 与 CSV 文件字节不符：声明 {declared}，实际 {raw_file_sha256}"
                        ),
                    ));
                }
                source_file_sha256_verified = true;
            }
            // JSON 单文件/Builtin 无法自引用校验自身字节（写入后哈希会变），只登记不冒充已验证。
            TargetForm::Json | TargetForm::Builtin => {
                source_file_sha256_unverifiable = true;
            }
        }
    }

    // ---- stage 28: 预算 ----
    if let Some(budget) = &options.budget {
        check_budget_inner(&freqs, target_delay_samples, budget)?;
    }

    // ---- 报告（§6.4/§6.5/§6.6/§6.7/§6.8）----
    let mut warnings = Vec::new();
    for field in &raw.x_fields {
        warnings.push(TargetWarning::new(
            TargetWarningCode::XFieldTopLevel,
            format!("顶层扩展字段 {field} 已忽略（不参与语义）"),
        ));
    }
    for field in &raw.duplicate_fields {
        warnings.push(TargetWarning::new(
            TargetWarningCode::DuplicateField,
            format!("顶层字段 {field} 重复出现，保留第一次出现的值"),
        ));
    }
    warnings.extend(raw.warnings.iter().cloned());
    if source_file_sha256_unverifiable {
        warnings.push(TargetWarning::new(
            TargetWarningCode::SourceFileHashNotVerifiable,
            "单文件 JSON/Builtin 的 source_file_sha256 无法自引用校验（写入后字节哈希变化）；只登记不冒充已验证",
        ));
    }
    if let Some(note) = raw.note.as_deref() {
        if note.chars().count() > 500 {
            warnings.push(TargetWarning::new(
                TargetWarningCode::NoteTooLong,
                format!(
                    "note 长度 {} 字符 > 500（warning，不影响语义）",
                    note.chars().count()
                ),
            ));
        }
    }
    if boost_warning {
        warnings.push(TargetWarning::new(
            TargetWarningCode::Boost,
            format!(
                "max_boost_db={max_boost_db:.6} dB > +{BOOST_WARN_DB} dB（warning；>+{BOOST_REJECT_DB} 需显式 allow_boost）"
            ),
        ));
    }
    if !has_phase && raw.unwrap_policy.is_some() {
        warnings.push(TargetWarning::new(
            TargetWarningCode::PhaseMetaIgnored,
            "无 phase 时 unwrap_policy 必须缺省；已忽略该字段（warning）",
        ));
    }
    let (min_interval_hz, max_interval_hz, min_octave, max_octave) = grid_stats(&freqs);
    push_grid_resolution_warnings(&freqs, &mut warnings);

    let coverage_at_20 = coverage_value(&freqs, &mag_final_db, COVERAGE_LO_HZ, edge_policy);
    let coverage_at_20k = coverage_value(&freqs, &mag_final_db, COVERAGE_HI_HZ, edge_policy);
    if edge_policy == EdgePolicy::Hold {
        if matches!(coverage_at_20.source, CoverageSource::HeldLow) {
            warnings.push(TargetWarning::new(
                TargetWarningCode::EdgeHeld,
                format!(
                    "20 Hz 低于网格下界 {} Hz，edge_policy=hold 取端点值",
                    freqs[0]
                ),
            ));
        }
        if matches!(coverage_at_20k.source, CoverageSource::HeldHigh) {
            warnings.push(TargetWarning::new(
                TargetWarningCode::EdgeHeld,
                format!(
                    "20 kHz 高于网格上界 {} Hz，edge_policy=hold 取端点值",
                    freqs[freqs.len() - 1]
                ),
            ));
        }
    }

    let mut notes = Vec::new();
    notes.push(format!(
        "tilt10 口径 = 20 Hz 0 dB → 20 kHz −10 dB（log-f），reference gain 独立；旧 −3 dB@16 kHz tilt 已 superseded（{SUPERSEDED_LEGACY_TILT_ID}）"
    ));
    if !has_phase {
        notes.push(
            "phase_target=none：调用方必须显式选择设计家族（不得把缺 phase 当零相位）".to_string(),
        );
    }
    if phase_ref == PhaseRef::Absolute {
        notes.push(
            "phase_ref=absolute：given phase 即期望总相位（含 −360·f·D/fs），不得再额外隐式叠加/加大延迟"
                .to_string(),
        );
    }
    if phase_ref == PhaseRef::DelayRemoved {
        notes.push(format!(
            "phase_ref=delay_removed：φ_tot(f)=φ_ex(f)−360·f·D/fs，D_target={} sample；报告 D_used 与残差（|D_used−D_target|<= {} sample）",
            target_delay_samples.unwrap_or(0),
            TARGET_DELAY_CONSISTENCY_SAMPLES
        ));
    }

    let report = TargetImportReport {
        form,
        point_count,
        has_phase,
        phase_target,
        phase_family_used: options.family,
        normalization,
        normalization_offset_db,
        reference_gain_db,
        applied_offset_db: normalization_offset_db + reference_gain_db,
        max_boost_db,
        boost_warning,
        allow_boost: options.allow_boost,
        min_freq_hz: freqs[0],
        max_freq_hz: freqs[freqs.len() - 1],
        min_interval_hz,
        max_interval_hz,
        min_octave_interval: min_octave,
        max_octave_interval: max_octave,
        covers_20_20k,
        at_20hz: coverage_at_20,
        at_20khz: coverage_at_20k,
        unwrap: unwrap_report,
        target_delay_samples,
        canonical_points_sha256: canonical_points_sha256.clone(),
        source_hash: raw.source_hash.clone(),
        source_hash_verified,
        source_file_sha256: raw.source_file_sha256.clone(),
        source_file_sha256_verified,
        canonical_points_hash_algo: CANONICAL_POINTS_HASH_ALGO,
        warnings,
        notes,
    };

    let meta = TargetMeta {
        schema: raw
            .schema
            .clone()
            .unwrap_or_else(|| TARGET_SCHEMA.to_string()),
        frequency_unit: raw
            .frequency_unit
            .clone()
            .unwrap_or_else(|| "Hz".to_string()),
        magnitude_unit: raw
            .magnitude_unit
            .clone()
            .unwrap_or_else(|| "dB".to_string()),
        phase_unit: raw.phase_unit.clone(),
        runtime_rate,
        normalization,
        reference_gain_db,
        phase_ref,
        unwrap_policy: if has_phase {
            raw.unwrap_policy.as_deref().and_then(UnwrapPolicy::from_id)
        } else {
            None
        },
        target_delay_samples,
        interpolation,
        edge_policy,
        source_hash: raw.source_hash.clone(),
        source_file_sha256: raw.source_file_sha256.clone(),
        note: raw.note.clone(),
    };

    Ok(TargetCurve {
        meta,
        points,
        mag_normalized_db,
        mag_final_db,
        phase_ex_unwrapped_deg: phase_ex_unwrapped,
        phase_total_deg,
        canonical_points_sha256,
        report,
    })
}

// ---------------------------------------------------------------------------
// 导入入口
// ---------------------------------------------------------------------------

/// 形式 A：CSV 字节 + sidecar JSON 文本导入。
pub fn import_csv_bytes(
    csv_bytes: &[u8],
    sidecar_json: &str,
    options: &ImportOptions,
) -> Result<TargetCurve, TargetError> {
    let json_bytes = sidecar_json.as_bytes();
    let parsed = JsonParser::parse_document(json_bytes).map_err(|error| {
        err_field(
            TargetErrorCode::Schema,
            format!("sidecar.byte{}", error.position),
            format!("sidecar JSON 解析失败：{}", error.message),
        )
    })?;
    let object = parsed.as_obj().ok_or_else(|| {
        err_field(
            TargetErrorCode::Schema,
            "sidecar",
            "sidecar 根必须是 JSON 对象",
        )
    })?;
    let meta = parse_raw_meta(object);
    let csv = parse_csv(csv_bytes);
    let (points, csv_has_phase, csv_error) = match csv {
        Ok(data) => (data.points, Some(data.has_phase), None),
        Err(error) => (Vec::new(), None, Some(error)),
    };
    let document = ParsedDocument {
        form: TargetForm::Csv,
        meta,
        points,
        csv_error,
        csv_has_phase,
        raw_file_bytes: csv_bytes.to_vec(),
    };
    import_document(document, options)
}

/// 形式 B：单 JSON 字节导入（`source_file_sha256` 按原始字节校验）。
pub fn import_json_bytes(
    json_bytes: &[u8],
    options: &ImportOptions,
) -> Result<TargetCurve, TargetError> {
    let parsed = JsonParser::parse_document(json_bytes).map_err(|error| {
        err_field(
            TargetErrorCode::Schema,
            format!("json.byte{}", error.position),
            format!("JSON 解析失败：{}", error.message),
        )
    })?;
    let object = parsed
        .as_obj()
        .ok_or_else(|| err_field(TargetErrorCode::Schema, "json", "JSON 根必须是对象"))?;
    let meta = parse_raw_meta(object);
    let mut points = Vec::new();
    let mut point_field_errors = Vec::new();
    let mut point_x_warnings = Vec::new();
    let mut not_array = false;
    if let Some((_, value)) = object.iter().find(|(key, _)| key == "points") {
        let parsed_points = parse_json_points(value);
        points = parsed_points.points;
        point_field_errors = parsed_points.field_errors;
        point_x_warnings = parsed_points.x_field_warnings;
        not_array = parsed_points.not_array;
    }

    // stage 8（§6.9.1）：points 内未知字段（在点数/频点检查之前）。
    if let Some(field) = point_field_errors.first() {
        return Err(err_field(
            TargetErrorCode::PointField,
            field,
            "points 元素只允许 freq_hz/mag_db/phase_deg（扩展字段用 x_ 前缀）",
        ));
    }
    if not_array {
        return Err(err_field(
            TargetErrorCode::Points,
            "points",
            "points 必须是数组",
        ));
    }
    let mut meta = meta;
    for field in point_x_warnings {
        meta.warnings.push(TargetWarning::new(
            TargetWarningCode::XFieldPoint,
            format!("{field} 已忽略（不参与语义）"),
        ));
    }
    let document = ParsedDocument {
        form: TargetForm::Json,
        meta,
        points,
        csv_error: None,
        csv_has_phase: None,
        raw_file_bytes: json_bytes.to_vec(),
    };
    import_document(document, options)
}

/// 形式 A：从文件加载 CSV + sidecar（sidecar 默认 = 同目录同名 + `.json`）。
pub fn load_csv_file(
    csv_path: &Path,
    sidecar_path: Option<&Path>,
    options: &ImportOptions,
) -> Result<TargetCurve, TargetError> {
    let csv_bytes = std::fs::read(csv_path).map_err(|error| {
        err_field(
            TargetErrorCode::CsvFormat,
            format!("csv:{}", csv_path.display()),
            format!("读取 CSV 失败：{error}"),
        )
    })?;
    let default_sidecar = default_sidecar_path(csv_path);
    let sidecar_path = sidecar_path.unwrap_or(default_sidecar.as_path());
    let sidecar_text = std::fs::read_to_string(sidecar_path).map_err(|error| {
        err_field(
            TargetErrorCode::MissingField,
            "sidecar",
            format!(
                "CSV 形式必须提供 sidecar（{}）：{error}",
                sidecar_path.display()
            ),
        )
    })?;
    import_csv_bytes(&csv_bytes, &sidecar_text, options)
}

/// 形式 B：从文件加载单 JSON。
pub fn load_json_file(path: &Path, options: &ImportOptions) -> Result<TargetCurve, TargetError> {
    let bytes = std::fs::read(path).map_err(|error| {
        err_field(
            TargetErrorCode::Schema,
            format!("json:{}", path.display()),
            format!("读取 JSON 失败：{error}"),
        )
    })?;
    import_json_bytes(&bytes, options)
}

/// CSV 形式默认 sidecar 路径：同目录、同名 + `.json`（§6.2）。
pub fn default_sidecar_path(csv_path: &Path) -> PathBuf {
    let mut name = csv_path.as_os_str().to_os_string();
    name.push(".json");
    PathBuf::from(name)
}

// ---------------------------------------------------------------------------
// 内置目标（§6.8）
// ---------------------------------------------------------------------------

/// 内置目标几何网格（§6.8：480 点，f_k = 20·1000^(k/479)，端点精确 20 Hz/20 kHz）。
pub fn builtin_grid() -> Vec<f64> {
    let count = BUILTIN_GRID_POINTS;
    let step = 3.0 / (count as f64 - 1.0); // log10(20000/20)=3
    let mut grid = Vec::with_capacity(count);
    for index in 0..count {
        grid.push(10f64.powf(20f64.log10() + step * index as f64));
    }
    grid[0] = TILT10_LO_HZ;
    grid[count - 1] = TILT10_HI_HZ;
    grid
}

/// tilt10 幅度（§6.8：mag_db(f) = −(10/3)·log10(f/20)，20–20 kHz）。
///
/// 端点精确取 20 Hz = 0 dB、20 kHz = −10 dB；其余按 log-f 线性（斜率 −10 dB/decade）。
pub fn tilt10_mag_db(freq_hz: f64) -> f64 {
    if freq_hz == TILT10_LO_HZ {
        return 0.0;
    }
    if freq_hz == TILT10_HI_HZ {
        return -10.0;
    }
    -TILT10_DB_PER_DECADE * (freq_hz / TILT10_LO_HZ).log10()
}

/// 构造内置目标（flat = 默认；tilt10 = 用户口径）。
pub fn builtin_target(
    target: BuiltinTarget,
    options: &BuiltinOptions,
) -> Result<TargetCurve, TargetError> {
    let fs = options.fs;
    let grid = builtin_grid();
    let points: Vec<TargetPoint> = grid
        .iter()
        .map(|freq| {
            let mag_db = match target {
                BuiltinTarget::Flat => 0.0,
                BuiltinTarget::Tilt10 => tilt10_mag_db(*freq),
            };
            TargetPoint::new(*freq, mag_db)
        })
        .collect();
    let mut meta = RawMeta {
        schema: Some(TARGET_SCHEMA.to_string()),
        frequency_unit: Some("Hz".to_string()),
        magnitude_unit: Some("dB".to_string()),
        runtime_rate: Some(fs),
        normalization: Some("none".to_string()),
        interpolation: Some("log_frequency_linear".to_string()),
        edge_policy: Some("hold".to_string()),
        ..RawMeta::default()
    };
    meta.reference_gain_db = Some(options.reference_gain_db);
    let raw_points: Vec<RawPoint> = points
        .iter()
        .map(|point| RawPoint {
            freq_hz: Some(point.freq_hz),
            mag_db: Some(point.mag_db),
            phase_deg: None,
            has_phase_key: false,
            freq_invalid_type: false,
            mag_invalid_type: false,
            phase_invalid_type: false,
        })
        .collect();
    let document = ParsedDocument {
        form: TargetForm::Builtin,
        meta,
        points: raw_points,
        csv_error: None,
        csv_has_phase: Some(false),
        raw_file_bytes: Vec::new(),
    };
    let import_options = ImportOptions::new(fs);
    import_document(document, &import_options)
}

/// 内置 flat（默认目标）。
pub fn builtin_flat(options: &BuiltinOptions) -> Result<TargetCurve, TargetError> {
    builtin_target(BuiltinTarget::Flat, options)
}

/// 内置 tilt10（20 Hz 0 dB → 20 kHz −10 dB，log-f；reference gain 独立）。
pub fn builtin_tilt10(options: &BuiltinOptions) -> Result<TargetCurve, TargetError> {
    builtin_target(BuiltinTarget::Tilt10, options)
}

// ---------------------------------------------------------------------------
// 非模态 solver 读取接口
// ---------------------------------------------------------------------------

/// 非模态 solver / 设计器 / 报告器的目标读取接口（T11 只提供读取，不做设计）。
///
/// 模态 solver 另走 T48–T54；本接口用于 1D（每 path 幅度/相位）目标逼近、PEQ fit（T64）、
/// 报告 oracle（T55）：任意频率采样 + 原生网格 + 采样来源 + 相位语义。
pub trait NonModalTargetSource {
    /// 运行采样率（Hz）。
    fn target_fs(&self) -> f64;
    /// 点数。
    fn target_point_count(&self) -> usize;
    /// 是否有相位目标（phase_target != none）。
    fn has_phase_target(&self) -> bool;
    /// 最终幅度（归一化 + reference_gain_db；edge_policy 生效）。
    fn mag_db_at(&self, freq_hz: f64) -> Result<f64, TargetError>;
    /// 形状幅度（归一化之后、reference_gain_db 之前；edge_policy 生效）。
    fn mag_shape_db_at(&self, freq_hz: f64) -> Result<f64, TargetError>;
    /// 目标总相位（deg；absolute 为给定值；delay_removed 已扣 360·f·D/fs）。
    fn phase_total_deg_at(&self, freq_hz: f64) -> Result<Option<f64>, TargetError>;
    /// 单点采样（含来源）。
    fn sample(&self, freq_hz: f64) -> Result<TargetSample, TargetError>;
    /// 频率网格采样。
    fn sample_grid(&self, freqs_hz: &[f64]) -> Result<Vec<TargetSample>, TargetError>;
    /// 原生网格频率（Hz）。
    fn native_grid_hz(&self) -> Vec<f64>;
}

impl NonModalTargetSource for TargetCurve {
    fn target_fs(&self) -> f64 {
        self.meta.runtime_rate
    }

    fn target_point_count(&self) -> usize {
        self.points.len()
    }

    fn has_phase_target(&self) -> bool {
        self.phase_total_deg.is_some()
    }

    fn mag_db_at(&self, freq_hz: f64) -> Result<f64, TargetError> {
        TargetCurve::mag_db_at(self, freq_hz)
    }

    fn mag_shape_db_at(&self, freq_hz: f64) -> Result<f64, TargetError> {
        TargetCurve::mag_shape_db_at(self, freq_hz)
    }

    fn phase_total_deg_at(&self, freq_hz: f64) -> Result<Option<f64>, TargetError> {
        TargetCurve::phase_total_deg_at(self, freq_hz)
    }

    fn sample(&self, freq_hz: f64) -> Result<TargetSample, TargetError> {
        TargetCurve::sample(self, freq_hz)
    }

    fn sample_grid(&self, freqs_hz: &[f64]) -> Result<Vec<TargetSample>, TargetError> {
        TargetCurve::sample_grid(self, freqs_hz)
    }

    fn native_grid_hz(&self) -> Vec<f64> {
        self.frequencies_hz()
    }
}

/// 对任意 `NonModalTargetSource` 做网格采样（dyn 便利函数）。
pub fn sample_target_grid(
    target: &dyn NonModalTargetSource,
    freqs_hz: &[f64],
) -> Result<Vec<TargetSample>, TargetError> {
    target.sample_grid(freqs_hz)
}

/// 几何（log-f）网格：lo..=hi 共 `points` 点，端点精确。
pub fn log_frequency_grid(lo_hz: f64, hi_hz: f64, points: usize) -> Vec<f64> {
    if points == 0 {
        return Vec::new();
    }
    if points == 1 {
        return vec![lo_hz];
    }
    if !(lo_hz.is_finite() && hi_hz.is_finite()) || lo_hz <= 0.0 || hi_hz <= lo_hz {
        return Vec::new();
    }
    let step = (hi_hz.log10() - lo_hz.log10()) / (points as f64 - 1.0);
    let mut grid = Vec::with_capacity(points);
    for index in 0..points {
        grid.push(10f64.powf(lo_hz.log10() + step * index as f64));
    }
    grid[0] = lo_hz;
    grid[points - 1] = hi_hz;
    grid
}

// ---------------------------------------------------------------------------
// TargetCurve 实现
// ---------------------------------------------------------------------------

impl TargetCurve {
    /// 元数据（§6.3）。
    pub fn meta(&self) -> &TargetMeta {
        &self.meta
    }

    /// 导入报告（§6.4/§6.5/§6.6/§6.7）。
    pub fn report(&self) -> &TargetImportReport {
        &self.report
    }

    /// 原始导入点（mag 为给定值，phase 为给定 φ_ex 值）。
    pub fn raw_points(&self) -> &[TargetPoint] {
        &self.points
    }

    /// 网格频率（Hz）。
    pub fn frequencies_hz(&self) -> Vec<f64> {
        self.points.iter().map(|point| point.freq_hz).collect()
    }

    /// 归一化后、reference gain 前的幅度。
    pub fn normalized_mag_db(&self) -> &[f64] {
        &self.mag_normalized_db
    }

    /// 归一化 + reference_gain_db 后的最终幅度。
    pub fn final_mag_db(&self) -> &[f64] {
        &self.mag_final_db
    }

    /// 展开后的 φ_ex（无 phase 为 None）。
    pub fn phase_ex_unwrapped_deg(&self) -> Option<&[f64]> {
        self.phase_ex_unwrapped_deg.as_deref()
    }

    /// 目标总相位（absolute = φ_ex；delay_removed 已扣 360·f·D/fs；无 phase 为 None）。
    pub fn phase_total_deg(&self) -> Option<&[f64]> {
        self.phase_total_deg.as_deref()
    }

    /// 运行采样率（Hz）。
    pub fn fs(&self) -> f64 {
        self.meta.runtime_rate
    }

    /// 是否有相位目标。
    pub fn has_phase(&self) -> bool {
        self.phase_total_deg.is_some()
    }

    /// 端点行为。
    pub fn edge_policy(&self) -> EdgePolicy {
        self.meta.edge_policy
    }

    /// 规范点序列 sha256（§6.3；用于 target stale/版本链）。
    pub fn target_sha256(&self) -> &str {
        &self.canonical_points_sha256
    }

    /// 最终幅度采样（edge_policy 生效）。
    pub fn mag_db_at(&self, freq_hz: f64) -> Result<f64, TargetError> {
        let freqs = self.frequencies_hz();
        let (value, _) =
            interpolate_log_f(&freqs, &self.mag_final_db, freq_hz, self.edge_policy())?;
        Ok(value)
    }

    /// 形状幅度采样（归一化之后、reference gain 之前）。
    pub fn mag_shape_db_at(&self, freq_hz: f64) -> Result<f64, TargetError> {
        let freqs = self.frequencies_hz();
        let (value, _) =
            interpolate_log_f(&freqs, &self.mag_normalized_db, freq_hz, self.edge_policy())?;
        Ok(value)
    }

    /// φ_ex 采样（无 phase 返回 None）。
    pub fn phase_ex_deg_at(&self, freq_hz: f64) -> Result<Option<f64>, TargetError> {
        let Some(phases) = self.phase_ex_unwrapped_deg.as_deref() else {
            return Ok(None);
        };
        let freqs = self.frequencies_hz();
        let (value, _) = interpolate_log_f(&freqs, phases, freq_hz, self.edge_policy())?;
        Ok(Some(value))
    }

    /// 目标总相位采样（无 phase 返回 None）。
    pub fn phase_total_deg_at(&self, freq_hz: f64) -> Result<Option<f64>, TargetError> {
        let Some(phases) = self.phase_total_deg.as_deref() else {
            return Ok(None);
        };
        let freqs = self.frequencies_hz();
        let (value, _) = interpolate_log_f(&freqs, phases, freq_hz, self.edge_policy())?;
        Ok(Some(value))
    }

    /// 单点采样（非模态 solver 主要入口）。
    pub fn sample(&self, freq_hz: f64) -> Result<TargetSample, TargetError> {
        let freqs = self.frequencies_hz();
        let (mag_db, mag_source) =
            interpolate_log_f(&freqs, &self.mag_final_db, freq_hz, self.edge_policy())?;
        let (mag_shape_db, _) =
            interpolate_log_f(&freqs, &self.mag_normalized_db, freq_hz, self.edge_policy())?;
        let (phase_total_deg, phase_source) = match self.phase_total_deg.as_deref() {
            Some(phases) => {
                let (value, source) =
                    interpolate_log_f(&freqs, phases, freq_hz, self.edge_policy())?;
                (Some(value), Some(source))
            }
            None => (None, None),
        };
        let phase_ex_deg = match self.phase_ex_unwrapped_deg.as_deref() {
            Some(phases) => {
                let (value, _) = interpolate_log_f(&freqs, phases, freq_hz, self.edge_policy())?;
                Some(value)
            }
            None => None,
        };
        Ok(TargetSample {
            freq_hz,
            mag_db,
            mag_shape_db,
            phase_total_deg,
            phase_ex_deg,
            mag_source,
            phase_source,
        })
    }

    /// 网格采样。
    pub fn sample_grid(&self, freqs_hz: &[f64]) -> Result<Vec<TargetSample>, TargetError> {
        let mut out = Vec::with_capacity(freqs_hz.len());
        for freq in freqs_hz {
            out.push(self.sample(*freq)?);
        }
        Ok(out)
    }

    /// 幅度网格（非模态 solver 便捷接口）：(freq_hz, mag_db)。
    pub fn magnitude_curve(&self) -> Vec<(f64, f64)> {
        self.points
            .iter()
            .zip(self.mag_final_db.iter())
            .map(|(point, mag)| (point.freq_hz, *mag))
            .collect()
    }

    /// 供 T64（PEQ fit）/T55（报告器）直接消费的幅度点表 `[(freq, mag_db)]`。
    pub fn modal_peq_curve_points(&self) -> Vec<(f64, f64)> {
        self.magnitude_curve()
    }

    /// 族相位接受性检查（§6.10）；返回报告用的 `phase_target` 类别。
    pub fn phase_target_for_family(
        &self,
        family: TargetFamily,
        allow_phase_ignored: bool,
    ) -> Result<PhaseTargetClass, TargetError> {
        if family.forbids_phase() && self.has_phase() {
            if allow_phase_ignored {
                return Ok(PhaseTargetClass::Ignored);
            }
            return Err(err_field(
                TargetErrorCode::PhaseUnsupported,
                "phase_ref",
                format!(
                    "族 {} 不接受 phase 目标；建议 corrected/lowdelay 或显式 allow_phase_ignored",
                    family.as_str()
                ),
            ));
        }
        if family.requires_phase() && !self.has_phase() {
            return Err(err_field(
                TargetErrorCode::PhaseUnsupported,
                "phase_ref",
                format!(
                    "族 {} 必须有相位目标（absolute/delay_removed）",
                    family.as_str()
                ),
            ));
        }
        Ok(match self.meta.phase_ref {
            PhaseRef::None => PhaseTargetClass::None,
            PhaseRef::Absolute => PhaseTargetClass::Absolute,
            PhaseRef::DelayRemoved => PhaseTargetClass::DelayRemoved,
        })
    }

    /// 设计预算检查（§6.9 码 28；与设计方联合判定，INCOMPLETE 而非静默截断）。
    pub fn check_budget(&self, budget: &TargetBudget) -> Result<BudgetReport, TargetError> {
        let freqs = self.frequencies_hz();
        check_budget_inner(&freqs, self.meta.target_delay_samples, budget)
    }

    /// delay_removed 目标的 D_used 一致性检查（§6.6：|D_used − D_target| <= 0.01 sample）。
    pub fn check_delay_consistency(&self, d_used_samples: f64) -> Result<(), TargetError> {
        if self.meta.phase_ref != PhaseRef::DelayRemoved {
            return Ok(());
        }
        let target = self.meta.target_delay_samples.unwrap_or(0) as f64;
        if !d_used_samples.is_finite()
            || (d_used_samples - target).abs() > TARGET_DELAY_CONSISTENCY_SAMPLES
        {
            return Err(err_field(
                TargetErrorCode::Delay,
                "D_used",
                format!(
                    "D_used={d_used_samples} 与 target_delay_samples={target} 偏差超过 {} sample",
                    TARGET_DELAY_CONSISTENCY_SAMPLES
                ),
            ));
        }
        Ok(())
    }

    /// 确定性 JSON 报告（证据/日志/下游 manifest 用；键序固定）。
    pub fn to_json_report(&self) -> String {
        let report = &self.report;
        let mut out = String::new();
        out.push('{');
        let mut first = true;
        push_json_str(
            &mut out,
            &mut first,
            "schema",
            "tspi-target-report-draft/v1",
        );
        push_json_str(&mut out, &mut first, "tool", "khaen-designer/targets");
        push_json_str(&mut out, &mut first, "form", report.form.as_str());
        push_json_raw(
            &mut out,
            &mut first,
            "point_count",
            &report.point_count.to_string(),
        );
        push_json_number(&mut out, &mut first, "fs", self.meta.runtime_rate);
        push_json_bool(&mut out, &mut first, "has_phase", report.has_phase);
        push_json_str(
            &mut out,
            &mut first,
            "phase_target",
            report.phase_target.as_str(),
        );
        push_json_opt_str(
            &mut out,
            &mut first,
            "phase_family_used",
            report.phase_family_used.map(TargetFamily::as_str),
        );
        push_json_str(
            &mut out,
            &mut first,
            "normalization",
            report.normalization.as_str(),
        );
        push_json_number(
            &mut out,
            &mut first,
            "normalization_offset_db",
            report.normalization_offset_db,
        );
        push_json_number(
            &mut out,
            &mut first,
            "reference_gain_db",
            report.reference_gain_db,
        );
        push_json_number(
            &mut out,
            &mut first,
            "applied_offset_db",
            report.applied_offset_db,
        );
        push_json_number(&mut out, &mut first, "max_boost_db", report.max_boost_db);
        push_json_bool(&mut out, &mut first, "boost_warning", report.boost_warning);
        push_json_bool(&mut out, &mut first, "allow_boost", report.allow_boost);
        push_json_number(&mut out, &mut first, "min_freq_hz", report.min_freq_hz);
        push_json_number(&mut out, &mut first, "max_freq_hz", report.max_freq_hz);
        push_json_number(
            &mut out,
            &mut first,
            "min_interval_hz",
            report.min_interval_hz,
        );
        push_json_number(
            &mut out,
            &mut first,
            "max_interval_hz",
            report.max_interval_hz,
        );
        push_json_number(
            &mut out,
            &mut first,
            "min_octave_interval",
            report.min_octave_interval,
        );
        push_json_number(
            &mut out,
            &mut first,
            "max_octave_interval",
            report.max_octave_interval,
        );
        push_json_bool(&mut out, &mut first, "covers_20_20k", report.covers_20_20k);
        push_json_raw(
            &mut out,
            &mut first,
            "at_20hz",
            &coverage_json(&report.at_20hz),
        );
        push_json_raw(
            &mut out,
            &mut first,
            "at_20khz",
            &coverage_json(&report.at_20khz),
        );
        match &report.unwrap {
            Some(unwrap) => push_json_raw(
                &mut out,
                &mut first,
                "unwrap",
                &format!(
                    "{{\"policy\":\"{}\",\"wraps_applied\":{},\"max_abs_delta_deg\":{},\"raw_max_abs_delta_deg\":{}}}",
                    unwrap.policy.as_str(),
                    unwrap.wraps_applied,
                    number_or_null(Some(unwrap.max_abs_delta_deg)),
                    number_or_null(Some(unwrap.raw_max_abs_delta_deg))
                ),
            ),
            None => push_json_raw(&mut out, &mut first, "unwrap", "null"),
        }
        match report.target_delay_samples {
            Some(delay) => push_json_raw(
                &mut out,
                &mut first,
                "target_delay_samples",
                &delay.to_string(),
            ),
            None => push_json_raw(&mut out, &mut first, "target_delay_samples", "null"),
        }
        push_json_str(
            &mut out,
            &mut first,
            "canonical_points_sha256",
            &report.canonical_points_sha256,
        );
        push_json_str(
            &mut out,
            &mut first,
            "canonical_points_hash_algo",
            report.canonical_points_hash_algo,
        );
        push_json_opt_str(
            &mut out,
            &mut first,
            "source_hash",
            report.source_hash.as_deref(),
        );
        push_json_bool(
            &mut out,
            &mut first,
            "source_hash_verified",
            report.source_hash_verified,
        );
        push_json_opt_str(
            &mut out,
            &mut first,
            "source_file_sha256",
            report.source_file_sha256.as_deref(),
        );
        push_json_bool(
            &mut out,
            &mut first,
            "source_file_sha256_verified",
            report.source_file_sha256_verified,
        );
        push_json_raw(&mut out, &mut first, "warnings", &warnings_json(report));
        push_json_raw(&mut out, &mut first, "notes", &notes_json(report));
        out.push('}');
        out
    }
}

fn number_or_null(value: Option<f64>) -> String {
    match value.and_then(canonical_number) {
        Some(text) => text,
        None => "null".to_string(),
    }
}

fn coverage_json(value: &CoverageValue) -> String {
    let index_json = match value.source.index() {
        Some(index) => index.to_string(),
        None => "null".to_string(),
    };
    format!(
        "{{\"value_db\":{},\"source\":\"{}\",\"index\":{}}}",
        number_or_null(value.value),
        value.source.as_str(),
        index_json
    )
}

fn warnings_json(report: &TargetImportReport) -> String {
    let mut out = String::from("[");
    for (index, warning) in report.warnings.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("{\"code\":\"");
        out.push_str(warning.code.as_str());
        out.push_str("\",\"detail\":");
        out.push_str(&json_string(&warning.detail));
        out.push('}');
    }
    out.push(']');
    out
}

fn notes_json(report: &TargetImportReport) -> String {
    let mut out = String::from("[");
    for (index, note) in report.notes.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&json_string(note));
    }
    out.push(']');
    out
}

fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", ch as u32));
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn push_json_raw(out: &mut String, first: &mut bool, key: &str, raw: &str) {
    if !*first {
        out.push(',');
    }
    *first = false;
    out.push_str(&json_string(key));
    out.push(':');
    out.push_str(raw);
}

fn push_json_str(out: &mut String, first: &mut bool, key: &str, value: &str) {
    push_json_raw(out, first, key, &json_string(value));
}

fn push_json_opt_str(out: &mut String, first: &mut bool, key: &str, value: Option<&str>) {
    match value {
        Some(value) => push_json_str(out, first, key, value),
        None => push_json_raw(out, first, key, "null"),
    }
}

fn push_json_bool(out: &mut String, first: &mut bool, key: &str, value: bool) {
    push_json_raw(out, first, key, if value { "true" } else { "false" });
}

fn push_json_number(out: &mut String, first: &mut bool, key: &str, value: f64) {
    push_json_raw(out, first, key, &number_or_null(Some(value)));
}

// ---------------------------------------------------------------------------
// 私有测试（tests/T11 通过 include! 进入本模块的 #[cfg(test)] 子模块）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod targets_tests {
    use super::*;

    include!("../tests/T11/target_import_cases.rs");
}
