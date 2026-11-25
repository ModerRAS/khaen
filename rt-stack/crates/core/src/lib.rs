//! TSPI DSP core（crate tspi-dsp-core）——WBS T06 scaffold。
//!
//! 本 crate 是 RT-first DSP 栈的 core 层：ABI v1 字节契约、RT 类型/错误码、常量、
//! plan 构建/校验（NONRT）与 kernel 模块注册。**本 crate 目前只有 stub**：
//! T07（direct）、T08（partitioned）、T09（workers）、T12（compile）、T23（nodes）、
//! T56（schedule_plan）实现各自 leaf 模块；本 lib.rs 与公共类型注册只由 integrator 写。
//!
//! 权威契约：projects/tspi-audio-dsp/rt-stack/spec/core-v1.md（T02）。
//! 参考语义：projects/tspi-audio-dsp/rt-stack/reference/SPEC.md（T03）。
//!
//! scaffold 状态：NOT_RUN（无 leaf 实现、无板端、无 RT 测量）。

pub mod abi;
pub mod compile;
pub mod direct;
pub mod events;
/// task-253（rt-mimo12k）：MIMO 12 kHz 低频修正支路（抽取/插值 + 复用 T08 分区卷积）。
pub mod mimo12k;
pub mod nodes;
pub mod partitioned;
pub mod plan;
pub mod rt;
pub mod schedule_plan;
pub mod workers;
