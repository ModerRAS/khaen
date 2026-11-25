//! KHAEN control 层（crate khaen-control） ——WBS T06 scaffold。
//!
//! **本 crate 目前只有 stub**：leaf 模块由各任务实现；lib.rs/公共注册只由 integrator 写。
//! scaffold 状态：NOT_RUN（无 leaf 实现、无板端、无 RT 测量）。
//!
//! 权威契约：rt-stack/spec/core-v1.md（T02）与 rt-stack/README.md 写域纪律。

pub mod host_socket;
pub mod modes;
pub mod rest;
pub mod web;
pub mod webui_assets;
