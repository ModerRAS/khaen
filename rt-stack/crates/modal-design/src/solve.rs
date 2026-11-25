//! [NONRT] 真实 5x5 因果有限 FIR 模态 solver（WBS T51）—— 本文件写域。
//!
//! ## 权威契约（只读输入，禁止在本文件改写）
//!
//! - scratch/lead/planner/TSPI-RT-STACK-WBS.md T51：因果有限 FIR 最小算子/迭代解与可恢复满秩
//!   夹具，报告未收敛/资源上界，**不写通用“消全部驻波”式承诺**。
//! - scratch/lead/planner/DSP-HOST-DECISION.md §2.5：先冻结 P_post 基线，再以 H_eff 求真实
//!   5x5 低频减尾/抑模态目标；正则/design 内 CV/白名单/lambda/gain headroom 归 T52；不是全带
//!   串扰反演，也不承诺消除全部驻波。
//! - docs/MIMO-COEFF-PLAN.md §3.1/§3.4/§5.1（求解数学与可辨识性依据，只读参考）：
//!   每输入独立求解 min_g Sum_p w_p |H_p(f)·g_i(f) - (d_{p,i}(f) - b_{p,i}(f))|^2 + lambda*||g_i||^2，
//!   P<M 时每频点欠定、最小范数解只保证设计点；P>=M 且良态时可恢复（F1 精确可恢复夹具）。
//! - rt-stack/spec/core-v1.md / rt-stack/README.md：T06 scaffold/RT 纪律；fs 属于
//!   {44.1,48,88.2,96} kHz；G 支撑 L_G<=4096 与 T07 kernel 容量对齐。
//! - crate::target（T50）：TargetMatrix（对角目标，非对角结构零）/ EffectivePlant（design
//!   position 的 H_eff）/ ModalMatrix（完整 G 复响应）；crate::timebase（T49）ComplexPoint。
//!   T50 §9 接口约定：只用 design positions/目标矩阵；不按 position 单独“最佳对齐”（T6 FC-04）；
//!   Preserve 期望 G=deltaD·I；Reduced 目标含声明 target delay；求解后必须声明完整 G（含对角）。
//! - T07 core::direct：G 系数布局 g[out][in][tap]（m*m*l_g，row-major），完整
//!   G = deltaD·I + g，L_G >= D+1、L_F=L_A+L_G+L_P-2<=4096；本 solver 产出的 g 直接按该布局
//!   提供 f64/f32 数组，不写 kernel/plan ABI。
//!
//! ## 本文件做什么（T51 DoD）
//!
//! 1. **因果有限 FIR 最小算子**：未知量是每输入 i 的 M×L_G 个**实** FIR 系数
//!    g[j][i][t]（t=0..L_G-1，因果、有限支撑），频域基 e^{-j2*pi*f*t/fs}；完整
//!    G_{ji}(f) = delta_{ji}*e^{-j2*pi*D*f/fs} + Sum_t g_{ji}[t]*e^{-j2*pi*f*t/fs}。
//!    不做 IFFT->加延迟->截断（会丢因果性/尾音），直接把目标写成带因果基的线性最小二乘。
//! 2. **迭代解（CGLS）**：法方程 (A^H W A + lambda I) g = A^H W b（lambda 默认 0，T52 正则可
//!    复用），matrix-free 共轭梯度最小二乘；初值 0 ⇒ 欠定时收敛到**最小范数解**。逐输入独立
//!    求解，每列报告迭代数/相对残差/收敛标记/停止原因。早停/最大迭代/退化都如实报告。
//! 3. **可恢复满秩夹具**：SolveProblem 可直接用合成 H/target 构造（Custom 模式），并由
//!    SolveProblem::from_target_plant 从 T50 产物构造（Preserve/Reduced 模式）。测试覆盖：
//!    良态满秩合成矩阵精确恢复已知算子（<=1%）、Preserve 恒等、Reduced 设计点拟合与
//!    validate_solution 一致；并覆盖满秩/欠定两态。
//! 4. **未收敛/资源上界报告**：SolveStatus::{PreserveBaseline,Converged,NonConverged,
//!    Underdetermined,ResourceLimited}；SolveReport 带每列残差/迭代、频率域可辨识性
//!    （position×participant 秩、条件数估计、FIR 秩上界）、资源上界（未知量/复数行/工作量/
//!    内存界）与设计点预测残差；资源超界时返回 solution=None 的结构化报告（不 panic）。
//! 5. **诚实边界**：applied=false、post_apply_measurement=not_requested、
//!    engine_compatibility=not_validated、runtime_validation=not_run、
//!    holdout_used_for_tuning=false；claims 明确 predicted-only、欠定最小范数解非唯一、
//!    不保证消除全部驻波；不产任何“实测改善”结论。近零目标行（|d|<T50 NEAR_ZERO 且在
//!    拟合带内）不参与相对残差 dB 统计（相对误差在零目标上无定义），只计入
//!    SolveReport::near_zero_target_points，绝不把 100+ dB 数值伪装成拟合失败。
//!
//! ## 本文件不做什么（明确非目标）
//!
//! - 不测量/不采音/不打开设备/不写文件/不启线程（纯计算；静态守卫测试保证）；
//! - 不做正则/lambda 选择/CV/白名单策略/增益帽（T52；本文件只暴露 ridge/path_mask/
//!   frequency_weights 原语供其调用）、不做 holdout 判定（T53）、不产 WAV/manifest（T54）、
//!   不编译/热切（T66）、不做 PEQ/delay 拟合（T64/T65）；
//! - 不改 T50 目标语义（非对角结构零）；不把目标矩阵改成全带串扰反演；
//! - 不声明“已应用/已实测/消除全部驻波/通用改善 X dB”。
//!
//! ## 算法细节（冻结语义）
//!
//! - **输入**：design positions 的 H_{p,j}(f)（每个 position 每个 participant 一条复响应，
//!   即 MIMO 行向量 H_p 属于 C^{1×M}）与对角目标 d_{p,i}(f)（TargetMatrix 第 i 个
//!   participant 的对角 cell；Preserve 为 per-position，Reduced 为 position-invariant）。
//! - **方程**（每输入 i，每个 design position p、每个拟合频点 f）：
//!   Sum_j H_{p,j}(f)*G_{j,i}(f) = d_{p,i}(f)，其中
//!   G_{j,i}(f)=delta_{j,i}*z_D(f)+Sum_t g_{j,i}[t]*z_t(f)，z_t(f)=e^{-j2*pi*f*t/fs}；
//!   等价地 Sum_{j,t} H_{p,j}(f)z_t(f)*g_{j,i}[t] = d_{p,i}(f) - delta_{j,i}H_{p,i}(f)z_D(f)。
//!   行按 sqrt(position_weight * frequency_weight) 加权（加权最小二乘）。
//! - **拟合带**：默认 20-300 Hz（MIMO 修正带；MIMO-COEFF-PLAN §1.2），可用
//!   SolveOptions::fit_band_hz 收窄；带宽外不受约束（报告明确）。D 由
//!   SolveOptions::delay_samples 声明并与 T50 target_delay_samples 对齐（见 validate）。
//! - **Preserve**：目标 = H_eff，D=0，期望 g=0、G=I；本模块直接给恒等基线并断言
//!   target==H_eff（容差内），不假装“求了修正”。
//! - **确定性**：无时间戳/无随机数/固定迭代顺序；SolveProblem::hash 与
//!   SolveSolution::hash 对输入与解逐位确定（-0.0 归一），供 T53/T54/T66 做 stale/hash 链。
//!
//! ## 关键不变量（测试与静态守卫覆盖）
//!
//! - 因果有限 FIR：系数只有 t 属于 [0,L_G)，G 的 delta_D 基线显式保留；L_G>=D+1 强制。
//! - 欠定不冒充可辨识：P<M（或 2*P*F<未知量）→ Underdetermined + 明确警告；最小范数解
//!   只拟合 design 点，**不作任何泛化/实测承诺**。
//! - 未收敛/资源上界如实报告：NonConverged 保留 best-effort 解与每列停止原因；
//!   ResourceLimited 返回 solution=None 与超界维度，不 panic、不截断硬算。
//! - 只消费 T50 公开产物：design positions + 对角目标；validation/holdout 永不进入求解。
//! - 无 I/O/线程/环境/序列化/不安全代码依赖；不写 kernel/plan ABI。

use crate::ingest::sha256_hex;
use crate::target::{
    EffectivePlant, ModalMatrix, ModalMatrixCell, TargetCellView, TargetMatrix,
    IMPROVEMENT_BAND_HZ, MAX_PARTICIPANTS, NEAR_ZERO_MAGNITUDE, SAMPLE_RATES_HZ,
};
use crate::timebase::ComplexPoint;
use tspi_dsp_core::rt::{MAX_LOW_BAND_PARTICIPANTS, TAPS_NORMAL_MAX};

// ===========================================================================
// 常量与 schema
// ===========================================================================

/// solver 产物 schema（T51 设计侧/离线产物；不是实测或已应用）。
pub const SOLVE_SCHEMA: &str = "tspi-modal-solve/v1";
/// solver 输出 hash domain。
pub const SOLVE_HASH_DOMAIN: &str = "tspi-modal-solve/v1";
/// solver 问题 hash domain（输入身份；与输出 hash 分离）。
pub const SOLVE_PROBLEM_HASH_DOMAIN: &str = "tspi-modal-solve-problem/v1";

/// 默认拟合带（Hz；MIMO 修正带，MIMO-COEFF-PLAN §1.2）。
pub const DEFAULT_FIT_BAND_HZ: (f64, f64) = (20.0, 300.0);
/// 默认 FIR 支撑 L_G（设计侧保守默认；生产由 T66/资源规划显式声明）。
pub const DEFAULT_TAPS: usize = 256;
/// 默认 CGLS 最大迭代数。
pub const DEFAULT_MAX_ITERATIONS: usize = 128;
/// 默认相对残差收敛门（法方程梯度范数相对初值）。
pub const DEFAULT_REL_TOLERANCE: f64 = 1.0e-9;
/// 默认每输入未知量上限（M×L_G；M<=8、L_G<=4096 ⇒ 架构上限 32768）。
pub const DEFAULT_MAX_UNKNOWNS: usize = 16_384;
/// 默认复数行上限（positions×fit 频点）。
pub const DEFAULT_MAX_ROWS: usize = 262_144;
/// 默认工作量上限（单位：复数乘加，含正/伴随变换与响应评估上界）。
pub const DEFAULT_MAX_WORK: u64 = 2_000_000_000;
/// 默认 ridge（T52 会用自己的 lambda 覆盖；0 = 纯最小二乘）。
pub const DEFAULT_RIDGE: f64 = 0.0;

/// 单次 solve 的 design position 硬上限。
pub const MAX_SOLVE_POSITIONS: usize = 32;
/// 频率网格点硬上限（与 T50 MAX_TARGET_POINTS 对齐）。
pub const MAX_SOLVE_GRID_POINTS: usize = 8192;
/// CGLS 最大迭代数硬上限（报告资源上界用）。
pub const MAX_SOLVE_ITERATIONS: usize = 4096;
/// ridge 硬上限（lambda 属 T52，本文件只接受有限非负且不超此值）。
pub const MAX_SOLVE_RIDGE: f64 = 1.0e2;
/// 相对收敛门硬上限（不允许把“收敛”放宽到无意义）。
pub const MAX_SOLVE_REL_TOLERANCE: f64 = 1.0e-2;
/// G 支撑硬上限（= core-v1 TAPS_NORMAL_MAX；fused L_F 另由 T07/T66 校验）。
pub const MAX_SOLVE_TAPS: usize = TAPS_NORMAL_MAX as usize;
/// unknown 上限硬上界（= M_MAX × L_G_MAX）。
pub const MAX_SOLVE_UNKNOWNS_HARD: usize = MAX_LOW_BAND_PARTICIPANTS * MAX_SOLVE_TAPS;
/// 复数行硬上界。
pub const MAX_SOLVE_ROWS_HARD: usize = MAX_SOLVE_POSITIONS * MAX_SOLVE_GRID_POINTS;
/// 工作量硬上界（防止误用把设计工具跑成无限预算）。
pub const MAX_SOLVE_WORK_HARD: u64 = 1_000_000_000_000;
/// 频率一致性相对容差（problem 内网格对齐）。
pub const SOLVE_GRID_REL_TOLERANCE: f64 = 1.0e-9;
/// 可辨识性秩判零阈值（相对 Gram 最大特征值；对应计划 sigma<1e-3 sigma_max）。
pub const RANK_ZERO_REL: f64 = 1.0e-6;
/// 高条件数告警阈值（MIMO-COEFF-PLAN §3.1：约束 cond<=100）。
pub const HIGH_CONDITION_WARN: f64 = 100.0;
/// 幅度 sanity 上限（与 T50 一致；防非有限/离谱输入）。
pub const SOLVE_MAGNITUDE_SANITY: f64 = 1.0e6;
/// dB 比值下界（避免 log(0)）。
pub const SOLVE_DB_FLOOR: f64 = 1.0e-12;

/// 诚实 claim（不进任何实测结论；T51 DoD 明确不写通用“消全部驻波”式承诺）。
pub const SOLVE_CLAIM: &str = "causal finite-FIR least-squares solve at design positions only; predicted-only, not applied, not measured; holdout never used for tuning; minimum-norm solution on rank-deficient systems is non-unique; no guarantee that all standing waves are eliminated";

// ===========================================================================
// 错误
// ===========================================================================

/// solver 错误种类（稳定机器码见 SolveErrorKind::code）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SolveErrorKind {
    /// 选项非法（taps/delay/带宽/ridge/迭代/上限/掩码）。
    Options,
    /// 频率网格/拟合带非法。
    Grid,
    /// design position/权重/label 非法。
    Position,
    /// 目标矩阵结构/绑定非法（含非对角目标、缺 cell、Preserve 不等于 H_eff）。
    Target,
    /// 采样率不在 P1 契约。
    SampleRate,
    /// participants 集合非法或与目标不一致。
    Participants,
    /// P 基线/plant hash 不一致或 stale。
    Baseline,
    /// target delay 与 G 基线 D 不一致（禁止事后平移）。
    Delay,
    /// 非有限/越界/数值下溢。
    Range,
    /// 缺失必需输入。
    Missing,
    /// 未支持的 profile/规模（如 M>8、L_G>4096）。
    Unsupported,
}

impl SolveErrorKind {
    /// 全部错误种类（报告/负向对照用；顺序稳定）。
    pub const ALL: [SolveErrorKind; 11] = [
        SolveErrorKind::Options,
        SolveErrorKind::Grid,
        SolveErrorKind::Position,
        SolveErrorKind::Target,
        SolveErrorKind::SampleRate,
        SolveErrorKind::Participants,
        SolveErrorKind::Baseline,
        SolveErrorKind::Delay,
        SolveErrorKind::Range,
        SolveErrorKind::Missing,
        SolveErrorKind::Unsupported,
    ];

    /// 稳定机器码（E_SOLVE_*）。
    pub const fn code(self) -> &'static str {
        match self {
            SolveErrorKind::Options => "E_SOLVE_OPTIONS",
            SolveErrorKind::Grid => "E_SOLVE_GRID",
            SolveErrorKind::Position => "E_SOLVE_POSITION",
            SolveErrorKind::Target => "E_SOLVE_TARGET",
            SolveErrorKind::SampleRate => "E_SOLVE_SAMPLE_RATE",
            SolveErrorKind::Participants => "E_SOLVE_PARTICIPANTS",
            SolveErrorKind::Baseline => "E_SOLVE_BASELINE",
            SolveErrorKind::Delay => "E_SOLVE_DELAY_MISMATCH",
            SolveErrorKind::Range => "E_SOLVE_RANGE",
            SolveErrorKind::Missing => "E_SOLVE_MISSING",
            SolveErrorKind::Unsupported => "E_SOLVE_UNSUPPORTED",
        }
    }
}

/// solver 错误（kind + 字段路径 + 说明；Display 含机器码）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolveError {
    kind: SolveErrorKind,
    field: String,
    detail: String,
}

impl SolveError {
    /// 构造（field 为 a.b[c] 形式路径）。
    pub fn new(kind: SolveErrorKind, field: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            kind,
            field: field.into(),
            detail: detail.into(),
        }
    }

    /// 错误种类。
    pub fn kind(&self) -> SolveErrorKind {
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

impl std::fmt::Display for SolveError {
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

impl std::error::Error for SolveError {}

fn err(kind: SolveErrorKind, field: impl Into<String>, detail: impl Into<String>) -> SolveError {
    SolveError::new(kind, field, detail)
}

// ===========================================================================
// 非致命诊断
// ===========================================================================

/// 非致命诊断（不改变解的有效性；报告必须原样带出）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolveWarning {
    /// 稳定告警码（W_SOLVE_*）。
    pub code: &'static str,
    /// 人读说明。
    pub message: String,
}

impl SolveWarning {
    /// 设计点欠定（P<M 或秩上界不足）；最小范数解非唯一。
    pub const UNDERDETERMINED: &'static str = "W_SOLVE_UNDERDETERMINED";
    /// CGLS 未在迭代上限内达到收敛门。
    pub const NON_CONVERGED: &'static str = "W_SOLVE_NON_CONVERGED";
    /// 资源上界（未知量/行数/工作量）拒绝硬算。
    pub const RESOURCE_BOUND: &'static str = "W_SOLVE_RESOURCE_BOUND";
    /// 使用了非零 ridge（T52 正则；解有偏、残差不趋零）。
    pub const RIDGE_USED: &'static str = "W_SOLVE_RIDGE_USED";
    /// 频率域条件数超工程建议（cond>100）。
    pub const HIGH_CONDITION: &'static str = "W_SOLVE_HIGH_CONDITION";
    /// Preserve 模式：期望 G=deltaD·I，本文件未“求修正”。
    pub const PRESERVE_IDENTITY: &'static str = "W_SOLVE_PRESERVE_IDENTITY";
    /// 上游目标状态非 predicted_pass（infeasible/incomplete）——只求解，不冒充可行。
    pub const TARGET_NOT_DELIVERABLE: &'static str = "W_SOLVE_TARGET_NOT_DELIVERABLE";
    /// 拟合带/支撑规模较大（仅提示计算量，不是失败）。
    pub const LARGE_RESOURCE: &'static str = "W_SOLVE_LARGE_RESOURCE";
    /// 法方程梯度已收敛但相对残差仍不低（近奇异/病态；系数解释受限）。
    pub const NORMAL_RESIDUAL_GAP: &'static str = "W_SOLVE_NORMAL_RESIDUAL_GAP";
}

fn push_warning(warnings: &mut Vec<SolveWarning>, code: &'static str, message: String) {
    if warnings
        .iter()
        .any(|existing| existing.code == code && existing.message == message)
    {
        return;
    }
    warnings.push(SolveWarning { code, message });
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

fn cconj(a: C) -> C {
    (a.0, -a.1)
}

fn cabs2(a: C) -> f64 {
    a.0 * a.0 + a.1 * a.1
}

fn cabs(a: C) -> f64 {
    a.0.hypot(a.1)
}

fn real_dot(left: &[f64], right: &[f64]) -> f64 {
    left.iter().zip(right.iter()).map(|(a, b)| a * b).sum()
}

fn norm2_complex(values: &[C]) -> f64 {
    values.iter().map(|value| cabs2(*value)).sum()
}

fn finite(value: f64, field: &str) -> Result<f64, SolveError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(err(
            SolveErrorKind::Range,
            field,
            format!("value must be finite, got {value}"),
        ))
    }
}

fn finite_positive(value: f64, field: &str) -> Result<f64, SolveError> {
    let value = finite(value, field)?;
    if value > 0.0 {
        Ok(value)
    } else {
        Err(err(
            SolveErrorKind::Range,
            field,
            format!("value must be > 0, got {value}"),
        ))
    }
}

fn fmt_f64(value: f64) -> String {
    let normalized = if value == 0.0 { 0.0 } else { value };
    debug_assert!(normalized.is_finite());
    format!("{normalized:.17e}")
}

fn magnitude_db_of(magnitude: f64) -> f64 {
    20.0 * magnitude.max(SOLVE_DB_FLOOR).log10()
}

fn ratio_db(numerator: f64, denominator: f64) -> f64 {
    20.0 * (numerator.max(SOLVE_DB_FLOOR) / denominator.max(SOLVE_DB_FLOOR)).log10()
}

fn points_equal(left: &ComplexPoint, right: &ComplexPoint) -> bool {
    let tolerance = 1.0e-9
        * left
            .re
            .abs()
            .max(left.im.abs())
            .max(right.re.abs())
            .max(right.im.abs())
            .max(1.0);
    let freq_tolerance =
        SOLVE_GRID_REL_TOLERANCE * left.freq_hz.abs().max(right.freq_hz.abs()).max(1.0);
    (left.freq_hz - right.freq_hz).abs() <= freq_tolerance
        && (left.re - right.re).abs() <= tolerance
        && (left.im - right.im).abs() <= tolerance
}

fn grid_freq_equal(left: f64, right: f64) -> bool {
    let tolerance = SOLVE_GRID_REL_TOLERANCE * left.abs().max(right.abs()).max(1.0);
    (left - right).abs() <= tolerance
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
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

/// 对称矩阵 Jacobi 特征值（n<=16；确定性；设计侧小矩阵用）。
fn symmetric_eigenvalues(matrix: &[f64], n: usize) -> Vec<f64> {
    let mut a = matrix.to_vec();
    for _sweep in 0..64 {
        let mut off = 0.0f64;
        for p in 0..n {
            for q in (p + 1)..n {
                off += a[p * n + q] * a[p * n + q];
            }
        }
        if off <= 1.0e-26 {
            break;
        }
        for p in 0..n {
            for q in (p + 1)..n {
                let apq = a[p * n + q];
                if apq.abs() <= 1.0e-18 {
                    continue;
                }
                let app = a[p * n + p];
                let aqq = a[q * n + q];
                let theta = (aqq - app) / (2.0 * apq);
                let t = if theta >= 0.0 {
                    1.0 / (theta + (1.0 + theta * theta).sqrt())
                } else {
                    -1.0 / (-theta + (1.0 + theta * theta).sqrt())
                };
                let c = 1.0 / (1.0 + t * t).sqrt();
                let s = t * c;
                for k in 0..n {
                    if k != p && k != q {
                        let akp = a[k * n + p];
                        let akq = a[k * n + q];
                        a[k * n + p] = c * akp - s * akq;
                        a[p * n + k] = a[k * n + p];
                        a[k * n + q] = s * akp + c * akq;
                        a[q * n + k] = a[k * n + q];
                    }
                }
                a[p * n + p] = app - t * apq;
                a[q * n + q] = aqq + t * apq;
                a[p * n + q] = 0.0;
                a[q * n + p] = 0.0;
            }
        }
    }
    let mut values: Vec<f64> = (0..n).map(|index| a[index * n + index]).collect();
    values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    values
}

/// Hermitian（m×m，row-major (re,im)）的特征值（升序；实化后成对平均）。
fn hermitian_eigenvalues(gram: &[C], m: usize) -> Vec<f64> {
    let n = 2 * m;
    let mut real = vec![0.0f64; n * n];
    for a in 0..m {
        for b in 0..m {
            let value = gram[a * m + b];
            real[a * n + b] = value.0;
            real[(m + a) * n + b] = value.1;
            real[a * n + (m + b)] = -value.1;
            real[(m + a) * n + (m + b)] = value.0;
        }
    }
    let values = symmetric_eigenvalues(&real, n);
    let mut paired = Vec::with_capacity(m);
    let mut index = 0;
    while index + 1 < values.len() {
        paired.push(0.5 * (values[index] + values[index + 1]));
        index += 2;
    }
    paired
}

// ===========================================================================
// 模式 / 状态 / 来源
// ===========================================================================

/// 求解模式（由上游目标模式派生；Custom 供合成夹具/离线原型）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolveMode {
    /// T50 Preserve：目标=实测 H_eff，期望 G=deltaD·I（D=0）。
    Preserve,
    /// T50 Reduced：目标=声明的更短衰减目标，求模态残余修正 g。
    Reduced,
    /// 自定义合成夹具：H/target/D 由调用方显式给出（本文件不做目标合法性推断）。
    Custom,
}

impl SolveMode {
    /// 稳定字面值。
    pub const fn as_str(self) -> &'static str {
        match self {
            SolveMode::Preserve => "preserve",
            SolveMode::Reduced => "reduced",
            SolveMode::Custom => "custom",
        }
    }
}

/// 求解状态（诚实：只表达求解/可辨识性/资源，不表达实测改善）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolveStatus {
    /// Preserve：输出恒等基线 G=deltaD·I，未求修正。
    PreserveBaseline,
    /// 全部输入 CGLS 收敛且可辨识性无阻塞。
    Converged,
    /// CGLS 未达收敛门（保留 best-effort 解；不得当已收敛使用）。
    NonConverged,
    /// 设计点欠定（P<M 或 FIR 秩上界不足）：最小范数解非唯一，不得宣称可辨识。
    Underdetermined,
    /// 资源上界拒绝硬算（solution=None；报告超界维度）。
    ResourceLimited,
}

impl SolveStatus {
    /// 稳定字面值。
    pub const fn as_str(self) -> &'static str {
        match self {
            SolveStatus::PreserveBaseline => "preserve_baseline",
            SolveStatus::Converged => "converged",
            SolveStatus::NonConverged => "non_converged",
            SolveStatus::Underdetermined => "underdetermined",
            SolveStatus::ResourceLimited => "resource_limited",
        }
    }

    /// 是否给出可用（可交付到下游）的解：Preserve/Converged 为 true（仍非实测）。
    pub const fn is_usable(self) -> bool {
        matches!(self, SolveStatus::PreserveBaseline | SolveStatus::Converged)
    }
}

/// 输入来源（T50 产物 hash 链；Custom 合成夹具为 None）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolveProvenance {
    /// 冻结 P 基线 hash。
    pub baseline_hash: String,
    /// H_eff plant hash（含 holdout；provenance）。
    pub plant_hash: String,
    /// design-only plant 摘要（目标身份）。
    pub design_plant_hash: String,
    /// 目标矩阵 hash。
    pub target_sha256: String,
    /// 公共整数 D0（provenance；本文件不再扣相位）。
    pub common_d0_samples: i64,
    /// 上游 TargetStatus 字面值（只作可行性输入，不当实测）。
    pub target_status: &'static str,
}

// ===========================================================================
// 问题（design positions + 对角目标）
// ===========================================================================

/// 一个 design position：每 participant 的 H_eff 行向量 + 每输入的对角目标响应。
///
/// h_eff[j] 是输出/participant dense 索引 j 的复响应（MIMO 行 H_{p,j}(·)）；
/// target[i] 是输入 dense 索引 i 的对角目标 d_{p,i}(·)（T50 非对角结构零）。
#[derive(Debug, Clone, PartialEq)]
pub struct SolvePosition {
    /// position label（T50 design position label）。
    pub label: String,
    /// 位置权重（>0；加权最小二乘）。
    pub weight: f64,
    /// H_eff：长度 M，h_eff[j] 按网格点。
    pub h_eff: Vec<Vec<ComplexPoint>>,
    /// 对角目标：长度 M，target[i] 按网格点。
    pub target: Vec<Vec<ComplexPoint>>,
}

/// 求解问题（只含 design positions；validation/holdout 永不进入）。
#[derive(Debug, Clone, PartialEq)]
pub struct SolveProblem {
    /// 参与者角色（dense 顺序；G 的行/列都是它）。
    pub roles: Vec<String>,
    /// 联合频率网格（严格升序、>0、<=fs/2）。
    pub grid_hz: Vec<f64>,
    /// 采样率（P1 契约）。
    pub sample_rate_hz: u32,
    /// design positions（>=1）。
    pub positions: Vec<SolvePosition>,
    /// 求解模式。
    pub mode: SolveMode,
    /// 目标是否 per-position（T50 preserve=true；reduced=false）。
    pub target_position_variant: bool,
    /// 目标声明的附加延迟（samples@fs；G 基线 D 必须与之对齐）。
    pub target_delay_samples: u32,
    /// T50 产物 hash 链（Custom 合成夹具为 None）。
    pub provenance: Option<SolveProvenance>,
}

impl SolveProblem {
    /// schema。
    pub const SCHEMA: &'static str = SOLVE_SCHEMA;

    /// M（participants 数）。
    pub fn m(&self) -> usize {
        self.roles.len()
    }

    /// 网格点数。
    pub fn grid_points(&self) -> usize {
        self.grid_hz.len()
    }

    /// 从 T50 产物构造（只取 design positions + 对角 target cells；校验绑定一致）。
    pub fn from_target_plant(
        target: &TargetMatrix,
        plant: &EffectivePlant,
    ) -> Result<Self, SolveError> {
        if plant.sample_rate_hz() != target.sample_rate_hz() {
            return Err(err(
                SolveErrorKind::SampleRate,
                "solve.sample_rate_hz",
                format!(
                    "plant fs {} != target fs {}",
                    plant.sample_rate_hz(),
                    target.sample_rate_hz()
                ),
            ));
        }
        if !SAMPLE_RATES_HZ.contains(&target.sample_rate_hz()) {
            return Err(err(
                SolveErrorKind::SampleRate,
                "solve.sample_rate_hz",
                format!(
                    "target fs {} is not a P1 design rate {:?}",
                    target.sample_rate_hz(),
                    SAMPLE_RATES_HZ
                ),
            ));
        }
        if plant.baseline_hash() != target.baseline_hash() {
            return Err(err(
                SolveErrorKind::Baseline,
                "solve.baseline_hash",
                "H_eff plant was built from a different frozen P baseline than the target",
            ));
        }
        if plant.participants().binding_hash() != target.participants().binding_hash() {
            return Err(err(
                SolveErrorKind::Participants,
                "solve.participants",
                "plant participants differ from the target participants (G would not be interchangeable)",
            ));
        }
        if plant.grid_hz().len() != target.grid_hz().len()
            || !plant
                .grid_hz()
                .iter()
                .zip(target.grid_hz().iter())
                .all(|(left, right)| grid_freq_equal(*left, *right))
        {
            return Err(err(
                SolveErrorKind::Grid,
                "solve.grid_hz",
                "plant grid differs from the target grid; G is undefined on mismatched grids",
            ));
        }
        if !target.verify_holdout_isolated() {
            return Err(err(
                SolveErrorKind::Target,
                "solve.target",
                "target cells reference validation/holdout labels; holdout must never enter the solve",
            ));
        }
        let m = target.participants().m();
        if m == 0 || m > MAX_PARTICIPANTS || m > MAX_LOW_BAND_PARTICIPANTS {
            return Err(err(
                SolveErrorKind::Unsupported,
                "solve.participants",
                format!(
                    "M={m} participants unsupported (P1 4.1.2/2.0 <= {MAX_LOW_BAND_PARTICIPANTS}; 5.3.2 M=8 is NOT_VERIFIED)"
                ),
            ));
        }
        if target.profile().m() != m {
            return Err(err(
                SolveErrorKind::Participants,
                "solve.profile",
                format!(
                    "profile {} expects M={} but target declares M={m}",
                    target.profile().as_str(),
                    target.profile().m()
                ),
            ));
        }
        let mut roles = Vec::with_capacity(m);
        for (index, participant) in target.participants().participants().iter().enumerate() {
            if participant.dense_index != index as u16 {
                return Err(err(
                    SolveErrorKind::Participants,
                    format!("solve.roles[{index}]"),
                    format!(
                        "participant dense_index {} is not dense-contiguous (expected {index})",
                        participant.dense_index
                    ),
                ));
            }
            if !is_role_token(&participant.role_id) {
                return Err(err(
                    SolveErrorKind::Participants,
                    format!("solve.roles[{index}]"),
                    format!(
                        "role id {:?} must match ^[A-Z][A-Z0-9]*$",
                        participant.role_id
                    ),
                ));
            }
            roles.push(participant.role_id.clone());
        }
        let preserve = target.decay().is_preserve();
        let target_delay = target.target_delay_samples();
        if preserve && target_delay != 0 {
            return Err(err(
                SolveErrorKind::Delay,
                "solve.target_delay_samples",
                "preserve targets carry no declared target delay; nonzero target delay is ambiguous (refusing)",
            ));
        }
        let variant = target.is_position_variant();
        if preserve != variant {
            return Err(err(
                SolveErrorKind::Target,
                "solve.target_position_variant",
                format!(
                    "target variant={variant} is inconsistent with decay mode preserve={preserve} (T50: preserve is per-position, reduced is position-invariant)"
                ),
            ));
        }
        let mut positions = Vec::new();
        for position in plant.design_positions() {
            if positions.len() >= MAX_SOLVE_POSITIONS {
                return Err(err(
                    SolveErrorKind::Unsupported,
                    "solve.positions",
                    format!("more than {MAX_SOLVE_POSITIONS} design positions unsupported"),
                ));
            }
            let mut h_eff: Vec<Vec<ComplexPoint>> = Vec::with_capacity(m);
            for participant in target.participants().participants() {
                let input = position.input(&participant.role_id).ok_or_else(|| {
                    err(
                        SolveErrorKind::Missing,
                        format!("solve.h_eff.{}.{}", position.label, participant.role_id),
                        "design position has no H_eff for this participant",
                    )
                })?;
                if input.dense_index != participant.dense_index {
                    return Err(err(
                        SolveErrorKind::Participants,
                        format!("solve.h_eff.{}.{}", position.label, participant.role_id),
                        format!(
                            "H_eff dense_index {} != participant dense_index {}",
                            input.dense_index, participant.dense_index
                        ),
                    ));
                }
                if input.points.len() != target.grid_hz().len() {
                    return Err(err(
                        SolveErrorKind::Grid,
                        format!("solve.h_eff.{}.{}", position.label, participant.role_id),
                        "H_eff point count differs from the target grid",
                    ));
                }
                h_eff.push(input.points.clone());
            }
            let mut target_rows: Vec<Vec<ComplexPoint>> = Vec::with_capacity(m);
            for dense in 0..m {
                let label = if variant {
                    Some(position.label.as_str())
                } else {
                    None
                };
                match target.cell(label, dense as u16, dense as u16) {
                    TargetCellView::Response(points) => {
                        if points.len() != target.grid_hz().len() {
                            return Err(err(
                                SolveErrorKind::Grid,
                                format!("solve.target[{dense}]"),
                                "target point count differs from the target grid",
                            ));
                        }
                        target_rows.push(points.to_vec());
                    }
                    TargetCellView::Zero => {
                        return Err(err(
                            SolveErrorKind::Missing,
                            format!("solve.target[{dense}]"),
                            "target diagonal cell is missing for this participant/position",
                        ));
                    }
                }
            }
            positions.push(SolvePosition {
                label: position.label.clone(),
                weight: position.weight,
                h_eff,
                target: target_rows,
            });
        }
        if positions.is_empty() {
            return Err(err(
                SolveErrorKind::Missing,
                "solve.positions",
                "no design position in H_eff plant; G is not identifiable from holdout only",
            ));
        }
        Ok(Self {
            roles,
            grid_hz: target.grid_hz().to_vec(),
            sample_rate_hz: target.sample_rate_hz(),
            positions,
            mode: if preserve {
                SolveMode::Preserve
            } else {
                SolveMode::Reduced
            },
            target_position_variant: variant,
            target_delay_samples: target_delay,
            provenance: Some(SolveProvenance {
                baseline_hash: target.baseline_hash().to_string(),
                plant_hash: target.plant_hash().to_string(),
                design_plant_hash: target.design_plant_hash().to_string(),
                target_sha256: target.target_sha256(),
                common_d0_samples: target.common_d0().samples(),
                target_status: target.status().as_str(),
            }),
        })
    }

    /// 校验问题（结构/网格/目标/delay/来源）。
    pub fn validate(&self) -> Result<(), SolveError> {
        let m = self.m();
        if m == 0 || m > MAX_PARTICIPANTS || m > MAX_LOW_BAND_PARTICIPANTS {
            return Err(err(
                SolveErrorKind::Unsupported,
                "solve.roles",
                format!("M={m} unsupported (1..={MAX_LOW_BAND_PARTICIPANTS})"),
            ));
        }
        for (index, role) in self.roles.iter().enumerate() {
            if !is_role_token(role) {
                return Err(err(
                    SolveErrorKind::Participants,
                    format!("solve.roles[{index}]"),
                    format!("role id {role:?} must match ^[A-Z][A-Z0-9]*$"),
                ));
            }
            if self.roles[..index].iter().any(|existing| existing == role) {
                return Err(err(
                    SolveErrorKind::Participants,
                    format!("solve.roles[{index}]"),
                    format!("duplicate role {role}"),
                ));
            }
        }
        if !SAMPLE_RATES_HZ.contains(&self.sample_rate_hz) {
            return Err(err(
                SolveErrorKind::SampleRate,
                "solve.sample_rate_hz",
                format!(
                    "sample rate {} is not a P1 design rate {:?}",
                    self.sample_rate_hz, SAMPLE_RATES_HZ
                ),
            ));
        }
        if self.grid_hz.len() < 2 || self.grid_hz.len() > MAX_SOLVE_GRID_POINTS {
            return Err(err(
                SolveErrorKind::Grid,
                "solve.grid_hz",
                format!(
                    "grid has {} points; supported 2..={MAX_SOLVE_GRID_POINTS}",
                    self.grid_hz.len()
                ),
            ));
        }
        let nyquist = self.sample_rate_hz as f64 / 2.0;
        let mut previous = f64::NEG_INFINITY;
        for (index, freq) in self.grid_hz.iter().copied().enumerate() {
            let freq = finite(freq, &format!("solve.grid_hz[{index}]"))?;
            if freq <= 0.0 || freq > nyquist || freq <= previous {
                return Err(err(
                    SolveErrorKind::Grid,
                    format!("solve.grid_hz[{index}]"),
                    format!(
                        "grid must be positive, <= Nyquist {nyquist}, strictly increasing; got {freq} after {previous}"
                    ),
                ));
            }
            previous = freq;
        }
        if self.positions.is_empty() || self.positions.len() > MAX_SOLVE_POSITIONS {
            return Err(err(
                SolveErrorKind::Position,
                "solve.positions",
                format!(
                    "design positions must be 1..={MAX_SOLVE_POSITIONS}, got {}",
                    self.positions.len()
                ),
            ));
        }
        let mut labels: Vec<&str> = Vec::with_capacity(self.positions.len());
        for (index, position) in self.positions.iter().enumerate() {
            if !is_label_token(&position.label) {
                return Err(err(
                    SolveErrorKind::Position,
                    format!("solve.positions[{index}].label"),
                    format!(
                        "position label {:?} must be a stable id token",
                        position.label
                    ),
                ));
            }
            if labels
                .iter()
                .any(|existing| *existing == position.label.as_str())
            {
                return Err(err(
                    SolveErrorKind::Position,
                    format!("solve.positions[{index}].label"),
                    format!("duplicate position label {}", position.label),
                ));
            }
            labels.push(position.label.as_str());
            finite_positive(position.weight, &format!("solve.positions[{index}].weight"))?;
            if position.h_eff.len() != m || position.target.len() != m {
                return Err(err(
                    SolveErrorKind::Position,
                    format!("solve.positions[{index}]"),
                    format!(
                        "position must have M={m} H_eff rows and target rows; got {} / {}",
                        position.h_eff.len(),
                        position.target.len()
                    ),
                ));
            }
            for dense in 0..m {
                if position.h_eff[dense].len() != self.grid_hz.len() {
                    return Err(err(
                        SolveErrorKind::Grid,
                        format!("solve.positions[{index}].h_eff[{dense}]"),
                        format!(
                            "H_eff has {} points but grid has {}",
                            position.h_eff[dense].len(),
                            self.grid_hz.len()
                        ),
                    ));
                }
                if position.target[dense].len() != self.grid_hz.len() {
                    return Err(err(
                        SolveErrorKind::Grid,
                        format!("solve.positions[{index}].target[{dense}]"),
                        format!(
                            "target has {} points but grid has {}",
                            position.target[dense].len(),
                            self.grid_hz.len()
                        ),
                    ));
                }
                for (point_index, (h_point, t_point)) in position.h_eff[dense]
                    .iter()
                    .zip(position.target[dense].iter())
                    .enumerate()
                {
                    let expected = self.grid_hz[point_index];
                    if !grid_freq_equal(h_point.freq_hz, expected) {
                        return Err(err(
                            SolveErrorKind::Grid,
                            format!(
                                "solve.positions[{index}].h_eff[{dense}][{point_index}].freq_hz"
                            ),
                            format!(
                                "H_eff frequency {} does not match grid {expected}",
                                h_point.freq_hz
                            ),
                        ));
                    }
                    if !grid_freq_equal(t_point.freq_hz, expected) {
                        return Err(err(
                            SolveErrorKind::Grid,
                            format!(
                                "solve.positions[{index}].target[{dense}][{point_index}].freq_hz"
                            ),
                            format!(
                                "target frequency {} does not match grid {expected}",
                                t_point.freq_hz
                            ),
                        ));
                    }
                    for value in [h_point.re, h_point.im, t_point.re, t_point.im] {
                        if !value.is_finite() || value.abs() > SOLVE_MAGNITUDE_SANITY {
                            return Err(err(
                                SolveErrorKind::Range,
                                format!("solve.positions[{index}]"),
                                format!(
                                    "complex value {value} is non-finite or beyond sanity {SOLVE_MAGNITUDE_SANITY}"
                                ),
                            ));
                        }
                    }
                }
            }
        }
        match self.mode {
            SolveMode::Preserve => {
                if self.target_delay_samples != 0 {
                    return Err(err(
                        SolveErrorKind::Delay,
                        "solve.target_delay_samples",
                        "preserve mode requires target-delay 0 (target == measured H_eff; no extra delay)",
                    ));
                }
                if !self.target_position_variant {
                    return Err(err(
                        SolveErrorKind::Target,
                        "solve.target_position_variant",
                        "preserve targets must be per-position (T50: target = H_eff per design position)",
                    ));
                }
                for (position_index, position) in self.positions.iter().enumerate() {
                    for dense in 0..m {
                        for (point_index, (h_point, t_point)) in position.h_eff[dense]
                            .iter()
                            .zip(position.target[dense].iter())
                            .enumerate()
                        {
                            if !points_equal(h_point, t_point) {
                                return Err(err(
                                    SolveErrorKind::Target,
                                    format!(
                                        "solve.positions[{position_index}].target[{dense}][{point_index}]"
                                    ),
                                    "preserve target must equal H_eff exactly (mode=preserve does not invent a target)",
                                ));
                            }
                        }
                    }
                }
            }
            SolveMode::Reduced => {
                if self.target_position_variant {
                    return Err(err(
                        SolveErrorKind::Target,
                        "solve.target_position_variant",
                        "reduced targets are position-invariant (T50 power-weighted design aggregation)",
                    ));
                }
                let first = &self.positions[0];
                for (position_index, position) in self.positions.iter().enumerate().skip(1) {
                    for dense in 0..m {
                        for (point_index, (left, right)) in first.target[dense]
                            .iter()
                            .zip(position.target[dense].iter())
                            .enumerate()
                        {
                            if !points_equal(left, right) {
                                return Err(err(
                                    SolveErrorKind::Target,
                                    format!(
                                        "solve.positions[{position_index}].target[{dense}][{point_index}]"
                                    ),
                                    "position-invariant target must be identical across design positions",
                                ));
                            }
                        }
                    }
                }
            }
            SolveMode::Custom => {}
        }
        if let Some(provenance) = &self.provenance {
            for (name, hash) in [
                ("baseline_hash", provenance.baseline_hash.as_str()),
                ("plant_hash", provenance.plant_hash.as_str()),
                ("design_plant_hash", provenance.design_plant_hash.as_str()),
                ("target_sha256", provenance.target_sha256.as_str()),
            ] {
                if !is_sha256_hex(hash) {
                    return Err(err(
                        SolveErrorKind::Baseline,
                        format!("solve.provenance.{name}"),
                        format!("expected 64 lowercase hex sha256, got {hash:?}"),
                    ));
                }
            }
            if provenance.common_d0_samples.abs() > 1_000_000 {
                return Err(err(
                    SolveErrorKind::Range,
                    "solve.provenance.common_d0_samples",
                    "common D0 provenance outside sanity range",
                ));
            }
        }
        Ok(())
    }

    /// 规范化文本（确定性；供 hash/stale）。
    pub fn canonical(&self) -> String {
        let mut canonical = String::new();
        canonical.push_str(SOLVE_PROBLEM_HASH_DOMAIN);
        canonical.push('\n');
        canonical.push_str(&format!(
            "schema={} mode={} fs={} m={} grid_points={} variant={} target_delay={}\n",
            SOLVE_SCHEMA,
            self.mode.as_str(),
            self.sample_rate_hz,
            self.m(),
            self.grid_hz.len(),
            self.target_position_variant,
            self.target_delay_samples
        ));
        for freq in &self.grid_hz {
            canonical.push_str(&fmt_f64(*freq));
            canonical.push('\n');
        }
        for role in &self.roles {
            canonical.push_str(&format!("role={role}\n"));
        }
        for position in &self.positions {
            canonical.push_str(&format!(
                "position={} weight={}\n",
                position.label,
                fmt_f64(position.weight)
            ));
            for dense in 0..self.m() {
                canonical.push_str(&format!(
                    "h_eff[{dense}]={}\n",
                    points_sha256(&position.h_eff[dense])
                ));
                canonical.push_str(&format!(
                    "target[{dense}]={}\n",
                    points_sha256(&position.target[dense])
                ));
            }
        }
        match &self.provenance {
            Some(provenance) => {
                canonical.push_str(&format!(
                    "provenance baseline={} plant={} design_plant={} target={} common_d0={} status={}\n",
                    provenance.baseline_hash,
                    provenance.plant_hash,
                    provenance.design_plant_hash,
                    provenance.target_sha256,
                    provenance.common_d0_samples,
                    provenance.target_status
                ));
            }
            None => canonical.push_str("provenance=synthetic\n"),
        }
        canonical
    }

    /// 问题 hash（输入身份；确定性）。
    pub fn hash(&self) -> String {
        sha256_hex(self.canonical().as_bytes())
    }
}

// ===========================================================================
// 选项
// ===========================================================================

/// 求解选项（T51 原语；lambda/白名单/CV 策略归 T52，本文件只接受显式值）。
#[derive(Debug, Clone, PartialEq)]
pub struct SolveOptions {
    /// G 支撑 L_G（1..=4096；taps 是支撑长度，不是线性相位）。
    pub taps: usize,
    /// G 的公共整数基线延迟 D（samples@fs；D+1<=L_G）。
    pub delay_samples: u32,
    /// 拟合带 [lo,hi]（默认 20-300 Hz；须落在 (0, fs/2]）。
    pub fit_band_hz: (f64, f64),
    /// Tikhonov ridge lambda（>=0；T52 正则时由其传入；默认 0 = 纯最小二乘）。
    pub ridge: f64,
    /// CGLS 最大迭代数。
    pub max_iterations: usize,
    /// 相对收敛门（法方程梯度范数相对初值）。
    pub rel_tolerance: f64,
    /// 每输入未知量上限（M×L_G）。
    pub max_unknowns: usize,
    /// 复数行上限（positions×fit 频点）。
    pub max_rows: usize,
    /// 工作量上限（复数乘加；含正/伴随与响应评估上界）。
    pub max_work: u64,
    /// 可选路径掩码（长度 M×M，row-major out*M+in；true=自由）。
    /// 供 T52 白名单/稀疏选择使用；None = 全部路径自由。
    pub path_mask: Option<Vec<bool>>,
    /// 可选每频点权重（长度 = 网格点数；只作用于拟合带内；None = 1）。
    /// 供 T52 频带/可信度加权；必须有限且 >0。
    pub frequency_weights: Option<Vec<f64>>,
}

impl Default for SolveOptions {
    fn default() -> Self {
        Self {
            taps: DEFAULT_TAPS,
            delay_samples: 0,
            fit_band_hz: DEFAULT_FIT_BAND_HZ,
            ridge: DEFAULT_RIDGE,
            max_iterations: DEFAULT_MAX_ITERATIONS,
            rel_tolerance: DEFAULT_REL_TOLERANCE,
            max_unknowns: DEFAULT_MAX_UNKNOWNS,
            max_rows: DEFAULT_MAX_ROWS,
            max_work: DEFAULT_MAX_WORK,
            path_mask: None,
            frequency_weights: None,
        }
    }
}

impl SolveOptions {
    /// 校验选项与问题的一致性（含 mode 的 D/target-delay 对齐规则）。
    pub fn validate(&self, problem: &SolveProblem) -> Result<(), SolveError> {
        if self.taps == 0 || self.taps > MAX_SOLVE_TAPS {
            return Err(err(
                SolveErrorKind::Options,
                "solve.options.taps",
                format!(
                    "taps must be 1..={MAX_SOLVE_TAPS} (G support cap TAPS_NORMAL_MAX), got {}",
                    self.taps
                ),
            ));
        }
        if self.delay_samples as usize >= self.taps {
            return Err(err(
                SolveErrorKind::Options,
                "solve.options.delay_samples",
                format!(
                    "G support L_G={} must be >= D+1={} (delta_D baseline must fit in support)",
                    self.taps,
                    self.delay_samples as u64 + 1
                ),
            ));
        }
        let fs = problem.sample_rate_hz as f64;
        let (lo, hi) = self.fit_band_hz;
        let lo = finite_positive(lo, "solve.options.fit_band_hz.0")?;
        let hi = finite_positive(hi, "solve.options.fit_band_hz.1")?;
        if !(lo < hi) || hi > fs / 2.0 {
            return Err(err(
                SolveErrorKind::Grid,
                "solve.options.fit_band_hz",
                format!(
                    "fit band [{lo},{hi}] must satisfy 0<lo<hi<=Nyquist {}",
                    fs / 2.0
                ),
            ));
        }
        if !self.ridge.is_finite() || self.ridge < 0.0 || self.ridge > MAX_SOLVE_RIDGE {
            return Err(err(
                SolveErrorKind::Options,
                "solve.options.ridge",
                format!(
                    "ridge must be finite in [0,{MAX_SOLVE_RIDGE}] (lambda policy belongs to T52), got {}",
                    self.ridge
                ),
            ));
        }
        if self.max_iterations == 0 || self.max_iterations > MAX_SOLVE_ITERATIONS {
            return Err(err(
                SolveErrorKind::Options,
                "solve.options.max_iterations",
                format!(
                    "max_iterations must be 1..={MAX_SOLVE_ITERATIONS}, got {}",
                    self.max_iterations
                ),
            ));
        }
        if !self.rel_tolerance.is_finite()
            || self.rel_tolerance <= 0.0
            || self.rel_tolerance > MAX_SOLVE_REL_TOLERANCE
        {
            return Err(err(
                SolveErrorKind::Options,
                "solve.options.rel_tolerance",
                format!(
                    "rel_tolerance must be finite in (0,{MAX_SOLVE_REL_TOLERANCE}], got {}",
                    self.rel_tolerance
                ),
            ));
        }
        if self.max_unknowns == 0 || self.max_unknowns > MAX_SOLVE_UNKNOWNS_HARD {
            return Err(err(
                SolveErrorKind::Options,
                "solve.options.max_unknowns",
                format!(
                    "max_unknowns must be 1..={MAX_SOLVE_UNKNOWNS_HARD}, got {}",
                    self.max_unknowns
                ),
            ));
        }
        if self.max_rows == 0 || self.max_rows > MAX_SOLVE_ROWS_HARD {
            return Err(err(
                SolveErrorKind::Options,
                "solve.options.max_rows",
                format!(
                    "max_rows must be 1..={MAX_SOLVE_ROWS_HARD}, got {}",
                    self.max_rows
                ),
            ));
        }
        if self.max_work == 0 || self.max_work > MAX_SOLVE_WORK_HARD {
            return Err(err(
                SolveErrorKind::Options,
                "solve.options.max_work",
                format!(
                    "max_work must be 1..={MAX_SOLVE_WORK_HARD}, got {}",
                    self.max_work
                ),
            ));
        }
        let m = problem.m();
        if let Some(mask) = &self.path_mask {
            if mask.len() != m * m {
                return Err(err(
                    SolveErrorKind::Options,
                    "solve.options.path_mask",
                    format!(
                        "path_mask must have M*M={} entries (row-major out*M+in), got {}",
                        m * m,
                        mask.len()
                    ),
                ));
            }
            if !mask.iter().any(|free| *free) {
                return Err(err(
                    SolveErrorKind::Options,
                    "solve.options.path_mask",
                    "path_mask disables every path (empty constraint set is not a solve)",
                ));
            }
        }
        if let Some(weights) = &self.frequency_weights {
            if weights.len() != problem.grid_hz.len() {
                return Err(err(
                    SolveErrorKind::Options,
                    "solve.options.frequency_weights",
                    format!(
                        "frequency_weights must have one entry per grid point ({}), got {}",
                        problem.grid_hz.len(),
                        weights.len()
                    ),
                ));
            }
            for (index, weight) in weights.iter().copied().enumerate() {
                finite_positive(weight, &format!("solve.options.frequency_weights[{index}]"))?;
            }
        }
        match problem.mode {
            SolveMode::Preserve => {
                if self.delay_samples != 0 {
                    return Err(err(
                        SolveErrorKind::Delay,
                        "solve.options.delay_samples",
                        "preserve mode requires D=0 so the identity baseline reproduces the measured target",
                    ));
                }
            }
            SolveMode::Reduced => {
                if self.delay_samples != problem.target_delay_samples {
                    return Err(err(
                        SolveErrorKind::Delay,
                        "solve.options.delay_samples",
                        format!(
                            "G baseline D={} must equal the declared target delay {} (no post-hoc shift; T6 FC-04)",
                            self.delay_samples, problem.target_delay_samples
                        ),
                    ));
                }
            }
            SolveMode::Custom => {}
        }
        Ok(())
    }

    /// 规范化文本（确定性；供输出 hash）。
    pub fn canonical(&self, problem: &SolveProblem) -> String {
        let mut canonical = String::new();
        canonical.push_str(&format!(
            "options taps={} delay={} band=[{},{}] ridge={} max_iterations={} rel_tolerance={} max_unknowns={} max_rows={} max_work={}\n",
            self.taps,
            self.delay_samples,
            fmt_f64(self.fit_band_hz.0),
            fmt_f64(self.fit_band_hz.1),
            fmt_f64(self.ridge),
            self.max_iterations,
            fmt_f64(self.rel_tolerance),
            self.max_unknowns,
            self.max_rows,
            self.max_work
        ));
        match &self.path_mask {
            Some(mask) => {
                let bits: String = mask
                    .iter()
                    .map(|free| if *free { '1' } else { '0' })
                    .collect();
                canonical.push_str(&format!("path_mask={bits}\n"));
            }
            None => canonical.push_str("path_mask=all\n"),
        }
        if let Some(weights) = &self.frequency_weights {
            let mut text = String::new();
            for weight in weights {
                text.push_str(&fmt_f64(*weight));
                text.push('\n');
            }
            canonical.push_str(&format!(
                "frequency_weights_sha={}\n",
                sha256_hex(text.as_bytes())
            ));
        } else {
            canonical.push_str("frequency_weights=uniform\n");
        }
        canonical.push_str(&format!("problem_hash={}\n", problem.hash()));
        canonical
    }
}

// ===========================================================================
// 报告
// ===========================================================================

/// 单输入列的 CGLS 结果。
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnReport {
    /// 输入 dense 索引。
    pub input_dense: u16,
    /// 自由路径数（掩码后）。
    pub free_paths: usize,
    /// 未知量（free_paths×taps）。
    pub unknowns: usize,
    /// CGLS 实际迭代数。
    pub iterations: usize,
    /// ||r||/||b||（加权最小二乘残差；b=0 时为 0）。
    pub relative_residual: f64,
    /// 法方程梯度相对初值（sqrt(gamma/gamma0)）。
    pub normal_residual: f64,
    /// 是否达到收敛门。
    pub converged: bool,
    /// 停止原因（tolerance/max_iterations/stagnation/zero_rhs/preserve_baseline/no_free_paths）。
    pub stop_reason: &'static str,
}

/// 可辨识性报告（频率域 position×participant 秩 + FIR 秩上界；**必要条件，不是充分条件**）。
#[derive(Debug, Clone, PartialEq)]
pub struct IdentifiabilityReport {
    /// 参与者数 M。
    pub participants: usize,
    /// design position 数 P。
    pub design_positions: usize,
    /// 拟合带内 position-mixing 矩阵秩最小值。
    pub per_frequency_rank_min: usize,
    /// 拟合带内 position-mixing 矩阵秩最大值。
    pub per_frequency_rank_max: usize,
    /// M - rank_min：每频点零空间维数下界。
    pub position_deficit: usize,
    /// 满秩频点的最大条件数估计（sqrt(lambda_max/lambda_min)）；有欠定频点时为 None。
    pub condition_max: Option<f64>,
    /// FIR 系统秩上界 min(2*P*F, max 每输入未知量)。
    pub fir_rank_bound: usize,
    /// 每输入最大未知量。
    pub unknowns_max_per_input: usize,
    /// 是否满足满列秩的必要条件（不保证实际满秩；由夹具/残差另行验证）。
    pub potentially_full_column_rank: bool,
    /// 是否欠定（position_deficit>0 或秩上界<未知量）。
    pub underdetermined: bool,
    /// 说明（诚实边界）。
    pub note: &'static str,
}

impl IdentifiabilityReport {
    /// 频率域秩判零说明（报告用）。
    pub const NOTE: &'static str = "necessary-only: per-frequency rank of stacked design-position rows and a FIR rank upper bound; actual FIR column rank is verified by fixtures, not by this estimate";
}

/// 资源上界报告（报告额度与实际用量；超界时 solution=None）。
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceReport {
    /// 每输入未知量上限（options）。
    pub max_unknowns_limit: usize,
    /// 复数行上限（options）。
    pub complex_rows_limit: usize,
    /// 工作量上限（options）。
    pub work_limit: u64,
    /// 估算工作量上界（复数乘加；按 max_iterations 的保守上界）。
    pub estimated_work: u64,
    /// 实际工作量（按实际迭代/响应评估计数）。
    pub work_used: u64,
    /// 内存上界估计（字节；工作向量 + 输入副本 + 解/响应）。
    pub memory_bound_bytes: u64,
    /// 是否在全部上界内。
    pub within_limits: bool,
    /// 首个超界维度（None=未超界）。
    pub limited_by: Option<&'static str>,
}

/// 求解报告（诚实：predicted-only；不含实测改善）。
#[derive(Debug, Clone, PartialEq)]
pub struct SolveReport {
    /// schema。
    pub schema: &'static str,
    /// 状态。
    pub status: SolveStatus,
    /// 模式。
    pub mode: SolveMode,
    /// M。
    pub m: usize,
    /// G 支撑 L_G。
    pub taps: usize,
    /// G 基线延迟 D。
    pub delay_samples: u32,
    /// fs。
    pub sample_rate_hz: u32,
    /// 拟合带。
    pub fit_band_hz: (f64, f64),
    /// 拟合带内网格点数。
    pub band_points: usize,
    /// 网格点数。
    pub grid_points: usize,
    /// design position 数。
    pub design_positions: usize,
    /// position 权重（报告用；顺序同 positions）。
    pub position_weights: Vec<f64>,
    /// 目标是否 per-position。
    pub target_position_variant: bool,
    /// 未知量总数（Sum_i free_i×taps）。
    pub unknowns_total: usize,
    /// 每输入最大未知量。
    pub unknowns_max_per_input: usize,
    /// 每输入复数行数（P×F）。
    pub complex_rows: usize,
    /// 逐输入列报告。
    pub columns: Vec<ColumnReport>,
    /// 总迭代数。
    pub iterations_total: usize,
    /// 最大相对残差（无列时为 0）。
    pub relative_residual_max: f64,
    /// 最大法方程梯度残差。
    pub normal_residual_max: f64,
    /// design positions、改善带（63-250 Hz）内预测最大相对残差（dB）。
    pub predicted_design_residual_max_db: f64,
    /// design positions、拟合带内预测最大相对残差（dB）。
    pub predicted_fit_band_residual_max_db: f64,
    /// 最坏 (position, role, residual_db)。
    pub predicted_residual_worst: Option<(String, String, f64)>,
    /// 拟合带内近零目标（|d|<近零 floor）的 (position,input,频点) 计数；这些点不参与相对残差
    /// 统计（相对误差在零目标上无定义），只计入本字段，不伪装成拟合失败/成功。
    pub near_zero_target_points: usize,
    /// 改善带内非对角 cell 最大幅度（dB；floor -240）。
    pub offdiag_max_db: f64,
    /// 改善带内对角相对 0 dB 最大偏离（dB）。
    pub diag_deviation_max_db: f64,
    /// 修正系数最大幅度（dB；全零时 None）。
    pub correction_coeff_max_db: Option<f64>,
    /// Preserve 模式期望恒等基线。
    pub preserve_expected_identity: bool,
    /// 可辨识性报告。
    pub identifiability: IdentifiabilityReport,
    /// 资源报告。
    pub resource: ResourceReport,
    /// T50 来源 hash 链（Custom 为 None）。
    pub provenance: Option<SolveProvenance>,
    /// holdout 是否参与调参（恒 false）。
    pub holdout_used_for_tuning: bool,
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
    pub warnings: Vec<SolveWarning>,
}

/// 求解产物（G 复响应 + 因果 FIR 系数；布局与 T07 kernel 对齐）。
#[derive(Debug, Clone, PartialEq)]
pub struct SolveSolution {
    /// 完整 G（含对角 deltaD 基线与非对角修正）在目标网格上的复响应。
    pub modal: ModalMatrix,
    /// 因果 FIR 修正系数 g[out][in][tap]（f64；长度 M*M*L_G；完整 G=deltaD·I+g）。
    pub coeffs_f64: Vec<f64>,
    /// 同布局 f32（T07 生产精度；设计后统一降位）。
    pub coeffs_f32: Vec<f32>,
    /// 系数 f64 规范化文本的 sha256（确定性）。
    pub coeffs_sha256: String,
    /// 解 hash（输入/选项/状态/系数；供 T53/T54/T66 hash 链）。
    pub hash: String,
}

impl SolveSolution {
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

/// 求解结果：报告 + 可选解（资源超界无解）。
#[derive(Debug, Clone, PartialEq)]
pub struct SolveOutcome {
    /// 报告。
    pub report: SolveReport,
    /// 解（ResourceLimited 时 None）。
    pub solution: Option<SolveSolution>,
}

// ===========================================================================
// CGLS 线性算子（matrix-free；因果 FIR 基）
// ===========================================================================

struct FirOperator<'a> {
    m: usize,
    positions: usize,
    band: usize,
    taps: usize,
    free: &'a [usize],
    hs: &'a [C],
    phase: &'a [C],
    ridge: f64,
}

impl<'a> FirOperator<'a> {
    fn unknowns(&self) -> usize {
        self.free.len() * self.taps
    }

    fn rows(&self) -> usize {
        self.positions * self.band
    }

    fn matvec_cost(&self) -> u64 {
        (self.free.len() * self.band * (self.taps + self.positions)) as u64
    }

    fn forward(&self, x: &[f64]) -> Vec<C> {
        let mut output = vec![(0.0, 0.0); self.rows()];
        for (free_index, output_path) in self.free.iter().copied().enumerate() {
            let base = free_index * self.taps;
            for k in 0..self.band {
                let step = self.phase[k];
                let mut acc = (0.0, 0.0);
                let mut power = (1.0, 0.0);
                for tap in 0..self.taps {
                    let coefficient = x[base + tap];
                    acc.0 += coefficient * power.0;
                    acc.1 += coefficient * power.1;
                    power = cmul(power, step);
                }
                for position in 0..self.positions {
                    let h = self.hs[(position * self.m + output_path) * self.band + k];
                    let offset = position * self.band + k;
                    output[offset].0 += h.0 * acc.0 - h.1 * acc.1;
                    output[offset].1 += h.0 * acc.1 + h.1 * acc.0;
                }
            }
        }
        output
    }

    fn adjoint(&self, y: &[C]) -> Vec<f64> {
        let mut output = vec![0.0f64; self.unknowns()];
        let mut weights = vec![(0.0, 0.0); self.band];
        for (free_index, output_path) in self.free.iter().copied().enumerate() {
            for weight in &mut weights {
                *weight = (0.0, 0.0);
            }
            for position in 0..self.positions {
                for k in 0..self.band {
                    let h = self.hs[(position * self.m + output_path) * self.band + k];
                    let value = y[position * self.band + k];
                    weights[k].0 += h.0 * value.0 + h.1 * value.1;
                    weights[k].1 += h.0 * value.1 - h.1 * value.0;
                }
            }
            let base = free_index * self.taps;
            for k in 0..self.band {
                let step = cconj(self.phase[k]);
                let weight = weights[k];
                let mut power = (1.0, 0.0);
                for tap in 0..self.taps {
                    output[base + tap] += weight.0 * power.0 - weight.1 * power.1;
                    power = cmul(power, step);
                }
            }
        }
        output
    }
}

struct CglsResult {
    x: Vec<f64>,
    iterations: usize,
    relative_residual: f64,
    normal_residual: f64,
    converged: bool,
    stop_reason: &'static str,
    work: u64,
}

/// 带阻尼 CGLS（初值 0 ⇒ 最小范数最小二乘解）。
fn cgls(operator: &FirOperator<'_>, b: &[C], max_iterations: usize, tolerance: f64) -> CglsResult {
    let mut x = vec![0.0f64; operator.unknowns()];
    let mut residual = b.to_vec();
    let bnorm2 = norm2_complex(&residual);
    let mut gradient = operator.adjoint(&residual);
    let gamma0 = real_dot(&gradient, &gradient);
    if !gamma0.is_finite() || !(gamma0 > 0.0) {
        let converged = gamma0 == 0.0 && bnorm2 == 0.0;
        return CglsResult {
            x,
            iterations: 0,
            relative_residual: if bnorm2 > 0.0 { 1.0 } else { 0.0 },
            normal_residual: if converged { 0.0 } else { 1.0 },
            converged,
            stop_reason: if bnorm2 == 0.0 {
                "zero_rhs"
            } else {
                "degenerate"
            },
            work: 0,
        };
    }
    let mut direction = gradient.clone();
    let mut gamma = gamma0;
    let mut work = 0u64;
    let mut iterations = 0usize;
    let mut converged = false;
    let mut stop_reason = "max_iterations";
    while iterations < max_iterations {
        let q = operator.forward(&direction);
        work += operator.matvec_cost();
        let qnorm2 = norm2_complex(&q);
        let pnorm2 = real_dot(&direction, &direction);
        let denominator = qnorm2 + operator.ridge * pnorm2;
        if !denominator.is_finite() || !(denominator > 0.0) {
            stop_reason = "stagnation";
            break;
        }
        let alpha = gamma / denominator;
        for (value, step) in x.iter_mut().zip(direction.iter()) {
            *value += alpha * step;
        }
        for (value, step) in residual.iter_mut().zip(q.iter()) {
            value.0 -= alpha * step.0;
            value.1 -= alpha * step.1;
        }
        gradient = operator.adjoint(&residual);
        work += operator.matvec_cost();
        if operator.ridge != 0.0 {
            for (value, current) in gradient.iter_mut().zip(x.iter()) {
                *value -= operator.ridge * current;
            }
        }
        iterations += 1;
        let gamma_new = real_dot(&gradient, &gradient);
        if !gamma_new.is_finite() {
            stop_reason = "stagnation";
            break;
        }
        let normal_rel = (gamma_new / gamma0).sqrt();
        if normal_rel <= tolerance {
            converged = true;
            stop_reason = "tolerance";
            gamma = gamma_new;
            break;
        }
        if !(gamma_new > 0.0) {
            stop_reason = "stagnation";
            gamma = gamma_new;
            break;
        }
        let beta = gamma_new / gamma;
        for (value, step) in direction.iter_mut().zip(gradient.iter()) {
            *value = *step + beta * *value;
        }
        gamma = gamma_new;
    }
    let rnorm2 = norm2_complex(&residual);
    let relative_residual = if bnorm2 > 0.0 {
        (rnorm2 / bnorm2).sqrt()
    } else {
        0.0
    };
    CglsResult {
        x,
        iterations,
        relative_residual,
        normal_residual: (gamma / gamma0).sqrt(),
        converged,
        stop_reason,
        work,
    }
}

// ===========================================================================
// 求解入口
// ===========================================================================

/// 从 T50 产物求解 G（只取 design positions + 对角目标；Preserve/Reduced 分派）。
pub fn solve_from_target_plant(
    target: &TargetMatrix,
    plant: &EffectivePlant,
    options: &SolveOptions,
) -> Result<SolveOutcome, SolveError> {
    let problem = SolveProblem::from_target_plant(target, plant)?;
    solve(&problem, options)
}

/// 求解：因果有限 FIR 最小范数最小二乘（逐输入 CGLS），返回完整报告 + 可选解。
///
/// 不 panic、不静默截断：非法输入 -> SolveError；资源超界 -> ResourceLimited +
/// solution=None；未收敛/欠定 -> 保留 best-effort 解 + 明确状态/警告。
pub fn solve(problem: &SolveProblem, options: &SolveOptions) -> Result<SolveOutcome, SolveError> {
    problem.validate()?;
    options.validate(problem)?;
    let m = problem.m();
    let taps = options.taps;
    let fs = problem.sample_rate_hz as f64;

    let band_indices: Vec<usize> = problem
        .grid_hz
        .iter()
        .enumerate()
        .filter(|(_, freq)| **freq >= options.fit_band_hz.0 && **freq <= options.fit_band_hz.1)
        .map(|(index, _)| index)
        .collect();
    if band_indices.len() < 2 {
        return Err(err(
            SolveErrorKind::Grid,
            "solve.fit_band_hz",
            format!(
                "fit band [{},{}] Hz contains {} grid point(s); at least 2 required",
                options.fit_band_hz.0,
                options.fit_band_hz.1,
                band_indices.len()
            ),
        ));
    }
    let band = band_indices.len();
    let positions = problem.positions.len();
    let grid_points = problem.grid_hz.len();

    let mut free_paths: Vec<Vec<usize>> = Vec::with_capacity(m);
    for input_dense in 0..m {
        let mut free = Vec::new();
        for output_dense in 0..m {
            let enabled = options
                .path_mask
                .as_ref()
                .map(|mask| mask[output_dense * m + input_dense])
                .unwrap_or(true);
            if enabled {
                free.push(output_dense);
            }
        }
        free_paths.push(free);
    }
    let unknowns_per_input: Vec<usize> = free_paths.iter().map(|free| free.len() * taps).collect();
    let unknowns_total: usize = unknowns_per_input.iter().sum();
    let unknowns_max = unknowns_per_input.iter().copied().max().unwrap_or(0);
    let complex_rows = positions * band;

    let mut estimated_work: u64 = 0;
    for free in &free_paths {
        let matvec = (2 * free.len() * band * (taps + positions)) as u64;
        estimated_work =
            estimated_work.saturating_add(matvec.saturating_mul(options.max_iterations as u64));
    }
    let response_work = (m as u64)
        .saturating_mul(m as u64)
        .saturating_mul(grid_points as u64)
        .saturating_mul(taps as u64);
    let residual_work = (positions as u64)
        .saturating_mul(m as u64)
        .saturating_mul(band as u64)
        .saturating_mul(m as u64);
    estimated_work = estimated_work
        .saturating_add(response_work)
        .saturating_add(residual_work);

    let memory_bound_bytes = 8u64
        .saturating_mul(unknowns_max as u64)
        .saturating_add(16u64.saturating_mul(complex_rows as u64))
        .saturating_add(16u64.saturating_mul((positions * m * band) as u64))
        .saturating_add(8u64.saturating_mul((m * m * taps) as u64))
        .saturating_add(16u64.saturating_mul((m * m * grid_points) as u64))
        .saturating_add(8u64.saturating_mul(unknowns_total as u64));

    let mut limited_by: Option<&'static str> = None;
    if unknowns_max > options.max_unknowns {
        limited_by = Some("unknowns");
    } else if complex_rows > options.max_rows {
        limited_by = Some("rows");
    } else if estimated_work > options.max_work {
        limited_by = Some("work");
    }
    let within_limits = limited_by.is_none();

    let identifiability = identifiability_report(problem, &band_indices, unknowns_max);

    if !within_limits {
        let mut warnings = Vec::new();
        push_warning(
            &mut warnings,
            SolveWarning::RESOURCE_BOUND,
            format!(
                "resource bound exceeded by {:?}: unknowns_max={} (limit {}), complex_rows={} (limit {}), estimated_work={} (limit {})",
                limited_by.unwrap_or("unknown"),
                unknowns_max,
                options.max_unknowns,
                complex_rows,
                options.max_rows,
                estimated_work,
                options.max_work
            ),
        );
        let report = SolveReport {
            schema: SOLVE_SCHEMA,
            status: SolveStatus::ResourceLimited,
            mode: problem.mode,
            m,
            taps,
            delay_samples: options.delay_samples,
            sample_rate_hz: problem.sample_rate_hz,
            fit_band_hz: options.fit_band_hz,
            band_points: band,
            grid_points,
            design_positions: positions,
            position_weights: problem
                .positions
                .iter()
                .map(|position| position.weight)
                .collect(),
            target_position_variant: problem.target_position_variant,
            unknowns_total,
            unknowns_max_per_input: unknowns_max,
            complex_rows,
            columns: Vec::new(),
            iterations_total: 0,
            relative_residual_max: 0.0,
            normal_residual_max: 0.0,
            predicted_design_residual_max_db: 0.0,
            predicted_fit_band_residual_max_db: 0.0,
            predicted_residual_worst: None,
            near_zero_target_points: 0,
            offdiag_max_db: 0.0,
            diag_deviation_max_db: 0.0,
            correction_coeff_max_db: None,
            preserve_expected_identity: problem.mode == SolveMode::Preserve,
            identifiability,
            resource: ResourceReport {
                max_unknowns_limit: options.max_unknowns,
                complex_rows_limit: options.max_rows,
                work_limit: options.max_work,
                estimated_work,
                work_used: 0,
                memory_bound_bytes,
                within_limits: false,
                limited_by,
            },
            provenance: problem.provenance.clone(),
            holdout_used_for_tuning: false,
            applied: false,
            post_apply_measurement: "not_requested",
            engine_compatibility: "not_validated",
            runtime_validation: "not_run",
            claims: SOLVE_CLAIM,
            warnings,
        };
        return Ok(SolveOutcome {
            report,
            solution: None,
        });
    }

    let mut row_scale = vec![0.0f64; positions * band];
    for (position_index, position) in problem.positions.iter().enumerate() {
        for (band_index, grid_index) in band_indices.iter().copied().enumerate() {
            let frequency_weight = options
                .frequency_weights
                .as_ref()
                .map(|weights| weights[grid_index])
                .unwrap_or(1.0);
            row_scale[position_index * band + band_index] =
                (position.weight * frequency_weight).sqrt();
        }
    }

    let mut h_band: Vec<C> = vec![(0.0, 0.0); positions * m * band];
    for position in 0..positions {
        for output_dense in 0..m {
            for (band_index, grid_index) in band_indices.iter().copied().enumerate() {
                let point = problem.positions[position].h_eff[output_dense][grid_index];
                h_band[(position * m + output_dense) * band + band_index] = (point.re, point.im);
            }
        }
    }
    let mut phase: Vec<C> = Vec::with_capacity(band);
    for grid_index in band_indices.iter().copied() {
        let angle = -2.0 * std::f64::consts::PI * problem.grid_hz[grid_index] / fs;
        phase.push((angle.cos(), angle.sin()));
    }
    let mut h_scaled = vec![(0.0, 0.0); h_band.len()];
    for position in 0..positions {
        for output_dense in 0..m {
            for band_index in 0..band {
                let scale = row_scale[position * band + band_index];
                let value = h_band[(position * m + output_dense) * band + band_index];
                h_scaled[(position * m + output_dense) * band + band_index] =
                    (value.0 * scale, value.1 * scale);
            }
        }
    }
    let mut delay_phase: Vec<C> = Vec::with_capacity(band);
    for grid_index in band_indices.iter().copied() {
        let angle = -2.0
            * std::f64::consts::PI
            * problem.grid_hz[grid_index]
            * options.delay_samples as f64
            / fs;
        delay_phase.push((angle.cos(), angle.sin()));
    }

    let mut coeffs_f64 = vec![0.0f64; m * m * taps];
    let mut columns: Vec<ColumnReport> = Vec::with_capacity(m);
    let mut iterations_total = 0usize;
    let mut work_used = 0u64;
    let mut relative_residual_max = 0.0f64;
    let mut normal_residual_max = 0.0f64;
    let preserve = problem.mode == SolveMode::Preserve;
    if preserve {
        for (input_dense, free) in free_paths.iter().enumerate() {
            columns.push(ColumnReport {
                input_dense: input_dense as u16,
                free_paths: free.len(),
                unknowns: free.len() * taps,
                iterations: 0,
                relative_residual: 0.0,
                normal_residual: 0.0,
                converged: true,
                stop_reason: "preserve_baseline",
            });
        }
    } else {
        for (input_dense, free) in free_paths.iter().enumerate() {
            let unknowns = free.len() * taps;
            if unknowns == 0 {
                columns.push(ColumnReport {
                    input_dense: input_dense as u16,
                    free_paths: 0,
                    unknowns: 0,
                    iterations: 0,
                    relative_residual: 0.0,
                    normal_residual: 0.0,
                    converged: true,
                    stop_reason: "no_free_paths",
                });
                continue;
            }
            let operator = FirOperator {
                m,
                positions,
                band,
                taps,
                free,
                hs: &h_scaled,
                phase: &phase,
                ridge: options.ridge,
            };
            let mut rhs = vec![(0.0, 0.0); complex_rows];
            for position in 0..positions {
                let position_data = &problem.positions[position];
                for band_index in 0..band {
                    let offset = position * band + band_index;
                    let scale = row_scale[offset];
                    let grid_index = band_indices[band_index];
                    let target_point = &position_data.target[input_dense][grid_index];
                    let baseline = h_scaled[(position * m + input_dense) * band + band_index];
                    let rotation = delay_phase[band_index];
                    let delayed = cmul(baseline, rotation);
                    rhs[offset] = (
                        target_point.re * scale - delayed.0,
                        target_point.im * scale - delayed.1,
                    );
                }
            }
            let result = cgls(
                &operator,
                &rhs,
                options.max_iterations,
                options.rel_tolerance,
            );
            work_used = work_used.saturating_add(result.work);
            iterations_total += result.iterations;
            relative_residual_max = relative_residual_max.max(result.relative_residual);
            normal_residual_max = normal_residual_max.max(result.normal_residual);
            for (free_index, output_dense) in free.iter().copied().enumerate() {
                for tap in 0..taps {
                    coeffs_f64[(output_dense * m + input_dense) * taps + tap] =
                        result.x[free_index * taps + tap];
                }
            }
            columns.push(ColumnReport {
                input_dense: input_dense as u16,
                free_paths: free.len(),
                unknowns,
                iterations: result.iterations,
                relative_residual: result.relative_residual,
                normal_residual: result.normal_residual,
                converged: result.converged,
                stop_reason: result.stop_reason,
            });
        }
    }

    let mut responses: Vec<C> = vec![(0.0, 0.0); m * m * grid_points];
    let mut response_work: u64 = 0;
    for output_dense in 0..m {
        for input_dense in 0..m {
            let base = (output_dense * m + input_dense) * taps;
            let cell_base = (output_dense * m + input_dense) * grid_points;
            for (grid_index, freq) in problem.grid_hz.iter().copied().enumerate() {
                let angle = -2.0 * std::f64::consts::PI * freq / fs;
                let step = (angle.cos(), angle.sin());
                let mut acc = (0.0, 0.0);
                let mut power = (1.0, 0.0);
                for tap in 0..taps {
                    let coefficient = coeffs_f64[base + tap];
                    acc.0 += coefficient * power.0;
                    acc.1 += coefficient * power.1;
                    power = cmul(power, step);
                }
                response_work += taps as u64;
                if output_dense == input_dense {
                    let delay_angle = angle * options.delay_samples as f64;
                    acc.0 += delay_angle.cos();
                    acc.1 += delay_angle.sin();
                }
                responses[cell_base + grid_index] = acc;
            }
        }
    }
    work_used = work_used.saturating_add(response_work);

    let mut cells: Vec<ModalMatrixCell> = Vec::with_capacity(m * m);
    for output_dense in 0..m {
        for input_dense in 0..m {
            let cell_base = (output_dense * m + input_dense) * grid_points;
            let mut points = Vec::with_capacity(grid_points);
            for (grid_index, freq) in problem.grid_hz.iter().copied().enumerate() {
                let value = responses[cell_base + grid_index];
                points.push(ComplexPoint {
                    freq_hz: freq,
                    re: value.0,
                    im: value.1,
                });
            }
            cells.push(ModalMatrixCell {
                out_dense: output_dense as u16,
                in_dense: input_dense as u16,
                points,
            });
        }
    }
    let modal = ModalMatrix::new(problem.roles.clone(), problem.grid_hz.clone(), cells).map_err(
        |error| {
            err(
                SolveErrorKind::Target,
                "solve.solution.modal",
                format!("constructed G failed T50 ModalMatrix validation: {error}"),
            )
        },
    )?;

    let mut improvement_max = 0.0f64;
    let mut fit_band_max = 0.0f64;
    let mut near_zero_target_points = 0usize;
    let mut worst: Option<(String, String, f64)> = None;
    for (position_index, position) in problem.positions.iter().enumerate() {
        for input_dense in 0..m {
            for (band_index, grid_index) in band_indices.iter().copied().enumerate() {
                let mut predicted = (0.0, 0.0);
                for output_dense in 0..m {
                    let h = h_band[(position_index * m + output_dense) * band + band_index];
                    let gain =
                        responses[(output_dense * m + input_dense) * grid_points + grid_index];
                    predicted.0 += h.0 * gain.0 - h.1 * gain.1;
                    predicted.1 += h.0 * gain.1 + h.1 * gain.0;
                }
                let target_point = position.target[input_dense][grid_index];
                let target_magnitude = cabs((target_point.re, target_point.im));
                if target_magnitude < NEAR_ZERO_MAGNITUDE {
                    // 零目标上的相对误差无定义：只计数，不制造 100+ dB 假残差。
                    near_zero_target_points += 1;
                    continue;
                }
                let diff = (predicted.0 - target_point.re, predicted.1 - target_point.im);
                let residual_db = ratio_db(cabs(diff), target_magnitude);
                if residual_db > fit_band_max {
                    fit_band_max = residual_db;
                }
                let freq = problem.grid_hz[grid_index];
                if freq >= IMPROVEMENT_BAND_HZ.0 && freq <= IMPROVEMENT_BAND_HZ.1 {
                    if residual_db > improvement_max {
                        improvement_max = residual_db;
                        worst = Some((
                            position.label.clone(),
                            problem.roles[input_dense].clone(),
                            residual_db,
                        ));
                    }
                }
            }
        }
    }
    let improvement_indices: Vec<usize> = problem
        .grid_hz
        .iter()
        .enumerate()
        .filter(|(_, freq)| **freq >= IMPROVEMENT_BAND_HZ.0 && **freq <= IMPROVEMENT_BAND_HZ.1)
        .map(|(index, _)| index)
        .collect();
    let mut offdiag_max_db = 0.0f64;
    let mut diag_deviation_max_db = 0.0f64;
    for output_dense in 0..m {
        for input_dense in 0..m {
            for grid_index in improvement_indices.iter().copied() {
                let value = responses[(output_dense * m + input_dense) * grid_points + grid_index];
                if output_dense != input_dense {
                    let db = magnitude_db_of(cabs(value));
                    if db > offdiag_max_db {
                        offdiag_max_db = db;
                    }
                } else {
                    let deviation = magnitude_db_of(cabs(value)).abs();
                    if deviation > diag_deviation_max_db {
                        diag_deviation_max_db = deviation;
                    }
                }
            }
        }
    }
    let max_coefficient = coeffs_f64
        .iter()
        .fold(0.0f64, |maximum, value| maximum.max(value.abs()));
    let correction_coeff_max_db = if max_coefficient > 0.0 {
        Some(magnitude_db_of(max_coefficient))
    } else {
        None
    };

    let any_non_converged = columns.iter().any(|column| !column.converged);
    let status = if preserve {
        SolveStatus::PreserveBaseline
    } else if identifiability.underdetermined {
        SolveStatus::Underdetermined
    } else if any_non_converged {
        SolveStatus::NonConverged
    } else {
        SolveStatus::Converged
    };

    let mut warnings = Vec::new();
    if identifiability.underdetermined {
        push_warning(
            &mut warnings,
            SolveWarning::UNDERDETERMINED,
            format!(
                "design-point system is underdetermined: position_deficit={}, fir_rank_bound={} vs unknowns_max_per_input={}; minimum-norm G is non-unique and only fits design positions (P={}, M={})",
                identifiability.position_deficit,
                identifiability.fir_rank_bound,
                identifiability.unknowns_max_per_input,
                positions,
                m
            ),
        );
    }
    if any_non_converged {
        push_warning(
            &mut warnings,
            SolveWarning::NON_CONVERGED,
            format!(
                "{} input column(s) did not reach the convergence gate; best-effort solution must not be treated as converged",
                columns.iter().filter(|column| !column.converged).count()
            ),
        );
    }
    if columns.iter().any(|column| {
        column.converged && column.normal_residual < 1.0e-3 && column.relative_residual > 1.0e-3
    }) {
        push_warning(
            &mut warnings,
            SolveWarning::NORMAL_RESIDUAL_GAP,
            "normal-equation gradient converged but weighted relative residual is not small: FIR basis is ill-conditioned on this band; treat coefficient-level interpretation with care (reported numbers are kept)".to_string(),
        );
    }
    if options.ridge > 0.0 {
        push_warning(
            &mut warnings,
            SolveWarning::RIDGE_USED,
            format!(
                "ridge lambda={} used: solution is biased toward zero correction and residuals need not reach zero (lambda policy belongs to T52)",
                options.ridge
            ),
        );
    }
    if let Some(condition) = identifiability.condition_max {
        if condition > HIGH_CONDITION_WARN {
            push_warning(
                &mut warnings,
                SolveWarning::HIGH_CONDITION,
                format!(
                    "frequency-domain condition estimate {condition:.3} exceeds engineering guidance {HIGH_CONDITION_WARN}; correction may be noise-sensitive"
                ),
            );
        }
    }
    if preserve {
        push_warning(
            &mut warnings,
            SolveWarning::PRESERVE_IDENTITY,
            "preserve target: G=deltaD·I (D=0) by construction; no modal correction is requested or claimed"
                .to_string(),
        );
    }
    if let Some(provenance) = &problem.provenance {
        if provenance.target_status == "infeasible" || provenance.target_status == "incomplete" {
            push_warning(
                &mut warnings,
                SolveWarning::TARGET_NOT_DELIVERABLE,
                format!(
                    "upstream target status is {:?}; solving is allowed but deliverability is not asserted",
                    provenance.target_status
                ),
            );
        }
    }
    if unknowns_total > DEFAULT_MAX_UNKNOWNS || estimated_work > DEFAULT_MAX_WORK / 2 {
        push_warning(
            &mut warnings,
            SolveWarning::LARGE_RESOURCE,
            format!(
                "large design-side solve: unknowns_total={unknowns_total}, estimated_work={estimated_work} complex MACs (reported, not a failure)"
            ),
        );
    }

    let resource = ResourceReport {
        max_unknowns_limit: options.max_unknowns,
        complex_rows_limit: options.max_rows,
        work_limit: options.max_work,
        estimated_work,
        work_used,
        memory_bound_bytes,
        within_limits: true,
        limited_by: None,
    };
    let report = SolveReport {
        schema: SOLVE_SCHEMA,
        status,
        mode: problem.mode,
        m,
        taps,
        delay_samples: options.delay_samples,
        sample_rate_hz: problem.sample_rate_hz,
        fit_band_hz: options.fit_band_hz,
        band_points: band,
        grid_points,
        design_positions: positions,
        position_weights: problem
            .positions
            .iter()
            .map(|position| position.weight)
            .collect(),
        target_position_variant: problem.target_position_variant,
        unknowns_total,
        unknowns_max_per_input: unknowns_max,
        complex_rows,
        columns,
        iterations_total,
        relative_residual_max,
        normal_residual_max,
        predicted_design_residual_max_db: improvement_max,
        predicted_fit_band_residual_max_db: fit_band_max,
        predicted_residual_worst: worst,
        near_zero_target_points,
        offdiag_max_db,
        diag_deviation_max_db,
        correction_coeff_max_db,
        preserve_expected_identity: preserve,
        identifiability,
        resource,
        provenance: problem.provenance.clone(),
        holdout_used_for_tuning: false,
        applied: false,
        post_apply_measurement: "not_requested",
        engine_compatibility: "not_validated",
        runtime_validation: "not_run",
        claims: SOLVE_CLAIM,
        warnings,
    };

    let coeffs_f32: Vec<f32> = coeffs_f64.iter().map(|value| *value as f32).collect();
    let coeffs_sha256 = coefficient_sha256(&coeffs_f64);
    let hash = solution_hash(problem, options, &report, &coeffs_f64);
    Ok(SolveOutcome {
        report,
        solution: Some(SolveSolution {
            modal,
            coeffs_f64,
            coeffs_f32,
            coeffs_sha256,
            hash,
        }),
    })
}

fn coefficient_sha256(coeffs: &[f64]) -> String {
    let mut text = String::new();
    for value in coeffs {
        text.push_str(&fmt_f64(*value));
        text.push('\n');
    }
    sha256_hex(text.as_bytes())
}

fn solution_hash(
    problem: &SolveProblem,
    options: &SolveOptions,
    report: &SolveReport,
    coeffs: &[f64],
) -> String {
    let mut text = String::new();
    text.push_str(SOLVE_HASH_DOMAIN);
    text.push('\n');
    text.push_str(&format!("problem_hash={}\n", problem.hash()));
    text.push_str(&options.canonical(problem));
    text.push_str(&format!(
        "status={} mode={} unknowns_total={} iterations_total={}\n",
        report.status.as_str(),
        report.mode.as_str(),
        report.unknowns_total,
        report.iterations_total
    ));
    text.push_str(&format!("coeffs_sha256={}\n", coefficient_sha256(coeffs)));
    sha256_hex(text.as_bytes())
}

/// 频率域可辨识性（position-mixing 秩/条件数）+ FIR 秩上界（必要条件）。
fn identifiability_report(
    problem: &SolveProblem,
    band_indices: &[usize],
    unknowns_max_per_input: usize,
) -> IdentifiabilityReport {
    let m = problem.m();
    let positions = problem.positions.len();
    let band = band_indices.len();
    let mut rank_min = m;
    let mut rank_max = 0usize;
    let mut condition_max = 0.0f64;
    let mut any_deficient = false;
    let mut gram: Vec<C> = vec![(0.0, 0.0); m * m];
    for grid_index in band_indices.iter().copied() {
        for value in &mut gram {
            *value = (0.0, 0.0);
        }
        for position in 0..positions {
            for a in 0..m {
                let left = problem.positions[position].h_eff[a][grid_index];
                let left = (left.re, left.im);
                for b in 0..m {
                    let right = problem.positions[position].h_eff[b][grid_index];
                    let right = (right.re, right.im);
                    gram[a * m + b] = cadd(gram[a * m + b], cmul(cconj(left), right));
                }
            }
        }
        let eigenvalues = hermitian_eigenvalues(&gram, m);
        let maximum = eigenvalues
            .iter()
            .copied()
            .fold(0.0f64, |value, candidate| value.max(candidate));
        let threshold = maximum * RANK_ZERO_REL;
        let valid: Vec<f64> = eigenvalues
            .iter()
            .copied()
            .filter(|value| *value > threshold)
            .collect();
        let rank = valid.len();
        rank_min = rank_min.min(rank);
        rank_max = rank_max.max(rank);
        if rank < m {
            any_deficient = true;
        } else if rank > 0 {
            let minimum = valid.iter().copied().fold(f64::INFINITY, f64::min);
            if minimum > 0.0 && maximum > 0.0 {
                let condition = (maximum / minimum).sqrt();
                if condition.is_finite() {
                    condition_max = condition_max.max(condition);
                }
            }
        }
    }
    let fir_rank_bound = 2usize
        .saturating_mul(band.saturating_mul(positions))
        .min(unknowns_max_per_input);
    let position_deficit = m.saturating_sub(rank_min);
    let underdetermined = position_deficit > 0
        || fir_rank_bound < unknowns_max_per_input
        || unknowns_max_per_input == 0;
    let potentially_full_column_rank = !underdetermined && rank_min == m;
    IdentifiabilityReport {
        participants: m,
        design_positions: positions,
        per_frequency_rank_min: rank_min,
        per_frequency_rank_max: rank_max,
        position_deficit,
        condition_max: if any_deficient || rank_min == 0 {
            None
        } else {
            Some(condition_max)
        },
        fir_rank_bound,
        unknowns_max_per_input,
        potentially_full_column_rank,
        underdetermined,
        note: IdentifiabilityReport::NOTE,
    }
}

// ===========================================================================
// 证据 JSON
// ===========================================================================

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

impl SolveOutcome {
    /// 证据 JSON（schema tspi-modal-solve/v1；确定性、无时间戳；predicted-only）。
    pub fn to_evidence_json(&self) -> String {
        let report = &self.report;
        let mut json = String::new();
        json.push('{');
        json.push_str(&format!("\"schema\":\"{}\",", report.schema));
        json.push_str(&format!(
            "\"status\":\"{}\",\"mode\":\"{}\",\"applied\":{},\"post_apply_measurement\":\"{}\",\"engine_compatibility\":\"{}\",\"runtime_validation\":\"{}\",",
            report.status.as_str(),
            report.mode.as_str(),
            report.applied,
            report.post_apply_measurement,
            report.engine_compatibility,
            report.runtime_validation
        ));
        json.push_str(&format!(
            "\"m\":{},\"taps\":{},\"delay_samples\":{},\"sample_rate_hz\":{},\"fit_band_hz\":[{},{}],\"band_points\":{},\"grid_points\":{},\"design_positions\":{},\"target_position_variant\":{},\"holdout_used_for_tuning\":{},",
            report.m,
            report.taps,
            report.delay_samples,
            report.sample_rate_hz,
            fmt_f64(report.fit_band_hz.0),
            fmt_f64(report.fit_band_hz.1),
            report.band_points,
            report.grid_points,
            report.design_positions,
            report.target_position_variant,
            report.holdout_used_for_tuning
        ));
        json.push_str("\"position_weights\":[");
        for (index, weight) in report.position_weights.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            json.push_str(&fmt_f64(*weight));
        }
        json.push_str("],");
        json.push_str(&format!(
            "\"unknowns_total\":{},\"unknowns_max_per_input\":{},\"complex_rows\":{},\"iterations_total\":{},\"relative_residual_max\":{},\"normal_residual_max\":{},",
            report.unknowns_total,
            report.unknowns_max_per_input,
            report.complex_rows,
            report.iterations_total,
            fmt_f64(report.relative_residual_max),
            fmt_f64(report.normal_residual_max)
        ));
        json.push_str(&format!(
            "\"predicted_design_residual_max_db\":{},\"predicted_fit_band_residual_max_db\":{},\"near_zero_target_points\":{},\"offdiag_max_db\":{},\"diag_deviation_max_db\":{},",
            fmt_f64(report.predicted_design_residual_max_db),
            fmt_f64(report.predicted_fit_band_residual_max_db),
            report.near_zero_target_points,
            fmt_f64(report.offdiag_max_db),
            fmt_f64(report.diag_deviation_max_db)
        ));
        match report.correction_coeff_max_db {
            Some(value) => {
                json.push_str(&format!("\"correction_coeff_max_db\":{},", fmt_f64(value)))
            }
            None => json.push_str("\"correction_coeff_max_db\":null,"),
        }
        match &report.predicted_residual_worst {
            Some((position, role, residual)) => json.push_str(&format!(
                "\"predicted_residual_worst\":{{\"position\":\"{}\",\"role\":\"{}\",\"residual_db\":{}}},",
                json_escape(position),
                json_escape(role),
                fmt_f64(*residual)
            )),
            None => json.push_str("\"predicted_residual_worst\":null,"),
        }
        json.push_str(&format!(
            "\"preserve_expected_identity\":{},\"claims\":\"{}\",",
            report.preserve_expected_identity,
            json_escape(report.claims)
        ));
        json.push_str("\"columns\":[");
        for (index, column) in report.columns.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            json.push_str(&format!(
                "{{\"input_dense\":{},\"free_paths\":{},\"unknowns\":{},\"iterations\":{},\"relative_residual\":{},\"normal_residual\":{},\"converged\":{},\"stop_reason\":\"{}\"}}",
                column.input_dense,
                column.free_paths,
                column.unknowns,
                column.iterations,
                fmt_f64(column.relative_residual),
                fmt_f64(column.normal_residual),
                column.converged,
                json_escape(column.stop_reason)
            ));
        }
        json.push_str("],");
        let ident = &report.identifiability;
        json.push_str(&format!(
            "\"identifiability\":{{\"participants\":{},\"design_positions\":{},\"per_frequency_rank_min\":{},\"per_frequency_rank_max\":{},\"position_deficit\":{},\"condition_max\":{},",
            ident.participants,
            ident.design_positions,
            ident.per_frequency_rank_min,
            ident.per_frequency_rank_max,
            ident.position_deficit,
            match ident.condition_max {
                Some(value) => fmt_f64(value),
                None => "null".to_string(),
            }
        ));
        json.push_str(&format!(
            "\"fir_rank_bound\":{},\"unknowns_max_per_input\":{},\"potentially_full_column_rank\":{},\"underdetermined\":{},\"note\":\"{}\"}},",
            ident.fir_rank_bound,
            ident.unknowns_max_per_input,
            ident.potentially_full_column_rank,
            ident.underdetermined,
            json_escape(ident.note)
        ));
        let resource = &report.resource;
        json.push_str(&format!(
            "\"resource\":{{\"max_unknowns_limit\":{},\"complex_rows_limit\":{},\"work_limit\":{},\"estimated_work\":{},\"work_used\":{},\"memory_bound_bytes\":{},\"within_limits\":{},\"limited_by\":{}}},",
            resource.max_unknowns_limit,
            resource.complex_rows_limit,
            resource.work_limit,
            resource.estimated_work,
            resource.work_used,
            resource.memory_bound_bytes,
            resource.within_limits,
            match resource.limited_by {
                Some(value) => format!("\"{}\"", json_escape(value)),
                None => "null".to_string(),
            }
        ));
        match &report.provenance {
            Some(provenance) => json.push_str(&format!(
                "\"provenance\":{{\"baseline_hash\":\"{}\",\"plant_hash\":\"{}\",\"design_plant_hash\":\"{}\",\"target_sha256\":\"{}\",\"common_d0_samples\":{},\"target_status\":\"{}\"}},",
                provenance.baseline_hash,
                provenance.plant_hash,
                provenance.design_plant_hash,
                provenance.target_sha256,
                provenance.common_d0_samples,
                json_escape(provenance.target_status)
            )),
            None => json.push_str("\"provenance\":null,"),
        }
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
                    "\"solution\":{{\"hash\":\"{}\",\"coeffs_sha256\":\"{}\",\"coefficients\":{}}}",
                    solution.hash,
                    solution.coeffs_sha256,
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
pub fn solve_source_text_for_guards() -> &'static str {
    include_str!("solve.rs")
}

const _: () = {
    assert!(MAX_LOW_BAND_PARTICIPANTS <= MAX_PARTICIPANTS);
    assert!(MAX_SOLVE_UNKNOWNS_HARD == MAX_LOW_BAND_PARTICIPANTS * MAX_SOLVE_TAPS);
    assert!(MAX_SOLVE_TAPS as u32 == TAPS_NORMAL_MAX);
};
