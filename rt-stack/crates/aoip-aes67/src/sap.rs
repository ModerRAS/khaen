//! SAP（Session Announcement Protocol，RFC 2974）P0 子集：通告 / 监听 / 会话表。
//!
//! 规范依据（本机原文 `artifacts/audio/aes67-spec/docs/rfc/rfc2974.txt`）：
//! - 报文头：V=1 / A / R / T / E / C + auth len + 16bit msg id hash + originating
//!   source（RFC 2974 §6，L303-408）。V MUST 为 1（L330-333）；R 由发送端置 0、
//!   接收端 MUST 忽略（L347-348）；A=0 为 32bit IPv4 源（L343-345）；T=1 为删除包
//!   （L350-351）；E=1 加密 / C=1 压缩：本子集不支持，fail closed。
//! - 端口 9875（L135）；TTL SHOULD 255（L136）；IPv4 全局 scope 组播地址
//!   224.2.127.254（L101-104）。
//! - payload：MIME 类型 `application/sdp` + 0x00 + SDP；若 payload 类型为
//!   application/sdp，该前缀可省略（L410-424）。本实现发送时带上前缀（AES67 实现
//!   指南 §8.1/参考工具口径），解析时两种形式都接受。
//! - 删除包：payload 为被删除会话的「单行 origin 字段」（L438-441）；本实现
//!   `encode_deletion` 只发这一行，接收侧按 (originating source, msg id hash) 删表。
//! - 通告间隔：RFC 2974 §3.1 L206 公式 `max(300, 8*N*size/limit)` 秒（至少 5 分钟）；
//!   AES67 部署实践常缩短（实现指南 §8.1 明确允许「按部署缩短（如 30 s）」），
//!   本实现默认 30 s、可按 [`SapConfig`] 配置（P0 口径，报告注 EXTENSION）。
//! - 隐式超时：10×通告周期，或 1 小时，取较大者（§4 L241-248）。
//! - msg id hash：16bit，与 originating source 组合唯一标识一个版本（§6 L371-378）；
//!   算法 RFC 未规定，本 crate 用 FNV-1a 32bit 的**低 16 位**（非 0）。
//!
//! 与 AES67 的关系：AES67 不强制 discovery（AES67-2023 §9）；使用 SAP 时应为 SAPv2
//! 并携带 §8 SDP（Annex E.2）。本模块只做发现/通告，**不做** RTSP/HTTP SDP 获取与
//! mDNS/DNS-SD（P1），也不参与 RT 音频路径（控制面独立线程，无 RT 分配）。
//!
//! 线程模型：
//! - [`SapAnnouncer`]：一个控制线程，启动/[`SapAnnouncer::update`] 时立即发通告，
//!   之后按配置周期重复；[`SapAnnouncer::stop`]（或 Drop）发删除包后退出。
//!   内容变化时先对旧 msg id hash 发删除包，再立即发新版本（RFC 2974 §5 L258-264）。
//! - [`SapMonitor`]：一个监听线程（默认组播 224.2.127.254:9875，可配置），校验报文头 /
//!   长度 / msg id，按 (source, hash) 维护最新 SDP 与删除状态；[snapshot](SapMonitor::snapshot)
//!   返回拥有所有权的会话快照（可跨线程查询）。
//!
//! 未验证（UNVERIFIED / NOT_RUN）：真实组播网络/跨交换机 TTL/IGMP 路径、与第三方
//! AES67 daemon 的 SAP 互通、板端长稳；离线单测只用本地 UDP 环回（unicast loopback）。

use std::collections::BTreeMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::sdp::{self, SdpError};

/// SAP 端口（RFC 2974 §3 L135：MUST 9875）。
pub const SAP_PORT: u16 = 9875;
/// IPv4 全局 scope SAP 组播地址（RFC 2974 §3 L101-104）。
pub const SAP_GLOBAL_GROUP: Ipv4Addr = Ipv4Addr::new(224, 2, 127, 254);
/// SAP TTL（RFC 2974 §3 L136：SHOULD 255）。
pub const SAP_TTL: u8 = 255;
/// 默认通告周期（AES67 部署实践 30 s；RFC 2974 §3.1 的 300 s 下限见模块注释）。
pub const DEFAULT_ANNOUNCE_INTERVAL: Duration = Duration::from_secs(30);
/// SDP payload 字节上限（RFC 2974 §6 L443-458：建议整包 < 1 kByte；本实现取 SDP ≤1024）。
pub const SAP_MAX_SDP_BYTES: usize = 1024;
/// SAP IPv4 报文头长度（字节）。
pub const SAP_HEADER_LEN: usize = 8;
/// `application/sdp` payload 类型前缀（含 NUL）。
pub const SAP_PAYLOAD_TYPE: &[u8] = b"application/sdp\0";
/// RFC 2974 §3.1 公式里的最小通告周期（秒；本实现默认 30 s 属部署缩短，见模块注释）。
pub const RFC2974_MIN_INTERVAL_SECS: u64 = 300;
/// RFC 2974 §4 隐式超时系数（10×通告周期）。
pub const SAP_TIMEOUT_FACTOR: u64 = 10;
/// RFC 2974 §4 隐式超时下限（1 小时）。
pub const SAP_MIN_TIMEOUT: Duration = Duration::from_secs(3_600);
/// 最近删除状态最多保留多少条（控制面有界内存；超出淘汰最旧）。
const MAX_DELETED_SESSIONS: usize = 64;

/// SAP 解析/发送/配置错误（Copy + 稳定错误名）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SapError {
    /// 报文短于 8 字节 IPv4 SAP 头。
    TooShort,
    /// V 字段不是 1（RFC 2974 §6 L330-333）。
    UnsupportedVersion,
    /// A=1（IPv6 originating source）：本 P0 只支持 IPv4。
    UnsupportedAddressType,
    /// E=1（加密）：不支持，fail closed。
    Encrypted,
    /// C=1（压缩）：不支持，fail closed。
    Compressed,
    /// auth len != 0（认证头）：不支持，fail closed。
    AuthUnsupported,
    /// payload 不是 UTF-8 / 非 ASCII，或为空。
    BadPayload,
    /// payload 类型不是 application/sdp（且不像 SDP 的 `v=`/`o=` 开头）。
    UnsupportedPayloadType,
    /// SDP payload 超过 [`SAP_MAX_SDP_BYTES`]。
    PayloadTooLong,
    /// originating source 为 0 或组播地址（RFC 2974 §6 L399-408）。
    BadOrigin,
    /// SDP `o=` 地址与 SAP originating source 不一致（通告路径强校验）。
    OriginMismatch,
    /// 携带的 SDP 解析失败。
    BadSdp(SdpError),
    /// 删除包 payload 不是单行 `o=` origin 字段（RFC 2974 §6 L438-441）。
    BadDeletionPayload,
    /// 配置非法（端口 0 / 周期过短 / 组播 TTL 0 等）。
    BadConfig,
    /// 通告已停止后继续 update。
    Stopped,
    /// socket I/O 错误。
    Io,
}

impl SapError {
    /// 稳定错误名（日志/证据 JSON 用）。
    pub const fn name(self) -> &'static str {
        match self {
            SapError::TooShort => "ERR_SAP_TOO_SHORT",
            SapError::UnsupportedVersion => "ERR_SAP_VERSION",
            SapError::UnsupportedAddressType => "ERR_SAP_ADDR_TYPE",
            SapError::Encrypted => "ERR_SAP_ENCRYPTED",
            SapError::Compressed => "ERR_SAP_COMPRESSED",
            SapError::AuthUnsupported => "ERR_SAP_AUTH",
            SapError::BadPayload => "ERR_SAP_PAYLOAD",
            SapError::UnsupportedPayloadType => "ERR_SAP_PAYLOAD_TYPE",
            SapError::PayloadTooLong => "ERR_SAP_PAYLOAD_TOO_LONG",
            SapError::BadOrigin => "ERR_SAP_ORIGIN",
            SapError::OriginMismatch => "ERR_SAP_ORIGIN_MISMATCH",
            SapError::BadSdp(_) => "ERR_SAP_SDP",
            SapError::BadDeletionPayload => "ERR_SAP_DELETION_PAYLOAD",
            SapError::BadConfig => "ERR_SAP_CONFIG",
            SapError::Stopped => "ERR_SAP_STOPPED",
            SapError::Io => "ERR_SAP_IO",
        }
    }
}

impl From<SdpError> for SapError {
    fn from(error: SdpError) -> Self {
        SapError::BadSdp(error)
    }
}

/// SAP 会话键：originating source + msg id hash（RFC 2974 §6 L371-378）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SapKey {
    /// originating source（IPv4）。
    pub originator: Ipv4Addr,
    /// 16bit message identifier hash。
    pub msg_id_hash: u16,
}

impl SapKey {
    /// 文本形式（日志/JSON 用，如 `192.168.1.10:1a2b`）。
    pub fn text(&self) -> String {
        format!("{}:{:04x}", self.originator, self.msg_id_hash)
    }
}

/// SAP 报文头解析结果（payload 借用输入 datagram）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SapPacket<'a> {
    /// T 位：true = 会话删除包。
    pub deletion: bool,
    /// 16bit message identifier hash。
    pub msg_id_hash: u16,
    /// originating source（IPv4）。
    pub originator: Ipv4Addr,
    payload: &'a [u8],
}

impl<'a> SapPacket<'a> {
    /// 解析 SAP 报文头（IPv4）；任何不支持的字段返回稳定错误，不 panic。
    pub fn parse(datagram: &'a [u8]) -> Result<Self, SapError> {
        if datagram.len() < SAP_HEADER_LEN {
            return Err(SapError::TooShort);
        }
        let first = datagram[0];
        if first >> 5 != 1 {
            return Err(SapError::UnsupportedVersion);
        }
        if (first >> 4) & 0x1 == 1 {
            return Err(SapError::UnsupportedAddressType);
        }
        // R 位：RFC 2974 §6 L347-348：接收端 MUST 忽略。
        let deletion = (first >> 2) & 0x1 == 1;
        if (first >> 1) & 0x1 == 1 {
            return Err(SapError::Encrypted);
        }
        if first & 0x1 == 1 {
            return Err(SapError::Compressed);
        }
        if datagram[1] != 0 {
            return Err(SapError::AuthUnsupported);
        }
        let msg_id_hash = u16::from_be_bytes([datagram[2], datagram[3]]);
        let originator = Ipv4Addr::new(datagram[4], datagram[5], datagram[6], datagram[7]);
        validate_originator(originator)?;
        let mut payload = &datagram[SAP_HEADER_LEN..];
        // 容忍尾部 NUL 填充（部分实现按 4 字节对齐）。
        while payload.last() == Some(&0) {
            payload = &payload[..payload.len() - 1];
        }
        if payload.is_empty() {
            return Err(SapError::BadPayload);
        }
        if payload.starts_with(SAP_PAYLOAD_TYPE) {
            payload = &payload[SAP_PAYLOAD_TYPE.len()..];
            while payload.last() == Some(&0) {
                payload = &payload[..payload.len() - 1];
            }
            if payload.is_empty() {
                return Err(SapError::BadPayload);
            }
        } else if !(payload.starts_with(b"v=") || payload.starts_with(b"o=")) {
            // RFC 2974 §6：application/sdp 前缀可省略（SDP 以 v= 开头）或删除包以
            // origin 行 o= 开头；其它 MIME 类型本子集不支持（fail closed）。
            return Err(SapError::UnsupportedPayloadType);
        }
        if payload.len() > SAP_MAX_SDP_BYTES {
            return Err(SapError::PayloadTooLong);
        }
        Ok(Self {
            deletion,
            msg_id_hash,
            originator,
            payload,
        })
    }

    /// payload 原始字节（已去除 payload 类型前缀与尾部 NUL）。
    pub fn payload(&self) -> &'a [u8] {
        self.payload
    }

    /// payload 文本（UTF-8）。
    pub fn text(&self) -> Result<&'a str, SapError> {
        std::str::from_utf8(self.payload).map_err(|_| SapError::BadPayload)
    }
}

/// 拥有所有权的 SAP 报文（socket 收包后交给会话表）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SapMessage {
    /// T 位：true = 会话删除包。
    pub deletion: bool,
    /// 16bit message identifier hash。
    pub msg_id_hash: u16,
    /// originating source（IPv4）。
    pub originator: Ipv4Addr,
    /// payload 文本（announce = SDP；deletion = 单行 origin 字段）。
    pub payload: String,
}

impl SapMessage {
    /// 由 UDP datagram 解析并拥有（socket 层唯一入口；失败即拒收）。
    pub fn from_datagram(datagram: &[u8]) -> Result<Self, SapError> {
        let packet = SapPacket::parse(datagram)?;
        let payload = packet.text()?.to_string();
        Ok(Self {
            deletion: packet.deletion,
            msg_id_hash: packet.msg_id_hash,
            originator: packet.originator,
            payload,
        })
    }

    /// 会话键（originator + msg id hash）。
    pub const fn key(&self) -> SapKey {
        SapKey {
            originator: self.originator,
            msg_id_hash: self.msg_id_hash,
        }
    }
}

/// 计算 16bit msg id hash（RFC 2974 §6：算法未规定；本实现 = FNV-1a 32bit 低 16 位，非 0）。
pub fn msg_id_hash(sdp_text: &str) -> u16 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in sdp_text.as_bytes() {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    let folded = (hash & 0xffff) as u16;
    if folded == 0 {
        1
    } else {
        folded
    }
}

/// 编码一条通告报文（含 `application/sdp\0` payload 类型前缀）。
pub fn encode_announcement(
    originator: Ipv4Addr,
    msg_id_hash: u16,
    sdp: &[u8],
) -> Result<Vec<u8>, SapError> {
    validate_originator(originator)?;
    if sdp.len() > SAP_MAX_SDP_BYTES {
        return Err(SapError::PayloadTooLong);
    }
    let text = std::str::from_utf8(sdp).map_err(|_| SapError::BadPayload)?;
    if !text.is_ascii() || text.is_empty() {
        return Err(SapError::BadPayload);
    }
    let mut datagram = Vec::with_capacity(SAP_HEADER_LEN + SAP_PAYLOAD_TYPE.len() + sdp.len());
    datagram.push(0b0010_0000); // V=1, A=0, R=0, T=0, E=0, C=0
    datagram.push(0); // auth len = 0
    datagram.extend_from_slice(&msg_id_hash.to_be_bytes());
    datagram.extend_from_slice(&originator.octets());
    datagram.extend_from_slice(SAP_PAYLOAD_TYPE);
    datagram.extend_from_slice(sdp);
    Ok(datagram)
}

/// 编码一条删除报文（T=1；payload 为单行 `o=` origin 字段 + CRLF，RFC 2974 §6 L438-441）。
pub fn encode_deletion(
    originator: Ipv4Addr,
    msg_id_hash: u16,
    origin_line: &str,
) -> Result<Vec<u8>, SapError> {
    validate_originator(originator)?;
    let line = origin_line.trim_end_matches(['\r', '\n']);
    if !line.starts_with("o=") || !line.is_ascii() || line.contains(['\r', '\n']) {
        return Err(SapError::BadDeletionPayload);
    }
    let payload_len = SAP_PAYLOAD_TYPE.len() + line.len() + 2;
    if payload_len > SAP_MAX_SDP_BYTES {
        return Err(SapError::PayloadTooLong);
    }
    let mut datagram = Vec::with_capacity(SAP_HEADER_LEN + payload_len);
    datagram.push(0b0010_0100); // V=1, A=0, R=0, T=1, E=0, C=0
    datagram.push(0);
    datagram.extend_from_slice(&msg_id_hash.to_be_bytes());
    datagram.extend_from_slice(&originator.octets());
    datagram.extend_from_slice(SAP_PAYLOAD_TYPE);
    datagram.extend_from_slice(line.as_bytes());
    datagram.extend_from_slice(b"\r\n");
    Ok(datagram)
}

/// originating source 合法性（RFC 2974 §6 L399-408：0 与组播地址非法）。
fn validate_originator(originator: Ipv4Addr) -> Result<(), SapError> {
    if originator.is_unspecified() || sdp::is_multicast_ipv4(originator) {
        return Err(SapError::BadOrigin);
    }
    Ok(())
}

/// SAP 通告配置（可配置组地址/端口/TTL/周期/出接口）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SapConfig {
    /// 目的组播/单播地址（默认 [`SAP_GLOBAL_GROUP`]；单播仅供离线 loopback 测试）。
    pub group: Ipv4Addr,
    /// 目的端口（默认 [`SAP_PORT`]）。
    pub port: u16,
    /// 组播 TTL（默认 [`SAP_TTL`] = 255）。
    pub ttl: u8,
    /// 周期通告间隔（默认 30 s；RFC 2974 §3.1 下限 300 s，本实现按 AES67 部署实践缩短）。
    pub interval: Duration,
    /// 组播出接口/源地址（Some 时用该源地址 bind 约束出接口；None = 系统默认路由）。
    ///
    /// P0 说明：std `UdpSocket` 无跨平台 `IP_MULTICAST_IF`；Some 仅做源地址绑定，
    /// 严格出接口选择标 NOT_WIRED（P1 视需要用 socket2/raw setsockopt）。
    pub interface: Option<Ipv4Addr>,
}

impl Default for SapConfig {
    fn default() -> Self {
        Self {
            group: SAP_GLOBAL_GROUP,
            port: SAP_PORT,
            ttl: SAP_TTL,
            interval: DEFAULT_ANNOUNCE_INTERVAL,
            interface: None,
        }
    }
}

impl SapConfig {
    /// 默认配置（224.2.127.254:9875、TTL 255、30 s）。
    pub const fn new() -> Self {
        Self {
            group: SAP_GLOBAL_GROUP,
            port: SAP_PORT,
            ttl: SAP_TTL,
            interval: DEFAULT_ANNOUNCE_INTERVAL,
            interface: None,
        }
    }

    /// 配置校验（端口/周期/TTL/目的地址）。
    pub fn validate(&self) -> Result<(), SapError> {
        if self.port == 0 {
            return Err(SapError::BadConfig);
        }
        if self.interval < Duration::from_millis(10) || self.interval > Duration::from_secs(86_400)
        {
            return Err(SapError::BadConfig);
        }
        if self.group.is_unspecified() {
            return Err(SapError::BadConfig);
        }
        if sdp::is_multicast_ipv4(self.group) && self.ttl == 0 {
            // TTL 0 的组播包不出本机（开发调试语义），生产 SAP 必须显式配置。
            return Err(SapError::BadConfig);
        }
        Ok(())
    }
}

/// 通告发送计数（快照）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SapSendCounters {
    /// 成功发出的通告包数。
    pub announcements: u64,
    /// 成功发出的删除包数。
    pub deletions: u64,
    /// 成功发出的 UDP 字节数。
    pub bytes: u64,
    /// send_to/编码失败次数。
    pub send_errors: u64,
}

/// 内部原子计数。
#[derive(Debug, Default)]
struct SendStats {
    announcements: AtomicU64,
    deletions: AtomicU64,
    bytes: AtomicU64,
    send_errors: AtomicU64,
}

impl SendStats {
    fn snapshot(&self) -> SapSendCounters {
        SapSendCounters {
            announcements: self.announcements.load(Ordering::Relaxed),
            deletions: self.deletions.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
            send_errors: self.send_errors.load(Ordering::Relaxed),
        }
    }
}

/// 已准备好的一个版本（SDP 文本 + msg id hash + 删除包用的 origin 行）。
#[derive(Clone, Debug, PartialEq, Eq)]
struct Announced {
    sdp: String,
    msg_id_hash: u16,
    origin_line: String,
}

/// 通告线程命令。
#[derive(Clone, Debug)]
enum AnnouncerCommand {
    Update(Announced),
    Stop,
}

/// 通告线程共享状态。
#[derive(Debug)]
struct AnnouncerState {
    current: Option<Announced>,
    next_due: Option<Instant>,
    command: Option<AnnouncerCommand>,
    stopped: bool,
}

/// 通告器共享数据（Arc 跨线程）。
#[derive(Debug)]
struct AnnouncerShared {
    state: Mutex<AnnouncerState>,
    changed: Condvar,
    config: SapConfig,
    originator: Ipv4Addr,
    stats: SendStats,
}

/// SAP 通告器（RFC 2974 发送侧；控制面独立线程，不进入 RT 音频路径）。
#[derive(Debug)]
pub struct SapAnnouncer {
    shared: Arc<AnnouncerShared>,
    thread: Option<JoinHandle<Result<(), SapError>>>,
}

impl SapAnnouncer {
    /// 启动：立即发一次通告，之后按 `config.interval` 周期重复。
    ///
    /// 要求 `sdp` 可被 [`sdp::parse_sdp`] 解析、带 `o=` 且其地址等于 `originator`
    /// （RFC 2974 通告的 SDP origin 应与 originating source 一致），且 SDP ≤1024 字节。
    pub fn start(
        config: SapConfig,
        originator: Ipv4Addr,
        sdp: impl Into<String>,
    ) -> Result<Self, SapError> {
        config.validate()?;
        validate_originator(originator)?;
        let announced = prepare_announcement(originator, sdp.into())?;
        let socket = bind_sender_socket(&config)?;
        let shared = Arc::new(AnnouncerShared {
            state: Mutex::new(AnnouncerState {
                current: Some(announced),
                next_due: Some(Instant::now()),
                command: None,
                stopped: false,
            }),
            changed: Condvar::new(),
            config,
            originator,
            stats: SendStats::default(),
        });
        let thread_shared = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("sap-announcer".to_string())
            .spawn(move || run_announcer(thread_shared, socket))
            .map_err(|_| SapError::Io)?;
        Ok(Self {
            shared,
            thread: Some(handle),
        })
    }

    /// 更新通告内容：校验通过后**立即**发送新版本；若内容/版本变化，先对旧
    /// msg id hash 发删除包（RFC 2974 §5）。
    pub fn update(&self, sdp: impl Into<String>) -> Result<(), SapError> {
        let announced = prepare_announcement(self.shared.originator, sdp.into())?;
        let mut state = self.shared.state.lock().map_err(|_| SapError::Io)?;
        if state.stopped {
            return Err(SapError::Stopped);
        }
        state.command = Some(AnnouncerCommand::Update(announced));
        self.shared.changed.notify_all();
        Ok(())
    }

    /// 当前（或待生效）通告的 SDP 文本。
    pub fn current_sdp(&self) -> Option<String> {
        let state = self.shared.state.lock().ok()?;
        match state.command.as_ref() {
            Some(AnnouncerCommand::Update(next)) => Some(next.sdp.clone()),
            _ => state.current.as_ref().map(|current| current.sdp.clone()),
        }
    }

    /// 发送计数快照。
    pub fn counters(&self) -> SapSendCounters {
        self.shared.stats.snapshot()
    }

    /// 配置。
    pub fn config(&self) -> SapConfig {
        self.shared.config
    }

    /// originating source。
    pub fn originator(&self) -> Ipv4Addr {
        self.shared.originator
    }

    /// 停止：发送删除包、join 通告线程（幂等）。
    pub fn stop(mut self) -> Result<(), SapError> {
        self.shutdown()
    }

    fn shutdown(&mut self) -> Result<(), SapError> {
        let Some(handle) = self.thread.take() else {
            return Ok(());
        };
        {
            let mut state = self.shared.state.lock().map_err(|_| SapError::Io)?;
            if !state.stopped {
                state.command = Some(AnnouncerCommand::Stop);
                self.shared.changed.notify_all();
            }
        }
        match handle.join() {
            Ok(result) => result,
            Err(_) => Err(SapError::Io),
        }
    }
}

impl Drop for SapAnnouncer {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// 通告前的准备与校验（SDP 解析 + `o=` 地址一致性 + 长度/ASCII）。
fn prepare_announcement(originator: Ipv4Addr, sdp: String) -> Result<Announced, SapError> {
    if sdp.len() > SAP_MAX_SDP_BYTES {
        return Err(SapError::PayloadTooLong);
    }
    if !sdp.is_ascii() {
        return Err(SapError::BadPayload);
    }
    let session = sdp::parse_sdp(&sdp).map_err(SapError::BadSdp)?;
    let origin = session.origin.ok_or(SapError::BadOrigin)?;
    if origin.ipv4() != originator {
        return Err(SapError::OriginMismatch);
    }
    let origin_line = format!(
        "o={} {} {} IN IP4 {}",
        origin.username,
        origin.session_id,
        origin.session_version,
        origin.ipv4()
    );
    Ok(Announced {
        msg_id_hash: msg_id_hash(&sdp),
        sdp,
        origin_line,
    })
}

/// 绑定通告 socket 并设置组播参数（TTL / 出接口 / 本机环回）。
fn bind_sender_socket(config: &SapConfig) -> Result<UdpSocket, SapError> {
    // std 的 UdpSocket 没有跨平台的 IP_MULTICAST_IF 设置接口（P0 零依赖约束）：
    // 指定 interface 时用源地址 bind 约束出接口（Windows 上即源地址选择 + 默认路由）；
    // 多网卡严格出接口选择留待 P1（socket2/raw setsockopt，报告标 NOT_WIRED）。
    let bind_address = config.interface.unwrap_or(Ipv4Addr::UNSPECIFIED);
    let socket = UdpSocket::bind((bind_address, 0)).map_err(|_| SapError::Io)?;
    socket
        .set_multicast_ttl_v4(u32::from(config.ttl))
        .map_err(|_| SapError::Io)?;
    // 同机监听（离线 loopback / 本机诊断）默认可收；生产组播也保持开启。
    socket
        .set_multicast_loop_v4(true)
        .map_err(|_| SapError::Io)?;
    Ok(socket)
}

/// 通告线程主循环。
fn run_announcer(shared: Arc<AnnouncerShared>, socket: UdpSocket) -> Result<(), SapError> {
    let destination = SocketAddrV4::new(shared.config.group, shared.config.port);
    let interval = shared.config.interval;
    loop {
        let mut state = shared.state.lock().map_err(|_| SapError::Io)?;
        if let Some(command) = state.command.take() {
            match command {
                AnnouncerCommand::Update(next) => {
                    let changed = state.current.as_ref().is_some_and(|current| {
                        current.msg_id_hash != next.msg_id_hash
                            || current.origin_line != next.origin_line
                    });
                    if changed {
                        if let Some(current) = state.current.as_ref() {
                            send_deletion(
                                &socket,
                                destination,
                                shared.originator,
                                current,
                                &shared.stats,
                            );
                        }
                    }
                    send_announcement(
                        &socket,
                        destination,
                        shared.originator,
                        &next,
                        &shared.stats,
                    );
                    state.current = Some(next);
                    state.next_due = Some(Instant::now() + interval);
                }
                AnnouncerCommand::Stop => {
                    if let Some(current) = state.current.take() {
                        send_deletion(
                            &socket,
                            destination,
                            shared.originator,
                            &current,
                            &shared.stats,
                        );
                    }
                    state.stopped = true;
                    return Ok(());
                }
            }
            continue;
        }
        let now = Instant::now();
        let due = state.next_due.unwrap_or(now);
        if now >= due {
            if let Some(current) = state.current.as_ref() {
                send_announcement(
                    &socket,
                    destination,
                    shared.originator,
                    current,
                    &shared.stats,
                );
            }
            state.next_due = Some(now + interval);
            continue;
        }
        let timeout = due - now;
        // 恢复时 `state` 由循环顶部的 `lock()` 重新获取；wait_timeout 返回的 guard
        // 在本语句结束时释放，避免与下一轮 lock 自锁。
        let _ = shared
            .changed
            .wait_timeout(state, timeout)
            .map_err(|_| SapError::Io)?;
    }
}

/// 发一条通告；失败只计数（控制面不外抛，避免线程退出）。
fn send_announcement(
    socket: &UdpSocket,
    destination: SocketAddrV4,
    originator: Ipv4Addr,
    announced: &Announced,
    stats: &SendStats,
) {
    match encode_announcement(originator, announced.msg_id_hash, announced.sdp.as_bytes()) {
        Ok(datagram) => match socket.send_to(&datagram, destination) {
            Ok(bytes) => {
                stats.announcements.fetch_add(1, Ordering::Relaxed);
                stats.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
            }
            Err(_) => {
                stats.send_errors.fetch_add(1, Ordering::Relaxed);
            }
        },
        Err(_) => {
            stats.send_errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// 发一条删除；失败只计数。
fn send_deletion(
    socket: &UdpSocket,
    destination: SocketAddrV4,
    originator: Ipv4Addr,
    announced: &Announced,
    stats: &SendStats,
) {
    match encode_deletion(originator, announced.msg_id_hash, &announced.origin_line) {
        Ok(datagram) => match socket.send_to(&datagram, destination) {
            Ok(bytes) => {
                stats.deletions.fetch_add(1, Ordering::Relaxed);
                stats.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
            }
            Err(_) => {
                stats.send_errors.fetch_add(1, Ordering::Relaxed);
            }
        },
        Err(_) => {
            stats.send_errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// 已入表的 SAP 会话（拥有所有权快照，可跨线程保存）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SapSession {
    /// 会话键。
    pub key: SapKey,
    /// 最近一次通告的 SDP 原文。
    pub sdp: String,
    /// SDP `o=` 源地址（若带且可解析）。
    pub origin_address: Option<Ipv4Addr>,
    /// AES67 媒体 profile 校验错误（None = 通过；语法能解析但非 P0 profile 的会话也会入表，
    /// 由调用方按错误名决定是否采用——不静默丢发现结果）。
    pub profile_error: Option<SdpError>,
    /// 最近收到该通告的单调毫秒（监视线程启动为 0）。
    pub received_ms: u64,
    /// 观测到的通告周期毫秒（首包用配置默认值）。
    pub announce_period_ms: u64,
}

/// SAP 入表结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SapOutcome {
    /// 新会话。
    Added,
    /// 同一键的 SDP 变化。
    Updated,
    /// 删除已有会话（显式 T=1 或超时）。
    Deleted(SapKey),
    /// 重复通告（SDP 未变，只刷新时间）。
    Unchanged,
    /// 未知删除等静默忽略。
    Ignored,
    /// 拒收（SDP 语法失败）；只计数，不影响已有会话。
    Rejected(&'static str),
}

/// SAP 接收/会话表计数。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SapCounters {
    /// 收到的 SAP datagram 数（含解析失败）。
    pub datagrams: u64,
    /// 通告包数。
    pub announcements: u64,
    /// 删除包数（含未知删除）。
    pub deletions: u64,
    /// 未知删除包数（无匹配会话）。
    pub unknown_deletions: u64,
    /// 接受的通告/删除数。
    pub accepted: u64,
    /// 拒收数（含解析失败与 SDP 语法失败）。
    pub rejected: u64,
    /// 报文级解析失败数（版本/长度/头字段等）。
    pub parse_errors: u64,
    /// 隐式超时删除数。
    pub expired: u64,
    /// 显式删除数。
    pub deleted: u64,
}

/// SAP 会话表：接收/更新/删除/超时（纯 NONRT 数据结构，无 socket）。
#[derive(Debug, Default)]
pub struct SapTable {
    entries: BTreeMap<SapKey, SapSession>,
    deleted: BTreeMap<SapKey, u64>,
    counters: SapCounters,
}

impl SapTable {
    /// 空表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 应用一条已解析 SAP 报文。
    pub fn apply(&mut self, message: &SapMessage, now_ms: u64) -> SapOutcome {
        self.counters.datagrams = self.counters.datagrams.saturating_add(1);
        let key = message.key();
        if message.deletion {
            self.counters.deletions = self.counters.deletions.saturating_add(1);
            return match self.entries.remove(&key) {
                Some(_) => {
                    self.counters.accepted = self.counters.accepted.saturating_add(1);
                    self.counters.deleted = self.counters.deleted.saturating_add(1);
                    self.record_deleted(key, now_ms);
                    SapOutcome::Deleted(key)
                }
                None => {
                    self.counters.unknown_deletions =
                        self.counters.unknown_deletions.saturating_add(1);
                    SapOutcome::Ignored
                }
            };
        }
        self.counters.announcements = self.counters.announcements.saturating_add(1);
        let Ok(session) = sdp::parse_sdp(&message.payload) else {
            self.counters.rejected = self.counters.rejected.saturating_add(1);
            return SapOutcome::Rejected(SdpError::Malformed.name());
        };
        let origin_address = session.origin_address();
        let profile_error = session.validate_media_profile().err();
        self.counters.accepted = self.counters.accepted.saturating_add(1);
        if let Some(existing) = self.entries.get_mut(&key) {
            let period = now_ms.saturating_sub(existing.received_ms);
            if existing.sdp == message.payload {
                existing.received_ms = now_ms;
                return SapOutcome::Unchanged;
            }
            existing.sdp = message.payload.clone();
            existing.origin_address = origin_address;
            existing.profile_error = profile_error;
            existing.received_ms = now_ms;
            if period > 0 {
                existing.announce_period_ms = period;
            }
            return SapOutcome::Updated;
        }
        self.entries.insert(
            key,
            SapSession {
                key,
                sdp: message.payload.clone(),
                origin_address,
                profile_error,
                received_ms: now_ms,
                announce_period_ms: DEFAULT_ANNOUNCE_INTERVAL.as_millis() as u64,
            },
        );
        SapOutcome::Added
    }

    /// 记录一条报文级解析失败（monitor 在 `SapMessage::from_datagram` 失败时调用）。
    pub fn record_parse_error(&mut self, _error: SapError) {
        self.counters.datagrams = self.counters.datagrams.saturating_add(1);
        self.counters.parse_errors = self.counters.parse_errors.saturating_add(1);
        self.counters.rejected = self.counters.rejected.saturating_add(1);
    }

    /// 隐式超时清理（RFC 2974 §4：10×通告周期 或 1 小时，取较大者）。
    pub fn expire(&mut self, now_ms: u64) -> Vec<SapKey> {
        let mut expired = Vec::new();
        self.entries.retain(|key, session| {
            let interval = session
                .announce_period_ms
                .saturating_mul(SAP_TIMEOUT_FACTOR)
                .max(SAP_MIN_TIMEOUT.as_millis() as u64);
            if now_ms.saturating_sub(session.received_ms) > interval {
                expired.push(*key);
                false
            } else {
                true
            }
        });
        self.counters.expired = self.counters.expired.saturating_add(expired.len() as u64);
        for key in &expired {
            self.record_deleted(*key, now_ms);
        }
        expired
    }

    /// 当前会话快照（按 key 有序）。
    pub fn snapshot(&self) -> Vec<SapSession> {
        self.entries.values().cloned().collect()
    }

    /// 单会话查询。
    pub fn get(&self, key: &SapKey) -> Option<&SapSession> {
        self.entries.get(key)
    }

    /// 删除状态查询：返回该键最近一次删除/超时的时刻（毫秒）。
    pub fn deleted_at(&self, key: &SapKey) -> Option<u64> {
        self.deleted.get(key).copied()
    }

    /// 最近删除状态快照（最多 [`MAX_DELETED_SESSIONS`] 条，按 key 有序）。
    pub fn deleted_sessions(&self) -> Vec<(SapKey, u64)> {
        self.deleted
            .iter()
            .map(|(key, at_ms)| (*key, *at_ms))
            .collect()
    }

    /// 会话数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 计数快照。
    pub fn counters(&self) -> SapCounters {
        self.counters
    }

    /// 记录删除状态（有界，超出淘汰 key 最小者）。
    fn record_deleted(&mut self, key: SapKey, at_ms: u64) {
        if self.deleted.len() >= MAX_DELETED_SESSIONS {
            if let Some(oldest) = self.deleted.keys().next().copied() {
                self.deleted.remove(&oldest);
            }
        }
        self.deleted.insert(key, at_ms);
    }
}

/// SAP 监听配置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SapMonitorConfig {
    /// 绑定地址（默认 `0.0.0.0:9875`；离线 loopback 用 `127.0.0.1:0` 自动选端口）。
    pub bind: SocketAddr,
    /// 要加入的组播组（None = 单播监听，仅测试/诊断）。
    pub group: Option<Ipv4Addr>,
    /// 组播加入接口地址（None = 系统默认）。
    pub interface: Option<Ipv4Addr>,
    /// recv 超时（决定 stop 响应粒度；默认 100 ms）。
    pub recv_timeout: Duration,
}

impl SapMonitorConfig {
    /// 生产监听：`0.0.0.0:9875` + 加入 RFC 2974 全局 scope 组播组。
    pub fn global() -> Self {
        Self {
            bind: SocketAddr::from((Ipv4Addr::UNSPECIFIED, SAP_PORT)),
            group: Some(SAP_GLOBAL_GROUP),
            interface: None,
            recv_timeout: Duration::from_millis(100),
        }
    }

    /// 单播监听（离线 loopback 测试；`bind` 可用端口 0 由系统分配）。
    pub fn unicast(bind: SocketAddr) -> Self {
        Self {
            bind,
            group: None,
            interface: None,
            recv_timeout: Duration::from_millis(50),
        }
    }
}

/// SAP 监听器（RFC 2974 接收侧；控制面独立线程，不进入 RT 音频路径）。
#[derive(Debug)]
pub struct SapMonitor {
    table: Arc<Mutex<SapTable>>,
    stop: Arc<AtomicBool>,
    local_addr: SocketAddr,
    thread: Option<JoinHandle<()>>,
}

impl SapMonitor {
    /// 启动监听线程并加入组播组（如有）。
    pub fn start(config: SapMonitorConfig) -> Result<Self, SapError> {
        let socket = UdpSocket::bind(config.bind).map_err(|_| SapError::Io)?;
        if let Some(group) = config.group {
            let interface = config.interface.unwrap_or(Ipv4Addr::UNSPECIFIED);
            socket
                .join_multicast_v4(&group, &interface)
                .map_err(|_| SapError::Io)?;
        }
        socket
            .set_read_timeout(Some(config.recv_timeout))
            .map_err(|_| SapError::Io)?;
        let local_addr = socket.local_addr().map_err(|_| SapError::Io)?;
        let table = Arc::new(Mutex::new(SapTable::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_table = Arc::clone(&table);
        let thread_stop = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name("sap-monitor".to_string())
            .spawn(move || run_monitor(thread_table, thread_stop, socket))
            .map_err(|_| SapError::Io)?;
        Ok(Self {
            table,
            stop,
            local_addr,
            thread: Some(handle),
        })
    }

    /// 实际绑定地址（端口 0 时据此拿系统分配端口）。
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// 当前会话快照。
    pub fn snapshot(&self) -> Vec<SapSession> {
        self.table
            .lock()
            .map(|table| table.snapshot())
            .unwrap_or_default()
    }

    /// 删除状态查询（见 [`SapTable::deleted_at`]）。
    pub fn deleted_at(&self, key: &SapKey) -> Option<u64> {
        self.table
            .lock()
            .ok()
            .and_then(|table| table.deleted_at(key))
    }

    /// 探测未知删除状态。
    pub fn deleted_sessions(&self) -> Vec<(SapKey, u64)> {
        self.table
            .lock()
            .map(|table| table.deleted_sessions())
            .unwrap_or_default()
    }

    /// 计数快照。
    pub fn counters(&self) -> SapCounters {
        self.table
            .lock()
            .map(|table| table.counters())
            .unwrap_or_default()
    }

    /// 停止监听线程（幂等）。
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if let Some(handle) = self.thread.take() {
            self.stop.store(true, Ordering::SeqCst);
            let _ = handle.join();
        }
    }
}

impl Drop for SapMonitor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 监听线程主循环：recv → 解析 → 入表/计数 → 周期过期。
fn run_monitor(table: Arc<Mutex<SapTable>>, stop: Arc<AtomicBool>, socket: UdpSocket) {
    let start = Instant::now();
    let mut buffer = [0u8; 2048];
    while !stop.load(Ordering::SeqCst) {
        match socket.recv_from(&mut buffer) {
            Ok((length, _source)) => {
                let now_ms = start.elapsed().as_millis() as u64;
                let Ok(mut table) = table.lock() else {
                    return;
                };
                match SapMessage::from_datagram(&buffer[..length]) {
                    Ok(message) => {
                        let _ = table.apply(&message, now_ms);
                    }
                    Err(error) => table.record_parse_error(error),
                }
                let _ = table.expire(now_ms);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                let now_ms = start.elapsed().as_millis() as u64;
                if let Ok(mut table) = table.lock() {
                    let _ = table.expire(now_ms);
                }
            }
            Err(_) => {
                // 非超时错误（如 Windows 上大包截断 WSAEMSGSIZE）：短暂退避后继续。
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdp::{Connection, Direction, Encoding, MediaClk, RefClk};
    use crate::sdp_write::{SdpDescription, SdpMedia, SdpOrigin};

    const TEST_ORIGIN: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 17);

    /// 生成一条可被 SAP 通告的合法 AES67 SDP（origin 地址 = 测试源地址）。
    fn test_sdp(origin: Ipv4Addr, port: u16, channels: u16) -> String {
        let media = SdpMedia {
            port,
            payload_type: 96,
            encoding: Encoding::L24,
            rate_hz: 48_000,
            channels,
            ptime_us: Some(1_000),
            framecount: Some(48),
            refclk: Some(RefClk::Local),
            mediaclk: Some(MediaClk {
                direct: false,
                offset_sec: 0,
            }),
            direction: Direction::SendOnly,
            channel_order: None,
            source_filter: None,
        };
        SdpDescription::new(
            SdpOrigin::new(1_311_738_121, 1, origin),
            format!("sap test flow {port}"),
            Connection {
                address: origin.octets(),
                ttl: None,
            },
            media,
        )
        .to_sdp()
        .expect("test SDP must render")
    }

    fn announcement(sdp: &str, originator: Ipv4Addr, hash: u16) -> SapMessage {
        let datagram = encode_announcement(originator, hash, sdp.as_bytes()).expect("encode");
        SapMessage::from_datagram(&datagram).expect("parse")
    }

    fn deletion(originator: Ipv4Addr, hash: u16) -> SapMessage {
        let line = format!("o=- 1 1 IN IP4 {originator}");
        let datagram = encode_deletion(originator, hash, &line).expect("encode deletion");
        SapMessage::from_datagram(&datagram).expect("parse deletion")
    }

    fn wait_until(deadline: Duration, mut predicate: impl FnMut() -> bool) -> bool {
        let end = Instant::now() + deadline;
        while Instant::now() < end {
            if predicate() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        predicate()
    }

    #[test]
    fn sap_header_roundtrip_announcement_and_deletion() {
        let sdp = test_sdp(TEST_ORIGIN, 5_004, 8);
        let datagram = encode_announcement(TEST_ORIGIN, 0x1234, sdp.as_bytes()).expect("encode");
        assert_eq!(datagram[0], 0b0010_0000, "V=1/A=0/T=0/E=0/C=0");
        assert_eq!(datagram[1], 0);
        let packet = SapPacket::parse(&datagram).expect("parse");
        assert!(!packet.deletion);
        assert_eq!(packet.msg_id_hash, 0x1234);
        assert_eq!(packet.originator, TEST_ORIGIN);
        assert_eq!(packet.text().expect("text"), sdp);
        assert!(packet.payload().starts_with(b"v=0"));
        // payload 类型前缀已被剥离（payload 从 v=0 开始）。
        assert_eq!(
            packet.payload(),
            &datagram[SAP_HEADER_LEN + SAP_PAYLOAD_TYPE.len()..]
        );

        let datagram = encode_deletion(TEST_ORIGIN, 0x1234, "o=- 1 1 IN IP4 192.168.1.17")
            .expect("encode deletion");
        assert_eq!(datagram[0], 0b0010_0100, "T=1");
        let packet = SapPacket::parse(&datagram).expect("parse deletion");
        assert!(packet.deletion);
        assert_eq!(packet.msg_id_hash, 0x1234);
        assert_eq!(packet.originator, TEST_ORIGIN);
        assert_eq!(
            packet.text().expect("text"),
            "o=- 1 1 IN IP4 192.168.1.17\r\n"
        );
        // 删除包 payload 是单行 origin 字段（RFC 2974 §6 L438-441）。
        assert!(packet.text().expect("text").starts_with("o="));
    }

    #[test]
    fn sap_rejects_unsupported_and_bad_packets_with_stable_names() {
        let sdp = test_sdp(TEST_ORIGIN, 5_004, 8);
        let base = encode_announcement(TEST_ORIGIN, 0x1234, sdp.as_bytes()).expect("encode");
        assert_eq!(SapPacket::parse(&[]).unwrap_err(), SapError::TooShort);
        assert_eq!(
            SapPacket::parse(&[0u8; 8]).unwrap_err(),
            SapError::UnsupportedVersion
        );
        let mut bad_version = base.clone();
        bad_version[0] = 0x40; // V=2（本地参考工具 aes67_ref.py 的写法，非 RFC 2974 §6）
        assert_eq!(
            SapPacket::parse(&bad_version).unwrap_err(),
            SapError::UnsupportedVersion
        );
        let mut ipv6 = base.clone();
        ipv6[0] = 0b0011_0000;
        assert_eq!(
            SapPacket::parse(&ipv6).unwrap_err(),
            SapError::UnsupportedAddressType
        );
        let mut auth = base.clone();
        auth[1] = 4;
        assert_eq!(
            SapPacket::parse(&auth).unwrap_err(),
            SapError::AuthUnsupported
        );
        let mut encrypted = base.clone();
        encrypted[0] |= 0b0000_0010;
        assert_eq!(
            SapPacket::parse(&encrypted).unwrap_err(),
            SapError::Encrypted
        );
        let mut compressed = base.clone();
        compressed[0] |= 0b0000_0001;
        assert_eq!(
            SapPacket::parse(&compressed).unwrap_err(),
            SapError::Compressed
        );
        let mut zero_origin = base.clone();
        zero_origin[4..8].fill(0);
        assert_eq!(
            SapPacket::parse(&zero_origin).unwrap_err(),
            SapError::BadOrigin
        );
        let mut multicast_origin = base.clone();
        multicast_origin[4..8].copy_from_slice(&[239, 1, 2, 3]);
        assert_eq!(
            SapPacket::parse(&multicast_origin).unwrap_err(),
            SapError::BadOrigin
        );
        let mut unknown_type = base.clone();
        unknown_type[SAP_HEADER_LEN] = b'X'; // "Xpplication/sdp\0..."
        assert_eq!(
            SapPacket::parse(&unknown_type).unwrap_err(),
            SapError::UnsupportedPayloadType
        );
        let mut bad_utf8 = base;
        bad_utf8.pop(); // 去掉末尾，再塞非法字节
        bad_utf8.push(0xff);
        bad_utf8.push(0xfe);
        assert_eq!(
            SapMessage::from_datagram(&bad_utf8).unwrap_err(),
            SapError::BadPayload
        );
        let mut short_payload = vec![0b0010_0000, 0];
        short_payload.extend_from_slice(&0x1234u16.to_be_bytes());
        short_payload.extend_from_slice(&TEST_ORIGIN.octets());
        assert_eq!(
            SapPacket::parse(&short_payload).unwrap_err(),
            SapError::BadPayload
        );
        assert_eq!(SapError::PayloadTooLong.name(), "ERR_SAP_PAYLOAD_TOO_LONG");
    }

    #[test]
    fn sap_msg_id_hash_is_stable_and_content_sensitive() {
        let first = test_sdp(TEST_ORIGIN, 5_004, 8);
        let second = test_sdp(TEST_ORIGIN, 5_006, 8);
        assert_eq!(msg_id_hash(&first), msg_id_hash(&first));
        assert_ne!(msg_id_hash(&first), msg_id_hash(&second));
        assert_ne!(msg_id_hash(&first), 0);
        assert_ne!(msg_id_hash(""), 0);
    }

    #[test]
    fn sap_encode_rejects_oversize_or_bad_payloads() {
        let too_long = "x".repeat(SAP_MAX_SDP_BYTES + 1);
        assert_eq!(
            encode_announcement(TEST_ORIGIN, 1, too_long.as_bytes()).unwrap_err(),
            SapError::PayloadTooLong
        );
        let exactly = "x".repeat(SAP_MAX_SDP_BYTES);
        assert!(encode_announcement(TEST_ORIGIN, 1, exactly.as_bytes()).is_ok());
        assert_eq!(
            encode_announcement(TEST_ORIGIN, 1, b"v=0\r\n\xff").unwrap_err(),
            SapError::BadPayload
        );
        assert_eq!(
            encode_deletion(TEST_ORIGIN, 1, "m=audio 5004 RTP/AVP 96").unwrap_err(),
            SapError::BadDeletionPayload
        );
        assert_eq!(
            encode_announcement(Ipv4Addr::UNSPECIFIED, 1, b"v=0\r\n").unwrap_err(),
            SapError::BadOrigin
        );
    }

    #[test]
    fn sap_announcer_rejects_bad_config_origin_and_sdp() {
        // SDP origin 与 originating source 不一致。
        let sdp = test_sdp(Ipv4Addr::new(192, 168, 1, 10), 5_004, 8);
        assert_eq!(
            SapAnnouncer::start(SapConfig::new(), Ipv4Addr::new(192, 168, 1, 11), sdp).unwrap_err(),
            SapError::OriginMismatch
        );
        // 非法 SDP。
        assert!(matches!(
            SapAnnouncer::start(
                SapConfig::new(),
                Ipv4Addr::new(192, 168, 1, 11),
                "v=0\r\ns=no-media\r\n"
            )
            .unwrap_err(),
            SapError::BadSdp(_)
        ));
        // 非法配置：端口 0 / 周期过短 / 组播 TTL 0。
        let mut config = SapConfig::new();
        config.port = 0;
        assert_eq!(config.validate().unwrap_err(), SapError::BadConfig);
        let mut config = SapConfig::new();
        config.interval = Duration::from_millis(1);
        assert_eq!(config.validate().unwrap_err(), SapError::BadConfig);
        let mut config = SapConfig::new();
        config.ttl = 0;
        assert_eq!(config.validate().unwrap_err(), SapError::BadConfig);
        // 超长 SDP（用空白填充到 1025 字节，保持可解析）。
        let mut long_name = String::from("v=0\r\n");
        long_name.push_str("o=- 1 1 IN IP4 192.168.1.11\r\n");
        let padding = "x".repeat(SAP_MAX_SDP_BYTES);
        long_name.push_str(&format!("s={padding}\r\n"));
        let media = test_sdp(Ipv4Addr::new(192, 168, 1, 11), 5_004, 8);
        long_name.push_str(&media[SAP_HEADER_LEN..]);
        assert_eq!(
            SapAnnouncer::start(SapConfig::new(), Ipv4Addr::new(192, 168, 1, 11), long_name)
                .unwrap_err(),
            SapError::PayloadTooLong
        );
    }

    #[test]
    fn sap_table_lifecycle_explicit_deletion_and_snapshot() {
        let sdp = test_sdp(TEST_ORIGIN, 5_004, 8);
        let key = SapKey {
            originator: TEST_ORIGIN,
            msg_id_hash: 0x1001,
        };
        let mut table = SapTable::new();
        assert_eq!(
            table.apply(&announcement(&sdp, TEST_ORIGIN, 0x1001), 1_000),
            SapOutcome::Added
        );
        assert_eq!(table.len(), 1);
        let session = table.snapshot().pop().expect("session");
        assert_eq!(session.key, key);
        assert_eq!(session.origin_address, Some(TEST_ORIGIN));
        assert_eq!(session.profile_error, None);
        assert_eq!(
            table.apply(&announcement(&sdp, TEST_ORIGIN, 0x1001), 2_000),
            SapOutcome::Unchanged
        );
        assert_eq!(table.snapshot()[0].received_ms, 2_000);
        // 同一 key、内容变化 → Updated（真实 announcer 会换 hash，这里测表逻辑）。
        let changed = test_sdp(TEST_ORIGIN, 5_006, 8);
        assert_eq!(
            table.apply(&announcement(&changed, TEST_ORIGIN, 0x1001), 3_000),
            SapOutcome::Updated
        );
        assert_eq!(
            table.apply(&deletion(TEST_ORIGIN, 0x9999), 4_000),
            SapOutcome::Ignored
        );
        assert_eq!(
            table.apply(&deletion(TEST_ORIGIN, 0x1001), 5_000),
            SapOutcome::Deleted(key)
        );
        assert!(table.is_empty());
        assert_eq!(table.deleted_at(&key), Some(5_000));
        assert_eq!(table.deleted_sessions(), vec![(key, 5_000)]);
        let counters = table.counters();
        assert_eq!(counters.datagrams, 5);
        assert_eq!(counters.announcements, 3);
        assert_eq!(counters.deletions, 2);
        assert_eq!(counters.unknown_deletions, 1);
        assert_eq!(counters.accepted, 4);
        assert_eq!(counters.rejected, 0);
        assert_eq!(counters.deleted, 1);
    }

    #[test]
    fn sap_table_records_profile_error_and_rejects_bad_sdp() {
        // L16/48k 能解析但非 P0 L24 媒体 profile：入表并记录错误（不静默丢弃）。
        let l16 = test_sdp(TEST_ORIGIN, 5_004, 2).replace("L24/48000/2", "L16/48000/2");
        let mut table = SapTable::new();
        assert_eq!(
            table.apply(&announcement(&l16, TEST_ORIGIN, 0x2001), 0),
            SapOutcome::Added
        );
        assert_eq!(
            table.snapshot()[0].profile_error,
            Some(crate::sdp::SdpError::UnsupportedEncoding)
        );
        // SDP 语法失败 → Rejected，不入表。
        let bad = "v=0\r\ns=no-media\r\n";
        assert_eq!(
            table.apply(&announcement(bad, TEST_ORIGIN, 0x2002), 1),
            SapOutcome::Rejected(SdpError::Malformed.name())
        );
        assert_eq!(table.len(), 1);
        assert_eq!(table.counters().rejected, 1);
    }

    #[test]
    fn sap_table_expires_after_rfc_timeout_floor() {
        let sdp = test_sdp(TEST_ORIGIN, 5_004, 8);
        let mut table = SapTable::new();
        table.apply(&announcement(&sdp, TEST_ORIGIN, 0x3001), 0);
        // 隐式超时 = max(10×30 s, 1 h) = 1 h。
        assert!(table.expire(SAP_MIN_TIMEOUT.as_millis() as u64).is_empty());
        assert_eq!(
            table.expire(SAP_MIN_TIMEOUT.as_millis() as u64 + 1),
            vec![SapKey {
                originator: TEST_ORIGIN,
                msg_id_hash: 0x3001,
            }]
        );
        assert!(table.is_empty());
        assert_eq!(table.counters().expired, 1);
        assert_eq!(
            table.deleted_at(&SapKey {
                originator: TEST_ORIGIN,
                msg_id_hash: 0x3001
            }),
            Some(3_600_001)
        );
    }

    #[test]
    fn sap_table_counts_parse_errors() {
        let mut table = SapTable::new();
        table.record_parse_error(SapError::UnsupportedVersion);
        let counters = table.counters();
        assert_eq!(counters.datagrams, 1);
        assert_eq!(counters.parse_errors, 1);
        assert_eq!(counters.rejected, 1);
        assert!(table.is_empty());
    }

    #[test]
    fn sap_constants_and_default_config_assertions() {
        assert_eq!(SAP_PORT, 9875);
        assert_eq!(SAP_GLOBAL_GROUP, Ipv4Addr::new(224, 2, 127, 254));
        assert_eq!(SAP_TTL, 255);
        assert_eq!(SAP_HEADER_LEN, 8);
        assert_eq!(SAP_MAX_SDP_BYTES, 1024);
        assert_eq!(SAP_PAYLOAD_TYPE, b"application/sdp\0");
        assert_eq!(RFC2974_MIN_INTERVAL_SECS, 300);
        assert_eq!(SAP_TIMEOUT_FACTOR, 10);
        assert_eq!(SAP_MIN_TIMEOUT, Duration::from_secs(3_600));
        let config = SapConfig::new();
        assert_eq!(config, SapConfig::default());
        assert_eq!(config.group, SAP_GLOBAL_GROUP);
        assert_eq!(config.port, SAP_PORT);
        assert_eq!(config.ttl, SAP_TTL);
        assert_eq!(config.interval, Duration::from_secs(30));
        assert!(config.validate().is_ok());
    }

    #[test]
    fn sap_monitor_receives_packet_and_rejects_garbage() {
        let monitor = SapMonitor::start(SapMonitorConfig::unicast(SocketAddr::from((
            Ipv4Addr::LOCALHOST,
            0,
        ))))
        .expect("monitor");
        let address = monitor.local_addr();
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("sender");
        let sdp = test_sdp(Ipv4Addr::LOCALHOST, 5_004, 8);
        let datagram =
            encode_announcement(Ipv4Addr::LOCALHOST, 0x4001, sdp.as_bytes()).expect("encode");
        sender
            .send_to(&datagram, address)
            .expect("send announcement");
        assert!(
            wait_until(Duration::from_secs(2), || monitor.snapshot().len() == 1),
            "monitor must receive announcement, counters={:?}",
            monitor.counters()
        );
        let snapshot = monitor.snapshot();
        assert_eq!(snapshot[0].sdp, sdp);
        assert_eq!(snapshot[0].key.msg_id_hash, 0x4001);
        // 坏包：版本 0 → parse_error 计数，不影响已入表会话。
        sender.send_to(&[0u8; 8], address).expect("send garbage");
        assert!(
            wait_until(Duration::from_secs(2), || monitor.counters().parse_errors
                >= 1),
            "monitor must count garbage, counters={:?}",
            monitor.counters()
        );
        assert_eq!(monitor.snapshot().len(), 1);
        assert_eq!(monitor.counters().rejected, 1);
        monitor.stop();
    }

    #[test]
    fn sap_two_instance_loopback_announce_update_delete() {
        let monitor = SapMonitor::start(SapMonitorConfig::unicast(SocketAddr::from((
            Ipv4Addr::LOCALHOST,
            0,
        ))))
        .expect("monitor");
        let address = monitor.local_addr();
        let mut config = SapConfig::new();
        config.group = Ipv4Addr::LOCALHOST;
        config.port = address.port();
        config.interval = Duration::from_millis(40);
        let first = test_sdp(Ipv4Addr::LOCALHOST, 5_004, 8);
        let announcer = SapAnnouncer::start(config, Ipv4Addr::LOCALHOST, first.clone())
            .expect("announcer start");
        assert!(
            wait_until(Duration::from_secs(3), || monitor.snapshot().len() == 1),
            "announcement must arrive, counters={:?}",
            monitor.counters()
        );
        assert_eq!(monitor.snapshot()[0].sdp, first);
        // 内容变化：立即通告新版本（旧 hash 先发删除包；新 hash 不同 → 最终只有新会话）。
        let second = test_sdp(Ipv4Addr::LOCALHOST, 5_006, 8);
        announcer.update(second.clone()).expect("update");
        assert!(
            wait_until(Duration::from_secs(3), || {
                monitor
                    .snapshot()
                    .iter()
                    .any(|session| session.sdp == second)
            }),
            "updated announcement must arrive, counters={:?}",
            monitor.counters()
        );
        assert!(
            wait_until(Duration::from_secs(3), || monitor.snapshot().len() == 1),
            "old version must be deleted, snapshot={:?}",
            monitor.snapshot()
        );
        // 停止：发删除包，快照清空。
        announcer.stop().expect("stop announcer");
        assert!(
            wait_until(Duration::from_secs(3), || monitor.snapshot().is_empty()),
            "deletion must clear session, snapshot={:?}",
            monitor.snapshot()
        );
        let counters = monitor.counters();
        assert!(counters.deletions >= 1, "counters={counters:?}");
        assert!(counters.announcements >= 2, "counters={counters:?}");
        monitor.stop();
    }

    #[test]
    fn sap_change_and_stop_are_immediate_with_long_interval() {
        let monitor = SapMonitor::start(SapMonitorConfig::unicast(SocketAddr::from((
            Ipv4Addr::LOCALHOST,
            0,
        ))))
        .expect("monitor");
        let address = monitor.local_addr();
        let mut config = SapConfig::new();
        config.group = Ipv4Addr::LOCALHOST;
        config.port = address.port();
        config.interval = Duration::from_secs(10); // 远长于测试窗口：只能靠“立即发送”到达
        let first = test_sdp(Ipv4Addr::LOCALHOST, 5_010, 8);
        let announcer = SapAnnouncer::start(config, Ipv4Addr::LOCALHOST, first).expect("announcer");
        assert!(wait_until(Duration::from_secs(2), || monitor
            .snapshot()
            .len()
            == 1));
        let second = test_sdp(Ipv4Addr::LOCALHOST, 5_012, 8);
        let changed_at = Instant::now();
        announcer.update(second.clone()).expect("update");
        assert!(
            wait_until(Duration::from_secs(2), || {
                monitor
                    .snapshot()
                    .iter()
                    .any(|session| session.sdp == second)
            }),
            "update must be immediate, counters={:?}",
            monitor.counters()
        );
        assert!(
            changed_at.elapsed() < Duration::from_secs(2),
            "update 延迟 {} ms",
            changed_at.elapsed().as_millis()
        );
        let stopped_at = Instant::now();
        announcer.stop().expect("stop");
        assert!(
            wait_until(Duration::from_secs(2), || monitor.snapshot().is_empty()),
            "deletion must be immediate, snapshot={:?}",
            monitor.snapshot()
        );
        assert!(stopped_at.elapsed() < Duration::from_secs(2));
        monitor.stop();
    }

    #[test]
    fn sap_periodic_reannounce_preserves_single_session() {
        let monitor = SapMonitor::start(SapMonitorConfig::unicast(SocketAddr::from((
            Ipv4Addr::LOCALHOST,
            0,
        ))))
        .expect("monitor");
        let address = monitor.local_addr();
        let mut config = SapConfig::new();
        config.group = Ipv4Addr::LOCALHOST;
        config.port = address.port();
        config.interval = Duration::from_millis(20);
        let sdp = test_sdp(Ipv4Addr::LOCALHOST, 5_004, 8);
        let announcer = SapAnnouncer::start(config, Ipv4Addr::LOCALHOST, sdp).expect("announcer");
        assert!(
            wait_until(Duration::from_secs(3), || monitor.counters().announcements
                >= 3),
            "periodic announcements expected, counters={:?}",
            monitor.counters()
        );
        assert_eq!(monitor.snapshot().len(), 1);
        assert!(announcer.counters().announcements >= 3);
        announcer.stop().expect("stop");
        monitor.stop();
    }
}
