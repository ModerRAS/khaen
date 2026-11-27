//! C3b1 + C3b2a：daemon rt-host 真实测量 job（single_point_rt_host）端到端测试。
//!
//! 走真实 TCP + 真实 axum 服务 + 真实 JobManager/daemon 审计索引；rt-host 侧用脚本化
//! FakeTransport（逐 op 断言 daemon 到底发了什么），因此不需要设备、不出声、可并行跑。
//!
//! 覆盖：
//! - 成功：validate 预检 -> cal_enter（带 MicProfile 的 mic 覆盖）-> cal_measure（异步
//!   accepted）-> 轮询 cal_status（measuring -> done）-> cal_solve（C3b2a，落 design.json）
//!   -> bundle 里 cal_leave，job succeeded；measurement.json/quality.json/design.json/
//!   report.json 内容与 sha 真实（design 逐字段拷贝 cal_solve 回执、verdict 是
//!   PREDICTED_ONLY 不是 PASS）、成功路径 cal_leave 恰好一次、请求顺序可断言；
//! - measurement failed -> job failed + cal_leave；
//! - cal_solve 被 rt-host 拒绝 -> job failed + 原业务码 + cal_leave + 无 design.json；
//! - cal_solve 回执畸形（缺 design 对象）-> job failed + 稳定码 E_CAL_SOLVE + cal_leave；
//! - 取消 -> job cancelled + cal_leave；
//! - cal_status 回执畸形 -> job failed + 稳定码 E_CAL_STATUS + cal_leave；
//! - 无 MicProfile -> 创建 409 mic_profile_missing，不排队假 job、不发任何 cal op；
//! - process 目标 -> 创建 503 not_wired（不冒充真实测量）；
//! - REST 输入校验：audible=false / 未知字段 -> 400。

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use khaen_daemon::calibration::backend::JobManagerCalibration;
use khaen_daemon::calibration::rt_host_backend::RtHostJobConfig;
use khaen_daemon::host_link::HostLink;
use khaen_host::sha256_hex;

use support::host_link_fake::FakeTransport;

const JOBS: &str = "/api/calibration/jobs";
const MIC: &str = "/api/calibration/mic";
const MIC_CAL: &str = "/api/calibration/mic/calibration";
const SERNO: &str = "7187696";
const DEVICE: &str = "hw:CARD=U18dB,DEV=0";
const SESSION: &str = "cal-000001-pass01";
const MEASURE_ID: u64 = 1;

/// 真实 miniDSP UMIK-1 校准文件的最小复刻（与 crates/daemon/tests/calibration_mic.rs 同源）。
const REAL_CAL: &str = concat!(
    "\"Sens Factor =-0.0109999dB, SERNO: 7187696\"\n",
    "\"Auto-generated 90-degree calibration file\"\n",
    "10.054\t-4.7795\n",
    "10.179\t-4.6166\n",
    "10.306\t-4.4569\n"
);

fn cal_sha() -> String {
    sha256_hex(REAL_CAL.as_bytes())
}

/// 测试用轮询配置：短间隔/短超时（不真睡 30 min）。
fn test_config() -> RtHostJobConfig {
    RtHostJobConfig {
        poll_interval: Duration::from_millis(10),
        measurement_timeout: Duration::from_secs(10),
    }
}

/// rt-host cal_status 回执（形状与 crates/rt-host/src/control/cal/mod.rs 的 status_json 同源）。
fn status_doc(state: &str, measure_id: Option<u64>, session_id: Option<&str>) -> Value {
    let done = state == "done";
    json!({
        "ok": true,
        "device": {
            "present": true,
            "output_started": true,
            "muted": false,
            "state": "streaming",
        },
        "mode": "calibration",
        "capture_source": "cal",
        "program_input_reaches_dsp": false,
        "playback_restored": false,
        "simulated": false,
        "engine": "in-process",
        "real_capture": true,
        "umik": {
            "open": true,
            "device": DEVICE,
            "capture_channels": 2,
            "capture_channel": 0,
            "declared": true,
            "declaration_source": "cal_enter_override",
            "declaration": {
                "model": "UMIK-1",
                "serial": SERNO,
                "cal_file": "umik1_718-7696_90deg.txt",
                "cal_sha256": cal_sha(),
                "orientation": "90deg",
                "clock_domain": "usb",
                "sample_rate_hz": 48_000,
            },
            "missing_reason": null,
        },
        "measure": {
            "state": state,
            "measure_id": measure_id,
            "pass": if measure_id.is_some() { json!(1) } else { Value::Null },
            "session_id": session_id,
            "elapsed_ms": 120,
            "progress": {
                "phase": "capture",
                "channel_index": 0,
                "channels_total": 1,
                "blocks_done": 3,
                "total_blocks": 10,
                "finished": done,
            },
            "error": null,
            "result": if done { measurement_summary() } else { Value::Null },
        },
        "solve": { "available": false, "last": null },
        "reasons": {},
    })
}

/// rt-host 的测量摘要（CalSummary JSON；真实字段名与 rt-host 同源）。
fn measurement_summary() -> Value {
    json!({
        "measure_id": MEASURE_ID,
        "pass": 1,
        "session_id": SESSION,
        "mode": "acoustic_umik",
        "engine": "in-process",
        "simulated": false,
        "sample_rate": 48_000,
        "drive_level_dbfs": -12.0,
        "capture_peak_dbfs": -28.5,
        "snr_db": 41.2,
        "capture_frames": 240_000,
        "timeline_frames": 240_000,
        "valid_slots": 3,
        "total_slots": 3,
        "clock_ppm": null,
        "clock_ppm_uncertainty_ppm": null,
        "drift_significant": null,
        "clock_ppm_ok": null,
        "xruns": 0,
        "clipped_samples": 0,
    })
}

fn measure_accepted_reply() -> Value {
    json!({
        "ok": true,
        "accepted": true,
        "op": "cal_measure",
        "measure_id": MEASURE_ID,
        "pass": 1,
        "session_id": SESSION,
        "state": "measuring",
        "simulated": false,
        "engine": "in-process",
        "artifacts_dir": "/var/lib/khaen/cal/cal-000001-pass01",
        "note": "measurement runs on a control-plane thread; poll cal_status",
        "result": null,
    })
}

fn leave_reply() -> Value {
    json!({ "ok": true, "left": true, "mode": "playback" })
}

/// rt-host cal_solve 回执（形状与 crates/rt-host/src/control/cal/mod.rs::solve 同源）：
/// 不提交、applied=false、verdict=PREDICTED_ONLY；design 是真实设计对象（测试只做拷贝断言）。
fn solve_reply() -> Value {
    json!({
        "ok": true,
        "applied": false,
        "verdict": "PREDICTED_ONLY",
        "design": {
            "schema": "khaen-rt-host.cal-design/v1",
            "ok": true,
            "applied": false,
            "verdict": "PREDICTED_ONLY",
            "measure_id": MEASURE_ID,
            "pass": 1,
            "session_id": SESSION,
            "simulated": false,
            "mode": "single_point_fir",
            "layout": "single_point",
            "channels": 2,
            "target": "flat",
            "engine": "in-process",
            "measurement_quality": measurement_summary(),
            "lanes": [
                {
                    "label": "L",
                    "slot": 0,
                    "lane": 0,
                    "playback_channel": 0,
                    "ir_samples": 256,
                    "ir_peak_index": 12,
                    "repeats_averaged": 1,
                    "taps": 128,
                    "discarded_peq_sections": 0,
                }
            ],
            "plan": {
                "path": "/var/lib/khaen/cal/cal-000001-pass01/solve/plan.bin",
                "report_path": "/var/lib/khaen/cal/cal-000001-pass01/solve/plan-report.json",
                "sha256": "b".repeat(64),
                "graph_id": "rt-cal-solve-1",
                "bytes": 4096,
                "block_size": 128,
                "sample_rate": 48_000,
                "channels": 2,
            },
            "admission": { "accepted": true, "summary": "accepted" },
            "graph_compile": "ok",
            "apply": {
                "supported": false,
                "reason": "cal_solve never commits: load plan.bin through the prepare/commit control channel when you decide to apply it",
            },
        },
        "note": "design artifacts were written next to the measurement; nothing was committed to the running data plane",
    })
}

fn failed_status_doc() -> Value {
    let mut document = status_doc("failed", Some(MEASURE_ID), Some(SESSION));
    document["measure"]["error"] = json!({
        "code": "measure_failed",
        "stage_code": "E_QUALITY_SNR",
        "message": "snr too low for a trustworthy measurement",
        "cancelled": false,
    });
    document
}

/// 起一个注入 rt-host backend 的 daemon（状态目录由测试持有，service 与 REST 同一个）。
async fn spawn_with_rt_host(
    transport: &Arc<FakeTransport>,
) -> (support::TestServer, support::TempStateDir) {
    let dir = support::TempStateDir::new("rt-host-job");
    let link = support::injected_link(Arc::clone(transport));
    let backend = JobManagerCalibration::new_with_rt_host(dir.path(), link, test_config())
        .expect("rt-host calibration backend");
    let mut server = support::spawn_at_with_service(dir.path(), Arc::new(backend)).await;
    server.sign_in().await;
    (server, dir)
}

/// process 目标的 backend（没有 rt-host 校准数据面）。
async fn spawn_with_process_target() -> (support::TestServer, support::TempStateDir) {
    let dir = support::TempStateDir::new("rt-host-process");
    let backend =
        JobManagerCalibration::new_with_rt_host(dir.path(), HostLink::process(), test_config())
            .expect("process-target calibration backend");
    let mut server = support::spawn_at_with_service(dir.path(), Arc::new(backend)).await;
    server.sign_in().await;
    (server, dir)
}

/// 走真实 C1 REST 保存一份 MicProfile（上传校准文件 + PUT profile）。
async fn save_mic_profile(server: &support::TestServer) -> Value {
    let response = support::request_bytes(
        server,
        "POST",
        MIC_CAL,
        "text/plain; charset=utf-8",
        REAL_CAL.as_bytes(),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let sha256 = response.json()["sha256"]
        .as_str()
        .expect("calibration upload sha256")
        .to_string();
    assert_eq!(sha256, cal_sha());
    let response = support::request_json(
        server,
        "PUT",
        MIC,
        &json!({
            "device": DEVICE,
            "serial": SERNO,
            "orientation": "90deg",
            "cal_sha256": sha256,
        })
        .to_string(),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    response.json()["profile"].clone()
}

fn create_body() -> Value {
    json!({
        "kind": "single_point_rt_host",
        "inputs": { "audible": true, "channels": [0, 1], "level_dbfs": -12.0 },
    })
}

async fn create_job(server: &support::TestServer, body: Value) -> support::HttpResponse {
    support::request_json(server, "POST", JOBS, &body.to_string()).await
}

async fn get_job(server: &support::TestServer, job_id: &str) -> Value {
    let response = support::request(server, "GET", &format!("{JOBS}/{job_id}")).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    response.json()
}

/// 轮询 job 详情直到终态（状态机由真实 JobManager 驱动）。
async fn wait_terminal(server: &support::TestServer, job_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let body = get_job(server, job_id).await;
        let state = body["job"]["status"].as_str().unwrap_or("");
        if matches!(state, "succeeded" | "failed" | "cancelled") {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "job {job_id} did not reach a terminal state: {body}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn artifact<'a>(body: &'a Value, name: &str) -> &'a Value {
    body["job"]["artifacts"]
        .as_array()
        .expect("artifacts array")
        .iter()
        .find(|artifact| artifact["name"] == json!(name))
        .unwrap_or_else(|| panic!("artifact {name} missing: {body}"))
}

fn ops(transport: &Arc<FakeTransport>) -> Vec<String> {
    transport
        .calls()
        .iter()
        .map(|call| call["op"].as_str().unwrap_or("<missing>").to_string())
        .collect()
}

fn count_op(transport: &Arc<FakeTransport>, op: &str) -> usize {
    ops(transport)
        .iter()
        .filter(|name| name.as_str() == op)
        .count()
}

async fn download(server: &support::TestServer, job_id: &str, name: &str) -> support::HttpResponse {
    support::request(server, "GET", &format!("{JOBS}/{job_id}/artifacts/{name}")).await
}

#[tokio::test(flavor = "current_thread")]
async fn success_job_records_real_measurement_and_leaves_exactly_once() {
    let transport = FakeTransport::new();
    transport.push_reply(status_doc("idle", None, None)); // validate probe
    transport.push_reply(status_doc("idle", None, None)); // cal_enter
    transport.push_reply(measure_accepted_reply()); // cal_measure
    transport.push_reply(status_doc("measuring", Some(MEASURE_ID), Some(SESSION))); // poll 1
    transport.push_reply(status_doc("done", Some(MEASURE_ID), Some(SESSION))); // poll 2
    transport.push_reply(solve_reply()); // cal_solve (C3b2a)
    transport.push_reply(leave_reply()); // cal_leave
    let (server, _dir) = spawn_with_rt_host(&transport).await;
    let profile = save_mic_profile(&server).await;
    let created = create_job(&server, create_body()).await;
    assert_eq!(created.status, 202, "{}", created.body_text());
    let created = created.json();
    let job_id = created["job_id"].as_str().expect("job_id").to_string();
    assert_eq!(created["ok"], json!(true));
    assert_eq!(
        created["location"],
        json!(format!("/api/calibration/jobs/{job_id}"))
    );

    let body = wait_terminal(&server, &job_id).await;
    let job = &body["job"];
    assert_eq!(job["status"], json!("succeeded"), "{body}");
    assert_eq!(job["kind"], json!("single_point_rt_host"));
    assert_eq!(job["verdict"], Value::Null, "no fabricated PASS: {body}");
    assert_eq!(job["error"], Value::Null);
    assert_eq!(job["engine"], json!("in-process"));
    // 会话输入如实回显（不是离线上传字段）。
    assert_eq!(
        body["inputs_rt_host"],
        json!({ "audible": true, "channels": [0, 1], "level_dbfs": -12.0 })
    );

    // 5 个真实步骤（C3b2a 多了 solve_g）+ 产物 sha（不编造时间戳，sha 来自 artifact ledger）。
    let steps = job["steps"].as_array().expect("steps");
    let names: Vec<&str> = steps
        .iter()
        .map(|step| step["name"].as_str().expect("step name"))
        .collect();
    assert_eq!(
        names,
        [
            "validate",
            "sweep_capture",
            "quality_check",
            "solve_g",
            "bundle"
        ],
        "{body}"
    );
    assert!(
        steps
            .iter()
            .all(|step| step["status"] == json!("succeeded")),
        "{steps:?}"
    );
    let measurement = artifact(&body, "measurement.json");
    let quality = artifact(&body, "quality.json");
    let design = artifact(&body, "design.json");
    let report = artifact(&body, "report.json");
    assert_eq!(steps[1]["sha256"], measurement["sha256"]);
    assert_eq!(steps[2]["sha256"], quality["sha256"]);
    assert_eq!(steps[3]["sha256"], design["sha256"]);
    assert_eq!(steps[4]["sha256"], report["sha256"]);
    for name in [
        "measurement.json",
        "quality.json",
        "design.json",
        "report.json",
        "artifacts.manifest.json",
    ] {
        let entry = artifact(&body, name);
        assert_eq!(entry["downloadable"], json!(true), "{name}: {entry}");
        assert_eq!(
            entry["sha256"].as_str().map(str::len),
            Some(64),
            "{name}: {entry}"
        );
        assert!(entry["bytes"].as_u64().unwrap_or(0) > 0, "{name}: {entry}");
    }

    // 下载测量证据：逐字段来自 rt-host 回执（不重算、不补默认值）。
    let response = download(&server, &job_id, "measurement.json").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert_eq!(
        response.header("x-khaen-artifact-sha256"),
        measurement["sha256"].as_str()
    );
    let measurement_json = response.json();
    assert_eq!(
        measurement_json["schema"],
        json!("khaen-calibration-rt-host-measurement/v1")
    );
    assert_eq!(measurement_json["job_id"], json!(job_id));
    assert_eq!(measurement_json["measure_id"], json!(MEASURE_ID));
    assert_eq!(measurement_json["session_id"], json!(SESSION));
    assert_eq!(measurement_json["state"], json!("done"));
    assert_eq!(measurement_json["engine"], json!("in-process"));
    assert_eq!(measurement_json["simulated"], json!(false));
    assert_eq!(measurement_json["real_capture"], json!(true));
    assert_eq!(measurement_json["result"]["snr_db"], json!(41.2));
    assert_eq!(
        measurement_json["summary"]["capture_peak_dbfs"],
        json!(-28.5)
    );
    assert_eq!(
        measurement_json["umik"]["declaration"]["serial"],
        json!(SERNO)
    );

    // quality.json：rt-host 没给 quality/verdict 对象 -> available:false，绝不写假 PASS。
    let response = download(&server, &job_id, "quality.json").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let quality_json = response.json();
    assert_eq!(quality_json["available"], json!(false));
    assert_eq!(quality_json["verdict"], Value::Null);
    assert!(
        quality_json["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "{quality_json}"
    );
    assert_eq!(
        quality_json["measurement_sha256"],
        measurement["sha256"].clone()
    );

    // design.json：cal_solve 回执逐字段拷贝（design/verdict/note），身份是 daemon 写的
    // job_id + 本次测量的 measure_id；verdict=PREDICTED_ONLY，绝不伪造 PASS/可应用方案。
    let response = download(&server, &job_id, "design.json").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert_eq!(
        response.header("x-khaen-artifact-sha256"),
        design["sha256"].as_str()
    );
    let design_json = response.json();
    assert_eq!(
        design_json["schema"],
        json!("khaen-calibration-rt-host-design/v1")
    );
    assert_eq!(design_json["job_id"], json!(job_id));
    assert_eq!(design_json["measure_id"], json!(MEASURE_ID));
    let solve = solve_reply();
    assert_eq!(design_json["design"], solve["design"]);
    assert_eq!(design_json["verdict"], json!("PREDICTED_ONLY"));
    assert_eq!(design_json["note"], solve["note"]);
    assert_eq!(design_json["design"]["applied"], json!(false));

    // report.json：真实 job id/measure_id/artifact sha；verdict 恒 null（design 只是
    // predicted-only 证据，plan compile/apply 属 C3b2b）。
    let response = download(&server, &job_id, "report.json").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let report_json = response.json();
    assert_eq!(
        report_json["schema"],
        json!("khaen-calibration-rt-host-report/v1")
    );
    assert_eq!(report_json["job_id"], json!(job_id));
    assert_eq!(report_json["measure_id"], json!(MEASURE_ID));
    assert_eq!(report_json["verdict"], Value::Null);
    assert_eq!(report_json["quality_available"], json!(false));
    assert_eq!(
        report_json["artifacts"][0]["name"],
        json!("measurement.json")
    );
    assert_eq!(report_json["artifacts"][0]["sha256"], measurement["sha256"]);
    assert_eq!(report_json["artifacts"][1]["sha256"], quality["sha256"]);
    assert_eq!(report_json["artifacts"][2]["name"], json!("design.json"));
    assert_eq!(report_json["artifacts"][2]["sha256"], design["sha256"]);
    // design 只汇总真实回执的 sha/verdict/applied；verdict 仍不是 job 级 PASS。
    assert_eq!(report_json["design"]["available"], json!(true));
    assert_eq!(report_json["design"]["sha256"], design["sha256"]);
    assert_eq!(report_json["design"]["verdict"], json!("PREDICTED_ONLY"));
    assert_eq!(report_json["design"]["applied"], json!(false));
    // 成功路径的 leave 只在 bundle 发生一次。
    assert_eq!(report_json["cal_leave"]["ok"], json!(true));
    assert_eq!(report_json["cal_leave"]["issued_by"], json!("bundle"));
    assert_eq!(report_json["cal_leave"]["issued_in_bundle"], json!(true));

    // 请求顺序与 cal_leave 次数：cal_solve 在校准模式内、leave 之前；成功路径只 leave
    // 一次，参数带真实 MicProfile。
    assert_eq!(
        ops(&transport),
        [
            "cal_status",
            "cal_enter",
            "cal_measure",
            "cal_status",
            "cal_status",
            "cal_solve",
            "cal_leave"
        ]
    );
    assert_eq!(count_op(&transport, "cal_leave"), 1);
    let calls = transport.calls();
    let enter = &calls[1];
    assert_eq!(enter["audible"], json!(true));
    assert_eq!(enter["mic"]["device"], json!(DEVICE));
    assert_eq!(enter["mic"]["serial"], json!(SERNO));
    assert_eq!(enter["mic"]["orientation"], json!("90deg"));
    assert_eq!(enter["mic"]["cal_sha256"], json!(cal_sha()));
    assert_eq!(
        enter["mic"]["cal_path"].as_str(),
        profile["cal_path"].as_str()
    );
    assert_eq!(enter["channels"], json!([0, 1]));
    assert_eq!(enter["level_dbfs"], json!(-12.0));
    assert_eq!(calls[2]["audible"], json!(true));
    // cal_solve 在 measurement done 之后、cal_leave 之前，且显式给 rt-host 的默认目标 flat。
    assert_eq!(calls[5]["op"], json!("cal_solve"));
    assert_eq!(calls[5]["target"], json!("flat"));
    assert_eq!(calls[6]["op"], json!("cal_leave"));

    // apply 在本切片必须如实拒绝（只产 predicted-only design，没有可应用 plan；
    // plan compile/apply 属 C3b2b）。
    let response =
        support::request_json(&server, "POST", &format!("{JOBS}/{job_id}/apply"), "{}").await;
    assert_eq!(response.status, 409, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["error"]["code"], json!("plan_unavailable"));
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("C3b2")),
        "{body}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn failed_measurement_fails_job_and_still_leaves() {
    let transport = FakeTransport::new();
    transport.push_reply(status_doc("idle", None, None)); // validate probe
    transport.push_reply(status_doc("idle", None, None)); // cal_enter
    transport.push_reply(measure_accepted_reply()); // cal_measure
    transport.push_reply(failed_status_doc()); // cal_status -> failed
    transport.push_reply(leave_reply()); // cal_leave
    let (server, _dir) = spawn_with_rt_host(&transport).await;
    save_mic_profile(&server).await;
    let created = create_job(&server, create_body()).await;
    assert_eq!(created.status, 202, "{}", created.body_text());
    let job_id = created.json()["job_id"]
        .as_str()
        .expect("job_id")
        .to_string();

    let body = wait_terminal(&server, &job_id).await;
    let job = &body["job"];
    assert_eq!(job["status"], json!("failed"), "{body}");
    assert_eq!(job["error"]["code"], json!("measure_failed"));
    let message = job["error"]["message"].as_str().expect("error message");
    assert!(message.contains("E_QUALITY_SNR"), "{message}");
    assert!(message.contains("snr too low"), "{message}");
    let steps = job["steps"].as_array().expect("steps");
    assert_eq!(steps[1]["name"], json!("sweep_capture"));
    assert_eq!(steps[1]["status"], json!("failed"), "{steps:?}");
    assert_eq!(steps[0]["status"], json!("succeeded"));
    assert_eq!(steps[2]["status"], json!("pending"));
    // 失败路径同样必须交还设备，且只 leave 一次。
    assert_eq!(count_op(&transport, "cal_leave"), 1);
    assert_eq!(
        ops(&transport).last().map(String::as_str),
        Some("cal_leave")
    );
    // 没有测量证据被写出来（失败不产生伪造 measurement）。
    assert!(
        job["artifacts"]
            .as_array()
            .expect("artifacts")
            .iter()
            .all(|artifact| artifact["name"] != json!("measurement.json")),
        "{job}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cancel_during_polling_cancels_job_and_leaves() {
    let transport = FakeTransport::new();
    transport.push_reply(status_doc("idle", None, None)); // validate probe
    transport.push_reply(status_doc("idle", None, None)); // cal_enter
    transport.push_reply(measure_accepted_reply()); // cal_measure
    for _ in 0..200 {
        transport.push_reply(status_doc("measuring", Some(MEASURE_ID), Some(SESSION)));
    }
    transport.push_reply(leave_reply()); // cal_leave
    let (server, _dir) = spawn_with_rt_host(&transport).await;
    save_mic_profile(&server).await;
    let created = create_job(&server, create_body()).await;
    assert_eq!(created.status, 202, "{}", created.body_text());
    let job_id = created.json()["job_id"]
        .as_str()
        .expect("job_id")
        .to_string();

    // 等测量真的开始（cal_status 至少被轮询一次）再取消。
    let deadline = Instant::now() + Duration::from_secs(10);
    while transport.call_count() < 4 {
        assert!(Instant::now() < deadline, "measurement never started");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let response = support::request(&server, "POST", &format!("{JOBS}/{job_id}/cancel")).await;
    assert_eq!(response.status, 200, "{}", response.body_text());

    let body = wait_terminal(&server, &job_id).await;
    let job = &body["job"];
    assert_eq!(job["status"], json!("cancelled"), "{body}");
    assert_eq!(job["error"], Value::Null);
    assert_eq!(count_op(&transport, "cal_leave"), 1);
    assert_eq!(
        ops(&transport).last().map(String::as_str),
        Some("cal_leave")
    );
    // 取消发生在轮询阶段（solve 之前）：没有伪造测量/设计证据。
    for name in ["measurement.json", "design.json"] {
        assert!(
            job["artifacts"]
                .as_array()
                .expect("artifacts")
                .iter()
                .all(|artifact| artifact["name"] != json!(name)),
            "{name}: {job}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_cal_status_fails_job_with_stable_code_and_leaves() {
    let transport = FakeTransport::new();
    transport.push_reply(status_doc("idle", None, None)); // validate probe
    transport.push_reply(status_doc("idle", None, None)); // cal_enter
    transport.push_reply(measure_accepted_reply()); // cal_measure
                                                    // 畸形回执：没有 measure 对象（不猜状态，不伪造结果）。
    transport.push_reply(json!({ "ok": true, "mode": "calibration" }));
    transport.push_reply(leave_reply());
    let (server, _dir) = spawn_with_rt_host(&transport).await;
    save_mic_profile(&server).await;
    let created = create_job(&server, create_body()).await;
    assert_eq!(created.status, 202, "{}", created.body_text());
    let job_id = created.json()["job_id"]
        .as_str()
        .expect("job_id")
        .to_string();

    let body = wait_terminal(&server, &job_id).await;
    let job = &body["job"];
    assert_eq!(job["status"], json!("failed"), "{body}");
    assert_eq!(job["error"]["code"], json!("E_CAL_STATUS"), "{body}");
    assert_eq!(count_op(&transport, "cal_leave"), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn cal_solve_error_fails_job_and_still_leaves_without_design() {
    let transport = FakeTransport::new();
    transport.push_reply(status_doc("idle", None, None)); // validate probe
    transport.push_reply(status_doc("idle", None, None)); // cal_enter
    transport.push_reply(measure_accepted_reply()); // cal_measure
    transport.push_reply(status_doc("done", Some(MEASURE_ID), Some(SESSION))); // poll -> done
                                                                               // cal_solve 被 rt-host 拒绝：业务错误码原样进 job.error.code，不伪造 design。
    transport.push_error_reply("solve_failed", "target FIR design failed for L (channel 0)");
    transport.push_reply(leave_reply()); // solve_g 失败路径尽早 cal_leave
    let (server, _dir) = spawn_with_rt_host(&transport).await;
    save_mic_profile(&server).await;
    let created = create_job(&server, create_body()).await;
    assert_eq!(created.status, 202, "{}", created.body_text());
    let job_id = created.json()["job_id"]
        .as_str()
        .expect("job_id")
        .to_string();

    let body = wait_terminal(&server, &job_id).await;
    let job = &body["job"];
    assert_eq!(job["status"], json!("failed"), "{body}");
    assert_eq!(job["error"]["code"], json!("solve_failed"), "{body}");
    let message = job["error"]["message"].as_str().expect("error message");
    assert!(message.contains("target FIR"), "{message}");
    // 步骤如实：measurement 成功，solve_g 失败，bundle 未跑。
    let steps = job["steps"].as_array().expect("steps");
    assert_eq!(steps.len(), 5, "{steps:?}");
    assert_eq!(steps[1]["status"], json!("succeeded"));
    assert_eq!(steps[2]["status"], json!("succeeded"));
    assert_eq!(steps[3]["name"], json!("solve_g"));
    assert_eq!(steps[3]["status"], json!("failed"), "{steps:?}");
    assert_eq!(steps[4]["status"], json!("pending"));
    // 失败路径同样交还设备且只 leave 一次（solve_g 之后、job 结束前）。
    assert_eq!(count_op(&transport, "cal_leave"), 1);
    assert_eq!(
        ops(&transport).last().map(String::as_str),
        Some("cal_leave")
    );
    // 没有设计证据被伪造；真实测量证据保留。
    assert!(
        job["artifacts"]
            .as_array()
            .expect("artifacts")
            .iter()
            .all(|artifact| artifact["name"] != json!("design.json")),
        "{job}"
    );
    assert!(
        job["artifacts"]
            .as_array()
            .expect("artifacts")
            .iter()
            .any(|artifact| artifact["name"] == json!("measurement.json")),
        "{job}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_cal_solve_reply_fails_with_e_cal_solve_without_design() {
    let transport = FakeTransport::new();
    transport.push_reply(status_doc("idle", None, None)); // validate probe
    transport.push_reply(status_doc("idle", None, None)); // cal_enter
    transport.push_reply(measure_accepted_reply()); // cal_measure
    transport.push_reply(status_doc("done", Some(MEASURE_ID), Some(SESSION))); // poll -> done
                                                                               // 回执是 ok，但缺 design 对象：稳定码 E_CAL_SOLVE，不补一个假 design。
    transport.push_reply(json!({ "ok": true, "applied": false, "verdict": "PREDICTED_ONLY" }));
    transport.push_reply(leave_reply()); // solve_g 失败路径尽早 cal_leave
    let (server, _dir) = spawn_with_rt_host(&transport).await;
    save_mic_profile(&server).await;
    let created = create_job(&server, create_body()).await;
    assert_eq!(created.status, 202, "{}", created.body_text());
    let job_id = created.json()["job_id"]
        .as_str()
        .expect("job_id")
        .to_string();

    let body = wait_terminal(&server, &job_id).await;
    let job = &body["job"];
    assert_eq!(job["status"], json!("failed"), "{body}");
    assert_eq!(job["error"]["code"], json!("E_CAL_SOLVE"), "{body}");
    let message = job["error"]["message"].as_str().expect("error message");
    assert!(message.contains("design"), "{message}");
    assert_eq!(job["steps"][3]["name"], json!("solve_g"));
    assert_eq!(job["steps"][3]["status"], json!("failed"), "{body}");
    assert_eq!(count_op(&transport, "cal_leave"), 1);
    assert!(
        job["artifacts"]
            .as_array()
            .expect("artifacts")
            .iter()
            .all(|artifact| artifact["name"] != json!("design.json")),
        "{job}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn missing_mic_profile_rejects_creation_with_409_and_sends_no_cal_op() {
    let transport = FakeTransport::new();
    let (server, _dir) = spawn_with_rt_host(&transport).await;
    let response = create_job(&server, create_body()).await;
    assert_eq!(response.status, 409, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["error"]["code"], json!("mic_profile_missing"));
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("microphone profile")),
        "{body}"
    );
    assert_eq!(
        transport.call_count(),
        0,
        "no cal op may be sent for a rejected creation"
    );
    // 没有排队假 job。
    let response = support::request(&server, "GET", JOBS).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.json()["total"], json!(0));
}

#[tokio::test(flavor = "current_thread")]
async fn process_target_rejects_creation_with_503_not_wired() {
    let (server, _dir) = spawn_with_process_target().await;
    let response = create_job(&server, create_body()).await;
    assert_eq!(response.status, 503, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["error"]["code"], json!("not_wired"));
    let message = body["error"]["message"].as_str().expect("message");
    assert!(message.contains("daemon-process"), "{message}");
    let response = support::request(&server, "GET", JOBS).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.json()["total"], json!(0));
}

#[tokio::test(flavor = "current_thread")]
async fn rest_input_validation_rejects_silent_or_unknown_inputs() {
    let transport = FakeTransport::new();
    let (server, _dir) = spawn_with_rt_host(&transport).await;
    // audible=false：真实测量必须显式授权可闻扫频。
    let response = create_job(
        &server,
        json!({ "kind": "single_point_rt_host", "inputs": { "audible": false } }),
    )
    .await;
    assert_eq!(response.status, 400, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["error"]["code"], json!("invalid_request"));
    assert_eq!(body["error"]["details"]["field"], json!("inputs.audible"));
    // 未知 inputs 字段 400（不静默忽略）。
    let response = create_job(
        &server,
        json!({
            "kind": "single_point_rt_host",
            "inputs": { "audible": true, "bogus": 1 },
        }),
    )
    .await;
    assert_eq!(response.status, 400, "{}", response.body_text());
    assert_eq!(response.json()["error"]["code"], json!("invalid_request"));
    // 电平越界 400。
    let response = create_job(
        &server,
        json!({
            "kind": "single_point_rt_host",
            "inputs": { "audible": true, "level_dbfs": 0.0 },
        }),
    )
    .await;
    assert_eq!(response.status, 400, "{}", response.body_text());
    assert_eq!(
        response.json()["error"]["details"]["field"],
        json!("inputs.level_dbfs")
    );
    assert_eq!(transport.call_count(), 0);
}
#[tokio::test(flavor = "current_thread")]
async fn leave_failure_after_successful_measurement_fails_job_with_e_cal_leave() {
    let transport = FakeTransport::new();
    transport.push_reply(status_doc("idle", None, None)); // validate probe
    transport.push_reply(status_doc("idle", None, None)); // cal_enter
    transport.push_reply(measure_accepted_reply()); // cal_measure
    transport.push_reply(status_doc("done", Some(MEASURE_ID), Some(SESSION))); // poll -> done
    transport.push_reply(solve_reply()); // cal_solve succeeds (design.json is written)
                                         // bundle 的 cal_leave 被 rt-host 拒绝（业务错误不重试）：
                                         // 测量/设计成功也不能声称设备已交还。
    transport.push_error_reply("cal_internal", "cannot leave calibration mode right now");
    let (server, _dir) = spawn_with_rt_host(&transport).await;
    save_mic_profile(&server).await;
    let created = create_job(&server, create_body()).await;
    assert_eq!(created.status, 202, "{}", created.body_text());
    let job_id = created.json()["job_id"]
        .as_str()
        .expect("job_id")
        .to_string();

    let body = wait_terminal(&server, &job_id).await;
    let job = &body["job"];
    assert_eq!(job["status"], json!("failed"), "{body}");
    assert_eq!(job["error"]["code"], json!("E_CAL_LEAVE"), "{body}");
    let message = job["error"]["message"].as_str().expect("message");
    assert!(message.contains("cal_leave failed"), "{message}");
    assert!(message.contains("cal_internal"), "{message}");
    // cal_leave 不重试（C3a）：只尝试一次。
    assert_eq!(count_op(&transport, "cal_leave"), 1);
    // 测量/设计证据本身是真实的（solve 成功后才在 bundle 失败），但 job 不因它们成功而成功。
    for name in ["measurement.json", "design.json"] {
        assert!(
            job["artifacts"]
                .as_array()
                .expect("artifacts")
                .iter()
                .any(|artifact| artifact["name"] == json!(name)),
            "{name}: {job}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn leave_failure_does_not_mask_the_original_measurement_error() {
    let transport = FakeTransport::new();
    transport.push_reply(status_doc("idle", None, None)); // validate probe
    transport.push_reply(status_doc("idle", None, None)); // cal_enter
    transport.push_reply(measure_accepted_reply()); // cal_measure
    transport.push_reply(failed_status_doc()); // cal_status -> measurement failed
    transport.push_error_reply("cal_internal", "cannot leave calibration mode right now");
    let (server, _dir) = spawn_with_rt_host(&transport).await;
    save_mic_profile(&server).await;
    let created = create_job(&server, create_body()).await;
    assert_eq!(created.status, 202, "{}", created.body_text());
    let job_id = created.json()["job_id"]
        .as_str()
        .expect("job_id")
        .to_string();

    let body = wait_terminal(&server, &job_id).await;
    let job = &body["job"];
    assert_eq!(job["status"], json!("failed"), "{body}");
    // 原错误码/原因保留（不因 leave 失败改写成 E_CAL_LEAVE），leave 失败只追加到 message。
    assert_eq!(job["error"]["code"], json!("measure_failed"), "{body}");
    let message = job["error"]["message"].as_str().expect("message");
    assert!(message.contains("E_QUALITY_SNR"), "{message}");
    assert!(message.contains("snr too low"), "{message}");
    assert!(message.contains("cal_leave failed"), "{message}");
    assert_eq!(count_op(&transport, "cal_leave"), 1);
}
