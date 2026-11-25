//! [NONRT] 模态目标构建（WBS T50）—— 拟合后冻结 P、H_eff = H_raw·P、真实 5×5 减尾目标、
//! baseline tone 只应用一次、高度全频 scope 分离、stereo strict 独立 profile 求解验证。
//!
//! ## 权威契约（只读输入，禁止在本文件改写）
//!
//! - `scratch/lead/planner/TSPI-RT-STACK-WBS.md` T50：拟合后冻结 P、H_eff=H_raw*P 与真实 5×5
//!   damping 目标；baseline tone 不双叠；stereo strict 受限 profile 单独求解验证。
//! - `scratch/lead/planner/DSP-HOST-DECISION.md` §0/§1/§2.5：高度普通全频（无 HP/LP/人工 18 ms），
//!   `mimo_participants` 显式 FL/FR/SL/SR/SUB1（5）来自任务边界；`y_p = H_raw,p P_post G A_map x`、
//!   `H_eff = H_raw P_post`；P/G 不交换；低带 broad tone shaping 归基线 P，G 只承担 modal residual；
//!   `curve_id` 只绑定基线，不是在 G 上再乘一次滤波；stereo strict 对输出集合 FL/FR 施硬约束，
//!   不得 runtime mask 现 G 破坏预测。
//! - `scratch/lead/planner/M1-FINAL-SPEC.md` §1/§2/§3：`H_eff = H_raw C`，在 curve 已生效的冻结基线
//!   H_eff 上选 preserve 或 `T*_60(f)/ratio`（探索 0.85/0.70/0.55，不保证可实现）；taps 是支撑长度；
//!   目标矩阵声明（M×M 复矩阵或 per-position）与公共 D 必须冻结；状态字段诚实
//!   （PREDICTED_PASS/PREDICTED_PARTIAL/INFEASIBLE/INCOMPLETE、`applied=false`）。
//! - `rt-stack/spec/mimo-t6.md`：FC-02 participants/dense_to_dac、FC-03 baseline/on 图差异唯一化、
//!   FC-04 公共整数 D0（只扣一次）、FC-07 电平/增益、FC-16 冻结后禁止事后改动；§3.1 参与集合
//!   与 height 显式 scope 排除；§4.4 近零基线 N/A；§4.5 T20 有效窗与 ≥15% 门是**实测**门（本文件
//!   不测量、不签实测）。
//! - `rt-stack/spec/core-v1.md`：T06 scaffold/RT 纪律、fs ∈ {44.1,48,88.2,96} kHz。
//! - `crates/modal-design/src/{ingest,timebase}.rs`（T48/T49）：本文件消费其只读类型；T49
//!   `tspi-modal-timebase/v2` 的 `CommonD0` 是**唯一**去相位输入，`H_raw` 已按 D0 去相位一次。
//!
//! ## 本文件做什么（T50 DoD）
//!
//! 1. **拟合后冻结 P**：`PostRoleFit`（每输出角色的 P_post 复响应 + 延迟/极性/增益/带限/模态 PEQ
//!    契约）经 [`FittedPostBaseline::freeze`] **移动语义**冻结为 [`FrozenPostBaseline`]（无任何
//!    mutator；hash 确定；`stale_reasons` 对输入 hash/fs/tone/participants 变化报 stale）。
//! 2. **H_eff = H_raw · P**：[`EffectivePlant::build`] 逐 position、逐 participant 做复乘
//!    `H_eff[p][j](f) = H_raw[p][j](f) · P_j(f)`；`H_raw` 必须是 T49 已去相位（只扣 D0）且物理极性
//!    **恰好归一化一次**的复响应；本模块无第二次极性/去相位入口。
//! 3. **真实 5×5 damping 目标**：[`build_target_matrix`] 只在 design position 上构建
//!    `D_{j,i}(f)`（对角、非对角结构零），participants 显式 5（FL/FR/SL/SR/SUB1，dense→DAC
//!    0/1/2/3/7）；衰减目标 [`DecayTarget`] 支持 `Preserve` 或 `T*_60 = ratio × T60_baseline`
//!    （0.85/0.70/0.55 预设；T6 T20 ≥15% 门对应 ratio ≤0.85）。holdout/validation position 的
//!    数据**绝不参与**目标构建（只记录 reserved 标签）。`Reduced` 目标的相位是**声明的附加目标延迟**
//!    （`TargetSpec::target_delay_samples`，默认 0 = D0-removed 平面零多余相位），不把测量的
//!    bulk/excess 相位直接平滑带进目标（避免把延迟当成模态修正；T51/T53 必须按声明 D 处理，禁止事后平移）。
//! 4. **baseline tone 只应用一次**：[`ToneBinding`] 记录冻结基线 tone（flat/tilt10；20 Hz 0 →
//!    20 kHz −10 dB log-f，端点外固定）；[`ToneApplicationProof`] 强制 `(P_post, G, target) =
//!    (1, 0, 0)`——G 与 target 对 tone 的应用次数必须为 0；[`TargetSpec`] 没有"target 再乘 tone"
//!    的 API，且残留 curve/g 侧 tone 声明一律 [`TargetErrorKind::Tone`] 拒收。
//! 5. **高度全频 scope 分开**：[`FullRangeHeightScope`] 显式记录 HFL/HFR 为全频（band 0..fs/2、
//!    无 HP/LP、无人工 delay、`in_low_band_matrix=false`）；高度进入 participants、或高度带限/
//!    人工 delay 一律 `E_TGT_SCOPE`；target 只含 5 个 participant 的 cells。
//! 6. **stereo strict 单独求解验证**：`TargetProfile::StereoStrict2` 是独立 profile / 独立 hash
//!    domain；[`build_stereo_strict_target`] 只产出 FL/FR 2×2 目标（`modal_support_to_other_outputs
//!    = false`）；[`validate_solution`] 校验候选 G（T51 产物）尺寸/角色/输出集合，并给出预测残差；
//!    把 5×5 目标/G 混入 stereo strict → `E_TGT_STEREO`。
//!
//! ## 本文件不做什么（明确非目标）
//!
//! - 不测量、不采音、不打开 ALSA/UMIK/MOTU、不写文件、不启线程（纯计算；静态守卫测试保证）；
//! - 不设计/求 G（T51）、不正则（T52）、不做 holdout 判定（T53）、不写 artifact WAV（T54）、
//!   不做 PEQ/delay 拟合（T64/T65）、不编译/热切（T66）、不启动 daemon job（T60）；
//! - 不产生任何"实测改善"结论：`TargetStatus` 只表达**目标可行性/覆盖率**（PREDICTED_* /
//!   INFEASIBLE / INCOMPLETE / PRESERVE），`applied=false`、`post_apply_measurement=not_requested`；
//! - 不改旧 calibration v1/v1.1 契约、不改 `spec/`、不复制 legacy_s1 的错误（高度旧 HP1800/
//!   1.8k–10k LP/18 ms 属被否定生成器设计，本文件不允许人工 delay）。
//!
//! ## 关键不变量（测试与 static guards 覆盖）
//!
//! - **P 冻结后不可改**：`FrozenPostBaseline` 字段全私有、无 `&mut self` API；`freeze(self)` 消费
//!   `FittedPostBaseline`；hash 对输入逐位确定（`-0.0` 归一、无时间戳进入 hash）。
//! - **H_eff 只有一处复乘**：[compose_effective_response] 是唯一 `H_raw × P` 组合点；极性只能在
//!   [RawResponse::normalize_polarity] 发生一次；去相位只能在 T49 `de_phase_analysis` 发生一次。
//! - **tone 单次**：`ToneApplicationProof` 只接受 `(1,0,0)`；target 继承 tone 的 id/shape/hash，
//!   不重算 tone；`TargetSpec` 不接受 target 侧 tone。
//! - **participants 显式**：不从角色名/频响/HP 推断；4.1.2 必须恰为 5 个显式角色与 DAC 0/1/2/3/7。
//! - **holdout 隔离**：目标只聚合 `PositionRole::Design`；validation 只出现在 reserved 列表。
//! - **高度全频**：HFL/HFR 不得进入低带矩阵；不得带限/人工 delay。
//!
//! ## schema
//!
//! [TARGET_SCHEMA] `tspi-modal-target/v1`（目标矩阵 + 诊断 + 冻结基线/plant hash）；
//! [PLANT_SCHEMA] `tspi-modal-plant/v1`（H_eff）；[P_FREEZE_SCHEMA] `tspi-modal-post-freeze/v1`
//! （冻结 P）。三者都是设计侧/离线产物，不代表实测或已应用。

use crate::ingest::{sha256_hex, IngestSession, PositionRole};
use crate::timebase::{
    AbsoluteResponse, CommonD0, ComplexPoint, Polarity, ResolvedPolarity, SlotPhase,
};
use khaen_designer::modal_peq::{ChannelPeq, DesignMode, PeqLimits};

// ===========================================================================
// 常量
// ===========================================================================

/// 目标矩阵/诊断 schema（T50 产物；T51/T53/T54/T63/T66 消费）。
pub const TARGET_SCHEMA: &str = "tspi-modal-target/v1";
/// H_eff plant schema。
pub const PLANT_SCHEMA: &str = "tspi-modal-plant/v1";
/// 冻结 P 基线 schema。
pub const P_FREEZE_SCHEMA: &str = "tspi-modal-post-freeze/v1";

/// P 冻结 hash domain（canonical 文本前缀；与 target/plant hash 域分离）。
pub const POST_FREEZE_HASH_DOMAIN: &str = "tspi-modal-post-freeze/v1";
/// H_eff hash domain。
pub const H_EFF_HASH_DOMAIN: &str = "tspi-modal-h-eff/v1";
/// 5×5（多声道）目标 hash domain。
pub const TARGET_HASH_DOMAIN: &str = "tspi-modal-target-matrix/v1";
/// stereo strict 目标 hash domain（独立；禁止与 5×5 互相复用）。
pub const STEREO_TARGET_HASH_DOMAIN: &str = "tspi-modal-stereo-strict-target/v1";
/// 目标曲线绑定 hash domain（curve_id 只绑定冻结基线）。
pub const CURVE_BINDING_HASH_DOMAIN: &str = "tspi-modal-curve-binding/v1";

/// P1 目标布局 id（唯一 overlay 预设语义；实际 role→DAC 由 T57 解析，本文件只消费显式映射）。
pub const P1_LAYOUT_4_1_2: &str = "4.1.2";
/// P1 4.1.2 显式低带 MIMO participants（WBS/DEC 任务边界；不从角色名/HP1800 推断）。
pub const P1_MIMO_PARTICIPANT_IDS: [&str; 5] = ["FL", "FR", "SL", "SR", "SUB1"];
/// 上述 participants 的 dense→容器/DAC 槽位（overlay 4.1.2：0/1/2/3/7；ch6 空）。
pub const P1_MIMO_PARTICIPANT_DAC: [u32; 5] = [0, 1, 2, 3, 7];
/// 高度角色（corrected 基线：普通全频、无 HP/LP、无人工 18 ms、不进低带矩阵）。
pub const HEIGHT_ROLE_IDS: [&str; 2] = ["HFL", "HFR"];
/// stereo strict 输出集合（硬约束；其余输出不承接节目/模态支援）。
pub const STEREO_STRICT_OUTPUT_ROLE_IDS: [&str; 2] = ["FL", "FR"];
/// participants 硬上限（5.3.2 未来 M=8；本版只实现 4.1.2 的 5，8 → E_TGT_UNSUPPORTED）。
pub const MAX_PARTICIPANTS: usize = 8;
/// P1 采样率（core-v1 §3.2）。
pub const SAMPLE_RATES_HZ: [u32; 4] = [44_100, 48_000, 88_200, 96_000];
/// 低频模态校正带（M1：默认 20–300 Hz；300–400 Hz 渐退另报）。
pub const MODAL_CORRECTION_BAND_HZ: (f64, f64) = (20.0, 300.0);
/// 改善判据带（T6 §3.3：63–250 Hz）。
pub const IMPROVEMENT_BAND_HZ: (f64, f64) = (63.0, 250.0);
/// 过渡带（300–400 Hz；报告，不作本文件门）。
pub const TRANSITION_BAND_HZ: (f64, f64) = (300.0, 400.0);
/// T6 §3.3 必需 T20 判据频带（1/3 oct 中心）。
pub const T20_REQUIRED_BANDS_HZ: [f64; 2] = [63.0, 125.0];
/// 可选 T20 报告频带（40 Hz）。
pub const T20_OPTIONAL_BAND_HZ: f64 = 40.0;
/// T6 §4.5 T20 门：有效 T20 下降 ≥15% ⇔ 目标衰减比 ≤0.85。
pub const DECAY_GATE_MAX_RATIO: f64 = 0.85;
/// M1 §1 探索的衰减比预设（0.85/0.70/0.55）。
pub const DECAY_RATIO_PRESETS: [f64; 3] = [0.85, 0.70, 0.55];
/// 默认分数倍频程平滑（room steady-state 目标平滑；可配置，冻结后不得测后改）。
pub const DEFAULT_SMOOTHING_OCTAVE: f64 = 1.0 / 6.0;
/// 平滑窗最小（octave）。
pub const MIN_SMOOTHING_OCTAVE: f64 = 1.0 / 24.0;
/// 平滑窗最大（octave）。
pub const MAX_SMOOTHING_OCTAVE: f64 = 1.0 / 2.0;
/// 目标网格最少点数（复响应最低 2 点；目标构建要求更密，否则报覆盖率不足）。
pub const MIN_TARGET_POINTS: usize = 8;
/// 目标网格最多点数（与 T48 ingest MAX_FR_POINTS 对齐）。
pub const MAX_TARGET_POINTS: usize = 8192;
/// 判据网格分辨率（T6 §3.3：≥1/24 oct）。
pub const CRITERION_GRID_OCTAVE: f64 = 1.0 / 24.0;
/// 改善带最低网格覆盖率（T6 §3.3 ≥80%）。
pub const IMPROVEMENT_BAND_MIN_COVERAGE: f64 = 0.80;
/// 频点一致性相对容差。
pub const GRID_FREQ_REL_TOLERANCE: f64 = 1.0e-9;
/// P_post 角色延迟上限（ms；与 calibration 契约 200 ms 一致）。
pub const POST_DELAY_MAX_MS: f64 = 200.0;
/// P_post 角色 gain 允许下限（dB；超出即拒收，不静默 clamp）。
pub const POST_GAIN_MIN_DB: f64 = -24.0;
/// P_post 角色 gain 允许上限（dB）。
pub const POST_GAIN_MAX_DB: f64 = 12.0;
/// corrected 基线禁止人工 delay（旧 18 ms 生成器设计已被否定）——容差。
pub const ARTIFICIAL_DELAY_TOLERANCE_MS: f64 = 1.0e-9;
/// 保护带限频率下限（Hz）。
pub const PROTECTION_FREQ_MIN_HZ: f64 = 20.0;
/// 保护带限频率上限（Hz；不得越过 300 Hz 模态校正带上界）。
pub const PROTECTION_FREQ_MAX_HZ: f64 = 300.0;
/// SUB1 保护低通上限（Hz；M1：SUB ≤130 Hz）。
pub const SUB1_PROTECTION_LP_MAX_HZ: f64 = 130.0;
/// 单路径修正软帽（dB；M1 §2：默认 ≤ −6 dB，必要时显式放宽）。
pub const CORRECTION_SOFT_CAP_DB: f64 = 6.0;
/// 单路径修正硬上限（dB；超过即 INFEASIBLE，不放宽）。
pub const CORRECTION_HARD_CAP_DB: f64 = 12.0;
/// 近零幅度 floor（线性；−120 dB）：低于此值视为近零基线并告警。
pub const NEAR_ZERO_MAGNITUDE: f64 = 1.0e-6;
/// 复数除法下界（避免除 0）。
pub const COMPLEX_DIVISION_FLOOR: f64 = 1.0e-30;
/// 基线 T60 有效范围（s；物理合理性 sanity，不是门）。
pub const T60_MIN_S: f64 = 0.05;
/// 基线 T60 有效上限（s）。
pub const T60_MAX_S: f64 = 5.0;
/// tilt10 参考下频（Hz；0 dB）。
pub const TILT10_LO_HZ: f64 = 20.0;
/// tilt10 参考上频（Hz；−10 dB）。
pub const TILT10_HI_HZ: f64 = 20_000.0;
/// tilt10 总跨度（dB）。
pub const TILT10_SPAN_DB: f64 = 10.0;
/// tilt10 斜率（dB/decade；−10 dB per 3 decades）。
pub const TILT10_DB_PER_DECADE: f64 = TILT10_SPAN_DB / 3.0;
/// tone 幅度自洽比较容差（dB；仅用于可选一致性检查/测试，不作测量门）。
pub const TONE_MATCH_TOLERANCE_DB: f64 = 0.5;
/// 频率网格点匹配相对容差（复响应网格对齐）。
pub const GRID_MATCH_REL_TOLERANCE: f64 = 1.0e-7;
/// 最大容器/输出通道数（role→DAC 槽位上限）。
pub const MAX_CONTAINER_CHANNELS: u32 = 32;

// ===========================================================================
// 错误
// ===========================================================================

/// 目标构建错误种类（机器码见 [TargetErrorKind::code]；稳定字面值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetErrorKind {
    /// 采样率不在 P1 契约。
    SampleRate,
    /// 频率网格非法/不一致/覆盖不足。
    Grid,
    /// participants 显式集合非法（数量/重复/顺序/与 session 不一致）。
    Participants,
    /// 角色/输出槽位/PEQ 契约非法。
    Role,
    /// 高度全频 scope 违规（高度进矩阵/带限/人工 delay）。
    Scope,
    /// tone 声明非法或重复应用（baseline tone 双叠）。
    Tone,
    /// curve 绑定非法（应用点/次数/残留 curve）。
    Curve,
    /// 衰减目标非法（比例/T60 频带缺失/越界）。
    Decay,
    /// 极性未知或重复归一化。
    Polarity,
    /// 冻结 P 基线非法或 stale。
    Baseline,
    /// position 标签/角色/权重/集合非法。
    Position,
    /// 目标矩阵/求解矩阵结构非法（尺寸/角色/输出集合）。
    Matrix,
    /// stereo strict 约束违规（复用 5×5 / 输出集合越界）。
    Stereo,
    /// 非有限值/越界/数值下溢。
    Range,
    /// 缺失必需输入。
    Missing,
    /// 未支持的 profile/布局/规模（如 5.3.2 M=8 本版未实现）。
    Unsupported,
}

impl TargetErrorKind {
    /// 全部错误种类（报告/负向对照用；顺序稳定）。
    pub const ALL: [TargetErrorKind; 16] = [
        TargetErrorKind::SampleRate,
        TargetErrorKind::Grid,
        TargetErrorKind::Participants,
        TargetErrorKind::Role,
        TargetErrorKind::Scope,
        TargetErrorKind::Tone,
        TargetErrorKind::Curve,
        TargetErrorKind::Decay,
        TargetErrorKind::Polarity,
        TargetErrorKind::Baseline,
        TargetErrorKind::Position,
        TargetErrorKind::Matrix,
        TargetErrorKind::Stereo,
        TargetErrorKind::Range,
        TargetErrorKind::Missing,
        TargetErrorKind::Unsupported,
    ];

    /// 稳定机器码（E_TGT_*）。
    pub const fn code(self) -> &'static str {
        match self {
            TargetErrorKind::SampleRate => "E_TGT_SAMPLE_RATE",
            TargetErrorKind::Grid => "E_TGT_GRID",
            TargetErrorKind::Participants => "E_TGT_PARTICIPANTS",
            TargetErrorKind::Role => "E_TGT_ROLE",
            TargetErrorKind::Scope => "E_TGT_SCOPE",
            TargetErrorKind::Tone => "E_TGT_TONE",
            TargetErrorKind::Curve => "E_TGT_CURVE",
            TargetErrorKind::Decay => "E_TGT_DECAY",
            TargetErrorKind::Polarity => "E_TGT_POLARITY",
            TargetErrorKind::Baseline => "E_TGT_BASELINE",
            TargetErrorKind::Position => "E_TGT_POSITION",
            TargetErrorKind::Matrix => "E_TGT_MATRIX",
            TargetErrorKind::Stereo => "E_TGT_STEREO",
            TargetErrorKind::Range => "E_TGT_RANGE",
            TargetErrorKind::Missing => "E_TGT_MISSING",
            TargetErrorKind::Unsupported => "E_TGT_UNSUPPORTED",
        }
    }
}

/// 目标构建错误（kind + 字段路径 + 说明；Display 含机器码）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetError {
    kind: TargetErrorKind,
    field: String,
    detail: String,
}

impl TargetError {
    /// 构造（field 为 a.b[c] 形式路径；detail 为人读说明）。
    pub fn new(kind: TargetErrorKind, field: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            kind,
            field: field.into(),
            detail: detail.into(),
        }
    }

    /// 错误种类。
    pub fn kind(&self) -> TargetErrorKind {
        self.kind
    }

    /// 稳定机器码。
    pub fn code(&self) -> &'static str {
        self.kind.code()
    }

    /// 字段路径。
    pub fn field(&self) -> &str {
        &self.field
    }

    /// 说明。
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl std::fmt::Display for TargetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}: {} ({})",
            self.code(),
            self.detail,
            self.field
        )
    }
}

impl std::error::Error for TargetError {}

fn err(kind: TargetErrorKind, field: impl Into<String>, detail: impl Into<String>) -> TargetError {
    TargetError::new(kind, field, detail)
}

// ===========================================================================
// 非致命诊断
// ===========================================================================

/// 非致命诊断（不改变产物有效性；报告必须原样带出）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetWarning {
    /// 稳定告警码（W_TGT_*）。
    pub code: &'static str,
    /// 人读说明。
    pub message: String,
}

impl TargetWarning {
    /// 基线在某 (p,j,f) 近零（无法可靠反演修正）。
    pub const NEAR_ZERO_BASELINE: &'static str = "W_TGT_NEAR_ZERO_BASELINE";
    /// 平滑窗宽于改善带/网格，目标可信度下降。
    pub const WIDE_SMOOTHING: &'static str = "W_TGT_WIDE_SMOOTHING";
    /// design position 数少于 3（T6 正式 6+3 由 T67 保证）。
    pub const FEW_DESIGN_POSITIONS: &'static str = "W_TGT_FEW_DESIGN_POSITIONS";
    /// 存在 validation/holdout position（本文件只记录 reserved，不使用）。
    pub const HOLDOUT_RESERVED: &'static str = "W_TGT_HOLDOUT_RESERVED";
    /// 衰减比低于预设探索下限（0.55），可行性风险高。
    pub const AGGRESSIVE_DECAY: &'static str = "W_TGT_AGGRESSIVE_DECAY";
    /// 目标按位置不变（Reduced 模式）；per-position 差异进报告。
    pub const POSITION_INVARIANT_TARGET: &'static str = "W_TGT_POSITION_INVARIANT";
    /// 保护带限覆盖了部分模态校正带（目标只应落在保护通带内）。
    pub const PROTECTION_OVERLAP: &'static str = "W_TGT_PROTECTION_OVERLAP";
    /// 过渡带不在网格/覆盖不足（仅报告）。
    pub const TRANSITION_BAND_GAP: &'static str = "W_TGT_TRANSITION_BAND_GAP";
    /// 高度全频 scope 被显式记录（height 不参与低带矩阵）。
    pub const HEIGHT_SCOPE_SEPARATED: &'static str = "W_TGT_HEIGHT_SCOPE_SEPARATED";
    /// stereo strict profile 独立构建（输出集合 FL/FR 硬约束）。
    pub const STEREO_STRICT_PROFILE: &'static str = "W_TGT_STEREO_STRICT_PROFILE";
    /// 计算量超预期但未失败（保留）。
    pub const LARGE_GRID: &'static str = "W_TGT_LARGE_GRID";
}

fn push_warning(warnings: &mut Vec<TargetWarning>, code: &'static str, message: String) {
    if warnings
        .iter()
        .any(|existing| existing.code == code && existing.message == message)
    {
        return;
    }
    warnings.push(TargetWarning { code, message });
}

// ===========================================================================
// 基础校验/数值辅助
// ===========================================================================

fn finite(value: f64, field: &str) -> Result<f64, TargetError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(err(
            TargetErrorKind::Range,
            field,
            format!("value must be finite, got {value}"),
        ))
    }
}

fn finite_in_range(value: f64, min: f64, max: f64, field: &str) -> Result<f64, TargetError> {
    let value = finite(value, field)?;
    if value < min || value > max {
        return Err(err(
            TargetErrorKind::Range,
            field,
            format!("value {value} outside [{min}, {max}]"),
        ));
    }
    Ok(value)
}

fn sample_rate_ok(sample_rate_hz: u32) -> bool {
    SAMPLE_RATES_HZ.contains(&sample_rate_hz)
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn is_id_token(text: &str, max_len: usize) -> bool {
    let bytes = text.as_bytes();
    if bytes.is_empty() || bytes.len() > max_len {
        return false;
    }
    let first = bytes[0];
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    bytes
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn is_role_id(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 {
        return false;
    }
    bytes[0].is_ascii_uppercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

/// 确定性 f64 文本（-0.0 归零；17 位有效数字科学计数；禁止 NaN/Inf）。
fn fmt_f64(value: f64) -> String {
    let normalized = if value == 0.0 { 0.0 } else { value };
    debug_assert!(normalized.is_finite());
    format!("{normalized:.17e}")
}

fn complex_multiply(left: (f64, f64), right: (f64, f64)) -> (f64, f64) {
    (
        left.0 * right.0 - left.1 * right.1,
        left.0 * right.1 + left.1 * right.0,
    )
}

fn magnitude_of(point: &ComplexPoint) -> f64 {
    point.re.hypot(point.im)
}

fn magnitude_db(point: &ComplexPoint) -> f64 {
    let magnitude = magnitude_of(point);
    20.0 * magnitude.max(NEAR_ZERO_MAGNITUDE).log10()
}

fn power(point: &ComplexPoint) -> f64 {
    point.re * point.re + point.im * point.im
}

fn ratio_db(numerator: f64, denominator: f64) -> f64 {
    20.0 * (numerator.max(NEAR_ZERO_MAGNITUDE) / denominator.max(NEAR_ZERO_MAGNITUDE)).log10()
}

fn approx_equal(left: f64, right: f64, tolerance: f64) -> bool {
    (left - right).abs() <= tolerance
}

fn grid_freq_equal(left: f64, right: f64) -> bool {
    let tolerance = GRID_FREQ_REL_TOLERANCE * left.abs().max(right.abs()).max(1.0);
    (left - right).abs() <= tolerance
}

/// 校验频率网格（严格升序、有限、>0、≤ fs/2、点数在范围内）。
fn validate_frequency_grid(
    grid_hz: &[f64],
    sample_rate_hz: u32,
    field: &str,
) -> Result<(), TargetError> {
    if grid_hz.len() < MIN_TARGET_POINTS {
        return Err(err(
            TargetErrorKind::Grid,
            field,
            format!(
                "grid has {} points; at least {MIN_TARGET_POINTS} required for room target construction",
                grid_hz.len()
            ),
        ));
    }
    if grid_hz.len() > MAX_TARGET_POINTS {
        return Err(err(
            TargetErrorKind::Grid,
            field,
            format!(
                "grid has {} points; at most {MAX_TARGET_POINTS} supported",
                grid_hz.len()
            ),
        ));
    }
    let nyquist = sample_rate_hz as f64 / 2.0;
    let mut previous = f64::NEG_INFINITY;
    for (index, freq) in grid_hz.iter().copied().enumerate() {
        let freq = finite(freq, &format!("{field}[{index}]"))?;
        if freq <= 0.0 {
            return Err(err(
                TargetErrorKind::Grid,
                format!("{field}[{index}]"),
                format!("frequency {freq} must be > 0"),
            ));
        }
        if freq > nyquist {
            return Err(err(
                TargetErrorKind::Grid,
                format!("{field}[{index}]"),
                format!("frequency {freq} exceeds Nyquist {nyquist} Hz"),
            ));
        }
        if freq <= previous {
            return Err(err(
                TargetErrorKind::Grid,
                format!("{field}[{index}]"),
                format!("frequencies must be strictly increasing ({previous} then {freq})"),
            ));
        }
        previous = freq;
    }
    Ok(())
}

fn grid_matches(left: &[f64], right: &[f64]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right.iter())
            .all(|(a, b)| grid_freq_equal(*a, *b))
}

/// 判据网格覆盖率：对 [lo, hi] 内按 1/24 oct 期望点，检查网格是否有 ±半格内的点。
fn band_coverage(freq_hz: &[f64], lo: f64, hi: f64) -> f64 {
    if !(lo > 0.0 && hi > lo) || freq_hz.is_empty() {
        return 0.0;
    }
    let step = CRITERION_GRID_OCTAVE;
    let half_step_ratio = 2.0f64.powf(step / 2.0);
    let mut expected = 0usize;
    let mut covered = 0usize;
    let mut freq = lo;
    while freq <= hi * (1.0 + 1.0e-12) {
        expected += 1;
        let lower = freq / half_step_ratio;
        let upper = freq * half_step_ratio;
        if freq_hz
            .iter()
            .any(|point| *point >= lower && *point <= upper)
        {
            covered += 1;
        }
        freq *= 2.0f64.powf(step);
    }
    if expected == 0 {
        return 0.0;
    }
    covered as f64 / expected as f64
}

/// 复响应网格校验：长度一致、逐点频点一致、逐点有限且幅度在 sanity 范围。
fn validate_complex_grid(
    points: &[ComplexPoint],
    grid_hz: &[f64],
    field: &str,
) -> Result<(), TargetError> {
    if points.len() != grid_hz.len() {
        return Err(err(
            TargetErrorKind::Grid,
            field,
            format!(
                "response has {} points but grid has {}",
                points.len(),
                grid_hz.len()
            ),
        ));
    }
    for (index, (point, expected)) in points.iter().zip(grid_hz.iter()).enumerate() {
        let index_field = format!("{field}[{index}]");
        let re = finite(point.re, &format!("{index_field}.re"))?;
        let im = finite(point.im, &format!("{index_field}.im"))?;
        if !grid_freq_equal(point.freq_hz, *expected) {
            return Err(err(
                TargetErrorKind::Grid,
                index_field,
                format!(
                    "response frequency {} does not match grid frequency {expected}",
                    point.freq_hz
                ),
            ));
        }
        let magnitude = re.hypot(im);
        if magnitude > 1.0e6 {
            return Err(err(
                TargetErrorKind::Range,
                format!("{index_field}.magnitude"),
                format!("magnitude {magnitude} exceeds +120 dB sanity bound"),
            ));
        }
    }
    Ok(())
}

/// 分数倍频程复数平滑：功率加权幅度（RMS）+ 功率加权圆均值相位。
///
/// 语义（冻结）：窗 [f·2^(-h/2), f·2^(h/2)]（h = smoothing_octave）；对窗内全部网格点按功率
/// |H|^2 加权：mag = sqrt(Σ|H|²/n)（RMS，保稳态能量、抑制单点 notches 的过冲）；
/// phase = atan2(Σ|H|² sin φ, Σ|H|² cos φ)（功率加权圆均值）。
///
/// 该平滑只在 target 构建内部使用（design positions），不修改测量数据；holdout 永不参与。
fn smooth_complex_response(points: &[ComplexPoint], smoothing_octave: f64) -> Vec<ComplexPoint> {
    let half_ratio = 2.0f64.powf(smoothing_octave / 2.0);
    let mut smoothed = Vec::with_capacity(points.len());
    for center in points {
        let lower = center.freq_hz / half_ratio;
        let upper = center.freq_hz * half_ratio;
        let mut count = 0usize;
        let mut power_sum = 0.0f64;
        let mut weighted_re = 0.0f64;
        let mut weighted_im = 0.0f64;
        for candidate in points {
            if candidate.freq_hz < lower || candidate.freq_hz > upper {
                continue;
            }
            let candidate_power = power(candidate);
            count += 1;
            power_sum += candidate_power;
            weighted_re += candidate_power * candidate.re;
            weighted_im += candidate_power * candidate.im;
        }
        if count == 0 || !(power_sum > COMPLEX_DIVISION_FLOOR) {
            smoothed.push(*center);
            continue;
        }
        let magnitude = (power_sum / count as f64).sqrt();
        let phase_norm = weighted_im.hypot(weighted_re);
        if !(phase_norm > COMPLEX_DIVISION_FLOOR) {
            smoothed.push(*center);
            continue;
        }
        smoothed.push(ComplexPoint {
            freq_hz: center.freq_hz,
            re: magnitude * (weighted_re / phase_norm),
            im: magnitude * (weighted_im / phase_norm),
        });
    }
    smoothed
}

/// 跨 position 的功率加权聚合（只用于 design positions；权重 >0 已校验）。
fn aggregate_positions(responses: &[&[ComplexPoint]], weights: &[f64]) -> Vec<ComplexPoint> {
    let first = match responses.first() {
        Some(first) => *first,
        None => return Vec::new(),
    };
    let total_weight: f64 = weights.iter().sum();
    let mut aggregated = Vec::with_capacity(first.len());
    for index in 0..first.len() {
        let mut power_sum = 0.0f64;
        let mut weighted_re = 0.0f64;
        let mut weighted_im = 0.0f64;
        for (response, weight) in responses.iter().zip(weights.iter()) {
            let point = &response[index];
            let point_power = power(point);
            power_sum += weight * point_power;
            weighted_re += weight * point_power * point.re;
            weighted_im += weight * point_power * point.im;
        }
        let magnitude = (power_sum / total_weight.max(1.0e-30)).sqrt();
        let phase_norm = weighted_im.hypot(weighted_re);
        if !(phase_norm > COMPLEX_DIVISION_FLOOR) {
            aggregated.push(ComplexPoint {
                freq_hz: first[index].freq_hz,
                re: 0.0,
                im: 0.0,
            });
            continue;
        }
        aggregated.push(ComplexPoint {
            freq_hz: first[index].freq_hz,
            re: magnitude * (weighted_re / phase_norm),
            im: magnitude * (weighted_im / phase_norm),
        });
    }
    aggregated
}

/// 以 log-f 线性插值读取复响应（目标查询；网格外 hold 端点）。
fn interpolate_complex(points: &[ComplexPoint], freq_hz: f64) -> (f64, f64) {
    if points.is_empty() {
        return (0.0, 0.0);
    }
    if freq_hz <= points[0].freq_hz {
        return (points[0].re, points[0].im);
    }
    let last = &points[points.len() - 1];
    if freq_hz >= last.freq_hz {
        return (last.re, last.im);
    }
    let mut lo = 0usize;
    let mut hi = points.len() - 1;
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if points[mid].freq_hz <= freq_hz {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let f0 = points[lo].freq_hz;
    let f1 = points[hi].freq_hz;
    let t = (freq_hz / f0).ln() / (f1 / f0).ln();
    (
        points[lo].re + (points[hi].re - points[lo].re) * t,
        points[lo].im + (points[hi].im - points[lo].im) * t,
    )
}

// ===========================================================================
// baseline tone（只应用一次：curve 绑定冻结基线，G/target 不再乘）
// ===========================================================================

/// 冻结基线的 broad tone 形状（M1：flat 0 dB；tilt10 20 Hz 0 → 20 kHz −10 dB log-f，端点外固定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToneShape {
    /// 平直（0 dB）。
    Flat,
    /// tilt10：−10·log10(f/20)/3 dB（20 Hz–20 kHz，端点外 hold）。
    Tilt10,
}

impl ToneShape {
    /// 解析字面值（未知 → None，调用方必须拒收，不猜）。
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "flat" => Some(ToneShape::Flat),
            "tilt10" => Some(ToneShape::Tilt10),
            _ => None,
        }
    }

    /// 稳定字面值。
    pub const fn as_str(self) -> &'static str {
        match self {
            ToneShape::Flat => "flat",
            ToneShape::Tilt10 => "tilt10",
        }
    }

    /// tone 幅度包络（dB）；flat 恒 0，tilt10 在 [20 Hz, 20 kHz] 内 log-f 线性、端点外固定。
    pub fn mag_db(self, freq_hz: f64) -> f64 {
        match self {
            ToneShape::Flat => 0.0,
            ToneShape::Tilt10 => {
                let clamped = freq_hz.clamp(TILT10_LO_HZ, TILT10_HI_HZ);
                -TILT10_SPAN_DB * (clamped / TILT10_LO_HZ).log10() / 3.0
            }
        }
    }

    /// tone 斜率（dB/decade；tilt10 = −10/3，flat = 0）。
    pub const fn slope_db_per_decade(self) -> f64 {
        match self {
            ToneShape::Flat => 0.0,
            ToneShape::Tilt10 => -TILT10_DB_PER_DECADE,
        }
    }
}

/// 冻结基线 tone 绑定（curve_id + 形状 + reference_gain + 可选目标 hash）。
///
/// 该绑定只描述已烘焙进 P_post 的 tone：tone 在 P 基线内应用一次，G 只承担 modal residual，
/// target 继承本绑定而不再应用 tone（见 [ToneApplicationProof]）。
#[derive(Debug, Clone, PartialEq)]
pub struct ToneBinding {
    curve_id: String,
    shape: ToneShape,
    reference_gain_db: f64,
    target_sha256: Option<String>,
}

impl ToneBinding {
    /// 构造（curve_id 为稳定 id token；reference_gain_db 默认 0；target hash 可选）。
    pub fn new(curve_id: impl Into<String>, shape: ToneShape) -> Result<Self, TargetError> {
        let curve_id = curve_id.into();
        if !is_id_token(&curve_id, 64) {
            return Err(err(
                TargetErrorKind::Tone,
                "tone.curve_id",
                format!("curve_id {curve_id:?} must match ^[A-Za-z0-9][A-Za-z0-9._-]{{0,63}}$"),
            ));
        }
        Ok(Self {
            curve_id,
            shape,
            reference_gain_db: 0.0,
            target_sha256: None,
        })
    }

    /// 内置 flat（curve_id = flat_baseline）。
    pub fn flat() -> Self {
        Self {
            curve_id: "flat_baseline".to_string(),
            shape: ToneShape::Flat,
            reference_gain_db: 0.0,
            target_sha256: None,
        }
    }

    /// 内置 tilt10（curve_id = tilt10_baseline）。
    pub fn tilt10() -> Self {
        Self {
            curve_id: "tilt10_baseline".to_string(),
            shape: ToneShape::Tilt10,
            reference_gain_db: 0.0,
            target_sha256: None,
        }
    }

    /// 设置独立 reference_gain_db（归一锚点；不得偷换）。
    pub fn with_reference_gain_db(mut self, reference_gain_db: f64) -> Result<Self, TargetError> {
        self.reference_gain_db =
            finite_in_range(reference_gain_db, -24.0, 24.0, "tone.reference_gain_db")?;
        Ok(self)
    }

    /// 绑定目标曲线内容 hash（T11 产物；64 lowercase hex）。
    pub fn with_target_sha256(
        mut self,
        target_sha256: impl Into<String>,
    ) -> Result<Self, TargetError> {
        let target_sha256 = target_sha256.into();
        if !is_sha256_hex(&target_sha256) {
            return Err(err(
                TargetErrorKind::Tone,
                "tone.target_sha256",
                format!("target_sha256 must be 64 lowercase hex, got {target_sha256:?}"),
            ));
        }
        self.target_sha256 = Some(target_sha256);
        Ok(self)
    }

    /// curve id。
    pub fn curve_id(&self) -> &str {
        &self.curve_id
    }

    /// tone 形状。
    pub fn shape(&self) -> ToneShape {
        self.shape
    }

    /// 独立 reference gain（dB）。
    pub fn reference_gain_db(&self) -> f64 {
        self.reference_gain_db
    }

    /// 目标曲线内容 hash（可选）。
    pub fn target_sha256(&self) -> Option<&str> {
        self.target_sha256.as_deref()
    }

    /// tone 幅度包络（dB，含 reference_gain）。
    pub fn mag_db(&self, freq_hz: f64) -> f64 {
        self.shape.mag_db(freq_hz) + self.reference_gain_db
    }

    /// 两个 tone 绑定是否为同一冻结定义（id/shape/reference gain/target hash 全等）。
    pub fn same_definition(&self, other: &ToneBinding) -> bool {
        self.curve_id == other.curve_id
            && self.shape == other.shape
            && self.reference_gain_db == other.reference_gain_db
            && self.target_sha256 == other.target_sha256
    }

    /// canonical 文本（确定性；供 hash/证据）。
    pub fn canonical(&self) -> String {
        let target = self.target_sha256.as_deref().unwrap_or("none");
        format!(
            "tone curve_id={} shape={} reference_gain_db={} target_sha256={}",
            self.curve_id,
            self.shape.as_str(),
            fmt_f64(self.reference_gain_db),
            target
        )
    }

    /// 绑定 hash。
    pub fn binding_hash(&self) -> String {
        sha256_hex(self.canonical().as_bytes())
    }
}

/// tone 应用次数声明（baseline tone 只应用一次；G 与 target 不得再应用）。
///
/// 规则（T13 GResidualBinding / M1 §1 的同构本地实现）：P_post 恰好 1 次；low-band residual G 与
/// target 必须 0 次。declared 允许构造任意声明（用于解析外部 manifest 后显式拒收），validate 才判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToneApplicationProof {
    post_baseline: u32,
    low_band_residual: u32,
    target: u32,
}

impl ToneApplicationProof {
    /// 合法声明：P_post 一次、其余零次。
    pub fn baseline_only() -> Self {
        Self {
            post_baseline: 1,
            low_band_residual: 0,
            target: 0,
        }
    }

    /// 原样声明（不校验；来自 manifest 的声明必须随后 validate）。
    pub fn declared(post_baseline: u32, low_band_residual: u32, target: u32) -> Self {
        Self {
            post_baseline,
            low_band_residual,
            target,
        }
    }

    /// 校验"baseline tone 只应用一次"。
    pub fn validate(&self) -> Result<(), TargetError> {
        if self.post_baseline != 1 {
            return Err(err(
                TargetErrorKind::Tone,
                "tone.applications.post_baseline",
                format!(
                    "baseline tone must be applied exactly once in P_post; declared {} (double-tone or missing tone)",
                    self.post_baseline
                ),
            ));
        }
        if self.low_band_residual != 0 {
            return Err(err(
                TargetErrorKind::Tone,
                "tone.applications.low_band_residual",
                format!(
                    "G must not re-apply the baseline tone (modal residual only); declared {}",
                    self.low_band_residual
                ),
            ));
        }
        if self.target != 0 {
            return Err(err(
                TargetErrorKind::Tone,
                "tone.applications.target",
                format!(
                    "target matrix must inherit the baseline tone, not apply it again; declared {}",
                    self.target
                ),
            ));
        }
        Ok(())
    }

    /// P_post 应用次数。
    pub fn post_baseline(&self) -> u32 {
        self.post_baseline
    }

    /// G 残差应用次数。
    pub fn low_band_residual(&self) -> u32 {
        self.low_band_residual
    }

    /// target 应用次数。
    pub fn target(&self) -> u32 {
        self.target
    }

    /// canonical 文本。
    pub fn canonical(&self) -> String {
        format!(
            "tone_applications post_baseline={} low_band_residual={} target={}",
            self.post_baseline, self.low_band_residual, self.target
        )
    }
}

/// curve 应用点（冻结基线唯一合法点 = P_post 的 TargetFir 基线阶段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveApplySite {
    /// 冻结基线 P_post（唯一合法应用点）。
    PostBaselineP,
    /// 低带残差 G（禁止：会与 baseline tone 双叠）。
    LowBandResidualG,
    /// 目标矩阵（禁止：target 只继承，不再应用）。
    TargetMatrix,
}

impl CurveApplySite {
    /// 解析字面值。
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "post_baseline_p" => Some(CurveApplySite::PostBaselineP),
            "low_band_residual_g" => Some(CurveApplySite::LowBandResidualG),
            "target_matrix" => Some(CurveApplySite::TargetMatrix),
            _ => None,
        }
    }

    /// 稳定字面值。
    pub const fn as_str(self) -> &'static str {
        match self {
            CurveApplySite::PostBaselineP => "post_baseline_p",
            CurveApplySite::LowBandResidualG => "low_band_residual_g",
            CurveApplySite::TargetMatrix => "target_matrix",
        }
    }
}

/// 曲线绑定声明（curve_id 只在冻结基线应用一次；G/target 的残留绑定必须拒收）。
#[derive(Debug, Clone, PartialEq)]
pub struct CurveBinding {
    curve_id: String,
    site: CurveApplySite,
    applications: u32,
    target_sha256: Option<String>,
}

impl CurveBinding {
    /// 构造（基础字段校验；应用点/次数语义由 validate_single_application 判定）。
    pub fn new(
        curve_id: impl Into<String>,
        site: CurveApplySite,
        applications: u32,
    ) -> Result<Self, TargetError> {
        let curve_id = curve_id.into();
        if !is_id_token(&curve_id, 64) {
            return Err(err(
                TargetErrorKind::Curve,
                "curve.curve_id",
                format!("curve_id {curve_id:?} must match ^[A-Za-z0-9][A-Za-z0-9._-]{{0,63}}$"),
            ));
        }
        if applications == 0 {
            return Err(err(
                TargetErrorKind::Curve,
                "curve.applications",
                "curve applications must be >= 1 (a declared binding with 0 applications is meaningless)",
            ));
        }
        Ok(Self {
            curve_id,
            site,
            applications,
            target_sha256: None,
        })
    }

    /// 合法基线绑定（P_post、恰好 1 次）。
    pub fn baseline_only(curve_id: impl Into<String>) -> Result<Self, TargetError> {
        Self::new(curve_id, CurveApplySite::PostBaselineP, 1)
    }

    /// 绑定目标曲线 hash。
    pub fn with_target_sha256(
        mut self,
        target_sha256: impl Into<String>,
    ) -> Result<Self, TargetError> {
        let target_sha256 = target_sha256.into();
        if !is_sha256_hex(&target_sha256) {
            return Err(err(
                TargetErrorKind::Curve,
                "curve.target_sha256",
                format!("target_sha256 must be 64 lowercase hex, got {target_sha256:?}"),
            ));
        }
        self.target_sha256 = Some(target_sha256);
        Ok(self)
    }

    /// curve id。
    pub fn curve_id(&self) -> &str {
        &self.curve_id
    }

    /// 应用点。
    pub fn site(&self) -> CurveApplySite {
        self.site
    }

    /// 应用次数。
    pub fn applications(&self) -> u32 {
        self.applications
    }

    /// 目标曲线 hash。
    pub fn target_sha256(&self) -> Option<&str> {
        self.target_sha256.as_deref()
    }

    /// 是否在基线 P 上应用。
    pub fn is_baseline(&self) -> bool {
        self.site == CurveApplySite::PostBaselineP
    }

    /// 校验"curve 只在冻结基线应用一次"（G/target 侧残留 → E_TGT_TONE/E_TGT_CURVE）。
    pub fn validate_single_application(&self) -> Result<(), TargetError> {
        if self.site != CurveApplySite::PostBaselineP {
            return Err(err(
                TargetErrorKind::Tone,
                "curve.site",
                format!(
                    "curve must be applied only at the frozen P_post baseline; declared site {:?} would double-stack the baseline tone",
                    self.site.as_str()
                ),
            ));
        }
        if self.applications != 1 {
            return Err(err(
                TargetErrorKind::Tone,
                "curve.applications",
                format!(
                    "curve must be applied exactly once on the frozen baseline; declared {}",
                    self.applications
                ),
            ));
        }
        Ok(())
    }

    /// canonical 文本。
    pub fn canonical(&self) -> String {
        let target = self.target_sha256.as_deref().unwrap_or("none");
        format!(
            "curve curve_id={} site={} applications={} target_sha256={}",
            self.curve_id,
            self.site.as_str(),
            self.applications,
            target
        )
    }

    /// 绑定 hash。
    pub fn binding_hash(&self) -> String {
        sha256_hex(format!("{CURVE_BINDING_HASH_DOMAIN}\n{}\n", self.canonical()).as_bytes())
    }
}

// ===========================================================================
// participants（显式 5；不从角色名/频响推断）
// ===========================================================================

/// 一个低带 MIMO participant（dense 索引 ↔ 角色 id ↔ 容器/DAC 槽位）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModalParticipant {
    /// dense 索引（0..M-1；G 的行/列）。
    pub dense_index: u16,
    /// 角色 id。
    pub role_id: String,
    /// 容器/DAC 槽位（该角色首槽位）。
    pub out_index: u32,
}

/// 校验后的显式 participants 集合（任务边界数据；4.1.2 恰 5 个）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModalParticipants {
    layout_id: String,
    participants: Vec<ModalParticipant>,
}

impl ModalParticipants {
    /// P1 4.1.2 显式 scope：FL/FR/SL/SR/SUB1 → DAC 0/1/2/3/7。
    pub fn p1_4_1_2() -> Self {
        let participants = P1_MIMO_PARTICIPANT_IDS
            .iter()
            .enumerate()
            .map(|(dense_index, role_id)| ModalParticipant {
                dense_index: dense_index as u16,
                role_id: role_id.to_string(),
                out_index: P1_MIMO_PARTICIPANT_DAC[dense_index],
            })
            .collect();
        Self {
            layout_id: P1_LAYOUT_4_1_2.to_string(),
            participants,
        }
    }

    /// stereo strict 显式 scope：FL/FR → DAC 0/1（独立受限 profile；不支援其它输出）。
    pub fn stereo_strict() -> Self {
        let participants = STEREO_STRICT_OUTPUT_ROLE_IDS
            .iter()
            .enumerate()
            .map(|(dense_index, role_id)| ModalParticipant {
                dense_index: dense_index as u16,
                role_id: role_id.to_string(),
                out_index: dense_index as u32,
            })
            .collect();
        Self {
            layout_id: P1_LAYOUT_4_1_2.to_string(),
            participants,
        }
    }

    /// 构造显式 participants（有序；角色 id 唯一、DAC 槽位唯一；高度不得进入低带矩阵）。
    pub fn new(
        layout_id: impl Into<String>,
        entries: Vec<(String, u32)>,
    ) -> Result<Self, TargetError> {
        let layout_id = layout_id.into();
        if !is_id_token(&layout_id, 64) {
            return Err(err(
                TargetErrorKind::Participants,
                "participants.layout_id",
                format!("layout_id {layout_id:?} must be a stable id token"),
            ));
        }
        if entries.is_empty() {
            return Err(err(
                TargetErrorKind::Missing,
                "participants",
                "explicit participants list must not be empty (scope comes from the task boundary, not role names)",
            ));
        }
        if entries.len() > MAX_PARTICIPANTS {
            return Err(err(
                TargetErrorKind::Participants,
                "participants",
                format!(
                    "{} participants exceed MAX_PARTICIPANTS={MAX_PARTICIPANTS}",
                    entries.len()
                ),
            ));
        }
        let mut participants: Vec<ModalParticipant> = Vec::with_capacity(entries.len());
        for (dense_index, (role_id, out_index)) in entries.iter().enumerate() {
            if !is_role_id(role_id) {
                return Err(err(
                    TargetErrorKind::Participants,
                    format!("participants[{dense_index}].role_id"),
                    format!("role id {role_id:?} must match ^[A-Z][A-Z0-9]*$"),
                ));
            }
            if HEIGHT_ROLE_IDS.contains(&role_id.as_str()) {
                return Err(err(
                    TargetErrorKind::Scope,
                    format!("participants[{dense_index}].role_id"),
                    format!(
                        "height role {role_id} must stay full-range and outside the low-band MIMO matrix (E_TGT_SCOPE)"
                    ),
                ));
            }
            if participants
                .iter()
                .any(|existing| existing.role_id == *role_id)
            {
                return Err(err(
                    TargetErrorKind::Participants,
                    format!("participants[{dense_index}].role_id"),
                    format!("duplicate participant {role_id}"),
                ));
            }
            if *out_index >= MAX_CONTAINER_CHANNELS {
                return Err(err(
                    TargetErrorKind::Range,
                    format!("participants[{dense_index}].out_index"),
                    format!(
                        "out_index {out_index} exceeds MAX_CONTAINER_CHANNELS={MAX_CONTAINER_CHANNELS}"
                    ),
                ));
            }
            if participants
                .iter()
                .any(|existing| existing.out_index == *out_index)
            {
                return Err(err(
                    TargetErrorKind::Participants,
                    format!("participants[{dense_index}].out_index"),
                    format!("duplicate out_index {out_index} (one participant per DAC slot)"),
                ));
            }
            participants.push(ModalParticipant {
                dense_index: dense_index as u16,
                role_id: role_id.clone(),
                out_index: *out_index,
            });
        }
        Ok(Self {
            layout_id,
            participants,
        })
    }

    /// 从 T48 ingest session 的显式声明构造（要求 4.1.2 恰 5 个、DAC 0/1/2/3/7；高度不在 participants）。
    pub fn from_session(session: &IngestSession) -> Result<Self, TargetError> {
        if session.layout != P1_LAYOUT_4_1_2 {
            return Err(err(
                TargetErrorKind::Unsupported,
                "session.layout",
                format!(
                    "session layout {:?} is not the P1 4.1.2 target layout {P1_LAYOUT_4_1_2:?} (5.3.2/M=8 is NOT_VERIFIED in this version)",
                    session.layout
                ),
            ));
        }
        let expected: Vec<String> = P1_MIMO_PARTICIPANT_IDS
            .iter()
            .map(|role| (*role).to_string())
            .collect();
        if session.participants != expected {
            return Err(err(
                TargetErrorKind::Participants,
                "session.participants",
                format!(
                    "session participants {:?} must equal the explicit task-boundary scope {expected:?} (no inference from role names/HP)",
                    session.participants
                ),
            ));
        }
        let mut entries = Vec::with_capacity(expected.len());
        for (dense_index, role_id) in expected.iter().enumerate() {
            if HEIGHT_ROLE_IDS.contains(&role_id.as_str()) {
                return Err(err(
                    TargetErrorKind::Scope,
                    "session.participants",
                    format!("height role {role_id} must not be a low-band MIMO participant"),
                ));
            }
            let speaker = session
                .speakers
                .iter()
                .find(|speaker| speaker.name == *role_id)
                .ok_or_else(|| {
                    err(
                        TargetErrorKind::Participants,
                        "session.speakers",
                        format!(
                            "participant {role_id} has no speaker declaration with an out_index"
                        ),
                    )
                })?;
            let expected_dac = P1_MIMO_PARTICIPANT_DAC[dense_index];
            if speaker.out_index != expected_dac {
                return Err(err(
                    TargetErrorKind::Participants,
                    format!("session.speakers[{role_id}].out_index"),
                    format!(
                        "role {role_id} out_index {} != expected 4.1.2 DAC slot {expected_dac} (overlay mapping must be resolved, not hardcoded)",
                        speaker.out_index
                    ),
                ));
            }
            entries.push((role_id.clone(), speaker.out_index));
        }
        Self::new(session.layout.clone(), entries)
    }

    /// 布局 id。
    pub fn layout_id(&self) -> &str {
        &self.layout_id
    }

    /// M（participants 数）。
    pub fn m(&self) -> usize {
        self.participants.len()
    }

    /// participants（dense 顺序）。
    pub fn participants(&self) -> &[ModalParticipant] {
        &self.participants
    }

    /// 角色 id 列表（dense 顺序）。
    pub fn role_ids(&self) -> Vec<&str> {
        self.participants
            .iter()
            .map(|participant| participant.role_id.as_str())
            .collect()
    }

    /// dense → DAC 槽位。
    pub fn dense_to_out_index(&self) -> Vec<u32> {
        self.participants
            .iter()
            .map(|participant| participant.out_index)
            .collect()
    }

    /// 角色 → dense 索引。
    pub fn dense_index_of(&self, role_id: &str) -> Option<u16> {
        self.participants
            .iter()
            .find(|participant| participant.role_id == role_id)
            .map(|participant| participant.dense_index)
    }

    /// 角色 → DAC 槽位。
    pub fn out_index_of(&self, role_id: &str) -> Option<u32> {
        self.participants
            .iter()
            .find(|participant| participant.role_id == role_id)
            .map(|participant| participant.out_index)
    }

    /// 是否包含角色。
    pub fn contains_role(&self, role_id: &str) -> bool {
        self.dense_index_of(role_id).is_some()
    }

    /// 是否恰为 P1 4.1.2 显式 scope（5 个、语义与任务边界一致）。
    pub fn is_p1_explicit(&self) -> bool {
        self.layout_id == P1_LAYOUT_4_1_2
            && self.participants.len() == P1_MIMO_PARTICIPANT_IDS.len()
            && self
                .participants
                .iter()
                .zip(P1_MIMO_PARTICIPANT_IDS.iter())
                .zip(P1_MIMO_PARTICIPANT_DAC.iter())
                .all(|((participant, expected_role), expected_dac)| {
                    participant.role_id == *expected_role && participant.out_index == *expected_dac
                })
    }

    /// 是否恰为 stereo strict scope（FL/FR → DAC 0/1）。
    pub fn is_stereo_strict(&self) -> bool {
        self.participants.len() == STEREO_STRICT_OUTPUT_ROLE_IDS.len()
            && self
                .participants
                .iter()
                .zip(STEREO_STRICT_OUTPUT_ROLE_IDS.iter())
                .enumerate()
                .all(|(dense_index, (participant, expected_role))| {
                    participant.role_id == *expected_role
                        && participant.dense_index == dense_index as u16
                })
    }

    /// canonical 文本（确定性；供 hash/证据）。
    pub fn canonical(&self) -> String {
        let mut canonical = String::new();
        canonical.push_str("participants layout=");
        canonical.push_str(&self.layout_id);
        canonical.push('\n');
        for participant in &self.participants {
            canonical.push_str(&format!(
                "dense{}={}:dac{}\n",
                participant.dense_index, participant.role_id, participant.out_index
            ));
        }
        canonical
    }

    /// 绑定 hash。
    pub fn binding_hash(&self) -> String {
        sha256_hex(self.canonical().as_bytes())
    }
}

// ===========================================================================
// 高度全频 / 输出 scope（显式分开，不从角色名推断频响）
// ===========================================================================

/// P_post 声明的保护带限（显式配置；corrected 基线不得默认加 HP/LP）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BandLimit {
    /// 无带限（全频）。
    None,
    /// 显式高通。
    HighPass { freq_hz: f64 },
    /// 显式低通。
    LowPass { freq_hz: f64 },
}

impl BandLimit {
    /// 稳定字面值。
    pub fn as_str(self) -> &'static str {
        match self {
            BandLimit::None => "none",
            BandLimit::HighPass { .. } => "highpass",
            BandLimit::LowPass { .. } => "lowpass",
        }
    }

    /// 频点（无带限时 None）。
    pub fn freq_hz(self) -> Option<f64> {
        match self {
            BandLimit::None => None,
            BandLimit::HighPass { freq_hz } | BandLimit::LowPass { freq_hz } => Some(freq_hz),
        }
    }

    /// 是否带限。
    pub fn is_band_limit(self) -> bool {
        !matches!(self, BandLimit::None)
    }

    /// canonical 文本。
    pub fn canonical(self) -> String {
        match self {
            BandLimit::None => "band_limit=none".to_string(),
            BandLimit::HighPass { freq_hz } => {
                format!("band_limit=highpass@{}", fmt_f64(freq_hz))
            }
            BandLimit::LowPass { freq_hz } => format!("band_limit=lowpass@{}", fmt_f64(freq_hz)),
        }
    }
}

/// 一个高度角色的全频 scope 记录。
#[derive(Debug, Clone, PartialEq)]
pub struct HeightRoleScope {
    /// 角色 id（HFL/HFR）。
    pub role_id: String,
    /// 容器/DAC 槽位。
    pub out_index: u32,
    /// 全频带（0..fs/2；不是 1.8k–10k 旧错误）。
    pub band_hz: (f64, f64),
    /// 保护带限（必须 None）。
    pub band_limit: BandLimit,
    /// 人工 delay（必须 0；旧 18 ms 生成器设计已被否定）。
    pub artificial_delay_ms: f64,
    /// 是否位于低带 5×5 矩阵（必须 false）。
    pub in_low_band_matrix: bool,
    /// P_post 在该角色低频段的最小幅度（dB；证据/报告，不作门）。
    pub p_low_band_min_db: f64,
    /// P_post 在该角色低频段的最大幅度（dB）。
    pub p_low_band_max_db: f64,
}

/// 高度全频 scope（与低带 5×5 矩阵显式分开；WBS T57/DEC §0）。
#[derive(Debug, Clone, PartialEq)]
pub struct FullRangeHeightScope {
    roles: Vec<HeightRoleScope>,
    sample_rate_hz: u32,
    excluded_from_low_band_matrix: bool,
}

impl FullRangeHeightScope {
    /// 角色列表。
    pub fn roles(&self) -> &[HeightRoleScope] {
        &self.roles
    }

    /// 采样率。
    pub fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    /// 是否保证不进低带矩阵（恒 true；记录进产物）。
    pub fn excluded_from_low_band_matrix(&self) -> bool {
        self.excluded_from_low_band_matrix
    }

    /// 按角色取。
    pub fn role(&self, role_id: &str) -> Option<&HeightRoleScope> {
        self.roles.iter().find(|role| role.role_id == role_id)
    }

    /// canonical 文本。
    pub fn canonical(&self) -> String {
        let mut canonical = String::new();
        canonical.push_str(&format!(
            "height_scope fs={} excluded_from_low_band_matrix={}\n",
            self.sample_rate_hz, self.excluded_from_low_band_matrix
        ));
        for role in &self.roles {
            canonical.push_str(&format!(
                "role={} dac={} band={}..{} {} artificial_delay_ms={} in_low_band_matrix={} p_low_band_db=[{},{}]\n",
                role.role_id,
                role.out_index,
                fmt_f64(role.band_hz.0),
                fmt_f64(role.band_hz.1),
                role.band_limit.canonical(),
                fmt_f64(role.artificial_delay_ms),
                role.in_low_band_matrix,
                fmt_f64(role.p_low_band_min_db),
                fmt_f64(role.p_low_band_max_db)
            ));
        }
        canonical
    }

    /// 绑定 hash。
    pub fn binding_hash(&self) -> String {
        sha256_hex(self.canonical().as_bytes())
    }
}

/// 输出角色 scope 种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputScopeKind {
    /// 低带 5×5 矩阵 participant（dense 索引）。
    LowBandMatrix { dense_index: u16 },
    /// 高度全频（不进低带矩阵）。
    FullRangeHeight,
    /// 其它（显式记为非矩阵输出；不得静默下混/造声道）。
    Other,
}

impl OutputScopeKind {
    /// 稳定字面值。
    pub const fn as_str(self) -> &'static str {
        match self {
            OutputScopeKind::LowBandMatrix { .. } => "low_band_matrix",
            OutputScopeKind::FullRangeHeight => "full_range_height",
            OutputScopeKind::Other => "other",
        }
    }
}

/// 一个输出角色的 scope 记录。
#[derive(Debug, Clone, PartialEq)]
pub struct OutputScopeEntry {
    /// 角色 id。
    pub role_id: String,
    /// 容器/DAC 槽位。
    pub out_index: u32,
    /// scope 种类。
    pub kind: OutputScopeKind,
    /// 该角色声明频带（高度/其它为全频；participant 由保护带限裁剪）。
    pub band_hz: (f64, f64),
    /// P_post 保护带限。
    pub band_limit: BandLimit,
}

/// 全部输出角色的 scope 表（唯一 role→DAC→矩阵归属；证据/报告用）。
#[derive(Debug, Clone, PartialEq)]
pub struct ScopedOutputs {
    layout_id: String,
    entries: Vec<OutputScopeEntry>,
}

impl ScopedOutputs {
    /// 全部 entries。
    pub fn entries(&self) -> &[OutputScopeEntry] {
        &self.entries
    }

    /// 低带矩阵 entries。
    pub fn low_band_entries(&self) -> Vec<&OutputScopeEntry> {
        self.entries
            .iter()
            .filter(|entry| matches!(entry.kind, OutputScopeKind::LowBandMatrix { .. }))
            .collect()
    }

    /// 高度 entries。
    pub fn height_entries(&self) -> Vec<&OutputScopeEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.kind == OutputScopeKind::FullRangeHeight)
            .collect()
    }

    /// canonical 文本。
    pub fn canonical(&self) -> String {
        let mut canonical = String::new();
        canonical.push_str(&format!("output_scope layout={}\n", self.layout_id));
        for entry in &self.entries {
            canonical.push_str(&format!(
                "role={} dac={} kind={} band={}..{} {}\n",
                entry.role_id,
                entry.out_index,
                entry.kind.as_str(),
                fmt_f64(entry.band_hz.0),
                fmt_f64(entry.band_hz.1),
                entry.band_limit.canonical()
            ));
        }
        canonical
    }
}

// ===========================================================================
// P_post 拟合结果（每输出角色；冻结前可组装，冻结后不可变）
// ===========================================================================

/// 一个输出角色的 P_post 拟合结果。
///
/// points 是**完整** P_post 复响应（已含 tone/PEQ/TargetFir/delay/polarity/gain；权威值），
/// 其余字段是阶段级契约（用于极性单次、带限、PEQ 契约、人工 delay 禁令、高度全频校验）。
#[derive(Debug, Clone, PartialEq)]
pub struct PostRoleFit {
    role_id: String,
    out_index: u32,
    points: Vec<ComplexPoint>,
    delay_samples: u32,
    polarity: i8,
    gain_db: f64,
    band_limit: BandLimit,
    artificial_delay_ms: f64,
    modal_peq: Option<ChannelPeq>,
}

impl PostRoleFit {
    /// 以显式复响应构造（identity 默认：delay 0、polarity +1、gain 0、无带限）。
    pub fn new(
        role_id: impl Into<String>,
        out_index: u32,
        points: Vec<ComplexPoint>,
    ) -> Result<Self, TargetError> {
        let role_id = role_id.into();
        if !is_role_id(&role_id) {
            return Err(err(
                TargetErrorKind::Role,
                "post.role_id",
                format!("role id {role_id:?} must match ^[A-Z][A-Z0-9]*$"),
            ));
        }
        if out_index >= MAX_CONTAINER_CHANNELS {
            return Err(err(
                TargetErrorKind::Role,
                "post.out_index",
                format!("out_index {out_index} exceeds MAX_CONTAINER_CHANNELS"),
            ));
        }
        for (index, point) in points.iter().enumerate() {
            let field = format!("post.points[{index}]");
            finite(point.freq_hz, &format!("{field}.freq_hz"))?;
            finite(point.re, &format!("{field}.re"))?;
            finite(point.im, &format!("{field}.im"))?;
            if point.freq_hz <= 0.0 {
                return Err(err(
                    TargetErrorKind::Grid,
                    format!("{field}.freq_hz"),
                    format!("frequency {} must be > 0", point.freq_hz),
                ));
            }
        }
        Ok(Self {
            role_id,
            out_index,
            points,
            delay_samples: 0,
            polarity: 1,
            gain_db: 0.0,
            band_limit: BandLimit::None,
            artificial_delay_ms: 0.0,
            modal_peq: None,
        })
    }

    /// 解析构造：由显式 PEQ + 整数 delay + 极性 + gain 计算完整 P_post 复响应（测试/离线夹具）。
    ///
    /// 这是"P = PEQ · z^-D · polarity · gain"的闭式实现；真实完整 P_post（含目标 FIR）由 T64/T65
    /// 拟合后经 [PostRoleFit::new] 以 points 传入，points 为权威值。
    #[allow(clippy::too_many_arguments)]
    pub fn analytic(
        role_id: impl Into<String>,
        out_index: u32,
        sample_rate_hz: u32,
        grid_hz: &[f64],
        modal_peq: Option<ChannelPeq>,
        delay_samples: u32,
        polarity: i8,
        gain_db: f64,
    ) -> Result<Self, TargetError> {
        if !sample_rate_ok(sample_rate_hz) {
            return Err(err(
                TargetErrorKind::SampleRate,
                "post.sample_rate_hz",
                format!("sample rate {sample_rate_hz} is not a P1 rate {SAMPLE_RATES_HZ:?}"),
            ));
        }
        validate_frequency_grid(grid_hz, sample_rate_hz, "post.grid_hz")?;
        let fs = sample_rate_hz as f64;
        if polarity != 1 && polarity != -1 {
            return Err(err(
                TargetErrorKind::Polarity,
                "post.polarity",
                format!("polarity {polarity} must be +1 or -1 (0/unknown must not be guessed)"),
            ));
        }
        finite_in_range(gain_db, POST_GAIN_MIN_DB, POST_GAIN_MAX_DB, "post.gain_db")?;
        let delay_limit = fs * POST_DELAY_MAX_MS / 1000.0;
        if delay_samples as f64 > delay_limit {
            return Err(err(
                TargetErrorKind::Range,
                "post.delay_samples",
                format!(
                    "delay {delay_samples} samples exceeds {POST_DELAY_MAX_MS} ms limit ({delay_limit} samples at {sample_rate_hz} Hz)"
                ),
            ));
        }
        let gain_linear = 10.0f64.powf(gain_db / 20.0);
        let mut points = Vec::with_capacity(grid_hz.len());
        for freq_hz in grid_hz {
            let (mut re, mut im) = match &modal_peq {
                Some(peq) => peq.cascade_complex(*freq_hz, fs),
                None => (1.0, 0.0),
            };
            let rotation = -2.0 * std::f64::consts::PI * freq_hz * delay_samples as f64 / fs;
            let (sin, cos) = rotation.sin_cos();
            let rotated_re = re * cos - im * sin;
            let rotated_im = re * sin + im * cos;
            re = rotated_re * (polarity as f64) * gain_linear;
            im = rotated_im * (polarity as f64) * gain_linear;
            points.push(ComplexPoint {
                freq_hz: *freq_hz,
                re,
                im,
            });
        }
        Ok(Self {
            role_id: role_id.into(),
            out_index,
            points,
            delay_samples,
            polarity,
            gain_db,
            band_limit: BandLimit::None,
            artificial_delay_ms: 0.0,
            modal_peq,
        })
    }

    /// identity P_post（每点 1+0j）。
    pub fn identity(
        role_id: impl Into<String>,
        out_index: u32,
        grid_hz: &[f64],
    ) -> Result<Self, TargetError> {
        let points = grid_hz
            .iter()
            .map(|freq_hz| ComplexPoint {
                freq_hz: *freq_hz,
                re: 1.0,
                im: 0.0,
            })
            .collect();
        Self::new(role_id, out_index, points)
    }

    /// 设置阶段元数据（delay/polarity/gain；points 不变，契约在冻结时复校）。
    pub fn with_meta(
        mut self,
        delay_samples: u32,
        polarity: i8,
        gain_db: f64,
    ) -> Result<Self, TargetError> {
        if polarity != 1 && polarity != -1 {
            return Err(err(
                TargetErrorKind::Polarity,
                "post.polarity",
                format!("polarity {polarity} must be +1 or -1 (0/unknown must not be guessed)"),
            ));
        }
        finite_in_range(gain_db, POST_GAIN_MIN_DB, POST_GAIN_MAX_DB, "post.gain_db")?;
        self.delay_samples = delay_samples;
        self.polarity = polarity;
        self.gain_db = gain_db;
        Ok(self)
    }

    /// 设置保护带限（显式；高度必须 None，SUB1 低通 ≤130 Hz）。
    pub fn with_band_limit(mut self, band_limit: BandLimit) -> Self {
        self.band_limit = band_limit;
        self
    }

    /// 设置人工 delay（corrected 基线禁止；仅用于负向夹具/旧数据显式声明）。
    pub fn with_artificial_delay_ms(
        mut self,
        artificial_delay_ms: f64,
    ) -> Result<Self, TargetError> {
        self.artificial_delay_ms = finite(artificial_delay_ms, "post.artificial_delay_ms")?;
        Ok(self)
    }

    /// 绑定阶段级模态 PEQ（证据/契约复核；points 仍为权威值）。
    pub fn with_modal_peq(mut self, modal_peq: ChannelPeq) -> Self {
        self.modal_peq = Some(modal_peq);
        self
    }

    /// 角色 id。
    pub fn role_id(&self) -> &str {
        &self.role_id
    }

    /// DAC 槽位。
    pub fn out_index(&self) -> u32 {
        self.out_index
    }

    /// P_post 复响应点。
    pub fn points(&self) -> &[ComplexPoint] {
        &self.points
    }

    /// 整数延迟（samples）。
    pub fn delay_samples(&self) -> u32 {
        self.delay_samples
    }

    /// 极性（±1；应用一次）。
    pub fn polarity(&self) -> i8 {
        self.polarity
    }

    /// 角色 gain（dB）。
    pub fn gain_db(&self) -> f64 {
        self.gain_db
    }

    /// 保护带限。
    pub fn band_limit(&self) -> BandLimit {
        self.band_limit
    }

    /// 人工 delay（ms；corrected 基线必须 0）。
    pub fn artificial_delay_ms(&self) -> f64 {
        self.artificial_delay_ms
    }

    /// 阶段级模态 PEQ（可选）。
    pub fn modal_peq(&self) -> Option<&ChannelPeq> {
        self.modal_peq.as_ref()
    }

    /// P_post 在指定频率的复响应（log-f 线性插值）。
    pub fn response_at(&self, freq_hz: f64) -> Result<(f64, f64), TargetError> {
        let freq_hz = finite(freq_hz, "post.freq_hz")?;
        if freq_hz <= 0.0 {
            return Err(err(
                TargetErrorKind::Range,
                "post.freq_hz",
                format!("frequency {freq_hz} must be > 0"),
            ));
        }
        Ok(interpolate_complex(&self.points, freq_hz))
    }

    /// P_post 幅度（dB）。
    pub fn magnitude_db_at(&self, freq_hz: f64) -> Result<f64, TargetError> {
        let (re, im) = self.response_at(freq_hz)?;
        let magnitude = re.hypot(im).max(NEAR_ZERO_MAGNITUDE);
        Ok(20.0 * magnitude.log10())
    }

    /// 契约校验（网格/极值/带限/人工 delay/PEQ 稳定性）。
    pub fn validate_contract(
        &self,
        sample_rate_hz: u32,
        grid_hz: &[f64],
    ) -> Result<(), TargetError> {
        if !sample_rate_ok(sample_rate_hz) {
            return Err(err(
                TargetErrorKind::SampleRate,
                "post.sample_rate_hz",
                format!("sample rate {sample_rate_hz} is not a P1 rate {SAMPLE_RATES_HZ:?}"),
            ));
        }
        if !is_role_id(&self.role_id) {
            return Err(err(
                TargetErrorKind::Role,
                "post.role_id",
                format!("role id {:?} must match ^[A-Z][A-Z0-9]*$", self.role_id),
            ));
        }
        if self.out_index >= MAX_CONTAINER_CHANNELS {
            return Err(err(
                TargetErrorKind::Role,
                format!("post.{}.out_index", self.role_id),
                format!(
                    "out_index {} exceeds MAX_CONTAINER_CHANNELS",
                    self.out_index
                ),
            ));
        }
        if self.polarity != 1 && self.polarity != -1 {
            return Err(err(
                TargetErrorKind::Polarity,
                format!("post.{}.polarity", self.role_id),
                format!("polarity {} must be +1 or -1", self.polarity),
            ));
        }
        let fs = sample_rate_hz as f64;
        let delay_limit = fs * POST_DELAY_MAX_MS / 1000.0;
        if self.delay_samples as f64 > delay_limit {
            return Err(err(
                TargetErrorKind::Range,
                format!("post.{}.delay_samples", self.role_id),
                format!(
                    "delay {} samples exceeds {POST_DELAY_MAX_MS} ms ({delay_limit} samples)",
                    self.delay_samples
                ),
            ));
        }
        finite_in_range(
            self.gain_db,
            POST_GAIN_MIN_DB,
            POST_GAIN_MAX_DB,
            &format!("post.{}.gain_db", self.role_id),
        )?;
        if self.artificial_delay_ms.abs() > ARTIFICIAL_DELAY_TOLERANCE_MS {
            return Err(err(
                TargetErrorKind::Scope,
                format!("post.{}.artificial_delay_ms", self.role_id),
                format!(
                    "corrected baseline forbids artificial delay (old 18 ms generator design was rejected); declared {} ms",
                    self.artificial_delay_ms
                ),
            ));
        }
        let is_height = HEIGHT_ROLE_IDS.contains(&self.role_id.as_str());
        match self.band_limit {
            BandLimit::None => {}
            BandLimit::HighPass { freq_hz } => {
                let freq_hz = finite_in_range(
                    freq_hz,
                    PROTECTION_FREQ_MIN_HZ,
                    PROTECTION_FREQ_MAX_HZ,
                    &format!("post.{}.band_limit.highpass_hz", self.role_id),
                )?;
                let _ = freq_hz;
                if is_height {
                    return Err(err(
                        TargetErrorKind::Scope,
                        format!("post.{}.band_limit", self.role_id),
                        format!(
                            "height role {} must be full-range (no HP/LP) in the corrected baseline",
                            self.role_id
                        ),
                    ));
                }
            }
            BandLimit::LowPass { freq_hz } => {
                let freq_hz = finite_in_range(
                    freq_hz,
                    PROTECTION_FREQ_MIN_HZ,
                    PROTECTION_FREQ_MAX_HZ,
                    &format!("post.{}.band_limit.lowpass_hz", self.role_id),
                )?;
                if is_height {
                    return Err(err(
                        TargetErrorKind::Scope,
                        format!("post.{}.band_limit", self.role_id),
                        format!(
                            "height role {} must be full-range (no HP/LP) in the corrected baseline",
                            self.role_id
                        ),
                    ));
                }
                if self.role_id == "SUB1" && freq_hz > SUB1_PROTECTION_LP_MAX_HZ {
                    return Err(err(
                        TargetErrorKind::Role,
                        format!("post.{}.band_limit.lowpass_hz", self.role_id),
                        format!(
                            "SUB1 protection low-pass {freq_hz} Hz exceeds {SUB1_PROTECTION_LP_MAX_HZ} Hz (M1: SUB <=130 Hz)"
                        ),
                    ));
                }
            }
        }
        if let Some(peq) = &self.modal_peq {
            let limits = PeqLimits::producer(DesignMode::ModalPlusWide, true, [20.0, 250.0]);
            peq.validate(sample_rate_hz, &limits).map_err(|error| {
                err(
                    TargetErrorKind::Baseline,
                    format!("post.{}.modal_peq", self.role_id),
                    format!("{error}"),
                )
            })?;
        }
        validate_complex_grid(
            &self.points,
            grid_hz,
            &format!("post.{}.points", self.role_id),
        )
    }
}

// ===========================================================================
// 拟合后冻结 P（移动语义；冻结后无 mutator）
// ===========================================================================

/// 冻结前的 P_post 基线输入（[FittedPostBaseline::new] 校验后持有一份；freeze 消费）。
#[derive(Debug, Clone, PartialEq)]
pub struct PostBaselineInput {
    /// 基线 id（稳定 token）。
    pub baseline_id: String,
    /// 采样率（P1 契约）。
    pub sample_rate_hz: u32,
    /// 显式 participants。
    pub participants: ModalParticipants,
    /// 冻结基线 tone 绑定。
    pub tone: ToneBinding,
    /// curve 绑定（唯一合法：P_post 基线一次）。
    pub curve: CurveBinding,
    /// tone 应用次数声明（必须 1/0/0）。
    pub tone_applications: ToneApplicationProof,
    /// 拟合输入 hash（T64/T65 产物：测量/校准/目标/参数；64 hex）。
    pub fit_inputs_sha256: String,
    /// T6 FC-04 公共整数 D0（provenance；去相位已在 T49 完成）。
    pub common_d0: CommonD0,
    /// 共同频率网格（H_raw 与 P 同一网格）。
    pub grid_hz: Vec<f64>,
    /// 每输出角色的 P_post 拟合结果。
    pub roles: Vec<PostRoleFit>,
    /// 创建时刻（unix 秒；**不进入 hash**）。
    pub created_unix: Option<u64>,
}

/// 未冻结的 P_post 基线（仍可被调用方丢弃/重建；不能直接用于 H_eff）。
#[derive(Debug, Clone, PartialEq)]
pub struct FittedPostBaseline {
    input: PostBaselineInput,
}

impl FittedPostBaseline {
    /// 校验并持有 P_post 基线输入（高度/带限/tone/curve/participants/PEQ 契约全部在此把关）。
    pub fn new(input: PostBaselineInput) -> Result<Self, TargetError> {
        if !is_id_token(&input.baseline_id, 64) {
            return Err(err(
                TargetErrorKind::Baseline,
                "baseline.baseline_id",
                format!(
                    "baseline_id {:?} must be a stable id token",
                    input.baseline_id
                ),
            ));
        }
        if !sample_rate_ok(input.sample_rate_hz) {
            return Err(err(
                TargetErrorKind::SampleRate,
                "baseline.sample_rate_hz",
                format!(
                    "sample rate {} is not a P1 rate {SAMPLE_RATES_HZ:?}",
                    input.sample_rate_hz
                ),
            ));
        }
        if input.participants.layout_id() != P1_LAYOUT_4_1_2 {
            return Err(err(
                TargetErrorKind::Unsupported,
                "baseline.participants.layout_id",
                format!(
                    "layout {:?} is not the P1 target layout {P1_LAYOUT_4_1_2:?}",
                    input.participants.layout_id()
                ),
            ));
        }
        if !is_sha256_hex(&input.fit_inputs_sha256)
            || input.fit_inputs_sha256.bytes().all(|byte| byte == b'0')
        {
            return Err(err(
                TargetErrorKind::Baseline,
                "baseline.fit_inputs_sha256",
                "fit input hash must be 64 lowercase hex and non-zero (stale binding anchor)",
            ));
        }
        validate_frequency_grid(&input.grid_hz, input.sample_rate_hz, "baseline.grid_hz")?;
        input.curve.validate_single_application()?;
        input.tone_applications.validate()?;
        if input.curve.curve_id() != input.tone.curve_id() {
            return Err(err(
                TargetErrorKind::Curve,
                "baseline.curve.curve_id",
                format!(
                    "curve binding id {:?} must equal the frozen baseline tone curve id {:?}",
                    input.curve.curve_id(),
                    input.tone.curve_id()
                ),
            ));
        }
        if input.curve.target_sha256() != input.tone.target_sha256() {
            return Err(err(
                TargetErrorKind::Curve,
                "baseline.curve.target_sha256",
                "curve binding target hash must equal the frozen baseline tone target hash (single tone binding)",
            ));
        }
        if input.roles.is_empty() {
            return Err(err(
                TargetErrorKind::Missing,
                "baseline.roles",
                "P_post baseline must declare at least one output role",
            ));
        }
        for (index, role) in input.roles.iter().enumerate() {
            role.validate_contract(input.sample_rate_hz, &input.grid_hz)
                .map_err(|error| {
                    err(
                        error.kind(),
                        format!("baseline.roles[{index}].{}", error.field()),
                        error.detail(),
                    )
                })?;
            if input.roles[..index]
                .iter()
                .any(|existing| existing.role_id() == role.role_id())
            {
                return Err(err(
                    TargetErrorKind::Role,
                    format!("baseline.roles[{index}].role_id"),
                    format!("duplicate output role {}", role.role_id()),
                ));
            }
            if input.roles[..index]
                .iter()
                .any(|existing| existing.out_index() == role.out_index())
            {
                return Err(err(
                    TargetErrorKind::Role,
                    format!("baseline.roles[{index}].out_index"),
                    format!(
                        "duplicate out_index {} (one P_post role per DAC slot)",
                        role.out_index()
                    ),
                ));
            }
        }
        for participant in input.participants.participants() {
            let role = input
                .roles
                .iter()
                .find(|role| role.role_id() == participant.role_id)
                .ok_or_else(|| {
                    err(
                        TargetErrorKind::Baseline,
                        format!("baseline.roles[{}]", participant.role_id),
                        format!(
                            "explicit participant {} has no P_post role (cannot compose H_eff)",
                            participant.role_id
                        ),
                    )
                })?;
            if role.out_index() != participant.out_index {
                return Err(err(
                    TargetErrorKind::Role,
                    format!("baseline.roles[{}].out_index", participant.role_id),
                    format!(
                        "participant {} out_index {} != P_post role out_index {}",
                        participant.role_id,
                        participant.out_index,
                        role.out_index()
                    ),
                ));
            }
        }
        Ok(Self { input })
    }

    /// 拟合后冻结 P（消费 self；返回不可变基线）。
    pub fn freeze(self) -> Result<FrozenPostBaseline, TargetError> {
        let frozen_hash = post_freeze_hash(&self.input);
        Ok(FrozenPostBaseline {
            input: self.input,
            frozen_hash,
        })
    }

    /// 基线 id。
    pub fn baseline_id(&self) -> &str {
        &self.input.baseline_id
    }

    /// 采样率。
    pub fn sample_rate_hz(&self) -> u32 {
        self.input.sample_rate_hz
    }

    /// participants。
    pub fn participants(&self) -> &ModalParticipants {
        &self.input.participants
    }

    /// tone 绑定。
    pub fn tone(&self) -> &ToneBinding {
        &self.input.tone
    }

    /// curve 绑定。
    pub fn curve(&self) -> &CurveBinding {
        &self.input.curve
    }

    /// tone 应用次数声明。
    pub fn tone_applications(&self) -> ToneApplicationProof {
        self.input.tone_applications
    }

    /// 拟合输入 hash。
    pub fn fit_inputs_sha256(&self) -> &str {
        &self.input.fit_inputs_sha256
    }

    /// 公共整数 D0。
    pub fn common_d0(&self) -> CommonD0 {
        self.input.common_d0
    }

    /// 共同频率网格。
    pub fn grid_hz(&self) -> &[f64] {
        &self.input.grid_hz
    }

    /// 全部输出角色。
    pub fn roles(&self) -> &[PostRoleFit] {
        &self.input.roles
    }

    /// 按角色取。
    pub fn role(&self, role_id: &str) -> Option<&PostRoleFit> {
        self.input
            .roles
            .iter()
            .find(|role| role.role_id() == role_id)
    }
}

/// 已冻结的 P_post 基线（H_eff 组合的唯一合法输入；字段全私有、无 mutator）。
#[derive(Debug, Clone, PartialEq)]
pub struct FrozenPostBaseline {
    input: PostBaselineInput,
    frozen_hash: String,
}

impl FrozenPostBaseline {
    /// schema = [P_FREEZE_SCHEMA]。
    pub const SCHEMA: &'static str = P_FREEZE_SCHEMA;

    /// 基线 id。
    pub fn baseline_id(&self) -> &str {
        &self.input.baseline_id
    }

    /// 采样率。
    pub fn sample_rate_hz(&self) -> u32 {
        self.input.sample_rate_hz
    }

    /// 显式 participants。
    pub fn participants(&self) -> &ModalParticipants {
        &self.input.participants
    }

    /// tone 绑定。
    pub fn tone(&self) -> &ToneBinding {
        &self.input.tone
    }

    /// curve 绑定。
    pub fn curve(&self) -> &CurveBinding {
        &self.input.curve
    }

    /// tone 应用次数声明。
    pub fn tone_applications(&self) -> ToneApplicationProof {
        self.input.tone_applications
    }

    /// 拟合输入 hash。
    pub fn fit_inputs_sha256(&self) -> &str {
        &self.input.fit_inputs_sha256
    }

    /// 公共整数 D0（T6 FC-04 provenance；去相位已在 T49 完成）。
    pub fn common_d0(&self) -> CommonD0 {
        self.input.common_d0
    }

    /// 共同频率网格。
    pub fn grid_hz(&self) -> &[f64] {
        &self.input.grid_hz
    }

    /// 全部输出角色。
    pub fn roles(&self) -> &[PostRoleFit] {
        &self.input.roles
    }

    /// 创建时刻（不进入 hash）。
    pub fn created_unix(&self) -> Option<u64> {
        self.input.created_unix
    }

    /// 冻结 hash（P 身份；H_eff/target 绑定用）。
    pub fn baseline_hash(&self) -> &str {
        &self.frozen_hash
    }

    /// 按角色取。
    pub fn role(&self, role_id: &str) -> Option<&PostRoleFit> {
        self.input
            .roles
            .iter()
            .find(|role| role.role_id() == role_id)
    }

    /// 角色 P_post 点（要求网格一致）。
    pub fn p_points(&self, role_id: &str) -> Option<&[ComplexPoint]> {
        self.role(role_id).map(|role| role.points())
    }

    /// 角色 P_post 在给定网格上的复响应（网格必须与冻结基线一致）。
    pub fn p_grid(&self, role_id: &str, grid_hz: &[f64]) -> Result<Vec<ComplexPoint>, TargetError> {
        if !grid_matches(grid_hz, &self.input.grid_hz) {
            return Err(err(
                TargetErrorKind::Grid,
                format!("post.{role_id}.grid_hz"),
                "requested grid does not match the frozen P baseline grid",
            ));
        }
        let role = self.role(role_id).ok_or_else(|| {
            err(
                TargetErrorKind::Missing,
                format!("post.{role_id}"),
                format!("frozen P baseline has no role {role_id}"),
            )
        })?;
        Ok(role.points().to_vec())
    }

    /// 角色 P_post 任意频点复响应（log-f 线性插值）。
    pub fn p_response(&self, role_id: &str, freq_hz: f64) -> Result<(f64, f64), TargetError> {
        let role = self.role(role_id).ok_or_else(|| {
            err(
                TargetErrorKind::Missing,
                format!("post.{role_id}"),
                format!("frozen P baseline has no role {role_id}"),
            )
        })?;
        role.response_at(freq_hz)
    }

    /// 校验 tone 单次应用（声明层；G/target 均 0 次）。
    pub fn verify_single_tone_application(&self) -> Result<(), TargetError> {
        self.input.curve.validate_single_application()?;
        self.input.tone_applications.validate()
    }

    /// 高度全频 scope（4.1.2 必须两个高度角色都存在且全频）。
    pub fn height_scope(&self) -> Result<FullRangeHeightScope, TargetError> {
        match height_scope_from_input(&self.input)? {
            Some(scope) => Ok(scope),
            None => Err(err(
                TargetErrorKind::Missing,
                "baseline.roles",
                "4.1.2 multichannel profile must declare HFL/HFR full-range roles",
            )),
        }
    }

    /// 可选高度 scope（stereo strict 允许没有高度输出；部分存在 → E_TGT_MISSING）。
    pub fn optional_height_scope(&self) -> Result<Option<FullRangeHeightScope>, TargetError> {
        height_scope_from_input(&self.input)
    }

    /// 全部输出角色 scope 表（participant/高度/其它显式分开）。
    pub fn output_scope(&self) -> Result<ScopedOutputs, TargetError> {
        output_scope_from_input(&self.input)
    }

    /// 当前基线身份（stale 检查输入）。
    pub fn identity(&self) -> BaselineIdentity {
        BaselineIdentity {
            fit_inputs_sha256: self.input.fit_inputs_sha256.clone(),
            sample_rate_hz: self.input.sample_rate_hz,
            tone_curve_id: self.input.tone.curve_id().to_string(),
            tone_shape: self.input.tone.shape(),
            tone_reference_gain_db: self.input.tone.reference_gain_db(),
            tone_target_sha256: self.input.tone.target_sha256().map(str::to_string),
            participants_binding_hash: self.input.participants.binding_hash(),
            grid_points: self.input.grid_hz.len(),
        }
    }

    /// stale 检查：P 拟合输入/fs/tone/curve/participants/网格任一变化都必须重新设计（P/G 不交换）。
    pub fn stale_reasons(&self, current: &BaselineIdentity) -> Vec<StaleReason> {
        let frozen = self.identity();
        let mut reasons = Vec::new();
        if frozen.fit_inputs_sha256 != current.fit_inputs_sha256 {
            reasons.push(StaleReason::new(
                "STALE_FIT_INPUTS",
                "fitting inputs changed (measurement/calibration/target/params); P must be re-fitted and re-frozen",
            ));
        }
        if frozen.sample_rate_hz != current.sample_rate_hz {
            reasons.push(StaleReason::new(
                "STALE_SAMPLE_RATE",
                format!(
                    "sample rate changed {} -> {}; P/G must be redesigned",
                    frozen.sample_rate_hz, current.sample_rate_hz
                ),
            ));
        }
        if frozen.tone_curve_id != current.tone_curve_id
            || frozen.tone_shape != current.tone_shape
            || frozen.tone_reference_gain_db != current.tone_reference_gain_db
            || frozen.tone_target_sha256 != current.tone_target_sha256
        {
            reasons.push(StaleReason::new(
                "STALE_TONE",
                "baseline tone binding changed; curve_id/tone is a frozen baseline binding, not a re-applied filter",
            ));
        }
        if frozen.participants_binding_hash != current.participants_binding_hash {
            reasons.push(StaleReason::new(
                "STALE_PARTICIPANTS",
                "MIMO participants mapping changed; target matrix dimensions/rows are bound to it",
            ));
        }
        if frozen.grid_points != current.grid_points {
            reasons.push(StaleReason::new(
                "STALE_GRID",
                format!(
                    "frequency grid changed ({} -> {} points)",
                    frozen.grid_points, current.grid_points
                ),
            ));
        }
        reasons
    }

    /// canonical 文本（确定性；不含时间戳）。
    pub fn canonical(&self) -> String {
        canonical_post_baseline(&self.input)
    }
}

/// 基线身份（stale 检查的当前值；由 host/daemon 在输入变化时提供）。
#[derive(Debug, Clone, PartialEq)]
pub struct BaselineIdentity {
    /// 拟合输入 hash。
    pub fit_inputs_sha256: String,
    /// 采样率。
    pub sample_rate_hz: u32,
    /// tone curve id。
    pub tone_curve_id: String,
    /// tone 形状。
    pub tone_shape: ToneShape,
    /// tone reference gain。
    pub tone_reference_gain_db: f64,
    /// tone 目标 hash。
    pub tone_target_sha256: Option<String>,
    /// participants 绑定 hash。
    pub participants_binding_hash: String,
    /// 网格点数。
    pub grid_points: usize,
}

/// 一条 stale 原因（稳定机器码 + 说明）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleReason {
    /// 稳定机器码（STALE_*）。
    pub code: &'static str,
    /// 说明。
    pub detail: String,
}

impl StaleReason {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

// ===========================================================================
// H_raw（T48/T49 产物适配；极性恰好归一化一次）
// ===========================================================================

/// 一个参与者在某测点的 H_raw 复响应（T49 已去相位一次；物理极性在此归一化一次）。
#[derive(Debug, Clone, PartialEq)]
pub struct RawResponse {
    role_id: String,
    out_index: u32,
    points: Vec<ComplexPoint>,
    polarity: Option<ResolvedPolarity>,
}

impl RawResponse {
    /// 未归一化极性的原始复响应（极性必须随后 exactly-once 归一化）。
    pub fn new(
        role_id: impl Into<String>,
        out_index: u32,
        points: Vec<ComplexPoint>,
    ) -> Result<Self, TargetError> {
        let role_id = role_id.into();
        if !is_role_id(&role_id) {
            return Err(err(
                TargetErrorKind::Role,
                "raw.role_id",
                format!("role id {role_id:?} must match ^[A-Z][A-Z0-9]*$"),
            ));
        }
        if out_index >= MAX_CONTAINER_CHANNELS {
            return Err(err(
                TargetErrorKind::Role,
                "raw.out_index",
                format!("out_index {out_index} exceeds MAX_CONTAINER_CHANNELS"),
            ));
        }
        if points.len() < 2 {
            return Err(err(
                TargetErrorKind::Grid,
                "raw.points",
                "at least 2 complex points are required",
            ));
        }
        for (index, point) in points.iter().enumerate() {
            let field = format!("raw.points[{index}]");
            finite(point.freq_hz, &format!("{field}.freq_hz"))?;
            finite(point.re, &format!("{field}.re"))?;
            finite(point.im, &format!("{field}.im"))?;
        }
        Ok(Self {
            role_id,
            out_index,
            points,
            polarity: None,
        })
    }

    /// 从 T49 [AbsoluteResponse] 构造：克隆后恰好一次乘入已解析物理极性（第二次 → E_TGT_POLARITY）。
    pub fn from_absolute_response(
        role_id: impl Into<String>,
        out_index: u32,
        response: &AbsoluteResponse,
        polarity: ResolvedPolarity,
    ) -> Result<Self, TargetError> {
        if response.polarity().is_some() {
            return Err(err(
                TargetErrorKind::Polarity,
                "raw.polarity",
                "absolute response already had polarity normalized once; refusing to apply it twice (double polarity)",
            ));
        }
        let mut normalized = response.clone();
        normalized.normalize_polarity(polarity).map_err(|error| {
            err(
                TargetErrorKind::Polarity,
                format!("raw.{}.polarity", response.label()),
                format!("{error}"),
            )
        })?;
        let mut raw = Self::new(role_id, out_index, normalized.points().to_vec())?;
        raw.polarity = Some(polarity);
        Ok(raw)
    }

    /// 从 T49 [SlotPhase]（公共相位结果）构造（角色/out_index/极性同源，极性只乘一次）。
    pub fn from_slot_phase(slot: &SlotPhase) -> Result<Self, TargetError> {
        Self::from_absolute_response(
            slot.speaker.clone(),
            slot.out_index,
            &slot.absolute,
            slot.polarity,
        )
    }

    /// 归一化物理极性（exactly once；已归一化再调用 → E_TGT_POLARITY）。
    pub fn normalize_polarity(mut self, polarity: ResolvedPolarity) -> Result<Self, TargetError> {
        if self.polarity.is_some() {
            return Err(err(
                TargetErrorKind::Polarity,
                format!("raw.{}.polarity", self.role_id),
                "physical polarity was already normalized once; double polarity is undefined",
            ));
        }
        if polarity.sign() == Polarity::Invert {
            for point in &mut self.points {
                point.re = -point.re;
                point.im = -point.im;
            }
        }
        self.polarity = Some(polarity);
        Ok(self)
    }

    /// 角色 id。
    pub fn role_id(&self) -> &str {
        &self.role_id
    }

    /// DAC 槽位。
    pub fn out_index(&self) -> u32 {
        self.out_index
    }

    /// H_raw 复响应点（T49 已去相位；极性视是否归一化）。
    pub fn points(&self) -> &[ComplexPoint] {
        &self.points
    }

    /// 已归一的物理极性（None = 尚未归一化）。
    pub fn polarity(&self) -> Option<ResolvedPolarity> {
        self.polarity
    }

    /// 极性是否已归一化。
    pub fn is_polarity_normalized(&self) -> bool {
        self.polarity.is_some()
    }
}

/// 一个测点的 H_raw（每 participant 一条；权重/角色显式）。
#[derive(Debug, Clone, PartialEq)]
pub struct RawPosition {
    label: String,
    role: PositionRole,
    weight: f64,
    responses: Vec<RawResponse>,
}

impl RawPosition {
    /// 构造（label 唯一、weight>0、responses 去掉重复角色）。
    pub fn new(
        label: impl Into<String>,
        role: PositionRole,
        weight: f64,
        responses: Vec<RawResponse>,
    ) -> Result<Self, TargetError> {
        let label = label.into();
        if !is_id_token(&label, 64) {
            return Err(err(
                TargetErrorKind::Position,
                "position.label",
                format!("position label {label:?} must be a stable id token"),
            ));
        }
        let weight = finite(weight, &format!("position.{label}.weight"))?;
        if !(weight > 0.0) {
            return Err(err(
                TargetErrorKind::Position,
                format!("position.{label}.weight"),
                format!("weight must be > 0, got {weight}"),
            ));
        }
        if responses.is_empty() {
            return Err(err(
                TargetErrorKind::Missing,
                format!("position.{label}.responses"),
                "position has no raw responses",
            ));
        }
        for (index, response) in responses.iter().enumerate() {
            if responses[..index]
                .iter()
                .any(|existing| existing.role_id() == response.role_id())
            {
                return Err(err(
                    TargetErrorKind::Position,
                    format!("position.{label}.responses[{index}]"),
                    format!(
                        "duplicate participant {} at one position",
                        response.role_id()
                    ),
                ));
            }
        }
        Ok(Self {
            label,
            role,
            weight,
            responses,
        })
    }

    /// 从 T49 slots（[SlotPhase]）构造：按 slot 排序、slot 必须是 0..n-1、角色唯一。
    pub fn from_slot_phases(
        label: impl Into<String>,
        role: PositionRole,
        weight: f64,
        slots: &[SlotPhase],
    ) -> Result<Self, TargetError> {
        let mut ordered: Vec<&SlotPhase> = slots.iter().collect();
        ordered.sort_by_key(|slot| slot.slot);
        for (index, slot) in ordered.iter().enumerate() {
            if slot.slot != index as u32 {
                return Err(err(
                    TargetErrorKind::Position,
                    format!("position.slots[{index}]"),
                    format!(
                        "slot indices must be contiguous 0..n-1; found slot {} at index {index}",
                        slot.slot
                    ),
                ));
            }
        }
        let mut responses = Vec::with_capacity(ordered.len());
        for slot in ordered {
            responses.push(RawResponse::from_slot_phase(slot)?);
        }
        Self::new(label, role, weight, responses)
    }

    /// label。
    pub fn label(&self) -> &str {
        &self.label
    }

    /// design|validation。
    pub fn role(&self) -> PositionRole {
        self.role
    }

    /// 权重。
    pub fn weight(&self) -> f64 {
        self.weight
    }

    /// 全部 responses。
    pub fn responses(&self) -> &[RawResponse] {
        &self.responses
    }

    /// 按角色取。
    pub fn response(&self, role_id: &str) -> Option<&RawResponse> {
        self.responses
            .iter()
            .find(|response| response.role_id() == role_id)
    }
}

/// 一次 H_raw 输入（全部 position；participants/网格/采样率显式）。
#[derive(Debug, Clone, PartialEq)]
pub struct RawPlant {
    layout_id: String,
    sample_rate_hz: u32,
    participants: ModalParticipants,
    grid_hz: Vec<f64>,
    positions: Vec<RawPosition>,
}

impl RawPlant {
    /// 构造并校验（fs/网格/participants 显式、网格一致、每个 position 覆盖全部 participant、
    /// 物理极性已 exactly-once 归一化、至少 1 个 design position）。
    pub fn new(
        layout_id: impl Into<String>,
        sample_rate_hz: u32,
        participants: ModalParticipants,
        grid_hz: Vec<f64>,
        positions: Vec<RawPosition>,
    ) -> Result<Self, TargetError> {
        let layout_id = layout_id.into();
        if !sample_rate_ok(sample_rate_hz) {
            return Err(err(
                TargetErrorKind::SampleRate,
                "raw.sample_rate_hz",
                format!("sample rate {sample_rate_hz} is not a P1 rate {SAMPLE_RATES_HZ:?}"),
            ));
        }
        if layout_id != participants.layout_id() {
            return Err(err(
                TargetErrorKind::Participants,
                "raw.layout_id",
                format!(
                    "raw layout {layout_id:?} != participants layout {:?}",
                    participants.layout_id()
                ),
            ));
        }
        validate_frequency_grid(&grid_hz, sample_rate_hz, "raw.grid_hz")?;
        if positions.is_empty() {
            return Err(err(
                TargetErrorKind::Missing,
                "raw.positions",
                "raw plant has no positions",
            ));
        }
        for (index, position) in positions.iter().enumerate() {
            if positions[..index]
                .iter()
                .any(|existing| existing.label() == position.label())
            {
                return Err(err(
                    TargetErrorKind::Position,
                    format!("raw.positions[{index}]"),
                    format!("duplicate position label {}", position.label()),
                ));
            }
            if position.responses().len() != participants.m() {
                return Err(err(
                    TargetErrorKind::Position,
                    format!("raw.positions[{}]", position.label()),
                    format!(
                        "position has {} responses but participants has M={}",
                        position.responses().len(),
                        participants.m()
                    ),
                ));
            }
            for participant in participants.participants() {
                let response = position.response(&participant.role_id).ok_or_else(|| {
                    err(
                        TargetErrorKind::Position,
                        format!("raw.positions[{}]", position.label()),
                        format!(
                            "missing raw response for participant {}",
                            participant.role_id
                        ),
                    )
                })?;
                if response.out_index() != participant.out_index {
                    return Err(err(
                        TargetErrorKind::Role,
                        format!(
                            "raw.positions[{}].{}",
                            position.label(),
                            participant.role_id
                        ),
                        format!(
                            "response out_index {} != participant out_index {}",
                            response.out_index(),
                            participant.out_index
                        ),
                    ));
                }
                if !response.is_polarity_normalized() {
                    return Err(err(
                        TargetErrorKind::Polarity,
                        format!(
                            "raw.positions[{}].{}",
                            position.label(),
                            participant.role_id
                        ),
                        "physical polarity must be normalized exactly once before H_eff (unknown polarity must not be guessed)",
                    ));
                }
                validate_complex_grid(
                    response.points(),
                    &grid_hz,
                    &format!(
                        "raw.positions[{}].{}.points",
                        position.label(),
                        participant.role_id
                    ),
                )?;
            }
        }
        if !positions
            .iter()
            .any(|position| position.role() == PositionRole::Design)
        {
            return Err(err(
                TargetErrorKind::Position,
                "raw.positions",
                "at least one design position is required to build the target (holdout/validation positions never tune the target)",
            ));
        }
        Ok(Self {
            layout_id,
            sample_rate_hz,
            participants,
            grid_hz,
            positions,
        })
    }

    /// 采样率。
    pub fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    /// 布局 id。
    pub fn layout_id(&self) -> &str {
        &self.layout_id
    }

    /// participants。
    pub fn participants(&self) -> &ModalParticipants {
        &self.participants
    }

    /// 共同网格。
    pub fn grid_hz(&self) -> &[f64] {
        &self.grid_hz
    }

    /// 全部 position。
    pub fn positions(&self) -> &[RawPosition] {
        &self.positions
    }

    /// design position 迭代器。
    pub fn design_positions(&self) -> impl Iterator<Item = &RawPosition> {
        self.positions
            .iter()
            .filter(|position| position.role() == PositionRole::Design)
    }

    /// validation/holdout position 迭代器。
    pub fn validation_positions(&self) -> impl Iterator<Item = &RawPosition> {
        self.positions
            .iter()
            .filter(|position| position.role() == PositionRole::Validation)
    }

    /// validation/holdout 标签（只记录 reserved，不参与目标）。
    pub fn validation_labels(&self) -> Vec<String> {
        self.validation_positions()
            .map(|position| position.label().to_string())
            .collect()
    }
}

// ===========================================================================
// H_eff = H_raw * P（唯一复乘点）
// ===========================================================================

/// 唯一 H_eff 组合点：H_eff = H_raw x P（逐频点复数乘）。
fn compose_effective_response(
    raw: &[ComplexPoint],
    post: &[ComplexPoint],
) -> Result<Vec<ComplexPoint>, TargetError> {
    if raw.len() != post.len() {
        return Err(err(
            TargetErrorKind::Grid,
            "h_eff.points",
            format!(
                "H_raw has {} points but P_post has {} points",
                raw.len(),
                post.len()
            ),
        ));
    }
    let mut composed = Vec::with_capacity(raw.len());
    for (raw_point, post_point) in raw.iter().zip(post.iter()) {
        if !grid_freq_equal(raw_point.freq_hz, post_point.freq_hz) {
            return Err(err(
                TargetErrorKind::Grid,
                "h_eff.points",
                format!(
                    "grid mismatch at {} Hz vs {} Hz between H_raw and P_post",
                    raw_point.freq_hz, post_point.freq_hz
                ),
            ));
        }
        let (re, im) =
            complex_multiply((raw_point.re, raw_point.im), (post_point.re, post_point.im));
        composed.push(ComplexPoint {
            freq_hz: raw_point.freq_hz,
            re,
            im,
        });
    }
    Ok(composed)
}

/// 一个 participant 在某 position 的 H_eff（H_raw x P）。
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveInput {
    /// 角色 id。
    pub role_id: String,
    /// dense 索引。
    pub dense_index: u16,
    /// H_eff 复响应点。
    pub points: Vec<ComplexPoint>,
}

/// 一个 position 的 H_eff（全部 participant）。
#[derive(Debug, Clone, PartialEq)]
pub struct EffectivePosition {
    /// position label。
    pub label: String,
    /// design|validation。
    pub role: PositionRole,
    /// 权重。
    pub weight: f64,
    /// 按 participant dense 顺序。
    pub inputs: Vec<EffectiveInput>,
}

impl EffectivePosition {
    /// 按角色取 H_eff。
    pub fn input(&self, role_id: &str) -> Option<&EffectiveInput> {
        self.inputs.iter().find(|input| input.role_id == role_id)
    }

    /// participant 名（dense 顺序）。
    pub fn participant_names(&self) -> Vec<&str> {
        self.inputs
            .iter()
            .map(|input| input.role_id.as_str())
            .collect()
    }
}

/// 一次 H_eff 构建结果（plant；T50/T51 的 plant 身份）。
#[derive(Debug, Clone, PartialEq)]
pub struct EffectivePlant {
    sample_rate_hz: u32,
    layout_id: String,
    participants: ModalParticipants,
    baseline_hash: String,
    common_d0: CommonD0,
    grid_hz: Vec<f64>,
    positions: Vec<EffectivePosition>,
    warnings: Vec<TargetWarning>,
}

impl EffectivePlant {
    /// schema = [PLANT_SCHEMA]。
    pub const SCHEMA: &'static str = PLANT_SCHEMA;

    /// 构建 H_eff：逐 position/participant 复乘 H_raw x P；P 冻结 hash/网格/participants 必须一致。
    pub fn build(baseline: &FrozenPostBaseline, raw: &RawPlant) -> Result<Self, TargetError> {
        if baseline.sample_rate_hz() != raw.sample_rate_hz() {
            return Err(err(
                TargetErrorKind::SampleRate,
                "h_eff.sample_rate_hz",
                format!(
                    "P baseline fs {} != raw plant fs {}",
                    baseline.sample_rate_hz(),
                    raw.sample_rate_hz()
                ),
            ));
        }
        if baseline.participants().binding_hash() != raw.participants().binding_hash()
            || !grid_matches(baseline.grid_hz(), raw.grid_hz())
        {
            return Err(err(
                TargetErrorKind::Baseline,
                "h_eff.binding",
                "participants or frequency grid differ between frozen P baseline and H_raw plant; H_eff is undefined",
            ));
        }
        let mut warnings = Vec::new();
        let mut positions = Vec::with_capacity(raw.positions().len());
        for raw_position in raw.positions() {
            let mut inputs = Vec::with_capacity(raw.participants().m());
            for participant in raw.participants().participants() {
                let response = raw_position.response(&participant.role_id).ok_or_else(|| {
                    err(
                        TargetErrorKind::Position,
                        format!("h_eff.{}.{}", raw_position.label(), participant.role_id),
                        "missing raw response",
                    )
                })?;
                let post = baseline.p_grid(&participant.role_id, raw.grid_hz())?;
                let points = compose_effective_response(response.points(), &post)?;
                let mut near_zero = 0usize;
                for point in &points {
                    if magnitude_of(point) < NEAR_ZERO_MAGNITUDE {
                        near_zero += 1;
                    }
                }
                if near_zero > 0 {
                    push_warning(
                        &mut warnings,
                        TargetWarning::NEAR_ZERO_BASELINE,
                        format!(
                            "position {} participant {} has {} near-zero H_eff point(s); modality is not identifiable there (N/A)",
                            raw_position.label(),
                            participant.role_id,
                            near_zero
                        ),
                    );
                }
                inputs.push(EffectiveInput {
                    role_id: participant.role_id.clone(),
                    dense_index: participant.dense_index,
                    points,
                });
            }
            positions.push(EffectivePosition {
                label: raw_position.label().to_string(),
                role: raw_position.role(),
                weight: raw_position.weight(),
                inputs,
            });
        }
        if !positions
            .iter()
            .any(|position| position.role == PositionRole::Design)
        {
            return Err(err(
                TargetErrorKind::Position,
                "h_eff.positions",
                "no design position in H_eff plant",
            ));
        }
        Ok(Self {
            sample_rate_hz: baseline.sample_rate_hz(),
            layout_id: baseline.participants().layout_id().to_string(),
            participants: baseline.participants().clone(),
            baseline_hash: baseline.baseline_hash().to_string(),
            common_d0: baseline.common_d0(),
            grid_hz: raw.grid_hz().to_vec(),
            positions,
            warnings,
        })
    }

    /// 采样率。
    pub fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    /// 布局 id。
    pub fn layout_id(&self) -> &str {
        &self.layout_id
    }

    /// participants。
    pub fn participants(&self) -> &ModalParticipants {
        &self.participants
    }

    /// 绑定冻结 P 基线 hash。
    pub fn baseline_hash(&self) -> &str {
        &self.baseline_hash
    }

    /// 公共整数 D0 provenance。
    pub fn common_d0(&self) -> CommonD0 {
        self.common_d0
    }

    /// 共同网格。
    pub fn grid_hz(&self) -> &[f64] {
        &self.grid_hz
    }

    /// 全部 position。
    pub fn positions(&self) -> &[EffectivePosition] {
        &self.positions
    }

    /// 按 label 取 position。
    pub fn position(&self, label: &str) -> Option<&EffectivePosition> {
        self.positions
            .iter()
            .find(|position| position.label == label)
    }

    /// design position 迭代器。
    pub fn design_positions(&self) -> impl Iterator<Item = &EffectivePosition> {
        self.positions
            .iter()
            .filter(|position| position.role == PositionRole::Design)
    }

    /// validation/holdout position 迭代器（绝不参与目标构建）。
    pub fn validation_positions(&self) -> impl Iterator<Item = &EffectivePosition> {
        self.positions
            .iter()
            .filter(|position| position.role == PositionRole::Validation)
    }

    /// validation/holdout 标签。
    pub fn validation_labels(&self) -> Vec<String> {
        self.validation_positions()
            .map(|position| position.label.clone())
            .collect()
    }

    /// 按 position label + 角色取 H_eff。
    pub fn input(&self, position_label: &str, role_id: &str) -> Option<&EffectiveInput> {
        self.position(position_label)
            .and_then(|position| position.input(role_id))
    }

    /// 诊断。
    pub fn warnings(&self) -> &[TargetWarning] {
        &self.warnings
    }

    /// canonical 文本（确定性；供 hash/证据）。
    pub fn canonical(&self) -> String {
        let mut canonical = String::new();
        canonical.push_str(&format!(
            "h_eff fs={} layout={} baseline_hash={} common_d0_samples={} grid_points={}\n",
            self.sample_rate_hz,
            self.layout_id,
            self.baseline_hash,
            self.common_d0.samples(),
            self.grid_hz.len()
        ));
        for position in &self.positions {
            canonical.push_str(&format!(
                "position={} role={} weight={}\n",
                position.label,
                position.role.as_str(),
                fmt_f64(position.weight)
            ));
            for input in &position.inputs {
                canonical.push_str(&format!(
                    "input role={} dense={} points_sha256={}\n",
                    input.role_id,
                    input.dense_index,
                    points_sha256(&input.points)
                ));
            }
        }
        canonical
    }

    /// 绑定 hash。
    pub fn hash(&self) -> String {
        sha256_hex(format!("{H_EFF_HASH_DOMAIN}\n{}\n", self.canonical()).as_bytes())
    }
}

/// design-only plant 摘要（holdout/validation 数据不进入目标身份）。
fn design_plant_digest(plant: &EffectivePlant) -> String {
    let mut text = String::new();
    text.push_str(&format!(
        "design_plant fs={} layout={} baseline_hash={} grid_points={}\n",
        plant.sample_rate_hz(),
        plant.layout_id(),
        plant.baseline_hash(),
        plant.grid_hz().len()
    ));
    for position in plant.design_positions() {
        text.push_str(&format!(
            "position={} weight={}\n",
            position.label,
            fmt_f64(position.weight)
        ));
        for input in &position.inputs {
            text.push_str(&format!(
                "input role={} dense={} points_sha256={}\n",
                input.role_id,
                input.dense_index,
                points_sha256(&input.points)
            ));
        }
    }
    sha256_hex(text.as_bytes())
}

fn points_sha256(points: &[ComplexPoint]) -> String {
    let mut text = String::new();
    for point in points {
        text.push_str(&format!(
            "{};{};{}\n",
            fmt_f64(point.freq_hz),
            fmt_f64(point.re),
            fmt_f64(point.im)
        ));
    }
    sha256_hex(text.as_bytes())
}

// ===========================================================================
// 衰减目标（preserve / T*_60 = ratio * T60_baseline）
// ===========================================================================

/// 衰减目标模式。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DecayMode {
    /// 保持基线衰减（目标 = 实测基线；G 预测恒等）。
    Preserve,
    /// 目标衰减 T*_60 = ratio * T60_baseline（0.85/0.70/0.55 探索预设）。
    Reduced { ratio: f64 },
}

impl DecayMode {
    /// 衰减比（Preserve = 1.0）。
    pub fn ratio(self) -> f64 {
        match self {
            DecayMode::Preserve => 1.0,
            DecayMode::Reduced { ratio } => ratio,
        }
    }

    /// 稳定字面值。
    pub fn as_str(self) -> &'static str {
        match self {
            DecayMode::Preserve => "preserve",
            DecayMode::Reduced { .. } => "reduced",
        }
    }

    /// canonical 文本。
    pub fn canonical(self) -> String {
        match self {
            DecayMode::Preserve => "decay_mode=preserve ratio=1".to_string(),
            DecayMode::Reduced { ratio } => {
                format!("decay_mode=reduced ratio={}", fmt_f64(ratio))
            }
        }
    }
}

/// 一个 T20 判据频带的基线衰减（实测 T60；不发明）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandDecay {
    /// 1/3 oct 带中心（Hz；63/125 必需，40 可选）。
    pub band_hz: f64,
    /// 基线实测 T60（s）。
    pub baseline_t60_s: f64,
}

/// 衰减目标（T*_60 声明；T6 §4.5 的 >=15% 实测门由 T53/T68 判，本文件只声明目标）。
#[derive(Debug, Clone, PartialEq)]
pub struct DecayTarget {
    mode: DecayMode,
    bands: Vec<BandDecay>,
}

impl DecayTarget {
    /// preserve 目标（目标 T60 = 基线 T60；不宣称改善）。
    pub fn preserve(bands: Vec<BandDecay>) -> Result<Self, TargetError> {
        let target = Self {
            mode: DecayMode::Preserve,
            bands,
        };
        target.validate()?;
        Ok(target)
    }

    /// 减尾目标（ratio 属于 (0,1]；T*_60 = ratio * baseline）。
    pub fn reduced(ratio: f64, bands: Vec<BandDecay>) -> Result<Self, TargetError> {
        let ratio = finite(ratio, "decay.ratio")?;
        if ratio <= 0.0 || ratio > 1.0 {
            return Err(err(
                TargetErrorKind::Decay,
                "decay.ratio",
                format!("decay ratio {ratio} must be in (0, 1]"),
            ));
        }
        let target = Self {
            mode: DecayMode::Reduced { ratio },
            bands,
        };
        target.validate()?;
        Ok(target)
    }

    /// 校验（bands 唯一/有限/T60 物理范围；63/125 必需；比例合法）。
    pub fn validate(&self) -> Result<(), TargetError> {
        if self.bands.is_empty() {
            return Err(err(
                TargetErrorKind::Decay,
                "decay.bands",
                "decay target requires measured baseline T60 bands",
            ));
        }
        for (index, band) in self.bands.iter().enumerate() {
            let field = format!("decay.bands[{index}]");
            let band_hz = finite(band.band_hz, &format!("{field}.band_hz"))?;
            if band_hz <= 0.0 {
                return Err(err(
                    TargetErrorKind::Decay,
                    format!("{field}.band_hz"),
                    "band centre must be > 0",
                ));
            }
            finite_in_range(
                band.baseline_t60_s,
                T60_MIN_S,
                T60_MAX_S,
                &format!("{field}.baseline_t60_s"),
            )?;
            if self.bands[..index]
                .iter()
                .any(|existing| approx_equal(existing.band_hz, band.band_hz, 1.0e-6))
            {
                return Err(err(
                    TargetErrorKind::Decay,
                    format!("{field}.band_hz"),
                    format!("duplicate band {band_hz} Hz"),
                ));
            }
        }
        for required in T20_REQUIRED_BANDS_HZ {
            if self.baseline_t60_s(required).is_none() {
                return Err(err(
                    TargetErrorKind::Decay,
                    "decay.bands",
                    format!(
                        "measured baseline T60 for {required} Hz band is required (T6 §3.3/§4.5); refusing to invent decay"
                    ),
                ));
            }
        }
        let ratio = self.mode.ratio();
        if ratio <= 0.0 || ratio > 1.0 {
            return Err(err(
                TargetErrorKind::Decay,
                "decay.ratio",
                format!("decay ratio {ratio} must be in (0, 1]"),
            ));
        }
        if self.mode == DecayMode::Preserve && ratio != 1.0 {
            return Err(err(
                TargetErrorKind::Decay,
                "decay.ratio",
                "preserve mode requires ratio == 1.0",
            ));
        }
        Ok(())
    }

    /// 模式。
    pub fn mode(&self) -> DecayMode {
        self.mode
    }

    /// 频带。
    pub fn bands(&self) -> &[BandDecay] {
        &self.bands
    }

    /// 衰减比。
    pub fn ratio(&self) -> f64 {
        self.mode.ratio()
    }

    /// 是否 preserve。
    pub fn is_preserve(&self) -> bool {
        self.mode == DecayMode::Preserve
    }

    /// 指定频带基线 T60（s）。
    pub fn baseline_t60_s(&self, band_hz: f64) -> Option<f64> {
        self.bands
            .iter()
            .find(|band| approx_equal(band.band_hz, band_hz, 1.0e-6))
            .map(|band| band.baseline_t60_s)
    }

    /// 指定频带目标 T*_60（s）。
    pub fn target_t60_s(&self, band_hz: f64) -> Option<f64> {
        self.baseline_t60_s(band_hz)
            .map(|baseline| baseline * self.ratio())
    }

    /// T20 下降百分比（目标口径，非实测）。
    pub fn reduction_percent(&self) -> f64 {
        (1.0 - self.ratio()) * 100.0
    }

    /// 目标口径是否满足 T6 T20 >=15% 门（实测仍须 T53/T68 复算）。
    pub fn meets_t20_gate(&self) -> bool {
        !self.is_preserve() && self.ratio() <= DECAY_GATE_MAX_RATIO
    }

    /// canonical 文本。
    pub fn canonical(&self) -> String {
        let mut canonical = self.mode.canonical();
        canonical.push('\n');
        for band in &self.bands {
            canonical.push_str(&format!(
                "band={} baseline_t60_s={} target_t60_s={}\n",
                fmt_f64(band.band_hz),
                fmt_f64(band.baseline_t60_s),
                fmt_f64(band.baseline_t60_s * self.ratio())
            ));
        }
        canonical
    }
}

// ===========================================================================
// 目标 profile / 状态 / 构建选项
// ===========================================================================

/// 目标 profile（5x5 多声道 vs stereo strict 2x2；两者独立 hash domain，禁止互相复用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetProfile {
    /// P1 4.1.2 真实 5x5 低带 MIMO 目标。
    Multichannel4x2_5,
    /// stereo strict 受限 profile（FL/FR 2x2；modal_support_to_other_outputs=false）。
    StereoStrict2,
}

impl TargetProfile {
    /// 解析字面值。
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "multichannel_4_1_2_5" => Some(TargetProfile::Multichannel4x2_5),
            "stereo_strict_2" => Some(TargetProfile::StereoStrict2),
            _ => None,
        }
    }

    /// 稳定字面值。
    pub const fn as_str(self) -> &'static str {
        match self {
            TargetProfile::Multichannel4x2_5 => "multichannel_4_1_2_5",
            TargetProfile::StereoStrict2 => "stereo_strict_2",
        }
    }

    /// 目标矩阵维度 M。
    pub const fn m(self) -> usize {
        match self {
            TargetProfile::Multichannel4x2_5 => 5,
            TargetProfile::StereoStrict2 => 2,
        }
    }

    /// 目标矩阵 hash domain（5x5 与 stereo strict 分离）。
    pub const fn hash_domain(self) -> &'static str {
        match self {
            TargetProfile::Multichannel4x2_5 => TARGET_HASH_DOMAIN,
            TargetProfile::StereoStrict2 => STEREO_TARGET_HASH_DOMAIN,
        }
    }

    /// stereo strict 是否允许模态支援到其它输出（硬约束：false）。
    pub const fn modal_support_to_other_outputs(self) -> bool {
        match self {
            TargetProfile::Multichannel4x2_5 => true,
            TargetProfile::StereoStrict2 => false,
        }
    }
}

/// 目标状态（诚实字段：只表达目标可行性/覆盖率，不表达实测改善）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetStatus {
    /// preserve 模式：目标 = 实测基线，不宣称任何减尾。
    Preserve,
    /// 目标可行（所需修正均在软帽内）；predicted only，未应用/未实测。
    PredictedPass,
    /// 目标部分可行（存在超出软帽但未超硬上限的修正）。
    PredictedPartial,
    /// 目标不可行（所需修正超硬上限；不得放宽帽来伪装）。
    Infeasible,
    /// 覆盖/输入不足，不能给出目标（不删点改门）。
    Incomplete,
}

impl TargetStatus {
    /// 稳定字面值。
    pub const fn as_str(self) -> &'static str {
        match self {
            TargetStatus::Preserve => "preserve",
            TargetStatus::PredictedPass => "predicted_pass",
            TargetStatus::PredictedPartial => "predicted_partial",
            TargetStatus::Infeasible => "infeasible",
            TargetStatus::Incomplete => "incomplete",
        }
    }
}

/// 目标构建选项（冻结后不得测后改；FC-16）。
#[derive(Debug, Clone, PartialEq)]
pub struct TargetOptions {
    /// 分数倍频程平滑（默认 1/6 oct；[MIN_SMOOTHING_OCTAVE, MAX_SMOOTHING_OCTAVE]）。
    pub smoothing_octave: f64,
    /// 近零 H_eff 幅度 floor（线性；低于此值视为 N/A）。
    pub near_zero_floor: f64,
    /// 是否强制 P1 显式 participants（生产必须 true；负向夹具可关闭）。
    pub require_explicit_participants: bool,
}

impl Default for TargetOptions {
    fn default() -> Self {
        Self {
            smoothing_octave: DEFAULT_SMOOTHING_OCTAVE,
            near_zero_floor: NEAR_ZERO_MAGNITUDE,
            require_explicit_participants: true,
        }
    }
}

impl TargetOptions {
    /// 校验。
    pub fn validate(&self) -> Result<(), TargetError> {
        finite_in_range(
            self.smoothing_octave,
            MIN_SMOOTHING_OCTAVE,
            MAX_SMOOTHING_OCTAVE,
            "target_options.smoothing_octave",
        )?;
        let floor = finite(self.near_zero_floor, "target_options.near_zero_floor")?;
        if floor <= 0.0 {
            return Err(err(
                TargetErrorKind::Range,
                "target_options.near_zero_floor",
                "near-zero floor must be > 0",
            ));
        }
        Ok(())
    }

    /// canonical 文本。
    pub fn canonical(&self) -> String {
        format!(
            "target_options smoothing_octave={} near_zero_floor={} require_explicit_participants={}",
            fmt_f64(self.smoothing_octave),
            fmt_f64(self.near_zero_floor),
            self.require_explicit_participants
        )
    }
}

// ===========================================================================
// 目标规格（冻结声明；不包含任何 target 侧 tone 应用入口）
// ===========================================================================

/// 目标矩阵构建规格（tone 从冻结基线**继承**；残留 curve 声明一律拒收）。
#[derive(Debug, Clone, PartialEq)]
pub struct TargetSpec {
    target_id: String,
    profile: TargetProfile,
    participants: ModalParticipants,
    decay: DecayTarget,
    tone: ToneBinding,
    tone_applications: ToneApplicationProof,
    curve: CurveBinding,
    residual_curve: Option<CurveBinding>,
    options: TargetOptions,
    common_d0: CommonD0,
    target_delay_samples: u32,
}

impl TargetSpec {
    /// 从冻结 P 基线派生（profile 由显式 participants 决定；tone/curve/D0 继承基线）。
    pub fn for_baseline(
        baseline: &FrozenPostBaseline,
        decay: DecayTarget,
    ) -> Result<Self, TargetError> {
        let participants = baseline.participants().clone();
        let profile = if participants.is_p1_explicit() {
            TargetProfile::Multichannel4x2_5
        } else if participants.is_stereo_strict() {
            TargetProfile::StereoStrict2
        } else {
            return Err(err(
                TargetErrorKind::Unsupported,
                "target.participants",
                format!(
                    "participants {:?} match neither the explicit 5x5 4.1.2 scope nor the stereo strict FL/FR scope (5.3.2/M=8 is NOT_VERIFIED)",
                    participants.role_ids()
                ),
            ));
        };
        Ok(Self {
            target_id: format!("{}-target", baseline.baseline_id()),
            profile,
            participants,
            decay,
            tone: baseline.tone().clone(),
            tone_applications: baseline.tone_applications(),
            curve: baseline.curve().clone(),
            residual_curve: None,
            options: TargetOptions::default(),
            common_d0: baseline.common_d0(),
            target_delay_samples: 0,
        })
    }

    /// stereo strict 独立 profile（必须由 stereo strict 专用 P 基线派生，禁止从 5x5 基线 runtime mask）。
    pub fn stereo_strict(
        baseline: &FrozenPostBaseline,
        decay: DecayTarget,
    ) -> Result<Self, TargetError> {
        if !baseline.participants().is_stereo_strict() {
            return Err(err(
                TargetErrorKind::Stereo,
                "target.stereo.participants",
                "stereo strict target requires its own constrained P baseline (FL/FR); reusing/masking the 5x5 baseline would invalidate predictions",
            ));
        }
        Ok(Self {
            target_id: format!("{}-stereo-target", baseline.baseline_id()),
            profile: TargetProfile::StereoStrict2,
            participants: baseline.participants().clone(),
            decay,
            tone: baseline.tone().clone(),
            tone_applications: baseline.tone_applications(),
            curve: baseline.curve().clone(),
            residual_curve: None,
            options: TargetOptions::default(),
            common_d0: baseline.common_d0(),
            target_delay_samples: 0,
        })
    }

    /// 目标 id。
    pub fn target_id(&self) -> &str {
        &self.target_id
    }

    /// profile。
    pub fn profile(&self) -> TargetProfile {
        self.profile
    }

    /// participants。
    pub fn participants(&self) -> &ModalParticipants {
        &self.participants
    }

    /// 衰减目标。
    pub fn decay(&self) -> &DecayTarget {
        &self.decay
    }

    /// tone（继承基线）。
    pub fn tone(&self) -> &ToneBinding {
        &self.tone
    }

    /// tone 应用次数声明（1/0/0）。
    pub fn tone_applications(&self) -> ToneApplicationProof {
        self.tone_applications
    }

    /// curve（继承基线）。
    pub fn curve(&self) -> &CurveBinding {
        &self.curve
    }

    /// 残留 curve 声明（必须 None；用于负向拒收）。
    pub fn residual_curve(&self) -> Option<&CurveBinding> {
        self.residual_curve.as_ref()
    }

    /// 选项。
    pub fn options(&self) -> &TargetOptions {
        &self.options
    }

    /// 公共 D0。
    pub fn common_d0(&self) -> CommonD0 {
        self.common_d0
    }

    /// 目标附加延迟（samples@fs；D0-removed 平面上的**额外**声明延迟，默认 0 = 零多余相位）。
    pub fn target_delay_samples(&self) -> u32 {
        self.target_delay_samples
    }

    /// 设置目标附加延迟（samples；上界在 validate 时按 P 基线 fs 复校，见 T6 FC-04/FC-09）。
    pub fn with_target_delay_samples(mut self, target_delay_samples: u32) -> Self {
        self.target_delay_samples = target_delay_samples;
        self
    }

    /// 设置目标 id（稳定 token）。
    pub fn with_target_id(mut self, target_id: impl Into<String>) -> Result<Self, TargetError> {
        let target_id = target_id.into();
        if !is_id_token(&target_id, 96) {
            return Err(err(
                TargetErrorKind::Matrix,
                "target.target_id",
                format!("target_id {target_id:?} must be a stable id token"),
            ));
        }
        self.target_id = target_id;
        Ok(self)
    }

    /// 设置选项。
    pub fn with_options(mut self, options: TargetOptions) -> Result<Self, TargetError> {
        options.validate()?;
        self.options = options;
        Ok(self)
    }

    /// 声明一个残留 curve（**非法输入**：构建时拒收，证明 G/target 不重复应用 tone）。
    pub fn with_residual_curve(mut self, residual_curve: CurveBinding) -> Self {
        self.residual_curve = Some(residual_curve);
        self
    }

    /// 校验规格与冻结基线一致性（tone 单次/curve 只在基线/D0 同一/participants 一致）。
    pub fn validate(&self, baseline: &FrozenPostBaseline) -> Result<(), TargetError> {
        if !is_id_token(&self.target_id, 96) {
            return Err(err(
                TargetErrorKind::Matrix,
                "target.target_id",
                format!("target_id {:?} must be a stable id token", self.target_id),
            ));
        }
        if let Some(residual) = &self.residual_curve {
            return Err(err(
                TargetErrorKind::Tone,
                "target.residual_curve",
                format!(
                    "residual curve {:?} declared at {:?}; baseline tone must be applied once and G must be modal-residual-only (no second tone filter)",
                    residual.curve_id(),
                    residual.site().as_str()
                ),
            ));
        }
        if self.profile == TargetProfile::StereoStrict2 {
            if !self.participants.is_stereo_strict() || !baseline.participants().is_stereo_strict()
            {
                return Err(err(
                    TargetErrorKind::Stereo,
                    "target.participants",
                    "stereo strict profile requires the constrained FL/FR participants scope on both target spec and P baseline",
                ));
            }
        } else if !self.participants.is_p1_explicit() || !baseline.participants().is_p1_explicit() {
            return Err(err(
                TargetErrorKind::Participants,
                "target.participants",
                "multichannel profile requires the explicit 5x5 4.1.2 participants scope on both target spec and P baseline",
            ));
        }
        if self.participants.binding_hash() != baseline.participants().binding_hash() {
            return Err(err(
                TargetErrorKind::Participants,
                "target.participants",
                "target participants binding hash differs from the frozen P baseline (no runtime re-masking)",
            ));
        }
        if self.participants.m() != self.profile.m() {
            return Err(err(
                TargetErrorKind::Matrix,
                "target.participants",
                format!(
                    "profile {} requires M={} participants, got {}",
                    self.profile.as_str(),
                    self.profile.m(),
                    self.participants.m()
                ),
            ));
        }
        self.tone_applications.validate()?;
        self.curve.validate_single_application()?;
        baseline.verify_single_tone_application()?;
        if !self.tone.same_definition(baseline.tone()) {
            return Err(err(
                TargetErrorKind::Tone,
                "target.tone",
                "target tone binding must inherit (not redefine) the frozen baseline tone",
            ));
        }
        if self.curve.curve_id() != baseline.curve().curve_id()
            || self.curve.target_sha256() != baseline.curve().target_sha256()
        {
            return Err(err(
                TargetErrorKind::Curve,
                "target.curve",
                "target curve binding must equal the frozen baseline curve binding (curve_id is a binding, not a re-applied filter)",
            ));
        }
        self.decay.validate()?;
        self.options.validate()?;
        let delay_limit = baseline.sample_rate_hz() as f64 * POST_DELAY_MAX_MS / 1000.0;
        if self.target_delay_samples as f64 > delay_limit {
            return Err(err(
                TargetErrorKind::Range,
                "target.target_delay_samples",
                format!(
                    "target delay {} samples exceeds {POST_DELAY_MAX_MS} ms ({delay_limit} samples at {} Hz)",
                    self.target_delay_samples,
                    baseline.sample_rate_hz()
                ),
            ));
        }
        if self.common_d0 != baseline.common_d0() {
            return Err(err(
                TargetErrorKind::Baseline,
                "target.common_d0",
                format!(
                    "target D0 {} differs from frozen baseline D0 {} (T6 FC-04 requires one common D0)",
                    self.common_d0.samples(),
                    baseline.common_d0().samples()
                ),
            ));
        }
        Ok(())
    }
}

// ===========================================================================
// 目标矩阵（对角目标；非对角结构零）
// ===========================================================================

/// 一个目标 cell（输入 i → 输出 j）。目标矩阵只存对角 cells；非对角结构零。
#[derive(Debug, Clone, PartialEq)]
pub struct TargetCell {
    /// None = position-invariant（reduced 模式）；Some(label) = per-position（preserve 模式）。
    pub position: Option<String>,
    /// 输出 dense 索引（G 的行）。
    pub output_dense: u16,
    /// 输入 dense 索引（G 的列）。
    pub input_dense: u16,
    /// 输出角色 id。
    pub output_role: String,
    /// 输入角色 id。
    pub input_role: String,
    /// 目标复响应 D_{j,i}(f)。
    pub points: Vec<ComplexPoint>,
}

/// 目标 cell 视图（未存储的非对角 = Zero）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TargetCellView<'a> {
    /// 结构零（无交叉馈送目标）。
    Zero,
    /// 目标复响应。
    Response(&'a [ComplexPoint]),
}

/// 目标构建诊断（只报告，不冒充实测改善）。
#[derive(Debug, Clone, PartialEq)]
pub struct TargetDiagnostics {
    /// 模态带内所需修正最大值（dB；|target/measured|，目标可行性代理）。
    pub required_correction_max_db: f64,
    /// 最坏 (position, role, correction_db)。
    pub required_correction_worst: Option<(String, String, f64)>,
    /// 改善带内相对目标的偏差 RMS（dB；测量模态纹波）。
    pub ripple_before_rms_db: f64,
    /// 改善带内最大偏差（dB）。
    pub ripple_before_max_db: f64,
    /// 改善带网格覆盖率（T6 §3.3 >=80%）。
    pub improvement_band_coverage: f64,
    /// 模态校正带网格覆盖率。
    pub modal_band_coverage: f64,
    /// 过渡带网格覆盖率（仅报告）。
    pub transition_band_coverage: f64,
    /// design position 数。
    pub design_positions: usize,
    /// validation/holdout 标签（reserved；绝不参与目标）。
    pub validation_positions_reserved: Vec<String>,
    /// 近零 H_eff 点数（N/A 候选）。
    pub near_zero_points: usize,
}

/// 目标矩阵 + 冻结绑定 + 诊断 + 状态（T50 唯一交付结构）。
#[derive(Debug, Clone, PartialEq)]
pub struct TargetMatrix {
    schema: &'static str,
    profile: TargetProfile,
    target_id: String,
    sample_rate_hz: u32,
    layout_id: String,
    participants: ModalParticipants,
    baseline_hash: String,
    plant_hash: String,
    design_plant_hash: String,
    common_d0: CommonD0,
    tone: ToneBinding,
    tone_applications: ToneApplicationProof,
    curve: CurveBinding,
    decay: DecayTarget,
    options: TargetOptions,
    target_delay_samples: u32,
    status: TargetStatus,
    grid_hz: Vec<f64>,
    cells: Vec<TargetCell>,
    diagnostics: TargetDiagnostics,
    height_scope: Option<FullRangeHeightScope>,
    output_scope: ScopedOutputs,
    warnings: Vec<TargetWarning>,
}

impl TargetMatrix {
    /// schema = [TARGET_SCHEMA]。
    pub const SCHEMA: &'static str = TARGET_SCHEMA;

    /// schema 字面值。
    pub fn schema(&self) -> &'static str {
        self.schema
    }

    /// profile。
    pub fn profile(&self) -> TargetProfile {
        self.profile
    }

    /// 目标 id。
    pub fn target_id(&self) -> &str {
        &self.target_id
    }

    /// 采样率。
    pub fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    /// 布局 id。
    pub fn layout_id(&self) -> &str {
        &self.layout_id
    }

    /// participants。
    pub fn participants(&self) -> &ModalParticipants {
        &self.participants
    }

    /// M。
    pub fn m(&self) -> usize {
        self.participants.m()
    }

    /// 冻结 P 基线 hash。
    pub fn baseline_hash(&self) -> &str {
        &self.baseline_hash
    }

    /// H_eff plant hash（provenance；含全部 position，包括 holdout）。
    pub fn plant_hash(&self) -> &str {
        &self.plant_hash
    }

    /// design-only H_eff 摘要（目标身份；holdout 数据不进入）。
    pub fn design_plant_hash(&self) -> &str {
        &self.design_plant_hash
    }

    /// 公共整数 D0。
    pub fn common_d0(&self) -> CommonD0 {
        self.common_d0
    }

    /// tone（继承基线）。
    pub fn tone(&self) -> &ToneBinding {
        &self.tone
    }

    /// tone 应用次数声明（1/0/0）。
    pub fn tone_applications(&self) -> ToneApplicationProof {
        self.tone_applications
    }

    /// curve 绑定。
    pub fn curve(&self) -> &CurveBinding {
        &self.curve
    }

    /// 衰减目标。
    pub fn decay(&self) -> &DecayTarget {
        &self.decay
    }

    /// 选项。
    pub fn options(&self) -> &TargetOptions {
        &self.options
    }

    /// 目标附加延迟（samples@fs；D0-removed 平面上的声明值，默认 0）。
    pub fn target_delay_samples(&self) -> u32 {
        self.target_delay_samples
    }

    /// 状态（只表达可行性/覆盖率）。
    pub fn status(&self) -> TargetStatus {
        self.status
    }

    /// 网格。
    pub fn grid_hz(&self) -> &[f64] {
        &self.grid_hz
    }

    /// 全部 cells（对角）。
    pub fn cells(&self) -> &[TargetCell] {
        &self.cells
    }

    /// 诊断。
    pub fn diagnostics(&self) -> &TargetDiagnostics {
        &self.diagnostics
    }

    /// 高度全频 scope（multichannel 必有；stereo strict 可无）。
    pub fn height_scope(&self) -> Option<&FullRangeHeightScope> {
        self.height_scope.as_ref()
    }

    /// 输出 scope 表。
    pub fn output_scope(&self) -> &ScopedOutputs {
        &self.output_scope
    }

    /// 诊断。
    pub fn warnings(&self) -> &[TargetWarning] {
        &self.warnings
    }

    /// 是否 per-position（preserve 模式）。
    pub fn is_position_variant(&self) -> bool {
        self.cells.iter().any(|cell| cell.position.is_some())
    }

    /// 诚实状态字段：产物未应用。
    pub fn applied(&self) -> bool {
        false
    }

    /// 诚实状态字段：未请求应用后实测。
    pub fn post_apply_measurement(&self) -> &'static str {
        "not_requested"
    }

    /// 诚实状态字段：引擎兼容性未验证。
    pub fn engine_compatibility(&self) -> &'static str {
        "not_validated"
    }

    /// 诚实状态字段：运行期未验证。
    pub fn runtime_validation(&self) -> &'static str {
        "not_run"
    }

    /// 目标 cell 查询（非对角恒 Zero）。
    pub fn cell(
        &self,
        position: Option<&str>,
        output_dense: u16,
        input_dense: u16,
    ) -> TargetCellView<'_> {
        if output_dense != input_dense {
            return TargetCellView::Zero;
        }
        if self.is_position_variant() {
            let position = match position {
                Some(position) => position,
                None => return TargetCellView::Zero,
            };
            self.cells
                .iter()
                .find(|cell| {
                    cell.position.as_deref() == Some(position)
                        && cell.output_dense == output_dense
                        && cell.input_dense == input_dense
                })
                .map(|cell| TargetCellView::Response(cell.points.as_slice()))
                .unwrap_or(TargetCellView::Zero)
        } else {
            if position.is_some() {
                return TargetCellView::Zero;
            }
            self.cells
                .iter()
                .find(|cell| {
                    cell.position.is_none()
                        && cell.output_dense == output_dense
                        && cell.input_dense == input_dense
                })
                .map(|cell| TargetCellView::Response(cell.points.as_slice()))
                .unwrap_or(TargetCellView::Zero)
        }
    }

    /// 对角目标响应（position 语义同 [TargetMatrix::cell]）。
    pub fn diag_response(&self, position: Option<&str>, dense: u16) -> Option<&[ComplexPoint]> {
        match self.cell(position, dense, dense) {
            TargetCellView::Response(points) => Some(points),
            TargetCellView::Zero => None,
        }
    }

    /// holdout 隔离自检：目标 cells 不得引用任何 validation 标签。
    pub fn verify_holdout_isolated(&self) -> bool {
        self.cells.iter().all(|cell| {
            cell.position
                .as_deref()
                .map(|position| {
                    !self
                        .diagnostics
                        .validation_positions_reserved
                        .iter()
                        .any(|label| label == position)
                })
                .unwrap_or(true)
        })
    }

    /// tone 单次应用自检。
    pub fn verify_tone_single_application(&self) -> Result<(), TargetError> {
        self.tone_applications.validate()?;
        self.curve.validate_single_application()?;
        if self.tone.curve_id() != self.curve.curve_id() {
            return Err(err(
                TargetErrorKind::Tone,
                "target.tone",
                "target tone curve id differs from the target curve binding",
            ));
        }
        Ok(())
    }

    /// 结构自检：对角 cells 完整（每 participant 至少 1 个 cell；preserve 必须覆盖全部 design position）。
    pub fn verify_structure(&self) -> Result<(), TargetError> {
        for participant in self.participants.participants() {
            let count = self
                .cells
                .iter()
                .filter(|cell| cell.output_dense == participant.dense_index)
                .count();
            if count == 0 {
                return Err(err(
                    TargetErrorKind::Matrix,
                    format!("target.cells[{}]", participant.role_id),
                    "missing diagonal target cell for participant",
                ));
            }
            if self.is_position_variant() && count != self.diagnostics.design_positions {
                return Err(err(
                    TargetErrorKind::Matrix,
                    format!("target.cells[{}]", participant.role_id),
                    format!(
                        "preserve target must have one cell per design position; {} cells vs {} positions",
                        count, self.diagnostics.design_positions
                    ),
                ));
            }
            if !self.is_position_variant() && count != 1 {
                return Err(err(
                    TargetErrorKind::Matrix,
                    format!("target.cells[{}]", participant.role_id),
                    format!(
                        "position-invariant target must have exactly one cell per participant; found {count}"
                    ),
                ));
            }
        }
        Ok(())
    }

    /// canonical 文本（确定性；供 hash）。
    pub fn canonical(&self) -> String {
        let mut canonical = String::new();
        canonical.push_str(&format!("{}\n", self.profile.hash_domain()));
        canonical.push_str(&format!(
            "schema={} version=1 profile={} target_id={} status={} target_delay_samples={}\n",
            self.schema,
            self.profile.as_str(),
            self.target_id,
            self.status.as_str(),
            self.target_delay_samples
        ));
        canonical.push_str(&format!(
            "fs={} layout={} baseline_hash={} design_plant_hash={} common_d0_samples={}\n",
            self.sample_rate_hz,
            self.layout_id,
            self.baseline_hash,
            self.design_plant_hash,
            self.common_d0.samples()
        ));
        canonical.push_str(&self.participants.canonical());
        canonical.push_str(&format!("{}\n", self.tone.canonical()));
        canonical.push_str(&format!("{}\n", self.tone_applications.canonical()));
        canonical.push_str(&format!("{}\n", self.curve.canonical()));
        canonical.push_str(&self.decay.canonical());
        canonical.push_str(&format!("{}\n", self.options.canonical()));
        canonical.push_str(&format!(
            "target_delay_samples={}\n",
            self.target_delay_samples
        ));
        canonical.push_str(&format!(
            "grid points={} sha256={}\n",
            self.grid_hz.len(),
            grid_sha256(&self.grid_hz)
        ));
        canonical.push_str(&format!(
            "diagnostics required_correction_max_db={} ripple_before_rms_db={} ripple_before_max_db={} improvement_coverage={} modal_coverage={} transition_coverage={} design_positions={} near_zero_points={}\n",
            fmt_f64(self.diagnostics.required_correction_max_db),
            fmt_f64(self.diagnostics.ripple_before_rms_db),
            fmt_f64(self.diagnostics.ripple_before_max_db),
            fmt_f64(self.diagnostics.improvement_band_coverage),
            fmt_f64(self.diagnostics.modal_band_coverage),
            fmt_f64(self.diagnostics.transition_band_coverage),
            self.diagnostics.design_positions,
            self.diagnostics.near_zero_points
        ));
        for label in &self.diagnostics.validation_positions_reserved {
            canonical.push_str(&format!("holdout_reserved={label}\n"));
        }
        if let Some(height_scope) = &self.height_scope {
            canonical.push_str(&height_scope.canonical());
        }
        canonical.push_str(&self.output_scope.canonical());
        for cell in &self.cells {
            canonical.push_str(&format!(
                "cell position={} out={} in={} role={} input_role={} points_sha256={}\n",
                cell.position.as_deref().unwrap_or("none"),
                cell.output_dense,
                cell.input_dense,
                cell.output_role,
                cell.input_role,
                points_sha256(&cell.points)
            ));
        }
        for warning in &self.warnings {
            canonical.push_str(&format!("warning={}\n", warning.code));
        }
        canonical
    }

    /// 目标 hash（profile 独立 domain；确定性）。
    pub fn target_sha256(&self) -> String {
        sha256_hex(self.canonical().as_bytes())
    }
}

fn grid_sha256(grid_hz: &[f64]) -> String {
    let mut text = String::new();
    for freq in grid_hz {
        text.push_str(&fmt_f64(*freq));
        text.push('\n');
    }
    sha256_hex(text.as_bytes())
}

// ===========================================================================
// 目标矩阵构建（design positions only；holdout 绝不参与）
// ===========================================================================

/// 构建 5x5 / stereo strict 目标矩阵（按 spec.profile 分派；design positions only）。
pub fn build_target_matrix(
    baseline: &FrozenPostBaseline,
    plant: &EffectivePlant,
    spec: &TargetSpec,
) -> Result<TargetMatrix, TargetError> {
    spec.validate(baseline)?;
    if plant.baseline_hash() != baseline.baseline_hash() {
        return Err(err(
            TargetErrorKind::Baseline,
            "target.baseline_hash",
            "H_eff plant was built from a different frozen P baseline; target is undefined",
        ));
    }
    if plant.sample_rate_hz() != baseline.sample_rate_hz() {
        return Err(err(
            TargetErrorKind::SampleRate,
            "target.sample_rate_hz",
            format!(
                "plant fs {} != baseline fs {}",
                plant.sample_rate_hz(),
                baseline.sample_rate_hz()
            ),
        ));
    }
    if plant.participants().binding_hash() != baseline.participants().binding_hash() {
        return Err(err(
            TargetErrorKind::Participants,
            "target.participants",
            "plant participants differ from the frozen P baseline participants",
        ));
    }
    if !grid_matches(plant.grid_hz(), baseline.grid_hz()) {
        return Err(err(
            TargetErrorKind::Grid,
            "target.grid_hz",
            "plant grid differs from the frozen P baseline grid",
        ));
    }

    let grid_hz = plant.grid_hz().to_vec();
    let options = spec.options().clone();
    options.validate()?;
    let decay = spec.decay().clone();
    let design_positions: Vec<&EffectivePosition> = plant.design_positions().collect();
    if design_positions.is_empty() {
        return Err(err(
            TargetErrorKind::Position,
            "target.design_positions",
            "at least one design position is required to build the target",
        ));
    }

    let mut warnings = Vec::new();
    if design_positions.len() < 3 {
        push_warning(
            &mut warnings,
            TargetWarning::FEW_DESIGN_POSITIONS,
            format!(
                "only {} design position(s); T6 formal design >=6 from T67, target is provisional",
                design_positions.len()
            ),
        );
    }
    let validation_positions_reserved = plant.validation_labels();
    if !validation_positions_reserved.is_empty() {
        push_warning(
            &mut warnings,
            TargetWarning::HOLDOUT_RESERVED,
            format!(
                "{} validation/holdout position(s) reserved and NOT used for target construction: {:?}",
                validation_positions_reserved.len(),
                validation_positions_reserved
            ),
        );
    }
    if options.smoothing_octave > 2.0 * IMPROVEMENT_BAND_HZ.0 / IMPROVEMENT_BAND_HZ.1 {
        push_warning(
            &mut warnings,
            TargetWarning::WIDE_SMOOTHING,
            format!(
                "smoothing octave {} is wide relative to the improvement band; target may be over-smoothed",
                options.smoothing_octave
            ),
        );
    }
    if !decay.is_preserve() && decay.ratio() < DECAY_RATIO_PRESETS[2] {
        push_warning(
            &mut warnings,
            TargetWarning::AGGRESSIVE_DECAY,
            format!(
                "decay ratio {} is below the explored preset minimum {}; feasibility is not guaranteed",
                decay.ratio(),
                DECAY_RATIO_PRESETS[2]
            ),
        );
    }

    let improvement_band_coverage =
        band_coverage(&grid_hz, IMPROVEMENT_BAND_HZ.0, IMPROVEMENT_BAND_HZ.1);
    let modal_band_coverage = band_coverage(
        &grid_hz,
        MODAL_CORRECTION_BAND_HZ.0,
        MODAL_CORRECTION_BAND_HZ.1,
    );
    let transition_band_coverage =
        band_coverage(&grid_hz, TRANSITION_BAND_HZ.0, TRANSITION_BAND_HZ.1);
    if improvement_band_coverage < IMPROVEMENT_BAND_MIN_COVERAGE {
        push_warning(
            &mut warnings,
            TargetWarning::TRANSITION_BAND_GAP,
            format!(
                "improvement band coverage {:.3} below {:.2} (T6 §3.3); target status becomes incomplete",
                improvement_band_coverage, IMPROVEMENT_BAND_MIN_COVERAGE
            ),
        );
    }
    if transition_band_coverage < IMPROVEMENT_BAND_MIN_COVERAGE {
        push_warning(
            &mut warnings,
            TargetWarning::TRANSITION_BAND_GAP,
            format!(
                "transition band (300-400 Hz) coverage {:.3} is reported only; guard band report will be sparse",
                transition_band_coverage
            ),
        );
    }

    let preserve = decay.is_preserve();
    if !preserve {
        push_warning(
            &mut warnings,
            TargetWarning::POSITION_INVARIANT_TARGET,
            "reduced-decay target is position-invariant (power-weighted design aggregation); per-position ripple remains a residual".to_string(),
        );
    }

    // 保护带限与模态校正带重叠检查（仅告警；目标只应落在保护通带内）。
    for role in baseline.roles() {
        if let BandLimit::LowPass { freq_hz } = role.band_limit() {
            if freq_hz < MODAL_CORRECTION_BAND_HZ.1 {
                push_warning(
                    &mut warnings,
                    TargetWarning::PROTECTION_OVERLAP,
                    format!(
                        "role {} protection low-pass {freq_hz} Hz overlaps the modal correction band; correction above it is attenuated by protection",
                        role.role_id()
                    ),
                );
            }
        }
    }
    if grid_hz.len() > 2048 {
        push_warning(
            &mut warnings,
            TargetWarning::LARGE_GRID,
            format!(
                "grid has {} points; target construction is O(positions * M * points * window)",
                grid_hz.len()
            ),
        );
    }

    let mut cells: Vec<TargetCell> = Vec::new();
    let mut required_correction_max_db = 0.0f64;
    let mut required_correction_worst: Option<(String, String, f64)> = None;
    let mut ripple_sq_sum = 0.0f64;
    let mut ripple_count = 0usize;
    let mut ripple_max = 0.0f64;
    let mut near_zero_points = 0usize;

    for participant in baseline.participants().participants() {
        let mut measured: Vec<(&EffectivePosition, &[ComplexPoint])> =
            Vec::with_capacity(design_positions.len());
        for position in &design_positions {
            let input = position.input(&participant.role_id).ok_or_else(|| {
                err(
                    TargetErrorKind::Missing,
                    format!("h_eff.{}.{}", position.label, participant.role_id),
                    "design position has no H_eff for this participant",
                )
            })?;
            measured.push((position, input.points.as_slice()));
        }
        if preserve {
            // preserve：目标 = 该 position 的实测 H_eff（G 预测恒等；不宣称减尾）。
            for (position, points) in &measured {
                cells.push(TargetCell {
                    position: Some(position.label.clone()),
                    output_dense: participant.dense_index,
                    input_dense: participant.dense_index,
                    output_role: participant.role_id.clone(),
                    input_role: participant.role_id.clone(),
                    points: points.to_vec(),
                });
            }
        } else {
            // reduced：per-position 功率平滑 -> design position 功率加权聚合（position-invariant D）。
            let smoothed: Vec<Vec<ComplexPoint>> = measured
                .iter()
                .map(|(_, points)| smooth_complex_response(points, options.smoothing_octave))
                .collect();
            let response_refs: Vec<&[ComplexPoint]> =
                smoothed.iter().map(|points| points.as_slice()).collect();
            let weights: Vec<f64> = measured
                .iter()
                .map(|(position, _)| position.weight)
                .collect();
            let aggregated = aggregate_positions(&response_refs, &weights);
            // 目标相位 = 声明的附加目标延迟线性相位（D0-removed 平面；默认 0 = 零多余相位）。
            // 只保留平滑后的幅度（RMS），不把测量的 bulk/excess 相位直接带进目标。
            let target_delay_samples = spec.target_delay_samples() as f64;
            let sample_rate = baseline.sample_rate_hz() as f64;
            let target: Vec<ComplexPoint> = aggregated
                .iter()
                .map(|point| {
                    let magnitude = magnitude_of(point);
                    let rotation =
                        -2.0 * std::f64::consts::PI * point.freq_hz * target_delay_samples
                            / sample_rate;
                    let (sin, cos) = rotation.sin_cos();
                    ComplexPoint {
                        freq_hz: point.freq_hz,
                        re: magnitude * cos,
                        im: magnitude * sin,
                    }
                })
                .collect();
            for (position, points) in &measured {
                for (index, freq) in grid_hz.iter().enumerate() {
                    if *freq < MODAL_CORRECTION_BAND_HZ.0 || *freq > MODAL_CORRECTION_BAND_HZ.1 {
                        continue;
                    }
                    let measured_mag = magnitude_of(&points[index]);
                    let target_mag = magnitude_of(&target[index]);
                    if measured_mag < options.near_zero_floor
                        || target_mag < options.near_zero_floor
                    {
                        near_zero_points += 1;
                        continue;
                    }
                    let correction_db = ratio_db(target_mag, measured_mag);
                    if correction_db.abs() > required_correction_max_db {
                        required_correction_max_db = correction_db.abs();
                        required_correction_worst = Some((
                            position.label.clone(),
                            participant.role_id.clone(),
                            correction_db,
                        ));
                    }
                    if *freq >= IMPROVEMENT_BAND_HZ.0 && *freq <= IMPROVEMENT_BAND_HZ.1 {
                        let deviation_db = ratio_db(measured_mag, target_mag);
                        let abs_deviation = deviation_db.abs();
                        ripple_sq_sum += deviation_db * deviation_db;
                        ripple_count += 1;
                        if abs_deviation > ripple_max {
                            ripple_max = abs_deviation;
                        }
                    }
                }
            }
            cells.push(TargetCell {
                position: None,
                output_dense: participant.dense_index,
                input_dense: participant.dense_index,
                output_role: participant.role_id.clone(),
                input_role: participant.role_id.clone(),
                points: target,
            });
        }
    }

    if near_zero_points > 0 {
        push_warning(
            &mut warnings,
            TargetWarning::NEAR_ZERO_BASELINE,
            format!(
                "{near_zero_points} near-zero (p,j,f) point(s) excluded from correction/ripple metrics (N/A candidates)",
                near_zero_points = near_zero_points
            ),
        );
    }

    let ripple_before_rms_db = if ripple_count > 0 {
        (ripple_sq_sum / ripple_count as f64).sqrt()
    } else {
        0.0
    };

    let status = if preserve {
        TargetStatus::Preserve
    } else if improvement_band_coverage < IMPROVEMENT_BAND_MIN_COVERAGE {
        TargetStatus::Incomplete
    } else if required_correction_max_db > CORRECTION_HARD_CAP_DB {
        TargetStatus::Infeasible
    } else if required_correction_max_db > CORRECTION_SOFT_CAP_DB {
        TargetStatus::PredictedPartial
    } else {
        TargetStatus::PredictedPass
    };

    let height_scope = match spec.profile() {
        TargetProfile::Multichannel4x2_5 => Some(baseline.height_scope()?),
        TargetProfile::StereoStrict2 => {
            push_warning(
                &mut warnings,
                TargetWarning::STEREO_STRICT_PROFILE,
                "stereo strict profile: output set is constrained to FL/FR; modal support to other outputs is disabled (independent target/G, no runtime mask of the 5x5 solution)".to_string(),
            );
            baseline.optional_height_scope()?
        }
    };
    if height_scope.is_some() {
        push_warning(
            &mut warnings,
            TargetWarning::HEIGHT_SCOPE_SEPARATED,
            "HFL/HFR are full-range (no HP/LP, no artificial delay) and excluded from the low-band matrix".to_string(),
        );
    }
    let output_scope = baseline.output_scope()?;
    let plant_hash = plant.hash();
    let design_plant_hash = design_plant_digest(plant);

    Ok(TargetMatrix {
        schema: TARGET_SCHEMA,
        profile: spec.profile(),
        target_id: spec.target_id().to_string(),
        sample_rate_hz: baseline.sample_rate_hz(),
        layout_id: baseline.participants().layout_id().to_string(),
        participants: baseline.participants().clone(),
        baseline_hash: baseline.baseline_hash().to_string(),
        plant_hash,
        design_plant_hash,
        common_d0: baseline.common_d0(),
        tone: spec.tone().clone(),
        tone_applications: spec.tone_applications(),
        curve: spec.curve().clone(),
        decay,
        options,
        target_delay_samples: spec.target_delay_samples(),
        status,
        grid_hz,
        cells,
        diagnostics: TargetDiagnostics {
            required_correction_max_db,
            required_correction_worst,
            ripple_before_rms_db,
            ripple_before_max_db: ripple_max,
            improvement_band_coverage,
            modal_band_coverage,
            transition_band_coverage,
            design_positions: design_positions.len(),
            validation_positions_reserved,
            near_zero_points,
        },
        height_scope,
        output_scope,
        warnings,
    })
    .and_then(|matrix| {
        matrix.verify_structure()?;
        Ok(matrix)
    })
}

/// stereo strict 独立构建入口（profile 必须为 StereoStrict2；复用 5x5 规格 → E_TGT_STEREO）。
pub fn build_stereo_strict_target(
    baseline: &FrozenPostBaseline,
    plant: &EffectivePlant,
    spec: &TargetSpec,
) -> Result<TargetMatrix, TargetError> {
    if spec.profile() != TargetProfile::StereoStrict2 {
        return Err(err(
            TargetErrorKind::Stereo,
            "target.profile",
            format!(
                "stereo strict builder requires profile {:?}, got {:?} (5x5 target must not be reused)",
                TargetProfile::StereoStrict2.as_str(),
                spec.profile().as_str()
            ),
        ));
    }
    build_target_matrix(baseline, plant, spec)
}

// ===========================================================================
// 求解矩阵（T51 产物）与受限 profile 校验
// ===========================================================================

/// 一个求解 cell（G 的 row=output j / col=input i 复响应）。
#[derive(Debug, Clone, PartialEq)]
pub struct ModalMatrixCell {
    /// 输出 dense 索引（G 的行）。
    pub out_dense: u16,
    /// 输入 dense 索引（G 的列）。
    pub in_dense: u16,
    /// G 复响应点（完整 G = deltaD·I + g；非对角只放修正）。
    pub points: Vec<ComplexPoint>,
}

/// 求解后的模态矩阵 G（T51 产物；本文件只做结构/受限 profile 校验，不解方程）。
#[derive(Debug, Clone, PartialEq)]
pub struct ModalMatrix {
    roles: Vec<String>,
    grid_hz: Vec<f64>,
    cells: Vec<ModalMatrixCell>,
}

impl ModalMatrix {
    /// 构造并校验（角色唯一、网格严格升序、cells 尺寸合法且不重复）。
    pub fn new(
        roles: Vec<String>,
        grid_hz: Vec<f64>,
        cells: Vec<ModalMatrixCell>,
    ) -> Result<Self, TargetError> {
        if roles.is_empty() || roles.len() > MAX_PARTICIPANTS {
            return Err(err(
                TargetErrorKind::Matrix,
                "solution.roles",
                format!(
                    "solution must declare 1..={MAX_PARTICIPANTS} roles, got {}",
                    roles.len()
                ),
            ));
        }
        for (index, role) in roles.iter().enumerate() {
            if !is_role_id(role) {
                return Err(err(
                    TargetErrorKind::Matrix,
                    format!("solution.roles[{index}]"),
                    format!("role id {role:?} must match ^[A-Z][A-Z0-9]*$"),
                ));
            }
            if roles[..index].iter().any(|existing| existing == role) {
                return Err(err(
                    TargetErrorKind::Matrix,
                    format!("solution.roles[{index}]"),
                    format!("duplicate role {role}"),
                ));
            }
        }
        if grid_hz.len() < 2 {
            return Err(err(
                TargetErrorKind::Grid,
                "solution.grid_hz",
                "solution grid needs at least 2 points",
            ));
        }
        let mut previous = f64::NEG_INFINITY;
        for (index, freq) in grid_hz.iter().enumerate() {
            let freq = finite(*freq, &format!("solution.grid_hz[{index}]"))?;
            if freq <= 0.0 || freq <= previous {
                return Err(err(
                    TargetErrorKind::Grid,
                    format!("solution.grid_hz[{index}]"),
                    "solution grid must be positive and strictly increasing",
                ));
            }
            previous = freq;
        }
        if cells.is_empty() {
            return Err(err(
                TargetErrorKind::Missing,
                "solution.cells",
                "solution matrix has no cells",
            ));
        }
        for (index, cell) in cells.iter().enumerate() {
            if cell.out_dense as usize >= roles.len() || cell.in_dense as usize >= roles.len() {
                return Err(err(
                    TargetErrorKind::Matrix,
                    format!("solution.cells[{index}]"),
                    format!(
                        "dense indices ({}, {}) exceed role count {}",
                        cell.out_dense,
                        cell.in_dense,
                        roles.len()
                    ),
                ));
            }
            if cells[..index].iter().any(|existing| {
                existing.out_dense == cell.out_dense && existing.in_dense == cell.in_dense
            }) {
                return Err(err(
                    TargetErrorKind::Matrix,
                    format!("solution.cells[{index}]"),
                    format!(
                        "duplicate solution cell ({}, {})",
                        cell.out_dense, cell.in_dense
                    ),
                ));
            }
            validate_complex_grid(
                &cell.points,
                &grid_hz,
                &format!("solution.cells[{index}].points"),
            )?;
        }
        Ok(Self {
            roles,
            grid_hz,
            cells,
        })
    }

    /// 单位 G（deltaD·I：对角 1+0j；用于 preserve/NC-01 对照）。
    pub fn identity(roles: Vec<String>, grid_hz: Vec<f64>) -> Result<Self, TargetError> {
        let mut cells = Vec::with_capacity(roles.len());
        for (index, _) in roles.iter().enumerate() {
            let points = grid_hz
                .iter()
                .map(|freq| ComplexPoint {
                    freq_hz: *freq,
                    re: 1.0,
                    im: 0.0,
                })
                .collect();
            cells.push(ModalMatrixCell {
                out_dense: index as u16,
                in_dense: index as u16,
                points,
            });
        }
        Self::new(roles, grid_hz, cells)
    }

    /// M。
    pub fn m(&self) -> usize {
        self.roles.len()
    }

    /// 角色（dense 顺序）。
    pub fn roles(&self) -> &[String] {
        &self.roles
    }

    /// 网格。
    pub fn grid_hz(&self) -> &[f64] {
        &self.grid_hz
    }

    /// cells。
    pub fn cells(&self) -> &[ModalMatrixCell] {
        &self.cells
    }

    /// 按 (out,in) 取 G。
    pub fn cell(&self, out_dense: u16, in_dense: u16) -> Option<&[ComplexPoint]> {
        self.cells
            .iter()
            .find(|cell| cell.out_dense == out_dense && cell.in_dense == in_dense)
            .map(|cell| cell.points.as_slice())
    }

    /// 是否单位 G（对角 1、非对角 0；容差内）。
    pub fn is_identity(&self, tolerance: f64) -> bool {
        self.cells.iter().all(|cell| {
            cell.points.iter().all(|point| {
                if cell.out_dense == cell.in_dense {
                    (point.re - 1.0).abs() <= tolerance && point.im.abs() <= tolerance
                } else {
                    point.re.abs() <= tolerance && point.im.abs() <= tolerance
                }
            })
        })
    }

    /// 非对角 cell 最大幅度（dB；无 → -inf 语义用 None 表示 0 幅度）。
    pub fn offdiag_max_db(&self, lo_hz: f64, hi_hz: f64) -> f64 {
        let mut maximum = 0.0f64;
        for cell in &self.cells {
            if cell.out_dense == cell.in_dense {
                continue;
            }
            for point in &cell.points {
                if point.freq_hz < lo_hz || point.freq_hz > hi_hz {
                    continue;
                }
                let db = magnitude_db(point);
                if db > maximum {
                    maximum = db;
                }
            }
        }
        maximum
    }
}

/// 一条求解校验检查项。
#[derive(Debug, Clone, PartialEq)]
pub struct SolutionCheck {
    /// 稳定检查名。
    pub name: &'static str,
    /// 是否通过。
    pub passed: bool,
    /// 说明。
    pub detail: String,
}

/// 求解矩阵校验结果（只校验结构/受限 profile/预测一致性；不签实测质量）。
#[derive(Debug, Clone, PartialEq)]
pub struct SolutionValidation {
    /// profile。
    pub profile: TargetProfile,
    /// 全部检查是否通过。
    pub passed: bool,
    /// 检查项。
    pub checks: Vec<SolutionCheck>,
    /// 改善带内最大复数相对残差（dB；None = 无有效点）。
    pub residual_max_db: Option<f64>,
    /// 最坏 (position, role, residual_db)。
    pub residual_worst: Option<(String, String, f64)>,
    /// 非对角最大幅度（dB）。
    pub offdiag_max_db: f64,
    /// 对角偏离 0 dB 的最大绝对值（dB）。
    pub diag_deviation_max_db: f64,
}

/// 校验求解矩阵 G 是否满足目标 profile（stereo strict 输出集合/尺寸；5x5 尺寸/角色）与预测一致性。
pub fn validate_solution(
    target: &TargetMatrix,
    plant: &EffectivePlant,
    solved: &ModalMatrix,
) -> Result<SolutionValidation, TargetError> {
    if plant.baseline_hash() != target.baseline_hash() {
        return Err(err(
            TargetErrorKind::Baseline,
            "solution.baseline_hash",
            "H_eff plant was built from a different frozen P baseline than the target",
        ));
    }
    if plant.participants().binding_hash() != target.participants().binding_hash() {
        return Err(err(
            TargetErrorKind::Participants,
            "solution.participants",
            "solution validation plant participants differ from the target participants",
        ));
    }
    if plant.sample_rate_hz() != target.sample_rate_hz() {
        return Err(err(
            TargetErrorKind::SampleRate,
            "solution.sample_rate_hz",
            "solution validation plant fs differs from the target fs",
        ));
    }
    let expected_roles: Vec<String> = target
        .participants()
        .role_ids()
        .iter()
        .map(|role| (*role).to_string())
        .collect();
    match target.profile() {
        TargetProfile::StereoStrict2 => {
            if solved.m() != 2 || solved.roles() != expected_roles.as_slice() {
                return Err(err(
                    TargetErrorKind::Stereo,
                    "solution.roles",
                    format!(
                        "stereo strict profile requires a 2x2 G over {:?}; got {} roles {:?} (5x5 G must not be reused by runtime masking)",
                        expected_roles,
                        solved.m(),
                        solved.roles()
                    ),
                ));
            }
        }
        TargetProfile::Multichannel4x2_5 => {
            if solved.m() != 5 || solved.roles() != expected_roles.as_slice() {
                return Err(err(
                    TargetErrorKind::Matrix,
                    "solution.roles",
                    format!(
                        "multichannel 5x5 target requires G over {:?}; got {} roles {:?}",
                        expected_roles,
                        solved.m(),
                        solved.roles()
                    ),
                ));
            }
        }
    }
    if !grid_matches(solved.grid_hz(), target.grid_hz()) {
        return Err(err(
            TargetErrorKind::Grid,
            "solution.grid_hz",
            "solution grid does not match the target grid",
        ));
    }
    // 对角 baseline（identity/deltaD）必须声明；缺对角 → 结构错误。
    for dense in 0..solved.m() as u16 {
        if solved.cell(dense, dense).is_none() {
            return Err(err(
                TargetErrorKind::Matrix,
                format!("solution.cells[{dense},{dense}]"),
                "G diagonal (deltaD identity baseline) cell is missing; full G must be declared",
            ));
        }
    }

    let mut checks: Vec<SolutionCheck> = Vec::new();
    let mut residual_max_db: Option<f64> = None;
    let mut residual_worst: Option<(String, String, f64)> = None;
    let design_positions: Vec<&EffectivePosition> = plant.design_positions().collect();
    let improvement_lo = IMPROVEMENT_BAND_HZ.0;
    let improvement_hi = IMPROVEMENT_BAND_HZ.1;

    for position in &design_positions {
        for participant in target.participants().participants() {
            let target_points = match target.cell(
                if target.is_position_variant() {
                    Some(position.label.as_str())
                } else {
                    None
                },
                participant.dense_index,
                participant.dense_index,
            ) {
                TargetCellView::Response(points) => points,
                TargetCellView::Zero => {
                    return Err(err(
                        TargetErrorKind::Matrix,
                        format!("target.cells[{}]", participant.role_id),
                        "target is missing the diagonal cell for this participant",
                    ));
                }
            };
            let mut worst_for_cell = 0.0f64;
            for (index, freq) in target.grid_hz().iter().enumerate() {
                if *freq < improvement_lo || *freq > improvement_hi {
                    continue;
                }
                let mut predicted_re = 0.0f64;
                let mut predicted_im = 0.0f64;
                for output in target.participants().participants() {
                    let input = position.input(&output.role_id).ok_or_else(|| {
                        err(
                            TargetErrorKind::Missing,
                            format!("h_eff.{}.{}", position.label, output.role_id),
                            "design position missing H_eff for participant",
                        )
                    })?;
                    let gain = match solved.cell(output.dense_index, participant.dense_index) {
                        Some(points) => points[index],
                        None => ComplexPoint {
                            freq_hz: *freq,
                            re: 0.0,
                            im: 0.0,
                        },
                    };
                    let (re, im) = complex_multiply(
                        (input.points[index].re, input.points[index].im),
                        (gain.re, gain.im),
                    );
                    predicted_re += re;
                    predicted_im += im;
                }
                let target_point = target_points[index];
                let diff_re = predicted_re - target_point.re;
                let diff_im = predicted_im - target_point.im;
                let diff_mag = diff_re.hypot(diff_im);
                let target_mag = magnitude_of(&target_point);
                let residual_db = ratio_db(diff_mag, target_mag);
                if residual_db > worst_for_cell {
                    worst_for_cell = residual_db;
                }
            }
            if residual_max_db
                .map(|current| worst_for_cell > current)
                .unwrap_or(true)
            {
                residual_max_db = Some(worst_for_cell);
                residual_worst = Some((
                    position.label.clone(),
                    participant.role_id.clone(),
                    worst_for_cell,
                ));
            }
        }
    }

    let mut diag_deviation_max_db = 0.0f64;
    for dense in 0..solved.m() as u16 {
        let points = solved.cell(dense, dense).ok_or_else(|| {
            err(
                TargetErrorKind::Matrix,
                format!("solution.cells[{dense},{dense}]"),
                "missing G diagonal",
            )
        })?;
        for point in points {
            if point.freq_hz < improvement_lo || point.freq_hz > improvement_hi {
                continue;
            }
            let deviation = magnitude_db(point).abs();
            if deviation > diag_deviation_max_db {
                diag_deviation_max_db = deviation;
            }
        }
    }
    let offdiag_max_db = solved.offdiag_max_db(improvement_lo, improvement_hi);
    let residual_limit_db = if target.decay().is_preserve() {
        1.0
    } else {
        CORRECTION_HARD_CAP_DB
    };
    let residual_ok = residual_max_db
        .map(|residual| residual <= residual_limit_db)
        .unwrap_or(false);

    checks.push(SolutionCheck {
        name: "profile_dimensions",
        passed: solved.m() == target.profile().m(),
        detail: format!(
            "profile {} expects M={}, got M={}",
            target.profile().as_str(),
            target.profile().m(),
            solved.m()
        ),
    });
    checks.push(SolutionCheck {
        name: "output_role_set",
        passed: solved.roles() == expected_roles.as_slice(),
        detail: format!(
            "G output/input roles {:?} must equal target participants {:?}",
            solved.roles(),
            expected_roles
        ),
    });
    checks.push(SolutionCheck {
        name: "modal_support_to_other_outputs",
        passed: !target.profile().modal_support_to_other_outputs()
            || target.profile() == TargetProfile::Multichannel4x2_5,
        detail: format!(
            "profile {} modal_support_to_other_outputs={}",
            target.profile().as_str(),
            target.profile().modal_support_to_other_outputs()
        ),
    });
    checks.push(SolutionCheck {
        name: "predicted_residual_improvement_band",
        passed: residual_ok,
        detail: format!(
            "max complex residual in {:?} Hz = {:?} dB (limit {residual_limit_db} dB)",
            IMPROVEMENT_BAND_HZ, residual_max_db
        ),
    });

    let passed = checks.iter().all(|check| check.passed);
    Ok(SolutionValidation {
        profile: target.profile(),
        passed,
        checks,
        residual_max_db,
        residual_worst,
        offdiag_max_db,
        diag_deviation_max_db,
    })
}

/// stereo strict 专用校验入口（目标必须为 stereo strict；否则 E_TGT_STEREO）。
pub fn validate_stereo_strict_solution(
    target: &TargetMatrix,
    plant: &EffectivePlant,
    solved: &ModalMatrix,
) -> Result<SolutionValidation, TargetError> {
    if target.profile() != TargetProfile::StereoStrict2 {
        return Err(err(
            TargetErrorKind::Stereo,
            "target.profile",
            format!(
                "stereo strict validation requires a stereo strict target, got {:?}",
                target.profile().as_str()
            ),
        ));
    }
    validate_solution(target, plant, solved)
}

// ===========================================================================
// 内部：P 冻结 hash / canonical / 高度与输出 scope 派生
// ===========================================================================

fn peq_canonical(peq: &ChannelPeq) -> String {
    let mut canonical = String::new();
    canonical.push_str(&format!(
        "peq name={} index={} bands={}\n",
        peq.name,
        peq.index,
        peq.bands.len()
    ));
    for band in &peq.bands {
        canonical.push_str(&format!(
            "band type={} freq={} q={} gain_db={}\n",
            band.band_type.as_str(),
            fmt_f64(band.freq_hz),
            fmt_f64(band.q),
            fmt_f64(band.gain_db)
        ));
    }
    canonical
}

fn canonical_post_baseline(input: &PostBaselineInput) -> String {
    let mut canonical = String::new();
    canonical.push_str(P_FREEZE_SCHEMA);
    canonical.push('\n');
    canonical.push_str(&format!(
        "baseline_id={} sample_rate_hz={} fit_inputs_sha256={} common_d0_samples={} grid_points={} grid_sha256={}\n",
        input.baseline_id,
        input.sample_rate_hz,
        input.fit_inputs_sha256,
        input.common_d0.samples(),
        input.grid_hz.len(),
        grid_sha256(&input.grid_hz)
    ));
    canonical.push_str(&input.participants.canonical());
    canonical.push_str(&format!("{}\n", input.tone.canonical()));
    canonical.push_str(&format!("{}\n", input.curve.canonical()));
    canonical.push_str(&format!("{}\n", input.tone_applications.canonical()));
    for role in &input.roles {
        let peq = role
            .modal_peq()
            .map(peq_canonical)
            .map(|text| sha256_hex(text.as_bytes()))
            .unwrap_or_else(|| "none".to_string());
        canonical.push_str(&format!(
            "role={} dac={} delay_samples={} polarity={} gain_db={} {} artificial_delay_ms={} modal_peq_sha256={} points_sha256={}\n",
            role.role_id(),
            role.out_index(),
            role.delay_samples(),
            role.polarity(),
            fmt_f64(role.gain_db()),
            role.band_limit().canonical(),
            fmt_f64(role.artificial_delay_ms()),
            peq,
            points_sha256(role.points())
        ));
    }
    canonical
}

fn post_freeze_hash(input: &PostBaselineInput) -> String {
    sha256_hex(
        format!(
            "{POST_FREEZE_HASH_DOMAIN}\n{}\n",
            canonical_post_baseline(input)
        )
        .as_bytes(),
    )
}

fn height_scope_from_input(
    input: &PostBaselineInput,
) -> Result<Option<FullRangeHeightScope>, TargetError> {
    let fs = input.sample_rate_hz as f64;
    let mut present = 0usize;
    let mut roles = Vec::new();
    for role_id in HEIGHT_ROLE_IDS {
        let role = match input.roles.iter().find(|role| role.role_id() == role_id) {
            Some(role) => role,
            None => continue,
        };
        present += 1;
        if input.participants.contains_role(role_id) {
            return Err(err(
                TargetErrorKind::Scope,
                format!("baseline.roles[{role_id}]"),
                format!("height role {role_id} must not participate in the low-band 5x5 matrix"),
            ));
        }
        if role.band_limit().is_band_limit() {
            return Err(err(
                TargetErrorKind::Scope,
                format!("baseline.roles[{role_id}].band_limit"),
                format!(
                    "height role {role_id} must be full-range; declared {} (legacy HP/LP errors are rejected)",
                    role.band_limit().canonical()
                ),
            ));
        }
        if role.artificial_delay_ms().abs() > ARTIFICIAL_DELAY_TOLERANCE_MS {
            return Err(err(
                TargetErrorKind::Scope,
                format!("baseline.roles[{role_id}].artificial_delay_ms"),
                format!(
                    "height role {role_id} must not carry artificial delay (old 18 ms design was rejected); declared {} ms",
                    role.artificial_delay_ms()
                ),
            ));
        }
        let mut min_db = f64::INFINITY;
        let mut max_db = f64::NEG_INFINITY;
        for point in role.points() {
            if point.freq_hz < MODAL_CORRECTION_BAND_HZ.0
                || point.freq_hz > MODAL_CORRECTION_BAND_HZ.1
            {
                continue;
            }
            let db = magnitude_db(point);
            if db < min_db {
                min_db = db;
            }
            if db > max_db {
                max_db = db;
            }
        }
        if !min_db.is_finite() {
            min_db = 0.0;
        }
        if !max_db.is_finite() {
            max_db = 0.0;
        }
        roles.push(HeightRoleScope {
            role_id: role_id.to_string(),
            out_index: role.out_index(),
            band_hz: (0.0, fs / 2.0),
            band_limit: BandLimit::None,
            artificial_delay_ms: 0.0,
            in_low_band_matrix: false,
            p_low_band_min_db: min_db,
            p_low_band_max_db: max_db,
        });
    }
    if present == 0 {
        return Ok(None);
    }
    if present != HEIGHT_ROLE_IDS.len() {
        return Err(err(
            TargetErrorKind::Missing,
            "baseline.roles",
            format!(
                "height scope must declare all of {:?} or none; found {present}",
                HEIGHT_ROLE_IDS
            ),
        ));
    }
    Ok(Some(FullRangeHeightScope {
        roles,
        sample_rate_hz: input.sample_rate_hz,
        excluded_from_low_band_matrix: true,
    }))
}

fn output_scope_from_input(input: &PostBaselineInput) -> Result<ScopedOutputs, TargetError> {
    let fs = input.sample_rate_hz as f64;
    let mut entries = Vec::with_capacity(input.roles.len());
    for role in &input.roles {
        let band_limit = role.band_limit();
        let band_hz = match band_limit {
            BandLimit::None => (0.0, fs / 2.0),
            BandLimit::HighPass { freq_hz } => (freq_hz, fs / 2.0),
            BandLimit::LowPass { freq_hz } => (0.0, freq_hz),
        };
        let kind = if let Some(dense_index) = input.participants.dense_index_of(role.role_id()) {
            OutputScopeKind::LowBandMatrix { dense_index }
        } else if HEIGHT_ROLE_IDS.contains(&role.role_id()) {
            OutputScopeKind::FullRangeHeight
        } else {
            OutputScopeKind::Other
        };
        let band_hz = match kind {
            OutputScopeKind::LowBandMatrix { .. } => band_hz,
            _ => (0.0, fs / 2.0),
        };
        entries.push(OutputScopeEntry {
            role_id: role.role_id().to_string(),
            out_index: role.out_index(),
            kind,
            band_hz,
            band_limit,
        });
    }
    Ok(ScopedOutputs {
        layout_id: input.participants.layout_id().to_string(),
        entries,
    })
}

// ===========================================================================
// 证据 JSON（确定性；仅设计侧/离线产物）
// ===========================================================================

fn json_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len() + 2);
    for character in text.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                escaped.push_str(&format!("\\u{:04x}", control as u32));
            }
            other => escaped.push(other),
        }
    }
    escaped
}

fn json_number(value: f64) -> String {
    fmt_f64(value)
}

impl TargetMatrix {
    /// 报告/证据 JSON（schema tspi-modal-target/v1；确定性；无时间戳）。
    pub fn to_evidence_json(&self) -> String {
        let mut json = String::new();
        json.push('{');
        json.push_str(&format!("\"schema\":\"{}\",", self.schema));
        json.push_str(&format!(
            "\"profile\":\"{}\",\"target_id\":\"{}\",\"status\":\"{}\",",
            self.profile.as_str(),
            json_escape(&self.target_id),
            self.status.as_str()
        ));
        json.push_str(
            "\"applied\":false,\"post_apply_measurement\":\"not_requested\",\"engine_compatibility\":\"not_validated\",\"runtime_validation\":\"not_run\",",
        );
        json.push_str(&format!(
            "\"sample_rate_hz\":{},\"layout_id\":\"{}\",\"m\":{},",
            self.sample_rate_hz,
            json_escape(&self.layout_id),
            self.m()
        ));
        json.push_str("\"participants\":[");
        for (index, participant) in self.participants.participants().iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            json.push_str(&format!(
                "{{\"dense\":{},\"role\":\"{}\",\"dac\":{}}}",
                participant.dense_index,
                json_escape(&participant.role_id),
                participant.out_index
            ));
        }
        json.push_str("],");
        json.push_str(&format!(
            "\"baseline_hash\":\"{}\",\"plant_hash\":\"{}\",\"design_plant_hash\":\"{}\",\"target_sha256\":\"{}\",",
            json_escape(&self.baseline_hash),
            json_escape(&self.plant_hash),
            json_escape(&self.design_plant_hash),
            self.target_sha256()
        ));
        json.push_str(&format!(
            "\"common_d0_samples\":{},\"target_delay_samples\":{},",
            self.common_d0.samples(),
            self.target_delay_samples
        ));
        json.push_str("\"tone\":{\"curve_id\":\"");
        json.push_str(&json_escape(self.tone.curve_id()));
        json.push_str("\",\"shape\":\"");
        json.push_str(self.tone.shape().as_str());
        json.push_str("\",\"reference_gain_db\":");
        json.push_str(&json_number(self.tone.reference_gain_db()));
        json.push_str(",\"target_sha256\":");
        match self.tone.target_sha256() {
            Some(hash) => {
                json.push('"');
                json.push_str(&json_escape(hash));
                json.push('"');
            }
            None => json.push_str("null"),
        }
        json.push_str(",\"applications\":{\"post_baseline\":");
        json.push_str(&self.tone_applications.post_baseline().to_string());
        json.push_str(",\"low_band_residual\":");
        json.push_str(&self.tone_applications.low_band_residual().to_string());
        json.push_str(",\"target\":");
        json.push_str(&self.tone_applications.target().to_string());
        json.push_str("}},");
        json.push_str(&format!(
            "\"curve\":{{\"curve_id\":\"{}\",\"site\":\"{}\",\"applications\":{},\"target_sha256\":{}}},",
            json_escape(self.curve.curve_id()),
            self.curve.site().as_str(),
            self.curve.applications(),
            self.curve
                .target_sha256()
                .map(|hash| format!("\"{}\"", json_escape(hash)))
                .unwrap_or_else(|| "null".to_string())
        ));
        json.push_str(&format!(
            "\"decay\":{{\"mode\":\"{}\",\"ratio\":{},\"reduction_percent\":{},\"meets_t20_gate\":{},\"bands\":[",
            self.decay.mode().as_str(),
            json_number(self.decay.ratio()),
            json_number(self.decay.reduction_percent()),
            self.decay.meets_t20_gate()
        ));
        for (index, band) in self.decay.bands().iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            let target = self
                .decay
                .target_t60_s(band.band_hz)
                .map(json_number)
                .unwrap_or_else(|| "null".to_string());
            json.push_str(&format!(
                "{{\"band_hz\":{},\"baseline_t60_s\":{},\"target_t60_s\":{}}}",
                json_number(band.band_hz),
                json_number(band.baseline_t60_s),
                target
            ));
        }
        json.push_str("]},");
        json.push_str(&format!(
            "\"options\":{{\"smoothing_octave\":{},\"near_zero_floor\":{},\"require_explicit_participants\":{}}},",
            json_number(self.options.smoothing_octave),
            json_number(self.options.near_zero_floor),
            self.options.require_explicit_participants
        ));
        json.push_str(&format!(
            "\"grid\":{{\"points\":{},\"sha256\":\"{}\"}},",
            self.grid_hz.len(),
            grid_sha256(&self.grid_hz)
        ));
        json.push_str(&format!(
            "\"diagnostics\":{{\"required_correction_max_db\":{},\"required_correction_worst\":{},\"ripple_before_rms_db\":{},\"ripple_before_max_db\":{},\"improvement_band_coverage\":{},\"modal_band_coverage\":{},\"transition_band_coverage\":{},\"design_positions\":{},\"near_zero_points\":{},\"holdout_reserved\":[",
            json_number(self.diagnostics.required_correction_max_db),
            self.diagnostics
                .required_correction_worst
                .as_ref()
                .map(|(position, role, correction)| format!(
                    "{{\"position\":\"{}\",\"role\":\"{}\",\"correction_db\":{}}}",
                    json_escape(position),
                    json_escape(role),
                    json_number(*correction)
                ))
                .unwrap_or_else(|| "null".to_string()),
            json_number(self.diagnostics.ripple_before_rms_db),
            json_number(self.diagnostics.ripple_before_max_db),
            json_number(self.diagnostics.improvement_band_coverage),
            json_number(self.diagnostics.modal_band_coverage),
            json_number(self.diagnostics.transition_band_coverage),
            self.diagnostics.design_positions,
            self.diagnostics.near_zero_points
        ));
        for (index, label) in self
            .diagnostics
            .validation_positions_reserved
            .iter()
            .enumerate()
        {
            if index > 0 {
                json.push(',');
            }
            json.push_str(&format!("\"{}\"", json_escape(label)));
        }
        json.push_str("]},");
        match &self.height_scope {
            Some(height_scope) => {
                json.push_str(
                    "\"height_scope\":{\"excluded_from_low_band_matrix\":true,\"roles\":[",
                );
                for (index, role) in height_scope.roles().iter().enumerate() {
                    if index > 0 {
                        json.push(',');
                    }
                    json.push_str(&format!(
                        "{{\"role\":\"{}\",\"dac\":{},\"band_hz\":[{},{}],\"band_limit\":\"{}\",\"artificial_delay_ms\":{},\"in_low_band_matrix\":{},\"p_low_band_db\":[{},{}]}}",
                        json_escape(&role.role_id),
                        role.out_index,
                        json_number(role.band_hz.0),
                        json_number(role.band_hz.1),
                        role.band_limit.as_str(),
                        json_number(role.artificial_delay_ms),
                        role.in_low_band_matrix,
                        json_number(role.p_low_band_min_db),
                        json_number(role.p_low_band_max_db)
                    ));
                }
                json.push_str("]},");
            }
            None => json.push_str("\"height_scope\":null,"),
        }
        json.push_str("\"output_scope\":[");
        for (index, entry) in self.output_scope.entries().iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            json.push_str(&format!(
                "{{\"role\":\"{}\",\"dac\":{},\"kind\":\"{}\",\"band_hz\":[{},{}],\"band_limit\":\"{}\"}}",
                json_escape(&entry.role_id),
                entry.out_index,
                entry.kind.as_str(),
                json_number(entry.band_hz.0),
                json_number(entry.band_hz.1),
                entry.band_limit.as_str()
            ));
        }
        json.push_str("],");
        let cells_sha = {
            let mut text = String::new();
            for cell in &self.cells {
                text.push_str(&format!(
                    "{};{};{};{}\n",
                    cell.position.as_deref().unwrap_or("none"),
                    cell.output_dense,
                    cell.input_dense,
                    points_sha256(&cell.points)
                ));
            }
            sha256_hex(text.as_bytes())
        };
        json.push_str(&format!(
            "\"cells\":{{\"count\":{},\"sha256\":\"{}\",\"position_variant\":{},\"offdiag\":\"structural_zero\"}},",
            self.cells.len(),
            cells_sha,
            self.is_position_variant()
        ));
        json.push_str(&format!(
            "\"holdout\":{{\"used_for_tuning\":false,\"reserved_labels\":[{}]}},",
            self.diagnostics
                .validation_positions_reserved
                .iter()
                .map(|label| format!("\"{}\"", json_escape(label)))
                .collect::<Vec<String>>()
                .join(",")
        ));
        json.push_str("\"warnings\":[");
        for (index, warning) in self.warnings.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            json.push_str(&format!(
                "{{\"code\":\"{}\",\"message\":\"{}\"}}",
                warning.code,
                json_escape(&warning.message)
            ));
        }
        json.push_str("],");
        json.push_str(
            "\"claim\":\"predicted-only target matrix (design positions only); not applied, not measured, no improvement claim\"",
        );
        json.push('}');
        json
    }
}

// ===========================================================================
// 静态守卫（单测用；证明本模块纯计算、无 I/O/线程/环境依赖）
// ===========================================================================

/// 本模块源码文本（单测静态守卫用；不参与运行期逻辑）。
pub fn target_source_text_for_guards() -> &'static str {
    include_str!("target.rs")
}
