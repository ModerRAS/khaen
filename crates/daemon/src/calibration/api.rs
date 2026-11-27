//! 校准 REST 的 axum 路由与 handler（T36）。
//!
//! # 端点与状态码
//!
//! | 方法 | 路径 | 成功 | 说明 |
//! |---|---|---|---|
//! | POST | /api/calibration/uploads | 201/200 | 原始字节 → `{upload_id,sha256,bytes,created}`；同 sha 幂等（200） |
//! | GET | /api/calibration/jobs | 200 | `{schema,available,reason,engine,active_job_id,total,limit,offset,jobs,reasons}` |
//! | POST | /api/calibration/jobs | 202 | `{job_id,location}`；同刻只允许一个 active job（409 switch_busy） |
//! | GET | /api/calibration/jobs/{id} | 200 | `{schema,job:{...},reasons}` |
//! | POST | /api/calibration/jobs/{id}/cancel | 200 | `{job_id,status}` |
//! | POST | /api/calibration/jobs/{id}/apply | 200 | prepare→commit（现有控制面）+ 回写 |
//! | POST | /api/calibration/jobs/{id}/rollback | 200 | rollback→commit（现有控制面）+ 回写 |
//! | GET | /api/calibration/jobs/{id}/artifacts/{name} | 200 | 白名单产物；按 manifest sha256 校验 |
//! | GET/PUT | /api/calibration/wizard | 200 | 向导配置（wizard.json） |
//! | GET | /api/calibration/wizard/state | 200 | 8 步向导状态（真实来源推导） |
//! | POST | /api/calibration/wizard/apply | 200 | 内部转 job apply |
//! | POST | /api/calibration/mic/calibration | 200 | 原始 UMIK txt（≤1 MiB）→ 校验 + 内容寻址存储 |
//! | GET | /api/calibration/mic | 200 | 当前 MicProfile（无 → profile:null + reasons） |
//! | PUT | /api/calibration/mic | 200 | 部分更新 device/serial/orientation/cal_sha256 |
//! | GET | /api/calibration/mic/status | 200 | profile + rt_host 实时 UMIK 状态（C3a：经 host link 的 cal_status；失败如实降级） |
//!
//! 所有 /api 路径都过现有 auth 中间件（登录会话；写方法额外 CSRF），错误体固定
//! `{"error":{"code","message","details"}}`（PLAN §4.1）。缺失字段一律 null + `reasons`，
//! 不编数值、不伪造进度。

use std::collections::{BTreeMap, HashMap};
use std::time::SystemTime;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Map, Value};

use khaen_core::sha256::sha256_hex;

use super::{StagedControlKind, StagedControlOp};
use crate::calibration::mic;
use crate::calibration::service::{
    AppliedVersion, CalibrationError, JobApply, JobInputs, JobQuery, JobRecord, JobResult,
    JobState, JobStep, NewJob, RtHostJobInputs, JOB_KIND_SINGLE_POINT_OFFLINE,
    JOB_KIND_SINGLE_POINT_RT_HOST, RT_HOST_MAX_CHANNELS, RT_HOST_MAX_LEVEL_DBFS,
    RT_HOST_MIN_LEVEL_DBFS,
};
use crate::calibration::store::{self, MAX_UPLOAD_BYTES};
use crate::calibration::wizard::{self, MeasurementDraft, WizardStateInputs, WIZARD_SCHEMA};
use crate::calibration::CalibrationHub;
use crate::host_link::{LinkSnapshot, Target, WireReply, HOST_LINK_UNAVAILABLE};
use crate::{error_response, DaemonState};

/// job 列表 schema。
pub const JOBS_SCHEMA: &str = "khaen-calibration-jobs/v1";
/// job 详情 schema。
pub const JOB_SCHEMA: &str = "khaen-calibration-job/v1";
/// 列表默认每页条数。
pub const DEFAULT_LIMIT: usize = 20;
/// 列表最大每页条数。
pub const MAX_LIMIT: usize = 100;
/// JSON 请求体上限（job/wizard 的 JSON 很小）。
pub const MAX_JSON_BODY: usize = 64 * 1024;
/// 单个 job 最多回显的产物数（与 backend manifest 上限一致）。
pub const MAX_LIST_ARTIFACTS: usize = 256;

/// 组装校准 REST 路由（挂到 /api 全局 auth 中间件之下）。
pub fn routes() -> Router<std::sync::Arc<DaemonState>> {
    Router::new()
        .route("/api/calibration/uploads", post(upload))
        .route("/api/calibration/jobs", get(list_jobs).post(create_job))
        .route("/api/calibration/jobs/{job_id}", get(job_detail))
        .route("/api/calibration/jobs/{job_id}/cancel", post(cancel_job))
        .route("/api/calibration/jobs/{job_id}/apply", post(apply_job))
        .route(
            "/api/calibration/jobs/{job_id}/rollback",
            post(rollback_job),
        )
        .route(
            "/api/calibration/jobs/{job_id}/artifacts/{name}",
            get(download_artifact),
        )
        .route("/api/calibration/wizard", get(get_wizard).put(put_wizard))
        .route("/api/calibration/wizard/state", get(get_wizard_state))
        .route("/api/calibration/wizard/apply", post(wizard_apply))
        // 真实校准桥接 C1：MicProfile + UMIK 校准文件（rt-host 未接线，status 如实降级）。
        .route(
            "/api/calibration/mic",
            get(get_mic_profile).put(put_mic_profile),
        )
        .route(
            "/api/calibration/mic/calibration",
            post(upload_mic_calibration),
        )
        .route("/api/calibration/mic/status", get(get_mic_status))
}

// ---------------------------------------------------------------------------
// 公共小工具
// ---------------------------------------------------------------------------

fn now_rfc3339() -> String {
    crate::status::rfc3339_utc(SystemTime::now())
}

fn ok_json(mut value: Value) -> Response {
    // docs/API.md §1：成功体推荐 ok:true（新端点应带）；对已有对象追加，不改任何既有字段。
    if let Some(object) = value.as_object_mut() {
        object.insert("ok".to_string(), Value::Bool(true));
    }
    (StatusCode::OK, Json(value)).into_response()
}

/// 读 JSON 请求体（空 body → Value::Null，由 handler 决定是否必填）。
async fn read_json_body(body: Body) -> Result<Value, CalibrationError> {
    let bytes = axum::body::to_bytes(body, MAX_JSON_BODY)
        .await
        .map_err(|error| {
            CalibrationError::invalid_request(
                "<body>",
                format!("cannot read request body: {error}"),
            )
        })?;
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes).map_err(|error| {
        CalibrationError::invalid_request(
            "<body>",
            format!("request body must be valid JSON: {error}"),
        )
    })
}

/// 要求 body 是 JSON object。
fn require_body_object(value: Value) -> Result<Map<String, Value>, CalibrationError> {
    match value {
        Value::Object(object) => Ok(object),
        Value::Null => Err(CalibrationError::invalid_request(
            "<body>",
            "request body must be a JSON object",
        )),
        _ => Err(CalibrationError::invalid_request(
            "<body>",
            "request body must be a JSON object",
        )),
    }
}

/// 拒绝未知字段（严格按 REST 契约；未知字段 400）。
fn reject_unknown_fields(
    object: &Map<String, Value>,
    allowed: &[&str],
    prefix: &str,
) -> Result<(), CalibrationError> {
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            let field = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            return Err(CalibrationError::invalid_request(
                &field,
                format!(
                    "unknown field {field:?}: allowed fields are {}",
                    allowed.join(", ")
                ),
            ));
        }
    }
    Ok(())
}

fn required_object<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a Map<String, Value>, CalibrationError> {
    match object.get(field) {
        None => Err(CalibrationError::invalid_request(
            field,
            format!("missing required field {field:?}"),
        )),
        Some(Value::Object(nested)) => Ok(nested),
        Some(_) => Err(CalibrationError::invalid_request(
            field,
            format!("{field} must be a JSON object"),
        )),
    }
}

fn required_string(object: &Map<String, Value>, field: &str) -> Result<String, CalibrationError> {
    match object.get(field) {
        None => Err(CalibrationError::invalid_request(
            field,
            format!("missing required field {field:?}"),
        )),
        Some(Value::String(text)) => Ok(text.clone()),
        Some(_) => Err(CalibrationError::invalid_request(
            field,
            format!("{field} must be a string"),
        )),
    }
}

fn required_number(object: &Map<String, Value>, field: &str) -> Result<f64, CalibrationError> {
    match object.get(field) {
        None => Err(CalibrationError::invalid_request(
            field,
            format!("missing required field {field:?}"),
        )),
        Some(Value::Number(number)) => Ok(number.as_f64().unwrap_or(f64::NAN)),
        Some(_) => Err(CalibrationError::invalid_request(
            field,
            format!("{field} must be a number"),
        )),
    }
}

fn required_u64(object: &Map<String, Value>, field: &str) -> Result<u64, CalibrationError> {
    match object.get(field) {
        None => Err(CalibrationError::invalid_request(
            field,
            format!("missing required field {field:?}"),
        )),
        Some(Value::Number(number)) => number.as_u64().ok_or_else(|| {
            CalibrationError::invalid_request(
                field,
                format!("{field} must be a non-negative integer"),
            )
        }),
        Some(_) => Err(CalibrationError::invalid_request(
            field,
            format!("{field} must be a non-negative integer"),
        )),
    }
}

/// job id 校验（路径参数；拒绝分隔符与超长值）。
fn validate_job_id(job_id: &str) -> Result<(), CalibrationError> {
    let ok = !job_id.is_empty()
        && job_id.len() <= 64
        && !job_id.contains("..")
        && job_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if ok {
        Ok(())
    } else {
        Err(CalibrationError::invalid_request(
            "job_id",
            "job_id must be 1..=64 characters of ASCII letters, digits, '-', '_' or '.'",
        ))
    }
}

fn insert_reason_if_null(
    reasons: &mut BTreeMap<String, String>,
    path: &str,
    is_null: bool,
    reason: impl Into<String>,
) {
    if is_null && !reasons.contains_key(path) {
        reasons.insert(path.to_string(), reason.into());
    }
}

// ---------------------------------------------------------------------------
// 上传
// ---------------------------------------------------------------------------

/// POST /api/calibration/uploads：原始字节 body（64 MiB 上限），内容寻址存盘。
pub(crate) async fn upload(
    State(state): State<std::sync::Arc<DaemonState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    // 1) 媒体类型（先把明显错误的请求挡掉，不读 body）。
    let essence = match store::content_type_essence(&headers) {
        Some(essence) => essence,
        None => {
            return CalibrationError::unsupported_format(None).into_response();
        }
    };
    if !store::is_supported_upload_content_type(&essence) {
        return CalibrationError::unsupported_format(Some(&essence)).into_response();
    }
    // 2) Content-Length 预检（超限直接 413，不读 body）。
    if let Some(length) = content_length(&headers) {
        if length > MAX_UPLOAD_BYTES {
            return CalibrationError::upload_too_large(MAX_UPLOAD_BYTES).into_response();
        }
    }
    // 3) 读取 body（有上限；chunked 超限在读取阶段报错）。
    let bytes = match axum::body::to_bytes(body, MAX_UPLOAD_BYTES as usize).await {
        Ok(bytes) => bytes,
        Err(error) => {
            if is_length_limit_error(&error) {
                return CalibrationError::upload_too_large(MAX_UPLOAD_BYTES).into_response();
            }
            return CalibrationError::invalid_request(
                "<body>",
                format!("cannot read upload body: {error}"),
            )
            .into_response();
        }
    };
    if bytes.is_empty() {
        return CalibrationError::invalid_request("<body>", "upload body must not be empty")
            .into_response();
    }
    // 4) sha256 + 原子写（阻塞 IO 放 spawn_blocking，不堵 async worker）。
    let store = state.calibration().uploads().clone();
    let payload = bytes.clone();
    let stored = tokio::task::spawn_blocking(move || store.save(payload.as_ref())).await;
    match stored {
        Ok(Ok((record, created))) => {
            let status = if created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            };
            (
                status,
                Json(json!({
                    "upload_id": record.upload_id,
                    "sha256": record.sha256,
                    "bytes": record.bytes,
                    "created": created,
                })),
            )
                .into_response()
        }
        Ok(Err(error)) => error.into_response(),
        Err(join) => {
            CalibrationError::internal(format!("upload worker failed: {join}")).into_response()
        }
    }
}

fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
}

/// axum body 读取错误是否是超限（http-body-util 的 LengthLimitError 在错误链里）。
fn is_length_limit_error(error: &axum::Error) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(current) = source {
        if current.to_string().contains("length limit") {
            return true;
        }
        source = current.source();
    }
    false
}

// ---------------------------------------------------------------------------
// jobs 列表 / 创建
// ---------------------------------------------------------------------------

/// GET /api/calibration/jobs?limit=&offset=
pub(crate) async fn list_jobs(
    State(state): State<std::sync::Arc<DaemonState>>,
    query: Result<Query<HashMap<String, String>>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let params = match query {
        Ok(Query(params)) => params,
        Err(error) => {
            return CalibrationError::invalid_request("<query>", error.body_text()).into_response()
        }
    };
    let list_query = match parse_page_query(&params) {
        Ok(query) => query,
        Err(error) => return error.into_response(),
    };
    let hub = state.calibration();
    let calibration = hub.status();
    let page = match hub.service().list_jobs(&list_query) {
        Ok(page) => page,
        Err(error) => return error.into_response(),
    };
    let mut reasons = BTreeMap::new();
    let reason = if calibration.wired {
        calibration.reason.clone()
    } else {
        let text = calibration
            .reason
            .clone()
            .unwrap_or_else(|| crate::calibration::NOT_WIRED_REASON.to_string());
        reasons.insert("jobs".to_string(), text.clone());
        Some(text)
    };
    insert_reason_if_null(
        &mut reasons,
        "engine",
        calibration.engine.is_none(),
        "engine is reported by the calibration job backend when it exposes one; this daemon has no engine source for offline jobs",
    );
    insert_reason_if_null(
        &mut reasons,
        "active_job_id",
        calibration.active_job.is_none(),
        if calibration.wired {
            "no calibration job is active"
        } else {
            crate::calibration::NOT_WIRED_REASON
        },
    );
    let jobs: Vec<Value> = page
        .jobs
        .iter()
        .take(list_query.limit)
        .map(|summary| {
            let record_json = json!({
                "id": summary.job_id,
                "kind": summary.kind,
                "status": summary.state.as_str(),
                "verdict": summary.verdict,
                "created_at": summary.created_at,
                "created_unix": summary.created_unix,
                "started_at": summary.started_at,
                "finished_at": summary.finished_at,
                "updated_unix": summary.updated_unix,
                "progress": summary.progress,
                "duration_ms": summary.duration_ms,
                "error": summary.error.as_ref().map(|error| json!({"code": error.code, "message": error.message})),
                "reason": summary.reason,
            });
            record_json
        })
        .collect();
    ok_json(json!({
        "schema": JOBS_SCHEMA,
        "available": calibration.wired,
        "reason": reason,
        "engine": calibration.engine,
        "active_job_id": calibration.active_job,
        "total": page.total,
        "limit": list_query.limit,
        "offset": list_query.offset,
        "jobs": jobs,
        "reasons": reasons,
    }))
}

fn parse_page_query(params: &HashMap<String, String>) -> Result<JobQuery, CalibrationError> {
    for key in params.keys() {
        if key != "limit" && key != "offset" {
            return Err(CalibrationError::invalid_request(
                key,
                format!("unknown query parameter {key:?}: allowed parameters are limit, offset"),
            ));
        }
    }
    let limit = match params.get("limit") {
        None => DEFAULT_LIMIT,
        Some(text) => {
            let value = text.trim().parse::<usize>().map_err(|_| {
                CalibrationError::invalid_request("limit", "limit must be an integer")
            })?;
            if !(1..=MAX_LIMIT).contains(&value) {
                return Err(CalibrationError::invalid_request(
                    "limit",
                    format!("limit must be in 1..={MAX_LIMIT}"),
                ));
            }
            value
        }
    };
    let offset = match params.get("offset") {
        None => 0,
        Some(text) => text.trim().parse::<usize>().map_err(|_| {
            CalibrationError::invalid_request("offset", "offset must be a non-negative integer")
        })?,
    };
    Ok(JobQuery { limit, offset })
}

/// POST /api/calibration/jobs：校验 kind/inputs/上传，交给服务创建（202）。
pub(crate) async fn create_job(
    State(state): State<std::sync::Arc<DaemonState>>,
    body: Body,
) -> Response {
    let value = match read_json_body(body).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let object = match require_body_object(value) {
        Ok(object) => object,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = reject_unknown_fields(&object, &["kind", "inputs"], "") {
        return error.into_response();
    }
    let kind = match required_string(&object, "kind") {
        Ok(kind) => kind,
        Err(error) => return error.into_response(),
    };
    match kind.as_str() {
        JOB_KIND_SINGLE_POINT_OFFLINE => {
            let inputs_object = match required_object(&object, "inputs") {
                Ok(inputs) => inputs,
                Err(error) => return error.into_response(),
            };
            if let Err(error) = reject_unknown_fields(
                inputs_object,
                &[
                    "plan_sha",
                    "baseline_upload_id",
                    "applied_upload_id",
                    "centers_hz",
                    "tolerance_db",
                    "min_bands",
                ],
                "inputs",
            ) {
                return error.into_response();
            }
            let inputs = match parse_job_inputs(state.calibration(), inputs_object) {
                Ok(inputs) => inputs,
                Err(error) => return error.into_response(),
            };
            let new_job = match resolve_uploads(state.calibration(), &inputs) {
                Ok(new_job) => new_job,
                Err(error) => return error.into_response(),
            };
            match state.calibration().service().start_job(&new_job) {
                Ok(record) => accepted_job_response(record),
                Err(error) => error.into_response(),
            }
        }
        // C3b1：真实测量 job。创建期由服务层校验 MicProfile/rt-host 可用性；这里只做
        // REST 输入形状校验（P08 可闻授权必须显式 true；未知字段 400）。
        JOB_KIND_SINGLE_POINT_RT_HOST => {
            let inputs_object = match required_object(&object, "inputs") {
                Ok(inputs) => inputs,
                Err(error) => return error.into_response(),
            };
            if let Err(error) = reject_unknown_fields(
                inputs_object,
                &["audible", "channels", "level_dbfs"],
                "inputs",
            ) {
                return error.into_response();
            }
            let inputs = match parse_rt_host_inputs(inputs_object) {
                Ok(inputs) => inputs,
                Err(error) => return error.into_response(),
            };
            match state
                .calibration()
                .service()
                .start_rt_host_job(&inputs)
            {
                Ok(record) => accepted_job_response(record),
                Err(error) => error.into_response(),
            }
        }
        other => CalibrationError::invalid_request(
            "kind",
            format!(
                "unsupported job kind {other:?}: defined kinds are {JOB_KIND_SINGLE_POINT_OFFLINE:?} (offline closed-loop evidence) and {JOB_KIND_SINGLE_POINT_RT_HOST:?} (real rt-host measurement)"
            ),
        )
        .into_response(),
    }
}

/// 202 创建回执（离线/rt-host job 共用同一形状）。
fn accepted_job_response(record: JobRecord) -> Response {
    let location = format!("/api/calibration/jobs/{}", record.job_id);
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "ok": true,
            "job_id": record.job_id,
            "state": record.state.as_str(),
            "location": location,
        })),
    )
        .into_response()
}

/// 解析 rt-host 真实测量 job 的 inputs（{audible:true, channels?,[u32], level_dbfs?:f64}）。
fn parse_rt_host_inputs(object: &Map<String, Value>) -> Result<RtHostJobInputs, CalibrationError> {
    let audible = match object.get("audible") {
        Some(Value::Bool(value)) => *value,
        Some(_) => {
            return Err(CalibrationError::invalid_request(
                "inputs.audible",
                "inputs.audible must be a boolean",
            ))
        }
        None => {
            return Err(CalibrationError::invalid_request(
                "inputs.audible",
                "missing required field \"audible\": a real measurement plays an audible sweep, so the request must explicitly authorize it (audible:true)",
            ))
        }
    };
    if !audible {
        return Err(CalibrationError::invalid_request(
            "inputs.audible",
            "inputs.audible must be true: a real rt-host measurement plays an audible sweep (P08); the daemon never starts a silent measurement",
        ));
    }
    let channels = match object.get("channels") {
        None | Some(Value::Null) => None,
        Some(Value::Array(values)) => {
            if values.is_empty() {
                return Err(CalibrationError::invalid_request(
                    "inputs.channels",
                    "inputs.channels must not be empty when provided",
                ));
            }
            if values.len() > RT_HOST_MAX_CHANNELS {
                return Err(CalibrationError::invalid_request(
                    "inputs.channels",
                    format!(
                        "inputs.channels has {} entries; at most {RT_HOST_MAX_CHANNELS} are supported",
                        values.len()
                    ),
                ));
            }
            let mut channels = Vec::with_capacity(values.len());
            let mut seen = Vec::with_capacity(values.len());
            for value in values {
                let number = value.as_u64().ok_or_else(|| {
                    CalibrationError::invalid_request(
                        "inputs.channels",
                        "inputs.channels entries must be non-negative integers",
                    )
                })?;
                let channel = u32::try_from(number).map_err(|_| {
                    CalibrationError::invalid_request(
                        "inputs.channels",
                        format!("inputs.channels entry {number} does not fit in u32"),
                    )
                })?;
                if seen.contains(&channel) {
                    return Err(CalibrationError::invalid_request(
                        "inputs.channels",
                        format!("inputs.channels contains duplicate channel {channel}"),
                    ));
                }
                seen.push(channel);
                channels.push(channel);
            }
            Some(channels)
        }
        Some(_) => {
            return Err(CalibrationError::invalid_request(
                "inputs.channels",
                "inputs.channels must be an array of non-negative integers",
            ))
        }
    };
    let level_dbfs = match object.get("level_dbfs") {
        None | Some(Value::Null) => None,
        Some(Value::Number(_)) => {
            let level = object
                .get("level_dbfs")
                .and_then(Value::as_f64)
                .expect("number is f64");
            if !level.is_finite()
                || !(RT_HOST_MIN_LEVEL_DBFS..=RT_HOST_MAX_LEVEL_DBFS).contains(&level)
            {
                return Err(CalibrationError::invalid_request(
                    "inputs.level_dbfs",
                    format!(
                        "inputs.level_dbfs must be a finite number in [{RT_HOST_MIN_LEVEL_DBFS}, {RT_HOST_MAX_LEVEL_DBFS}] dBFS"
                    ),
                ));
            }
            Some(level)
        }
        Some(_) => {
            return Err(CalibrationError::invalid_request(
                "inputs.level_dbfs",
                "inputs.level_dbfs must be a number",
            ))
        }
    };
    Ok(RtHostJobInputs {
        audible,
        channels,
        level_dbfs,
    })
}

fn parse_job_inputs(
    hub: &CalibrationHub,
    object: &Map<String, Value>,
) -> Result<JobInputs, CalibrationError> {
    let plan_sha = required_string(object, "plan_sha")?;
    if !store::is_sha256_hex(&plan_sha) {
        return Err(CalibrationError::invalid_request(
            "inputs.plan_sha",
            "plan_sha must be a 64-character hex sha256",
        ));
    }
    let baseline_upload_id = required_string(object, "baseline_upload_id")?;
    if !store::is_upload_id(&baseline_upload_id) {
        return Err(CalibrationError::invalid_upload(format!(
            "inputs.baseline_upload_id must be 64 lowercase hex characters, got {baseline_upload_id:?}"
        ))
        .with_detail("field", json!("inputs.baseline_upload_id")));
    }
    let applied_upload_id = required_string(object, "applied_upload_id")?;
    if !store::is_upload_id(&applied_upload_id) {
        return Err(CalibrationError::invalid_upload(format!(
            "inputs.applied_upload_id must be 64 lowercase hex characters, got {applied_upload_id:?}"
        ))
        .with_detail("field", json!("inputs.applied_upload_id")));
    }
    // 提前校验存在性（在服务调用前给出稳定 404，而不是把坏 id 传给后端）。
    if hub.uploads().lookup(&baseline_upload_id)?.is_none() {
        return Err(CalibrationError::upload_not_found(&baseline_upload_id)
            .with_detail("field", json!("inputs.baseline_upload_id")));
    }
    if hub.uploads().lookup(&applied_upload_id)?.is_none() {
        return Err(CalibrationError::upload_not_found(&applied_upload_id)
            .with_detail("field", json!("inputs.applied_upload_id")));
    }
    let centers_hz = parse_centers(object.get("centers_hz"))?;
    let tolerance_db = required_number(object, "tolerance_db")?;
    if !tolerance_db.is_finite() || tolerance_db <= 0.0 || tolerance_db > wizard::MAX_TOLERANCE_DB {
        return Err(CalibrationError::invalid_request(
            "inputs.tolerance_db",
            format!(
                "tolerance_db must be a finite number in (0, {}]",
                wizard::MAX_TOLERANCE_DB
            ),
        ));
    }
    let min_bands = required_u64(object, "min_bands")?;
    let min_bands = u32::try_from(min_bands).map_err(|_| {
        CalibrationError::invalid_request("inputs.min_bands", "min_bands does not fit in u32")
    })?;
    if min_bands == 0 {
        return Err(CalibrationError::invalid_request(
            "inputs.min_bands",
            "min_bands must be >= 1",
        ));
    }
    if min_bands as usize > centers_hz.len() {
        return Err(CalibrationError::invalid_request(
            "inputs.min_bands",
            format!(
                "min_bands ({min_bands}) must not exceed the number of centers_hz ({})",
                centers_hz.len()
            ),
        ));
    }
    Ok(JobInputs {
        plan_sha: plan_sha.to_ascii_lowercase(),
        baseline_upload_id,
        applied_upload_id,
        centers_hz,
        tolerance_db,
        min_bands,
    })
}

fn parse_centers(value: Option<&Value>) -> Result<Vec<f64>, CalibrationError> {
    let values = match value {
        None => {
            return Err(CalibrationError::invalid_request(
                "inputs.centers_hz",
                "missing required field \"centers_hz\"",
            ))
        }
        Some(Value::Array(values)) => values,
        Some(_) => {
            return Err(CalibrationError::invalid_request(
                "inputs.centers_hz",
                "centers_hz must be an array of numbers",
            ))
        }
    };
    if values.is_empty() {
        return Err(CalibrationError::invalid_request(
            "inputs.centers_hz",
            "centers_hz must not be empty",
        ));
    }
    if values.len() > wizard::MAX_CENTERS {
        return Err(CalibrationError::invalid_request(
            "inputs.centers_hz",
            format!(
                "centers_hz must have at most {} entries",
                wizard::MAX_CENTERS
            ),
        ));
    }
    let mut centers = Vec::with_capacity(values.len());
    for value in values {
        let center = value.as_f64().ok_or_else(|| {
            CalibrationError::invalid_request(
                "inputs.centers_hz",
                "centers_hz entries must be numbers",
            )
        })?;
        if !center.is_finite() || center < wizard::MIN_CENTER_HZ || center > wizard::MAX_CENTER_HZ {
            return Err(CalibrationError::invalid_request(
                "inputs.centers_hz",
                format!(
                    "centers_hz entries must be finite Hz values in [{}, {}]",
                    wizard::MIN_CENTER_HZ,
                    wizard::MAX_CENTER_HZ
                ),
            ));
        }
        centers.push(center);
    }
    for pair in centers.windows(2) {
        // 上面已校验每个值有限且 > 0，可直接比较（不会出现 NaN）。
        if pair[0] >= pair[1] {
            return Err(CalibrationError::invalid_request(
                "inputs.centers_hz",
                "centers_hz must be strictly increasing",
            ));
        }
    }
    Ok(centers)
}

fn resolve_uploads(hub: &CalibrationHub, inputs: &JobInputs) -> Result<NewJob, CalibrationError> {
    let baseline = hub
        .uploads()
        .lookup(&inputs.baseline_upload_id)?
        .ok_or_else(|| CalibrationError::upload_not_found(&inputs.baseline_upload_id))?;
    let applied = hub
        .uploads()
        .lookup(&inputs.applied_upload_id)?
        .ok_or_else(|| CalibrationError::upload_not_found(&inputs.applied_upload_id))?;
    Ok(NewJob {
        kind: crate::calibration::service::JOB_KIND_SINGLE_POINT_OFFLINE.to_string(),
        inputs: inputs.clone(),
        baseline: baseline.as_ref_record(),
        applied: applied.as_ref_record(),
    })
}

// ---------------------------------------------------------------------------
// job 详情 / 取消
// ---------------------------------------------------------------------------

/// GET /api/calibration/jobs/{id}
pub(crate) async fn job_detail(
    State(state): State<std::sync::Arc<DaemonState>>,
    Path(job_id): Path<String>,
) -> Response {
    if let Err(error) = validate_job_id(&job_id) {
        return error.into_response();
    }
    let hub = state.calibration();
    match hub.service().job(&job_id) {
        Ok(record) => {
            let job = job_json(&record);
            let mut body = json!({
                "schema": JOB_SCHEMA,
                "job": job,
                // docs/API.md §7 加法：详情顶层也暴露 result/artifacts/error（与 job 内同源）。
                "result": job["result"].clone(),
                "artifacts": job["artifacts"].clone(),
                "error": job["error"].clone(),
                "reasons": job_reasons(&record),
            });
            // C3b1 加法：rt-host 真实测量 job 的会话输入（离线 job 无此字段；离线输入仍走 inputs）。
            if record.kind == JOB_KIND_SINGLE_POINT_RT_HOST {
                if let Some(inputs) = hub.service().rt_host_inputs(&record.job_id) {
                    body["inputs_rt_host"] = json!({
                        "audible": inputs.audible,
                        "channels": inputs.channels,
                        "level_dbfs": inputs.level_dbfs,
                    });
                }
            }
            ok_json(body)
        }
        Err(error) => error.into_response(),
    }
}

/// POST /api/calibration/jobs/{id}/cancel
pub(crate) async fn cancel_job(
    State(state): State<std::sync::Arc<DaemonState>>,
    Path(job_id): Path<String>,
) -> Response {
    if let Err(error) = validate_job_id(&job_id) {
        return error.into_response();
    }
    match state.calibration().service().cancel(&job_id) {
        Ok(record) => ok_json(json!({
            "job_id": record.job_id,
            "status": record.state.as_str(),
            "state": record.state.as_str(),
        })),
        Err(error) => error.into_response(),
    }
}

/// 单个 job 的 REST JSON（job 对象；缺失字段 null，原因在 job_reasons）。
fn job_json(record: &JobRecord) -> Value {
    // 步骤名按 job 种类取（离线 5 步 / rt-host 4 步）：固定用离线步骤会把 rt-host 的
    // sweep_capture/quality_check 抹成 compile/levels 占位（不诚实）。
    let step_names = crate::calibration::service::job_step_names(&record.kind);
    let steps: Vec<Value> = step_names
        .iter()
        .map(|name| {
            let step = record
                .steps
                .iter()
                .find(|step| step.name == *name)
                .map(step_json)
                .unwrap_or_else(|| {
                    json!({
                        "name": name,
                        "status": "pending",
                        "started_at": Value::Null,
                        "finished_at": Value::Null,
                        "sha256": Value::Null,
                        "detail": Value::Null,
                    })
                });
            step
        })
        .collect();
    let artifacts: Vec<Value> = record
        .artifacts
        .iter()
        .take(MAX_LIST_ARTIFACTS)
        .map(|artifact| {
            json!({
                "name": artifact.name,
                "sha256": artifact.sha256,
                "bytes": artifact.bytes,
                "content_type": artifact
                    .content_type
                    .clone()
                    .unwrap_or_else(|| store::artifact_content_type(&artifact.name).to_string()),
                "downloadable": store::is_whitelisted_artifact(&artifact.name),
            })
        })
        .collect();
    json!({
        "id": record.job_id,
        "kind": record.kind,
        "status": record.state.as_str(),
        "step": record.active_step,
        "verdict": record.verdict,
        "created_at": record.created_at,
        "created_unix": record.created_unix,
        "started_at": record.started_at,
        "finished_at": record.finished_at,
        "updated_unix": record.updated_unix,
        "progress": record.progress,
        "duration_ms": record.duration_ms,
        "engine": record.engine,
        "steps": steps,
        "artifacts": artifacts,
        "inputs": record.inputs.as_ref().map(inputs_json),
        "result": record.result.as_ref().map(result_json),
        "error": record.error.as_ref().map(|error| json!({"code": error.code, "message": error.message})),
        "reason": record.reasons.get("state").cloned(),
        "apply_state": record.apply.state.as_str(),
        "applied_version": record.apply.applied_version,
        "apply": apply_json(&record.apply),
    })
}

fn step_json(step: &JobStep) -> Value {
    json!({
        "name": step.name,
        "status": step.state.as_str(),
        "started_at": step.started_at,
        "finished_at": step.finished_at,
        "sha256": step.sha256,
        "detail": step.detail,
    })
}

fn inputs_json(inputs: &JobInputs) -> Value {
    json!({
        "plan_sha": inputs.plan_sha,
        "baseline_upload_id": inputs.baseline_upload_id,
        "applied_upload_id": inputs.applied_upload_id,
        "centers_hz": inputs.centers_hz,
        "tolerance_db": inputs.tolerance_db,
        "min_bands": inputs.min_bands,
    })
}

fn result_json(result: &JobResult) -> Value {
    let bands: Vec<Value> = result
        .bands
        .iter()
        .map(|band| {
            json!({
                "hz": band.hz,
                "predicted_db": band.predicted_db,
                "observed_db": band.observed_db,
                "delta_db": band.delta_db,
                "missing": band.missing,
            })
        })
        .collect();
    json!({
        "verdict": result.verdict,
        "max_delta_db": result.max_delta_db,
        "compared_bands": result.compared_bands,
        "missing_bands": result.missing_bands,
        "violations": result.violations,
        "bands": bands,
        "design": result.design.as_ref().map(|design| json!({
            "residual": design.residual,
            "admission": {
                "accepted": design.admission_accepted,
                "code": design.admission_code,
                "reason": design.admission_reason,
            },
        })),
        "plan": result.plan.as_ref().map(|plan| json!({
            "sha256": plan.sha256,
            "graph_id": plan.graph_id,
        })),
        // 合同 §7：measured（上传录音）与 simulated（方案预测电平）必须如实分开标注。
        "measured": result.measured.as_ref().map(|measured| json!({
            "baseline_upload_id": measured.baseline_upload_id,
            "applied_upload_id": measured.applied_upload_id,
            "sample_rate_hz": measured.sample_rate_hz,
            "source": measured.source,
        })),
        "simulated": result.simulated.as_ref().map(|simulated| json!({
            "predicted_from": simulated.predicted_from,
            "plan_sha256": simulated.plan_sha256,
            "graph_id": simulated.graph_id,
        })),
    })
}

fn apply_json(apply: &JobApply) -> Value {
    json!({
        "state": apply.state.as_str(),
        "applied_version": apply.applied_version,
        "previous_version": apply.previous_version,
        "applied_at": apply.applied_at,
        "plan_sha256": apply.plan_sha256,
    })
}

/// job 缺失字段的原因（服务给的原因 + daemon 推导，不编值）。
fn job_reasons(record: &JobRecord) -> Map<String, Value> {
    let mut reasons: BTreeMap<String, String> = record.reasons.clone();
    let state = record.state.as_str();
    let rt_host = record.kind == crate::calibration::service::JOB_KIND_SINGLE_POINT_RT_HOST;
    insert_reason_if_null(
        &mut reasons,
        "verdict",
        record.verdict.is_none(),
        if rt_host {
            format!(
                "job is {state}: a real measurement job reports no PASS in this slice (quality/verdict arrive with C3b2); quality.json records available:false with the reason"
            )
        } else {
            format!("job is {state}: the verdict is produced by the closed_loop stage")
        },
    );
    insert_reason_if_null(
        &mut reasons,
        "created_at",
        record.created_at.is_none(),
        "the calibration backend did not report a creation timestamp",
    );
    insert_reason_if_null(
        &mut reasons,
        "started_at",
        record.started_at.is_none(),
        if record.state == JobState::Queued {
            "job has not started yet (queued)".to_string()
        } else {
            "the calibration backend did not report a start timestamp".to_string()
        },
    );
    insert_reason_if_null(
        &mut reasons,
        "finished_at",
        record.finished_at.is_none(),
        if record.state.is_terminal() {
            "the calibration backend did not report a finish timestamp".to_string()
        } else {
            format!("job is not finished yet (state={state})")
        },
    );
    insert_reason_if_null(
        &mut reasons,
        "duration_ms",
        record.duration_ms.is_none(),
        "duration_ms is derived by the backend from its stage timestamps; it is null until those exist",
    );
    insert_reason_if_null(
        &mut reasons,
        "engine",
        record.engine.is_none(),
        if rt_host {
            "the rt-host engine name is copied from the cal_status reply once the measurement completes; the daemon does not invent an engine"
        } else {
            "offline single-point jobs do not open an audio engine in the daemon process; engine is reported by the job backend when it has one"
        },
    );
    insert_reason_if_null(
        &mut reasons,
        "inputs",
        record.inputs.is_none(),
        if rt_host {
            "rt-host measurement jobs take no uploaded recordings: session inputs (audible/channels/level_dbfs) are echoed as inputs_rt_host"
        } else {
            "the calibration backend did not persist the job inputs; the daemon only echoes what the backend reports"
        },
    );
    insert_reason_if_null(
        &mut reasons,
        "result",
        record.result.is_none(),
        if rt_host {
            format!(
                "job is {state}: rt-host measurement jobs have no closed-loop comparison result in this slice (solve/apply arrive with C3b2); measurement evidence is in measurement.json/quality.json/report.json"
            )
        } else {
            format!(
                "job is {state}: analysis results exist after the closed_loop and bundle stages"
            )
        },
    );
    insert_reason_if_null(
        &mut reasons,
        "apply.applied_version",
        record.apply.applied_version.is_none(),
        "job has not been applied to the control plane (apply_state=not_applied)".to_string(),
    );
    insert_reason_if_null(
        &mut reasons,
        "apply.previous_version",
        record.apply.previous_version.is_none(),
        "no version was running before the job was applied",
    );
    insert_reason_if_null(
        &mut reasons,
        "apply.plan_sha256",
        record.apply.plan_sha256.is_none(),
        "the job has not been applied, so there is no applied plan sha256 yet",
    );
    reasons
        .into_iter()
        .map(|(key, value)| (key, Value::String(value)))
        .collect()
}

// ---------------------------------------------------------------------------
// apply / rollback（两阶段：服务取 plan/目标版本 → 现有控制面 prepare/commit）
// ---------------------------------------------------------------------------

/// POST /api/calibration/jobs/{id}/apply
///
/// 无 stage 字段 = 旧单发语义（prepare+commit 一次完成，现有前端/测试兼容）；
/// stage=prepare|commit|abort = 两阶段（token 只能消费一次；apply 按 host_link.target 路由）。
pub(crate) async fn apply_job(
    State(state): State<std::sync::Arc<DaemonState>>,
    Path(job_id): Path<String>,
    body: Body,
) -> Response {
    if let Err(error) = validate_job_id(&job_id) {
        return error.into_response();
    }
    match parse_staged_request(body).await {
        Ok(StagedRequest::Legacy) => apply_flow(&state, &job_id).await,
        Ok(StagedRequest::Prepare) => apply_prepare(&state, &job_id).await,
        Ok(StagedRequest::Commit { engine_token }) => {
            apply_commit(&state, &job_id, &engine_token).await
        }
        Ok(StagedRequest::Abort { engine_token }) => {
            apply_abort(&state, &job_id, &engine_token).await
        }
        Err(error) => error.into_response(),
    }
}

/// POST /api/calibration/wizard/apply {job_id?}
pub(crate) async fn wizard_apply(
    State(state): State<std::sync::Arc<DaemonState>>,
    body: Body,
) -> Response {
    let value = match read_json_body(body).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let job_id = match value {
        Value::Null => None,
        Value::Object(object) => {
            if let Err(error) = reject_unknown_fields(&object, &["job_id"], "") {
                return error.into_response();
            }
            match object.get("job_id") {
                None | Some(Value::Null) => None,
                Some(Value::String(job_id)) => {
                    if let Err(error) = validate_job_id(job_id) {
                        return error.into_response();
                    }
                    Some(job_id.clone())
                }
                Some(_) => {
                    return CalibrationError::invalid_request(
                        "job_id",
                        "job_id must be a string or null",
                    )
                    .into_response()
                }
            }
        }
        _ => {
            return CalibrationError::invalid_request("<body>", "body must be a JSON object")
                .into_response()
        }
    };
    let job_id = match job_id {
        Some(job_id) => job_id,
        None => {
            let calibration = state.calibration().status();
            if !calibration.wired {
                return CalibrationError::not_wired(
                    calibration
                        .reason
                        .as_deref()
                        .unwrap_or(crate::calibration::NOT_WIRED_REASON),
                )
                .into_response();
            }
            match calibration.last_job_id {
                Some(job_id) => job_id,
                None => {
                    return CalibrationError::no_applicable_job(
                        "body has no job_id and no calibration job has run yet",
                    )
                    .into_response()
                }
            }
        }
    };
    apply_flow(&state, &job_id).await
}

async fn apply_flow(state: &std::sync::Arc<DaemonState>, job_id: &str) -> Response {
    let service = std::sync::Arc::clone(state.calibration().service());
    // 1) 服务侧校验 job 状态并给出 plan 引用。
    let apply_plan = match service.apply(job_id) {
        Ok(plan) => plan,
        Err(error) => return error.into_response(),
    };
    // 2) 记录 apply 前的运行状态（snapshot：回滚目标）。
    let before = control_status(state).await;
    let plan_path = apply_plan.plan_path.to_string_lossy().to_string();
    // 3) 控制面两阶段：prepare -> commit（按 host_link.target 路由：process=进程内，
    //    rt-host=控制通道；绝不把 rt-host 的 apply 打到 daemon 进程内引擎）。
    let prepare = match control_prepare_plan(state, &plan_path, &apply_plan.plan_sha256).await {
        Ok(prepare) => prepare,
        Err(response) => return response,
    };
    let Some(engine_token) = prepare["engine_token"].as_str() else {
        return CalibrationError::internal("control plane prepare response has no engine_token")
            .into_response();
    };
    // 先 commit（process 目标下是同步的，不引入 await 空隙让别人顶掉草稿），
    // 再 best-effort 记录方案元数据；记录失败不影响已成功的 commit。
    let commit = match control_commit(state, engine_token).await {
        Ok(commit) => commit,
        Err(response) => return response,
    };
    crate::record_prepared_plan(state, plan_path.clone()).await;
    crate::record_commit(state, &commit);
    let readback = control_status(state).await;
    let version = commit["version"].as_u64();
    let graph_id = commit["graph_id"].as_str().unwrap_or_default().to_string();
    let previous_version = before["actual_version"].as_u64();
    let applied_at = now_rfc3339();
    let mut reasons: BTreeMap<String, String> = BTreeMap::new();
    reasons.insert(
        "snapshot.sha256".to_string(),
        "the control plane status does not expose the running plan sha256; only version/graph_id are available".to_string(),
    );
    let job_json = match version {
        Some(version) => {
            let applied = AppliedVersion {
                version,
                graph_id: graph_id.clone(),
                previous_version,
            };
            match service.apply_committed(job_id, &applied) {
                Ok(record) => {
                    reasons.extend(record.reasons.clone());
                    json!({
                        "schema": JOB_SCHEMA,
                        "job": job_json(&record),
                    })
                }
                Err(error) => {
                    // 控制面已经生效；回写失败只影响 job 记录，不能让调用方以为没生效。
                    reasons.insert(
                        "job".to_string(),
                        format!(
                            "the plan was applied but the calibration service could not record it: {}",
                            error.message()
                        ),
                    );
                    Value::Null
                }
            }
        }
        None => {
            reasons.insert(
                "applied_version".to_string(),
                "control plane commit response has no version field".to_string(),
            );
            Value::Null
        }
    };
    ok_json(json!({
        "applied": true,
        "job_id": job_id,
        "applied_version": version,
        "applied_block": commit["applied_block"].clone(),
        "graph_id": graph_id,
        "sha256": apply_plan.plan_sha256,
        "snapshot": {
            "version": previous_version,
            "graph_id": before["graph_id"].clone(),
            "sha256": Value::Null,
        },
        "ack": { "synthetic": false, "actual": true },
        "readback": readback,
        "applied_at": applied_at,
        "job": job_json,
    "reasons": reasons.into_iter().map(|(key, value)| (key, Value::String(value))).collect::<Map<String, Value>>(),
    }))
}

/// POST /api/calibration/jobs/{id}/rollback
///
/// 无 stage 字段 = 旧单发语义（prepare+commit 一次完成，现有前端/测试兼容）；
/// stage=prepare|commit|abort = 两阶段（token 只能消费一次；按 host_link.target 路由）。
pub(crate) async fn rollback_job(
    State(state): State<std::sync::Arc<DaemonState>>,
    Path(job_id): Path<String>,
    body: Body,
) -> Response {
    if let Err(error) = validate_job_id(&job_id) {
        return error.into_response();
    }
    match parse_staged_request(body).await {
        Ok(StagedRequest::Legacy) => rollback_flow(&state, &job_id).await,
        Ok(StagedRequest::Prepare) => rollback_prepare(&state, &job_id).await,
        Ok(StagedRequest::Commit { engine_token }) => {
            rollback_commit(&state, &job_id, &engine_token).await
        }
        Ok(StagedRequest::Abort { engine_token }) => {
            rollback_abort(&state, &job_id, &engine_token).await
        }
        Err(error) => error.into_response(),
    }
}

/// 旧单发语义的 rollback 流程（prepare -> commit 一次完成）。
async fn rollback_flow(state: &std::sync::Arc<DaemonState>, job_id: &str) -> Response {
    let state = std::sync::Arc::clone(state);
    let job_id = job_id.to_string();
    let service = std::sync::Arc::clone(state.calibration().service());
    let target = match service.rollback(&job_id) {
        Ok(target) => target,
        Err(error) => return error.into_response(),
    };
    let before = control_status(&state).await;
    let prepare = match control_rollback(&state, Some(target.target_version)).await {
        Ok(prepare) => prepare,
        Err(response) => return response,
    };
    let Some(engine_token) = prepare["engine_token"].as_str() else {
        return CalibrationError::internal("control plane rollback response has no engine_token")
            .into_response();
    };
    let commit = match control_commit(&state, engine_token).await {
        Ok(commit) => commit,
        Err(response) => return response,
    };
    crate::record_commit(&state, &commit);
    let readback = control_status(&state).await;
    let version = commit["version"].as_u64();
    let graph_id = commit["graph_id"].as_str().unwrap_or_default().to_string();
    let rolled_back_at = now_rfc3339();
    let mut reasons: BTreeMap<String, String> = BTreeMap::new();
    reasons.insert(
        "sha256".to_string(),
        "the control plane rollback response does not expose the restored plan sha256; only version/graph_id are available".to_string(),
    );
    reasons.insert(
        "snapshot".to_string(),
        format!(
            "rollback target was version {}; the pre-rollback running version was {} (reported by control status, may be null)",
            target.target_version,
            before["actual_version"]
                .as_u64()
                .map(|version| version.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        ),
    );
    let job_value = match version {
        Some(version) => {
            let applied = AppliedVersion {
                version,
                graph_id: graph_id.clone(),
                previous_version: Some(target.target_version),
            };
            match service.rollback_committed(&job_id, &applied) {
                Ok(record) => json!({
                    "schema": JOB_SCHEMA,
                    "job": job_json(&record),
                }),
                Err(error) => {
                    reasons.insert(
                        "job".to_string(),
                        format!(
                            "the plan was rolled back but the calibration service could not record it: {}",
                            error.message()
                        ),
                    );
                    Value::Null
                }
            }
        }
        None => Value::Null,
    };
    ok_json(json!({
        "rolled_back": true,
        "job_id": job_id,
        "restored_version": target.target_version,
        "applied_version": version,
        "graph_id": graph_id,
        "sha256": Value::Null,
        "readback": readback,
        "rolled_back_at": rolled_back_at,
        "job": job_value,
        "reasons": reasons.into_iter().map(|(key, value)| (key, Value::String(value))).collect::<Map<String, Value>>(),
    }))
}

// ---------------------------------------------------------------------------
// 两阶段控制面路由（process / rt-host）与 apply/rollback staged 流程
// ---------------------------------------------------------------------------

/// apply/rollback 请求体的 stage 分支（无 stage = 旧单发语义）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum StagedRequest {
    /// 旧单发：prepare+commit 一次完成。
    Legacy,
    /// 只 prepare，返回 token。
    Prepare,
    /// 用 prepare 的 token 生效。
    Commit { engine_token: String },
    /// 丢弃 prepare 的草稿。
    Abort { engine_token: String },
}

/// 读 apply/rollback 请求体：空 body / 无 stage = Legacy；其它严格校验（未知字段 400）。
async fn parse_staged_request(body: Body) -> Result<StagedRequest, CalibrationError> {
    let value = read_json_body(body).await?;
    let object = match value {
        Value::Null => return Ok(StagedRequest::Legacy),
        Value::Object(object) => object,
        _ => {
            return Err(CalibrationError::invalid_request(
                "<body>",
                "request body must be a JSON object or empty",
            ))
        }
    };
    reject_unknown_fields(&object, &["stage", "engine_token"], "")?;
    let Some(stage) = object.get("stage") else {
        if object.contains_key("engine_token") {
            return Err(CalibrationError::invalid_request(
                "stage",
                "engine_token requires stage \"commit\" or stage \"abort\"",
            ));
        }
        return Ok(StagedRequest::Legacy);
    };
    let stage = stage
        .as_str()
        .ok_or_else(|| CalibrationError::invalid_request("stage", "stage must be a string"))?;
    match stage {
        "prepare" => Ok(StagedRequest::Prepare),
        "commit" => Ok(StagedRequest::Commit {
            engine_token: required_string(&object, "engine_token")?,
        }),
        "abort" => Ok(StagedRequest::Abort {
            engine_token: required_string(&object, "engine_token")?,
        }),
        other => Err(CalibrationError::invalid_request(
            "stage",
            format!("unknown stage {other:?}: expected prepare, commit or abort"),
        )),
    }
}

/// 未知/已消费 token（409 engine_token_invalid；不泄露其它 job 的草稿信息）。
fn engine_token_invalid(engine_token: &str) -> CalibrationError {
    CalibrationError::new(
        StatusCode::CONFLICT,
        super::service::code::ENGINE_TOKEN_INVALID,
        format!(
            "engine_token {engine_token:?} is unknown or was already consumed: it must come from a prepare response of this same job"
        ),
    )
}

/// 控制面 prepare（方案草稿）：process 目标走进程内控制线程，rt-host 目标走控制通道转发。
#[allow(clippy::result_large_err)] // Err 是已经构造好的 axum Response：必须原样保留转发状态码/错误体
async fn control_prepare_plan(
    state: &std::sync::Arc<DaemonState>,
    plan_path: &str,
    plan_sha256: &str,
) -> Result<Value, Response> {
    if state.host_link().target() == Target::Process {
        return state
            .control()
            .prepare_json(plan_path, plan_sha256)
            .map_err(|error| error_response(error).into_response());
    }
    let reply = crate::forward(
        state,
        json!({ "op": "prepare", "plan_path": plan_path, "plan_sha256": plan_sha256 }),
    )
    .await;
    control_reply(reply)
}

/// 控制面 commit（块边界生效）。
#[allow(clippy::result_large_err)] // Err 是已经构造好的 axum Response：必须原样保留转发状态码/错误体
async fn control_commit(
    state: &std::sync::Arc<DaemonState>,
    engine_token: &str,
) -> Result<Value, Response> {
    if state.host_link().target() == Target::Process {
        return state
            .control()
            .commit_json(engine_token)
            .map_err(|error| error_response(error).into_response());
    }
    let reply = crate::forward(
        state,
        json!({ "op": "commit", "engine_token": engine_token }),
    )
    .await;
    control_reply(reply)
}

/// 控制面 abort（丢弃草稿）。
#[allow(clippy::result_large_err)] // Err 是已经构造好的 axum Response：必须原样保留转发状态码/错误体
async fn control_abort(
    state: &std::sync::Arc<DaemonState>,
    engine_token: &str,
) -> Result<Value, Response> {
    if state.host_link().target() == Target::Process {
        return state
            .control()
            .abort_json(engine_token)
            .map_err(|error| error_response(error).into_response());
    }
    let reply = crate::forward(
        state,
        json!({ "op": "abort", "engine_token": engine_token }),
    )
    .await;
    control_reply(reply)
}

/// 控制面 rollback 草稿（第一个阶段）。
#[allow(clippy::result_large_err)] // Err 是已经构造好的 axum Response：必须原样保留转发状态码/错误体
async fn control_rollback(
    state: &std::sync::Arc<DaemonState>,
    target: Option<u64>,
) -> Result<Value, Response> {
    if state.host_link().target() == Target::Process {
        return state
            .control()
            .rollback_json(target)
            .map_err(|error| error_response(error).into_response());
    }
    let mut request = json!({ "op": "rollback" });
    if let Some(version) = target {
        request["target"] = json!(version);
    }
    control_reply(crate::forward(state, request).await)
}

/// 当前控制面 status 快照（拿不到返回 null，由调用方如实处理）。
async fn control_status(state: &std::sync::Arc<DaemonState>) -> Value {
    if state.host_link().target() == Target::Process {
        return state.control().status_json();
    }
    let worker = std::sync::Arc::clone(state);
    match tokio::task::spawn_blocking(move || worker.host_link().status_snapshot()).await {
        Ok(LinkSnapshot::Socket { status, .. }) => status,
        _ => Value::Null,
    }
}

/// 转发应答 -> 成功取 body / 失败原样返回（保留 rt-host 的业务错误码与 HTTP 映射）。
#[allow(clippy::result_large_err)] // Err 是已经构造好的 axum Response（转发状态码/错误体原样保留）
fn control_reply((status, Json(body)): (StatusCode, Json<Value>)) -> Result<Value, Response> {
    if status.is_success() {
        Ok(body)
    } else {
        Err((status, Json(body)).into_response())
    }
}

/// apply 第一阶段：job 产物 -> 控制面 prepare 草稿 + 一次性 token。
async fn apply_prepare(state: &std::sync::Arc<DaemonState>, job_id: &str) -> Response {
    let service = std::sync::Arc::clone(state.calibration().service());
    let apply_plan = match service.apply(job_id) {
        Ok(plan) => plan,
        Err(error) => return error.into_response(),
    };
    let before = control_status(state).await;
    let plan_path = apply_plan.plan_path.to_string_lossy().to_string();
    let prepare = match control_prepare_plan(state, &plan_path, &apply_plan.plan_sha256).await {
        Ok(prepare) => prepare,
        Err(response) => return response,
    };
    let Some(engine_token) = prepare["engine_token"].as_str().map(str::to_string) else {
        return CalibrationError::internal("control plane prepare response has no engine_token")
            .into_response();
    };
    crate::record_prepared_plan(state, plan_path).await;
    let previous_version = before["actual_version"].as_u64();
    state.calibration().staging().stage(StagedControlOp {
        job_id: job_id.to_string(),
        kind: StagedControlKind::Apply,
        control_token: engine_token.clone(),
        plan_sha256: Some(apply_plan.plan_sha256.clone()),
        target_version: None,
        previous_version,
        created_at: now_rfc3339(),
    });
    let mut body = prepare;
    if let Some(object) = body.as_object_mut() {
        object.insert("stage".to_string(), json!("prepare"));
        object.insert("job_id".to_string(), json!(job_id));
        object.insert("plan_sha256".to_string(), json!(apply_plan.plan_sha256));
        object.insert("previous_version".to_string(), json!(previous_version));
        object.insert("engine_token".to_string(), json!(engine_token));
    }
    ok_json(body)
}

/// apply 第二阶段：commit 生效并回写 job apply 记录。
async fn apply_commit(
    state: &std::sync::Arc<DaemonState>,
    job_id: &str,
    engine_token: &str,
) -> Response {
    let Some(op) =
        state
            .calibration()
            .staging()
            .take(engine_token, job_id, StagedControlKind::Apply)
    else {
        return engine_token_invalid(engine_token).into_response();
    };
    let commit = match control_commit(state, engine_token).await {
        Ok(commit) => commit,
        Err(response) => return response,
    };
    crate::record_commit(state, &commit);
    let version = commit["version"].as_u64();
    let graph_id = commit["graph_id"].as_str().unwrap_or_default().to_string();
    let readback = control_status(state).await;
    let service = std::sync::Arc::clone(state.calibration().service());
    let mut reasons: BTreeMap<String, String> = BTreeMap::new();
    let job_value = match version {
        Some(version) => {
            let applied = AppliedVersion {
                version,
                graph_id: graph_id.clone(),
                previous_version: op.previous_version,
            };
            match service.apply_committed(job_id, &applied) {
                Ok(record) => job_json(&record),
                Err(error) => {
                    reasons.insert(
                        "job".to_string(),
                        format!(
                            "the plan was applied but the calibration service could not record it: {}",
                            error.message()
                        ),
                    );
                    Value::Null
                }
            }
        }
        None => {
            reasons.insert(
                "applied_version".to_string(),
                "control plane commit response has no version field".to_string(),
            );
            Value::Null
        }
    };
    ok_json(json!({
        "stage": "commit",
        "applied": true,
        "job_id": job_id,
        "applied_version": version,
        "applied_block": commit["applied_block"].clone(),
        "graph_id": graph_id,
        "sha256": op.plan_sha256,
        "previous_version": op.previous_version,
        "readback": readback,
        "job": job_value,
        "reasons": reasons,
    }))
}

/// apply abort：丢弃控制面草稿，不改变运行态。
async fn apply_abort(
    state: &std::sync::Arc<DaemonState>,
    job_id: &str,
    engine_token: &str,
) -> Response {
    let Some(op) =
        state
            .calibration()
            .staging()
            .take(engine_token, job_id, StagedControlKind::Apply)
    else {
        return engine_token_invalid(engine_token).into_response();
    };
    let abort = match control_abort(state, engine_token).await {
        Ok(abort) => abort,
        Err(response) => return response,
    };
    ok_json(json!({
        "stage": "abort",
        "aborted": true,
        "job_id": job_id,
        "plan_sha256": op.plan_sha256,
        "control": abort,
    }))
}

/// rollback 第一阶段：job apply 前的版本 -> 控制面 rollback 草稿 + token。
async fn rollback_prepare(state: &std::sync::Arc<DaemonState>, job_id: &str) -> Response {
    let service = std::sync::Arc::clone(state.calibration().service());
    let target = match service.rollback(job_id) {
        Ok(target) => target,
        Err(error) => return error.into_response(),
    };
    let before = control_status(state).await;
    let prepare = match control_rollback(state, Some(target.target_version)).await {
        Ok(prepare) => prepare,
        Err(response) => return response,
    };
    let Some(engine_token) = prepare["engine_token"].as_str().map(str::to_string) else {
        return CalibrationError::internal("control plane rollback response has no engine_token")
            .into_response();
    };
    let previous_version = before["actual_version"].as_u64();
    state.calibration().staging().stage(StagedControlOp {
        job_id: job_id.to_string(),
        kind: StagedControlKind::Rollback,
        control_token: engine_token.clone(),
        plan_sha256: None,
        target_version: Some(target.target_version),
        previous_version,
        created_at: now_rfc3339(),
    });
    let mut body = prepare;
    if let Some(object) = body.as_object_mut() {
        object.insert("stage".to_string(), json!("prepare"));
        object.insert("job_id".to_string(), json!(job_id));
        object.insert("target_version".to_string(), json!(target.target_version));
        object.insert("previous_version".to_string(), json!(previous_version));
        object.insert("engine_token".to_string(), json!(engine_token));
    }
    ok_json(body)
}

/// rollback 第二阶段：commit 生效并回写 job rolled_back 记录。
async fn rollback_commit(
    state: &std::sync::Arc<DaemonState>,
    job_id: &str,
    engine_token: &str,
) -> Response {
    let Some(op) =
        state
            .calibration()
            .staging()
            .take(engine_token, job_id, StagedControlKind::Rollback)
    else {
        return engine_token_invalid(engine_token).into_response();
    };
    let commit = match control_commit(state, engine_token).await {
        Ok(commit) => commit,
        Err(response) => return response,
    };
    crate::record_commit(state, &commit);
    let version = commit["version"].as_u64();
    let graph_id = commit["graph_id"].as_str().unwrap_or_default().to_string();
    let readback = control_status(state).await;
    let service = std::sync::Arc::clone(state.calibration().service());
    let mut reasons: BTreeMap<String, String> = BTreeMap::new();
    let job_value = match version {
        Some(version) => {
            let applied = AppliedVersion {
                version,
                graph_id: graph_id.clone(),
                previous_version: op.target_version,
            };
            match service.rollback_committed(job_id, &applied) {
                Ok(record) => job_json(&record),
                Err(error) => {
                    reasons.insert(
                        "job".to_string(),
                        format!(
                            "the plan was rolled back but the calibration service could not record it: {}",
                            error.message()
                        ),
                    );
                    Value::Null
                }
            }
        }
        None => {
            reasons.insert(
                "applied_version".to_string(),
                "control plane commit response has no version field".to_string(),
            );
            Value::Null
        }
    };
    ok_json(json!({
        "stage": "commit",
        "rolled_back": true,
        "job_id": job_id,
        "restored_version": op.target_version,
        "applied_version": version,
        "graph_id": graph_id,
        "readback": readback,
        "job": job_value,
        "reasons": reasons,
    }))
}

/// rollback abort：丢弃控制面草稿，不改变运行态。
async fn rollback_abort(
    state: &std::sync::Arc<DaemonState>,
    job_id: &str,
    engine_token: &str,
) -> Response {
    let Some(op) =
        state
            .calibration()
            .staging()
            .take(engine_token, job_id, StagedControlKind::Rollback)
    else {
        return engine_token_invalid(engine_token).into_response();
    };
    let abort = match control_abort(state, engine_token).await {
        Ok(abort) => abort,
        Err(response) => return response,
    };
    ok_json(json!({
        "stage": "abort",
        "aborted": true,
        "job_id": job_id,
        "target_version": op.target_version,
        "control": abort,
    }))
}

// ---------------------------------------------------------------------------
// 产物下载
// ---------------------------------------------------------------------------

/// GET /api/calibration/jobs/{id}/artifacts/{name}
pub(crate) async fn download_artifact(
    State(state): State<std::sync::Arc<DaemonState>>,
    Path((job_id, name)): Path<(String, String)>,
) -> Response {
    if let Err(error) = validate_job_id(&job_id) {
        return error.into_response();
    }
    // 白名单先于任何后端调用：不在白名单的名字一律 404（不泄露存在性）。
    if !store::is_safe_artifact_name(&name) || !store::is_whitelisted_artifact(&name) {
        return CalibrationError::artifact_not_found(&job_id, &name).into_response();
    }
    let service = state.calibration().service();
    let artifacts = match service.artifacts(&job_id) {
        Ok(artifacts) => artifacts,
        Err(error) => return error.into_response(),
    };
    let Some(info) = artifacts.iter().find(|artifact| artifact.name == name) else {
        return CalibrationError::artifact_not_found(&job_id, &name).into_response();
    };
    let bytes = match service.read_artifact(&job_id, &name) {
        Ok(bytes) => bytes,
        Err(error) => return error.into_response(),
    };
    let actual = sha256_hex(&bytes);
    if !actual.eq_ignore_ascii_case(info.sha256.trim()) {
        return CalibrationError::artifact_sha_mismatch(
            &job_id,
            &name,
            info.sha256.trim(),
            &actual,
        )
        .into_response();
    }
    let content_type = info
        .content_type
        .clone()
        .unwrap_or_else(|| store::artifact_content_type(&name).to_string());
    let mut response = (StatusCode::OK, bytes).into_response();
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&content_type) {
        headers.insert(CONTENT_TYPE, value);
    }
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
        headers.insert(CONTENT_DISPOSITION, value);
    }
    if let Ok(value) = HeaderValue::from_str(&info.sha256) {
        headers.insert("x-khaen-artifact-sha256", value);
    }
    response
}

// ---------------------------------------------------------------------------
// wizard
// ---------------------------------------------------------------------------

fn wizard_json(load: &wizard::WizardLoad, saved_fields: Option<Vec<String>>) -> Value {
    let mut reasons: Map<String, Value> = wizard::config_reasons(&load.stored);
    if let Some(corruption) = &load.corruption {
        reasons.insert("wizard_store".to_string(), json!(corruption));
    }
    let mut body = json!({
        "schema": WIZARD_SCHEMA,
        "config": load.stored.config_json(),
        "defaults": wizard::WizardStored::defaults_json(),
        "saved": load.saved,
        "updated_at": load.updated_at,
        "reasons": Value::Object(reasons),
    });
    if let Some(fields) = saved_fields {
        body["set_fields"] = json!(fields);
    }
    body
}

/// GET /api/calibration/wizard
pub(crate) async fn get_wizard(State(state): State<std::sync::Arc<DaemonState>>) -> Response {
    let load = state.calibration().wizard().load();
    ok_json(wizard_json(&load, None))
}

/// PUT /api/calibration/wizard（部分字段；未知字段 400）
pub(crate) async fn put_wizard(
    State(state): State<std::sync::Arc<DaemonState>>,
    body: Body,
) -> Response {
    let value = match read_json_body(body).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if !value.is_object() {
        return CalibrationError::invalid_request(
            "<body>",
            "wizard PUT body must be a JSON object",
        )
        .into_response();
    }
    let hub = state.calibration();
    let load = hub.wizard().load();
    let outcome = match wizard::apply_wizard_patch(&load.stored, hub.uploads(), &value) {
        Ok(outcome) => outcome,
        Err(error) => return error.into_response(),
    };
    let updated_at = match hub.wizard().save(&outcome.stored) {
        Ok(updated_at) => updated_at,
        Err(error) => return error.into_response(),
    };
    let saved = wizard::WizardLoad {
        stored: outcome.stored,
        saved: true,
        updated_at: Some(updated_at),
        corruption: None,
    };
    ok_json(wizard_json(&saved, Some(outcome.set_fields)))
}

/// GET /api/calibration/wizard/state
pub(crate) async fn get_wizard_state(State(state): State<std::sync::Arc<DaemonState>>) -> Response {
    let hub = state.calibration();
    let load = hub.wizard().load();
    let aggregated = crate::status::aggregate(&state, SystemTime::now());
    let control_status = state.control().status_json();
    let calibration = hub.status();
    let service = std::sync::Arc::clone(hub.service());
    // 最近 job：优先状态里的 last_job_id，其次列表第一条（后端按新→旧返回）。
    let last_job_id = calibration.last_job_id.clone();
    let last_job = match last_job_id {
        Some(job_id) => service.job(&job_id).ok(),
        None => match service.list_jobs(&JobQuery {
            limit: 1,
            offset: 0,
        }) {
            Ok(page) => page
                .jobs
                .first()
                .and_then(|summary| service.job(&summary.job_id).ok()),
            Err(_) => None,
        },
    };
    let measurement = measurement_draft(hub, &load.stored, last_job.as_ref());
    let updated_at = now_rfc3339();
    let mut value = wizard::derive_state(WizardStateInputs {
        status: &aggregated,
        control_status: &control_status,
        calibration: &calibration,
        last_job: last_job.as_ref(),
        stored: &load.stored,
        measurement: &measurement,
        updated_at: &updated_at,
    });
    // 配置损坏/未保存也要在状态里如实说明（向导配置是第 3 步 gate 的来源）。
    if let Some(corruption) = &load.corruption {
        if let Some(reasons) = value["reasons"].as_object_mut() {
            reasons.insert("wizard_store".to_string(), json!(corruption));
        }
    }
    if !load.saved {
        if let Some(reasons) = value["reasons"].as_object_mut() {
            reasons.insert(
                "wizard".to_string(),
                json!("wizard settings have not been saved yet: PUT /api/calibration/wizard persists them to the state directory"),
            );
        }
    }
    // 测量草稿的缺失原因（step5 刷新恢复依赖它）。
    if let Some(reasons) = value["reasons"].as_object_mut() {
        if measurement.baseline_upload_id.is_none() && measurement.applied_upload_id.is_none() {
            reasons.insert(
                "measurement".to_string(),
                json!("no offline measurement uploads recorded yet: upload the baseline and applied WAV files, then PUT /api/calibration/wizard with measurement (or start a job)"),
            );
        }
    }
    value["updated_at"] = json!(updated_at);
    ok_json(value)
}

/// 组装第 5 步的测量草稿：最近 job inputs 优先，其次 wizard.json 的 measurement。
fn measurement_draft(
    hub: &CalibrationHub,
    stored: &wizard::WizardStored,
    last_job: Option<&JobRecord>,
) -> MeasurementDraft {
    let mut draft = if let Some(inputs) = last_job.and_then(|job| job.inputs.as_ref()) {
        MeasurementDraft {
            baseline_upload_id: Some(inputs.baseline_upload_id.clone()),
            applied_upload_id: Some(inputs.applied_upload_id.clone()),
            baseline_sha256: None,
            applied_sha256: None,
        }
    } else {
        MeasurementDraft {
            baseline_upload_id: stored.measurement.baseline_upload_id.clone(),
            applied_upload_id: stored.measurement.applied_upload_id.clone(),
            baseline_sha256: None,
            applied_sha256: None,
        }
    };
    for (id, sha) in [
        (draft.baseline_upload_id.clone(), &mut draft.baseline_sha256),
        (draft.applied_upload_id.clone(), &mut draft.applied_sha256),
    ] {
        if let Some(upload_id) = id {
            if let Ok(Some(record)) = hub.uploads().lookup(&upload_id) {
                *sha = Some(record.sha256);
            }
        }
    }
    draft
}

// ---------------------------------------------------------------------------
// 麦克风 profile（C1：UMIK 校准文件 + MicProfile；rt-host 未接线时 status 如实降级）
// ---------------------------------------------------------------------------

fn mic_profile_json(load: &mic::MicProfileLoad) -> Value {
    json!({
        "schema": mic::MIC_PROFILE_SCHEMA,
        "profile": load.profile.as_ref().map(|profile| profile.to_json()),
        "reasons": load.reasons,
    })
}

/// GET /api/calibration/mic：当前 MicProfile + reasons（无 profile 时 200 + null + 原因）。
pub(crate) async fn get_mic_profile(State(state): State<std::sync::Arc<DaemonState>>) -> Response {
    let load = state.calibration().mic().load();
    ok_json(mic_profile_json(&load))
}

/// PUT /api/calibration/mic：部分更新（未知字段 400 calibration_mic_unknown_field）。
pub(crate) async fn put_mic_profile(
    State(state): State<std::sync::Arc<DaemonState>>,
    body: Body,
) -> Response {
    let value = match read_json_body(body).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let store = state.calibration().mic();
    let current = store.load();
    let profile = match mic::apply_profile_patch(current.profile.as_ref(), store, &value) {
        Ok(profile) => profile,
        Err(error) => return error.into_response(),
    };
    let load = mic::MicProfileLoad {
        profile: Some(profile),
        reasons: Vec::new(),
        corruption: None,
    };
    ok_json(mic_profile_json(&load))
}

// ---------------------------------------------------------------------------
// 实时 rt-host UMIK 状态（C3a：GET /api/calibration/mic/status 的 rt_host 段）
// ---------------------------------------------------------------------------

/// process 目标下实时 UMIK 状态的固定降级原因（C3a；process 目标没有校准能力）。
pub const PROCESS_TARGET_MIC_REASON: &str =
    "rt-host control target required for live UMIK status (process target cannot calibrate)";
/// rt-host `cal_status` 回执不是可用状态文档时的稳定码（缺 `umik`/字段类型不符）。
pub const MALFORMED_CAL_STATUS: &str = "malformed_cal_status_reply";

/// 不可用的 `rt_host` 段：稳定码 + 含 code/message 的原因（HTTP 仍 200）。
fn rt_host_unavailable(code: &str, reason: impl Into<String>) -> Value {
    json!({
        "available": false,
        "code": code,
        "reason": reason.into(),
    })
}

/// 字段读取：缺失/显式 null → null（不编），类型不符 → Err（由调用方按畸形回执降级）。
fn cal_status_string_field(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<Value, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(Value::Null),
        Some(Value::String(text)) => Ok(Value::String(text.clone())),
        Some(_) => Err(format!("{path} must be a string or null")),
    }
}

fn cal_status_bool_field(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<Value, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(Value::Null),
        Some(Value::Bool(value)) => Ok(Value::Bool(*value)),
        Some(_) => Err(format!("{path} must be a boolean or null")),
    }
}

/// 把 rt-host `cal_status` 回执映射成 status 的 `rt_host` 段（可用时）。
///
/// 只做加法映射：`mode` ← 顶层 `mode`；`declared`/`open`/`device`/
/// `declaration_source` ← `umik.*`；`reasons` ← 顶层 `reasons.mic`（UMIK 声明缺失
/// 原因）；`calibration.{simulated,engine}` ← 顶层 `simulated`/`engine`。
/// 字段缺失/显式 null 如实 null（不编）；`umik` 缺失或字段类型不符 → Err（畸形回执）。
fn cal_status_rt_host_fields(status: &Value) -> Result<Value, String> {
    let object = status
        .as_object()
        .ok_or_else(|| "reply must be a JSON object".to_string())?;
    if let Some(ok) = object.get("ok") {
        if ok != &Value::Bool(true) {
            return Err(format!("reply ok must be true, got {ok}"));
        }
    }
    let umik = match object.get("umik") {
        Some(Value::Object(umik)) => umik,
        None | Some(Value::Null) => {
            return Err(
                "reply has no umik object (cal_status must report the UMIK declaration)"
                    .to_string(),
            )
        }
        Some(_) => return Err("umik must be a JSON object".to_string()),
    };
    let mode = cal_status_string_field(object, "mode", "mode")?;
    let declared = cal_status_bool_field(umik, "declared", "umik.declared")?;
    let open = cal_status_bool_field(umik, "open", "umik.open")?;
    let device = cal_status_string_field(umik, "device", "umik.device")?;
    let declaration_source =
        cal_status_string_field(umik, "declaration_source", "umik.declaration_source")?;
    // UMIK 声明缺失原因：rt-host 把它放在顶层 reasons.mic（字符串），daemon 原样透出。
    let reasons = match object.get("reasons") {
        None | Some(Value::Null) => Value::Null,
        Some(Value::Object(reasons)) => match reasons.get("mic") {
            None | Some(Value::Null) => Value::Null,
            Some(Value::String(text)) => Value::String(text.clone()),
            Some(_) => return Err("reasons.mic must be a string or null".to_string()),
        },
        Some(_) => return Err("reasons must be a JSON object or null".to_string()),
    };
    let simulated = cal_status_bool_field(object, "simulated", "simulated")?;
    let engine = cal_status_string_field(object, "engine", "engine")?;
    Ok(json!({
        "available": true,
        "reason": Value::Null,
        "mode": mode,
        "declared": declared,
        "open": open,
        "device": device,
        "declaration_source": declaration_source,
        "reasons": reasons,
        "calibration": {
            "simulated": simulated,
            "engine": engine,
        },
    }))
}

/// 读取 rt-host 的实时 UMIK 状态（只读 `cal_status`；阻塞 socket 往返放 blocking 线程池）。
///
/// 只对 `rt-host` 目标发请求；process 目标没有校准能力，如实降级。任何失败（链路不可用、
/// rt-host 未接线/拒绝、回执畸形）都映射成 `available:false` + 稳定 `code` + 含
/// code/message 的 `reason`，不吞错、不编 declared/open；HTTP 状态由调用方保持 200。
async fn live_rt_host_mic_status(state: &std::sync::Arc<DaemonState>) -> Value {
    if state.host_link().target() == Target::Process {
        return rt_host_unavailable("process_target", PROCESS_TARGET_MIC_REASON);
    }
    let link = std::sync::Arc::clone(state.host_link());
    match tokio::task::spawn_blocking(move || link.call(json!({ "op": "cal_status" }))).await {
        Ok(Ok(WireReply::Success(status))) => match cal_status_rt_host_fields(&status) {
            Ok(fields) => fields,
            Err(problem) => rt_host_unavailable(
                MALFORMED_CAL_STATUS,
                format!(
                    "rt-host cal_status reply is malformed ({MALFORMED_CAL_STATUS}): {problem}"
                ),
            ),
        },
        Ok(Ok(WireReply::Failure { code, message, .. })) => rt_host_unavailable(
            &code,
            format!("rt-host rejected cal_status ({code}): {message}"),
        ),
        Ok(Err(error)) => rt_host_unavailable(
            HOST_LINK_UNAVAILABLE,
            format!("{HOST_LINK_UNAVAILABLE}: {}", error.message),
        ),
        Err(join_error) => rt_host_unavailable(
            "internal_error",
            format!("cal_status forwarding task failed (internal_error): {join_error}"),
        ),
    }
}

/// GET /api/calibration/mic/status：MicProfile + rt-host 实时 UMIK 状态（恒 200，只读）。
///
/// profile/reasons 仍来自 MicProfileStore（C1，不受 rt-host 状态影响）；`rt_host` 来自
/// host link 的 `cal_status`（C3a）。rt-host 未接线/不可用/回执畸形时如实降级为
/// `available:false` + 稳定 code/reason，**绝不编造 declared/open**。
pub(crate) async fn get_mic_status(State(state): State<std::sync::Arc<DaemonState>>) -> Response {
    let load = state.calibration().mic().load();
    let mut body = mic_profile_json(&load);
    body["rt_host"] = live_rt_host_mic_status(&state).await;
    ok_json(body)
}

/// POST /api/calibration/mic/calibration：原始 UMIK txt body（≤1 MiB），校验后内容寻址存盘。
pub(crate) async fn upload_mic_calibration(
    State(state): State<std::sync::Arc<DaemonState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    // 1) 文件名（X-KHAEN-FILENAME 可选；只接受 bare filename，缺省 umik.txt）。
    let filename = match mic::calibration_filename_from_headers(&headers) {
        Ok(filename) => filename,
        Err(error) => return error.into_response(),
    };
    // 2) Content-Length 预检（超限直接 413，不读 body）。
    if let Some(length) = content_length(&headers) {
        if length > mic::MAX_CAL_BYTES {
            return mic::too_large_bytes().into_response();
        }
    }
    // 3) 读取 body（有上限；chunked 超限在读取阶段报错）。
    let bytes = match axum::body::to_bytes(body, mic::MAX_CAL_BYTES as usize).await {
        Ok(bytes) => bytes,
        Err(error) => {
            if is_length_limit_error(&error) {
                return mic::too_large_bytes().into_response();
            }
            return mic::invalid(format!("cannot read microphone calibration body: {error}"))
                .into_response();
        }
    };
    // 4) 解析 + 内容寻址写盘（阻塞 IO 放 spawn_blocking，不堵 async worker）。
    let store = state.calibration().mic().clone();
    let stored =
        tokio::task::spawn_blocking(move || store.store_calibration(&filename, &bytes)).await;
    match stored {
        Ok(Ok(record)) => ok_json(json!({
            "sha256": record.sha256,
            "filename": record.filename,
            "bytes": record.bytes,
            "serial": record.parsed.serno,
            "parsed": record.parsed.to_json(),
        })),
        Ok(Err(error)) => error.into_response(),
        Err(join) => {
            CalibrationError::internal(format!("microphone calibration worker failed: {join}"))
                .into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// 单元测试（纯 JSON/校验逻辑；HTTP 端到端在 tests/calibration_*.rs）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calibration::service::code;

    #[test]
    fn page_query_defaults_and_bounds() {
        let mut params = HashMap::new();
        assert_eq!(
            parse_page_query(&params).expect("defaults"),
            JobQuery {
                limit: DEFAULT_LIMIT,
                offset: 0
            }
        );
        params.insert("limit".into(), "5".into());
        params.insert("offset".into(), "10".into());
        assert_eq!(
            parse_page_query(&params).expect("explicit"),
            JobQuery {
                limit: 5,
                offset: 10
            }
        );
        for (key, value) in [
            ("limit", "0"),
            ("limit", "101"),
            ("limit", "x"),
            ("offset", "-1"),
            ("other", "1"),
        ] {
            let mut params = HashMap::new();
            params.insert(key.into(), value.into());
            let error = parse_page_query(&params).expect_err("rejected");
            assert_eq!(error.code(), code::INVALID_REQUEST, "{key}={value}");
        }
    }

    #[test]
    fn job_id_validation_rejects_path_tricks() {
        assert!(validate_job_id("job-0001").is_ok());
        assert!(validate_job_id("a.b_c-1").is_ok());
        for bad in ["", "..", "a/b", "a\\b", "job 1", &"x".repeat(65)] {
            let error = validate_job_id(bad).expect_err("rejected");
            assert_eq!(error.code(), code::INVALID_REQUEST, "{bad:?}");
        }
    }

    #[test]
    fn job_reasons_fill_every_missing_field_without_inventing_values() {
        let new_job = NewJob {
            kind: crate::calibration::service::JOB_KIND_SINGLE_POINT_OFFLINE.to_string(),
            inputs: JobInputs {
                plan_sha: "0".repeat(64),
                baseline_upload_id: "1".repeat(64),
                applied_upload_id: "2".repeat(64),
                centers_hz: vec![100.0, 1000.0],
                tolerance_db: 2.0,
                min_bands: 2,
            },
            baseline: crate::calibration::service::UploadRef {
                upload_id: "1".repeat(64),
                sha256: "1".repeat(64),
                bytes: 1,
                path: std::path::PathBuf::from("a"),
            },
            applied: crate::calibration::service::UploadRef {
                upload_id: "2".repeat(64),
                sha256: "2".repeat(64),
                bytes: 2,
                path: std::path::PathBuf::from("b"),
            },
        };
        let record = JobRecord::new_queued("job-0001", &new_job, None);
        let job = job_json(&record);
        assert_eq!(job["id"], json!("job-0001"));
        assert_eq!(job["status"], json!("queued"));
        assert_eq!(job["verdict"], Value::Null);
        assert_eq!(job["steps"].as_array().expect("steps").len(), 5);
        for step in job["steps"].as_array().expect("steps") {
            assert_eq!(step["status"], json!("pending"));
            assert_eq!(step["sha256"], Value::Null);
        }
        let reasons = job_reasons(&record);
        for path in [
            "verdict",
            "created_at",
            "started_at",
            "finished_at",
            "duration_ms",
            "engine",
            "result",
            "apply.applied_version",
        ] {
            assert!(
                reasons.get(path).is_some_and(|value| value.is_string()),
                "{path} 缺原因"
            );
        }
        assert!(
            reasons.get("inputs").is_none(),
            "inputs 有回显，不该有缺失原因"
        );
    }

    #[test]
    fn json_body_helpers_are_strict() {
        let object = serde_json::json!({"a": 1, "b": "x"});
        let object = object.as_object().expect("object");
        assert!(reject_unknown_fields(object, &["a", "b"], "").is_ok());
        let error = reject_unknown_fields(object, &["a"], "").expect_err("unknown");
        assert_eq!(error.code(), code::INVALID_REQUEST);
        assert_eq!(required_string(object, "b").expect("string"), "x");
        assert!(required_string(object, "missing").is_err());
        assert!(required_u64(object, "a").is_ok());
        assert!(required_number(object, "a").is_ok());
    }

    #[test]
    fn centers_validation_rejects_unsorted_and_out_of_range() {
        assert!(parse_centers(Some(&json!([100.0, 1000.0]))).is_ok());
        for bad in [
            json!([]),
            json!([1000.0, 100.0]),
            json!([0.0]),
            json!([25000.0]),
            json!(["100"]),
            json!([100.0, 100.0]),
        ] {
            assert!(parse_centers(Some(&bad)).is_err(), "{bad}");
        }
        assert!(parse_centers(None).is_err());
    }
}
