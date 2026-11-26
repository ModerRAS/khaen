//! `/healthz` 与 `/api/meta`：探活与版本/能力自描述。
//!
//! 所有值来自现有真实常量，不编造：
//! - `api_version` ← 契约 B 的 `khaen_host::PROTOCOL_VERSION`（REST 与主机控制协议同一版本事实源，
//!   见 `docs/CONTRACTS.md`；契约升级时随之递增）。
//! - `web_version` ← `webui/package.json` 的 `version`（构建期由 `build.rs` 提取进
//!   `webui_assets::WEBUI_PACKAGE_VERSION`；读不到时回退 workspace 版本），即实际被内嵌的
//!   控制台版本。
//! - `ops` / `surfaces` / `not_wired` ← `khaen_host::control` 的 `WIRED_OPS` / `WIRED_SURFACES` /
//!   `NOT_WIRED_OPS` / `NOT_WIRED_SURFACES`，未实现的项如实进 `not_wired` 并带原因（没有假成功）。
//!   M2 起 `/api/status` 是页面数据源的聚合 schema（`khaen-status/v1`），能力清单只在这里，
//!   `/api/status` 不再重复。
//! - `host_link` ← 当前控制通道实况（T70f-b）：`target`（rt-host/daemon-process）、`socket` 路径、
//!   `connected`、`mode`（socket/file/none）与不可用原因。每次都现取 probe（socket status 或状态文件），
//!   不缓存、不编造连通性。
//!
//! `/healthz` 不要求认证（当前进程没有认证层，探活必须永远可用）；`uptime_s` 是进程启动以来的整秒数。

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};

use khaen_host::control::{NOT_WIRED_OPS, NOT_WIRED_SURFACES, WIRED_OPS, WIRED_SURFACES};

use crate::{DaemonState, API_VERSION, VERSION, WEB_VERSION};

/// 无认证探活：`{ok, version, uptime_s}`（字段集固定，供部署/负载均衡轮询）。
pub(crate) async fn healthz(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "version": VERSION,
            "uptime_s": state.uptime_s(),
        })),
    )
}

/// 版本与能力自描述：`{api_version, web_version, ops, surfaces, calibration, host_link, not_wired}`。
///
/// 前端用 `not_wired` 驱动"未接线"灰态；`ops`/`surfaces` 是已接通的真实能力清单。
/// `calibration.wired` 来自当前注入的校准服务（默认未接线），随实现状态变化。
pub(crate) async fn api_meta(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    let calibration = state.calibration_status();
    // host_link 能力：现取 probe（socket status 往返可能阻塞，放到 blocking 线程池）。
    let worker = Arc::clone(&state);
    let host_link = match tokio::task::spawn_blocking(move || {
        crate::status::link_capability(&worker, std::time::SystemTime::now())
    })
    .await
    {
        Ok(host_link) => host_link,
        Err(join_error) => json!({
            "target": state.host_link().target().as_str(),
            "mode": "none",
            "connected": false,
            "socket": state.host_link().socket().map(|path| path.display().to_string()),
            "error": format!("host_link probe task failed: {join_error}"),
        }),
    };
    let mut not_wired: Vec<Value> =
        Vec::with_capacity(NOT_WIRED_OPS.len() + NOT_WIRED_SURFACES.len() + 1);
    for &(op, reason) in NOT_WIRED_OPS {
        not_wired.push(json!({ "op": op, "reason": reason }));
    }
    for &(surface, reason) in NOT_WIRED_SURFACES {
        not_wired.push(json!({ "surface": surface, "reason": reason }));
    }
    if !calibration.wired {
        let reason = calibration
            .reason
            .clone()
            .unwrap_or_else(|| crate::calibration::NOT_WIRED_REASON.to_string());
        not_wired.push(json!({ "op": "calibration.jobs", "reason": reason }));
    }
    (
        StatusCode::OK,
        Json(json!({
            "api_version": API_VERSION,
            "web_version": WEB_VERSION,
            "ops": WIRED_OPS,
            "surfaces": WIRED_SURFACES,
            "calibration": {
                "wired": calibration.wired,
                "reason": calibration.reason,
            },
            "host_link": host_link,
            "not_wired": not_wired,
        })),
    )
}
