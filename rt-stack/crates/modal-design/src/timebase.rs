//! [NONRT] 公共时间轴/相位/ppm（T49） ——WBS T49 写域。
//!
//! 本模块是 P1 低频 MIMO 设计链的**时间轴/相位重建层**：把 T48 ingest
//! （`khaen-modal-design::ingest`，契约 `tspi-modal-ingest/v1`）已经校验过的
//! raw/source/diag/position 产物读成**公共时间基准**，供 T50（target）/T52–T54
//! （solver/validate/artifact）与 T63/T66/T67/T68（多点/编译应用/复测/独立
//! checker）消费。本模块是纯计算：**不采音、不写盘、不做解卷积、不打开任何音频
//! 设备/进程**；所有输入都来自 T48 的强类型结构（或真实 `read_bundle()` 结果）。
//!
//! 权威输入（只读）：
//! - WBS T49 行与 §1/§6/§7（scratch/lead/planner/TSPI-RT-STACK-WBS.md）；
//! - DSP-HOST-DECISION §2.3（公共时间轴/clock_ppm：按实际 crop/coarse/frac/slot
//!   恢复，不盲套 CSV 标签或**双扣 fractional delay**，避免**物理极性重复乘**）；
//! - docs/MIMO-COEFF-PLAN.md §2.4a（fr.csv 相位语义：`absolute_phase_deg =
//!   phase_deg - 360*f*delay_samples/fs`，减号；不得用 CONTRACT 的相对整数 delay）；
//! - docs/MIMO-ROADMAP.md §3.3（τ=137 @48k 符号单测：23–300 Hz 相位残差 ≤1.0°，
//!   τ=0/137/411 线性；>1° 即判符号或字段错，不许调阈值放过）；
//! - rt-stack/spec/mimo-t6.md FC-04/FC-05/FC-06/FC-08、§2.1 timeline/clock、§4.1
//!   （只扣声明公共 D0；极性只依据声明字段，**不得在分析中再次乘极性**）、§4.2；
//! - rt-stack/spec/core-v1.md §3.1/§3.2（容量/采样率常量）与 §12.1/§12.2（本 crate
//!   无 RT 区，[NONRT]）；
//! - rt-stack/README.md §3（写域纪律）与 evidence/T48/INPUT-FORMAT.md（输入契约）。
//!
//! # 冻结的时基模型（tspi-modal-timebase/v1）
//!
//! 一个 position 是一次连续 UMIK 1ch 采集；其中按 slot 依次播放 M 个 participant
//! 的扫频（每个 slot R 遍）。MOTU render frame 域与 UMIK capture sample 域是**两个独立
//! 时钟域**（UMIK 是独立 USB ADC，不与 MOTU 同源）。记
//!
//! ~~~text
//! capture_sample = capture_origin + (render_frame - render_origin) * samples_per_frame
//! samples_per_frame = (fs_capture / fs_render) * (1 + residual_ppm / 1e6)
//! ~~~
//!
//! 本模块要求 capture/analysis 采样率 == session 采样率（[validate_capture_file] 与
//! [reconstruct_position] 复核），因此上式分母 ratio 恒为 1：
//! `samples_per_frame = 1 + residual_ppm/1e6`（见 [ClockMap::samples_per_frame]）。保留
//! 比值形式仅说明一般定义；跨 rate 重采样（如 96k capture vs 48k render）不在 T49 支持
//! 范围，ratio≠1 不得被误读成本模块已处理。`residual_ppm` 是**记录坐标里仍残留**的相对
//! 时钟漂移，不是绝对采样率。
//!
//! - `render_frame_index`（diag，MOTU/epoch 域）与 `nominal_start`/`actual_start`
//!   （diag，UMIK capture 域）重建**残差 clock_ppm**（记录坐标里仍残留的 ppm）：
//!   1. `RenderGridLsq`：对 (render_frame_index, nominal_start) 做最小二乘（斜率−1）；
//!   2. `OnsetDelayLsq`：对 (render_frame_index, actual_start − analysis.delay_samples)
//!      做最小二乘（不同 slot 的传播延迟被减掉，常数原点在主值上相消）；
//!   3. `RepeatInterval`：同一 slot 相邻 repeat 的实测起点间隔 / ((sweep+gap)*fs)。
//!   三法独立（不同物理量、不同字段）→ 交叉一致性；再与 diag 声明的 `clock` 对拍。
//! - 声明口径（FC-06/B2 T2）：必须同时有 `clock_ppm` 与 `clock_ppm_uncertainty_ppm`；
//!   只有 |ppm| > 2σ 才算漂移并要求声明校正（`correction`）。`correction=resample`
//!   表示坐标已按声明 ppm 校正 → 记录坐标里的**残差** ppm 应≈0（本模块校验）；
//!   `correction=none` 表示坐标未校正 → 残差 ppm 必须与声明一致（本模块校验）。
//! - `delay` 的三个独立表示（**去相位只扣公共整数 D0 一次**）：diag 的 `lag_samples =
//!   actual_start − nominal_start`（槽栅格原点）、analysis 的 `delay_samples`（producer
//!   逐 participant 实测传播延迟，含分数/距离/尘埃项）、以及 T6 FC-04 的**预声明整数公共
//!   D0**（[CommonD0]）。diag lag 与 measured delay 只应差一个**常数原点**（pre-guard/
//!   onset 偏差；逐 slot 相同）；本模块用中位数估计并检查残差 spread，`lag` **只用于**
//!   槽对齐/一致性。去相位**只扣 D0 一次**；measured delay 只记录为
//!   `analysis_delay_samples` 与 `per_participant_residual_delay_samples = measured − D0`
//!   （provenance；不进入任何相位扣除，供 T6/T50 报告残余群延迟）。
//! - `fr` 相位：`phase_ref` 解析为 [PhaseBasis]：
//!   `AlignedSegment`（测量相位含传播延迟；绝对相位 = phase − 360·f·D0/fs，**只减 D0 一次**）、
//!   `Absolute`（已含总相位：**绝不能再扣** D0/delay，否则就是"双去相位"）、
//!   `DelayRemoved`（v1 ingest 无可机读声明 D → 不支持，拒绝）。
//!   [AbsoluteResponse] 只能由 [de_phase_analysis] 构造，类型层面不提供再次扣延迟的
//!   入口；`delay_removed_samples` 记录本模块到底扣了多少（恒等于声明的 D0 或 0）。
//! - 预声明方式：D0 不在 T48 ingest 文件里（FC-04 属冻结条件），由调用方以 [CommonD0]
//!   显式传入；本模块把 D0 写进全部产物/摘要（`common_d0_samples`）供 T6 冻结清单对拍，
//!   并且**不接受**在相位重建后做逐通道/逐测点"最佳对齐"平移（FC-04 禁止事后平移）。
//! - 极性：analysis.polarity ∈ {−1, 0, 1}；0=unknown → 拒（MIMO 不得猜极性）。
//!   [AbsoluteResponse::normalize_polarity] 只允许乘一次（第二次 → `E_TB_DOUBLE_POLARITY`）；
//!   `AbsoluteResponse::polarity()` 记录是否已归一（`None` = 仍为实测物理极性）。
//! - MOTU 可选电学 reference：**另一独立通道/文件**（`session.reference` +
//!   `position.reference`），不是 UMIK；只用于提供公共相位基准。`independent` 必须
//!   true、设备必须 ≠ UMIK 采集设备；reference 的 analysis 由调用方提供（T62/复测层
//!   产物），本模块只负责去相位一次并给出相对响应 [RelativeResponse]。
//!
//! # 错误码（稳定，供 T50+ 与证据链引用）
//!
//! | code | 语义 |
//! | --- | --- |
//! | E_TB_CLOCK_CONFLICT | 独立 ppm 估计互相矛盾 / 与声明 ppm 矛盾 / 声明 resample 后仍有残差漂移 |
//! | E_TB_CLOCK_DRIFT | 显著漂移（|ppm|>2σ）但未声明校正，或声明校正后残差仍显著 |
//! | E_TB_SLOT_ORDER | slot 覆盖/顺序/speaker/out_index/跨 position 参与者不一致 |
//! | E_TB_DELAY_ORIGIN | diag lag 与 analysis delay 的逐 slot 原点偏移不是常数（spread 超门） |
//! | E_TB_PHASE_REF | `phase_ref` 未知/语义不符 v1，或跨 slot 相位基准不一致 |
//! | E_TB_DOUBLE_DEPHASE | 对已经是 absolute 的响应再次要求扣延迟（防双去相位） |
//! | E_TB_POLARITY_UNKNOWN | analysis.polarity=0/非 ±1（MIMO 不得猜极性） |
//! | E_TB_DOUBLE_POLARITY | 同一响应第二次归一化极性（防双反相） |
//! | E_TB_PHASE_GRID | 相对响应两侧频率网格不一致 |
//! | E_TB_REFERENCE | reference 未声明/非独立/与 UMIK 同设备/raw 与声明不符/缺 reference analysis |
//! | E_TB_COMMON_D | 公共整数 D0（T6 FC-04）越界/非法声明（|D0| > COMMON_D0_SANITY_SAMPLES） |
//! | E_TB_RANGE | 非有限值/越界/字段自洽性失败（delay_coarse/delay_ms 与 delay_samples 不一致）/数值下溢 |
//! | E_TB_MISSING | 缺 position/measurement/必需时基信息 |
//! | E_TB_UNSUPPORTED | 采样率/采集形态不符合 P1（48/96k、UMIK 1ch） |
//!
//! # 写域与不变量
//!
//! - 只读纯计算：本文件不含文件/网络/进程/线程/音频设备 API（单测静态守卫）。
//! - 仅 [NONRT] 控制面使用；禁止在 RT 区调用。
//! - 不注册公共 mod/CLI、不改 Cargo.toml；`timebase` 已由 T06 在 lib.rs 注册。
//! - 确定性：同输入 → 同输出（无随机、无时间、无浮点顺序依赖以外的状态）。
//!
//! # 未测（NOT_RUN）
//!
//! 本模块只在合成夹具与 T48 契约结构上验证；**无真实 daemon 产物/板端采集/UMIK·MOTU
//! 实测**；reference.raw 的声学/电学反卷积不在本模块（T62/T50）；不声明任何真实
//! room/clock 数值。

#![forbid(unsafe_code)]

use crate::ingest::{
    CaptureFile, DriftCorrection, IngestBundle, IngestMeasurement, IngestPosition, IngestSession,
    PositionRole, SampleFormat, SourceAnalysis, TimebaseDiag,
};
use khaen_core::rt::{
    MAX_INPUT_CHANNELS, MAX_LOW_BAND_PARTICIPANTS, MAX_OUTPUT_CHANNELS, SAMPLE_RATES_HZ,
};

// ===========================================================================
// schema / 冻结常量
// ===========================================================================

/// 时基重建产物 schema（T50/T54/T63/T66 报告引用）。
pub const TIMEBASE_SCHEMA: &str = "tspi-modal-timebase/v2";

/// 独立 ppm 估计之间允许的最大偏差（ppm；另有 3σ 项）。
pub const CLOCK_AGREEMENT_TOLERANCE_PPM: f64 = 2.0;
/// 残差 ppm 与 diag 声明 ppm 的允许偏差（correction=none；另有 3σ 项）。
pub const CLOCK_DECLARED_TOLERANCE_PPM: f64 = 2.0;
/// correction=resample 时记录坐标允许残留的 ppm（另有 3σ 项）。
pub const CLOCK_RESIDUAL_TOLERANCE_PPM: f64 = 1.0;
/// 一致性判据的 σ 倍数（FC-06：2σ 判显著；本模块交叉校验用 3σ 容差）。
pub const CLOCK_SIGMA_FACTOR: f64 = 3.0;
/// 加权平均时 ppm 不确定度下限（避免精确夹具 σ=0 造成除零/无穷权重）。
pub const CLOCK_WEIGHT_FLOOR_PPM: f64 = 1.0e-6;
/// 声明/独立 ppm 的 sanity 上界（ingest 已校验；直接结构复用同一上界）。
pub const CLOCK_PPM_SANITY_LIMIT: f64 = 1000.0;
/// ppm 不确定度的 sanity 上界。
pub const CLOCK_UNCERTAINTY_SANITY_LIMIT: f64 = 1000.0;

/// diag lag 与 analysis delay 的原点偏移逐 slot 残差允许上限（样本）。
pub const DELAY_ORIGIN_SPREAD_MAX_SAMPLES: f64 = 0.5;
/// repeat 间隔相对模型（(sweep+gap)*fs*sppf）的残差告警门（样本）。
pub const REPEAT_INTERVAL_RESIDUAL_TOLERANCE_SAMPLES: f64 = 2.0;
/// slot 间隔相对预期（R*(sweep+gap)*fs）的相对容差。
pub const SLOT_SPACING_RELATIVE_TOLERANCE: f64 = 0.05;
/// slot 间隔相对预期的绝对容差（帧）。
pub const SLOT_SPACING_ABSOLUTE_TOLERANCE_FRAMES: f64 = 2.0;
/// lag_samples 的 sanity 上界（ingest 用 |lag|≤fs；本模块同口径）。
pub const DELAY_SANITY_FACTOR: f64 = 1.0;
/// 复数除法分母下限（避免 0/极小参考响应）。
pub const COMPLEX_DIVISION_FLOOR: f64 = 1.0e-30;
/// T6 FC-04 公共整数 D0 的 sanity 上界（样本；±1e6 ≈ ±20.8 s @48k）。
///
/// 这只是防错声明/溢出的 sanity 上界，不是 T6 验收门；正常声明的 D0 远小于此。
pub const COMMON_D0_SANITY_SAMPLES: i64 = 1_000_000;
/// delay_ms 与 delay_samples/fs*1000 的绝对一致性容差（ms；1 ns，兼容 6 位小数 ms 记录）。
pub const DELAY_MS_ABSOLUTE_TOLERANCE_MS: f64 = 1.0e-6;
/// delay_ms 一致性的相对项（避免 20 s 级延迟把绝对容差压得过严）。
pub const DELAY_MS_RELATIVE_TOLERANCE: f64 = 1.0e-9;

// ===========================================================================
// 错误
// ===========================================================================

/// 时基重建错误种类（机器码见 [TimebaseErrorKind::code]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimebaseErrorKind {
    /// 独立 ppm 估计互相矛盾 / 与声明矛盾 / resample 后仍有残差漂移。
    ClockConflict,
    /// 显著漂移未声明校正，或声明校正后残差仍显著。
    ClockDrift,
    /// slot 覆盖/顺序/speaker/out_index/跨 position 参与者不一致。
    SlotOrder,
    /// diag lag 与 analysis delay 的逐 slot 原点偏移不是常数。
    DelayOrigin,
    /// `phase_ref` 未知/语义不符 v1，或跨 slot 相位基准不一致。
    PhaseReference,
    /// 对已是 absolute 的响应再次扣延迟（双去相位）。
    DoubleDePhase,
    /// analysis.polarity 未知（0/非 ±1）。
    PolarityUnknown,
    /// 同一响应第二次归一化极性。
    DoublePolarity,
    /// 相对响应两侧频率网格不一致。
    PhaseGrid,
    /// MOTU 电学 reference 通道声明/文件/分析不合法。
    Reference,
    /// 公共整数 D0（T6 FC-04）声明越界/非法。
    CommonDelay,
    /// 非有限值/越界/字段自洽性失败/数值下溢。
    Range,
    /// 缺 position/measurement/必需时基信息。
    Missing,
    /// 采样率/采集形态不符合 P1。
    Unsupported,
}

impl TimebaseErrorKind {
    /// 稳定机器码（证据/报告引用）。
    pub const fn code(self) -> &'static str {
        match self {
            TimebaseErrorKind::ClockConflict => "E_TB_CLOCK_CONFLICT",
            TimebaseErrorKind::ClockDrift => "E_TB_CLOCK_DRIFT",
            TimebaseErrorKind::SlotOrder => "E_TB_SLOT_ORDER",
            TimebaseErrorKind::DelayOrigin => "E_TB_DELAY_ORIGIN",
            TimebaseErrorKind::PhaseReference => "E_TB_PHASE_REF",
            TimebaseErrorKind::DoubleDePhase => "E_TB_DOUBLE_DEPHASE",
            TimebaseErrorKind::PolarityUnknown => "E_TB_POLARITY_UNKNOWN",
            TimebaseErrorKind::DoublePolarity => "E_TB_DOUBLE_POLARITY",
            TimebaseErrorKind::PhaseGrid => "E_TB_PHASE_GRID",
            TimebaseErrorKind::Reference => "E_TB_REFERENCE",
            TimebaseErrorKind::CommonDelay => "E_TB_COMMON_D",
            TimebaseErrorKind::Range => "E_TB_RANGE",
            TimebaseErrorKind::Missing => "E_TB_MISSING",
            TimebaseErrorKind::Unsupported => "E_TB_UNSUPPORTED",
        }
    }
}

/// 时基重建错误：kind + 逻辑上下文（如 position.p01.slot.2）+ 人读细节。
#[derive(Debug, Clone, PartialEq)]
pub struct TimebaseError {
    kind: TimebaseErrorKind,
    context: String,
    detail: String,
}

impl TimebaseError {
    fn new(kind: TimebaseErrorKind, context: impl Into<String>, detail: impl Into<String>) -> Self {
        TimebaseError {
            kind,
            context: context.into(),
            detail: detail.into(),
        }
    }

    fn clock_conflict(context: impl Into<String>, detail: impl Into<String>) -> Self {
        TimebaseError::new(TimebaseErrorKind::ClockConflict, context, detail)
    }

    fn clock_drift(context: impl Into<String>, detail: impl Into<String>) -> Self {
        TimebaseError::new(TimebaseErrorKind::ClockDrift, context, detail)
    }

    fn slot_order(context: impl Into<String>, detail: impl Into<String>) -> Self {
        TimebaseError::new(TimebaseErrorKind::SlotOrder, context, detail)
    }

    fn delay_origin(context: impl Into<String>, detail: impl Into<String>) -> Self {
        TimebaseError::new(TimebaseErrorKind::DelayOrigin, context, detail)
    }

    fn phase_ref(context: impl Into<String>, detail: impl Into<String>) -> Self {
        TimebaseError::new(TimebaseErrorKind::PhaseReference, context, detail)
    }

    fn polarity_unknown(context: impl Into<String>, detail: impl Into<String>) -> Self {
        TimebaseError::new(TimebaseErrorKind::PolarityUnknown, context, detail)
    }

    fn double_polarity(context: impl Into<String>, detail: impl Into<String>) -> Self {
        TimebaseError::new(TimebaseErrorKind::DoublePolarity, context, detail)
    }

    fn common_d(context: impl Into<String>, detail: impl Into<String>) -> Self {
        TimebaseError::new(TimebaseErrorKind::CommonDelay, context, detail)
    }

    fn phase_grid(context: impl Into<String>, detail: impl Into<String>) -> Self {
        TimebaseError::new(TimebaseErrorKind::PhaseGrid, context, detail)
    }

    fn reference(context: impl Into<String>, detail: impl Into<String>) -> Self {
        TimebaseError::new(TimebaseErrorKind::Reference, context, detail)
    }

    fn range(context: impl Into<String>, detail: impl Into<String>) -> Self {
        TimebaseError::new(TimebaseErrorKind::Range, context, detail)
    }

    fn missing(context: impl Into<String>, detail: impl Into<String>) -> Self {
        TimebaseError::new(TimebaseErrorKind::Missing, context, detail)
    }

    fn unsupported(context: impl Into<String>, detail: impl Into<String>) -> Self {
        TimebaseError::new(TimebaseErrorKind::Unsupported, context, detail)
    }

    /// 错误种类。
    pub fn kind(&self) -> TimebaseErrorKind {
        self.kind
    }

    /// 稳定机器码。
    pub fn code(&self) -> &'static str {
        self.kind.code()
    }

    /// 逻辑上下文（字段路径/position/slot）。
    pub fn context(&self) -> &str {
        &self.context
    }

    /// 人读细节。
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl std::fmt::Display for TimebaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}: {}", self.code(), self.context, self.detail)
    }
}

impl std::error::Error for TimebaseError {}

// ===========================================================================
// 非致命诊断（不阻塞交付，但下游/报告必须能看到）
// ===========================================================================

/// 时基重建的非致命诊断项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimebaseWarning {
    /// 稳定 code（见下方常量）。
    pub code: &'static str,
    /// 人读说明。
    pub message: String,
}

impl TimebaseWarning {
    /// 无独立 ppm 估计，只能信任 diag 声明（单 slot 且无重复间隔时）。
    pub const DECLARED_ONLY_CLOCK: &'static str = "W_TB_DECLARED_ONLY_CLOCK";
    /// slot 间隔与预期（R*(sweep+gap)*fs）偏离超容差。
    pub const SLOT_SPACING: &'static str = "W_TB_SLOT_SPACING";
    /// 相邻 slot 的 repeat 时间窗重叠（物理上不可取；producer/契约问题）。
    pub const SLOT_OVERLAP: &'static str = "W_TB_SLOT_OVERLAP";
    /// repeat 间隔与建模值偏离超容差。
    pub const REPEAT_SPACING: &'static str = "W_TB_REPEAT_SPACING";

    fn new(code: &'static str, message: impl Into<String>) -> Self {
        TimebaseWarning {
            code,
            message: message.into(),
        }
    }
}

fn push_warning(warnings: &mut Vec<TimebaseWarning>, code: &'static str, message: String) {
    warnings.push(TimebaseWarning::new(code, message));
}

// ===========================================================================
// 相位基准语义（phase_ref 解析）
// ===========================================================================

/// analysis 产物 FR 相位的基准语义（由 producer 的 `phase_ref` 字符串解析；冻结 v1/v2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseBasis {
    /// `aligned_segment(...)`：测量相位含传播延迟；绝对相位 = phase − 360·f·**D0**/fs
    /// （只扣公共整数 D0 一次，**不扣** analysis.delay_samples）。
    AlignedSegment,
    /// `absolute...`：FR 相位已经是总相位；**绝不能再扣 D0/delay**（双去相位）。
    Absolute,
    /// `delay_removed(...)`：相位已去一个声明延迟；v1 ingest 无可机读声明 D → 不支持。
    DelayRemoved,
}

impl PhaseBasis {
    /// 解析 producer `phase_ref` 字符串。冻结规则（v1）：
    /// - 含 `aligned_segment`（大小写不敏感）→ [PhaseBasis::AlignedSegment]（producer 现行串
    ///   `aligned_segment(已去粗延迟); absolute_phase_deg = phase_deg - 360*f*delay_samples/fs`
    ///   命中此条；串里的 `delay_samples` 只是 producer 标注，**本模块去相位只扣 D0**）；
    /// - 含 `delay_removed` → [PhaseBasis::DelayRemoved]；
    /// - 去掉首尾空白后以 `absolute` 开头（或等于 `absolute`）→ [PhaseBasis::Absolute]；
    /// - 其它/空 → `E_TB_PHASE_REF`。
    pub fn parse(phase_ref: &str) -> Result<Self, TimebaseError> {
        let trimmed = phase_ref.trim();
        if trimmed.is_empty() {
            return Err(TimebaseError::phase_ref(
                "analysis.phase_ref",
                "phase_ref is empty; absolute phase reconstruction is undefined",
            ));
        }
        let lowered = trimmed.to_ascii_lowercase();
        if lowered.contains("aligned_segment") {
            return Ok(PhaseBasis::AlignedSegment);
        }
        if lowered.contains("delay_removed") {
            return Ok(PhaseBasis::DelayRemoved);
        }
        if lowered == "absolute" || lowered.starts_with("absolute") {
            return Ok(PhaseBasis::Absolute);
        }
        Err(TimebaseError::phase_ref(
            "analysis.phase_ref",
            format!(
                "unsupported phase_ref semantics: {trimmed:?} (known: aligned_segment|absolute|delay_removed)"
            ),
        ))
    }

    /// 机器名（报告/摘要）。
    pub const fn as_str(self) -> &'static str {
        match self {
            PhaseBasis::AlignedSegment => "aligned_segment",
            PhaseBasis::Absolute => "absolute",
            PhaseBasis::DelayRemoved => "delay_removed",
        }
    }
}

impl std::fmt::Display for PhaseBasis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ===========================================================================
// 公共整数 D0（T6 FC-04）
// ===========================================================================

/// T6 FC-04 预声明的**公共整数 D0**（samples@fs）。
///
/// FC-04 要求 baseline/on/两轮使用同一个 D0，并且**只扣 D0**、不按测点/通道事后对齐。
/// 本类型是 [de_phase_analysis]/[de_phase_reference] 的**唯一**去相位输入：`AlignedSegment`
/// 相位只扣本值一次；T48 的逐 participant `analysis.delay_samples` 只作为 provenance /
/// residual 记录（见 `SlotTimebase::per_participant_residual_delay_samples`），绝不参与
/// 相位扣除。D0 的预声明与跨轮冻结由 host/冻结清单负责（本模块只消费 + 回显）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommonD0 {
    samples: i64,
}

impl CommonD0 {
    /// 从声明的整数样本数构造（samples@fs）。
    ///
    /// `|samples| > COMMON_D0_SANITY_SAMPLES` → `E_TB_COMMON_D`（sanity 上界，不是 T6 门）。
    pub fn new(samples: i64) -> Result<Self, TimebaseError> {
        if samples > COMMON_D0_SANITY_SAMPLES || samples < -COMMON_D0_SANITY_SAMPLES {
            return Err(TimebaseError::common_d(
                "timebase.common_d0_samples",
                format!(
                    "declared common D0 {samples} samples outside sanity ±{COMMON_D0_SANITY_SAMPLES} samples (samples@fs)"
                ),
            ));
        }
        Ok(CommonD0 { samples })
    }

    /// 无预延迟声明（D0 = 0 samples）。
    pub const fn zero() -> Self {
        CommonD0 { samples: 0 }
    }

    /// 声明值（整数样本）。
    pub const fn samples(self) -> i64 {
        self.samples
    }

    /// 声明值（f64；去相位旋转用）。
    pub fn as_f64(self) -> f64 {
        self.samples as f64
    }

    /// 逐 participant 实测延迟相对 D0 的残差（provenance；不参与去相位）。
    pub fn residual_of(self, measured_delay_samples: f64) -> f64 {
        measured_delay_samples - self.as_f64()
    }
}

impl Default for CommonD0 {
    fn default() -> Self {
        CommonD0::zero()
    }
}

impl std::fmt::Display for CommonD0 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "D0={} samples@fs", self.samples)
    }
}

// ===========================================================================
// 极性（单次归一化）
// ===========================================================================

/// 物理极性（analysis.polarity 归一化后的 ±1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polarity {
    /// +1：与参考同相。
    Normal,
    /// −1：反相。
    Invert,
}

impl Polarity {
    /// 解析 analysis.polarity（只接受 ±1；0=unknown → 拒绝）。
    pub fn parse(value: i32) -> Result<Self, TimebaseError> {
        match value {
            1 => Ok(Polarity::Normal),
            -1 => Ok(Polarity::Invert),
            0 => Err(TimebaseError::polarity_unknown(
                "analysis.polarity",
                "polarity=0 means unknown; MIMO design must not guess polarity (FC-08/T6 §3.5)",
            )),
            other => Err(TimebaseError::polarity_unknown(
                "analysis.polarity",
                format!("unsupported polarity {other} (only -1|1 accepted)"),
            )),
        }
    }

    /// 乘性符号（+1.0/−1.0）。
    pub const fn sign(self) -> f64 {
        match self {
            Polarity::Normal => 1.0,
            Polarity::Invert => -1.0,
        }
    }

    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            Polarity::Normal => "normal",
            Polarity::Invert => "invert",
        }
    }
}

/// 极性来源（证据链追溯：participant 的实测极性 vs 电学 reference）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolaritySource {
    /// 来自 participant 的 analysis 字段。
    Analysis,
    /// 来自 MOTU 电学 reference 通道。
    Reference,
}

/// 已解析/记录来源的极性（用于"只应用一次"的防双乘语义）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedPolarity {
    sign: Polarity,
    source: PolaritySource,
}

impl ResolvedPolarity {
    /// 解析并绑定来源；0/非法 → `E_TB_POLARITY_UNKNOWN`。
    pub fn resolve(value: i32, source: PolaritySource) -> Result<Self, TimebaseError> {
        Ok(ResolvedPolarity {
            sign: Polarity::parse(value)?,
            source,
        })
    }

    /// 极性值。
    pub const fn sign(self) -> Polarity {
        self.sign
    }

    /// 来源。
    pub const fn source(self) -> PolaritySource {
        self.source
    }

    /// ±1 符号。
    pub const fn as_i8(self) -> i8 {
        match self.sign {
            Polarity::Normal => 1,
            Polarity::Invert => -1,
        }
    }
}

impl std::fmt::Display for ResolvedPolarity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let source = match self.source {
            PolaritySource::Analysis => "analysis",
            PolaritySource::Reference => "reference",
        };
        write!(f, "{} ({source})", self.as_i8())
    }
}

// ===========================================================================
// 复响应与单次去相位
// ===========================================================================

/// 一个频率点的复数响应（`re/im` 为线性幅度域，不是 dB）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ComplexPoint {
    /// 频率（Hz）。
    pub freq_hz: f64,
    /// 实部。
    pub re: f64,
    /// 虚部。
    pub im: f64,
}

impl ComplexPoint {
    /// 线性幅度。
    pub fn magnitude(&self) -> f64 {
        self.re.hypot(self.im)
    }

    /// 主值相位（deg，(−180,180]）。
    pub fn phase_deg(&self) -> f64 {
        self.im.atan2(self.re).to_degrees()
    }

    /// 由 dB 幅度 + deg 相位构造。
    pub fn from_polar(freq_hz: f64, db: f64, phase_deg: f64) -> Result<Self, TimebaseError> {
        let freq_hz = finite(freq_hz, "fr.freq_hz")?;
        let db = finite(db, "fr.db")?;
        let phase_deg = finite(phase_deg, "fr.phase_deg")?;
        if !(freq_hz > 0.0) {
            return Err(TimebaseError::range("fr.freq_hz", "frequency must be > 0"));
        }
        let magnitude = 10.0f64.powf(db / 20.0);
        let phase_rad = phase_deg.to_radians();
        Ok(ComplexPoint {
            freq_hz,
            re: magnitude * phase_rad.cos(),
            im: magnitude * phase_rad.sin(),
        })
    }
}

/// **已在本模块去相位一次**的绝对复响应。
///
/// 构造入口只有 [de_phase_analysis]（以及内部 reference 路径）：本类型不提供任何
/// 再次扣延迟的 API，因此"双去相位"在类型层面不可达；`delay_removed_samples`
/// 记录到底扣了多少（`AlignedSegment` 恒为声明的公共整数 D0，`Absolute` 时恒为 0）。
#[derive(Debug, Clone, PartialEq)]
pub struct AbsoluteResponse {
    label: String,
    points: Vec<ComplexPoint>,
    basis: PhaseBasis,
    common_d0: CommonD0,
    delay_removed_samples: f64,
    polarity: Option<ResolvedPolarity>,
}

impl AbsoluteResponse {
    /// 标签（如 P01/FL 或 P01/reference）。
    pub fn label(&self) -> &str {
        &self.label
    }

    /// 频率点（升序；与 producer FR 网格一致）。
    pub fn points(&self) -> &[ComplexPoint] {
        &self.points
    }

    /// 相位基准语义。
    pub fn basis(&self) -> PhaseBasis {
        self.basis
    }

    /// 本模块已从相位中扣除的总延迟（样本；absolute 基准为 0）。
    pub fn delay_removed_samples(&self) -> f64 {
        self.delay_removed_samples
    }

    /// 本次响应使用的 T6 FC-04 公共整数 D0（`Absolute` 基准下仍记录声明值，只是未扣）。
    pub fn common_d0(&self) -> CommonD0 {
        self.common_d0
    }

    /// 已归一化的极性（`None` = 仍是实测物理极性，未乘过任何极性符号）。
    pub fn polarity(&self) -> Option<ResolvedPolarity> {
        self.polarity
    }

    /// 频率点数。
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// 是否为空（合法响应至少 2 点）。
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// 把已声明的物理极性**乘一次**进响应（invert 时整体乘 −1），并记录来源。
    ///
    /// 只能成功一次：第二次调用（不论极性是否相同）→ `E_TB_DOUBLE_POLARITY`，
    /// 用来防"分析里已经反相过、应用 P_post 时又反一次"的双反相。
    pub fn normalize_polarity(&mut self, polarity: ResolvedPolarity) -> Result<(), TimebaseError> {
        if let Some(existing) = self.polarity {
            return Err(TimebaseError::double_polarity(
                format!("response.{}", self.label),
                format!(
                    "polarity already normalized once ({existing}), refusing to apply {polarity} again"
                ),
            ));
        }
        if polarity.sign() == Polarity::Invert {
            for point in &mut self.points {
                point.re = -point.re;
                point.im = -point.im;
            }
        }
        self.polarity = Some(polarity);
        Ok(())
    }

    /// 除以一条**绝对**参考响应，得到相对响应（每个频点除一次）。
    ///
    /// 结果类型 [RelativeResponse] 没有 `relative_to`，因此"再除一次参考/再扣一次相位"
    /// 不可链式发生。频率网格必须一致（`E_TB_PHASE_GRID`）。
    pub fn relative_to(
        &self,
        reference: &AbsoluteResponse,
    ) -> Result<RelativeResponse, TimebaseError> {
        if self.points.len() != reference.points.len() {
            return Err(TimebaseError::phase_grid(
                format!("response.{}.relative", self.label),
                format!(
                    "frequency grid length mismatch: {} vs reference {} ({})",
                    self.points.len(),
                    reference.points.len(),
                    reference.label
                ),
            ));
        }
        let mut points = Vec::with_capacity(self.points.len());
        for (value, ref_value) in self.points.iter().zip(reference.points.iter()) {
            let tolerance = 1.0e-9 * value.freq_hz.abs().max(1.0);
            if (value.freq_hz - ref_value.freq_hz).abs() > tolerance {
                return Err(TimebaseError::phase_grid(
                    format!("response.{}.relative", self.label),
                    format!(
                        "frequency grid mismatch at {:?} Hz vs {:?} Hz ({})",
                        value.freq_hz, ref_value.freq_hz, reference.label
                    ),
                ));
            }
            let denom = ref_value.re * ref_value.re + ref_value.im * ref_value.im;
            if !(denom > COMPLEX_DIVISION_FLOOR) {
                return Err(TimebaseError::range(
                    format!("response.{}.relative", self.label),
                    format!(
                        "reference magnitude at {} Hz is too small for division ({})",
                        ref_value.freq_hz, reference.label
                    ),
                ));
            }
            points.push(ComplexPoint {
                freq_hz: value.freq_hz,
                re: (value.re * ref_value.re + value.im * ref_value.im) / denom,
                im: (value.im * ref_value.re - value.re * ref_value.im) / denom,
            });
        }
        Ok(RelativeResponse {
            label: self.label.clone(),
            reference_label: reference.label.clone(),
            points,
        })
    }
}

/// 相对参考的复响应（只读；不能再做相对化，避免重复除参考/重复去相位）。
#[derive(Debug, Clone, PartialEq)]
pub struct RelativeResponse {
    label: String,
    reference_label: String,
    points: Vec<ComplexPoint>,
}

impl RelativeResponse {
    /// participant 标签。
    pub fn label(&self) -> &str {
        &self.label
    }

    /// 参考标签。
    pub fn reference_label(&self) -> &str {
        &self.reference_label
    }

    /// 频率点。
    pub fn points(&self) -> &[ComplexPoint] {
        &self.points
    }
}

/// **单次**去相位：把 analysis 的 FR 相位按 `phase_ref` 语义转成绝对复响应。
///
/// - `AlignedSegment`：`φ_abs = φ_fr − 2π·f·**D0**/fs`——**只扣公共整数 D0 一次**；
///   **不扣** T48 `analysis.delay_samples`（实测传播延迟，见 [CommonD0] 与模块文档）；
/// - `Absolute`：不扣任何值（`delay_removed_samples=0`；仍记录声明的 D0 provenance）；
/// - `DelayRemoved`：v1 ingest 无可机读声明 D → `E_TB_PHASE_REF`。
///
/// 同时复校 T48 analysis 的 delay 三字段自洽（`delay_coarse_samples == floor(delay_samples)`、
/// `delay_ms == delay_samples/fs*1000`，见 [validate_analysis_delay_fields]）。
///
/// 本函数**不做**极性归一化（[AbsoluteResponse::normalize_polarity] 单独做一次），
/// 也**不做**相对参考除法（[AbsoluteResponse::relative_to] 只做一次）。
pub fn de_phase_analysis(
    analysis: &SourceAnalysis,
    common_d0: CommonD0,
    label: impl Into<String>,
) -> Result<AbsoluteResponse, TimebaseError> {
    let label = label.into();
    let basis = PhaseBasis::parse(&analysis.phase_ref)?;
    if analysis.fr.len() < 2 {
        return Err(TimebaseError::missing(
            format!("analysis.{label}.fr"),
            "at least 2 finite FR points are required for phase reconstruction",
        ));
    }
    // T48 实测延迟只做 provenance + 字段自洽复校；绝不进入相位旋转。
    let _measured_delay_samples =
        validate_analysis_delay_fields(analysis, &format!("analysis.{label}"))?;
    let mut points = Vec::with_capacity(analysis.fr.len());
    let mut previous_freq = f64::NEG_INFINITY;
    for point in &analysis.fr {
        let complex = ComplexPoint::from_polar(point.freq_hz, point.db, point.phase_deg)?;
        if complex.freq_hz <= previous_freq {
            return Err(TimebaseError::range(
                format!("analysis.{label}.fr"),
                "FR frequencies must be strictly increasing",
            ));
        }
        previous_freq = complex.freq_hz;
        points.push(complex);
    }
    let sample_rate_hz = analysis.meta.sample_rate_hz;
    if !SAMPLE_RATES_HZ.contains(&sample_rate_hz) {
        return Err(TimebaseError::unsupported(
            format!("analysis.{label}.meta.sample_rate"),
            format!("sample rate {sample_rate_hz} Hz is not a P1 rate {SAMPLE_RATES_HZ:?}"),
        ));
    }
    let sample_rate = sample_rate_hz as f64;
    let d0_samples = common_d0.as_f64();
    let (points, delay_removed_samples) = match basis {
        PhaseBasis::AlignedSegment => {
            // 单次去相位：complex *= exp(-j·2π·f·D0/fs)；只扣公共整数 D0，不扣 measured delay。
            for point in &mut points {
                let rotation =
                    -2.0 * std::f64::consts::PI * point.freq_hz * d0_samples / sample_rate;
                let (sin, cos) = rotation.sin_cos();
                let re = point.re * cos - point.im * sin;
                let im = point.re * sin + point.im * cos;
                point.re = re;
                point.im = im;
            }
            (points, d0_samples)
        }
        PhaseBasis::Absolute => {
            // 已经是总相位：绝不再次扣 D0/delay（否则就是双去相位）。
            (points, 0.0)
        }
        PhaseBasis::DelayRemoved => {
            return Err(TimebaseError::phase_ref(
                format!("analysis.{label}.phase_ref"),
                "phase_ref=delay_removed is unsupported by v1 ingest (no machine-readable removed D); declare aligned_segment + CommonD0 instead; refusing to guess (E_TB_PHASE_REF)",
            ));
        }
    };
    Ok(AbsoluteResponse {
        label,
        points,
        basis,
        common_d0,
        delay_removed_samples,
        polarity: None,
    })
}

// ===========================================================================
// MOTU 可选电学 reference（独立通道）
// ===========================================================================

/// 已解析的 MOTU 电学 reference 通道（另一独立设备/文件，不是 UMIK）。
#[derive(Debug, Clone, PartialEq)]
pub struct ReferenceChannel {
    /// 设备串（必须 ≠ UMIK 采集设备）。
    pub device: String,
    /// 通道数。
    pub channels: u16,
    /// 样本格式。
    pub sample_format: SampleFormat,
    /// 必须是独立通道（本栈只接受 true）。
    pub independent: bool,
    /// 去相位使用的 T6 FC-04 公共整数 D0（与 participant 同一值）。
    pub common_d0: CommonD0,
    /// reference 实测延迟（样本；provenance，**不参与**相位扣除；由 T62/复测层分析产物给出）。
    pub delay_samples: f64,
    /// reference 实测延迟相对公共 D0 的残差（样本；provenance，不参与相位扣除）。
    pub residual_delay_samples: f64,
    /// reference 极性（通常 normal；从 analysis 字段解析并记录）。
    pub polarity: ResolvedPolarity,
    /// reference 的绝对复响应（已按 D0 去相位一次，未乘极性）。
    pub response: AbsoluteResponse,
}

/// 解析/校验 MOTU 电学 reference 通道并构造其绝对响应（单次去相位）。
///
/// 规则（DSP-HOST-DECISION §2.3 / WBS T49 DoD）：
/// - `session.reference` 必须存在，`independent=true`，`device` ≠ `session.capture.device`；
/// - `position.reference`（reference.raw）必须存在，且通道数/格式/采样率与声明一致；
/// - `reference_analysis.polarity` 必须 ±1；`phase_ref` 按 v1 语义解析并**只扣公共整数
///   D0 一次**（与 participant 同一 D0；reference 实测延迟只作 provenance/residual）；
/// - 本函数不验证 reference 是否被同一采集时钟污染（UMIK 物理上不可能采集 reference，
///   因为 reference 是 MOTU 电学另通道；独立性由 device/文件/independent 字段保证）。
pub fn de_phase_reference(
    session: &IngestSession,
    position: &IngestPosition,
    common_d0: CommonD0,
    reference_analysis: &SourceAnalysis,
) -> Result<ReferenceChannel, TimebaseError> {
    let declared = session.reference.as_ref().ok_or_else(|| {
        TimebaseError::reference(
            "session.reference",
            "reference analysis supplied but session.json does not declare a MOTU electrical reference",
        )
    })?;
    if !declared.independent {
        return Err(TimebaseError::reference(
            "session.reference.independent",
            "reference channel must be an independent device/path (independent=true)",
        ));
    }
    if declared.device == session.capture.device {
        return Err(TimebaseError::reference(
            "session.reference.device",
            format!(
                "reference device {:?} must differ from UMIK capture device {:?}",
                declared.device, session.capture.device
            ),
        ));
    }
    if declared.channels == 0 || declared.channels as usize > MAX_INPUT_CHANNELS {
        return Err(TimebaseError::reference(
            "session.reference.channels",
            format!(
                "reference channels {} outside 1..={MAX_INPUT_CHANNELS}",
                declared.channels
            ),
        ));
    }
    let raw: &CaptureFile = position.reference.as_ref().ok_or_else(|| {
        TimebaseError::reference(
            format!("position.{}.reference", position.meta.name),
            "session declares a reference channel but reference.raw is absent",
        )
    })?;
    if raw.channels != declared.channels
        || raw.sample_format != declared.sample_format
        || raw.sample_rate_hz != session.sample_rate_hz
    {
        return Err(TimebaseError::reference(
            format!("position.{}.reference.raw", position.meta.name),
            format!(
                "reference.raw declaration mismatch: channels {}/{} format {}/{} fs {}/{}",
                raw.channels,
                declared.channels,
                raw.sample_format.as_str(),
                declared.sample_format.as_str(),
                raw.sample_rate_hz,
                session.sample_rate_hz
            ),
        ));
    }
    let polarity =
        ResolvedPolarity::resolve(reference_analysis.polarity, PolaritySource::Reference)?;
    let delay_samples = validate_analysis_delay_fields(reference_analysis, "reference.analysis")?;
    let response = de_phase_analysis(
        reference_analysis,
        common_d0,
        format!("{}/reference", position.meta.name),
    )?;
    Ok(ReferenceChannel {
        device: declared.device.clone(),
        channels: declared.channels,
        sample_format: declared.sample_format,
        independent: declared.independent,
        common_d0,
        delay_samples,
        residual_delay_samples: common_d0.residual_of(delay_samples),
        polarity,
        response,
    })
}

// ===========================================================================
// 槽相位（公共相位重建）
// ===========================================================================

/// 一个 participant slot 的公共相位结果。
#[derive(Debug, Clone, PartialEq)]
pub struct SlotPhase {
    /// slot 序号。
    pub slot: u32,
    /// participant 角色名。
    pub speaker: String,
    /// 播放设备通道号。
    pub out_index: u32,
    /// 实测极性（已解析；未乘进响应）。
    pub polarity: ResolvedPolarity,
    /// 绝对复响应（已按公共整数 D0 去相位一次；未乘极性）。
    pub absolute: AbsoluteResponse,
}

/// 一个 position 的公共相位（全部 participant + 可选电学 reference + 相对响应）。
#[derive(Debug, Clone, PartialEq)]
pub struct CommonPhase {
    /// position label。
    pub position: String,
    /// 公共相位基准语义（全部 slot 必须一致）。
    pub basis: PhaseBasis,
    /// 本 position 全部响应只扣一次的 T6 FC-04 公共整数 D0。
    pub common_d0: CommonD0,
    /// 按 slot 升序。
    pub slots: Vec<SlotPhase>,
    /// 电学 reference（session 声明时存在）。
    pub reference: Option<ReferenceChannel>,
    /// 每个 slot 相对 reference 的响应（无 reference 时为空）。
    pub relative: Vec<RelativeResponse>,
    /// 非致命诊断。
    pub warnings: Vec<TimebaseWarning>,
}

impl CommonPhase {
    /// 按 participant 名取 slot 相位。
    pub fn slot(&self, speaker: &str) -> Option<&SlotPhase> {
        self.slots.iter().find(|slot| slot.speaker == speaker)
    }
}

/// 公共相位重建：每个 participant **只按公共整数 D0 去相位一次**、解析极性（不乘）、
/// 可选除以 MOTU 电学 reference **一次**（reference 同样只扣同一 D0）。
///
/// D0 取自 `timebase.common_d0`（[reconstruct_position] 已记录并复校的 T6 FC-04 声明），
/// 逐 participant 的 measured delay **不**参与任何相位扣除，只在 SlotTimebase 里作
/// provenance/residual。
///
/// 前提：`timebase` 必须由 [reconstruct_position] 对同一 `position` 生成（用于 slot 顺序、
/// 已解析的相位基准/极性与时间轴一致性）。`reference_analysis` 在 `session.reference`
/// 声明时**必须**提供（T62/复测层产物），否则 `E_TB_REFERENCE`；未声明时提供则同样拒绝。
pub fn reconstruct_common_phase(
    timebase: &PositionTimebase,
    position: &IngestPosition,
    session: &IngestSession,
    reference_analysis: Option<&SourceAnalysis>,
) -> Result<CommonPhase, TimebaseError> {
    let common_d0 = timebase.common_d0;
    let mut measurements: Vec<&IngestMeasurement> = position.measurements.iter().collect();
    measurements.sort_by_key(|measurement| measurement.slot);
    if measurements.len() != timebase.slots.len() {
        return Err(TimebaseError::missing(
            format!("position.{}.slots", position.meta.name),
            format!(
                "timebase has {} slots but position has {} measurements",
                timebase.slots.len(),
                measurements.len()
            ),
        ));
    }
    let mut warnings = Vec::new();
    let mut slots = Vec::with_capacity(measurements.len());
    let mut basis: Option<PhaseBasis> = None;
    for (index, measurement) in measurements.iter().enumerate() {
        let timing = &timebase.slots[index];
        if timing.slot != measurement.slot || timing.speaker != measurement.speaker {
            return Err(TimebaseError::slot_order(
                format!("position.{}.slot.{}", position.meta.name, measurement.slot),
                "timebase slot order does not match position measurements",
            ));
        }
        let parsed = PhaseBasis::parse(&measurement.analysis.phase_ref)?;
        if parsed == PhaseBasis::DelayRemoved {
            return Err(TimebaseError::phase_ref(
                format!("position.{}.slot.{}", position.meta.name, measurement.slot),
                "phase_ref=delay_removed is unsupported by v1 ingest (no declared D); refusing to guess",
            ));
        }
        match basis {
            None => basis = Some(parsed),
            Some(expected) if expected != parsed => {
                return Err(TimebaseError::phase_ref(
                    format!("position.{}.slot.{}", position.meta.name, measurement.slot),
                    format!(
                        "phase basis changed within one position: {expected} vs {parsed}; common phase is undefined"
                    ),
                ));
            }
            Some(_) => {}
        }
        let absolute = de_phase_analysis(
            &measurement.analysis,
            common_d0,
            format!("{}/{}", position.meta.name, measurement.speaker),
        )?;
        slots.push(SlotPhase {
            slot: measurement.slot,
            speaker: measurement.speaker.clone(),
            out_index: measurement.out_index,
            polarity: timing.polarity,
            absolute,
        });
    }
    let reference = match (session.reference.as_ref(), reference_analysis) {
        (Some(_), Some(analysis)) => {
            Some(de_phase_reference(session, position, common_d0, analysis)?)
        }
        (Some(_), None) => {
            return Err(TimebaseError::reference(
                format!("position.{}.reference", position.meta.name),
                "session declares a MOTU electrical reference but no reference analysis was supplied",
            ));
        }
        (None, Some(_)) => {
            return Err(TimebaseError::reference(
                format!("position.{}.reference", position.meta.name),
                "reference analysis supplied but session does not declare a reference channel",
            ));
        }
        (None, None) => None,
    };
    let mut relative = Vec::new();
    if let Some(reference) = &reference {
        for slot in &slots {
            relative.push(slot.absolute.relative_to(&reference.response)?);
        }
    } else {
        push_warning(
            &mut warnings,
            "W_TB_ABSOLUTE_PHASE_ONLY",
            "no MOTU electrical reference: absolute phase is referenced to the sweep start (render frame) only".to_string(),
        );
    }
    let basis = basis.ok_or_else(|| {
        TimebaseError::missing(
            format!("position.{}.phase", position.meta.name),
            "position has no measurements; cannot reconstruct common phase",
        )
    })?;
    Ok(CommonPhase {
        position: position.meta.name.clone(),
        basis,
        common_d0,
        slots,
        reference,
        relative,
        warnings,
    })
}

// ===========================================================================
// 时钟重建（coarse/slot/clock_ppm 的 ppm 部分）
// ===========================================================================

/// ppm 估计方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockMethod {
    /// (render_frame_index, nominal_start) 最小二乘。
    RenderGridLsq,
    /// (render_frame_index, actual_start − analysis.delay_samples) 最小二乘。
    OnsetDelayLsq,
    /// 同 slot 相邻 repeat 起点间隔 / ((sweep+gap)*fs)。
    RepeatInterval,
    /// 多方法加权合并。
    Combined,
    /// 无独立估计，只能信任 diag 声明。
    DeclaredOnly,
}

impl ClockMethod {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            ClockMethod::RenderGridLsq => "render_grid_lsq",
            ClockMethod::OnsetDelayLsq => "onset_delay_lsq",
            ClockMethod::RepeatInterval => "repeat_interval",
            ClockMethod::Combined => "combined",
            ClockMethod::DeclaredOnly => "declared_only",
        }
    }
}

impl std::fmt::Display for ClockMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 单个独立 ppm 估计。
#[derive(Debug, Clone, PartialEq)]
pub struct ClockEstimate {
    /// 方法。
    pub method: ClockMethod,
    /// ppm（正 = capture 采样率快于 render 时钟）。
    pub ppm: f64,
    /// 1σ 不确定度（ppm）。
    pub uncertainty_ppm: f64,
    /// 参与拟合的点数/间隔数。
    pub points: usize,
    /// 拟合残差 RMS（capture 样本；RepeatInterval 为间隔残差）。
    pub residual_rms_samples: f64,
}

/// 一个 position 的 render↔capture 时钟映射（重建结果）。
#[derive(Debug, Clone, PartialEq)]
pub struct ClockMap {
    /// 采样率（Hz；P1 48k/96k）。
    pub sample_rate_hz: u32,
    /// diag 声明的 ppm 中位数（producer 实测，可能已被校正）。
    pub declared_ppm: f64,
    /// diag 声明的 σ 最大值。
    pub declared_uncertainty_ppm: f64,
    /// diag 声明的显著漂移（任一 slot；FC-06 2σ 口径）。
    pub declared_drift_significant: bool,
    /// diag 声明的校正方式（全部 slot 一致）。
    pub correction: DriftCorrection,
    /// 记录坐标里**仍残留**的 ppm（consumers 用它做 render↔capture 映射）。
    pub residual_ppm: f64,
    /// 残差 ppm 的 1σ。
    pub residual_uncertainty_ppm: f64,
    /// 残差漂移是否显著（|residual|>2σ，σ 取 max(独立, 声明)）。
    pub residual_drift_significant: bool,
    /// 全部独立估计（按方法固定顺序）。
    pub estimates: Vec<ClockEstimate>,
    /// 采用的方法。
    pub method: ClockMethod,
    /// capture 样本 / render 帧：`(fs_capture/fs_render)*(1+residual_ppm/1e6)`；本模块强制
    /// `fs_capture == fs_render`（见 [validate_capture_file]）故 ratio=1，实际值 =
    /// `1 + residual_ppm/1e6`（见 [ClockMap::capture_of_render]）。
    pub samples_per_frame: f64,
    /// 映射原点（render 帧）。
    pub render_origin: f64,
    /// 映射原点（capture 样本）。
    pub capture_origin: f64,
    /// 参与重建的 slot 数。
    pub points: usize,
}

impl ClockMap {
    /// render 帧 → capture 样本（仿射；残差 ppm 已含在 `samples_per_frame`）。
    pub fn capture_of_render(&self, render_frame: f64) -> f64 {
        self.capture_origin + (render_frame - self.render_origin) * self.samples_per_frame
    }

    /// capture 样本 → render 帧（逆仿射）。
    pub fn render_of_capture(&self, capture_sample: f64) -> f64 {
        self.render_origin + (capture_sample - self.capture_origin) / self.samples_per_frame
    }

    /// 是否存在独立（非 DeclaredOnly）估计。
    pub fn independent(&self) -> bool {
        self.method != ClockMethod::DeclaredOnly
    }
}

/// 最小二乘结果（内部）。
struct LeastSquares {
    slope: f64,
    residual_rms: f64,
    slope_sigma: f64,
    points: usize,
}

fn least_squares(xs: &[f64], ys: &[f64]) -> Option<LeastSquares> {
    if xs.len() != ys.len() || xs.len() < 2 {
        return None;
    }
    let count = xs.len() as f64;
    let mean_x = xs.iter().sum::<f64>() / count;
    let mean_y = ys.iter().sum::<f64>() / count;
    let mut den = 0.0;
    let mut num = 0.0;
    for (x, y) in xs.iter().zip(ys.iter()) {
        let dx = x - mean_x;
        den += dx * dx;
        num += dx * (y - mean_y);
    }
    if !(den > 0.0) {
        return None;
    }
    let slope = num / den;
    let mut sse = 0.0;
    for (x, y) in xs.iter().zip(ys.iter()) {
        let residual = y - (mean_y + slope * (x - mean_x));
        sse += residual * residual;
    }
    let residual_rms = (sse / count).sqrt();
    let dof = (count - 2.0).max(1.0);
    let slope_sigma = (sse / dof).sqrt() / den.sqrt();
    Some(LeastSquares {
        slope,
        residual_rms,
        slope_sigma,
        points: xs.len(),
    })
}

fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        Some(0.5 * (sorted[mid - 1] + sorted[mid]))
    } else {
        Some(sorted[mid])
    }
}

fn median_abs_deviation(values: &[f64], center: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let deviations: Vec<f64> = values.iter().map(|value| (value - center).abs()).collect();
    median(&deviations).unwrap_or(0.0)
}

fn finite(value: f64, context: &str) -> Result<f64, TimebaseError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(TimebaseError::range(
            context,
            "value must be finite (NaN/Inf are never valid timebase data)",
        ))
    }
}

fn finite_in_range(value: f64, min: f64, max: f64, context: &str) -> Result<f64, TimebaseError> {
    let value = finite(value, context)?;
    if value < min || value > max {
        return Err(TimebaseError::range(
            context,
            format!("value {value} outside [{min}, {max}]"),
        ));
    }
    Ok(value)
}

/// 复校 T48 analysis 的延迟字段自洽性（T49 重新校验 ingest 不变量；P3-2）。
///
/// - `delay_samples` 有限；
/// - `delay_coarse_samples == floor(delay_samples)`（producer 口径，整数样本）；
/// - `delay_ms == delay_samples / fs * 1000`（容差 [DELAY_MS_ABSOLUTE_TOLERANCE_MS] +
///   [DELAY_MS_RELATIVE_TOLERANCE]·|expected|，兼容 6 位小数 ms 记录）。
///
/// 返回 `delay_samples`（provenance；**不**用于去相位——去相位只扣 [CommonD0]）。
fn validate_analysis_delay_fields(
    analysis: &SourceAnalysis,
    context: &str,
) -> Result<f64, TimebaseError> {
    let delay_samples = finite(analysis.delay_samples, &format!("{context}.delay_samples"))?;
    let delay_ms = finite(analysis.delay_ms, &format!("{context}.delay_ms"))?;
    let sample_rate_hz = analysis.meta.sample_rate_hz;
    if sample_rate_hz == 0 {
        return Err(TimebaseError::unsupported(
            format!("{context}.meta.sample_rate"),
            "sample rate 0 makes delay_ms self-consistency undefined",
        ));
    }
    let expected_ms = delay_samples / sample_rate_hz as f64 * 1000.0;
    let tolerance_ms =
        DELAY_MS_ABSOLUTE_TOLERANCE_MS + DELAY_MS_RELATIVE_TOLERANCE * expected_ms.abs();
    if !expected_ms.is_finite() || (delay_ms - expected_ms).abs() > tolerance_ms {
        return Err(TimebaseError::range(
            format!("{context}.delay_ms"),
            format!(
                "delay_ms {delay_ms} inconsistent with delay_samples {delay_samples} @ {sample_rate_hz} Hz (expected {expected_ms} ms, tolerance ±{tolerance_ms} ms)"
            ),
        ));
    }
    let coarse = delay_samples.floor();
    if !coarse.is_finite() || coarse.abs() > 9.0e15 {
        return Err(TimebaseError::range(
            format!("{context}.delay_samples"),
            "delay_samples magnitude exceeds exact integer range",
        ));
    }
    if analysis.delay_coarse_samples != coarse as i64 {
        return Err(TimebaseError::range(
            format!("{context}.delay_coarse_samples"),
            format!(
                "delay_coarse_samples {} != floor(delay_samples) = {}",
                analysis.delay_coarse_samples, coarse as i64
            ),
        ));
    }
    Ok(delay_samples)
}

/// diag 声明天线（per-position 聚合）。
struct DeclaredClock {
    ppm: f64,
    uncertainty_ppm: f64,
    drift_significant: bool,
    correction: DriftCorrection,
}

fn aggregate_declared_clock(
    measurements: &[&IngestMeasurement],
    _sample_rate_hz: u32,
) -> Result<DeclaredClock, TimebaseError> {
    if measurements.is_empty() {
        return Err(TimebaseError::missing(
            "position.measurements",
            "no measurements to reconstruct clock from",
        ));
    }
    let mut ppms = Vec::with_capacity(measurements.len());
    let mut sigmas = Vec::with_capacity(measurements.len());
    let mut any_significant = false;
    let mut correction: Option<DriftCorrection> = None;
    for measurement in measurements {
        let clock = measurement.diag.clock;
        let context = format!("diag.slot.{}.clock", measurement.slot);
        let ppm = finite_in_range(
            clock.clock_ppm,
            -CLOCK_PPM_SANITY_LIMIT,
            CLOCK_PPM_SANITY_LIMIT,
            &format!("{context}.clock_ppm"),
        )?;
        let sigma = finite_in_range(
            clock.clock_ppm_uncertainty_ppm,
            0.0,
            CLOCK_UNCERTAINTY_SANITY_LIMIT,
            &format!("{context}.clock_ppm_uncertainty_ppm"),
        )?;
        if !clock.clock_ppm_ok {
            return Err(TimebaseError::clock_conflict(
                format!("{context}.clock_ppm_ok"),
                "clock_ppm_ok=false is not deliverable (ingest must reject it)",
            ));
        }
        let computed_significant = ppm.abs() > 2.0 * sigma;
        if computed_significant != clock.drift_significant {
            return Err(TimebaseError::clock_conflict(
                format!("{context}.drift_significant"),
                format!(
                    "drift_significant={} contradicts |clock_ppm|={ppm} vs 2σ={}",
                    clock.drift_significant,
                    2.0 * sigma
                ),
            ));
        }
        if clock.drift_significant && clock.correction == DriftCorrection::None {
            return Err(TimebaseError::clock_drift(
                format!("{context}.correction"),
                "significant drift declared but correction=none",
            ));
        }
        match correction {
            None => correction = Some(clock.correction),
            Some(expected) if expected != clock.correction => {
                return Err(TimebaseError::clock_conflict(
                    format!("{context}.correction"),
                    format!(
                        "drift correction changed within one position: {} vs {}",
                        expected.as_str(),
                        clock.correction.as_str()
                    ),
                ));
            }
            Some(_) => {}
        }
        any_significant |= clock.drift_significant;
        ppms.push(ppm);
        sigmas.push(sigma);
    }
    let ppm = median(&ppms).unwrap_or(0.0);
    let sigma = sigmas.iter().cloned().fold(0.0f64, f64::max);
    let max_spread = ppms
        .iter()
        .map(|value| (value - ppm).abs())
        .fold(0.0f64, f64::max);
    let spread_tolerance = (CLOCK_SIGMA_FACTOR * sigma).max(CLOCK_AGREEMENT_TOLERANCE_PPM);
    if max_spread > spread_tolerance {
        return Err(TimebaseError::clock_conflict(
            "diag.clock",
            format!(
                "declared clock_ppm varies across slots beyond tolerance: spread {max_spread} ppm > {spread_tolerance} ppm"
            ),
        ));
    }
    Ok(DeclaredClock {
        ppm,
        uncertainty_ppm: sigma,
        drift_significant: any_significant,
        correction: correction.unwrap_or(DriftCorrection::None),
    })
}

fn render_grid_estimate(
    measurements: &[&IngestMeasurement],
) -> Result<Option<ClockEstimate>, TimebaseError> {
    if measurements.len() < 2 {
        return Ok(None);
    }
    let mut xs = Vec::with_capacity(measurements.len());
    let mut ys = Vec::with_capacity(measurements.len());
    for measurement in measurements {
        xs.push(measurement.diag.render_frame_index as f64);
        ys.push(finite(
            measurement.diag.nominal_start,
            "diag.nominal_start",
        )?);
    }
    let fit = match least_squares(&xs, &ys) {
        Some(fit) => fit,
        None => return Ok(None),
    };
    Ok(Some(ClockEstimate {
        method: ClockMethod::RenderGridLsq,
        ppm: (fit.slope - 1.0) * 1.0e6,
        uncertainty_ppm: fit.slope_sigma * 1.0e6,
        points: fit.points,
        residual_rms_samples: fit.residual_rms,
    }))
}

fn onset_delay_estimate(
    measurements: &[&IngestMeasurement],
) -> Result<Option<ClockEstimate>, TimebaseError> {
    if measurements.len() < 2 {
        return Ok(None);
    }
    let mut xs = Vec::with_capacity(measurements.len());
    let mut ys = Vec::with_capacity(measurements.len());
    for measurement in measurements {
        xs.push(measurement.diag.render_frame_index as f64);
        let onset = finite(measurement.diag.actual_start, "diag.actual_start")?;
        let delay = finite(measurement.analysis.delay_samples, "analysis.delay_samples")?;
        ys.push(onset - delay);
    }
    let fit = match least_squares(&xs, &ys) {
        Some(fit) => fit,
        None => return Ok(None),
    };
    Ok(Some(ClockEstimate {
        method: ClockMethod::OnsetDelayLsq,
        ppm: (fit.slope - 1.0) * 1.0e6,
        uncertainty_ppm: fit.slope_sigma * 1.0e6,
        points: fit.points,
        residual_rms_samples: fit.residual_rms,
    }))
}

fn repeat_interval_estimate(
    measurements: &[&IngestMeasurement],
    session: &IngestSession,
) -> Result<Option<ClockEstimate>, TimebaseError> {
    let fs = session.sample_rate_hz as f64;
    let interval_frames = (session.stimulus.seconds + session.stimulus.repeat_gap_seconds) * fs;
    if !(interval_frames.is_finite() && interval_frames > 0.0) {
        return Ok(None);
    }
    let mut ratios = Vec::new();
    for measurement in measurements {
        let starts = &measurement.diag.repeat_starts;
        if starts.len() < 2 {
            continue;
        }
        for window in starts.windows(2) {
            let first = finite(window[0], "diag.repeat_starts")?;
            let second = finite(window[1], "diag.repeat_starts")?;
            let observed = second - first;
            if !(observed > 0.0) {
                return Err(TimebaseError::slot_order(
                    format!("diag.slot.{}.repeat_starts", measurement.slot),
                    "repeat starts must be strictly increasing",
                ));
            }
            ratios.push(observed / interval_frames);
        }
    }
    if ratios.is_empty() {
        return Ok(None);
    }
    let ratio = match median(&ratios) {
        Some(ratio) => ratio,
        None => return Ok(None),
    };
    let mad = median_abs_deviation(&ratios, ratio);
    let sigma_ratio = if ratios.len() > 1 {
        1.4826 * mad / (ratios.len() as f64).sqrt()
    } else {
        0.0
    };
    let mut residuals = Vec::new();
    for measurement in measurements {
        for window in measurement.diag.repeat_starts.windows(2) {
            residuals.push((window[1] - window[0]) - ratio * interval_frames);
        }
    }
    let residual_rms = if residuals.is_empty() {
        0.0
    } else {
        (residuals.iter().map(|value| value * value).sum::<f64>() / residuals.len() as f64).sqrt()
    };
    Ok(Some(ClockEstimate {
        method: ClockMethod::RepeatInterval,
        ppm: (ratio - 1.0) * 1.0e6,
        uncertainty_ppm: sigma_ratio * 1.0e6,
        points: ratios.len(),
        residual_rms_samples: residual_rms,
    }))
}

fn adopt_clock_estimates(estimates: &[ClockEstimate]) -> Option<(f64, f64, ClockMethod)> {
    if estimates.is_empty() {
        return None;
    }
    if estimates.len() == 1 {
        let single = &estimates[0];
        return Some((
            single.ppm,
            single.uncertainty_ppm.max(CLOCK_WEIGHT_FLOOR_PPM),
            single.method,
        ));
    }
    let mut weight_sum = 0.0;
    let mut weighted = 0.0;
    for estimate in estimates {
        let sigma = estimate.uncertainty_ppm.max(CLOCK_WEIGHT_FLOOR_PPM);
        let weight = 1.0 / (sigma * sigma);
        weight_sum += weight;
        weighted += weight * estimate.ppm;
    }
    if weight_sum <= 0.0 {
        return None;
    }
    Some((
        weighted / weight_sum,
        (1.0 / weight_sum).sqrt(),
        ClockMethod::Combined,
    ))
}

fn reconstruct_clock(
    measurements: &[&IngestMeasurement],
    session: &IngestSession,
) -> Result<(ClockMap, Vec<TimebaseWarning>), TimebaseError> {
    let sample_rate_hz = session.sample_rate_hz;
    let declared = aggregate_declared_clock(measurements, sample_rate_hz)?;
    let mut warnings = Vec::new();
    let mut estimates = Vec::new();
    if let Some(estimate) = render_grid_estimate(measurements)? {
        estimates.push(estimate);
    }
    if let Some(estimate) = onset_delay_estimate(measurements)? {
        estimates.push(estimate);
    }
    if let Some(estimate) = repeat_interval_estimate(measurements, session)? {
        estimates.push(estimate);
    }
    for (index, first) in estimates.iter().enumerate() {
        for second in estimates.iter().skip(index + 1) {
            let difference = (first.ppm - second.ppm).abs();
            let tolerance = (CLOCK_SIGMA_FACTOR * (first.uncertainty_ppm + second.uncertainty_ppm))
                .max(CLOCK_AGREEMENT_TOLERANCE_PPM);
            if difference > tolerance {
                return Err(TimebaseError::clock_conflict(
                    "clock.estimates",
                    format!(
                        "independent clock estimates disagree: {}={} ppm vs {}={} ppm (|Δ|={} ppm > {} ppm)",
                        first.method, first.ppm, second.method, second.ppm, difference, tolerance
                    ),
                ));
            }
        }
    }
    let adopted = adopt_clock_estimates(&estimates);
    let (residual_ppm, residual_uncertainty_ppm, method) = match adopted {
        Some((ppm, uncertainty, method)) => (ppm, uncertainty, method),
        None => {
            let residual = if declared.correction == DriftCorrection::Resample {
                0.0
            } else {
                declared.ppm
            };
            push_warning(
                &mut warnings,
                TimebaseWarning::DECLARED_ONLY_CLOCK,
                "no independent clock estimate available (needs >=2 slots or >=2 repeats); trusting diag clock declaration".to_string(),
            );
            (
                residual,
                declared.uncertainty_ppm,
                ClockMethod::DeclaredOnly,
            )
        }
    };
    // 与 diag 声明对拍：
    // - correction=none：记录坐标应保留声明 ppm（残差≈声明）；
    // - correction=resample：坐标已校正，残差应≈0。
    let sigma_sum = residual_uncertainty_ppm + declared.uncertainty_ppm;
    match declared.correction {
        DriftCorrection::None => {
            let tolerance = (CLOCK_SIGMA_FACTOR * sigma_sum).max(CLOCK_DECLARED_TOLERANCE_PPM);
            let difference = (residual_ppm - declared.ppm).abs();
            if difference > tolerance {
                return Err(TimebaseError::clock_conflict(
                    "clock.declared",
                    format!(
                        "reconstructed residual ppm {} disagrees with declared {} ppm (|Δ|={} > {} ppm)",
                        residual_ppm, declared.ppm, difference, tolerance
                    ),
                ));
            }
        }
        DriftCorrection::Resample => {
            if method != ClockMethod::DeclaredOnly {
                let tolerance = (CLOCK_SIGMA_FACTOR * residual_uncertainty_ppm)
                    .max(CLOCK_RESIDUAL_TOLERANCE_PPM);
                if residual_ppm.abs() > tolerance {
                    return Err(TimebaseError::clock_drift(
                        "clock.residual",
                        format!(
                            "declared correction=resample but recorded coordinates still drift by {} ppm (> {} ppm)",
                            residual_ppm, tolerance
                        ),
                    ));
                }
            }
        }
    }
    // 显著漂移/校正一致性（FC-06）：
    let significance_sigma = residual_uncertainty_ppm.max(declared.uncertainty_ppm);
    let residual_drift_significant = residual_ppm.abs() > 2.0 * significance_sigma;
    if declared.correction == DriftCorrection::None
        && residual_drift_significant
        && !declared.drift_significant
    {
        return Err(TimebaseError::clock_drift(
            "clock.drift",
            format!(
                "reconstructed drift {} ppm is significant (2σ={} ppm) but not declared/corrected",
                residual_ppm,
                2.0 * significance_sigma
            ),
        ));
    }
    let samples_per_frame = 1.0 + residual_ppm / 1.0e6;
    let render_origin = measurements[0].diag.render_frame_index as f64;
    // capture 原点：优先用 (actual_start − analysis.delay) 的模型残差（与 onset 估计一致），
    // 否则退回 nominal_start（槽栅格原点）。
    let mut capture_offsets = Vec::with_capacity(measurements.len());
    for measurement in measurements {
        let onset = measurement.diag.actual_start - measurement.analysis.delay_samples;
        capture_offsets.push(
            onset
                - (measurement.diag.render_frame_index as f64 - render_origin) * samples_per_frame,
        );
    }
    let capture_origin = match median(&capture_offsets) {
        Some(value) => value,
        None => {
            let nominals: Vec<f64> = measurements
                .iter()
                .map(|measurement| measurement.diag.nominal_start)
                .collect();
            median(&nominals).ok_or_else(|| {
                TimebaseError::missing("position.clock", "no measurements for clock origin")
            })?
        }
    };
    Ok((
        ClockMap {
            sample_rate_hz,
            declared_ppm: declared.ppm,
            declared_uncertainty_ppm: declared.uncertainty_ppm,
            declared_drift_significant: declared.drift_significant,
            correction: declared.correction,
            residual_ppm,
            residual_uncertainty_ppm,
            residual_drift_significant,
            estimates,
            method,
            samples_per_frame,
            render_origin,
            capture_origin,
            points: measurements.len(),
        },
        warnings,
    ))
}

// ===========================================================================
// coarse/frac 延迟分解
// ===========================================================================

/// 延迟的 coarse/frac 分解（coarse 为整数样本，frac ∈ [0,1)）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DelaySplit {
    /// 总延迟（样本；可为负）。
    pub total_samples: f64,
    /// 整数粗延迟（= floor(total)）。
    pub coarse_samples: i64,
    /// 小数部分（= total − coarse ∈ [0,1)）。
    pub frac_samples: f64,
}

impl DelaySplit {
    /// 从总延迟构造；NaN/Inf → `E_TB_RANGE`。
    pub fn new(total_samples: f64) -> Result<Self, TimebaseError> {
        let total_samples = finite(total_samples, "delay.total_samples")?;
        let coarse = total_samples.floor();
        if coarse.abs() > 9.0e15 {
            return Err(TimebaseError::range(
                "delay.total_samples",
                "delay magnitude exceeds exact f64 integer range",
            ));
        }
        Ok(DelaySplit {
            total_samples,
            coarse_samples: coarse as i64,
            frac_samples: total_samples - coarse,
        })
    }

    /// 归一化小数到 (−0.5, 0.5]（用于跨表示比较；不改变 coarse）。
    pub fn centered_fraction(&self) -> f64 {
        if self.frac_samples > 0.5 {
            self.frac_samples - 1.0
        } else {
            self.frac_samples
        }
    }
}

// ===========================================================================
// slot / position 时基
// ===========================================================================

/// 一个 participant slot 的时间基结果。
#[derive(Debug, Clone, PartialEq)]
pub struct SlotTimebase {
    /// slot 序号（0..M-1）。
    pub slot: u32,
    /// participant 角色名。
    pub speaker: String,
    /// 播放设备通道号。
    pub out_index: u32,
    /// slot 扫频起点对应的 host render frame（MOTU/epoch 域）。
    pub render_frame_index: u64,
    /// 扫频起点在 UMIK capture 中的样本下标（整数，diag 记录）。
    pub capture_sample_index: u64,
    /// 名义槽起点（capture 样本）。
    pub nominal_start: f64,
    /// 互相关实测起点（capture 样本）。
    pub actual_start: f64,
    /// actual − nominal（capture 样本；diag 声明）。
    pub lag_samples: f64,
    /// 重复次数。
    pub repeats: u32,
    /// 每遍实测起点（capture 样本）。
    pub repeat_starts: Vec<f64>,
    /// 每遍起点换算到公共 render frame 域（由 clock map 逆映射）。
    pub repeat_render_frames: Vec<f64>,
    /// diag 原点下的 coarse/frac 分解（lag_samples）。
    pub lag_split: DelaySplit,
    /// analysis 声明的逐 participant 实测传播延迟（样本；**provenance**，不参与去相位）。
    pub analysis_delay_samples: f64,
    /// analysis 的粗延迟字段（producer 口径，整数）。
    pub analysis_coarse_samples: i64,
    /// analysis 的 delay_samples − delay_coarse_samples（producer 口径小数残差，可为任意值）。
    pub analysis_frac_samples: f64,
    /// analysis 原点下的 coarse/frac 分解（= floor(delay_samples) 口径）。
    pub analysis_split: DelaySplit,
    /// 逐 participant 实测延迟相对公共整数 D0 的残差（样本；provenance，**不参与**去相位）。
    pub per_participant_residual_delay_samples: f64,
    /// 上述残差的 coarse/frac 分解（provenance；不参与去相位）。
    pub per_participant_residual_split: DelaySplit,
    /// 该 slot 的物理极性（已解析；未乘进响应）。
    pub polarity: ResolvedPolarity,
    /// 该 slot 的相位基准语义。
    pub phase_basis: PhaseBasis,
    /// lag − delay 相对 position 常数原点的残差（样本）。
    pub delay_origin_residual_samples: f64,
}

/// 一个 position 的完整时间基结果。
#[derive(Debug, Clone, PartialEq)]
pub struct PositionTimebase {
    /// position label。
    pub label: String,
    /// design|validation。
    pub role: PositionRole,
    /// 设计/验证权重。
    pub weight: f64,
    /// session id（公共时轴）。
    pub session_id: String,
    /// host epoch id（公共 render frame 基准）。
    pub epoch_id: u64,
    /// host graph id（64 hex）。
    pub graph_id: String,
    /// 采样率（Hz）。
    pub sample_rate_hz: u32,
    /// T6 FC-04 预声明公共整数 D0（samples@fs；全部 slot 共用同一值，去相位只扣它一次）。
    pub common_d0: CommonD0,
    /// render↔capture 时钟映射。
    pub clock: ClockMap,
    /// 按 slot 升序的 slot 时基。
    pub slots: Vec<SlotTimebase>,
    /// diag lag 与 analysis delay 的常数原点偏移（样本；中位数）。
    pub delay_origin_offset_samples: f64,
    /// 原点偏移的逐 slot 最大残差（样本；需 ≤ DELAY_ORIGIN_SPREAD_MAX_SAMPLES）。
    pub delay_origin_spread_samples: f64,
    /// slot render frame 间隔中位数（帧）。
    pub slot_interval_frames_median: f64,
    /// slot render frame 间隔极差（帧）。
    pub slot_interval_frames_spread: f64,
    /// 预期 slot 间隔（= R*(sweep+gap)*fs，帧）。
    pub expected_slot_interval_frames: f64,
    /// 非致命诊断。
    pub warnings: Vec<TimebaseWarning>,
}

impl PositionTimebase {
    /// 按 slot 序号取 slot 时基。
    pub fn slot(&self, slot: u32) -> Option<&SlotTimebase> {
        self.slots.iter().find(|entry| entry.slot == slot)
    }

    /// 按 participant 名取 slot 时基。
    pub fn measurement(&self, speaker: &str) -> Option<&SlotTimebase> {
        self.slots.iter().find(|entry| entry.speaker == speaker)
    }

    /// participant 名（按 slot 顺序）。
    pub fn participant_names(&self) -> Vec<&str> {
        self.slots
            .iter()
            .map(|entry| entry.speaker.as_str())
            .collect()
    }

    /// 生成可序列化摘要（T50/T54/T63/T66 报告引用）。
    pub fn summary(&self) -> PositionSummary {
        PositionSummary {
            label: self.label.clone(),
            role: self.role.as_str(),
            weight: self.weight,
            common_d0_samples: self.common_d0.samples(),
            clock: ClockSummary {
                declared_ppm: self.clock.declared_ppm,
                declared_uncertainty_ppm: self.clock.declared_uncertainty_ppm,
                correction: self.clock.correction.as_str(),
                residual_ppm: self.clock.residual_ppm,
                residual_uncertainty_ppm: self.clock.residual_uncertainty_ppm,
                residual_drift_significant: self.clock.residual_drift_significant,
                method: self.clock.method.as_str(),
                independent: self.clock.independent(),
                samples_per_frame: self.clock.samples_per_frame,
                render_origin: self.clock.render_origin,
                capture_origin: self.clock.capture_origin,
                estimates: self.clock.estimates.clone(),
            },
            delay_origin_offset_samples: self.delay_origin_offset_samples,
            delay_origin_spread_samples: self.delay_origin_spread_samples,
            slot_interval_frames_median: self.slot_interval_frames_median,
            expected_slot_interval_frames: self.expected_slot_interval_frames,
            slots: self
                .slots
                .iter()
                .map(|slot| SlotSummary {
                    slot: slot.slot,
                    speaker: slot.speaker.clone(),
                    out_index: slot.out_index,
                    render_frame_index: slot.render_frame_index,
                    capture_sample_index: slot.capture_sample_index,
                    nominal_start: slot.nominal_start,
                    actual_start: slot.actual_start,
                    lag_samples: slot.lag_samples,
                    coarse_samples: slot.lag_split.coarse_samples,
                    frac_samples: slot.lag_split.frac_samples,
                    analysis_delay_samples: slot.analysis_delay_samples,
                    analysis_coarse_samples: slot.analysis_coarse_samples,
                    analysis_frac_samples: slot.analysis_frac_samples,
                    per_participant_residual_delay_samples: slot
                        .per_participant_residual_delay_samples,
                    per_participant_residual_coarse_samples: slot
                        .per_participant_residual_split
                        .coarse_samples,
                    per_participant_residual_frac_samples: slot
                        .per_participant_residual_split
                        .frac_samples,
                    polarity: slot.polarity.as_i8(),
                    phase_basis: slot.phase_basis.as_str(),
                    repeats: slot.repeats,
                    repeat_render_frames: slot.repeat_render_frames.clone(),
                })
                .collect(),
            warnings: self.warnings.clone(),
        }
    }
}

/// 一个 session 的完整时间基（全部 position + 公共 ids）。
#[derive(Debug, Clone, PartialEq)]
pub struct BundleTimebase {
    /// session id。
    pub session_id: String,
    /// 公共 epoch id。
    pub epoch_id: u64,
    /// 公共 graph id。
    pub graph_id: String,
    /// 采样率（Hz）。
    pub sample_rate_hz: u32,
    /// 整个 bundle（全部 position/轮次）共用的 T6 FC-04 公共整数 D0。
    pub common_d0: CommonD0,
    /// 按 position label 顺序（= ingest bundle 顺序）。
    pub positions: Vec<PositionTimebase>,
    /// 非致命诊断（各 position 汇总）。
    pub warnings: Vec<TimebaseWarning>,
}

impl BundleTimebase {
    /// 按 position label 取时基。
    pub fn position(&self, label: &str) -> Option<&PositionTimebase> {
        self.positions.iter().find(|entry| entry.label == label)
    }

    /// design position 迭代器。
    pub fn design_positions(&self) -> impl Iterator<Item = &PositionTimebase> {
        self.positions
            .iter()
            .filter(|entry| entry.role == PositionRole::Design)
    }

    /// validation/holdout position 迭代器。
    pub fn validation_positions(&self) -> impl Iterator<Item = &PositionTimebase> {
        self.positions
            .iter()
            .filter(|entry| entry.role == PositionRole::Validation)
    }

    /// 生成可序列化摘要（schema [TIMEBASE_SCHEMA]）。
    pub fn summary(&self) -> TimebaseSummary {
        TimebaseSummary {
            schema: TIMEBASE_SCHEMA,
            session_id: self.session_id.clone(),
            epoch_id: self.epoch_id,
            graph_id: self.graph_id.clone(),
            sample_rate_hz: self.sample_rate_hz,
            common_d0_samples: self.common_d0.samples(),
            positions: self
                .positions
                .iter()
                .map(PositionTimebase::summary)
                .collect(),
            warnings: self.warnings.clone(),
        }
    }
}

/// 可序列化时基摘要（下游报告引用）。
#[derive(Debug, Clone, PartialEq)]
pub struct TimebaseSummary {
    /// schema = [TIMEBASE_SCHEMA]。
    pub schema: &'static str,
    /// session id。
    pub session_id: String,
    /// 公共 epoch id。
    pub epoch_id: u64,
    /// 公共 graph id。
    pub graph_id: String,
    /// 采样率（Hz）。
    pub sample_rate_hz: u32,
    /// T6 FC-04 公共整数 D0（samples@fs；全部 position/轮次同一值）。
    pub common_d0_samples: i64,
    /// 各 position 摘要。
    pub positions: Vec<PositionSummary>,
    /// 非致命诊断。
    pub warnings: Vec<TimebaseWarning>,
}

/// position 摘要。
#[derive(Debug, Clone, PartialEq)]
pub struct PositionSummary {
    /// position label。
    pub label: String,
    /// design|validation。
    pub role: &'static str,
    /// 设计/验证权重。
    pub weight: f64,
    /// T6 FC-04 公共整数 D0（samples@fs）。
    pub common_d0_samples: i64,
    /// 时钟摘要。
    pub clock: ClockSummary,
    /// diag/analysis 延迟常数原点偏移（样本）。
    pub delay_origin_offset_samples: f64,
    /// 原点残差最大值（样本）。
    pub delay_origin_spread_samples: f64,
    /// slot 间隔中位数（帧）。
    pub slot_interval_frames_median: f64,
    /// 预期 slot 间隔（帧）。
    pub expected_slot_interval_frames: f64,
    /// 各 slot 摘要。
    pub slots: Vec<SlotSummary>,
    /// 非致命诊断。
    pub warnings: Vec<TimebaseWarning>,
}

/// 时钟摘要。
#[derive(Debug, Clone, PartialEq)]
pub struct ClockSummary {
    /// diag 声明 ppm。
    pub declared_ppm: f64,
    /// diag 声明 σ。
    pub declared_uncertainty_ppm: f64,
    /// 校正方式（none|resample）。
    pub correction: &'static str,
    /// 残差 ppm。
    pub residual_ppm: f64,
    /// 残差 ppm σ。
    pub residual_uncertainty_ppm: f64,
    /// 残差漂移是否显著。
    pub residual_drift_significant: bool,
    /// 采用方法。
    pub method: &'static str,
    /// 是否有独立估计。
    pub independent: bool,
    /// samples_per_frame。
    pub samples_per_frame: f64,
    /// render 原点。
    pub render_origin: f64,
    /// capture 原点。
    pub capture_origin: f64,
    /// 全部独立估计。
    pub estimates: Vec<ClockEstimate>,
}

/// slot 摘要。
#[derive(Debug, Clone, PartialEq)]
pub struct SlotSummary {
    /// slot 序号。
    pub slot: u32,
    /// participant 角色名。
    pub speaker: String,
    /// 播放设备通道号。
    pub out_index: u32,
    /// host render frame（MOTU/epoch 域）。
    pub render_frame_index: u64,
    /// capture 起点样本下标。
    pub capture_sample_index: u64,
    /// 名义起点（capture 样本）。
    pub nominal_start: f64,
    /// 实测起点（capture 样本）。
    pub actual_start: f64,
    /// diag lag（capture 样本）。
    pub lag_samples: f64,
    /// diag 原点 coarse 延迟（样本）。
    pub coarse_samples: i64,
    /// diag 原点 frac 延迟（[0,1)）。
    pub frac_samples: f64,
    /// analysis 逐 participant 实测传播延迟（provenance；不参与去相位）。
    pub analysis_delay_samples: f64,
    /// analysis 粗延迟字段。
    pub analysis_coarse_samples: i64,
    /// analysis 小数残差（producer 口径）。
    pub analysis_frac_samples: f64,
    /// 逐 participant 实测延迟相对公共 D0 的残差（样本；provenance，不参与去相位）。
    pub per_participant_residual_delay_samples: f64,
    /// 上述残差的 coarse 部分（样本）。
    pub per_participant_residual_coarse_samples: i64,
    /// 上述残差的 frac 部分（[0,1)）。
    pub per_participant_residual_frac_samples: f64,
    /// 物理极性（±1）。
    pub polarity: i8,
    /// 相位基准语义。
    pub phase_basis: &'static str,
    /// 重复次数。
    pub repeats: u32,
    /// 每遍起点的公共 render frame。
    pub repeat_render_frames: Vec<f64>,
}

// ===========================================================================
// position / bundle 重建
// ===========================================================================

fn validate_session_for_timebase(session: &IngestSession) -> Result<(), TimebaseError> {
    if !SAMPLE_RATES_HZ.contains(&session.sample_rate_hz) {
        return Err(TimebaseError::unsupported(
            "session.sample_rate_hz",
            format!(
                "sample rate {} Hz is not a P1 rate {SAMPLE_RATES_HZ:?}",
                session.sample_rate_hz
            ),
        ));
    }
    if session.capture.channels != 1 {
        return Err(TimebaseError::unsupported(
            "session.capture.channels",
            format!(
                "P1 UMIK capture must be 1ch, session declares {}",
                session.capture.channels
            ),
        ));
    }
    if let Some(reference) = &session.reference {
        if !reference.independent {
            return Err(TimebaseError::reference(
                "session.reference.independent",
                "reference channel must be independent (independent=true)",
            ));
        }
        if reference.device == session.capture.device {
            return Err(TimebaseError::reference(
                "session.reference.device",
                "reference device must differ from UMIK capture device",
            ));
        }
        if reference.channels == 0 || reference.channels as usize > MAX_INPUT_CHANNELS {
            return Err(TimebaseError::reference(
                "session.reference.channels",
                format!(
                    "reference channels {} outside 1..={MAX_INPUT_CHANNELS}",
                    reference.channels
                ),
            ));
        }
    }
    Ok(())
}

fn validate_capture_file(
    capture: &CaptureFile,
    context: &str,
    session: &IngestSession,
) -> Result<(), TimebaseError> {
    if capture.channels != 1 {
        return Err(TimebaseError::unsupported(
            format!("{context}.channels"),
            format!("P1 UMIK capture file must be 1ch, got {}", capture.channels),
        ));
    }
    if capture.sample_rate_hz != session.sample_rate_hz {
        return Err(TimebaseError::unsupported(
            format!("{context}.sample_rate_hz"),
            format!(
                "capture sample rate {} != session {}",
                capture.sample_rate_hz, session.sample_rate_hz
            ),
        ));
    }
    if capture.frames == 0 {
        return Err(TimebaseError::range(
            format!("{context}.frames"),
            "capture has 0 frames",
        ));
    }
    Ok(())
}

fn resolve_participant_out_index(
    session: &IngestSession,
    speaker: &str,
    out_index: u32,
    context: &str,
) -> Result<(), TimebaseError> {
    let declared = session
        .speakers
        .iter()
        .find(|entry| entry.name == speaker)
        .ok_or_else(|| {
            TimebaseError::slot_order(
                context,
                format!("participant {speaker:?} not declared in session.speakers"),
            )
        })?;
    if declared.out_index != out_index {
        return Err(TimebaseError::slot_order(
            context,
            format!(
                "participant {speaker:?} out_index {out_index} != session speaker declaration {}",
                declared.out_index
            ),
        ));
    }
    if out_index as usize >= MAX_OUTPUT_CHANNELS {
        return Err(TimebaseError::range(
            context,
            format!("out_index {out_index} outside MAX_OUTPUT_CHANNELS={MAX_OUTPUT_CHANNELS}"),
        ));
    }
    Ok(())
}

fn validate_slot_diag(
    measurement: &IngestMeasurement,
    session: &IngestSession,
    expected_slot: u32,
    context: &str,
) -> Result<TimebaseDiag, TimebaseError> {
    let diag = measurement.diag.clone();
    if diag.slot != expected_slot || measurement.slot != expected_slot {
        return Err(TimebaseError::slot_order(
            context,
            format!(
                "slot mismatch: measurement {} / diag {} / expected {expected_slot}",
                measurement.slot, diag.slot
            ),
        ));
    }
    if diag.speaker != measurement.speaker {
        return Err(TimebaseError::slot_order(
            context,
            format!(
                "diag speaker {:?} != measurement speaker {:?}",
                diag.speaker, measurement.speaker
            ),
        ));
    }
    if diag.channel != measurement.out_index
        || measurement.analysis.channel != measurement.out_index
    {
        return Err(TimebaseError::slot_order(
            context,
            format!(
                "channel mismatch: diag {} / analysis {} / out_index {}",
                diag.channel, measurement.analysis.channel, measurement.out_index
            ),
        ));
    }
    if diag.session_id != session.session_id {
        return Err(TimebaseError::slot_order(
            format!("{context}.session_id"),
            format!(
                "diag session_id {:?} != session {:?}",
                diag.session_id, session.session_id
            ),
        ));
    }
    if diag.epoch_id == 0 {
        return Err(TimebaseError::range(
            format!("{context}.epoch_id"),
            "epoch_id must be > 0",
        ));
    }
    if diag.graph_id.len() != 64
        || !diag
            .graph_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || diag.graph_id.bytes().all(|byte| byte == b'0')
    {
        return Err(TimebaseError::range(
            format!("{context}.graph_id"),
            "graph_id must be 64 lowercase hex and not all zero",
        ));
    }
    if diag.repeats == 0 {
        return Err(TimebaseError::range(
            format!("{context}.repeats"),
            "repeats must be >= 1",
        ));
    }
    if diag.repeats != session.stimulus.repeats {
        return Err(TimebaseError::slot_order(
            format!("{context}.repeats"),
            format!(
                "diag repeats {} != stimulus.repeats {}",
                diag.repeats, session.stimulus.repeats
            ),
        ));
    }
    if diag.repeat_starts.len() != diag.repeats as usize {
        return Err(TimebaseError::slot_order(
            format!("{context}.repeat_starts"),
            format!(
                "repeat_starts length {} != repeats {}",
                diag.repeat_starts.len(),
                diag.repeats
            ),
        ));
    }
    if diag.render_frame_index == 0 {
        return Err(TimebaseError::range(
            format!("{context}.render_frame_index"),
            "render_frame_index must be > 0",
        ));
    }
    if diag.onset_invalid {
        return Err(TimebaseError::slot_order(
            format!("{context}.onset_invalid"),
            "onset_invalid=true is not deliverable",
        ));
    }
    let nominal = finite(diag.nominal_start, &format!("{context}.nominal_start"))?;
    let actual = finite(diag.actual_start, &format!("{context}.actual_start"))?;
    let lag = finite(diag.lag_samples, &format!("{context}.lag_samples"))?;
    if nominal < 0.0 || actual < 0.0 {
        return Err(TimebaseError::range(
            context,
            "nominal_start/actual_start must be >= 0",
        ));
    }
    let fs = session.sample_rate_hz as f64;
    if lag.abs() > DELAY_SANITY_FACTOR * fs {
        return Err(TimebaseError::range(
            format!("{context}.lag_samples"),
            format!("|lag|={} exceeds sanity bound {fs}", lag.abs()),
        ));
    }
    if (lag - (actual - nominal)).abs() > 0.01 {
        return Err(TimebaseError::range(
            format!("{context}.lag_samples"),
            "lag_samples must equal actual_start - nominal_start (±0.01)",
        ));
    }
    if (diag.capture_sample_index as f64 - actual).abs() > 1.0 {
        return Err(TimebaseError::range(
            format!("{context}.capture_sample_index"),
            "capture_sample_index must be within 1 sample of actual_start",
        ));
    }
    let mut previous = f64::NEG_INFINITY;
    for (index, start) in diag.repeat_starts.iter().enumerate() {
        let start = finite(*start, &format!("{context}.repeat_starts[{index}]"))?;
        if start < 0.0 || start <= previous {
            return Err(TimebaseError::slot_order(
                format!("{context}.repeat_starts[{index}]"),
                "repeat_starts must be >= 0 and strictly increasing",
            ));
        }
        previous = start;
    }
    let first_repeat = *diag.repeat_starts.first().ok_or_else(|| {
        TimebaseError::missing(format!("{context}.repeat_starts"), "empty repeat_starts")
    })?;
    if (first_repeat - actual).abs() > 1.0 {
        return Err(TimebaseError::range(
            format!("{context}.repeat_starts[0]"),
            "repeat_starts[0] must be within 1 sample of actual_start",
        ));
    }
    Ok(diag)
}

/// 重建一个 position 的时间基（clock/slot/coarse-frac/极性/相位基准/公共 D0）。
///
/// `common_d0` 是 T6 FC-04 预声明的**公共整数 D0**（samples@fs）：本函数把它写进
/// [PositionTimebase::common_d0]，并对每个 slot 记录 `per_participant_residual_delay_samples
/// = analysis.delay_samples − D0`（provenance；**不**参与去相位）。T48 measured delay 只有
/// 在 [reconstruct_common_phase]/[de_phase_analysis] 之外作为时间轴一致性/provenance 使用。
///
/// 纯计算；不读文件、不采音。输入必须是 T48 ingest 产出的结构（或同 schema 的直接构造，
/// 用于 T49 自身负向单测）；所有 ingest 已校验的不变量在此**重新校验**（防直接构造绕过），
/// 包括 analysis.delay_samples/delay_coarse_samples/delay_ms 自洽与 analysis fs == session fs。
pub fn reconstruct_position(
    position: &IngestPosition,
    session: &IngestSession,
    common_d0: CommonD0,
) -> Result<PositionTimebase, TimebaseError> {
    validate_session_for_timebase(session)?;
    validate_capture_file(
        &position.capture,
        &format!("position.{}.capture", position.meta.name),
        session,
    )?;
    if position.measurements.is_empty() {
        return Err(TimebaseError::missing(
            format!("position.{}", position.meta.name),
            "position has no measurements",
        ));
    }
    if position.measurements.len() > MAX_LOW_BAND_PARTICIPANTS {
        return Err(TimebaseError::range(
            format!("position.{}.measurements", position.meta.name),
            format!(
                "{} participants exceed MAX_LOW_BAND_PARTICIPANTS={MAX_LOW_BAND_PARTICIPANTS}",
                position.measurements.len()
            ),
        ));
    }
    let mut measurements: Vec<&IngestMeasurement> = position.measurements.iter().collect();
    measurements.sort_by_key(|measurement| measurement.slot);
    let mut warnings = Vec::new();
    let mut epoch_id = None;
    let mut graph_id: Option<String> = None;
    let mut previous_render_frame: Option<u64> = None;
    let mut previous_capture_index: Option<u64> = None;
    let mut basis: Option<PhaseBasis> = None;
    let mut lag_minus_delay = Vec::with_capacity(measurements.len());
    let mut prepared = Vec::with_capacity(measurements.len());
    for (index, measurement) in measurements.iter().enumerate() {
        let context = format!("position.{}.slot.{}", position.meta.name, index);
        let diag = validate_slot_diag(measurement, session, index as u32, &context)?;
        resolve_participant_out_index(
            session,
            &measurement.speaker,
            measurement.out_index,
            &context,
        )?;
        match epoch_id {
            None => epoch_id = Some(diag.epoch_id),
            Some(expected) if expected != diag.epoch_id => {
                return Err(TimebaseError::slot_order(
                    format!("{context}.epoch_id"),
                    format!(
                        "epoch_id changed within one position: {expected} vs {}",
                        diag.epoch_id
                    ),
                ));
            }
            Some(_) => {}
        }
        match &graph_id {
            None => graph_id = Some(diag.graph_id.clone()),
            Some(expected) if expected != &diag.graph_id => {
                return Err(TimebaseError::slot_order(
                    format!("{context}.graph_id"),
                    format!(
                        "graph_id changed within one position: {expected} vs {}",
                        diag.graph_id
                    ),
                ));
            }
            Some(_) => {}
        }
        if let Some(previous) = previous_render_frame {
            if diag.render_frame_index <= previous {
                return Err(TimebaseError::slot_order(
                    format!("{context}.render_frame_index"),
                    "render_frame_index must be strictly increasing with slot",
                ));
            }
        }
        previous_render_frame = Some(diag.render_frame_index);
        if let Some(previous) = previous_capture_index {
            if diag.capture_sample_index <= previous {
                return Err(TimebaseError::slot_order(
                    format!("{context}.capture_sample_index"),
                    "capture_sample_index must be strictly increasing with slot",
                ));
            }
        }
        previous_capture_index = Some(diag.capture_sample_index);
        let parsed_basis = PhaseBasis::parse(&measurement.analysis.phase_ref)?;
        if parsed_basis == PhaseBasis::DelayRemoved {
            return Err(TimebaseError::phase_ref(
                format!("{context}.phase_ref"),
                "phase_ref=delay_removed is unsupported by v1 ingest (no declared D); refusing to guess",
            ));
        }
        match basis {
            None => basis = Some(parsed_basis),
            Some(expected) if expected != parsed_basis => {
                return Err(TimebaseError::phase_ref(
                    format!("{context}.phase_ref"),
                    format!(
                        "phase basis changed within one position: {expected} vs {parsed_basis}"
                    ),
                ));
            }
            Some(_) => {}
        }
        let polarity =
            ResolvedPolarity::resolve(measurement.analysis.polarity, PolaritySource::Analysis)?;
        if measurement.analysis.meta.sample_rate_hz != session.sample_rate_hz {
            return Err(TimebaseError::unsupported(
                format!("{context}.analysis.meta.sample_rate"),
                format!(
                    "analysis sample rate {} != session {} (T49 requires equal capture/render rates)",
                    measurement.analysis.meta.sample_rate_hz, session.sample_rate_hz
                ),
            ));
        }
        let delay =
            validate_analysis_delay_fields(&measurement.analysis, &format!("{context}.analysis"))?;
        lag_minus_delay.push(diag.lag_samples - delay);
        prepared.push((
            diag,
            parsed_basis,
            polarity,
            delay,
            measurement.analysis.delay_coarse_samples,
            measurement.speaker.clone(),
        ));
    }
    let (clock, clock_warnings) = reconstruct_clock(&measurements, session)?;
    warnings.extend(clock_warnings);
    let delay_origin_offset_samples = median(&lag_minus_delay).ok_or_else(|| {
        TimebaseError::missing(
            format!("position.{}.delay", position.meta.name),
            "no measurements for delay origin",
        )
    })?;
    let delay_origin_spread_samples = lag_minus_delay
        .iter()
        .map(|value| (value - delay_origin_offset_samples).abs())
        .fold(0.0f64, f64::max);
    if delay_origin_spread_samples > DELAY_ORIGIN_SPREAD_MAX_SAMPLES {
        return Err(TimebaseError::delay_origin(
            format!("position.{}.delay", position.meta.name),
            format!(
                "diag lag vs analysis delay origin is not constant: max residual {} samples > {} samples",
                delay_origin_spread_samples, DELAY_ORIGIN_SPREAD_MAX_SAMPLES
            ),
        ));
    }
    let fs = session.sample_rate_hz as f64;
    let expected_slot_interval_frames = session.stimulus.repeats as f64
        * (session.stimulus.seconds + session.stimulus.repeat_gap_seconds)
        * fs;
    let mut slots = Vec::with_capacity(prepared.len());
    let mut repeat_interval_residual_max = 0.0f64;
    for (diag, parsed_basis, polarity, delay, analysis_coarse, speaker) in prepared {
        let lag_split = DelaySplit::new(diag.lag_samples)?;
        let analysis_split = DelaySplit::new(delay)?;
        let analysis_frac_samples = delay - analysis_coarse as f64;
        let per_participant_residual_delay_samples = common_d0.residual_of(delay);
        let per_participant_residual_split =
            DelaySplit::new(per_participant_residual_delay_samples)?;
        let expected_repeat_interval = (session.stimulus.seconds
            + session.stimulus.repeat_gap_seconds)
            * fs
            * clock.samples_per_frame;
        for window in diag.repeat_starts.windows(2) {
            let residual = ((window[1] - window[0]) - expected_repeat_interval).abs();
            repeat_interval_residual_max = repeat_interval_residual_max.max(residual);
        }
        let repeat_render_frames = diag
            .repeat_starts
            .iter()
            .map(|start| clock.render_of_capture(*start))
            .collect();
        slots.push(SlotTimebase {
            slot: diag.slot,
            speaker,
            out_index: diag.channel,
            render_frame_index: diag.render_frame_index,
            capture_sample_index: diag.capture_sample_index,
            nominal_start: diag.nominal_start,
            actual_start: diag.actual_start,
            lag_samples: diag.lag_samples,
            repeats: diag.repeats,
            repeat_starts: diag.repeat_starts.clone(),
            repeat_render_frames,
            lag_split,
            analysis_delay_samples: delay,
            analysis_coarse_samples: analysis_coarse,
            analysis_frac_samples,
            analysis_split,
            per_participant_residual_delay_samples,
            per_participant_residual_split,
            polarity,
            phase_basis: parsed_basis,
            delay_origin_residual_samples: (diag.lag_samples - delay) - delay_origin_offset_samples,
        });
    }
    let slot_render_frames: Vec<f64> = slots
        .iter()
        .map(|slot| slot.render_frame_index as f64)
        .collect();
    let mut intervals = Vec::with_capacity(slot_render_frames.len().saturating_sub(1));
    for window in slot_render_frames.windows(2) {
        intervals.push(window[1] - window[0]);
    }
    let slot_interval_frames_median = median(&intervals).unwrap_or(0.0);
    let slot_interval_frames_spread = if intervals.is_empty() {
        0.0
    } else {
        let min = intervals.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = intervals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        max - min
    };
    if !intervals.is_empty() {
        let tolerance = SLOT_SPACING_RELATIVE_TOLERANCE * expected_slot_interval_frames
            + SLOT_SPACING_ABSOLUTE_TOLERANCE_FRAMES;
        if (slot_interval_frames_median - expected_slot_interval_frames).abs() > tolerance {
            push_warning(
                &mut warnings,
                TimebaseWarning::SLOT_SPACING,
                format!(
                    "median slot interval {} frames != expected sequential interval {} frames (tolerance ±{} frames)",
                    slot_interval_frames_median, expected_slot_interval_frames, tolerance
                ),
            );
        }
        for (index, window) in slots.windows(2).enumerate() {
            let last_repeat = window[0]
                .repeat_render_frames
                .last()
                .copied()
                .unwrap_or(f64::NAN);
            let next_slot_frame = window[1].render_frame_index as f64;
            if last_repeat.is_finite() && last_repeat > next_slot_frame {
                push_warning(
                    &mut warnings,
                    TimebaseWarning::SLOT_OVERLAP,
                    format!(
                        "repeat window of slot {} ends at render frame {} after slot {} starts at {}",
                        index,
                        last_repeat,
                        index + 1,
                        next_slot_frame
                    ),
                );
            }
        }
    }
    if repeat_interval_residual_max > REPEAT_INTERVAL_RESIDUAL_TOLERANCE_SAMPLES {
        push_warning(
            &mut warnings,
            TimebaseWarning::REPEAT_SPACING,
            format!(
                "repeat interval deviates from modeled (sweep+gap)*fs*sppf by up to {} samples (> {} samples)",
                repeat_interval_residual_max, REPEAT_INTERVAL_RESIDUAL_TOLERANCE_SAMPLES
            ),
        );
    }
    Ok(PositionTimebase {
        label: position.meta.name.clone(),
        role: position.meta.role,
        weight: position.meta.weight,
        session_id: session.session_id.clone(),
        epoch_id: epoch_id.unwrap_or(0),
        graph_id: graph_id.unwrap_or_default(),
        sample_rate_hz: session.sample_rate_hz,
        common_d0,
        clock,
        slots,
        delay_origin_offset_samples,
        delay_origin_spread_samples,
        slot_interval_frames_median,
        slot_interval_frames_spread,
        expected_slot_interval_frames,
        warnings,
    })
}

/// 重建整个 ingest bundle 的时间基（全部 position + 公共 session/epoch/graph）。
///
/// 校验：session 采样率/UMIK 1ch/reference 声明；每 position 独立重建；全部 position 的
/// session_id/epoch_id/graph_id 必须一致（公共 render frame 基准）；participant 列表
/// 必须一致（MIMO 需要跨 position 对齐同一组 role）。空 bundle → `E_TB_MISSING`。
///
/// `common_d0` 是整个 bundle（全部 position/轮次）共用的 T6 FC-04 公共整数 D0；跨轮一致性
/// 由 host/冻结清单保证（FC-04/FC-16），本函数只接受一个声明值并回显到每个 position。
pub fn reconstruct_bundle(
    bundle: &IngestBundle,
    common_d0: CommonD0,
) -> Result<BundleTimebase, TimebaseError> {
    validate_session_for_timebase(&bundle.session)?;
    if bundle.positions.is_empty() {
        return Err(TimebaseError::missing(
            "bundle.positions",
            "ingest bundle contains no positions",
        ));
    }
    let mut positions = Vec::with_capacity(bundle.positions.len());
    for position in &bundle.positions {
        positions.push(reconstruct_position(position, &bundle.session, common_d0)?);
    }
    let first = &positions[0];
    let mut warnings = Vec::new();
    for entry in &positions {
        if entry.session_id != first.session_id
            || entry.epoch_id != first.epoch_id
            || entry.graph_id != first.graph_id
        {
            return Err(TimebaseError::slot_order(
                format!("bundle.{}", entry.label),
                "cross-position common render frame basis differs (session_id/epoch_id/graph_id)",
            ));
        }
        if entry.participant_names() != first.participant_names() {
            return Err(TimebaseError::slot_order(
                format!("bundle.{}", entry.label),
                "participants differ across positions; MIMO common timebase requires the same role set",
            ));
        }
        warnings.extend(entry.warnings.iter().cloned());
    }
    Ok(BundleTimebase {
        session_id: first.session_id.clone(),
        epoch_id: first.epoch_id,
        graph_id: first.graph_id.clone(),
        sample_rate_hz: first.sample_rate_hz,
        common_d0,
        positions,
        warnings,
    })
}

// ===========================================================================
// 静态守卫（单测用：证明本模块无 I/O/音频/进程依赖）
// ===========================================================================

/// 本模块源码文本（单测静态守卫用；不参与运行期逻辑）。
pub fn timebase_source_text_for_guards() -> &'static str {
    include_str!("timebase.rs")
}
