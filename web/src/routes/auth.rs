//! 登录 / 注销 / 会话 + 鉴权中间件。

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Extension, Request, State};
use axum::http::{header, HeaderMap, HeaderValue};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use serde::Serialize;

use crate::auth::{
    self, clear_session_cookie, session_cookie, validate_password, AuthState, Authenticated,
    LoginRequest, SetupRequest,
};
use crate::error::AppResult;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub ok: bool,
    pub csrf_token: String,
    pub expires_in: u64,
    pub auth_kind: &'static str,
}

#[derive(Debug, Serialize)]
pub struct AuthStatusResponse {
    pub password_configured: bool,
    pub setup_required: bool,
    /// token 仍保留为恢复/CLI 兜底；不会因为设置密码而失效。
    pub token_fallback: bool,
    pub mock: bool,
    pub min_password_len: usize,
    pub recommended_password_len: usize,
}

#[derive(Debug, Serialize)]
pub struct SessionResponse {
    pub authenticated: bool,
    pub auth_kind: &'static str,
    pub csrf_token: Option<String>,
    pub expires_in: u64,
}

#[derive(Debug, Serialize)]
pub struct OkResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// 客户端 IP 提取：有 ConnectInfo 时按 IP 限速，测试/同机时归为 "local"。
pub struct ClientIp(pub String);

impl axum::extract::FromRequestParts<AppState> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let ip = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ci| ci.0.ip().to_string())
            .unwrap_or_else(|| "local".to_string());
        Ok(ClientIp(ip))
    }
}

/// GET /api/auth/status（公开）：是否已设置管理密码、是否需要首设。
pub async fn status(State(state): State<AppState>) -> Json<AuthStatusResponse> {
    let configured = state.auth.password_configured();
    Json(AuthStatusResponse {
        password_configured: configured,
        setup_required: !configured,
        token_fallback: true,
        mock: state.mock,
        min_password_len: 8,
        recommended_password_len: 10,
    })
}

fn login_response(success: crate::auth::LoginSuccess, secure: bool) -> AppResult<Response> {
    let cookie = session_cookie(&success.session_id, success.expires_in, secure);
    let mut response = Json(LoginResponse {
        ok: true,
        csrf_token: success.csrf_token,
        expires_in: success.expires_in,
        auth_kind: "session",
    })
    .into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie)
            .map_err(|e| crate::error::AppError::internal(e.to_string()))?,
    );
    Ok(response)
}

/// POST /api/auth/setup（公开，仅未配置时）：设置管理密码并立即登录。
pub async fn setup(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    Json(body): Json<SetupRequest>,
) -> AppResult<Response> {
    if state.auth.password_configured() {
        return Err(crate::error::AppError::conflict(
            "管理密码已设置；如需重置请通过 CLI/恢复流程",
        ));
    }
    validate_password(&body.password, &body.confirm)?;
    let success = state.auth.setup_password(&client_ip, &body.password)?;
    login_response(success, state.auth.cookie_secure())
}

/// POST /api/auth/login：password（管理密码）或 token（恢复/CLI 兜底）二选一。
pub async fn login(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    Json(body): Json<LoginRequest>,
) -> AppResult<Response> {
    let success = match body.password.as_deref().filter(|p| !p.is_empty()) {
        Some(password) => state.auth.login_password(&client_ip, password)?,
        None => {
            let token = body
                .token
                .as_deref()
                .filter(|t| !t.is_empty())
                .ok_or_else(|| crate::error::AppError::bad_request("需要 password 或 token"))?;
            state.auth.login(&client_ip, token)?
        }
    };
    login_response(success, state.auth.cookie_secure())
}

/// GET /api/auth/session
pub async fn session(
    State(state): State<AppState>,
    Extension(authenticated): Extension<Authenticated>,
) -> Json<SessionResponse> {
    let csrf_token = match &authenticated {
        Authenticated::Session { csrf } => Some(csrf.clone()),
        Authenticated::Token => None,
    };
    Json(SessionResponse {
        authenticated: true,
        auth_kind: authenticated.kind(),
        csrf_token,
        expires_in: state.auth.ttl_secs(),
    })
}

/// POST /api/auth/logout
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    state.auth.logout(&headers);
    let mut response = Json(OkResponse {
        ok: true,
        message: Some("已注销".into()),
    })
    .into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&clear_session_cookie()).unwrap(),
    );
    response
}

/// 受保护路由的鉴权中间件：cookie 会话或 X-Auth-Token / Bearer。
/// 写操作（POST/PUT/PATCH/DELETE）对会话认证强制校验 X-CSRF-Token。
pub async fn require_auth(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let authenticated = match state.auth.authenticate(request.headers()) {
        Ok(auth) => auth,
        Err(e) => return e.into_response(),
    };
    if auth::is_write_method(request.method()) {
        if let Err(e) = auth::check_csrf(&authenticated, request.headers()) {
            return e.into_response();
        }
    }
    request.extensions_mut().insert(authenticated);
    next.run(request).await
}

/// 测试/工具用：构造认证状态（不参与路由）。
#[allow(dead_code)]
pub fn auth_state(config: &crate::config::AppConfig) -> AuthState {
    AuthState::new(config)
}
