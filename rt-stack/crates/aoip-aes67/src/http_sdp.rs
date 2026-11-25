//! 极简 HTTP/1.1 SDP 分发服务端 + 客户端（RAVENNA 配置/发现面的一部分）。
//!
//! 规范依据：
//! - RAVENNA Operating Principles Draft 1.0（scratch/ravenna/research/pdf/op-principles.txt）：
//!   - 每个节点 RECOMMENDED 提供 HTTP 配置服务；sink 节点 MUST 发布 HTTP 配置服务：
//!     §3.4 L563-569、§3.5 L603-604；
//!   - 服务以标准服务类型 _http._tcp 广告：§3.5.1 L607-622。
//! - RAVENNA Network Requirements Draft 0.3（scratch/ravenna/research/pdf/net-req.txt）：
//!   HTTP 配置端口可配、默认 8080：§2.6 L186-190。
//! - AES67-2023 Annex E.6（docs/text/aes67-2023-draft-231218.txt L2721-2732）：发现/取 SDP 的主路径
//!   是 DNS-SD + RTSP；HTTP 只承载设备配置（本模块按任务要求提供 SDP 分发的极简 GET 端点，
//!   供无法走 RTSP 的对端/调试工具直接取 SDP；RAVENNA OP 未规定 HTTP 取 SDP 的路径，属 EXTENSION）。
//! - HTTP/1.1 报文格式：本机语料未含 RFC 7230/2616 原文，实现按通用 HTTP/1.1 语法：
//!   请求行 METHOD SP request-target SP HTTP-version CRLF、头部 CRLF 分隔、空行、实体；
//!   响应状态行 + 头部 + 空行 + 实体；GET/HEAD 语义与 404/405 状态码为通用 HTTP 语义。
//!   逐字段行为在测试里用真实 loopback 报文验证（见报告 §EXTENSION/UNVERIFIED）。
//!
//! 实现口径：
//! - 仅 IPv4；路径 GET /<stream>.sdp（stream 名即 RtspStream/HTTP 流名）；
//! - 200 带 Content-Type: application/sdp 与 Content-Length（HTTP/1.1 必需）；
//! - 未知流 404、非 GET 方法 405（带 Allow: GET）、畸形请求 400、非 HTTP/1.x 505；
//! - 客户端 GET 拉取并校验状态码/Content-Type/Content-Length；
//! - 纯 std、零外部依赖；每连接一个线程；Connection: close（极简，不实现 keep-alive/分块）。

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::receiver::{ReceiverConfig, ReceiverError};
use crate::sdp::{self, Session};

/// HTTP 配置服务默认端口（RAVENNA Network Requirements §2.6 L186-190：默认 8080）。
pub const HTTP_DEFAULT_PORT: u16 = 8080;
/// HTTP 请求头上限。
pub const HTTP_MAX_HEADER_BYTES: usize = 8 * 1024;
/// HTTP 实体上限（SDP 很小；同 RTSP 口径）。
pub const HTTP_MAX_BODY_BYTES: usize = 64 * 1024;

/// HTTP SDP 错误（稳定错误名）。
#[derive(Debug)]
pub enum HttpSdpError {
    /// 报文不完整（内部解析用）。
    Incomplete,
    /// I/O 错误。
    Io(io::Error),
    /// 连接失败。
    Connect(io::Error),
    /// 协议错误。
    Protocol(&'static str),
    /// 非 200 响应。
    BadStatus(u16),
    /// Content-Type 不是 application/sdp。
    NotSdp,
    /// 请求的方法不支持（服务端内部；对外映射 405）。
    MethodNotAllowed,
    /// SDP 解析错误。
    Sdp(sdp::SdpError),
    /// receiver 配置构造错误。
    Receiver(ReceiverError),
}

impl HttpSdpError {
    /// 稳定错误名。
    pub const fn name(&self) -> &'static str {
        match self {
            HttpSdpError::Incomplete => "ERR_HTTP_INCOMPLETE",
            HttpSdpError::Io(_) => "ERR_HTTP_IO",
            HttpSdpError::Connect(_) => "ERR_HTTP_CONNECT",
            HttpSdpError::Protocol(_) => "ERR_HTTP_PROTOCOL",
            HttpSdpError::BadStatus(_) => "ERR_HTTP_BAD_STATUS",
            HttpSdpError::NotSdp => "ERR_HTTP_NOT_SDP",
            HttpSdpError::MethodNotAllowed => "ERR_HTTP_METHOD_NOT_ALLOWED",
            HttpSdpError::Sdp(_) => "ERR_HTTP_SDP",
            HttpSdpError::Receiver(_) => "ERR_HTTP_RECEIVER",
        }
    }
}

impl fmt::Display for HttpSdpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpSdpError::Incomplete => f.write_str("incomplete HTTP message"),
            HttpSdpError::Io(e) => write!(f, "http io: {e}"),
            HttpSdpError::Connect(e) => write!(f, "http connect: {e}"),
            HttpSdpError::Protocol(what) => write!(f, "http protocol: {what}"),
            HttpSdpError::BadStatus(code) => write!(f, "http unexpected status {code}"),
            HttpSdpError::NotSdp => f.write_str("http content-type is not application/sdp"),
            HttpSdpError::MethodNotAllowed => f.write_str("http method not allowed"),
            HttpSdpError::Sdp(e) => write!(f, "http sdp: {e:?}"),
            HttpSdpError::Receiver(e) => write!(f, "http receiver: {e:?}"),
        }
    }
}

impl std::error::Error for HttpSdpError {}

impl From<io::Error> for HttpSdpError {
    fn from(error: io::Error) -> Self {
        HttpSdpError::Io(error)
    }
}

impl From<sdp::SdpError> for HttpSdpError {
    fn from(error: sdp::SdpError) -> Self {
        HttpSdpError::Sdp(error)
    }
}

type HttpResult<T> = Result<T, HttpSdpError>;

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn trim_ows(value: &str) -> &str {
    value.trim_matches(|c| c == ' ' || c == '\t')
}

/// 一条 HTTP 请求（本模块只关心请求行与少量头）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRequest {
    /// 方法（GET/HEAD/...）。
    pub method: String,
    /// request-target（origin-form，如 /Line%201.sdp）。
    pub target: String,
    /// HTTP 版本（如 1.1）。
    pub version: String,
    /// 头字段。
    pub headers: Vec<(String, String)>,
}

impl HttpRequest {
    /// 解析请求（数据不完整返回 Incomplete）。
    pub fn parse(buffer: &[u8]) -> HttpResult<(Self, usize)> {
        let Some(end) = find_header_end(buffer) else {
            if buffer.len() > HTTP_MAX_HEADER_BYTES {
                return Err(HttpSdpError::Protocol("header too large"));
            }
            return Err(HttpSdpError::Incomplete);
        };
        if end > HTTP_MAX_HEADER_BYTES {
            return Err(HttpSdpError::Protocol("header too large"));
        }
        let head =
            std::str::from_utf8(&buffer[..end]).map_err(|_| HttpSdpError::Protocol("non-utf8"))?;
        let mut lines = head.split("\r\n");
        let request_line = lines
            .next()
            .ok_or(HttpSdpError::Protocol("empty request"))?;
        let mut parts = request_line.split(' ');
        let method = parts.next().unwrap_or_default();
        let target = parts.next().unwrap_or_default();
        let version_token = parts.next().unwrap_or_default();
        if method.is_empty() || target.is_empty() || parts.next().is_some() {
            return Err(HttpSdpError::Protocol("bad request line"));
        }
        let version = version_token
            .strip_prefix("HTTP/")
            .ok_or(HttpSdpError::Protocol("bad http version"))?;
        let mut headers = Vec::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            let Some((name, value)) = line.split_once(':') else {
                return Err(HttpSdpError::Protocol("bad header line"));
            };
            headers.push((name.to_string(), trim_ows(value).to_string()));
        }
        Ok((
            Self {
                method: method.to_string(),
                target: target.to_string(),
                version: version.to_string(),
                headers,
            },
            end + 4,
        ))
    }

    /// 大小写不敏感取头。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

fn percent_decode(input: &str) -> HttpResult<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(HttpSdpError::Protocol("bad percent escape"));
            }
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3])
                .map_err(|_| HttpSdpError::Protocol("bad percent escape"))?;
            let value = u8::from_str_radix(hex, 16)
                .map_err(|_| HttpSdpError::Protocol("bad percent escape"))?;
            out.push(value);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).map_err(|_| HttpSdpError::Protocol("path is not utf-8"))
}

/// 一条可由 HTTP GET 分发的 SDP 流（路径为 /<name>.sdp）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpSdpStream {
    /// 流名（对应 RTSP 会话名；路径 = /<name>.sdp）。
    pub name: String,
    /// SDP 文本。
    pub sdp: String,
}

impl HttpSdpStream {
    /// 构造。
    pub fn new(name: impl Into<String>, sdp: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            sdp: sdp.into(),
        }
    }

    /// 请求路径（/name.sdp；名字里的空格等字符按百分号编码由客户端负责）。
    pub fn path(&self) -> String {
        format!("/{}.sdp", self.name)
    }

    /// 路径匹配（百分号解码后比较）。
    fn matches_target(&self, target: &str) -> bool {
        let target = target.split(['?', '#']).next().unwrap_or(target);
        let Ok(decoded) = percent_decode(target) else {
            return false;
        };
        decoded == self.path()
    }
}

/// HTTP SDP 服务端配置。
#[derive(Clone, Debug)]
pub struct HttpSdpServerConfig {
    /// 监听地址（IPv4）。
    pub bind: SocketAddr,
    /// 连接读写超时。
    pub timeout: Duration,
    /// Server 头值。
    pub server_name: String,
}

impl HttpSdpServerConfig {
    /// 以监听地址构造默认配置。
    pub fn new(bind: SocketAddr) -> Self {
        Self {
            bind,
            timeout: Duration::from_secs(5),
            server_name: "tspi-aoip-aes67".to_string(),
        }
    }
}

/// HTTP SDP 服务端（GET /<stream>.sdp → 200 application/sdp）。
#[derive(Debug)]
pub struct HttpSdpServer {
    local_addr: SocketAddr,
    streams: Arc<Vec<HttpSdpStream>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl HttpSdpServer {
    /// 启动服务端（accept 线程 + 每连接线程）。
    pub fn start(config: HttpSdpServerConfig, streams: Vec<HttpSdpStream>) -> HttpResult<Self> {
        let listener = TcpListener::bind(config.bind)?;
        listener.set_nonblocking(true)?;
        let local_addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let streams = Arc::new(streams);
        let thread_streams = Arc::clone(&streams);
        let thread_config = config.clone();
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("http-sdp-accept".to_string())
            .spawn(move || loop {
                if thread_stop.load(Ordering::Relaxed) {
                    break;
                }
                match listener.accept() {
                    Ok((stream, _peer)) => {
                        let streams = Arc::clone(&thread_streams);
                        let config = thread_config.clone();
                        let _ = thread::Builder::new()
                            .name("http-sdp-conn".to_string())
                            .spawn(move || {
                                let _ = handle_connection(stream, &streams, &config);
                            });
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            })
            .map_err(HttpSdpError::Io)?;
        Ok(Self {
            local_addr,
            streams,
            stop,
            thread: Some(thread),
        })
    }

    /// 实际监听地址。
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// 流名 → SDP（测试/诊断用）。
    pub fn stream_sdp(&self, name: &str) -> Option<String> {
        self.streams
            .iter()
            .find(|stream| stream.name == name)
            .map(|stream| stream.sdp.clone())
    }

    /// 停止服务端。
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for HttpSdpServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> io::Result<()> {
    let mut out = Vec::with_capacity(128 + body.len());
    out.extend_from_slice(b"HTTP/1.1 ");
    out.extend_from_slice(status.to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(reason.as_bytes());
    out.extend_from_slice(b"\r\n");
    for (name, value) in headers {
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    stream.write_all(&out)?;
    stream.flush()
}

fn handle_connection(
    mut stream: TcpStream,
    streams: &[HttpSdpStream],
    config: &HttpSdpServerConfig,
) -> HttpResult<()> {
    let _ = stream.set_nonblocking(false);
    stream.set_read_timeout(Some(config.timeout))?;
    stream.set_write_timeout(Some(config.timeout))?;
    let mut buffer: Vec<u8> = Vec::with_capacity(2048);
    let mut chunk = [0u8; 4096];
    loop {
        if let Ok((request, _consumed)) = HttpRequest::parse(&buffer) {
            let server = config.server_name.as_str();
            if request.version != "1.0" && request.version != "1.1" {
                return write_response(
                    &mut stream,
                    505,
                    "HTTP Version Not Supported",
                    &[("Server", server), ("Connection", "close")],
                    b"",
                )
                .map_err(HttpSdpError::Io);
            }
            if request.method != "GET" && request.method != "HEAD" {
                return write_response(
                    &mut stream,
                    405,
                    "Method Not Allowed",
                    &[
                        ("Server", server),
                        ("Allow", "GET, HEAD"),
                        ("Connection", "close"),
                    ],
                    b"",
                )
                .map_err(HttpSdpError::Io);
            }
            let Some(found) = streams
                .iter()
                .find(|stream| stream.matches_target(&request.target))
            else {
                return write_response(
                    &mut stream,
                    404,
                    "Not Found",
                    &[("Server", server), ("Connection", "close")],
                    b"",
                )
                .map_err(HttpSdpError::Io);
            };
            let body = if request.method == "HEAD" {
                &[][..]
            } else {
                found.sdp.as_bytes()
            };
            let length = found.sdp.len().to_string();
            return write_response(
                &mut stream,
                200,
                "OK",
                &[
                    ("Server", server),
                    ("Content-Type", "application/sdp"),
                    ("Content-Length", length.as_str()),
                    ("Connection", "close"),
                ],
                body,
            )
            .map_err(HttpSdpError::Io);
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(read) => {
                buffer.extend_from_slice(&chunk[..read]);
                if buffer.len() > HTTP_MAX_HEADER_BYTES + HTTP_MAX_BODY_BYTES {
                    return write_response(
                        &mut stream,
                        400,
                        "Bad Request",
                        &[
                            ("Server", config.server_name.as_str()),
                            ("Connection", "close"),
                        ],
                        b"",
                    )
                    .map_err(HttpSdpError::Io);
                }
            }
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.kind() == io::ErrorKind::TimedOut =>
            {
                return Ok(());
            }
            Err(error) => return Err(HttpSdpError::Io(error)),
        }
    }
}

fn parse_http_url(url: &str) -> HttpResult<(String, u16, String)> {
    let rest = url
        .strip_prefix("http://")
        .ok_or(HttpSdpError::Protocol("url must start with http://"))?;
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (
            host,
            port.parse::<u16>()
                .map_err(|_| HttpSdpError::Protocol("bad port"))?,
        ),
        None => (authority, HTTP_DEFAULT_PORT),
    };
    if host.is_empty() {
        return Err(HttpSdpError::Protocol("empty host"));
    }
    Ok((host.to_string(), port, path.to_string()))
}

/// 一次成功的 HTTP GET 结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpSdpResponse {
    /// 请求 URL。
    pub url: String,
    /// 状态码（正常路径恒 200）。
    pub status: u16,
    /// Content-Type 原值。
    pub content_type: String,
    /// SDP 文本。
    pub sdp: String,
}

impl HttpSdpResponse {
    /// 解析 SDP（借用）。
    pub fn parse(&self) -> HttpResult<Session<'_>> {
        sdp::parse_sdp(&self.sdp).map_err(HttpSdpError::Sdp)
    }

    /// 解析 SDP 并构造既有 receiver 配置。
    pub fn receiver_config(&self, interface: Option<Ipv4Addr>) -> HttpResult<ReceiverConfig> {
        let session = self.parse()?;
        ReceiverConfig::from_session(&session, interface).map_err(HttpSdpError::Receiver)
    }
}

/// HTTP 客户端：GET 拉取 SDP 文本（仅 IPv4）。
pub fn http_get_sdp(url: &str, timeout: Duration) -> HttpResult<HttpSdpResponse> {
    let (host, port, path) = parse_http_url(url)?;
    let mut stream = TcpStream::connect((host.as_str(), port)).map_err(HttpSdpError::Connect)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let host_header = if port == HTTP_DEFAULT_PORT {
        host.clone()
    } else {
        format!("{host}:{port}")
    };
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host_header}\r\nAccept: application/sdp\r\nUser-Agent: tspi-aoip-aes67\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(HttpSdpError::Io)?;
    stream.flush().map_err(HttpSdpError::Io)?;
    let mut raw: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).map_err(|error| {
            if error.kind() == io::ErrorKind::TimedOut || error.kind() == io::ErrorKind::WouldBlock
            {
                HttpSdpError::Protocol("timeout")
            } else {
                HttpSdpError::Io(error)
            }
        })?;
        if read == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..read]);
        if raw.len() > HTTP_MAX_HEADER_BYTES + HTTP_MAX_BODY_BYTES {
            return Err(HttpSdpError::Protocol("response too large"));
        }
    }
    let Some(end) = find_header_end(&raw) else {
        return Err(HttpSdpError::Protocol("incomplete response"));
    };
    let head = std::str::from_utf8(&raw[..end])
        .map_err(|_| HttpSdpError::Protocol("non-utf8 response header"))?;
    let mut lines = head.split("\r\n");
    let status_line = lines
        .next()
        .ok_or(HttpSdpError::Protocol("empty response"))?;
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") {
        return Err(HttpSdpError::Protocol("bad http version"));
    }
    let status = parts
        .next()
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or(HttpSdpError::Protocol("bad status line"))?;
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(HttpSdpError::Protocol("bad header line"));
        };
        headers.push((name.to_string(), trim_ows(value).to_string()));
    }
    if status != 200 {
        return Err(HttpSdpError::BadStatus(status));
    }
    let lookup = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };
    let content_type =
        lookup("content-type").ok_or(HttpSdpError::Protocol("missing content-type"))?;
    if !content_type
        .to_ascii_lowercase()
        .starts_with("application/sdp")
    {
        return Err(HttpSdpError::NotSdp);
    }
    let content_length = lookup("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or(HttpSdpError::Protocol("missing content-length"))?;
    let body = &raw[end + 4..];
    if body.len() < content_length {
        return Err(HttpSdpError::Protocol("short body"));
    }
    let sdp = String::from_utf8(body[..content_length].to_vec())
        .map_err(|_| HttpSdpError::Protocol("sdp is not utf-8"))?;
    if sdp.is_empty() {
        return Err(HttpSdpError::Protocol("empty sdp"));
    }
    Ok(HttpSdpResponse {
        url: url.to_string(),
        status,
        content_type: content_type.to_string(),
        sdp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdp::{Connection, Direction, Encoding, MediaClk, PtpRefClk, RefClk};
    use crate::sdp_write::{SdpDescription, SdpMedia, SdpOrigin};
    use std::net::Ipv4Addr;

    const LOCAL: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 1);

    fn test_sdp() -> String {
        SdpDescription::new(
            SdpOrigin::new(1311738121, 1, Ipv4Addr::new(192, 168, 63, 222)),
            "HTTP SDP test",
            Connection {
                address: Ipv4Addr::new(239, 69, 1, 2).octets(),
                ttl: Some(32),
            },
            SdpMedia {
                port: 5006,
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
        )
        .to_sdp()
        .expect("sdp")
    }

    fn free_addr() -> SocketAddr {
        let listener = TcpListener::bind((LOCAL, 0)).expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        addr
    }

    fn start_server(streams: Vec<HttpSdpStream>) -> HttpSdpServer {
        HttpSdpServer::start(HttpSdpServerConfig::new(free_addr()), streams).expect("server")
    }

    #[test]
    fn http_request_parse_roundtrip() {
        let raw = b"GET /Line%201.sdp HTTP/1.1\r\nHost: 127.0.0.1:8080\r\nAccept: application/sdp\r\n\r\n";
        let (request, consumed) = HttpRequest::parse(raw).expect("parse");
        assert_eq!(consumed, raw.len());
        assert_eq!(request.method, "GET");
        assert_eq!(request.target, "/Line%201.sdp");
        assert_eq!(request.version, "1.1");
        assert_eq!(request.header("host"), Some("127.0.0.1:8080"));
        assert!(matches!(
            HttpRequest::parse(&raw[..raw.len() - 2]),
            Err(HttpSdpError::Incomplete)
        ));
        assert!(HttpRequest::parse(b"GET /x HTTP/1.1\r\nbadheader\r\n\r\n").is_err());
        // 路径转义解码与匹配。
        let stream = HttpSdpStream::new("Line 1", "v=0");
        assert!(stream.matches_target("/Line%201.sdp"));
        assert!(stream.matches_target("/Line%201.sdp?x=1"));
        assert!(!stream.matches_target("/Line%202.sdp"));
    }

    #[test]
    fn http_get_sdp_roundtrip() {
        let server = start_server(vec![HttpSdpStream::new("Line 1", test_sdp())]);
        let url = format!("http://{}/Line%201.sdp", server.local_addr());
        let response = http_get_sdp(&url, Duration::from_secs(2)).expect("get");
        assert_eq!(response.status, 200);
        assert_eq!(response.sdp, test_sdp());
        assert_eq!(response.content_type, "application/sdp");
        let receiver = response.receiver_config(Some(LOCAL)).expect("receiver cfg");
        assert_eq!(receiver.port, 5006);
        assert_eq!(receiver.channels, 8);
        // 名字编码：%20 与空格等价（服务端按百分号解码后比较）。
        let stream = HttpSdpStream::new("Line 1", test_sdp());
        assert!(stream.matches_target("/Line%201.sdp"));
        assert!(!stream.matches_target("/Line%202.sdp"));
        server.stop();
    }

    #[test]
    fn http_404_405_400_and_version_errors() {
        let server = start_server(vec![HttpSdpStream::new("Line 1", test_sdp())]);
        let addr = server.local_addr();
        // 404 未知流。
        let mut stream = TcpStream::connect(addr).expect("connect");
        stream
            .write_all(b"GET /Missing.sdp HTTP/1.1\r\nHost: x\r\n\r\n")
            .expect("write");
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).expect("read");
        let text = String::from_utf8_lossy(&raw);
        assert!(text.starts_with("HTTP/1.1 404 Not Found\r\n"), "{text}");
        // 405 非 GET；必须带 Allow。
        let mut stream = TcpStream::connect(addr).expect("connect");
        stream
            .write_all(b"POST /Line%201.sdp HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n")
            .expect("write");
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).expect("read");
        let text = String::from_utf8_lossy(&raw);
        assert!(
            text.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"),
            "{text}"
        );
        assert!(text.to_ascii_lowercase().contains("allow: get, head"));
        // 505 版本不支持。
        let mut stream = TcpStream::connect(addr).expect("connect");
        stream
            .write_all(b"GET /Line%201.sdp HTTP/2.0\r\nHost: x\r\n\r\n")
            .expect("write");
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).expect("read");
        let text = String::from_utf8_lossy(&raw);
        assert!(
            text.starts_with("HTTP/1.1 505 HTTP Version Not Supported\r\n"),
            "{text}"
        );
        // 200 的头部断言：Content-Type/Content-Length 正确。
        let mut stream = TcpStream::connect(addr).expect("connect");
        stream
            .write_all(b"GET /Line%201.sdp HTTP/1.1\r\nHost: x\r\n\r\n")
            .expect("write");
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).expect("read");
        let text = String::from_utf8_lossy(&raw);
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text}");
        assert!(text.contains("Content-Type: application/sdp\r\n"));
        assert!(text.contains(&format!("Content-Length: {}\r\n", test_sdp().len())));
        assert!(text.ends_with(&test_sdp()));
        server.stop();
    }

    #[test]
    fn http_client_rejects_non_sdp_and_bad_status() {
        let listener = TcpListener::bind((LOCAL, 0)).expect("bind");
        let addr = listener.local_addr().expect("addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let body = b"hello";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            stream.write_all(response.as_bytes()).expect("write");
            stream.write_all(body).expect("write body");
        });
        let error = http_get_sdp(
            &format!("http://{addr}/Line%201.sdp"),
            Duration::from_secs(2),
        )
        .expect_err("must reject");
        assert_eq!(error.name(), "ERR_HTTP_NOT_SDP");
        handle.join().expect("join");
        // 500 → BadStatus。
        let listener = TcpListener::bind((LOCAL, 0)).expect("bind");
        let addr = listener.local_addr().expect("addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            stream
                .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n")
                .expect("write");
        });
        let error = http_get_sdp(&format!("http://{addr}/x.sdp"), Duration::from_secs(2))
            .expect_err("must reject");
        assert_eq!(error.name(), "ERR_HTTP_BAD_STATUS");
        handle.join().expect("join");
        // 连接失败 → Connect。
        let error = http_get_sdp("http://127.0.0.1:1/x.sdp", Duration::from_secs(1))
            .expect_err("must reject");
        assert_eq!(error.name(), "ERR_HTTP_CONNECT");
    }

    #[test]
    fn http_and_rtsp_serve_identical_sdp() {
        let sdp = test_sdp();
        let http = start_server(vec![HttpSdpStream::new("Line 1", sdp.clone())]);
        let rtsp = crate::rtsp::RtspServer::start(
            crate::rtsp::RtspServerConfig::new(free_addr()),
            vec![crate::rtsp::RtspStream::new("Line 1", sdp.clone())],
        )
        .expect("rtsp server");
        let via_http = http_get_sdp(
            &format!("http://{}/Line%201.sdp", http.local_addr()),
            Duration::from_secs(2),
        )
        .expect("http get");
        let via_rtsp = crate::rtsp::RtspClient::with_timeout(Duration::from_secs(2))
            .describe(&format!("rtsp://{}/by-name/Line%201", rtsp.local_addr()))
            .expect("rtsp describe");
        assert_eq!(via_http.sdp, sdp);
        assert_eq!(via_rtsp.sdp, sdp);
        assert_eq!(
            via_http.receiver_config(Some(LOCAL)).expect("cfg").port,
            via_rtsp.receiver_config(Some(LOCAL)).expect("cfg").port
        );
        rtsp.stop();
        http.stop();
    }
}
