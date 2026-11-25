//! khaen 主机侧：块引擎、设备所有权、SPSC 传输与后端接口。
//!
//! 本 crate 只做"设备和块"这一层，不含控制协议（T25）与 daemon（T30）。
//! 实时约束：`BlockEngine::run_block` 与 `AudioBackend::capture/playout` 不得分配、加锁或做 IO。

pub mod admission;
pub mod backend;
pub mod control;
pub mod engine;
pub mod graph;
pub mod owner;
pub mod ring;
pub mod safety;

pub use admission::{admit, Admission, AdmissionLimits, Budget, Rejection};
pub use backend::{AudioBackend, BackendError, DeviceConfig, NullBackend};
pub use control::{ControlError, ControlState, PROTOCOL_VERSION};
pub use engine::{
    BlockEngine, EngineConfig, EngineError, EngineStats, GraphInfo, MuteReceipt, Passthrough,
    Processor, Silent, SlotEngine, DEFAULT_BLOCK_FRAMES, MAX_BLOCK_FRAMES,
};
pub use graph::{CompileError, PlanGraph};
pub use owner::{DeviceGuard, DeviceId, OwnerError};
pub use ring::{channel, Consumer, Producer};
pub use safety::{
    FaultReason, FaultRecord, RecoveryError, RuntimeState, SafetyPolicy, SafetyStatus,
    SafetySupervisor,
};
// 控制面的输入是方案 ABI v3 与方案文件 sha256；这里重导出，避免上层（daemon/测试）再直接依赖
// khaen-core，也保证"校验用的 sha256 实现"只有一份。
pub use khaen_core::plan;
pub use khaen_core::sha256::sha256_hex;
