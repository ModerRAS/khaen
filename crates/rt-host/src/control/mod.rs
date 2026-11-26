//! rt-host 跨进程控制通道（T70f-a）：契约 B 的行 JSON 服务端。
//!
//! daemon（控制面，REST）与 rt-host（数据面，AES67 -> 方案图 -> ALSA）是两个进程；
//! 本模块在 rt-host 进程内提供控制通道，把契约 B 的 op 变成对块线程唯一 owner 的 Runtime
//! 的块边界动作。daemon 侧的代理（把 REST 请求转成本协议的客户端）不在本任务（T70f-b）。
//!
//! # 分层（传输与协议核心解耦）
//!
//!     Unix socket（cfg(unix)，unix.rs）     每连接一个处理线程
//!            | 一行 JSON 请求 / 一行 JSON 响应（transport.rs）
//!       Mutex<ControlCore>（core.rs）        平台无关的协议状态机：读文件/解码/准入/编译、
//!            |                              token、surface 补丁、回滚、格式/模式草稿、status
//!            | mpsc 命令 + 一次性 ack（command.rs）
//!       块线程（recovery::drive_loop）       唯一 Runtime owner：块边界执行 mute / 换图，
//!            |                             按 KHAEN_STATS_EVERY_BLOCKS 发布状态快照
//!       Arc<Mutex<Option<RuntimeSnapshot>>>（snapshot.rs）
//!
//! 关键约束：
//! - 控制线程不直接碰 Runtime/AudioBackend：只发命令，等一次性 ack 回执；
//! - Runtime::run_block（RT 路径）一行不加：命令在块循环的块边界用 try_recv 取出执行，
//!   状态快照在块循环里发布，控制线程只读快照（附 age），不阻塞块线程；
//! - 设备缺失（T42o absent）时控制通道照常响应 status；设备缺失期间块循环用小片睡眠轮询命令，
//!   所以 mute / commit 仍会被受理，图在恢复出块后的第一个块边界生效（如实上报 applied_block）。
//!
//! # 传输
//!
//! - Unix domain socket：std::os::unix::net::UnixListener（cfg(unix)），路径取
//!   KHAEN_CONTROL_SOCKET，默认 /run/khaen/rt-host.sock；KHAEN_CONTROL=off 关闭。
//! - 一行 JSON 请求 / 一行 JSON 响应；UTF-8、无 BOM、单行；请求行上限 64 KiB
//!   （khaen_host::control::MAX_REQUEST_BYTES），超限回 bad_request 并丢弃到下一个换行
//!   （单行坏请求不中断连接）。
//! - 非 Unix 平台没有传输实现（Windows 上本进程本来就只是编译桩）；协议核心仍平台无关地
//!   编译并用内存双工/回环夹具测试。
//!
//! # 能力上报（如实）
//!
//! 已实现的 op 全部在 status.wired 里；五个语义界面 surfaces 全部接通（surface 的响应
//! 形状与 host 的 ControlState 一致）。format 的真实边界：**数据面执行器/设备格式在
//! rt-host 进程生命周期内固定**，format 的两阶段只改控制面的编译格式，commit 如实报
//! device_restart_required=true / pcm_reconfigured=false，不谎称已重开 PCM
//! （M3b 热重配未实现）。状态里 format_mismatch / notes.format_live_change 说明这一点。

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub mod bridge;
pub mod command;
pub mod core;
pub mod snapshot;
pub mod transport;

#[cfg(unix)]
mod unix;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub use bridge::ControlBridge;
pub use core::{ControlCore, ControlCoreConfig};

/// 控制通道开关环境变量：off / 0 / false 关闭，on / 1 / true 或不设 = 打开。
pub const CONTROL_ENV: &str = "KHAEN_CONTROL";
/// 控制 socket 路径环境变量（默认 DEFAULT_CONTROL_SOCKET）。
pub const CONTROL_SOCKET_ENV: &str = "KHAEN_CONTROL_SOCKET";
/// 默认 Unix socket 路径（systemd 部署里通常由 RuntimeDirectory 建目录）。
pub const DEFAULT_CONTROL_SOCKET: &str = "/run/khaen/rt-host.sock";

/// 控制通道配置（来自环境变量；解析逻辑平台无关，便于测试）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlConfig {
    /// 是否启动控制通道（KHAEN_CONTROL=off 关闭）。
    pub enabled: bool,
    /// Unix socket 路径（非 Unix 平台不用，但配置解析照常进行）。
    pub socket_path: PathBuf,
}

impl Default for ControlConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            socket_path: PathBuf::from(DEFAULT_CONTROL_SOCKET),
        }
    }
}

/// 控制通道配置错误（配置/环境类，宿主按配置错误退出码 2 处理）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlConfigError {
    message: String,
}

impl ControlConfigError {
    /// 人读消息。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for ControlConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ControlConfigError {}

impl ControlConfig {
    /// 从环境变量读取；非法值点名变量（不静默回退，避免"以为开了其实没开"）。
    pub fn from_env() -> Result<Self, ControlConfigError> {
        Self::parse(
            std::env::var(CONTROL_ENV).ok(),
            std::env::var(CONTROL_SOCKET_ENV).ok(),
        )
    }

    /// 纯解析（测试用，不读环境）：control = KHAEN_CONTROL 的原始值。
    pub fn parse(
        control: Option<String>,
        socket: Option<String>,
    ) -> Result<Self, ControlConfigError> {
        let enabled = match control.as_deref().map(str::trim) {
            None | Some("") => true,
            Some(value) if value.eq_ignore_ascii_case("on") => true,
            Some(value) if value.eq_ignore_ascii_case("true") => true,
            Some("1") => true,
            Some(value) if value.eq_ignore_ascii_case("off") => false,
            Some(value) if value.eq_ignore_ascii_case("false") => false,
            Some("0") => false,
            Some(other) => {
                return Err(ControlConfigError {
                    message: format!("{CONTROL_ENV} must be on/off/1/0/true/false, got {other:?}"),
                });
            }
        };
        let socket_path = match socket.as_deref().map(str::trim) {
            None => PathBuf::from(DEFAULT_CONTROL_SOCKET),
            Some("") => {
                return Err(ControlConfigError {
                    message: format!("{CONTROL_SOCKET_ENV} must not be empty"),
                });
            }
            Some(path) => PathBuf::from(path),
        };
        Ok(Self {
            enabled,
            socket_path,
        })
    }
}

/// 控制通道服务端句柄：Drop 时停止接受新连接并移除 socket 文件。
///
/// 注意：已有连接的处理器线程是分离的（每连接一个），进程退出/句柄 Drop 时不会强杀它们；
/// 生产进程退出时随进程结束，测试里由客户端关闭连接让它们退出。
#[cfg(unix)]
#[derive(Debug)]
pub struct ControlServer {
    inner: unix::UnixControlServer,
}

#[cfg(not(unix))]
#[derive(Debug)]
pub struct ControlServer {
    /// 非 Unix 平台没有传输实现，本类型只用于统一函数签名（永不被构造）。
    _private: (),
}

impl ControlServer {
    /// 实际监听的 socket 路径（非 Unix 平台为 None）。
    pub fn socket_path(&self) -> Option<&Path> {
        #[cfg(unix)]
        {
            Some(self.inner.path())
        }
        #[cfg(not(unix))]
        {
            None
        }
    }
}

/// 启动控制通道（bind + accept 线程）。失败原因原样返回；**调用方不得因此退出进程**
/// （数据面优先：控制通道不可用只打印原因并继续，见 app::run）。
pub fn start(config: &ControlConfig, core: Arc<Mutex<ControlCore>>) -> io::Result<ControlServer> {
    #[cfg(unix)]
    {
        unix::UnixControlServer::start(&config.socket_path, core)
            .map(|inner| ControlServer { inner })
    }
    #[cfg(not(unix))]
    {
        let _ = (config, core);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "control channel transport requires Unix domain sockets; this platform has no rt-host data plane (compile-only stub)",
        ))
    }
}
