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
//! - pipeline：场景级编排（B0/B1/B2/B3 + 3 折 design-only CV 选 λ + report.json）。
//!
//! 所有产物必须标注 SIMULATED / UNPROVEN；predicted 不得写成 PASS。指标口径以
//! rt-stack/spec/mimo-objective-v1.md 为准（与 CONTRACT 冲突时以 spec 为准并在此说明）。

pub mod baseline;
pub mod constraints;
pub mod decay;
pub mod emperor;
pub mod evaluate;
pub mod fill;
pub mod fir;
pub mod objective;
pub mod pipeline;
pub mod report;
pub mod solve;
pub mod target;

pub use decay::{candidate_b_target, evaluate_decay, fit_path_firs, t20_from_ir, Grid, MatrixMode};
pub use evaluate::{BandPlan, EModal, HoldoutEvaluation, SpatialStats};
pub use fill::{
    donor_criterion, evaluate_bands, taps_requirements, FillConfig, FillDesign, FillSolver,
    SinglePointH,
};
pub use pipeline::{run_scene, RunConfig, SceneInput, SceneRun};
pub use solve::Cx;
