//! TSPI benchmark 与 trace metrics（crate khaen-bench）——WBS T06 scaffold。
//!
//! T47 写域：crates/bench/src/（整个 src 由 T47 组织，见 WBS T47 行）。
//! T47 仅编写基准工具/trace metrics，无板执行；kernel wall T1/T4/Amdahl 分报，
//! busy_compute/wait/max_kernel/queue、CV/range 单位可复算。
//!
//! scaffold 状态：NOT_RUN。

pub mod metrics;
