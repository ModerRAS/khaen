//! [NONRT] 自有严格 JSON 解析/规范化写作（零第三方依赖）。
//!
//! session manifest、`tspi-fir/v1` manifest、`tspi-cal-ir-draft/v1` 绑定与 runner 报告全部是 JSON；
//! crate 不引入 serde。本模块是唯一 JSON 入口：
//!
//! - **解析**：RFC 8259 子集，严格拒绝注释/尾逗号/重复键/非有限数/尾随内容/超深嵌套；
//!   数字按 JSON 语法校验后经 `f64` 解析（整数 <= 2^53 精确；更大整数只用于计数，不做位级运算）。
//! - **写作**：[`Json::to_canonical`] 确定性输出：对象键按字典序、无空白、最短数字表示；
//!   同一 Value 永远得到同一字符串（报告/图 hash/证据可复算）。
//!
//! 错误：语法/结构 -> `E_FILE_JSON`；字段缺失/类型不符 -> `E_FILE_FIELD`（由调用方 require_* 产生）。

use std::collections::BTreeMap;

use crate::RunnerResult;
use crate::{RunnerError, E_FILE_FIELD, E_FILE_JSON, MAX_JSON_DEPTH};

/// JSON 值（对象键有序，写作/round-trip 确定）。
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    /// `null`。
    Null,
    /// `true` / `false`。
    Bool(bool),
    /// 有限 JSON 数字（解析时拒绝 NaN/Inf/溢出）。
    Number(f64),
    /// UTF-8 字符串。
    String(String),
    /// 数组。
    Array(Vec<Json>),
    /// 对象（BTreeMap：键有序、无重复）。
    Object(BTreeMap<String, Json>),
}

impl Json {
    /// 空对象。
    pub fn object() -> Self {
        Json::Object(BTreeMap::new())
    }

    /// 构造对象（重复键后者覆盖前者；调用方保证语义唯一）。
    pub fn from_entries(entries: Vec<(&str, Json)>) -> Self {
        let mut map = BTreeMap::new();
        for (key, value) in entries {
            map.insert(key.to_string(), value);
        }
        Json::Object(map)
    }

    /// 该值类型的稳定名称（错误消息/报告用）。
    pub fn kind(&self) -> &'static str {
        match self {
            Json::Null => "null",
            Json::Bool(_) => "bool",
            Json::Number(_) => "number",
            Json::String(_) => "string",
            Json::Array(_) => "array",
            Json::Object(_) => "object",
        }
    }

    /// 对象取字段（非对象/缺字段 -> None）。
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(map) => map.get(key),
            _ => None,
        }
    }

    /// 对象视图。
    pub fn as_object(&self) -> Option<&BTreeMap<String, Json>> {
        match self {
            Json::Object(map) => Some(map),
            _ => None,
        }
    }

    /// 数组视图。
    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(values) => Some(values),
            _ => None,
        }
    }

    /// 字符串视图。
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(text) => Some(text),
            _ => None,
        }
    }

    /// 布尔视图。
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// 数字视图（f64；仅 Number）。
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Number(value) => Some(*value),
            _ => None,
        }
    }

    /// 非负整数字段（u32/u64 语义；小数/负数/超界 -> None）。
    pub fn as_u64(&self) -> Option<u64> {
        let value = self.as_f64()?;
        if !value.is_finite() || value < 0.0 || value.fract() != 0.0 || value > u64::MAX as f64 {
            return None;
        }
        Some(value as u64)
    }

    /// 整数字段（i64 语义）。
    pub fn as_i64(&self) -> Option<i64> {
        let value = self.as_f64()?;
        if !value.is_finite() || value.fract() != 0.0 {
            return None;
        }
        if value < i64::MIN as f64 || value > i64::MAX as f64 {
            return None;
        }
        Some(value as i64)
    }

    /// 必填字段（缺失/类型不匹配返回错误；类型检查由调用方继续做）。
    pub fn require(&self, key: &str) -> RunnerResult<&Json> {
        match self.get(key) {
            Some(value) => Ok(value),
            None => Err(RunnerError::new(
                E_FILE_FIELD,
                format!("missing required field '{}' in {}", key, self.kind()),
            )),
        }
    }

    /// 必填字符串字段。
    pub fn require_str(&self, key: &str) -> RunnerResult<&str> {
        let value = self.require(key)?;
        value.as_str().ok_or_else(|| {
            RunnerError::new(
                E_FILE_FIELD,
                format!("field '{}' must be a string (got {})", key, value.kind()),
            )
        })
    }

    /// 必填 u64 字段（非负整数）。
    pub fn require_u64(&self, key: &str) -> RunnerResult<u64> {
        let value = self.require(key)?;
        value.as_u64().ok_or_else(|| {
            RunnerError::new(
                E_FILE_FIELD,
                format!(
                    "field '{}' must be a non-negative integer (got {})",
                    key,
                    value.kind()
                ),
            )
        })
    }

    /// 必填 f64 字段。
    pub fn require_f64(&self, key: &str) -> RunnerResult<f64> {
        let value = self.require(key)?;
        value.as_f64().ok_or_else(|| {
            RunnerError::new(
                E_FILE_FIELD,
                format!("field '{}' must be a number (got {})", key, value.kind()),
            )
        })
    }

    /// 必填对象字段。
    pub fn require_object(&self, key: &str) -> RunnerResult<&BTreeMap<String, Json>> {
        let value = self.require(key)?;
        value.as_object().ok_or_else(|| {
            RunnerError::new(
                E_FILE_FIELD,
                format!("field '{}' must be an object (got {})", key, value.kind()),
            )
        })
    }

    /// 必填数组字段。
    pub fn require_array(&self, key: &str) -> RunnerResult<&[Json]> {
        let value = self.require(key)?;
        value.as_array().ok_or_else(|| {
            RunnerError::new(
                E_FILE_FIELD,
                format!("field '{}' must be an array (got {})", key, value.kind()),
            )
        })
    }

    /// 解析 JSON 文本（严格；拒绝尾随内容/重复键/非有限数字/超深嵌套）。
    pub fn parse(text: &str) -> RunnerResult<Json> {
        let mut parser = Parser {
            bytes: text.as_bytes(),
            pos: 0,
        };
        parser.skip_ws();
        let value = parser.parse_value(0)?;
        parser.skip_ws();
        if parser.pos != parser.bytes.len() {
            return Err(parser.error("trailing content after JSON value"));
        }
        Ok(value)
    }

    /// 确定性规范化序列化（键有序、无空白、最短数字）。
    pub fn to_canonical(&self) -> String {
        let mut out = String::new();
        self.write_canonical(&mut out);
        out
    }

    fn write_canonical(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(true) => out.push_str("true"),
            Json::Bool(false) => out.push_str("false"),
            Json::Number(value) => write_number(*value, out),
            Json::String(text) => write_string(text, out),
            Json::Array(values) => {
                out.push('[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    value.write_canonical(out);
                }
                out.push(']');
            }
            Json::Object(map) => {
                out.push('{');
                for (index, (key, value)) in map.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write_string(key, out);
                    out.push(':');
                    value.write_canonical(out);
                }
                out.push('}');
            }
        }
    }
}

/// 便捷：字符串值。
pub fn string(text: impl Into<String>) -> Json {
    Json::String(text.into())
}

/// 便捷：u64 值（<= 2^53 时精确）。
pub fn uint(value: u64) -> Json {
    Json::Number(value as f64)
}

/// 便捷：f64 值（调用方保证有限；非有限写 null 的防御在写函数内）。
pub fn number(value: f64) -> Json {
    Json::Number(value)
}

fn write_number(value: f64, out: &mut String) {
    if !value.is_finite() {
        // 防御：非有限 JSON 数字不合法；写 null 而不是 panic（解析端也不会产生）。
        out.push_str("null");
        return;
    }
    if value == 0.0 {
        out.push('0');
        return;
    }
    let integral = value.fract() == 0.0;
    if integral && value.abs() < 1e16 {
        out.push_str(&(value as i64).to_string());
    } else {
        out.push_str(&format!("{}", value));
    }
}

fn write_string(text: &str, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str("\\u00");
                out.push(HEX[((c as u32) >> 4) as usize] as char);
                out.push(HEX[((c as u32) & 0x0f) as usize] as char);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn error(&self, message: &str) -> RunnerError {
        RunnerError::new(
            E_FILE_JSON,
            format!("{} at byte offset {}", message, self.pos),
        )
    }

    fn skip_ws(&mut self) {
        while let Some(&byte) = self.bytes.get(self.pos) {
            if byte == b' ' || byte == b'\t' || byte == b'\n' || byte == b'\r' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn expect(&mut self, byte: u8) -> RunnerResult<()> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.error(&format!("expected '{}'", byte as char)))
        }
    }

    fn parse_value(&mut self, depth: usize) -> RunnerResult<Json> {
        if depth > MAX_JSON_DEPTH {
            return Err(self.error("maximum JSON nesting depth exceeded"));
        }
        match self.peek() {
            None => Err(self.error("unexpected end of input")),
            Some(b'{') => self.parse_object(depth),
            Some(b'[') => self.parse_array(depth),
            Some(b'"') => Ok(Json::String(self.parse_string()?)),
            Some(b't') => {
                self.expect_literal(b"true")?;
                Ok(Json::Bool(true))
            }
            Some(b'f') => {
                self.expect_literal(b"false")?;
                Ok(Json::Bool(false))
            }
            Some(b'n') => {
                self.expect_literal(b"null")?;
                Ok(Json::Null)
            }
            Some(byte) if byte == b'-' || byte.is_ascii_digit() => self.parse_number(),
            Some(byte) => Err(self.error(&format!("unexpected byte 0x{:02x}", byte))),
        }
    }

    fn expect_literal(&mut self, literal: &[u8]) -> RunnerResult<()> {
        if self.bytes.len() >= self.pos + literal.len()
            && &self.bytes[self.pos..self.pos + literal.len()] == literal
        {
            self.pos += literal.len();
            Ok(())
        } else {
            Err(self.error("invalid literal"))
        }
    }

    fn parse_object(&mut self, depth: usize) -> RunnerResult<Json> {
        self.expect(b'{')?;
        let mut map: BTreeMap<String, Json> = BTreeMap::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Object(map));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.error("object key must be a string"));
            }
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(b':')?;
            self.skip_ws();
            let value = self.parse_value(depth + 1)?;
            if map.insert(key.clone(), value).is_some() {
                return Err(self.error(&format!("duplicate object key '{}'", key)));
            }
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(self.error("expected ',' or '}' in object")),
            }
        }
        Ok(Json::Object(map))
    }

    fn parse_array(&mut self, depth: usize) -> RunnerResult<Json> {
        self.expect(b'[')?;
        let mut values = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Array(values));
        }
        loop {
            self.skip_ws();
            values.push(self.parse_value(depth + 1)?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(self.error("expected ',' or ']' in array")),
            }
        }
        Ok(Json::Array(values))
    }

    fn parse_string(&mut self) -> RunnerResult<String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let byte = match self.peek() {
                Some(value) => value,
                None => return Err(self.error("unterminated string")),
            };
            match byte {
                b'"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.pos += 1;
                    let escaped = match self.peek() {
                        Some(value) => value,
                        None => return Err(self.error("unterminated escape")),
                    };
                    self.pos += 1;
                    match escaped {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{08}'),
                        b'f' => out.push('\u{0c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let first = self.parse_hex4()?;
                            let codepoint = if (0xD800..=0xDBFF).contains(&first) {
                                if self.peek() != Some(b'\\') {
                                    return Err(self.error("lone high surrogate"));
                                }
                                self.pos += 1;
                                if self.peek() != Some(b'u') {
                                    return Err(self.error("expected low surrogate"));
                                }
                                self.pos += 1;
                                let second = self.parse_hex4()?;
                                if !(0xDC00..=0xDFFF).contains(&second) {
                                    return Err(self.error("invalid low surrogate"));
                                }
                                0x10000 + ((first as u32 - 0xD800) << 10) + (second as u32 - 0xDC00)
                            } else if (0xDC00..=0xDFFF).contains(&first) {
                                return Err(self.error("lone low surrogate"));
                            } else {
                                first as u32
                            };
                            match char::from_u32(codepoint) {
                                Some(ch) => out.push(ch),
                                None => return Err(self.error("invalid unicode escape")),
                            }
                        }
                        _ => return Err(self.error("invalid escape character")),
                    }
                }
                byte if byte < 0x20 => return Err(self.error("raw control character in string")),
                _ => {
                    let ch = self.next_char()?;
                    out.push(ch);
                }
            }
        }
    }

    fn parse_hex4(&mut self) -> RunnerResult<u16> {
        let mut value: u16 = 0;
        for _ in 0..4 {
            let byte = match self.peek() {
                Some(value) => value,
                None => return Err(self.error("truncated unicode escape")),
            };
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => return Err(self.error("invalid hex digit in unicode escape")),
            };
            value = (value << 4) | digit as u16;
            self.pos += 1;
        }
        Ok(value)
    }

    fn next_char(&mut self) -> RunnerResult<char> {
        let rest = &self.bytes[self.pos..];
        let text = std::str::from_utf8(rest).map_err(|_| self.error("invalid UTF-8"))?;
        let ch = match text.chars().next() {
            Some(value) => value,
            None => return Err(self.error("unexpected end of input")),
        };
        self.pos += ch.len_utf8();
        Ok(ch)
    }

    fn parse_number(&mut self) -> RunnerResult<Json> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
                if let Some(byte) = self.peek() {
                    if byte.is_ascii_digit() {
                        return Err(self.error("leading zero in number"));
                    }
                }
            }
            Some(byte) if byte.is_ascii_digit() => {
                while let Some(next) = self.peek() {
                    if next.is_ascii_digit() {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
            }
            _ => return Err(self.error("invalid number")),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            let mut digits = 0;
            while let Some(next) = self.peek() {
                if next.is_ascii_digit() {
                    self.pos += 1;
                    digits += 1;
                } else {
                    break;
                }
            }
            if digits == 0 {
                return Err(self.error("missing fraction digits"));
            }
        }
        if let Some(b'e') | Some(b'E') = self.peek() {
            self.pos += 1;
            if let Some(b'+') | Some(b'-') = self.peek() {
                self.pos += 1;
            }
            let mut digits = 0;
            while let Some(next) = self.peek() {
                if next.is_ascii_digit() {
                    self.pos += 1;
                    digits += 1;
                } else {
                    break;
                }
            }
            if digits == 0 {
                return Err(self.error("missing exponent digits"));
            }
        }
        let text = match std::str::from_utf8(&self.bytes[start..self.pos]) {
            Ok(value) => value,
            Err(_) => return Err(self.error("invalid number bytes")),
        };
        let value: f64 = match text.parse() {
            Ok(value) => value,
            Err(_) => return Err(self.error("invalid number")),
        };
        if !value.is_finite() {
            return Err(self.error("number out of range (non-finite)"));
        }
        Ok(Json::Number(value))
    }
}
