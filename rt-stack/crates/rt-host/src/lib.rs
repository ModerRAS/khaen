//! TSPI RT host（crate khaen-rt-host）——WBS T06 scaffold。
//!
//! RT host 层：SPSC、admission、clock、swap、format、fault、ALSA/AoIP/scheduler 等。
//! **本 crate 目前只有 stub**：T16（spsc）、T17（alsa）、T18（admission）、T19（clock）、
//! T20（swap）、T21（fault）、T22（format）、T43（aoip）、T45（scheduler）实现各 leaf；
//! T26 新增 src/main.rs（host/daemon bin）并串行整合。lib.rs/公共注册只由 integrator 写。
//!
//! 权威契约：rt-stack/spec/core-v1.md（T02）。scaffold 状态：NOT_RUN。

pub mod admission;
/// 校准 Unix socket 与进程内虚拟输入。播放模式不改输入缓冲。
pub mod cal_control;
/// task-235：[NONRT] 自研用户态 AES67 接收端 → host capture 固定块适配（库级接线）。
pub mod aes67_rx;
pub mod alsa;
pub mod aoip;
pub mod clock;
/// T203/task-210：[NONRT] host 自持缺中置内容适配（compat 依赖已移除）。
pub mod content_map;
pub mod fault;
pub mod format;
/// task-253（rt-mimo12k）：MIMO 12 kHz 低频修正支路 host 接线（plan-arena 直通 + 12k 修正量）。
pub mod mimo12k;
pub mod scheduler;
pub mod spsc;
pub mod swap;

// ---------------------------------------------------------------------------
// Linux 真实 ALSA 后端依赖注册（WBS T90；Lead 2026-11-17 批准 target-specific 依赖）
// ---------------------------------------------------------------------------
//
// `alsa` crate 只在 Linux target 参与编译与链接（见 crates/rt-host/Cargo.toml 的
// `[target.'cfg(target_os = "linux")'.dependencies]`）；Windows/macOS 开发与 CI 不编译它，
// 继续使用 T17 的 fake backend（src/alsa.rs；无硬件打开路径）。
// 真实 backend 接线（open/hw_params/actual/start/read/write/recover/close）归 T17-follow-up/T26；
// 本项只提供 cfg 门控的依赖可见性，避免 Linux 依赖泄漏到 Windows 构建。

/// 本 target 是否编译了真实 ALSA 后端依赖（Linux = true；Windows/CI = false）。
#[cfg(target_os = "linux")]
pub const REAL_ALSA_BACKEND_DEP: bool = true;
/// 本 target 是否编译了真实 ALSA 后端依赖（Linux = true；Windows/CI = false）。
#[cfg(not(target_os = "linux"))]
pub const REAL_ALSA_BACKEND_DEP: bool = false;

/// [Linux only] 真实 ALSA backend crate（`alsa` 0.12，Cargo.lock 锁定精确版本）。
/// 非 Linux target 不编译/不导出本项；T17 fake backend 保持不变。
#[cfg(target_os = "linux")]
pub use alsa as alsa_backend_crate;
