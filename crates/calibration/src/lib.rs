//! 校准数学核心：反卷积、扫频时间轴、IR 提取/几何、噪声估计、RT60、PEQ/相位拟合
//! 与多点位置契约。
//!
//! 迁自 v1 `khaen-calibration` + `khaen-cal-dsp`（`legacy/pre-rewrite`）。本 crate 只含
//! 纯离线数学与契约层：不打开声卡、不依赖 host/daemon、不读仓库外路径。
//!
//! **T70h-a 回迁 spike（capture/时基/测量骨架）**：`capture`（T62 UMIK 采集）、
//! `stage_backend`（T67 stage 测量后端，Fake/Tools 引擎）、rt-host 的 `clock`/`format`
//! 经 `legacy_clock`/`legacy_format` 原样迁入；v2 core 缺的 v1 硬件 ABI/RT 错误码/控制块
//! 由 `legacy_contract` 契约替身承接（正式迁移应换 v3 类型后删除）。旧 Plan(compile/apply)
//! 路径按任务卡先 stub 成 `E_STAGE_NOT_WIRED`（T76 未迁入）。
//!
//! **仍未迁入**（依赖重写中的 host/core，见迁移报告）：`measured_design`/`fit_wiring`/
//! `host_apply`（T67/T70/T76 接线）、`compile_apply`/`peq_plan`（T66 plan 编译/应用）、
//! 以及 CLI bin。这些模块需要 v1 core 的 plan ABI v1/v2 与 factorized/fused compile 引擎、
//! v1 rt-host 的 admission/swap、以及 v1 compat 的 JSON 层；新仓的 core（T10，方案 v3 ABI）
//! 与 host（T20+）只提供 v2 契约，不含这些 v1 实现，故按任务规则不复制（见迁移报告）。
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
    clippy::large_enum_variant,
    clippy::len_zero,
    clippy::let_and_return,
    clippy::manual_clamp,
    clippy::manual_contains,
    clippy::manual_div_ceil,
    clippy::manual_range_contains,
    clippy::manual_is_multiple_of,
    clippy::manual_memcpy,
    clippy::needless_borrows_for_generic_args,
    clippy::needless_range_loop,
    clippy::neg_cmp_op_on_partial_ord,
    clippy::question_mark,
    clippy::redundant_closure,
    clippy::single_match,
    clippy::too_many_arguments,
    clippy::unnecessary_cast,
    clippy::unnecessary_map_or,
    clippy::while_let_loop
)]

pub mod band_levels;
pub mod closed_loop;
pub mod closed_loop_run;
pub mod deconvolve;
pub mod fir_plan;
pub mod ir_distance;
pub mod ir_extract;
pub mod job;
pub mod microfft;
pub mod noise_est;
pub mod offline_job;
pub mod peq_fit;
pub mod phase_fit;
pub mod plan_compile;
pub mod plan_levels;
pub mod positions;
pub mod rt60;
pub mod single_point_bundle;
pub mod single_point_job;
pub mod single_point_scene;
pub mod sweep;
pub mod wav;

pub mod capture;
pub mod stage_backend;

pub(crate) mod legacy_clock;
pub(crate) mod legacy_contract;
pub(crate) mod legacy_format;

pub use band_levels::{
    band_powers, relative_band_levels, BandLevelsConfig, BandLevelsError,
    ALIGNMENT as BAND_LEVELS_ALIGNMENT, BAD_INPUT as BAND_LEVELS_BAD_INPUT,
    CONFIG as BAND_LEVELS_CONFIG, DEFAULT_WINDOW_LEN,
};
pub use closed_loop::{
    compare, levels_bytes, levels_sha256, report_bytes, report_sha256, BandComparison, BandLevel,
    ClosedLoopConfig, ClosedLoopError, ClosedLoopReport, LevelSet, PointComparison, PointLevels,
    Verdict, DEFAULT_MIN_COMPARED_BANDS, DEFAULT_TOLERANCE_DB,
    LEVELS_SCHEMA as CLOSED_LOOP_LEVELS_SCHEMA, REPORT_SCHEMA as CLOSED_LOOP_REPORT_SCHEMA,
};
pub use closed_loop_run::{
    build_run_bundle, ClosedLoopRunError, RunBundle, RunConfig, RunInputs,
    MANIFEST_SCHEMA as CLOSED_LOOP_RUN_MANIFEST_SCHEMA, TOOL_NAME as CLOSED_LOOP_RUN_TOOL_NAME,
};
pub use deconvolve::deconvolve;
pub use fir_plan::{
    compile_fir_plan, CompiledFirPlan, FirPlanConfig, FirPlanError, LaneFir,
    BAD_INPUT as FIR_PLAN_BAD_INPUT, BUILD as FIR_PLAN_BUILD, CONFIG as FIR_PLAN_CONFIG,
    ENCODE as FIR_PLAN_ENCODE,
};
pub use plan_compile::{
    compile_peq_design, CompiledPlan, PlanCompileError, PlanCompileOptions, PLAN_ARTIFACT_PATH,
};
pub use plan_levels::{
    lane_name, predicted_levels, PlanLevelsError, DEFAULT_CENTERS_HZ,
    TOOL_NAME as PLAN_LEVELS_TOOL_NAME,
};
pub use single_point_bundle::{
    build_bundle, design_digest, verify_bundle, write_bundle, BundleError, BundleFile, PlanBundle,
    Provenance, BUNDLE_SCHEMA, MANIFEST_SCHEMA, REPORT_SCHEMA,
};
pub use single_point_scene::{
    fir_artifact_path, run_single_point_scene, write_scene_artifacts, SceneArtifacts,
    SceneBridgeError, SceneSummary, FIR_ARTIFACT_DIR, METRICS_ARTIFACT_PATH, REPORT_ARTIFACT_PATH,
};
pub use wav::{parse_wav, read_wav_file, Wav, WavError};
