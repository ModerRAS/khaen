//! [NONRT] 模态正则（WBS T52）—— design 内 CV / 固定 3-4 白名单 / lambda 扫描与 L-curve /
//! gain headroom 与先验 / P>=M 适用边界 / holdout 不调参。本文件写域。
//!
//! ## 权威契约（只读输入，禁止在本文件改写）
//!
//! - scratch/lead/planner/TSPI-RT-STACK-WBS.md T52：design 内 CV、固定 3-4 白名单、lambda/gain
//!   headroom 与先验、P>=M 适用边界标记、holdout 不调参；deps T51（solve）完成。
//! - scratch/lead/planner/DSP-HOST-DECISION.md §2.5：先冻结 P_post 基线，再以 H_eff 做真实 5x5
//!   低频减尾/抑模态；**正则、design 内 CV、固定白名单/带限、因果 D/Delta、增益/相干和/时域峰值
//!   与保护共同约束**；不是全带串扰反演，也不承诺消除全部驻波。
//! - rt-stack/spec/mimo-t6.md §4.8 PR-3 / §5.1 / §5.3：每修正路径增益帽（默认软帽 <= -6 dB，
//!   允许显式放宽至 <= +6 dB；由系数文件实算的每路径最大增益 <= 声明帽 + 2 dB）；holdout 是
//!   **从未参与调参**的测点集合，禁止用于 lambda/CV、白名单/稀疏选择、Delta/D 选择、target/curve
//!   设计、gain/limiter 调整、analysis 参数选择——任何一项违反即 FAIL-07。
//! - crate::solve（T51）：SolveOptions 暴露 ridge（lambda）、path_mask（M*M 自由路径白名单）、
//!   frequency_weights（逐频权重）原语；SolveProblem 字段公开（positions/weights/target），可在
//!   design 内做留出/CV 子集重解；ridge>0 时报告 lambda 有偏与 W_SOLVE_RIDGE_USED。
//! - crate::target（T50）：TargetMatrix/EffectivePlant/ModalMatrix/validate_solution；T50 §9 接口
//!   约定：只用 design positions/目标矩阵，不按 position 单独最佳对齐（T6 FC-04）。
//! - rt-stack/README.md（T06）：leaf 只写本文件 + tests/T52 + evidence/T52；不写 Cargo.toml /
//!   lib.rs / public_registration.rs / CLI；不新增第三方依赖。
//!
//! ## 本文件做什么（T52 DoD）
//!
//! 1. **design 内 CV（不是 holdout）**：把 **design positions** 分成 k 折（默认留一法 LOO），
//!    每折在 P-1 个 design positions 上重解、在留出的 design position 上计算预测相对残差 dB；
//!    CV 只使用 design positions，holdout/validation 永不进入（holdout_used_for_tuning=false，
//!    且 forbidden_tuning_labels 与 design label 冲突直接拒收）。
//! 2. **固定 3-4 白名单**：WhitelistKind::{DiagOnly, DiagAdjacent, DiagSub1, Full} 四个**结构
//!    固定**候选（对角 / 对角+相邻 / 对角+SUB1 支撑 / 全 5x5）；掩码只由 roles 与 dense 索引决定，
//!    与测量数据无关（不按残差挑路径，禁止数据驱动稀疏化）。
//! 3. **lambda 扫描 / L-curve**：固定 lambda 网格按 plant 尺度归一（lambda_used=lambda*scale，
//!    scale=mean|H|^2），逐点报告 design 残差范数、解范数、残差 dB、curvature 与 L-curve 拐点；
//!    L-curve 只作诊断，最终选择由 CV 决定（CV 不可用时才回退 L-curve 拐点，并如实标记）。
//! 4. **gain headroom 与先验**：解后按系数实算的 G 复响应评估每路径（含对角）改善带内最大增益，
//!    与声明帽（PR-3：修正路径默认 -6 dB、容差 +2 dB）比较；候选先按 CV 排序，再按 headroom
//!    可行性回退（有界尝试次数），不可行则返回 best-effort 解 + 明确失败标记（不静默放行）。
//!    先验 PriorKind::{ZeroCorrection, FrozenCoefficients}：零修正先验即 Tikhonov 收缩到
//!    identity 基线（g=0）；冻结先验把上一次接受解作为 g_prior，通过 target' = d - H*g_prior
//!    的等价变换在 T51 solver 上实现（lambda 越大越靠近先验），并报告到先验的距离。
//! 5. **P>=M 适用边界标记**：ApplicabilityBoundary 显式标记 applicable（P>M）、
//!    boundary_p_equals_m（P==M，全设计可辨识边界但 CV 折 P-1<M 欠定）、
//!    not_applicable_p_below_m（P<M，欠定、最小范数解非唯一、不得交付）；每候选另报
//!    掩码后的频率域/FIR 秩必要条件（necessary-only）。
//! 6. **诚实边界**：applied=false、post_apply_measurement=not_requested、
//!    engine_compatibility=not_validated、runtime_validation=not_run；
//!    deliverable 只在适用边界内、解可用且 headroom 通过时为 true；claims 明确 predicted-only、
//!    design 内 CV、holdout 未调参、不保证消除全部驻波；不产任何"实测改善"结论。
//!
//! ## 本文件不做什么（明确非目标）
//!
//! - 不测量/不采音/不打开设备/不写文件/不启线程（纯计算；静态守卫测试保证）；
//! - 不做 holdout 判定（T53）、不产 WAV/manifest（T54）、不编译/热切（T66）、不做 PEQ/delay
//!   拟合（T64/T65）、不启 daemon job（T60）；不改变 T51 solver 语义（只调用其公开入口）；
//! - 不做数据驱动的路径选择/频带自动搜索（白名单与 lambda 网格固定枚举）；
//! - 不声明"已应用/已实测/消除全部驻波/通用改善 X dB"。
//!
//! ## 算法细节（冻结语义）
//!
//! - **CV 有效性**：每折训练集（P - held_out）必须让 T51 判为可辨识（非 Underdetermined），
//!   否则该候选 CV 无效（不把欠定折的分数当泛化证据）。因此 P>M 时默认 LOO 折训练集=P-1>=M；
//!   P==M 时折训练集 P-1<M 必然欠定 → CV 无效 → 选择回退 L-curve（并标记 boundary）。
//! - **CV 评分**：每折在留出 design position 上按改善带 63-250 Hz 计算每 (position,input) 的
//!   最大相对残差 dB（近零目标点跳过，不制造 100+ dB 假残差）；fold mean/worst 上报；
//!   候选 cv_mean=各折 mean 的平均，cv_worst=各折 worst 的最大。
//! - **选择策略（固定、可复现）**：先按 cv_mean 升序排序；在 selection_tolerance_db 容差带内
//!   依次偏好：更少自由路径（更强结构先验）→ 更大 lambda_used（更强正则）→ 白名单枚举序 →
//!   lambda 网格序。容差带选中的候选**就是 headroom 回退的首个尝试项**（不再只在报告里标
//!   tie_break 而让严格 cv_mean 最小候选先通过 headroom）；仅当它通过 headroom 且最终
//!   selected_whitelist/selected_lambda 确为该候选时才写 tie_break_used=true。headroom 回退把
//!   选择改掉时 tie_break_used=false，并由 fallback_used/headroom_fallback_attempts 如实表达。
//! - **headroom 回退**：初始候选（CV 有效：容差带选中项；CV 无效：L-curve 扫描首点）不满足
//!   headroom 时，按候选顺序（CV 有效：其余候选 cv_mean 排序；CV 无效：L-curve 残差排序）有界
//!   尝试 MAX_HEADROOM_FALLBACK_ATTEMPTS 个；全不通过则返回 worst_excess 最小的 best-effort 解
//!   并标 deliverable=false（不静默放行）。
//! - **lambda_scan 自洽**：lambda_scan.scan_whitelist 恒等于最终 selection.selected_whitelist；
//!   headroom 回退把白名单改掉时按最终白名单重算扫描（L-curve 拐点仍只作诊断）。
//! - **可用可辨识域（如实边界；不放宽 T51 契约）**：T51 判 underdetermined 的 FIR 必要条件为
//!   paths_into_input_i * taps <= min(2*band_points*positions, unknowns_max)；design 内 CV 的每折
//!   只用 P-1 个训练 position，故保守可用域为
//!   `taps <= 2*(P-1)*band_points / free_paths`（free_paths 取候选总自由路径数是每 input 入路径
//!   数的上界，故该式是充分条件）。默认 taps=256、T6 正式 P>=6、1/6 oct 拟合带 25 点（1/24 oct
//!   94 点）时，full/diag_sub1 的折训练集欠定 → 候选 cv_valid=false；若无任何 CV 有效候选则回退
//!   L-curve，且 P=6 时全矩阵覆盖未必可辨识（2*P*band_points < free_paths*taps）→ 最终状态
//!   Underdetermined、deliverable=false（best-effort 返回），不得把 ridge 欠定解当可交付。
//! - **确定性**：无时间戳/无随机数/固定迭代顺序；RegularizedSolution::hash 覆盖 problem hash、
//!   先验 hash、base solve hash 与最终系数；同输入同输出。
//! - **近零目标**：改善带内 |target|<NEAR_ZERO_MAGNITUDE 的点不参与相对残差统计（与 T51/T50
//!   一致），只计数。
//!
//! ## 关键不变量（测试与静态守卫覆盖）
//!
//! - 白名单固定 4 个且结构定义与数据无关；holdout/validation label 永不进入 CV/选择；
//! - P<M 标记 not_applicable 且 deliverable=false；P==M 标记 boundary 且 cv_valid=false；
//! - 先验通过等价 target 变换实现，lambda 增大时解向先验收缩（冻结先验可验证）；
//! - headroom 用系数实算的 G（含 deltaD 基线）评估，不用预测模型代替；违反必报；
//! - 无 I/O/线程/环境/序列化/不安全代码依赖；不写 kernel/plan ABI。

use crate::ingest::sha256_hex;
use crate::solve::{
    solve, SolveError, SolveMode, SolveOptions, SolvePosition, SolveProblem, SolveSolution,
    SolveStatus, MAX_SOLVE_RIDGE, MAX_SOLVE_TAPS,
};
use crate::target::{
    validate_solution, EffectivePlant, ModalMatrix, ModalMatrixCell, SolutionValidation,
    TargetMatrix, IMPROVEMENT_BAND_HZ, MAX_PARTICIPANTS, NEAR_ZERO_MAGNITUDE,
};
use crate::timebase::ComplexPoint;
use tspi_dsp_core::rt::MAX_LOW_BAND_PARTICIPANTS;

// ===========================================================================
// 常量与 schema
// ===========================================================================

/// 正则产物 schema（T52 设计侧/离线产物；不是实测或已应用）。
pub const REGULARIZE_SCHEMA: &str = "tspi-modal-regularize/v1";
/// 正则产物 hash domain。
pub const REGULARIZE_HASH_DOMAIN: &str = "tspi-modal-regularize/v1";
/// 先验 hash domain（与产物 hash 分离）。
pub const REGULARIZE_PRIOR_HASH_DOMAIN: &str = "tspi-modal-regularize-prior/v1";

/// 固定 lambda 扫描网格（按 plant 尺度归一后使用；0 = 纯最小二乘基线）。
pub const LAMBDA_GRID: [f64; 8] = [0.0, 1.0e-6, 1.0e-5, 1.0e-4, 1.0e-3, 1.0e-2, 1.0e-1, 1.0];
/// lambda 网格最大点数（固定枚举，不允许大规模数据驱动扫描）。
pub const MAX_LAMBDA_GRID_POINTS: usize = 16;
/// lambda 网格硬上限（= T51 MAX_SOLVE_RIDGE；实际 lambda_used 还须 <= 该值）。
pub const MAX_LAMBDA: f64 = MAX_SOLVE_RIDGE;
/// 默认 CV 折数（0 = leave-one-out；LOO 超过 MAX_CV_FOLDS 时按 MAX_CV_FOLDS 折截断）。
pub const DEFAULT_CV_FOLDS: usize = 0;
/// CV 折数硬上限（LOO 截断与显式 k 折共用）。
pub const MAX_CV_FOLDS: usize = 16;
/// 默认 CV 选择容差（dB）：容差带内按更简单/更强正则偏好。
pub const DEFAULT_SELECTION_TOLERANCE_DB: f64 = 0.25;
/// CV 选择容差硬上限（dB；不允许把"更好"糊成任意大容差）。
pub const MAX_SELECTION_TOLERANCE_DB: f64 = 6.0;

/// 默认修正路径增益帽（dB；mimo-t6 §4.8 PR-3 默认软帽 <= -6 dB）。
pub const DEFAULT_CORRECTION_GAIN_CAP_DB: f64 = -6.0;
/// 修正路径增益帽下限（dB；允许显式放宽的硬边界）。
pub const CORRECTION_GAIN_CAP_MIN_DB: f64 = -24.0;
/// 修正路径增益帽上限（dB；PR-3 允许显式放宽至 <= +6 dB）。
pub const CORRECTION_GAIN_CAP_MAX_DB: f64 = 6.0;
/// 默认对角路径最大增益帽（dB；相对 0 dB 基线，防过度提升）。
pub const DEFAULT_DIAGONAL_GAIN_CAP_DB: f64 = 6.0;
/// 对角增益帽下限（dB）。
pub const DIAGONAL_GAIN_CAP_MIN_DB: f64 = 0.0;
/// 对角增益帽上限（dB）。
pub const DIAGONAL_GAIN_CAP_MAX_DB: f64 = 24.0;
/// 默认 headroom 门容差（dB；PR-3：每路径最大增益 <= 声明帽 + 2 dB）。
pub const HEADROOM_TOLERANCE_DB: f64 = 2.0;
/// headroom 容差硬上限（dB）。
pub const MAX_HEADROOM_TOLERANCE_DB: f64 = 6.0;
/// headroom 不可行时最多再尝试的候选数（含初始候选共 8 次有界尝试）。
pub const MAX_HEADROOM_FALLBACK_ATTEMPTS: usize = 8;

/// lambda 尺度归一的下限（mean|H|^2 低于此值视为退化 plant，scale 取 1 并告警）。
pub const LAMBDA_SCALE_FLOOR: f64 = 1.0e-12;
/// 先验系数 sanity 上限（防非有限/离谱输入）。
pub const PRIOR_COEFF_SANITY: f64 = 1.0e6;
/// dB 比值下界（避免 log(0)）。
pub const REGULARIZE_DB_FLOOR: f64 = 1.0e-12;
/// 网格频率一致性相对容差。
pub const REGULARIZE_GRID_REL_TOLERANCE: f64 = 1.0e-9;
/// L-curve 曲率至少需要的正 lambda 点数。
pub const L_CURVE_MIN_POINTS: usize = 3;

/// 诚实 claim（不进任何实测结论）。
pub const REGULARIZE_CLAIM: &str = "design-internal CV over design positions only; fixed structural whitelists and fixed lambda scan; predicted-only, not applied, not measured; holdout/validation never used for tuning; P>=M applicability boundary marked; gain headroom checked against PR-3 caps; no guarantee that all standing waves are eliminated";

/// 适用边界字面值。
pub const APPLICABILITY_APPLICABLE: &str = "applicable";
/// 适用边界字面值（P==M：全设计可辨识边界，但 CV 折欠定）。
pub const APPLICABILITY_BOUNDARY: &str = "boundary_p_equals_m";
/// 适用边界字面值（P<M：欠定，不得交付）。
pub const APPLICABILITY_NOT_APPLICABLE: &str = "not_applicable_p_below_m";

/// 选择方法字面值（CV）。
pub const SELECTION_METHOD_CV: &str = "cv";
/// 选择方法字面值（CV 不可用时的 L-curve 回退）。
pub const SELECTION_METHOD_L_CURVE: &str = "l_curve_fallback";
/// 选择方法字面值（Preserve 基线，无修正可调）。
pub const SELECTION_METHOD_PRESERVE: &str = "preserve_baseline";

/// 应用边界说明（报告用，固定文本）。
pub const APPLICABILITY_NOTE: &str = "P>=M is the full-matrix identifiability boundary (necessary condition; T51/MIMO-COEFF-PLAN). Design-internal CV needs each fold training set to remain identifiable; at P==M a leave-one-out fold has P-1<M positions and is underdetermined, so CV is invalid and selection falls back to the L-curve (marked). P<M is not applicable and must not be delivered.";

/// L-curve 说明（报告用，固定文本）。
pub const L_CURVE_NOTE: &str = "L-curve (log10 design residual norm vs log10 solution norm) is a diagnostic only; the selected candidate comes from design-internal CV, and the L-curve corner is reported as a cross-check. The L-curve corner is used for lambda only when CV is invalid (marked as fallback).";

/// 候选适用性说明（报告用，固定文本）。
pub const CANDIDATE_APPLICABILITY_NOTE: &str = "necessary-only: masked per-frequency mixing rank bound min(P, free_paths) and FIR rank bound min(2*P*band_points, free_paths*taps); actual rank is verified by solve status and residual, not by this bound";

/// headroom 说明（报告用，固定文本）。
pub const HEADROOM_NOTE: &str = "PR-3: per-path maximum gain computed from the final G response (deltaD baseline + correction FIR) inside the 63-250 Hz improvement band; correction (off-diagonal) paths use the declared soft cap (default -6 dB) and diagonal paths use the diagonal cap; tolerance +2 dB. Violations are reported and drive bounded candidate fallback, never silently accepted.";

// ===========================================================================
// 错误
// ===========================================================================

/// 正则错误种类（稳定机器码见 RegularizeErrorKind::code）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RegularizeErrorKind {
    /// 选项非法（lambda 网格/CV 折数/容差/headroom 帽/base 选项被占用）。
    Options,
    /// 问题/产物结构非法（网格、角色、模态矩阵构造失败）。
    Problem,
    /// 白名单候选非法（空/重复/缺少所需角色/尺寸不匹配）。
    Whitelist,
    /// lambda 网格或尺度归一非法。
    Lambda,
    /// CV 折非法（折数、留出/训练集为空）。
    Fold,
    /// gain headroom 规格非法。
    Headroom,
    /// 先验非法（taps/roles/grid/系数长度/非有限/Preserve 下给冻结先验）。
    Prior,
    /// holdout/validation label 进入调参输入（必须拒收）。
    Holdout,
    /// 数值范围（非有限/越界）。
    Range,
    /// 缺失必需输入。
    Missing,
    /// 上游 T51 solver / T50 校验错误。
    Upstream,
}

impl RegularizeErrorKind {
    /// 全部错误种类（报告/负向对照用；顺序稳定）。
    pub const ALL: [RegularizeErrorKind; 11] = [
        RegularizeErrorKind::Options,
        RegularizeErrorKind::Problem,
        RegularizeErrorKind::Whitelist,
        RegularizeErrorKind::Lambda,
        RegularizeErrorKind::Fold,
        RegularizeErrorKind::Headroom,
        RegularizeErrorKind::Prior,
        RegularizeErrorKind::Holdout,
        RegularizeErrorKind::Range,
        RegularizeErrorKind::Missing,
        RegularizeErrorKind::Upstream,
    ];

    /// 稳定机器码（E_REG_*）。
    pub const fn code(self) -> &'static str {
        match self {
            RegularizeErrorKind::Options => "E_REG_OPTIONS",
            RegularizeErrorKind::Problem => "E_REG_PROBLEM",
            RegularizeErrorKind::Whitelist => "E_REG_WHITELIST",
            RegularizeErrorKind::Lambda => "E_REG_LAMBDA",
            RegularizeErrorKind::Fold => "E_REG_FOLD",
            RegularizeErrorKind::Headroom => "E_REG_HEADROOM",
            RegularizeErrorKind::Prior => "E_REG_PRIOR",
            RegularizeErrorKind::Holdout => "E_REG_HOLDOUT",
            RegularizeErrorKind::Range => "E_REG_RANGE",
            RegularizeErrorKind::Missing => "E_REG_MISSING",
            RegularizeErrorKind::Upstream => "E_REG_UPSTREAM",
        }
    }
}

/// 正则错误（kind + 字段路径 + 说明；Display 含机器码）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegularizeError {
    kind: RegularizeErrorKind,
    field: String,
    detail: String,
}

impl RegularizeError {
    /// 构造（field 为 a.b[c] 形式路径）。
    pub fn new(
        kind: RegularizeErrorKind,
        field: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            field: field.into(),
            detail: detail.into(),
        }
    }

    /// 错误种类。
    pub fn kind(&self) -> RegularizeErrorKind {
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

impl std::fmt::Display for RegularizeError {
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

impl std::error::Error for RegularizeError {}

fn err(
    kind: RegularizeErrorKind,
    field: impl Into<String>,
    detail: impl Into<String>,
) -> RegularizeError {
    RegularizeError::new(kind, field, detail)
}

fn map_solve_error(error: SolveError) -> RegularizeError {
    err(
        RegularizeErrorKind::Upstream,
        format!("solve.{}", error.field()),
        format!("{}: {}", error.code(), error.detail()),
    )
}

fn map_target_error(error: crate::target::TargetError) -> RegularizeError {
    err(
        RegularizeErrorKind::Problem,
        "regularize.solution.modal",
        format!("{error}"),
    )
}

// ===========================================================================
// 非致命诊断
// ===========================================================================

/// 非致命诊断（不改变解的有效性；报告必须原样带出）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegularizeWarning {
    /// 稳定告警码（W_REG_*）。
    pub code: &'static str,
    /// 人读说明。
    pub message: String,
}

impl RegularizeWarning {
    /// 退化 plant（mean|H|^2 低于尺度下限；lambda 尺度取 1）。
    pub const DEGENERATE_PLANT: &'static str = "W_REG_DEGENERATE_PLANT";
    /// P<M：欠定、最小范数解非唯一、不得交付。
    pub const UNDERDETERMINED: &'static str = "W_REG_UNDERDETERMINED";
    /// CV 不可用/无效（选择回退 L-curve，明确标记）。
    pub const CV_INVALID: &'static str = "W_REG_CV_INVALID";
    /// P==M 边界：CV 折 P-1<M 欠定，CV 不作选择依据。
    pub const CV_BOUNDARY: &'static str = "W_REG_CV_BOUNDARY";
    /// CV 容差带内发生了简化/更强正则偏好（tie-break）。
    pub const CV_TIEBREAK: &'static str = "W_REG_CV_TIEBREAK";
    /// L-curve 点不足/曲率不可算，无拐点。
    pub const L_CURVE_UNAVAILABLE: &'static str = "W_REG_L_CURVE_UNAVAILABLE";
    /// L-curve 平坦（曲率全为 0/None），拐点不可辨。
    pub const L_CURVE_FLAT: &'static str = "W_REG_L_CURVE_FLAT";
    /// 选中的 lambda 位于网格端点（正则不足/过强风险提示）。
    pub const LAMBDA_AT_EDGE: &'static str = "W_REG_LAMBDA_AT_EDGE";
    /// 最终解不满足声明 gain headroom（best-effort 返回但 deliverable=false）。
    pub const HEADROOM_VIOLATION: &'static str = "W_REG_HEADROOM_VIOLATION";
    /// 初始候选 headroom 不可行，已回退到其它候选。
    pub const HEADROOM_FALLBACK: &'static str = "W_REG_HEADROOM_FALLBACK";
    /// 使用了冻结先验（解偏向 g_prior）。
    pub const PRIOR_FROZEN: &'static str = "W_REG_PRIOR_FROZEN";
    /// 解几乎完全由先验决定（lambda 相对数据项过强）。
    pub const PRIOR_DOMINANT: &'static str = "W_REG_PRIOR_DOMINANT";
    /// Preserve 模式无修正可调（未做 CV/lambda/白名单选择）。
    pub const PRESERVE_NO_TUNING: &'static str = "W_REG_PRESERVE_NO_TUNING";
    /// 上游 solver 状态非 usable（未收敛/欠定/资源受限），交付性受影响。
    pub const UPSTREAM_STATUS: &'static str = "W_REG_UPSTREAM_STATUS";
    /// 改善带内近零目标点被跳过（不参与相对残差统计）。
    pub const NEAR_ZERO_TARGETS: &'static str = "W_REG_NEAR_ZERO_TARGETS";
    /// design positions 不足以做 CV（<2 或折后训练集为空）。
    pub const INSUFFICIENT_POSITIONS: &'static str = "W_REG_INSUFFICIENT_POSITIONS";
}

fn push_warning(warnings: &mut Vec<RegularizeWarning>, code: &'static str, message: String) {
    if warnings
        .iter()
        .any(|existing| existing.code == code && existing.message == message)
    {
        return;
    }
    warnings.push(RegularizeWarning { code, message });
}

// ===========================================================================
// 固定白名单（结构固定；与测量数据无关）
// ===========================================================================

/// 固定白名单候选（WBS T52 "固定 3-4 白名单"；**不按数据挑路径**）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WhitelistKind {
    /// 仅对角路径（M 条；最强结构先验）。
    DiagOnly,
    /// 对角 + dense 相邻 participant 双向路径（4.1.2 下 5+8=13 条）。
    DiagAdjacent,
    /// 对角 + SUB1 与其它 participant 的双向路径（4.1.2 下 5+8=13 条）。
    DiagSub1,
    /// 全 5x5（M*M 条；无结构先验，最弱正则）。
    Full,
}

impl WhitelistKind {
    /// 固定候选全集（顺序即枚举序，用于确定性 tie-break）。
    pub const ALL: [WhitelistKind; 4] = [
        WhitelistKind::DiagOnly,
        WhitelistKind::DiagAdjacent,
        WhitelistKind::DiagSub1,
        WhitelistKind::Full,
    ];

    /// 稳定字面值。
    pub const fn as_str(self) -> &'static str {
        match self {
            WhitelistKind::DiagOnly => "diag_only",
            WhitelistKind::DiagAdjacent => "diag_adjacent",
            WhitelistKind::DiagSub1 => "diag_sub1",
            WhitelistKind::Full => "full",
        }
    }

    /// 枚举序（tie-break 用；0 最保守）。
    pub const fn order(self) -> usize {
        match self {
            WhitelistKind::DiagOnly => 0,
            WhitelistKind::DiagAdjacent => 1,
            WhitelistKind::DiagSub1 => 2,
            WhitelistKind::Full => 3,
        }
    }

    /// 结构固定说明（报告用）。
    pub const fn definition(self) -> &'static str {
        match self {
            WhitelistKind::DiagOnly => "out_dense == in_dense",
            WhitelistKind::DiagAdjacent => {
                "out_dense == in_dense or |out_dense - in_dense| == 1 (dense neighbours, both directions)"
            }
            WhitelistKind::DiagSub1 => {
                "out_dense == in_dense or out/in participant role == SUB1 (both directions)"
            }
            WhitelistKind::Full => "all M*M paths free",
        }
    }

    /// 构造 M*M 自由路径掩码（row-major out*M+in；true=自由）。
    ///
    /// 掩码只由 roles/dense 索引决定，**不读取任何测量数据**；DiagSub1 要求存在角色
    /// 名恰为 SUB1 的 participant（否则拒收，不静默退化成其它集合）。
    pub fn mask(self, m: usize, roles: &[String]) -> Result<Vec<bool>, RegularizeError> {
        if m == 0 || m > MAX_PARTICIPANTS || m > MAX_LOW_BAND_PARTICIPANTS {
            return Err(err(
                RegularizeErrorKind::Whitelist,
                "regularize.whitelist.m",
                format!("M={m} unsupported for whitelists (1..={MAX_LOW_BAND_PARTICIPANTS})"),
            ));
        }
        if roles.len() != m {
            return Err(err(
                RegularizeErrorKind::Whitelist,
                "regularize.whitelist.roles",
                format!("roles length {} != M={m}", roles.len()),
            ));
        }
        let sub1 = roles.iter().position(|role| role == "SUB1");
        if self == WhitelistKind::DiagSub1 && sub1.is_none() {
            return Err(err(
                RegularizeErrorKind::Whitelist,
                "regularize.whitelist.diag_sub1",
                format!(
                    "diag_sub1 requires a participant role named SUB1; roles are {roles:?} (refusing to silently fall back)"
                ),
            ));
        }
        let mut mask = vec![false; m * m];
        for out_dense in 0..m {
            for in_dense in 0..m {
                let free = match self {
                    WhitelistKind::DiagOnly => out_dense == in_dense,
                    WhitelistKind::DiagAdjacent => {
                        out_dense == in_dense || out_dense.abs_diff(in_dense) == 1
                    }
                    WhitelistKind::DiagSub1 => match sub1 {
                        Some(sub) => out_dense == in_dense || out_dense == sub || in_dense == sub,
                        None => false,
                    },
                    WhitelistKind::Full => true,
                };
                mask[out_dense * m + in_dense] = free;
            }
        }
        Ok(mask)
    }

    /// 自由路径数（掩码构造成功时）。
    pub fn free_path_count(self, m: usize, roles: &[String]) -> Result<usize, RegularizeError> {
        Ok(self.mask(m, roles)?.iter().filter(|free| **free).count())
    }
}

// ===========================================================================
// 先验
// ===========================================================================

/// 先验种类（正则收缩目标）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PriorKind {
    /// 零修正先验：g_prior=0（Tikhonov 收缩到 identity/deltaD 基线；lambda 越大修正越小）。
    ZeroCorrection,
    /// 冻结系数先验：g_prior 来自上一次接受解（lambda 越大解越靠近该先验）。
    FrozenCoefficients,
}

impl PriorKind {
    /// 稳定字面值。
    pub const fn as_str(self) -> &'static str {
        match self {
            PriorKind::ZeroCorrection => "zero_correction",
            PriorKind::FrozenCoefficients => "frozen_coefficients",
        }
    }
}

/// 正则先验（零修正或冻结系数；冻结先验通过等价 target 变换实现，不改 T51 solver 语义）。
#[derive(Debug, Clone, PartialEq)]
pub struct RegularizationPrior {
    kind: PriorKind,
    roles: Vec<String>,
    grid_hz: Vec<f64>,
    taps: usize,
    coeffs: Vec<f64>,
    hash: String,
}

impl RegularizationPrior {
    /// 零修正先验（默认；无先验系数）。
    pub fn zero() -> Self {
        let mut canonical = String::new();
        canonical.push_str(REGULARIZE_PRIOR_HASH_DOMAIN);
        canonical.push('\n');
        canonical.push_str("kind=zero_correction\n");
        Self {
            kind: PriorKind::ZeroCorrection,
            roles: Vec::new(),
            grid_hz: Vec::new(),
            taps: 0,
            coeffs: Vec::new(),
            hash: sha256_hex(canonical.as_bytes()),
        }
    }

    /// 冻结系数先验（调用方显式给出 roles/grid/taps/coeffs；布局 [out][in][tap]）。
    pub fn frozen(
        roles: Vec<String>,
        grid_hz: Vec<f64>,
        taps: usize,
        coeffs: Vec<f64>,
    ) -> Result<Self, RegularizeError> {
        let m = roles.len();
        if m == 0 || m > MAX_PARTICIPANTS || m > MAX_LOW_BAND_PARTICIPANTS {
            return Err(err(
                RegularizeErrorKind::Prior,
                "regularize.prior.roles",
                format!("prior must declare 1..={MAX_LOW_BAND_PARTICIPANTS} roles, got {m}"),
            ));
        }
        for (index, role) in roles.iter().enumerate() {
            if !is_role_token(role) {
                return Err(err(
                    RegularizeErrorKind::Prior,
                    format!("regularize.prior.roles[{index}]"),
                    format!("role id {role:?} must match ^[A-Z][A-Z0-9]*$"),
                ));
            }
            if roles[..index].iter().any(|existing| existing == role) {
                return Err(err(
                    RegularizeErrorKind::Prior,
                    format!("regularize.prior.roles[{index}]"),
                    format!("duplicate role {role}"),
                ));
            }
        }
        if taps == 0 || taps > MAX_SOLVE_TAPS {
            return Err(err(
                RegularizeErrorKind::Prior,
                "regularize.prior.taps",
                format!("prior taps must be 1..={MAX_SOLVE_TAPS}, got {taps}"),
            ));
        }
        if grid_hz.len() < 2 {
            return Err(err(
                RegularizeErrorKind::Prior,
                "regularize.prior.grid_hz",
                "prior grid needs at least 2 points",
            ));
        }
        let mut previous = f64::NEG_INFINITY;
        for (index, freq) in grid_hz.iter().copied().enumerate() {
            if !freq.is_finite() || freq <= 0.0 || freq <= previous {
                return Err(err(
                    RegularizeErrorKind::Prior,
                    format!("regularize.prior.grid_hz[{index}]"),
                    "prior grid must be finite, positive and strictly increasing",
                ));
            }
            previous = freq;
        }
        if coeffs.len() != m * m * taps {
            return Err(err(
                RegularizeErrorKind::Prior,
                "regularize.prior.coeffs",
                format!(
                    "prior coefficients must be M*M*taps={} entries, got {}",
                    m * m * taps,
                    coeffs.len()
                ),
            ));
        }
        for (index, value) in coeffs.iter().copied().enumerate() {
            if !value.is_finite() || value.abs() > PRIOR_COEFF_SANITY {
                return Err(err(
                    RegularizeErrorKind::Range,
                    format!("regularize.prior.coeffs[{index}]"),
                    format!("prior coefficient must be finite with |g| <= {PRIOR_COEFF_SANITY}"),
                ));
            }
        }
        let mut canonical = String::new();
        canonical.push_str(REGULARIZE_PRIOR_HASH_DOMAIN);
        canonical.push('\n');
        canonical.push_str(&format!(
            "kind=frozen_coefficients m={m} taps={taps} grid_points={}\n",
            grid_hz.len()
        ));
        for role in &roles {
            canonical.push_str(&format!("role={role}\n"));
        }
        for freq in &grid_hz {
            canonical.push_str(&fmt_f64(*freq));
            canonical.push('\n');
        }
        for value in &coeffs {
            canonical.push_str(&fmt_f64(*value));
            canonical.push('\n');
        }
        let hash = sha256_hex(canonical.as_bytes());
        Ok(Self {
            kind: PriorKind::FrozenCoefficients,
            roles,
            grid_hz,
            taps,
            coeffs,
            hash,
        })
    }

    /// 从 T51 解构造冻结先验（须与 problem 的 roles/grid/taps 对齐）。
    pub fn from_solution(
        solution: &SolveSolution,
        problem: &SolveProblem,
    ) -> Result<Self, RegularizeError> {
        let m = solution.modal.m();
        if m == 0 {
            return Err(err(
                RegularizeErrorKind::Prior,
                "regularize.prior.solution",
                "solution modal matrix has no roles",
            ));
        }
        let denominator = m * m;
        if solution.coeffs_f64.len() % denominator != 0 {
            return Err(err(
                RegularizeErrorKind::Prior,
                "regularize.prior.solution.coeffs",
                format!(
                    "solution coefficients length {} is not a multiple of M*M={denominator}",
                    solution.coeffs_f64.len()
                ),
            ));
        }
        let taps = solution.coeffs_f64.len() / denominator;
        let prior = Self::frozen(
            solution.modal.roles().to_vec(),
            solution.modal.grid_hz().to_vec(),
            taps,
            solution.coeffs_f64.clone(),
        )?;
        prior.validate_for(problem, taps)?;
        Ok(prior)
    }

    /// 从 T52 正则解构造冻结先验（下游上一轮接受解作为先验）。
    pub fn from_regularized(
        solution: &RegularizedSolution,
        problem: &SolveProblem,
    ) -> Result<Self, RegularizeError> {
        let m = solution.modal.m();
        if m == 0 {
            return Err(err(
                RegularizeErrorKind::Prior,
                "regularize.prior.solution",
                "solution modal matrix has no roles",
            ));
        }
        let denominator = m * m;
        if solution.coeffs_f64.len() % denominator != 0 {
            return Err(err(
                RegularizeErrorKind::Prior,
                "regularize.prior.solution.coeffs",
                format!(
                    "solution coefficients length {} is not a multiple of M*M={denominator}",
                    solution.coeffs_f64.len()
                ),
            ));
        }
        let taps = solution.coeffs_f64.len() / denominator;
        let prior = Self::frozen(
            solution.modal.roles().to_vec(),
            solution.modal.grid_hz().to_vec(),
            taps,
            solution.coeffs_f64.clone(),
        )?;
        prior.validate_for(problem, taps)?;
        Ok(prior)
    }

    /// 种类。
    pub fn kind(&self) -> PriorKind {
        self.kind
    }

    /// 是否零修正先验。
    pub fn is_zero(&self) -> bool {
        self.kind == PriorKind::ZeroCorrection
    }

    /// 先验 hash（确定性）。
    pub fn hash(&self) -> &str {
        &self.hash
    }

    /// 先验 taps（零先验为 0）。
    pub fn taps(&self) -> usize {
        self.taps
    }

    /// 先验系数（零先验为空）。
    pub fn coeffs(&self) -> &[f64] {
        &self.coeffs
    }

    /// 先验 roles（零先验为空）。
    pub fn roles(&self) -> &[String] {
        &self.roles
    }

    /// 先验 grid（零先验为空）。
    pub fn grid_hz(&self) -> &[f64] {
        &self.grid_hz
    }

    /// 与 problem/options.taps 对齐校验（零先验恒通过）。
    pub fn validate_for(&self, problem: &SolveProblem, taps: usize) -> Result<(), RegularizeError> {
        if self.is_zero() {
            return Ok(());
        }
        let m = problem.m();
        if self.taps != taps {
            return Err(err(
                RegularizeErrorKind::Prior,
                "regularize.prior.taps",
                format!("prior taps {} must equal base solve taps {taps}", self.taps),
            ));
        }
        if self.roles != problem.roles {
            return Err(err(
                RegularizeErrorKind::Prior,
                "regularize.prior.roles",
                "prior roles must equal problem roles (dense order matters for G layout)",
            ));
        }
        if self.grid_hz.len() != problem.grid_hz.len()
            || !self
                .grid_hz
                .iter()
                .zip(problem.grid_hz.iter())
                .all(|(left, right)| grid_freq_equal(*left, *right))
        {
            return Err(err(
                RegularizeErrorKind::Prior,
                "regularize.prior.grid_hz",
                "prior grid must equal problem grid; G on a different grid is not interchangeable",
            ));
        }
        if self.coeffs.len() != m * m * taps {
            return Err(err(
                RegularizeErrorKind::Prior,
                "regularize.prior.coeffs",
                format!(
                    "prior coefficients must be M*M*taps={} entries, got {}",
                    m * m * taps,
                    self.coeffs.len()
                ),
            ));
        }
        Ok(())
    }
}

// ===========================================================================
// gain headroom 规格
// ===========================================================================

/// gain headroom 规格（mimo-t6 §4.8 PR-3）。
#[derive(Debug, Clone, PartialEq)]
pub struct HeadroomSpec {
    /// 修正路径（非对角）增益帽 dB（默认 -6 dB；允许显式放宽至 +6 dB）。
    pub correction_cap_db: f64,
    /// 对角路径最大增益帽 dB（默认 +6 dB，相对 0 dB 基线）。
    pub diagonal_cap_db: f64,
    /// 门容差 dB（PR-3：最大增益 <= 帽 + 2 dB）。
    pub tolerance_db: f64,
}

impl Default for HeadroomSpec {
    fn default() -> Self {
        Self {
            correction_cap_db: DEFAULT_CORRECTION_GAIN_CAP_DB,
            diagonal_cap_db: DEFAULT_DIAGONAL_GAIN_CAP_DB,
            tolerance_db: HEADROOM_TOLERANCE_DB,
        }
    }
}

impl HeadroomSpec {
    /// 校验规格范围。
    pub fn validate(&self) -> Result<(), RegularizeError> {
        if !self.correction_cap_db.is_finite()
            || self.correction_cap_db < CORRECTION_GAIN_CAP_MIN_DB
            || self.correction_cap_db > CORRECTION_GAIN_CAP_MAX_DB
        {
            return Err(err(
                RegularizeErrorKind::Headroom,
                "regularize.options.headroom.correction_cap_db",
                format!(
                    "correction gain cap must be finite in [{CORRECTION_GAIN_CAP_MIN_DB},{CORRECTION_GAIN_CAP_MAX_DB}] dB (PR-3 default -6, relaxed max +6), got {}",
                    self.correction_cap_db
                ),
            ));
        }
        if !self.diagonal_cap_db.is_finite()
            || self.diagonal_cap_db < DIAGONAL_GAIN_CAP_MIN_DB
            || self.diagonal_cap_db > DIAGONAL_GAIN_CAP_MAX_DB
        {
            return Err(err(
                RegularizeErrorKind::Headroom,
                "regularize.options.headroom.diagonal_cap_db",
                format!(
                    "diagonal gain cap must be finite in [{DIAGONAL_GAIN_CAP_MIN_DB},{DIAGONAL_GAIN_CAP_MAX_DB}] dB, got {}",
                    self.diagonal_cap_db
                ),
            ));
        }
        if !self.tolerance_db.is_finite()
            || self.tolerance_db < 0.0
            || self.tolerance_db > MAX_HEADROOM_TOLERANCE_DB
        {
            return Err(err(
                RegularizeErrorKind::Headroom,
                "regularize.options.headroom.tolerance_db",
                format!(
                    "headroom tolerance must be finite in [0,{MAX_HEADROOM_TOLERANCE_DB}] dB (PR-3: +2 dB), got {}",
                    self.tolerance_db
                ),
            ));
        }
        Ok(())
    }
}

// ===========================================================================
// 选项
// ===========================================================================

/// 正则选项（固定候选枚举；base 的 ridge/path_mask 由本模块独占）。
#[derive(Debug, Clone, PartialEq)]
pub struct RegularizeOptions {
    /// 基础 T51 solve 选项（taps/delay/fit band/迭代/资源上限/频带权重；ridge 与 path_mask 须留空）。
    pub base: SolveOptions,
    /// lambda 候选网格（固定枚举；按 plant 尺度归一后使用）。
    pub lambda_grid: Vec<f64>,
    /// 固定白名单候选子集（必须来自 WhitelistKind::ALL；不允许自定义自由路径）。
    pub whitelists: Vec<WhitelistKind>,
    /// CV 折数（0 = leave-one-out；>=2 为显式 k 折；1 非法）。
    pub cv_folds: usize,
    /// CV 容差带（dB；带内按更简单/更强正则偏好）。
    pub selection_tolerance_db: f64,
    /// 正则先验（默认零修正先验）。
    pub prior: RegularizationPrior,
    /// gain headroom 规格。
    pub headroom: HeadroomSpec,
    /// 禁止进入调参的 label（holdout/validation；与 design position label 冲突直接拒收）。
    pub forbidden_tuning_labels: Vec<String>,
}

impl Default for RegularizeOptions {
    fn default() -> Self {
        Self {
            base: SolveOptions::default(),
            lambda_grid: LAMBDA_GRID.to_vec(),
            whitelists: WhitelistKind::ALL.to_vec(),
            cv_folds: DEFAULT_CV_FOLDS,
            selection_tolerance_db: DEFAULT_SELECTION_TOLERANCE_DB,
            prior: RegularizationPrior::zero(),
            headroom: HeadroomSpec::default(),
            forbidden_tuning_labels: Vec::new(),
        }
    }
}

impl RegularizeOptions {
    /// 校验选项与问题的一致性（不读 holdout；任何 holdout label 冲突拒收）。
    pub fn validate(&self, problem: &SolveProblem) -> Result<(), RegularizeError> {
        problem.validate().map_err(map_solve_error)?;
        let m = problem.m();
        let positions = problem.positions.len();
        if self.base.ridge != 0.0 {
            return Err(err(
                RegularizeErrorKind::Options,
                "regularize.options.base.ridge",
                "regularizer owns the ridge/lambda scan; leave base.ridge = 0",
            ));
        }
        if self.base.path_mask.is_some() {
            return Err(err(
                RegularizeErrorKind::Options,
                "regularize.options.base.path_mask",
                "regularizer owns the fixed whitelist; leave base.path_mask = None",
            ));
        }
        self.base.validate(problem).map_err(map_solve_error)?;
        if self.lambda_grid.is_empty() {
            return Err(err(
                RegularizeErrorKind::Lambda,
                "regularize.options.lambda_grid",
                "lambda grid must not be empty",
            ));
        }
        if self.lambda_grid.len() > MAX_LAMBDA_GRID_POINTS {
            return Err(err(
                RegularizeErrorKind::Lambda,
                "regularize.options.lambda_grid",
                format!(
                    "lambda grid has {} points; fixed scan is capped at {MAX_LAMBDA_GRID_POINTS}",
                    self.lambda_grid.len()
                ),
            ));
        }
        let mut previous = f64::NEG_INFINITY;
        for (index, lambda) in self.lambda_grid.iter().copied().enumerate() {
            if !lambda.is_finite() || lambda < 0.0 || lambda > MAX_LAMBDA {
                return Err(err(
                    RegularizeErrorKind::Lambda,
                    format!("regularize.options.lambda_grid[{index}]"),
                    format!("lambda must be finite in [0,{MAX_LAMBDA}], got {lambda}"),
                ));
            }
            if lambda <= previous {
                return Err(err(
                    RegularizeErrorKind::Lambda,
                    format!("regularize.options.lambda_grid[{index}]"),
                    format!("lambda grid must be strictly increasing; {lambda} after {previous}"),
                ));
            }
            previous = lambda;
        }
        if self.whitelists.is_empty() {
            return Err(err(
                RegularizeErrorKind::Whitelist,
                "regularize.options.whitelists",
                "at least one fixed whitelist candidate is required",
            ));
        }
        if self.whitelists.len() > WhitelistKind::ALL.len() {
            return Err(err(
                RegularizeErrorKind::Whitelist,
                "regularize.options.whitelists",
                format!(
                    "at most {} fixed whitelist candidates; got {}",
                    WhitelistKind::ALL.len(),
                    self.whitelists.len()
                ),
            ));
        }
        for (index, whitelist) in self.whitelists.iter().copied().enumerate() {
            if self.whitelists[..index]
                .iter()
                .any(|existing| *existing == whitelist)
            {
                return Err(err(
                    RegularizeErrorKind::Whitelist,
                    format!("regularize.options.whitelists[{index}]"),
                    format!("duplicate whitelist {}", whitelist.as_str()),
                ));
            }
            whitelist.mask(m, &problem.roles)?;
        }
        if self.cv_folds != 0 {
            if self.cv_folds < 2 || self.cv_folds > MAX_CV_FOLDS {
                return Err(err(
                    RegularizeErrorKind::Fold,
                    "regularize.options.cv_folds",
                    format!(
                        "cv_folds must be 0 (leave-one-out) or 2..={MAX_CV_FOLDS}, got {}",
                        self.cv_folds
                    ),
                ));
            }
            if self.cv_folds > positions {
                return Err(err(
                    RegularizeErrorKind::Fold,
                    "regularize.options.cv_folds",
                    format!(
                        "cv_folds={} exceeds design positions={positions}",
                        self.cv_folds
                    ),
                ));
            }
        }
        if !self.selection_tolerance_db.is_finite()
            || self.selection_tolerance_db < 0.0
            || self.selection_tolerance_db > MAX_SELECTION_TOLERANCE_DB
        {
            return Err(err(
                RegularizeErrorKind::Options,
                "regularize.options.selection_tolerance_db",
                format!(
                    "selection tolerance must be finite in [0,{MAX_SELECTION_TOLERANCE_DB}] dB, got {}",
                    self.selection_tolerance_db
                ),
            ));
        }
        self.headroom.validate()?;
        self.prior.validate_for(problem, self.base.taps)?;
        if problem.mode == SolveMode::Preserve && !self.prior.is_zero() {
            return Err(err(
                RegularizeErrorKind::Prior,
                "regularize.options.prior",
                "preserve target requests no correction; a frozen prior is meaningless and is refused",
            ));
        }
        for (index, label) in self.forbidden_tuning_labels.iter().enumerate() {
            if label.is_empty() || label.len() > 64 || !is_label_token(label) {
                return Err(err(
                    RegularizeErrorKind::Holdout,
                    format!("regularize.options.forbidden_tuning_labels[{index}]"),
                    format!("label {label:?} must be a stable id token"),
                ));
            }
            if problem
                .positions
                .iter()
                .any(|position| position.label == *label)
            {
                return Err(err(
                    RegularizeErrorKind::Holdout,
                    format!("regularize.options.forbidden_tuning_labels[{index}]"),
                    format!(
                        "label {label:?} is declared as a design position and as forbidden tuning input; holdout/validation must never enter CV/lambda/whitelist selection (mimo-t6 §5.1)"
                    ),
                ));
            }
        }
        Ok(())
    }
}

// ===========================================================================
// 报告数据结构
// ===========================================================================

/// P>=M 适用边界标记（WBS T52 DoD）。
#[derive(Debug, Clone, PartialEq)]
pub struct ApplicabilityBoundary {
    /// M。
    pub m: usize,
    /// design position 数 P。
    pub design_positions: usize,
    /// P>=M（全矩阵可辨识必要条件）。
    pub p_ge_m: bool,
    /// 边界状态字面值（applicable / boundary_p_equals_m / not_applicable_p_below_m）。
    pub status: &'static str,
    /// design 内 CV 有效性要求 P>M（LOO 折训练集 P-1>=M）。
    pub cv_requires_p_gt_m: bool,
    /// 说明。
    pub note: &'static str,
}

/// 单候选掩码后的适用性（necessary-only）。
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateApplicability {
    /// 白名单。
    pub whitelist: WhitelistKind,
    /// 自由路径数。
    pub free_paths: usize,
    /// 每频点混合矩阵秩上界 min(P, free_paths)。
    pub per_frequency_rank_bound: usize,
    /// FIR 秩上界 min(2*P*band_points, free_paths*taps)。
    pub fir_rank_bound: usize,
    /// 每频点满列秩必要条件 P>=free_paths（necessary-only）。
    pub per_frequency_full_rank_necessary: bool,
    /// FIR 可辨识必要条件 2*P*band_points>=free_paths*taps（necessary-only）。
    pub fir_rank_necessary: bool,
    /// 说明。
    pub note: &'static str,
}

/// 单折 CV 报告。
#[derive(Debug, Clone, PartialEq)]
pub struct FoldReport {
    /// 折索引。
    pub fold_index: usize,
    /// 留出的 design position label。
    pub held_out_labels: Vec<String>,
    /// 训练集 design position 数。
    pub training_positions: usize,
    /// T51 solve 状态字面值。
    pub solve_status: &'static str,
    /// 该折是否给出可用解且留出点有有效残差。
    pub usable: bool,
    /// 该折训练问题是否被 T51 判为欠定。
    pub underdetermined: bool,
    /// 迭代总数。
    pub iterations: usize,
    /// 留出点平均相对残差 dB（无有效点 → NaN）。
    pub mean_residual_db: f64,
    /// 留出点最坏相对残差 dB（无有效点 → NaN）。
    pub worst_residual_db: f64,
    /// 有效网格点数。
    pub valid_points: usize,
    /// 近零目标跳过点数。
    pub near_zero_points: usize,
    /// 该折解是否满足 headroom。
    pub headroom_feasible: bool,
    /// 该折解 headroom 最坏超限 dB。
    pub headroom_worst_excess_db: f64,
}

/// 单候选（白名单 × lambda）CV 报告。
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateReport {
    /// 白名单。
    pub whitelist: WhitelistKind,
    /// 自由路径数。
    pub free_paths: usize,
    /// lambda 网格值（归一前）。
    pub lambda: f64,
    /// 实际 ridge（lambda * lambda_scale）。
    pub lambda_used: f64,
    /// CV 平均相对残差 dB（无效 → None）。
    pub cv_mean_db: Option<f64>,
    /// CV 最坏折相对残差 dB（无效 → None）。
    pub cv_worst_db: Option<f64>,
    /// CV 是否有效（全部折可用且非欠定）。
    pub cv_valid: bool,
    /// CV 排名（1 起；无效 → None）。
    pub cv_rank: Option<usize>,
    /// 是否存在欠定折。
    pub fold_underdetermined: bool,
    /// 全部折解是否满足 headroom。
    pub headroom_feasible_in_folds: bool,
    /// 折解 headroom 最坏超限 dB。
    pub headroom_worst_excess_in_folds_db: f64,
    /// 掩码后适用性。
    pub applicability: CandidateApplicability,
    /// 折明细。
    pub folds: Vec<FoldReport>,
}

/// 单 lambda 扫描点。
#[derive(Debug, Clone, PartialEq)]
pub struct LambdaPoint {
    /// lambda 网格值（归一前）。
    pub lambda: f64,
    /// 实际 ridge。
    pub lambda_used: f64,
    /// design positions 上拟合带加权残差范数。
    pub design_residual_norm: f64,
    /// 解范数（||g-g_prior||；零先验即 ||g||）。
    pub solution_norm: f64,
    /// 拟合带最大相对残差 dB。
    pub fit_band_max_db: f64,
    /// 改善带最大相对残差 dB。
    pub improvement_band_max_db: f64,
    /// log-log 曲率（端点/退化 → None）。
    pub curvature: Option<f64>,
    /// T51 solve 状态字面值。
    pub solve_status: &'static str,
    /// 该点解是否满足 headroom。
    pub headroom_feasible: bool,
    /// 该点解 headroom 最坏超限 dB。
    pub headroom_worst_excess_db: f64,
}

/// lambda 扫描 / L-curve 报告。
#[derive(Debug, Clone, PartialEq)]
pub struct LambdaScan {
    /// lambda 尺度（mean|H|^2）。
    pub lambda_scale: f64,
    /// 本扫描使用的白名单（恒等于最终 selection.selected_whitelist：headroom 回退改白名单时重算）。
    pub scan_whitelist: WhitelistKind,
    /// 扫描点（顺序同 lambda 网格）。
    pub points: Vec<LambdaPoint>,
    /// L-curve 拐点索引（None = 不可辨）。
    pub l_curve_corner_index: Option<usize>,
    /// L-curve 拐点 lambda（None = 不可辨）。
    pub l_curve_corner_lambda: Option<f64>,
    /// 说明（L-curve 只作诊断）。
    pub note: &'static str,
}

/// 选择报告。
#[derive(Debug, Clone, PartialEq)]
pub struct SelectionReport {
    /// 选择方法（cv / l_curve_fallback / preserve_baseline）。
    pub method: &'static str,
    /// 最终白名单。
    pub selected_whitelist: WhitelistKind,
    /// 最终 lambda（归一前）。
    pub selected_lambda: f64,
    /// 最终实际 ridge。
    pub selected_lambda_used: f64,
    /// CV 排名（1 起；未走 CV → None）。
    pub selected_cv_rank: Option<usize>,
    /// 选中候选 CV 平均 dB（未走 CV → None）。
    pub selected_cv_mean_db: Option<f64>,
    /// CV 是否有效。
    pub cv_valid: bool,
    /// CV 无效原因（None = CV 有效）。
    pub cv_reason: Option<&'static str>,
    /// 是否使用了回退（L-curve 或 headroom 回退）。
    pub fallback_used: bool,
    /// 最终选择是否确为容差带 tie-break 选中项（headroom 回退改掉选择时为 false）。
    pub tie_break_used: bool,
    /// L-curve 拐点 lambda（诊断）。
    pub l_curve_corner_lambda: Option<f64>,
    /// headroom 回退额外尝试次数（0 = 初始候选即通过）。
    pub headroom_fallback_attempts: usize,
}

/// 单路径 headroom。
#[derive(Debug, Clone, PartialEq)]
pub struct PathHeadroom {
    /// 输出 dense 索引。
    pub out_dense: u16,
    /// 输入 dense 索引。
    pub in_dense: u16,
    /// 输出角色。
    pub out_role: String,
    /// 输入角色。
    pub in_role: String,
    /// 是否为修正路径（非对角）。
    pub is_correction: bool,
    /// 改善带内最大增益 dB（由最终 G 实算）。
    pub max_gain_db: f64,
    /// 声明帽 dB。
    pub cap_db: f64,
    /// 余量 dB = cap + tolerance - max_gain（<0 即超限）。
    pub margin_db: f64,
    /// 是否通过。
    pub passed: bool,
}

/// gain headroom 报告（mimo-t6 §4.8 PR-3）。
#[derive(Debug, Clone, PartialEq)]
pub struct HeadroomReport {
    /// 修正路径帽 dB。
    pub correction_cap_db: f64,
    /// 对角路径帽 dB。
    pub diagonal_cap_db: f64,
    /// 容差 dB。
    pub tolerance_db: f64,
    /// 评估频带。
    pub band_hz: (f64, f64),
    /// 每路径明细（M*M 条，row-major out*M+in）。
    pub paths: Vec<PathHeadroom>,
    /// 修正路径最大增益 dB。
    pub max_correction_gain_db: f64,
    /// 对角路径最大增益 dB。
    pub max_diagonal_gain_db: f64,
    /// 最坏超限 dB（0 = 全部通过）。
    pub worst_excess_db: f64,
    /// 是否全部通过。
    pub passed: bool,
    /// 说明。
    pub note: &'static str,
}

/// 设计残差报告（由最终 G 实算）。
#[derive(Debug, Clone, PartialEq)]
pub struct DesignResidualReport {
    /// 拟合带最大相对残差 dB。
    pub fit_band_max_db: f64,
    /// 改善带最大相对残差 dB。
    pub improvement_band_max_db: f64,
    /// 拟合带加权残差范数。
    pub residual_norm: f64,
    /// 最坏 (position, role, dB)。
    pub worst: Option<(String, String, f64)>,
    /// 近零目标跳过点数。
    pub near_zero_points: usize,
    /// 有效网格点数。
    pub valid_points: usize,
}

/// T50 validate_solution 摘要（只有 from_target_plant 入口填充）。
#[derive(Debug, Clone, PartialEq)]
pub struct SolutionValidationSummary {
    /// profile 字面值。
    pub profile: &'static str,
    /// 全部结构/预测检查是否通过。
    pub passed: bool,
    /// 改善带最大复数相对残差 dB。
    pub residual_max_db: Option<f64>,
    /// 非对角最大幅度 dB。
    pub offdiag_max_db: f64,
    /// 对角偏离 0 dB 最大值。
    pub diag_deviation_max_db: f64,
    /// 检查项 (name, passed, detail)。
    pub checks: Vec<(String, bool, String)>,
}

impl SolutionValidationSummary {
    fn from_validation(validation: SolutionValidation) -> Self {
        Self {
            profile: validation.profile.as_str(),
            passed: validation.passed,
            residual_max_db: validation.residual_max_db,
            offdiag_max_db: validation.offdiag_max_db,
            diag_deviation_max_db: validation.diag_deviation_max_db,
            checks: validation
                .checks
                .into_iter()
                .map(|check| (check.name.to_string(), check.passed, check.detail))
                .collect(),
        }
    }
}

// ===========================================================================
// 产物
// ===========================================================================

/// 正则解（最终 G 复响应 + 因果 FIR 系数；布局与 T07 kernel 对齐）。
#[derive(Debug, Clone, PartialEq)]
pub struct RegularizedSolution {
    /// 最终 G（含 deltaD 基线 + 先验 + 修正）。
    pub modal: ModalMatrix,
    /// 最终因果 FIR 系数 g[out][in][tap]（完整 G=deltaD·I+g；f64）。
    pub coeffs_f64: Vec<f64>,
    /// 同布局 f32（生产精度输入）。
    pub coeffs_f32: Vec<f32>,
    /// 最终系数 f64 规范化文本的 sha256（正则域）。
    pub coeffs_sha256: String,
    /// 正则解 hash（problem/prior/base solve/系数；供 T53/T54/T66 hash 链）。
    pub hash: String,
    /// 上游 T51 解 hash（无 → None）。
    pub base_solve_hash: Option<String>,
    /// 先验 hash。
    pub prior_hash: String,
}

impl RegularizedSolution {
    /// G 复响应按 (out,in) 取。
    pub fn response(&self, out_dense: u16, in_dense: u16) -> Option<&[ComplexPoint]> {
        self.modal.cell(out_dense, in_dense)
    }

    /// 系数按 (out,in,tap) 取。
    pub fn coeff(&self, out_dense: usize, in_dense: usize, tap: usize) -> Option<f64> {
        let m = self.modal.m();
        let taps = self.coeffs_f64.len() / (m * m);
        if out_dense >= m || in_dense >= m || tap >= taps {
            return None;
        }
        Some(self.coeffs_f64[(out_dense * m + in_dense) * taps + tap])
    }
}

/// 正则报告（诚实：predicted-only；不含实测改善）。
#[derive(Debug, Clone, PartialEq)]
pub struct RegularizeReport {
    /// schema。
    pub schema: &'static str,
    /// 模式字面值。
    pub mode: &'static str,
    /// M。
    pub m: usize,
    /// taps。
    pub taps: usize,
    /// G 基线延迟 D。
    pub delay_samples: u32,
    /// fs。
    pub sample_rate_hz: u32,
    /// 拟合带。
    pub fit_band_hz: (f64, f64),
    /// 改善带（63-250 Hz）。
    pub improvement_band_hz: (f64, f64),
    /// 网格点数。
    pub grid_points: usize,
    /// 拟合带网格点数。
    pub band_points: usize,
    /// design position 数。
    pub design_positions: usize,
    /// design position label（顺序同 problem）。
    pub position_labels: Vec<String>,
    /// design position 权重。
    pub position_weights: Vec<f64>,
    /// 适用边界。
    pub applicability: ApplicabilityBoundary,
    /// lambda 尺度。
    pub lambda_scale: f64,
    /// lambda 扫描 / L-curve。
    pub lambda_scan: LambdaScan,
    /// 全部候选（白名单 × lambda）。
    pub candidates: Vec<CandidateReport>,
    /// 选择报告。
    pub selection: SelectionReport,
    /// 先验种类字面值。
    pub prior_kind: &'static str,
    /// 先验 hash（零先验也有；None 仅不可能分支）。
    pub prior_hash: Option<String>,
    /// 解到先验的距离 ||g-g_prior||。
    pub prior_distance_norm: f64,
    /// headroom 报告。
    pub headroom: HeadroomReport,
    /// 最终 T51 solve 状态字面值。
    pub final_solve_status: &'static str,
    /// 最终设计残差（由最终 G 实算）。
    pub final_residual: DesignResidualReport,
    /// T50 validate_solution 摘要（仅 from_target_plant 入口）。
    pub solution_validation: Option<SolutionValidationSummary>,
    /// CV 只使用 design positions（恒 true）。
    pub cv_uses_only_design_positions: bool,
    /// holdout 是否参与调参（恒 false）。
    pub holdout_used_for_tuning: bool,
    /// 禁止进入调参的 label（holdout/validation）。
    pub forbidden_tuning_labels: Vec<String>,
    /// 是否可交付（适用边界内、解可用、headroom 通过）。
    pub deliverable: bool,
    /// 是否已应用（恒 false）。
    pub applied: bool,
    /// 应用后实测（恒 not_requested）。
    pub post_apply_measurement: &'static str,
    /// 引擎兼容性（恒 not_validated）。
    pub engine_compatibility: &'static str,
    /// 运行期验证（恒 not_run）。
    pub runtime_validation: &'static str,
    /// 诚实 claim。
    pub claims: &'static str,
    /// 诊断。
    pub warnings: Vec<RegularizeWarning>,
    /// 解 hash（无解 → None）。
    pub solution_hash: Option<String>,
    /// 系数 sha256（无解 → None）。
    pub coeffs_sha256: Option<String>,
    /// 上游 base solve hash（无解 → None）。
    pub base_solve_hash: Option<String>,
}

/// 正则结果：报告 + 可选解（资源受限等无解）。
#[derive(Debug, Clone, PartialEq)]
pub struct RegularizeOutcome {
    /// 报告。
    pub report: RegularizeReport,
    /// 解（无解时 None；best-effort 解也返回并带 deliverable=false）。
    pub solution: Option<RegularizedSolution>,
}

// ===========================================================================
// 数值辅助
// ===========================================================================

/// 内部复数（(re, im)；f64 设计侧）。
type C = (f64, f64);

fn cadd(a: C, b: C) -> C {
    (a.0 + b.0, a.1 + b.1)
}

fn cmul(a: C, b: C) -> C {
    (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0)
}

fn cabs(a: C) -> f64 {
    a.0.hypot(a.1)
}

fn cabs2(a: C) -> f64 {
    a.0 * a.0 + a.1 * a.1
}

fn fmt_f64(value: f64) -> String {
    let normalized = if value == 0.0 { 0.0 } else { value };
    debug_assert!(normalized.is_finite());
    format!("{normalized:.17e}")
}

fn magnitude_db_of(magnitude: f64) -> f64 {
    20.0 * magnitude.max(REGULARIZE_DB_FLOOR).log10()
}

fn ratio_db(numerator: f64, denominator: f64) -> f64 {
    20.0 * (numerator.max(REGULARIZE_DB_FLOOR) / denominator.max(REGULARIZE_DB_FLOOR)).log10()
}

fn grid_freq_equal(left: f64, right: f64) -> bool {
    let tolerance = REGULARIZE_GRID_REL_TOLERANCE * left.abs().max(right.abs()).max(1.0);
    (left - right).abs() <= tolerance
}

fn is_role_token(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 {
        return false;
    }
    bytes[0].is_ascii_uppercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn is_label_token(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    if !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    bytes
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn json_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len() + 8);
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

fn json_f64(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.17e}")
    } else {
        "null".to_string()
    }
}

fn json_opt_f64(value: Option<f64>) -> String {
    match value {
        Some(value) => json_f64(value),
        None => "null".to_string(),
    }
}

// ===========================================================================
// 派生量
// ===========================================================================

/// 拟合带内网格索引（含端点；须 >=2 点）。
fn fit_band_indices(
    problem: &SolveProblem,
    fit_band_hz: (f64, f64),
) -> Result<Vec<usize>, RegularizeError> {
    let indices: Vec<usize> = problem
        .grid_hz
        .iter()
        .enumerate()
        .filter(|(_, freq)| **freq >= fit_band_hz.0 && **freq <= fit_band_hz.1)
        .map(|(index, _)| index)
        .collect();
    if indices.len() < 2 {
        return Err(err(
            RegularizeErrorKind::Problem,
            "regularize.fit_band_hz",
            format!(
                "fit band [{},{}] Hz contains {} grid point(s); at least 2 required",
                fit_band_hz.0,
                fit_band_hz.1,
                indices.len()
            ),
        ));
    }
    Ok(indices)
}

/// 频带内网格索引（无点 → 空 vec）。
fn band_indices(problem: &SolveProblem, lo_hz: f64, hi_hz: f64) -> Vec<usize> {
    problem
        .grid_hz
        .iter()
        .enumerate()
        .filter(|(_, freq)| **freq >= lo_hz && **freq <= hi_hz)
        .map(|(index, _)| index)
        .collect()
}

/// lambda 尺度 = 拟合带内 mean|H|^2（跨 design positions/band/output cells）。
fn compute_lambda_scale(problem: &SolveProblem, fit_indices: &[usize], m: usize) -> f64 {
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for position in &problem.positions {
        for output_dense in 0..m {
            for grid_index in fit_indices.iter().copied() {
                let point = position.h_eff[output_dense][grid_index];
                sum += cabs2((point.re, point.im));
                count += 1;
            }
        }
    }
    if count == 0 {
        0.0
    } else {
        sum / count as f64
    }
}

/// P>=M 适用边界。
fn applicability_boundary(problem: &SolveProblem) -> ApplicabilityBoundary {
    let m = problem.m();
    let design_positions = problem.positions.len();
    let p_ge_m = design_positions >= m;
    let status = if design_positions > m {
        APPLICABILITY_APPLICABLE
    } else if design_positions == m {
        APPLICABILITY_BOUNDARY
    } else {
        APPLICABILITY_NOT_APPLICABLE
    };
    ApplicabilityBoundary {
        m,
        design_positions,
        p_ge_m,
        status,
        cv_requires_p_gt_m: true,
        note: APPLICABILITY_NOTE,
    }
}

/// 候选掩码后的适用性（necessary-only）。
fn candidate_applicability(
    whitelist: WhitelistKind,
    free_paths: usize,
    positions: usize,
    band_points: usize,
    taps: usize,
) -> CandidateApplicability {
    let per_frequency_rank_bound = positions.min(free_paths);
    let fir_rank_bound = (2usize.saturating_mul(positions).saturating_mul(band_points))
        .min(free_paths.saturating_mul(taps));
    CandidateApplicability {
        whitelist,
        free_paths,
        per_frequency_rank_bound,
        fir_rank_bound,
        per_frequency_full_rank_necessary: positions >= free_paths,
        fir_rank_necessary: fir_rank_bound >= free_paths.saturating_mul(taps),
        note: CANDIDATE_APPLICABILITY_NOTE,
    }
}

/// CV 折计划（确定性 round-robin）。
#[derive(Debug, Clone, PartialEq)]
struct FoldPlan {
    fold_count: usize,
    held_out: Vec<Vec<usize>>,
    training: Vec<Vec<usize>>,
}

fn build_folds(positions: usize, requested: usize) -> Result<FoldPlan, RegularizeError> {
    if positions < 2 {
        return Ok(FoldPlan {
            fold_count: 0,
            held_out: Vec::new(),
            training: Vec::new(),
        });
    }
    let fold_count = if requested == 0 {
        positions.min(MAX_CV_FOLDS)
    } else {
        requested
    };
    if fold_count < 2 || fold_count > positions {
        return Err(err(
            RegularizeErrorKind::Fold,
            "regularize.cv.folds",
            format!("fold count {fold_count} is invalid for {positions} design positions"),
        ));
    }
    let mut held_out = vec![Vec::new(); fold_count];
    let mut training = vec![Vec::new(); fold_count];
    for position in 0..positions {
        let fold = position % fold_count;
        held_out[fold].push(position);
        for (other, bucket) in training.iter_mut().enumerate() {
            if other != fold {
                bucket.push(position);
            }
        }
    }
    if held_out.iter().any(|bucket| bucket.is_empty())
        || training.iter().any(|bucket| bucket.is_empty())
    {
        return Err(err(
            RegularizeErrorKind::Fold,
            "regularize.cv.folds",
            "fold assignment produced an empty held-out or training bucket",
        ));
    }
    Ok(FoldPlan {
        fold_count,
        held_out,
        training,
    })
}

// ===========================================================================
// 先验 / 子集 / 求解辅助
// ===========================================================================

/// 先验 FIR 复响应（M*M*grid；零先验返回 None）。
fn prior_fir_responses(
    prior: &RegularizationPrior,
    problem: &SolveProblem,
) -> Result<Option<Vec<C>>, RegularizeError> {
    if prior.is_zero() {
        return Ok(None);
    }
    let m = problem.m();
    let taps = prior.taps;
    let grid_len = problem.grid_hz.len();
    let fs = problem.sample_rate_hz as f64;
    if prior.coeffs.len() != m * m * taps {
        return Err(err(
            RegularizeErrorKind::Prior,
            "regularize.prior.coeffs",
            "prior coefficient count does not match M*M*taps",
        ));
    }
    let mut responses = vec![(0.0, 0.0); m * m * grid_len];
    for out_dense in 0..m {
        for in_dense in 0..m {
            let base = (out_dense * m + in_dense) * taps;
            let cell_base = (out_dense * m + in_dense) * grid_len;
            for (grid_index, freq) in problem.grid_hz.iter().copied().enumerate() {
                let angle = -2.0 * std::f64::consts::PI * freq / fs;
                let step = (angle.cos(), angle.sin());
                let mut acc = (0.0, 0.0);
                let mut power = (1.0, 0.0);
                for tap in 0..taps {
                    let coefficient = prior.coeffs[base + tap];
                    acc.0 += coefficient * power.0;
                    acc.1 += coefficient * power.1;
                    power = cmul(power, step);
                }
                responses[cell_base + grid_index] = acc;
            }
        }
    }
    Ok(Some(responses))
}

/// 构造子集问题；冻结先验时把 target 变换为 d - H*g_prior 并切到 Custom 模式。
fn subset_problem(
    problem: &SolveProblem,
    indices: &[usize],
    prior_response: Option<&[C]>,
) -> Result<SolveProblem, RegularizeError> {
    let m = problem.m();
    let grid_len = problem.grid_hz.len();
    let mut positions = Vec::with_capacity(indices.len());
    for &index in indices {
        let source = problem.positions.get(index).ok_or_else(|| {
            err(
                RegularizeErrorKind::Missing,
                format!("regularize.positions[{index}]"),
                "position index out of range",
            )
        })?;
        if let Some(prior_resp) = prior_response {
            let mut target_rows: Vec<Vec<ComplexPoint>> = Vec::with_capacity(m);
            for input_dense in 0..m {
                let mut points = Vec::with_capacity(grid_len);
                for (grid_index, freq) in problem.grid_hz.iter().copied().enumerate() {
                    let mut predicted = (0.0, 0.0);
                    for output_dense in 0..m {
                        let h = source.h_eff[output_dense][grid_index];
                        let prior =
                            prior_resp[(output_dense * m + input_dense) * grid_len + grid_index];
                        predicted = cadd(predicted, cmul((h.re, h.im), prior));
                    }
                    let desired = source.target[input_dense][grid_index];
                    points.push(ComplexPoint {
                        freq_hz: freq,
                        re: desired.re - predicted.0,
                        im: desired.im - predicted.1,
                    });
                }
                target_rows.push(points);
            }
            positions.push(SolvePosition {
                label: source.label.clone(),
                weight: source.weight,
                h_eff: source.h_eff.clone(),
                target: target_rows,
            });
        } else {
            positions.push(source.clone());
        }
    }
    let mode = if prior_response.is_some() {
        SolveMode::Custom
    } else {
        problem.mode
    };
    Ok(SolveProblem {
        roles: problem.roles.clone(),
        grid_hz: problem.grid_hz.clone(),
        sample_rate_hz: problem.sample_rate_hz,
        positions,
        mode,
        target_position_variant: problem.target_position_variant,
        target_delay_samples: problem.target_delay_samples,
        provenance: problem.provenance.clone(),
    })
}

/// 由 base 选项 + 候选 lambda/掩码构造 T51 solve 选项。
fn candidate_solve_options(base: &SolveOptions, lambda_used: f64, mask: &[bool]) -> SolveOptions {
    let mut options = base.clone();
    options.ridge = lambda_used;
    options.path_mask = Some(mask.to_vec());
    options
}

/// 由 base 解 + 先验物化最终解（系数相加；G=deltaD*I+g 复响应由最终系数重算）。
fn materialize(
    problem: &SolveProblem,
    options: &RegularizeOptions,
    prior: &RegularizationPrior,
    base: &SolveSolution,
) -> Result<RegularizedSolution, RegularizeError> {
    let m = problem.m();
    let taps = options.base.taps;
    let grid_len = problem.grid_hz.len();
    let fs = problem.sample_rate_hz as f64;
    let expected = m * m * taps;
    if base.coeffs_f64.len() != expected {
        return Err(err(
            RegularizeErrorKind::Upstream,
            "regularize.materialize.base.coeffs",
            format!(
                "base solve coefficients length {} != M*M*taps={expected}",
                base.coeffs_f64.len()
            ),
        ));
    }
    let mut coeffs = Vec::with_capacity(expected);
    if prior.is_zero() {
        coeffs.extend_from_slice(&base.coeffs_f64);
    } else {
        if prior.coeffs.len() != expected {
            return Err(err(
                RegularizeErrorKind::Prior,
                "regularize.prior.coeffs",
                format!(
                    "prior coefficient count {} != M*M*taps={expected}",
                    prior.coeffs.len()
                ),
            ));
        }
        for index in 0..expected {
            coeffs.push(prior.coeffs[index] + base.coeffs_f64[index]);
        }
    }
    let mut cells = Vec::with_capacity(m * m);
    for out_dense in 0..m {
        for in_dense in 0..m {
            let base_index = (out_dense * m + in_dense) * taps;
            let mut points = Vec::with_capacity(grid_len);
            for freq in problem.grid_hz.iter().copied() {
                let angle = -2.0 * std::f64::consts::PI * freq / fs;
                let step = (angle.cos(), angle.sin());
                let mut acc = (0.0, 0.0);
                let mut power = (1.0, 0.0);
                for tap in 0..taps {
                    let coefficient = coeffs[base_index + tap];
                    acc.0 += coefficient * power.0;
                    acc.1 += coefficient * power.1;
                    power = cmul(power, step);
                }
                if out_dense == in_dense {
                    let delay_angle = angle * options.base.delay_samples as f64;
                    acc.0 += delay_angle.cos();
                    acc.1 += delay_angle.sin();
                }
                points.push(ComplexPoint {
                    freq_hz: freq,
                    re: acc.0,
                    im: acc.1,
                });
            }
            cells.push(ModalMatrixCell {
                out_dense: out_dense as u16,
                in_dense: in_dense as u16,
                points,
            });
        }
    }
    let modal = ModalMatrix::new(problem.roles.clone(), problem.grid_hz.clone(), cells)
        .map_err(map_target_error)?;
    let coeffs_f32: Vec<f32> = coeffs.iter().map(|value| *value as f32).collect();
    let coeffs_sha256 = coefficient_sha256(&coeffs);
    let base_solve_hash = Some(base.hash.clone());
    let mut canonical = String::new();
    canonical.push_str(REGULARIZE_HASH_DOMAIN);
    canonical.push('\n');
    canonical.push_str(&format!("problem_hash={}\n", problem.hash()));
    canonical.push_str(&format!("prior_hash={}\n", prior.hash()));
    canonical.push_str(&format!(
        "base_solve_hash={}\n",
        base_solve_hash.as_deref().unwrap_or("none")
    ));
    canonical.push_str(&format!("coeffs_sha256={coeffs_sha256}\n"));
    let hash = sha256_hex(canonical.as_bytes());
    Ok(RegularizedSolution {
        modal,
        coeffs_f64: coeffs,
        coeffs_f32,
        coeffs_sha256,
        hash,
        base_solve_hash,
        prior_hash: prior.hash().to_string(),
    })
}

fn coefficient_sha256(coeffs: &[f64]) -> String {
    let mut text = String::new();
    text.push_str(REGULARIZE_HASH_DOMAIN);
    text.push('\n');
    text.push_str("coeffs\n");
    for value in coeffs {
        text.push_str(&fmt_f64(*value));
        text.push('\n');
    }
    sha256_hex(text.as_bytes())
}

// ===========================================================================
// 残差 / headroom 评估（全部由最终 G 实算，不用预测模型代替）
// ===========================================================================

#[derive(Debug, Clone, PartialEq)]
struct CellResidual {
    position_label: String,
    input_dense: usize,
    max_db: f64,
    points: usize,
    near_zero: usize,
}

fn predict_diff(
    problem: &SolveProblem,
    solution: &RegularizedSolution,
    position_index: usize,
    input_dense: usize,
    grid_index: usize,
) -> ((f64, f64), f64, f64) {
    let m = problem.m();
    let position = &problem.positions[position_index];
    let mut predicted = (0.0, 0.0);
    for output_dense in 0..m {
        let h = position.h_eff[output_dense][grid_index];
        let gain = solution
            .modal
            .cell(output_dense as u16, input_dense as u16)
            .expect("materialized modal must contain every cell")[grid_index];
        predicted.0 += h.re * gain.re - h.im * gain.im;
        predicted.1 += h.re * gain.im + h.im * gain.re;
    }
    let desired = position.target[input_dense][grid_index];
    let diff = (predicted.0 - desired.re, predicted.1 - desired.im);
    let diff_mag = cabs(diff);
    let desired_mag = cabs((desired.re, desired.im));
    (diff, diff_mag, desired_mag)
}

fn evaluate_cells(
    problem: &SolveProblem,
    indices: &[usize],
    solution: &RegularizedSolution,
    band: &[usize],
) -> Vec<CellResidual> {
    let m = problem.m();
    let mut cells = Vec::with_capacity(indices.len() * m);
    for &position_index in indices {
        let label = problem.positions[position_index].label.clone();
        for input_dense in 0..m {
            let mut max_db = f64::NEG_INFINITY;
            let mut points = 0usize;
            let mut near_zero = 0usize;
            for &grid_index in band {
                let (_, diff_mag, desired_mag) =
                    predict_diff(problem, solution, position_index, input_dense, grid_index);
                if desired_mag < NEAR_ZERO_MAGNITUDE {
                    near_zero += 1;
                    continue;
                }
                max_db = max_db.max(ratio_db(diff_mag, desired_mag));
                points += 1;
            }
            cells.push(CellResidual {
                position_label: label.clone(),
                input_dense,
                max_db,
                points,
                near_zero,
            });
        }
    }
    cells
}

fn evaluate_design(
    problem: &SolveProblem,
    solution: &RegularizedSolution,
    fit_indices: &[usize],
    improvement_indices: &[usize],
    frequency_weights: Option<&[f64]>,
) -> DesignResidualReport {
    let m = problem.m();
    let mut residual_norm_sq = 0.0f64;
    let mut fit_max = 0.0f64;
    let mut improvement_max = 0.0f64;
    let mut worst: Option<(String, String, f64)> = None;
    let mut near_zero_points = 0usize;
    let mut valid_points = 0usize;
    for (position_index, position) in problem.positions.iter().enumerate() {
        for input_dense in 0..m {
            let mut cell_fit_max = 0.0f64;
            let mut cell_improvement_max = 0.0f64;
            for &grid_index in fit_indices {
                let (diff, diff_mag, desired_mag) =
                    predict_diff(problem, solution, position_index, input_dense, grid_index);
                let weight = position.weight
                    * frequency_weights
                        .map(|weights| weights[grid_index])
                        .unwrap_or(1.0);
                residual_norm_sq += weight * (diff.0 * diff.0 + diff.1 * diff.1);
                if desired_mag < NEAR_ZERO_MAGNITUDE {
                    near_zero_points += 1;
                    continue;
                }
                valid_points += 1;
                let db = ratio_db(diff_mag, desired_mag);
                cell_fit_max = cell_fit_max.max(db);
                fit_max = fit_max.max(db);
            }
            for &grid_index in improvement_indices {
                let (_, diff_mag, desired_mag) =
                    predict_diff(problem, solution, position_index, input_dense, grid_index);
                if desired_mag < NEAR_ZERO_MAGNITUDE {
                    continue;
                }
                let db = ratio_db(diff_mag, desired_mag);
                cell_improvement_max = cell_improvement_max.max(db);
                improvement_max = improvement_max.max(db);
            }
            if worst
                .as_ref()
                .map(|current| cell_improvement_max > current.2)
                .unwrap_or(cell_improvement_max > 0.0)
            {
                worst = Some((
                    position.label.clone(),
                    problem.roles[input_dense].clone(),
                    cell_improvement_max,
                ));
            }
        }
    }
    DesignResidualReport {
        fit_band_max_db: fit_max,
        improvement_band_max_db: improvement_max,
        residual_norm: residual_norm_sq.sqrt(),
        worst,
        near_zero_points,
        valid_points,
    }
}

fn evaluate_headroom(
    problem: &SolveProblem,
    solution: &RegularizedSolution,
    improvement_indices: &[usize],
    spec: &HeadroomSpec,
) -> HeadroomReport {
    let m = problem.m();
    let mut paths = Vec::with_capacity(m * m);
    for out_dense in 0..m {
        for in_dense in 0..m {
            let points = solution
                .modal
                .cell(out_dense as u16, in_dense as u16)
                .expect("materialized modal must contain every cell");
            let mut max_gain_db = f64::NEG_INFINITY;
            for &grid_index in improvement_indices {
                let point = points[grid_index];
                max_gain_db = max_gain_db.max(magnitude_db_of(cabs((point.re, point.im))));
            }
            let is_correction = out_dense != in_dense;
            let cap_db = if is_correction {
                spec.correction_cap_db
            } else {
                spec.diagonal_cap_db
            };
            let margin_db = cap_db + spec.tolerance_db - max_gain_db;
            paths.push(PathHeadroom {
                out_dense: out_dense as u16,
                in_dense: in_dense as u16,
                out_role: problem.roles[out_dense].clone(),
                in_role: problem.roles[in_dense].clone(),
                is_correction,
                max_gain_db,
                cap_db,
                margin_db,
                passed: margin_db >= 0.0,
            });
        }
    }
    let max_correction_gain_db = paths
        .iter()
        .filter(|path| path.is_correction)
        .map(|path| path.max_gain_db)
        .fold(f64::NEG_INFINITY, f64::max);
    let max_diagonal_gain_db = paths
        .iter()
        .filter(|path| !path.is_correction)
        .map(|path| path.max_gain_db)
        .fold(f64::NEG_INFINITY, f64::max);
    let worst_excess_db = paths
        .iter()
        .map(|path| (-path.margin_db).max(0.0))
        .fold(0.0f64, f64::max);
    HeadroomReport {
        correction_cap_db: spec.correction_cap_db,
        diagonal_cap_db: spec.diagonal_cap_db,
        tolerance_db: spec.tolerance_db,
        band_hz: IMPROVEMENT_BAND_HZ,
        paths,
        max_correction_gain_db: if max_correction_gain_db.is_finite() {
            max_correction_gain_db
        } else {
            0.0
        },
        max_diagonal_gain_db: if max_diagonal_gain_db.is_finite() {
            max_diagonal_gain_db
        } else {
            0.0
        },
        worst_excess_db,
        passed: worst_excess_db <= 0.0,
        note: HEADROOM_NOTE,
    }
}

// ===========================================================================
// 完整候选求解 + 缓存
// ===========================================================================

#[derive(Debug, Clone, PartialEq)]
struct FullCandidate {
    status: &'static str,
    solution: RegularizedSolution,
    headroom: HeadroomReport,
    design: DesignResidualReport,
    delta_norm: f64,
}

#[derive(Debug, Clone, PartialEq)]
struct FullCacheEntry {
    whitelist: WhitelistKind,
    lambda: f64,
    candidate: FullCandidate,
}

#[allow(clippy::too_many_arguments)]
fn full_candidate_cached(
    cache: &mut Vec<FullCacheEntry>,
    problem: &SolveProblem,
    options: &RegularizeOptions,
    prior: &RegularizationPrior,
    prior_response: Option<&[C]>,
    whitelist: WhitelistKind,
    mask: &[bool],
    lambda: f64,
    lambda_used: f64,
    fit_indices: &[usize],
    improvement_indices: &[usize],
    all_indices: &[usize],
) -> Result<FullCandidate, RegularizeError> {
    if let Some(entry) = cache
        .iter()
        .find(|entry| entry.whitelist == whitelist && entry.lambda == lambda)
    {
        return Ok(entry.candidate.clone());
    }
    let full_problem = subset_problem(problem, all_indices, prior_response)?;
    let solve_options = candidate_solve_options(&options.base, lambda_used, mask);
    let outcome = solve(&full_problem, &solve_options).map_err(map_solve_error)?;
    let base_solution = outcome.solution.as_ref().ok_or_else(|| {
        err(
            RegularizeErrorKind::Upstream,
            "regularize.solution",
            format!(
                "T51 solve returned no solution (status={}); resource limits must be raised by the caller, not silently truncated",
                outcome.report.status.as_str()
            ),
        )
    })?;
    let solution = materialize(problem, options, prior, base_solution)?;
    let headroom = evaluate_headroom(problem, &solution, improvement_indices, &options.headroom);
    let design = evaluate_design(
        problem,
        &solution,
        fit_indices,
        improvement_indices,
        options.base.frequency_weights.as_deref(),
    );
    let delta_norm = base_solution
        .coeffs_f64
        .iter()
        .map(|value| value * value)
        .sum::<f64>()
        .sqrt();
    let candidate = FullCandidate {
        status: outcome.report.status.as_str(),
        solution,
        headroom,
        design,
        delta_norm,
    };
    cache.push(FullCacheEntry {
        whitelist,
        lambda,
        candidate: candidate.clone(),
    });
    Ok(candidate)
}

// ===========================================================================
// L-curve
// ===========================================================================

fn compute_curvatures(points: &mut [LambdaPoint]) {
    let positive: Vec<usize> = points
        .iter()
        .enumerate()
        .filter(|(_, point)| point.lambda > 0.0)
        .map(|(index, _)| index)
        .collect();
    if positive.len() < L_CURVE_MIN_POINTS {
        return;
    }
    for window in 1..positive.len() - 1 {
        let previous = positive[window - 1];
        let current = positive[window];
        let next = positive[window + 1];
        let residuals = [
            points[previous].design_residual_norm,
            points[current].design_residual_norm,
            points[next].design_residual_norm,
        ];
        let norms = [
            points[previous].solution_norm,
            points[current].solution_norm,
            points[next].solution_norm,
        ];
        if residuals
            .iter()
            .chain(norms.iter())
            .any(|value| !value.is_finite() || *value <= REGULARIZE_DB_FLOOR)
        {
            continue;
        }
        let x: Vec<f64> = residuals.iter().map(|value| value.log10()).collect();
        let y: Vec<f64> = norms.iter().map(|value| value.log10()).collect();
        let x_prime = 0.5 * (x[2] - x[0]);
        let y_prime = 0.5 * (y[2] - y[0]);
        let x_second = x[2] - 2.0 * x[1] + x[0];
        let y_second = y[2] - 2.0 * y[1] + y[0];
        let denominator = (x_prime * x_prime + y_prime * y_prime).powf(1.5);
        if !denominator.is_finite() || denominator <= 0.0 {
            continue;
        }
        let curvature = ((x_prime * y_second - y_prime * x_second) / denominator).abs();
        if curvature.is_finite() {
            points[current].curvature = Some(curvature);
        }
    }
}

fn l_curve_corner(points: &[LambdaPoint]) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (index, point) in points.iter().enumerate() {
        if let Some(curvature) = point.curvature {
            if best.map(|(_, value)| curvature > value).unwrap_or(true) {
                best = Some((index, curvature));
            }
        }
    }
    best.map(|(index, _)| index)
}

/// 对给定白名单扫描全部 lambda 网格点（复用完整候选缓存），返回扫描点与 L-curve 拐点索引。
///
/// 扫描只依赖白名单与 lambda 网格；headroom 回退把最终白名单改掉时用同 helper 按最终白名单重算，
/// 保证 lambda_scan.scan_whitelist 与 selection.selected_whitelist 自洽。
#[allow(clippy::too_many_arguments)]
fn scan_lambda_points(
    cache: &mut Vec<FullCacheEntry>,
    problem: &SolveProblem,
    options: &RegularizeOptions,
    prior: &RegularizationPrior,
    prior_response: Option<&[C]>,
    whitelist: WhitelistKind,
    mask: &[bool],
    lambda_scale: f64,
    fit_indices: &[usize],
    improvement_indices: &[usize],
    all_indices: &[usize],
) -> Result<(Vec<LambdaPoint>, Option<usize>), RegularizeError> {
    let mut points: Vec<LambdaPoint> = Vec::new();
    for lambda in options.lambda_grid.iter().copied() {
        let lambda_used = lambda * lambda_scale;
        let full = full_candidate_cached(
            cache,
            problem,
            options,
            prior,
            prior_response,
            whitelist,
            mask,
            lambda,
            lambda_used,
            fit_indices,
            improvement_indices,
            all_indices,
        )?;
        points.push(LambdaPoint {
            lambda,
            lambda_used,
            design_residual_norm: full.design.residual_norm,
            solution_norm: full.delta_norm,
            fit_band_max_db: full.design.fit_band_max_db,
            improvement_band_max_db: full.design.improvement_band_max_db,
            curvature: None,
            solve_status: full.status,
            headroom_feasible: full.headroom.passed,
            headroom_worst_excess_db: full.headroom.worst_excess_db,
        });
    }
    compute_curvatures(&mut points);
    let corner = l_curve_corner(&points);
    Ok((points, corner))
}

// ===========================================================================
// CV / 选择
// ===========================================================================

/// 容差带偏好键（唯一来源）：更少自由路径 → 更大 lambda_used → 白名单枚举序 → lambda 网格序。
///
/// `compare_candidates` 与 `prefer_candidate` 都从这里派生，避免两套排序键漂移（O-2）。
fn compare_preference(left: &CandidateReport, right: &CandidateReport) -> std::cmp::Ordering {
    left.free_paths
        .cmp(&right.free_paths)
        .then(
            right
                .lambda_used
                .partial_cmp(&left.lambda_used)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
        .then(left.whitelist.order().cmp(&right.whitelist.order()))
        .then(
            left.lambda
                .partial_cmp(&right.lambda)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
}

fn compare_candidates(left: &CandidateReport, right: &CandidateReport) -> std::cmp::Ordering {
    let left_mean = left.cv_mean_db.unwrap_or(f64::INFINITY);
    let right_mean = right.cv_mean_db.unwrap_or(f64::INFINITY);
    left_mean
        .partial_cmp(&right_mean)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| compare_preference(left, right))
}

/// 容差带内偏好：更少自由路径 → 更大 lambda_used → 白名单枚举序 → lambda 网格序。
fn prefer_candidate(left: &CandidateReport, right: &CandidateReport) -> bool {
    compare_preference(left, right) == std::cmp::Ordering::Less
}

/// 对有效候选排名并返回 (选中索引, 是否 tie-break)。
fn rank_and_select(candidates: &mut [CandidateReport], tolerance_db: f64) -> Option<(usize, bool)> {
    let mut valid: Vec<usize> = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| candidate.cv_valid && candidate.cv_mean_db.is_some())
        .map(|(index, _)| index)
        .collect();
    if valid.is_empty() {
        return None;
    }
    valid.sort_by(|left, right| compare_candidates(&candidates[*left], &candidates[*right]));
    for (rank, index) in valid.iter().enumerate() {
        candidates[*index].cv_rank = Some(rank + 1);
    }
    let best = valid[0];
    let best_mean = candidates[best].cv_mean_db.unwrap_or(f64::INFINITY);
    let mut selected = best;
    for index in valid.iter().copied().skip(1) {
        let mean = candidates[index].cv_mean_db.unwrap_or(f64::INFINITY);
        if mean > best_mean + tolerance_db {
            break;
        }
        if prefer_candidate(&candidates[index], &candidates[selected]) {
            selected = index;
        }
    }
    Some((selected, selected != best))
}

/// CV 无效原因（报告用）。
fn cv_invalid_reason(fold_plan: &FoldPlan, positions: usize, m: usize) -> &'static str {
    if fold_plan.fold_count < 2 || positions < 2 {
        "insufficient_design_positions"
    } else if positions < m {
        "p_below_m"
    } else if positions == m {
        "p_equals_m_boundary"
    } else {
        "no_valid_fold"
    }
}

/// 回退白名单：优先 FIR 可辨识必要条件成立且自由路径最少的候选。
fn fallback_whitelist(
    problem: &SolveProblem,
    options: &RegularizeOptions,
    band_points: usize,
) -> Result<WhitelistKind, RegularizeError> {
    let m = problem.m();
    let positions = problem.positions.len();
    let mut chosen: Option<(usize, usize)> = None;
    for (order, whitelist) in options.whitelists.iter().copied().enumerate() {
        let free_paths = whitelist.free_path_count(m, &problem.roles)?;
        let applicability = candidate_applicability(
            whitelist,
            free_paths,
            positions,
            band_points,
            options.base.taps,
        );
        if !applicability.fir_rank_necessary {
            continue;
        }
        if chosen.map(|(paths, _)| free_paths < paths).unwrap_or(true) {
            chosen = Some((free_paths, order));
        }
    }
    match chosen {
        Some((_, order)) => Ok(options.whitelists[order]),
        None => Ok(options.whitelists[0]),
    }
}

// ===========================================================================
// 入口：regularize
// ===========================================================================

/// 正则入口：design 内 CV + 固定白名单 + lambda/L-curve + headroom/先验 + P>=M 边界。
///
/// 不 panic、不静默截断：非法输入 → RegularizeError；上游资源受限 → RegularizeError；
/// CV 不可用/headroom 不可行 → 回退并如实报告（best-effort 解 + deliverable=false）。
pub fn regularize(
    problem: &SolveProblem,
    options: &RegularizeOptions,
) -> Result<RegularizeOutcome, RegularizeError> {
    options.validate(problem)?;
    let m = problem.m();
    let taps = options.base.taps;
    let positions = problem.positions.len();
    let fit_indices = fit_band_indices(problem, options.base.fit_band_hz)?;
    let band_points = fit_indices.len();
    let improvement_indices = band_indices(problem, IMPROVEMENT_BAND_HZ.0, IMPROVEMENT_BAND_HZ.1);
    if improvement_indices.is_empty() {
        return Err(err(
            RegularizeErrorKind::Problem,
            "regularize.improvement_band_hz",
            format!(
                "grid has no points in {:?} Hz; CV scoring/headroom would be undefined",
                IMPROVEMENT_BAND_HZ
            ),
        ));
    }
    let all_indices: Vec<usize> = (0..positions).collect();
    let applicability = applicability_boundary(problem);

    let mut warnings: Vec<RegularizeWarning> = Vec::new();
    let raw_lambda_scale = compute_lambda_scale(problem, &fit_indices, m);
    let lambda_scale = if raw_lambda_scale <= LAMBDA_SCALE_FLOOR {
        push_warning(
            &mut warnings,
            RegularizeWarning::DEGENERATE_PLANT,
            format!(
                "mean|H|^2={raw_lambda_scale:.3e} is below the lambda scale floor {LAMBDA_SCALE_FLOOR:.1e}; lambda scaling uses 1.0 (plant is degenerate)"
            ),
        );
        1.0
    } else {
        raw_lambda_scale
    };
    let max_lambda_used = options
        .lambda_grid
        .iter()
        .copied()
        .map(|lambda| lambda * lambda_scale)
        .fold(0.0f64, f64::max);
    if max_lambda_used > MAX_LAMBDA {
        return Err(err(
            RegularizeErrorKind::Lambda,
            "regularize.options.lambda_grid",
            format!(
                "max lambda_used={max_lambda_used:.3e} exceeds T51 MAX_SOLVE_RIDGE={MAX_LAMBDA:.1e}; reduce the lambda grid or the plant scale is incompatible"
            ),
        ));
    }
    if !applicability.p_ge_m {
        push_warning(
            &mut warnings,
            RegularizeWarning::UNDERDETERMINED,
            format!(
                "P={} design positions < M={m}: full-matrix system is underdetermined; minimum-norm G is non-unique and deliverable=false",
                positions
            ),
        );
    }
    if problem.mode == SolveMode::Preserve {
        return regularize_preserve(
            problem,
            options,
            applicability,
            lambda_scale,
            fit_indices,
            improvement_indices,
            warnings,
        );
    }
    if positions < 2 {
        push_warning(
            &mut warnings,
            RegularizeWarning::INSUFFICIENT_POSITIONS,
            format!("only {positions} design position(s): design-internal CV cannot be formed"),
        );
    }
    let prior = &options.prior;
    if !prior.is_zero() {
        push_warning(
            &mut warnings,
            RegularizeWarning::PRIOR_FROZEN,
            format!(
                "frozen prior hash={} used: solution is biased toward g_prior and residuals need not reach zero",
                prior.hash()
            ),
        );
    }
    let prior_response = prior_fir_responses(prior, problem)?;
    let prior_response_slice = prior_response.as_deref();

    // ---- 固定白名单掩码（结构固定；不读测量数据） ----
    let mut masks: Vec<Vec<bool>> = Vec::with_capacity(options.whitelists.len());
    for whitelist in options.whitelists.iter().copied() {
        masks.push(whitelist.mask(m, &problem.roles)?);
    }

    // ---- design 内 CV ----
    let fold_plan = build_folds(positions, options.cv_folds)?;
    let mut candidates: Vec<CandidateReport> = Vec::new();
    for (whitelist_index, whitelist) in options.whitelists.iter().copied().enumerate() {
        let mask = &masks[whitelist_index];
        let free_paths = mask.iter().filter(|free| **free).count();
        let applicability =
            candidate_applicability(whitelist, free_paths, positions, band_points, taps);
        for lambda in options.lambda_grid.iter().copied() {
            let lambda_used = lambda * lambda_scale;
            let mut folds: Vec<FoldReport> = Vec::new();
            let mut fold_underdetermined = false;
            let mut headroom_feasible_in_folds = true;
            let mut headroom_worst_excess_in_folds_db = 0.0f64;
            for fold_index in 0..fold_plan.fold_count {
                let training = &fold_plan.training[fold_index];
                let held_out = &fold_plan.held_out[fold_index];
                let training_problem = subset_problem(problem, training, prior_response_slice)?;
                let solve_options = candidate_solve_options(&options.base, lambda_used, mask);
                let outcome = solve(&training_problem, &solve_options).map_err(|error| {
                    err(
                        RegularizeErrorKind::Upstream,
                        format!("regularize.cv.fold[{fold_index}].solve"),
                        format!("{}: {} ({})", error.code(), error.detail(), error.field()),
                    )
                })?;
                let status = outcome.report.status;
                let underdetermined = outcome.report.identifiability.underdetermined;
                fold_underdetermined |= underdetermined;
                let iterations = outcome.report.iterations_total;
                let (
                    mean_residual_db,
                    worst_residual_db,
                    valid_points,
                    near_zero_points,
                    fold_headroom_feasible,
                    fold_headroom_worst,
                ) = match &outcome.solution {
                    Some(base_solution) => {
                        let final_solution = materialize(problem, options, prior, base_solution)?;
                        let cells = evaluate_cells(
                            problem,
                            held_out,
                            &final_solution,
                            &improvement_indices,
                        );
                        let mut sum = 0.0f64;
                        let mut count = 0usize;
                        let mut worst = 0.0f64;
                        let mut points = 0usize;
                        let mut near_zero = 0usize;
                        for cell in &cells {
                            points += cell.points;
                            near_zero += cell.near_zero;
                            if cell.points == 0 {
                                continue;
                            }
                            sum += cell.max_db;
                            count += 1;
                            worst = worst.max(cell.max_db);
                        }
                        let mean = if count > 0 {
                            sum / count as f64
                        } else {
                            f64::NAN
                        };
                        let headroom = evaluate_headroom(
                            problem,
                            &final_solution,
                            &improvement_indices,
                            &options.headroom,
                        );
                        (
                            mean,
                            if count > 0 { worst } else { f64::NAN },
                            points,
                            near_zero,
                            headroom.passed,
                            headroom.worst_excess_db,
                        )
                    }
                    None => (f64::NAN, f64::NAN, 0, 0, false, f64::NAN),
                };
                let usable = status.is_usable() && valid_points > 0;
                headroom_feasible_in_folds &= fold_headroom_feasible;
                if fold_headroom_worst.is_finite() {
                    headroom_worst_excess_in_folds_db =
                        headroom_worst_excess_in_folds_db.max(fold_headroom_worst);
                }
                folds.push(FoldReport {
                    fold_index,
                    held_out_labels: held_out
                        .iter()
                        .map(|index| problem.positions[*index].label.clone())
                        .collect(),
                    training_positions: training.len(),
                    solve_status: status.as_str(),
                    usable,
                    underdetermined,
                    iterations,
                    mean_residual_db,
                    worst_residual_db,
                    valid_points,
                    near_zero_points,
                    headroom_feasible: fold_headroom_feasible,
                    headroom_worst_excess_db: if fold_headroom_worst.is_finite() {
                        fold_headroom_worst
                    } else {
                        0.0
                    },
                });
            }
            let fold_count = folds.len();
            let all_usable = fold_count >= 2 && folds.iter().all(|fold| fold.usable);
            let cv_valid = all_usable && !fold_underdetermined;
            let (cv_mean_db, cv_worst_db) = if cv_valid {
                let mean =
                    folds.iter().map(|fold| fold.mean_residual_db).sum::<f64>() / fold_count as f64;
                let worst = folds
                    .iter()
                    .map(|fold| fold.worst_residual_db)
                    .fold(f64::NEG_INFINITY, f64::max);
                (Some(mean), Some(worst))
            } else {
                (None, None)
            };
            candidates.push(CandidateReport {
                whitelist,
                free_paths,
                lambda,
                lambda_used,
                cv_mean_db,
                cv_worst_db,
                cv_valid,
                cv_rank: None,
                fold_underdetermined,
                headroom_feasible_in_folds,
                headroom_worst_excess_in_folds_db,
                applicability: applicability.clone(),
                folds,
            });
        }
    }

    let cv_selection = rank_and_select(&mut candidates, options.selection_tolerance_db);
    // 容差带实际选中的候选（tie-break 生效时 != 严格 cv_mean 最小候选）；它是 headroom 首选尝试项。
    let cv_selected = cv_selection.map(|(index, _)| {
        let candidate = &candidates[index];
        (candidate.whitelist, candidate.lambda)
    });
    // CV 阶段是否发生容差带 tie-break；最终 tie_break_used 还要看该选择是否真的落在最终解上。
    let mut tie_break_engaged = false;
    let mut cv_valid = cv_selection.is_some();
    let mut cv_reason: Option<&'static str> = None;
    let (
        mut scan_whitelist,
        mut initial_lambda,
        selection_method,
        selected_cv_rank,
        selected_cv_mean,
    ) = match cv_selection {
        Some((index, tie_break)) => {
            tie_break_engaged = tie_break;
            let candidate = &candidates[index];
            (
                candidate.whitelist,
                candidate.lambda,
                SELECTION_METHOD_CV,
                candidate.cv_rank,
                candidate.cv_mean_db,
            )
        }
        None => {
            cv_valid = false;
            let reason = cv_invalid_reason(&fold_plan, positions, m);
            cv_reason = Some(reason);
            push_warning(
                &mut warnings,
                if positions == m {
                    RegularizeWarning::CV_BOUNDARY
                } else {
                    RegularizeWarning::CV_INVALID
                },
                format!(
                    "design-internal CV is invalid ({reason}); selection falls back to the L-curve corner and is marked as fallback"
                ),
            );
            let whitelist = fallback_whitelist(problem, options, band_points)?;
            (whitelist, f64::NAN, SELECTION_METHOD_L_CURVE, None, None)
        }
    };
    let mask_index = |whitelist: WhitelistKind| -> usize {
        options
            .whitelists
            .iter()
            .position(|candidate| *candidate == whitelist)
            .expect("scan whitelist must be one of the configured candidates")
    };
    let mut cache: Vec<FullCacheEntry> = Vec::new();
    let (mut scan_points, mut corner_index) = scan_lambda_points(
        &mut cache,
        problem,
        options,
        prior,
        prior_response_slice,
        scan_whitelist,
        &masks[mask_index(scan_whitelist)],
        lambda_scale,
        &fit_indices,
        &improvement_indices,
        &all_indices,
    )?;
    if corner_index.is_none() {
        let positive_points = scan_points
            .iter()
            .filter(|point| point.lambda > 0.0)
            .count();
        push_warning(
            &mut warnings,
            if positive_points < L_CURVE_MIN_POINTS {
                RegularizeWarning::L_CURVE_UNAVAILABLE
            } else {
                RegularizeWarning::L_CURVE_FLAT
            },
            format!(
                "L-curve corner is not resolvable (positive lambda points={positive_points}, computed curvatures={}); lambda choice stays CV-driven or falls back to the smallest-residual scan point",
                scan_points
                    .iter()
                    .filter(|point| point.curvature.is_some())
                    .count()
            ),
        );
    }
    let mut l_curve_corner_lambda = corner_index.map(|index| scan_points[index].lambda);
    if !cv_valid {
        initial_lambda = match corner_index {
            Some(index) => scan_points[index].lambda,
            None => scan_points
                .iter()
                .filter(|point| point.lambda > 0.0 || scan_points.len() == 1)
                .min_by(|left, right| {
                    left.design_residual_norm
                        .partial_cmp(&right.design_residual_norm)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|point| point.lambda)
                .unwrap_or(options.lambda_grid[0]),
        };
    }
    let lambda_at_edge = scan_points
        .iter()
        .position(|point| point.lambda == initial_lambda)
        .map(|index| index == 0 || index + 1 == scan_points.len())
        .unwrap_or(false);
    if lambda_at_edge {
        push_warning(
            &mut warnings,
            RegularizeWarning::LAMBDA_AT_EDGE,
            format!(
                "selected lambda={initial_lambda} sits at a scan-grid edge; regularization may be under/over-determined (reported, not silently changed)"
            ),
        );
    }

    // ---- 候选顺序（CV 排序或 L-curve 残差排序），headroom 有界回退 ----
    let mut ordered: Vec<(WhitelistKind, f64)> = Vec::new();
    if cv_valid {
        // 容差带选中项（可能不是严格 cv_mean 最小候选）必须第一个吃 headroom；
        // 否则严格最小候选先通过 headroom 会把 tie-break 选择静默丢弃（P2-1）。
        if let Some(selected) = cv_selected {
            ordered.push(selected);
        }
        let mut valid: Vec<&CandidateReport> = candidates
            .iter()
            .filter(|candidate| candidate.cv_valid)
            .collect();
        valid.sort_by(|left, right| compare_candidates(left, right));
        for candidate in valid {
            if Some((candidate.whitelist, candidate.lambda)) == cv_selected {
                continue;
            }
            ordered.push((candidate.whitelist, candidate.lambda));
        }
    } else {
        let mut scan_order: Vec<&LambdaPoint> = scan_points.iter().collect();
        scan_order.sort_by(|left, right| {
            left.design_residual_norm
                .partial_cmp(&right.design_residual_norm)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(
                    left.lambda
                        .partial_cmp(&right.lambda)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
        });
        for point in scan_order {
            ordered.push((scan_whitelist, point.lambda));
        }
        for whitelist in options.whitelists.iter().copied() {
            if whitelist == scan_whitelist {
                continue;
            }
            for lambda in options.lambda_grid.iter().copied() {
                ordered.push((whitelist, lambda));
            }
        }
    }
    let mut chosen: Option<(usize, FullCandidate)> = None;
    let mut best_fallback: Option<(usize, FullCandidate)> = None;
    for (attempt, (whitelist, lambda)) in ordered
        .iter()
        .copied()
        .enumerate()
        .take(MAX_HEADROOM_FALLBACK_ATTEMPTS)
    {
        let mask = &masks[options
            .whitelists
            .iter()
            .position(|candidate| *candidate == whitelist)
            .expect("ordered whitelist must be configured")];
        let lambda_used = lambda * lambda_scale;
        let full = full_candidate_cached(
            &mut cache,
            problem,
            options,
            prior,
            prior_response_slice,
            whitelist,
            mask,
            lambda,
            lambda_used,
            &fit_indices,
            &improvement_indices,
            &all_indices,
        )?;
        let acceptable = full.headroom.passed && status_usable(full.status);
        if acceptable {
            chosen = Some((attempt, full));
            break;
        }
        let better = match &best_fallback {
            None => true,
            Some((_, current)) => {
                let current_score = (
                    status_usable(current.status) as i32,
                    current.headroom.passed as i32,
                    -current.headroom.worst_excess_db,
                );
                let new_score = (
                    status_usable(full.status) as i32,
                    full.headroom.passed as i32,
                    -full.headroom.worst_excess_db,
                );
                new_score > current_score
            }
        };
        if better {
            best_fallback = Some((attempt, full));
        }
    }
    let (selected_attempt, selected_full) = match chosen {
        Some(found) => found,
        None => {
            let fallback = best_fallback.ok_or_else(|| {
                err(
                    RegularizeErrorKind::Missing,
                    "regularize.selection",
                    "no candidate could be solved under the configured resource limits",
                )
            })?;
            push_warning(
                &mut warnings,
                RegularizeWarning::HEADROOM_VIOLATION,
                format!(
                    "no candidate passed the declared gain headroom; returning best-effort candidate (worst excess {:.3} dB) with deliverable=false",
                    fallback.1.headroom.worst_excess_db
                ),
            );
            fallback
        }
    };
    let selected_whitelist = ordered
        .get(selected_attempt)
        .map(|(whitelist, _)| *whitelist)
        .unwrap_or(scan_whitelist);
    let selected_lambda = ordered
        .get(selected_attempt)
        .map(|(_, lambda)| *lambda)
        .unwrap_or(initial_lambda);
    let selected_lambda_used = selected_lambda * lambda_scale;
    // 报告自洽：lambda 扫描必须描述最终选中白名单（headroom 回退可能把白名单改掉）。
    if selected_whitelist != scan_whitelist {
        let (points, corner) = scan_lambda_points(
            &mut cache,
            problem,
            options,
            prior,
            prior_response_slice,
            selected_whitelist,
            &masks[mask_index(selected_whitelist)],
            lambda_scale,
            &fit_indices,
            &improvement_indices,
            &all_indices,
        )?;
        scan_points = points;
        corner_index = corner;
        l_curve_corner_lambda = corner_index.map(|index| scan_points[index].lambda);
        scan_whitelist = selected_whitelist;
    }
    // tie_break_used 只表示容差带 tie-break 选择真的成为最终解；被 headroom 回退覆盖时如实为 false。
    let tie_break_used = tie_break_engaged
        && cv_selected
            .map(|(whitelist, lambda)| whitelist == selected_whitelist && lambda == selected_lambda)
            .unwrap_or(false);
    if tie_break_used {
        push_warning(
            &mut warnings,
            RegularizeWarning::CV_TIEBREAK,
            format!(
                "CV tie-break inside {} dB selected a simpler/stronger-regularized candidate over the strict CV minimum",
                options.selection_tolerance_db
            ),
        );
    }
    if selected_attempt > 0 {
        push_warning(
            &mut warnings,
            RegularizeWarning::HEADROOM_FALLBACK,
            format!(
                "initial candidate failed gain headroom/status; fallback #{} selected whitelist={} lambda={} (passed={})",
                selected_attempt,
                selected_whitelist.as_str(),
                selected_lambda,
                selected_full.headroom.passed
            ),
        );
    }
    let selected_candidate = candidates.iter().find(|candidate| {
        candidate.whitelist == selected_whitelist && candidate.lambda == selected_lambda
    });
    let selected_cv_rank = selected_candidate
        .and_then(|candidate| candidate.cv_rank)
        .or(selected_cv_rank);
    let selected_cv_mean_db = selected_candidate
        .and_then(|candidate| candidate.cv_mean_db)
        .or(selected_cv_mean);
    let final_status = selected_full.status;
    if !status_usable(final_status) {
        push_warning(
            &mut warnings,
            RegularizeWarning::UPSTREAM_STATUS,
            format!(
                "final solve status={final_status} is not usable; deliverable=false (best-effort solution returned)"
            ),
        );
    }
    if selected_full.headroom.worst_excess_db > 0.0 {
        push_warning(
            &mut warnings,
            RegularizeWarning::HEADROOM_VIOLATION,
            format!(
                "final solution exceeds the declared gain headroom by {:.3} dB",
                selected_full.headroom.worst_excess_db
            ),
        );
    }
    if selected_full.design.near_zero_points != 0 {
        push_warning(
            &mut warnings,
            RegularizeWarning::NEAR_ZERO_TARGETS,
            format!(
                "{} near-zero target points were skipped in relative-residual statistics (no fake 100+ dB residuals)",
                selected_full.design.near_zero_points
            ),
        );
    }
    let prior_distance_norm = selected_full.delta_norm;
    if !prior.is_zero() && prior_distance_norm > 0.0 {
        let coeff_norm = selected_full
            .solution
            .coeffs_f64
            .iter()
            .map(|value| value * value)
            .sum::<f64>()
            .sqrt();
        if coeff_norm > 0.0 && prior_distance_norm / coeff_norm < 0.05 {
            push_warning(
                &mut warnings,
                RegularizeWarning::PRIOR_DOMINANT,
                format!(
                    "solution is dominated by the frozen prior (||g-g_prior||/||g||={:.3e} < 0.05); lambda is strong relative to the data term",
                    prior_distance_norm / coeff_norm
                ),
            );
        }
    }
    let deliverable =
        applicability.p_ge_m && status_usable(final_status) && selected_full.headroom.passed;
    if !deliverable && applicability.p_ge_m && !selected_full.headroom.passed {
        push_warning(
            &mut warnings,
            RegularizeWarning::HEADROOM_VIOLATION,
            "deliverable=false because the final solution violates the declared gain headroom"
                .to_string(),
        );
    }
    let fallback_used = !cv_valid || selected_attempt > 0;
    let selection = SelectionReport {
        method: selection_method,
        selected_whitelist,
        selected_lambda,
        selected_lambda_used,
        selected_cv_rank,
        selected_cv_mean_db,
        cv_valid,
        cv_reason,
        fallback_used,
        tie_break_used,
        l_curve_corner_lambda,
        headroom_fallback_attempts: selected_attempt,
    };
    let lambda_scan = LambdaScan {
        lambda_scale,
        scan_whitelist,
        points: scan_points,
        l_curve_corner_index: corner_index,
        l_curve_corner_lambda,
        note: L_CURVE_NOTE,
    };
    let report = RegularizeReport {
        schema: REGULARIZE_SCHEMA,
        mode: problem.mode.as_str(),
        m,
        taps,
        delay_samples: options.base.delay_samples,
        sample_rate_hz: problem.sample_rate_hz,
        fit_band_hz: options.base.fit_band_hz,
        improvement_band_hz: IMPROVEMENT_BAND_HZ,
        grid_points: problem.grid_hz.len(),
        band_points,
        design_positions: positions,
        position_labels: problem
            .positions
            .iter()
            .map(|position| position.label.clone())
            .collect(),
        position_weights: problem
            .positions
            .iter()
            .map(|position| position.weight)
            .collect(),
        applicability,
        lambda_scale,
        lambda_scan,
        candidates,
        selection,
        prior_kind: prior.kind().as_str(),
        prior_hash: Some(prior.hash().to_string()),
        prior_distance_norm,
        headroom: selected_full.headroom.clone(),
        final_solve_status: final_status,
        final_residual: selected_full.design.clone(),
        solution_validation: None,
        cv_uses_only_design_positions: true,
        holdout_used_for_tuning: false,
        forbidden_tuning_labels: options.forbidden_tuning_labels.clone(),
        deliverable,
        applied: false,
        post_apply_measurement: "not_requested",
        engine_compatibility: "not_validated",
        runtime_validation: "not_run",
        claims: REGULARIZE_CLAIM,
        warnings,
        solution_hash: Some(selected_full.solution.hash.clone()),
        coeffs_sha256: Some(selected_full.solution.coeffs_sha256.clone()),
        base_solve_hash: selected_full.solution.base_solve_hash.clone(),
    };
    Ok(RegularizeOutcome {
        report,
        solution: Some(selected_full.solution),
    })
}

fn status_usable(status: &'static str) -> bool {
    status == SolveStatus::Converged.as_str() || status == SolveStatus::PreserveBaseline.as_str()
}

#[allow(clippy::too_many_arguments)]
fn regularize_preserve(
    problem: &SolveProblem,
    options: &RegularizeOptions,
    applicability: ApplicabilityBoundary,
    lambda_scale: f64,
    fit_indices: Vec<usize>,
    improvement_indices: Vec<usize>,
    mut warnings: Vec<RegularizeWarning>,
) -> Result<RegularizeOutcome, RegularizeError> {
    let m = problem.m();
    let taps = options.base.taps;
    push_warning(
        &mut warnings,
        RegularizeWarning::PRESERVE_NO_TUNING,
        "preserve target: G=deltaD*I by construction; design-internal CV/lambda/whitelist selection is not applicable (no modal correction requested)"
            .to_string(),
    );
    let mut solve_options = options.base.clone();
    solve_options.ridge = 0.0;
    solve_options.path_mask = None;
    let outcome = solve(problem, &solve_options).map_err(map_solve_error)?;
    let base_solution = outcome.solution.as_ref().ok_or_else(|| {
        err(
            RegularizeErrorKind::Upstream,
            "regularize.preserve.solution",
            "preserve solve returned no solution",
        )
    })?;
    let solution = materialize(problem, options, &options.prior, base_solution)?;
    let headroom = evaluate_headroom(problem, &solution, &improvement_indices, &options.headroom);
    let design = evaluate_design(
        problem,
        &solution,
        &fit_indices,
        &improvement_indices,
        options.base.frequency_weights.as_deref(),
    );
    let final_status = outcome.report.status.as_str();
    let deliverable = applicability.p_ge_m && status_usable(final_status) && headroom.passed;
    if !status_usable(final_status) {
        push_warning(
            &mut warnings,
            RegularizeWarning::UPSTREAM_STATUS,
            format!("preserve solve status={final_status} is not usable; deliverable=false"),
        );
    }
    let delta_norm = base_solution
        .coeffs_f64
        .iter()
        .map(|value| value * value)
        .sum::<f64>()
        .sqrt();
    let selection = SelectionReport {
        method: SELECTION_METHOD_PRESERVE,
        selected_whitelist: WhitelistKind::DiagOnly,
        selected_lambda: 0.0,
        selected_lambda_used: 0.0,
        selected_cv_rank: None,
        selected_cv_mean_db: None,
        cv_valid: false,
        cv_reason: Some("preserve_baseline"),
        fallback_used: false,
        tie_break_used: false,
        l_curve_corner_lambda: None,
        headroom_fallback_attempts: 0,
    };
    let report = RegularizeReport {
        schema: REGULARIZE_SCHEMA,
        mode: problem.mode.as_str(),
        m,
        taps,
        delay_samples: options.base.delay_samples,
        sample_rate_hz: problem.sample_rate_hz,
        fit_band_hz: options.base.fit_band_hz,
        improvement_band_hz: IMPROVEMENT_BAND_HZ,
        grid_points: problem.grid_hz.len(),
        band_points: fit_indices.len(),
        design_positions: problem.positions.len(),
        position_labels: problem
            .positions
            .iter()
            .map(|position| position.label.clone())
            .collect(),
        position_weights: problem
            .positions
            .iter()
            .map(|position| position.weight)
            .collect(),
        applicability,
        lambda_scale,
        lambda_scan: LambdaScan {
            lambda_scale,
            scan_whitelist: selection.selected_whitelist,
            points: Vec::new(),
            l_curve_corner_index: None,
            l_curve_corner_lambda: None,
            note: L_CURVE_NOTE,
        },
        candidates: Vec::new(),
        selection,
        prior_kind: options.prior.kind().as_str(),
        prior_hash: Some(options.prior.hash().to_string()),
        prior_distance_norm: delta_norm,
        headroom,
        final_solve_status: final_status,
        final_residual: design,
        solution_validation: None,
        cv_uses_only_design_positions: true,
        holdout_used_for_tuning: false,
        forbidden_tuning_labels: options.forbidden_tuning_labels.clone(),
        deliverable,
        applied: false,
        post_apply_measurement: "not_requested",
        engine_compatibility: "not_validated",
        runtime_validation: "not_run",
        claims: REGULARIZE_CLAIM,
        warnings,
        solution_hash: Some(solution.hash.clone()),
        coeffs_sha256: Some(solution.coeffs_sha256.clone()),
        base_solve_hash: solution.base_solve_hash.clone(),
    };
    Ok(RegularizeOutcome {
        report,
        solution: Some(solution),
    })
}

// ===========================================================================
// 入口：regularize_from_target_plant（T50 桥接 + validate_solution）
// ===========================================================================

/// 从 T50 产物（TargetMatrix + EffectivePlant）正则：自动把 validation/holdout label
/// 加入 forbidden_tuning_labels，并在产出后用 T50 validate_solution 复核最终 G。
pub fn regularize_from_target_plant(
    target: &TargetMatrix,
    plant: &EffectivePlant,
    options: &RegularizeOptions,
) -> Result<RegularizeOutcome, RegularizeError> {
    if !target.verify_holdout_isolated() {
        return Err(err(
            RegularizeErrorKind::Holdout,
            "regularize.target",
            "target cells reference validation/holdout labels; holdout must never enter the regularizer",
        ));
    }
    let problem = SolveProblem::from_target_plant(target, plant).map_err(map_solve_error)?;
    let mut effective = options.clone();
    for label in plant.validation_labels() {
        if !effective.forbidden_tuning_labels.contains(&label) {
            effective.forbidden_tuning_labels.push(label);
        }
    }
    let mut outcome = regularize(&problem, &effective)?;
    if let Some(solution) = &outcome.solution {
        match validate_solution(target, plant, &solution.modal) {
            Ok(validation) => {
                outcome.report.solution_validation =
                    Some(SolutionValidationSummary::from_validation(validation));
            }
            Err(error) => {
                return Err(err(
                    RegularizeErrorKind::Upstream,
                    "regularize.solution_validation",
                    format!("final G failed T50 validate_solution contract checks: {error}"),
                ));
            }
        }
    }
    Ok(outcome)
}

// ===========================================================================
// 证据 JSON
// ===========================================================================

impl RegularizeOutcome {
    /// 证据 JSON（schema tspi-modal-regularize/v1；确定性、无时间戳；predicted-only）。
    pub fn to_evidence_json(&self) -> String {
        let report = &self.report;
        let mut json = String::new();
        json.push('{');
        json.push_str(&format!("\"schema\":\"{}\",", report.schema));
        json.push_str(&format!(
            "\"mode\":\"{}\",\"m\":{},\"taps\":{},\"delay_samples\":{},\"sample_rate_hz\":{},\"fit_band_hz\":[{},{}],\"improvement_band_hz\":[{},{}],\"grid_points\":{},\"band_points\":{},\"design_positions\":{},",
            report.mode,
            report.m,
            report.taps,
            report.delay_samples,
            report.sample_rate_hz,
            json_f64(report.fit_band_hz.0),
            json_f64(report.fit_band_hz.1),
            json_f64(report.improvement_band_hz.0),
            json_f64(report.improvement_band_hz.1),
            report.grid_points,
            report.band_points,
            report.design_positions
        ));
        json.push_str("\"position_labels\":[");
        for (index, label) in report.position_labels.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            json.push_str(&format!("\"{}\"", json_escape(label)));
        }
        json.push_str("],\"position_weights\":[");
        for (index, weight) in report.position_weights.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            json.push_str(&json_f64(*weight));
        }
        json.push_str("],");
        let applicability = &report.applicability;
        json.push_str(&format!(
            "\"applicability\":{{\"m\":{},\"design_positions\":{},\"p_ge_m\":{},\"status\":\"{}\",\"cv_requires_p_gt_m\":{},\"note\":\"{}\"}},",
            applicability.m,
            applicability.design_positions,
            applicability.p_ge_m,
            applicability.status,
            applicability.cv_requires_p_gt_m,
            json_escape(applicability.note)
        ));
        json.push_str(&format!(
            "\"lambda_scale\":{},\"lambda_scan\":{{\"scan_whitelist\":\"{}\",\"points\":[",
            json_f64(report.lambda_scan.lambda_scale),
            report.lambda_scan.scan_whitelist.as_str()
        ));
        for (index, point) in report.lambda_scan.points.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            json.push_str(&format!(
                "{{\"lambda\":{},\"lambda_used\":{},\"design_residual_norm\":{},\"solution_norm\":{},\"fit_band_max_db\":{},\"improvement_band_max_db\":{},\"curvature\":{},\"solve_status\":\"{}\",\"headroom_feasible\":{},\"headroom_worst_excess_db\":{}}}",
                json_f64(point.lambda),
                json_f64(point.lambda_used),
                json_f64(point.design_residual_norm),
                json_f64(point.solution_norm),
                json_f64(point.fit_band_max_db),
                json_f64(point.improvement_band_max_db),
                json_opt_f64(point.curvature),
                point.solve_status,
                point.headroom_feasible,
                json_f64(point.headroom_worst_excess_db)
            ));
        }
        json.push_str(&format!(
            "],\"l_curve_corner_index\":{},\"l_curve_corner_lambda\":{},\"note\":\"{}\"}},",
            match report.lambda_scan.l_curve_corner_index {
                Some(index) => index.to_string(),
                None => "null".to_string(),
            },
            json_opt_f64(report.lambda_scan.l_curve_corner_lambda),
            json_escape(report.lambda_scan.note)
        ));
        json.push_str("\"candidates\":[");
        for (index, candidate) in report.candidates.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            let applicability = &candidate.applicability;
            json.push_str(&format!(
                "{{\"whitelist\":\"{}\",\"free_paths\":{},\"lambda\":{},\"lambda_used\":{},\"cv_mean_db\":{},\"cv_worst_db\":{},\"cv_valid\":{},\"cv_rank\":{},\"fold_underdetermined\":{},\"headroom_feasible_in_folds\":{},\"headroom_worst_excess_in_folds_db\":{},\"applicability\":{{\"free_paths\":{},\"per_frequency_rank_bound\":{},\"fir_rank_bound\":{},\"per_frequency_full_rank_necessary\":{},\"fir_rank_necessary\":{},\"note\":\"{}\"}},\"folds\":[",
                candidate.whitelist.as_str(),
                candidate.free_paths,
                json_f64(candidate.lambda),
                json_f64(candidate.lambda_used),
                json_opt_f64(candidate.cv_mean_db),
                json_opt_f64(candidate.cv_worst_db),
                candidate.cv_valid,
                match candidate.cv_rank {
                    Some(rank) => rank.to_string(),
                    None => "null".to_string(),
                },
                candidate.fold_underdetermined,
                candidate.headroom_feasible_in_folds,
                json_f64(candidate.headroom_worst_excess_in_folds_db),
                applicability.free_paths,
                applicability.per_frequency_rank_bound,
                applicability.fir_rank_bound,
                applicability.per_frequency_full_rank_necessary,
                applicability.fir_rank_necessary,
                json_escape(applicability.note)
            ));
            for (fold_index, fold) in candidate.folds.iter().enumerate() {
                if fold_index > 0 {
                    json.push(',');
                }
                json.push_str(&format!(
                    "{{\"fold_index\":{},\"held_out_labels\":[",
                    fold.fold_index
                ));
                for (label_index, label) in fold.held_out_labels.iter().enumerate() {
                    if label_index > 0 {
                        json.push(',');
                    }
                    json.push_str(&format!("\"{}\"", json_escape(label)));
                }
                json.push_str(&format!(
                    "],\"training_positions\":{},\"solve_status\":\"{}\",\"usable\":{},\"underdetermined\":{},\"iterations\":{},\"mean_residual_db\":{},\"worst_residual_db\":{},\"valid_points\":{},\"near_zero_points\":{},\"headroom_feasible\":{},\"headroom_worst_excess_db\":{}}}",
                    fold.training_positions,
                    fold.solve_status,
                    fold.usable,
                    fold.underdetermined,
                    fold.iterations,
                    json_f64(fold.mean_residual_db),
                    json_f64(fold.worst_residual_db),
                    fold.valid_points,
                    fold.near_zero_points,
                    fold.headroom_feasible,
                    json_f64(fold.headroom_worst_excess_db)
                ));
            }
            json.push_str("]}");
        }
        json.push_str("],");
        let selection = &report.selection;
        json.push_str(&format!(
            "\"selection\":{{\"method\":\"{}\",\"selected_whitelist\":\"{}\",\"selected_lambda\":{},\"selected_lambda_used\":{},\"selected_cv_rank\":{},\"selected_cv_mean_db\":{},\"cv_valid\":{},\"cv_reason\":{},\"fallback_used\":{},\"tie_break_used\":{},\"l_curve_corner_lambda\":{},\"headroom_fallback_attempts\":{}}},",
            selection.method,
            selection.selected_whitelist.as_str(),
            json_f64(selection.selected_lambda),
            json_f64(selection.selected_lambda_used),
            match selection.selected_cv_rank {
                Some(rank) => rank.to_string(),
                None => "null".to_string(),
            },
            json_opt_f64(selection.selected_cv_mean_db),
            selection.cv_valid,
            match selection.cv_reason {
                Some(reason) => format!("\"{}\"", json_escape(reason)),
                None => "null".to_string(),
            },
            selection.fallback_used,
            selection.tie_break_used,
            json_opt_f64(selection.l_curve_corner_lambda),
            selection.headroom_fallback_attempts
        ));
        json.push_str(&format!(
            "\"prior\":{{\"kind\":\"{}\",\"hash\":{},\"distance_norm\":{}}},",
            report.prior_kind,
            match &report.prior_hash {
                Some(hash) => format!("\"{}\"", json_escape(hash)),
                None => "null".to_string(),
            },
            json_f64(report.prior_distance_norm)
        ));
        let headroom = &report.headroom;
        json.push_str(&format!(
            "\"headroom\":{{\"correction_cap_db\":{},\"diagonal_cap_db\":{},\"tolerance_db\":{},\"band_hz\":[{},{}],\"paths\":[",
            json_f64(headroom.correction_cap_db),
            json_f64(headroom.diagonal_cap_db),
            json_f64(headroom.tolerance_db),
            json_f64(headroom.band_hz.0),
            json_f64(headroom.band_hz.1)
        ));
        for (index, path) in headroom.paths.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            json.push_str(&format!(
                "{{\"out_dense\":{},\"in_dense\":{},\"out_role\":\"{}\",\"in_role\":\"{}\",\"is_correction\":{},\"max_gain_db\":{},\"cap_db\":{},\"margin_db\":{},\"passed\":{}}}",
                path.out_dense,
                path.in_dense,
                json_escape(&path.out_role),
                json_escape(&path.in_role),
                path.is_correction,
                json_f64(path.max_gain_db),
                json_f64(path.cap_db),
                json_f64(path.margin_db),
                path.passed
            ));
        }
        json.push_str(&format!(
            "],\"max_correction_gain_db\":{},\"max_diagonal_gain_db\":{},\"worst_excess_db\":{},\"passed\":{},\"note\":\"{}\"}},",
            json_f64(headroom.max_correction_gain_db),
            json_f64(headroom.max_diagonal_gain_db),
            json_f64(headroom.worst_excess_db),
            headroom.passed,
            json_escape(headroom.note)
        ));
        let residual = &report.final_residual;
        json.push_str(&format!(
            "\"final\":{{\"solve_status\":\"{}\",\"fit_band_max_db\":{},\"improvement_band_max_db\":{},\"residual_norm\":{},\"near_zero_points\":{},\"valid_points\":{},\"worst\":",
            report.final_solve_status,
            json_f64(residual.fit_band_max_db),
            json_f64(residual.improvement_band_max_db),
            json_f64(residual.residual_norm),
            residual.near_zero_points,
            residual.valid_points
        ));
        match &residual.worst {
            Some((position, role, value)) => json.push_str(&format!(
                "{{\"position\":\"{}\",\"role\":\"{}\",\"residual_db\":{}}}",
                json_escape(position),
                json_escape(role),
                json_f64(*value)
            )),
            None => json.push_str("null"),
        }
        json.push_str("},");
        match &report.solution_validation {
            Some(validation) => {
                json.push_str(&format!(
                    "\"solution_validation\":{{\"profile\":\"{}\",\"passed\":{},\"residual_max_db\":{},\"offdiag_max_db\":{},\"diag_deviation_max_db\":{},\"checks\":[",
                    validation.profile,
                    validation.passed,
                    json_opt_f64(validation.residual_max_db),
                    json_f64(validation.offdiag_max_db),
                    json_f64(validation.diag_deviation_max_db)
                ));
                for (index, (name, passed, detail)) in validation.checks.iter().enumerate() {
                    if index > 0 {
                        json.push(',');
                    }
                    json.push_str(&format!(
                        "{{\"name\":\"{}\",\"passed\":{},\"detail\":\"{}\"}}",
                        json_escape(name),
                        passed,
                        json_escape(detail)
                    ));
                }
                json.push_str("]},");
            }
            None => json.push_str("\"solution_validation\":null,"),
        }
        json.push_str(&format!(
            "\"cv_uses_only_design_positions\":{},\"holdout_used_for_tuning\":{},\"forbidden_tuning_labels\":[",
            report.cv_uses_only_design_positions, report.holdout_used_for_tuning
        ));
        for (index, label) in report.forbidden_tuning_labels.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            json.push_str(&format!("\"{}\"", json_escape(label)));
        }
        json.push_str(&format!(
            "],\"deliverable\":{},\"applied\":{},\"post_apply_measurement\":\"{}\",\"engine_compatibility\":\"{}\",\"runtime_validation\":\"{}\",\"claims\":\"{}\",",
            report.deliverable,
            report.applied,
            report.post_apply_measurement,
            report.engine_compatibility,
            report.runtime_validation,
            json_escape(report.claims)
        ));
        json.push_str("\"warnings\":[");
        for (index, warning) in report.warnings.iter().enumerate() {
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
        match &self.solution {
            Some(solution) => {
                json.push_str(&format!(
                    "\"solution\":{{\"hash\":\"{}\",\"coeffs_sha256\":\"{}\",\"base_solve_hash\":{},\"prior_hash\":\"{}\",\"coefficients\":{}}}",
                    solution.hash,
                    solution.coeffs_sha256,
                    match &solution.base_solve_hash {
                        Some(hash) => format!("\"{}\"", json_escape(hash)),
                        None => "null".to_string(),
                    },
                    json_escape(&solution.prior_hash),
                    solution.coeffs_f64.len()
                ));
            }
            None => json.push_str("\"solution\":null"),
        }
        json.push('}');
        json
    }
}

// ===========================================================================
// 静态守卫（单测用；证明本模块纯计算、无 I/O/线程/环境依赖）
// ===========================================================================

/// 本模块源码文本（单测静态守卫用；不参与运行期逻辑）。
pub fn regularize_source_text_for_guards() -> &'static str {
    include_str!("regularize.rs")
}
