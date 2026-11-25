//! 校准数学核心：反卷积、扫频时间轴、IR 提取/几何、噪声估计、RT60、PEQ/相位拟合
//! 与多点位置契约。
//!
//! 迁自 v1 `khaen-calibration` + `khaen-cal-dsp`（`legacy/pre-rewrite`）。本 crate 只含
//! 纯离线数学与契约层：不打开声卡、不依赖 host/daemon、不读仓库外路径。
//!
//! **未迁入**（依赖重写中的 host/core，见迁移报告）：`capture`（T62 UMIK 采集）、
//! `stage_backend`（T67 stage 测量后端）、`measured_design`/`fit_wiring`/`host_apply`
//! （T67/T70/T76 接线）、`compile_apply`/`peq_plan`（T66 plan 编译/应用）、以及 CLI bin。
//! 这些模块需要 v1 core 的 plan ABI v1/v2 与 factorized/fused compile 引擎、v1 rt-host 的
//! admission/swap、以及 v1 compat 的 JSON 层；新仓的 core（T10，方案 v3 ABI）与 host（T20+）
//! 只提供 v2 契约，不含这些 v1 实现，故按任务规则不复制（见迁移报告"需要 core 提供什么"）。
//!
//! RT 常量与块描述统一来自 `khaen_core::rt`（v2 唯一来源，不再本地镜像）。

// 迁移说明：本 crate 从 legacy/pre-rewrite 原样迁入，算法与数值路径不改写。下列 clippy 建议
// 属风格/数值改写建议（部分会改变 NaN/边界语义）；迁移期统一在 crate 级允许，避免为过 lint
// 改动算术路径——行为回归由 tests/ 的冻结数值测试保护。理由与 mimo-design 迁移先例一致。
#![allow(
    clippy::bool_assert_comparison,
    clippy::bool_comparison,
    clippy::chunks_exact_to_as_chunks,
    clippy::clone_on_copy,
    clippy::doc_lazy_continuation,
    clippy::explicit_auto_deref,
    clippy::field_reassign_with_default,
    clippy::len_zero,
    clippy::let_and_return,
    clippy::manual_clamp,
    clippy::manual_div_ceil,
    clippy::manual_is_multiple_of,
    clippy::manual_memcpy,
    clippy::needless_borrows_for_generic_args,
    clippy::needless_range_loop,
    clippy::neg_cmp_op_on_partial_ord,
    clippy::question_mark,
    clippy::redundant_closure,
    clippy::single_match,
    clippy::too_many_arguments,
    clippy::unnecessary_map_or
)]

pub mod deconvolve;
pub mod ir_distance;
pub mod ir_extract;
pub mod job;
pub mod microfft;
pub mod noise_est;
pub mod peq_fit;
pub mod phase_fit;
pub mod plan_compile;
pub mod positions;
pub mod rt60;
pub mod sweep;

#[path = "capture_consts.rs"]
pub mod capture;

pub use deconvolve::deconvolve;
pub use plan_compile::{
    compile_peq_design, CompiledPlan, PlanCompileError, PlanCompileOptions, PLAN_ARTIFACT_PATH,
};
