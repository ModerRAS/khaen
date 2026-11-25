//! [NONRT] 校准 v1/v1.1 -> plan 适配（core-v1 §11.10 / §5.3） ——WBS T13 写域。
//!
//! 本模块是 rt-stack compat leaf `calibration`（WBS T13，deps T11/T12/T05）。职责是把
//! **既有 `tspi-calibration/v1` 与 `/v1.1` YAML（只读、语义不改）** 适配成 P1 自研栈的
//! **内部 IR 草案**（`tspi-cal-ir-draft/v1`），再投影成 T12 编译入口可消费的 P 阶段，
//! 并绑定 T11 目标曲线（期望响应）引用，供 T26/T60/T66 在 prepare/apply 期使用。
//!
//! 权威契约（只读输入）：
//! - `calibration-spec/CONTRACT.md` v1/v1.1（§2 顶层结构、§2.1 字段表、§3 语义、
//!   §4.1 渲染映射、§5 校验规则、§5.1 校验器覆盖差异、§10 v1.1 差异）；
//! - `rt-stack/spec/compat-matrix.md` §5 Y12–Y16/Y24（calibration 消费要求、位置/生命周期、
//!   未知项策略、→渲染映射、IR/manifest version/hash/role/fs/taps/placement/P/phase/D/stale）；
//! - `rt-stack/spec/fir-toolbox.md` v1.2-draft §5.1（P_post 规范链序）、§5.2/§5.8（PEQ 消费语义、
//!   与 calibration v1.1 的关系）、§7.1/§7.4（P/G 行列、stale/版本/确定性）、§6（目标曲线仅设计输入）；
//! - `rt-stack/spec/core-v1.md` v1.1.1 §11.10（`TspiRoleV1`/`TspiDiagFirV1` 的 delay/delay_q16/
//!   gain/polarity 字段与总时延口径）、§5.3（stale 双条件）、§15.1 N3（分数 D 折入 A/P 后对拍）；
//! - `scratch/lead/planner/TSPI-RT-STACK-WBS.md` T13 行与 §1/§5/§6/§7；
//! - `scratch/lead/planner/DSP-HOST-DECISION.md` §2（完整校准）、§3（FIR/融合/目标草案）；
//! - T11 实装 `crates/designer/src/targets.rs`（目标曲线 `TargetCurve` 公共访问器）与
//!   T12 实装 `crates/core/src/compile.rs`（`CompileOptions`/`CompileInput`/`StagePlan`/
//!   `design_delay_to_plan`）。
//!
//! # 范围
//! 1. **只读** YAML 解析 + CONTRACT §5 规则校验（v1 严格 peaking/Q≤10；v1.1 shelf 语义
//!    q=RBJ slope S、peaking q 无上限）；未知 schema/type/枚举/单位一律拒收，未知**扩展字段**
//!    只 warning（CONTRACT §5 规则 12、compat-matrix Y14）。
//! 2. 与渲染基线角色表**合并**（CONTRACT §4.1）：`delay_samples` 相加、`polarity` XOR、
//!    `gain_db` 相加、PEQ 按序追加且 **type 原样保留**；重复应用检测（同一校准二次应用即拒收）。
//! 3. **内部 IR 草案** `tspi-cal-ir-draft/v1`：`ir_version`、内容 `ir_hash`（规范化序列化
//!    SHA-256）、role/fs/layout 绑定、`P/phase/D/taps/placement` 记录、stereo mask/目标 profile、
//!    以及 **stale 判定**（input/fs/role/P/hash 变化原因逐条给出）。
//! 4. **curve 不双叠**：目标曲线（T11 期望响应）在 IR 中最多绑定一次，且只落在基线 P 设计
//!    （`TargetFir` 阶段）；G 只允许 modal residual、**禁止**绑定 curve（双叠即错误）。校准
//!    `filters`/`gain_db`/`delay`/`polarity` 也不得被二次折入 P FIR（本模块逐条给出唯一应用点）。
//! 5. **与 T12 接口**：IR 投影成事实上的 P 阶段（行主序 f64 + `core::compile::StagePlan`），
//!    经 `CompileOptions` 的 fs/B/taps/预算校验后交给 `compile()`；delay/polarity 的
//!    coeff-embedded 与 role-metadata 两种编码二选一，禁止重复计（见 `PostEncoding`）。
//!
//! # 非目标
//! - 不设计/拟合 FIR、不算 biquad 系数（T41/T64/T40）、不做 5×5 solver（T50/T51）；
//! - 不读测量 CSV、不改 CONTRACT 字段、不写/覆盖任何校准文件（只读；T13 无写文件路径）；
//! - 不做 apply/prepare/rollback/RT、不碰 ALSA/CueMix/服务/板端/网络；不新增依赖（纯 std + `tspi-dsp-core`）；
//! - 不定义 overlay layout 解析（T57）与 channelmap 渲染（T25）：本模块只接受调用方给的
//!   `LayoutRoleTable` / `RenderRoleBase` 作为输入并做一致性校验。
//!
//! # 与 T11 的接口（数据合同，无 crate 依赖）
//! compat crate 只依赖 `tspi-dsp-core`（workspace 依赖未在本任务写域内，禁止加依赖）。T11 的
//! `TargetCurve` 在 `tspi-fir-designer` crate，因此本模块用 **纯数据视图** 对接：T26/T60 用
//! T11 公共访问器构造一个本地 adapter 类型并实现 [`TargetCurveSource`]：
//!
//! ```text
//! // T26/T60 侧（伪代码；本地 newtype，避免跨 crate orphan 限制）：
//! struct T11Curve<'a>(&'a designer::targets::TargetCurve);
//! impl compat::calibration::TargetCurveSource for T11Curve<'_> {
//!     fn curve_id(&self) -> &str { /* 设计器给的曲线 id */ }
//!     fn fs_hz(&self) -> f64 { self.0.fs() }
//!     fn target_sha256(&self) -> &str { self.0.target_sha256() }
//!     fn phase_ref(&self) -> Option<&str> { Some(self.0.meta().phase_ref.as_str()) }
//!     fn target_delay_samples(&self) -> Option<u32> { self.0.meta().target_delay_samples }
//!     fn point_count(&self) -> usize { self.0.raw_points().len() }
//! }
//! ```
//!
//! [`TargetCurveRef::from_source`] 做 schema 化校验（fs 必须等于本 IR fs、`target_sha256`
//! 必须是 64 hex、`phase_ref` 未知值拒收、`delay_removed` 必须有整数 D），然后
//! [`IrDraftBuilder::with_curve`] 绑定一次；G residual 侧只能 [`IrDraftBuilder::g_residual_participants`]。
//!
//! # 错误/警告命名
//! - 错误码 `E_CAL_*`（校准文件/合并，语义未知一律拒收）与 `E_IR_*`（IR 草案/投影）。
//! - 警告码 `W_CAL_*`（CONTRACT 规定的 warning：未知扩展字段、v1.1 可读性提示、boost>6 等）。
//! - 单文件多错并存时本模块返回**第一个**错误（CONTRACT 未要求聚合；顺序固定为
//!   YAML -> schema -> 顶层字段 -> channels 逐条 -> 跨通道归一化/reference -> layout），
//!   与 Python 独立复核器 `validate_calibration.py` 的检查集合对齐（见 evidence/T13/MANIFEST.md）。
//!
//! # RT / 板端
//! 本模块是 NONRT 控制/设计期路径（读文件、分配 Vec、字符串格式化），**不是 RT-MODULE**，
//! 不得被 RT worker 调用。当前状态：**leaf 实现 + 真实 `cargo test -p tspi-dsp-compat calibration`**；
//! 未上板、未做真实校准/测量、未与 T26/T60/T66 集成、未形成实际 plan 字节（见 evidence/T13）。

use std::fmt;
use std::path::Path;

use tspi_dsp_core::compile::{design_delay_to_plan, CompileOptions, StagePlan};
use tspi_dsp_core::rt::{
    BLOCK_SIZES, MAX_LOW_BAND_PARTICIPANTS, MAX_OUTPUT_ROLES, SAMPLE_RATES_HZ, TAPS_EXTENDED_MAX,
    TAPS_NORMAL_MAX,
};

// ===========================================================================
// 常量（CONTRACT v1/v1.1、compat-matrix Y12–Y16、core-v1 §11.10）
// ===========================================================================

/// 既有校准 schema v1（字段/语义冻结，本模块只读）。
pub const CAL_SCHEMA_V1: &str = "tspi-calibration/v1";
/// 既有校准 schema v1.1（新增 shelf type 与 peaking q 放宽；本模块只读）。
pub const CAL_SCHEMA_V11: &str = "tspi-calibration/v1.1";
/// 内部 IR 草案 schema（P1 新，不是 CONTRACT 字段）。
pub const IR_SCHEMA: &str = "tspi-cal-ir-draft/v1";
/// IR 草案整数版本；字段语义变化必须升版（compat-matrix Y24）。
pub const IR_VERSION: u32 = 1;
/// IR `ir_hash` 规范化序列化算法标识（f64 以 `to_bits` 十六进制，键序固定，无空白）。
pub const IR_HASH_ALGO: &str = "tspi-cal-ir-canonical-v1";
/// 渠道数上限（CONTRACT §5 规则 3）。
pub const CAL_MAX_CHANNELS: usize = 32;
/// 每通道 PEQ 段数上限（CONTRACT §5 规则 8 / channelmap 硬上限）。
pub const CAL_MAX_FILTERS: usize = 32;
/// 延迟补偿上限毫秒（CONTRACT §3.1：channelmap delay_ms 0..200）。
pub const CAL_DELAY_MAX_MS: f64 = 200.0;
/// 校准 trim `gain_db` 下界（CONTRACT §2.1）。
pub const CAL_TRIM_MIN_DB: f64 = -12.0;
/// 校准 trim `gain_db` 上界（CONTRACT §2.1）。
pub const CAL_TRIM_MAX_DB: f64 = 6.0;
/// 单段 PEQ `gain_db` 下界（CONTRACT §2.1）。
pub const CAL_FILTER_GAIN_MIN_DB: f64 = -24.0;
/// 单段 PEQ `gain_db` 上界（CONTRACT §2.1）。
pub const CAL_FILTER_GAIN_MAX_DB: f64 = 24.0;
/// PEQ 频率下界（CONTRACT §2.1：20 Hz）。
pub const CAL_FREQ_MIN_HZ: f64 = 20.0;
/// PEQ 频率上界基准（CONTRACT §2.1：min(20000, fs/2-1)）。
pub const CAL_FREQ_MAX_HZ: f64 = 20_000.0;
/// 单通道 PEQ 正向提升拒收阈值（CONTRACT §5 规则 9）。
pub const CAL_POS_BOOST_REJECT_DB: f64 = 12.0;
/// 单通道 PEQ 正向提升 warning 阈值（CONTRACT §5 规则 9）。
pub const CAL_POS_BOOST_WARN_DB: f64 = 6.0;
/// v1/v1.1 peaking Q 下界（CONTRACT §2.1）。
pub const CAL_PEAKING_Q_MIN: f64 = 0.1;
/// v1 peaking Q 上界（CONTRACT §10：v1 为 0.1..=10）。
pub const CAL_PEAKING_Q_MAX_V1: f64 = 10.0;
/// v1.1 shelf slope S 下界（CONTRACT §3.4/§10：`q` 承载 RBJ slope S）。
pub const CAL_SHELF_SLOPE_MIN: f64 = 0.1;
/// v1.1 shelf slope S 上界（CamillaDSP `slope=12*S` 恒 ≤12 dB/oct）。
pub const CAL_SHELF_SLOPE_MAX: f64 = 1.0;
/// 合并后 role gain 下界（channelmap v2 `roles[].gain_db` -60..+12）。
pub const CHANNEL_MAP_GAIN_MIN_DB: f64 = -60.0;
/// 合并后 role gain 上界（channelmap v2 `roles[].gain_db` -60..+12）。
pub const CHANNEL_MAP_GAIN_MAX_DB: f64 = 12.0;
/// CONTRACT §5 规则 2 声明可接受的采样率（部署口径为 48k/96k；其它仅 warning）。
pub const CAL_DECLARED_SAMPLE_RATES_HZ: [u32; 4] = [44_100, 48_000, 88_200, 96_000];

const _: () = {
    assert!(IR_VERSION == 1);
    assert!(CAL_MAX_CHANNELS == 32 && CAL_MAX_FILTERS == 32);
    assert!(CAL_DELAY_MAX_MS == 200.0);
    assert!(CAL_TRIM_MIN_DB == -12.0 && CAL_TRIM_MAX_DB == 6.0);
    assert!(CAL_FILTER_GAIN_MIN_DB == -24.0 && CAL_FILTER_GAIN_MAX_DB == 24.0);
    assert!(CAL_POS_BOOST_REJECT_DB == 12.0 && CAL_POS_BOOST_WARN_DB == 6.0);
    assert!(CAL_PEAKING_Q_MIN == 0.1 && CAL_PEAKING_Q_MAX_V1 == 10.0);
    assert!(CAL_SHELF_SLOPE_MIN == 0.1 && CAL_SHELF_SLOPE_MAX == 1.0);
    assert!(CHANNEL_MAP_GAIN_MIN_DB == -60.0 && CHANNEL_MAP_GAIN_MAX_DB == 12.0);
    assert!(CAL_DECLARED_SAMPLE_RATES_HZ[0] == 44_100);
    assert!(SAMPLE_RATES_HZ[0] == 48_000 && SAMPLE_RATES_HZ[1] == 96_000);
    assert!(BLOCK_SIZES[0] == 64 && BLOCK_SIZES[1] == 128);
    assert!(MAX_OUTPUT_ROLES == 16);
};

/// P_post 规范链序（fir-toolbox §5.1；CONTRACT §4.2 是其不含 TargetFir 的缩减链，
/// 校准阶段的相对顺序一致：PEQ 在 gain 之前、delay/polarity 在 gain 之后）。
pub const POST_PLACEMENT_CHAIN: [PostStage; 7] = [
    PostStage::CrossoverHpLp,
    PostStage::ModalPeq,
    PostStage::TargetFir,
    PostStage::Gain,
    PostStage::Delay,
    PostStage::Polarity,
    PostStage::Protection,
];

// ===========================================================================
// 错误 / 警告
// ===========================================================================

// (errors and warnings appended in part 2)
/// 校准适配错误码（`E_CAL_*` 文件/合并；`E_IR_*` IR 草案/投影）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CalibrationErrorCode {
    /// 输入不是合法 UTF-8。
    Encoding,
    /// 读取校准文件失败（IO）。
    Io,
    /// YAML 语法错误（缩进/引号/流式集合等）。
    YamlSyntax,
    /// 使用了本模块明确不支持的 YAML 特性（anchor/tag/多文档/块标量/多行流式）。
    YamlUnsupported,
    /// YAML 顶层不是 mapping。
    YamlRoot,
    /// schema 缺失/不是 `tspi-calibration/v1` 或 `/v1.1`。
    Schema,
    /// 必填字段缺失（sample_rate/reference_index/channels/filters 等）。
    MissingField,
    /// 字段类型错误（该是整数/字符串/映射/数组而不是）。
    FieldType,
    /// channels 为空或超过 32 条。
    Channels,
    /// 通道 name 非法或不唯一。
    ChannelName,
    /// 通道 index 非法/不唯一/超出 `measurement.playback.channels`。
    ChannelIndex,
    /// sample_rate 非法（非整数/非正）。
    SampleRate,
    /// reference_index 非法或不在 channels[].index 中。
    ReferenceIndex,
    /// layout 字段类型非法（应为字符串）。
    Layout,
    /// `tool`/`version`/`note`/`created_unix` 字段类型非法。
    Metadata,
    /// measurement 块类型/值非法（mode/render_offsets/playback.channels）。
    Measurement,
    /// delay_samples 非整数/为负/超 200 ms 上限/合并后越界。
    Delay,
    /// delay_samples 全文件最小值不是 0（未归一化）。
    DelayNormalization,
    /// polarity 非 normal|invert（含兼容别名 1/-1）。
    Polarity,
    /// channels[].gain_db 越界（[-12,+6]）。
    TrimGain,
    /// filters 不是数组或缺失。
    Filters,
    /// filters 段数超过 32。
    FilterCount,
    /// filter 字段类型/必填错误。
    FilterField,
    /// filter 频率越界（20..=min(20000, fs/2-1)）。
    FilterFreq,
    /// filter q 非法（v1 0.1..=10；v1.1 peaking ≥0.1；v1.1 shelf S ∈ [0.1,1.0]）。
    FilterQ,
    /// filter 单段 gain_db 越界（[-24,+24]）。
    FilterGain,
    /// filter type 未知（消费方必须整份拒收，不降级）。
    FilterTypeUnknown,
    /// v1 文件出现 peaking 之外的 type（整份拒收，v1 消费方读不了）。
    FilterTypeNotInV1,
    /// 单通道 PEQ 正向提升总和 > 12 dB。
    PositiveBoost,
    /// `channels[].measured` 不是 mapping（warning 级；此码保留为严格模式的错误形态）。
    Measured,
    /// 给定了 layout 角色表但 calibration.layout 不一致/缺失。
    LayoutMismatch,
    /// calibration 里的角色不在 layout 角色表中。
    LayoutRoleUnknown,
    /// calibration channels[].index 与 layout 角色首输出不一致。
    LayoutIndexMismatch,
    /// 合并：校准通道 name 在渲染基线角色表中不存在（禁止跨布局套用）。
    RoleNotFound,
    /// 合并：校准通道 index 与基线角色 index 不一致。
    RoleIndexMismatch,
    /// 合并：同一校准被二次应用（origin 已标 CalibrationApplied）。
    DoubleApply,
    /// 合并：基线角色表 name/index 重复或 gain 越界。
    BaseRoleInvalid,
    /// 合并：合并后 PEQ > 32 段。
    PeqOverflow,
    /// 合并：合并后延时 > 200 ms。
    DelayOverflow,
    /// 合并：合并后 role gain 超出 channelmap [-60,+12]。
    MergedGainRange,
    /// 校准文件 sample_rate 与目标 profile fs 不一致。
    FsMismatch,
    /// IR：profile id/hash/target profile 字段非法。
    IrProfile,
    /// IR：fs 不是部署口径 {48000, 96000}（plan/CompileOptions 拒绝）。
    IrRateUnsupported,
    /// IR：role 数/绑定非法（重复、超 16、为空）。
    IrRoles,
    /// IR：stereo mask 非法（stereo_strict 必须恰为 FL/FR；mask 名字必须在 role 表内）。
    IrStereoMask,
    /// IR：目标曲线重复绑定（curve 不双叠的第一条门）。
    IrCurveDoubleApply,
    /// IR：目标曲线 `target_sha256` 非 64 hex。
    IrCurveHash,
    /// IR：目标曲线 fs 与 IR fs 不一致。
    IrCurveFs,
    /// IR：目标曲线 phase_ref 未知/缺失所需字段。
    IrCurvePhase,
    /// IR：目标曲线点数 < 2（T11 目标导入保证 >=2）。
    IrCurvePoints,
    /// IR：G residual 绑定了 curve（禁止；G 只 modal residual）。
    IrGCurve,
    /// IR：`x_` 之外的扩展字段（unknown 拒收）。
    IrFieldUnknown,
    /// IR：扩展字段重复键。
    IrExtensionDuplicate,
    /// IR：delay 到 plan 的 Q16 拆分失败（core `design_delay_to_plan` 拒收）。
    IrDelayPlan,
    /// IR：delay/polarity 编码模式与投影自相矛盾（重复计）。
    IrEncodingConflict,
    /// IR：P 阶段 taps 超 profile 上限（INFEASIBLE，不静默截尾）。
    IrTapsBudget,
    /// IR：P 阶段 f32 系数区超预算。
    IrCapacity,
    /// IR：`CompileOptions` 自身非法（fs/B/max_taps/extended/budget）。
    IrCompileOptions,
    /// IR：IR fs 与 `CompileOptions.sample_rate_hz` 不一致。
    IrRateMismatch,
}

impl CalibrationErrorCode {
    /// 全部错误码（顺序即文档顺序；测试覆盖 100%）。
    pub const ALL: &'static [CalibrationErrorCode] = &[
        CalibrationErrorCode::Encoding,
        CalibrationErrorCode::Io,
        CalibrationErrorCode::YamlSyntax,
        CalibrationErrorCode::YamlUnsupported,
        CalibrationErrorCode::YamlRoot,
        CalibrationErrorCode::Schema,
        CalibrationErrorCode::MissingField,
        CalibrationErrorCode::FieldType,
        CalibrationErrorCode::Channels,
        CalibrationErrorCode::ChannelName,
        CalibrationErrorCode::ChannelIndex,
        CalibrationErrorCode::SampleRate,
        CalibrationErrorCode::ReferenceIndex,
        CalibrationErrorCode::Layout,
        CalibrationErrorCode::Metadata,
        CalibrationErrorCode::Measurement,
        CalibrationErrorCode::Delay,
        CalibrationErrorCode::DelayNormalization,
        CalibrationErrorCode::Polarity,
        CalibrationErrorCode::TrimGain,
        CalibrationErrorCode::Filters,
        CalibrationErrorCode::FilterCount,
        CalibrationErrorCode::FilterField,
        CalibrationErrorCode::FilterFreq,
        CalibrationErrorCode::FilterQ,
        CalibrationErrorCode::FilterGain,
        CalibrationErrorCode::FilterTypeUnknown,
        CalibrationErrorCode::FilterTypeNotInV1,
        CalibrationErrorCode::PositiveBoost,
        CalibrationErrorCode::Measured,
        CalibrationErrorCode::LayoutMismatch,
        CalibrationErrorCode::LayoutRoleUnknown,
        CalibrationErrorCode::LayoutIndexMismatch,
        CalibrationErrorCode::RoleNotFound,
        CalibrationErrorCode::RoleIndexMismatch,
        CalibrationErrorCode::DoubleApply,
        CalibrationErrorCode::BaseRoleInvalid,
        CalibrationErrorCode::PeqOverflow,
        CalibrationErrorCode::DelayOverflow,
        CalibrationErrorCode::MergedGainRange,
        CalibrationErrorCode::FsMismatch,
        CalibrationErrorCode::IrProfile,
        CalibrationErrorCode::IrRateUnsupported,
        CalibrationErrorCode::IrRoles,
        CalibrationErrorCode::IrStereoMask,
        CalibrationErrorCode::IrCurveDoubleApply,
        CalibrationErrorCode::IrCurveHash,
        CalibrationErrorCode::IrCurveFs,
        CalibrationErrorCode::IrCurvePhase,
        CalibrationErrorCode::IrCurvePoints,
        CalibrationErrorCode::IrGCurve,
        CalibrationErrorCode::IrFieldUnknown,
        CalibrationErrorCode::IrExtensionDuplicate,
        CalibrationErrorCode::IrDelayPlan,
        CalibrationErrorCode::IrEncodingConflict,
        CalibrationErrorCode::IrTapsBudget,
        CalibrationErrorCode::IrCapacity,
        CalibrationErrorCode::IrCompileOptions,
        CalibrationErrorCode::IrRateMismatch,
    ];

    /// `E_CAL_*` / `E_IR_*` 字面值。
    pub fn as_str(self) -> &'static str {
        match self {
            CalibrationErrorCode::Encoding => "E_CAL_ENCODING",
            CalibrationErrorCode::Io => "E_CAL_IO",
            CalibrationErrorCode::YamlSyntax => "E_CAL_YAML_SYNTAX",
            CalibrationErrorCode::YamlUnsupported => "E_CAL_YAML_UNSUPPORTED",
            CalibrationErrorCode::YamlRoot => "E_CAL_YAML_ROOT",
            CalibrationErrorCode::Schema => "E_CAL_SCHEMA",
            CalibrationErrorCode::MissingField => "E_CAL_MISSING_FIELD",
            CalibrationErrorCode::FieldType => "E_CAL_FIELD_TYPE",
            CalibrationErrorCode::Channels => "E_CAL_CHANNELS",
            CalibrationErrorCode::ChannelName => "E_CAL_NAME",
            CalibrationErrorCode::ChannelIndex => "E_CAL_INDEX",
            CalibrationErrorCode::SampleRate => "E_CAL_SAMPLE_RATE",
            CalibrationErrorCode::ReferenceIndex => "E_CAL_REFERENCE_INDEX",
            CalibrationErrorCode::Layout => "E_CAL_LAYOUT",
            CalibrationErrorCode::Metadata => "E_CAL_METADATA",
            CalibrationErrorCode::Measurement => "E_CAL_MEASUREMENT",
            CalibrationErrorCode::Delay => "E_CAL_DELAY",
            CalibrationErrorCode::DelayNormalization => "E_CAL_DELAY_NORMALIZATION",
            CalibrationErrorCode::Polarity => "E_CAL_POLARITY",
            CalibrationErrorCode::TrimGain => "E_CAL_TRIM_GAIN",
            CalibrationErrorCode::Filters => "E_CAL_FILTERS",
            CalibrationErrorCode::FilterCount => "E_CAL_FILTER_COUNT",
            CalibrationErrorCode::FilterField => "E_CAL_FILTER_FIELD",
            CalibrationErrorCode::FilterFreq => "E_CAL_FILTER_FREQ",
            CalibrationErrorCode::FilterQ => "E_CAL_FILTER_Q",
            CalibrationErrorCode::FilterGain => "E_CAL_FILTER_GAIN",
            CalibrationErrorCode::FilterTypeUnknown => "E_CAL_FILTER_TYPE_UNKNOWN",
            CalibrationErrorCode::FilterTypeNotInV1 => "E_CAL_FILTER_TYPE_NOT_IN_V1",
            CalibrationErrorCode::PositiveBoost => "E_CAL_POSITIVE_BOOST",
            CalibrationErrorCode::Measured => "E_CAL_MEASURED",
            CalibrationErrorCode::LayoutMismatch => "E_CAL_LAYOUT_MISMATCH",
            CalibrationErrorCode::LayoutRoleUnknown => "E_CAL_LAYOUT_ROLE_UNKNOWN",
            CalibrationErrorCode::LayoutIndexMismatch => "E_CAL_LAYOUT_INDEX_MISMATCH",
            CalibrationErrorCode::RoleNotFound => "E_CAL_ROLE_NOT_FOUND",
            CalibrationErrorCode::RoleIndexMismatch => "E_CAL_ROLE_INDEX_MISMATCH",
            CalibrationErrorCode::DoubleApply => "E_CAL_DOUBLE_APPLY",
            CalibrationErrorCode::BaseRoleInvalid => "E_CAL_BASE_ROLE_INVALID",
            CalibrationErrorCode::PeqOverflow => "E_CAL_PEQ_OVERFLOW",
            CalibrationErrorCode::DelayOverflow => "E_CAL_DELAY_OVERFLOW",
            CalibrationErrorCode::MergedGainRange => "E_CAL_MERGED_GAIN_RANGE",
            CalibrationErrorCode::FsMismatch => "E_CAL_FS_MISMATCH",
            CalibrationErrorCode::IrProfile => "E_IR_PROFILE",
            CalibrationErrorCode::IrRateUnsupported => "E_IR_RATE_UNSUPPORTED",
            CalibrationErrorCode::IrRoles => "E_IR_ROLES",
            CalibrationErrorCode::IrStereoMask => "E_IR_STEREO_MASK",
            CalibrationErrorCode::IrCurveDoubleApply => "E_IR_CURVE_DOUBLE_APPLY",
            CalibrationErrorCode::IrCurveHash => "E_IR_CURVE_HASH",
            CalibrationErrorCode::IrCurveFs => "E_IR_CURVE_FS",
            CalibrationErrorCode::IrCurvePhase => "E_IR_CURVE_PHASE",
            CalibrationErrorCode::IrCurvePoints => "E_IR_CURVE_POINTS",
            CalibrationErrorCode::IrGCurve => "E_IR_G_CURVE",
            CalibrationErrorCode::IrFieldUnknown => "E_IR_FIELD_UNKNOWN",
            CalibrationErrorCode::IrExtensionDuplicate => "E_IR_EXTENSION_DUPLICATE",
            CalibrationErrorCode::IrDelayPlan => "E_IR_DELAY_PLAN",
            CalibrationErrorCode::IrEncodingConflict => "E_IR_ENCODING_CONFLICT",
            CalibrationErrorCode::IrTapsBudget => "E_IR_TAPS_BUDGET",
            CalibrationErrorCode::IrCapacity => "E_IR_CAPACITY",
            CalibrationErrorCode::IrCompileOptions => "E_IR_COMPILE_OPTIONS",
            CalibrationErrorCode::IrRateMismatch => "E_IR_RATE_MISMATCH",
        }
    }
}

/// 校准适配错误（`field` 是字段路径，如 `channels[2].filters[0].q`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalibrationError {
    /// 错误码。
    pub code: CalibrationErrorCode,
    /// 字段路径（无则 `None`）。
    pub field: Option<String>,
    /// 人读细节（含期望/实际值）。
    pub detail: String,
}

impl CalibrationError {
    /// 构造。
    pub fn new(code: CalibrationErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            field: None,
            detail: detail.into(),
        }
    }

    /// 带字段路径构造。
    pub fn with_field(
        code: CalibrationErrorCode,
        field: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            code,
            field: Some(field.into()),
            detail: detail.into(),
        }
    }

    /// 码字面值。
    pub fn code_str(&self) -> &'static str {
        self.code.as_str()
    }
}

impl fmt::Display for CalibrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.field {
            Some(field) => write!(f, "{} [{}]: {}", self.code.as_str(), field, self.detail),
            None => write!(f, "{}: {}", self.code.as_str(), self.detail),
        }
    }
}

impl std::error::Error for CalibrationError {}

/// 警告码（CONTRACT：error=拒绝，warning=接受但记录；警告不得参与应用语义）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CalibrationWarningCode {
    /// 未知顶层字段（CONTRACT §5 规则 12：warning；扩展性保留，不参与应用）。
    UnknownTopField,
    /// 未知 channel 字段（warning）。
    UnknownChannelField,
    /// 未知 filter 字段（warning）。
    UnknownFilterField,
    /// 未知 measurement 字段（warning）。
    UnknownMeasurementField,
    /// 重复字段（保留后出现值；warning）。
    DuplicateField,
    /// polarity 缺省（默认 normal；warning）。
    PolarityDefault,
    /// polarity 使用 ±1 别名（warning；归一化）。
    PolarityAlias,
    /// sample_rate 不在声明集合 {44100,48000,88200,96000}（warning）。
    SampleRateNonDeclared,
    /// v1.1 peaking q>10（契约合法；channelmap 路线会拒收，P1 modal PEQ 可用）。
    PeakingQHigh,
    /// v1.1 文件只含 Peaking 且 Q≤10：v1 消费方可直接读取。
    V11ReadableByV1,
    /// reference_index 指向 delay_samples≠0 的通道（建议指向 D=0 的到达最晚通道）。
    ReferenceNotZeroDelay,
    /// 单通道 PEQ 正向提升 >6 dB（未到 12 dB 拒收阈值）。
    PositiveBoostWarn,
    /// `channels[].measured` 不是 mapping（诊断字段，忽略）。
    MeasuredNotMapping,
    /// note 超过 500 字符。
    NoteTooLong,
    /// layout 角色未全部校准（partial calibration，合法）。
    LayoutRoleUncalibrated,
    /// 合并时渲染基线角色未被校准覆盖（partial calibration，合法）。
    RoleUncalibrated,
    /// schema 兼容性提示（v1.1 文件按 v1 语义读取的风险）。
    SchemaCompatibility,
}

impl CalibrationWarningCode {
    /// 全部警告码（测试覆盖 100%）。
    pub const ALL: &'static [CalibrationWarningCode] = &[
        CalibrationWarningCode::UnknownTopField,
        CalibrationWarningCode::UnknownChannelField,
        CalibrationWarningCode::UnknownFilterField,
        CalibrationWarningCode::UnknownMeasurementField,
        CalibrationWarningCode::DuplicateField,
        CalibrationWarningCode::PolarityDefault,
        CalibrationWarningCode::PolarityAlias,
        CalibrationWarningCode::SampleRateNonDeclared,
        CalibrationWarningCode::PeakingQHigh,
        CalibrationWarningCode::V11ReadableByV1,
        CalibrationWarningCode::ReferenceNotZeroDelay,
        CalibrationWarningCode::PositiveBoostWarn,
        CalibrationWarningCode::MeasuredNotMapping,
        CalibrationWarningCode::NoteTooLong,
        CalibrationWarningCode::LayoutRoleUncalibrated,
        CalibrationWarningCode::RoleUncalibrated,
        CalibrationWarningCode::SchemaCompatibility,
    ];

    /// 警告码字面值。
    pub fn as_str(self) -> &'static str {
        match self {
            CalibrationWarningCode::UnknownTopField => "W_CAL_UNKNOWN_TOP_FIELD",
            CalibrationWarningCode::UnknownChannelField => "W_CAL_UNKNOWN_CHANNEL_FIELD",
            CalibrationWarningCode::UnknownFilterField => "W_CAL_UNKNOWN_FILTER_FIELD",
            CalibrationWarningCode::UnknownMeasurementField => "W_CAL_UNKNOWN_MEASUREMENT_FIELD",
            CalibrationWarningCode::DuplicateField => "W_CAL_DUPLICATE_FIELD",
            CalibrationWarningCode::PolarityDefault => "W_CAL_POLARITY_DEFAULT",
            CalibrationWarningCode::PolarityAlias => "W_CAL_POLARITY_ALIAS",
            CalibrationWarningCode::SampleRateNonDeclared => "W_CAL_SAMPLE_RATE_NON_DECLARED",
            CalibrationWarningCode::PeakingQHigh => "W_CAL_PEAKING_Q_HIGH",
            CalibrationWarningCode::V11ReadableByV1 => "W_CAL_V11_READABLE_BY_V1",
            CalibrationWarningCode::ReferenceNotZeroDelay => "W_CAL_REFERENCE_NOT_ZERO_DELAY",
            CalibrationWarningCode::PositiveBoostWarn => "W_CAL_POSITIVE_BOOST_WARN",
            CalibrationWarningCode::MeasuredNotMapping => "W_CAL_MEASURED_NOT_MAPPING",
            CalibrationWarningCode::NoteTooLong => "W_CAL_NOTE_TOO_LONG",
            CalibrationWarningCode::LayoutRoleUncalibrated => "W_CAL_LAYOUT_ROLE_UNCALIBRATED",
            CalibrationWarningCode::RoleUncalibrated => "W_CAL_ROLE_UNCALIBRATED",
            CalibrationWarningCode::SchemaCompatibility => "W_CAL_SCHEMA_COMPATIBILITY",
        }
    }
}

/// 一条警告。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalibrationWarning {
    /// 警告码。
    pub code: CalibrationWarningCode,
    /// 字段路径（无则 `None`）。
    pub field: Option<String>,
    /// 人读细节。
    pub detail: String,
}

impl CalibrationWarning {
    fn new(
        code: CalibrationWarningCode,
        field: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            code,
            field: Some(field.into()),
            detail: detail.into(),
        }
    }

    /// 码字面值。
    pub fn code_str(&self) -> &'static str {
        self.code.as_str()
    }
}

type CalResult<T> = Result<T, CalibrationError>;

fn err(code: CalibrationErrorCode, detail: impl Into<String>) -> CalibrationError {
    CalibrationError::new(code, detail)
}

fn errf(
    code: CalibrationErrorCode,
    field: impl Into<String>,
    detail: impl Into<String>,
) -> CalibrationError {
    CalibrationError::with_field(code, field, detail)
}

// ===========================================================================
// 领域类型（CONTRACT §2/§3/§10）
// ===========================================================================

/// P_post 规范链阶段（fir-toolbox §5.1；校准 adapter 用它记录 placement）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PostStage {
    /// 输出分频 HP/LP（T39；校准不产生）。
    CrossoverHpLp,
    /// 模态/宽带 PEQ（T41/T64；校准 filters 的唯一应用点）。
    ModalPeq,
    /// 输出目标 FIR（T37/T55；目标曲线 curve 的唯一应用点）。
    TargetFir,
    /// 角色 gain（role gain；校准 trim 合并后一次应用）。
    Gain,
    /// 整数延迟（delay_samples；coefficient-embedded 或 role 字段二选一）。
    Delay,
    /// 极性（XOR；coefficient-embedded 或 role 字段二选一）。
    Polarity,
    /// 最终保护/limiter（T23；校准不改）。
    Protection,
}

impl PostStage {
    /// 全部阶段（规范链序）。
    pub const ALL: [PostStage; 7] = POST_PLACEMENT_CHAIN;

    /// 阶段 id（IR 规范化序列化用）。
    pub fn as_str(self) -> &'static str {
        match self {
            PostStage::CrossoverHpLp => "crossover_hp_lp",
            PostStage::ModalPeq => "modal_peq",
            PostStage::TargetFir => "target_fir",
            PostStage::Gain => "gain",
            PostStage::Delay => "delay",
            PostStage::Polarity => "polarity",
            PostStage::Protection => "protection",
        }
    }

    /// 规范链序下标（用于校验 placement 顺序）。
    pub fn chain_index(self) -> usize {
        match self {
            PostStage::CrossoverHpLp => 0,
            PostStage::ModalPeq => 1,
            PostStage::TargetFir => 2,
            PostStage::Gain => 3,
            PostStage::Delay => 4,
            PostStage::Polarity => 5,
            PostStage::Protection => 6,
        }
    }
}

/// 校准文件 schema 版本（v1/v1.1；字段语义由 CONTRACT 冻结）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CalibrationSchema {
    /// `tspi-calibration/v1`（filters 只允许 peaking，Q≤10）。
    V1,
    /// `tspi-calibration/v1.1`（新增 low_shelf/high_shelf；peaking q 无上限）。
    V11,
}

impl CalibrationSchema {
    /// 从字面值解析（未知= None；消费方必须整份拒收）。
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            CAL_SCHEMA_V1 => Some(CalibrationSchema::V1),
            CAL_SCHEMA_V11 => Some(CalibrationSchema::V11),
            _ => None,
        }
    }

    /// 字面值。
    pub fn as_str(self) -> &'static str {
        match self {
            CalibrationSchema::V1 => CAL_SCHEMA_V1,
            CalibrationSchema::V11 => CAL_SCHEMA_V11,
        }
    }
}

/// PEQ 段类型（v1 只 peaking；v1.1 另允许 shelf；未知 type 整份拒收）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PeqType {
    /// RBJ peaking；`q` 是 Q。
    Peaking,
    /// RBJ low shelf；`q` 承载 slope S（不是 Q）。
    LowShelf,
    /// RBJ high shelf；`q` 承载 slope S（不是 Q）。
    HighShelf,
}

impl PeqType {
    /// 全部已知 type。
    pub const ALL: [PeqType; 3] = [PeqType::Peaking, PeqType::LowShelf, PeqType::HighShelf];

    /// 从字面值解析（未知= None；调用方必须拒收整份文件，不做 peaking 降级）。
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "peaking" => Some(PeqType::Peaking),
            "low_shelf" => Some(PeqType::LowShelf),
            "high_shelf" => Some(PeqType::HighShelf),
            _ => None,
        }
    }

    /// 字面值。
    pub fn as_str(self) -> &'static str {
        match self {
            PeqType::Peaking => "peaking",
            PeqType::LowShelf => "low_shelf",
            PeqType::HighShelf => "high_shelf",
        }
    }

    /// 是否 shelf（q 字段语义为 RBJ slope S）。
    pub fn is_shelf(self) -> bool {
        matches!(self, PeqType::LowShelf | PeqType::HighShelf)
    }
}

/// 极性（CONTRACT §3.2；兼容别名 1/-1 归一化，应用时 XOR）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Polarity {
    /// 不反相（+1）。
    Normal,
    /// 反相（乘 -1，180°）。
    Invert,
}

impl Polarity {
    /// 从字面值解析；兼容 `1`/`-1` 别名（调用方记录 warning）。
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "normal" => Some(Polarity::Normal),
            "invert" => Some(Polarity::Invert),
            _ => None,
        }
    }

    /// 字面值。
    pub fn as_str(self) -> &'static str {
        match self {
            Polarity::Normal => "normal",
            Polarity::Invert => "invert",
        }
    }

    /// 极性乘子（+1/-1）。
    pub fn multiplier(self) -> i8 {
        match self {
            Polarity::Normal => 1,
            Polarity::Invert => -1,
        }
    }

    /// 应用合并规则：`XOR(render, calibration invert)`（CONTRACT §3.2/§4.1）。
    pub fn xor(self, other: Polarity) -> Polarity {
        if self == other {
            Polarity::Normal
        } else {
            Polarity::Invert
        }
    }
}

/// 一条 PEQ 段（已校验；type 原样保留，禁止降级）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeqBand {
    /// 中心/转折频率（Hz）。
    pub freq_hz: f64,
    /// peaking 为 Q；shelf 为 RBJ slope S（CONTRACT §3.4，字段名仍是 q）。
    pub q: f64,
    /// 段增益（dB；[-24,+24]）。
    pub gain_db: f64,
    /// 段类型。
    pub filter_type: PeqType,
}

impl PeqBand {
    /// 构造（调用方保证已按 CONTRACT 校验；本构造不做校验）。
    pub fn new(freq_hz: f64, q: f64, gain_db: f64, filter_type: PeqType) -> Self {
        Self {
            freq_hz,
            q,
            gain_db,
            filter_type,
        }
    }
}

/// 校准文件里的 PEQ 段（保留「type 是否显式给出」以便 v1 兼容性报告）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CalibrationFilter {
    /// 已校验段。
    pub band: PeqBand,
    /// 文件中是否显式写了 `type`（缺省=peaking）。
    pub type_explicit: bool,
}

/// 渲染基线角色来源标记（防止同一校准被二次应用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RoleSpecOrigin {
    /// 未应用校准的渲染基线（profile/overlay 的角色值）。
    Uncalibrated,
    /// 已经应用过校准的角色值；再次应用必须拒收（避免双叠）。
    CalibrationApplied,
}

/// 渲染基线角色（来自 overlay/channelmap profile；T25/T57 解析后提供）。
#[derive(Clone, Debug, PartialEq)]
pub struct RenderRoleBase {
    /// 角色 id（如 FL/SUB1；layout-presets 大写）。
    pub name: String,
    /// 播放设备物理通道号（0-based；`channels[].index` 域）。
    pub index: u32,
    /// 渲染层已有的整数延迟（samples；CONTRACT §4.1 相加）。
    pub delay_samples: u32,
    /// 渲染层已有极性（XOR）。
    pub polarity: Polarity,
    /// 渲染层已有 gain（dB；与校准 trim 相加后须在 [-60,+12]）。
    pub gain_db: f64,
    /// 渲染层已有 PEQ 链（按序追加校准段；type 原样保留）。
    pub peq: Vec<PeqBand>,
    /// 来源标记（`CalibrationApplied` 时禁止再次合并）。
    pub origin: RoleSpecOrigin,
}

impl RenderRoleBase {
    /// 以默认值构造未校准基线角色（gain=0、无 PEQ、normal、delay=0）。
    pub fn uncalibrated(name: impl Into<String>, index: u32) -> Self {
        Self {
            name: name.into(),
            index,
            delay_samples: 0,
            polarity: Polarity::Normal,
            gain_db: 0.0,
            peq: Vec::new(),
            origin: RoleSpecOrigin::Uncalibrated,
        }
    }
}

/// 一个校准通道（只读解析结果；文件内容不改）。
#[derive(Clone, Debug, PartialEq)]
pub struct CalibrationChannel {
    /// 角色 id。
    pub name: String,
    /// 播放设备物理通道号。
    pub index: u32,
    /// 延迟补偿（samples，整数；D_i = round(max t - t_i)，min=0）。
    pub delay_samples: u32,
    /// 极性。
    pub polarity: Polarity,
    /// 极性是否用了 ±1 别名（warning 已记录）。
    pub polarity_alias: bool,
    /// trim（dB；缺省 None 等价 0）。
    pub gain_db: Option<f64>,
    /// PEQ 段（按文件顺序；type 已校验）。
    pub filters: Vec<CalibrationFilter>,
    /// 是否带 `measured` 证据块（只记录存在性；内容不参与应用）。
    pub has_measured: bool,
}

/// `tool` 字段（string 或 {name,version} 两种形式都兼容）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalibrationTool {
    /// 工具名。
    pub name: String,
    /// 工具版本（字符串化；数字也转字符串）。
    pub version: Option<String>,
}

/// 解析后的校准文档（只读视图；CONTRACT §2 字段语义不改）。
#[derive(Clone, Debug, PartialEq)]
pub struct CalibrationDocument {
    /// schema 版本。
    pub schema: CalibrationSchema,
    /// schema 原始字面值（保留 v1/v1.1 原样）。
    pub schema_string: String,
    /// 采样率（Hz）。
    pub sample_rate: u32,
    /// 参考通道的 index（必须是 channels[].index 之一）。
    pub reference_index: u32,
    /// layout id（可选；建议必填，供映射交叉校验）。
    pub layout: Option<String>,
    /// 创建时间（可选）。
    pub created_unix: Option<u64>,
    /// 工具信息（可选）。
    pub tool: Option<CalibrationTool>,
    /// 顶层 version 字符串（可选）。
    pub tool_version: Option<String>,
    /// 人读备注（可选）。
    pub note: Option<String>,
    /// 通道列表（文件顺序；消费者按 name 匹配角色）。
    pub channels: Vec<CalibrationChannel>,
    /// 归一化后的 warning（不参与应用）。
    pub warnings: Vec<CalibrationWarning>,
    /// 原始文件字节 SHA-256（小写 hex；IR 的 source hash 不变式）。
    pub source_sha256: String,
    /// 解析时是否给出了 layout 角色表（决定 layout 交叉校验是否执行）。
    pub layout_checked: bool,
}

impl CalibrationDocument {
    /// 是否为 v1 文件（v1.1 消费者需注意 shelf/q 语义）。
    pub fn is_v1(&self) -> bool {
        self.schema == CalibrationSchema::V1
    }

    /// 按 name 查通道。
    pub fn channel(&self, name: &str) -> Option<&CalibrationChannel> {
        self.channels.iter().find(|channel| channel.name == name)
    }

    /// 通道的 trim（缺省 0 dB）。
    pub fn trim_db(&self, name: &str) -> Option<f64> {
        self.channel(name)
            .map(|channel| channel.gain_db.unwrap_or(0.0))
    }

    /// 延迟上限样本数（floor(0.2 * fs)；CONTRACT §3.1）。
    pub fn delay_max_samples(&self) -> u32 {
        delay_max_samples(self.sample_rate)
    }
}

/// `floor(0.2 * fs)`：calibration delay_samples 上限（样本）。
pub fn delay_max_samples(sample_rate_hz: u32) -> u32 {
    (CAL_DELAY_MAX_MS / 1000.0 * sample_rate_hz as f64).floor() as u32
}

/// layout 角色表项（T57 的 overlay 解析结果；本模块只消费）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutRole {
    /// 角色 id。
    pub id: String,
    /// 该角色可用的播放输出（0-based；首元素用于 index 交叉校验）。
    pub outputs: Vec<u16>,
}

/// layout 角色表（layout id + 角色顺序）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutRoleTable {
    /// layout id（如 `4.1.2`/`5.3.2`）。
    pub layout_id: String,
    /// 角色表（顺序即唯一映射表顺序；`ch6` 空场景不列入角色）。
    pub roles: Vec<LayoutRole>,
}

impl LayoutRoleTable {
    /// 构造。
    pub fn new(layout_id: impl Into<String>, roles: Vec<LayoutRole>) -> Self {
        Self {
            layout_id: layout_id.into(),
            roles,
        }
    }

    /// 角色 id 是否存在。
    pub fn contains(&self, name: &str) -> bool {
        self.roles.iter().any(|role| role.id == name)
    }

    /// 角色首个输出通道（交叉校验用）。
    pub fn first_output(&self, name: &str) -> Option<u16> {
        self.roles
            .iter()
            .find(|role| role.id == name)
            .and_then(|role| role.outputs.first().copied())
    }
}

// ===========================================================================
// SHA-256（纯 std；source/IR hash）
// ===========================================================================

/// SHA-256（FIPS 180-4，32 字节）。
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

/// SHA-256 小写十六进制。
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

// ===========================================================================
// 极简 YAML 子集解析器（纯 std；CONTRACT 校准文件只需要 block/flow mapping+sequence）
// ===========================================================================
//
// 支持：缩进 block mapping / block sequence、单行 flow mapping `{...}` / flow sequence
// `[...]`、plain/single-quoted/double-quoted 标量、`#` 注释、`---`/`...` 文档标记。
// 明确**不支持**（遇到即 E_CAL_YAML_UNSUPPORTED，不猜）：anchor/alias（`&`/`*`）、tag（`!`）、
// block scalar（`|`/`>`）、多行 plain/flow 标量、多文档（第二个 `---` 后拒绝）、tab 缩进。
// 值类型按 YAML 1.1/1.2 常用子集推断：null/~、true/false、整数、浮点、字符串。

/// YAML 子集值（保持 mapping 键序，供未知字段 warning/重复字段检测）。
#[derive(Clone, Debug, PartialEq)]
enum Yaml {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Seq(Vec<Yaml>),
    Map(Vec<(String, Yaml)>),
}

impl Yaml {
    fn type_name(&self) -> &'static str {
        match self {
            Yaml::Null => "null",
            Yaml::Bool(_) => "bool",
            Yaml::Int(_) => "int",
            Yaml::Float(_) => "float",
            Yaml::Str(_) => "string",
            Yaml::Seq(_) => "sequence",
            Yaml::Map(_) => "mapping",
        }
    }

    fn as_map(&self) -> Option<&[(String, Yaml)]> {
        match self {
            Yaml::Map(fields) => Some(fields),
            _ => None,
        }
    }

    fn as_seq(&self) -> Option<&[Yaml]> {
        match self {
            Yaml::Seq(items) => Some(items),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Yaml::Str(value) => Some(value),
            _ => None,
        }
    }

    fn as_i64(&self) -> Option<i64> {
        match self {
            Yaml::Int(value) => Some(*value),
            _ => None,
        }
    }

    /// 数值视图：整数与有限浮点都接受（CONTRACT 中 freq/gain/q 允许两种写法）。
    fn as_f64(&self) -> Option<f64> {
        match self {
            Yaml::Int(value) => Some(*value as f64),
            Yaml::Float(value) if value.is_finite() => Some(*value),
            _ => None,
        }
    }
}

/// 一行预处理后的 YAML 内容（去除注释/尾空白）。
#[derive(Clone, Debug)]
struct YamlLine {
    number: usize,
    indent: usize,
    text: String,
}

fn yaml_err(line: usize, detail: impl Into<String>) -> CalibrationError {
    errf(
        CalibrationErrorCode::YamlSyntax,
        format!("line {line}"),
        detail.into(),
    )
}

fn yaml_unsupported(line: usize, detail: impl Into<String>) -> CalibrationError {
    errf(
        CalibrationErrorCode::YamlUnsupported,
        format!("line {line}"),
        detail.into(),
    )
}

/// 去除注释（引号外的 `#`；`#` 必须在行首或前面是空白才算注释）并 trim 行尾。
fn strip_yaml_comment(line: &str) -> Result<&str, bool> {
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut chars = line.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if in_double {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_double = false;
            }
            continue;
        }
        if in_single {
            if ch == '\'' {
                if matches!(chars.peek(), Some((_, '\''))) {
                    chars.next();
                } else {
                    in_single = false;
                }
            }
            continue;
        }
        match ch {
            '"' => in_double = true,
            '\'' => in_single = true,
            '#' => {
                let at_start = index == 0;
                let prev_blank = line[..index]
                    .chars()
                    .last()
                    .is_some_and(|prev| prev == ' ' || prev == '\t');
                if at_start || prev_blank {
                    return Ok(line[..index].trim_end());
                }
            }
            _ => {}
        }
    }
    if in_single || in_double {
        return Err(false);
    }
    Ok(line.trim_end())
}

/// 预处理：UTF-8 -> 行列表（去注释/跳空行/文档标记；tab 缩进拒绝）。
fn preprocess_yaml(bytes: &[u8]) -> CalResult<Vec<YamlLine>> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        err(
            CalibrationErrorCode::Encoding,
            format!("校准文件不是 UTF-8：{error}"),
        )
    })?;
    let mut lines = Vec::new();
    let mut document_count = 0usize;
    for (index, raw) in text.split('\n').enumerate() {
        let number = index + 1;
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if raw.contains('\t') && raw[..raw.len() - raw.trim_start().len()].contains('\t') {
            return Err(yaml_unsupported(
                number,
                "缩进中的 TAB 不受支持（YAML 禁止）",
            ));
        }
        let trimmed_end = strip_yaml_comment(raw)
            .map_err(|_| yaml_unsupported(number, "多行引号字符串/未闭合引号不受支持"))?;
        if trimmed_end.trim().is_empty() {
            continue;
        }
        let indent = trimmed_end.len() - trimmed_end.trim_start_matches(' ').len();
        let body = trimmed_end[indent..].to_string();
        if indent == 0 && (body == "---" || body.starts_with("--- ")) {
            document_count += 1;
            if document_count > 1 {
                return Err(yaml_unsupported(number, "多文档 YAML 不受支持"));
            }
            continue;
        }
        if indent == 0 && body == "..." {
            continue;
        }
        lines.push(YamlLine {
            number,
            indent,
            text: body,
        });
    }
    Ok(lines)
}

/// 把一个标量 token 推断成 YAML 值（带引号时调用方已剥离引号，此处不接收引号）。
fn infer_yaml_scalar(token: &str) -> Yaml {
    let trimmed = token.trim();
    if trimmed.is_empty()
        || trimmed == "~"
        || trimmed == "null"
        || trimmed == "Null"
        || trimmed == "NULL"
    {
        return Yaml::Null;
    }
    if trimmed == "true" || trimmed == "True" || trimmed == "TRUE" {
        return Yaml::Bool(true);
    }
    if trimmed == "false" || trimmed == "False" || trimmed == "FALSE" {
        return Yaml::Bool(false);
    }
    // 纯整数（允许 +/- 前缀；下划线分隔不在支持范围，会落到 Float 解析失败 -> Str）。
    let digits = trimmed
        .strip_prefix('+')
        .or_else(|| trimmed.strip_prefix('-'))
        .unwrap_or(trimmed);
    if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
        if let Ok(value) = trimmed.parse::<i64>() {
            return Yaml::Int(value);
        }
    }
    // 数值 token：只含数字/小数点/指数符号时按 f64 解析；失败按字符串。
    let numeric_chars = trimmed
        .bytes()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b'e' | b'E' | b'+' | b'-'));
    if numeric_chars && trimmed.bytes().any(|byte| byte.is_ascii_digit()) {
        if let Ok(value) = trimmed.parse::<f64>() {
            return Yaml::Float(value);
        }
    }
    Yaml::Str(trimmed.to_string())
}

/// 单引号字符串：内部 `''` 表示一个 `'`。
fn parse_single_quoted(text: &str, pos: &mut usize, line: usize) -> CalResult<String> {
    let bytes = text.as_bytes();
    debug_assert_eq!(bytes.get(*pos), Some(&b'\''));
    *pos += 1;
    let mut out = String::new();
    while *pos < bytes.len() {
        let ch = text[*pos..].chars().next().expect("char boundary");
        *pos += ch.len_utf8();
        if ch == '\'' {
            if bytes.get(*pos) == Some(&b'\'') {
                out.push('\'');
                *pos += 1;
                continue;
            }
            return Ok(out);
        }
        out.push(ch);
    }
    Err(yaml_err(line, "单引号字符串未闭合"))
}

/// 双引号字符串：支持 `\"` `\\` `\/` `\n` `\r` `\t` `\uXXXX`；其它转义拒绝。
fn parse_double_quoted(text: &str, pos: &mut usize, line: usize) -> CalResult<String> {
    let bytes = text.as_bytes();
    debug_assert_eq!(bytes.get(*pos), Some(&b'"'));
    *pos += 1;
    let mut out = String::new();
    while *pos < bytes.len() {
        let ch = text[*pos..].chars().next().expect("char boundary");
        *pos += ch.len_utf8();
        if ch == '"' {
            return Ok(out);
        }
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        if *pos >= bytes.len() {
            return Err(yaml_err(line, "双引号字符串转义未完成"));
        }
        let escape = text[*pos..].chars().next().expect("char boundary");
        *pos += escape.len_utf8();
        match escape {
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            '/' => out.push('/'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            '0' => out.push('\0'),
            'u' => {
                if *pos + 4 > bytes.len() {
                    return Err(yaml_err(line, "\\u 转义需要 4 位十六进制"));
                }
                let hex = &text[*pos..*pos + 4];
                let code = u32::from_str_radix(hex, 16)
                    .map_err(|_| yaml_err(line, format!("\\u 转义非十六进制：{hex}")))?;
                *pos += 4;
                let decoded = char::from_u32(code)
                    .ok_or_else(|| yaml_err(line, format!("\\u 转义非合法码点：{hex}")))?;
                out.push(decoded);
            }
            other => {
                return Err(yaml_unsupported(
                    line,
                    format!("不支持的 YAML 双引号转义：\\{other}"),
                ))
            }
        }
    }
    Err(yaml_err(line, "双引号字符串未闭合"))
}

/// 解析单行内联值：flow 集合或标量；必须消费完整行。
fn parse_inline_value(text: &str, line: usize) -> CalResult<Yaml> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Yaml::Null);
    }
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        let mut pos = 0usize;
        let value = parse_flow_value(trimmed, &mut pos, line)?;
        skip_flow_ws(trimmed, &mut pos);
        if pos != trimmed.len() {
            return Err(yaml_err(line, "flow 集合后存在多余内容"));
        }
        return Ok(value);
    }
    if trimmed.starts_with('&') || trimmed.starts_with('*') || trimmed.starts_with('!') {
        return Err(yaml_unsupported(line, "anchor/alias/tag 不受支持"));
    }
    if trimmed.starts_with('|') || trimmed.starts_with('>') {
        return Err(yaml_unsupported(line, "block scalar（| / >）不受支持"));
    }
    // 引号标量优先：其内容可以合法包含 "; " / "#"，不走嵌套 mapping 判定。
    if trimmed.starts_with('"') || trimmed.starts_with('\'') {
        let mut pos = 0usize;
        let scalar = parse_flow_scalar(trimmed, &mut pos, line, false)?;
        skip_flow_ws(trimmed, &mut pos);
        if pos != trimmed.len() {
            return Err(yaml_err(line, "引号标量后存在多余内容"));
        }
        return Ok(scalar);
    }
    if trimmed.contains(": ") {
        return Err(yaml_unsupported(
            line,
            "行内嵌套 mapping（plain 标量里出现 ': '）不受支持",
        ));
    }
    let mut pos = 0usize;
    let scalar = parse_flow_scalar(trimmed, &mut pos, line, false)?;
    skip_flow_ws(trimmed, &mut pos);
    if pos != trimmed.len() {
        return Err(yaml_err(line, "标量后存在多余内容"));
    }
    Ok(scalar)
}

fn skip_flow_ws(text: &str, pos: &mut usize) {
    let bytes = text.as_bytes();
    while *pos < bytes.len() && matches!(bytes[*pos], b' ' | b'\t') {
        *pos += 1;
    }
}

/// flow 标量：引号字符串或 plain token（到 `,` / `}` / `]` / key 模式下的 `:` 结束）。
fn parse_flow_scalar(text: &str, pos: &mut usize, line: usize, is_key: bool) -> CalResult<Yaml> {
    let bytes = text.as_bytes();
    if *pos >= bytes.len() {
        return Err(yaml_err(line, "flow 集合缺少元素"));
    }
    match bytes[*pos] {
        b'"' => Ok(Yaml::Str(parse_double_quoted(text, pos, line)?)),
        b'\'' => Ok(Yaml::Str(parse_single_quoted(text, pos, line)?)),
        b'&' | b'*' | b'!' => Err(yaml_unsupported(line, "anchor/alias/tag 不受支持")),
        _ => {
            let start = *pos;
            while *pos < bytes.len() {
                let byte = bytes[*pos];
                if matches!(byte, b',' | b'}' | b']') {
                    break;
                }
                if is_key && byte == b':' {
                    break;
                }
                *pos += 1;
            }
            let token = text[start..*pos].trim();
            if token.is_empty() {
                return Err(yaml_err(line, "flow 集合中出现空元素/空键"));
            }
            Ok(infer_yaml_scalar(token))
        }
    }
}

/// flow mapping/sequence 递归解析；`pos` 指向 `{` 或 `[`。
fn parse_flow_value(text: &str, pos: &mut usize, line: usize) -> CalResult<Yaml> {
    let bytes = text.as_bytes();
    if *pos >= bytes.len() {
        return Err(yaml_err(line, "flow 集合提前结束"));
    }
    match bytes[*pos] {
        b'{' => {
            *pos += 1;
            let mut fields = Vec::new();
            loop {
                skip_flow_ws(text, pos);
                if bytes.get(*pos) == Some(&b'}') {
                    *pos += 1;
                    return Ok(Yaml::Map(fields));
                }
                let key_value = parse_flow_scalar(text, pos, line, true)?;
                let key = match key_value {
                    Yaml::Str(value) => value,
                    other => {
                        return Err(yaml_err(
                            line,
                            format!("flow mapping 的键必须是字符串，实际 {}", other.type_name()),
                        ))
                    }
                };
                skip_flow_ws(text, pos);
                if bytes.get(*pos) != Some(&b':') {
                    return Err(yaml_err(line, "flow mapping 键后缺 ':'"));
                }
                *pos += 1;
                skip_flow_ws(text, pos);
                let value = parse_flow_value(text, pos, line)?;
                fields.push((key, value));
                skip_flow_ws(text, pos);
                match bytes.get(*pos) {
                    Some(b',') => {
                        *pos += 1;
                    }
                    Some(b'}') => {
                        *pos += 1;
                        return Ok(Yaml::Map(fields));
                    }
                    _ => return Err(yaml_err(line, "flow mapping 缺 ',' 或 '}'")),
                }
            }
        }
        b'[' => {
            *pos += 1;
            let mut items = Vec::new();
            loop {
                skip_flow_ws(text, pos);
                if bytes.get(*pos) == Some(&b']') {
                    *pos += 1;
                    return Ok(Yaml::Seq(items));
                }
                let value = parse_flow_value(text, pos, line)?;
                items.push(value);
                skip_flow_ws(text, pos);
                match bytes.get(*pos) {
                    Some(b',') => {
                        *pos += 1;
                    }
                    Some(b']') => {
                        *pos += 1;
                        return Ok(Yaml::Seq(items));
                    }
                    _ => return Err(yaml_err(line, "flow sequence 缺 ',' 或 ']'")),
                }
            }
        }
        b'"' => Ok(Yaml::Str(parse_double_quoted(text, pos, line)?)),
        b'\'' => Ok(Yaml::Str(parse_single_quoted(text, pos, line)?)),
        b'&' | b'*' | b'!' => Err(yaml_unsupported(line, "anchor/alias/tag 不受支持")),
        _ => parse_flow_scalar(text, pos, line, false),
    }
}

/// 在 `text` 中找 block mapping 的 key/value 分隔符 `:`（引号/flow 深度外，后随空白或行尾）。
/// 返回 (key, value)，均已 trim；key 去掉引号。
fn split_yaml_key(text: &str, line: usize) -> CalResult<Option<(String, &str)>> {
    let bytes = text.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut depth = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        let ch = text[index..].chars().next().expect("char boundary");
        if in_double {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_double = false;
            }
            index += ch.len_utf8();
            continue;
        }
        if in_single {
            if ch == '\'' {
                if bytes.get(index + 1) == Some(&b'\'') {
                    index += 1;
                } else {
                    in_single = false;
                }
            }
            index += ch.len_utf8();
            continue;
        }
        match ch {
            '"' => in_double = true,
            '\'' => in_single = true,
            '{' | '[' => depth += 1,
            '}' | ']' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => {
                let next = bytes.get(index + 1).copied();
                if next.is_none() || next == Some(b' ') || next == Some(b'\t') {
                    let key_raw = text[..index].trim();
                    let rest = &text[index + 1..];
                    if key_raw.is_empty() {
                        return Err(yaml_err(line, "mapping 的键为空"));
                    }
                    let key = unquote_yaml_key(key_raw, line)?;
                    return Ok(Some((key, rest)));
                }
            }
            _ => {}
        }
        index += ch.len_utf8();
    }
    Ok(None)
}

/// block mapping 的 key 去引号（plain 或单/双引号）。
fn unquote_yaml_key(key_raw: &str, line: usize) -> CalResult<String> {
    if key_raw.starts_with('"') {
        let mut pos = 0usize;
        let value = parse_double_quoted(key_raw, &mut pos, line)?;
        if pos != key_raw.len() {
            return Err(yaml_err(line, "mapping 键的双引号后有多余内容"));
        }
        return Ok(value);
    }
    if key_raw.starts_with('\'') {
        let mut pos = 0usize;
        let value = parse_single_quoted(key_raw, &mut pos, line)?;
        if pos != key_raw.len() {
            return Err(yaml_err(line, "mapping 键的单引号后有多余内容"));
        }
        return Ok(value);
    }
    if key_raw.starts_with('&') || key_raw.starts_with('*') || key_raw.starts_with('!') {
        return Err(yaml_unsupported(line, "anchor/alias/tag 不受支持"));
    }
    Ok(key_raw.to_string())
}

/// 解析 block mapping（首行 indent 必须等于 `indent`）。
fn parse_block_map(lines: &[YamlLine], index: &mut usize, indent: usize) -> CalResult<Yaml> {
    let mut fields: Vec<(String, Yaml)> = Vec::new();
    while *index < lines.len() {
        let line = &lines[*index];
        if line.indent < indent {
            break;
        }
        if line.indent > indent {
            return Err(yaml_err(line.number, "缩进不一致（mapping 内缩进变深）"));
        }
        if line.text == "-" || line.text.starts_with("- ") {
            break;
        }
        let (key, rest) = split_yaml_key(&line.text, line.number)?
            .ok_or_else(|| yaml_err(line.number, format!("无法解析 mapping 行：{}", line.text)))?;
        let rest_trimmed = rest.trim();
        *index += 1;
        let value = if rest_trimmed.is_empty() {
            if *index < lines.len() {
                let next = &lines[*index];
                if next.indent > indent {
                    let nested_indent = next.indent;
                    parse_block_node(lines, index, nested_indent)?
                } else if next.indent == indent && (next.text == "-" || next.text.starts_with("- "))
                {
                    parse_block_seq(lines, index, indent)?
                } else {
                    Yaml::Null
                }
            } else {
                Yaml::Null
            }
        } else {
            parse_inline_value(rest_trimmed, line.number)?
        };
        fields.push((key, value));
    }
    Ok(Yaml::Map(fields))
}

/// 解析 block sequence（首行 indent 必须等于 `indent` 且以 `-` 开头）。
fn parse_block_seq(lines: &[YamlLine], index: &mut usize, indent: usize) -> CalResult<Yaml> {
    let mut items: Vec<Yaml> = Vec::new();
    while *index < lines.len() {
        let line = &lines[*index];
        if line.indent < indent {
            break;
        }
        if line.indent > indent {
            return Err(yaml_err(line.number, "缩进不一致（sequence 内缩进变深）"));
        }
        if !(line.text == "-" || line.text.starts_with("- ")) {
            break;
        }
        let body = if line.text == "-" {
            String::new()
        } else {
            line.text[1..].trim_start().to_string()
        };
        let line_number = line.number;
        *index += 1;
        let value = if body.is_empty() {
            if *index < lines.len() && lines[*index].indent > indent {
                let nested_indent = lines[*index].indent;
                parse_block_node(lines, index, nested_indent)?
            } else {
                Yaml::Null
            }
        } else if body.starts_with('{') || body.starts_with('[') {
            parse_inline_value(&body, line_number)?
        } else if body.starts_with("- ") || body == "-" {
            return Err(yaml_unsupported(
                line_number,
                "嵌套 sequence-in-sequence（'- -'）不受支持",
            ));
        } else if let Some((key, rest)) = split_yaml_key(&body, line_number)? {
            // 序列项是 block mapping，且首键写在 '-' 同一行（如 `- name: FL`）。
            let first_rest = rest.trim().to_string();
            let first_value = if first_rest.is_empty() {
                if *index < lines.len() && lines[*index].indent > indent {
                    let nested_indent = lines[*index].indent;
                    parse_block_node(lines, index, nested_indent)?
                } else {
                    Yaml::Null
                }
            } else {
                parse_inline_value(&first_rest, line_number)?
            };
            let mut fields: Vec<(String, Yaml)> = vec![(key, first_value)];
            if *index < lines.len() && lines[*index].indent > indent {
                let nested_indent = lines[*index].indent;
                let nested = parse_block_map(lines, index, nested_indent)?;
                match nested {
                    Yaml::Map(more) => fields.extend(more),
                    other => {
                        return Err(yaml_err(
                            line_number,
                            format!(
                                "sequence 项的续行必须是 mapping，实际 {}",
                                other.type_name()
                            ),
                        ))
                    }
                }
            }
            Yaml::Map(fields)
        } else {
            parse_inline_value(&body, line_number)?
        };
        items.push(value);
    }
    Ok(Yaml::Seq(items))
}

/// 解析一个 block 节点：首行 indent 决定 mapping/sequence。
fn parse_block_node(lines: &[YamlLine], index: &mut usize, indent: usize) -> CalResult<Yaml> {
    if *index >= lines.len() {
        return Ok(Yaml::Null);
    }
    let first = &lines[*index];
    if first.text == "-" || first.text.starts_with("- ") {
        parse_block_seq(lines, index, indent)
    } else {
        parse_block_map(lines, index, indent)
    }
}

/// 解析整份 YAML 文本（只允许单文档）。
fn parse_yaml_document(bytes: &[u8]) -> CalResult<Yaml> {
    let lines = preprocess_yaml(bytes)?;
    if lines.is_empty() {
        return Err(err(
            CalibrationErrorCode::YamlRoot,
            "空文档（缺少 YAML 内容）",
        ));
    }
    if lines[0].indent != 0 {
        return Err(yaml_err(lines[0].number, "顶层节点必须从第 0 列开始"));
    }
    let mut index = 0usize;
    let value = parse_block_node(&lines, &mut index, 0)?;
    if index != lines.len() {
        return Err(yaml_err(
            lines[index].number,
            "文档在顶层节点后仍有内容（顶层只允许一个 mapping）",
        ));
    }
    Ok(value)
}

// ===========================================================================
// 校准文件解析与 CONTRACT §5 校验（v1/v1.1 语义不改）
// ===========================================================================

/// 解析上下文：收集 warning（error 直接返回第一个）。
struct ParseCtx {
    warnings: Vec<CalibrationWarning>,
}

impl ParseCtx {
    fn new() -> Self {
        Self {
            warnings: Vec::new(),
        }
    }

    fn warn(
        &mut self,
        code: CalibrationWarningCode,
        field: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.warnings
            .push(CalibrationWarning::new(code, field, detail));
    }
}

fn field_path(base: &str, key: &str) -> String {
    if base.is_empty() {
        key.to_string()
    } else {
        format!("{base}.{key}")
    }
}

fn index_path(base: &str, index: usize) -> String {
    format!("{base}[{index}]")
}

fn expect_map<'a>(
    value: &'a Yaml,
    field: &str,
    code: CalibrationErrorCode,
) -> CalResult<&'a [(String, Yaml)]> {
    value.as_map().ok_or_else(|| {
        errf(
            code,
            field,
            format!("必须是 mapping，实际 {}", value.type_name()),
        )
    })
}

fn expect_seq<'a>(
    value: &'a Yaml,
    field: &str,
    code: CalibrationErrorCode,
) -> CalResult<&'a [Yaml]> {
    value.as_seq().ok_or_else(|| {
        errf(
            code,
            field,
            format!("必须是数组，实际 {}", value.type_name()),
        )
    })
}

fn expect_str<'a>(value: &'a Yaml, field: &str, code: CalibrationErrorCode) -> CalResult<&'a str> {
    value.as_str().ok_or_else(|| {
        errf(
            code,
            field,
            format!("必须是字符串，实际 {}", value.type_name()),
        )
    })
}

fn expect_num(value: &Yaml, field: &str, code: CalibrationErrorCode) -> CalResult<f64> {
    value.as_f64().ok_or_else(|| {
        errf(
            code,
            field,
            format!("必须是有限数值，实际 {}", value.type_name()),
        )
    })
}

fn expect_u32(value: &Yaml, field: &str, code: CalibrationErrorCode) -> CalResult<u32> {
    match value.as_i64() {
        Some(raw) if raw >= 0 && raw <= u32::MAX as i64 => Ok(raw as u32),
        Some(raw) => Err(errf(
            code,
            field,
            format!("必须是非负 32 位整数，实际 {raw}"),
        )),
        None => Err(errf(
            code,
            field,
            format!("必须是整数，实际 {}", value.type_name()),
        )),
    }
}

/// 通道 name 规则：`^[A-Z][A-Z0-9]*$`（CONTRACT §2.1）。
fn is_role_id(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_uppercase() => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
}

/// 标量转字符串（数字/字符串都接受；bool/null 拒绝）。
fn scalar_to_string(value: &Yaml, field: &str) -> CalResult<String> {
    match value {
        Yaml::Str(text) => Ok(text.clone()),
        Yaml::Int(number) => Ok(number.to_string()),
        Yaml::Float(number) => Ok(format!("{number}")),
        other => Err(errf(
            CalibrationErrorCode::Metadata,
            field,
            format!("必须是字符串/数字，实际 {}", other.type_name()),
        )),
    }
}

/// 解析 `tool` 字段（string 或 {name,version}）。
fn parse_tool(value: &Yaml, ctx: &mut ParseCtx) -> CalResult<CalibrationTool> {
    if let Some(name) = value.as_str() {
        return Ok(CalibrationTool {
            name: name.to_string(),
            version: None,
        });
    }
    let fields = expect_map(value, "tool", CalibrationErrorCode::Metadata)?;
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut seen: Vec<&str> = Vec::new();
    for (key, item) in fields {
        if seen.contains(&key.as_str()) {
            ctx.warn(
                CalibrationWarningCode::DuplicateField,
                field_path("tool", key),
                "tool 字段重复（保留后出现值）",
            );
        }
        seen.push(key.as_str());
        match key.as_str() {
            "name" => {
                name =
                    Some(expect_str(item, "tool.name", CalibrationErrorCode::Metadata)?.to_string())
            }
            "version" => version = Some(scalar_to_string(item, "tool.version")?),
            other => ctx.warn(
                CalibrationWarningCode::UnknownTopField,
                field_path("tool", other),
                format!("tool 未知字段 {other}（warning）"),
            ),
        }
    }
    let name = name.ok_or_else(|| {
        errf(
            CalibrationErrorCode::Metadata,
            "tool.name",
            "tool mapping 必须含 name",
        )
    })?;
    Ok(CalibrationTool { name, version })
}

/// measurement 块解析结果（只留校验所需；内容不参与应用）。
struct MeasurementInfo {
    mode: Option<String>,
    playback_channels: Option<u32>,
    render_offset_names: Vec<String>,
}

fn parse_measurement(value: &Yaml, ctx: &mut ParseCtx) -> CalResult<MeasurementInfo> {
    let fields = expect_map(value, "measurement", CalibrationErrorCode::Measurement)?;
    let mut info = MeasurementInfo {
        mode: None,
        playback_channels: None,
        render_offset_names: Vec::new(),
    };
    let mut seen: Vec<&str> = Vec::new();
    for (key, item) in fields {
        if seen.contains(&key.as_str()) {
            ctx.warn(
                CalibrationWarningCode::DuplicateField,
                field_path("measurement", key),
                "measurement 字段重复（保留后出现值）",
            );
        }
        seen.push(key.as_str());
        match key.as_str() {
            "mode" => {
                let mode = expect_str(item, "measurement.mode", CalibrationErrorCode::Measurement)?;
                if mode != "direct" && mode != "through_render" {
                    return Err(errf(
                        CalibrationErrorCode::Measurement,
                        "measurement.mode",
                        format!("必须是 direct|through_render，实际 {mode}"),
                    ));
                }
                info.mode = Some(mode.to_string());
            }
            "playback" => {
                let playback = expect_map(
                    item,
                    "measurement.playback",
                    CalibrationErrorCode::Measurement,
                )?;
                for (sub_key, sub_value) in playback {
                    if sub_key == "channels" {
                        let channels = expect_u32(
                            sub_value,
                            "measurement.playback.channels",
                            CalibrationErrorCode::Measurement,
                        )?;
                        info.playback_channels = Some(channels);
                    }
                }
            }
            "render_offsets" => {
                let seq = expect_seq(
                    item,
                    "measurement.render_offsets",
                    CalibrationErrorCode::Measurement,
                )?;
                for (offset_index, offset) in seq.iter().enumerate() {
                    let entry = expect_map(
                        offset,
                        &index_path("measurement.render_offsets", offset_index),
                        CalibrationErrorCode::Measurement,
                    )?;
                    let mut name: Option<String> = None;
                    for (sub_key, sub_value) in entry {
                        if sub_key == "name" {
                            name = Some(
                                expect_str(
                                    sub_value,
                                    &format!(
                                        "{}.name",
                                        index_path("measurement.render_offsets", offset_index)
                                    ),
                                    CalibrationErrorCode::Measurement,
                                )?
                                .to_string(),
                            );
                        }
                    }
                    match name {
                        Some(name) => info.render_offset_names.push(name),
                        None => {
                            return Err(errf(
                                CalibrationErrorCode::Measurement,
                                index_path("measurement.render_offsets", offset_index),
                                "render_offsets 项必须是 {name,delay_samples,polarity} 且含 name",
                            ))
                        }
                    }
                }
            }
            "sweep" | "capture" | "pre_seconds" | "post_seconds" | "repeats" => {
                // 复现信息，不参与应用；类型/内容不做语义校验（CONTRACT §2.2）。
            }
            other => ctx.warn(
                CalibrationWarningCode::UnknownMeasurementField,
                field_path("measurement", other),
                format!("measurement 未知字段 {other}（warning）"),
            ),
        }
    }
    if info.mode.as_deref() == Some("through_render") && info.render_offset_names.is_empty() {
        return Err(errf(
            CalibrationErrorCode::Measurement,
            "measurement.render_offsets",
            "mode=through_render 时必须提供非空 render_offsets",
        ));
    }
    Ok(info)
}

/// 解析单条 filter（type/freq/q/gain 规则按 CONTRACT §2.1/§5/§10 分 v1/v1.1 执行）。
fn parse_filter(
    value: &Yaml,
    schema: CalibrationSchema,
    fs_hz: f64,
    field: &str,
    ctx: &mut ParseCtx,
) -> CalResult<CalibrationFilter> {
    let fields = expect_map(value, field, CalibrationErrorCode::FilterField)?;
    let freq_max = CAL_FREQ_MAX_HZ.min(fs_hz / 2.0 - 1.0);
    let mut freq: Option<f64> = None;
    let mut q: Option<f64> = None;
    let mut gain: Option<f64> = None;
    let mut filter_type: Option<PeqType> = None;
    let mut type_explicit = false;
    let mut seen: Vec<&str> = Vec::new();
    for (key, item) in fields {
        if seen.contains(&key.as_str()) {
            ctx.warn(
                CalibrationWarningCode::DuplicateField,
                field_path(field, key),
                "filter 字段重复（保留后出现值）",
            );
        }
        seen.push(key.as_str());
        match key.as_str() {
            "type" => {
                let raw = expect_str(
                    item,
                    &field_path(field, "type"),
                    CalibrationErrorCode::FilterField,
                )?;
                let parsed = PeqType::from_id(raw).ok_or_else(|| {
                    errf(
                        CalibrationErrorCode::FilterTypeUnknown,
                        field_path(field, "type"),
                        format!("未知 filter type {raw}（消费方必须整份拒收，不降级）"),
                    )
                })?;
                if schema == CalibrationSchema::V1 && parsed != PeqType::Peaking {
                    return Err(errf(
                        CalibrationErrorCode::FilterTypeNotInV1,
                        field_path(field, "type"),
                        format!("v1 只允许 peaking（实际 {}）", parsed.as_str()),
                    ));
                }
                filter_type = Some(parsed);
                type_explicit = true;
            }
            "freq" => {
                let value = expect_num(
                    item,
                    &field_path(field, "freq"),
                    CalibrationErrorCode::FilterFreq,
                )?;
                if !(CAL_FREQ_MIN_HZ..=freq_max).contains(&value) {
                    return Err(errf(
                        CalibrationErrorCode::FilterFreq,
                        field_path(field, "freq"),
                        format!(
                            "freq 必须在 [{} , {}] Hz，实际 {value}",
                            CAL_FREQ_MIN_HZ, freq_max
                        ),
                    ));
                }
                freq = Some(value);
            }
            "q" => {
                q = Some(expect_num(
                    item,
                    &field_path(field, "q"),
                    CalibrationErrorCode::FilterQ,
                )?);
            }
            "gain_db" => {
                let value = expect_num(
                    item,
                    &field_path(field, "gain_db"),
                    CalibrationErrorCode::FilterGain,
                )?;
                if !(CAL_FILTER_GAIN_MIN_DB..=CAL_FILTER_GAIN_MAX_DB).contains(&value) {
                    return Err(errf(
                        CalibrationErrorCode::FilterGain,
                        field_path(field, "gain_db"),
                        format!(
                            "单段 gain_db 必须在 [{}, {}]",
                            CAL_FILTER_GAIN_MIN_DB, CAL_FILTER_GAIN_MAX_DB
                        ),
                    ));
                }
                gain = Some(value);
            }
            other => ctx.warn(
                CalibrationWarningCode::UnknownFilterField,
                field_path(field, other),
                format!("filter 未知字段 {other}（warning）"),
            ),
        }
    }
    let filter_type = filter_type.unwrap_or(PeqType::Peaking);
    let freq = freq.ok_or_else(|| {
        errf(
            CalibrationErrorCode::FilterField,
            field_path(field, "freq"),
            "filter 缺必填 freq",
        )
    })?;
    let q = q.ok_or_else(|| {
        errf(
            CalibrationErrorCode::FilterField,
            field_path(field, "q"),
            "filter 缺必填 q",
        )
    })?;
    let gain = gain.ok_or_else(|| {
        errf(
            CalibrationErrorCode::FilterField,
            field_path(field, "gain_db"),
            "filter 缺必填 gain_db",
        )
    })?;
    match (schema, filter_type) {
        (CalibrationSchema::V1, _) => {
            if !(CAL_PEAKING_Q_MIN..=CAL_PEAKING_Q_MAX_V1).contains(&q) {
                return Err(errf(
                    CalibrationErrorCode::FilterQ,
                    field_path(field, "q"),
                    format!("v1 q 必须在 [0.1, 10]，实际 {q}"),
                ));
            }
        }
        (CalibrationSchema::V11, PeqType::Peaking) => {
            if q < CAL_PEAKING_Q_MIN {
                return Err(errf(
                    CalibrationErrorCode::FilterQ,
                    field_path(field, "q"),
                    format!("v1.1 peaking q 必须 >= 0.1，实际 {q}"),
                ));
            }
            if q > CAL_PEAKING_Q_MAX_V1 {
                ctx.warn(
                    CalibrationWarningCode::PeakingQHigh,
                    field_path(field, "q"),
                    format!(
                        "peaking q={q} > 10：契约合法（P1 modal PEQ 可用）；channelmap 应用路线会拒绝整份文件，只能走 CamillaDSP 片段路径"
                    ),
                );
            }
        }
        (CalibrationSchema::V11, _) => {
            if !(CAL_SHELF_SLOPE_MIN..=CAL_SHELF_SLOPE_MAX).contains(&q) {
                return Err(errf(
                    CalibrationErrorCode::FilterQ,
                    field_path(field, "q"),
                    format!(
                        "v1.1 shelf 的 q 是 RBJ slope S，必须在 [0.1,1.0]（CamillaDSP slope=12q <=12 dB/oct），实际 {q}"
                    ),
                ));
            }
        }
    }
    Ok(CalibrationFilter {
        band: PeqBand {
            freq_hz: freq,
            q,
            gain_db: gain,
            filter_type,
        },
        type_explicit,
    })
}

/// 解析 channels 数组（逐条校验；跨通道检查在调用方完成）。
fn parse_channels(
    value: &Yaml,
    schema: CalibrationSchema,
    fs_hz: f64,
    playback_channels: Option<u32>,
    ctx: &mut ParseCtx,
) -> CalResult<Vec<CalibrationChannel>> {
    let items = expect_seq(value, "channels", CalibrationErrorCode::Channels)?;
    if items.is_empty() {
        return Err(err(
            CalibrationErrorCode::Channels,
            "channels 必须是非空数组",
        ));
    }
    if items.len() > CAL_MAX_CHANNELS {
        return Err(errf(
            CalibrationErrorCode::Channels,
            "channels",
            format!(
                "channels 超过 {CAL_MAX_CHANNELS} 条（实际 {}）",
                items.len()
            ),
        ));
    }
    let delay_max = delay_max_samples(fs_hz as u32);
    let mut channels = Vec::with_capacity(items.len());
    let mut names: Vec<String> = Vec::new();
    let mut indexes: Vec<u32> = Vec::new();
    for (channel_index, item) in items.iter().enumerate() {
        let base = index_path("channels", channel_index);
        let fields = expect_map(item, &base, CalibrationErrorCode::FieldType)?;
        let mut name: Option<String> = None;
        let mut index: Option<u32> = None;
        let mut delay: Option<u32> = None;
        let mut polarity: Option<Polarity> = None;
        let mut polarity_alias = false;
        let mut gain: Option<f64> = None;
        let mut filters: Option<Vec<CalibrationFilter>> = None;
        let mut has_measured = false;
        let mut seen: Vec<&str> = Vec::new();
        for (key, field_value) in fields {
            let path = field_path(&base, key);
            if seen.contains(&key.as_str()) {
                ctx.warn(
                    CalibrationWarningCode::DuplicateField,
                    path.clone(),
                    "channel 字段重复（保留后出现值）",
                );
            }
            seen.push(key.as_str());
            match key.as_str() {
                "name" => {
                    let raw = expect_str(field_value, &path, CalibrationErrorCode::ChannelName)?;
                    if !is_role_id(raw) {
                        return Err(errf(
                            CalibrationErrorCode::ChannelName,
                            path,
                            format!(
                                "name 必须是角色 id（^[A-Z][A-Z0-9]*$，如 FL/SUB1），实际 {raw}"
                            ),
                        ));
                    }
                    name = Some(raw.to_string());
                }
                "index" => {
                    index = Some(expect_u32(
                        field_value,
                        &path,
                        CalibrationErrorCode::ChannelIndex,
                    )?);
                }
                "delay_samples" => {
                    let value = expect_u32(field_value, &path, CalibrationErrorCode::Delay)?;
                    if value > delay_max {
                        return Err(errf(
                            CalibrationErrorCode::Delay,
                            path,
                            format!(
                                "delay_samples={value} 超过 200 ms 上限（{delay_max} samples @ {} Hz）",
                                fs_hz as u32
                            ),
                        ));
                    }
                    delay = Some(value);
                }
                "polarity" => match field_value {
                    Yaml::Str(text) => {
                        let parsed = Polarity::from_id(text).ok_or_else(|| {
                            errf(
                                CalibrationErrorCode::Polarity,
                                path.clone(),
                                format!(
                                    "polarity 必须是 normal|invert（或别名 1/-1），实际 {text}"
                                ),
                            )
                        })?;
                        polarity = Some(parsed);
                    }
                    Yaml::Int(1) => {
                        polarity = Some(Polarity::Normal);
                        polarity_alias = true;
                    }
                    Yaml::Int(-1) => {
                        polarity = Some(Polarity::Invert);
                        polarity_alias = true;
                    }
                    other => {
                        return Err(errf(
                            CalibrationErrorCode::Polarity,
                            path,
                            format!(
                                "polarity 必须是 normal|invert（或别名 1/-1），实际 {}",
                                other.type_name()
                            ),
                        ))
                    }
                },
                "gain_db" => {
                    let value = expect_num(field_value, &path, CalibrationErrorCode::TrimGain)?;
                    if !(CAL_TRIM_MIN_DB..=CAL_TRIM_MAX_DB).contains(&value) {
                        return Err(errf(
                            CalibrationErrorCode::TrimGain,
                            path,
                            format!(
                                "channels[].gain_db 必须在 [{}, {}]",
                                CAL_TRIM_MIN_DB, CAL_TRIM_MAX_DB
                            ),
                        ));
                    }
                    gain = Some(value);
                }
                "filters" => {
                    let seq = match field_value.as_seq() {
                        Some(seq) => seq,
                        None => {
                            return Err(errf(
                                CalibrationErrorCode::Filters,
                                path,
                                format!(
                                    "filters 必须是数组（可为空），实际 {}",
                                    field_value.type_name()
                                ),
                            ))
                        }
                    };
                    if seq.len() > CAL_MAX_FILTERS {
                        return Err(errf(
                            CalibrationErrorCode::FilterCount,
                            path,
                            format!("filters 超过 {CAL_MAX_FILTERS} 段（实际 {}）", seq.len()),
                        ));
                    }
                    let mut parsed_filters = Vec::with_capacity(seq.len());
                    let mut positive_boost = 0.0f64;
                    for (filter_index, filter_value) in seq.iter().enumerate() {
                        let filter_field = index_path(&field_path(&base, "filters"), filter_index);
                        let filter = parse_filter(filter_value, schema, fs_hz, &filter_field, ctx)?;
                        if filter.band.gain_db > 0.0 {
                            positive_boost += filter.band.gain_db;
                        }
                        parsed_filters.push(filter);
                    }
                    if positive_boost > CAL_POS_BOOST_REJECT_DB {
                        return Err(errf(
                            CalibrationErrorCode::PositiveBoost,
                            field_path(&base, "filters"),
                            format!(
                                "PEQ 正向提升总和 {positive_boost:.1} dB > {CAL_POS_BOOST_REJECT_DB} dB（需要负 trim/减少 boost）"
                            ),
                        ));
                    }
                    if positive_boost > CAL_POS_BOOST_WARN_DB {
                        ctx.warn(
                            CalibrationWarningCode::PositiveBoostWarn,
                            field_path(&base, "filters"),
                            format!(
                                "PEQ 正向提升总和 {positive_boost:.1} dB > {CAL_POS_BOOST_WARN_DB} dB（注意余量）"
                            ),
                        );
                    }
                    filters = Some(parsed_filters);
                }
                "measured" => match field_value {
                    Yaml::Null => {}
                    Yaml::Map(_) => has_measured = true,
                    other => ctx.warn(
                        CalibrationWarningCode::MeasuredNotMapping,
                        path,
                        format!(
                            "measured 建议为 mapping（诊断用，可忽略），实际 {}",
                            other.type_name()
                        ),
                    ),
                },
                other => ctx.warn(
                    CalibrationWarningCode::UnknownChannelField,
                    path,
                    format!("channel 未知字段 {other}（warning；不参与应用）"),
                ),
            }
        }
        let name = name.ok_or_else(|| {
            errf(
                CalibrationErrorCode::ChannelName,
                field_path(&base, "name"),
                "channel 缺必填 name",
            )
        })?;
        if names.contains(&name) {
            return Err(errf(
                CalibrationErrorCode::ChannelName,
                field_path(&base, "name"),
                format!("channels[].name 重复：{name}"),
            ));
        }
        names.push(name.clone());
        let index = index.ok_or_else(|| {
            errf(
                CalibrationErrorCode::ChannelIndex,
                field_path(&base, "index"),
                "channel 缺必填 index",
            )
        })?;
        if indexes.contains(&index) {
            return Err(errf(
                CalibrationErrorCode::ChannelIndex,
                field_path(&base, "index"),
                format!("channels[].index 重复：{index}"),
            ));
        }
        indexes.push(index);
        if let Some(limit) = playback_channels {
            if index >= limit {
                return Err(errf(
                    CalibrationErrorCode::ChannelIndex,
                    field_path(&base, "index"),
                    format!(
                        "index={index} 超出 measurement.playback.channels={limit}（index 是设备物理通道域）"
                    ),
                ));
            }
        }
        let delay = delay.ok_or_else(|| {
            errf(
                CalibrationErrorCode::Delay,
                field_path(&base, "delay_samples"),
                "channel 缺必填 delay_samples",
            )
        })?;
        if polarity.is_none() {
            ctx.warn(
                CalibrationWarningCode::PolarityDefault,
                field_path(&base, "polarity"),
                "polarity 缺省：按 normal 处理（建议显式写 normal）",
            );
        }
        if polarity_alias {
            ctx.warn(
                CalibrationWarningCode::PolarityAlias,
                field_path(&base, "polarity"),
                "polarity 使用 ±1 别名（已归一化为 normal/invert）",
            );
        }
        let filters = filters.ok_or_else(|| {
            errf(
                CalibrationErrorCode::Filters,
                field_path(&base, "filters"),
                "channel 缺必填 filters（可为空数组 []）",
            )
        })?;
        channels.push(CalibrationChannel {
            name,
            index,
            delay_samples: delay,
            polarity: polarity.unwrap_or(Polarity::Normal),
            polarity_alias,
            gain_db: gain,
            filters,
            has_measured,
        });
    }
    Ok(channels)
}

/// layout 交叉校验（compat-matrix Y12/Y13；CONTRACT §5 规则 10）。
fn check_layout(
    doc_layout: &str,
    channels: &[CalibrationChannel],
    table: &LayoutRoleTable,
    ctx: &mut ParseCtx,
) -> CalResult<()> {
    if doc_layout != table.layout_id {
        return Err(errf(
            CalibrationErrorCode::LayoutMismatch,
            "layout",
            format!(
                "calibration.layout={doc_layout} 与给定 layout 角色表 {} 不一致（禁止跨布局套用）",
                table.layout_id
            ),
        ));
    }
    let mut covered: Vec<&str> = Vec::new();
    for channel in channels {
        if !table.contains(&channel.name) {
            return Err(errf(
                CalibrationErrorCode::LayoutRoleUnknown,
                format!("channels[{}].name", channel.index),
                format!(
                    "name={} 不在 layout={} 的角色表中（禁止把别的布局校准套到本 profile）",
                    channel.name, table.layout_id
                ),
            ));
        }
        if let Some(expected) = table.first_output(&channel.name) {
            if channel.index != expected as u32 {
                return Err(errf(
                    CalibrationErrorCode::LayoutIndexMismatch,
                    format!("channels[{}].index", channel.index),
                    format!(
                        "index={} 与 layout={} 映射不一致（{} 期望 {}）",
                        channel.index, table.layout_id, channel.name, expected
                    ),
                ));
            }
        }
        covered.push(&channel.name);
    }
    for role in &table.roles {
        if !covered.contains(&role.id.as_str()) {
            ctx.warn(
                CalibrationWarningCode::LayoutRoleUncalibrated,
                format!("layout.{}", role.id),
                format!(
                    "layout={} 角色 {} 不在本校准文件中（partial calibration，合法；该角色保持渲染基线）",
                    table.layout_id, role.id
                ),
            );
        }
    }
    Ok(())
}

/// 解析校准 YAML 原始字节（不做 layout 交叉校验）。
pub fn parse_calibration_yaml(bytes: &[u8]) -> CalResult<CalibrationDocument> {
    parse_calibration_yaml_with_layout(bytes, None)
}

/// 解析校准 YAML 原始字节，并可选按 layout 角色表交叉校验（T57 解析结果作为输入）。
pub fn parse_calibration_yaml_with_layout(
    bytes: &[u8],
    layout_table: Option<&LayoutRoleTable>,
) -> CalResult<CalibrationDocument> {
    let source_sha256 = sha256_hex(bytes);
    let root = parse_yaml_document(bytes)?;
    let root_fields = match &root {
        Yaml::Map(fields) => fields,
        other => {
            return Err(errf(
                CalibrationErrorCode::YamlRoot,
                "",
                format!("YAML 顶层必须是 mapping，实际 {}", other.type_name()),
            ))
        }
    };

    let mut ctx = ParseCtx::new();
    // 第一遍：只收集原始字段（YAML 键序任意，语义校验在收集完之后）。
    let mut schema_value: Option<Yaml> = None;
    let mut sample_rate_value: Option<Yaml> = None;
    let mut reference_value: Option<Yaml> = None;
    let mut layout_value: Option<Yaml> = None;
    let mut created_value: Option<Yaml> = None;
    let mut tool_value: Option<Yaml> = None;
    let mut version_value: Option<Yaml> = None;
    let mut note_value: Option<Yaml> = None;
    let mut measurement_value: Option<Yaml> = None;
    let mut channels_value: Option<Yaml> = None;
    let mut seen_top: Vec<&str> = Vec::new();
    for (key, value) in root_fields {
        if seen_top.contains(&key.as_str()) {
            ctx.warn(
                CalibrationWarningCode::DuplicateField,
                key.clone(),
                "顶层字段重复（保留后出现值）",
            );
        }
        seen_top.push(key.as_str());
        match key.as_str() {
            "schema" => schema_value = Some(value.clone()),
            "sample_rate" => sample_rate_value = Some(value.clone()),
            "reference_index" => reference_value = Some(value.clone()),
            "layout" => layout_value = Some(value.clone()),
            "created_unix" => created_value = Some(value.clone()),
            "tool" => tool_value = Some(value.clone()),
            "version" => version_value = Some(value.clone()),
            "note" => note_value = Some(value.clone()),
            "measurement" => measurement_value = Some(value.clone()),
            "channels" => channels_value = Some(value.clone()),
            other => ctx.warn(
                CalibrationWarningCode::UnknownTopField,
                other.to_string(),
                format!("未知顶层字段 {other}（warning；不参与应用）"),
            ),
        }
    }

    let schema_string = match schema_value {
        Some(Yaml::Str(text)) => text,
        Some(other) => {
            return Err(errf(
                CalibrationErrorCode::Schema,
                "schema",
                format!("schema 必须是字符串，实际 {}", other.type_name()),
            ))
        }
        None => {
            return Err(errf(
                CalibrationErrorCode::Schema,
                "schema",
                format!("缺少 schema（必须是 {CAL_SCHEMA_V1} 或 {CAL_SCHEMA_V11}）"),
            ))
        }
    };
    let schema = CalibrationSchema::from_id(&schema_string).ok_or_else(|| {
        errf(
            CalibrationErrorCode::Schema,
            "schema",
            format!(
                "schema={schema_string} 不受支持（必须是 {CAL_SCHEMA_V1} 或 {CAL_SCHEMA_V11}）"
            ),
        )
    })?;

    let sample_rate = match sample_rate_value {
        Some(value) => {
            let raw = value.as_i64().ok_or_else(|| {
                errf(
                    CalibrationErrorCode::SampleRate,
                    "sample_rate",
                    format!("sample_rate 必须是整数，实际 {}", value.type_name()),
                )
            })?;
            if raw <= 0 || raw > u32::MAX as i64 {
                return Err(errf(
                    CalibrationErrorCode::SampleRate,
                    "sample_rate",
                    format!("sample_rate 必须是正 32 位整数，实际 {raw}"),
                ));
            }
            raw as u32
        }
        None => {
            return Err(errf(
                CalibrationErrorCode::MissingField,
                "sample_rate",
                "缺少必填 sample_rate",
            ))
        }
    };
    if !CAL_DECLARED_SAMPLE_RATES_HZ.contains(&sample_rate) {
        ctx.warn(
            CalibrationWarningCode::SampleRateNonDeclared,
            "sample_rate",
            format!("sample_rate={sample_rate} 不是声明集合 {CAL_DECLARED_SAMPLE_RATES_HZ:?}"),
        );
    }
    let reference_index = match reference_value {
        Some(value) => match value.as_i64() {
            Some(raw) if raw >= 0 && raw <= u32::MAX as i64 => raw as u32,
            Some(raw) => {
                return Err(errf(
                    CalibrationErrorCode::ReferenceIndex,
                    "reference_index",
                    format!("reference_index 必须是非负整数，实际 {raw}"),
                ))
            }
            None => {
                return Err(errf(
                    CalibrationErrorCode::ReferenceIndex,
                    "reference_index",
                    format!("reference_index 必须是非负整数，实际 {}", value.type_name()),
                ))
            }
        },
        None => {
            return Err(errf(
                CalibrationErrorCode::MissingField,
                "reference_index",
                "缺少必填 reference_index",
            ))
        }
    };
    let layout_id = match layout_value {
        None | Some(Yaml::Null) => None,
        Some(Yaml::Str(text)) => Some(text),
        Some(other) => {
            return Err(errf(
                CalibrationErrorCode::Layout,
                "layout",
                format!("layout 必须是字符串，实际 {}", other.type_name()),
            ))
        }
    };
    let created_unix = match created_value {
        None | Some(Yaml::Null) => None,
        Some(value) => match value.as_i64() {
            Some(raw) if raw >= 0 => Some(raw as u64),
            _ => {
                return Err(errf(
                    CalibrationErrorCode::Metadata,
                    "created_unix",
                    format!("created_unix 必须是非负整数，实际 {}", value.type_name()),
                ))
            }
        },
    };
    let tool = match tool_value {
        None | Some(Yaml::Null) => None,
        Some(value) => Some(parse_tool(&value, &mut ctx)?),
    };
    let tool_version = match version_value {
        None | Some(Yaml::Null) => None,
        Some(value) => Some(scalar_to_string(&value, "version")?),
    };
    let note = match note_value {
        None | Some(Yaml::Null) => None,
        Some(Yaml::Str(text)) => {
            if text.chars().count() > 500 {
                ctx.warn(
                    CalibrationWarningCode::NoteTooLong,
                    "note",
                    format!("note 超过 500 字符（实际 {}）", text.chars().count()),
                );
            }
            Some(text)
        }
        Some(other) => {
            return Err(errf(
                CalibrationErrorCode::Metadata,
                "note",
                format!("note 必须是字符串，实际 {}", other.type_name()),
            ))
        }
    };
    let measurement = match measurement_value {
        None | Some(Yaml::Null) => MeasurementInfo {
            mode: None,
            playback_channels: None,
            render_offset_names: Vec::new(),
        },
        Some(value) => parse_measurement(&value, &mut ctx)?,
    };
    let channels_yaml = channels_value.ok_or_else(|| {
        errf(
            CalibrationErrorCode::MissingField,
            "channels",
            "缺少必填 channels",
        )
    })?;
    let channels = parse_channels(
        &channels_yaml,
        schema,
        sample_rate as f64,
        measurement.playback_channels,
        &mut ctx,
    )?;

    // 跨通道规则（CONTRACT §5 规则 4/5）。
    let minimum_delay = channels
        .iter()
        .map(|channel| channel.delay_samples)
        .min()
        .unwrap_or(0);
    if minimum_delay != 0 {
        return Err(errf(
            CalibrationErrorCode::DelayNormalization,
            "channels[].delay_samples",
            format!(
                "delay_samples 未归一化：min={minimum_delay}（必须为 0；D_i=round(max t - t_i)）"
            ),
        ));
    }
    if !channels
        .iter()
        .any(|channel| channel.index == reference_index)
    {
        return Err(errf(
            CalibrationErrorCode::ReferenceIndex,
            "reference_index",
            format!("reference_index={reference_index} 不在 channels[].index 中"),
        ));
    }
    if let Some(reference_channel) = channels
        .iter()
        .find(|channel| channel.index == reference_index)
    {
        if reference_channel.delay_samples != 0 {
            ctx.warn(
                CalibrationWarningCode::ReferenceNotZeroDelay,
                "reference_index",
                format!(
                    "reference_index 指向的通道 {} delay_samples={}（建议指向 D=0/到达最晚的通道）",
                    reference_channel.name, reference_channel.delay_samples
                ),
            );
        }
    }
    if measurement.mode.as_deref() == Some("through_render") {
        for name in &measurement.render_offset_names {
            if !channels.iter().any(|channel| &channel.name == name) {
                ctx.warn(
                    CalibrationWarningCode::UnknownMeasurementField,
                    "measurement.render_offsets",
                    format!("render_offsets.name={name} 不在 channels 中（warning）"),
                );
            }
        }
    }

    // v1.1 可读性提示（CONTRACT §10：只含 Peaking 且 Q<=10 时 v1 消费方可读）。
    if schema == CalibrationSchema::V11 {
        let v1_readable = channels.iter().all(|channel| {
            channel.filters.iter().all(|filter| {
                filter.band.filter_type == PeqType::Peaking && filter.band.q <= CAL_PEAKING_Q_MAX_V1
            })
        });
        if v1_readable {
            ctx.warn(
                CalibrationWarningCode::V11ReadableByV1,
                "schema",
                "v1.1 文件只含 Peaking 且 Q<=10：v1 消费方可直接读取",
            );
        }
    }

    // layout 交叉校验（可选输入）。
    let mut layout_checked = false;
    if let Some(table) = layout_table {
        if let Some(doc_layout) = layout_id.as_deref() {
            check_layout(doc_layout, &channels, table, &mut ctx)?;
            layout_checked = true;
        } else {
            return Err(errf(
                CalibrationErrorCode::LayoutMismatch,
                "layout",
                format!(
                    "给定 layout 角色表 {} 但校准文件缺 layout 字段（无法交叉校验，拒绝猜）",
                    table.layout_id
                ),
            ));
        }
    }

    Ok(CalibrationDocument {
        schema,
        schema_string,
        sample_rate,
        reference_index,
        layout: layout_id,
        created_unix,
        tool,
        tool_version,
        note,
        channels,
        warnings: ctx.warnings,
        source_sha256,
        layout_checked,
    })
}

/// 读取并解析校准文件（只读；不写回）。
pub fn load_calibration_file(path: &Path) -> CalResult<CalibrationDocument> {
    load_calibration_file_with_layout(path, None)
}

/// 读取并解析校准文件，并可选按 layout 角色表交叉校验。
pub fn load_calibration_file_with_layout(
    path: &Path,
    layout_table: Option<&LayoutRoleTable>,
) -> CalResult<CalibrationDocument> {
    let bytes = std::fs::read(path).map_err(|error| {
        errf(
            CalibrationErrorCode::Io,
            path.display().to_string(),
            format!("读取校准文件失败：{error}"),
        )
    })?;
    parse_calibration_yaml_with_layout(&bytes, layout_table)
}

// ===========================================================================
// 合并：校准 -> 渲染基线角色（CONTRACT §4.1；禁止二次应用/双乘）
// ===========================================================================

/// 合并后的单角色（渲染基线 + 校准；只读视图，供 IR 构建/报告）。
#[derive(Clone, Debug, PartialEq)]
pub struct MergedRole {
    /// 角色 id。
    pub name: String,
    /// 播放设备物理通道号。
    pub index: u32,
    /// 合并后延迟（渲染 delay + 校准 delay；samples，整数）。
    pub delay_samples: u32,
    /// 合并后极性（渲染 XOR 校准）。
    pub polarity: Polarity,
    /// 合并后 gain（渲染 gain + 校准 trim；dB）。
    pub gain_db: f64,
    /// 合并后 PEQ 链（渲染 PEQ 按序 + 校准 filters；type 原样保留）。
    pub peq: Vec<PeqBand>,
    /// 本角色是否被本校准覆盖。
    pub calibrated: bool,
}

/// 合并结果（CONTRACT §4.1 一次合并；同一基线角色只能合并一次）。
#[derive(Clone, Debug, PartialEq)]
pub struct MergedCalibration {
    /// 源校准 schema（v1/v1.1 原样）。
    pub schema: CalibrationSchema,
    /// 源 schema 字面值。
    pub schema_string: String,
    /// 源文件字节 SHA-256。
    pub source_sha256: String,
    /// 目标 profile 采样率（= 校准文件 sample_rate）。
    pub sample_rate: u32,
    /// 校准文件 layout id（可选）。
    pub layout_id: Option<String>,
    /// 合并后角色（按渲染基线数组顺序；覆盖调用方给定角色集）。
    pub roles: Vec<MergedRole>,
    /// 源文件 warning + 合并 warning。
    pub warnings: Vec<CalibrationWarning>,
}

fn merged_gain(gain: f64, field: String) -> CalResult<f64> {
    if !gain.is_finite() {
        return Err(errf(
            CalibrationErrorCode::MergedGainRange,
            field,
            "合并后 gain 不是有限数",
        ));
    }
    if !(CHANNEL_MAP_GAIN_MIN_DB..=CHANNEL_MAP_GAIN_MAX_DB).contains(&gain) {
        return Err(errf(
            CalibrationErrorCode::MergedGainRange,
            field,
            format!(
                "合并后 role gain 必须在 [{}, {}] dB（channelmap 约束），实际 {gain}",
                CHANNEL_MAP_GAIN_MIN_DB, CHANNEL_MAP_GAIN_MAX_DB
            ),
        ));
    }
    Ok(gain)
}

fn checked_add_delay(base: u32, calibration: u32, delay_max: u32, field: String) -> CalResult<u32> {
    let merged = base.checked_add(calibration).ok_or_else(|| {
        errf(
            CalibrationErrorCode::DelayOverflow,
            &field,
            "合并后 delay 溢出 u32",
        )
    })?;
    if merged > delay_max {
        return Err(errf(
            CalibrationErrorCode::DelayOverflow,
            field,
            format!("合并后 delay={merged} samples 超过 200 ms 上限（{delay_max} samples）"),
        ));
    }
    Ok(merged)
}

/// 合并校准到渲染基线角色（CONTRACT §4.1）：
///
/// - delay_samples 相加（<= 200 ms）；
/// - polarity = XOR(render, calibration invert)（不双乘）；
/// - gain_db 相加（结果须在 channelmap [-60,+12]）；
/// - PEQ 按序追加（<=32，type 原样保留，不得降级）；
/// - 同一基线（origin=CalibrationApplied）二次应用 -> E_CAL_DOUBLE_APPLY；
/// - 校准文件角色必须在基线中存在且 index 一致（禁止跨布局套用）。
pub fn merge_calibration(
    document: &CalibrationDocument,
    layout_table: Option<&LayoutRoleTable>,
    base_roles: &[RenderRoleBase],
    fs_hz: u32,
) -> CalResult<MergedCalibration> {
    if fs_hz == 0 {
        return Err(errf(
            CalibrationErrorCode::FsMismatch,
            "fs_hz",
            "目标 profile fs 必须为正整数",
        ));
    }
    if document.sample_rate != fs_hz {
        return Err(errf(
            CalibrationErrorCode::FsMismatch,
            "sample_rate",
            format!(
                "校准文件 sample_rate={} 与目标 profile fs={fs_hz} 不一致（delay 单位不同，禁止套用）",
                document.sample_rate
            ),
        ));
    }
    if base_roles.is_empty() {
        return Err(errf(
            CalibrationErrorCode::BaseRoleInvalid,
            "base_roles",
            "渲染基线角色表不能为空",
        ));
    }
    if let Some(table) = layout_table {
        match document.layout.as_deref() {
            Some(layout) => check_layout(layout, &document.channels, table, &mut ParseCtx::new())?,
            None => {
                return Err(errf(
                    CalibrationErrorCode::LayoutMismatch,
                    "layout",
                    format!(
                        "给定 layout 角色表 {} 但校准文件缺 layout 字段（无法交叉校验，拒绝猜）",
                        table.layout_id
                    ),
                ))
            }
        }
    }
    // 基线合法性：name/index 唯一、gain/PEQ 在 channelmap 范围。
    let mut base_names: Vec<&str> = Vec::new();
    let mut base_indexes: Vec<u32> = Vec::new();
    let mut warnings = document.warnings.clone();
    for (position, role) in base_roles.iter().enumerate() {
        if base_names.contains(&role.name.as_str()) {
            return Err(errf(
                CalibrationErrorCode::BaseRoleInvalid,
                format!("base_roles[{position}].name"),
                format!("渲染基线角色 name 重复：{}", role.name),
            ));
        }
        base_names.push(role.name.as_str());
        if base_indexes.contains(&role.index) {
            return Err(errf(
                CalibrationErrorCode::BaseRoleInvalid,
                format!("base_roles[{position}].index"),
                format!("渲染基线角色 index 重复：{}", role.index),
            ));
        }
        base_indexes.push(role.index);
        if !role.gain_db.is_finite() {
            return Err(errf(
                CalibrationErrorCode::BaseRoleInvalid,
                format!("base_roles[{position}].gain_db"),
                "渲染基线 gain 不是有限数",
            ));
        }
        if role.peq.len() > CAL_MAX_FILTERS {
            return Err(errf(
                CalibrationErrorCode::PeqOverflow,
                format!("base_roles[{position}].peq"),
                format!("渲染基线 PEQ 已超过 {CAL_MAX_FILTERS} 段"),
            ));
        }
    }

    let delay_max = delay_max_samples(fs_hz);
    let mut merged_roles: Vec<MergedRole> = Vec::with_capacity(base_roles.len());
    let mut calibrated_indexes: Vec<usize> = Vec::new();
    for (base_position, role) in base_roles.iter().enumerate() {
        let mut merged = MergedRole {
            name: role.name.clone(),
            index: role.index,
            delay_samples: role.delay_samples,
            polarity: role.polarity,
            gain_db: role.gain_db,
            peq: role.peq.clone(),
            calibrated: false,
        };
        if let Some(channel) = document.channel(&role.name) {
            if role.origin == RoleSpecOrigin::CalibrationApplied {
                return Err(errf(
                    CalibrationErrorCode::DoubleApply,
                    format!("base_roles[{base_position}]"),
                    format!(
                        "角色 {} 已应用过校准（origin=CalibrationApplied），禁止二次合并（避免 delay/polarity/PEQ 双叠）",
                        role.name
                    ),
                ));
            }
            if channel.index != role.index {
                return Err(errf(
                    CalibrationErrorCode::RoleIndexMismatch,
                    format!("base_roles[{base_position}].index"),
                    format!(
                        "校准通道 {} index={} 与渲染基线 index={} 不一致（防止错路由）",
                        channel.name, channel.index, role.index
                    ),
                ));
            }
            merged.delay_samples = checked_add_delay(
                role.delay_samples,
                channel.delay_samples,
                delay_max,
                format!("base_roles[{base_position}].delay_samples"),
            )?;
            merged.polarity = role.polarity.xor(channel.polarity);
            merged.gain_db = merged_gain(
                role.gain_db + channel.gain_db.unwrap_or(0.0),
                format!("base_roles[{base_position}].gain_db"),
            )?;
            for filter in &channel.filters {
                merged.peq.push(filter.band);
            }
            if merged.peq.len() > CAL_MAX_FILTERS {
                return Err(errf(
                    CalibrationErrorCode::PeqOverflow,
                    format!("base_roles[{base_position}].peq"),
                    format!(
                        "合并后 PEQ {} 段 > {CAL_MAX_FILTERS} 段上限（拒绝静默丢段）",
                        merged.peq.len()
                    ),
                ));
            }
            merged.calibrated = true;
            calibrated_indexes.push(base_position);
        }
        merged_roles.push(merged);
    }
    // 校准文件里有、渲染基线里没有的角色：拒绝（CONTRACT §4.1：name 不存在 -> error）。
    for channel in &document.channels {
        if !base_names.contains(&channel.name.as_str()) {
            return Err(errf(
                CalibrationErrorCode::RoleNotFound,
                format!("channels.{}", channel.name),
                format!(
                    "校准通道 {} 在渲染基线角色表中不存在（禁止把别的布局校准套到本 profile）",
                    channel.name
                ),
            ));
        }
    }
    for (position, role) in base_roles.iter().enumerate() {
        if !calibrated_indexes.contains(&position) {
            warnings.push(CalibrationWarning::new(
                CalibrationWarningCode::RoleUncalibrated,
                format!("base_roles[{position}]"),
                format!(
                    "渲染基线角色 {} 未被本校准覆盖（partial calibration，合法；保持基线值）",
                    role.name
                ),
            ));
        }
    }
    Ok(MergedCalibration {
        schema: document.schema,
        schema_string: document.schema_string.clone(),
        source_sha256: document.source_sha256.clone(),
        sample_rate: document.sample_rate,
        layout_id: document.layout.clone(),
        roles: merged_roles,
        warnings,
    })
}

// ===========================================================================
// T11 目标曲线适配（纯数据合同；curve 最多绑定一次）
// ===========================================================================

/// 目标相位参考语义（T11 PhaseRef 的 compat 侧镜像；只允许已知值）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CalPhaseRef {
    /// 只有幅度目标（不得当零相位；设计家族显式选择）。
    None,
    /// 期望的总相位（已展开，deg）。
    Absolute,
    /// 去粗延迟相位；需 target_delay_samples。
    DelayRemoved,
}

impl CalPhaseRef {
    /// 从 T11 字面值解析（未知值必须拒收，不猜）。
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "none" => Some(CalPhaseRef::None),
            "absolute" => Some(CalPhaseRef::Absolute),
            "delay_removed" => Some(CalPhaseRef::DelayRemoved),
            _ => None,
        }
    }

    /// 字面值。
    pub fn as_str(self) -> &'static str {
        match self {
            CalPhaseRef::None => "none",
            CalPhaseRef::Absolute => "absolute",
            CalPhaseRef::DelayRemoved => "delay_removed",
        }
    }

    /// 是否要求 target_delay_samples。
    pub fn requires_delay(self) -> bool {
        self == CalPhaseRef::DelayRemoved
    }
}

/// T11 目标曲线适配 trait（T26/T60 用本地 newtype 实现；避免 crate 依赖/orphan 限制）。
///
/// T11 侧取值映射（crates/designer/src/targets.rs 公共访问器）：
/// - curve_id：设计器/报告侧曲线标识；
/// - fs_hz = TargetCurve::fs()；
/// - target_sha256 = TargetCurve::target_sha256()；
/// - phase_ref = TargetCurve::meta().phase_ref.as_str()（none/absolute/delay_removed）；
/// - target_delay_samples = TargetCurve::meta().target_delay_samples；
/// - point_count = TargetCurve::raw_points().len()。
pub trait TargetCurveSource {
    /// 设计器侧曲线 id（非空）。
    fn curve_id(&self) -> &str;
    /// T11 TargetCurve::fs()；必须等于本 IR fs。
    fn fs_hz(&self) -> f64;
    /// T11 TargetCurve::target_sha256()（64 hex）。
    fn target_sha256(&self) -> &str;
    /// T11 meta().phase_ref.as_str()；None=未知 -> 拒收。
    fn phase_ref(&self) -> Option<&str>;
    /// T11 meta().target_delay_samples。
    fn target_delay_samples(&self) -> Option<u32>;
    /// T11 raw_points().len()。
    fn point_count(&self) -> usize;
}

/// 已校验的目标曲线引用（IR curve binding 的输入）。
#[derive(Clone, Debug, PartialEq)]
pub struct TargetCurveRef {
    /// 曲线 id。
    pub curve_id: String,
    /// 目标曲线内容 hash（T11 target_sha256）。
    pub target_sha256: String,
    /// 曲线设计采样率（Hz；必须等于 IR fs）。
    pub fs_hz: f64,
    /// 相位参考语义。
    pub phase_ref: CalPhaseRef,
    /// delay_removed 时的整数 D_target（samples）。
    pub target_delay_samples: Option<u32>,
    /// 点数（信息性；必须 >= 2）。
    pub point_count: usize,
}

impl TargetCurveRef {
    /// 从 T11 访问器适配（校验未知语义/hash/fs/D）。
    pub fn from_source<S: TargetCurveSource + ?Sized>(source: &S) -> CalResult<Self> {
        let curve_id = source.curve_id().trim().to_string();
        if curve_id.is_empty() {
            return Err(errf(
                CalibrationErrorCode::IrProfile,
                "curve.curve_id",
                "curve_id 不能为空",
            ));
        }
        let target_sha256 = source.target_sha256().trim().to_string();
        if !is_sha256_hex(&target_sha256) {
            return Err(errf(
                CalibrationErrorCode::IrCurveHash,
                "curve.target_sha256",
                format!("target_sha256 必须是 64 hex，实际 {target_sha256:?}"),
            ));
        }
        let fs_hz = source.fs_hz();
        if !fs_hz.is_finite() || fs_hz <= 0.0 {
            return Err(errf(
                CalibrationErrorCode::IrCurveFs,
                "curve.fs_hz",
                format!("目标曲线 fs 必须是有限正数，实际 {fs_hz}"),
            ));
        }
        let phase_ref = match source.phase_ref() {
            Some(id) => CalPhaseRef::from_id(id).ok_or_else(|| {
                errf(
                    CalibrationErrorCode::IrCurvePhase,
                    "curve.phase_ref",
                    format!(
                        "未知 phase_ref={id}（只允许 none/absolute/delay_removed；unknown 拒收）"
                    ),
                )
            })?,
            None => {
                return Err(errf(
                    CalibrationErrorCode::IrCurvePhase,
                    "curve.phase_ref",
                    "phase_ref 缺失/未知（T11 适配器必须给出 none|absolute|delay_removed）",
                ))
            }
        };
        let target_delay_samples = source.target_delay_samples();
        if phase_ref.requires_delay() {
            let delay = target_delay_samples.ok_or_else(|| {
                errf(
                    CalibrationErrorCode::IrCurvePhase,
                    "curve.target_delay_samples",
                    "phase_ref=delay_removed 必须给出整数 target_delay_samples",
                )
            })?;
            let delay_max = delay_max_samples(fs_hz as u32);
            if delay > delay_max {
                return Err(errf(
                    CalibrationErrorCode::IrCurvePhase,
                    "curve.target_delay_samples",
                    format!("target_delay_samples={delay} 超过 200 ms 上限（{delay_max} samples）"),
                ));
            }
        } else if target_delay_samples.is_some() {
            return Err(errf(
                CalibrationErrorCode::IrCurvePhase,
                "curve.target_delay_samples",
                "phase_ref != delay_removed 时禁止携带 target_delay_samples（语义歧义）",
            ));
        }
        let point_count = source.point_count();
        if point_count < 2 {
            return Err(errf(
                CalibrationErrorCode::IrCurvePoints,
                "curve.point_count",
                format!("目标曲线点数必须 >= 2（T11 保证），实际 {point_count}"),
            ));
        }
        Ok(Self {
            curve_id,
            target_sha256,
            fs_hz,
            phase_ref,
            target_delay_samples,
            point_count,
        })
    }
}

// ===========================================================================
// 内部 IR 草案（tspi-cal-ir-draft/v1）——version/hash/role/stale/placement
// ===========================================================================

/// 曲线应用点：P1 只允许基线 P 的输出目标 FIR（TargetFir）阶段。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CurveApplySite {
    /// 基线 P 设计的 TargetFir 阶段（唯一合法应用点）。
    TargetFirBaseline,
}

impl CurveApplySite {
    /// 字面值。
    pub fn as_str(self) -> &'static str {
        match self {
            CurveApplySite::TargetFirBaseline => "target_fir_baseline",
        }
    }
}

/// IR 内的目标曲线绑定（curve 最多一条；应用点固定 TargetFirBaseline）。
#[derive(Clone, Debug, PartialEq)]
pub struct CurveBinding {
    /// 曲线 id。
    pub curve_id: String,
    /// 目标曲线内容 hash。
    pub target_sha256: String,
    /// 曲线设计 fs（必须等于 IR fs）。
    pub fs_hz: f64,
    /// 相位参考语义。
    pub phase_ref: CalPhaseRef,
    /// delay_removed 时的整数 D_target。
    pub target_delay_samples: Option<u32>,
    /// 点数。
    pub point_count: usize,
    /// 应用点（唯一）。
    pub apply_site: CurveApplySite,
}

/// G 残差规则：只允许 modal residual（curve/tone 已在 P 基线，禁止双叠）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GResidualRule {
    /// G 只承担模态残差；禁止再次应用 curve/tone。
    ModalResidualOnly,
}

impl GResidualRule {
    /// 字面值。
    pub fn as_str(self) -> &'static str {
        match self {
            GResidualRule::ModalResidualOnly => "modal_residual_only",
        }
    }
}

/// G 残差绑定（ir_draft 只记录规则/参与者；curve 必须为 None）。
#[derive(Clone, Debug, PartialEq)]
pub struct GResidualBinding {
    /// 规则（固定 ModalResidualOnly）。
    pub rule: GResidualRule,
    /// 5x5 低带 participants（角色 id；P1 典型 FL/FR/SL/SR/SUB1）。
    pub participants: Vec<String>,
}

impl GResidualBinding {
    /// 纯模态残差（无 curve）。
    pub fn modal_residual_only(participants: Vec<String>) -> Self {
        Self {
            rule: GResidualRule::ModalResidualOnly,
            participants,
        }
    }

    /// curve 绑定必须为 None（G 只 modal residual；双叠即错误）。
    pub fn curve_id(&self) -> Option<&str> {
        None
    }
}

/// IR 源信息（校准文件不变式：v1/v1.1 schema + 源字节 hash）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrSource {
    /// 源 schema（v1/v1.1）。
    pub schema: CalibrationSchema,
    /// 源 schema 字面值。
    pub schema_string: String,
    /// 源文件字节 SHA-256（小写 hex）。
    pub source_sha256: String,
    /// 源文件 sample_rate。
    pub sample_rate: u32,
    /// 是否 legacy v1（v1.1=false）。
    pub legacy_v1: bool,
}

/// IR 的 profile 绑定（stereo mask / 目标 profile / P 基线 hash）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrProfileBinding {
    /// profile id。
    pub profile_id: String,
    /// 渲染 profile 文档 hash（P/G 基线身份；变化即 stale）。
    pub profile_sha256: String,
    /// 是否 stereo_strict（DEC §1：默认 true，FL/FR 直出，其余零填）。
    pub stereo_strict: bool,
    /// stereo mask 角色 id（stereo_strict 时必须恰为 FL/FR）。
    pub stereo_mask: Vec<String>,
    /// 目标 profile 标识（如 target_multichannel_4_1_2）。
    pub target_profile: String,
}

/// IR role 绑定项（唯一 role -> 物理通道映射）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrRoleRef {
    /// 角色 id。
    pub name: String,
    /// 播放物理通道号。
    pub index: u32,
}

/// delay/polarity 的编码方式（二选一，禁止重复计）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PostEncoding {
    /// 折入 P FIR 系数（P = delta(D) * polarity；role 字段 delay=0/polarity=+1）。
    CoeffEmbedded,
    /// 放 role 元数据（P = [1.0]；role 字段 delay=D/polarity=±1；对应 core-v1
    /// TspiRoleV1.delay_samples/delay_q16/polarity 与 TspiDiagFirV1 语义）。
    RoleMetadata,
}

impl PostEncoding {
    /// 字面值（IR 规范化序列化/报告用）。
    pub fn as_str(self) -> &'static str {
        match self {
            PostEncoding::CoeffEmbedded => "coeff_embedded",
            PostEncoding::RoleMetadata => "role_metadata",
        }
    }
}

/// IR 的 P_post 单角色（placement 按规范链序；PEQ/gain/delay/polarity 各只记一次）。
#[derive(Clone, Debug, PartialEq)]
pub struct IrPostRole {
    /// 角色 id。
    pub name: String,
    /// 物理通道号。
    pub index: u32,
    /// 实际总延迟（samples；两种编码下都等于滤波/role 的整数 D）。
    pub delay_samples: u32,
    /// 次样本延迟（校准为整数 D，恒 0；保留字段对齐 core-v1 §11.10）。
    pub delay_q16: u32,
    /// 语义极性（合并结果；应用一次）。
    pub polarity: Polarity,
    /// 合并后 role gain（dB；Gain 节点一次应用，不折入 P FIR）。
    pub gain_db: f64,
    /// 校准 PEQ 段（ModalPeq 阶段一次应用；不折入 P FIR）。
    pub peq: Vec<PeqBand>,
    /// 本角色出现的阶段（按规范链序子集；Gain/Protection 恒在）。
    pub placement: Vec<PostStage>,
    /// 本角色 P FIR taps（CoeffEmbedded = D+1；RoleMetadata = 1）。
    pub p_taps: u32,
}

/// 内部 IR 草案（`tspi-cal-ir-draft/v1`；P1 新 schema，不是 CONTRACT 字段）。
#[derive(Clone, Debug, PartialEq)]
pub struct IrDraft {
    schema: String,
    ir_version: u32,
    created_unix: u64,
    hash: String,
    source: IrSource,
    layout_id: Option<String>,
    profile: IrProfileBinding,
    roles: Vec<IrRoleRef>,
    post: Vec<IrPostRole>,
    curve: Option<CurveBinding>,
    g_residual: GResidualBinding,
    encoding: PostEncoding,
    extensions: Vec<(String, String)>,
    warnings: Vec<CalibrationWarning>,
}

impl IrDraft {
    /// IR schema 字面值。
    pub fn schema(&self) -> &str {
        &self.schema
    }

    /// IR 整数版本。
    pub fn ir_version(&self) -> u32 {
        self.ir_version
    }

    /// 创建时间（unix 秒；元数据，不参与 ir_hash）。
    pub fn created_unix(&self) -> u64 {
        self.created_unix
    }

    /// 内容 hash（规范化序列化 SHA-256；不含 created_unix/warning）。
    pub fn ir_hash(&self) -> &str {
        &self.hash
    }

    /// hash 算法标识。
    pub fn hash_algo(&self) -> &'static str {
        IR_HASH_ALGO
    }

    /// 源校准信息。
    pub fn source(&self) -> &IrSource {
        &self.source
    }

    /// layout id。
    pub fn layout_id(&self) -> Option<&str> {
        self.layout_id.as_deref()
    }

    /// profile 绑定。
    pub fn profile(&self) -> &IrProfileBinding {
        &self.profile
    }

    /// role 绑定。
    pub fn roles(&self) -> &[IrRoleRef] {
        &self.roles
    }

    /// P_post 角色（placement/PEQ/gain/delay/polarity）。
    pub fn post(&self) -> &[IrPostRole] {
        &self.post
    }

    /// 曲线绑定（0/1 条；curve 不双叠）。
    pub fn curve(&self) -> Option<&CurveBinding> {
        self.curve.as_ref()
    }

    /// curve 应用次数（0 或 1；G 侧恒无 curve）。
    pub fn curve_application_count(&self) -> usize {
        usize::from(self.curve.is_some()) + usize::from(self.g_residual.curve_id().is_some())
    }

    /// G 残差绑定（rule/participants；无 curve）。
    pub fn g_residual(&self) -> &GResidualBinding {
        &self.g_residual
    }

    /// delay/polarity 编码方式（二选一）。
    pub fn encoding(&self) -> PostEncoding {
        self.encoding
    }

    /// 扩展字段（仅 x_ 前缀；warning 级，不参与应用语义）。
    pub fn extensions(&self) -> &[(String, String)] {
        &self.extensions
    }

    /// 警告（源文件 warning + IR warning）。
    pub fn warnings(&self) -> &[CalibrationWarning] {
        &self.warnings
    }

    /// 按 name 取 P_post 角色。
    pub fn post_role(&self, name: &str) -> Option<&IrPostRole> {
        self.post.iter().find(|role| role.name == name)
    }
}

/// IR 草案构造器（校验 -> 绑定 curve/participants/扩展 -> build 计算 hash）。
#[derive(Clone, Debug)]
pub struct IrDraftBuilder {
    merged: MergedCalibration,
    profile: IrProfileBinding,
    created_unix: u64,
    encoding: PostEncoding,
    curve: Option<CurveBinding>,
    g_residual: GResidualBinding,
    extensions: Vec<(String, String)>,
    warnings: Vec<CalibrationWarning>,
}

impl IrDraftBuilder {
    /// 从合并结果 + profile 绑定构造（校验 fs/role/profile/stereo mask）。
    pub fn new(
        merged: &MergedCalibration,
        profile: IrProfileBinding,
        created_unix: u64,
    ) -> CalResult<Self> {
        if !SAMPLE_RATES_HZ.contains(&merged.sample_rate) {
            return Err(errf(
                CalibrationErrorCode::IrRateUnsupported,
                "source.sample_rate",
                format!(
                    "IR/plan 只支持部署采样率 {SAMPLE_RATES_HZ:?}，实际 {}（44.1k/88.2k 校准文件不能进 v1 plan）",
                    merged.sample_rate
                ),
            ));
        }
        if profile.profile_id.trim().is_empty() {
            return Err(errf(
                CalibrationErrorCode::IrProfile,
                "profile.profile_id",
                "profile_id 不能为空",
            ));
        }
        if !is_sha256_hex(profile.profile_sha256.trim()) {
            return Err(errf(
                CalibrationErrorCode::IrProfile,
                "profile.profile_sha256",
                format!(
                    "profile_sha256 必须是 64 hex，实际 {:?}",
                    profile.profile_sha256
                ),
            ));
        }
        if profile.target_profile.trim().is_empty() {
            return Err(errf(
                CalibrationErrorCode::IrProfile,
                "profile.target_profile",
                "target_profile 不能为空",
            ));
        }
        if merged.roles.is_empty() || merged.roles.len() > MAX_OUTPUT_ROLES {
            return Err(errf(
                CalibrationErrorCode::IrRoles,
                "roles",
                format!(
                    "IR 角色数必须在 1..={MAX_OUTPUT_ROLES}，实际 {}",
                    merged.roles.len()
                ),
            ));
        }
        let mut names: Vec<&str> = Vec::new();
        let mut indexes: Vec<u32> = Vec::new();
        for (position, role) in merged.roles.iter().enumerate() {
            if names.contains(&role.name.as_str()) {
                return Err(errf(
                    CalibrationErrorCode::IrRoles,
                    format!("roles[{position}].name"),
                    format!("IR 角色 name 重复：{}", role.name),
                ));
            }
            names.push(role.name.as_str());
            if indexes.contains(&role.index) {
                return Err(errf(
                    CalibrationErrorCode::IrRoles,
                    format!("roles[{position}].index"),
                    format!("IR 角色 index 重复：{}", role.index),
                ));
            }
            indexes.push(role.index);
        }
        // stereo mask：stereo_strict 时必须恰为 FL/FR；否则名字必须在 role 表内且无重复。
        let mut mask_seen: Vec<&str> = Vec::new();
        for name in &profile.stereo_mask {
            if mask_seen.contains(&name.as_str()) {
                return Err(errf(
                    CalibrationErrorCode::IrStereoMask,
                    "profile.stereo_mask",
                    format!("stereo mask 角色重复：{name}"),
                ));
            }
            mask_seen.push(name.as_str());
            if !names.contains(&name.as_str()) {
                return Err(errf(
                    CalibrationErrorCode::IrStereoMask,
                    "profile.stereo_mask",
                    format!("stereo mask 角色 {name} 不在 IR role 绑定中"),
                ));
            }
        }
        if profile.stereo_strict {
            if profile.stereo_mask.len() != 2
                || profile.stereo_mask[0] != "FL"
                || profile.stereo_mask[1] != "FR"
            {
                return Err(errf(
                    CalibrationErrorCode::IrStereoMask,
                    "profile.stereo_mask",
                    format!(
                        "stereo_strict=true 时 stereo_mask 必须恰为 [FL, FR]，实际 {:?}",
                        profile.stereo_mask
                    ),
                ));
            }
        }
        let warnings = merged.warnings.clone();
        Ok(Self {
            merged: merged.clone(),
            profile,
            created_unix,
            encoding: PostEncoding::CoeffEmbedded,
            curve: None,
            g_residual: GResidualBinding::modal_residual_only(Vec::new()),
            extensions: Vec::new(),
            warnings,
        })
    }

    /// 选择 delay/polarity 编码（默认 CoeffEmbedded；二选一，禁止重复计）。
    pub fn with_encoding(mut self, encoding: PostEncoding) -> Self {
        self.encoding = encoding;
        self
    }

    /// 绑定目标曲线（最多一次；重复绑定 -> E_IR_CURVE_DOUBLE_APPLY）。
    pub fn with_curve(&mut self, curve: TargetCurveRef) -> CalResult<()> {
        if self.curve.is_some() {
            return Err(errf(
                CalibrationErrorCode::IrCurveDoubleApply,
                "curve",
                "目标曲线已绑定过（curve 不双叠：只允许在基线 P 的 TargetFir 阶段应用一次）",
            ));
        }
        if curve.fs_hz != self.merged.sample_rate as f64 {
            return Err(errf(
                CalibrationErrorCode::IrCurveFs,
                "curve.fs_hz",
                format!(
                    "目标曲线 fs={} 与 IR fs={} 不一致（目标与 P 必须同 fs，禁止重采样复用）",
                    curve.fs_hz, self.merged.sample_rate
                ),
            ));
        }
        if !is_sha256_hex(&curve.target_sha256) {
            return Err(errf(
                CalibrationErrorCode::IrCurveHash,
                "curve.target_sha256",
                "target_sha256 必须是 64 hex",
            ));
        }
        if curve.phase_ref.requires_delay() {
            let delay = curve.target_delay_samples.ok_or_else(|| {
                errf(
                    CalibrationErrorCode::IrCurvePhase,
                    "curve.target_delay_samples",
                    "delay_removed 曲线必须给出 target_delay_samples",
                )
            })?;
            if delay > delay_max_samples(self.merged.sample_rate) {
                return Err(errf(
                    CalibrationErrorCode::IrCurvePhase,
                    "curve.target_delay_samples",
                    "target_delay_samples 超过 200 ms 上限",
                ));
            }
        }
        self.curve = Some(CurveBinding {
            curve_id: curve.curve_id,
            target_sha256: curve.target_sha256,
            fs_hz: curve.fs_hz,
            phase_ref: curve.phase_ref,
            target_delay_samples: curve.target_delay_samples,
            point_count: curve.point_count,
            apply_site: CurveApplySite::TargetFirBaseline,
        });
        Ok(())
    }

    /// 设置 G 残差 participants（G 只 modal residual；curve 恒不绑到 G）。
    pub fn with_g_residual_participants(&mut self, participants: Vec<String>) -> CalResult<()> {
        if participants.len() > MAX_LOW_BAND_PARTICIPANTS {
            return Err(errf(
                CalibrationErrorCode::IrRoles,
                "g_residual.participants",
                format!(
                    "G participants 最多 {MAX_LOW_BAND_PARTICIPANTS} 个，实际 {}",
                    participants.len()
                ),
            ));
        }
        let mut seen: Vec<&str> = Vec::new();
        for name in &participants {
            if seen.contains(&name.as_str()) {
                return Err(errf(
                    CalibrationErrorCode::IrRoles,
                    "g_residual.participants",
                    format!("G participant 重复：{name}"),
                ));
            }
            seen.push(name.as_str());
            if !self.merged.roles.iter().any(|role| &role.name == name) {
                return Err(errf(
                    CalibrationErrorCode::IrRoles,
                    "g_residual.participants",
                    format!("G participant {name} 不在 IR role 绑定中"),
                ));
            }
        }
        self.g_residual = GResidualBinding::modal_residual_only(participants);
        Ok(())
    }

    /// 追加扩展字段（仅 x_ 前缀；非 x_ -> E_IR_FIELD_UNKNOWN，重复 -> E_IR_EXTENSION_DUPLICATE）。
    pub fn with_extension(
        &mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> CalResult<()> {
        let key = key.into();
        if !key.starts_with("x_") {
            return Err(errf(
                CalibrationErrorCode::IrFieldUnknown,
                format!("extensions.{key}"),
                format!("未知扩展字段 {key}（IR 只接受 x_ 前缀扩展；其它未知字段拒收）"),
            ));
        }
        if key.len() < 3 {
            return Err(errf(
                CalibrationErrorCode::IrFieldUnknown,
                "extensions",
                "x_ 扩展字段名不能为空",
            ));
        }
        if self.extensions.iter().any(|(existing, _)| existing == &key) {
            return Err(errf(
                CalibrationErrorCode::IrExtensionDuplicate,
                format!("extensions.{key}"),
                format!("扩展字段重复：{key}"),
            ));
        }
        self.extensions.push((key, value.into()));
        Ok(())
    }

    /// 构建 IR：计算每角色 P_post（placement/taps/编码）、规范化 hash。
    pub fn build(mut self) -> CalResult<IrDraft> {
        if self.curve.is_some() && self.encoding == PostEncoding::RoleMetadata {
            // 不冲突：curve 只影响 TargetFir 阶段；这里保留显式检查点（未来编码扩展时使用）。
        }
        let mut post: Vec<IrPostRole> = Vec::with_capacity(self.merged.roles.len());
        for role in &self.merged.roles {
            // 校准 delay 是整数 samples；用 T12 design_delay_to_plan 验证 plan 映射（q16 必须 0）。
            let plan = design_delay_to_plan(role.delay_samples as f64).map_err(|status| {
                errf(
                    CalibrationErrorCode::IrDelayPlan,
                    format!("roles.{}", role.name),
                    format!(
                        "delay_samples={} 无法映射到 plan（RtStatus={status:?}）",
                        role.delay_samples
                    ),
                )
            })?;
            if plan.delay_q16 != 0 {
                return Err(errf(
                    CalibrationErrorCode::IrDelayPlan,
                    format!("roles.{}", role.name),
                    format!(
                        "校准 delay 为整数 samples，plan delay_q16 必须为 0，实际 {}",
                        plan.delay_q16
                    ),
                ));
            }
            let p_taps = match self.encoding {
                PostEncoding::CoeffEmbedded => role.delay_samples + 1,
                PostEncoding::RoleMetadata => 1,
            };
            let mut placement: Vec<PostStage> = Vec::new();
            if !role.peq.is_empty() {
                placement.push(PostStage::ModalPeq);
            }
            if self.curve.is_some() {
                placement.push(PostStage::TargetFir);
            }
            placement.push(PostStage::Gain);
            if role.delay_samples > 0 {
                placement.push(PostStage::Delay);
            }
            if role.polarity == Polarity::Invert {
                placement.push(PostStage::Polarity);
            }
            placement.push(PostStage::Protection);
            // placement 必须按规范链序严格递增。
            for pair in placement.windows(2) {
                if pair[0].chain_index() >= pair[1].chain_index() {
                    return Err(errf(
                        CalibrationErrorCode::IrEncodingConflict,
                        format!("roles.{}", role.name),
                        "placement 顺序与 P_post 规范链序不一致",
                    ));
                }
            }
            post.push(IrPostRole {
                name: role.name.clone(),
                index: role.index,
                delay_samples: role.delay_samples,
                delay_q16: 0,
                polarity: role.polarity,
                gain_db: role.gain_db,
                peq: role.peq.clone(),
                placement,
                p_taps,
            });
        }
        let roles = post
            .iter()
            .map(|role| IrRoleRef {
                name: role.name.clone(),
                index: role.index,
            })
            .collect();
        let source = IrSource {
            schema: self.merged.schema,
            schema_string: self.merged.schema_string.clone(),
            source_sha256: self.merged.source_sha256.clone(),
            sample_rate: self.merged.sample_rate,
            legacy_v1: self.merged.schema == CalibrationSchema::V1,
        };
        // 扩展字段按 key 排序，保证 hash 与插入顺序无关。
        self.extensions.sort_by(|left, right| left.0.cmp(&right.0));
        let mut draft = IrDraft {
            schema: IR_SCHEMA.to_string(),
            ir_version: IR_VERSION,
            created_unix: self.created_unix,
            hash: String::new(),
            source,
            layout_id: self.merged.layout_id.clone(),
            profile: self.profile,
            roles,
            post,
            curve: self.curve,
            g_residual: self.g_residual,
            encoding: self.encoding,
            extensions: self.extensions,
            warnings: self.warnings,
        };
        draft.hash = sha256_hex(draft.canonical_hash_input().as_bytes());
        Ok(draft)
    }
}

// ===========================================================================
// 规范化序列化与 ir_hash（确定性；curve/role/fs/D/taps/placement 全入 hash）
// ===========================================================================

fn push_json_escaped(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other if (other as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", other as u32));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

fn push_json_key_str(out: &mut String, key: &str, value: &str) {
    out.push('"');
    out.push_str(key);
    out.push_str("\":");
    push_json_escaped(out, value);
}

fn push_json_key_int(out: &mut String, key: &str, value: u64) {
    out.push('"');
    out.push_str(key);
    out.push_str("\":");
    out.push_str(&value.to_string());
}

fn push_json_key_bool(out: &mut String, key: &str, value: bool) {
    out.push('"');
    out.push_str(key);
    out.push_str("\":");
    out.push_str(if value { "true" } else { "false" });
}

/// f64 以 IEEE-754 位模式十六进制串序列化（跨平台确定性；非有限值已在入口拒收）。
fn push_json_key_f64_bits(out: &mut String, key: &str, value: f64) {
    out.push('"');
    out.push_str(key);
    out.push_str("\":\"0x");
    out.push_str(&format!("{:016x}", value.to_bits()));
    out.push('"');
}

impl IrDraft {
    fn canonical_json(&self, include_created_unix: bool) -> String {
        let mut out = String::new();
        out.push('{');
        push_json_key_str(&mut out, "schema", &self.schema);
        out.push(',');
        push_json_key_int(&mut out, "ir_version", self.ir_version as u64);
        if include_created_unix {
            out.push(',');
            push_json_key_int(&mut out, "created_unix", self.created_unix);
        }
        out.push_str(",\"source\":{");
        push_json_key_str(&mut out, "schema", &self.source.schema_string);
        out.push(',');
        push_json_key_str(&mut out, "source_sha256", &self.source.source_sha256);
        out.push(',');
        push_json_key_int(&mut out, "sample_rate", self.source.sample_rate as u64);
        out.push(',');
        push_json_key_bool(&mut out, "legacy_v1", self.source.legacy_v1);
        out.push('}');
        out.push_str(",\"layout\":");
        match &self.layout_id {
            Some(layout) => push_json_escaped(&mut out, layout),
            None => out.push_str("null"),
        }
        out.push_str(",\"profile\":{");
        push_json_key_str(&mut out, "profile_id", &self.profile.profile_id);
        out.push(',');
        push_json_key_str(&mut out, "profile_sha256", &self.profile.profile_sha256);
        out.push(',');
        push_json_key_bool(&mut out, "stereo_strict", self.profile.stereo_strict);
        out.push_str(",\"stereo_mask\":[");
        for (index, name) in self.profile.stereo_mask.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            push_json_escaped(&mut out, name);
        }
        out.push(']');
        out.push(',');
        push_json_key_str(&mut out, "target_profile", &self.profile.target_profile);
        out.push('}');
        out.push_str(",\"roles\":[");
        for (index, role) in self.roles.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push('{');
            push_json_key_str(&mut out, "name", &role.name);
            out.push(',');
            push_json_key_int(&mut out, "index", role.index as u64);
            out.push('}');
        }
        out.push(']');
        out.push_str(",\"post\":[");
        for (index, role) in self.post.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push('{');
            push_json_key_str(&mut out, "name", &role.name);
            out.push(',');
            push_json_key_int(&mut out, "index", role.index as u64);
            out.push(',');
            push_json_key_int(&mut out, "delay_samples", role.delay_samples as u64);
            out.push(',');
            push_json_key_int(&mut out, "delay_q16", role.delay_q16 as u64);
            out.push(',');
            push_json_key_str(&mut out, "polarity", role.polarity.as_str());
            out.push(',');
            push_json_key_f64_bits(&mut out, "gain_db_bits", role.gain_db);
            out.push_str(",\"peq\":[");
            for (peq_index, band) in role.peq.iter().enumerate() {
                if peq_index > 0 {
                    out.push(',');
                }
                out.push('{');
                push_json_key_f64_bits(&mut out, "freq_bits", band.freq_hz);
                out.push(',');
                push_json_key_f64_bits(&mut out, "q_bits", band.q);
                out.push(',');
                push_json_key_f64_bits(&mut out, "gain_bits", band.gain_db);
                out.push(',');
                push_json_key_str(&mut out, "type", band.filter_type.as_str());
                out.push('}');
            }
            out.push(']');
            out.push_str(",\"placement\":[");
            for (stage_index, stage) in role.placement.iter().enumerate() {
                if stage_index > 0 {
                    out.push(',');
                }
                push_json_escaped(&mut out, stage.as_str());
            }
            out.push(']');
            out.push(',');
            push_json_key_int(&mut out, "p_taps", role.p_taps as u64);
            out.push('}');
        }
        out.push(']');
        out.push_str(",\"curve\":");
        match &self.curve {
            None => out.push_str("null"),
            Some(curve) => {
                out.push('{');
                push_json_key_str(&mut out, "curve_id", &curve.curve_id);
                out.push(',');
                push_json_key_str(&mut out, "target_sha256", &curve.target_sha256);
                out.push(',');
                push_json_key_f64_bits(&mut out, "fs_bits", curve.fs_hz);
                out.push(',');
                push_json_key_str(&mut out, "phase_ref", curve.phase_ref.as_str());
                out.push(',');
                out.push_str("\"target_delay_samples\":");
                match curve.target_delay_samples {
                    Some(delay) => out.push_str(&delay.to_string()),
                    None => out.push_str("null"),
                }
                out.push(',');
                push_json_key_int(&mut out, "point_count", curve.point_count as u64);
                out.push(',');
                push_json_key_str(&mut out, "apply_site", curve.apply_site.as_str());
                out.push('}');
            }
        }
        out.push_str(",\"g_residual\":{");
        push_json_key_str(&mut out, "rule", self.g_residual.rule.as_str());
        out.push_str(",\"participants\":[");
        for (index, name) in self.g_residual.participants.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            push_json_escaped(&mut out, name);
        }
        out.push_str("]},");
        push_json_key_str(&mut out, "encoding", self.encoding.as_str());
        out.push_str(",\"extensions\":[");
        for (index, (key, value)) in self.extensions.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push('[');
            push_json_escaped(&mut out, key);
            out.push(',');
            push_json_escaped(&mut out, value);
            out.push(']');
        }
        out.push_str("]}");
        out
    }

    /// 完整规范化文本（含 created_unix；用于人读/报告，不直接做 hash 输入）。
    pub fn canonical_form(&self) -> String {
        self.canonical_json(true)
    }

    /// ir_hash 的规范化输入（不含 created_unix；warning 不参与语义）。
    pub fn canonical_hash_input(&self) -> String {
        self.canonical_json(false)
    }

    /// curve 单次应用不变式（G 侧无 curve；应用次数 <= 1）。
    pub fn validate_curve_single_application(&self) -> CalResult<()> {
        if self.curve_application_count() > 1 {
            return Err(errf(
                CalibrationErrorCode::IrCurveDoubleApply,
                "curve",
                format!(
                    "curve 应用次数 {} > 1（不双叠规则被破坏）",
                    self.curve_application_count()
                ),
            ));
        }
        if self.g_residual.curve_id().is_some() {
            return Err(errf(
                CalibrationErrorCode::IrGCurve,
                "g_residual.curve_id",
                "G residual 禁止绑定 curve（G 只 modal residual；curve 属于基线 P）",
            ));
        }
        Ok(())
    }
}

// ===========================================================================
// stale 判定（input/fs/role/P/hash 变化即 stale；WBS §5 / compat-matrix Y24）
// ===========================================================================

/// stale 原因（逐条可判；任何一条即需要重新设计/重编译）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StaleReason {
    /// 采样率变化。
    Fs,
    /// layout id 变化。
    Layout,
    /// role -> 物理通道映射变化。
    Roles,
    /// 渲染 profile/P 基线 hash 变化。
    Profile,
    /// 目标曲线 hash 变化（含新增/删除曲线）。
    Target,
    /// 源校准文件 hash 变化。
    Calibration,
}

impl StaleReason {
    /// 字面值（报告用）。
    pub fn as_str(self) -> &'static str {
        match self {
            StaleReason::Fs => "STALE_FS",
            StaleReason::Layout => "STALE_LAYOUT",
            StaleReason::Roles => "STALE_ROLES",
            StaleReason::Profile => "STALE_PROFILE",
            StaleReason::Target => "STALE_TARGET",
            StaleReason::Calibration => "STALE_CALIBRATION",
        }
    }
}

/// 当前运行/设计上下文（调用方在 prepare 期给出；与 IR 记录比较判定 stale）。
#[derive(Clone, Copy, Debug)]
pub struct IrCurrent<'a> {
    /// 当前 profile 采样率。
    pub fs_hz: u32,
    /// 当前 layout id（None=无 layout 绑定）。
    pub layout_id: Option<&'a str>,
    /// 当前 role -> 物理通道映射（有序；与 IR 记录比较）。
    pub role_indices: &'a [(&'a str, u32)],
    /// 当前渲染 profile 文档 hash。
    pub profile_sha256: &'a str,
    /// 当前目标曲线 hash（None=当前无曲线）。
    pub target_sha256: Option<&'a str>,
    /// 当前源校准文件 hash。
    pub calibration_sha256: &'a str,
}

impl IrDraft {
    /// 计算 stale 原因（空 = fresh；顺序固定 Fs/Layout/Roles/Profile/Target/Calibration）。
    pub fn stale_reasons(&self, current: &IrCurrent<'_>) -> Vec<StaleReason> {
        let mut reasons = Vec::new();
        if current.fs_hz != self.source.sample_rate {
            reasons.push(StaleReason::Fs);
        }
        if current.layout_id != self.layout_id.as_deref() {
            reasons.push(StaleReason::Layout);
        }
        let roles_match = current.role_indices.len() == self.roles.len()
            && current.role_indices.iter().zip(self.roles.iter()).all(
                |((current_name, current_index), stored)| {
                    *current_name == stored.name.as_str() && *current_index == stored.index
                },
            );
        if !roles_match {
            reasons.push(StaleReason::Roles);
        }
        if current.profile_sha256 != self.profile.profile_sha256 {
            reasons.push(StaleReason::Profile);
        }
        let stored_target = self
            .curve
            .as_ref()
            .map(|curve| curve.target_sha256.as_str());
        if current.target_sha256 != stored_target {
            reasons.push(StaleReason::Target);
        }
        if current.calibration_sha256 != self.source.source_sha256 {
            reasons.push(StaleReason::Calibration);
        }
        reasons
    }

    /// 是否 stale（任一原因）。
    pub fn is_stale(&self, current: &IrCurrent<'_>) -> bool {
        !self.stale_reasons(current).is_empty()
    }
}

// ===========================================================================
// 与 T12 接口：P 阶段投影（StagePlan/CompileOptions/design_delay_to_plan）
// ===========================================================================

/// IR 的单角色 plan 元数据（对齐 core-v1 §11.10 TspiRoleV1/TspiDiagFirV1 语义；
/// 具体 plan 字节打包归 T26/T66）。
#[derive(Clone, Debug, PartialEq)]
pub struct RolePlanMetadata {
    /// 角色 id。
    pub name: String,
    /// plan 内的输出 role 序号（0..O-1，非物理通道号）。
    pub role_index: u16,
    /// 物理输出通道号（scatter 前）。
    pub out_channel: u16,
    /// plan role/Diag 的整数延迟（RoleMetadata 编码 = D；CoeffEmbedded 编码 = 0）。
    pub delay_samples: u32,
    /// 次样本延迟（校准整数 D 恒 0；对齐 v1.1 I01）。
    pub delay_q16: u32,
    /// plan role 表极性乘子（RoleMetadata = ±1；CoeffEmbedded = +1，已折入系数避免双乘）。
    pub polarity: i8,
    /// 合并后 role gain（dB；Gain 节点一次应用，不折入 P FIR）。
    pub gain_db: f64,
    /// 实际总延迟（两种编码一致；crossfade D 对齐/报告用 total_delay_q16 口径）。
    pub total_delay_samples: u32,
}

/// IR 的 P 阶段投影（直接可喂 T12 CompileInput.p/l_p 与 StagePlan 列表）。
#[derive(Clone, Debug, PartialEq)]
pub struct CalibrationPostStage {
    /// 采样率（Hz）。
    pub sample_rate_hz: u32,
    /// 输出角色数 O。
    pub n_role: u16,
    /// 行主序 P FIR 的公共声明支撑 L_P（各行零填充到该长度）。
    pub l_p: u32,
    /// 行主序 f64 系数（O x L_P；delay/polarity 已按编码折入或留在 role 元数据）。
    pub p: Vec<f64>,
    /// 逐角色 core::compile::StagePlan 投影（taps/delay/polarity/coeffs_f32）。
    pub stage_plans: Vec<StagePlan>,
    /// 逐角色 plan 元数据。
    pub roles: Vec<RolePlanMetadata>,
    /// delay/polarity 编码（二选一；用于防止 T26 重复应用）。
    pub encoding: PostEncoding,
    /// 最大实际总延迟（samples；crossfade D 对齐）。
    pub max_total_delay_samples: u32,
    /// 是否有 P FIR 的 coefficient-embedded 延迟（CoeffEmbedded 且最大延迟 > 0）。
    pub delay_embedded_in_coefficients: bool,
}

impl CalibrationPostStage {
    /// 按 role 序号取 P FIR 行。
    pub fn p_row(&self, role_index: usize) -> Option<&[f64]> {
        let l_p = self.l_p as usize;
        let start = role_index.checked_mul(l_p)?;
        let end = start.checked_add(l_p)?;
        self.p.get(start..end)
    }
}

impl IrDraft {
    /// 角色 P FIR 行（按编码：CoeffEmbedded = delta(D)*polarity；RoleMetadata = [1.0]）。
    pub fn p_fir_row(&self, name: &str) -> CalResult<Vec<f64>> {
        let role = self.post_role(name).ok_or_else(|| {
            errf(
                CalibrationErrorCode::IrRoles,
                format!("roles.{name}"),
                format!("IR 中没有角色 {name}"),
            )
        })?;
        match self.encoding {
            PostEncoding::CoeffEmbedded => {
                let taps = role.p_taps as usize;
                if taps == 0 || role.delay_samples as usize >= taps {
                    return Err(errf(
                        CalibrationErrorCode::IrEncodingConflict,
                        format!("roles.{name}.p_taps"),
                        "CoeffEmbedded 编码的 p_taps 必须 = delay+1",
                    ));
                }
                let mut row = vec![0.0; taps];
                row[role.delay_samples as usize] = role.polarity.multiplier() as f64;
                Ok(row)
            }
            PostEncoding::RoleMetadata => Ok(vec![1.0]),
        }
    }

    /// 最大实际总延迟（samples；crossfade D 对齐用）。
    pub fn max_total_delay_samples(&self) -> u32 {
        self.post
            .iter()
            .map(|role| role.delay_samples)
            .max()
            .unwrap_or(0)
    }

    /// 逐角色 plan 元数据（不生成 P 系数）。
    pub fn role_plan_metadata(&self) -> CalResult<Vec<RolePlanMetadata>> {
        let mut out = Vec::with_capacity(self.post.len());
        for (role_index, role) in self.post.iter().enumerate() {
            let role_u16 = u16::try_from(role_index).map_err(|_| {
                errf(
                    CalibrationErrorCode::IrRoles,
                    format!("roles.{}", role.name),
                    "role 序号超出 u16",
                )
            })?;
            let out_channel = u16::try_from(role.index).map_err(|_| {
                errf(
                    CalibrationErrorCode::IrRoles,
                    format!("roles.{}", role.name),
                    format!("物理通道号 {} 超出 u16", role.index),
                )
            })?;
            let (delay, polarity) = match self.encoding {
                PostEncoding::CoeffEmbedded => (0u32, 1i8),
                PostEncoding::RoleMetadata => (role.delay_samples, role.polarity.multiplier()),
            };
            out.push(RolePlanMetadata {
                name: role.name.clone(),
                role_index: role_u16,
                out_channel,
                delay_samples: delay,
                delay_q16: 0,
                polarity,
                gain_db: role.gain_db,
                total_delay_samples: role.delay_samples,
            });
        }
        Ok(out)
    }

    /// 生成 P 阶段投影（无 profile 上限校验；调用方用 p_stage_with_options 做准入）。
    pub fn p_stage(&self) -> CalResult<CalibrationPostStage> {
        let mut rows: Vec<Vec<f64>> = Vec::with_capacity(self.post.len());
        for role in &self.post {
            rows.push(self.p_fir_row(&role.name)?);
        }
        let l_p = rows.iter().map(|row| row.len()).max().unwrap_or(1).max(1);
        let n_role = u16::try_from(self.post.len())
            .map_err(|_| errf(CalibrationErrorCode::IrRoles, "roles", "角色数超出 u16"))?;
        let mut p = Vec::with_capacity(rows.len() * l_p);
        let mut stage_plans = Vec::with_capacity(rows.len());
        let mut roles = Vec::with_capacity(rows.len());
        let metadata = self.role_plan_metadata()?;
        for (index, (row, meta)) in rows.into_iter().zip(metadata.into_iter()).enumerate() {
            let mut row = row;
            row.resize(l_p, 0.0);
            let _ = index;
            stage_plans.push(StagePlan {
                index: meta.role_index,
                in_index: meta.role_index,
                taps: l_p as u32,
                delay_samples: meta.delay_samples,
                delay_q16: meta.delay_q16,
                polarity: meta.polarity,
                flags: 0,
                coeffs_f32: row.iter().map(|value| *value as f32).collect(),
            });
            p.extend_from_slice(&row);
            roles.push(meta);
        }
        let max_total_delay_samples = self.max_total_delay_samples();
        Ok(CalibrationPostStage {
            sample_rate_hz: self.source.sample_rate,
            n_role,
            l_p: l_p as u32,
            p,
            stage_plans,
            roles,
            encoding: self.encoding,
            max_total_delay_samples,
            delay_embedded_in_coefficients: self.encoding == PostEncoding::CoeffEmbedded
                && max_total_delay_samples > 0,
        })
    }

    /// 构造并校验 CompileOptions（T12 接口；fs 与 IR 一致）。
    pub fn compile_options(
        &self,
        block_size: u16,
        max_taps: u32,
        extended_taps_admitted: bool,
        coeff_budget_bytes: u64,
    ) -> CalResult<CompileOptions> {
        let options = CompileOptions {
            sample_rate_hz: self.source.sample_rate,
            block_size,
            max_taps,
            extended_taps_admitted,
            coeff_budget_bytes,
        };
        let status = options.validate();
        if !status.is_ok() {
            return Err(errf(
                CalibrationErrorCode::IrCompileOptions,
                "compile_options",
                format!("CompileOptions 非法（RtStatus={status:?}）"),
            ));
        }
        Ok(options)
    }

    /// P 阶段 + profile 准入校验（fs/taps/系数预算；不静默截尾）。
    pub fn p_stage_with_options(
        &self,
        options: &CompileOptions,
    ) -> CalResult<CalibrationPostStage> {
        let status = options.validate();
        if !status.is_ok() {
            return Err(errf(
                CalibrationErrorCode::IrCompileOptions,
                "compile_options",
                format!("CompileOptions 非法（RtStatus={status:?}）"),
            ));
        }
        if options.sample_rate_hz != self.source.sample_rate {
            return Err(errf(
                CalibrationErrorCode::IrRateMismatch,
                "compile_options.sample_rate_hz",
                format!(
                    "CompileOptions.sample_rate_hz={} 与 IR fs={} 不一致",
                    options.sample_rate_hz, self.source.sample_rate
                ),
            ));
        }
        let stage = self.p_stage()?;
        if stage.l_p > options.max_taps {
            return Err(errf(
                CalibrationErrorCode::IrTapsBudget,
                "p.l_p",
                format!(
                    "P FIR 支撑 {} taps 超过 profile max_taps={}（INFEASIBLE；禁止静默截尾）",
                    stage.l_p, options.max_taps
                ),
            ));
        }
        let coeff_bytes = stage.n_role as u64 * stage.l_p as u64 * 4;
        if coeff_bytes > options.coeff_budget_bytes {
            return Err(errf(
                CalibrationErrorCode::IrCapacity,
                "p",
                format!(
                    "P f32 系数 {coeff_bytes} B 超过预算 {} B",
                    options.coeff_budget_bytes
                ),
            ));
        }
        Ok(stage)
    }

    /// 默认 profile 常量下的 P 阶段（常规 taps 4096 / 4 MiB 预算；48k/128）。
    pub fn p_stage_default(&self) -> CalResult<CalibrationPostStage> {
        let options = self.compile_options(128, TAPS_NORMAL_MAX, false, 4 * 1024 * 1024)?;
        self.p_stage_with_options(&options)
    }
}

const _: () = {
    assert!(TAPS_NORMAL_MAX == 4096);
    assert!(TAPS_EXTENDED_MAX == 16_384);
};
