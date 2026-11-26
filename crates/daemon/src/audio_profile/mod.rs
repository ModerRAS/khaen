//! 音频通道 profile 后端（docs/API.md §12，v1.2 新增）。
//!
//! 组成：
//! - model：[AudioProfile] 数据模型 + 规范化解析 + §12.1 校验（4 类结构性错误 + warning）+
//!   prepare 的 diff + 启用 lane 排序；
//! - store：audio-profile.json 原子持久化 + 两阶段草稿（prepare/commit/abort）；
//! - plan：profile -> plan ABI v3（复用 khaen-plan-builder）+ 只读 inspect 预览；
//! - api：GET/PUT /api/audio-profile、POST /api/audio-profile/plan、GET /api/device/inputs。
//!
//! 边界：本模块只做控制面（profile 配置 + 方案生成），保存 profile 不改数据面；
//! 不写 CueMix、不给未使用 DAC 做任何操作（§12.4）。

pub mod api;
pub mod model;
pub mod plan;
pub mod store;

pub use api::routes;
pub use model::{AudioProfile, SpeakerRole, SCHEMA};
pub use store::AudioProfileStore;

#[cfg(test)]
mod tests;
