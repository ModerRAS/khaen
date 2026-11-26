//! T70f-b daemon → rt-host 控制通道代理的端到端测试（真实 TCP 打真实 axum 服务）。
//!
//! 覆盖：
//! - 每个控制 op 的转发形状（请求字段逐字、响应原样透传、既有 X-KHAEN-* / Cache-Control 头）；
//! - 服务端业务错误码 → HTTP 状态码的既有映射（透传错误体，不重试）；
//! - socket 不可用时诚实降级：503 + 稳定码 `host_link_unavailable`，且绝不作用于
//!   daemon 进程内引擎（不假成功）；
//! - 重试语义：连接失败可重试（含 commit）；超时只对非 commit 的 op 重试一次；
//! - 真实 TCP 回环上的行协议 framing + 超时降级（生产是 Unix socket，同一份编解码）；
//! - /api/status 的三来源（socket / file / none）与 `host_link.mode/target/age_ms`；
//! - /api/meta 的 host_link 能力实况；/api/device 优先用 rt-host 的 device 字段；
//! - process 目标的显式 `target: daemon-process` 标注；
//! - 转发 op 仍走 M1 认证/CSRF 守卫。

// 大号 rt-host status fixture（serde_json::json!）需要更深的宏递归。
#![recursion_limit = "256"]

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use khaen_daemon::auth::SESSION_COOKIE;
use khaen_daemon::host_link::{Target, HOST_LINK_UNAVAILABLE};
use serde_json::{json, Value};

use support::host_link_fake::{
    rt_status_document, DownTransport, FakeTransport, LoopbackAction, LoopbackServer,
};
use support::{TempStateDir, TestServer};

/// 断言 /api 响应带既有契约头（M1 语义不因转发而改变）。
fn assert_api_headers(response: &support::HttpResponse) {
    assert_eq!(
        response.header("content-type"),
        Some("application/json"),
        "{}",
        response.body_text()
    );
    assert_eq!(response.header("cache-control"), Some("no-store"));
    assert_eq!(
        response.header("x-khaen-api-version"),
        Some(khaen_daemon::API_VERSION.to_string().as_str())
    );
}

/// 匿名/无 CSRF 的原始请求头（M1 守卫测试用）。
fn cookie_only(server: &TestServer) -> Vec<(String, String)> {
    let session = server.session().expect("test server session");
    vec![(
        "Cookie".to_string(),
        format!("{SESSION_COOKIE}={}", session.token),
    )]
}

#[tokio::test(flavor = "current_thread")]
async fn forwards_every_control_op_to_rt_host_and_returns_its_response() {
    let fake = FakeTransport::new();
    let server = support::spawn_with_link(support::injected_link(Arc::clone(&fake)), None).await;

    // 1) mute
    let response = support::request_json(&server, "POST", "/api/mute", r#"{"mute":true}"#).await;
    assert_eq!(response.status, 200);
    assert_api_headers(&response);
    // docs/API.md §4：回执同时回传 mute（真实回读）与 muted 别名；其余字段原样透传。
    assert_eq!(
        response.json(),
        json!({ "ok": true, "mute": true, "muted": true, "applied_block": 11, "idempotent": false })
    );

    // 2) prepare
    let response = support::request_json(
        &server,
        "POST",
        "/api/prepare",
        r#"{"plan_path":"/tmp/plan.bin","plan_sha256":"aabbccdd"}"#,
    )
    .await;
    assert_eq!(response.status, 200);
    assert_api_headers(&response);
    assert_eq!(
        response.json(),
        json!({
            "ok": true,
            "engine_token": "0123456789abcdef",
            "planned_version": 2,
            "graph_id": "graph-2",
            "admission": { "accepted": true, "budget": {} },
        }),
        "prepare 响应必须原样透传（daemon 不增删字段）"
    );

    // 3) commit / 4) abort
    for (path, token) in [
        ("/api/commit", "0123456789abcdef"),
        ("/api/abort", "0123456789abcdef"),
    ] {
        let body = format!(r#"{{"engine_token":"{token}"}}"#);
        let response = support::request_json(&server, "POST", path, &body).await;
        assert_eq!(response.status, 200, "{path}: {}", response.body_text());
        assert_api_headers(&response);
        assert_eq!(response.json()["ok"], json!(true), "{path}");
    }

    // 5) coeff（GET）
    let response = support::request(&server, "GET", "/api/coeff").await;
    assert_eq!(response.status, 200);
    assert_api_headers(&response);
    assert_eq!(response.json()["version"], json!(2));
    assert_eq!(response.json()["graph_id"], json!("graph-2"));

    // 6) inspect
    let response = support::request_json(
        &server,
        "POST",
        "/api/plan/inspect",
        r#"{"plan_path":"/tmp/plan.bin"}"#,
    )
    .await;
    assert_eq!(response.status, 200);
    assert_api_headers(&response);
    assert_eq!(response.json()["staged"], json!(false));
    assert_eq!(response.json()["sha256"], json!("aabbccdd"));

    // 7) surface
    let response = support::request_json(
        &server,
        "POST",
        "/api/surface",
        r#"{"surface":"volume","data":{"gains_db":[]}}"#,
    )
    .await;
    assert_eq!(response.status, 200);
    assert_api_headers(&response);
    assert_eq!(response.json()["surface"], json!("volume"));
    assert_eq!(response.json()["engine_token"], json!("fedcba9876543210"));

    // 8) rollback（不带 target）
    let response = support::request_json(&server, "POST", "/api/rollback", "{}").await;
    assert_eq!(response.status, 200);
    assert_api_headers(&response);
    assert_eq!(response.json()["rolled_back_to"], json!(1));

    // 9) format
    let response = support::request_json(
        &server,
        "POST",
        "/api/format",
        r#"{"sample_rate":48000,"chunksize":128}"#,
    )
    .await;
    assert_eq!(response.status, 200);
    assert_api_headers(&response);
    assert_eq!(response.json()["engine_token"], json!("8899aabbccddeeff"));
    assert_eq!(response.json()["device_restart_required"], json!(true));

    // 请求逐字对照：daemon 转出去的正是 REST 里的字段（不增编 op/参数）。
    let calls = fake.calls();
    assert_eq!(calls.len(), 9, "9 个控制 op 各转发一次: {calls:?}");
    assert_eq!(calls[0], json!({ "op": "mute", "mute": true }));
    assert_eq!(
        calls[1],
        json!({ "op": "prepare", "plan_path": "/tmp/plan.bin", "plan_sha256": "aabbccdd" })
    );
    assert_eq!(
        calls[2],
        json!({ "op": "commit", "engine_token": "0123456789abcdef" })
    );
    assert_eq!(
        calls[3],
        json!({ "op": "abort", "engine_token": "0123456789abcdef" })
    );
    assert_eq!(calls[4], json!({ "op": "coeff" }));
    assert_eq!(
        calls[5],
        json!({ "op": "inspect", "plan_path": "/tmp/plan.bin" })
    );
    assert_eq!(
        calls[6],
        json!({ "op": "surface", "surface": "volume", "data": { "gains_db": [] } })
    );
    assert_eq!(calls[7], json!({ "op": "rollback" }));
    assert_eq!(
        calls[8],
        json!({ "op": "format", "sample_rate": 48000, "chunksize": 128 })
    );

    // rt-host 目标下 daemon 进程内引擎保持闲置：没有一个 op 落到进程内 ControlState。
    assert_eq!(server.state.control().status_json()["mute"], json!(false));
    assert_eq!(
        server.state.control().coeff_json()["version"],
        Value::Null,
        "转发模式不得悄悄使用进程内引擎"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn forwarded_server_errors_keep_the_contract_http_mapping() {
    let fake = FakeTransport::new();
    let server = support::spawn_with_link(support::injected_link(Arc::clone(&fake)), None).await;

    for (code, expected_status) in [
        ("bad_request", 400u16),
        ("unknown_op", 404),
        ("unsupported", 501),
        ("admission_rejected", 409),
        ("engine_token_invalid", 409),
        ("no_base_plan", 409),
        ("no_history", 409),
        ("not_applied", 500),
    ] {
        fake.push_error_reply(code, &format!("{code} from rt-host"));
        let before = fake.call_count();
        let response =
            support::request_json(&server, "POST", "/api/commit", r#"{"engine_token":"t"}"#).await;
        assert_eq!(response.status, expected_status, "{code}");
        assert_api_headers(&response);
        let body = response.json();
        assert_eq!(body["ok"], json!(false), "{code}");
        assert_eq!(body["error"], json!(code), "错误码必须原样透传: {body}");
        assert_eq!(body["message"], json!(format!("{code} from rt-host")));
        assert_eq!(
            fake.call_count() - before,
            1,
            "服务端业务错误是有效响应，不能重试: {code}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn control_ops_return_503_host_link_unavailable_when_the_socket_is_down() {
    let down = Arc::new(DownTransport);
    let server = support::spawn_with_link(support::injected_link(Arc::clone(&down)), None).await;

    let posts: [(&str, &str); 8] = [
        ("/api/mute", r#"{"mute":true}"#),
        (
            "/api/prepare",
            r#"{"plan_path":"/tmp/plan.bin","plan_sha256":"aabbccdd"}"#,
        ),
        ("/api/commit", r#"{"engine_token":"t"}"#),
        ("/api/abort", r#"{"engine_token":"t"}"#),
        ("/api/plan/inspect", r#"{"plan_path":"/tmp/plan.bin"}"#),
        (
            "/api/surface",
            r#"{"surface":"volume","data":{"gains_db":[]}}"#,
        ),
        ("/api/rollback", "{}"),
        ("/api/format", r#"{"sample_rate":48000}"#),
    ];
    for (path, body) in posts {
        let response = support::request_json(&server, "POST", path, body).await;
        assert_eq!(response.status, 503, "{path}: {}", response.body_text());
        assert_api_headers(&response);
        let json = response.json();
        assert_eq!(json["ok"], json!(false), "{path}");
        assert_eq!(json["error"], json!(HOST_LINK_UNAVAILABLE), "{path}");
        assert!(
            json["message"]
                .as_str()
                .is_some_and(|message| message.contains("unavailable")),
            "{path}: {json}"
        );
    }
    // coeff 是 GET，同样如实 503（不退回进程内引擎）。
    let response = support::request(&server, "GET", "/api/coeff").await;
    assert_eq!(response.status, 503);
    assert_api_headers(&response);
    assert_eq!(response.json()["error"], json!(HOST_LINK_UNAVAILABLE));

    // 诚实降级：进程内引擎没有被这些请求动过（不假成功）。
    assert_eq!(server.state.control().status_json()["mute"], json!(false));
    assert_eq!(server.state.control().coeff_json()["version"], Value::Null);
}

#[tokio::test(flavor = "current_thread")]
async fn mute_is_retried_once_after_a_timeout_but_commit_is_not() {
    let fake = FakeTransport::new();
    let server = support::spawn_with_link(support::injected_link(Arc::clone(&fake)), None).await;

    // 超时一次后重试成功：mute 是幂等的块边界请求，重试安全。
    fake.push_transport_error(khaen_daemon::host_link::TransportError::Timeout(
        "no response in time".to_string(),
    ));
    let response = support::request_json(&server, "POST", "/api/mute", r#"{"mute":true}"#).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert_eq!(response.json()["applied_block"], json!(11));
    assert_eq!(fake.call_count(), 2, "超时后必须重试一次（mute）");

    // commit 超时：第一次可能已经生效，重试会把成功报成失败，所以只发一次、如实 503。
    let before = fake.call_count();
    fake.push_transport_error(khaen_daemon::host_link::TransportError::Timeout(
        "block loop busy".to_string(),
    ));
    let response =
        support::request_json(&server, "POST", "/api/commit", r#"{"engine_token":"t"}"#).await;
    assert_eq!(response.status, 503);
    assert_eq!(response.json()["error"], json!(HOST_LINK_UNAVAILABLE));
    assert_eq!(
        fake.call_count() - before,
        1,
        "commit 超时不得重试（避免把已生效报成失败）"
    );
    assert!(
        response.json()["message"]
            .as_str()
            .is_some_and(|message| message.contains("1 attempt")),
        "{}",
        response.body_text()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn connect_failure_is_retried_even_for_commit() {
    let fake = FakeTransport::new();
    let server = support::spawn_with_link(support::injected_link(Arc::clone(&fake)), None).await;
    fake.push_transport_error(khaen_daemon::host_link::TransportError::Connect(
        "socket missing".to_string(),
    ));
    let response =
        support::request_json(&server, "POST", "/api/commit", r#"{"engine_token":"t"}"#).await;
    assert_eq!(response.status, 200, "连接失败请求肯定没送达，重试安全");
    assert_eq!(fake.call_count(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn real_tcp_loopback_round_trips_the_line_protocol() {
    let received: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let received_in_server = Arc::clone(&received);
    let server = LoopbackServer::start(move |_index, request| {
        received_in_server
            .lock()
            .expect("received")
            .push(request.clone());
        LoopbackAction::Reply(json!({
            "ok": true,
            "mute": request.get("mute").cloned().unwrap_or(json!(false)),
            "applied_block": 3,
            "idempotent": false,
        }))
    });
    let link = support::injected_link(Arc::new(server.transport()));
    let daemon = support::spawn_with_link(link, None).await;

    let response = support::request_json(&daemon, "POST", "/api/mute", r#"{"mute":true}"#).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert_eq!(response.json()["applied_block"], json!(3));
    assert_eq!(server.connections(), 1, "一次 op 一条短连接");

    // 一次 op 只转发一次（没有被超时误触发重试）。
    let calls = received.lock().expect("received").clone();
    assert_eq!(calls.len(), 1, "一次控制 op 只应产生一条请求: {calls:?}");
    assert_eq!(calls[0], json!({ "op": "mute", "mute": true }));
}

#[tokio::test(flavor = "current_thread")]
async fn real_tcp_loopback_timeout_degrades_status_and_ops_honestly() {
    let server = LoopbackServer::start(|_index, _request| {
        LoopbackAction::Delay(Duration::from_millis(350), json!({ "ok": true }))
    });
    // 显式 200ms 往返超时：服务端 350ms 后才回，客户端必然先超时。
    let link = support::injected_link_with_timeout(
        Arc::new(server.transport()),
        Duration::from_millis(200),
    );
    let daemon = support::spawn_with_link(link, None).await;

    let response = support::request(&daemon, "GET", "/api/status").await;
    assert_eq!(response.status, 200, "超时也只能降级，绝不 500");
    let body = response.json();
    assert_eq!(body["host_link"]["connected"], json!(false));
    assert_eq!(body["host_link"]["mode"], json!("none"));
    assert_eq!(body["host_link"]["target"], json!("rt-host"));
    let reason = body["reasons"]["host_link.connected"]
        .as_str()
        .expect("host_link.connected 原因");
    assert!(
        reason.contains("timeout"),
        "原因必须说明超时（如实、不编数值）: {reason}"
    );

    let response = support::request_json(&daemon, "POST", "/api/mute", r#"{"mute":true}"#).await;
    assert_eq!(response.status, 503, "{}", response.body_text());
    assert_eq!(response.json()["error"], json!(HOST_LINK_UNAVAILABLE));
}

/// 一份字段齐全的 rt-host 状态文件（schema 与 crates/rt-host/src/main.rs 同源）。
fn rt_status_file_document() -> Value {
    json!({
        "schema": "khaen-rt-status/v1",
        "blocks": 4321,
        "state": "running",
        "muted": false,
        "safe": false,
        "plan_rate": 48_000,
        "device_rate": 48_000,
        "rate_asserted": true,
        "input": { "packets": 99, "underruns": 1, "jitter_fill": 128, "ssrc": 123_456_789u64 },
        "output": { "xruns": 0, "blocks_written": 4000, "bytes_written": 33_792_000u64 },
        "receiver": { "packets_received": 100, "queue_full_drops": 2, "recv_errors": 0 },
        "graph_id": "0011223344556677",
        "last_error": null,
        "device_state": "present",
        "absent_since": null,
        "reopen_attempts": 0,
        "last_device_error": null,
    })
}

#[tokio::test(flavor = "current_thread")]
async fn status_reports_socket_mode_from_rt_host_status() {
    let fake = FakeTransport::with_rt_status(rt_status_document());
    let server = support::spawn_with_link(support::injected_link(Arc::clone(&fake)), None).await;

    let response = support::request(&server, "GET", "/api/status").await;
    assert_eq!(response.status, 200);
    assert_api_headers(&response);
    let body = response.json();
    assert_eq!(body["schema"], json!("khaen-status/v1"));
    assert_eq!(body["daemon"]["version"], json!(env!("CARGO_PKG_VERSION")));

    assert_eq!(body["host_link"]["connected"], json!(true));
    assert_eq!(body["host_link"]["mode"], json!("socket"));
    assert_eq!(body["host_link"]["target"], json!("rt-host"));
    assert_eq!(
        body["host_link"]["socket"],
        json!("/tmp/khaen-test-rt-host.sock")
    );
    assert_eq!(body["host_link"]["age_ms"], json!(42));
    assert_eq!(body["host_link"]["status_age_ms"], json!(42));

    // rt-host 自己报的 state/plan/format/device/counters 全部映射过来。
    assert_eq!(body["state"]["value"], json!("running"));
    assert_eq!(body["plan"]["graph_id"], json!("cafebabe0011"));
    assert_eq!(
        body["plan"]["version"],
        json!(7),
        "socket 模式的 plan.version 来自 rt-host actual_version"
    );
    assert_eq!(body["format"]["block_frames"], json!(256));
    assert_eq!(body["format"]["channels"], json!(24));
    assert_eq!(body["format"]["plan_rate"], json!(48_000));
    assert_eq!(body["input"]["packets"], json!(111));
    assert_eq!(body["output"]["blocks"], json!(4321));
    assert_eq!(body["output"]["out_errors"], json!(5));
    assert_eq!(body["device"]["state"], json!("present"));
    assert_eq!(body["device"]["rate_readback"], json!(48_000));

    // 控制通道接通后 not_wired 不再列 host_link.control。
    let not_wired = body["not_wired"].as_array().expect("not_wired");
    assert!(not_wired
        .iter()
        .all(|entry| entry["path"] != json!("host_link.control")));
}

#[tokio::test(flavor = "current_thread")]
async fn status_falls_back_to_the_file_with_mode_file_and_the_socket_path() {
    let dir = TempStateDir::new("host-link-file");
    let status_file = dir.path().join("rt-host-status.json");
    std::fs::write(&status_file, rt_status_file_document().to_string()).expect("write status file");
    let down = Arc::new(DownTransport);
    let mut server = support::spawn_at_with_link(
        dir.path(),
        support::injected_link(Arc::clone(&down)),
        Some(status_file),
    )
    .await;
    server.sign_in().await;

    let response = support::request(&server, "GET", "/api/status").await;
    assert_eq!(response.status, 200);
    let body = response.json();
    assert_eq!(body["host_link"]["mode"], json!("file"));
    assert_eq!(body["host_link"]["connected"], json!(true));
    assert_eq!(body["host_link"]["target"], json!("rt-host"));
    assert_eq!(
        body["host_link"]["socket"],
        json!("/tmp/khaen-test-rt-host.sock"),
        "socket 不可用时仍如实上报配置路径"
    );
    assert_eq!(body["state"]["value"], json!("running"));
    assert_eq!(body["output"]["blocks"], json!(4321));
    let socket_reason = body["reasons"]["host_link.socket"]
        .as_str()
        .expect("host_link.socket 原因");
    assert!(socket_reason.contains("unavailable"), "{socket_reason}");
    assert!(socket_reason.contains("mode=file"), "{socket_reason}");
    let not_wired = body["not_wired"].as_array().expect("not_wired");
    assert!(
        not_wired
            .iter()
            .any(|entry| entry["path"] == json!("host_link.control")),
        "socket 不可用时控制通道必须进 not_wired: {not_wired:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn status_reports_mode_none_when_both_sources_are_unavailable() {
    let down = Arc::new(DownTransport);
    let server = support::spawn_with_link(support::injected_link(Arc::clone(&down)), None).await;

    let response = support::request(&server, "GET", "/api/status").await;
    assert_eq!(response.status, 200);
    let body = response.json();
    assert_eq!(body["host_link"]["mode"], json!("none"));
    assert_eq!(body["host_link"]["connected"], json!(false));
    assert_eq!(
        body["host_link"]["socket"],
        json!("/tmp/khaen-test-rt-host.sock")
    );
    assert!(body["reasons"]["host_link.connected"].is_string());
    assert_eq!(body["state"]["value"], Value::Null);
    assert_eq!(body["daemon"]["version"], json!(env!("CARGO_PKG_VERSION")));
}

#[tokio::test(flavor = "current_thread")]
async fn process_target_marks_responses_and_status() {
    // support::spawn 默认注入 process 目标（仅测试/离线演示）。
    let server = support::spawn().await;

    let response = support::request_json(&server, "POST", "/api/mute", r#"{"mute":true}"#).await;
    assert_eq!(response.status, 200);
    assert_api_headers(&response);
    let body = response.json();
    assert_eq!(body["target"], json!(Target::Process.as_str()));
    assert_eq!(body["mute"], json!(true), "process 模式仍作用于进程内引擎");

    let response = support::request(&server, "GET", "/api/coeff").await;
    assert_eq!(response.status, 200);
    assert_eq!(response.json()["target"], json!("daemon-process"));

    let response = support::request(&server, "GET", "/api/status").await;
    assert_eq!(response.status, 200);
    let body = response.json();
    assert_eq!(body["host_link"]["target"], json!("daemon-process"));
    assert_eq!(body["host_link"]["socket"], Value::Null);
    assert!(matches!(
        body["host_link"]["mode"].as_str(),
        Some("file") | Some("none")
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn meta_reports_the_live_host_link_capability() {
    // rt-host 目标 + socket 可用：meta 报 mode=socket/connected=true。
    let fake = FakeTransport::with_rt_status(rt_status_document());
    let server = support::spawn_with_link(support::injected_link(Arc::clone(&fake)), None).await;
    let response = support::request(&server, "GET", "/api/meta").await;
    assert_eq!(response.status, 200);
    let meta = response.json();
    assert_eq!(meta["host_link"]["target"], json!("rt-host"));
    assert_eq!(meta["host_link"]["mode"], json!("socket"));
    assert_eq!(meta["host_link"]["connected"], json!(true));
    assert_eq!(
        meta["host_link"]["socket"],
        json!("/tmp/khaen-test-rt-host.sock")
    );
    assert_eq!(meta["host_link"]["error"], Value::Null);
    // 能力清单语义不变（仍逐项来自契约常量 + 校准后端）。
    assert_eq!(
        meta["not_wired"].as_array().map(Vec::len),
        Some(
            khaen_host::control::NOT_WIRED_OPS.len()
                + khaen_host::control::NOT_WIRED_SURFACES.len()
                + 1
        )
    );

    // process 目标：meta 报 daemon-process / socket null / 原因。
    let server = support::spawn().await;
    let meta = support::request(&server, "GET", "/api/meta").await.json();
    assert_eq!(meta["host_link"]["target"], json!("daemon-process"));
    assert_eq!(meta["host_link"]["socket"], Value::Null);
    assert!(meta["host_link"]["error"]
        .as_str()
        .is_some_and(|error| error.contains("KHAEN_RT_TARGET=process")));
}

#[tokio::test(flavor = "current_thread")]
async fn device_prefers_rt_host_device_fields_when_the_socket_is_live() {
    let fake = FakeTransport::with_rt_status(rt_status_document());
    let server = support::spawn_with_link(support::injected_link(Arc::clone(&fake)), None).await;
    let response = support::request(&server, "GET", "/api/device").await;
    assert_eq!(response.status, 200);
    let body = response.json();
    assert_eq!(body["source"], json!("rt-host"));
    assert_eq!(body["wired"], json!(true));
    assert_eq!(body["state"], json!("streaming"));
    assert_eq!(body["plan_rate"], json!(48_000));
    assert_eq!(body["device_rate"], json!(48_000));
    assert_eq!(body["rate_asserted"], json!(true));
    assert_eq!(body["device_state"], json!("present"));

    // socket 不可用：退回 daemon 监督器快照并带上真实原因（不冒充 rt-host）。
    let down = Arc::new(DownTransport);
    let server = support::spawn_with_link(support::injected_link(Arc::clone(&down)), None).await;
    let body = support::request(&server, "GET", "/api/device").await.json();
    assert_eq!(body["source"], json!("daemon-monitor"));
    assert_eq!(body["wired"], json!(false));
    assert!(body["rt_host_unavailable"]
        .as_str()
        .is_some_and(|reason| reason.contains("unavailable")));
}

#[tokio::test(flavor = "current_thread")]
async fn forwarded_ops_still_require_session_and_csrf() {
    let fake = FakeTransport::new();
    let server = support::spawn_with_link(support::injected_link(Arc::clone(&fake)), None).await;

    // 匿名：401（M1 守卫仍然包住转发路由）。
    let response =
        support::request_with(&server, "POST", "/api/mute", &[], Some(r#"{"mute":true}"#)).await;
    assert_eq!(response.status, 401, "{}", response.body_text());
    // M1 认证错误体形状（auth::code::Error）：{"error":{code,message,details}}
    assert_eq!(response.json()["error"]["code"], json!("unauthorized"));
    // GET 控制 op 也要求会话。
    let response = support::request_with(&server, "GET", "/api/coeff", &[], None).await;
    assert_eq!(response.status, 401);

    // 有 Cookie 但缺 CSRF：403，且没有转发出去。
    let before = fake.call_count();
    let response = support::request_with(
        &server,
        "POST",
        "/api/mute",
        &cookie_only(&server),
        Some(r#"{"mute":true}"#),
    )
    .await;
    assert_eq!(response.status, 403, "{}", response.body_text());
    assert_eq!(response.json()["error"]["code"], json!("forbidden"));
    assert_eq!(fake.call_count(), before, "被守卫拦下的请求不得转发");

    // 补齐 CSRF 后正常转发。
    let response = support::request_json(&server, "POST", "/api/mute", r#"{"mute":true}"#).await;
    assert_eq!(response.status, 200);
    assert_eq!(fake.call_count(), before + 1);
}
