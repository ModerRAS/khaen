//! 主机控制协议（契约 B）：行 JSON 请求 / 行 JSON 响应。
//!
//! 这一层只做协议，不碰 socket：`serve_loop` 接任意 `BufRead` + `Write`，
//! 因此可以在内存管道上跑完整协议测试，Unix socket 只负责把连接塞进来。
//!
//! 能力上报必须真实：已经能干的 op 进 `wired`，还没实现的进 `not_wired` 并带原因；
//! 未实现 op 的请求返回 `unsupported`，**不返回假成功**。

pub mod serve;

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use crate::backend::NullBackend;
use crate::engine::{BlockEngine, EngineConfig, EngineError, Passthrough};

/// 协议版本（随契约追加递增）。
pub const PROTOCOL_VERSION: u16 = 1;

/// 单行请求的最大字节数；超过即拒绝处理。
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;

/// 已实现的 op。
pub const WIRED_OPS: &[&str] = &["status", "mute"];

/// 尚未实现的 op 及其原因（如实上报用）。
pub const NOT_WIRED_OPS: &[(&str, &str)] = &[
    (
        "prepare",
        "no plan admission yet: plan loading lands with the graph executor (T21)",
    ),
    (
        "commit",
        "no graph switch yet: commit cannot apply a plan before T21",
    ),
    ("abort", "no draft plan to abort before prepare is wired"),
    (
        "coeff",
        "no coefficient versioning yet: coefficients are compiled into the graph (T22)",
    ),
];

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
}

type DispatchResult = Result<Value, (&'static str, String)>;

fn bad_request(message: impl Into<String>) -> (&'static str, String) {
    (error::BAD_REQUEST, message.into())
}

/// 控制面状态：目前包着块引擎（直通链），等 T21 图执行器落地再换实现。
#[derive(Debug)]
pub struct ControlState {
    engine: BlockEngine<Passthrough>,
    runtime_state: &'static str,
    safe_mode: bool,
}

impl ControlState {
    /// 用给定配置新建（直通处理链，未接真实设备）。
    pub fn new(config: EngineConfig) -> Result<Self, EngineError> {
        Ok(Self {
            engine: BlockEngine::new(config, Passthrough)?,
            runtime_state: "idle",
            safe_mode: false,
        })
    }

    /// 底层引擎（非实时路径使用）。
    pub fn engine(&self) -> &BlockEngine<Passthrough> {
        &self.engine
    }

    /// 底层引擎的可变引用（测试与驱动代码块用）。
    pub fn engine_mut(&mut self) -> &mut BlockEngine<Passthrough> {
        &mut self.engine
    }

    /// 处理一行请求，返回一行响应（不含换行）。
    pub fn handle_line(&mut self, line: &str) -> String {
        match self.dispatch(line) {
            Ok(value) => value.to_string(),
            Err((code, message)) => json!({
                "ok": false,
                "error": code,
                "message": message,
            })
            .to_string(),
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
            other => {
                if NOT_WIRED_OPS.iter().any(|(name, _)| *name == other) {
                    let reason = NOT_WIRED_OPS
                        .iter()
                        .find(|(name, _)| *name == other)
                        .map(|(_, reason)| *reason)
                        .unwrap_or("not wired");
                    Err((
                        error::UNSUPPORTED,
                        format!("op {other} not wired: {reason}"),
                    ))
                } else {
                    Err((error::UNKNOWN_OP, format!("unknown op: {other}")))
                }
            }
        }
    }

    fn status(&self) -> Value {
        let stats = self.engine.stats();
        let mut reasons = Map::new();
        reasons.insert(
            "graph_id".to_string(),
            json!("no plan loaded; plan admission lands with T21"),
        );
        reasons.insert(
            "epoch".to_string(),
            json!("no coefficient versioning yet; epoch stays 0 until T22"),
        );
        reasons.insert(
            "xruns".to_string(),
            json!("no audio device opened; nothing can underrun or overrun yet"),
        );
        reasons.insert(
            "actual_version".to_string(),
            json!("no release installed; version reporting lands with T40/T41"),
        );
        reasons.insert(
            "underruns".to_string(),
            json!("no device pacing yet; counters stay 0 while the engine is driven by callers"),
        );
        let not_wired: Vec<Value> = NOT_WIRED_OPS
            .iter()
            .map(|(op, reason)| json!({ "op": op, "reason": reason }))
            .collect();
        json!({
            "ok": true,
            "protocol_version": PROTOCOL_VERSION,
            "runtime_state": self.runtime_state,
            "graph_id": Value::Null,
            "epoch": 0,
            "blocks": stats.blocks,
            "xruns": 0,
            "underruns": stats.underruns,
            "faults": stats.faults,
            "silent": stats.mute,
            "mute": stats.mute,
            "safe_mode": self.safe_mode,
            "actual_version": Value::Null,
            "wired": WIRED_OPS,
            "not_wired": not_wired,
            "reasons": reasons,
        })
    }

    fn mute(&mut self, object: &Map<String, Value>) -> DispatchResult {
        let mute = match object.get("mute") {
            Some(Value::Bool(mute)) => *mute,
            Some(_) => return Err(bad_request("field mute must be a boolean")),
            None => return Err(bad_request("missing field: mute")),
        };
        let receipt = self.engine.request_mute(mute);
        Ok(json!({
            "ok": true,
            "mute": receipt.mute,
            "applied_block": receipt.applied_block,
            "idempotent": receipt.idempotent,
        }))
    }
}

/// 一个键值说明表，给调用方解释当前实现边界。
pub type Notes = BTreeMap<&'static str, &'static str>;
