//! report.json / metrics.csv / FIR CSV 输出原语（契约 §8）。
//!
//! 本模块只做序列化（零依赖的最小 JSON writer + C 风格 %.17g 格式化），
//! 报告语义由 pipeline 组装；数值格式：
//! - JSON：f64 最短往返十进制（NaN/±Inf → null，JSON 不允许）；
//! - FIR CSV：首行注释 + 每行 %.17g；
//! - metrics.csv：UTF-8 CSV，每 variant×点×带一行。

/// 最小 JSON 值（对象保持插入顺序，便于人读与 diff）。
#[derive(Clone, Debug)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn num(v: f64) -> Json {
        if v.is_finite() {
            Json::Num(v)
        } else {
            Json::Null
        }
    }
    pub fn opt_num(v: Option<f64>) -> Json {
        match v {
            Some(x) => Json::num(x),
            None => Json::Null,
        }
    }
    pub fn str<S: Into<String>>(s: S) -> Json {
        Json::Str(s.into())
    }
    pub fn arr(items: Vec<Json>) -> Json {
        Json::Arr(items)
    }
    pub fn obj(items: Vec<(&str, Json)>) -> Json {
        Json::Obj(items.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }
    /// 紧凑序列化。
    pub fn to_compact(&self) -> String {
        let mut out = String::new();
        self.write_compact(&mut out);
        out
    }
    /// 2 空格缩进的美化序列化（report.json 用，便于人工审查与 diff）。
    pub fn to_pretty(&self) -> String {
        let mut out = String::new();
        self.write_pretty(&mut out, 0);
        out.push('\n');
        out
    }
    fn write_compact(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Num(v) => out.push_str(&fmt_json_num(*v)),
            Json::Str(s) => write_json_str(s, out),
            Json::Arr(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write_compact(out);
                }
                out.push(']');
            }
            Json::Obj(items) => {
                out.push('{');
                for (i, (k, v)) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_json_str(k, out);
                    out.push(':');
                    v.write_compact(out);
                }
                out.push('}');
            }
        }
    }
    fn write_pretty(&self, out: &mut String, indent: usize) {
        match self {
            Json::Arr(items) => {
                if items.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push_str("[\n");
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(",\n");
                    }
                    push_indent(out, indent + 1);
                    v.write_pretty(out, indent + 1);
                }
                out.push('\n');
                push_indent(out, indent);
                out.push(']');
            }
            Json::Obj(items) => {
                if items.is_empty() {
                    out.push_str("{}");
                    return;
                }
                out.push_str("{\n");
                for (i, (k, v)) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(",\n");
                    }
                    push_indent(out, indent + 1);
                    write_json_str(k, out);
                    out.push_str(": ");
                    v.write_pretty(out, indent + 1);
                }
                out.push('\n');
                push_indent(out, indent);
                out.push('}');
            }
            other => other.write_compact(out),
        }
    }
}

fn push_indent(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push_str("  ");
    }
}

/// 校准产物 JSON 里浮点数的**规范化有效数字位数**。
///
/// 跨平台 libm（glibc / ucrt）对 sin/exp/ln 等的末位结果可能差 1 ulp（相对 1e-16），
/// 若直接输出最短表示，同一输入在 Windows 与 Linux 上产物文本不同、sha256 不同，
/// 违反"同输入同参数必同 sha"（契约 C.3）。量化到 12 位有效数字后，这种末位差
/// 不会改变文本；10 位远超任何有物理意义的精度要求（dB/Hz/延迟），
/// 且落在量化边界上的概率约 1e-6/值，可忽略。
pub const CANONICAL_SIG_DIGITS: usize = 10;

fn fmt_json_num(v: f64) -> String {
    if !v.is_finite() {
        return "null".to_string();
    }
    if v == 0.0 {
        return "0".to_string();
    }
    // std 的浮点格式化与解析都是确定性的（不经过平台 libm）：
    // 先按固定有效位数格式化，再解析回 f64，最后用最短往返表示输出。
    let rounded: f64 = match format!("{:.*e}", CANONICAL_SIG_DIGITS - 1, v).parse() {
        Ok(value) => value,
        Err(_) => v,
    };
    if rounded == 0.0 {
        return "0".to_string();
    }
    format!("{}", rounded)
}

fn write_json_str(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// C 风格 %.17g：17 位有效数字；指数在 exp < -4 或 exp >= 17 时用科学计数法；
/// 去掉小数尾随 0；指数至少两位（e-05 / e+17）。
pub fn fmt_g17(x: f64) -> String {
    if x == 0.0 {
        return "0".to_string();
    }
    if !x.is_finite() {
        return if x.is_nan() {
            "nan".to_string()
        } else if x > 0.0 {
            "inf".to_string()
        } else {
            "-inf".to_string()
        };
    }
    let sign = if x < 0.0 { "-" } else { "" };
    let ax = x.abs();
    let exp = ax.log10().floor() as i32;
    if !(-4..17).contains(&exp) {
        let s = format!("{:.16e}", ax);
        let (mant, e) = s.split_once('e').unwrap_or((s.as_str(), "0"));
        let mant = mant.trim_end_matches('0').trim_end_matches('.');
        let e: i32 = e.parse().unwrap_or(0);
        format!(
            "{}{}e{}{:02}",
            sign,
            mant,
            if e < 0 { "-" } else { "+" },
            e.abs()
        )
    } else {
        let decimals = (16 - exp).max(0) as usize;
        let mut s = format!("{:.*}", decimals, ax);
        if s.contains('.') {
            s = s.trim_end_matches('0').trim_end_matches('.').to_string();
        }
        format!("{}{}", sign, s)
    }
}

/// FIR CSV 首行（契约 §8）：own 布局 source=<SRC>；modal 布局 out=<OUT> in=<IN>。
pub fn fir_csv_header(
    source: Option<&str>,
    out_id: Option<&str>,
    in_id: Option<&str>,
    taps: usize,
    sample_rate_hz: u32,
    delay_samples: usize,
) -> String {
    match (source, out_id, in_id) {
        (Some(src), _, _) => format!(
            "# source={} taps={} sample_rate_hz={} delay_samples={}",
            src, taps, sample_rate_hz, delay_samples
        ),
        (None, Some(o), Some(i)) => format!(
            "# out={} in={} taps={} sample_rate_hz={} delay_samples={}",
            o, i, taps, sample_rate_hz, delay_samples
        ),
        _ => format!(
            "# taps={} sample_rate_hz={} delay_samples={}",
            taps, sample_rate_hz, delay_samples
        ),
    }
}

/// 单源 FIR CSV 内容：首行注释 + 每行一个 tap（%.17g）。
pub fn fir_csv(source: &str, taps: &[f64], sample_rate_hz: u32, delay_samples: usize) -> String {
    let mut out = String::new();
    out.push_str(&fir_csv_header(
        Some(source),
        None,
        None,
        taps.len(),
        sample_rate_hz,
        delay_samples,
    ));
    out.push('\n');
    for t in taps {
        out.push_str(&fmt_g17(*t));
        out.push('\n');
    }
    out
}

/// 通用 CSV 字段转义（含逗号/引号/换行时加引号）。
pub fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escapes_and_roundtrip() {
        let v = Json::Obj(vec![
            ("a".to_string(), Json::Num(1.5)),
            ("b".to_string(), Json::Str("x\"y\nz".to_string())),
            (
                "c".to_string(),
                Json::Arr(vec![Json::Null, Json::Bool(true)]),
            ),
        ]);
        let text = v.to_compact();
        assert_eq!(text, "{\"a\":1.5,\"b\":\"x\\\"y\\nz\",\"c\":[null,true]}");
        // 美化输出仍是合法结构
        let pretty = v.to_pretty();
        assert!(pretty.contains("\"a\": 1.5"));
    }

    #[test]
    fn non_finite_numbers_become_null() {
        assert_eq!(Json::num(f64::NAN).to_compact(), "null");
        assert_eq!(Json::num(f64::INFINITY).to_compact(), "null");
        assert_eq!(Json::opt_num(None).to_compact(), "null");
        assert_eq!(Json::opt_num(Some(0.25)).to_compact(), "0.25");
    }

    #[test]
    fn g17_roundtrips_and_formats() {
        for v in [
            0.0,
            1.0,
            -1.0,
            0.1,
            1.0 / 3.0,
            1e-6,
            -2.5e-5,
            1234567.8901234567,
            1e17,
            1e-17,
            std::f64::consts::PI,
        ] {
            let s = fmt_g17(v);
            let back: f64 = s.parse().unwrap();
            assert_eq!(
                back.to_bits(),
                v.to_bits(),
                "value {} -> text {} -> {}",
                v,
                s,
                back
            );
        }
        assert_eq!(fmt_g17(0.0), "0");
        assert_eq!(fmt_g17(1.0), "1");
        assert_eq!(fmt_g17(0.1), "0.10000000000000001");
        assert_eq!(fmt_g17(1e-6), "9.9999999999999995e-07");
        assert_eq!(fmt_g17(1e17), "1e+17");
    }

    #[test]
    fn fir_csv_header_layouts() {
        assert_eq!(
            fir_csv_header(Some("SUB1"), None, None, 2048, 48000, 1024),
            "# source=SUB1 taps=2048 sample_rate_hz=48000 delay_samples=1024"
        );
        assert_eq!(
            fir_csv_header(None, Some("SUB1"), Some("SUB2"), 2048, 48000, 1024),
            "# out=SUB1 in=SUB2 taps=2048 sample_rate_hz=48000 delay_samples=1024"
        );
    }
}
