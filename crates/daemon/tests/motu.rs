//! T57 MOTU 控制面端到端测试：真实 TCP 打真实 axum 服务 + 注入假 CueMix 控制面（禁真实网络）。
//!
//! 覆盖：检测（未检测/检测但控制网不通/正常）、GET /api/motu 现状与降级、两阶段音量
//! （prepare 不写设备 / commit 单键写 + 回读 / 双通道两个单键写 / token 幂等与替换）、
//! 回读不一致与部分失败如实 502、设备离线时保留草稿、批量拒绝、/api/mute 设备副作用
//! （成功/未检测跳过/失败 502 且保留原控制面回执）、未授权 401 / 缺 CSRF 403、并发。

mod support;

use std::time::Duration;

use serde_json::{json, Value};

use khaen_daemon::auth::SESSION_COOKIE;
use khaen_daemon::motu::device_codes;
use support::motu_fake::{ready_detection, FakeCueMix, InjectedFailure, WriteCall};
use support::{request, request_json, request_with, TestServer};

const VOLUME: &str = "/api/motu/volume";

fn token_of(response: &support::HttpResponse) -> String {
    response.json()["engine_token"]
        .as_str()
        .expect("engine_token in prepare response")
        .to_string()
}

async fn prepare(server: &TestServer, body: &str) -> support::HttpResponse {
    request_json(server, "POST", VOLUME, body).await
}

async fn commit(server: &TestServer, token: &str) -> support::HttpResponse {
    let body = format!(r#"{{"stage":"commit","engine_token":"{token}"}}"#);
    request_json(server, "POST", VOLUME, &body).await
}

fn close(left: f64, right: f64) -> bool {
    (left - right).abs() < 1e-6
}

#[tokio::test(flavor = "current_thread")]
async fn plain_test_servers_never_wire_a_real_device() {
    // 默认测试服务注入 FakeCueMix(no_motu_card)：即使这台机器接着 MOTU，测试也不会写设备属性
    // （T57 的设备写入只在 spawn_with_motu 显式注入假设备时发生）。
    let server = support::spawn().await;
    let body = request(&server, "GET", "/api/motu").await.json();
    assert_eq!(body["detected"], json!(false));
    assert_eq!(body["reason"], json!("no_motu_card"));
    assert_eq!(body["write"]["status"], json!("not_detected"));

    let response = request_json(&server, "POST", "/api/mute", r#"{"mute":true}"#).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert!(
        response.json().get("motu").is_none(),
        "/api/mute must not perform a CueMix side effect"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn status_reports_no_device_without_error() {
    let fake = FakeCueMix::not_detected();
    let server = support::spawn_with_motu(fake.clone()).await;

    let response = request(&server, "GET", "/api/motu").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["schema"], json!("khaen-motu/v1"));
    assert_eq!(body["detected"], json!(false));
    assert_eq!(body["control_reachable"], json!(false));
    assert_eq!(body["model"], Value::Null);
    assert_eq!(body["reason"], json!("cards_unavailable"));
    assert_eq!(body["mute"], Value::Null);
    assert_eq!(body["vol"], Value::Null);
    assert_eq!(body["state"], Value::Null);
    assert_eq!(body["write"]["status"], json!("not_detected"));
    assert_eq!(body["write"]["batch"], json!(false));
    assert_eq!(body["write"]["two_phase"], json!(true));
    assert_eq!(body["channel_count"], json!(2));
    assert_eq!(body["channels"][0]["name"], json!("Main 1"));
    assert_eq!(body["channels"][1]["datastore"]["index"], json!(1));
    assert!(
        body["reasons"]["state"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "未检测到时必须给出原因: {body}"
    );
    assert_eq!(
        fake.read_calls(),
        0,
        "not detected -> never dial the device"
    );
    assert!(fake.writes().is_empty());

    // 未检测到时不暴露写能力：prepare/commit 都 503 + 稳定码，且不写设备。
    let response = prepare(&server, r#"{"stage":"prepare","channel":1,"db":-6}"#).await;
    assert_eq!(response.status, 503);
    assert_eq!(response.json()["error"]["code"], json!("motu_not_detected"));
    let response = commit(&server, "0123456789abcdef").await;
    assert_eq!(response.status, 503);
    assert!(fake.writes().is_empty(), "device must stay untouched");
}

#[tokio::test(flavor = "current_thread")]
async fn status_reports_the_device_state_when_ready() {
    let fake = FakeCueMix::ready();
    fake.set_mute_state(true);
    fake.set_faders([1.0, 0.5]);
    let server = support::spawn_with_motu(fake.clone()).await;

    let response = request(&server, "GET", "/api/motu").await;
    assert_eq!(response.status, 200);
    let body = response.json();
    assert_eq!(body["detected"], json!(true));
    assert_eq!(body["model"], json!("UltraLite-mk5"));
    assert_eq!(body["control_reachable"], json!(true));
    assert_eq!(body["reason"], json!("detected"));
    assert_eq!(body["mute"], json!(true));
    assert_eq!(body["vol"][0]["channel"], json!(1));
    assert_eq!(body["vol"][0]["linear"], json!(1.0));
    assert_eq!(body["vol"][0]["db"], json!(0.0));
    assert!(close(
        body["vol"][1]["linear"].as_f64().expect("linear"),
        0.5
    ));
    let db = body["vol"][1]["db"].as_f64().expect("db");
    assert!((db - -6.0206).abs() < 1e-3, "db = {db}");
    assert_eq!(body["state"]["sample_rate_hz"], json!(48_000));
    assert_eq!(body["state"]["main_trim"], json!(31));
    assert_eq!(body["state"]["device"]["model_id"], json!("UL5L"));
    assert_eq!(body["write"]["status"], json!("wired"));
    assert_eq!(body["reasons"], json!({}));
    assert_eq!(fake.read_calls(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn status_reports_control_unreachable_and_read_failure_honestly() {
    let fake = FakeCueMix::unreachable();
    let server = support::spawn_with_motu(fake.clone()).await;
    let response = request(&server, "GET", "/api/motu").await;
    assert_eq!(response.status, 200);
    let body = response.json();
    assert_eq!(body["detected"], json!(true));
    assert_eq!(body["control_reachable"], json!(false));
    assert_eq!(body["reason"], json!("control_unreachable"));
    assert_eq!(body["mute"], Value::Null);
    assert_eq!(body["write"]["status"], json!("unreachable"));
    assert!(body["reasons"]["state"]
        .as_str()
        .expect("reason")
        .contains("control network"));
    let response = prepare(&server, r#"{"stage":"prepare","channel":1,"linear":0.5}"#).await;
    assert_eq!(response.status, 503);
    assert_eq!(
        response.json()["error"]["code"],
        json!("motu_control_unreachable")
    );

    // 设备在但读失败：字段 null + read_error，仍 200（不冒充"设备没有状态"）。
    let fake = FakeCueMix::ready();
    fake.fail_read(khaen_daemon::motu::MotuError::new(
        device_codes::TIMEOUT,
        "fake readback timed out",
    ));
    let server = support::spawn_with_motu(fake).await;
    let response = request(&server, "GET", "/api/motu").await;
    assert_eq!(response.status, 200);
    let body = response.json();
    assert_eq!(body["detected"], json!(true));
    assert_eq!(body["mute"], Value::Null);
    assert_eq!(body["read_error"]["code"], json!(device_codes::TIMEOUT));
    assert!(body["reasons"]["mute"]
        .as_str()
        .expect("reason")
        .contains("readback timed out"));
}

#[tokio::test(flavor = "current_thread")]
async fn volume_prepare_does_not_write_and_commit_writes_one_channel_with_readback() {
    let fake = FakeCueMix::ready();
    let server = support::spawn_with_motu(fake.clone()).await;

    let response = prepare(&server, r#"{"stage":"prepare","channel":1,"db":-6.0206}"#).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let prepared = response.json();
    assert_eq!(prepared["stage"], json!("prepare"));
    assert_eq!(prepared["writes_device"], json!(false));
    assert_eq!(prepared["target"]["channels"][0]["channel"], json!(1));
    assert_eq!(prepared["target"]["channels"][0]["name"], json!("Main 1"));
    assert_eq!(prepared["write_plan"]["batch"], json!(false));
    assert_eq!(prepared["write_plan"]["writes"], json!(1));
    assert!(
        prepared["commit_supported"].as_bool().unwrap_or(false),
        "两阶段必须真的可 commit"
    );
    assert_eq!(fake.write_count(), 0, "prepare must not write the device");
    let token = token_of(&response);

    let response = commit(&server, &token).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let committed = response.json();
    assert_eq!(committed["stage"], json!("commit"));
    assert_eq!(committed["applied"], json!(true));
    assert_eq!(committed["writes_device"], json!(true));
    assert_eq!(committed["idempotent"], json!(false));
    let channel = &committed["channels"][0];
    assert_eq!(channel["channel"], json!(1));
    assert_eq!(channel["name"], json!("Main 1"));
    assert_eq!(channel["index"], json!(0));
    assert_eq!(channel["before"], json!(1.0));
    assert_eq!(channel["verified"], json!(true));
    assert_eq!(channel["frame_sent"], json!(true));
    let wrote = channel["wrote"].as_f64().expect("wrote");
    assert!(close(wrote, 0.5), "wrote = {wrote}");
    assert!(close(
        channel["readback"].as_f64().expect("readback"),
        wrote
    ));

    let writes = fake.writes();
    assert_eq!(writes.len(), 1, "one single-key write: {writes:?}");
    match writes[0] {
        WriteCall::Volume {
            channel_index,
            linear,
        } => {
            assert_eq!(channel_index, 0);
            assert!(close(linear, wrote));
        }
        other => panic!("expected a volume write, got {other:?}"),
    }
    assert!(close(fake.fader(0), wrote));
    assert_eq!(fake.fader(1), 1.0, "Main 2 must stay untouched");

    // 设备现状从真实读路径反映（不编数值）。
    let status = request(&server, "GET", "/api/motu").await.json();
    assert!(close(
        status["vol"][0]["linear"].as_f64().expect("linear"),
        wrote
    ));
    assert_eq!(status["mute"], json!(false));

    // 目标值已在位：再 prepare+commit 是幂等的，不发写帧。
    let again =
        token_of(&prepare(&server, r#"{"stage":"prepare","channel":1,"db":-6.0206}"#).await);
    let response = commit(&server, &again).await;
    assert_eq!(response.status, 200);
    let body = response.json();
    assert_eq!(body["idempotent"], json!(true));
    assert_eq!(body["channels"][0]["frame_sent"], json!(false));
    assert_eq!(body["channels"][0]["changed"], json!(false));
}

#[tokio::test(flavor = "current_thread")]
async fn volume_commit_of_both_channels_is_two_single_key_writes() {
    let fake = FakeCueMix::ready();
    let server = support::spawn_with_motu(fake.clone()).await;

    let token = token_of(&prepare(&server, r#"{"channel":"both","linear":0.5}"#).await);
    let response = commit(&server, &token).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["channels"][0]["channel"], json!(1));
    assert_eq!(body["channels"][1]["channel"], json!(2));
    assert_eq!(body["channels"][1]["index"], json!(1));
    assert_eq!(body["channels"][1]["verified"], json!(true));
    let writes = fake.writes();
    assert_eq!(
        writes.len(),
        2,
        "two separate single-key writes: {writes:?}"
    );
    match (writes[0], writes[1]) {
        (
            WriteCall::Volume {
                channel_index: first_index,
                linear: first_linear,
            },
            WriteCall::Volume {
                channel_index: second_index,
                linear: second_linear,
            },
        ) => {
            assert_eq!(first_index, 0, "Main 1 written first");
            assert_eq!(second_index, 1);
            assert!(close(first_linear, 0.5));
            assert!(close(second_linear, 0.5));
        }
        other => panic!("expected two volume writes, got {other:?}"),
    }
    assert!(close(fake.fader(0), 0.5));
    assert!(close(fake.fader(1), 0.5));

    // PUT 与 POST 同语义（T54 描述用 PUT，T57 定稿 POST；两者都支持）。
    let response = request_json(&server, "PUT", VOLUME, r#"{"channel":2,"linear":0.25}"#).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let token = token_of(&response);
    let response = request_json(
        &server,
        "PUT",
        VOLUME,
        &format!(r#"{{"stage":"commit","engine_token":"{token}"}}"#),
    )
    .await;
    assert_eq!(response.status, 200);
    assert_eq!(response.json()["channels"][0]["channel"], json!(2));
    assert!(close(fake.fader(1), 0.25));
    assert!(close(fake.fader(0), 0.5), "Main 1 untouched by channel 2");
}

#[tokio::test(flavor = "current_thread")]
async fn volume_prepare_is_idempotent_and_a_new_target_invalidates_the_old_token() {
    let fake = FakeCueMix::ready();
    let server = support::spawn_with_motu(fake.clone()).await;

    let first = prepare(&server, r#"{"channel":1,"db":-6}"#).await;
    let first_token = token_of(&first);
    let second = prepare(&server, r#"{"channel":1,"db":-6}"#).await;
    assert_eq!(token_of(&second), first_token, "same target -> same token");

    let replaced = prepare(&server, r#"{"channel":2,"linear":0.25}"#).await;
    let replaced_token = token_of(&replaced);
    assert_ne!(replaced_token, first_token);

    let response = commit(&server, &first_token).await;
    assert_eq!(response.status, 409);
    assert_eq!(
        response.json()["error"]["code"],
        json!("engine_token_invalid")
    );
    assert!(fake.writes().is_empty());

    let response = commit(&server, &replaced_token).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert!(close(fake.fader(1), 0.25));
    assert_eq!(fake.fader(0), 1.0, "Main 1 untouched");

    // 草稿一次性：commit 后同 token 再 commit -> 409。
    let response = commit(&server, &replaced_token).await;
    assert_eq!(response.status, 409);
    assert_eq!(
        response.json()["error"]["code"],
        json!("engine_token_invalid")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn volume_rejects_batch_and_invalid_requests_without_touching_the_device() {
    let fake = FakeCueMix::ready();
    let server = support::spawn_with_motu(fake.clone()).await;

    // 批量写：400 + 稳定码 motu_batch_rejected。
    let response = prepare(&server, r#"{"channels":[1,2,3],"linear":0.5}"#).await;
    assert_eq!(response.status, 400);
    assert_eq!(
        response.json()["error"]["code"],
        json!("motu_batch_rejected")
    );

    for body in [
        r#"{"channels":[],"linear":0.5}"#,
        r#"{"channel":3,"linear":0.5}"#,
        r#"{"channels":[1,3],"linear":0.5}"#,
        r#"{"channel":1,"channels":[1],"linear":0.5}"#,
        r#"{"channel":1,"db":-6,"linear":0.5}"#,
        r#"{"channel":1}"#,
        r#"{"linear":0.5}"#,
        r#"{"channel":1,"linear":2.0}"#,
        r#"{"channel":1,"db":3.0}"#,
        r#"{"channel":1,"db":"loud"}"#,
        r#"{"stage":"apply","channel":1,"db":-6}"#,
    ] {
        let response = prepare(&server, body).await;
        assert_eq!(response.status, 400, "{body}: {}", response.body_text());
        assert_eq!(
            response.json()["error"]["code"],
            json!("invalid_request"),
            "{body}"
        );
    }

    // commit 的 token 校验。
    let response = request_json(&server, "POST", VOLUME, r#"{"stage":"commit"}"#).await;
    assert_eq!(response.status, 400);
    let response = commit(&server, "0000000000000000").await;
    assert_eq!(response.status, 409);
    assert_eq!(
        response.json()["error"]["code"],
        json!("engine_token_invalid")
    );

    assert!(
        fake.writes().is_empty(),
        "no invalid request may write the device"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn volume_readback_mismatch_is_502_and_never_reported_as_success() {
    let fake = FakeCueMix::ready();
    let server = support::spawn_with_motu(fake.clone()).await;

    let token = token_of(&prepare(&server, r#"{"channel":2,"db":-6}"#).await);
    // 设备"接受"了写但回读是另一个值：honest 502 + 设备错误码。
    fake.fail_volume_on(
        1,
        InjectedFailure::applied(
            khaen_daemon::motu::MotuError::new(
                device_codes::READBACK_MISMATCH,
                "wrote 0.501187 but device reports 0.99",
            ),
            0.99,
        ),
    );
    let response = commit(&server, &token).await;
    assert_eq!(response.status, 502, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], json!(false));
    assert_eq!(
        body["error"]["code"],
        json!(device_codes::READBACK_MISMATCH)
    );
    assert_eq!(body["error"]["details"]["channel"], json!(2));
    assert_eq!(body["error"]["details"]["completed"], json!([]));
    assert_eq!(
        body["error"]["details"]["motu"]["code"],
        json!(device_codes::READBACK_MISMATCH)
    );
    assert!(
        body.get("channels").is_none(),
        "no fake success payload: {body}"
    );

    // 设备实际值就是这个错误值：GET 如实反映（不把 wrote 当现状）。
    let status = request(&server, "GET", "/api/motu").await.json();
    assert!(close(
        status["vol"][1]["linear"].as_f64().expect("linear"),
        0.99
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn volume_partial_failure_reports_the_channels_it_verified() {
    let fake = FakeCueMix::ready();
    let server = support::spawn_with_motu(fake.clone()).await;

    let token = token_of(&prepare(&server, r#"{"channel":"both","db":-6}"#).await);
    fake.fail_volume_on(
        1,
        InjectedFailure::error(khaen_daemon::motu::MotuError::new(
            device_codes::READBACK_TIMEOUT,
            "device did not echo koBusFader[1]",
        )),
    );
    let response = commit(&server, &token).await;
    assert_eq!(response.status, 502, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["error"]["code"], json!(device_codes::READBACK_TIMEOUT));
    let completed = body["error"]["details"]["completed"]
        .as_array()
        .expect("completed array");
    assert_eq!(completed.len(), 1, "only Main 1 was verified: {body}");
    assert_eq!(completed[0]["channel"], json!(1));
    assert_eq!(completed[0]["verified"], json!(true));
    assert_eq!(body["error"]["details"]["writes_device"], json!(true));
    let first_wrote = completed[0]["wrote"].as_f64().expect("wrote");
    assert!(
        close(fake.fader(0), first_wrote),
        "first channel really changed"
    );
    assert_eq!(fake.fader(1), 1.0, "second channel untouched");
}

#[tokio::test(flavor = "current_thread")]
async fn volume_commit_keeps_the_draft_when_the_device_disappears() {
    let fake = FakeCueMix::ready();
    let server = support::spawn_with_motu(fake.clone()).await;
    let token = token_of(&prepare(&server, r#"{"channel":1,"linear":0.5}"#).await);

    let mut unreachable = ready_detection("UltraLite-mk5");
    unreachable.control_reachable = false;
    unreachable.reason = khaen_daemon::motu::REASON_CONTROL_UNREACHABLE;
    fake.set_detection(unreachable);
    let response = commit(&server, &token).await;
    assert_eq!(response.status, 503);
    assert_eq!(
        response.json()["error"]["code"],
        json!("motu_control_unreachable")
    );
    assert!(fake.writes().is_empty());

    // 设备恢复后同一 token 还能 commit（门禁失败不消费草稿）。
    fake.set_detection(ready_detection("UltraLite-mk5"));
    let response = commit(&server, &token).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert!(close(fake.fader(0), 0.5));
}

#[tokio::test(flavor = "current_thread")]
async fn mute_is_software_only_and_never_writes_cuemix() {
    let fake = FakeCueMix::ready();
    let server = support::spawn_with_motu(fake.clone()).await;

    for mute in [true, false] {
        let body = json!({ "mute": mute });
        let response = request_json(&server, "POST", "/api/mute", &body.to_string()).await;
        assert_eq!(response.status, 200, "{}", response.body_text());
        let body = response.json();
        assert_eq!(body["mute"], json!(mute));
        assert_eq!(body["target"], json!("daemon-process"));
        assert!(
            body.get("motu").is_none(),
            "mute must not report or perform a CueMix operation: {body}"
        );
    }
    assert!(
        fake.writes().is_empty(),
        "mute/umute must never write CueMix: {:?}",
        fake.writes()
    );
    assert!(!fake.mute_state(), "device mute state must stay untouched");
}

#[tokio::test(flavor = "current_thread")]
async fn motu_endpoints_require_a_session_and_csrf() {
    let fake = FakeCueMix::ready();
    let dir = support::TempStateDir::new("motu-auth");
    let mut server = support::spawn_at_with_motu(dir.path(), fake.clone()).await;

    // 匿名：401（/api/motu 不是公开路径）。
    let response = request_with(&server, "GET", "/api/motu", &[], None).await;
    assert_eq!(response.status, 401);
    let response = request_with(
        &server,
        "POST",
        VOLUME,
        &[],
        Some(r#"{"stage":"prepare","channel":1,"db":-6}"#),
    )
    .await;
    assert_eq!(response.status, 401);

    server.sign_in().await;
    let session = server.session().expect("session").clone();
    let cookie = vec![(
        "Cookie".to_string(),
        format!("{SESSION_COOKIE}={}", session.token),
    )];
    // 已登录 GET 可以，但写方法缺 CSRF -> 403。
    let response = request_with(&server, "GET", "/api/motu", &cookie, None).await;
    assert_eq!(response.status, 200);
    let response = request_with(
        &server,
        "POST",
        VOLUME,
        &cookie,
        Some(r#"{"stage":"prepare","channel":1,"db":-6}"#),
    )
    .await;
    assert_eq!(response.status, 403);
    assert_eq!(response.json()["error"]["code"], json!("forbidden"));
    assert!(fake.writes().is_empty());

    // 带 CSRF（Cookie 会话）才通过。
    let response = prepare(&server, r#"{"stage":"prepare","channel":1,"db":-6}"#).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_commits_with_the_same_token_only_one_wins() {
    let fake = FakeCueMix::ready();
    let server = support::spawn_with_motu(fake.clone()).await;
    let token = token_of(&prepare(&server, r#"{"channel":1,"linear":0.5}"#).await);
    fake.set_slow(Duration::from_millis(60));

    let body = format!(r#"{{"stage":"commit","engine_token":"{token}"}}"#);
    let (first, second) = tokio::join!(
        request_json(&server, "POST", VOLUME, &body),
        request_json(&server, "POST", VOLUME, &body),
    );
    let mut statuses = [first.status, second.status];
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 409], "token is single-use under races");
    assert!(
        fake.max_concurrent() >= 2,
        "the test must actually exercise concurrency (max {})",
        fake.max_concurrent()
    );
    assert_eq!(fake.write_count(), 1, "only the winning commit wrote");
    assert!(close(fake.fader(0), 0.5));
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_mute_requests_are_software_only() {
    let server = support::spawn().await;
    let (first, second) = tokio::join!(
        request_json(&server, "POST", "/api/mute", r#"{"mute":true}"#),
        request_json(&server, "POST", "/api/mute", r#"{"mute":true}"#),
    );
    for response in [&first, &second] {
        assert_eq!(response.status, 200, "{}", response.body_text());
        let body = response.json();
        assert_eq!(body["mute"], json!(true));
        assert!(body.get("motu").is_none());
    }
}
