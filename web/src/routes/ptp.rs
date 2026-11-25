//! PTP 路由：linuxptp 状态/配置，以及当前模式对应的 PTP 来源说明。

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::config::RunMode;
use crate::error::{AppError, AppResult};
use crate::ptp::{PtpApplyOutcome, PtpConfView, PtpStatus};
use crate::service::{service_info, ServiceInfo};
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct PtpModeInfo {
    pub mode: String,
    pub ptp_source: String,
    pub ptp_role: String,
    pub ptp_version: String,
    pub ptp_services: Vec<String>,
    /// true = AES67 方案 B1，PTP 由 AoIP daemon 内置，本页 linuxptp 操作不适用。
    pub builtin_ptp: bool,
}

#[derive(Debug, Serialize)]
pub struct PtpResponse {
    pub status: PtpStatus,
    pub conf: PtpConfView,
    pub services: Vec<ServiceInfo>,
    pub mode: PtpModeInfo,
    pub notes: Vec<String>,
    pub mock: bool,
}

async fn collect_services(state: &AppState) -> Vec<ServiceInfo> {
    let mut units = vec![
        state.config.ptp.ptp4l_service.clone(),
        state.config.ptp.phc2sys_service.clone(),
        state.config.ptp.statime_service.clone(),
    ];
    units.retain(|u| !u.trim().is_empty());
    units.dedup();
    let mut out = Vec::new();
    for unit in units {
        out.push(service_info(state.services.as_ref(), &unit).await);
    }
    out
}

/// GET /api/ptp
pub async fn status(State(state): State<AppState>) -> AppResult<Json<PtpResponse>> {
    let mode = state.mode.current_mode();
    let profile = state.config.mode.profile(mode);
    let (source, role, version, ptp_services, aoip_service) = match profile {
        Some(p) => (
            p.ptp_source.clone(),
            p.ptp_role.clone(),
            p.ptp_version.clone(),
            p.ptp_services.clone(),
            Some(p.aoip_service.clone()),
        ),
        None => (
            "linuxptp".to_string(),
            "slave".to_string(),
            "PTPv2".to_string(),
            vec![
                state.config.ptp.ptp4l_service.clone(),
                state.config.ptp.phc2sys_service.clone(),
            ],
            None,
        ),
    };
    let status = state.ptp.status(&source, aoip_service.as_deref()).await;
    let conf = state.ptp.get_conf().await;
    let services = collect_services(&state).await;
    let builtin_ptp = ptp_services.is_empty();
    let mut notes = Vec::new();
    match mode {
        RunMode::Aes67 if builtin_ptp => notes.push(
            "当前为 AES67 方案 B1：PTPv2 slave 由 aes67 daemon 内置提供，未运行 ptp4l/phc2sys；\
             本页的启动/停止与 masterOnly/priority1 配置仅对方案 B2 有意义。"
                .into(),
        ),
        RunMode::Dante => notes.push(
            "当前为 Dante 模式：PTPv1 由 Inferno 配套的 Statime 提供，pmc 不适用；\
             锁定状态按 statime.service 日志推断（见状态详情）。"
                .into(),
        ),
        RunMode::Off => notes.push("当前未启用运行模式，状态来自 linuxptp（pmc）。".into()),
        _ => notes.push(
            "方案 B2：本机 ptp4l 作为 PTP 主时钟；修改 masterOnly/priority1 会先备份 /etc/ptp4l.conf 再重启 ptp4l/phc2sys。"
                .into(),
        ),
    }
    Ok(Json(PtpResponse {
        status,
        conf,
        services,
        mode: PtpModeInfo {
            mode: mode.as_str().to_string(),
            ptp_source: source,
            ptp_role: role,
            ptp_version: version,
            ptp_services,
            builtin_ptp,
        },
        notes,
        mock: state.mock,
    }))
}

#[derive(Debug, Serialize)]
pub struct PtpServiceOpResponse {
    pub ok: bool,
    pub action: &'static str,
    pub services: Vec<ServiceInfo>,
}

async fn service_op(
    state: &AppState,
    action: &'static str,
) -> AppResult<Json<PtpServiceOpResponse>> {
    let units = [
        state.config.ptp.ptp4l_service.clone(),
        state.config.ptp.phc2sys_service.clone(),
    ];
    for unit in units.iter().filter(|u| !u.trim().is_empty()) {
        let result = match action {
            "start" => state.services.start(unit).await,
            "stop" => state.services.stop(unit).await,
            _ => state.services.restart(unit).await,
        };
        if let Err(e) = result {
            return Err(AppError::unavailable(format!("{action} {unit} 失败: {e}")));
        }
    }
    Ok(Json(PtpServiceOpResponse {
        ok: true,
        action,
        services: collect_services(state).await,
    }))
}

/// POST /api/ptp/start
pub async fn start(State(state): State<AppState>) -> AppResult<Json<PtpServiceOpResponse>> {
    service_op(&state, "start").await
}
/// POST /api/ptp/stop
pub async fn stop(State(state): State<AppState>) -> AppResult<Json<PtpServiceOpResponse>> {
    service_op(&state, "stop").await
}
/// POST /api/ptp/restart
pub async fn restart(State(state): State<AppState>) -> AppResult<Json<PtpServiceOpResponse>> {
    service_op(&state, "restart").await
}

/// GET /api/ptp/config
pub async fn get_config(State(state): State<AppState>) -> Json<PtpConfView> {
    Json(state.ptp.get_conf().await)
}

#[derive(Debug, Deserialize)]
pub struct PtpConfigRequest {
    #[serde(default)]
    pub master_only: Option<bool>,
    #[serde(default)]
    pub priority1: Option<u8>,
}

#[derive(Debug, Serialize)]
pub struct PtpConfigResponse {
    pub ok: bool,
    pub outcome: PtpApplyOutcome,
    pub warnings: Vec<String>,
}

/// PUT /api/ptp/config  { "master_only": true, "priority1": 10 }
pub async fn put_config(
    State(state): State<AppState>,
    Json(body): Json<PtpConfigRequest>,
) -> AppResult<Json<PtpConfigResponse>> {
    let outcome = state
        .ptp
        .apply_conf(body.master_only, body.priority1)
        .await?;
    let mode = state.mode.current_mode();
    let mut warnings = Vec::new();
    if mode == RunMode::Aes67 && state.config.mode.aes67.ptp_services.is_empty() {
        warnings.push(
            "当前 AES67 为方案 B1（daemon 内置 PTP slave），修改 linuxptp 配置不会影响当前链路；方案 B2 才生效"
                .into(),
        );
    }
    warnings.push("配置修改前已自动备份；重启失败会自动恢复备份".into());
    Ok(Json(PtpConfigResponse {
        ok: true,
        outcome,
        warnings,
    }))
}
