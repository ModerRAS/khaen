// T42 私有测试：自有 REST 控制面（R01-R51/X01-X22、R47/R48/R49、actual commit ack）。
//
// 本文件由 crates/control/src/rest.rs 的 #[cfg(test)] 子模块通过 include! 编译执行
// （见 tests/T42/README.md；不改 integrator 独占的 public_registration.rs）。
// 运行：cargo test -p khaen-control rest
//
// 全部离线：不打开设备、不启服务、不跑真实 host；engine/calibration 用本 crate 的
// MemoryEngine/MemoryCalibration 或测试内 ScriptedCalibration 替身。

// use super::* 由外层 #[cfg(test)] 模块提供（include! 共享作用域）

use std::collections::BTreeMap;

const TEST_TOKEN: &str = "test-token-0123456789abcdef";
const TEST_PASSWORD: &str = "correct horse battery staple";
const P52_PROFILE: &str = "p1-412";

fn test_config() -> RestConfig {
    RestConfig {
        api_token: Some(TEST_TOKEN.to_string()),
        pbkdf2_iterations: 100,
        ..RestConfig::default()
    }
}

fn service_cfg(config: RestConfig) -> RestService {
    RestService::with_config(Box::new(MemoryEngine::new()), config)
}

fn service() -> RestService {
    service_cfg(test_config())
}

fn service_engine(engine: MemoryEngine) -> RestService {
    RestService::with_config(Box::new(engine), test_config())
}

fn authed(request: RestRequest) -> RestRequest {
    request.header(TOKEN_HEADER, TEST_TOKEN)
}

fn get(path: &str) -> RestRequest {
    authed(RestRequest::get(path))
}

fn post(path: &str) -> RestRequest {
    authed(RestRequest::post(path))
}

fn post_json(path: &str, body: &str) -> RestRequest {
    authed(RestRequest::post(path).json_body(body))
}

fn put_json(path: &str, body: &str) -> RestRequest {
    authed(RestRequest::put(path).json_body(body))
}

fn put_raw(path: &str) -> RestRequest {
    authed(RestRequest::put(path))
}

fn json_body(response: &RestResponse) -> JsonValue {
    JsonValue::parse(&response.body).expect("response body must be valid JSON")
}

fn expect_str(value: &JsonValue, key: &str) -> String {
    match value.get(key) {
        Some(JsonValue::Str(text)) => text.clone(),
        other => panic!("field {key}: expected string, got {other:?}"),
    }
}

fn expect_num(value: &JsonValue, key: &str) -> f64 {
    match value.get(key) {
        Some(JsonValue::Num(number)) => *number,
        other => panic!("field {key}: expected number, got {other:?}"),
    }
}

fn expect_bool(value: &JsonValue, key: &str) -> bool {
    match value.get(key) {
        Some(JsonValue::Bool(flag)) => *flag,
        other => panic!("field {key}: expected bool, got {other:?}"),
    }
}

fn expect_obj<'a>(value: &'a JsonValue, key: &str) -> &'a JsonValue {
    match value.get(key) {
        Some(child @ JsonValue::Obj(_)) => child,
        other => panic!("field {key}: expected object, got {other:?}"),
    }
}

fn expect_arr_len(value: &JsonValue, key: &str) -> usize {
    match value.get(key) {
        Some(JsonValue::Arr(items)) => items.len(),
        other => panic!("field {key}: expected array, got {other:?}"),
    }
}

fn expect_null(value: &JsonValue, key: &str) {
    match value.get(key) {
        Some(JsonValue::Null) => {}
        other => panic!("field {key}: expected null, got {other:?}"),
    }
}

fn error_code(response: &RestResponse) -> String {
    let body = json_body(response);
    let error = expect_obj(&body, "error");
    expect_str(error, "code")
}

fn assert_status(response: &RestResponse, status: u16) {
    assert_eq!(response.status, status, "body={}", response.body);
}

fn assert_error(response: &RestResponse, status: u16, code: &str) {
    assert_status(response, status);
    assert_eq!(error_code(response), code, "body={}", response.body);
    assert!(
        response.body.contains("\"error\""),
        "error envelope missing: {}",
        response.body
    );
}

fn cookie_from(response: &RestResponse) -> String {
    let raw = response
        .header_value("Set-Cookie")
        .expect("Set-Cookie header");
    raw.split(';').next().expect("cookie pair").to_string()
}

fn csrf_from(response: &RestResponse) -> String {
    let body = json_body(response);
    expect_str(&body, "csrf_token")
}

fn cookie_request(method: Method, path: &str, cookie: &str) -> RestRequest {
    RestRequest::new(method, path).header("Cookie", cookie)
}

fn setup_session(service: &RestService) -> (String, String) {
    let body = format!("{{\"password\":\"{TEST_PASSWORD}\",\"confirm\":\"{TEST_PASSWORD}\"}}");
    let response = service.handle(&RestRequest::post("/api/auth/setup").json_body(&body));
    assert_status(&response, 200);
    (cookie_from(&response), csrf_from(&response))
}

fn p1_target_body() -> String {
    format!(
        "{{\"layout\":\"4.1.2\",\"channels\":8,\"profile\":\"{P52_PROFILE}\",\"roles\":[{{\"role\":\"FL\",\"dac\":0}},{{\"role\":\"FR\",\"dac\":1}},{{\"role\":\"SL\",\"dac\":2}},{{\"role\":\"SR\",\"dac\":3}},{{\"role\":\"HFL\",\"dac\":4}},{{\"role\":\"HFR\",\"dac\":5}},{{\"role\":\"SUB1\",\"dac\":7}}],\"low_band_dacs\":[0,1,2,3,7]}}"
    )
}

fn baseline_coeff() -> CoeffRef {
    CoeffRef::new("cal-baseline", hex_lower(&sha256(b"cal-baseline"))).expect("baseline coeff")
}

fn is_hex64(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

// ---------------------------------------------------------------------------
// R01/R50/X01：healthz、安全头、API 版本、404/405
// ---------------------------------------------------------------------------

#[test]
fn t42_rest_healthz_is_anonymous_and_has_r01_fields() {
    let service = service();
    let response = service.handle(&RestRequest::get("/healthz"));
    assert_status(&response, 200);
    assert_eq!(response.header_value(API_VERSION_HEADER), Some("1"));
    let body = json_body(&response);
    assert_eq!(expect_str(&body, "status"), "ok");
    assert_eq!(expect_str(&body, "version"), REST_VERSION);
    assert!(!expect_bool(&body, "mock"));
    assert!(expect_num(&body, "uptime_secs") >= 0.0);
    assert!(expect_num(&body, "now_unix") > 0.0);
}

#[test]
fn t42_rest_security_headers_present_on_success_and_error() {
    let service = service();
    for response in [
        service.handle(&RestRequest::get("/healthz")),
        service.handle(&RestRequest::get("/api/not-a-real-path")),
        service.handle(&RestRequest::get("/api/runtime/status")),
    ] {
        for (name, value) in SECURITY_HEADERS {
            assert_eq!(
                response.header_value(name),
                Some(value),
                "missing header {name}"
            );
        }
        assert_eq!(
            response.header_value(API_VERSION_HEADER),
            Some(API_VERSION_RESPONSE_VALUE)
        );
    }
}

#[test]
fn t42_rest_api_version_header_is_optional_and_rejects_incompatible() {
    let service = service();
    let ok = service.handle(&RestRequest::get("/healthz").header(API_VERSION_HEADER, "1"));
    assert_status(&ok, 200);
    let also_ok = service.handle(&RestRequest::get("/healthz").header(API_VERSION_HEADER, "v1"));
    assert_status(&also_ok, 200);
    let rejected = service.handle(&RestRequest::get("/healthz").header(API_VERSION_HEADER, "2"));
    assert_error(&rejected, 400, CODE_UNSUPPORTED_API_VERSION);
    assert_eq!(
        rejected.header_value(API_VERSION_HEADER),
        Some(API_VERSION_RESPONSE_VALUE)
    );
    let no_header = service.handle(&RestRequest::get("/healthz"));
    assert_status(&no_header, 200);
}

#[test]
fn t42_rest_unknown_paths_are_json_404() {
    let service = service();
    let api = service.handle(&get("/api/definitely-not-a-route"));
    assert_error(&api, 404, CODE_NOT_FOUND);
    let non_api = service.handle(&get("/static/app.js"));
    assert_error(&non_api, 404, CODE_NOT_FOUND);
}

#[test]
fn t42_rest_known_path_wrong_method_405() {
    let service = service();
    let response = service.handle(&post("/api/runtime/volume"));
    assert_error(&response, 405, CODE_METHOD_NOT_ALLOWED);
    let response = service.handle(&get("/api/runtime/commit"));
    assert_error(&response, 405, CODE_METHOD_NOT_ALLOWED);
    let response = service.handle(&get("/api/auth/setup"));
    assert_error(&response, 405, CODE_METHOD_NOT_ALLOWED);
}

#[test]
fn t42_rest_protected_routes_require_auth() {
    let service = service();
    for path in [
        "/api/runtime/status",
        "/api/runtime/config",
        "/api/runtime/volume",
        "/api/runtime/mute",
        "/api/mode",
        "/api/audio/format",
        "/api/calibration/jobs",
        "/api/auth/session",
    ] {
        let response = service.handle(&RestRequest::get(path));
        assert_error(&response, 401, CODE_UNAUTHORIZED);
    }
    let write = service.handle(&RestRequest::put("/api/runtime/volume").json_body("{\"db\":-10}"));
    assert_error(&write, 401, CODE_UNAUTHORIZED);
}

#[test]
fn t42_rest_header_token_bearer_and_legacy_token_are_accepted() {
    let service = service();
    let via_new =
        service.handle(&RestRequest::get("/api/runtime/status").header(TOKEN_HEADER, TEST_TOKEN));
    assert_status(&via_new, 200);
    let bearer = format!("Bearer {TEST_TOKEN}");
    let via_bearer = service
        .handle(&RestRequest::get("/api/runtime/status").header(AUTHORIZATION_HEADER, &bearer));
    assert_status(&via_bearer, 200);
    let legacy = service
        .handle(&RestRequest::get("/api/runtime/status").header(LEGACY_TOKEN_HEADER, TEST_TOKEN));
    assert_status(&legacy, 200);
}

#[test]
fn t42_rest_wrong_token_401_without_leaking_token() {
    let service = service();
    let response = service
        .handle(&RestRequest::get("/api/runtime/status").header(TOKEN_HEADER, "wrong-token-value"));
    assert_error(&response, 401, CODE_UNAUTHORIZED);
    assert!(!response.body.contains("wrong-token-value"));
    assert!(!response.body.contains(TEST_TOKEN));
}

#[test]
fn t42_rest_trailing_slash_is_normalized() {
    let service = service();
    let response = service.handle(&get("/api/runtime/status/"));
    assert_status(&response, 200);
    let response = service.handle(&get("/healthz/"));
    assert_status(&response, 200);
}

#[test]
fn t42_rest_routes_table_covers_new_surfaces() {
    let routes = RestService::routes();
    for expected in [
        ("GET", "/healthz"),
        ("GET", "/api/auth/status"),
        ("POST", "/api/auth/login"),
        ("GET", "/api/runtime/status"),
        ("PUT", "/api/runtime/config"),
        ("POST", "/api/runtime/prepare"),
        ("POST", "/api/runtime/commit"),
        ("POST", "/api/runtime/abort"),
        ("PUT", "/api/runtime/volume"),
        ("GET", "/api/runtime/mute"),
        ("PUT", "/api/runtime/mode"),
        ("PUT", "/api/runtime/target"),
        ("PUT", "/api/runtime/coeff"),
        ("GET", "/api/mode"),
        ("PUT", "/api/mode"),
        ("GET", "/api/audio/format"),
        ("PUT", "/api/audio/format"),
        ("GET", "/api/channelmap/input-mode"),
        ("POST", "/api/calibration/jobs"),
        ("POST", "/api/calibration/jobs/{id}/apply"),
        ("POST", "/api/calibration/jobs/{id}/rollback"),
    ] {
        assert!(routes.contains(&expected), "routes() missing {expected:?}");
    }
}

// ---------------------------------------------------------------------------
// R02-R06/K03：认证 / 会话 / CSRF / 失败锁定 / 有界会话
// ---------------------------------------------------------------------------

fn setup_body(password: &str, confirm: &str) -> String {
    format!("{{\"password\":\"{password}\",\"confirm\":\"{confirm}\"}}")
}

#[test]
fn t42_rest_auth_status_anonymous_fields() {
    let service = service();
    let response = service.handle(&RestRequest::get("/api/auth/status"));
    assert_status(&response, 200);
    let body = json_body(&response);
    assert!(!expect_bool(&body, "password_configured"));
    assert!(expect_bool(&body, "setup_required"));
    assert!(expect_bool(&body, "token_fallback"));
    assert!(!expect_bool(&body, "mock"));
    assert_eq!(
        expect_num(&body, "min_password_len"),
        MIN_PASSWORD_LEN as f64
    );
    assert_eq!(
        expect_num(&body, "recommended_password_len"),
        RECOMMENDED_PASSWORD_LEN as f64
    );
}

#[test]
fn t42_rest_auth_setup_validates_password_and_logs_in() {
    let service = service();
    let short = service
        .handle(&RestRequest::post("/api/auth/setup").json_body(&setup_body("short", "short")));
    assert_error(&short, 400, CODE_INVALID_FIELD);
    let mismatch = service.handle(
        &RestRequest::post("/api/auth/setup")
            .json_body(&setup_body(TEST_PASSWORD, "different-password")),
    );
    assert_error(&mismatch, 400, CODE_INVALID_FIELD);
    // 无效尝试不改状态：仍可首设成功。
    let (cookie, csrf) = setup_session(&service);
    assert!(cookie.starts_with("khaen_session="));
    assert_eq!(csrf.len(), 32);
    let status = service.handle(&RestRequest::get("/api/auth/status"));
    let body = json_body(&status);
    assert!(expect_bool(&body, "password_configured"));
    assert!(!expect_bool(&body, "setup_required"));
    let again = service.handle(
        &RestRequest::post("/api/auth/setup").json_body(&setup_body(TEST_PASSWORD, TEST_PASSWORD)),
    );
    assert_error(&again, 409, CODE_CONFLICT);
}

#[test]
fn t42_rest_auth_login_failure_lockout_and_recovery() {
    let service = service();
    setup_session(&service);
    for attempt in 1..=4 {
        let response = service.handle(
            &RestRequest::post("/api/auth/login")
                .json_body("{\"password\":\"wrong password value\"}"),
        );
        assert_error(&response, 401, CODE_UNAUTHORIZED);
        let _ = attempt;
    }
    let fifth = service.handle(
        &RestRequest::post("/api/auth/login").json_body("{\"password\":\"wrong password value\"}"),
    );
    assert_error(&fifth, 429, CODE_RATE_LIMITED);
    // 锁定期内即使密码正确也拒绝（R04）。
    let locked = service.handle(
        &RestRequest::post("/api/auth/login")
            .json_body(&format!("{{\"password\":\"{TEST_PASSWORD}\"}}")),
    );
    assert_error(&locked, 429, CODE_RATE_LIMITED);
}

#[test]
fn t42_rest_auth_login_token_and_argument_validation() {
    let service = service();
    let token_login = service.handle(
        &RestRequest::post("/api/auth/login").json_body(&format!("{{\"token\":\"{TEST_TOKEN}\"}}")),
    );
    assert_status(&token_login, 200);
    let body = json_body(&token_login);
    assert!(expect_bool(&body, "ok"));
    assert_eq!(expect_str(&body, "auth_kind"), "session");
    assert_eq!(csrf_from(&token_login).len(), 32);
    let both = service.handle(&RestRequest::post("/api/auth/login").json_body(&format!(
        "{{\"token\":\"{TEST_TOKEN}\",\"password\":\"{TEST_PASSWORD}\"}}"
    )));
    assert_error(&both, 400, CODE_INVALID_FIELD);
    let none = service.handle(&RestRequest::post("/api/auth/login").json_body("{}"));
    assert_error(&none, 400, CODE_MISSING_FIELD);
    let wrong_token = service
        .handle(&RestRequest::post("/api/auth/login").json_body("{\"token\":\"not-the-token\"}"));
    assert_error(&wrong_token, 401, CODE_UNAUTHORIZED);
    // 未设置密码时 password 登录 409（不冒充已配置）。
    let fresh = service_cfg(test_config());
    let unconfigured = fresh.handle(
        &RestRequest::post("/api/auth/login")
            .json_body(&format!("{{\"password\":\"{TEST_PASSWORD}\"}}")),
    );
    assert_error(&unconfigured, 409, CODE_CONFLICT);
}

#[test]
fn t42_rest_auth_session_and_logout() {
    let service = service();
    let (cookie, csrf) = setup_session(&service);
    let session = service.handle(&cookie_request(Method::Get, "/api/auth/session", &cookie));
    assert_status(&session, 200);
    let body = json_body(&session);
    assert!(expect_bool(&body, "authenticated"));
    assert_eq!(expect_str(&body, "auth_kind"), "session");
    assert_eq!(expect_str(&body, "csrf_token"), csrf);
    assert!(expect_num(&body, "expires_in") > 0.0);
    assert!(expect_num(&body, "created_unix") > 0.0);

    let token_session = service.handle(&get("/api/auth/session"));
    let body = json_body(&token_session);
    assert_eq!(expect_str(&body, "auth_kind"), "token");
    expect_null(&body, "csrf_token");

    // Cookie 会话注销是写方法：缺 CSRF 403。
    let no_csrf = service.handle(&cookie_request(Method::Post, "/api/auth/logout", &cookie));
    assert_error(&no_csrf, 403, CODE_FORBIDDEN);
    let logout = service.handle(
        &cookie_request(Method::Post, "/api/auth/logout", &cookie).header(CSRF_HEADER, &csrf),
    );
    assert_status(&logout, 200);
    let after = service.handle(&cookie_request(Method::Get, "/api/auth/session", &cookie));
    assert_error(&after, 401, CODE_UNAUTHORIZED);
    // token 注销免 CSRF 且幂等。
    let token_logout = service.handle(&post("/api/auth/logout"));
    assert_status(&token_logout, 200);
}

#[test]
fn t42_rest_csrf_required_for_session_writes_only() {
    let service = service();
    let (cookie, csrf) = setup_session(&service);
    let volume = "{\"volume_db\":-20}";
    let missing = service
        .handle(&cookie_request(Method::Put, "/api/runtime/volume", &cookie).json_body(volume));
    assert_error(&missing, 403, CODE_FORBIDDEN);
    let wrong = service.handle(
        &cookie_request(Method::Put, "/api/runtime/volume", &cookie)
            .header(CSRF_HEADER, "deadbeef")
            .json_body(volume),
    );
    assert_error(&wrong, 403, CODE_FORBIDDEN);
    let ok = service.handle(
        &cookie_request(Method::Put, "/api/runtime/volume", &cookie)
            .header(CSRF_HEADER, &csrf)
            .json_body(volume),
    );
    assert_status(&ok, 200);
    // 别名头也接受。
    let alias = service.handle(
        &cookie_request(Method::Put, "/api/runtime/mute", &cookie)
            .header(CSRF_HEADER_ALIAS, &csrf)
            .json_body("{\"mute\":true}"),
    );
    assert_status(&alias, 200);
    // header token 写方法免 CSRF（K03）。
    let token_write = service.handle(&put_json("/api/runtime/volume", "{\"db\":-6}"));
    assert_status(&token_write, 200);
}

#[test]
fn t42_rest_sessions_are_bounded_and_expire() {
    let mut config = test_config();
    config.max_sessions = 2;
    config.session_ttl_secs = 3600;
    let service = service_cfg(config);
    let (first_cookie, _) = setup_session(&service);
    let second = service.handle(
        &RestRequest::post("/api/auth/login")
            .json_body(&format!("{{\"password\":\"{TEST_PASSWORD}\"}}")),
    );
    assert_status(&second, 200);
    let third = service.handle(
        &RestRequest::post("/api/auth/login")
            .json_body(&format!("{{\"password\":\"{TEST_PASSWORD}\"}}")),
    );
    assert_status(&third, 200);
    let evicted = service.handle(&cookie_request(
        Method::Get,
        "/api/auth/session",
        &first_cookie,
    ));
    assert_error(&evicted, 401, CODE_UNAUTHORIZED);
    let latest = cookie_from(&third);
    let alive = service.handle(&cookie_request(Method::Get, "/api/auth/session", &latest));
    assert_status(&alive, 200);

    let mut expired_config = test_config();
    expired_config.session_ttl_secs = 0;
    let expired = service_cfg(expired_config);
    let (cookie, _) = setup_session(&expired);
    let response = expired.handle(&cookie_request(Method::Get, "/api/auth/session", &cookie));
    assert_error(&response, 401, CODE_UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// R49/K15/K56：runtime status/config + prepare/commit actual ack（不假 ack）
// ---------------------------------------------------------------------------

fn changed_target_body() -> String {
    p1_target_body().replace("\"profile\":\"p1-412\"", "\"profile\":\"p1-412-room-a\"")
}

fn prepared_config_body() -> String {
    format!(
        "{{\"mode\":\"target_multichannel\",\"sample_rate\":96000,\"chunksize\":64,\"target\":{}}}",
        changed_target_body()
    )
}

fn prepare_ok(service: &RestService) -> String {
    let response = service.handle(&post_json("/api/runtime/prepare", "{}"));
    assert_status(&response, 200);
    let body = json_body(&response);
    assert!(expect_bool(&body, "prepared"));
    let prepare_id = expect_str(&body, "prepare_id");
    assert!(prepare_id.starts_with("prep-"));
    assert!(is_hex64(&expect_str(&body, "planned_target_hash")));
    assert!(is_hex64(&expect_str(&body, "planned_config_hash")));
    prepare_id
}

fn commit_body_of(service: &RestService, prepare_id: &str) -> JsonValue {
    let body = format!("{{\"prepare_id\":\"{prepare_id}\"}}");
    let response = service.handle(&post_json("/api/runtime/commit", &body));
    assert_status(&response, 200);
    json_body(&response)
}

#[test]
fn t42_rest_runtime_status_reports_actual_identity() {
    let service = service();
    let response = service.handle(&get("/api/runtime/status"));
    assert_status(&response, 200);
    let body = json_body(&response);
    assert_eq!(expect_str(&body, "schema"), RUNTIME_STATUS_SCHEMA);
    assert_eq!(expect_str(&body, "runtime_state"), "Running");
    assert_eq!(expect_str(&body, "device_owner"), "playback");
    assert_eq!(expect_str(&body, "actual_version"), "v0001");
    assert!(is_hex64(&expect_str(&body, "graph_id")));
    assert_eq!(expect_str(&body, "mode"), "toslink");
    assert_eq!(expect_str(&body, "mode_profile"), "toslink-2.0");
    assert_eq!(expect_num(&body, "sample_rate"), 48000.0);
    assert_eq!(expect_num(&body, "chunksize"), 128.0);
    assert_eq!(expect_num(&body, "volume_db"), -12.0);
    assert!(!expect_bool(&body, "mute"));
    assert_eq!(expect_num(&body, "epoch"), 1.0);
    assert_eq!(expect_num(&body, "xruns"), 0.0);
    assert_eq!(expect_num(&body, "clipped_samples"), 0.0);
    assert_eq!(expect_arr_len(&body, "available_coeffs"), 1);
    expect_null(&body, "coeff");
    let target = expect_obj(&body, "target");
    assert_eq!(expect_str(target, "layout"), "4.1.2");
    assert_eq!(
        expect_str(&body, "target_hash"),
        TargetSpec::p1_412().fingerprint()
    );
    expect_null(&body, "pending_prepare");
    assert_eq!(expect_num(&body, "api_version"), 1.0);
}

#[test]
fn t42_rest_runtime_config_get_and_put_intent_separation() {
    let service = service();
    let response = service.handle(&get("/api/runtime/config"));
    assert_status(&response, 200);
    let body = json_body(&response);
    assert_eq!(expect_str(&body, "schema"), CONFIG_SCHEMA);
    assert!(!expect_bool(&body, "requires_commit"));
    assert_eq!(expect_str(&body, "actual_version"), "v0001");
    let config = expect_obj(&body, "config");
    assert_eq!(
        expect_str(config, "config_hash"),
        RuntimeConfig::default().fingerprint()
    );

    let update = service.handle(&put_json("/api/runtime/config", &prepared_config_body()));
    assert_status(&update, 200);
    let body = json_body(&update);
    assert!(expect_bool(&body, "requires_commit"));
    assert_eq!(expect_str(&body, "actual_version"), "v0001");
    let config = expect_obj(&body, "config");
    assert_eq!(expect_str(config, "mode"), "target_multichannel");
    assert_eq!(expect_num(config, "sample_rate"), 96000.0);
    assert_eq!(expect_num(config, "chunksize"), 64.0);
    let status = json_body(&service.handle(&get("/api/runtime/status")));
    assert_eq!(expect_str(&status, "mode"), "toslink");
    assert_eq!(expect_num(&status, "sample_rate"), 48000.0);
}

#[test]
fn t42_rest_runtime_config_rejects_bad_values_and_fields() {
    let service = service();
    assert_error(
        &service.handle(&put_json("/api/runtime/config", "{}")),
        400,
        CODE_MISSING_FIELD,
    );
    assert_error(
        &service.handle(&put_json("/api/runtime/config", "{\"bogus\":1}")),
        400,
        CODE_UNKNOWN_FIELD,
    );
    assert_error(
        &service.handle(&put_json("/api/runtime/config", "{\"mode\":\"bogus\"}")),
        400,
        CODE_UNKNOWN_MODE,
    );
    assert_error(
        &service.handle(&put_json("/api/runtime/config", "{\"chunksize\":256}")),
        400,
        CODE_UNSUPPORTED_VALUE,
    );
    assert_error(
        &service.handle(&put_json("/api/runtime/config", "{\"chunksize\":32}")),
        400,
        CODE_UNSUPPORTED_VALUE,
    );
    assert_error(
        &service.handle(&put_json("/api/runtime/config", "{\"sample_rate\":44100}")),
        400,
        CODE_UNSUPPORTED_VALUE,
    );
    let bad_target = p1_target_body().replace("\"role\":\"FL\"", "\"role\":\"SUB4\"");
    let body = format!("{{\"target\":{bad_target}}}");
    assert_error(
        &service.handle(&put_json("/api/runtime/config", &body)),
        400,
        CODE_UNKNOWN_ROLE,
    );
    let bad_coeff = "{\"coeff\":{\"version\":\"cal-baseline\",\"sha256\":\"deadbeef\"}}";
    assert_error(
        &service.handle(&put_json("/api/runtime/config", bad_coeff)),
        400,
        CODE_INVALID_FIELD,
    );
    let unknown_coeff = format!(
        "{{\"coeff\":{{\"version\":\"cal-nope\",\"sha256\":\"{}\"}}}}",
        hex_lower(&sha256(b"cal-nope"))
    );
    assert_error(
        &service.handle(&put_json("/api/runtime/config", &unknown_coeff)),
        400,
        CODE_UNKNOWN_COEFF,
    );
}

#[test]
fn t42_rest_prepare_commit_applies_actual_version_and_is_idempotent() {
    let service = service();
    let update = service.handle(&put_json("/api/runtime/config", &prepared_config_body()));
    assert_status(&update, 200);
    let prepare_id = prepare_ok(&service);
    let status_before_commit = json_body(&service.handle(&get("/api/runtime/status")));
    assert_eq!(expect_str(&status_before_commit, "mode"), "toslink");
    assert_eq!(
        expect_str(&status_before_commit, "pending_prepare"),
        prepare_id
    );

    let commit = commit_body_of(&service, &prepare_id);
    assert!(expect_bool(&commit, "committed"));
    assert!(!expect_bool(&commit, "replayed"));
    assert_eq!(expect_str(&commit, "prepare_id"), prepare_id);
    assert_eq!(expect_str(&commit, "actual_version"), "v0002");
    assert!(is_hex64(&expect_str(&commit, "graph_id")));
    assert_eq!(expect_num(&commit, "epoch"), 2.0);
    assert!(is_hex64(&expect_str(&commit, "target_hash")));

    let status = json_body(&service.handle(&get("/api/runtime/status")));
    assert_eq!(expect_str(&status, "mode"), "target_multichannel");
    assert_eq!(expect_num(&status, "sample_rate"), 96000.0);
    assert_eq!(expect_num(&status, "chunksize"), 64.0);
    assert_eq!(expect_num(&status, "epoch"), 2.0);
    expect_null(&status, "pending_prepare");
    let config = json_body(&service.handle(&get("/api/runtime/config")));
    assert!(!expect_bool(&config, "requires_commit"));

    // R49 幂等：同一 prepare_id 重复 commit 返回同一 actual ack。
    let replay = commit_body_of(&service, &prepare_id);
    assert!(expect_bool(&replay, "replayed"));
    assert_eq!(expect_str(&replay, "actual_version"), "v0002");
    assert_eq!(
        expect_str(&replay, "graph_id"),
        expect_str(&commit, "graph_id")
    );
}

#[test]
fn t42_rest_commit_requires_matching_prepare() {
    let service = service();
    assert_error(
        &service.handle(&post_json("/api/runtime/commit", "{}")),
        400,
        CODE_MISSING_FIELD,
    );
    let unknown = service.handle(&post_json(
        "/api/runtime/commit",
        "{\"prepare_id\":\"prep-9999\"}",
    ));
    assert_error(&unknown, 409, CODE_NOT_PREPARED);
    assert_error(
        &service.handle(&post_json(
            "/api/runtime/abort",
            "{\"prepare_id\":\"prep-9999\"}",
        )),
        409,
        CODE_NOT_PREPARED,
    );
    assert_error(
        &service.handle(&post_json("/api/runtime/commit", "{\"bogus\":1}")),
        400,
        CODE_UNKNOWN_FIELD,
    );
}

#[test]
fn t42_rest_prepare_is_invalidated_by_config_change() {
    let service = service();
    let prepare_id = prepare_ok(&service);
    let changed = service.handle(&put_json("/api/runtime/config", "{\"mode\":\"dante\"}"));
    assert_status(&changed, 200);
    let stale = service.handle(&post_json(
        "/api/runtime/commit",
        &format!("{{\"prepare_id\":\"{prepare_id}\"}}"),
    ));
    assert_error(&stale, 409, CODE_STALE);
    // 重新 prepare 后可提交。
    let new_id = prepare_ok(&service);
    let commit = commit_body_of(&service, &new_id);
    assert_eq!(expect_str(&commit, "mode"), "dante");
}

#[test]
fn t42_rest_abort_invalidates_plan() {
    let service = service();
    let prepare_id = prepare_ok(&service);
    let body = format!("{{\"prepare_id\":\"{prepare_id}\"}}");
    let abort = service.handle(&post_json("/api/runtime/abort", &body));
    assert_status(&abort, 200);
    let abort_body = json_body(&abort);
    assert!(expect_bool(&abort_body, "aborted"));
    let commit = service.handle(&post_json("/api/runtime/commit", &body));
    assert_error(&commit, 409, CODE_STALE);
    let again = service.handle(&post_json("/api/runtime/abort", &body));
    assert_status(&again, 200);
    assert!(expect_bool(&json_body(&again), "already_invalidated"));
}

#[test]
fn t42_rest_second_prepare_replaces_first() {
    let service = service();
    let first = prepare_ok(&service);
    let second = prepare_ok(&service);
    assert_ne!(first, second);
    let stale = service.handle(&post_json(
        "/api/runtime/commit",
        &format!("{{\"prepare_id\":\"{first}\"}}"),
    ));
    assert_error(&stale, 409, CODE_STALE);
    let commit = commit_body_of(&service, &second);
    assert!(expect_bool(&commit, "committed"));
}

#[test]
fn t42_rest_engine_failures_map_to_errors_without_fake_success() {
    let mut engine = MemoryEngine::new();
    engine.set_fail_prepare(Some(RtStatus::ERR_PLAN_STALE));
    let service = service_engine(engine);
    assert_error(
        &service.handle(&post_json("/api/runtime/prepare", "{}")),
        409,
        CODE_STALE,
    );

    let mut engine = MemoryEngine::new();
    engine.set_fail_commit(Some(RtStatus::ERR_SWITCH_BUSY));
    let service = service_engine(engine);
    let prepare_id = prepare_ok(&service);
    let body = format!("{{\"prepare_id\":\"{prepare_id}\"}}");
    assert_error(
        &service.handle(&post_json("/api/runtime/commit", &body)),
        409,
        CODE_SWITCH_BUSY,
    );
    assert_error(
        &service.handle(&post_json("/api/runtime/commit", &body)),
        409,
        CODE_STALE,
    );

    let mut engine = MemoryEngine::new();
    engine.set_device_owner("calibration");
    let service = service_engine(engine);
    assert_error(
        &service.handle(&post_json("/api/runtime/prepare", "{}")),
        409,
        CODE_DEVICE_BUSY,
    );
}

#[test]
fn t42_rest_invalid_engine_commit_ack_is_500_not_success() {
    let mut engine = MemoryEngine::new();
    engine.set_bad_commit_ack(true);
    let service = service_engine(engine);
    let prepare_id = prepare_ok(&service);
    let response = service.handle(&post_json(
        "/api/runtime/commit",
        &format!("{{\"prepare_id\":\"{prepare_id}\"}}"),
    ));
    assert_error(&response, 500, CODE_INTERNAL_ERROR);
    let status = json_body(&service.handle(&get("/api/runtime/status")));
    expect_null(&status, "pending_prepare");
}

// ---------------------------------------------------------------------------
// R49/K53/R10/R11：volume / mute / mode（actual ack、202、状态机错误）
// ---------------------------------------------------------------------------

#[test]
fn t42_rest_volume_actual_ack_and_bounds() {
    let service = service();
    let initial = json_body(&service.handle(&get("/api/runtime/volume")));
    assert_eq!(expect_num(&initial, "volume_db"), -12.0);
    assert!(!expect_bool(&initial, "mute"));

    let write = service.handle(&put_json("/api/runtime/volume", "{\"volume_db\":-20.5}"));
    assert_status(&write, 200);
    let body = json_body(&write);
    assert_eq!(expect_num(&body, "volume_db"), -20.5);
    assert_eq!(expect_num(&body, "requested_volume_db"), -20.5);
    let read_back = json_body(&service.handle(&get("/api/runtime/volume")));
    assert_eq!(expect_num(&read_back, "volume_db"), -20.5);

    // 旧字段名 db 也接受（R19 真实实现口径）。
    let legacy = service.handle(&put_json("/api/runtime/volume", "{\"db\":-6}"));
    assert_status(&legacy, 200);
    assert_eq!(expect_num(&json_body(&legacy), "volume_db"), -6.0);

    for good in ["-150", "50", "0"] {
        let response = service.handle(&put_json(
            "/api/runtime/volume",
            &format!("{{\"volume_db\":{good}}}"),
        ));
        assert_status(&response, 200);
    }
    for bad in ["60", "-150.1", "1000"] {
        let response = service.handle(&put_json(
            "/api/runtime/volume",
            &format!("{{\"volume_db\":{bad}}}"),
        ));
        assert_error(&response, 400, CODE_INVALID_FIELD);
    }
    assert_error(
        &service.handle(&put_json("/api/runtime/volume", "{}")),
        400,
        CODE_MISSING_FIELD,
    );
    assert_error(
        &service.handle(&put_json(
            "/api/runtime/volume",
            "{\"db\":-1,\"volume_db\":-2}",
        )),
        400,
        CODE_INVALID_FIELD,
    );
    assert_error(
        &service.handle(&put_json("/api/runtime/volume", "{\"bogus\":1}")),
        400,
        CODE_UNKNOWN_FIELD,
    );
}

#[test]
fn t42_rest_mute_is_independent_and_idempotent() {
    let service = service();
    let initial = json_body(&service.handle(&get("/api/runtime/mute")));
    assert!(!expect_bool(&initial, "mute"));
    assert_eq!(expect_num(&initial, "volume_db"), -12.0);

    let write = service.handle(&put_json("/api/runtime/mute", "{\"mute\":true}"));
    assert_status(&write, 200);
    let body = json_body(&write);
    assert!(expect_bool(&body, "mute"));
    assert_eq!(expect_num(&body, "volume_db"), -12.0);
    // 幂等：重复 mute 返回同一 actual。
    let repeat = service.handle(&put_json("/api/runtime/mute", "{\"mute\":true}"));
    assert_status(&repeat, 200);
    assert!(expect_bool(&json_body(&repeat), "mute"));
    // 写 volume 不解除 mute（K53）。
    let volume = service.handle(&put_json("/api/runtime/volume", "{\"db\":-30}"));
    assert_status(&volume, 200);
    assert!(expect_bool(&json_body(&volume), "mute"));
    let unmute = service.handle(&put_json("/api/runtime/mute", "{\"mute\":false}"));
    assert_status(&unmute, 200);
    assert!(!expect_bool(&json_body(&unmute), "mute"));
    assert_eq!(expect_num(&json_body(&unmute), "volume_db"), -30.0);
    assert_error(
        &service.handle(&put_json("/api/runtime/mute", "{}")),
        400,
        CODE_MISSING_FIELD,
    );
    assert_error(
        &service.handle(&put_json("/api/runtime/mute", "{\"mute\":\"yes\"}")),
        400,
        CODE_INVALID_FIELD,
    );
}

#[test]
fn t42_rest_volume_and_mute_are_rejected_while_calibration_owns_device() {
    let mut engine = MemoryEngine::new();
    engine.set_device_owner("calibration");
    let service = service_engine(engine);
    assert_error(
        &service.handle(&put_json("/api/runtime/volume", "{\"db\":-10}")),
        409,
        CODE_DEVICE_BUSY,
    );
    assert_error(
        &service.handle(&put_json("/api/runtime/mute", "{\"mute\":true}")),
        409,
        CODE_DEVICE_BUSY,
    );
    // 读 actual 仍然可用（R48：状态可查）。
    assert_status(&service.handle(&get("/api/runtime/volume")), 200);
}

#[test]
fn t42_rest_legacy_mode_get_has_r10_fields() {
    let service = service();
    let response = service.handle(&get("/api/mode"));
    assert_status(&response, 200);
    let body = json_body(&response);
    assert_eq!(expect_str(&body, "schema"), MODE_STATUS_SCHEMA);
    assert_eq!(expect_str(&body, "mode"), "toslink");
    assert_eq!(expect_str(&body, "mode_label"), "Toslink 2.0");
    assert_eq!(expect_str(&body, "state"), "idle");
    assert_eq!(expect_str(&body, "switch_state"), "idle");
    assert_eq!(expect_num(&body, "progress"), 100.0);
    assert!(expect_bool(&body, "enabled"));
    assert!(!expect_bool(&body, "restore_on_start"));
    assert_eq!(expect_str(&body, "strategy"), "internal");
    assert_eq!(expect_arr_len(&body, "profiles"), ALLOWED_MODES.len());
    assert_eq!(expect_arr_len(&body, "steps"), 0);
    assert!(expect_arr_len(&body, "warnings") == 0);
    expect_null(&body, "last_error");
    expect_null(&body, "step");
    let script_status = expect_obj(&body, "script_status");
    assert!(!expect_bool(script_status, "available"));
    let persisted = expect_obj(&body, "persisted");
    assert_eq!(expect_str(persisted, "mode"), "toslink");
    assert!(!expect_bool(persisted, "auto_restore"));
    let target_layout = expect_obj(&body, "target_layout");
    assert_eq!(expect_str(target_layout, "layout"), "4.1.2");
    assert_eq!(
        expect_str(&body, "target_hash"),
        TargetSpec::p1_412().fingerprint()
    );
    assert_eq!(expect_num(&body, "sample_rate"), 48000.0);
    assert_eq!(expect_num(&body, "chunksize"), 128.0);
    assert_eq!(expect_str(&body, "actual_version"), "v0001");
}

#[test]
fn t42_rest_mode_switch_returns_202_with_actual_ack() {
    let service = service();
    let response = service.handle(&put_json("/api/mode", "{\"mode\":\"dante\"}"));
    assert_status(&response, 202);
    let body = json_body(&response);
    assert_eq!(expect_str(&body, "mode"), "dante");
    assert_eq!(expect_str(&body, "mode_label"), "Dante");
    assert_eq!(expect_str(&body, "switch_state"), "idle");
    assert_eq!(expect_str(&body, "state"), "idle");
    assert_eq!(expect_str(&body, "profile"), "dante-96k");
    let status = json_body(&service.handle(&get("/api/runtime/status")));
    assert_eq!(expect_str(&status, "mode"), "dante");
    assert_eq!(expect_str(&status, "mode_profile"), "dante-96k");

    let runtime = service.handle(&put_json(
        "/api/runtime/mode",
        "{\"mode\":\"target_multichannel\"}",
    ));
    assert_status(&runtime, 202);
    assert_eq!(
        expect_str(&json_body(&runtime), "mode"),
        "target_multichannel"
    );
    let read = service.handle(&get("/api/runtime/mode"));
    assert_status(&read, 200);
    assert_eq!(expect_str(&json_body(&read), "mode"), "target_multichannel");
}

#[test]
fn t42_rest_mode_rejections() {
    let service = service();
    assert_error(
        &service.handle(&put_json("/api/mode", "{\"mode\":\"bogus\"}")),
        400,
        CODE_UNKNOWN_MODE,
    );
    assert_error(
        &service.handle(&put_json("/api/mode", "{}")),
        400,
        CODE_MISSING_FIELD,
    );
    assert_error(
        &service.handle(&put_json("/api/mode", "{\"mode\":\"dante\",\"bogus\":1}")),
        400,
        CODE_UNKNOWN_FIELD,
    );

    let mut disabled = test_config();
    disabled.enable_mode_switch = false;
    let service = service_cfg(disabled);
    assert_error(
        &service.handle(&put_json("/api/mode", "{\"mode\":\"dante\"}")),
        403,
        CODE_FORBIDDEN,
    );

    let mut engine = MemoryEngine::new();
    engine.set_switch_busy(true);
    let service = service_engine(engine);
    assert_error(
        &service.handle(&put_json("/api/mode", "{\"mode\":\"dante\"}")),
        409,
        CODE_SWITCH_BUSY,
    );
}

#[test]
fn t42_rest_script_status_is_available_false_not_500() {
    let service = service();
    let response = service.handle(&get("/api/mode/script-status"));
    assert_status(&response, 200);
    let body = json_body(&response);
    assert!(!expect_bool(&body, "available"));
    assert!(expect_str(&body, "message").contains("internal"));
    expect_null(&body, "script");
}

// ---------------------------------------------------------------------------
// R49：target / coeff 版本（GET actual + PUT desired，prepare/commit 后生效）
// ---------------------------------------------------------------------------

#[test]
fn t42_rest_target_get_reports_actual_and_desired() {
    let service = service();
    let response = service.handle(&get("/api/runtime/target"));
    assert_status(&response, 200);
    let body = json_body(&response);
    assert_eq!(
        expect_str(&body, "target_hash"),
        TargetSpec::p1_412().fingerprint()
    );
    assert_eq!(
        expect_str(&body, "actual_target_hash"),
        TargetSpec::p1_412().fingerprint()
    );
    assert!(!expect_bool(&body, "requires_commit"));
    let target = expect_obj(&body, "target");
    assert_eq!(expect_str(target, "layout"), "4.1.2");
    assert_eq!(expect_num(target, "channels"), 8.0);
    let roles = target
        .get("roles")
        .and_then(|value| match value {
            JsonValue::Arr(items) => Some(items.len()),
            _ => None,
        })
        .expect("target roles array");
    assert_eq!(roles, P1_412_ROLES.len());
    assert_eq!(
        expect_str(&body, "config_hash"),
        RuntimeConfig::default().fingerprint()
    );
}

#[test]
fn t42_rest_target_put_updates_desired_only() {
    let service = service();
    let update = service.handle(&put_json("/api/runtime/target", &changed_target_body()));
    assert_status(&update, 200);
    let body = json_body(&update);
    assert!(expect_bool(&body, "requires_commit"));
    assert_eq!(
        expect_str(&body, "actual_target_hash"),
        TargetSpec::p1_412().fingerprint()
    );
    assert_ne!(
        expect_str(&body, "target_hash"),
        expect_str(&body, "actual_target_hash")
    );
    assert_ne!(
        expect_str(&body, "config_hash"),
        RuntimeConfig::default().fingerprint()
    );
    // actual 未变，desired 变了；prepare/commit 后 actual 才变。
    let status = json_body(&service.handle(&get("/api/runtime/status")));
    assert_eq!(
        expect_str(&status, "target_hash"),
        TargetSpec::p1_412().fingerprint()
    );
    let prepare_id = prepare_ok(&service);
    let commit = commit_body_of(&service, &prepare_id);
    let applied_hash = expect_str(&commit, "target_hash");
    let status = json_body(&service.handle(&get("/api/runtime/status")));
    assert_eq!(expect_str(&status, "target_hash"), applied_hash);
    let target = expect_obj(&status, "target");
    assert_eq!(expect_str(target, "profile"), "p1-412-room-a");
}

#[test]
fn t42_rest_target_put_rejects_unknown_roles_and_duplicates() {
    let service = service();
    let unknown_role = p1_target_body().replace("\"role\":\"FL\"", "\"role\":\"SUB4\"");
    assert_error(
        &service.handle(&put_json("/api/runtime/target", &unknown_role)),
        400,
        CODE_UNKNOWN_ROLE,
    );
    let duplicate_dac = p1_target_body().replace("\"dac\":1", "\"dac\":0");
    assert_error(
        &service.handle(&put_json("/api/runtime/target", &duplicate_dac)),
        400,
        CODE_INVALID_FIELD,
    );
    let duplicate_role = p1_target_body().replace("\"role\":\"FR\"", "\"role\":\"FL\"");
    assert_error(
        &service.handle(&put_json("/api/runtime/target", &duplicate_role)),
        400,
        CODE_INVALID_FIELD,
    );
    let narrow = p1_target_body().replace("\"channels\":8", "\"channels\":1");
    assert_error(
        &service.handle(&put_json("/api/runtime/target", &narrow)),
        400,
        CODE_INVALID_FIELD,
    );
    // 未知字段严格拒收（target_hash 是响应投影字段，不是请求字段）。
    let with_hash = p1_target_body().replace("{\"layout\"", "{\"target_hash\":\"x\",\"layout\"");
    assert_error(
        &service.handle(&put_json("/api/runtime/target", &with_hash)),
        400,
        CODE_UNKNOWN_FIELD,
    );
    // 包装形式 {"target":{...}} 也接受。
    let wrapped = format!("{{\"target\":{}}}", p1_target_body());
    assert_status(
        &service.handle(&put_json("/api/runtime/target", &wrapped)),
        200,
    );
}

#[test]
fn t42_rest_coeff_get_lists_available_versions() {
    let service = service();
    let response = service.handle(&get("/api/runtime/coeff"));
    assert_status(&response, 200);
    let body = json_body(&response);
    assert_eq!(expect_arr_len(&body, "available_coeffs"), 1);
    expect_null(&body, "coeff");
    expect_null(&body, "actual_coeff");
    let last_good = expect_obj(&body, "last_good_coeff");
    assert_eq!(expect_str(last_good, "version"), "cal-baseline");
    assert!(!expect_bool(&body, "requires_commit"));
}

#[test]
fn t42_rest_coeff_put_then_commit_applies_version() {
    let service = service();
    let baseline = baseline_coeff();
    let body = format!(
        "{{\"coeff\":{{\"version\":\"{}\",\"sha256\":\"{}\"}}}}",
        baseline.version, baseline.sha256
    );
    let update = service.handle(&put_json("/api/runtime/coeff", &body));
    assert_status(&update, 200);
    let update_body = json_body(&update);
    assert!(expect_bool(&update_body, "requires_commit"));
    let selected = expect_obj(&update_body, "coeff");
    assert_eq!(expect_str(selected, "version"), "cal-baseline");
    let prepare_id = prepare_ok(&service);
    let commit = commit_body_of(&service, &prepare_id);
    let applied = expect_obj(&commit, "coeff");
    assert_eq!(expect_str(applied, "version"), "cal-baseline");
    let status = json_body(&service.handle(&get("/api/runtime/status")));
    let actual = expect_obj(&status, "coeff");
    assert_eq!(expect_str(actual, "version"), "cal-baseline");
    // 直接字段形式（无 wrapper）也接受；清空回 baseline。
    let clear = service.handle(&put_json("/api/runtime/coeff", "{\"coeff\":null}"));
    assert_status(&clear, 200);
    assert!(expect_bool(&json_body(&clear), "requires_commit"));
    let direct = format!(
        "{{\"version\":\"{}\",\"sha256\":\"{}\"}}",
        baseline.version, baseline.sha256
    );
    assert_status(
        &service.handle(&put_json("/api/runtime/coeff", &direct)),
        200,
    );
}

#[test]
fn t42_rest_coeff_rejections() {
    let service = service();
    let unknown = format!(
        "{{\"coeff\":{{\"version\":\"cal-nope\",\"sha256\":\"{}\"}}}}",
        hex_lower(&sha256(b"cal-nope"))
    );
    assert_error(
        &service.handle(&put_json("/api/runtime/coeff", &unknown)),
        400,
        CODE_UNKNOWN_COEFF,
    );
    assert_error(
        &service.handle(&put_json(
            "/api/runtime/coeff",
            "{\"coeff\":{\"version\":\"x\",\"sha256\":\"short\"}}",
        )),
        400,
        CODE_INVALID_FIELD,
    );
    assert_error(
        &service.handle(&put_json("/api/runtime/coeff", "{\"coeff\":{\"version\":\"bad version\",\"sha256\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}}")),
        400,
        CODE_INVALID_FIELD,
    );
    assert_error(
        &service.handle(&put_json(
            "/api/runtime/coeff",
            "{\"coeff\":{\"version\":\"x\"}}",
        )),
        400,
        CODE_MISSING_FIELD,
    );
    assert_error(
        &service.handle(&put_json("/api/runtime/coeff", "{}")),
        400,
        CODE_MISSING_FIELD,
    );
}

// ---------------------------------------------------------------------------
// R13/R14/R47/X01/X02/X15/X22：audio/format、input-mode、退役端点、严格拒收
// ---------------------------------------------------------------------------

#[test]
fn t42_rest_audio_format_get_limits_and_actual() {
    let service = service();
    let response = service.handle(&get("/api/audio/format"));
    assert_status(&response, 200);
    let body = json_body(&response);
    assert_eq!(expect_str(&body, "schema"), AUDIO_FORMAT_SCHEMA);
    assert_eq!(expect_str(&body, "mode"), "toslink");
    assert_eq!(expect_num(&body, "sample_rate"), 48000.0);
    assert_eq!(expect_num(&body, "bit_depth"), 24.0);
    assert_eq!(expect_num(&body, "channels"), 8.0);
    assert_eq!(expect_num(&body, "chunksize"), 128.0);
    assert!(expect_arr_len(&body, "warnings") >= 1);
    expect_null(&body, "target_level");
    let limits = expect_obj(&body, "limits");
    let rates = limits
        .get("sample_rates")
        .and_then(|value| match value {
            JsonValue::Arr(items) => Some(items.len()),
            _ => None,
        })
        .expect("sample_rates array");
    assert_eq!(rates, SAMPLE_RATES_HZ.len());
    let sizes = limits
        .get("chunksizes")
        .and_then(|value| match value {
            JsonValue::Arr(items) => Some(items.len()),
            _ => None,
        })
        .expect("chunksizes array");
    assert_eq!(sizes, BLOCK_SIZES.len());
    assert_eq!(expect_arr_len(limits, "ui_reference_chunksizes"), 6);
    let xrun = expect_obj(&body, "xrun");
    assert_eq!(expect_num(xrun, "xruns"), 0.0);
    assert_eq!(expect_str(&body, "actual_version"), "v0001");
}

#[test]
fn t42_rest_audio_format_put_applies_through_prepare_commit() {
    let service = service();
    let response = service.handle(&put_json("/api/audio/format", "{\"sample_rate\":96000}"));
    assert_status(&response, 200);
    let body = json_body(&response);
    assert!(expect_bool(&body, "ok"));
    let format = expect_obj(&body, "format");
    assert_eq!(expect_num(format, "sample_rate"), 96000.0);
    let outcome = expect_obj(&body, "outcome");
    assert_eq!(expect_str(outcome, "actual_version"), "v0002");
    assert!(is_hex64(&expect_str(outcome, "graph_id")));
    let read_back = json_body(&service.handle(&get("/api/audio/format")));
    assert_eq!(expect_num(&read_back, "sample_rate"), 96000.0);

    let low_latency = service.handle(&put_json("/api/audio/format", "{\"chunksize\":64}"));
    assert_status(&low_latency, 200);
    let body = json_body(&low_latency);
    assert!(expect_arr_len(&body, "warnings") >= 2);
    assert_eq!(expect_num(expect_obj(&body, "format"), "chunksize"), 64.0);
}

#[test]
fn t42_rest_audio_format_rejects_invalid_without_clamping() {
    let service = service();
    for body in [
        "{\"chunksize\":32}",
        "{\"chunksize\":256}",
        "{\"chunksize\":512}",
        "{\"chunksize\":1024}",
    ] {
        assert_error(
            &service.handle(&put_json("/api/audio/format", body)),
            400,
            CODE_UNSUPPORTED_VALUE,
        );
    }
    assert_error(
        &service.handle(&put_json("/api/audio/format", "{\"sample_rate\":44100}")),
        400,
        CODE_UNSUPPORTED_VALUE,
    );
    assert_error(
        &service.handle(&put_json("/api/audio/format", "{}")),
        400,
        CODE_MISSING_FIELD,
    );
    assert_error(
        &service.handle(&put_json("/api/audio/format", "{\"target_level\":2048}")),
        400,
        CODE_NOT_SUPPORTED,
    );
    assert_error(
        &service.handle(&put_json("/api/audio/format", "{\"bogus\":1}")),
        400,
        CODE_UNKNOWN_FIELD,
    );
    // 失败不半生效：actual 仍为 48k/128。
    let format = json_body(&service.handle(&get("/api/audio/format")));
    assert_eq!(expect_num(&format, "sample_rate"), 48000.0);
    assert_eq!(expect_num(&format, "chunksize"), 128.0);
}

#[test]
fn t42_rest_audio_format_failure_keeps_old_config() {
    let mut engine = MemoryEngine::new();
    engine.set_fail_prepare(Some(RtStatus::ERR_DEVICE_PARAMS));
    let service = service_engine(engine);
    let response = service.handle(&put_json("/api/audio/format", "{\"sample_rate\":96000}"));
    assert_error(&response, 409, CODE_DEVICE_PARAMS);
    let format = json_body(&service.handle(&get("/api/audio/format")));
    assert_eq!(expect_num(&format, "sample_rate"), 48000.0);
    let config = json_body(&service.handle(&get("/api/runtime/config")));
    assert_eq!(
        expect_num(expect_obj(&config, "config"), "sample_rate"),
        48000.0
    );
}

#[test]
fn t42_rest_input_mode_is_retired_with_frozen_codes() {
    let service = service();
    let get_response = service.handle(&get("/api/channelmap/input-mode"));
    assert_status(&get_response, 200);
    let body = json_body(&get_response);
    assert_eq!(expect_str(&body, "status"), "not_supported");
    assert_eq!(expect_str(&body, "reason"), "p1-no-content-upmix");
    let put_response = service.handle(&put_json(
        "/api/channelmap/input-mode",
        "{\"mode\":\"stereo\"}",
    ));
    assert_error(&put_response, 400, CODE_NOT_SUPPORTED);
    let anonymous = service.handle(&RestRequest::get("/api/channelmap/input-mode"));
    assert_error(&anonymous, 401, CODE_UNAUTHORIZED);
}

#[test]
fn t42_rest_retired_endpoint_returns_410_with_replacement() {
    let service = service().with_retired("/api/camilladsp/volume", "/api/runtime/volume");
    let response = service.handle(&post_json("/api/camilladsp/volume", "{\"db\":-10}"));
    assert_error(&response, 410, CODE_RETIRED);
    let body = json_body(&response);
    let error = expect_obj(&body, "error");
    assert_eq!(expect_str(error, "replacement"), "/api/runtime/volume");
}

#[test]
fn t42_rest_payload_too_large_is_413() {
    let mut config = test_config();
    config.max_body_bytes = 64;
    let service = service_cfg(config);
    let big = format!("{{\"mode\":\"toslink\",\"pad\":\"{}\"}}", "a".repeat(200));
    let response = service.handle(&put_json("/api/runtime/config", &big));
    assert_error(&response, 413, CODE_PAYLOAD_TOO_LARGE);
}

#[test]
fn t42_rest_json_strictness_duplicate_nan_depth_and_types() {
    let service = service();
    let duplicate = service.handle(&put_json(
        "/api/runtime/config",
        "{\"mode\":\"toslink\",\"mode\":\"dante\"}",
    ));
    assert_error(&duplicate, 400, CODE_INVALID_JSON);
    let nan = service.handle(&put_json("/api/runtime/volume", "{\"volume_db\":NaN}"));
    assert_error(&nan, 400, CODE_INVALID_JSON);
    let infinity = service.handle(&put_json("/api/runtime/volume", "{\"volume_db\":Infinity}"));
    assert_error(&infinity, 400, CODE_INVALID_JSON);
    let trailing = service.handle(&put_json(
        "/api/runtime/config",
        "{\"mode\":\"toslink\"} trailing",
    ));
    assert_error(&trailing, 400, CODE_INVALID_JSON);
    let non_object = service.handle(&put_json("/api/runtime/config", "[]"));
    assert_error(&non_object, 400, CODE_INVALID_JSON);
    let empty = service.handle(&put_raw("/api/runtime/config"));
    assert_error(&empty, 400, CODE_INVALID_JSON);
    let deeply_nested = format!("{{\"mode\":{}{}}}", "[".repeat(20), "]".repeat(20));
    let deep = service.handle(&put_json("/api/runtime/config", &deeply_nested));
    assert_error(&deep, 400, CODE_INVALID_JSON);
    let wrong_type = service.handle(&put_json("/api/runtime/mute", "{\"mute\":\"yes\"}"));
    assert_error(&wrong_type, 400, CODE_INVALID_FIELD);
    let unknown = service.handle(&put_json(
        "/api/runtime/config",
        "{\"mode\":\"toslink\",\"unknown_field\":1}",
    ));
    assert_error(&unknown, 400, CODE_UNKNOWN_FIELD);
    let body = json_body(&unknown);
    let error = expect_obj(&body, "error");
    assert!(expect_str(error, "message").contains("unknown_field"));
}

// ---------------------------------------------------------------------------
// R48/K14：校准 job REST（MemoryCalibration 回环 + 未接线 503 不伪造）
// ---------------------------------------------------------------------------

fn service_with_memory_calibration() -> RestService {
    RestService::with_config(Box::new(MemoryEngine::new()), test_config())
        .with_calibration(Box::new(MemoryCalibration::new()))
}

#[test]
fn t42_rest_calibration_without_port_is_503_not_fake_job() {
    let service = service();
    assert_error(
        &service.handle(&get("/api/calibration/jobs")),
        503,
        CODE_SERVICE_UNAVAILABLE,
    );
    assert_error(
        &service.handle(&post_json("/api/calibration/jobs", "{}")),
        503,
        CODE_SERVICE_UNAVAILABLE,
    );
    assert_error(
        &service.handle(&get("/api/calibration/jobs/job-0001")),
        503,
        CODE_SERVICE_UNAVAILABLE,
    );
    assert_error(
        &service.handle(&post_json("/api/calibration/jobs/job-0001/apply", "{}")),
        503,
        CODE_SERVICE_UNAVAILABLE,
    );
}

#[test]
fn t42_rest_calibration_start_status_cancel_flow() {
    let service = service_with_memory_calibration();
    let empty = service.handle(&get("/api/calibration/jobs"));
    assert_status(&empty, 200);
    let body = json_body(&empty);
    assert_eq!(expect_str(&body, "schema"), CAL_JOBS_SCHEMA);
    assert_eq!(expect_num(&body, "count"), 0.0);

    let start = service.handle(&post_json("/api/calibration/jobs", "{}"));
    assert_status(&start, 201);
    let body = json_body(&start);
    assert_eq!(expect_str(&body, "schema"), CAL_JOB_SCHEMA);
    let job = expect_obj(&body, "job");
    let job_id = expect_str(job, "job_id");
    assert_eq!(job_id, "job-0001");
    assert_eq!(expect_str(job, "kind"), "full");
    assert_eq!(expect_str(job, "state"), "queued");
    assert_eq!(expect_num(job, "progress"), 0.0);
    expect_null(job, "applied_version");
    assert_eq!(expect_str(job, "last_good_version"), "cal-baseline");

    let get_one = service.handle(&get(&format!("/api/calibration/jobs/{job_id}")));
    assert_status(&get_one, 200);
    assert_eq!(expect_str(&json_body(&get_one), "job_id"), job_id);
    let list = json_body(&service.handle(&get("/api/calibration/jobs")));
    assert_eq!(expect_num(&list, "count"), 1.0);

    // R48：同一时刻只允许一个 active job（冲突 409）。
    let second = service.handle(&post_json("/api/calibration/jobs", "{\"kind\":\"retest\"}"));
    assert_error(&second, 409, CODE_SWITCH_BUSY);

    let cancel = service.handle(&post_json(
        &format!("/api/calibration/jobs/{job_id}/cancel"),
        "{}",
    ));
    assert_status(&cancel, 200);
    let body = json_body(&cancel);
    assert!(expect_bool(&body, "cancelled"));
    assert_eq!(expect_str(expect_obj(&body, "job"), "state"), "cancelled");
    // 终态 job 再取消 -> 409（协作式取消语义）。
    let again = service.handle(&post_json(
        &format!("/api/calibration/jobs/{job_id}/cancel"),
        "{}",
    ));
    assert_error(&again, 409, CODE_SWITCH_BUSY);
    // 终态后可启动新 job。
    let next = service.handle(&post_json("/api/calibration/jobs", "{\"kind\":\"retest\"}"));
    assert_status(&next, 201);
    assert_eq!(
        expect_str(expect_obj(&json_body(&next), "job"), "kind"),
        "retest"
    );
}

#[test]
fn t42_rest_calibration_job_argument_validation_and_404() {
    let service = service_with_memory_calibration();
    assert_error(
        &service.handle(&post_json("/api/calibration/jobs", "{\"kind\":\"bogus\"}")),
        400,
        CODE_INVALID_FIELD,
    );
    assert_error(
        &service.handle(&post_json("/api/calibration/jobs", "{\"bogus\":1}")),
        400,
        CODE_UNKNOWN_FIELD,
    );
    let long_label = format!("{{\"label\":\"{}\"}}", "x".repeat(200));
    assert_error(
        &service.handle(&post_json("/api/calibration/jobs", &long_label)),
        400,
        CODE_INVALID_FIELD,
    );
    assert_error(
        &service.handle(&get("/api/calibration/jobs/job-9999")),
        404,
        CODE_NOT_FOUND,
    );
    // 非法 job id 形式按未知路径 404。
    assert_error(
        &service.handle(&get("/api/calibration/jobs/bad id")),
        404,
        CODE_NOT_FOUND,
    );
}

#[test]
fn t42_rest_calibration_apply_retest_rollback_state_errors() {
    let service = service_with_memory_calibration();
    let start = service.handle(&post_json("/api/calibration/jobs", "{}"));
    assert_status(&start, 201);
    let job_id = expect_str(expect_obj(&json_body(&start), "job"), "job_id");
    // queued 不可 apply/retest。
    assert_error(
        &service.handle(&post_json(
            &format!("/api/calibration/jobs/{job_id}/apply"),
            "{}",
        )),
        409,
        CODE_SWITCH_BUSY,
    );
    assert_error(
        &service.handle(&post_json(
            &format!("/api/calibration/jobs/{job_id}/retest"),
            "{}",
        )),
        409,
        CODE_SWITCH_BUSY,
    );
    // rollback 有 last-good 目标 -> 200 且带 last_good_version（MemoryCalibration 回环）。
    let rollback = service.handle(&post_json(
        &format!("/api/calibration/jobs/{job_id}/rollback"),
        "{}",
    ));
    assert_status(&rollback, 200);
    let body = json_body(&rollback);
    assert!(expect_bool(&body, "rolled_back"));
    assert_eq!(expect_str(&body, "last_good_version"), "cal-baseline");
    // 未知 id 的 apply/cancel/rollback -> 404。
    for action in ["apply", "cancel", "retest", "rollback"] {
        assert_error(
            &service.handle(&post_json(
                &format!("/api/calibration/jobs/job-9999/{action}"),
                "{}",
            )),
            404,
            CODE_NOT_FOUND,
        );
    }
}

// ---------------------------------------------------------------------------
// R48/K14：ScriptedCalibration 坏 ack / 坏状态 -> 500（不假成功）
// ---------------------------------------------------------------------------

struct ScriptedCalibration {
    jobs: BTreeMap<String, CalJobView>,
    apply_missing_ack: bool,
    rollback_missing_last_good: bool,
    corrupt_state: bool,
    corrupt_progress: bool,
    fail_start: Option<RtStatus>,
}

fn prepared_job_view() -> CalJobView {
    let now = now_unix();
    CalJobView {
        job_id: "job-0001".to_string(),
        kind: "full".to_string(),
        state: "prepared".to_string(),
        stage: None,
        progress: 0.8,
        applied_version: None,
        last_good_version: Some("cal-baseline".to_string()),
        failure: None,
        created_unix: now,
        updated_unix: now,
    }
}

fn scripted_calibration(job: CalJobView) -> ScriptedCalibration {
    let mut jobs = BTreeMap::new();
    jobs.insert(job.job_id.clone(), job);
    ScriptedCalibration {
        jobs,
        apply_missing_ack: false,
        rollback_missing_last_good: false,
        corrupt_state: false,
        corrupt_progress: false,
        fail_start: None,
    }
}

impl ScriptedCalibration {
    fn sanitize(&self, mut job: CalJobView) -> CalJobView {
        if self.corrupt_state {
            job.state = "bogus-state".to_string();
        }
        if self.corrupt_progress {
            job.progress = f64::NAN;
        }
        job
    }
}

impl CalibrationPort for ScriptedCalibration {
    fn start(&mut self, _request: &CalStartRequest) -> Result<CalJobView, CalPortError> {
        match self.fail_start {
            Some(status) => Err(CalPortError::Rejected(status)),
            None => Err(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY)),
        }
    }

    fn get(&mut self, job_id: &str) -> Option<CalJobView> {
        self.jobs.get(job_id).cloned().map(|job| self.sanitize(job))
    }

    fn list(&mut self) -> Vec<CalJobView> {
        self.jobs
            .values()
            .cloned()
            .map(|job| self.sanitize(job))
            .collect()
    }

    fn cancel(&mut self, job_id: &str) -> Result<CalJobView, CalPortError> {
        self.jobs
            .get(job_id)
            .cloned()
            .map(|job| self.sanitize(job))
            .ok_or(CalPortError::NotFound)
    }

    fn apply(&mut self, job_id: &str) -> Result<CalJobView, CalPortError> {
        let mut job = self
            .jobs
            .get(job_id)
            .cloned()
            .ok_or(CalPortError::NotFound)?;
        job.state = "complete".to_string();
        job.stage = None;
        job.progress = 1.0;
        job.applied_version = if self.apply_missing_ack {
            None
        } else {
            Some("cal-v2".to_string())
        };
        Ok(self.sanitize(job))
    }

    fn retest(&mut self, job_id: &str) -> Result<CalJobView, CalPortError> {
        if !self.jobs.contains_key(job_id) {
            return Err(CalPortError::NotFound);
        }
        let now = now_unix();
        Ok(CalJobView {
            job_id: "job-0002".to_string(),
            kind: "retest".to_string(),
            state: "queued".to_string(),
            stage: None,
            progress: 0.0,
            applied_version: None,
            last_good_version: Some("cal-baseline".to_string()),
            failure: None,
            created_unix: now,
            updated_unix: now,
        })
    }

    fn rollback(&mut self, job_id: &str) -> Result<CalJobView, CalPortError> {
        let mut job = self
            .jobs
            .get(job_id)
            .cloned()
            .ok_or(CalPortError::NotFound)?;
        job.state = "rolled_back".to_string();
        job.stage = None;
        job.progress = 1.0;
        if self.rollback_missing_last_good {
            job.last_good_version = None;
        }
        Ok(self.sanitize(job))
    }
}

fn service_with_scripted(port: ScriptedCalibration) -> RestService {
    RestService::with_config(Box::new(MemoryEngine::new()), test_config())
        .with_calibration(Box::new(port))
}

#[test]
fn t42_rest_calibration_apply_and_rollback_actual_acks() {
    let service = service_with_scripted(scripted_calibration(prepared_job_view()));
    let get_one = service.handle(&get("/api/calibration/jobs/job-0001"));
    assert_status(&get_one, 200);
    assert_eq!(
        expect_str(expect_obj(&json_body(&get_one), "job"), "state"),
        "prepared"
    );
    let apply = service.handle(&post_json("/api/calibration/jobs/job-0001/apply", "{}"));
    assert_status(&apply, 200);
    let body = json_body(&apply);
    assert!(expect_bool(&body, "ok"));
    assert_eq!(expect_str(&body, "applied_version"), "cal-v2");
    assert_eq!(expect_str(expect_obj(&body, "job"), "state"), "complete");
    let retest = service.handle(&post_json("/api/calibration/jobs/job-0001/retest", "{}"));
    assert_status(&retest, 201);
    let body = json_body(&retest);
    assert_eq!(expect_str(&body, "job_id"), "job-0002");
    assert_eq!(expect_str(expect_obj(&body, "job"), "kind"), "retest");
    let rollback = service.handle(&post_json("/api/calibration/jobs/job-0001/rollback", "{}"));
    assert_status(&rollback, 200);
    let body = json_body(&rollback);
    assert!(expect_bool(&body, "rolled_back"));
    assert_eq!(expect_str(&body, "last_good_version"), "cal-baseline");
    assert_eq!(expect_str(expect_obj(&body, "job"), "state"), "rolled_back");
}

#[test]
fn t42_rest_calibration_bad_apply_ack_is_500() {
    let mut port = scripted_calibration(prepared_job_view());
    port.apply_missing_ack = true;
    let service = service_with_scripted(port);
    let response = service.handle(&post_json("/api/calibration/jobs/job-0001/apply", "{}"));
    assert_error(&response, 500, CODE_INTERNAL_ERROR);
}

#[test]
fn t42_rest_calibration_bad_rollback_ack_is_500() {
    let mut port = scripted_calibration(prepared_job_view());
    port.rollback_missing_last_good = true;
    let service = service_with_scripted(port);
    let response = service.handle(&post_json("/api/calibration/jobs/job-0001/rollback", "{}"));
    assert_error(&response, 500, CODE_INTERNAL_ERROR);
}

#[test]
fn t42_rest_calibration_corrupt_state_or_progress_is_500() {
    let mut port = scripted_calibration(prepared_job_view());
    port.corrupt_state = true;
    let service = service_with_scripted(port);
    assert_error(
        &service.handle(&get("/api/calibration/jobs/job-0001")),
        500,
        CODE_INTERNAL_ERROR,
    );
    assert_error(
        &service.handle(&get("/api/calibration/jobs")),
        500,
        CODE_INTERNAL_ERROR,
    );

    let mut port = scripted_calibration(prepared_job_view());
    port.corrupt_progress = true;
    let service = service_with_scripted(port);
    assert_error(
        &service.handle(&get("/api/calibration/jobs/job-0001")),
        500,
        CODE_INTERNAL_ERROR,
    );
}

#[test]
fn t42_rest_calibration_start_rejections_map_to_http() {
    let mut port = scripted_calibration(prepared_job_view());
    port.fail_start = Some(RtStatus::ERR_DEVICE_BUSY);
    let service = service_with_scripted(port);
    assert_error(
        &service.handle(&post_json("/api/calibration/jobs", "{}")),
        409,
        CODE_DEVICE_BUSY,
    );

    let port = scripted_calibration(prepared_job_view());
    let service = service_with_scripted(port);
    assert_error(
        &service.handle(&post_json("/api/calibration/jobs", "{}")),
        409,
        CODE_SWITCH_BUSY,
    );
    assert_error(
        &service.handle(&get("/api/calibration/jobs/job-9999")),
        404,
        CODE_NOT_FOUND,
    );
}

// ---------------------------------------------------------------------------
// 证据样例：--nocapture 时打印关键响应体（供 evidence/T42 抓取；断言已在上面测试）
// ---------------------------------------------------------------------------

#[test]
fn t42_rest_evidence_sample_responses() {
    let service = service();
    println!(
        "SAMPLE STATUS {}",
        service.handle(&get("/api/runtime/status")).body
    );
    println!(
        "SAMPLE AUTH_STATUS {}",
        service.handle(&RestRequest::get("/api/auth/status")).body
    );
    println!(
        "SAMPLE CONFIG {}",
        service.handle(&get("/api/runtime/config")).body
    );
    let update = service.handle(&put_json("/api/runtime/config", &prepared_config_body()));
    assert_status(&update, 200);
    println!("SAMPLE CONFIG_PUT {}", update.body);
    let prepare = service.handle(&post_json("/api/runtime/prepare", "{}"));
    assert_status(&prepare, 200);
    println!("SAMPLE PREPARE {}", prepare.body);
    let prepare_id = expect_str(&json_body(&prepare), "prepare_id");
    let commit = service.handle(&post_json(
        "/api/runtime/commit",
        &format!("{{\"prepare_id\":\"{prepare_id}\"}}"),
    ));
    assert_status(&commit, 200);
    println!("SAMPLE COMMIT {}", commit.body);
    println!(
        "SAMPLE VOLUME {}",
        service
            .handle(&put_json("/api/runtime/volume", "{\"db\":-18.5}"))
            .body
    );
    println!(
        "SAMPLE MODE_PUT {}",
        service
            .handle(&put_json("/api/mode", "{\"mode\":\"dante\"}"))
            .body
    );
    println!(
        "SAMPLE AUDIO_FORMAT {}",
        service.handle(&get("/api/audio/format")).body
    );
    println!(
        "SAMPLE INPUT_MODE {}",
        service.handle(&get("/api/channelmap/input-mode")).body
    );
    let cal = RestService::with_config(Box::new(MemoryEngine::new()), test_config())
        .with_calibration(Box::new(MemoryCalibration::new()));
    let job = cal.handle(&post_json(
        "/api/calibration/jobs",
        "{\"kind\":\"full\",\"label\":\"evidence\"}",
    ));
    assert_status(&job, 201);
    println!("SAMPLE CAL_START {}", job.body);
    println!(
        "SAMPLE ERROR_404 {}",
        service.handle(&get("/api/nope")).body
    );
}
// ---------------------------------------------------------------------------
// F-13/F-14/F-28/F-30：synthetic 显式标记、4.1.2 槽位逐项校验、ModeController 接线
// ---------------------------------------------------------------------------

fn service_with_mode_engine(services: crate::modes::SimulatedServices) -> RestService {
    let engine = crate::modes::ModeEngine::new(
        Box::new(MemoryEngine::new()),
        Box::new(services),
    )
    .expect("mode engine");
    RestService::with_config(Box::new(engine), test_config())
}

#[test]
fn t42_rest_commit_ack_is_explicitly_synthetic_for_memory_engine() {
    // F-13：MemoryEngine 的 blocks_committed=1/ack_mask=0xF 是内存合成的；
    // 回执必须显式 synthetic=true/actual=false，页面据此不得显示「已生效（actual）」。
    let service = service();
    let prepare_id = prepare_ok(&service);
    let commit = commit_body_of(&service, &prepare_id);
    assert_eq!(expect_str(&commit, "engine"), "memory");
    assert!(expect_bool(&commit, "synthetic"));
    assert!(!expect_bool(&commit, "actual"));
    // 合成 ack 字段仍在（验证消费者侧 ack 判据），但已被显式标注为非 actual。
    assert_eq!(expect_num(&commit, "blocks_committed"), 1.0);
    assert_eq!(expect_num(&commit, "ack_mask"), 15.0);
    let status = json_body(&service.handle(&get("/api/runtime/status")));
    assert_eq!(expect_str(&status, "engine"), "memory");
    assert!(expect_bool(&status, "synthetic"));
    assert!(!expect_bool(&status, "actual"));
    assert!(!expect_bool(&status, "mode_controller"));
    assert_eq!(expect_str(&status, "mode_services"), "not_wired");
    // prepare 回执同样显式。
    let prepared = json_body(&service.handle(&post_json("/api/runtime/prepare", "{}")));
    assert!(expect_bool(&prepared, "synthetic"));
    assert!(!expect_bool(&prepared, "actual"));
}

#[test]
fn t42_rest_mode_ack_is_synthetic_without_mode_controller() {
    let service = service();
    let response = service.handle(&put_json("/api/mode", "{\"mode\":\"dante\"}"));
    assert_status(&response, 202);
    let body = json_body(&response);
    assert_eq!(expect_str(&body, "mode"), "dante");
    assert_eq!(expect_str(&body, "switch_state"), "idle");
    assert!(!expect_bool(&body, "mode_controller"));
    assert!(!expect_bool(&body, "mode_actual"));
    assert!(expect_bool(&body, "synthetic"));
    assert!(!expect_bool(&body, "actual"));
    expect_null(&body, "mode_evidence");
}

#[test]
fn t42_rest_mode_switch_runs_t44_controller_state_machine() {
    // F-28：daemon 便携路径接 ModeEngine；成功切换 = 新 epoch + 唯一 servo + 回滚证据字段。
    let service = service_with_mode_engine(crate::modes::SimulatedServices::new());
    let before = json_body(&service.handle(&get("/api/mode")));
    assert!(expect_bool(&before, "mode_controller"));
    assert_eq!(expect_str(&before, "mode_services"), "simulated");
    assert!(!expect_bool(&before, "mode_actual"));
    expect_null(&before, "mode_evidence");
    let response = service.handle(&put_json("/api/mode", "{\"mode\":\"dante\"}"));
    assert_status(&response, 202);
    let body = json_body(&response);
    assert_eq!(expect_str(&body, "mode"), "dante");
    assert_eq!(expect_str(&body, "switch_state"), "idle");
    assert!(expect_bool(&body, "mode_controller"));
    // 服务面仍是模拟实现：不得显示成板端 actual。
    assert!(!expect_bool(&body, "mode_actual"));
    let evidence = expect_obj(&body, "mode_evidence");
    assert!(expect_num(evidence, "epoch") > 0.0);
    assert!(expect_num(evidence, "switch_seq") > 0.0);
    assert_eq!(expect_str(evidence, "servo_owner"), "ptp");
    assert!(expect_num(evidence, "servo_claims") >= 1.0);
    assert!(!expect_bool(evidence, "rolled_back"));
    assert_eq!(expect_num(evidence, "last_exit_code"), 0.0);
    assert_eq!(expect_str(evidence, "fallback"), "none");
    // snapshot 反映状态机 actual（mode/profile/epoch），不再由 MemoryEngine 直接改。
    let status = json_body(&service.handle(&get("/api/runtime/status")));
    assert_eq!(expect_str(&status, "mode"), "dante");
    assert_eq!(expect_str(&status, "mode_profile"), "dante-96k");
    assert_eq!(expect_num(&status, "sample_rate"), 96_000.0);
    assert!(expect_bool(&status, "mode_controller"));
    // 状态机新 epoch（首次切换从 0 -> 1），且与 ack evidence 一致。
    assert_eq!(expect_num(&status, "epoch"), expect_num(evidence, "epoch"));
    assert!(expect_num(&status, "epoch") > 0.0);
    assert_eq!(expect_str(&status, "device_owner"), "playback");
}

#[test]
fn t42_rest_mode_switch_then_format_commit_does_not_revert_snapshot() {
    // N-01（F-28 回归）：切换后 snapshot 不得再用 controller 里切换时的旧
    // sample_rate/chunksize/epoch 覆盖 inner 的真实提交；format/target 必须反映随后的 commit。
    let service = service_with_mode_engine(crate::modes::SimulatedServices::new());
    let switched = service.handle(&put_json("/api/mode", "{\"mode\":\"dante\"}"));
    assert_status(&switched, 202);
    let switch_body = json_body(&switched);
    let switch_epoch = expect_num(expect_obj(&switch_body, "mode_evidence"), "epoch");
    let fmt_before = json_body(&service.handle(&get("/api/audio/format")));
    assert_eq!(expect_str(&fmt_before, "mode"), "dante");
    assert_eq!(expect_num(&fmt_before, "sample_rate"), 96_000.0);
    assert_eq!(expect_num(&fmt_before, "chunksize"), 128.0);
    assert_eq!(expect_num(&fmt_before, "epoch"), switch_epoch);
    // F-13：format GET 也是身份面：内存 engine 必须显式 synthetic，不得写 RT host actual。
    assert_eq!(expect_str(&fmt_before, "engine"), "memory");
    assert!(expect_bool(&fmt_before, "synthetic"));
    assert!(!expect_bool(&fmt_before, "actual"));
    assert_ne!(
        expect_str(&fmt_before, "source"),
        "internal RT host actual (T26)"
    );

    // 组合路径：切换后只改 chunksize，经真实 prepare/commit 生效。
    let applied = service.handle(&put_json("/api/audio/format", "{\"chunksize\":64}"));
    assert_status(&applied, 200);
    let applied_body = json_body(&applied);
    let outcome = expect_obj(&applied_body, "outcome");
    assert!(expect_bool(outcome, "synthetic"));
    assert!(!expect_bool(outcome, "actual"));
    let commit_epoch = expect_num(outcome, "epoch");
    assert!(
        commit_epoch > switch_epoch,
        "commit epoch {commit_epoch} must be newer than switch epoch {switch_epoch}"
    );

    let fmt_after = json_body(&service.handle(&get("/api/audio/format")));
    assert_eq!(expect_str(&fmt_after, "mode"), "dante");
    assert_eq!(expect_num(&fmt_after, "sample_rate"), 96_000.0);
    assert_eq!(expect_num(&fmt_after, "chunksize"), 64.0);
    assert_eq!(
        expect_num(&fmt_after, "epoch"),
        commit_epoch,
        "format GET must report the committed epoch, not the switch-time controller epoch"
    );
    let status_after = json_body(&service.handle(&get("/api/runtime/status")));
    assert_eq!(expect_str(&status_after, "mode"), "dante");
    assert_eq!(expect_num(&status_after, "sample_rate"), 96_000.0);
    assert_eq!(expect_num(&status_after, "chunksize"), 64.0);
    assert_eq!(expect_num(&status_after, "epoch"), commit_epoch);
    // 设备 owner 仍来自状态机（playback），不被 inner commit 抹掉。
    assert_eq!(expect_str(&status_after, "device_owner"), "playback");
    // 切换后的 desired 同步到 actual：runtime config 不再保留旧 48k/128。
    let config = json_body(&service.handle(&get("/api/runtime/config")));
    let desired = expect_obj(&config, "config");
    assert_eq!(expect_str(desired, "mode"), "dante");
    assert_eq!(expect_num(desired, "sample_rate"), 96_000.0);
    assert_eq!(expect_num(desired, "chunksize"), 64.0);
}

#[test]
fn t42_rest_volume_mute_format_reads_are_explicitly_synthetic() {
    // F-13：未接真实 host engine 时，volume/mute/format 回执同样必须显式 synthetic。
    let service = service();
    let volume = service.handle(&put_json("/api/runtime/volume", "{\"db\":-18.5}"));
    assert_status(&volume, 200);
    let volume_body = json_body(&volume);
    assert_eq!(expect_str(&volume_body, "engine"), "memory");
    assert!(expect_bool(&volume_body, "synthetic"));
    assert!(!expect_bool(&volume_body, "actual"));
    let volume_read = json_body(&service.handle(&get("/api/runtime/volume")));
    assert!(expect_bool(&volume_read, "synthetic"));
    assert!(!expect_bool(&volume_read, "actual"));
    let mute = service.handle(&put_json("/api/runtime/mute", "{\"mute\":true}"));
    assert_status(&mute, 200);
    let mute_body = json_body(&mute);
    assert!(expect_bool(&mute_body, "synthetic"));
    assert!(!expect_bool(&mute_body, "actual"));
    let mute_read = json_body(&service.handle(&get("/api/runtime/mute")));
    assert!(expect_bool(&mute_read, "synthetic"));
    assert!(!expect_bool(&mute_read, "actual"));
    let format = json_body(&service.handle(&get("/api/audio/format")));
    assert_eq!(expect_str(&format, "engine"), "memory");
    assert!(expect_bool(&format, "synthetic"));
    assert!(!expect_bool(&format, "actual"));
    assert_ne!(
        expect_str(&format, "source"),
        "internal RT host actual (T26)"
    );
    assert!(expect_str(&format, "source").contains("synthetic"));
}

#[test]
fn t42_rest_format_put_warning_follows_synthetic_actual_branch() {
    // N-11（F-13 残留）：format PUT 的 warnings 不得固定写「prepare/commit actual ack」，
    // 必须与 outcome.engine/synthetic/actual 同源分支。
    let synthetic = format_put_warnings(&EngineInfo::memory(), Some(64));
    assert_eq!(
        synthetic.len(),
        2,
        "chunksize=64 must append measurement-window warning"
    );
    let synthetic_text = match &synthetic[0] {
        JsonValue::Str(text) => text.as_str(),
        other => panic!("warning[0] must be string: {other:?}"),
    };
    assert!(
        synthetic_text.contains("synthetic"),
        "synthetic ack warning must say synthetic: {synthetic_text}"
    );
    assert!(
        !synthetic_text.contains("prepare/commit actual ack"),
        "synthetic ack warning must not claim actual ack: {synthetic_text}"
    );

    let actual_info = EngineInfo {
        engine: "host",
        synthetic: false,
        mode_controller: true,
        mode_services: "systemd",
    };
    let actual = format_put_warnings(&actual_info, None);
    assert_eq!(actual.len(), 1);
    let actual_text = match &actual[0] {
        JsonValue::Str(text) => text.as_str(),
        other => panic!("warning[0] must be string: {other:?}"),
    };
    assert!(
        actual_text.contains("prepare/commit actual ack"),
        "real host engine keeps actual ack wording: {actual_text}"
    );
    assert!(
        !actual_text.contains("synthetic"),
        "real host engine must not say synthetic: {actual_text}"
    );

    // 端到端：内存 engine 的 PUT 200 回执 warnings 必须走 synthetic 分支，且与 outcome 一致。
    let service = service();
    let response = service.handle(&put_json("/api/audio/format", "{\"chunksize\":64}"));
    assert_status(&response, 200);
    let body = json_body(&response);
    assert!(expect_bool(expect_obj(&body, "outcome"), "synthetic"));
    assert_eq!(expect_arr_len(&body, "warnings"), 2);
    let warnings = match body.get("warnings") {
        Some(JsonValue::Arr(items)) => items,
        other => panic!("warnings: {other:?}"),
    };
    let first = match &warnings[0] {
        JsonValue::Str(text) => text.as_str(),
        other => panic!("warning[0] must be string: {other:?}"),
    };
    assert!(
        first.contains("synthetic"),
        "PUT format warnings[0] must say synthetic: {first}"
    );
    assert!(!first.contains("prepare/commit actual ack"));
}

#[test]
fn t42_rest_mode_switch_failure_rolls_back_to_off_with_evidence() {
    // F-28：失败路径必须有回滚语义（off + last_exit_code 9 + rolled_back=true），
    // 不得把 failed switch 当 switched 返回。
    let mut services = crate::modes::SimulatedServices::new();
    services.fail_next_start(crate::modes::ServiceUnit::StatimeInferno);
    let service = service_with_mode_engine(services);
    let response = service.handle(&put_json("/api/mode", "{\"mode\":\"dante\"}"));
    assert_status(&response, 400);
    assert_eq!(error_code(&response), CODE_INVALID_PLAN);
    let body = json_body(&service.handle(&get("/api/mode")));
    assert_eq!(expect_str(&body, "mode"), "off");
    let evidence = expect_obj(&body, "mode_evidence");
    assert!(expect_bool(evidence, "rolled_back"));
    assert_eq!(expect_num(evidence, "last_exit_code"), 9.0);
    assert_eq!(expect_str(evidence, "fallback"), "off");
    // 回滚后无 servo（失败清理已释放）。
    expect_null(evidence, "servo_owner");
}

#[test]
fn t42_rest_mode_rolled_back_has_dedicated_rest_code() {
    // F-30：124（模式回滚）不得映射成 stale(119 语义)。
    let rolled_back = rt_status_to_error(RtStatus::from_code(
        khaen_compat::ws::RT_STATUS_MODE_ROLLED_BACK,
    ));
    assert_eq!(rolled_back.status, 409);
    assert_eq!(rolled_back.code, CODE_ROLLED_BACK);
    let stale = rt_status_to_error(RtStatus::ERR_PLAN_STALE);
    assert_eq!(stale.code, CODE_STALE);
    assert_ne!(rolled_back.code, stale.code);
}

#[test]
fn t42_rest_target_412_rejects_non_p1_slot_table() {
    // F-14：4.1.2 的 SUB1 只能是槽 7（ch6 空）；C/SUB2/SUB3 不属于 4.1.2。
    let service = service();
    let cases = [
        p1_target_body().replace("\"role\":\"SUB1\",\"dac\":7", "\"role\":\"SUB1\",\"dac\":6"),
        p1_target_body()
            .replace("\"channels\":8", "\"channels\":10")
            .replace("\"role\":\"SUB1\",\"dac\":7", "\"role\":\"SUB1\",\"dac\":9"),
        p1_target_body().replace(
            "{\"role\":\"SUB1\",\"dac\":7}",
            "{\"role\":\"SUB1\",\"dac\":7},{\"role\":\"SUB2\",\"dac\":6}",
        ),
        p1_target_body().replace(",{\"role\":\"SUB1\",\"dac\":7}", ""),
        p1_target_body()
            .replace("\"role\":\"SL\",\"dac\":2", "\"role\":\"SL\",\"dac\":3")
            .replace("\"role\":\"SR\",\"dac\":3", "\"role\":\"SR\",\"dac\":2"),
        // 低带 dense 表必须 [0,1,2,3,7]（P1_412_LOW_BAND_DACS）
        p1_target_body().replace(
            "\"low_band_dacs\":[0,1,2,3,7]",
            "\"low_band_dacs\":[0,1,2,3]",
        ),
    ];
    for body in cases {
        let response = service.handle(&put_json("/api/runtime/target", &body));
        assert_status(&response, 400);
        assert_eq!(
            error_code(&response),
            CODE_LAYOUT_ROLES_MISMATCH,
            "body={body} response={}",
            response.body
        );
    }
    // P1 参考表本身仍接受（正例，防止把合法表误杀）。
    let ok = service.handle(&put_json("/api/runtime/target", &p1_target_body()));
    assert_status(&ok, 200);
}


// ---------------------------------------------------------------------------
// 校准 UI 契约（task: cal-ui）：settings/status/apply/clear + level/delay/bass lanes
// ---------------------------------------------------------------------------

use super::calibration_contract::{
    CalAppliedState, CalBassResults, CalLaneResult, CalLaneResults, CAL_APPLY_SCHEMA, CAL_LANES,
    CAL_LANES_SCHEMA, CAL_SETTINGS_SCHEMA, CAL_STATUS_SCHEMA,
};
use super::CODE_PENDING_HOST_FIELD;

/// 校准 UI 测试替身：per-lane 结果 + apply/clear 回执（不碰 host/声卡）。
struct LanesCalibration {
    jobs: BTreeMap<String, CalJobView>,
    order: Vec<String>,
    next_id: u64,
    applied: CalAppliedState,
}

impl LanesCalibration {
    fn new() -> Self {
        LanesCalibration {
            jobs: BTreeMap::new(),
            order: Vec::new(),
            next_id: 0,
            applied: CalAppliedState::default(),
        }
    }

    fn store(&mut self, request: &CalStartRequest) -> CalJobView {
        self.next_id += 1;
        let job = CalJobView {
            job_id: format!("job-{:04}", self.next_id),
            kind: request.kind.clone(),
            state: "complete".to_string(),
            stage: request.stage.clone(),
            progress: 1.0,
            applied_version: None,
            last_good_version: Some("cal-baseline".to_string()),
            failure: None,
            created_unix: 1,
            updated_unix: 2,
        };
        self.jobs.insert(job.job_id.clone(), job.clone());
        self.order.push(job.job_id.clone());
        job
    }
}

impl CalibrationPort for LanesCalibration {
    fn start(&mut self, request: &CalStartRequest) -> Result<CalJobView, CalPortError> {
        Ok(self.store(request))
    }
    fn get(&mut self, job_id: &str) -> Option<CalJobView> {
        self.jobs.get(job_id).cloned()
    }
    fn list(&mut self) -> Vec<CalJobView> {
        self.order
            .iter()
            .filter_map(|job_id| self.jobs.get(job_id).cloned())
            .collect()
    }
    fn cancel(&mut self, job_id: &str) -> Result<CalJobView, CalPortError> {
        let job = self.jobs.get_mut(job_id).ok_or(CalPortError::NotFound)?;
        job.state = "cancelled".to_string();
        Ok(job.clone())
    }
    fn apply(&mut self, _job_id: &str) -> Result<CalJobView, CalPortError> {
        Err(CalPortError::PendingHostField("job apply pending host".to_string()))
    }
    fn retest(&mut self, _job_id: &str) -> Result<CalJobView, CalPortError> {
        Err(CalPortError::PendingHostField("retest pending host".to_string()))
    }
    fn rollback(&mut self, _job_id: &str) -> Result<CalJobView, CalPortError> {
        Err(CalPortError::PendingHostField("rollback pending host".to_string()))
    }
    fn lane_results(&mut self, job_id: &str) -> Option<CalLaneResults> {
        let stage = self.jobs.get(job_id)?.stage.clone()?;
        let lanes = match stage.as_str() {
            "level" => CAL_LANES
                .iter()
                .enumerate()
                .map(|(index, lane)| {
                    CalLaneResult::level_locked(
                        lane,
                        -12.0 - index as f64 * 0.1,
                        28.0 - index as f64,
                        -3.0 + index as f64 * 0.5,
                    )
                })
                .collect(),
            "delay" => CAL_LANES
                .iter()
                .enumerate()
                .map(|(index, lane)| {
                    let relative = index as i64 * 12;
                    CalLaneResult::delay_locked(lane, 1000 + relative, relative)
                })
                .collect(),
            "bass" => Vec::new(),
            _ => return None,
        };
        let bass = if stage == "bass" {
            let mut settings = super::calibration_contract::BassSettings::default();
            settings.enabled = true;
            settings.recompute_pre_gain();
            Some(CalBassResults::passed(&settings))
        } else {
            None
        };
        Some(CalLaneResults {
            stage: stage.clone(),
            reference_lane: (stage == "bass").then(|| "FL".to_string()),
            target_dbfs: (stage == "level").then_some(-12.0),
            lanes,
            bass,
        })
    }
    fn apply_trims(
        &mut self,
        request: &super::calibration_contract::CalApplyRequest,
    ) -> Result<CalAppliedState, CalPortError> {
        let mut applied = CalAppliedState::default();
        applied.level_trims = request.level_trims.clone();
        applied.delays = request.delays.clone();
        applied.bass = request.bass.clone();
        applied.applied_unix = 10;
        applied.source_job = request.job_id.clone();
        self.applied = applied.clone();
        Ok(applied)
    }
    fn clear_trims(&mut self, _targets: &[String]) -> Result<CalAppliedState, CalPortError> {
        let cleared = CalAppliedState {
            cleared_unix: 11,
            ..CalAppliedState::default()
        };
        self.applied = cleared.clone();
        Ok(cleared)
    }
}

fn service_with_lanes_calibration() -> RestService {
    RestService::with_config(Box::new(MemoryEngine::new()), test_config())
        .with_calibration(Box::new(LanesCalibration::new()))
}

fn expect_array<'a>(value: &'a JsonValue, key: &str) -> &'a Vec<JsonValue> {
    match value.get(key) {
        Some(JsonValue::Arr(items)) => items,
        other => panic!("expected array field {key}, got {other:?}"),
    }
}

#[test]
fn t42_rest_calibration_settings_roundtrip_and_validation() {
    let service = service_with_lanes_calibration();
    let default = service.handle(&get("/api/calibration/settings"));
    assert_status(&default, 200);
    let body = json_body(&default);
    assert_eq!(expect_str(&body, "schema"), CAL_SETTINGS_SCHEMA);
    assert_eq!(
        expect_str(&body, "calibration_state"),
        "RELATIVE_UNCALIBRATED"
    );
    let settings = expect_obj(&body, "settings");
    assert_eq!(expect_str(expect_obj(settings, "mic"), "kind"), "usb");
    assert!(!expect_bool(expect_obj(&body, "validation"), "ok"));
    let pending = expect_array(&body, "pending_host_fields");
    assert!(pending.iter().any(|item| item == &JsonValue::str("level.lanes")));
    let bass = expect_obj(settings, "bass");
    assert_eq!(expect_num(bass, "subs_count"), 3.0);
    expect_array(bass, "subs");
    expect_obj(bass, "routing");

    let valid = r#"{"mic":{"kind":"analog_48v","device":"hw:UltraLitemk5,0","channel":0,"phantom_48v":true,"gain_db":24},"cal_file":{"path":"/var/lib/khaen-rt-stack/cal/umik.txt","name":"umik.txt","sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","source":"path"},"orientation_deg":90,"target_mode":"tilt10","bass":{"enabled":true,"crossover_hz":80,"main_slope":"LR4","sub_slope":"LR4","linear_phase":true,"sub_gain_db":0,"strategy":"multi_b","subs":[{"id":"SUB1","output":"7","hp_hz":20,"lp_hz":120,"gain_db":0,"delay_samples":0,"polarity":1,"modal_notch":[{"freq_hz":63,"gain_db":-6,"q":4}]},{"id":"SUB_L","output":"8","hp_hz":20,"lp_hz":120,"gain_db":-2,"delay_samples":12,"polarity":-1,"modal_notch":[]},{"id":"SUB_R","output":"9","hp_hz":20,"lp_hz":120,"gain_db":-2,"delay_samples":24,"polarity":1,"modal_notch":[]}],"routing":{"mode":"all_to_all","lfe_weights":[0.34,0.33,0.33],"left_weights":[0.5,0.25,0.25],"right_weights":[0.25,0.25,0.5],"stereo_bias":0.0}}}"#;
    let saved = service.handle(&post_json("/api/calibration/settings", valid));
    assert_status(&saved, 200);
    let body = json_body(&saved);
    assert_eq!(expect_str(&body, "calibration_state"), "CALIBRATED_ABSOLUTE");
    assert!(expect_bool(expect_obj(&body, "validation"), "ok"));
    let settings = expect_obj(&body, "settings");
    assert_eq!(expect_str(expect_obj(settings, "mic"), "kind"), "analog_48v");
    assert_eq!(expect_num(expect_obj(settings, "bass"), "subs_count"), 3.0);
    expect_num(expect_obj(settings, "bass"), "pre_gain_db");

    let bad_sha = r#"{"mic":{"kind":"usb","device":"hw:U18dB,0"},"cal_file":{"path":"a.txt","sha256":"xyz"}}"#;
    assert_error(
        &service.handle(&post_json("/api/calibration/settings", bad_sha)),
        400,
        CODE_INVALID_FIELD,
    );
    let phantom_usb = r#"{"mic":{"kind":"usb","device":"hw:U18dB,0","phantom_48v":true}}"#;
    assert_error(
        &service.handle(&post_json("/api/calibration/settings", phantom_usb)),
        400,
        CODE_INVALID_FIELD,
    );
    let bad_gain = r#"{"mic":{"kind":"usb","device":"hw:U18dB,0","gain_db":99}}"#;
    assert_error(
        &service.handle(&post_json("/api/calibration/settings", bad_gain)),
        400,
        CODE_INVALID_FIELD,
    );
    let bad_crossover = r#"{"mic":{"kind":"usb","device":"hw:U18dB,0"},"bass":{"enabled":true,"crossover_hz":200}}"#;
    assert_error(
        &service.handle(&post_json("/api/calibration/settings", bad_crossover)),
        400,
        CODE_INVALID_FIELD,
    );
    assert_error(
        &service.handle(&post_json(
            "/api/calibration/settings",
            r#"{"mic":{"kind":"usb","device":"hw:U18dB,0"},"bogus":1}"#,
        )),
        400,
        CODE_UNKNOWN_FIELD,
    );
    // cal-file 上传：host 字段未落地 -> 501 明确 pending。
    assert_error(
        &service.handle(&post_json("/api/calibration/settings/cal-file", "{}")),
        501,
        CODE_PENDING_HOST_FIELD,
    );
}

#[test]
fn t42_rest_calibration_level_delay_bass_lanes_and_apply_clear() {
    let service = service_with_lanes_calibration();
    // level：kind=emperor + stage=level + 显式出声授权。
    let level = service.handle(&post_json(
        "/api/calibration/jobs",
        r#"{"kind":"emperor","stage":"level","audible_sweep_authorized":true}"#,
    ));
    assert_status(&level, 201);
    let body = json_body(&level);
    assert_eq!(expect_str(expect_obj(&body, "job"), "stage"), "level");
    let lanes = expect_obj(&body, "lanes");
    assert_eq!(expect_str(lanes, "schema"), CAL_LANES_SCHEMA);
    assert_eq!(expect_str(lanes, "stage"), "level");
    let rows = expect_array(lanes, "lanes");
    assert_eq!(rows.len(), 7);
    assert_eq!(expect_str(&rows[0], "lane"), "FL");
    assert_eq!(expect_str(&rows[0], "state"), "locked");
    expect_num(&rows[0], "achieved_dbfs");
    expect_num(&rows[0], "snr_db");
    expect_num(&rows[0], "locked_gain_db");
    let progress_text = expect_str(&body, "progress_text");
    assert!(progress_text.contains("Level"), "{progress_text}");
    let job_id = expect_str(&body, "job_id");
    let fetched = json_body(&service.handle(&get(&format!("/api/calibration/jobs/{job_id}"))));
    assert_eq!(expect_array(expect_obj(&fetched, "lanes"), "lanes").len(), 7);

    // delay：LFE 用互相关方法。
    let delay = service.handle(&post_json(
        "/api/calibration/jobs",
        r#"{"kind":"emperor","stage":"delay","audible_sweep_authorized":true}"#,
    ));
    assert_status(&delay, 201);
    let body = json_body(&delay);
    assert_eq!(expect_str(expect_obj(&body, "job"), "stage"), "delay");
    let rows = expect_array(expect_obj(&body, "lanes"), "lanes");
    assert_eq!(expect_str(&rows[0], "method"), "double_gate");
    assert_eq!(
        expect_str(rows.iter().find(|row| expect_str(row, "lane") == "SUB1").expect("SUB1"), "method"),
        "lfe_xcorr_40_150"
    );
    expect_num(&rows[0], "relative_samples");

    // bass：多炮结果 + 护栏。
    let bass = service.handle(&post_json(
        "/api/calibration/jobs",
        r#"{"kind":"emperor","stage":"bass","audible_sweep_authorized":true}"#,
    ));
    assert_status(&bass, 201);
    let body = json_body(&bass);
    assert_eq!(expect_str(expect_obj(&body, "job"), "stage"), "bass");
    let bass_result = expect_obj(expect_obj(&body, "lanes"), "bass");
    assert!(expect_bool(bass_result, "guardrail_passed"));
    assert_eq!(expect_array(bass_result, "subs").len(), 3);
    expect_num(bass_result, "pre_gain_db");

    // apply：level trims 7 条 + delay；状态写入 daemon。
    let apply_level = service.handle(&post_json(
        "/api/calibration/apply",
        r#"{"targets":["level"],"level_trims":[{"lane":"FL","gain_db":-3.0},{"lane":"FR","gain_db":-2.5},{"lane":"SL","gain_db":-2.0},{"lane":"SR","gain_db":-1.5},{"lane":"HFL","gain_db":-1.0},{"lane":"HFR","gain_db":-0.5},{"lane":"SUB1","gain_db":0.0}]}"#,
    ));
    assert_status(&apply_level, 200);
    let applied = json_body(&apply_level);
    assert_eq!(expect_str(&applied, "schema"), CAL_APPLY_SCHEMA);
    assert_eq!(expect_array(expect_obj(&applied, "applied"), "level_trims").len(), 7);
    assert!(!expect_bool(&applied, "applied_to_audio"));
    assert!(expect_bool(&applied, "not_applied_to_audio"));
    let delay_apply = service.handle(&post_json(
        "/api/calibration/apply",
        r#"{"targets":["delay"],"delays":[{"lane":"FL","delay_samples":0},{"lane":"SUB1","delay_samples":24}]}"#,
    ));
    assert_status(&delay_apply, 200);
    let bass_apply = service.handle(&post_json(
        "/api/calibration/apply",
        r#"{"targets":["bass"],"bass":{"enabled":true,"crossover_hz":80,"main_slope":"LR4","sub_slope":"LR4","linear_phase":true,"sub_gain_db":0,"strategy":"multi_b"}}"#,
    ));
    assert_status(&bass_apply, 200);
    let status = json_body(&service.handle(&get("/api/calibration/status")));
    assert_eq!(expect_str(&status, "schema"), CAL_STATUS_SCHEMA);
    let applied = expect_obj(&status, "applied");
    assert!(expect_bool(expect_obj(applied, "bass"), "enabled"));
    let workflow = expect_array(&status, "workflow");
    assert!(workflow.iter().any(|item| expect_str(item, "stage") == "level"));
    assert!(workflow.iter().any(|item| expect_str(item, "stage") == "bass"));

    // clear。
    let cleared = service.handle(&post_json("/api/calibration/clear", r#"{"targets":["level"]}"#));
    assert_status(&cleared, 200);
    let body = json_body(&cleared);
    assert!(expect_array(&body, "cleared").iter().any(|item| item == &JsonValue::str("level")));
    expect_null(expect_obj(&body, "applied"), "bass");

    // 校验：level 必须 kind=emperor + 出声授权；未知 lane；bass 需要出声授权。
    assert_error(
        &service.handle(&post_json(
            "/api/calibration/jobs",
            r#"{"kind":"full","stage":"level","audible_sweep_authorized":true}"#,
        )),
        400,
        CODE_INVALID_FIELD,
    );
    assert_error(
        &service.handle(&post_json(
            "/api/calibration/jobs",
            r#"{"kind":"emperor","stage":"level"}"#,
        )),
        400,
        CODE_INVALID_FIELD,
    );
    assert_error(
        &service.handle(&post_json(
            "/api/calibration/jobs",
            r#"{"kind":"emperor","stage":"delay","audible_sweep_authorized":true,"lanes":["FL","BOGUS"]}"#,
        )),
        400,
        CODE_INVALID_FIELD,
    );
    assert_error(
        &service.handle(&post_json(
            "/api/calibration/jobs",
            r#"{"kind":"emperor","stage":"crossover","audible_sweep_authorized":true}"#,
        )),
        400,
        CODE_INVALID_FIELD,
    );
    // apply 缺 trims -> 400；未知 lane -> 400。
    assert_error(
        &service.handle(&post_json("/api/calibration/apply", r#"{"targets":["level"]}"#)),
        400,
        CODE_MISSING_FIELD,
    );
    assert_error(
        &service.handle(&post_json(
            "/api/calibration/apply",
            r#"{"targets":["level"],"level_trims":[{"lane":"BOGUS","gain_db":0}]}"#,
        )),
        400,
        CODE_INVALID_FIELD,
    );
}

#[test]
fn t42_rest_calibration_apply_clear_pending_501_with_default_port() {
    let service = service_with_memory_calibration();
    // MemoryCalibration 未实现 apply/clear 的 host 字段：默认 501 pending_host_field。
    assert_error(
        &service.handle(&post_json(
            "/api/calibration/apply",
            r#"{"targets":["level"],"level_trims":[{"lane":"FL","gain_db":-1}]}"#,
        )),
        501,
        CODE_PENDING_HOST_FIELD,
    );
    assert_error(
        &service.handle(&post_json("/api/calibration/clear", "{}")),
        501,
        CODE_PENDING_HOST_FIELD,
    );
    // 默认设置 status 仍可读（不依赖 host 字段）。
    let status = service.handle(&get("/api/calibration/status"));
    assert_status(&status, 200);
    assert_eq!(
        expect_str(&json_body(&status), "calibration_state"),
        "RELATIVE_UNCALIBRATED"
    );
}
