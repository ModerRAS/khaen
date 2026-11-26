//! Byte transport abstraction and the std-only WebSocket transport.
//!
//! [Transport] is the injection point for tests and for the daemon: one
//! send_binary call is exactly one WebSocket binary frame carrying one
//! datastore message, and recv_binary returns the next complete binary message.

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use crate::error::CueMixError;
use crate::ws::{self, FrameDecoder, Message, MessageAssembler, Opcode};

/// Blocking byte transport used by the CueMix client.
pub trait Transport: Send {
    /// Send exactly one binary frame whose payload is one datastore message.
    fn send_binary(&mut self, payload: &[u8]) -> Result<(), CueMixError>;
    /// Receive the next complete binary message; None when the timeout expired.
    fn recv_binary(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, CueMixError>;
    /// Short transport description for logs/snapshots (e.g. "ws:1.2.3.4:1280").
    fn description(&self) -> String;
    /// Best-effort close.
    fn close(&mut self);
}

/// WebSocket connection parameters.
#[derive(Clone, Debug)]
pub struct WsConfig {
    /// Remote control endpoint.
    pub addr: SocketAddr,
    /// Request path (CueMix 5 uses "/").
    pub path: String,
    /// Overall connect + handshake timeout.
    pub connect_timeout: Duration,
    /// User-Agent header value.
    pub user_agent: String,
}

impl WsConfig {
    /// Options for the given endpoint with CueMix defaults.
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            path: "/".to_string(),
            connect_timeout: Duration::from_secs(2),
            user_agent: format!("khaen-motu/{}", crate::VERSION),
        }
    }
}

/// Std-only WebSocket transport (client role, binary messages only).
#[derive(Debug)]
pub struct WsTransport {
    stream: TcpStream,
    decoder: FrameDecoder,
    assembler: MessageAssembler,
    remote: SocketAddr,
    closed: bool,
}

impl WsTransport {
    /// TCP connect + RFC 6455 upgrade with Sec-WebSocket-Accept validation.
    pub fn connect(config: &WsConfig) -> Result<Self, CueMixError> {
        let mut stream = TcpStream::connect_timeout(&config.addr, config.connect_timeout)
            .map_err(|source| CueMixError::io("connect", source))?;
        stream
            .set_nodelay(true)
            .map_err(|source| CueMixError::io("set_nodelay", source))?;

        let host_header = config.addr.to_string();
        let handshake = ws::build_handshake_request(&host_header, &config.path, &config.user_agent);
        stream
            .set_write_timeout(Some(config.connect_timeout))
            .map_err(|source| CueMixError::io("set write timeout", source))?;
        stream
            .write_all(handshake.request.as_bytes())
            .map_err(|source| CueMixError::io("handshake write", source))?;

        let deadline = Instant::now() + config.connect_timeout;
        let mut buffer = Vec::with_capacity(2048);
        let rest = loop {
            if let Some(position) = ws::find_handshake_end(&buffer) {
                let head = buffer[..position].to_vec();
                let rest = buffer[position + 4..].to_vec();
                let response = ws::parse_handshake_head(&head)?;
                ws::validate_handshake(&response, &handshake.key)?;
                break rest;
            }
            if buffer.len() > ws::MAX_HANDSHAKE_BYTES {
                return Err(CueMixError::Handshake {
                    reason: "handshake response too large".to_string(),
                });
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(CueMixError::timeout("handshake"));
            }
            stream
                .set_read_timeout(Some(remaining.max(Duration::from_millis(1))))
                .map_err(|source| CueMixError::io("set read timeout", source))?;
            let mut chunk = [0u8; 4096];
            match stream.read(&mut chunk) {
                Ok(0) => return Err(CueMixError::Closed),
                Ok(count) => buffer.extend_from_slice(&chunk[..count]),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    continue
                }
                Err(error) => return Err(CueMixError::io("handshake read", error)),
            }
        };

        let mut decoder = FrameDecoder::new();
        decoder.push(&rest);
        Ok(Self {
            stream,
            decoder,
            assembler: MessageAssembler::new(),
            remote: config.addr,
            closed: false,
        })
    }

    fn write_raw(&mut self, bytes: &[u8]) -> Result<(), CueMixError> {
        if self.closed {
            return Err(CueMixError::Closed);
        }
        self.stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|source| CueMixError::io("set write timeout", source))?;
        self.stream
            .write_all(bytes)
            .and_then(|()| self.stream.flush())
            .map_err(|source| CueMixError::io("write", source))
    }

    /// Read once from the socket into the frame decoder.
    ///
    /// Returns false on read timeout (no bytes were available inside the wait).
    fn pump(&mut self, wait: Duration) -> Result<bool, CueMixError> {
        let wait = wait.max(Duration::from_millis(1));
        self.stream
            .set_read_timeout(Some(wait))
            .map_err(|source| CueMixError::io("set read timeout", source))?;
        let mut chunk = [0u8; 16 * 1024];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err(CueMixError::Closed),
                Ok(count) => {
                    self.decoder.push(&chunk[..count]);
                    return Ok(true);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(false)
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(CueMixError::io("read", error)),
            }
        }
    }

    fn acknowledge_close(&mut self) {
        self.closed = true;
        let frame = ws::encode_close(Some(1000), "");
        let _ = self.stream.write_all(&frame);
        let _ = self.stream.flush();
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

impl Transport for WsTransport {
    fn send_binary(&mut self, payload: &[u8]) -> Result<(), CueMixError> {
        let frame = ws::encode_binary(payload);
        self.write_raw(&frame)
    }

    fn recv_binary(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, CueMixError> {
        let deadline = Instant::now() + timeout;
        loop {
            while let Some(frame) = self.decoder.next_frame()? {
                match frame.opcode {
                    Opcode::Ping => {
                        let pong = ws::encode_pong(&frame.payload);
                        self.write_raw(&pong)?;
                    }
                    Opcode::Pong => {}
                    Opcode::Close => {
                        self.acknowledge_close();
                        return Err(CueMixError::Closed);
                    }
                    Opcode::Binary | Opcode::Text | Opcode::Continuation => {
                        if let Some(message) = self.assembler.push(frame)? {
                            match message {
                                Message::Binary(payload) => return Ok(Some(payload)),
                                Message::Text(_) => continue,
                            }
                        }
                    }
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            if !self.pump(remaining)? {
                return Ok(None);
            }
        }
    }

    fn description(&self) -> String {
        format!("ws:{}", self.remote)
    }

    fn close(&mut self) {
        if !self.closed {
            self.acknowledge_close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    /// Minimal one-shot WebSocket server used to prove handshake + ping/pong +
    /// binary round trip over a real TCP socket.
    fn spawn_test_server() -> (SocketAddr, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            // Read the handshake request head.
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match stream.read(&mut byte) {
                    Ok(1) => head.push(byte[0]),
                    _ => return,
                }
                if head.len() > 8192 {
                    return;
                }
            }
            let request = String::from_utf8_lossy(&head).to_string();
            let key = request
                .lines()
                .find_map(|line| line.strip_prefix("Sec-WebSocket-Key: "))
                .unwrap_or("")
                .trim()
                .to_string();
            let accept = crate::crypto::websocket_accept_key(&key);
            let response = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
                accept
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
            // Unmasked ping followed by an unmasked binary message.
            stream
                .write_all(&ws::encode_frame(true, Opcode::Ping, b"hi", None))
                .expect("write ping");
            stream
                .write_all(&ws::encode_frame(true, Opcode::Binary, b"datastore", None))
                .expect("write binary");
            // 期望先收到被掩码的 pong，再收到客户端的二进制帧。
            // 一次 read 可能同时拿到 pong 与 binary（TCP 合并），也可能因调度推迟：
            // 用截止时间轮询并按帧类型分别处理——不能丢帧，也不能赌单次 read 的时序。
            let mut decoder = FrameDecoder::new();
            let mut pong_seen = false;
            let mut response_bytes = Vec::new();
            let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
            let deadline = Instant::now() + Duration::from_secs(5);
            while (!pong_seen || response_bytes.is_empty()) && Instant::now() < deadline {
                let mut chunk = [0u8; 1024];
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(count) => decoder.push(&chunk[..count]),
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) => {}
                    Err(_) => break,
                }
                while let Some(frame) = decoder.next_frame().expect("decode") {
                    match frame.opcode {
                        Opcode::Pong => {
                            assert_eq!(frame.payload, b"hi".to_vec());
                            pong_seen = true;
                        }
                        Opcode::Binary => response_bytes.extend_from_slice(&frame.payload),
                        _ => {}
                    }
                }
            }
            assert!(pong_seen, "server must see the client pong");
            assert_eq!(response_bytes, b"write".to_vec());
        });
        (addr, handle)
    }

    #[test]
    fn handshake_ping_and_binary_roundtrip_over_tcp() {
        let (addr, handle) = spawn_test_server();
        let mut config = WsConfig::new(addr);
        config.connect_timeout = Duration::from_secs(5);
        let mut transport = WsTransport::connect(&config).expect("connect");
        assert_eq!(transport.description(), format!("ws:{}", addr));
        let payload = transport
            .recv_binary(Duration::from_secs(5))
            .expect("recv")
            .expect("binary message");
        assert_eq!(payload, b"datastore".to_vec());
        transport.send_binary(b"write").expect("send");
        transport.close();
        handle.join().expect("server thread");
    }

    #[test]
    fn connect_refused_yields_io_error() {
        // Bind then drop to get a port that is very likely closed.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        let mut config = WsConfig::new(addr);
        config.connect_timeout = Duration::from_millis(200);
        let error = WsTransport::connect(&config).expect_err("must fail");
        assert_eq!(error.code(), crate::error::codes::IO);
    }
}
