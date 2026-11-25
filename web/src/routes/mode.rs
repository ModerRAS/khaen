//! 运行模式路由：GET /api/mode、PUT /api/mode。

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::config::RunMode;
use crate::error::{AppError, AppResult};
use crate::mode::{ModeComponents, ModeStatusView, ScriptStatus};
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct ModeResponse {
    #[serde(flatten)]
    pub status: ModeStatusView,
    pub components: ModeComponents,
    pub mock: bool,
}

async fn response_for(state: &AppState, mode: RunMode) -> ModeResponse {
    ModeResponse {
        status: state.mode.status_with_persisted().await,
        components: state.mode.components(mode).await,
        mock: state.mock,
    }
}

/// GET /api/mode
pub async fn get_mode(State(state): State<AppState>) -> Json<ModeResponse> {
    let mode = state.mode.current_mode();
    Json(response_for(&state, mode).await)
}

#[derive(Debug, Deserialize)]
pub struct SwitchModeRequest {
    pub mode: String,
}

#[derive(Debug, Serialize)]
pub struct ScriptStatusResponse {
    pub available: bool,
    pub switch_script: String,
    pub script: Option<ScriptStatus>,
    pub mock: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// GET /api/mode/script-status：主动读取 audio-stack 的 tspi-mode status --json。
pub async fn script_status(State(state): State<AppState>) -> AppResult<Json<ScriptStatusResponse>> {
    let script = state.mode.query_script_status().await?;
    let available = script.is_some();
    Ok(Json(ScriptStatusResponse {
        available,
        switch_script: state.config.mode.switch_script.clone(),
        script,
        mock: state.mock,
        message: if available {
            None
        } else if state.mock {
            Some("mock 模式不执行 tspi-mode 脚本".into())
        } else {
            Some("切换脚本未配置或不存在（/usr/local/bin/tspi-mode）".into())
        },
    }))
}

/// PUT /api/mode  { "mode": "dante" | "aes67" | "off" }
/// 触发异步切换，立即返回 202；切换在途时再请求返回 409。
pub async fn put_mode(
    State(state): State<AppState>,
    Json(body): Json<SwitchModeRequest>,
) -> AppResult<(StatusCode, Json<ModeResponse>)> {
    let mode: RunMode = body
        .mode
        .parse()
        .map_err(|e: String| AppError::bad_request(e))?;
    state.mode.begin_switch(mode).await?;
    let response = ModeResponse {
        status: state.mode.status_with_persisted().await,
        components: state.mode.components(mode).await,
        mock: state.mock,
    };
    Ok((StatusCode::ACCEPTED, Json(response)))
}
