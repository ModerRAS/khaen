//! `/api/status` 聚合（M2 / T35）：PLAN §4.4 的 `khaen-status/v1`（页面唯一数据源）。
//!
//! 数据来源全部真实，不编造：
//! - `daemon`：进程版本、`uptime_s`、启动时解析并注入 [`DaemonState`] 的监听地址
//!   （handler 不读环境变量）。
//! - rt-host 状态文件（`KHAEN_RT_STATUS_FILE`，默认 [`DEFAULT_RT_STATUS_FILE`]，启动时注入
//!   [`DaemonState`]；测试注入 tempdir）：`state/muted/safe/last_error/plan_rate/device_rate/`
//!   `rate_asserted/graph_id/device_state/blocks/input/output/receiver` 等。文件由
//!   `crates/rt-host/src/main.rs::write_status_file` 原子写出，schema [`RT_STATUS_SCHEMA`]。
//! - `format.block_frames` / `format.channels`：daemon 的 [`EngineConfig`]。
//! - `device.state`：优先 rt-host 的 `device_state`；rt-host 状态不可用或该字段缺失时
//!   退回 daemon `DeviceMonitor` 的生命周期状态（`absent` → absent，其它 → present）。
//!
//! 退化（**绝不 500**）：状态文件缺失/读不了/不是合法 JSON/不是 JSON 对象/schema 不认识/过期
//! （mtime 与 now 之差 > [`FRESH_MS`]）时 `host_link.connected=false`，rt-host 字段一律
//! `null` + `reasons`，daemon 自身字段照常返回；HTTP 状态码永远是 200。
//!
//! `reasons` 是“字段路径 → 原因”的映射（例如 `{"format.device_rate": "rt-host ..."}`）；
//! 未实现的能力进 `not_wired`（元素 `{path, reason}`，与 `reasons` 同形）。拿不到的数据一律
//! `null` + 原因，不编数值（PLAN §4.4 / AGENTS.md §5）。
//!
//! rt-host 状态文件里的 `output.blocks_written` 与 `reopen_attempts` 在 §4.4 没有
//! 对应字段，本模块不为了“用上数据”而加 schema 之外的字段；`output.blocks` 取文件顶层的
//! `blocks`（rt-host 已处理块数）。

use std::net::SocketAddr;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use khaen_host::EngineConfig;

use crate::{DaemonState, VERSION};

/// 聚合对象 schema 版本（PLAN §4.4）。
pub const SCHEMA: &str = "khaen-status/v1";
/// rt-host 状态文件的默认路径（生产由部署指向 `/var/lib/khaen/`）。
pub const DEFAULT_RT_STATUS_FILE: &str = "/var/lib/khaen/rt-host-status.json";
/// rt-host 状态文件 schema（与 `crates/rt-host/src/main.rs::write_status_file` 同源）。
pub const RT_STATUS_SCHEMA: &str = "khaen-rt-status/v1";
/// `host_link.connected` 的新鲜度上限：状态文件 mtime 与 now 之差超过它即视为过期（毫秒）。
pub const FRESH_MS: u64 = 2_000;
/// T70f 控制通道未实现（`host_link.socket` 与 `not_wired` 共用同一份说明）。
const SOCKET_REASON: &str = "T70f control channel is not implemented: rt-host has no UNIX socket yet, so the daemon can only read its status file and cannot query or command it";
/// 校准 job 后端未实现（M5）。
const CALIBRATION_REASON: &str = "calibration jobs are not wired (M5)";
/// rt-host 状态文件不写 output errors 的说明（RuntimeStatus 有该字段，但 write_status_file 未落盘）。
const OUT_ERRORS_REASON: &str = "rt-host status file (khaen-rt-status/v1) does not report output errors: write_status_file omits RuntimeStatus.output_errors";
/// AES67 接收参数没有数据源（rt-host 进程的启动输入，daemon 拿不到）。
const AES67_REASON: &str = "rt-host status file (khaen-rt-status/v1) does not report AES67 receive parameters; they are rt-host process inputs and there is no T70f channel to query them";
/// 编码没有数据源（daemon 不打开 ALSA，rt-host 状态文件也不写采样格式）。
const ENCODING_REASON: &str = "encoding is not reported by the daemon EngineConfig nor by the rt-host status file (no ALSA sample format source in this process)";
/// 设备标识不在 DaemonState 的既有配置里（KHAEN_DEVICE_ID 只用于构造监督器 probe）。
const DEVICE_ID_REASON: &str = "daemon does not expose a configured device id: KHAEN_DEVICE_ID is only consumed by the device supervisor probe";
/// 声卡名（hw:CARD=...,DEV=...）没有既有配置来源。
const DEVICE_CARD_REASON: &str = "the ALSA PCM name (hw:CARD=...,DEV=...) is not configured in the daemon and is not reported by the rt-host status file";

/// 聚合所需的已解析数据源（全部来自启动期配置或真实进程快照）。
struct StatusSources<'a> {
    /// 启动时解析的监听地址（不在 handler 里读环境变量）。
    bind: SocketAddr,
    /// 进程已运行整秒数。
    uptime_s: u64,
    /// daemon 引擎配置（`format.block_frames` / `format.channels` 的真实来源）。
    engine: &'a EngineConfig,
    /// daemon 设备监督器最近一次快照（`device.state` 的回退来源）。
    device: &'a Value,
    /// rt-host 状态文件路径（启动期注入；测试为 tempdir）。
    rt_status_file: &'a Path,
}

/// 从 daemon 状态构造 `khaen-status/v1` 聚合对象（`now` 由调用方给出）。
pub(crate) fn aggregate(state: &DaemonState, now: SystemTime) -> Value {
    let engine = state.engine_config();
    let device = state.device_json();
    aggregate_at(
        &StatusSources {
            bind: state.bind(),
            uptime_s: state.uptime_s(),
            engine: &engine,
            device: &device,
            rt_status_file: state.rt_status_file(),
        },
        now,
    )
}

/// 聚合主体：纯函数（文件读取 + 时间注入），便于用 fixture 覆盖全部退化路径。
fn aggregate_at(sources: &StatusSources<'_>, now: SystemTime) -> Value {
    let mut reasons = Map::new();
    let file = read_rt_status(sources.rt_status_file, now);

    // host_link：connected 以“文件可读、可解析、新鲜”为准；since/status_age_ms 只要有 mtime 就如实上报。
    if let Some(text) = file.unavailable_reason() {
        insert_reason(&mut reasons, "host_link.connected", text);
    }
    let since = match file.mtime() {
        Some(mtime) => Value::String(rfc3339_utc(mtime)),
        None => {
            let text = file
                .unavailable_reason()
                .unwrap_or("rt-host status file is not readable");
            insert_reason(&mut reasons, "host_link.since", text);
            Value::Null
        }
    };
    let status_age_ms = match file.age_ms() {
        Some(age_ms) => json!(age_ms),
        None => {
            let text = file
                .unavailable_reason()
                .unwrap_or("rt-host status file is not readable");
            insert_reason(&mut reasons, "host_link.status_age_ms", text);
            Value::Null
        }
    };

    // rt-host 字段：文件不可用 → 全 null + 同一条原因；文件可用 → 逐字段映射（缺哪个补哪个原因）。
    let rt = match &file {
        RtFile::Fresh { document, .. } => rt_fields_from_file(document, &mut reasons),
        RtFile::Missing { reason } | RtFile::Invalid { reason, .. } => {
            degraded_rt_fields(reason, &mut reasons)
        }
    };

    // 只读能力/数据源固定缺口：与 rt-host 在不在线无关。
    insert_reason(&mut reasons, "host_link.socket", SOCKET_REASON);
    insert_reason(&mut reasons, "format.encoding", ENCODING_REASON);
    for path in [
        "format.aes67.channels",
        "format.aes67.map",
        "format.aes67.pt",
        "format.aes67.group",
        "format.aes67.port",
    ] {
        insert_reason(&mut reasons, path, AES67_REASON);
    }
    insert_reason(&mut reasons, "output.out_errors", OUT_ERRORS_REASON);
    insert_reason(&mut reasons, "device.id", DEVICE_ID_REASON);
    insert_reason(&mut reasons, "device.card", DEVICE_CARD_REASON);
    for path in [
        "calibration.active_job",
        "calibration.last_job_id",
        "calibration.last_verdict",
    ] {
        insert_reason(&mut reasons, path, CALIBRATION_REASON);
    }
    // plan 的元数据只有 T70f 控制通道能拿到（状态文件只写 graph_id）。
    insert_reason(&mut reasons, "plan.sha256", plan_metadata_reason("sha256"));
    insert_reason(
        &mut reasons,
        "plan.version",
        plan_metadata_reason("version"),
    );
    insert_reason(
        &mut reasons,
        "plan.latency_samples",
        plan_metadata_reason("latency_samples"),
    );
    insert_reason(
        &mut reasons,
        "plan.applied_at",
        plan_metadata_reason("applied_at (RFC3339)"),
    );
    insert_reason(
        &mut reasons,
        "plan.source",
        plan_metadata_reason("source (upload|calibration)"),
    );

    // device.rate_readback 与 format.device_rate 同源；文件报了 null 时两个路径都要有原因。
    if rt.device_rate.is_null() {
        let text = reasons
            .get("format.device_rate")
            .and_then(Value::as_str)
            .unwrap_or("rt-host device rate is unavailable")
            .to_string();
        insert_reason(&mut reasons, "device.rate_readback", text);
    }

    let device_state = device_state_value(&file, sources.device, &mut reasons);
    let connected = matches!(file, RtFile::Fresh { .. });

    json!({
        "schema": SCHEMA,
        "daemon": {
            "version": VERSION,
            "uptime_s": sources.uptime_s,
            "bind": sources.bind.to_string(),
        },
        "host_link": {
            "connected": connected,
            "since": since,
            "socket": Value::Null,
            "status_age_ms": status_age_ms,
        },
        "state": {
            "value": rt.state_value,
            "muted": rt.state_muted,
            "safe": rt.state_safe,
            "last_error": rt.state_last_error,
        },
        "plan": {
            "graph_id": rt.graph_id,
            "sha256": Value::Null,
            "version": Value::Null,
            "latency_samples": Value::Null,
            "applied_at": Value::Null,
            "source": Value::Null,
        },
        "format": {
            "plan_rate": rt.plan_rate,
            "device_rate": rt.device_rate,
            "rate_asserted": rt.rate_asserted,
            "block_frames": sources.engine.block_frames as u64,
            "channels": sources.engine.channels as u64,
            "encoding": Value::Null,
            "aes67": {
                "channels": Value::Null,
                "map": Value::Null,
                "pt": Value::Null,
                "group": Value::Null,
                "port": Value::Null,
            },
        },
        "input": rt.input,
        "output": rt.output,
        "device": {
            "id": Value::Null,
            "card": Value::Null,
            "state": device_state,
            "rate_readback": rt.device_rate.clone(),
        },
        "calibration": {
            "active_job": Value::Null,
            "last_job_id": Value::Null,
            "last_verdict": Value::Null,
        },
        "not_wired": [
            { "path": "host_link.socket", "reason": SOCKET_REASON },
            { "path": "calibration.jobs", "reason": CALIBRATION_REASON },
        ],
        "reasons": reasons,
    })
}

/// rt-host 状态文件的判定结果。
enum RtFile {
    /// 文件不存在或 stat 失败（连 mtime 都拿不到）。
    Missing {
        /// 人看的稳定原因。
        reason: String,
    },
    /// 文件可读但不可用（损坏/非 JSON 对象/schema 不匹配/过期），或读取失败但有 mtime。
    Invalid {
        /// 文件 mtime。
        mtime: SystemTime,
        /// mtime 与 now 之差（毫秒；mtime 在未来时按 0）。
        age_ms: u64,
        /// 人看的稳定原因。
        reason: String,
    },
    /// 文件可读、schema 匹配且新鲜。
    Fresh {
        /// 文件 mtime。
        mtime: SystemTime,
        /// mtime 与 now 之差（毫秒）。
        age_ms: u64,
        /// 解析后的 JSON 对象。
        document: Map<String, Value>,
    },
}

impl RtFile {
    /// 文件 mtime（文件不存在时为 None）。
    fn mtime(&self) -> Option<SystemTime> {
        match self {
            Self::Missing { .. } => None,
            Self::Invalid { mtime, .. } | Self::Fresh { mtime, .. } => Some(*mtime),
        }
    }

    /// mtime 与 now 之差（毫秒；文件不存在时为 None）。
    fn age_ms(&self) -> Option<u64> {
        match self {
            Self::Missing { .. } => None,
            Self::Invalid { age_ms, .. } | Self::Fresh { age_ms, .. } => Some(*age_ms),
        }
    }

    /// 不可用原因（可用时为 None）。
    fn unavailable_reason(&self) -> Option<&str> {
        match self {
            Self::Missing { reason } | Self::Invalid { reason, .. } => Some(reason),
            Self::Fresh { .. } => None,
        }
    }
}

/// 读并校验 rt-host 状态文件；任何异常都只降级，不向调用方报错（handler 绝不 500）。
fn read_rt_status(path: &Path, now: SystemTime) -> RtFile {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            return RtFile::Missing {
                reason: format!(
                    "rt-host status file {} is not available: {error}",
                    path.display()
                ),
            }
        }
    };
    let mtime = match metadata.modified() {
        Ok(mtime) => mtime,
        Err(error) => {
            return RtFile::Missing {
                reason: format!(
                    "rt-host status file {} mtime is unreadable: {error}",
                    path.display()
                ),
            }
        }
    };
    let age_ms = age_ms(mtime, now);
    if age_ms > FRESH_MS {
        return RtFile::Invalid {
            mtime,
            age_ms,
            reason: format!(
                "rt-host status file {} is stale: status_age_ms={age_ms} > {FRESH_MS}",
                path.display()
            ),
        };
    }
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            return RtFile::Invalid {
                mtime,
                age_ms,
                reason: format!(
                    "rt-host status file {} is unreadable: {error}",
                    path.display()
                ),
            }
        }
    };
    let document: Value = match serde_json::from_str(&text) {
        Ok(document) => document,
        Err(error) => {
            return RtFile::Invalid {
                mtime,
                age_ms,
                reason: format!(
                    "rt-host status file {} is not valid JSON: {error}",
                    path.display()
                ),
            }
        }
    };
    let Some(document) = document.as_object() else {
        return RtFile::Invalid {
            mtime,
            age_ms,
            reason: format!(
                "rt-host status file {} is not a JSON object",
                path.display()
            ),
        };
    };
    if let Some(schema) = document.get("schema") {
        let schema = schema.as_str().unwrap_or("<not a string>");
        if schema != RT_STATUS_SCHEMA {
            return RtFile::Invalid {
                mtime,
                age_ms,
                reason: format!(
                    "rt-host status file {} has unsupported schema {schema:?} (expected {RT_STATUS_SCHEMA})",
                    path.display()
                ),
            };
        }
    }
    // 状态文件没有 schema 字段时按“宽容”处理：仍按字段名映射（原子写保证正常文件不会缺这个键）；
    // 具体缺哪些数据字段由 rt_fields_from_file 逐个登记原因。
    RtFile::Fresh {
        mtime,
        age_ms,
        document: document.clone(),
    }
}

/// mtime 与 now 之差（毫秒）；mtime 在未来（时钟回拨）时按 0，不伪造未来年龄。
fn age_ms(mtime: SystemTime, now: SystemTime) -> u64 {
    match now.duration_since(mtime) {
        Ok(age) => u64::try_from(age.as_millis()).unwrap_or(u64::MAX),
        Err(_) => 0,
    }
}

/// rt-host 字段的映射结果（全部是 JSON 值，`null` 表示不可用）。
struct RtFields {
    state_value: Value,
    state_muted: Value,
    state_safe: Value,
    state_last_error: Value,
    graph_id: Value,
    plan_rate: Value,
    device_rate: Value,
    rate_asserted: Value,
    input: Value,
    output: Value,
}

/// 文件不可用：rt-host 字段全 `null`，每个字段路径都登记同一条原因。
fn degraded_rt_fields(reason: &str, reasons: &mut Map<String, Value>) -> RtFields {
    for path in [
        "state.value",
        "state.muted",
        "state.safe",
        "state.last_error",
        "plan.graph_id",
        "format.plan_rate",
        "format.device_rate",
        "format.rate_asserted",
        "input.packets",
        "input.underruns",
        "input.jitter_fill",
        "input.ssrc",
        "input.queue_full_drops",
        "input.recv_errors",
        "output.blocks",
        "output.xruns",
        "output.bytes_written",
        "device.rate_readback",
    ] {
        insert_reason(reasons, path, reason);
    }
    RtFields {
        state_value: Value::Null,
        state_muted: Value::Null,
        state_safe: Value::Null,
        state_last_error: Value::Null,
        graph_id: Value::Null,
        plan_rate: Value::Null,
        device_rate: Value::Null,
        rate_asserted: Value::Null,
        input: json!({
            "packets": Value::Null,
            "underruns": Value::Null,
            "jitter_fill": Value::Null,
            "ssrc": Value::Null,
            "queue_full_drops": Value::Null,
            "recv_errors": Value::Null,
        }),
        output: json!({
            "blocks": Value::Null,
            "xruns": Value::Null,
            "out_errors": Value::Null,
            "bytes_written": Value::Null,
        }),
    }
}

/// 文件可用：逐字段映射；缺失/类型不对/被报成 null 的字段给 `null` + 原因。
fn rt_fields_from_file(
    document: &Map<String, Value>,
    reasons: &mut Map<String, Value>,
) -> RtFields {
    let state_value = required_string(document, "state", "state.value", reasons);
    let state_muted = required_bool(document, "muted", "state.muted", reasons);
    let state_safe = required_bool(document, "safe", "state.safe", reasons);
    // last_error 的 null 是“没有错误”这一真实状态，不是缺数据，因此不登记原因。
    let state_last_error =
        nullable_string(document, "last_error", "state.last_error", reasons, false);
    let graph_id = nullable_string(document, "graph_id", "plan.graph_id", reasons, true);
    let plan_rate = required_u64(document, "plan_rate", "format.plan_rate", reasons);
    let device_rate = nullable_u64(document, "device_rate", "format.device_rate", reasons);
    let rate_asserted = required_bool(document, "rate_asserted", "format.rate_asserted", reasons);
    let input = json!({
        "packets": required_u64(document, "input.packets", "input.packets", reasons),
        "underruns": required_u64(document, "input.underruns", "input.underruns", reasons),
        "jitter_fill": required_u64(document, "input.jitter_fill", "input.jitter_fill", reasons),
        "ssrc": nullable_u64(document, "input.ssrc", "input.ssrc", reasons),
        "queue_full_drops": required_u64(
            document,
            "receiver.queue_full_drops",
            "input.queue_full_drops",
            reasons,
        ),
        "recv_errors": required_u64(
            document,
            "receiver.recv_errors",
            "input.recv_errors",
            reasons,
        ),
    });
    let output = json!({
        "blocks": required_u64(document, "blocks", "output.blocks", reasons),
        "xruns": required_u64(document, "output.xruns", "output.xruns", reasons),
        "out_errors": Value::Null,
        "bytes_written": required_u64(
            document,
            "output.bytes_written",
            "output.bytes_written",
            reasons,
        ),
    });
    RtFields {
        state_value,
        state_muted,
        state_safe,
        state_last_error,
        graph_id,
        plan_rate,
        device_rate,
        rate_asserted,
        input,
        output,
    }
}

/// 按 `a.b` 路径取状态文件里的字段（只支持一层嵌套）。
fn lookup<'a>(document: &'a Map<String, Value>, source: &str) -> Option<&'a Value> {
    match source.split_once('.') {
        Some((outer, inner)) => document
            .get(outer)
            .and_then(Value::as_object)
            .and_then(|object| object.get(inner)),
        None => document.get(source),
    }
}

/// 必填非负整数：缺失/类型不对/null 都返回 `null` 并登记原因。
fn required_u64(
    document: &Map<String, Value>,
    source: &str,
    path: &str,
    reasons: &mut Map<String, Value>,
) -> Value {
    match lookup(document, source) {
        Some(Value::Number(number)) => match number.as_u64() {
            Some(value) => json!(value),
            None => {
                insert_reason(
                    reasons,
                    path,
                    format!("rt-host status file field `{source}` is not a non-negative integer"),
                );
                Value::Null
            }
        },
        Some(Value::Null) => {
            insert_reason(
                reasons,
                path,
                format!("rt-host status file reports `{source}` as null"),
            );
            Value::Null
        }
        Some(_) => {
            insert_reason(
                reasons,
                path,
                format!("rt-host status file field `{source}` is not a non-negative integer"),
            );
            Value::Null
        }
        None => {
            insert_reason(
                reasons,
                path,
                format!("rt-host status file does not provide field `{source}`"),
            );
            Value::Null
        }
    }
}

/// 可空非负整数（rt-host 用 null 表示“还没读到”）：null 也给原因。
fn nullable_u64(
    document: &Map<String, Value>,
    source: &str,
    path: &str,
    reasons: &mut Map<String, Value>,
) -> Value {
    required_u64(document, source, path, reasons)
}

/// 必填布尔：缺失/类型不对/null 都返回 `null` 并登记原因。
fn required_bool(
    document: &Map<String, Value>,
    source: &str,
    path: &str,
    reasons: &mut Map<String, Value>,
) -> Value {
    match lookup(document, source) {
        Some(Value::Bool(value)) => json!(value),
        Some(Value::Null) => {
            insert_reason(
                reasons,
                path,
                format!("rt-host status file reports `{source}` as null"),
            );
            Value::Null
        }
        Some(_) => {
            insert_reason(
                reasons,
                path,
                format!("rt-host status file field `{source}` is not a boolean"),
            );
            Value::Null
        }
        None => {
            insert_reason(
                reasons,
                path,
                format!("rt-host status file does not provide field `{source}`"),
            );
            Value::Null
        }
    }
}

/// 必填字符串：缺失/类型不对/null 都返回 `null` 并登记原因。
fn required_string(
    document: &Map<String, Value>,
    source: &str,
    path: &str,
    reasons: &mut Map<String, Value>,
) -> Value {
    nullable_string(document, source, path, reasons, true)
}

/// 可空字符串；`report_null` 决定显式 null 是否算“缺数据”而登记原因。
fn nullable_string(
    document: &Map<String, Value>,
    source: &str,
    path: &str,
    reasons: &mut Map<String, Value>,
    report_null: bool,
) -> Value {
    match lookup(document, source) {
        Some(Value::String(value)) => json!(value),
        Some(Value::Null) => {
            if report_null {
                insert_reason(
                    reasons,
                    path,
                    format!("rt-host status file reports `{source}` as null"),
                );
            }
            Value::Null
        }
        Some(_) => {
            insert_reason(
                reasons,
                path,
                format!("rt-host status file field `{source}` is not a string"),
            );
            Value::Null
        }
        None => {
            insert_reason(
                reasons,
                path,
                format!("rt-host status file does not provide field `{source}`"),
            );
            Value::Null
        }
    }
}

/// `device.state`：优先 rt-host `device_state`，退而用 daemon `DeviceMonitor`。
fn device_state_value(file: &RtFile, monitor: &Value, reasons: &mut Map<String, Value>) -> Value {
    if let RtFile::Fresh { document, .. } = file {
        match document.get("device_state").and_then(Value::as_str) {
            Some(state @ ("present" | "absent")) => return json!(state),
            Some(other) => {
                insert_reason(
                    reasons,
                    "device.state",
                    format!(
                        "rt-host status file reports unsupported device_state {other:?} (expected present|absent)"
                    ),
                );
            }
            None => {}
        }
    }
    if let Some(state) = monitor_device_state(monitor) {
        return json!(state);
    }
    let monitor_reason = monitor
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("no probe configured in this process");
    let reason = match file {
        RtFile::Fresh { .. } => format!(
            "rt-host status file does not provide device_state; daemon device monitor is not wired: {monitor_reason}"
        ),
        RtFile::Missing { reason } | RtFile::Invalid { reason, .. } => {
            format!("{reason}; daemon device monitor is not wired: {monitor_reason}")
        }
    };
    insert_reason(reasons, "device.state", reason);
    Value::Null
}

/// daemon `DeviceMonitor` 快照 → `present`/`absent`（未接线/未知状态 → None）。
fn monitor_device_state(monitor: &Value) -> Option<&'static str> {
    if monitor.get("wired").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    match monitor.get("state").and_then(Value::as_str) {
        Some("absent") => Some("absent"),
        Some(_) => Some("present"),
        None => None,
    }
}

/// 登记“字段路径 → 原因”；同路径只保留第一个原因。
fn insert_reason(reasons: &mut Map<String, Value>, path: &str, reason: impl Into<String>) {
    reasons
        .entry(path.to_string())
        .or_insert_with(|| Value::String(reason.into()));
}

/// plan 元数据（sha256/version/...）拿不到的统一原因（状态文件只写 graph_id）。
fn plan_metadata_reason(field: &str) -> String {
    format!(
        "rt-host status file ({RT_STATUS_SCHEMA}) does not report the applied plan {field}; that needs the T70f control channel (not implemented)"
    )
}

/// `SystemTime` → RFC3339 UTC 字符串（秒精度，`Z` 结尾；不引入时间库）。
fn rfc3339_utc(time: SystemTime) -> String {
    let seconds = match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        Err(error) => -i64::try_from(error.duration().as_secs()).unwrap_or(i64::MAX),
    };
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// 自 1970-01-01 起的天数 → (年, 月, 日)；Howard Hinnant 的 civil_from_days 算法（无闰年特判）。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = u32::try_from(day_of_year - (153 * month_prime + 2) / 5 + 1).unwrap_or(1);
    let month_prime_value = u32::try_from(month_prime).unwrap_or(0);
    let month = if month_prime_value < 10 {
        month_prime_value + 3
    } else {
        month_prime_value - 9
    };
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use super::*;

    /// rt-host 状态不可用时，所有 rt-host 字段路径（测试断言用）。
    const RT_HOST_PATHS: &[&str] = &[
        "state.value",
        "state.muted",
        "state.safe",
        "state.last_error",
        "plan.graph_id",
        "format.plan_rate",
        "format.device_rate",
        "format.rate_asserted",
        "input.packets",
        "input.underruns",
        "input.jitter_fill",
        "input.ssrc",
        "input.queue_full_drops",
        "input.recv_errors",
        "output.blocks",
        "output.xruns",
        "output.bytes_written",
        "device.rate_readback",
    ];

    /// 测试用临时目录（Drop 时递归删除）。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let mut path = std::env::temp_dir();
            path.push(format!(
                "khaen-daemon-status-{}-{tag}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }

        fn file(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn test_bind() -> SocketAddr {
        "0.0.0.0:80".parse().expect("test bind address")
    }

    fn test_engine() -> EngineConfig {
        EngineConfig {
            block_frames: 128,
            sample_rate: 48_000,
            channels: 22,
        }
    }

    fn monitor_not_wired() -> Value {
        json!({ "wired": false, "reason": "no probe configured in this process" })
    }

    /// 一份字段齐全的 rt-host 状态文件内容（值故意与默认值不同，防止“碰巧相等”）。
    fn full_document() -> Value {
        json!({
            "schema": RT_STATUS_SCHEMA,
            "blocks": 1234,
            "state": "running",
            "muted": false,
            "safe": false,
            "plan_rate": 48_000,
            "device_rate": 48_000,
            "rate_asserted": true,
            "input": {
                "packets": 111,
                "underruns": 2,
                "jitter_fill": 192,
                "ssrc": 534_361_700u64,
            },
            "output": {
                "xruns": 3,
                "blocks_written": 1200,
                "bytes_written": 10_137_600u64,
            },
            "receiver": {
                "packets_received": 115,
                "queue_full_drops": 1,
                "recv_errors": 0,
            },
            "graph_id": "8bccdeadbeef",
            "last_error": null,
            "device_state": "present",
            "absent_since": null,
            "reopen_attempts": 0,
            "last_device_error": null,
        })
    }

    fn write_document(path: &Path, document: &Value) {
        std::fs::write(path, document.to_string()).expect("write rt-host status fixture");
    }

    fn aggregate_with(path: &Path, now: SystemTime) -> Value {
        aggregate_with_monitor(path, &monitor_not_wired(), now)
    }

    fn aggregate_with_monitor(path: &Path, monitor: &Value, now: SystemTime) -> Value {
        let engine = test_engine();
        aggregate_at(
            &StatusSources {
                bind: test_bind(),
                uptime_s: 7,
                engine: &engine,
                device: monitor,
                rt_status_file: path,
            },
            now,
        )
    }

    /// 按 `a.b` 路径取聚合对象里的值（测试断言用）。
    fn at<'a>(body: &'a Value, path: &str) -> &'a Value {
        let mut current = body;
        for part in path.split('.') {
            current = &current[part];
        }
        current
    }

    fn reason_of<'a>(body: &'a Value, path: &str) -> &'a str {
        body["reasons"][path]
            .as_str()
            .unwrap_or_else(|| panic!("reasons[{path}] must be a string: {}", body["reasons"]))
    }

    fn top_level_keys(body: &Value) -> Vec<String> {
        let mut keys: Vec<String> = body
            .as_object()
            .expect("aggregate body must be an object")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    }

    /// 断言 rt-host 字段全部为 null 且每个路径都有原因。
    fn assert_rt_host_absent(body: &Value) {
        for path in RT_HOST_PATHS {
            assert_eq!(at(body, path), &Value::Null, "{path} 应为 null");
            assert!(
                !reason_of(body, path).is_empty(),
                "{path} 缺原因: {}",
                body["reasons"]
            );
        }
    }

    /// daemon 自身字段在退化时照常真实（版本/uptime/bind/引擎格式）。
    fn assert_daemon_fields(body: &Value) {
        assert_eq!(body["schema"], json!(SCHEMA));
        assert_eq!(body["daemon"]["version"], json!(VERSION));
        assert_eq!(body["daemon"]["uptime_s"], json!(7));
        assert_eq!(body["daemon"]["bind"], json!("0.0.0.0:80"));
        assert_eq!(at(body, "format.block_frames"), &json!(128));
        assert_eq!(at(body, "format.channels"), &json!(22));
    }

    #[test]
    fn rfc3339_utc_matches_known_instants() {
        assert_eq!(rfc3339_utc(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            rfc3339_utc(UNIX_EPOCH + Duration::from_secs(1_000_000_000)),
            "2001-09-09T01:46:40Z"
        );
        assert_eq!(
            rfc3339_utc(UNIX_EPOCH + Duration::from_secs(1_709_164_800)),
            "2024-02-29T00:00:00Z"
        );
        assert_eq!(
            rfc3339_utc(UNIX_EPOCH - Duration::from_secs(86_400)),
            "1969-12-31T00:00:00Z"
        );
    }

    #[test]
    fn present_status_file_is_aggregated_into_the_v1_schema() {
        let dir = TempDir::new("present");
        let path = dir.file("rt-host-status.json");
        write_document(&path, &full_document());
        let body = aggregate_with(&path, SystemTime::now());

        // 顶层字段集严格照 PLAN §4.4，不多不少。
        let expected: Vec<String> = [
            "calibration",
            "daemon",
            "device",
            "format",
            "host_link",
            "input",
            "not_wired",
            "output",
            "plan",
            "reasons",
            "schema",
            "state",
        ]
        .iter()
        .map(|key| (*key).to_string())
        .collect();
        assert_eq!(top_level_keys(&body), expected);

        assert_daemon_fields(&body);

        assert_eq!(body["host_link"]["connected"], json!(true));
        assert_eq!(body["host_link"]["socket"], Value::Null);
        let age = body["host_link"]["status_age_ms"]
            .as_u64()
            .expect("status_age_ms 必须是整数");
        assert!(age <= FRESH_MS, "刚写的状态文件不该过期: age={age}");
        let since = body["host_link"]["since"]
            .as_str()
            .expect("since 必须是 RFC3339 字符串");
        assert_eq!(since.len(), 20, "since 必须是秒精度 RFC3339: {since}");
        assert!(since.ends_with('Z'), "since 必须是 UTC: {since}");
        assert!(body["reasons"].get("host_link.connected").is_none());

        assert_eq!(body["state"]["value"], json!("running"));
        assert_eq!(body["state"]["muted"], json!(false));
        assert_eq!(body["state"]["safe"], json!(false));
        assert_eq!(body["state"]["last_error"], Value::Null);
        assert!(
            body["reasons"].get("state.last_error").is_none(),
            "last_error 的 null 是“没有错误”的真实状态，不该登记原因"
        );

        assert_eq!(at(&body, "plan.graph_id"), &json!("8bccdeadbeef"));
        for path in [
            "plan.sha256",
            "plan.version",
            "plan.latency_samples",
            "plan.applied_at",
            "plan.source",
        ] {
            assert_eq!(at(&body, path), &Value::Null, "{path}");
            assert!(!reason_of(&body, path).is_empty(), "{path} 缺原因");
        }

        assert_eq!(at(&body, "format.plan_rate"), &json!(48_000));
        assert_eq!(at(&body, "format.device_rate"), &json!(48_000));
        assert_eq!(at(&body, "format.rate_asserted"), &json!(true));
        assert_eq!(at(&body, "format.encoding"), &Value::Null);
        assert_eq!(reason_of(&body, "format.encoding"), ENCODING_REASON);
        for path in [
            "format.aes67.channels",
            "format.aes67.map",
            "format.aes67.pt",
            "format.aes67.group",
            "format.aes67.port",
        ] {
            assert_eq!(at(&body, path), &Value::Null, "{path}");
            assert_eq!(reason_of(&body, path), AES67_REASON, "{path}");
        }

        assert_eq!(at(&body, "input.packets"), &json!(111));
        assert_eq!(at(&body, "input.underruns"), &json!(2));
        assert_eq!(at(&body, "input.jitter_fill"), &json!(192));
        assert_eq!(at(&body, "input.ssrc"), &json!(534_361_700u64));
        assert_eq!(at(&body, "input.queue_full_drops"), &json!(1));
        assert_eq!(at(&body, "input.recv_errors"), &json!(0));

        assert_eq!(at(&body, "output.blocks"), &json!(1234));
        assert_eq!(at(&body, "output.xruns"), &json!(3));
        assert_eq!(at(&body, "output.bytes_written"), &json!(10_137_600u64));
        assert_eq!(at(&body, "output.out_errors"), &Value::Null);
        assert_eq!(reason_of(&body, "output.out_errors"), OUT_ERRORS_REASON);

        assert_eq!(at(&body, "device.state"), &json!("present"));
        assert_eq!(at(&body, "device.rate_readback"), &json!(48_000));
        assert_eq!(at(&body, "device.id"), &Value::Null);
        assert_eq!(at(&body, "device.card"), &Value::Null);
        assert!(!reason_of(&body, "device.id").is_empty());
        assert!(!reason_of(&body, "device.card").is_empty());

        for path in [
            "calibration.active_job",
            "calibration.last_job_id",
            "calibration.last_verdict",
        ] {
            assert_eq!(at(&body, path), &Value::Null, "{path}");
            assert_eq!(reason_of(&body, path), CALIBRATION_REASON, "{path}");
        }

        let not_wired = body["not_wired"].as_array().expect("not_wired 必须是数组");
        assert!(not_wired
            .iter()
            .any(|entry| entry["path"] == json!("host_link.socket")));
        assert!(not_wired
            .iter()
            .any(|entry| entry["path"] == json!("calibration.jobs")));
        for entry in not_wired {
            assert_eq!(
                entry.as_object().map(|object| object.len()),
                Some(2),
                "not_wired 元素必须恰好是 path + reason: {entry}"
            );
            assert!(entry["reason"]
                .as_str()
                .is_some_and(|text| !text.is_empty()));
        }
        assert_eq!(reason_of(&body, "host_link.socket"), SOCKET_REASON);
        assert!(body["reasons"]
            .as_object()
            .expect("reasons 必须是对象")
            .values()
            .all(Value::is_string));
    }

    #[test]
    fn absent_status_file_degrades_but_keeps_daemon_fields() {
        let dir = TempDir::new("absent");
        let path = dir.file("missing.json");
        let body = aggregate_with(&path, SystemTime::now());

        assert_daemon_fields(&body);
        assert_eq!(body["host_link"]["connected"], json!(false));
        assert_eq!(body["host_link"]["since"], Value::Null);
        assert_eq!(body["host_link"]["status_age_ms"], Value::Null);
        assert!(!reason_of(&body, "host_link.connected").is_empty());
        assert!(!reason_of(&body, "host_link.since").is_empty());
        assert!(!reason_of(&body, "host_link.status_age_ms").is_empty());
        assert_rt_host_absent(&body);
        assert_eq!(at(&body, "device.state"), &Value::Null);
        assert!(!reason_of(&body, "device.state").is_empty());
    }

    #[test]
    fn corrupt_status_file_degrades_without_losing_age_and_since() {
        let dir = TempDir::new("corrupt");
        let path = dir.file("rt-host-status.json");
        for text in [
            "{ not json",
            "[]",
            "\"just a string\"",
            "{\"schema\":\"khaen-rt-status/v1\",",
        ] {
            std::fs::write(&path, text).expect("write corrupt fixture");
            let body = aggregate_with(&path, SystemTime::now());
            assert_daemon_fields(&body);
            assert_eq!(body["host_link"]["connected"], json!(false), "{text}");
            assert!(
                body["host_link"]["since"].is_string(),
                "文件存在时 mtime 仍然真实: {text}"
            );
            assert!(
                body["host_link"]["status_age_ms"].is_u64(),
                "文件存在时 age 仍然真实: {text}"
            );
            assert!(!reason_of(&body, "host_link.connected").is_empty());
            assert_rt_host_absent(&body);
        }
        // 明确区分损坏原因：合法 JSON 但不是对象。
        std::fs::write(&path, "[]").expect("write array fixture");
        let body = aggregate_with(&path, SystemTime::now());
        assert!(reason_of(&body, "host_link.connected").contains("not a JSON object"));
        // 明确区分损坏原因：损坏的 JSON 文本。
        std::fs::write(&path, "{ not json").expect("write broken fixture");
        let body = aggregate_with(&path, SystemTime::now());
        assert!(reason_of(&body, "host_link.connected").contains("not valid JSON"));
    }

    #[test]
    fn unsupported_schema_degrades_all_rt_host_fields() {
        let dir = TempDir::new("schema");
        let path = dir.file("rt-host-status.json");
        write_document(
            &path,
            &json!({ "schema": "khaen-rt-status/v2", "blocks": 5, "state": "running" }),
        );
        let body = aggregate_with(&path, SystemTime::now());
        assert_eq!(body["host_link"]["connected"], json!(false));
        assert!(reason_of(&body, "host_link.connected").contains("unsupported schema"));
        assert_rt_host_absent(&body);
        assert_eq!(at(&body, "output.blocks"), &Value::Null);
    }

    #[test]
    fn stale_status_file_degrades_but_reports_real_age() {
        let dir = TempDir::new("stale");
        let path = dir.file("rt-host-status.json");
        write_document(&path, &full_document());
        let mtime = std::fs::metadata(&path)
            .expect("metadata")
            .modified()
            .expect("mtime");
        let body = aggregate_with(&path, mtime + Duration::from_secs(5));

        assert_daemon_fields(&body);
        assert_eq!(body["host_link"]["connected"], json!(false));
        let age = body["host_link"]["status_age_ms"]
            .as_u64()
            .expect("age 必须是整数");
        assert!((5_000..6_000).contains(&age), "age={age}");
        assert!(reason_of(&body, "host_link.connected").contains("stale"));
        assert!(body["host_link"]["since"].is_string());
        assert_rt_host_absent(&body);
        assert_eq!(at(&body, "device.state"), &Value::Null);
    }

    #[test]
    fn freshness_boundary_is_two_seconds() {
        let dir = TempDir::new("boundary");
        let path = dir.file("rt-host-status.json");
        write_document(&path, &full_document());
        let mtime = std::fs::metadata(&path)
            .expect("metadata")
            .modified()
            .expect("mtime");

        let fresh = aggregate_with(&path, mtime + Duration::from_millis(FRESH_MS));
        assert_eq!(fresh["host_link"]["connected"], json!(true));
        assert_eq!(
            fresh["host_link"]["status_age_ms"],
            json!(FRESH_MS),
            "2000ms 仍在阈值内"
        );

        let stale = aggregate_with(&path, mtime + Duration::from_millis(FRESH_MS + 1));
        assert_eq!(stale["host_link"]["connected"], json!(false));
        assert_eq!(stale["host_link"]["status_age_ms"], json!(FRESH_MS + 1));
    }

    #[test]
    fn missing_fields_get_null_and_reasons() {
        let dir = TempDir::new("partial");
        let path = dir.file("rt-host-status.json");
        write_document(
            &path,
            &json!({
                "schema": RT_STATUS_SCHEMA,
                "blocks": 9,
                "state": "running",
                "device_state": "absent",
            }),
        );
        let body = aggregate_with(&path, SystemTime::now());

        // 文件新鲜：host_link 已连接。
        assert_eq!(body["host_link"]["connected"], json!(true));
        // 文件里有的字段照实映射。
        assert_eq!(at(&body, "output.blocks"), &json!(9));
        assert_eq!(body["state"]["value"], json!("running"));
        assert_eq!(at(&body, "device.state"), &json!("absent"));
        // 文件里没有的字段：null + 逐路径原因。
        for path in [
            "state.muted",
            "state.safe",
            "format.plan_rate",
            "format.device_rate",
            "format.rate_asserted",
            "plan.graph_id",
            "input.packets",
            "input.queue_full_drops",
            "output.xruns",
            "output.bytes_written",
            "device.rate_readback",
        ] {
            assert_eq!(at(&body, path), &Value::Null, "{path}");
            assert!(
                !reason_of(&body, path).is_empty(),
                "{path} 缺原因: {}",
                body["reasons"]
            );
        }
    }

    #[test]
    fn explicitly_null_fields_are_reported_with_reasons_except_last_error() {
        let dir = TempDir::new("nulls");
        let path = dir.file("rt-host-status.json");
        let mut document = full_document();
        document["graph_id"] = Value::Null;
        document["device_rate"] = Value::Null;
        document["input"]["ssrc"] = Value::Null;
        write_document(&path, &document);
        let body = aggregate_with(&path, SystemTime::now());

        assert_eq!(body["host_link"]["connected"], json!(true));
        for path in [
            "plan.graph_id",
            "format.device_rate",
            "device.rate_readback",
            "input.ssrc",
        ] {
            assert_eq!(at(&body, path), &Value::Null, "{path}");
            assert!(
                reason_of(&body, path).contains("null"),
                "{path} 的原因应说明文件报了 null: {}",
                reason_of(&body, path)
            );
        }
        // 未用的 output.blocks_written / reopen_attempts 不进 schema（不编造额外字段）。
        assert!(body["output"].get("blocks_written").is_none());
        assert!(body.get("reopen_attempts").is_none());
    }

    #[test]
    fn device_state_falls_back_to_the_daemon_monitor() {
        let dir = TempDir::new("device");
        let missing = dir.file("missing.json");
        let wired_streaming = json!({
            "wired": true,
            "state": "streaming",
            "plan_rate": 48_000,
            "device_rate": 48_000,
            "rate_asserted": true,
        });
        let wired_absent = json!({
            "wired": true,
            "state": "absent",
            "plan_rate": 48_000,
            "device_rate": null,
            "rate_asserted": false,
        });

        let streaming = aggregate_with_monitor(&missing, &wired_streaming, SystemTime::now());
        assert_eq!(at(&streaming, "device.state"), &json!("present"));
        assert_eq!(at(&streaming, "device.rate_readback"), &Value::Null);

        let absent = aggregate_with_monitor(&missing, &wired_absent, SystemTime::now());
        assert_eq!(at(&absent, "device.state"), &json!("absent"));
        // rate_readback 只用 rt-host 的 device_rate（监督器快照不是回读来源），缺就给 null + 原因。
        assert_eq!(at(&absent, "device.rate_readback"), &Value::Null);
        assert!(!reason_of(&absent, "device.rate_readback").is_empty());

        // rt-host 状态新鲜时以它为准（数据面权威），不被 daemon 监督器覆盖。
        let path = dir.file("rt-host-status.json");
        write_document(&path, &full_document());
        let fresh = aggregate_with_monitor(&path, &wired_absent, SystemTime::now());
        assert_eq!(at(&fresh, "device.state"), &json!("present"));
    }
}
