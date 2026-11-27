//! `/api/status` 聚合（M2 / T35 + T70f-b）：PLAN §4.4 的 `khaen-status/v1`（页面唯一数据源）。
//!
//! 数据来源全部真实，不编造（优先级从高到低）：
//! - **rt-host 控制 socket status**（`host_link` 模块；`KHAEN_RT_SOCKET`，默认
//!   `/run/khaen/rt-host.sock`）：rt-host 自己报的 `age_ms/runtime_state/mute/safe_mode/`
//!   `graph_id/actual_version/plan_rate/device_rate/rate_asserted/format/device/counters`。
//!   `host_link.mode="socket"`。
//! - rt-host 状态文件（`KHAEN_RT_STATUS_FILE`，默认 [`DEFAULT_RT_STATUS_FILE`]，启动时注入
//!   [`DaemonState`]；测试注入 tempdir）：socket 不可用时的退回路径，文件名/字段由
//!   `crates/rt-host/src/main.rs::write_status_file` 原子写出，schema [`RT_STATUS_SCHEMA`]。
//!   `host_link.mode="file"`。
//! - 两者都没有：rt-host 字段全 `null` + 原因，`host_link.mode="none"`、`connected=false`。
//! - `daemon`：进程版本、`uptime_s`、启动时解析并注入 [`DaemonState`] 的监听地址
//!   （handler 不读环境变量）。
//! - `format.block_frames` / `format.channels`：socket 来源用 rt-host 自报的数据面格式
//!   （`format.block_size/channels`）；文件/无来源时退回 daemon 的 [`EngineConfig`]。
//! - `device.state`：优先 rt-host 的 `device_state`（socket status 或状态文件）；不可用时
//!   退回 daemon `DeviceMonitor` 的生命周期状态（`absent` → absent，其它 → present）。
//! - `calibration.active_job` / `last_job_id` / `last_verdict`：daemon 注入的校准服务状态
//!   （`CalibrationHub::status`：服务只读状态 + 进程内 JobSink 事件缓存合并）。后端未接线
//!   时三字段 null + 原因，`not_wired` 里才出现 `calibration.jobs` 条目。
//!
//! 退化（**绝不 500**）：socket 调用失败/超时、状态文件缺失/读不了/不是合法 JSON/不是 JSON
//! 对象/schema 不认识/过期（mtime 与 now 之差 > [`FRESH_MS`]）时逐级降级，
//! `host_link.connected` 如实为 false 时 rt-host 字段一律 `null` + `reasons`，daemon 自身
//! 字段照常返回；HTTP 状态码永远是 200。`host_link.mode`（socket/file/none）与
//! `reasons`/`not_wired` 随实况变化，不再写死“T70f 未实现”。
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

use crate::calibration::service::CalibrationStatus;
use crate::host_link::{LinkSnapshot, Target, HOST_LINK_UNAVAILABLE};
use crate::speaker_test;
use crate::{DaemonState, VERSION};

/// 聚合对象 schema 版本（PLAN §4.4）。
pub const SCHEMA: &str = "khaen-status/v1";
/// rt-host 状态文件的默认路径（生产由部署指向 `/var/lib/khaen/`）。
pub const DEFAULT_RT_STATUS_FILE: &str = "/var/lib/khaen/rt-host-status.json";
/// rt-host 状态文件 schema（与 `crates/rt-host/src/main.rs::write_status_file` 同源）。
pub const RT_STATUS_SCHEMA: &str = "khaen-rt-status/v1";
/// `host_link.connected` 的新鲜度上限：状态文件 mtime 与 now 之差超过它即视为过期（毫秒）。
pub const FRESH_MS: u64 = 2_000;
/// process 目标（`KHAEN_RT_TARGET=process`）：控制 op 作用于 daemon 进程内引擎，不访问 rt-host。
const PROCESS_TARGET_REASON: &str = "KHAEN_RT_TARGET=process: control ops act on the daemon process-internal engine (test/offline mode); the rt-host data plane is NOT commanded";
/// socket 来源不提供启动时刻（status 只报快照年龄）。
const SOCKET_SINCE_REASON: &str = "the rt-host control status reports a snapshot age (age_ms) but not a process start time; host_link.since is only derived from the status-file mtime";
/// 校准服务未接线时的兜底原因（正常由 `CalibrationStatus::reason` 覆盖）。
const CALIBRATION_REASON: &str = "calibration jobs are not wired";
/// 状态文件不写 output errors 的说明（RuntimeStatus 有该字段，但 write_status_file 未落盘）。
const OUT_ERRORS_FILE_REASON: &str = "rt-host status file (khaen-rt-status/v1) does not report output errors: write_status_file omits RuntimeStatus.output_errors";
/// socket status 也没有 output errors 时的原因（正常不会出现：rt-host status 有该字段）。
const OUT_ERRORS_SOCKET_REASON: &str = "rt-host control status does not report output_errors";
/// rt-host 控制 status 没有顶层 mode（旧版本/协议违规）时的原因。
const RT_HOST_MODE_MISSING_REASON: &str = "rt-host control status does not report a string top-level mode: the running runtime mode cannot be read";
/// 状态文件 schema 不落运行时 mode 的原因（mode 只由控制 status op 报告）。
const RT_STATUS_FILE_MODE_REASON: &str = "rt-host status file (khaen-rt-status/v1) does not report the runtime mode (mode comes from the control status op only); no value is fabricated";
/// AES67 接收参数没有数据源（rt-host 进程的启动输入；控制通道 status 也不报它们）。
const AES67_REASON: &str = "AES67 receive parameters are rt-host process inputs: neither the rt-host control status (op status) nor the status file reports them";
/// 编码没有数据源（daemon 不打开 ALSA，rt-host status/文件都不写采样格式）。
const ENCODING_REASON: &str = "encoding is not reported by the daemon EngineConfig nor by the rt-host control status (no ALSA sample format source in this process)";
/// 设备标识不在 DaemonState 的既有配置里（KHAEN_DEVICE_ID 只用于构造监督器 probe）。
const DEVICE_ID_REASON: &str = "daemon does not expose a configured device id: KHAEN_DEVICE_ID is only consumed by the device supervisor probe";
/// 声卡名（hw:CARD=...,DEV=...）没有既有配置来源。
const DEVICE_CARD_REASON: &str = "the ALSA PCM name (hw:CARD=...,DEV=...) is not configured in the daemon and is not reported by the rt-host control status";

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
    /// 校准服务状态（`/api/status` 的 calibration 三字段来源；测试注入）。
    calibration: &'a CalibrationStatus,
    /// 控制目标（rt-host / process）：决定 host_link.target 与原因文案。
    target: Target,
    /// 控制通道快照（socket status / socket 不可用 / process）；每次聚合现取。
    link: &'a LinkSnapshot,
}

/// 从 daemon 状态构造 `khaen-status/v1` 聚合对象（`now` 由调用方给出）。
pub(crate) fn aggregate(state: &DaemonState, now: SystemTime) -> Value {
    let engine = state.engine_config();
    let device = state.device_json();
    let calibration = state.calibration_status();
    // socket status 往返（含至多一次重试）在这里发生；同步函数，调用方（handler）放到 blocking 线程池。
    let link = state.host_link().status_snapshot();
    aggregate_at(
        &StatusSources {
            bind: state.bind(),
            uptime_s: state.uptime_s(),
            engine: &engine,
            device: &device,
            rt_status_file: state.rt_status_file(),
            calibration: &calibration,
            target: state.host_link().target(),
            link: &link,
        },
        now,
    )
}

/// rt-host 数据的实际来源（socket 优先，其次新鲜状态文件，最后 none）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// 控制 socket 的 status op。
    Socket,
    /// 退回 rt-host 状态文件（新鲜且 schema 可识别）。
    File,
    /// 两者都不可用。
    None,
}

impl Source {
    /// `host_link.mode` 的稳定字符串。
    fn mode(self) -> &'static str {
        match self {
            Self::Socket => "socket",
            Self::File => "file",
            Self::None => "none",
        }
    }

    /// 是否拿到了 rt-host 自己的数据。
    fn connected(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// 聚合主体：纯函数（socket 快照/文件读取 + 时间注入），便于用 fixture 覆盖全部退化路径。
///
/// socket 快照由调用方（[`aggregate`]）现取并注入，本函数不直接碰 socket，
/// 所以三种来源（socket/file/none）与它们的降级语义都能在 Windows 上单测。
fn aggregate_at(sources: &StatusSources<'_>, now: SystemTime) -> Value {
    let mut reasons = Map::new();

    // 1) 数据来源：socket 是主路径；只有 socket 不可用才退回状态文件；文件也不可用则 none。
    let file = if sources.link.is_socket() {
        None
    } else {
        Some(read_rt_status(sources.rt_status_file, now))
    };
    let source = if sources.link.is_socket() {
        Source::Socket
    } else if matches!(file, Some(RtFile::Fresh { .. })) {
        Source::File
    } else {
        Source::None
    };
    let mode = source.mode();
    let connected = source.connected();

    // 2) host_link：connected 以“拿到 rt-host 自己的数据”为准，mode 说明来源。
    let socket = sources
        .link
        .socket()
        .map(|path| Value::String(path.display().to_string()));
    let file_since = file.as_ref().and_then(RtFile::mtime).map(rfc3339_utc);
    let file_age = file.as_ref().and_then(RtFile::age_ms);
    let file_reason = file
        .as_ref()
        .and_then(RtFile::unavailable_reason)
        .map(str::to_string);
    let since = match source {
        Source::Socket => {
            // socket status 只报快照年龄，没有进程启动时刻：如实给 null + 原因。
            insert_reason(&mut reasons, "host_link.since", SOCKET_SINCE_REASON);
            Value::Null
        }
        _ => match file_since {
            Some(since) => Value::String(since),
            None => {
                insert_reason(
                    &mut reasons,
                    "host_link.since",
                    file_reason
                        .clone()
                        .unwrap_or_else(|| "rt-host status file is not readable".to_string()),
                );
                Value::Null
            }
        },
    };
    let age = match source {
        Source::Socket => match sources.link {
            LinkSnapshot::Socket { age_ms, .. } => *age_ms,
            _ => None,
        },
        _ => file_age,
    };
    let age = match age {
        Some(age) => json!(age),
        None => {
            let reason = match source {
                // rt-host 在块线程发布首个快照之前 age_ms 为 null（runtime_state=starting）。
                Source::Socket => "the rt-host control status does not report age_ms yet: the block loop has not published its first runtime snapshot".to_string(),
                _ => file_reason
                    .clone()
                    .unwrap_or_else(|| "rt-host status file is not readable".to_string()),
            };
            // age_ms 是 T70f-b 的新字段名；status_age_ms 保留 v1 兼容，两个路径都要能读到原因。
            insert_reason(&mut reasons, "host_link.age_ms", reason.clone());
            insert_reason(&mut reasons, "host_link.status_age_ms", reason);
            Value::Null
        }
    };

    // socket 为什么不可用 / process 目标为什么不用 socket：只对确实拿不到的字段登记原因。
    if !sources.link.is_socket() {
        match (sources.target, sources.link) {
            (_, LinkSnapshot::Unavailable { reason, .. }) => {
                let text = format!(
                    "rt-host control socket is unavailable ({reason}); the daemon fell back to the rt-host status file only when it is fresh (host_link.mode={mode})"
                );
                insert_reason(&mut reasons, "host_link.socket", text);
            }
            (Target::Process, _) => {
                insert_reason(&mut reasons, "host_link.socket", PROCESS_TARGET_REASON);
            }
            // target=rt-host 时 snapshot 只可能是 Socket/Unavailable（这里保留兜底文案，不会 panic）。
            (Target::RtHost, _) => {
                insert_reason(&mut reasons, "host_link.socket", PROCESS_TARGET_REASON);
            }
        }
    }
    if !connected {
        // 一条原因同时说清“socket 为什么不可用”和“状态文件为什么也不能兜底”，
        // 不丢信息（文件损坏/过期/schema 不支持的具体原因必须能读到）。
        let socket_part = match sources.link {
            LinkSnapshot::Unavailable { reason, .. } => {
                Some(format!("rt-host control socket is unavailable ({reason})"))
            }
            LinkSnapshot::Process => Some(PROCESS_TARGET_REASON.to_string()),
            LinkSnapshot::Socket { .. } => None,
        };
        let file_part = file_reason
            .clone()
            .unwrap_or_else(|| "no rt-host status file is available".to_string());
        let text = match socket_part {
            Some(socket_part) => {
                format!("{socket_part}; no usable rt-host status file fallback: {file_part}")
            }
            None => file_part,
        };
        insert_reason(&mut reasons, "host_link.connected", text);
    }

    // 3) rt-host 字段：socket status > 状态文件 > 全 null + 原因。
    let rt = match (source, sources.link) {
        (Source::Socket, LinkSnapshot::Socket { status, .. }) => match status.as_object() {
            Some(document) => rt_fields_from_socket(document, &mut reasons),
            None => degraded_rt_fields(
                "rt-host control status response is not a JSON object (protocol violation)",
                &mut reasons,
            ),
        },
        (Source::File, _) => match file.as_ref() {
            Some(RtFile::Fresh { document, .. }) => rt_fields_from_file(document, &mut reasons),
            _ => unreachable!("Source::File 只来自 Fresh 状态文件"),
        },
        _ => {
            let reason = reasons
                .get("host_link.connected")
                .and_then(Value::as_str)
                .unwrap_or("rt-host status is not available")
                .to_string();
            degraded_rt_fields(&reason, &mut reasons)
        }
    };

    // rt-host 运行时模式（CONTRACTS B.1.5 的 status.mode，docs/API.md §9.7/G3）：
    // 只来自 rt-host 控制 status 的顶层 mode（如 "aes67"/"off"）；状态文件 schema 不落它，
    // process 目标没有 rt-host。取不到一律 null + reasons["mode"]，不用 host_link.mode（传输模式）冒充。
    let runtime_mode = if matches!(sources.target, Target::Process) {
        insert_reason(&mut reasons, "mode", PROCESS_TARGET_REASON);
        Value::Null
    } else {
        match source {
            Source::Socket => match sources.link {
                LinkSnapshot::Socket { status, .. } => {
                    match status.get("mode").and_then(Value::as_str) {
                        Some(mode) => Value::String(mode.to_string()),
                        None => {
                            insert_reason(&mut reasons, "mode", RT_HOST_MODE_MISSING_REASON);
                            Value::Null
                        }
                    }
                }
                _ => {
                    insert_reason(&mut reasons, "mode", RT_HOST_MODE_MISSING_REASON);
                    Value::Null
                }
            },
            Source::File => {
                insert_reason(&mut reasons, "mode", RT_STATUS_FILE_MODE_REASON);
                Value::Null
            }
            Source::None => {
                let reason = reasons
                    .get("host_link.connected")
                    .and_then(Value::as_str)
                    .unwrap_or("rt-host runtime mode is unavailable")
                    .to_string();
                insert_reason(&mut reasons, "mode", reason);
                Value::Null
            }
        }
    };

    // 当前已生效方案的只读回读：socket 主路径直接转发 rt-host 的加法字段；
    // 不把状态文件/本地默认值当前值（文件 schema 没有，process 目标没有 rt-host）。
    let readback = readback_value(sources.link, &mut reasons);

    // 编码：socket 主路径读 rt-host 自报的 format.encoding；file/process/旧 rt-host → null + 原因。
    let format_encoding = format_encoding_value(source, sources.link, &mut reasons);
    for path in [
        "format.aes67.channels",
        "format.aes67.map",
        "format.aes67.pt",
        "format.aes67.group",
        "format.aes67.port",
    ] {
        insert_reason(&mut reasons, path, AES67_REASON);
    }
    // output errors 是唯一一个“socket 模式能拿到、文件模式拿不到”的既有缺口。
    if rt.output.get("out_errors").is_some_and(Value::is_null) {
        insert_reason(
            &mut reasons,
            "output.out_errors",
            if source == Source::Socket {
                OUT_ERRORS_SOCKET_REASON
            } else {
                OUT_ERRORS_FILE_REASON
            },
        );
    }
    insert_reason(&mut reasons, "device.id", DEVICE_ID_REASON);
    insert_reason(&mut reasons, "device.card", DEVICE_CARD_REASON);

    // calibration：有真实服务来源就填；未接线/降级一律 null + 原因（不编 job/verdict）。
    let calibration_wired = sources.calibration.wired;
    let calibration_reason = sources
        .calibration
        .reason
        .clone()
        .unwrap_or_else(|| CALIBRATION_REASON.to_string());
    for (path, present, reason_when_missing) in [
        (
            "calibration.active_job",
            sources.calibration.active_job.is_some(),
            "no calibration job is active",
        ),
        (
            "calibration.last_job_id",
            sources.calibration.last_job_id.is_some(),
            "no calibration job has been started yet",
        ),
        (
            "calibration.last_verdict",
            sources.calibration.last_verdict.is_some(),
            "no calibration verdict has been reported yet",
        ),
    ] {
        if !present {
            insert_reason(
                &mut reasons,
                path,
                if calibration_wired {
                    reason_when_missing
                } else {
                    calibration_reason.as_str()
                },
            );
        }
    }

    // plan 元数据：graph_id/version 能拿到（socket status 报 graph_id/actual_version），其余没有数据源。
    let socket_source = source == Source::Socket;
    insert_reason(
        &mut reasons,
        "plan.sha256",
        plan_metadata_reason("sha256", mode, socket_source),
    );
    if rt.plan_version.is_null() {
        insert_reason(
            &mut reasons,
            "plan.version",
            plan_metadata_reason("version", mode, socket_source),
        );
    }
    for field in [
        "latency_samples",
        "applied_at (RFC3339)",
        "source (upload|calibration)",
    ] {
        insert_reason(
            &mut reasons,
            match field {
                "latency_samples" => "plan.latency_samples",
                "applied_at (RFC3339)" => "plan.applied_at",
                _ => "plan.source",
            },
            plan_metadata_reason(field, mode, socket_source),
        );
    }

    // device.rate_readback 与 format.device_rate 同源；来源报了 null 时两个路径都要有原因。
    if rt.device_rate.is_null() {
        let text = reasons
            .get("format.device_rate")
            .and_then(Value::as_str)
            .unwrap_or("rt-host device rate is unavailable")
            .to_string();
        insert_reason(&mut reasons, "device.rate_readback", text);
    }

    let device_state =
        device_state_value(file.as_ref(), sources.link, sources.device, &mut reasons);

    // 数据面块长/通道数：socket status 的 format 是权威（它驱动真正出块的引擎）。
    let (block_frames, channels) = format_dimensions(sources, source, &mut reasons);

    // 热重配追踪（M3b）：socket/file 的 rt-host status 都带 reconfigure 段；daemon 聚合原样带出
    // （合同 §3 的 reconfigure:{generation,...}），拿不到就 null + reason。
    let reconfigure = match (&sources.link, file.as_ref()) {
        (LinkSnapshot::Socket { status, .. }, _) => {
            status.get("reconfigure").cloned().unwrap_or(Value::Null)
        }
        (_, Some(RtFile::Fresh { document, .. })) => {
            document.get("reconfigure").cloned().unwrap_or(Value::Null)
        }
        _ => Value::Null,
    };
    if reconfigure.is_null() {
        insert_reason(
            &mut reasons,
            "reconfigure",
            "the current rt-host status source does not expose reconfigure (generation/last_gap_ms/last_result); only M3b rt-host builds report it",
        );
    }
    // 测试音状态（T71 加法字段）：socket status 优先；文件回退里 speaker_test=null / 缺失时
    // 给全 null + 原因（控制通道未启用时状态文件写不出真实状态），绝不编 active:false。
    let speaker_socket_document = match &sources.link {
        LinkSnapshot::Socket { status, .. } => Some(status),
        _ => None,
    };
    // 状态文件里的 document 是 Map；project_status 统一吃 Value（克隆一份很小的快照）。
    let speaker_file_document = match file.as_ref() {
        Some(RtFile::Fresh { document, .. }) => Some(Value::Object(document.clone())),
        _ => None,
    };
    let speaker_unavailable_reason = match sources.link {
        LinkSnapshot::Unavailable { reason, .. } => {
            format!("the rt-host control socket is unavailable ({reason})")
        }
        LinkSnapshot::Process => PROCESS_TARGET_REASON.to_string(),
        LinkSnapshot::Socket { .. } => "the rt-host control status is unavailable".to_string(),
    };
    let (speaker_test, speaker_test_reason) = speaker_test::project_status(
        speaker_socket_document,
        speaker_file_document.as_ref(),
        &speaker_unavailable_reason,
    );
    if let Some(reason) = speaker_test_reason {
        insert_reason(&mut reasons, "speaker_test", reason);
    }

    // 合同 §3 的顶层 muted/safe 别名（真实值来自 state 块，同源不重造）。
    let state_muted = rt.state_muted.clone();
    let state_safe = rt.state_safe.clone();

    json!({
        "ok": true,
        "schema": SCHEMA,
        "daemon": {
            "version": VERSION,
            "uptime_s": sources.uptime_s,
            "bind": sources.bind.to_string(),
        },
        "host_link": {
            "connected": connected,
            "mode": mode,
            "target": sources.target.as_str(),
            "socket": socket,
            "since": since,
            "age_ms": age.clone(),
            "status_age_ms": age,
        },
        "state": {
            "value": rt.state_value,
            "muted": rt.state_muted,
            "safe": rt.state_safe,
            "last_error": rt.state_last_error,
        },
        "muted": state_muted,
        "safe": state_safe,
        "reconfigure": reconfigure,
        // 运行时模式（加法字段）：rt-host 控制 status 顶层 mode；file/none/process 为 null+原因。
        "mode": runtime_mode,
        // 当前已生效方案的只读回读（加法字段）：volume/layout/routing 的“当前值”数据源。
        "readback": readback,
        "speaker_test": speaker_test,
        "plan": {
            "graph_id": rt.graph_id,
            "sha256": Value::Null,
            "version": rt.plan_version,
            "latency_samples": Value::Null,
            "applied_at": Value::Null,
            "source": Value::Null,
        },
        "format": {
            "plan_rate": rt.plan_rate,
            "device_rate": rt.device_rate.clone(),
            "rate_asserted": rt.rate_asserted,
            "block_frames": block_frames,
            "channels": channels,
            "encoding": format_encoding,
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
            "rate_readback": rt.device_rate,
        },
        "calibration": {
            "active_job": sources.calibration.active_job,
            "last_job_id": sources.calibration.last_job_id,
            "last_verdict": sources.calibration.last_verdict,
        },
        "not_wired": not_wired_entries(sources, socket_source, calibration_wired, &calibration_reason),
        "reasons": reasons,
    })
}

/// `format.encoding`：socket 主路径读 rt-host 自报的编码；其余来源 null + 原因。
///
/// 编码不是 daemon 能推断的（daemon 不打开 ALSA），状态文件 schema 也不落它；只有 rt-host
/// 控制 status 的 format.encoding 是真实来源。拿不到就不编（不把 S24_3LE 写死成本地默认）。
fn format_encoding_value(
    source: Source,
    link: &LinkSnapshot,
    reasons: &mut Map<String, Value>,
) -> Value {
    if source == Source::Socket {
        if let LinkSnapshot::Socket { status, .. } = link {
            if let Some(encoding) = status
                .get("format")
                .and_then(|format| format.get("encoding"))
                .and_then(Value::as_str)
            {
                return json!(encoding);
            }
            insert_reason(
                reasons,
                "format.encoding",
                "rt-host control status does not report format.encoding (older rt-host?); update or restart rt-host",
            );
        }
    } else if source == Source::File {
        insert_reason(
            reasons,
            "format.encoding",
            "rt-host status file (khaen-rt-status/v1) does not report the output encoding; only the control status op reports it",
        );
    } else {
        insert_reason(reasons, "format.encoding", ENCODING_REASON);
    }
    Value::Null
}

/// 数据面块长/通道数：socket status 的 `format.block_size/channels` 是权威（rt-host 真正驱动
/// 出块的引擎格式）；文件/无来源退回 daemon 的 [`EngineConfig`]（旧行为）。
fn format_dimensions(
    sources: &StatusSources<'_>,
    source: Source,
    reasons: &mut Map<String, Value>,
) -> (Value, Value) {
    if source == Source::Socket {
        if let LinkSnapshot::Socket { status, .. } = sources.link {
            if let Some(format) = status.get("format").and_then(Value::as_object) {
                let block = format.get("block_size").and_then(Value::as_u64);
                let channels = format.get("channels").and_then(Value::as_u64);
                if let (Some(block), Some(channels)) = (block, channels) {
                    return (json!(block), json!(channels));
                }
            }
        }
        insert_reason(
            reasons,
            "format.block_frames",
            "rt-host control status does not report format.block_size/channels",
        );
        insert_reason(
            reasons,
            "format.channels",
            "rt-host control status does not report format.block_size/channels",
        );
    }
    (
        json!(sources.engine.block_frames as u64),
        json!(sources.engine.channels as u64),
    )
}

/// not_wired 能力清单：控制通道没接通当前 rt-host（非 socket 模式）时列 host_link.control；
/// 校准后端未接线时列 calibration.jobs（原因随实况）。
fn not_wired_entries(
    sources: &StatusSources<'_>,
    socket_mode: bool,
    calibration_wired: bool,
    calibration_reason: &str,
) -> Vec<Value> {
    let mut entries = Vec::new();
    if !socket_mode {
        let reason = match (sources.target, sources.link) {
            (_, LinkSnapshot::Unavailable { reason, .. }) => format!(
                "rt-host control socket {} is unavailable ({reason}); control ops return 503 {HOST_LINK_UNAVAILABLE} and are NOT applied to the daemon process engine",
                sources
                    .link
                    .socket()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default()
            ),
            (Target::Process, _) => PROCESS_TARGET_REASON.to_string(),
            (Target::RtHost, _) => format!(
                "rt-host control channel is not available (host_link.mode={}); control ops return 503 {HOST_LINK_UNAVAILABLE}",
                "none"
            ),
        };
        entries.push(json!({ "path": "host_link.control", "reason": reason }));
    }
    if !calibration_wired {
        entries.push(json!({ "path": "calibration.jobs", "reason": calibration_reason }));
    }
    entries
}

/// `/api/meta` 的 host_link 能力对象（现取 probe：target/mode/connected/socket/error）。
///
/// 与 `/api/status` 的区别：meta 只回答“链路能不能用”，不带整份状态；仍是真实探测，不缓存。
pub(crate) fn link_capability(state: &DaemonState, now: SystemTime) -> Value {
    let link = state.host_link().status_snapshot();
    link_capability_at(
        state.host_link().target(),
        &link,
        state.rt_status_file(),
        now,
    )
}

fn link_capability_at(
    target: Target,
    link: &LinkSnapshot,
    status_file: &Path,
    now: SystemTime,
) -> Value {
    let socket_mode = link.is_socket();
    let file_fresh = if socket_mode {
        false
    } else {
        matches!(read_rt_status(status_file, now), RtFile::Fresh { .. })
    };
    let mode = if socket_mode {
        "socket"
    } else if file_fresh {
        "file"
    } else {
        "none"
    };
    let error = match (target, link) {
        (_, LinkSnapshot::Socket { .. }) => Value::Null,
        (_, LinkSnapshot::Unavailable { reason, .. }) => Value::String(reason.clone()),
        (Target::Process, _) => Value::String(PROCESS_TARGET_REASON.to_string()),
        (Target::RtHost, LinkSnapshot::Process) => {
            Value::String("no rt-host control link in this process".to_string())
        }
    };
    json!({
        "target": target.as_str(),
        "mode": mode,
        "connected": mode != "none",
        "socket": link.socket().map(|path| Value::String(path.display().to_string())),
        "error": error,
    })
}

/// 状态聚合任务异常（JoinError，正常不可达）时的兜底 body：schema 完整、daemon 字段真实、
/// 其余 null + 原因；`/api/status` 永远 200 的契约不破。
pub(crate) fn internal_failure_body(state: &DaemonState, message: &str) -> Value {
    let engine = state.engine_config();
    let device = json!({ "wired": false, "reason": message });
    let calibration = state.calibration_status();
    let link = LinkSnapshot::Unavailable {
        socket: state.host_link().config().socket.clone(),
        reason: format!("status aggregation task failed: {message}"),
    };
    aggregate_at(
        &StatusSources {
            bind: state.bind(),
            uptime_s: state.uptime_s(),
            engine: &engine,
            device: &device,
            rt_status_file: state.rt_status_file(),
            calibration: &calibration,
            target: state.host_link().target(),
            link: &link,
        },
        SystemTime::now(),
    )
}

/// rt-host 状态文件的判定结果。
///
/// T46 起 system 模块复用同一份读取/新鲜度判定（/api/system/info 与 /api/status 同口径）。
pub(crate) enum RtFile {
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
    pub(crate) fn mtime(&self) -> Option<SystemTime> {
        match self {
            Self::Missing { .. } => None,
            Self::Invalid { mtime, .. } | Self::Fresh { mtime, .. } => Some(*mtime),
        }
    }

    /// mtime 与 now 之差（毫秒；文件不存在时为 None）。
    pub(crate) fn age_ms(&self) -> Option<u64> {
        match self {
            Self::Missing { .. } => None,
            Self::Invalid { age_ms, .. } | Self::Fresh { age_ms, .. } => Some(*age_ms),
        }
    }

    /// 不可用原因（可用时为 None）。
    pub(crate) fn unavailable_reason(&self) -> Option<&str> {
        match self {
            Self::Missing { reason } | Self::Invalid { reason, .. } => Some(reason),
            Self::Fresh { .. } => None,
        }
    }
}

/// 读并校验 rt-host 状态文件；任何异常都只降级，不向调用方报错（handler 绝不 500）。
pub(crate) fn read_rt_status(path: &Path, now: SystemTime) -> RtFile {
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
    plan_version: Value,
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
        "plan.version",
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
        plan_version: Value::Null,
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
    // 状态文件只写 graph_id，不写实际生效版本（actual_version）：如实给 null + 原因，
    // 因此这里不登记（由聚合器统一为 plan.version 说明缺失原因）。
    let plan_version = Value::Null;
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
        plan_version,
        plan_rate,
        device_rate,
        rate_asserted,
        input,
        output,
    }
}

/// socket status（契约 B 的 status op）→ v1 字段；缺失/null 逐路径给原因。
///
/// 与状态文件同形但更全：多报 `actual_version`（→ plan.version）、`output_errors`
/// （→ output.out_errors）与数据面 `format`；并在 rt-host 自己给了 `reasons[字段]` 时
/// 优先用它的解释（例如“还没有方案生效”）。
fn rt_fields_from_socket(
    document: &Map<String, Value>,
    reasons: &mut Map<String, Value>,
) -> RtFields {
    let state_value = socket_required_string(document, "runtime_state", "state.value", reasons);
    let state_muted = socket_required_bool(document, "mute", "state.muted", reasons);
    let state_safe = socket_required_bool(document, "safe_mode", "state.safe", reasons);
    // last_error 的 null 是“没有错误”这一真实状态（与状态文件语义一致），不登记原因。
    let state_last_error =
        socket_nullable_string(document, "last_error", "state.last_error", reasons, false);
    let graph_id = match socket_value(document, "graph_id") {
        Some(Value::String(id)) => json!(id),
        _ => {
            insert_socket_reason(
                document,
                reasons,
                "plan.graph_id",
                "graph_id",
                "rt-host control status reports no applied plan graph_id yet",
            );
            Value::Null
        }
    };
    let plan_version = match socket_value(document, "actual_version") {
        Some(Value::Number(number)) if number.as_u64().is_some() => json!(number),
        _ => {
            insert_socket_reason(
                document,
                reasons,
                "plan.version",
                "actual_version",
                "rt-host control status reports no applied plan version yet (actual_version)",
            );
            Value::Null
        }
    };
    let plan_rate = socket_required_u64(document, "plan_rate", "format.plan_rate", reasons);
    let device_rate = socket_nullable_u64(document, "device_rate", "format.device_rate", reasons);
    let rate_asserted =
        socket_required_bool(document, "rate_asserted", "format.rate_asserted", reasons);
    let input = json!({
        "packets": socket_required_u64(document, "input.packets", "input.packets", reasons),
        "underruns": socket_required_u64(document, "input.underruns", "input.underruns", reasons),
        "jitter_fill": socket_required_u64(
            document,
            "input.jitter_fill",
            "input.jitter_fill",
            reasons,
        ),
        "ssrc": socket_nullable_u64(document, "input.ssrc", "input.ssrc", reasons),
        "queue_full_drops": socket_required_u64(
            document,
            "receiver.queue_full_drops",
            "input.queue_full_drops",
            reasons,
        ),
        "recv_errors": socket_required_u64(
            document,
            "receiver.recv_errors",
            "input.recv_errors",
            reasons,
        ),
    });
    let output = json!({
        "blocks": socket_required_u64(document, "blocks", "output.blocks", reasons),
        "xruns": socket_required_u64(document, "output.xruns", "output.xruns", reasons),
        "out_errors": socket_required_u64(
            document,
            "output_errors",
            "output.out_errors",
            reasons,
        ),
        "bytes_written": socket_required_u64(
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
        plan_version,
        plan_rate,
        device_rate,
        rate_asserted,
        input,
        output,
    }
}

/// 按 `a.b` 路径取 rt-host status 字段（与状态文件的 lookup 同形）。
fn socket_value<'a>(document: &'a Map<String, Value>, source: &str) -> Option<&'a Value> {
    lookup(document, source)
}

/// rt-host status 自己的 `reasons[字段]` 文案（优先用它的解释，不另编）。
fn socket_reason<'a>(document: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    document
        .get("reasons")
        .and_then(Value::as_object)
        .and_then(|reasons| reasons.get(key))
        .and_then(Value::as_str)
}

/// 登记原因：rt-host 自己给了解释就用它（前缀 `rt-host: ` 标注来源），否则用稳定兜底文案。
fn insert_socket_reason(
    document: &Map<String, Value>,
    reasons: &mut Map<String, Value>,
    path: &str,
    rt_key: &str,
    fallback: &str,
) {
    let text = socket_reason(document, rt_key)
        .map(|reason| format!("rt-host: {reason}"))
        .unwrap_or_else(|| fallback.to_string());
    insert_reason(reasons, path, text);
}

/// 原因文案里的数据源标签：状态文件 / 控制通道 status。
const FILE_FIELD_LABEL: &str = "rt-host status file";
const SOCKET_FIELD_LABEL: &str = "rt-host control status";

/// 按 `a.b` 路径取字段（只支持一层嵌套；socket status 与状态文件同形）。
fn lookup<'a>(document: &'a Map<String, Value>, source: &str) -> Option<&'a Value> {
    match source.split_once('.') {
        Some((outer, inner)) => document
            .get(outer)
            .and_then(Value::as_object)
            .and_then(|object| object.get(inner)),
        None => document.get(source),
    }
}

/// 字段取值核心：缺失/类型不对/null 都返回 `null` 并登记“字段路径 → 原因”。
fn field_u64(
    document: &Map<String, Value>,
    source: &str,
    path: &str,
    reasons: &mut Map<String, Value>,
    label: &str,
) -> Value {
    match lookup(document, source) {
        Some(Value::Number(number)) => match number.as_u64() {
            Some(value) => json!(value),
            None => {
                insert_reason(
                    reasons,
                    path,
                    format!("{label} field `{source}` is not a non-negative integer"),
                );
                Value::Null
            }
        },
        Some(Value::Null) => {
            insert_reason(reasons, path, format!("{label} reports `{source}` as null"));
            Value::Null
        }
        Some(_) => {
            insert_reason(
                reasons,
                path,
                format!("{label} field `{source}` is not a non-negative integer"),
            );
            Value::Null
        }
        None => {
            insert_reason(
                reasons,
                path,
                format!("{label} does not provide field `{source}`"),
            );
            Value::Null
        }
    }
}

/// 布尔字段取值（缺失/类型不对/null 都返回 `null` + 原因）。
fn field_bool(
    document: &Map<String, Value>,
    source: &str,
    path: &str,
    reasons: &mut Map<String, Value>,
    label: &str,
) -> Value {
    match lookup(document, source) {
        Some(Value::Bool(value)) => json!(value),
        Some(Value::Null) => {
            insert_reason(reasons, path, format!("{label} reports `{source}` as null"));
            Value::Null
        }
        Some(_) => {
            insert_reason(
                reasons,
                path,
                format!("{label} field `{source}` is not a boolean"),
            );
            Value::Null
        }
        None => {
            insert_reason(
                reasons,
                path,
                format!("{label} does not provide field `{source}`"),
            );
            Value::Null
        }
    }
}

/// 字符串字段取值；`report_null` 决定显式 null 是否算“缺数据”而登记原因
/// （例如 last_error 的 null 是“没有错误”的真实状态，不该登记）。
fn field_string(
    document: &Map<String, Value>,
    source: &str,
    path: &str,
    reasons: &mut Map<String, Value>,
    label: &str,
    report_null: bool,
) -> Value {
    match lookup(document, source) {
        Some(Value::String(value)) => json!(value),
        Some(Value::Null) => {
            if report_null {
                insert_reason(reasons, path, format!("{label} reports `{source}` as null"));
            }
            Value::Null
        }
        Some(_) => {
            insert_reason(
                reasons,
                path,
                format!("{label} field `{source}` is not a string"),
            );
            Value::Null
        }
        None => {
            insert_reason(
                reasons,
                path,
                format!("{label} does not provide field `{source}`"),
            );
            Value::Null
        }
    }
}

/// 状态文件字段：必填非负整数。
fn required_u64(
    document: &Map<String, Value>,
    source: &str,
    path: &str,
    reasons: &mut Map<String, Value>,
) -> Value {
    field_u64(document, source, path, reasons, FILE_FIELD_LABEL)
}

/// 状态文件字段：可空非负整数（rt-host 用 null 表示“还没读到”；null 也给原因）。
fn nullable_u64(
    document: &Map<String, Value>,
    source: &str,
    path: &str,
    reasons: &mut Map<String, Value>,
) -> Value {
    field_u64(document, source, path, reasons, FILE_FIELD_LABEL)
}

/// 状态文件字段：必填布尔。
fn required_bool(
    document: &Map<String, Value>,
    source: &str,
    path: &str,
    reasons: &mut Map<String, Value>,
) -> Value {
    field_bool(document, source, path, reasons, FILE_FIELD_LABEL)
}

/// 状态文件字段：必填字符串。
fn required_string(
    document: &Map<String, Value>,
    source: &str,
    path: &str,
    reasons: &mut Map<String, Value>,
) -> Value {
    field_string(document, source, path, reasons, FILE_FIELD_LABEL, true)
}

/// 状态文件字段：可空字符串；`report_null` 决定显式 null 是否登记原因。
fn nullable_string(
    document: &Map<String, Value>,
    source: &str,
    path: &str,
    reasons: &mut Map<String, Value>,
    report_null: bool,
) -> Value {
    field_string(
        document,
        source,
        path,
        reasons,
        FILE_FIELD_LABEL,
        report_null,
    )
}

/// 控制通道 status 字段：必填非负整数（语义同状态文件，原因文案点名控制通道）。
fn socket_required_u64(
    document: &Map<String, Value>,
    source: &str,
    path: &str,
    reasons: &mut Map<String, Value>,
) -> Value {
    field_u64(document, source, path, reasons, SOCKET_FIELD_LABEL)
}

/// 控制通道 status 字段：可空非负整数。
fn socket_nullable_u64(
    document: &Map<String, Value>,
    source: &str,
    path: &str,
    reasons: &mut Map<String, Value>,
) -> Value {
    field_u64(document, source, path, reasons, SOCKET_FIELD_LABEL)
}

/// 控制通道 status 字段：必填布尔。
fn socket_required_bool(
    document: &Map<String, Value>,
    source: &str,
    path: &str,
    reasons: &mut Map<String, Value>,
) -> Value {
    field_bool(document, source, path, reasons, SOCKET_FIELD_LABEL)
}

/// 控制通道 status 字段：必填字符串。
fn socket_required_string(
    document: &Map<String, Value>,
    source: &str,
    path: &str,
    reasons: &mut Map<String, Value>,
) -> Value {
    field_string(document, source, path, reasons, SOCKET_FIELD_LABEL, true)
}

/// 控制通道 status 字段：可空字符串。
fn socket_nullable_string(
    document: &Map<String, Value>,
    source: &str,
    path: &str,
    reasons: &mut Map<String, Value>,
    report_null: bool,
) -> Value {
    field_string(
        document,
        source,
        path,
        reasons,
        SOCKET_FIELD_LABEL,
        report_null,
    )
}

/// `/api/status.readback`：当前已生效方案的只读回读。
///
/// 只有 rt-host 控制 status（socket 主路径）能提供它（rt-host 的 `base_plan` 是块边界已生效方案）。
/// 状态文件 schema 不落这份数据、process 目标没有 rt-host；这些情况一律 null + 原因，
/// 不用 daemon 的 EngineConfig、默认布局或上次缓存冒充“当前值”。
fn readback_value(link: &LinkSnapshot, reasons: &mut Map<String, Value>) -> Value {
    match link {
        LinkSnapshot::Socket { status, .. } => match status.get("readback") {
            Some(readback @ Value::Object(_)) => readback.clone(),
            Some(other) => {
                insert_reason(
                    reasons,
                    "readback",
                    format!("rt-host control status readback is not an object: {other}"),
                );
                Value::Null
            }
            None => {
                insert_reason(
                    reasons,
                    "readback",
                    "the rt-host control status does not report readback (older rt-host?); update or restart rt-host to read the applied plan",
                );
                Value::Null
            }
        },
        LinkSnapshot::Unavailable { reason, .. } => {
            insert_reason(
                reasons,
                "readback",
                format!("the rt-host control socket is unavailable ({reason}); readback requires the rt-host control status"),
            );
            Value::Null
        }
        LinkSnapshot::Process => {
            insert_reason(reasons, "readback", PROCESS_TARGET_REASON);
            Value::Null
        }
    }
}

/// `device.state`：优先 rt-host 的 `device_state`（socket status 或状态文件），
/// 退而用 daemon `DeviceMonitor` 的生命周期状态。
fn device_state_value(
    file: Option<&RtFile>,
    link: &LinkSnapshot,
    monitor: &Value,
    reasons: &mut Map<String, Value>,
) -> Value {
    let file_fresh = matches!(file, Some(RtFile::Fresh { .. }));
    let rt_device_state = match link {
        LinkSnapshot::Socket { status, .. } => status.get("device_state").and_then(Value::as_str),
        _ => file_fresh
            .then(|| {
                file.and_then(|file| match file {
                    RtFile::Fresh { document, .. } => {
                        document.get("device_state").and_then(Value::as_str)
                    }
                    RtFile::Missing { .. } | RtFile::Invalid { .. } => None,
                })
            })
            .flatten(),
    };
    match rt_device_state {
        Some(state @ ("present" | "absent")) => return json!(state),
        Some(other) => {
            insert_reason(
                reasons,
                "device.state",
                format!(
                    "rt-host reports unsupported device_state {other:?} (expected present|absent)"
                ),
            );
        }
        None => {}
    }
    if let Some(state) = monitor_device_state(monitor) {
        return json!(state);
    }
    let monitor_reason = monitor
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("no probe configured in this process");
    let rt_reason = match link {
        LinkSnapshot::Socket { .. } => {
            "rt-host control status does not report device_state".to_string()
        }
        LinkSnapshot::Unavailable { reason, .. } => reason.clone(),
        LinkSnapshot::Process => PROCESS_TARGET_REASON.to_string(),
    };
    let reason = if file_fresh {
        format!(
            "rt-host status file does not provide device_state; {rt_reason}; daemon device monitor is not wired: {monitor_reason}"
        )
    } else {
        let file_reason = file
            .and_then(RtFile::unavailable_reason)
            .unwrap_or("rt-host status file is not available");
        format!("{file_reason}; {rt_reason}; daemon device monitor is not wired: {monitor_reason}")
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

/// plan 元数据（sha256/latency_samples/...）拿不到的统一原因。
///
/// socket 模式：rt-host 的 status op 只报 graph_id/actual_version，没有 sha256/延迟/生效时刻/来源；
/// 文件/无来源：状态文件只写 graph_id，而且控制通道当前不可用（如实说明，不写死“T70f 未实现”）。
fn plan_metadata_reason(field: &str, mode: &str, socket_source: bool) -> String {
    if socket_source {
        format!(
            "the rt-host control status (op status) does not report the applied plan {field}: only graph_id and actual_version are reported"
        )
    } else {
        format!(
            "rt-host status is not available via the control channel (host_link.mode={mode}); the status file ({RT_STATUS_SCHEMA}) only carries graph_id, not the applied plan {field}"
        )
    }
}

/// `SystemTime` → RFC3339 UTC 字符串（秒精度，`Z` 结尾；不引入时间库）。
pub(crate) fn rfc3339_utc(time: SystemTime) -> String {
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
        "mode",
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

    /// 默认测试用校准状态：未接线 + 兜底原因（与生产默认 NotWiredCalibration 同形）。
    fn not_wired_calibration() -> CalibrationStatus {
        CalibrationStatus {
            wired: false,
            engine: None,
            active_job: None,
            last_job_id: None,
            last_verdict: None,
            reason: Some(CALIBRATION_REASON.to_string()),
        }
    }

    fn aggregate_with_monitor(path: &Path, monitor: &Value, now: SystemTime) -> Value {
        let calibration = not_wired_calibration();
        aggregate_with_calibration(path, monitor, &calibration, now)
    }

    /// 测试用控制 socket 路径（fixture：socket 不可用，退回状态文件）。
    const TEST_SOCKET: &str = "/tmp/khaen-test-rt-host.sock";

    /// socket 不可用的控制通道快照（文件模式/无来源测试用；target=rt-host）。
    fn unavailable_link(reason: &str) -> LinkSnapshot {
        LinkSnapshot::Unavailable {
            socket: PathBuf::from(TEST_SOCKET),
            reason: reason.to_string(),
        }
    }

    fn aggregate_with_calibration(
        path: &Path,
        monitor: &Value,
        calibration: &CalibrationStatus,
        now: SystemTime,
    ) -> Value {
        let link = unavailable_link(
            "connect: rt-host control socket is not available in this test fixture",
        );
        aggregate_with_sources(path, monitor, calibration, Target::RtHost, &link, now)
    }

    /// 用显式来源/目标聚合（socket/file/none 三模式与 process 目标的单测入口）。
    fn aggregate_with_sources(
        path: &Path,
        monitor: &Value,
        calibration: &CalibrationStatus,
        target: Target,
        link: &LinkSnapshot,
        now: SystemTime,
    ) -> Value {
        let engine = test_engine();
        aggregate_at(
            &StatusSources {
                bind: test_bind(),
                uptime_s: 7,
                engine: &engine,
                device: monitor,
                rt_status_file: path,
                calibration,
                target,
                link,
            },
            now,
        )
    }

    /// socket 模式聚合：一份 rt-host status 响应 + 不存在的状态文件（socket 是主路径）。
    fn aggregate_socket(status: &Value, now: SystemTime) -> Value {
        let dir = TempDir::new("socket-mode");
        let path = dir.file("missing.json");
        let monitor = monitor_not_wired();
        let calibration = not_wired_calibration();
        let link = LinkSnapshot::Socket {
            socket: PathBuf::from(TEST_SOCKET),
            status: status.clone(),
            age_ms: status.get("age_ms").and_then(Value::as_u64),
        };
        aggregate_with_sources(&path, &monitor, &calibration, Target::RtHost, &link, now)
    }

    /// 一份完整的 rt-host 控制 status 响应（形状与 `crates/rt-host/src/control/core.rs` 同源；
    /// 值故意与默认值不同，防止“碰巧相等”）。
    fn full_socket_status() -> Value {
        json!({
            "ok": true,
            "protocol_version": 1,
            "runtime_state": "running",
            "graph_id": "cafebabe0011",
            "epoch": 3,
            "blocks": 4321,
            "xruns": 2,
            "underruns": 1,
            "faults": 0,
            "silent": false,
            "mute": false,
            "safe_mode": false,
            "actual_version": 7,
            "mode": "aes67",
            "format": { "sample_rate": 48_000, "block_size": 256, "channels": 24, "encoding": "S24_3LE" },
            "control_format": { "sample_rate": 48_000, "block_size": 256, "channels": 24 },
            "format_mismatch": false,
            "device_restart_required": false,
            "switch_pending": false,
            "plan_rate": 48_000,
            "device_rate": 48_000,
            "rate_asserted": true,
            "last_error": null,
            "output_errors": 5,
            "output_started": true,
            "input": {
                "packets": 111,
                "frames_filled": 2222,
                "lost": 0,
                "late": 0,
                "underruns": 2,
                "overruns": 0,
                "malformed": 0,
                "jitter_fill": 192,
                "ssrc": 534_361_700u64,
            },
            "output": { "xruns": 2, "blocks_written": 1200, "bytes_written": 10_137_600u64 },
            "receiver": {
                "packets_received": 115,
                "bytes_received": 1_000u64,
                "queue_full_drops": 1,
                "recv_errors": 0,
                "running": true,
            },
            "device": {
                "state": "streaming",
                "plan_rate": 48_000,
                "device_rate": 48_000,
                "rate_asserted": true,
                "last_error": null,
                "attempts": 0,
                "since_ms": 0,
            },
            "device_state": "present",
            "absent_since": null,
            "reopen_attempts": 0,
            "last_device_error": null,
            "wired": ["status", "mute"],
            "not_wired": [],
            "calibration": { "enabled": false },
            "surfaces": ["volume"],
            "surface_reasons": {},
            "readback": {
                "available": true,
                "source": "base_plan",
                "sample_rate": 48_000,
                "block_size": 256,
                "content": {
                    "in_channels": 2,
                    "out_lanes": 13,
                    "folds": [{ "src": 0, "dst": 0, "gain": 1.0 }],
                },
                "crossover": { "lanes": 13, "filters": [{ "lane": 0, "taps": 2048 }] },
                "matrix": { "rows": 13, "cols": 13, "decimation": 16, "interpolation": 16, "taps": 1 },
                "protection": {
                    "lanes": 13,
                    "entries": [{ "lane": 0, "delay_samples": 0, "polarity": 1, "gain_db": -2.5, "peq_bands": 0 }],
                },
                "routing": { "dac_channels": 22, "routes": [{ "lane": 0, "dac": 0 }] },
            },
            "age_ms": 42,
            "stale": false,
            "snapshot_interval_ms": 2,
            "reasons": {},
            "notes": {},
        })
    }

    #[test]
    fn wired_calibration_fills_status_fields_and_drops_not_wired_entry() {
        let dir = TempDir::new("calibration-wired");
        let path = dir.file("missing.json");
        let calibration = CalibrationStatus {
            wired: true,
            engine: Some("offline".into()),
            active_job: Some("job-0002".into()),
            last_job_id: Some("job-0001".into()),
            last_verdict: Some("PASS".into()),
            reason: None,
        };
        let body = aggregate_with_calibration(
            &path,
            &monitor_not_wired(),
            &calibration,
            SystemTime::now(),
        );
        assert_eq!(at(&body, "calibration.active_job"), &json!("job-0002"));
        assert_eq!(at(&body, "calibration.last_job_id"), &json!("job-0001"));
        assert_eq!(at(&body, "calibration.last_verdict"), &json!("PASS"));
        for path_ in [
            "calibration.active_job",
            "calibration.last_job_id",
            "calibration.last_verdict",
        ] {
            assert!(
                body["reasons"].get(path_).is_none(),
                "{path_} 有真实值时不登记原因"
            );
        }
        let not_wired = body["not_wired"].as_array().expect("not_wired 必须是数组");
        assert!(not_wired
            .iter()
            .all(|entry| entry["path"] != json!("calibration.jobs")));
    }

    #[test]
    fn wired_calibration_without_jobs_keeps_null_plus_reason() {
        let dir = TempDir::new("calibration-wired-empty");
        let path = dir.file("missing.json");
        let calibration = CalibrationStatus {
            wired: true,
            engine: None,
            active_job: None,
            last_job_id: None,
            last_verdict: None,
            reason: None,
        };
        let body = aggregate_with_calibration(
            &path,
            &monitor_not_wired(),
            &calibration,
            SystemTime::now(),
        );
        for path_ in [
            "calibration.active_job",
            "calibration.last_job_id",
            "calibration.last_verdict",
        ] {
            assert_eq!(at(&body, path_), &Value::Null, "{path_}");
            let reason = reason_of(&body, path_);
            assert!(
                reason.starts_with("no calibration"),
                "{path_} 的原因应说明还没 job: {reason}"
            );
        }
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
        // 顶层字段集：PLAN §4.4 的 12 个必备字段 + 合同 §3 的加法别名
        // （ok/muted/safe/reconfigure，见 docs/API.md §3 与 v1.1/v1.2 变更记录）。
        let expected: Vec<String> = [
            "calibration",
            "daemon",
            "device",
            "format",
            "host_link",
            "input",
            "mode",
            "muted",
            "not_wired",
            "ok",
            "output",
            "plan",
            "readback",
            "reasons",
            "reconfigure",
            "safe",
            "schema",
            "speaker_test",
            "state",
        ]
        .iter()
        .map(|key| (*key).to_string())
        .collect();
        assert_eq!(top_level_keys(&body), expected);

        assert_daemon_fields(&body);

        assert_eq!(body["host_link"]["connected"], json!(true));
        assert_eq!(body["host_link"]["mode"], json!("file"));
        // 运行时 mode 只来自控制 status op：状态文件回退时 null + 原因（不拿传输模式冒充）。
        assert_eq!(body["mode"], Value::Null);
        assert!(reason_of(&body, "mode").contains("status file"));
        assert_eq!(body["host_link"]["target"], json!("rt-host"));
        assert_eq!(
            body["host_link"]["socket"],
            json!(TEST_SOCKET),
            "rt-host 目标下如实上报配置的 socket 路径（不可用时已降级到文件）"
        );
        let age = body["host_link"]["status_age_ms"]
            .as_u64()
            .expect("status_age_ms 必须是整数");
        assert!(age <= FRESH_MS, "刚写的状态文件不该过期: age={age}");
        assert_eq!(
            body["host_link"]["age_ms"],
            json!(age),
            "age_ms 与 status_age_ms 同源（文件 mtime 年龄）"
        );
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
        assert!(
            reason_of(&body, "format.encoding").contains("status file"),
            "file 来源必须说明状态文件不落编码: {}",
            reason_of(&body, "format.encoding")
        );
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
        assert_eq!(
            reason_of(&body, "output.out_errors"),
            OUT_ERRORS_FILE_REASON
        );

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
            .any(|entry| entry["path"] == json!("host_link.control")));
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
        let socket_reason = reason_of(&body, "host_link.socket");
        assert!(
            socket_reason.contains("unavailable"),
            "socket 原因应说明不可用: {socket_reason}"
        );
        assert!(
            socket_reason.contains("mode=file"),
            "socket 原因应说明当前降级来源: {socket_reason}"
        );
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
        assert_eq!(body["host_link"]["mode"], json!("none"));
        assert_eq!(body["host_link"]["target"], json!("rt-host"));
        assert_eq!(body["host_link"]["socket"], json!(TEST_SOCKET));
        assert_eq!(body["host_link"]["since"], Value::Null);
        assert_eq!(body["host_link"]["status_age_ms"], Value::Null);
        assert_eq!(body["host_link"]["age_ms"], Value::Null);
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

    #[test]
    fn socket_status_is_the_primary_source_and_fills_socket_only_fields() {
        let body = aggregate_socket(&full_socket_status(), SystemTime::now());

        // host_link：socket 模式（connected=true、age 来自 rt-host 快照、since 无来源）。
        assert_eq!(body["host_link"]["connected"], json!(true));
        assert_eq!(body["host_link"]["mode"], json!("socket"));
        assert_eq!(body["host_link"]["target"], json!("rt-host"));
        assert_eq!(body["host_link"]["socket"], json!(TEST_SOCKET));
        assert_eq!(body["host_link"]["age_ms"], json!(42));
        // 运行时 mode：socket status 顶层 mode 原样投影（与 host_link.mode=传输模式无关）。
        assert_eq!(body["mode"], json!("aes67"));
        assert!(
            body["reasons"].get("mode").is_none(),
            "socket status 有真实 mode，不该登记缺口原因: {}",
            body["reasons"]
        );
        assert_eq!(body["host_link"]["status_age_ms"], json!(42));
        assert_eq!(body["host_link"]["since"], Value::Null);
        assert!(reason_of(&body, "host_link.since").contains("snapshot age"));
        assert!(body["reasons"].get("host_link.connected").is_none());
        assert!(body["reasons"].get("host_link.socket").is_none());

        // state / plan：actual_version 是 v1 的 plan.version（文件模式拿不到）。
        assert_eq!(body["state"]["value"], json!("running"));
        assert_eq!(body["state"]["muted"], json!(false));
        assert_eq!(body["state"]["safe"], json!(false));
        assert_eq!(body["state"]["last_error"], Value::Null);
        assert!(body["reasons"].get("state.last_error").is_none());
        assert_eq!(at(&body, "plan.graph_id"), &json!("cafebabe0011"));
        assert_eq!(at(&body, "plan.version"), &json!(7));
        assert!(body["reasons"].get("plan.version").is_none());
        assert_eq!(at(&body, "plan.sha256"), &Value::Null);
        assert!(reason_of(&body, "plan.sha256").contains("control status"));

        // format：socket 的 format 是权威数据面格式（256/24，而不是 daemon 引擎的 128/2）。
        assert_eq!(at(&body, "format.block_frames"), &json!(256));
        assert_eq!(at(&body, "format.channels"), &json!(24));
        assert_eq!(at(&body, "format.plan_rate"), &json!(48_000));
        assert_eq!(at(&body, "format.device_rate"), &json!(48_000));
        assert_eq!(at(&body, "format.rate_asserted"), &json!(true));
        assert_eq!(at(&body, "format.encoding"), &json!("S24_3LE"));
        assert!(
            body["reasons"].get("format.encoding").is_none(),
            "socket status 拿到真实编码，不该再登记缺口原因: {}",
            body["reasons"]
        );

        // counters：socket 模式还能拿到 output.out_errors（文件模式拿不到）。
        assert_eq!(at(&body, "input.packets"), &json!(111));
        assert_eq!(at(&body, "input.underruns"), &json!(2));
        assert_eq!(at(&body, "input.jitter_fill"), &json!(192));
        assert_eq!(at(&body, "input.ssrc"), &json!(534_361_700u64));
        assert_eq!(at(&body, "input.queue_full_drops"), &json!(1));
        assert_eq!(at(&body, "input.recv_errors"), &json!(0));
        assert_eq!(at(&body, "output.blocks"), &json!(4321));
        assert_eq!(at(&body, "output.xruns"), &json!(2));
        assert_eq!(at(&body, "output.out_errors"), &json!(5));
        assert_eq!(at(&body, "output.bytes_written"), &json!(10_137_600u64));
        assert!(
            body["reasons"].get("output.out_errors").is_none(),
            "socket 模式拿到了真实 output_errors，不该再登记缺口原因"
        );

        // device：rt-host 报的 device_state 优先。
        assert_eq!(at(&body, "device.state"), &json!("present"));
        assert_eq!(at(&body, "device.rate_readback"), &json!(48_000));

        // readback：socket 主路径原样转发已生效方案的只读语义，不用默认值/daemon 配置冒充。
        assert_eq!(body["readback"]["available"], json!(true));
        assert_eq!(body["readback"]["source"], json!("base_plan"));
        assert_eq!(at(&body, "readback.content.out_lanes"), &json!(13));
        assert_eq!(
            at(&body, "readback.protection.entries"),
            &json!([{
                "lane": 0,
                "delay_samples": 0,
                "polarity": 1,
                "gain_db": -2.5,
                "peq_bands": 0,
            }])
        );
        assert_eq!(at(&body, "readback.routing.dac_channels"), &json!(22));
        assert!(
            body["reasons"].get("readback").is_none(),
            "socket 拿到真实 readback，不该登记缺口原因: {}",
            body["reasons"]
        );

        // 控制通道已接通：not_wired 里不该再有 host_link.control。
        let not_wired = body["not_wired"].as_array().expect("not_wired");
        assert!(not_wired
            .iter()
            .all(|entry| entry["path"] != json!("host_link.control")));
        assert!(not_wired
            .iter()
            .any(|entry| entry["path"] == json!("calibration.jobs")));
    }

    #[test]
    fn socket_status_without_readback_is_null_with_reason() {
        let mut status = full_socket_status();
        status
            .as_object_mut()
            .expect("status object")
            .remove("readback");
        let body = aggregate_socket(&status, SystemTime::now());
        assert_eq!(body["readback"], Value::Null);
        assert!(
            reason_of(&body, "readback").contains("does not report readback"),
            "{}",
            reason_of(&body, "readback")
        );
    }

    #[test]
    fn socket_status_forwards_rt_host_reasons_for_missing_plan_fields() {
        let mut status = full_socket_status();
        status["graph_id"] = Value::Null;
        status["actual_version"] = Value::Null;
        status["reasons"] = json!({
            "graph_id": "no plan applied to the running engine yet",
            "actual_version": "no plan applied to the running engine yet",
        });
        let body = aggregate_socket(&status, SystemTime::now());
        assert_eq!(at(&body, "plan.graph_id"), &Value::Null);
        assert_eq!(at(&body, "plan.version"), &Value::Null);
        assert!(
            reason_of(&body, "plan.graph_id").contains("rt-host: no plan applied"),
            "{}",
            reason_of(&body, "plan.graph_id")
        );
        assert!(
            reason_of(&body, "plan.version").contains("rt-host: no plan applied"),
            "{}",
            reason_of(&body, "plan.version")
        );
    }

    #[test]
    fn socket_connected_before_the_first_snapshot_keeps_null_fields_with_reasons() {
        let status = json!({
            "ok": true,
            "protocol_version": 1,
            "runtime_state": "starting",
            "age_ms": Value::Null,
            "format": { "sample_rate": 48_000, "block_size": 128, "channels": 22 },
            "reasons": { "snapshot": "the block loop has not published its first runtime snapshot yet" },
        });
        let body = aggregate_socket(&status, SystemTime::now());
        // socket 通了就是 connected（链路可用），但每个还没数据的字段都是 null + 原因。
        assert_eq!(body["host_link"]["connected"], json!(true));
        assert_eq!(body["host_link"]["mode"], json!("socket"));
        assert_eq!(body["host_link"]["age_ms"], Value::Null);
        // 这个旧/裁剪的 status 响应没有顶层 mode：null + 原因，不编造。
        assert_eq!(body["mode"], Value::Null);
        assert!(reason_of(&body, "mode").contains("does not report"));
        assert!(reason_of(&body, "host_link.age_ms").contains("first runtime snapshot"));
        assert_eq!(body["state"]["value"], json!("starting"));
        assert_eq!(body["state"]["muted"], Value::Null);
        assert!(reason_of(&body, "state.muted").contains("control status"));
        assert_eq!(at(&body, "plan.graph_id"), &Value::Null);
        assert_eq!(at(&body, "output.blocks"), &Value::Null);
        // 数据面格式仍然可用（rt-host status 总会带 format）。
        assert_eq!(at(&body, "format.block_frames"), &json!(128));
        assert_eq!(at(&body, "format.channels"), &json!(22));
    }

    #[test]
    fn process_target_never_uses_the_socket_and_says_so() {
        let dir = TempDir::new("process-target");
        let path = dir.file("rt-host-status.json");
        write_document(&path, &full_document());
        let monitor = monitor_not_wired();
        let calibration = not_wired_calibration();
        let body = aggregate_with_sources(
            &path,
            &monitor,
            &calibration,
            Target::Process,
            &LinkSnapshot::Process,
            SystemTime::now(),
        );
        assert_eq!(body["host_link"]["target"], json!("daemon-process"));
        assert_eq!(body["host_link"]["mode"], json!("file"));
        assert_eq!(
            body["host_link"]["socket"],
            Value::Null,
            "process 目标不使用 socket"
        );
        assert_eq!(body["host_link"]["connected"], json!(true));
        assert_eq!(reason_of(&body, "host_link.socket"), PROCESS_TARGET_REASON);
        // process 目标没有 rt-host：运行时 mode null + 同一条目标原因。
        assert_eq!(body["mode"], Value::Null);
        assert_eq!(reason_of(&body, "mode"), PROCESS_TARGET_REASON);
        let not_wired = body["not_wired"].as_array().expect("not_wired");
        let control = not_wired
            .iter()
            .find(|entry| entry["path"] == json!("host_link.control"))
            .expect("process 目标必须把控制通道列进 not_wired");
        assert!(
            control["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("KHAEN_RT_TARGET=process")),
            "{control}"
        );
    }

    #[test]
    fn process_target_without_a_status_file_is_mode_none_but_daemon_fields_stay_real() {
        let dir = TempDir::new("process-target-none");
        let path = dir.file("missing.json");
        let monitor = monitor_not_wired();
        let calibration = not_wired_calibration();
        let body = aggregate_with_sources(
            &path,
            &monitor,
            &calibration,
            Target::Process,
            &LinkSnapshot::Process,
            SystemTime::now(),
        );
        assert_daemon_fields(&body);
        assert_eq!(body["host_link"]["target"], json!("daemon-process"));
        assert_eq!(body["host_link"]["mode"], json!("none"));
        assert_eq!(body["host_link"]["connected"], json!(false));
        assert!(reason_of(&body, "host_link.connected").contains("KHAEN_RT_TARGET=process"));
        assert_rt_host_absent(&body);
    }

    #[test]
    fn link_capability_reports_socket_file_none_and_process_truthfully() {
        let dir = TempDir::new("link-capability");
        let path = dir.file("rt-host-status.json");
        let now = SystemTime::now();

        // socket：连通、mode=socket、无错误。
        let socket_link = LinkSnapshot::Socket {
            socket: PathBuf::from(TEST_SOCKET),
            status: full_socket_status(),
            age_ms: Some(42),
        };
        let socket_cap = link_capability_at(Target::RtHost, &socket_link, &path, now);
        assert_eq!(socket_cap["target"], json!("rt-host"));
        assert_eq!(socket_cap["mode"], json!("socket"));
        assert_eq!(socket_cap["connected"], json!(true));
        assert_eq!(socket_cap["socket"], json!(TEST_SOCKET));
        assert_eq!(socket_cap["error"], Value::Null);

        // socket 不可用 + 新鲜文件：mode=file（connected 仍为 true，因为文件是真实退化来源）。
        write_document(&path, &full_document());
        let unavailable = unavailable_link("connect refused");
        let file_cap = link_capability_at(Target::RtHost, &unavailable, &path, now);
        assert_eq!(file_cap["mode"], json!("file"));
        assert_eq!(file_cap["connected"], json!(true));
        assert!(file_cap["error"]
            .as_str()
            .is_some_and(|error| error.contains("connect refused")));

        // socket 不可用 + 文件也没有：mode=none、connected=false。
        let missing_cap =
            link_capability_at(Target::RtHost, &unavailable, &dir.file("nope.json"), now);
        assert_eq!(missing_cap["mode"], json!("none"));
        assert_eq!(missing_cap["connected"], json!(false));

        // process：socket 为 null，target 标 daemon-process，并说明原因。
        let process_cap = link_capability_at(Target::Process, &LinkSnapshot::Process, &path, now);
        assert_eq!(process_cap["target"], json!("daemon-process"));
        assert_eq!(process_cap["socket"], Value::Null);
        assert_eq!(process_cap["mode"], json!("file"));
        assert!(process_cap["error"]
            .as_str()
            .is_some_and(|error| error.contains("KHAEN_RT_TARGET=process")));
    }
}
