//! /api/audio-profile 与 /api/device/inputs 的 REST 层（docs/API.md §12.2）。
//!
//! - GET /api/audio-profile：read-only 永远 200。已保存 profile（首次运行默认 2.0）+ validation +
//!   capability（AES67 从 rt-host channels_status 只读推导；声卡输入按现有 MOTU 只读控制面探测）+
//!   plan 摘要（lanes / enabled_outputs / dac_channels / graph_id）。拿不到的字段 null + reasons。
//! - PUT /api/audio-profile：两阶段 prepare（校验+diff+一次性 token，不碰引擎）/ commit（原子落盘+
//!   revision）/ abort（丢弃草稿）。结构性错误 400 bad_request（details.validation）。
//! - GET /api/device/inputs：声卡采集通道能力。ALSA capture 设备枚举已实现（`alsa_capture`
//!   加法字段），但 ALSA 不给逐通道 analog/optical 角色，所以 `available:false` + reason，
//!   不编通道列表；采样率能只读读到就如实给。
//!
//! 全是控制面路径（非实时路径）：socket / 设备 IO 都在 tokio blocking 线程池里做。

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

use crate::api_error::{read_json_body, ApiError};
use crate::host_link::{LinkSnapshot, Target, WireReply};
use crate::{status, DaemonState};

use super::model::{AudioProfile, DEVICE_CHANNELS, MAX_DEVICE_CHANNELS, SCHEMA};
use super::store::CurrentProfile;

/// 组装 audio-profile / device-inputs 路由（挂在全局 auth 中间件之下）。
pub fn routes() -> Router<Arc<DaemonState>> {
    Router::new()
        .route("/api/audio-profile", get(get_profile).put(put_profile))
        .route(
            "/api/audio-profile/plan",
            post(super::plan::generate_plan_endpoint),
        )
        .route("/api/device/inputs", get(get_device_inputs))
}

// ---------------------------------------------------------------------------
// GET /api/audio-profile
// ---------------------------------------------------------------------------

/// GET /api/audio-profile：read-only，永远 200（聚合任务 panic 也只降级不 500）。
pub(crate) async fn get_profile(State(state): State<Arc<DaemonState>>) -> Response {
    let worker = Arc::clone(&state);
    let body = match tokio::task::spawn_blocking(move || compose_profile_snapshot(&worker)).await {
        Ok(body) => body,
        Err(join_error) => {
            degraded_profile_snapshot(&format!("audio profile snapshot task failed: {join_error}"))
        }
    };
    (StatusCode::OK, Json(body)).into_response()
}

/// rt-host 当前真实输出通道数（status.format.channels；1..=22）。
///
/// 首次运行默认 profile 不能写死 22：MOTU 是 22，但通用 ALSA 设备可能是 2/6/8 等。
/// 只读 control status；process 目标/链路不可用/字段缺失时给 None + reasons，调用方退回默认 22。
pub(crate) fn live_output_channels(
    state: &DaemonState,
    reasons: &mut BTreeMap<String, String>,
) -> Option<u16> {
    if state.host_link().target() == Target::Process {
        reasons.insert(
            "profile.device.channels".to_string(),
            "KHAEN_RT_TARGET=process: the process-internal engine does not report the real output device channel count; the default profile uses the MOTU default 22".to_string(),
        );
        return None;
    }
    match state.host_link().status_snapshot() {
        LinkSnapshot::Socket { status, .. } => {
            let raw = status
                .get("format")
                .and_then(|format| format.get("channels"))
                .and_then(Value::as_u64);
            match raw {
                Some(channels) if (1..=u64::from(MAX_DEVICE_CHANNELS)).contains(&channels) => {
                    Some(channels as u16)
                }
                Some(channels) => {
                    reasons.insert(
                        "profile.device.channels".to_string(),
                        format!(
                            "rt-host reports {channels} output channels, outside the plan ABI range 1..={MAX_DEVICE_CHANNELS}; the default profile uses {DEVICE_CHANNELS}"
                        ),
                    );
                    None
                }
                None => {
                    reasons.insert(
                        "profile.device.channels".to_string(),
                        "the rt-host control status does not report format.channels; the default profile uses the MOTU default 22 instead of a fabricated count".to_string(),
                    );
                    None
                }
            }
        }
        LinkSnapshot::Unavailable { reason, .. } => {
            reasons.insert(
                "profile.device.channels".to_string(),
                format!("cannot read the rt-host output channel count: {reason}; the default profile uses the MOTU default 22"),
            );
            None
        }
        LinkSnapshot::Process => None,
    }
}

/// 合成 GET 快照（同步；调用方在 blocking 线程池里）。
pub(crate) fn compose_profile_snapshot(state: &DaemonState) -> Value {
    let mut reasons: BTreeMap<String, String> = BTreeMap::new();
    let live_channels = live_output_channels(state, &mut reasons);
    let current = state.audio_profile().current();
    if let Some(reason) = current.unreadable_reason() {
        reasons.insert("profile".to_string(), reason.to_string());
    }
    let (profile, saved, revision) = match &current {
        CurrentProfile::Default { revision } => {
            // 首次运行默认布局仍 2.0，但 device.channels 跟随 rt-host 当前真实输出通道数；
            // 读不到时退回 MOTU 默认 22，并在 reasons 里说明来源。
            (
                Some(AudioProfile::for_device_channels(
                    live_channels.unwrap_or(DEVICE_CHANNELS),
                )),
                false,
                Some(*revision),
            )
        }
        CurrentProfile::Saved(saved) => (Some(saved.profile.clone()), true, Some(saved.revision)),
        CurrentProfile::Unreadable { .. } => (None, false, None),
    };
    let (profile_json, validation_json, plan_json) = match &profile {
        Some(profile) => {
            let validation = profile.validate();
            let lanes = profile.enabled_lanes().len();
            let graph_id = graph_id_probe(state, &mut reasons);
            (
                profile.to_json(),
                validation.to_json(),
                json!({
                    "lanes": lanes,
                    "enabled_outputs": lanes,
                    "dac_channels": profile.device_channels,
                    "graph_id": graph_id,
                }),
            )
        }
        None => (
            Value::Null,
            Value::Null,
            json!({
                "lanes": Value::Null,
                "enabled_outputs": Value::Null,
                "dac_channels": Value::Null,
                "graph_id": Value::Null,
            }),
        ),
    };
    let aes67 = aes67_capability(state, &mut reasons);
    let device_inputs = device_inputs_probe(state);
    json!({
        "ok": true,
        "schema": SCHEMA,
        "profile": profile_json,
        "validation": validation_json,
        "capability": {
            "aes67": aes67,
            "device_inputs": device_inputs,
        },
        "plan": plan_json,
        "revision": revision,
        "saved": saved,
        "reasons": reasons_json(&reasons),
    })
}

/// 聚合任务 panic 时的降级快照：全部 null + 原因，仍 200。
fn degraded_profile_snapshot(reason: &str) -> Value {
    json!({
        "ok": true,
        "schema": SCHEMA,
        "profile": Value::Null,
        "validation": Value::Null,
        "capability": {
            "aes67": {
                "channels": Value::Null,
                "map": Value::Null,
                "channel_order": Value::Null,
                "channel_order_source": Value::Null,
            },
            "device_inputs": {
                "available": Value::Null,
                "optical_mode": Value::Null,
                "sample_rate": Value::Null,
                "channels": Value::Null,
                "source": "motu-alsa",
                "reason": reason,
            },
        },
        "plan": {
            "lanes": Value::Null,
            "enabled_outputs": Value::Null,
            "dac_channels": Value::Null,
            "graph_id": Value::Null,
        },
        "revision": Value::Null,
        "saved": Value::Null,
        "reasons": { "profile": reason, "capability": reason },
    })
}

/// rt-host 当前生效方案的 graph_id（socket status 只读；拿不到 null + 原因）。
fn graph_id_probe(state: &DaemonState, reasons: &mut BTreeMap<String, String>) -> Value {
    let snapshot = state.host_link().status_snapshot();
    match snapshot {
        LinkSnapshot::Socket { status, .. } => match status.get("graph_id") {
            Some(Value::String(graph_id)) if !graph_id.is_empty() => {
                Value::String(graph_id.clone())
            }
            _ => {
                reasons.insert(
                    "plan.graph_id".to_string(),
                    "the rt-host control status does not report an applied plan graph_id (no plan committed yet, or an rt-host build without plan reporting)".to_string(),
                );
                Value::Null
            }
        },
        LinkSnapshot::Unavailable { reason, .. } => {
            reasons.insert(
                "plan.graph_id".to_string(),
                format!("cannot read the applied plan graph_id: {reason}"),
            );
            Value::Null
        }
        LinkSnapshot::Process => {
            reasons.insert(
                "plan.graph_id".to_string(),
                "KHAEN_RT_TARGET=process: the daemon process-internal engine is not the rt-host data plane; the applied plan graph_id is not reported".to_string(),
            );
            Value::Null
        }
    }
}

/// AES67 能力：只读推导自 rt-host channels_status 的 current（没有 SDP 解析来源，
/// channel_order / channel_order_source 如实给 null / none + 原因）。
fn aes67_capability(state: &DaemonState, reasons: &mut BTreeMap<String, String>) -> Value {
    let mut null_capability = json!({
        "channels": Value::Null,
        "map": Value::Null,
        "channel_order": Value::Null,
        "channel_order_source": Value::Null,
    });
    if state.host_link().target() == Target::Process {
        reasons.insert(
            "capability.aes67".to_string(),
            "KHAEN_RT_TARGET=process: the daemon process-internal engine has no AES67 receiver; AES67 capability is not reported".to_string(),
        );
        return null_capability;
    }
    let reply = state.host_link().call(json!({ "op": "channels_status" }));
    match reply {
        Ok(WireReply::Success(body)) => {
            let current = body.get("current");
            let channels = current
                .and_then(|current| current.get("aes67_channels"))
                .and_then(Value::as_u64);
            let map = current
                .and_then(|current| current.get("aes67_map"))
                .filter(|value| value.is_array())
                .cloned();
            if channels.is_none() {
                reasons.insert(
                    "capability.aes67.channels".to_string(),
                    "the rt-host channels status answered but does not report current.aes67_channels".to_string(),
                );
            }
            if map.is_none() {
                reasons.insert(
                    "capability.aes67.map".to_string(),
                    "the rt-host channels status answered but does not report current.aes67_map"
                        .to_string(),
                );
            }
            reasons.insert(
                "capability.aes67.channel_order".to_string(),
                "the daemon has no SDP parser (NSP channel-order= descriptors are not captured) and the rt-host channels status does not report the AES67 channel order; it is not fabricated".to_string(),
            );
            json!({
                "channels": channels,
                "map": map,
                "channel_order": Value::Null,
                "channel_order_source": "none",
            })
        }
        Ok(WireReply::Failure { code, message, .. }) => {
            reasons.insert(
                "capability.aes67".to_string(),
                format!("the rt-host channels status rejected the probe ({code}: {message}); AES67 capability is not reported"),
            );
            null_capability["channel_order_source"] = Value::Null;
            null_capability
        }
        Err(error) => {
            reasons.insert("capability.aes67".to_string(), error.message);
            null_capability
        }
    }
}

/// 声卡输入能力探测（现有 MOTU 只读控制面 + ALSA capture 设备枚举）。
///
/// `available` 仍为 false：ALSA 只暴露设备级 capture PCM，逐通道 analog/optical 角色没有
/// 可靠元数据源；因此不编通道表，只给 `alsa_capture` 真实枚举 + reason。
pub(crate) fn device_inputs_probe(state: &DaemonState) -> Value {
    let control = Arc::clone(state.motu().control());
    let detection = control.detection();
    let mut sample_rate = Value::Null;
    let mut detail;
    if detection.is_ready() {
        match control.read() {
            Ok(reading) => {
                sample_rate = reading
                    .sample_rate_hz
                    .map(|rate| json!(rate))
                    .unwrap_or(Value::Null);
                detail = "the MOTU control surface is reachable and its sample rate was read"
                    .to_string();
                if sample_rate.is_null() {
                    detail.push_str(
                        "; the control surface does not report kSampleRate, so sample_rate is null",
                    );
                }
            }
            Err(error) => {
                detail = format!(
                    "the MOTU control connection failed: {} ({})",
                    error.message, error.code
                );
            }
        }
    } else {
        detail = format!(
            "the MOTU control surface is not available: {}",
            crate::motu::api::detection_reason_text(detection.reason)
        );
    }
    // ALSA capture 设备枚举（加法字段）：真实设备名/方向/通道范围；逐通道 analog/optical
    // 角色 ALSA 不暴露，所以 channels 仍保持 null + 原因，不把设备名猜成通道角色。
    let (alsa_capture, capture_detail) = match crate::audio_profile::capture::enumerate_capture() {
        Ok(devices) => {
            let count = devices.len();
            let note = if count == 0 {
                "ALSA capture enumeration succeeded but reported no capture PCM devices".to_string()
            } else {
                format!(
                    "ALSA capture enumeration reported {count} capture PCM device(s); per-channel analog/optical roles are not exposed by ALSA, so the input channel list is not fabricated"
                )
            };
            (crate::audio_profile::capture::devices_json(&devices), note)
        }
        Err(error) => (
            Value::Null,
            format!("ALSA capture enumeration is unavailable: {error}"),
        ),
    };
    let reason = format!(
        "the daemon can enumerate sound-card capture devices but cannot map each channel to analog/optical without a device-specific metadata source; {capture_detail}. {detail}"
    );
    json!({
        "available": false,
        "optical_mode": Value::Null,
        "sample_rate": sample_rate,
        "channels": Value::Null,
        "source": "motu-alsa",
        "alsa_capture": alsa_capture,
        "reason": reason,
    })
}

/// GET /api/device/inputs：read-only，永远 200；拿不到 available:false + reason。
pub(crate) async fn get_device_inputs(State(state): State<Arc<DaemonState>>) -> Response {
    let worker = Arc::clone(&state);
    let capability = match tokio::task::spawn_blocking(move || device_inputs_probe(&worker)).await {
        Ok(capability) => capability,
        Err(join_error) => json!({
            "available": false,
            "optical_mode": Value::Null,
            "sample_rate": Value::Null,
            "channels": Value::Null,
            "source": "motu-alsa",
            "reason": format!("device input probe task failed: {join_error}"),
        }),
    };
    let mut body = capability;
    if let Some(object) = body.as_object_mut() {
        object.insert("ok".to_string(), Value::Bool(true));
    }
    (StatusCode::OK, Json(body)).into_response()
}

// ---------------------------------------------------------------------------
// PUT /api/audio-profile（两阶段）
// ---------------------------------------------------------------------------

/// PUT /api/audio-profile：stage=prepare|commit|abort。
pub(crate) async fn put_profile(State(state): State<Arc<DaemonState>>, body: Body) -> Response {
    let object = match read_json_body(body).await {
        Ok(object) => object,
        Err(error) => return error.into_response(),
    };
    let stage = match object.get("stage") {
        Some(Value::String(stage)) => stage.clone(),
        Some(_) => {
            return ApiError::invalid_field(
                "stage",
                "field stage must be a string (prepare|commit|abort)",
            )
            .into_response()
        }
        None => {
            return ApiError::invalid_field("stage", "missing field: stage (prepare|commit|abort)")
                .into_response()
        }
    };
    match stage.as_str() {
        "prepare" => prepare_profile(state, object).await,
        "commit" => commit_profile(state, object).await,
        "abort" => abort_profile(state, object).await,
        other => ApiError::invalid_field(
            "stage",
            format!("stage must be prepare, commit or abort (got {other:?})"),
        )
        .into_response(),
    }
}

/// 第一阶段：解析 + 校验 + diff + 一次性 token（不碰引擎、不写盘）。
async fn prepare_profile(state: Arc<DaemonState>, object: Map<String, Value>) -> Response {
    let Some(profile_value) = object.get("profile") else {
        return ApiError::invalid_field(
            "profile",
            "missing field: profile (the full profile object)",
        )
        .into_response();
    };
    let profile = match AudioProfile::parse(profile_value) {
        Ok(profile) => profile,
        Err(error) => return ApiError::invalid_field(&error.field, error.message).into_response(),
    };
    let validation = profile.validate();
    if !validation.ok() {
        return ApiError::bad_request(
            "the audio profile is structurally invalid: see details.validation (only four structural error classes exist in docs/API.md 12.1)",
        )
        .with_detail("validation", validation.to_json())
        .into_response();
    }
    let now_rfc = status::rfc3339_utc(SystemTime::now());
    let worker = Arc::clone(&state);
    let result = tokio::task::spawn_blocking(move || {
        let current = worker.audio_profile().current();
        let mut reasons = Map::new();
        let baseline = match &current {
            CurrentProfile::Default { .. } => AudioProfile::default(),
            CurrentProfile::Saved(saved) => saved.profile.clone(),
            CurrentProfile::Unreadable { reason } => {
                reasons.insert("baseline".to_string(), json!(reason));
                AudioProfile::default()
            }
        };
        let diff = baseline.diff(&profile);
        let draft =
            worker
                .audio_profile()
                .put_draft(profile.clone(), diff, Instant::now(), &now_rfc);
        (profile, diff, draft, reasons)
    })
    .await;
    let (profile, diff, draft, mut reasons) = match result {
        Ok(result) => result,
        Err(join_error) => {
            return ApiError::internal(format!("audio profile prepare task failed: {join_error}"))
                .into_response()
        }
    };
    reasons.insert("engine_touched".to_string(), json!(false));
    let body = json!({
        "ok": true,
        "stage": "prepare",
        "profile": profile.to_json(),
        "validation": profile.validate().to_json(),
        "diff": diff.to_json(),
        "token": draft.token,
        "expires_in_s": draft.expires_in_s,
        "reasons": Value::Object(reasons),
    });
    (StatusCode::OK, Json(body)).into_response()
}

/// 第二阶段：token 换草稿 -> 原子落盘 + revision+1。
async fn commit_profile(state: Arc<DaemonState>, object: Map<String, Value>) -> Response {
    let token = match token_field(&object) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    let now_rfc = status::rfc3339_utc(SystemTime::now());
    let worker = Arc::clone(&state);
    let result = tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let draft = worker
            .audio_profile()
            .take_draft(&token, Instant::now())
            .map_err(|error| ApiError::conflict(error.code(), error.message()))?;
        let validation = draft.profile.validate();
        if !validation.ok() {
            return Err(ApiError::bad_request(
                "the prepared audio profile is structurally invalid: see details.validation",
            )
            .with_detail("validation", validation.to_json()));
        }
        let saved = worker
            .audio_profile()
            .save(draft.profile.clone(), &now_rfc)
            .map_err(ApiError::internal)?;
        Ok(saved)
    })
    .await;
    match result {
        Ok(Ok(saved)) => {
            let body = json!({
                "ok": true,
                "applied": true,
                "saved": true,
                "profile": saved.profile.to_json(),
                "revision": saved.revision,
                "saved_at": saved.saved_at,
            });
            (StatusCode::OK, Json(body)).into_response()
        }
        Ok(Err(error)) => error.into_response(),
        Err(join_error) => {
            ApiError::internal(format!("audio profile commit task failed: {join_error}"))
                .into_response()
        }
    }
}

/// 第三阶段：丢弃草稿（token 必须匹配；不写盘）。
async fn abort_profile(state: Arc<DaemonState>, object: Map<String, Value>) -> Response {
    let token = match token_field(&object) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    let worker = Arc::clone(&state);
    let result = tokio::task::spawn_blocking(move || {
        worker
            .audio_profile()
            .take_draft(&token, Instant::now())
            .map_err(|error| (error.code(), error.message()))
    })
    .await;
    match result {
        Ok(Ok(_)) => (StatusCode::OK, Json(json!({ "ok": true, "aborted": true }))).into_response(),
        Ok(Err((code, message))) => ApiError::conflict(code, message).into_response(),
        Err(join_error) => {
            ApiError::internal(format!("audio profile abort task failed: {join_error}"))
                .into_response()
        }
    }
}

/// commit/abort 的 token 字段（token；兼容 engine_token）。
fn token_field(object: &Map<String, Value>) -> Result<String, ApiError> {
    match object.get("token").or_else(|| object.get("engine_token")) {
        Some(Value::String(token)) if !token.trim().is_empty() => Ok(token.trim().to_string()),
        Some(Value::String(_)) => Err(ApiError::invalid_field(
            "token",
            "field token must not be empty",
        )),
        Some(_) => Err(ApiError::invalid_field(
            "token",
            "field token must be a string",
        )),
        None => Err(ApiError::invalid_field(
            "token",
            "missing field: token (the one returned by stage=prepare)",
        )),
    }
}

/// 原因表 -> JSON 对象。
pub(crate) fn reasons_json(reasons: &BTreeMap<String, String>) -> Value {
    let mut object = Map::new();
    for (path, reason) in reasons {
        object.insert(path.clone(), Value::String(reason.clone()));
    }
    Value::Object(object)
}
