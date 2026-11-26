//! 校准向导配置与状态（T36 / PLAN §10.4）。
//!
//! # 配置（GET/PUT /api/calibration/wizard）
//!
//! 持久化到状态目录（KHAEN_STATE_DIR）下的 `wizard.json`，原子写 0600。GET 返回
//! **生效配置 + 默认值 + saved**：未保存时配置等于默认值，saved=false，reasons 说明哪些
//! 字段还没配置；PUT 接受部分字段（未知字段 400），校验通过后写盘。
//!
//! 字段与取值都来自真实来源，不编：
//! - `orientation`：UMIK 的朝向惯例 `0deg` / `90deg`（听音位多点要求 90deg，见
//!   crates/calibration/src/positions.rs；单点向导允许用户选）；
//! - `target_mode`：designer 的 BuiltinTarget id `flat` / `tilt10`（默认 flat）；
//! - `single_point`：离线闭环 CLI 的默认 1/3-oct 中心（25 Hz - 20 kHz，30 带）、
//!   tolerance_db=2.0、min_bands=3（与 crates/calibration 的公开常量同值；daemon 生产
//!   代码不依赖校准后端，故在此冻结成 REST 契约的默认值）。
//!
//! # 状态（GET /api/calibration/wizard/state）
//!
//! 由 **真实来源**推导，刷新可恢复：步骤 1 读 /api/status 聚合（设备/速率/主机连接），
//! 步骤 2 只读展示 /api/status 的 format/channels（/api/channels 已实现；不可读时 blocked），步骤 3/4 读向导配置，步骤 5-8 读最近
//! 一个校准 job 与 apply 状态；后端未接线时步骤 5-8 一律 not_available + reason（不阻塞
//! 向导）。没有数据来源的字段给 null + reasons，不编进度。

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use super::service::{CalibrationError, CalibrationStatus, JobRecord, JobState};
use super::store::{is_safe_artifact_name, is_sha256_hex, write_atomic, UploadStore};

/// 向导配置文件（状态目录下）。
pub const WIZARD_FILE: &str = "wizard.json";
/// 配置 schema（GET/PUT 响应与磁盘格式同名）。
pub const WIZARD_SCHEMA: &str = "khaen-calibration-wizard/v1";
/// 磁盘存储格式 schema（比 REST 多 measurement/updated_at）。
pub const WIZARD_STORE_SCHEMA: &str = "khaen-calibration-wizard-store/v1";
/// 向导状态 schema。
pub const WIZARD_STATE_SCHEMA: &str = "khaen-calibration-wizard-state/v1";
/// 麦克风朝向取值（UMIK 惯例：0deg 水平 / 90deg 垂直）。
pub const MIC_ORIENTATIONS: [&str; 2] = ["0deg", "90deg"];
/// 目标曲线取值（designer BuiltinTarget 的 id）。
pub const TARGET_MODES: [&str; 2] = ["flat", "tilt10"];
/// 默认目标曲线（designer BuiltinTarget::DEFAULT）。
pub const DEFAULT_TARGET_MODE: &str = "flat";
/// 默认 1/3-oct 中心频率（25 Hz - 20 kHz，30 带；与 calibration CLI 默认同值）。
pub const DEFAULT_CENTERS_HZ: [f64; 30] = [
    25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0, 500.0,
    630.0, 800.0, 1000.0, 1250.0, 1600.0, 2000.0, 2500.0, 3150.0, 4000.0, 5000.0, 6300.0, 8000.0,
    10000.0, 12500.0, 16000.0, 20000.0,
];
/// 默认判定容差（dB；与 calibration CLI 默认同值）。
pub const DEFAULT_TOLERANCE_DB: f64 = 2.0;
/// 默认最少可比较带数。
pub const DEFAULT_MIN_BANDS: u32 = 3;
/// centers_hz 上限（防坏请求塞爆磁盘/内存）。
pub const MAX_CENTERS: usize = 64;
/// 中心频率允许范围（Hz）。
pub const MIN_CENTER_HZ: f64 = 1.0;
/// 中心频率允许范围上限（Hz；1/3-oct 标准表最高 20 kHz）。
pub const MAX_CENTER_HZ: f64 = 20_000.0;
/// 判定容差允许范围（dB）。
pub const MAX_TOLERANCE_DB: f64 = 24.0;
/// 麦克风校准文件名最大长度。
pub const MAX_MIC_FILENAME_LEN: usize = 128;
/// wizard.json 权限位（Unix）。
pub const WIZARD_FILE_MODE: u32 = 0o600;

/// 步骤 5（测量）持久化草稿：两段 WAV 的上传 id。
///
/// PLAN §10.4 的 config 字段不含 uploads，但向导刷新后要能恢复第 5 步，所以这里作为
/// **持久化扩展**保存在 wizard.json（PUT 可选字段 measurement）。job 创建后以最近的 job
/// inputs 为准（job 是更权威的来源）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WizardMeasurement {
    /// baseline WAV 上传 id。
    pub baseline_upload_id: Option<String>,
    /// applied WAV 上传 id。
    pub applied_upload_id: Option<String>,
    /// 计划校验的 plan sha256（可选）。
    pub plan_sha: Option<String>,
}

/// 磁盘上已保存的向导配置（Option = 未保存/未设置；生效值见 [`EffectiveWizard`]）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WizardStored {
    /// 麦克风校准文件 sha256（= 上传 id）。
    pub mic_cal_sha256: Option<String>,
    /// 麦克风校准文件名（展示用）。
    pub mic_cal_filename: Option<String>,
    /// 麦克风朝向（MIC_ORIENTATIONS）。
    pub orientation: Option<String>,
    /// 目标曲线（TARGET_MODES；None = 用默认）。
    pub target_mode: Option<String>,
    /// 中心频率（None = 用默认）。
    pub centers_hz: Option<Vec<f64>>,
    /// 判定容差（None = 用默认）。
    pub tolerance_db: Option<f64>,
    /// 最少可比较带数（None = 用默认）。
    pub min_bands: Option<u32>,
    /// 步骤 5 草稿。
    pub measurement: WizardMeasurement,
}

/// 生效配置（未保存字段填默认值）。
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveWizard {
    /// 见 [`WizardStored::mic_cal_sha256`]。
    pub mic_cal_sha256: Option<String>,
    /// 见 [`WizardStored::mic_cal_filename`]。
    pub mic_cal_filename: Option<String>,
    /// 见 [`WizardStored::orientation`]。
    pub orientation: Option<String>,
    /// 目标曲线（默认 flat）。
    pub target_mode: String,
    /// 1/3-oct 中心频率（默认 25 Hz - 20 kHz）。
    pub centers_hz: Vec<f64>,
    /// 判定容差（默认 2.0 dB）。
    pub tolerance_db: f64,
    /// 最少可比较带数（默认 3）。
    pub min_bands: u32,
}

impl WizardStored {
    /// 计算生效配置（默认值来自本模块常量）。
    pub fn effective(&self) -> EffectiveWizard {
        EffectiveWizard {
            mic_cal_sha256: self.mic_cal_sha256.clone(),
            mic_cal_filename: self.mic_cal_filename.clone(),
            orientation: self.orientation.clone(),
            target_mode: self
                .target_mode
                .clone()
                .unwrap_or_else(|| DEFAULT_TARGET_MODE.to_string()),
            centers_hz: self
                .centers_hz
                .clone()
                .unwrap_or_else(|| DEFAULT_CENTERS_HZ.to_vec()),
            tolerance_db: self.tolerance_db.unwrap_or(DEFAULT_TOLERANCE_DB),
            min_bands: self.min_bands.unwrap_or(DEFAULT_MIN_BANDS),
        }
    }

    /// 生效配置的 REST JSON（GET/PUT 的 config 字段）。
    pub fn config_json(&self) -> Value {
        let effective = self.effective();
        json!({
            "mic_cal_sha256": effective.mic_cal_sha256,
            "mic_cal_filename": effective.mic_cal_filename,
            "orientation": effective.orientation,
            "target_mode": effective.target_mode,
            "single_point": {
                "centers_hz": effective.centers_hz,
                "tolerance_db": effective.tolerance_db,
                "min_bands": effective.min_bands,
            },
        })
    }

    /// 默认值 JSON（GET 的 defaults 字段；真实常量，不是示意值）。
    pub fn defaults_json() -> Value {
        json!({
            "mic_cal_sha256": Value::Null,
            "mic_cal_filename": Value::Null,
            "orientation": Value::Null,
            "target_mode": DEFAULT_TARGET_MODE,
            "single_point": {
                "centers_hz": DEFAULT_CENTERS_HZ,
                "tolerance_db": DEFAULT_TOLERANCE_DB,
                "min_bands": DEFAULT_MIN_BANDS,
            },
        })
    }

    /// 磁盘格式 JSON（含 measurement 与 updated_at）。
    pub fn store_json(&self, updated_at: &str) -> Value {
        json!({
            "schema": WIZARD_STORE_SCHEMA,
            "updated_at": updated_at,
            "config": {
                "mic_cal_sha256": self.mic_cal_sha256,
                "mic_cal_filename": self.mic_cal_filename,
                "orientation": self.orientation,
                "target_mode": self.target_mode,
                "single_point": {
                    "centers_hz": self.centers_hz,
                    "tolerance_db": self.tolerance_db,
                    "min_bands": self.min_bands,
                },
            },
            "measurement": {
                "baseline_upload_id": self.measurement.baseline_upload_id,
                "applied_upload_id": self.measurement.applied_upload_id,
                "plan_sha": self.measurement.plan_sha,
            },
        })
    }

    /// 从磁盘 JSON 恢复（宽松：未知字段忽略、可选字段缺失按未设置；类型不对报 corrupt）。
    pub fn from_store_json(document: &Value) -> Result<(Self, Option<String>), String> {
        let object = document
            .as_object()
            .ok_or_else(|| "wizard store document must be a JSON object".to_string())?;
        if let Some(schema) = object.get("schema") {
            let schema = schema.as_str().unwrap_or("");
            if schema != WIZARD_STORE_SCHEMA && schema != WIZARD_SCHEMA {
                return Err(format!(
                    "unsupported wizard store schema {schema:?} (expected {WIZARD_STORE_SCHEMA})"
                ));
            }
        }
        let updated_at = object
            .get("updated_at")
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut stored = Self::default();
        if let Some(config) = object.get("config") {
            apply_stored_config(&mut stored, config)?;
        }
        if let Some(measurement) = object.get("measurement") {
            stored.measurement = parse_measurement(measurement, "measurement")?;
        }
        Ok((stored, updated_at))
    }
}

/// 宽松解析磁盘上的 config 段（类型错 → 返回原因，由调用方按 corrupt 降级）。
fn apply_stored_config(stored: &mut WizardStored, config: &Value) -> Result<(), String> {
    let object = config
        .as_object()
        .ok_or_else(|| "wizard store config must be a JSON object".to_string())?;
    for (key, value) in object {
        match (key.as_str(), value) {
            ("mic_cal_sha256", Value::Null) => {}
            ("mic_cal_sha256", Value::String(text)) if is_sha256_hex(text) => {
                stored.mic_cal_sha256 = Some(text.to_ascii_lowercase());
            }
            ("mic_cal_filename", Value::Null) => {}
            ("mic_cal_filename", Value::String(text)) if is_safe_artifact_name(text) => {
                stored.mic_cal_filename = Some(text.clone());
            }
            ("orientation", Value::Null) => {}
            ("orientation", Value::String(text)) if MIC_ORIENTATIONS.contains(&text.as_str()) => {
                stored.orientation = Some(text.clone());
            }
            ("target_mode", Value::Null) => {}
            ("target_mode", Value::String(text)) if TARGET_MODES.contains(&text.as_str()) => {
                stored.target_mode = Some(text.clone());
            }
            ("single_point", Value::Null) => {}
            ("single_point", Value::Object(single)) => {
                apply_stored_single_point(stored, single)?;
            }
            // 未知字段：向前兼容忽略（磁盘格式不拒绝；REST PUT 才拒绝未知字段）。
            _ => {}
        }
    }
    Ok(())
}

fn apply_stored_single_point(
    stored: &mut WizardStored,
    single: &Map<String, Value>,
) -> Result<(), String> {
    for (key, value) in single {
        match (key.as_str(), value) {
            ("centers_hz", Value::Null) => {}
            ("centers_hz", Value::Array(values)) => {
                stored.centers_hz = Some(parse_centers_lenient(values)?);
            }
            ("tolerance_db", Value::Null) => {}
            ("tolerance_db", Value::Number(number)) => {
                stored.tolerance_db = Some(number.as_f64().unwrap_or(f64::NAN));
            }
            ("min_bands", Value::Null) => {}
            ("min_bands", Value::Number(number)) => {
                stored.min_bands = Some(number.as_u64().unwrap_or(0) as u32);
            }
            _ => {}
        }
    }
    Ok(())
}

fn parse_centers_lenient(values: &[Value]) -> Result<Vec<f64>, String> {
    let mut centers = Vec::with_capacity(values.len());
    for value in values {
        centers.push(
            value.as_f64().ok_or_else(|| {
                "wizard store single_point.centers_hz must be numbers".to_string()
            })?,
        );
    }
    Ok(centers)
}

fn parse_measurement(value: &Value, field: &str) -> Result<WizardMeasurement, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("wizard store {field} must be a JSON object"))?;
    let mut measurement = WizardMeasurement::default();
    for (key, value) in object {
        match (key.as_str(), value) {
            ("baseline_upload_id", Value::Null) => {}
            ("baseline_upload_id", Value::String(text)) => {
                measurement.baseline_upload_id = Some(text.clone())
            }
            ("applied_upload_id", Value::Null) => {}
            ("applied_upload_id", Value::String(text)) => {
                measurement.applied_upload_id = Some(text.clone())
            }
            ("plan_sha", Value::Null) => {}
            ("plan_sha", Value::String(text)) => measurement.plan_sha = Some(text.clone()),
            _ => {}
        }
    }
    Ok(measurement)
}

// ---------------------------------------------------------------------------
// 存储
// ---------------------------------------------------------------------------

/// 一次加载的结果：配置 + 是否已保存 + 损坏原因。
#[derive(Debug, Clone)]
pub struct WizardLoad {
    /// 已保存配置（未保存/损坏时为默认）。
    pub stored: WizardStored,
    /// 磁盘上是否有合法配置。
    pub saved: bool,
    /// 最近保存时间（RFC3339 UTC）。
    pub updated_at: Option<String>,
    /// 文件存在但损坏/读不了的原因（GET 的 reasons 会带出去；不冒充 saved）。
    pub corruption: Option<String>,
}

/// 向导配置存储（状态目录下的 wizard.json）。
#[derive(Debug, Clone)]
pub struct WizardStore {
    path: PathBuf,
}

impl WizardStore {
    /// 状态目录 → `<state_dir>/wizard.json`。
    pub fn new(state_dir: &Path) -> Self {
        Self {
            path: state_dir.join(WIZARD_FILE),
        }
    }

    /// 配置文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 读取配置（不存在 → 默认 + saved=false；损坏 → 默认 + corruption 原因）。
    pub fn load(&self) -> WizardLoad {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return WizardLoad {
                    stored: WizardStored::default(),
                    saved: false,
                    updated_at: None,
                    corruption: None,
                }
            }
            Err(error) => {
                return WizardLoad {
                    stored: WizardStored::default(),
                    saved: false,
                    updated_at: None,
                    corruption: Some(format!(
                        "cannot read wizard config {}: {error}",
                        self.path.display()
                    )),
                }
            }
        };
        let document: Value = match serde_json::from_str(&text) {
            Ok(document) => document,
            Err(error) => {
                return WizardLoad {
                    stored: WizardStored::default(),
                    saved: false,
                    updated_at: None,
                    corruption: Some(format!(
                        "wizard config {} is not valid JSON: {error}",
                        self.path.display()
                    )),
                }
            }
        };
        match WizardStored::from_store_json(&document) {
            Ok((stored, updated_at)) => WizardLoad {
                stored,
                saved: true,
                updated_at,
                corruption: None,
            },
            Err(reason) => WizardLoad {
                stored: WizardStored::default(),
                saved: false,
                updated_at: None,
                corruption: Some(format!(
                    "wizard config {} is not usable: {reason}",
                    self.path.display()
                )),
            },
        }
    }

    /// 原子写配置（0600），返回本次 updated_at（RFC3339 UTC）。
    pub fn save(&self, stored: &WizardStored) -> Result<String, CalibrationError> {
        let updated_at = now_rfc3339();
        let bytes =
            serde_json::to_vec_pretty(&stored.store_json(&updated_at)).map_err(|error| {
                CalibrationError::internal(format!("cannot serialize wizard config: {error}"))
            })?;
        write_atomic(&self.path, &bytes, WIZARD_FILE_MODE).map_err(|error| {
            CalibrationError::internal(format!(
                "cannot write wizard config {}: {error}",
                self.path.display()
            ))
        })?;
        Ok(updated_at)
    }
}

/// 当前时刻的 RFC3339 UTC 文本（与 /api/status 的 rfc3339_utc 同源）。
pub(crate) fn now_rfc3339() -> String {
    crate::status::rfc3339_utc(std::time::SystemTime::now())
}

// ---------------------------------------------------------------------------
// REST PUT：严格校验 + 合并
// ---------------------------------------------------------------------------

/// PUT /api/calibration/wizard 的载荷应用结果。
#[derive(Debug, Clone)]
pub struct WizardPatchOutcome {
    /// 合并后的已保存配置。
    pub stored: WizardStored,
    /// 本次新设置的字段名（响应里 saved_fields）。
    pub set_fields: Vec<String>,
}

/// 应用一次 PUT 载荷（严格：未知字段 400；类型/范围错 400；引用的上传必须存在）。
///
/// 允许的顶层字段：mic_cal_sha256 / mic_cal_filename / orientation / target_mode /
/// single_point / measurement。null 表示清空（回到默认/未设置）。
pub fn apply_wizard_patch(
    current: &WizardStored,
    uploads: &UploadStore,
    payload: &Value,
) -> Result<WizardPatchOutcome, CalibrationError> {
    let object = payload.as_object().ok_or_else(|| {
        CalibrationError::invalid_request("<body>", "wizard PUT body must be a JSON object")
    })?;
    let mut next = current.clone();
    let mut set_fields = Vec::new();
    for (key, value) in object {
        let field = key.as_str();
        match field {
            "mic_cal_sha256" => {
                next.mic_cal_sha256 = match parse_upload_id_field(value, "mic_cal_sha256")? {
                    Some(sha256) => {
                        if uploads.lookup(&sha256)?.is_none() {
                            return Err(CalibrationError::upload_not_found(&sha256)
                                .with_detail("field", json!("mic_cal_sha256")));
                        }
                        Some(sha256)
                    }
                    None => None,
                };
                set_fields.push("mic_cal_sha256".to_string());
            }
            "mic_cal_filename" => {
                next.mic_cal_filename = match value {
                    Value::Null => None,
                    Value::String(name) => {
                        if !is_safe_artifact_name(name) || name.len() > MAX_MIC_FILENAME_LEN {
                            return Err(CalibrationError::invalid_request(
                                "mic_cal_filename",
                                format!(
                                    "mic_cal_filename must be a bare file name of at most {MAX_MIC_FILENAME_LEN} characters without path separators"
                                ),
                            ));
                        }
                        Some(name.clone())
                    }
                    _ => {
                        return Err(CalibrationError::invalid_request(
                            "mic_cal_filename",
                            "mic_cal_filename must be a string or null",
                        ))
                    }
                };
                set_fields.push("mic_cal_filename".to_string());
            }
            "orientation" => {
                next.orientation = match value {
                    Value::Null => None,
                    Value::String(orientation) => {
                        if !MIC_ORIENTATIONS.contains(&orientation.as_str()) {
                            return Err(CalibrationError::invalid_request(
                                "orientation",
                                format!(
                                    "orientation must be one of {MIC_ORIENTATIONS:?} (UMIK convention), got {orientation:?}"
                                ),
                            ));
                        }
                        Some(orientation.clone())
                    }
                    _ => {
                        return Err(CalibrationError::invalid_request(
                            "orientation",
                            "orientation must be a string or null",
                        ))
                    }
                };
                set_fields.push("orientation".to_string());
            }
            "target_mode" => {
                next.target_mode = match value {
                    Value::Null => None,
                    Value::String(mode) => {
                        if !TARGET_MODES.contains(&mode.as_str()) {
                            return Err(CalibrationError::invalid_request(
                                "target_mode",
                                format!(
                                    "target_mode must be one of {TARGET_MODES:?} (designer BuiltinTarget ids), got {mode:?}"
                                ),
                            ));
                        }
                        Some(mode.clone())
                    }
                    _ => {
                        return Err(CalibrationError::invalid_request(
                            "target_mode",
                            "target_mode must be a string or null",
                        ))
                    }
                };
                set_fields.push("target_mode".to_string());
            }
            "single_point" => {
                apply_single_point_patch(&mut next, value)?;
                set_fields.push("single_point".to_string());
            }
            "measurement" => {
                next.measurement = parse_measurement_patch(uploads, value)?;
                set_fields.push("measurement".to_string());
            }
            other => {
                return Err(CalibrationError::invalid_request(
                    other,
                    format!("unknown field {other:?}: allowed fields are mic_cal_sha256, mic_cal_filename, orientation, target_mode, single_point, measurement"),
                ));
            }
        }
    }
    validate_stored(&next)?;
    Ok(WizardPatchOutcome {
        stored: next,
        set_fields,
    })
}

fn parse_upload_id_field(value: &Value, field: &str) -> Result<Option<String>, CalibrationError> {
    match value {
        Value::Null => Ok(None),
        Value::String(text) => {
            if !is_sha256_hex(text) {
                return Err(CalibrationError::invalid_request(
                    field,
                    format!("{field} must be a 64-character hex sha256"),
                ));
            }
            Ok(Some(text.to_ascii_lowercase()))
        }
        _ => Err(CalibrationError::invalid_request(
            field,
            format!("{field} must be a string or null"),
        )),
    }
}

fn apply_single_point_patch(
    stored: &mut WizardStored,
    value: &Value,
) -> Result<(), CalibrationError> {
    let object = match value {
        Value::Null => {
            stored.centers_hz = None;
            stored.tolerance_db = None;
            stored.min_bands = None;
            return Ok(());
        }
        Value::Object(object) => object,
        _ => {
            return Err(CalibrationError::invalid_request(
                "single_point",
                "single_point must be a JSON object or null",
            ))
        }
    };
    for (key, value) in object {
        match key.as_str() {
            "centers_hz" => {
                stored.centers_hz = match value {
                    Value::Null => None,
                    Value::Array(values) => Some(parse_centers_strict(values)?),
                    _ => {
                        return Err(CalibrationError::invalid_request(
                            "single_point.centers_hz",
                            "centers_hz must be an array of numbers or null",
                        ))
                    }
                };
            }
            "tolerance_db" => {
                stored.tolerance_db = match value {
                    Value::Null => None,
                    Value::Number(number) => {
                        let tolerance = number.as_f64().unwrap_or(f64::NAN);
                        if !tolerance.is_finite()
                            || tolerance <= 0.0
                            || tolerance > MAX_TOLERANCE_DB
                        {
                            return Err(CalibrationError::invalid_request(
                                "single_point.tolerance_db",
                                format!(
                                    "tolerance_db must be a finite number in (0, {MAX_TOLERANCE_DB}]"
                                ),
                            ));
                        }
                        Some(tolerance)
                    }
                    _ => {
                        return Err(CalibrationError::invalid_request(
                            "single_point.tolerance_db",
                            "tolerance_db must be a number or null",
                        ))
                    }
                };
            }
            "min_bands" => {
                stored.min_bands = match value {
                    Value::Null => None,
                    Value::Number(number) => {
                        let bands =
                            number.as_u64().filter(|value| *value > 0).ok_or_else(|| {
                                CalibrationError::invalid_request(
                                    "single_point.min_bands",
                                    "min_bands must be a positive integer",
                                )
                            })?;
                        let bands = u32::try_from(bands).map_err(|_| {
                            CalibrationError::invalid_request(
                                "single_point.min_bands",
                                "min_bands does not fit in u32",
                            )
                        })?;
                        Some(bands)
                    }
                    _ => {
                        return Err(CalibrationError::invalid_request(
                            "single_point.min_bands",
                            "min_bands must be an integer or null",
                        ))
                    }
                };
            }
            other => {
                return Err(CalibrationError::invalid_request(
                    &format!("single_point.{other}"),
                    format!(
                        "unknown field single_point.{other:?}: allowed fields are centers_hz, tolerance_db, min_bands"
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn parse_centers_strict(values: &[Value]) -> Result<Vec<f64>, CalibrationError> {
    if values.is_empty() {
        return Err(CalibrationError::invalid_request(
            "single_point.centers_hz",
            "centers_hz must not be empty",
        ));
    }
    if values.len() > MAX_CENTERS {
        return Err(CalibrationError::invalid_request(
            "single_point.centers_hz",
            format!("centers_hz must have at most {MAX_CENTERS} entries"),
        ));
    }
    let mut centers = Vec::with_capacity(values.len());
    for value in values {
        let center = value.as_f64().ok_or_else(|| {
            CalibrationError::invalid_request(
                "single_point.centers_hz",
                "centers_hz entries must be numbers",
            )
        })?;
        if !center.is_finite() || center < MIN_CENTER_HZ || center > MAX_CENTER_HZ {
            return Err(CalibrationError::invalid_request(
                "single_point.centers_hz",
                format!(
                    "centers_hz entries must be finite Hz values in [{MIN_CENTER_HZ}, {MAX_CENTER_HZ}]"
                ),
            ));
        }
        centers.push(center);
    }
    for pair in centers.windows(2) {
        // 上面已校验每个值有限且 > 0，可直接比较（不会出现 NaN）。
        if pair[0] >= pair[1] {
            return Err(CalibrationError::invalid_request(
                "single_point.centers_hz",
                "centers_hz must be strictly increasing",
            ));
        }
    }
    Ok(centers)
}

fn parse_measurement_patch(
    uploads: &UploadStore,
    value: &Value,
) -> Result<WizardMeasurement, CalibrationError> {
    let object = match value {
        Value::Null => return Ok(WizardMeasurement::default()),
        Value::Object(object) => object,
        _ => {
            return Err(CalibrationError::invalid_request(
                "measurement",
                "measurement must be a JSON object or null",
            ))
        }
    };
    let mut measurement = WizardMeasurement::default();
    for (key, value) in object {
        match key.as_str() {
            "baseline_upload_id" => {
                measurement.baseline_upload_id =
                    require_upload_id(uploads, value, "measurement.baseline_upload_id")?;
            }
            "applied_upload_id" => {
                measurement.applied_upload_id =
                    require_upload_id(uploads, value, "measurement.applied_upload_id")?;
            }
            "plan_sha" => {
                measurement.plan_sha = parse_upload_id_field(value, "measurement.plan_sha")?;
            }
            other => {
                return Err(CalibrationError::invalid_request(
                    &format!("measurement.{other}"),
                    format!(
                        "unknown field measurement.{other:?}: allowed fields are baseline_upload_id, applied_upload_id, plan_sha"
                    ),
                ));
            }
        }
    }
    Ok(measurement)
}

/// 校验一个上传 id 字段：格式合法且上传真实存在（不存在 404 upload_not_found）。
pub(crate) fn require_upload_id(
    uploads: &UploadStore,
    value: &Value,
    field: &str,
) -> Result<Option<String>, CalibrationError> {
    let Some(id) = parse_upload_id_field(value, field)? else {
        return Ok(None);
    };
    if uploads.lookup(&id)?.is_none() {
        return Err(CalibrationError::upload_not_found(&id).with_detail("field", json!(field)));
    }
    Ok(Some(id))
}

/// 合并后的整体一致性校验（min_bands <= centers_hz.len 等）。
fn validate_stored(stored: &WizardStored) -> Result<(), CalibrationError> {
    let effective = stored.effective();
    if effective.min_bands as usize > effective.centers_hz.len() {
        return Err(CalibrationError::invalid_request(
            "single_point.min_bands",
            format!(
                "min_bands ({}) must not exceed the number of centers_hz ({})",
                effective.min_bands,
                effective.centers_hz.len()
            ),
        ));
    }
    if stored.mic_cal_filename.is_some() && stored.mic_cal_sha256.is_none() {
        return Err(CalibrationError::invalid_request(
            "mic_cal_filename",
            "mic_cal_filename requires mic_cal_sha256: set the calibration file sha256 first",
        ));
    }
    Ok(())
}

/// GET 响应的 reasons：哪些字段还是默认值/未配置（如实说明，不含假数据）。
pub(crate) fn config_reasons(stored: &WizardStored) -> Map<String, Value> {
    let mut reasons = Map::new();
    if stored.mic_cal_sha256.is_none() {
        reasons.insert(
            "mic_cal_sha256".to_string(),
            json!("microphone calibration file is not configured: upload it with POST /api/calibration/uploads and PUT its sha256 here"),
        );
    }
    if stored.orientation.is_none() {
        reasons.insert(
            "orientation".to_string(),
            json!("microphone orientation is not configured: expected one of 0deg/90deg (UMIK convention)"),
        );
    }
    if stored.target_mode.is_none() {
        reasons.insert(
            "target_mode".to_string(),
            json!(format!("target_mode is not configured; the effective value is the daemon default {DEFAULT_TARGET_MODE:?} (designer BuiltinTarget)")),
        );
    }
    if stored.centers_hz.is_none() && stored.tolerance_db.is_none() && stored.min_bands.is_none() {
        reasons.insert(
            "single_point".to_string(),
            json!("single point parameters are not configured; the effective values are the offline closed-loop defaults (25 Hz - 20 kHz 1/3-oct centers, tolerance 2.0 dB, min_bands 3)"),
        );
    }
    reasons
}

// ---------------------------------------------------------------------------
// 向导状态推导
// ---------------------------------------------------------------------------

/// 步骤 5 的测量草稿（来自最近 job 或持久化草稿）。
#[derive(Debug, Clone, Default)]
pub struct MeasurementDraft {
    /// baseline 上传 id。
    pub baseline_upload_id: Option<String>,
    /// applied 上传 id。
    pub applied_upload_id: Option<String>,
    /// baseline 上传的 sha256（从 uploads 目录校验）。
    pub baseline_sha256: Option<String>,
    /// applied 上传的 sha256。
    pub applied_sha256: Option<String>,
}

/// 推导向导状态所需的真实输入。
pub struct WizardStateInputs<'a> {
    /// /api/status 的聚合对象（khaen-status/v1）。
    pub status: &'a Value,
    /// 控制面 status_json（snapshot 来源）。
    pub control_status: &'a Value,
    /// 校准服务状态（wired/active/last）。
    pub calibration: &'a CalibrationStatus,
    /// 最近一个 job（服务给出；未接线为 None）。
    pub last_job: Option<&'a JobRecord>,
    /// 向导配置（持久化草稿）。
    pub stored: &'a WizardStored,
    /// 测量草稿（来自最近 job 或持久化配置；None 表示都没有）。
    pub measurement: &'a MeasurementDraft,
    /// 当前时刻（RFC3339 UTC）。
    pub updated_at: &'a str,
}

/// 构造向导状态 JSON（GET /api/calibration/wizard/state）。
///
/// 语义（与前端 task-37 对齐）：
/// - `passed`：该步 gate 满足，可继续；
/// - `blocked`：用户可修复，下一步禁用并显示 reason；
/// - `not_available`：能力未接入（灰态 + reason + task），不阻塞向导。
pub(crate) fn derive_state(inputs: WizardStateInputs<'_>) -> Value {
    let mut steps = Vec::with_capacity(8);
    let mut reasons = Map::new();

    // 1. 设备检查：/api/status 的 device/format/host_link。
    let device_present = inputs.status["device"]["state"] == json!("present");
    let rate_asserted = inputs.status["format"]["rate_asserted"] == json!(true);
    let host_connected = inputs.status["host_link"]["connected"] == json!(true);
    let mut device_reasons: Vec<&str> = Vec::new();
    if !device_present {
        device_reasons.push("device.state is not \"present\"");
    }
    if !rate_asserted {
        device_reasons.push("format.rate_asserted is not true (device rate is not asserted)");
    }
    if !host_connected {
        device_reasons
            .push("host_link.connected is false (rt-host status is unavailable or stale)");
    }
    let device_step = step(
        1,
        "device_check",
        if device_reasons.is_empty() {
            "passed"
        } else {
            "blocked"
        },
        if device_reasons.is_empty() {
            Value::Null
        } else {
            json!(format!(
                "device check not satisfied: {}",
                device_reasons.join("; ")
            ))
        },
        Value::Null,
    );
    steps.push(device_step);

    // 2. 布局与声道：/api/channels 已实现（T55/T58/T61），本步只读展示 /api/status 的
    //    format/channels 事实；可读即 passed，不可读则 blocked（fail-closed，不写死 milestone）。
    let output_channels = inputs.status["format"]["channels"].as_u64();
    let channels_step = match output_channels {
        Some(channels) if channels > 0 => step(
            2,
            "layout_channels",
            "passed",
            json!(format!(
                "output format is readable from /api/status ({channels} channels); this wizard step is read-only. Live channel mapping is served by /api/channels and edited on the Plan & channels page"
            )),
            Value::Null,
        ),
        _ => step(
            2,
            "layout_channels",
            "blocked",
            json!("channel/format status is unavailable: /api/status format.channels is null, so the wizard cannot verify the output layout. Check rt-host status (or /api/channels) and retry"),
            Value::Null,
        ),
    };
    steps.push(channels_step);

    // 3. 麦克风与校准设置：向导配置真实字段。
    let mut missing: Vec<&str> = Vec::new();
    if inputs.stored.mic_cal_sha256.is_none() {
        missing.push("mic_cal_sha256");
    }
    if inputs.stored.orientation.is_none() {
        missing.push("orientation");
    }
    let mic_step = if missing.is_empty() {
        step(
            3,
            "mic_settings",
            "passed",
            if inputs.stored.target_mode.is_none() {
                json!(format!(
                    "orientation and microphone calibration file are saved; target_mode is not set and uses the default {:?}",
                    DEFAULT_TARGET_MODE
                ))
            } else {
                Value::Null
            },
            Value::Null,
        )
    } else {
        step(
            3,
            "mic_settings",
            "blocked",
            json!(format!(
                "wizard settings are incomplete: {} not configured",
                missing.join(", ")
            )),
            Value::Null,
        )
    };
    steps.push(mic_step);

    // 4. 测点：khaen 只支持单点（皇帝位）；多点/ART 是 NOT_AVAILABLE。
    steps.push(step(
        4,
        "positions",
        "passed",
        json!("single listening position (sweet spot) only: multi-point / ART measurement is NOT_AVAILABLE in khaen"),
        json!("P3"),
    ));

    // 5-8：job 后端未接线 → 如实 not_available（不阻塞向导）。
    if !inputs.calibration.wired {
        for (step_number, key, label) in [
            (5usize, "measurement", "job backend"),
            (6, "analysis", "job backend"),
            (7, "design", "job backend"),
            (8, "review_apply", "job backend"),
        ] {
            steps.push(step(
                step_number,
                key,
                "not_available",
                inputs
                    .calibration
                    .reason
                    .as_deref()
                    .map(|reason| json!(reason))
                    .unwrap_or_else(|| json!(format!("{label} is not wired"))),
                json!("T70h-b"),
            ));
        }
    } else {
        let last = inputs.last_job;
        steps.push(measurement_step(last, inputs.measurement));
        steps.push(analysis_step(last));
        steps.push(design_step(last));
        steps.push(review_step(last));
    }

    // current_step：第一个 blocked 的步骤（not_available 不阻塞）；没有 blocked 时停在 8。
    let current_step = steps
        .iter()
        .find(|entry| entry["status"] == json!("blocked"))
        .and_then(|entry| entry["step"].as_u64())
        .unwrap_or(8);

    let measurement_json = measurement_json(inputs.measurement);
    let snapshot = if inputs.calibration.wired {
        snapshot_json(inputs.control_status, &mut reasons)
    } else {
        reasons.insert(
            "snapshot".to_string(),
            json!("calibration backend is not wired: no applied plan snapshot is available"),
        );
        Value::Null
    };

    json!({
        "schema": WIZARD_STATE_SCHEMA,
        "current_step": current_step,
        "steps": steps,
        "active_job_id": inputs.calibration.active_job,
        "last_job_id": inputs.calibration.last_job_id,
        "measurement": measurement_json,
        "snapshot": snapshot,
        "updated_at": inputs.updated_at,
        "reasons": Value::Object(reasons),
    })
}

fn step(number: usize, key: &str, status: &str, reason: Value, task: Value) -> Value {
    json!({
        "step": number,
        "key": key,
        "status": status,
        "reason": reason,
        "task": task,
    })
}

fn measurement_step(last_job: Option<&JobRecord>, measurement: &MeasurementDraft) -> Value {
    let mut missing: Vec<&str> = Vec::new();
    if measurement.baseline_upload_id.is_none() {
        missing.push("baseline_upload_id");
    }
    if measurement.applied_upload_id.is_none() {
        missing.push("applied_upload_id");
    }
    match last_job {
        None if missing.is_empty() => step(
            5,
            "measurement",
            "passed",
            json!("offline measurement inputs are saved in the wizard draft; start the job with POST /api/calibration/jobs"),
            Value::Null,
        ),
        None => step(
            5,
            "measurement",
            "blocked",
            json!(format!(
                "offline measurement needs two uploaded WAV files; missing: {}",
                missing.join(", ")
            )),
            Value::Null,
        ),
        Some(job) if job.state == JobState::Succeeded => step(
            5,
            "measurement",
            "passed",
            json!(format!("measurement inputs were captured by job {}", job.job_id)),
            Value::Null,
        ),
        Some(job) => step(
            5,
            "measurement",
            "blocked",
            json!(format!(
                "job {} is {}: measurement is not finished",
                job.job_id,
                job.state.as_str()
            )),
            Value::Null,
        ),
    }
}

fn analysis_step(last_job: Option<&JobRecord>) -> Value {
    match last_job {
        None => step(
            6,
            "analysis",
            "blocked",
            json!("no calibration job yet: run a job before reviewing the analysis"),
            Value::Null,
        ),
        Some(job) if job.state == JobState::Succeeded => step(
            6,
            "analysis",
            "passed",
            json!(format!(
                "job {} succeeded with verdict {}",
                job.job_id,
                job.verdict.as_deref().unwrap_or("<not reported>")
            )),
            Value::Null,
        ),
        Some(job) => step(
            6,
            "analysis",
            "blocked",
            json!(format!(
                "job {} is {}: analysis requires a succeeded job",
                job.job_id,
                job.state.as_str()
            )),
            Value::Null,
        ),
    }
}

fn design_step(last_job: Option<&JobRecord>) -> Value {
    let plan = last_job
        .and_then(|job| job.result.as_ref())
        .and_then(|result| result.plan.as_ref());
    match (last_job, plan) {
        (Some(job), Some(plan)) => step(
            7,
            "design",
            "passed",
            json!(format!(
                "job {} produced plan {} (graph_id {})",
                job.job_id, plan.sha256, plan.graph_id
            )),
            Value::Null,
        ),
        (Some(job), None) => step(
            7,
            "design",
            "blocked",
            json!(format!(
                "job {} has no applicable plan artifact yet",
                job.job_id
            )),
            Value::Null,
        ),
        (None, _) => step(
            7,
            "design",
            "blocked",
            json!(
                "no calibration job yet: the design is produced by the closed-loop stage of a job"
            ),
            Value::Null,
        ),
    }
}

fn review_step(last_job: Option<&JobRecord>) -> Value {
    match last_job {
        None => step(
            8,
            "review_apply",
            "blocked",
            json!("no calibration job yet: nothing to review or apply"),
            Value::Null,
        ),
        Some(job) if job.apply.state == super::service::ApplyState::Applied => step(
            8,
            "review_apply",
            "passed",
            json!(format!(
                "job {} is applied at version {}",
                job.job_id,
                job.apply
                    .applied_version
                    .map(|version| version.to_string())
                    .unwrap_or_else(|| "<not reported>".to_string())
            )),
            Value::Null,
        ),
        Some(job) if job.state == JobState::Succeeded => step(
            8,
            "review_apply",
            "blocked",
            json!(format!(
                "job {} is succeeded but not applied: POST /api/calibration/jobs/{}/apply",
                job.job_id, job.job_id
            )),
            Value::Null,
        ),
        Some(job) => step(
            8,
            "review_apply",
            "blocked",
            json!(format!(
                "job {} is {}: only succeeded jobs can be applied",
                job.job_id,
                job.state.as_str()
            )),
            Value::Null,
        ),
    }
}

fn snapshot_json(control_status: &Value, reasons: &mut Map<String, Value>) -> Value {
    let Some(version) = control_status["actual_version"].as_u64() else {
        reasons.insert(
            "snapshot".to_string(),
            json!("no plan applied yet: the control plane reports no actual_version"),
        );
        return Value::Null;
    };
    let graph_id = control_status["graph_id"].clone();
    reasons.insert(
        "snapshot.sha256".to_string(),
        json!("the control plane status does not expose the running plan sha256; only version/graph_id are available"),
    );
    json!({
        "version": version,
        "graph_id": graph_id,
        "sha256": Value::Null,
    })
}

fn measurement_json(measurement: &MeasurementDraft) -> Value {
    json!({
        "baseline_upload_id": measurement.baseline_upload_id,
        "applied_upload_id": measurement.applied_upload_id,
        "baseline_sha256": measurement.baseline_sha256,
        "applied_sha256": measurement.applied_sha256,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "khaen-cal-wizard-{}-{tag}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
        path
    }

    #[test]
    fn defaults_are_the_frozen_offline_cli_values() {
        let stored = WizardStored::default();
        let effective = stored.effective();
        assert_eq!(effective.target_mode, "flat");
        assert_eq!(effective.centers_hz.len(), 30);
        assert_eq!(effective.centers_hz[0], 25.0);
        assert_eq!(effective.centers_hz[29], 20_000.0);
        assert_eq!(effective.tolerance_db, 2.0);
        assert_eq!(effective.min_bands, 3);
        assert!(effective.mic_cal_sha256.is_none());
        assert!(effective.orientation.is_none());
        let defaults = WizardStored::defaults_json();
        assert_eq!(defaults["target_mode"], json!("flat"));
        assert_eq!(defaults["single_point"]["min_bands"], json!(3));
    }

    #[test]
    fn patch_rejects_unknown_fields_and_bad_enums() {
        let dir = temp_dir("patch");
        let uploads = UploadStore::new(&dir);
        let current = WizardStored::default();
        for (payload, field) in [
            (json!({"nope": 1}), "nope"),
            (json!({"orientation": "free"}), "orientation"),
            (json!({"target_mode": "harman"}), "target_mode"),
            (json!({"single_point": {"bogus": 1}}), "single_point.bogus"),
            (json!({"measurement": {"bogus": "x"}}), "measurement.bogus"),
            (json!({"mic_cal_sha256": "not-a-sha"}), "mic_cal_sha256"),
            (json!({"mic_cal_filename": "a/b.wav"}), "mic_cal_filename"),
            (
                json!({"single_point": {"centers_hz": [1000.0, 100.0]}}),
                "single_point.centers_hz",
            ),
            (
                json!({"single_point": {"tolerance_db": 0.0}}),
                "single_point.tolerance_db",
            ),
            (
                json!({"single_point": {"min_bands": 0}}),
                "single_point.min_bands",
            ),
        ] {
            let error = apply_wizard_patch(&current, &uploads, &payload).expect_err("rejected");
            assert_eq!(
                error.code(),
                super::super::service::code::INVALID_REQUEST,
                "{payload}"
            );
            assert_eq!(
                error.details().get("field").and_then(Value::as_str),
                Some(field),
                "{payload}"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn patch_requires_referenced_uploads_to_exist() {
        let dir = temp_dir("uploads");
        let uploads = UploadStore::new(&dir);
        let current = WizardStored::default();
        let missing_id = "a".repeat(64);
        let error = apply_wizard_patch(&current, &uploads, &json!({"mic_cal_sha256": missing_id}))
            .expect_err("missing upload");
        assert_eq!(error.code(), super::super::service::code::UPLOAD_NOT_FOUND);
        let (record, _) = uploads.save(b"cal-file-bytes").expect("save");
        let outcome = apply_wizard_patch(
            &current,
            &uploads,
            &json!({
                "mic_cal_sha256": record.upload_id,
                "mic_cal_filename": "umik-1-cal.txt",
                "orientation": "90deg",
                "target_mode": "tilt10",
                "single_point": {"centers_hz": [100.0, 1000.0], "tolerance_db": 1.5, "min_bands": 2},
                "measurement": {"baseline_upload_id": record.upload_id, "applied_upload_id": record.upload_id},
            }),
        )
        .expect("valid patch");
        let effective = outcome.stored.effective();
        assert_eq!(effective.orientation.as_deref(), Some("90deg"));
        assert_eq!(effective.target_mode, "tilt10");
        assert_eq!(effective.centers_hz, vec![100.0, 1000.0]);
        assert_eq!(effective.tolerance_db, 1.5);
        assert_eq!(effective.min_bands, 2);
        assert_eq!(
            outcome.stored.measurement.baseline_upload_id,
            outcome.stored.mic_cal_sha256
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn store_roundtrips_and_reports_corruption() {
        let dir = temp_dir("store");
        let store = WizardStore::new(&dir);
        let load = store.load();
        assert!(!load.saved);
        assert!(load.corruption.is_none());
        let stored = WizardStored {
            mic_cal_sha256: Some("b".repeat(64)),
            mic_cal_filename: Some("cal.txt".into()),
            orientation: Some("90deg".into()),
            target_mode: Some("flat".into()),
            centers_hz: Some(vec![100.0, 1000.0]),
            tolerance_db: Some(2.5),
            min_bands: Some(2),
            measurement: WizardMeasurement {
                baseline_upload_id: Some("b".repeat(64)),
                applied_upload_id: None,
                plan_sha: None,
            },
        };
        let updated_at = store.save(&stored).expect("save");
        assert!(updated_at.ends_with('Z'));
        let load = store.load();
        assert!(load.saved);
        assert!(load.corruption.is_none());
        assert_eq!(load.stored, stored);
        assert_eq!(load.updated_at.as_deref(), Some(updated_at.as_str()));
        // 原子写后目录里没有临时文件。
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("read dir")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        std::fs::write(store.path(), b"{not json").expect("corrupt");
        let load = store.load();
        assert!(!load.saved);
        assert!(load.corruption.is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn state_marks_unwired_job_steps_not_available_and_derives_step1() {
        let status = json!({
            "device": {"state": "present"},
            "format": {"rate_asserted": true, "channels": 22},
            "host_link": {"connected": true},
        });
        let control = json!({"actual_version": Value::Null});
        let calibration = CalibrationStatus {
            wired: false,
            engine: None,
            active_job: None,
            last_job_id: None,
            last_verdict: None,
            reason: Some("not wired".into()),
        };
        let stored = WizardStored::default();
        let measurement = MeasurementDraft::default();
        let state = derive_state(WizardStateInputs {
            status: &status,
            control_status: &control,
            calibration: &calibration,
            last_job: None,
            stored: &stored,
            measurement: &measurement,
            updated_at: "2026-01-01T00:00:00Z",
        });
        assert_eq!(state["schema"], json!(WIZARD_STATE_SCHEMA));
        assert_eq!(state["steps"][0]["status"], json!("passed"));
        // 第 2 步：/api/channels 已实现，只读展示 status.format.channels → passed（无 task）。
        assert_eq!(state["steps"][1]["status"], json!("passed"));
        assert_eq!(state["steps"][1]["task"], Value::Null);
        assert_eq!(state["steps"][2]["status"], json!("blocked"));
        assert_eq!(state["current_step"], json!(3));
        for index in 4..8 {
            assert_eq!(state["steps"][index]["status"], json!("not_available"));
        }
        assert!(state["snapshot"].is_null());
        assert!(state["reasons"]["snapshot"].is_string());
    }

    #[test]
    fn layout_step_is_blocked_when_channel_status_is_missing() {
        let status = json!({
            "device": {"state": "present"},
            "format": {"rate_asserted": true, "channels": Value::Null},
            "host_link": {"connected": true},
        });
        let control = json!({"actual_version": Value::Null});
        let calibration = CalibrationStatus {
            wired: false,
            engine: None,
            active_job: None,
            last_job_id: None,
            last_verdict: None,
            reason: Some("not wired".into()),
        };
        let stored = WizardStored::default();
        let measurement = MeasurementDraft::default();
        let state = derive_state(WizardStateInputs {
            status: &status,
            control_status: &control,
            calibration: &calibration,
            last_job: None,
            stored: &stored,
            measurement: &measurement,
            updated_at: "2026-01-01T00:00:00Z",
        });
        assert_eq!(state["steps"][1]["status"], json!("blocked"));
        assert_eq!(state["steps"][1]["task"], Value::Null);
        assert!(state["steps"][1]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("format.channels")));
    }
}
