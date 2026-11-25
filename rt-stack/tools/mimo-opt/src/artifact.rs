//! 4.5 编译产物可复现：参数快照 + 报告 + 方案 + manifest + --verify。
//!
//! 硬约束（与计划 4.5 验收一致）：
//! - 确定性：不写时间戳 / 主机名 / 绝对路径 / PID / 随机数。同一输入 + 同一参数
//!   两次运行，manifest 里所有 sha256 完全相同（连 manifest.json 本身逐字节相同）。
//! - 可校验：[verify_run]（CLI --verify <dir>）重算 manifest 声明的每个文件的 sha256
//!   与字节数，重算 result_sha256，任何不符都指出具体文件/字段。
//! - 自洽：result_sha256 = sha256(canonical(结果描述))，结果描述里显式包含
//!   params_sha256 以及方案内容（FIR CSV / plan.bin）与报告内容的散列，
//!   不是"文件列表再散列一遍"。
//!
//! 目录布局（相对路径由调用方给出；本模块只负责写盘、散列与校验）：
//!
//!   --- 单场景 ----------------------------------------------
//!   run-<params_sha256 前 16 位>/
//!     params.json       完整输入参数快照（键排序、2 空格缩进）
//!     report.json       现有场景报告（内容与旧布局逐字节一致）
//!     metrics.csv       variant × 点 × 带指标
//!     fir/*.csv         方案内容（B2/B3 每个源一个 FIR）
//!     manifest.json     files[] + params_sha256 + result_sha256 + schema
//!
//!   --- 多场景 ----------------------------------------------
//!   run-<hash>/
//!     params.json
//!     manifest.json
//!     scenes/<scene_id>/{report.json,metrics.csv,fir/*.csv}
//!
//! plan.bin：仅当设计路径真的产出方案二进制时才写（本 CLI 的仿真夹具路径产出的是
//! 文本 FIR 候选库，**不伪造**运行期 host plan ABI）。manifest 的 plan 字段显式记录
//! present=false 与原因；result_sha256 仍然绑定方案内容（FIR CSV 散列）与报告内容。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use khaen_mimo_design::report::Json;
use khaen_room_sim::sha256;

/// manifest schema 版本。
pub const MANIFEST_SCHEMA: &str = "khaen-mimo-artifact-manifest/v1";
/// params 快照 schema 版本。
pub const PARAMS_SCHEMA: &str = "khaen-mimo-params/v1";
/// 结果描述 schema 版本（result_sha256 的域分隔）。
pub const RESULT_SCHEMA: &str = "khaen-mimo-result/v1";
/// 方案二进制 schema（预留给产出 plan.bin 的路径；本 CLI 路径当前不产出）。
pub const PLAN_SCHEMA: &str = "khaen-mimo-plan/v1";
/// 产物目录前缀。
pub const RUN_DIR_PREFIX: &str = "run-";
/// 报告文件名。
pub const REPORT_FILE: &str = "report.json";
/// 参数快照文件名。
pub const PARAMS_FILE: &str = "params.json";
/// manifest 文件名（不列入 files[]，避免自引用）。
pub const MANIFEST_FILE: &str = "manifest.json";

/// 文件角色：决定它参与结果描述的哪一段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileRole {
    /// params.json（输入参数快照）
    Params,
    /// report.json（现有报告）
    Report,
    /// 方案内容：FIR CSV / plan.bin
    Plan,
    /// 辅助结果：metrics.csv 等
    Aux,
}

impl FileRole {
    pub fn as_str(self) -> &'static str {
        match self {
            FileRole::Params => "params",
            FileRole::Report => "report",
            FileRole::Plan => "plan",
            FileRole::Aux => "aux",
        }
    }

    fn parse(value: &str) -> Option<FileRole> {
        match value {
            "params" => Some(FileRole::Params),
            "report" => Some(FileRole::Report),
            "plan" => Some(FileRole::Plan),
            "aux" => Some(FileRole::Aux),
            _ => None,
        }
    }
}

/// 一个待写产物文件（相对路径用 '/' 分隔；内容在内存里）。
#[derive(Clone, Debug)]
pub struct ArtifactFile {
    /// 相对路径（必须以 '/' 分隔，禁止绝对路径 / ..）
    pub path: String,
    pub role: FileRole,
    /// 所属场景（report/plan/aux 用；params 为 None）
    pub scene: Option<String>,
    pub bytes: Vec<u8>,
}

impl ArtifactFile {
    pub fn new(
        path: impl Into<String>,
        role: FileRole,
        scene: Option<String>,
        bytes: Vec<u8>,
    ) -> Self {
        Self {
            path: path.into(),
            role,
            scene,
            bytes,
        }
    }

    fn sha256(&self) -> String {
        sha256::hex_digest(&self.bytes)
    }
}

/// 方案二进制（预留给真实产出 plan.bin 的路径）。
#[derive(Clone, Debug)]
pub struct PlanArtifact {
    /// 相对路径（通常 "plan.bin"）
    pub path: String,
    /// 方案格式声明的 schema
    pub format: String,
    /// 是否运行期 host plan ABI（false = 离线设计方案，不是可直接 apply 的运行期 plan）
    pub runtime_abi: bool,
    pub bytes: Vec<u8>,
}

/// 写产物请求。
pub struct RunWriteRequest<'a> {
    pub dir: &'a Path,
    pub tool_name: &'a str,
    pub tool_version: &'a str,
    /// 已序列化的 params.json 文本（canonical：键排序 + 2 空格缩进）
    pub params_text: String,
    /// 其余产物（不含 params.json / manifest.json）
    pub files: Vec<ArtifactFile>,
    /// 方案二进制（None = 该路径不产出；manifest 里记录原因）
    pub plan: Option<PlanArtifact>,
    /// plan 缺失原因（plan = None 时写入 manifest，便于审计）
    pub plan_absent_reason: &'a str,
}

/// 写产物结果（打印用）。
#[derive(Clone, Debug)]
pub struct RunWriteOutcome {
    pub dir: PathBuf,
    pub run_id: String,
    pub params_sha256: String,
    pub result_sha256: String,
    pub file_count: usize,
    pub manifest_path: PathBuf,
}

/// run-<hash> 目录名：hash = params_sha256 前 16 位。
pub fn run_dir_name(params_sha256: &str) -> String {
    let short: String = params_sha256.chars().take(16).collect();
    format!("{RUN_DIR_PREFIX}{short}")
}

// ===========================================================================
// 规范 JSON：写入（键递归排序、2 空格缩进、行尾 \n）
// ===========================================================================

/// 规范 JSON 序列化：对象键递归按字典序排序，2 空格缩进，UTF-8，行尾换行。
///
/// 数值用 Rust Display（f64 最短往返十进制）；非有限数（JSON 不允许）写成 null。
pub fn canonical_json(value: &Json) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out, 0);
    out.push('\n');
    out
}

fn write_canonical(value: &Json, out: &mut String, indent: usize) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Json::Num(number) => {
            if number.is_finite() {
                out.push_str(&format!("{}", number));
            } else {
                out.push_str("null");
            }
        }
        Json::Str(text) => write_json_str(text, out),
        Json::Arr(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n");
                }
                push_indent(out, indent + 1);
                write_canonical(item, out, indent + 1);
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
            let mut sorted: Vec<&(String, Json)> = items.iter().collect();
            sorted.sort_by(|left, right| left.0.cmp(&right.0));
            out.push_str("{\n");
            for (index, (key, item)) in sorted.iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n");
                }
                push_indent(out, indent + 1);
                write_json_str(key, out);
                out.push_str(": ");
                write_canonical(item, out, indent + 1);
            }
            out.push('\n');
            push_indent(out, indent);
            out.push('}');
        }
    }
}

fn push_indent(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push_str("  ");
    }
}

fn write_json_str(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
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

fn json_opt_str(value: Option<String>) -> Json {
    match value {
        Some(text) => Json::Str(text),
        None => Json::Null,
    }
}

// ===========================================================================
// 规范 JSON：解析（verify 用；零外部依赖）
// ===========================================================================

/// 极简严格 JSON 解析器（verify 读 manifest / params 用；零外部依赖）。
///
/// 支持 RFC 8259（对象/数组/字符串/数字/true/false/null，含转义与代理对）；
/// 拒绝重复键、尾随内容、控制字符与非法数字。数字一律解析为 f64。
pub fn parse_json(text: &str) -> Result<Json, String> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        at: 0,
        depth: 0,
    };
    let value = parser.parse_value()?;
    parser.skip_ws();
    if parser.at != parser.bytes.len() {
        return Err(format!(
            "JSON 解析失败：位置 {} 之后还有尾随内容",
            parser.at
        ));
    }
    Ok(value)
}

const MAX_JSON_DEPTH: usize = 64;

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        while let Some(byte) = self.bytes.get(self.at) {
            if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
                self.at += 1;
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn error(&self, message: &str) -> String {
        format!("JSON 解析失败（位置 {}）：{}", self.at, message)
    }

    fn parse_value(&mut self) -> Result<Json, String> {
        self.depth += 1;
        if self.depth > MAX_JSON_DEPTH {
            return Err(self.error("嵌套层数过深"));
        }
        self.skip_ws();
        let next = self.peek().ok_or_else(|| self.error("意外结尾"))?;
        let value = match next {
            b'{' => self.parse_object()?,
            b'[' => self.parse_array()?,
            b'"' => Json::Str(self.parse_string_body()?),
            b't' => {
                self.expect_literal("true")?;
                Json::Bool(true)
            }
            b'f' => {
                self.expect_literal("false")?;
                Json::Bool(false)
            }
            b'n' => {
                self.expect_literal("null")?;
                Json::Null
            }
            b'-' | b'0'..=b'9' => self.parse_number()?,
            other => {
                return Err(self.error(&format!("非法字符 '{}'", other as char)));
            }
        };
        self.depth -= 1;
        Ok(value)
    }

    fn expect_literal(&mut self, literal: &str) -> Result<(), String> {
        if self.bytes[self.at..].starts_with(literal.as_bytes()) {
            self.at += literal.len();
            Ok(())
        } else {
            Err(self.error(&format!("期望字面量 {}", literal)))
        }
    }

    fn parse_object(&mut self) -> Result<Json, String> {
        self.at += 1; // '{'
        let mut items: Vec<(String, Json)> = Vec::new();
        let mut seen: BTreeMap<String, ()> = BTreeMap::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(Json::Obj(items));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.error("对象键必须是字符串"));
            }
            let key = self.parse_string_body()?;
            if seen.insert(key.clone(), ()).is_some() {
                return Err(self.error(&format!("重复的对象键 {:?}", key)));
            }
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(self.error("对象键后缺少 ':'"));
            }
            self.at += 1;
            let value = self.parse_value()?;
            items.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.at += 1;
                }
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Json::Obj(items));
                }
                _ => return Err(self.error("对象成员之间缺少 ',' 或 '}'")),
            }
        }
    }

    fn parse_array(&mut self) -> Result<Json, String> {
        self.at += 1; // '['
        let mut items: Vec<Json> = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            let value = self.parse_value()?;
            items.push(value);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.at += 1;
                }
                Some(b']') => {
                    self.at += 1;
                    return Ok(Json::Arr(items));
                }
                _ => return Err(self.error("数组元素之间缺少 ',' 或 ']'")),
            }
        }
    }

    fn parse_string_body(&mut self) -> Result<String, String> {
        // 调用方保证当前字节是 '"'
        self.at += 1;
        let mut out = String::new();
        loop {
            let byte = self.peek().ok_or_else(|| self.error("字符串未闭合"))?;
            match byte {
                b'"' => {
                    self.at += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.at += 1;
                    let escape = self.peek().ok_or_else(|| self.error("转义未完成"))?;
                    self.at += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let first = self.parse_hex4()?;
                            let ch = if (0xd800..=0xdbff).contains(&first) {
                                if self.peek() != Some(b'\\')
                                    || self.bytes.get(self.at + 1) != Some(&b'u')
                                {
                                    return Err(self.error("高代理项后缺少 \\u 低代理项"));
                                }
                                self.at += 2;
                                let second = self.parse_hex4()?;
                                if !(0xdc00..=0xdfff).contains(&second) {
                                    return Err(self.error("低代理项非法"));
                                }
                                let combined =
                                    0x10000 + ((first - 0xd800) << 10) + (second - 0xdc00);
                                char::from_u32(combined)
                                    .ok_or_else(|| self.error("代理对组合后不是合法字符"))?
                            } else if (0xdc00..=0xdfff).contains(&first) {
                                return Err(self.error("孤立低代理项"));
                            } else {
                                char::from_u32(first)
                                    .ok_or_else(|| self.error("\\u 转义不是合法字符"))?
                            };
                            out.push(ch);
                        }
                        other => {
                            return Err(self.error(&format!("非法转义 '\\{}'", other as char)));
                        }
                    }
                }
                0x00..=0x1f => {
                    return Err(self.error("字符串中包含未转义的控制字符"));
                }
                _ => {
                    // UTF-8 多字节序列：按原字节复制并推进
                    let start = self.at;
                    let width = utf8_width(byte);
                    if start + width > self.bytes.len() {
                        return Err(self.error("UTF-8 序列被截断"));
                    }
                    let slice = &self.bytes[start..start + width];
                    let text =
                        std::str::from_utf8(slice).map_err(|_| self.error("非法 UTF-8 序列"))?;
                    out.push_str(text);
                    self.at += width;
                }
            }
        }
    }

    fn parse_hex4(&mut self) -> Result<u32, String> {
        if self.at + 4 > self.bytes.len() {
            return Err(self.error("\\u 转义不足 4 位"));
        }
        let mut value = 0u32;
        for offset in 0..4 {
            let digit = self.bytes[self.at + offset];
            let nibble = match digit {
                b'0'..=b'9' => (digit - b'0') as u32,
                b'a'..=b'f' => (digit - b'a' + 10) as u32,
                b'A'..=b'F' => (digit - b'A' + 10) as u32,
                _ => return Err(self.error("\\u 转义含非十六进制字符")),
            };
            value = value * 16 + nibble;
        }
        self.at += 4;
        Ok(value)
    }

    fn parse_number(&mut self) -> Result<Json, String> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        // 整数部分
        match self.peek() {
            Some(b'0') => {
                self.at += 1;
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return Err(self.error("数字不允许前导零"));
                }
            }
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.at += 1;
                }
            }
            _ => return Err(self.error("数字缺少整数部分")),
        }
        // 小数部分
        if self.peek() == Some(b'.') {
            self.at += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("小数点后缺少数字"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.at += 1;
            }
        }
        // 指数
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("指数缺少数字"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.at += 1;
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.at])
            .map_err(|_| self.error("数字不是合法 UTF-8"))?;
        let number: f64 = text
            .parse()
            .map_err(|_| self.error(&format!("数字 {:?} 无法解析为 f64", text)))?;
        Ok(Json::Num(number))
    }
}

fn utf8_width(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first >> 5 == 0b110 {
        2
    } else if first >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

// ===========================================================================
// manifest 读取辅助（verify 用）
// ===========================================================================

fn obj_get<'a>(value: &'a Json, key: &str) -> Option<&'a Json> {
    match value {
        Json::Obj(items) => items
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, item)| item),
        _ => None,
    }
}

fn as_str<'a>(value: &'a Json, context: &str) -> Result<&'a str, String> {
    match value {
        Json::Str(text) => Ok(text),
        _ => Err(format!("{} 必须是字符串", context)),
    }
}

fn as_arr<'a>(value: &'a Json, context: &str) -> Result<&'a Vec<Json>, String> {
    match value {
        Json::Arr(items) => Ok(items),
        _ => Err(format!("{} 必须是数组", context)),
    }
}

fn as_u64(value: &Json, context: &str) -> Result<u64, String> {
    match value {
        Json::Num(number)
            if number.is_finite()
                && *number >= 0.0
                && number.fract() == 0.0
                && *number < 9.0e15 =>
        {
            Ok(*number as u64)
        }
        _ => Err(format!("{} 必须是非负整数", context)),
    }
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// 相对路径安全校验：禁止绝对路径、盘符、..、空段、反斜杠、NUL。
pub fn validate_relative_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("相对路径为空".to_string());
    }
    if path.contains('\\') {
        return Err(format!("相对路径必须用 '/' 分隔: {:?}", path));
    }
    if path.contains('\0') {
        return Err(format!("相对路径含 NUL: {:?}", path));
    }
    if path.starts_with('/') {
        return Err(format!("相对路径不得以 '/' 开头: {:?}", path));
    }
    if path.len() >= 2 && path.as_bytes()[1] == b':' {
        return Err(format!("相对路径不得含盘符: {:?}", path));
    }
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(format!("相对路径含非法段: {:?}", path));
        }
    }
    Ok(())
}

/// 目录里实际存在的文件（相对路径用 '/' 分隔，排除 exclude）。
fn actual_files(root: &Path, exclude: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    fn walk(root: &Path, current: &Path, out: &mut Vec<String>) -> Result<(), String> {
        let entries = std::fs::read_dir(current)
            .map_err(|error| format!("读取目录 {} 失败: {}", current.display(), error))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("遍历目录失败: {}", error))?;
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out)?;
            } else {
                let relative = path
                    .strip_prefix(root)
                    .map_err(|_| format!("路径 {} 不在 {} 下", path.display(), root.display()))?;
                let text = relative
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join("/");
                out.push(text);
            }
        }
        Ok(())
    }
    walk(root, root, &mut out)?;
    out.retain(|path| path != exclude);
    out.sort();
    Ok(out)
}

// ===========================================================================
// 写产物
// ===========================================================================

/// 写一次运行的产物目录，返回 run_id / params_sha256 / result_sha256。
///
/// 目录约定：
/// - 目录不存在 → 创建；
/// - 目录已存在且含 manifest.json（本工具产物）→ 整体重建（同输入必然写出同样字节）；
/// - 目录已存在但不含 manifest.json → 拒绝覆盖（避免误删用户数据）。
pub fn write_run(request: RunWriteRequest<'_>) -> Result<RunWriteOutcome, String> {
    let params_sha256 = sha256::hex_digest(request.params_text.as_bytes());
    let run_id = run_dir_name(&params_sha256);

    let mut files: Vec<ArtifactFile> = Vec::with_capacity(request.files.len() + 1);
    files.push(ArtifactFile::new(
        PARAMS_FILE,
        FileRole::Params,
        None,
        request.params_text.into_bytes(),
    ));
    files.extend(request.files);
    if let Some(plan) = &request.plan {
        files.push(ArtifactFile::new(
            plan.path.clone(),
            FileRole::Plan,
            None,
            plan.bytes.clone(),
        ));
    }
    // 路径安全 + 去重 + 排序（manifest 的 files[] 固定按 path 升序）
    for file in &files {
        validate_relative_path(&file.path)?;
        if file.path == MANIFEST_FILE {
            return Err(format!("产物路径不得使用保留名 {}", MANIFEST_FILE));
        }
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    for pair in files.windows(2) {
        if pair[0].path == pair[1].path {
            return Err(format!("产物路径重复: {}", pair[0].path));
        }
    }

    let file_entries: Vec<(String, FileRole, Option<String>, String, u64)> = files
        .iter()
        .map(|file| {
            (
                file.path.clone(),
                file.role,
                file.scene.clone(),
                file.sha256(),
                file.bytes.len() as u64,
            )
        })
        .collect();
    let result = build_result_descriptor(&params_sha256, &file_entries);
    let result_text = canonical_json(&result);
    let result_sha256 = sha256::hex_digest(result_text.as_bytes());

    let plan_json = match &request.plan {
        Some(plan) => Json::Obj(vec![
            ("present".to_string(), Json::Bool(true)),
            ("path".to_string(), Json::str(plan.path.clone())),
            (
                "sha256".to_string(),
                Json::str(sha256::hex_digest(&plan.bytes)),
            ),
            ("bytes".to_string(), Json::Num(plan.bytes.len() as f64)),
            ("format".to_string(), Json::str(plan.format.clone())),
            ("runtime_abi".to_string(), Json::Bool(plan.runtime_abi)),
        ]),
        None => Json::Obj(vec![
            ("present".to_string(), Json::Bool(false)),
            (
                "reason".to_string(),
                Json::str(request.plan_absent_reason.to_string()),
            ),
            ("expected_format".to_string(), Json::str(PLAN_SCHEMA)),
            ("runtime_abi".to_string(), Json::Bool(false)),
        ]),
    };

    let manifest_files: Vec<Json> = file_entries
        .iter()
        .map(|(path, role, _, hash, bytes)| {
            Json::Obj(vec![
                ("path".to_string(), Json::str(path.clone())),
                ("role".to_string(), Json::str(role.as_str())),
                ("sha256".to_string(), Json::str(hash.clone())),
                ("bytes".to_string(), Json::Num(*bytes as f64)),
            ])
        })
        .collect();

    let manifest = Json::Obj(vec![
        ("schema".to_string(), Json::str(MANIFEST_SCHEMA)),
        (
            "tool".to_string(),
            Json::Obj(vec![
                ("name".to_string(), Json::str(request.tool_name)),
                ("version".to_string(), Json::str(request.tool_version)),
            ]),
        ),
        ("run_id".to_string(), Json::str(run_id.clone())),
        (
            "params_sha256".to_string(),
            Json::str(params_sha256.clone()),
        ),
        (
            "result_sha256".to_string(),
            Json::str(result_sha256.clone()),
        ),
        ("plan".to_string(), plan_json),
        ("result".to_string(), result),
        ("files".to_string(), Json::Arr(manifest_files)),
        (
            "determinism".to_string(),
            Json::str(
                "no timestamp / hostname / absolute path / pid / random number is recorded; \
                 identical inputs and parameters produce identical sha256 values",
            ),
        ),
    ]);
    let manifest_text = canonical_json(&manifest);

    // ---- 落盘（manifest 最后写：没写完的目录没有 manifest，verify 会报缺失）----
    prepare_run_dir(request.dir)?;
    for file in &files {
        write_bytes(&request.dir.join(&file.path), &file.bytes)?;
    }
    let manifest_path = request.dir.join(MANIFEST_FILE);
    write_bytes(&manifest_path, manifest_text.as_bytes())?;

    Ok(RunWriteOutcome {
        dir: request.dir.to_path_buf(),
        run_id,
        params_sha256,
        result_sha256,
        file_count: files.len(),
        manifest_path,
    })
}

fn build_result_descriptor(
    params_sha256: &str,
    entries: &[(String, FileRole, Option<String>, String, u64)],
) -> Json {
    let mut reports: Vec<Json> = Vec::new();
    let mut plans: Vec<Json> = Vec::new();
    let mut aux: Vec<Json> = Vec::new();
    for (path, role, scene, hash, _bytes) in entries {
        let entry = |with_scene: bool| {
            let mut fields = Vec::new();
            if with_scene {
                fields.push(("scene".to_string(), json_opt_str(scene.clone())));
            }
            fields.push(("path".to_string(), Json::str(path.clone())));
            fields.push(("sha256".to_string(), Json::str(hash.clone())));
            Json::Obj(fields)
        };
        match role {
            FileRole::Params => {}
            FileRole::Report => reports.push(Json::Obj(vec![
                (
                    "scene".to_string(),
                    Json::str(scene.clone().unwrap_or_default()),
                ),
                ("path".to_string(), Json::str(path.clone())),
                ("sha256".to_string(), Json::str(hash.clone())),
            ])),
            FileRole::Plan => plans.push(entry(true)),
            FileRole::Aux => aux.push(entry(true)),
        }
    }
    // files 已按 path 升序；这里再按 (scene, path) 排一次，
    // 保证顺序与文件枚举方式无关（只由内容决定）。
    let sort_by_scene_path = |items: &mut Vec<Json>| {
        items.sort_by(|left, right| {
            let key = |value: &Json| -> (String, String) {
                let scene = match obj_get(value, "scene") {
                    Some(Json::Str(text)) => text.clone(),
                    _ => String::new(),
                };
                let path = match obj_get(value, "path") {
                    Some(Json::Str(text)) => text.clone(),
                    _ => String::new(),
                };
                (scene, path)
            };
            key(left).cmp(&key(right))
        });
    };
    sort_by_scene_path(&mut reports);
    sort_by_scene_path(&mut plans);
    sort_by_scene_path(&mut aux);
    Json::Obj(vec![
        ("schema".to_string(), Json::str(RESULT_SCHEMA)),
        ("params_sha256".to_string(), Json::str(params_sha256)),
        ("reports".to_string(), Json::Arr(reports)),
        ("plans".to_string(), Json::Arr(plans)),
        ("aux".to_string(), Json::Arr(aux)),
    ])
}

fn prepare_run_dir(dir: &Path) -> Result<(), String> {
    if dir.exists() {
        if !dir.is_dir() {
            return Err(format!("产物路径已存在且不是目录: {}", dir.display()));
        }
        if dir.join(MANIFEST_FILE).is_file() {
            std::fs::remove_dir_all(dir)
                .map_err(|error| format!("清理旧产物目录 {} 失败: {}", dir.display(), error))?;
        } else {
            let mut entries = std::fs::read_dir(dir)
                .map_err(|error| format!("读取目录 {} 失败: {}", dir.display(), error))?;
            if entries.next().is_some() {
                return Err(format!(
                    "目录 {} 已存在且不是本工具产物（无 {}），拒绝覆盖",
                    dir.display(),
                    MANIFEST_FILE
                ));
            }
        }
    }
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("创建目录 {} 失败: {}", dir.display(), error))
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建目录 {} 失败: {}", parent.display(), error))?;
    }
    std::fs::write(path, bytes).map_err(|error| format!("写 {} 失败: {}", path.display(), error))
}

// ===========================================================================
// 校验产物（--verify）
// ===========================================================================

/// 单条校验结果。
#[derive(Clone, Debug)]
pub struct VerifyCheck {
    pub subject: String,
    pub ok: bool,
    pub detail: String,
}

/// --verify <dir> 的结果。
#[derive(Clone, Debug, Default)]
pub struct VerifyOutcome {
    pub checks: Vec<VerifyCheck>,
    pub errors: Vec<String>,
    pub params_sha256: Option<String>,
    pub result_sha256: Option<String>,
    pub file_count: usize,
}

impl VerifyOutcome {
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }

    fn check(&mut self, subject: impl Into<String>, ok: bool, detail: impl Into<String>) {
        self.checks.push(VerifyCheck {
            subject: subject.into(),
            ok,
            detail: detail.into(),
        });
    }

    fn fail(&mut self, subject: impl Into<String>, detail: impl Into<String>) {
        let subject = subject.into();
        let detail = detail.into();
        self.errors.push(format!("{}: {}", subject, detail));
        self.check(subject, false, detail);
    }
}

#[derive(Clone, Debug)]
struct DeclaredFile {
    path: String,
    role: Option<FileRole>,
    sha256: String,
    bytes: u64,
}

/// 校验一个产物目录：manifest 完整性 + 每个文件字节/sha256 + 结果描述自洽。
pub fn verify_run(dir: &Path) -> VerifyOutcome {
    let mut outcome = VerifyOutcome::default();
    let manifest_path = dir.join(MANIFEST_FILE);
    let manifest_text = match std::fs::read_to_string(&manifest_path) {
        Ok(text) => text,
        Err(error) => {
            outcome.fail(
                MANIFEST_FILE,
                format!("读取 {} 失败: {}", manifest_path.display(), error),
            );
            return outcome;
        }
    };
    let manifest = match parse_json(&manifest_text) {
        Ok(value) => value,
        Err(error) => {
            outcome.fail(MANIFEST_FILE, error);
            return outcome;
        }
    };
    let schema = match obj_get(&manifest, "schema") {
        Some(value) => match as_str(value, "schema") {
            Ok(text) => text.to_string(),
            Err(error) => {
                outcome.fail("manifest.schema", error);
                return outcome;
            }
        },
        None => {
            outcome.fail("manifest.schema", "缺少 schema 字段");
            return outcome;
        }
    };
    if schema != MANIFEST_SCHEMA {
        outcome.fail(
            "manifest.schema",
            format!("期望 {}，实际 {}", MANIFEST_SCHEMA, schema),
        );
        return outcome;
    }
    outcome.check("manifest.schema", true, schema.clone());

    // ---- files[] ----
    let declared = match parse_declared_files(&manifest) {
        Ok(files) => files,
        Err(error) => {
            outcome.fail("manifest.files", error);
            return outcome;
        }
    };
    if declared.is_empty() {
        outcome.fail("manifest.files", "files[] 为空");
        return outcome;
    }
    for file in &declared {
        if file.role.is_none() {
            outcome.fail(&file.path, "files[] 条目缺少 role");
        }
    }
    outcome.file_count = declared.len();
    outcome.check(
        "manifest.files",
        true,
        format!("{} 个文件声明", declared.len()),
    );

    // 实际文件集合（排除 manifest.json 自身）
    let actual = match actual_files(dir, MANIFEST_FILE) {
        Ok(files) => files,
        Err(error) => {
            outcome.fail("目录", error);
            return outcome;
        }
    };
    let declared_paths: Vec<&str> = declared.iter().map(|file| file.path.as_str()).collect();
    for path in &actual {
        if !declared_paths.contains(&path.as_str()) {
            outcome.fail(path, "目录中存在未在 manifest 声明的文件（篡改或残留）");
        }
    }

    // ---- 逐文件校验 ----
    let mut actual_hashes: BTreeMap<String, String> = BTreeMap::new();
    for file in &declared {
        let path = dir.join(&file.path);
        match std::fs::read(&path) {
            Ok(bytes) => {
                let hash = sha256::hex_digest(&bytes);
                if bytes.len() as u64 != file.bytes {
                    outcome.fail(
                        &file.path,
                        format!("字节数不符：manifest {}，实际 {}", file.bytes, bytes.len()),
                    );
                    continue;
                }
                if hash != file.sha256 {
                    outcome.fail(
                        &file.path,
                        format!("sha256 不符：manifest {}，实际 {}", file.sha256, hash),
                    );
                    continue;
                }
                outcome.check(
                    file.path.clone(),
                    true,
                    format!("{} B sha256={}", bytes.len(), hash),
                );
                actual_hashes.insert(file.path.clone(), hash);
            }
            Err(error) => {
                if std::fs::metadata(&path).is_err() {
                    outcome.fail(&file.path, "manifest 声明但目录中不存在");
                } else {
                    outcome.fail(&file.path, format!("读取失败: {}", error));
                }
            }
        }
    }

    // ---- params_sha256 ----
    let declared_params_sha = match obj_get(&manifest, "params_sha256") {
        Some(value) => match as_str(value, "params_sha256") {
            Ok(text) => text.to_string(),
            Err(error) => {
                outcome.fail("manifest.params_sha256", error);
                String::new()
            }
        },
        None => {
            outcome.fail("manifest.params_sha256", "缺少 params_sha256");
            String::new()
        }
    };
    match declared.iter().find(|file| file.path == PARAMS_FILE) {
        Some(entry) => {
            if entry.role != Some(FileRole::Params) {
                outcome.fail(PARAMS_FILE, "files[] 中 params.json 的 role 必须是 params");
            }
            if !declared_params_sha.is_empty() && declared_params_sha != entry.sha256 {
                outcome.fail(
                    "manifest.params_sha256",
                    format!(
                        "与 files[] 中 params.json 的 sha256 不一致（{} vs {}）",
                        declared_params_sha, entry.sha256
                    ),
                );
            }
            match actual_hashes.get(PARAMS_FILE) {
                Some(hash) => {
                    if !declared_params_sha.is_empty() && declared_params_sha != *hash {
                        outcome.fail(
                            "manifest.params_sha256",
                            format!(
                                "与实际 params.json sha256 不一致（{} vs {}）",
                                declared_params_sha, hash
                            ),
                        );
                    }
                    outcome.params_sha256 = Some(hash.clone());
                }
                None => outcome.fail(PARAMS_FILE, "params.json 校验失败，无法确认 params_sha256"),
            }
        }
        None => outcome.fail(PARAMS_FILE, "files[] 中缺少 params.json"),
    }

    // ---- result 描述 + result_sha256 ----
    let declared_result_sha = match obj_get(&manifest, "result_sha256") {
        Some(value) => match as_str(value, "result_sha256") {
            Ok(text) => text.to_string(),
            Err(error) => {
                outcome.fail("manifest.result_sha256", error);
                String::new()
            }
        },
        None => {
            outcome.fail("manifest.result_sha256", "缺少 result_sha256");
            String::new()
        }
    };
    match obj_get(&manifest, "result") {
        None => outcome.fail("manifest.result", "缺少 result 描述"),
        Some(result) => match verify_result_descriptor(result, &declared, &actual_hashes) {
            Ok(()) => {
                outcome.check("manifest.result", true, "结果描述与 files[]/实际内容一致");
                if !declared_result_sha.is_empty() {
                    let recomputed = sha256::hex_digest(canonical_json(result).as_bytes());
                    if recomputed != declared_result_sha {
                        outcome.fail(
                            "manifest.result_sha256",
                            format!(
                                "重算不符：manifest {}，重算 {}",
                                declared_result_sha, recomputed
                            ),
                        );
                    } else {
                        outcome.check(
                            "manifest.result_sha256",
                            true,
                            format!("重算一致 {}", recomputed),
                        );
                        outcome.result_sha256 = Some(recomputed);
                    }
                }
            }
            Err(error) => outcome.fail("manifest.result", error),
        },
    }

    outcome
}

fn parse_declared_files(manifest: &Json) -> Result<Vec<DeclaredFile>, String> {
    let items = as_arr(obj_get(manifest, "files").ok_or("缺少 files[]")?, "files")?;
    let mut declared: Vec<DeclaredFile> = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let context = format!("files[{}]", index);
        let path = as_str(
            obj_get(item, "path").ok_or_else(|| format!("{} 缺少 path", context))?,
            &format!("{}.path", context),
        )?
        .to_string();
        validate_relative_path(&path)?;
        if declared.iter().any(|file| file.path == path) {
            return Err(format!("files[] 中路径重复: {}", path));
        }
        let role = match obj_get(item, "role") {
            Some(value) => {
                let text = as_str(value, &format!("{}.role", context))?;
                Some(FileRole::parse(text).ok_or_else(|| {
                    format!(
                        "{}.role 非法 {:?}（支持 params|report|plan|aux）",
                        context, text
                    )
                })?)
            }
            None => None,
        };
        let hash = as_str(
            obj_get(item, "sha256").ok_or_else(|| format!("{} 缺少 sha256", context))?,
            &format!("{}.sha256", context),
        )?
        .to_string();
        if !is_sha256_hex(&hash) {
            return Err(format!(
                "{}.sha256 不是 64 位小写十六进制: {:?}",
                context, hash
            ));
        }
        let bytes = as_u64(
            obj_get(item, "bytes").ok_or_else(|| format!("{} 缺少 bytes", context))?,
            &format!("{}.bytes", context),
        )?;
        declared.push(DeclaredFile {
            path,
            role,
            sha256: hash,
            bytes,
        });
    }
    Ok(declared)
}

fn verify_result_descriptor(
    result: &Json,
    declared: &[DeclaredFile],
    actual_hashes: &BTreeMap<String, String>,
) -> Result<(), String> {
    let schema = as_str(
        obj_get(result, "schema").ok_or("result 缺少 schema")?,
        "result.schema",
    )?;
    if schema != RESULT_SCHEMA {
        return Err(format!(
            "result.schema 期望 {}，实际 {}",
            RESULT_SCHEMA, schema
        ));
    }
    let params_sha = as_str(
        obj_get(result, "params_sha256").ok_or("result 缺少 params_sha256")?,
        "result.params_sha256",
    )?;
    let params_entry = declared
        .iter()
        .find(|file| file.path == PARAMS_FILE)
        .ok_or("files[] 中缺少 params.json")?;
    if params_sha != params_entry.sha256 {
        return Err("result.params_sha256 与 files[] 中 params.json sha256 不一致".to_string());
    }
    // reports 必须存在且非空
    let reports = as_arr(
        obj_get(result, "reports").ok_or("result 缺少 reports")?,
        "result.reports",
    )?;
    if reports.is_empty() {
        return Err("result.reports 为空（没有任何报告被 result_sha256 绑定）".to_string());
    }
    let mut report_paths: Vec<String> = Vec::new();
    for (index, item) in reports.iter().enumerate() {
        report_paths.push(verify_result_entry(
            item,
            &format!("result.reports[{}]", index),
            declared,
            actual_hashes,
            FileRole::Report,
        )?);
    }
    let mut plan_paths =
        verify_optional_entries(result, "plans", declared, actual_hashes, FileRole::Plan)?;
    let mut aux_paths =
        verify_optional_entries(result, "aux", declared, actual_hashes, FileRole::Aux)?;
    report_paths.sort();
    plan_paths.sort();
    aux_paths.sort();

    // 反向检查：每个 report/plan/aux 角色文件都必须被 result 描述绑定
    for file in declared {
        match file.role {
            Some(FileRole::Report) => {
                if !report_paths.contains(&file.path) {
                    return Err(format!("{} 未出现在 result.reports 中", file.path));
                }
            }
            Some(FileRole::Plan) => {
                if !plan_paths.contains(&file.path) {
                    return Err(format!("{} 未出现在 result.plans 中", file.path));
                }
            }
            Some(FileRole::Aux) => {
                if !aux_paths.contains(&file.path) {
                    return Err(format!("{} 未出现在 result.aux 中", file.path));
                }
            }
            Some(FileRole::Params) | None => {}
        }
    }
    Ok(())
}

fn verify_optional_entries(
    result: &Json,
    key: &str,
    declared: &[DeclaredFile],
    actual_hashes: &BTreeMap<String, String>,
    role: FileRole,
) -> Result<Vec<String>, String> {
    let items = match obj_get(result, key) {
        Some(value) => as_arr(value, &format!("result.{}", key))?,
        None => return Ok(Vec::new()),
    };
    let mut paths = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        paths.push(verify_result_entry(
            item,
            &format!("result.{}[{}]", key, index),
            declared,
            actual_hashes,
            role,
        )?);
    }
    Ok(paths)
}

fn verify_result_entry(
    item: &Json,
    context: &str,
    declared: &[DeclaredFile],
    actual_hashes: &BTreeMap<String, String>,
    expected_role: FileRole,
) -> Result<String, String> {
    let path = as_str(
        obj_get(item, "path").ok_or_else(|| format!("{} 缺少 path", context))?,
        &format!("{}.path", context),
    )?
    .to_string();
    validate_relative_path(&path)?;
    let hash = as_str(
        obj_get(item, "sha256").ok_or_else(|| format!("{} 缺少 sha256", context))?,
        &format!("{}.sha256", context),
    )?
    .to_string();
    if !is_sha256_hex(&hash) {
        return Err(format!("{}.sha256 不是 64 位小写十六进制", context));
    }
    let declared_entry = declared
        .iter()
        .find(|file| file.path == path)
        .ok_or_else(|| format!("{} 引用的 {} 不在 files[] 中", context, path))?;
    if declared_entry.role != Some(expected_role) {
        return Err(format!(
            "{} 引用的 {} 角色不符（期望 {}）",
            context,
            path,
            expected_role.as_str()
        ));
    }
    if declared_entry.sha256 != hash {
        return Err(format!(
            "{} 的 sha256 与 files[] 不一致（{} vs {}）",
            context, hash, declared_entry.sha256
        ));
    }
    match actual_hashes.get(&path) {
        Some(actual) => {
            if *actual != hash {
                return Err(format!(
                    "{} 的实际内容 sha256 与 result 描述不一致（{} vs {}）",
                    context, hash, actual
                ));
            }
        }
        None => {
            return Err(format!(
                "{} 引用的 {} 未通过文件校验，无法绑定进 result_sha256",
                context, path
            ));
        }
    }
    Ok(path)
}

// ===========================================================================
// 单元测试
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_params() -> String {
        canonical_json(&Json::obj(vec![
            ("z", Json::Num(1.5)),
            ("a", Json::Arr(vec![Json::Str("x".to_string()), Json::Null])),
            (
                "m",
                Json::obj(vec![("b", Json::Bool(true)), ("a", Json::Num(-2.0))]),
            ),
        ]))
    }

    #[test]
    fn canonical_json_sorts_keys_and_indents() {
        let text = sample_params();
        assert_eq!(
            text,
            "{\n  \"a\": [\n    \"x\",\n    null\n  ],\n  \"m\": {\n    \"a\": -2,\n    \"b\": true\n  },\n  \"z\": 1.5\n}\n"
        );
        // 幂等：重排对象插入顺序后输出逐字节相同
        let reordered = Json::obj(vec![
            (
                "m",
                Json::obj(vec![("a", Json::Num(-2.0)), ("b", Json::Bool(true))]),
            ),
            ("a", Json::Arr(vec![Json::Str("x".to_string()), Json::Null])),
            ("z", Json::Num(1.5)),
        ]);
        assert_eq!(canonical_json(&reordered), text);
    }

    #[test]
    fn json_parser_roundtrips_canonical_output() {
        let text = sample_params();
        let parsed = parse_json(&text).expect("parse");
        assert_eq!(canonical_json(&parsed), text);
        // 字符串转义 / Unicode / 代理对 / 数字形式
        let value = parse_json(r#"{"s":"a\"b\\c\n\t\u4e2d\ud83d\ude00","n":-1.5e-7,"t":true}"#)
            .expect("parse");
        let expected_number = format!("{}", -1.5e-7);
        assert_eq!(
            canonical_json(&value),
            format!(
                "{{\n  \"n\": {},\n  \"s\": \"a\\\"b\\\\c\\n\\t中😀\",\n  \"t\": true\n}}\n",
                expected_number
            )
        );
        // 幂等再解析
        assert_eq!(
            canonical_json(&parse_json(&canonical_json(&value)).unwrap()),
            canonical_json(&value)
        );
    }

    #[test]
    fn json_parser_rejects_malformed_input() {
        for bad in [
            "",
            "nul",
            "{}x",
            "{\"a\":1,}",
            "[1,]",
            "{\"a\" 1}",
            "{\"a\":1,\"a\":2}",
            "01",
            "\"unterminated",
            "\"bad\\q\"",
            "{\"a\":[1,2}",
        ] {
            assert!(parse_json(bad).is_err(), "应拒绝非法 JSON: {:?}", bad);
        }
    }

    #[test]
    fn relative_path_validation() {
        for good in ["report.json", "fir/SUB1.csv", "scenes/s1/report.json"] {
            validate_relative_path(good).unwrap_or_else(|error| panic!("{}: {}", good, error));
        }
        for bad in [
            "",
            "/abs/report.json",
            "C:/x.json",
            "a\\b.json",
            "../x.json",
            "a//b.json",
            "./a.json",
            "a/../b.json",
        ] {
            assert!(
                validate_relative_path(bad).is_err(),
                "应拒绝路径: {:?}",
                bad
            );
        }
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "khaen-mimo-artifact-{tag}-{}-{stamp}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn sample_request(dir: &Path) -> RunWriteRequest<'_> {
        let report = canonical_json(&Json::obj(vec![
            ("scene_id", Json::Str("scene-x".to_string())),
            ("value", Json::Num(3.25)),
        ]));
        RunWriteRequest {
            dir,
            tool_name: "khaen-mimo-opt",
            tool_version: "0.0.0-test",
            params_text: sample_params(),
            files: vec![
                ArtifactFile::new(
                    "report.json",
                    FileRole::Report,
                    Some("scene-x".to_string()),
                    report.into_bytes(),
                ),
                ArtifactFile::new(
                    "fir/SUB1.csv",
                    FileRole::Plan,
                    Some("scene-x".to_string()),
                    b"# source=SUB1\ntaps=2\n0.5\n".to_vec(),
                ),
                ArtifactFile::new(
                    "metrics.csv",
                    FileRole::Aux,
                    Some("scene-x".to_string()),
                    b"scene,variant\nscene-x,B0\n".to_vec(),
                ),
            ],
            plan: None,
            plan_absent_reason: "unit test: no binary plan on this path",
        }
    }

    #[test]
    fn write_then_verify_roundtrip() {
        let dir = scratch_dir("roundtrip");
        let outcome = write_run(sample_request(&dir)).expect("write");
        assert_eq!(outcome.file_count, 4);
        assert!(outcome.manifest_path.is_file());
        assert_eq!(outcome.params_sha256.len(), 64);
        assert_eq!(outcome.result_sha256.len(), 64);

        let verify = verify_run(&dir);
        assert!(verify.ok(), "errors={:?}", verify.errors);
        assert_eq!(verify.file_count, 4);
        assert_eq!(
            verify.params_sha256.as_deref(),
            Some(outcome.params_sha256.as_str())
        );
        assert_eq!(
            verify.result_sha256.as_deref(),
            Some(outcome.result_sha256.as_str())
        );

        // 同一内容重写一次（同目录重建）→ manifest 字节逐字节相同
        let manifest_before = std::fs::read(&outcome.manifest_path).unwrap();
        let again = write_run(sample_request(&dir)).expect("rewrite");
        assert_eq!(again.params_sha256, outcome.params_sha256);
        assert_eq!(again.result_sha256, outcome.result_sha256);
        assert_eq!(
            std::fs::read(dir.join(MANIFEST_FILE)).unwrap(),
            manifest_before
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_detects_tampered_report() {
        let dir = scratch_dir("tamper-report");
        write_run(sample_request(&dir)).expect("write");
        let path = dir.join("report.json");
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[0] = b'[';
        std::fs::write(&path, bytes).unwrap();
        let verify = verify_run(&dir);
        assert!(!verify.ok());
        assert!(
            verify
                .errors
                .iter()
                .any(|error| error.contains("report.json")),
            "errors={:?}",
            verify.errors
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_detects_tampered_manifest_hash_and_extra_file() {
        let dir = scratch_dir("tamper-manifest");
        write_run(sample_request(&dir)).expect("write");
        let report_sha = sha256::hex_digest(&std::fs::read(dir.join("report.json")).unwrap());
        let manifest_path = dir.join(MANIFEST_FILE);
        let text = std::fs::read_to_string(&manifest_path).unwrap();
        let tampered = text.replacen(&report_sha, &"0".repeat(64), 1);
        assert_ne!(tampered, text);
        std::fs::write(&manifest_path, tampered).unwrap();
        let verify = verify_run(&dir);
        assert!(!verify.ok());
        assert!(
            verify
                .errors
                .iter()
                .any(|error| error.contains("report.json")),
            "errors={:?}",
            verify.errors
        );
        let _ = std::fs::remove_dir_all(&dir);

        // 多余文件（不在 manifest 里）
        let dir = scratch_dir("extra-file");
        write_run(sample_request(&dir)).expect("write");
        std::fs::write(dir.join("extra.txt"), b"x").unwrap();
        let verify = verify_run(&dir);
        assert!(!verify.ok());
        assert!(
            verify
                .errors
                .iter()
                .any(|error| error.contains("extra.txt")),
            "errors={:?}",
            verify.errors
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_detects_result_hash_tamper() {
        let dir = scratch_dir("tamper-result");
        let outcome = write_run(sample_request(&dir)).expect("write");
        let manifest_path = dir.join(MANIFEST_FILE);
        let text = std::fs::read_to_string(&manifest_path).unwrap();
        let tampered = text.replace(
            &format!("\"result_sha256\": \"{}\"", outcome.result_sha256),
            "\"result_sha256\": \"0000000000000000000000000000000000000000000000000000000000000000\"",
        );
        assert_ne!(tampered, text, "manifest 里应存在 result_sha256");
        std::fs::write(&manifest_path, tampered).unwrap();
        let verify = verify_run(&dir);
        assert!(!verify.ok());
        assert!(
            verify
                .errors
                .iter()
                .any(|error| error.contains("result_sha256")),
            "errors={:?}",
            verify.errors
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_refuses_to_clobber_foreign_directory() {
        let dir = scratch_dir("foreign-dir");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("keep-me.txt"), b"user data").unwrap();
        let error = write_run(sample_request(&dir)).expect_err("must refuse");
        assert!(error.contains("拒绝覆盖"), "error={}", error);
        assert!(dir.join("keep-me.txt").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
