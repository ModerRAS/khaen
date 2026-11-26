//! /api/system/* handler（T46）：info / logs / diagnostics / password / restart。
//!
//! 纪律（与 /api/status 一致）：
//! - 拿不到的值一律 null + reasons（字段路径 -> 原因），整组缺失时给空组，不编造；
//! - 采集失败（/proc 读不到、systemctl/journalctl 不存在、socket 不可用）只降级，不 500；
//! - 只有明确的请求错误用 400（invalid_request），journalctl 不可用 503 logs_unavailable，
//!   重启未启用 403 restart_disabled，重启失败 503 restart_failed；
//! - 错误体保持 M1 契约：{"error":{"code","message","details"}}。
//!
//! 阻塞边界：/proc 读、systemctl/journalctl spawn、host_link socket 往返都是阻塞操作，
//! 全部放进 tokio blocking 线程池，不占用 axum reactor。

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::SystemTime;

use axum::body::Body;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};

use crate::host_link::LinkSnapshot;
use crate::system::error_code as code;
use crate::system::probe::{DAEMON_UNIT, RT_HOST_UNIT};
use crate::system::{
    log_unit_name, BuildInfo, RestartPolicy, DEFAULT_LOG_LINES, DIAGNOSTICS_SCHEMA, INFO_SCHEMA,
    LOGS_SCHEMA, MAX_LOG_LINES, RESTART_TARGET,
};
use crate::DaemonState;

/// rt-host 控制 socket 只报快照年龄、没有进程启动时刻（与 /api/status 同一口径）。
const SOCKET_SINCE_REASON: &str = "the rt-host control status reports a snapshot age (age_ms) but not a process start time; host_link.since is only derived from the rt-host status-file mtime";
/// process 目标（测试/离线演示）不访问 rt-host：连接状态如实为 false。
const PROCESS_TARGET_REASON: &str = "KHAEN_RT_TARGET=process: control ops act on the daemon process-internal engine (test/offline mode); the rt-host data plane is NOT commanded";
/// 诊断包里每个 unit 的最近日志行数（摘要，不是全量导出）。
const DIAGNOSTIC_LOG_LINES: usize = 50;

fn now_rfc3339(now: SystemTime) -> String {
    crate::status::rfc3339_utc(now)
}

/// M1 错误体：{"error":{"code","message","details"}}。
fn error_response(
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
    error_response(
        StatusCode::BAD_REQUEST,
        code::INVALID_REQUEST,
        message,
        details,
    )
}

// ---------------------------------------------------------------------------
// GET /api/system/info
// ---------------------------------------------------------------------------

/// GET /api/system/info：系统信息页唯一数据源（T47 契约对齐）。
pub(crate) async fn info(State(state): State<Arc<DaemonState>>) -> Response {
    let worker = Arc::clone(&state);
    let body =
        match tokio::task::spawn_blocking(move || compose_info(&worker, SystemTime::now())).await {
            Ok(body) => body,
            // 采集任务 panic 也不能变成 500：系统信息契约是 200 + null + 原因。
            Err(join_error) => fallback_info_body(&state, &join_error.to_string()),
        };
    (StatusCode::OK, Json(body)).into_response()
}

/// 构造 /api/system/info 的完整响应（同步；调用方放进 blocking 线程池）。
pub fn compose_info(state: &DaemonState, now: SystemTime) -> Value {
    let build: &BuildInfo = state.build_info();
    let probe = state.system_probe();
    let mut reasons: BTreeMap<String, String> = build.reasons.clone();

    let hostname = string_or_reason(probe.hostname(), "hostname", &mut reasons);
    let kernel = string_or_reason(probe.kernel(), "kernel", &mut reasons);
    let os = string_or_reason(probe.os(), "os", &mut reasons);

    let devices = match probe.devices() {
        Ok(devices) => {
            let mut values = Vec::with_capacity(devices.len());
            for (index, device) in devices.iter().enumerate() {
                let prefix = format!("devices[{index}]");
                let card_id = option_string_or_reason(
                    device.card_id.clone(),
                    format!("{prefix}.card_id"),
                    &mut reasons,
                );
                let driver = option_string_or_reason(
                    device.driver.clone(),
                    format!("{prefix}.driver"),
                    &mut reasons,
                );
                let model = option_string_or_reason(
                    device.model.clone(),
                    format!("{prefix}.model"),
                    &mut reasons,
                );
                let channels = match device.channels {
                    Some(channels) => json!(channels),
                    None => {
                        insert_device_reason(&mut reasons, &prefix, "channels", &device.reasons);
                        Value::Null
                    }
                };
                let rates = match &device.rates {
                    Some(rates) => json!(rates),
                    None => {
                        insert_device_reason(&mut reasons, &prefix, "rates", &device.reasons);
                        Value::Null
                    }
                };
                values.push(json!({
                    "card_id": card_id,
                    "driver": driver,
                    "model": model,
                    "channels": channels,
                    "rates": rates,
                }));
            }
            Value::Array(values)
        }
        Err(reason) => {
            reasons.insert("devices".to_string(), reason);
            Value::Array(Vec::new())
        }
    };

    let daemon_service =
        compose_service(probe.service(DAEMON_UNIT), "services.daemon", &mut reasons);
    let rt_host_service = compose_service(
        probe.service(RT_HOST_UNIT),
        "services.rt_host",
        &mut reasons,
    );

    let link = state.host_link().status_snapshot();
    let (host_link, host_link_reasons) = compose_host_link(state, now, &link);
    reasons.extend(host_link_reasons);

    let policy: RestartPolicy = state.restart_policy().clone();
    let last_restarted_at = state
        .last_restarted_at()
        .map(Value::String)
        .unwrap_or(Value::Null);
    if !policy.allowed() {
        if let Some(reason) = policy.reason() {
            reasons.insert("restart".to_string(), reason.to_string());
        }
    }
    let restart = json!({
        "allowed": policy.allowed(),
        "reason": policy.reason(),
        "target": RESTART_TARGET,
        "last_restarted_at": last_restarted_at,
    });

    let option_field = |value: &Option<String>| match value {
        Some(value) => Value::String(value.clone()),
        None => Value::Null,
    };

    json!({
        "ok": true,
        "schema": INFO_SCHEMA,
        // 合同 §3 的 {version, host}：分别取 healthz 同一版本口径与 hostname（加法别名，不改旧字段）。
        "version": build.healthz_version(),
        "host": hostname,
        "crate_version": build.crate_version,
        "release_version": option_field(&build.release_version),
        "commit": option_field(&build.commit),
        "build_profile": build.build_profile,
        "rustc": option_field(&build.rustc),
        "target": build.target,
        "bind": state.bind().to_string(),
        "state_dir": state.state_dir().display().to_string(),
        "rt_status_file": state.rt_status_file().display().to_string(),
        "uptime_s": state.uptime_s(),
        "hostname": hostname,
        "kernel": kernel,
        "os": os,
        "services": {
            "daemon": daemon_service,
            "rt_host": rt_host_service,
        },
        "devices": devices,
        "host_link": host_link,
        "restart": restart,
        "reasons": reasons_json(&reasons),
    })
}

fn reasons_json(reasons: &BTreeMap<String, String>) -> Value {
    let mut object = Map::new();
    for (path, reason) in reasons {
        object.insert(path.clone(), Value::String(reason.clone()));
    }
    Value::Object(object)
}

fn string_or_reason(
    result: Result<String, String>,
    path: &str,
    reasons: &mut BTreeMap<String, String>,
) -> Value {
    match result {
        Ok(value) => Value::String(value),
        Err(reason) => {
            reasons.insert(path.to_string(), reason);
            Value::Null
        }
    }
}

fn option_string_or_reason(
    value: Option<String>,
    path: String,
    reasons: &mut BTreeMap<String, String>,
) -> Value {
    match value {
        Some(value) => Value::String(value),
        None => {
            reasons.insert(path, "value is not reported by the collector".to_string());
            Value::Null
        }
    }
}

fn insert_device_reason(
    reasons: &mut BTreeMap<String, String>,
    prefix: &str,
    field: &str,
    device_reasons: &[(String, String)],
) {
    let reason = device_reasons
        .iter()
        .find(|(name, _)| name == field)
        .map(|(_, reason)| reason.clone())
        .unwrap_or_else(|| "value is not reported by the collector".to_string());
    reasons.insert(format!("{prefix}.{field}"), reason);
}

/// 服务状态 -> 响应对象（active/enabled/pid + 单个 summary reason），逐字段缺失原因进 reasons。
fn compose_service(
    snapshot: crate::system::ServiceSnapshot,
    prefix: &str,
    reasons: &mut BTreeMap<String, String>,
) -> Value {
    let fields = [
        ("active", snapshot.active.map(Value::Bool)),
        ("enabled", snapshot.enabled.map(Value::Bool)),
        ("pid", snapshot.pid.map(|pid| json!(pid))),
    ];
    for (field, value) in &fields {
        if value.is_none() {
            let reason = snapshot
                .reason(field)
                .map(ToString::to_string)
                .unwrap_or_else(|| "value is not reported by systemctl".to_string());
            reasons.insert(format!("{prefix}.{field}"), reason);
        }
    }
    json!({
        "active": fields[0].1.clone(),
        "enabled": fields[1].1.clone(),
        "pid": fields[2].1.clone(),
        "reason": snapshot.summary_reason(),
    })
}

/// host_link 实况（socket > 新鲜状态文件 > 不可用），与 /api/status 同口径。
fn compose_host_link(
    state: &DaemonState,
    now: SystemTime,
    link: &LinkSnapshot,
) -> (Value, BTreeMap<String, String>) {
    let mut reasons = BTreeMap::new();
    let socket_mode = link.is_socket();
    let file = if socket_mode {
        None
    } else {
        Some(crate::status::read_rt_status(state.rt_status_file(), now))
    };
    let file_fresh = matches!(file, Some(crate::status::RtFile::Fresh { .. }));
    let connected = socket_mode || file_fresh;
    let socket = link
        .socket()
        .map(|path| Value::String(path.display().to_string()))
        .unwrap_or(Value::Null);
    if !socket_mode {
        match link {
            LinkSnapshot::Unavailable { reason, .. } => {
                reasons.insert(
                    "host_link.socket".to_string(),
                    format!("rt-host control socket is unavailable ({reason})"),
                );
            }
            _ => {
                reasons.insert(
                    "host_link.socket".to_string(),
                    PROCESS_TARGET_REASON.to_string(),
                );
            }
        }
    }
    let (since, age) = if socket_mode {
        reasons.insert(
            "host_link.since".to_string(),
            SOCKET_SINCE_REASON.to_string(),
        );
        let age = match link {
            LinkSnapshot::Socket { age_ms, .. } => *age_ms,
            _ => None,
        };
        if age.is_none() {
            reasons.insert(
                "host_link.status_age_ms".to_string(),
                "the rt-host control status does not report age_ms yet: the block loop has not published its first runtime snapshot".to_string(),
            );
        }
        (Value::Null, age)
    } else {
        let file_reason = file
            .as_ref()
            .and_then(crate::status::RtFile::unavailable_reason)
            .map(ToString::to_string);
        let since = match file.as_ref().and_then(crate::status::RtFile::mtime) {
            Some(mtime) => Value::String(crate::status::rfc3339_utc(mtime)),
            None => {
                reasons.insert(
                    "host_link.since".to_string(),
                    file_reason
                        .clone()
                        .unwrap_or_else(|| "rt-host status file is not readable".to_string()),
                );
                Value::Null
            }
        };
        let age = file.as_ref().and_then(crate::status::RtFile::age_ms);
        if age.is_none() {
            reasons.insert(
                "host_link.status_age_ms".to_string(),
                file_reason
                    .clone()
                    .unwrap_or_else(|| "rt-host status file is not readable".to_string()),
            );
        }
        (since, age)
    };
    if !connected {
        let socket_part = match link {
            LinkSnapshot::Unavailable { reason, .. } => {
                Some(format!("rt-host control socket is unavailable ({reason})"))
            }
            LinkSnapshot::Process => Some(PROCESS_TARGET_REASON.to_string()),
            LinkSnapshot::Socket { .. } => None,
        };
        let file_part = file
            .as_ref()
            .and_then(crate::status::RtFile::unavailable_reason)
            .map(ToString::to_string)
            .unwrap_or_else(|| "no rt-host status file is available".to_string());
        let text = match socket_part {
            Some(socket_part) => {
                format!("{socket_part}; no usable rt-host status file fallback: {file_part}")
            }
            None => file_part,
        };
        reasons.insert("host_link.connected".to_string(), text);
    }
    (
        json!({
            "connected": connected,
            "since": since,
            "socket": socket,
            "status_age_ms": age.map_or(Value::Null, |age| json!(age)),
        }),
        reasons,
    )
}

/// 采集任务异常时的兜底 body：schema 完整、daemon 自身字段真实、其余 null + 原因。
fn fallback_info_body(state: &DaemonState, message: &str) -> Value {
    let build = state.build_info();
    json!({
        "ok": true,
        "schema": INFO_SCHEMA,
        "crate_version": build.crate_version,
        "release_version": build.release_version,
        "commit": build.commit,
        "build_profile": build.build_profile,
        "rustc": build.rustc,
        "target": build.target,
        "bind": state.bind().to_string(),
        "state_dir": state.state_dir().display().to_string(),
        "rt_status_file": state.rt_status_file().display().to_string(),
        "uptime_s": state.uptime_s(),
        "hostname": Value::Null,
        "kernel": Value::Null,
        "os": Value::Null,
        "services": {
            "daemon": { "active": Value::Null, "enabled": Value::Null, "pid": Value::Null, "reason": message },
            "rt_host": { "active": Value::Null, "enabled": Value::Null, "pid": Value::Null, "reason": message },
        },
        "devices": [],
        "host_link": { "connected": false, "since": Value::Null, "socket": Value::Null, "status_age_ms": Value::Null },
        "restart": { "allowed": state.restart_policy().allowed(), "reason": state.restart_policy().reason(), "target": RESTART_TARGET, "last_restarted_at": Value::Null },
        "reasons": { "info": format!("system info collection failed: {message}") },
    })
}

// ---------------------------------------------------------------------------
// GET /api/system/logs
// ---------------------------------------------------------------------------

/// GET /api/system/logs?unit=daemon|rt-host&lines=N
pub(crate) async fn logs(
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
    let requested_unit = params.get("unit").map(String::as_str).unwrap_or("daemon");
    let Some(unit_name) = log_unit_name(requested_unit) else {
        return invalid_request(
            format!("unit must be one of daemon/rt-host (got {requested_unit:?})"),
            json!({"field": "unit", "allowed": ["daemon", "rt-host"]}),
        );
    };
    let unit = if requested_unit == "rt_host" {
        "rt-host"
    } else {
        requested_unit
    };
    let requested_lines = match params.get("lines") {
        None => DEFAULT_LOG_LINES,
        Some(raw) => match raw.trim().parse::<usize>() {
            Ok(lines) if (1..=MAX_LOG_LINES).contains(&lines) => lines,
            _ => {
                return invalid_request(
                    format!("lines must be an integer in 1..={MAX_LOG_LINES} (got {raw:?})"),
                    json!({"field": "lines", "min": 1, "max": MAX_LOG_LINES}),
                )
            }
        },
    };
    let probe = Arc::clone(state.system_probe());
    match tokio::task::spawn_blocking(move || probe.logs(unit_name, requested_lines)).await {
        Ok(Ok(lines)) => (
            StatusCode::OK,
            Json(json!({
                "schema": LOGS_SCHEMA,
                "unit": unit,
                "unit_name": unit_name,
                "requested_lines": requested_lines,
                "lines": lines,
                "truncated": false,
                "source": "journalctl",
                "reason": Value::Null,
            })),
        )
            .into_response(),
        Ok(Err(reason)) => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            code::LOGS_UNAVAILABLE,
            format!("journal logs are unavailable for unit {unit_name}"),
            json!({"reason": reason, "unit": unit}),
        ),
        Err(join_error) => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            code::LOGS_UNAVAILABLE,
            format!("journal log collection task failed for unit {unit_name}"),
            json!({"reason": format!("log collection task failed: {join_error}"), "unit": unit}),
        ),
    }
}

// ---------------------------------------------------------------------------
// GET /api/system/diagnostics
// ---------------------------------------------------------------------------

/// GET /api/system/diagnostics：JSON 附件下载（合并 status + info + 设备 + 最近日志摘要）。
pub(crate) async fn diagnostics(State(state): State<Arc<DaemonState>>) -> Response {
    let worker = Arc::clone(&state);
    let now = SystemTime::now();
    let body = match tokio::task::spawn_blocking(move || compose_diagnostics(&worker, now)).await {
        Ok(body) => body,
        Err(join_error) => json!({
            "schema": DIAGNOSTICS_SCHEMA,
            "generated_at": now_rfc3339(now),
            "error": format!("diagnostics collection failed: {join_error}"),
        }),
    };
    let filename = diagnostics_filename(state.build_info(), now);
    let text = serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".to_string());
    let mut response = Response::new(Body::from(text));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, value);
    }
    response
}

/// 诊断包内容（同步；调用方放进 blocking 线程池）。只包含系统/状态/设备/日志摘要，
/// 不包含 auth.json、密码、会话 token、CSRF 或 Cookie。
fn compose_diagnostics(state: &DaemonState, now: SystemTime) -> Value {
    let system = compose_info(state, now);
    let status = crate::status::aggregate(state, now);
    let device = state.device_json();
    let probe = state.system_probe();
    let daemon_logs = log_summary(probe.logs(DAEMON_UNIT, DIAGNOSTIC_LOG_LINES));
    let rt_host_logs = log_summary(probe.logs(RT_HOST_UNIT, DIAGNOSTIC_LOG_LINES));
    json!({
        "schema": DIAGNOSTICS_SCHEMA,
        "generated_at": now_rfc3339(now),
        "build": {
            "crate_version": system["crate_version"],
            "release_version": system["release_version"],
            "commit": system["commit"],
            "build_profile": system["build_profile"],
            "rustc": system["rustc"],
            "target": system["target"],
        },
        "system": system,
        "status": status,
        "device": device,
        "logs": {
            "daemon": daemon_logs,
            "rt_host": rt_host_logs,
        },
        "notes": [
            "diagnostics bundle: system info + aggregated status + device snapshot + recent journal lines",
            "auth password hashes, session tokens and CSRF secrets are never included",
        ],
    })
}

fn log_summary(result: Result<Vec<String>, String>) -> Value {
    match result {
        Ok(lines) => json!({
            "available": true,
            "source": "journalctl",
            "lines": lines,
            "reason": Value::Null,
        }),
        Err(reason) => json!({
            "available": false,
            "source": "journalctl",
            "lines": [],
            "reason": reason,
        }),
    }
}

/// 诊断包文件名：khaen-diagnostics-v<version>-<UTC 紧凑时间>.json（版本去掉前导 v）。
pub(crate) fn diagnostics_filename(build: &BuildInfo, now: SystemTime) -> String {
    let version = build
        .release_version
        .as_deref()
        .unwrap_or(&build.crate_version)
        .trim()
        .trim_start_matches(['v', 'V']);
    let version = if version.is_empty() {
        "unknown"
    } else {
        version
    };
    let stamp = now_rfc3339(now)
        .replace(['-', ':'], "")
        .replace(".000Z", "Z");
    format!("khaen-diagnostics-v{version}-{stamp}.json")
}

// ---------------------------------------------------------------------------
// POST /api/system/password
// ---------------------------------------------------------------------------

/// POST /api/system/password：{old,new}；复用 M1 认证语义（中间件已保证登录 + CSRF）。
pub(crate) async fn password(
    State(state): State<Arc<DaemonState>>,
    body: axum::body::Bytes,
) -> Response {
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => {
            return invalid_request(
                format!("request body must be valid JSON: {error}"),
                json!({"field": "<body>"}),
            )
        }
    };
    let Some(object) = payload.as_object() else {
        return invalid_request(
            "request body must be a JSON object",
            json!({"field": "<body>"}),
        );
    };
    // docs/API.md §8 字段名 current；旧客户端用 old，作为别名保留（两者都给且不同 → 400）。
    let old_password = match (object.get("current"), object.get("old")) {
        (Some(Value::String(current)), None) | (None, Some(Value::String(current))) => {
            current.clone()
        }
        (Some(Value::String(current)), Some(Value::String(old))) if current == old => {
            current.clone()
        }
        (Some(_), _) | (_, Some(_)) => {
            return invalid_request(
                "fields current/old must be strings and must not disagree",
                json!({"field": "current"}),
            )
        }
        (None, None) => {
            return invalid_request("missing field: current", json!({"field": "current"}))
        }
    };
    let new_password = match object.get("new") {
        Some(Value::String(value)) => value.clone(),
        Some(_) => return invalid_request("field new must be a string", json!({"field": "new"})),
        None => return invalid_request("missing field: new", json!({"field": "new"})),
    };
    let length = new_password.chars().count();
    if length < crate::auth::MIN_PASSWORD_LEN {
        return error_response(
            StatusCode::BAD_REQUEST,
            code::INVALID_PASSWORD,
            format!(
                "new password must be at least {} characters (got {length})",
                crate::auth::MIN_PASSWORD_LEN
            ),
            json!({"field": "new", "min_length": crate::auth::MIN_PASSWORD_LEN}),
        );
    }
    let result = state.auth().change_password(&old_password, &new_password);
    match result {
        Ok(issued) => crate::auth::session_response(StatusCode::OK, &issued),
        Err(error) => {
            // 契约稳定码：新密码过短一律 invalid_password（/api/auth/password 仍是 weak_password）。
            if error.code() == crate::auth::code::WEAK_PASSWORD {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    code::INVALID_PASSWORD,
                    error.message().to_string(),
                    json!({"field": "new"}),
                );
            }
            error.into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// POST /api/system/restart
// ---------------------------------------------------------------------------

/// POST /api/system/restart：{target:"rt-host"}；只有 KHAEN_ALLOW_RESTART=1 才启用。
pub(crate) async fn restart(
    State(state): State<Arc<DaemonState>>,
    body: axum::body::Bytes,
) -> Response {
    let payload: Value = match serde_json::from_slice::<Value>(&body) {
        Ok(value) if value.is_null() => Value::Object(Map::new()),
        Ok(value) => value,
        Err(error) => {
            return invalid_request(
                format!("request body must be valid JSON: {error}"),
                json!({"field": "<body>"}),
            )
        }
    };
    let Some(object) = payload.as_object() else {
        return invalid_request(
            "request body must be a JSON object",
            json!({"field": "<body>"}),
        );
    };
    // docs/API.md §8：请求可以是 {confirm} 或 {target}；缺省 target 视为 rt-host
    //（前端二次确认只需要 confirm，不必重复写 target）。
    let target = match object.get("target") {
        Some(Value::String(target)) => target.clone(),
        Some(_) => {
            return invalid_request("field target must be a string", json!({"field": "target"}))
        }
        None => RESTART_TARGET.to_string(),
    };
    if target == "daemon" || target == "self" || target == "daemon-self" {
        return error_response(
            StatusCode::NOT_IMPLEMENTED,
            code::UNSUPPORTED,
            "daemon self-restart is not implemented",
            json!({
                "reason": "restarting the daemon would terminate this API server; use systemctl (or the deploy pipeline) from a maintenance shell, then reconnect",
                "target": target,
            }),
        );
    }
    if target != RESTART_TARGET {
        // 合同 §8：只实现了 rt-host；其它 target 如实 501 unsupported（不假称可重启）。
        return error_response(
            StatusCode::NOT_IMPLEMENTED,
            code::UNSUPPORTED,
            format!("restart target {target:?} is not implemented"),
            json!({"field": "target", "supported": [RESTART_TARGET], "target": target}),
        );
    }
    let policy = state.restart_policy().clone();
    if !policy.allowed() {
        return error_response(
            StatusCode::FORBIDDEN,
            code::RESTART_DISABLED,
            "restarting the rt-host data plane is disabled",
            json!({
                "reason": policy.reason().unwrap_or("restart is not enabled"),
                "target": RESTART_TARGET,
            }),
        );
    }
    let probe = Arc::clone(state.system_probe());
    let unit = RT_HOST_UNIT;
    match tokio::task::spawn_blocking(move || probe.restart_service(unit)).await {
        Ok(Ok(())) => {
            let restarted_at = now_rfc3339(SystemTime::now());
            state.record_restart(restarted_at.clone());
            (
                StatusCode::OK,
                Json(json!({
                    "ok": true,
                    "target": RESTART_TARGET,
                    "restarted": true,
                    "restarted_at": restarted_at,
                    "reason": Value::Null,
                })),
            )
                .into_response()
        }
        Ok(Err(reason)) => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            code::RESTART_FAILED,
            "systemctl restart khaen-rt-host failed",
            json!({"reason": reason, "target": RESTART_TARGET}),
        ),
        Err(join_error) => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            code::RESTART_FAILED,
            "restart task failed",
            json!({"reason": format!("restart task failed: {join_error}"), "target": RESTART_TARGET}),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::BuildInfo;
    use std::time::{Duration, UNIX_EPOCH};

    fn build_info(release: Option<&str>) -> BuildInfo {
        BuildInfo {
            crate_version: "0.1.0".to_string(),
            release_version: release.map(str::to_string),
            commit: Some("855f4c4".to_string()),
            commit_source: Some("git".to_string()),
            rustc: Some("rustc 1.89.0".to_string()),
            build_profile: "release".to_string(),
            target: "x86_64-unknown-linux-gnu".to_string(),
            reasons: BTreeMap::new(),
        }
    }

    #[test]
    fn diagnostics_filename_carries_release_version_and_compact_utc() {
        // 2026-11-26T07:35:23Z（与 T47 文档里的时间戳同形）。
        let now = UNIX_EPOCH + Duration::from_secs(1_795_678_523);
        let name = diagnostics_filename(&build_info(Some("v0.1.2")), now);
        assert_eq!(name, "khaen-diagnostics-v0.1.2-20261126T073523Z.json");
        let fallback = diagnostics_filename(&build_info(None), now);
        assert_eq!(fallback, "khaen-diagnostics-v0.1.0-20261126T073523Z.json");
    }

    #[test]
    fn restart_policy_is_fail_closed() {
        assert!(!RestartPolicy::from_value(None).allowed());
        assert!(RestartPolicy::from_value(Some("0")).reason().is_some());
        assert!(RestartPolicy::from_value(Some("yes")).reason().is_some());
        assert!(RestartPolicy::from_value(Some(" 1 ")).allowed());
        assert!(RestartPolicy::enabled().reason().is_none());
    }

    #[test]
    fn log_unit_name_accepts_contract_spellings() {
        assert_eq!(log_unit_name("daemon"), Some(DAEMON_UNIT));
        assert_eq!(log_unit_name("rt-host"), Some(RT_HOST_UNIT));
        assert_eq!(log_unit_name("rt_host"), Some(RT_HOST_UNIT));
        assert_eq!(log_unit_name("khaen-daemon"), None);
    }
}
