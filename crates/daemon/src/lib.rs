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
//! - `/api/status`：M2 聚合状态（PLAN §4.4 的 `khaen-status/v1`，见 `status` 模块）：
//!   读 rt-host 状态文件（`KHAEN_RT_STATUS_FILE`，启动时注入）+ daemon 自身字段
//!   （版本/uptime/bind/引擎格式）；状态文件缺失/损坏/过期时 `host_link.connected=false`、
//!   rt-host 字段全 null + reasons，daemon 字段照常返回，**绝不 500**。
//! - `/api/meta`：`{api_version,web_version,ops,surfaces,not_wired}`，值与能力来自
//!   `khaen_host::control` 的既有常量（见 `meta` 模块）。
//! - `/api/auth/*`：M1 认证（setup/login/session/logout/password），argon2id PHC 存
//!   `${KHAEN_STATE_DIR:-/var/lib/khaen}/auth.json`（见 `auth` 模块）。除 `/healthz` 与
//!   `/api/auth/status|setup|login` 外，全部 `/api` 要求会话；写方法（Cookie 会话）额外要求
//!   `X-KHAEN-CSRF`；`/api` 响应统一带 `X-KHAEN-API-Version: 1` / `X-KHAEN-Web-Version` /
//!   `Cache-Control: no-store`。
//! - 其余 `GET`：内嵌 `webui/dist` 静态资源；未知路径回退 `index.html`（SPA fallback），
//!   路径查找只命中编译期内嵌表、从不读文件系统，目录穿越被显式拒绝（见 `webui` 模块）。

pub mod auth;
pub mod bind;
pub mod device;
mod meta;
pub mod status;
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

/// daemon 启动错误：引擎配置错误或认证存储不可用（两者都 fail-closed，不静默降级）。
#[derive(Debug)]
pub enum DaemonError {
    /// 引擎/控制面构造失败。
    Engine(khaen_host::EngineError),
    /// 认证存储（auth.json）不可用或内容损坏。
    Auth(auth::AuthError),
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Engine(error) => write!(f, "engine: {error}"),
            Self::Auth(error) => write!(f, "auth: {error}"),
        }
    }
}

impl std::error::Error for DaemonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Engine(error) => Some(error),
            Self::Auth(error) => Some(error),
        }
    }
}

impl From<khaen_host::EngineError> for DaemonError {
    fn from(error: khaen_host::EngineError) -> Self {
        Self::Engine(error)
    }
}

impl From<auth::AuthError> for DaemonError {
    fn from(error: auth::AuthError) -> Self {
        Self::Auth(error)
    }
}

/// daemon 共享状态。
#[derive(Debug)]
pub struct DaemonState {
    control: Mutex<ControlState>,
    device: Mutex<DeviceMonitor>,
    /// 认证/会话存储（argon2id PHC + 会话 + 登录限速）。
    auth: Mutex<auth::AuthStore>,
    /// 进程启动时刻（`/healthz` 的 `uptime_s`）。
    started: std::time::Instant,
    /// 启动时解析的监听地址（`/api/status` 的 `daemon.bind`；handler 不读环境变量）。
    bind: std::net::SocketAddr,
    /// rt-host 状态文件路径（`/api/status` 聚合读取；生产由 `KHAEN_RT_STATUS_FILE` 注入）。
    rt_status_file: std::path::PathBuf,
}

impl DaemonState {
    /// 用主机默认配置新建；设备监督器默认未接线（原因随 status 一起上报）；
    /// 认证状态目录取 `KHAEN_STATE_DIR`（缺省 `/var/lib/khaen`）。
    pub fn new(config: EngineConfig) -> Result<Self, DaemonError> {
        Self::new_with_auth(config, auth::AuthConfig::from_env())
    }

    /// 用显式认证配置新建（测试注入临时目录/低成本 KDF 参数；生产由 `new` 走环境变量）。
    pub fn new_with_auth(
        config: EngineConfig,
        auth_config: auth::AuthConfig,
    ) -> Result<Self, DaemonError> {
        Ok(Self {
            control: Mutex::new(ControlState::new(config)?),
            device: Mutex::new(DeviceMonitor::disabled(
                "device supervisor is not wired: no probe/rate-control configured in this process",
            )),
            auth: Mutex::new(auth::AuthStore::load(auth_config)?),
            started: std::time::Instant::now(),
            bind: bind::default_addr(),
            rt_status_file: std::path::PathBuf::from(status::DEFAULT_RT_STATUS_FILE),
        })
    }

    /// 接线设备监督器（由 `main` 按配置构造；测试也用真实监督器 + 假组件）。
    pub fn with_device(mut self, monitor: DeviceMonitor) -> Self {
        self.device = Mutex::new(monitor);
        self
    }

    /// 注入启动时解析的监听地址（由 `main` 从 `KHAEN_BIND`/`KHAEN_PORT` 解析后传入）。
    ///
    /// `/api/status` 的 `daemon.bind` 用它，handler 里不读环境变量（测试可注入任意值）。
    pub fn with_bind(mut self, bind: std::net::SocketAddr) -> Self {
        self.bind = bind;
        self
    }

    /// 注入 rt-host 状态文件路径（`/api/status` 聚合读取；测试注入 tempdir）。
    pub fn with_rt_status_file(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.rt_status_file = path.into();
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

    /// 认证/会话存储（中间件与 auth handler 使用）。
    pub(crate) fn auth(&self) -> std::sync::MutexGuard<'_, auth::AuthStore> {
        self.auth
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

    /// 启动时解析的监听地址（`/api/status` 的 `daemon.bind`）。
    pub fn bind(&self) -> std::net::SocketAddr {
        self.bind
    }

    /// rt-host 状态文件路径（`/api/status` 聚合读取）。
    pub fn rt_status_file(&self) -> &std::path::Path {
        &self.rt_status_file
    }

    /// 控制面当前的引擎配置（`/api/status` 的 `format.block_frames`/`channels` 来源）。
    pub fn engine_config(&self) -> EngineConfig {
        self.control().engine().config().clone()
    }
}

/// 组装路由。
pub fn router(state: Arc<DaemonState>) -> Router {
    Router::new()
        .route("/healthz", get(meta::healthz))
        // M1 认证（PLAN §4.2）：status/setup/login 匿名可访问，其余要求会话。
        .route("/api/auth/status", get(auth::status))
        .route("/api/auth/setup", post(auth::setup))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/session", get(auth::session))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/password", post(auth::password))
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
        // 认证守卫包住全部路由（含 fallback）：只对 /api 生效，并给 /api 响应补版本/缓存头。
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            auth::guard,
        ))
        .with_state(state)
}

/// 启动 HTTP 服务（阻塞直到 listener 关闭）。
///
/// 用 `into_make_service_with_connect_info` 让中间件拿到对端 IP：登录限速按来源（IP）隔离。
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: Arc<DaemonState>,
) -> std::io::Result<()> {
    let app = router(state).into_make_service_with_connect_info::<std::net::SocketAddr>();
    axum::serve(listener, app).await
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

/// M2 聚合状态：PLAN §4.4 的 `khaen-status/v1`（页面唯一数据源）。
///
/// 从 daemon 自身（版本/uptime/bind/引擎格式）与 rt-host 状态文件聚合；文件缺失/损坏/过期只降级
/// （`host_link.connected=false` + null + reasons），任何情况下都返回 200（见 `status` 模块）。
async fn status(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    let body = status::aggregate(&state, std::time::SystemTime::now());
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
