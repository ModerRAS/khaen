//! M0-A 元信息端到端测试：`/healthz`、`/api/meta`、未知 `/api/*` 结构化 404。
//!
//! `/api/meta` 的断言直接对照 `khaen_host::control` 的真实能力常量，防止出现编造值。

mod support;

use khaen_host::control::{NOT_WIRED_OPS, NOT_WIRED_SURFACES, WIRED_OPS, WIRED_SURFACES};
use serde_json::Value;

#[tokio::test(flavor = "current_thread")]
async fn healthz_is_anonymous_and_reports_ok_version_uptime() {
    let server = support::spawn().await;
    let response = support::request(&server, "GET", "/healthz").await;
    assert_eq!(response.status, 200);
    assert_eq!(
        response.header("content-type"),
        Some("application/json"),
        "healthz 应返回 JSON"
    );
    let body = response.json();
    assert_eq!(body["ok"], Value::Bool(true));
    assert_eq!(
        body["version"],
        Value::String(env!("CARGO_PKG_VERSION").to_string())
    );
    let uptime = body["uptime_s"].as_u64().expect("uptime_s 必须是整数");
    assert!(uptime < 3600, "刚启动的进程 uptime_s 不应很大: {uptime}");
    // 字段集固定为 {ok, version, uptime_s}。
    assert_eq!(
        body.as_object().map(|object| object.len()),
        Some(3),
        "healthz 字段集必须是 {{ok,version,uptime_s}}: {body}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn api_meta_reports_real_version_and_capability_constants() {
    let server = support::spawn().await;
    let response = support::request(&server, "GET", "/api/meta").await;
    assert_eq!(response.status, 200);
    let body = response.json();

    // 版本：api_version 来自契约 B 协议版本常量；web_version 是 workspace 版本。
    assert_eq!(
        body["api_version"],
        Value::from(khaen_daemon::API_VERSION),
        "api_version 必须来自 khaen_host::PROTOCOL_VERSION"
    );
    assert_eq!(
        body["api_version"],
        Value::from(u32::from(khaen_host::PROTOCOL_VERSION))
    );
    assert_eq!(
        body["web_version"],
        Value::String(khaen_daemon::WEB_VERSION.to_string())
    );

    // 能力：与 /api/status 同源的真实常量，逐项一致。
    assert_eq!(body["ops"], serde_json::json!(WIRED_OPS));
    assert_eq!(body["surfaces"], serde_json::json!(WIRED_SURFACES));

    let not_wired = body["not_wired"].as_array().expect("not_wired 必须是数组");
    // 控制面能力（逐项照常量）+ 校准 job 后端（默认未接线，条目随实现状态变化）。
    assert_eq!(
        not_wired.len(),
        NOT_WIRED_OPS.len() + NOT_WIRED_SURFACES.len() + 1,
        "not_wired 必须完整反映未实现 op / 未接通 surface / 未接线后端: {not_wired:?}"
    );
    for ((op, reason), entry) in NOT_WIRED_OPS.iter().zip(not_wired.iter()) {
        assert_eq!(entry["op"], Value::String((*op).to_string()));
        assert_eq!(entry["reason"], Value::String((*reason).to_string()));
    }
    for ((surface, reason), entry) in NOT_WIRED_SURFACES
        .iter()
        .zip(not_wired.iter().skip(NOT_WIRED_OPS.len()))
    {
        assert_eq!(entry["surface"], Value::String((*surface).to_string()));
        assert_eq!(entry["reason"], Value::String((*reason).to_string()));
    }
    let calibration_entry = not_wired.last().expect("not_wired 非空");
    assert_eq!(
        calibration_entry["op"],
        Value::String("calibration.jobs".into())
    );
    assert!(
        calibration_entry["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "校准后端未接线必须带原因: {calibration_entry}"
    );
    // meta.calibration 如实反映当前注入的服务状态（默认 NotWired）。
    assert_eq!(body["calibration"]["wired"], Value::Bool(false));
    assert!(body["calibration"]["reason"]
        .as_str()
        .is_some_and(|reason| !reason.is_empty()));
    // meta.host_link：当前控制通道实况（T70f-b）。默认测试服务是 process 目标（进程内引擎），
    // 如实标 daemon-process + socket=null + 原因；能力清单 not_wired 语义不变（上面已逐项对齐）。
    assert_eq!(
        body["host_link"]["target"],
        Value::String("daemon-process".into())
    );
    assert_eq!(body["host_link"]["socket"], Value::Null);
    assert!(matches!(
        body["host_link"]["mode"].as_str(),
        Some("file") | Some("none")
    ));
    assert!(body["host_link"]["error"]
        .as_str()
        .is_some_and(|error| error.contains("KHAEN_RT_TARGET=process")));
}

#[test]
fn web_version_comes_from_the_webui_package_version() {
    let package_json =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../webui/package.json");
    match std::fs::read_to_string(&package_json) {
        Ok(text) => {
            let json: Value =
                serde_json::from_str(&text).expect("webui/package.json 必须是合法 JSON");
            let version = json["version"]
                .as_str()
                .expect("webui/package.json 必须有 version 字段");
            assert_eq!(
                khaen_daemon::WEB_VERSION,
                version,
                "meta.web_version 必须来自 webui/package.json 的真实 version"
            );
        }
        // webui/package.json 不存在时 build.rs 回退 workspace 版本；此处锁住回退语义。
        Err(_) => assert_eq!(khaen_daemon::WEB_VERSION, env!("CARGO_PKG_VERSION")),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_api_paths_keep_structured_json_404() {
    let server = support::spawn().await;
    for (method, path) in [
        ("GET", "/api/nope"),
        ("GET", "/api"),
        ("GET", "/api/device/nope"),
        ("POST", "/api/nope"),
        ("DELETE", "/api/nope"),
        ("GET", "/api/../Cargo.toml"),
    ] {
        let response = support::request(&server, method, path).await;
        assert_eq!(response.status, 404, "{method} {path}");
        assert_eq!(
            response.header("content-type"),
            Some("application/json"),
            "{method} {path} 必须是 JSON 而不是 SPA 页面"
        );
        let body = response.json();
        assert_eq!(body["ok"], Value::Bool(false), "{method} {path}");
        assert_eq!(
            body["error"],
            Value::String("not_found".into()),
            "{method} {path}"
        );
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|message| message.contains(path)),
            "{method} {path} 的 message 应包含路径: {body}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_non_api_non_get_paths_are_json_404() {
    let server = support::spawn().await;
    let response = support::request(&server, "POST", "/not-a-route").await;
    assert_eq!(response.status, 404);
    assert_eq!(response.header("content-type"), Some("application/json"));
    assert_eq!(response.json()["error"], Value::String("not_found".into()));
}

#[tokio::test(flavor = "current_thread")]
async fn existing_api_routes_survive_the_static_fallback() {
    let server = support::spawn().await;
    // /api/status 仍由原 handler 处理（静态 fallback 不能遮蔽已有 API 路由）。
    let response = support::request(&server, "GET", "/api/status").await;
    assert_eq!(response.status, 200);
    let body = response.json();
    assert_eq!(body["schema"], Value::String("khaen-status/v1".into()));
    assert_eq!(
        body["daemon"]["version"],
        Value::String(env!("CARGO_PKG_VERSION").into())
    );
    // /api/device 同样保留。
    let response = support::request(&server, "GET", "/api/device").await;
    assert_eq!(response.status, 200);
    assert_eq!(response.json()["wired"], Value::Bool(false));
}
