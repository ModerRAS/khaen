//! T46 系统页后端端到端测试：真实 TCP 打真实 axum 服务，系统采集用注入的假 SystemProbe。
//!
//! 覆盖：/healthz release 版本、/api/system/info 的三口径/服务/设备/host_link/restart/reasons、
//! 采集失败全 null + 原因、/api/system/logs 的参数校验与 503 logs_unavailable、
//! /api/system/diagnostics 的文件名/无敏感字段、/api/system/password 的 auth 语义（旧密码错 401、
//! 新密码短 400 invalid_password、成功后旧会话失效）、/api/system/restart 的开关/403/501/成功回执、
//! 以及认证与 CSRF 守卫。
//!
//! 真机 systemctl/journalctl 语义属 NOT_RUN（Windows 本机没有 systemd）；NativeProbe 的
//! 命令映射与 fake /proc 解析由 system/probe.rs 的单测覆盖。

mod support;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use khaen_daemon::auth::SESSION_COOKIE;
use khaen_daemon::system::{BuildInfo, DeviceInfo, RestartPolicy, ServiceSnapshot, SystemProbe};
use serde_json::Value;

use support::{spawn_at_with_system, spawn_with_system, TestServer, TEST_PASSWORD};

/// 假系统采集：所有字段可注入，记录重启调用。
#[derive(Debug)]
struct FakeProbe {
    hostname: Result<String, String>,
    kernel: Result<String, String>,
    os: Result<String, String>,
    devices: Result<Vec<DeviceInfo>, String>,
    services: HashMap<String, ServiceSnapshot>,
    logs: HashMap<String, Result<Vec<String>, String>>,
    restarts: Mutex<Vec<String>>,
    restart_result: Result<(), String>,
}

impl FakeProbe {
    fn healthy() -> Self {
        let mut services = HashMap::new();
        services.insert("khaen-daemon".to_string(), good_service(4211));
        services.insert("khaen-rt-host".to_string(), good_service(4247));
        let mut logs = HashMap::new();
        logs.insert(
            "khaen-daemon".to_string(),
            Ok(vec![
                "Nov 26 06:00:00 AoipDSP khaen-daemon[4211]: starting".to_string(),
                "Nov 26 06:00:01 AoipDSP khaen-daemon[4211]: listening".to_string(),
            ]),
        );
        logs.insert(
            "khaen-rt-host".to_string(),
            Ok(vec![
                "Nov 26 06:00:02 AoipDSP khaen-rt-host[4247]: running".to_string()
            ]),
        );
        Self {
            hostname: Ok("AoipDSP".to_string()),
            kernel: Ok("6.8.0-45-generic".to_string()),
            os: Ok("Debian GNU/Linux 12 (bookworm)".to_string()),
            devices: Ok(vec![DeviceInfo {
                card_id: Some("UltraLitemk5".to_string()),
                driver: Some("USB-Audio".to_string()),
                model: Some("MOTU UltraLite mk5".to_string()),
                channels: Some(22),
                rates: Some(vec![44_100, 48_000, 88_200, 96_000]),
                reasons: Vec::new(),
            }]),
            services,
            logs,
            restarts: Mutex::new(Vec::new()),
            restart_result: Ok(()),
        }
    }

    fn degraded() -> Self {
        Self {
            hostname: Err("/proc/sys/kernel/hostname is not readable".to_string()),
            kernel: Err("/proc/version is not readable".to_string()),
            os: Err("/etc/os-release is not readable".to_string()),
            devices: Err("/proc/asound/cards is not readable".to_string()),
            services: HashMap::new(),
            logs: HashMap::new(),
            restarts: Mutex::new(Vec::new()),
            restart_result: Err("systemctl restart failed".to_string()),
        }
    }

    fn restarts(&self) -> Vec<String> {
        self.restarts.lock().expect("restarts").clone()
    }
}

impl SystemProbe for FakeProbe {
    fn hostname(&self) -> Result<String, String> {
        self.hostname.clone()
    }

    fn kernel(&self) -> Result<String, String> {
        self.kernel.clone()
    }

    fn os(&self) -> Result<String, String> {
        self.os.clone()
    }

    fn devices(&self) -> Result<Vec<DeviceInfo>, String> {
        self.devices.clone()
    }

    fn service(&self, unit: &str) -> ServiceSnapshot {
        // 没有预设的 unit：如实报"systemctl 没有这个信息"（三字段 None + 原因）。
        self.services
            .get(unit)
            .cloned()
            .unwrap_or_else(|| ServiceSnapshot {
                active: None,
                enabled: None,
                pid: None,
                reasons: vec![(
                    "active",
                    "systemctl is unavailable in this test".to_string(),
                )],
            })
    }

    fn logs(&self, unit: &str, lines: usize) -> Result<Vec<String>, String> {
        let _ = lines;
        self.logs.get(unit).cloned().unwrap_or_else(|| {
            Err("journalctl is not available on this host (systemd not detected)".to_string())
        })
    }

    fn restart_service(&self, unit: &str) -> Result<(), String> {
        self.restarts
            .lock()
            .expect("restarts")
            .push(unit.to_string());
        self.restart_result.clone()
    }
}

fn good_service(pid: u32) -> ServiceSnapshot {
    ServiceSnapshot {
        active: Some(true),
        enabled: Some(true),
        pid: Some(pid),
        reasons: Vec::new(),
    }
}

fn build_info(release: Option<&str>) -> BuildInfo {
    let mut reasons = std::collections::BTreeMap::new();
    if release.is_none() {
        reasons.insert(
            "release_version".to_string(),
            "KHAEN_BUILD_VERSION is not set at build time: this is an unversioned build"
                .to_string(),
        );
    }
    BuildInfo {
        crate_version: "0.1.0".to_string(),
        release_version: release.map(str::to_string),
        commit: Some("855f4c4".to_string()),
        commit_source: Some("git rev-parse HEAD at build time".to_string()),
        rustc: Some("rustc 1.89.0 (29483883e 2025-08-04)".to_string()),
        build_profile: "release".to_string(),
        target: "x86_64-unknown-linux-gnu".to_string(),
        reasons,
    }
}

async fn spawn_healthy() -> TestServer {
    spawn_with_system(
        Arc::new(FakeProbe::healthy()),
        build_info(Some("v0.1.2")),
        RestartPolicy::disabled("KHAEN_ALLOW_RESTART is not set: restarting rt-host is disabled"),
    )
    .await
}

fn cookie(token: &str) -> (String, String) {
    ("Cookie".to_string(), format!("{SESSION_COOKIE}={token}"))
}

// ---------------------------------------------------------------------------
// /healthz（匿名；version = release 版本）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn healthz_is_anonymous_and_reports_the_release_version() {
    let server = spawn_at_with_system(
        &std::env::temp_dir().join(format!("khaen-system-healthz-{}", std::process::id())),
        Arc::new(FakeProbe::healthy()),
        build_info(Some("v0.1.2")),
        RestartPolicy::disabled("test"),
    )
    .await;
    let response = support::request_with(&server, "GET", "/healthz", &[], None).await;
    assert_eq!(response.status, 200);
    let body = response.json();
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(body["version"], Value::String("v0.1.2".to_string()));
    assert!(body["uptime_s"].as_u64().is_some());
    assert_eq!(body.as_object().map(|object| object.len()), Some(3));
}

// ---------------------------------------------------------------------------
// /api/system/info
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn info_reports_three_versions_services_devices_and_restart_capability() {
    let server = spawn_healthy().await;
    let response = support::request(&server, "GET", "/api/system/info").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["schema"], Value::String("khaen-system-info/v1".into()));
    // 版本三口径：crate 0.1.0 != release v0.1.2，页面据此提示（不合并成一个版本）。
    assert_eq!(body["crate_version"], Value::String("0.1.0".into()));
    assert_eq!(body["release_version"], Value::String("v0.1.2".into()));
    assert_eq!(body["commit"], Value::String("855f4c4".into()));
    assert_eq!(body["build_profile"], Value::String("release".into()));
    assert!(body["rustc"]
        .as_str()
        .is_some_and(|value| value.starts_with("rustc")));
    assert_eq!(
        body["target"],
        Value::String("x86_64-unknown-linux-gnu".into())
    );
    // daemon 自身真实字段。
    assert!(body["bind"].as_str().is_some());
    assert!(body["state_dir"].as_str().is_some());
    assert!(body["rt_status_file"].as_str().is_some());
    assert!(body["uptime_s"].as_u64().is_some());
    // 主机信息来自注入的采集器。
    assert_eq!(body["hostname"], Value::String("AoipDSP".into()));
    assert_eq!(body["kernel"], Value::String("6.8.0-45-generic".into()));
    assert_eq!(
        body["os"],
        Value::String("Debian GNU/Linux 12 (bookworm)".into())
    );
    // 服务：active/enabled/pid 全在；daemon 与 rt_host 键（前端兼容 rt-host 写法）。
    assert_eq!(body["services"]["daemon"]["active"], Value::Bool(true));
    assert_eq!(body["services"]["daemon"]["enabled"], Value::Bool(true));
    assert_eq!(body["services"]["daemon"]["pid"], Value::from(4211));
    assert!(body["services"]["daemon"]["reason"].is_null());
    assert_eq!(body["services"]["rt_host"]["pid"], Value::from(4247));
    // 设备：字段与来源一致（MOTU 由前端按 card_id/model 标出）。
    let devices = body["devices"].as_array().expect("devices array");
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0]["card_id"], Value::String("UltraLitemk5".into()));
    assert_eq!(devices[0]["driver"], Value::String("USB-Audio".into()));
    assert_eq!(devices[0]["channels"], Value::from(22));
    assert_eq!(
        devices[0]["rates"],
        serde_json::json!([44_100, 48_000, 88_200, 96_000])
    );
    // host_link：测试服务是 process 目标 + 没有 rt-host 状态文件 -> 如实断开 + 原因。
    assert_eq!(body["host_link"]["connected"], Value::Bool(false));
    assert!(body["reasons"]["host_link.connected"].as_str().is_some());
    // 重启能力：默认未启用（fail-closed），target 恒为 rt-host。
    assert_eq!(body["restart"]["allowed"], Value::Bool(false));
    assert_eq!(body["restart"]["target"], Value::String("rt-host".into()));
    assert!(body["restart"]["reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("KHAEN_ALLOW_RESTART")));
}

#[tokio::test(flavor = "current_thread")]
async fn info_degrades_to_null_with_reasons_when_collectors_fail() {
    let server = spawn_with_system(
        Arc::new(FakeProbe::degraded()),
        build_info(None),
        RestartPolicy::disabled("KHAEN_ALLOW_RESTART is not set: restarting rt-host is disabled"),
    )
    .await;
    let response = support::request(&server, "GET", "/api/system/info").await;
    assert_eq!(
        response.status,
        200,
        "采集失败不能 500: {}",
        response.body_text()
    );
    let body = response.json();
    // release 版本缺失：null + 原因；crate 版本仍是真实值。
    assert!(body["release_version"].is_null());
    assert!(body["reasons"]["release_version"]
        .as_str()
        .is_some_and(|reason| reason.contains("KHAEN_BUILD_VERSION")));
    assert_eq!(body["crate_version"], Value::String("0.1.0".into()));
    for field in ["hostname", "kernel", "os"] {
        assert!(body[field].is_null(), "{field} 必须为 null: {body}");
        assert!(
            body["reasons"][field].as_str().is_some(),
            "{field} 必须有原因"
        );
    }
    assert_eq!(body["devices"], serde_json::json!([]));
    assert!(body["reasons"]["devices"].as_str().is_some());
    for (unit, key) in [("daemon", "daemon"), ("rt_host", "rt_host")] {
        let _ = unit;
        for field in ["active", "enabled", "pid"] {
            assert!(body["services"][key][field].is_null(), "{key}.{field}");
            assert!(
                body["reasons"][format!("services.{key}.{field}")]
                    .as_str()
                    .is_some(),
                "services.{key}.{field} 必须有原因"
            );
        }
        assert!(body["services"][key]["reason"].as_str().is_some());
    }
    assert!(body["restart"]["allowed"] == Value::Bool(false));
    assert!(body["reasons"]["restart"].as_str().is_some());
}

// ---------------------------------------------------------------------------
// /api/system/logs
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn logs_returns_journal_lines_for_the_requested_unit() {
    let server = spawn_healthy().await;
    let response = support::request(&server, "GET", "/api/system/logs?unit=daemon&lines=2").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["schema"], Value::String("khaen-system-logs/v1".into()));
    assert_eq!(body["unit"], Value::String("daemon".into()));
    assert_eq!(body["unit_name"], Value::String("khaen-daemon".into()));
    assert_eq!(body["requested_lines"], Value::from(2));
    assert_eq!(body["lines"].as_array().map(Vec::len), Some(2));
    assert_eq!(body["source"], Value::String("journalctl".into()));
    assert!(body["reason"].is_null());
    assert_eq!(body["truncated"], Value::Bool(false));

    let response =
        support::request(&server, "GET", "/api/system/logs?unit=rt-host&lines=100").await;
    assert_eq!(response.status, 200);
    let body = response.json();
    assert_eq!(body["unit"], Value::String("rt-host".into()));
    assert_eq!(body["unit_name"], Value::String("khaen-rt-host".into()));
    assert_eq!(body["lines"].as_array().map(Vec::len), Some(1));
}

#[tokio::test(flavor = "current_thread")]
async fn logs_returns_503_logs_unavailable_when_journalctl_is_missing() {
    let server = spawn_with_system(
        Arc::new(FakeProbe::degraded()),
        build_info(Some("v0.1.2")),
        RestartPolicy::disabled("test"),
    )
    .await;
    let response = support::request(&server, "GET", "/api/system/logs?unit=daemon&lines=200").await;
    assert_eq!(response.status, 503, "{}", response.body_text());
    let body = response.json();
    assert_eq!(
        body["error"]["code"],
        Value::String("logs_unavailable".into())
    );
    assert!(body["error"]["details"]["reason"]
        .as_str()
        .is_some_and(|reason| !reason.is_empty()));
}

#[tokio::test(flavor = "current_thread")]
async fn logs_rejects_invalid_parameters_with_400_invalid_request() {
    let server = spawn_healthy().await;
    for path in [
        "/api/system/logs?unit=bogus&lines=200",
        "/api/system/logs?unit=daemon&lines=501",
        "/api/system/logs?unit=daemon&lines=0",
        "/api/system/logs?unit=daemon&lines=abc",
    ] {
        let response = support::request(&server, "GET", path).await;
        assert_eq!(response.status, 400, "{path}: {}", response.body_text());
        assert_eq!(
            response.json()["error"]["code"],
            Value::String("invalid_request".into()),
            "{path}"
        );
    }
}

// ---------------------------------------------------------------------------
// /api/system/diagnostics
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn diagnostics_download_has_filename_and_no_secrets() {
    let server = spawn_healthy().await;
    let session = server.session().expect("session").clone();
    let response = support::request(&server, "GET", "/api/system/diagnostics").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert_eq!(response.header("content-type"), Some("application/json"));
    let disposition = response
        .header("content-disposition")
        .expect("content-disposition");
    assert!(
        disposition.starts_with("attachment; filename=\"khaen-diagnostics-v0.1.2-"),
        "{disposition}"
    );
    assert!(disposition.ends_with(".json\""), "{disposition}");
    let text = response.body_text();
    let body: Value = serde_json::from_str(&text).expect("diagnostics must be valid JSON");
    assert_eq!(body["schema"], Value::String("khaen-diagnostics/v1".into()));
    assert!(body["generated_at"].as_str().is_some());
    assert_eq!(body["system"]["hostname"], Value::String("AoipDSP".into()));
    assert_eq!(
        body["status"]["schema"],
        Value::String("khaen-status/v1".into())
    );
    assert_eq!(body["logs"]["daemon"]["available"], Value::Bool(true));
    assert_eq!(
        body["logs"]["daemon"]["lines"].as_array().map(Vec::len),
        Some(2)
    );
    // 敏感字段绝不出现在诊断包里。
    assert!(
        !text.contains(TEST_PASSWORD),
        "diagnostics must not leak the password"
    );
    assert!(
        !text.contains(&session.token),
        "diagnostics must not leak the session token"
    );
    assert!(
        !text.contains(&session.csrf),
        "diagnostics must not leak the CSRF token"
    );
}

// ---------------------------------------------------------------------------
// /api/system/password
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn password_change_reuses_auth_semantics_and_rotates_the_session() {
    let server = spawn_healthy().await;
    let old = server.session().expect("session").clone();

    // 旧密码错：401 invalid_credentials（不是会话问题）。
    let response = support::request_json(
        &server,
        "POST",
        "/api/system/password",
        &serde_json::json!({"old": "wrong-password", "new": "khaen-new-password"}).to_string(),
    )
    .await;
    assert_eq!(response.status, 401, "{}", response.body_text());
    assert_eq!(
        response.json()["error"]["code"],
        Value::String("invalid_credentials".into())
    );

    // 新密码短：400 invalid_password（T47 契约的稳定码）。
    let response = support::request_json(
        &server,
        "POST",
        "/api/system/password",
        &serde_json::json!({"old": TEST_PASSWORD, "new": "short"}).to_string(),
    )
    .await;
    assert_eq!(response.status, 400, "{}", response.body_text());
    assert_eq!(
        response.json()["error"]["code"],
        Value::String("invalid_password".into())
    );

    // 成功：下发新会话（新 Cookie/CSRF），旧会话立即失效。
    let response = support::request_json(
        &server,
        "POST",
        "/api/system/password",
        &serde_json::json!({"old": TEST_PASSWORD, "new": "khaen-new-password"}).to_string(),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let new_csrf = response.json()["csrf"]
        .as_str()
        .expect("new csrf")
        .to_string();
    let new_token = response.session_token().expect("new cookie");

    let headers = [cookie(&old.token)];
    let response = support::request_with(&server, "GET", "/api/system/info", &headers, None).await;
    assert_eq!(
        response.status,
        401,
        "旧会话必须失效: {}",
        response.body_text()
    );

    let headers = [cookie(&new_token), ("X-KHAEN-CSRF".to_string(), new_csrf)];
    let response = support::request_with(&server, "GET", "/api/system/info", &headers, None).await;
    assert_eq!(
        response.status,
        200,
        "新会话必须可用: {}",
        response.body_text()
    );
}

// ---------------------------------------------------------------------------
// /api/system/restart
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn restart_is_disabled_by_default_and_daemon_self_restart_is_501() {
    let server = spawn_healthy().await;
    let response = support::request_json(
        &server,
        "POST",
        "/api/system/restart",
        &serde_json::json!({"target": "rt-host"}).to_string(),
    )
    .await;
    assert_eq!(response.status, 403, "{}", response.body_text());
    let body = response.json();
    assert_eq!(
        body["error"]["code"],
        Value::String("restart_disabled".into())
    );
    assert!(body["error"]["details"]["reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("KHAEN_ALLOW_RESTART")));

    let response = support::request_json(
        &server,
        "POST",
        "/api/system/restart",
        &serde_json::json!({"target": "daemon"}).to_string(),
    )
    .await;
    assert_eq!(response.status, 501, "{}", response.body_text());
    assert_eq!(
        response.json()["error"]["code"],
        Value::String("unsupported".into())
    );
    // 未知目标：501 unsupported（合同 §8：只实现 rt-host，其它如实未实现）。
    let response = support::request_json(
        &server,
        "POST",
        "/api/system/restart",
        &serde_json::json!({"target": "bogus"}).to_string(),
    )
    .await;
    assert_eq!(response.status, 501);
    assert_eq!(
        response.json()["error"]["code"],
        Value::String("unsupported".into())
    );
}

#[tokio::test(flavor = "current_thread")]
async fn restart_restarts_rt_host_when_enabled_and_reports_the_receipt() {
    let probe = Arc::new(FakeProbe::healthy());
    let injected: Arc<dyn SystemProbe> = probe.clone();
    let server = spawn_with_system(
        injected,
        build_info(Some("v0.1.2")),
        RestartPolicy::enabled(),
    )
    .await;
    // 启用后 info 如实 allowed=true。
    let response = support::request(&server, "GET", "/api/system/info").await;
    assert_eq!(response.json()["restart"]["allowed"], Value::Bool(true));

    let response = support::request_json(
        &server,
        "POST",
        "/api/system/restart",
        &serde_json::json!({"target": "rt-host"}).to_string(),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(body["restarted"], Value::Bool(true));
    assert_eq!(body["target"], Value::String("rt-host".into()));
    assert!(body["restarted_at"].as_str().is_some());
    assert_eq!(probe.restarts(), vec!["khaen-rt-host".to_string()]);

    // info.restart.last_restarted_at 记录本次真实重启。
    let response = support::request(&server, "GET", "/api/system/info").await;
    assert!(response.json()["restart"]["last_restarted_at"]
        .as_str()
        .is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn restart_failure_is_503_restart_failed_with_reason() {
    let mut probe = FakeProbe::healthy();
    probe.restart_result = Err("Job for khaen-rt-host.service failed".to_string());
    let server = spawn_with_system(
        Arc::new(probe),
        build_info(Some("v0.1.2")),
        RestartPolicy::enabled(),
    )
    .await;
    let response = support::request_json(
        &server,
        "POST",
        "/api/system/restart",
        &serde_json::json!({"target": "rt-host"}).to_string(),
    )
    .await;
    assert_eq!(response.status, 503, "{}", response.body_text());
    let body = response.json();
    assert_eq!(
        body["error"]["code"],
        Value::String("restart_failed".into())
    );
    assert!(body["error"]["details"]["reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("failed")));
}

// ---------------------------------------------------------------------------
// 认证 / CSRF 守卫
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn system_endpoints_require_a_session_and_csrf() {
    let state_dir = std::env::temp_dir().join(format!("khaen-system-auth-{}", std::process::id()));
    let server = spawn_at_with_system(
        &state_dir,
        Arc::new(FakeProbe::healthy()),
        build_info(Some("v0.1.2")),
        RestartPolicy::enabled(),
    )
    .await;
    for (method, path) in [
        ("GET", "/api/system/info"),
        ("GET", "/api/system/logs?unit=daemon&lines=10"),
        ("GET", "/api/system/diagnostics"),
        ("POST", "/api/system/restart"),
        ("POST", "/api/system/password"),
    ] {
        let response = support::request_with(&server, method, path, &[], None).await;
        assert_eq!(
            response.status,
            401,
            "{method} {path}: {}",
            response.body_text()
        );
    }
    // 有会话但写方法缺 CSRF：403。
    let mut server = server;
    server.sign_in().await;
    let session = server.session().expect("session").clone();
    let headers = [cookie(&session.token)];
    let response = support::request_with(
        &server,
        "POST",
        "/api/system/restart",
        &headers,
        Some(r#"{"target":"rt-host"}"#),
    )
    .await;
    assert_eq!(response.status, 403, "{}", response.body_text());
    assert_eq!(
        response.json()["error"]["code"],
        Value::String("forbidden".into())
    );
}
