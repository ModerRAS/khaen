//! [NONRT] 设计内 vs 未见 holdout 的预测校验 + T20 质量窗（WBS T53）—— 本文件写域。
//!
//! ## 权威契约（只读输入，禁止在本文件改写）
//!
//! - scratch/lead/planner/KHAEN-RT-STACK-WBS.md T53：设计内与未见 holdout 分开，独立 raw/IR
//!   预测校验工具与 T20 质量窗；门 3 dB/15% 预声明，不能替代后续实际复测。
//! - scratch/lead/planner/DSP-HOST-DECISION.md §2.7/§2.8：应用后复测是 P1 必需；holdout 从未用于
//!   调参；有效集 63–250 Hz 逐点/最差/每输入误差改善 ≥3 dB、有效 T20 下降 ≥15% 是**工程起始门**
//!   （预先冻结统计/有效窗，不是物理保证）；基线误差近零 N/A、T20 无窗、实测未达/缺重复均
//!   INCOMPLETE 或 FAIL，不删点改门伪 PASS。
//! - rt-stack/spec/mimo-t6.md（T73）：§3.2 design/holdout 分离与最少点数；§3.3 频率网格/判据带；
//!   §3.6 有效集最低要求；§4.3 误差/改善 I 与 I_use 定义；§4.4 近零基线 N/A；§4.5 T20 与
//!   "有效 T20 窗" V1–V6；§5.1/§5.3 holdout 禁止用途与密封；§8 PASS/FAIL/INCOMPLETE/NOT_RUN；
//!   §4.9 预测-实测一致性（本文件只产设计侧 predicted，不做实测）。
//! - crates/modal-design/src/target.rs（T50）：TargetMatrix（仅 design positions 构建目标；
//!   validation labels reserved）、EffectivePlant（H_eff = H_raw·P；含 design/validation 标签）、
//!   ModalMatrix（完整 G=deltaD·I+g）、validate_solution（design 结构/预测残差）。
//! - crates/modal-design/src/solve.rs（T51）/regularize.rs（T52）：SolveSolution.modal /
//!   RegularizedSolution.modal 是完整 G；本文件只校验其 holdout 预测，不改其语义。
//!
//! ## 本文件做什么（T53 DoD）
//!
//! 1. **设计内与未见 holdout 分开**：输入为 (a) 用于构建目标的 design plant、(b) 只含未参与设计/
//!    调参的 validation 测点的独立 holdout plant（允许同时携带 design 测点以便通过 T50 RawPlant
//!    构造，但本文件只对 validation 测点做预测评估）；TuningDeclaration 声明 design labels、
//!    调参输入 hash 与 holdout labels/seal；任何 holdout label 出现在调参 labels、或 target 的
//!    `validation_positions_reserved` 与本轮 holdout 清单不一致（事后增删/替换）→ F-07 失败。
//! 2. **独立 plant 预测误差**：对每个 holdout 测点 p、每个 input i，按 T6 §4.3 在 63–250 Hz
//!    改善带计算 `E_off(p,i)`（G=I 基线相对冻结目标 D 的误差能量）、`E_on(p,i)`（G 应用后的预测
//!    误差能量 y_pred(p,i,f)=Σ_j H_p,j(f)·G_{j,i}(f) 相对对角目标 D_ii(f) 的 63–250 Hz 能量）、
//!    `I_raw/I_use`（I_use 用重复测量噪声能量 E_noise 保守化）。按 T6 §4.3，**I_raw 与 I_use 都截断到
//!    60 dB 报告上限**（不参与超额加分）；JSON 逐点标注 `i_raw_truncated/i_use_truncated`，数值底
//!    （E_on 或 E_noise = 0）另以 `numeric_floor` 标注。
//! 3. **T20 质量窗**：对每个 holdout 测点 × 必需 1/3 oct 带（63/125 Hz）× off/on 提供的
//!    带限 IR，按 §4.5 做 Schroeder 反向能量积分、[-5,-25] dB 线性回归与 V1–V6 有效窗检查，
//!    报告 `T20`、R²、残差 RMS、噪声底余量、-5/-25/-35 dB 时刻、时长、拟合点与 σ_T20；
//!    门 = median(T20_on) ≤ 0.85·median(T20_off) 且无测点恶化 >5%。缺 IR → T20 NOT_RUN
//!    → 总判 INCOMPLETE（不得 PASS）。
//! 4. **最差点**：valid (p,i) 的 I_use 最小值/中位数、每 input 最小值、每测点最小值与
//!    对应 E_off/E_on/E_noise 全部报告；单点不达标即 F-01，不允许汇总值遮盖。
//! 5. **σ 定义（本文件冻结）**：
//!    - `σ²_c(p,j,f) = mean_{a<b} 0.5·|X^(a)_{p,j}(f) − X^(b)_{p,j}(f)|²`（重复测量对；
//!      与 T6 §4.3 `E_noise = 0.5·Σ|X^a−X^b|²` 同一口径，估计单次测量的复噪声总方差）；
//!    - `E_noise(p,i) = Σ_j Σ_f σ²_c(p,j,f)·|G_{j,i}(f)|²`（噪声经 G 传播到 input i 的
//!      预测响应误差能量；E_off 侧对角噪声能量 `E_noise_off(p,i) = Σ_f σ²_c(p,i,f)`）；
//!    - `σ_I(p,i) = 10/ln(10) · sqrt( (E_noise_off/E_off)² + (E_noise/max(E_on,E_noise))² )`（I_use 的
//!      一阶 delta-method 不确定度；设计侧代理，不是实测置信区间）；
//!    - T20 回归 `σ_b = sqrt(SS_res/(n-2)/Σ(t-t̄)²)`，`σ_T20 = (T20/|b|)·σ_b`。
//! 6. **not_run/INCOMPLETE 如实**：缺重复噪声、缺 IR、holdout 点数不足、覆盖不足、target 只在
//!    design 上冻结（preserve/per-position target）、seal 不可证 → 对应 gate/总判 INCOMPLETE/
//!    NOT_RUN；IR 已提供但某 band 全部窗无效 → 该 band gate=INCOMPLETE(I-06)（与总判一致，
//!    不用 NOT_RUN 掩盖证据不足）；只有预测改善门与 T20 门都 Pass 且隔离通过才给
//!    PREDICTED_PASS，并恒带
//!    `applied=false / measured=false / can_substitute_real_remeasurement=false /
//!    requires_post_apply_remeasurement=true`。
//! 7. **不调参**：本文件不修改 target/G/lambda/whitelist/Δ/D/positions/analysis 参数；唯一输出
//!    是检验报告。DECLARED_GATES 是编译期固定值，无 options 可放宽（WBS §5：门只能按
//!    mimo-t6 §11.5 升版改，不得测后放宽）。holdout 数据从不进入任何拟合/选择路径。
//!
//! ## 本文件不做什么（明确非目标）
//!
//! - 不测量/不采音/不打开 ALSA/UMIK/MOTU/不写文件/不启线程（纯计算；静态守卫测试保证）；
//! - 不做实际应用后复测、不产 checker 实测报告、不签 T6 PASS（T67/T68 的实测门由独立 checker
//!   从真实 raw 重算；本文件只能给 **PREDICTED** 结论）；
//! - 不做 solver/正则/PEQ/delay 拟合（T51/T52/T64/T65）、不产 WAV/manifest（T54）、不编译/热切
//!   （T66）、不启 daemon job（T60）；
//! - 不把 design 点结果当 holdout PASS 依据；不删点、不改门、不做"最佳对齐"/事后归一/平移；
//! - 不改旧 calibration-spec/T73 spec 字段语义，不复制 legacy_s1 的上混/高度带限错误。
//!
//! ## σ 定义（冻结；测试覆盖公式与单调性）
//!
//! 对 holdout 测点 p、输出角色 j、评价频点 f，重复测量序列 X^(a)（a=0..A-1，A≥2）：
//!
//! ~~~text
//! σ²_c(p,j,f) = mean_{a<b} 0.5·|X^(a) − X^(b)|²          [线性功率单位]
//! σ_abs(p,j,f) = sqrt(σ²_c(p,j,f))                        [线性幅度单位]
//! E_noise(p,i) = Σ_{j∈J} Σ_{f∈F_eff} σ²_c(p,j,f)·|G_{j,i}(f)|²
//! E_noise_off(p,i) = Σ_{f∈F_eff} σ²_c(p,i,f)              [G=I 时只有对角输出]
//! σ_I(p,i) = (10/ln 10)·sqrt( (E_noise_off/E_off)² + (E_noise/max(E_on,E_noise))² )   [dB]
//! ~~~
//!
//! 若 A=1 或缺某角色重复 → 该 (p,i) 记 NoNoiseData（不伪造 0 噪声）；若全部重复序列完全相同
//! （σ²_c=0）→ 允许但报 warning zero_noise_estimate（I_use 退化为 I_raw，不超额加分）。
//!
//! ## T20 有效窗（冻结；mimo-t6 §4.5）
//!
//! 输入 h(t) 必须是**已按 1/3 oct 带限**（对应 band_hz）的 IR（raw→band 过滤属独立分析脚本，
//! checker 另以冻结脚本复算；本文件 band_filter_ir 提供同一公开滤波实现供 raw 预处理与测试，
//! 但窗口有效性只认证**送入的** IR）。Schroeder EDC：
//!
//! ~~~text
//! L(t) = 10·log10( ∫_t^∞ h²(τ)dτ / ∫_0^∞ h²(τ)dτ )   [dB]
//! T20  = 3·(t(-25) − t(-5)) = −60/b，L(t)=a+b·t 在 [-25,-5] dB 样本上最小二乘
//! ~~~
//!
//! 有效窗 V1–V6（任一不满足 → 该估计 N/A，不得当 0/失败混入统计）：
//!
//! - V1 噪声底余量：`L_noise = 10log10(P_tail·N/E_total)`（尾部 5% RMS 功率 × 时长 / 总能量），
//!   要求 `L(-25) − L_noise = −25 − L_noise ≥ 10 dB`；
//! - V2 采集时长：`N/fs ≥ 1.2·t(-25)` 且尾部达到噪声底 `L(last) ≤ L_noise+5 dB`；若
//!   `t(-35)` 可测（`L(t)≤L_noise+5` 前已到 −35 dB）再加 `N/fs ≥ 1.2·t(-35)`；
//! - V3 拟合：R² ≥ 0.99 且回归残差 RMS ≤ 1.0 dB；
//! - V4 物理：斜率 b<0 且 T20 ∈ [0.05, 2.0] s；
//! - V5 带内 SNR：声明 `snr_db ≥ 25` 且无削波/非有限样本；
//! - V6 测量有效性：无 xrun/deadline miss/失锁/丢块。
//!
//! ## 判定（mimo-t6 §8）
//!
//! 总判优先级 **FAIL > INCOMPLETE > PREDICTED_PASS/NOT_RUN**：
//! - FAIL：隔离失败 F-07；任一 valid (p,i) 的 I_use <3.0 dB（F-01，含最差点/每 input 最低点）；
//!   任一必需 T20 band 门失败（F-02）。
//! - INCOMPLETE：holdout 点数 <3（I-03）、有效集/N/A/覆盖不足（I-06/I-07）、缺重复噪声、缺 IR、
//!   缺 target holdout 冻结（I-09）、seal 不可证（I-04）。
//! - NOT_RUN：既无重复噪声也无 IR（未执行 T53 校验）。
//! - PREDICTED_PASS：隔离 Pass、预测改善门 Pass、T20 门 Pass；**仍不是实测 PASS**。
//!
//! ## 关键不变量（测试与静态守卫覆盖）
//!
//! - 只评估 holdout_plant 的 PositionRole::Validation 测点；design 点只用于绑定/结构校验；
//! - holdout label 出现在 tuning labels → F-07；target reserved labels 与 holdout 清单不一致 → F-07；
//! - 门值来自 DECLARED_GATES，无任何放宽入口；不按结果挑点/删点/改统计；
//! - 无 I/O/线程/环境/序列化/unsafe；hash 对输入逐位确定（无时间戳/随机数）；
//! - 证据 JSON（HoldoutReport::to_evidence_json）可被独立 checker 解析并复算。

use std::fmt::Write as _;

use crate::ingest::sha256_hex;
use crate::target::{
    validate_solution, EffectivePlant, EffectivePosition, ModalMatrix, ModalParticipants,
    TargetCellView, TargetMatrix, IMPROVEMENT_BAND_HZ, T20_OPTIONAL_BAND_HZ, T20_REQUIRED_BANDS_HZ,
};
use crate::timebase::ComplexPoint;

// ===========================================================================
// schema / hash domain
// ===========================================================================

/// 校验输入声明 schema（本文件输入契约；不是实测报告）。
pub const VALIDATE_SCHEMA: &str = "khaen-modal-holdout/v1";
/// 校验报告 schema。
pub const VALIDATE_REPORT_SCHEMA: &str = "khaen-modal-holdout-report/v1";
/// 报告 hash domain（与 target/solve/regularize 域分离）。
pub const VALIDATE_HASH_DOMAIN: &str = "khaen-modal-holdout/v1";
/// holdout label 清单 hash domain。
pub const HOLDOUT_LABELS_HASH_DOMAIN: &str = "khaen-modal-holdout-labels/v1";

/// 本文件固定 claim（诚实边界；测试逐字核验）。
pub const VALIDATE_CLAIM: &str = "design-side predicted validation only; holdout data never used for tuning; not applied, not measured, not a substitute for post-apply real remeasurement; 3 dB / 15% are pre-declared engineering starting gates, not physical guarantees; missing noise/IR/holdout evidence yields INCOMPLETE or NOT_RUN";

// ===========================================================================
// 预声明工程门（固定；无 options 可放宽）
// ===========================================================================

/// T6 §4.3 改善门：所有 valid holdout (p,i) 的 I_use ≥ 3.0 dB（含最差/每 input 最低点）。
pub const IMPROVEMENT_GATE_DB: f64 = 3.0;
/// T6 §4.5 T20 门：median(T20_on) ≤ 0.85 × median(T20_off)（≥15% 下降）。
pub const T20_REDUCTION_GATE_RATIO: f64 = 0.85;
/// T6 §4.5 T20 门：任一测点 on 相对 off 恶化 ≤5%（ratio ≤ 1.05）。
pub const T20_POSITION_WORSEN_RATIO: f64 = 1.05;
/// T6 §4.4 近零基线：E_off ≤ 4·E_noise → I = N/A。
pub const NEAR_ZERO_E_OFF_OVER_NOISE: f64 = 4.0;
/// 改善 dB 报告上限（数值底；不参与超额加分）。
pub const NUMERIC_FLOOR_I_DB: f64 = 60.0;

/// T6 §3.2 正式 holdout 最少测点数。
pub const MIN_HOLDOUT_POSITIONS: usize = 3;
/// T6 §3.6 有效 (p,i) 最低比例（4.1.2：3×5=15 → ≥12）。
pub const MIN_VALID_POINT_FRACTION: f64 = 0.80;
/// T6 §3.6 至少 2 个 holdout 测点各 ≥4 个有效 input。
pub const MIN_POSITIONS_WITH_FOUR_VALID_INPUTS: usize = 2;
/// T6 §3.6 每个 input 至少 2 个有效测点。
pub const MIN_POSITIONS_PER_INPUT: usize = 2;
/// T6 §4.4 N/A 比例上限（超过 → I-07）。
pub const MAX_NA_FRACTION: f64 = 0.20;
/// T6 §3.3 改善带网格覆盖率门。
pub const IMPROVEMENT_GRID_MIN_COVERAGE: f64 = 0.80;
/// T6 §3.3 分析网格分辨率（1/24 oct）。
pub const CRITERION_GRID_DIVISIONS_PER_OCTAVE: f64 = 24.0;

/// T6 §4.5 V1 噪声底余量（dB）。
pub const T20_NOISE_MARGIN_DB: f64 = 10.0;
/// T6 §4.5 V2 采集时长因子。
pub const T20_DURATION_FACTOR: f64 = 1.2;
/// T6 §4.5 V2 尾部噪声底容差（dB）。
pub const T20_TAIL_MARGIN_DB: f64 = 5.0;
/// T6 §4.5 V3 拟合 R² 下限。
pub const T20_MIN_R2: f64 = 0.99;
/// T6 §4.5 V3 回归残差 RMS 上限（dB）。
pub const T20_MAX_RESIDUAL_RMS_DB: f64 = 1.0;
/// T6 §4.5 V4 T20 物理下限（s）。
pub const T20_MIN_S: f64 = 0.05;
/// T6 §4.5 V4 T20 物理上限（s）。
pub const T20_MAX_S: f64 = 2.0;
/// T6 §4.5 V5 带内 SNR 下限（dB）。
pub const T20_MIN_SNR_DB: f64 = 25.0;
/// T6 §4.5 V1 噪声底尾部窗口（IR 最后 5%）。
pub const T20_NOISE_TAIL_FRACTION: f64 = 0.05;
/// T6 §4.5 拟合区间下限（dB）。
pub const T20_FIT_LO_DB: f64 = -5.0;
/// T6 §4.5 拟合区间上限（dB）。
pub const T20_FIT_HI_DB: f64 = -25.0;
/// T6 §4.5 每 band 至少 3 个有效测点。
pub const T20_MIN_VALID_POSITIONS: usize = 3;
/// T6 §4.5 每 band ≥2/3 holdout 测点有有效估计。
pub const T20_MIN_VALID_POSITION_FRACTION: f64 = 2.0 / 3.0;
/// T20 最小拟合样本数（防止退化回归）。
pub const T20_MIN_FIT_POINTS: usize = 16;

/// 1/3 oct 带滤波器级数（两级相同 RBJ 2nd-order bandpass → 4th-order）。
pub const BAND_FILTER_STAGES: usize = 2;

/// 改善带判据网格预期 1/24 oct 点数（向下取整 + 1；63–250 Hz → 48）。
pub fn expected_1_24_oct_points(lo_hz: f64, hi_hz: f64) -> usize {
    let octaves = (hi_hz / lo_hz).log2();
    (octaves * CRITERION_GRID_DIVISIONS_PER_OCTAVE).floor() as usize + 1
}

/// 改善带 1/24 oct 网格最小点数（= ceil(0.8·expected)；63–250 Hz → 39）。
pub fn min_improvement_band_points() -> usize {
    (IMPROVEMENT_GRID_MIN_COVERAGE
        * expected_1_24_oct_points(IMPROVEMENT_BAND_HZ.0, IMPROVEMENT_BAND_HZ.1) as f64)
        .ceil() as usize
}

/// 预声明工程门集合（冻结；报告逐条回显，测试断言不可放宽）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeclaredGates {
    /// 改善门（dB）。
    pub improvement_gate_db: f64,
    /// T20 中位数下降比。
    pub t20_reduction_gate_ratio: f64,
    /// T20 单测点恶化比上限。
    pub t20_position_worsen_ratio: f64,
    /// 近零基线 N/A 比。
    pub near_zero_e_off_over_noise: f64,
    /// 改善报告上限（dB）。
    pub numeric_floor_i_db: f64,
    /// holdout 最少测点。
    pub min_holdout_positions: usize,
    /// 有效点最低比例。
    pub min_valid_point_fraction: f64,
    /// 至少该数量测点各 ≥4 有效 input。
    pub min_positions_with_four_valid_inputs: usize,
    /// 每 input 至少该数量有效测点。
    pub min_positions_per_input: usize,
    /// N/A 比例上限。
    pub max_na_fraction: f64,
    /// 改善带（Hz）。
    pub improvement_band_hz: (f64, f64),
    /// 改善带网格覆盖率门。
    pub improvement_grid_min_coverage: f64,
    /// 判据网格分辨率（1/24 oct）。
    pub criterion_grid_divisions_per_octave: f64,
    /// T20 噪声底余量（dB）。
    pub t20_noise_margin_db: f64,
    /// T20 采集时长因子。
    pub t20_duration_factor: f64,
    /// T20 拟合区间（dB）。
    pub t20_fit_window_db: (f64, f64),
    /// T20 R² 下限。
    pub t20_min_r2: f64,
    /// T20 残差 RMS 上限（dB）。
    pub t20_max_residual_rms_db: f64,
    /// T20 物理范围（s）。
    pub t20_range_s: (f64, f64),
    /// T20 SNR 下限（dB）。
    pub t20_min_snr_db: f64,
    /// T20 每 band 最少有效测点。
    pub t20_min_valid_positions: usize,
    /// T20 每 band 有效测点比例。
    pub t20_min_valid_position_fraction: f64,
    /// 权威来源（原文引用）。
    pub authority: &'static str,
}

/// 冻结的预声明门（T53 唯一门值来源；无放宽入口）。
pub const DECLARED_GATES: DeclaredGates = DeclaredGates {
    improvement_gate_db: IMPROVEMENT_GATE_DB,
    t20_reduction_gate_ratio: T20_REDUCTION_GATE_RATIO,
    t20_position_worsen_ratio: T20_POSITION_WORSEN_RATIO,
    near_zero_e_off_over_noise: NEAR_ZERO_E_OFF_OVER_NOISE,
    numeric_floor_i_db: NUMERIC_FLOOR_I_DB,
    min_holdout_positions: MIN_HOLDOUT_POSITIONS,
    min_valid_point_fraction: MIN_VALID_POINT_FRACTION,
    min_positions_with_four_valid_inputs: MIN_POSITIONS_WITH_FOUR_VALID_INPUTS,
    min_positions_per_input: MIN_POSITIONS_PER_INPUT,
    max_na_fraction: MAX_NA_FRACTION,
    improvement_band_hz: IMPROVEMENT_BAND_HZ,
    improvement_grid_min_coverage: IMPROVEMENT_GRID_MIN_COVERAGE,
    criterion_grid_divisions_per_octave: CRITERION_GRID_DIVISIONS_PER_OCTAVE,
    t20_noise_margin_db: T20_NOISE_MARGIN_DB,
    t20_duration_factor: T20_DURATION_FACTOR,
    t20_fit_window_db: (T20_FIT_LO_DB, T20_FIT_HI_DB),
    t20_min_r2: T20_MIN_R2,
    t20_max_residual_rms_db: T20_MAX_RESIDUAL_RMS_DB,
    t20_range_s: (T20_MIN_S, T20_MAX_S),
    t20_min_snr_db: T20_MIN_SNR_DB,
    t20_min_valid_positions: T20_MIN_VALID_POSITIONS,
    t20_min_valid_position_fraction: T20_MIN_VALID_POSITION_FRACTION,
    authority: "WBS T53 + DSP-HOST-DECISION §2.7/§2.8 + mimo-t6 §3.2/§3.3/§3.6/§4.3/§4.4/§4.5/§8",
};

// ===========================================================================
// 错误
// ===========================================================================

/// T53 校验错误类别（输入/契约问题；不是判定结果）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidateErrorKind {
    /// 冻结基线 hash 不一致。
    Baseline,
    /// participants 绑定不一致。
    Participants,
    /// 采样率/布局不一致。
    SampleRate,
    /// 频率网格不一致或非法。
    Grid,
    /// 矩阵/维度/角色结构错误。
    Matrix,
    /// 缺少必要输入。
    Missing,
    /// holdout 声明/调参声明/密封字段非法。
    Holdout,
    /// 重复噪声输入非法。
    Repeats,
    /// T20 IR 输入非法。
    T20,
}

impl ValidateErrorKind {
    /// 稳定错误码。
    pub const fn code(self) -> &'static str {
        match self {
            ValidateErrorKind::Baseline => "E_VAL_BASELINE",
            ValidateErrorKind::Participants => "E_VAL_PARTICIPANTS",
            ValidateErrorKind::SampleRate => "E_VAL_SAMPLE_RATE",
            ValidateErrorKind::Grid => "E_VAL_GRID",
            ValidateErrorKind::Matrix => "E_VAL_MATRIX",
            ValidateErrorKind::Missing => "E_VAL_MISSING",
            ValidateErrorKind::Holdout => "E_VAL_HOLDOUT",
            ValidateErrorKind::Repeats => "E_VAL_REPEATS",
            ValidateErrorKind::T20 => "E_VAL_T20",
        }
    }
}

/// T53 输入/契约错误（结构化；不 panic）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidateError {
    kind: ValidateErrorKind,
    field: String,
    detail: String,
}

impl ValidateError {
    /// 构造。
    pub fn new(
        kind: ValidateErrorKind,
        field: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            field: field.into(),
            detail: detail.into(),
        }
    }

    /// 类别。
    pub fn kind(&self) -> ValidateErrorKind {
        self.kind
    }

    /// 稳定错误码。
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

impl std::fmt::Display for ValidateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}): {}", self.code(), self.field, self.detail)
    }
}

impl std::error::Error for ValidateError {}

fn err(
    kind: ValidateErrorKind,
    field: impl Into<String>,
    detail: impl Into<String>,
) -> ValidateError {
    ValidateError::new(kind, field, detail)
}

// ===========================================================================
// 输入：调参声明 / 重复噪声 / T20 IR
// ===========================================================================

/// holdout label 清单的规范 hash（排序后 join；只含 label 文本，无时间戳）。
pub fn holdout_labels_sha256(labels: &[String]) -> String {
    let mut sorted: Vec<&str> = labels.iter().map(|label| label.as_str()).collect();
    sorted.sort_unstable();
    sha256_hex(format!("{HOLDOUT_LABELS_HASH_DOMAIN}\n{}", sorted.join("\n")).as_bytes())
}

/// 调参/密封声明（T6 §5.1/§5.3：holdout 从未参与调参；点清单先于设计选定并 hash 冻结）。
#[derive(Debug, Clone, PartialEq)]
pub struct TuningDeclaration {
    /// 参与目标/拟合/正则/选择/分析参数确定的 design labels。
    pub design_labels: Vec<String>,
    /// 实际参与过任何调参环节的 label 清单（含 design 点；出现 holdout label ⇒ F-07）。
    pub tuning_labels: Vec<String>,
    /// 调参输入 hash（只允许 design raw/config；不得含 holdout raw hash）。
    pub tuning_inputs_sha256: String,
    /// 本轮 holdout labels（role=validation；不得事后替换/增删）。
    pub holdout_labels: Vec<String>,
    /// holdout labels 的规范 hash（holdout_labels_sha256）。
    pub holdout_labels_sha256: String,
    /// holdout 清单是否在设计/拟合前选定并密封（不可证 → INCOMPLETE I-04）。
    pub holdout_sealed_before_design: bool,
}

impl TuningDeclaration {
    /// 构造：从 design/holdout labels 计算规范 hash；默认 tuning_labels = design labels，
    /// tuning_inputs_sha256 留空（由 with_tuning 填入；空值会被 validate_holdout 拒收）。
    pub fn new(design_labels: Vec<String>, holdout_labels: Vec<String>) -> Self {
        let tuning_labels = design_labels.clone();
        let hash = holdout_labels_sha256(&holdout_labels);
        Self {
            design_labels,
            tuning_labels,
            tuning_inputs_sha256: String::new(),
            holdout_labels,
            holdout_labels_sha256: hash,
            holdout_sealed_before_design: true,
        }
    }

    /// 设置调参 labels 与调参输入 hash。
    pub fn with_tuning(
        mut self,
        tuning_labels: Vec<String>,
        tuning_inputs_sha256: impl Into<String>,
    ) -> Self {
        self.tuning_labels = tuning_labels;
        self.tuning_inputs_sha256 = tuning_inputs_sha256.into();
        self
    }

    /// 设置 seal 声明。
    pub fn with_seal(mut self, holdout_sealed_before_design: bool) -> Self {
        self.holdout_sealed_before_design = holdout_sealed_before_design;
        self
    }
}

/// 一个输出角色的重复测量序列（A≥2；每序列与 target 网格逐点对齐的复响应）。
#[derive(Debug, Clone, PartialEq)]
pub struct RoleRepeats {
    /// 输出角色 id（必须属于 participants）。
    pub role_id: String,
    /// 重复序列（X^(0), X^(1), ...；每条长度 = grid 点数）。
    pub series: Vec<Vec<ComplexPoint>>,
}

/// 一个 holdout 测点的重复测量（用于 σ/E_noise；只允许 holdout labels）。
#[derive(Debug, Clone, PartialEq)]
pub struct NoiseRepeats {
    /// holdout 测点 label。
    pub position_label: String,
    /// 每个输出角色一条重复序列。
    pub roles: Vec<RoleRepeats>,
}

/// T20 IR 状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrState {
    /// baseline（G=I）。
    Off,
    /// MIMO-on（G 应用）。
    On,
}

impl IrState {
    /// 稳定字面值。
    pub const fn as_str(self) -> &'static str {
        match self {
            IrState::Off => "off",
            IrState::On => "on",
        }
    }
}

/// T20 IR 来源（决定报告口径；任何来源都不构成"实测 PASS"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrOrigin {
    /// 设计侧预测模型 IR（predicted-only）。
    PredictedModel,
    /// 外部提供的带限 IR（如独立 checker 的测量分析产物；T53 仍只认证窗口，不签实测）。
    ExternalBandLimited,
}

impl IrOrigin {
    /// 稳定字面值。
    pub const fn as_str(self) -> &'static str {
        match self {
            IrOrigin::PredictedModel => "predicted_model",
            IrOrigin::ExternalBandLimited => "external_band_limited",
        }
    }
}

/// 单条 T20 IR 输入（**已按 band_hz 做 1/3 oct 带限**；见模块文档）。
#[derive(Debug, Clone, PartialEq)]
pub struct BandIr {
    /// holdout 测点 label。
    pub position_label: String,
    /// 1/3 oct 带中心（必需 63/125；可选报告 40）。
    pub band_hz: f64,
    /// off/on。
    pub state: IrState,
    /// 带限 IR 样本（fs 与 target 一致；t=0 为 onset）。
    pub samples: Vec<f64>,
    /// 声明的带内 SNR（dB；V5 ≥25）。
    pub snr_db: f64,
    /// ADC/数字削波标志（V5 需 false）。
    pub clipped: bool,
    /// xrun 标志（V6 需 false）。
    pub xrun: bool,
    /// deadline miss 标志（V6 需 false）。
    pub deadline_miss: bool,
    /// 失锁标志（V6 需 false）。
    pub lost_lock: bool,
    /// 丢块数（V6 需 0）。
    pub dropped_blocks: u64,
}

/// holdout 校验请求（声明 + 重复噪声 + T20 IR）。
#[derive(Debug, Clone, PartialEq)]
pub struct HoldoutRequest {
    /// 调参/密封声明。
    pub tuning: TuningDeclaration,
    /// 重复测量噪声（E_noise/σ；缺 → I 门 NOT_RUN）。
    pub repeats: Vec<NoiseRepeats>,
    /// T20 带限 IR（缺 → T20 门 NOT_RUN）。
    pub ir: Vec<BandIr>,
    /// IR 来源口径。
    pub ir_origin: IrOrigin,
}

impl HoldoutRequest {
    /// 构造（无噪声/无 IR；只声明调参信息）。
    pub fn new(tuning: TuningDeclaration) -> Self {
        Self {
            tuning,
            repeats: Vec::new(),
            ir: Vec::new(),
            ir_origin: IrOrigin::PredictedModel,
        }
    }

    /// 追加重复测量。
    pub fn with_repeats(mut self, repeats: Vec<NoiseRepeats>) -> Self {
        self.repeats = repeats;
        self
    }

    /// 追加 T20 IR。
    pub fn with_ir(mut self, ir: Vec<BandIr>, origin: IrOrigin) -> Self {
        self.ir = ir;
        self.ir_origin = origin;
        self
    }
}

// ===========================================================================
// 输出：状态/点/报告
// ===========================================================================

/// 单个 gate 的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateStatus {
    /// 门通过。
    Pass,
    /// 门明确失败（FAIL，优先级最高）。
    Fail,
    /// 未提供/未执行（NOT_RUN）。
    NotRun,
    /// 证据缺失/无效/不足，不能判（INCOMPLETE）。
    Incomplete,
}

impl GateStatus {
    /// 稳定字面值。
    pub const fn as_str(self) -> &'static str {
        match self {
            GateStatus::Pass => "PASS",
            GateStatus::Fail => "FAIL",
            GateStatus::NotRun => "NOT_RUN",
            GateStatus::Incomplete => "INCOMPLETE",
        }
    }
}

/// 单个 (p,i) 预测点状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointStatus {
    /// 有效（进入改善门）。
    Valid,
    /// 近零基线 N/A（E_off ≤ 4·E_noise；不计 PASS 也不计 FAIL）。
    NearZero,
    /// 缺重复噪声（不能算 I_use；不伪造 0 噪声）。
    NoNoiseData,
    /// 改善带网格覆盖不足。
    InsufficientCoverage,
    /// 非有限数值。
    NonFinite,
    /// target 未在 holdout 侧冻结（per-position/preserve target 缺该测点 cell）。
    MissingTarget,
}

impl PointStatus {
    /// 稳定字面值。
    pub const fn as_str(self) -> &'static str {
        match self {
            PointStatus::Valid => "valid",
            PointStatus::NearZero => "near_zero_na",
            PointStatus::NoNoiseData => "no_noise_data",
            PointStatus::InsufficientCoverage => "insufficient_coverage",
            PointStatus::NonFinite => "non_finite",
            PointStatus::MissingTarget => "missing_target",
        }
    }
}

/// 一个 holdout (p,i) 的预测误差/改善/σ（数值字段仅在可计算时 Some）。
#[derive(Debug, Clone, PartialEq)]
pub struct PointPrediction {
    /// holdout 测点 label。
    pub position_label: String,
    /// input 角色 id。
    pub input_role: String,
    /// 状态。
    pub status: PointStatus,
    /// 状态原因（稳定字面值）。
    pub status_reason: Option<&'static str>,
    /// G=I 基线相对目标误差能量。
    pub e_off: Option<f64>,
    /// G 应用后预测误差能量。
    pub e_on: Option<f64>,
    /// 重复噪声经 G 传播的误差能量。
    pub e_noise: Option<f64>,
    /// G=I 侧对角噪声能量（σ_I 用）。
    pub e_noise_off: Option<f64>,
    /// I_raw = 10log10(E_off/E_on)（dB）。
    pub i_raw_db: Option<f64>,
    /// I_use = 10log10(E_off/max(E_on,E_noise))（dB；门只用它）。
    pub i_use_db: Option<f64>,
    /// 数值底（E_on/E_noise 为 0）。
    pub numeric_floor: bool,
    /// I_raw 是否被 60 dB 报告上限截断（T6 §4.3）。
    pub i_raw_truncated: bool,
    /// I_use 是否被 60 dB 报告上限截断（T6 §4.3）。
    pub i_use_truncated: bool,
    /// 代表复噪声 RMS 幅度（sqrt(mean σ²_c)）。
    pub sigma_abs_rms: Option<f64>,
    /// I 的一阶不确定度（dB；delta-method 代理）。
    pub sigma_i_db: Option<f64>,
    /// 改善带网格覆盖点数。
    pub band_points: usize,
    /// 改善带覆盖比例（实际/预期 1/24 oct 点数）。
    pub band_coverage: f64,
}

/// 最差 (p,i)（valid 点中 I_use 最小）。
#[derive(Debug, Clone, PartialEq)]
pub struct WorstIUse {
    /// 测点 label。
    pub position_label: String,
    /// input 角色 id。
    pub input_role: String,
    /// I_use（dB）。
    pub i_use_db: f64,
    /// I_raw（dB）。
    pub i_raw_db: Option<f64>,
    /// E_off。
    pub e_off: f64,
    /// E_on。
    pub e_on: f64,
    /// E_noise。
    pub e_noise: f64,
}

/// σ 定义记录（报告逐字回显；测试核验公式字符串）。
#[derive(Debug, Clone, PartialEq)]
pub struct SigmaModel {
    /// 复噪声方差定义。
    pub definition: &'static str,
    /// 数据来源。
    pub source: &'static str,
    /// 使用的重复对总数（跨 position/role/freq）。
    pub repeat_pair_count: usize,
    /// E_noise 公式。
    pub formula_e_noise: &'static str,
    /// σ_I 公式。
    pub formula_sigma_i: &'static str,
    /// 备注（设计侧代理，不是实测置信区间）。
    pub note: &'static str,
}

/// holdout 预测校验报告段。
#[derive(Debug, Clone, PartialEq)]
pub struct HoldoutPrediction {
    /// gate 状态。
    pub gate: GateStatus,
    /// gate 码（稳定：PASS/F-01/I-03/I-06/I-07/I-09/not_run）。
    pub gate_code: &'static str,
    /// gate 原因。
    pub reasons: Vec<&'static str>,
    /// 全部候选 (p,i)（按 holdout label 顺序 × participants 顺序）。
    pub points: Vec<PointPrediction>,
    /// 候选点数。
    pub candidate_points: usize,
    /// valid 点数。
    pub valid_points: usize,
    /// N/A 点数。
    pub near_zero_points: usize,
    /// 无效点数。
    pub invalid_points: usize,
    /// N/A 比例。
    pub near_zero_fraction: f64,
    /// 有效点比例。
    pub valid_fraction: f64,
    /// valid I_use 中位数（dB）。
    pub median_i_use_db: Option<f64>,
    /// 最差点（valid 中 I_use 最小）。
    pub worst_i_use: Option<WorstIUse>,
    /// 每 input 的 valid I_use 最小值。
    pub per_input_min_db: Vec<(String, Option<f64>)>,
    /// 每测点的 valid I_use 最小值。
    pub per_position_min_db: Vec<(String, Option<f64>)>,
    /// 是否存在数值底。
    pub numeric_floor: bool,
    /// σ 定义。
    pub sigma: SigmaModel,
}

/// T20 有效窗拟合结果（单 IR/band/state）。
#[derive(Debug, Clone, PartialEq)]
pub struct T20WindowFit {
    /// band 中心（Hz）。
    pub band_hz: f64,
    /// T20（s；无效窗口 None）。
    pub t20_s: Option<f64>,
    /// 拟合斜率（dB/s）。
    pub slope_db_per_s: Option<f64>,
    /// 拟合截距（dB）。
    pub intercept_db: Option<f64>,
    /// R²。
    pub r_squared: Option<f64>,
    /// 回归残差 RMS（dB）。
    pub residual_rms_db: Option<f64>,
    /// 斜率标准误（dB/s）。
    pub slope_sigma_db_per_s: Option<f64>,
    /// σ_T20（s）。
    pub t20_sigma_s: Option<f64>,
    /// EDC 噪声底（dB）。
    pub noise_floor_db: Option<f64>,
    /// 噪声底余量（dB；-25 − L_noise）。
    pub noise_margin_db: Option<f64>,
    /// t(-5)（s）。
    pub t_minus_5_s: Option<f64>,
    /// t(-25)（s）。
    pub t_minus_25_s: Option<f64>,
    /// t(-35)（s；不可测 None）。
    pub t_minus_35_s: Option<f64>,
    /// 采集时长（s）。
    pub duration_s: f64,
    /// 拟合样本数。
    pub fit_points: usize,
    /// V1–V4 与样本有效性（true = 窗口有效）。
    pub valid: bool,
    /// 无效原因（稳定字面值）。
    pub reasons: Vec<&'static str>,
}

/// 一个 holdout 测点、一个必需 band 的 off/on T20 对比。
#[derive(Debug, Clone, PartialEq)]
pub struct T20PositionReport {
    /// 测点 label。
    pub position_label: String,
    /// off 拟合。
    pub off: Option<T20WindowFit>,
    /// on 拟合。
    pub on: Option<T20WindowFit>,
    /// off+on 均有效（进入门统计）。
    pub valid: bool,
    /// 无效原因。
    pub reasons: Vec<&'static str>,
    /// on/off T20 比。
    pub ratio: Option<f64>,
    /// T20 下降百分比（正 = 下降）。
    pub reduction_percent: Option<f64>,
}

/// 一个必需 band 的 T20 质量窗/门报告。
#[derive(Debug, Clone, PartialEq)]
pub struct T20BandReport {
    /// band 中心（Hz）。
    pub band_hz: f64,
    /// gate 状态。
    pub gate: GateStatus,
    /// gate 码（PASS/F-02/I-06）。
    pub gate_code: &'static str,
    /// gate 原因。
    pub reasons: Vec<&'static str>,
    /// 每测点对比。
    pub positions: Vec<T20PositionReport>,
    /// 有效测点数。
    pub valid_positions: usize,
    /// holdout 测点数。
    pub total_positions: usize,
    /// off T20 中位数（s）。
    pub median_off_s: Option<f64>,
    /// on T20 中位数（s）。
    pub median_on_s: Option<f64>,
    /// 中位数比。
    pub median_ratio: Option<f64>,
    /// 中位数下降百分比。
    pub reduction_percent: Option<f64>,
    /// 单测点最大恶化比（on/off）。
    pub max_position_ratio: Option<f64>,
}

/// T20 质量窗报告段。
#[derive(Debug, Clone, PartialEq)]
pub struct T20Report {
    /// 总 gate。
    pub gate: GateStatus,
    /// gate 码。
    pub gate_code: &'static str,
    /// 原因。
    pub reasons: Vec<&'static str>,
    /// IR 来源口径。
    pub origin: IrOrigin,
    /// 必需 band 报告（63、125）。
    pub bands: Vec<T20BandReport>,
    /// 可选报告 band（40 Hz；不参与门）。
    pub optional_bands: Vec<T20BandReport>,
}

/// holdout 隔离审计。
#[derive(Debug, Clone, PartialEq)]
pub struct IsolationReport {
    /// 状态（Pass/Fail/Incomplete）。
    pub gate: GateStatus,
    /// 码（PASS/F-07/I-04）。
    pub gate_code: &'static str,
    /// 原因。
    pub reasons: Vec<&'static str>,
    /// design labels（本轮）。
    pub design_labels: Vec<String>,
    /// holdout labels（本轮声明）。
    pub holdout_labels: Vec<String>,
    /// 声明的 holdout hash。
    pub declared_holdout_sha256: String,
    /// 由实际 holdout 清单重算的 hash。
    pub computed_holdout_sha256: String,
    /// hash 一致。
    pub labels_hash_match: bool,
    /// 调参 labels。
    pub tuning_labels: Vec<String>,
    /// 调参输入 hash。
    pub tuning_inputs_sha256: String,
    /// 调参 labels 与 holdout 的交集（必须为空）。
    pub tuning_holdout_overlap: Vec<String>,
    /// target 预留的 validation labels。
    pub target_reserved_labels: Vec<String>,
    /// target 自身 holdout 隔离自检。
    pub target_holdout_isolated: bool,
    /// holdout 清单声明在设计前密封。
    pub sealed_before_design: bool,
}

/// 设计侧预测摘要（T50 validate_solution；只 design positions，不作 holdout 门）。
#[derive(Debug, Clone, PartialEq)]
pub struct DesignPredictionSummary {
    /// 结构校验是否全部通过（含 design 残差上限）。
    pub passed: bool,
    /// 检查项 (name, passed, detail)。
    pub checks: Vec<(String, bool, String)>,
    /// design 改善带最大相对残差（dB）。
    pub predicted_residual_max_db: Option<f64>,
    /// 最坏 (position, role, residual_db)。
    pub predicted_residual_worst: Option<(String, String, f64)>,
    /// 非对角最大幅度（dB）。
    pub offdiag_max_db: f64,
    /// 对角偏离 0 dB 最大绝对值（dB）。
    pub diag_deviation_max_db: f64,
    /// 作用域（design positions only）。
    pub scope: &'static str,
}

/// 报告身份/绑定 hash 链。
#[derive(Debug, Clone, PartialEq)]
pub struct ReportIdentity {
    /// target schema。
    pub target_schema: &'static str,
    /// target id。
    pub target_id: String,
    /// target 状态（T50 可行性口径，不是实测）。
    pub target_status: &'static str,
    /// target sha256。
    pub target_sha256: String,
    /// 冻结基线 hash。
    pub baseline_hash: String,
    /// target 绑定的 design plant hash。
    pub design_plant_hash: String,
    /// design plant 实算 hash（须等于 design_plant_hash）。
    pub design_plant_hash_observed: String,
    /// holdout plant 实算 hash。
    pub holdout_plant_hash: String,
    /// G 解 hash（由调用方提供；可为 None）。
    pub solution_hash: Option<String>,
    /// M。
    pub m: usize,
    /// participants role ids（dense 顺序）。
    pub participants: Vec<String>,
    /// fs。
    pub sample_rate_hz: u32,
    /// layout。
    pub layout_id: String,
    /// grid 点数。
    pub grid_points: usize,
    /// 公共 D0（samples）。
    pub common_d0_samples: i64,
    /// target 附加 delay（samples）。
    pub target_delay_samples: u32,
}

/// 总判枚举（**永不**表示实测 PASS）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldoutVerdict {
    /// 预测门与 T20 门都通过且隔离通过（predicted-only）。
    PredictedPass,
    /// 明确门失败。
    PredictedFail,
    /// 证据缺失/无效/不足。
    Incomplete,
    /// 未执行校验（无重复噪声且无 IR）。
    NotRun,
}

impl HoldoutVerdict {
    /// 稳定字面值。
    pub const fn as_str(self) -> &'static str {
        match self {
            HoldoutVerdict::PredictedPass => "PREDICTED_PASS",
            HoldoutVerdict::PredictedFail => "PREDICTED_FAIL",
            HoldoutVerdict::Incomplete => "INCOMPLETE",
            HoldoutVerdict::NotRun => "NOT_RUN",
        }
    }
}

/// T53 校验报告（predicted-only；不可替代实际复测）。
#[derive(Debug, Clone, PartialEq)]
pub struct HoldoutReport {
    /// schema。
    pub schema: &'static str,
    /// 总判。
    pub verdict: HoldoutVerdict,
    /// 总判码。
    pub verdict_code: &'static str,
    /// 总判原因。
    pub verdict_reasons: Vec<&'static str>,
    /// 预声明门（固定）。
    pub declared: DeclaredGates,
    /// 身份/hash 链。
    pub identity: ReportIdentity,
    /// holdout 隔离审计。
    pub isolation: IsolationReport,
    /// 预测校验。
    pub prediction: HoldoutPrediction,
    /// T20 质量窗。
    pub t20: T20Report,
    /// design 侧预测摘要（不作 holdout 门）。
    pub design_prediction: DesignPredictionSummary,
    /// 诚实字段：是否已应用（恒 false）。
    pub applied: bool,
    /// 诚实字段：是否实测（恒 false）。
    pub measured: bool,
    /// 应用后实测状态（恒 not_run）。
    pub post_apply_measurement: &'static str,
    /// holdout 是否参与调参（恒 false）。
    pub holdout_used_for_tuning: bool,
    /// 能否替代后续实际复测（恒 false）。
    pub can_substitute_real_remeasurement: bool,
    /// 是否要求应用后复测（恒 true）。
    pub requires_post_apply_remeasurement: bool,
    /// 诚实 claim。
    pub claims: &'static str,
    /// warning 码。
    pub warnings: Vec<&'static str>,
    /// 报告内容 hash（确定性；不含本字段）。
    pub report_hash: String,
}

// ===========================================================================
// 报告序列化 / hash
// ===========================================================================

impl HoldoutReport {
    /// schema 字面值。
    pub const SCHEMA: &'static str = VALIDATE_REPORT_SCHEMA;

    /// 证据 JSON（可被独立 checker 解析；无 NaN/Inf）。
    pub fn to_evidence_json(&self) -> String {
        let mut out = String::new();
        out.push('{');
        out.push_str(&format!("\"schema\":{},", json_str(self.schema)));
        out.push_str(&format!(
            "\"verdict\":{},\"verdict_code\":{},",
            json_str(self.verdict.as_str()),
            json_str(self.verdict_code)
        ));
        out.push_str(&format!(
            "\"declared\":{{\"improvement_gate_db\":{},\"i_report_cap_db\":{},\"t20_reduction_gate_ratio\":{},\"t20_position_worsen_ratio\":{},\"near_zero_e_off_over_noise\":{},\"min_holdout_positions\":{},\"min_valid_point_fraction\":{},\"min_positions_with_four_valid_inputs\":{},\"min_positions_per_input\":{},\"max_na_fraction\":{},\"improvement_band_hz\":[{},{}],\"improvement_grid_min_coverage\":{},\"t20_noise_margin_db\":{},\"t20_fit_window_db\":[{},{}],\"t20_min_r2\":{},\"t20_max_residual_rms_db\":{},\"t20_range_s\":[{},{}],\"t20_min_snr_db\":{},\"t20_min_valid_positions\":{},\"t20_min_valid_position_fraction\":{},\"authority\":{}}},",
            json_num(self.declared.improvement_gate_db),
            json_num(self.declared.numeric_floor_i_db),
            json_num(self.declared.t20_reduction_gate_ratio),
            json_num(self.declared.t20_position_worsen_ratio),
            json_num(self.declared.near_zero_e_off_over_noise),
            self.declared.min_holdout_positions,
            json_num(self.declared.min_valid_point_fraction),
            self.declared.min_positions_with_four_valid_inputs,
            self.declared.min_positions_per_input,
            json_num(self.declared.max_na_fraction),
            json_num(self.declared.improvement_band_hz.0),
            json_num(self.declared.improvement_band_hz.1),
            json_num(self.declared.improvement_grid_min_coverage),
            json_num(self.declared.t20_noise_margin_db),
            json_num(self.declared.t20_fit_window_db.0),
            json_num(self.declared.t20_fit_window_db.1),
            json_num(self.declared.t20_min_r2),
            json_num(self.declared.t20_max_residual_rms_db),
            json_num(self.declared.t20_range_s.0),
            json_num(self.declared.t20_range_s.1),
            json_num(self.declared.t20_min_snr_db),
            self.declared.t20_min_valid_positions,
            json_num(self.declared.t20_min_valid_position_fraction),
            json_str(self.declared.authority)
        ));
        out.push_str(&format!(
            "\"identity\":{{\"target_schema\":{},\"target_id\":{},\"target_status\":{},\"target_sha256\":{},\"baseline_hash\":{},\"design_plant_hash\":{},\"design_plant_hash_observed\":{},\"holdout_plant_hash\":{},\"solution_hash\":{},\"m\":{},\"participants\":{},\"sample_rate_hz\":{},\"layout_id\":{},\"grid_points\":{},\"common_d0_samples\":{},\"target_delay_samples\":{}}},",
            json_str(self.identity.target_schema),
            json_str(&self.identity.target_id),
            json_str(self.identity.target_status),
            json_str(&self.identity.target_sha256),
            json_str(&self.identity.baseline_hash),
            json_str(&self.identity.design_plant_hash),
            json_str(&self.identity.design_plant_hash_observed),
            json_str(&self.identity.holdout_plant_hash),
            json_opt_str(&self.identity.solution_hash),
            self.identity.m,
            json_str_array(&self.identity.participants),
            self.identity.sample_rate_hz,
            json_str(&self.identity.layout_id),
            self.identity.grid_points,
            self.identity.common_d0_samples,
            self.identity.target_delay_samples
        ));
        out.push_str(&format!(
            "\"isolation\":{{\"gate\":{},\"gate_code\":{},\"reasons\":{},\"design_labels\":{},\"holdout_labels\":{},\"declared_holdout_sha256\":{},\"computed_holdout_sha256\":{},\"labels_hash_match\":{},\"tuning_labels\":{},\"tuning_inputs_sha256\":{},\"tuning_holdout_overlap\":{},\"target_reserved_labels\":{},\"target_holdout_isolated\":{},\"sealed_before_design\":{}}},",
            json_str(self.isolation.gate.as_str()),
            json_str(self.isolation.gate_code),
            json_str_array(&self.isolation.reasons),
            json_str_array(&self.isolation.design_labels),
            json_str_array(&self.isolation.holdout_labels),
            json_str(&self.isolation.declared_holdout_sha256),
            json_str(&self.isolation.computed_holdout_sha256),
            json_bool(self.isolation.labels_hash_match),
            json_str_array(&self.isolation.tuning_labels),
            json_str(&self.isolation.tuning_inputs_sha256),
            json_str_array(&self.isolation.tuning_holdout_overlap),
            json_str_array(&self.isolation.target_reserved_labels),
            json_bool(self.isolation.target_holdout_isolated),
            json_bool(self.isolation.sealed_before_design)
        ));
        out.push_str(&format!(
            "\"prediction\":{{\"gate\":{},\"gate_code\":{},\"reasons\":{},\"candidate_points\":{},\"valid_points\":{},\"near_zero_points\":{},\"invalid_points\":{},\"near_zero_fraction\":{},\"valid_fraction\":{},\"median_i_use_db\":{},\"numeric_floor\":{},\"sigma\":{{\"definition\":{},\"source\":{},\"repeat_pair_count\":{},\"formula_e_noise\":{},\"formula_sigma_i\":{},\"note\":{}}},\"per_input_min_db\":{},\"per_position_min_db\":{},\"points\":[",
            json_str(self.prediction.gate.as_str()),
            json_str(self.prediction.gate_code),
            json_str_array(&self.prediction.reasons),
            self.prediction.candidate_points,
            self.prediction.valid_points,
            self.prediction.near_zero_points,
            self.prediction.invalid_points,
            json_num(self.prediction.near_zero_fraction),
            json_num(self.prediction.valid_fraction),
            json_opt_num(self.prediction.median_i_use_db),
            json_bool(self.prediction.numeric_floor),
            json_str(self.prediction.sigma.definition),
            json_str(self.prediction.sigma.source),
            self.prediction.sigma.repeat_pair_count,
            json_str(self.prediction.sigma.formula_e_noise),
            json_str(self.prediction.sigma.formula_sigma_i),
            json_str(self.prediction.sigma.note),
            json_pairs_opt_db(&self.prediction.per_input_min_db),
            json_pairs_opt_db(&self.prediction.per_position_min_db)
        ));
        let points: Vec<String> = self.prediction.points.iter().map(point_to_json).collect();
        out.push_str(&points.join(","));
        out.push_str("],");
        out.push_str(&format!(
            "\"worst_i_use\":{}}},",
            match &self.prediction.worst_i_use {
                None => "null".to_string(),
                Some(worst) => format!(
                    "{{\"position_label\":{},\"input_role\":{},\"i_use_db\":{},\"i_raw_db\":{},\"e_off\":{},\"e_on\":{},\"e_noise\":{}}}",
                    json_str(&worst.position_label),
                    json_str(&worst.input_role),
                    json_num(worst.i_use_db),
                    json_opt_num(worst.i_raw_db),
                    json_num(worst.e_off),
                    json_num(worst.e_on),
                    json_num(worst.e_noise)
                ),
            }
        ));
        out.push_str(&format!(
            "\"t20\":{{\"gate\":{},\"gate_code\":{},\"reasons\":{},\"origin\":{},\"bands\":[",
            json_str(self.t20.gate.as_str()),
            json_str(self.t20.gate_code),
            json_str_array(&self.t20.reasons),
            json_str(self.t20.origin.as_str())
        ));
        let bands: Vec<String> = self.t20.bands.iter().map(t20_band_to_json).collect();
        out.push_str(&bands.join(","));
        out.push_str("],\"optional_bands\":[");
        let optional: Vec<String> = self
            .t20
            .optional_bands
            .iter()
            .map(t20_band_to_json)
            .collect();
        out.push_str(&optional.join(","));
        out.push_str("]},");
        out.push_str(&format!(
            "\"design_prediction\":{{\"passed\":{},\"scope\":{},\"predicted_residual_max_db\":{},\"predicted_residual_worst\":{},\"offdiag_max_db\":{},\"diag_deviation_max_db\":{},\"checks\":[",
            json_bool(self.design_prediction.passed),
            json_str(self.design_prediction.scope),
            json_opt_num(self.design_prediction.predicted_residual_max_db),
            match &self.design_prediction.predicted_residual_worst {
                None => "null".to_string(),
                Some((position, role, residual)) => format!(
                    "{{\"position\":{},\"role\":{},\"residual_db\":{}}}",
                    json_str(position),
                    json_str(role),
                    json_num(*residual)
                ),
            },
            json_num(self.design_prediction.offdiag_max_db),
            json_num(self.design_prediction.diag_deviation_max_db)
        ));
        let checks: Vec<String> = self
            .design_prediction
            .checks
            .iter()
            .map(|(name, passed, detail)| {
                format!(
                    "{{\"name\":{},\"passed\":{},\"detail\":{}}}",
                    json_str(name),
                    json_bool(*passed),
                    json_str(detail)
                )
            })
            .collect();
        out.push_str(&checks.join(","));
        out.push_str("]},");
        out.push_str(&format!(
            "\"honesty\":{{\"applied\":{},\"measured\":{},\"post_apply_measurement\":{},\"holdout_used_for_tuning\":{},\"can_substitute_real_remeasurement\":{},\"requires_post_apply_remeasurement\":{},\"claims\":{}}},",
            json_bool(self.applied),
            json_bool(self.measured),
            json_str(self.post_apply_measurement),
            json_bool(self.holdout_used_for_tuning),
            json_bool(self.can_substitute_real_remeasurement),
            json_bool(self.requires_post_apply_remeasurement),
            json_str(self.claims)
        ));
        out.push_str(&format!(
            "\"verdict_reasons\":{},\"warnings\":{},\"report_hash\":{}",
            json_str_array(&self.verdict_reasons),
            json_str_array(&self.warnings),
            json_str(&self.report_hash)
        ));
        out.push('}');
        out
    }

    /// 报告内容 hash（确定性；覆盖身份/隔离/预测点/T20/门）。
    pub fn content_hash(&self) -> String {
        sha256_hex(format!("{VALIDATE_HASH_DOMAIN}\n{}", self.canonical_content()).as_bytes())
    }

    fn canonical_content(&self) -> String {
        let mut text = String::new();
        text.push_str(VALIDATE_REPORT_SCHEMA);
        text.push('\n');
        let _ = writeln!(
            text,
            "target_sha256={} baseline_hash={} design_plant_hash={} holdout_plant_hash={} solution_hash={}",
            self.identity.target_sha256,
            self.identity.baseline_hash,
            self.identity.design_plant_hash,
            self.identity.holdout_plant_hash,
            self.identity.solution_hash.as_deref().unwrap_or("none")
        );
        let _ = writeln!(
            text,
            "verdict={} code={} gate_improvement={} gate_t20_ratio={}",
            self.verdict.as_str(),
            self.verdict_code,
            fmt_f64(self.declared.improvement_gate_db),
            fmt_f64(self.declared.t20_reduction_gate_ratio)
        );
        let _ = writeln!(
            text,
            "isolation={} labels={} tuning={}",
            self.isolation.gate.as_str(),
            self.isolation.holdout_labels.join(","),
            self.isolation.tuning_labels.join(",")
        );
        let _ = writeln!(
            text,
            "prediction gate={} valid={} na={} invalid={}",
            self.prediction.gate.as_str(),
            self.prediction.valid_points,
            self.prediction.near_zero_points,
            self.prediction.invalid_points
        );
        for point in &self.prediction.points {
            let _ = writeln!(
                text,
                "point {} {} status={} i_use={} i_raw={} e_off={} e_on={} e_noise={} sigma_i={}",
                point.position_label,
                point.input_role,
                point.status.as_str(),
                fmt_opt(point.i_use_db),
                fmt_opt(point.i_raw_db),
                fmt_opt(point.e_off),
                fmt_opt(point.e_on),
                fmt_opt(point.e_noise),
                fmt_opt(point.sigma_i_db)
            );
        }
        for band in &self.t20.bands {
            let _ = writeln!(
                text,
                "t20 band={} gate={} valid={}/{} median_off={} median_on={} ratio={}",
                fmt_f64(band.band_hz),
                band.gate.as_str(),
                band.valid_positions,
                band.total_positions,
                fmt_opt(band.median_off_s),
                fmt_opt(band.median_on_s),
                fmt_opt(band.median_ratio)
            );
            for position in &band.positions {
                let _ = writeln!(
                    text,
                    "t20pos {} band={} valid={} off={} on={} ratio={} reasons={}",
                    position.position_label,
                    fmt_f64(band.band_hz),
                    position.valid,
                    fmt_opt(position.off.as_ref().and_then(|fit| fit.t20_s)),
                    fmt_opt(position.on.as_ref().and_then(|fit| fit.t20_s)),
                    fmt_opt(position.ratio),
                    position.reasons.join(",")
                );
            }
        }
        text
    }
}

// ===========================================================================
// JSON 小工具（无第三方依赖；NaN/Inf → null）
// ===========================================================================

fn fmt_f64(value: f64) -> String {
    if !value.is_finite() {
        return "nan".to_string();
    }
    if value == 0.0 {
        return "0".to_string();
    }
    format!("{value:.17e}")
}

fn fmt_opt(value: Option<f64>) -> String {
    value.map(fmt_f64).unwrap_or_else(|| "none".to_string())
}

fn json_num(value: f64) -> String {
    if !value.is_finite() {
        "null".to_string()
    } else if value == 0.0 {
        "0".to_string()
    } else {
        format!("{value:.17e}")
    }
}

fn json_opt_num(value: Option<f64>) -> String {
    value.map(json_num).unwrap_or_else(|| "null".to_string())
}

fn json_bool(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

fn json_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_opt_str(value: &Option<String>) -> String {
    match value {
        Some(text) => json_str(text),
        None => "null".to_string(),
    }
}

fn json_str_array<S: AsRef<str>>(values: &[S]) -> String {
    let items: Vec<String> = values
        .iter()
        .map(|value| json_str(value.as_ref()))
        .collect();
    format!("[{}]", items.join(","))
}

fn json_pairs_opt_db(pairs: &[(String, Option<f64>)]) -> String {
    let items: Vec<String> = pairs
        .iter()
        .map(|(name, value)| {
            format!(
                "{{\"role\":{},\"min_i_use_db\":{}}}",
                json_str(name),
                json_opt_num(*value)
            )
        })
        .collect();
    format!("[{}]", items.join(","))
}

fn point_to_json(point: &PointPrediction) -> String {
    format!(
        "{{\"position_label\":{},\"input_role\":{},\"status\":{},\"status_reason\":{},\"e_off\":{},\"e_on\":{},\"e_noise\":{},\"e_noise_off\":{},\"i_raw_db\":{},\"i_use_db\":{},\"i_raw_truncated\":{},\"i_use_truncated\":{},\"numeric_floor\":{},\"sigma_abs_rms\":{},\"sigma_i_db\":{},\"band_points\":{},\"band_coverage\":{}}}",
        json_str(&point.position_label),
        json_str(&point.input_role),
        json_str(point.status.as_str()),
        match point.status_reason {
            Some(reason) => json_str(reason),
            None => "null".to_string(),
        },
        json_opt_num(point.e_off),
        json_opt_num(point.e_on),
        json_opt_num(point.e_noise),
        json_opt_num(point.e_noise_off),
        json_opt_num(point.i_raw_db),
        json_opt_num(point.i_use_db),
        json_bool(point.i_raw_truncated),
        json_bool(point.i_use_truncated),
        json_bool(point.numeric_floor),
        json_opt_num(point.sigma_abs_rms),
        json_opt_num(point.sigma_i_db),
        point.band_points,
        json_num(point.band_coverage)
    )
}

fn fit_to_json(fit: &T20WindowFit) -> String {
    format!(
        "{{\"band_hz\":{},\"t20_s\":{},\"slope_db_per_s\":{},\"intercept_db\":{},\"r_squared\":{},\"residual_rms_db\":{},\"slope_sigma_db_per_s\":{},\"t20_sigma_s\":{},\"noise_floor_db\":{},\"noise_margin_db\":{},\"t_minus_5_s\":{},\"t_minus_25_s\":{},\"t_minus_35_s\":{},\"duration_s\":{},\"fit_points\":{},\"valid\":{},\"reasons\":{}}}",
        json_num(fit.band_hz),
        json_opt_num(fit.t20_s),
        json_opt_num(fit.slope_db_per_s),
        json_opt_num(fit.intercept_db),
        json_opt_num(fit.r_squared),
        json_opt_num(fit.residual_rms_db),
        json_opt_num(fit.slope_sigma_db_per_s),
        json_opt_num(fit.t20_sigma_s),
        json_opt_num(fit.noise_floor_db),
        json_opt_num(fit.noise_margin_db),
        json_opt_num(fit.t_minus_5_s),
        json_opt_num(fit.t_minus_25_s),
        json_opt_num(fit.t_minus_35_s),
        json_num(fit.duration_s),
        fit.fit_points,
        json_bool(fit.valid),
        json_str_array(&fit.reasons)
    )
}

fn t20_position_to_json(position: &T20PositionReport) -> String {
    format!(
        "{{\"position_label\":{},\"valid\":{},\"reasons\":{},\"ratio\":{},\"reduction_percent\":{},\"off\":{},\"on\":{}}}",
        json_str(&position.position_label),
        json_bool(position.valid),
        json_str_array(&position.reasons),
        json_opt_num(position.ratio),
        json_opt_num(position.reduction_percent),
        match &position.off {
            Some(fit) => fit_to_json(fit),
            None => "null".to_string(),
        },
        match &position.on {
            Some(fit) => fit_to_json(fit),
            None => "null".to_string(),
        }
    )
}

fn t20_band_to_json(band: &T20BandReport) -> String {
    let positions: Vec<String> = band.positions.iter().map(t20_position_to_json).collect();
    format!(
        "{{\"band_hz\":{},\"gate\":{},\"gate_code\":{},\"reasons\":{},\"valid_positions\":{},\"total_positions\":{},\"median_off_s\":{},\"median_on_s\":{},\"median_ratio\":{},\"reduction_percent\":{},\"max_position_ratio\":{},\"positions\":[{}]}}",
        json_num(band.band_hz),
        json_str(band.gate.as_str()),
        json_str(band.gate_code),
        json_str_array(&band.reasons),
        band.valid_positions,
        band.total_positions,
        json_opt_num(band.median_off_s),
        json_opt_num(band.median_on_s),
        json_opt_num(band.median_ratio),
        json_opt_num(band.reduction_percent),
        json_opt_num(band.max_position_ratio),
        positions.join(",")
    )
}

// ===========================================================================
// 数值/网格/复响应小工具
// ===========================================================================

fn finite(value: f64, field: &str) -> Result<f64, ValidateError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(err(
            ValidateErrorKind::Missing,
            field,
            "value must be finite",
        ))
    }
}

fn complex_multiply(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0)
}

fn complex_abs_sq(a: (f64, f64)) -> f64 {
    a.0 * a.0 + a.1 * a.1
}

fn point_tuple(point: &ComplexPoint) -> (f64, f64) {
    (point.re, point.im)
}

fn band_indices(grid_hz: &[f64], lo_hz: f64, hi_hz: f64) -> Vec<usize> {
    grid_hz
        .iter()
        .enumerate()
        .filter(|(_, freq)| **freq >= lo_hz && **freq <= hi_hz)
        .map(|(index, _)| index)
        .collect()
}

fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        Some(0.5 * (sorted[middle - 1] + sorted[middle]))
    } else {
        Some(sorted[middle])
    }
}

fn db_ratio(numerator: f64, denominator: f64) -> Option<f64> {
    if numerator > 0.0 && denominator > 0.0 {
        Some(10.0 * (numerator / denominator).log10())
    } else {
        None
    }
}

fn is_hex_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn sort_labels(labels: &[String]) -> Vec<String> {
    let mut sorted = labels.to_vec();
    sorted.sort();
    sorted
}

fn sorted_strings(labels: &[&str]) -> Vec<String> {
    let mut sorted: Vec<String> = labels.iter().map(|label| (*label).to_string()).collect();
    sorted.sort();
    sorted
}

/// 网格一致（长度 + 相对容差 1e-9）。
pub fn grids_match(left: &[f64], right: &[f64]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter().zip(right.iter()).all(|(a, b)| {
        let scale = a.abs().max(b.abs()).max(1.0);
        (a - b).abs() <= 1e-9 * scale
    })
}

fn squared_error(left: &[ComplexPoint], right: &[ComplexPoint], indices: &[usize]) -> f64 {
    let mut sum = 0.0;
    for index in indices {
        let a = point_tuple(&left[*index]);
        let b = point_tuple(&right[*index]);
        sum += complex_abs_sq((a.0 - b.0, a.1 - b.1));
    }
    sum
}

// ===========================================================================
// T20：1/3 oct 带限 IR 与 Schroeder 有效窗
// ===========================================================================

/// 1/3 oct RBJ bandpass 系数（恒定峰值增益 0 dB；两级级联 = 4th order，约 24 dB/oct 裙边）。
fn third_octave_bandpass_coefficients(
    sample_rate_hz: f64,
    band_hz: f64,
) -> Result<[(f64, f64, f64, f64, f64); BAND_FILTER_STAGES], ValidateError> {
    if !(sample_rate_hz > 0.0) || !(band_hz > 0.0) || band_hz >= 0.5 * sample_rate_hz {
        return Err(err(
            ValidateErrorKind::T20,
            "t20.band_filter",
            format!("invalid fs {sample_rate_hz} / band {band_hz} for 1/3 oct bandpass"),
        ));
    }
    let bandwidth_oct = 1.0 / 3.0;
    let q = 1.0 / (2.0 * (std::f64::consts::LN_2 * 0.5 * bandwidth_oct).sinh());
    let w0 = 2.0 * std::f64::consts::PI * band_hz / sample_rate_hz;
    let (sin_w0, cos_w0) = w0.sin_cos();
    let alpha = sin_w0 / (2.0 * q);
    let a0 = 1.0 + alpha;
    let b0 = alpha / a0;
    let b1 = 0.0;
    let b2 = -alpha / a0;
    let a1 = (-2.0 * cos_w0) / a0;
    let a2 = (1.0 - alpha) / a0;
    Ok([(b0, b1, b2, a1, a2); BAND_FILTER_STAGES])
}

/// 对 raw IR 做冻结的 1/3 oct 带限（两级 RBJ bandpass；公开实现供 raw 预处理/测试）。
///
/// 注意：T20 质量窗只认证送入 BandIr 的**已带限** IR；本函数属于分析脚本侧预处理，
/// 其 filter 参数（Q、两级）冻结并在报告/证据中声明。
pub fn band_filter_ir(
    raw_ir: &[f64],
    sample_rate_hz: u32,
    band_hz: f64,
) -> Result<Vec<f64>, ValidateError> {
    if raw_ir.is_empty() {
        return Err(err(ValidateErrorKind::T20, "t20.ir", "raw IR is empty"));
    }
    for (index, sample) in raw_ir.iter().enumerate() {
        if !sample.is_finite() {
            return Err(err(
                ValidateErrorKind::T20,
                format!("t20.ir[{index}]"),
                "IR samples must be finite",
            ));
        }
    }
    let coefficients = third_octave_bandpass_coefficients(sample_rate_hz as f64, band_hz)?;
    let mut signal = raw_ir.to_vec();
    for (b0, b1, b2, a1, a2) in coefficients.iter() {
        let mut z1 = 0.0f64;
        let mut z2 = 0.0f64;
        for sample in signal.iter_mut() {
            let input = *sample;
            let output = b0 * input + z1;
            z1 = b1 * input - a1 * output + z2;
            z2 = b2 * input - a2 * output;
            *sample = output;
        }
    }
    Ok(signal)
}

fn first_crossing_seconds(decay_db: &[f64], sample_rate_hz: f64, threshold_db: f64) -> Option<f64> {
    let mut previous: Option<f64> = None;
    for (index, level) in decay_db.iter().enumerate() {
        if !level.is_finite() && !level.is_infinite() {
            return None;
        }
        if *level <= threshold_db {
            if index == 0 {
                return Some(0.0);
            }
            if let Some(previous_level) = previous {
                let span = previous_level - *level;
                let fraction = if span.abs() > 0.0 {
                    (previous_level - threshold_db) / span
                } else {
                    0.0
                };
                return Some((index as f64 - 1.0 + fraction) / sample_rate_hz);
            }
            return Some(index as f64 / sample_rate_hz);
        }
        previous = Some(*level);
    }
    None
}

/// 由**已 1/3 oct 带限** IR 估计 T20 有效窗（mimo-t6 §4.5 V1–V4；V5/V6 由 gate 汇总）。
pub fn estimate_t20_window(
    band_limited_ir: &[f64],
    sample_rate_hz: u32,
    band_hz: f64,
) -> Result<T20WindowFit, ValidateError> {
    let sample_rate = sample_rate_hz as f64;
    let count = band_limited_ir.len();
    let duration_s = count as f64 / sample_rate;
    let mut fit = T20WindowFit {
        band_hz,
        t20_s: None,
        slope_db_per_s: None,
        intercept_db: None,
        r_squared: None,
        residual_rms_db: None,
        slope_sigma_db_per_s: None,
        t20_sigma_s: None,
        noise_floor_db: None,
        noise_margin_db: None,
        t_minus_5_s: None,
        t_minus_25_s: None,
        t_minus_35_s: None,
        duration_s,
        fit_points: 0,
        valid: false,
        reasons: Vec::new(),
    };
    if count == 0 {
        fit.reasons.push("no_samples");
        return Ok(fit);
    }
    for sample in band_limited_ir {
        if !sample.is_finite() {
            fit.reasons.push("non_finite_samples");
            return Ok(fit);
        }
    }
    if !(sample_rate > 0.0) {
        return Err(err(
            ValidateErrorKind::T20,
            "t20.sample_rate_hz",
            "sample rate must be > 0",
        ));
    }

    // Schroeder 反向能量积分。
    let mut energy = 0.0f64;
    let mut decay_db = vec![0.0f64; count];
    for index in (0..count).rev() {
        energy += band_limited_ir[index] * band_limited_ir[index];
        decay_db[index] = if energy > 0.0 {
            10.0 * energy.log10()
        } else {
            f64::NEG_INFINITY
        };
    }
    let total_energy = energy;
    if !(total_energy > 0.0) || !total_energy.is_finite() {
        fit.reasons.push("zero_energy");
        return Ok(fit);
    }
    for level in decay_db.iter_mut() {
        *level -= 10.0 * total_energy.log10();
    }

    // V1/V2 噪声底：尾部 5% RMS 功率 × 时长 / 总能量。
    let tail = ((count as f64 * T20_NOISE_TAIL_FRACTION).ceil() as usize)
        .max(1)
        .min(count);
    let mut tail_power = 0.0f64;
    for sample in &band_limited_ir[count - tail..] {
        tail_power += sample * sample;
    }
    tail_power /= tail as f64;
    let noise_energy = tail_power * count as f64;
    let noise_floor_db = if noise_energy > 0.0 {
        10.0 * (noise_energy / total_energy).log10()
    } else {
        f64::NEG_INFINITY
    };
    fit.noise_floor_db = Some(noise_floor_db);

    let t_minus_5 = first_crossing_seconds(&decay_db, sample_rate, T20_FIT_LO_DB);
    let t_minus_25 = first_crossing_seconds(&decay_db, sample_rate, T20_FIT_HI_DB);
    let t_minus_35 = first_crossing_seconds(&decay_db, sample_rate, -35.0);
    fit.t_minus_5_s = t_minus_5;
    fit.t_minus_25_s = t_minus_25;
    fit.t_minus_35_s = t_minus_35;
    let (Some(_t5), Some(t25)) = (t_minus_5, t_minus_25) else {
        fit.reasons.push("fit_range_unavailable");
        return Ok(fit);
    };

    // [-25,-5] dB 区间内样本线性回归 L(t)=a+b*t。
    let mut times = Vec::new();
    let mut levels = Vec::new();
    for (index, level) in decay_db.iter().enumerate() {
        if *level <= T20_FIT_LO_DB && *level >= T20_FIT_HI_DB && level.is_finite() {
            times.push(index as f64 / sample_rate);
            levels.push(*level);
        }
    }
    fit.fit_points = times.len();
    if times.len() < T20_MIN_FIT_POINTS {
        fit.reasons.push("few_fit_points");
        return Ok(fit);
    }
    let n = times.len() as f64;
    let mean_t = times.iter().sum::<f64>() / n;
    let mean_l = levels.iter().sum::<f64>() / n;
    let mut sxx = 0.0f64;
    let mut sxy = 0.0f64;
    for (time, level) in times.iter().zip(levels.iter()) {
        let dt = time - mean_t;
        sxx += dt * dt;
        sxy += dt * (level - mean_l);
    }
    if !(sxx > 0.0) || !sxy.is_finite() {
        fit.reasons.push("fit_degenerate");
        return Ok(fit);
    }
    let slope = sxy / sxx;
    let intercept = mean_l - slope * mean_t;
    let mut ss_res = 0.0f64;
    let mut ss_tot = 0.0f64;
    for (time, level) in times.iter().zip(levels.iter()) {
        let predicted = intercept + slope * time;
        let residual = level - predicted;
        ss_res += residual * residual;
        let centered = level - mean_l;
        ss_tot += centered * centered;
    }
    let residual_rms = (ss_res / n).sqrt();
    let r_squared = if ss_tot > 0.0 {
        1.0 - ss_res / ss_tot
    } else {
        0.0
    };
    let slope_sigma = if n > 2.0 && sxx > 0.0 {
        Some((ss_res / (n - 2.0) / sxx).sqrt())
    } else {
        None
    };
    fit.slope_db_per_s = Some(slope);
    fit.intercept_db = Some(intercept);
    fit.residual_rms_db = Some(residual_rms);
    fit.r_squared = Some(r_squared);
    fit.slope_sigma_db_per_s = slope_sigma;

    if slope < 0.0 {
        let t20 = -60.0 / slope;
        fit.t20_s = Some(t20);
        if let Some(sigma_b) = slope_sigma {
            fit.t20_sigma_s = Some((t20 / slope.abs()) * sigma_b);
        }
    }

    // V1 噪声底余量。
    fit.noise_margin_db = Some(-25.0 - noise_floor_db);
    if !(-25.0 - noise_floor_db >= T20_NOISE_MARGIN_DB) {
        fit.reasons.push("v1_noise_margin");
    }
    // V2 采集时长与尾部噪声底。
    if duration_s < T20_DURATION_FACTOR * t25 {
        fit.reasons.push("v2_duration");
    }
    let last_level = decay_db[count - 1];
    if !last_level.is_finite() || last_level > noise_floor_db + T20_TAIL_MARGIN_DB {
        fit.reasons.push("v2_tail");
    }
    if let Some(t35) = t_minus_35 {
        let measurable = -35.0 >= noise_floor_db + T20_TAIL_MARGIN_DB;
        if measurable && duration_s < T20_DURATION_FACTOR * t35 {
            fit.reasons.push("v2_duration_35");
        }
    }
    // V3 拟合质量。
    if !(r_squared >= T20_MIN_R2) {
        fit.reasons.push("v3_r_squared");
    }
    if !(residual_rms <= T20_MAX_RESIDUAL_RMS_DB) {
        fit.reasons.push("v3_residual_rms");
    }
    // V4 物理合理性。
    if !(slope < 0.0) {
        fit.reasons.push("v4_slope");
    } else if let Some(t20) = fit.t20_s {
        if !(t20 >= T20_MIN_S && t20 <= T20_MAX_S) {
            fit.reasons.push("v4_t20_range");
        }
    }
    fit.valid = fit.reasons.is_empty();
    Ok(fit)
}

// ===========================================================================
// T20 gate 组装
// ===========================================================================

fn find_band_ir<'a>(
    request: &'a HoldoutRequest,
    label: &str,
    band_hz: f64,
    state: IrState,
) -> Option<&'a BandIr> {
    request.ir.iter().find(|ir| {
        ir.position_label == label && (ir.band_hz - band_hz).abs() < 1e-9 && ir.state == state
    })
}

fn ir_declared_reasons(ir: &BandIr, gates: &DeclaredGates) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if !(ir.snr_db >= gates.t20_min_snr_db) {
        reasons.push("v5_snr_below_min");
    }
    if ir.clipped {
        reasons.push("v5_clipped");
    }
    if ir.xrun {
        reasons.push("v6_xrun");
    }
    if ir.deadline_miss {
        reasons.push("v6_deadline_miss");
    }
    if ir.lost_lock {
        reasons.push("v6_lost_lock");
    }
    if ir.dropped_blocks != 0 {
        reasons.push("v6_dropped_blocks");
    }
    reasons
}

fn evaluate_t20_band(
    holdout_labels: &[String],
    request: &HoldoutRequest,
    gates: &DeclaredGates,
    sample_rate_hz: u32,
    band_hz: f64,
) -> T20BandReport {
    let mut positions = Vec::new();
    let mut valid_off = Vec::new();
    let mut valid_on = Vec::new();
    let mut valid_count = 0usize;
    let mut max_ratio: Option<f64> = None;
    for position_label in holdout_labels {
        let off_ir = find_band_ir(request, position_label, band_hz, IrState::Off);
        let on_ir = find_band_ir(request, position_label, band_hz, IrState::On);
        let mut reasons: Vec<&'static str> = Vec::new();
        let mut off_fit = None;
        let mut on_fit = None;
        match off_ir {
            None => reasons.push("missing_off_ir"),
            Some(ir) => match estimate_t20_window(&ir.samples, sample_rate_hz, band_hz) {
                Ok(mut fit) => {
                    fit.reasons.extend(ir_declared_reasons(ir, gates));
                    fit.valid = fit.reasons.is_empty();
                    off_fit = Some(fit);
                }
                Err(_) => reasons.push("off_ir_invalid"),
            },
        }
        match on_ir {
            None => reasons.push("missing_on_ir"),
            Some(ir) => match estimate_t20_window(&ir.samples, sample_rate_hz, band_hz) {
                Ok(mut fit) => {
                    fit.reasons.extend(ir_declared_reasons(ir, gates));
                    fit.valid = fit.reasons.is_empty();
                    on_fit = Some(fit);
                }
                Err(_) => reasons.push("on_ir_invalid"),
            },
        }
        let ratio = match (&off_fit, &on_fit) {
            (Some(off), Some(on)) => match (off.t20_s, on.t20_s) {
                (Some(off_t20), Some(on_t20)) if off_t20 > 0.0 => Some(on_t20 / off_t20),
                _ => None,
            },
            _ => None,
        };
        let valid = off_fit.as_ref().map(|fit| fit.valid).unwrap_or(false)
            && on_fit.as_ref().map(|fit| fit.valid).unwrap_or(false)
            && ratio.is_some();
        if valid {
            valid_count += 1;
            valid_off.push(off_fit.as_ref().and_then(|fit| fit.t20_s).unwrap());
            valid_on.push(on_fit.as_ref().and_then(|fit| fit.t20_s).unwrap());
            if let Some(ratio) = ratio {
                max_ratio = Some(max_ratio.map(|current| current.max(ratio)).unwrap_or(ratio));
            }
        } else if off_fit.as_ref().map(|fit| !fit.valid).unwrap_or(false)
            || on_fit.as_ref().map(|fit| !fit.valid).unwrap_or(false)
        {
            reasons.push("window_invalid");
        }
        positions.push(T20PositionReport {
            position_label: position_label.clone(),
            off: off_fit,
            on: on_fit,
            valid,
            reasons,
            ratio,
            reduction_percent: ratio.map(|ratio| (1.0 - ratio) * 100.0),
        });
    }
    let median_off = median(&valid_off);
    let median_on = median(&valid_on);
    let median_ratio = match (median_off, median_on) {
        (Some(off), Some(on)) if off > 0.0 => Some(on / off),
        _ => None,
    };
    let required_valid = {
        let fraction_valid =
            (T20_MIN_VALID_POSITION_FRACTION * holdout_labels.len() as f64).ceil() as usize;
        gates.t20_min_valid_positions.max(fraction_valid)
    };
    let mut gate = GateStatus::Pass;
    let mut code = "PASS";
    let mut reasons: Vec<&'static str> = Vec::new();
    if holdout_labels.is_empty() {
        gate = GateStatus::NotRun;
        code = "not_run";
        reasons.push("no_holdout_positions");
    } else if valid_count == 0 {
        // IR 已提供但没有一个有效窗：这是证据不足（I-06 INCOMPLETE），不是“未执行”
        // NOT_RUN；band 级状态与聚合后的 I-06 语义保持一致（O-3）。
        gate = GateStatus::Incomplete;
        code = "I-06";
        reasons.push("no_valid_t20_windows");
    } else if valid_count < required_valid {
        gate = GateStatus::Incomplete;
        code = "I-06";
        reasons.push("t20_valid_positions_insufficient");
        if valid_count < gates.t20_min_valid_positions {
            reasons.push("t20_below_min_valid_positions");
        }
    } else if let Some(ratio) = median_ratio {
        let mut failed = false;
        if !(ratio <= gates.t20_reduction_gate_ratio) {
            failed = true;
            reasons.push("f02_median_reduction");
        }
        if let Some(max_ratio) = max_ratio {
            if !(max_ratio <= gates.t20_position_worsen_ratio) {
                failed = true;
                reasons.push("f02_position_worsening");
            }
        }
        if failed {
            gate = GateStatus::Fail;
            code = "F-02";
        }
    } else {
        gate = GateStatus::Incomplete;
        code = "I-06";
        reasons.push("t20_median_unavailable");
    }
    T20BandReport {
        band_hz,
        gate,
        gate_code: code,
        reasons,
        positions,
        valid_positions: valid_count,
        total_positions: holdout_labels.len(),
        median_off_s: median_off,
        median_on_s: median_on,
        median_ratio,
        reduction_percent: median_ratio.map(|ratio| (1.0 - ratio) * 100.0),
        max_position_ratio: max_ratio,
    }
}

fn evaluate_t20(
    holdout_labels: &[String],
    request: &HoldoutRequest,
    gates: &DeclaredGates,
    sample_rate_hz: u32,
) -> T20Report {
    let mut bands = Vec::new();
    let mut optional_bands = Vec::new();
    for band_hz in T20_REQUIRED_BANDS_HZ {
        bands.push(evaluate_t20_band(
            holdout_labels,
            request,
            gates,
            sample_rate_hz,
            band_hz,
        ));
    }
    if request
        .ir
        .iter()
        .any(|ir| (ir.band_hz - T20_OPTIONAL_BAND_HZ).abs() < 1e-9)
    {
        optional_bands.push(evaluate_t20_band(
            holdout_labels,
            request,
            gates,
            sample_rate_hz,
            T20_OPTIONAL_BAND_HZ,
        ));
    }
    let mut gate = GateStatus::Pass;
    let mut code = "PASS";
    let mut reasons: Vec<&'static str> = Vec::new();
    if request.ir.is_empty() {
        gate = GateStatus::NotRun;
        code = "not_run";
        reasons.push("no_ir_provided");
    } else {
        let mut any_fail = false;
        let mut any_incomplete = false;
        let mut any_not_run = false;
        for band in &bands {
            match band.gate {
                GateStatus::Fail => any_fail = true,
                GateStatus::Incomplete => any_incomplete = true,
                GateStatus::NotRun => any_not_run = true,
                GateStatus::Pass => {}
            }
        }
        if any_fail {
            gate = GateStatus::Fail;
            code = "F-02";
            reasons.push("t20_gate_failed");
        } else if any_incomplete {
            gate = GateStatus::Incomplete;
            code = "I-06";
            reasons.push("t20_evidence_incomplete");
        } else if any_not_run {
            gate = GateStatus::Incomplete;
            code = "I-06";
            reasons.push("t20_band_not_run");
        }
    }
    T20Report {
        gate,
        gate_code: code,
        reasons,
        origin: request.ir_origin,
        bands,
        optional_bands,
    }
}

// ===========================================================================
// 输入契约校验
// ===========================================================================

fn check_identity_compat(
    target: &TargetMatrix,
    design_plant: &EffectivePlant,
    holdout_plant: &EffectivePlant,
) -> Result<(), ValidateError> {
    let baseline_hash = target.baseline_hash();
    for (name, plant) in [
        ("design_plant", design_plant),
        ("holdout_plant", holdout_plant),
    ] {
        if plant.baseline_hash() != baseline_hash {
            return Err(err(
                ValidateErrorKind::Baseline,
                format!("{name}.baseline_hash"),
                format!(
                    "plant baseline {} differs from target baseline {baseline_hash}",
                    plant.baseline_hash()
                ),
            ));
        }
        if plant.participants().binding_hash() != target.participants().binding_hash() {
            return Err(err(
                ValidateErrorKind::Participants,
                format!("{name}.participants"),
                "plant participants differ from target participants (explicit participant scope mismatch)",
            ));
        }
        if plant.sample_rate_hz() != target.sample_rate_hz() {
            return Err(err(
                ValidateErrorKind::SampleRate,
                format!("{name}.sample_rate_hz"),
                format!(
                    "plant fs {} differs from target fs {}",
                    plant.sample_rate_hz(),
                    target.sample_rate_hz()
                ),
            ));
        }
        if plant.layout_id() != target.layout_id() {
            return Err(err(
                ValidateErrorKind::SampleRate,
                format!("{name}.layout_id"),
                format!(
                    "plant layout {:?} differs from target layout {:?}",
                    plant.layout_id(),
                    target.layout_id()
                ),
            ));
        }
        if !grids_match(plant.grid_hz(), target.grid_hz()) {
            return Err(err(
                ValidateErrorKind::Grid,
                format!("{name}.grid_hz"),
                "plant grid differs from target grid (D0/fs/grid must be frozen)",
            ));
        }
    }
    if design_plant.hash() != target.plant_hash() {
        return Err(err(
            ValidateErrorKind::Baseline,
            "design_plant.hash",
            format!(
                "design plant hash {} does not equal target.plant_hash {}; pass the exact plant used to build the target",
                design_plant.hash(),
                target.plant_hash()
            ),
        ));
    }
    if design_plant.design_positions().count() != target.diagnostics().design_positions {
        return Err(err(
            ValidateErrorKind::Missing,
            "design_plant.design_positions",
            format!(
                "design plant has {} design positions but target was built with {}",
                design_plant.design_positions().count(),
                target.diagnostics().design_positions
            ),
        ));
    }
    Ok(())
}

fn validate_request_inputs(
    target: &TargetMatrix,
    design_plant: &EffectivePlant,
    holdout_plant: &EffectivePlant,
    request: &HoldoutRequest,
) -> Result<(), ValidateError> {
    let tuning = &request.tuning;
    let design_labels = sorted_strings(
        &design_plant
            .design_positions()
            .map(|position| position.label.as_str())
            .collect::<Vec<&str>>(),
    );
    let declared_design = sort_labels(&tuning.design_labels);
    if design_labels != declared_design {
        return Err(err(
            ValidateErrorKind::Holdout,
            "tuning.design_labels",
            format!(
                "declared design labels {declared_design:?} differ from design plant labels {design_labels:?}"
            ),
        ));
    }
    if tuning.holdout_labels.is_empty() {
        return Err(err(
            ValidateErrorKind::Holdout,
            "tuning.holdout_labels",
            "holdout declaration is empty; a T6 holdout needs >=3 validation positions",
        ));
    }
    let mut sorted_holdout = tuning.holdout_labels.clone();
    sorted_holdout.sort();
    if sorted_holdout
        .windows(2)
        .any(|window| window[0] == window[1])
    {
        return Err(err(
            ValidateErrorKind::Holdout,
            "tuning.holdout_labels",
            "holdout labels must be unique",
        ));
    }
    let plant_holdout = sorted_strings(
        &holdout_plant
            .validation_labels()
            .iter()
            .map(|label| label.as_str())
            .collect::<Vec<&str>>(),
    );
    if plant_holdout != sorted_holdout {
        return Err(err(
            ValidateErrorKind::Holdout,
            "tuning.holdout_labels",
            format!(
                "declared holdout labels {sorted_holdout:?} differ from holdout plant validation labels {plant_holdout:?}"
            ),
        ));
    }
    if !is_hex_sha256(&tuning.holdout_labels_sha256) {
        return Err(err(
            ValidateErrorKind::Holdout,
            "tuning.holdout_labels_sha256",
            "holdout labels sha256 must be 64 lowercase hex characters",
        ));
    }
    if !is_hex_sha256(&tuning.tuning_inputs_sha256) {
        return Err(err(
            ValidateErrorKind::Holdout,
            "tuning.tuning_inputs_sha256",
            "tuning inputs sha256 must be 64 lowercase hex characters (declare the actual design-side tuning inputs)",
        ));
    }

    let participants = target.participants();
    let role_ids: Vec<&str> = participants
        .participants()
        .iter()
        .map(|participant| participant.role_id.as_str())
        .collect();

    for (position_index, repeats) in request.repeats.iter().enumerate() {
        let field = format!("repeats[{position_index}]");
        if !sorted_holdout.contains(&repeats.position_label) {
            return Err(err(
                ValidateErrorKind::Repeats,
                format!("{field}.position_label"),
                format!(
                    "repeat position {:?} is not a declared holdout label (noise must be measured on holdout positions only)",
                    repeats.position_label
                ),
            ));
        }
        if repeats.roles.len() != role_ids.len() {
            return Err(err(
                ValidateErrorKind::Repeats,
                format!("{field}.roles"),
                format!(
                    "position {} has {} role repeat series but participants M={}",
                    repeats.position_label,
                    repeats.roles.len(),
                    role_ids.len()
                ),
            ));
        }
        for (role_index, role) in repeats.roles.iter().enumerate() {
            let role_field = format!("{field}.roles[{role_index}]");
            if !role_ids.contains(&role.role_id.as_str()) {
                return Err(err(
                    ValidateErrorKind::Repeats,
                    format!("{role_field}.role_id"),
                    format!("role {:?} is not a target participant", role.role_id),
                ));
            }
            if repeats.roles[..role_index]
                .iter()
                .any(|existing| existing.role_id == role.role_id)
            {
                return Err(err(
                    ValidateErrorKind::Repeats,
                    format!("{role_field}.role_id"),
                    format!("duplicate role {:?} in repeat position", role.role_id),
                ));
            }
            if role.series.len() < 2 {
                return Err(err(
                    ValidateErrorKind::Repeats,
                    format!("{role_field}.series"),
                    format!(
                        "role {:?} has {} repeat series; sigma/E_noise needs >=2 independent repeats",
                        role.role_id,
                        role.series.len()
                    ),
                ));
            }
            for (series_index, series) in role.series.iter().enumerate() {
                let series_field = format!("{role_field}.series[{series_index}]");
                if series.len() != target.grid_hz().len() {
                    return Err(err(
                        ValidateErrorKind::Repeats,
                        format!("{series_field}.len"),
                        format!(
                            "repeat series has {} points but target grid has {}",
                            series.len(),
                            target.grid_hz().len()
                        ),
                    ));
                }
                for (point_index, point) in series.iter().enumerate() {
                    if !point.freq_hz.is_finite() || !point.re.is_finite() || !point.im.is_finite()
                    {
                        return Err(err(
                            ValidateErrorKind::Repeats,
                            format!("{series_field}.points[{point_index}]"),
                            "repeat response points must be finite",
                        ));
                    }
                    let expected = target.grid_hz()[point_index];
                    let scale = expected.abs().max(point.freq_hz.abs()).max(1.0);
                    if (expected - point.freq_hz).abs() > 1e-9 * scale {
                        return Err(err(
                            ValidateErrorKind::Repeats,
                            format!("{series_field}.points[{point_index}].freq_hz"),
                            format!(
                                "repeat grid frequency {} differs from target grid {}",
                                point.freq_hz, expected
                            ),
                        ));
                    }
                }
            }
        }
    }

    let allowed_bands = [
        T20_REQUIRED_BANDS_HZ[0],
        T20_REQUIRED_BANDS_HZ[1],
        T20_OPTIONAL_BAND_HZ,
    ];
    for (ir_index, ir) in request.ir.iter().enumerate() {
        let field = format!("ir[{ir_index}]");
        if !sorted_holdout.contains(&ir.position_label) {
            return Err(err(
                ValidateErrorKind::T20,
                format!("{field}.position_label"),
                format!(
                    "IR position {:?} is not a declared holdout label",
                    ir.position_label
                ),
            ));
        }
        if !allowed_bands
            .iter()
            .any(|band| (*band - ir.band_hz).abs() < 1e-9)
        {
            return Err(err(
                ValidateErrorKind::T20,
                format!("{field}.band_hz"),
                format!(
                    "band {} Hz is not a T6 required band {:?} (optional report band {})",
                    ir.band_hz, T20_REQUIRED_BANDS_HZ, T20_OPTIONAL_BAND_HZ
                ),
            ));
        }
        if !ir.snr_db.is_finite() {
            return Err(err(
                ValidateErrorKind::T20,
                format!("{field}.snr_db"),
                "declared SNR must be finite",
            ));
        }
        if ir.samples.is_empty() {
            return Err(err(
                ValidateErrorKind::T20,
                format!("{field}.samples"),
                "IR samples are empty",
            ));
        }
        for (sample_index, sample) in ir.samples.iter().enumerate() {
            if !sample.is_finite() {
                return Err(err(
                    ValidateErrorKind::T20,
                    format!("{field}.samples[{sample_index}]"),
                    "IR samples must be finite",
                ));
            }
        }
        for (previous_index, previous) in request.ir[..ir_index].iter().enumerate() {
            if previous.position_label == ir.position_label
                && (previous.band_hz - ir.band_hz).abs() < 1e-9
                && previous.state == ir.state
            {
                return Err(err(
                    ValidateErrorKind::T20,
                    format!("{field}"),
                    format!(
                        "duplicate IR for position {} band {} state {} (also ir[{previous_index}])",
                        ir.position_label,
                        ir.band_hz,
                        ir.state.as_str()
                    ),
                ));
            }
        }
    }
    let _ = finite(0.0, "validate.internal")?;
    Ok(())
}

// ===========================================================================
// holdout 隔离审计
// ===========================================================================

fn evaluate_isolation(
    target: &TargetMatrix,
    design_plant: &EffectivePlant,
    holdout_plant: &EffectivePlant,
    request: &HoldoutRequest,
) -> IsolationReport {
    let design_labels = sorted_strings(
        &design_plant
            .design_positions()
            .map(|position| position.label.as_str())
            .collect::<Vec<&str>>(),
    );
    let holdout_labels = sort_labels(&request.tuning.holdout_labels);
    let plant_holdout = sorted_strings(
        &holdout_plant
            .validation_labels()
            .iter()
            .map(|label| label.as_str())
            .collect::<Vec<&str>>(),
    );
    let computed_holdout_sha256 = holdout_labels_sha256(&holdout_labels);
    let declared_holdout_sha256 = request.tuning.holdout_labels_sha256.clone();
    let labels_hash_match =
        computed_holdout_sha256 == declared_holdout_sha256 && plant_holdout == holdout_labels;
    let target_reserved_labels = sorted_strings(
        &target
            .diagnostics()
            .validation_positions_reserved
            .iter()
            .map(|label| label.as_str())
            .collect::<Vec<&str>>(),
    );
    let target_holdout_isolated = target.verify_holdout_isolated();
    let tuning_labels = sort_labels(&request.tuning.tuning_labels);
    let tuning_holdout_overlap: Vec<String> = tuning_labels
        .iter()
        .filter(|label| holdout_labels.iter().any(|holdout| holdout == *label))
        .cloned()
        .collect();
    let mut reasons: Vec<&'static str> = Vec::new();
    let mut fail = false;
    let mut incomplete = false;
    if !labels_hash_match {
        fail = true;
        reasons.push("holdout_labels_sha256_mismatch");
    }
    if !tuning_holdout_overlap.is_empty() {
        fail = true;
        reasons.push("tuning_labels_include_holdout");
    }
    if design_labels
        .iter()
        .any(|label| holdout_labels.contains(label))
    {
        fail = true;
        reasons.push("design_and_holdout_labels_overlap");
    }
    if !target_holdout_isolated {
        fail = true;
        reasons.push("target_holdout_not_isolated");
    }
    if !target_reserved_labels.is_empty() && target_reserved_labels != holdout_labels {
        fail = true;
        reasons.push("target_reserved_labels_mismatch");
    }
    if !request.tuning.holdout_sealed_before_design {
        incomplete = true;
        reasons.push("holdout_seal_not_provable");
    }
    let (gate, code) = if fail {
        (GateStatus::Fail, "F-07")
    } else if incomplete {
        (GateStatus::Incomplete, "I-04")
    } else {
        (GateStatus::Pass, "PASS")
    };
    IsolationReport {
        gate,
        gate_code: code,
        reasons,
        design_labels,
        holdout_labels,
        declared_holdout_sha256,
        computed_holdout_sha256,
        labels_hash_match,
        tuning_labels,
        tuning_inputs_sha256: request.tuning.tuning_inputs_sha256.clone(),
        tuning_holdout_overlap,
        target_reserved_labels,
        target_holdout_isolated,
        sealed_before_design: request.tuning.holdout_sealed_before_design,
    }
}

// ===========================================================================
// σ / E_noise 计算
// ===========================================================================

/// 按重复对计算 σ²_c（每频点）；返回 (σ²_c 序列, 使用重复对数)。
fn role_sigma_squared(role: &RoleRepeats, grid_len: usize) -> (Vec<f64>, usize) {
    let mut sigma_squared = vec![0.0f64; grid_len];
    let mut pair_count = 0usize;
    for first in 0..role.series.len() {
        for second in (first + 1)..role.series.len() {
            pair_count += 1;
            for index in 0..grid_len {
                let a = point_tuple(&role.series[first][index]);
                let b = point_tuple(&role.series[second][index]);
                let difference = (a.0 - b.0, a.1 - b.1);
                sigma_squared[index] += 0.5 * complex_abs_sq(difference);
            }
        }
    }
    if pair_count > 0 {
        for value in sigma_squared.iter_mut() {
            *value /= pair_count as f64;
        }
    }
    (sigma_squared, pair_count)
}

fn position_noise_maps(
    request: &HoldoutRequest,
    position_label: &str,
    participants: &ModalParticipants,
    grid_len: usize,
) -> Option<(Vec<Option<Vec<f64>>>, usize)> {
    let repeats = request
        .repeats
        .iter()
        .find(|repeats| repeats.position_label == position_label)?;
    let mut maps = Vec::with_capacity(participants.m());
    let mut pair_count = 0usize;
    for participant in participants.participants() {
        let role = repeats
            .roles
            .iter()
            .find(|role| role.role_id == participant.role_id)?;
        let (sigma_squared, pairs) = role_sigma_squared(role, grid_len);
        pair_count += pairs;
        maps.push(Some(sigma_squared));
    }
    Some((maps, pair_count))
}

// ===========================================================================
// holdout 预测校验
// ===========================================================================

fn evaluate_prediction(
    target: &TargetMatrix,
    holdout_plant: &EffectivePlant,
    solution: &ModalMatrix,
    request: &HoldoutRequest,
    gates: &DeclaredGates,
) -> HoldoutPrediction {
    let participants = target.participants();
    let m = participants.m();
    let grid_hz = target.grid_hz();
    let zone = band_indices(
        grid_hz,
        gates.improvement_band_hz.0,
        gates.improvement_band_hz.1,
    );
    let expected_points =
        expected_1_24_oct_points(gates.improvement_band_hz.0, gates.improvement_band_hz.1);
    let band_coverage = if expected_points > 0 {
        zone.len() as f64 / expected_points as f64
    } else {
        0.0
    };
    let mut points: Vec<PointPrediction> = Vec::new();
    let mut pair_count_total = 0usize;
    let holdout_positions: Vec<&EffectivePosition> = holdout_plant.validation_positions().collect();
    let mut early_gate: Option<(GateStatus, &'static str, Vec<&'static str>)> = None;
    if target.is_position_variant() {
        early_gate = Some((
            GateStatus::Incomplete,
            "I-09",
            vec!["preserve_target_not_frozen_on_holdout"],
        ));
    }
    for position in &holdout_positions {
        let observation =
            position_noise_maps(request, &position.label, participants, grid_hz.len());
        if let Some((_, pairs)) = &observation {
            pair_count_total += pairs;
        }
        let mut cell_points = Vec::with_capacity(m);
        for participant in participants.participants() {
            let mut point = PointPrediction {
                position_label: position.label.clone(),
                input_role: participant.role_id.clone(),
                status: PointStatus::Valid,
                status_reason: None,
                e_off: None,
                e_on: None,
                e_noise: None,
                e_noise_off: None,
                i_raw_db: None,
                i_use_db: None,
                numeric_floor: false,
                i_raw_truncated: false,
                i_use_truncated: false,
                sigma_abs_rms: None,
                sigma_i_db: None,
                band_points: zone.len(),
                band_coverage,
            };
            if band_coverage < gates.improvement_grid_min_coverage
                || zone.len() < min_improvement_band_points()
            {
                point.status = PointStatus::InsufficientCoverage;
                point.status_reason = Some("improvement_band_grid_coverage_below_gate");
                cell_points.push(point);
                continue;
            }
            let target_points =
                match target.cell(None, participant.dense_index, participant.dense_index) {
                    TargetCellView::Response(points) => points,
                    TargetCellView::Zero => {
                        point.status = PointStatus::MissingTarget;
                        point.status_reason = Some("target_diagonal_cell_missing");
                        cell_points.push(point);
                        continue;
                    }
                };
            let mut h_eff: Vec<&[ComplexPoint]> = Vec::with_capacity(m);
            let mut missing_h = false;
            for output in participants.participants() {
                match position.input(&output.role_id) {
                    Some(input) => h_eff.push(input.points.as_slice()),
                    None => {
                        missing_h = true;
                        break;
                    }
                }
            }
            if missing_h {
                point.status = PointStatus::NonFinite;
                point.status_reason = Some("holdout_position_missing_h_eff");
                cell_points.push(point);
                continue;
            }
            let e_off = squared_error(
                h_eff[participant.dense_index as usize],
                target_points,
                &zone,
            );
            // T50/T51 语义：单测量位置响应 y_pred(p,i,f) = Σ_j H_p,j(f)·G_{j,i}(f)，
            // 对角目标 D_{i,i}(f)（非对角结构零在声学求和后不单独建模）。
            let mut e_on = 0.0f64;
            let mut non_finite = false;
            for index in &zone {
                let mut predicted_re = 0.0f64;
                let mut predicted_im = 0.0f64;
                for (output_dense, output) in participants.participants().iter().enumerate() {
                    let gain = match solution.cell(output.dense_index, participant.dense_index) {
                        Some(points) => points,
                        None => {
                            if output.dense_index == participant.dense_index {
                                non_finite = true;
                            }
                            continue;
                        }
                    };
                    let h = point_tuple(&h_eff[output_dense][*index]);
                    let g = point_tuple(&gain[*index]);
                    let (re, im) = complex_multiply(h, g);
                    predicted_re += re;
                    predicted_im += im;
                }
                let target = point_tuple(&target_points[*index]);
                e_on += complex_abs_sq((predicted_re - target.0, predicted_im - target.1));
            }
            if non_finite || !e_off.is_finite() || !e_on.is_finite() {
                point.status = PointStatus::NonFinite;
                point.status_reason = Some("non_finite_prediction_error");
                cell_points.push(point);
                continue;
            }
            point.e_off = Some(e_off);
            point.e_on = Some(e_on);
            match &observation {
                None => {
                    point.status = PointStatus::NoNoiseData;
                    point.status_reason = Some("no_repeats_for_position");
                    cell_points.push(point);
                    continue;
                }
                Some((maps, _)) => {
                    let mut e_noise = 0.0f64;
                    let mut sigma_sum = 0.0f64;
                    let mut missing_role = false;
                    for (output_dense, output) in participants.participants().iter().enumerate() {
                        let sigma_squared = match &maps[output_dense] {
                            Some(map) => map,
                            None => {
                                missing_role = true;
                                break;
                            }
                        };
                        let gain = match solution.cell(output.dense_index, participant.dense_index)
                        {
                            Some(points) => points,
                            None => {
                                if output.dense_index == participant.dense_index {
                                    non_finite = true;
                                }
                                continue;
                            }
                        };
                        for index in &zone {
                            let g = point_tuple(&gain[*index]);
                            let gain_power = complex_abs_sq(g);
                            e_noise += sigma_squared[*index] * gain_power;
                            sigma_sum += sigma_squared[*index];
                        }
                    }
                    if missing_role {
                        point.status = PointStatus::NoNoiseData;
                        point.status_reason = Some("missing_repeat_role");
                        cell_points.push(point);
                        continue;
                    }
                    if non_finite || !e_noise.is_finite() {
                        point.status = PointStatus::NonFinite;
                        point.status_reason = Some("non_finite_noise");
                        cell_points.push(point);
                        continue;
                    }
                    let e_noise_off: f64 = {
                        let sigma_squared = match &maps[participant.dense_index as usize] {
                            Some(map) => map,
                            None => {
                                point.status = PointStatus::NoNoiseData;
                                point.status_reason = Some("missing_repeat_role");
                                cell_points.push(point);
                                continue;
                            }
                        };
                        zone.iter().map(|index| sigma_squared[*index]).sum()
                    };
                    point.e_noise = Some(e_noise);
                    point.e_noise_off = Some(e_noise_off);
                    point.sigma_abs_rms = Some((sigma_sum / (m * zone.len()).max(1) as f64).sqrt());
                    if e_off > 0.0 && e_on > 0.0 {
                        let rel_off = e_noise_off / e_off;
                        // I_use 的门用的是 max(E_on,E_noise)；σ_I 的 on 侧分母同样取该 max，
                        // 避免 E_on 远低于噪声底时给出无意义的 1e18 dB 不确定度。
                        let rel_on = e_noise / e_on.max(e_noise);
                        point.sigma_i_db = Some(
                            (10.0 / std::f64::consts::LN_10)
                                * (rel_off * rel_off + rel_on * rel_on).sqrt(),
                        );
                    }
                    if e_off <= gates.near_zero_e_off_over_noise * e_noise {
                        point.status = PointStatus::NearZero;
                        point.status_reason = Some("e_off_le_4x_e_noise");
                        cell_points.push(point);
                        continue;
                    }
                    let denominator = e_on.max(e_noise);
                    let cap_db = gates.numeric_floor_i_db;
                    if denominator == 0.0 {
                        point.numeric_floor = true;
                        point.i_raw_db = Some(cap_db);
                        point.i_use_db = Some(cap_db);
                        // 理论边界（E_on/E_noise 同为 0）：I 为 +inf，按 T6 §4.3 报告上限截断。
                        point.i_raw_truncated = true;
                        point.i_use_truncated = true;
                    } else {
                        // T6 §4.3：I_raw 与 I_use 都受 60 dB 报告上限约束（不参与超额加分）。
                        let raw = db_ratio(e_off, e_on);
                        point.i_raw_db = raw.map(|value| value.min(cap_db));
                        point.i_raw_truncated = raw.map(|value| value > cap_db).unwrap_or(false);
                        let used = db_ratio(e_off, denominator);
                        point.i_use_db = used.map(|value| value.min(cap_db));
                        point.i_use_truncated = used.map(|value| value > cap_db).unwrap_or(false);
                        if e_on == 0.0 || e_noise == 0.0 {
                            point.numeric_floor = true;
                        }
                    }
                    point.status = PointStatus::Valid;
                    point.status_reason = None;
                    cell_points.push(point);
                }
            }
        }
        points.extend(cell_points);
    }

    let candidate_points = points.len();
    let valid_points = points
        .iter()
        .filter(|point| point.status == PointStatus::Valid)
        .count();
    let near_zero_points = points
        .iter()
        .filter(|point| point.status == PointStatus::NearZero)
        .count();
    let invalid_points = candidate_points.saturating_sub(valid_points + near_zero_points);
    let valid_fraction = if candidate_points > 0 {
        valid_points as f64 / candidate_points as f64
    } else {
        0.0
    };
    let near_zero_fraction = if candidate_points > 0 {
        near_zero_points as f64 / candidate_points as f64
    } else {
        0.0
    };
    let i_use_values: Vec<f64> = points
        .iter()
        .filter(|point| point.status == PointStatus::Valid)
        .filter_map(|point| point.i_use_db)
        .collect();
    let median_i_use = median(&i_use_values);
    let mut worst: Option<WorstIUse> = None;
    for point in points
        .iter()
        .filter(|point| point.status == PointStatus::Valid)
    {
        let (Some(i_use), Some(e_off), Some(e_on), Some(e_noise)) =
            (point.i_use_db, point.e_off, point.e_on, point.e_noise)
        else {
            continue;
        };
        let replace = worst
            .as_ref()
            .map(|current| i_use < current.i_use_db)
            .unwrap_or(true);
        if replace {
            worst = Some(WorstIUse {
                position_label: point.position_label.clone(),
                input_role: point.input_role.clone(),
                i_use_db: i_use,
                i_raw_db: point.i_raw_db,
                e_off,
                e_on,
                e_noise,
            });
        }
    }
    let mut per_input_min: Vec<(String, Option<f64>)> = participants
        .participants()
        .iter()
        .map(|participant| {
            let min = points
                .iter()
                .filter(|point| {
                    point.status == PointStatus::Valid && point.input_role == participant.role_id
                })
                .filter_map(|point| point.i_use_db)
                .fold(None::<f64>, |current, value| {
                    Some(current.map(|existing| existing.min(value)).unwrap_or(value))
                });
            (participant.role_id.clone(), min)
        })
        .collect();
    per_input_min.sort_by(|left, right| left.0.cmp(&right.0));
    let mut per_position_min: Vec<(String, Option<f64>)> = holdout_positions
        .iter()
        .map(|position| {
            let min = points
                .iter()
                .filter(|point| {
                    point.status == PointStatus::Valid && point.position_label == position.label
                })
                .filter_map(|point| point.i_use_db)
                .fold(None::<f64>, |current, value| {
                    Some(current.map(|existing| existing.min(value)).unwrap_or(value))
                });
            (position.label.clone(), min)
        })
        .collect();
    per_position_min.sort_by(|left, right| left.0.cmp(&right.0));

    let mut gate = GateStatus::Pass;
    let mut gate_code = "PASS";
    let mut reasons: Vec<&'static str> = Vec::new();
    let below_gate = worst
        .as_ref()
        .map(|worst| worst.i_use_db < gates.improvement_gate_db)
        .unwrap_or(false);
    if let Some((early_status, early_code, early_reasons)) = early_gate {
        gate = early_status;
        gate_code = early_code;
        reasons.extend(early_reasons);
    } else if holdout_positions.len() < gates.min_holdout_positions {
        gate = GateStatus::Incomplete;
        gate_code = "I-03";
        reasons.push("holdout_positions_below_min");
    } else if request.repeats.is_empty() {
        gate = GateStatus::NotRun;
        gate_code = "not_run";
        reasons.push("no_repeat_noise");
    } else if below_gate {
        gate = GateStatus::Fail;
        gate_code = "F-01";
        reasons.push("f01_improvement_gate");
        if let Some(worst) = &worst {
            if worst.i_use_db < gates.improvement_gate_db {
                reasons.push("f01_worst_point_below_gate");
            }
        }
        for (_, min) in &per_input_min {
            if min
                .map(|value| value < gates.improvement_gate_db)
                .unwrap_or(false)
            {
                reasons.push("f01_per_input_min_below_gate");
                break;
            }
        }
    } else if valid_points == 0 {
        gate = GateStatus::Incomplete;
        gate_code = "I-07";
        reasons.push("no_valid_holdout_points");
    } else {
        let mut incomplete = false;
        if valid_fraction < gates.min_valid_point_fraction {
            incomplete = true;
            reasons.push("i07_valid_point_fraction_below_min");
        }
        if near_zero_fraction > gates.max_na_fraction {
            incomplete = true;
            reasons.push("i07_near_zero_fraction_above_max");
        }
        let mut positions_with_four = 0usize;
        for position in &holdout_positions {
            let count = points
                .iter()
                .filter(|point| {
                    point.status == PointStatus::Valid && point.position_label == position.label
                })
                .count();
            if count >= 4 {
                positions_with_four += 1;
            }
        }
        if positions_with_four < gates.min_positions_with_four_valid_inputs {
            incomplete = true;
            reasons.push("i07_positions_with_four_valid_inputs_below_min");
        }
        let mut input_position_counts: Vec<usize> = Vec::new();
        for participant in participants.participants() {
            let count = holdout_positions
                .iter()
                .filter(|position| {
                    points.iter().any(|point| {
                        point.status == PointStatus::Valid
                            && point.position_label == position.label
                            && point.input_role == participant.role_id
                    })
                })
                .count();
            input_position_counts.push(count);
        }
        if input_position_counts
            .iter()
            .any(|count| *count < gates.min_positions_per_input)
        {
            incomplete = true;
            reasons.push("i07_input_positions_below_min");
        }
        if points
            .iter()
            .any(|point| point.status == PointStatus::InsufficientCoverage)
        {
            incomplete = true;
            reasons.push("i07_grid_coverage_insufficient");
        }
        if incomplete {
            gate = GateStatus::Incomplete;
            gate_code = "I-07";
        }
    }
    let numeric_floor = points.iter().any(|point| point.numeric_floor);
    let sigma = SigmaModel {
        definition: "sigma_squared_complex_c(p,j,f) = mean_{a<b} 0.5*|X^(a)-X^(b)|^2 over repeat pairs (T6 §4.3 E_noise convention; per-measurement complex noise variance)",
        source: "off/baseline repeat measurements at holdout positions, propagated through the applied G",
        repeat_pair_count: pair_count_total,
        formula_e_noise: "E_noise(p,i) = sum_j sum_f sigma_squared_c(p,j,f)*|G_ji(f)|^2; E_noise_off(p,i) = sum_f sigma_squared_c(p,i,f)",
        formula_sigma_i: "sigma_I = 10/ln(10)*sqrt((E_noise_off/E_off)^2 + (E_noise/max(E_on,E_noise))^2) dB (first-order delta-method on I_use, design-side proxy only)",
        note: "design-side noise proxy, not a measured confidence interval; a zero (or fully identical) repeat set reduces I_use to I_raw and never adds credit",
    };
    HoldoutPrediction {
        gate,
        gate_code,
        reasons,
        points,
        candidate_points,
        valid_points,
        near_zero_points,
        invalid_points,
        near_zero_fraction,
        valid_fraction,
        median_i_use_db: median_i_use,
        worst_i_use: worst,
        per_input_min_db: per_input_min,
        per_position_min_db: per_position_min,
        numeric_floor,
        sigma,
    }
}

// ===========================================================================
// 主入口
// ===========================================================================

/// T53 holdout 预测校验主入口（设计侧 predicted-only；不替代实际复测）。
///
/// 输入：target（T50，只由 design positions 构建）、design_plant（**必须**是构建 target 的
/// 同一 plant，hash 绑定）、holdout_plant（独立 validation 测点；不得参与调参）、solution
/// （T51/T52 的完整 G）、request（调参/密封声明 + 重复噪声 + T20 IR）。
pub fn validate_holdout(
    target: &TargetMatrix,
    design_plant: &EffectivePlant,
    holdout_plant: &EffectivePlant,
    solution: &ModalMatrix,
    request: &HoldoutRequest,
) -> Result<HoldoutReport, ValidateError> {
    check_identity_compat(target, design_plant, holdout_plant)?;
    validate_request_inputs(target, design_plant, holdout_plant, request)?;
    let design_validation = validate_solution(target, design_plant, solution).map_err(|error| {
        err(
            ValidateErrorKind::Matrix,
            error.field(),
            format!("{}: {}", error.code(), error.detail()),
        )
    })?;
    let isolation = evaluate_isolation(target, design_plant, holdout_plant, request);
    let prediction = evaluate_prediction(target, holdout_plant, solution, request, &DECLARED_GATES);
    let t20 = evaluate_t20(
        &isolation.holdout_labels,
        request,
        &DECLARED_GATES,
        target.sample_rate_hz(),
    );
    let design_prediction = DesignPredictionSummary {
        passed: design_validation.passed,
        checks: design_validation
            .checks
            .iter()
            .map(|check| (check.name.to_string(), check.passed, check.detail.clone()))
            .collect(),
        predicted_residual_max_db: design_validation.residual_max_db,
        predicted_residual_worst: design_validation.residual_worst.clone(),
        offdiag_max_db: design_validation.offdiag_max_db,
        diag_deviation_max_db: design_validation.diag_deviation_max_db,
        scope: "design_positions_only; not a holdout gate and not a measured improvement",
    };

    let mut fails: Vec<&'static str> = Vec::new();
    let mut incompletes: Vec<&'static str> = Vec::new();
    let mut not_runs: Vec<&'static str> = Vec::new();
    match isolation.gate {
        GateStatus::Fail => fails.push(isolation.gate_code),
        GateStatus::Incomplete => incompletes.push(isolation.gate_code),
        GateStatus::NotRun => not_runs.push(isolation.gate_code),
        GateStatus::Pass => {}
    }
    match prediction.gate {
        GateStatus::Fail => fails.push(prediction.gate_code),
        GateStatus::Incomplete => incompletes.push(prediction.gate_code),
        GateStatus::NotRun => not_runs.push(prediction.gate_code),
        GateStatus::Pass => {}
    }
    match t20.gate {
        GateStatus::Fail => fails.push(t20.gate_code),
        GateStatus::Incomplete => incompletes.push(t20.gate_code),
        GateStatus::NotRun => not_runs.push(t20.gate_code),
        GateStatus::Pass => {}
    }
    let (verdict, verdict_code) = if let Some(code) = fails.first() {
        (HoldoutVerdict::PredictedFail, *code)
    } else if let Some(code) = incompletes.first() {
        (HoldoutVerdict::Incomplete, *code)
    } else if prediction.gate == GateStatus::NotRun && t20.gate == GateStatus::NotRun {
        (HoldoutVerdict::NotRun, "not_run")
    } else if !not_runs.is_empty() {
        (HoldoutVerdict::Incomplete, not_runs[0])
    } else {
        (HoldoutVerdict::PredictedPass, "PREDICTED_PASS")
    };
    let mut verdict_reasons: Vec<&'static str> = Vec::new();
    verdict_reasons.extend(isolation.reasons.iter().copied());
    verdict_reasons.extend(prediction.reasons.iter().copied());
    verdict_reasons.extend(t20.reasons.iter().copied());

    let mut warnings: Vec<&'static str> = vec![
        "predicted_only_not_measured",
        "not_a_substitute_for_post_apply_remeasurement",
    ];
    if isolation.holdout_labels.len() < DECLARED_GATES.min_holdout_positions {
        warnings.push("holdout_positions_below_formal_minimum");
    }
    if request.repeats.is_empty() {
        warnings.push("no_repeat_noise_provided");
    } else if prediction
        .points
        .iter()
        .all(|point| point.e_noise.map(|value| value == 0.0).unwrap_or(false))
    {
        warnings.push("zero_noise_estimate");
    }
    if request.ir.is_empty() {
        warnings.push("no_t20_ir_provided");
    }
    if target.is_position_variant() {
        warnings.push("preserve_target_not_holdout_frozen");
    }
    if request.ir_origin == IrOrigin::PredictedModel {
        warnings.push("t20_ir_origin_predicted_model");
    }
    if !design_validation.passed {
        warnings.push("design_prediction_validation_not_passed");
    }

    let identity = ReportIdentity {
        target_schema: target.schema(),
        target_id: target.target_id().to_string(),
        target_status: target.status().as_str(),
        target_sha256: target.target_sha256(),
        baseline_hash: target.baseline_hash().to_string(),
        design_plant_hash: target.plant_hash().to_string(),
        design_plant_hash_observed: design_plant.hash(),
        holdout_plant_hash: holdout_plant.hash(),
        solution_hash: solution_hash_hint(solution),
        m: target.participants().m(),
        participants: target
            .participants()
            .participants()
            .iter()
            .map(|participant| participant.role_id.clone())
            .collect(),
        sample_rate_hz: target.sample_rate_hz(),
        layout_id: target.layout_id().to_string(),
        grid_points: target.grid_hz().len(),
        common_d0_samples: target.common_d0().samples(),
        target_delay_samples: target.target_delay_samples(),
    };

    let mut report = HoldoutReport {
        schema: VALIDATE_REPORT_SCHEMA,
        verdict,
        verdict_code,
        verdict_reasons,
        declared: DECLARED_GATES,
        identity,
        isolation,
        prediction,
        t20,
        design_prediction,
        applied: false,
        measured: false,
        post_apply_measurement: "not_run",
        holdout_used_for_tuning: false,
        can_substitute_real_remeasurement: false,
        requires_post_apply_remeasurement: true,
        claims: VALIDATE_CLAIM,
        warnings,
        report_hash: String::new(),
    };
    report.report_hash = report.content_hash();
    Ok(report)
}

fn solution_hash_hint(solution: &ModalMatrix) -> Option<String> {
    let mut text = String::from("khaen-modal-holdout-solution-hint/v1\n");
    let _ = writeln!(text, "roles={}", solution.roles().join(","));
    let _ = writeln!(text, "grid_points={}", solution.grid_hz().len());
    for cell in solution.cells() {
        let _ = writeln!(
            text,
            "cell {} {} points={}",
            cell.out_dense,
            cell.in_dense,
            cell.points.len()
        );
        for point in &cell.points {
            let _ = writeln!(
                text,
                "{} {} {}",
                fmt_f64(point.freq_hz),
                fmt_f64(point.re),
                fmt_f64(point.im)
            );
        }
    }
    Some(sha256_hex(text.as_bytes()))
}

/// 本文件源码（静态守卫测试用：无 I/O/线程/环境/序列化/unsafe 依赖）。
pub fn validate_source_text_for_guards() -> &'static str {
    include_str!("validate.rs")
}
