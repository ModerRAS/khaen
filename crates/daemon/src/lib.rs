//! daemon 骨架（T30/T31）：把主机控制面暴露成 REST。
//!
//! 只有一个后端：`ControlState`（块引擎 + 方案草稿）。没有 MemoryEngine，也没有"假装成功"的接口：
//! 还没实现的 op 返回 501 并带原因，`/api/status` 的 wired/not_wired 如实反映当前能力。
//!
//! # 错误码 → HTTP 状态码
//!
//! - `bad_request` → 400：请求字段缺失/类型不对、方案文件读不到、sha256 不符、方案解码失败。
//! - `unknown_op` → 404：REST 上没有这个 op 资源，与未知路径一致（另一种合理选择是 400，
//!   这里固定选 404，并由测试锁住）。
//! - `unsupported` → 501：op 存在但当前未实现。
//! - `admission_rejected` → 409：方案超预算/结构非法，或与当前运行时配置不匹配。
//! - `engine_token_invalid` → 409：没有对应草稿或 token 不符（先 prepare / 用正确 token）。
//! - 其余 → 500。

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::Value;

use khaen_host::control::{error, ControlError};
use khaen_host::{ControlState, EngineConfig};

/// daemon 共享状态。
#[derive(Debug)]
pub struct DaemonState {
    control: Mutex<ControlState>,
}

impl DaemonState {
    /// 用主机默认配置新建。
    pub fn new(config: EngineConfig) -> Result<Self, khaen_host::EngineError> {
        Ok(Self {
            control: Mutex::new(ControlState::new(config)?),
        })
    }

    /// 控制面（测试与内部使用）。
    pub fn control(&self) -> std::sync::MutexGuard<'_, ControlState> {
        self.control
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 组装路由。
pub fn router(state: Arc<DaemonState>) -> Router {
    Router::new()
        .route("/api/status", get(status))
        .route("/api/mute", post(mute))
        .route("/api/prepare", post(prepare))
        .route("/api/commit", post(commit))
        .route("/api/abort", post(abort))
        .route("/api/coeff", get(coeff))
        .with_state(state)
}

/// 启动 HTTP 服务（阻塞直到 listener 关闭）。
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: Arc<DaemonState>,
) -> std::io::Result<()> {
    axum::serve(listener, router(state)).await
}

async fn status(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    let body = state.control().status_json();
    (StatusCode::OK, Json(body))
}

async fn mute(
    State(state): State<Arc<DaemonState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let mute = match payload.get("mute") {
        Some(Value::Bool(mute)) => *mute,
        Some(_) => return bad_request("field mute must be a boolean"),
        None => return bad_request("missing field: mute"),
    };
    let body = state.control().mute_json(mute);
    (StatusCode::OK, Json(body))
}

async fn prepare(
    State(state): State<Arc<DaemonState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let (plan_path, plan_sha256) = match (payload.get("plan_path"), payload.get("plan_sha256")) {
        (Some(Value::String(path)), Some(Value::String(sha))) => (path.clone(), sha.clone()),
        _ => return bad_request("fields plan_path and plan_sha256 must be strings"),
    };
    // prepare 会读文件、解码、编译图：非实时路径。这里串行化控制面（音频线程不碰这把锁）。
    match state.control().prepare_json(&plan_path, &plan_sha256) {
        Ok(body) => (StatusCode::OK, Json(body)),
        Err(err) => error_response(err),
    }
}

async fn commit(
    State(state): State<Arc<DaemonState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let engine_token = match payload.get("engine_token") {
        Some(Value::String(token)) => token.clone(),
        _ => return bad_request("field engine_token must be a string"),
    };
    match state.control().commit_json(&engine_token) {
        Ok(body) => (StatusCode::OK, Json(body)),
        Err(err) => error_response(err),
    }
}

async fn abort(
    State(state): State<Arc<DaemonState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let engine_token = match payload.get("engine_token") {
        Some(Value::String(token)) => token.clone(),
        _ => return bad_request("field engine_token must be a string"),
    };
    match state.control().abort_json(&engine_token) {
        Ok(body) => (StatusCode::OK, Json(body)),
        Err(err) => error_response(err),
    }
}

async fn coeff(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    (StatusCode::OK, Json(state.control().coeff_json()))
}

/// 控制面错误码 → HTTP 状态码（映射表见模块文档）。
fn http_status(code: &str) -> StatusCode {
    match code {
        error::BAD_REQUEST => StatusCode::BAD_REQUEST,
        error::UNKNOWN_OP => StatusCode::NOT_FOUND,
        error::UNSUPPORTED => StatusCode::NOT_IMPLEMENTED,
        error::ADMISSION_REJECTED | error::ENGINE_TOKEN_INVALID => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn error_response(err: ControlError) -> (StatusCode, Json<Value>) {
    (http_status(err.code), Json(err.to_json()))
}

fn bad_request(message: impl Into<String>) -> (StatusCode, Json<Value>) {
    error_response(ControlError::new(error::BAD_REQUEST, message))
}
