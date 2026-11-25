//! 公共测试注册（integrator 独占；crate tspi-fir-designer）。
//!
//! 约定（见 rt-stack/README.md §测试约定）：
//! - leaf 测试文件放本目录的 <Txx>/ 子目录，例如 tests/T07/direct.rs；
//! - 合入时由 integrator 在本文件加 #[path = "<Txx>/<file>.rs"] mod ...; 行，
//!   不直接改 leaf 文件；cargo 只把本文件当 integration test 目标。
//!
//! T06 scaffold 阶段：尚无 leaf 测试注册（0 tests 合法，见 evidence/T06/manifest.md）。

// WBS T10（linear 设计器）集成测试；evidence/T10/manifest.md 请求 integrator 注册（34 tests）。
// `tests/T33..T38/T41` 等使用 src/*.rs 内 `#[cfg(test)] include!` 自注册，**不要**在此重复注册。
#[path = "T10/linear_contract.rs"]
mod t10_linear_contract;
#[path = "T10/linear_dod.rs"]
mod t10_linear_dod;
#[path = "T10/linear_vectors.rs"]
mod t10_linear_vectors;
