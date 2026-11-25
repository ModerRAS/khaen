//! MOTU / CueMix 路由。写操作都在认证中间件之后，并且必须带 `confirm: true`。

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use crate::error::{AppError, AppResult};
use crate::motu::{self, WriteReport};
use crate::state::AppState;

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct RawQuery {
    raw: Option<String>,
}

fn wants_raw(query: &RawQuery) -> bool {
    matches!(query.raw.as_deref(), Some("1") | Some("true") | Some("yes"))
}

fn require_confirm(confirm: Option<bool>) -> AppResult<()> {
    if confirm == Some(true) {
        Ok(())
    } else {
        Err(AppError::bad_request("写操作需要请求体 confirm: true"))
    }
}

pub struct WriteBody(WriteReport);

impl IntoResponse for WriteBody {
    fn into_response(self) -> Response {
        let status = if self.0.ok {
            StatusCode::OK
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        (status, Json(self.0)).into_response()
    }
}

/// GET /api/motu/status
pub async fn status(State(state): State<AppState>) -> Json<motu::MotuStatus> {
    Json(state.motu.status().await)
}

/// GET /api/motu/snapshot?raw=1
pub async fn snapshot(
    State(state): State<AppState>,
    Query(query): Query<RawQuery>,
) -> AppResult<Json<motu::SnapshotView>> {
    Ok(Json(state.motu.snapshot(wants_raw(&query)).await?))
}

#[derive(Debug, Deserialize)]
pub struct MonitorBody {
    trim_db: Option<f64>,
    confirm: Option<bool>,
}

/// POST /api/motu/monitor  { "trim_db": 28, "confirm": true }
pub async fn monitor(
    State(state): State<AppState>,
    Json(body): Json<MonitorBody>,
) -> AppResult<WriteBody> {
    require_confirm(body.confirm)?;
    let Some(trim_db) = body.trim_db else {
        return Err(AppError::bad_request("需要 trim_db"));
    };
    let trim = motu::parse_trim_db(trim_db)?;
    Ok(WriteBody(state.motu.set_monitor(trim).await?))
}

#[derive(Debug, Deserialize)]
pub struct MuteBody {
    mute: Option<bool>,
    confirm: Option<bool>,
}

/// POST /api/motu/direct-mute  { "mute": true, "confirm": true }
pub async fn direct_mute(
    State(state): State<AppState>,
    Json(body): Json<MuteBody>,
) -> AppResult<WriteBody> {
    require_confirm(body.confirm)?;
    let Some(mute) = body.mute else {
        return Err(AppError::bad_request("需要 mute: true 或 false"));
    };
    Ok(WriteBody(state.motu.direct_mute(mute).await?))
}

#[derive(Debug, Deserialize)]
pub struct RestoreBody {
    #[serde(default)]
    snapshot_id: Option<String>,
    confirm: Option<bool>,
}

/// POST /api/motu/restore  { "snapshot_id": "...", "confirm": true }
pub async fn restore(
    State(state): State<AppState>,
    Json(body): Json<RestoreBody>,
) -> AppResult<WriteBody> {
    require_confirm(body.confirm)?;
    Ok(WriteBody(state.motu.restore(body.snapshot_id).await?))
}
