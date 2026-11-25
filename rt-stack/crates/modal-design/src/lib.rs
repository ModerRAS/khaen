//! TSPI 模态 MIMO 设计（crate khaen-modal-design） ——WBS T06。
//!
//! **真实现状（非 stub）**：已公开 leaf 模块 `artifact` / `ingest` / `timebase` / `target` /
//! `solve` / `regularize` / `validate`；各模块由对应 WBS 任务实现，并通过 crate 内
//! `#[cfg(test)]` leaf 测试（`tests/T49..T54`、`public_registration` 等）离线验证。
//! 已验证范围：`cargo test -p khaen-modal-design --offline` 的离线单元/集成测试（见
//! evidence/T49..T54）。**未上板、未实测**：无真实 IR/房间测量、无应用后 T6 数据。
//!
//! 权威契约：rt-stack/spec/core-v1.md（T02）与 rt-stack/README.md 写域纪律。

pub mod artifact;
pub mod ingest;
pub mod regularize;
pub mod solve;
pub mod target;
pub mod timebase;
pub mod validate;
