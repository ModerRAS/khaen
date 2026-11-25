//! daemon 骨架（T30/T31）：把主机控制面暴露成 REST。
//!
//! 只有一个后端：`ControlState`（块引擎）。没有 MemoryEngine，也没有"假装成功"的接口：
//! 还没实现的 op 返回 501 并带原因，`/api/status` 的 wired/not_wired 如实反映当前能力。

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use khaen_host::control::{error, NOT_WIRED_OPS};
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
        .route("/api/prepare", post(not_wired_prepare))
        .route("/api/commit", post(not_wired_commit))
        .route("/api/abort", post(not_wired_abort))
        .route("/api/coeff", get(not_wired_coeff))
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
        Some(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "ok": false,
                    "error": error::BAD_REQUEST,
                    "message": "field mute must be a boolean",
                })),
            );
        }
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "ok": false,
                    "error": error::BAD_REQUEST,
                    "message": "missing field: mute",
                })),
            );
        }
    };
    let body = state.control().mute_json(mute);
    (StatusCode::OK, Json(body))
}

fn unsupported(op: &str) -> (StatusCode, Json<Value>) {
    let reason = NOT_WIRED_OPS
        .iter()
        .find(|(name, _)| *name == op)
        .map(|(_, reason)| *reason)
        .unwrap_or("not wired");
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "ok": false,
            "error": error::UNSUPPORTED,
            "message": format!("op {op} not wired: {reason}"),
        })),
    )
}

async fn not_wired_prepare() -> impl IntoResponse {
    unsupported("prepare")
}

async fn not_wired_commit() -> impl IntoResponse {
    unsupported("commit")
}

async fn not_wired_abort() -> impl IntoResponse {
    unsupported("abort")
}

async fn not_wired_coeff() -> impl IntoResponse {
    unsupported("coeff")
}
