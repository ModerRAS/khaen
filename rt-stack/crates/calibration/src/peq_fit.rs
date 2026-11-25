//! [NONRT] PEQ fit 约 10 段 + flat/tilt10 目标 + 目标 FIR + trim（WBS T64）。
//!
//! 本模块是新 daemon 校准链的 P_post 基线单 owner：
//! - 每输出约 10 段模态 PEQ（只切、允许 IIR），带宽/模态带由调用者给；
//! - 宽带 PEQ/shelf 对齐 flat/tilt10 目标（D7 允许 +3 dB boost，producer 硬帽）；
//! - 输出目标 FIR（线性相位，D 上报，按 4096 常规预算；**唯一延迟定义**：固有群延迟
//!   GD=(taps-1)/2 只由系数承载，下游 T66 plan role.delay_samples 不得再叠加，见
//!   `design_target_fir` 与 compile_apply::linear_phase_group_delay_samples）；
//! - 目标只接受内置 flat/tilt10（用户裁决 F-15：自定义 sha256 一律 E_FIT_TARGET）；
//! - 每通道 trim 与 D7 削波/headroom 报告（预测峰值 <= -6 dBFS、headroom >= 6 dB）；
//! - 发布 khaen-modal-peq/v1 系数、目标 FIR 系数与 khaen-peq-fit/v1 报告；
//! - 模态残差（PEQ 后 / P 后）与给 T50/G 的残余曲线同在报告里，G 不得重复切同一 tone。
//!
//! 依赖（只读上游实现，不改其文件）：T41 modal_peq（RBJ/检测/原图比对）、T11 targets
//! （flat/tilt10/target 读取）、T63 positions（多点 plan 与质量门）、T49 timebase（phase basis）、
//! T60 job（StageCtx artifact 发布）。
//!
//! 写域：本文件 + crates/calibration/tests/T64/ + rt-stack/evidence/T64/。未改公共 lib.rs/mod.rs/CLI。

use std::fmt;
use std::path::{Path, PathBuf};

use khaen_designer::modal_peq::{
    compare_design_to_original, detect_modal_modes, freq_upper_hz, interpolate_curve_db,
    log_frequency_grid, octave_distance, validate_curve, validate_sample_rate, BandType, Biquad,
    ChannelPeq, CurvePoint, DesignMode, DetectedMode, ModalBand, ModalDetectConfig, PeqDesign,
    PeqError, PeqSource, DEFAULT_MODAL_BAND_HZ, MAX_MODAL_PEAKINGS,
};
use khaen_designer::targets::{
    builtin_target, BuiltinOptions, BuiltinTarget, NonModalTargetSource, TargetCurve, TargetError,
};
use khaen_modal_design::timebase::{CommonD0, PhaseBasis};

use crate::job::{sha256_hex, ArtifactKind, ArtifactRecord, StageCtx, StageFailure};
use crate::microfft::{next_pow2_at_least, rfft};
use crate::positions::{MultipointPlan, MultipointPosition, PositionQualityReport, QualityScope};

// ---------------------------------------------------------------------------
// 常量（契约 / D7 / 工程默认）
// ---------------------------------------------------------------------------

/// 报告 schema。
pub const PEQ_FIT_SCHEMA: &str = "khaen-peq-fit/v1";
/// 目标 FIR 系数产物 schema。
pub const TARGET_FIR_SCHEMA: &str = "khaen-target-fir/v1";
/// 报告 artifact 相对路径（job artifacts_dir 下）。
pub const FIT_REPORT_ARTIFACT_PATH: &str = "fit/design-report.json";
/// khaen-modal-peq/v1 系数 artifact 相对路径。
pub const FIT_MODAL_PEQ_ARTIFACT_PATH: &str = "fit/modal-peq.json";
/// 目标 FIR 系数 artifact 相对路径。
pub const FIT_TARGET_FIR_ARTIFACT_PATH: &str = "fit/target-fir.json";

/// 工具名（写进 khaen-modal-peq/v1 tool.name）。
pub const FIT_TOOL_NAME: &str = "khaen-peq-fit";
/// 工具版本。
pub const FIT_TOOL_VERSION: &str = "t64-1";

/// 参考归一化频率（T3/需求 1 AC1：FR 与 target 均 1 kHz 归一）。
pub const REFERENCE_FREQ_HZ: f64 = 1_000.0;
/// 契约频率下界（CONTRACT/spec 5.2）。
pub const FIT_FREQ_MIN_HZ: f64 = 20.0;
/// 契约频率上界（20 kHz，实际再与 fs/2-1 取小）。
pub const FIT_FREQ_MAX_HZ: f64 = 20_000.0;
/// 拟合网格分辨率（1/48 oct，spec 5.4/5.6）。
pub const FIT_GRID_OCTAVE: f64 = 1.0 / 48.0;
/// 质量/残差报告网格（1/24 oct，多测点验收口径）。
pub const REPORT_GRID_OCTAVE: f64 = 1.0 / 24.0;

/// 宽带候选 Q 集合（requirements 6 AC2：0.3-2.0）。
pub const WIDE_Q_CANDIDATES: [f64; 6] = [0.3, 0.5, 0.7, 1.0, 1.4, 2.0];
/// 宽带候选频率网格分辨率（requirements 6 AC2：不粗于 1/12 oct）。
pub const WIDE_FREQ_OCTAVE: f64 = 1.0 / 12.0;
/// 宽带 smoothing（requirements 6 AC1：1/6 oct）。
pub const WIDE_SMOOTH_OCTAVE: f64 = 1.0 / 6.0;
/// 宽带残差 RMS 停止门（requirements 6 AC3：0.5 dB）。
pub const WIDE_STOP_RMS_DB: f64 = 0.5;
/// 宽带单段增益绝对值上限（requirements 6 AC4：6 dB）。
pub const WIDE_SINGLE_GAIN_LIMIT_DB: f64 = 6.0;
/// 宽带单步最小可行增益（requirements 6 AC3：0.2 dB）。
pub const WIDE_MIN_STEP_DB: f64 = 0.2;
/// 宽带候选与已采纳模态段的最小间隔（requirements 6 AC5：1/6 oct）。
pub const WIDE_MODAL_EXCLUSION_OCTAVE: f64 = 1.0 / 6.0;
/// 宽带候选彼此最小间隔（1/24 oct，避免叠段）。
pub const WIDE_SELF_EXCLUSION_OCTAVE: f64 = 1.0 / 24.0;
/// shelf 生产端 slope S（CONTRACT 3.4：producer fit 默认 S=0.7）。
pub const SHELF_SLOPE_S: f64 = 0.7;

/// 单段 boost 硬帽（D7 producer：+3.0 dB）。
pub const D7_SINGLE_BOOST_CAP_DB: f64 = 3.0;
/// 每通道正向总和 warning（CONTRACT 5 规则 9 / D7：>6 dB）。
pub const D7_POSITIVE_SUM_WARN_DB: f64 = 6.0;
/// 每通道正向总和 reject（D7：>12 dB）。
pub const D7_POSITIVE_SUM_REJECT_DB: f64 = 12.0;
/// 预测峰值上限（D7：<= -6 dBFS）。
pub const D7_PREDICTED_PEAK_LIMIT_DBFS: f64 = -6.0;
/// 最小 headroom（D7：>= 6 dB）。
pub const D7_MIN_HEADROOM_DB: f64 = 6.0;
/// D7 默认 boost cap（opt-in；缺省只切）。
pub const DEFAULT_BOOST_CAP_DB: f64 = 0.0;
/// boost cap 上限（D7：+3 dB）。
pub const MAX_BOOST_CAP_DB: f64 = 3.0;

/// trim 下限（CONTRACT 3.3：-12 dB）。
pub const TRIM_MIN_DB: f64 = -12.0;
/// trim 上限（CONTRACT 3.3：+6 dB；拟合只使用 <= 0 的安全 trim）。
pub const TRIM_MAX_DB: f64 = 6.0;

/// 模态/宽带总 cut 预算（工程默认，防止 10 段 x -12 dB 过修；报告为内部预算）。
/// 约 10 段 x 7 dB 的余量：真实房间低带常同时存在 8-10 个模态，预算过小会把\n/// 检出截断成 3 段；单段仍受 max_cut 12 dB 与 2a/2b 原图比对约束。
pub const DEFAULT_CUT_BUDGET_DB: f64 = 72.0;
/// 宽带段数上限（requirements 6 AC3：12）。
pub const DEFAULT_WIDE_MAX_STAGES: usize = 12;
/// 合并后每通道段数上限（requirements 2 AC5：22；CONTRACT 硬上限 32）。
pub const DEFAULT_MAX_TOTAL_STAGES: usize = 22;
/// 目标 FIR taps 上限（spec 7.2：常规 per-route <=4096）。
pub const MAX_TARGET_FIR_TAPS: usize = 4_096;
/// 目标 FIR taps 下限。
pub const MIN_TARGET_FIR_TAPS: usize = 8;
/// 目标 FIR 缺省 taps（线性相位 D=(N-1)/2 约 5.3 ms @48k）。
pub const DEFAULT_TARGET_FIR_TAPS: usize = 512;
/// 目标 FIR 缺省频带下界（模态带以上；模态带由 PEQ + G 负责，避免与 G 双切）。
pub const DEFAULT_TARGET_FIR_BAND_HZ: [f64; 2] = [300.0, 20_000.0];
/// 目标 FIR 频带边缘 taper（1/3 oct 内淡出到 0 dB，抑边缘纹波）。
pub const TARGET_FIR_EDGE_TAPER_OCTAVE: f64 = 1.0 / 3.0;
/// 目标 FIR 单点正增益硬上限（D7：+3 dB）。
pub const TARGET_FIR_MAX_POSITIVE_DB: f64 = 3.0;
/// 目标 FIR 负增益下限（避免设计出的极端 cut）。
pub const TARGET_FIR_NEGATIVE_LIMIT_DB: f64 = -40.0;
/// 目标 FIR 频带内允许的报告偏差（> 该值只 warning，不 FAIL）。
pub const TARGET_FIR_DEVIATION_WARN_DB: f64 = 1.0;

/// 单段模态 cut 上限（spec 5.2：单段 gain >= -24，但模态 cut 生产端按 12 dB 上限收敛）。
pub const T41_MODAL_MAX_CUT_DB: f64 = 12.0;
/// 2a 逐段回退的最大次数。
pub const MAX_RULE_2A_ADJUSTMENTS: usize = 16;
/// 2a 回退单步（dB）。
pub const RULE_2A_BACKOFF_STEP_DB: f64 = 1.0;
/// 2a 生效阈值（spec 5.6.2a：峰值高出目标 > +3 dB 或变差 > 3 dB）。
pub const RULE_2A_LIMIT_DB: f64 = 3.0;

/// 目标驱动 2a 恢复（target-aware recovery）证据 schema。
///
/// 设计原则：**不放宽任何门限**。标准拟合 rule_2a FAIL 时，仅从当前残差峰推导
/// 一个额外 peaking 段，复用既有 2a 修复循环，并且只有在恢复后 rule_2a 两 clause
/// 真正通过时才采纳；否则保留原 FAIL 并在证据里写 unresolved 原因。
pub const RECOVERY_SCHEMA: &str = "khaen-target-aware-recovery/v1";
/// 恢复证据 artifact（仅触发时生成；否则不产生新 artifact）。
pub const RECOVERY_ARTIFACT_PATH: &str = "fit/recovery.json";
/// 候选频率偏移（相对最大正残差峰，octave；固定顺序保证确定性）。
const RECOVERY_FREQ_OFFSETS_OCT: [f64; 5] = [0.0, -1.0 / 48.0, 1.0 / 48.0, -1.0 / 24.0, 1.0 / 24.0];
/// 候选 Q（固定顺序；<=10 保持在 T41 peaking Q 边界内）。
const RECOVERY_Q_ORDER: [f64; 7] = [4.0, 3.0, 5.0, 6.0, 2.0, 8.0, 10.0];
/// 候选初始 cut（dB）；随后由既有 2a 修复循环在 [−12,0] 内继续调整。
const RECOVERY_GAIN_STEPS_DB: [f64; 3] = [-0.5, -1.5, -3.0];
/// 固定候选预算（5*7*3=105；超预算立即停止，保证确定性与可解释耗时）。
pub const RECOVERY_MAX_CANDIDATES: usize = 105;

/// 多测点同频差异 > 6 dB 时在模态检测中降权（requirements 3 AC4）。
pub const MODAL_SPREAD_DOWNWEIGHT_DB: f64 = 6.0;
/// 单测点模态突出度最低门（requirements 5 AC2：2 dB）。
pub const POSITION_MIN_PROMINENCE_DB: f64 = 2.0;

/// 错误码。
pub const E_FIT_OPTIONS: &str = "E_FIT_OPTIONS";
pub const E_FIT_FS: &str = "E_FIT_FS";
pub const E_FIT_PLAN: &str = "E_FIT_PLAN";
pub const E_FIT_TARGET: &str = "E_FIT_TARGET";
pub const E_FIT_TARGET_PHASE: &str = "E_FIT_TARGET_PHASE";
pub const E_FIT_CURVE: &str = "E_FIT_CURVE";
pub const E_FIT_CHANNEL: &str = "E_FIT_CHANNEL";
pub const E_FIT_DUPLICATE: &str = "E_FIT_DUPLICATE";
pub const E_FIT_UNKNOWN_POSITION: &str = "E_FIT_UNKNOWN_POSITION";
pub const E_FIT_MISSING_POSITION: &str = "E_FIT_MISSING_POSITION";
pub const E_FIT_BAND: &str = "E_FIT_BAND";
pub const E_FIT_QUALITY_MISSING: &str = "E_FIT_QUALITY_MISSING";
pub const E_FIT_QUALITY_BLOCKED: &str = "E_FIT_QUALITY_BLOCKED";
pub const E_FIT_QUALITY_STALE: &str = "E_FIT_QUALITY_STALE";
pub const E_FIT_DESIGN: &str = "E_FIT_DESIGN";
pub const E_FIT_NUMERIC: &str = "E_FIT_NUMERIC";
pub const E_FIT_HEADROOM: &str = "E_FIT_HEADROOM";

// ---------------------------------------------------------------------------
// 错误类型
// ---------------------------------------------------------------------------

/// PEQ fit 错误：稳定错误码 + 可读消息。
#[derive(Debug, Clone, PartialEq)]
pub struct PeqFitError {
    pub code: &'static str,
    pub message: String,
}

impl PeqFitError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for PeqFitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for PeqFitError {}

fn fit_err<T>(code: &'static str, message: impl Into<String>) -> Result<T, PeqFitError> {
    Err(PeqFitError::new(code, message))
}

fn peq_error(error: PeqError) -> PeqFitError {
    PeqFitError::new(E_FIT_DESIGN, error.to_string())
}

fn target_error(error: TargetError) -> PeqFitError {
    PeqFitError::new(E_FIT_TARGET, error.to_string())
}

// ---------------------------------------------------------------------------
// 输入：测点曲线 / 通道频带 / 目标 FIR 选项 / 选项 / 请求
// ---------------------------------------------------------------------------

/// 单通道名匹配 ^[A-Z][A-Z0-9]*$（CONTRACT 5 规则 3）。
fn is_channel_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    match bytes.next() {
        Some(first) if first.is_ascii_uppercase() => {}
        _ => return false,
    }
    bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

/// 单个（测点 label，通道）的实测幅度曲线（Hz/dB，已是相对形状）。
#[derive(Clone, Debug, PartialEq)]
pub struct PositionCurve {
    pub position: String,
    pub channel: String,
    pub points: Vec<CurvePoint>,
}

impl PositionCurve {
    pub fn new(
        position: impl Into<String>,
        channel: impl Into<String>,
        points: Vec<CurvePoint>,
    ) -> Self {
        Self {
            position: position.into(),
            channel: channel.into(),
            points,
        }
    }

    fn validate(&self) -> Result<(), PeqFitError> {
        if self.position.trim().is_empty() {
            return fit_err(E_FIT_CURVE, "测点 label 为空");
        }
        if !is_channel_name(&self.channel) {
            return fit_err(
                E_FIT_CHANNEL,
                format!("通道名 {:?} 不匹配 ^[A-Z][A-Z0-9]*$", self.channel),
            );
        }
        validate_curve(&self.points).map_err(|error| {
            PeqFitError::new(
                E_FIT_CURVE,
                format!(
                    "测点 {} 通道 {} 曲线非法：{error}",
                    self.position, self.channel
                ),
            )
        })
    }
}

/// 单通道工作频带（HP/LP 来自 caller/upmix 预设；缺省全带）。
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelBand {
    pub channel: String,
    pub band_hz: [f64; 2],
}

impl ChannelBand {
    pub fn new(channel: impl Into<String>, band_hz: [f64; 2]) -> Self {
        Self {
            channel: channel.into(),
            band_hz,
        }
    }
}

/// 目标 FIR 设计选项。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TargetFirOptions {
    pub enabled: bool,
    pub taps: usize,
    pub band_hz: [f64; 2],
    pub allow_positive_db: f64,
}

impl Default for TargetFirOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            taps: DEFAULT_TARGET_FIR_TAPS,
            band_hz: DEFAULT_TARGET_FIR_BAND_HZ,
            allow_positive_db: 0.0,
        }
    }
}

/// PEQ fit 选项（全部显式、可复现地写进报告）。
#[derive(Clone, Debug, PartialEq)]
pub struct PeqFitOptions {
    pub modal_band_hz: [f64; 2],
    pub wide_band_hz: [f64; 2],
    pub reference_freq_hz: f64,
    pub boost_cap_db: f64,
    pub cut_budget_db: f64,
    pub wide_max_stages: usize,
    pub max_total_stages: usize,
    pub low_band_trusted: bool,
    pub program_peak_reference_dbfs: f64,
    pub trim_range_db: [f64; 2],
    pub target_fir: TargetFirOptions,
    pub require_design_quality: bool,
    pub require_t6_minimums: bool,
}

impl Default for PeqFitOptions {
    fn default() -> Self {
        Self {
            modal_band_hz: DEFAULT_MODAL_BAND_HZ,
            wide_band_hz: [80.0, 16_000.0],
            reference_freq_hz: REFERENCE_FREQ_HZ,
            boost_cap_db: DEFAULT_BOOST_CAP_DB,
            cut_budget_db: DEFAULT_CUT_BUDGET_DB,
            wide_max_stages: DEFAULT_WIDE_MAX_STAGES,
            max_total_stages: DEFAULT_MAX_TOTAL_STAGES,
            low_band_trusted: true,
            program_peak_reference_dbfs: -18.0,
            trim_range_db: [TRIM_MIN_DB, TRIM_MAX_DB],
            target_fir: TargetFirOptions::default(),
            require_design_quality: true,
            require_t6_minimums: false,
        }
    }
}

impl PeqFitOptions {
    /// 校验并归一化（越界明确拒绝；cap 越界按 D7 执行值收敛并返回 warning）。
    pub fn validate(&self, fs: u32) -> Result<Vec<String>, PeqFitError> {
        let mut warnings = Vec::new();
        validate_sample_rate(fs).map_err(|error| PeqFitError::new(E_FIT_FS, error.to_string()))?;
        let upper = freq_upper_hz(fs as f64);
        for (name, band) in [
            ("modal_band_hz", self.modal_band_hz),
            ("wide_band_hz", self.wide_band_hz),
        ] {
            if !band[0].is_finite()
                || !band[1].is_finite()
                || band[0] <= 0.0
                || !(band[1] > band[0])
            {
                return fit_err(E_FIT_OPTIONS, format!("{name}={band:?} 非法"));
            }
            if band[0] < FIT_FREQ_MIN_HZ || band[1] > upper {
                return fit_err(
                    E_FIT_OPTIONS,
                    format!("{name}={band:?} 越界 [{FIT_FREQ_MIN_HZ}, {upper}]"),
                );
            }
        }
        if !self.reference_freq_hz.is_finite()
            || self.reference_freq_hz < FIT_FREQ_MIN_HZ
            || self.reference_freq_hz > upper
        {
            return fit_err(
                E_FIT_OPTIONS,
                format!("reference_freq_hz={} 非法", self.reference_freq_hz),
            );
        }
        if !self.boost_cap_db.is_finite() {
            return fit_err(E_FIT_OPTIONS, "boost_cap_db 非有限");
        }
        if self.boost_cap_db > MAX_BOOST_CAP_DB {
            warnings.push(format!(
                "boost-cap {:.2} dB 超过 D7 +{MAX_BOOST_CAP_DB} dB，按 +{MAX_BOOST_CAP_DB} dB 执行",
                self.boost_cap_db
            ));
        }
        if self.boost_cap_db < 0.0 {
            warnings.push(format!(
                "boost-cap {:.2} dB < 0，按 0（只切）执行",
                self.boost_cap_db
            ));
        }
        if !self.cut_budget_db.is_finite() || self.cut_budget_db <= 0.0 {
            return fit_err(E_FIT_OPTIONS, "cut_budget_db 须为有限正数");
        }
        if self.wide_max_stages == 0 || self.wide_max_stages > DEFAULT_MAX_TOTAL_STAGES {
            return fit_err(
                E_FIT_OPTIONS,
                format!(
                    "wide_max_stages={} 越界 [1, {DEFAULT_MAX_TOTAL_STAGES}]",
                    self.wide_max_stages
                ),
            );
        }
        if self.max_total_stages == 0 || self.max_total_stages > 32 {
            return fit_err(E_FIT_OPTIONS, "max_total_stages 越界 [1, 32]");
        }
        if !self.program_peak_reference_dbfs.is_finite() {
            return fit_err(E_FIT_OPTIONS, "program_peak_reference_dbfs 非有限");
        }
        if !self.trim_range_db[0].is_finite()
            || !self.trim_range_db[1].is_finite()
            || self.trim_range_db[0] < TRIM_MIN_DB
            || self.trim_range_db[1] > TRIM_MAX_DB
            || !(self.trim_range_db[1] > self.trim_range_db[0])
        {
            return fit_err(
                E_FIT_OPTIONS,
                format!(
                    "trim_range_db={:?} 越界（CONTRACT trim in [{TRIM_MIN_DB}, {TRIM_MAX_DB}]）",
                    self.trim_range_db
                ),
            );
        }
        if self.target_fir.enabled {
            if self.target_fir.taps < MIN_TARGET_FIR_TAPS
                || self.target_fir.taps > MAX_TARGET_FIR_TAPS
            {
                return fit_err(
                    E_FIT_OPTIONS,
                    format!(
                        "target_fir.taps={} 越界 [{MIN_TARGET_FIR_TAPS}, {MAX_TARGET_FIR_TAPS}]",
                        self.target_fir.taps
                    ),
                );
            }
            let [lo, hi] = self.target_fir.band_hz;
            if !lo.is_finite() || !hi.is_finite() || lo <= 0.0 || !(hi > lo) || hi > upper {
                return fit_err(
                    E_FIT_OPTIONS,
                    format!(
                        "target_fir.band_hz={:?} 非法（须在 [>0, {upper}]）",
                        self.target_fir.band_hz
                    ),
                );
            }
            if !self.target_fir.allow_positive_db.is_finite()
                || self.target_fir.allow_positive_db < 0.0
            {
                return fit_err(E_FIT_OPTIONS, "target_fir.allow_positive_db 须为有限非负数");
            }
            if self.target_fir.allow_positive_db > TARGET_FIR_MAX_POSITIVE_DB {
                warnings.push(format!(
                    "target_fir.allow_positive_db={:.2} 超过 D7 +{TARGET_FIR_MAX_POSITIVE_DB} dB，按 +{TARGET_FIR_MAX_POSITIVE_DB} dB 执行",
                    self.target_fir.allow_positive_db
                ));
            }
        }
        Ok(warnings)
    }

    /// 归一化后的 boost cap（D7 执行值）。
    pub fn effective_boost_cap_db(&self) -> f64 {
        self.boost_cap_db.clamp(0.0, MAX_BOOST_CAP_DB)
    }

    /// 归一化后的目标 FIR 正增益执行值。
    pub fn effective_fir_positive_cap_db(&self) -> f64 {
        if !self.target_fir.enabled {
            0.0
        } else {
            self.target_fir
                .allow_positive_db
                .clamp(0.0, TARGET_FIR_MAX_POSITIVE_DB)
        }
    }
}

/// 一次 PEQ fit 请求（只读借入 plan/target/curves/quality）。
pub struct PeqFitRequest<'a> {
    pub plan: &'a MultipointPlan,
    pub target: &'a TargetCurve,
    pub curves: &'a [PositionCurve],
    pub channel_bands: &'a [ChannelBand],
    pub design_quality: Option<&'a PositionQualityReport>,
    pub options: PeqFitOptions,
    pub created_unix: u64,
}

impl<'a> PeqFitRequest<'a> {
    pub fn new(
        plan: &'a MultipointPlan,
        target: &'a TargetCurve,
        curves: &'a [PositionCurve],
    ) -> Self {
        Self {
            plan,
            target,
            curves,
            channel_bands: &[],
            design_quality: None,
            options: PeqFitOptions::default(),
            created_unix: 0,
        }
    }

    pub fn with_options(mut self, options: PeqFitOptions) -> Self {
        self.options = options;
        self
    }

    pub fn with_channel_bands(mut self, channel_bands: &'a [ChannelBand]) -> Self {
        self.channel_bands = channel_bands;
        self
    }

    pub fn with_design_quality(mut self, quality: &'a PositionQualityReport) -> Self {
        self.design_quality = Some(quality);
        self
    }

    pub fn with_created_unix(mut self, created_unix: u64) -> Self {
        self.created_unix = created_unix;
        self
    }
}

// ---------------------------------------------------------------------------
// JSON（无 serde 依赖：确定性、字段顺序固定、f64 17 位有效数字）
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum J {
    Null,
    Bool(bool),
    Uint(u64),
    Int(i64),
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(&'static str, J)>),
}

impl J {
    fn num(value: f64) -> J {
        if !value.is_finite() {
            return J::Null;
        }
        if value == 0.0 {
            return J::Num(0.0);
        }
        J::Num(value)
    }

    fn string(value: impl Into<String>) -> J {
        J::Str(value.into())
    }

    fn pair_num(values: [f64; 2]) -> J {
        J::Arr(vec![J::num(values[0]), J::num(values[1])])
    }

    fn json_string(value: &str) -> String {
        let mut out = String::with_capacity(value.len() + 2);
        out.push('"');
        for character in value.chars() {
            match character {
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
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            J::Null => out.push_str("null"),
            J::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            J::Uint(value) => out.push_str(&value.to_string()),
            J::Int(value) => out.push_str(&value.to_string()),
            J::Num(value) => out.push_str(&format!("{value:.17e}")),
            J::Str(value) => {
                out.push_str(&J::json_string(value));
            }
            J::Arr(values) => {
                out.push('[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    value.write(out);
                }
                out.push(']');
            }
            J::Obj(fields) => {
                out.push('{');
                for (index, (key, value)) in fields.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&J::json_string(key));
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }

    fn to_json(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }
}

// ---------------------------------------------------------------------------
// 小工具（插值/平滑/响应）
// ---------------------------------------------------------------------------

/// 在 log-f 网格上取插值（端点 hold；网格须严格递增）。
fn sample_curve_db(grid: &[f64], values: &[f64], freq_hz: f64) -> f64 {
    if grid.is_empty() || values.len() != grid.len() {
        return 0.0;
    }
    if freq_hz <= grid[0] {
        return values[0];
    }
    let last = grid.len() - 1;
    if freq_hz >= grid[last] {
        return values[last];
    }
    let mut low = 0usize;
    let mut high = last;
    while high - low > 1 {
        let mid = (low + high) / 2;
        if grid[mid] <= freq_hz {
            low = mid;
        } else {
            high = mid;
        }
    }
    let f0 = grid[low].max(f64::MIN_POSITIVE);
    let f1 = grid[high].max(f64::MIN_POSITIVE);
    let ratio = (freq_hz / f0).ln() / (f1 / f0).ln();
    let t = ratio.clamp(0.0, 1.0);
    values[low] + t * (values[high] - values[low])
}

/// 1/6 oct 等移动平均（half_window 以网格点为单位的半窗）。
fn moving_average(values: &[f64], half_window: usize) -> Vec<f64> {
    let count = values.len();
    if half_window == 0 {
        return values.to_vec();
    }
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        let start = index.saturating_sub(half_window);
        let end = (index + half_window + 1).min(count);
        let slice = &values[start..end];
        out.push(slice.iter().sum::<f64>() / slice.len() as f64);
    }
    out
}

fn rms(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    (values.iter().map(|value| value * value).sum::<f64>() / values.len() as f64).sqrt()
}

fn max_abs(values: &[f64]) -> f64 {
    values
        .iter()
        .fold(0.0_f64, |acc, value| acc.max(value.abs()))
}

fn band_indices(grid: &[f64], band_hz: [f64; 2]) -> Vec<usize> {
    grid.iter()
        .enumerate()
        .filter(|(_, freq)| **freq >= band_hz[0] && **freq <= band_hz[1])
        .map(|(index, _)| index)
        .collect()
}

fn intersect_band(a: [f64; 2], b: [f64; 2]) -> Option<[f64; 2]> {
    let lo = a[0].max(b[0]);
    let hi = a[1].min(b[1]);
    if hi > lo * 1.01 {
        Some([lo, hi])
    } else {
        None
    }
}

/// 若干 RBJ 段的级联幅度（dB，求和；与 spec 5.6 自洽口径一致）。
fn bands_response_db(bands: &[ModalBand], grid: &[f64], fs: f64) -> Vec<f64> {
    let biquads: Vec<Biquad> = bands.iter().map(|band| band.biquad()).collect();
    grid.iter()
        .map(|freq| {
            biquads
                .iter()
                .map(|biquad| biquad.magnitude_db(*freq, fs))
                .sum::<f64>()
        })
        .collect()
}

fn bands_response_at(bands: &[ModalBand], freq_hz: f64, fs: f64) -> f64 {
    bands
        .iter()
        .map(|band| band.biquad().magnitude_db(freq_hz, fs))
        .sum()
}

/// 线性相位 FIR 在单点 f 的幅度（dB；直接 DFT，无第三方 FFT）。
fn fir_magnitude_db_at(coeffs: &[f64], fs: f64, freq_hz: f64) -> f64 {
    if coeffs.is_empty() {
        return 0.0;
    }
    let omega = 2.0 * std::f64::consts::PI * freq_hz / fs;
    let mut re = 0.0_f64;
    let mut im = 0.0_f64;
    for (index, coeff) in coeffs.iter().enumerate() {
        let phase = omega * index as f64;
        re += coeff * phase.cos();
        im -= coeff * phase.sin();
    }
    20.0 * re.hypot(im).max(f64::MIN_POSITIVE).log10()
}

fn fir_magnitude_db_grid(coeffs: &[f64], fs: f64, grid: &[f64]) -> Vec<f64> {
    grid.iter()
        .map(|freq| fir_magnitude_db_at(coeffs, fs, *freq))
        .collect()
}

/// 目标曲线形状（anchor 归一后）在网格上的采样。
pub struct TargetShape {
    pub grid: Vec<f64>,
    pub values_db: Vec<f64>,
    pub anchor_db: f64,
    pub id: String,
    pub sha256: String,
    pub normalization: String,
    pub reference_gain_db: f64,
    pub point_count: usize,
}

impl TargetShape {
    fn db_at(&self, freq_hz: f64) -> f64 {
        sample_curve_db(&self.grid, &self.values_db, freq_hz)
    }
}

/// 目标识别：按 canonical sha256 反查内置 flat/tilt10；对不上返回 None。
///
/// F-15（用户裁决）：现行目标**只有** flat 与 tilt10。对不上两者 sha256 的曲线不再降级为
/// `custom` 继续拟合，而由 [`prepare_target`] 返回稳定错误 E_FIT_TARGET。
fn identify_builtin_target(target: &TargetCurve, fs: f64) -> Option<&'static str> {
    let reference_gain_db = target.meta().reference_gain_db;
    let options = BuiltinOptions::new(fs).with_reference_gain_db(reference_gain_db);
    for candidate in [BuiltinTarget::Flat, BuiltinTarget::Tilt10] {
        let Ok(builtin) = builtin_target(candidate, &options) else {
            continue;
        };
        if builtin.target_sha256() == target.target_sha256() {
            return Some(candidate.id());
        }
    }
    None
}

fn prepare_target(
    target: &TargetCurve,
    fs: u32,
    reference_freq_hz: f64,
) -> Result<TargetShape, PeqFitError> {
    let fs_f = fs as f64;
    if !target.meta().runtime_rate.is_finite() || (target.meta().runtime_rate - fs_f).abs() > 1e-6 {
        return fit_err(
            E_FIT_TARGET,
            format!(
                "target runtime_rate={} != profile fs={fs}",
                target.meta().runtime_rate
            ),
        );
    }
    if target.has_phase_target() {
        return fit_err(
            E_FIT_TARGET_PHASE,
            "PEQ fit 只消费幅度目标；带 phase 的目标请走 T40/T65 相位设计",
        );
    }
    let anchor_db = target.mag_db_at(reference_freq_hz).map_err(target_error)?;
    if !anchor_db.is_finite() {
        return fit_err(E_FIT_TARGET, "target 在参考频率处非有限");
    }
    let upper = freq_upper_hz(fs_f);
    let grid = log_frequency_grid(FIT_FREQ_MIN_HZ, upper, 1.0 / 96.0);
    let mut values_db = Vec::with_capacity(grid.len());
    for freq in &grid {
        let value = target.mag_db_at(*freq).map_err(target_error)? - anchor_db;
        if !value.is_finite() {
            return fit_err(E_FIT_TARGET, format!("target @ {freq} Hz 非有限"));
        }
        values_db.push(value);
    }
    // F-15：只允许内置 flat/tilt10；custom/其它 sha256 一律稳定拒收 E_FIT_TARGET。
    let id = identify_builtin_target(target, fs_f).ok_or_else(|| {
        PeqFitError::new(
            E_FIT_TARGET,
            format!(
                "target sha256={} 不是内置 flat/tilt10（用户裁决只允许 flat/tilt10；\
                 自定义目标需先走 T11 目标导入裁决）",
                target.target_sha256()
            ),
        )
    })?;
    Ok(TargetShape {
        grid,
        values_db,
        anchor_db,
        id: id.to_string(),
        sha256: target.target_sha256().to_string(),
        normalization: target.report().normalization.as_str().to_string(),
        reference_gain_db: target.meta().reference_gain_db,
        point_count: target.report().point_count,
    })
}

// ---------------------------------------------------------------------------
// 公开报告结构（khaen-peq-fit/v1）
// ---------------------------------------------------------------------------

/// 一个 Band 的报告视图。
#[derive(Clone, Debug, PartialEq)]
pub struct BandReport {
    pub band_type: String,
    pub freq_hz: f64,
    pub q: f64,
    pub gain_db: f64,
    pub b: [f64; 3],
    pub a: [f64; 3],
    pub modal: bool,
}

impl BandReport {
    fn json(&self) -> J {
        J::Obj(vec![
            ("type", J::string(self.band_type.clone())),
            ("freq_hz", J::num(self.freq_hz)),
            ("q", J::num(self.q)),
            ("gain_db", J::num(self.gain_db)),
            (
                "b",
                J::Arr(self.b.iter().map(|value| J::num(*value)).collect()),
            ),
            (
                "a",
                J::Arr(self.a.iter().map(|value| J::num(*value)).collect()),
            ),
            ("modal", J::Bool(self.modal)),
        ])
    }
}

/// 目标驱动 2a 恢复证据（每通道；只记录实际发生的事，不含任何门限放宽）。
#[derive(Clone, Debug, PartialEq)]
pub struct TargetAwareRecoveryReport {
    /// 通道名。
    pub channel: String,
    /// 标准拟合 rule_2a FAIL 而触发恢复尝试。
    pub triggered: bool,
    /// 恢复后 rule_2a 两 clause 真过并已采纳。
    pub applied: bool,
    /// 触发时的最大正残差峰频率（Hz）。
    pub peak_freq_hz: Option<f64>,
    /// 恢复前 2a 峰高出目标（dB）。
    pub before_peak_over_db: f64,
    /// 恢复前 2a 最差变差（dB）。
    pub before_worst_worsening_db: f64,
    /// 恢复前 modal RMS（PEQ 后，dB）。
    pub before_rms_after_db: f64,
    /// 采纳后 2a 峰高出目标（dB）。
    pub after_peak_over_db: f64,
    /// 采纳后 2a 最差变差（dB）。
    pub after_worst_worsening_db: f64,
    /// 采纳后 modal RMS（PEQ 后，dB）。
    pub after_rms_after_db: f64,
    /// 实际尝试的候选数（固定预算内）。
    pub candidates_tried: usize,
    /// 采纳的额外段（未采纳时为空）。
    pub added_bands: Vec<BandReport>,
    /// 确定性选择目标（例如 "min max(peak,worsen) then |rms delta|"）。
    pub objective: String,
    /// 结果说明（中文；unresolved 时含原因与下一步）。
    pub note: String,
}

impl TargetAwareRecoveryReport {
    fn json(&self) -> J {
        let band_json = |band: &BandReport| band.json();
        J::Obj(vec![
            ("channel", J::string(self.channel.clone())),
            ("triggered", J::Bool(self.triggered)),
            ("applied", J::Bool(self.applied)),
            (
                "peak_freq_hz",
                match self.peak_freq_hz {
                    Some(value) => J::num(value),
                    None => J::Null,
                },
            ),
            (
                "before",
                J::Obj(vec![
                    ("peak_over_db", J::num(self.before_peak_over_db)),
                    ("worst_worsening_db", J::num(self.before_worst_worsening_db)),
                    ("rms_after_db", J::num(self.before_rms_after_db)),
                ]),
            ),
            (
                "after",
                J::Obj(vec![
                    ("peak_over_db", J::num(self.after_peak_over_db)),
                    ("worst_worsening_db", J::num(self.after_worst_worsening_db)),
                    ("rms_after_db", J::num(self.after_rms_after_db)),
                ]),
            ),
            ("candidates_tried", J::Uint(self.candidates_tried as u64)),
            (
                "added_bands",
                J::Arr(self.added_bands.iter().map(band_json).collect()),
            ),
            ("objective", J::string(self.objective.clone())),
            ("note", J::string(self.note.clone())),
            (
                "limits",
                J::Arr(vec![
                    J::string("no rule_2a/D7/headroom threshold was relaxed"),
                    J::string("recovery is accepted only when rule_2a both clauses pass on the real design curve"),
                    J::string("before/after values are computed from the same gate code; extra band is reported for audit"),
                ]),
            ),
        ])
    }
}

/// 频带残余统计（修正前/后 RMS 与最大绝对值）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResidualStats {
    pub band_hz: [f64; 2],
    pub rms_before_db: f64,
    pub rms_after_db: f64,
    pub max_before_db: f64,
    pub max_after_db: f64,
}

impl ResidualStats {
    fn json(&self) -> J {
        J::Obj(vec![
            ("band_hz", J::pair_num(self.band_hz)),
            ("rms_before_db", J::num(self.rms_before_db)),
            ("rms_after_db", J::num(self.rms_after_db)),
            ("max_before_db", J::num(self.max_before_db)),
            ("max_after_db", J::num(self.max_after_db)),
        ])
    }
}

/// 逐测点残余/规则摘要。
#[derive(Clone, Debug, PartialEq)]
pub struct PositionResidual {
    pub label: String,
    pub role: String,
    pub weight: f64,
    pub modal_rms_before_db: f64,
    pub modal_rms_after_peq_db: f64,
    pub modal_rms_after_p_db: f64,
    pub modal_max_after_p_db: f64,
    pub rule_2a_pass: bool,
    pub rule_2b_pass: bool,
}

impl PositionResidual {
    fn json(&self) -> J {
        J::Obj(vec![
            ("label", J::string(self.label.clone())),
            ("role", J::string(self.role.clone())),
            ("weight", J::num(self.weight)),
            ("modal_rms_before_db", J::num(self.modal_rms_before_db)),
            (
                "modal_rms_after_peq_db",
                J::num(self.modal_rms_after_peq_db),
            ),
            ("modal_rms_after_p_db", J::num(self.modal_rms_after_p_db)),
            ("modal_max_after_p_db", J::num(self.modal_max_after_p_db)),
            ("rule_2a_pass", J::Bool(self.rule_2a_pass)),
            ("rule_2b_pass", J::Bool(self.rule_2b_pass)),
        ])
    }
}

/// 单通道完整 fit 报告。
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelFitReport {
    pub name: String,
    pub index: u32,
    pub work_band_hz: [f64; 2],
    pub band_count: usize,
    pub modal_band_count: usize,
    pub wide_band_count: usize,
    pub shelf_count: usize,
    pub bands: Vec<BandReport>,
    pub single_filter_max_boost_db: f64,
    pub filter_positive_sum_db: f64,
    pub target_fir_positive_max_db: f64,
    pub applied_boost_db: f64,
    pub boost_reduced_db: f64,
    pub boost_cap_db: f64,
    pub boost_limits_pass: bool,
    pub program_peak_reference_dbfs: f64,
    pub predicted_peak_before_trim_dbfs: f64,
    pub trim_db: f64,
    pub predicted_peak_dbfs: f64,
    pub headroom_db: f64,
    pub headroom_pass: bool,
    pub target_fir_taps: usize,
    pub target_fir_band_hz: [f64; 2],
    pub target_fir_group_delay_samples: f64,
    pub target_fir_group_delay_ms: f64,
    pub target_fir_max_deviation_db: f64,
    pub target_fir_max_positive_db: f64,
    pub target_fir_coeff_sha256: String,
    pub modal_residual: ResidualStats,
    pub modal_residual_after_p: ResidualStats,
    pub wide_residual_after_p: Option<ResidualStats>,
    pub g_residual_curve: Vec<[f64; 2]>,
    pub modal_residual_per_position: Vec<PositionResidual>,
    /// 2a pass（只由 design 测点决定；holdout 单列在 holdout_rule_2a_pass）。
    pub rule_2a_pass: bool,
    /// 2b pass（只由 design 测点决定；holdout 单列在 holdout_rule_2b_pass）。
    pub rule_2b_pass: bool,
    pub rule_2a_peak_over_target_db: f64,
    pub rule_2a_worst_worsening_db: f64,
    pub rule_2b_worst_point_worsening_db: f64,
    pub rule_2b_one_third_octave_ripple_db: f64,
    pub rule_2b_new_narrow_notch_db: f64,
    pub rule_2b_raw_max_deviation_db_1_24_oct: f64,
    /// validation/holdout 2a pass（只报告，不并进 rule_2a_pass，不参与调参）。
    pub holdout_rule_2a_pass: bool,
    /// validation/holdout 2b pass（只报告，不并进 rule_2b_pass，不参与调参）。
    pub holdout_rule_2b_pass: bool,
    /// holdout 2a 最坏峰高出目标（dB）。
    pub holdout_rule_2a_peak_over_target_db: f64,
    /// holdout 2a 最坏变差（dB）。
    pub holdout_rule_2a_worst_worsening_db: f64,
    /// holdout 2b 最坏逐点恶化（dB）。
    pub holdout_rule_2b_worst_point_worsening_db: f64,
    /// holdout 2b 最坏 1/3 oct ripple（dB）。
    pub holdout_rule_2b_one_third_octave_ripple_db: f64,
    /// holdout 2b 最坏新增窄带缺口（dB）。
    pub holdout_rule_2b_new_narrow_notch_db: f64,
    /// holdout 2b 1/24 oct 原始分辨率最大偏差（dB）。
    pub holdout_rule_2b_raw_max_deviation_db_1_24_oct: f64,
    pub unfilled_dips: Vec<[f64; 2]>,
    pub warnings: Vec<String>,
}

impl ChannelFitReport {
    fn json(&self) -> J {
        J::Obj(vec![
            ("name", J::string(self.name.clone())),
            ("index", J::Uint(self.index as u64)),
            ("work_band_hz", J::pair_num(self.work_band_hz)),
            ("band_count", J::Uint(self.band_count as u64)),
            ("modal_band_count", J::Uint(self.modal_band_count as u64)),
            ("wide_band_count", J::Uint(self.wide_band_count as u64)),
            ("shelf_count", J::Uint(self.shelf_count as u64)),
            (
                "bands",
                J::Arr(self.bands.iter().map(BandReport::json).collect()),
            ),
            (
                "boost",
                J::Obj(vec![
                    (
                        "single_filter_max_db",
                        J::num(self.single_filter_max_boost_db),
                    ),
                    (
                        "filter_positive_sum_db",
                        J::num(self.filter_positive_sum_db),
                    ),
                    (
                        "target_fir_positive_max_db",
                        J::num(self.target_fir_positive_max_db),
                    ),
                    ("applied_boost_db", J::num(self.applied_boost_db)),
                    ("boost_reduced_db", J::num(self.boost_reduced_db)),
                    ("cap_db", J::num(self.boost_cap_db)),
                    ("limits_pass", J::Bool(self.boost_limits_pass)),
                ]),
            ),
            (
                "headroom",
                J::Obj(vec![
                    (
                        "program_peak_reference_dbfs",
                        J::num(self.program_peak_reference_dbfs),
                    ),
                    (
                        "predicted_peak_before_trim_dbfs",
                        J::num(self.predicted_peak_before_trim_dbfs),
                    ),
                    ("trim_db", J::num(self.trim_db)),
                    ("predicted_peak_dbfs", J::num(self.predicted_peak_dbfs)),
                    ("headroom_db", J::num(self.headroom_db)),
                    ("pass", J::Bool(self.headroom_pass)),
                ]),
            ),
            (
                "target_fir",
                J::Obj(vec![
                    ("taps", J::Uint(self.target_fir_taps as u64)),
                    ("band_hz", J::pair_num(self.target_fir_band_hz)),
                    (
                        "group_delay_samples",
                        J::num(self.target_fir_group_delay_samples),
                    ),
                    ("group_delay_ms", J::num(self.target_fir_group_delay_ms)),
                    ("max_deviation_db", J::num(self.target_fir_max_deviation_db)),
                    ("max_positive_db", J::num(self.target_fir_max_positive_db)),
                    (
                        "coeff_sha256",
                        J::string(self.target_fir_coeff_sha256.clone()),
                    ),
                ]),
            ),
            ("modal_residual", self.modal_residual.json()),
            ("modal_residual_after_p", self.modal_residual_after_p.json()),
            (
                "wide_residual_after_p",
                match self.wide_residual_after_p {
                    Some(stats) => stats.json(),
                    None => J::Null,
                },
            ),
            (
                "g_residual_curve",
                J::Arr(
                    self.g_residual_curve
                        .iter()
                        .map(|pair| J::pair_num(*pair))
                        .collect(),
                ),
            ),
            (
                "modal_residual_per_position",
                J::Arr(
                    self.modal_residual_per_position
                        .iter()
                        .map(PositionResidual::json)
                        .collect(),
                ),
            ),
            (
                "rules",
                J::Obj(vec![
                    ("rule_2a_pass", J::Bool(self.rule_2a_pass)),
                    ("rule_2b_pass", J::Bool(self.rule_2b_pass)),
                    (
                        "rule_2a_peak_over_target_db",
                        J::num(self.rule_2a_peak_over_target_db),
                    ),
                    (
                        "rule_2a_worst_worsening_db",
                        J::num(self.rule_2a_worst_worsening_db),
                    ),
                    (
                        "rule_2b_worst_point_worsening_db",
                        J::num(self.rule_2b_worst_point_worsening_db),
                    ),
                    (
                        "rule_2b_one_third_octave_ripple_db",
                        J::num(self.rule_2b_one_third_octave_ripple_db),
                    ),
                    (
                        "rule_2b_new_narrow_notch_db",
                        J::num(self.rule_2b_new_narrow_notch_db),
                    ),
                    (
                        "rule_2b_raw_max_deviation_db_1_24_oct",
                        J::num(self.rule_2b_raw_max_deviation_db_1_24_oct),
                    ),
                ]),
            ),
            (
                "holdout_rules",
                J::Obj(vec![
                    ("holdout_rule_2a_pass", J::Bool(self.holdout_rule_2a_pass)),
                    ("holdout_rule_2b_pass", J::Bool(self.holdout_rule_2b_pass)),
                    (
                        "holdout_rule_2a_peak_over_target_db",
                        J::num(self.holdout_rule_2a_peak_over_target_db),
                    ),
                    (
                        "holdout_rule_2a_worst_worsening_db",
                        J::num(self.holdout_rule_2a_worst_worsening_db),
                    ),
                    (
                        "holdout_rule_2b_worst_point_worsening_db",
                        J::num(self.holdout_rule_2b_worst_point_worsening_db),
                    ),
                    (
                        "holdout_rule_2b_one_third_octave_ripple_db",
                        J::num(self.holdout_rule_2b_one_third_octave_ripple_db),
                    ),
                    (
                        "holdout_rule_2b_new_narrow_notch_db",
                        J::num(self.holdout_rule_2b_new_narrow_notch_db),
                    ),
                    (
                        "holdout_rule_2b_raw_max_deviation_db_1_24_oct",
                        J::num(self.holdout_rule_2b_raw_max_deviation_db_1_24_oct),
                    ),
                ]),
            ),
            (
                "unfilled_dips",
                J::Arr(
                    self.unfilled_dips
                        .iter()
                        .map(|pair| J::pair_num(*pair))
                        .collect(),
                ),
            ),
            (
                "warnings",
                J::Arr(
                    self.warnings
                        .iter()
                        .map(|line| J::string(line.clone()))
                        .collect(),
                ),
            ),
        ])
    }
}

/// 目标曲线报告信息。
#[derive(Clone, Debug, PartialEq)]
pub struct FitTargetReport {
    pub id: String,
    pub sha256: String,
    pub normalization: String,
    pub reference_gain_db: f64,
    pub anchor_db: f64,
    pub reference_freq_hz: f64,
    pub point_count: usize,
}

/// 质量门报告信息。
#[derive(Clone, Debug, PartialEq)]
pub struct FitQualityReport {
    pub required: bool,
    pub present: bool,
    pub scope: String,
    pub plan_sha256: String,
    pub continue_allowed: bool,
    pub points_usable: usize,
    pub points_total: usize,
}

/// 选项报告（可复现）。
#[derive(Clone, Debug, PartialEq)]
pub struct FitOptionsReport {
    pub modal_band_hz: [f64; 2],
    pub wide_band_hz: [f64; 2],
    pub reference_freq_hz: f64,
    pub boost_cap_db: f64,
    pub cut_budget_db: f64,
    pub wide_max_stages: usize,
    pub max_total_stages: usize,
    pub low_band_trusted: bool,
    pub program_peak_reference_dbfs: f64,
    pub trim_range_db: [f64; 2],
    pub target_fir_enabled: bool,
    pub target_fir_taps: usize,
    pub target_fir_band_hz: [f64; 2],
    pub target_fir_allow_positive_db: f64,
    pub require_design_quality: bool,
    pub require_t6_minimums: bool,
}

/// 时轴/公共 D0 报告（T49 口径；P 链延迟信息）。
#[derive(Clone, Debug, PartialEq)]
pub struct FitDelayReport {
    pub phase_basis: String,
    pub common_d0_samples: i64,
    pub common_d0_ms: f64,
}

/// 总体检查位。
///
/// all_rule_2a_pass/all_rule_2b_pass 只汇总各通道的 **design** pass 位；
/// validation/holdout 的 2a/2b 单列在每条 ChannelFitReport 的 holdout_* 字段，
/// 不参与这里的总体检查位。
#[derive(Clone, Debug, PartialEq)]
pub struct FitChecks {
    pub all_rule_2a_pass: bool,
    pub all_rule_2b_pass: bool,
    pub all_headroom_pass: bool,
    pub all_boost_limits_pass: bool,
}

/// 一次完整 PEQ fit 报告（khaen-peq-fit/v1）。
#[derive(Clone, Debug, PartialEq)]
pub struct PeqFitReport {
    pub tool_name: String,
    pub tool_version: String,
    pub created_unix: u64,
    pub fs: u32,
    pub session_id: String,
    pub layout: String,
    pub plan_sha256: String,
    pub measurement_sha256: String,
    pub target: FitTargetReport,
    pub quality: FitQualityReport,
    pub options: FitOptionsReport,
    pub delay: FitDelayReport,
    pub channels: Vec<ChannelFitReport>,
    pub p_sha256: String,
    pub checks: FitChecks,
    pub warnings: Vec<String>,
}

impl PeqFitReport {
    /// 报告 JSON（确定性；同输入同字节）。
    pub fn to_json(&self) -> String {
        let checks = J::Obj(vec![
            ("all_rule_2a_pass", J::Bool(self.checks.all_rule_2a_pass)),
            ("all_rule_2b_pass", J::Bool(self.checks.all_rule_2b_pass)),
            ("all_headroom_pass", J::Bool(self.checks.all_headroom_pass)),
            (
                "all_boost_limits_pass",
                J::Bool(self.checks.all_boost_limits_pass),
            ),
        ]);
        let report = J::Obj(vec![
            ("schema", J::string(PEQ_FIT_SCHEMA)),
            (
                "tool",
                J::Obj(vec![
                    ("name", J::string(self.tool_name.clone())),
                    ("version", J::string(self.tool_version.clone())),
                ]),
            ),
            ("created_unix", J::Uint(self.created_unix)),
            ("fs", J::Uint(self.fs as u64)),
            ("session_id", J::string(self.session_id.clone())),
            ("layout", J::string(self.layout.clone())),
            ("plan_sha256", J::string(self.plan_sha256.clone())),
            (
                "measurement",
                J::Obj(vec![("sha256", J::string(self.measurement_sha256.clone()))]),
            ),
            (
                "target",
                J::Obj(vec![
                    ("id", J::string(self.target.id.clone())),
                    ("sha256", J::string(self.target.sha256.clone())),
                    (
                        "normalization",
                        J::string(self.target.normalization.clone()),
                    ),
                    ("reference_gain_db", J::num(self.target.reference_gain_db)),
                    ("anchor_db", J::num(self.target.anchor_db)),
                    ("reference_freq_hz", J::num(self.target.reference_freq_hz)),
                    ("point_count", J::Uint(self.target.point_count as u64)),
                ]),
            ),
            (
                "quality",
                J::Obj(vec![
                    ("required", J::Bool(self.quality.required)),
                    ("present", J::Bool(self.quality.present)),
                    ("scope", J::string(self.quality.scope.clone())),
                    ("plan_sha256", J::string(self.quality.plan_sha256.clone())),
                    ("continue_allowed", J::Bool(self.quality.continue_allowed)),
                    ("points_usable", J::Uint(self.quality.points_usable as u64)),
                    ("points_total", J::Uint(self.quality.points_total as u64)),
                ]),
            ),
            (
                "options",
                J::Obj(vec![
                    ("modal_band_hz", J::pair_num(self.options.modal_band_hz)),
                    ("wide_band_hz", J::pair_num(self.options.wide_band_hz)),
                    ("reference_freq_hz", J::num(self.options.reference_freq_hz)),
                    ("boost_cap_db", J::num(self.options.boost_cap_db)),
                    ("cut_budget_db", J::num(self.options.cut_budget_db)),
                    (
                        "wide_max_stages",
                        J::Uint(self.options.wide_max_stages as u64),
                    ),
                    (
                        "max_total_stages",
                        J::Uint(self.options.max_total_stages as u64),
                    ),
                    ("low_band_trusted", J::Bool(self.options.low_band_trusted)),
                    (
                        "program_peak_reference_dbfs",
                        J::num(self.options.program_peak_reference_dbfs),
                    ),
                    ("trim_range_db", J::pair_num(self.options.trim_range_db)),
                    (
                        "target_fir",
                        J::Obj(vec![
                            ("enabled", J::Bool(self.options.target_fir_enabled)),
                            ("taps", J::Uint(self.options.target_fir_taps as u64)),
                            ("band_hz", J::pair_num(self.options.target_fir_band_hz)),
                            (
                                "allow_positive_db",
                                J::num(self.options.target_fir_allow_positive_db),
                            ),
                        ]),
                    ),
                    (
                        "require_design_quality",
                        J::Bool(self.options.require_design_quality),
                    ),
                    (
                        "require_t6_minimums",
                        J::Bool(self.options.require_t6_minimums),
                    ),
                ]),
            ),
            (
                "delay",
                J::Obj(vec![
                    ("phase_basis", J::string(self.delay.phase_basis.clone())),
                    ("common_d0_samples", J::Int(self.delay.common_d0_samples)),
                    ("common_d0_ms", J::num(self.delay.common_d0_ms)),
                ]),
            ),
            (
                "channels",
                J::Arr(self.channels.iter().map(ChannelFitReport::json).collect()),
            ),
            ("p_sha256", J::string(self.p_sha256.clone())),
            ("checks", checks),
            (
                "warnings",
                J::Arr(
                    self.warnings
                        .iter()
                        .map(|line| J::string(line.clone()))
                        .collect(),
                ),
            ),
        ]);
        report.to_json()
    }
}

// ---------------------------------------------------------------------------
// 内部结构与入口
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct PreparedPosition {
    pub label: String,
    pub role: &'static str,
    pub weight: f64,
    pub db: Vec<f64>,
}

#[derive(Clone, Debug)]
pub struct ChannelInput {
    pub name: String,
    pub index: u32,
    pub work_band_hz: [f64; 2],
    pub design_positions: Vec<PreparedPosition>,
    pub validation_positions: Vec<PreparedPosition>,
    pub modal_avg_db: Vec<f64>,
    pub wide_avg_db: Vec<f64>,
}

pub struct FitContext {
    pub fs: u32,
    pub fs_f: f64,
    pub upper_hz: f64,
    pub grid: Vec<f64>,
    pub target_db: Vec<f64>,
    pub target: TargetShape,
    pub plan_sha256: String,
    pub measurement_sha256: String,
    pub phase_basis: String,
    pub common_d0_samples: i64,
    pub channels: Vec<ChannelInput>,
}

/// 目标 FIR 设计结果（内部 + 报告/artifact 来源）。
#[derive(Clone, Debug)]
struct TargetFirDesign {
    enabled: bool,
    taps: usize,
    band_hz: [f64; 2],
    group_delay_samples: f64,
    coeffs: Vec<f64>,
    max_deviation_db: f64,
    max_positive_db: f64,
}

impl TargetFirDesign {
    fn disabled(band_hz: [f64; 2]) -> Self {
        Self {
            enabled: false,
            taps: 0,
            band_hz,
            group_delay_samples: 0.0,
            coeffs: Vec::new(),
            max_deviation_db: 0.0,
            max_positive_db: 0.0,
        }
    }

    fn coeff_sha256(&self) -> String {
        let mut canonical = String::with_capacity(self.coeffs.len() * 26 + 32);
        canonical.push_str("khaen-target-fir-coeffs/v1\n");
        canonical.push_str(&format!(
            "taps={} band={:.17e},{:.17e}\n",
            self.taps, self.band_hz[0], self.band_hz[1]
        ));
        for coeff in &self.coeffs {
            canonical.push_str(&format!("{coeff:.17e}\n"));
        }
        sha256_hex(canonical.as_bytes())
    }
}

/// 单通道 fit 结果（报告 + 系数 + FIR）。
struct ChannelFitOutcome {
    report: ChannelFitReport,
    channel: ChannelPeq,
    fir: TargetFirDesign,
    /// 目标驱动 2a 恢复证据（未触发时 None）。
    recovery: Option<TargetAwareRecoveryReport>,
}

/// 目标 FIR 的公开系数产物（单通道）。
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelTargetFir {
    pub name: String,
    pub index: u32,
    pub taps: usize,
    pub band_hz: [f64; 2],
    /// 线性相位目标 FIR 的固有群延迟 = (taps-1)/2 samples，已含在 `coeffs` 里；
    /// F-16：下游 plan role 延迟不得再叠加（二者同时非 0 → E_CA_DOUBLE_COUNT）。
    pub group_delay_samples: f64,
    pub coeffs: Vec<f64>,
    pub max_deviation_db: f64,
    pub max_positive_db: f64,
}

/// 一次 PEQ fit 的完整产出（报告 + 系数 + FIR）。
#[derive(Clone, Debug)]
pub struct PeqFitOutput {
    pub report: PeqFitReport,
    pub peq: PeqDesign,
    pub target_firs: Vec<ChannelTargetFir>,
    /// 目标驱动 2a 恢复证据（按通道；仅触发的通道出现）。
    pub recovery: Vec<TargetAwareRecoveryReport>,
}

/// 一个待发布的 artifact。
#[derive(Clone, Debug)]
pub struct FitArtifact {
    pub kind: ArtifactKind,
    pub path: String,
    pub bytes: Vec<u8>,
    pub sha256: String,
}

/// 待发布的 artifact 集合（report + khaen-modal-peq 系数 + 目标 FIR）。
#[derive(Clone, Debug)]
pub struct FitArtifacts {
    pub report_sha256: String,
    pub entries: Vec<FitArtifact>,
}

impl FitArtifacts {
    /// 直接写到一个目录（job artifacts_dir 语义；相对路径含 fit/ 子目录）。
    pub fn write_to_dir(&self, dir: &Path) -> std::io::Result<Vec<PathBuf>> {
        let mut written = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            let path = dir.join(&entry.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, &entry.bytes)?;
            written.push(path);
        }
        Ok(written)
    }

    /// 通过 T60 StageCtx 登记 artifact（同 job 重跑幂等；内容变化由 job.rs 拒收）。
    pub fn write_stage_artifacts(
        &self,
        ctx: &StageCtx<'_>,
    ) -> Result<Vec<ArtifactRecord>, StageFailure> {
        let mut records = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            records.push(ctx.write_artifact(
                entry.kind.clone(),
                &entry.path,
                &entry.bytes,
                &entry.sha256,
            )?);
        }
        Ok(records)
    }
}

impl PeqFitOutput {
    /// 组装并发布 artifact（报告 hash 一次性计算；coefficients 侧回绑 report hash）。
    pub fn artifacts(&self) -> FitArtifacts {
        let report_json = self.report.to_json();
        let report_sha256 = sha256_hex(report_json.as_bytes());
        let mut peq = self.peq.clone();
        peq.fit_report_sha256 = Some(report_sha256.clone());
        let peq_json = peq.to_json();
        let fir_json = self.target_fir_json(&report_sha256);
        let mut entries = Vec::with_capacity(3);
        entries.push(FitArtifact {
            kind: ArtifactKind::DesignReport,
            path: FIT_REPORT_ARTIFACT_PATH.to_string(),
            sha256: sha256_hex(report_json.as_bytes()),
            bytes: report_json.into_bytes(),
        });
        entries.push(FitArtifact {
            kind: ArtifactKind::CalibrationDraft,
            path: FIT_MODAL_PEQ_ARTIFACT_PATH.to_string(),
            sha256: sha256_hex(peq_json.as_bytes()),
            bytes: peq_json.into_bytes(),
        });
        entries.push(FitArtifact {
            kind: ArtifactKind::FirBundle,
            path: FIT_TARGET_FIR_ARTIFACT_PATH.to_string(),
            sha256: sha256_hex(fir_json.as_bytes()),
            bytes: fir_json.into_bytes(),
        });
        if self.recovery.iter().any(|report| report.triggered) {
            let recovery_json = self.recovery_json(&report_sha256);
            entries.push(FitArtifact {
                kind: ArtifactKind::Other("target_aware_recovery".to_string()),
                path: RECOVERY_ARTIFACT_PATH.to_string(),
                sha256: sha256_hex(recovery_json.as_bytes()),
                bytes: recovery_json.into_bytes(),
            });
        }
        FitArtifacts {
            report_sha256,
            entries,
        }
    }

    fn target_fir_json(&self, report_sha256: &str) -> String {
        let channels = J::Arr(
            self.target_firs
                .iter()
                .map(|fir| {
                    J::Obj(vec![
                        ("name", J::string(fir.name.clone())),
                        ("index", J::Uint(fir.index as u64)),
                        ("taps", J::Uint(fir.taps as u64)),
                        ("band_hz", J::pair_num(fir.band_hz)),
                        ("group_delay_samples", J::num(fir.group_delay_samples)),
                        (
                            "coeffs",
                            J::Arr(fir.coeffs.iter().map(|value| J::num(*value)).collect()),
                        ),
                        ("max_deviation_db", J::num(fir.max_deviation_db)),
                        ("max_positive_db", J::num(fir.max_positive_db)),
                    ])
                })
                .collect(),
        );
        let document = J::Obj(vec![
            ("schema", J::string(TARGET_FIR_SCHEMA)),
            (
                "tool",
                J::Obj(vec![
                    ("name", J::string(FIT_TOOL_NAME)),
                    ("version", J::string(FIT_TOOL_VERSION)),
                ]),
            ),
            ("created_unix", J::Uint(self.report.created_unix)),
            ("fs", J::Uint(self.report.fs as u64)),
            ("plan_sha256", J::string(self.report.plan_sha256.clone())),
            (
                "target_sha256",
                J::string(self.report.target.sha256.clone()),
            ),
            ("report_sha256", J::string(report_sha256)),
            ("channels", channels),
            (
                "warnings",
                J::Arr(
                    self.report
                        .warnings
                        .iter()
                        .map(|line| J::string(line.clone()))
                        .collect(),
                ),
            ),
        ]);
        document.to_json()
    }

    fn recovery_json(&self, report_sha256: &str) -> String {
        let channels = J::Arr(
            self.recovery
                .iter()
                .map(TargetAwareRecoveryReport::json)
                .collect(),
        );
        let document = J::Obj(vec![
            ("schema", J::string(RECOVERY_SCHEMA)),
            (
                "tool",
                J::Obj(vec![
                    ("name", J::string(FIT_TOOL_NAME)),
                    ("version", J::string(FIT_TOOL_VERSION)),
                ]),
            ),
            ("created_unix", J::Uint(self.report.created_unix)),
            ("fs", J::Uint(self.report.fs as u64)),
            ("plan_sha256", J::string(self.report.plan_sha256.clone())),
            (
                "measurement_sha256",
                J::string(self.report.measurement_sha256.clone()),
            ),
            ("target_id", J::string(self.report.target.id.clone())),
            ("report_sha256", J::string(report_sha256.to_string())),
            ("channels", channels),
        ]);
        document.to_json()
    }

    /// 报告 JSON（便于测试/调用方直接写盘）。
    pub fn report_json(&self) -> String {
        self.report.to_json()
    }
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

/// 执行一次完整 PEQ fit（模态 + 宽带 + 目标 FIR + trim），发布系数与报告。
pub fn fit_peq(request: &PeqFitRequest<'_>) -> Result<PeqFitOutput, PeqFitError> {
    let context = prepare_context(request)?;
    let option_warnings = request.options.validate(context.fs)?;
    let mut warnings: Vec<String> = option_warnings;
    let mut outcomes = Vec::with_capacity(context.channels.len());
    for channel in &context.channels {
        let outcome = fit_channel(&context, channel, &request.options, request.created_unix)?;
        outcomes.push(outcome);
    }
    let output = assemble_output(request, &context, &outcomes, &mut warnings)?;
    Ok(output)
}

// ---------------------------------------------------------------------------
// 输入准备（plan / target / curves / quality / 通道工作带）
// ---------------------------------------------------------------------------

fn prepare_context(request: &PeqFitRequest<'_>) -> Result<FitContext, PeqFitError> {
    let plan = request.plan;
    plan.validate()
        .map_err(|error| PeqFitError::new(E_FIT_PLAN, error.to_string()))?;
    if request.options.require_t6_minimums {
        plan.require_t6_minimums()
            .map_err(|error| PeqFitError::new(E_FIT_PLAN, error.to_string()))?;
    }
    let fs = plan.sample_rate_hz;
    validate_sample_rate(fs).map_err(|error| PeqFitError::new(E_FIT_FS, error.to_string()))?;
    let fs_f = fs as f64;
    let upper_hz = freq_upper_hz(fs_f);
    let target = prepare_target(request.target, fs, request.options.reference_freq_hz)?;

    // 质量门：T63 明确不允许在缺 SNR/clock/phase 时继续。
    if let Some(report) = request.design_quality {
        if report.scope != QualityScope::Design {
            return fit_err(
                E_FIT_QUALITY_STALE,
                format!(
                    "design_quality scope={}（须为 design）",
                    report.scope.as_str()
                ),
            );
        }
        if report.plan_sha256 != plan.plan_sha256() {
            return fit_err(
                E_FIT_QUALITY_STALE,
                "design_quality 与当前 plan 不匹配（stale）",
            );
        }
        if !report.continue_allowed() {
            return fit_err(
                E_FIT_QUALITY_BLOCKED,
                format!(
                    "T63 quality gate 拒绝继续（{}/{} usable）：{}",
                    report.points_usable,
                    report.points_total,
                    report.reasons.join(",")
                ),
            );
        }
    } else if request.options.require_design_quality {
        return fit_err(
            E_FIT_QUALITY_MISSING,
            "缺 T63 design scope quality 报告，不得进入拟合",
        );
    }

    if request.curves.is_empty() {
        return fit_err(E_FIT_CURVE, "curves 为空");
    }
    let plan_positions: Vec<(String, &'static str, f64)> = plan
        .design_positions()
        .map(|position| (position.label.clone(), position.role_str(), position.weight))
        .chain(
            plan.validation_positions()
                .map(|position| (position.label.clone(), position.role_str(), position.weight)),
        )
        .collect();
    // 通道集合与曲线合法性。
    let mut channel_names: Vec<String> = Vec::new();
    let mut seen_pairs: Vec<(String, String)> = Vec::new();
    for curve in request.curves {
        curve.validate()?;
        if !plan_positions
            .iter()
            .any(|(label, _, _)| label == &curve.position)
        {
            return fit_err(
                E_FIT_UNKNOWN_POSITION,
                format!("曲线测点 {} 不在 plan 中", curve.position),
            );
        }
        let pair = (curve.position.clone(), curve.channel.clone());
        if seen_pairs.contains(&pair) {
            return fit_err(
                E_FIT_DUPLICATE,
                format!("重复曲线（测点 {} 通道 {}）", pair.0, pair.1),
            );
        }
        seen_pairs.push(pair);
        if !channel_names.contains(&curve.channel) {
            channel_names.push(curve.channel.clone());
        }
    }
    channel_names.sort();
    // 每个 design/validation 测点在每个通道都必须有曲线。
    for name in &channel_names {
        for (label, _, _) in &plan_positions {
            let present = request
                .curves
                .iter()
                .any(|curve| &curve.channel == name && &curve.position == label);
            if !present {
                return fit_err(
                    E_FIT_MISSING_POSITION,
                    format!("缺曲线（测点 {label} 通道 {name}）"),
                );
            }
        }
    }
    // 通道工作带。
    let mut bands: Vec<(String, [f64; 2])> = Vec::new();
    for band in request.channel_bands {
        if !channel_names.contains(&band.channel) {
            return fit_err(
                E_FIT_BAND,
                format!("channel_bands 通道 {} 无对应曲线", band.channel),
            );
        }
        if bands.iter().any(|(name, _)| name == &band.channel) {
            return fit_err(
                E_FIT_BAND,
                format!("channel_bands 重复通道 {}", band.channel),
            );
        }
        if !band.band_hz[0].is_finite()
            || !band.band_hz[1].is_finite()
            || !(band.band_hz[1] > band.band_hz[0])
        {
            return fit_err(
                E_FIT_BAND,
                format!(
                    "channel_bands {} band {:?} 非法",
                    band.channel, band.band_hz
                ),
            );
        }
        bands.push((band.channel.clone(), band.band_hz));
    }

    // 公共拟合网格：所有曲线与目标覆盖范围的交集。
    let mut lo = FIT_FREQ_MIN_HZ;
    let mut hi = upper_hz;
    for curve in request.curves {
        lo = lo.max(curve.points[0].freq_hz);
        hi = hi.min(curve.points[curve.points.len() - 1].freq_hz);
    }
    hi = hi.min(FIT_FREQ_MAX_HZ);
    if !(hi > lo * 2.0) {
        return fit_err(E_FIT_CURVE, format!("曲线覆盖范围过窄（lo={lo}, hi={hi}）"));
    }
    let grid = log_frequency_grid(lo, hi, FIT_GRID_OCTAVE);
    let target_db: Vec<f64> = grid.iter().map(|freq| target.db_at(*freq)).collect();

    let phase_basis = PhaseBasis::parse(&plan.phase_ref)
        .map(|basis| basis.as_str().to_string())
        .map_err(|error| PeqFitError::new(E_FIT_PLAN, error.to_string()))?;
    let common_d0_samples = plan.common_d0.samples();

    let mut channels: Vec<ChannelInput> = Vec::with_capacity(channel_names.len());
    for (index, name) in channel_names.iter().enumerate() {
        let work_band_hz = match bands.iter().find(|(channel, _)| channel == name) {
            Some((_, band)) => [band[0].max(lo), band[1].min(hi)],
            None => [lo, hi],
        };
        if !(work_band_hz[1] > work_band_hz[0] * 1.01) {
            return fit_err(E_FIT_BAND, format!("通道 {name} 工作带与拟合范围交集不足"));
        }
        let mut design_positions = Vec::new();
        let mut validation_positions = Vec::new();
        for (label, role, weight) in &plan_positions {
            let curve = request
                .curves
                .iter()
                .find(|curve| &curve.channel == name && &curve.position == label)
                .expect("prepared curve missing");
            let db: Vec<f64> = grid
                .iter()
                .map(|freq| {
                    interpolate_curve_db(&curve.points, *freq).map_err(|error| {
                        PeqFitError::new(
                            E_FIT_CURVE,
                            format!("曲线插值失败（{label}/{name}）：{error}"),
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let prepared = PreparedPosition {
                label: label.clone(),
                role,
                weight: *weight,
                db,
            };
            if *role == "design" {
                design_positions.push(prepared);
            } else {
                validation_positions.push(prepared);
            }
        }
        let modal_avg_db = weighted_magnitude_average(&design_positions);
        let wide_avg_db = weighted_power_average(&design_positions);
        let half_window = ((WIDE_SMOOTH_OCTAVE / 2.0) / FIT_GRID_OCTAVE)
            .round()
            .max(1.0) as usize;
        let wide_avg_db = moving_average(&wide_avg_db, half_window);
        channels.push(ChannelInput {
            name: name.clone(),
            index: index as u32,
            work_band_hz,
            design_positions,
            validation_positions,
            modal_avg_db,
            wide_avg_db,
        });
    }

    let measurement_sha256 = measurement_digest(plan, request.curves);
    Ok(FitContext {
        fs,
        fs_f,
        upper_hz,
        grid,
        target_db,
        target,
        plan_sha256: plan.plan_sha256(),
        measurement_sha256,
        phase_basis,
        common_d0_samples,
        channels,
    })
}

/// 测点曲线的确定性摘要（position/channel/点表；用于 stale/绑定）。
fn measurement_digest(plan: &MultipointPlan, curves: &[PositionCurve]) -> String {
    let mut sorted: Vec<&PositionCurve> = curves.iter().collect();
    sorted.sort_by(|a, b| {
        (a.position.as_str(), a.channel.as_str()).cmp(&(b.position.as_str(), b.channel.as_str()))
    });
    let mut canonical = String::with_capacity(sorted.len() * 4096);
    canonical.push_str("khaen-peq-fit-measurements/v1\n");
    canonical.push_str(&plan.plan_sha256());
    canonical.push('\n');
    for curve in sorted {
        canonical.push_str(&format!("{}\t{}\t", curve.position, curve.channel));
        for point in &curve.points {
            canonical.push_str(&format!("{:.17e}:{:.17e};", point.freq_hz, point.mag_db));
        }
        canonical.push('\n');
    }
    sha256_hex(canonical.as_bytes())
}

/// 模态用幅度（线性幅度）加权平均；同频测点差 > 6 dB 时降权（requirements 3 AC4）。
fn weighted_magnitude_average(positions: &[PreparedPosition]) -> Vec<f64> {
    if positions.is_empty() {
        return Vec::new();
    }
    let count = positions[0].db.len();
    let total_weight: f64 = positions.iter().map(|position| position.weight).sum();
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        let amplitude = |position: &PreparedPosition| 10f64.powf(position.db[index] / 20.0);
        let mut mean = positions
            .iter()
            .map(|position| position.weight * amplitude(position))
            .sum::<f64>()
            / total_weight;
        let mut min_db = f64::INFINITY;
        let mut max_db = f64::NEG_INFINITY;
        for position in positions {
            min_db = min_db.min(position.db[index]);
            max_db = max_db.max(position.db[index]);
        }
        if positions.len() >= 2 && (max_db - min_db) > MODAL_SPREAD_DOWNWEIGHT_DB {
            let mean_db = 20.0 * mean.max(f64::MIN_POSITIVE).log10();
            let mut weight_sum = 0.0_f64;
            let mut acc = 0.0_f64;
            for position in positions {
                let deviation = (position.db[index] - mean_db) / 3.0;
                let weight = position.weight / (1.0 + deviation * deviation);
                acc += weight * amplitude(position);
                weight_sum += weight;
            }
            if weight_sum > 0.0 {
                mean = acc / weight_sum;
            }
        }
        out.push(20.0 * mean.max(f64::MIN_POSITIVE).log10());
    }
    out
}

/// 宽带用功率平均后再开方（requirements 3 AC4）。
fn weighted_power_average(positions: &[PreparedPosition]) -> Vec<f64> {
    if positions.is_empty() {
        return Vec::new();
    }
    let count = positions[0].db.len();
    let total_weight: f64 = positions.iter().map(|position| position.weight).sum();
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        let power = positions
            .iter()
            .map(|position| position.weight * 10f64.powf(position.db[index] / 10.0))
            .sum::<f64>()
            / total_weight;
        out.push(10.0 * power.max(f64::MIN_POSITIVE).log10());
    }
    out
}

// ---------------------------------------------------------------------------
// 频带拟合：模态（~10 段，只切）+ 宽带（shelf/peaking 对齐目标）
// ---------------------------------------------------------------------------

struct BandsFit {
    bands: Vec<ModalBand>,
    modal_count: usize,
    wide_count: usize,
    warnings: Vec<String>,
}

fn modal_band_for(input: &ChannelInput, options: &PeqFitOptions, grid: &[f64]) -> Option<[f64; 2]> {
    let band = intersect_band(options.modal_band_hz, input.work_band_hz)?;
    let band = intersect_band(band, [grid[0], grid[grid.len() - 1]])?;
    Some(band)
}

/// 宽带拟合/残差带：调用者宽带范围 ∩ 工作带 ∩ 网格，并**排除模态带**。
/// 这样 T41 producer 的“模态 peaking <=10”计数不会被宽带段污染，也不会与 G 双切。
fn wide_band_for(input: &ChannelInput, options: &PeqFitOptions, grid: &[f64]) -> Option<[f64; 2]> {
    let mut band = intersect_band(options.wide_band_hz, input.work_band_hz)?;
    band = intersect_band(band, [grid[0], grid[grid.len() - 1]])?;
    if let Some(modal) = intersect_band(options.modal_band_hz, input.work_band_hz) {
        if modal[1] > band[0] && modal[0] < band[1] {
            band = [band[0].max(modal[1]), band[1]];
        }
    }
    if band[1] > band[0] * 1.01 {
        Some(band)
    } else {
        None
    }
}

/// 逐测点模态突出度（峰 - max(左右 1/6 oct 低点)），用于多测点可重现性门。
fn position_prominence_db(position: &PreparedPosition, grid: &[f64], freq_hz: f64) -> f64 {
    let at = sample_curve_db(grid, &position.db, freq_hz);
    let left = sample_curve_db(grid, &position.db, freq_hz / 1.122462048309373);
    let right = sample_curve_db(grid, &position.db, freq_hz * 1.122462048309373);
    at - left.max(right)
}

fn fit_bands(
    context: &FitContext,
    input: &ChannelInput,
    options: &PeqFitOptions,
) -> Result<BandsFit, PeqFitError> {
    let fs_f = context.fs_f;
    let mut warnings = Vec::new();
    let mut modal_bands: Vec<ModalBand> = Vec::new();
    let modal_band = modal_band_for(input, options, &context.grid);
    let mut cut_sum = 0.0_f64;
    if let Some(band_hz) = modal_band {
        let curve_points: Vec<CurvePoint> = context
            .grid
            .iter()
            .zip(input.modal_avg_db.iter())
            .map(|(freq, db)| CurvePoint::new(*freq, *db))
            .collect();
        let config = ModalDetectConfig {
            fs: fs_f,
            band_hz,
            ..ModalDetectConfig::default()
        };
        let detected: Vec<DetectedMode> =
            detect_modal_modes(&curve_points, &config).map_err(|error| peq_error(error))?;
        for mode in detected {
            if modal_bands.len() >= MAX_MODAL_PEAKINGS {
                warnings.push(format!(
                    "通道 {} 模态段达到 {MAX_MODAL_PEAKINGS} 上限，后续检出忽略",
                    input.name
                ));
                break;
            }
            if input.design_positions.len() >= 2 {
                let reproducible = input
                    .design_positions
                    .iter()
                    .filter(|position| {
                        position_prominence_db(position, &context.grid, mode.freq_hz)
                            >= POSITION_MIN_PROMINENCE_DB
                    })
                    .count();
                if reproducible < 2 {
                    warnings.push(format!(
                        "通道 {} 模态 {:.1} Hz 不可重现（仅 {} 个测点有 >={} dB 峰），跳过",
                        input.name, mode.freq_hz, reproducible, POSITION_MIN_PROMINENCE_DB
                    ));
                    continue;
                }
            }
            if mode.freq_hz <= 60.0 && !options.low_band_trusted {
                warnings.push(format!(
                    "通道 {} 20-60 Hz 频段不可信，跳过模态 {:.1} Hz",
                    input.name, mode.freq_hz
                ));
                continue;
            }
            if mode.q_unresolved {
                warnings.push(format!(
                    "通道 {} 模态 {:.1} Hz 带宽窄于网格，Q={:.2} 标记 q_unresolved（采用最细可分辨 Q）",
                    input.name, mode.freq_hz, mode.q
                ));
            }
            let remaining = (options.cut_budget_db - cut_sum).max(0.0);
            if remaining < 0.2 {
                warnings.push(format!(
                    "通道 {} 模态总 cut 达到预算 {:.1} dB，停止追加模态段",
                    input.name, options.cut_budget_db
                ));
                break;
            }
            // 多测点：取各测点局部突出度的最大值，保证最坏测点也不残留 >+3 dB 峰
            // （只切；单段仍受 T41 max_cut 12 dB 限制）。
            let mut effective_prominence = mode.prominence_db;
            if input.design_positions.len() >= 2 {
                for position in &input.design_positions {
                    let prominence = position_prominence_db(position, &context.grid, mode.freq_hz);
                    effective_prominence = effective_prominence.max(prominence);
                }
            }
            let gain_db = (-(effective_prominence - config.target_margin_db)
                .min(config.max_cut_db))
            .min(0.0)
            .max(-remaining);
            if gain_db.abs() < 0.2 {
                continue;
            }
            cut_sum += -gain_db;
            modal_bands.push(
                ModalBand::from_params(BandType::Peaking, mode.freq_hz, mode.q, gain_db, fs_f)
                    .map_err(peq_error)?,
            );
        }
    } else {
        warnings.push(format!(
            "通道 {} 模态带与工作带无交集，跳过模态 PEQ",
            input.name
        ));
    }

    let mut wide_bands: Vec<ModalBand> = Vec::new();
    if let Some(wide_band) = wide_band_for(input, options, &context.grid) {
        let indices = band_indices(&context.grid, wide_band);
        if indices.len() >= 8 {
            let mut current = bands_response_db(&modal_bands, &context.grid, fs_f);
            let mut boost_room = options.effective_boost_cap_db();
            let mut positive_sum = 0.0_f64;
            fit_wide_bands(
                context,
                input,
                options,
                wide_band,
                &indices,
                &modal_bands,
                &mut wide_bands,
                &mut current,
                &mut positive_sum,
                &mut boost_room,
                &mut cut_sum,
                &mut warnings,
            )?;
        } else {
            warnings.push(format!(
                "通道 {} 宽带带内网格点不足，跳过宽带段",
                input.name
            ));
        }
    } else {
        warnings.push(format!(
            "通道 {} 宽带带与工作带无交集，跳过宽带 PEQ/shelf",
            input.name
        ));
    }

    let mut bands = modal_bands.clone();
    bands.extend(wide_bands.iter().copied());
    let modal_count = modal_bands.len();
    if bands.len() > options.max_total_stages {
        // 按 |gain| 从小到大删除宽带段，直到满足总段数上限。
        let mut removable: Vec<usize> = (modal_count..bands.len()).collect();
        removable.sort_by(|a, b| {
            bands[*a]
                .gain_db
                .abs()
                .partial_cmp(&bands[*b].gain_db.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let excess = bands.len() - options.max_total_stages;
        let drop: Vec<usize> = removable.into_iter().take(excess).collect();
        let mut kept = Vec::with_capacity(bands.len());
        for (index, band) in bands.into_iter().enumerate() {
            if !drop.contains(&index) {
                kept.push(band);
            }
        }
        warnings.push(format!(
            "通道 {} 合并后段数超上限 {}，删除 {} 段（|gain| 最小）",
            input.name, options.max_total_stages, excess
        ));
        bands = kept;
    }
    let wide_count = bands.len().saturating_sub(modal_count.min(bands.len()));
    if bands.len() > 32 {
        return fit_err(
            E_FIT_DESIGN,
            format!(
                "通道 {} 段数 {} > CONTRACT 硬上限 32",
                input.name,
                bands.len()
            ),
        );
    }
    Ok(BandsFit {
        bands,
        modal_count,
        wide_count,
        warnings,
    })
}

fn band_error_rms(measured: &[f64], target: &[f64], current: &[f64], indices: &[usize]) -> f64 {
    if indices.is_empty() {
        return 0.0;
    }
    let sum: f64 = indices
        .iter()
        .map(|index| {
            let error = target[*index] - measured[*index] - current[*index];
            error * error
        })
        .sum();
    (sum / indices.len() as f64).sqrt()
}

#[allow(clippy::too_many_arguments)]
fn try_add_shelf(
    context: &FitContext,
    input: &ChannelInput,
    options: &PeqFitOptions,
    _wide_band: [f64; 2],
    indices: &[usize],
    measured: &[f64],
    current: &mut [f64],
    band_type: BandType,
    freq_hz: f64,
    gain_db: f64,
    positive_sum: &mut f64,
    boost_room: &mut f64,
    cut_sum: &mut f64,
    wide_bands: &mut Vec<ModalBand>,
    warnings: &mut Vec<String>,
) -> Result<bool, PeqFitError> {
    let mut gain = gain_db.clamp(-WIDE_SINGLE_GAIN_LIMIT_DB, WIDE_SINGLE_GAIN_LIMIT_DB);
    if gain > 0.0 {
        gain = gain.min(*boost_room);
    } else {
        let room = (options.cut_budget_db - *cut_sum).max(0.0);
        gain = gain.max(-room);
    }
    if gain.abs() < WIDE_MIN_STEP_DB {
        return Ok(false);
    }
    let band = ModalBand::from_params(band_type, freq_hz, SHELF_SLOPE_S, gain, context.fs_f)
        .map_err(peq_error)?;
    let before = band_error_rms(measured, &context.target_db, current, indices);
    let mut trial = current.to_vec();
    for (index, value) in trial.iter_mut().enumerate() {
        *value += band
            .biquad()
            .magnitude_db(context.grid[index], context.fs_f);
    }
    let after = band_error_rms(measured, &context.target_db, &trial, indices);
    if after >= before - 1e-3 {
        return Ok(false);
    }
    if gain > 0.0 {
        *positive_sum += gain;
        *boost_room = (*boost_room - gain).max(0.0);
    } else {
        *cut_sum += -gain;
    }
    wide_bands.push(band);
    current.copy_from_slice(&trial);
    warnings.push(format!(
        "通道 {} 加 {} {:.1} Hz gain={:.2} dB（宽带 RMS {:.2} -> {:.2} dB）",
        input.name,
        band_type.as_str(),
        freq_hz,
        gain,
        before,
        after
    ));
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn fit_wide_bands(
    context: &FitContext,
    input: &ChannelInput,
    options: &PeqFitOptions,
    wide_band: [f64; 2],
    indices: &[usize],
    modal_bands: &[ModalBand],
    wide_bands: &mut Vec<ModalBand>,
    current: &mut [f64],
    positive_sum: &mut f64,
    boost_room: &mut f64,
    cut_sum: &mut f64,
    warnings: &mut Vec<String>,
) -> Result<(), PeqFitError> {
    let fs_f = context.fs_f;
    let measured = &input.wide_avg_db;
    let target_lo = context.target.db_at(wide_band[0]);
    let target_hi = context.target.db_at(wide_band[1]);
    let tilt = target_hi - target_lo;
    if tilt.abs() > 1.0 {
        let freq = (wide_band[0] * wide_band[1])
            .sqrt()
            .clamp(wide_band[0], wide_band[1]);
        let _ = try_add_shelf(
            context,
            input,
            options,
            wide_band,
            indices,
            measured,
            current,
            BandType::HighShelf,
            freq,
            tilt,
            positive_sum,
            boost_room,
            cut_sum,
            wide_bands,
            warnings,
        )?;
    }
    if wide_band[0] < 800.0 {
        let reference = options.reference_freq_hz.min(wide_band[1]);
        let low_delta = target_lo - context.target.db_at(reference);
        if low_delta.abs() > 1.0 {
            let freq = (wide_band[0] * 200.0_f64.min(wide_band[1]))
                .sqrt()
                .clamp(wide_band[0], wide_band[1]);
            let _ = try_add_shelf(
                context,
                input,
                options,
                wide_band,
                indices,
                measured,
                current,
                BandType::LowShelf,
                freq,
                low_delta,
                positive_sum,
                boost_room,
                cut_sum,
                wide_bands,
                warnings,
            )?;
        }
    }

    let candidates: Vec<f64> = log_frequency_grid(wide_band[0], wide_band[1], WIDE_FREQ_OCTAVE);
    let half_window = ((1.5) / FIT_GRID_OCTAVE).round().max(2.0) as usize;
    for _stage in 0..options.wide_max_stages {
        if wide_bands.len() >= options.wide_max_stages {
            break;
        }
        let base_error = band_error_rms(measured, &context.target_db, current, indices);
        if base_error < WIDE_STOP_RMS_DB {
            break;
        }
        let mut best: Option<(f64, f64, f64, f64)> = None; // (freq, q, gain, improvement)
        for &freq in &candidates {
            // 宽带段不得落在模态带内（否则会被算作模态 peaking 并与 G 双切）。
            if freq <= options.modal_band_hz[1] + 1e-9 {
                continue;
            }
            if modal_bands
                .iter()
                .any(|band| octave_distance(band.freq_hz, freq) < WIDE_MODAL_EXCLUSION_OCTAVE)
            {
                continue;
            }
            if wide_bands
                .iter()
                .any(|band| octave_distance(band.freq_hz, freq) < WIDE_SELF_EXCLUSION_OCTAVE)
            {
                continue;
            }
            let center = context
                .grid
                .iter()
                .enumerate()
                .min_by(|a, b| {
                    (a.1 - freq)
                        .abs()
                        .partial_cmp(&(b.1 - freq).abs())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(index, _)| index)
                .unwrap_or(0);
            let window_lo = center.saturating_sub(half_window);
            let window_hi = (center + half_window + 1).min(context.grid.len());
            let window: Vec<usize> = (window_lo..window_hi)
                .filter(|index| indices.contains(index))
                .collect();
            if window.len() < 8 {
                continue;
            }
            for &q in &WIDE_Q_CANDIDATES {
                // 线性化形状：用 +1 dB 的 peaking 响应作单位形状（最终用精确 biquad 验收）。
                let shape_band = ModalBand::from_params(BandType::Peaking, freq, q, 1.0, fs_f)
                    .map_err(peq_error)?;
                let shape: Vec<f64> = window
                    .iter()
                    .map(|index| shape_band.biquad().magnitude_db(context.grid[*index], fs_f))
                    .collect();
                let desired: Vec<f64> = window
                    .iter()
                    .map(|index| context.target_db[*index] - measured[*index] - current[*index])
                    .collect();
                let num: f64 = shape.iter().zip(desired.iter()).map(|(m, d)| m * d).sum();
                let den: f64 = shape.iter().map(|m| m * m).sum();
                if den <= 1e-12 {
                    continue;
                }
                let mut gain = num / den;
                let is_modal = freq >= options.modal_band_hz[0] && freq <= options.modal_band_hz[1];
                let upper = if is_modal {
                    0.0
                } else {
                    (*boost_room).max(0.0)
                };
                let lower =
                    -((options.cut_budget_db - *cut_sum).max(0.0)).min(WIDE_SINGLE_GAIN_LIMIT_DB);
                gain = gain.clamp(lower, upper.min(WIDE_SINGLE_GAIN_LIMIT_DB));
                if gain.abs() < WIDE_MIN_STEP_DB {
                    continue;
                }
                let base_sq: f64 = desired.iter().map(|d| d * d).sum();
                let gain_sq = gain * gain * den - 2.0 * gain * num;
                let improvement = base_sq - (base_sq + gain_sq);
                if improvement <= 1e-6 {
                    continue;
                }
                if best.map_or(true, |(_, _, _, best_improvement)| {
                    improvement > best_improvement
                }) {
                    best = Some((freq, q, gain, improvement));
                }
            }
        }
        let Some((freq, q, gain, _)) = best else {
            break;
        };
        let band =
            ModalBand::from_params(BandType::Peaking, freq, q, gain, fs_f).map_err(peq_error)?;
        let mut trial = current.to_vec();
        for (index, value) in trial.iter_mut().enumerate() {
            *value += band.biquad().magnitude_db(context.grid[index], fs_f);
        }
        let after = band_error_rms(measured, &context.target_db, &trial, indices);
        if after >= base_error - 1e-4 {
            break;
        }
        if gain > 0.0 {
            *positive_sum += gain;
            *boost_room = (*boost_room - gain).max(0.0);
        } else {
            *cut_sum += -gain;
        }
        wide_bands.push(band);
        current.copy_from_slice(&trial);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 2a 逐段回退（spec 5.6.2a：任一测点模态带峰高出目标 > 3 dB 或变差 > 3 dB -> 降增益/跳段）
// ---------------------------------------------------------------------------

fn curve_points(grid: &[f64], values: &[f64]) -> Vec<CurvePoint> {
    grid.iter()
        .zip(values.iter())
        .map(|(freq, db)| CurvePoint::new(*freq, *db))
        .collect()
}

fn adjust_rule_2a(
    context: &FitContext,
    input: &ChannelInput,
    channel: &mut ChannelPeq,
    modal_band: Option<[f64; 2]>,
    warnings: &mut Vec<String>,
) -> usize {
    let Some(modal_band) = modal_band else {
        return 0;
    };
    let target_points = curve_points(&context.grid, &context.target_db);
    let position_points: Vec<Vec<CurvePoint>> = input
        .design_positions
        .iter()
        .map(|position| curve_points(&context.grid, &position.db))
        .collect();
    let mut adjustments = 0usize;
    for _ in 0..MAX_RULE_2A_ADJUSTMENTS {
        let mut peak_over = f64::NEG_INFINITY;
        let mut worst_worsening = f64::NEG_INFINITY;
        let mut worst_freq: Option<f64> = None;
        for points in &position_points {
            let Ok(comparison) =
                compare_design_to_original(channel, context.fs, points, &target_points, modal_band)
            else {
                break;
            };
            peak_over = peak_over.max(comparison.rule_2a.peak_over_target_db);
            if comparison.rule_2a.worst_worsening_db > worst_worsening {
                worst_worsening = comparison.rule_2a.worst_worsening_db;
                worst_freq = comparison.rule_2a.worst_freq_hz;
            }
        }
        if peak_over <= RULE_2A_LIMIT_DB && worst_worsening <= RULE_2A_LIMIT_DB {
            break;
        }
        // 只有“相对修正前变差”能由本段负责（cut 只会让该点更深）；单纯峰仍高于目标
        // 说明 cut 不足，减少增益只会更差，必须如实报告而不是把段删光。
        if worst_worsening <= RULE_2A_LIMIT_DB {
            break;
        }
        let target_freq = worst_freq;
        // 找最近的段（无 worst_freq 时取 |gain| 最大的段）。
        let index = match target_freq {
            Some(freq) => channel
                .bands
                .iter()
                .enumerate()
                .min_by(|a, b| {
                    octave_distance(a.1.freq_hz, freq)
                        .partial_cmp(&octave_distance(b.1.freq_hz, freq))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(index, _)| index),
            None => channel
                .bands
                .iter()
                .enumerate()
                .max_by(|a, b| {
                    a.1.gain_db
                        .abs()
                        .partial_cmp(&b.1.gain_db.abs())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(index, _)| index),
        };
        let Some(index) = index else {
            break;
        };
        let band = channel.bands[index];
        let new_gain = if band.gain_db < 0.0 {
            (band.gain_db + RULE_2A_BACKOFF_STEP_DB).min(0.0)
        } else {
            (band.gain_db - RULE_2A_BACKOFF_STEP_DB).max(0.0)
        };
        let distance = (new_gain - band.gain_db).abs();
        if new_gain.abs() < 0.2 {
            warnings.push(format!(
                "通道 {} 2a 回退：删除 {:.1} Hz 段（原 gain={:.2} dB）",
                input.name, band.freq_hz, band.gain_db
            ));
            channel.bands.remove(index);
        } else {
            let Ok(rebuilt) = ModalBand::from_params(
                band.band_type,
                band.freq_hz,
                band.q,
                new_gain,
                context.fs_f,
            ) else {
                break;
            };
            channel.bands[index] = rebuilt;
            warnings.push(format!(
                "通道 {} 2a 回退：{:.1} Hz gain {:.2} -> {:.2} dB",
                input.name, band.freq_hz, band.gain_db, new_gain
            ));
        }
        adjustments += 1;
        if distance <= 0.0 {
            break;
        }
    }
    adjustments
}

/// 最坏测点在模态带的残余峰（PEQ-only），返回 (residual_db, freq_hz)。
fn worst_modal_peak_residual(
    context: &FitContext,
    input: &ChannelInput,
    channel: &ChannelPeq,
    modal_band: [f64; 2],
) -> Option<(f64, f64)> {
    let grid = log_frequency_grid(modal_band[0], modal_band[1], FIT_GRID_OCTAVE);
    let biquads: Vec<Biquad> = channel.bands.iter().map(|band| band.biquad()).collect();
    let mut worst_peak = f64::NEG_INFINITY;
    let mut worst_freq: Option<f64> = None;
    for position in &input.design_positions {
        for freq in &grid {
            let peq_db: f64 = biquads
                .iter()
                .map(|biquad| biquad.magnitude_db(*freq, context.fs_f))
                .sum();
            let residual = sample_curve_db(&context.grid, &position.db, *freq) + peq_db
                - context.target.db_at(*freq);
            if residual > worst_peak {
                worst_peak = residual;
                worst_freq = Some(*freq);
            }
        }
    }
    worst_freq.map(|freq| (worst_peak, freq))
}

/// 2a 峰高出目标 > +3 dB 时，加深最近的模态 cut（只切）；已达单段上限则如实保留失败。
fn level_rule_2a_peak_over(
    context: &FitContext,
    input: &ChannelInput,
    channel: &mut ChannelPeq,
    modal_band: Option<[f64; 2]>,
    warnings: &mut Vec<String>,
) -> usize {
    let Some(modal_band) = modal_band else {
        return 0;
    };
    let mut adjustments = 0usize;
    for _ in 0..8 {
        let Some((peak, freq)) = worst_modal_peak_residual(context, input, channel, modal_band)
        else {
            break;
        };
        if peak <= RULE_2A_LIMIT_DB {
            break;
        }
        let mut candidates: Vec<(usize, f64)> = channel
            .bands
            .iter()
            .enumerate()
            .map(|(index, band)| (index, octave_distance(band.freq_hz, freq)))
            .collect();
        candidates.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let mut adjusted = false;
        for (index, _) in candidates {
            let band = channel.bands[index];
            let limit = if band.band_type.is_shelf() {
                -WIDE_SINGLE_GAIN_LIMIT_DB
            } else {
                -T41_MODAL_MAX_CUT_DB
            };
            if band.gain_db <= limit + 1e-9 {
                continue;
            }
            let new_gain = (band.gain_db - RULE_2A_BACKOFF_STEP_DB).max(limit);
            let Ok(rebuilt) = ModalBand::from_params(
                band.band_type,
                band.freq_hz,
                band.q,
                new_gain,
                context.fs_f,
            ) else {
                continue;
            };
            channel.bands[index] = rebuilt;
            warnings.push(format!(
                "通道 {} 2a 峰高出目标 {:.2} dB @ {:.1} Hz：{:.1} Hz gain {:.2} -> {:.2} dB",
                input.name, peak, freq, band.freq_hz, band.gain_db, new_gain
            ));
            adjusted = true;
            adjustments += 1;
            break;
        }
        if !adjusted {
            break;
        }
    }
    adjustments
}

// ---------------------------------------------------------------------------
// 目标 FIR（线性相位；频域采样 + 直接 IDFT，无第三方 FFT）
// ---------------------------------------------------------------------------

fn design_target_fir(
    context: &FitContext,
    input: &ChannelInput,
    channel: &ChannelPeq,
    options: &PeqFitOptions,
    positive_cap_db: f64,
) -> Result<TargetFirDesign, PeqFitError> {
    let fir_options = options.target_fir;
    let nominal_band = fir_options.band_hz;
    if !fir_options.enabled {
        return Ok(TargetFirDesign::disabled(nominal_band));
    }
    let band = match intersect_band(
        [
            nominal_band[0].max(context.grid[0]),
            nominal_band[1].min(context.upper_hz),
        ],
        [context.grid[0], context.grid[context.grid.len() - 1]],
    ) {
        Some(band) => band,
        None => {
            return Ok(TargetFirDesign::disabled(nominal_band));
        }
    };
    let taps = fir_options.taps;
    let nyquist = context.fs_f / 2.0;
    let taper = TARGET_FIR_EDGE_TAPER_OCTAVE;
    let inner_lo = band[0] * 2f64.powf(taper);
    let inner_hi = band[1] / 2f64.powf(taper);
    let desired_at = |freq_hz: f64| -> f64 {
        if freq_hz < band[0] || freq_hz > band[1] {
            return 0.0;
        }
        let peq_db = bands_response_at(&channel.bands, freq_hz, context.fs_f);
        let measured_db = sample_curve_db(&context.grid, &input.wide_avg_db, freq_hz);
        let target_db = context.target.db_at(freq_hz);
        let mut value = target_db - measured_db - peq_db;
        value = value.clamp(TARGET_FIR_NEGATIVE_LIMIT_DB, positive_cap_db.max(0.0));
        // 频带边缘 1/3 oct 内 raised-cosine 淡出到 0 dB（抑制边缘纹波）。
        let weight = if freq_hz <= inner_lo {
            let position = ((freq_hz / band[0]).log2()) / taper;
            let position = position.clamp(0.0, 1.0);
            0.5 - 0.5 * (std::f64::consts::PI * position).cos()
        } else if freq_hz >= inner_hi {
            let position = ((band[1] / freq_hz).log2()) / taper;
            let position = position.clamp(0.0, 1.0);
            0.5 - 0.5 * (std::f64::consts::PI * position).cos()
        } else {
            1.0
        };
        value * weight
    };
    let half = taps / 2;
    let mut amplitudes = Vec::with_capacity(half + 1);
    for index in 0..=half {
        let freq = index as f64 * context.fs_f / taps as f64;
        let db = if freq < band[0] || freq > band[1] || freq >= nyquist {
            0.0
        } else {
            desired_at(freq)
        };
        amplitudes.push(10f64.powf(db / 20.0));
    }
    // F-16 唯一延迟定义：本 design 为线性相位（h[n]=h[taps-1-n]），系数自身已含固有群延迟
    //   GD = (taps-1)/2 samples（奇 taps 整数、偶 taps 半整数）。
    // 该延迟**只由系数承载**；下游 T66 plan 的 role.delay_samples(D_i) 是额外整数延迟，
    // 总延迟 = GD + D_i —— 二者同时非 0 即双扣，T66 compile 一律拒收 E_CA_DOUBLE_COUNT
    // （见 compile_apply::linear_phase_group_delay_samples / reject_double_count）。
    // 因此带 target FIR 的角色在 plan 侧只能声明 D_i = 0（定义落地前）。
    let delay = (taps as f64 - 1.0) / 2.0;
    let mut coeffs = vec![0.0_f64; taps];
    for (index, coeff) in coeffs.iter_mut().enumerate() {
        let mut sum = amplitudes[0];
        if taps % 2 == 0 {
            sum += amplitudes[half] * (std::f64::consts::PI * (index as f64 - delay)).cos();
            for bin in 1..half {
                sum += 2.0
                    * amplitudes[bin]
                    * (2.0 * std::f64::consts::PI * bin as f64 * (index as f64 - delay)
                        / taps as f64)
                        .cos();
            }
        } else {
            for bin in 1..=half {
                sum += 2.0
                    * amplitudes[bin]
                    * (2.0 * std::f64::consts::PI * bin as f64 * (index as f64 - delay)
                        / taps as f64)
                        .cos();
            }
        }
        *coeff = sum / taps as f64;
    }
    // 频带内 1/48 oct 网格核验 achieved vs desired（只报告，不迭代）。
    let eval_grid = log_frequency_grid(band[0], band[1], 1.0 / 48.0);
    let mut max_deviation = 0.0_f64;
    let mut max_positive = f64::NEG_INFINITY;
    for freq in &eval_grid {
        let achieved = fir_magnitude_db_at(&coeffs, context.fs_f, *freq);
        max_positive = max_positive.max(achieved);
        if *freq >= inner_lo && *freq <= inner_hi {
            let desired = desired_at(*freq);
            max_deviation = max_deviation.max((achieved - desired).abs());
        }
    }
    Ok(TargetFirDesign {
        enabled: true,
        taps,
        band_hz: band,
        group_delay_samples: delay,
        coeffs,
        max_deviation_db: max_deviation,
        max_positive_db: max_positive.max(0.0),
    })
}

// ---------------------------------------------------------------------------
// trim / headroom（D7：预测峰值 <= -6 dBFS、headroom >= 6 dB、trim 不越 -12）
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct HeadroomStats {
    program_peak_reference_dbfs: f64,
    predicted_peak_before_trim_dbfs: f64,
    trim_db: f64,
    predicted_peak_dbfs: f64,
    headroom_db: f64,
    boost_reduced_db: f64,
    pass: bool,
}

impl HeadroomStats {
    fn empty(program_peak_reference_dbfs: f64) -> Self {
        Self {
            program_peak_reference_dbfs,
            predicted_peak_before_trim_dbfs: program_peak_reference_dbfs,
            trim_db: 0.0,
            predicted_peak_dbfs: program_peak_reference_dbfs,
            headroom_db: -program_peak_reference_dbfs,
            boost_reduced_db: 0.0,
            pass: true,
        }
    }
}

fn headroom_grid(context: &FitContext) -> Vec<f64> {
    log_frequency_grid(FIT_FREQ_MIN_HZ, context.upper_hz, REPORT_GRID_OCTAVE)
}

fn chain_response_db(
    bands: &[ModalBand],
    fir: &TargetFirDesign,
    grid: &[f64],
    fs_f: f64,
) -> Vec<f64> {
    let mut response = bands_response_db(bands, grid, fs_f);
    if fir.enabled && !fir.coeffs.is_empty() {
        let fir_response = fir_magnitude_db_grid(&fir.coeffs, fs_f, grid);
        for (value, add) in response.iter_mut().zip(fir_response.iter()) {
            *value += *add;
        }
    }
    response
}

fn chain_peak_gain_db(grid: &[f64], response: &[f64], reference_freq_hz: f64) -> f64 {
    let reference = sample_curve_db(grid, response, reference_freq_hz);
    response
        .iter()
        .map(|value| value - reference)
        .fold(f64::NEG_INFINITY, f64::max)
}

fn enforce_headroom_and_design(
    context: &FitContext,
    input: &ChannelInput,
    channel: &mut ChannelPeq,
    options: &PeqFitOptions,
    warnings: &mut Vec<String>,
) -> Result<(TargetFirDesign, HeadroomStats, bool), PeqFitError> {
    let grid = headroom_grid(context);
    let reference_freq = options.reference_freq_hz;
    let program_peak = options.program_peak_reference_dbfs;
    let trim_min = options.trim_range_db[0];
    let cap_limit = options.effective_fir_positive_cap_db();
    let mut fir_cap = cap_limit;
    let mut fir = design_target_fir(context, input, channel, options, fir_cap)?;
    let mut reduced = false;
    let mut boost_reduced = 0.0_f64;
    let mut full_predicted = f64::INFINITY;
    for _iteration in 0..6 {
        // 1) PEQ-only 若需要 trim < trim_min，先减少正增益（D7：不满足则减少 boost 或加负 trim）。
        loop {
            let response = bands_response_db(&channel.bands, &grid, context.fs_f);
            let peq_predicted = program_peak + chain_peak_gain_db(&grid, &response, reference_freq);
            let trim_need = D7_PREDICTED_PEAK_LIMIT_DBFS - peq_predicted;
            if trim_need >= trim_min - 1e-9 {
                break;
            }
            let mut best: Option<(usize, f64)> = None;
            for (index, band) in channel.bands.iter().enumerate() {
                if band.gain_db > 0.0 && best.map_or(true, |(_, gain)| band.gain_db > gain) {
                    best = Some((index, band.gain_db));
                }
            }
            let Some((index, gain)) = best else {
                break;
            };
            let step = gain.min(1.0);
            boost_reduced += step;
            reduced = true;
            let new_gain = gain - step;
            if new_gain < 0.2 {
                channel.bands.remove(index);
            } else {
                let band = channel.bands[index];
                channel.bands[index] = ModalBand::from_params(
                    band.band_type,
                    band.freq_hz,
                    band.q,
                    new_gain,
                    context.fs_f,
                )
                .map_err(peq_error)?;
            }
        }
        let peq_response = bands_response_db(&channel.bands, &grid, context.fs_f);
        let peq_predicted = program_peak + chain_peak_gain_db(&grid, &peq_response, reference_freq);
        // 2) 目标 FIR 正增益只能占 PEQ 余量。
        let room = D7_PREDICTED_PEAK_LIMIT_DBFS - peq_predicted;
        let cap = cap_limit.min(room.max(0.0));
        if cap < fir_cap - 1e-9 {
            fir_cap = cap;
            fir = design_target_fir(context, input, channel, options, fir_cap)?;
            reduced = true;
        }
        // 3) 全链预测。
        let response = chain_response_db(&channel.bands, &fir, &grid, context.fs_f);
        full_predicted = program_peak + chain_peak_gain_db(&grid, &response, reference_freq);
        let trim_need = D7_PREDICTED_PEAK_LIMIT_DBFS - full_predicted;
        if trim_need >= trim_min - 1e-9 {
            break;
        }
        if fir_cap > 0.0 {
            let excess = (full_predicted - D7_PREDICTED_PEAK_LIMIT_DBFS).max(0.5);
            fir_cap = (fir_cap - excess).max(0.0);
            fir = design_target_fir(context, input, channel, options, fir_cap)?;
            reduced = true;
            continue;
        }
        return fit_err(
            E_FIT_HEADROOM,
            format!(
                "通道 {} D7 不可满足：预测峰值 {:.2} dBFS，需要 trim {:.2} dB < 下限 {:.2} dB，且已无正增益可减",
                input.name, full_predicted, trim_need, trim_min
            ),
        );
    }
    let trim_need = D7_PREDICTED_PEAK_LIMIT_DBFS - full_predicted;
    let trim_db = trim_need
        .clamp(options.trim_range_db[0], options.trim_range_db[1])
        .min(0.0);
    let predicted = full_predicted + trim_db;
    if predicted > D7_PREDICTED_PEAK_LIMIT_DBFS + 1e-6 {
        return fit_err(
            E_FIT_HEADROOM,
            format!(
                "通道 {} D7 削波不可满足：预测峰值 {:.3} dBFS（trim {:.2} dB 后）",
                input.name, predicted, trim_db
            ),
        );
    }
    let headroom_db = -predicted;
    let pass = headroom_db >= D7_MIN_HEADROOM_DB - 1e-9;
    if !pass {
        warnings.push(format!(
            "通道 {} headroom {:.2} dB < {:.1} dB",
            input.name, headroom_db, D7_MIN_HEADROOM_DB
        ));
    }
    Ok((
        fir,
        HeadroomStats {
            program_peak_reference_dbfs: program_peak,
            predicted_peak_before_trim_dbfs: full_predicted,
            trim_db,
            predicted_peak_dbfs: predicted,
            headroom_db,
            boost_reduced_db: boost_reduced,
            pass,
        },
        reduced,
    ))
}

// ---------------------------------------------------------------------------
// 规则聚合（T41 原图比对 2a/2b；design 与 validation/holdout 分开统计）
// ---------------------------------------------------------------------------

/// 2a/2b 聚合统计。
///
/// pass 位只由 design 测点决定（拟合质量门）；validation/holdout 的 pass/统计放
/// holdout_* 字段，只报告、不并进 rule_2a_pass/rule_2b_pass，也不参与调参。
struct RuleAggregate {
    /// design 测点 2a 全过（拟合质量门）。
    rule_2a_pass: bool,
    /// design 测点 2b 全过（拟合质量门）。
    rule_2b_pass: bool,
    /// design 测点 2a 最坏峰高出目标（dB）。
    peak_over_target_db: f64,
    /// design 测点 2a 最坏变差（dB）。
    worst_worsening_db: f64,
    /// design 测点 2b 最坏逐点恶化（dB）。
    worst_point_worsening_db: f64,
    /// design 测点 2b 最坏 1/3 oct ripple（dB）。
    one_third_octave_ripple_db: f64,
    /// design 测点 2b 最坏新增窄带缺口（dB）。
    new_narrow_notch_db: f64,
    /// design 测点 2b 1/24 oct 原始分辨率最大偏差（dB）。
    raw_max_deviation_db_1_24_oct: f64,
    /// validation/holdout 测点 2a 全过（只报告；不并进 rule_2a_pass）。
    holdout_rule_2a_pass: bool,
    /// validation/holdout 测点 2b 全过（只报告；不并进 rule_2b_pass）。
    holdout_rule_2b_pass: bool,
    /// holdout 2a 最坏峰高出目标（dB）。
    holdout_peak_over_target_db: f64,
    /// holdout 2a 最坏变差（dB）。
    holdout_worst_worsening_db: f64,
    /// holdout 2b 最坏逐点恶化（dB）。
    holdout_worst_point_worsening_db: f64,
    /// holdout 2b 最坏 1/3 oct ripple（dB）。
    holdout_one_third_octave_ripple_db: f64,
    /// holdout 2b 最坏新增窄带缺口（dB）。
    holdout_new_narrow_notch_db: f64,
    /// holdout 2b 1/24 oct 原始分辨率最大偏差（dB）。
    holdout_raw_max_deviation_db_1_24_oct: f64,
    per_position: Vec<(String, bool, bool)>,
}

impl RuleAggregate {
    fn empty() -> Self {
        Self {
            rule_2a_pass: true,
            rule_2b_pass: true,
            peak_over_target_db: f64::NEG_INFINITY,
            worst_worsening_db: f64::NEG_INFINITY,
            worst_point_worsening_db: f64::NEG_INFINITY,
            one_third_octave_ripple_db: 0.0,
            new_narrow_notch_db: 0.0,
            raw_max_deviation_db_1_24_oct: 0.0,
            holdout_rule_2a_pass: true,
            holdout_rule_2b_pass: true,
            holdout_peak_over_target_db: f64::NEG_INFINITY,
            holdout_worst_worsening_db: f64::NEG_INFINITY,
            holdout_worst_point_worsening_db: f64::NEG_INFINITY,
            holdout_one_third_octave_ripple_db: 0.0,
            holdout_new_narrow_notch_db: 0.0,
            holdout_raw_max_deviation_db_1_24_oct: 0.0,
            per_position: Vec::new(),
        }
    }
}

fn evaluate_rules(
    context: &FitContext,
    input: &ChannelInput,
    channel: &ChannelPeq,
    modal_band: Option<[f64; 2]>,
    warnings: &mut Vec<String>,
) -> Result<RuleAggregate, PeqFitError> {
    let mut aggregate = RuleAggregate::empty();
    let Some(modal_band) = modal_band else {
        return Ok(aggregate);
    };
    let target_points = curve_points(&context.grid, &context.target_db);
    // 拟合质量门 pass 位只由 design 测点决定；holdout 不参与调参也不得污染 pass 位。
    for position in &input.design_positions {
        let points = curve_points(&context.grid, &position.db);
        let comparison =
            compare_design_to_original(channel, context.fs, &points, &target_points, modal_band)
                .map_err(peq_error)?;
        let passed_2a = comparison.rule_2a.passed;
        let passed_2b = comparison.rule_2b.passed;
        aggregate.rule_2a_pass &= passed_2a;
        aggregate.rule_2b_pass &= passed_2b;
        aggregate.peak_over_target_db = aggregate
            .peak_over_target_db
            .max(comparison.rule_2a.peak_over_target_db);
        aggregate.worst_worsening_db = aggregate
            .worst_worsening_db
            .max(comparison.rule_2a.worst_worsening_db);
        aggregate.worst_point_worsening_db = aggregate
            .worst_point_worsening_db
            .max(comparison.rule_2b.worst_point_worsening_db);
        aggregate.one_third_octave_ripple_db = aggregate
            .one_third_octave_ripple_db
            .max(comparison.rule_2b.one_third_octave_ripple_db);
        aggregate.new_narrow_notch_db = aggregate
            .new_narrow_notch_db
            .max(comparison.rule_2b.new_narrow_notch_db);
        aggregate.raw_max_deviation_db_1_24_oct = aggregate
            .raw_max_deviation_db_1_24_oct
            .max(comparison.rule_2b.raw_max_deviation_db_1_24_oct);
        aggregate
            .per_position
            .push((position.label.clone(), passed_2a, passed_2b));
    }
    // validation/holdout 单独统计：只报告，不并进 rule_2a_pass/rule_2b_pass。
    for position in &input.validation_positions {
        let points = curve_points(&context.grid, &position.db);
        let comparison =
            compare_design_to_original(channel, context.fs, &points, &target_points, modal_band)
                .map_err(peq_error)?;
        let passed_2a = comparison.rule_2a.passed;
        let passed_2b = comparison.rule_2b.passed;
        aggregate.holdout_rule_2a_pass &= passed_2a;
        aggregate.holdout_rule_2b_pass &= passed_2b;
        aggregate.holdout_peak_over_target_db = aggregate
            .holdout_peak_over_target_db
            .max(comparison.rule_2a.peak_over_target_db);
        aggregate.holdout_worst_worsening_db = aggregate
            .holdout_worst_worsening_db
            .max(comparison.rule_2a.worst_worsening_db);
        aggregate.holdout_worst_point_worsening_db = aggregate
            .holdout_worst_point_worsening_db
            .max(comparison.rule_2b.worst_point_worsening_db);
        aggregate.holdout_one_third_octave_ripple_db = aggregate
            .holdout_one_third_octave_ripple_db
            .max(comparison.rule_2b.one_third_octave_ripple_db);
        aggregate.holdout_new_narrow_notch_db = aggregate
            .holdout_new_narrow_notch_db
            .max(comparison.rule_2b.new_narrow_notch_db);
        aggregate.holdout_raw_max_deviation_db_1_24_oct = aggregate
            .holdout_raw_max_deviation_db_1_24_oct
            .max(comparison.rule_2b.raw_max_deviation_db_1_24_oct);
        aggregate
            .per_position
            .push((position.label.clone(), passed_2a, passed_2b));
    }
    if !aggregate.rule_2a_pass {
        warnings.push(format!(
            "通道 {} design 2a 未通过（峰高出目标 {:.2} dB / 最差变差 {:.2} dB）",
            input.name, aggregate.peak_over_target_db, aggregate.worst_worsening_db
        ));
    }
    if !aggregate.rule_2b_pass {
        warnings.push(format!(
            "通道 {} design 2b 未通过（逐点恶化 {:.2} dB / 1/3oct ripple {:.2} dB / 新增缺口 {:.2} dB）",
            input.name,
            aggregate.worst_point_worsening_db,
            aggregate.one_third_octave_ripple_db,
            aggregate.new_narrow_notch_db
        ));
    }
    if !aggregate.holdout_rule_2a_pass {
        warnings.push(format!(
            "通道 {} holdout 2a 未通过（只报告，不影响 design pass；峰高出目标 {:.2} dB / 最差变差 {:.2} dB）",
            input.name, aggregate.holdout_peak_over_target_db, aggregate.holdout_worst_worsening_db
        ));
    }
    if !aggregate.holdout_rule_2b_pass {
        warnings.push(format!(
            "通道 {} holdout 2b 未通过（只报告，不影响 design pass；逐点恶化 {:.2} dB / 1/3oct ripple {:.2} dB / 新增缺口 {:.2} dB）",
            input.name,
            aggregate.holdout_worst_point_worsening_db,
            aggregate.holdout_one_third_octave_ripple_db,
            aggregate.holdout_new_narrow_notch_db
        ));
    }
    for position in &input.validation_positions {
        warnings.push(format!(
            "通道 {} validation 测点 {} 只报告不参与拟合（holdout）",
            input.name, position.label
        ));
    }
    Ok(aggregate)
}

// ---------------------------------------------------------------------------
// 残差（模态 PEQ 后 / P 后 / 给 G 的残余曲线 / 逐测点）
// ---------------------------------------------------------------------------

fn residual_stats(
    context: &FitContext,
    measured: &[f64],
    band_hz: [f64; 2],
    extra: &[f64],
) -> ResidualStats {
    let indices = band_indices(&context.grid, band_hz);
    if indices.is_empty() {
        return ResidualStats {
            band_hz,
            rms_before_db: 0.0,
            rms_after_db: 0.0,
            max_before_db: 0.0,
            max_after_db: 0.0,
        };
    }
    let mut before = Vec::with_capacity(indices.len());
    let mut after = Vec::with_capacity(indices.len());
    for index in &indices {
        let baseline = measured[*index] - context.target_db[*index];
        before.push(baseline);
        after.push(baseline + extra[*index]);
    }
    ResidualStats {
        band_hz,
        rms_before_db: rms(&before),
        rms_after_db: rms(&after),
        max_before_db: max_abs(&before),
        max_after_db: max_abs(&after),
    }
}

fn g_residual_curve(
    context: &FitContext,
    input: &ChannelInput,
    channel: &ChannelPeq,
    fir: &TargetFirDesign,
    trim_db: f64,
    modal_band: [f64; 2],
) -> Vec<[f64; 2]> {
    let grid = log_frequency_grid(modal_band[0], modal_band[1], REPORT_GRID_OCTAVE);
    grid.iter()
        .map(|freq| {
            let measured = sample_curve_db(&context.grid, &input.modal_avg_db, *freq);
            let peq = bands_response_at(&channel.bands, *freq, context.fs_f);
            let fir_db = if fir.enabled {
                fir_magnitude_db_at(&fir.coeffs, context.fs_f, *freq)
            } else {
                0.0
            };
            [
                *freq,
                measured + peq + fir_db + trim_db - context.target.db_at(*freq),
            ]
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn modal_position_residuals(
    context: &FitContext,
    input: &ChannelInput,
    peq_response: &[f64],
    p_response: &[f64],
    modal_band: [f64; 2],
    aggregate: &RuleAggregate,
) -> Vec<PositionResidual> {
    let indices = band_indices(&context.grid, modal_band);
    let mut out = Vec::new();
    let positions: Vec<&PreparedPosition> = input
        .design_positions
        .iter()
        .chain(input.validation_positions.iter())
        .collect();
    for position in positions {
        let mut before = Vec::with_capacity(indices.len());
        let mut after_peq = Vec::with_capacity(indices.len());
        let mut after_p = Vec::with_capacity(indices.len());
        for index in &indices {
            let baseline = position.db[*index] - context.target_db[*index];
            before.push(baseline);
            after_peq.push(baseline + peq_response[*index]);
            after_p.push(baseline + p_response[*index]);
        }
        let (passed_2a, passed_2b) = aggregate
            .per_position
            .iter()
            .find(|(label, _, _)| label == &position.label)
            .map(|(_, a, b)| (*a, *b))
            .unwrap_or((false, false));
        out.push(PositionResidual {
            label: position.label.clone(),
            role: position.role.to_string(),
            weight: position.weight,
            modal_rms_before_db: rms(&before),
            modal_rms_after_peq_db: rms(&after_peq),
            modal_rms_after_p_db: rms(&after_p),
            modal_max_after_p_db: max_abs(&after_p),
            rule_2a_pass: passed_2a,
            rule_2b_pass: passed_2b,
        });
    }
    out
}

fn find_unfilled_dips(
    context: &FitContext,
    measured: &[f64],
    extra: &[f64],
    band_hz: [f64; 2],
) -> Vec<[f64; 2]> {
    let grid = log_frequency_grid(band_hz[0], band_hz[1], REPORT_GRID_OCTAVE);
    let residual: Vec<f64> = grid
        .iter()
        .map(|freq| {
            sample_curve_db(&context.grid, measured, *freq) - context.target.db_at(*freq)
                + sample_curve_db(&context.grid, extra, *freq)
        })
        .collect();
    let mut dips: Vec<[f64; 2]> = Vec::new();
    for index in 1..grid.len().saturating_sub(1) {
        if residual[index] <= -3.0
            && residual[index] < residual[index - 1]
            && residual[index] < residual[index + 1]
        {
            dips.push([grid[index], residual[index]]);
        }
    }
    dips.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap_or(std::cmp::Ordering::Equal));
    dips.truncate(32);
    dips
}

// ---------------------------------------------------------------------------
// 单通道编排
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 目标驱动 rule_2a 恢复（target-aware recovery；不改门限）
// ---------------------------------------------------------------------------

/// 单个恢复候选（固定顺序遍历 + 确定性择优）。
struct RecoveryCandidate {
    objective: f64,
    rms_delta: f64,
    peak: f64,
    worsen: f64,
    freq: f64,
    q: f64,
    gain: f64,
    channel: ChannelPeq,
    band: ModalBand,
}

fn recovery_candidate_better(a: &RecoveryCandidate, b: &RecoveryCandidate) -> bool {
    const EPS: f64 = 1e-9;
    if (a.objective - b.objective).abs() > EPS {
        return a.objective < b.objective;
    }
    if (a.rms_delta - b.rms_delta).abs() > EPS {
        return a.rms_delta < b.rms_delta;
    }
    if (a.freq - b.freq).abs() > EPS {
        return a.freq < b.freq;
    }
    if (a.q - b.q).abs() > EPS {
        return a.q < b.q;
    }
    // 同等 objective/rms/freq/Q 时取更浅的 cut（更少干预）。
    a.gain > b.gain
}

/// 只按 design 测点聚合 rule_2a 两 clause（与 evaluate_rules 同门代码）。
pub fn aggregate_design_rule_2a(
    context: &FitContext,
    input: &ChannelInput,
    channel: &ChannelPeq,
    modal_band: [f64; 2],
) -> Result<(bool, f64, f64), PeqFitError> {
    let target_points = curve_points(&context.grid, &context.target_db);
    let mut pass = true;
    let mut peak = f64::NEG_INFINITY;
    let mut worsen = f64::NEG_INFINITY;
    for position in &input.design_positions {
        let points = curve_points(&context.grid, &position.db);
        let comparison =
            compare_design_to_original(channel, context.fs, &points, &target_points, modal_band)
                .map_err(peq_error)?;
        pass &= comparison.rule_2a.passed;
        peak = peak.max(comparison.rule_2a.peak_over_target_db);
        worsen = worsen.max(comparison.rule_2a.worst_worsening_db);
    }
    Ok((pass, peak, worsen))
}

/// PEQ-only modal RMS（与报告 modal_residual.rms_after_db 同口径）。
fn peq_modal_rms_after_db(
    context: &FitContext,
    input: &ChannelInput,
    channel: &ChannelPeq,
    modal_band: [f64; 2],
) -> f64 {
    let response = bands_response_db(&channel.bands, &context.grid, context.fs_f);
    residual_stats(context, &input.modal_avg_db, modal_band, &response).rms_after_db
}

fn recovery_band_report(band: &ModalBand, options: &PeqFitOptions) -> BandReport {
    let is_modal = band.band_type == BandType::Peaking
        && band.freq_hz >= options.modal_band_hz[0]
        && band.freq_hz <= options.modal_band_hz[1];
    BandReport {
        band_type: band.band_type.as_str().to_string(),
        freq_hz: band.freq_hz,
        q: band.q,
        gain_db: band.gain_db,
        b: band.b,
        a: band.a,
        modal: is_modal,
    }
}

fn recovery_unresolved(
    input: &ChannelInput,
    peak_freq_hz: Option<f64>,
    before_peak: f64,
    before_worsen: f64,
    before_rms: f64,
    tried: usize,
    note: impl Into<String>,
) -> TargetAwareRecoveryReport {
    TargetAwareRecoveryReport {
        channel: input.name.clone(),
        triggered: true,
        applied: false,
        peak_freq_hz,
        before_peak_over_db: before_peak,
        before_worst_worsening_db: before_worsen,
        before_rms_after_db: before_rms,
        after_peak_over_db: before_peak,
        after_worst_worsening_db: before_worsen,
        after_rms_after_db: before_rms,
        candidates_tried: tried,
        added_bands: Vec::new(),
        objective: "min max(peak,worsen) then |rms delta|".to_string(),
        note: note.into(),
    }
}

/// 标准拟合 rule_2a FAIL 时的确定性目标驱动恢复。
///
/// 只做一件事：从当前 PEQ 残差在完整 modal 评估带内找最大正残差峰（含带边；
/// 修复检测器 1/6 oct 带边盲区），按固定候选顺序试加一个 peaking cut，
/// 每个候选复用既有 level/adjust 2a 修复循环，然后在**同一门代码**下检查两 clause。
/// 仅当两 clause ≤3 dB 才采纳；否则返回 applied=false 的结构化证据。
pub fn try_target_aware_recovery(
    context: &FitContext,
    input: &ChannelInput,
    channel: &ChannelPeq,
    modal_band: [f64; 2],
    options: &PeqFitOptions,
) -> Result<(Option<ChannelPeq>, TargetAwareRecoveryReport), PeqFitError> {
    let (base_pass, base_peak, base_worsen) =
        aggregate_design_rule_2a(context, input, channel, modal_band)?;
    let before_rms = peq_modal_rms_after_db(context, input, channel, modal_band);
    if base_pass {
        return Ok((
            None,
            TargetAwareRecoveryReport {
                channel: input.name.clone(),
                triggered: false,
                applied: false,
                peak_freq_hz: None,
                before_peak_over_db: base_peak,
                before_worst_worsening_db: base_worsen,
                before_rms_after_db: before_rms,
                after_peak_over_db: base_peak,
                after_worst_worsening_db: base_worsen,
                after_rms_after_db: before_rms,
                candidates_tried: 0,
                added_bands: Vec::new(),
                objective: "min max(peak,worsen) then |rms delta|".to_string(),
                note: "rule_2a 已通过，无需恢复（未触发）".to_string(),
            },
        ));
    }
    let Some((peak_value, peak_freq)) =
        worst_modal_peak_residual(context, input, channel, modal_band)
    else {
        return Ok((
            None,
            recovery_unresolved(
                input,
                None,
                base_peak,
                base_worsen,
                before_rms,
                0,
                "无法定位 modal 正残差峰；保持 FAIL（需检查输入曲线/拟合带）",
            ),
        ));
    };
    if peak_value <= RULE_2A_LIMIT_DB {
        return Ok((
            None,
            recovery_unresolved(
                input,
                Some(peak_freq),
                base_peak,
                base_worsen,
                before_rms,
                0,
                format!(
                    "rule_2a FAIL 仅由 worsening 触发（峰 {:.2} dB <= {:.1} dB）：新增 cut 不能降低既有变差，保持 FAIL；下一步需多点/目标形状治理，不得放宽门",
                    peak_value, RULE_2A_LIMIT_DB
                ),
            ),
        ));
    }

    let mut tried = 0usize;
    let mut best: Option<RecoveryCandidate> = None;
    let mut best_attempt: Option<(f64, f64, f64, f64)> = None; // (peak,worsen,rms,freq)
    'candidates: for offset in RECOVERY_FREQ_OFFSETS_OCT {
        let freq = peak_freq * 2f64.powf(offset);
        if !freq.is_finite() || freq < modal_band[0] - 1e-9 || freq > modal_band[1] + 1e-9 {
            continue;
        }
        for q in RECOVERY_Q_ORDER {
            for gain in RECOVERY_GAIN_STEPS_DB {
                if tried >= RECOVERY_MAX_CANDIDATES {
                    break 'candidates;
                }
                tried += 1;
                let Ok(candidate) =
                    ModalBand::from_params(BandType::Peaking, freq, q, gain, context.fs_f)
                else {
                    continue;
                };
                let mut trial = channel.clone();
                if trial.bands.len() >= MAX_MODAL_PEAKINGS {
                    continue;
                }
                trial.push_band(candidate);
                let mut tmp_warnings = Vec::new();
                for _round in 0..2 {
                    let leveled = level_rule_2a_peak_over(
                        context,
                        input,
                        &mut trial,
                        Some(modal_band),
                        &mut tmp_warnings,
                    );
                    let adjusted = adjust_rule_2a(
                        context,
                        input,
                        &mut trial,
                        Some(modal_band),
                        &mut tmp_warnings,
                    );
                    if leveled == 0 && adjusted == 0 {
                        break;
                    }
                }
                let Ok((pass, peak, worsen)) =
                    aggregate_design_rule_2a(context, input, &trial, modal_band)
                else {
                    continue;
                };
                let rms = peq_modal_rms_after_db(context, input, &trial, modal_band);
                let objective = peak.max(worsen);
                match best_attempt {
                    None => best_attempt = Some((peak, worsen, rms, freq)),
                    Some((bp, bw, _br, _bf)) => {
                        if objective < bp.max(bw) {
                            best_attempt = Some((peak, worsen, rms, freq));
                        }
                    }
                }
                if !pass {
                    continue;
                }
                let candidate_state = RecoveryCandidate {
                    objective,
                    rms_delta: (rms - before_rms).abs(),
                    peak,
                    worsen,
                    freq,
                    q,
                    gain,
                    channel: trial,
                    band: candidate,
                };
                let replace = match &best {
                    None => true,
                    Some(current) => recovery_candidate_better(&candidate_state, current),
                };
                if replace {
                    best = Some(candidate_state);
                }
            }
        }
    }

    if let Some(chosen) = best {
        let added = vec![recovery_band_report(&chosen.band, options)];
        let report = TargetAwareRecoveryReport {
            channel: input.name.clone(),
            triggered: true,
            applied: true,
            peak_freq_hz: Some(peak_freq),
            before_peak_over_db: base_peak,
            before_worst_worsening_db: base_worsen,
            before_rms_after_db: before_rms,
            after_peak_over_db: chosen.peak,
            after_worst_worsening_db: chosen.worsen,
            after_rms_after_db: peq_modal_rms_after_db(context, input, &chosen.channel, modal_band),
            candidates_tried: tried,
            added_bands: added,
            objective: "min max(peak,worsen) then |rms delta|".to_string(),
            note: "accepted: rule_2a both clauses pass under unchanged gate thresholds".to_string(),
        };
        return Ok((Some(chosen.channel), report));
    }

    let (best_peak, best_worsen, best_rms, best_freq) =
        best_attempt.unwrap_or((base_peak, base_worsen, before_rms, peak_freq));
    Ok((
        None,
        recovery_unresolved(
            input,
            Some(peak_freq),
            base_peak,
            base_worsen,
            before_rms,
            tried,
            format!(
                "unresolved: {tried} candidates tried; best objective peak={best_peak:.2} worsening={best_worsen:.2} rms={best_rms:.2} @ {best_freq:.1} Hz did not pass both clauses; original FAIL kept (next steps: multi-point/measurement quality or target-shape review; no gate relaxation)",
            ),
        ),
    ))
}

fn fit_channel(
    context: &FitContext,
    input: &ChannelInput,
    options: &PeqFitOptions,
    _created_unix: u64,
) -> Result<ChannelFitOutcome, PeqFitError> {
    let bands_fit = fit_bands(context, input, options)?;
    let mut warnings = bands_fit.warnings.clone();
    let mut channel = ChannelPeq::new(&input.name, input.index).map_err(peq_error)?;
    for band in &bands_fit.bands {
        channel.push_band(*band);
    }
    let modal_band = modal_band_for(input, options, &context.grid);
    let mut adjustments = 0usize;
    let mut fir = TargetFirDesign::disabled(options.target_fir.band_hz);
    let mut headroom = HeadroomStats::empty(options.program_peak_reference_dbfs);
    for _round in 0..2 {
        let leveled =
            level_rule_2a_peak_over(context, input, &mut channel, modal_band, &mut warnings);
        let round_adjustments =
            adjust_rule_2a(context, input, &mut channel, modal_band, &mut warnings);
        let (new_fir, new_headroom, reduced) =
            enforce_headroom_and_design(context, input, &mut channel, options, &mut warnings)?;
        fir = new_fir;
        headroom = new_headroom;
        adjustments += leveled + round_adjustments;
        if leveled == 0 && round_adjustments == 0 && !reduced {
            break;
        }
    }
    // 目标驱动 2a 恢复：只在标准拟合 rule_2a FAIL 时尝试；接纳条件=同一门代码下
    // 两 clause 真过；无解时保留原 FAIL 并把 before/after 与 unresolved 原因写入证据。
    let mut recovery: Option<TargetAwareRecoveryReport> = None;
    if let Some(band) = modal_band {
        let (pass, _, _) = aggregate_design_rule_2a(context, input, &channel, band)?;
        if !pass {
            let (recovered, mut report) =
                try_target_aware_recovery(context, input, &channel, band, options)?;
            if let Some(mut candidate) = recovered {
                let (candidate_fir, candidate_headroom, _candidate_reduced) =
                    enforce_headroom_and_design(
                        context,
                        input,
                        &mut candidate,
                        options,
                        &mut warnings,
                    )?;
                let (after_pass, after_peak, after_worsen) =
                    aggregate_design_rule_2a(context, input, &candidate, band)?;
                if after_pass {
                    report.applied = true;
                    report.after_peak_over_db = after_peak;
                    report.after_worst_worsening_db = after_worsen;
                    report.after_rms_after_db =
                        peq_modal_rms_after_db(context, input, &candidate, band);
                    if let Some(added) = report.added_bands.first() {
                        warnings.push(format!(
                            "通道 {} 目标驱动 2a 恢复已采纳：新增 {:.2} Hz/Q={:.2}/gain={:.2} dB（peak {:.2}->{:.2} / worsening {:.2}->{:.2}；未改动任何门限）",
                            input.name,
                            added.freq_hz,
                            added.q,
                            added.gain_db,
                            report.before_peak_over_db,
                            report.after_peak_over_db,
                            report.before_worst_worsening_db,
                            report.after_worst_worsening_db
                        ));
                    }
                    channel = candidate;
                    fir = candidate_fir;
                    headroom = candidate_headroom;
                } else {
                    report.applied = false;
                    report.added_bands.clear();
                    report.after_peak_over_db = after_peak;
                    report.after_worst_worsening_db = after_worsen;
                    report.note = format!(
                        "unresolved: enforce_headroom_and_design 后 2a 再次未过（peak {:.2} / worsening {:.2}），保持原 FAIL；未放宽任何门限",
                        after_peak, after_worsen
                    );
                }
            }
            recovery = Some(report);
        }
    }
    channel.bands.sort_by(|a, b| {
        a.freq_hz
            .partial_cmp(&b.freq_hz)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if fir.enabled && fir.max_deviation_db > TARGET_FIR_DEVIATION_WARN_DB {
        warnings.push(format!(
            "通道 {} 目标 FIR 频带内偏差 {:.2} dB > {:.1} dB（taps={}，可提 taps 或放宽目标）",
            input.name, fir.max_deviation_db, TARGET_FIR_DEVIATION_WARN_DB, fir.taps
        ));
    }

    let rule_aggregate = evaluate_rules(context, input, &channel, modal_band, &mut warnings)?;
    let peq_response = bands_response_db(&channel.bands, &context.grid, context.fs_f);
    let fir_response = if fir.enabled && !fir.coeffs.is_empty() {
        fir_magnitude_db_grid(&fir.coeffs, context.fs_f, &context.grid)
    } else {
        vec![0.0; context.grid.len()]
    };
    let mut p_response = Vec::with_capacity(context.grid.len());
    for index in 0..context.grid.len() {
        p_response.push(peq_response[index] + fir_response[index] + headroom.trim_db);
    }
    let effective_modal_band = modal_band.unwrap_or([context.grid[0], context.grid[0]]);
    let modal_residual = residual_stats(
        context,
        &input.modal_avg_db,
        effective_modal_band,
        &peq_response,
    );
    let modal_residual_after_p = residual_stats(
        context,
        &input.modal_avg_db,
        effective_modal_band,
        &p_response,
    );
    let wide_band = wide_band_for(input, options, &context.grid);
    let wide_residual_after_p =
        wide_band.map(|band| residual_stats(context, &input.wide_avg_db, band, &p_response));
    let g_curve = modal_band
        .map(|band| g_residual_curve(context, input, &channel, &fir, headroom.trim_db, band))
        .unwrap_or_default();
    let per_position = modal_position_residuals(
        context,
        input,
        &peq_response,
        &p_response,
        effective_modal_band,
        &rule_aggregate,
    );
    let unfilled_band = wide_band
        .map(|band| [band[0].max(100.0), band[1].min(10_000.0)])
        .filter(|band| band[1] > band[0] * 1.01)
        .unwrap_or([100.0, 1_000.0]);
    let unfilled_dips = find_unfilled_dips(context, &input.wide_avg_db, &p_response, unfilled_band);

    // boost / D7 统计
    let mut single_max = 0.0_f64;
    let mut positive_sum = 0.0_f64;
    let mut modal_count = 0usize;
    let mut shelf_count = 0usize;
    let mut bands_report = Vec::with_capacity(channel.bands.len());
    for band in &channel.bands {
        if band.gain_db > 0.0 {
            single_max = single_max.max(band.gain_db);
            positive_sum += band.gain_db;
        }
        let is_modal = band.band_type == BandType::Peaking
            && band.freq_hz >= options.modal_band_hz[0]
            && band.freq_hz <= options.modal_band_hz[1];
        if is_modal {
            modal_count += 1;
        }
        if band.band_type.is_shelf() {
            shelf_count += 1;
        }
        bands_report.push(BandReport {
            band_type: band.band_type.as_str().to_string(),
            freq_hz: band.freq_hz,
            q: band.q,
            gain_db: band.gain_db,
            b: band.b,
            a: band.a,
            modal: is_modal,
        });
    }
    let fir_positive = fir.max_positive_db.max(0.0);
    let applied_boost = positive_sum + fir_positive;
    let mut boost_limits_pass = true;
    if single_max > D7_SINGLE_BOOST_CAP_DB + 1e-9 {
        boost_limits_pass = false;
        warnings.push(format!(
            "通道 {} D7 违规：单段 boost {:.2} dB > +{D7_SINGLE_BOOST_CAP_DB} dB",
            input.name, single_max
        ));
    }
    if positive_sum > D7_POSITIVE_SUM_WARN_DB + 1e-9 {
        warnings.push(format!(
            "通道 {} D7 warning：每通道正向总和 {:.2} dB > {D7_POSITIVE_SUM_WARN_DB} dB",
            input.name, positive_sum
        ));
    }
    if positive_sum > D7_POSITIVE_SUM_REJECT_DB + 1e-9 {
        boost_limits_pass = false;
        warnings.push(format!(
            "通道 {} D7 拒绝：每通道正向总和 {:.2} dB > {D7_POSITIVE_SUM_REJECT_DB} dB",
            input.name, positive_sum
        ));
    }
    let band_count = bands_report.len();
    let wide_count = band_count.saturating_sub(modal_count);
    let coeff_sha256 = fir.coeff_sha256();
    let report = ChannelFitReport {
        name: input.name.clone(),
        index: input.index,
        work_band_hz: input.work_band_hz,
        band_count,
        modal_band_count: modal_count,
        wide_band_count: wide_count,
        shelf_count,
        bands: bands_report,
        single_filter_max_boost_db: single_max,
        filter_positive_sum_db: positive_sum,
        target_fir_positive_max_db: fir_positive,
        applied_boost_db: applied_boost,
        boost_reduced_db: headroom.boost_reduced_db,
        boost_cap_db: options.effective_boost_cap_db(),
        boost_limits_pass,
        program_peak_reference_dbfs: headroom.program_peak_reference_dbfs,
        predicted_peak_before_trim_dbfs: headroom.predicted_peak_before_trim_dbfs,
        trim_db: headroom.trim_db,
        predicted_peak_dbfs: headroom.predicted_peak_dbfs,
        headroom_db: headroom.headroom_db,
        headroom_pass: headroom.pass,
        target_fir_taps: fir.taps,
        target_fir_band_hz: fir.band_hz,
        target_fir_group_delay_samples: fir.group_delay_samples,
        target_fir_group_delay_ms: fir.group_delay_samples * 1000.0 / context.fs_f,
        target_fir_max_deviation_db: fir.max_deviation_db,
        target_fir_max_positive_db: fir_positive,
        target_fir_coeff_sha256: coeff_sha256,
        modal_residual,
        modal_residual_after_p,
        wide_residual_after_p,
        g_residual_curve: g_curve,
        modal_residual_per_position: per_position,
        rule_2a_pass: rule_aggregate.rule_2a_pass,
        rule_2b_pass: rule_aggregate.rule_2b_pass,
        rule_2a_peak_over_target_db: finite_or(rule_aggregate.peak_over_target_db, 0.0),
        rule_2a_worst_worsening_db: finite_or(rule_aggregate.worst_worsening_db, 0.0),
        rule_2b_worst_point_worsening_db: finite_or(rule_aggregate.worst_point_worsening_db, 0.0),
        rule_2b_one_third_octave_ripple_db: rule_aggregate.one_third_octave_ripple_db,
        rule_2b_new_narrow_notch_db: rule_aggregate.new_narrow_notch_db,
        rule_2b_raw_max_deviation_db_1_24_oct: rule_aggregate.raw_max_deviation_db_1_24_oct,
        holdout_rule_2a_pass: rule_aggregate.holdout_rule_2a_pass,
        holdout_rule_2b_pass: rule_aggregate.holdout_rule_2b_pass,
        holdout_rule_2a_peak_over_target_db: finite_or(
            rule_aggregate.holdout_peak_over_target_db,
            0.0,
        ),
        holdout_rule_2a_worst_worsening_db: finite_or(
            rule_aggregate.holdout_worst_worsening_db,
            0.0,
        ),
        holdout_rule_2b_worst_point_worsening_db: finite_or(
            rule_aggregate.holdout_worst_point_worsening_db,
            0.0,
        ),
        holdout_rule_2b_one_third_octave_ripple_db: rule_aggregate
            .holdout_one_third_octave_ripple_db,
        holdout_rule_2b_new_narrow_notch_db: rule_aggregate.holdout_new_narrow_notch_db,
        holdout_rule_2b_raw_max_deviation_db_1_24_oct: rule_aggregate
            .holdout_raw_max_deviation_db_1_24_oct,
        unfilled_dips,
        warnings,
    };
    let _ = adjustments;
    let _ = bands_fit.modal_count;
    let _ = bands_fit.wide_count;
    Ok(ChannelFitOutcome {
        report,
        channel,
        fir,
        recovery,
    })
}

fn finite_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

// ---------------------------------------------------------------------------
// 报告 / 系数装配
// ---------------------------------------------------------------------------

fn p_digest(outcomes: &[ChannelFitOutcome]) -> String {
    let mut canonical = String::with_capacity(outcomes.len() * 2048);
    // P 内容摘要：只绑定 P 本身（段表/trim/FIR），不绑定输入 hash，
    // 这样 holdout/validation 曲线变化不影响“基线 P 是否相同”的判定；
    // stale 由报告里的 plan_sha256 / target.sha256 / measurement.sha256 字段单独表达。
    canonical.push_str("khaen-peq-fit-p/v1\n");
    for outcome in outcomes {
        let channel = &outcome.channel;
        canonical.push_str(&format!("channel={}/{}\n", channel.name, channel.index));
        for band in &channel.bands {
            canonical.push_str(&format!(
                "band={}:{:.17e}:{:.17e}:{:.17e}\n",
                band.band_type.as_str(),
                band.freq_hz,
                band.q,
                band.gain_db
            ));
        }
        canonical.push_str(&format!("trim={:.17e}\n", outcome.report.trim_db));
        canonical.push_str(&format!("fir={}\n", outcome.fir.coeff_sha256()));
    }
    sha256_hex(canonical.as_bytes())
}

/// T64 生产端限额校验（模态 peaking <=10 按频率归类；shelf 每类 <=1；总段 <=32；
/// 模态只切；boost 帽 D7）。T41 PeqDesign.validate 的 producer 口径只适用于
/// modal_only（其 modal_peaking_count 不按频率分类），因此 T64 的 modal_plus_wide
/// 产物在这里按 spec 5.2 自校验，并把警告写进 report/design.warnings。
fn validate_design_limits(
    request: &PeqFitRequest<'_>,
    outcomes: &[ChannelFitOutcome],
) -> Result<Vec<String>, PeqFitError> {
    let mut warnings = Vec::new();
    let fs_f = request.plan.sample_rate_hz as f64;
    let modal_lo = request.options.modal_band_hz[0];
    let modal_hi = request.options.modal_band_hz[1];
    let boost_cap = request.options.effective_boost_cap_db();
    for outcome in outcomes {
        let name = &outcome.channel.name;
        let bands = &outcome.channel.bands;
        if bands.len() > 32 {
            return fit_err(
                E_FIT_DESIGN,
                format!("通道 {name} 段数 {} > CONTRACT 硬上限 32", bands.len()),
            );
        }
        for band in bands {
            band.validate_contract(fs_f).map_err(peq_error)?;
        }
        let modal_peakings = bands
            .iter()
            .filter(|band| {
                band.band_type == BandType::Peaking
                    && band.freq_hz >= modal_lo
                    && band.freq_hz <= modal_hi
            })
            .count();
        if modal_peakings > MAX_MODAL_PEAKINGS {
            return fit_err(
                E_FIT_DESIGN,
                format!("通道 {name} 模态 peaking 段 {modal_peakings} > {MAX_MODAL_PEAKINGS}"),
            );
        }
        for band_type in [BandType::LowShelf, BandType::HighShelf] {
            let count = bands
                .iter()
                .filter(|band| band.band_type == band_type)
                .count();
            if count > 1 {
                return fit_err(
                    E_FIT_DESIGN,
                    format!(
                        "通道 {name} {} 段 {count} > 1（producer 约定）",
                        band_type.as_str()
                    ),
                );
            }
        }
        let mut positive_sum = 0.0_f64;
        for band in bands {
            let is_modal = band.band_type == BandType::Peaking
                && band.freq_hz >= modal_lo
                && band.freq_hz <= modal_hi;
            if is_modal && band.gain_db > 0.0 {
                return fit_err(
                    E_FIT_DESIGN,
                    format!(
                        "通道 {name} 模态段 {:.1} Hz gain={:.3} > 0（只切）",
                        band.freq_hz, band.gain_db
                    ),
                );
            }
            if band.gain_db > 0.0 {
                if band.gain_db > D7_SINGLE_BOOST_CAP_DB + 1e-6 {
                    return fit_err(
                        E_FIT_DESIGN,
                        format!(
                            "通道 {name} 单段 boost {:.3} dB > +{D7_SINGLE_BOOST_CAP_DB}",
                            band.gain_db
                        ),
                    );
                }
                positive_sum += band.gain_db;
            }
        }
        if positive_sum > D7_POSITIVE_SUM_REJECT_DB + 1e-6 {
            return fit_err(
                E_FIT_DESIGN,
                format!("通道 {name} 正向总和 {positive_sum:.3} dB > {D7_POSITIVE_SUM_REJECT_DB}"),
            );
        }
        if positive_sum > D7_POSITIVE_SUM_WARN_DB + 1e-6 {
            warnings.push(format!(
                "通道 {name} 正向总和 {positive_sum:.3} dB > {D7_POSITIVE_SUM_WARN_DB}（D7 warning）"
            ));
        }
        if boost_cap <= 0.0 && positive_sum > 1e-6 {
            return fit_err(
                E_FIT_DESIGN,
                format!("通道 {name} allow_boost=false 但有正增益 {positive_sum:.3} dB"),
            );
        }
    }
    Ok(warnings)
}

fn assemble_output(
    request: &PeqFitRequest<'_>,
    context: &FitContext,
    outcomes: &[ChannelFitOutcome],
    warnings: &mut Vec<String>,
) -> Result<PeqFitOutput, PeqFitError> {
    let fs = context.fs;
    let has_wide = outcomes.iter().any(|outcome| {
        outcome.channel.bands.iter().any(|band| {
            band.band_type.is_shelf() || {
                let is_modal = band.band_type == BandType::Peaking
                    && band.freq_hz >= request.options.modal_band_hz[0]
                    && band.freq_hz <= request.options.modal_band_hz[1];
                !is_modal
            }
        })
    });
    let mode = if has_wide {
        DesignMode::ModalPlusWide
    } else {
        DesignMode::ModalOnly
    };
    let mut design = PeqDesign::new(fs, mode).map_err(peq_error)?;
    design.tool_name = FIT_TOOL_NAME.to_string();
    design.tool_version = FIT_TOOL_VERSION.to_string();
    design.created_unix = request.created_unix;
    design.source = PeqSource {
        measurement_sha256: Some(context.measurement_sha256.clone()),
        curve_id: Some(context.target.id.clone()),
        target_sha256: Some(context.target.sha256.clone()),
        calibration_sha256: None,
    };
    design.allow_boost = request.options.effective_boost_cap_db() > 0.0;
    design.band_hz = request.options.modal_band_hz;
    for outcome in outcomes {
        design
            .add_channel(outcome.channel.clone())
            .map_err(peq_error)?;
    }
    let design_warnings = validate_design_limits(request, outcomes)?;
    design.warnings = design_warnings.clone();
    warnings.extend(design_warnings);

    let quality = match request.design_quality {
        Some(report) => FitQualityReport {
            required: request.options.require_design_quality,
            present: true,
            scope: report.scope.as_str().to_string(),
            plan_sha256: report.plan_sha256.clone(),
            continue_allowed: report.continue_allowed(),
            points_usable: report.points_usable,
            points_total: report.points_total,
        },
        None => FitQualityReport {
            required: request.options.require_design_quality,
            present: false,
            scope: "none".to_string(),
            plan_sha256: String::new(),
            continue_allowed: false,
            points_usable: 0,
            points_total: 0,
        },
    };
    let options_report = FitOptionsReport {
        modal_band_hz: request.options.modal_band_hz,
        wide_band_hz: request.options.wide_band_hz,
        reference_freq_hz: request.options.reference_freq_hz,
        boost_cap_db: request.options.effective_boost_cap_db(),
        cut_budget_db: request.options.cut_budget_db,
        wide_max_stages: request.options.wide_max_stages,
        max_total_stages: request.options.max_total_stages,
        low_band_trusted: request.options.low_band_trusted,
        program_peak_reference_dbfs: request.options.program_peak_reference_dbfs,
        trim_range_db: request.options.trim_range_db,
        target_fir_enabled: request.options.target_fir.enabled,
        target_fir_taps: request.options.target_fir.taps,
        target_fir_band_hz: request.options.target_fir.band_hz,
        target_fir_allow_positive_db: request.options.effective_fir_positive_cap_db(),
        require_design_quality: request.options.require_design_quality,
        require_t6_minimums: request.options.require_t6_minimums,
    };
    let delay = FitDelayReport {
        phase_basis: context.phase_basis.clone(),
        common_d0_samples: context.common_d0_samples,
        common_d0_ms: context.common_d0_samples as f64 * 1000.0 / context.fs_f,
    };
    let channels_report: Vec<ChannelFitReport> = outcomes
        .iter()
        .map(|outcome| outcome.report.clone())
        .collect();
    let checks = FitChecks {
        all_rule_2a_pass: channels_report.iter().all(|channel| channel.rule_2a_pass),
        all_rule_2b_pass: channels_report.iter().all(|channel| channel.rule_2b_pass),
        all_headroom_pass: channels_report.iter().all(|channel| channel.headroom_pass),
        all_boost_limits_pass: channels_report
            .iter()
            .all(|channel| channel.boost_limits_pass),
    };
    let report = PeqFitReport {
        tool_name: FIT_TOOL_NAME.to_string(),
        tool_version: FIT_TOOL_VERSION.to_string(),
        created_unix: request.created_unix,
        fs,
        session_id: context_target_session(request),
        layout: request.plan.layout.clone(),
        plan_sha256: context.plan_sha256.clone(),
        measurement_sha256: context.measurement_sha256.clone(),
        target: FitTargetReport {
            id: context.target.id.clone(),
            sha256: context.target.sha256.clone(),
            normalization: context.target.normalization.clone(),
            reference_gain_db: context.target.reference_gain_db,
            anchor_db: context.target.anchor_db,
            reference_freq_hz: request.options.reference_freq_hz,
            point_count: context.target.point_count,
        },
        quality,
        options: options_report,
        delay,
        channels: channels_report,
        p_sha256: p_digest(outcomes),
        checks,
        warnings: warnings.clone(),
    };
    let target_firs: Vec<ChannelTargetFir> = outcomes
        .iter()
        .map(|outcome| ChannelTargetFir {
            name: outcome.report.name.clone(),
            index: outcome.report.index,
            taps: outcome.fir.taps,
            band_hz: outcome.fir.band_hz,
            group_delay_samples: outcome.fir.group_delay_samples,
            coeffs: outcome.fir.coeffs.clone(),
            max_deviation_db: outcome.fir.max_deviation_db,
            max_positive_db: outcome.fir.max_positive_db,
        })
        .collect();
    let recovery: Vec<TargetAwareRecoveryReport> = outcomes
        .iter()
        .filter_map(|outcome| outcome.recovery.clone())
        .collect();
    Ok(PeqFitOutput {
        report,
        peq: design,
        target_firs,
        recovery,
    })
}

fn context_target_session(request: &PeqFitRequest<'_>) -> String {
    request.plan.session_id.clone()
}

/// 皇帝位这一轮从 `fit_peq` 留下的线性相位目标 FIR。模态段和宽带段只计数后丢掉。
#[derive(Clone, Debug, PartialEq)]
pub struct TargetFirOnly {
    pub coeffs: Vec<f64>,
    pub discarded_sections: usize,
}

/// 用全频脉冲响应的幅度调用 `fit_peq`，只返回目标 FIR。
/// 响应短于 64 点时按 0 dB 平坦曲线拟合，不另写一套代价函数。
pub fn fit_target_fir_only(ir_48k: &[f64], target_name: &str) -> Result<TargetFirOnly, String> {
    let points = if ir_48k.len() >= 64 {
        curve_from_ir(ir_48k)
    } else {
        log_frequency_grid(20.0, 20_000.0, 1.0)
            .into_iter()
            .map(|freq| CurvePoint::new(freq, 0.0))
            .collect()
    };
    fit_target_fir_from_points(&points, target_name)
}

/// 同一条 `fit_peq` 路径。调用方给出已经是 Hz/dB 的曲线。
pub fn fit_target_fir_from_points(
    points: &[CurvePoint],
    target_name: &str,
) -> Result<TargetFirOnly, String> {
    let builtin = match target_name {
        "flat" => BuiltinTarget::Flat,
        "tilt10" => BuiltinTarget::Tilt10,
        _ => return Err("target 只能是 flat 或 tilt10".to_string()),
    };
    let target = builtin_target(builtin, &BuiltinOptions::new(48_000.0))
        .map_err(|error| error.to_string())?;
    let plan = emperor_fit_plan()?;
    let curves = vec![
        PositionCurve::new("D1", "FL", points.to_vec()),
        PositionCurve::new("H1", "FL", points.to_vec()),
    ];
    let mut options = PeqFitOptions::default();
    options.require_design_quality = false;
    options.require_t6_minimums = false;
    options.wide_max_stages = 2;
    options.max_total_stages = 4;
    options.target_fir.taps = 65;
    options.target_fir.enabled = true;
    let request = PeqFitRequest::new(&plan, &target, &curves).with_options(options);
    let output = fit_peq(&request).map_err(|error| error.to_string())?;
    let discarded_sections = output
        .peq
        .channels
        .iter()
        .map(|channel| channel.bands.len())
        .sum();
    let coeffs = output
        .target_firs
        .first()
        .map(|fir| fir.coeffs.clone())
        .ok_or_else(|| "fit_peq 没有目标 FIR".to_string())?;
    Ok(TargetFirOnly {
        coeffs,
        discarded_sections,
    })
}

fn emperor_fit_plan() -> Result<MultipointPlan, String> {
    let positions = vec![
        MultipointPosition::design("D1", 1.0)
            .map_err(|error| error.to_string())?
            .with_mic_orientation("90deg"),
        MultipointPosition::validation("H1", 1.0)
            .map_err(|error| error.to_string())?
            .with_mic_orientation("90deg"),
    ];
    MultipointPlan::new(
        "emperor-seat",
        "4.1.2",
        48_000,
        vec!["FL".to_string()],
        CommonD0::zero(),
        "aligned_segment",
        positions,
    )
    .map_err(|error| error.to_string())
}

fn curve_from_ir(ir: &[f64]) -> Vec<CurvePoint> {
    let nfft = next_pow2_at_least(ir.len().max(256));
    let spec = rfft(ir, nfft);
    let grid = log_frequency_grid(20.0, 20_000.0, 1.0 / 3.0);
    let mut points: Vec<CurvePoint> = grid
        .into_iter()
        .map(|freq| {
            let bin = ((freq / 48_000.0) * nfft as f64).round() as usize;
            let bin = bin.clamp(1, spec.len().saturating_sub(1));
            let mag = spec[bin].abs().max(1e-12);
            CurvePoint::new(freq, 20.0 * mag.log10())
        })
        .collect();
    let anchor = points
        .iter()
        .min_by(|left, right| {
            (left.freq_hz - 1_000.0)
                .abs()
                .partial_cmp(&(right.freq_hz - 1_000.0).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|point| point.mag_db)
        .unwrap_or(0.0);
    for point in &mut points {
        point.mag_db -= anchor;
    }
    points
}

// ---------------------------------------------------------------------------
// 私有测试（tests/T64 通过 include! 进入本模块的 #[cfg(test)] 子模块）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod emperor_target_fir {
    use super::*;

    #[test]
    fn fit_peq_sections_are_dropped() {
        let grid = log_frequency_grid(20.0, 20_000.0, 1.0 / 3.0);
        let points: Vec<CurvePoint> = grid
            .into_iter()
            .map(|freq| {
                let distance = (freq / 80.0).log2();
                let bump = 12.0 * (-(distance * distance) / (2.0 * 0.08 * 0.08)).exp();
                CurvePoint::new(freq, bump)
            })
            .collect();
        let got = fit_target_fir_from_points(&points, "flat").expect("fit");
        assert_eq!(got.coeffs.len(), 65);
        assert!(
            got.discarded_sections > 0,
            "peaked curve should emit sections that this round drops"
        );
        assert!(got.coeffs.iter().all(|coeff| coeff.is_finite()));
    }
}

#[cfg(test)]
mod peq_fit_tests {
    include!("../tests/T64/peq_fit_cases.rs");
}
