//! 单进程运行时接线（T70d）：AES67 UDP 收包线程 → SPSC → 块引擎 → ALSA 输出。
//!
//! 把三块已经就位的组件接成一条可跑的通路：
//!
//! 收包线程（std::net::UdpSocket）      SPSC（ring::channel，无锁）        RT / 块线程
//! recv() → RTP datagram → push → [已分配 PacketSlot 槽位] → pop → feed_packet()
//!                                                                  fill_block()
//!                                                                  SlotEngine::run_block()
//!                                                                  safety 安全静音
//!                                                                  AlsaOutputBackend::playout()
//!
//! # 线程与职责
//!
//! | 线程 | 入口 | 允许做的事 |
//! | --- | --- | --- |
//! | 收包线程 | Aes67Receiver | socket 收包、把整包拷进 SPSC（不解析、不分配、不碰 RT 状态） |
//! | 块线程（RT） | Runtime::run_block | 从 SPSC 取包 → Aes67Input::feed_packet / fill_block → SlotEngine::run_block → 写后端 |
//! | 控制线程 | Runtime::poll_device / Runtime::status / Runtime::prepare_plan / Runtime::commit_plan | 设备生命周期、状态快照、方案解码/准入/编译（允许 IO 与堆分配） |
//!
//! # 为什么 SPSC 里搬的是原始 RTP 包，而不是解码后的块
//!
//! Aes67Input 是单所有者对象（feed_packet(&mut self) / fill_block(&mut self)），
//! 且两个方法都保证零分配、零锁、无 IO——收包与取块必须在同一个线程上下文里调用。
//! Runtime 持有 Aes67Input（任务要求），因此块线程负责 feed_packet + fill_block；
//! 收包线程只做网络 → 无锁环的搬运。
//!
//! 这样做的代价是 RT 线程要解析 RTP/L24；收益是不需要块级队列：音频时钟仍然是唯一的
//! 拉取者（块节奏由设备周期决定），不会出现收包线程按媒体速率产块、与设备时钟漂移导致的
//! 队列深度漂移。RT 线程每个块最多处理的包数被 SPSC 容量静态限定，因此解码工作量有界。
//!
//! # 实时约束
//!
//! Runtime::run_block 路径（含收包环出队、feed/fill、图处理、输出写）零分配、零锁、
//! 无日志、不 panic；所有缓冲（输入块、输出块、包缓冲、SPSC 槽位、S24_3LE 字节缓冲）都在
//! 构造期分配。错误经返回值/计数上报，不在 RT 路径构造带 String 的错误（后端故障的
//! BackendError::Fault(String) 是 alsa_backend 已声明的唯一分配例外，只发生在设备错误路径）。
//!
//! 控制面方法（Runtime::poll_device、Runtime::status、方案加载）不是 RT 安全：
//! DeviceSupervisor::poll 会做系统调用/外部命令，安全监督器构造故障文本会分配。调用方必须
//! 把它们放在控制上下文（独立线程或块循环之外），不要放进 RT 回调。
//!
//! # 设备掉线/缺失：fail-closed，不退出
//!
//! 声卡被拔掉/重新枚举（ALSA 写返回 `alsa pcm disconnected` 等）或启动时设备不在位时，
//! 生产宿主不应退出：Runtime 提供一条控制路径的生命周期：
//!
//! - 用 [`Runtime::new_deferred`] / [`Runtime::with_source_deferred`] 构造（**不**打开设备，
//!   收包线程/引擎/缓冲照常就位）；
//! - 设备未打开期间 [`Runtime::run_block`] 直接返回 [`BackendError::NotStarted`]：
//!   不推进输入/图状态、不写设备（fail-closed，进程保持运行）；
//! - 控制路径按退避调用 [`Runtime::open_device`] 重开；成功后**重建输入抖动缓冲**（抽干收包环
//!   里断线期间积压的旧包、重新分配 Aes67Input，预填从零开始），当前方案图/版本保持不动
//!   （不需要重新 apply_plan，version/graph_id 语义不变）；
//! - [`Runtime::clear_device_loss_fault`] 显式清除"设备丢失"造成的安全故障；处理器/欠载/
//!   换图等其它故障仍锁存（fail-closed 语义不变）；
//! - [`Runtime::close_device`] 释放旧句柄（设备已掉线时调用不会 panic）。
//!
//! 设备缺失永不导致退出；退避、日志限频与状态文件由宿主（rt-host）的监督循环负责。
//! 只有配置/方案准入类错误与无法恢复的内部错误才由宿主决定退出。
//!
//! # 未实现 / 边界（如实声明）
//!
//! - PTP / 媒体时钟：Aes67Input 的边界就是不碰 PTP；本模块同样不做首块延迟与时钟锚定。
//! - SO_RCVBUF：std::net::UdpSocket 没有 setsockopt。Linux 上用平台 FFI 做 best-effort
//!   设置（ReceiverStats::rcvbuf_applied 如实上报是否成功），其它平台恒为 false；
//!   不为此引入新依赖。
//! - 设备监督器的流控制：生产装配里 DeviceSupervisor 的 StreamControl 是 NullStream
//!   （只验证状态机与速率，不真正开关音频流）；ALSA PCM 由 Runtime::new 在构造期打开，
//!   设备缺失/掉线场景用 Runtime::new_deferred + Runtime::open_device 由宿主重试
//!   （见"设备掉线/缺失"一节）。把 Streaming 状态与后端 start/stop 联动留给 daemon 层
//!   （需要共享后端所有权，超出本任务）。
//! - 冗余流 / 丢包补偿 / RTCP：单流、无重传（与 aes67_input 的边界一致）。

use std::fmt;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use khaen_core::plan::{Plan, PlanError};

use crate::admission::{Admission, AdmissionLimits};
use crate::aes67_bus::{Aes67InputBus, Aes67InputBusStats};
use crate::aes67_input::{
    Aes67Input, Aes67InputConfig, Aes67InputStats, FeedOutcome, InputError, L24_SAMPLE_BYTES,
};
use crate::alsa_backend::{AlsaOutputBackend, AlsaStats, PcmOutput};
use crate::backend::{AudioBackend, BackendError, DeviceConfig};
use crate::capture::{
    CaptureConfig, CaptureError, CaptureFillStatus, CaptureInput, CaptureStats, PcmCapture,
};
use crate::engine::{
    EngineConfig, EngineError, GraphInfo, MuteReceipt, Passthrough, Processor, Silent, SlotEngine,
};
use crate::graph::{CompileError, PlanGraph};
use crate::output_stage::{OutputStage, OutputStageStatus};
use crate::ring::{self, Consumer, Producer};
use crate::safety::{FaultReason, RuntimeState, SafetyPolicy, SafetySupervisor};
use crate::supervisor::{
    ClockSource, CommandRateControl, DeviceProbe, DeviceSupervisor, LifecycleState, MonotonicClock,
    NullStream, RateControl, StreamControl, SupervisorPolicy, SupervisorStatus, SysfsProbe,
};

// ===========================================================================
// 常量与稳定错误码
// ===========================================================================

/// 收包 SPSC 的默认容量（包槽数）。
pub const DEFAULT_PENDING_PACKETS: usize = 64;
/// 单个 UDP 载荷槽的默认字节数（覆盖 AES67 常见包长：8ch × 48 帧 = 1164B + 12B 头）。
pub const DEFAULT_PACKET_BYTES: usize = 2048;
/// 默认 socket 读超时（毫秒）；决定 Aes67Receiver::stop() 的最坏 join 延迟。
pub const DEFAULT_RECV_TIMEOUT_MS: u64 = 100;
/// 单个 UDP 载荷槽的编译期上限（槽位是定长数组，避免 RT 路径分配/回收堆内存）。
pub const MAX_PACKET_BYTES: usize = 2048;
/// 收包 SPSC 容量上限（防止配置把内存撑爆）。
pub const MAX_PENDING_PACKETS: usize = 1024;
/// RTP 基础头字节数（校验一个载荷至少能装一帧时使用）。
pub const RTP_HEADER_BYTES: usize = 12;

/// 运行时稳定错误码（固定字符串；状态/日志/协议里直接引用，不随实现变化）。
pub mod codes {
    /// 运行时配置自相矛盾（块长/速率/通道/收包参数）。
    pub const CONFIG: &str = "ERR_RUNTIME_CONFIG";
    /// 块引擎错误。
    pub const ENGINE: &str = "ERR_RUNTIME_ENGINE";
    /// 输出后端错误。
    pub const BACKEND: &str = "ERR_RUNTIME_BACKEND";
    /// 输入连续欠载达到安全阈值（输出已安全静音）。
    pub const UNDERRUN: &str = "ERR_RUNTIME_INPUT_UNDERRUN";
    /// 输入连续溢出达到安全阈值。
    pub const OVERRUN: &str = "ERR_RUNTIME_INPUT_OVERRUN";
    /// 多流输入总线配置错误。
    pub const INPUT_BUS: &str = "ERR_RUNTIME_INPUT_BUS";
    /// ALSA 采集输入错误。
    pub const CAPTURE: &str = "ERR_RUNTIME_CAPTURE";
    /// 其它内部故障。
    pub const INTERNAL: &str = "ERR_RUNTIME_INTERNAL";
    /// UDP socket 绑定失败。
    pub const RECEIVER_BIND: &str = "ERR_AES67_RECEIVER_BIND";
    /// 组播 join 失败。
    pub const RECEIVER_JOIN: &str = "ERR_AES67_RECEIVER_JOIN";
    /// socket 配置（读超时等）失败。
    pub const RECEIVER_SOCKET: &str = "ERR_AES67_RECEIVER_SOCKET";
    /// 收包线程创建失败。
    pub const RECEIVER_THREAD_SPAWN: &str = "ERR_AES67_RECEIVER_THREAD_SPAWN";
    /// 收包线程 panic（join 失败）。
    pub const RECEIVER_THREAD_PANIC: &str = "ERR_AES67_RECEIVER_THREAD_PANIC";
    /// 热重配被拒绝：目标配置非法（未触碰任何句柄）。
    pub const RECONFIGURE_CONFIG: &str = "ERR_RECONFIGURE_CONFIG";
    /// 热重配失败：收包侧重建失败（socket bind/join/线程）。
    pub const RECONFIGURE_RECEIVER: &str = "ERR_RECONFIGURE_RECEIVER";
    /// 热重配失败：新输出设备打不开。
    pub const RECONFIGURE_DEVICE: &str = "ERR_RECONFIGURE_DEVICE";
    /// 热重配失败：新 hw params 回读与目标不一致（或读不到）。
    pub const RECONFIGURE_VERIFY: &str = "ERR_RECONFIGURE_HW_VERIFY";
    /// 热重配失败且旧配置也恢复不了（设备/收包至少一项没回到旧状态）。
    pub const RECONFIGURE_ROLLBACK: &str = "ERR_RECONFIGURE_ROLLBACK";
}

// ===========================================================================
// 错误
// ===========================================================================

/// 收包侧错误：稳定错误码 + 人看的说明（构造于非实时路径）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiverError {
    /// 稳定错误码（见 codes）。
    pub code: &'static str,
    /// 具体说明。
    pub message: String,
}

impl ReceiverError {
    /// 新建。
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for ReceiverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ReceiverError {}

/// 运行时错误（构造/块路径/方案加载共用）。
///
/// code() 给稳定错误码；Display 给人看。RT 路径只会返回 Engine 或 Backend 变体
/// （且仅在后端已声明可分配的故障路径），正常块不产生错误对象。
#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeError {
    /// 运行时配置非法或三段配置互相矛盾。
    Config(&'static str),
    /// AES67 输入配置/新建失败。
    Input(InputError),
    /// 多流 AES67 输入总线配置错误。
    Bus(crate::aes67_bus::BusError),
    /// ALSA 采集输入错误（配置/打开/线程/设备故障）。
    Capture(CaptureError),
    /// 块引擎错误。
    Engine(EngineError),
    /// 输出后端错误。
    Backend(BackendError),
    /// 收包 socket/线程错误。
    Receiver(ReceiverError),
    /// 方案文件解码失败（ABI v3 fail-closed）。
    Plan(PlanError),
    /// 方案图编译失败（准入拒绝单独展开，见 Admission 变体）。
    Compile(CompileError),
    /// 准入拒绝（单独展开，便于控制面原样上报 admission_rejected）。
    Admission(Admission),
    /// 方案草稿与当前执行器版本不匹配（过期草稿）。
    DraftInvalid(&'static str),
}

impl RuntimeError {
    /// 稳定错误码。
    pub fn code(&self) -> &'static str {
        match self {
            RuntimeError::Config(_) => codes::CONFIG,
            RuntimeError::Input(err) => err.name(),
            RuntimeError::Bus(_) => codes::INPUT_BUS,
            RuntimeError::Capture(err) => match err {
                CaptureError::BadConfig(_) => codes::CONFIG,
                _ => codes::CAPTURE,
            },
            RuntimeError::Engine(_) => codes::ENGINE,
            RuntimeError::Backend(_) => codes::BACKEND,
            RuntimeError::Receiver(err) => err.code,
            RuntimeError::Plan(_) => "ERR_RUNTIME_PLAN",
            RuntimeError::Compile(_) => "ERR_RUNTIME_PLAN_COMPILE",
            RuntimeError::Admission(_) => "admission_rejected",
            RuntimeError::DraftInvalid(_) => "engine_token_invalid",
        }
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeError::Config(message) => write!(f, "runtime config: {message}"),
            RuntimeError::Input(err) => write!(f, "aes67 input: {err}"),
            RuntimeError::Bus(err) => write!(f, "aes67 input bus: {err}"),
            RuntimeError::Capture(err) => write!(f, "alsa capture: {err}"),
            RuntimeError::Engine(err) => write!(f, "engine: {err}"),
            RuntimeError::Backend(err) => write!(f, "backend: {err}"),
            RuntimeError::Receiver(err) => write!(f, "receiver: {err}"),
            RuntimeError::Plan(err) => write!(f, "plan decode: {err}"),
            RuntimeError::Compile(err) => write!(f, "plan compile: {err}"),
            RuntimeError::Admission(admission) => {
                write!(f, "admission rejected: {}", admission.summary())
            }
            RuntimeError::DraftInvalid(message) => write!(f, "plan draft: {message}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<ReceiverError> for RuntimeError {
    fn from(value: ReceiverError) -> Self {
        RuntimeError::Receiver(value)
    }
}

// ===========================================================================
// 收包配置 / 包源
// ===========================================================================

/// 收包侧配置（构造期固定）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiverConfig {
    /// SPSC 容量（包槽数，向上取整到 2 的幂）；收包过快时满队列丢包并计数。
    pub pending_packets: usize,
    /// 单包最大字节数（≤ MAX_PACKET_BYTES）；超过的 datagram 会被截断（RTP 解析计 malformed）。
    pub packet_bytes: usize,
    /// socket 读超时（毫秒，≥1）。决定 stop() 的最坏 join 延迟。
    pub recv_timeout_ms: u64,
    /// 组播 join 的接口地址；None = INADDR_ANY（由系统选默认接口）。
    pub interface: Option<Ipv4Addr>,
    /// 请求的 SO_RCVBUF 字节数（best-effort：Linux 平台 FFI 设置，其它平台不生效）。
    pub rcvbuf_bytes: Option<usize>,
}

impl Default for ReceiverConfig {
    fn default() -> Self {
        Self {
            pending_packets: DEFAULT_PENDING_PACKETS,
            packet_bytes: DEFAULT_PACKET_BYTES,
            recv_timeout_ms: DEFAULT_RECV_TIMEOUT_MS,
            interface: None,
            rcvbuf_bytes: None,
        }
    }
}

impl ReceiverConfig {
    /// 校验收包配置；错误信息点名字段（由调用方映射成稳定错误码）。
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.pending_packets == 0 || self.pending_packets > MAX_PENDING_PACKETS {
            return Err("receiver.pending_packets must be in 1..=MAX_PENDING_PACKETS");
        }
        if self.packet_bytes <= RTP_HEADER_BYTES || self.packet_bytes > MAX_PACKET_BYTES {
            return Err("receiver.packet_bytes must be in (RTP_HEADER_BYTES, MAX_PACKET_BYTES]");
        }
        if self.recv_timeout_ms == 0 || self.recv_timeout_ms > 60_000 {
            return Err("receiver.recv_timeout_ms must be in 1..=60000");
        }
        Ok(())
    }
}

/// 一个可注入的 UDP 包源。
///
/// 契约：
/// - recv 返回一个完整 datagram（RTP 包）的字节数，写入 buf[..n]；
/// - 空闲时必须在有限时间内返回 WouldBlock/TimedOut（收包线程靠这个检查停止标志，
///   永远阻塞的实现在 stop() 时无法 join）；
/// - 实现方在接收线程上运行，允许加锁/分配（不是 RT 路径）。
pub trait PacketSource: Send {
    /// 收一个 datagram；Ok(0) 表示一个空 datagram（不是无数据）。
    fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize>;

    /// 本端地址（诊断用；未知返回 None）。
    fn local_addr(&self) -> Option<SocketAddr> {
        None
    }

    /// SO_RCVBUF 是否真的设置成功（best-effort；默认 false）。
    fn rcvbuf_applied(&self) -> bool {
        false
    }
}

/// 采集模式下替代 UDP 的惰性包源：不占端口，按固定节拍返回 WouldBlock。
///
/// 采集模式不复用 AES67 收包路径，但 Runtime 的 receiver 字段仍是单所有者结构；
/// 用惰性源保持"收包线程可停止/统计可读"的形状，避免让接收器变成 Option 后
/// 在热重配/设备重开的每条路径上都多一层分支（采集模式的热重配本身 fail-closed）。
struct IdlePacketSource;

impl PacketSource for IdlePacketSource {
    fn recv(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        thread::sleep(Duration::from_millis(20));
        Err(io::Error::from(io::ErrorKind::WouldBlock))
    }
}

/// 真实 UDP 收包源：绑定组播端口 → join 组播组 → 设置读超时 →（可选）SO_RCVBUF。
#[derive(Debug)]
pub struct UdpPacketSource {
    socket: UdpSocket,
    rcvbuf_applied: bool,
}

impl UdpPacketSource {
    /// 按输入配置绑定端口并 join 组播组。
    ///
    /// 错误映射：bind → codes::RECEIVER_BIND，join → codes::RECEIVER_JOIN，
    /// set_read_timeout → codes::RECEIVER_SOCKET。错误码稳定，消息带 OS 原始错误。
    pub fn bind(
        input: &Aes67InputConfig,
        receiver: &ReceiverConfig,
    ) -> Result<Self, ReceiverError> {
        // IO I2b：组播 → bind 任意地址再 join 组；单播 → 直接 bind 本端目的地址，
        // 不 join（单播 SDP 的 c= 就是接收端本机地址）。两条路径都设置读超时/rcvbuf。
        let multicast = input.multicast_addr.is_multicast();
        let local = if multicast {
            SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, input.port)
        } else {
            SocketAddrV4::new(input.multicast_addr, input.port)
        };
        let socket = UdpSocket::bind(local).map_err(|err| {
            ReceiverError::new(
                codes::RECEIVER_BIND,
                format!("bind udp {local} failed: {err}"),
            )
        })?;
        let timeout = Duration::from_millis(receiver.recv_timeout_ms.max(1));
        socket.set_read_timeout(Some(timeout)).map_err(|err| {
            ReceiverError::new(
                codes::RECEIVER_SOCKET,
                format!("set udp read timeout {timeout:?} failed: {err}"),
            )
        })?;
        if multicast {
            let interface = receiver.interface.unwrap_or(Ipv4Addr::UNSPECIFIED);
            socket
                .join_multicast_v4(&input.multicast_addr, &interface)
                .map_err(|err| {
                    ReceiverError::new(
                        codes::RECEIVER_JOIN,
                        format!(
                            "join multicast group {} on interface {interface} failed: {err}",
                            input.multicast_addr
                        ),
                    )
                })?;
        }
        let rcvbuf_applied = receiver
            .rcvbuf_bytes
            .is_some_and(|bytes| apply_rcvbuf(&socket, bytes));
        Ok(Self {
            socket,
            rcvbuf_applied,
        })
    }

    /// 本端 socket 地址。
    pub fn socket_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }
}

impl PacketSource for UdpPacketSource {
    fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.socket.recv(buf)
    }

    fn local_addr(&self) -> Option<SocketAddr> {
        UdpSocket::local_addr(&self.socket).ok()
    }

    fn rcvbuf_applied(&self) -> bool {
        self.rcvbuf_applied
    }
}

/// best-effort 设置 SO_RCVBUF：Linux 用平台 FFI（std 没有 setsockopt），其它平台不生效。
#[cfg(target_os = "linux")]
fn apply_rcvbuf(socket: &UdpSocket, bytes: usize) -> bool {
    use std::os::fd::AsRawFd;

    // Linux <sys/socket.h> 的常量；只编译在 Linux 目标上（其它平台走下面的 no-op）。
    const SOL_SOCKET: i32 = 1;
    const SO_RCVBUF: i32 = 8;
    extern "C" {
        fn setsockopt(
            socket: i32,
            level: i32,
            option_name: i32,
            option_value: *const std::ffi::c_void,
            option_len: u32,
        ) -> i32;
    }

    let value = i32::try_from(bytes).unwrap_or(i32::MAX);
    // SAFETY: fd 来自仍然存活的 UdpSocket；option_value 指向一个存活的 i32，
    // option_len 是 Linux socklen_t（u32）下的 sizeof(i32)，与 setsockopt 的 C 约定一致。
    unsafe {
        setsockopt(
            socket.as_raw_fd(),
            SOL_SOCKET,
            SO_RCVBUF,
            (&value as *const i32).cast(),
            core::mem::size_of::<i32>() as u32,
        ) == 0
    }
}

#[cfg(not(target_os = "linux"))]
fn apply_rcvbuf(_socket: &UdpSocket, _bytes: usize) -> bool {
    false
}

// ===========================================================================
// 收包线程
// ===========================================================================

/// 收包线程累计统计（原子快照，不阻塞音频线程）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReceiverStats {
    /// 成功推进 SPSC 的 datagram 数（空 datagram 也计）。
    pub packets_received: u64,
    /// 累计字节数。
    pub bytes_received: u64,
    /// SPSC 满导致的丢包数（收包过快/块线程停顿）。
    pub queue_full_drops: u64,
    /// recv 的系统错误数（不含正常的 WouldBlock/TimedOut）。
    pub recv_errors: u64,
    /// SO_RCVBUF 是否设置成功（best-effort；非 Linux 恒为 false）。
    pub rcvbuf_applied: bool,
    /// 收包线程是否在跑（stop() 之后为 false）。
    pub running: bool,
}

/// SPSC 槽位：定长字节数组 + 长度。
///
/// 定长是为了让 push/pop 只做值移动：收包线程 push 不分配，块线程 pop 不释放。
/// 槽位上限 MAX_PACKET_BYTES，由配置校验保证。
struct PacketSlot {
    len: u16,
    bytes: [u8; MAX_PACKET_BYTES],
}

impl PacketSlot {
    fn new() -> Self {
        Self {
            len: 0,
            bytes: [0u8; MAX_PACKET_BYTES],
        }
    }
}

/// 收包线程计数器。
#[derive(Debug, Default)]
struct ReceiverCounters {
    packets_received: AtomicU64,
    bytes_received: AtomicU64,
    queue_full_drops: AtomicU64,
    recv_errors: AtomicU64,
}

/// AES67 UDP 收包线程：socket → SPSC。
///
/// 生命周期：Aes67Receiver::spawn 启动线程，Aes67Receiver::stop 置停止标志并 join；
/// Drop 时会自动 stop（析构不 panic）。线程只把完整 datagram 拷进 SPSC 槽位，不解析 RTP、
/// 不分配内存、不碰 Aes67Input（单所有者留在调用方/RT 侧）。
pub struct Aes67Receiver {
    stop: Arc<AtomicBool>,
    counters: Arc<ReceiverCounters>,
    consumer: Consumer<PacketSlot>,
    handle: Option<JoinHandle<()>>,
    local_addr: Option<SocketAddr>,
    rcvbuf_applied: bool,
}

impl Aes67Receiver {
    /// 启动收包线程，返回接收器（SPSC 消费端在它内部，用 next_packet 取包）。
    ///
    /// source 必须满足 PacketSource 的空闲时有限时间返回契约，否则 stop() 无法 join。
    pub fn spawn<S: PacketSource + 'static>(
        source: S,
        config: &ReceiverConfig,
    ) -> Result<Self, RuntimeError> {
        config.validate().map_err(RuntimeError::Config)?;
        let (producer, consumer) = ring::channel::<PacketSlot>(config.pending_packets);
        let local_addr = source.local_addr();
        let rcvbuf_applied = source.rcvbuf_applied();
        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(ReceiverCounters::default());
        let thread_stop = Arc::clone(&stop);
        let thread_counters = Arc::clone(&counters);
        let packet_bytes = config.packet_bytes;
        let handle = thread::Builder::new()
            .name(String::from("khaen-aes67-rx"))
            .spawn(move || {
                receiver_loop(source, producer, thread_stop, thread_counters, packet_bytes)
            })
            .map_err(|err| {
                RuntimeError::Receiver(ReceiverError::new(
                    codes::RECEIVER_THREAD_SPAWN,
                    format!("spawn AES67 receiver thread failed: {err}"),
                ))
            })?;
        Ok(Self {
            stop,
            counters,
            consumer,
            handle: Some(handle),
            local_addr,
            rcvbuf_applied,
        })
    }

    /// 块线程（RT 安全）取一个包到调用方预分配缓冲；无包返回 None。
    ///
    /// 零分配、无锁、不阻塞：最多把槽位里的字节拷进 buf。buf 小于包长时截断
    /// （RTP 解析会按 malformed 计数，不会 panic）。
    pub fn next_packet(&mut self, buf: &mut [u8]) -> Option<usize> {
        let slot = self.consumer.pop()?;
        let len = usize::from(slot.len).min(buf.len());
        buf[..len].copy_from_slice(&slot.bytes[..len]);
        Some(len)
    }

    /// 统计快照。
    pub fn stats(&self) -> ReceiverStats {
        ReceiverStats {
            packets_received: self.counters.packets_received.load(Ordering::Relaxed),
            bytes_received: self.counters.bytes_received.load(Ordering::Relaxed),
            queue_full_drops: self.counters.queue_full_drops.load(Ordering::Relaxed),
            recv_errors: self.counters.recv_errors.load(Ordering::Relaxed),
            rcvbuf_applied: self.rcvbuf_applied,
            running: self.handle.is_some() && !self.stop.load(Ordering::Acquire),
        }
    }

    /// 收包线程是否在跑。
    pub fn is_running(&self) -> bool {
        self.stats().running
    }

    /// 本端地址（未知为 None）。
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.local_addr
    }

    /// 停止收包线程并 join；幂等。返回停止后的统计快照。
    ///
    /// 最坏阻塞时间 ≈ 一次 recv 超时（ReceiverConfig::recv_timeout_ms）。
    pub fn stop(&mut self) -> Result<ReceiverStats, RuntimeError> {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            handle.join().map_err(|_| {
                RuntimeError::Receiver(ReceiverError::new(
                    codes::RECEIVER_THREAD_PANIC,
                    "AES67 receiver thread panicked",
                ))
            })?;
        }
        Ok(self.stats())
    }
}

impl Drop for Aes67Receiver {
    fn drop(&mut self) {
        // 析构不 panic：join 失败只意味着线程已经 panic，这里没有可上报的通道。
        let _ = self.stop();
    }
}

// ===========================================================================
// 收包源工厂（热重配用）：重建收包线程需要有"如何再造一个同款 PacketSource"的能力
// ===========================================================================

/// 收包源工厂类型：给新的 `Aes67InputConfig` + `ReceiverConfig` 造一个新的包源。
///
/// 生产装配（[`Runtime::new`] / [`Runtime::new_deferred`]）用真实 UDP 绑定；测试注入
/// 的假包源没有"克隆自己"的能力，因此它们用 [`Runtime::with_source`] 构造（工厂为
/// `None`，热重配若需要重绑收包会如实拒绝，不假装重建）。
pub type SourceFactory = Box<
    dyn Fn(
            &Aes67InputConfig,
            &ReceiverConfig,
        ) -> Result<Box<dyn PacketSource + Send>, ReceiverError>
        + Send,
>;

/// 把 `Box<dyn PacketSource>` 适配成可交给 [`Aes67Receiver::spawn`] 的具体类型。
struct BoxedSource(Box<dyn PacketSource + Send>);

impl PacketSource for BoxedSource {
    fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.recv(buf)
    }

    fn local_addr(&self) -> Option<SocketAddr> {
        self.0.local_addr()
    }

    fn rcvbuf_applied(&self) -> bool {
        self.0.rcvbuf_applied()
    }
}

/// 启动装配：采集模式用惰性包源占位（不占 UDP 端口），否则按每路 AES67 流绑定 UDP。
fn startup_sources(
    config: &RuntimeConfig,
) -> Result<Vec<Box<dyn PacketSource + Send>>, RuntimeError> {
    if config.capture.is_some() {
        return Ok(vec![Box::new(IdlePacketSource)]);
    }
    let mut sources: Vec<Box<dyn PacketSource + Send>> = Vec::new();
    for input in std::iter::once(&config.input).chain(config.additional_inputs.iter()) {
        sources.push(Box::new(UdpPacketSource::bind(input, &config.receiver)?));
    }
    Ok(sources)
}

/// 打开真实 ALSA 采集源并启动采集线程（仅 Linux；其它平台 fail-closed）。
#[cfg(target_os = "linux")]
fn open_capture(config: &CaptureConfig) -> Result<CaptureInput, RuntimeError> {
    let source = crate::capture::AlsaCapture::open(config).map_err(RuntimeError::Capture)?;
    CaptureInput::spawn_boxed(Box::new(source), config.clone()).map_err(RuntimeError::Capture)
}

/// 非 Linux：没有真实 ALSA 采集路径，如实 fail-closed（测试用 with_capture_source 注入）。
#[cfg(not(target_os = "linux"))]
fn open_capture(_config: &CaptureConfig) -> Result<CaptureInput, RuntimeError> {
    Err(RuntimeError::Capture(CaptureError::Open(String::from(
        "ALSA capture input is only supported on Linux; inject a capture source in tests",
    ))))
}

/// 生产收包源工厂：真实 UDP 绑定（bind + join 组播 + 读超时）。
fn udp_source_factory() -> SourceFactory {
    Box::new(|input: &Aes67InputConfig, receiver: &ReceiverConfig| {
        let source = UdpPacketSource::bind(input, receiver)?;
        Ok(Box::new(source) as Box<dyn PacketSource + Send>)
    })
}

/// 收包线程主循环：读一个 datagram → 拷进 SPSC 槽位 → 满则计数丢弃。
fn receiver_loop<S: PacketSource>(
    mut source: S,
    producer: Producer<PacketSlot>,
    stop: Arc<AtomicBool>,
    counters: Arc<ReceiverCounters>,
    packet_bytes: usize,
) {
    let recv_len = packet_bytes.min(MAX_PACKET_BYTES);
    while !stop.load(Ordering::Acquire) {
        let mut slot = PacketSlot::new();
        match source.recv(&mut slot.bytes[..recv_len]) {
            Ok(len) => {
                let len = len.min(recv_len);
                slot.len = len as u16;
                if producer.push(slot) {
                    // 先 push 再计数：块线程/测试看到计数时槽位已经可见。
                    counters.packets_received.fetch_add(1, Ordering::Relaxed);
                    counters
                        .bytes_received
                        .fetch_add(u64::try_from(len).unwrap_or(0), Ordering::Relaxed);
                } else {
                    counters.queue_full_drops.fetch_add(1, Ordering::Relaxed);
                }
            }
            Err(err) => match err.kind() {
                io::ErrorKind::WouldBlock
                | io::ErrorKind::TimedOut
                | io::ErrorKind::Interrupted => {}
                _ => {
                    counters.recv_errors.fetch_add(1, Ordering::Relaxed);
                    // 持续错误（网卡消失等）时不要忙等烧 CPU；停止标志在下一轮检查。
                    thread::sleep(Duration::from_millis(1));
                }
            },
        }
    }
}

// ===========================================================================
// 运行时配置
// ===========================================================================

/// 未加载方案时的初始处理链策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnloadedPolicy {
    /// 直通（默认）：还没有方案时输出 = 输入，便于联调设备链路。
    #[default]
    Passthrough,
    /// 静音：还没有方案时输出整块静音（fail-closed 部署用）。
    Silence,
}

/// 生产设备监督器的装配参数。
///
/// 默认 enabled = false：没有配置速率控制命令时不去猜能控制什么设备（能力上报真实）。
/// 打开后 Runtime::new 装配 SysfsProbe + CommandRateControl + NullStream + MonotonicClock。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SupervisorOptions {
    /// 是否装配真实设备监督器（false 时用 NoDeviceDriver，状态恒为 Absent）。
    pub enabled: bool,
    /// 方案速率；0 = 取 engine.sample_rate（推荐，避免两处配置漂移）。
    pub plan_rate: u32,
    /// 状态机策略（重试/退避/稳定回读次数）。
    pub policy: SupervisorPolicy,
    /// sysfs 卡名匹配串（None = 用 DeviceConfig::device 名字面量）。
    pub card_match: Option<String>,
    /// 控制通道拉起命令（等价 v1 的 motu-net-up）；None = 只读检查 ALSA 控制节点。
    pub control_argv: Option<Vec<String>>,
    /// 读设备速率的命令 argv（必须打印一个整数）。
    pub rate_read_argv: Vec<String>,
    /// 写设备速率的命令 argv（{rate} 占位符；缺省时速率作为最后一个参数追加）。
    pub rate_assert_argv: Vec<String>,
}

/// 单进程运行时配置（构造期固定；运行期不改）。
///
/// block_frames 由三段配置共同表达：engine.block_frames 是唯一来源，
/// input.block_frames / device.block_frames 必须与它一致（RuntimeConfig::validate 检查），
/// 这样不存在块长三处各说各话的可能。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    /// 主 AES67 输入配置（组播地址/端口/通道映射/块长）。
    pub input: Aes67InputConfig,
    /// 额外 AES67 流（多流稀疏混入同一条输入总线；空 = 单流旧行为）。
    pub additional_inputs: Vec<Aes67InputConfig>,
    /// ALSA 采集输入（Some = 用本机 PCM 采集替代 AES67 收包；IO I4）。
    /// 与 `additional_inputs` 互斥；宽度/块长/速率必须与 engine 一致。
    pub capture: Option<CaptureConfig>,
    /// 块引擎配置（块长/速率/通道）。
    pub engine: EngineConfig,
    /// 输出设备配置（设备名/速率/块长/通道）。
    pub device: DeviceConfig,
    /// 安全策略（连续欠载/溢出阈值）。
    pub safety: SafetyPolicy,
    /// 收包配置（SPSC 容量/包长/读超时/接口）。
    pub receiver: ReceiverConfig,
    /// 设备监督器装配参数。
    pub supervisor: SupervisorOptions,
    /// 未加载方案时的初始处理链策略。
    pub unloaded_policy: UnloadedPolicy,
}

impl RuntimeConfig {
    /// 校验配置：三段块长/速率/通道必须一致，收包槽位必须装得下一帧 L24。
    pub fn validate(&self) -> Result<(), RuntimeError> {
        self.input.validate().map_err(RuntimeError::Input)?;
        self.engine.validate().map_err(RuntimeError::Engine)?;
        if self.additional_inputs.len() > crate::aes67_bus::MAX_BUS_STREAMS - 1 {
            return Err(RuntimeError::Config(
                "additional_inputs exceeds the input bus stream limit",
            ));
        }
        for stream in &self.additional_inputs {
            stream.validate().map_err(RuntimeError::Input)?;
            if stream.output_channels != self.input.output_channels
                || stream.block_frames != self.input.block_frames
                || stream.sample_rate != self.input.sample_rate
            {
                return Err(RuntimeError::Config(
                    "additional_inputs must match the primary input's output_channels/block_frames/sample_rate",
                ));
            }
        }
        if let Some(capture) = &self.capture {
            capture.validate().map_err(RuntimeError::Capture)?;
            if !self.additional_inputs.is_empty() {
                return Err(RuntimeError::Config(
                    "capture input and additional AES67 streams are mutually exclusive",
                ));
            }
            if capture.input_channels != self.engine.input_channels
                || capture.block_frames != self.engine.block_frames
                || capture.sample_rate != self.engine.sample_rate
            {
                return Err(RuntimeError::Config(
                    "capture input width/block/rate must match engine input_channels/block_frames/sample_rate",
                ));
            }
        }
        self.receiver.validate().map_err(RuntimeError::Config)?;
        if self.device.device.as_str().is_empty() {
            return Err(RuntimeError::Config("device name must not be empty"));
        }
        if self.input.block_frames != self.engine.block_frames
            || self.device.block_frames != self.engine.block_frames
        {
            return Err(RuntimeError::Config(
                "block_frames must match across input, engine and device config",
            ));
        }
        if self.input.sample_rate != self.engine.sample_rate
            || self.device.sample_rate != self.engine.sample_rate
        {
            return Err(RuntimeError::Config(
                "sample_rate must match across input, engine and device config",
            ));
        }
        if self.input.output_channels != self.engine.input_channels
            || self.device.input_channels != self.engine.input_channels
        {
            return Err(RuntimeError::Config(
                "input.output_channels, device.input_channels and engine.input_channels must match",
            ));
        }
        if self.device.output_channels != self.engine.output_channels {
            return Err(RuntimeError::Config(
                "device.output_channels must match engine.output_channels",
            ));
        }
        let one_frame_bytes = self
            .input
            .channels
            .checked_mul(L24_SAMPLE_BYTES)
            .and_then(|bytes| bytes.checked_add(RTP_HEADER_BYTES))
            .ok_or(RuntimeError::Config("input channels overflow packet size"))?;
        if self.receiver.packet_bytes < one_frame_bytes {
            return Err(RuntimeError::Config(
                "receiver.packet_bytes cannot hold one AES67 L24 frame",
            ));
        }
        if self.supervisor.enabled
            && (self.supervisor.rate_read_argv.is_empty()
                || self.supervisor.rate_assert_argv.is_empty())
        {
            return Err(RuntimeError::Config(
                "enabled device supervisor requires rate_read_argv and rate_assert_argv",
            ));
        }
        let plan_rate = self.effective_plan_rate();
        if plan_rate == 0 {
            return Err(RuntimeError::Config("plan rate must be positive"));
        }
        Ok(())
    }

    /// 生效方案速率：显式 supervisor.plan_rate，或回落到 engine.sample_rate。
    pub fn effective_plan_rate(&self) -> u32 {
        if self.supervisor.plan_rate == 0 {
            self.engine.sample_rate
        } else {
            self.supervisor.plan_rate
        }
    }
}

// ===========================================================================
// 设备监督器接线
// ===========================================================================

/// 设备生命周期驱动接口：把 DeviceSupervisor 的类型参数从 Runtime 里擦掉。
///
/// 生产装配用 DeviceSupervisor（SysfsProbe + CommandRateControl）；测试用假
/// probe/rate/stream 实现同一状态机后装箱注入。
pub trait DeviceDriver: Send {
    /// 推进一步状态机，并把设备故障上报给安全监督器。
    fn poll(&mut self, safety: &mut SafetySupervisor) -> SupervisorStatus;

    /// 当前快照（不推进状态机；可能克隆错误文本）。
    fn status(&self) -> SupervisorStatus;

    /// 显式请求重试（清除锁存/退避）。
    fn request_retry(&mut self) {}
}

impl<P, R, S, C> DeviceDriver for DeviceSupervisor<P, R, S, C>
where
    P: DeviceProbe + Send,
    R: RateControl + Send,
    S: StreamControl + Send,
    C: ClockSource + Send,
{
    fn poll(&mut self, safety: &mut SafetySupervisor) -> SupervisorStatus {
        DeviceSupervisor::poll(self, safety)
    }

    fn status(&self) -> SupervisorStatus {
        DeviceSupervisor::status(self)
    }

    fn request_retry(&mut self) {
        DeviceSupervisor::request_retry(self)
    }
}

/// 未接设备监督器时的驱动：状态恒为 Absent，poll 不改变安全状态。
///
/// 用于 supervisor.enabled = false 的装配（例如只跑块通路的联调/测试）。
/// 如实上报设备监督未接线，不假装 Streaming。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoDeviceDriver {
    plan_rate: u32,
}

impl NoDeviceDriver {
    /// 新建；plan_rate 只用于状态字段（应为生效方案速率）。
    pub fn new(plan_rate: u32) -> Self {
        Self { plan_rate }
    }
}

impl DeviceDriver for NoDeviceDriver {
    fn poll(&mut self, _safety: &mut SafetySupervisor) -> SupervisorStatus {
        self.status()
    }

    fn status(&self) -> SupervisorStatus {
        SupervisorStatus {
            state: LifecycleState::Absent,
            plan_rate: self.plan_rate,
            device_rate: None,
            rate_asserted: false,
            last_error: None,
            attempts: 0,
            since_ms: 0,
        }
    }
}

/// 未加载方案时的初始处理链（热重配/构造共用；返回 Send 的处理链）。
fn unloaded_processor(policy: UnloadedPolicy) -> Box<dyn Processor + Send> {
    match policy {
        UnloadedPolicy::Passthrough => Box::new(Passthrough),
        UnloadedPolicy::Silence => Box::new(Silent),
    }
}

/// 按配置装配生产设备监督器。
fn build_device_driver(config: &RuntimeConfig) -> Result<Box<dyn DeviceDriver>, RuntimeError> {
    let plan_rate = config.effective_plan_rate();
    if !config.supervisor.enabled {
        return Ok(Box::new(NoDeviceDriver::new(plan_rate)));
    }
    let mut probe = SysfsProbe::new(config.device.device.as_str());
    if let Some(card_match) = &config.supervisor.card_match {
        probe = probe.with_card_match(card_match.clone());
    }
    if let Some(control_argv) = &config.supervisor.control_argv {
        probe = probe.with_control_command(control_argv.clone());
    }
    let rate = CommandRateControl::new(
        config.supervisor.rate_read_argv.clone(),
        config.supervisor.rate_assert_argv.clone(),
    );
    let supervisor = DeviceSupervisor::with_components(
        plan_rate,
        config.supervisor.policy.clone(),
        probe,
        rate,
        NullStream,
        MonotonicClock::new(),
    );
    Ok(Box::new(supervisor))
}

// ===========================================================================
// 方案草稿
// ===========================================================================

/// prepare_plan 产出的方案草稿：已解码、已准入、已编译成可执行图。
///
/// 只存在于控制线程：commit_plan 把图移交给 SlotEngine（下一块边界生效），
/// 直接丢弃等价于 abort（不改变正在跑的音频）。
pub struct PlanDraft {
    version: u64,
    graph_id: String,
    latency_samples: u64,
    processor: Box<dyn Processor + Send>,
}

impl PlanDraft {
    /// 方案版本号（commit 生效后成为 actual_version）。
    pub fn version(&self) -> u64 {
        self.version
    }

    /// 方案 graph_id（ABI v3 的 sha256 十六进制串）。
    pub fn graph_id(&self) -> &str {
        &self.graph_id
    }

    /// 图估算的 matrix 段延迟（全采样率样本；与 PlanGraph::estimated_latency_samples 一致）。
    pub fn latency_samples(&self) -> u64 {
        self.latency_samples
    }

    /// 拆出草稿部件（版本号、graph_id、处理链）。
    ///
    /// 跨进程控制通道用：控制线程编译草稿，块线程把处理链交给自己的 SlotEngine
    /// （见 Runtime::stage_plan_draft）。普通单进程路径用 Runtime::commit_plan，
    /// 它会额外校验草稿版本，不需要拆件。
    pub fn into_parts(self) -> (u64, String, Box<dyn Processor + Send>) {
        (self.version, self.graph_id, self.processor)
    }
}

/// 在控制线程编译一份方案草稿（不持有 Runtime、不碰执行器）：准入 + 图编译。
///
/// 这是跨进程控制通道（rt-host）的 prepare 基元：调用方已读文件、校验 sha256 并用
/// Plan::decode 解码出方案，这里只做准入与图编译；版本号由控制面单调分配后传入。
/// 错误映射与 Runtime::prepare_plan 一致：准入拒绝是 RuntimeError::Admission，
/// 结构非法/与运行时格式不匹配是 RuntimeError::Compile。不改变任何正在跑的音频。
pub fn compile_plan_draft(
    plan: &Plan,
    engine_config: &EngineConfig,
    limits: &AdmissionLimits,
    version: u64,
) -> Result<PlanDraft, RuntimeError> {
    let graph = match PlanGraph::compile(plan, engine_config, limits) {
        Ok(graph) => graph,
        Err(CompileError::AdmissionRejected(admission)) => {
            return Err(RuntimeError::Admission(admission));
        }
        Err(err) => return Err(RuntimeError::Compile(err)),
    };
    Ok(PlanDraft {
        version,
        graph_id: plan.graph_id_hex(),
        latency_samples: graph.estimated_latency_samples(),
        processor: Box::new(graph),
    })
}

impl fmt::Debug for PlanDraft {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlanDraft")
            .field("version", &self.version)
            .field("graph_id", &self.graph_id)
            .field("latency_samples", &self.latency_samples)
            .finish_non_exhaustive()
    }
}

// ===========================================================================
// 状态快照
// ===========================================================================

/// 运行时状态快照（给 daemon status 用；只在非实时路径构造）。
///
/// state 是安全监督器状态（Idle/Running/Muted/Faulted）；device 是设备监督器完整快照；
/// device_rate / plan_rate / rate_asserted 是 device 里同名字段的镜像（方便上层直接读）。
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeStatus {
    /// 安全监督器状态（是否需要静音以它为准）。
    pub state: RuntimeState,
    /// 已处理的块数（含欠载/安全静音块）。
    pub blocks: u64,
    /// 主流 AES67 输入统计快照（含 jitter 水位/SSRC；多流时等价于 input_streams[0]）。
    pub input: Aes67InputStats,
    /// 每路 AES67 输入统计快照（第一项主流；单流时长度 1；顺序与 RuntimeConfig 流表一致）。
    pub input_streams: Vec<Aes67InputStats>,
    /// 输入模式：`"aes67"` 或 `"alsa_capture"`。
    pub input_mode: &'static str,
    /// ALSA 采集统计快照（采集模式为 Some；AES67 模式为 None）。
    pub capture: Option<CaptureStats>,
    /// ALSA 输出统计快照。
    pub output: AlsaStats,
    /// 收包线程统计快照。
    pub receiver: ReceiverStats,
    /// 设备监督器完整快照。
    pub device: SupervisorStatus,
    /// 设备最近回读速率（镜像 device.device_rate）。
    pub device_rate: Option<u32>,
    /// 方案速率（镜像 device.plan_rate）。
    pub plan_rate: u32,
    /// 速率是否已断言并回读一致（镜像 device.rate_asserted）。
    pub rate_asserted: bool,
    /// 输出后端写失败（xrun 之外的终态故障）累计次数。
    pub output_errors: u64,
    /// 输出后端是否已打开（构造期打开，正常为 true）。
    pub output_started: bool,
    /// 当前生效图快照（未加载方案时各字段为 None）。
    pub graph: GraphInfo,
    /// 是否有已 stage、尚未在块边界生效的图切换。
    pub switch_pending: bool,
    /// 节目输出是否被软件静音（块边界后的值；校准旁路期间为 false）。
    pub muted: bool,
    /// 输出级完整状态：用户静音/校准旁路/硬静音计数/淡入进度。
    pub output_stage: OutputStageStatus,
    /// AES67/采集输入是否被暂停（校准期间自动停输入；暂停时块路径写静音且不计欠载）。
    pub input_paused: bool,
    /// 最近一次错误码：优先数据面（块路径）错误，否则设备监督器最近失败码；没有则 None。
    ///
    /// 数据面错误码见 codes；设备错误码见 SupervisorErrorCode::as_str
    /// （如 device_absent）。成功块不清数据面错误（最近一次错误语义）。
    pub last_error: Option<&'static str>,
}

// ===========================================================================
// 运行时
// ===========================================================================

/// 单进程运行时：AES67 收包线程 → SPSC → Aes67Input → SlotEngine → AlsaOutputBackend。
///
/// # 线程
///
/// - 收包线程由 Aes67Receiver 持有，Runtime 析构时自动 stop + join；
/// - run_block 由块线程（设备周期）调用；
/// - poll_device / status / 方案加载由控制线程调用（非 RT 安全）。
///
/// 泛型 P 是平台无关的 PCM 设备（Linux 用 AlsaPcm，测试用假设备），因此本模块在
/// Windows 上也能编译并用假设备跑完整测试。
pub struct Runtime<P: PcmOutput> {
    config: RuntimeConfig,
    input: Aes67InputBus,
    engine: SlotEngine,
    backend: AlsaOutputBackend<P>,
    safety: SafetySupervisor,
    driver: Box<dyn DeviceDriver>,
    receiver: Aes67Receiver,
    /// 额外流的收包线程（与 `config.additional_inputs` 同序；空 = 单流）。
    additional_receivers: Vec<Aes67Receiver>,
    /// ALSA 采集输入（Some = 采集模式；AES67 收包路径不参与输入）。
    capture: Option<CaptureInput>,
    /// 输入块（block_frames × output_channels），构造期分配。
    block: Vec<f32>,
    /// 输出块（block_frames × channels），构造期分配；含安全静音后的最终写设备内容。
    out_block: Vec<f32>,
    /// 单包缓冲（receiver.packet_bytes），构造期分配；块线程复用它接收 SPSC 包。
    packet_buf: Vec<u8>,
    blocks: u64,
    output_errors: u64,
    consecutive_underruns: u32,
    consecutive_overruns: u32,
    /// 待上报给安全监督器的故障（RT 路径只写这个 Copy 枚举，不构造 String）。
    pending_fault: Option<FaultReason>,
    /// 最近一次故障原因（留到控制路径清除，供"已恢复的欠载"判定；只写 Copy 枚举）。
    last_fault_reason: Option<FaultReason>,
    /// 设备丢失标记：块路径的 backend 写失败把它置位，控制路径恢复设备后显式清除
    /// （避免把欠载/处理器等其它锁存故障一起清掉）。
    device_fault: bool,
    last_error: Option<&'static str>,
    /// 进行中的校准播放（T70f-c；None = 播放模式）。
    calibration: Option<ActiveCalibration>,
    /// 收包源工厂（热重配/输入暂停恢复重建收包线程用；测试注入的假包源为 None）。
    source_factory: Option<SourceFactory>,
    /// 输入暂停标记：true 时块路径不抽包、不填输入、不计欠载，输出硬静音。
    input_paused: bool,
    /// 输出级：DSP 之后的软件静音/校准旁路/安全硬静音唯一真值源。
    output_stage: OutputStage,
}

impl<P: PcmOutput> fmt::Debug for Runtime<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Runtime")
            .field("config", &self.config)
            .field("blocks", &self.blocks)
            .field("output_errors", &self.output_errors)
            .field("pending_fault", &self.pending_fault)
            .field("device_started", &self.backend.is_started())
            .field("device_fault", &self.device_fault)
            .field("last_error", &self.last_error)
            .field("receiver", &self.receiver.stats())
            .field("capture", &self.capture.as_ref().map(CaptureInput::stats))
            .field("graph", &self.engine.graph_info())
            .finish_non_exhaustive()
    }
}

impl<P: PcmOutput> Runtime<P> {
    /// 构造并启动运行时：校验配置 → 打开输出设备 → 绑定组播并启动收包线程。
    ///
    /// 构造期完成全部堆分配与设备 IO；之后的 run_block 不再分配。
    /// 失败时已启动的资源由 Drop 释放（收包线程若已启动会被 join）。
    pub fn new(config: RuntimeConfig) -> Result<Self, RuntimeError> {
        config.validate()?;
        let sources = startup_sources(&config)?;
        let driver = build_device_driver(&config)?;
        Self::build_validated(
            config,
            sources,
            driver,
            true,
            Some(udp_source_factory()),
            None,
        )
    }

    /// 构造运行时但**不打开输出设备**：设备缺失/掉线由控制路径 [`Runtime::open_device`] 重试。
    ///
    /// 与 [`Runtime::new`] 的唯一区别是构造期不做 `backend.start`：收包线程、引擎与缓冲照常
    /// 就位，[`Runtime::run_block`] 在设备打开前 fail-closed 返回 NotStarted（不推进输入/图状态、
    /// 不写设备）。用于"声卡不在位也要保持进程运行"的宿主（rt-host）。
    pub fn new_deferred(config: RuntimeConfig) -> Result<Self, RuntimeError> {
        config.validate()?;
        let sources = startup_sources(&config)?;
        let driver = build_device_driver(&config)?;
        Self::build_validated(
            config,
            sources,
            driver,
            false,
            Some(udp_source_factory()),
            None,
        )
    }

    /// 用注入的多个包源构造运行时（测试/嵌入式接线用）：第一个是主流，其余与
    /// `config.additional_inputs` 同序；数量必须等于 1 + additional_inputs.len()。
    pub fn with_sources(
        config: RuntimeConfig,
        sources: Vec<Box<dyn PacketSource + Send>>,
    ) -> Result<Self, RuntimeError> {
        config.validate()?;
        if config.capture.is_some() {
            return Err(RuntimeError::Config(
                "with_sources cannot be used with capture input; use Runtime::with_capture_source",
            ));
        }
        let expected = 1 + config.additional_inputs.len();
        if sources.len() != expected {
            return Err(RuntimeError::Config(
                "with_sources needs one packet source per input stream (primary + additional_inputs)",
            ));
        }
        let driver = build_device_driver(&config)?;
        Self::build_validated(config, sources, driver, true, None, None)
    }

    /// 用注入的 PacketSource（假包源/其它 socket 实现）构造运行时。
    pub fn with_source<S: PacketSource + 'static>(
        config: RuntimeConfig,
        source: S,
    ) -> Result<Self, RuntimeError> {
        Self::build_validated_one(config, Box::new(source), true, None)
    }

    /// 注入假包源 + 收包源工厂（热重配测试用）：工厂负责按新 input/收包配置再造一个
    /// 假包源；`source` 是启动时的那一个。
    pub fn with_source_factory<S, F>(
        config: RuntimeConfig,
        source: S,
        factory: F,
    ) -> Result<Self, RuntimeError>
    where
        S: PacketSource + 'static,
        F: Fn(
                &Aes67InputConfig,
                &ReceiverConfig,
            ) -> Result<Box<dyn PacketSource + Send>, ReceiverError>
            + Send
            + 'static,
    {
        Self::build_validated_one(config, Box::new(source), true, Some(Box::new(factory)))
    }

    /// [`with_source_factory`] 的"设备延迟打开"版本（热重配 + 设备缺失测试用）。
    pub fn with_source_deferred_factory<S, F>(
        config: RuntimeConfig,
        source: S,
        factory: F,
    ) -> Result<Self, RuntimeError>
    where
        S: PacketSource + 'static,
        F: Fn(
                &Aes67InputConfig,
                &ReceiverConfig,
            ) -> Result<Box<dyn PacketSource + Send>, ReceiverError>
            + Send
            + 'static,
    {
        Self::build_validated_one(config, Box::new(source), false, Some(Box::new(factory)))
    }

    /// [`Runtime::with_source`] 的"设备延迟打开"版本：构造期不碰设备（设备缺失模拟/重开测试用）。
    pub fn with_source_deferred<S: PacketSource + 'static>(
        config: RuntimeConfig,
        source: S,
    ) -> Result<Self, RuntimeError> {
        Self::build_validated_one(config, Box::new(source), false, None)
    }

    /// 用注入的包源 + 设备驱动构造运行时（测试/daemon 注入用）。
    pub fn with_source_and_driver<S: PacketSource + 'static>(
        config: RuntimeConfig,
        source: S,
        driver: Box<dyn DeviceDriver>,
    ) -> Result<Self, RuntimeError> {
        let mut runtime = Self::build_validated_one(config, Box::new(source), true, None)?;
        runtime.driver = driver;
        Ok(runtime)
    }

    /// 用注入的采集源构造运行时（测试/嵌入式接线用；`config.capture` 必须为 Some）。
    ///
    /// 生产装配走 [`Runtime::new`]/[`Runtime::new_deferred`]：Linux 上打开真实 ALSA capture；
    /// 测试在本机注入假采集源，验证块路径/统计/停止语义。
    pub fn with_capture_source(
        config: RuntimeConfig,
        source: Box<dyn PcmCapture>,
    ) -> Result<Self, RuntimeError> {
        if config.capture.is_none() {
            return Err(RuntimeError::Config(
                "with_capture_source requires config.capture",
            ));
        }
        config.validate()?;
        let driver = build_device_driver(&config)?;
        Self::build_validated(
            config,
            vec![Box::new(IdlePacketSource)],
            driver,
            true,
            None,
            Some(source),
        )
    }

    /// 单包源注入的统一入口：`additional_inputs` 必须为空（注入构造函数只能给主流一个源）。
    fn build_validated_one(
        config: RuntimeConfig,
        source: Box<dyn PacketSource + Send>,
        open_device: bool,
        source_factory: Option<SourceFactory>,
    ) -> Result<Self, RuntimeError> {
        config.validate()?;
        if !config.additional_inputs.is_empty() {
            return Err(RuntimeError::Config(
                "injected-source constructors cannot supply additional AES67 streams; use Runtime::new or Runtime::with_sources",
            ));
        }
        if config.capture.is_some() {
            return Err(RuntimeError::Config(
                "injected AES67 sources cannot be combined with capture input; use Runtime::with_capture_source",
            ));
        }
        let driver = build_device_driver(&config)?;
        Self::build_validated(
            config,
            vec![source],
            driver,
            open_device,
            source_factory,
            None,
        )
    }

    fn build_validated(
        config: RuntimeConfig,
        mut sources: Vec<Box<dyn PacketSource + Send>>,
        driver: Box<dyn DeviceDriver>,
        open_device: bool,
        source_factory: Option<SourceFactory>,
        injected_capture: Option<Box<dyn PcmCapture>>,
    ) -> Result<Self, RuntimeError> {
        let expected = 1 + config.additional_inputs.len();
        if sources.len() != expected {
            return Err(RuntimeError::Config(
                "one packet source is required per input stream (primary + additional_inputs)",
            ));
        }
        let streams: Vec<Aes67InputConfig> = std::iter::once(config.input.clone())
            .chain(config.additional_inputs.iter().cloned())
            .collect();
        let input = Aes67InputBus::new(streams).map_err(RuntimeError::Bus)?;
        let initial = unloaded_processor(config.unloaded_policy);
        let engine =
            SlotEngine::new(config.engine.clone(), initial).map_err(RuntimeError::Engine)?;
        let mut backend: AlsaOutputBackend<P> = AlsaOutputBackend::new();
        if open_device {
            backend
                .start(&config.device)
                .map_err(RuntimeError::Backend)?;
        }
        let safety = SafetySupervisor::new(config.safety.clone());
        let receiver = Aes67Receiver::spawn(BoxedSource(sources.remove(0)), &config.receiver)?;
        let mut additional_receivers = Vec::with_capacity(sources.len());
        for source in sources {
            additional_receivers.push(Aes67Receiver::spawn(BoxedSource(source), &config.receiver)?);
        }
        let capture = match (&config.capture, injected_capture) {
            (Some(capture_config), Some(source)) => Some(
                CaptureInput::spawn_boxed(source, capture_config.clone())
                    .map_err(RuntimeError::Capture)?,
            ),
            (Some(capture_config), None) => Some(open_capture(capture_config)?),
            (None, Some(_)) => {
                return Err(RuntimeError::Config(
                    "injected capture source requires config.capture",
                ));
            }
            (None, None) => None,
        };
        let block = vec![0.0f32; input.samples_per_block()];
        let out_block = vec![0.0f32; engine.output_samples_per_block()];
        // Runtime 保持既有“块边界立即切换”的逐位合同：不引入淡入淡出；
        // OutputStage 仍保留可配置淡入能力，供将来显式启用。
        let output_stage =
            OutputStage::new(config.engine.sample_rate, config.engine.output_channels, 0);
        let packet_buf = vec![0u8; config.receiver.packet_bytes];
        Ok(Self {
            config,
            input,
            engine,
            backend,
            safety,
            driver,
            receiver,
            additional_receivers,
            capture,
            block,
            out_block,
            packet_buf,
            blocks: 0,
            output_errors: 0,
            consecutive_underruns: 0,
            consecutive_overruns: 0,
            pending_fault: None,
            last_fault_reason: None,
            device_fault: false,
            last_error: None,
            calibration: None,
            source_factory,
            input_paused: false,
            output_stage,
        })
    }

    /// 配置视图。
    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    /// 主流输入视图（诊断/测试用；RT 线程之外不要并发访问）。
    pub fn input(&self) -> &Aes67Input {
        self.input
            .stream_input(0)
            .expect("runtime input bus always has a primary stream")
    }

    /// 多流输入总线视图（诊断/测试用；RT 线程之外不要并发访问）。
    pub fn input_bus(&self) -> &Aes67InputBus {
        &self.input
    }

    /// 多流输入总线累计统计（控制面快照；会分配每路统计）。
    pub fn input_bus_stats(&self) -> Aes67InputBusStats {
        self.input.stats()
    }

    /// 块引擎视图。
    pub fn engine(&self) -> &SlotEngine {
        &self.engine
    }

    /// 当前生效图快照。
    pub fn graph_info(&self) -> GraphInfo {
        self.engine.graph_info()
    }

    /// 输出后端视图（测试里读假设备捕获的字节）。
    pub fn backend(&self) -> &AlsaOutputBackend<P> {
        &self.backend
    }

    /// 收包器视图。
    pub fn receiver(&self) -> &Aes67Receiver {
        &self.receiver
    }

    /// 安全监督器视图。
    pub fn safety(&self) -> &SafetySupervisor {
        &self.safety
    }

    /// 已处理块数。
    pub fn blocks(&self) -> u64 {
        self.blocks
    }

    /// 停止所有收包线程并 join；幂等。返回主流的收包统计。
    pub fn stop_receiver(&mut self) -> Result<ReceiverStats, RuntimeError> {
        let stats = self.receiver.stop();
        for receiver in &mut self.additional_receivers {
            let _ = receiver.stop();
        }
        stats
    }

    /// 某一路收包线程的统计快照（0 = 主流，1.. = additional_inputs；越界为 None）。
    pub fn receiver_stats(&self, stream: usize) -> Option<ReceiverStats> {
        if stream == 0 {
            Some(self.receiver.stats())
        } else {
            self.additional_receivers
                .get(stream - 1)
                .map(Aes67Receiver::stats)
        }
    }

    /// 主流收包线程是否在跑。
    pub fn receiver_running(&self) -> bool {
        self.receiver.is_running()
    }

    /// 所有流的收包线程是否都在跑（单流时等价于 [`Runtime::receiver_running`]）。
    pub fn receivers_running(&self) -> bool {
        self.receiver.is_running()
            && self
                .additional_receivers
                .iter()
                .all(Aes67Receiver::is_running)
    }

    /// 输入是否处于暂停状态（校准自动停输入）。
    pub fn input_paused(&self) -> bool {
        self.input_paused
    }

    /// 控制路径：暂停输入（停止/join 所有 AES67 收包线程并清空抖动缓冲；采集模式清采集队列）。
    ///
    /// 暂停后 `run_block` 写静音、**不**累计输入欠载、**不**触发安全故障；这是校准期间
    /// “不依赖外部发送端停止/iptables 也能安全测量”的入口。恢复走 [`Runtime::resume_input`]。
    ///
    /// AES67 模式需要构造时提供收包源工厂（生产 `new/new_deferred` 都有；注入假源通常没有，
    /// 此时暂停可以成功，恢复会如实返回配置错误，不假装重建）。
    pub fn pause_input(&mut self) -> Result<(), RuntimeError> {
        if self.input_paused {
            return Ok(());
        }
        // 先构造新输入总线（配置已校验，失败概率只可能是内部错误），再停线程，避免停完才发现分配失败。
        let fresh_input = if self.capture.is_some() {
            None
        } else {
            Some(Aes67InputBus::new(self.input_configs()).map_err(RuntimeError::Bus)?)
        };
        let mut first_error = None;
        if let Some(capture) = self.capture.as_mut() {
            capture.flush();
        } else {
            if let Err(err) = self.receiver.stop() {
                first_error = Some(err);
            }
            for receiver in &mut self.additional_receivers {
                if let Err(err) = receiver.stop() {
                    first_error.get_or_insert(err);
                }
            }
            while self.receiver.next_packet(&mut self.packet_buf).is_some() {}
            for receiver in &mut self.additional_receivers {
                while receiver.next_packet(&mut self.packet_buf).is_some() {}
            }
            if let Some(fresh) = fresh_input {
                self.input = fresh;
            }
        }
        self.input_paused = true;
        self.consecutive_underruns = 0;
        self.consecutive_overruns = 0;
        match first_error {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    /// 控制路径：恢复输入（AES67 模式按当前配置重新绑定组播并发包线程；采集模式清队列）。
    ///
    /// 新抖动缓冲从零预填（Warmup 静音块），恢复后不会把暂停期间旧包/断流混进节目。
    /// 失败时保持暂停状态并返回错误（fail-closed）；已构造的临时接收器由 Drop 停止。
    pub fn resume_input(&mut self) -> Result<(), RuntimeError> {
        if !self.input_paused {
            return Ok(());
        }
        if let Some(capture) = self.capture.as_mut() {
            capture.flush();
            self.input_paused = false;
            self.consecutive_underruns = 0;
            self.consecutive_overruns = 0;
            return Ok(());
        }
        let Some(factory) = self.source_factory.as_ref() else {
            return Err(RuntimeError::Config(
                "input source factory unavailable; cannot resume AES67 input",
            ));
        };
        let streams = self.input_configs();
        let mut sources = Vec::with_capacity(streams.len());
        for input in &streams {
            sources.push(factory(input, &self.config.receiver)?);
        }
        let fresh_input = Aes67InputBus::new(streams).map_err(RuntimeError::Bus)?;
        let primary = Aes67Receiver::spawn(BoxedSource(sources.remove(0)), &self.config.receiver)?;
        let mut additional = Vec::with_capacity(sources.len());
        for source in sources {
            additional.push(Aes67Receiver::spawn(
                BoxedSource(source),
                &self.config.receiver,
            )?);
        }
        self.receiver = primary;
        self.additional_receivers = additional;
        self.input = fresh_input;
        self.input_paused = false;
        self.consecutive_underruns = 0;
        self.consecutive_overruns = 0;
        Ok(())
    }

    /// 当前配置对应的输入流表（主流 + additional_inputs；控制路径分配）。
    fn input_configs(&self) -> Vec<Aes67InputConfig> {
        std::iter::once(self.config.input.clone())
            .chain(self.config.additional_inputs.iter().cloned())
            .collect()
    }

    /// 输出设备是否已打开（PCM 句柄在手；设备掉线/尚未打开时为 false）。
    pub fn device_started(&self) -> bool {
        self.backend.is_started()
    }

    /// 控制路径：打开输出设备（设备缺失/打不开时返回后端错误，运行时保持可用、不退出）。
    ///
    /// 打开成功后**重建输入抖动缓冲**：先抽干收包环里断线期间积压的旧包，再换一个全新的
    /// [`Aes67Input`]（预填从零重新开始，Warmup 静音块之后才恢复出声）；当前方案图与版本
    /// 保持不动（不需要重新 apply_plan，version/graph_id 语义不变）。块缓冲/收包线程不重建：
    /// 它们与设备生命周期无关。
    ///
    /// 不是 RT 安全（设备 IO/堆分配），只允许控制线程调用。
    pub fn open_device(&mut self) -> Result<(), RuntimeError> {
        if self.backend.is_started() {
            return Ok(());
        }
        // 先分配新的抖动缓冲再碰设备：配置已校验，分配失败只会是内部错误；失败时不改变旧状态。
        let streams: Vec<Aes67InputConfig> = std::iter::once(self.config.input.clone())
            .chain(self.config.additional_inputs.iter().cloned())
            .collect();
        let fresh_input = Aes67InputBus::new(streams).map_err(RuntimeError::Bus)?;
        self.backend
            .start(&self.config.device)
            .map_err(RuntimeError::Backend)?;
        // 设备打开后才丢弃断线期间的旧包：它们不属于恢复后的新抖动缓冲。
        while self.receiver.next_packet(&mut self.packet_buf).is_some() {
            // 收包统计仍在 receiver 侧累计；这里只把旧载荷从 SPSC 里抽走。
        }
        for receiver in &mut self.additional_receivers {
            while receiver.next_packet(&mut self.packet_buf).is_some() {}
        }
        if let Some(capture) = self.capture.as_mut() {
            capture.flush();
        }
        self.input = fresh_input;
        self.consecutive_underruns = 0;
        self.consecutive_overruns = 0;
        Ok(())
    }

    /// 控制路径：关闭输出设备并释放句柄（幂等；设备已掉线时调用也不会 panic）。
    pub fn close_device(&mut self) -> Result<(), RuntimeError> {
        self.backend.stop().map_err(RuntimeError::Backend)
    }

    /// 控制路径：显式上报"输出设备不在位"故障（启动时设备缺失/重开失败用）。
    ///
    /// 与块路径写失败等价：`last_error=BACKEND`、`device_fault=true`、安全监督器立即 faulted
    /// （fail-closed：输出静音、状态 `safe=true`）。恢复后由 [`Runtime::clear_device_loss_fault`]
    /// 显式清除；其它锁存故障不受影响。
    pub fn report_device_loss(&mut self, message: impl Into<String>) {
        self.last_error = Some(codes::BACKEND);
        self.device_fault = true;
        self.last_fault_reason = Some(FaultReason::Internal);
        if self.pending_fault == Some(FaultReason::Internal) {
            self.pending_fault = None;
        }
        self.safety.note_processor_fault(message);
    }

    /// 控制路径：设备恢复后清除"设备丢失"造成的安全故障。
    ///
    /// 只清块路径因 backend 写失败记录的 Internal 故障（`device_fault`）；处理器/欠载/换图等
    /// 其它故障仍保持锁存（fail-closed 语义不变）。恢复必须显式调用（与 `clear_fault` 一致）。
    /// 返回是否真的清掉了安全故障。
    pub fn clear_device_loss_fault(&mut self) -> bool {
        if !self.device_fault {
            return false;
        }
        self.device_fault = false;
        if self.pending_fault == Some(FaultReason::Internal) {
            self.pending_fault = None;
        }
        self.consecutive_underruns = 0;
        self.consecutive_overruns = 0;
        // 只清"设备丢失"这一类锁存：安全故障若是别的来源（欠载/处理器等），保持 fail-closed。
        if self.last_fault_reason != Some(FaultReason::Internal) {
            return false;
        }
        self.last_fault_reason = None;
        if self.safety.state() == RuntimeState::Faulted {
            return self.safety.clear_fault().is_ok();
        }
        true
    }

    /// 块循环一步：取包 → 取一块输入 → 图处理 → 安全静音 → 写输出设备，返回最终输出块。
    ///
    /// 设备未打开（缺失/掉线）时直接返回 [`BackendError::NotStarted`]：不抽包、不推进输入/图
    /// 状态、不写设备，进程保持运行等控制路径重开（fail-closed）。
    ///
    /// 顺序与实时约束：
    /// 1. 从 SPSC 抽干待处理 RTP 包并 feed_packet（零分配/无锁；每个块最多处理 SPSC 容量个包，
    ///    因此解码工作量有界）；
    /// 2. fill_block 取一块输入；欠载/预热时输入块被清零（整块静音，播放游标照常前进）；
    /// 3. SlotEngine::run_block（块边界换图/静音语义都在引擎内）；
    /// 4. 输入欠载、安全监督器要求静音、或有待上报故障时，把输出块整块清零
    ///    （图状态继续前进，恢复时不带旧数据）；
    /// 5. playout 写设备。
    ///
    /// 正常路径零分配、零锁、无日志、不 panic；错误经返回值上报并计数。
    pub fn run_block(&mut self) -> Result<&[f32], RuntimeError> {
        if !self.backend.is_started() {
            // 设备不在位：fail-closed 直接返回，不抽包、不推进输入/图状态、不写设备。
            // 控制路径用 open_device 重开成功后再恢复块循环（进程本身不退出）。
            return Err(RuntimeError::Backend(BackendError::NotStarted));
        }
        // 1) 输入：暂停期间不抽包、不填抖动环、不计欠载；校准控制路径会显式 pause/resume。
        //    非暂停时：采集模式从采集块队列拉一块；AES67 模式先抽干收包 SPSC 再填抖动环。
        //    两条路径的 fill 都是声明的实时安全操作（零分配/无锁/无 IO）。
        let input_paused = self.input_paused;
        let input_filled = if input_paused {
            self.block.fill(0.0);
            false
        } else if let Some(capture) = self.capture.as_mut() {
            let outcome = capture.fill_block(&mut self.block);
            outcome.status == CaptureFillStatus::Filled
        } else {
            while let Some(len) = self.receiver.next_packet(&mut self.packet_buf) {
                if let Ok(FeedOutcome::Overflow { .. }) =
                    self.input.feed_packet(0, &self.packet_buf[..len])
                {
                    self.consecutive_overruns = self.consecutive_overruns.saturating_add(1);
                    if self.consecutive_overruns >= self.config.safety.max_consecutive_overruns {
                        self.note_fault(FaultReason::Overrun);
                    }
                }
            }
            for index in 0..self.additional_receivers.len() {
                while let Some(len) =
                    self.additional_receivers[index].next_packet(&mut self.packet_buf)
                {
                    if let Ok(FeedOutcome::Overflow { .. }) =
                        self.input.feed_packet(index + 1, &self.packet_buf[..len])
                    {
                        self.consecutive_overruns = self.consecutive_overruns.saturating_add(1);
                        if self.consecutive_overruns >= self.config.safety.max_consecutive_overruns
                        {
                            self.note_fault(FaultReason::Overrun);
                        }
                    }
                }
            }
            // 2) 取一块输入；至少一路有数据即视为已填（断流流按 0 贡献，不 gate 其它流）。
            match self.input.fill_block(&mut self.block) {
                Ok(outcome) => outcome.filled_streams > 0,
                Err(_) => {
                    self.last_error = Some(codes::CONFIG);
                    self.note_fault(FaultReason::Underrun);
                    return Err(RuntimeError::Input(InputError::BadConfig(
                        "input bus fill_block rejected its own preallocated block",
                    )));
                }
            }
        };
        if input_paused {
            // 暂停是控制面主动状态：不累计输入欠载，不触发安全故障。
        } else if input_filled {
            self.consecutive_underruns = 0;
            self.consecutive_overruns = 0;
        } else {
            self.consecutive_underruns = self.consecutive_underruns.saturating_add(1);
            if self.consecutive_underruns >= self.config.safety.max_consecutive_underruns {
                self.note_fault(FaultReason::Underrun);
            }
        }

        // 3) 块引擎（块边界换图/静音都在里面）。
        let processed = match self.engine.run_block(&self.block) {
            Ok(processed) => processed,
            Err(err) => {
                self.last_error = Some(codes::ENGINE);
                self.note_fault(FaultReason::ProcessorFault);
                return Err(RuntimeError::Engine(err));
            }
        };
        self.out_block.copy_from_slice(processed);

        // 4) 输出级：安全故障/输入欠载立即硬清零；用户静音/校准旁路/淡入都在这里统一处理。
        //    图已处理过本块（状态保持前进），不会因为静音回退 DSP 状态。
        let hard_mute = input_paused
            || !input_filled
            || self.safety.fault_requires_mute()
            || self.pending_fault.is_some();
        self.output_stage.apply(&mut self.out_block, hard_mute);

        // 5) 写输出后端。
        self.blocks = self.blocks.saturating_add(1);
        if let Err(err) = self.backend.playout(&self.out_block) {
            self.output_errors = self.output_errors.saturating_add(1);
            self.last_error = Some(codes::BACKEND);
            // 设备丢失标记：控制路径恢复设备后只清这一类故障（clear_device_loss_fault）。
            self.device_fault = true;
            self.note_fault(FaultReason::Internal);
            return Err(RuntimeError::Backend(err));
        }
        self.safety.note_block_ok();
        Ok(&self.out_block)
    }

    /// 控制路径：把数据面故障交给安全监督器，并推进设备生命周期状态机一步。
    ///
    /// 不是 RT 安全（DeviceSupervisor::poll 会做系统调用/外部命令，安全监督器构造
    /// 故障文本会分配）；由控制线程按固定节拍调用，每次最多推进一步。
    pub fn poll_device(&mut self) -> SupervisorStatus {
        if let Some(reason) = self.pending_fault.take() {
            self.safety.note_processor_fault(fault_message(reason));
        }
        self.driver.poll(&mut self.safety)
    }

    /// 请求设备监督器立即重试（清除锁存/退避）。
    pub fn request_device_retry(&mut self) {
        self.driver.request_retry();
    }

    /// 是否有待上报给安全监督器的故障（RT 路径只置位，控制路径 poll）。
    pub fn has_pending_fault(&self) -> bool {
        self.pending_fault.is_some()
    }

    /// 控制路径：清掉「输入欠载已恢复」的安全故障。
    ///
    /// 只处理 `FaultReason::Underrun` 且当前已无连续欠载的情况（典型：启动瞬间 RTP 包还没到）；
    /// 处理器/后端/换图故障不会自动清除，仍要求人工确认（fail-closed）。
    /// 返回是否真的从 Faulted 恢复。
    pub fn clear_recovered_underrun_fault(&mut self) -> bool {
        if self.safety.state() != RuntimeState::Faulted {
            return false;
        }
        if self.last_fault_reason != Some(FaultReason::Underrun) || self.consecutive_underruns != 0
        {
            return false;
        }
        if self.safety.clear_fault().is_ok() {
            self.last_fault_reason = None;
            true
        } else {
            false
        }
    }

    /// 用户静音请求：块边界生效（回执里的 applied_block 是生效块号）。
    ///
    /// 与安全静音相互独立：用户解除静音不会清除安全故障，锁存期间 run_block 仍然整块清零
    /// （fail-closed）。这里同时把请求写进安全监督器的 user_mute，状态与引擎保持一致。
    pub fn request_mute(&mut self, mute: bool) -> MuteReceipt {
        let receipt = self.output_stage.request_program_mute(mute);
        self.safety.set_user_mute(mute);
        receipt
    }

    /// 用户静音请求（与安全静音独立的用户意图；热重配前后原样恢复用）。
    pub fn user_mute_requested(&self) -> bool {
        self.output_stage.program_mute_requested()
    }

    /// 输出级当前实际静音状态（块边界后的值；校准旁路时即使请求了静音也为 false）。
    pub fn muted(&self) -> bool {
        self.output_stage.muffled()
    }

    /// 输出级视图（测试/控制面诊断）。
    pub fn output_stage(&self) -> &OutputStage {
        &self.output_stage
    }

    /// 控制路径：设置/清除校准旁路。
    ///
    /// 旁路为 true 时 program mute 不再清零输出（扫频可闻），但安全故障仍优先；
    /// cal_enter/cal_leave 负责与输入暂停、原静音状态一起成对使用。
    pub fn set_calibration_bypass(&mut self, on: bool) {
        self.output_stage.set_calibration_bypass(on);
    }

    /// 当前校准旁路是否开启。
    pub fn calibration_bypass(&self) -> bool {
        self.output_stage.calibration_bypass()
    }

    /// 状态快照（非实时路径；会克隆设备监督器的错误文本）。
    pub fn status(&self) -> RuntimeStatus {
        let device = self.driver.status();
        let device_error = device.last_error.as_ref().map(|err| err.code.as_str());
        RuntimeStatus {
            state: self.safety.state(),
            blocks: self.blocks,
            input: self
                .input
                .stream_input(0)
                .map(Aes67Input::stats)
                .unwrap_or_default(),
            input_streams: self.input_bus_stats().streams,
            input_mode: if self.capture.is_some() {
                "alsa_capture"
            } else {
                "aes67"
            },
            capture: self.capture.as_ref().map(CaptureInput::stats),
            output: self.backend.stats(),
            receiver: self.receiver.stats(),
            device_rate: device.device_rate,
            plan_rate: device.plan_rate,
            rate_asserted: device.rate_asserted,
            output_errors: self.output_errors,
            output_started: self.backend.is_started(),
            graph: self.engine.graph_info(),
            switch_pending: self.engine.switch_pending(),
            muted: self.output_stage.muffled(),
            output_stage: self.output_stage.status(),
            input_paused: self.input_paused,
            last_error: self.last_error.or(device_error),
            device,
        }
    }

    /// 下一个方案的版本号（还没有任何图生效时为 1）。
    pub fn pending_plan_version(&self) -> u64 {
        match self.engine.graph_info().version {
            Some(version) => version.saturating_add(1),
            None => 1,
        }
    }

    /// 方案加载第一步（控制路径）：解码 ABI v3 → 准入/编译成图 → 返回草稿。
    ///
    /// 校验全部复用既有实现（Plan::decode / PlanGraph::compile，后者内部调用 admit），
    /// 这里不重复实现任何校验。不改变正在跑的音频；commit_plan 才在下一块边界换图。
    pub fn prepare_plan(
        &mut self,
        bytes: &[u8],
        limits: &AdmissionLimits,
    ) -> Result<PlanDraft, RuntimeError> {
        let plan = Plan::decode(bytes).map_err(RuntimeError::Plan)?;
        let version = self.pending_plan_version();
        compile_plan_draft(&plan, self.engine.config(), limits, version)
    }

    /// 方案加载第二步：把草稿排入 SlotEngine 的下一块边界，返回将生效的块号。
    ///
    /// 过期草稿（版本号不是当前下一个版本）被拒绝，不改变执行器（engine_token_invalid）。
    /// 与 SlotEngine::stage_processor 语义一致：连续 stage 以最后一次为准，不打断当前块。
    pub fn commit_plan(&mut self, draft: PlanDraft) -> Result<u64, RuntimeError> {
        let expected = self.pending_plan_version();
        if draft.version != expected {
            return Err(RuntimeError::DraftInvalid(
                "stale plan draft: version is not the next version; prepare again",
            ));
        }
        Ok(self
            .engine
            .stage_processor(draft.processor, draft.version, draft.graph_id))
    }

    /// 控制通道：把控制线程编译好的草稿排入下一块边界（版本由控制面分配，这里不重复校验）。
    ///
    /// 与 Runtime::commit_plan 的区别：后者校验"草稿版本 == 下一个版本"（单 owner 控制面用），
    /// 跨进程控制通道的版本由控制核心单调分配，因此直接交给执行器。返回新图将生效的块号
    /// （与 SlotEngine::stage_processor 一致）。
    pub fn stage_plan_draft(&mut self, draft: PlanDraft) -> u64 {
        let (version, graph_id, processor) = draft.into_parts();
        self.engine.stage_processor(processor, version, graph_id)
    }
    // -----------------------------------------------------------------------
    // 热重配（M3b）：控制路径换采样率/通道/AES67 输入/输出设备，失败按旧配置回滚
    // -----------------------------------------------------------------------

    /// 控制路径热重配（M3b）：不退出进程，换输入/引擎/设备配置。
    ///
    /// 调用者约束：必须在块循环的块边界调用（此时没有块在跑），并且已经静音。
    /// 本方法自己做"关旧设备/收包 → 建新对象 → 开新设备 → 回读校验"，任何一步失败都
    /// 按旧配置恢复（best-effort），返回 [`ReconfigureFailure`] 说明回滚后的真实状态。
    ///
    /// 语义：
    /// - `target` 必须是完整的新 [`RuntimeConfig`]（调用方从当前配置派生，`config.validate` 先跑）；
    /// - 引擎格式（rate/block/channels）变化才重建 [`SlotEngine`]，旧图随旧引擎丢弃
    ///   （`graph_dropped=true`）；`plan` 是控制线程按 `target.engine` 预编译的草稿，给了就
    ///   在新引擎上 stage（下一块边界生效）；
    /// - 收包 socket 只在 port/multicast/收包参数变化时重绑（需要构造时注入的收包源工厂）；
    /// - 设备必须能按新 DeviceConfig 打开，且 `verify` 回读的 hw rate/channels 必须与目标一致，
    ///   否则关新句柄、按旧配置重开；
    /// - 块计数（`blocks`）连续、安全监督器与用户静音保持；ALSA 统计与输入统计会从新句柄/
    ///   新抖动环重新计数（`alsa_stats_reset`/`input_stats_reset` 如实上报）。
    ///
    /// 不是 RT 路径（控制线程调用；设备 IO/分配都在这里），`run_block` 热路径一行不改。
    pub fn reconfigure(
        &mut self,
        target: RuntimeConfig,
        plan: Option<PlanDraft>,
        verify: impl FnOnce() -> Result<HwReadback, String>,
    ) -> Result<ReconfigureOutcome, ReconfigureFailure> {
        let old = self.config.clone();
        let old_device_was_started = self.backend.is_started();
        let old_receiver_was_running = self.receiver.is_running();
        // 采集输入的热重配（换设备/通道/映射）尚未实现：fail-closed，不静默换源。
        if old.capture.is_some() || target.capture.is_some() {
            return Err(ReconfigureFailure {
                code: codes::RECONFIGURE_CONFIG,
                stage: "capture",
                message: "ALSA capture input hot reconfiguration is not implemented yet; restart the runtime with the new capture config instead (fail-closed, no source is silently swapped)".to_string(),
                rolled_back: true,
                device_started: old_device_was_started,
                receiver_running: old_receiver_was_running,
            });
        }
        // 多流输入的热重配（重绑/重建每路收包线程）尚未实现：fail-closed，不静默丢流。
        if !old.additional_inputs.is_empty() || !target.additional_inputs.is_empty() {
            return Err(ReconfigureFailure {
                code: codes::RECONFIGURE_CONFIG,
                stage: "multi_input",
                message: "multi-stream AES67 input hot reconfiguration is not implemented yet; restart the runtime with the new input list instead (fail-closed, no stream is silently dropped)".to_string(),
                rolled_back: true,
                device_started: old_device_was_started,
                receiver_running: old_receiver_was_running,
            });
        }
        // 1) 校验目标配置：失败时什么都没碰。
        if let Err(err) = target.validate() {
            return Err(ReconfigureFailure {
                code: codes::RECONFIGURE_CONFIG,
                stage: "validate",
                message: err.to_string(),
                rolled_back: true,
                device_started: old_device_was_started,
                receiver_running: old_receiver_was_running,
            });
        }
        // 2) 先把新对象全部分配好（分配失败时也什么都没碰）。
        let new_input = match Aes67InputBus::new(vec![target.input.clone()]) {
            Ok(input) => input,
            Err(err) => {
                return Err(ReconfigureFailure {
                    code: codes::RECONFIGURE_CONFIG,
                    stage: "input",
                    message: err.to_string(),
                    rolled_back: true,
                    device_started: old_device_was_started,
                    receiver_running: old_receiver_was_running,
                });
            }
        };
        let engine_changed = target.engine != old.engine;
        let new_engine = if engine_changed {
            match SlotEngine::new(
                target.engine.clone(),
                unloaded_processor(target.unloaded_policy),
            ) {
                Ok(engine) => Some(engine),
                Err(err) => {
                    return Err(ReconfigureFailure {
                        code: codes::RECONFIGURE_CONFIG,
                        stage: "engine",
                        message: err.to_string(),
                        rolled_back: true,
                        device_started: old_device_was_started,
                        receiver_running: old_receiver_was_running,
                    });
                }
            }
        } else {
            None
        };
        let new_driver = match build_device_driver(&target) {
            Ok(driver) => driver,
            Err(err) => {
                return Err(ReconfigureFailure {
                    code: codes::RECONFIGURE_CONFIG,
                    stage: "driver",
                    message: err.to_string(),
                    rolled_back: true,
                    device_started: old_device_was_started,
                    receiver_running: old_receiver_was_running,
                });
            }
        };
        let new_block = vec![0.0f32; new_input.samples_per_block()];
        let new_out_block = if engine_changed {
            Some(vec![0.0f32; target.engine.output_samples_per_block()])
        } else {
            None
        };
        // 3) 收包：只有 port/组播地址/收包参数变化才需要重绑 socket。
        let needs_rebind = target.input.port != old.input.port
            || target.input.multicast_addr != old.input.multicast_addr
            || target.receiver != old.receiver;
        if needs_rebind && self.source_factory.is_none() {
            return Err(ReconfigureFailure {
                code: codes::RECONFIGURE_RECEIVER,
                stage: "receiver_source",
                message: "this runtime was built with an injected packet source and no source factory; rebinding the AES67 receiver (port/multicast/interface change) is unavailable, so the configuration was not changed (fail-closed)".to_string(),
                rolled_back: true,
                device_started: old_device_was_started,
                receiver_running: old_receiver_was_running,
            });
        }
        let mut new_receiver: Option<Aes67Receiver> = None;
        if needs_rebind {
            // 同端口不能叠两个 socket：先停旧收包线程（最坏一次 recv 超时的 join）。
            let _ = self.receiver.stop();
            match self.spawn_receiver(&target) {
                Ok(receiver) => new_receiver = Some(receiver),
                Err(err) => {
                    let receiver_running = self.restore_receiver(&old);
                    return Err(ReconfigureFailure {
                        code: codes::RECONFIGURE_RECEIVER,
                        stage: "receiver_spawn",
                        message: err.to_string(),
                        rolled_back: receiver_running,
                        device_started: old_device_was_started,
                        receiver_running,
                    });
                }
            }
        }
        // 4) 设备：先关旧 PCM，再按新配置打开；失败/回读不符就按旧配置恢复。
        if let Err(err) = self.backend.stop() {
            // stop 目前不会失败；真失败了也按"新设备还没开"处理，回滚收包。
            drop(new_receiver);
            let receiver_running = self.restore_receiver_if_stopped(&old, needs_rebind);
            return Err(ReconfigureFailure {
                code: codes::RECONFIGURE_DEVICE,
                stage: "device_close",
                message: format!("close output device failed: {err}"),
                rolled_back: receiver_running,
                device_started: false,
                receiver_running,
            });
        }
        if let Err(err) = self.backend.start(&target.device) {
            drop(new_receiver);
            let (device_started, receiver_running) =
                self.rollback_reconfigure(&old, needs_rebind, old_device_was_started);
            let mut message = format!("open new output device failed: {err}");
            if !device_started && old_device_was_started {
                message.push_str("; the previous output device could not be reopened either (device absent, fail-closed)");
            }
            return Err(ReconfigureFailure {
                code: codes::RECONFIGURE_DEVICE,
                stage: "device_open",
                message,
                rolled_back: device_started == old_device_was_started && receiver_running,
                device_started,
                receiver_running,
            });
        }
        // 5) 回读校验：必须是真实 hw params（rate/channels 都要读到，且与目标一致）。
        let readback = match verify() {
            Ok(readback) => readback,
            Err(message) => {
                return Err(self.fail_after_verify(
                    &old,
                    needs_rebind,
                    old_device_was_started,
                    format!("hw params readback failed: {message}"),
                ));
            }
        };
        let target_channels = u32::try_from(target.device.output_channels).ok();
        let mismatch = if readback.sample_rate.is_none() || readback.channels.is_none() {
            Some(format!(
                "hw params readback unavailable (rate {:?}, channels {:?}); hot reconfiguration is fail-closed without a real readback",
                readback.sample_rate, readback.channels
            ))
        } else if readback.sample_rate != Some(target.device.sample_rate) {
            Some(format!(
                "hw rate readback {:?} Hz != target {} Hz",
                readback.sample_rate, target.device.sample_rate
            ))
        } else if readback.channels != target_channels {
            Some(format!(
                "hw channels readback {:?} != target {:?}",
                readback.channels, target_channels
            ))
        } else {
            None
        };
        if let Some(message) = mismatch {
            return Err(self.fail_after_verify(
                &old,
                needs_rebind,
                old_device_was_started,
                message,
            ));
        }
        // 6) 全部校验通过：提交新状态（块计数/安全状态/用户静音保持）。
        let graph_dropped = engine_changed && plan.is_none();
        if needs_rebind {
            self.receiver = new_receiver.take().expect("receiver spawned above");
        }
        self.config = target;
        self.input = new_input;
        self.block = new_block;
        if let Some(engine) = new_engine {
            self.engine = engine;
        }
        if let Some(out_block) = new_out_block {
            self.out_block = out_block;
        }
        self.driver = new_driver;
        self.consecutive_underruns = 0;
        self.consecutive_overruns = 0;
        if !old_device_was_started {
            // 设备此前 absent：重配成功即恢复，清掉"设备丢失"锁存故障（其它锁存故障保持）。
            self.clear_device_loss_fault();
        }
        if let Some(draft) = plan {
            self.stage_plan_draft(draft);
        }
        Ok(ReconfigureOutcome {
            format: self.config.engine.clone(),
            receiver_rebound: needs_rebind,
            engine_rebuilt: engine_changed,
            graph_dropped,
            device_was_absent: !old_device_was_started,
            hw: readback,
            blocks: self.blocks,
            alsa_stats_reset: old_device_was_started,
            input_stats_reset: true,
        })
    }

    /// 热重配收尾失败（新设备已打开但回读校验不符）：关新设备、按旧配置恢复。
    fn fail_after_verify(
        &mut self,
        old: &RuntimeConfig,
        needs_rebind: bool,
        old_device_was_started: bool,
        message: String,
    ) -> ReconfigureFailure {
        let (device_started, receiver_running) =
            self.rollback_reconfigure(old, needs_rebind, old_device_was_started);
        ReconfigureFailure {
            code: codes::RECONFIGURE_VERIFY,
            stage: "hw_verify",
            message,
            rolled_back: device_started == old_device_was_started && receiver_running,
            device_started,
            receiver_running,
        }
    }

    /// 失败回滚：关掉新设备（如果开过）、按旧配置重开旧设备与旧收包线程。
    ///
    /// 返回（回滚后设备是否打开, 回滚后收包是否在跑）。旧设备本来就没打开时"恢复"=
    /// 保持关闭（设备 absent 的 fail-closed 语义），不算失败。
    fn rollback_reconfigure(
        &mut self,
        old: &RuntimeConfig,
        needs_rebind: bool,
        old_device_was_started: bool,
    ) -> (bool, bool) {
        let _ = self.backend.stop();
        let device_started = if old_device_was_started {
            match self.backend.start(&old.device) {
                Ok(()) => true,
                Err(err) => {
                    // 旧设备也打不开：进入 fail-closed（不退出进程，由宿主按退避重开）。
                    self.report_device_loss(format!(
                        "hot reconfiguration rolled back but the previous output device could not be reopened: {err}",
                    ));
                    false
                }
            }
        } else {
            false
        };
        let receiver_running = self.restore_receiver_if_stopped(old, needs_rebind);
        (device_started, receiver_running)
    }

    /// 收包线程被停掉后按给定配置重建；没停过则原样返回当前状态。
    fn restore_receiver_if_stopped(&mut self, old: &RuntimeConfig, stopped: bool) -> bool {
        if stopped && !self.receiver.is_running() {
            self.restore_receiver(old)
        } else {
            self.receiver.is_running()
        }
    }

    /// 按给定配置重建收包线程（热重配回滚用；需要构造时注入的收包源工厂）。
    fn restore_receiver(&mut self, config: &RuntimeConfig) -> bool {
        match self.spawn_receiver(config) {
            Ok(receiver) => {
                self.receiver = receiver;
                self.receiver.is_running()
            }
            Err(_) => false,
        }
    }

    /// 用收包源工厂按配置造一个新收包线程（不装配到 self）。
    fn spawn_receiver(&self, config: &RuntimeConfig) -> Result<Aes67Receiver, RuntimeError> {
        let Some(factory) = self.source_factory.as_ref() else {
            return Err(RuntimeError::Receiver(ReceiverError::new(
                codes::RECONFIGURE_RECEIVER,
                "this runtime has no source factory (injected packet source); the AES67 receiver cannot be rebuilt",
            )));
        };
        let source = factory(&config.input, &config.receiver)?;
        Aes67Receiver::spawn(BoxedSource(source), &config.receiver)
    }

    // -----------------------------------------------------------------------
    // 校准播放（T70f-c）：控制线程预渲染，块线程逐块写设备
    // -----------------------------------------------------------------------

    /// 控制路径：开始一段校准播放（预渲染数据在此一次性移交给 Runtime）。
    ///
    /// 只登记，不写设备；块循环看到 [Runtime::calibration_playback_active] 后改用
    /// [Runtime::run_calibration_block] 写设备。返回总块数。已在校准播放时返回配置错误
    /// （同一时刻只允许一段；由控制面串行化）。不是 RT 路径。
    pub fn begin_calibration_playback(
        &mut self,
        playback: CalibrationPlayback,
    ) -> Result<u64, RuntimeError> {
        if self.calibration.is_some() {
            return Err(RuntimeError::Config(
                "calibration playback already active (single session at a time)",
            ));
        }
        if playback.block_frames() != self.config.engine.block_frames {
            return Err(RuntimeError::Config(
                "calibration playback block_frames does not match the runtime engine block size",
            ));
        }
        if playback.channels() != self.config.engine.output_channels {
            return Err(RuntimeError::Config(
                "calibration playback channels do not match the runtime engine channels",
            ));
        }
        let total = playback.total_blocks();
        self.calibration = Some(ActiveCalibration {
            playback,
            next_block: 0,
        });
        Ok(total)
    }

    /// 控制路径：结束校准播放（回到播放模式）。返回此前是否有校准播放。
    pub fn end_calibration_playback(&mut self) -> bool {
        self.calibration.take().is_some()
    }

    /// 是否有进行中的校准播放。
    pub fn calibration_playback_active(&self) -> bool {
        self.calibration.is_some()
    }

    /// 校准播放进度（已写块数, 总块数）；没有校准播放时为 None。
    pub fn calibration_playback_progress(&self) -> Option<(u64, u64)> {
        self.calibration.as_ref().map(|active| {
            (
                active.next_block.min(active.playback.total_blocks()),
                active.playback.total_blocks(),
            )
        })
    }

    /// 块循环一步（校准模式）：把预渲染块写到设备；扫频写完后输出收尾静音。
    ///
    /// RT 约束：只用预分配缓冲做定长 memcpy/fill（无分配、无锁、无 IO），真实设备 IO
    /// 仍走 backend.playout（与 run_block 同一条后端路径、同样的 xrun 恢复）。
    /// 安全静音照旧：安全监督器要求静音或已有待上报故障时整块清零（fail-closed）。
    /// 不推进输入抖动缓冲、不跑方案图、不改图版本。
    pub fn run_calibration_block(&mut self) -> Result<&[f32], RuntimeError> {
        if !self.backend.is_started() {
            return Err(RuntimeError::Backend(BackendError::NotStarted));
        }
        let Some(active) = self.calibration.as_mut() else {
            return Err(RuntimeError::Config("no calibration playback is active"));
        };
        let block_samples = active.playback.block_frames * active.playback.channels;
        if active.next_block < active.playback.total_blocks {
            let offset = (active.next_block as usize) * block_samples;
            self.out_block
                .copy_from_slice(&active.playback.data[offset..offset + block_samples]);
            active.next_block += 1;
        } else {
            // 扫频数据已播完：收尾静音，直到控制线程显式 end（不要让节目混进采集尾巴）。
            self.out_block.fill(0.0);
        }
        let done = active.next_block.min(active.playback.total_blocks);
        let total = active.playback.total_blocks;
        active
            .playback
            .progress
            .blocks_done
            .store(done, Ordering::Release);
        active
            .playback
            .progress
            .started
            .store(true, Ordering::Release);
        if done >= total {
            active
                .playback
                .progress
                .finished
                .store(true, Ordering::Release);
        }
        // 校准播放同样经过输出级：安全故障硬清零；program mute 在 calibration_bypass
        // 为 true 时放行，保证扫频不被用户静音状态吞掉。输入暂停不参与校准播放。
        let hard_mute = self.safety.fault_requires_mute() || self.pending_fault.is_some();
        self.output_stage.apply(&mut self.out_block, hard_mute);
        self.blocks = self.blocks.saturating_add(1);
        if let Err(err) = self.backend.playout(&self.out_block) {
            self.output_errors = self.output_errors.saturating_add(1);
            self.last_error = Some(codes::BACKEND);
            self.device_fault = true;
            self.note_fault(FaultReason::Internal);
            return Err(RuntimeError::Backend(err));
        }
        self.safety.note_block_ok();
        Ok(&self.out_block)
    }

    /// 一步完成 prepare + commit（控制面简单路径）；返回将生效的块号。
    pub fn apply_plan(
        &mut self,
        bytes: &[u8],
        limits: &AdmissionLimits,
    ) -> Result<u64, RuntimeError> {
        let draft = self.prepare_plan(bytes, limits)?;
        self.commit_plan(draft)
    }

    /// RT 路径把连续故障升级成待上报故障（只写 Copy 枚举，不分配）。
    fn note_fault(&mut self, reason: FaultReason) {
        if self.pending_fault.is_none() {
            self.pending_fault = Some(reason);
        }
        if self.last_fault_reason.is_none() {
            self.last_fault_reason = Some(reason);
        }
        if self.last_error.is_none() {
            let code = match reason {
                FaultReason::Underrun => codes::UNDERRUN,
                FaultReason::Overrun => codes::OVERRUN,
                FaultReason::ProcessorFault => codes::ENGINE,
                FaultReason::CommitFailed => codes::ENGINE,
                FaultReason::Internal => codes::INTERNAL,
            };
            self.last_error = Some(code);
        }
    }
}

/// 故障原因 → 安全监督器消息（非实时路径调用，允许分配）。
fn fault_message(reason: FaultReason) -> &'static str {
    match reason {
        FaultReason::Underrun => "runtime: consecutive input underruns",
        FaultReason::Overrun => "runtime: consecutive input overruns",
        FaultReason::ProcessorFault => "runtime: processor fault",
        FaultReason::CommitFailed => "runtime: plan commit failed",
        FaultReason::Internal => "runtime: output device fault",
    }
}

// ===========================================================================
// 校准播放 seam（T70f-c）：控制线程预渲染整段 sweep，块线程逐块写设备
// ===========================================================================
//
// 校准模式（旧 cal_control 设计回迁）里音频线程要"播放一段由非实时线程生成的扫频"：
// 控制线程把 T61 时间轴完整渲染成帧交错 f32（时长 10~30 s，内存几 MB），交给块线程；
// 块线程每块只做一次 memcpy（+ 安全静音判断）就写设备，**不跑方案图**——这样既不会
// 因为未加载方案时的 fail-closed 静音策略把扫频吞掉，也不会把校准信号混进节目链。
//
// RT 约束：run_calibration_block 只做定长 memcpy/fill、原子计数、后端写；无分配、无锁、
// 无 IO（backend.playout 与 run_block 同一条后端路径），不推进输入抖动缓冲与图状态。
// 数据在 begin_calibration_playback（控制路径）一次性交给 Runtime，之后块线程只读。

/// 校准播放进度（块线程写、控制线程/校准引擎读；原子量，无锁）。
#[derive(Debug)]
pub struct CalibrationProgress {
    total_blocks: AtomicU64,
    blocks_done: AtomicU64,
    started: AtomicBool,
    finished: AtomicBool,
}

impl CalibrationProgress {
    /// 新建（总块数 = 播放时长 / 块长）。
    pub fn new(total_blocks: u64) -> Self {
        Self {
            total_blocks: AtomicU64::new(total_blocks),
            blocks_done: AtomicU64::new(0),
            started: AtomicBool::new(false),
            finished: AtomicBool::new(false),
        }
    }

    /// 总块数。
    pub fn total_blocks(&self) -> u64 {
        self.total_blocks.load(Ordering::Acquire)
    }

    /// 已写出的块数（含收尾静音块）。
    pub fn blocks_done(&self) -> u64 {
        self.blocks_done.load(Ordering::Acquire)
    }

    /// 扫频数据是否已经全部写出（之后块线程输出收尾静音，直到 end）。
    pub fn finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    /// 块线程是否已经开始播放（第一次 run_calibration_block）。
    pub fn started(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }
}

/// 一段预渲染好的校准播放数据（帧交错 f32，长度 = 总块数 x 块长 x 通道数）。
pub struct CalibrationPlayback {
    data: Arc<[f32]>,
    block_frames: usize,
    channels: usize,
    total_blocks: u64,
    progress: Arc<CalibrationProgress>,
}

impl core::fmt::Debug for CalibrationPlayback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CalibrationPlayback")
            .field("block_frames", &self.block_frames)
            .field("channels", &self.channels)
            .field("total_blocks", &self.total_blocks)
            .field("frames", &(self.data.len() / self.channels.max(1)))
            .finish_non_exhaustive()
    }
}

impl CalibrationPlayback {
    /// 用帧交错 f32 构造；长度必须是 块长 x 通道数 的整数倍，数据不能为空。
    pub fn new(
        interleaved: Vec<f32>,
        block_frames: usize,
        channels: usize,
        progress: Arc<CalibrationProgress>,
    ) -> Result<Self, RuntimeError> {
        if block_frames == 0 || channels == 0 {
            return Err(RuntimeError::Config(
                "calibration playback: zero block/channels",
            ));
        }
        let block_samples = block_frames
            .checked_mul(channels)
            .ok_or(RuntimeError::Config(
                "calibration playback: block size overflow",
            ))?;
        if interleaved.is_empty() || !interleaved.len().is_multiple_of(block_samples) {
            return Err(RuntimeError::Config(
                "calibration playback: data length is not a multiple of block_frames*channels",
            ));
        }
        let total_blocks = (interleaved.len() / block_samples) as u64;
        if total_blocks == 0 {
            return Err(RuntimeError::Config("calibration playback: empty data"));
        }
        // 进度对象与数据一起移交：调用方按同一总块数构造 CalibrationProgress。
        if progress.total_blocks() != total_blocks {
            return Err(RuntimeError::Config(
                "calibration playback: progress total_blocks does not match data",
            ));
        }
        Ok(Self {
            data: Arc::from(interleaved),
            block_frames,
            channels,
            total_blocks,
            progress,
        })
    }

    /// 总块数。
    pub fn total_blocks(&self) -> u64 {
        self.total_blocks
    }

    /// 每块帧数。
    pub fn block_frames(&self) -> usize {
        self.block_frames
    }

    /// 通道数（= 数据帧内样本数）。
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// 进度共享句柄（校准引擎轮询用）。
    pub fn progress(&self) -> Arc<CalibrationProgress> {
        Arc::clone(&self.progress)
    }
}

/// Runtime 内正在进行的校准播放。
struct ActiveCalibration {
    playback: CalibrationPlayback,
    /// 下一块要写出的数据块号；>= total_blocks 之后写出收尾静音。
    next_block: u64,
}

// ===========================================================================
// 热重配 seam（M3b）：控制路径换配置的机械结果/失败信息
// ===========================================================================

/// 新设备打开后从 hw params 回读到的真实值（None = 该字段读不到）。
///
/// 回读校验由宿主注入（rt-host 读 /proc/asound 的 hw_params；测试注入假值）：
/// Runtime 只做"rate/channels 必须都读到且等于目标"的判定，不猜设备能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HwReadback {
    /// hw params 回读的速率（Hz）。
    pub sample_rate: Option<u32>,
    /// hw params 回读的通道数。
    pub channels: Option<u32>,
}

/// 热重配成功的机械结果（rt-host 控制面在此基础上上报 gap/代际/回滚）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconfigureOutcome {
    /// 生效后的执行器格式。
    pub format: EngineConfig,
    /// 收包线程是否重绑（false = socket 没动，只换了输入抖动环/映射）。
    pub receiver_rebound: bool,
    /// 块引擎是否重建（格式变化才重建）。
    pub engine_rebuilt: bool,
    /// 旧图是否随旧引擎被丢弃（格式变了且没有预编译草稿可 stage）。
    pub graph_dropped: bool,
    /// 重配前输出设备是否处于未打开（absent/掉线）状态。
    pub device_was_absent: bool,
    /// 回读校验过的真实 hw params。
    pub hw: HwReadback,
    /// 块计数（热重配不重置累计块数，保持音频时间连续）。
    pub blocks: u64,
    /// ALSA 统计是否因重开设备被清零（AlsaOutputBackend::start 重置统计）。
    pub alsa_stats_reset: bool,
    /// 输入抖动环/输入统计是否重建（热重配总重建输入，预热静音随后重新开始）。
    pub input_stats_reset: bool,
}

/// 热重配失败（已尽力按旧配置回滚；字段说明回滚后的真实状态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconfigureFailure {
    /// 稳定错误码（见 codes::RECONFIGURE_*）。
    pub code: &'static str,
    /// 失败阶段（validate/input/engine/driver/receiver_source/receiver_spawn/device_close/
    /// device_open/hw_verify）。
    pub stage: &'static str,
    /// 人读原因。
    pub message: String,
    /// 是否已按旧配置恢复（false = 设备/收包至少一项没回到旧状态）。
    pub rolled_back: bool,
    /// 回滚后输出设备是否已打开。
    pub device_started: bool,
    /// 回滚后收包线程是否在跑。
    pub receiver_running: bool,
}
/// Linux 上可直接出声的运行时（真实 ALSA PCM）。
#[cfg(target_os = "linux")]
pub type LinuxRuntime = Runtime<crate::alsa_backend::AlsaPcm>;
