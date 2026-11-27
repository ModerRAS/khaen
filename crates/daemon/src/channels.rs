//! M4 声道/计划映射后端（T55）+ rt-host 热重配接线（T61）。
//!
//! # GET /api/channels（恒 200，拿不到就 null + reasons）
//!
//! target=rt-host 时优先级：
//! 1. rt-host channels_status（M3b 热重配的现取状态）：current（channels/sample_rate/
//!    aes67_*/jitter/device）、device.probe、generation、busy、pending、last_gap_ms、
//!    last_result。这是数据面唯一权威来源，拿不到时逐字段 null + reasons；
//! 2. 当前已生效方案的 routing 作为 mapping / output.dac_map 的来源：优先用 rt-host 控制
//!    status 的 `readback.routing`（真实已加载的 base plan，即使方案不是本 daemon 准备的），
//!    拿不到时才退回 daemon 在 prepare/upload 时按 graph_id 记录的方案 routing 段；
//! 3. 契约 B status（op status）的 data-plane format 作为 output.channels / format 的降级来源。
//!
//! - output.channels：channels_status.current.channels > 方案 routing.dac_channels > status.format.channels；
//! - output.dac_map / mapping：当前已生效方案的 routing（长度 dac_channels，静音 null）；
//!   运行图被上一次 channels_commit 丢弃（graph_dropped）或 routing 与数据面通道数不一致时
//!   如实 null + 原因（不拿旧 routing 冒充当前）；
//! - input.{channels,map}：channels_status.current.{aes67_channels,aes67_map}（AES67 输入的真实配置）；
//!   input.source（sdp/manual）与 input.sdp_sha256 控制通道不报 -> null + reasons；
//! - applied：daemon 本地镜像（最近一次经本 daemon 成功的 channels_commit；快照 + 记录时刻）；
//! - hot_reconfigure：能力上报（true / false + reason / null = 无法核实）；not_wired 只列
//!   确认的能力缺口（rt-host 报 unsupported/unknown_op、process 目标）。
//!
//! # PUT /api/channels（两阶段，stage=prepare|commit|abort，兼容 action 字段）
//!
//! - prepare：解析补丁（mapping/dac_map/channels/sample_rate/aes67_*/jitter_frames，含
//!   output/input 嵌套别名），把 rt-host 协议字段原样转发给 channels_prepare，返回
//!   rt-host 的 engine_token/diff/current/target/graph 与 commit_supported: true；
//!   mapping/dac_map 是方案 routing（lane -> DAC），不是 rt-host 通道字段：
//!   prepare 会校验并算 planned diff，但明确上报"由方案 commit 生效"，
//!   只有 mapping 改动、没有 rt-host 可应用字段时返回 commit_supported:false + blockers
//!   （不签发假 token、不假成功）；
//! - commit：把 token 原样交给 channels_commit，成功回执原样透传
//!   （applied/changed/generation/gap_ms/format/graph_dropped/... 一个不改），并更新本地镜像；
//!   失败回执（ok=false，例如 ERR_RECONFIGURE_DEVICE + rolled_back）按错误码映射 HTTP，
//!   回滚语义（rolled_back/device_started/receiver_running/applied/generation）放进 details 原样带出；
//! - abort：channels_abort 丢弃草稿，成功回执透传。
//!
//! 错误码/HTTP 映射沿用 host_link 既有规则（见 host_link_status）：
//! rt-host 业务错误码透传（不重写文案），断链 503 host_link_unavailable，
//! busy/草稿/token/admission 类 409，设备不可用 503，重配失败（ERR_RECONFIGURE_*）502，
//! 未实现 501，参数 400。
//!
//! # process 目标（KHAEN_RT_TARGET=process）
//!
//! rt-host 热重配在 daemon 进程内引擎上没有对应实现：PUT 的任何阶段都如实 501
//! host_link_unsupported + reason，不签发 token、不假成功。GET 仍返回 daemon 进程内
//! 控制面的真实快照（并显式标 target=daemon-process）。
//!
//! # 阻塞边界
//!
//! 控制通道往返是阻塞 socket IO：GET 的聚合与 PUT 的三阶段都放进 blocking 线程池。

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

use crate::audio_profile::model::{AudioProfile, SpeakerRole};
use crate::audio_profile::store::CurrentProfile;
use crate::channels_mirror::PendingRouting;
use crate::host_link::{LinkError, LinkSnapshot, Target, WireReply, HOST_LINK_UNAVAILABLE};
use crate::plan_store::{PlanRecord, RoutingView};
use crate::system::error_code as code;
use crate::DaemonState;

/// /api/channels 响应的 schema。
pub const SCHEMA: &str = "khaen-channels/v1";
/// process 目标下热重配不可用的原因（稳定文案）。
pub const HOT_RECONFIG_PROCESS_REASON: &str = "KHAEN_RT_TARGET=process: control ops act on the daemon process-internal engine, which has no data-plane hot reconfiguration (no ALSA device / AES67 receiver); no channels draft is created and no token is issued, so a commit cannot pretend to succeed";
/// rt-host 进程没有接通 channels 上下文（KHAEN_CAL/运行时未装配）时 commit 不可用的原因。
pub const HOT_RECONFIG_UNSUPPORTED_REASON: &str = "the connected rt-host has no data-plane channel context (channels ops are not wired in that process): no hot reconfiguration can be applied, so the commit is refused instead of pretending success";
/// lane -> DAC 的 mapping 为什么不能由 channels_commit 生效（稳定文案；prepare/commit 回执共用）。
pub const MAPPING_PLAN_OWNED_REASON: &str = "the lane -> DAC mapping is the plan routing section (ABI v3 routing), not a field of the rt-host channels op: channels_commit applies the output channel count / sample rate / AES67 input configuration at a block boundary and cannot rewrite the running plan graph; when the engine format changes rt-host drops the graph (graph_dropped=true), so the mapping takes effect again only after a plan whose routing section matches the new format is prepared and committed";
/// rt-host 控制 status 不报 AES67 输入参数时（channels_status 也不可用）的原因。
const AES67_INPUT_REASON: &str = "the rt-host channels status is unavailable: AES67 input parameters (KHAEN_AES67_*) are rt-host process inputs and the contract-B status op does not report them";
/// 输入来源（sdp/manual）没有数据源。
const AES67_SOURCE_REASON: &str = "the rt-host control channel reports the AES67 receive parameters but not whether they came from an SDP descriptor or a manual configuration; source is not fabricated";
/// 输入 SDP sha256 没有数据源。
const AES67_SDP_REASON: &str = "the rt-host channels status reports the AES67 input configuration but not its SDP descriptor; sdp_sha256 is not fabricated";
/// 一次 prepare 允许的 mapping 条目上限（防御性；DAC 通道数本身远小于它）。
const MAX_MAPPING_ENTRIES: usize = 128;
/// 最大请求体（JSON 控制面）。
const MAX_JSON_BODY: usize = 64 * 1024;

/// 组装 /api/channels 路由（挂到全局 auth 中间件之下）。
pub fn routes() -> Router<Arc<DaemonState>> {
    Router::new().route("/api/channels", get(get_channels).put(put_channels))
}

// ===========================================================================
// 数据源探测（status op + channels_status op）
// ===========================================================================

/// channels_status 的探测结果。
#[derive(Debug)]
enum ChannelsReply {
    /// rt-host 应答 ok=true（M3b 已接通）。
    Ok(Value),
    /// rt-host 应答 ok=false（例如旧版 rt-host 的 unsupported/unknown_op）。
    Failure {
        /// 稳定错误码（rt-host 原样）。
        code: String,
        /// 人读说明（rt-host 原样）。
        message: String,
    },
    /// 控制通道不可用（连接/超时/协议错误）。
    Unavailable {
        /// 人读原因。
        reason: String,
    },
    /// 没去探测（status op 已失败 / process 目标），带原因。
    Skipped {
        /// 人读原因。
        reason: String,
    },
}

impl ChannelsReply {
    fn reason(&self) -> Option<&str> {
        match self {
            Self::Ok(_) => None,
            Self::Failure { message, .. } => Some(message),
            Self::Unavailable { reason } | Self::Skipped { reason } => Some(reason),
        }
    }

    /// rt-host 明确回答"这个 op 没接通"（不是链路问题）。
    fn is_unsupported(&self) -> bool {
        matches!(
            self,
            Self::Failure { code, .. }
                if code == "unsupported" || code == "unknown_op" || code == "not_wired"
        )
    }
}

/// 探一次 rt-host：先 status（routing 关联用），socket 通了再 channels_status。
fn probe_channels(state: &DaemonState) -> (LinkSnapshot, ChannelsReply) {
    if state.host_link().target() == Target::Process {
        return (
            LinkSnapshot::Process,
            ChannelsReply::Skipped {
                reason: HOT_RECONFIG_PROCESS_REASON.to_string(),
            },
        );
    }
    let snapshot = state.host_link().status_snapshot();
    let channels = match &snapshot {
        LinkSnapshot::Socket { .. } => {
            match state
                .host_link()
                .call(json!({ "op": "channels_status" }))
            {
                Ok(WireReply::Success(body)) => ChannelsReply::Ok(body),
                Ok(WireReply::Failure { code, message, .. }) => {
                    ChannelsReply::Failure { code, message }
                }
                Err(error) => ChannelsReply::Unavailable {
                    reason: error.message,
                },
            }
        }
        LinkSnapshot::Unavailable { reason, .. } => ChannelsReply::Skipped {
            reason: format!(
                "rt-host control socket is unavailable ({reason}); channels_status was not attempted"
            ),
        },
        LinkSnapshot::Process => ChannelsReply::Skipped {
            reason: HOT_RECONFIG_PROCESS_REASON.to_string(),
        },
    };
    (snapshot, channels)
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
    /// 控制 status 的 `readback`（rt-host 真实已加载 base plan 的只读回读；拿不到为 None）。
    readback: Option<Value>,
    /// rt-host 数据面 format.channels（文件/不可用时为 None）。
    format_channels: Option<u16>,
    /// rt-host 数据面 format.sample_rate（文件/不可用时为 None）。
    format_sample_rate: Option<u64>,
    /// rt-host 数据面 format.block_size（文件/不可用时为 None）。
    format_block_frames: Option<u64>,
    /// rt-host 运行模式（aes67 / off）。
    mode: Option<String>,
    /// 契约 B status 的 reconfigure 增量（旧字段降级来源；channels_status 优先）。
    generation: Option<u64>,
    busy: Option<bool>,
    last_gap_ms: Option<u64>,
    last_result: Option<Value>,
    /// 数据来源级别的不可用原因。
    reasons: BTreeMap<String, String>,
}

/// 取当前数据面实况（同步；调用方在 blocking 线程池里）。
fn live_data(state: &DaemonState, snapshot: &LinkSnapshot, now: SystemTime) -> LiveData {
    match state.host_link().target() {
        Target::Process => {
            // process 目标：daemon 进程内控制面就是"数据面"（测试/离线演示）。
            let status = state.control().status_json();
            LiveData {
                source: "daemon-process",
                version: status.get("actual_version").and_then(Value::as_u64),
                graph_id: status
                    .get("graph_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                readback: status
                    .get("readback")
                    .filter(|value| value.is_object())
                    .cloned(),
                format_channels: status
                    .get("format")
                    .and_then(|format| format.get("channels"))
                    .and_then(Value::as_u64)
                    .and_then(|channels| u16::try_from(channels).ok()),
                format_sample_rate: status
                    .get("format")
                    .and_then(|format| format.get("sample_rate"))
                    .and_then(Value::as_u64),
                format_block_frames: status
                    .get("format")
                    .and_then(|format| format.get("block_size"))
                    .and_then(Value::as_u64),
                mode: status
                    .get("mode")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                generation: None,
                busy: None,
                last_gap_ms: None,
                last_result: None,
                reasons: BTreeMap::new(),
            }
        }
        Target::RtHost => match snapshot {
            LinkSnapshot::Socket { status, .. } => {
                let format = status.get("format");
                let reconfigure = status.get("reconfigure");
                LiveData {
                    source: "socket",
                    version: status.get("actual_version").and_then(Value::as_u64),
                    graph_id: status
                        .get("graph_id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    readback: status
                        .get("readback")
                        .filter(|value| value.is_object())
                        .cloned(),
                    format_channels: format
                        .and_then(|format| format.get("channels"))
                        .and_then(Value::as_u64)
                        .and_then(|channels| u16::try_from(channels).ok()),
                    format_sample_rate: format
                        .and_then(|format| format.get("sample_rate"))
                        .and_then(Value::as_u64),
                    format_block_frames: format
                        .and_then(|format| format.get("block_size"))
                        .and_then(Value::as_u64),
                    mode: status
                        .get("mode")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    generation: reconfigure
                        .and_then(|reconfigure| reconfigure.get("generation"))
                        .and_then(Value::as_u64),
                    busy: reconfigure
                        .and_then(|reconfigure| reconfigure.get("in_flight"))
                        .and_then(Value::as_bool),
                    last_gap_ms: reconfigure
                        .and_then(|reconfigure| reconfigure.get("last_gap_ms"))
                        .and_then(Value::as_u64),
                    last_result: reconfigure
                        .and_then(|reconfigure| reconfigure.get("last_result"))
                        .cloned(),
                    reasons: BTreeMap::new(),
                }
            }
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
                        readback: None,
                        format_channels: None,
                        format_sample_rate: None,
                        format_block_frames: None,
                        mode: None,
                        generation: None,
                        busy: None,
                        last_gap_ms: None,
                        last_result: None,
                        reasons,
                    },
                    other => {
                        let file_reason = other
                            .unavailable_reason()
                            .map(str::to_string)
                            .unwrap_or_else(|| "no rt-host status file is available".to_string());
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
        },
    }
}

/// 当前生效方案的 routing 视图。
///
/// 优先用 rt-host 控制 status 的 `readback.routing`：这是 rt-host 真实加载的 base plan，
/// 即使方案由别的 daemon/重启前准备，也能给出当前 lane -> DAC 绑定；拿不到或形状不可信时
/// 退回本 daemon 按 graph_id 记录的方案 routing 段（旧行为）。两条路都拿不到才 None。
fn current_routing(state: &DaemonState, live: &LiveData) -> Option<RoutingView> {
    if let Some(routing) = live
        .readback
        .as_ref()
        .and_then(|readback| routing_from_readback(readback, live.graph_id.as_deref()))
    {
        return Some(routing);
    }
    let graph_id = live.graph_id.as_deref()?;
    let record = state.plans().plan_by_graph_id(graph_id)?;
    record.routing()
}

/// 从控制 status 的 `readback` 解析 routing；形状不可信时返回 None（由调用方退回本地记录）。
///
/// 只接受完整、自洽的 routing：dac_channels 在 1..=u16::MAX、每条 route 的 lane/dac 都落在
/// dac_channels 内、同一个 DAC 不被两条 route 同时占用（否则 lane -> DAC 映射有歧义，不猜）。
fn routing_from_readback(readback: &Value, graph_id: Option<&str>) -> Option<RoutingView> {
    let routing = readback.get("routing").filter(|value| value.is_object())?;
    let dac_channels = routing
        .get("dac_channels")
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())?;
    if dac_channels == 0 {
        return None;
    }
    let routes = routing.get("routes")?.as_array()?;
    let mut parsed = Vec::with_capacity(routes.len());
    let mut used_dacs = BTreeSet::new();
    for route in routes {
        let lane = route
            .get("lane")
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())?;
        let dac = route
            .get("dac")
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())?;
        if lane >= dac_channels || dac >= dac_channels {
            return None;
        }
        if !used_dacs.insert(dac) {
            return None;
        }
        parsed.push((lane, dac));
    }
    Some(RoutingView {
        graph_id: graph_id.map(str::to_string),
        dac_channels,
        routes: parsed,
    })
}

// ===========================================================================
// GET /api/channels
// ===========================================================================

/// GET /api/channels。
pub(crate) async fn get_channels(State(state): State<Arc<DaemonState>>) -> Response {
    let worker = Arc::clone(&state);
    let body =
        match tokio::task::spawn_blocking(move || compose_channels(&worker, SystemTime::now()))
            .await
        {
            Ok(body) => body,
            Err(join_error) => {
                degraded_channels_body(&format!("channel aggregation task failed: {join_error}"))
            }
        };
    (StatusCode::OK, Json(body)).into_response()
}

/// 聚合任务 panic 时的兜底：schema 完整、所有字段 null + 原因，仍 200。
fn degraded_channels_body(reason: &str) -> Value {
    json!({
        "schema": SCHEMA,
        "target": Value::Null,
        "runtime_mode": Value::Null,
        "output": { "channels": Value::Null, "dac_map": Value::Null, "channels_source": Value::Null },
        "input": { "channels": Value::Null, "map": Value::Null, "source": Value::Null, "sdp_sha256": Value::Null },
        "mapping": Value::Null,
        "plan": { "source": Value::Null, "version": Value::Null, "graph_id": Value::Null, "routing_known": Value::Null, "routing_invalidated": Value::Null },
        "format": Value::Null,
        "device": Value::Null,
        "current": Value::Null,
        "generation": Value::Null,
        "busy": Value::Null,
        "pending": Value::Null,
        "last_gap_ms": Value::Null,
        "last_result": Value::Null,
        "applied": Value::Null,
        "hot_reconfigure": { "wired": Value::Null, "reason": reason },
        // T71 加法字段：profile 不可用时如实空数组 + 原因（不编角色）。
        "speaker_roles": [],
        "input_roles": [],
        "not_wired": [{ "path": "channels", "reason": reason }],
        "reasons": { "channels": reason, "speaker_roles": reason, "input_roles": reason },
    })
}

/// 合成 GET /api/channels 响应（同步；调用方在 blocking 线程池里）。
fn compose_channels(state: &DaemonState, now: SystemTime) -> Value {
    let (snapshot, channels_reply) = probe_channels(state);
    let live = live_data(state, &snapshot, now);
    let mut reasons = live.reasons.clone();
    let routing = current_routing(state, &live);
    let channels_status = match &channels_reply {
        ChannelsReply::Ok(body) => Some(body),
        other => {
            let reason = other
                .reason()
                .unwrap_or("rt-host channels status is unavailable");
            reasons.insert(
                "channels_status".to_string(),
                format!("rt-host channels_status is unavailable: {reason}"),
            );
            None
        }
    };
    let current = channels_status
        .and_then(|body| body.get("current"))
        .filter(|value| value.is_object());
    let mirror = state.channels_mirror();
    let routing_invalidation = mirror.routing_invalidation();
    // output.channels：channels_status.current.channels > 方案 routing.dac_channels > status.format.channels。
    let mut output_source: Option<&'static str> = None;
    let mut output_channels = current
        .and_then(|current| current.get("channels"))
        .and_then(Value::as_u64);
    if output_channels.is_some() {
        output_source = Some("rt-host-channels-status");
    }
    if output_channels.is_none() {
        output_channels = routing
            .as_ref()
            .map(|routing| u64::from(routing.dac_channels));
        if output_channels.is_some() {
            output_source = Some("plan-routing");
        }
    }
    if output_channels.is_none() {
        output_channels = live.format_channels.map(u64::from);
        if output_channels.is_some() {
            output_source = Some("rt-host-control-status");
        }
    }
    if output_channels.is_none() {
        reasons.insert(
            "output.channels".to_string(),
            "the rt-host channels status, the rt-host data-plane format and the applied-plan routing all fail to report output channels".to_string(),
        );
    }
    // 数据面通道数与方案 routing 的 dac_channels 不一致：方案 routing 不能代表运行中的图。
    let routing_dac_channels = routing
        .as_ref()
        .map(|routing| u64::from(routing.dac_channels));
    let routing_mismatch = match (routing_dac_channels, output_channels, output_source) {
        (Some(dac_channels), Some(channels), Some("rt-host-channels-status")) => {
            dac_channels != channels
        }
        _ => false,
    };
    if routing_mismatch {
        let reason = format!(
            "the applied plan routing declares dac_channels={} but the rt-host data plane currently reports {} output channels; the plan graph was rebuilt/dropped by a hot reconfiguration, so the lane -> DAC mapping is not trusted (commit a plan whose routing matches the running format)",
            routing_dac_channels.unwrap_or(0),
            output_channels.unwrap_or(0)
        );
        reasons.insert("mapping".to_string(), reason.clone());
        reasons.insert("output.dac_map".to_string(), reason);
    }
    if let Some(invalidation) = &routing_invalidation {
        reasons.insert("mapping".to_string(), invalidation.reason.clone());
        reasons.insert("output.dac_map".to_string(), invalidation.reason.clone());
    }
    let mapping_trusted = !routing_mismatch && routing_invalidation.is_none();
    let no_routing_reason = match &live.graph_id {
        Some(graph_id) => format!(
            "no applied-plan routing is available: the rt-host control status readback does not carry a usable routing section and the daemon has no plan record for graph_id {graph_id:?} (the plan was prepared/uploaded through another daemon or before a restart); the lane -> DAC mapping is not fabricated"
        ),
        None => "no applied plan is known: the rt-host control status reports no graph_id/readback routing and the daemon has no prepared plan".to_string(),
    };
    let (mapping, dac_map) = if !mapping_trusted {
        (Value::Null, Value::Null)
    } else {
        match &routing {
            Some(routing) => (routing.mapping_json(), routing.dac_map_json()),
            None => {
                reasons.insert("mapping".to_string(), no_routing_reason.clone());
                reasons.insert("output.dac_map".to_string(), no_routing_reason.clone());
                (Value::Null, Value::Null)
            }
        }
    };
    // input：AES67 接收配置来自 channels_status.current；source/sdp 控制通道不报。
    let input_channels = current
        .and_then(|current| current.get("aes67_channels"))
        .and_then(Value::as_u64);
    let input_map = current
        .and_then(|current| current.get("aes67_map"))
        .filter(|value| value.is_array())
        .cloned();
    if input_channels.is_none() {
        reasons.insert("input.channels".to_string(), AES67_INPUT_REASON.to_string());
    }
    if input_map.is_none() {
        reasons.insert("input.map".to_string(), AES67_INPUT_REASON.to_string());
    }
    reasons.insert("input.source".to_string(), AES67_SOURCE_REASON.to_string());
    reasons.insert("input.sdp_sha256".to_string(), AES67_SDP_REASON.to_string());
    // format：channels_status.current 的格式字段（引擎/设备口径）；拿不到时退回契约 B
    // status 的 data-plane format（仍是真的，但只有 flat 视图，block_frames 来自 block_size）。
    let format = match current {
        Some(current) => json!({
            "channels": current.get("channels").cloned().unwrap_or(Value::Null),
            "sample_rate": current.get("sample_rate").cloned().unwrap_or(Value::Null),
            "block_frames": current.get("block_frames").cloned().unwrap_or(Value::Null),
            "plan_rate": current.get("plan_rate").cloned().unwrap_or(Value::Null),
            "device": current.get("device").cloned().unwrap_or(Value::Null),
            "source": "rt-host-channels-status",
        }),
        None => match (
            live.format_channels,
            live.format_sample_rate,
            live.format_block_frames,
        ) {
            (None, None, None) => {
                reasons.insert(
                    "format".to_string(),
                    "the rt-host channels status (which reports the data-plane engine format) is unavailable".to_string(),
                );
                Value::Null
            }
            (channels, sample_rate, block_frames) => json!({
                "channels": channels,
                "sample_rate": sample_rate,
                "block_frames": block_frames,
                "plan_rate": Value::Null,
                "device": Value::Null,
                "source": "rt-host-control-status",
            }),
        },
    };
    // device：channels_status.device（name/state/probe）原样；拿不到 null + 原因。
    let device = match channels_status.and_then(|body| body.get("device")) {
        Some(device) if device.is_object() => device.clone(),
        _ => {
            reasons.insert(
                "device".to_string(),
                "the rt-host channels status does not report the output device/probe".to_string(),
            );
            Value::Null
        }
    };
    // generation / busy / pending / last_*：channels_status 优先，status.reconfigure 降级。
    let generation = channels_status
        .and_then(|body| body.get("generation"))
        .and_then(Value::as_u64)
        .or(live.generation);
    if generation.is_none() {
        reasons.insert(
            "generation".to_string(),
            "neither the rt-host channels status nor the control status reports a reconfiguration generation".to_string(),
        );
    }
    let busy = channels_status
        .and_then(|body| body.get("busy"))
        .and_then(Value::as_bool)
        .or(live.busy);
    let pending = channels_status
        .and_then(|body| body.get("pending"))
        .cloned()
        .unwrap_or(Value::Null);
    let last_gap_ms = channels_status
        .and_then(|body| body.get("last_gap_ms"))
        .and_then(Value::as_u64)
        .or(live.last_gap_ms);
    let last_result = channels_status
        .and_then(|body| body.get("last_result"))
        .cloned()
        .or(live.last_result);
    if let Some(Value::Object(channels_reasons)) =
        channels_status.and_then(|body| body.get("reasons"))
    {
        // rt-host 自己的原因（busy/device）：原样并入（我们的字段路径都带点，不会冲突）。
        for (key, value) in channels_reasons {
            if let Some(reason) = value.as_str() {
                reasons.insert(key.clone(), reason.to_string());
            }
        }
    }
    // 热重配能力：成功 = wired；rt-host 明确 unsupported = not wired + 原因；链路/未探测 = 无法核实。
    let process_target = state.host_link().target() == Target::Process;
    let (hot_wired, hot_reason) = if process_target {
        // process 目标：daemon 进程内引擎没有数据面热重配（不是"无法核实"，是确定的 false）。
        (Some(false), Some(HOT_RECONFIG_PROCESS_REASON.to_string()))
    } else {
        match &channels_reply {
        ChannelsReply::Ok(_) => (Some(true), None),
        ChannelsReply::Failure { code, message } if channels_reply.is_unsupported() => (
            Some(false),
            Some(format!(
                "{HOT_RECONFIG_UNSUPPORTED_REASON} (rt-host reported {code}: {message})"
            )),
        ),
        ChannelsReply::Failure { code, message } => (
            None,
            Some(format!(
                "cannot verify hot-reconfiguration support: rt-host rejected channels_status with {code}: {message}"
            )),
        ),
        ChannelsReply::Unavailable { reason } | ChannelsReply::Skipped { reason } => (
            None,
            Some(format!("cannot verify hot-reconfiguration support: {reason}")),
        ),
        }
    };
    if hot_reason.is_some() {
        reasons.insert(
            "channels.hot_reconfigure".to_string(),
            hot_reason.clone().unwrap_or_default(),
        );
    }
    // applied：daemon 本地镜像（快照，不是实时数据）。
    let applied = match mirror.last_commit() {
        Some(commit) => commit.to_json(),
        None => {
            reasons.insert(
                "applied".to_string(),
                "no channels configuration has been committed through this daemon since it started (the local mirror is empty; the live data plane is still reported from channels_status)".to_string(),
            );
            Value::Null
        }
    };
    let not_wired: Vec<Value> = if process_target {
        vec![json!({ "path": "channels.hot_reconfigure", "reason": HOT_RECONFIG_PROCESS_REASON })]
    } else if channels_reply.is_unsupported() {
        vec![json!({ "path": "channels.hot_reconfigure", "reason": hot_reason })]
    } else {
        Vec::new()
    };
    let routing_known = mapping_trusted && routing.is_some();
    // T71 加法字段：profile 的物理输出角色 / 已启用输入通道角色（给 routing/channels 页显示角色名）。
    let (speaker_roles, input_roles) = profile_role_fields(state, &mut reasons);
    json!({
        "ok": true,
        "schema": SCHEMA,
        "target": state.host_link().target().as_str(),
        "runtime_mode": live.mode,
        "output": {
            "channels": output_channels,
            "dac_map": dac_map,
            "channels_source": output_source,
        },
        "input": {
            "channels": input_channels,
            "map": input_map,
            "source": Value::Null,
            "sdp_sha256": Value::Null,
        },
        "mapping": mapping,
        "plan": {
            "source": live.source,
            "version": live.version,
            "graph_id": live.graph_id,
            "routing_known": routing_known,
            "routing_invalidated": routing_invalidation.map(|invalidation| json!({
                "generation": invalidation.generation,
                "recorded_at": invalidation.recorded_at,
                "reason": invalidation.reason,
            })),
        },
        "format": format,
        "device": device,
        "current": current.cloned().unwrap_or(Value::Null),
        "generation": generation,
        "busy": busy,
        "pending": pending,
        "last_gap_ms": last_gap_ms,
        "last_result": last_result,
        "applied": applied,
        "speaker_roles": speaker_roles,
        "input_roles": input_roles,
        "hot_reconfigure": {
            "wired": hot_wired,
            "reason": hot_reason,
        },
        "not_wired": not_wired,
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

/// /api/channels 的加法字段（T71）：profile 的 22 行输出角色 + 已启用输入源的角色通道。
///
/// 只读自 audio-profile store（首次运行默认 2.0）；profile 文件不可读时如实空数组 + 原因，
/// 不编角色。既有 channels 形状不变（纯加法）。
fn profile_role_fields(
    state: &DaemonState,
    reasons: &mut BTreeMap<String, String>,
) -> (Value, Value) {
    let current = state.audio_profile().current();
    let profile = match current {
        CurrentProfile::Default { .. } => AudioProfile::default(),
        CurrentProfile::Saved(saved) => saved.profile,
        CurrentProfile::Unreadable { reason } => {
            reasons.insert("speaker_roles".to_string(), reason.clone());
            reasons.insert("input_roles".to_string(), reason);
            return (json!([]), json!([]));
        }
    };
    let speaker_roles = profile
        .outputs
        .iter()
        .map(|row| {
            json!({
                "dac": row.dac,
                "role": row.role.as_str(),
                "enabled": row.enabled,
            })
        })
        .collect::<Vec<_>>();
    let mut input_roles = Vec::new();
    for source in &profile.sources {
        if !source.enabled {
            continue;
        }
        for channel in &source.channels {
            if channel.role == SpeakerRole::None {
                continue;
            }
            input_roles.push(json!({
                "source": source.kind.as_str(),
                "channel": channel.channel,
                "role": channel.role.as_str(),
            }));
        }
    }
    (json!(speaker_roles), json!(input_roles))
}

// ===========================================================================
// PUT /api/channels（两阶段）
// ===========================================================================

/// /api/channels 的业务错误（{error:{code,message,details}}，与 MOTU/plan 端点同形）。
#[derive(Debug)]
struct ChannelApiError {
    status: StatusCode,
    code: String,
    message: String,
    details: Value,
}

impl ChannelApiError {
    fn invalid(field: &str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: code::INVALID_REQUEST.to_string(),
            message: message.into(),
            details: json!({ "field": field }),
        }
    }

    fn unsupported(message: impl Into<String>, details: Value) -> Self {
        Self {
            status: StatusCode::NOT_IMPLEMENTED,
            code: "host_link_unsupported".to_string(),
            message: message.into(),
            details,
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: code::INTERNAL_ERROR.to_string(),
            message: message.into(),
            details: Value::Null,
        }
    }

    /// 控制通道断链 -> 503 + 稳定码 host_link_unavailable（不假成功、不重写原因）。
    fn unavailable(error: LinkError, details: Value) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: HOST_LINK_UNAVAILABLE.to_string(),
            message: error.message,
            details,
        }
    }

    /// rt-host 业务错误：错误码/文案原样透传，HTTP 状态按既有码表映射，details 带完整回执
    /// （commit 失败的 rolled_back/device_started/receiver_running/applied/generation 都在里面）。
    fn rt_host(code: String, message: String, body: Value, extra: Value) -> Self {
        let mut details = match body {
            Value::Object(object) => object,
            other => Map::from_iter([("body".to_string(), other)]),
        };
        if let Value::Object(extra) = extra {
            for (key, value) in extra {
                details.insert(key, value);
            }
        }
        Self {
            status: host_link_status(&code),
            code,
            message,
            details: Value::Object(details),
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

/// rt-host 业务错误码 -> HTTP 状态码（沿用 host_link 既有码表的扩展；见模块文档）。
fn host_link_status(code: &str) -> StatusCode {
    match code {
        "bad_request" => StatusCode::BAD_REQUEST,
        "unknown_op" | "unsupported" | "not_wired" | "not_implemented" => {
            StatusCode::NOT_IMPLEMENTED
        }
        "admission_rejected"
        | "engine_token_invalid"
        | "no_base_plan"
        | "no_history"
        | "no_channels_draft"
        | "channels_busy"
        | "reconfiguring"
        | "cal_busy" => StatusCode::CONFLICT,
        "device_unavailable" | "device_absent" => StatusCode::SERVICE_UNAVAILABLE,
        HOST_LINK_UNAVAILABLE => StatusCode::SERVICE_UNAVAILABLE,
        // 重配已在块边界执行过但失败/回滚：上游数据面错误（details 带 rolled_back 真值）。
        other if other.starts_with("ERR_RECONFIGURE_") => StatusCode::BAD_GATEWAY,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// PUT /api/channels：两阶段（stage=prepare|commit|abort，兼容 action 字段）。
pub(crate) async fn put_channels(State(state): State<Arc<DaemonState>>, body: Body) -> Response {
    let bytes = match axum::body::to_bytes(body, MAX_JSON_BODY).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return ChannelApiError::invalid("<body>", format!("cannot read request body: {error}"))
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
                    "<body>",
                    format!("request body must be valid JSON: {error}"),
                )
                .into_response()
            }
        }
    };
    let Some(object) = payload.as_object() else {
        return ChannelApiError::invalid("<body>", "request body must be a JSON object")
            .into_response();
    };
    let stage = match object.get("stage").or_else(|| object.get("action")) {
        None => "prepare",
        Some(Value::String(stage)) => stage.as_str(),
        Some(_) => {
            return ChannelApiError::invalid(
                "stage",
                "field stage must be a string (prepare|commit|abort)",
            )
            .into_response()
        }
    };
    let operation = match stage {
        "prepare" => Operation::Prepare,
        "commit" => Operation::Commit,
        "abort" => Operation::Abort,
        other => {
            return ChannelApiError::invalid(
                "stage",
                format!("stage must be prepare, commit or abort (got {other:?})"),
            )
            .into_response()
        }
    };
    // process 目标：任何阶段都如实 501（不存在本地草稿，签不出 token）。
    if state.host_link().target() == Target::Process {
        return ChannelApiError::unsupported(
            HOT_RECONFIG_PROCESS_REASON,
            json!({
                "target": Target::Process.as_str(),
                "stage": stage,
                "commit_supported": false,
                "reason": HOT_RECONFIG_PROCESS_REASON,
            }),
        )
        .into_response();
    }
    let object = object.clone();
    let worker = Arc::clone(&state);
    let result = tokio::task::spawn_blocking(move || match operation {
        Operation::Prepare => prepare_channels(&worker, &object, SystemTime::now()),
        Operation::Commit => commit_channels(&worker, &object, SystemTime::now()),
        Operation::Abort => abort_channels(&worker, &object, SystemTime::now()),
    })
    .await;
    match result {
        Ok(Ok(value)) => (StatusCode::OK, Json(value)).into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(join_error) => {
            ChannelApiError::internal(format!("channel {stage} task failed: {join_error}"))
                .into_response()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    Prepare,
    Commit,
    Abort,
}
// ---------------------------------------------------------------------------
// 补丁解析（daemon 词汇 -> rt-host 协议字段）
// ---------------------------------------------------------------------------

/// 解析后的 PUT 补丁。
#[derive(Debug, Default)]
struct ChannelsPatch {
    /// 要原样转发给 rt-host channels_prepare 的字段（键 = rt-host 协议字段名）。
    rt_fields: Map<String, Value>,
    /// 请求的 lane -> DAC mapping（dac 升序；来自 mapping 或 dac_map）。
    mapping: Option<Vec<(u16, u16)>>,
    /// mapping 的来源（"mapping" / "dac_map"），用于原因文案。
    mapping_source: &'static str,
    /// prepare 期间产生的警告（不阻塞，但要如实上报）。
    warnings: Vec<String>,
}

/// 允许的顶层字段（未知字段 400：拼错字段不能静默变成"什么都没改"）。
const PATCH_FIELDS: &[&str] = &[
    "stage",
    "action",
    "mapping",
    "dac_map",
    "output",
    "input",
    "output_channels",
    "channels",
    "sample_rate",
    "aes67_group",
    "aes67_port",
    "aes67_payload_type",
    "aes67_channels",
    "aes67_map",
    "input_channels",
    "input_map",
    "jitter_frames",
];

fn parse_channels_patch(object: &Map<String, Value>) -> Result<ChannelsPatch, ChannelApiError> {
    for key in object.keys() {
        if !PATCH_FIELDS.contains(&key.as_str()) {
            return Err(ChannelApiError::invalid(
                key,
                format!("unknown field {key:?}; supported fields are {PATCH_FIELDS:?}"),
            ));
        }
    }
    let mut patch = ChannelsPatch::default();
    let output = nested_object(object, "output")?;
    let input = nested_object(object, "input")?;
    // ---- 输出通道数（channels / output_channels / output.channels）----
    let mut explicit_channels: Option<u64> = None;
    for (field, value) in [
        ("channels", object.get("channels")),
        ("output_channels", object.get("output_channels")),
        (
            "output.channels",
            output.as_ref().and_then(|output| output.get("channels")),
        ),
    ] {
        if let Some(value) = value {
            let channels = unsigned(value, field)?;
            if let Some(previous) = explicit_channels {
                if previous != channels {
                    return Err(ChannelApiError::invalid(
                        field,
                        format!("conflicting output channel counts: {previous} vs {channels}"),
                    ));
                }
            }
            explicit_channels = Some(channels);
        }
    }
    // ---- dac_map（DAC -> lane；null = 静音槽）----
    let dac_map_field = match (
        object.get("dac_map"),
        output.as_ref().and_then(|output| output.get("dac_map")),
    ) {
        (Some(_), Some(_)) => {
            return Err(ChannelApiError::invalid(
                "dac_map",
                "give dac_map either at the top level or inside output, not both",
            ))
        }
        (Some(value), None) => Some(("dac_map", value)),
        (None, Some(value)) => Some(("output.dac_map", value)),
        (None, None) => None,
    };
    let dac_map = match dac_map_field {
        Some((field, value)) => Some(parse_dac_map(field, value)?),
        None => None,
    };
    // ---- mapping（[{lane,dac}]）----
    let mapping = match object.get("mapping") {
        Some(value) => Some(parse_mapping(value)?),
        None => None,
    };
    if let (Some(mapping), Some((_, dac_map))) = (&mapping, &dac_map) {
        let left: BTreeSet<(u16, u16)> = mapping.iter().copied().collect();
        let right: BTreeSet<(u16, u16)> = dac_map.iter().copied().collect();
        if left != right {
            return Err(ChannelApiError::invalid(
                "mapping",
                "mapping and dac_map describe different lane -> DAC routes; give only one form (or make them identical)",
            ));
        }
    }
    match (&mapping, &dac_map) {
        (Some(mapping), _) => {
            patch.mapping = Some(mapping.clone());
            patch.mapping_source = "mapping";
        }
        (None, Some((_, dac_map))) => {
            patch.mapping = Some(dac_map.clone());
            patch.mapping_source = "dac_map";
        }
        (None, None) => {}
    }
    // ---- 派生输出通道数（dac_map 的长度）----
    let mut channels = explicit_channels;
    if let (Some(explicit), Some((entries, _))) = (explicit_channels, &dac_map) {
        if explicit != *entries as u64 {
            return Err(ChannelApiError::invalid(
                "dac_map",
                format!(
                    "dac_map has {entries} entries but channels is {explicit}: dac_map length must equal the output channel count"
                ),
            ));
        }
    }
    if channels.is_none() {
        if let Some((entries, _)) = &dac_map {
            channels = Some(*entries as u64);
            patch.warnings.push(format!(
                "output channel count was derived from the dac_map length ({entries})"
            ));
        }
    }
    if let Some(channels) = channels {
        patch
            .rt_fields
            .insert("channels".to_string(), json!(channels));
    }
    // ---- 其余 rt-host 通道字段（原样转发）----
    for field in [
        "sample_rate",
        "aes67_port",
        "aes67_payload_type",
        "jitter_frames",
    ] {
        if let Some(value) = object.get(field) {
            patch
                .rt_fields
                .insert(field.to_string(), json!(unsigned(value, field)?));
        }
    }
    if let Some(value) = object.get("aes67_group") {
        let group = value.as_str().ok_or_else(|| {
            ChannelApiError::invalid(
                "aes67_group",
                "field aes67_group must be an IPv4 address string",
            )
        })?;
        patch
            .rt_fields
            .insert("aes67_group".to_string(), json!(group));
    }
    // AES67 输入通道数 / 映射：
    // aes67_channels / input_channels / input.channels，aes67_map / input_map / input.map。
    let mut input_channels: Option<(String, u64)> = None;
    for (field, value) in [
        ("aes67_channels", object.get("aes67_channels")),
        ("input_channels", object.get("input_channels")),
        (
            "input.channels",
            input.as_ref().and_then(|input| input.get("channels")),
        ),
    ] {
        if let Some(value) = value {
            let channels = unsigned(value, field)?;
            if let Some((previous_field, previous)) = &input_channels {
                if *previous != channels {
                    return Err(ChannelApiError::invalid(
                        field,
                        format!(
                            "conflicting AES67 input channel counts: {previous} ({previous_field}) vs {channels}"
                        ),
                    ));
                }
            }
            input_channels = Some((field.to_string(), channels));
        }
    }
    if let Some((_, channels)) = input_channels {
        patch
            .rt_fields
            .insert("aes67_channels".to_string(), json!(channels));
    }
    let mut input_map: Option<(String, Value)> = None;
    for (field, value) in [
        ("aes67_map", object.get("aes67_map")),
        ("input_map", object.get("input_map")),
        (
            "input.map",
            input.as_ref().and_then(|input| input.get("map")),
        ),
    ] {
        if let Some(value) = value {
            let entries = value.as_array().ok_or_else(|| {
                ChannelApiError::invalid(
                    field,
                    format!("field {field} must be an array of output slot indexes"),
                )
            })?;
            let mut map = Vec::with_capacity(entries.len());
            for entry in entries {
                let slot = entry.as_u64().ok_or_else(|| {
                    ChannelApiError::invalid(
                        field,
                        format!("field {field} entries must be non-negative integers"),
                    )
                })?;
                map.push(json!(slot));
            }
            if let Some((previous_field, previous)) = &input_map {
                if *previous != Value::Array(map.clone()) {
                    return Err(ChannelApiError::invalid(
                        field,
                        format!(
                            "conflicting AES67 input maps given by {previous_field} and {field}"
                        ),
                    ));
                }
            }
            input_map = Some((field.to_string(), Value::Array(map)));
        }
    }
    if let Some((_, map)) = input_map {
        patch.rt_fields.insert("aes67_map".to_string(), map);
    }
    // ---- 嵌套组里出现未知键：拼错不静默 ----
    if let Some(output) = &output {
        for key in output.keys() {
            if !matches!(key.as_str(), "channels" | "dac_map") {
                return Err(ChannelApiError::invalid(
                    "output",
                    format!(
                        "unknown field output.{key:?}; supported fields are [\"channels\", \"dac_map\"]"
                    ),
                ));
            }
        }
    }
    if let Some(input) = &input {
        for key in input.keys() {
            if !matches!(key.as_str(), "channels" | "map") {
                return Err(ChannelApiError::invalid(
                    "input",
                    format!(
                        "unknown field input.{key:?}; supported fields are [\"channels\", \"map\"]"
                    ),
                ));
            }
        }
    }
    if patch.rt_fields.is_empty() && patch.mapping.is_none() {
        return Err(ChannelApiError::invalid(
            "<body>",
            format!(
                "patch must contain at least one channel field; supported fields are {PATCH_FIELDS:?}"
            ),
        ));
    }
    Ok(patch)
}

fn nested_object(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<Map<String, Value>>, ChannelApiError> {
    match object.get(field) {
        None => Ok(None),
        Some(Value::Object(object)) => Ok(Some(object.clone())),
        Some(_) => Err(ChannelApiError::invalid(
            field,
            format!("field {field} must be an object"),
        )),
    }
}

fn unsigned(value: &Value, field: &str) -> Result<u64, ChannelApiError> {
    value.as_u64().ok_or_else(|| {
        ChannelApiError::invalid(
            field,
            format!("field {field} must be a non-negative integer"),
        )
    })
}

/// 解析 mapping（[{lane,dac}]；lane/dac 都必须是 u16 且各自不重复）。
fn parse_mapping(value: &Value) -> Result<Vec<(u16, u16)>, ChannelApiError> {
    let entries = value.as_array().ok_or_else(|| {
        ChannelApiError::invalid("mapping", "field mapping must be an array of {lane, dac}")
    })?;
    if entries.is_empty() {
        return Err(ChannelApiError::invalid(
            "mapping",
            "field mapping must not be empty",
        ));
    }
    if entries.len() > MAX_MAPPING_ENTRIES {
        return Err(ChannelApiError::invalid(
            "mapping",
            format!(
                "mapping has too many entries ({} > {MAX_MAPPING_ENTRIES})",
                entries.len()
            ),
        ));
    }
    let mut lanes: BTreeSet<u16> = BTreeSet::new();
    let mut dacs: BTreeSet<u16> = BTreeSet::new();
    let mut mapping = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let Some(entry) = entry.as_object() else {
            return Err(ChannelApiError::invalid(
                "mapping",
                format!("mapping[{index}] must be an object {{lane, dac}}"),
            ));
        };
        let lane = u16_field(entry, "lane", index)?;
        let dac = u16_field(entry, "dac", index)?;
        if !lanes.insert(lane) {
            return Err(ChannelApiError::invalid(
                "mapping",
                format!("mapping lists lane {lane} more than once"),
            ));
        }
        if !dacs.insert(dac) {
            return Err(ChannelApiError::invalid(
                "mapping",
                format!("mapping sends two lanes to dac {dac}"),
            ));
        }
        mapping.push((lane, dac));
    }
    mapping.sort_unstable_by_key(|(_, dac)| *dac);
    Ok(mapping)
}

/// 解析 dac_map（DAC -> lane；null = 该 DAC 静音）。
/// 返回 (DAC 槽位数, (lane, dac) 且 dac 升序)；空槽（null）只体现在长度里。
fn parse_dac_map(field: &str, value: &Value) -> Result<(usize, Vec<(u16, u16)>), ChannelApiError> {
    let entries = value.as_array().ok_or_else(|| {
        ChannelApiError::invalid(
            field,
            format!("field {field} must be an array (DAC index -> lane, null = muted)"),
        )
    })?;
    if entries.is_empty() {
        return Err(ChannelApiError::invalid(
            field,
            format!("field {field} must not be empty"),
        ));
    }
    if entries.len() > MAX_MAPPING_ENTRIES {
        return Err(ChannelApiError::invalid(
            field,
            format!(
                "{field} has too many entries ({} > {MAX_MAPPING_ENTRIES})",
                entries.len()
            ),
        ));
    }
    let mut lanes: BTreeSet<u16> = BTreeSet::new();
    let mut mapping = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if entry.is_null() {
            continue;
        }
        let lane = entry.as_u64().ok_or_else(|| {
            ChannelApiError::invalid(
                field,
                format!("{field}[{index}] must be a non-negative integer lane or null"),
            )
        })?;
        let lane = u16::try_from(lane).map_err(|_| {
            ChannelApiError::invalid(field, format!("{field}[{index}] does not fit in u16"))
        })?;
        if !lanes.insert(lane) {
            return Err(ChannelApiError::invalid(
                field,
                format!("{field} lists lane {lane} more than once"),
            ));
        }
        let dac = u16::try_from(index).map_err(|_| {
            ChannelApiError::invalid(field, format!("{field} is too long for u16 dac indexes"))
        })?;
        mapping.push((lane, dac));
    }
    Ok((entries.len(), mapping))
}

fn u16_field(
    entry: &Map<String, Value>,
    field: &str,
    index: usize,
) -> Result<u16, ChannelApiError> {
    match entry.get(field).and_then(Value::as_u64) {
        Some(value) => u16::try_from(value).map_err(|_| {
            ChannelApiError::invalid(
                "mapping",
                format!("mapping[{index}].{field} does not fit in u16"),
            )
        }),
        None => Err(ChannelApiError::invalid(
            "mapping",
            format!("mapping[{index}].{field} must be a non-negative integer"),
        )),
    }
}
// ---------------------------------------------------------------------------
// prepare
// ---------------------------------------------------------------------------

/// prepare 时关联到的当前方案路由（用于 diff / 边界校验；拿不到就少做校验并如实给原因）。
#[derive(Debug, Default)]
struct BaseRouting {
    graph_id: Option<String>,
    routing: Option<RoutingView>,
    out_lanes: Option<u16>,
}

/// 需要 base 时探一次 rt-host status（op status；只读、带一次重试）。
fn probe_base_routing(state: &DaemonState) -> Result<BaseRouting, ChannelApiError> {
    match state.host_link().status_snapshot() {
        LinkSnapshot::Socket { status, .. } => {
            let graph_id = status
                .get("graph_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            let record = graph_id
                .as_deref()
                .and_then(|graph_id| state.plans().plan_by_graph_id(graph_id));
            let routing = record.as_ref().and_then(PlanRecord::routing);
            let out_lanes = record.as_ref().and_then(|record| record.out_lanes);
            Ok(BaseRouting {
                graph_id,
                routing,
                out_lanes,
            })
        }
        LinkSnapshot::Unavailable { reason, .. } => Err(ChannelApiError::unavailable(
            LinkError {
                code: HOST_LINK_UNAVAILABLE,
                message: format!(
                    "cannot prepare a channels change: rt-host control socket is unavailable ({reason})"
                ),
            },
            json!({ "phase": "channels_prepare", "target": Target::RtHost.as_str() }),
        )),
        LinkSnapshot::Process => Err(ChannelApiError::unsupported(
            HOT_RECONFIG_PROCESS_REASON,
            json!({ "target": Target::Process.as_str(), "stage": "prepare" }),
        )),
    }
}

/// 校验 mapping 的边界（能拿到的边界才校验；拿不到就如实说明只做了结构校验）。
///
/// DAC 边界恒严格（输出通道数装不下的 DAC 不可能生效）。lane 边界只在
/// `strict_lanes`（显式 `mapping`）时严格拒绝；`dac_map` 允许指向当前方案之外的 lane：
/// 那是给"目标方案"（例如输出通道增长后的新 plan）准备的槽位，prepare 只提示、不拒绝。
fn validate_mapping_bounds(
    mapping: &[(u16, u16)],
    channels: Option<u64>,
    base: Option<&BaseRouting>,
    strict_lanes: bool,
) -> Result<(BTreeMap<String, String>, Vec<String>), ChannelApiError> {
    let mut reasons = BTreeMap::new();
    let mut warnings = Vec::new();
    let bound = channels
        .map(|channels| u16::try_from(channels).unwrap_or(u16::MAX))
        .or_else(|| {
            base.and_then(|base| base.routing.as_ref().map(|routing| routing.dac_channels))
        });
    let Some(bound) = bound else {
        reasons.insert(
            "mapping".to_string(),
            "the current applied-plan routing and the target output channel count are both unknown, so only the mapping structure (u16 / unique lane / unique DAC) was validated".to_string(),
        );
        return Ok((reasons, warnings));
    };
    for (lane, dac) in mapping {
        if *dac >= bound {
            return Err(ChannelApiError::invalid(
                "mapping",
                format!("mapping targets dac {dac} but the target has {bound} output channels"),
            ));
        }
        if let Some(out_lanes) = base.and_then(|base| base.out_lanes) {
            if *lane >= out_lanes {
                if strict_lanes {
                    return Err(ChannelApiError::invalid(
                        "mapping",
                        format!(
                            "mapping targets lane {lane} but the running plan has out_lanes {out_lanes}"
                        ),
                    ));
                }
                warnings.push(format!(
                    "dac_map references lane {lane} beyond the running plan out_lanes {out_lanes}: the mapping only takes effect for a future plan whose routing section has at least {} lane(s)",
                    u32::from(*lane) + 1
                ));
            }
        }
    }
    if base.and_then(|base| base.routing.as_ref()).is_none() {
        reasons.insert(
            "mapping.current".to_string(),
            "the current applied-plan routing is unknown, so the planned diff is computed against null and only structural/bounds validation was performed".to_string(),
        );
    }
    Ok((reasons, warnings))
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

fn mapping_json(mapping: &[(u16, u16)]) -> Value {
    let mut sorted = mapping.to_vec();
    sorted.sort_unstable_by_key(|(lane, _)| *lane);
    Value::Array(
        sorted
            .into_iter()
            .map(|(lane, dac)| json!({"lane": lane, "dac": dac}))
            .collect(),
    )
}

/// prepare 的 mapping 段（如实说明这是方案路由、不是 channels_commit 能改的字段）。
fn mapping_section(
    patch: &ChannelsPatch,
    base_mapping: Option<&[(u16, u16)]>,
    mapping_changed: Option<bool>,
) -> Value {
    let target = patch.mapping.clone().unwrap_or_default();
    json!({
        "requested": true,
        "source": patch.mapping_source,
        "current": base_mapping.map(mapping_json),
        "target": mapping_json(&target),
        "changed": mapping_changed,
        "diff": planned_diff(base_mapping, &target),
        "applied_by": "plan routing section (plan prepare + commit)",
        "applies_with_this_commit": false,
        "reason": MAPPING_PLAN_OWNED_REASON,
    })
}

/// prepare 回执的 preview（webui 兼容：old/new channels + input_channels；gap 不估）。
fn preview_from_prepare(response: &Value) -> Value {
    let channels = |view: Option<&Value>| {
        view.and_then(Value::as_object).map(|object| {
            json!({
                "channels": object.get("channels").cloned().unwrap_or(Value::Null),
                "input_channels": object.get("aes67_channels").cloned().unwrap_or(Value::Null),
            })
        })
    };
    json!({
        "old": channels(response.get("current")).unwrap_or(Value::Null),
        "new": channels(response.get("target")).unwrap_or(Value::Null),
        "audio_gap_ms_estimate": Value::Null,
        "audio_gap_ms_estimate_reason": "the real mute-to-resume gap is measured at commit (gap_ms); prepare does not estimate it",
    })
}
/// 第一阶段：解析/校验补丁 -> rt-host channels_prepare（除非只有方案路由改动）。
fn prepare_channels(
    state: &DaemonState,
    object: &Map<String, Value>,
    now: SystemTime,
) -> Result<Value, ChannelApiError> {
    let patch = parse_channels_patch(object)?;
    let mut reasons = BTreeMap::new();
    let mut warnings = patch.warnings.clone();
    let base = if patch.mapping.is_some() {
        Some(probe_base_routing(state)?)
    } else {
        None
    };
    let base_mapping = base
        .as_ref()
        .and_then(|base| base.routing.as_ref())
        .map(|routing| routing.routes.clone());
    let mut mapping_changed = None;
    let rt_fields = patch.rt_fields.clone();
    if let Some(mapping) = &patch.mapping {
        // 目标输出通道数：显式 channels / dac_map 长度优先；只给 mapping 时沿用自己的
        // 当前通道数（不猜）。mapping 指向当前范围外的 DAC 时必须显式给 channels：
        // 悄悄把输出通道数按 max(dac)+1 放大 = 不告诉用户就重开设备，这里拒绝并要求显式表达。
        let target_channels = rt_fields.get("channels").and_then(Value::as_u64);
        let base_channels = base
            .as_ref()
            .and_then(|base| base.routing.as_ref())
            .map(|routing| u64::from(routing.dac_channels));
        if let (None, Some(base_channels)) = (target_channels, base_channels) {
            if let Some(max_dac) = mapping.iter().map(|(_, dac)| u64::from(*dac)).max() {
                if max_dac + 1 > base_channels {
                    return Err(ChannelApiError::invalid(
                        "mapping",
                        format!(
                            "mapping targets dac {max_dac} but the running plan has dac_channels={base_channels}: give channels (or a dac_map of the target length) explicitly to grow the output channel count instead of silently deriving it"
                        ),
                    ));
                }
            }
        }
        let (mapping_reasons, mapping_warnings) = validate_mapping_bounds(
            mapping,
            target_channels,
            base.as_ref(),
            patch.mapping_source == "mapping",
        )?;
        reasons.extend(mapping_reasons);
        warnings.extend(mapping_warnings);
        mapping_changed = base_mapping
            .as_ref()
            .map(|current| current.as_slice() != mapping.as_slice());
        if mapping_changed == Some(true) {
            warnings.push(
                "the lane -> DAC mapping change cannot be applied by channels_commit: it is the plan routing section and takes effect when a matching plan is prepared and committed (see mapping.reason)".to_string(),
            );
        }
        if let Some(base) = &base {
            if base.graph_id.is_none() {
                reasons.insert(
                    "mapping.current".to_string(),
                    "the rt-host control status reports no applied graph_id, so the current routing (and the planned diff) is unknown".to_string(),
                );
            } else if base.routing.is_none() {
                reasons.insert(
                    "mapping.current".to_string(),
                    format!(
                        "rt-host reports graph_id {:?} but the daemon has no plan record for it (the plan was prepared/uploaded through another daemon or before a restart), so the current routing and planned diff are unknown",
                        base.graph_id
                    ),
                );
            }
        }
    }
    // 只有方案路由改动、没有任何 rt-host 可应用字段：如实拒绝签发 token（不假成功）。
    if patch.mapping.is_some() && rt_fields.is_empty() {
        state.channels_mirror().clear_pending();
        let base_view = base.as_ref().and_then(|base| base.routing.as_ref());
        return Ok(json!({
            "ok": true,
            "schema": SCHEMA,
            "stage": "prepare",
            "target": Target::RtHost.as_str(),
            "engine_token": Value::Null,
            "token": Value::Null,
            "planned_generation": Value::Null,
            "changed": mapping_changed,
            "commit_supported": false,
            "mapping": mapping_section(&patch, base_mapping.as_deref(), mapping_changed),
            "diff": Value::Null,
            "current": base_view.map(|routing| routing.to_json()),
            "target": patch.mapping.as_ref().map(|mapping| mapping_json(mapping)),
            "preview": Value::Null,
            "warnings": warnings,
            "blockers": [MAPPING_PLAN_OWNED_REASON],
            "not_wired": [{ "path": "channels.hot_reconfigure.mapping", "reason": MAPPING_PLAN_OWNED_REASON }],
            "reasons": reasons_json(&reasons),
            "recorded_at": crate::status::rfc3339_utc(now),
        }));
    }
    state.channels_mirror().clear_pending();
    let mut request = Map::new();
    request.insert("op".to_string(), json!("channels_prepare"));
    for (field, value) in rt_fields {
        request.insert(field, value);
    }
    let reply = state.host_link().call(Value::Object(request));
    match reply {
        Ok(WireReply::Success(body)) => {
            let token = body
                .get("engine_token")
                .or_else(|| body.get("token"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| {
                    ChannelApiError::internal(
                        "rt-host channels_prepare answered ok=true without an engine_token; the draft cannot be committed without one",
                    )
                })?;
            let planned_generation = body.get("planned_generation").and_then(Value::as_u64);
            if let Some(mapping) = &patch.mapping {
                state.channels_mirror().put_pending(PendingRouting {
                    token: token.clone(),
                    planned_generation,
                    base_mapping: base_mapping.clone(),
                    target_mapping: mapping.clone(),
                    mapping_changed,
                    recorded_at: crate::status::rfc3339_utc(now),
                });
            }
            let graph_dropped_warning = body
                .get("graph")
                .and_then(|graph| graph.get("action"))
                .and_then(Value::as_str)
                .filter(|action| *action == "dropped")
                .map(|_| {
                    "rt-host will drop the running plan graph at commit (the engine format changes): a plan whose routing matches the new format must be prepared and committed afterwards".to_string()
                });
            if let Some(warning) = graph_dropped_warning {
                warnings.push(warning);
            }
            let mut response = body;
            let preview = preview_from_prepare(&response);
            let object = response.as_object_mut().ok_or_else(|| {
                ChannelApiError::internal("rt-host channels_prepare response is not a JSON object")
            })?;
            object.insert("schema".to_string(), json!(SCHEMA));
            object.insert("stage".to_string(), json!("prepare"));
            object.insert("target".to_string(), json!(Target::RtHost.as_str()));
            object.insert("commit_supported".to_string(), json!(true));
            if patch.mapping.is_some() {
                object.insert(
                    "mapping".to_string(),
                    mapping_section(&patch, base_mapping.as_deref(), mapping_changed),
                );
            } else {
                object.insert(
                    "mapping".to_string(),
                    json!({
                        "requested": false,
                        "reason": "no lane -> DAC mapping change was requested; the running plan routing is untouched",
                    }),
                );
            }
            object.insert("preview".to_string(), preview);
            object.insert("warnings".to_string(), json!(warnings));
            object.insert("blockers".to_string(), json!([]));
            object.insert("not_wired".to_string(), json!([]));
            object.insert("reasons".to_string(), reasons_json(&reasons));
            Ok(response)
        }
        Ok(WireReply::Failure {
            code,
            message,
            body,
        }) => Err(ChannelApiError::rt_host(
            code,
            message,
            body,
            json!({ "phase": "channels_prepare", "target": Target::RtHost.as_str() }),
        )),
        Err(error) => Err(ChannelApiError::unavailable(
            error,
            json!({ "phase": "channels_prepare", "target": Target::RtHost.as_str() }),
        )),
    }
}

// ---------------------------------------------------------------------------
// commit / abort
// ---------------------------------------------------------------------------

/// commit 的 token 字段（engine_token 或 token；非空字符串）。
fn token_field(object: &Map<String, Value>) -> Result<String, ChannelApiError> {
    match object.get("engine_token").or_else(|| object.get("token")) {
        Some(Value::String(token)) if !token.trim().is_empty() => Ok(token.trim().to_string()),
        Some(_) => Err(ChannelApiError::invalid(
            "engine_token",
            "field engine_token must be a non-empty string",
        )),
        None => Err(ChannelApiError::invalid(
            "engine_token",
            "missing field: engine_token (commit/abort the token returned by stage=prepare)",
        )),
    }
}

/// 第二阶段：token 交给 rt-host channels_commit；成功回执原样透传 + 本地镜像更新。
fn commit_channels(
    state: &DaemonState,
    object: &Map<String, Value>,
    now: SystemTime,
) -> Result<Value, ChannelApiError> {
    let token = token_field(object)?;
    let reply = state
        .host_link()
        .call(json!({ "op": "channels_commit", "token": token }));
    match reply {
        Ok(WireReply::Success(body)) => {
            let recorded_at = crate::status::rfc3339_utc(now);
            let pending = state.channels_mirror().peek_pending(&token);
            let mirrored = state.channels_mirror().record_commit(&body, &recorded_at);
            let mut response = body;
            let routing = commit_routing_section(&response, pending.as_ref());
            let object = response.as_object_mut().ok_or_else(|| {
                ChannelApiError::internal("rt-host channels_commit response is not a JSON object")
            })?;
            object.insert("schema".to_string(), json!(SCHEMA));
            object.insert("stage".to_string(), json!("commit"));
            object.insert("target".to_string(), json!(Target::RtHost.as_str()));
            object.insert("routing".to_string(), routing);
            object.insert(
                "local".to_string(),
                json!({
                    "mirror_updated": mirrored.is_some(),
                    "pending_routing_known": pending.is_some(),
                    "recorded_at": recorded_at,
                }),
            );
            Ok(response)
        }
        Ok(WireReply::Failure {
            code,
            message,
            body,
        }) => {
            // 草稿确实不在了（rt-host 报 no_channels_draft）时才丢弃本地意图；busy/cal_busy/
            // token 不符时草稿还在，保留以便重试（不把可重试错误变成永久失败）。
            if code == "no_channels_draft" {
                state.channels_mirror().clear_pending();
            }
            Err(ChannelApiError::rt_host(
                code,
                message,
                body,
                json!({ "phase": "channels_commit", "target": Target::RtHost.as_str() }),
            ))
        }
        Err(error) => Err(ChannelApiError::unavailable(
            error,
            json!({ "phase": "channels_commit", "target": Target::RtHost.as_str() }),
        )),
    }
}

/// commit 回执的 routing 段：channels_commit 不改方案 routing，这里如实说明是否还需方案重提交。
fn commit_routing_section(response: &Value, pending: Option<&PendingRouting>) -> Value {
    let graph_dropped = response
        .get("graph_dropped")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    match pending {
        Some(pending) => {
            let mapping_changed = pending.mapping_changed;
            json!({
                "requested": true,
                "changed": mapping_changed,
                "current": pending.base_mapping.as_ref().map(|mapping| mapping_json(mapping)),
                "target": mapping_json(&pending.target_mapping),
                "applied": false,
                "applied_by": "plan routing section (plan prepare + commit)",
                "graph_dropped": graph_dropped,
                "plan_recommit_required": graph_dropped || mapping_changed != Some(false),
                "reason": MAPPING_PLAN_OWNED_REASON,
            })
        }
        None => json!({
            "requested": Value::Null,
            "applied": false,
            "graph_dropped": graph_dropped,
            "plan_recommit_required": graph_dropped,
            "reason": "no lane -> DAC routing draft is known to this daemon for this token (prepared by another client or before a daemon restart); channels_commit never changes the plan routing section, so a plan whose routing matches the new format must be committed separately",
        }),
    }
}

/// abort：丢弃 rt-host 草稿（本地意图一并丢弃）。
fn abort_channels(
    state: &DaemonState,
    object: &Map<String, Value>,
    now: SystemTime,
) -> Result<Value, ChannelApiError> {
    let token = token_field(object)?;
    let reply = state
        .host_link()
        .call(json!({ "op": "channels_abort", "token": token }));
    match reply {
        Ok(WireReply::Success(body)) => {
            state.channels_mirror().clear_pending();
            let mut response = body;
            let object = response.as_object_mut().ok_or_else(|| {
                ChannelApiError::internal("rt-host channels_abort response is not a JSON object")
            })?;
            object.insert("schema".to_string(), json!(SCHEMA));
            object.insert("stage".to_string(), json!("abort"));
            object.insert("target".to_string(), json!(Target::RtHost.as_str()));
            object.insert(
                "local".to_string(),
                json!({
                    "pending_routing_discarded": true,
                    "recorded_at": crate::status::rfc3339_utc(now),
                }),
            );
            Ok(response)
        }
        Ok(WireReply::Failure {
            code,
            message,
            body,
        }) => {
            if code == "no_channels_draft" {
                state.channels_mirror().clear_pending();
            }
            Err(ChannelApiError::rt_host(
                code,
                message,
                body,
                json!({ "phase": "channels_abort", "target": Target::RtHost.as_str() }),
            ))
        }
        Err(error) => Err(ChannelApiError::unavailable(
            error,
            json!({ "phase": "channels_abort", "target": Target::RtHost.as_str() }),
        )),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().expect("object").clone()
    }

    #[test]
    fn readback_routing_parses_a_complete_mapping() {
        let routing = routing_from_readback(
            &json!({
                "available": true,
                "source": "base_plan",
                "routing": {
                    "dac_channels": 4,
                    "routes": [{"lane": 0, "dac": 1}, {"lane": 1, "dac": 3}]
                }
            }),
            Some("graph-external"),
        )
        .expect("routing");
        assert_eq!(routing.dac_channels, 4);
        assert_eq!(routing.routes, vec![(0, 1), (1, 3)]);
        assert_eq!(routing.graph_id.as_deref(), Some("graph-external"));
    }

    #[test]
    fn readback_routing_rejects_ambiguous_or_out_of_range_shapes() {
        for bad in [
            json!({ "routing": null }),
            json!({ "routing": { "dac_channels": 0, "routes": [] } }),
            json!({ "routing": { "dac_channels": 4, "routes": [{ "lane": 0, "dac": 4 }] } }),
            json!({ "routing": { "dac_channels": 4, "routes": [{ "lane": 4, "dac": 0 }] } }),
            json!({ "routing": { "dac_channels": 4, "routes": [
                { "lane": 0, "dac": 1 }, { "lane": 1, "dac": 1 }
            ] } }),
            json!({ "routing": { "dac_channels": 4, "routes": [{ "lane": 0 }] } }),
            json!({ "routing": { "dac_channels": 4 } }),
        ] {
            assert!(
                routing_from_readback(&bad, None).is_none(),
                "must not trust {bad}"
            );
        }
    }

    #[test]
    fn patch_parses_flat_and_nested_aliases_into_rt_host_fields() {
        let patch = parse_channels_patch(&object(json!({
            "output": { "channels": 6, "dac_map": [0, 1, 2, 3, 4, 5] },
            "input": { "channels": 4, "map": [0, 1, 2, 3] },
            "sample_rate": 48_000,
            "aes67_group": "239.69.1.1",
            "aes67_port": 5_004,
            "aes67_payload_type": 98,
            "jitter_frames": 256,
        })))
        .expect("patch");
        assert_eq!(
            patch.rt_fields,
            object(json!({
                "channels": 6,
                "sample_rate": 48_000,
                "aes67_group": "239.69.1.1",
                "aes67_port": 5_004,
                "aes67_payload_type": 98,
                "aes67_channels": 4,
                "aes67_map": [0, 1, 2, 3],
                "jitter_frames": 256,
            }))
        );
        assert_eq!(
            patch.mapping,
            Some(vec![(0, 0), (1, 1), (2, 2), (3, 3), (4, 4), (5, 5)])
        );
        assert!(
            patch.warnings.is_empty(),
            "dac_map with explicit channels: no derivation warning"
        );
    }

    #[test]
    fn dac_map_length_derives_the_output_channel_count() {
        let patch = parse_channels_patch(&object(json!({
            "dac_map": [0, Value::Null, 1, Value::Null],
            "aes67_port": 5_006,
        })))
        .expect("patch");
        assert_eq!(patch.rt_fields.get("channels"), Some(&json!(4)));
        assert_eq!(patch.mapping, Some(vec![(0, 0), (1, 2)]));
        assert_eq!(patch.warnings.len(), 1);
        assert!(patch.warnings[0].contains("derived"));
    }

    #[test]
    fn patch_rejects_unknown_fields_conflicts_and_empty_patches() {
        for bad in [
            json!({ "sample_rte": 48_000 }),
            json!({ "output": { "channels": 4, "dca_map": [] } }),
            json!({ "channels": 4, "output_channels": 6 }),
            json!({ "channels": 4, "dac_map": [0, 1] }),
            json!({ "mapping": [{ "lane": 0, "dac": 0 }], "dac_map": [1] }),
            json!({ "mapping": [] }),
            json!({ "dac_map": [] }),
            json!({ "mapping": [{ "lane": 0, "dac": 0 }, { "lane": 0, "dac": 1 }] }),
            json!({ "aes67_group": 7 }),
            json!({ "aes67_port": -1 }),
        ] {
            assert!(
                parse_channels_patch(&object(bad.clone())).is_err(),
                "must reject {bad}"
            );
        }
        let error =
            parse_channels_patch(&object(json!({ "stage": "prepare" }))).expect_err("empty");
        assert_eq!(error.code, "invalid_request");
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
        let diff = planned_diff(None, &[(0, 1)]);
        assert_eq!(diff["added"], json!([{"lane": 0, "dac": 1}]));
        assert_eq!(diff["removed"], json!([]));
    }

    #[test]
    fn rt_host_business_errors_map_to_honest_http_statuses() {
        assert_eq!(host_link_status("bad_request"), StatusCode::BAD_REQUEST);
        assert_eq!(host_link_status("channels_busy"), StatusCode::CONFLICT);
        assert_eq!(host_link_status("no_channels_draft"), StatusCode::CONFLICT);
        assert_eq!(
            host_link_status("engine_token_invalid"),
            StatusCode::CONFLICT
        );
        assert_eq!(host_link_status("cal_busy"), StatusCode::CONFLICT);
        assert_eq!(
            host_link_status("device_unavailable"),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            host_link_status("device_absent"),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            host_link_status(HOST_LINK_UNAVAILABLE),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            host_link_status("ERR_RECONFIGURE_DEVICE"),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            host_link_status("ERR_RECONFIGURE_HW_VERIFY"),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(host_link_status("unsupported"), StatusCode::NOT_IMPLEMENTED);
        assert_eq!(host_link_status("unknown_op"), StatusCode::NOT_IMPLEMENTED);
        assert_eq!(
            host_link_status("something_else"),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn commit_failure_details_keep_the_rollback_truth() {
        let error = ChannelApiError::rt_host(
            "ERR_RECONFIGURE_DEVICE".to_string(),
            "open new output device failed".to_string(),
            json!({
                "ok": false,
                "error": "ERR_RECONFIGURE_DEVICE",
                "stage": "device_open",
                "rolled_back": true,
                "applied": false,
                "generation": 2,
                "gap_ms": 12,
            }),
            json!({ "phase": "channels_commit" }),
        );
        assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        assert_eq!(error.code, "ERR_RECONFIGURE_DEVICE");
        assert_eq!(error.details["rolled_back"], json!(true));
        assert_eq!(error.details["stage"], json!("device_open"));
        assert_eq!(error.details["phase"], json!("channels_commit"));
    }

    #[test]
    fn mapping_section_says_the_binding_is_the_plan_routing_not_channels_commit() {
        let patch = ChannelsPatch {
            mapping: Some(vec![(0, 1)]),
            mapping_source: "mapping",
            ..ChannelsPatch::default()
        };
        let section = mapping_section(&patch, Some(&[(0, 0)]), Some(true));
        assert_eq!(
            section["applied_by"],
            json!("plan routing section (plan prepare + commit)")
        );
        assert_eq!(section["applies_with_this_commit"], json!(false));
        assert_eq!(
            section["diff"]["changed"],
            json!([{"lane": 0, "from": 0, "to": 1}])
        );
        assert!(section["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("plan graph")));
    }
}
