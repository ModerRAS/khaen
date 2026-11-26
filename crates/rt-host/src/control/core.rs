//! 控制协议核心（平台无关）：契约 B 的 op 状态机，不碰 socket、不碰 Runtime。
//!
//! 线程位置：控制服务端的每连接线程持锁调用 [ControlCore::handle_line]（多连接串行化）。
//! 重活（读方案文件、解码、准入、图编译、surface 补丁、回滚重编译）都在控制线程做；
//! 只有"块边界动作"（静音、换图）通过命令通道交给块线程，并等一次性 ack。
//!
//! # 与 host::control::ControlState 的关系
//!
//! 契约 B 的响应形状与错误码以 host 的 ControlState 为权威参考；本核心复用它公开的
//! helper（engine_token/budget_json/format_json/apply_surface_patch/compile_error_to_control），
//! 所以 daemon 下一任务的代理可以把这里的响应原样转发（同一份字段形状）。
//!
//! 差异（如实）：
//! - 这里没有自己的执行器：真正生效要经块线程，所以 commit 的 applied_block 来自块线程回执；
//! - 本进程的数据面执行器/设备格式在进程生命周期内固定：format 两阶段只改控制面编译格式，
//!   commit 如实报 device_restart_required=true / pcm_reconfigured=false（M3b 热重配未实现）；
//!   此后 prepare/commit 计划会因数据面格式不匹配被块线程如实拒绝（not_applied）。
//! - status 带快照年龄（age_ms/stale），图信息来自块线程发布的快照（不阻塞块线程）。

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::Duration;

use khaen_core::rt::{BLOCK_SIZES, SAMPLE_RATES_HZ};
use khaen_core::sha256::sha256;
use khaen_host::admission::{admit, AdmissionLimits};
use khaen_host::control::{
    apply_surface_patch, budget_json, compile_error_to_control, engine_token, error, format_json,
    string_field, ControlError, RuntimeMode, MAX_REQUEST_BYTES, NOT_WIRED_OPS, NOT_WIRED_SURFACES,
    PROTOCOL_VERSION, WIRED_OPS, WIRED_SURFACES,
};
use khaen_host::engine::EngineConfig;
use khaen_host::plan::Plan;
use khaen_host::runtime::{compile_plan_draft, PlanDraft, RuntimeError};
use serde_json::{json, Map, Value};

use super::command::{CommandReply, ControlCommand};
use super::snapshot::SharedSnapshot;

/// commit/mute 等块边界动作等一次 ack 的上限。
///
/// 正常块周期是毫秒级；设备缺失期间块循环以 50 ms 小片轮询命令，5 s 足够宽裕。
/// 超时如实回 not_applied（不得假报成功）。
pub const COMMAND_ACK_TIMEOUT: Duration = Duration::from_secs(5);

/// 已生效方案历史上限（与 host ControlState 一致：最近 16 个版本）。
const HISTORY_LIMIT: usize = 16;

/// ControlCore 的构造参数。
pub struct ControlCoreConfig {
    /// 数据面执行器格式（进程内固定；初始也是控制面编译格式）。
    pub data_plane_format: EngineConfig,
    /// 准入预算。
    pub limits: AdmissionLimits,
    /// 块边界命令的发送端（块线程持有接收端）。
    pub commands: Sender<ControlCommand>,
    /// 块线程发布快照的共享槽。
    pub snapshot: SharedSnapshot,
    /// 初始运行模式（rt-host 默认 aes67：输出使能，输入源固定 aes67）。
    pub mode: RuntimeMode,
    /// 启动时已 apply 的方案（作为语义界面补丁基线与回滚历史的第一项）。
    pub initial_plan: Option<Plan>,
    /// 启动方案对应的版本号（没有方案时为 0）。
    pub initial_version: u64,
    /// ack 等待上限。
    pub command_timeout: Duration,
}

/// prepare 产出的方案草稿：token、编译好的图、方案对象（生效后成为基线/历史）。
struct DraftPlan {
    /// 确定性 engine_token（版本号 + 文件内容 sha256 前 8 字节）。
    token: String,
    /// 方案版本号；commit 生效后成为 actual_version。
    version: u64,
    /// 方案 graph_id（ABI v3 的 sha256 十六进制串）。
    graph_id: String,
    /// prepare 响应；同 token 重复 prepare 直接返回同一份（幂等）。
    response: Value,
    /// 方案对象：commit 成功后成为 surface 补丁基线与回滚历史条目。
    plan: Plan,
    /// 编译好的图；commit 时移交给块线程的 SlotEngine。
    draft: PlanDraft,
}

/// 格式变更草稿（两阶段；与方案/模式草稿互斥）。
struct FormatDraft {
    token: String,
    version: u64,
    /// 目标控制面编译格式（只允许 sample_rate / block_frames 改变）。
    target: EngineConfig,
    /// prepare 响应；同 token 重复 prepare 返回同一份。
    response: Value,
}

/// 模式变更草稿（两阶段；与方案/格式草稿互斥）。
struct ModeDraft {
    token: String,
    version: u64,
    target: RuntimeMode,
    changed: bool,
    response: Value,
}

/// 一份已生效方案的历史条目（完整方案 + 版本 + graph_id）。
#[derive(Clone)]
struct CommittedPlan {
    version: u64,
    plan: Plan,
}

/// 控制面状态机：唯一执行器在块线程，这里只持有协议状态与"待生效草稿"。
pub struct ControlCore {
    /// 控制面编译格式（format op 可改；初始等于数据面格式）。
    engine_config: EngineConfig,
    /// 数据面执行器格式（进程内固定）。
    data_plane_format: EngineConfig,
    limits: AdmissionLimits,
    commands: Sender<ControlCommand>,
    snapshot: SharedSnapshot,
    command_timeout: Duration,
    /// 当前运行模式（off = 输出整链静音；aes67 = 使能，输入源固定 aes67）。
    mode: RuntimeMode,
    /// 已分配/生效的最大方案版本（单调递增；回滚不复用版本号）。
    current_version: u64,
    draft: Option<DraftPlan>,
    pending_format: Option<FormatDraft>,
    pending_mode: Option<ModeDraft>,
    /// 最近一次 commit 生效的方案：语义界面补丁基线。
    base_plan: Option<Plan>,
    /// 已生效方案有限历史（版本升序）。
    history: Vec<CommittedPlan>,
}

impl std::fmt::Debug for ControlCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlCore")
            .field("engine_config", &self.engine_config)
            .field("data_plane_format", &self.data_plane_format)
            .field("mode", &self.mode)
            .field("current_version", &self.current_version)
            .field("has_draft", &self.draft.is_some())
            .field("pending_format", &self.pending_format.is_some())
            .field("pending_mode", &self.pending_mode.is_some())
            .field("base_plan", &self.base_plan.is_some())
            .field("history_len", &self.history.len())
            .finish_non_exhaustive()
    }
}

/// 从请求对象里取字符串字段的同款 helper（复用 host 的实现，错误形状一致）。
fn bad_request(message: impl Into<String>) -> ControlError {
    ControlError::new(error::BAD_REQUEST, message)
}

fn token_invalid(message: impl Into<String>) -> ControlError {
    ControlError::new(error::ENGINE_TOKEN_INVALID, message)
}

fn not_applied(message: impl Into<String>) -> ControlError {
    ControlError::new(error::NOT_APPLIED, message)
}

fn admission_rejected(message: impl Into<String>) -> ControlError {
    ControlError::new(error::ADMISSION_REJECTED, message)
}

/// 草稿编译错误 -> 契约 B 错误码（与 host ControlState 同一映射）。
fn draft_error(err: RuntimeError) -> ControlError {
    match err {
        RuntimeError::Admission(admission) => admission_rejected(format!(
            "plan rejected by admission: {}",
            admission.summary()
        )),
        RuntimeError::Compile(err) => compile_error_to_control(err),
        RuntimeError::Plan(err) => bad_request(format!("cannot decode plan: {err}")),
        other => bad_request(format!("plan cannot be compiled: {other}")),
    }
}

impl ControlCore {
    /// 新建控制核心。
    pub fn new(config: ControlCoreConfig) -> Self {
        let initial_plan = config.initial_plan;
        let history = initial_plan
            .as_ref()
            .map(|plan| {
                vec![CommittedPlan {
                    version: config.initial_version,
                    plan: plan.clone(),
                }]
            })
            .unwrap_or_default();
        Self {
            engine_config: config.data_plane_format.clone(),
            data_plane_format: config.data_plane_format,
            limits: config.limits,
            commands: config.commands,
            snapshot: config.snapshot,
            command_timeout: config.command_timeout,
            mode: config.mode,
            current_version: config.initial_version,
            draft: None,
            pending_format: None,
            pending_mode: None,
            base_plan: initial_plan,
            history,
        }
    }

    /// 处理一行请求，返回一行响应（不含换行）。所有错误都映射成稳定错误对象。
    pub fn handle_line(&mut self, line: &str) -> String {
        match self.dispatch(line) {
            Ok(value) => value.to_string(),
            Err(err) => err.to_json().to_string(),
        }
    }

    /// 下一个方案版本号（准备阶段分配；commit 生效前不消耗）。
    fn next_version(&self) -> u64 {
        self.current_version.saturating_add(1)
    }

    /// 读共享快照（克隆；控制线程允许分配，不阻塞块线程）。
    fn snapshot(&self) -> Option<super::snapshot::RuntimeSnapshot> {
        self.snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 发送一条块边界命令并等一次性 ack（超时/通道关闭如实回 not_applied）。
    fn send(
        &self,
        build: impl FnOnce(Sender<CommandReply>) -> ControlCommand,
    ) -> Result<CommandReply, ControlError> {
        let (ack, replies) = mpsc::channel();
        self.commands.send(build(ack)).map_err(|_| {
            not_applied(
                "rt-host block loop is not running: control command channel is closed; nothing was applied",
            )
        })?;
        match replies.recv_timeout(self.command_timeout) {
            Ok(reply) => Ok(reply),
            Err(RecvTimeoutError::Timeout) => Err(not_applied(format!(
                "rt-host block loop did not acknowledge the block-boundary action within {:?}; nothing is reported as applied",
                self.command_timeout
            ))),
            Err(RecvTimeoutError::Disconnected) => Err(not_applied(
                "rt-host block loop stopped before acknowledging the block-boundary action; nothing is reported as applied",
            )),
        }
    }

    fn dispatch(&mut self, line: &str) -> Result<Value, ControlError> {
        if line.len() > MAX_REQUEST_BYTES {
            return Err(bad_request(format!(
                "request exceeds {MAX_REQUEST_BYTES} bytes"
            )));
        }
        if line.trim().is_empty() {
            return Err(bad_request("empty request"));
        }
        let value: Value = serde_json::from_str(line)
            .map_err(|err| bad_request(format!("invalid json: {err}")))?;
        let object = value
            .as_object()
            .ok_or_else(|| bad_request("request must be a json object"))?;
        let op = match object.get("op") {
            Some(Value::String(op)) => op.clone(),
            Some(_) => return Err(bad_request("field op must be a string")),
            None => return Err(bad_request("missing field: op")),
        };
        match op.as_str() {
            "status" => Ok(self.status()),
            "mute" => self.mute(object),
            "prepare" => {
                let plan_path = string_field(object, "plan_path")?;
                let plan_sha256 = string_field(object, "plan_sha256")?;
                self.prepare_json(plan_path, plan_sha256)
            }
            "commit" => {
                let engine_token = string_field(object, "engine_token")?;
                self.commit_json(engine_token)
            }
            "abort" => {
                let engine_token = string_field(object, "engine_token")?;
                self.abort_json(engine_token)
            }
            "coeff" => Ok(self.coeff_json()),
            "format" => self.prepare_format_json(object),
            "rollback" => {
                let target = match object.get("target") {
                    Some(Value::Number(number)) => Some(number.as_u64().ok_or_else(|| {
                        bad_request("field target must be a non-negative integer version")
                    })?),
                    Some(Value::Null) | None => None,
                    Some(_) => {
                        return Err(bad_request(
                            "field target must be a non-negative integer version",
                        ))
                    }
                };
                self.rollback_json(target)
            }
            "inspect" => {
                let plan_path = string_field(object, "plan_path")?;
                let plan_sha256 = match object.get("plan_sha256") {
                    Some(Value::String(sha)) => Some(sha.as_str()),
                    Some(_) => return Err(bad_request("field plan_sha256 must be a string")),
                    None => None,
                };
                self.inspect_json(plan_path, plan_sha256)
            }
            "surface" => {
                let surface = string_field(object, "surface")?;
                let data = object.get("data").cloned().unwrap_or_else(|| json!({}));
                self.prepare_surface_json(surface, &data)
            }
            other => {
                if let Some((_, reason)) = NOT_WIRED_OPS.iter().find(|(name, _)| *name == other) {
                    Err(ControlError::new(
                        error::UNSUPPORTED,
                        format!("op {other} not wired: {reason}"),
                    ))
                } else {
                    Err(ControlError::new(
                        error::UNKNOWN_OP,
                        format!("unknown op: {other}"),
                    ))
                }
            }
        }
    }

    // ---------------------------------------------------------------
    // status / coeff
    // ---------------------------------------------------------------

    fn status(&self) -> Value {
        let Some(snapshot) = self.snapshot() else {
            let mut reasons = Map::new();
            reasons.insert(
                "snapshot".to_string(),
                json!("the control channel is up before the block loop published its first runtime snapshot; retry status"),
            );
            return json!({
                "ok": true,
                "protocol_version": PROTOCOL_VERSION,
                "runtime_state": "starting",
                "graph_id": Value::Null,
                "epoch": Value::Null,
                "blocks": Value::Null,
                "xruns": Value::Null,
                "underruns": Value::Null,
                "faults": Value::Null,
                "silent": Value::Null,
                "mute": Value::Null,
                "safe_mode": Value::Null,
                "actual_version": Value::Null,
                "mode": self.mode.as_str(),
                "format": format_json(&self.engine_config),
                "control_format": format_json(&self.engine_config),
                "format_mismatch": false,
                "device_restart_required": false,
                "switch_pending": Value::Null,
                "plan_rate": Value::Null,
                "device_rate": Value::Null,
                "rate_asserted": Value::Null,
                "last_error": Value::Null,
                "output_errors": Value::Null,
                "output_started": Value::Null,
                "device_rate": Value::Null,
                "device": Value::Null,
                "device_state": Value::Null,
                "absent_since": Value::Null,
                "reopen_attempts": Value::Null,
                "last_device_error": Value::Null,
                "wired": WIRED_OPS,
                "not_wired": [],
                "surfaces": surfaces_json(),
                "surface_reasons": surface_reasons_json(),
                "age_ms": Value::Null,
                "stale": Value::Null,
                "snapshot_interval_ms": Value::Null,
                "reasons": reasons,
                "notes": notes_json(),
            });
        };
        let status = &snapshot.status;
        let graph = &status.graph;
        let lifecycle = &snapshot.lifecycle;
        let format_mismatch = self.engine_config != self.data_plane_format;
        let silent = status.muted || status.state.requires_mute() || lifecycle.is_absent();
        let safe_mode = status.state.requires_mute() || lifecycle.is_absent();
        let mut reasons = Map::new();
        if graph.graph_id.is_none() {
            reasons.insert(
                "graph_id".to_string(),
                json!("no plan applied to the running engine yet: prepare + commit a plan; the graph switches at the next block boundary"),
            );
        }
        if graph.epoch.is_none() {
            reasons.insert(
                "epoch".to_string(),
                json!("no graph switch has taken effect yet; epoch counts applied plan switches"),
            );
        }
        if graph.version.is_none() {
            reasons.insert(
                "actual_version".to_string(),
                json!("no plan applied to the running engine yet; actual_version is the version of the running graph"),
            );
        }
        if silent && !status.muted {
            reasons.insert(
                "silent".to_string(),
                json!("output is silent because the safety/device state requires mute; the user mute flag is separate"),
            );
        }
        if format_mismatch {
            reasons.insert(
                "device_restart_required".to_string(),
                json!("the control-plane compile format was changed via the format op; the running rt-host data plane keeps its process-lifetime engine/device format until the process is restarted (M3b hot reconfiguration is not implemented)"),
            );
        }
        if lifecycle.is_absent() {
            reasons.insert(
                "safe_mode".to_string(),
                json!("output device is absent: the block loop is fail-closed (no blocks run, nothing is written to the device) and retries the device with backoff; the process stays up"),
            );
        }
        let device = &status.device;
        json!({
            "ok": true,
            "protocol_version": PROTOCOL_VERSION,
            "runtime_state": status.state.as_str(),
            "graph_id": graph.graph_id,
            "epoch": graph.epoch,
            "blocks": status.blocks,
            "xruns": status.output.xruns,
            "underruns": status.input.underruns,
            "faults": snapshot.faults,
            "silent": silent,
            "mute": status.muted,
            "safe_mode": safe_mode,
            "actual_version": graph.version,
            "mode": self.mode.as_str(),
            "format": format_json(&snapshot.format),
            "control_format": format_json(&self.engine_config),
            "format_mismatch": format_mismatch,
            "device_restart_required": format_mismatch,
            "switch_pending": status.switch_pending,
            "plan_rate": status.plan_rate,
            "device_rate": status.device_rate,
            "rate_asserted": status.rate_asserted,
            "last_error": status.last_error,
            "output_errors": status.output_errors,
            "output_started": status.output_started,
            "input": {
                "packets": status.input.packets,
                "frames_filled": status.input.frames_filled,
                "lost": status.input.lost,
                "late": status.input.late,
                "underruns": status.input.underruns,
                "overruns": status.input.overruns,
                "malformed": status.input.malformed,
                "jitter_fill": status.input.jitter_fill,
                "ssrc": status.input.ssrc,
            },
            "output": {
                "xruns": status.output.xruns,
                "blocks_written": status.output.blocks_written,
                "bytes_written": status.output.bytes_written,
            },
            "receiver": {
                "packets_received": status.receiver.packets_received,
                "bytes_received": status.receiver.bytes_received,
                "queue_full_drops": status.receiver.queue_full_drops,
                "recv_errors": status.receiver.recv_errors,
                "running": status.receiver.running,
            },
            "device": {
                "state": device.state.as_str(),
                "plan_rate": device.plan_rate,
                "device_rate": device.device_rate,
                "rate_asserted": device.rate_asserted,
                "last_error": device.last_error.as_ref().map(|err| err.code.as_str()),
                "attempts": device.attempts,
                "since_ms": device.since_ms,
            },
            "device_state": if lifecycle.is_absent() { "absent" } else { "present" },
            "absent_since": lifecycle.absent_since_ms(),
            "reopen_attempts": lifecycle.reopen_attempts(),
            "last_device_error": lifecycle.last_device_error(),
            "wired": WIRED_OPS,
            "not_wired": NOT_WIRED_OPS
                .iter()
                .map(|(op, reason)| json!({ "op": op, "reason": reason }))
                .collect::<Vec<Value>>(),
            "surfaces": surfaces_json(),
            "surface_reasons": surface_reasons_json(),
            "age_ms": snapshot.age_ms(),
            "stale": snapshot.stale(),
            "snapshot_interval_ms": snapshot.publish_period_ms,
            "reasons": reasons,
            "notes": notes_json(),
        })
    }

    /// 当前系数/图版本（coeff）：只报快照里真实存在的值，没有就 null + reasons，不编数。
    pub fn coeff_json(&self) -> Value {
        let snapshot = self.snapshot();
        let mut reasons = Map::new();
        let (version, graph_id, applied_block, age_ms) = match &snapshot {
            Some(snapshot) => {
                let graph = &snapshot.status.graph;
                (
                    graph.version,
                    graph.graph_id.clone(),
                    graph.applied_block,
                    json!(snapshot.age_ms()),
                )
            }
            None => (None, None, None, Value::Null),
        };
        if version.is_some() {
            reasons.insert(
                "last_good_version".to_string(),
                json!("whole-plan switching only: the running graph is the last good version; coefficient-level revisions land with T22"),
            );
        } else {
            reasons.insert(
                "version".to_string(),
                json!("no plan applied to the running engine yet: prepare + commit, then run one block; the graph switches at the next block boundary"),
            );
            reasons.insert(
                "last_good_version".to_string(),
                json!("no graph has been applied yet, so there is no last-good version"),
            );
            reasons.insert("graph_id".to_string(), json!("no plan applied yet"));
            reasons.insert(
                "applied_block".to_string(),
                json!("no plan applied yet; applied_block is the block where the running graph took over"),
            );
        }
        if snapshot.is_none() {
            reasons.insert(
                "snapshot".to_string(),
                json!("the block loop has not published a runtime snapshot yet"),
            );
        }
        let available_versions: Vec<u64> = self.history.iter().map(|entry| entry.version).collect();
        json!({
            "ok": true,
            "version": version,
            "last_good_version": version,
            "graph_id": graph_id,
            "applied_block": applied_block,
            "available_versions": available_versions,
            "age_ms": age_ms,
            "reasons": reasons,
        })
    }

    // ---------------------------------------------------------------
    // mute / mode
    // ---------------------------------------------------------------

    fn mute(&mut self, object: &Map<String, Value>) -> Result<Value, ControlError> {
        let mute = match object.get("mute") {
            Some(Value::Bool(mute)) => *mute,
            Some(_) => return Err(bad_request("field mute must be a boolean")),
            None => return Err(bad_request("missing field: mute")),
        };
        let reply = self.send(|ack| ControlCommand::Mute { mute, ack })?;
        match reply {
            CommandReply::Mute {
                mute,
                applied_block,
                idempotent,
            } => Ok(json!({
                "ok": true,
                "mute": mute,
                "applied_block": applied_block,
                "idempotent": idempotent,
            })),
            other => Err(unexpected_reply(other)),
        }
    }

    /// 模式界面第一阶段（surface=mode）：校验目标模式 -> 存草稿；commit 才在块边界生效。
    pub fn prepare_mode_json(&mut self, data: &Value) -> Result<Value, ControlError> {
        let object = data
            .as_object()
            .ok_or_else(|| bad_request("field data must be a json object"))?;
        let field = object
            .get("mode")
            .and_then(Value::as_str)
            .ok_or_else(|| bad_request("field data.mode must be a string"))?;
        let target = match field {
            "off" => RuntimeMode::Off,
            "on" | "aes67" => RuntimeMode::Aes67,
            other => {
                return Err(bad_request(format!(
                    "mode {other:?} is not wired: supported are off/on/aes67 (toslink/dante input sources are not implemented)"
                )))
            }
        };
        // 与方案/格式草稿互斥：模式提交不依赖方案，但混用会让 commit 出歧义。
        self.draft = None;
        self.pending_format = None;
        let version = self.next_version();
        let canonical = format!("mode:{}", target.as_str());
        let token = engine_token(version, &sha256(canonical.as_bytes()));
        let changed = target != self.mode;
        if let Some(draft) = &self.pending_mode {
            if draft.token == token {
                return Ok(draft.response.clone());
            }
        }
        let response = json!({
            "ok": true,
            "surface": "mode",
            "engine_token": token,
            "planned_version": version,
            "mode": target.as_str(),
            "changed": changed,
            "applies_to": "control-plane engine (output enable)",
            "note": "real capture-source switching belongs to the data plane/device supervisor (T70g); toslink/dante are not implemented",
        });
        self.pending_mode = Some(ModeDraft {
            token,
            version,
            target,
            changed,
            response: response.clone(),
        });
        Ok(response)
    }

    /// 模式界面第二阶段：off = 块边界静音整链；aes67/on = 解除静音（真机经块线程生效）。
    fn commit_mode(&mut self, engine_token: &str) -> Result<Value, ControlError> {
        let draft = self.pending_mode.take().ok_or_else(|| {
            token_invalid("no prepared mode draft: call surface mode before commit")
        })?;
        if draft.token != engine_token {
            let message = format!(
                "engine_token mismatch: prepared token {}, request {engine_token}",
                draft.token
            );
            self.pending_mode = Some(draft);
            return Err(token_invalid(message));
        }
        if !draft.changed {
            return Ok(json!({
                "ok": true,
                "changed": false,
                "applied_block": Value::Null,
                "version": draft.version,
                "mode": draft.target.as_str(),
                "note": "mode already matches; nothing applied",
            }));
        }
        let mute = draft.target == RuntimeMode::Off;
        let reply = self.send(|ack| ControlCommand::Mute { mute, ack })?;
        let applied_block = match reply {
            CommandReply::Mute { applied_block, .. } => applied_block,
            other => return Err(unexpected_reply(other)),
        };
        self.mode = draft.target;
        Ok(json!({
            "ok": true,
            "changed": true,
            "applied_block": applied_block,
            "version": draft.version,
            "mode": self.mode.as_str(),
        }))
    }

    // ---------------------------------------------------------------
    // prepare / commit / abort / surface / rollback
    // ---------------------------------------------------------------

    /// 方案加载（prepare）：读文件 -> 校验 sha256 -> 解码 -> 准入 -> 图编译 -> 存草稿并生成 token。
    ///
    /// 非实时路径（控制线程）：允许文件 IO 与堆分配；不改变正在跑的音频（commit 才生效）。
    /// 幂等/替换语义与 host ControlState 一致：同 token 重复 prepare 返回同一份响应；
    /// 不同 token 的 prepare 覆盖旧草稿。
    pub fn prepare_json(
        &mut self,
        plan_path: &str,
        plan_sha256: &str,
    ) -> Result<Value, ControlError> {
        let bytes = std::fs::read(plan_path)
            .map_err(|err| bad_request(format!("cannot read plan file {plan_path}: {err}")))?;
        let actual_sha = khaen_host::sha256_hex(&bytes);
        if !actual_sha.eq_ignore_ascii_case(plan_sha256.trim()) {
            return Err(bad_request(format!(
                "plan sha256 mismatch: request {plan_sha256}, file {plan_path} is {actual_sha}"
            )));
        }
        let version = self.next_version();
        let token = engine_token(version, &sha256(&bytes));
        if let Some(draft) = &self.draft {
            if draft.token == token {
                return Ok(draft.response.clone());
            }
        }
        let plan = Plan::decode(&bytes)
            .map_err(|err| bad_request(format!("cannot decode plan {plan_path}: {err}")))?;
        self.store_plan_draft(plan, version, token, None)
    }

    /// 语义界面（surface）第一阶段：以最近一次生效的方案为基线打补丁 -> 准入/编译 -> 存草稿。
    ///
    /// 支持的界面见 WIRED_SURFACES；format/mode 委托给各自的两阶段入口（同一个 commit/abort）。
    /// 未接通的界面返回 unsupported 并带原因，不返回假成功。
    pub fn prepare_surface_json(
        &mut self,
        surface: &str,
        data: &Value,
    ) -> Result<Value, ControlError> {
        if let Some((_, reason)) = NOT_WIRED_SURFACES.iter().find(|(name, _)| *name == surface) {
            return Err(ControlError::new(
                error::UNSUPPORTED,
                format!("surface {surface} not wired: {reason}"),
            ));
        }
        if surface == "format" {
            let object = data
                .as_object()
                .ok_or_else(|| bad_request("field data must be a json object"))?;
            return self.prepare_format_json(object);
        }
        if surface == "mode" {
            return self.prepare_mode_json(data);
        }
        if !WIRED_SURFACES.contains(&surface) {
            return Err(bad_request(format!("unknown surface: {surface}")));
        }
        let base = self.base_plan.clone().ok_or_else(|| {
            ControlError::new(
                error::NO_BASE_PLAN,
                "no committed plan to patch: prepare + commit a plan first",
            )
        })?;
        let patched = apply_surface_patch(&base, surface, data)?;
        let bytes = patched
            .encode_checked()
            .map_err(|err| bad_request(format!("patched plan cannot be encoded: {err}")))?;
        let version = self.next_version();
        let token = engine_token(version, &sha256(&bytes));
        if let Some(draft) = &self.draft {
            if draft.token == token {
                return Ok(draft.response.clone());
            }
        }
        self.store_plan_draft(patched, version, token, Some(surface))
    }

    /// 把方案对象编译成草稿并保存（prepare / surface / rollback 共用）。
    fn store_plan_draft(
        &mut self,
        plan: Plan,
        version: u64,
        token: String,
        surface: Option<&str>,
    ) -> Result<Value, ControlError> {
        let admission = admit(&plan, &self.limits);
        if !admission.accepted {
            let mut message = format!("plan rejected by admission: {}", admission.summary());
            if let Some(surface) = surface {
                message = format!(
                    "surface {surface} patch rejected by admission: {}",
                    admission.summary()
                );
            }
            return Err(admission_rejected(message));
        }
        let draft = compile_plan_draft(&plan, &self.engine_config, &self.limits, version)
            .map_err(draft_error)?;
        let graph_id = plan.graph_id_hex();
        let mut response = json!({
            "ok": true,
            "engine_token": token,
            "planned_version": version,
            "graph_id": graph_id,
            "admission": {
                "accepted": true,
                "budget": budget_json(&admission.budget),
            },
        });
        if let Some(surface) = surface {
            if let Some(object) = response.as_object_mut() {
                object.insert("surface".to_string(), json!(surface));
            }
        }
        self.pending_format = None;
        self.pending_mode = None;
        self.draft = Some(DraftPlan {
            token,
            version,
            graph_id,
            response: response.clone(),
            plan,
            draft,
        });
        Ok(response)
    }

    /// 提交草稿：方案草稿 -> 块线程换图；模式草稿 -> 块边界静音；格式草稿 -> 控制面编译格式。
    pub fn commit_json(&mut self, engine_token: &str) -> Result<Value, ControlError> {
        if self.pending_mode.is_some() {
            return self.commit_mode(engine_token);
        }
        if self.pending_format.is_some() {
            return self.commit_format(engine_token);
        }
        let draft = self
            .draft
            .take()
            .ok_or_else(|| token_invalid("no prepared plan draft: call prepare before commit"))?;
        if draft.token != engine_token {
            let message = format!(
                "engine_token mismatch: prepared token {}, request {engine_token}",
                draft.token
            );
            // 错 token 不销毁草稿：放回去，调用方可以用正确 token 重试。
            self.draft = Some(draft);
            return Err(token_invalid(message));
        }
        // 数据面格式与控制面编译格式不一致时（format 已改控制面），任何草稿都无法在本进程生效：
        // 如实回 not_applied，不把格式不匹配的图塞进引擎。
        if self.engine_config != self.data_plane_format {
            let message = format!(
                "control-plane format {} Hz / {} frames / {} ch differs from the running data plane {} Hz / {} frames / {} ch; draft not applied (rt-host restart required, device_restart_required=true)",
                self.engine_config.sample_rate,
                self.engine_config.block_frames,
                self.engine_config.channels,
                self.data_plane_format.sample_rate,
                self.data_plane_format.block_frames,
                self.data_plane_format.channels
            );
            self.draft = Some(draft);
            return Err(not_applied(message));
        }
        let DraftPlan {
            version,
            graph_id,
            plan,
            draft: prepared,
            ..
        } = draft;
        let format = self.engine_config.clone();
        let reply = self.send(move |ack| ControlCommand::CommitPlan {
            draft: prepared,
            format,
            ack,
        })?;
        let applied_block = match reply {
            CommandReply::PlanApplied { applied_block } => applied_block,
            other => return Err(unexpected_reply(other)),
        };
        // 生效后：成为语义界面（volume/routing/layout）的补丁基线，并进入可回滚历史。
        self.base_plan = Some(plan.clone());
        self.push_history(CommittedPlan { version, plan });
        self.current_version = self.current_version.max(version);
        Ok(json!({
            "ok": true,
            "applied_block": applied_block,
            "version": version,
            "all_ack": true,
            "graph_id": graph_id,
        }))
    }

    /// 丢弃草稿（abort）：无副作用，正在跑的图不受影响；错 token 不销毁草稿。
    pub fn abort_json(&mut self, engine_token: &str) -> Result<Value, ControlError> {
        if let Some(draft) = self.pending_mode.as_ref() {
            if draft.token != engine_token {
                let message = format!(
                    "engine_token mismatch: prepared token {}, request {engine_token}",
                    draft.token
                );
                return Err(token_invalid(message));
            }
            self.pending_mode = None;
            return Ok(json!({ "ok": true, "aborted": true, "aborted_surface": "mode" }));
        }
        if let Some(draft) = self.pending_format.as_ref() {
            if draft.token != engine_token {
                let message = format!(
                    "engine_token mismatch: prepared token {}, request {engine_token}",
                    draft.token
                );
                return Err(token_invalid(message));
            }
            self.pending_format = None;
            return Ok(json!({ "ok": true, "aborted": true, "aborted_surface": "format" }));
        }
        let draft = self
            .draft
            .take()
            .ok_or_else(|| token_invalid("no prepared plan draft: call prepare before abort"))?;
        if draft.token != engine_token {
            let message = format!(
                "engine_token mismatch: prepared token {}, request {engine_token}",
                draft.token
            );
            self.draft = Some(draft);
            return Err(token_invalid(message));
        }
        Ok(json!({ "ok": true, "aborted": true }))
    }

    /// 回滚第一阶段（两阶段）：把有限历史里的旧版本重新准备成草稿，版本号递增、历史不改写。
    pub fn rollback_json(&mut self, target: Option<u64>) -> Result<Value, ControlError> {
        let Some(current) = self.history.last() else {
            return Err(ControlError::new(
                error::NO_HISTORY,
                "no committed plan history to roll back to: prepare + commit at least one plan first",
            ));
        };
        let target_version = match target {
            Some(version) => version,
            None => {
                if self.history.len() < 2 {
                    return Err(bad_request(
                        "nothing to roll back: only one committed plan version exists",
                    ));
                }
                self.history[self.history.len() - 2].version
            }
        };
        if target_version == current.version {
            return Err(bad_request(format!(
                "version {target_version} is already the running graph; rollback target must be an older version"
            )));
        }
        let available: Vec<u64> = self.history.iter().map(|entry| entry.version).collect();
        let entry = self
            .history
            .iter()
            .find(|entry| entry.version == target_version)
            .cloned()
            .ok_or_else(|| {
                bad_request(format!(
                    "version {target_version} is not in rollback history (available: {available:?})"
                ))
            })?;
        let plan = entry.plan;
        let bytes = plan
            .encode_checked()
            .map_err(|err| bad_request(format!("rollback plan cannot be encoded: {err}")))?;
        let version = self.next_version();
        let token = engine_token(version, &sha256(&bytes));
        if let Some(draft) = &self.draft {
            if draft.token == token {
                return Ok(draft.response.clone());
            }
        }
        let admission = admit(&plan, &self.limits);
        if !admission.accepted {
            return Err(admission_rejected(format!(
                "rollback plan rejected by admission: {}",
                admission.summary()
            )));
        }
        let draft = compile_plan_draft(&plan, &self.engine_config, &self.limits, version)
            .map_err(draft_error)?;
        let graph_id = plan.graph_id_hex();
        let response = json!({
            "ok": true,
            "rolled_back_to": target_version,
            "engine_token": token,
            "planned_version": version,
            "graph_id": graph_id,
            "admission": {
                "accepted": true,
                "budget": budget_json(&admission.budget),
            },
        });
        self.pending_format = None;
        self.pending_mode = None;
        self.draft = Some(DraftPlan {
            token,
            version,
            graph_id,
            response: response.clone(),
            plan,
            draft,
        });
        Ok(response)
    }

    // ---------------------------------------------------------------
    // format
    // ---------------------------------------------------------------

    /// 格式变更第一阶段（format op）：校验目标格式 -> 存草稿；commit 才改控制面编译格式。
    ///
    /// 真实边界：数据面执行器/设备格式在 rt-host 进程生命周期内固定（M3b 热重配未实现），
    /// 所以响应如实给 device_restart_required（目标 != 数据面格式时为 true）与 note；
    /// compile 后本进程的图无法在旧格式数据面上生效（commit 会回 not_applied）。
    pub fn prepare_format_json(
        &mut self,
        object: &Map<String, Value>,
    ) -> Result<Value, ControlError> {
        let mut target = self.engine_config.clone();
        let mut requested = false;
        if let Some(value) = object.get("sample_rate") {
            let sample_rate = value
                .as_u64()
                .ok_or_else(|| bad_request("field sample_rate must be a non-negative integer"))?;
            let sample_rate = u32::try_from(sample_rate)
                .map_err(|_| bad_request("sample_rate does not fit in u32"))?;
            if !SAMPLE_RATES_HZ.contains(&sample_rate) {
                return Err(bad_request(format!(
                    "sample_rate must be one of {SAMPLE_RATES_HZ:?}"
                )));
            }
            target.sample_rate = sample_rate;
            requested = true;
        }
        if let Some(value) = object.get("chunksize") {
            let chunksize = value
                .as_u64()
                .ok_or_else(|| bad_request("field chunksize must be a non-negative integer"))?;
            let chunksize = u16::try_from(chunksize)
                .map_err(|_| bad_request("chunksize does not fit in u16"))?;
            if !BLOCK_SIZES.contains(&chunksize) {
                return Err(bad_request(format!(
                    "chunksize must be one of {BLOCK_SIZES:?}"
                )));
            }
            target.block_frames = usize::from(chunksize);
            requested = true;
        }
        if !requested {
            return Err(bad_request(
                "at least one of sample_rate / chunksize is required",
            ));
        }
        // 格式草稿与方案/模式草稿互斥：方案草稿按旧格式编译，混用会让 commit 出歧义。
        self.draft = None;
        self.pending_mode = None;
        let version = self.next_version();
        let canonical = format!(
            "format:{}:{}:{}",
            target.sample_rate, target.block_frames, target.channels
        );
        let token = engine_token(version, &sha256(canonical.as_bytes()));
        if let Some(draft) = &self.pending_format {
            if draft.token == token {
                return Ok(draft.response.clone());
            }
        }
        let changed = target != self.engine_config;
        let device_restart_required = target != self.data_plane_format;
        let response = json!({
            "ok": true,
            "engine_token": token,
            "planned_version": version,
            "format": format_json(&target),
            "changed": changed,
            "applies_to": "control-plane engine",
            "device_restart_required": device_restart_required,
            "data_plane_format": format_json(&self.data_plane_format),
            "note": "format changes only rebuild the control-plane compile format in this process; the running data-plane engine/device format is fixed for the rt-host process lifetime, so a real change requires restarting khaen-rt-host with the new format (M3b hot reconfiguration is not implemented)",
        });
        self.pending_format = Some(FormatDraft {
            token,
            version,
            target,
            response: response.clone(),
        });
        Ok(response)
    }

    /// 格式变更第二阶段：只改控制面编译格式（数据面执行器不动，如实上报 device_restart_required）。
    fn commit_format(&mut self, engine_token: &str) -> Result<Value, ControlError> {
        let draft = self
            .pending_format
            .take()
            .ok_or_else(|| token_invalid("no prepared format draft: call format before commit"))?;
        if draft.token != engine_token {
            let message = format!(
                "engine_token mismatch: prepared token {}, request {engine_token}",
                draft.token
            );
            self.pending_format = Some(draft);
            return Err(token_invalid(message));
        }
        if draft.target == self.engine_config {
            let device_restart_required = self.engine_config != self.data_plane_format;
            return Ok(json!({
                "ok": true,
                "changed": false,
                "applied_block": Value::Null,
                "version": draft.version,
                "format": format_json(&draft.target),
                "data_plane_format": format_json(&self.data_plane_format),
                "device_restart_required": device_restart_required,
                "note": "control-plane format already matches; nothing rebuilt",
            }));
        }
        // 只重建控制面的编译格式：数据面执行器/设备在本进程生命周期内不变。
        self.engine_config = draft.target;
        let cleared_history = !self.history.is_empty();
        self.base_plan = None;
        self.history.clear();
        self.current_version = self.current_version.max(draft.version);
        let device_restart_required = self.engine_config != self.data_plane_format;
        Ok(json!({
            "ok": true,
            "changed": true,
            "applied_block": Value::Null,
            "version": draft.version,
            "format": format_json(&self.engine_config),
            "control_format": format_json(&self.engine_config),
            "data_plane_format": format_json(&self.data_plane_format),
            "applies_to": "control-plane engine",
            "all_ack": true,
            "graph_dropped": false,
            "control_graph_invalidated": true,
            "history_cleared": cleared_history,
            "mute_preserved": true,
            "device_restart_required": device_restart_required,
            "pcm_reconfigured": false,
            "note": "control-plane compile format updated and the running-plan baseline/history invalidated (plans embed the format); the running data-plane engine, its graph and the PCM stream are untouched, so a real format change still requires restarting khaen-rt-host (device_restart_required=true; M3b hot reconfiguration is not implemented)",
        }))
    }

    // ---------------------------------------------------------------
    // inspect
    // ---------------------------------------------------------------

    /// 诊断/试运行（inspect）：读文件 -> 校验（可选 sha）-> 解码 -> 准入 -> 图编译，
    /// 返回结果但**不产生草稿、不改变正在跑的图**（staged 恒 false）。
    pub fn inspect_json(
        &self,
        plan_path: &str,
        plan_sha256: Option<&str>,
    ) -> Result<Value, ControlError> {
        let bytes = std::fs::read(plan_path)
            .map_err(|err| bad_request(format!("cannot read plan file {plan_path}: {err}")))?;
        let actual_sha = khaen_host::sha256_hex(&bytes);
        if let Some(expected) = plan_sha256 {
            if !actual_sha.eq_ignore_ascii_case(expected.trim()) {
                return Err(bad_request(format!(
                    "plan sha256 mismatch: request {expected}, file {plan_path} is {actual_sha}"
                )));
            }
        }
        let plan = Plan::decode(&bytes)
            .map_err(|err| bad_request(format!("cannot decode plan {plan_path}: {err}")))?;
        let admission = admit(&plan, &self.limits);
        let rejections: Vec<Value> = admission
            .rejections
            .iter()
            .map(|rejection| json!({ "code": rejection.code, "detail": rejection.detail }))
            .collect();
        let budget = budget_json(&admission.budget);
        let compile = if admission.accepted {
            match compile_plan_draft(&plan, &self.engine_config, &self.limits, 0) {
                Ok(draft) => json!({
                    "compiled": true,
                    "latency_samples": draft.latency_samples(),
                }),
                Err(err) => {
                    let mapped = draft_error(err);
                    json!({
                        "compiled": false,
                        "error_code": mapped.code,
                        "message": mapped.message,
                    })
                }
            }
        } else {
            json!({
                "compiled": false,
                "error_code": error::ADMISSION_REJECTED,
                "message": admission.summary(),
            })
        };
        Ok(json!({
            "ok": true,
            "staged": false,
            "sha256": actual_sha,
            "graph_id": plan.graph_id_hex(),
            "plan": {
                "block_size": plan.block_size,
                "sample_rate": plan.sample_rate,
                "in_channels": plan.content.as_ref().map(|content| content.in_channels),
                "lanes": plan.content.as_ref().map(|content| content.out_lanes),
                "dac_channels": plan.routing.as_ref().map(|routing| routing.dac_channels),
            },
            "admission": {
                "accepted": admission.accepted,
                "rejections": rejections,
                "budget": budget,
            },
            "compile": compile,
        }))
    }

    /// 记录一个已生效版本到有限历史；超出上限丢最旧。
    fn push_history(&mut self, entry: CommittedPlan) {
        self.history.push(entry);
        if self.history.len() > HISTORY_LIMIT {
            self.history.remove(0);
        }
    }
}

/// 块线程回执与请求不匹配时的稳定错误。
///
/// 正常路径不会走到这里（命令类型与 ack 一一对应）；Rejected 也优先按块线程给的
/// 稳定错误码与说明返回（不把拒绝吞成泛化错误）。
fn unexpected_reply(reply: CommandReply) -> ControlError {
    match reply {
        CommandReply::Rejected { code, message } => ControlError::new(
            code,
            format!("rt-host block loop rejected the block-boundary action: {message}"),
        ),
        other => ControlError::new(
            error::NOT_APPLIED,
            format!("rt-host block loop returned an unexpected reply: {other:?}"),
        ),
    }
}

/// 已实现的语义界面（全部 true；未接通的在 NOT_WIRED_SURFACES 里）。
fn surfaces_json() -> Value {
    let mut surfaces = Map::new();
    for surface in WIRED_SURFACES {
        surfaces.insert((*surface).to_string(), json!(true));
    }
    for (surface, _reason) in NOT_WIRED_SURFACES {
        surfaces.insert((*surface).to_string(), json!(false));
    }
    Value::Object(surfaces)
}

/// 未接通界面的原因（当前为空对象）。
fn surface_reasons_json() -> Value {
    let mut reasons = Map::new();
    for (surface, reason) in NOT_WIRED_SURFACES {
        reasons.insert((*surface).to_string(), json!(reason));
    }
    Value::Object(reasons)
}

/// 控制面实现边界的固定说明（不随快照变化；status.notes）。
fn notes_json() -> Value {
    let mut notes = Map::new();
    notes.insert(
        "transport".to_string(),
        json!(
            "unix domain socket; one line JSON request -> one line JSON response (UTF-8, no BOM)"
        ),
    );
    notes.insert(
        "block_owner".to_string(),
        json!("the rt-host block loop is the only owner of the runtime engine; mute/commit take effect at block boundaries and applied_block is the block where they take effect"),
    );
    notes.insert(
        "format_live_change".to_string(),
        json!("the data-plane engine/device format is fixed for the rt-host process lifetime; format commit only updates the control-plane compile format and reports device_restart_required=true (M3b hot reconfiguration is not implemented)"),
    );
    notes.insert(
        "capture_source".to_string(),
        json!("mode is an output-enable switch over the currently implemented aes67 input; real capture-source switching (toslink/dante) is T70g and not implemented"),
    );
    Value::Object(notes)
}
