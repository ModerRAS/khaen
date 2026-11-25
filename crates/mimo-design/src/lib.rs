//! 低频 MIMO 优化（calib-chain-20261121 / mimo-opt-20261122）。
//!
//! **SIMULATED / SYNTHETIC ONLY**：输入必须是 room-sim 生成的仿真 H(f)，或格式相同但明确标注为合成的数据。
//! KHAEN 生产设计路径仍是 khaen-modal-design（真实测量 provenance 门 + CGLS 求解器）；
//! 本 crate 是仿真沙盒，用于在真实测量到位前验证目标函数/约束/指标口径。
//!
//! 模块：
//! - solve：复数值类型 + N<=4 复高斯消元 + 逐频点加权岭最小二乘 + IRLS MIMO FIR 设计；
//! - fir：实对称 Toeplitz 正规方程 + Levinson-Durbin O(L²) + 精确频点响应；
//! - target：design 空间 RMS 目标 A(f)（功率域 1/12 oct 平滑、相位 0、beta 缩放）；
//! - baseline：B0 RAW 与 B1 delay/gain/polarity 坐标下降；
//! - constraints：增益帽 / headroom 缩放 / E_effort / 峰值界报告；
//! - evaluate：契约 §5 指标（R_k / E_modal / 空间统计 / holdout 一致性/严重恶化）；
//! - report：最小 JSON writer、%.17g、FIR CSV；
//! - quality：4.1 测量质量门（逐 1/3 oct SNR → Known/Unknown，Unknown 排除拟合、不等于能力不足）；
//! - authority：4.4 权威加权（正则项 Γ_s(f) = Γ0/|H_s(f)|²，默认关闭）；
//! - limits：4.4 安全钳位（单段 +3 dB / 正向和 6 dB / 预测峰值 −1 dBFS）+ FULL/PARTIAL/NONE；
//! - pipeline：场景级编排（B0/B1/B2/B3 + 3 折 design-only CV 选 λ + report.json）。
//!
//! 所有产物必须标注 SIMULATED / UNPROVEN；predicted 不得写成 PASS。指标口径以旧仓
//! `rt-stack/spec/mimo-objective-v1.md` 为准（该 spec 未随 v2 迁入；历史实现见
//! `legacy/pre-rewrite` 分支，v2 契约见仓库 `docs/CONTRACTS.md`）。
//!
//! v2 迁移说明：纯算法/编排层原样迁入；legacy 的 CLI harness（`examples/` 四个离线
//! 分析程序，读 CSV/room-sim 证据）不随仓迁入，避免仓库外数据依赖。

// 迁移说明：本 crate 从 legacy/pre-rewrite 原样迁入，算法与数值路径不改写。下列 clippy
// 建议属风格/数值改写建议；迁移期统一在 crate 级允许，避免为过 lint 改动算术路径
// （行为回归由 tests/ 的冻结数值测试保护）。
#![allow(
    clippy::needless_range_loop,
    clippy::field_reassign_with_default,
    clippy::type_complexity,
    clippy::needless_borrow,
    clippy::if_same_then_else,
    clippy::should_implement_trait,
    clippy::manual_is_multiple_of,
    clippy::manual_div_ceil,
    clippy::manual_range_contains,
    clippy::chunks_exact_to_as_chunks,
    clippy::neg_cmp_op_on_partial_ord,
    clippy::doc_overindented_list_items,
    clippy::doc_lazy_continuation,
    clippy::len_without_is_empty,
    clippy::too_many_arguments
)]

pub mod authority;
pub mod baseline;
pub mod constraints;
pub mod decay;
pub mod emperor;
pub mod evaluate;
pub mod fill;
pub mod fir;
pub mod limits;
pub mod objective;
pub mod pipeline;
pub mod quality;
pub mod report;
pub mod solve;
pub mod target;

pub use authority::{authority_weights, AuthorityConfig, AuthorityWeights};
pub use decay::{candidate_b_target, evaluate_decay, fit_path_firs, t20_from_ir, Grid, MatrixMode};
pub use evaluate::{BandPlan, EModal, HoldoutEvaluation, SpatialStats};
pub use fill::{
    donor_criterion, evaluate_bands, taps_requirements, FillConfig, FillDesign, FillSolver,
    SinglePointH,
};
pub use limits::{
    clamp_solution, predicted_peak_dbfs, ClampHit, ClampInput, LimitKind, OutcomeState, Residual,
    SafetyConfig, SafetyReport, MAX_BAND_BOOST_DB, MAX_POSITIVE_SUM_DB, MAX_PREDICTED_PEAK_DBFS,
    REFERENCE_PEAK_DBFS,
};
pub use pipeline::{run_scene, RunConfig, SceneInput, SceneRun};
pub use quality::{
    band_snr_curve, band_snr_db, band_snr_estimate, gate_from_band_snr, gate_from_evidence,
    gate_from_ir, BandQuality, BandState, QualityConfig, QualityGate, DEFAULT_MIN_SNR_DB,
    THIRD_OCTAVE_CENTERS_HZ_20_20K,
};
pub use solve::{Cx, DesignOptions};
