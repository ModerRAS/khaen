//! 健康检查、系统信息、电源操作路由。

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::error::AppResult;
use crate::power::{check_power, PowerAction, PowerResult};
use crate::state::AppState;
use crate::util;

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub mock: bool,
    pub uptime_secs: u64,
    pub now_unix: u64,
}

/// GET /healthz（唯一无需鉴权的数据接口）
pub async fn healthz(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: crate::VERSION,
        mock: state.mock,
        uptime_secs: state.uptime_secs(),
        now_unix: util::now_unix(),
    })
}

#[derive(Debug, Serialize)]
pub struct SystemStatus {
    pub version: &'static str,
    pub mock: bool,
    pub uptime_secs: u64,
    pub listen: String,
    pub allow_reboot: bool,
    pub allow_shutdown: bool,
    pub mode_enabled: bool,
    pub restore_on_start: bool,
    pub session_ttl_secs: u64,
    pub token_configured: bool,
}

/// GET /api/system
pub async fn info(State(state): State<AppState>) -> Json<SystemStatus> {
    Json(SystemStatus {
        version: crate::VERSION,
        mock: state.mock,
        uptime_secs: state.uptime_secs(),
        listen: state.config.listen.clone(),
        allow_reboot: state.config.system.allow_reboot,
        allow_shutdown: state.config.system.allow_shutdown,
        mode_enabled: state.config.mode.enabled,
        restore_on_start: state.config.mode.restore_on_start,
        session_ttl_secs: state.config.session_ttl_secs,
        token_configured: !state.config.token.trim().is_empty(),
    })
}

#[derive(Debug, Deserialize)]
pub struct PowerRequest {
    #[serde(default)]
    pub confirm: Option<String>,
}

/// POST /api/system/reboot
pub async fn reboot(
    State(state): State<AppState>,
    Json(body): Json<PowerRequest>,
) -> AppResult<Json<PowerResult>> {
    check_power(
        &state.config.system,
        PowerAction::Reboot,
        body.confirm.as_deref(),
    )?;
    Ok(Json(state.power.action(PowerAction::Reboot).await?))
}

/// POST /api/system/shutdown
pub async fn shutdown(
    State(state): State<AppState>,
    Json(body): Json<PowerRequest>,
) -> AppResult<Json<PowerResult>> {
    check_power(
        &state.config.system,
        PowerAction::Shutdown,
        body.confirm.as_deref(),
    )?;
    Ok(Json(state.power.action(PowerAction::Shutdown).await?))
}
