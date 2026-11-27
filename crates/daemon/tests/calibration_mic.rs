//! 真实校准桥接 C1/C3a：MicProfile + UMIK 校准文件 + 实时 UMIK 状态端到端测试
//! （真实 TCP + 真实 axum 服务 + 临时状态目录）。
//!
//! 覆盖：GET/status 无 profile 的诚实降级（200 + null + reasons）、
//! POST 真实 miniDSP UMIK 格式（首行 Sens Factor + SERNO、引号注释、TAB 两列）解析与内容寻址存储、
//! 非法文件/未知字段/串号不符/朝向非法/超大文件的稳定错误码、PUT 首次保存与部分更新、
//! 重启后仍可读（mic.json 原子写）、0600 权限（Windows 跳过）、鉴权/CSRF；
//! C3a：status 的 `rt_host` 段经既有 host link 转发 `{op:"cal_status"}`（脚本化假传输），
//! 逐字段映射真实回执（declared/open/device/declaration_source/mode/calibration），
//! 缺失字段如实 null，rt-host 未接线/链路不可用/回执畸形一律 available:false + 稳定 code，
//! HTTP 恒 200 且 profile/reasons 不受影响。

mod support;

use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};

use khaen_daemon::calibration::api::{MALFORMED_CAL_STATUS, PROCESS_TARGET_MIC_REASON};
use khaen_daemon::host_link::{TransportError, HOST_LINK_UNAVAILABLE};
use khaen_host::sha256_hex;

use support::host_link_fake::FakeTransport;

const MIC: &str = "/api/calibration/mic";
const MIC_STATUS: &str = "/api/calibration/mic/status";
const MIC_CAL: &str = "/api/calibration/mic/calibration";

const SERNO: &str = "7187696";
const DEVICE: &str = "hw:CARD=U18dB,DEV=0";

/// 真实 miniDSP UMIK-1 校准文件的最小复刻（与仓库外 `calibration-spec/mics/umik1_718-7696_90deg.txt` 同格式）。
const REAL_CAL: &str = concat!(
    "\"Sens Factor =-0.0109999dB, SERNO: 7187696\"\n",
    "\"Auto-generated 90-degree calibration file\"\n",
    "10.054\t-4.7795\n",
    "10.179\t-4.6166\n",
    "10.306\t-4.4569\n",
    "10.434\t-4.3004\n",
    "10.564\t-4.1471\n"
);

fn mic_dir(state_dir: &Path) -> std::path::PathBuf {
    state_dir.join("calibration")
}

fn cal_file(state_dir: &Path, sha256: &str) -> std::path::PathBuf {
    mic_dir(state_dir)
        .join("mics")
        .join(format!("{sha256}.txt"))
}

fn assert_error_code(body: &Value, expected_code: &str) {
    let error = body
        .get("error")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("error body must be an object: {body}"));
    assert_eq!(
        error.len(),
        3,
        "M1 error body must be exactly {{code,message,details}}: {body}"
    );
    assert_eq!(error["code"], json!(expected_code), "{body}");
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "error.message must be a non-empty string: {body}"
    );
    assert!(error["details"].is_object(), "{body}");
}

async fn get_mic(server: &support::TestServer) -> Value {
    let response = support::request(server, "GET", MIC).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    response.json()
}

async fn get_mic_status(server: &support::TestServer) -> Value {
    let response = support::request(server, "GET", MIC_STATUS).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    response.json()
}

async fn put_mic(server: &support::TestServer, body: &Value) -> support::HttpResponse {
    support::request_json(server, "PUT", MIC, &body.to_string()).await
}

fn cal_headers(server: &support::TestServer, filename: Option<&str>) -> Vec<(String, String)> {
    let session = server.session().expect("session").clone();
    let mut headers = vec![
        (
            "Cookie".to_string(),
            format!("khaen_session={}", session.token),
        ),
        ("X-KHAEN-CSRF".to_string(), session.csrf),
        (
            "Content-Type".to_string(),
            "text/plain; charset=utf-8".to_string(),
        ),
    ];
    if let Some(filename) = filename {
        headers.push(("x-khaen-filename".to_string(), filename.to_string()));
    }
    headers
}

async fn post_cal(
    server: &support::TestServer,
    filename: Option<&str>,
    bytes: &[u8],
) -> support::HttpResponse {
    let headers = cal_headers(server, filename);
    support::request_bytes_with(server, "POST", MIC_CAL, &headers, Some(bytes)).await
}

async fn upload_cal(server: &support::TestServer) -> Value {
    let response = post_cal(
        server,
        Some("umik1_718-7696_90deg.txt"),
        REAL_CAL.as_bytes(),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    response.json()
}

// ---------------------------------------------------------------------------
// C3a：status 的 rt_host 段接真实 rt-host cal_status（脚本化假传输，全离线）
// ---------------------------------------------------------------------------

/// rt-host `cal_status` 的成功回执（形状与 `crates/rt-host/src/control/cal/mod.rs`
/// 的 `status_json` 同源；值固定便于逐字段断言）。
fn cal_status_document() -> Value {
    json!({
        "ok": true,
        "device": null,
        "mode": "calibration",
        "capture_source": "cal",
        "program_input_reaches_dsp": false,
        "playback_restored": false,
        "simulated": false,
        "engine": "in-process",
        "real_capture": true,
        "umik": {
            "open": true,
            "device": "hw:CARD=U18dB,DEV=0",
            "capture_channels": 2,
            "capture_channel": 0,
            "declared": true,
            "declaration_source": "cal_enter_override",
            "declaration": {
                "model": "UMIK-1",
                "serial": SERNO,
                "cal_file": "umik1_718-7696_90deg.txt",
                "cal_sha256": "0".repeat(64),
                "orientation": "90deg",
                "clock_domain": "usb",
                "sample_rate_hz": 48_000,
            },
            "missing_reason": null,
        },
        "reasons": {},
    })
}

/// 起一个注入“下一次 cal_status 只回这一份回执”的假传输服务的 daemon。
async fn spawn_with_cal_status(reply: Value) -> (support::TestServer, Arc<FakeTransport>) {
    let transport = FakeTransport::new();
    transport.push_reply(reply);
    let server =
        support::spawn_with_link(support::injected_link(Arc::clone(&transport)), None).await;
    (server, transport)
}

/// 走 C1 的上传 + PUT 保存一份 profile，返回保存后的 profile JSON。
async fn save_test_profile(server: &support::TestServer) -> Value {
    let uploaded = upload_cal(server).await;
    let sha256 = uploaded["sha256"].as_str().expect("sha256").to_string();
    let response = put_mic(
        server,
        &json!({
            "device": DEVICE,
            "serial": SERNO,
            "orientation": "90deg",
            "cal_sha256": sha256,
        }),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    response.json()["profile"].clone()
}

#[tokio::test(flavor = "current_thread")]
async fn status_maps_a_live_cal_status_reply_field_by_field() {
    let (server, transport) = spawn_with_cal_status(cal_status_document()).await;
    let profile = save_test_profile(&server).await;

    let status = get_mic_status(&server).await;
    assert_eq!(status["ok"], json!(true), "{status}");
    assert_eq!(status["schema"], json!("khaen-mic-profile/v1"));
    assert_eq!(status["profile"], profile, "profile 仍来自 MicProfileStore");
    assert_eq!(status["reasons"], json!([]), "{status}");

    let rt = &status["rt_host"];
    assert_eq!(rt["available"], json!(true), "{status}");
    assert!(
        rt["reason"].is_null(),
        "可用时 reason 必须是 null: {status}"
    );
    assert_eq!(rt["mode"], json!("calibration"), "{status}");
    assert_eq!(rt["declared"], json!(true), "{status}");
    assert_eq!(rt["open"], json!(true), "{status}");
    assert_eq!(rt["device"], json!("hw:CARD=U18dB,DEV=0"), "{status}");
    assert_eq!(
        rt["declaration_source"],
        json!("cal_enter_override"),
        "{status}"
    );
    assert!(rt["reasons"].is_null(), "没有 UMIK 缺失原因: {status}");
    assert_eq!(
        rt["calibration"],
        json!({"simulated": false, "engine": "in-process"}),
        "{status}"
    );
    // 只发一条只读 cal_status；daemon 不触发任何测量/校准写 op。
    assert_eq!(transport.calls(), vec![json!({"op": "cal_status"})]);
}

#[tokio::test(flavor = "current_thread")]
async fn status_maps_a_missing_umik_declaration_without_faking_it() {
    let missing = "UMIK declaration missing: set KHAEN_UMIK_SERIAL, KHAEN_UMIK_CAL_FILE and KHAEN_UMIK_CAL_SHA256";
    let mut reply = cal_status_document();
    reply["mode"] = json!("playback");
    reply["simulated"] = json!(true);
    reply["engine"] = json!("fake");
    reply["umik"]["open"] = json!(false);
    reply["umik"]["declared"] = json!(false);
    reply["umik"]["device"] = json!("hw:0,0");
    reply["umik"]["declaration_source"] = json!("missing");
    reply["umik"]["declaration"] = Value::Null;
    reply["umik"]["missing_reason"] = json!(missing);
    reply["reasons"] = json!({ "mic": missing });
    let (server, _transport) = spawn_with_cal_status(reply).await;

    let status = get_mic_status(&server).await;
    // rt-host 可达（available:true）与“UMIK 未声明”是两件事，不能混成一个假值。
    let rt = &status["rt_host"];
    assert_eq!(rt["available"], json!(true), "{status}");
    assert_eq!(rt["mode"], json!("playback"), "{status}");
    assert_eq!(rt["declared"], json!(false), "{status}");
    assert_eq!(rt["open"], json!(false), "{status}");
    assert_eq!(rt["device"], json!("hw:0,0"), "{status}");
    assert_eq!(rt["declaration_source"], json!("missing"), "{status}");
    assert_eq!(rt["reasons"], json!(missing), "{status}");
    assert_eq!(
        rt["calibration"],
        json!({"simulated": true, "engine": "fake"}),
        "{status}"
    );
    // 没有保存 profile 时 profile 段仍如实为空（rt_host 可用不冒充 profile）。
    assert!(status["profile"].is_null(), "{status}");
    assert!(
        status["reasons"]
            .as_array()
            .expect("reasons")
            .iter()
            .filter_map(Value::as_str)
            .any(|reason| reason.contains("no microphone profile saved yet")),
        "{status}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn status_maps_missing_cal_status_fields_as_null() {
    let (server, _transport) = spawn_with_cal_status(json!({ "ok": true, "umik": {} })).await;

    let status = get_mic_status(&server).await;
    let rt = &status["rt_host"];
    assert_eq!(rt["available"], json!(true), "{status}");
    for field in [
        "mode",
        "declared",
        "open",
        "device",
        "declaration_source",
        "reasons",
    ] {
        assert!(
            rt[field].is_null(),
            "rt_host.{field} 缺失时必须如实 null: {status}"
        );
    }
    assert_eq!(
        rt["calibration"],
        json!({"simulated": null, "engine": null}),
        "{status}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn status_degrades_when_rt_host_does_not_wire_calibration() {
    for code in ["unsupported", "not_wired"] {
        let transport = FakeTransport::new();
        let message = format!("calibration ops are not wired in this process ({code})");
        transport.push_error_reply(code, &message);
        let server =
            support::spawn_with_link(support::injected_link(Arc::clone(&transport)), None).await;

        let status = get_mic_status(&server).await;
        assert_eq!(status["ok"], json!(true), "{status}");
        let rt = &status["rt_host"];
        assert_eq!(rt["available"], json!(false), "{status}");
        assert_eq!(rt["code"], json!(code), "{status}");
        let reason = rt["reason"].as_str().expect("rt_host.reason");
        assert!(reason.contains(code), "{reason}");
        assert!(reason.contains(&message), "{reason}");
        for field in [
            "declared",
            "open",
            "device",
            "declaration_source",
            "mode",
            "calibration",
            "reasons",
        ] {
            assert!(
                rt.get(field).is_none(),
                "不可用回执不能编造 rt_host.{field}: {status}"
            );
        }
        assert_eq!(transport.call_count(), 1, "rt-host 业务错误不重试");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn status_degrades_when_the_control_socket_is_unavailable() {
    let transport = FakeTransport::new();
    transport.push_transport_error(TransportError::Connect("no listener".to_string()));
    transport.push_transport_error(TransportError::Connect("no listener".to_string()));
    let server =
        support::spawn_with_link(support::injected_link(Arc::clone(&transport)), None).await;

    let status = get_mic_status(&server).await;
    assert_eq!(status["ok"], json!(true), "{status}");
    let rt = &status["rt_host"];
    assert_eq!(rt["available"], json!(false), "{status}");
    assert_eq!(rt["code"], json!(HOST_LINK_UNAVAILABLE), "{status}");
    let reason = rt["reason"].as_str().expect("rt_host.reason");
    assert!(reason.contains(HOST_LINK_UNAVAILABLE), "{reason}");
    assert!(reason.contains("cal_status"), "{reason}");
    assert!(
        reason.contains("2 attempt"),
        "连接失败按既有策略重试一次: {reason}"
    );
    assert_eq!(
        transport.calls(),
        vec![json!({"op": "cal_status"}), json!({"op": "cal_status"})],
        "重试的是同一条只读 cal_status"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn status_degrades_on_a_malformed_cal_status_reply() {
    for reply in [
        json!({"ok": true}),
        json!({"ok": true, "umik": "not-an-object"}),
        json!({"ok": true, "umik": {}, "reasons": []}),
    ] {
        let (server, _transport) = spawn_with_cal_status(reply.clone()).await;
        let status = get_mic_status(&server).await;
        assert_eq!(status["ok"], json!(true), "{reply}: {status}");
        let rt = &status["rt_host"];
        assert_eq!(rt["available"], json!(false), "{reply}: {status}");
        assert_eq!(rt["code"], json!(MALFORMED_CAL_STATUS), "{reply}: {status}");
        let reason = rt["reason"].as_str().expect("rt_host.reason");
        assert!(reason.contains(MALFORMED_CAL_STATUS), "{reply}: {reason}");
        assert!(!reason.is_empty());
        assert!(
            rt.get("declared").is_none(),
            "畸形回执不能编造 declared: {reply}: {status}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn get_and_status_report_missing_profile_honestly() {
    let server = support::spawn().await;

    let body = get_mic(&server).await;
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["schema"], json!("khaen-mic-profile/v1"));
    assert!(body["profile"].is_null(), "{body}");
    let reasons = body["reasons"].as_array().expect("reasons array");
    assert!(
        !reasons.is_empty(),
        "missing profile must come with a reason"
    );
    assert!(
        reasons
            .iter()
            .filter_map(Value::as_str)
            .any(|reason| reason.contains("no microphone profile saved yet")),
        "{body}"
    );
    assert!(
        reasons.iter().all(Value::is_string),
        "reasons must be an array of strings: {body}"
    );

    let status = get_mic_status(&server).await;
    assert_eq!(status["ok"], json!(true));
    assert_eq!(status["schema"], json!("khaen-mic-profile/v1"));
    assert!(status["profile"].is_null());
    assert!(!status["reasons"].as_array().expect("reasons").is_empty());
    // 默认测试服务是 process 目标：没有任何校准能力，如实降级（不碰 socket、不编 declared）。
    assert_eq!(status["rt_host"]["available"], json!(false));
    assert_eq!(
        status["rt_host"]["code"],
        json!("process_target"),
        "{status}"
    );
    assert_eq!(
        status["rt_host"]["reason"],
        json!(PROCESS_TARGET_MIC_REASON),
        "{status}"
    );
    for field in ["declared", "open", "device", "declaration_source", "mode"] {
        assert!(
            status["rt_host"].get(field).is_none(),
            "process 目标不能编造 rt_host.{field}: {status}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn upload_parses_real_umik_file_and_stores_it_content_addressed() {
    let server = support::spawn().await;
    let body = upload_cal(&server).await;
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["sha256"], json!(sha256_hex(REAL_CAL.as_bytes())));
    assert_eq!(body["filename"], json!("umik1_718-7696_90deg.txt"));
    assert_eq!(body["bytes"], json!(REAL_CAL.len() as u64));
    assert_eq!(body["serial"], json!(SERNO));
    assert_eq!(body["parsed"]["serno"], json!(SERNO));
    assert_eq!(body["parsed"]["points"], json!(5));
    let sens = body["parsed"]["sens_factor_db"]
        .as_f64()
        .expect("sens factor from header");
    assert!((sens - -0.0109999).abs() < 1e-12, "{sens}");

    // 内容寻址：<state_dir>/calibration/mics/<sha256>.txt 与原始字节逐字节一致。
    let sha256 = sha256_hex(REAL_CAL.as_bytes());
    let path = cal_file(&server.state_dir, &sha256);
    assert!(path.is_file(), "calibration file at {}", path.display());
    assert_eq!(
        std::fs::read(&path).expect("read cal file"),
        REAL_CAL.as_bytes()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "calibration file must be 0600");
    }

    // 同内容重复上传：幂等（同 sha256）；缺 X-KHAEN-FILENAME 时文件名默认 umik.txt。
    let again = post_cal(&server, None, REAL_CAL.as_bytes()).await;
    assert_eq!(again.status, 200, "{}", again.body_text());
    let again = again.json();
    assert_eq!(again["sha256"], json!(sha256));
    assert_eq!(again["filename"], json!("umik.txt"));
    assert_eq!(again["parsed"]["points"], json!(5));

    // 客户端文件名只做展示：恶意路径被拒，且不可能影响 auth.json。
    let traversal = post_cal(&server, Some("../../auth.json"), REAL_CAL.as_bytes()).await;
    assert_eq!(traversal.status, 400, "{}", traversal.body_text());
    assert_error_code(&traversal.json(), "calibration_mic_invalid");
    assert!(
        server.state_dir.join("auth.json").is_file(),
        "auth.json 不该被动过"
    );
    let entries: Vec<String> = std::fs::read_dir(mic_dir(&server.state_dir).join("mics"))
        .expect("mics dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        entries,
        vec![format!("{sha256}.json"), format!("{sha256}.txt")],
        "只应按 sha256 内容寻址落盘"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn upload_rejects_invalid_files_with_stable_codes() {
    let server = support::spawn().await;
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("bom", b"\xEF\xBB\xBFSERNO: 1\n20 0\n".to_vec()),
        ("invalid utf8", b"SERNO: 1\n20 0\n\xFF".to_vec()),
        ("no serno", b"1000.000000 0.011\n2000 0.0\n".to_vec()),
        ("no data", b"SERNO: 123\n\"comment\"\n".to_vec()),
        ("three columns", b"SERNO: 1\n1000 0.0 7\n".to_vec()),
        ("zero frequency", b"SERNO: 1\n0 0.0\n".to_vec()),
        (
            "bad sens factor",
            b"Sens Factor =louddB, SERNO: 1\n20 0\n".to_vec(),
        ),
    ];
    for (name, bytes) in cases {
        let response = post_cal(&server, None, &bytes).await;
        assert_eq!(response.status, 400, "{name}: {}", response.body_text());
        assert_error_code(&response.json(), "calibration_mic_invalid");
    }
    // 非法文件不落盘。
    assert!(
        !mic_dir(&server.state_dir).join("mics").is_dir(),
        "invalid calibration files must not be stored"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn upload_rejects_oversize_with_413() {
    let server = support::spawn().await;
    let too_large = khaen_daemon::calibration::mic::MAX_CAL_BYTES + 1;
    assert_eq!(khaen_daemon::calibration::mic::MAX_CAL_BYTES, 1024 * 1024);
    let mut headers = cal_headers(&server, None);
    headers.push(("Content-Length".to_string(), too_large.to_string()));
    let response = support::request_bytes_with(&server, "POST", MIC_CAL, &headers, None).await;
    assert_eq!(response.status, 413, "{}", response.body_text());
    let body = response.json();
    assert_error_code(&body, "calibration_mic_too_large");
    assert_eq!(
        body["error"]["details"]["limit_bytes"],
        json!(too_large - 1)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn put_saves_profile_and_get_returns_it_after_restart() {
    let dir = support::TempStateDir::new("mic-restart");
    let mut first = support::spawn_at(dir.path()).await;
    first.sign_in().await;
    let uploaded = upload_cal(&first).await;
    let sha256 = uploaded["sha256"].as_str().expect("sha256").to_string();

    let expected_profile = json!({
        "schema": "khaen-mic-profile/v1",
        "device": DEVICE,
        "serial": SERNO,
        "orientation": "90deg",
        "cal_sha256": sha256,
        "cal_filename": "umik1_718-7696_90deg.txt",
        "cal_path": cal_file(&first.state_dir, &sha256).to_string_lossy(),
        "parsed": {
            "serno": SERNO,
            "sens_factor_db": -0.0109999,
            "points": 5,
        },
    });

    let response = put_mic(
        &first,
        &json!({
            "device": DEVICE,
            "serial": SERNO,
            "orientation": "90deg",
            "cal_sha256": sha256,
        }),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["schema"], json!("khaen-mic-profile/v1"));
    assert_eq!(body["reasons"], json!([]));
    let profile = &body["profile"];
    for (key, value) in expected_profile.as_object().expect("object") {
        assert_eq!(&profile[key], value, "profile.{key}");
    }
    let parsed_sens = profile["parsed"]["sens_factor_db"]
        .as_f64()
        .expect("parsed.sens_factor_db");
    assert!((parsed_sens - -0.0109999).abs() < 1e-12);
    let updated_at = profile["updated_at"].as_str().expect("updated_at");
    assert!(updated_at.ends_with('Z'), "{updated_at}");

    // 磁盘：<state_dir>/calibration/mic.json（0600，schema 齐全）。
    let profile_path = mic_dir(&first.state_dir).join("mic.json");
    assert!(profile_path.is_file(), "{}", profile_path.display());
    let document: Value =
        serde_json::from_slice(&std::fs::read(&profile_path).expect("read mic.json"))
            .expect("mic.json is JSON");
    assert_eq!(document["schema"], json!("khaen-mic-profile/v1"));
    assert_eq!(document["serial"], json!(SERNO));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&profile_path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "mic.json must be 0600");
    }

    // GET / status 读回同一 profile。
    let got = get_mic(&first).await;
    assert_eq!(got["profile"], *profile);
    assert_eq!(got["reasons"], json!([]));
    let status = get_mic_status(&first).await;
    assert_eq!(status["profile"], *profile);
    assert_eq!(status["rt_host"]["available"], json!(false));

    // 部分更新：只改 orientation，其余字段保持（PUT 是 merge）。
    let updated = put_mic(&first, &json!({"orientation": "0deg"})).await;
    assert_eq!(updated.status, 200, "{}", updated.body_text());
    let updated = updated.json();
    assert_eq!(updated["profile"]["orientation"], json!("0deg"));
    assert_eq!(updated["profile"]["device"], json!(DEVICE));
    assert_eq!(updated["profile"]["serial"], json!(SERNO));
    assert_eq!(updated["profile"]["cal_sha256"], json!(sha256));

    // 重启（同一状态目录，新 DaemonState）：profile 仍可读。
    drop(first);
    let mut second = support::spawn_at(dir.path()).await;
    second.sign_in().await;
    let reread = get_mic(&second).await;
    assert_eq!(reread["profile"]["orientation"], json!("0deg"));
    assert_eq!(reread["profile"]["cal_sha256"], json!(sha256));
    assert_eq!(reread["profile"]["parsed"]["points"], json!(5));
    let status = get_mic_status(&second).await;
    assert_eq!(status["profile"], reread["profile"]);
}

#[tokio::test(flavor = "current_thread")]
async fn put_rejects_invalid_updates_with_stable_codes() {
    let server = support::spawn().await;
    let uploaded = upload_cal(&server).await;
    let sha256 = uploaded["sha256"].as_str().expect("sha256").to_string();

    // 空 body / 空对象。
    let response = put_mic(&server, &json!({})).await;
    assert_eq!(response.status, 400, "{}", response.body_text());
    assert_error_code(&response.json(), "calibration_mic_invalid");
    let empty = support::request_json(&server, "PUT", MIC, "").await;
    assert_eq!(empty.status, 400, "{}", empty.body_text());
    assert_error_code(&empty.json(), "calibration_mic_invalid");

    // 未知字段。
    let response = put_mic(&server, &json!({"colour": "red"})).await;
    assert_eq!(response.status, 400, "{}", response.body_text());
    let body = response.json();
    assert_error_code(&body, "calibration_mic_unknown_field");
    assert_eq!(body["error"]["details"]["field"], json!("colour"));

    // 首次保存缺 device（orientation/cal 齐全）。
    let response = put_mic(
        &server,
        &json!({"orientation": "90deg", "cal_sha256": sha256}),
    )
    .await;
    assert_eq!(response.status, 400, "{}", response.body_text());
    let body = response.json();
    assert_error_code(&body, "calibration_mic_invalid");
    assert_eq!(body["error"]["details"]["field"], json!("device"));

    // 串号不符（文件 SERNO = 7187696）。
    let response = put_mic(
        &server,
        &json!({
            "device": DEVICE,
            "serial": "0000000",
            "orientation": "90deg",
            "cal_sha256": sha256,
        }),
    )
    .await;
    assert_eq!(response.status, 400, "{}", response.body_text());
    let body = response.json();
    assert_error_code(&body, "calibration_mic_serial_mismatch");
    assert_eq!(body["error"]["details"]["serial"], json!("0000000"));
    assert_eq!(body["error"]["details"]["serno"], json!(SERNO));

    // 朝向非法。
    let response = put_mic(
        &server,
        &json!({
            "device": DEVICE,
            "orientation": "180deg",
            "cal_sha256": sha256,
        }),
    )
    .await;
    assert_eq!(response.status, 400, "{}", response.body_text());
    let body = response.json();
    assert_error_code(&body, "calibration_mic_orientation_invalid");
    assert_eq!(
        body["error"]["details"]["allowed"],
        json!(["0deg", "90deg"])
    );

    // 引用的校准文件不存在。
    let response = put_mic(
        &server,
        &json!({
            "device": DEVICE,
            "orientation": "90deg",
            "cal_sha256": "0".repeat(64),
        }),
    )
    .await;
    assert_eq!(response.status, 400, "{}", response.body_text());
    assert_error_code(&response.json(), "calibration_mic_not_found");

    // cal_sha256 格式非法。
    let response = put_mic(
        &server,
        &json!({"device": DEVICE, "orientation": "90deg", "cal_sha256": "nothex"}),
    )
    .await;
    assert_eq!(response.status, 400, "{}", response.body_text());
    let body = response.json();
    assert_error_code(&body, "calibration_mic_invalid");
    assert_eq!(body["error"]["details"]["field"], json!("cal_sha256"));

    // 以上失败都不落盘：GET 仍是 profile:null + reason。
    let got = get_mic(&server).await;
    assert!(got["profile"].is_null(), "{got}");
    assert!(!mic_dir(&server.state_dir).join("mic.json").is_file());
}

#[tokio::test(flavor = "current_thread")]
async fn mic_endpoints_require_session_and_csrf() {
    let server = support::spawn().await;
    // 匿名 GET/status → 401（M1 错误体）。
    for path in [MIC, MIC_STATUS] {
        let response = support::request_with(&server, "GET", path, &[], None).await;
        assert_eq!(response.status, 401, "{path}: {}", response.body_text());
        let body = response.json();
        assert_eq!(body["error"]["code"], json!("unauthorized"), "{body}");
    }
    // 匿名 POST 校准文件 → 401。
    let anonymous = support::request_bytes_with(
        &server,
        "POST",
        MIC_CAL,
        &[("Content-Type".to_string(), "text/plain".to_string())],
        Some(REAL_CAL.as_bytes()),
    )
    .await;
    assert_eq!(anonymous.status, 401, "{}", anonymous.body_text());

    // 有会话但写操作缺 CSRF → 403 forbidden。
    let session = server.session().expect("session").clone();
    let cookie_only = vec![
        (
            "Cookie".to_string(),
            format!("khaen_session={}", session.token),
        ),
        (
            "Content-Type".to_string(),
            "text/plain; charset=utf-8".to_string(),
        ),
    ];
    let no_csrf_post = support::request_bytes_with(
        &server,
        "POST",
        MIC_CAL,
        &cookie_only,
        Some(REAL_CAL.as_bytes()),
    )
    .await;
    assert_eq!(no_csrf_post.status, 403, "{}", no_csrf_post.body_text());
    assert_eq!(
        no_csrf_post.json()["error"]["code"],
        json!("forbidden"),
        "{}",
        no_csrf_post.body_text()
    );

    let no_csrf_put = support::request_bytes_with(
        &server,
        "PUT",
        MIC,
        &cookie_only,
        Some(br#"{"orientation":"90deg"}"#),
    )
    .await;
    assert_eq!(no_csrf_put.status, 403, "{}", no_csrf_put.body_text());
}
