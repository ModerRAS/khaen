//! M1 认证端到端测试（PLAN §4.1/§4.2 / T34）：真实 TCP -> 真实 axum -> 临时状态目录的 auth.json。
//!
//! 覆盖：
//! - 公开端点（status/setup/login/healthz）与受保护 /api 的 401；
//! - setup 201 + Cookie 属性 + argon2id PHC 存储 + 0600（Unix）/非明文；
//! - 校验错误稳定码、已配置 409、未配置登录 409、错密码 401；
//! - 同来源 5 次/60s 后 429 + Retry-After；
//! - 会话端点、CSRF 缺失/错误 403、X-KHAEN-Token 等价回退（免 CSRF）；
//! - logout 幂等 + 清 Cookie；改密码后旧会话失效、新会话可用、旧密码失效；
//! - daemon 重启（第二个 DaemonState/router + 同一个 auth.json）后旧会话仍可校验；
//! - 会话过期拒绝；/api 响应版本/缓存头；/healthz 不带这些头（M0 字段集不变）。

mod support;

use std::sync::Arc;

use khaen_daemon::auth::{
    code, AuthConfig, API_VERSION_HEADER, CSRF_BYTES, CSRF_HEADER, MIN_PASSWORD_LEN,
    SESSION_COOKIE, SESSION_TTL_SECS, TOKEN_HEADER, WEB_VERSION_HEADER,
};
use khaen_daemon::{API_VERSION, WEB_VERSION};
use serde_json::{json, Value};
use support::{HttpResponse, TempStateDir, TEST_PASSWORD};

const NEW_PASSWORD: &str = "a brand new long password";

fn cookie(token: &str) -> (String, String) {
    ("Cookie".to_string(), format!("{SESSION_COOKIE}={token}"))
}

fn header(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

fn is_hex(value: &str, bytes: usize) -> bool {
    value.len() == bytes * 2 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sorted_keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

/// 断言 PLAN §4.1 的固定错误体 {"error":{"code","message","details":{}}}。
fn assert_auth_error(response: &HttpResponse, status: u16, expected_code: &str) {
    assert_eq!(
        response.status,
        status,
        "unexpected status for {}",
        response.body_text()
    );
    assert_eq!(response.header("content-type"), Some("application/json"));
    let body = response.json();
    assert_eq!(
        sorted_keys(&body),
        vec!["error".to_string()],
        "错误体顶层只有 error: {body}"
    );
    let error = body["error"].as_object().expect("error object");
    let mut keys: Vec<&String> = error.keys().collect();
    keys.sort();
    assert_eq!(keys, vec!["code", "details", "message"]);
    assert_eq!(
        error["code"].as_str(),
        Some(expected_code),
        "稳定错误码: {body}"
    );
    assert!(error["message"]
        .as_str()
        .is_some_and(|message| !message.is_empty()));
    assert_eq!(error["details"], json!({}));
}

fn credentials(response: &HttpResponse) -> (String, String) {
    let token = response
        .session_token()
        .expect("auth response must set khaen_session cookie");
    let csrf = response.json()["csrf"]
        .as_str()
        .expect("auth response must contain csrf")
        .to_string();
    (token, csrf)
}

fn session_body(token: &str) -> Vec<(String, String)> {
    vec![cookie(token)]
}

fn is_rfc3339_utc(value: &str) -> bool {
    value.len() == 20
        && value.ends_with('Z')
        && value.as_bytes()[4] == b'-'
        && value.as_bytes()[7] == b'-'
        && value.as_bytes()[10] == b'T'
        && value.as_bytes()[13] == b':'
        && value.as_bytes()[16] == b':'
}

// ---------------------------------------------------------------------------
// 公开/受保护端点
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn status_and_healthz_are_public_but_other_api_requires_session() {
    let dir = TempStateDir::new("auth-public");
    let server = support::spawn_at(dir.path()).await;

    let status = support::request(&server, "GET", "/api/auth/status").await;
    assert_eq!(status.status, 200);
    assert_eq!(
        sorted_keys(&status.json()),
        vec!["authenticated".to_string(), "configured".to_string()]
    );
    assert_eq!(status.json()["configured"], Value::Bool(false));
    assert_eq!(status.json()["authenticated"], Value::Bool(false));

    // /healthz 仍然匿名可用（M0 字段集不变：只断言可用 + 字段集三字段）。
    let healthz = support::request(&server, "GET", "/healthz").await;
    assert_eq!(healthz.status, 200);
    assert_eq!(healthz.json()["ok"], Value::Bool(true));
    assert_eq!(
        healthz.json().as_object().map(|object| object.len()),
        Some(3),
        "healthz 字段集必须保持 {{ok,version,uptime_s}}"
    );

    // 其余 /api 无会话一律 401 + PLAN 错误体。
    for path in ["/api/status", "/api/meta", "/api/device", "/api/coeff"] {
        let response = support::request(&server, "GET", path).await;
        assert_auth_error(&response, 401, code::UNAUTHORIZED);
    }
    // 写方法同样先 401（不会落到 handler）。
    let response = support::request_json(
        &server,
        "POST",
        "/api/mute",
        &json!({ "mute": true }).to_string(),
    )
    .await;
    assert_auth_error(&response, 401, code::UNAUTHORIZED);
}

#[tokio::test(flavor = "current_thread")]
async fn api_responses_carry_version_and_no_store_headers() {
    let server = support::spawn().await;
    let anonymous = support::request_with(&server, "GET", "/api/status", &[], None).await;
    assert_auth_error(&anonymous, 401, code::UNAUTHORIZED);

    for response in [
        support::request(&server, "GET", "/api/status").await,
        support::request(&server, "GET", "/api/meta").await,
        support::request(&server, "GET", "/api/auth/status").await,
        anonymous,
    ] {
        assert_eq!(
            response.header(API_VERSION_HEADER),
            Some(API_VERSION.to_string().as_str()),
            "缺 X-KHAEN-API-Version"
        );
        assert_eq!(
            response.header(WEB_VERSION_HEADER),
            Some(WEB_VERSION),
            "缺 X-KHAEN-Web-Version"
        );
        assert_eq!(
            response.header("cache-control"),
            Some("no-store"),
            "缺 Cache-Control: no-store"
        );
    }

    // /healthz 不属于 /api：M0 语义保持不变（不带 API 头）。
    let healthz = support::request(&server, "GET", "/healthz").await;
    assert_eq!(healthz.status, 200);
    assert!(healthz.header(API_VERSION_HEADER).is_none());
}

// ---------------------------------------------------------------------------
// setup
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn setup_stores_argon2id_and_issues_a_session_cookie() {
    let dir = TempStateDir::new("auth-setup");
    let server = support::spawn_at(dir.path()).await;
    let body = json!({ "password": TEST_PASSWORD, "confirm": TEST_PASSWORD }).to_string();
    let response = support::request_json(&server, "POST", "/api/auth/setup", &body).await;
    assert_eq!(response.status, 201, "{}", response.body_text());

    let payload = response.json();
    assert_eq!(
        sorted_keys(&payload),
        vec!["csrf".to_string(), "expires_in".to_string()],
        "setup 响应只含 {{csrf,expires_in}}: {payload}"
    );
    let csrf = payload["csrf"].as_str().expect("csrf");
    assert!(is_hex(csrf, CSRF_BYTES), "csrf 必须是 32B 十六进制: {csrf}");
    assert_eq!(
        payload["expires_in"].as_u64(),
        Some(SESSION_TTL_SECS),
        "会话 8h"
    );

    // Cookie 属性：HttpOnly; SameSite=Strict; Path=/（LAN HTTP 不加 Secure）。
    let set_cookie = response.header("set-cookie").expect("Set-Cookie");
    assert!(set_cookie.starts_with("khaen_session="), "{set_cookie}");
    assert!(set_cookie.contains("Path=/"), "{set_cookie}");
    assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
    assert!(set_cookie.contains("SameSite=Strict"), "{set_cookie}");
    assert!(
        !set_cookie.contains("Secure"),
        "LAN HTTP 不能加 Secure: {set_cookie}"
    );
    assert!(
        set_cookie.contains(&format!("Max-Age={SESSION_TTL_SECS}")),
        "{set_cookie}"
    );

    // 会话即可用：session 端点返回 csrf / expires_at。
    let token = response.session_token().expect("session token");
    let session = support::request_with(
        &server,
        "GET",
        "/api/auth/session",
        &session_body(&token),
        None,
    )
    .await;
    assert_eq!(session.status, 200, "{}", session.body_text());
    assert_eq!(
        sorted_keys(&session.json()),
        vec![
            "authenticated".to_string(),
            "csrf".to_string(),
            "expires_at".to_string()
        ]
    );
    assert_eq!(session.json()["authenticated"], Value::Bool(true));
    assert_eq!(session.json()["csrf"].as_str(), Some(csrf));
    assert!(
        session.json()["expires_at"]
            .as_str()
            .is_some_and(is_rfc3339_utc),
        "expires_at 必须是 RFC3339 UTC: {}",
        session.json()["expires_at"]
    );

    // 已配置后 status 如实上报。
    let status = support::request_with(
        &server,
        "GET",
        "/api/auth/status",
        &session_body(&token),
        None,
    )
    .await;
    assert_eq!(status.json()["configured"], Value::Bool(true));
    assert_eq!(status.json()["authenticated"], Value::Bool(true));

    // auth.json：存在、非明文密码、argon2id PHC；Unix 上 0600。
    let store = std::fs::read_to_string(dir.auth_path()).expect("auth.json must exist");
    assert!(
        !store.contains(TEST_PASSWORD),
        "auth.json 不能存明文密码: {store}"
    );
    assert!(
        !store.contains(&token),
        "auth.json 不能存裸会话 token: {store}"
    );
    assert!(store.contains("$argon2id$"), "必须是 argon2id PHC: {store}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.auth_path())
            .expect("auth.json metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "auth.json 权限必须是 0600，实际 {mode:o}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn setup_validation_errors_are_stable() {
    let dir = TempStateDir::new("auth-setup-invalid");
    let server = support::spawn_at(dir.path()).await;

    // 过短密码：400 weak_password。
    let short = "x".repeat(MIN_PASSWORD_LEN - 1);
    let body = json!({ "password": short, "confirm": short }).to_string();
    let response = support::request_json(&server, "POST", "/api/auth/setup", &body).await;
    assert_auth_error(&response, 400, code::WEAK_PASSWORD);

    // confirm 不一致：400 password_mismatch。
    let body = json!({ "password": TEST_PASSWORD, "confirm": "different confirm" }).to_string();
    let response = support::request_json(&server, "POST", "/api/auth/setup", &body).await;
    assert_auth_error(&response, 400, code::PASSWORD_MISMATCH);

    // 缺 confirm：400 missing_field。
    let body = json!({ "password": TEST_PASSWORD }).to_string();
    let response = support::request_json(&server, "POST", "/api/auth/setup", &body).await;
    assert_auth_error(&response, 400, code::MISSING_FIELD);

    // confirm 类型不对：400 invalid_field。
    let body = json!({ "password": TEST_PASSWORD, "confirm": 42 }).to_string();
    let response = support::request_json(&server, "POST", "/api/auth/setup", &body).await;
    assert_auth_error(&response, 400, code::INVALID_FIELD);

    // 非法 JSON / 非对象：400 invalid_json。
    for body in ["{not json", "[]"] {
        let response = support::request_json(&server, "POST", "/api/auth/setup", body).await;
        assert_auth_error(&response, 400, code::INVALID_JSON);
    }

    // 失败的 setup 不能把服务改成已配置。
    let status = support::request(&server, "GET", "/api/auth/status").await;
    assert_eq!(status.json()["configured"], Value::Bool(false));

    // 合法 setup 成功；再 setup 一次 409 already_configured。
    let body = json!({ "password": TEST_PASSWORD, "confirm": TEST_PASSWORD }).to_string();
    let response = support::request_json(&server, "POST", "/api/auth/setup", &body).await;
    assert_eq!(response.status, 201, "{}", response.body_text());
    let response = support::request_json(&server, "POST", "/api/auth/setup", &body).await;
    assert_auth_error(&response, 409, code::ALREADY_CONFIGURED);
}

// ---------------------------------------------------------------------------
// login
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn login_requires_setup_and_reports_bad_password() {
    let dir = TempStateDir::new("auth-login");
    let server = support::spawn_at(dir.path()).await;
    let body = json!({ "password": TEST_PASSWORD }).to_string();

    // 未配置前禁止登录：409 not_configured。
    let response = support::request_json(&server, "POST", "/api/auth/login", &body).await;
    assert_auth_error(&response, 409, code::NOT_CONFIGURED);

    // setup 后错密码：401 invalid_credentials（无 Set-Cookie）。
    let setup = json!({ "password": TEST_PASSWORD, "confirm": TEST_PASSWORD }).to_string();
    let response = support::request_json(&server, "POST", "/api/auth/setup", &setup).await;
    assert_eq!(response.status, 201);
    let wrong = json!({ "password": "definitely the wrong password" }).to_string();
    let response = support::request_json(&server, "POST", "/api/auth/login", &wrong).await;
    assert_auth_error(&response, 401, code::INVALID_CREDENTIALS);
    assert!(response.header("set-cookie").is_none());

    // 正确密码：200 {csrf, expires_in} + 新 Cookie，新会话可用。
    let response = support::request_json(&server, "POST", "/api/auth/login", &body).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert_eq!(
        sorted_keys(&response.json()),
        vec!["csrf".to_string(), "expires_in".to_string()]
    );
    let (token, _csrf) = credentials(&response);
    let session = support::request_with(
        &server,
        "GET",
        "/api/auth/session",
        &session_body(&token),
        None,
    )
    .await;
    assert_eq!(session.status, 200);
    assert_eq!(session.json()["authenticated"], Value::Bool(true));
}

#[tokio::test(flavor = "current_thread")]
async fn login_rate_limit_is_429_with_retry_after() {
    let server = support::spawn().await;
    let wrong = json!({ "password": "wrong password value" }).to_string();
    for attempt in 1..=5 {
        let response = support::request_json(&server, "POST", "/api/auth/login", &wrong).await;
        assert_auth_error(&response, 401, code::INVALID_CREDENTIALS);
        assert_eq!(
            response.header("retry-after"),
            None,
            "前 5 次失败不应限速（attempt {attempt}）"
        );
    }
    // 第 6 次（即使密码正确）也 429 + Retry-After。
    let correct = json!({ "password": TEST_PASSWORD }).to_string();
    let response = support::request_json(&server, "POST", "/api/auth/login", &correct).await;
    assert_auth_error(&response, 429, code::RATE_LIMITED);
    assert!(response.header("set-cookie").is_none());
    let retry_after: u64 = response
        .header("retry-after")
        .expect("Retry-After")
        .parse()
        .expect("Retry-After 秒数");
    assert!(
        (1..=60).contains(&retry_after),
        "Retry-After 应在 1..=60s: {retry_after}"
    );
    // 限速窗口内继续 429。
    let response = support::request_json(&server, "POST", "/api/auth/login", &correct).await;
    assert_auth_error(&response, 429, code::RATE_LIMITED);
}

// ---------------------------------------------------------------------------
// 会话 / CSRF / token
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn session_endpoint_requires_a_session() {
    let server = support::spawn().await;
    let anonymous = support::request_with(&server, "GET", "/api/auth/session", &[], None).await;
    assert_auth_error(&anonymous, 401, code::UNAUTHORIZED);
    let bogus_cookie = vec![cookie("not-a-real-session-token")];
    let anonymous =
        support::request_with(&server, "GET", "/api/auth/session", &bogus_cookie, None).await;
    assert_auth_error(&anonymous, 401, code::UNAUTHORIZED);

    let session = server.session().expect("authed server");
    let response = support::request_with(
        &server,
        "GET",
        "/api/auth/session",
        &session_body(&session.token),
        None,
    )
    .await;
    assert_eq!(response.status, 200);
    assert_eq!(
        response.json()["csrf"].as_str(),
        Some(session.csrf.as_str())
    );
    assert!(is_hex(&session.csrf, CSRF_BYTES));
}

#[tokio::test(flavor = "current_thread")]
async fn csrf_is_required_for_cookie_writes_and_token_exempt() {
    let server = support::spawn().await;
    let session = server.session().expect("authed server");
    let mute = json!({ "mute": true }).to_string();

    // Cookie 会话写方法缺 CSRF -> 403 forbidden。
    let response = support::request_with(
        &server,
        "POST",
        "/api/mute",
        &session_body(&session.token),
        Some(&mute),
    )
    .await;
    assert_auth_error(&response, 403, code::FORBIDDEN);

    // 错误 CSRF -> 403。
    let mut wrong = session_body(&session.token);
    wrong.push(header(
        CSRF_HEADER,
        "0000000000000000000000000000000000000000000000000000000000000000",
    ));
    let response = support::request_with(&server, "POST", "/api/mute", &wrong, Some(&mute)).await;
    assert_auth_error(&response, 403, code::FORBIDDEN);

    // 正确 CSRF -> 200（原有写方法语义不变）。
    let mut right = session_body(&session.token);
    right.push(header(CSRF_HEADER, &session.csrf));
    let response = support::request_with(&server, "POST", "/api/mute", &right, Some(&mute)).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert_eq!(response.json()["mute"], Value::Bool(true));

    // token 认证免 CSRF：只带 X-KHAEN-Token 的 POST 可以成功。
    let token_only = vec![header(TOKEN_HEADER, &session.token)];
    let response = support::request_with(
        &server,
        "POST",
        "/api/mute",
        &token_only,
        Some(&json!({ "mute": false }).to_string()),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert_eq!(response.json()["mute"], Value::Bool(false));

    // 读方法带 Cookie 即可。
    let response = support::request_with(
        &server,
        "GET",
        "/api/status",
        &session_body(&session.token),
        None,
    )
    .await;
    assert_eq!(response.status, 200);
}

#[tokio::test(flavor = "current_thread")]
async fn token_header_is_a_cookie_equivalent() {
    let server = support::spawn().await;
    let session = server.session().expect("authed server");
    let with_token = vec![header(TOKEN_HEADER, &session.token)];
    let response = support::request_with(&server, "GET", "/api/status", &with_token, None).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert_eq!(
        response.json()["schema"],
        Value::String("khaen-status/v1".into())
    );

    // 错误 token 与空 token 都 401。
    for value in ["invalid-token", ""] {
        let headers = vec![header(TOKEN_HEADER, value)];
        let response = support::request_with(&server, "GET", "/api/status", &headers, None).await;
        assert_auth_error(&response, 401, code::UNAUTHORIZED);
    }
}

// ---------------------------------------------------------------------------
// logout
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn logout_clears_session_and_is_idempotent() {
    let server = support::spawn().await;
    let session = server.session().expect("authed server");
    let (token, csrf) = (session.token.clone(), session.csrf.clone());

    // 缺 CSRF 的 logout 仍按写方法规则 403（会话还在）。
    let response = support::request_with(
        &server,
        "POST",
        "/api/auth/logout",
        &session_body(&token),
        None,
    )
    .await;
    assert_auth_error(&response, 403, code::FORBIDDEN);

    // 正确 logout：200 + 清 Cookie（Max-Age=0）。
    let mut headers = session_body(&token);
    headers.push(header(CSRF_HEADER, &csrf));
    let response = support::request_with(&server, "POST", "/api/auth/logout", &headers, None).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let set_cookie = response.header("set-cookie").expect("clear Set-Cookie");
    assert!(set_cookie.starts_with("khaen_session=;"), "{set_cookie}");
    assert!(set_cookie.contains("Max-Age=0"), "{set_cookie}");

    // 旧会话已失效。
    let response = support::request_with(
        &server,
        "GET",
        "/api/auth/session",
        &session_body(&token),
        None,
    )
    .await;
    assert_auth_error(&response, 401, code::UNAUTHORIZED);

    // 幂等：没有会话的 logout 也 200 + 清 Cookie。
    let response = support::request_with(&server, "POST", "/api/auth/logout", &[], None).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let set_cookie = response.header("set-cookie").expect("clear Set-Cookie");
    assert!(set_cookie.contains("Max-Age=0"), "{set_cookie}");
}

// ---------------------------------------------------------------------------
// 改密码
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn password_change_invalidates_old_sessions() {
    let server = support::spawn().await;
    let first = server.session().expect("authed server").clone();

    let change = |old: &str, new: &str| {
        let body = json!({ "old": old, "new": new }).to_string();
        let mut headers = session_body(&first.token);
        headers.push(header(CSRF_HEADER, &first.csrf));
        (body, headers)
    };

    // 旧密码错误：401 invalid_credentials，旧会话仍有效。
    let (body, headers) = change("wrong old password", NEW_PASSWORD);
    let response =
        support::request_with(&server, "POST", "/api/auth/password", &headers, Some(&body)).await;
    assert_auth_error(&response, 401, code::INVALID_CREDENTIALS);
    let still_valid = support::request_with(
        &server,
        "GET",
        "/api/auth/session",
        &session_body(&first.token),
        None,
    )
    .await;
    assert_eq!(still_valid.status, 200);

    // 新密码过短：400 weak_password，旧会话仍有效。
    let (body, headers) = change(TEST_PASSWORD, "short");
    let response =
        support::request_with(&server, "POST", "/api/auth/password", &headers, Some(&body)).await;
    assert_auth_error(&response, 400, code::WEAK_PASSWORD);

    // 缺 CSRF：403。
    let body = json!({ "old": TEST_PASSWORD, "new": NEW_PASSWORD }).to_string();
    let response = support::request_with(
        &server,
        "POST",
        "/api/auth/password",
        &session_body(&first.token),
        Some(&body),
    )
    .await;
    assert_auth_error(&response, 403, code::FORBIDDEN);

    // 正确改密：200 + 新会话；旧会话立刻失效。
    let (body, headers) = change(TEST_PASSWORD, NEW_PASSWORD);
    let response =
        support::request_with(&server, "POST", "/api/auth/password", &headers, Some(&body)).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let (new_token, new_csrf) = credentials(&response);
    assert_ne!(new_token, first.token, "必须签发新会话");
    let old_session = support::request_with(
        &server,
        "GET",
        "/api/auth/session",
        &session_body(&first.token),
        None,
    )
    .await;
    assert_auth_error(&old_session, 401, code::UNAUTHORIZED);
    let new_session = support::request_with(
        &server,
        "GET",
        "/api/auth/session",
        &session_body(&new_token),
        None,
    )
    .await;
    assert_eq!(new_session.status, 200);
    assert_eq!(new_session.json()["csrf"].as_str(), Some(new_csrf.as_str()));

    // 旧密码失效（401），新密码可登录（200）。
    let old_login = json!({ "password": TEST_PASSWORD }).to_string();
    let response = support::request_json(&server, "POST", "/api/auth/login", &old_login).await;
    assert_auth_error(&response, 401, code::INVALID_CREDENTIALS);
    let new_login = json!({ "password": NEW_PASSWORD }).to_string();
    let response = support::request_json(&server, "POST", "/api/auth/login", &new_login).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
}

// ---------------------------------------------------------------------------
// 重启持久化 / 过期
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn sessions_survive_restart_via_auth_json() {
    let dir = Arc::new(TempStateDir::new("auth-restart"));
    let first_server = support::spawn_at(dir.path()).await;
    let session = support::bootstrap_session(&first_server).await;
    let first_session = support::request_with(
        &first_server,
        "GET",
        "/api/auth/session",
        &session_body(&session.token),
        None,
    )
    .await;
    assert_eq!(first_session.status, 200);
    let expires_at = first_session.json()["expires_at"]
        .as_str()
        .expect("expires_at")
        .to_string();
    drop(first_server);

    // 第二个 DaemonState/router，共享同一个 auth.json（模拟 daemon 重启）。
    let second_server = support::spawn_at(dir.path()).await;
    let status = support::request(&second_server, "GET", "/api/auth/status").await;
    assert_eq!(status.status, 200);
    assert_eq!(
        status.json()["configured"],
        Value::Bool(true),
        "重启后 configured 必须从 auth.json 恢复"
    );
    assert_eq!(
        status.json()["authenticated"],
        Value::Bool(false),
        "无 Cookie 时不得冒充已登录"
    );

    // 旧 Cookie 在重启后仍可校验（会话持久化在 auth.json）。
    let session_after_restart = support::request_with(
        &second_server,
        "GET",
        "/api/auth/session",
        &session_body(&session.token),
        None,
    )
    .await;
    assert_eq!(
        session_after_restart.status,
        200,
        "重启后旧会话必须仍可用: {}",
        session_after_restart.body_text()
    );
    assert_eq!(
        session_after_restart.json()["csrf"].as_str(),
        Some(session.csrf.as_str())
    );
    assert_eq!(
        session_after_restart.json()["expires_at"].as_str(),
        Some(expires_at.as_str())
    );

    // 同一密码仍可登录（auth.json 里的 PHC 可校验）；错密码 401。
    let login = json!({ "password": TEST_PASSWORD }).to_string();
    let response = support::request_json(&second_server, "POST", "/api/auth/login", &login).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let wrong = json!({ "password": "wrong after restart" }).to_string();
    let response = support::request_json(&second_server, "POST", "/api/auth/login", &wrong).await;
    assert_auth_error(&response, 401, code::INVALID_CREDENTIALS);
}

#[tokio::test(flavor = "current_thread")]
async fn expired_sessions_are_rejected() {
    let dir = TempStateDir::new("auth-expiry");
    let config = AuthConfig::for_tests(dir.path()).with_session_ttl_secs(0);
    let server = support::spawn_at_with_config(dir.path(), config).await;
    let session = support::bootstrap_session(&server).await;

    let response = support::request_with(
        &server,
        "GET",
        "/api/auth/session",
        &session_body(&session.token),
        None,
    )
    .await;
    assert_auth_error(&response, 401, code::UNAUTHORIZED);

    // status 端点带过期 Cookie 时如实报 authenticated=false。
    let status = support::request_with(
        &server,
        "GET",
        "/api/auth/status",
        &session_body(&session.token),
        None,
    )
    .await;
    assert_eq!(status.status, 200);
    assert_eq!(status.json()["configured"], Value::Bool(true));
    assert_eq!(status.json()["authenticated"], Value::Bool(false));
}
