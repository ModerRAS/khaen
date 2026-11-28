//! T61：daemon /api/channels 接 rt-host M3b 热重配 —— 端到端测试。
//!
//! 真实 TCP 打真实 axum 服务；rt-host 用脚本化假传输（与生产同一份行协议编解码）。
//! 覆盖：GET 真实 channels_status 形态/降级、PUT prepare 转发字段逐字、
//! commit 成功回执透传 + 本地镜像、失败回滚语义、busy/断链、abort、process 501、
//! 方案图被丢弃后的 routing 失效与方案 commit 恢复、认证/CSRF。
//!
//! 真机热重配（会重开 PCM/改采样率）属 NOT_RUN。

mod support;

use std::sync::Arc;

use khaen_daemon::auth::SESSION_COOKIE;
use khaen_host::plan::RoutingSection;
use serde_json::{json, Value};

use support::host_link_fake::{
    channels_commit_document, channels_status_document, rt_status_document, DownTransport,
    FakeTransport,
};
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

/// rt-host status 文档：指定 graph_id 与数据面 format.channels。
fn rt_status(graph_id: &str, channels: u32) -> Value {
    let mut status = rt_status_document();
    status["graph_id"] = json!(graph_id);
    status["actual_version"] = json!(7);
    status["format"]["channels"] = json!(channels);
    status["format"]["sample_rate"] = json!(48_000);
    status["format"]["block_size"] = json!(128);
    status["mode"] = json!("aes67");
    status
}

/// 启动 rt-host 目标服务（status + channels_status 默认文档），返回服务与假传输。
async fn spawn_with_graph(graph_id: &str, channels: u32) -> (TestServer, Arc<FakeTransport>) {
    let fake = FakeTransport::with_rt_status_and_channels(
        rt_status(graph_id, channels),
        channels_status_document(),
    );
    let link = support::injected_link(Arc::clone(&fake));
    let server = support::spawn_with_link(link, None).await;
    (server, fake)
}

fn cookie(token: &str) -> (String, String) {
    ("Cookie".to_string(), format!("{SESSION_COOKIE}={token}"))
}

async fn upload_plan(server: &TestServer) -> (Vec<u8>, String) {
    let (bytes, graph_id) = plan_bytes();
    let response = support::request_bytes(
        server,
        "POST",
        "/api/plan/upload",
        "application/octet-stream",
        &bytes,
    )
    .await;
    assert_eq!(response.status, 201, "{}", response.body_text());
    (bytes, graph_id)
}

async fn put_channels(server: &TestServer, body: Value) -> HttpResponse {
    support::request_json(server, "PUT", "/api/channels", &body.to_string()).await
}

fn op_of(request: &Value) -> &str {
    request.get("op").and_then(Value::as_str).unwrap_or("")
}

// ---------------------------------------------------------------------------
// GET /api/channels：channels_status 真实形态
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn get_channels_reports_the_live_channels_status_shape() {
    let (bytes, graph_id) = plan_bytes();
    let mut channels = channels_status_document();
    channels["generation"] = json!(5);
    channels["last_gap_ms"] = json!(17);
    channels["last_result"] = json!({ "ok": true, "applied": true, "generation": 5 });
    channels["current"]["aes67_port"] = json!(5_008);
    let fake = FakeTransport::with_rt_status_and_channels(rt_status(&graph_id, 4), channels);
    let link = support::injected_link(Arc::clone(&fake));
    let server = support::spawn_with_link(link, None).await;
    let response = support::request_bytes(
        &server,
        "POST",
        "/api/plan/upload",
        "application/octet-stream",
        &bytes,
    )
    .await;
    assert_eq!(response.status, 201, "{}", response.body_text());

    let response = support::request(&server, "GET", "/api/channels").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["schema"], "khaen-channels/v1");
    assert_eq!(body["target"], "rt-host");
    assert_eq!(body["output"]["channels"], 4);
    assert_eq!(body["output"]["channels_source"], "rt-host-channels-status");
    assert_eq!(
        body["output"]["dac_map"],
        json!([0, Value::Null, 1, Value::Null])
    );
    assert_eq!(
        body["mapping"],
        json!([{"lane": 0, "dac": 0}, {"lane": 1, "dac": 2}])
    );
    assert_eq!(body["input"]["channels"], 2);
    assert_eq!(body["input"]["map"], json!([0, 1]));
    // source/sdp 控制通道不报：null + 原因（不编 sdp/manual）。
    assert!(body["input"]["source"].is_null());
    assert!(body["input"]["sdp_sha256"].is_null());
    assert!(body["reasons"]["input.source"].as_str().is_some());
    assert!(body["reasons"]["input.sdp_sha256"].as_str().is_some());
    assert_eq!(body["generation"], 5);
    assert_eq!(body["last_gap_ms"], 17);
    assert_eq!(body["last_result"]["generation"], 5);
    assert_eq!(body["busy"], false);
    assert!(body["pending"].is_null());
    assert_eq!(body["device"]["name"], "hw:CARD=UltraLite");
    assert_eq!(body["device"]["state"], "present");
    assert_eq!(body["device"]["probe"]["available"], true);
    assert_eq!(body["current"]["aes67_port"], 5_008);
    assert_eq!(body["format"]["source"], "rt-host-channels-status");
    assert_eq!(body["format"]["channels"], 4);
    assert_eq!(body["hot_reconfigure"]["wired"], true);
    assert!(body["hot_reconfigure"]["reason"].is_null());
    assert_eq!(body["not_wired"], json!([]));
    assert_eq!(body["plan"]["routing_known"], true);
    assert_eq!(body["plan"]["graph_id"], graph_id.as_str());
    // 本地镜像尚无记录：null + 原因。
    assert!(body["applied"].is_null());
    assert!(body["reasons"]["applied"].as_str().is_some());
    // op 顺序：status（routing 关联）-> channels_status。
    let calls = fake.calls();
    assert_eq!(op_of(&calls[0]), "status");
    assert_eq!(op_of(&calls[1]), "channels_status");
}

#[tokio::test(flavor = "current_thread")]
async fn get_channels_reports_the_rt_host_aes67_input_provenance() {
    let (_, graph_id) = plan_bytes();
    let mut channels = channels_status_document();
    channels["input_source"] = json!({
        "source": "sdp",
        "sdp_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
    });
    let fake = FakeTransport::with_rt_status_and_channels(rt_status(&graph_id, 4), channels);
    let link = support::injected_link(Arc::clone(&fake));
    let server = support::spawn_with_link(link, None).await;

    let response = support::request(&server, "GET", "/api/channels").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["input"]["source"], json!("sdp"));
    assert_eq!(
        body["input"]["sdp_sha256"],
        json!("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
    );
    assert!(
        !body["reasons"]
            .as_object()
            .expect("reasons")
            .contains_key("input.source"),
        "provenance must remove the input.source reason: {body}"
    );
    assert!(
        !body["reasons"]
            .as_object()
            .expect("reasons")
            .contains_key("input.sdp_sha256"),
        "provenance must remove the input.sdp_sha256 reason: {body}"
    );

    // manual：没有 SDP 文件，sdp_sha256 必须 null 且给明确原因（不是缺数据源）。
    let mut manual = channels_status_document();
    manual["input_source"] = json!({ "source": "manual", "sdp_sha256": Value::Null });
    let fake = FakeTransport::with_rt_status_and_channels(rt_status(&graph_id, 4), manual);
    let link = support::injected_link(Arc::clone(&fake));
    let server = support::spawn_with_link(link, None).await;
    let response = support::request(&server, "GET", "/api/channels").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["input"]["source"], json!("manual"));
    assert!(body["input"]["sdp_sha256"].is_null());
    assert!(
        body["reasons"]["input.sdp_sha256"]
            .as_str()
            .expect("manual sdp reason")
            .contains("manually"),
        "manual must explain why sdp_sha256 is null: {body}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn get_channels_uses_readback_routing_for_a_plan_loaded_by_another_daemon() {
    // rt-host 已经加载了一份方案（例如重启前/别的 daemon 准备），本 daemon 的方案库没有记录；
    // control status 的 readback.routing 是当前 lane -> DAC 的真实来源。
    let graph_id = "externally-loaded-graph";
    let mut status = rt_status(graph_id, 4);
    status["readback"] = json!({
        "available": true,
        "source": "base_plan",
        "routing": {
            "dac_channels": 4,
            "routes": [{"lane": 0, "dac": 1}, {"lane": 1, "dac": 3}]
        }
    });
    let fake = FakeTransport::with_rt_status_and_channels(status, channels_status_document());
    let link = support::injected_link(Arc::clone(&fake));
    let server = support::spawn_with_link(link, None).await;

    let response = support::request(&server, "GET", "/api/channels").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["plan"]["graph_id"], graph_id);
    assert_eq!(body["plan"]["routing_known"], true);
    assert_eq!(
        body["output"]["dac_map"],
        json!([Value::Null, 0, Value::Null, 1])
    );
    assert_eq!(
        body["mapping"],
        json!([{"lane": 0, "dac": 1}, {"lane": 1, "dac": 3}])
    );
    assert!(
        !body["reasons"]
            .as_object()
            .expect("reasons")
            .contains_key("mapping"),
        "readback routing must not produce a mapping reason: {body}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn get_channels_degrades_with_reasons_when_channels_status_is_unsupported() {
    let fake = FakeTransport::new();
    // 第一次调用（status）成功；第二次（channels_status）是 rt-host 的业务 unsupported。
    fake.push_reply(rt_status("old-rt-host", 4));
    fake.push_error_reply(
        "unsupported",
        "hot reconfiguration is not wired in this process: there is no data-plane channel context",
    );
    let link = support::injected_link(Arc::clone(&fake));
    let server = support::spawn_with_link(link, None).await;
    let response = support::request(&server, "GET", "/api/channels").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    // 真实降级：output.channels 退回契约 B status 的 data-plane format（仍是真的）。
    assert_eq!(body["output"]["channels"], 4);
    assert_eq!(body["output"]["channels_source"], "rt-host-control-status");
    assert_eq!(body["format"]["source"], "rt-host-control-status");
    assert_eq!(body["format"]["channels"], 4);
    assert_eq!(body["format"]["block_frames"], 128);
    // channels_status 拿不到的字段：null + 原因（不编）。
    assert!(body["current"].is_null());
    assert!(body["device"].is_null());
    assert!(body["input"]["channels"].is_null());
    assert!(body["input"]["map"].is_null());
    assert!(body["generation"].is_null());
    assert!(body["reasons"]["generation"].as_str().is_some());
    assert!(body["reasons"]["device"].as_str().is_some());
    assert!(body["reasons"]["channels_status"]
        .as_str()
        .is_some_and(|reason| reason.contains("not wired")));
    // rt-host 明确没接通：如实进 not_wired（不是链路问题）。
    assert_eq!(body["hot_reconfigure"]["wired"], false);
    assert_eq!(body["not_wired"][0]["path"], "channels.hot_reconfigure");
    assert!(body["not_wired"][0]["reason"].as_str().is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn get_channels_degrades_with_reasons_when_the_control_link_is_down() {
    let link = support::injected_link(Arc::new(DownTransport));
    let server = support::spawn_with_link(link, None).await;
    let response = support::request(&server, "GET", "/api/channels").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    // 链路断：所有数据面字段 null + 原因；能力无法核实（null，不是 false）。
    assert!(body["output"]["channels"].is_null());
    assert!(body["output"]["dac_map"].is_null());
    assert!(body["mapping"].is_null());
    assert!(body["input"]["channels"].is_null());
    assert!(body["current"].is_null());
    assert!(body["generation"].is_null());
    assert!(body["hot_reconfigure"]["wired"].is_null());
    assert!(body["hot_reconfigure"]["reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("unavailable")));
    assert_eq!(
        body["not_wired"],
        json!([]),
        "链路断不是能力缺口（不冒充 not_wired）"
    );
    assert!(body["reasons"]["channels_status"].as_str().is_some());
    assert_eq!(body["plan"]["source"], "none");
}

// ---------------------------------------------------------------------------
// PUT /api/channels：prepare 转发
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn put_prepare_forwards_rt_host_fields_verbatim() {
    let (_, graph_id) = plan_bytes();
    let (server, fake) = spawn_with_graph(&graph_id, 4).await;
    upload_plan(&server).await;

    let body = json!({
        "stage": "prepare",
        "output": { "dac_map": [0, null, 1, null] },
        "input": { "channels": 4, "map": [0, 1, 2, 3] },
        "content_channels": 2,
        "sample_rate": 48_000,
        "aes67_group": "239.69.1.9",
        "aes67_port": 5_006,
        "aes67_payload_type": 98,
        "jitter_frames": 256,
    });
    let response = put_channels(&server, body).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let prepare = response.json();
    assert_eq!(prepare["ok"], true);
    assert_eq!(prepare["schema"], "khaen-channels/v1");
    assert_eq!(prepare["stage"], "prepare");
    assert_eq!(prepare["target"], "rt-host");
    assert_eq!(prepare["commit_supported"], true);
    assert_eq!(prepare["engine_token"], "channels-token-1");
    assert_eq!(prepare["token"], "channels-token-1");
    assert_eq!(prepare["planned_generation"], 3);
    // dac_map 长度派生输出通道数（显式提示，不是静默）。
    assert!(prepare["warnings"]
        .as_array()
        .is_some_and(|warnings| warnings.iter().any(|warning| warning
            .as_str()
            .is_some_and(|text| text.contains("derived")))));
    // dac_map 与运行中的 routing 一致：mapping.changed=false（方案 routing 不被改）。
    assert_eq!(prepare["mapping"]["requested"], true);
    assert_eq!(prepare["mapping"]["changed"], false);
    assert_eq!(
        prepare["mapping"]["applied_by"],
        "plan routing section (plan prepare + commit)"
    );
    // preview 来自 rt-host 回执的 current/target（gap 不估）。
    assert_eq!(prepare["preview"]["old"]["channels"], 4);
    assert!(prepare["preview"]["audio_gap_ms_estimate"].is_null());

    // 转发给 rt-host 的字段逐字（不含 daemon 自己的 mapping/dac_map/output/input 词汇）。
    let calls = fake.calls();
    assert_eq!(op_of(&calls[0]), "status");
    assert_eq!(op_of(&calls[1]), "channels_prepare");
    assert_eq!(
        calls[1],
        json!({
            "op": "channels_prepare",
            "channels": 4,
            "content_channels": 2,
            "sample_rate": 48_000,
            "aes67_group": "239.69.1.9",
            "aes67_port": 5_006,
            "aes67_payload_type": 98,
            "aes67_channels": 4,
            "aes67_map": [0, 1, 2, 3],
            "jitter_frames": 256,
        })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn put_prepare_forwards_raw_sdp_to_the_rt_host() {
    let (_, graph_id) = plan_bytes();
    let (server, fake) = spawn_with_graph(&graph_id, 4).await;
    let sdp = "v=0\r\ns=test\r\nc=IN IP4 239.69.1.9\r\n";
    let response = put_channels(&server, json!({ "stage": "prepare", "sdp": sdp })).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let calls = fake.calls();
    assert_eq!(op_of(&calls[0]), "channels_prepare");
    assert_eq!(calls[0]["sdp"], json!(sdp));
}

#[tokio::test(flavor = "current_thread")]
async fn put_prepare_rt_host_business_error_is_passed_through() {
    let fake = FakeTransport::new();
    fake.push_error_reply(
        "device_unavailable",
        "ALSA device hw:CARD=UltraLite is not present in /proc/asound/cards",
    );
    let link = support::injected_link(Arc::clone(&fake));
    let server = support::spawn_with_link(link, None).await;
    let response = put_channels(&server, json!({ "stage": "prepare", "aes67_port": 5_006 })).await;
    // 设备不可用：503（fail-closed，不签发 token）。
    assert_eq!(response.status, 503, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["error"]["code"], "device_unavailable");
    assert!(body["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("not present")));
    assert_eq!(body["error"]["details"]["phase"], "channels_prepare");
    assert_eq!(fake.call_count(), 1, "业务错误是有效响应：不重试、不编成功");
}
// ---------------------------------------------------------------------------
// PUT /api/channels：commit / abort
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn put_commit_passes_through_the_receipt_and_updates_the_local_mirror() {
    let (_, graph_id) = plan_bytes();
    let (server, fake) = spawn_with_graph(&graph_id, 4).await;
    upload_plan(&server).await;
    // prepare：mapping 不变 + 改 AES67 端口（rt-host 图保持）。
    let prepare = put_channels(
        &server,
        json!({
            "stage": "prepare",
            "dac_map": [0, null, 1, null],
            "aes67_port": 5_006,
        }),
    )
    .await;
    assert_eq!(prepare.status, 200, "{}", prepare.body_text());
    let token = prepare.json()["engine_token"]
        .as_str()
        .expect("token")
        .to_string();
    let response = put_channels(&server, json!({ "stage": "commit", "engine_token": token })).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let commit = response.json();
    // rt-host 回执逐字段原样透传（不是 daemon 重编的）。
    let expected = channels_commit_document();
    for field in [
        "op",
        "applied",
        "changed",
        "idempotent",
        "generation",
        "gap_ms",
        "format",
        "channels",
        "device_restarted",
        "pcm_reconfigured",
        "receiver_rebound",
        "engine_rebuilt",
        "graph_dropped",
        "device_was_absent",
        "hw",
        "alsa_stats_reset",
        "input_stats_reset",
        "rolled_back",
    ] {
        assert_eq!(
            commit[field], expected[field],
            "field {field} must pass through"
        );
    }
    assert_eq!(commit["schema"], "khaen-channels/v1");
    assert_eq!(commit["stage"], "commit");
    assert_eq!(commit["target"], "rt-host");
    // commit 不改方案 routing：如实说明（mapping 本来是 running plan 的）。
    assert_eq!(commit["routing"]["requested"], true);
    assert_eq!(commit["routing"]["changed"], false);
    assert_eq!(commit["routing"]["applied"], false);
    assert_eq!(commit["routing"]["plan_recommit_required"], false);
    assert_eq!(commit["local"]["mirror_updated"], true);
    assert_eq!(commit["local"]["pending_routing_known"], true);
    // 本地镜像更新：GET 的 applied 段来自刚提交的回执（快照 + 记录时刻）。
    let response = support::request(&server, "GET", "/api/channels").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["applied"]["source"], "daemon-channels-commit");
    assert_eq!(body["applied"]["applied"], true);
    assert_eq!(body["applied"]["generation"], 3);
    assert_eq!(body["applied"]["gap_ms"], 412);
    assert_eq!(body["applied"]["format"]["channels"], 4);
    assert_eq!(body["applied"]["graph_dropped"], false);
    assert!(body["applied"]["recorded_at"].as_str().is_some());
    assert!(body["reasons"]["applied"].is_null());
    // 实时字段仍以 channels_status 为准（镜像不覆盖实时数据源）。
    assert_eq!(body["generation"], 2);
    // 第 3 次调用是 channels_commit，token 原样。
    let calls = fake.calls();
    assert_eq!(op_of(&calls[2]), "channels_commit");
    assert_eq!(calls[2]["token"], "channels-token-1");
}

#[tokio::test(flavor = "current_thread")]
async fn put_commit_failure_keeps_the_rollback_truth_and_maps_502() {
    let (_, graph_id) = plan_bytes();
    let (server, fake) = spawn_with_graph(&graph_id, 4).await;
    // rt-host 在块边界执行过重配但打开新设备失败 -> ok=false + 回滚真值。
    fake.push_error_body(json!({
        "ok": false,
        "op": "channels_commit",
        "error": "ERR_RECONFIGURE_DEVICE",
        "stage": "device_open",
        "message": "open new output device failed: Device or resource busy",
        "rolled_back": true,
        "device_started": true,
        "receiver_running": true,
        "gap_ms": 37,
        "generation": 4,
        "applied": false,
    }));
    let response = put_channels(
        &server,
        json!({ "stage": "commit", "token": "channels-token-1" }),
    )
    .await;
    // 上游数据面失败（已回滚）：502，错误码/文案/回滚真值原样带出。
    assert_eq!(response.status, 502, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["error"]["code"], "ERR_RECONFIGURE_DEVICE");
    assert!(body["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("Device or resource busy")));
    assert_eq!(body["error"]["details"]["rolled_back"], true);
    assert_eq!(body["error"]["details"]["stage"], "device_open");
    assert_eq!(body["error"]["details"]["applied"], false);
    assert_eq!(body["error"]["details"]["generation"], 4);
    assert_eq!(body["error"]["details"]["phase"], "channels_commit");
    // 失败不写成功镜像：GET applied 仍 null + 原因。
    let response = support::request(&server, "GET", "/api/channels").await;
    let body = response.json();
    assert!(body["applied"].is_null());
    assert!(body["reasons"]["applied"].as_str().is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn put_commit_busy_and_missing_draft_map_to_409() {
    let (_, graph_id) = plan_bytes();
    let (server, fake) = spawn_with_graph(&graph_id, 4).await;
    fake.push_error_reply(
        "channels_busy",
        "another channels commit is in flight (or its acknowledgement was lost)",
    );
    let response = put_channels(
        &server,
        json!({ "stage": "commit", "engine_token": "channels-token-1" }),
    )
    .await;
    assert_eq!(response.status, 409, "{}", response.body_text());
    assert_eq!(response.json()["error"]["code"], "channels_busy");
    // 草稿不存在：409 no_channels_draft（可重试 prepare）。
    fake.push_error_reply("no_channels_draft", "no prepared channels draft");
    let response = put_channels(
        &server,
        json!({ "stage": "commit", "engine_token": "channels-token-1" }),
    )
    .await;
    assert_eq!(response.status, 409, "{}", response.body_text());
    assert_eq!(response.json()["error"]["code"], "no_channels_draft");
    // busy 类错误不消耗本地意图：再提交一次仍会转发（不把可重试错误变成永久失败）。
    assert_eq!(
        fake.calls()
            .iter()
            .filter(|call| op_of(call) == "channels_commit")
            .count(),
        2
    );
}

#[tokio::test(flavor = "current_thread")]
async fn put_abort_discards_the_rt_host_draft_and_the_local_routing_intent() {
    let (_, graph_id) = plan_bytes();
    let (server, fake) = spawn_with_graph(&graph_id, 4).await;
    upload_plan(&server).await;
    let prepare = put_channels(
        &server,
        json!({
            "stage": "prepare",
            "dac_map": [0, null, 1, null],
            "aes67_port": 5_006,
        }),
    )
    .await;
    assert_eq!(prepare.status, 200, "{}", prepare.body_text());
    let token = prepare.json()["engine_token"]
        .as_str()
        .expect("token")
        .to_string();
    let response = put_channels(&server, json!({ "stage": "abort", "engine_token": token })).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let abort = response.json();
    assert_eq!(abort["ok"], true);
    assert_eq!(abort["aborted"], true);
    assert_eq!(abort["aborted_token"], "channels-token-1");
    assert_eq!(abort["stage"], "abort");
    assert_eq!(abort["local"]["pending_routing_discarded"], true);
    // 本地路由意图已丢弃：之后再 commit 的回执如实报 pending unknown（不冒充知道 mapping）。
    let commit = put_channels(
        &server,
        json!({ "stage": "commit", "engine_token": "channels-token-1" }),
    )
    .await;
    assert_eq!(commit.status, 200, "{}", commit.body_text());
    let commit = commit.json();
    assert_eq!(commit["local"]["pending_routing_known"], false);
    assert!(commit["routing"]["requested"].is_null());
    // abort 之后 rt-host 回执的 aborted_token 原样透传（不是 daemon 编的）。
    let calls = fake.calls();
    let abort_call = calls
        .iter()
        .find(|call| op_of(call) == "channels_abort")
        .expect("channels_abort forwarded");
    assert_eq!(abort_call["token"], "channels-token-1");
}
// ---------------------------------------------------------------------------
// 断链 / process 目标 / 认证
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn put_link_down_is_503_host_link_unavailable_for_every_stage() {
    let link = support::injected_link(Arc::new(DownTransport));
    let server = support::spawn_with_link(link, None).await;
    for (body, stage) in [
        (
            json!({ "stage": "prepare", "aes67_port": 5_006 }),
            "prepare",
        ),
        (json!({ "stage": "commit", "engine_token": "t" }), "commit"),
        (json!({ "stage": "abort", "engine_token": "t" }), "abort"),
    ] {
        let response = put_channels(&server, body).await;
        assert_eq!(response.status, 503, "{stage}: {}", response.body_text());
        let body = response.json();
        assert_eq!(body["error"]["code"], "host_link_unavailable", "{stage}");
        assert_eq!(
            body["error"]["details"]["phase"],
            format!("channels_{stage}")
        );
        assert!(body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("unavailable")));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn put_process_target_is_501_for_every_stage_without_a_fake_token() {
    // process 目标（默认测试服务）：没有数据面热重配，任何阶段都如实 501 + reason。
    let server = support::spawn().await;
    for (body, stage) in [
        (
            json!({ "stage": "prepare", "aes67_port": 5_006 }),
            "prepare",
        ),
        (json!({ "stage": "commit", "engine_token": "t" }), "commit"),
        (json!({ "stage": "abort", "engine_token": "t" }), "abort"),
    ] {
        let response = put_channels(&server, body).await;
        assert_eq!(response.status, 501, "{stage}: {}", response.body_text());
        let body = response.json();
        assert_eq!(body["error"]["code"], "host_link_unsupported", "{stage}");
        assert_eq!(body["error"]["details"]["target"], "daemon-process");
        assert_eq!(body["error"]["details"]["commit_supported"], false);
        assert!(body["error"]["details"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("KHAEN_RT_TARGET=process")));
    }
    // GET 仍返回进程内控制面的真实快照 + 显式 target，并如实列 not_wired。
    let response = support::request(&server, "GET", "/api/channels").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["target"], "daemon-process");
    assert_eq!(body["hot_reconfigure"]["wired"], false);
    assert_eq!(body["not_wired"][0]["path"], "channels.hot_reconfigure");
    assert!(body["output"]["channels"].as_u64().is_some(), "{body}");
}

#[tokio::test(flavor = "current_thread")]
async fn channels_write_requires_a_session_and_csrf() {
    let (_, graph_id) = plan_bytes();
    let (server, _) = spawn_with_graph(&graph_id, 4).await;
    // 无会话 401。
    let response = support::request_with(
        &server,
        "PUT",
        "/api/channels",
        &[],
        Some(r#"{"stage":"commit","engine_token":"t"}"#),
    )
    .await;
    assert_eq!(response.status, 401, "{}", response.body_text());
    // 有会话但缺 CSRF 403（写方法统一要求）。
    let session = server.session().expect("session").clone();
    let headers = [cookie(&session.token)];
    let response = support::request_with(
        &server,
        "PUT",
        "/api/channels",
        &headers,
        Some(r#"{"stage":"commit","engine_token":"t"}"#),
    )
    .await;
    assert_eq!(response.status, 403, "{}", response.body_text());
}

// ---------------------------------------------------------------------------
// 方案图被 channels_commit 丢弃：routing 失效 -> 方案重新 commit 后恢复
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn get_reports_mapping_unknown_after_a_graph_dropped_commit_until_a_plan_is_committed() {
    let (bytes, graph_id) = plan_bytes();
    let (server, fake) = spawn_with_graph(&graph_id, 4).await;
    let response = support::request_bytes(
        &server,
        "POST",
        "/api/plan/upload",
        "application/octet-stream",
        &bytes,
    )
    .await;
    assert_eq!(response.status, 201, "{}", response.body_text());
    // 引擎格式变化的 commit：rt-host 丢图（graph_dropped=true）。
    let mut dropped = channels_commit_document();
    dropped["graph_dropped"] = json!(true);
    dropped["engine_rebuilt"] = json!(true);
    dropped["generation"] = json!(3);
    fake.push_reply(dropped);
    let commit = put_channels(
        &server,
        json!({ "stage": "commit", "engine_token": "channels-token-1" }),
    )
    .await;
    assert_eq!(commit.status, 200, "{}", commit.body_text());
    assert_eq!(commit.json()["graph_dropped"], true);

    // 丢图后：daemon 记录过的方案 routing 不再代表运行中的图 -> mapping null + 原因。
    let response = support::request(&server, "GET", "/api/channels").await;
    let body = response.json();
    assert!(body["mapping"].is_null(), "{body}");
    assert!(body["output"]["dac_map"].is_null());
    assert_eq!(body["plan"]["routing_known"], false);
    assert_eq!(body["plan"]["routing_invalidated"]["generation"], 3);
    assert!(body["reasons"]["mapping"]
        .as_str()
        .is_some_and(|reason| reason.contains("graph_dropped")));

    // 方案重新 commit（回执带 graph_id）后 routing 恢复可信。
    let commit = support::request_json(
        &server,
        "POST",
        "/api/commit",
        r#"{"engine_token":"plan-token"}"#,
    )
    .await;
    assert_eq!(commit.status, 200, "{}", commit.body_text());
    let response = support::request(&server, "GET", "/api/channels").await;
    let body = response.json();
    assert_eq!(
        body["mapping"],
        json!([{"lane": 0, "dac": 0}, {"lane": 1, "dac": 2}])
    );
    assert_eq!(body["plan"]["routing_known"], true);
    assert!(body["plan"]["routing_invalidated"].is_null());
    assert_eq!(
        body["applied"]["graph_dropped"], true,
        "镜像保留上次丢图事实（历史快照）"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn put_commit_with_a_changed_mapping_reports_plan_recommit_required() {
    let (_, graph_id) = plan_bytes();
    let (server, _) = spawn_with_graph(&graph_id, 4).await;
    upload_plan(&server).await;
    // mapping 改动 + 一个 rt-host 可应用字段：prepare 成功（commit_supported），
    // 但回执明确说 mapping 仍要方案重提交。
    let prepare = put_channels(
        &server,
        json!({
            "stage": "prepare",
            "mapping": [{"lane": 0, "dac": 1}, {"lane": 1, "dac": 3}],
            "aes67_port": 5_006,
        }),
    )
    .await;
    assert_eq!(prepare.status, 200, "{}", prepare.body_text());
    let prepare = prepare.json();
    assert_eq!(prepare["commit_supported"], true);
    assert_eq!(prepare["mapping"]["changed"], true);
    assert!(prepare["warnings"]
        .as_array()
        .is_some_and(|warnings| warnings.iter().any(|warning| warning
            .as_str()
            .is_some_and(|text| text.contains("custom plan") || text.contains("plan routing")))));
    let response = put_channels(
        &server,
        json!({ "stage": "commit", "engine_token": "channels-token-1" }),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let commit = response.json();
    assert_eq!(commit["routing"]["requested"], true);
    assert_eq!(commit["routing"]["changed"], true);
    assert_eq!(commit["routing"]["applied"], false);
    assert_eq!(commit["routing"]["plan_recommit_required"], true);
    assert_eq!(
        commit["routing"]["current"],
        json!([{"lane": 0, "dac": 0}, {"lane": 1, "dac": 2}])
    );
    assert_eq!(
        commit["routing"]["target"],
        json!([{"lane": 0, "dac": 1}, {"lane": 1, "dac": 3}])
    );
}
// ---------------------------------------------------------------------------
// 回环传输：同一份行协议在真实 socket 上跑 channels op
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn channels_ops_work_over_a_real_loopback_socket() {
    use support::host_link_fake::{LoopbackAction, LoopbackServer};

    // 真实 TCP 回环上跑一行 JSON/一行 JSON（与生产 Unix socket 共用 protocol::exchange）。
    let loopback = LoopbackServer::start(|_index, request| {
        let op = request.get("op").and_then(Value::as_str).unwrap_or("");
        let reply = match op {
            "status" => rt_status("loopback-graph", 4),
            "channels_status" => channels_status_document(),
            "channels_commit" => channels_commit_document(),
            "channels_abort" => {
                json!({ "ok": true, "aborted": true, "aborted_token": request["token"] })
            }
            other => json!({
                "ok": false,
                "error": "unknown_op",
                "message": format!("loopback has no reply for {other}"),
            }),
        };
        LoopbackAction::Reply(reply)
    });
    let link = support::injected_link(Arc::new(loopback.transport()));
    let server = support::spawn_with_link(link, None).await;

    let response = support::request(&server, "GET", "/api/channels").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["output"]["channels"], 4);
    assert_eq!(body["input"]["channels"], 2);
    assert_eq!(body["generation"], 2);

    let response = put_channels(
        &server,
        json!({ "stage": "commit", "engine_token": "loopback-token" }),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["applied"], true);
    assert_eq!(body["generation"], 3);
    assert_eq!(body["gap_ms"], 412);
    // 每次逻辑调用一条连接（行协议交换）：回环服务端确实被连过（GET 两次 + commit 一次）。
    assert!(
        loopback.connections() >= 3,
        "connections={}",
        loopback.connections()
    );
}
// ---------------------------------------------------------------------------
// 输出通道增长（webui 的 output.channels + identity dac_map 形态）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn put_prepare_accepts_a_channel_growth_dac_map_beyond_the_running_plan_lanes() {
    let (_, graph_id) = plan_bytes();
    let (server, fake) = spawn_with_graph(&graph_id, 4).await;
    upload_plan(&server).await;
    // 前端把 output.channels 从 4 改成 6 时生成 identity dac_map [0..5]：lane 2..5 超出
    // 当前方案 out_lanes=2，但那是目标方案的槽位 -> prepare 只提示、不拒绝，channels 6 转发。
    let response = put_channels(
        &server,
        json!({
            "stage": "prepare",
            "output": { "channels": 6, "dac_map": [0, 1, 2, 3, 4, 5] },
        }),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let prepare = response.json();
    assert_eq!(prepare["commit_supported"], true);
    assert_eq!(prepare["mapping"]["changed"], true);
    assert!(prepare["warnings"]
        .as_array()
        .is_some_and(|warnings| warnings.iter().any(|warning| warning
            .as_str()
            .is_some_and(|text| text.contains("beyond the running plan out_lanes")))));
    let calls = fake.calls();
    let forwarded = calls
        .iter()
        .find(|call| op_of(call) == "channels_prepare")
        .expect("channels_prepare forwarded");
    assert_eq!(forwarded["channels"], 6);
    // 方案路由达成的目标通道数变化：commit 回执明确要求方案重新提交。
    let commit = put_channels(
        &server,
        json!({ "stage": "commit", "engine_token": "channels-token-1" }),
    )
    .await;
    assert_eq!(commit.status, 200, "{}", commit.body_text());
    let commit = commit.json();
    assert_eq!(commit["routing"]["changed"], true);
    assert_eq!(commit["routing"]["plan_recommit_required"], true);
    assert_eq!(
        commit["routing"]["target"],
        json!([
            {"lane": 0, "dac": 0},
            {"lane": 1, "dac": 1},
            {"lane": 2, "dac": 2},
            {"lane": 3, "dac": 3},
            {"lane": 4, "dac": 4},
            {"lane": 5, "dac": 5}
        ])
    );
}
