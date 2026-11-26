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
//! - `no_base_plan` → 409：surface 补丁前还没有已生效方案。
//! - `no_history` → 409：rollback 前还没有任何已生效版本。
//! - 其余 → 500。
//!
//! # 路由（M0 静态托管）
//!
//! - `/api/*`：控制面 REST（语义见各 handler）；未知 `/api` 路径返回结构化 JSON 404
//!   （`{ok:false,error:"not_found",message}`），不回退成 SPA 页面。
//! - `/healthz`：无认证探活 `{ok,version,uptime_s}`。
//! - `/api/meta`：`{api_version,web_version,ops,surfaces,not_wired}`，值与能力来自
//!   `khaen_host::control` 的既有常量（见 `meta` 模块）。
//! - 其余 `GET`：内嵌 `webui/dist` 静态资源；未知路径回退 `index.html`（SPA fallback），
//!   路径查找只命中编译期内嵌表、从不读文件系统，目录穿越被显式拒绝（见 `webui` 模块）。

pub mod bind;
pub mod device;
mod meta;
mod webui;
pub mod webui_assets;

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::Value;

use khaen_host::control::{error, ControlError};
use khaen_host::{ControlState, EngineConfig};

pub use device::DeviceMonitor;

/// daemon 版本（workspace `version`，即 `CARGO_PKG_VERSION`）。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 控制台版本：构建期从 `webui/package.json` 的 `version` 读取
/// （`build.rs` 生成 `webui_assets::WEBUI_PACKAGE_VERSION`；`webui/package.json` 读不到时
/// 回退 workspace 版本）。`/api/meta` 的 `web_version` 直接用它，不另编一个数字。
pub const WEB_VERSION: &str = webui_assets::WEBUI_PACKAGE_VERSION;

/// REST API 版本：与契约 B 的主机控制协议版本同源（`khaen_host::PROTOCOL_VERSION` 是版本事实源）。
pub const API_VERSION: u32 = khaen_host::PROTOCOL_VERSION as u32;

/// daemon 共享状态。
#[derive(Debug)]
pub struct DaemonState {
    control: Mutex<ControlState>,
    device: Mutex<DeviceMonitor>,
    /// 进程启动时刻（`/healthz` 的 `uptime_s`）。
    started: std::time::Instant,
}

impl DaemonState {
    /// 用主机默认配置新建；设备监督器默认未接线（原因随 status 一起上报）。
    pub fn new(config: EngineConfig) -> Result<Self, khaen_host::EngineError> {
        Ok(Self {
            control: Mutex::new(ControlState::new(config)?),
            device: Mutex::new(DeviceMonitor::disabled(
                "device supervisor is not wired: no probe/rate-control configured in this process",
            )),
            started: std::time::Instant::now(),
        })
    }

    /// 接线设备监督器（由 `main` 按配置构造；测试也用真实监督器 + 假组件）。
    pub fn with_device(mut self, monitor: DeviceMonitor) -> Self {
        self.device = Mutex::new(monitor);
        self
    }

    /// 控制面（测试与内部使用）。
    pub fn control(&self) -> std::sync::MutexGuard<'_, ControlState> {
        self.control
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 设备监督器（测试与内部使用）。
    pub fn device(&self) -> std::sync::MutexGuard<'_, DeviceMonitor> {
        self.device
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 轮询设备监督器，并在锁存时把引擎静音（fail-closed）。返回最新设备快照。
    ///
    /// 恢复必须是用户显式 `/api/mute false`，这里绝不自动解除静音。
    pub fn poll_device(&self) -> Value {
        let snapshot = self.device().poll();
        // 锁存期间每次轮询都重新下发静音：请求是块边界幂等的，代价只是一次控制面锁；
        // 用户若在锁存期间显式解除静音，下一次轮询必须把它重新压回去（fail-closed）。
        if self.device().requires_mute() {
            let _ = self.control().mute_json(true);
        }
        snapshot
    }

    /// 最近一次设备快照（不轮询）。
    pub fn device_json(&self) -> Value {
        self.device().status()
    }

    /// 进程启动以来的整秒数（`/healthz` 用；测试里通常为 0）。
    pub fn uptime_s(&self) -> u64 {
        self.started.elapsed().as_secs()
    }
}

/// 组装路由。
pub fn router(state: Arc<DaemonState>) -> Router {
    Router::new()
        .route("/healthz", get(meta::healthz))
        .route("/api/meta", get(meta::api_meta))
        .route("/api/status", get(status))
        .route("/api/mute", post(mute))
        .route("/api/prepare", post(prepare))
        .route("/api/commit", post(commit))
        .route("/api/abort", post(abort))
        .route("/api/coeff", get(coeff))
        .route("/api/plan/inspect", post(inspect))
        .route("/api/surface", post(surface))
        .route("/api/rollback", post(rollback))
        .route("/api/format", post(format_prepare))
        .route("/api/device", get(device_status))
        .route("/api/device/retry", post(device_retry))
        // 非 /api 的 GET/HEAD 走内嵌静态资源 + SPA 回退；未知 /api/* 返回结构化 JSON 404。
        .fallback(webui::fallback)
        .with_state(state)
}

/// 启动 HTTP 服务（阻塞直到 listener 关闭）。
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: Arc<DaemonState>,
) -> std::io::Result<()> {
    axum::serve(listener, router(state)).await
}

async fn format_prepare(
    State(state): State<Arc<DaemonState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let object = match payload.as_object() {
        Some(object) => object,
        None => return bad_request("request body must be a json object"),
    };
    match state.control().prepare_format_json(object) {
        Ok(body) => (StatusCode::OK, Json(body)),
        Err(err) => error_response(err),
    }
}

async fn status(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    let mut body = state.control().status_json();
    if let Some(object) = body.as_object_mut() {
        object.insert("device".to_string(), state.device_json());
    }
    (StatusCode::OK, Json(body))
}

async fn device_status(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    (StatusCode::OK, Json(state.device_json()))
}

async fn device_retry(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    (StatusCode::OK, Json(state.device().request_retry()))
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

/// 语义界面第一阶段（volume/routing/layout）：只准备补丁草稿，commit 仍走 /api/commit。
async fn surface(
    State(state): State<Arc<DaemonState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let surface = match payload.get("surface") {
        Some(Value::String(surface)) => surface.clone(),
        Some(_) => return bad_request("field surface must be a string"),
        None => return bad_request("missing field: surface"),
    };
    let data = payload
        .get("data")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    match state.control().prepare_surface_json(&surface, &data) {
        Ok(body) => (StatusCode::OK, Json(body)),
        Err(err) => error_response(err),
    }
}

/// 回滚第一阶段：把历史里的旧版本准备成草稿（版本号递增），commit 仍走 /api/commit。
async fn rollback(
    State(state): State<Arc<DaemonState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let target = match payload.get("target") {
        Some(Value::Number(number)) => match number.as_u64() {
            Some(version) => Some(version),
            None => return bad_request("field target must be a non-negative integer version"),
        },
        Some(Value::Null) | None => None,
        Some(_) => return bad_request("field target must be a non-negative integer version"),
    };
    match state.control().rollback_json(target) {
        Ok(body) => (StatusCode::OK, Json(body)),
        Err(err) => error_response(err),
    }
}

async fn coeff(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    (StatusCode::OK, Json(state.control().coeff_json()))
}

/// 只读试运行：读方案文件 → 解码 → 准入 → 图编译，不产生草稿、不改变正在跑的图。
async fn inspect(
    State(state): State<Arc<DaemonState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let plan_path = match payload.get("plan_path") {
        Some(Value::String(path)) => path.clone(),
        Some(_) => return bad_request("field plan_path must be a string"),
        None => return bad_request("missing field: plan_path"),
    };
    let plan_sha256 = match payload.get("plan_sha256") {
        Some(Value::String(sha)) => Some(sha.clone()),
        Some(_) => return bad_request("field plan_sha256 must be a string"),
        None => None,
    };
    match state
        .control()
        .inspect_json(&plan_path, plan_sha256.as_deref())
    {
        Ok(body) => (StatusCode::OK, Json(body)),
        Err(err) => error_response(err),
    }
}

/// 控制面错误码 → HTTP 状态码（映射表见模块文档）。
fn http_status(code: &str) -> StatusCode {
    match code {
        error::BAD_REQUEST => StatusCode::BAD_REQUEST,
        error::UNKNOWN_OP => StatusCode::NOT_FOUND,
        error::UNSUPPORTED => StatusCode::NOT_IMPLEMENTED,
        error::ADMISSION_REJECTED
        | error::ENGINE_TOKEN_INVALID
        | error::NO_BASE_PLAN
        | error::NO_HISTORY => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn error_response(err: ControlError) -> (StatusCode, Json<Value>) {
    (http_status(err.code), Json(err.to_json()))
}

fn bad_request(message: impl Into<String>) -> (StatusCode, Json<Value>) {
    error_response(ControlError::new(error::BAD_REQUEST, message))
}
