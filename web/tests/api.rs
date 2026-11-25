//! 路由/集成测试：tower ServiceExt 直接调用 axum Router（无需监听端口）。
//!
//! 覆盖：认证/CSRF、总览、CamillaDSP、PTP、AES67/Inferno、ALSA、音频格式、
//! 运行模式（双向切换、互斥 409、失败回滚、重启恢复）、电源、静态资源、安全头。

use std::time::Duration;

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use khaen_audio_web::build_router;
use khaen_audio_web::config::{AppConfig, RunMode};
use khaen_audio_web::state::{AppState, MockOptions};

const TOKEN: &str = "mock-token";

struct TestApp {
    router: Router,
    state: AppState,
}

impl TestApp {
    fn new() -> Self {
        Self::with_config(MockOptions::default(), |_| {})
    }
    fn with_config(opts: MockOptions, mutate: impl FnOnce(&mut AppConfig)) -> Self {
        let mut config = AppConfig::mock_defaults();
        mutate(&mut config);
        let state = AppState::mock_with(config, opts);
        let router = build_router(state.clone());
        Self { router, state }
    }

    async fn send(&self, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("router oneshot");
        let status = response.status();
        let headers = response.headers().clone();
        let content_type = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
            .await
            .expect("read body");
        let body = if content_type.contains("application/json") {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        } else {
            Value::String(String::from_utf8_lossy(&bytes).to_string())
        };
        (status, headers, body)
    }

    async fn get_token(&self, uri: &str) -> (StatusCode, HeaderMap, Value) {
        self.send(request("GET", uri, None, &[("x-auth-token", TOKEN)]))
            .await
    }

    async fn post_token(&self, uri: &str, body: Value) -> (StatusCode, HeaderMap, Value) {
        self.send(request("POST", uri, Some(body), &[("x-auth-token", TOKEN)]))
            .await
    }

    async fn put_token(&self, uri: &str, body: Value) -> (StatusCode, HeaderMap, Value) {
        self.send(request("PUT", uri, Some(body), &[("x-auth-token", TOKEN)]))
            .await
    }

    async fn login(&self) -> (String, String) {
        let (status, headers, body) = self
            .send(request(
                "POST",
                "/api/auth/login",
                Some(json!({ "token": TOKEN })),
                &[],
            ))
            .await;
        assert_eq!(status, StatusCode::OK, "login failed: {body}");
        let cookie = headers
            .get_all(header::SET_COOKIE)
            .iter()
            .next()
            .expect("Set-Cookie")
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let csrf = body["csrf_token"].as_str().expect("csrf_token").to_string();
        (cookie, csrf)
    }
}

fn request(method: &str, uri: &str, body: Option<Value>, extra: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    let mut has_content_type = false;
    for (key, value) in extra {
        if *key == "content-type" {
            has_content_type = true;
        }
        builder = builder.header(*key, *value);
    }
    let body = match body {
        Some(value) => {
            if !has_content_type {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
            }
            Body::from(serde_json::to_vec(&value).unwrap())
        }
        None => Body::empty(),
    };
    builder.body(body).unwrap()
}

fn request_with_cookie(
    method: &str,
    uri: &str,
    body: Option<Value>,
    cookie: &str,
    csrf: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, cookie);
    if let Some(csrf) = csrf {
        builder = builder.header("x-csrf-token", csrf);
    }
    let body = match body {
        Some(value) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&value).unwrap())
        }
        None => Body::empty(),
    };
    builder.body(body).unwrap()
}

async fn wait_mode(app: &TestApp, timeout_ms: u64) -> Value {
    let started = std::time::Instant::now();
    loop {
        let (status, _, body) = app.get_token("/api/mode").await;
        assert_eq!(status, StatusCode::OK);
        if body["state"] != Value::String("switching".into()) {
            return body;
        }
        if started.elapsed() > Duration::from_millis(timeout_ms) {
            panic!("等待模式切换完成超时: {body}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ===== 认证 =====

#[tokio::test]
async fn healthz_is_public() {
    let app = TestApp::new();
    let (status, headers, body) = app.send(request("GET", "/healthz", None, &[])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["mock"], true);
    assert!(body["version"].as_str().unwrap().starts_with("0."));
    assert!(headers.contains_key("content-security-policy"));
    assert_eq!(headers["x-frame-options"], "DENY");
}

#[tokio::test]
async fn api_requires_auth() {
    let app = TestApp::new();
    for uri in [
        "/api/overview",
        "/api/mode",
        "/api/camilladsp",
        "/api/alsa",
        "/api/motu/status",
        "/api/motu/snapshot",
    ] {
        let (status, _, body) = app.send(request("GET", uri, None, &[])).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}: {body}");
        assert_eq!(body["error"]["code"], "unauthorized");
    }
}

#[tokio::test]
async fn login_session_and_logout() {
    let app = TestApp::new();
    let (cookie, csrf) = app.login().await;
    assert!(cookie.starts_with("khaen_session="));

    // 会话可用
    let (status, _, body) = app
        .send(request_with_cookie(
            "GET",
            "/api/auth/session",
            None,
            &cookie,
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["authenticated"], true);
    assert_eq!(body["auth_kind"], "session");
    assert_eq!(body["csrf_token"].as_str(), Some(csrf.as_str()));

    let (status, _, body) = app
        .send(request_with_cookie(
            "GET",
            "/api/overview",
            None,
            &cookie,
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["mock"], true);

    // 注销后会话失效
    let (status, _, _) = app
        .send(request_with_cookie(
            "POST",
            "/api/auth/logout",
            None,
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = app
        .send(request_with_cookie(
            "GET",
            "/api/overview",
            None,
            &cookie,
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn wrong_token_rejected() {
    let app = TestApp::new();
    let (status, _, body) = app
        .send(request(
            "POST",
            "/api/auth/login",
            Some(json!({ "token": "wrong" })),
            &[],
        ))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "unauthorized");
}

#[tokio::test]
async fn first_run_setup_password_then_password_login() {
    let app = TestApp::new();
    let (status, _, body) = app
        .send(request("GET", "/api/auth/status", None, &[]))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["setup_required"], true);
    assert_eq!(body["password_configured"], false);
    assert_eq!(body["token_fallback"], true);

    // 弱密码 / 不一致
    let (status, _, _) = app
        .send(request(
            "POST",
            "/api/auth/setup",
            Some(json!({ "password": "short", "confirm": "short" })),
            &[],
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = app
        .send(request(
            "POST",
            "/api/auth/setup",
            Some(json!({ "password": "correct-horse", "confirm": "different" })),
            &[],
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 首次设置成功，立即返回会话 cookie
    let (status, headers, body) = app
        .send(request(
            "POST",
            "/api/auth/setup",
            Some(json!({ "password": "correct-horse", "confirm": "correct-horse" })),
            &[],
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["csrf_token"].as_str().unwrap().len() >= 32);
    let cookie = headers
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    assert!(cookie.starts_with("khaen_session="));

    // 已配置后 status 不再要求 setup，重复 setup 409
    let (_, _, body) = app
        .send(request("GET", "/api/auth/status", None, &[]))
        .await;
    assert_eq!(body["setup_required"], false);
    assert_eq!(body["password_configured"], true);
    let (status, _, body) = app
        .send(request(
            "POST",
            "/api/auth/setup",
            Some(json!({ "password": "another-pass", "confirm": "another-pass" })),
            &[],
        ))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // 会话可用
    let (status, _, body) = app
        .send(request_with_cookie(
            "GET",
            "/api/auth/session",
            None,
            &cookie,
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["authenticated"], true);

    // 密码登录；token 仍可恢复登录
    let (status, _, body) = app
        .send(request(
            "POST",
            "/api/auth/login",
            Some(json!({ "password": "correct-horse" })),
            &[],
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _, body) = app
        .send(request(
            "POST",
            "/api/auth/login",
            Some(json!({ "token": TOKEN })),
            &[],
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _, body) = app
        .send(request(
            "POST",
            "/api/auth/login",
            Some(json!({ "password": "wrong-password" })),
            &[],
        ))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "unauthorized");
}

#[tokio::test]
async fn password_login_locks_after_failures() {
    let app = TestApp::new();
    let (status, _, _) = app
        .send(request(
            "POST",
            "/api/auth/setup",
            Some(json!({ "password": "correct-horse", "confirm": "correct-horse" })),
            &[],
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    for _ in 0..5 {
        let (status, _, _) = app
            .send(request(
                "POST",
                "/api/auth/login",
                Some(json!({ "password": "bad-password" })),
                &[],
            ))
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, _, body) = app
        .send(request(
            "POST",
            "/api/auth/login",
            Some(json!({ "password": "correct-horse" })),
            &[],
        ))
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["error"]["code"], "too_many_requests");
}

#[tokio::test]
async fn write_requires_csrf_for_session() {
    let app = TestApp::new();
    let (cookie, csrf) = app.login().await;
    // F-04：旧 web 的 CamillaDSP 启停路由已删除；写接口用合法的 /api/audio/format PUT 验证 CSRF。
    let payload = json!({ "chunksize": 128 });
    // 无 CSRF → 403
    let (status, _, body) = app
        .send(request_with_cookie(
            "PUT",
            "/api/audio/format",
            Some(payload.clone()),
            &cookie,
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "forbidden");
    // 错误 CSRF → 403
    let (status, _, _) = app
        .send(request_with_cookie(
            "PUT",
            "/api/audio/format",
            Some(payload.clone()),
            &cookie,
            Some("wrong"),
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // 正确 CSRF → 200
    let (status, _, body) = app
        .send(request_with_cookie(
            "PUT",
            "/api/audio/format",
            Some(payload),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);
}

#[tokio::test]
async fn token_header_can_write_without_csrf() {
    let app = TestApp::new();
    let (status, _, body) = app
        .put_token("/api/audio/format", json!({ "chunksize": 128 }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);
}

#[tokio::test]
async fn camilladsp_control_routes_removed() {
    // F-04/T72 前置：旧 web 不再注册 CamillaDSP 启停接口；GET status 标记 legacy。
    let app = TestApp::new();
    for path in [
        "/api/camilladsp/start",
        "/api/camilladsp/stop",
        "/api/camilladsp/restart",
    ] {
        let (status, _, _) = app.post_token(path, json!({})).await;
        assert!(
            status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED,
            "{path} 不应再是可调用的启停接口（实际 {status}）"
        );
    }
    let (status, _, body) = app.get_token("/api/camilladsp").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["legacy"], true, "只读 status 必须显式标记 legacy");
}

// ===== 总览 =====

#[tokio::test]
async fn overview_has_expected_data() {
    let app = TestApp::new();
    let (status, _, body) = app.get_token("/api/overview").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["mock"], true);
    assert!(body["system"]["cpu"]["cores"].as_u64().unwrap() > 0);
    assert!(body["system"]["realtime"]["summary"]
        .as_str()
        .unwrap()
        .contains("PREEMPT_RT"));
    assert!(!body["services"].as_array().unwrap().is_empty());
    let units: Vec<&str> = body["services"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["unit"].as_str().unwrap())
        .collect();
    assert!(units.contains(&"tspi-camilladsp.service"), "{units:?}");
    assert!(units.contains(&"tspi-ptp4l-master.service"), "{units:?}");
    assert!(units.contains(&"tspi-phc2sys.service"), "{units:?}");
    assert!(units.contains(&"tspi-statime-inferno.service"), "{units:?}");
    assert!(units.contains(&"tspi-aes67-daemon.service"), "{units:?}");
    assert!(
        !units.contains(&"camilladsp.service"),
        "旧名字不应再出现: {units:?}"
    );
    assert!(body["service_notes"][0]
        .as_str()
        .unwrap()
        .contains("Inferno"));
    assert_eq!(body["mode"]["mode"], "off");
    assert!(body["alsa"]["motu_present"].as_bool().unwrap());
    assert_eq!(body["camilla"]["live"]["state"], "Running");
    assert!(body["camilla"]["xrun"]["recent_count"].as_u64().unwrap() >= 1);
    assert_eq!(body["power"]["allow_reboot"], false);
}

// ===== CamillaDSP =====

#[tokio::test]
async fn camilladsp_status_volume_configs() {
    let app = TestApp::new();
    let (status, _, body) = app.get_token("/api/camilladsp").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["live"]["state"], "Running");
    assert_eq!(body["live"]["version"], "4.1.3");
    assert!(body["configs"].as_array().unwrap().len() >= 3);
    assert_eq!(body["mock"], true);
    assert_eq!(body["legacy"], true, "F-04：status 只读且标记 legacy");

    let (status, _, body) = app
        .post_token("/api/camilladsp/volume", json!({ "db": -20.0 }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["volume_db"], -20.0);
    let (_, _, body) = app.get_token("/api/camilladsp").await;
    assert_eq!(body["live"]["volume_db"], -20.0);

    // 越界音量
    let (status, _, _) = app
        .post_token("/api/camilladsp/volume", json!({ "db": 100.0 }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn camilladsp_switch_config_and_validation() {
    let app = TestApp::new();
    // 合法切换
    let (status, _, body) = app
        .post_token(
            "/api/camilladsp/configs/switch",
            json!({ "name": "dante.yml" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["outcome"]["applied_via"], "websocket");
    let (_, _, content) = app.get_token("/api/camilladsp/config").await;
    assert!(content["content"].as_str().unwrap().contains("96000"));

    // 非法 YAML
    let (status, _, body) = app
        .post_token(
            "/api/camilladsp/configs/switch",
            json!({ "name": "broken.yml" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["error"]["message"].as_str().unwrap().contains("YAML"));

    // 路径穿越 / 不存在的文件
    for name in ["../etc/passwd.yml", "missing.yml", "aes67.json"] {
        let (status, _, _) = app
            .post_token("/api/camilladsp/configs/switch", json!({ "name": name }))
            .await;
        assert!(
            status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND,
            "{name} 应被拒绝，实际 {status}"
        );
    }

    // 校验端点
    let (status, _, body) = app
        .post_token(
            "/api/camilladsp/config/validate",
            json!({ "content": "devices:\n  samplerate: 48000\n" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["valid"], true);
    let (status, _, _) = app
        .post_token(
            "/api/camilladsp/config/validate",
            json!({ "content": "devices: [unclosed\n" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ===== PTP =====

#[tokio::test]
async fn ptp_status_and_config_update() {
    let app = TestApp::new();
    let (status, _, body) = app.get_token("/api/ptp").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"]["locked"], true);
    assert_eq!(body["status"]["offset_ns"], -37);
    assert_eq!(body["status"]["port_state"], "SLAVE");
    assert!(!body["services"].as_array().unwrap().is_empty());
    assert!(!body["notes"].as_array().unwrap().is_empty());

    let (status, _, body) = app
        .put_token(
            "/api/ptp/config",
            json!({ "master_only": true, "priority1": 5 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["outcome"]["conf"]["master_only"], true);
    assert_eq!(body["outcome"]["conf"]["priority1"], 5);
    assert!(body["outcome"]["backup"].as_str().is_some());

    let (_, _, body) = app.get_token("/api/ptp/config").await;
    assert_eq!(body["master_only"], true);
    assert_eq!(body["priority1"], 5);
    // 至少提供一个字段
    let (status, _, _) = app.put_token("/api/ptp/config", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ===== AES67 / Inferno =====

#[tokio::test]
async fn aes67_and_inferno_endpoints() {
    let app = TestApp::new();
    for (kind, label) in [("aes67", "AES67"), ("inferno", "Inferno")] {
        let uri = format!("/api/{kind}");
        let (status, _, body) = app.get_token(&uri).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["kind"], kind);
        assert!(body["label"].as_str().unwrap().contains(label));
        assert!(body["service"]["available"].as_bool().unwrap());
        assert!(!body["config"].as_array().unwrap().is_empty());
        assert!(!body["notes"].as_array().unwrap().is_empty());

        let (status, _, body) = app.get_token(&format!("/api/{kind}/logs?lines=10")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!body["text"].as_str().unwrap().is_empty());

        let (status, _, body) = app.get_token(&format!("/api/{kind}/config")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!body["files"].as_array().unwrap().is_empty());

        let (status, _, body) = app
            .post_token(&format!("/api/{kind}/start"), json!({}))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["service"]["active"], true);
        let (status, _, body) = app
            .post_token(&format!("/api/{kind}/stop"), json!({}))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["service"]["active"], false);
    }
}

// ===== ALSA =====

#[tokio::test]
async fn alsa_view_and_test_tone() {
    let app = TestApp::new();
    let (status, _, body) = app.get_token("/api/alsa").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["motu_present"], true);
    assert!(body["cards"].as_array().unwrap().len() >= 2);
    assert!(!body["hw_params"].as_array().unwrap().is_empty());
    assert!(body["usb_devices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["is_motu"] == true));

    let (status, _, body) = app
        .post_token("/api/alsa/test-tone", json!({ "device": "plughw:1,0" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);
    assert_eq!(body["result"]["device"], "plughw:1,0");

    let (status, _, _) = app
        .post_token(
            "/api/alsa/test-tone",
            json!({ "device": "bad device;rm -rf" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ===== 音频格式 =====

#[tokio::test]
async fn audio_format_get_and_limits() {
    let app = TestApp::new();
    let (status, _, body) = app.get_token("/api/audio/format").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sample_rate"], 48000);
    assert_eq!(body["bit_depth"], 24);
    assert_eq!(body["channels"], 2);
    assert_eq!(body["chunksize"], 128);
    assert_eq!(body["profiles"].as_array().unwrap().len(), 2);
    assert_eq!(
        body["limits"]["sample_rates"],
        json!([44100, 48000, 88200, 96000]),
        "白名单只保留 ≤96 kHz 四档"
    );
    assert!(body["limits"]["chunksizes"]
        .as_array()
        .unwrap()
        .contains(&json!(32)));
    assert!(body["output_format"].as_str().unwrap().contains("MOTU"));

    // 合法修改
    let (status, _, body) = app
        .put_token(
            "/api/audio/format",
            json!({ "chunksize": 256, "sample_rate": 96000 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["format"]["chunksize"], 256);
    assert_eq!(body["format"]["sample_rate"], 96000);

    // 越界
    for payload in [
        json!({ "chunksize": 100 }),
        json!({ "sample_rate": 12345 }),
        json!({ "target_level": 999999 }),
        json!({}),
    ] {
        let (status, _, _) = app.put_token("/api/audio/format", payload.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{payload} 应被拒绝");
    }

    // chunksize=32 警告
    let (status, _, body) = app
        .put_token("/api/audio/format", json!({ "chunksize": 32 }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w.as_str().unwrap().contains("高风险")));
}

#[tokio::test]
async fn audio_format_apply_failure_keeps_old_config() {
    let app = TestApp::with_config(
        MockOptions {
            camilla_apply_fails: true,
            ..Default::default()
        },
        |_| {},
    );
    let (status, _, body) = app
        .put_token("/api/audio/format", json!({ "chunksize": 512 }))
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    // 原配置未被改动
    let (_, _, body) = app.get_token("/api/audio/format").await;
    assert_eq!(body["chunksize"], 128);
}

#[tokio::test]
async fn audio_format_rejects_192k_with_stable_error() {
    // 用户硬约束：禁 176.4k/192k（默认 48k，96k 可）；PUT 必须稳定拒绝且不改配置。
    let app = TestApp::new();
    let (status, _, body) = app.get_token("/api/audio/format").await;
    assert_eq!(status, StatusCode::OK);
    let rates: Vec<u64> = body["limits"]["sample_rates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect();
    assert_eq!(rates, vec![44100, 48000, 88200, 96000]);
    assert!(!rates.contains(&192000));
    assert!(!rates.contains(&176400));

    for forbidden in [192000u64, 176400] {
        let (status, _, body) = app
            .put_token("/api/audio/format", json!({ "sample_rate": forbidden }))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "bad_request", "{body}");
        let message = body["error"]["message"].as_str().unwrap();
        assert!(
            message.contains(&format!("{forbidden} Hz 已禁用")),
            "{message}"
        );
        assert!(message.contains("禁止 176400/192000 Hz"), "{message}");
    }

    // 拒绝路径不得改动当前配置（mock 默认 48k）
    let (status, _, body) = app.get_token("/api/audio/format").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sample_rate"], 48000);
}

#[tokio::test]
async fn app_js_sample_rate_dropdown_is_data_driven_and_has_no_192k() {
    // UI 下拉只遍历后端 limits.sample_rates；静态资源中不得出现 192k/176.4k 档位。
    let app = TestApp::new();
    let (status, _, body) = app.send(request("GET", "/app.js", None, &[])).await;
    assert_eq!(status, StatusCode::OK);
    let js = body.as_str().unwrap();
    assert!(
        js.contains("limits.sample_rates"),
        "采样率下拉应由后端 limits 驱动"
    );
    assert!(!js.contains("192000"), "UI 不得出现 192k 档位");
    assert!(!js.contains("176400"), "UI 不得出现 176.4k 档位");
}

// ===== 电源 =====

#[tokio::test]
async fn power_disabled_by_default() {
    let app = TestApp::new();
    let (status, _, body) = app
        .post_token("/api/system/reboot", json!({ "confirm": "REBOOT" }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "forbidden");
    assert!(app.state.mock_power_actions().is_empty());
}

#[tokio::test]
async fn power_enabled_requires_phrase() {
    let app = TestApp::with_config(MockOptions::default(), |config| {
        config.system.allow_reboot = true;
        config.system.allow_shutdown = true;
    });
    let (status, _, body) = app
        .post_token("/api/system/reboot", json!({ "confirm": "wrong" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, _, body) = app
        .post_token("/api/system/reboot", json!({ "confirm": "REBOOT" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["mock"], true);
    assert_eq!(app.state.mock_power_actions(), vec!["reboot".to_string()]);

    let (status, _, _) = app
        .post_token("/api/system/shutdown", json!({ "confirm": "SHUTDOWN" }))
        .await;
    assert_eq!(status, StatusCode::OK);
}

// ===== 运行模式 =====

#[tokio::test]
async fn mode_initial_state_and_components() {
    let app = TestApp::new();
    let (status, _, body) = app.get_token("/api/mode").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["mode"], "off");
    assert_eq!(body["state"], "idle");
    assert_eq!(body["enabled"], true);
    assert_eq!(body["profiles"].as_array().unwrap().len(), 2);
    assert!(body["components"].is_object());
    // B1：AES67 profile 不包含 linuxptp
    let aes67 = body["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["mode"] == "aes67")
        .unwrap();
    assert!(aes67["ptp_services"].as_array().unwrap().is_empty());
    assert_eq!(aes67["ptp_source"], "aes67-daemon");
    let dante = body["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["mode"] == "dante")
        .unwrap();
    assert_eq!(dante["input_format"], "96 kHz / 24-bit / 2 ch");
}

#[tokio::test]
async fn mode_switch_both_directions_and_off() {
    let app = TestApp::new();
    // off → aes67
    let (status, _, body) = app.put_token("/api/mode", json!({ "mode": "aes67" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["state"], "switching");
    let body = wait_mode(&app, 8000).await;
    assert_eq!(body["state"], "idle");
    assert_eq!(body["mode"], "aes67");
    assert_eq!(body["components"]["ptp"]["source"], "aes67-daemon");
    assert_eq!(body["components"]["audio"]["sample_rate"], 48000);
    assert_eq!(body["components"]["camilla"]["state"], "Running");
    assert_eq!(app.state.mock_store_mode(), Some(RunMode::Aes67));

    // aes67 → dante
    let (status, _, _) = app.put_token("/api/mode", json!({ "mode": "dante" })).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let body = wait_mode(&app, 8000).await;
    assert_eq!(body["mode"], "dante");
    assert_eq!(body["components"]["ptp"]["source"], "statime");
    assert_eq!(body["components"]["ptp"]["version"], "PTPv1");
    assert_eq!(body["components"]["audio"]["sample_rate"], 96000);
    assert_eq!(app.state.mock_store_mode(), Some(RunMode::Dante));

    // dante → off
    let (status, _, _) = app.put_token("/api/mode", json!({ "mode": "off" })).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let body = wait_mode(&app, 8000).await;
    assert_eq!(body["mode"], "off");
    assert_eq!(body["state"], "idle");
    assert_eq!(app.state.mock_store_mode(), Some(RunMode::Off));

    // 同一模式的重复切换 → 409
    let (status, _, _) = app.put_token("/api/mode", json!({ "mode": "off" })).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn mode_switch_conflicts_while_in_flight() {
    let app = TestApp::with_config(
        MockOptions {
            switch_step_delay_ms: 250,
            ..Default::default()
        },
        |_| {},
    );
    let (status, _, _) = app.put_token("/api/mode", json!({ "mode": "aes67" })).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, _, body) = app.put_token("/api/mode", json!({ "mode": "dante" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "conflict");
    let body = wait_mode(&app, 10000).await;
    assert_eq!(body["mode"], "aes67");
}

#[tokio::test]
async fn mode_switch_failure_rolls_back() {
    let app = TestApp::with_config(
        MockOptions {
            fail_start: vec!["inferno.service".into()],
            switch_step_delay_ms: 2,
            ..Default::default()
        },
        |_| {},
    );
    // 先切到 AES67
    let (status, _, _) = app.put_token("/api/mode", json!({ "mode": "aes67" })).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let body = wait_mode(&app, 8000).await;
    assert_eq!(body["mode"], "aes67");

    // 切 Dante 时 Inferno 启动失败 → 失败 + 回滚到 AES67
    let (status, _, _) = app.put_token("/api/mode", json!({ "mode": "dante" })).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let body = wait_mode(&app, 10000).await;
    assert_eq!(body["state"], "failed", "{body}");
    assert_eq!(body["mode"], "aes67", "失败后应回到切换前模式");
    assert!(body["last_error"].as_str().unwrap().contains("inferno"));
    assert!(body["steps"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["key"] == "rollback"));
    assert_eq!(app.state.mock_store_mode(), Some(RunMode::Aes67));

    // 回滚后 AES67 服务应重新运行，仍可再次切 off
    let (_, _, aes) = app.get_token("/api/aes67").await;
    assert_eq!(aes["service"]["active"], true);
    let (status, _, _) = app.put_token("/api/mode", json!({ "mode": "off" })).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let body = wait_mode(&app, 8000).await;
    assert_eq!(body["mode"], "off");
}

#[tokio::test]
async fn mode_restored_from_persisted_state() {
    let app = TestApp::with_config(
        MockOptions {
            persisted_mode: Some(RunMode::Dante),
            switch_step_delay_ms: 2,
            ..Default::default()
        },
        |config| {
            // 默认 D4 不自动恢复；本用例显式开启，验证可选的“恢复上次模式”能力
            config.mode.restore_on_start = true;
        },
    );
    // 模拟服务启动：按 state.json 恢复
    let restored = app
        .state
        .mode
        .restore_from_store()
        .await
        .expect("restore ok");
    assert!(restored.is_some());
    let body = wait_mode(&app, 10000).await;
    assert_eq!(body["mode"], "dante");
    assert_eq!(body["components"]["ptp"]["source"], "statime");
    assert_eq!(app.state.mock_store_mode(), Some(RunMode::Dante));
}

#[tokio::test]
async fn mode_disabled_rejects_switch() {
    let app = TestApp::with_config(MockOptions::default(), |config| {
        config.mode.enabled = false;
    });
    let (status, _, body) = app.put_token("/api/mode", json!({ "mode": "dante" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn mode_persisted_is_displayed_but_not_auto_loaded() {
    // D4：开机音频 off；state.json 里的上次模式只展示、不自动切换
    let app = TestApp::with_config(
        MockOptions {
            persisted_mode: Some(RunMode::Aes67),
            ..Default::default()
        },
        |_| {},
    );
    let (status, _, body) = app.get_token("/api/mode").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["mode"], "off");
    assert_eq!(body["persisted"]["mode"], "aes67");
    assert_eq!(body["persisted"]["auto_restore"], false);
    assert_eq!(body["restore_on_start"], false);
}

#[tokio::test]
async fn mode_script_status_endpoint_in_mock() {
    let app = TestApp::new();
    let (status, _, body) = app.get_token("/api/mode/script-status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["available"], false);
    assert_eq!(body["mock"], true);
    assert!(body["message"].as_str().unwrap().contains("mock"));
}

// ===== 系统 / 静态资源 =====

#[tokio::test]
async fn system_info_and_static_assets() {
    let app = TestApp::new();
    let (status, _, body) = app.get_token("/api/system").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["mock"], true);
    assert_eq!(body["allow_reboot"], false);

    let (status, headers, body) = app.send(request("GET", "/", None, &[])).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers["content-type"]
        .to_str()
        .unwrap()
        .contains("text/html"));
    assert!(body.as_str().unwrap().contains("泰山派"));
    assert!(!body.as_str().unwrap().contains("http://cdn"));

    let (status, headers, _) = app.send(request("GET", "/app.js", None, &[])).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers["content-type"]
        .to_str()
        .unwrap()
        .contains("javascript"));

    let (status, headers, _) = app.send(request("GET", "/test-tone.wav", None, &[])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "audio/wav");

    let (status, _, _) = app.send(request("GET", "/nope", None, &[])).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ===== 声道分配 / channelmap v2 =====

async fn channelmap_draft_5_1_2(app: &TestApp) -> Value {
    let (status, _, body) = app
        .post_token(
            "/api/channelmap/draft",
            json!({ "input_layout": "h16-5.1.2", "output_layout": "5.1.2" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["document"].clone()
}

#[tokio::test]
async fn channelmap_view_needs_setup_and_real_presets() {
    let app = TestApp::new();
    let (status, _, body) = app.get_token("/api/channelmap").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], true);
    assert_eq!(body["needs_setup"], true);
    assert!(body["document"].is_null());
    assert_eq!(body["presets"]["schema"], "tspi-layout-presets/v2");
    assert!(body["presets"]["input_layouts"]["h16-7.1.4"].is_object());
    assert!(body["presets"]["output_layouts"]["5.1.2"].is_object());
    assert!(body["presets"]["remap_presets"]["5.1.2->5.1.2"].is_object());
    assert!(!body["presets"]["remap_presets"]["7.1.4->5.3.2"].is_object());
    assert!(!body["presets"]["remap_presets"]["7.1.4->4.1.2"].is_object());
    // 板端 MOTU 真相（audio/overlay/etc/tspi-audio/layout-presets.yml）：
    // FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1=7，ch6 空，SUB=Line Out 8。
    let l412 = &body["presets"]["output_layouts"]["4.1.2"]["roles"];
    let roles: Vec<(String, Vec<u64>)> = l412
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap().to_string(),
                r["outputs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|x| x.as_u64().unwrap())
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        roles,
        vec![
            ("FL".to_string(), vec![0u64]),
            ("FR".to_string(), vec![1]),
            ("SL".to_string(), vec![2]),
            ("SR".to_string(), vec![3]),
            ("HFL".to_string(), vec![4]),
            ("HFR".to_string(), vec![5]),
            ("SUB1".to_string(), vec![7]),
        ]
    );
    assert!(!roles.iter().any(|(_, outs)| outs.contains(&6)));
    assert_eq!(body["probe"]["playback"]["channels_max"], 22);
    assert_eq!(body["probe"]["capture"]["channels_max"], 16);
    assert_eq!(body["mode_off"], true);
    assert_eq!(body["mock"], true);
    assert!(body["effective_roles"].as_array().unwrap().is_empty());
    // 无 active 时 apply → 400
    let (status, _, _) = app.post_token("/api/channelmap/apply", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn channelmap_input_mode_defaults_and_override() {
    let app = TestApp::new();
    let (status, _, body) = app.get_token("/api/channelmap/input-mode").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["content_mode"], "auto");
    assert_eq!(body["detected"], "unknown");
    assert!(body["override"].is_null());
    assert_eq!(body["state_available"], false);
    assert_eq!(body["reason"], "daemon-not-running");

    let (status, _, body) = app
        .put_token(
            "/api/channelmap/input-mode",
            json!({ "mode": "force-stereo" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["override"], "stereo", "force-stereo 归一化为 stereo");
    assert_eq!(body["content_mode"], "stereo");

    let (status, _, body) = app.get_token("/api/channelmap/input-mode").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["override"], "stereo");
    assert_eq!(body["content_mode"], "stereo");

    let (status, _, body) = app
        .put_token("/api/channelmap/input-mode", json!({ "mode": "auto" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["override"], "auto");
    assert_eq!(body["content_mode"], "auto");

    let (status, _, _) = app
        .put_token("/api/channelmap/input-mode", json!({ "mode": "bogus" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn channelmap_draft_validate_save_and_apply_off() {
    let app = TestApp::new();
    let (status, _, draft) = app
        .post_token(
            "/api/channelmap/draft",
            json!({ "input_layout": "h16-5.1.2", "output_layout": "5.1.2" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{draft}");
    let document = draft["document"].clone();
    assert_eq!(document["schema"], "tspi-channelmap/v2");
    assert_eq!(document["version"], 2);
    assert_eq!(document["input"]["layout"], "h16-5.1.2");
    assert_eq!(document["output"]["layout"], "5.1.2");
    assert_eq!(document["output"]["roles"].as_array().unwrap().len(), 8);
    assert_eq!(document["remap"]["routes"].as_array().unwrap().len(), 0);
    let sub1 = document["output"]["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "SUB1")
        .unwrap();
    assert_eq!(sub1["sources"][0]["source"], "LFE");

    let (status, _, report) = app
        .post_token("/api/channelmap/validate", json!({ "document": document }))
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["validation"]["valid"], true, "{report}");
    assert_eq!(report["effective_roles"].as_array().unwrap().len(), 8);

    let (status, _, saved) = app
        .put_token("/api/channelmap", json!({ "document": document }))
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let (status, _, view) = app.get_token("/api/channelmap").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["needs_setup"], false);
    assert_eq!(
        view["document"]["output"]["roles"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
    assert_eq!(view["effective_roles"].as_array().unwrap().len(), 8);
    assert!(!view["input_roles"].as_array().unwrap().is_empty());

    let (status, _, applied) = app
        .post_token("/api/channelmap/apply", json!({ "document": document }))
        .await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    assert_eq!(applied["saved_only"], true);
    assert_eq!(applied["applied"], false);
    assert_eq!(applied["mode"], "off");
}

#[tokio::test]
async fn channelmap_apply_in_audio_mode_and_failure_rollback() {
    let app = TestApp::new();
    let document = channelmap_draft_5_1_2(&app).await;
    let (status, _, _) = app.put_token("/api/mode", json!({ "mode": "aes67" })).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    for _ in 0..100 {
        let (_, _, view) = app.get_token("/api/mode").await;
        if view["current"] == "aes67" && view["switching"] == false {
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let (status, _, applied) = app
        .post_token("/api/channelmap/apply", json!({ "document": document }))
        .await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    assert_eq!(applied["applied"], true);
    assert_eq!(applied["saved_only"], false);

    app.state.channelmap.set_mock_apply_fail(true);
    let (status, _, body) = app
        .post_token("/api/channelmap/apply", json!({ "document": document }))
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body["error"]["message"].as_str().unwrap().contains("回滚"));
    let (status, _, body) = app.post_token("/api/channelmap/rollback", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["rolled_back"], true);
}

#[tokio::test]
async fn channelmap_channel_mismatch_blocks_apply() {
    let app = TestApp::new();
    let mut document = channelmap_draft_5_1_2(&app).await;
    document["output"]["roles"][0]["outputs"] = json!([99]);
    let (status, _, report) = app
        .post_token("/api/channelmap/validate", json!({ "document": document }))
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["validation"]["valid"], false);
    assert_eq!(report["validation"]["channel_mismatch"], true);
    assert!(report["validation"]["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e.as_str().unwrap().contains("播放通道数")));

    let (status, _, _) = app
        .put_token("/api/channelmap", json!({ "document": document }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, body) = app
        .post_token("/api/channelmap/apply", json!({ "document": document }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _, body) = app
        .put_token(
            "/api/channelmap",
            json!({ "document": document, "force": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn channelmap_peq_type_and_q_rules_mirror_board_cli() {
    // 本地回退（mock/测试）必须镜像板端 tspi-channelmap：type 白名单 + 按 type 分流的 q 界限。
    let app = TestApp::new();
    let mut document = channelmap_draft_5_1_2(&app).await;
    document["output"]["roles"][0]["peq"] = json!([
        { "freq": 100.0, "q": 0.7, "gain_db": 2.5, "type": "low_shelf" }
    ]);
    let (status, _, report) = app
        .post_token("/api/channelmap/validate", json!({ "document": document }))
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["validation"]["valid"], true, "{report}");

    // shelf q=1.0（S=1 → slope=12）通过；q=1.5 拒绝（裁决 5）
    document["output"]["roles"][0]["peq"] = json!([
        { "freq": 100.0, "q": 1.0, "gain_db": -1.0, "type": "high_shelf" }
    ]);
    let (status, _, report) = app
        .post_token("/api/channelmap/validate", json!({ "document": document }))
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["validation"]["valid"], true, "{report}");

    document["output"]["roles"][0]["peq"] = json!([
        { "freq": 100.0, "q": 1.5, "gain_db": -1.0, "type": "low_shelf" }
    ]);
    let (status, _, report) = app
        .post_token("/api/channelmap/validate", json!({ "document": document }))
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["validation"]["valid"], false, "{report}");
    assert!(
        report["validation"]["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e.as_str().unwrap().contains("[0.1,1.0]")),
        "{report}"
    );

    // q > 10：板端 peaking q ∈ [0.1,10]，本地回退也必须拒绝（旧实现允许 1e6）
    document["output"]["roles"][0]["peq"] = json!([
        { "freq": 2000.0, "q": 15.0, "gain_db": -2.0, "type": "peaking" }
    ]);
    let (status, _, report) = app
        .post_token("/api/channelmap/validate", json!({ "document": document }))
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["validation"]["valid"], false, "{report}");
    assert!(
        report["validation"]["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e.as_str().unwrap().contains("[0.1, 10]")),
        "{report}"
    );

    // 未知 type：板端 schema error rc=2，本地必须同样拒绝
    document["output"]["roles"][0]["peq"] = json!([
        { "freq": 1000.0, "q": 1.0, "gain_db": -1.0, "type": "notch" }
    ]);
    let (status, _, report) = app
        .post_token("/api/channelmap/validate", json!({ "document": document }))
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["validation"]["valid"], false, "{report}");
    assert!(
        report["validation"]["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e.as_str().unwrap().contains("peq[0].type")),
        "{report}"
    );

    // q 越界的文档保存必须被阻止（PUT 无 force → 400）
    let (status, _, body) = app
        .put_token("/api/channelmap", json!({ "document": document }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn channelmap_profiles_export_import() {
    let app = TestApp::new();
    let document = channelmap_draft_5_1_2(&app).await;
    let (status, _, _) = app
        .put_token("/api/channelmap", json!({ "document": document }))
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, body) = app
        .post_token("/api/channelmap/profiles", json!({ "name": "alice-5.1.2" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["profile"]["name"], "alice-5.1.2");

    let (status, _, list) = app.get_token("/api/channelmap/profiles").await;
    assert_eq!(status, StatusCode::OK);
    assert!(list["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["name"] == "alice-5.1.2"));

    let (status, _, loaded) = app
        .post_token(
            "/api/channelmap/profiles/load",
            json!({ "name": "alice-5.1.2" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{loaded}");
    assert_eq!(loaded["document"]["output"]["layout"], "5.1.2");

    let (status, _, raw) = app.get_token("/api/channelmap/export").await;
    assert_eq!(status, StatusCode::OK);
    let text = raw.as_str().unwrap();
    assert!(text.contains("tspi-channelmap/v2"));

    let (status, _, body) = app
        .post_token(
            "/api/channelmap/import",
            json!({ "yaml": text, "name": "bob-import" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _, body) = app
        .post_token(
            "/api/channelmap/profiles/delete",
            json!({ "name": "bob-import" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, _, _) = app
        .post_token("/api/channelmap/profiles", json!({ "name": "../evil" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn channelmap_fir_upload_and_test_tone() {
    let app = TestApp::new();
    let mut taps = Vec::new();
    for i in 0..1024 {
        taps.extend_from_slice(format!("{i} 0\n").as_bytes());
    }
    let (status, _, entry) = app
        .post_token(
            "/api/channelmap/fir",
            json!({ "name": "fl_48k.txt", "data_base64": base64_encode(&taps) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{entry}");
    assert_eq!(entry["taps"], 1024);
    assert_eq!(entry["level"], "recommended");
    assert!(entry["group_delay_ms"].is_null());

    let wav = khaen_audio_web::channelmap::build_test_tone_wav(1, 0, 48_000, 100, false);
    let (status, _, entry) = app
        .post_token(
            "/api/channelmap/fir",
            json!({ "name": "fl_48k.wav", "data_base64": base64_encode(&wav) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{entry}");
    assert_eq!(entry["sample_rate"], 48000);
    assert_eq!(entry["taps"], 4800);
    assert_eq!(entry["level"], "experimental");
    assert!((entry["group_delay_ms"].as_f64().unwrap() - 50.0).abs() < 0.1);

    let (status, _, _) = app
        .post_token(
            "/api/channelmap/fir",
            json!({ "name": "../evil.wav", "data_base64": base64_encode(b"x") }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let document = channelmap_draft_5_1_2(&app).await;
    let (status, _, _) = app
        .put_token("/api/channelmap", json!({ "document": document }))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, tone) = app
        .post_token(
            "/api/channelmap/test-tone",
            json!({ "role": "FL", "kind": "sweep", "duration_ms": 200 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{tone}");
    assert_eq!(tone["played"], true);
    assert_eq!(tone["output_channel"], 0);
    assert_eq!(tone["channels"], 22);
    assert!(tone["wav_bytes"].as_u64().unwrap() > 1000);
    let (status, _, _) = app.post_token("/api/channelmap/test-tone", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn motu_status_and_direct_mute_roundtrip() {
    let app = TestApp::new();
    let (status, _, page) = app.send(request("GET", "/", None, &[])).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.as_str().unwrap().contains("data-page=\"motu\""));

    let (status, _, body) = app
        .send(request(
            "POST",
            "/api/motu/direct-mute",
            Some(json!({"mute": true, "confirm": true})),
            &[],
        ))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    let (status, _, body) = app.get_token("/api/motu/status").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["online"], true);
    assert_eq!(body["mock"], true);
    assert_eq!(body["main_trim"], 28);
    assert_eq!(body["main_db"], -28.0);
    assert_eq!(body["discovery"]["ip"], "169.254.111.126");
    assert!(body["discovery"]["uid"].is_string());
    assert!(body["discovery"]["version"].is_string());
    assert!(body["last_read_unix"].is_number());
    assert!(body["direct"].is_array());
    assert!(body["phantom"].is_array());
    assert!(body["direct_count"].as_u64().unwrap() >= 2);
    assert!(body["direct"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["alias"] == "in7→out7"));
    assert!(body["direct"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["alias"] == "in8→out8"));
    assert!(body["last_error"].is_null());

    let (status, _, snap) = app.get_token("/api/motu/snapshot").await;
    assert_eq!(status, StatusCode::OK, "{snap}");
    assert_eq!(snap["buses"].as_array().unwrap().len(), 14);
    assert_eq!(snap["inputs"].as_array().unwrap().len(), 32);
    assert!(snap["raw"].is_null());
    assert!(snap["nonzero"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["host"] == true && row["input_name"] == "USB Main L"));

    let (status, _, raw) = app.get_token("/api/motu/snapshot?raw=1").await;
    assert_eq!(status, StatusCode::OK, "{raw}");
    assert!(raw["raw"]["values"]["kMainTrim"].is_array());
    assert_eq!(raw["raw_omitted"], false);

    let (status, _, denied) = app
        .post_token("/api/motu/direct-mute", json!({ "mute": true }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{denied}");
    assert_eq!(denied["error"]["code"], "bad_request");

    let (status, _, muted) = app
        .post_token(
            "/api/motu/direct-mute",
            json!({ "mute": true, "confirm": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{muted}");
    assert_eq!(muted["verified"], true);
    assert_eq!(muted["rollback"], false);
    assert_eq!(muted["host_inputs_untouched"], true);
    assert!(muted["written"].as_u64().unwrap() > 0);
    assert!(muted["snapshot_id"].is_string());

    let (status, _, body) = app.get_token("/api/motu/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["direct_count"], 0);

    let (status, _, snap) = app.get_token("/api/motu/snapshot").await;
    assert_eq!(status, StatusCode::OK);
    let nonzero = snap["nonzero"].as_array().unwrap();
    assert!(nonzero.iter().any(|row| row["input_name"] == "USB Main L"));
    assert!(nonzero.iter().all(|row| row["direct"] == false));

    let (status, _, restored) = app
        .post_token(
            "/api/motu/direct-mute",
            json!({ "mute": false, "confirm": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert_eq!(restored["verified"], true);
    let (status, _, body) = app.get_token("/api/motu/status").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["direct_count"].as_u64().unwrap() >= 2);

    let (status, _, monitor) = app
        .post_token(
            "/api/motu/monitor",
            json!({ "trim_db": 10, "confirm": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{monitor}");
    assert_eq!(monitor["written"], 1);
    assert_eq!(monitor["verified"], true);
    let (status, _, body) = app.get_token("/api/motu/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["main_trim"], 10);
    assert_eq!(body["main_db"], -10.0);

    let (status, _, back) = app
        .post_token(
            "/api/motu/restore",
            json!({ "snapshot_id": muted["snapshot_id"], "confirm": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{back}");
    assert_eq!(back["verified"], true);
}

#[tokio::test]
async fn motu_apply_fail_returns_rollback() {
    let app = TestApp::with_config(
        MockOptions {
            motu_apply_fails: true,
            ..MockOptions::default()
        },
        |_| {},
    );
    let (status, _, body) = app
        .post_token(
            "/api/motu/direct-mute",
            json!({ "mute": true, "confirm": true }),
        )
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["ok"], false);
    assert_eq!(body["verified"], false);
    assert_eq!(body["rollback"], true);
    assert_eq!(body["rollback_verified"], true);
    assert_eq!(body["written"], 0);
    assert_eq!(body["error"]["code"], "internal_error");
    let (status, _, body) = app.get_token("/api/motu/status").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["direct_count"].as_u64().unwrap() >= 2);
    assert!(body["last_error"].is_string());
}

#[tokio::test]
async fn speaker_route_preset_compiles_and_draft_rejects_remap() {
    let app = TestApp::new();
    let (status, _, body) = app.get_token("/api/channelmap/speaker-route").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["center_gain_label"], "−3 dB");
    assert!(body["roles"].as_array().unwrap().len() >= 12);

    let (status, _, body) = app
        .post_token(
            "/api/channelmap/speaker-route",
            json!({
                "input": body["input_512"],
                "output": body["output_412"]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true, "{body}");
    let paths: Vec<(u64, u64)> = body["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|path| {
            (
                path["out_index"].as_u64().unwrap(),
                path["in_index"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        paths,
        vec![
            (0, 0),
            (1, 1),
            (2, 4),
            (3, 5),
            (4, 6),
            (5, 7),
            (7, 3)
        ]
    );
    assert!(body["edges"]
        .as_array()
        .unwrap()
        .iter()
        .any(|edge| edge["strategy"] == "center_downmix"));

    let (status, _, body) = app
        .post_token(
            "/api/channelmap/draft",
            json!({ "input_layout": "nsp-5.1.2", "output_layout": "4.1.2" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("channelmap"));

    let (status, _, body) = app
        .post_token(
            "/api/channelmap/draft",
            json!({ "input_layout": "h16-5.1.2", "output_layout": "4.1.2" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

fn base64_encode(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}
