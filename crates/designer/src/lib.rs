//! FIR 滤波器工具箱：线性相位 / 最小相位 / 砖墙 / 变迹 / 分频 / 目标导入。
//!
//! 纯数学设计器：无 I/O、无硬件、无仓库外路径依赖。迁自 v1 `khaen-designer`
//! （`legacy/pre-rewrite`），公共 API 与测试用例保持原样。

// 迁移说明：本 crate 从 legacy/pre-rewrite 原样迁入，算法与数值路径不改写。下列 clippy 建议
// 属风格/数值改写建议（部分会改变 NaN/边界语义）；迁移期统一在 crate 级允许，避免为过 lint
// 改动算术路径——行为回归由 tests/ 的冻结数值测试保护。理由与 mimo-design 迁移先例一致。
#![allow(
    clippy::approx_constant,
    clippy::bool_comparison,
    clippy::chunks_exact_to_as_chunks,
    clippy::collapsible_if,
    clippy::doc_lazy_continuation,
    clippy::doc_overindented_list_items,
    clippy::field_reassign_with_default,
    clippy::if_same_then_else,
    clippy::int_plus_one,
    clippy::len_zero,
    clippy::manual_clamp,
    clippy::manual_contains,
    clippy::manual_div_ceil,
    clippy::manual_is_multiple_of,
    clippy::needless_range_loop,
    clippy::neg_cmp_op_on_partial_ord,
    clippy::question_mark,
    clippy::single_element_loop,
    clippy::too_many_arguments,
    clippy::unnecessary_map_or,
    clippy::useless_borrows_in_formatting,
    clippy::useless_format,
    clippy::vec_init_then_push
)]

pub mod apodizing;
pub mod brickwall;
pub mod corrected;
pub mod crossover;
pub mod linear;
pub mod min_fast;
pub mod min_slow;
pub mod modal_peq;
pub mod nos;
pub mod phase;
pub mod report;
pub mod targets;
