//! AES67 UDP 接收端（[IO]）：组播/单播 socket + 收包线程 + 抖动缓冲。
//!
//! 线程模型（1 IO 线程 + 1 consumer 线程，无锁 SPSC）：
//!
//! ```text
//! [socket 组播/单播] ──recv_from（20 ms 超时轮询 stop）──► RTP 头解析
//!     ──► jitter::JitterWriter::push_packet（L24 解码直接写环，零分配）
//! consumer（host capture/RT 线程）──► jitter::JitterReader::read_block（128 帧整块）
//! ```
//!
//! - 启动期（`bind`）允许分配/开 socket/建线程/设 `SO_RCVBUF`/join 组播；
//! - 收包热路径：固定 2048 字节栈缓冲 + 纯函数解析 + 预分配环，无堆分配/无锁/无
//!   文件日志；错误（畸形包、socket 错误、丢弃）只累加原子计数，不阻塞收包；
//! - 线程优先级可配置（`ReceiverConfig::io_priority`）：Linux = SCHED_FIFO，
//!   Windows = `SetThreadPriority`；设置结果记录为 Applied/Failed（不阻断收包），
//!   真实 RT 权限/调度验证属板端 **NOT_RUN**；
//! - 时钟：离线用 `UsrvClock`（单调时基）；`PtpPhcClock` 明确 NOT_WIRED（板端接
//!   `/dev/ptp0` + T19 servo 锁状态后才允许开始采集）。
//!
//! 板端真实链路（网卡/IGMP/交换机 PTP、真实 H16 源、电平/静音窗口）**NOT_RUN**。
//!
//! task-320 监听面：接收接口支持单接口/多接口列表（组播 = 每个接口一条
//! `IP_ADD_MEMBERSHIP`，**绝不退回 INADDR_ANY**；单播 = bind 主接口）；可选
//! `group_allow`（只 join/接受列出的组；SDP 组不在列表 → fail closed）与
//! `source_allow`（IPv4/CIDR 来源白名单：收包线程在 RTP 解析/抖动缓冲之前丢弃并计数，
//! 不污染 jitter/underrun）。默认（空列表）与旧行为完全一致。

use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::clock::{MediaClock, UsrvClock};
use crate::jitter::{self, JitterConfig, JitterError, JitterReader, JitterStats};
use crate::rtp;
use crate::sdp::{self, SdpError, Session};
use crate::listen::{GroupAllow, ReceiverInterfaces, SourceAllow};
use crate::sockopt;

/// 单个 UDP 数据报最大接收长度（48 帧×8ch×3B + 12B 头 ≈ 1164 B；留 2 ms ptime 余量）。
pub const MAX_PACKET_BYTES: usize = 2_048;
/// 默认 `SO_RCVBUF` 请求值（AES67 1 ms 包突发；4 MiB 覆盖 ~50 ms×8ch 峰值）。
pub const DEFAULT_RCVBUF_BYTES: usize = 4 * 1024 * 1024;
/// 默认 `recv_from` 超时（ms）：决定 stop 轮询粒度（不参与媒体时钟）。
pub const DEFAULT_RECV_TIMEOUT_MS: u64 = 20;

/// 媒体时钟来源（接收端）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiverClock {
    /// 用户态单调时钟（离线/loopback；已验证）。
    Usrv,
    /// PTP 硬件时钟（`/dev/ptp0` PHC）：**NOT_WIRED**，板端接。
    PtpPhc {
        /// PTP domain（SDP `a=ts-refclk` 声明）。
        domain: u8,
    },
}

impl ReceiverClock {
    /// 稳定名称。
    pub const fn name(self) -> &'static str {
        match self {
            ReceiverClock::Usrv => "usrvclock",
            ReceiverClock::PtpPhc { .. } => "ptp-phc(not-wired)",
        }
    }
}

/// 接收端配置（启动期固定）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiverConfig {
    /// 媒体目的地址（SDP `c=`）。
    pub media: Ipv4Addr,
    /// 媒体 UDP 端口（SDP `m=audio`；单播测试允许 0 = 临时端口）。
    pub port: u16,
    /// 是否组播（SDP 地址 224/4）。
    pub multicast: bool,
    /// 绑定/加入网卡地址集合（组播 = 每个接口一条 join；单播 = bind 主接口；
    /// 空 = 旧行为 0.0.0.0）。
    pub interfaces: ReceiverInterfaces,
    /// 组播组白名单（空 = 旧行为；非空时 SDP 组必须在列表内，否则 fail closed）。
    pub group_allow: GroupAllow,
    /// RTP 来源白名单（IPv4/CIDR；空 = 旧行为：接受任意来源）。
    pub source_allow: SourceAllow,
    /// 媒体通道数（SDP rtpmap；1..=8）。
    pub channels: u16,
    /// `SO_RCVBUF` 请求值（字节；0 = 不调优）。
    pub rcvbuf_bytes: usize,
    /// `recv_from` 超时（ms）。
    pub recv_timeout_ms: u64,
    /// IO 线程优先级（None = 不设置；Linux SCHED_FIFO / Windows 映射）。
    pub io_priority: Option<i32>,
    /// 媒体时钟来源。
    pub clock: ReceiverClock,
    /// 抖动缓冲配置。
    pub jitter: JitterConfig,
    /// 可选 SSRC 过滤（Some 时只接受该 SSRC 的包；用于同端口多源/保活与测试流共存时隔离）。
    pub ssrc_filter: Option<u32>,
}

impl ReceiverConfig {
    /// 由已校验 SDP 会话构造默认配置（48k/L24/单 flow；单接口兼容入口）。
    pub fn from_session(
        session: &Session<'_>,
        interface: Option<Ipv4Addr>,
    ) -> Result<Self, ReceiverError> {
        let interfaces = match interface {
            Some(address) => ReceiverInterfaces::from_addr(address),
            None => ReceiverInterfaces::EMPTY,
        };
        Self::from_session_with_interfaces(session, interfaces)
    }

    /// 由已校验 SDP 会话 + 接口集合构造默认配置（多接口/网卡名解析后的入口）。
    ///
    /// 组白名单/来源白名单默认空（旧行为）；调用方按配置赋值后再 `validate()`
    /// （或直接交给 `bind()`，内部会校验）。
    pub fn from_session_with_interfaces(
        session: &Session<'_>,
        interfaces: ReceiverInterfaces,
    ) -> Result<Self, ReceiverError> {
        session
            .validate_media_profile()
            .map_err(ReceiverError::Sdp)?;
        let connection = session
            .connection()
            .ok_or(ReceiverError::Sdp(SdpError::MissingConnection))?;
        let channels = session.media.rtpmap.channels;
        let jitter = JitterConfig::aes67_default(channels).map_err(ReceiverError::Jitter)?;
        let config = Self {
            media: connection.ipv4(),
            port: session.media.port,
            multicast: connection.is_multicast(),
            interfaces,
            group_allow: GroupAllow::EMPTY,
            source_allow: SourceAllow::EMPTY,
            channels,
            rcvbuf_bytes: DEFAULT_RCVBUF_BYTES,
            recv_timeout_ms: DEFAULT_RECV_TIMEOUT_MS,
            io_priority: None,
            clock: ReceiverClock::Usrv,
            jitter,
            ssrc_filter: None,
        };
        config.validate()?;
        Ok(config)
    }

    /// 校验（组播端口必须非 0；时钟必须已接线）。
    pub fn validate(&self) -> Result<(), ReceiverError> {
        if self.multicast && self.port == 0 {
            return Err(ReceiverError::Config);
        }
        if self.channels == 0 || self.channels > jitter::MAX_CHANNELS as u16 {
            return Err(ReceiverError::Config);
        }
        if self.channels != self.jitter.channels {
            return Err(ReceiverError::Config);
        }
        if self.recv_timeout_ms == 0 {
            return Err(ReceiverError::Config);
        }
        if matches!(self.clock, ReceiverClock::PtpPhc { .. }) {
            return Err(ReceiverError::ClockNotWired);
        }
        // task-320：组白名单只对组播有意义；非空时必须命中 SDP 组（fail closed）。
        if !self.group_allow.is_empty() {
            if !self.multicast {
                return Err(ReceiverError::Config);
            }
            if !self.group_allow.contains(self.media) {
                return Err(ReceiverError::GroupNotAllowed);
            }
        }
        // 单播只能 bind 一个地址；多接口仅组播 join 有意义（不静默只用主接口）。
        if !self.multicast && self.interfaces.len() > 1 {
            return Err(ReceiverError::Config);
        }
        self.jitter.validate().map_err(ReceiverError::Jitter)?;
        Ok(())
    }
}

/// 接收端错误（Copy 名称；I/O 错误保留 source）。
#[derive(Debug)]
pub enum ReceiverError {
    /// 配置非法。
    Config,
    /// SDP 媒体 profile 不可接受。
    Sdp(SdpError),
    /// 抖动缓冲配置/布局错误。
    Jitter(JitterError),
    /// socket 创建/绑定/选项失败。
    Socket(io::Error),
    /// 组播加入失败（网卡/地址/权限）。
    MulticastJoin(io::Error),
    /// IO 线程创建失败。
    Thread,
    /// PTP 时钟尚未接线（NOT_WIRED）。
    ClockNotWired,
    /// 组播组不在 `group_allow` 白名单（fail closed；不 join、不收包）。
    GroupNotAllowed,
    /// 组播"只接受已 join 组"限制设置失败（Linux `IP_MULTICAST_ALL=0`；
    /// `group_allow` 非空时 fail closed，空时按旧行为继续并在报告里如实记录）。
    MulticastRestrict(io::Error),
}

impl ReceiverError {
    /// 稳定错误名（证据/日志/JSON 用）。
    pub const fn name(&self) -> &'static str {
        match self {
            ReceiverError::Config => "ERR_RX_CONFIG",
            ReceiverError::Sdp(_) => "ERR_RX_SDP",
            ReceiverError::Jitter(_) => "ERR_RX_JITTER",
            ReceiverError::Socket(_) => "ERR_RX_SOCKET",
            ReceiverError::MulticastJoin(_) => "ERR_RX_MULTICAST_JOIN",
            ReceiverError::Thread => "ERR_RX_THREAD",
            ReceiverError::ClockNotWired => "ERR_RX_CLOCK_NOT_WIRED",
            ReceiverError::GroupNotAllowed => "ERR_RX_GROUP_NOT_ALLOWED",
            ReceiverError::MulticastRestrict(_) => "ERR_RX_MULTICAST_RESTRICT",
        }
    }
}

/// IO 线程计数快照（Copy；非阻塞）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IoStats {
    /// UDP 数据报数。
    pub datagrams: u64,
    /// 字节数。
    pub bytes: u64,
    /// RTP 头合法的包数（含后续 malformed payload）。
    pub rtp_packets: u64,
    /// 成功写入抖动缓冲的包数。
    pub accepted_packets: u64,
    /// 写入环的帧数。
    pub accepted_frames: u64,
    /// 被 SSRC 过滤丢弃的包数。
    pub filtered_ssrc: u64,
    /// 被来源白名单（`source_allow`）丢弃的包数（RTP 解析/Jitter 之前；不进 jitter 统计）。
    pub filtered_source: u64,
    /// 丢弃包（过期/溢出/重复/late）。
    pub dropped_packets: u64,
    /// RTP 头/负载畸形包数。
    pub malformed_packets: u64,
    /// 超过接收缓冲（截断）的数据报数（诊断）。
    pub oversized_datagrams: u64,
    /// socket 接收错误次数（不致命，继续循环）。
    pub socket_errors: u64,
}

/// IO 线程原子计数（Arc 共享；热路径只 fetch_add）。
#[derive(Debug, Default)]
struct IoCounters {
    datagrams: AtomicU64,
    bytes: AtomicU64,
    rtp_packets: AtomicU64,
    accepted_packets: AtomicU64,
    accepted_frames: AtomicU64,
    filtered_ssrc: AtomicU64,
    filtered_source: AtomicU64,
    dropped_packets: AtomicU64,
    malformed_packets: AtomicU64,
    oversized_datagrams: AtomicU64,
    socket_errors: AtomicU64,
}

impl IoCounters {
    fn snapshot(&self) -> IoStats {
        use Ordering::Relaxed;
        IoStats {
            datagrams: self.datagrams.load(Relaxed),
            bytes: self.bytes.load(Relaxed),
            rtp_packets: self.rtp_packets.load(Relaxed),
            accepted_packets: self.accepted_packets.load(Relaxed),
            accepted_frames: self.accepted_frames.load(Relaxed),
            filtered_ssrc: self.filtered_ssrc.load(Relaxed),
            filtered_source: self.filtered_source.load(Relaxed),
            dropped_packets: self.dropped_packets.load(Relaxed),
            malformed_packets: self.malformed_packets.load(Relaxed),
            oversized_datagrams: self.oversized_datagrams.load(Relaxed),
            socket_errors: self.socket_errors.load(Relaxed),
        }
    }
}

/// 接收端统计快照（IO + 抖动缓冲）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReceiverStats {
    /// socket/收包线程计数。
    pub io: IoStats,
    /// 抖动缓冲/播放计数。
    pub jitter: JitterStats,
}

/// 组播"只接受本 socket 已 join 的组"内核限制状态（证据 JSON 用；task-320）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MulticastRestrict {
    /// 非组播 flow（不适用）。
    NotApplicable,
    /// 已禁止接收未 join 的组（Linux `IP_MULTICAST_ALL=0`）。
    Restricted,
    /// 平台无该选项（Windows：内核只交付本 socket join 的组）。
    PlatformDefault,
    /// 设置失败但按旧行为继续（仅 `group_allow` 为空时；报告如实记录）。
    FailedOpen,
}

impl MulticastRestrict {
    /// 稳定名称（证据 JSON）。
    pub const fn name(self) -> &'static str {
        match self {
            MulticastRestrict::NotApplicable => "not_applicable",
            MulticastRestrict::Restricted => "restricted",
            MulticastRestrict::PlatformDefault => "platform_default",
            MulticastRestrict::FailedOpen => "failed_open",
        }
    }
}

/// IO 线程优先级状态（绑定后异步设置，见 `Aes67Receiver::priority_status`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PriorityStatus {
    /// 未配置优先级。
    NotConfigured,
    /// 线程已创建、优先级设置尚未完成（短暂瞬态）。
    Pending,
    /// 优先级已应用（SCHED_FIFO / SetThreadPriority 成功）。
    Applied,
    /// 优先级设置失败（失败原因名；不阻断收包）。
    Failed(&'static str),
}

/// 线程优先级错误。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PriorityError {
    /// 平台未接线。
    Unsupported,
    /// 系统调用失败（权限/策略）。
    Failed,
}

impl PriorityError {
    /// 稳定名称。
    pub const fn name(self) -> &'static str {
        match self {
            PriorityError::Unsupported => "ERR_PRIORITY_UNSUPPORTED",
            PriorityError::Failed => "ERR_PRIORITY_FAILED",
        }
    }
}

/// 当前线程设置实时优先级（启动期一次；Linux SCHED_FIFO / Windows 映射）。
#[cfg(target_os = "linux")]
pub fn set_current_thread_priority(priority: i32) -> Result<(), PriorityError> {
    #[repr(C)]
    struct SchedParam {
        sched_priority: i32,
    }
    extern "C" {
        fn pthread_self() -> usize;
        fn pthread_setschedparam(thread: usize, policy: i32, param: *const SchedParam) -> i32;
    }
    const SCHED_FIFO: i32 = 1;
    let param = SchedParam {
        sched_priority: priority,
    };
    // SAFETY: pthread_self 无参数返回当前线程；pthread_setschedparam 读取只读 param。
    let result = unsafe { pthread_setschedparam(pthread_self(), SCHED_FIFO, &param) };
    if result == 0 {
        Ok(())
    } else {
        Err(PriorityError::Failed)
    }
}

/// 当前线程设置接收 IO 优先级（Windows：`SetThreadPriority`；数值映射见实现）。
#[cfg(windows)]
pub fn set_current_thread_priority(priority: i32) -> Result<(), PriorityError> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThread() -> isize;
        fn SetThreadPriority(handle: isize, priority: i32) -> i32;
    }
    const THREAD_PRIORITY_ABOVE_NORMAL: i32 = 1;
    const THREAD_PRIORITY_HIGHEST: i32 = 2;
    const THREAD_PRIORITY_TIME_CRITICAL: i32 = 15;
    let mapped = if priority >= 90 {
        THREAD_PRIORITY_TIME_CRITICAL
    } else if priority >= 70 {
        THREAD_PRIORITY_HIGHEST
    } else {
        THREAD_PRIORITY_ABOVE_NORMAL
    };
    // SAFETY: GetCurrentThread 返回伪句柄；SetThreadPriority 只作用于当前线程。
    let result = unsafe { SetThreadPriority(GetCurrentThread(), mapped) };
    if result != 0 {
        Ok(())
    } else {
        Err(PriorityError::Failed)
    }
}

/// 其它平台：不接线。
#[cfg(not(any(windows, target_os = "linux")))]
pub fn set_current_thread_priority(_priority: i32) -> Result<(), PriorityError> {
    Err(PriorityError::Unsupported)
}

/// 收包线程状态（stop 位 + join 句柄）。
struct IoThread {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl IoThread {
    fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for IoThread {
    fn drop(&mut self) {
        self.stop();
    }
}

/// AES67 用户态接收端：socket + 收包线程 + 抖动缓冲读端。
pub struct Aes67Receiver {
    reanchor_req: std::sync::Arc<std::sync::atomic::AtomicBool>,
    reader: Option<JitterReader>,
    /// 证据用副本：`take_reader` 交给 capture 线程后，`report_json`/`jitter_stats`
    /// 仍能读到真实统计（Arc 语义，reader 句柄 Clone 无代价）。
    report_reader: Option<JitterReader>,
    io_thread: IoThread,
    counters: Arc<IoCounters>,
    priority: Arc<AtomicU8>,
    priority_error: Option<&'static str>,
    local_addr: SocketAddr,
    rcvbuf_applied: usize,
    multicast_restrict: MulticastRestrict,
    config: ReceiverConfig,
    clock: UsrvClock,
}

// 优先级状态原子编码（0=Pending, 1=Applied, 2=Failed）。
const PRIORITY_PENDING: u8 = 0;
const PRIORITY_APPLIED: u8 = 1;
const PRIORITY_FAILED: u8 = 2;

impl std::fmt::Debug for Aes67Receiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Aes67Receiver")
            .field("local_addr", &self.local_addr)
            .field("config", &self.config)
            .field("rcvbuf_applied", &self.rcvbuf_applied)
            .finish()
    }
}

impl Aes67Receiver {
    /// 绑定 socket、加入组播（如需要）、分配抖动环并启动收包线程。
    pub fn bind(config: ReceiverConfig) -> Result<Self, ReceiverError> {
        config.validate()?;
        if matches!(config.clock, ReceiverClock::PtpPhc { .. }) {
            return Err(ReceiverError::ClockNotWired);
        }
        let (writer, reader) = jitter::channel(config.jitter).map_err(ReceiverError::Jitter)?;
        let clock = UsrvClock::new(config.jitter.rate_hz);

        // 组播 socket 必须 bind 通配地址（0.0.0.0:port），否则只收得到绑定接口的流量；
        // 接口约束由 IP_ADD_MEMBERSHIP 的 imr_interface 负责（见下）。
        let bind_addr = if config.multicast {
            SocketAddr::from((Ipv4Addr::UNSPECIFIED, config.port))
        } else {
            SocketAddr::from((
                config.interfaces.primary().unwrap_or(Ipv4Addr::UNSPECIFIED),
                config.port,
            ))
        };
        let socket = UdpSocket::bind(bind_addr).map_err(ReceiverError::Socket)?;
        socket
            .set_read_timeout(Some(Duration::from_millis(config.recv_timeout_ms)))
            .map_err(ReceiverError::Socket)?;
        let rcvbuf_applied = if config.rcvbuf_bytes > 0 {
            match sockopt::set_receive_buffer(&socket, config.rcvbuf_bytes) {
                Ok(applied) => applied,
                Err(_) => 0,
            }
        } else {
            0
        };
        let multicast_restrict = if config.multicast {
            // 先关"接收本机其它 socket join 的组"，再 join（Linux 默认开启会带来
            // "来什么收什么" 的另一半问题；group_allow 非空时这是硬要求）。
            match sockopt::exclude_unjoined_multicast(&socket) {
                Ok(true) => MulticastRestrict::Restricted,
                Ok(false) => MulticastRestrict::PlatformDefault,
                Err(_) if config.group_allow.is_empty() => MulticastRestrict::FailedOpen,
                Err(error) => return Err(ReceiverError::MulticastRestrict(error)),
            }
        } else {
            MulticastRestrict::NotApplicable
        };
        if config.multicast {
            let group = config.media;
            if config.interfaces.is_empty() {
                // 旧行为：空配置 = 系统默认接口（0.0.0.0）。显式配置时绝不退回这里。
                socket
                    .join_multicast_v4(&group, &Ipv4Addr::UNSPECIFIED)
                    .map_err(ReceiverError::MulticastJoin)?;
            } else {
                // 每个配置接口一条显式 IP_ADD_MEMBERSHIP；任一失败 = fail closed（不静默
                // 只用主接口、不退回 INADDR_ANY）。
                for interface in config.interfaces.addresses() {
                    socket
                        .join_multicast_v4(&group, interface)
                        .map_err(ReceiverError::MulticastJoin)?;
                }
            }
            // loopback 便于本机集成测试；生产默认也开（不影响跨机收包）。
            let _ = socket.set_multicast_loop_v4(true);
        }
        let local_addr = socket.local_addr().map_err(ReceiverError::Socket)?;

        let reanchor_req = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(IoCounters::default());
        let priority_state = Arc::new(AtomicU8::new(PRIORITY_PENDING));
        let stop = Arc::new(AtomicBool::new(false));
        let priority_config = config.io_priority;
        let priority_error = match priority_config {
            Some(priority) => {
                // 启动期先试一次（主线程），记录是否支持；实际 IO 线程仍会重试。
                match set_current_thread_priority(priority) {
                    Ok(()) => None,
                    Err(error) => Some(error.name()),
                }
            }
            None => None,
        };
        let io_counters = Arc::clone(&counters);
        let io_reanchor = Arc::clone(&reanchor_req);
        let io_stop = Arc::clone(&stop);
        let io_priority_state = Arc::clone(&priority_state);
        let handle = thread::Builder::new()
            .name("aes67-rx-io".to_string())
            .spawn(move || {
                run_io_loop(
                    socket,
                    writer,
                    clock,
                    config.ssrc_filter,
                    config.source_allow,
                    io_reanchor,
                    io_stop,
                    io_counters,
                    io_priority_state,
                    priority_config,
                );
            })
            .map_err(|_| ReceiverError::Thread)?;
        if priority_config.is_none() {
            priority_state.store(PRIORITY_APPLIED, Ordering::Relaxed);
        }
        Ok(Self {
            reanchor_req,
            reader: Some(reader),
            report_reader: None,
            io_thread: IoThread {
                stop,
                handle: Some(handle),
            },
            counters,
            priority: priority_state,
            priority_error,
            local_addr,
            rcvbuf_applied,
            multicast_restrict,
            config,
            clock,
        })
    }

    /// 请求内部重锚（下一个包作为新流首包）。用于 stay 模式流间隙恢复，避免重启进程。
    pub fn request_reanchor(&self) {
        self.reanchor_req.store(true, std::sync::atomic::Ordering::Release);
    }

    /// 重锚请求标志（可跨线程 clone，用于播放线程在流间隙时请求重锚）。
    pub fn reanchor_flag(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        std::sync::Arc::clone(&self.reanchor_req)
    }

    /// 本地绑定地址（测试/证据用；端口 0 时给出内核分配端口）。
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// 配置。
    pub fn config(&self) -> ReceiverConfig {
        self.config
    }

    /// 媒体时钟（UsrvClock；离线路径）。
    pub fn clock(&self) -> &UsrvClock {
        &self.clock
    }

    /// 取出抖动缓冲读端（交给 capture/RT 线程；只能取一次）。
    ///
    /// 取出后本接收端仍保留一个统计副本（`report_reader`），`report_json` 不会退化为
    /// 零统计（证据真实性）。
    pub fn take_reader(&mut self) -> Option<JitterReader> {
        let taken = self.reader.take();
        self.report_reader = taken.clone();
        taken
    }

    /// 抖动缓冲读端引用（未取出时）。
    pub fn reader(&self) -> Option<&JitterReader> {
        self.reader.as_ref()
    }

    /// 实际生效的 `SO_RCVBUF`（0 = 未调优/失败）。
    pub fn rcvbuf_applied(&self) -> usize {
        self.rcvbuf_applied
    }

    /// IO 线程优先级状态（启动后异步置位）。
    pub fn priority_status(&self) -> PriorityStatus {
        match self.config.io_priority {
            None => PriorityStatus::NotConfigured,
            Some(_) => match self.priority.load(Ordering::Acquire) {
                PRIORITY_APPLIED => PriorityStatus::Applied,
                PRIORITY_FAILED => {
                    PriorityStatus::Failed(self.priority_error.unwrap_or("ERR_PRIORITY_FAILED"))
                }
                _ => PriorityStatus::Pending,
            },
        }
    }

    /// IO 计数快照。
    pub fn io_stats(&self) -> IoStats {
        self.counters.snapshot()
    }

    /// 抖动缓冲统计快照（reader 已交给 capture 线程后仍可读）。
    pub fn jitter_stats(&self) -> JitterStats {
        match self.reader.as_ref().or(self.report_reader.as_ref()) {
            Some(reader) => reader.stats(),
            None => JitterStats::default(),
        }
    }

    /// 组合统计快照。
    pub fn stats(&self) -> ReceiverStats {
        ReceiverStats {
            io: self.io_stats(),
            jitter: self.jitter_stats(),
        }
    }

    /// 停止收包线程并等待退出（幂等；退出路径 ≤ recv_timeout）。
    pub fn stop(&mut self) {
        self.io_thread.stop();
    }

    /// 证据 JSON（NONRT；调用方在收流结束后调用；不参与热路径）。
    pub fn report_json(&self) -> String {
        let io = self.io_stats();
        let jitter = match self.reader.as_ref().or(self.report_reader.as_ref()) {
            Some(reader) => reader.stats(),
            None => JitterStats::default(),
        };
        let media = format!("{}:{}", self.config.media, self.config.port);
        let interface = match self.config.interfaces.primary() {
            Some(address) => address.to_string(),
            None => "0.0.0.0".to_string(),
        };
        let interfaces = self
            .config
            .interfaces
            .addresses()
            .iter()
            .map(|address| format!("\"{address}\""))
            .collect::<Vec<_>>()
            .join(",");
        let group_allow = self
            .config
            .group_allow
            .groups()
            .iter()
            .map(|group| format!("\"{group}\""))
            .collect::<Vec<_>>()
            .join(",");
        let source_allow = self
            .config
            .source_allow
            .cidrs()
            .iter()
            .map(|cidr| format!("\"{cidr}\""))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"proto\":\"aes67-rx-proto\",\"version\":\"{}\",\"transport\":\"{}\",\
\"media\":\"{}\",\"interface\":\"{}\",\"interfaces\":[{}],\"group_allow\":[{}],\
\"source_allow\":[{}],\"multicast_restrict\":\"{}\",\"channels\":{},\"rate_hz\":{},\
\"block_frames\":{},\"capacity_frames\":{},\"latency_frames\":{},\
\"rcvbuf_requested\":{},\"rcvbuf_applied\":{},\"clock\":\"{}\",\
\"io_priority\":{},\"priority_state\":\"{}\",\"io\":{{\"datagrams\":{},\
\"bytes\":{},\"rtp_packets\":{},\"accepted_packets\":{},\"accepted_frames\":{},\
\"filtered_ssrc\":{},\"filtered_source\":{},\"dropped_packets\":{},\"malformed_packets\":{},\
\"oversized_datagrams\":{},\
\"socket_errors\":{}}},\"jitter\":{{\"packets\":{},\"frames_received\":{},\
\"frames_played\":{},\"underruns\":{},\"warmups\":{},\"not_due\":{},\
\"overruns\":{},\"late_packets\":{},\"late_frames\":{},\"lost_packets\":{},\
\"duplicate_packets\":{},\"out_of_order_packets\":{},\"timestamp_jumps\":{},\
\"malformed_packets\":{},\"stream_restarts\":{},\"resync_blocks\":{},\
\"high_water_frames\":{}}}}}",
            crate::PROTO_VERSION,
            if self.config.multicast {
                "multicast"
            } else {
                "unicast"
            },
            media,
            interface,
            interfaces,
            group_allow,
            source_allow,
            self.multicast_restrict.name(),
            self.config.channels,
            self.config.jitter.rate_hz,
            self.config.jitter.block_frames,
            self.config.jitter.capacity_frames,
            self.config.jitter.latency_frames(),
            self.config.rcvbuf_bytes,
            self.rcvbuf_applied,
            self.config.clock.name(),
            match self.config.io_priority {
                Some(priority) => format!("\"{priority}\""),
                None => "null".to_string(),
            },
            match self.priority_status() {
                PriorityStatus::NotConfigured => "not_configured",
                PriorityStatus::Pending => "pending",
                PriorityStatus::Applied => "applied",
                PriorityStatus::Failed(_) => "failed",
            },
            io.datagrams,
            io.bytes,
            io.rtp_packets,
            io.accepted_packets,
            io.accepted_frames,
            io.filtered_ssrc,
            io.filtered_source,
            io.dropped_packets,
            io.malformed_packets,
            io.oversized_datagrams,
            io.socket_errors,
            jitter.packets,
            jitter.frames_received,
            jitter.frames_played,
            jitter.underruns,
            jitter.warmups,
            jitter.not_due,
            jitter.overruns,
            jitter.late_packets,
            jitter.late_frames,
            jitter.lost_packets,
            jitter.duplicate_packets,
            jitter.out_of_order_packets,
            jitter.timestamp_jumps,
            jitter.malformed_packets,
            jitter.stream_restarts,
            jitter.resync_blocks,
            jitter.high_water_frames,
        )
    }
}

/// IO 收包线程主体（热路径：无堆分配/无锁/无格式化）。
fn run_io_loop(
    socket: UdpSocket,
    writer: jitter::JitterWriter,
    clock: UsrvClock,
    ssrc_filter: Option<u32>,
    source_allow: SourceAllow,
    reanchor_req: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    counters: Arc<IoCounters>,
    priority_state: Arc<AtomicU8>,
    requested_priority: Option<i32>,
) {
    if let Some(priority) = requested_priority {
        match set_current_thread_priority(priority) {
            Ok(()) => priority_state.store(PRIORITY_APPLIED, Ordering::Release),
            Err(_) => priority_state.store(PRIORITY_FAILED, Ordering::Release),
        }
    }
    let mut buffer = [0u8; MAX_PACKET_BYTES];
    while !stop.load(Ordering::Acquire) {
        if reanchor_req.swap(false, Ordering::AcqRel) {
            writer.request_reanchor();
        }
        match socket.recv_from(&mut buffer) {
            Ok((length, source)) => {
                counters.datagrams.fetch_add(1, Ordering::Relaxed);
                counters.bytes.fetch_add(length as u64, Ordering::Relaxed);
                if length == MAX_PACKET_BYTES {
                    counters.oversized_datagrams.fetch_add(1, Ordering::Relaxed);
                }
                // task-320：来源白名单在 RTP 解析/抖动缓冲之前丢弃（不污染 jitter/underrun）。
                if !source_allow.is_empty() && !source_allow.contains_ip(source.ip()) {
                    counters.filtered_source.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                match rtp::Header::parse(&buffer[..length]) {
                    Ok(parsed) => {
                        counters.rtp_packets.fetch_add(1, Ordering::Relaxed);
                        let header = parsed.header;
                        if let Some(expected) = ssrc_filter {
                            if header.ssrc != expected {
                                counters.filtered_ssrc.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
                        }
                        let payload = parsed.payload(&buffer[..length]);
                        let now_ns = clock.now_ns();
                        match writer.push_packet(now_ns, &header, payload) {
                            jitter::PushResult::Accepted { frames }
                            | jitter::PushResult::Restarted { frames } => {
                                counters.accepted_packets.fetch_add(1, Ordering::Relaxed);
                                counters
                                    .accepted_frames
                                    .fetch_add(frames as u64, Ordering::Relaxed);
                            }
                            _ => {
                                counters.dropped_packets.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                    Err(_) => {
                        counters.malformed_packets.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            Err(error) => match error.kind() {
                io::ErrorKind::WouldBlock
                | io::ErrorKind::TimedOut
                | io::ErrorKind::Interrupted => {}
                _ => {
                    // socket 错误不终止收包（继续尝试；错误计数不阻塞）。
                    counters.socket_errors.fetch_add(1, Ordering::Relaxed);
                }
            },
        }
    }
}

/// 由 SDP 文档直接绑定接收端（文件输入 → 校验 → socket）。
pub fn bind_from_sdp(
    document: &sdp::SdpDocument,
    interface: Option<Ipv4Addr>,
) -> Result<Aes67Receiver, ReceiverError> {
    let session = document.parse().map_err(ReceiverError::Sdp)?;
    let config = ReceiverConfig::from_session(&session, interface)?;
    Aes67Receiver::bind(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jitter::JitterConfig;
    use crate::listen::Ipv4Cidr;
    use crate::rtp::Header;
    use std::time::{Duration, Instant};

    fn unicast_config(port: u16, channels: u16) -> ReceiverConfig {
        ReceiverConfig {
            media: Ipv4Addr::LOCALHOST,
            port,
            multicast: false,
            interfaces: ReceiverInterfaces::from_addr(Ipv4Addr::LOCALHOST),
            group_allow: GroupAllow::EMPTY,
            source_allow: SourceAllow::EMPTY,
            channels,
            rcvbuf_bytes: 1 << 20,
            recv_timeout_ms: 10,
            io_priority: None,
            clock: ReceiverClock::Usrv,
            jitter: JitterConfig::new(channels, 48_000, 128, 8_192, 4_000).expect("jitter config"),
            ssrc_filter: None,
        }
    }

    fn wait_until<F: Fn() -> bool>(deadline: Duration, predicate: F) -> bool {
        let start = Instant::now();
        while start.elapsed() < deadline {
            if predicate() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        predicate()
    }

    #[test]
    fn receiver_unicast_loopback_receives_and_anchors() {
        let mut receiver = Aes67Receiver::bind(unicast_config(0, 2)).expect("bind");
        let port = receiver.local_addr().port();
        assert!(port > 0);
        let mut sender = crate::sender::Aes67RtpSender::bind(crate::sender::SenderConfig::unicast(
            SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            2,
        ))
        .expect("sender");
        for _ in 0..4 {
            sender.send_silence().expect("send");
        }
        let receiver = &mut receiver;
        assert!(wait_until(Duration::from_millis(500), || {
            receiver.io_stats().accepted_packets >= 4
        }));
        let stats = receiver.io_stats();
        assert!(stats.datagrams >= 4, "{stats:?}");
        assert!(stats.accepted_packets >= 4, "{stats:?}");
        let jitter = receiver.jitter_stats();
        assert!(jitter.frames_received >= 192, "{jitter:?}");
        let report = receiver.report_json();
        assert!(report.contains("\"transport\":\"unicast\""));
        assert!(report.contains("\"clock\":\"usrvclock\""));
        receiver.stop();
    }

    #[test]
    fn receiver_multicast_join_and_bind() {
        // 不依赖组播回环投递（Windows 上可能被策略拦截）；只验证 bind/join 成功。
        let config = ReceiverConfig {
            media: Ipv4Addr::new(239, 255, 42, 99),
            port: 15_432,
            multicast: true,
            interfaces: ReceiverInterfaces::from_addr(Ipv4Addr::LOCALHOST),
            group_allow: GroupAllow::EMPTY,
            source_allow: SourceAllow::EMPTY,
            channels: 2,
            rcvbuf_bytes: 1 << 20,
            recv_timeout_ms: 10,
            io_priority: None,
            clock: ReceiverClock::Usrv,
            jitter: JitterConfig::aes67_default(2).expect("jitter"),
            ssrc_filter: None,
        };
        let mut receiver = Aes67Receiver::bind(config).expect("multicast bind+join");
        assert!(receiver.rcvbuf_applied() > 0);
        assert_eq!(receiver.local_addr().port(), 15_432);
        let report = receiver.report_json();
        // task-320：显式接口列表 + join 绑定证据（单接口 = 只有它，不得出现 0.0.0.0 之外的兜底）。
        assert!(report.contains("\"interfaces\":[\"127.0.0.1\"]"), "{report}");
        assert!(report.contains("\"group_allow\":[]"), "{report}");
        assert!(report.contains("\"source_allow\":[]"), "{report}");
        // Linux = restricted（IP_MULTICAST_ALL=0）；Windows = platform_default（内核按成员交付）。
        assert!(
            report.contains("\"multicast_restrict\":\"restricted\"")
                || report.contains("\"multicast_restrict\":\"platform_default\""),
            "{report}"
        );
        receiver.stop();
    }

    #[test]
    fn receiver_group_allow_and_interface_list_validation() {
        // 组播：SDP 组不在白名单 → ERR_RX_GROUP_NOT_ALLOWED（fail closed，不 join）。
        let mut config = ReceiverConfig {
            media: Ipv4Addr::new(239, 255, 42, 98),
            port: 15_433,
            multicast: true,
            interfaces: ReceiverInterfaces::from_addr(Ipv4Addr::LOCALHOST),
            group_allow: GroupAllow::try_from_groups(&[Ipv4Addr::new(239, 255, 42, 99)])
                .expect("group allow"),
            source_allow: SourceAllow::EMPTY,
            channels: 2,
            rcvbuf_bytes: 1 << 20,
            recv_timeout_ms: 10,
            io_priority: None,
            clock: ReceiverClock::Usrv,
            jitter: JitterConfig::aes67_default(2).expect("jitter"),
            ssrc_filter: None,
        };
        assert_eq!(
            config.validate().unwrap_err().name(),
            "ERR_RX_GROUP_NOT_ALLOWED"
        );
        assert_eq!(
            Aes67Receiver::bind(config).unwrap_err().name(),
            "ERR_RX_GROUP_NOT_ALLOWED"
        );
        config.group_allow = GroupAllow::try_from_groups(&[Ipv4Addr::new(239, 255, 42, 98)])
            .expect("group allow");
        config.validate().expect("组命中白名单必须通过");
        // 单播 + 组白名单 = 配置错误（组白名单只对组播有语义）。
        let mut unicast = unicast_config(0, 2);
        unicast.group_allow =
            GroupAllow::try_from_groups(&[Ipv4Addr::new(239, 255, 42, 98)]).expect("group allow");
        assert_eq!(unicast.validate().unwrap_err().name(), "ERR_RX_CONFIG");
        // 单播 + 多接口 = 配置错误（不静默只用主接口）。
        let mut unicast = unicast_config(0, 2);
        unicast.interfaces = ReceiverInterfaces::try_from_addresses(&[
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::new(127, 0, 0, 2),
        ])
        .expect("dual interface");
        assert_eq!(unicast.validate().unwrap_err().name(), "ERR_RX_CONFIG");
        // 来源白名单本身允许（含 /0 全放行）；命中/未命中由下一组收包测试覆盖。
        let mut unicast = unicast_config(0, 2);
        unicast.source_allow =
            SourceAllow::try_from_cidrs(&[Ipv4Cidr::parse("192.0.2.0/24").expect("cidr")])
                .expect("source allow");
        unicast.validate().expect("来源白名单对单播合法");
    }

    #[test]
    fn receiver_source_allow_drops_foreign_sources_before_jitter() {
        let mut config = unicast_config(0, 2);
        config.source_allow =
            SourceAllow::try_from_cidrs(&[Ipv4Cidr::parse("192.0.2.1/32").expect("cidr")])
                .expect("source allow");
        let mut receiver = Aes67Receiver::bind(config).expect("bind");
        let port = receiver.local_addr().port();
        let mut sender = crate::sender::Aes67RtpSender::bind(
            crate::sender::SenderConfig::unicast(SocketAddr::from((Ipv4Addr::LOCALHOST, port)), 2),
        )
        .expect("sender");
        for _ in 0..4 {
            sender.send_silence().expect("send");
        }
        assert!(
            wait_until(Duration::from_millis(500), || {
                receiver.io_stats().datagrams >= 4
            }),
            "{:?}",
            receiver.io_stats()
        );
        let stats = receiver.io_stats();
        assert!(stats.filtered_source >= 4, "{stats:?}");
        assert_eq!(
            stats.accepted_packets, 0,
            "白名单外来源不得进入 jitter：{stats:?}"
        );
        assert_eq!(receiver.jitter_stats().frames_received, 0);
        let report = receiver.report_json();
        assert!(report.contains("\"filtered_source\":"), "{report}");
        assert!(
            report.contains("\"source_allow\":[\"192.0.2.1/32\"]"),
            "{report}"
        );
        receiver.stop();
    }

    #[test]
    fn receiver_source_allow_accepts_listed_cidr() {
        let mut config = unicast_config(0, 2);
        config.source_allow =
            SourceAllow::try_from_cidrs(&[Ipv4Cidr::parse("127.0.0.0/8").expect("cidr")])
                .expect("source allow");
        let mut receiver = Aes67Receiver::bind(config).expect("bind");
        let port = receiver.local_addr().port();
        let mut sender = crate::sender::Aes67RtpSender::bind(
            crate::sender::SenderConfig::unicast(SocketAddr::from((Ipv4Addr::LOCALHOST, port)), 2),
        )
        .expect("sender");
        for _ in 0..4 {
            sender.send_silence().expect("send");
        }
        assert!(
            wait_until(Duration::from_millis(500), || {
                receiver.io_stats().accepted_packets >= 4
            }),
            "{:?}",
            receiver.io_stats()
        );
        assert_eq!(receiver.io_stats().filtered_source, 0);
        receiver.stop();
    }

    #[test]
    fn receiver_clock_and_config_validation() {
        let mut config = unicast_config(0, 2);
        config.clock = ReceiverClock::PtpPhc { domain: 0 };
        let error = Aes67Receiver::bind(config).expect_err("ptp not wired");
        assert_eq!(error.name(), "ERR_RX_CLOCK_NOT_WIRED");
        let mut config = unicast_config(0, 2);
        config.jitter.channels = 3; // channels 与 jitter 不一致
        assert_eq!(config.validate().err().unwrap().name(), "ERR_RX_CONFIG");
        let mut config = unicast_config(0, 2);
        config.channels = 0;
        assert_eq!(config.validate().err().unwrap().name(), "ERR_RX_CONFIG");
    }

    #[test]
    fn receiver_priority_config_records_state() {
        let mut config = unicast_config(0, 2);
        config.io_priority = Some(50);
        let mut receiver = Aes67Receiver::bind(config).expect("bind");
        assert!(wait_until(Duration::from_millis(300), || {
            !matches!(receiver.priority_status(), PriorityStatus::Pending)
        }));
        match receiver.priority_status() {
            PriorityStatus::Applied => {}
            PriorityStatus::Failed(name) => assert!(name.starts_with("ERR_PRIORITY")),
            other => panic!("unexpected priority state {other:?}"),
        }
        assert!(receiver.report_json().contains("\"io_priority\":\"50\""));
        receiver.stop();
    }

    #[test]
    fn receiver_from_sdp_requires_valid_profile() {
        let document = sdp::SdpDocument::from_text(
            "v=0\r\n\
s=t\r\n\
c=IN IP4 239.69.16.146/32\r\n\
m=audio 5004 RTP/AVP 103\r\n\
a=rtpmap:103 L24/48000/8\r\n\
a=ptime:1\r\n\
a=ts-refclk:ptp=IEEE1588-2008:00-11-22-33-44-55-66-77:0\r\n",
        );
        let session = document.parse().expect("parse");
        let config =
            ReceiverConfig::from_session(&session, Some(Ipv4Addr::LOCALHOST)).expect("config");
        assert!(config.multicast);
        assert_eq!(config.channels, 8);
        assert_eq!(config.port, 5_004);
        assert_eq!(config.jitter.block_frames, 128);
        assert_eq!(config.jitter.latency_frames(), 256);
        let mut invalid = document.text().to_string();
        invalid = invalid.replace("L24/48000/8", "L24/44100/8");
        let bad = sdp::SdpDocument::from_text(invalid);
        let bad_session = bad.parse().expect("parse 44.1k");
        assert_eq!(
            ReceiverConfig::from_session(&bad_session, None)
                .unwrap_err()
                .name(),
            "ERR_RX_SDP"
        );
    }

    #[test]
    fn receiver_sender_header_helper_is_public() {
        // 校验公共 API 形状（sender/receiver 共用 RTP 头构造）。
        let header = Header {
            version: 2,
            padding: false,
            extension: false,
            csrc_count: 0,
            marker: false,
            payload_type: 96,
            sequence: 1,
            timestamp: 48,
            ssrc: 2,
        };
        let mut bytes = [0u8; 12];
        header.write_into(&mut bytes);
        assert_eq!(Header::parse(&bytes).unwrap().header, header);
    }
}
