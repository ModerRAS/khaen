//! MOTU REST 层（T57）：`GET /api/motu` + `POST/PUT /api/motu/volume`（两阶段）+ /api/mute 副作用。
//!
//! 阻塞边界：检测（/proc 读 + TCP probe）与设备读写都是阻塞调用，全部放进 tokio blocking
//! 线程池；handler 里没有 await 期间的锁持有。
//!
//! 诚实语义：
//! - GET 恒 200：检测/设备读失败只降级成 null + `reasons`（未知 ≠ false：任务 panic 时
//!   detected=null，不冒充"未检测到"）。
//! - 音量 prepare 只校验 + 生成 token，不碰写帧（响应 `writes_device=false`）；commit 才逐通道
//!   单键写 + 回读。设备不可用 503（稳定码），设备写失败/回读不一致 502 + 设备错误码；
//!   部分成功时 `completed` 列出已验证的通道，绝不报告"全部成功"。
//! - 批量拒绝：一次请求超过两个主通道（或列表里点名其它总线）400——CueMix 设备对批量写敏感。
//! - /api/mute：target=process 时先做原控制面静音，再把同一次请求单键写 kMuteEnable[0]；
//!   设备写失败 502 且响应里保留 `control` 原回执（引擎静音成功 ≠ 设备静音成功）。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Map, Value};

use crate::system::error_code as sys_code;
use crate::DaemonState;

use super::{
    bus_label, db_to_fader, device_codes, error_code, fader_to_db, quantize_linear,
    DeviceDetection, MotuControl, MotuError, MotuReading, PendingVolumeDraft, WriteReceipt,
    CHANNEL_COUNT, MAX_JSON_BODY, PENDING_TTL_SECS, REASON_CARDS_UNAVAILABLE,
    REASON_CONTROL_UNREACHABLE, REASON_DETECTED, REASON_DISABLED, REASON_NO_MOTU_CARD,
    SCHEMA_STATUS, SCHEMA_VOLUME,
};

/// 组装 MOTU 路由（挂在全局 auth 中间件之下：会话 + 写方法 CSRF 自动生效）。
///
/// `/api/motu/volume` 同时注册 POST 与 PUT：T54 任务描述写的是 PUT，T57 定稿用 POST；
/// 两者走同一个 handler / 同一套两阶段语义，都算写方法（CSRF 强制）。
pub fn routes() -> Router<Arc<DaemonState>> {
    Router::new()
        .route("/api/motu", get(get_motu))
        .route("/api/motu/volume", post(volume).put(volume))
}

// ---------------------------------------------------------------------------
// GET /api/motu
// ---------------------------------------------------------------------------

/// GET /api/motu：检测对象 + mute/vol 现状（恒 200，拿不到的字段 null + reasons）。
pub(crate) async fn get_motu(State(state): State<Arc<DaemonState>>) -> Response {
    let control = Arc::clone(state.motu().control());
    let body = match tokio::task::spawn_blocking(move || compose_motu(&control)).await {
        Ok(body) => body,
        Err(join_error) => degraded_status_json(&format!("MOTU status task failed: {join_error}")),
    };
    (StatusCode::OK, Json(body)).into_response()
}

/// 合成 GET /api/motu 响应（同步；调用方在 blocking 线程池里）。
fn compose_motu(control: &Arc<dyn MotuControl>) -> Value {
    let detection = control.detection();
    let mut reasons: BTreeMap<String, String> = BTreeMap::new();
    let mut reading: Option<MotuReading> = None;
    let mut read_error: Value = Value::Null;
    if detection.is_ready() {
        match control.read() {
            Ok(state) => reading = Some(state),
            Err(error) => {
                let reason = format!(
                    "the MOTU control connection failed: {} ({})",
                    error.message, error.code
                );
                for path in ["mute", "vol", "state"] {
                    reasons.insert(path.to_string(), reason.clone());
                }
                read_error = error.to_json();
            }
        }
    } else {
        let reason = detection_reason_text(detection.reason);
        for path in ["mute", "vol", "state"] {
            reasons.insert(path.to_string(), reason.clone());
        }
    }
    let (mute, vol, state) = match &reading {
        Some(reading) => (
            json!(reading.mute),
            reading_vol_json(reading),
            reading_state_json(reading),
        ),
        None => (Value::Null, Value::Null, Value::Null),
    };
    json!({
        "schema": SCHEMA_STATUS,
        "detected": detection.detected,
        "model": &detection.model,
        "card_id": &detection.card_id,
        "card_index": detection.card_index,
        "control_reachable": detection.control_reachable,
        "reason": detection.reason,
        "reason_detail": detection_reason_text(detection.reason),
        "channel_count": CHANNEL_COUNT,
        "channels": channels_json(),
        "mute": mute,
        "vol": vol,
        "state": state,
        "read_error": read_error,
        "write": write_capability_json(&detection),
        "not_wired": [],
        "reasons": reasons_json(&reasons),
    })
}

/// 任务 panic 时的降级响应：detected=null（未知不冒充 false）+ 原因，仍 200。
fn degraded_status_json(reason: &str) -> Value {
    json!({
        "schema": SCHEMA_STATUS,
        "detected": Value::Null,
        "model": Value::Null,
        "card_id": Value::Null,
        "card_index": Value::Null,
        "control_reachable": Value::Null,
        "reason": Value::Null,
        "reason_detail": Value::Null,
        "channel_count": CHANNEL_COUNT,
        "channels": channels_json(),
        "mute": Value::Null,
        "vol": Value::Null,
        "state": Value::Null,
        "read_error": Value::Null,
        "write": {
            "status": "unknown",
            "single_key": true,
            "readback": true,
            "two_phase": true,
            "batch": false,
            "max_channels_per_request": CHANNEL_COUNT,
            "reason": reason,
        },
        "not_wired": [{"path": "motu", "reason": reason}],
        "reasons": {"motu": reason},
    })
}

/// 可寻址的主通道列表（channel 1 = Main 1 / koBusFader[0]）。
fn channels_json() -> Value {
    Value::Array(
        (0..CHANNEL_COUNT)
            .map(|index| {
                json!({
                    "channel": index + 1,
                    "name": channel_name(index + 1),
                    "datastore": {"key": "koBusFader", "index": index},
                })
            })
            .collect(),
    )
}

/// 每通道当前音量（linear + dB）；设备没上报的通道是 null。
fn reading_vol_json(reading: &MotuReading) -> Value {
    Value::Array(
        (0..CHANNEL_COUNT)
            .map(|index| {
                let linear = reading.main_faders.get(index as usize).copied().flatten();
                json!({
                    "channel": index + 1,
                    "name": channel_name(index + 1),
                    "datastore": {"key": "koBusFader", "index": index},
                    "linear": linear,
                    "db": linear.and_then(fader_to_db),
                })
            })
            .collect(),
    )
}

/// 设备状态细节（拿不到的字段 null，missing 如实列出）。
fn reading_state_json(reading: &MotuReading) -> Value {
    json!({
        "sample_rate_hz": reading.sample_rate_hz,
        "main_trim": reading.main_trim,
        "main_bus_mute": reading.main_bus_mute,
        "device": {
            "model_id": &reading.model_id,
            "name": &reading.device_name,
            "version": &reading.version,
            "api_version": reading.api_version,
        },
        "missing": reading.missing,
    })
}

/// 写能力上报（wired / unreachable / not_detected；batch 恒 false —— 设备对批量写敏感）。
fn write_capability_json(detection: &DeviceDetection) -> Value {
    let status = if detection.is_ready() {
        "wired"
    } else if detection.detected {
        "unreachable"
    } else {
        "not_detected"
    };
    let reason = if detection.is_ready() {
        Value::Null
    } else {
        Value::String(detection_reason_text(detection.reason))
    };
    json!({
        "status": status,
        "single_key": true,
        "readback": true,
        "two_phase": true,
        "batch": false,
        "max_channels_per_request": CHANNEL_COUNT,
        "reason": reason,
    })
}

/// 通道名（1 = Main 1；越界 null）。
fn channel_name(channel: u16) -> Value {
    match channel.checked_sub(1).and_then(bus_label) {
        Some(name) => Value::String(name.to_string()),
        None => Value::Null,
    }
}

fn reasons_json(reasons: &BTreeMap<String, String>) -> Value {
    let mut object = Map::new();
    for (path, reason) in reasons {
        object.insert(path.clone(), Value::String(reason.clone()));
    }
    Value::Object(object)
}

/// 检测原因的稳定文案（检测本身只给稳定码，这里补人类可读解释）。
fn detection_reason_text(reason: &str) -> String {
    match reason {
        REASON_DETECTED => {
            "MOTU control is available: a MOTU card is present and the USB-CDC control network answered the TCP probe"
                .to_string()
        }
        REASON_NO_MOTU_CARD => {
            "no MOTU card was found in /proc/asound/cards: MOTU control is unavailable on this host (this is not an error)"
                .to_string()
        }
        REASON_CARDS_UNAVAILABLE => {
            "the ALSA cards file (/proc/asound/cards) is not readable (e.g. Windows or no ALSA devices): MOTU control is unavailable, which is not an error"
                .to_string()
        }
        REASON_CONTROL_UNREACHABLE => {
            "a MOTU card is present but the USB-CDC control network did not answer the TCP probe: check the control network link (and the device control password configuration)"
                .to_string()
        }
        REASON_DISABLED => {
            "MOTU control is disabled by KHAEN_MOTU_DISABLE: detection and writes are off".to_string()
        }
        other => format!("unrecognized MOTU detection reason {other:?}"),
    }
}

/// 检测对象 JSON（GET 的扁平字段之外，详情/错误 details 复用同一份）。
fn detection_json(detection: &DeviceDetection) -> Value {
    json!({
        "detected": detection.detected,
        "model": &detection.model,
        "card_id": &detection.card_id,
        "card_index": detection.card_index,
        "control_reachable": detection.control_reachable,
        "reason": detection.reason,
        "reason_detail": detection_reason_text(detection.reason),
    })
}

// ---------------------------------------------------------------------------
// POST/PUT /api/motu/volume（两阶段）
// ---------------------------------------------------------------------------

/// 音量目标：主通道（1..=2）+ 8.24 量化后的线性值。
#[derive(Clone, Debug, PartialEq)]
struct VolumeTarget {
    channels: Vec<u16>,
    linear: f64,
    db: Option<f64>,
}

/// POST/PUT /api/motu/volume：`stage=prepare`（默认）| `stage=commit`。
pub(crate) async fn volume(State(state): State<Arc<DaemonState>>, body: Body) -> Response {
    let bytes = match axum::body::to_bytes(body, MAX_JSON_BODY).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return MotuApiError::invalid("<body>", format!("cannot read request body: {error}"))
                .into_response()
        }
    };
    let payload: Value = if bytes.is_empty() {
        Value::Null
    } else {
        match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(error) => {
                return MotuApiError::invalid(
                    "<body>",
                    format!("request body must be valid JSON: {error}"),
                )
                .into_response()
            }
        }
    };
    let Some(object) = payload.as_object() else {
        return MotuApiError::invalid("<body>", "request body must be a JSON object")
            .into_response();
    };
    let stage = match object.get("stage").or_else(|| object.get("action")) {
        None => "prepare",
        Some(Value::String(stage)) => stage.as_str(),
        Some(_) => {
            return MotuApiError::invalid("stage", "field stage must be a string (prepare|commit)")
                .into_response()
        }
    };
    match stage {
        "prepare" => {
            let object = object.clone();
            let worker = Arc::clone(&state);
            finish(tokio::task::spawn_blocking(move || prepare(&worker, &object)).await)
        }
        "commit" => {
            let object = object.clone();
            let worker = Arc::clone(&state);
            finish(tokio::task::spawn_blocking(move || commit(&worker, &object)).await)
        }
        other => MotuApiError::invalid(
            "stage",
            format!("stage must be prepare or commit (got {other:?})"),
        )
        .into_response(),
    }
}

/// 把 blocking 任务结果变成 HTTP 响应。
fn finish(result: Result<Result<Value, MotuApiError>, tokio::task::JoinError>) -> Response {
    match result {
        Ok(Ok(value)) => (StatusCode::OK, Json(value)).into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(join_error) => {
            MotuApiError::internal(format!("MOTU volume task failed: {join_error}")).into_response()
        }
    }
}

/// 第一阶段：校验请求 + 确认设备可用 + 存草稿（**不写设备**）。
fn prepare(state: &DaemonState, object: &Map<String, Value>) -> Result<Value, MotuApiError> {
    let target = parse_target(object)?;
    let control = Arc::clone(state.motu().control());
    let detection = control.detection();
    ensure_ready(&detection)?;
    let now = Instant::now();
    let created_at = crate::status::rfc3339_utc(SystemTime::now());
    let draft = state
        .motu()
        .put_draft(target.channels, target.linear, target.db, now, &created_at);
    Ok(json!({
        "ok": true,
        "schema": SCHEMA_VOLUME,
        "stage": "prepare",
        "engine_token": draft.token,
        "token": draft.token,
        "created_at": draft.created_at,
        "expires_in_s": PENDING_TTL_SECS,
        "target": target_json(&draft),
        "write_plan": write_plan_json(&draft.channels),
        "commit_supported": true,
        "two_phase": true,
        "writes_device": false,
        "applies_to": super::APPLIES_TO,
        "target_device": detection_json(&detection),
    }))
}

/// 第二阶段：token 校验通过后逐通道单键写 + 回读（一次 commit 消费草稿）。
fn commit(state: &DaemonState, object: &Map<String, Value>) -> Result<Value, MotuApiError> {
    let token = token_field(object)?;
    let control = Arc::clone(state.motu().control());
    let detection = control.detection();
    // 设备不可用时保留草稿（设备恢复后同一 token 还能 commit）。
    ensure_ready(&detection)?;
    let draft = state
        .motu()
        .take_draft(&token, Instant::now())
        .map_err(MotuApiError::token)?;
    let mut receipts: Vec<(u16, WriteReceipt<f64>)> = Vec::new();
    for channel in &draft.channels {
        let index = channel.saturating_sub(1);
        match control.write_volume(index, draft.linear) {
            Ok(receipt) => receipts.push((*channel, receipt)),
            Err(error) => {
                let extra = json!({
                    "channel": channel,
                    "requested_channels": draft.channels,
                    "requested_linear": draft.linear,
                    "completed": receipts_json(&receipts),
                    "writes_device": !receipts.is_empty(),
                    "draft_consumed": true,
                });
                return Err(MotuApiError::device(error, extra));
            }
        }
    }
    let idempotent = receipts.iter().all(|(_, receipt)| !receipt.frame_sent);
    Ok(json!({
        "ok": true,
        "schema": SCHEMA_VOLUME,
        "stage": "commit",
        "engine_token": draft.token,
        "applied": true,
        "idempotent": idempotent,
        "created_at": draft.created_at,
        "linear": draft.linear,
        "db": draft.db,
        "channels": receipts_json(&receipts),
        "write_plan": write_plan_json(&draft.channels),
        "writes_device": true,
        "applies_to": super::APPLIES_TO,
        "target_device": detection_json(&detection),
    }))
}

fn token_field(object: &Map<String, Value>) -> Result<String, MotuApiError> {
    match object.get("engine_token").or_else(|| object.get("token")) {
        Some(Value::String(token)) if !token.trim().is_empty() => Ok(token.trim().to_string()),
        Some(_) => Err(MotuApiError::invalid(
            "engine_token",
            "field engine_token must be a non-empty string",
        )),
        None => Err(MotuApiError::invalid(
            "engine_token",
            "missing field: engine_token (commit the token returned by stage=prepare)",
        )),
    }
}

/// 解析音量目标（结构/范围校验；写前完成，不碰设备）。
fn parse_target(object: &Map<String, Value>) -> Result<VolumeTarget, MotuApiError> {
    let linear = match (object.get("db"), object.get("linear")) {
        (Some(_), Some(_)) => {
            return Err(MotuApiError::invalid(
                "db",
                "give exactly one of db or linear, not both",
            ))
        }
        (None, None) => return Err(MotuApiError::invalid("db", "missing field: db or linear")),
        (Some(value), None) => {
            let db = value
                .as_f64()
                .ok_or_else(|| MotuApiError::invalid("db", "field db must be a finite number"))?;
            let converted =
                db_to_fader(db).map_err(|error| MotuApiError::invalid("db", error.to_string()))?;
            quantize_linear(converted)
                .map_err(|error| MotuApiError::invalid("db", error.to_string()))?
        }
        (None, Some(value)) => {
            let linear = value.as_f64().ok_or_else(|| {
                MotuApiError::invalid("linear", "field linear must be a finite number")
            })?;
            quantize_linear(linear)
                .map_err(|error| MotuApiError::invalid("linear", error.to_string()))?
        }
    };
    let channels = parse_channels(object)?;
    Ok(VolumeTarget {
        channels,
        linear,
        db: fader_to_db(linear),
    })
}

/// 解析通道选择：`channel`（1|2|"both"）或 `channels`（[1]、[2] 或 [1,2]）。
///
/// 批量拒绝：列表长度 > 2、或点名主通道以外的通道 -> 400（CueMix 设备对批量写敏感，
/// 这是硬约束；也不暴露其它总线）。
fn parse_channels(object: &Map<String, Value>) -> Result<Vec<u16>, MotuApiError> {
    match (object.get("channel"), object.get("channels")) {
        (Some(_), Some(_)) => Err(MotuApiError::invalid(
            "channel",
            "give exactly one of channel or channels, not both",
        )),
        (None, None) => Err(MotuApiError::invalid(
            "channel",
            "missing field: channel (1..=2 or \"both\") or channels ([1], [2] or [1,2])",
        )),
        (Some(value), None) => {
            if let Some(text) = value.as_str() {
                if text.trim().eq_ignore_ascii_case("both") {
                    return Ok((1..=CHANNEL_COUNT).collect());
                }
                return Err(MotuApiError::invalid(
                    "channel",
                    format!("field channel must be 1, 2 or \"both\" (got {text:?})"),
                ));
            }
            Ok(vec![single_channel(value, "channel")?])
        }
        (None, Some(value)) => {
            let Some(array) = value.as_array() else {
                return Err(MotuApiError::invalid(
                    "channels",
                    "field channels must be an array of main channel numbers",
                ));
            };
            if array.is_empty() {
                return Err(MotuApiError::invalid(
                    "channels",
                    "field channels must not be empty",
                ));
            }
            if array.len() > CHANNEL_COUNT as usize {
                return Err(MotuApiError::batch_rejected(
                    format!(
                        "refusing a batch MOTU volume write of {} channels: at most the two main channels (Main 1/Main 2) are addressable, each with its own single-key write and readback",
                        array.len()
                    ),
                    json!({"limit": CHANNEL_COUNT, "requested": array.len()}),
                ));
            }
            let mut channels: Vec<u16> = Vec::with_capacity(array.len());
            for (position, entry) in array.iter().enumerate() {
                let channel = single_channel(entry, &format!("channels[{position}]"))?;
                if channels.contains(&channel) {
                    return Err(MotuApiError::invalid(
                        "channels",
                        format!("channels lists channel {channel} more than once"),
                    ));
                }
                channels.push(channel);
            }
            channels.sort_unstable();
            Ok(channels)
        }
    }
}

fn single_channel(value: &Value, field: &str) -> Result<u16, MotuApiError> {
    let Some(number) = value.as_u64() else {
        return Err(MotuApiError::invalid(
            field,
            format!("field {field} must be a main channel number (1..={CHANNEL_COUNT})"),
        ));
    };
    if !(1..=CHANNEL_COUNT as u64).contains(&number) {
        return Err(MotuApiError::invalid(
            field,
            format!(
                "channel {number} is outside the addressable main channels 1..={CHANNEL_COUNT}: other CueMix buses are not exposed (bulk bus writes are rejected because the control link is sensitive to them)"
            ),
        ));
    }
    Ok(number as u16)
}

/// prepare/commit 共用的目标 JSON。
fn target_json(draft: &PendingVolumeDraft) -> Value {
    json!({
        "channels": Value::Array(
            draft
                .channels
                .iter()
                .map(|channel| {
                    let index = channel.saturating_sub(1);
                    json!({
                        "channel": channel,
                        "name": channel_name(*channel),
                        "datastore": {"key": "koBusFader", "index": index},
                    })
                })
                .collect()
        ),
        "linear": draft.linear,
        "db": draft.db,
    })
}

/// 写计划：逐通道单键写 + 回读（无批量）。
fn write_plan_json(channels: &[u16]) -> Value {
    json!({
        "kind": "single-key+readback",
        "batch": false,
        "writes_per_channel": 1,
        "writes": channels.len(),
        "steps": Value::Array(
            channels
                .iter()
                .map(|channel| {
                    json!({
                        "channel": channel,
                        "key": "koBusFader",
                        "index": channel.saturating_sub(1),
                        "frames": 1,
                    })
                })
                .collect()
        ),
        "verified_by": "device datastore readback: a write only counts as done when the device streams the same value back",
    })
}

/// 已验证写回执数组（channel/name 附加在设备回执之外）。
fn receipts_json(receipts: &[(u16, WriteReceipt<f64>)]) -> Value {
    Value::Array(
        receipts
            .iter()
            .map(|(channel, receipt)| {
                let mut value = receipt.to_json();
                if let Some(object) = value.as_object_mut() {
                    object.insert("channel".to_string(), json!(channel));
                    object.insert("name".to_string(), channel_name(*channel));
                }
                value
            })
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// 设备门禁与错误
// ---------------------------------------------------------------------------

/// 写前门禁：设备必须 detected 且控制网可达；否则 503 + 稳定码（不尝试写）。
fn ensure_ready(detection: &DeviceDetection) -> Result<(), MotuApiError> {
    if detection.is_ready() {
        return Ok(());
    }
    let code = if detection.detected {
        error_code::CONTROL_UNREACHABLE
    } else if detection.reason == REASON_DISABLED {
        error_code::CONTROL_DISABLED
    } else {
        error_code::NOT_DETECTED
    };
    Err(MotuApiError::gate(code, detection))
}

/// 设备错误码 -> HTTP 状态码（设备通信/回读失败 = 502；参数 = 400；未实现 = 501；其余 500）。
fn device_status(code: &str) -> StatusCode {
    if code == device_codes::NOT_DETECTED {
        StatusCode::SERVICE_UNAVAILABLE
    } else if code == device_codes::RANGE {
        StatusCode::BAD_REQUEST
    } else if code == device_codes::UNSUPPORTED {
        StatusCode::NOT_IMPLEMENTED
    } else if code == device_codes::IO
        || code == device_codes::TIMEOUT
        || code == device_codes::HANDSHAKE
        || code == device_codes::PROTOCOL
        || code == device_codes::CLOSED
        || code == device_codes::AUTH_REQUIRED
        || code == device_codes::AUTH_FAILED
        || code == device_codes::READBACK_TIMEOUT
        || code == device_codes::READBACK_MISMATCH
    {
        StatusCode::BAD_GATEWAY
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

/// MOTU 端点的业务错误（`{error:{code,message,details}}`，与 /api/channels 同形）。
#[derive(Debug)]
struct MotuApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    details: Value,
}

impl MotuApiError {
    fn invalid(field: &str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: sys_code::INVALID_REQUEST,
            message: message.into(),
            details: json!({"field": field}),
        }
    }

    fn batch_rejected(message: impl Into<String>, details: Value) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: error_code::BATCH_REJECTED,
            message: message.into(),
            details,
        }
    }

    fn gate(code: &'static str, detection: &DeviceDetection) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code,
            message: detection_reason_text(detection.reason),
            details: json!({
                "reason": detection.reason,
                "detection": detection_json(detection),
            }),
        }
    }

    fn token(error: super::MotuTokenError) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: error.code(),
            message: error.message().to_string(),
            details: json!({"reason": error.message()}),
        }
    }

    fn device(error: MotuError, extra: Value) -> Self {
        let status = device_status(error.code);
        let mut details = match extra {
            Value::Object(object) => object,
            _ => Map::new(),
        };
        details.insert("motu".to_string(), error.to_json());
        details.insert("phase".to_string(), json!("motu_volume_commit"));
        Self {
            status,
            code: error.code,
            message: error.message,
            details: Value::Object(details),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: sys_code::INTERNAL_ERROR,
            message: message.into(),
            details: Value::Null,
        }
    }

    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({
                "ok": false,
                "error": {
                    "code": self.code,
                    "message": self.message,
                    "details": self.details,
                }
            })),
        )
            .into_response()
    }
}

// ---------------------------------------------------------------------------
// /api/mute 的 MOTU 副作用
// ---------------------------------------------------------------------------

/// 副作用结果：设备不可用时跳过（如实上报），可用时写入并回读，失败时带上检测对象。
enum MuteEffect {
    /// 设备不可用：不写设备，200 + 原因。
    Skipped(DeviceDetection),
    /// 写入并回读成功。
    Written(DeviceDetection, WriteReceipt<bool>),
    /// 设备写入/回读失败（检测对象一并带回，便于响应说明设备状态）。
    Failed(DeviceDetection, Box<MotuError>),
}

fn apply_mute_effect(control: &Arc<dyn MotuControl>, mute: bool) -> MuteEffect {
    let detection = control.detection();
    if !detection.is_ready() {
        return MuteEffect::Skipped(detection);
    }
    match control.write_mute(mute) {
        Ok(receipt) => MuteEffect::Written(detection, receipt),
        Err(error) => MuteEffect::Failed(detection, Box::new(error)),
    }
}

/// /api/mute（target=process）的 MOTU 附加副作用。
///
/// 先由调用方完成原控制面静音（`control_body` 是它的回执），这里只追加设备写入：
/// - 设备不可用：原回执照常 200 + `motu` 说明未应用（不报错，Windows/无设备是常态）；
/// - 设备写入并回读成功：200 + `motu.write` 回执；
/// - 设备写入/回读失败：502（或内部 500）+ `error`，并把原控制面回执放在 `control` 里
///   —— 明确区分"引擎已静音"与"设备静音失败"，不假成功。
pub(crate) async fn with_mute_side_effect(
    state: &Arc<DaemonState>,
    mute: bool,
    control_body: Value,
) -> (StatusCode, Json<Value>) {
    let control = Arc::clone(state.motu().control());
    let result = tokio::task::spawn_blocking(move || apply_mute_effect(&control, mute)).await;
    match result {
        Ok(MuteEffect::Skipped(detection)) => (
            StatusCode::OK,
            Json(attach_motu(control_body, mute_motu_json(&detection, None))),
        ),
        Ok(MuteEffect::Written(detection, receipt)) => (
            StatusCode::OK,
            Json(attach_motu(
                control_body,
                mute_motu_json(&detection, Some(&receipt)),
            )),
        ),
        Ok(MuteEffect::Failed(detection, error)) => {
            let status = device_status(error.code);
            (
                status,
                Json(json!({
                    "ok": false,
                    "error": {
                        "code": error.code,
                        "message": format!(
                            "/api/mute was applied to the daemon control plane (see control) but the MOTU device write failed: {}",
                            error.message
                        ),
                        "details": {
                            "phase": "motu_mute",
                            "motu": detection_json(&detection),
                            "motu_error": error.to_json(),
                        },
                    },
                    "control": control_body,
                })),
            )
        }
        Err(join_error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "ok": false,
                "error": {
                    "code": sys_code::INTERNAL_ERROR,
                    "message": format!(
                        "/api/mute was applied to the daemon control plane (see control) but the MOTU side-effect task failed: {join_error}"
                    ),
                    "details": {"phase": "motu_mute"},
                },
                "control": control_body,
            })),
        ),
    }
}

/// 把 motu 对象挂到原控制面回执上（回执必须是 JSON object；不是就原样返回）。
fn attach_motu(mut body: Value, motu: Value) -> Value {
    if let Some(object) = body.as_object_mut() {
        object.insert("motu".to_string(), motu);
    }
    body
}

/// /api/mute 响应里的 MOTU 说明对象。
fn mute_motu_json(detection: &DeviceDetection, receipt: Option<&WriteReceipt<bool>>) -> Value {
    json!({
        "detected": detection.detected,
        "model": &detection.model,
        "control_reachable": detection.control_reachable,
        "reason": detection.reason,
        "reason_detail": detection_reason_text(detection.reason),
        "side_effect": true,
        "applied": receipt.is_some(),
        "write": receipt.map(WriteReceipt::to_json),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().expect("json object").clone()
    }

    fn channels_of(request: Value) -> Result<Vec<u16>, MotuApiError> {
        parse_channels(&object(request))
    }

    fn target_of(request: Value) -> Result<VolumeTarget, MotuApiError> {
        parse_target(&object(request))
    }

    #[test]
    fn target_from_db_is_quantized_and_reports_achieved_db() {
        let target = target_of(json!({"channel": 1, "db": -6.0206})).expect("valid");
        assert_eq!(target.channels, vec![1]);
        assert!(
            (target.linear - 0.5).abs() < 1e-4,
            "linear = {}",
            target.linear
        );
        let achieved = target.db.expect("db");
        assert!(
            (achieved - -6.020_599_913_3).abs() < 1e-3,
            "db = {achieved}"
        );
    }

    #[test]
    fn target_from_linear_is_quantized() {
        let target = target_of(json!({"channel": "both", "linear": 0.5})).expect("valid");
        assert_eq!(target.channels, vec![1, 2]);
        assert!((target.linear - 0.5).abs() < 1e-6);
    }

    #[test]
    fn target_rejects_bad_values() {
        for request in [
            json!({"channel": 1, "db": 3.0}),
            json!({"channel": 1, "db": -6.0, "linear": 0.5}),
            json!({"channel": 1}),
            json!({"channel": 1, "linear": 1.5}),
            json!({"channel": 1, "linear": -0.1}),
            json!({"channel": 1, "db": "loud"}),
            json!({"channel": 7, "db": -6.0}),
        ] {
            assert!(target_of(request.clone()).is_err(), "{request}");
        }
    }

    #[test]
    fn channel_selection_accepts_one_or_both_main_channels() {
        assert_eq!(channels_of(json!({"channel": 1})).expect("1"), vec![1]);
        assert_eq!(channels_of(json!({"channel": 2})).expect("2"), vec![2]);
        assert_eq!(
            channels_of(json!({"channel": "both"})).expect("both"),
            vec![1, 2]
        );
        assert_eq!(
            channels_of(json!({"channels": [2, 1]})).expect("sorted"),
            vec![1, 2]
        );
        assert_eq!(channels_of(json!({"channels": [2]})).expect("two"), vec![2]);
    }

    #[test]
    fn batch_writes_are_rejected_with_a_stable_code() {
        let error = channels_of(json!({"channels": [1, 2, 3]})).expect_err("batch rejected");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.code, error_code::BATCH_REJECTED);
        assert_eq!(error.details["limit"], json!(CHANNEL_COUNT));
        assert!(!error.message.is_empty());
    }

    #[test]
    fn channel_selection_rejects_ambiguous_or_duplicate_input() {
        for request in [
            json!({"channel": 1, "channels": [1]}),
            json!({}),
            json!({"channels": []}),
            json!({"channels": [1, 1]}),
            json!({"channels": [1, 3]}),
            json!({"channel": 0}),
            json!({"channel": 3}),
            json!({"channel": "left"}),
            json!({"channels": "1"}),
            json!({"channel": 1.5}),
        ] {
            assert!(channels_of(request.clone()).is_err(), "{request}");
        }
    }

    #[test]
    fn device_codes_map_to_honest_http_statuses() {
        assert_eq!(
            device_status(device_codes::NOT_DETECTED),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(device_status(device_codes::RANGE), StatusCode::BAD_REQUEST);
        assert_eq!(
            device_status(device_codes::UNSUPPORTED),
            StatusCode::NOT_IMPLEMENTED
        );
        for code in [
            device_codes::IO,
            device_codes::TIMEOUT,
            device_codes::HANDSHAKE,
            device_codes::PROTOCOL,
            device_codes::CLOSED,
            device_codes::AUTH_REQUIRED,
            device_codes::AUTH_FAILED,
            device_codes::READBACK_TIMEOUT,
            device_codes::READBACK_MISMATCH,
        ] {
            assert_eq!(device_status(code), StatusCode::BAD_GATEWAY, "{code}");
        }
        assert_eq!(
            device_status("E_SOMETHING_NEW"),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    fn ready_detection() -> DeviceDetection {
        DeviceDetection {
            detected: true,
            card_id: Some("UltraLitemk5".to_string()),
            card_index: Some(0),
            model: Some("UltraLite-mk5".to_string()),
            control_reachable: true,
            reason: REASON_DETECTED,
        }
    }

    fn absent_detection(reason: &'static str) -> DeviceDetection {
        DeviceDetection {
            detected: false,
            card_id: None,
            card_index: None,
            model: None,
            control_reachable: false,
            reason,
        }
    }

    #[test]
    fn gate_refuses_writes_without_a_usable_device() {
        assert!(ensure_ready(&ready_detection()).is_ok());

        let error = ensure_ready(&absent_detection(REASON_CARDS_UNAVAILABLE)).expect_err("absent");
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.code, error_code::NOT_DETECTED);
        assert_eq!(error.details["reason"], json!(REASON_CARDS_UNAVAILABLE));

        let error = ensure_ready(&absent_detection(REASON_NO_MOTU_CARD)).expect_err("no card");
        assert_eq!(error.code, error_code::NOT_DETECTED);

        let mut unreachable = ready_detection();
        unreachable.control_reachable = false;
        unreachable.reason = REASON_CONTROL_UNREACHABLE;
        let error = ensure_ready(&unreachable).expect_err("link");
        assert_eq!(error.code, error_code::CONTROL_UNREACHABLE);

        let error = ensure_ready(&absent_detection(REASON_DISABLED)).expect_err("disabled");
        assert_eq!(error.code, error_code::CONTROL_DISABLED);
    }

    #[test]
    fn detection_reason_text_is_stable_and_explains_each_code() {
        for reason in [
            REASON_DETECTED,
            REASON_NO_MOTU_CARD,
            REASON_CARDS_UNAVAILABLE,
            REASON_CONTROL_UNREACHABLE,
            REASON_DISABLED,
        ] {
            let text = detection_reason_text(reason);
            assert!(text.len() > 20, "{reason}: {text}");
            assert_ne!(text, detection_reason_text("E_OTHER_REASON"));
        }
        assert!(detection_reason_text("E_OTHER_REASON").contains("E_OTHER_REASON"));
    }

    #[test]
    fn write_capability_reports_wired_unreachable_and_not_detected() {
        assert_eq!(
            write_capability_json(&ready_detection())["status"],
            json!("wired")
        );
        let mut unreachable = ready_detection();
        unreachable.control_reachable = false;
        unreachable.reason = REASON_CONTROL_UNREACHABLE;
        assert_eq!(
            write_capability_json(&unreachable)["status"],
            json!("unreachable")
        );
        assert_eq!(
            write_capability_json(&absent_detection(REASON_NO_MOTU_CARD))["status"],
            json!("not_detected")
        );
        let capability = write_capability_json(&ready_detection());
        assert_eq!(capability["single_key"], json!(true));
        assert_eq!(capability["two_phase"], json!(true));
        assert_eq!(capability["batch"], json!(false));
        assert_eq!(capability["reason"], Value::Null);
    }

    #[test]
    fn receipts_report_channel_name_and_verified_readback() {
        let receipt = WriteReceipt {
            key: "koBusFader",
            index: 0,
            before: Some(1.0),
            wrote: 0.5,
            readback: 0.5,
            changed: true,
            frame_sent: true,
        };
        let json = receipts_json(&[(1, receipt)]);
        assert_eq!(json[0]["channel"], json!(1));
        assert_eq!(json[0]["name"], json!("Main 1"));
        assert_eq!(json[0]["verified"], json!(true));
        assert_eq!(json[0]["frame_sent"], json!(true));
    }

    #[test]
    fn mute_side_effect_json_is_explicit_about_applied_state() {
        let skipped = mute_motu_json(&absent_detection(REASON_CARDS_UNAVAILABLE), None);
        assert_eq!(skipped["applied"], json!(false));
        assert_eq!(skipped["write"], Value::Null);
        assert_eq!(skipped["reason"], json!(REASON_CARDS_UNAVAILABLE));

        let receipt = WriteReceipt {
            key: "kMuteEnable",
            index: 0,
            before: Some(false),
            wrote: true,
            readback: true,
            changed: true,
            frame_sent: true,
        };
        let written = mute_motu_json(&ready_detection(), Some(&receipt));
        assert_eq!(written["applied"], json!(true));
        assert_eq!(written["write"]["wrote"], json!(true));
        assert_eq!(written["write"]["verified"], json!(true));
    }

    /// GET 合成用替身：可编排检测与读取结果，不碰网络。
    #[derive(Debug)]
    struct ComposedControl {
        detection: DeviceDetection,
        reading: Result<MotuReading, MotuError>,
        reads: std::sync::atomic::AtomicUsize,
    }

    impl ComposedControl {
        fn new(detection: DeviceDetection, reading: Result<MotuReading, MotuError>) -> Arc<Self> {
            Arc::new(Self {
                detection,
                reading,
                reads: std::sync::atomic::AtomicUsize::new(0),
            })
        }

        /// 交给 compose_motu 的 trait 对象（保留具体句柄读计数）。
        fn as_control(self: &Arc<Self>) -> Arc<dyn MotuControl> {
            let concrete: Arc<Self> = Arc::clone(self);
            let control: Arc<dyn MotuControl> = concrete;
            control
        }
    }

    impl MotuControl for ComposedControl {
        fn detection(&self) -> DeviceDetection {
            self.detection.clone()
        }
        fn read(&self) -> Result<MotuReading, MotuError> {
            self.reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.reading.clone()
        }
        fn write_mute(&self, _mute: bool) -> Result<WriteReceipt<bool>, MotuError> {
            Err(MotuError::new(device_codes::UNSUPPORTED, "not used"))
        }
        fn write_volume(
            &self,
            _channel_index: u16,
            _linear: f64,
        ) -> Result<WriteReceipt<f64>, MotuError> {
            Err(MotuError::new(device_codes::UNSUPPORTED, "not used"))
        }
    }

    #[test]
    fn status_composition_reports_real_state_when_the_device_answers() {
        let reading = MotuReading {
            model_id: Some("UL5L".to_string()),
            device_name: Some("UltraLite-mk5".to_string()),
            version: Some("1.0.0".to_string()),
            api_version: Some(10),
            mute: Some(true),
            sample_rate_hz: Some(48_000),
            main_faders: vec![Some(1.0), Some(0.5)],
            main_trim: Some(31),
            main_bus_mute: Some(false),
            missing: Vec::new(),
        };
        let control = ComposedControl::new(ready_detection(), Ok(reading));
        let body = compose_motu(&control.as_control());
        assert_eq!(body["detected"], json!(true));
        assert_eq!(body["model"], json!("UltraLite-mk5"));
        assert_eq!(body["control_reachable"], json!(true));
        assert_eq!(body["mute"], json!(true));
        assert_eq!(body["vol"][0]["linear"], json!(1.0));
        assert_eq!(body["vol"][1]["linear"], json!(0.5));
        assert!((body["vol"][1]["db"].as_f64().expect("db") - -6.0206).abs() < 1e-3);
        assert_eq!(body["state"]["sample_rate_hz"], json!(48_000));
        assert_eq!(body["state"]["device"]["model_id"], json!("UL5L"));
        assert_eq!(body["reasons"], json!({}));
        assert_eq!(body["write"]["status"], json!("wired"));
    }

    #[test]
    fn status_composition_degrades_to_null_and_reasons() {
        let control = ComposedControl::new(
            absent_detection(REASON_CARDS_UNAVAILABLE),
            Err(MotuError::new(device_codes::NOT_DETECTED, "no device")),
        );
        let body = compose_motu(&control.as_control());
        assert_eq!(body["detected"], json!(false));
        assert_eq!(body["mute"], Value::Null);
        assert_eq!(body["vol"], Value::Null);
        assert_eq!(body["state"], Value::Null);
        assert_eq!(
            body["reasons"]["state"],
            json!(detection_reason_text(REASON_CARDS_UNAVAILABLE))
        );
        assert_eq!(
            control.reads.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "not detected -> the control connection is never attempted"
        );

        let control = ComposedControl::new(
            ready_detection(),
            Err(MotuError::new(device_codes::TIMEOUT, "readback timed out")),
        );
        let body = compose_motu(&control.as_control());
        assert_eq!(body["detected"], json!(true));
        assert_eq!(body["mute"], Value::Null);
        assert_eq!(body["read_error"]["code"], json!(device_codes::TIMEOUT));
        assert!(body["reasons"]["state"]
            .as_str()
            .expect("reason")
            .contains("readback timed out"));
    }

    #[test]
    fn degraded_status_does_not_claim_a_detection_result() {
        let body = degraded_status_json("task failed");
        assert_eq!(body["detected"], Value::Null);
        assert_eq!(body["control_reachable"], Value::Null);
        assert_eq!(body["not_wired"][0]["path"], json!("motu"));
        assert_eq!(body["write"]["status"], json!("unknown"));
    }
}
