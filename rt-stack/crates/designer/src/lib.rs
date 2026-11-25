//! KHAEN FIR 工具箱设计器（crate khaen-designer） ——WBS T06 scaffold。
//!
//! **本 crate 目前只有 stub**：leaf 模块由各任务实现；lib.rs/公共注册只由 integrator 写。
//! scaffold 状态：NOT_RUN（无 leaf 实现、无板端、无 RT 测量）。
//!
//! 权威契约：rt-stack/spec/core-v1.md（T02）与 rt-stack/README.md 写域纪律。

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
