//! Minimal RFC 6455 WebSocket client: handshake helpers and frame codec.
//!
//! Only what the CueMix control channel needs: client handshake with
//! Sec-WebSocket-Accept validation, masked binary frames, ping/pong, close and
//! fragmented message reassembly. No extensions, no subprotocols.

use crate::crypto::{base64_encode, websocket_accept_key};
use crate::error::CueMixError;
use crate::rng::fill_random;

/// Largest accepted single frame payload (1 MiB).
pub const MAX_FRAME_PAYLOAD: usize = 1 << 20;
/// Largest accepted reassembled message payload (4 MiB).
pub const MAX_MESSAGE_PAYLOAD: usize = 4 << 20;
/// Largest accepted HTTP handshake response head (64 KiB).
pub const MAX_HANDSHAKE_BYTES: usize = 64 * 1024;

/// WebSocket frame opcodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opcode {
    /// Continuation of a fragmented message (0x0).
    Continuation,
    /// Text message (0x1).
    Text,
    /// Binary message (0x2).
    Binary,
    /// Connection close (0x8).
    Close,
    /// Ping (0x9).
    Ping,
    /// Pong (0xA).
    Pong,
}

impl Opcode {
    /// Decode the 4-bit opcode; None for reserved values.
    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0x0 => Some(Self::Continuation),
            0x1 => Some(Self::Text),
            0x2 => Some(Self::Binary),
            0x8 => Some(Self::Close),
            0x9 => Some(Self::Ping),
            0xa => Some(Self::Pong),
            _ => None,
        }
    }

    /// Wire value of the opcode.
    pub const fn to_byte(self) -> u8 {
        match self {
            Self::Continuation => 0x0,
            Self::Text => 0x1,
            Self::Binary => 0x2,
            Self::Close => 0x8,
            Self::Ping => 0x9,
            Self::Pong => 0xa,
        }
    }

    /// True for control opcodes (Close, Ping, Pong).
    pub const fn is_control(self) -> bool {
        matches!(self, Self::Close | Self::Ping | Self::Pong)
    }
}

/// One raw WebSocket frame as decoded from the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawFrame {
    /// FIN bit (true = last fragment).
    pub fin: bool,
    /// Frame opcode.
    pub opcode: Opcode,
    /// Unmasked payload.
    pub payload: Vec<u8>,
}

fn apply_mask(payload: &mut [u8], mask: [u8; 4]) {
    for (i, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[i & 3];
    }
}

/// Encode one frame. A mask key is required for client -> server frames.
pub fn encode_frame(fin: bool, opcode: Opcode, payload: &[u8], mask: Option<[u8; 4]>) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    let mut first = if fin { 0x80 } else { 0 };
    first |= opcode.to_byte();
    out.push(first);
    let mask_bit = if mask.is_some() { 0x80 } else { 0 };
    match payload.len() {
        len @ 0..=125 => out.push(mask_bit | len as u8),
        len @ 126..=65535 => {
            out.push(mask_bit | 126);
            out.extend_from_slice(&(len as u16).to_be_bytes());
        }
        len => {
            out.push(mask_bit | 127);
            out.extend_from_slice(&(len as u64).to_be_bytes());
        }
    }
    match mask {
        Some(key) => {
            out.extend_from_slice(&key);
            let start = out.len();
            out.extend_from_slice(payload);
            apply_mask(&mut out[start..], key);
        }
        None => out.extend_from_slice(payload),
    }
    out
}

/// Encode a masked binary frame (client role).
pub fn encode_binary(payload: &[u8]) -> Vec<u8> {
    let mut mask = [0u8; 4];
    fill_random(&mut mask);
    encode_frame(true, Opcode::Binary, payload, Some(mask))
}

/// Encode a masked pong frame answering a ping payload.
pub fn encode_pong(payload: &[u8]) -> Vec<u8> {
    let mut mask = [0u8; 4];
    fill_random(&mut mask);
    encode_frame(true, Opcode::Pong, payload, Some(mask))
}

/// Encode a masked close frame.
pub fn encode_close(code: Option<u16>, reason: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(2 + reason.len().min(123));
    if let Some(code) = code {
        payload.extend_from_slice(&code.to_be_bytes());
        let max_reason = 123.min(reason.len());
        payload.extend_from_slice(&reason.as_bytes()[..max_reason]);
    }
    let mut mask = [0u8; 4];
    fill_random(&mut mask);
    encode_frame(true, Opcode::Close, &payload, Some(mask))
}

/// Incremental decoder for raw frames (accepts masked or unmasked frames).
#[derive(Debug)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
    max_frame: usize,
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameDecoder {
    /// New decoder with the default frame size guard.
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            max_frame: MAX_FRAME_PAYLOAD,
        }
    }

    /// Append bytes received from the socket.
    pub fn push(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    /// Bytes buffered but not yet decoded (test helper).
    #[cfg(test)]
    pub fn buffered_len(&self) -> usize {
        self.buffer.len()
    }

    /// Decode the next complete frame, or None when more bytes are needed.
    pub fn next_frame(&mut self) -> Result<Option<RawFrame>, CueMixError> {
        if self.buffer.len() < 2 {
            return Ok(None);
        }
        let first = self.buffer[0];
        let second = self.buffer[1];
        let fin = first & 0x80 != 0;
        if first & 0x70 != 0 {
            return Err(protocol_error("RSV bits are not supported"));
        }
        let opcode =
            Opcode::from_byte(first & 0x0f).ok_or_else(|| protocol_error("reserved opcode"))?;
        let masked = second & 0x80 != 0;
        let mut length = (second & 0x7f) as usize;
        let mut offset = 2usize;
        if length == 126 {
            if self.buffer.len() < 4 {
                return Ok(None);
            }
            length = u16::from_be_bytes([self.buffer[2], self.buffer[3]]) as usize;
            offset = 4;
        } else if length == 127 {
            if self.buffer.len() < 10 {
                return Ok(None);
            }
            let mut raw = [0u8; 8];
            raw.copy_from_slice(&self.buffer[2..10]);
            let wide = u64::from_be_bytes(raw);
            if wide > usize::MAX as u64 {
                return Err(protocol_error("frame length does not fit usize"));
            }
            length = wide as usize;
            offset = 10;
        }
        if length > self.max_frame {
            return Err(protocol_error("frame payload exceeds limit"));
        }
        if opcode.is_control() {
            if !fin {
                return Err(protocol_error("fragmented control frame"));
            }
            if length > 125 {
                return Err(protocol_error("control frame payload exceeds 125 bytes"));
            }
        }
        let mask_len = if masked { 4 } else { 0 };
        let total = offset + mask_len + length;
        if self.buffer.len() < total {
            return Ok(None);
        }
        let mut payload = self.buffer[offset + mask_len..total].to_vec();
        if masked {
            let mut key = [0u8; 4];
            key.copy_from_slice(&self.buffer[offset..offset + 4]);
            apply_mask(&mut payload, key);
        }
        self.buffer.drain(..total);
        Ok(Some(RawFrame {
            fin,
            opcode,
            payload,
        }))
    }
}

/// A complete data message (text or binary) reassembled from fragments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// Reassembled text message.
    Text(Vec<u8>),
    /// Reassembled binary message.
    Binary(Vec<u8>),
}

/// Reassembles fragmented data messages; control frames must be handled before
/// being fed here.
#[derive(Debug, Default)]
pub struct MessageAssembler {
    pending: Option<(Opcode, Vec<u8>)>,
}

impl MessageAssembler {
    /// New empty assembler.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one data frame (Binary/Text/Continuation).
    pub fn push(&mut self, frame: RawFrame) -> Result<Option<Message>, CueMixError> {
        match frame.opcode {
            Opcode::Binary | Opcode::Text => {
                if self.pending.is_some() {
                    return Err(protocol_error(
                        "new data frame before fragmented message completed",
                    ));
                }
                if frame.fin {
                    Ok(Some(finish_message(frame.opcode, frame.payload)))
                } else {
                    self.pending = Some((frame.opcode, frame.payload));
                    Ok(None)
                }
            }
            Opcode::Continuation => match self.pending.take() {
                Some((opcode, mut buffer)) => {
                    buffer.extend_from_slice(&frame.payload);
                    if buffer.len() > MAX_MESSAGE_PAYLOAD {
                        return Err(protocol_error("reassembled message exceeds limit"));
                    }
                    if frame.fin {
                        Ok(Some(finish_message(opcode, buffer)))
                    } else {
                        self.pending = Some((opcode, buffer));
                        Ok(None)
                    }
                }
                None => Err(protocol_error("continuation without initial frame")),
            },
            Opcode::Close | Opcode::Ping | Opcode::Pong => {
                Err(protocol_error("control frame passed to message assembler"))
            }
        }
    }
}

fn finish_message(opcode: Opcode, payload: Vec<u8>) -> Message {
    match opcode {
        Opcode::Text => Message::Text(payload),
        _ => Message::Binary(payload),
    }
}

fn protocol_error(reason: &str) -> CueMixError {
    CueMixError::Protocol {
        reason: reason.to_string(),
    }
}

// --------------------------------------------------------------- handshake

/// Client handshake request: raw HTTP text plus the generated nonce key.
#[derive(Clone, Debug)]
pub struct HandshakeRequest {
    /// HTTP request bytes (UTF-8).
    pub request: String,
    /// Base64 Sec-WebSocket-Key sent in the request.
    pub key: String,
}

/// Build a WebSocket client handshake request.
pub fn build_handshake_request(
    host_header: &str,
    path: &str,
    user_agent: &str,
) -> HandshakeRequest {
    let mut nonce = [0u8; 16];
    fill_random(&mut nonce);
    let key = base64_encode(&nonce);
    let path = if path.is_empty() { "/" } else { path };
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {}\r\nSec-WebSocket-Version: 13\r\nUser-Agent: {}\r\n\r\n",
        path, host_header, key, user_agent
    );
    HandshakeRequest { request, key }
}

/// Parsed HTTP response head of the upgrade request.
#[derive(Clone, Debug)]
pub struct HandshakeResponse {
    /// HTTP status code.
    pub status_code: u16,
    /// Header name/value pairs (names lowercased, values trimmed).
    pub headers: Vec<(String, String)>,
}

impl HandshakeResponse {
    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        let lowered = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == lowered)
            .map(|(_, value)| value.as_str())
    }
}

/// Parse an HTTP response head (without the trailing empty line).
pub fn parse_handshake_head(head: &[u8]) -> Result<HandshakeResponse, CueMixError> {
    let text =
        std::str::from_utf8(head).map_err(|_| protocol_error("handshake response is not UTF-8"))?;
    let mut lines = text.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| protocol_error("empty handshake response"))?;
    let mut parts = status_line.split_whitespace();
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/1.") {
        return Err(protocol_error("handshake response is not HTTP/1.x"));
    }
    let status_code = parts
        .next()
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| protocol_error("missing handshake status code"))?;
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(protocol_error("malformed handshake header"));
        };
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    Ok(HandshakeResponse {
        status_code,
        headers,
    })
}

fn header_contains_token(value: &str, token: &str) -> bool {
    value
        .split(',')
        .any(|part| part.trim().eq_ignore_ascii_case(token))
}

/// Validate the upgrade response against the key we sent.
pub fn validate_handshake(response: &HandshakeResponse, key: &str) -> Result<(), CueMixError> {
    if response.status_code != 101 {
        return Err(CueMixError::Handshake {
            reason: format!("upgrade refused with HTTP {}", response.status_code),
        });
    }
    match response.header("upgrade") {
        Some(value) if header_contains_token(value, "websocket") => {}
        _ => {
            return Err(CueMixError::Handshake {
                reason: "missing Upgrade: websocket header".to_string(),
            })
        }
    }
    match response.header("connection") {
        Some(value) if header_contains_token(value, "upgrade") => {}
        _ => {
            return Err(CueMixError::Handshake {
                reason: "missing Connection: Upgrade header".to_string(),
            })
        }
    }
    let expected = websocket_accept_key(key);
    match response.header("sec-websocket-accept") {
        Some(value) if value == expected => Ok(()),
        Some(value) => Err(CueMixError::Handshake {
            reason: format!("bad Sec-WebSocket-Accept {:?}", value),
        }),
        None => Err(CueMixError::Handshake {
            reason: "missing Sec-WebSocket-Accept header".to_string(),
        }),
    }
}

/// Locate the end of the HTTP head in a byte buffer ("\r\n\r\n").
pub fn find_handshake_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unmask_frame(frame: &[u8]) -> Vec<u8> {
        // Re-encode a received masked frame without the mask bit, as a server would send it.
        let mut decoder = FrameDecoder::new();
        decoder.push(frame);
        let raw = decoder.next_frame().expect("decode").expect("frame");
        encode_frame(raw.fin, raw.opcode, &raw.payload, None)
    }

    #[test]
    fn encodes_small_masked_binary_frame() {
        let frame = encode_frame(true, Opcode::Binary, b"abc", Some([1, 2, 3, 4]));
        assert_eq!(frame[0], 0x82);
        assert_eq!(frame[1], 0x80 | 3);
        assert_eq!(&frame[2..6], &[1, 2, 3, 4]);
        assert_eq!(&frame[6..], &[b'a' ^ 1, b'b' ^ 2, b'c' ^ 3]);
    }

    #[test]
    fn length_boundaries_use_extended_headers() {
        for (length, header) in [
            (0usize, 2usize),
            (125, 2),
            (126, 4),
            (65535, 4),
            (65536, 10),
        ] {
            let payload = vec![0x5au8; length];
            let frame = encode_frame(true, Opcode::Binary, &payload, None);
            assert_eq!(frame.len(), header + length, "length {}", length);
            let mut decoder = FrameDecoder::new();
            decoder.push(&frame);
            let raw = decoder.next_frame().expect("decode").expect("frame");
            assert_eq!(raw.payload.len(), length, "length {}", length);
            assert_eq!(raw.payload, payload);
        }
    }

    #[test]
    fn masked_roundtrip_and_unmasked_roundtrip() {
        let payload: Vec<u8> = (0..=255u8).collect();
        for mask in [None, Some([0xde, 0xad, 0xbe, 0xef])] {
            let frame = encode_frame(true, Opcode::Binary, &payload, mask);
            let mut decoder = FrameDecoder::new();
            decoder.push(&frame);
            let raw = decoder.next_frame().expect("decode").expect("frame");
            assert_eq!(raw.payload, payload);
            assert!(raw.fin);
            assert_eq!(raw.opcode, Opcode::Binary);
        }
    }

    #[test]
    fn decoder_waits_for_complete_frames() {
        let frame = encode_frame(true, Opcode::Binary, b"hello", Some([9, 8, 7, 6]));
        let mut decoder = FrameDecoder::new();
        for split in 0..frame.len() - 1 {
            decoder.push(&frame[split..split + 1]);
            assert!(
                decoder.next_frame().expect("partial").is_none(),
                "after {} bytes",
                split + 1
            );
        }
        decoder.push(&frame[frame.len() - 1..]);
        let raw = decoder.next_frame().expect("decode").expect("frame");
        assert_eq!(raw.payload, b"hello");
        assert_eq!(decoder.buffered_len(), 0);
    }

    #[test]
    fn decodes_ping_pong_and_close_frames() {
        let mut decoder = FrameDecoder::new();
        decoder.push(&encode_frame(
            true,
            Opcode::Ping,
            b"ping",
            Some([1, 1, 1, 1]),
        ));
        decoder.push(&encode_frame(true, Opcode::Pong, b"", None));
        decoder.push(&encode_frame(
            true,
            Opcode::Close,
            &[0x03, 0xe8],
            Some([2, 2, 2, 2]),
        ));
        assert_eq!(
            decoder.next_frame().expect("ping").expect("frame").opcode,
            Opcode::Ping
        );
        assert_eq!(
            decoder.next_frame().expect("pong").expect("frame").opcode,
            Opcode::Pong
        );
        let close = decoder.next_frame().expect("close").expect("frame");
        assert_eq!(close.opcode, Opcode::Close);
        assert_eq!(close.payload, vec![0x03, 0xe8]);
    }

    #[test]
    fn rejects_bad_frames() {
        let mut decoder = FrameDecoder::new();
        decoder.push(&[0x82 | 0x40, 0x00]);
        assert!(decoder.next_frame().is_err(), "RSV bits");

        let mut decoder = FrameDecoder::new();
        decoder.push(&[0x83, 0x00]);
        assert!(decoder.next_frame().is_err(), "reserved opcode");

        let mut decoder = FrameDecoder::new();
        let mut oversized = vec![0x82, 0x80 | 127];
        oversized.extend_from_slice(&(MAX_FRAME_PAYLOAD as u64 + 1).to_be_bytes());
        decoder.push(&oversized);
        assert!(decoder.next_frame().is_err(), "oversized frame");

        let mut decoder = FrameDecoder::new();
        decoder.push(&encode_frame(false, Opcode::Ping, b"x", Some([1, 2, 3, 4])));
        assert!(decoder.next_frame().is_err(), "fragmented control frame");
    }

    #[test]
    fn assembles_fragmented_binary_message() {
        let mut assembler = MessageAssembler::new();
        let mut decoder = FrameDecoder::new();
        decoder.push(&encode_frame(
            false,
            Opcode::Binary,
            b"frag",
            Some([1, 2, 3, 4]),
        ));
        decoder.push(&encode_frame(
            true,
            Opcode::Continuation,
            b"ment",
            Some([4, 3, 2, 1]),
        ));
        let first = assembler
            .push(decoder.next_frame().expect("f1").expect("frame"))
            .expect("push f1");
        assert!(first.is_none());
        let message = assembler
            .push(decoder.next_frame().expect("f2").expect("frame"))
            .expect("push f2");
        assert_eq!(message, Some(Message::Binary(b"fragment".to_vec())));
    }

    #[test]
    fn assembler_rejects_bad_fragment_sequences() {
        let mut assembler = MessageAssembler::new();
        let stray = RawFrame {
            fin: true,
            opcode: Opcode::Continuation,
            payload: b"x".to_vec(),
        };
        assert!(assembler.push(stray).is_err());
        let start = RawFrame {
            fin: false,
            opcode: Opcode::Binary,
            payload: b"a".to_vec(),
        };
        assert_eq!(assembler.push(start).expect("start"), None);
        let restart = RawFrame {
            fin: true,
            opcode: Opcode::Binary,
            payload: b"b".to_vec(),
        };
        assert!(assembler.push(restart).is_err());
    }

    #[test]
    fn builds_and_validates_handshake() {
        let request = build_handshake_request("169.254.111.126:1280", "/", "khaen-motu/test");
        assert!(request.request.starts_with("GET / HTTP/1.1\r\n"));
        assert!(request.request.contains("Sec-WebSocket-Key: "));
        assert!(request.request.contains("Upgrade: websocket\r\n"));
        assert!(request.request.contains("Sec-WebSocket-Version: 13\r\n"));
        assert!(request.request.ends_with("\r\n\r\n"));

        let accept = websocket_accept_key(&request.key);
        let head = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n",
            accept
        );
        let response = parse_handshake_head(head.as_bytes()).expect("parse");
        assert_eq!(response.status_code, 101);
        validate_handshake(&response, &request.key).expect("validate");
    }

    #[test]
    fn rejects_bad_handshakes() {
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let refused = parse_handshake_head(b"HTTP/1.1 400 Bad Request\r\n").expect("parse");
        assert!(validate_handshake(&refused, key).is_err());

        let no_accept = parse_handshake_head(
            b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n",
        )
        .expect("parse");
        assert!(validate_handshake(&no_accept, key).is_err());

        let bad_accept = parse_handshake_head(
            b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: nope\r\n",
        )
        .expect("parse");
        assert!(validate_handshake(&bad_accept, key).is_err());

        assert!(parse_handshake_head(b"garbage").is_err());
    }

    #[test]
    fn finds_handshake_head_end() {
        assert_eq!(
            find_handshake_end(b"HTTP/1.1 101\r\nA: b\r\n\r\nrest"),
            Some(18)
        );
        assert_eq!(find_handshake_end(b"HTTP/1.1 101\r\n"), None);
    }

    #[test]
    fn encoded_server_frame_helper_roundtrips_mask() {
        let masked = encode_frame(true, Opcode::Binary, b"payload", Some([7, 7, 7, 7]));
        let unmasked = unmask_frame(&masked);
        assert_eq!(unmasked[1], b"payload".len() as u8);
        let mut decoder = FrameDecoder::new();
        decoder.push(&unmasked);
        assert_eq!(
            decoder
                .next_frame()
                .expect("decode")
                .expect("frame")
                .payload,
            b"payload".to_vec()
        );
    }
}
