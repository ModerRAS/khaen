//! 确定性 JSON 组装与写盘。
//!
//! - 对象一律用 object()：键先进 BTreeMap 再转 serde_json Map，序列化顺序与 serde_json
//!   是否启用 preserve_order 无关，永远是键字典序；
//! - 浮点由 serde_json 用最短往返十进制（ryu）输出：同一个 f64 永远同一字节；
//! - 写盘 = to_string_pretty + 末尾换行；目标已存在算写失败（codes::WRITE，CLI 退出码 1），
//!   防止覆盖上一次的证据文件；
//! - 非有限浮点（NaN/正负无穷）写 null：JSON 表示不了，也绝不编一个大数。

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Map, Value};

use crate::codes::WRITE;
use crate::AnalyzeError;

/// 键字典序的 JSON 对象（键为字面量，值任意）。
pub fn object(pairs: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    let mut entries = BTreeMap::new();
    for (key, value) in pairs {
        entries.insert(key.to_string(), value);
    }
    Value::Object(entries.into_iter().collect::<Map<String, Value>>())
}

/// 有限浮点 -> JSON 数值；非有限 -> null。
pub fn number(value: f64) -> Value {
    serde_json::Number::from_f64(value)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

/// Option<f64> -> JSON 数值或 null。
pub fn opt_number(value: Option<f64>) -> Value {
    value.map_or(Value::Null, number)
}

/// 非负整数（计数/长度）-> JSON 数值。
pub fn count(value: u64) -> Value {
    Value::from(value)
}

/// f64 数组 -> JSON 数组（非有限项变 null）。
pub fn numbers(values: &[f64]) -> Value {
    Value::Array(values.iter().map(|&value| number(value)).collect())
}

/// Option<f64> 数组 -> JSON 数组（None/非有限变 null）。
pub fn opt_numbers(values: &[Option<f64>]) -> Value {
    Value::Array(values.iter().map(|&value| opt_number(value)).collect())
}

/// 写确定性 JSON 文件（pretty + 末尾换行），返回写出的字节（调用方用于 sha256）。
///
/// - 目标已存在 -> E_WRITE（不覆盖已有证据）；
/// - 父目录不存在则创建；创建/写盘失败 -> E_WRITE。
pub fn write_document(path: &Path, document: &Value) -> Result<Vec<u8>, AnalyzeError> {
    if path.exists() {
        return Err(AnalyzeError::new(
            WRITE,
            format!("output file {} already exists", path.display()),
        ));
    }
    let mut text = serde_json::to_string_pretty(document)
        .map_err(|err| AnalyzeError::new(WRITE, format!("serialize {}: {err}", path.display())))?;
    text.push(NEWLINE);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|err| {
                AnalyzeError::new(WRITE, format!("create {}: {err}", parent.display()))
            })?;
        }
    }
    std::fs::write(path, text.as_bytes())
        .map_err(|err| AnalyzeError::new(WRITE, format!("write {}: {err}", path.display())))?;
    Ok(text.into_bytes())
}

/// 单行紧凑 JSON（stdout 摘要用；不含换行，由调用方 println）。
pub fn to_line(document: &Value) -> Result<String, AnalyzeError> {
    serde_json::to_string(document)
        .map_err(|err| AnalyzeError::new(WRITE, format!("serialize summary: {err}")))
}

/// 换行字节（写成常量避免在文档里出现转义）。
const NEWLINE: char = '\n';
