//! M2 `/api/status` 聚合端到端测试：真实 TCP 打真实 axum 服务 + 真实 rt-host 状态文件 fixture。
//!
//! 覆盖：`khaen-status/v1` schema / 字段映射、daemon 自身字段真实（版本/bind/引擎格式）、
//! 状态文件损坏/缺失/过期时的退化语义（`connected=false` + null + reasons，仍 200，绝不 500）。

mod support;

use std::path::Path;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

/// 一份字段齐全的 rt-host 状态文件（schema 与 crates/rt-host/src/main.rs 的 write_status_file 同源）。
fn rt_status_document() -> Value {
    json!({
        "schema": "khaen-rt-status/v1",
        "blocks": 4321,
        "state": "running",
        "muted": false,
        "safe": false,
        "plan_rate": 48000,
        "device_rate": 48000,
        "rate_asserted": true,
        "input": {"packets": 99, "underruns": 1, "jitter_fill": 128, "ssrc": 123456789u64},
        "output": {"xruns": 0, "blocks_written": 4000, "bytes_written": 33792000u64},
        "receiver": {"packets_received": 100, "queue_full_drops": 2, "recv_errors": 0},
        "graph_id": "0011223344556677",
        "last_error": null,
        "device_state": "present",
        "absent_since": null,
        "reopen_attempts": 0,
        "last_device_error": null,
    })
}

fn write_rt_status(path: &Path, document: &Value) {
    std::fs::write(path, document.to_string()).expect("write rt-host status fixture");
}

async fn get_status(server: &support::TestServer) -> (u16, Value) {
    let response = support::request(server, "GET", "/api/status").await;
    (response.status, response.json())
}

#[tokio::test(flavor = "current_thread")]
async fn status_aggregates_a_fresh_rt_host_status_file() {
    let dir = support::TempStateDir::new("status-fresh");
    let rt_status_file = dir.path().join("rt-host-status.json");
    write_rt_status(&rt_status_file, &rt_status_document());
    let mut server = support::spawn_at_with_rt_status_file(dir.path(), rt_status_file).await;
    server.sign_in().await;

    let response = support::request(&server, "GET", "/api/status").await;
    assert_eq!(response.status, 200);
    // M1 的响应头语义保持不变。
    assert_eq!(response.header("content-type"), Some("application/json"));
    assert_eq!(response.header("cache-control"), Some("no-store"));
    let body = response.json();

    assert_eq!(body["schema"], Value::String("khaen-status/v1".into()));
    // daemon 自身字段：版本来自进程、bind 来自启动时解析并注入的监听地址。
    assert_eq!(
        body["daemon"]["version"],
        Value::String(env!("CARGO_PKG_VERSION").into())
    );
    assert_eq!(body["daemon"]["bind"], Value::String(server.addr.clone()));
    assert!(body["daemon"]["uptime_s"].is_u64());

    // rt-host 状态文件新鲜：host_link 已连接，字段逐项映射。
    assert_eq!(body["host_link"]["connected"], Value::Bool(true));
    assert_eq!(body["host_link"]["socket"], Value::Null);
    assert!(body["host_link"]["status_age_ms"].is_u64());
    assert!(body["host_link"]["since"]
        .as_str()
        .is_some_and(|since| since.ends_with('Z')));
    assert_eq!(body["state"]["value"], Value::String("running".into()));
    assert_eq!(body["state"]["muted"], Value::Bool(false));
    assert_eq!(body["state"]["last_error"], Value::Null);
    assert_eq!(
        body["plan"]["graph_id"],
        Value::String("0011223344556677".into())
    );
    assert_eq!(body["format"]["plan_rate"], Value::from(48_000u64));
    assert_eq!(body["format"]["device_rate"], Value::from(48_000u64));
    assert_eq!(body["format"]["rate_asserted"], Value::Bool(true));
    // 引擎格式来自 daemon EngineConfig::default（128 帧 / 2 声道）。
    assert_eq!(body["format"]["block_frames"], Value::from(128u64));
    assert_eq!(body["format"]["channels"], Value::from(2u64));
    assert_eq!(body["input"]["packets"], Value::from(99u64));
    assert_eq!(body["input"]["underruns"], Value::from(1u64));
    assert_eq!(body["input"]["queue_full_drops"], Value::from(2u64));
    assert_eq!(body["output"]["blocks"], Value::from(4321u64));
    assert_eq!(body["output"]["bytes_written"], Value::from(33_792_000u64));
    assert_eq!(body["device"]["state"], Value::String("present".into()));
    assert_eq!(body["device"]["rate_readback"], Value::from(48_000u64));
    // 测试服务显式用 process 目标（进程内引擎）：host_link.target 如实标注，
    // 控制通道按能力进 not_wired（path=host_link.control），不再写死“T70f 未实现”。
    assert_eq!(
        body["host_link"]["target"],
        Value::String("daemon-process".into())
    );
    assert_eq!(body["host_link"]["mode"], Value::String("file".into()));
    assert_eq!(body["host_link"]["socket"], Value::Null);
    assert!(body["not_wired"].as_array().is_some_and(|items| items
        .iter()
        .any(|item| item["path"] == json!("host_link.control"))));
    assert!(body["reasons"]["plan.version"].is_string());
}

#[tokio::test(flavor = "current_thread")]
async fn status_degrades_to_null_reasons_and_never_500() {
    let dir = support::TempStateDir::new("status-degraded");
    let rt_status_file = dir.path().join("rt-host-status.json");
    // 损坏文件：文件可定位（mtime 真实），但内容不可用。
    std::fs::write(&rt_status_file, b"{ not json").expect("write corrupt fixture");
    let mut server =
        support::spawn_at_with_rt_status_file(dir.path(), rt_status_file.clone()).await;
    server.sign_in().await;

    let (status, body) = get_status(&server).await;
    assert_eq!(status, 200, "损坏的状态文件不能变成 500");
    assert_eq!(body["schema"], Value::String("khaen-status/v1".into()));
    assert_eq!(body["host_link"]["connected"], Value::Bool(false));
    assert!(
        body["host_link"]["since"].is_string(),
        "文件存在时 mtime 仍然真实"
    );
    assert!(body["host_link"]["status_age_ms"].is_u64());
    assert_eq!(body["state"]["value"], Value::Null);
    assert!(body["reasons"]["state.value"].is_string());
    assert_eq!(body["plan"]["graph_id"], Value::Null);
    // daemon 自身字段仍返回。
    assert_eq!(
        body["daemon"]["version"],
        Value::String(env!("CARGO_PKG_VERSION").into())
    );
    assert_eq!(body["format"]["block_frames"], Value::from(128u64));

    // 文件被删：since/status_age_ms 变 null，仍 200。
    std::fs::remove_file(&rt_status_file).expect("remove rt status file");
    let (status, body) = get_status(&server).await;
    assert_eq!(status, 200);
    assert_eq!(body["host_link"]["connected"], Value::Bool(false));
    assert_eq!(body["host_link"]["since"], Value::Null);
    assert_eq!(body["host_link"]["status_age_ms"], Value::Null);
    assert!(body["reasons"]["host_link.status_age_ms"].is_string());
    assert_eq!(body["output"]["blocks"], Value::Null);

    // 过期文件：把 mtime 拨到 10 秒前，仍 200 且 connected=false。
    write_rt_status(&rt_status_file, &rt_status_document());
    let file = std::fs::File::options()
        .write(true)
        .open(&rt_status_file)
        .expect("open rt status file");
    let old = SystemTime::now() - Duration::from_secs(10);
    file.set_modified(old).expect("set stale mtime");
    drop(file);
    let (status, body) = get_status(&server).await;
    assert_eq!(status, 200);
    assert_eq!(body["host_link"]["connected"], Value::Bool(false));
    let age = body["host_link"]["status_age_ms"]
        .as_u64()
        .expect("stale status_age_ms");
    assert!(age >= 9_000, "status_age_ms={age}");
    assert!(body["reasons"]["host_link.connected"]
        .as_str()
        .is_some_and(|text| text.contains("stale")));
    assert_eq!(body["state"]["value"], Value::Null);
    assert_eq!(
        body["daemon"]["version"],
        Value::String(env!("CARGO_PKG_VERSION").into())
    );
}
