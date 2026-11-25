//! 公共测试注册（integrator 独占；crate khaen-calibration）。
//!
//! 约定（见 rt-stack/README.md §测试约定）：
//! - leaf 测试文件放本目录的 <Txx>/ 子目录，例如 tests/T07/direct.rs；
//! - 合入时由 integrator 在本文件加 #[path = "<Txx>/<file>.rs"] mod ...; 行，
//!   不直接改 leaf 文件；cargo 只把本文件当 integration test 目标。
//!
//! T06 scaffold 阶段：尚无 leaf 测试注册（0 tests 合法，见 evidence/T06/manifest.md）。

// WBS T60（校准 job 公共契约）集成测试；evidence/T60/MANIFEST.md 请求 integrator 注册（5 tests）。
#[path = "T60/job_contract.rs"]
mod t60_job_contract;
