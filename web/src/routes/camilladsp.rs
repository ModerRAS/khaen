//! CamillaDSP legacy 只读路由（F-04）。
//!
//! 本 crate（旧 web / khaen-audio-web）属于**退役栈镜像**：Phase 1 运行镜像不安装它，
//! REST/WS/Web 由 khaen-rt-daemon 提供（T42/T44/T70）。源码保留 GET status/configs/config 仅供
//! 历史对照与 T72 取证；**不再提供** CamillaDSP 启停接口（POST /camilladsp/{start,stop,restart}
//! 已删除，旧前端调用会得到 404）。不要在退役后把它重新接到生产控制面。

use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::camilla::{
    extract_devices_fields, scan_xrun, validate_camilla_yaml, ApplyOutcome, CamillaLive,
    ConfigEntry, DeviceFields, XrunInfo,
};
use crate::error::{AppError, AppResult};
use crate::service::{service_info, ServiceInfo};
use crate::state::AppState;
use crate::util;

#[derive(Debug, Serialize)]
pub struct CamillaStatusResponse {
    pub live: CamillaLive,
    pub service: ServiceInfo,
    pub xrun: XrunInfo,
    pub ws_url: String,
    pub config_file: String,
    pub config_dir: String,
    pub configs: Vec<ConfigEntry>,
    pub mock: bool,
    /// F-04：本进程是退役栈 legacy 只读面（不再是 CamillaDSP 控制入口）。
    pub legacy: bool,
}

/// GET /api/camilladsp
pub async fn status(State(state): State<AppState>) -> AppResult<Json<CamillaStatusResponse>> {
    let live = state.camilla.live().await;
    let service = service_info(state.services.as_ref(), state.camilla.service()).await;
    let logs = state
        .services
        .logs(state.camilla.service(), 100)
        .await
        .unwrap_or_default();
    let xrun = scan_xrun(&logs);
    let configs = state.camilla.list_configs().await;
    Ok(Json(CamillaStatusResponse {
        live,
        service,
        xrun,
        ws_url: state.camilla.ws_url().to_string(),
        config_file: state.camilla.config_file().to_string(),
        config_dir: state.camilla.config_dir().to_string(),
        configs,
        mock: state.mock,
        legacy: true,
    }))
}

// F-04（2026-11-17）：已删除 start/stop/restart 三个启停 handler 与 ServiceOpResponse。
// 旧 web 在 Phase 1 镜像里不安装；保留的 GET 仅作 legacy 只读状态展示（见 status.legacy）。

#[derive(Debug, Serialize)]
pub struct ConfigsResponse {
    pub configs: Vec<ConfigEntry>,
    pub config_file: String,
    pub config_dir: String,
}

/// GET /api/camilladsp/configs
pub async fn configs(State(state): State<AppState>) -> Json<ConfigsResponse> {
    Json(ConfigsResponse {
        configs: state.camilla.list_configs().await,
        config_file: state.camilla.config_file().to_string(),
        config_dir: state.camilla.config_dir().to_string(),
    })
}

#[derive(Debug, Deserialize)]
pub struct NameQuery {
    pub name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ConfigContent {
    pub name: Option<String>,
    pub path: String,
    pub content: String,
    pub truncated: bool,
}

/// GET /api/camilladsp/config?name=xxx.yml
pub async fn read_config(
    State(state): State<AppState>,
    Query(query): Query<NameQuery>,
) -> AppResult<Json<ConfigContent>> {
    match query.name.as_deref().filter(|n| !n.trim().is_empty()) {
        Some(name) => {
            util::validate_yaml_name(name).map_err(AppError::bad_request)?;
            let (path, content) = state.camilla.read_named_config(name).await?;
            let truncated = content.len() >= 2 * 1024 * 1024;
            Ok(Json(ConfigContent {
                name: Some(name.to_string()),
                path,
                content,
                truncated,
            }))
        }
        None => {
            let content = state.camilla.current_config_yaml().await?;
            Ok(Json(ConfigContent {
                name: None,
                path: state.camilla.config_file().to_string(),
                content,
                truncated: false,
            }))
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SwitchRequest {
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct SwitchResponse {
    pub ok: bool,
    pub outcome: ApplyOutcome,
    pub live: CamillaLive,
    pub service: ServiceInfo,
    pub warnings: Vec<String>,
}

/// POST /api/camilladsp/configs/switch  { "name": "aes67.yml" }
pub async fn switch_config(
    State(state): State<AppState>,
    Json(body): Json<SwitchRequest>,
) -> AppResult<Json<SwitchResponse>> {
    util::validate_yaml_name(&body.name).map_err(AppError::bad_request)?;
    let outcome = state.camilla.apply_named_config(&body.name).await?;
    let mut warnings = Vec::new();
    if outcome.applied_via == "systemd_restart" {
        warnings.push("websocket 应用失败，已通过 systemd 重启 CamillaDSP".to_string());
    }
    if outcome.backup.is_some() {
        warnings.push("原配置已备份，可在必要时手动回退".to_string());
    }
    warnings.push("改动已同步写入 config_file，重启后仍然生效".to_string());
    let live = state.camilla.live().await;
    let service = service_info(state.services.as_ref(), state.camilla.service()).await;
    Ok(Json(SwitchResponse {
        ok: true,
        outcome,
        live,
        service,
        warnings,
    }))
}

#[derive(Debug, Deserialize)]
pub struct ValidateRequest {
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct ValidateResponse {
    pub valid: bool,
    pub devices: Option<DeviceFields>,
}

/// POST /api/camilladsp/config/validate
pub async fn validate_config(
    Json(body): Json<ValidateRequest>,
) -> AppResult<Json<ValidateResponse>> {
    validate_camilla_yaml(&body.content)?;
    Ok(Json(ValidateResponse {
        valid: true,
        devices: extract_devices_fields(&body.content),
    }))
}

#[derive(Debug, Deserialize)]
pub struct VolumeRequest {
    pub db: f64,
}

#[derive(Debug, Serialize)]
pub struct VolumeResponse {
    pub ok: bool,
    pub volume_db: f64,
}

/// POST /api/camilladsp/volume  { "db": -12.0 }
pub async fn set_volume(
    State(state): State<AppState>,
    Json(body): Json<VolumeRequest>,
) -> AppResult<Json<VolumeResponse>> {
    let volume_db = state.camilla.set_volume(body.db).await?;
    Ok(Json(VolumeResponse {
        ok: true,
        volume_db,
    }))
}
