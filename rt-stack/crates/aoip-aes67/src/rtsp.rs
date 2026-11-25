//! RTSP/1.0 子集（RAVENNA 连接管理面，P1 发现/协商面的一部分）。
//!
//! 规范依据（本机原文，注释中的行号指对应文件）：
//! - RFC 2326（RTSP 1.0）：scratch/aes67-rtsp/rfc/rfc2326.txt
//!   - RTSP URL 语法 / 默认端口 554：§3.2 L773-808（L804）；
//!   - 请求行 / 方法表：§6.1 L1134-1155；
//!   - 请求必须带 CSeq：§12.17 L2775-2781；表 3 L2491；
//!   - 响应长度：Content-Length MUST（有实体时）：§4.4 L1082-1094、§12.14 L2755-2761；
//!   - DESCRIBE 取 SDP（MUST 含全部初始化信息）：§10.2 L1687-1727（示例 L1696-1720）；
//!   - 不支持的方法 MUST 501：表 2 注 L1648-1652；
//!   - SETUP 的 Transport 参数：§10.4 L1812-1856、§12.39 L3230-3414（语法 L3380-3403）；
//!   - 405 必须带 Allow：§11.3.1 L2305-2313；454 会话不存在：§11.3.5 L2331-2334；
//!   - 461 不支持的传输：§11.3.12 L2381-2384；501 语义：表 2 注 L1648-1652；
//!   - Session 头 + timeout 参数（默认 60 s）：§12.37 L3174-3199（L3185、L3199）；
//!   - PLAY 前 SETUP 必须已被确认：§10.5 L1858-1863；TEARDOWN 语义：§10.7 L2038-2045。
//! - AES67-2023 Annex E.6：RAVENNA 用 DNS-SD 发现 + RTSP 取 SDP
//!   （docs/text/aes67-2023-draft-231218.txt L2721-2732）。
//! - RAVENNA Operating Principles Draft 1.0（scratch/ravenna/research/pdf/op-principles.txt）：
//!   - RTSP 支持 MUST；DESCRIBE 取 SDP：§3.4 L556-562；
//!   - 会话未配置/不存在时回 NOT FOUND 但保持连接（推荐行为）：§3.4.1 L570-584；
//!   - /by-id/<session id> 与 /by-name/<session name> 路径、rtsp://<host>:<port>：§3.5.3 L666-707。
//!
//! 实现口径（与 P0 sap/sdp_write 一致）：
//! - 纯 std、零外部依赖；控制面独立线程，不参与 RT 音频路径；
//! - SETUP 不分配 RTP/RTCP 端口：组播回显 SDP 声明的 destination/port/ttl，单播回显客户端的
//!   client_port 并把 SDP 声明的发送端口作为 server_port（本 crate 是控制面，媒体由既有
//!   receiver/sender 承担；见报告 §NOT_RUN/NOT_WIRED）；
//! - 单 TCP 连接多请求（按到达顺序处理）、并发多客户端（每连接一个线程）；
//! - 空闲连接/会话超时清理（RFC 2326 §12.37 默认 60 s，可配）；
//! - RAVENNA 推荐但本子集未实现：ANNOUNCE 异步推送（OP §3.4.1）——DESCRIBE 未命中回 404
//!   并保持连接（便于后续扩展推送），见 crate README 的 NOT_RUN 表。
//!
//! EXTENSION（语料未规定、本实现自定）：单播 SETUP 的 server_port 取值、默认组播 TTL 32、
//! 客户端由 SDP 自动生成 SETUP Transport。服务端 stop 不强制关闭已建立连接（空闲超时自然退出）。

use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::receiver::{ReceiverConfig, ReceiverError};
use crate::sdp::{self, Session};

/// RTSP 版本字符串（RFC 2326 §6.1）。
pub const RTSP_VERSION: &str = "RTSP/1.0";
/// RTSP 默认端口（RFC 2326 §3.2 L804：未给端口时按 554 处理）。
pub const RTSP_DEFAULT_PORT: u16 = 554;
/// RAVENNA 参考实现（aes67-linux-daemon）的默认 RTSP 端口（EXTENSION：RAVENNA OP 未规定端口）。
pub const RAVENNA_DEFAULT_RTSP_PORT: u16 = 8854;
/// 默认会话/连接空闲超时（RFC 2326 §12.37 L3199：默认 60 秒）。
pub const RTSP_DEFAULT_TIMEOUT_SECS: u64 = 60;
/// 默认组播 TTL（EXTENSION：RFC 2326 只定义 ttl 参数，不给缺省值；AES67 实践常用 32）。
pub const RTSP_DEFAULT_MULTICAST_TTL: u8 = 32;
/// 请求头上限（防滥用；本子集请求头很小，8 KiB 足够）。
pub const RTSP_MAX_HEADER_BYTES: usize = 8 * 1024;
/// 实体上限（DESCRIBE 请求无实体；ANNOUNCE 本子集不支持，但要能读完不卡死）。
pub const RTSP_MAX_BODY_BYTES: usize = 64 * 1024;
/// 服务端在 OPTIONS 中公布的已实现方法（RFC 2326 §10.1 L1667-1669 的 Public 头示例）。
pub const RTSP_PUBLIC_METHODS: &str = "OPTIONS, DESCRIBE, SETUP, PLAY, TEARDOWN";

/// RTSP 错误（稳定错误名 + 可选 source；I/O 错误保留原始 error）。
#[derive(Debug)]
pub enum RtspError {
    /// 报文还不完整（服务端循环等待更多字节；不是协议错误）。
    Incomplete,
    /// I/O 错误。
    Io(io::Error),
    /// 连接失败（客户端：connect 失败）。
    Connect(io::Error),
    /// 协议错误（稳定短名，便于日志/证据 JSON）。
    Protocol(&'static str),
    /// 非 2xx 响应（客户端）。
    BadStatus(u16),
    /// DESCRIBE 响应的 Content-Type 不是 application/sdp。
    NotSdp,
    /// 传输参数不支持（RFC 2326 §11.3.12 → 461）。
    UnsupportedTransport,
    /// SDP 解析错误。
    Sdp(sdp::SdpError),
    /// receiver 配置构造错误。
    Receiver(ReceiverError),
    /// 超时。
    Timeout,
    /// 服务端已停止。
    Stopped,
}

impl RtspError {
    /// 稳定错误名（日志/证据 JSON 用）。
    pub const fn name(&self) -> &'static str {
        match self {
            RtspError::Incomplete => "ERR_RTSP_INCOMPLETE",
            RtspError::Io(_) => "ERR_RTSP_IO",
            RtspError::Connect(_) => "ERR_RTSP_CONNECT",
            RtspError::Protocol(_) => "ERR_RTSP_PROTOCOL",
            RtspError::BadStatus(_) => "ERR_RTSP_BAD_STATUS",
            RtspError::NotSdp => "ERR_RTSP_NOT_SDP",
            RtspError::UnsupportedTransport => "ERR_RTSP_UNSUPPORTED_TRANSPORT",
            RtspError::Sdp(_) => "ERR_RTSP_SDP",
            RtspError::Receiver(_) => "ERR_RTSP_RECEIVER",
            RtspError::Timeout => "ERR_RTSP_TIMEOUT",
            RtspError::Stopped => "ERR_RTSP_STOPPED",
        }
    }
}

impl fmt::Display for RtspError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RtspError::Incomplete => f.write_str("incomplete RTSP message"),
            RtspError::Io(e) => write!(f, "rtsp io: {e}"),
            RtspError::Connect(e) => write!(f, "rtsp connect: {e}"),
            RtspError::Protocol(what) => write!(f, "rtsp protocol: {what}"),
            RtspError::BadStatus(code) => write!(f, "rtsp unexpected status {code}"),
            RtspError::NotSdp => f.write_str("rtsp response content-type is not application/sdp"),
            RtspError::UnsupportedTransport => f.write_str("rtsp unsupported transport"),
            RtspError::Sdp(e) => write!(f, "rtsp sdp: {e:?}"),
            RtspError::Receiver(e) => write!(f, "rtsp receiver: {e:?}"),
            RtspError::Timeout => f.write_str("rtsp timeout"),
            RtspError::Stopped => f.write_str("rtsp server stopped"),
        }
    }
}

impl std::error::Error for RtspError {}

impl From<io::Error> for RtspError {
    fn from(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::TimedOut || error.kind() == io::ErrorKind::WouldBlock {
            RtspError::Timeout
        } else {
            RtspError::Io(error)
        }
    }
}

impl From<sdp::SdpError> for RtspError {
    fn from(error: sdp::SdpError) -> Self {
        RtspError::Sdp(error)
    }
}

impl From<ReceiverError> for RtspError {
    fn from(error: ReceiverError) -> Self {
        RtspError::Receiver(error)
    }
}

type RtspResult<T> = Result<T, RtspError>;

fn header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn trim_ows(value: &str) -> &str {
    value.trim_matches(|c| c == ' ' || c == '\t')
}

/// 一条 RTSP 请求（已解析；header 名保留原始大小写，查询大小写不敏感）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RtspRequest {
    /// 方法（如 DESCRIBE）。
    pub method: String,
    /// Request-URI（绝对 URL 或 *，RFC 2326 §6.1 L1153）。
    pub uri: String,
    /// RTSP 版本（如 1.0）。
    pub version: String,
    /// 头字段（名称, 值）。
    pub headers: Vec<(String, String)>,
    /// 实体（RFC 2326 §4.4；本子集服务端不消费实体，但按 Content-Length 读走）。
    pub body: Vec<u8>,
}

impl RtspRequest {
    /// 从字节流解析一条请求。
    ///
    /// 返回 (request, consumed)；数据不完整返回 RtspError::Incomplete。
    pub fn parse(buffer: &[u8]) -> RtspResult<(Self, usize)> {
        let Some(end) = header_end(buffer) else {
            if buffer.len() > RTSP_MAX_HEADER_BYTES {
                return Err(RtspError::Protocol("header too large"));
            }
            return Err(RtspError::Incomplete);
        };
        if end > RTSP_MAX_HEADER_BYTES {
            return Err(RtspError::Protocol("header too large"));
        }
        let head = std::str::from_utf8(&buffer[..end])
            .map_err(|_| RtspError::Protocol("non-utf8 header"))?;
        let mut lines = head.split("\r\n");
        let request_line = lines.next().ok_or(RtspError::Protocol("empty request"))?;
        let mut parts = request_line.split(' ');
        let method = parts.next().unwrap_or_default();
        let uri = parts.next().unwrap_or_default();
        let version_token = parts.next().unwrap_or_default();
        if method.is_empty() || uri.is_empty() || parts.next().is_some() {
            return Err(RtspError::Protocol("bad request line"));
        }
        let version = version_token
            .strip_prefix("RTSP/")
            .ok_or(RtspError::Protocol("bad rtsp version"))?;
        if version != "1.0" {
            return Err(RtspError::Protocol("unsupported rtsp version"));
        }
        let mut headers = Vec::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            let Some((name, value)) = line.split_once(':') else {
                return Err(RtspError::Protocol("bad header line"));
            };
            if name.is_empty() || name.bytes().any(|b| b <= b' ' || b >= 0x7f || b == b':') {
                return Err(RtspError::Protocol("bad header name"));
            }
            headers.push((name.to_string(), trim_ows(value).to_string()));
        }
        let mut body_len = 0usize;
        for (name, value) in &headers {
            if name.eq_ignore_ascii_case("content-length") {
                body_len = value
                    .parse::<usize>()
                    .map_err(|_| RtspError::Protocol("bad content-length"))?;
            }
        }
        if body_len > RTSP_MAX_BODY_BYTES {
            return Err(RtspError::Protocol("body too large"));
        }
        let total = end + 4 + body_len;
        if buffer.len() < total {
            return Err(RtspError::Incomplete);
        }
        Ok((
            Self {
                method: method.to_string(),
                uri: uri.to_string(),
                version: version.to_string(),
                headers,
                body: buffer[end + 4..total].to_vec(),
            },
            total,
        ))
    }

    /// 大小写不敏感取头（同名多值取第一个）。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// 解析 CSeq（RFC 2326 §12.17：所有请求 MUST 带 CSeq）。
    pub fn cseq(&self) -> RtspResult<u32> {
        let value = self
            .header("cseq")
            .ok_or(RtspError::Protocol("missing cseq"))?;
        value
            .parse::<u32>()
            .map_err(|_| RtspError::Protocol("bad cseq"))
    }
}

/// 状态码原因短语（RFC 2326 §7.1.1；未列出的用 HTTP 通行短语）。
pub const fn status_reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        454 => "Session Not Found",
        461 => "Unsupported Transport",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        505 => "RTSP Version Not Supported",
        _ => "Unknown",
    }
}

/// 一条 RTSP 响应。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RtspResponse {
    /// 状态码。
    pub status: u16,
    /// 原因短语（编码时写入状态行）。
    pub reason: String,
    /// 头字段。
    pub headers: Vec<(String, String)>,
    /// 实体。
    pub body: Vec<u8>,
}

impl RtspResponse {
    /// 构造响应（原因短语按状态码查表）。
    pub fn new(status: u16) -> Self {
        Self {
            status,
            reason: status_reason(status).to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// 追加头字段。
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// 设置实体。
    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    /// 大小写不敏感取头。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// 序列化为线上字节（CRLF；有实体时自动补 Content-Length）。
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(128 + self.body.len());
        out.extend_from_slice(RTSP_VERSION.as_bytes());
        out.push(b' ');
        out.extend_from_slice(self.status.to_string().as_bytes());
        out.push(b' ');
        out.extend_from_slice(self.reason.as_bytes());
        out.extend_from_slice(b"\r\n");
        let mut has_length = false;
        for (name, value) in &self.headers {
            if name.eq_ignore_ascii_case("content-length") {
                has_length = true;
            }
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(b": ");
            out.extend_from_slice(value.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        if !self.body.is_empty() && !has_length {
            out.extend_from_slice(b"Content-Length: ");
            out.extend_from_slice(self.body.len().to_string().as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&self.body);
        out
    }

    /// 从字节流解析一条响应（客户端用；DESCRIBE 等有实体）。
    pub fn parse(buffer: &[u8]) -> RtspResult<(Self, usize)> {
        let Some(end) = header_end(buffer) else {
            if buffer.len() > RTSP_MAX_HEADER_BYTES {
                return Err(RtspError::Protocol("header too large"));
            }
            return Err(RtspError::Incomplete);
        };
        let head = std::str::from_utf8(&buffer[..end])
            .map_err(|_| RtspError::Protocol("non-utf8 header"))?;
        let mut lines = head.split("\r\n");
        let status_line = lines.next().ok_or(RtspError::Protocol("empty response"))?;
        let mut parts = status_line.splitn(3, ' ');
        let version = parts.next().unwrap_or_default();
        let status = parts
            .next()
            .and_then(|value| value.parse::<u16>().ok())
            .ok_or(RtspError::Protocol("bad status line"))?;
        let reason = parts.next().unwrap_or_default().to_string();
        if version != RTSP_VERSION {
            return Err(RtspError::Protocol("bad rtsp version"));
        }
        let mut headers = Vec::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            let Some((name, value)) = line.split_once(':') else {
                return Err(RtspError::Protocol("bad header line"));
            };
            headers.push((name.to_string(), trim_ows(value).to_string()));
        }
        let mut body_len = 0usize;
        for (name, value) in &headers {
            if name.eq_ignore_ascii_case("content-length") {
                body_len = value
                    .parse::<usize>()
                    .map_err(|_| RtspError::Protocol("bad content-length"))?;
            }
        }
        if body_len > RTSP_MAX_BODY_BYTES {
            return Err(RtspError::Protocol("body too large"));
        }
        let total = end + 4 + body_len;
        if buffer.len() < total {
            return Err(RtspError::Incomplete);
        }
        Ok((
            Self {
                status,
                reason,
                headers,
                body: buffer[end + 4..total].to_vec(),
            },
            total,
        ))
    }
}

/// SETUP 的传输参数（RFC 2326 §12.39 子集）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RtspTransport {
    /// 传输协议 / profile（本子集只接受 RTP/AVP）。
    pub profile: String,
    /// 下层传输（UDP；不实现的 TCP 会被拒绝）。
    pub lower_transport: Option<String>,
    /// 组播标志（multicast；缺省为 multicast，RFC 2326 §12.39 L3270-3272）。
    pub multicast: bool,
    /// 组播目的地址。
    pub destination: Option<Ipv4Addr>,
    /// 组播 RTP/RTCP 端口对中的 RTP 端口。
    pub port: Option<u16>,
    /// 单播客户端 RTP 端口。
    pub client_port: Option<u16>,
    /// 单播服务端 RTP 端口（响应回显）。
    pub server_port: Option<u16>,
    /// 组播 TTL。
    pub ttl: Option<u8>,
    /// 模式（PLAY 缺省）。
    pub mode: Option<String>,
}

impl Default for RtspTransport {
    fn default() -> Self {
        Self {
            profile: "RTP/AVP".to_string(),
            lower_transport: None,
            multicast: false,
            destination: None,
            port: None,
            client_port: None,
            server_port: None,
            ttl: None,
            mode: None,
        }
    }
}

fn parse_port_pair(value: &str) -> Option<u16> {
    value.split('-').next()?.parse::<u16>().ok()
}

impl RtspTransport {
    /// 解析 Transport 头（多个逗号分隔的 spec 只取第一个，RFC 2326 §12.39 L3238-3257）。
    ///
    /// 非 RTP/AVP 或 TCP 下层 → RtspError::UnsupportedTransport（服务端映射 461）。
    pub fn parse(value: &str) -> RtspResult<Self> {
        let spec = value.split(',').next().unwrap_or_default();
        let mut parts = spec.split(';');
        let head = trim_ows(parts.next().unwrap_or_default());
        let mut head_parts = head.split('/');
        let profile = head_parts.next().unwrap_or_default();
        let sub_profile = head_parts.next().unwrap_or_default();
        let lower = head_parts.next();
        if !profile.eq_ignore_ascii_case("RTP") || !sub_profile.eq_ignore_ascii_case("AVP") {
            return Err(RtspError::UnsupportedTransport);
        }
        if let Some(lower) = lower {
            if !lower.eq_ignore_ascii_case("UDP") {
                // 本子集不做 TCP/Interleaved 承载（控制面，无 RT 路径分配）。
                return Err(RtspError::UnsupportedTransport);
            }
        }
        let mut transport = RtspTransport {
            profile: "RTP/AVP".to_string(),
            lower_transport: lower.map(|value| value.to_ascii_uppercase()),
            ..RtspTransport::default()
        };
        let mut saw_direction = false;
        for raw in parts {
            let param = trim_ows(raw);
            if param.is_empty() {
                continue;
            }
            let (name, value) = match param.split_once('=') {
                Some((name, value)) => (trim_ows(name), Some(trim_ows(value))),
                None => (param, None),
            };
            match name.to_ascii_lowercase().as_str() {
                "multicast" => {
                    transport.multicast = true;
                    saw_direction = true;
                }
                "unicast" => {
                    transport.multicast = false;
                    saw_direction = true;
                }
                "destination" => {
                    let value = value.ok_or(RtspError::Protocol("bad destination"))?;
                    transport.destination = Some(
                        value
                            .parse::<Ipv4Addr>()
                            .map_err(|_| RtspError::Protocol("bad destination"))?,
                    );
                }
                "port" => {
                    let value = value.ok_or(RtspError::Protocol("bad port"))?;
                    transport.port =
                        Some(parse_port_pair(value).ok_or(RtspError::Protocol("bad port"))?);
                }
                "client_port" => {
                    let value = value.ok_or(RtspError::Protocol("bad client_port"))?;
                    transport.client_port =
                        Some(parse_port_pair(value).ok_or(RtspError::Protocol("bad client_port"))?);
                }
                "server_port" => {
                    let value = value.ok_or(RtspError::Protocol("bad server_port"))?;
                    transport.server_port =
                        Some(parse_port_pair(value).ok_or(RtspError::Protocol("bad server_port"))?);
                }
                "ttl" => {
                    let value = value.ok_or(RtspError::Protocol("bad ttl"))?;
                    transport.ttl = Some(
                        value
                            .parse::<u8>()
                            .map_err(|_| RtspError::Protocol("bad ttl"))?,
                    );
                }
                "mode" => {
                    transport.mode =
                        value.map(|value| value.trim_matches('"').to_ascii_uppercase());
                }
                "interleaved" | "layers" | "ssrc" | "append" | "source" => {
                    // 本子集不消费这些参数（不拒绝，保持互操作宽松）。
                }
                _ => {
                    // RFC 2326 §12.39 未定义参数：忽略（宽松互操作）。
                }
            }
        }
        if !saw_direction {
            // RFC 2326 §12.39 L3272：缺省为 multicast。
            transport.multicast = true;
        }
        Ok(transport)
    }

    /// 生成 Transport 头值（响应回显实际选择的参数）。
    pub fn header_value(&self) -> String {
        let mut out = String::from("RTP/AVP");
        if let Some(lower) = &self.lower_transport {
            out.push('/');
            out.push_str(lower);
        }
        out.push(';');
        out.push_str(if self.multicast {
            "multicast"
        } else {
            "unicast"
        });
        if let Some(destination) = self.destination {
            out.push_str(";destination=");
            out.push_str(&destination.to_string());
        }
        if let Some(port) = self.port {
            out.push_str(";port=");
            out.push_str(&port.to_string());
        }
        if let Some(port) = self.server_port {
            out.push_str(";server_port=");
            out.push_str(&port.to_string());
        }
        if let Some(port) = self.client_port {
            out.push_str(";client_port=");
            out.push_str(&port.to_string());
        }
        if let Some(ttl) = self.ttl {
            out.push_str(";ttl=");
            out.push_str(&ttl.to_string());
        }
        if let Some(mode) = &self.mode {
            out.push_str(";mode=\"");
            out.push_str(mode);
            out.push('"');
        }
        out
    }
}

/// 百分号解码（RTSP URL 的 path 段；非法编码返回协议错误）。
fn percent_decode(input: &str) -> RtspResult<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(RtspError::Protocol("bad percent escape"));
            }
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3])
                .map_err(|_| RtspError::Protocol("bad percent escape"))?;
            let value = u8::from_str_radix(hex, 16)
                .map_err(|_| RtspError::Protocol("bad percent escape"))?;
            out.push(value);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).map_err(|_| RtspError::Protocol("path is not utf-8"))
}

/// 从 Request-URI 提取 path（RFC 2326 §3.2 L779-794）。
pub fn request_uri_path(uri: &str) -> RtspResult<String> {
    if uri == "*" {
        return Ok("*".to_string());
    }
    let path = if let Some(rest) = uri
        .strip_prefix("rtsp://")
        .or_else(|| uri.strip_prefix("rtspu://"))
    {
        // 跳过 authority；本子集 IPv4 only，不处理 IPv6 字面量。
        match rest.find('/') {
            Some(index) => &rest[index..],
            None => "/",
        }
    } else if uri.starts_with('/') {
        uri
    } else {
        return Err(RtspError::Protocol("unsupported request uri"));
    };
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let path = if path.is_empty() { "/" } else { path };
    percent_decode(path)
}

/// 一条可被 RTSP/HTTP 发现面引用的流（名字 + SDP）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RtspStream {
    /// 会话名（RAVENNA /by-name/<session name>，OP §3.5.3.3 L686-707）。
    pub name: String,
    /// 会话 ID（RAVENNA /by-id/<session id>，OP §3.5.3.2 L677-685）。
    pub id: Option<u32>,
    /// SDP 文本（通常由 sdp_write::SdpDescription::to_sdp 生成）。
    pub sdp: String,
}

impl RtspStream {
    /// 以会话名构造。
    pub fn new(name: impl Into<String>, sdp: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            id: None,
            sdp: sdp.into(),
        }
    }

    /// 带会话 ID（/by-id/<id>）。
    pub fn with_id(mut self, id: u32) -> Self {
        self.id = Some(id);
        self
    }

    /// 路径匹配（/by-name/<name>、/by-id/<id>、以及 /<name>）。
    pub fn matches_path(&self, path: &str) -> bool {
        if let Some(rest) = path.strip_prefix("/by-name/") {
            return rest == self.name;
        }
        if let Some(rest) = path.strip_prefix("/by-id/") {
            return self.id.is_some_and(|id| rest.parse::<u32>() == Ok(id));
        }
        path.strip_prefix('/') == Some(self.name.as_str())
    }
}

/// 服务端配置。
#[derive(Clone, Debug)]
pub struct RtspServerConfig {
    /// 监听地址。
    pub bind: SocketAddr,
    /// 连接空闲超时（RFC 2326 §12.37 默认 60 s；测试可缩短）。
    pub connection_timeout: Duration,
    /// RTSP Session 空闲超时（会话表清理；默认 60 s）。
    pub session_timeout: Duration,
    /// Server 头值。
    pub server_name: String,
    /// 最大并发连接数（超出直接关闭）。
    pub max_clients: usize,
}

impl RtspServerConfig {
    /// 以监听地址构造默认配置。
    pub fn new(bind: SocketAddr) -> Self {
        let timeout = Duration::from_secs(RTSP_DEFAULT_TIMEOUT_SECS);
        Self {
            bind,
            connection_timeout: timeout,
            session_timeout: timeout,
            server_name: "tspi-aoip-aes67".to_string(),
            max_clients: 32,
        }
    }

    /// 设置连接/会话超时（测试用短超时）。
    pub fn with_timeouts(mut self, connection: Duration, session: Duration) -> Self {
        self.connection_timeout = connection;
        self.session_timeout = session;
        self
    }
}

#[derive(Debug)]
struct ServerSession {
    stream: usize,
    #[allow(dead_code)]
    transport: RtspTransport,
    playing: bool,
    last_activity: Instant,
}

#[derive(Debug)]
struct ServerContext {
    config: RtspServerConfig,
    streams: Arc<Vec<RtspStream>>,
    sessions: Arc<Mutex<HashMap<u32, ServerSession>>>,
    next_session: AtomicU32,
    clients: AtomicU32,
    stop: Arc<AtomicBool>,
}

impl ServerContext {
    fn resolve_stream(&self, request_uri: &str) -> Option<usize> {
        let path = request_uri_path(request_uri).ok()?;
        if path == "*" {
            return None;
        }
        self.streams
            .iter()
            .position(|stream| stream.matches_path(&path))
    }

    fn session_id(&self) -> u32 {
        loop {
            let candidate = self.next_session.fetch_add(1, Ordering::Relaxed);
            if candidate != 0 {
                return candidate;
            }
        }
    }
}

/// RTSP 服务端（每连接一个线程；控制面，无 RT 路径分配）。
#[derive(Debug)]
pub struct RtspServer {
    local_addr: SocketAddr,
    context: Arc<ServerContext>,
    accept_thread: Option<JoinHandle<()>>,
    cleanup_thread: Option<JoinHandle<()>>,
}

impl RtspServer {
    /// 启动服务端（监听 + accept 线程 + 会话清理线程）。
    pub fn start(config: RtspServerConfig, streams: Vec<RtspStream>) -> RtspResult<Self> {
        let listener = TcpListener::bind(config.bind).map_err(RtspError::Io)?;
        listener.set_nonblocking(true).map_err(RtspError::Io)?;
        let local_addr = listener.local_addr().map_err(RtspError::Io)?;
        let max_clients = config.max_clients;
        let context = Arc::new(ServerContext {
            config,
            streams: Arc::new(streams),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            next_session: AtomicU32::new(1),
            clients: AtomicU32::new(0),
            stop: Arc::new(AtomicBool::new(false)),
        });
        let accept_context = Arc::clone(&context);
        let accept_thread = thread::Builder::new()
            .name("rtsp-accept".to_string())
            .spawn(move || loop {
                if accept_context.stop.load(Ordering::Relaxed) {
                    break;
                }
                match listener.accept() {
                    Ok((stream, _peer)) => {
                        let active = accept_context.clients.fetch_add(1, Ordering::SeqCst) + 1;
                        if active as usize > max_clients {
                            accept_context.clients.fetch_sub(1, Ordering::SeqCst);
                            let _ = stream.shutdown(std::net::Shutdown::Both);
                            continue;
                        }
                        let connection_context = Arc::clone(&accept_context);
                        let spawned =
                            thread::Builder::new()
                                .name("rtsp-conn".to_string())
                                .spawn(move || {
                                    handle_connection(stream, connection_context);
                                });
                        if spawned.is_err() {
                            accept_context.clients.fetch_sub(1, Ordering::SeqCst);
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            })
            .map_err(RtspError::Io)?;
        let cleanup_context = Arc::clone(&context);
        let cleanup_thread = thread::Builder::new()
            .name("rtsp-cleanup".to_string())
            .spawn(move || {
                let timeout = cleanup_context.config.session_timeout;
                loop {
                    thread::sleep(Duration::from_millis(20));
                    if cleanup_context.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Ok(mut sessions) = cleanup_context.sessions.lock() {
                        sessions.retain(|_, session| session.last_activity.elapsed() < timeout);
                    } else {
                        break;
                    }
                }
            })
            .map_err(RtspError::Io)?;
        Ok(Self {
            local_addr,
            context,
            accept_thread: Some(accept_thread),
            cleanup_thread: Some(cleanup_thread),
        })
    }

    /// 实际监听地址。
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// 当前活动会话数（超时清理后递减）。
    pub fn session_count(&self) -> usize {
        self.context
            .sessions
            .lock()
            .map(|sessions| sessions.len())
            .unwrap_or(0)
    }

    /// 当前活动会话 (id, 流名)。
    pub fn active_sessions(&self) -> Vec<(u32, String)> {
        let Ok(sessions) = self.context.sessions.lock() else {
            return Vec::new();
        };
        let mut out: Vec<(u32, String)> = sessions
            .iter()
            .map(|(id, session)| (*id, self.context.streams[session.stream].name.clone()))
            .collect();
        out.sort_by_key(|(id, _)| *id);
        out
    }

    /// 按流名取 SDP（测试/诊断用）。
    pub fn stream_sdp(&self, name: &str) -> Option<String> {
        self.context
            .streams
            .iter()
            .find(|stream| stream.name == name)
            .map(|stream| stream.sdp.clone())
    }

    /// 停止服务端（关闭 accept/cleanup 线程；已建立连接由空闲超时自然退出）。
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.context.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.cleanup_thread.take() {
            let _ = handle.join();
        }
        if let Ok(mut sessions) = self.context.sessions.lock() {
            sessions.clear();
        }
    }
}

impl Drop for RtspServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn write_response(stream: &mut TcpStream, response: &RtspResponse) -> io::Result<()> {
    stream.write_all(&response.to_bytes())?;
    stream.flush()
}

fn handle_connection(mut stream: TcpStream, context: Arc<ServerContext>) {
    // Windows 下 accept 得到的 socket 可能继承监听 socket 的非阻塞模式；显式改回阻塞，
    // 否则第一次 read 可能立刻 WouldBlock 被当成空闲超时（会把刚建立的连接关掉）。
    let _ = stream.set_nonblocking(false);
    let timeout = context.config.connection_timeout;
    if stream.set_read_timeout(Some(timeout)).is_err()
        || stream.set_write_timeout(Some(timeout)).is_err()
    {
        let _ = context.clients.fetch_sub(1, Ordering::SeqCst);
        return;
    }
    let mut buffer: Vec<u8> = Vec::with_capacity(2048);
    let mut chunk = [0u8; 4096];
    let mut session_id: Option<u32> = None;
    loop {
        while !buffer.is_empty() {
            match RtspRequest::parse(&buffer) {
                Ok((request, consumed)) => {
                    buffer.drain(..consumed);
                    let mut response = handle_request(&request, &context, &mut session_id);
                    if let Some(cseq) = request.header("cseq") {
                        if response.header("cseq").is_none() {
                            response
                                .headers
                                .push(("CSeq".to_string(), cseq.to_string()));
                        }
                    }
                    let close = response.status == 400;
                    if write_response(&mut stream, &response).is_err() {
                        let _ = context.clients.fetch_sub(1, Ordering::SeqCst);
                        return;
                    }
                    if close {
                        let _ = context.clients.fetch_sub(1, Ordering::SeqCst);
                        return;
                    }
                }
                Err(RtspError::Incomplete) => break,
                Err(_) => {
                    let response = RtspResponse::new(400)
                        .with_header("Server", context.config.server_name.clone());
                    let _ = write_response(&mut stream, &response);
                    let _ = context.clients.fetch_sub(1, Ordering::SeqCst);
                    return;
                }
            }
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                buffer.extend_from_slice(&chunk[..read]);
                if buffer.len() > RTSP_MAX_HEADER_BYTES + RTSP_MAX_BODY_BYTES {
                    let response = RtspResponse::new(400)
                        .with_header("Server", context.config.server_name.clone());
                    let _ = write_response(&mut stream, &response);
                    break;
                }
            }
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.kind() == io::ErrorKind::TimedOut =>
            {
                // 空闲超时：关闭连接并回收（连接级清理）。
                break;
            }
            Err(_) => break,
        }
    }
    let _ = context.clients.fetch_sub(1, Ordering::SeqCst);
}

fn handle_request(
    request: &RtspRequest,
    context: &ServerContext,
    session_id: &mut Option<u32>,
) -> RtspResponse {
    let server = context.config.server_name.clone();
    // RFC 2326 §12.17 L2775-2776：CSeq MUST 出现在所有请求中 —— 缺失回 400。
    if request.cseq().is_err() {
        return RtspResponse::new(400).with_header("Server", server);
    }
    match request.method.as_str() {
        "OPTIONS" => RtspResponse::new(200)
            .with_header("Server", server)
            .with_header("Public", RTSP_PUBLIC_METHODS)
            .with_header("Allow", RTSP_PUBLIC_METHODS),
        "DESCRIBE" => describe(request, context),
        "SETUP" => setup(request, context, session_id),
        "PLAY" => play(request, context),
        "TEARDOWN" => teardown(request, context),
        // 未实现/未知方法：RFC 2326 表 2 注 L1648-1652 → 501 Not Implemented。
        other => RtspResponse::new(501)
            .with_header("Server", server)
            .with_header("Public", RTSP_PUBLIC_METHODS)
            .with_header("Unsupported", other),
    }
}

fn describe(request: &RtspRequest, context: &ServerContext) -> RtspResponse {
    let server = context.config.server_name.clone();
    // Accept 头可选；出现时必须接受 application/sdp（RFC 2326 §10.2 L1690-1693）。
    if let Some(accept) = request.header("accept") {
        let accepts_sdp = accept.split(',').any(|value| {
            let value = trim_ows(value).to_ascii_lowercase();
            value == "application/sdp" || value == "*/*" || value == "application/*"
        });
        if !accepts_sdp {
            return RtspResponse::new(406).with_header("Server", server);
        }
    }
    // RAVENNA OP §3.4.1 L578-584：会话不存在时回 NOT FOUND 但保持连接（404）。
    let Some(index) = context.resolve_stream(&request.uri) else {
        return RtspResponse::new(404).with_header("Server", server);
    };
    let sdp = context.streams[index].sdp.clone();
    RtspResponse::new(200)
        .with_header("Server", server)
        .with_header("Content-Type", "application/sdp")
        .with_header("Content-Length", sdp.len().to_string())
        .with_body(sdp)
}

fn setup(
    request: &RtspRequest,
    context: &ServerContext,
    session_id: &mut Option<u32>,
) -> RtspResponse {
    let server = context.config.server_name.clone();
    let Some(index) = context.resolve_stream(&request.uri) else {
        return RtspResponse::new(404).with_header("Server", server);
    };
    let Some(transport_header) = request.header("transport") else {
        // 表 3 L2510：Transport 对 SETUP 是必需头。
        return RtspResponse::new(400).with_header("Server", server);
    };
    let requested = match RtspTransport::parse(transport_header) {
        Ok(transport) => transport,
        Err(_) => return RtspResponse::new(461).with_header("Server", server),
    };
    // 由 SDP 补全组播 destination/port/ttl；SDP 不可解析时回 500（服务端内部错误）。
    let sdp_text = &context.streams[index].sdp;
    let Ok(parsed) = sdp::parse_sdp(sdp_text) else {
        return RtspResponse::new(500).with_header("Server", server);
    };
    let mut chosen = requested.clone();
    if chosen.multicast {
        let connection = parsed.connection();
        let destination = chosen
            .destination
            .or_else(|| connection.map(|connection| connection.ipv4()));
        let port = chosen.port.or(Some(parsed.media.port));
        let ttl = chosen
            .ttl
            .or_else(|| connection.and_then(|connection| connection.ttl));
        let (Some(destination), Some(port)) = (destination, port) else {
            return RtspResponse::new(461).with_header("Server", server);
        };
        chosen.destination = Some(destination);
        chosen.port = Some(port);
        chosen.ttl = Some(ttl.unwrap_or(RTSP_DEFAULT_MULTICAST_TTL));
    } else {
        // 本子集不分配 RT 端口（控制面）：单播 SETUP 只回显 client_port，
        // server_port 取 SDP 声明的媒体端口（EXTENSION，见模块文档）。
        if chosen.client_port.is_none() {
            return RtspResponse::new(461).with_header("Server", server);
        }
        chosen.server_port = Some(parsed.media.port);
        chosen.ttl = None;
        chosen.destination = None;
        chosen.port = None;
    }
    let id = context.session_id();
    let timeout_secs = context.config.session_timeout.as_secs().max(1);
    *session_id = Some(id);
    if let Ok(mut sessions) = context.sessions.lock() {
        sessions.insert(
            id,
            ServerSession {
                stream: index,
                transport: chosen.clone(),
                playing: false,
                last_activity: Instant::now(),
            },
        );
    }
    RtspResponse::new(200)
        .with_header("Server", server)
        .with_header("Session", format!("{id};timeout={timeout_secs}"))
        .with_header("Transport", chosen.header_value())
}

fn session_from_header(request: &RtspRequest) -> Option<u32> {
    let value = request.header("session")?;
    value.split(';').next()?.trim().parse::<u32>().ok()
}

fn play(request: &RtspRequest, context: &ServerContext) -> RtspResponse {
    let server = context.config.server_name.clone();
    let Some(id) = session_from_header(request) else {
        return RtspResponse::new(454).with_header("Server", server);
    };
    let Ok(mut sessions) = context.sessions.lock() else {
        return RtspResponse::new(500).with_header("Server", server);
    };
    let Some(session) = sessions.get_mut(&id) else {
        return RtspResponse::new(454).with_header("Server", server);
    };
    session.playing = true;
    session.last_activity = Instant::now();
    let timeout_secs = context.config.session_timeout.as_secs().max(1);
    RtspResponse::new(200)
        .with_header("Server", server)
        .with_header("Session", format!("{id};timeout={timeout_secs}"))
}

fn teardown(request: &RtspRequest, context: &ServerContext) -> RtspResponse {
    let server = context.config.server_name.clone();
    let Some(id) = session_from_header(request) else {
        return RtspResponse::new(454).with_header("Server", server);
    };
    let Ok(mut sessions) = context.sessions.lock() else {
        return RtspResponse::new(500).with_header("Server", server);
    };
    if sessions.remove(&id).is_none() {
        return RtspResponse::new(454).with_header("Server", server);
    }
    RtspResponse::new(200).with_header("Server", server)
}

/// DESCRIBE 结果（客户端）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RtspDescribed {
    /// 请求的 URL。
    pub url: String,
    /// SDP 文本。
    pub sdp: String,
    /// 响应 Content-Type。
    pub content_type: String,
}

impl RtspDescribed {
    /// 解析 SDP（借用；用于读取媒体参数）。
    pub fn parse(&self) -> RtspResult<Session<'_>> {
        sdp::parse_sdp(&self.sdp).map_err(RtspError::Sdp)
    }

    /// 解析 SDP 并构造既有 Aes67Receiver 的配置（任务要求：交给现有 receiver 配置）。
    pub fn receiver_config(&self, interface: Option<Ipv4Addr>) -> RtspResult<ReceiverConfig> {
        let session = self.parse()?;
        ReceiverConfig::from_session(&session, interface).map_err(RtspError::Receiver)
    }
}

/// RTSP 客户端配置。
#[derive(Clone, Debug)]
pub struct RtspClientConfig {
    /// 连接/读超时。
    pub timeout: Duration,
    /// User-Agent 头值。
    pub user_agent: String,
}

impl Default for RtspClientConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            user_agent: "tspi-aoip-aes67".to_string(),
        }
    }
}

impl RtspClientConfig {
    /// 以超时构造。
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            timeout,
            ..Self::default()
        }
    }
}

fn parse_rtsp_url(url: &str) -> RtspResult<(String, u16)> {
    let rest = url
        .strip_prefix("rtsp://")
        .ok_or(RtspError::Protocol("url must start with rtsp://"))?;
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (
            host,
            port.parse::<u16>()
                .map_err(|_| RtspError::Protocol("bad port"))?,
        ),
        None => (authority, RTSP_DEFAULT_PORT),
    };
    if host.is_empty() {
        return Err(RtspError::Protocol("empty host"));
    }
    Ok((host.to_string(), port))
}

/// 一个已建立控制连接的 RTSP 会话（客户端）。
#[derive(Debug)]
pub struct RtspConnection {
    stream: TcpStream,
    url: String,
    cseq: u32,
    config: RtspClientConfig,
    sdp: Option<String>,
    session_id: Option<u32>,
    transport: Option<RtspTransport>,
}

impl RtspConnection {
    /// 发送单条请求并读回响应（单连接多请求；CSeq 自动递增）。
    pub fn request(
        &mut self,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> RtspResult<RtspResponse> {
        self.cseq += 1;
        let cseq = self.cseq;
        let mut message = Vec::with_capacity(256);
        message.extend_from_slice(method.as_bytes());
        message.push(b' ');
        message.extend_from_slice(uri.as_bytes());
        message.extend_from_slice(b" RTSP/1.0\r\n");
        message.extend_from_slice(b"CSeq: ");
        message.extend_from_slice(cseq.to_string().as_bytes());
        message.extend_from_slice(b"\r\nUser-Agent: ");
        message.extend_from_slice(self.config.user_agent.as_bytes());
        message.extend_from_slice(b"\r\n");
        for (name, value) in headers {
            message.extend_from_slice(name.as_bytes());
            message.extend_from_slice(b": ");
            message.extend_from_slice(value.as_bytes());
            message.extend_from_slice(b"\r\n");
        }
        if !body.is_empty() {
            message.extend_from_slice(b"Content-Length: ");
            message.extend_from_slice(body.len().to_string().as_bytes());
            message.extend_from_slice(b"\r\n");
        }
        message.extend_from_slice(b"\r\n");
        message.extend_from_slice(body);
        self.stream.write_all(&message)?;
        self.stream.flush()?;
        let mut buffer: Vec<u8> = Vec::with_capacity(4096);
        let mut chunk = [0u8; 4096];
        loop {
            match RtspResponse::parse(&buffer) {
                Ok((response, _consumed)) => {
                    let expected = cseq.to_string();
                    match response.header("cseq") {
                        Some(value) if value == expected => {}
                        _ => return Err(RtspError::Protocol("cseq mismatch")),
                    }
                    return Ok(response);
                }
                Err(RtspError::Incomplete) => {}
                Err(error) => return Err(error),
            }
            let read = self.stream.read(&mut chunk)?;
            if read == 0 {
                return Err(RtspError::Protocol("connection closed"));
            }
            buffer.extend_from_slice(&chunk[..read]);
            if buffer.len() > RTSP_MAX_HEADER_BYTES + RTSP_MAX_BODY_BYTES {
                return Err(RtspError::Protocol("response too large"));
            }
        }
    }

    /// DESCRIBE：取 SDP（RFC 2326 §10.2）。
    pub fn describe(&mut self) -> RtspResult<String> {
        let url = self.url.clone();
        let response = self.request("DESCRIBE", &url, &[("Accept", "application/sdp")], &[])?;
        if response.status != 200 {
            return Err(RtspError::BadStatus(response.status));
        }
        let content_type = response
            .header("content-type")
            .ok_or(RtspError::Protocol("missing content-type"))?;
        if !content_type
            .to_ascii_lowercase()
            .starts_with("application/sdp")
        {
            return Err(RtspError::NotSdp);
        }
        let declared_length = match response.header("content-length") {
            Some(length) => Some(
                length
                    .parse::<usize>()
                    .map_err(|_| RtspError::Protocol("bad content-length"))?,
            ),
            None => None,
        };
        let sdp = String::from_utf8(response.body)
            .map_err(|_| RtspError::Protocol("sdp is not utf-8"))?;
        if let Some(length) = declared_length {
            if length != sdp.len() {
                return Err(RtspError::Protocol("content-length mismatch"));
            }
        }
        if sdp.is_empty() {
            return Err(RtspError::Protocol("empty sdp"));
        }
        self.sdp = Some(sdp.clone());
        Ok(sdp)
    }

    /// SETUP：以显式 Transport 头建立传输（RFC 2326 §10.4）。
    pub fn setup_with_transport(&mut self, transport_header: &str) -> RtspResult<u32> {
        let url = self.url.clone();
        let response = self.request("SETUP", &url, &[("Transport", transport_header)], &[])?;
        if response.status != 200 {
            return Err(RtspError::BadStatus(response.status));
        }
        let session = response
            .header("session")
            .ok_or(RtspError::Protocol("missing session"))?;
        let id = session
            .split(';')
            .next()
            .and_then(|value| value.trim().parse::<u32>().ok())
            .ok_or(RtspError::Protocol("bad session"))?;
        self.session_id = Some(id);
        if let Some(transport) = response.header("transport") {
            self.transport = RtspTransport::parse(transport).ok();
        }
        Ok(id)
    }

    /// SETUP：由 SDP 自动生成 Transport（组播取 c=/m=/ttl；单播要求调用方显式给端口）。
    pub fn setup(&mut self) -> RtspResult<u32> {
        let sdp = match &self.sdp {
            Some(sdp) => sdp.clone(),
            None => self.describe()?,
        };
        let parsed = sdp::parse_sdp(&sdp)?;
        let connection = parsed
            .connection()
            .ok_or(RtspError::Protocol("sdp missing connection"))?;
        if !connection.is_multicast() {
            return Err(RtspError::Protocol(
                "unicast setup requires explicit transport",
            ));
        }
        let ttl = connection.ttl.unwrap_or(RTSP_DEFAULT_MULTICAST_TTL);
        let header = format!(
            "RTP/AVP;multicast;destination={};port={};ttl={}",
            connection.ipv4(),
            parsed.media.port,
            ttl
        );
        self.setup_with_transport(&header)
    }

    /// PLAY（RFC 2326 §10.5；MUST 在 SETUP 被确认后）。
    pub fn play(&mut self) -> RtspResult<()> {
        let Some(session_id) = self.session_id else {
            return Err(RtspError::Protocol("play before setup"));
        };
        let session_text = session_id.to_string();
        let url = self.url.clone();
        let response = self.request("PLAY", &url, &[("Session", &session_text)], &[])?;
        if response.status != 200 {
            return Err(RtspError::BadStatus(response.status));
        }
        Ok(())
    }

    /// TEARDOWN（RFC 2326 §10.7）。
    pub fn teardown(&mut self) -> RtspResult<()> {
        let Some(session_id) = self.session_id else {
            return Err(RtspError::Protocol("teardown before setup"));
        };
        let session_text = session_id.to_string();
        let url = self.url.clone();
        let response = self.request("TEARDOWN", &url, &[("Session", &session_text)], &[])?;
        if response.status != 200 {
            return Err(RtspError::BadStatus(response.status));
        }
        self.session_id = None;
        Ok(())
    }

    /// 已取得的 SDP（DESCRIBE 之后）。
    pub fn sdp(&self) -> Option<&str> {
        self.sdp.as_deref()
    }

    /// 当前 Session ID（SETUP 之后）。
    pub fn session_id(&self) -> Option<u32> {
        self.session_id
    }

    /// 服务端回显的传输参数。
    pub fn transport(&self) -> Option<&RtspTransport> {
        self.transport.as_ref()
    }

    /// 解析 SDP 并构造既有 receiver 配置。
    pub fn receiver_config(&self, interface: Option<Ipv4Addr>) -> RtspResult<ReceiverConfig> {
        let sdp = self
            .sdp
            .as_deref()
            .ok_or(RtspError::Protocol("no sdp (describe first)"))?;
        let session = sdp::parse_sdp(sdp)?;
        ReceiverConfig::from_session(&session, interface).map_err(RtspError::Receiver)
    }
}

/// RTSP 客户端。
#[derive(Clone, Debug, Default)]
pub struct RtspClient {
    config: RtspClientConfig,
}

impl RtspClient {
    /// 默认客户端（5 s 超时）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 带超时构造。
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            config: RtspClientConfig::with_timeout(timeout),
        }
    }

    /// 建立到 rtsp://host[:port]/... 的 TCP 控制连接。
    pub fn connect(&self, url: &str) -> RtspResult<RtspConnection> {
        let (host, port) = parse_rtsp_url(url)?;
        let stream = TcpStream::connect((host.as_str(), port)).map_err(RtspError::Connect)?;
        stream
            .set_read_timeout(Some(self.config.timeout))
            .map_err(RtspError::Io)?;
        stream
            .set_write_timeout(Some(self.config.timeout))
            .map_err(RtspError::Io)?;
        Ok(RtspConnection {
            stream,
            url: url.to_string(),
            cseq: 0,
            config: self.config.clone(),
            sdp: None,
            session_id: None,
            transport: None,
        })
    }

    /// 一步 DESCRIBE（开连接、取 SDP、关连接）。
    pub fn describe(&self, url: &str) -> RtspResult<RtspDescribed> {
        let mut connection = self.connect(url)?;
        let sdp = connection.describe()?;
        Ok(RtspDescribed {
            url: url.to_string(),
            sdp,
            content_type: "application/sdp".to_string(),
        })
    }

    /// 完整握手：DESCRIBE → SETUP → PLAY，返回处于播放态的连接。
    pub fn open(&self, url: &str) -> RtspResult<RtspConnection> {
        let mut connection = self.connect(url)?;
        connection.describe()?;
        connection.setup()?;
        connection.play()?;
        Ok(connection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdp::{Connection, Direction, Encoding, MediaClk, PtpRefClk, RefClk};
    use crate::sdp_write::{SdpDescription, SdpMedia, SdpOrigin};
    use std::net::Ipv4Addr;
    use std::time::{Duration, Instant};

    const LOCAL: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 1);

    fn test_sdp() -> String {
        let description = SdpDescription::new(
            SdpOrigin::new(1311738121, 1, Ipv4Addr::new(192, 168, 63, 222)),
            "RTSP test session",
            Connection {
                address: Ipv4Addr::new(239, 69, 1, 1).octets(),
                ttl: Some(32),
            },
            SdpMedia {
                port: 5004,
                payload_type: 96,
                encoding: Encoding::L24,
                rate_hz: 48_000,
                channels: 8,
                ptime_us: Some(1_000),
                framecount: Some(48),
                refclk: Some(RefClk::Ptp(PtpRefClk {
                    domain: 0,
                    has_grandmaster: true,
                    grandmaster: [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77],
                })),
                mediaclk: Some(MediaClk {
                    direct: true,
                    offset_sec: 0,
                }),
                direction: Direction::SendOnly,
                channel_order: None,
                source_filter: None,
            },
        );
        description.to_sdp().expect("sdp")
    }

    fn test_streams() -> Vec<RtspStream> {
        vec![
            RtspStream::new("Line 1", test_sdp()).with_id(1),
            RtspStream::new("Line 2", test_sdp()).with_id(2),
        ]
    }

    fn free_addr() -> SocketAddr {
        let listener = TcpListener::bind((LOCAL, 0)).expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        addr
    }

    fn fast_config() -> RtspServerConfig {
        RtspServerConfig::new(free_addr())
            .with_timeouts(Duration::from_secs(2), Duration::from_secs(2))
    }

    fn wait_until(deadline: Duration, mut predicate: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < deadline {
            if predicate() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        predicate()
    }

    #[test]
    fn rtsp_request_parse_roundtrip_and_headers() {
        let raw = b"DESCRIBE rtsp://192.168.1.1:8554/by-name/Line%201 RTSP/1.0\r\nCSeq: 312\r\nAccept: application/sdp\r\nUser-Agent: t\r\nContent-Length: 4\r\n\r\nbody";
        let (request, consumed) = RtspRequest::parse(raw).expect("parse");
        assert_eq!(consumed, raw.len());
        assert_eq!(request.method, "DESCRIBE");
        assert_eq!(request.version, "1.0");
        assert_eq!(request.cseq().expect("cseq"), 312);
        assert_eq!(request.header("accept"), Some("application/sdp"));
        assert_eq!(request.header("CSeq"), Some("312"));
        assert_eq!(request.body, b"body");
        // 不完整 → Incomplete，不误报协议错。
        assert!(matches!(
            RtspRequest::parse(&raw[..raw.len() - 2]),
            Err(RtspError::Incomplete)
        ));
        // 缺 CSeq → 明确错误名。
        let (request, _) = RtspRequest::parse(b"OPTIONS * RTSP/1.0\r\n\r\n").expect("parse");
        assert_eq!(
            request.cseq().expect_err("cseq").name(),
            "ERR_RTSP_PROTOCOL"
        );
        // 版本非 1.0 → 拒绝。
        assert!(RtspRequest::parse(b"OPTIONS * RTSP/2.0\r\nCSeq: 1\r\n\r\n").is_err());
    }

    #[test]
    fn rtsp_response_encoding_has_content_length_and_crlf() {
        let response = RtspResponse::new(200)
            .with_header("CSeq", "7")
            .with_header("Content-Type", "application/sdp")
            .with_body(b"v=0\r\n".to_vec());
        let bytes = response.to_bytes();
        let text = String::from_utf8(bytes.clone()).expect("utf8");
        assert!(text.starts_with("RTSP/1.0 200 OK\r\n"));
        assert!(text.contains("Content-Length: 5\r\n"));
        assert!(text.ends_with("\r\n\r\nv=0\r\n"));
        let (parsed, consumed) = RtspResponse::parse(&bytes).expect("parse");
        assert_eq!(consumed, bytes.len());
        assert_eq!(parsed.status, 200);
        assert_eq!(parsed.body, b"v=0\r\n");
        assert_eq!(parsed.header("cseq"), Some("7"));
        assert_eq!(status_reason(461), "Unsupported Transport");
        assert_eq!(status_reason(501), "Not Implemented");
        assert_eq!(status_reason(454), "Session Not Found");
    }

    #[test]
    fn rtsp_transport_parsing_and_rejection() {
        let transport =
            RtspTransport::parse("RTP/AVP;multicast;destination=239.69.1.1;port=5004-5005;ttl=32")
                .expect("transport");
        assert!(transport.multicast);
        assert_eq!(transport.destination, Some(Ipv4Addr::new(239, 69, 1, 1)));
        assert_eq!(transport.port, Some(5004));
        assert_eq!(transport.ttl, Some(32));
        assert_eq!(
            transport.header_value(),
            "RTP/AVP;multicast;destination=239.69.1.1;port=5004;ttl=32"
        );
        let transport =
            RtspTransport::parse("RTP/AVP;unicast;client_port=4588-4589").expect("transport");
        assert!(!transport.multicast);
        assert_eq!(transport.client_port, Some(4588));
        assert!(
            RtspTransport::parse("RTP/AVP")
                .expect("transport")
                .multicast
        );
        assert!(matches!(
            RtspTransport::parse("RTP/AVP/TCP;unicast;interleaved=0-1"),
            Err(RtspError::UnsupportedTransport)
        ));
        assert!(matches!(
            RtspTransport::parse("RAW/RAW/UDP"),
            Err(RtspError::UnsupportedTransport)
        ));
        assert!(RtspTransport::parse("RTP/AVP;multicast;ttl=999").is_err());
    }

    #[test]
    fn rtsp_request_uri_path_forms() {
        assert_eq!(
            request_uri_path("rtsp://192.168.0.1:8554/by-name/Line%201").expect("path"),
            "/by-name/Line 1"
        );
        assert_eq!(
            request_uri_path("rtsp://host/by-id/2").expect("path"),
            "/by-id/2"
        );
        assert_eq!(request_uri_path("*").expect("path"), "*");
        assert_eq!(request_uri_path("rtsp://host:554/").expect("path"), "/");
        assert_eq!(
            request_uri_path("/by-name/x?y=1").expect("path"),
            "/by-name/x"
        );
        assert!(request_uri_path("http://x/y").is_err());
        assert!(request_uri_path("rtsp://host/by-name/%ZZ").is_err());
        let stream = RtspStream::new("Line 1", "v=0").with_id(7);
        assert!(stream.matches_path("/by-name/Line 1"));
        assert!(stream.matches_path("/by-id/7"));
        assert!(stream.matches_path("/Line 1"));
        assert!(!stream.matches_path("/by-id/8"));
        assert!(!stream.matches_path("/by-name/other"));
    }

    #[test]
    fn rtsp_server_full_loopback_flow_with_multiple_requests() {
        let server = RtspServer::start(fast_config(), test_streams()).expect("server");
        let url = format!("rtsp://{}/by-name/Line%201", server.local_addr());
        let client = RtspClient::with_timeout(Duration::from_secs(2));
        let mut connection = client.connect(&url).expect("connect");
        // 同一 TCP 连接上多请求：OPTIONS → DESCRIBE → SETUP → PLAY → TEARDOWN。
        let options = connection
            .request("OPTIONS", &url, &[], &[])
            .expect("options");
        assert_eq!(options.status, 200);
        assert!(options
            .header("public")
            .expect("public")
            .contains("DESCRIBE"));
        let sdp = connection.describe().expect("describe");
        assert!(sdp.contains("m=audio 5004 RTP/AVP 96"));
        let session = connection.setup().expect("setup");
        assert!(session >= 1);
        assert!(wait_until(Duration::from_millis(500), || server
            .session_count()
            == 1));
        let transport = connection.transport().expect("transport echo");
        assert!(transport.multicast);
        assert_eq!(transport.destination, Some(Ipv4Addr::new(239, 69, 1, 1)));
        assert_eq!(transport.port, Some(5004));
        assert_eq!(transport.ttl, Some(32));
        connection.play().expect("play");
        connection.teardown().expect("teardown");
        assert!(wait_until(Duration::from_millis(500), || server
            .session_count()
            == 0));
        // TEARDOWN 之后同一连接还能继续请求（keep-alive）。
        let response = connection
            .request("OPTIONS", &url, &[], &[])
            .expect("options2");
        assert_eq!(response.status, 200);
        drop(connection);
        server.stop();
    }

    #[test]
    fn rtsp_error_methods_and_missing_cseq() {
        let server = RtspServer::start(fast_config(), test_streams()).expect("server");
        let url = format!("rtsp://{}/by-name/Line%201", server.local_addr());
        let client = RtspClient::with_timeout(Duration::from_secs(2));
        let mut connection = client.connect(&url).expect("connect");
        // 未实现方法 → 501。
        let response = connection
            .request("ANNOUNCE", &url, &[], b"v=0\r\n")
            .expect("announce");
        assert_eq!(response.status, 501);
        assert!(response.header("unsupported").is_some());
        // SETUP 不支持的传输 → 461。
        let response = connection
            .request(
                "SETUP",
                &url,
                &[("Transport", "RTP/AVP/TCP;unicast;interleaved=0-1")],
                &[],
            )
            .expect("setup tcp");
        assert_eq!(response.status, 461);
        // SETUP 缺 Transport → 400，服务端随后关闭连接。
        let mut raw = TcpStream::connect(server.local_addr()).expect("raw connect");
        raw.write_all(b"SETUP rtsp://host/by-name/Line%201 RTSP/1.0\r\nCSeq: 9\r\n\r\n")
            .expect("write");
        let mut buf = [0u8; 512];
        let read = raw.read(&mut buf).expect("read");
        let text = String::from_utf8_lossy(&buf[..read]);
        assert!(text.starts_with("RTSP/1.0 400 Bad Request\r\n"), "{text}");
        drop(raw);
        server.stop();
    }

    #[test]
    fn rtsp_unknown_path_404_keeps_connection() {
        let server = RtspServer::start(fast_config(), test_streams()).expect("server");
        let url = format!("rtsp://{}/by-name/Missing", server.local_addr());
        let client = RtspClient::with_timeout(Duration::from_secs(2));
        let mut connection = client.connect(&url).expect("connect");
        let response = connection
            .request("DESCRIBE", &url, &[("Accept", "application/sdp")], &[])
            .expect("describe");
        assert_eq!(response.status, 404);
        // RAVENNA OP §3.4.1：404 但保持连接，后续请求仍有效。
        let response = connection
            .request("OPTIONS", &url, &[], &[])
            .expect("options");
        assert_eq!(response.status, 200);
        // 406：Accept 不含 application/sdp。
        let ok_url = format!("rtsp://{}/by-name/Line%201", server.local_addr());
        let response = connection
            .request("DESCRIBE", &ok_url, &[("Accept", "application/rtsl")], &[])
            .expect("describe 406");
        assert_eq!(response.status, 406);
        drop(connection);
        server.stop();
    }

    #[test]
    fn rtsp_describe_body_roundtrips_through_sdp_parser() {
        let server = RtspServer::start(fast_config(), test_streams()).expect("server");
        let url = format!("rtsp://{}/by-name/Line%201", server.local_addr());
        let client = RtspClient::with_timeout(Duration::from_secs(2));
        let described = client.describe(&url).expect("describe");
        assert_eq!(described.sdp, test_sdp());
        let session = described.parse().expect("parse");
        assert_eq!(session.media.port, 5004);
        assert_eq!(session.media.rtpmap.channels, 8);
        assert_eq!(session.media.rtpmap.encoding, Encoding::L24);
        let receiver = described
            .receiver_config(Some(LOCAL))
            .expect("receiver cfg");
        assert_eq!(receiver.port, 5004);
        assert_eq!(receiver.channels, 8);
        assert!(receiver.multicast);
        // 未监听的端口 → 明确的连接错误。
        let error = client
            .connect("rtsp://127.0.0.1:1/by-name/x")
            .expect_err("connect");
        assert_eq!(error.name(), "ERR_RTSP_CONNECT");
        server.stop();
    }

    #[test]
    fn rtsp_concurrent_clients_and_session_cleanup() {
        let config = RtspServerConfig::new(free_addr())
            .with_timeouts(Duration::from_secs(2), Duration::from_millis(200));
        let server = RtspServer::start(config, test_streams()).expect("server");
        let url = format!("rtsp://{}/by-name/Line%201", server.local_addr());
        let mut handles = Vec::new();
        for _ in 0..4 {
            let url = url.clone();
            handles.push(thread::spawn(move || {
                let client = RtspClient::with_timeout(Duration::from_secs(2));
                let mut connection = client.connect(&url).expect("connect");
                connection.describe().expect("describe");
                let id = connection.setup().expect("setup");
                connection.play().expect("play");
                connection.teardown().expect("teardown");
                id
            }));
        }
        let ids: Vec<u32> = handles
            .into_iter()
            .map(|handle| handle.join().expect("join"))
            .collect();
        let unique: std::collections::BTreeSet<u32> = ids.iter().copied().collect();
        assert_eq!(unique.len(), 4, "session ids must be unique");
        // 只 SETUP 不 TEARDOWN：会话超时后由清理线程回收。
        let mut connection = RtspClient::with_timeout(Duration::from_secs(2))
            .connect(&url)
            .expect("connect");
        connection.describe().expect("describe");
        connection.setup().expect("setup");
        assert!(wait_until(Duration::from_millis(300), || server
            .session_count()
            == 1));
        assert!(wait_until(Duration::from_secs(3), || server
            .session_count()
            == 0));
        drop(connection);
        server.stop();
    }

    #[test]
    fn rtsp_client_rejects_non_sdp_and_bad_status() {
        // 假服务端：DESCRIBE 返回 text/plain，客户端必须明确报 NotSdp。
        let listener = TcpListener::bind((LOCAL, 0)).expect("bind");
        let addr = listener.local_addr().expect("addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            stream
                .write_all(b"RTSP/1.0 200 OK\r\nCSeq: 1\r\nContent-Type: text/plain\r\nContent-Length: 2\r\n\r\nhi")
                .expect("write");
        });
        let client = RtspClient::with_timeout(Duration::from_secs(2));
        let error = client
            .connect(&format!("rtsp://{addr}/by-name/Line%201"))
            .and_then(|mut connection| connection.describe().map(|_| ()))
            .expect_err("must reject");
        assert_eq!(error.name(), "ERR_RTSP_NOT_SDP");
        handle.join().expect("join");
        // 假服务端：404 → BadStatus(404)。
        let listener = TcpListener::bind((LOCAL, 0)).expect("bind");
        let addr = listener.local_addr().expect("addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            stream
                .write_all(b"RTSP/1.0 404 Not Found\r\nCSeq: 1\r\n\r\n")
                .expect("write");
        });
        let error = client
            .connect(&format!("rtsp://{addr}/by-name/x"))
            .and_then(|mut connection| connection.describe().map(|_| ()))
            .expect_err("must reject");
        assert_eq!(error.name(), "ERR_RTSP_BAD_STATUS");
        handle.join().expect("join");
    }

    #[test]
    fn rtsp_url_parsing_and_defaults() {
        assert_eq!(
            parse_rtsp_url("rtsp://192.168.1.10/by-name/x").expect("url"),
            ("192.168.1.10".to_string(), RTSP_DEFAULT_PORT)
        );
        assert_eq!(
            parse_rtsp_url("rtsp://host:8854/by-id/1").expect("url"),
            ("host".to_string(), 8854)
        );
        assert!(parse_rtsp_url("http://host/x").is_err());
        assert_eq!(RTSP_DEFAULT_PORT, 554);
        assert_eq!(RTSP_DEFAULT_MULTICAST_TTL, 32);
        assert_eq!(RAVENNA_DEFAULT_RTSP_PORT, 8854);
    }
}
