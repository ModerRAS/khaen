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
use crate::aes67_input::{
    Aes67Input, Aes67InputConfig, Aes67InputStats, BlockStatus, FeedOutcome, InputError,
    L24_SAMPLE_BYTES,
};
use crate::alsa_backend::{AlsaOutputBackend, AlsaStats, PcmOutput};
use crate::backend::{AudioBackend, BackendError, DeviceConfig};
use crate::engine::{
    EngineConfig, EngineError, GraphInfo, MuteReceipt, Passthrough, Processor, Silent, SlotEngine,
};
use crate::graph::{CompileError, PlanGraph};
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
        let local = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, input.port);
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
    /// AES67 输入配置（组播地址/端口/通道映射/块长）。
    pub input: Aes67InputConfig,
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
        if self.input.output_channels != self.engine.channels
            || self.device.channels != self.engine.channels
        {
            return Err(RuntimeError::Config(
                "channels must match across input.output_channels, engine and device config",
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
    /// AES67 输入统计快照（含 jitter 水位/SSRC）。
    pub input: Aes67InputStats,
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
    /// 引擎实际静音状态（块边界后的值；用户请求与安全静音任一为真即为 true）。
    pub muted: bool,
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
    input: Aes67Input,
    engine: SlotEngine,
    backend: AlsaOutputBackend<P>,
    safety: SafetySupervisor,
    driver: Box<dyn DeviceDriver>,
    receiver: Aes67Receiver,
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
        let source = UdpPacketSource::bind(&config.input, &config.receiver)?;
        let driver = build_device_driver(&config)?;
        Self::build_validated(config, source, driver, true)
    }

    /// 构造运行时但**不打开输出设备**：设备缺失/掉线由控制路径 [`Runtime::open_device`] 重试。
    ///
    /// 与 [`Runtime::new`] 的唯一区别是构造期不做 `backend.start`：收包线程、引擎与缓冲照常
    /// 就位，[`Runtime::run_block`] 在设备打开前 fail-closed 返回 NotStarted（不推进输入/图状态、
    /// 不写设备）。用于"声卡不在位也要保持进程运行"的宿主（rt-host）。
    pub fn new_deferred(config: RuntimeConfig) -> Result<Self, RuntimeError> {
        config.validate()?;
        let source = UdpPacketSource::bind(&config.input, &config.receiver)?;
        let driver = build_device_driver(&config)?;
        Self::build_validated(config, source, driver, false)
    }

    /// 用注入的 PacketSource（假包源/其它 socket 实现）构造运行时。
    pub fn with_source<S: PacketSource + 'static>(
        config: RuntimeConfig,
        source: S,
    ) -> Result<Self, RuntimeError> {
        config.validate()?;
        let driver = build_device_driver(&config)?;
        Self::build_validated(config, source, driver, true)
    }

    /// [`Runtime::with_source`] 的"设备延迟打开"版本：构造期不碰设备（设备缺失模拟/重开测试用）。
    pub fn with_source_deferred<S: PacketSource + 'static>(
        config: RuntimeConfig,
        source: S,
    ) -> Result<Self, RuntimeError> {
        config.validate()?;
        let driver = build_device_driver(&config)?;
        Self::build_validated(config, source, driver, false)
    }

    /// 用注入的包源 + 设备驱动构造运行时（测试/daemon 注入用）。
    pub fn with_source_and_driver<S: PacketSource + 'static>(
        config: RuntimeConfig,
        source: S,
        driver: Box<dyn DeviceDriver>,
    ) -> Result<Self, RuntimeError> {
        config.validate()?;
        Self::build_validated(config, source, driver, true)
    }

    fn build_validated<S: PacketSource + 'static>(
        config: RuntimeConfig,
        source: S,
        driver: Box<dyn DeviceDriver>,
        open_device: bool,
    ) -> Result<Self, RuntimeError> {
        let input = Aes67Input::new(config.input.clone()).map_err(RuntimeError::Input)?;
        let initial: Box<dyn Processor + Send> = match config.unloaded_policy {
            UnloadedPolicy::Passthrough => Box::new(Passthrough),
            UnloadedPolicy::Silence => Box::new(Silent),
        };
        let engine =
            SlotEngine::new(config.engine.clone(), initial).map_err(RuntimeError::Engine)?;
        let mut backend: AlsaOutputBackend<P> = AlsaOutputBackend::new();
        if open_device {
            backend
                .start(&config.device)
                .map_err(RuntimeError::Backend)?;
        }
        let safety = SafetySupervisor::new(config.safety.clone());
        let receiver = Aes67Receiver::spawn(source, &config.receiver)?;
        let block = vec![0.0f32; input.samples_per_block()];
        let out_block = vec![0.0f32; engine.samples_per_block()];
        let packet_buf = vec![0u8; config.receiver.packet_bytes];
        Ok(Self {
            config,
            input,
            engine,
            backend,
            safety,
            driver,
            receiver,
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
        })
    }

    /// 配置视图。
    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    /// AES67 输入视图（诊断/测试用；RT 线程之外不要并发访问）。
    pub fn input(&self) -> &Aes67Input {
        &self.input
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

    /// 停止收包线程并 join；幂等。返回停止后的收包统计。
    pub fn stop_receiver(&mut self) -> Result<ReceiverStats, RuntimeError> {
        self.receiver.stop()
    }

    /// 收包线程是否在跑。
    pub fn receiver_running(&self) -> bool {
        self.receiver.is_running()
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
        let fresh_input =
            Aes67Input::new(self.config.input.clone()).map_err(RuntimeError::Input)?;
        self.backend
            .start(&self.config.device)
            .map_err(RuntimeError::Backend)?;
        // 设备打开后才丢弃断线期间的旧包：它们不属于恢复后的新抖动缓冲。
        while self.receiver.next_packet(&mut self.packet_buf).is_some() {
            // 收包统计仍在 receiver 侧累计；这里只把旧载荷从 SPSC 里抽走。
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
        // 1) 网络包 → 输入抖动缓冲。feed_packet 与 fill_block 都是 Aes67Input 声明的实时安全操作。
        while let Some(len) = self.receiver.next_packet(&mut self.packet_buf) {
            if let Ok(FeedOutcome::Overflow { .. }) =
                self.input.feed_packet(&self.packet_buf[..len])
            {
                self.consecutive_overruns = self.consecutive_overruns.saturating_add(1);
                if self.consecutive_overruns >= self.config.safety.max_consecutive_overruns {
                    self.note_fault(FaultReason::Overrun);
                }
            }
        }

        // 2) 取一块输入；非 Filled 即整块静音（fill_block 已把输入块清零）。
        let outcome = self.input.fill_block(&mut self.block);
        let input_filled = outcome.status == BlockStatus::Filled;
        if input_filled {
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

        // 4) 安全静音/输入静音：图已处理过本块（状态保持前进），输出整块清零。
        let muted = !input_filled || self.safety.requires_mute() || self.pending_fault.is_some();
        if muted {
            self.out_block.fill(0.0);
        }

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
        let receipt = self.engine.request_mute(mute);
        self.safety.set_user_mute(mute);
        receipt
    }

    /// 引擎当前实际静音状态（块边界后的值，来自块引擎统计）。
    pub fn muted(&self) -> bool {
        self.engine.stats().mute
    }

    /// 状态快照（非实时路径；会克隆设备监督器的错误文本）。
    pub fn status(&self) -> RuntimeStatus {
        let device = self.driver.status();
        let device_error = device.last_error.as_ref().map(|err| err.code.as_str());
        RuntimeStatus {
            state: self.safety.state(),
            blocks: self.blocks,
            input: self.input.stats(),
            output: self.backend.stats(),
            receiver: self.receiver.stats(),
            device_rate: device.device_rate,
            plan_rate: device.plan_rate,
            rate_asserted: device.rate_asserted,
            output_errors: self.output_errors,
            output_started: self.backend.is_started(),
            graph: self.engine.graph_info(),
            switch_pending: self.engine.switch_pending(),
            muted: self.engine.stats().mute,
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

/// Linux 上可直接出声的运行时（真实 ALSA PCM）。
#[cfg(target_os = "linux")]
pub type LinuxRuntime = Runtime<crate::alsa_backend::AlsaPcm>;
