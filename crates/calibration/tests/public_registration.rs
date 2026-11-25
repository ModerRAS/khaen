//! 公共测试注册（crate khaen-calibration）。
//!
//! 约定：leaf 测试文件放 tests/<Txx>/ 子目录，由本文件用 `#[path = "<Txx>/<file>.rs"] mod ...;`
//! 注册为集成测试目标（cargo 只把本文件当 integration test 目标；子目录里的 `main.rs` 除外）。
//! 其余 leaf 测试经各 src 模块末尾的 `#[cfg(test)] include!(...)` 进入 lib 单测，**不要**在此重复注册。
//!
//! 迁入范围（T11）：T60（job 契约）与 T61（sweep 契约）在此注册；T63/T64/T65 由
//! positions/peq_fit/phase_fit 模块自注册。T62/T66/T67/T68/T70/T75/T76 依赖未迁入的
//! host 侧模块（capture/stage_backend/compile_apply/fit_wiring），见迁移报告。

// WBS T60（校准 job 公共契约）集成测试（5 tests）。
#[path = "T60/job_contract.rs"]
mod t60_job_contract;

// WBS T61（逐输出扫频 / 渲染时间轴公共契约）集成测试。
#[path = "T61/sweep_contract.rs"]
mod t61_sweep_contract;
