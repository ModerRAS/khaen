//! 最小 JSON 解析器（够读 ground-truth.json）：对象成员顺序保留、数字 f64、字符串转义（含 \uXXXX 与
//! 代理对）、嵌套数组/对象。零外部依赖；不提供序列化（validation 输出直接手写）。

use std::path::Path;

/// Parsed JSON value. Object members keep document order.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    /// Parse one complete JSON document (leading/trailing whitespace allowed, optional UTF-8 BOM).
    pub fn parse(text: &str) -> Result<Json, String> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut parser = Parser {
            bytes: text.as_bytes(),
            position: 0,
        };
        parser.skip_whitespace();
        let value = parser.parse_value()?;
        parser.skip_whitespace();
        if parser.position != parser.bytes.len() {
            return Err(parser.error("trailing bytes after the top-level JSON value"));
        }
        Ok(value)
    }

    pub fn parse_file(path: &Path) -> Result<Json, String> {
        let bytes = std::fs::read(path)
            .map_err(|error| format!("cannot read JSON file {}: {error}", path.display()))?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|error| format!("JSON file {} is not valid UTF-8: {error}", path.display()))?;
        Json::parse(text).map_err(|error| format!("invalid JSON in {}: {error}", path.display()))
    }

    /// Object member lookup (first match wins; JSON fixture keys are unique).
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(members) => members
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// Nested lookup: at(&["rir", "scale"]).
    pub fn at(&self, keys: &[&str]) -> Option<&Json> {
        let mut current = self;
        for key in keys {
            current = current.get(key)?;
        }
        Some(current)
    }

    pub fn as_object(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Object(members) => Some(members),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Number(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        let value = self.as_f64()?;
        if value.is_finite() && value >= 0.0 && value.fract() == 0.0 && value <= u64::MAX as f64 {
            Some(value as u64)
        } else {
            None
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// Stable description for error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Json::Null => "null",
            Json::Bool(_) => "bool",
            Json::Number(_) => "number",
            Json::String(_) => "string",
            Json::Array(_) => "array",
            Json::Object(_) => "object",
        }
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Parser<'a> {
    fn error(&self, message: &str) -> String {
        format!("{message} (at byte offset {})", self.position)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.position += 1;
        Some(byte)
    }

    fn skip_whitespace(&mut self) {
        while let Some(byte) = self.peek() {
            if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
                self.position += 1;
            } else {
                break;
            }
        }
    }

    fn expect(&mut self, expected: u8) -> Result<(), String> {
        match self.advance() {
            Some(byte) if byte == expected => Ok(()),
            Some(byte) => Err(self.error(&format!(
                "expected '{}', found '{}'",
                expected as char, byte as char
            ))),
            None => Err(self.error(&format!(
                "expected '{}', found end of input",
                expected as char
            ))),
        }
    }

    fn parse_value(&mut self) -> Result<Json, String> {
        self.skip_whitespace();
        match self.peek() {
            None => Err(self.error("unexpected end of input")),
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'"') => Ok(Json::String(self.parse_string()?)),
            Some(b't') => {
                self.parse_literal("true")?;
                Ok(Json::Bool(true))
            }
            Some(b'f') => {
                self.parse_literal("false")?;
                Ok(Json::Bool(false))
            }
            Some(b'n') => {
                self.parse_literal("null")?;
                Ok(Json::Null)
            }
            Some(byte) if byte == b'-' || byte.is_ascii_digit() => self.parse_number(),
            Some(byte) => Err(self.error(&format!("unexpected character '{}'", byte as char))),
        }
    }

    fn parse_literal(&mut self, literal: &str) -> Result<(), String> {
        for expected in literal.bytes() {
            match self.advance() {
                Some(byte) if byte == expected => {}
                _ => return Err(self.error(&format!("invalid literal, expected '{literal}'"))),
            }
        }
        Ok(())
    }

    fn parse_object(&mut self) -> Result<Json, String> {
        self.expect(b'{')?;
        let mut members: Vec<(String, Json)> = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.advance();
            return Ok(Json::Object(members));
        }
        loop {
            self.skip_whitespace();
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            let value = self.parse_value()?;
            members.push((key, value));
            self.skip_whitespace();
            match self.advance() {
                Some(b',') => continue,
                Some(b'}') => break,
                Some(byte) => {
                    return Err(self.error(&format!(
                        "expected ',' or '}}' in object, found '{}'",
                        byte as char
                    )))
                }
                None => return Err(self.error("unterminated object")),
            }
        }
        Ok(Json::Object(members))
    }

    fn parse_array(&mut self) -> Result<Json, String> {
        self.expect(b'[')?;
        let mut items: Vec<Json> = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.advance();
            return Ok(Json::Array(items));
        }
        loop {
            let value = self.parse_value()?;
            items.push(value);
            self.skip_whitespace();
            match self.advance() {
                Some(b',') => continue,
                Some(b']') => break,
                Some(byte) => {
                    return Err(self.error(&format!(
                        "expected ',' or ']' in array, found '{}'",
                        byte as char
                    )))
                }
                None => return Err(self.error("unterminated array")),
            }
        }
        Ok(Json::Array(items))
    }

    fn parse_number(&mut self) -> Result<Json, String> {
        let start = self.position;
        if self.peek() == Some(b'-') {
            self.advance();
        }
        match self.peek() {
            Some(b'0') => {
                self.advance();
            }
            Some(byte) if byte.is_ascii_digit() => {
                while matches!(self.peek(), Some(byte) if byte.is_ascii_digit()) {
                    self.advance();
                }
            }
            _ => return Err(self.error("invalid number: expected a digit")),
        }
        if self.peek() == Some(b'.') {
            self.advance();
            if !matches!(self.peek(), Some(byte) if byte.is_ascii_digit()) {
                return Err(self.error("invalid number: expected a digit after '.'"));
            }
            while matches!(self.peek(), Some(byte) if byte.is_ascii_digit()) {
                self.advance();
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            self.advance();
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.advance();
            }
            if !matches!(self.peek(), Some(byte) if byte.is_ascii_digit()) {
                return Err(self.error("invalid number: expected an exponent digit"));
            }
            while matches!(self.peek(), Some(byte) if byte.is_ascii_digit()) {
                self.advance();
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.position])
            .map_err(|_| self.error("number is not valid UTF-8"))?;
        text.parse::<f64>()
            .map(Json::Number)
            .map_err(|_| self.error(&format!("invalid number '{text}'")))
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let byte = match self.advance() {
                Some(byte) => byte,
                None => return Err(self.error("unterminated string")),
            };
            match byte {
                b'"' => break,
                b'\\' => {
                    let escape = match self.advance() {
                        Some(escape) => escape,
                        None => return Err(self.error("unterminated escape sequence")),
                    };
                    match escape {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0c),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let first = self.parse_hex4()?;
                            let code_point = if (0xD800..=0xDBFF).contains(&first) {
                                if self.advance() != Some(b'\\') || self.advance() != Some(b'u') {
                                    return Err(
                                        self.error("high surrogate without a following \\u escape")
                                    );
                                }
                                let second = self.parse_hex4()?;
                                if !(0xDC00..=0xDFFF).contains(&second) {
                                    return Err(self.error("invalid low surrogate"));
                                }
                                0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00)
                            } else if (0xDC00..=0xDFFF).contains(&first) {
                                return Err(self.error("lone low surrogate"));
                            } else {
                                first
                            };
                            let character = char::from_u32(code_point)
                                .ok_or_else(|| self.error("invalid unicode escape"))?;
                            let mut buffer = [0u8; 4];
                            out.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
                        }
                        other => {
                            return Err(self
                                .error(&format!("invalid escape character '{}'", other as char)))
                        }
                    }
                }
                byte if byte < 0x20 => {
                    return Err(self.error("raw control character in string"));
                }
                other => out.push(other),
            }
        }
        String::from_utf8(out).map_err(|_| self.error("string is not valid UTF-8"))
    }

    fn parse_hex4(&mut self) -> Result<u32, String> {
        let mut value = 0u32;
        for _ in 0..4 {
            let byte = match self.advance() {
                Some(byte) => byte,
                None => return Err(self.error("truncated \\u escape")),
            };
            let digit = match byte {
                b'0'..=b'9' => (byte - b'0') as u32,
                b'a'..=b'f' => (byte - b'a' + 10) as u32,
                b'A'..=b'F' => (byte - b'A' + 10) as u32,
                _ => return Err(self.error("invalid hex digit in \\u escape")),
            };
            value = (value << 4) | digit;
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scalars_and_numbers() {
        assert_eq!(Json::parse("null").unwrap(), Json::Null);
        assert_eq!(Json::parse("true").unwrap(), Json::Bool(true));
        assert_eq!(Json::parse("false").unwrap(), Json::Bool(false));
        assert_eq!(Json::parse("20").unwrap(), Json::Number(20.0));
        assert_eq!(Json::parse("-1.5e-3").unwrap(), Json::Number(-1.5e-3));
        assert_eq!(Json::parse("12.5").unwrap().as_f64(), Some(12.5));
        assert_eq!(Json::parse("20261122").unwrap().as_u64(), Some(20261122));
        assert_eq!(Json::parse("1.5").unwrap().as_u64(), None);
    }

    #[test]
    fn preserves_object_order_and_nesting() {
        let document = r#"{"b": 1, "a": {"x": [1, 2, {"y": "z"}]}, "c": [true, false, null]}"#;
        let value = Json::parse(document).unwrap();
        let members = value.as_object().unwrap();
        assert_eq!(members[0].0, "b");
        assert_eq!(members[1].0, "a");
        assert_eq!(members[2].0, "c");
        assert_eq!(value.at(&["a", "x"]).unwrap().as_array().unwrap().len(), 3);
        assert_eq!(
            value.at(&["a", "x"]).unwrap().as_array().unwrap()[2]
                .get("y")
                .unwrap()
                .as_str(),
            Some("z")
        );
    }

    #[test]
    fn decodes_string_escapes() {
        let value = Json::parse(r#""a\"b\\c\/d\n\t\r\b\f\u4e2d\ud83d\ude00""#).unwrap();
        assert_eq!(
            value.as_str().unwrap(),
            "a\"b\\c/d\n\t\r\u{8}\u{c}\u{4e2d}\u{1f600}"
        );
    }

    #[test]
    fn parses_utf8_bom_and_raw_utf8() {
        let value = Json::parse("\u{feff}{\"中\": \"文\"}").unwrap();
        assert_eq!(value.get("中").unwrap().as_str(), Some("文"));
    }

    #[test]
    fn rejects_malformed_documents() {
        assert!(Json::parse("").is_err());
        assert!(Json::parse("{").is_err());
        assert!(Json::parse("{\"a\": }").is_err());
        assert!(Json::parse("[1, 2,]").is_err());
        assert!(Json::parse("{\"a\": 1} trailing").is_err());
        assert!(Json::parse("nan").is_err());
        assert!(Json::parse("\"unterminated").is_err());
        assert!(Json::parse("{\"a\" 1}").is_err());
    }

    #[test]
    fn parses_fixture_shaped_document() {
        let document = r#"{
            "format": "tspi-roomforge-mimo-fixture/v1",
            "seed": 20261122,
            "rir": {"format": "f32", "channels": 1, "length_samples": 24000,
                    "fft_size": 65536, "scale": 0.5},
            "room": {"length_m": 4.1, "reflections": {"left": 0.866}},
            "frequency_grid_hz": [19.775390625, 20.5078125],
            "rir_files": [{"source": "SUB1", "mic": "P01", "sha256": "ab"}]
        }"#;
        let value = Json::parse(document).unwrap();
        assert_eq!(
            value.get("format").unwrap().as_str(),
            Some("tspi-roomforge-mimo-fixture/v1")
        );
        assert_eq!(value.at(&["rir", "scale"]).unwrap().as_f64(), Some(0.5));
        assert_eq!(value.at(&["room", "length_m"]).unwrap().as_f64(), Some(4.1));
        assert_eq!(
            value.get("frequency_grid_hz").unwrap().as_array().unwrap()[0].as_f64(),
            Some(19.775390625)
        );
        assert_eq!(
            value.get("rir_files").unwrap().as_array().unwrap()[0]
                .get("mic")
                .unwrap()
                .as_str(),
            Some("P01")
        );
    }
}
