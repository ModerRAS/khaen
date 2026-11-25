//! ALSA / MOTU 路由。

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::alsa::{AlsaView, ToneResult};
use crate::error::AppResult;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct AlsaResponse {
    #[serde(flatten)]
    pub view: AlsaView,
    pub default_device: String,
    pub tone_hint: String,
}

/// GET /api/alsa
pub async fn view(State(state): State<AppState>) -> Json<AlsaResponse> {
    let view = state.alsa.view().await;
    let hint = if view.motu_present {
        "已检测到 MOTU UltraLite-mk5（USB）".to_string()
    } else {
        "未检测到 MOTU USB 设备（mock 模式会固定显示一台）".to_string()
    };
    Json(AlsaResponse {
        default_device: state.config.alsa.default_device.clone(),
        tone_hint: hint,
        view,
    })
}

#[derive(Debug, Deserialize)]
pub struct ToneRequest {
    #[serde(default)]
    pub device: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ToneResponse {
    pub ok: bool,
    pub result: ToneResult,
}

/// POST /api/alsa/test-tone  { "device": "plughw:1,0" }
pub async fn test_tone(
    State(state): State<AppState>,
    Json(body): Json<ToneRequest>,
) -> AppResult<Json<ToneResponse>> {
    let result = state.alsa.test_tone(body.device.as_deref()).await?;
    Ok(Json(ToneResponse {
        ok: result.success,
        result,
    }))
}
