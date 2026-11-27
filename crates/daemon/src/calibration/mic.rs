//! 麦克风 profile 与 UMIK 校准文件（真实校准桥接 C1）。
//!
//! # 范围
//!
//! 只做 daemon 侧的 **MicProfile 持久化 + UMIK txt 校验/存储**，不改 calibration job
//! backend/service 语义。实时 UMIK 状态由 `calibration::api::get_mic_status` 经 host
//! link 的 `cal_status`（C3a）读取；本模块不碰 rt-host、也从不编造实时采集数据。
//!
//! # 存储（全部在 daemon 状态目录下）
//!
//! | 路径 | 内容 |
//! |---|---|
//! | `<state_dir>/calibration/mic.json` | 当前 MicProfile（schema [`MIC_PROFILE_SCHEMA`]，0600 原子写） |
//! | `<state_dir>/calibration/mics/<sha256>.txt` | 上传的 UMIK 校准文件本体（内容寻址，0600） |
//! | `<state_dir>/calibration/mics/<sha256>.json` | 上传回执（原始文件名/字节数/上传时刻，0600） |
//!
//! 磁盘文件名只由字节 sha256 决定（[`sha256_hex`]），客户端给的 `X-KHAEN-FILENAME`
//! 只写入回执和 `MicProfile.cal_filename`，**不参与路径拼接**；PUT 时复核
//! `<sha>.txt` 的字节 sha256 仍等于文件名（内容寻址自校验）。
//!
//! # UMIK txt（保守只接受真实 miniDSP 格式）
//!
//! 真实 UMIK-1 文件（仓库外 `calibration-spec/mics/umik1_718-7696_90deg.txt`）形如：
//!
//! ```text
//! "Sens Factor =-0.0109999dB, SERNO: 7187696"
//! "Auto-generated 90-degree calibration file"
//! 10.054    -4.7795
//! ...
//! ```
//!
//! 接受规则：≤1 MiB、UTF-8/ASCII 无 BOM、**首行必须声明 SERNO**（`SERNO:` / `SERNO ` /
//! 引号包住均可；首行可带 `Sens Factor=<f64>dB` 写入 `parsed.sens_factor_db`，缺失为 null，
//! 不猜）；数据前允许空行与注释行（`"` / `#` / `;` / `*` / `'` 开头——真实文件第 2 行
//! 就是引号注释）；数据行必须恰好两列可解析 f64，频率有限且 >0、灵敏度有限；
//! ≥1 点、≤[`MAX_CAL_POINTS`] 点。
//!
//! 解析失败一律 [`code::MIC_INVALID`]（400，details.field 指向坏行/文件），超限用
//! [`code::MIC_TOO_LARGE`]（413）；不静默截断、不跳过坏行、不把注释当数据。
//!
//! # MicProfile（`khaen-mic-profile/v1`，HANDOFF REPORT §4）
//!
//! 字段 `device / serial / orientation / cal_sha256 / cal_filename / cal_path / parsed / updated_at`。
//! PUT 是部分更新：首次保存必须给全 `device`/`orientation`/`cal_sha256`（`serial` 可省略，
//! 按文件 SERNO 填；给了就必须与 SERNO 完全一致，否则 [`code::MIC_SERIAL_MISMATCH`]）。

use std::path::{Path, PathBuf};

use axum::http::{HeaderMap, StatusCode};
use serde_json::{json, Map, Value};

use khaen_core::sha256::sha256_hex;

use super::service::CalibrationError;
use super::store::{is_sha256_hex, is_upload_id, write_atomic};
use super::wizard::now_rfc3339;

/// 状态目录下的校准子目录。
pub const MIC_DIR: &str = "calibration";
/// profile 文件名（`<state_dir>/calibration/mic.json`）。
pub const MIC_PROFILE_FILE: &str = "mic.json";
/// 校准文件内容寻址子目录。
pub const MIC_CAL_SUBDIR: &str = "mics";
/// MicProfile schema（REST 与磁盘同名）。
pub const MIC_PROFILE_SCHEMA: &str = "khaen-mic-profile/v1";
/// 校准文件上传回执 schema（内部磁盘格式，不经 REST 暴露）。
pub const MIC_CAL_META_SCHEMA: &str = "khaen-mic-calibration/v1";
/// 校准文件大小上限：1 MiB（不是上传 WAV 的 64 MiB）。
pub const MAX_CAL_BYTES: u64 = 1024 * 1024;
/// 校准文件最多数据点数（防坏文件塞爆内存/响应）。
pub const MAX_CAL_POINTS: usize = 100_000;
/// UMIK 朝向（miniDSP 惯例：0° / 90°）。
pub const MIC_ORIENTATIONS: [&str; 2] = ["0deg", "90deg"];
/// 默认校准文件名（请求没给 `X-KHAEN-FILENAME` 时）。
pub const DEFAULT_CAL_FILENAME: &str = "umik.txt";
/// 校准文件名最大长度。
pub const MAX_CAL_FILENAME_LEN: usize = 128;
/// ALSA 设备名最大长度。
pub const MAX_DEVICE_LEN: usize = 128;
/// 序列号最大长度。
pub const MAX_SERIAL_LEN: usize = 64;
/// mic.json / 校准文件的 Unix 权限位（0600，与 auth.json 同档）。
pub const MIC_FILE_MODE: u32 = 0o600;
/// GET 无 profile 时的固定原因（200 + profile:null + 此原因）。
pub const NO_PROFILE_REASON: &str = "no microphone profile saved yet: PUT /api/calibration/mic";

/// 稳定错误码（M1 错误体 `{error:{code,message,details}}`）。
pub mod code {
    /// 校准文件/请求字段非法（400）。
    pub const MIC_INVALID: &str = "calibration_mic_invalid";
    /// 校准文件超过大小/点数上限（413）。
    pub const MIC_TOO_LARGE: &str = "calibration_mic_too_large";
    /// PUT 含未知字段（400）。
    pub const MIC_UNKNOWN_FIELD: &str = "calibration_mic_unknown_field";
    /// PUT 引用的 cal_sha256 没有已上传文件（400）。
    pub const MIC_NOT_FOUND: &str = "calibration_mic_not_found";
    /// PUT 的 serial 与文件 SERNO 不一致（400）。
    pub const MIC_SERIAL_MISMATCH: &str = "calibration_mic_serial_mismatch";
    /// PUT 的 orientation 不是 0deg/90deg（400）。
    pub const MIC_ORIENTATION_INVALID: &str = "calibration_mic_orientation_invalid";
}

// ---------------------------------------------------------------------------
// 错误构造（M1 错误体 + 稳定码，见模块 docs）
// ---------------------------------------------------------------------------

/// 400 `calibration_mic_invalid`。
pub fn invalid(message: impl Into<String>) -> CalibrationError {
    CalibrationError::new(StatusCode::BAD_REQUEST, code::MIC_INVALID, message.into())
}

/// 400 `calibration_mic_invalid` + details.field。
pub fn invalid_field(field: &str, message: impl Into<String>) -> CalibrationError {
    invalid(message).with_detail("field", json!(field))
}

/// 413 `calibration_mic_too_large`（字节上限，details.limit_bytes）。
pub fn too_large_bytes() -> CalibrationError {
    CalibrationError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        code::MIC_TOO_LARGE,
        format!("calibration file exceeds the {MAX_CAL_BYTES}-byte limit"),
    )
    .with_detail("limit_bytes", json!(MAX_CAL_BYTES))
}

/// 413 `calibration_mic_too_large`（点数上限，details.limit_points）。
fn too_many_points(points: usize) -> CalibrationError {
    CalibrationError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        code::MIC_TOO_LARGE,
        format!(
            "calibration file has more than {MAX_CAL_POINTS} data points (saw at least {points})"
        ),
    )
    .with_detail("limit_points", json!(MAX_CAL_POINTS))
}

/// 400 `calibration_mic_unknown_field`（details.field + details.allowed）。
fn unknown_field(field: &str) -> CalibrationError {
    CalibrationError::new(
        StatusCode::BAD_REQUEST,
        code::MIC_UNKNOWN_FIELD,
        format!(
            "unknown field {field:?}: allowed fields are {}",
            ALLOWED_PROFILE_FIELDS.join(", ")
        ),
    )
    .with_detail("field", json!(field))
    .with_detail("allowed", json!(ALLOWED_PROFILE_FIELDS))
}

/// 400 `calibration_mic_not_found`（PUT 引用的校准文件不存在）。
fn calibration_not_found(sha256: &str) -> CalibrationError {
    CalibrationError::new(
        StatusCode::BAD_REQUEST,
        code::MIC_NOT_FOUND,
        format!(
            "no uploaded calibration file with sha256 {sha256}: POST it to /api/calibration/mic/calibration first"
        ),
    )
    .with_detail("cal_sha256", json!(sha256))
}

/// 400 `calibration_mic_serial_mismatch`（PUT serial 与文件 SERNO 不一致）。
fn serial_mismatch(declared: &str, serno: &str, sha256: &str) -> CalibrationError {
    CalibrationError::new(
        StatusCode::BAD_REQUEST,
        code::MIC_SERIAL_MISMATCH,
        format!(
            "serial {declared:?} does not match the calibration file SERNO {serno:?} (cal_sha256 {sha256})"
        ),
    )
    .with_detail("field", json!("serial"))
    .with_detail("serial", json!(declared))
    .with_detail("serno", json!(serno))
    .with_detail("cal_sha256", json!(sha256))
}

/// 400 `calibration_mic_orientation_invalid`（details.allowed）。
fn orientation_invalid(value: &str) -> CalibrationError {
    CalibrationError::new(
        StatusCode::BAD_REQUEST,
        code::MIC_ORIENTATION_INVALID,
        format!("orientation must be one of {MIC_ORIENTATIONS:?} (UMIK convention), got {value:?}"),
    )
    .with_detail("field", json!("orientation"))
    .with_detail("allowed", json!(MIC_ORIENTATIONS))
}

// ---------------------------------------------------------------------------
// UMIK txt 解析
// ---------------------------------------------------------------------------

/// 服务端解析出的校准文件摘要（REST `parsed` 字段与磁盘格式共用）。
#[derive(Debug, Clone, PartialEq)]
pub struct MicParsed {
    /// 文件首行声明的 SERNO（原样，不做大小写/分隔符归一化）。
    pub serno: String,
    /// 首行 `Sens Factor=<f64>dB`；文件没声明时为 None（不猜）。
    pub sens_factor_db: Option<f64>,
    /// 数据点数（1..=MAX_CAL_POINTS）。
    pub points: usize,
}

impl MicParsed {
    /// REST/磁盘 JSON（字段顺序固定：serno / sens_factor_db / points）。
    pub fn to_json(&self) -> Value {
        json!({
            "serno": self.serno,
            "sens_factor_db": self.sens_factor_db,
            "points": self.points,
        })
    }

    /// 从磁盘 JSON 恢复（严格：类型/范围不对返回原因，由调用方按 corrupt 降级）。
    pub fn from_json(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "parsed must be a JSON object".to_string())?;
        let serno = required_str_field(object, "parsed", "serno")?;
        if !is_valid_serial(serno) {
            return Err(format!(
                "parsed.serno is not a valid UMIK serial: {serno:?}"
            ));
        }
        let serno = serno.to_string();
        let sens_factor_db = match object.get("sens_factor_db") {
            None | Some(Value::Null) => None,
            Some(Value::Number(number)) => match number.as_f64() {
                Some(value) if value.is_finite() => Some(value),
                _ => return Err(SENS_FACTOR_TYPE.to_string()),
            },
            Some(_) => return Err(SENS_FACTOR_TYPE.to_string()),
        };
        let points = match object.get("points").and_then(Value::as_u64) {
            Some(points) => usize::try_from(points).unwrap_or(usize::MAX),
            None => return Err("parsed.points must be a non-negative integer".to_string()),
        };
        if !(1..=MAX_CAL_POINTS).contains(&points) {
            return Err(format!(
                "parsed.points must be in 1..={MAX_CAL_POINTS}, got {points}"
            ));
        }
        Ok(Self {
            serno,
            sens_factor_db,
            points,
        })
    }
}

const SENS_FACTOR_TYPE: &str = "parsed.sens_factor_db must be a finite number or null";

/// 解析一份 UMIK 校准 txt（规则见模块 docs；失败返回 M1 稳定错误）。
pub fn parse_umik_calibration(bytes: &[u8]) -> Result<MicParsed, CalibrationError> {
    if bytes.is_empty() {
        return Err(invalid_field("calibration", "calibration file is empty"));
    }
    if bytes.len() as u64 > MAX_CAL_BYTES {
        return Err(too_large_bytes());
    }
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Err(invalid_field(
            "calibration",
            "calibration file must not start with a UTF-8 BOM",
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|error| {
        invalid_field(
            "calibration",
            format!("calibration file must be UTF-8/ASCII text: {error}"),
        )
    })?;
    if text.contains('\0') {
        return Err(invalid_field(
            "calibration",
            "calibration file must be text, not binary (found a NUL byte)",
        ));
    }
    let mut lines = text.lines().enumerate();
    let Some((_, header)) = lines.next() else {
        return Err(invalid_field("calibration", "calibration file is empty"));
    };
    let header = header.trim();
    if header.is_empty() {
        return Err(invalid_field(
            "calibration",
            "calibration file must start with the SERNO header line (miniDSP UMIK format, e.g. \"Sens Factor =-0.011dB, SERNO: 7187696\")",
        ));
    }
    let (serno, sens_factor_db) = parse_umik_header(header)?;
    let mut points = 0usize;
    for (index, raw_line) in lines {
        let line = raw_line.trim();
        if line.is_empty() || is_comment_line(line) {
            continue;
        }
        let line_number = index + 1;
        let mut columns = line.split_whitespace();
        let Some(frequency) = columns.next() else {
            continue;
        };
        let Some(sensitivity) = columns.next() else {
            return Err(data_line_error(
                line_number,
                "expected two columns (Hz and dB)",
                line,
            ));
        };
        if columns.next().is_some() {
            return Err(data_line_error(
                line_number,
                "expected exactly two columns (Hz and dB)",
                line,
            ));
        }
        let frequency = frequency.parse::<f64>().map_err(|_| {
            data_line_error(line_number, "frequency column is not a number (Hz)", line)
        })?;
        if !frequency.is_finite() || frequency <= 0.0 {
            return Err(data_line_error(
                line_number,
                "frequency must be a finite number greater than 0 Hz",
                line,
            ));
        }
        let sensitivity = sensitivity.parse::<f64>().map_err(|_| {
            data_line_error(line_number, "sensitivity column is not a number (dB)", line)
        })?;
        if !sensitivity.is_finite() {
            return Err(data_line_error(
                line_number,
                "sensitivity must be a finite number (dB)",
                line,
            ));
        }
        points += 1;
        if points > MAX_CAL_POINTS {
            return Err(too_many_points(points));
        }
    }
    if points == 0 {
        return Err(invalid_field(
            "calibration",
            "calibration file has no frequency/sensitivity data points after the SERNO header",
        ));
    }
    Ok(MicParsed {
        serno,
        sens_factor_db,
        points,
    })
}

/// 首行：必须声明 SERNO（大小写不敏感），可选 `Sens Factor=<f64>dB`。
fn parse_umik_header(header: &str) -> Result<(String, Option<f64>), CalibrationError> {
    let lower = header.to_ascii_lowercase();
    let Some(position) = lower.find("serno") else {
        return Err(invalid_field(
            "calibration",
            "first line must declare SERNO (miniDSP UMIK format, e.g. \"Sens Factor =-0.011dB, SERNO: 7187696\")",
        ));
    };
    let rest = &header[position + "serno".len()..];
    let Some(serno) = extract_serial(rest) else {
        return Err(invalid_field(
            "calibration",
            format!("first line declares SERNO but has no serial number value: {header:?}"),
        ));
    };
    if !is_valid_serial(&serno) {
        return Err(invalid_field(
            "calibration",
            format!(
                "SERNO {serno:?} is not a valid serial (1..={MAX_SERIAL_LEN} ASCII letters/digits/'-'/'_')"
            ),
        ));
    }
    let sens_factor_db = parse_sens_factor(&lower, header)?;
    Ok((serno, sens_factor_db))
}

/// 从 `SERNO` 关键字之后的文本里取第一个序列号 token。
fn extract_serial(rest: &str) -> Option<String> {
    let mut serial = String::new();
    for character in rest.chars() {
        if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
            serial.push(character);
        } else if serial.is_empty() {
            if matches!(character, ':' | '=' | ',' | '"' | '\'' | ' ' | '\t') {
                continue;
            }
            return None;
        } else {
            break;
        }
    }
    if serial.is_empty() {
        None
    } else {
        Some(serial)
    }
}

/// 首行可选的 `Sens Factor =<f64>dB`（缺失 → None；声明了但解析不了 → 拒绝）。
fn parse_sens_factor(lower: &str, header: &str) -> Result<Option<f64>, CalibrationError> {
    let Some(position) = lower.find("sens factor") else {
        return Ok(None);
    };
    let rest = header[position + "sens factor".len()..].trim_start();
    let rest = rest
        .strip_prefix('=')
        .or_else(|| rest.strip_prefix(':'))
        .map(str::trim_start)
        .ok_or_else(|| {
            invalid_field(
                "calibration",
                format!("Sens Factor must be followed by '=' and a dB value: {header:?}"),
            )
        })?;
    let token: String = rest
        .chars()
        .take_while(|character| matches!(character, '0'..='9' | '+' | '-' | '.' | 'e' | 'E'))
        .collect();
    if token.is_empty() {
        return Err(invalid_field(
            "calibration",
            format!("Sens Factor must be followed by a dB value: {header:?}"),
        ));
    }
    let value = token.parse::<f64>().map_err(|error| {
        invalid_field(
            "calibration",
            format!("Sens Factor {token:?} is not a number: {error}"),
        )
    })?;
    if !value.is_finite() {
        return Err(invalid_field(
            "calibration",
            format!("Sens Factor must be a finite dB value: {token:?}"),
        ));
    }
    Ok(Some(value))
}

fn data_line_error(line_number: usize, reason: &str, line: &str) -> CalibrationError {
    invalid_field(
        "calibration",
        format!("line {line_number}: {reason}: {:?}", truncate_line(line)),
    )
}

fn truncate_line(line: &str) -> String {
    const MAX_CHARS: usize = 80;
    if line.chars().count() <= MAX_CHARS {
        return line.to_string();
    }
    let truncated: String = line.chars().take(MAX_CHARS).collect();
    format!("{truncated}...")
}

fn is_comment_line(line: &str) -> bool {
    matches!(
        line.chars().next(),
        Some('"') | Some('\'') | Some('#') | Some(';') | Some('*') | Some('!')
    )
}

/// 序列号：1..=64 个 ASCII 字母/数字/`-`/`_`（miniDSP SERNO 实际是数字）。
pub fn is_valid_serial(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SERIAL_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

// ---------------------------------------------------------------------------
// MicProfile 与校准文件记录
// ---------------------------------------------------------------------------

/// 已上传的校准文件（内容寻址，`<state_dir>/calibration/mics/<sha256>.txt`）。
#[derive(Debug, Clone, PartialEq)]
pub struct MicCalRecord {
    /// 文件字节 sha256（= 磁盘文件名）。
    pub sha256: String,
    /// 首次上传时客户端给的文件名（只做展示/回显；不参与路径拼接）。
    pub filename: String,
    /// 文件字节数。
    pub bytes: u64,
    /// 磁盘路径。
    pub path: PathBuf,
    /// 服务端解析结果。
    pub parsed: MicParsed,
}

/// 当前 MicProfile（`<state_dir>/calibration/mic.json`；字段见 HANDOFF REPORT §4）。
#[derive(Debug, Clone, PartialEq)]
pub struct MicProfile {
    /// UMIK 采集设备（ALSA 名，如 `hw:CARD=U18dB,DEV=0`）。
    pub device: String,
    /// UMIK 序列号（必须与校准文件 SERNO 一致）。
    pub serial: String,
    /// 朝向：`0deg` / `90deg`。
    pub orientation: String,
    /// 校准文件字节 sha256。
    pub cal_sha256: String,
    /// 校准文件原始文件名（展示用）。
    pub cal_filename: String,
    /// 校准文件磁盘路径（内容寻址）。
    pub cal_path: String,
    /// 校准文件解析摘要。
    pub parsed: MicParsed,
    /// 保存时刻（RFC3339 UTC）。
    pub updated_at: String,
}

impl MicProfile {
    /// REST/磁盘 JSON（schema 在每个 profile 对象里，与 GET 响应同源）。
    pub fn to_json(&self) -> Value {
        json!({
            "schema": MIC_PROFILE_SCHEMA,
            "device": self.device,
            "serial": self.serial,
            "orientation": self.orientation,
            "cal_sha256": self.cal_sha256,
            "cal_filename": self.cal_filename,
            "cal_path": self.cal_path,
            "parsed": self.parsed.to_json(),
            "updated_at": self.updated_at,
        })
    }

    /// 从磁盘 JSON 恢复（严格：缺字段/类型错/取值越界返回原因，按 corrupt 降级）。
    pub fn from_json(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "profile must be a JSON object".to_string())?;
        let schema = required_str_field(object, "profile", "schema")?;
        if schema != MIC_PROFILE_SCHEMA {
            return Err(format!(
                "unsupported schema {schema:?} (expected {MIC_PROFILE_SCHEMA})"
            ));
        }
        let device = required_str_field(object, "profile", "device")?;
        if device.is_empty() {
            return Err("profile.device must not be empty".to_string());
        }
        let device = device.to_string();
        let serial = required_str_field(object, "profile", "serial")?;
        if !is_valid_serial(serial) {
            return Err(format!(
                "profile.serial is not a valid UMIK serial: {serial:?}"
            ));
        }
        let serial = serial.to_string();
        let orientation = required_str_field(object, "profile", "orientation")?;
        if !MIC_ORIENTATIONS.contains(&orientation) {
            return Err(format!(
                "profile.orientation must be one of {MIC_ORIENTATIONS:?}, got {orientation:?}"
            ));
        }
        let orientation = orientation.to_string();
        let cal_sha256 = required_str_field(object, "profile", "cal_sha256")?;
        if !is_sha256_hex(cal_sha256) {
            return Err(format!(
                "profile.cal_sha256 must be 64 hex characters, got {cal_sha256:?}"
            ));
        }
        let cal_sha256 = cal_sha256.to_ascii_lowercase();
        let cal_filename = required_str_field(object, "profile", "cal_filename")?;
        if !is_safe_cal_filename(cal_filename) {
            return Err(format!(
                "profile.cal_filename is not a bare file name: {cal_filename:?}"
            ));
        }
        let cal_filename = cal_filename.to_string();
        let cal_path = required_str_field(object, "profile", "cal_path")?;
        if cal_path.is_empty() {
            return Err("profile.cal_path must not be empty".to_string());
        }
        let cal_path = cal_path.to_string();
        let parsed = MicParsed::from_json(
            object
                .get("parsed")
                .ok_or_else(|| "profile.parsed is missing".to_string())?,
        )?;
        let updated_at = required_str_field(object, "profile", "updated_at")?;
        if updated_at.is_empty() {
            return Err("profile.updated_at must not be empty".to_string());
        }
        Ok(Self {
            device,
            serial,
            orientation,
            cal_sha256,
            cal_filename,
            cal_path,
            parsed,
            updated_at: updated_at.to_string(),
        })
    }
}

/// 一次 profile 读取的结果（GET/PUT/status 共用；corruption 不冒充已保存）。
#[derive(Debug, Clone, PartialEq)]
pub struct MicProfileLoad {
    /// 已保存的 profile（文件不存在/损坏时为 None）。
    pub profile: Option<MicProfile>,
    /// 缺失/损坏原因（无 profile 时至少一条，GET/status 原样透出）。
    pub reasons: Vec<String>,
    /// 文件存在但不可用时的原因（PUT 用它决定是否允许覆盖修复）。
    pub corruption: Option<String>,
}

impl MicProfileLoad {
    fn missing() -> Self {
        Self {
            profile: None,
            reasons: vec![NO_PROFILE_REASON.to_string()],
            corruption: None,
        }
    }

    fn corrupt(reason: String) -> Self {
        Self {
            profile: None,
            reasons: vec![reason.clone()],
            corruption: Some(reason),
        }
    }
}

fn required_str_field<'a>(
    object: &'a Map<String, Value>,
    path: &str,
    field: &str,
) -> Result<&'a str, String> {
    match object.get(field) {
        Some(Value::String(text)) => Ok(text),
        Some(_) => Err(format!("{path}.{field} must be a string")),
        None => Err(format!("{path}.{field} is missing")),
    }
}

/// 校准文件名是否安全（bare filename；拒绝分隔符/`..`/控制字符/超长）。
pub fn is_safe_cal_filename(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_CAL_FILENAME_LEN
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && !name.chars().any(char::is_control)
}

// ---------------------------------------------------------------------------
// 存储：<state_dir>/calibration/{mic.json,mics/<sha256>.txt}
// ---------------------------------------------------------------------------

/// MicProfile 存储：状态目录下的 `calibration/`。
#[derive(Debug, Clone)]
pub struct MicProfileStore {
    root: PathBuf,
}

impl MicProfileStore {
    /// 状态目录 → `<state_dir>/calibration`。
    pub fn new(state_dir: &Path) -> Self {
        Self {
            root: state_dir.join(MIC_DIR),
        }
    }

    /// 校准存储根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// profile 文件路径（`<root>/mic.json`）。
    pub fn profile_path(&self) -> PathBuf {
        self.root.join(MIC_PROFILE_FILE)
    }

    /// 校准文件目录（`<root>/mics`）。
    pub fn cal_dir(&self) -> PathBuf {
        self.root.join(MIC_CAL_SUBDIR)
    }

    /// 内容寻址的校准文件路径（先校验 sha256 是 64 位小写十六进制，防路径穿越）。
    pub fn calibration_path(&self, sha256: &str) -> Result<PathBuf, CalibrationError> {
        if !is_upload_id(sha256) {
            return Err(invalid_field(
                "cal_sha256",
                format!(
                    "cal_sha256 must be 64 lowercase hex characters (the sha256 of the calibration file), got {sha256:?}"
                ),
            ));
        }
        Ok(self.cal_dir().join(format!("{sha256}.txt")))
    }

    fn calibration_meta_path(&self, sha256: &str) -> PathBuf {
        self.cal_dir().join(format!("{sha256}.json"))
    }

    /// 读取 profile（不存在 → 缺失原因；损坏 → corrupt 原因；引用文件缺失 → reasons 追加）。
    pub fn load(&self) -> MicProfileLoad {
        let path = self.profile_path();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return MicProfileLoad::missing()
            }
            Err(error) => {
                return MicProfileLoad::corrupt(format!(
                    "cannot read microphone profile {}: {error}",
                    path.display()
                ))
            }
        };
        let document: Value = match serde_json::from_str(&text) {
            Ok(document) => document,
            Err(error) => {
                return MicProfileLoad::corrupt(format!(
                    "microphone profile {} is not valid JSON: {error}",
                    path.display()
                ))
            }
        };
        match MicProfile::from_json(&document) {
            Ok(profile) => {
                let mut reasons = Vec::new();
                self.check_profile_cal_path(&profile, &mut reasons);
                MicProfileLoad {
                    profile: Some(profile),
                    reasons,
                    corruption: None,
                }
            }
            Err(reason) => MicProfileLoad::corrupt(format!(
                "microphone profile {} is not usable: {reason}",
                path.display()
            )),
        }
    }

    /// 保存 profile（写盘时刷新 updated_at；0600 原子写）。
    pub fn save(&self, mut profile: MicProfile) -> Result<MicProfile, CalibrationError> {
        profile.updated_at = now_rfc3339();
        let bytes = serde_json::to_vec_pretty(&profile.to_json()).map_err(|error| {
            CalibrationError::internal(format!("cannot serialize microphone profile: {error}"))
        })?;
        let path = self.profile_path();
        write_atomic(&path, &bytes, MIC_FILE_MODE).map_err(|error| {
            CalibrationError::internal(format!(
                "cannot write microphone profile {}: {error}",
                path.display()
            ))
        })?;
        Ok(profile)
    }

    /// 校验并内容寻址保存一份校准文件（阻塞 IO；调用方用 spawn_blocking）。
    ///
    /// 先解析（非法文件一个字节都不落盘），再原子写 `mics/<sha256>.txt` 与上传回执。
    /// 同内容重复上传幂等（同名文件覆盖写，回执的 filename 取最近一次请求）。
    pub fn store_calibration(
        &self,
        filename: &str,
        bytes: &[u8],
    ) -> Result<MicCalRecord, CalibrationError> {
        if !is_safe_cal_filename(filename) {
            return Err(invalid_field(
                "x-khaen-filename",
                format!("filename must be a bare file name, got {filename:?}"),
            ));
        }
        let parsed = parse_umik_calibration(bytes)?;
        let sha256 = sha256_hex(bytes);
        let path = self.calibration_path(&sha256)?;
        write_atomic(&path, bytes, MIC_FILE_MODE).map_err(|error| {
            CalibrationError::internal(format!(
                "cannot store microphone calibration file {}: {error}",
                path.display()
            ))
        })?;
        let meta = json!({
            "schema": MIC_CAL_META_SCHEMA,
            "sha256": sha256,
            "filename": filename,
            "bytes": bytes.len() as u64,
            "uploaded_at": now_rfc3339(),
        });
        let meta_bytes = serde_json::to_vec_pretty(&meta).map_err(|error| {
            CalibrationError::internal(format!(
                "cannot serialize microphone calibration metadata: {error}"
            ))
        })?;
        let meta_path = self.calibration_meta_path(&sha256);
        write_atomic(&meta_path, &meta_bytes, MIC_FILE_MODE).map_err(|error| {
            CalibrationError::internal(format!(
                "cannot store microphone calibration metadata {}: {error}",
                meta_path.display()
            ))
        })?;
        Ok(MicCalRecord {
            sha256,
            filename: filename.to_string(),
            bytes: bytes.len() as u64,
            path,
            parsed,
        })
    }

    /// 查找已上传的校准文件（不存在 → None；存在则复核内容寻址 sha256 并重新解析）。
    pub fn lookup_calibration(
        &self,
        sha256: &str,
    ) -> Result<Option<MicCalRecord>, CalibrationError> {
        let path = self.calibration_path(sha256)?;
        if !path.is_file() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path).map_err(|error| {
            CalibrationError::internal(format!(
                "cannot read stored microphone calibration file {}: {error}",
                path.display()
            ))
        })?;
        if bytes.len() as u64 > MAX_CAL_BYTES {
            return Err(invalid_field(
                "cal_sha256",
                format!(
                    "stored calibration file {} exceeds the {MAX_CAL_BYTES}-byte limit",
                    path.display()
                ),
            ));
        }
        let actual = sha256_hex(&bytes);
        if actual != sha256 {
            return Err(invalid_field(
                "cal_sha256",
                format!(
                    "stored calibration file {} does not match its content address: expected sha256 {sha256}, got {actual}",
                    path.display()
                ),
            ));
        }
        let parsed = parse_umik_calibration(&bytes).map_err(|error| {
            CalibrationError::internal(format!(
                "stored microphone calibration file {} is no longer a valid UMIK file: {}",
                path.display(),
                error.message()
            ))
        })?;
        let filename = self
            .read_cal_meta_filename(sha256)
            .unwrap_or_else(|| format!("{sha256}.txt"));
        Ok(Some(MicCalRecord {
            sha256: sha256.to_string(),
            filename,
            bytes: bytes.len() as u64,
            path,
            parsed,
        }))
    }

    fn read_cal_meta_filename(&self, sha256: &str) -> Option<String> {
        let text = std::fs::read_to_string(self.calibration_meta_path(sha256)).ok()?;
        let document: Value = serde_json::from_str(&text).ok()?;
        let object = document.as_object()?;
        if object.get("schema").and_then(Value::as_str) != Some(MIC_CAL_META_SCHEMA) {
            return None;
        }
        let filename = object.get("filename").and_then(Value::as_str)?;
        if !is_safe_cal_filename(filename) {
            return None;
        }
        Some(filename.to_string())
    }

    fn check_profile_cal_path(&self, profile: &MicProfile, reasons: &mut Vec<String>) {
        let expected = match self.calibration_path(&profile.cal_sha256) {
            Ok(path) => path,
            Err(_) => {
                reasons.push(format!(
                    "profile.cal_sha256 {:?} is not a valid content address",
                    profile.cal_sha256
                ));
                return;
            }
        };
        if Path::new(&profile.cal_path) != expected {
            reasons.push(format!(
                "profile.cal_path {:?} is not the content-addressed calibration path {}; the stored profile is inconsistent",
                profile.cal_path,
                expected.display()
            ));
        }
        if !expected.is_file() {
            reasons.push(format!(
                "calibration file {} referenced by the profile is missing: POST /api/calibration/mic/calibration to upload it again",
                expected.display()
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// PUT /api/calibration/mic：部分更新 + 严格校验
// ---------------------------------------------------------------------------

/// PUT 允许的字段（未知字段 400 `calibration_mic_unknown_field`）。
pub const ALLOWED_PROFILE_FIELDS: [&str; 4] = ["device", "serial", "orientation", "cal_sha256"];

/// 应用一次 PUT 载荷（部分更新；未知字段/取值非法 400；引用的校准文件必须已上传）。
///
/// 合并后的 profile 至少要有 device/orientation/cal_sha256；serial 省略时取文件 SERNO，
/// 提供时必须与 SERNO 完全一致（否则 [`code::MIC_SERIAL_MISMATCH`]）。
pub fn apply_profile_patch(
    current: Option<&MicProfile>,
    store: &MicProfileStore,
    payload: &Value,
) -> Result<MicProfile, CalibrationError> {
    let object = payload.as_object().ok_or_else(|| {
        invalid_field(
            "<body>",
            "PUT /api/calibration/mic body must be a JSON object",
        )
    })?;
    if object.is_empty() {
        return Err(invalid_field(
            "<body>",
            format!(
                "PUT body must set at least one of {}",
                ALLOWED_PROFILE_FIELDS.join(", ")
            ),
        ));
    }
    let mut device = current.map(|profile| profile.device.clone());
    let mut serial = current.map(|profile| profile.serial.clone());
    let mut orientation = current.map(|profile| profile.orientation.clone());
    let mut cal_sha256 = current.map(|profile| profile.cal_sha256.clone());
    for (key, value) in object {
        match key.as_str() {
            "device" => device = Some(parse_device(value)?),
            "serial" => serial = Some(parse_serial(value)?),
            "orientation" => orientation = Some(parse_orientation(value)?),
            "cal_sha256" => cal_sha256 = Some(parse_cal_sha256(value)?),
            other => return Err(unknown_field(other)),
        }
    }
    let Some(cal_sha256) = cal_sha256 else {
        return Err(invalid_field(
            "cal_sha256",
            "cal_sha256 is required when no microphone profile exists yet: upload the UMIK txt with POST /api/calibration/mic/calibration and pass the returned sha256",
        ));
    };
    let Some(record) = store.lookup_calibration(&cal_sha256)? else {
        return Err(calibration_not_found(&cal_sha256));
    };
    let serial = match serial {
        Some(declared) => {
            if declared != record.parsed.serno {
                return Err(serial_mismatch(
                    &declared,
                    &record.parsed.serno,
                    &cal_sha256,
                ));
            }
            declared
        }
        None => record.parsed.serno.clone(),
    };
    let Some(orientation) = orientation else {
        return Err(invalid_field(
            "orientation",
            format!(
                "orientation is required when no microphone profile exists yet: one of {MIC_ORIENTATIONS:?}"
            ),
        ));
    };
    let Some(device) = device else {
        return Err(invalid_field(
            "device",
            "device is required when no microphone profile exists yet: pass the UMIK ALSA device, e.g. \"hw:CARD=U18dB,DEV=0\"",
        ));
    };
    store.save(MicProfile {
        device,
        serial,
        orientation,
        cal_sha256: record.sha256,
        cal_filename: record.filename,
        cal_path: record.path.to_string_lossy().into_owned(),
        parsed: record.parsed,
        updated_at: String::new(),
    })
}

fn parse_device(value: &Value) -> Result<String, CalibrationError> {
    let Value::String(raw) = value else {
        return Err(invalid_field(
            "device",
            "device must be a string (ALSA device name, e.g. \"hw:CARD=U18dB,DEV=0\")",
        ));
    };
    let device = raw.trim();
    if device.is_empty() {
        return Err(invalid_field("device", "device must not be empty"));
    }
    if device.len() > MAX_DEVICE_LEN {
        return Err(invalid_field(
            "device",
            format!("device must be at most {MAX_DEVICE_LEN} bytes"),
        ));
    }
    if device
        .chars()
        .any(|character| character.is_control() || matches!(character, '/' | '\\'))
    {
        return Err(invalid_field(
            "device",
            "device must not contain path separators or control characters",
        ));
    }
    Ok(device.to_string())
}

fn parse_serial(value: &Value) -> Result<String, CalibrationError> {
    let Value::String(raw) = value else {
        return Err(invalid_field(
            "serial",
            "serial must be a string (the SERNO declared in the UMIK calibration file)",
        ));
    };
    let serial = raw.trim();
    if !is_valid_serial(serial) {
        return Err(invalid_field(
            "serial",
            format!(
                "serial must be 1..={MAX_SERIAL_LEN} ASCII letters/digits/'-'/'_', got {serial:?}"
            ),
        ));
    }
    Ok(serial.to_string())
}

fn parse_orientation(value: &Value) -> Result<String, CalibrationError> {
    let Value::String(raw) = value else {
        return Err(invalid_field(
            "orientation",
            format!("orientation must be one of {MIC_ORIENTATIONS:?}"),
        ));
    };
    if !MIC_ORIENTATIONS.contains(&raw.as_str()) {
        return Err(orientation_invalid(raw));
    }
    Ok(raw.clone())
}

fn parse_cal_sha256(value: &Value) -> Result<String, CalibrationError> {
    let Value::String(raw) = value else {
        return Err(invalid_field(
            "cal_sha256",
            "cal_sha256 must be a string (the sha256 returned by POST /api/calibration/mic/calibration)",
        ));
    };
    let sha256 = raw.trim();
    if !is_sha256_hex(sha256) {
        return Err(invalid_field(
            "cal_sha256",
            format!("cal_sha256 must be 64 hex characters, got {sha256:?}"),
        ));
    }
    Ok(sha256.to_ascii_lowercase())
}

// ---------------------------------------------------------------------------
// X-KHAEN-FILENAME
// ---------------------------------------------------------------------------

/// 从请求头取校准文件名（缺省 `umik.txt`；只接受 bare filename）。
pub fn calibration_filename_from_headers(headers: &HeaderMap) -> Result<String, CalibrationError> {
    let requested = match headers.get("x-khaen-filename") {
        None => None,
        Some(value) => Some(value.to_str().map_err(|error| {
            invalid_field(
                "x-khaen-filename",
                format!("x-khaen-filename must be UTF-8/ASCII text: {error}"),
            )
        })?),
    };
    sanitize_calibration_filename(requested)
}

/// 校准文件名归一/校验：缺省/空 → `umik.txt`；拒绝路径分隔符、`..`、控制字符、超长。
pub fn sanitize_calibration_filename(requested: Option<&str>) -> Result<String, CalibrationError> {
    let Some(raw) = requested else {
        return Ok(DEFAULT_CAL_FILENAME.to_string());
    };
    let name = raw.trim();
    if name.is_empty() {
        return Ok(DEFAULT_CAL_FILENAME.to_string());
    }
    if !is_safe_cal_filename(name) {
        return Err(invalid_field(
            "x-khaen-filename",
            format!(
                "x-khaen-filename must be a bare file name (1..={MAX_CAL_FILENAME_LEN} characters, no path separators, no '..')"
            ),
        ));
    }
    Ok(name.to_string())
}

// ---------------------------------------------------------------------------
// 单元测试（解析/校验/存储；HTTP 端到端在 tests/calibration_mic.rs）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// 真实 miniDSP UMIK-1 文件的最小复刻（首行 Sens Factor + SERNO、引号注释、TAB 两列）。
    const REAL_SAMPLE: &str = concat!(
        "\"Sens Factor =-0.0109999dB, SERNO: 7187696\"\n",
        "\"Auto-generated 90-degree calibration file\"\n",
        "10.054\t-4.7795\n",
        "10.179\t-4.6166\n",
        "10.306\t-4.4569\n"
    );

    fn temp_state_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!("khaen-mic-{}-{tag}-{unique}", std::process::id()));
        std::fs::create_dir_all(&path).expect("temp dir");
        path
    }

    fn sample_store(tag: &str) -> (PathBuf, MicProfileStore) {
        let dir = temp_state_dir(tag);
        let store = MicProfileStore::new(&dir);
        (dir, store)
    }

    #[test]
    fn parses_real_umik_file() {
        let parsed = parse_umik_calibration(REAL_SAMPLE.as_bytes()).expect("real sample parses");
        assert_eq!(parsed.serno, "7187696");
        assert_eq!(parsed.points, 3);
        let sens = parsed.sens_factor_db.expect("sens factor from header");
        assert!((sens - -0.0109999).abs() < 1e-12, "{sens}");
        let json = parsed.to_json();
        assert_eq!(json["serno"], json!("7187696"));
        assert_eq!(json["points"], json!(3));
        assert_eq!(MicParsed::from_json(&json).expect("roundtrip"), parsed);
    }

    #[test]
    fn parses_plain_serno_headers_crlf_and_blank_lines() {
        // 简单 SERNO 头 + 无 Sens Factor（不猜灵敏度）。
        let parsed = parse_umik_calibration(b"SERNO: 12345\n\n1000.000000 0.011\n2000 0.0\n")
            .expect("plain header");
        assert_eq!(parsed.serno, "12345");
        assert_eq!(parsed.sens_factor_db, None);
        assert_eq!(parsed.points, 2);
        // CRLF + "SERNO 123"（无冒号）+ 注释行。
        let parsed =
            parse_umik_calibration(b"SERNO 123\r\n# comment\r\n* other comment\r\n20.0 -1.5\r\n")
                .expect("crlf header");
        assert_eq!(parsed.serno, "123");
        assert_eq!(parsed.points, 1);
        // 首行 Sens Factor 与 SERNO 顺序无关。
        let parsed = parse_umik_calibration(b"\"SERNO: 777\", \"Sens Factor =+1.25dB\"\n20 0\n")
            .expect("serno first");
        assert_eq!(parsed.serno, "777");
        assert_eq!(parsed.sens_factor_db, Some(1.25));
    }

    #[test]
    fn rejects_invalid_calibration_files() {
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("empty", Vec::new()),
            ("bom", b"\xEF\xBB\xBFSERNO: 1\n20 0\n".to_vec()),
            ("invalid utf8", b"SERNO: 1\n20 0\n\xFF".to_vec()),
            ("binary nul", b"SERNO: 1\n\x001\n".to_vec()),
            ("no serno header", b"1000 0.011\n2000 0.0\n".to_vec()),
            ("comment first line", b"\"just a comment\"\n20 0\n".to_vec()),
            ("serno without value", b"SERNO:\n20 0\n".to_vec()),
            ("no data", b"SERNO: 123\n\"comment\"\n\n".to_vec()),
            ("three columns", b"SERNO: 1\n1000 0.0 7\n".to_vec()),
            ("one column", b"SERNO: 1\n1000\n".to_vec()),
            ("non-numeric", b"SERNO: 1\nabc 0.0\n".to_vec()),
            ("non-numeric db", b"SERNO: 1\n1000 loud\n".to_vec()),
            ("zero frequency", b"SERNO: 1\n0 0.0\n".to_vec()),
            ("negative frequency", b"SERNO: 1\n-20 0.0\n".to_vec()),
            ("nan frequency", b"SERNO: 1\nNaN 0.0\n".to_vec()),
            ("infinite db", b"SERNO: 1\n1000 inf\n".to_vec()),
            (
                "bad sens factor",
                b"Sens Factor =louddB, SERNO: 1\n20 0\n".to_vec(),
            ),
        ];
        for (name, bytes) in cases {
            let error = parse_umik_calibration(&bytes).expect_err(name);
            assert_eq!(error.code(), code::MIC_INVALID, "{name}: {error}");
            assert!(
                !error.message().is_empty(),
                "{name} must explain why it was rejected"
            );
        }
    }

    #[test]
    fn enforces_size_and_point_limits() {
        let oversized = vec![b' '; MAX_CAL_BYTES as usize + 1];
        let error = parse_umik_calibration(&oversized).expect_err("too large bytes");
        assert_eq!(error.code(), code::MIC_TOO_LARGE);
        assert_eq!(error.details()["limit_bytes"], json!(MAX_CAL_BYTES));

        // 100001 个最小数据点（约 400 KB，仍在 1 MiB 内）→ 点数上限。
        let mut text = String::from("SERNO: 1\n");
        for _ in 0..=MAX_CAL_POINTS {
            text.push_str("1 0\n");
        }
        let error = parse_umik_calibration(text.as_bytes()).expect_err("too many points");
        assert_eq!(error.code(), code::MIC_TOO_LARGE);
        assert_eq!(error.details()["limit_points"], json!(MAX_CAL_POINTS));
    }

    #[test]
    fn profile_json_is_strict_and_roundtrips() {
        let parsed = MicParsed {
            serno: "7187696".to_string(),
            sens_factor_db: Some(-0.0109999),
            points: 2,
        };
        let profile = MicProfile {
            device: "hw:CARD=U18dB,DEV=0".to_string(),
            serial: "7187696".to_string(),
            orientation: "90deg".to_string(),
            cal_sha256: "a".repeat(64),
            cal_filename: "umik1_718-7696_90deg.txt".to_string(),
            cal_path: format!("/tmp/calibration/mics/{}.txt", "a".repeat(64)),
            parsed: parsed.clone(),
            updated_at: "2026-11-27T09:30:00Z".to_string(),
        };
        let json = profile.to_json();
        assert_eq!(json["schema"], json!(MIC_PROFILE_SCHEMA));
        assert_eq!(MicProfile::from_json(&json).expect("roundtrip"), profile);

        let mut bad_schema = json.clone();
        bad_schema["schema"] = json!("khaen-mic-profile/v2");
        assert!(MicProfile::from_json(&bad_schema).is_err());
        let mut bad_orientation = json.clone();
        bad_orientation["orientation"] = json!("180deg");
        assert!(MicProfile::from_json(&bad_orientation).is_err());
        let mut missing_path = json.clone();
        missing_path
            .as_object_mut()
            .expect("object")
            .remove("cal_path");
        assert!(MicProfile::from_json(&missing_path).is_err());
        let mut bad_sha = json;
        bad_sha["cal_sha256"] = json!("not-a-sha");
        assert!(MicProfile::from_json(&bad_sha).is_err());

        let mut bad_points = parsed.to_json();
        bad_points["points"] = json!(0);
        assert!(MicParsed::from_json(&bad_points).is_err());
        let mut bad_sens = MicParsed {
            serno: "1".to_string(),
            sens_factor_db: None,
            points: 1,
        }
        .to_json();
        bad_sens["sens_factor_db"] = json!("loud");
        assert!(MicParsed::from_json(&bad_sens).is_err());
    }

    #[test]
    fn store_is_content_addressed_and_lookup_revalidates() {
        let (dir, store) = sample_store("store");
        let record = store
            .store_calibration("umik1_718-7696_90deg.txt", REAL_SAMPLE.as_bytes())
            .expect("store calibration");
        assert_eq!(record.sha256, sha256_hex(REAL_SAMPLE.as_bytes()));
        assert_eq!(record.filename, "umik1_718-7696_90deg.txt");
        assert_eq!(record.bytes, REAL_SAMPLE.len() as u64);
        assert_eq!(record.parsed.serno, "7187696");
        // 内容寻址路径 = <state>/calibration/mics/<sha>.txt。
        let expected = dir
            .join(MIC_DIR)
            .join(MIC_CAL_SUBDIR)
            .join(format!("{}.txt", record.sha256));
        assert_eq!(record.path, expected);
        assert_eq!(
            std::fs::read(&expected).expect("stored file"),
            REAL_SAMPLE.as_bytes()
        );
        assert!(dir
            .join(MIC_DIR)
            .join(MIC_CAL_SUBDIR)
            .join(format!("{}.json", record.sha256))
            .is_file());

        let looked_up = store
            .lookup_calibration(&record.sha256)
            .expect("lookup")
            .expect("present");
        assert_eq!(looked_up, record);
        assert_eq!(
            store.lookup_calibration(&"f".repeat(64)).expect("missing"),
            None
        );

        // 内容被篡改（字节与文件名不再对应）→ 400 calibration_mic_invalid，不冒充可用。
        std::fs::write(&expected, b"SERNO: 1\n20 0\n").expect("tamper");
        let error = store
            .lookup_calibration(&record.sha256)
            .expect_err("tampered file rejected");
        assert_eq!(error.code(), code::MIC_INVALID);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn profile_save_load_roundtrip_and_corruption_reasons() {
        let (dir, store) = sample_store("profile");
        let record = store
            .store_calibration("umik.txt", REAL_SAMPLE.as_bytes())
            .expect("store calibration");
        let profile = MicProfile {
            device: "hw:CARD=U18dB,DEV=0".to_string(),
            serial: "7187696".to_string(),
            orientation: "90deg".to_string(),
            cal_sha256: record.sha256.clone(),
            cal_filename: record.filename.clone(),
            cal_path: record.path.to_string_lossy().into_owned(),
            parsed: record.parsed.clone(),
            updated_at: String::new(),
        };
        let saved = store.save(profile).expect("save");
        assert!(saved.updated_at.ends_with('Z'));
        let load = store.load();
        assert!(load.corruption.is_none(), "{load:?}");
        assert!(load.reasons.is_empty(), "{load:?}");
        let loaded = load.profile.expect("profile");
        assert_eq!(loaded.orientation, "90deg");
        assert_eq!(loaded.cal_sha256, record.sha256);
        assert_eq!(loaded.parsed.serno, "7187696");
        assert_eq!(loaded.updated_at, saved.updated_at);
        assert!(store.profile_path().is_file());

        // 校准文件被删：profile 仍回显，但 reasons 如实说明不可用。
        std::fs::remove_file(&record.path).expect("remove cal file");
        let load = store.load();
        assert!(load.profile.is_some());
        assert!(
            load.reasons.iter().any(|reason| reason.contains("missing")),
            "{load:?}"
        );

        // mic.json 损坏：profile=null + corruption 原因（不冒充已保存）。
        std::fs::write(store.profile_path(), b"{not json").expect("corrupt profile");
        let load = store.load();
        assert!(load.profile.is_none());
        assert!(load.corruption.is_some());
        assert!(!load.reasons.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn apply_profile_patch_validates_and_merges() {
        let (dir, store) = sample_store("patch");
        let record = store
            .store_calibration("umik1_718-7696_90deg.txt", REAL_SAMPLE.as_bytes())
            .expect("store calibration");

        // 空 body / 未知字段。
        let error = apply_profile_patch(None, &store, &json!({})).expect_err("empty");
        assert_eq!(error.code(), code::MIC_INVALID);
        let error =
            apply_profile_patch(None, &store, &json!({"colour": "red"})).expect_err("unknown");
        assert_eq!(error.code(), code::MIC_UNKNOWN_FIELD);
        // cal_sha256 不存在。
        let error = apply_profile_patch(
            None,
            &store,
            &json!({
                "device": "hw:0,0",
                "orientation": "90deg",
                "cal_sha256": "0".repeat(64),
            }),
        )
        .expect_err("missing calibration");
        assert_eq!(error.code(), code::MIC_NOT_FOUND);
        // 首次保存缺 orientation / device（按检查顺序 orientation 先）。
        let error = apply_profile_patch(
            None,
            &store,
            &json!({"device": "hw:0,0", "cal_sha256": record.sha256}),
        )
        .expect_err("missing orientation");
        assert_eq!(error.code(), code::MIC_INVALID);
        assert_eq!(error.details()["field"], json!("orientation"));
        let error = apply_profile_patch(
            None,
            &store,
            &json!({"orientation": "90deg", "cal_sha256": record.sha256}),
        )
        .expect_err("missing device");
        assert_eq!(error.code(), code::MIC_INVALID);
        assert_eq!(error.details()["field"], json!("device"));
        // serial 不符。
        let error = apply_profile_patch(
            None,
            &store,
            &json!({
                "device": "hw:0,0",
                "serial": "0000000",
                "orientation": "90deg",
                "cal_sha256": record.sha256,
            }),
        )
        .expect_err("serial mismatch");
        assert_eq!(error.code(), code::MIC_SERIAL_MISMATCH);
        assert_eq!(error.details()["serial"], json!("0000000"));
        assert_eq!(error.details()["serno"], json!("7187696"));
        // orientation 非法。
        let error = apply_profile_patch(
            None,
            &store,
            &json!({
                "device": "hw:0,0",
                "orientation": "180deg",
                "cal_sha256": record.sha256,
            }),
        )
        .expect_err("orientation invalid");
        assert_eq!(error.code(), code::MIC_ORIENTATION_INVALID);
        assert_eq!(error.details()["allowed"], json!(MIC_ORIENTATIONS));

        // 首次保存：serial 省略 → 取文件 SERNO。
        let saved = apply_profile_patch(
            None,
            &store,
            &json!({
                "device": "hw:CARD=U18dB,DEV=0",
                "orientation": "90deg",
                "cal_sha256": record.sha256,
            }),
        )
        .expect("create profile");
        assert_eq!(saved.serial, "7187696");
        assert_eq!(saved.cal_filename, "umik1_718-7696_90deg.txt");
        assert_eq!(saved.cal_path, record.path.to_string_lossy());

        // 部分更新：只改 orientation，其余字段保持。
        let updated = apply_profile_patch(Some(&saved), &store, &json!({"orientation": "0deg"}))
            .expect("partial update");
        assert_eq!(updated.orientation, "0deg");
        assert_eq!(updated.device, saved.device);
        assert_eq!(updated.serial, saved.serial);
        assert_eq!(updated.cal_sha256, saved.cal_sha256);
        let reloaded = store.load().profile.expect("reloaded profile");
        assert_eq!(reloaded.orientation, "0deg");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn filenames_are_sanitized_and_headers_are_case_insensitive() {
        assert_eq!(
            sanitize_calibration_filename(None).expect("default"),
            DEFAULT_CAL_FILENAME
        );
        assert_eq!(
            sanitize_calibration_filename(Some("   ")).expect("blank default"),
            DEFAULT_CAL_FILENAME
        );
        assert_eq!(
            sanitize_calibration_filename(Some("umik1_718-7696_90deg.txt")).expect("bare"),
            "umik1_718-7696_90deg.txt"
        );
        for bad in [
            "../../auth.json",
            "sub/umik.txt",
            "..\\umik.txt",
            "..",
            "a/../b",
        ] {
            let error = sanitize_calibration_filename(Some(bad)).expect_err(bad);
            assert_eq!(error.code(), code::MIC_INVALID, "{bad}");
        }
        let too_long = "a".repeat(MAX_CAL_FILENAME_LEN + 1);
        assert!(sanitize_calibration_filename(Some(&too_long)).is_err());

        let mut headers = HeaderMap::new();
        assert_eq!(
            calibration_filename_from_headers(&headers).expect("absent"),
            DEFAULT_CAL_FILENAME
        );
        headers.insert("X-KHAEN-FILENAME", "umik.txt".parse().expect("header"));
        assert_eq!(
            calibration_filename_from_headers(&headers).expect("present"),
            "umik.txt"
        );
    }
}
