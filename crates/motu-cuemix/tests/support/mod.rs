//! Independent minimal WebSocket/device simulator for the integration tests.
//!
//! This module deliberately re-implements RFC 6455 framing, SHA-1/base64 and
//! the datastore message layout instead of reusing the crate internals, so the
//! tests validate the client against an independent implementation.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

pub const PROXY_ID: u16 = 0xFFFE;
pub const PROXY_SUB_PUBLIC_KEY: u16 = 6;
pub const PROXY_SUB_PASSWORD_STATUS: u16 = 2;
pub const PROXY_SUB_AUTH_REQUEST: u16 = 0;
pub const PROXY_SUB_AUTH_RESPONSE: u16 = 1;

pub const MUTE_ENABLE_ID: u16 = 5019;
pub const SAMPLE_RATE_ID: u16 = 10;
pub const KO_BUS_FADER_ID: u16 = 1027;
pub const KO_BUS_MUTE_ID: u16 = 1028;
pub const MAIN_TRIM_ID: u16 = 5011;
pub const MODEL_ID_ID: u16 = 21;
pub const DEV_NAME_ID: u16 = 3;
pub const DEV_UID_ID: u16 = 2;
pub const API_VERSION_ID: u16 = 8;

/// 2048-bit RSA public key used as the fake device key (test fixture).
pub const RSA2048_PEM_SPKI: &str = "-----BEGIN PUBLIC KEY-----\nMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAl292mpHCyN5IFXfpomM5\nRStK7F62t3CJUJVXxi53EsQSyOKjVZUwCm2cnWaKKqltEI4t2dgg5HrwqpEv4JSj\niXRYumKWeGf7EPG0UG06PnlGHIHVjLIs3hapLclZXcc7YL9EcI53oK/0qf3RKyhx\nKwLfd93W1Wm1h91uSRQDAD1+9oNQKr5NndVs0F9TZot6d3SXDGy40ONmDGUdqw+7\nr3R06LdqXKWid3hbdtT3skVS2xVQx/3zWmjRI8VqexrsjfRLxA5boLL7KgrPtvBn\nORFzoEarbu2pQYEJj3QLDRCplty3t8sfYL+mwItunaip1l706qPntCWY7or/qf4J\nEwIDAQAB\n-----END PUBLIC KEY-----\n";

// ------------------------------------------------------------- SHA-1/base64

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut state: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let mut blocks = data.chunks_exact(64);
    for block in blocks.by_ref() {
        sha1_block(&mut state, block);
    }
    let rest = blocks.remainder();
    let mut tail = [0u8; 128];
    tail[..rest.len()].copy_from_slice(rest);
    tail[rest.len()] = 0x80;
    let tail_len = if rest.len() < 56 { 64 } else { 128 };
    let bits = (data.len() as u64) * 8;
    tail[tail_len - 8..tail_len].copy_from_slice(&bits.to_be_bytes());
    for block in tail[..tail_len].chunks_exact(64) {
        sha1_block(&mut state, block);
    }
    let mut out = [0u8; 20];
    for (index, word) in state.iter().enumerate() {
        out[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn sha1_block(state: &mut [u32; 5], block: &[u8]) {
    let mut w = [0u32; 80];
    for (index, chunk) in block.chunks_exact(4).enumerate() {
        w[index] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    for index in 16..80 {
        w[index] = (w[index - 3] ^ w[index - 8] ^ w[index - 14] ^ w[index - 16]).rotate_left(1);
    }
    let mut a = state[0];
    let mut b = state[1];
    let mut c = state[2];
    let mut d = state[3];
    let mut e = state[4];
    for (index, word) in w.iter().enumerate() {
        let (f, k) = if index < 20 {
            ((b & c) | ((!b) & d), 0x5A82_7999u32)
        } else if index < 40 {
            (b ^ c ^ d, 0x6ED9_EBA1)
        } else if index < 60 {
            ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC)
        } else {
            (b ^ c ^ d, 0xCA62_C1D6)
        };
        let temp = a
            .rotate_left(5)
            .wrapping_add(f)
            .wrapping_add(e)
            .wrapping_add(k)
            .wrapping_add(*word);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = temp;
    }
    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
}

pub fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let first = chunk[0] as u32;
        let second = chunk.get(1).copied().unwrap_or(0) as u32;
        let third = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (first << 16) | (second << 8) | third;
        out.push(TABLE[((triple >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

pub fn accept_key(key: &str) -> String {
    let mut data = Vec::new();
    data.extend_from_slice(key.as_bytes());
    data.extend_from_slice(WS_GUID.as_bytes());
    base64(&sha1(&data))
}

// ------------------------------------------------------------------- frames

pub fn frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 10);
    out.push(0x80 | opcode);
    match payload.len() {
        len @ 0..=125 => out.push(len as u8),
        len @ 126..=65535 => {
            out.push(126);
            out.extend_from_slice(&(len as u16).to_be_bytes());
        }
        len => {
            out.push(127);
            out.extend_from_slice(&(len as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(payload);
    out
}

pub fn fragmented_frames(payload: &[u8], pieces: usize) -> Vec<Vec<u8>> {
    let chunk_size = payload.len().div_ceil(pieces).max(1);
    let chunks: Vec<&[u8]> = payload.chunks(chunk_size).collect();
    if chunks.len() <= 1 {
        return vec![frame(0x2, payload)];
    }
    let last = chunks.len() - 1;
    chunks
        .iter()
        .enumerate()
        .map(|(index, chunk)| {
            if index == 0 {
                frame_with_fin(false, 0x2, chunk)
            } else if index == last {
                frame_with_fin(true, 0x0, chunk)
            } else {
                frame_with_fin(false, 0x0, chunk)
            }
        })
        .collect()
}

fn frame_with_fin(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut encoded = frame(opcode, payload);
    if !fin {
        encoded[0] &= 0x7f;
    } else if opcode == 0x0 {
        encoded[0] |= 0x80;
    }
    encoded
}

/// Decode a client (always masked) frame; None when more bytes are needed.
fn decode_client_frame(buffer: &mut Vec<u8>) -> Option<(u8, Vec<u8>)> {
    if buffer.len() < 2 {
        return None;
    }
    let opcode = buffer[0] & 0x0f;
    let masked = buffer[1] & 0x80 != 0;
    let mut length = (buffer[1] & 0x7f) as usize;
    let mut offset = 2usize;
    if length == 126 {
        if buffer.len() < 4 {
            return None;
        }
        length = u16::from_be_bytes([buffer[2], buffer[3]]) as usize;
        offset = 4;
    } else if length == 127 {
        if buffer.len() < 10 {
            return None;
        }
        let mut raw = [0u8; 8];
        raw.copy_from_slice(&buffer[2..10]);
        length = u64::from_be_bytes(raw) as usize;
        offset = 10;
    }
    let mask_len = if masked { 4 } else { 0 };
    let total = offset + mask_len + length;
    if buffer.len() < total {
        return None;
    }
    let mut payload = buffer[offset + mask_len..total].to_vec();
    if masked {
        let mask = [
            buffer[offset],
            buffer[offset + 1],
            buffer[offset + 2],
            buffer[offset + 3],
        ];
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index & 3];
        }
    }
    buffer.drain(..total);
    Some((opcode, payload))
}

// ------------------------------------------------------------------- server

pub const OPCODE_TEXT: u8 = 0x1;
pub const OPCODE_BINARY: u8 = 0x2;
pub const OPCODE_CLOSE: u8 = 0x8;
pub const OPCODE_PING: u8 = 0x9;
pub const OPCODE_PONG: u8 = 0xA;

/// Host -> device datastore message observed by the fake device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostMessage {
    pub id: u16,
    pub index: u16,
    pub value: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EchoMode {
    /// Echo the written value back (device applied it).
    Normal,
    /// Echo a different value (device clamped/rejected).
    Wrong,
    /// Do not echo (device silent).
    Silent,
}

#[derive(Clone, Debug)]
pub struct ServerOptions {
    pub state: Vec<(u16, u16, Vec<u8>)>,
    pub auth: bool,
    pub echo: EchoMode,
    pub ping_first: bool,
    pub bad_accept: bool,
    pub text_before_state: bool,
    pub fragment_state: bool,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            state: default_state(),
            auth: false,
            echo: EchoMode::Normal,
            ping_first: false,
            bad_accept: false,
            text_before_state: false,
            fragment_state: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ServerLog {
    pub sessions: usize,
    pub handshake_ok: bool,
    pub host_messages: Vec<HostMessage>,
    pub auth_cipher_len: Option<usize>,
    pub pong_received: bool,
    pub close_received: bool,
    pub multi_message_frames: usize,
}

pub struct FakeServer {
    pub addr: SocketAddr,
    pub log: Arc<Mutex<ServerLog>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl FakeServer {
    pub fn start(options: ServerOptions) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake server");
        let addr = listener.local_addr().expect("fake server addr");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let log = Arc::new(Mutex::new(ServerLog::default()));
        let log_for_thread = Arc::clone(&log);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !stop_for_thread.load(std::sync::atomic::Ordering::Relaxed)
                && Instant::now() < deadline
            {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(_) => break,
                };
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                let mut first = [0u8; 1];
                match stream.read(&mut first) {
                    Ok(0) => continue,
                    Ok(1) if first[0] == b'G' => {
                        serve_session(&mut stream, first[0], &options, &log_for_thread);
                        break;
                    }
                    Ok(_) => continue,
                    Err(_) => continue,
                }
            }
        });
        Self {
            addr,
            log,
            stop,
            handle: Some(handle),
        }
    }

    pub fn messages(&self) -> Vec<HostMessage> {
        self.log.lock().expect("log lock").host_messages.clone()
    }

    pub fn log_snapshot(&self) -> ServerLog {
        self.log.lock().expect("log lock").clone()
    }

    /// Signal the accept loop, join the server thread and return the final log.
    pub fn shutdown(mut self) -> ServerLog {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        self.log_snapshot()
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn serve_session(
    stream: &mut TcpStream,
    first: u8,
    options: &ServerOptions,
    log: &Arc<Mutex<ServerLog>>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut head = vec![first];
    let deadline = Instant::now() + Duration::from_secs(10);
    while !head.ends_with(b"\r\n\r\n") && Instant::now() < deadline {
        let mut chunk = [0u8; 1024];
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(count) => head.extend_from_slice(&chunk[..count]),
            Err(_) => return,
        }
    }
    let request = String::from_utf8_lossy(&head).to_string();
    let key = request
        .lines()
        .find_map(|line| line.strip_prefix("Sec-WebSocket-Key: "))
        .unwrap_or("")
        .trim()
        .to_string();
    if key.is_empty() {
        return;
    }
    let response_accept = if options.bad_accept {
        "AAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string()
    } else {
        accept_key(&key)
    };
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        response_accept
    );
    if stream.write_all(response.as_bytes()).is_err() {
        return;
    }
    {
        let mut guard = log.lock().expect("log lock");
        guard.sessions += 1;
        guard.handshake_ok = !options.bad_accept;
    }
    let mut buffer = Vec::new();
    if options.ping_first {
        let _ = stream.write_all(&frame(OPCODE_PING, b"hi"));
    }
    if options.auth {
        send_proxy(stream, PROXY_SUB_PUBLIC_KEY, RSA2048_PEM_SPKI.as_bytes());
        send_proxy(stream, PROXY_SUB_PASSWORD_STATUS, &[1]);
        send_proxy(stream, PROXY_SUB_AUTH_REQUEST, b"UL5LFE7E6E");
        // Wait for the client auth response (proxy sub-message 1).
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if Instant::now() >= deadline {
                return;
            }
            match read_ws_frame(stream, &mut buffer, Duration::from_millis(500)) {
                Err(()) => return,
                Ok(None) => continue,
                Ok(Some((OPCODE_CLOSE, _))) => return,
                Ok(Some((OPCODE_PING, payload))) => {
                    let _ = stream.write_all(&frame(OPCODE_PONG, &payload));
                }
                Ok(Some((OPCODE_BINARY, payload))) => {
                    if payload.len() >= 6
                        && u16::from_be_bytes([payload[0], payload[1]]) == PROXY_ID
                        && u16::from_be_bytes([payload[2], payload[3]]) == PROXY_SUB_AUTH_RESPONSE
                    {
                        let len = u16::from_be_bytes([payload[4], payload[5]]) as usize;
                        log.lock().expect("log lock").auth_cipher_len = Some(len);
                        break;
                    }
                }
                Ok(Some(_)) => {}
            }
        }
    }
    if options.text_before_state {
        let _ = stream.write_all(&frame(OPCODE_TEXT, b"ignored-text-frame"));
    }
    if options.fragment_state {
        for (id, index, value) in &options.state {
            let mut payload = id.to_be_bytes().to_vec();
            payload.extend_from_slice(&index.to_be_bytes());
            payload.extend_from_slice(value);
            for piece in fragmented_frames(&payload, 3) {
                if stream.write_all(&piece).is_err() {
                    return;
                }
            }
        }
    } else {
        for (id, index, value) in &options.state {
            let mut payload = id.to_be_bytes().to_vec();
            payload.extend_from_slice(&index.to_be_bytes());
            payload.extend_from_slice(value);
            if stream.write_all(&frame(OPCODE_BINARY, &payload)).is_err() {
                return;
            }
        }
    }
    // Echo loop.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if Instant::now() >= deadline {
            return;
        }
        let (opcode, payload) = match read_ws_frame(stream, &mut buffer, Duration::from_millis(300))
        {
            Err(()) => return,
            Ok(None) => continue,
            Ok(Some(frame)) => frame,
        };
        match opcode {
            OPCODE_CLOSE => {
                log.lock().expect("log lock").close_received = true;
                return;
            }
            OPCODE_PING => {
                let _ = stream.write_all(&frame(OPCODE_PONG, &payload));
            }
            OPCODE_PONG => {
                log.lock().expect("log lock").pong_received = true;
            }
            OPCODE_BINARY => handle_host_message(stream, &payload, options, log),
            _ => {}
        }
    }
}

fn handle_host_message(
    stream: &mut TcpStream,
    payload: &[u8],
    options: &ServerOptions,
    log: &Arc<Mutex<ServerLog>>,
) {
    if payload.len() < 6 {
        return;
    }
    let id = u16::from_be_bytes([payload[0], payload[1]]);
    let index = u16::from_be_bytes([payload[2], payload[3]]);
    let len = u16::from_be_bytes([payload[4], payload[5]]) as usize;
    if payload.len() < 6 + len {
        return;
    }
    let mut value = payload[6..6 + len].to_vec();
    {
        let mut guard = log.lock().expect("log lock");
        if payload.len() != 6 + len {
            guard.multi_message_frames += 1;
        }
        guard.host_messages.push(HostMessage {
            id,
            index,
            value: value.clone(),
        });
    }
    if matches!(id, PROXY_ID | 0xFFFF | 6000) {
        return;
    }
    match options.echo {
        EchoMode::Silent => {}
        EchoMode::Normal => {
            let mut reply = id.to_be_bytes().to_vec();
            reply.extend_from_slice(&index.to_be_bytes());
            reply.extend_from_slice(&value);
            let _ = stream.write_all(&frame(OPCODE_BINARY, &reply));
        }
        EchoMode::Wrong => {
            if let Some(last) = value.last_mut() {
                *last ^= 0xff;
            }
            let mut reply = id.to_be_bytes().to_vec();
            reply.extend_from_slice(&index.to_be_bytes());
            reply.extend_from_slice(&value);
            let _ = stream.write_all(&frame(OPCODE_BINARY, &reply));
        }
    }
}

fn send_proxy(stream: &mut TcpStream, sub_id: u16, payload: &[u8]) {
    let mut message = PROXY_ID.to_be_bytes().to_vec();
    message.extend_from_slice(&sub_id.to_be_bytes());
    message.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    message.extend_from_slice(payload);
    let _ = stream.write_all(&frame(OPCODE_BINARY, &message));
}

/// Ok(Some(frame)) = frame, Ok(None) = read timed out, Err(()) = peer closed.
fn read_ws_frame(
    stream: &mut TcpStream,
    buffer: &mut Vec<u8>,
    timeout: Duration,
) -> Result<Option<(u8, Vec<u8>)>, ()> {
    let _ = stream.set_read_timeout(Some(timeout.max(Duration::from_millis(1))));
    let start = Instant::now();
    loop {
        if let Some(frame) = decode_client_frame(buffer) {
            return Ok(Some(frame));
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Ok(None);
        }
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk) {
            Ok(0) => return Err(()),
            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(None)
            }
            Err(_) => return Err(()),
        }
    }
}

// --------------------------------------------------------------- fixtures

pub fn fader_raw(value: f64) -> Vec<u8> {
    let raw = (value * (1 << 24) as f64).round() as i32;
    raw.to_be_bytes().to_vec()
}

pub fn string32(text: &str) -> Vec<u8> {
    let mut out = [0u8; 32];
    let bytes = text.as_bytes();
    let len = bytes.len().min(31);
    out[..len].copy_from_slice(&bytes[..len]);
    out.to_vec()
}

pub fn byte(value: u8) -> Vec<u8> {
    vec![value]
}

pub fn int(value: i32) -> Vec<u8> {
    value.to_be_bytes().to_vec()
}

pub fn u16_value(value: u16) -> Vec<u8> {
    value.to_be_bytes().to_vec()
}

/// Datastore state used by default: matches the recorded N150 baseline where
/// relevant (mute off, 48 kHz, main faders at unity, main trim 31).
pub fn default_state() -> Vec<(u16, u16, Vec<u8>)> {
    vec![
        (MUTE_ENABLE_ID, 0, byte(0)),
        (SAMPLE_RATE_ID, 0, int(48_000)),
        (KO_BUS_FADER_ID, 0, fader_raw(1.0)),
        (KO_BUS_FADER_ID, 1, fader_raw(1.0)),
        (MAIN_TRIM_ID, 0, byte(31)),
        (KO_BUS_MUTE_ID, 0, byte(0)),
        (MODEL_ID_ID, 0, string32("UL5L")),
        (DEV_NAME_ID, 0, string32("UltraLite-mk5")),
        (DEV_UID_ID, 0, vec![85, 76, 53, 76, 70, 69, 55, 69, 54, 69]),
        (API_VERSION_ID, 0, u16_value(10)),
    ]
}

/// N150 /proc/asound/cards text used by CLI info tests.
pub const N150_CARDS: &str = " 0 [UltraLitemk5   ]: USB-Audio - UltraLite-mk5\n                      MOTU UltraLite-mk5 at usb-0000:00:14.0-2.1, high speed\n 1 [U18dB          ]: USB-Audio - Umik-1  Gain: 18dB\n 3 [PCH            ]: HDA-Intel - HDA Intel PCH\n";

pub fn unique_temp_path(prefix: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("{}-{}-{}.tmp", prefix, std::process::id(), nanos))
}
