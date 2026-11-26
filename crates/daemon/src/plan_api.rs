//! M4 方案 REST（T55）：POST /api/plan/upload、GET /api/plan/history。
//!
//! - POST /api/plan/upload：原始 ABI v3 字节（Content-Type 不限；上限 4 MiB）。先查
//!   Content-Length / body 长度，再校验可选 sha256（query 的 sha256 或请求头
//!   X-KHAEN-Plan-SHA256），然后解码校验 ABI v3，最后内容寻址存到
//!   `<KHAEN_STATE_DIR>/plans/<sha256>.plan`（同内容幂等：201 首次 / 200 复用）。响应带
//!   graph_id / segments / latency_samples / routing / admission 等真实元数据。
//! - GET /api/plan/history?limit=&offset=：daemon 见到的已生效历史 + 当前生效版本。
//!   条目来自 commit 回执（/api/commit 成功时记录）与 rt-host / 控制面的
//!   available_versions；历史版本的 graph_id 控制面不暴露时如实 null + reasons。
//!
//! 错误体保持 M1 契约：{"error":{"code","message","details"}}；参数错 400 invalid_request，
//! 超限 413 plan_too_large，sha 不符 400 plan_sha_mismatch，解码失败 400 plan_decode_failed，
//! 存储 IO 失败 500 internal_error。

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::SystemTime;

use axum::body::Body;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Map, Value};

use crate::host_link::{WireReply, HOST_LINK_UNAVAILABLE};
use crate::plan_store::{
    CommitRecord, PlanStoreError, HISTORY_DEFAULT_LIMIT, HISTORY_MAX_LIMIT, MAX_PLAN_BYTES,
    SCHEMA_HISTORY,
};
use crate::system::error_code as code;
use crate::DaemonState;

/// 上传体读取上限（比契约上限多留 1 字节用于区分"恰好 4 MiB"与"超限"）。
const BODY_READ_LIMIT: usize = MAX_PLAN_BYTES + 1;

/// 组装 /api/plan/* 路由（挂到全局 auth 中间件之下）。
pub fn routes() -> Router<Arc<DaemonState>> {
    Router::new()
        .route("/api/plan/upload", post(upload))
        .route("/api/plan/history", axum::routing::get(history))
}

fn api_error(
    status: StatusCode,
    error_code: &'static str,
    message: impl Into<String>,
    details: Value,
) -> Response {
    (
        status,
        Json(json!({
            "error": {
                "code": error_code,
                "message": message.into(),
                "details": details,
            }
        })),
    )
        .into_response()
}

fn invalid_request(message: impl Into<String>, details: Value) -> Response {
    api_error(
        StatusCode::BAD_REQUEST,
        code::INVALID_REQUEST,
        message,
        details,
    )
}

// ---------------------------------------------------------------------------
// POST /api/plan/upload
// ---------------------------------------------------------------------------

/// POST /api/plan/upload：原始 ABI v3 字节（上限 4 MiB）。
pub(crate) async fn upload(
    State(state): State<Arc<DaemonState>>,
    headers: HeaderMap,
    query: Result<Query<HashMap<String, String>>, QueryRejection>,
    body: Body,
) -> Response {
    let params = match query {
        Ok(Query(params)) => params,
        Err(rejection) => {
            return invalid_request(
                format!("invalid query string: {}", rejection.body_text()),
                json!({"field": "<query>"}),
            )
        }
    };
    // 1) 读 body（上限判定与 axum 的长度限制错误都映射成 413）。
    //    注意先读完（最多 MAX+1 字节）再回 413：早退会让未读的请求体触发连接 reset，
    //    对只用 Content-Length 判断的客户端不友好；这里让 hyper 正常排空/关闭连接。
    let bytes = match axum::body::to_bytes(body, BODY_READ_LIMIT).await {
        Ok(bytes) => bytes,
        Err(error) => {
            if is_length_limit_error(&error) {
                return api_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "plan_too_large",
                    format!("plan upload exceeds the {MAX_PLAN_BYTES}-byte limit"),
                    json!({"max_bytes": MAX_PLAN_BYTES}),
                );
            }
            return invalid_request(
                format!("cannot read upload body: {error}"),
                json!({"field": "<body>"}),
            );
        }
    };
    if bytes.is_empty() {
        return invalid_request("upload body must not be empty", json!({"field": "<body>"}));
    }
    if bytes.len() > MAX_PLAN_BYTES {
        return api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "plan_too_large",
            format!(
                "plan upload is {} bytes, exceeding the {MAX_PLAN_BYTES}-byte limit",
                bytes.len()
            ),
            json!({"max_bytes": MAX_PLAN_BYTES, "received_bytes": bytes.len()}),
        );
    }
    let actual_sha = khaen_host::sha256_hex(&bytes);
    // 3) 可选 sha256 校验（query sha256=/sha= 或请求头 X-KHAEN-Plan-SHA256 / X-KHAEN-SHA256）。
    if let Some(expected) = expected_sha(&params, &headers) {
        match expected {
            Err(message) => {
                return invalid_request(message, json!({"field": "sha256"}));
            }
            Ok(expected) => {
                if !actual_sha.eq_ignore_ascii_case(&expected) {
                    return api_error(
                        StatusCode::BAD_REQUEST,
                        "plan_sha_mismatch",
                        format!(
                            "plan sha256 mismatch: request expected {expected}, upload body is {actual_sha}"
                        ),
                        json!({"expected": expected, "actual": actual_sha}),
                    );
                }
            }
        }
    }
    // 4) 解码 + 内容寻址落盘（阻塞 IO/解码放 blocking 线程池）。
    let store = state.plans().clone();
    let payload = bytes.clone();
    let now = crate::status::rfc3339_utc(SystemTime::now());
    let stored = tokio::task::spawn_blocking(move || store.store_upload(&payload, &now)).await;
    match stored {
        Ok(Ok((record, created))) => {
            let mut body = record.to_json();
            if let Some(object) = body.as_object_mut() {
                object.insert("ok".to_string(), Value::Bool(true));
                object.insert(
                    "schema".to_string(),
                    Value::String("khaen-plan/v1".to_string()),
                );
                object.insert("created".to_string(), Value::Bool(created));
                object.insert("deduplicated".to_string(), Value::Bool(!created));
            }
            let status = if created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            };
            (status, Json(body)).into_response()
        }
        Ok(Err(error)) => plan_store_error(&error),
        Err(join_error) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            code::INTERNAL_ERROR,
            format!("plan upload task failed: {join_error}"),
            json!({}),
        ),
    }
}

fn plan_store_error(error: &PlanStoreError) -> Response {
    let status = match error {
        PlanStoreError::Decode(_) => StatusCode::BAD_REQUEST,
        PlanStoreError::TooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
        PlanStoreError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    let details = match error {
        PlanStoreError::Decode(reason) => json!({"reason": reason}),
        PlanStoreError::TooLarge(size) => {
            json!({"max_bytes": MAX_PLAN_BYTES, "received_bytes": size})
        }
        PlanStoreError::Io(reason) => json!({"reason": reason}),
    };
    api_error(status, error.code(), error.message(), details)
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

/// 可选 sha256 校验值的来源（query 优先，其次请求头）；格式非法返回 Err(人读原因)。
fn expected_sha(
    params: &HashMap<String, String>,
    headers: &HeaderMap,
) -> Option<Result<String, String>> {
    let raw = params
        .get("sha256")
        .or_else(|| params.get("sha"))
        .cloned()
        .or_else(|| {
            ["x-khaen-plan-sha256", "x-khaen-sha256"]
                .iter()
                .find_map(|name| headers.get(*name))
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        })?;
    let trimmed = raw.trim().to_string();
    if trimmed.len() != 64
        || !trimmed
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Some(Err(format!(
            "sha256 must be 64 hex characters (got {trimmed:?})"
        )));
    }
    Some(Ok(trimmed))
}

// ---------------------------------------------------------------------------
// GET /api/plan/history
// ---------------------------------------------------------------------------

/// GET /api/plan/history?limit=&offset=：已生效历史（commit 回执 + 当前生效版本）。
pub(crate) async fn history(
    State(state): State<Arc<DaemonState>>,
    query: Result<Query<HashMap<String, String>>, QueryRejection>,
) -> Response {
    let params = match query {
        Ok(Query(params)) => params,
        Err(rejection) => {
            return invalid_request(
                format!("invalid query string: {}", rejection.body_text()),
                json!({"field": "<query>"}),
            )
        }
    };
    let limit = match params.get("limit") {
        None => HISTORY_DEFAULT_LIMIT,
        Some(raw) => match raw.trim().parse::<usize>() {
            Ok(limit) if (1..=HISTORY_MAX_LIMIT).contains(&limit) => limit,
            _ => {
                return invalid_request(
                    format!("limit must be an integer in 1..={HISTORY_MAX_LIMIT} (got {raw:?})"),
                    json!({"field": "limit", "min": 1, "max": HISTORY_MAX_LIMIT}),
                )
            }
        },
    };
    let offset = match params.get("offset") {
        None => 0,
        Some(raw) => match raw.trim().parse::<usize>() {
            Ok(offset) => offset,
            Err(_) => {
                return invalid_request(
                    format!("offset must be a non-negative integer (got {raw:?})"),
                    json!({"field": "offset"}),
                )
            }
        },
    };
    let worker = Arc::clone(&state);
    let now = SystemTime::now();
    match tokio::task::spawn_blocking(move || compose_history(&worker, now, limit, offset)).await {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(join_error) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            code::INTERNAL_ERROR,
            format!("plan history task failed: {join_error}"),
            json!({}),
        ),
    }
}

/// 已生效历史的实时数据源（rt-host / 进程内控制面 / 状态文件 / 无）。
#[derive(Debug, Default)]
struct HistoryLive {
    source: &'static str,
    current_version: Option<u64>,
    current_graph_id: Option<String>,
    available_versions: Vec<u64>,
    reasons: BTreeMap<String, String>,
}

/// 现取历史数据源（同步；调用方在 blocking 线程池里）。
fn history_live(state: &DaemonState, now: SystemTime) -> HistoryLive {
    match state.host_link().target() {
        crate::host_link::Target::Process => {
            let status = state.control().status_json();
            let coeff = state.control().coeff_json();
            let mut reasons = BTreeMap::new();
            let available_versions = versions_from(&coeff, "available_versions", &mut reasons);
            HistoryLive {
                source: "daemon-process",
                current_version: status.get("actual_version").and_then(Value::as_u64),
                current_graph_id: status
                    .get("graph_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                available_versions,
                reasons,
            }
        }
        crate::host_link::Target::RtHost => {
            let link = state.host_link().status_snapshot();
            let mut reasons = BTreeMap::new();
            match link {
                crate::host_link::LinkSnapshot::Socket { status, .. } => {
                    let current_version = status.get("actual_version").and_then(Value::as_u64);
                    let current_graph_id = status
                        .get("graph_id")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    // coeff op 是只读的；rt-host 的可用版本（历史）由它上报。
                    let available_versions = match state.host_link().call(json!({ "op": "coeff" }))
                    {
                        Ok(WireReply::Success(body)) => {
                            versions_from(&body, "available_versions", &mut reasons)
                        }
                        Ok(WireReply::Failure {
                            code: failure_code,
                            message,
                            ..
                        }) => {
                            reasons.insert(
                                "history.available_versions".to_string(),
                                format!("rt-host coeff op failed: {failure_code}: {message}"),
                            );
                            Vec::new()
                        }
                        Err(error) => {
                            reasons.insert(
                                "history.available_versions".to_string(),
                                format!(
                                    "rt-host coeff op is unavailable ({}): {}",
                                    HOST_LINK_UNAVAILABLE, error.message
                                ),
                            );
                            Vec::new()
                        }
                    };
                    HistoryLive {
                        source: "rt-host",
                        current_version,
                        current_graph_id,
                        available_versions,
                        reasons,
                    }
                }
                crate::host_link::LinkSnapshot::Unavailable { reason, .. } => {
                    let file = crate::status::read_rt_status(state.rt_status_file(), now);
                    match file {
                        crate::status::RtFile::Fresh { document, .. } => HistoryLive {
                            source: "file",
                            current_version: None,
                            current_graph_id: document
                                .get("graph_id")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            available_versions: Vec::new(),
                            reasons: BTreeMap::from([(
                                "history.available_versions".to_string(),
                                format!(
                                    "rt-host control socket is unavailable ({reason}); the status file reports graph_id only, not plan versions"
                                ),
                            )]),
                        },
                        other => {
                            let file_reason = other
                                .unavailable_reason()
                                .map(str::to_string)
                                .unwrap_or_else(|| "no rt-host status file is available".to_string());
                            HistoryLive {
                                source: "none",
                                current_version: None,
                                current_graph_id: None,
                                available_versions: Vec::new(),
                                reasons: BTreeMap::from([(
                                    "history".to_string(),
                                    format!(
                                        "rt-host control socket is unavailable ({reason}); no fresh status-file fallback: {file_reason}"
                                    ),
                                )]),
                            }
                        }
                    }
                }
                crate::host_link::LinkSnapshot::Process => HistoryLive {
                    source: "none",
                    reasons: BTreeMap::from([(
                        "history".to_string(),
                        "no rt-host control link in this daemon process".to_string(),
                    )]),
                    ..HistoryLive::default()
                },
            }
        }
    }
}

fn versions_from(value: &Value, field: &str, reasons: &mut BTreeMap<String, String>) -> Vec<u64> {
    match value.get(field).and_then(Value::as_array) {
        Some(entries) => entries
            .iter()
            .filter_map(Value::as_u64)
            .collect::<Vec<u64>>(),
        None => {
            reasons.insert(
                format!("history.{field}"),
                "the control plane does not report available plan versions".to_string(),
            );
            Vec::new()
        }
    }
}

/// 合成 /api/plan/history 响应（同步；调用方在 blocking 线程池里）。
fn compose_history(state: &DaemonState, now: SystemTime, limit: usize, offset: usize) -> Value {
    let live = history_live(state, now);
    let mut reasons = live.reasons.clone();
    let commits = state.plans().commits();
    let mut entries: BTreeMap<u64, Value> = BTreeMap::new();
    // 1) daemon 亲自记录过的 commit 回执（最可靠的来源）。
    for record in &commits {
        entries.insert(record.version, commit_entry(record));
    }
    // 2) 控制面/rt-host 上报的可用版本：没有更多元数据的版本如实 null + 原因。
    let current_version = live.current_version;
    for version in &live.available_versions {
        if entries.contains_key(version) {
            continue;
        }
        let graph_id = if Some(*version) == current_version {
            live.current_graph_id.clone()
        } else {
            None
        };
        entries.insert(
            *version,
            json!({
                "version": version,
                "graph_id": graph_id,
                "applied_block": Value::Null,
                "recorded_at": Value::Null,
                "source": live.source,
                "sha256": Value::Null,
                "latency_samples": Value::Null,
            }),
        );
    }
    // 3) 当前生效版本（可能还没被 commit 记录到，例如 daemon 重启前就生效的版本）。
    if let Some(version) = current_version {
        entries.entry(version).or_insert_with(|| {
            json!({
                "version": version,
                "graph_id": live.current_graph_id,
                "applied_block": Value::Null,
                "recorded_at": Value::Null,
                "source": "observed",
                "sha256": Value::Null,
                "latency_samples": Value::Null,
            })
        });
    }
    if current_version.is_none() {
        reasons.insert(
            "history.current".to_string(),
            "the live control plane does not report an applied plan version".to_string(),
        );
    }
    if live.available_versions.is_empty() && current_version.is_some() {
        reasons.insert(
            "history.historic_graph_ids".to_string(),
            "the control plane reports available_versions only when it has a rollback history; historic graph_ids/sha256 are not exposed by the control status".to_string(),
        );
    }
    // 版本降序（最新在前）。
    let mut list: Vec<Value> = entries.into_iter().rev().map(|(_, entry)| entry).collect();
    let total = list.len();
    let start = offset.min(total);
    let end = start.saturating_add(limit).min(total);
    let page = list.drain(start..end).collect::<Vec<Value>>();
    // terms of docs/API.md §11-D：entries 是历史形状（保留不改义），
    // versions 是同一份数据的别名（加法），ok 是成功字段。
    let versions_alias = page.clone();
    json!({
        "ok": true,
        "schema": SCHEMA_HISTORY,
        "source": live.source,
        "current": {
            "version": live.current_version,
            "graph_id": live.current_graph_id,
        },
        "entries": page,
        "versions": versions_alias,
        "pagination": {
            "limit": limit,
            "offset": offset,
            "total": total,
            "has_more": end < total,
        },
        "reasons": reasons_json(&reasons),
    })
}

fn commit_entry(record: &CommitRecord) -> Value {
    let mut entry = record.to_json();
    if let Some(object) = entry.as_object_mut() {
        object.insert("observed".to_string(), Value::Bool(true));
    }
    entry
}

fn reasons_json(reasons: &BTreeMap<String, String>) -> Value {
    let mut object = Map::new();
    for (path, reason) in reasons {
        object.insert(path.clone(), Value::String(reason.clone()));
    }
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn expected_sha_accepts_query_and_header_and_rejects_malformed_values() {
        let mut params = HashMap::new();
        let headers = HeaderMap::new();
        assert!(expected_sha(&params, &headers).is_none());
        params.insert("sha256".to_string(), "AB".repeat(32));
        assert_eq!(
            expected_sha(&params, &headers)
                .expect("present")
                .expect("valid"),
            "AB".repeat(32)
        );
        params.insert("sha256".to_string(), "not-a-sha".to_string());
        assert!(expected_sha(&params, &headers).expect("present").is_err());

        let mut headers = HeaderMap::new();
        headers.insert(
            "x-khaen-plan-sha256",
            HeaderValue::from_static(
                "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
            ),
        );
        assert!(expected_sha(&HashMap::new(), &headers)
            .expect("present")
            .is_ok());
    }
}
