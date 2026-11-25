//! GET/PUT /api/audio/format：采样率 / chunksize / target_level 展示与有限编辑。

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::audio::{build_view, chunksize_warning, validate_update, AudioFormatView};
use crate::camilla::{extract_devices_fields, patch_devices_fields, scan_xrun, ApplyOutcome};
use crate::error::AppResult;
use crate::state::AppState;

async fn current_format(state: &AppState) -> AudioFormatView {
    let mode = state.mode.current_mode();
    let profile = state.config.mode.profile(mode);
    let yaml = state.camilla.current_config_yaml().await.ok();
    let fields = yaml.as_deref().and_then(extract_devices_fields);
    let source = if fields.is_some() {
        "camilladsp 当前激活配置"
    } else {
        "模式默认值（未读取到 CamillaDSP 配置）"
    };
    let config_path = profile
        .map(|p| p.camilla_config.clone())
        .unwrap_or_else(|| state.camilla.config_file().to_string());
    let logs = state
        .services
        .logs(state.camilla.service(), 100)
        .await
        .unwrap_or_default();
    let xrun = scan_xrun(&logs);
    build_view(
        &state.config,
        mode,
        fields,
        source,
        Some(config_path),
        xrun,
        state.mock,
    )
}

/// GET /api/audio/format
pub async fn get_format(State(state): State<AppState>) -> Json<AudioFormatView> {
    Json(current_format(&state).await)
}

#[derive(Debug, Deserialize)]
pub struct UpdateFormatRequest {
    #[serde(default)]
    pub sample_rate: Option<u32>,
    #[serde(default)]
    pub chunksize: Option<u32>,
    #[serde(default)]
    pub target_level: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct UpdateFormatResponse {
    pub ok: bool,
    pub outcome: ApplyOutcome,
    pub format: AudioFormatView,
    pub warnings: Vec<String>,
}

/// PUT /api/audio/format  { "sample_rate": 96000, "chunksize": 128, "target_level": 2048 }
///
/// 修改当前模式 CamillaDSP 配置的 devices 字段，先备份再经 websocket SetConfigJson 应用；
/// 应用失败自动回滚（见 camilla::RealCamilla）。
pub async fn put_format(
    State(state): State<AppState>,
    Json(body): Json<UpdateFormatRequest>,
) -> AppResult<Json<UpdateFormatResponse>> {
    validate_update(body.sample_rate, body.chunksize, body.target_level)?;
    let mode = state.mode.current_mode();
    let target_path = state
        .config
        .mode
        .profile(mode)
        .map(|p| p.camilla_config.clone())
        .unwrap_or_else(|| state.camilla.config_file().to_string());
    let current = state.camilla.current_config_yaml().await?;
    let patched = patch_devices_fields(
        &current,
        body.sample_rate,
        body.chunksize,
        body.target_level,
    )?;
    let outcome = state
        .camilla
        .apply_config_content(&patched, &target_path)
        .await?;
    let format = current_format(&state).await;
    let mut warnings = format.warnings.clone();
    if let Some(w) = chunksize_warning(body.chunksize) {
        warnings.push(w);
    }
    warnings.push("改动已应用；请观察 10~30 秒 xrun 与播放状态，必要时用备份文件回退".into());
    if outcome.backup.is_none() {
        warnings.push("未生成备份文件（mock 模式或原配置文件不存在）".into());
    }
    Ok(Json(UpdateFormatResponse {
        ok: true,
        outcome,
        format,
        warnings,
    }))
}
