//! Minimal dependency-free JSON value builder (stats/evidence output).
//!
//! We avoid serde in this prototype: the emitted documents are small and the
//! exact field names are part of the evidence contract checked by scripts.

use std::fmt::Write as _;

/// Numeric inputs accepted by `J::num` (f64 only implements From for small ints,
/// so provide an explicit conversion trait that also covers u64/usize).
pub trait IntoNum {
    fn into_num(self) -> f64;
}
macro_rules! impl_into_num {
    ($($t:ty),* $(,)?) => {
        $(impl IntoNum for $t { fn into_num(self) -> f64 { self as f64 } })*
    };
}
impl_into_num!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize, f32, f64);

#[derive(Debug, Clone, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    pub fn obj<I, K>(pairs: I) -> J
    where
        I: IntoIterator<Item = (K, J)>,
        K: Into<String>,
    {
        J::Obj(pairs.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    pub fn arr<I: IntoIterator<Item = J>>(items: I) -> J {
        J::Arr(items.into_iter().collect())
    }

    pub fn str<S: Into<String>>(s: S) -> J {
        J::Str(s.into())
    }

    pub fn num<N: IntoNum>(n: N) -> J {
        J::Num(n.into_num())
    }

    pub fn bool(b: bool) -> J {
        J::Bool(b)
    }

    pub fn opt_num(n: Option<f64>) -> J {
        match n {
            Some(v) => J::Num(v),
            None => J::Null,
        }
    }

    pub fn get(&self, key: &str) -> Option<&J> {
        match self {
            J::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            J::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            J::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn to_json(&self) -> String {
        let mut s = String::new();
        self.write_compact(&mut s);
        s
    }

    pub fn to_json_pretty(&self) -> String {
        let mut s = String::new();
        self.write_pretty(&mut s, 0);
        s.push('\n');
        s
    }

    fn write_compact(&self, out: &mut String) {
        match self {
            J::Null => out.push_str("null"),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Num(n) => write_num(out, *n),
            J::Str(s) => write_str(out, s),
            J::Arr(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write_compact(out);
                }
                out.push(']');
            }
            J::Obj(pairs) => {
                out.push('{');
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_str(out, k);
                    out.push(':');
                    v.write_compact(out);
                }
                out.push('}');
            }
        }
    }

    fn write_pretty(&self, out: &mut String, indent: usize) {
        let pad = "  ".repeat(indent);
        let pad2 = "  ".repeat(indent + 1);
        match self {
            J::Arr(items) if !items.is_empty() => {
                out.push_str("[\n");
                for (i, v) in items.iter().enumerate() {
                    out.push_str(&pad2);
                    v.write_pretty(out, indent + 1);
                    if i + 1 < items.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&pad);
                out.push(']');
            }
            J::Obj(pairs) if !pairs.is_empty() => {
                out.push_str("{\n");
                for (i, (k, v)) in pairs.iter().enumerate() {
                    out.push_str(&pad2);
                    write_str(out, k);
                    out.push_str(": ");
                    v.write_pretty(out, indent + 1);
                    if i + 1 < pairs.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&pad);
                out.push('}');
            }
            other => other.write_compact(out),
        }
    }
}

fn write_num(out: &mut String, n: f64) {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9.0e15 {
        let _ = write!(out, "{}", n as i64);
    } else if n.is_finite() {
        let _ = write!(out, "{:.6}", n);
    } else {
        out.push_str("null");
    }
}

fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escapes_and_numbers() {
        let v = J::obj(vec![
            ("name", J::str("a\"b\\c")),
            ("n", J::num(48_000u32)),
            ("f", J::num(0.125)),
            ("ok", J::bool(true)),
            ("empty", J::Null),
        ]);
        let s = v.to_json();
        assert!(s.contains("\"name\":\"a\\\"b\\\\c\""));
        assert!(s.contains("\"n\":48000"));
        assert!(s.contains("\"f\":0.125000"));
        assert!(s.contains("\"ok\":true"));
        assert!(s.contains("\"empty\":null"));
        // prettified form is valid-ish: contains newlines and brackets
        let p = v.to_json_pretty();
        assert!(p.starts_with('{'));
        assert!(p.trim_end().ends_with('}'));
    }
}
