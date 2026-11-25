//! khaen 主机侧：块引擎、设备所有权、SPSC 传输与后端接口。
//!
//! 本 crate 只做"设备和块"这一层，不含控制协议（T25）与 daemon（T30）。
//! 实时约束：`BlockEngine::run_block` 与 `AudioBackend::capture/playout` 不得分配、加锁或做 IO。

pub mod backend;
pub mod control;
pub mod engine;
pub mod owner;
pub mod ring;

pub use backend::{AudioBackend, BackendError, DeviceConfig, NullBackend};
pub use control::{ControlState, PROTOCOL_VERSION};
pub use engine::{
    BlockEngine, EngineConfig, EngineError, EngineStats, MuteReceipt, Passthrough, Processor,
    Silent, DEFAULT_BLOCK_FRAMES, MAX_BLOCK_FRAMES,
};
pub use owner::{DeviceGuard, DeviceId, OwnerError};
pub use ring::{channel, Consumer, Producer};
