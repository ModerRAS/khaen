//! 契约 B 的行协议编解码（平台无关纯函数）。
//!
//! 与 `crates/rt-host/src/control/transport.rs` 是同一份契约（也是 host `control::serve` 的
//! 语义）：UTF-8、无 BOM、一行 JSON 请求 / 一行 JSON 响应，行以 `\n` 结束；请求行上限
//! [`MAX_REQUEST_BYTES`]（与 `khaen_host::control` 同源）；单行坏请求由服务端回
//! `bad_request`，客户端把响应原样透传（不吞坏响应、不编造成功）。
//!
//! 这一层只碰字节与 JSON、不碰 socket，所以 Windows 上也能完整单测；`cfg(unix)` 的
//! Unix socket 传输与测试用的 TCP 回环传输共用同一份编解码（[`exchange`]）。

use std::fmt;
use std::io::{self, BufRead, Write};

use serde_json::Value;

/// 请求行字节上限（与 host/rt-host 的协议常量同源，不另编一个数字）。
pub use khaen_host::control::MAX_REQUEST_BYTES;

/// 响应行字节上限：status 快照含嵌套统计（实际约 2–4 KiB），1 MiB 只是兜底；
/// 超限按协议错误处理，不为坏服务端无限分配内存。
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// 编解码错误（传输层错误见 [`crate::host_link::TransportError`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// 请求不是 JSON 对象，或缺 `op`，或 `op` 不是非空字符串。
    InvalidRequest(String),
    /// 请求行超过 [`MAX_REQUEST_BYTES`]。
    RequestTooLarge {
        /// 实际字节数。
        bytes: usize,
        /// 上限。
        limit: usize,
    },
    /// 响应行超过 [`MAX_RESPONSE_BYTES`]。
    ResponseTooLarge {
        /// 实际（至少）字节数。
        bytes: usize,
        /// 上限。
        limit: usize,
    },
    /// 响应不是合法 UTF-8。
    InvalidUtf8(String),
    /// 响应不是合法 JSON 对象，或缺少契约字段。
    InvalidResponse(String),
    /// `ok=false` 但缺少字符串字段 `error`。
    InvalidError(String),
    /// 对端在完整响应行之前关闭了连接。
    UnexpectedEof,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(message) => write!(f, "invalid control request: {message}"),
            Self::RequestTooLarge { bytes, limit } => {
                write!(
                    f,
                    "control request is {bytes} bytes, over the {limit}-byte line limit"
                )
            }
            Self::ResponseTooLarge { bytes, limit } => {
                write!(
                    f,
                    "control response is at least {bytes} bytes, over the {limit}-byte line limit"
                )
            }
            Self::InvalidUtf8(message) => {
                write!(f, "control response is not valid UTF-8: {message}")
            }
            Self::InvalidResponse(message) => write!(f, "invalid control response: {message}"),
            Self::InvalidError(message) => write!(f, "invalid control error response: {message}"),
            Self::UnexpectedEof => write!(
                f,
                "rt-host closed the control connection before sending a complete response line"
            ),
        }
    }
}

impl std::error::Error for ProtocolError {}

/// 服务端回复的分类：成功响应原样透传，业务错误带稳定错误码 + 原响应体。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireReply {
    /// `{"ok":true,...}`：REST 原样透传（不增删字段）。
    Success(Value),
    /// `{"ok":false,"error":<code>,"message":<text>}`：错误码透传，REST 按既有码表映射 HTTP 状态。
    Failure {
        /// 稳定错误码（契约 B.3，例如 `engine_token_invalid`）。
        code: String,
        /// 人读说明。
        message: String,
        /// 完整响应体（REST 原样返回）。
        body: Value,
    },
}

impl WireReply {
    /// 取出要返回给 REST 客户端的 JSON 体（成功/失败同一份形状）。
    pub fn into_body(self) -> Value {
        match self {
            Self::Success(value) => value,
            Self::Failure { body, .. } => body,
        }
    }

    /// 是否是服务端业务错误。
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failure { .. })
    }
}

/// 把一条请求编码成**单行** JSON（不含结尾换行；serde_json 的紧凑输出不会产生裸换行，
/// 字符串里的换行必然被转义，因此“一行”是编码保证而不是调用方约定）。
///
/// 校验：顶层必须是对象、`op` 必须是非空字符串、整行不超过 [`MAX_REQUEST_BYTES`]。
pub fn encode_request(request: &Value) -> Result<Vec<u8>, ProtocolError> {
    let Some(object) = request.as_object() else {
        return Err(ProtocolError::InvalidRequest(
            "request must be a JSON object".to_string(),
        ));
    };
    match object.get("op") {
        Some(Value::String(op)) if !op.trim().is_empty() => {}
        Some(Value::String(_)) => {
            return Err(ProtocolError::InvalidRequest(
                "request field `op` must not be empty".to_string(),
            ))
        }
        Some(_) => {
            return Err(ProtocolError::InvalidRequest(
                "request field `op` must be a string".to_string(),
            ))
        }
        None => {
            return Err(ProtocolError::InvalidRequest(
                "request is missing the string field `op`".to_string(),
            ))
        }
    }
    let bytes = serde_json::to_vec(request).map_err(|error| {
        ProtocolError::InvalidRequest(format!("request cannot be encoded: {error}"))
    })?;
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(ProtocolError::RequestTooLarge {
            bytes: bytes.len(),
            limit: MAX_REQUEST_BYTES,
        });
    }
    Ok(bytes)
}

/// 解析一行响应的 JSON 体（允许结尾 `\r\n`，但不接受中间换行/前后垃圾）。
pub fn parse_response(line: &[u8]) -> Result<Value, ProtocolError> {
    let text =
        std::str::from_utf8(line).map_err(|error| ProtocolError::InvalidUtf8(error.to_string()))?;
    let text = text.trim_end_matches(['\r', '\n']);
    if text.trim().is_empty() {
        return Err(ProtocolError::InvalidResponse(
            "empty response line".to_string(),
        ));
    }
    let value: Value = serde_json::from_str(text)
        .map_err(|error| ProtocolError::InvalidResponse(format!("not valid JSON: {error}")))?;
    if !value.is_object() {
        return Err(ProtocolError::InvalidResponse(
            "response must be a JSON object".to_string(),
        ));
    }
    Ok(value)
}

/// 把响应体分类成成功/业务错误（`ok` 字段是契约的唯一判据，不做“猜成功”）。
pub fn classify_response(response: Value) -> Result<WireReply, ProtocolError> {
    let object = response.as_object().ok_or_else(|| {
        ProtocolError::InvalidResponse("response must be a JSON object".to_string())
    })?;
    match object.get("ok") {
        Some(Value::Bool(true)) => Ok(WireReply::Success(response)),
        Some(Value::Bool(false)) => {
            let code = object
                .get("error")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ProtocolError::InvalidError(
                        "ok=false response is missing the string field `error`".to_string(),
                    )
                })?
                .to_string();
            let message = object
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Ok(WireReply::Failure {
                code,
                message,
                body: response,
            })
        }
        Some(_) => Err(ProtocolError::InvalidResponse(
            "response field `ok` must be a boolean".to_string(),
        )),
        None => Err(ProtocolError::InvalidResponse(
            "response is missing the boolean field `ok`".to_string(),
        )),
    }
}

/// 解码一行响应（解析 + 分类）。
pub fn decode_response(line: &[u8]) -> Result<WireReply, ProtocolError> {
    classify_response(parse_response(line)?)
}

/// 一行读取的结果（语义与 rt-host `control::transport` 相同）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineRead {
    /// 对端关闭（没有更多字节）。
    Eof,
    /// 读到完整一行（不含换行符）。
    Line,
    /// 超长行：已丢弃到下一个换行（保持行边界同步）。
    TooLong,
}

/// 读一行（不含结尾换行），超过 [`MAX_RESPONSE_BYTES`] 时丢弃剩余部分并返回 [`LineRead::TooLong`]。
///
/// 用 fill_buf/consume 手动组行：阻塞式读取下与 read_until 等价，但能限制单行内存，
/// 并在超限后把连接重新同步到下一个换行（单行坏响应不无限吃内存）。
pub fn read_line_limited<R: BufRead>(reader: &mut R, line: &mut Vec<u8>) -> io::Result<LineRead> {
    line.clear();
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return Ok(if line.is_empty() {
                LineRead::Eof
            } else {
                LineRead::Line
            });
        }
        if let Some(position) = chunk.iter().position(|byte| *byte == b'\n') {
            line.extend_from_slice(&chunk[..position]);
            reader.consume(position + 1);
            return Ok(if line.len() > MAX_RESPONSE_BYTES {
                LineRead::TooLong
            } else {
                LineRead::Line
            });
        }
        line.extend_from_slice(chunk);
        let consumed = chunk.len();
        reader.consume(consumed);
        if line.len() > MAX_RESPONSE_BYTES {
            // 超限：丢弃到下一个换行（不累积），下次读仍从行边界开始。
            loop {
                let chunk = reader.fill_buf()?;
                if chunk.is_empty() {
                    return Ok(LineRead::TooLong);
                }
                if let Some(position) = chunk.iter().position(|byte| *byte == b'\n') {
                    reader.consume(position + 1);
                    return Ok(LineRead::TooLong);
                }
                let consumed = chunk.len();
                reader.consume(consumed);
            }
        }
    }
}

/// 一次请求/响应的传输错误：区分 IO 与协议（IO 再区分“请求肯定没送达”与“不确定”）。
#[derive(Debug)]
pub enum ExchangeError {
    /// 写请求或读响应时的 IO 错误（超时也从这里出来，由传输层按 `ErrorKind` 分类）。
    Io(io::Error),
    /// 请求编码 / 响应解码错误。
    Protocol(ProtocolError),
}

impl fmt::Display for ExchangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Protocol(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for ExchangeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Protocol(error) => Some(error),
        }
    }
}

/// 在任意 `BufRead` + `Write` 上做一次完整交换：写一行请求 → 读一行响应 → 分类。
///
/// 生产实现是 Unix socket（`cfg(unix)`）；测试可以塞内存缓冲或 TCP 回环，二者走同一份代码，
/// 所以行协议/超长行/坏 JSON 的行为在 Windows 上也能测。
pub fn exchange<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    request: &Value,
) -> Result<WireReply, ExchangeError> {
    let payload = encode_request(request).map_err(ExchangeError::Protocol)?;
    writer.write_all(&payload).map_err(ExchangeError::Io)?;
    writer.write_all(b"\n").map_err(ExchangeError::Io)?;
    writer.flush().map_err(ExchangeError::Io)?;
    let mut line = Vec::new();
    match read_line_limited(reader, &mut line).map_err(ExchangeError::Io)? {
        LineRead::Eof => Err(ExchangeError::Protocol(ProtocolError::UnexpectedEof)),
        LineRead::TooLong => Err(ExchangeError::Protocol(ProtocolError::ResponseTooLarge {
            bytes: line.len(),
            limit: MAX_RESPONSE_BYTES,
        })),
        LineRead::Line => decode_response(&line).map_err(ExchangeError::Protocol),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Cursor};

    use serde_json::json;

    use super::*;

    /// 每种 op 的请求（REST handler 产出的形状；与 host/rt-host 的 dispatch 字段一致）。
    fn requests_by_op() -> Vec<(&'static str, Value)> {
        vec![
            ("status", json!({ "op": "status" })),
            ("mute", json!({ "op": "mute", "mute": true })),
            (
                "prepare",
                json!({ "op": "prepare", "plan_path": "/tmp/plan.bin", "plan_sha256": "aabbccdd" }),
            ),
            (
                "commit",
                json!({ "op": "commit", "engine_token": "0123456789abcdef" }),
            ),
            (
                "abort",
                json!({ "op": "abort", "engine_token": "0123456789abcdef" }),
            ),
            ("coeff", json!({ "op": "coeff" })),
            (
                "inspect",
                json!({ "op": "inspect", "plan_path": "/tmp/plan.bin" }),
            ),
            (
                "surface",
                json!({ "op": "surface", "surface": "volume", "data": { "gains_db": [] } }),
            ),
            ("rollback", json!({ "op": "rollback", "target": 3 })),
            ("format", json!({ "op": "format", "sample_rate": 48000 })),
        ]
    }

    #[test]
    fn encode_request_covers_every_op_as_a_single_line() {
        for (op, request) in requests_by_op() {
            let bytes = encode_request(&request).unwrap_or_else(|error| panic!("{op}: {error}"));
            assert!(!bytes.is_empty(), "{op}");
            assert!(
                !bytes.contains(&b'\n') && !bytes.contains(&b'\r'),
                "{op} 编码必须是单行: {bytes:?}"
            );
            // 字节可逆：解析回来必须是同一个 JSON（字段顺序不参与语义）。
            let decoded: Value = serde_json::from_slice(&bytes).expect("roundtrip json");
            assert_eq!(decoded, request, "{op}");
        }
    }

    #[test]
    fn encode_request_is_compact_and_keeps_single_key_shape() {
        // 单键对象的字符串形式是确定的（serde_json 紧凑输出，无空格）。
        let encoded = encode_request(&json!({ "op": "status" })).expect("encode");
        assert_eq!(
            String::from_utf8(encoded).expect("utf8"),
            r#"{"op":"status"}"#
        );
        // 多键对象：解析回原值（不锁字段顺序，serde_json 默认按 BTreeMap 排序）。
        let encoded = encode_request(&json!({ "op": "mute", "mute": false })).expect("encode");
        let decoded: Value = serde_json::from_slice(&encoded).expect("json");
        assert_eq!(decoded["op"], json!("mute"));
        assert_eq!(decoded["mute"], json!(false));
    }

    #[test]
    fn encode_request_rejects_malformed_requests() {
        for request in [json!(null), json!([]), json!("op"), json!(3)] {
            let error = encode_request(&request).expect_err("non-object");
            assert!(
                matches!(error, ProtocolError::InvalidRequest(_)),
                "{request}: {error}"
            );
        }
        for request in [
            json!({}),
            json!({ "op": null }),
            json!({ "op": 7 }),
            json!({ "op": ["status"] }),
            json!({ "op": "" }),
            json!({ "op": "  " }),
        ] {
            let error = encode_request(&request).expect_err("bad op");
            assert!(
                matches!(error, ProtocolError::InvalidRequest(_)),
                "{request}: {error}"
            );
            assert!(
                error.to_string().contains("op"),
                "错误应点名 op 字段: {error}"
            );
        }
    }

    #[test]
    fn encode_request_enforces_the_request_line_limit() {
        let oversized = "x".repeat(MAX_REQUEST_BYTES);
        let error = encode_request(&json!({ "op": "prepare", "plan_path": oversized }))
            .expect_err("too large");
        match error {
            ProtocolError::RequestTooLarge { bytes, limit } => {
                assert!(bytes > MAX_REQUEST_BYTES, "bytes={bytes}");
                assert_eq!(limit, MAX_REQUEST_BYTES);
            }
            other => panic!("expected RequestTooLarge: {other}"),
        }
    }

    #[test]
    fn decode_response_classifies_success_and_server_errors() {
        let success =
            decode_response(br#"{"ok":true,"mute":false,"applied_block":7}"#).expect("ok");
        match success {
            WireReply::Success(body) => {
                assert_eq!(body["ok"], json!(true));
                assert_eq!(body["applied_block"], json!(7));
            }
            other => panic!("expected success: {other:?}"),
        }

        let failure = decode_response(
            br#"{"ok":false,"error":"engine_token_invalid","message":"no prepared plan draft"}"#,
        )
        .expect("failure");
        match failure {
            WireReply::Failure {
                code,
                message,
                body,
            } => {
                assert_eq!(code, "engine_token_invalid");
                assert_eq!(message, "no prepared plan draft");
                // 错误体原样保留：REST 层直接透传，不改写。
                assert_eq!(body["ok"], json!(false));
                assert_eq!(body["error"], json!("engine_token_invalid"));
            }
            other => panic!("expected failure: {other:?}"),
        }
        let failure = WireReply::Failure {
            code: "x".into(),
            message: String::new(),
            body: json!({ "ok": false }),
        };
        assert!(failure.is_failure());
        assert!(!decode_response(br#"{"ok":true}"#).expect("ok").is_failure());
    }

    #[test]
    fn decode_response_tolerates_crlf_and_reports_missing_message() {
        let reply =
            decode_response(b"{\"ok\":false,\"error\":\"no_history\"}\r\n").expect("decode");
        match reply {
            WireReply::Failure { code, message, .. } => {
                assert_eq!(code, "no_history");
                assert_eq!(message, "", "缺 message 时如实给空串，不编谎");
            }
            other => panic!("expected failure: {other:?}"),
        }
    }

    #[test]
    fn decode_response_rejects_malformed_lines() {
        fn is_invalid_response(error: &ProtocolError) -> bool {
            matches!(error, ProtocolError::InvalidResponse(_))
        }
        fn is_invalid_error(error: &ProtocolError) -> bool {
            matches!(error, ProtocolError::InvalidError(_))
        }
        fn is_invalid_utf8(error: &ProtocolError) -> bool {
            matches!(error, ProtocolError::InvalidUtf8(_))
        }
        /// 对某个畸形响应行的判定函数（测试表用，避免 clippy::type_complexity）。
        type Check = fn(&ProtocolError) -> bool;
        let cases: &[(&[u8], Check)] = &[
            (b"", is_invalid_response),
            (b"\r\n", is_invalid_response),
            (b"{ not json", is_invalid_response),
            (b"[]", is_invalid_response),
            (b"\"ok\"", is_invalid_response),
            (b"null", is_invalid_response),
            (b"{\"mute\":true}", is_invalid_response),
            (b"{\"ok\":\"yes\"}", is_invalid_response),
            (b"{\"ok\":false,\"message\":\"x\"}", is_invalid_error),
            (b"{\"ok\":false,\"error\":7}", is_invalid_error),
            (b"\xff\xfe", is_invalid_utf8),
        ];
        for (line, check) in cases {
            let error = decode_response(line).expect_err("malformed");
            assert!(check(&error), "{line:?}: {error}");
        }
    }

    #[test]
    fn read_line_limited_splits_lines_and_resyncs_after_oversize() {
        let mut reader = BufReader::new(Cursor::new(b"first\r\nsecond\nthird"));
        let mut line = Vec::new();
        assert_eq!(
            read_line_limited(&mut reader, &mut line).expect("line"),
            LineRead::Line
        );
        assert_eq!(line, b"first\r");
        assert_eq!(
            read_line_limited(&mut reader, &mut line).expect("line"),
            LineRead::Line
        );
        assert_eq!(line, b"second");
        // 最后一行没有换行符：EOF 前按一行处理。
        assert_eq!(
            read_line_limited(&mut reader, &mut line).expect("line"),
            LineRead::Line
        );
        assert_eq!(line, b"third");
        assert_eq!(
            read_line_limited(&mut reader, &mut line).expect("eof"),
            LineRead::Eof
        );
    }

    #[test]
    fn read_line_limited_discards_oversized_line_and_keeps_sync() {
        let mut payload = vec![b'a'; MAX_RESPONSE_BYTES + 16];
        payload.extend_from_slice(b"\n{\"ok\":true}\n");
        let mut reader = BufReader::new(Cursor::new(payload));
        let mut line = Vec::new();
        assert_eq!(
            read_line_limited(&mut reader, &mut line).expect("too long"),
            LineRead::TooLong
        );
        // 超限后仍从下一行边界继续读（坏行不破坏连接）。
        assert_eq!(
            read_line_limited(&mut reader, &mut line).expect("line"),
            LineRead::Line
        );
        assert_eq!(line, br#"{"ok":true}"#);
    }

    #[test]
    fn read_line_limited_accepts_exactly_the_response_limit() {
        // 构造恰好 MAX_RESPONSE_BYTES 字节的一行：{"ok":true,"pad":"..."} + 换行。
        let prefix = br#"{"ok":true,"pad":""#;
        let suffix = b"\"}";
        let pad = MAX_RESPONSE_BYTES - prefix.len() - suffix.len();
        let mut payload = Vec::with_capacity(MAX_RESPONSE_BYTES + 8);
        payload.extend_from_slice(prefix);
        payload.extend(std::iter::repeat_n(b'x', pad));
        payload.extend_from_slice(suffix);
        assert_eq!(payload.len(), MAX_RESPONSE_BYTES);
        payload.push(b'\n');
        payload.extend_from_slice(b"{\"ok\":true}\n");
        let mut reader = BufReader::new(Cursor::new(payload));
        let mut line = Vec::new();
        assert_eq!(
            read_line_limited(&mut reader, &mut line).expect("line"),
            LineRead::Line
        );
        assert_eq!(line.len(), MAX_RESPONSE_BYTES);
        assert_eq!(
            read_line_limited(&mut reader, &mut line).expect("line"),
            LineRead::Line
        );
        assert_eq!(line, br#"{"ok":true}"#);
    }

    #[test]
    fn exchange_writes_one_line_and_decodes_reply() {
        let response = Cursor::new(br#"{"ok":true,"aborted":true}"#.to_vec());
        let mut reader = BufReader::new(response);
        let mut writer: Vec<u8> = Vec::new();
        let reply = exchange(
            &mut reader,
            &mut writer,
            &json!({ "op": "abort", "engine_token": "t" }),
        )
        .expect("exchange");
        assert!(matches!(reply, WireReply::Success(_)));
        assert_eq!(writer, b"{\"engine_token\":\"t\",\"op\":\"abort\"}\n");
    }

    #[test]
    fn exchange_reports_eof_and_oversized_responses_as_protocol_errors() {
        let mut reader = BufReader::new(Cursor::new(Vec::new()));
        let mut writer: Vec<u8> = Vec::new();
        let error =
            exchange(&mut reader, &mut writer, &json!({ "op": "status" })).expect_err("eof");
        assert!(matches!(
            error,
            ExchangeError::Protocol(ProtocolError::UnexpectedEof)
        ));

        let mut payload = vec![b'x'; MAX_RESPONSE_BYTES + 1];
        payload.push(b'\n');
        let mut reader = BufReader::new(Cursor::new(payload));
        let mut writer: Vec<u8> = Vec::new();
        let error =
            exchange(&mut reader, &mut writer, &json!({ "op": "status" })).expect_err("too large");
        assert!(matches!(
            error,
            ExchangeError::Protocol(ProtocolError::ResponseTooLarge { .. })
        ));

        // 请求编码错误在写之前就失败（不会往对端写半行）。
        let mut reader = BufReader::new(Cursor::new(br#"{"ok":true}"#.to_vec()));
        let mut writer: Vec<u8> = Vec::new();
        let error =
            exchange(&mut reader, &mut writer, &json!({ "op": "" })).expect_err("bad request");
        assert!(matches!(
            error,
            ExchangeError::Protocol(ProtocolError::InvalidRequest(_))
        ));
        assert!(writer.is_empty(), "请求不合法时不该写出任何字节");
    }
}
