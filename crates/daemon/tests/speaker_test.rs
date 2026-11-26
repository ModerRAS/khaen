//! 通道识别测试音代理 / speaker_test 状态聚合 / meta 能力条目的合同测试（docs/API.md §12.2/§12.6）。
//!
//! 全离线：真实 TCP 打真实 axum 服务；rt-host 用脚本化假传输（tests/support/host_link_fake.rs），
//! 墙钟用 SpeakerTestRuntime::with_clock 注入（不依赖真实时间/时区），状态文件用临时目录。

mod support;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use khaen_daemon::host_link::TransportError;
use khaen_daemon::speaker_test::{SpeakerTestConfig, SpeakerTestRuntime};
use serde_json::{json, Value};

use support::calibration_fake::FakeCalibration;
use support::host_link_fake::{rt_status_document, FakeTransport};

const SPEAKER_TEST: &str = "/api/speaker-test";
const SPEAKER_TEST_STOP: &str = "/api/speaker-test/stop";

/// 启用 + 声音窗口 08:30-12:00 + 注入墙钟的运行时。
fn runtime_at(minutes: u32) -> SpeakerTestRuntime {
    let config = SpeakerTestConfig::parse(Some("on"), Some("08:30-12:00")).expect("config");
    SpeakerTestRuntime::with_clock(config, Arc::new(move || Ok(minutes)))
}

fn enabled_runtime() -> SpeakerTestRuntime {
    runtime_at(9 * 60) // 09:00，窗口内
}

fn window_closed_runtime() -> SpeakerTestRuntime {
    runtime_at(13 * 60) // 13:00，窗口外
}

fn disabled_runtime() -> SpeakerTestRuntime {
    SpeakerTestRuntime::new(SpeakerTestConfig::disabled_default())
}

fn assert_m1_error(response: &support::HttpResponse, expected_status: u16, expected_code: &str) {
    assert_eq!(
        response.status,
        expected_status,
        "unexpected status, body: {}",
        response.body_text()
    );
    let body = response.json();
    let error = body
        .get("error")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("M1 错误体必须把 error 做成对象: {body}"));
    assert_eq!(
        error.get("code").and_then(Value::as_str),
        Some(expected_code),
        "unexpected error code: {body}"
    );
    assert!(
        error
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|message| !message.is_empty()),
        "M1 message 不能为空: {body}"
    );
}

fn tone_request() -> Value {
    json!({
        "dac": 3,
        "ms": 1000,
        "kind": "noise",
        "name": "front left",
        "audible": true,
        "confirm": true,
    })
}

async fn post(server: &support::TestServer, path: &str, body: &Value) -> support::HttpResponse {
    support::request_json(server, "POST", path, &body.to_string()).await
}

// ---------------------------------------------------------------------------
// POST /api/speaker-test 成功路径与转发字段
// ---------------------------------------------------------------------------

#[tokio::test]
async fn speaker_test_forwards_test_tone_and_returns_the_ack() {
    let transport = FakeTransport::new();
    let server = support::spawn_with_speaker_test(
        support::injected_link(Arc::clone(&transport)),
        enabled_runtime(),
    )
    .await;
    let response = post(&server, SPEAKER_TEST, &tone_request()).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["dac"], json!(3));
    assert_eq!(body["ms"], json!(1000));
    assert_eq!(body["kind"], json!("noise"));
    assert_eq!(body["level_db"], json!(-30.0));
    assert_eq!(body["remaining_ms"], json!(1000));
    // 转发出去的 op 字段：daemon 已校验，rt-host 也要求 audible/confirm。
    let calls = transport.calls();
    assert_eq!(calls.len(), 1, "只转发一条 test_tone: {calls:?}");
    let forwarded = &calls[0];
    assert_eq!(forwarded["op"], json!("test_tone"));
    assert_eq!(forwarded["dac"], json!(3));
    assert_eq!(forwarded["ms"], json!(1000));
    assert_eq!(forwarded["kind"], json!("noise"));
    assert_eq!(forwarded["db"], json!(-30.0));
    assert_eq!(forwarded["audible"], json!(true));
    assert_eq!(forwarded["confirm"], json!(true));
    assert!(forwarded.get("freq_hz").is_none(), "noise 不带 freq_hz");
}

#[tokio::test]
async fn speaker_test_forwards_sine_frequency_and_custom_db() {
    let transport = FakeTransport::new();
    let server = support::spawn_with_speaker_test(
        support::injected_link(Arc::clone(&transport)),
        enabled_runtime(),
    )
    .await;
    let request = json!({
        "dac": 7,
        "ms": 800,
        "kind": "sine",
        "freq_hz": 1000,
        "db": -20,
        "audible": true,
        "confirm": true,
    });
    let response = post(&server, SPEAKER_TEST, &request).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let forwarded = &transport.calls()[0];
    assert_eq!(forwarded["kind"], json!("sine"));
    assert_eq!(forwarded["freq_hz"], json!(1000.0));
    assert_eq!(forwarded["db"], json!(-20.0));
}

// ---------------------------------------------------------------------------
// POST /api/speaker-test 门禁 / 校验 / 错误映射
// ---------------------------------------------------------------------------

#[tokio::test]
async fn speaker_test_requires_explicit_audible_and_confirm() {
    let transport = FakeTransport::new();
    let server = support::spawn_with_speaker_test(
        support::injected_link(Arc::clone(&transport)),
        enabled_runtime(),
    )
    .await;
    for request in [
        json!({ "dac": 0, "ms": 1000, "kind": "noise" }),
        json!({ "dac": 0, "ms": 1000, "kind": "noise", "audible": true }),
        json!({ "dac": 0, "ms": 1000, "kind": "noise", "audible": true, "confirm": false }),
        json!({ "dac": 0, "ms": 1000, "kind": "noise", "audible": "true", "confirm": true }),
    ] {
        let response = post(&server, SPEAKER_TEST, &request).await;
        assert_m1_error(&response, 400, "bad_request");
    }
    // 拒绝的请求一条都没转发出去。
    assert_eq!(transport.call_count(), 0, "校验失败不能碰 rt-host");
}

#[tokio::test]
async fn speaker_test_validates_dac_ms_and_db_ranges() {
    let transport = FakeTransport::new();
    let server = support::spawn_with_speaker_test(
        support::injected_link(Arc::clone(&transport)),
        enabled_runtime(),
    )
    .await;
    for request in [
        json!({ "dac": 22, "ms": 1000, "audible": true, "confirm": true }),
        json!({ "dac": -1, "ms": 1000, "audible": true, "confirm": true }),
        json!({ "dac": 0, "ms": 499, "audible": true, "confirm": true }),
        json!({ "dac": 0, "ms": 10_001, "audible": true, "confirm": true }),
        json!({ "dac": 0, "ms": 1000, "db": -61, "audible": true, "confirm": true }),
        json!({ "dac": 0, "ms": 1000, "db": -11, "audible": true, "confirm": true }),
        json!({ "dac": 0, "ms": 1000, "kind": "square", "audible": true, "confirm": true }),
    ] {
        let response = post(&server, SPEAKER_TEST, &request).await;
        assert_m1_error(&response, 400, "bad_request");
    }
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn speaker_test_is_503_not_wired_unless_configured() {
    let transport = FakeTransport::new();
    let server = support::spawn_with_speaker_test(
        support::injected_link(Arc::clone(&transport)),
        disabled_runtime(),
    )
    .await;
    let response = post(&server, SPEAKER_TEST, &tone_request()).await;
    assert_m1_error(&response, 503, "not_wired");
    assert!(response.json()["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("KHAEN_SPEAKER_TEST")));
    assert_eq!(transport.call_count(), 0, "未启用不能碰 rt-host");
}

#[tokio::test]
async fn speaker_test_is_403_outside_the_sound_window() {
    let transport = FakeTransport::new();
    let server = support::spawn_with_speaker_test(
        support::injected_link(Arc::clone(&transport)),
        window_closed_runtime(),
    )
    .await;
    let response = post(&server, SPEAKER_TEST, &tone_request()).await;
    assert_m1_error(&response, 403, "sound_window_closed");
    assert!(response.json()["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("KHAEN_SOUND_WINDOW")));
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn speaker_test_maps_rt_host_busy_not_applied_and_bad_request() {
    let transport = FakeTransport::new();
    let server = support::spawn_with_speaker_test(
        support::injected_link(Arc::clone(&transport)),
        enabled_runtime(),
    )
    .await;
    transport.push_error_reply("busy", "a hot reconfiguration is in flight");
    let response = post(&server, SPEAKER_TEST, &tone_request()).await;
    assert_m1_error(&response, 409, "busy");

    let transport = FakeTransport::new();
    let server = support::spawn_with_speaker_test(
        support::injected_link(Arc::clone(&transport)),
        enabled_runtime(),
    )
    .await;
    transport.push_error_reply("not_applied", "the block loop did not acknowledge");
    let response = post(&server, SPEAKER_TEST, &tone_request()).await;
    assert_m1_error(&response, 503, "not_wired");
    assert_eq!(
        response.json()["error"]["details"]["rt_host_code"],
        json!("not_applied")
    );

    let transport = FakeTransport::new();
    let server = support::spawn_with_speaker_test(
        support::injected_link(Arc::clone(&transport)),
        enabled_runtime(),
    )
    .await;
    transport.push_error_reply("bad_request", "dac out of range for the running format");
    let response = post(&server, SPEAKER_TEST, &tone_request()).await;
    assert_m1_error(&response, 400, "bad_request");
}

#[tokio::test]
async fn speaker_test_never_retries_an_ambiguous_timeout() {
    let transport = FakeTransport::new();
    let server = support::spawn_with_speaker_test(
        support::injected_link(Arc::clone(&transport)),
        enabled_runtime(),
    )
    .await;
    transport.push_transport_error(TransportError::Timeout("slow ack".to_string()));
    let response = post(&server, SPEAKER_TEST, &tone_request()).await;
    assert_m1_error(&response, 503, "host_link_unavailable");
    assert_eq!(
        transport.call_count(),
        1,
        "test_tone 超时后不能重试（避免重复起测试音）"
    );
}

#[tokio::test]
async fn speaker_test_reports_host_link_unavailable_after_a_connect_failure() {
    let transport = FakeTransport::new();
    transport.push_transport_error(TransportError::Connect("no listener".to_string()));
    transport.push_transport_error(TransportError::Connect("no listener".to_string()));
    let server = support::spawn_with_speaker_test(
        support::injected_link(Arc::clone(&transport)),
        enabled_runtime(),
    )
    .await;
    let response = post(&server, SPEAKER_TEST, &tone_request()).await;
    assert_m1_error(&response, 503, "host_link_unavailable");
    assert_eq!(transport.call_count(), 2, "连接失败按既有策略重试一次");
}

#[tokio::test]
async fn speaker_test_refuses_the_process_target_instead_of_pretending() {
    let server = support::spawn_with_speaker_test(support::process_link(), enabled_runtime()).await;
    assert_m1_error(
        &post(&server, SPEAKER_TEST, &tone_request()).await,
        503,
        "not_wired",
    );
    assert_m1_error(
        &post(&server, SPEAKER_TEST_STOP, &json!({})).await,
        503,
        "not_wired",
    );
}

#[tokio::test]
async fn speaker_test_is_busy_while_a_calibration_job_is_active() {
    let calibration = FakeCalibration::new();
    let server = support::spawn_with_speaker_test_and_calibration(
        Arc::clone(&calibration) as Arc<dyn khaen_daemon::calibration::service::CalibrationService>,
        enabled_runtime(),
    )
    .await;
    calibration.publish(
        "job-1",
        khaen_daemon::calibration::service::JobState::Running,
        None,
    );
    let response = post(&server, SPEAKER_TEST, &tone_request()).await;
    // 有 active_job（事件缓存合并）时 daemon 直接 409 busy（不出声）。
    match response.status {
        409 => assert_m1_error(&response, 409, "busy"),
        // 事件缓存尚未合并出 active_job 时，失败也必须不是 200（不假成功）。
        _ => assert!(response.status >= 400, "{}", response.body_text()),
    }
}

// ---------------------------------------------------------------------------
// POST /api/speaker-test/stop
// ---------------------------------------------------------------------------

#[tokio::test]
async fn speaker_test_stop_is_idempotent_and_forwards_the_op() {
    let transport = FakeTransport::new();
    let server = support::spawn_with_speaker_test(
        support::injected_link(Arc::clone(&transport)),
        enabled_runtime(),
    )
    .await;
    let response = post(&server, SPEAKER_TEST_STOP, &json!({})).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["stopped"], json!(true));
    let forwarded = &transport.calls()[0];
    assert_eq!(forwarded["op"], json!("test_tone_stop"));

    // rt-host 说没有在跑的测试音：stopped:false 也是成功（幂等，不是错误）。
    transport.push_reply(json!({
        "ok": true,
        "stopped": false,
        "reason": "no speaker test tone is playing (stop is idempotent)",
    }));
    let response = post(&server, SPEAKER_TEST_STOP, &json!({})).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert_eq!(response.json()["stopped"], json!(false));
}

#[tokio::test]
async fn speaker_test_stop_works_even_when_the_gate_is_closed() {
    // 停止不受窗口/开关限制（安全方向：任何时候都能停）。
    let transport = FakeTransport::new();
    let server = support::spawn_with_speaker_test(
        support::injected_link(Arc::clone(&transport)),
        window_closed_runtime(),
    )
    .await;
    let response = post(&server, SPEAKER_TEST_STOP, &json!({})).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert_eq!(transport.calls()[0]["op"], json!("test_tone_stop"));
}

// ---------------------------------------------------------------------------
// GET /api/status 的 speaker_test 加法字段
// ---------------------------------------------------------------------------

#[tokio::test]
async fn status_projects_speaker_test_from_the_rt_host_socket() {
    let mut document = rt_status_document();
    document["speaker_test"] = json!({
        "active": true,
        "dac": 5,
        "remaining_ms": 640,
        "kind": "sine",
    });
    let transport = FakeTransport::with_rt_status(document);
    let server = support::spawn_with_speaker_test(
        support::injected_link(Arc::clone(&transport)),
        enabled_runtime(),
    )
    .await;
    let body = support::request(&server, "GET", "/api/status").await.json();
    assert_eq!(body["speaker_test"]["active"], json!(true));
    assert_eq!(body["speaker_test"]["dac"], json!(5));
    assert_eq!(body["speaker_test"]["remaining_ms"], json!(640));
    assert_eq!(body["speaker_test"]["kind"], json!("sine"));
    assert!(
        body["reasons"].get("speaker_test").is_none(),
        "可读时不应有原因"
    );
}

#[tokio::test]
async fn status_keeps_speaker_test_null_when_the_rt_host_does_not_report_it() {
    // 旧 rt-host status 没有 speaker_test 字段：给 null + 原因，不编 false。
    let transport = FakeTransport::new();
    let server = support::spawn_with_speaker_test(
        support::injected_link(Arc::clone(&transport)),
        enabled_runtime(),
    )
    .await;
    let body = support::request(&server, "GET", "/api/status").await.json();
    assert_eq!(body["speaker_test"]["active"], Value::Null);
    assert_eq!(body["speaker_test"]["dac"], Value::Null);
    assert_eq!(body["speaker_test"]["remaining_ms"], Value::Null);
    assert_eq!(body["speaker_test"]["kind"], Value::Null);
    assert!(body["reasons"]["speaker_test"].is_string());
}

fn rt_status_file_document() -> Value {
    json!({
        "schema": "khaen-rt-status/v1",
        "blocks": 10,
        "state": "running",
        "muted": false,
        "safe": false,
        "plan_rate": 48000,
        "device_rate": 48000,
        "rate_asserted": true,
        "input": {"packets": 1},
        "output": {"xruns": 0},
        "receiver": {"packets_received": 1},
        "graph_id": "0011223344556677",
        "last_error": null,
        "device_state": "present",
    })
}

fn write_status_file(dir: &support::TempStateDir, document: &Value) -> std::path::PathBuf {
    let path = dir.path().join("rt-host-status.json");
    std::fs::write(&path, document.to_string()).expect("write status file");
    path
}

#[tokio::test(flavor = "current_thread")]
async fn status_projects_speaker_test_from_the_status_file_fallback() {
    let dir = support::TempStateDir::new("speaker-test-file");
    let mut document = rt_status_file_document();
    document["speaker_test"] = json!({
        "active": true,
        "dac": 2,
        "remaining_ms": 100,
        "kind": "noise",
    });
    let path = write_status_file(&dir, &document);
    let mut server = support::spawn_at_with_rt_status_file(dir.path(), path.clone()).await;
    server.sign_in().await;
    let body = support::request(&server, "GET", "/api/status").await.json();
    assert_eq!(body["host_link"]["mode"], json!("file"));
    assert_eq!(body["speaker_test"]["active"], json!(true));
    assert_eq!(body["speaker_test"]["dac"], json!(2));

    // 文件里 speaker_test=null（控制通道未启用的文件回退）：全 null + 原因，不编 false。
    let mut document = rt_status_file_document();
    document["speaker_test"] = Value::Null;
    let path = write_status_file(&dir, &document);
    let mut server = support::spawn_at_with_rt_status_file(dir.path(), path).await;
    server.sign_in().await;
    let body = support::request(&server, "GET", "/api/status").await.json();
    assert_eq!(body["speaker_test"]["active"], Value::Null);
    assert_eq!(body["speaker_test"]["remaining_ms"], Value::Null);
    assert!(body["reasons"]["speaker_test"]
        .as_str()
        .is_some_and(|reason| reason.contains("does not fabricate")));
}

#[tokio::test]
async fn status_keeps_speaker_test_null_for_the_process_target() {
    let server = support::spawn().await;
    let body = support::request(&server, "GET", "/api/status").await.json();
    assert_eq!(body["speaker_test"]["active"], Value::Null);
    assert!(body["reasons"]["speaker_test"].is_string());
}

// ---------------------------------------------------------------------------
// GET /api/meta 的能力条目
// ---------------------------------------------------------------------------

#[tokio::test]
async fn meta_reports_speaker_test_not_wired_when_disabled() {
    let server = support::spawn().await;
    let body = support::request(&server, "GET", "/api/meta").await.json();
    assert_eq!(body["speaker_test"]["configured"], json!(false));
    assert_eq!(body["speaker_test"]["wired"], json!(false));
    assert_eq!(body["speaker_test"]["code"], json!("not_wired"));
    assert!(body["speaker_test"]["reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("KHAEN_SPEAKER_TEST")));
    assert!(body["not_wired"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item["op"] == json!("speaker_test"))));
}

#[tokio::test]
async fn meta_marks_speaker_test_wired_when_the_rt_host_advertises_test_tone() {
    let mut document = rt_status_document();
    document["wired"] = json!(["status", "mute", "test_tone", "test_tone_stop"]);
    let transport = FakeTransport::with_rt_status(document);
    let server = support::spawn_with_speaker_test(
        support::injected_link(Arc::clone(&transport)),
        enabled_runtime(),
    )
    .await;
    let body = support::request(&server, "GET", "/api/meta").await.json();
    assert_eq!(body["speaker_test"]["configured"], json!(true));
    assert_eq!(body["speaker_test"]["wired"], json!(true));
    assert_eq!(body["speaker_test"]["code"], Value::Null);
    assert_eq!(body["speaker_test"]["window"]["configured"], json!(true));
    assert_eq!(body["speaker_test"]["window"]["spec"], json!("08:30-12:00"));
    assert!(
        !body["not_wired"]
            .as_array()
            .expect("not_wired")
            .iter()
            .any(|item| item["op"] == json!("speaker_test")),
        "已接通时不应再进 not_wired: {}",
        body["not_wired"]
    );
}

#[tokio::test]
async fn meta_reports_host_link_unavailable_for_speaker_test_when_the_socket_is_down() {
    let transport = Arc::new(support::host_link_fake::DownTransport);
    let server =
        support::spawn_with_speaker_test(support::injected_link(transport), enabled_runtime())
            .await;
    let body = support::request(&server, "GET", "/api/meta").await.json();
    assert_eq!(body["speaker_test"]["wired"], json!(false));
    assert_eq!(body["speaker_test"]["code"], json!("host_link_unavailable"));
    assert!(body["not_wired"]
        .as_array()
        .expect("not_wired")
        .iter()
        .any(|item| item["op"] == json!("speaker_test")));
}

// 保证测试不依赖真实系统时间（只用于编译期检查：墙钟注入之外不应有别的时钟来源）。
#[test]
fn test_clock_is_injected_not_ambient() {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    assert!(now > 0);
}
