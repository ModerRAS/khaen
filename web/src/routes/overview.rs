//! GET /api/overview 总览：系统 + 服务 + 模式 + CamillaDSP + PTP + 声卡。

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::alsa::AlsaView;
use crate::camilla::{scan_xrun, CamillaLive, XrunInfo};
use crate::error::AppResult;
use crate::mode::{AoipComponent, ModeStatusView, PtpComponent, SwitchState};
use crate::service::{service_info, ServiceInfo};
use crate::state::AppState;
use crate::system::SystemInfo;
use crate::util;

#[derive(Debug, Serialize)]
pub struct ModeSummary {
    pub mode: String,
    pub mode_label: String,
    pub state: SwitchState,
    pub target: Option<String>,
    pub target_label: Option<String>,
    pub last_error: Option<String>,
    pub strategy: String,
    pub switching: bool,
    pub ptp_version: Option<String>,
    pub ptp_source: Option<String>,
    pub ptp_role: Option<String>,
    pub input_format: Option<String>,
    pub output_format: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CamillaOverview {
    pub live: CamillaLive,
    pub service: ServiceInfo,
    pub xrun: XrunInfo,
    pub ws_url: String,
    pub config_file: String,
    pub config_dir: String,
}

#[derive(Debug, Serialize)]
pub struct PowerFlags {
    pub allow_reboot: bool,
    pub allow_shutdown: bool,
}

#[derive(Debug, Serialize)]
pub struct OverviewResponse {
    pub mock: bool,
    pub version: &'static str,
    pub uptime_secs: u64,
    pub now_unix: u64,
    pub system: SystemInfo,
    pub mode: ModeSummary,
    pub services: Vec<ServiceInfo>,
    /// 服务状态说明（如 Inferno 是 ALSA 插件、无独立服务）。
    pub service_notes: Vec<String>,
    pub camilla: CamillaOverview,
    pub ptp: PtpComponent,
    pub aoip: AoipComponent,
    pub alsa: AlsaView,
    pub power: PowerFlags,
}

/// OEM v2 实际音频单元（按显示顺序）。
pub const ACTUAL_AUDIO_UNITS: &[&str] = &[
    "khaen-camilladsp.service",
    "khaen-ptp4l-master.service",
    "khaen-phc2sys.service",
    "khaen-statime-inferno.service",
    "khaen-aes67-daemon.service",
    "khaen-ravenna-lkm.service",
];

/// 服务状态页单元：真机用 systemctl list-unit-files 'khaen-*.service' 自动发现（避免
/// config 里旧名字 not-found 误导）；mock 返回 OEM v2 固定清单；失败回退配置。
async fn service_units(state: &AppState) -> Vec<String> {
    if state.mock {
        return ACTUAL_AUDIO_UNITS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
    }
    if let Ok(out) = state
        .runner
        .run(
            "systemctl",
            &[
                "list-unit-files",
                "khaen-*.service",
                "--no-pager",
                "--no-legend",
            ],
        )
        .await
    {
        if out.success {
            let mut found = Vec::new();
            for line in out.stdout.lines() {
                let Some(unit) = line.split_whitespace().next() else {
                    continue;
                };
                if ACTUAL_AUDIO_UNITS.contains(&unit) {
                    found.push(unit.to_string());
                }
            }
            if !found.is_empty() {
                return found;
            }
        }
    }
    let mut units = state.overview_units();
    units.retain(|u| !u.eq_ignore_ascii_case("inferno.service"));
    units
}

pub async fn overview(State(state): State<AppState>) -> AppResult<Json<OverviewResponse>> {
    let status: ModeStatusView = state.mode.status();
    let mode = status.mode;
    let components = state.mode.components(mode).await;
    let system = state.system.info().await;

    let mut services = Vec::new();
    for unit in service_units(&state).await {
        services.push(service_info(state.services.as_ref(), &unit).await);
    }
    let live = state.camilla.live().await;
    let camilla_service = service_info(state.services.as_ref(), state.camilla.service()).await;
    let logs = state
        .services
        .logs(state.camilla.service(), 100)
        .await
        .unwrap_or_default();
    let xrun = scan_xrun(&logs);
    let alsa = state.alsa.view().await;
    let profile = status.profiles.iter().find(|p| p.mode == mode.as_str());

    Ok(Json(OverviewResponse {
        mock: state.mock,
        version: crate::VERSION,
        uptime_secs: state.uptime_secs(),
        now_unix: util::now_unix(),
        system,
        mode: ModeSummary {
            mode: mode.as_str().to_string(),
            mode_label: status.mode_label.clone(),
            state: status.state,
            target: status.target.map(|m| m.as_str().to_string()),
            target_label: status.target_label.clone(),
            last_error: status.last_error.clone(),
            strategy: status.strategy.clone(),
            switching: status.state == SwitchState::Switching,
            ptp_version: profile.map(|p| p.ptp_version.clone()),
            ptp_source: profile.map(|p| p.ptp_source.clone()),
            ptp_role: profile.map(|p| p.ptp_role.clone()),
            input_format: profile.map(|p| p.input_format.clone()),
            output_format: profile.map(|p| p.output_format.clone()),
        },
        services,
        service_notes: vec![
            "Inferno 是 ALSA 插件（libasound_module_pcm_inferno），无独立 systemd 服务；Dante 的 PTPv1 从时钟由 khaen-statime-inferno.service 提供。"
                .into(),
        ],
        camilla: CamillaOverview {
            live,
            service: camilla_service,
            xrun,
            ws_url: state.camilla.ws_url().to_string(),
            config_file: state.camilla.config_file().to_string(),
            config_dir: state.camilla.config_dir().to_string(),
        },
        ptp: components.ptp,
        aoip: components.aoip,
        alsa,
        power: PowerFlags {
            allow_reboot: state.config.system.allow_reboot,
            allow_shutdown: state.config.system.allow_shutdown,
        },
    }))
}
