//! 主机控制协议（契约 B）：行 JSON 请求 / 行 JSON 响应。
//!
//! 这一层只做协议与方案加载，不碰 socket：`serve_loop` 接任意 `BufRead` + `Write`，
//! 因此可以在内存管道上跑完整协议测试，Unix socket 只负责把连接塞进来。
//!
//! # 实时边界
//!
//! `prepare` 允许 IO（读方案文件）与堆分配（解码 / 准入 / 图编译），但只在控制线程执行；
//! 它产出的草稿由 `commit` 交给执行器，由 [`SlotEngine`] 在下一块边界原子换图。
//! 音频线程（`run_block`）不读文件、不加锁、不分配、不打印。
//!
//! # 能力上报
//!
//! 已经能干的 op 进 `wired`，还没实现的进 `not_wired` 并带原因；未实现 op 的请求返回
//! `unsupported`，**不返回假成功**。

pub mod serve;

use std::collections::BTreeMap;

use khaen_core::plan::{Fold, Plan};
use khaen_core::rt::{BLOCK_SIZES, SAMPLE_RATES_HZ};
use khaen_core::sha256::{sha256, sha256_hex};
use serde_json::{json, Map, Value};

use crate::admission::{admit, AdmissionLimits, Budget};
use crate::backend::NullBackend;
use crate::engine::{EngineConfig, EngineError, Passthrough, Processor, SlotEngine};
use crate::graph::{CompileError, PlanGraph};

/// 协议版本（随契约追加递增）。
pub const PROTOCOL_VERSION: u16 = 1;

/// 单行请求的最大字节数；超过即拒绝处理。
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;

/// 已实现的 op。
pub const WIRED_OPS: &[&str] = &[
    "status", "mute", "prepare", "commit", "abort", "coeff", "surface", "inspect", "rollback",
    "format",
];

/// 可回滚历史的长度上限：只保留最近 N 个已生效方案（完整方案对象，控制线程内存）。
const HISTORY_LIMIT: usize = 16;

/// 已接通的语义界面（"surface" op 的 surface 字段可接受值）。
///
/// 两阶段语义与 prepare/commit 完全一致：surface 只**准备**一份打了补丁的方案草稿并返回
/// engine_token，真正生效仍由 commit 在下一块边界完成；abort 丢弃。
pub const WIRED_SURFACES: &[&str] = &["volume", "routing", "layout", "format", "mode"];

/// 尚未接通的界面及原因（如实上报；请求返回 `unsupported`，不返回假成功）。
///
/// 五个界面都已接通；这里保留机制，后续新增界面时先登记为未接通。
pub const NOT_WIRED_SURFACES: &[(&str, &str)] = &[];

/// 尚未实现的 op 及其原因（如实上报用）。
///
/// 目前全部 op 已接通；常量与 `reasons` / `unsupported` 机制保留，后续新增 op 时先把它放在这里，
/// 未实现前不得返回假成功。
pub const NOT_WIRED_OPS: &[(&str, &str)] = &[];

/// 协议错误码（契约 B.3）。
pub mod error {
    /// 请求不可解析或字段缺失。
    pub const BAD_REQUEST: &str = "bad_request";
    /// op 不存在。
    pub const UNKNOWN_OP: &str = "unknown_op";
    /// op 存在但当前未实现。
    pub const UNSUPPORTED: &str = "unsupported";
    /// 未生效（不得谎报成功）。
    pub const NOT_APPLIED: &str = "not_applied";
    /// 运行时没在跑。
    pub const RUNTIME_NOT_RUNNING: &str = "runtime_not_running";
    /// 准入被拒。
    pub const ADMISSION_REJECTED: &str = "admission_rejected";
    /// engine_token 无效。
    pub const ENGINE_TOKEN_INVALID: &str = "engine_token_invalid";
    /// 还没有已生效的方案可打补丁（surface op 前置条件）。
    pub const NO_BASE_PLAN: &str = "no_base_plan";
    /// 没有可回滚的历史版本（rollback 前置条件）。
    pub const NO_HISTORY: &str = "no_history";
}

/// 控制面错误：稳定错误码 + 给人看的说明。
///
/// REST 层按 `code` 映射 HTTP 状态码（见 `khaen-daemon`），行协议层直接序列化成错误响应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlError {
    /// 稳定错误码（见 [`error`]）。
    pub code: &'static str,
    /// 具体说明。
    pub message: String,
}

impl ControlError {
    /// 新建错误。
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 序列化为错误响应 JSON。
    pub fn to_json(&self) -> Value {
        json!({
            "ok": false,
            "error": self.code,
            "message": self.message,
        })
    }
}

type DispatchResult = Result<Value, ControlError>;

fn bad_request(message: impl Into<String>) -> ControlError {
    ControlError::new(error::BAD_REQUEST, message)
}

fn token_invalid(message: impl Into<String>) -> ControlError {
    ControlError::new(error::ENGINE_TOKEN_INVALID, message)
}

/// 从请求对象里取字符串字段；缺失或类型不对都给 `bad_request`。
pub fn string_field<'a>(
    object: &'a Map<String, Value>,
    name: &str,
) -> Result<&'a str, ControlError> {
    match object.get(name) {
        Some(Value::String(value)) => Ok(value.as_str()),
        Some(_) => Err(bad_request(format!("field {name} must be a string"))),
        None => Err(bad_request(format!("missing field: {name}"))),
    }
}

/// prepare 产出的草稿：已读文件、已校验 sha256、已解码、已准入、已编译成可执行图。
///
/// 草稿只存在于控制线程；commit 时把编译好的图交给执行器，abort / 被新 prepare 替换时直接析构。
struct DraftPlan {
    /// 确定性 engine_token（版本号 + 文件内容 sha256 前 8 字节）。
    token: String,
    /// 计划版本号；commit 生效后成为 `actual_version`。
    version: u64,
    /// 方案 graph_id（方案 ABI v3 的 sha256 十六进制串）。
    graph_id: String,
    /// 编译好的图；commit 时移交 `SlotEngine::stage_processor`。
    processor: Box<dyn Processor + Send>,
    /// prepare 响应；同一 token 重复 prepare 直接返回同一份（幂等）。
    response: Value,
    /// 草稿对应的方案；commit 成功后成为后续 surface 补丁的基线。
    plan: Plan,
}

impl core::fmt::Debug for DraftPlan {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DraftPlan")
            .field("token", &self.token)
            .field("version", &self.version)
            .field("graph_id", &self.graph_id)
            .finish_non_exhaustive()
    }
}

/// 确定性 engine_token：版本号 16 位十六进制 + 文件内容 sha256 前 16 位十六进制。
pub fn engine_token(version: u64, sha: &[u8; 32]) -> String {
    let mut prefix = [0u8; 8];
    prefix.copy_from_slice(&sha[..8]);
    format!("{:016x}{:016x}", version, u64::from_be_bytes(prefix))
}

/// 准入预算序列化（字段与 [`Budget`] 一一对应）。
pub fn budget_json(budget: &Budget) -> Value {
    json!({
        "plan_bytes": budget.plan_bytes,
        "coefficients": budget.coefficients,
        "matrix_cells": budget.matrix_cells,
        "matrix_taps_per_cell": budget.matrix_taps_per_cell,
        "convolution_taps": budget.convolution_taps,
        "latency_samples": budget.latency_samples,
        "lanes": budget.lanes,
        "dac_channels": budget.dac_channels,
        "state_samples": budget.state_samples,
        "memory_bytes": budget.memory_bytes,
    })
}

/// `PlanGraph::compile` 的失败映射（prepare 阶段，非实时）：
///
/// - **admission_rejected**：方案本身不能进实时图——准入拒绝、结构非法、抽头超限、
///   非 1 的抽取/插值（T22 才支持）、非有限系数。与 `admit` 拒绝同属"这份方案不行"。
/// - **bad_request**：方案与本次运行的运行时配置不匹配——块大小 / 采样率 / 通道数 / lane 链
///   不衔接。换一份匹配配置的方案即可，属于请求内容问题（`Engine` 表示运行时配置非法，
///   构造时已校验过，正常不可达）。
pub fn compile_error_to_control(err: CompileError) -> ControlError {
    let message = format!("plan graph compile failed: {err}");
    match err {
        CompileError::AdmissionRejected(_)
        | CompileError::TapsTooLong { .. }
        | CompileError::UnsupportedRates { .. }
        | CompileError::InvalidSection(_)
        | CompileError::NonFiniteCoefficient(_) => {
            ControlError::new(error::ADMISSION_REJECTED, message)
        }
        CompileError::Engine(_)
        | CompileError::BlockSizeMismatch { .. }
        | CompileError::SampleRateMismatch { .. }
        | CompileError::ChannelMismatch { .. }
        | CompileError::LaneMismatch { .. } => bad_request(message),
    }
}

/// 读取非负整数字段（JSON number 且必须能放进 u64）。
fn unsigned_field(object: &Map<String, Value>, name: &str) -> Result<u64, ControlError> {
    object
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| bad_request(format!("field {name} must be a non-negative integer")))
}

/// 读取有限浮点字段（拒绝 NaN/Infinity，避免把非法增益带进方案）。
fn finite_f64_field(object: &Map<String, Value>, name: &str) -> Result<f64, ControlError> {
    let value = object
        .get(name)
        .and_then(Value::as_f64)
        .ok_or_else(|| bad_request(format!("field {name} must be a finite number")))?;
    if !value.is_finite() {
        return Err(bad_request(format!("field {name} must be finite")));
    }
    Ok(value)
}

/// 把一段 `data` 对象应用到基线上，产出打了补丁的方案（只做字段搬运与类型检查，
/// 校验交给随后的 `encode_checked` / 准入 / 图编译——不在这一层重复实现契约校验）。
pub fn apply_surface_patch(base: &Plan, surface: &str, data: &Value) -> Result<Plan, ControlError> {
    let object = data
        .as_object()
        .ok_or_else(|| bad_request("field data must be a json object"))?;
    let mut plan = base.clone();
    match surface {
        "volume" => {
            let protection = plan.protection.as_mut().ok_or_else(|| {
                bad_request("plan has no protection section: volume needs per-lane gains")
            })?;
            let gains = object
                .get("gains_db")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    bad_request("field data.gains_db must be an array of {lane, gain_db}")
                })?;
            if gains.is_empty() {
                return Err(bad_request("data.gains_db must not be empty"));
            }
            for item in gains {
                let item = item
                    .as_object()
                    .ok_or_else(|| bad_request("data.gains_db entries must be objects"))?;
                let lane = u16::try_from(unsigned_field(item, "lane")?)
                    .map_err(|_| bad_request("lane does not fit in u16"))?;
                let gain_db = finite_f64_field(item, "gain_db")?;
                let entry = protection
                    .entries
                    .iter_mut()
                    .find(|entry| entry.lane == lane)
                    .ok_or_else(|| bad_request(format!("protection has no lane {lane}")))?;
                entry.gain_db = gain_db as f32;
            }
        }
        "routing" => {
            let routing = plan
                .routing
                .as_mut()
                .ok_or_else(|| bad_request("plan has no routing section"))?;
            let routes = object
                .get("routes")
                .and_then(Value::as_array)
                .ok_or_else(|| bad_request("field data.routes must be an array of [lane, dac]"))?;
            if routes.is_empty() {
                return Err(bad_request("data.routes must not be empty"));
            }
            let mut parsed = Vec::with_capacity(routes.len());
            for item in routes {
                let pair = item
                    .as_array()
                    .ok_or_else(|| bad_request("data.routes entries must be [lane, dac] arrays"))?;
                if pair.len() != 2 {
                    return Err(bad_request(
                        "data.routes entries must have exactly 2 elements",
                    ));
                }
                let lane = u16::try_from(
                    pair[0]
                        .as_u64()
                        .ok_or_else(|| bad_request("route lane must be a non-negative integer"))?,
                )
                .map_err(|_| bad_request("route lane does not fit in u16"))?;
                let dac = u16::try_from(
                    pair[1]
                        .as_u64()
                        .ok_or_else(|| bad_request("route dac must be a non-negative integer"))?,
                )
                .map_err(|_| bad_request("route dac does not fit in u16"))?;
                parsed.push((lane, dac));
            }
            if let Some(value) = object.get("dac_channels") {
                routing.dac_channels = u16::try_from(value.as_u64().ok_or_else(|| {
                    bad_request("field data.dac_channels must be a non-negative integer")
                })?)
                .map_err(|_| bad_request("dac_channels does not fit in u16"))?;
            }
            for (lane, dac) in &parsed {
                if *dac >= routing.dac_channels {
                    return Err(bad_request(format!(
                        "route lane {lane} -> dac {dac} is outside dac_channels {}",
                        routing.dac_channels
                    )));
                }
            }
            routing.routes = parsed;
        }
        "layout" => {
            let content = plan
                .content
                .as_mut()
                .ok_or_else(|| bad_request("plan has no content section"))?;
            content.in_channels = u16::try_from(unsigned_field(object, "in_channels")?)
                .map_err(|_| bad_request("in_channels does not fit in u16"))?;
            content.out_lanes = u16::try_from(unsigned_field(object, "out_lanes")?)
                .map_err(|_| bad_request("out_lanes does not fit in u16"))?;
            let folds = object
                .get("folds")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    bad_request("field data.folds must be an array of [src, dst, gain]")
                })?;
            if folds.is_empty() {
                return Err(bad_request("data.folds must not be empty"));
            }
            let mut parsed = Vec::with_capacity(folds.len());
            for item in folds {
                let triple = item.as_array().ok_or_else(|| {
                    bad_request("data.folds entries must be [src, dst, gain] arrays")
                })?;
                if triple.len() != 3 {
                    return Err(bad_request(
                        "data.folds entries must have exactly 3 elements",
                    ));
                }
                let src = u16::try_from(
                    triple[0]
                        .as_u64()
                        .ok_or_else(|| bad_request("fold src must be a non-negative integer"))?,
                )
                .map_err(|_| bad_request("fold src does not fit in u16"))?;
                let dst = u16::try_from(
                    triple[1]
                        .as_u64()
                        .ok_or_else(|| bad_request("fold dst must be a non-negative integer"))?,
                )
                .map_err(|_| bad_request("fold dst does not fit in u16"))?;
                let gain = triple[2]
                    .as_f64()
                    .ok_or_else(|| bad_request("fold gain must be a finite number"))?;
                if !gain.is_finite() {
                    return Err(bad_request("fold gain must be finite"));
                }
                parsed.push(Fold {
                    src,
                    dst,
                    gain: gain as f32,
                });
            }
            content.folds = parsed;
        }
        other => return Err(bad_request(format!("unknown surface: {other}"))),
    }
    Ok(plan)
}
/// 格式快照（status / format 响应用同一份字段名）。
pub fn format_json(config: &EngineConfig) -> Value {
    json!({
        "sample_rate": config.sample_rate,
        "block_size": config.block_frames,
        "channels": config.channels,
    })
}

/// 控制面状态：持有唯一执行器（[`SlotEngine`]）与至多一份方案草稿。
#[derive(Debug)]
pub struct ControlState {
    /// 块边界可换图的执行器（唯一 owner）。
    engine: SlotEngine,
    /// 准入预算（prepare 时使用）。
    limits: AdmissionLimits,
    runtime_state: &'static str,
    safe_mode: bool,
    /// 当前方案草稿（prepare / surface 产出，commit / abort 消费，新 prepare 可替换）。
    draft: Option<DraftPlan>,
    /// 当前格式变更草稿（format 产出，commit / abort 消费；与方案草稿互斥）。
    pending_format: Option<FormatDraft>,
    /// 当前模式变更草稿（surface=mode 产出，commit / abort 消费；与其它草稿互斥）。
    pending_mode: Option<ModeDraft>,
    /// 当前运行模式（控制面层；见 [`RuntimeMode`] 的边界说明）。
    mode: RuntimeMode,
    /// 最近一次 commit 生效的方案：语义界面（volume/routing/layout）以它为基线打补丁。
    base_plan: Option<Plan>,
    /// 已生效方案的有限历史（版本升序），供 rollback 回到旧版本。
    history: Vec<CommittedPlan>,
}

/// 运行模式（控制面层：输出使能 + 输入源声明）。
///
/// 真实采集源切换属于 rt-host/设备监督层（T70g 未实现）；控制面能诚实做到的是
/// `off`（整链静音）与 `aes67`（当前唯一已实现的输入源，等价于“使能输出”）。
/// `toslink` / `dante` 等其它输入源没有实现，显式拒绝，不假装可选。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMode {
    /// 关闭：整链静音（与 mute 同一个块边界静音机制，仅语义入口不同）。
    Off,
    /// 使能：输入源固定为 aes67（当前唯一实现的源）。
    Aes67,
}

impl RuntimeMode {
    /// 稳定字符串（status/响应/日志共用）。
    pub fn as_str(self) -> &'static str {
        match self {
            RuntimeMode::Off => "off",
            RuntimeMode::Aes67 => "aes67",
        }
    }
}

/// 模式变更草稿（两阶段；与方案/格式草稿互斥）。
#[derive(Debug, Clone)]
struct ModeDraft {
    token: String,
    version: u64,
    target: RuntimeMode,
    changed: bool,
    response: Value,
}

/// 格式变更草稿（两阶段；与方案草稿互斥）。
#[derive(Debug, Clone)]
struct FormatDraft {
    token: String,
    version: u64,
    /// 目标引擎配置（只允许 sample_rate / block_frames 改变）。
    target: EngineConfig,
    /// prepare 响应；同 token 重复 prepare 返回同一份。
    response: Value,
}

/// 一份已生效方案的历史条目（完整方案 + 版本 + graph_id）。
#[derive(Debug, Clone)]
struct CommittedPlan {
    /// 生效时的版本号。
    version: u64,
    /// 完整方案：回滚时重新编译，不依赖任何旧处理器对象。
    plan: Plan,
}

impl ControlState {
    /// 用给定配置新建（初始处理链为直通，未加载方案）。
    pub fn new(config: EngineConfig) -> Result<Self, EngineError> {
        Ok(Self {
            engine: SlotEngine::new(config, Box::new(Passthrough))?,
            limits: AdmissionLimits::default(),
            runtime_state: "idle",
            safe_mode: false,
            draft: None,
            pending_format: None,
            pending_mode: None,
            mode: RuntimeMode::Aes67,
            base_plan: None,
            history: Vec::new(),
        })
    }

    /// 底层执行器（非实时路径使用）。
    pub fn engine(&self) -> &SlotEngine {
        &self.engine
    }

    /// 底层执行器的可变引用（测试与驱动代码用，例如手工跑块观察换图）。
    pub fn engine_mut(&mut self) -> &mut SlotEngine {
        &mut self.engine
    }

    /// 处理一行请求，返回一行响应（不含换行）。
    pub fn handle_line(&mut self, line: &str) -> String {
        match self.dispatch(line) {
            Ok(value) => value.to_string(),
            Err(err) => err.to_json().to_string(),
        }
    }

    /// 打开假后端，让 status 报告 running（协议自检用）。
    pub fn open_null_backend(&mut self) -> Result<NullBackend, crate::backend::BackendError> {
        use crate::backend::AudioBackend;
        let device = self
            .engine
            .device_config(crate::owner::DeviceId::new("null-backend"));
        let mut backend = NullBackend::new();
        backend.start(&device)?;
        self.runtime_state = "running";
        Ok(backend)
    }

    fn dispatch(&mut self, line: &str) -> DispatchResult {
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

    /// 状态快照（与 status op 同一份 JSON，供 REST 层直接复用）。
    pub fn status_json(&self) -> Value {
        self.status()
    }

    /// 静音请求（与 mute op 同一份 JSON，供 REST 层直接复用）。
    pub fn mute_json(&mut self, mute: bool) -> Value {
        let receipt = self.engine.request_mute(mute);
        json!({
            "ok": true,
            "mute": receipt.mute,
            "applied_block": receipt.applied_block,
            "idempotent": receipt.idempotent,
        })
    }

    /// 方案加载（prepare）：读文件 → 校验 sha256 → 解码 → 准入 → 编译图 → 存草稿并生成 token。
    ///
    /// 这是非实时路径：允许文件 IO 与堆分配。成功不改变正在跑的音频（只在 `commit` 后于块边界换图）。
    ///
    /// 幂等/替换语义：同一 token（同版本号 + 同文件内容）重复 prepare 直接返回上次的响应，
    /// 不重复解码/编译，草稿保持不变；不同 token 的 prepare 覆盖旧草稿（后到的为准），
    /// 旧草稿的图在此（控制线程）析构。
    pub fn prepare_json(
        &mut self,
        plan_path: &str,
        plan_sha256: &str,
    ) -> Result<Value, ControlError> {
        let bytes = std::fs::read(plan_path)
            .map_err(|err| bad_request(format!("cannot read plan file {plan_path}: {err}")))?;
        let actual_sha = sha256_hex(&bytes);
        if !actual_sha.eq_ignore_ascii_case(plan_sha256.trim()) {
            return Err(bad_request(format!(
                "plan sha256 mismatch: request {plan_sha256}, file {plan_path} is {actual_sha}"
            )));
        }
        // 版本号取"下一个生效版本"；已 stage 未生效的切换不占版本号（单 pending 槽，后 stage 覆盖先 stage）。
        let version = self.next_plan_version();
        let token = engine_token(version, &sha256(&bytes));
        if let Some(draft) = &self.draft {
            if draft.token == token {
                return Ok(draft.response.clone());
            }
        }
        let plan = Plan::decode(&bytes)
            .map_err(|err| bad_request(format!("cannot decode plan {plan_path}: {err}")))?;
        let admission = admit(&plan, &self.limits);
        if !admission.accepted {
            return Err(ControlError::new(
                error::ADMISSION_REJECTED,
                format!(
                    "plan {plan_path} rejected by admission: {}",
                    admission.summary()
                ),
            ));
        }
        let config = self.engine.config().clone();
        let graph =
            PlanGraph::compile(&plan, &config, &self.limits).map_err(compile_error_to_control)?;
        let graph_id = plan.graph_id_hex();
        let response = json!({
            "ok": true,
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
            processor: Box::new(graph),
            response: response.clone(),
            plan,
        });
        Ok(response)
    }

    /// 诊断/试运行（inspect）：读方案文件 → 校验（可选 sha）→ 解码 → 准入 → 图编译，
    /// 返回结果但**不产生草稿、不改变正在跑的图**。
    ///
    /// 这是给"先把产物验一遍再决定要不要 prepare"的调用方（例如校准 CLI 产物）用的：
    /// - 读不到文件 / sha 不符 / 解不出方案 → `bad_request`（4xx）；
    /// - 文件可读但方案不可准入 / 图编译失败 → 仍然是 `ok:true`，在 `admission.accepted=false`
    ///   或 `compile.compiled=false` 里如实报告，便于一次看到多个问题；
    /// - `staged` 恒为 false：调用方可以放心把它当只读操作。
    pub fn inspect_json(
        &self,
        plan_path: &str,
        plan_sha256: Option<&str>,
    ) -> Result<Value, ControlError> {
        let bytes = std::fs::read(plan_path)
            .map_err(|err| bad_request(format!("cannot read plan file {plan_path}: {err}")))?;
        let actual_sha = sha256_hex(&bytes);
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
            let config = self.engine.config().clone();
            match PlanGraph::compile(&plan, &config, &self.limits) {
                Ok(graph) => json!({
                    "compiled": true,
                    "latency_samples": graph.estimated_latency_samples(),
                }),
                Err(err) => {
                    let mapped = compile_error_to_control(err);
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
    /// 模式界面第一阶段（surface=mode）：校验目标模式 → 存草稿；commit 才在块边界生效。
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
        let version = self.next_plan_version();
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
            "note": "real capture-source switching belongs to rt-host/device supervisor; toslink/dante are not implemented",
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

    /// 模式界面第二阶段：off = 块边界静音整链；aes67/on = 解除静音。不是 RT 路径。
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
        let receipt = self.engine.request_mute(draft.target == RuntimeMode::Off);
        self.mode = draft.target;
        Ok(json!({
            "ok": true,
            "changed": true,
            "applied_block": receipt.applied_block,
            "version": draft.version,
            "mode": self.mode.as_str(),
        }))
    }

    /// 格式变更第一阶段（format op）：校验目标格式 → 存草稿；commit 才真正重建执行器。
    ///
    /// 语义：格式（采样率 / 块长）不是方案补丁，而是执行器配置；commit 会重建 SlotEngine
    /// 并**丢弃正在跑的图**（方案里嵌了格式字段，旧图对新格式不再合法），响应如实说明这一点，
    /// 并提示 device_restart_required（真实声卡必须重开 PCM 流，本控制面不碰设备）。
    pub fn prepare_format_json(
        &mut self,
        object: &Map<String, Value>,
    ) -> Result<Value, ControlError> {
        let mut target = self.engine.config().clone();
        let current = target.clone();
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
        let version = self.next_plan_version();
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
        let response = json!({
            "ok": true,
            "engine_token": token,
            "planned_version": version,
            "format": format_json(&target),
            "changed": target != current,
            "applies_to": "control-plane engine",
            "device_restart_required": true,
            "note": "format change rebuilds the control-plane engine and drops the running graph; a plan matching the new format must be prepared again (a real device requires reopening the PCM stream)",
        });
        self.pending_format = Some(FormatDraft {
            token,
            version,
            target,
            response: response.clone(),
        });
        Ok(response)
    }

    /// 格式变更第二阶段：重建执行器（仅格式真正变化时）。不是 RT 路径，允许分配。
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
        let current = self.engine.config().clone();
        if draft.target == current {
            return Ok(json!({
                "ok": true,
                "changed": false,
                "applied_block": Value::Null,
                "version": draft.version,
                "format": format_json(&draft.target),
                "note": "format already matches the running engine; nothing rebuilt",
            }));
        }
        let was_muted = self.engine.stats().mute;
        let engine =
            SlotEngine::new(draft.target.clone(), Box::new(Passthrough)).map_err(|err| {
                bad_request(format!("cannot rebuild engine for the new format: {err}"))
            })?;
        self.engine = engine;
        if was_muted {
            self.engine.request_mute(true);
        }
        // 旧格式下的方案/历史对新格式不再合法（方案嵌了格式字段），如实清空。
        self.base_plan = None;
        let dropped_history = !self.history.is_empty();
        self.history.clear();
        Ok(json!({
            "ok": true,
            "changed": true,
            "applied_block": 1,
            "version": draft.version,
            "format": format_json(&self.engine.config().clone()),
            "all_ack": true,
            "graph_dropped": true,
            "history_cleared": dropped_history,
            "mute_preserved": was_muted,
            "device_restart_required": true,
        }))
    }

    /// 提交草稿：把图排入执行器的下一块边界，返回它将被应用的块号。
    ///
    /// `applied_block` 是"下一块"的块号：commit 本身不打断正在算的块，换图发生在下一次
    /// `run_block` 的入口（那块的编号就是 `applied_block`）。
    ///
    /// `all_ack` 语义：单 owner、单引擎（本控制面独占一个 [`SlotEngine`]），没有第二个引擎需要
    /// 确认，切换在下一块边界必然完成，因此恒为 true；多引擎（主/备）冗余要等 T40 以后再扩展。
    pub fn commit_json(&mut self, engine_token: &str) -> Result<Value, ControlError> {
        // 三类草稿互斥；存在哪个就提交哪个。
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
        let applied_block =
            self.engine
                .stage_processor(draft.processor, draft.version, draft.graph_id.clone());
        // 生效后：成为语义界面（volume/routing/layout）的补丁基线，并进入可回滚历史。
        self.base_plan = Some(draft.plan.clone());
        self.push_history(CommittedPlan {
            version: draft.version,
            plan: draft.plan,
        });
        Ok(json!({
            "ok": true,
            "applied_block": applied_block,
            "version": draft.version,
            "all_ack": true,
            "graph_id": draft.graph_id,
        }))
    }

    /// 语义界面（surface）第一阶段：以最近一次生效的方案为基线打补丁 → 准入/编译 → 存草稿。
    ///
    /// 两阶段语义与 prepare/commit 完全相同：这里只准备，`commit` 才在下一块边界生效，
    /// `abort` 丢弃。支持的界面见 [`WIRED_SURFACES`]；未接通的界面返回 `unsupported` 并带原因，
    /// 不返回假成功。
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
            // 格式不是方案补丁：委托给执行器配置的两阶段入口（同一个 commit/abort）。
            let object = data
                .as_object()
                .ok_or_else(|| bad_request("field data must be a json object"))?;
            return self.prepare_format_json(object);
        }
        if surface == "mode" {
            // 模式不是方案补丁：委托给运行模式的两阶段入口（同一个 commit/abort）。
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
        let version = self.next_plan_version();
        let token = engine_token(version, &sha256(&bytes));
        if let Some(draft) = &self.draft {
            if draft.token == token {
                return Ok(draft.response.clone());
            }
        }
        let admission = admit(&patched, &self.limits);
        if !admission.accepted {
            return Err(ControlError::new(
                error::ADMISSION_REJECTED,
                format!(
                    "surface {surface} patch rejected by admission: {}",
                    admission.summary()
                ),
            ));
        }
        let config = self.engine.config().clone();
        let graph = PlanGraph::compile(&patched, &config, &self.limits)
            .map_err(compile_error_to_control)?;
        let graph_id = patched.graph_id_hex();
        let response = json!({
            "ok": true,
            "surface": surface,
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
            processor: Box::new(graph),
            response: response.clone(),
            plan: patched,
        });
        Ok(response)
    }
    /// 记录一个已生效版本到有限历史；超出上限丢最旧（drop 最旧完整方案）。
    fn push_history(&mut self, entry: CommittedPlan) {
        self.history.push(entry);
        if self.history.len() > HISTORY_LIMIT {
            self.history.remove(0);
        }
    }

    /// 回滚第一阶段（两阶段）：把有限历史里的旧版本重新准备成草稿。
    ///
    /// 语义：版本号**递增**（回滚是一次前进式的新提交，历史不被改写）；不改变正在跑的图；
    /// 真正生效仍由 `commit` 在下一块边界完成，`abort` 丢弃。响应里的 `rolled_back_to`
    /// 指明草稿内容来自哪个旧版本。
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
        let version = self.next_plan_version();
        let token = engine_token(version, &sha256(&bytes));
        if let Some(draft) = &self.draft {
            if draft.token == token {
                return Ok(draft.response.clone());
            }
        }
        let admission = admit(&plan, &self.limits);
        if !admission.accepted {
            return Err(ControlError::new(
                error::ADMISSION_REJECTED,
                format!(
                    "rollback plan rejected by admission: {}",
                    admission.summary()
                ),
            ));
        }
        let config = self.engine.config().clone();
        let graph =
            PlanGraph::compile(&plan, &config, &self.limits).map_err(compile_error_to_control)?;
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
            processor: Box::new(graph),
            response: response.clone(),
            plan,
        });
        Ok(response)
    }
    /// 丢弃草稿（abort）：无副作用，正在跑的图不受影响。
    ///
    /// token 不符或没有草稿都返回 `engine_token_invalid`；成功返回 `{"ok":true,"aborted":true}`。
    pub fn abort_json(&mut self, engine_token: &str) -> Result<Value, ControlError> {
        // 模式草稿同样可 abort；错 token 不销毁草稿。
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
        // 格式草稿同样可 abort；错 token 不销毁草稿。
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
            // 错 token 不销毁草稿：放回去。
            self.draft = Some(draft);
            return Err(token_invalid(message));
        }
        Ok(json!({ "ok": true, "aborted": true }))
    }

    /// 当前系数/图版本（coeff）：只报已生效的真实值，没有就 null + reasons 说明，不编数。
    pub fn coeff_json(&self) -> Value {
        let info = self.engine.graph_info();
        let mut reasons = Map::new();
        if info.version.is_some() {
            reasons.insert(
                "last_good_version".to_string(),
                json!(concat!(
                    "whole-plan switching only: the running graph is the last good version; ",
                    "coefficient-level revisions land with T22"
                )),
            );
        } else {
            reasons.insert(
                "version".to_string(),
                json!("no plan applied yet: prepare + commit, then run one block; the graph switches at the next block boundary"),
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
        let available_versions: Vec<u64> = self.history.iter().map(|entry| entry.version).collect();
        json!({
            "ok": true,
            "version": info.version,
            "last_good_version": info.version,
            "graph_id": info.graph_id,
            "applied_block": info.applied_block,
            "available_versions": available_versions,
            "reasons": reasons,
        })
    }

    /// 下一个方案版本号（未加载过方案时为 1）。
    fn next_plan_version(&self) -> u64 {
        match self.engine.graph_info().version {
            Some(version) => version + 1,
            None => 1,
        }
    }

    fn status(&self) -> Value {
        let stats = self.engine.stats();
        let info = self.engine.graph_info();
        let mut reasons = Map::new();
        reasons.insert(
            "xruns".to_string(),
            json!("no audio device opened; nothing can underrun or overrun yet"),
        );
        reasons.insert(
            "underruns".to_string(),
            json!("no device pacing yet; counters stay 0 while the engine is driven by callers"),
        );
        if info.graph_id.is_none() {
            reasons.insert(
                "graph_id".to_string(),
                json!("no plan applied yet: call prepare + commit; the graph switches at the next block boundary"),
            );
        }
        if info.epoch.is_none() {
            reasons.insert(
                "epoch".to_string(),
                json!("no graph switch has taken effect yet; epoch counts applied plan switches"),
            );
        }
        if info.version.is_none() {
            reasons.insert(
                "actual_version".to_string(),
                json!("no plan applied yet; actual_version is the version of the running graph"),
            );
        }
        let not_wired: Vec<Value> = NOT_WIRED_OPS
            .iter()
            .map(|(op, reason)| json!({ "op": op, "reason": reason }))
            .collect();
        // 五个语义界面的真实能力：接通与否逐项上报，未接通的给原因，不编造。
        let mut surfaces = Map::new();
        for surface in WIRED_SURFACES {
            surfaces.insert((*surface).to_string(), json!(true));
        }
        let mut surface_reasons = Map::new();
        for (surface, reason) in NOT_WIRED_SURFACES {
            surfaces.insert((*surface).to_string(), json!(false));
            surface_reasons.insert((*surface).to_string(), json!(reason));
        }
        json!({
            "ok": true,
            "protocol_version": PROTOCOL_VERSION,
            "runtime_state": self.runtime_state,
            "graph_id": info.graph_id,
            "epoch": info.epoch,
            "blocks": stats.blocks,
            "xruns": 0,
            "underruns": stats.underruns,
            "faults": stats.faults,
            "silent": stats.mute,
            "mute": stats.mute,
            "safe_mode": self.safe_mode,
            "actual_version": info.version,
            "format": format_json(&self.engine.config().clone()),
            "mode": self.mode.as_str(),
            "wired": WIRED_OPS,
            "not_wired": not_wired,
            "surfaces": surfaces,
            "surface_reasons": surface_reasons,
            "reasons": reasons,
        })
    }

    fn mute(&mut self, object: &Map<String, Value>) -> DispatchResult {
        let mute = match object.get("mute") {
            Some(Value::Bool(mute)) => *mute,
            Some(_) => return Err(bad_request("field mute must be a boolean")),
            None => return Err(bad_request("missing field: mute")),
        };
        Ok(self.mute_json(mute))
    }
}

/// 一个键值说明表，给调用方解释当前实现边界。
pub type Notes = BTreeMap<&'static str, &'static str>;
