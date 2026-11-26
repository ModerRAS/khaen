//! M4 声道/计划映射后端（T55）：GET /api/channels + PUT /api/channels（两阶段）。
//!
//! GET /api/channels（恒 200，拿不到就 null + reasons）：
//! - output.channels：DAC 输出通道数（优先当前已生效方案的 routing.dac_channels，
//!   否则退回 rt-host 数据面 format.channels）；
//! - output.dac_map：长度 dac_channels 的数组，第 i 项是喂给 DAC i 的 lane（静音 null）；
//! - input.{channels,map,source,sdp_sha256}：AES67 输入参数是 rt-host 进程输入
//!   （KHAEN_AES67_*），控制通道 status 不报，因此 null + reasons；source 取 rt-host 的运行模式
//!   （aes67 / off）。
//! - mapping：[{lane,dac}]，来自当前已生效方案的 routing 段（daemon 在 prepare/upload 时记录，
//!   按 rt-host status 报的 graph_id 关联）；关联不到时 null + reasons，不编映射。
//! - not_wired：能力缺口（channels.hot_reconfigure），前端据此禁用"应用"。
//!
//! PUT /api/channels 两阶段：
//! - stage=prepare：校验 mapping（结构 + 与当前 routing 的边界/重复），返回确定性 token 与
//!   planned diff（added/removed/changed）；
//! - stage=commit：token 校验通过后返回 501 host_link_unsupported + reason —— rt-host 的热重配
//!   （重开 PCM + 块边界换 routing）尚未接通，这里**不假成功**，也不作用于 daemon 进程内引擎。
//!
//! 阻塞边界：rt-host status 往返是阻塞 socket IO，整个合成/校验放进 blocking 线程池。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::SystemTime;

use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Map, Value};

use crate::host_link::LinkSnapshot;
use crate::plan_store::{PlanRecord, RoutingView};
use crate::system::error_code as code;
use crate::DaemonState;

/// /api/channels 响应的 schema。
pub const SCHEMA: &str = "khaen-channels/v1";
/// 改声道映射的 commit 为什么 501（稳定文案；前端展示 reason）。
pub const HOT_RECONFIG_REASON: &str = "changing the output channel mapping requires rt-host hot reconfiguration (reopen the PCM device and swap the plan routing at a block boundary); that path is not wired in this daemon build, so the commit is refused instead of pretending success";
/// AES67 输入参数没有数据源（rt-host 进程输入，控制 status 不报）。
const AES67_INPUT_REASON: &str = "AES67 input parameters (channels/map/sdp) are rt-host process inputs (KHAEN_AES67_*); the rt-host control status does not report them";
/// 一次 prepare 允许的 mapping 条目上限（防御性；DAC 通道数本身远小于它）。
const MAX_MAPPING_ENTRIES: usize = 128;
/// 最大请求体（JSON 控制面）。
const MAX_JSON_BODY: usize = 64 * 1024;

/// 组装 /api/channels 路由（挂到全局 auth 中间件之下）。
pub fn routes() -> Router<Arc<DaemonState>> {
    Router::new().route("/api/channels", get(get_channels).put(put_channels))
}

/// 现取的数据面实况（socket status > 新鲜状态文件 > 不可用；process 目标用进程内控制面）。
#[derive(Debug, Default)]
struct LiveData {
    /// 数据来源："socket" / "file" / "none" / "daemon-process"。
    source: &'static str,
    /// 当前生效方案版本（拿不到为 None）。
    version: Option<u64>,
    /// 当前生效方案 graph_id（拿不到为 None）。
    graph_id: Option<String>,
    /// rt-host 数据面 format.channels（文件/不可用时为 None）。
    format_channels: Option<u16>,
    /// rt-host 运行模式（aes67 / off）。
    mode: Option<String>,
    /// 数据来源级别的不可用原因。
    reasons: BTreeMap<String, String>,
}

/// 取当前数据面实况（同步；调用方在 blocking 线程池里）。
fn live_data(state: &DaemonState, now: SystemTime) -> LiveData {
    match state.host_link().target() {
        crate::host_link::Target::Process => {
            // process 目标：daemon 进程内控制面就是"数据面"（测试/离线演示）。
            let status = state.control().status_json();
            LiveData {
                source: "daemon-process",
                version: status.get("actual_version").and_then(Value::as_u64),
                graph_id: status
                    .get("graph_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                format_channels: status
                    .get("format")
                    .and_then(|format| format.get("channels"))
                    .and_then(Value::as_u64)
                    .and_then(|channels| u16::try_from(channels).ok()),
                mode: status
                    .get("mode")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                reasons: BTreeMap::new(),
            }
        }
        crate::host_link::Target::RtHost => {
            let link = state.host_link().status_snapshot();
            match link {
                LinkSnapshot::Socket { status, .. } => LiveData {
                    source: "socket",
                    version: status.get("actual_version").and_then(Value::as_u64),
                    graph_id: status
                        .get("graph_id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    format_channels: status
                        .get("format")
                        .and_then(|format| format.get("channels"))
                        .and_then(Value::as_u64)
                        .and_then(|channels| u16::try_from(channels).ok()),
                    mode: status
                        .get("mode")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    reasons: BTreeMap::new(),
                },
                LinkSnapshot::Unavailable { reason, .. } => {
                    let file = crate::status::read_rt_status(state.rt_status_file(), now);
                    let mut reasons = BTreeMap::new();
                    match file {
                        crate::status::RtFile::Fresh { document, .. } => LiveData {
                            source: "file",
                            version: None,
                            graph_id: document
                                .get("graph_id")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            format_channels: None,
                            mode: None,
                            reasons,
                        },
                        other => {
                            let file_reason = other
                                .unavailable_reason()
                                .map(str::to_string)
                                .unwrap_or_else(|| {
                                    "no rt-host status file is available".to_string()
                                });
                            reasons.insert(
                                "plan".to_string(),
                                format!("rt-host control socket is unavailable ({reason}); no fresh status-file fallback: {file_reason}"),
                            );
                            LiveData {
                                source: "none",
                                reasons,
                                ..LiveData::default()
                            }
                        }
                    }
                }
                LinkSnapshot::Process => LiveData {
                    source: "none",
                    reasons: BTreeMap::from([(
                        "plan".to_string(),
                        "no rt-host control link in this daemon process".to_string(),
                    )]),
                    ..LiveData::default()
                },
            }
        }
    }
}

/// 当前生效方案的 routing 视图（graph_id 关联到 daemon 记录过的方案）。
fn current_routing(state: &DaemonState, live: &LiveData) -> Option<(PlanRecord, RoutingView)> {
    let graph_id = live.graph_id.as_deref()?;
    let record = state.plans().plan_by_graph_id(graph_id)?;
    let routing = record.routing()?;
    Some((record, routing))
}

/// GET /api/channels。
pub(crate) async fn get_channels(State(state): State<Arc<DaemonState>>) -> Response {
    let worker = Arc::clone(&state);
    let body = match tokio::task::spawn_blocking(move || {
        compose_channels(&worker, SystemTime::now())
    })
    .await
    {
        Ok(body) => body,
        Err(join_error) => json!({
            "schema": SCHEMA,
            "output": { "channels": Value::Null, "dac_map": Value::Null },
            "input": { "channels": Value::Null, "map": Value::Null, "source": Value::Null, "sdp_sha256": Value::Null },
            "mapping": Value::Null,
            "not_wired": [{ "path": "channels", "reason": format!("channel aggregation task failed: {join_error}") }],
            "reasons": { "channels": format!("channel aggregation task failed: {join_error}") },
        }),
    };
    (StatusCode::OK, Json(body)).into_response()
}

/// 合成 GET /api/channels 响应（同步；调用方在 blocking 线程池里）。
fn compose_channels(state: &DaemonState, now: SystemTime) -> Value {
    let live = live_data(state, now);
    let mut reasons = live.reasons.clone();
    let routing = current_routing(state, &live);
    let mut output_channels = routing.as_ref().map(|(_, routing)| routing.dac_channels);
    if output_channels.is_none() {
        output_channels = live.format_channels;
    }
    if output_channels.is_none() {
        reasons.insert(
            "output.channels".to_string(),
            "the rt-host data-plane format does not report channels and no applied-plan routing is known".to_string(),
        );
    }
    let no_routing_reason = match &live.graph_id {
        Some(graph_id) => format!(
            "the applied plan routing section is not known to the daemon (rt-host reports graph_id {graph_id:?}, which matches no plan prepared/uploaded through this daemon; the rt-host control status reports graph_id but not routing)"
        ),
        None => "no applied plan is known: the rt-host control status reports no graph_id and the daemon has no prepared plan".to_string(),
    };
    let mapping = match &routing {
        Some((_, routing)) => routing.mapping_json(),
        None => {
            reasons.insert("mapping".to_string(), no_routing_reason.clone());
            Value::Null
        }
    };
    let dac_map = match &routing {
        Some((_, routing)) => routing.dac_map_json(),
        None => {
            reasons.insert("output.dac_map".to_string(), no_routing_reason.clone());
            Value::Null
        }
    };
    let input_source = live.mode.clone();
    if input_source.is_none() {
        reasons.insert(
            "input.source".to_string(),
            "the rt-host control status does not report the runtime mode".to_string(),
        );
    }
    for path in ["input.channels", "input.map", "input.sdp_sha256"] {
        reasons.insert(path.to_string(), AES67_INPUT_REASON.to_string());
    }
    json!({
        "schema": SCHEMA,
        "output": {
            "channels": output_channels,
            "dac_map": dac_map,
        },
        "input": {
            "channels": Value::Null,
            "map": Value::Null,
            "source": input_source,
            "sdp_sha256": Value::Null,
        },
        "mapping": mapping,
        "plan": {
            "source": live.source,
            "version": live.version,
            "graph_id": live.graph_id,
            "routing_known": routing.is_some(),
        },
        "not_wired": [
            { "path": "channels.hot_reconfigure", "reason": HOT_RECONFIG_REASON },
        ],
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

/// PUT /api/channels 的业务错误。
#[derive(Debug)]
struct ChannelApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    details: Value,
}

impl ChannelApiError {
    fn invalid(message: impl Into<String>, details: Value) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: code::INVALID_REQUEST,
            message: message.into(),
            details,
        }
    }

    fn conflict(error: crate::plan_store::ChannelTokenError) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: error.code(),
            message: format!("cannot commit channel mapping: {}", error.message()),
            details: json!({"reason": error.message()}),
        }
    }

    fn unsupported(message: impl Into<String>, details: Value) -> Self {
        Self {
            status: StatusCode::NOT_IMPLEMENTED,
            code: "host_link_unsupported",
            message: message.into(),
            details,
        }
    }

    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({
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

/// PUT /api/channels：两阶段（stage=prepare|commit，兼容 action 字段）。
pub(crate) async fn put_channels(State(state): State<Arc<DaemonState>>, body: Body) -> Response {
    let bytes = match axum::body::to_bytes(body, MAX_JSON_BODY).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return ChannelApiError::invalid(
                format!("cannot read request body: {error}"),
                json!({"field": "<body>"}),
            )
            .into_response()
        }
    };
    let payload: Value = if bytes.is_empty() {
        Value::Null
    } else {
        match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(error) => {
                return ChannelApiError::invalid(
                    format!("request body must be valid JSON: {error}"),
                    json!({"field": "<body>"}),
                )
                .into_response()
            }
        }
    };
    let Some(object) = payload.as_object() else {
        return ChannelApiError::invalid(
            "request body must be a JSON object",
            json!({"field": "<body>"}),
        )
        .into_response();
    };
    let stage = match object.get("stage").or_else(|| object.get("action")) {
        None => "prepare",
        Some(Value::String(stage)) => stage.as_str(),
        Some(_) => {
            return ChannelApiError::invalid(
                "field stage must be a string (prepare|commit)",
                json!({"field": "stage"}),
            )
            .into_response()
        }
    };
    match stage {
        "prepare" => {
            let object = object.clone();
            let worker = Arc::clone(&state);
            let result = tokio::task::spawn_blocking(move || {
                prepare_channels(&worker, &object, SystemTime::now())
            })
            .await;
            match result {
                Ok(Ok(value)) => (StatusCode::OK, Json(value)).into_response(),
                Ok(Err(error)) => error.into_response(),
                Err(join_error) => ChannelApiError::invalid(
                    format!("channel prepare task failed: {join_error}"),
                    json!({"field": "<task>"}),
                )
                .into_response(),
            }
        }
        "commit" => {
            let object = object.clone();
            let worker = Arc::clone(&state);
            let result =
                tokio::task::spawn_blocking(move || commit_channels_with_state(&worker, &object))
                    .await;
            match result {
                Ok(Ok(value)) => (StatusCode::OK, Json(value)).into_response(),
                Ok(Err(error)) => error.into_response(),
                Err(join_error) => ChannelApiError::invalid(
                    format!("channel commit task failed: {join_error}"),
                    json!({"field": "<task>"}),
                )
                .into_response(),
            }
        }
        other => ChannelApiError::invalid(
            format!("stage must be prepare or commit (got {other:?})"),
            json!({"field": "stage", "allowed": ["prepare", "commit"]}),
        )
        .into_response(),
    }
}

/// 第一阶段：解析/校验 mapping、算 planned diff、存草稿并返回 token。
fn prepare_channels(
    state: &DaemonState,
    object: &Map<String, Value>,
    now: SystemTime,
) -> Result<Value, ChannelApiError> {
    let mapping = parse_mapping(object.get("mapping"))?;
    let live = live_data(state, now);
    let routing = current_routing(state, &live);
    let base = routing.as_ref().map(|(_, routing)| routing.clone());
    // 有基线时校验边界与 lane 是否存在；没有基线时只做结构校验（如实给原因）。
    if let Some(base) = &base {
        let record = routing.as_ref().map(|(record, _)| record);
        let max_lane = record.and_then(|record| record.out_lanes);
        for (lane, dac) in &mapping {
            if *dac >= base.dac_channels {
                return Err(ChannelApiError::invalid(
                    format!(
                        "mapping targets dac {} but the running plan has dac_channels {}",
                        dac, base.dac_channels
                    ),
                    json!({"field": "mapping", "lane": lane, "dac": dac, "dac_channels": base.dac_channels}),
                ));
            }
            if let Some(max_lane) = max_lane {
                if *lane >= max_lane {
                    return Err(ChannelApiError::invalid(
                        format!(
                            "mapping targets lane {} but the running plan has out_lanes {}",
                            lane, max_lane
                        ),
                        json!({"field": "mapping", "lane": lane, "dac": dac, "out_lanes": max_lane}),
                    ));
                }
            }
        }
    }
    let current = base.as_ref().map(RoutingView::mapping_json);
    let diff = planned_diff(base.as_ref().map(|view| view.routes.as_slice()), &mapping);
    let created_at = crate::status::rfc3339_utc(now);
    let pending = state
        .plans()
        .put_pending_channels(mapping.clone(), base.clone(), &created_at);
    let mut reasons = Map::new();
    if base.is_none() {
        reasons.insert(
            "current".to_string(),
            Value::String(
                "current applied-plan routing is unknown, so only structural validation was performed (bounds/duplicate DAC tunnels will be rejected once the running plan is known)"
                    .to_string(),
            ),
        );
    }
    Ok(json!({
        "ok": true,
        "schema": SCHEMA,
        "stage": "prepare",
        "engine_token": pending.token,
        "token": pending.token,
        "mapping": mapping_json(&pending.mapping),
        "current": current,
        "diff": diff,
        "base": base.map(|routing| routing.to_json()),
        "commit_supported": false,
        "applies_to": "rt-host data plane (output channel mapping)",
        "reason": HOT_RECONFIG_REASON,
        "not_wired": [{ "path": "channels.hot_reconfigure", "reason": HOT_RECONFIG_REASON }],
        "reasons": Value::Object(reasons),
    }))
}

/// 第二阶段：token 校验通过后如实 501（rt-host 热重配未接通，不假成功）。
fn commit_channels_with_state(
    state: &DaemonState,
    object: &Map<String, Value>,
) -> Result<Value, ChannelApiError> {
    let token = match object.get("engine_token").or_else(|| object.get("token")) {
        Some(Value::String(token)) if !token.trim().is_empty() => token.trim().to_string(),
        Some(_) => {
            return Err(ChannelApiError::invalid(
                "field engine_token must be a non-empty string",
                json!({"field": "engine_token"}),
            ))
        }
        None => {
            return Err(ChannelApiError::invalid(
                "missing field: engine_token (commit the token returned by stage=prepare)",
                json!({"field": "engine_token"}),
            ))
        }
    };
    let pending = state
        .plans()
        .peek_pending_channels(&token)
        .map_err(ChannelApiError::conflict)?;
    Err(ChannelApiError::unsupported(
        "channel mapping commit is not wired: rt-host hot reconfiguration is unavailable",
        json!({
            "reason": HOT_RECONFIG_REASON,
            "target": crate::system::RESTART_TARGET,
            "engine_token": pending.token,
            "mapping": mapping_json(&pending.mapping),
            "commit_supported": false,
        }),
    ))
}

fn mapping_json(mapping: &[(u16, u16)]) -> Value {
    Value::Array(
        mapping
            .iter()
            .map(|(lane, dac)| json!({"lane": lane, "dac": dac}))
            .collect(),
    )
}

/// 解析并校验 mapping（结构 + 重复；边界校验在拿到当前 routing 后做）。
fn parse_mapping(value: Option<&Value>) -> Result<Vec<(u16, u16)>, ChannelApiError> {
    let Some(Value::Array(entries)) = value else {
        return Err(ChannelApiError::invalid(
            "field mapping must be an array of {lane, dac}",
            json!({"field": "mapping"}),
        ));
    };
    if entries.is_empty() {
        return Err(ChannelApiError::invalid(
            "field mapping must not be empty",
            json!({"field": "mapping"}),
        ));
    }
    if entries.len() > MAX_MAPPING_ENTRIES {
        return Err(ChannelApiError::invalid(
            format!(
                "mapping has too many entries ({} > {MAX_MAPPING_ENTRIES})",
                entries.len()
            ),
            json!({"field": "mapping", "max_entries": MAX_MAPPING_ENTRIES}),
        ));
    }
    let mut lanes: BTreeSet<u16> = BTreeSet::new();
    let mut dacs: BTreeSet<u16> = BTreeSet::new();
    let mut mapping = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let Some(entry) = entry.as_object() else {
            return Err(ChannelApiError::invalid(
                format!("mapping[{index}] must be an object {{lane, dac}}"),
                json!({"field": format!("mapping[{index}]")}),
            ));
        };
        let lane = u16_field(entry, "lane", index)?;
        let dac = u16_field(entry, "dac", index)?;
        if !lanes.insert(lane) {
            return Err(ChannelApiError::invalid(
                format!("mapping lists lane {lane} more than once"),
                json!({"field": "mapping", "lane": lane}),
            ));
        }
        if !dacs.insert(dac) {
            return Err(ChannelApiError::invalid(
                format!("mapping sends two lanes to dac {dac}"),
                json!({"field": "mapping", "dac": dac}),
            ));
        }
        mapping.push((lane, dac));
    }
    Ok(mapping)
}

fn u16_field(
    entry: &Map<String, Value>,
    field: &str,
    index: usize,
) -> Result<u16, ChannelApiError> {
    match entry.get(field).and_then(Value::as_u64) {
        Some(value) => u16::try_from(value).map_err(|_| {
            ChannelApiError::invalid(
                format!("mapping[{index}].{field} does not fit in u16"),
                json!({"field": format!("mapping[{index}].{field}")}),
            )
        }),
        None => Err(ChannelApiError::invalid(
            format!("mapping[{index}].{field} must be a non-negative integer"),
            json!({"field": format!("mapping[{index}].{field}")}),
        )),
    }
}

/// planned diff：added = 新路由里多出来的 (lane,dac)；removed = 旧路由里消失的；
/// changed = 同一 lane 改到别的 dac（同时出现在 added/removed 里，这里单独列出）。
fn planned_diff(current: Option<&[(u16, u16)]>, target: &[(u16, u16)]) -> Value {
    let empty: &[(u16, u16)] = &[];
    let current = current.unwrap_or(empty);
    let current_set: BTreeSet<(u16, u16)> = current.iter().copied().collect();
    let target_set: BTreeSet<(u16, u16)> = target.iter().copied().collect();
    let added: Vec<Value> = target_set
        .difference(&current_set)
        .map(|(lane, dac)| json!({"lane": lane, "dac": dac}))
        .collect();
    let removed: Vec<Value> = current_set
        .difference(&target_set)
        .map(|(lane, dac)| json!({"lane": lane, "dac": dac}))
        .collect();
    let lane_dac: BTreeMap<u16, u16> = current.iter().copied().collect();
    let target_lane_dac: BTreeMap<u16, u16> = target.iter().copied().collect();
    let changed: Vec<Value> = target_lane_dac
        .iter()
        .filter_map(|(lane, dac)| {
            let previous = lane_dac.get(lane)?;
            if previous == dac {
                None
            } else {
                Some(json!({"lane": lane, "from": previous, "to": dac}))
            }
        })
        .collect();
    json!({
        "added": added,
        "removed": removed,
        "changed": changed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(mapping: Value) -> Result<Vec<(u16, u16)>, ChannelApiError> {
        parse_mapping(Some(&mapping))
    }

    #[test]
    fn mapping_validation_rejects_missing_fields_duplicates_and_empty() {
        assert!(request(json!([])).is_err());
        assert!(request(json!([{"lane": 0}])).is_err());
        assert!(request(json!([{"lane": 0, "dac": 0}, {"lane": 0, "dac": 1}])).is_err());
        assert!(request(json!([{"lane": 0, "dac": 0}, {"lane": 1, "dac": 0}])).is_err());
        assert!(request(json!([{"lane": -1, "dac": 0}])).is_err());
        assert!(request(json!([{"lane": 70000, "dac": 0}])).is_err());
        assert_eq!(
            request(json!([{"lane": 0, "dac": 1}, {"lane": 1, "dac": 3}])).expect("valid"),
            vec![(0, 1), (1, 3)]
        );
    }

    #[test]
    fn planned_diff_reports_added_removed_and_changed() {
        let diff = planned_diff(Some(&[(0, 0), (1, 2)]), &[(0, 0), (1, 3), (2, 5)]);
        assert_eq!(
            diff["added"],
            json!([{"lane": 1, "dac": 3}, {"lane": 2, "dac": 5}])
        );
        assert_eq!(diff["removed"], json!([{"lane": 1, "dac": 2}]));
        assert_eq!(diff["changed"], json!([{"lane": 1, "from": 2, "to": 3}]));

        // 没有基线：全部算新增。
        let diff = planned_diff(None, &[(0, 1)]);
        assert_eq!(diff["added"], json!([{"lane": 0, "dac": 1}]));
        assert_eq!(diff["removed"], json!([]));
        assert_eq!(diff["changed"], json!([]));
    }

    #[test]
    fn hot_reconfigure_reason_is_stable_and_mentions_the_boundary() {
        assert!(HOT_RECONFIG_REASON.contains("hot reconfiguration"));
        assert!(HOT_RECONFIG_REASON.contains("refused"));
    }
}
