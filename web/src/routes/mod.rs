//! HTTP 路由：公开（/healthz、登录、静态资源）与受保护（/api/*）两部分。

pub mod alsa;
pub mod aoip;
pub mod audio;
pub mod auth;
pub mod camilladsp;
pub mod channelmap;
pub mod mode;
pub mod motu;
pub mod overview;
pub mod ptp;
pub mod system;

use std::time::Instant;

use axum::extract::Request;
use axum::http::{header, HeaderValue};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;

use crate::assets;
use crate::state::AppState;

pub fn build_router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/auth/session", get(auth::session))
        .route("/auth/logout", post(auth::logout))
        .route("/overview", get(overview::overview))
        .route("/system", get(system::info))
        .route("/system/reboot", post(system::reboot))
        .route("/system/shutdown", post(system::shutdown))
        .route("/mode", get(mode::get_mode).put(mode::put_mode))
        .route("/mode/script-status", get(mode::script_status))
        .route(
            "/audio/format",
            get(audio::get_format).put(audio::put_format),
        )
        // F-04：旧 web 是退役镜像里的 legacy 只读控制面；**不得**提供 CamillaDSP 启停接口。
        // Phase1 运行镜像不安装旧 web；源码也不再注册 POST /camilladsp/{start,stop,restart}（T72 前置）。
        .route("/camilladsp", get(camilladsp::status))
        .route("/camilladsp/configs", get(camilladsp::configs))
        .route("/camilladsp/config", get(camilladsp::read_config))
        .route(
            "/camilladsp/configs/switch",
            post(camilladsp::switch_config),
        )
        .route(
            "/camilladsp/config/validate",
            post(camilladsp::validate_config),
        )
        .route("/camilladsp/volume", post(camilladsp::set_volume))
        .route("/ptp", get(ptp::status))
        .route("/ptp/start", post(ptp::start))
        .route("/ptp/stop", post(ptp::stop))
        .route("/ptp/restart", post(ptp::restart))
        .route("/ptp/config", get(ptp::get_config).put(ptp::put_config))
        .route("/aes67", get(aoip::aes67_status))
        .route("/aes67/logs", get(aoip::aes67_logs))
        .route("/aes67/config", get(aoip::aes67_config))
        .route("/aes67/start", post(aoip::aes67_start))
        .route("/aes67/stop", post(aoip::aes67_stop))
        .route("/aes67/restart", post(aoip::aes67_restart))
        .route("/inferno", get(aoip::inferno_status))
        .route("/inferno/logs", get(aoip::inferno_logs))
        .route("/inferno/config", get(aoip::inferno_config))
        .route("/inferno/start", post(aoip::inferno_start))
        .route("/inferno/stop", post(aoip::inferno_stop))
        .route("/inferno/restart", post(aoip::inferno_restart))
        .route("/alsa", get(alsa::view))
        .route("/alsa/test-tone", post(alsa::test_tone))
        .route("/motu/status", get(motu::status))
        .route("/motu/snapshot", get(motu::snapshot))
        .route("/motu/monitor", post(motu::monitor))
        .route("/motu/direct-mute", post(motu::direct_mute))
        .route("/motu/restore", post(motu::restore))
        .route(
            "/channelmap",
            get(channelmap::get_channelmap).put(channelmap::save_channelmap),
        )
        .route("/channelmap/render", get(channelmap::render_config))
        .route(
            "/channelmap/input-mode",
            get(channelmap::get_input_mode).put(channelmap::set_input_mode),
        )
        .route("/channelmap/validate", post(channelmap::validate_doc))
        .route("/channelmap/draft", post(channelmap::draft_channelmap))
        .route(
            "/channelmap/speaker-route",
            get(channelmap::get_speaker_route).post(channelmap::post_speaker_route),
        )
        .route("/channelmap/apply", post(channelmap::apply_channelmap))
        .route(
            "/channelmap/rollback",
            post(channelmap::rollback_channelmap),
        )
        .route(
            "/channelmap/profiles",
            get(channelmap::list_profiles).post(channelmap::save_profile),
        )
        .route("/channelmap/profiles/load", post(channelmap::load_profile))
        .route(
            "/channelmap/profiles/delete",
            post(channelmap::delete_profile),
        )
        .route("/channelmap/export", get(channelmap::export_channelmap))
        .route("/channelmap/import", post(channelmap::import_channelmap))
        .route(
            "/channelmap/fir",
            get(channelmap::list_fir).post(channelmap::upload_fir),
        )
        .route("/channelmap/test-tone", post(channelmap::test_tone))
        .with_state(state.clone());

    let protected = protected.layer(middleware::from_fn_with_state(
        state.clone(),
        auth::require_auth,
    ));

    Router::new()
        .route("/healthz", get(system::healthz))
        .route("/api/auth/status", get(auth::status))
        .route("/api/auth/setup", post(auth::setup))
        .route("/api/auth/login", post(auth::login))
        .nest("/api", protected)
        .fallback(get(assets::static_handler))
        .layer(middleware::from_fn(security_headers))
        .layer(middleware::from_fn(access_log))
        .with_state(state)
}

/// 安全响应头（无外部 CDN；页面只允许同源脚本）。
async fn security_headers(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// 简单访问日志（写 stderr → systemd journal）。
async fn access_log(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let started = Instant::now();
    let response = next.run(req).await;
    let status = response.status().as_u16();
    if path.starts_with("/api") || path == "/healthz" {
        eprintln!(
            "{method} {path} -> {status} ({} ms)",
            started.elapsed().as_millis()
        );
    }
    response
}
