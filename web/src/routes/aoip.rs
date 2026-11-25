//! AES67 / Inferno 路由（共用 AoipBackend）。

use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::aoip::{AoipConfigFile, AoipKind, AoipLogs, AoipView};
use crate::error::{AppError, AppResult};
use crate::service::ServiceInfo;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct LinesQuery {
    pub lines: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct AoipConfigResponse {
    pub kind: String,
    pub label: String,
    pub webui_url: String,
    pub files: Vec<AoipConfigFile>,
    pub notes: Vec<String>,
    pub mock: bool,
}

#[derive(Debug, Serialize)]
pub struct AoipServiceOpResponse {
    pub ok: bool,
    pub kind: String,
    pub action: &'static str,
    pub service: ServiceInfo,
}

async fn status_impl(state: &AppState, kind: AoipKind) -> Json<AoipView> {
    Json(state.aoip.view(kind).await)
}

async fn logs_impl(
    state: &AppState,
    kind: AoipKind,
    lines: Option<usize>,
) -> AppResult<Json<AoipLogs>> {
    let default_lines = state.aoip.cfg(kind).log_lines;
    let n = lines.unwrap_or(default_lines).clamp(1, 2000);
    state.aoip.logs(kind, n).await.map(Json)
}

async fn config_impl(state: &AppState, kind: AoipKind) -> Json<AoipConfigResponse> {
    let view = state.aoip.view(kind).await;
    Json(AoipConfigResponse {
        kind: view.kind,
        label: view.label,
        webui_url: view.webui_url,
        files: view.config,
        notes: view.notes,
        mock: view.mock,
    })
}

async fn op_impl(
    state: &AppState,
    kind: AoipKind,
    action: &'static str,
) -> AppResult<Json<AoipServiceOpResponse>> {
    let service = match action {
        "start" => state.aoip.start(kind).await?,
        "stop" => state.aoip.stop(kind).await?,
        _ => state.aoip.restart(kind).await?,
    };
    if !service.available {
        return Err(AppError::unavailable(
            service
                .error
                .clone()
                .unwrap_or_else(|| format!("{} 不可用", kind.label())),
        ));
    }
    Ok(Json(AoipServiceOpResponse {
        ok: true,
        kind: kind.as_str().to_string(),
        action,
        service,
    }))
}

// ===== AES67 =====

pub async fn aes67_status(State(state): State<AppState>) -> Json<AoipView> {
    status_impl(&state, AoipKind::Aes67).await
}

pub async fn aes67_logs(
    State(state): State<AppState>,
    Query(query): Query<LinesQuery>,
) -> AppResult<Json<AoipLogs>> {
    logs_impl(&state, AoipKind::Aes67, query.lines).await
}

pub async fn aes67_config(State(state): State<AppState>) -> Json<AoipConfigResponse> {
    config_impl(&state, AoipKind::Aes67).await
}

pub async fn aes67_start(State(state): State<AppState>) -> AppResult<Json<AoipServiceOpResponse>> {
    op_impl(&state, AoipKind::Aes67, "start").await
}

pub async fn aes67_stop(State(state): State<AppState>) -> AppResult<Json<AoipServiceOpResponse>> {
    op_impl(&state, AoipKind::Aes67, "stop").await
}

pub async fn aes67_restart(
    State(state): State<AppState>,
) -> AppResult<Json<AoipServiceOpResponse>> {
    op_impl(&state, AoipKind::Aes67, "restart").await
}

// ===== Inferno =====

pub async fn inferno_status(State(state): State<AppState>) -> Json<AoipView> {
    status_impl(&state, AoipKind::Inferno).await
}

pub async fn inferno_logs(
    State(state): State<AppState>,
    Query(query): Query<LinesQuery>,
) -> AppResult<Json<AoipLogs>> {
    logs_impl(&state, AoipKind::Inferno, query.lines).await
}

pub async fn inferno_config(State(state): State<AppState>) -> Json<AoipConfigResponse> {
    config_impl(&state, AoipKind::Inferno).await
}

pub async fn inferno_start(
    State(state): State<AppState>,
) -> AppResult<Json<AoipServiceOpResponse>> {
    op_impl(&state, AoipKind::Inferno, "start").await
}

pub async fn inferno_stop(State(state): State<AppState>) -> AppResult<Json<AoipServiceOpResponse>> {
    op_impl(&state, AoipKind::Inferno, "stop").await
}

pub async fn inferno_restart(
    State(state): State<AppState>,
) -> AppResult<Json<AoipServiceOpResponse>> {
    op_impl(&state, AoipKind::Inferno, "restart").await
}
