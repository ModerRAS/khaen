//! T55 M4 后端端到端测试：/api/channels（两阶段 + 数据映射）与 /api/plan/upload|history。
//!
//! 真实 TCP 打真实 axum 服务；rt-host status 用脚本化假传输（与生产同一份行协议编解码）。
//! 覆盖：上传校验（认证/CSRF/sha256/解码/超限/幂等）、history 分页与 commit 关联、channels 的
//! routing 映射与 null 原因、方案路由（mapping/dac_map）由 channels_commit 之外生效的如实上报、
//! token 语义与认证守卫。T61 起热重配的真接通（转发/透传/回滚）见 tests/channels_hot.rs；
//! 真机热重配属 NOT_RUN。

mod support;

use std::sync::Arc;

use khaen_daemon::auth::SESSION_COOKIE;
use khaen_daemon::plan_store::MAX_PLAN_BYTES;
use khaen_host::plan::RoutingSection;
use khaen_host::sha256_hex;
use serde_json::{json, Value};

use support::host_link_fake::{rt_status_document, FakeTransport};
use support::{HttpResponse, TestServer};

/// 测试方案：identity 直通 + routing lane0->dac0、lane1->dac2（dac_channels=4）。
fn plan_bytes() -> (Vec<u8>, String) {
    let mut plan = support::plan_fixture::identity_plan();
    plan.routing = Some(RoutingSection {
        dac_channels: 4,
        routes: vec![(0, 0), (1, 2)],
    });
    let graph_id = plan.graph_id_hex();
    (plan.encode_checked().expect("encode plan"), graph_id)
}

/// rt-host status 文档：指定当前 graph_id 与数据面 format.channels。
fn rt_status(graph_id: &str, channels: u32) -> Value {
    let mut status = rt_status_document();
    status["graph_id"] = json!(graph_id);
    status["actual_version"] = json!(7);
    status["format"]["channels"] = json!(channels);
    status["mode"] = json!("aes67");
    status
}

/// 启动 rt-host 目标服务（脚本化 status），自动登录。
async fn spawn_with_graph(graph_id: &str, channels: u32) -> TestServer {
    let fake = FakeTransport::with_rt_status(rt_status(graph_id, channels));
    let link = support::injected_link(Arc::clone(&fake));
    support::spawn_with_link(link, None).await
}

fn cookie(token: &str) -> (String, String) {
    ("Cookie".to_string(), format!("{SESSION_COOKIE}={token}"))
}

async fn upload_plan(server: &TestServer, bytes: &[u8], query: &str) -> HttpResponse {
    support::request_bytes(
        server,
        "POST",
        &format!("/api/plan/upload{query}"),
        "application/octet-stream",
        bytes,
    )
    .await
}

// ---------------------------------------------------------------------------
// POST /api/plan/upload
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn plan_upload_validates_auth_sha_decode_and_is_idempotent() {
    let server = support::spawn().await;
    let (bytes, graph_id) = plan_bytes();
    let expected_sha = sha256_hex(&bytes);

    // 未认证 401；有会话但缺 CSRF 403。
    let response =
        support::request_bytes_with(&server, "POST", "/api/plan/upload", &[], Some(&bytes)).await;
    assert_eq!(response.status, 401, "{}", response.body_text());
    let session = server.session().expect("session").clone();
    let headers = [cookie(&session.token)];
    let response =
        support::request_bytes_with(&server, "POST", "/api/plan/upload", &headers, Some(&bytes))
            .await;
    assert_eq!(
        response.status,
        403,
        "缺 CSRF 必须 403: {}",
        response.body_text()
    );

    // 第一次：201 + 完整真实元数据。
    let response = upload_plan(&server, &bytes, "").await;
    assert_eq!(response.status, 201, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["schema"], Value::String("khaen-plan/v1".into()));
    assert_eq!(body["created"], Value::Bool(true));
    assert_eq!(body["sha256"], Value::String(expected_sha.clone()));
    assert_eq!(body["graph_id"], Value::String(graph_id));
    assert_eq!(
        body["segments"],
        json!(["content", "crossover", "matrix", "protection", "routing"])
    );
    assert_eq!(body["segment_count"], Value::from(5));
    assert_eq!(body["sample_rate"], Value::from(96_000));
    assert_eq!(body["block_size"], Value::from(128));
    assert!(body["latency_samples"].as_u64().is_some());
    assert_eq!(body["routing"]["dac_channels"], Value::from(4));
    assert_eq!(
        body["routing"]["mapping"],
        json!([{"lane": 0, "dac": 0}, {"lane": 1, "dac": 2}])
    );
    assert!(body["stored_path"]
        .as_str()
        .is_some_and(|path| path.ends_with(".plan")));
    assert_eq!(body["admission"]["accepted"], Value::Bool(true));

    // 同内容幂等：200 + deduplicated=true（内容寻址存储，不重复写）。
    let response = upload_plan(&server, &bytes, "").await;
    assert_eq!(response.status, 200);
    assert_eq!(response.json()["deduplicated"], Value::Bool(true));
    assert_eq!(response.json()["created"], Value::Bool(false));

    // 正确 sha256（query）也接受。
    let response = upload_plan(&server, &bytes, &format!("?sha256={expected_sha}")).await;
    assert_eq!(response.status, 200, "{}", response.body_text());

    // sha 不符 400 plan_sha_mismatch。
    let wrong = "0".repeat(64);
    let response = upload_plan(&server, &bytes, &format!("?sha256={wrong}")).await;
    assert_eq!(response.status, 400, "{}", response.body_text());
    assert_eq!(
        response.json()["error"]["code"],
        Value::String("plan_sha_mismatch".into())
    );

    // 不是 ABI v3：400 plan_decode_failed（带原因）。
    let response = upload_plan(&server, b"not a khaen plan", "").await;
    assert_eq!(response.status, 400, "{}", response.body_text());
    let body = response.json();
    assert_eq!(
        body["error"]["code"],
        Value::String("plan_decode_failed".into())
    );
    assert!(body["error"]["details"]["reason"].as_str().is_some());

    // 空 body：400 invalid_request。
    let response = upload_plan(&server, b"", "").await;
    assert_eq!(response.status, 400);
}

#[tokio::test(flavor = "current_thread")]
async fn plan_upload_rejects_oversized_bodies_with_413() {
    let server = support::spawn().await;
    // 超过 4 MiB 上限一个字节（内容全 0，不做解码，必须先被大小挡住）。
    let oversized = vec![0u8; MAX_PLAN_BYTES + 1];
    let response = upload_plan(&server, &oversized, "").await;
    assert_eq!(response.status, 413, "{}", response.body_text());
    let body = response.json();
    assert_eq!(
        body["error"]["code"],
        Value::String("plan_too_large".into())
    );
    assert_eq!(
        body["error"]["details"]["max_bytes"],
        Value::from(MAX_PLAN_BYTES)
    );
}

// ---------------------------------------------------------------------------
// GET /api/plan/history
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn plan_history_lists_commit_records_and_paginates() {
    let server = support::spawn().await;
    // history 需要真实 prepare→commit：用与 daemon 默认引擎配置匹配的 identity 方案。
    let bytes = support::plan_fixture::identity_plan()
        .encode_checked()
        .expect("encode identity plan");
    let temp = support::plan_fixture::TempPlan::new("m4-history", &bytes);
    let prepare_body = json!({
        "plan_path": temp.path(),
        "plan_sha256": temp.sha256(),
    })
    .to_string();
    let response = support::request_json(&server, "POST", "/api/prepare", &prepare_body).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let token = response.json()["engine_token"]
        .as_str()
        .expect("prepare engine_token")
        .to_string();
    let commit_body = json!({ "engine_token": token }).to_string();
    let response = support::request_json(&server, "POST", "/api/commit", &commit_body).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let commit = response.json();
    let version = commit["version"].as_u64().expect("commit version");
    let graph_id = commit["graph_id"]
        .as_str()
        .expect("commit graph_id")
        .to_string();

    let response = support::request(&server, "GET", "/api/plan/history?limit=10").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(
        body["schema"],
        Value::String("khaen-plan-history/v1".into())
    );
    // process 目标下 commit 只是"排入下一块边界"，没有任何块运行过 => actual_version 仍 null；
    // history 如实把 current 报 null + 原因，而不是把 staged 版本冒充成已生效。
    assert!(body["current"]["version"].is_null(), "{body}");
    assert!(body["reasons"]["history.current"].as_str().is_some());
    let entries = body["entries"].as_array().expect("entries");
    assert!(
        !entries.is_empty(),
        "history 必须包含刚 commit 的版本: {body}"
    );
    let entry = entries
        .iter()
        .find(|entry| entry["version"] == json!(version))
        .expect("committed version entry");
    assert_eq!(entry["graph_id"], json!(graph_id));
    assert_eq!(entry["source"], Value::String("commit".into()));
    assert_eq!(entry["sha256"], Value::String(temp.sha256().to_string()));
    assert!(entry["applied_block"].as_u64().is_some());
    assert!(entry["recorded_at"].as_str().is_some());
    assert!(body["pagination"]["total"].as_u64().unwrap() >= 1);

    // 分页：limit=1 只回 1 条，offset 越过总数回空页。
    let response = support::request(&server, "GET", "/api/plan/history?limit=1&offset=0").await;
    assert_eq!(response.json()["entries"].as_array().map(Vec::len), Some(1));
    let response = support::request(&server, "GET", "/api/plan/history?limit=1&offset=999").await;
    let body = response.json();
    assert_eq!(body["entries"].as_array().map(Vec::len), Some(0));
    assert_eq!(body["pagination"]["has_more"], Value::Bool(false));
    // 参数错：400 invalid_request。
    for path in [
        "/api/plan/history?limit=0",
        "/api/plan/history?limit=101",
        "/api/plan/history?offset=abc",
    ] {
        let response = support::request(&server, "GET", path).await;
        assert_eq!(response.status, 400, "{path}: {}", response.body_text());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn plan_history_reports_the_rt_host_current_version_and_available_versions() {
    // 假 rt-host：status 报 actual_version=7 / graph_id=graph-x；coeff 默认回执报 available_versions。
    let server = spawn_with_graph("graph-x", 4).await;
    let response = support::request(&server, "GET", "/api/plan/history?limit=10").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["source"], Value::String("rt-host".into()));
    assert_eq!(body["current"]["version"], Value::from(7));
    assert_eq!(body["current"]["graph_id"], Value::String("graph-x".into()));
    let entries = body["entries"].as_array().expect("entries");
    let current = entries
        .iter()
        .find(|entry| entry["version"] == json!(7))
        .expect("current version entry");
    assert_eq!(current["source"], Value::String("observed".into()));
    assert_eq!(current["graph_id"], Value::String("graph-x".into()));
    // coeff 的 available_versions（1、2）也进历史；历史版本的 graph_id 控制面不报 -> null。
    assert!(entries.iter().any(|entry| entry["version"] == json!(1)));
    assert!(entries.iter().any(|entry| entry["version"] == json!(2)));
    let historic = entries
        .iter()
        .find(|entry| entry["version"] == json!(1))
        .expect("historic entry");
    assert!(historic["graph_id"].is_null());
}

// ---------------------------------------------------------------------------
// GET /api/channels
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn channels_maps_the_running_plan_routing_and_reports_real_gaps() {
    let (bytes, graph_id) = plan_bytes();
    let server = spawn_with_graph(&graph_id, 4).await;
    let response = upload_plan(&server, &bytes, "").await;
    assert_eq!(response.status, 201, "{}", response.body_text());

    let response = support::request(&server, "GET", "/api/channels").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["schema"], Value::String("khaen-channels/v1".into()));
    assert_eq!(body["target"], Value::String("rt-host".into()));
    // output：channels_status 的 current.channels（真实数据面）；mapping / dac_map 来自真实 routing 段。
    assert_eq!(body["output"]["channels"], Value::from(4));
    assert_eq!(
        body["output"]["channels_source"],
        Value::String("rt-host-channels-status".into())
    );
    assert_eq!(
        body["mapping"],
        json!([{"lane": 0, "dac": 0}, {"lane": 1, "dac": 2}])
    );
    assert_eq!(
        body["output"]["dac_map"],
        json!([0, Value::Null, 1, Value::Null])
    );
    // input：T61 起来自 channels_status.current 的 AES67 真实配置；source/sdp 仍无数据源。
    assert_eq!(body["input"]["channels"], Value::from(2));
    assert_eq!(body["input"]["map"], json!([0, 1]));
    assert!(body["input"]["source"].is_null());
    assert!(body["input"]["sdp_sha256"].is_null());
    for path in ["input.source", "input.sdp_sha256"] {
        assert!(
            body["reasons"][path]
                .as_str()
                .is_some_and(|reason| !reason.is_empty()),
            "{path} 必须有原因: {body}"
        );
    }
    // 热重配已接通：不再列 not_wired；hot_reconfigure.wired=true（能力真实上报）。
    assert_eq!(body["not_wired"], json!([]));
    assert_eq!(body["hot_reconfigure"]["wired"], Value::Bool(true));
    assert_eq!(body["generation"], Value::from(2));
    assert_eq!(body["busy"], Value::Bool(false));
    assert_eq!(body["device"]["state"], Value::String("present".into()));
    assert_eq!(body["device"]["probe"]["available"], Value::Bool(true));
    assert_eq!(body["format"]["channels"], Value::from(4));
    assert_eq!(body["format"]["sample_rate"], Value::from(48_000));
    assert_eq!(body["format"]["block_frames"], Value::from(128));
    assert_eq!(body["current"]["aes67_port"], Value::from(5_004));
    // 本地镜像此时为空（本 daemon 还没 commit 过 channels）：null + 原因，不编。
    assert!(body["applied"].is_null());
    assert!(body["reasons"]["applied"].as_str().is_some());
    assert_eq!(body["plan"]["graph_id"], json!(graph_id));
    assert_eq!(body["plan"]["routing_known"], Value::Bool(true));
}

#[tokio::test(flavor = "current_thread")]
async fn channels_degrades_to_null_with_reasons_when_the_routing_is_unknown() {
    let server = spawn_with_graph("deadbeef-not-a-known-graph", 4).await;
    let response = support::request(&server, "GET", "/api/channels").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    // 数据面 channels 仍来自 rt-host channels_status.current（真实值）。
    assert_eq!(body["output"]["channels"], Value::from(4));
    assert_eq!(
        body["output"]["channels_source"],
        Value::String("rt-host-channels-status".into())
    );
    // routing 未知：mapping / dac_map null + 原因（不编映射）。
    assert!(body["mapping"].is_null());
    assert!(body["output"]["dac_map"].is_null());
    assert!(body["reasons"]["mapping"]
        .as_str()
        .is_some_and(|reason| reason.contains("routing")));
    assert_eq!(body["plan"]["routing_known"], Value::Bool(false));
    // AES67 输入来自 channels_status，与 routing 是否已知无关。
    assert_eq!(body["input"]["channels"], Value::from(2));
}

// ---------------------------------------------------------------------------
// PUT /api/channels（两阶段）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn channels_put_mapping_only_prepare_reports_the_plan_owned_blocker() {
    let (bytes, graph_id) = plan_bytes();
    let server = spawn_with_graph(&graph_id, 4).await;
    let response = upload_plan(&server, &bytes, "").await;
    assert_eq!(response.status, 201, "{}", response.body_text());

    // mapping/dac_map 是方案 routing：channels_commit 改不了，prepare 如实返回 blocker、
    // 不签发 token（不假成功）。
    let prepare_body = json!({
        "stage": "prepare",
        "mapping": [{"lane": 0, "dac": 1}, {"lane": 1, "dac": 3}],
    })
    .to_string();
    let response = support::request_json(&server, "PUT", "/api/channels", &prepare_body).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let prepare = response.json();
    assert_eq!(prepare["stage"], Value::String("prepare".into()));
    assert_eq!(prepare["commit_supported"], Value::Bool(false));
    assert!(prepare["engine_token"].is_null(), "{prepare}");
    assert_eq!(prepare["mapping"]["changed"], Value::Bool(true));
    assert_eq!(
        prepare["mapping"]["diff"]["added"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(
        prepare["mapping"]["diff"]["removed"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(
        prepare["mapping"]["diff"]["changed"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(
        prepare["mapping"]["current"],
        json!([{"lane": 0, "dac": 0}, {"lane": 1, "dac": 2}])
    );
    assert_eq!(
        prepare["mapping"]["applied_by"],
        Value::String("plan routing section (plan prepare + commit)".into())
    );
    assert!(prepare["blockers"][0]
        .as_str()
        .is_some_and(|reason| reason.contains("plan routing")));

    // 越界 mapping（dac 9 >= dac_channels 4）：400 invalid_request。
    let bad_body = json!({"stage": "prepare", "mapping": [{"lane": 0, "dac": 9}]}).to_string();
    let response = support::request_json(&server, "PUT", "/api/channels", &bad_body).await;
    assert_eq!(response.status, 400, "{}", response.body_text());
    assert_eq!(
        response.json()["error"]["code"],
        Value::String("invalid_request".into())
    );

    // 未知 stage：400。
    let bad_body = json!({"stage": "blah"}).to_string();
    let response = support::request_json(&server, "PUT", "/api/channels", &bad_body).await;
    assert_eq!(response.status, 400);
}

#[tokio::test(flavor = "current_thread")]
async fn channels_put_prepare_without_running_plan_only_does_structural_validation() {
    let server = spawn_with_graph("unknown-graph", 4).await;
    let body = json!({"stage": "prepare", "mapping": [{"lane": 0, "dac": 1}]}).to_string();
    let response = support::request_json(&server, "PUT", "/api/channels", &body).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let prepare = response.json();
    // 运行 routing 未知：仍然不签发 token（mapping 是方案路由，channels_commit 改不了），
    // 但 planned diff 与结构校验照做，并如实给原因。
    assert!(prepare["mapping"]["current"].is_null());
    assert_eq!(prepare["mapping"]["changed"], Value::Null);
    assert!(prepare["reasons"].to_string().contains("structure"));
    assert!(prepare["engine_token"].is_null());
    assert_eq!(prepare["commit_supported"], Value::Bool(false));
    assert!(prepare["reasons"]["mapping.current"]
        .as_str()
        .is_some_and(|reason| reason.contains("routing") || reason.contains("graph_id")));
}

#[tokio::test(flavor = "current_thread")]
async fn channels_put_commit_without_prepare_is_409_no_channels_draft() {
    let fake =
        support::host_link_fake::FakeTransport::with_rt_status(rt_status("unknown-graph", 4));
    // rt-host 侧没有草稿：业务错误码原样透传（409，不重试、不编成功）。
    fake.push_error_reply("no_channels_draft", "no prepared channels draft");
    let link = support::injected_link(Arc::clone(&fake));
    let server = support::spawn_with_link(link, None).await;
    let body = json!({"stage": "commit", "engine_token": "aabbccdd"}).to_string();
    let response = support::request_json(&server, "PUT", "/api/channels", &body).await;
    assert_eq!(response.status, 409, "{}", response.body_text());
    let body = response.json();
    assert_eq!(
        body["error"]["code"],
        Value::String("no_channels_draft".into())
    );
    assert_eq!(fake.call_count(), 1, "业务错误是有效响应：只发一次");
}

// ---------------------------------------------------------------------------
// 认证 / CSRF 守卫
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn m4_endpoints_require_a_session_and_csrf() {
    let (bytes, graph_id) = plan_bytes();
    let server = spawn_with_graph(&graph_id, 4).await;
    for (method, path) in [
        ("GET", "/api/channels"),
        ("GET", "/api/plan/history"),
        ("PUT", "/api/channels"),
        ("POST", "/api/plan/upload"),
    ] {
        let response = support::request_with(&server, method, path, &[], None).await;
        assert_eq!(
            response.status,
            401,
            "{method} {path}: {}",
            response.body_text()
        );
    }
    // 有会话但缺 CSRF：写方法 403。
    let session = server.session().expect("session").clone();
    let headers = [cookie(&session.token)];
    let response =
        support::request_bytes_with(&server, "POST", "/api/plan/upload", &headers, Some(&bytes))
            .await;
    assert_eq!(response.status, 403, "{}", response.body_text());
}
