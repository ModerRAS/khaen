//! [NONRT] 多点位置/重复质量/设计验证分组（WBS T63，deps T62/T49/T60） ——本文件是 WBS T63 写域。
//!
//! 职责：把「多点 UMIK 声学测量」的预声明测点集合（label/role/weight，不要求 xyz 坐标）
//! 与逐测点重复质量门做成可离线验证、可持久化、可在 job 重启后恢复的 [NONRT] 层：
//!
//! 1. MultipointPlan：一个 session 的测点计划 —— label/role/weight 必填（design/validation
//!    显式分组），坐标可选（绝不要求 xyz），但听音位麦克风朝向必须声明为 90deg
//!    （ACCEPTANCE §0.1.2；0deg/未声明一律拒收，见 E_POS_MIC_ORIENTATION）；公共
//!    session/layout/fs/participants/T6 FC-04 公共整数 D0/相位基准声明一并冻结；
//!    plan_sha256 是自 hash，变更即 stale。
//! 2. evaluate_quality：按 scope 对 design 或 validation 组做重复质量门。每个
//!    (测点, participant) 的每遍 measurement 必须同时具备 SNR（全带 + 低频带）、
//!    clock（ppm/uncertainty/ok）、phase（phase_ref/phase/delay/polarity/残余相位）
//!    证据；缺任一（SNR/clock/phase）即返回错误，绝不补默认值、绝不继续拟合/求解。
//!    有值但超门（SNR<25 dB、延迟极差>0.3 sample、幅频极差>0.4 dB、极性不一致、残余相位
//!    >15°、公共 D 残差 >±0.5 sample、未声明校正的显著漂移等）→ 该 measurement 不可用，
//!    报告 continue_allowed=false 并给稳定原因码。
//! 3. 设计/验证分组：design 组只用于拟合，validation/holdout 组不参与拟合；evaluate_quality
//!    的 QualityScope::Design 会拒收 validation 测点输入（反之亦然），防止 holdout 污染。
//! 4. job 可恢复：plan/quality 产物是确定性 JSON（同输入逐字节相同）并带 plan 自 hash；
//!    write_positions_stage_artifacts 通过 T60 StageCtx 登记 artifact（重跑同内容幂等，
//!    内容变化触发 job artifact immutability 拒收）；load_resume_state 在进程重启后从
//!    artifact 目录重建 plan + quality 摘要并校验 plan hash/stale，不重放采集。
//!
//! 权威契约（只读输入）：
//! - WBS T63 行与 §1/§5/§6/§7（scratch/lead/planner/TSPI-RT-STACK-WBS.md）；
//! - DSP-HOST-DECISION §2.1（UMIK 多点仅 label/role/weight 必填、坐标/朝向可选、
//!   design/validation 独立、预声明测点/频带/质量掩码、不看 holdout 再挑点）与 §2.6
//!   （job 状态机/可恢复/失败 last-good/mute）；
//! - rt-stack/spec/mimo-t6.md：§2.1（positions.design[]/holdout[]，label/role/weight 必填、
//!   空间坐标可选；common_d_samples/phase_ref FC-04/FC-06）、§3.2（4.1.2 design >=6、
//!   holdout >=3、总数推荐 9；5+3 仅 pilot=INCOMPLETE）、§3.5（重复质量/SNR 双门/clock 2σ/
//!   残余相位 15°/公共 D ±0.5 sample）、§5.1-§5.3（holdout 不参与调参、密封）、§8.4（I-03/I-07）；
//! - calibration-spec/ACCEPTANCE（只读）：T2 UMIK 重复性/漂移/SNR、§0.2.2 clock ppm+sigma、
//!   §0.2.4 残余相位跨度、§0.5 峰值门；
//! - rt-stack/README.md §3 写域纪律；T60 crates/calibration/src/job.rs（artifact/checkpoint）；
//! - T62 crates/calibration/src/capture.rs（clock/onset/timebase 生产者语义：本模块只消费其
//!   per-repeat 度量，不重新做 onset/反卷积；capture_peak_dbfs 门属 T62/T73）；
//! - T49 crates/modal-design/src/timebase.rs（CommonD0/PhaseBasis 语义，本模块直接复用，
//!   不另立互斥 D0/相位定义）；
//! - T48 crates/modal-design/src/ingest.rs（PositionRole 显式角色，不从名称/坐标推断）。
//!
//! 实现边界（NOT_RUN）：
//! - 纯 [NONRT] 计算 + 文件 artifact 生成：不打开 ALSA/不采音/不驱动扫频/不拟合/不求解/
//!   apply（T61/T62/T64-T66/T67 职责）；不访问网络/不启动进程；
//! - 不实现 holdout 密封协议（T73/T68）和跨轮预注册（T68/T69），只保证 plan hash/角色分组
//!   可供这些上层引用；
//! - 不决定最终 T6 PASS：本模块只判「质量门是否允许继续」，最终 I/T20/复现门属 T67-T69/T31；
//! - 无真实 UMIK/MOTU/板端数据：单测全部为合成度量夹具（见 tests/T63）；evidence/T63 记录
//!   NOT_RUN 项；
//! - 不注册公共 mod/CLI、不改 Cargo.toml/lib.rs/tests/public_registration.rs；测试用本文件末尾
//!   #[cfg(test)] mod t63_tests { include!(...) } 自注册（T06 纪律）。

#![forbid(unsafe_code)]

use std::fmt;
use std::fmt::Write as _;

use tspi_dsp_core::rt::{MAX_LOW_BAND_PARTICIPANTS, SAMPLE_RATES_HZ};
use tspi_modal_design::ingest::PositionRole;
use tspi_modal_design::timebase::{CommonD0, PhaseBasis};

use crate::capture::UMIK_ORIENTATION_VERTICAL;
use crate::job::{sha256_hex, ArtifactKind, ArtifactRecord, StageCtx, StageFailure};

// ---------------------------------------------------------------------------
// schema / 上限 / 质量门常量（mimo-t6 §3.2/§3.5、ACCEPTANCE §0.2、WBS T63）
// ---------------------------------------------------------------------------

/// 测点计划 artifact schema（positions/plan.json；T6 冻结清单 positions 段的机读来源）。
pub const POSITION_PLAN_SCHEMA: &str = "tspi-calibration-positions/v1";
/// 测点质量报告 schema（positions/quality-<scope>.json）。
pub const POSITION_QUALITY_SCHEMA: &str = "tspi-calibration-position-quality/v1";

/// plan artifact 相对 job artifacts 目录的路径。
pub const PLAN_ARTIFACT_PATH: &str = "positions/plan.json";
/// design 组质量报告 artifact 路径。
pub const QUALITY_DESIGN_ARTIFACT_PATH: &str = "positions/quality-design.json";
/// validation/holdout 组质量报告 artifact 路径。
pub const QUALITY_VALIDATION_ARTIFACT_PATH: &str = "positions/quality-validation.json";

/// 4.1.2 正式 T6 的 design 测点下限（mimo-t6 §3.2：>=6；5+3 仅 pilot=I-03）。
pub const T6_MIN_DESIGN_POINTS: usize = 6;
/// 4.1.2 正式 T6 的 holdout/validation 测点下限（mimo-t6 §3.2：>=3）。
pub const T6_MIN_HOLDOUT_POINTS: usize = 3;
/// 推荐总测点数（mimo-t6 §3.2：6+3）。
pub const T6_RECOMMENDED_POINTS: usize = T6_MIN_DESIGN_POINTS + T6_MIN_HOLDOUT_POINTS;

/// 单个 plan 的测点总数上限（T60 JobRequest：design <=64、holdout <=16）。
pub const MAX_POINTS: usize = 80;
/// design 测点数上限（与 T60 JobRequest.design_points 上限一致）。
pub const MAX_DESIGN_POINTS: usize = 64;
/// validation/holdout 测点数上限（与 T60 JobRequest.holdout_points 上限一致）。
pub const MAX_VALIDATION_POINTS: usize = 16;
/// 测点 label 最大字符数。
pub const MAX_LABEL_CHARS: usize = 64;
/// session/layout 等标识串最大字符数。
pub const MAX_ID_CHARS: usize = 128;
/// 相位基准声明串最大字符数。
pub const MAX_PHASE_REF_CHARS: usize = 256;
/// 权重上界（仅防溢出/坏输入；不归一化，归一化由 T52/T53 负责）。
pub const MAX_WEIGHT: f64 = 1.0e9;

/// design/off 每测点最少有效重复次数（mimo-t6 §3.4：design/off >=3，与 B2 T2 对齐）。
pub const MIN_DESIGN_REPEATS: u32 = 3;
/// on 每测点最少有效重复次数（mimo-t6 §3.4：on >=2）。
pub const MIN_ON_REPEATS: u32 = 2;
/// 每测点重复次数上限（T61/T62 MAX_REPEATS=10）。
pub const MAX_REPEATS: u32 = 10;

/// 重复延迟极差上限（sample；mimo-t6 §3.5：3 次重复 <=0.3 sample）。
pub const DELAY_SPREAD_MAX_SAMPLES: f64 = 0.3;
/// 重复幅频极差上限（dB；mimo-t6 §3.5：1/12 oct 平滑后 <=0.4 dB）。
pub const MAGNITUDE_SPREAD_MAX_DB: f64 = 0.4;
/// 幅频极差检查频带（Hz；mimo-t6 §3.5 / B2 T2 100-10k）。
pub const MAGNITUDE_SPREAD_BAND_HZ: (f64, f64) = (100.0, 10_000.0);
/// SNR 双门之一：全带 200 Hz-8 kHz（带限用自身通带）>=25 dB（ACCEPTANCE §0.5 / mimo-t6 §3.5）。
pub const SNR_MIN_DB: f64 = 25.0;
/// SNR 双门之二：T6-M 低带 63-250 Hz >=25 dB（mimo-t6 §3.5）。
pub const LOW_BAND_SNR_MIN_DB: f64 = 25.0;
/// SNR 全带约定频带（报告/复核用，本模块不重算频谱）。
pub const SNR_FULL_BAND_HZ: (f64, f64) = (200.0, 8_000.0);
/// SNR 低频带（报告/复核用，本模块不重算频谱）。
pub const SNR_LOW_BAND_HZ: (f64, f64) = (63.0, 250.0);

/// 相干性门（mimo-t6 §3.5：magnitude-squared coherence >=0.9 的点 >=80%）。
pub const COHERENCE_MIN: f64 = 0.9;
/// 相干性门覆盖比例（mimo-t6 §3.5）。
pub const COHERENCE_MIN_FRACTION: f64 = 0.8;

/// 补偿后 10 kHz 残余相位上限（deg；mimo-t6 §3.5 / ACCEPTANCE §0.2.4）。
pub const RESIDUAL_PHASE_10K_MAX_DEG: f64 = 15.0;
/// 残余相位可用性要求 sigma_ppm（mimo-t6 §3.5 / ACCEPTANCE §0.2.4）。
pub const RESIDUAL_PHASE_SIGMA_MAX_PPM: f64 = 0.4;
/// 残余相位 15° 判定的默认时间跨度（秒；ACCEPTANCE §0.2.4）。
pub const RESIDUAL_PHASE_DEFAULT_SPAN_SECONDS: f64 = 10.4;
/// 残余相位 15° 判定的替代跨度（秒；ACCEPTANCE §0.2.4 >=60 s）。
pub const RESIDUAL_PHASE_MIN_SPAN_SECONDS: f64 = 60.0;
/// 残余相位 15° 判定的替代重复数（ACCEPTANCE §0.2.4 >=5 遍）。
pub const RESIDUAL_PHASE_MIN_REPEATS: u32 = 5;

/// 漂移显著性因子：|ppm| > 2 sigma 才可称漂移并要求声明校正（ACCEPTANCE §0.2.2 / mimo-t6 FC-06）。
pub const DRIFT_SIGNIFICANCE_SIGMA: f64 = 2.0;
/// 测得公共延迟相对声明 D0 的允许残差（sample；mimo-t6 §3.5：<=±0.5 sample）。
pub const COMMON_D_RESIDUAL_MAX_SAMPLES: f64 = 0.5;

// ---------------------------------------------------------------------------
// 稳定原因码/错误码（报告 JSON、REST、evidence 引用；未知一律拒收）
// ---------------------------------------------------------------------------

/// 质量门：全带 SNR 低于门。
pub const Q_SNR_LOW: &str = "E_POS_SNR_LOW";
/// 质量门：低频带 SNR 低于门。
pub const Q_SNR_LOWBAND_LOW: &str = "E_POS_SNR_LOWBAND_LOW";
/// 质量门：clock 估计无效（clock_ppm_ok=false）。
pub const Q_CLOCK_INVALID: &str = "E_POS_CLOCK_INVALID";
/// 质量门：显著漂移（|ppm|>2σ）但未声明校正。
pub const Q_CLOCK_DRIFT_UNCORRECTED: &str = "E_POS_CLOCK_DRIFT_UNCORRECTED";
/// 质量门：残余相位 10 kHz 超过 15°。
pub const Q_RESIDUAL_PHASE_HIGH: &str = "E_POS_RESIDUAL_PHASE_HIGH";
/// 质量门：残余相位声明跨度/σ 不满足 ACCEPTANCE §0.2.4。
pub const Q_RESIDUAL_PHASE_WINDOW: &str = "E_POS_RESIDUAL_PHASE_WINDOW";
/// 质量门：公共 D 残差超过 ±0.5 sample。
pub const Q_COMMON_D_RESIDUAL: &str = "E_POS_COMMON_D_RESIDUAL";
/// 质量门：重复延迟极差超过 0.3 sample。
pub const Q_DELAY_SPREAD: &str = "E_POS_DELAY_SPREAD";
/// 质量门：重复幅频极差超过 0.4 dB。
pub const Q_MAGNITUDE_SPREAD: &str = "E_POS_MAGNITUDE_SPREAD";
/// 质量门：重复间极性不一致（不得猜）。
pub const Q_POLARITY_INCONSISTENT: &str = "E_POS_POLARITY_INCONSISTENT";
/// 质量门：相干性低于 0.9。
pub const Q_COHERENCE_LOW: &str = "E_POS_COHERENCE_LOW";
/// 质量门：onset 无效（T62 onset_invalid=true）。
pub const Q_ONSET_INVALID: &str = "E_POS_ONSET_INVALID";
/// 门：design 可用测点数不足 T6_MIN_DESIGN_POINTS。
pub const Q_DESIGN_POINTS_USABLE: &str = "E_POS_DESIGN_POINTS_USABLE";
/// 门：validation/holdout 可用测点数不足 T6_MIN_HOLDOUT_POINTS。
pub const Q_VALIDATION_POINTS_USABLE: &str = "E_POS_VALIDATION_POINTS_USABLE";
/// job stage 门：质量报告不允许继续（应失败/INCOMPLETE，不得拟合）。
pub const Q_QUALITY_INCOMPLETE: &str = "E_POS_QUALITY_INCOMPLETE";

/// 缺少全带 SNR 度量（DoD：缺 SNR 不继续）。
pub const E_SNR_MISSING: &str = "E_POS_SNR_MISSING";
/// 缺少低频带 SNR 度量。
pub const E_SNR_LOWBAND_MISSING: &str = "E_POS_SNR_LOWBAND_MISSING";
/// 缺少 clock 度量（ppm/uncertainty/ok）。
pub const E_CLOCK_MISSING: &str = "E_POS_CLOCK_MISSING";
/// 缺少 delay 度量。
pub const E_DELAY_MISSING: &str = "E_POS_DELAY_MISSING";
/// 缺少 phase_ref 声明（DoD：缺 phase 不继续）。
pub const E_PHASE_MISSING: &str = "E_POS_PHASE_MISSING";
/// phase_ref 语义非法/不支持。
pub const E_PHASE_REF_INVALID: &str = "E_POS_PHASE_REF_INVALID";
/// 缺少极性（未知极性不得猜）。
pub const E_POLARITY_MISSING: &str = "E_POS_POLARITY_MISSING";
/// 极性=0（unknown；不得猜）。
pub const E_POLARITY_UNKNOWN: &str = "E_POS_POLARITY_UNKNOWN";
/// 缺少残余相位证据。
pub const E_RESIDUAL_PHASE_MISSING: &str = "E_POS_RESIDUAL_PHASE_MISSING";
/// 缺少公共 D 残差证据。
pub const E_COMMON_D_MISSING: &str = "E_POS_COMMON_D_MISSING";
/// 缺少重复幅频数据。
pub const E_MAGNITUDE_MISSING: &str = "E_POS_MAGNITUDE_MISSING";
/// 幅频网格/长度非法。
pub const E_MAGNITUDE_GRID: &str = "E_POS_MAGNITUDE_GRID";
/// 相干性值非法（非有限/越界）。
pub const E_COHERENCE_INVALID: &str = "E_POS_COHERENCE_INVALID";

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// T63 [NONRT] 错误（稳定 code 供 REST/log/evidence；未知/越界一律拒绝，不静默降级）。
#[derive(Debug, Clone, PartialEq)]
pub enum PositionsError {
    /// 配置/字段非法（字段级消息）。
    InvalidConfig {
        /// 字段名。
        field: &'static str,
        /// 人读说明。
        detail: String,
    },
    /// plan 内 position label 重复。
    PlanDuplicateLabel {
        /// 重复的 label。
        label: String,
    },
    /// plan 测点数非法（0/超上限）。
    PlanPoints {
        /// 实际总数。
        total: usize,
        /// 上限。
        limit: usize,
        /// 细节。
        detail: String,
    },
    /// design 测点数不足 T6 下限（mimo-t6 §3.2 I-03）。
    PlanDesignPoints {
        /// 实际。
        actual: usize,
        /// 要求。
        required: usize,
    },
    /// validation/holdout 测点数不足 T6 下限。
    PlanHoldoutPoints {
        /// 实际。
        actual: usize,
        /// 要求。
        required: usize,
    },
    /// plan 相位基准声明非法。
    PlanPhaseRef {
        /// 细节。
        detail: String,
    },
    /// 听音位麦克风朝向不是 90deg（ACCEPTANCE §0.1.2：多声道听音位必须 90°）。
    MicOrientation {
        /// 测点 label。
        label: String,
        /// 实际声明（None = 未声明）。
        actual: Option<String>,
    },
    /// quality scope 在 plan 内没有对应测点。
    QualityScopeEmpty {
        /// scope 机器名。
        scope: &'static str,
    },
    /// quality 输入引用了 plan 中不存在的测点。
    PositionUnknown {
        /// label。
        label: String,
    },
    /// quality 输入缺少 plan 中应有的测点。
    PositionMissing {
        /// label。
        label: String,
    },
    /// quality 输入重复提供同一测点。
    PositionDuplicate {
        /// label。
        label: String,
    },
    /// design/validation 分组越界（holdout 输入进 design scope 或反之）。
    GroupLeak {
        /// label。
        label: String,
        /// 实际角色机器名。
        role: &'static str,
    },
    /// 未知 participant。
    ParticipantUnknown {
        /// speaker。
        speaker: String,
    },
    /// 缺少 participant 的 measurement。
    ParticipantMissing {
        /// speaker。
        speaker: String,
    },
    /// participant 重复。
    ParticipantDuplicate {
        /// speaker。
        speaker: String,
    },
    /// 重复次数不足（design/off >=3、on >=2；缺重复质量不继续）。
    RepeatsInsufficient {
        /// 测点。
        point: String,
        /// participant。
        speaker: String,
        /// 要求。
        required: u32,
        /// 实际。
        actual: u32,
    },
    /// 重复序号非法/重复。
    RepeatsDuplicate {
        /// 测点。
        point: String,
        /// participant。
        speaker: String,
        /// 重复序号。
        repeat: u32,
    },
    /// 缺少必需度量（SNR/clock/phase 等；不补默认值、不继续）。
    MissingMetric {
        /// 测点。
        point: String,
        /// participant。
        speaker: String,
        /// 重复序号。
        repeat: u32,
        /// 度量名。
        metric: &'static str,
        /// 稳定错误码。
        code: &'static str,
    },
    /// 度量值非法（非有限/越界/长度不符）。
    InvalidMetric {
        /// 测点。
        point: String,
        /// participant。
        speaker: String,
        /// 重复序号。
        repeat: u32,
        /// 度量名。
        metric: &'static str,
        /// 稳定错误码。
        code: &'static str,
        /// 细节。
        detail: String,
    },
    /// artifact schema 不符。
    ArtifactSchema {
        /// 期望。
        expected: &'static str,
        /// 实际。
        actual: String,
    },
    /// artifact 自 hash/plan hash 校验失败。
    ArtifactHash {
        /// 细节。
        detail: String,
    },
    /// quality artifact 与 plan 不匹配（plan 变更后 stale）。
    StaleQuality {
        /// plan 当前 hash。
        expected_plan_sha256: String,
        /// artifact 内 hash。
        actual_plan_sha256: String,
    },
    /// JSON 解析失败（严格，无第三方依赖）。
    Json {
        /// 细节。
        detail: String,
    },
}

impl PositionsError {
    /// 稳定错误码（REST/log/evidence）。
    pub fn code(&self) -> &'static str {
        match self {
            PositionsError::InvalidConfig { .. } => "E_POS_CONFIG",
            PositionsError::PlanDuplicateLabel { .. } => "E_POS_PLAN_DUPLICATE_LABEL",
            PositionsError::PlanPoints { .. } => "E_POS_PLAN_POINTS",
            PositionsError::PlanDesignPoints { .. } => "E_POS_PLAN_DESIGN_POINTS",
            PositionsError::PlanHoldoutPoints { .. } => "E_POS_PLAN_HOLDOUT_POINTS",
            PositionsError::PlanPhaseRef { .. } => "E_POS_PLAN_PHASE_REF",
            PositionsError::MicOrientation { .. } => "E_POS_MIC_ORIENTATION",
            PositionsError::QualityScopeEmpty { .. } => "E_POS_SCOPE_EMPTY",
            PositionsError::PositionUnknown { .. } => "E_POS_POSITION_UNKNOWN",
            PositionsError::PositionMissing { .. } => "E_POS_POSITION_MISSING",
            PositionsError::PositionDuplicate { .. } => "E_POS_POSITION_DUPLICATE",
            PositionsError::GroupLeak { .. } => "E_POS_GROUP_LEAK",
            PositionsError::ParticipantUnknown { .. } => "E_POS_PARTICIPANT_UNKNOWN",
            PositionsError::ParticipantMissing { .. } => "E_POS_PARTICIPANT_MISSING",
            PositionsError::ParticipantDuplicate { .. } => "E_POS_PARTICIPANT_DUPLICATE",
            PositionsError::RepeatsInsufficient { .. } => "E_POS_REPEATS_INSUFFICIENT",
            PositionsError::RepeatsDuplicate { .. } => "E_POS_REPEATS_DUPLICATE",
            PositionsError::MissingMetric { code, .. } => *code,
            PositionsError::InvalidMetric { code, .. } => *code,
            PositionsError::ArtifactSchema { .. } => "E_POS_ARTIFACT_SCHEMA",
            PositionsError::ArtifactHash { .. } => "E_POS_ARTIFACT_HASH",
            PositionsError::StaleQuality { .. } => "E_POS_QUALITY_STALE",
            PositionsError::Json { .. } => "E_POS_JSON",
        }
    }
}

impl fmt::Display for PositionsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PositionsError::InvalidConfig { field, detail } => {
                write!(f, "{}: invalid config field {field}: {detail}", self.code())
            }
            PositionsError::PlanDuplicateLabel { label } => {
                write!(f, "{}: duplicate position label {label:?}", self.code())
            }
            PositionsError::PlanPoints {
                total,
                limit,
                detail,
            } => write!(
                f,
                "{}: invalid plan point count {total} (limit {limit}): {detail}",
                self.code()
            ),
            PositionsError::PlanDesignPoints { actual, required } => write!(
                f,
                "{}: design points {actual} < required {required} (mimo-t6 §3.2)",
                self.code()
            ),
            PositionsError::PlanHoldoutPoints { actual, required } => write!(
                f,
                "{}: holdout/validation points {actual} < required {required} (mimo-t6 §3.2)",
                self.code()
            ),
            PositionsError::PlanPhaseRef { detail } => {
                write!(f, "{}: invalid phase_ref: {detail}", self.code())
            }
            PositionsError::MicOrientation { label, actual } => write!(
                f,
                "{}: listening position {label:?} must declare mic_orientation {UMIK_ORIENTATION_VERTICAL:?} (got {actual:?})",
                self.code()
            ),
            PositionsError::QualityScopeEmpty { scope } => write!(
                f,
                "{}: plan has no {scope} positions to evaluate",
                self.code()
            ),
            PositionsError::PositionUnknown { label } => write!(
                f,
                "{}: quality input references unknown position {label:?}",
                self.code()
            ),
            PositionsError::PositionMissing { label } => write!(
                f,
                "{}: quality input is missing planned position {label:?}",
                self.code()
            ),
            PositionsError::PositionDuplicate { label } => write!(
                f,
                "{}: duplicate quality input for position {label:?}",
                self.code()
            ),
            PositionsError::GroupLeak { label, role } => write!(
                f,
                "{}: position {label:?} has role {role} and must not be passed to the other quality scope (holdout contamination guard)",
                self.code()
            ),
            PositionsError::ParticipantUnknown { speaker } => write!(
                f,
                "{}: unknown participant {speaker:?} (not in plan.participants)",
                self.code()
            ),
            PositionsError::ParticipantMissing { speaker } => write!(
                f,
                "{}: missing measurement for participant {speaker:?}",
                self.code()
            ),
            PositionsError::ParticipantDuplicate { speaker } => write!(
                f,
                "{}: duplicate measurement for participant {speaker:?}",
                self.code()
            ),
            PositionsError::RepeatsInsufficient {
                point,
                speaker,
                required,
                actual,
            } => write!(
                f,
                "{}: {point}/{speaker} has {actual} repeats, requires >= {required}",
                self.code()
            ),
            PositionsError::RepeatsDuplicate {
                point,
                speaker,
                repeat,
            } => write!(
                f,
                "{}: {point}/{speaker} repeat index {repeat} duplicated/out of order",
                self.code()
            ),
            PositionsError::MissingMetric {
                point,
                speaker,
                repeat,
                metric,
                code,
            } => write!(
                f,
                "{code}: {point}/{speaker} repeat {repeat} is missing required metric {metric} (quality gates never continue without it)"
            ),
            PositionsError::InvalidMetric {
                point,
                speaker,
                repeat,
                metric,
                code,
                detail,
            } => write!(
                f,
                "{code}: {point}/{speaker} repeat {repeat} metric {metric} is invalid: {detail}"
            ),
            PositionsError::ArtifactSchema { expected, actual } => write!(
                f,
                "{}: artifact schema {actual:?} does not match {expected:?}",
                self.code()
            ),
            PositionsError::ArtifactHash { detail } => {
                write!(f, "{}: {detail}", self.code())
            }
            PositionsError::StaleQuality {
                expected_plan_sha256,
                actual_plan_sha256,
            } => write!(
                f,
                "{}: quality artifact plan_sha256 {actual_plan_sha256} != current plan {expected_plan_sha256}",
                self.code()
            ),
            PositionsError::Json { detail } => write!(f, "{}: {detail}", self.code()),
        }
    }
}

impl std::error::Error for PositionsError {}

// ---------------------------------------------------------------------------
// 严格 JSON（无第三方依赖；只服务本模块 artifact 的读写）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

fn json_error(detail: impl Into<String>) -> PositionsError {
    PositionsError::Json {
        detail: detail.into(),
    }
}

fn parse_json(text: &str) -> Result<Json, PositionsError> {
    let mut parser = JsonParser {
        bytes: text.as_bytes(),
        pos: 0,
    };
    let value = parser.parse_value()?;
    parser.skip_whitespace();
    if parser.pos != parser.bytes.len() {
        return Err(json_error(format!(
            "trailing bytes after JSON value at byte {}",
            parser.pos
        )));
    }
    Ok(value)
}

struct JsonParser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> JsonParser<'a> {
    fn error(&self, detail: impl Into<String>) -> PositionsError {
        json_error(format!("at byte {}: {}", self.pos, detail.into()))
    }

    fn skip_whitespace(&mut self) {
        while let Some(byte) = self.bytes.get(self.pos) {
            if matches!(byte, b' ' | b'\n' | b'\r' | b'\t') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn expect(&mut self, expected: u8) -> Result<(), PositionsError> {
        if self.peek() == Some(expected) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.error(format!("expected {:?}", expected as char)))
        }
    }

    fn parse_literal(&mut self, literal: &str) -> Result<(), PositionsError> {
        if self.bytes[self.pos..].starts_with(literal.as_bytes()) {
            self.pos += literal.len();
            Ok(())
        } else {
            Err(self.error(format!("expected literal {literal}")))
        }
    }

    fn parse_value(&mut self) -> Result<Json, PositionsError> {
        self.skip_whitespace();
        match self.peek() {
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'"') => self.parse_string().map(Json::String),
            Some(b't') => {
                self.parse_literal("true")?;
                Ok(Json::Bool(true))
            }
            Some(b'f') => {
                self.parse_literal("false")?;
                Ok(Json::Bool(false))
            }
            Some(b'n') => {
                self.parse_literal("null")?;
                Ok(Json::Null)
            }
            Some(b'-') | Some(b'0'..=b'9') => self.parse_number().map(Json::Number),
            Some(other) => Err(self.error(format!("unexpected byte {:?}", other as char))),
            None => Err(self.error("unexpected end of JSON input")),
        }
    }

    fn parse_number(&mut self) -> Result<f64, PositionsError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| self.error("number is not valid UTF-8"))?;
        if text.is_empty() || text == "-" {
            return Err(self.error("invalid number"));
        }
        let value: f64 = text.parse().map_err(|_| self.error("invalid number"))?;
        if !value.is_finite() {
            return Err(self.error("number is not finite"));
        }
        Ok(value)
    }

    fn parse_string(&mut self) -> Result<String, PositionsError> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let byte = self
                .peek()
                .ok_or_else(|| self.error("unterminated string"))?;
            match byte {
                b'"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.pos += 1;
                    let escape = self
                        .peek()
                        .ok_or_else(|| self.error("unterminated escape"))?;
                    self.pos += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{08}'),
                        b'f' => out.push('\u{0c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.parse_unicode_escape()?),
                        other => {
                            return Err(self
                                .error(format!("unsupported escape backslash-{}", other as char)))
                        }
                    }
                }
                _ => {
                    let rest = std::str::from_utf8(&self.bytes[self.pos..])
                        .map_err(|_| self.error("string is not valid UTF-8"))?;
                    let character = rest
                        .chars()
                        .next()
                        .ok_or_else(|| self.error("unexpected end of string"))?;
                    out.push(character);
                    self.pos += character.len_utf8();
                }
            }
        }
    }

    fn parse_unicode_escape(&mut self) -> Result<char, PositionsError> {
        if self.pos + 4 > self.bytes.len() {
            return Err(self.error("truncated unicode escape"));
        }
        let hex = std::str::from_utf8(&self.bytes[self.pos..self.pos + 4])
            .map_err(|_| self.error("invalid unicode escape"))?;
        let value =
            u32::from_str_radix(hex, 16).map_err(|_| self.error("invalid unicode escape"))?;
        self.pos += 4;
        let character = char::from_u32(value)
            .ok_or_else(|| self.error("unicode escape is not a valid scalar value"))?;
        if character.is_control() {
            return Err(self.error("control characters must be escaped by their JSON form"));
        }
        Ok(character)
    }

    fn parse_array(&mut self) -> Result<Json, PositionsError> {
        self.expect(b'[')?;
        self.skip_whitespace();
        let mut items = Vec::new();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Array(items));
        }
        loop {
            items.push(self.parse_value()?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Json::Array(items));
                }
                _ => return Err(self.error("expected ',' or ']' in array")),
            }
        }
    }

    fn parse_object(&mut self) -> Result<Json, PositionsError> {
        self.expect(b'{')?;
        self.skip_whitespace();
        let mut entries: Vec<(String, Json)> = Vec::new();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Object(entries));
        }
        loop {
            self.skip_whitespace();
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            let value = self.parse_value()?;
            if entries.iter().any(|(existing, _)| existing == &key) {
                return Err(self.error(format!("duplicate object key {key:?}")));
            }
            entries.push((key, value));
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Json::Object(entries));
                }
                _ => return Err(self.error("expected ',' or '}' in object")),
            }
        }
    }
}

fn json_object<'a>(value: &'a Json, what: &str) -> Result<&'a [(String, Json)], PositionsError> {
    match value {
        Json::Object(entries) => Ok(entries),
        _ => Err(json_error(format!("{what} must be a JSON object"))),
    }
}

fn json_field<'a>(entries: &'a [(String, Json)], key: &str) -> Option<&'a Json> {
    entries
        .iter()
        .find(|(existing, _)| existing == key)
        .map(|(_, value)| value)
}

fn json_required<'a>(
    entries: &'a [(String, Json)],
    key: &str,
    what: &str,
) -> Result<&'a Json, PositionsError> {
    json_field(entries, key)
        .ok_or_else(|| json_error(format!("{what} is missing required field {key:?}")))
}

fn json_as_string<'a>(value: &'a Json, what: &str) -> Result<&'a str, PositionsError> {
    match value {
        Json::String(text) => Ok(text.as_str()),
        _ => Err(json_error(format!("{what} must be a JSON string"))),
    }
}

fn json_as_bool(value: &Json, what: &str) -> Result<bool, PositionsError> {
    match value {
        Json::Bool(flag) => Ok(*flag),
        _ => Err(json_error(format!("{what} must be a JSON boolean"))),
    }
}

fn json_as_number(value: &Json, what: &str) -> Result<f64, PositionsError> {
    match value {
        Json::Number(number) => Ok(*number),
        _ => Err(json_error(format!("{what} must be a JSON number"))),
    }
}

fn json_as_u64(value: &Json, what: &str) -> Result<u64, PositionsError> {
    let number = json_as_number(value, what)?;
    if number < 0.0 || number.fract() != 0.0 || number > u64::MAX as f64 {
        return Err(json_error(format!("{what} must be a non-negative integer")));
    }
    Ok(number as u64)
}

fn json_as_i64(value: &Json, what: &str) -> Result<i64, PositionsError> {
    let number = json_as_number(value, what)?;
    if number.fract() != 0.0 || number < i64::MIN as f64 || number > i64::MAX as f64 {
        return Err(json_error(format!("{what} must be an integer")));
    }
    Ok(number as i64)
}

fn json_as_array<'a>(value: &'a Json, what: &str) -> Result<&'a [Json], PositionsError> {
    match value {
        Json::Array(items) => Ok(items.as_slice()),
        _ => Err(json_error(format!("{what} must be a JSON array"))),
    }
}

fn json_string_field<'a>(
    entries: &'a [(String, Json)],
    key: &str,
    what: &str,
) -> Result<&'a str, PositionsError> {
    json_as_string(json_required(entries, key, what)?, key)
}

fn json_number_field(
    entries: &[(String, Json)],
    key: &str,
    what: &str,
) -> Result<f64, PositionsError> {
    json_as_number(json_required(entries, key, what)?, key)
}

fn json_bool_field(
    entries: &[(String, Json)],
    key: &str,
    what: &str,
) -> Result<bool, PositionsError> {
    json_as_bool(json_required(entries, key, what)?, key)
}

fn json_u64_field(
    entries: &[(String, Json)],
    key: &str,
    what: &str,
) -> Result<u64, PositionsError> {
    json_as_u64(json_required(entries, key, what)?, key)
}

// JSON 写入（确定性；调用前必须保证 f64 有限、字符串已过校验）。
fn push_json_str(out: &mut String, value: &str) {
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            other if (other as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", other as u32);
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

fn push_json_number(out: &mut String, value: f64) {
    if value.is_finite() {
        let _ = write!(out, "{value}");
    } else {
        out.push_str("null");
    }
}

fn push_json_opt_number(out: &mut String, value: Option<f64>) {
    match value {
        Some(value) => push_json_number(out, value),
        None => out.push_str("null"),
    }
}

fn push_json_opt_str(out: &mut String, value: Option<&str>) {
    match value {
        Some(value) => push_json_str(out, value),
        None => out.push_str("null"),
    }
}

// ---------------------------------------------------------------------------
// 测点（label/role/weight 必填；坐标/朝向可选，不要求 xyz）
// ---------------------------------------------------------------------------

fn is_clean_label(value: &str) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.chars().count() <= MAX_LABEL_CHARS
        && !value.chars().any(|character| character.is_control())
}

fn is_clean_identifier(value: &str, max_chars: usize) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.chars().count() <= max_chars
        && !value.chars().any(|character| character.is_control())
}

fn is_participant_name(value: &str) -> bool {
    let mut characters = value.chars();
    match characters.next() {
        Some(first) if first.is_ascii_uppercase() => {}
        _ => return false,
    }
    if value.len() > 16 {
        return false;
    }
    characters.all(|character| character.is_ascii_uppercase() || character.is_ascii_digit())
}

/// 一个预声明测点：label/role/weight 必填；空间坐标/麦克风朝向可选（不要求 xyz）。
#[derive(Debug, Clone, PartialEq)]
pub struct MultipointPosition {
    /// 测点 label（唯一、非空、<=64 字符、无控制字符）。
    pub label: String,
    /// 显式角色（design/validation；不从名称/坐标推断，T48 PositionRole）。
    pub role: PositionRole,
    /// 设计/验证权重（有限、>0；归一化由 T52/T53 负责）。
    pub weight: f64,
    /// 粗略空间坐标（米，可选；P1 不要求）。
    pub coordinates_m: Option<[f64; 3]>,
    /// 麦克风朝向声明（听音位 plan 内必填且必须为 90deg；ACCEPTANCE §0.1.2）。
    pub mic_orientation: Option<String>,
}

impl MultipointPosition {
    /// 构造并校验一个测点（label/role/weight 必填；坐标/朝向默认 None，听音位 plan 校验要求 90deg）。
    pub fn new(
        label: impl Into<String>,
        role: PositionRole,
        weight: f64,
    ) -> Result<Self, PositionsError> {
        let position = MultipointPosition {
            label: label.into(),
            role,
            weight,
            coordinates_m: None,
            mic_orientation: None,
        };
        position.validate()?;
        Ok(position)
    }

    /// design 测点（参与拟合；不得用于 holdout 评估）。
    pub fn design(label: impl Into<String>, weight: f64) -> Result<Self, PositionsError> {
        Self::new(label, PositionRole::Design, weight)
    }

    /// validation/holdout 测点（不得参与调参）。
    pub fn validation(label: impl Into<String>, weight: f64) -> Result<Self, PositionsError> {
        Self::new(label, PositionRole::Validation, weight)
    }

    /// 可选空间坐标（米；不要求）。
    pub fn with_coordinates(mut self, coordinates_m: [f64; 3]) -> Self {
        self.coordinates_m = Some(coordinates_m);
        self
    }

    /// 可选麦克风朝向声明。
    pub fn with_mic_orientation(mut self, orientation: impl Into<String>) -> Self {
        self.mic_orientation = Some(orientation.into());
        self
    }

    /// 严格校验（未知/越界全部拒绝）。
    pub fn validate(&self) -> Result<(), PositionsError> {
        if !is_clean_label(&self.label) {
            return Err(PositionsError::InvalidConfig {
                field: "position.label",
                detail: "must be non-empty, trimmed, <=64 chars and free of control characters"
                    .to_string(),
            });
        }
        if !self.weight.is_finite() || self.weight <= 0.0 || self.weight > MAX_WEIGHT {
            return Err(PositionsError::InvalidConfig {
                field: "position.weight",
                detail: format!("must be finite and in (0, {MAX_WEIGHT}]"),
            });
        }
        if let Some(coordinates) = self.coordinates_m {
            if !coordinates.iter().all(|value| value.is_finite()) {
                return Err(PositionsError::InvalidConfig {
                    field: "position.coordinates_m",
                    detail: "coordinates are optional, but when present must be finite".to_string(),
                });
            }
        }
        if let Some(orientation) = &self.mic_orientation {
            if orientation.trim().is_empty() || orientation.chars().count() > 32 {
                return Err(PositionsError::InvalidConfig {
                    field: "position.mic_orientation",
                    detail: "mic_orientation is optional, but when present must be non-empty and <=32 chars".to_string(),
                });
            }
        }
        Ok(())
    }

    /// 角色机器名（design/validation）。
    pub const fn role_str(&self) -> &'static str {
        self.role.as_str()
    }
}

// ---------------------------------------------------------------------------
// 多点计划（预声明、冻结、可哈希、可恢复）
// ---------------------------------------------------------------------------

/// 一个 session 的预声明多点计划（design/validation 分组 + 公共 D0/相位基准）。
///
/// 这是听音位多点测量计划：每个测点必须声明 mic_orientation = 90deg
/// （ACCEPTANCE §0.1.2；0deg 只用于单只近场，不得进入听音位 session）。
#[derive(Debug, Clone, PartialEq)]
pub struct MultipointPlan {
    /// session id（与 T61/T62/T48 session 一致）。
    pub session_id: String,
    /// 目标布局 id（如 4.1.2；实际 overlay 解析归 T57）。
    pub layout: String,
    /// 采样率（Hz；core-v1 §3.2：48k/96k）。
    pub sample_rate_hz: u32,
    /// 低频 MIMO 显式参与角色（有序；<=8；不从角色名推断）。
    pub participants: Vec<String>,
    /// T6 FC-04 预声明公共整数 D0（samples@fs；baseline/on/两轮同一值）。
    pub common_d0: CommonD0,
    /// 预声明相位基准语义串（aligned_segment/absolute；T49 PhaseBasis 解释）。
    pub phase_ref: String,
    /// 全部测点（design 与 validation 按声明顺序；label 全 plan 唯一）。
    pub positions: Vec<MultipointPosition>,
}

impl MultipointPlan {
    /// 构造并校验一个 plan（label/role/weight 必填；design/validation 均非空）。
    pub fn new(
        session_id: impl Into<String>,
        layout: impl Into<String>,
        sample_rate_hz: u32,
        participants: Vec<String>,
        common_d0: CommonD0,
        phase_ref: impl Into<String>,
        positions: Vec<MultipointPosition>,
    ) -> Result<Self, PositionsError> {
        let plan = MultipointPlan {
            session_id: session_id.into(),
            layout: layout.into(),
            sample_rate_hz,
            participants,
            common_d0,
            phase_ref: phase_ref.into(),
            positions,
        };
        plan.validate()?;
        Ok(plan)
    }

    /// 严格校验（未知/越界全部拒绝；坐标不要求，但听音位朝向必须是 90deg）。
    pub fn validate(&self) -> Result<(), PositionsError> {
        if !is_clean_identifier(&self.session_id, MAX_ID_CHARS) {
            return Err(PositionsError::InvalidConfig {
                field: "plan.session_id",
                detail: format!(
                    "must be non-empty, trimmed, <={MAX_ID_CHARS} chars, no control chars"
                ),
            });
        }
        if !is_clean_identifier(&self.layout, MAX_ID_CHARS) {
            return Err(PositionsError::InvalidConfig {
                field: "plan.layout",
                detail: format!(
                    "must be non-empty, trimmed, <={MAX_ID_CHARS} chars, no control chars"
                ),
            });
        }
        if !SAMPLE_RATES_HZ.contains(&self.sample_rate_hz) {
            return Err(PositionsError::InvalidConfig {
                field: "plan.sample_rate_hz",
                detail: format!("runtime accepts only {SAMPLE_RATES_HZ:?}"),
            });
        }
        if self.participants.is_empty() || self.participants.len() > MAX_LOW_BAND_PARTICIPANTS {
            return Err(PositionsError::InvalidConfig {
                field: "plan.participants",
                detail: format!("needs 1..={MAX_LOW_BAND_PARTICIPANTS} explicit participants"),
            });
        }
        for (index, participant) in self.participants.iter().enumerate() {
            if !is_participant_name(participant) {
                return Err(PositionsError::InvalidConfig {
                    field: "plan.participants",
                    detail: format!(
                        "participant {index} {participant:?} must match ^[A-Z][A-Z0-9]{{0,15}}$ (explicit role id)"
                    ),
                });
            }
            if self.participants[..index].contains(participant) {
                return Err(PositionsError::InvalidConfig {
                    field: "plan.participants",
                    detail: format!("duplicate participant {participant:?}"),
                });
            }
        }
        if !is_clean_identifier(&self.phase_ref, MAX_PHASE_REF_CHARS) {
            return Err(PositionsError::PlanPhaseRef {
                detail: format!(
                    "must be non-empty, trimmed, <={MAX_PHASE_REF_CHARS} chars, no control chars"
                ),
            });
        }
        match PhaseBasis::parse(&self.phase_ref) {
            Ok(PhaseBasis::DelayRemoved) => {
                return Err(PositionsError::PlanPhaseRef {
                    detail: "delay_removed has no machine-readable removed delay in v1 ingest; declare aligned_segment + CommonD0 instead".to_string(),
                })
            }
            Ok(_) => {}
            Err(error) => {
                return Err(PositionsError::PlanPhaseRef {
                    detail: format!("unsupported phase_ref semantics: {error}"),
                })
            }
        }
        if self.positions.is_empty() || self.positions.len() > MAX_POINTS {
            return Err(PositionsError::PlanPoints {
                total: self.positions.len(),
                limit: MAX_POINTS,
                detail: "plan needs 1..=MAX_POINTS positions".to_string(),
            });
        }
        let mut seen: Vec<&str> = Vec::with_capacity(self.positions.len());
        let mut design = 0usize;
        let mut validation = 0usize;
        for position in &self.positions {
            position.validate()?;
            // 听音位 session 必须是 90°（麦轴垂直朝上）；0deg/未声明/其它值明确拒收。
            if position.mic_orientation.as_deref() != Some(UMIK_ORIENTATION_VERTICAL) {
                return Err(PositionsError::MicOrientation {
                    label: position.label.clone(),
                    actual: position.mic_orientation.clone(),
                });
            }
            if seen.contains(&position.label.as_str()) {
                return Err(PositionsError::PlanDuplicateLabel {
                    label: position.label.clone(),
                });
            }
            seen.push(position.label.as_str());
            match position.role {
                PositionRole::Design => design += 1,
                PositionRole::Validation => validation += 1,
            }
        }
        if design == 0 || validation == 0 {
            return Err(PositionsError::InvalidConfig {
                field: "plan.positions",
                detail: "both design and validation/holdout groups must be declared (pre-declared split; holdout never used for tuning)".to_string(),
            });
        }
        if design > MAX_DESIGN_POINTS {
            return Err(PositionsError::PlanPoints {
                total: design,
                limit: MAX_DESIGN_POINTS,
                detail: "too many design positions".to_string(),
            });
        }
        if validation > MAX_VALIDATION_POINTS {
            return Err(PositionsError::PlanPoints {
                total: validation,
                limit: MAX_VALIDATION_POINTS,
                detail: "too many validation/holdout positions".to_string(),
            });
        }
        Ok(())
    }

    /// 设计中必须满足的 T6 最低点数（6 design + 3 holdout；5+3 仅 pilot -> I-03，不得 PASS）。
    pub fn require_t6_minimums(&self) -> Result<(), PositionsError> {
        let design = self.design_count();
        if design < T6_MIN_DESIGN_POINTS {
            return Err(PositionsError::PlanDesignPoints {
                actual: design,
                required: T6_MIN_DESIGN_POINTS,
            });
        }
        let validation = self.validation_count();
        if validation < T6_MIN_HOLDOUT_POINTS {
            return Err(PositionsError::PlanHoldoutPoints {
                actual: validation,
                required: T6_MIN_HOLDOUT_POINTS,
            });
        }
        Ok(())
    }

    /// design 测点迭代器（只用于拟合；holdout 不得进入调参）。
    pub fn design_positions(&self) -> impl Iterator<Item = &MultipointPosition> {
        self.positions
            .iter()
            .filter(|position| position.role == PositionRole::Design)
    }

    /// validation/holdout 测点迭代器（不参与调参）。
    pub fn validation_positions(&self) -> impl Iterator<Item = &MultipointPosition> {
        self.positions
            .iter()
            .filter(|position| position.role == PositionRole::Validation)
    }

    /// design 测点数。
    pub fn design_count(&self) -> usize {
        self.design_positions().count()
    }

    /// validation/holdout 测点数。
    pub fn validation_count(&self) -> usize {
        self.validation_positions().count()
    }

    /// 总测点数。
    pub fn total_count(&self) -> usize {
        self.positions.len()
    }

    /// design 权重和（仅报告；本模块不归一化）。
    pub fn design_weight_sum(&self) -> f64 {
        self.design_positions()
            .map(|position| position.weight)
            .sum()
    }

    /// 按 label 取测点。
    pub fn position(&self, label: &str) -> Option<&MultipointPosition> {
        self.positions
            .iter()
            .find(|position| position.label == label)
    }

    /// 本 plan 的相位基准语义（解析后的 T49 类型）。
    pub fn phase_basis(&self) -> Result<PhaseBasis, PositionsError> {
        PhaseBasis::parse(&self.phase_ref).map_err(|error| PositionsError::PlanPhaseRef {
            detail: error.to_string(),
        })
    }

    fn write_json(&self, out: &mut String, include_hash: bool) {
        out.push('{');
        out.push_str("\"schema\":");
        push_json_str(out, POSITION_PLAN_SCHEMA);
        out.push_str(",\"session_id\":");
        push_json_str(out, &self.session_id);
        out.push_str(",\"layout\":");
        push_json_str(out, &self.layout);
        let _ = write!(out, ",\"sample_rate_hz\":{}", self.sample_rate_hz);
        out.push_str(",\"participants\":[");
        for (index, participant) in self.participants.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            push_json_str(out, participant);
        }
        let _ = write!(
            out,
            "],\"common_d0_samples\":{},\"phase_ref\":",
            self.common_d0.samples()
        );
        push_json_str(out, &self.phase_ref);
        out.push_str(",\"design\":[");
        for (index, position) in self
            .positions
            .iter()
            .filter(|position| position.role == PositionRole::Design)
            .enumerate()
        {
            if index > 0 {
                out.push(',');
            }
            write_position_json(out, position);
        }
        out.push_str("],\"holdout\":[");
        for (index, position) in self
            .positions
            .iter()
            .filter(|position| position.role == PositionRole::Validation)
            .enumerate()
        {
            if index > 0 {
                out.push(',');
            }
            write_position_json(out, position);
        }
        let _ = write!(
            out,
            "],\"counts\":{{\"design\":{},\"validation\":{}}}",
            self.design_count(),
            self.validation_count()
        );
        if include_hash {
            out.push_str(",\"plan_sha256\":");
            push_json_str(out, &self.plan_sha256());
        }
        out.push('}');
    }

    fn json_without_hash(&self) -> String {
        let mut out = String::with_capacity(2048);
        self.write_json(&mut out, false);
        out
    }

    /// plan 自 hash（sha256；对不含 plan_sha256 字段的确定性 JSON 计算）。
    pub fn plan_sha256(&self) -> String {
        sha256_hex(self.json_without_hash().as_bytes())
    }

    /// 确定性 JSON（tspi-calibration-positions/v1；含 plan_sha256 自 hash）。
    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(2048);
        self.write_json(&mut out, true);
        out
    }

    /// 从 JSON artifact 严格重建 plan 并校验自 hash（job 重启恢复入口）。
    pub fn from_json(text: &str) -> Result<Self, PositionsError> {
        let root = parse_json(text)?;
        let entries = json_object(&root, "positions plan")?;
        let schema = json_string_field(entries, "schema", "positions plan")?;
        if schema != POSITION_PLAN_SCHEMA {
            return Err(PositionsError::ArtifactSchema {
                expected: POSITION_PLAN_SCHEMA,
                actual: schema.to_string(),
            });
        }
        let session_id = json_string_field(entries, "session_id", "positions plan")?.to_string();
        let layout = json_string_field(entries, "layout", "positions plan")?.to_string();
        let sample_rate_hz = json_u64_field(entries, "sample_rate_hz", "positions plan")? as u32;
        let participants_json = json_as_array(
            json_required(entries, "participants", "positions plan")?,
            "positions plan.participants",
        )?;
        let mut participants = Vec::with_capacity(participants_json.len());
        for participant in participants_json {
            participants
                .push(json_as_string(participant, "positions plan.participants[]")?.to_string());
        }
        let common_d0_samples = json_as_i64(
            json_required(entries, "common_d0_samples", "positions plan")?,
            "positions plan.common_d0_samples",
        )?;
        let common_d0 =
            CommonD0::new(common_d0_samples).map_err(|error| PositionsError::InvalidConfig {
                field: "plan.common_d0_samples",
                detail: error.to_string(),
            })?;
        let phase_ref = json_string_field(entries, "phase_ref", "positions plan")?.to_string();
        let mut positions = Vec::new();
        for (field, role) in [
            ("design", PositionRole::Design),
            ("holdout", PositionRole::Validation),
        ] {
            let array = json_as_array(
                json_required(entries, field, "positions plan")?,
                "positions plan position group",
            )?;
            for value in array {
                positions.push(parse_position_json(value, role)?);
            }
        }
        let plan = MultipointPlan {
            session_id,
            layout,
            sample_rate_hz,
            participants,
            common_d0,
            phase_ref,
            positions,
        };
        plan.validate()?;
        if let Some(counts) = json_field(entries, "counts") {
            let counts = json_object(counts, "positions plan.counts")?;
            let design = json_u64_field(counts, "design", "positions plan.counts")? as usize;
            let validation =
                json_u64_field(counts, "validation", "positions plan.counts")? as usize;
            if design != plan.design_count() || validation != plan.validation_count() {
                return Err(PositionsError::ArtifactHash {
                    detail: format!(
                        "positions plan counts mismatch: artifact design={design} validation={validation}, computed design={} validation={}",
                        plan.design_count(),
                        plan.validation_count()
                    ),
                });
            }
        }
        let recorded = json_string_field(entries, "plan_sha256", "positions plan")?;
        let computed = plan.plan_sha256();
        if recorded != computed {
            return Err(PositionsError::ArtifactHash {
                detail: format!(
                    "positions plan self-hash mismatch: artifact={recorded} computed={computed}"
                ),
            });
        }
        Ok(plan)
    }
}

fn write_position_json(out: &mut String, position: &MultipointPosition) {
    out.push('{');
    out.push_str("\"label\":");
    push_json_str(out, &position.label);
    out.push_str(",\"role\":");
    push_json_str(out, position.role.as_str());
    out.push_str(",\"weight\":");
    push_json_number(out, position.weight);
    out.push_str(",\"coordinates_m\":");
    match position.coordinates_m {
        Some(coordinates) => {
            out.push('[');
            for (index, value) in coordinates.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                push_json_number(out, *value);
            }
            out.push(']');
        }
        None => out.push_str("null"),
    }
    out.push_str(",\"mic_orientation\":");
    push_json_opt_str(out, position.mic_orientation.as_deref());
    out.push('}');
}

fn parse_position_json(
    value: &Json,
    expected_role: PositionRole,
) -> Result<MultipointPosition, PositionsError> {
    let entries = json_object(value, "positions plan position")?;
    let label = json_string_field(entries, "label", "positions plan position")?.to_string();
    let role_text = json_string_field(entries, "role", "positions plan position")?;
    let role = match role_text {
        "design" => PositionRole::Design,
        "validation" => PositionRole::Validation,
        other => {
            return Err(json_error(format!(
                "positions plan position role must be design|validation, got {other:?}"
            )))
        }
    };
    if role != expected_role {
        return Err(json_error(format!(
            "positions plan position {label:?} role {role_text:?} is in the wrong group array"
        )));
    }
    let weight = json_number_field(entries, "weight", "positions plan position")?;
    let coordinates_m = match json_field(entries, "coordinates_m") {
        None | Some(Json::Null) => None,
        Some(Json::Array(values)) => {
            if values.len() != 3 {
                return Err(json_error(
                    "positions plan coordinates_m must be null or a 3-element array",
                ));
            }
            let mut coordinates = [0.0f64; 3];
            for (index, item) in values.iter().enumerate() {
                coordinates[index] = json_as_number(item, "positions plan coordinates_m[]")?;
            }
            Some(coordinates)
        }
        Some(_) => {
            return Err(json_error(
                "positions plan coordinates_m must be null or a 3-element array",
            ))
        }
    };
    let mic_orientation = match json_field(entries, "mic_orientation") {
        None | Some(Json::Null) => None,
        Some(item) => Some(json_as_string(item, "positions plan mic_orientation")?.to_string()),
    };
    let position = MultipointPosition {
        label,
        role,
        weight,
        coordinates_m,
        mic_orientation,
    };
    position.validate()?;
    Ok(position)
}

// ---------------------------------------------------------------------------
// 质量输入（生产者：T62 capture / T48 analysis；本模块只读）
// ---------------------------------------------------------------------------

/// 质量评估 scope：design 拟合组或 validation/holdout 组（不允许互相混用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityScope {
    /// design 组（拟合用；pre-fit QualityCheck 阶段）。
    Design,
    /// validation/holdout 组（不参与调参；remeasure/holdout 阶段）。
    Validation,
}

impl QualityScope {
    /// 机器名（design/validation）。
    pub const fn as_str(self) -> &'static str {
        match self {
            QualityScope::Design => "design",
            QualityScope::Validation => "validation",
        }
    }

    /// 对应角色。
    pub const fn role(self) -> PositionRole {
        match self {
            QualityScope::Design => PositionRole::Design,
            QualityScope::Validation => PositionRole::Validation,
        }
    }

    /// 解析机器名；未知拒绝。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "design" => Some(QualityScope::Design),
            "validation" => Some(QualityScope::Validation),
            _ => None,
        }
    }

    /// 本 scope 的 T6 最低可用点数。
    pub const fn min_usable_points(self) -> usize {
        match self {
            QualityScope::Design => T6_MIN_DESIGN_POINTS,
            QualityScope::Validation => T6_MIN_HOLDOUT_POINTS,
        }
    }
}

impl fmt::Display for QualityScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 重复质量使用的策略（design/off 要求 >=3、on 要求 >=2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatPolicy {
    /// design/off 测量（mimo-t6 §3.4：>=3 次有效重复）。
    DesignOff,
    /// on 测量（mimo-t6 §3.4：>=2 次）。
    On,
}

impl RepeatPolicy {
    /// 机器名（design_off/on）。
    pub const fn as_str(self) -> &'static str {
        match self {
            RepeatPolicy::DesignOff => "design_off",
            RepeatPolicy::On => "on",
        }
    }

    /// 最小有效重复次数。
    pub const fn min_repeats(self) -> u32 {
        match self {
            RepeatPolicy::DesignOff => MIN_DESIGN_REPEATS,
            RepeatPolicy::On => MIN_ON_REPEATS,
        }
    }

    /// 解析机器名；未知拒绝。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "design_off" => Some(RepeatPolicy::DesignOff),
            "on" => Some(RepeatPolicy::On),
            _ => None,
        }
    }
}

impl fmt::Display for RepeatPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 一遍测量的质量度量（SNR/clock/phase 三组均为 Option；缺任一 -> 不继续）。
///
/// 字段语义与生产者（T62/T48/T49）对齐：
/// - snr_db：全带 200 Hz-8 kHz（带限用自身通带）SNR（dB）；
/// - snr_lowband_db：T6 低带 63-250 Hz SNR（dB）；
/// - clock_ppm/clock_ppm_uncertainty_ppm/clock_ppm_ok：ACCEPTANCE §0.2.2 口径
///   （只有 |ppm|>2σ 才是漂移；clock_ppm_ok=false -> 该遍无效）；
/// - clock_drift_corrected：显著漂移时是否已声明校正（true 才允许）；
/// - phase_ref/phase_deg/delay_samples：T49 PhaseBasis 语义下的相位/传播延迟；
/// - residual_phase_10k_deg/residual_phase_span_seconds/residual_phase_sigma_ppm：
///   ACCEPTANCE §0.2.4 的补偿后残余相位证据；
/// - common_d_residual_samples：实测公共延迟相对声明 D0 的残差（mimo-t6 §3.5 +-0.5）；
/// - polarity：+-1；0/None = unknown（不得猜）；
/// - magnitude_db：与 MeasurementQualityInput.magnitude_grid_hz 同长的幅频（重复极差用）；
/// - coherence：可选相干性（若提供则 >=0.9 才记有效点比例）；
/// - onset_invalid：T62 onset 有效性（true -> 该遍无效）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RepeatQualityInput {
    /// 重复序号（0-based；必须 0..n-1 连续）。
    pub repeat: u32,
    /// 全带 SNR（dB）。
    pub snr_db: Option<f64>,
    /// 低带 63-250 Hz SNR（dB）。
    pub snr_lowband_db: Option<f64>,
    /// UMIK 相对 render 的 clock ppm。
    pub clock_ppm: Option<f64>,
    /// clock ppm 1 sigma。
    pub clock_ppm_uncertainty_ppm: Option<f64>,
    /// clock 估计是否有效（false -> 该遍无效）。
    pub clock_ppm_ok: Option<bool>,
    /// 显著漂移是否已声明校正（|ppm|>2σ 时必需 true）。
    pub clock_drift_corrected: Option<bool>,
    /// 相位基准语义串（T49 PhaseBasis）。
    pub phase_ref: Option<String>,
    /// 相位（deg；具体频点由生产者声明）。
    pub phase_deg: Option<f64>,
    /// 实测传播延迟（样本；T49 provenance）。
    pub delay_samples: Option<f64>,
    /// 补偿后 10 kHz 残余相位（deg）。
    pub residual_phase_10k_deg: Option<f64>,
    /// 残余相位对应的数据跨度（秒）。
    pub residual_phase_span_seconds: Option<f64>,
    /// 残余相位对应的 sigma_ppm。
    pub residual_phase_sigma_ppm: Option<f64>,
    /// 实测公共延迟相对声明 D0 的残差（样本）。
    pub common_d_residual_samples: Option<f64>,
    /// 极性（+-1；0=unknown）。
    pub polarity: Option<i32>,
    /// 重复幅频（dB；长度 = magnitude_grid_hz）。
    pub magnitude_db: Option<Vec<f64>>,
    /// magnitude-squared coherence（可选）。
    pub coherence: Option<f64>,
    /// onset 是否无效（T62；true -> 该遍无效）。
    pub onset_invalid: Option<bool>,
}

impl RepeatQualityInput {
    /// 以重复序号构造空度量（测试/生产者逐字段填）。
    pub fn new(repeat: u32) -> Self {
        RepeatQualityInput {
            repeat,
            ..RepeatQualityInput::default()
        }
    }
}

/// 一个 (position, participant) 的质量输入。
#[derive(Debug, Clone, PartialEq)]
pub struct MeasurementQualityInput {
    /// participant（必须在 plan.participants 内）。
    pub speaker: String,
    /// 幅频网格（Hz；严格递增、>0、<=fs/2）。
    pub magnitude_grid_hz: Vec<f64>,
    /// 每遍度量（1..=MAX_REPEATS；序号连续）。
    pub repeats: Vec<RepeatQualityInput>,
}

impl MeasurementQualityInput {
    /// 构造。
    pub fn new(speaker: impl Into<String>, magnitude_grid_hz: Vec<f64>) -> Self {
        MeasurementQualityInput {
            speaker: speaker.into(),
            magnitude_grid_hz,
            repeats: Vec::new(),
        }
    }

    /// 追加一遍度量。
    pub fn push_repeat(&mut self, repeat: RepeatQualityInput) {
        self.repeats.push(repeat);
    }
}

/// 一个测点的质量输入。
#[derive(Debug, Clone, PartialEq)]
pub struct PositionQualityInput {
    /// 测点 label（必须在 plan 内且角色与 scope 一致）。
    pub label: String,
    /// 每 participant 的度量。
    pub measurements: Vec<MeasurementQualityInput>,
}

impl PositionQualityInput {
    /// 构造。
    pub fn new(label: impl Into<String>) -> Self {
        PositionQualityInput {
            label: label.into(),
            measurements: Vec::new(),
        }
    }
}

/// 一次 quality 评估的完整输入。
#[derive(Debug, Clone, PartialEq)]
pub struct MultipointQualityInput {
    /// 评估 scope（design 或 validation；不得混入另一组测点）。
    pub scope: QualityScope,
    /// 重复策略（design/off >=3、on >=2）。
    pub repeat_policy: RepeatPolicy,
    /// 本 scope 的逐测点输入（必须恰好覆盖 plan 该组）。
    pub positions: Vec<PositionQualityInput>,
}

impl MultipointQualityInput {
    /// 构造空输入。
    pub fn new(scope: QualityScope, repeat_policy: RepeatPolicy) -> Self {
        MultipointQualityInput {
            scope,
            repeat_policy,
            positions: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// 质量报告
// ---------------------------------------------------------------------------

/// 一个 measurement 的重复质量结果。
#[derive(Debug, Clone, PartialEq)]
pub struct MeasurementQuality {
    /// participant。
    pub speaker: String,
    /// 提供的重复遍数。
    pub repeats_supplied: u32,
    /// 完全通过门/度量的重复遍数。
    pub repeats_valid: u32,
    /// 是否可用（全部重复有效且重复极差/极性一致）。
    pub usable: bool,
    /// 全带 SNR 最小值（dB）。
    pub snr_db_min: Option<f64>,
    /// 低带 SNR 最小值（dB）。
    pub snr_lowband_db_min: Option<f64>,
    /// clock ppm（首遍记录；逐遍验门）。
    pub clock_ppm: Option<f64>,
    /// clock sigma（首遍记录）。
    pub clock_ppm_uncertainty_ppm: Option<f64>,
    /// 重复延迟极差（样本）。
    pub delay_spread_samples: Option<f64>,
    /// 重复幅频极差（dB；检查频带内）。
    pub magnitude_spread_db: Option<f64>,
    /// 极性（全部重复一致时记录）。
    pub polarity: Option<i32>,
    /// 相干性最小值（若提供）。
    pub coherence_min: Option<f64>,
    /// 是否提供相干性度量。
    pub coherence_available: bool,
    /// 不可用原因码（去重）。
    pub reasons: Vec<String>,
}

impl MeasurementQuality {
    fn new(speaker: String) -> Self {
        MeasurementQuality {
            speaker,
            repeats_supplied: 0,
            repeats_valid: 0,
            usable: false,
            snr_db_min: None,
            snr_lowband_db_min: None,
            clock_ppm: None,
            clock_ppm_uncertainty_ppm: None,
            delay_spread_samples: None,
            magnitude_spread_db: None,
            polarity: None,
            coherence_min: None,
            coherence_available: false,
            reasons: Vec::new(),
        }
    }
}

/// 一个测点的重复质量结果。
#[derive(Debug, Clone, PartialEq)]
pub struct PositionQuality {
    /// 测点 label。
    pub label: String,
    /// 角色。
    pub role: PositionRole,
    /// 权重（与 plan 一致；用于 T52/T53 报告）。
    pub weight: f64,
    /// 是否可用（全部 participant measurement 可用）。
    pub usable: bool,
    /// 不可用原因码（去重）。
    pub reasons: Vec<String>,
    /// 逐 participant 结果（按 plan.participants 顺序）。
    pub measurements: Vec<MeasurementQuality>,
}

/// 一次 scope 的 quality 报告（tspi-calibration-position-quality/v1；job artifact）。
#[derive(Debug, Clone, PartialEq)]
pub struct PositionQualityReport {
    /// schema。
    pub schema: &'static str,
    /// 被评估 plan 的 plan_sha256（stale 判定）。
    pub plan_sha256: String,
    /// scope。
    pub scope: QualityScope,
    /// 重复策略。
    pub repeat_policy: RepeatPolicy,
    /// 最小重复次数（= policy.min_repeats()）。
    pub min_repeats: u32,
    /// plan 的 design 总数（信息）。
    pub plan_design_total: usize,
    /// plan 的 validation 总数（信息）。
    pub plan_validation_total: usize,
    /// 本 scope 评估的测点数。
    pub points_total: usize,
    /// 本 scope 可用测点数。
    pub points_usable: usize,
    /// 是否允许继续（false -> job 必须失败/INCOMPLETE，不得拟合）。
    pub continue_allowed: bool,
    /// 顶层原因码（去重）。
    pub reasons: Vec<String>,
    /// 逐测点结果（按 plan 声明顺序）。
    pub points: Vec<PositionQuality>,
}

impl PositionQualityReport {
    /// 是否允许继续（显式方法便于 T64/T66 消费）。
    pub fn continue_allowed(&self) -> bool {
        self.continue_allowed
    }

    /// 本 scope 要求的 T6 最低可用点数。
    pub fn required_usable_points(&self) -> usize {
        self.scope.min_usable_points()
    }

    fn write_json(&self) -> String {
        let mut out = String::with_capacity(8192);
        out.push('{');
        out.push_str("\"schema\":");
        push_json_str(&mut out, self.schema);
        out.push_str(",\"plan_sha256\":");
        push_json_str(&mut out, &self.plan_sha256);
        out.push_str(",\"scope\":");
        push_json_str(&mut out, self.scope.as_str());
        out.push_str(",\"repeat_policy\":");
        push_json_str(&mut out, self.repeat_policy.as_str());
        let _ = write!(
            out,
            ",\"min_repeats\":{},\"plan_design_total\":{},\"plan_validation_total\":{},\"points_total\":{},\"points_usable\":{},\"continue_allowed\":{}",
            self.min_repeats,
            self.plan_design_total,
            self.plan_validation_total,
            self.points_total,
            self.points_usable,
            self.continue_allowed
        );
        out.push_str(",\"reasons\":[");
        for (index, reason) in self.reasons.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            push_json_str(&mut out, reason);
        }
        out.push_str("],\"points\":[");
        for (index, point) in self.points.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push('{');
            out.push_str("\"label\":");
            push_json_str(&mut out, &point.label);
            out.push_str(",\"role\":");
            push_json_str(&mut out, point.role.as_str());
            out.push_str(",\"weight\":");
            push_json_number(&mut out, point.weight);
            let _ = write!(out, ",\"usable\":{},", point.usable);
            out.push_str("\"reasons\":[");
            for (reason_index, reason) in point.reasons.iter().enumerate() {
                if reason_index > 0 {
                    out.push(',');
                }
                push_json_str(&mut out, reason);
            }
            out.push_str("],\"measurements\":[");
            for (measurement_index, measurement) in point.measurements.iter().enumerate() {
                if measurement_index > 0 {
                    out.push(',');
                }
                out.push('{');
                out.push_str("\"speaker\":");
                push_json_str(&mut out, &measurement.speaker);
                let _ = write!(
                    out,
                    ",\"repeats_supplied\":{},\"repeats_valid\":{},\"usable\":{}",
                    measurement.repeats_supplied, measurement.repeats_valid, measurement.usable
                );
                out.push_str(",\"snr_db_min\":");
                push_json_opt_number(&mut out, measurement.snr_db_min);
                out.push_str(",\"snr_lowband_db_min\":");
                push_json_opt_number(&mut out, measurement.snr_lowband_db_min);
                out.push_str(",\"clock_ppm\":");
                push_json_opt_number(&mut out, measurement.clock_ppm);
                out.push_str(",\"clock_ppm_uncertainty_ppm\":");
                push_json_opt_number(&mut out, measurement.clock_ppm_uncertainty_ppm);
                out.push_str(",\"delay_spread_samples\":");
                push_json_opt_number(&mut out, measurement.delay_spread_samples);
                out.push_str(",\"magnitude_spread_db\":");
                push_json_opt_number(&mut out, measurement.magnitude_spread_db);
                out.push_str(",\"polarity\":");
                match measurement.polarity {
                    Some(polarity) => {
                        let _ = write!(out, "{polarity}");
                    }
                    None => out.push_str("null"),
                }
                out.push_str(",\"coherence_min\":");
                push_json_opt_number(&mut out, measurement.coherence_min);
                let _ = write!(
                    out,
                    ",\"coherence_available\":{},\"reasons\":[",
                    measurement.coherence_available
                );
                for (reason_index, reason) in measurement.reasons.iter().enumerate() {
                    if reason_index > 0 {
                        out.push(',');
                    }
                    push_json_str(&mut out, reason);
                }
                out.push_str("]}");
            }
            out.push_str("]}");
        }
        out.push_str("]}");
        out
    }

    /// 确定性 JSON（job artifact / evidence）。
    pub fn to_json(&self) -> String {
        self.write_json()
    }
}

/// quality artifact 的可恢复摘要（重启后校验用；不重放采集）。
#[derive(Debug, Clone, PartialEq)]
pub struct QualityArtifactSummary {
    /// 被评估 plan 的 hash。
    pub plan_sha256: String,
    /// scope。
    pub scope: QualityScope,
    /// 重复策略。
    pub repeat_policy: RepeatPolicy,
    /// 本 scope 测点数。
    pub points_total: usize,
    /// 本 scope 可用测点数。
    pub points_usable: usize,
    /// plan design 总数。
    pub plan_design_total: usize,
    /// plan validation 总数。
    pub plan_validation_total: usize,
    /// 是否允许继续。
    pub continue_allowed: bool,
    /// 顶层原因码。
    pub reasons: Vec<String>,
    /// 逐测点摘要。
    pub points: Vec<QualityPointSummary>,
}

/// quality artifact 中的逐测点摘要。
#[derive(Debug, Clone, PartialEq)]
pub struct QualityPointSummary {
    /// 测点 label。
    pub label: String,
    /// 角色。
    pub role: PositionRole,
    /// 是否可用。
    pub usable: bool,
}

impl QualityArtifactSummary {
    /// 从 quality artifact JSON 严格解析摘要并做内部一致性校验。
    pub fn from_json(text: &str) -> Result<Self, PositionsError> {
        let root = parse_json(text)?;
        let entries = json_object(&root, "position quality")?;
        let schema = json_string_field(entries, "schema", "position quality")?;
        if schema != POSITION_QUALITY_SCHEMA {
            return Err(PositionsError::ArtifactSchema {
                expected: POSITION_QUALITY_SCHEMA,
                actual: schema.to_string(),
            });
        }
        let plan_sha256 =
            json_string_field(entries, "plan_sha256", "position quality")?.to_string();
        let scope_text = json_string_field(entries, "scope", "position quality")?;
        let scope = QualityScope::parse(scope_text).ok_or_else(|| {
            json_error(format!(
                "position quality scope must be design|validation, got {scope_text:?}"
            ))
        })?;
        let repeat_policy_text = json_string_field(entries, "repeat_policy", "position quality")?;
        let repeat_policy = RepeatPolicy::parse(repeat_policy_text).ok_or_else(|| {
            json_error(format!(
                "position quality repeat_policy must be design_off|on, got {repeat_policy_text:?}"
            ))
        })?;
        let points_total = json_u64_field(entries, "points_total", "position quality")? as usize;
        let points_usable = json_u64_field(entries, "points_usable", "position quality")? as usize;
        let plan_design_total =
            json_u64_field(entries, "plan_design_total", "position quality")? as usize;
        let plan_validation_total =
            json_u64_field(entries, "plan_validation_total", "position quality")? as usize;
        let continue_allowed = json_bool_field(entries, "continue_allowed", "position quality")?;
        let reasons_json = json_as_array(
            json_required(entries, "reasons", "position quality")?,
            "position quality.reasons",
        )?;
        let mut reasons = Vec::with_capacity(reasons_json.len());
        for reason in reasons_json {
            reasons.push(json_as_string(reason, "position quality.reasons[]")?.to_string());
        }
        let points_json = json_as_array(
            json_required(entries, "points", "position quality")?,
            "position quality.points",
        )?;
        let mut points = Vec::with_capacity(points_json.len());
        for point in points_json {
            let point_entries = json_object(point, "position quality point")?;
            let label =
                json_string_field(point_entries, "label", "position quality point")?.to_string();
            let role_text = json_string_field(point_entries, "role", "position quality point")?;
            let role = match role_text {
                "design" => PositionRole::Design,
                "validation" => PositionRole::Validation,
                other => {
                    return Err(json_error(format!(
                        "position quality point role must be design|validation, got {other:?}"
                    )))
                }
            };
            let usable = json_bool_field(point_entries, "usable", "position quality point")?;
            points.push(QualityPointSummary {
                label,
                role,
                usable,
            });
        }
        let summary = QualityArtifactSummary {
            plan_sha256,
            scope,
            repeat_policy,
            points_total,
            points_usable,
            plan_design_total,
            plan_validation_total,
            continue_allowed,
            reasons,
            points,
        };
        summary.check_internal_consistency()?;
        Ok(summary)
    }

    fn check_internal_consistency(&self) -> Result<(), PositionsError> {
        if self.points.len() != self.points_total {
            return Err(PositionsError::ArtifactHash {
                detail: format!(
                    "quality points_total {} != parsed points {}",
                    self.points_total,
                    self.points.len()
                ),
            });
        }
        let usable = self.points.iter().filter(|point| point.usable).count();
        if usable != self.points_usable {
            return Err(PositionsError::ArtifactHash {
                detail: format!(
                    "quality points_usable {} != count of usable points {}",
                    self.points_usable, usable
                ),
            });
        }
        let expected_role = self.scope.role();
        if self.points.iter().any(|point| point.role != expected_role) {
            return Err(PositionsError::ArtifactHash {
                detail: format!(
                    "quality scope {} contains points with the other role (design/validation grouping violated)",
                    self.scope.as_str()
                ),
            });
        }
        let min = self.scope.min_usable_points();
        if self.continue_allowed && self.points_usable < min {
            return Err(PositionsError::ArtifactHash {
                detail: format!(
                    "quality continue_allowed=true but points_usable {} < required {min}",
                    self.points_usable
                ),
            });
        }
        Ok(())
    }

    /// 与当前 plan 比对（plan hash/dimension/scope 覆盖；stale 检测）。
    pub fn verify_against_plan(&self, plan: &MultipointPlan) -> Result<(), PositionsError> {
        let current = plan.plan_sha256();
        if self.plan_sha256 != current {
            return Err(PositionsError::StaleQuality {
                expected_plan_sha256: current,
                actual_plan_sha256: self.plan_sha256.clone(),
            });
        }
        if self.plan_design_total != plan.design_count()
            || self.plan_validation_total != plan.validation_count()
        {
            return Err(PositionsError::ArtifactHash {
                detail: format!(
                    "quality plan counts mismatch: artifact design={} validation={}, plan design={} validation={}",
                    self.plan_design_total,
                    self.plan_validation_total,
                    plan.design_count(),
                    plan.validation_count()
                ),
            });
        }
        let expected: Vec<&str> = plan
            .positions
            .iter()
            .filter(|position| position.role == self.scope.role())
            .map(|position| position.label.as_str())
            .collect();
        if expected.len() != self.points_total {
            return Err(PositionsError::ArtifactHash {
                detail: format!(
                    "quality scope {} covers {} points but plan has {}",
                    self.scope.as_str(),
                    self.points_total,
                    expected.len()
                ),
            });
        }
        for point in &self.points {
            if !expected.contains(&point.label.as_str()) {
                return Err(PositionsError::ArtifactHash {
                    detail: format!(
                        "quality scope {} point {:?} is not in the planned group",
                        self.scope.as_str(),
                        point.label
                    ),
                });
            }
        }
        let _ = &self.repeat_policy;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 质量评估
// ---------------------------------------------------------------------------

fn push_reason(reasons: &mut Vec<String>, code: &str) {
    if !reasons.iter().any(|reason| reason == code) {
        reasons.push(code.to_string());
    }
}

fn required_finite(
    value: Option<f64>,
    point: &str,
    speaker: &str,
    repeat: u32,
    metric: &'static str,
    code: &'static str,
) -> Result<f64, PositionsError> {
    match value {
        None => Err(PositionsError::MissingMetric {
            point: point.to_string(),
            speaker: speaker.to_string(),
            repeat,
            metric,
            code,
        }),
        Some(value) if !value.is_finite() => Err(PositionsError::InvalidMetric {
            point: point.to_string(),
            speaker: speaker.to_string(),
            repeat,
            metric,
            code,
            detail: "value must be finite".to_string(),
        }),
        Some(value) => Ok(value),
    }
}

fn required_polarity(
    value: Option<i32>,
    point: &str,
    speaker: &str,
    repeat: u32,
) -> Result<i32, PositionsError> {
    match value {
        None => Err(PositionsError::MissingMetric {
            point: point.to_string(),
            speaker: speaker.to_string(),
            repeat,
            metric: "polarity",
            code: E_POLARITY_MISSING,
        }),
        Some(0) => Err(PositionsError::MissingMetric {
            point: point.to_string(),
            speaker: speaker.to_string(),
            repeat,
            metric: "polarity",
            code: E_POLARITY_UNKNOWN,
        }),
        Some(value) if value == 1 || value == -1 => Ok(value),
        Some(_) => Err(PositionsError::InvalidMetric {
            point: point.to_string(),
            speaker: speaker.to_string(),
            repeat,
            metric: "polarity",
            code: E_POLARITY_MISSING,
            detail: "polarity must be exactly +1 or -1 (unknown is never guessed)".to_string(),
        }),
    }
}

fn required_phase_ref(
    value: Option<&String>,
    point: &str,
    speaker: &str,
    repeat: u32,
) -> Result<PhaseBasis, PositionsError> {
    let text = match value {
        Some(text) if !text.trim().is_empty() => text,
        _ => {
            return Err(PositionsError::MissingMetric {
                point: point.to_string(),
                speaker: speaker.to_string(),
                repeat,
                metric: "phase_ref",
                code: E_PHASE_MISSING,
            })
        }
    };
    let basis = PhaseBasis::parse(text).map_err(|error| PositionsError::InvalidMetric {
        point: point.to_string(),
        speaker: speaker.to_string(),
        repeat,
        metric: "phase_ref",
        code: E_PHASE_REF_INVALID,
        detail: error.to_string(),
    })?;
    if basis == PhaseBasis::DelayRemoved {
        return Err(PositionsError::InvalidMetric {
            point: point.to_string(),
            speaker: speaker.to_string(),
            repeat,
            metric: "phase_ref",
            code: E_PHASE_REF_INVALID,
            detail: "delay_removed has no machine-readable removed delay in v1 ingest".to_string(),
        });
    }
    Ok(basis)
}

fn validate_magnitude_grid(
    grid: &[f64],
    sample_rate_hz: u32,
    point: &str,
    speaker: &str,
) -> Result<(), PositionsError> {
    if grid.is_empty() {
        return Err(PositionsError::InvalidMetric {
            point: point.to_string(),
            speaker: speaker.to_string(),
            repeat: 0,
            metric: "magnitude_grid_hz",
            code: E_MAGNITUDE_GRID,
            detail: "magnitude grid must not be empty".to_string(),
        });
    }
    let nyquist = sample_rate_hz as f64 / 2.0;
    let mut previous = 0.0f64;
    for (index, frequency) in grid.iter().enumerate() {
        if !frequency.is_finite() || *frequency <= 0.0 || *frequency > nyquist {
            return Err(PositionsError::InvalidMetric {
                point: point.to_string(),
                speaker: speaker.to_string(),
                repeat: 0,
                metric: "magnitude_grid_hz",
                code: E_MAGNITUDE_GRID,
                detail: format!(
                    "grid[{index}]={frequency} must be finite, >0 and <= fs/2={nyquist}"
                ),
            });
        }
        if index > 0 && *frequency <= previous {
            return Err(PositionsError::InvalidMetric {
                point: point.to_string(),
                speaker: speaker.to_string(),
                repeat: 0,
                metric: "magnitude_grid_hz",
                code: E_MAGNITUDE_GRID,
                detail: "magnitude grid frequencies must be strictly increasing".to_string(),
            });
        }
        previous = *frequency;
    }
    Ok(())
}

struct RepeatCheck {
    valid: bool,
    reasons: Vec<&'static str>,
    snr_db: f64,
    snr_lowband_db: f64,
    clock_ppm: f64,
    clock_sigma_ppm: f64,
    delay_samples: f64,
    polarity: i32,
    coherence: Option<f64>,
    magnitude_db: Vec<f64>,
}

fn check_repeat(
    point: &MultipointPosition,
    speaker: &str,
    repeat: &RepeatQualityInput,
    repeats_total: u32,
) -> Result<RepeatCheck, PositionsError> {
    let label = point.label.as_str();
    // ---- SNR（缺 -> 不继续）----
    let snr_db = required_finite(
        repeat.snr_db,
        label,
        speaker,
        repeat.repeat,
        "snr_db",
        E_SNR_MISSING,
    )?;
    let snr_lowband_db = required_finite(
        repeat.snr_lowband_db,
        label,
        speaker,
        repeat.repeat,
        "snr_lowband_db",
        E_SNR_LOWBAND_MISSING,
    )?;
    // ---- clock（缺 -> 不继续）----
    let clock_ppm = required_finite(
        repeat.clock_ppm,
        label,
        speaker,
        repeat.repeat,
        "clock_ppm",
        E_CLOCK_MISSING,
    )?;
    let clock_sigma_ppm = required_finite(
        repeat.clock_ppm_uncertainty_ppm,
        label,
        speaker,
        repeat.repeat,
        "clock_ppm_uncertainty_ppm",
        E_CLOCK_MISSING,
    )?;
    if clock_sigma_ppm <= 0.0 {
        return Err(PositionsError::InvalidMetric {
            point: label.to_string(),
            speaker: speaker.to_string(),
            repeat: repeat.repeat,
            metric: "clock_ppm_uncertainty_ppm",
            code: E_CLOCK_MISSING,
            detail: "clock ppm uncertainty must be > 0".to_string(),
        });
    }
    let clock_ok = match repeat.clock_ppm_ok {
        None => {
            return Err(PositionsError::MissingMetric {
                point: label.to_string(),
                speaker: speaker.to_string(),
                repeat: repeat.repeat,
                metric: "clock_ppm_ok",
                code: E_CLOCK_MISSING,
            })
        }
        Some(clock_ok) => clock_ok,
    };
    // ---- phase（缺 -> 不继续）----
    let _phase_basis =
        required_phase_ref(repeat.phase_ref.as_ref(), label, speaker, repeat.repeat)?;
    let _phase_deg = required_finite(
        repeat.phase_deg,
        label,
        speaker,
        repeat.repeat,
        "phase_deg",
        E_PHASE_MISSING,
    )?;
    let delay_samples = required_finite(
        repeat.delay_samples,
        label,
        speaker,
        repeat.repeat,
        "delay_samples",
        E_DELAY_MISSING,
    )?;
    let residual_phase_deg = required_finite(
        repeat.residual_phase_10k_deg,
        label,
        speaker,
        repeat.repeat,
        "residual_phase_10k_deg",
        E_RESIDUAL_PHASE_MISSING,
    )?;
    let residual_phase_span = required_finite(
        repeat.residual_phase_span_seconds,
        label,
        speaker,
        repeat.repeat,
        "residual_phase_span_seconds",
        E_RESIDUAL_PHASE_MISSING,
    )?;
    let residual_phase_sigma = required_finite(
        repeat.residual_phase_sigma_ppm,
        label,
        speaker,
        repeat.repeat,
        "residual_phase_sigma_ppm",
        E_RESIDUAL_PHASE_MISSING,
    )?;
    let common_d_residual = required_finite(
        repeat.common_d_residual_samples,
        label,
        speaker,
        repeat.repeat,
        "common_d_residual_samples",
        E_COMMON_D_MISSING,
    )?;
    let polarity = required_polarity(repeat.polarity, label, speaker, repeat.repeat)?;
    // ---- 幅频（重复极差用；缺 -> 不继续）----
    let magnitude_db = match &repeat.magnitude_db {
        None => {
            return Err(PositionsError::MissingMetric {
                point: label.to_string(),
                speaker: speaker.to_string(),
                repeat: repeat.repeat,
                metric: "magnitude_db",
                code: E_MAGNITUDE_MISSING,
            })
        }
        Some(values) => {
            if values.iter().any(|value| !value.is_finite()) {
                return Err(PositionsError::InvalidMetric {
                    point: label.to_string(),
                    speaker: speaker.to_string(),
                    repeat: repeat.repeat,
                    metric: "magnitude_db",
                    code: E_MAGNITUDE_GRID,
                    detail: "magnitude_db values must be finite".to_string(),
                });
            }
            values.clone()
        }
    };
    // ---- 可选相干性 ----
    let coherence = match repeat.coherence {
        None => None,
        Some(value) => {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(PositionsError::InvalidMetric {
                    point: label.to_string(),
                    speaker: speaker.to_string(),
                    repeat: repeat.repeat,
                    metric: "coherence",
                    code: E_COHERENCE_INVALID,
                    detail: "coherence must be finite and in [0,1]".to_string(),
                });
            }
            Some(value)
        }
    };

    // ---- 门（有值但超限 -> 该遍不可用；不静默通过）----
    let mut reasons: Vec<&'static str> = Vec::new();
    if !clock_ok {
        reasons.push(Q_CLOCK_INVALID);
    }
    let significant = clock_ppm.abs() > DRIFT_SIGNIFICANCE_SIGMA * clock_sigma_ppm;
    if significant && repeat.clock_drift_corrected != Some(true) {
        reasons.push(Q_CLOCK_DRIFT_UNCORRECTED);
    }
    if snr_db < SNR_MIN_DB {
        reasons.push(Q_SNR_LOW);
    }
    if snr_lowband_db < LOW_BAND_SNR_MIN_DB {
        reasons.push(Q_SNR_LOWBAND_LOW);
    }
    if residual_phase_deg > RESIDUAL_PHASE_10K_MAX_DEG {
        reasons.push(Q_RESIDUAL_PHASE_HIGH);
    }
    let span_ok = (residual_phase_sigma <= RESIDUAL_PHASE_SIGMA_MAX_PPM
        && residual_phase_span >= RESIDUAL_PHASE_DEFAULT_SPAN_SECONDS)
        || residual_phase_span >= RESIDUAL_PHASE_MIN_SPAN_SECONDS
        || repeats_total >= RESIDUAL_PHASE_MIN_REPEATS;
    if !span_ok {
        reasons.push(Q_RESIDUAL_PHASE_WINDOW);
    }
    if common_d_residual.abs() > COMMON_D_RESIDUAL_MAX_SAMPLES {
        reasons.push(Q_COMMON_D_RESIDUAL);
    }
    if let Some(coherence) = coherence {
        if coherence < COHERENCE_MIN {
            reasons.push(Q_COHERENCE_LOW);
        }
    }
    if repeat.onset_invalid == Some(true) {
        reasons.push(Q_ONSET_INVALID);
    }
    Ok(RepeatCheck {
        valid: reasons.is_empty(),
        reasons,
        snr_db,
        snr_lowband_db,
        clock_ppm,
        clock_sigma_ppm,
        delay_samples,
        polarity,
        coherence,
        magnitude_db,
    })
}

fn magnitude_spread(
    grid: &[f64],
    magnitudes: &[Vec<f64>],
    point: &str,
    speaker: &str,
) -> Result<f64, PositionsError> {
    if magnitudes.is_empty() {
        return Ok(0.0);
    }
    let length = magnitudes[0].len();
    for (index, values) in magnitudes.iter().enumerate() {
        if values.len() != length {
            return Err(PositionsError::InvalidMetric {
                point: point.to_string(),
                speaker: speaker.to_string(),
                repeat: index as u32,
                metric: "magnitude_db",
                code: E_MAGNITUDE_GRID,
                detail: "magnitude_db length must equal magnitude_grid_hz length".to_string(),
            });
        }
    }
    // 检查频带 [100, 10k]；若网格不在该频带内则退化为全部网格点。
    let indices: Vec<usize> = (0..length)
        .filter(|index| {
            let frequency = grid[*index];
            frequency >= MAGNITUDE_SPREAD_BAND_HZ.0 && frequency <= MAGNITUDE_SPREAD_BAND_HZ.1
        })
        .collect();
    let checked = if indices.is_empty() {
        (0..length).collect::<Vec<usize>>()
    } else {
        indices
    };
    let mut worst = 0.0f64;
    for index in checked {
        let mut minimum = f64::INFINITY;
        let mut maximum = f64::NEG_INFINITY;
        for values in magnitudes {
            minimum = minimum.min(values[index]);
            maximum = maximum.max(values[index]);
        }
        worst = worst.max(maximum - minimum);
    }
    Ok(worst)
}

fn check_measurement(
    plan: &MultipointPlan,
    point: &MultipointPosition,
    input: &MeasurementQualityInput,
    policy: RepeatPolicy,
) -> Result<MeasurementQuality, PositionsError> {
    let label = point.label.as_str();
    let speaker = input.speaker.as_str();
    let required = policy.min_repeats();
    let supplied = input.repeats.len() as u32;
    if supplied < required {
        return Err(PositionsError::RepeatsInsufficient {
            point: label.to_string(),
            speaker: speaker.to_string(),
            required,
            actual: supplied,
        });
    }
    if supplied > MAX_REPEATS {
        return Err(PositionsError::InvalidConfig {
            field: "measurement.repeats",
            detail: format!(
                "{label}/{speaker} supplies {supplied} repeats but limit is {MAX_REPEATS}"
            ),
        });
    }
    validate_magnitude_grid(
        &input.magnitude_grid_hz,
        plan.sample_rate_hz,
        label,
        speaker,
    )?;
    // 重复序号必须严格 0..n-1（连续、无重复），否则重复质量不可追溯。
    let mut seen: Vec<u32> = Vec::with_capacity(input.repeats.len());
    for repeat in &input.repeats {
        if repeat.repeat >= MAX_REPEATS || seen.contains(&repeat.repeat) {
            return Err(PositionsError::RepeatsDuplicate {
                point: label.to_string(),
                speaker: speaker.to_string(),
                repeat: repeat.repeat,
            });
        }
        seen.push(repeat.repeat);
    }
    seen.sort_unstable();
    if seen
        .iter()
        .enumerate()
        .any(|(index, value)| *value != index as u32)
    {
        return Err(PositionsError::RepeatsDuplicate {
            point: label.to_string(),
            speaker: speaker.to_string(),
            repeat: 0,
        });
    }

    let mut quality = MeasurementQuality::new(speaker.to_string());
    quality.repeats_supplied = supplied;
    let mut checks: Vec<RepeatCheck> = Vec::with_capacity(input.repeats.len());
    for repeat in &input.repeats {
        if let Some(values) = &repeat.magnitude_db {
            if values.len() != input.magnitude_grid_hz.len() {
                return Err(PositionsError::InvalidMetric {
                    point: label.to_string(),
                    speaker: speaker.to_string(),
                    repeat: repeat.repeat,
                    metric: "magnitude_db",
                    code: E_MAGNITUDE_GRID,
                    detail: format!(
                        "magnitude_db length {} != grid length {}",
                        values.len(),
                        input.magnitude_grid_hz.len()
                    ),
                });
            }
        }
        let check = check_repeat(point, speaker, repeat, supplied)?;
        if check.valid {
            quality.repeats_valid += 1;
        } else {
            for reason in &check.reasons {
                push_reason(&mut quality.reasons, reason);
            }
        }
        checks.push(check);
    }
    // 重复极差/极性一致性（有值但超限 -> 不可用）。
    let delays: Vec<f64> = checks.iter().map(|check| check.delay_samples).collect();
    let delay_spread = delays
        .iter()
        .fold(f64::NEG_INFINITY, |acc, value| acc.max(*value))
        - delays
            .iter()
            .fold(f64::INFINITY, |acc, value| acc.min(*value));
    if delay_spread > DELAY_SPREAD_MAX_SAMPLES {
        push_reason(&mut quality.reasons, Q_DELAY_SPREAD);
    }
    let magnitudes: Vec<Vec<f64>> = checks
        .iter()
        .map(|check| check.magnitude_db.clone())
        .collect();
    let spread = magnitude_spread(&input.magnitude_grid_hz, &magnitudes, label, speaker)?;
    if spread > MAGNITUDE_SPREAD_MAX_DB {
        push_reason(&mut quality.reasons, Q_MAGNITUDE_SPREAD);
    }
    let first_polarity = checks.first().map(|check| check.polarity);
    let polarity_consistent = checks
        .iter()
        .all(|check| Some(check.polarity) == first_polarity);
    if !polarity_consistent {
        push_reason(&mut quality.reasons, Q_POLARITY_INCONSISTENT);
    }
    let snr_min = checks
        .iter()
        .map(|check| check.snr_db)
        .fold(f64::INFINITY, f64::min);
    let snr_lowband_min = checks
        .iter()
        .map(|check| check.snr_lowband_db)
        .fold(f64::INFINITY, f64::min);
    let coherence_values: Vec<f64> = checks.iter().filter_map(|check| check.coherence).collect();
    quality.coherence_available = checks.iter().all(|check| check.coherence.is_some());
    quality.coherence_min =
        coherence_values
            .iter()
            .fold(None, |acc: Option<f64>, value| match acc {
                Some(current) => Some(current.min(*value)),
                None => Some(*value),
            });
    quality.snr_db_min = Some(snr_min);
    quality.snr_lowband_db_min = Some(snr_lowband_min);
    if let Some(first) = checks.first() {
        quality.clock_ppm = Some(first.clock_ppm);
        quality.clock_ppm_uncertainty_ppm = Some(first.clock_sigma_ppm);
    }
    quality.delay_spread_samples = Some(delay_spread);
    quality.magnitude_spread_db = Some(spread);
    quality.polarity = if polarity_consistent {
        first_polarity
    } else {
        None
    };
    quality.usable = quality.reasons.is_empty();
    Ok(quality)
}

/// 评估一个 scope 的重复质量（缺 SNR/clock/phase -> 错误；超门 -> 不可用/不继续）。
///
/// 设计/验证分组硬约束：
/// - scope=Design 只接受 plan 的 design 测点，validation 输入 -> E_POS_GROUP_LEAK；
/// - scope=Validation 只接受 validation 测点，design 输入 -> E_POS_GROUP_LEAK；
/// - 输入必须恰好覆盖该 scope 全部测点且每测点覆盖全部 participants（缺 -> 错误）。
pub fn evaluate_quality(
    plan: &MultipointPlan,
    input: &MultipointQualityInput,
) -> Result<PositionQualityReport, PositionsError> {
    plan.validate()?;
    let scope = input.scope;
    let expected: Vec<&MultipointPosition> = plan
        .positions
        .iter()
        .filter(|position| position.role == scope.role())
        .collect();
    if expected.is_empty() {
        return Err(PositionsError::QualityScopeEmpty {
            scope: scope.as_str(),
        });
    }
    // 1) 输入测点集合校验（未知/重复/分组越界/缺失）。
    let mut seen: Vec<&str> = Vec::with_capacity(input.positions.len());
    for position in &input.positions {
        let planned =
            plan.position(&position.label)
                .ok_or_else(|| PositionsError::PositionUnknown {
                    label: position.label.clone(),
                })?;
        if planned.role != scope.role() {
            return Err(PositionsError::GroupLeak {
                label: position.label.clone(),
                role: planned.role.as_str(),
            });
        }
        if seen.contains(&position.label.as_str()) {
            return Err(PositionsError::PositionDuplicate {
                label: position.label.clone(),
            });
        }
        seen.push(position.label.as_str());
    }
    for planned in &expected {
        if !seen.contains(&planned.label.as_str()) {
            return Err(PositionsError::PositionMissing {
                label: planned.label.clone(),
            });
        }
    }
    // 2) 逐测点评估（按 plan 声明顺序）。
    let mut points: Vec<PositionQuality> = Vec::with_capacity(expected.len());
    let mut report_reasons: Vec<String> = Vec::new();
    for planned in &expected {
        let position_input = input
            .positions
            .iter()
            .find(|position| position.label == planned.label)
            .expect("input covered above");
        // participants 覆盖校验（未知/重复/缺失）。
        let mut seen_speakers: Vec<&str> = Vec::with_capacity(position_input.measurements.len());
        for measurement in &position_input.measurements {
            if !plan
                .participants
                .iter()
                .any(|participant| participant == &measurement.speaker)
            {
                return Err(PositionsError::ParticipantUnknown {
                    speaker: measurement.speaker.clone(),
                });
            }
            if seen_speakers.contains(&measurement.speaker.as_str()) {
                return Err(PositionsError::ParticipantDuplicate {
                    speaker: measurement.speaker.clone(),
                });
            }
            seen_speakers.push(measurement.speaker.as_str());
        }
        let mut quality = PositionQuality {
            label: planned.label.clone(),
            role: planned.role,
            weight: planned.weight,
            usable: true,
            reasons: Vec::new(),
            measurements: Vec::with_capacity(plan.participants.len()),
        };
        for participant in &plan.participants {
            let measurement = position_input
                .measurements
                .iter()
                .find(|measurement| &measurement.speaker == participant)
                .ok_or_else(|| PositionsError::ParticipantMissing {
                    speaker: participant.clone(),
                })?;
            let result = check_measurement(plan, planned, measurement, input.repeat_policy)?;
            if !result.usable {
                quality.usable = false;
                for reason in &result.reasons {
                    push_reason(&mut quality.reasons, reason);
                }
            }
            quality.measurements.push(result);
        }
        if !quality.usable {
            let reasons: Vec<String> = quality.reasons.clone();
            for reason in &reasons {
                push_reason(&mut report_reasons, reason);
            }
        }
        points.push(quality);
    }
    let points_usable = points.iter().filter(|point| point.usable).count();
    let points_total = points.len();
    let required = scope.min_usable_points();
    let continue_allowed = points_total >= required && points_usable >= required;
    if points_usable < required {
        push_reason(
            &mut report_reasons,
            match scope {
                QualityScope::Design => Q_DESIGN_POINTS_USABLE,
                QualityScope::Validation => Q_VALIDATION_POINTS_USABLE,
            },
        );
    }
    Ok(PositionQualityReport {
        schema: POSITION_QUALITY_SCHEMA,
        plan_sha256: plan.plan_sha256(),
        scope,
        repeat_policy: input.repeat_policy,
        min_repeats: input.repeat_policy.min_repeats(),
        plan_design_total: plan.design_count(),
        plan_validation_total: plan.validation_count(),
        points_total,
        points_usable,
        continue_allowed,
        reasons: report_reasons,
        points,
    })
}

/// quality 报告映射为 job stage 失败（continue_allowed=false -> 不得进入拟合）。
pub fn quality_stage_failure(report: &PositionQualityReport) -> Option<StageFailure> {
    if report.continue_allowed() {
        return None;
    }
    Some(StageFailure::failed(
        Q_QUALITY_INCOMPLETE,
        format!(
            "position quality gate blocked continuation for scope {} ({}/{} usable, required {}): {}",
            report.scope.as_str(),
            report.points_usable,
            report.points_total,
            report.required_usable_points(),
            report.reasons.join(",")
        ),
    ))
}

// ---------------------------------------------------------------------------
// job artifact / 重启恢复
// ---------------------------------------------------------------------------

fn stage_failure(error: PositionsError) -> StageFailure {
    StageFailure::failed(error.code(), error.to_string())
}

/// plan 的 job artifact 三元组（相对路径、字节、版本）。
pub fn plan_stage_artifact(
    plan: &MultipointPlan,
) -> Result<(String, Vec<u8>, String), PositionsError> {
    plan.validate()?;
    Ok((
        PLAN_ARTIFACT_PATH.to_string(),
        plan.to_json().into_bytes(),
        plan.plan_sha256(),
    ))
}

/// quality 报告的 job artifact 三元组（路径按 scope 固定；内容确定性）。
pub fn quality_stage_artifact(
    report: &PositionQualityReport,
) -> Result<(String, Vec<u8>, String), PositionsError> {
    let path = match report.scope {
        QualityScope::Design => QUALITY_DESIGN_ARTIFACT_PATH,
        QualityScope::Validation => QUALITY_VALIDATION_ARTIFACT_PATH,
    };
    Ok((
        path.to_string(),
        report.to_json().into_bytes(),
        format!("{}-{}", report.scope.as_str(), report.plan_sha256),
    ))
}

/// 通过 T60 StageCtx 登记 plan + quality artifact（job 可恢复/幂等）。
///
/// 约束：同一 job 内重跑 stage 时同路径同内容 -> job.rs 返回已登记记录（幂等）；
/// plan 变化导致内容不同 -> job.rs E_ARTIFACT_IMMUTABLE 拒收（stale 保护）。
pub fn write_positions_stage_artifacts(
    ctx: &StageCtx<'_>,
    plan: &MultipointPlan,
    reports: &[PositionQualityReport],
) -> Result<Vec<ArtifactRecord>, StageFailure> {
    let plan_sha256 = plan.plan_sha256();
    let (path, bytes, version) = plan_stage_artifact(plan).map_err(stage_failure)?;
    let mut records = vec![ctx.write_artifact(
        ArtifactKind::Other("positions_plan".to_string()),
        &path,
        &bytes,
        &version,
    )?];
    let mut scopes: Vec<QualityScope> = Vec::with_capacity(reports.len());
    for report in reports {
        if report.plan_sha256 != plan_sha256 {
            return Err(stage_failure(PositionsError::StaleQuality {
                expected_plan_sha256: plan_sha256.clone(),
                actual_plan_sha256: report.plan_sha256.clone(),
            }));
        }
        if scopes.contains(&report.scope) {
            return Err(stage_failure(PositionsError::InvalidConfig {
                field: "quality_reports",
                detail: format!(
                    "duplicate quality report for scope {}",
                    report.scope.as_str()
                ),
            }));
        }
        scopes.push(report.scope);
        let (path, bytes, version) = quality_stage_artifact(report).map_err(stage_failure)?;
        records.push(ctx.write_artifact(ArtifactKind::QualityReport, &path, &bytes, &version)?);
    }
    Ok(records)
}

/// 重启后可恢复的 positions 状态（plan + 已产出的 quality 摘要）。
#[derive(Debug, Clone, PartialEq)]
pub struct PositionsResumeState {
    /// 从 plan artifact 重建的 plan（自 hash 已校验）。
    pub plan: MultipointPlan,
    /// 已产出的 quality artifact 摘要（按输入顺序；scope 唯一）。
    pub quality: Vec<QualityArtifactSummary>,
}

impl PositionsResumeState {
    /// 按 scope 取 quality 摘要。
    pub fn quality_for(&self, scope: QualityScope) -> Option<&QualityArtifactSummary> {
        self.quality.iter().find(|summary| summary.scope == scope)
    }

    /// design + validation 两个 scope 都已产出且都允许继续。
    pub fn all_scopes_passed(&self) -> bool {
        [QualityScope::Design, QualityScope::Validation]
            .iter()
            .all(|scope| {
                self.quality_for(*scope)
                    .map(|summary| summary.continue_allowed)
                    .unwrap_or(false)
            })
    }
}

/// 从 artifact 目录字节重建 positions 状态（job 重启恢复；不重放采集）。
///
/// - plan_bytes 必须是 MultipointPlan::to_json 产物（自 hash 校验）；
/// - 每个 quality artifact 必须与 plan hash 一致、schema 正确、内部计数自洽；
/// - 同一 scope 不允许重复 artifact（否则无法判定哪一个有效）。
pub fn load_resume_state(
    plan_bytes: &[u8],
    quality_artifacts: &[&[u8]],
) -> Result<PositionsResumeState, PositionsError> {
    let plan_text =
        std::str::from_utf8(plan_bytes).map_err(|error| PositionsError::ArtifactHash {
            detail: format!("plan artifact is not valid UTF-8: {error}"),
        })?;
    let plan = MultipointPlan::from_json(plan_text)?;
    let mut quality: Vec<QualityArtifactSummary> = Vec::with_capacity(quality_artifacts.len());
    for bytes in quality_artifacts {
        let text = std::str::from_utf8(bytes).map_err(|error| PositionsError::ArtifactHash {
            detail: format!("quality artifact is not valid UTF-8: {error}"),
        })?;
        let summary = QualityArtifactSummary::from_json(text)?;
        summary.verify_against_plan(&plan)?;
        if quality
            .iter()
            .any(|existing| existing.scope == summary.scope)
        {
            return Err(PositionsError::InvalidConfig {
                field: "quality_artifacts",
                detail: format!(
                    "duplicate quality artifact for scope {}",
                    summary.scope.as_str()
                ),
            });
        }
        quality.push(summary);
    }
    Ok(PositionsResumeState { plan, quality })
}

// ---------------------------------------------------------------------------
// 测试注册（leaf 私有测试自注册；integrator 不要再写 public_registration.rs）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod t63_tests {
    include!("../tests/T63/positions_contract.rs");
}
