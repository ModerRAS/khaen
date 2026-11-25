//! 模态 MIMO 设计：测量摄取（ingest）、公共时基（timebase）、目标（target）、
//! 求解（solve）、正则化（regularize）、验证（validate）与逐路径系数 artifact。
//!
//! 纯离线设计工具：无硬件、无仓库外路径依赖。迁自 v1 `khaen-modal-design`
//! （`legacy/pre-rewrite`），公共 API 与测试用例保持原样。

// 迁移说明：本 crate 从 legacy/pre-rewrite 原样迁入，算法与数值路径不改写。下列 clippy 建议
// 属风格/数值改写建议（部分会改变 NaN/边界语义）；迁移期统一在 crate 级允许，避免为过 lint
// 改动算术路径——行为回归由 tests/ 的冻结数值测试保护。理由与 mimo-design 迁移先例一致。
#![allow(
    clippy::chunks_exact_to_as_chunks,
    clippy::collapsible_if,
    clippy::doc_lazy_continuation,
    clippy::manual_contains,
    clippy::manual_inspect,
    clippy::manual_is_multiple_of,
    clippy::manual_pattern_char_comparison,
    clippy::manual_range_contains,
    clippy::manual_range_patterns,
    clippy::manual_slice_fill,
    clippy::needless_borrow,
    clippy::needless_range_loop,
    clippy::neg_cmp_op_on_partial_ord,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::useless_format
)]

pub mod artifact;
pub mod ingest;
pub mod regularize;
pub mod solve;
pub mod target;
pub mod timebase;
pub mod validate;
