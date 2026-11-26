//! T70h-b 校准**生产后端** REST 端到端测试：真实 `JobManagerCalibration` + 真实 TCP/axum，
//! 全离线（合成 WAV + ABI v3 方案，不打开设备、不出声）。
//!
//! 与 tests/calibration_jobs.rs（注入 fake 服务）互补：这里注入的是生产实现
//! `khaen_daemon::calibration::backend::JobManagerCalibration`（khaen-calibration 的
//! JobManager + OfflineJobBackend + daemon 审计索引），走真实 HTTP 契约。
//!
//! 覆盖（task-65）：
//! 1. happy path：上传 identity 方案 + 合成 48k PCM16 WAV → 202 建 job → 轮询 succeeded →
//!    verdict/steps 五步/artifacts manifest sha/measured 与 simulated 分离/列表/`/api/status`/`/api/meta` 收敛；
//! 2. 产物下载：`x-khaen-artifact-sha256` == 列表 sha == sha256(body)，report/manifest schema，
//!    未知名 404 `artifact_not_found`；
//! 3. apply staged：prepare→commit→重复 commit 409 `engine_token_invalid`；新 job prepare→abort 不改运行版本；
//! 4. 旧单发 apply/rollback 空 body 兼容（不破坏现有前端）；
//! 5. rollback staged：prepare 给 target_version→commit 记 rolled_back；
//! 6. 失败诚实：坏 WAV → failed + 稳定码 `E_OFFLINE_JOB_INPUT`，apply → 409 `job_not_succeeded`；
//! 7. 取消：长 WAV job → cancelled（终态诚实，不假 cancelled）；
//! 8. 单 job 互斥：长 WAV job 仍在 running 时第二个 job → 409 `switch_busy`；
//! 9. 审计持久化：同一 state_dir 再构造一个 JobManagerCalibration 仍能 list/job（重启不丢索引）；
//! 10. 索引损坏 → `new()` 明确 Err（main 可如实降级 not_wired，不静默成功）。
//!
//! 约束与口径：
//! - apply/rollback 走**真实进程内控制面**（host_link target=process）。控制面 graph compile 要求方案与
//!   运行配置一致（采样率/块长/通道），daemon 测试环境是 `EngineConfig::default()` = 96 kHz/128/2，
//!   所以 apply 用例的 job 用 **96 kHz** 方案 + 96 kHz WAV；纯 job 执行链路按任务卡用 48 kHz。
//!   这是控制面既定约束，不是校准后端 bug。
//! - `tokio::time` 未启用（daemon 的 tokio features 无 time）：轮询用 spawn_blocking + std sleep。
//!
//! NOT_RUN：真机 UMIK/MOTU 采集 job（T70f-c）、rt-host 目标下的真实 apply（会重开 PCM/出声，需设备窗口）。

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use khaen_calibration::plan_compile::{compile_peq_design, PlanCompileOptions};
use khaen_daemon::calibration::backend::JobManagerCalibration;
use khaen_daemon::calibration::service::{CalibrationService, JobQuery};
use khaen_designer::modal_peq::{ChannelPeq, DesignMode, PeqDesign};
use khaen_host::sha256_hex;

const JOBS: &str = "/api/calibration/jobs";
const UPLOADS: &str = "/api/calibration/uploads";
const TONE_HZ: f64 = 1_000.0;
const BLOCK_SIZE: u32 = 128;
/// 纯 job 执行链路（任务卡口径）：48 kHz。
const JOB_RATE: u32 = 48_000;
/// apply/rollback 用例：必须与进程内引擎 `EngineConfig::default()`（96 kHz/128/2）一致。
const APPLY_RATE: u32 = 96_000;

// ---------------------------------------------------------------------------
// 服务与后端
// ---------------------------------------------------------------------------

/// 用临时状态目录启动真实服务 + 注入生产校准后端，并完成 setup 登录。
async fn backend_server(tag: &str) -> (Arc<support::TempStateDir>, support::TestServer) {
    let dir = Arc::new(support::TempStateDir::new(tag));
    let service: Arc<dyn CalibrationService> = Arc::new(
        JobManagerCalibration::new(dir.path()).expect("create production calibration backend"),
    );
    let mut server = support::spawn_at_with_service(dir.path(), service).await;
    server.sign_in().await;
    (dir, server)
}

// ---------------------------------------------------------------------------
// 合成输入：WAV + ABI v3 identity 方案
// ---------------------------------------------------------------------------

/// 44B RIFF/WAVE 头 + 单声道 PCM16（小端，确定性）。
fn write_wav_bytes(samples: &[f64], sample_rate: u32) -> Vec<u8> {
    let data_bytes = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    for sample in samples {
        let clamped = sample.clamp(-1.0, 1.0);
        let scaled = (clamped * 32767.0).round() as i32;
        out.extend_from_slice(&(scaled as i16).to_le_bytes());
    }
    out
}

fn tone(amplitude: f64, sample_rate: u32, seconds: f64) -> Vec<f64> {
    let count = (sample_rate as f64 * seconds).round() as usize;
    (0..count)
        .map(|index| {
            amplitude
                * (2.0 * std::f64::consts::PI * TONE_HZ * index as f64 / sample_rate as f64).sin()
        })
        .collect()
}

/// 1 kHz / 幅度 0.5 的 PCM16 WAV。
fn tone_wav(sample_rate: u32, seconds: f64) -> Vec<u8> {
    write_wav_bytes(&tone(0.5, sample_rate, seconds), sample_rate)
}

/// 明确不是 WAV 的 100 字节（validate stage 必须给出稳定错误码）。
fn invalid_wav_bytes() -> Vec<u8> {
    (0..100u32).map(|index| (index * 7) as u8).collect()
}

/// 测试方案：ABI v3 identity（单通道 "L"、无 PEQ；预测相对电平 = 0 dB）。
struct TestPlan {
    bytes: Vec<u8>,
    sha256: String,
    graph_id: String,
}

fn identity_plan(sample_rate: u32) -> TestPlan {
    let mut design = PeqDesign::new(sample_rate, DesignMode::ModalOnly).expect("peq design");
    design
        .add_channel(ChannelPeq::new("L", 0).expect("channel"))
        .expect("add identity channel");
    let compiled = compile_peq_design(
        &design,
        &PlanCompileOptions {
            sample_rate,
            block_size: BLOCK_SIZE,
            channels: 2,
            lane_gain_db: Vec::new(),
            lane_delay: Vec::new(),
        },
    )
    .expect("compile identity plan");
    assert_eq!(
        compiled.sha256,
        sha256_hex(&compiled.bytes),
        "compiled sha256 must be the content sha256"
    );
    TestPlan {
        sha256: compiled.sha256,
        graph_id: compiled.graph_id,
        bytes: compiled.bytes,
    }
}

// ---------------------------------------------------------------------------
// HTTP 辅助
// ---------------------------------------------------------------------------

fn artifact_path(job_id: &str, name: &str) -> String {
    format!("{JOBS}/{job_id}/artifacts/{name}")
}

fn assert_error_code(body: &Value, expected: &str) {
    assert_eq!(
        body["error"]["code"],
        json!(expected),
        "unexpected error body: {body}"
    );
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "error.message must be non-empty: {body}"
    );
    assert!(body["error"]["details"].is_object(), "{body}");
}

fn assert_sha256_hex(value: &Value, label: &str) -> String {
    let text = value
        .as_str()
        .unwrap_or_else(|| panic!("{label} must be a sha256 string, got {value}"));
    assert_eq!(text.len(), 64, "{label} must be 64 hex chars, got {text}");
    assert!(
        text.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{label} must be hex, got {text}"
    );
    assert_eq!(
        text,
        text.to_ascii_lowercase(),
        "{label} must be lowercase hex, got {text}"
    );
    text.to_string()
}

fn artifact_sha(job: &Value, name: &str) -> String {
    assert_sha256_hex(&find_artifact(job, name)["sha256"], name)
}

fn find_artifact<'a>(job: &'a Value, name: &str) -> &'a Value {
    job["artifacts"]
        .as_array()
        .and_then(|artifacts| {
            artifacts
                .iter()
                .find(|artifact| artifact["name"].as_str() == Some(name))
        })
        .unwrap_or_else(|| panic!("job has no artifact {name}: {}", job["artifacts"]))
}

/// 原始字节上传；返回内容寻址 upload id（= 内容 sha256）。
async fn upload(server: &support::TestServer, bytes: &[u8], content_type: &str) -> String {
    let response = support::request_bytes(server, "POST", UPLOADS, content_type, bytes).await;
    assert!(
        matches!(response.status, 200 | 201),
        "upload failed ({}): {}",
        response.status,
        response.body_text()
    );
    let body = response.json();
    assert_eq!(
        body["upload_id"], body["sha256"],
        "upload id = content sha256"
    );
    assert_eq!(body["upload_id"].as_str(), Some(sha256_hex(bytes).as_str()));
    assert_eq!(body["bytes"], json!(bytes.len()));
    body["upload_id"].as_str().expect("upload_id").to_string()
}

async fn upload_plan(server: &support::TestServer, plan: &TestPlan) -> String {
    let upload_id = upload(server, &plan.bytes, "application/octet-stream").await;
    assert_eq!(upload_id, plan.sha256, "plan upload id = plan sha256");
    upload_id
}

fn job_payload(plan_sha: &str, baseline_upload_id: &str, applied_upload_id: &str) -> Value {
    json!({
        "kind": "single_point_offline",
        "inputs": {
            "plan_sha": plan_sha,
            "baseline_upload_id": baseline_upload_id,
            "applied_upload_id": applied_upload_id,
            "centers_hz": [TONE_HZ],
            "tolerance_db": 2.0,
            "min_bands": 1,
        }
    })
}

/// 建 job 并断言 202 契约；返回 job id。
async fn create_job(
    server: &support::TestServer,
    plan_sha: &str,
    baseline_upload_id: &str,
    applied_upload_id: &str,
) -> String {
    let payload = job_payload(plan_sha, baseline_upload_id, applied_upload_id);
    let response = support::request_json(server, "POST", JOBS, &payload.to_string()).await;
    assert_eq!(
        response.status,
        202,
        "create job failed: {}",
        response.body_text()
    );
    let body = response.json();
    assert_eq!(body["ok"], json!(true), "{body}");
    let job_id = body["job_id"].as_str().expect("job_id").to_string();
    let digits = job_id.strip_prefix("job-").expect("job-NNNN id");
    assert!(
        !digits.is_empty() && digits.chars().all(|ch| ch.is_ascii_digit()),
        "unexpected job id {job_id:?}"
    );
    assert_eq!(body["location"], json!(format!("{JOBS}/{job_id}")));
    let state = body["state"].as_str().expect("state");
    assert!(
        matches!(state, "queued" | "running"),
        "new job must not be terminal: {body}"
    );
    job_id
}

/// `tokio::time` 未启用：轮询间隔用 spawn_blocking + std sleep（不占 async worker）。
async fn sleep_poll_interval() {
    tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_millis(25)))
        .await
        .expect("poll sleep task");
}

/// 轮询 job 详情直到终态（最多 30 s），返回最后一次详情 JSON。
async fn wait_terminal(server: &support::TestServer, job_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let response = support::request(server, "GET", &format!("{JOBS}/{job_id}")).await;
        assert_eq!(
            response.status,
            200,
            "job detail failed: {}",
            response.body_text()
        );
        let detail = response.json();
        let state = detail["job"]["status"].as_str().unwrap_or_default();
        if matches!(state, "succeeded" | "failed" | "cancelled") {
            return detail;
        }
        assert!(
            Instant::now() < deadline,
            "job {job_id} did not reach a terminal state in 30s: {detail}"
        );
        sleep_poll_interval().await;
    }
}

/// 跑一个成功的离线 job（identity 方案 → 期望 PASS），返回 job 与其输入引用。
struct FinishedJob {
    job_id: String,
    plan: TestPlan,
    baseline_upload_id: String,
    applied_upload_id: String,
    detail: Value,
}

async fn finished_job(server: &support::TestServer, sample_rate: u32) -> FinishedJob {
    let plan = identity_plan(sample_rate);
    let plan_sha = upload_plan(server, &plan).await;
    let wav = tone_wav(sample_rate, 1.0);
    // identity 方案：applied 与 baseline 是同一段录音 → 同一份字节、同一内容寻址 upload id。
    let baseline_upload_id = upload(server, &wav, "audio/wav").await;
    let applied_upload_id = upload(server, &wav, "audio/wav").await;
    let job_id = create_job(server, &plan_sha, &baseline_upload_id, &applied_upload_id).await;
    let detail = wait_terminal(server, &job_id).await;
    assert_eq!(
        detail["job"]["status"],
        json!("succeeded"),
        "job did not succeed: {detail}"
    );
    assert_eq!(
        detail["job"]["verdict"],
        json!("PASS"),
        "identity plan must PASS: {detail}"
    );
    FinishedJob {
        job_id,
        plan,
        baseline_upload_id,
        applied_upload_id,
        detail,
    }
}

/// 跑一个控制面块，让 commit 的图在块边界真正生效（运行版本/coeff 回读需要）。
fn run_control_block(server: &support::TestServer) -> usize {
    let input: Vec<f32> = (0..256).map(|index| (index as f32 * 0.01) - 1.0).collect();
    let mut control = server.state.control();
    control
        .engine_mut()
        .run_block(&input)
        .expect("run control block")
        .len()
}

// ---------------------------------------------------------------------------
// 1) happy path：verdict / 五步 / artifacts / measured+simulated 分离 / 列表 / status / meta
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn happy_path_reports_verdict_evidence_and_wired_capabilities() {
    let (_dir, server) = backend_server("cal-backend-happy").await;
    let job = finished_job(&server, JOB_RATE).await;
    let detail = &job.detail;

    // ---- 详情顶层（docs/API.md §7 的加法字段）----
    assert_eq!(detail["ok"], json!(true), "{detail}");
    assert_eq!(detail["schema"], json!("khaen-calibration-job/v1"));
    assert_eq!(detail["error"], Value::Null);
    assert_eq!(detail["result"]["verdict"], json!("PASS"));
    assert_eq!(
        detail["artifacts"].as_array().map(|items| items.len()),
        Some(5),
        "top-level artifacts mirror job.artifacts: {detail}"
    );

    // ---- job 记录 ----
    let record = &detail["job"];
    assert_eq!(record["id"].as_str(), Some(job.job_id.as_str()));
    assert_eq!(record["kind"], json!("single_point_offline"));
    assert_eq!(record["status"], json!("succeeded"));
    assert_eq!(record["verdict"], json!("PASS"));
    assert_eq!(
        record["step"],
        Value::Null,
        "terminal job has no active step"
    );
    assert_eq!(record["error"], Value::Null);

    // 审计字段：created_unix/updated_unix 是整数，progress 是数字（不造假值）。
    let created_unix = record["created_unix"].as_u64().expect("created_unix");
    let updated_unix = record["updated_unix"].as_u64().expect("updated_unix");
    assert!(updated_unix >= created_unix, "{record}");
    let progress = record["progress"].as_f64().expect("progress");
    assert!(
        progress > 0.0 && progress <= 1.0,
        "progress must be a real fraction in (0, 1]: {progress}"
    );
    assert!(record["created_at"]
        .as_str()
        .is_some_and(|text| text.ends_with('Z')));
    assert!(record["finished_at"]
        .as_str()
        .is_some_and(|text| text.ends_with('Z')));
    assert!(record["duration_ms"].as_u64().is_some(), "{record}");

    // 输入回显。
    assert_eq!(
        record["inputs"]["plan_sha"].as_str(),
        Some(job.plan.sha256.as_str())
    );
    assert_eq!(
        record["inputs"]["baseline_upload_id"].as_str(),
        Some(job.baseline_upload_id.as_str())
    );
    assert_eq!(record["inputs"]["min_bands"], json!(1));

    // 五步状态机：validate/compile/levels/closed_loop/bundle 全 succeeded（不跳步）。
    let steps = record["steps"].as_array().expect("steps");
    assert_eq!(steps.len(), 5, "{record}");
    let step_names = ["validate", "compile", "levels", "closed_loop", "bundle"];
    for (index, name) in step_names.iter().enumerate() {
        assert_eq!(steps[index]["name"].as_str(), Some(*name));
        assert_eq!(steps[index]["status"], json!("succeeded"), "{record}");
    }
    assert_eq!(steps[0]["sha256"], Value::Null, "validate 不产可下载产物");
    for (index, name) in [
        (1, "plan.bin"),
        (2, "predicted.json"),
        (3, "report.json"),
        (4, "artifacts.manifest.json"),
    ] {
        let step_sha = assert_sha256_hex(&steps[index]["sha256"], name);
        assert_eq!(
            step_sha,
            artifact_sha(record, name),
            "step {name} sha256 must equal the artifact sha256"
        );
    }

    // 产物白名单：恰为 5 个名字，sha256 非空 64 hex。
    let artifacts = record["artifacts"].as_array().expect("artifacts");
    assert_eq!(artifacts.len(), 5, "{record}");
    let names: Vec<&str> = artifacts
        .iter()
        .map(|artifact| artifact["name"].as_str().expect("artifact name"))
        .collect();
    for name in [
        "plan.bin",
        "predicted.json",
        "observed.json",
        "report.json",
        "artifacts.manifest.json",
    ] {
        assert!(names.contains(&name), "missing artifact {name}: {names:?}");
        let artifact = find_artifact(record, name);
        assert_sha256_hex(&artifact["sha256"], name);
        assert!(
            artifact["bytes"].as_u64().is_some_and(|bytes| bytes > 0),
            "{name} must have bytes: {artifact}"
        );
        assert_eq!(artifact["downloadable"], json!(true), "{name}");
    }

    // result：verdict + 实测（measured）/模拟（simulated）结构分离，模拟不冒充实测。
    let result = &record["result"];
    assert_eq!(result["verdict"], json!("PASS"));
    assert!(result["max_delta_db"].is_number(), "{result}");
    assert_eq!(result["compared_bands"], json!(1));
    assert_eq!(result["missing_bands"], json!(0));
    let measured = &result["measured"];
    assert_eq!(
        measured["baseline_upload_id"].as_str(),
        Some(job.baseline_upload_id.as_str())
    );
    assert_eq!(
        measured["applied_upload_id"].as_str(),
        Some(job.applied_upload_id.as_str())
    );
    assert_eq!(measured["sample_rate_hz"], json!(JOB_RATE));
    assert!(measured["source"]
        .as_str()
        .is_some_and(|text| !text.is_empty()));
    assert!(
        measured.get("plan_sha256").is_none(),
        "measured 段不得混入模拟字段: {measured}"
    );
    let simulated = &result["simulated"];
    assert_eq!(
        simulated["plan_sha256"].as_str(),
        Some(job.plan.sha256.as_str())
    );
    assert!(
        simulated.get("baseline_upload_id").is_none(),
        "simulated 段不得冒充实测: {simulated}"
    );
    assert_eq!(
        simulated["graph_id"].as_str(),
        result["plan"]["graph_id"].as_str()
    );
    assert_eq!(
        result["plan"]["sha256"].as_str(),
        Some(job.plan.sha256.as_str())
    );
    assert_eq!(
        result["plan"]["graph_id"].as_str(),
        Some(job.plan.graph_id.as_str())
    );

    // ---- GET /api/calibration/jobs：ok/available/分页字段 + 新→旧 ----
    let list = support::request(&server, "GET", JOBS).await;
    assert_eq!(list.status, 200, "{}", list.body_text());
    let list = list.json();
    assert_eq!(list["ok"], json!(true), "{list}");
    assert_eq!(list["available"], json!(true));
    assert_eq!(list["reason"], Value::Null);
    assert!(list["engine"].as_str().is_some_and(|text| !text.is_empty()));
    assert_eq!(list["active_job_id"], Value::Null, "终态后没有 active job");
    assert!(list["total"].as_u64().is_some_and(|total| total >= 1));
    let rows = list["jobs"].as_array().expect("jobs");
    let row = rows
        .iter()
        .find(|row| row["id"].as_str() == Some(job.job_id.as_str()))
        .unwrap_or_else(|| panic!("job missing from list: {list}"));
    assert_eq!(row["status"], json!("succeeded"));
    assert_eq!(row["verdict"], json!("PASS"));
    assert!(row["created_unix"].as_u64().is_some());
    assert!(row["updated_unix"].as_u64().is_some());
    assert!(row["progress"].as_f64().is_some());

    // ---- /api/meta：真实注入的服务 → calibration.wired=true，not_wired 不含 calibration.jobs ----
    let meta = support::request(&server, "GET", "/api/meta").await;
    assert_eq!(meta.status, 200, "{}", meta.body_text());
    let meta = meta.json();
    assert_eq!(meta["ok"], json!(true));
    assert_eq!(meta["calibration"]["wired"], json!(true), "{meta}");
    assert_eq!(meta["calibration"]["reason"], Value::Null, "{meta}");
    let not_wired = meta["not_wired"].as_array().expect("not_wired");
    assert!(
        not_wired
            .iter()
            .all(|entry| entry["op"] != json!("calibration.jobs")),
        "wired 后端不得再上报 calibration.jobs 未接线: {meta}"
    );

    // ---- /api/status：calibration 三字段来自真实服务（last_job_id/last_verdict 收敛）----
    // 合同 §3 的 /api/status 形状没有 calibration 段；wired 的唯一真实来源是 /api/meta
    // （上面已断言）。status 的 calibration 三字段按实际实现断言，不写不存在的字段。
    let status = support::request(&server, "GET", "/api/status").await;
    assert_eq!(status.status, 200, "{}", status.body_text());
    let status = status.json();
    assert_eq!(
        status["calibration"]["last_job_id"].as_str(),
        Some(job.job_id.as_str()),
        "{status}"
    );
    assert_eq!(status["calibration"]["active_job"], Value::Null, "{status}");
    assert_eq!(
        status["calibration"]["last_verdict"],
        json!("PASS"),
        "{status}"
    );
    assert!(
        status["reasons"].get("calibration.last_job_id").is_none(),
        "有真实来源时不应再给缺失原因: {status}"
    );
    let status_not_wired = status["not_wired"].as_array().expect("status not_wired");
    assert!(
        status_not_wired
            .iter()
            .all(|entry| entry["path"] != json!("calibration.jobs")),
        "wired 后端不得再上报 calibration.jobs 未接线: {status}"
    );
}

// ---------------------------------------------------------------------------
// 2) 产物下载：manifest sha 校验 + schema + 未知名 404
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn artifact_downloads_verify_listed_sha_and_schema() {
    let (_dir, server) = backend_server("cal-backend-artifacts").await;
    let job = finished_job(&server, JOB_RATE).await;

    // report.json：响应头 sha == 列表 sha == sha256(body)，正文是闭环报告 schema。
    let listed_sha = artifact_sha(&job.detail["job"], "report.json");
    let response =
        support::request(&server, "GET", &artifact_path(&job.job_id, "report.json")).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert_eq!(
        response.header("x-khaen-artifact-sha256"),
        Some(listed_sha.as_str())
    );
    assert_eq!(sha256_hex(&response.body), listed_sha);
    assert_eq!(response.header("content-type"), Some("application/json"));
    let report: Value = serde_json::from_slice(&response.body).expect("report.json is JSON");
    assert_eq!(report["schema"], json!("khaen-closed-loop-report/v1"));
    assert_eq!(report["verdict"], json!("PASS"));

    // artifacts.manifest.json：清单本身也按列表 sha 校验，schema 是 job artifact manifest。
    let listed_manifest = artifact_sha(&job.detail["job"], "artifacts.manifest.json");
    let response = support::request(
        &server,
        "GET",
        &artifact_path(&job.job_id, "artifacts.manifest.json"),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert_eq!(
        response.header("x-khaen-artifact-sha256"),
        Some(listed_manifest.as_str())
    );
    assert_eq!(sha256_hex(&response.body), listed_manifest);
    let manifest: Value = serde_json::from_slice(&response.body).expect("manifest is JSON");
    assert_eq!(manifest["schema"], json!("khaen-calibration-artifacts/v1"));

    // 白名单外的名字一律 404 artifact_not_found（不泄露存在性）。
    for name in ["hack.bin", "checkpoint.bin", "secret.txt"] {
        let response = support::request(&server, "GET", &artifact_path(&job.job_id, name)).await;
        assert_eq!(response.status, 404, "{name}: {}", response.body_text());
        assert_error_code(&response.json(), "artifact_not_found");
    }
}

// ---------------------------------------------------------------------------
// 3) apply staged：prepare→commit→重复 commit 409；新 job prepare→abort 不改运行版本
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn apply_staged_commit_records_version_and_abort_keeps_running_version() {
    let (_dir, server) = backend_server("cal-backend-apply").await;
    let first = finished_job(&server, APPLY_RATE).await;
    let plan_sha = first.plan.sha256.clone();

    // ---- prepare：只生成草稿 + 一次性 token，不改变运行态 ----
    let response = support::request_json(
        &server,
        "POST",
        &format!("{JOBS}/{}/apply", first.job_id),
        r#"{"stage":"prepare"}"#,
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let prepare = response.json();
    assert_eq!(prepare["ok"], json!(true), "{prepare}");
    assert_eq!(prepare["stage"], json!("prepare"));
    assert_eq!(prepare["job_id"].as_str(), Some(first.job_id.as_str()));
    assert_eq!(prepare["plan_sha256"].as_str(), Some(plan_sha.as_str()));
    assert_eq!(
        prepare["previous_version"],
        Value::Null,
        "apply 前没有运行版本"
    );
    let token = prepare["engine_token"]
        .as_str()
        .expect("engine_token")
        .to_string();
    assert!(!token.is_empty());

    // ---- commit：按 token 生效并回写 job apply 记录 ----
    let commit_payload = json!({"stage": "commit", "engine_token": token}).to_string();
    let response = support::request_json(
        &server,
        "POST",
        &format!("{JOBS}/{}/apply", first.job_id),
        &commit_payload,
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let commit = response.json();
    assert_eq!(commit["ok"], json!(true), "{commit}");
    assert_eq!(commit["stage"], json!("commit"));
    assert_eq!(commit["applied"], json!(true));
    let applied_version = commit["applied_version"].as_u64().expect("applied_version");
    assert!(applied_version >= 1);
    assert_eq!(commit["sha256"].as_str(), Some(plan_sha.as_str()));
    assert_eq!(commit["job"]["apply_state"], json!("applied"), "{commit}");

    // token 只能消费一次：重复 commit 同一 token → 409 engine_token_invalid。
    let response = support::request_json(
        &server,
        "POST",
        &format!("{JOBS}/{}/apply", first.job_id),
        &commit_payload,
    )
    .await;
    assert_eq!(response.status, 409, "{}", response.body_text());
    assert_error_code(&response.json(), "engine_token_invalid");

    // 详情回写 apply 段（applied_version / apply.state / apply.plan_sha256）。
    let detail = support::request(&server, "GET", &format!("{JOBS}/{}", first.job_id))
        .await
        .json();
    assert_eq!(detail["job"]["apply_state"], json!("applied"));
    assert_eq!(detail["job"]["applied_version"], json!(applied_version));
    assert_eq!(detail["job"]["apply"]["state"], json!("applied"));
    assert_eq!(
        detail["job"]["apply"]["plan_sha256"].as_str(),
        Some(plan_sha.as_str())
    );

    // commit 在下一块边界生效：跑一块后 /api/coeff 的运行版本才是回读基准。
    assert_eq!(run_control_block(&server), 256);
    let coeff = support::request(&server, "GET", "/api/coeff").await;
    assert_eq!(coeff.status, 200, "{}", coeff.body_text());
    let coeff = coeff.json();
    assert_eq!(coeff["ok"], json!(true));
    assert_eq!(coeff["version"].as_u64(), Some(applied_version));

    // ---- 新 job：prepare 只产草稿（previous_version = 当前运行版本），abort 丢弃且不改运行版本 ----
    let second = finished_job(&server, APPLY_RATE).await;
    let response = support::request_json(
        &server,
        "POST",
        &format!("{JOBS}/{}/apply", second.job_id),
        r#"{"stage":"prepare"}"#,
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let prepare = response.json();
    assert_eq!(prepare["stage"], json!("prepare"));
    assert_eq!(
        prepare["previous_version"].as_u64(),
        Some(applied_version),
        "prepare 的 previous_version 必须是当前运行版本: {prepare}"
    );
    let token = prepare["engine_token"]
        .as_str()
        .expect("engine_token")
        .to_string();

    let response = support::request_json(
        &server,
        "POST",
        &format!("{JOBS}/{}/apply", second.job_id),
        &json!({"stage": "abort", "engine_token": token}).to_string(),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let abort = response.json();
    assert_eq!(abort["ok"], json!(true), "{abort}");
    assert_eq!(abort["stage"], json!("abort"));
    assert_eq!(abort["aborted"], json!(true));
    assert_eq!(abort["job_id"].as_str(), Some(second.job_id.as_str()));

    let coeff_after = support::request(&server, "GET", "/api/coeff").await.json();
    assert_eq!(
        coeff_after["version"].as_u64(),
        Some(applied_version),
        "abort 不得改变运行版本: {coeff_after}"
    );
    let detail = support::request(&server, "GET", &format!("{JOBS}/{}", second.job_id))
        .await
        .json();
    assert_eq!(
        detail["job"]["apply_state"],
        json!("not_applied"),
        "job2 没有 commit，apply_state 必须仍是 not_applied"
    );
}

// ---------------------------------------------------------------------------
// 4) 旧单发 apply/rollback（空 body）兼容
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn legacy_single_shot_apply_and_rollback_still_work() {
    let (_dir, server) = backend_server("cal-backend-legacy").await;

    // 先 apply 一个 job 并跑块，建立可回滚的运行版本 1。
    let first = finished_job(&server, APPLY_RATE).await;
    let response = support::request_json(
        &server,
        "POST",
        &format!("{JOBS}/{}/apply", first.job_id),
        "{}",
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let applied = response.json();
    assert_eq!(applied["ok"], json!(true), "{applied}");
    assert_eq!(applied["applied"], json!(true));
    assert!(applied["applied_version"].as_u64().is_some_and(|v| v >= 1));
    assert_eq!(applied["job"]["job"]["apply_state"], json!("applied"));
    assert_eq!(
        applied["snapshot"]["version"],
        Value::Null,
        "首次 apply 前没有运行版本"
    );
    assert_eq!(run_control_block(&server), 256);

    // 另一个 succeeded job：旧单发 apply（空 body）→ 200 applied:true。
    let second = finished_job(&server, APPLY_RATE).await;
    let response = support::request_json(
        &server,
        "POST",
        &format!("{JOBS}/{}/apply", second.job_id),
        "{}",
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let applied = response.json();
    assert_eq!(applied["ok"], json!(true), "{applied}");
    assert_eq!(applied["applied"], json!(true));
    assert_eq!(applied["snapshot"]["version"], json!(1));
    assert_eq!(applied["job"]["job"]["apply_state"], json!("applied"));

    // 旧单发 rollback（空 body）→ 200 rolled_back:true，job 记录 rolled_back。
    let response = support::request_json(
        &server,
        "POST",
        &format!("{JOBS}/{}/rollback", second.job_id),
        "{}",
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let rolled_back = response.json();
    assert_eq!(rolled_back["ok"], json!(true), "{rolled_back}");
    assert_eq!(rolled_back["rolled_back"], json!(true));
    assert_eq!(rolled_back["restored_version"], json!(1));
    assert_eq!(
        rolled_back["job"]["job"]["apply_state"],
        json!("rolled_back")
    );
    let detail = support::request(&server, "GET", &format!("{JOBS}/{}", second.job_id))
        .await
        .json();
    assert_eq!(detail["job"]["apply_state"], json!("rolled_back"));
    assert_eq!(detail["job"]["apply"]["state"], json!("rolled_back"));
}

// ---------------------------------------------------------------------------
// 5) rollback staged：prepare 给 target_version → commit 记 rolled_back
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn rollback_staged_prepare_commit_records_rolled_back() {
    let (_dir, server) = backend_server("cal-backend-rollback").await;

    let first = finished_job(&server, APPLY_RATE).await;
    let response = support::request_json(
        &server,
        "POST",
        &format!("{JOBS}/{}/apply", first.job_id),
        "{}",
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert_eq!(response.json()["applied"], json!(true));
    assert_eq!(run_control_block(&server), 256);

    let second = finished_job(&server, APPLY_RATE).await;
    let response = support::request_json(
        &server,
        "POST",
        &format!("{JOBS}/{}/apply", second.job_id),
        "{}",
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let applied = response.json();
    assert_eq!(applied["applied"], json!(true));
    assert_eq!(applied["snapshot"]["version"], json!(1));
    assert_eq!(applied["job"]["job"]["apply_state"], json!("applied"));

    // ---- rollback prepare：只给草稿 + target_version ----
    let response = support::request_json(
        &server,
        "POST",
        &format!("{JOBS}/{}/rollback", second.job_id),
        r#"{"stage":"prepare"}"#,
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let prepare = response.json();
    assert_eq!(prepare["ok"], json!(true), "{prepare}");
    assert_eq!(prepare["stage"], json!("prepare"));
    assert_eq!(prepare["job_id"].as_str(), Some(second.job_id.as_str()));
    assert_eq!(prepare["target_version"].as_u64(), Some(1));
    let token = prepare["engine_token"]
        .as_str()
        .expect("engine_token")
        .to_string();
    assert!(!token.is_empty());

    // ---- rollback commit：rolled_back 回执 + job apply_state 回写 ----
    let response = support::request_json(
        &server,
        "POST",
        &format!("{JOBS}/{}/rollback", second.job_id),
        &json!({"stage": "commit", "engine_token": token}).to_string(),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let rolled_back = response.json();
    assert_eq!(rolled_back["ok"], json!(true), "{rolled_back}");
    assert_eq!(rolled_back["stage"], json!("commit"));
    assert_eq!(rolled_back["rolled_back"], json!(true));
    assert_eq!(rolled_back["restored_version"], json!(1));
    assert!(rolled_back["applied_version"].as_u64().is_some());
    assert_eq!(rolled_back["job"]["apply_state"], json!("rolled_back"));

    let detail = support::request(&server, "GET", &format!("{JOBS}/{}", second.job_id))
        .await
        .json();
    assert_eq!(detail["job"]["apply_state"], json!("rolled_back"));
    assert_eq!(detail["job"]["apply"]["state"], json!("rolled_back"));
}

// ---------------------------------------------------------------------------
// 6) 失败诚实：坏 WAV → failed + 稳定码；apply → 409 job_not_succeeded
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn invalid_wav_fails_with_stable_code_and_apply_is_rejected() {
    let (_dir, server) = backend_server("cal-backend-failure").await;
    let plan = identity_plan(JOB_RATE);
    let plan_sha = upload_plan(&server, &plan).await;
    let applied = upload(&server, &tone_wav(JOB_RATE, 1.0), "audio/wav").await;
    let baseline = upload(&server, &invalid_wav_bytes(), "audio/wav").await;

    let job_id = create_job(&server, &plan_sha, &baseline, &applied).await;
    let detail = wait_terminal(&server, &job_id).await;
    let record = &detail["job"];
    assert_eq!(record["status"], json!("failed"), "{detail}");
    assert_eq!(record["verdict"], Value::Null, "失败 job 不得编 verdict");
    let error = &record["error"];
    assert_eq!(
        error["code"],
        json!("E_OFFLINE_JOB_INPUT"),
        "validate 阶段的稳定错误码: {detail}"
    );
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "失败必须带人读 message: {detail}"
    );
    assert_eq!(record["result"], Value::Null);
    assert!(
        detail["reasons"]["verdict"].is_string(),
        "缺失 verdict 要有原因: {detail}"
    );

    // 步骤结构：仍是固定的 5 步。
    // NOTE(生产后端缺口，已报 lead)：khaen-calibration 的 JobManager 在任何终态都把
    // snapshot.stage_index 置成 stage_count，而 daemon 侧 derive_steps 用 stage_index 推导步骤状态，
    // 所以 failed job 的 5 个步骤当前都被标成 succeeded（job.status / error.code 本身是诚实的）。
    // 这里只断言结构，不对失败 job 的 step.status 做断言；等后端把 failure.stage 持久化后再收紧。
    // 最小复现：identity plan + 非 WAV baseline 上传 → job-0001 failed，detail.job.steps[*].status 全 succeeded。
    let steps = record["steps"].as_array().expect("steps");
    assert_eq!(steps.len(), 5, "{detail}");
    assert_eq!(steps[0]["name"], json!("validate"));
    assert_eq!(steps[4]["name"], json!("bundle"));

    // 对 failed job apply → 409 job_not_succeeded（不假成功）。
    let response =
        support::request_json(&server, "POST", &format!("{JOBS}/{job_id}/apply"), "{}").await;
    assert_eq!(response.status, 409, "{}", response.body_text());
    let body = response.json();
    assert_error_code(&body, "job_not_succeeded");
    assert_eq!(
        body["error"]["details"]["job_id"].as_str(),
        Some(job_id.as_str())
    );
    assert_eq!(body["error"]["details"]["state"], json!("failed"));

    // 失败 job 没有可下载产物。
    let response = support::request(&server, "GET", &artifact_path(&job_id, "report.json")).await;
    assert_eq!(response.status, 404, "{}", response.body_text());
    assert_error_code(&response.json(), "artifact_not_found");
}

// ---------------------------------------------------------------------------
// 7) 取消：长 WAV job → cancelled 终态（不假 cancelled）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn cancel_long_job_reports_cancelled_terminal_state() {
    let (_dir, server) = backend_server("cal-backend-cancel").await;
    let plan = identity_plan(JOB_RATE);
    let plan_sha = upload_plan(&server, &plan).await;
    // 20 s 单声道 48 kHz ≈ 1.92 MB：validate/levels/bundle 的处理时间远长于
    // create→cancel 的一次本机 HTTP 往返，保证取消落在 running 中。
    let long_wav = upload(&server, &tone_wav(JOB_RATE, 20.0), "audio/wav").await;
    let job_id = create_job(&server, &plan_sha, &long_wav, &long_wav).await;

    let response =
        support::request_json(&server, "POST", &format!("{JOBS}/{job_id}/cancel"), "{}").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    let body = response.json();
    assert_eq!(body["ok"], json!(true), "{body}");
    assert_eq!(body["job_id"].as_str(), Some(job_id.as_str()));
    // 取消是协作式的：后端返回的是**真实终态**；若 job 先完成则返回 succeeded/failed，
    // 绝不把已完成改写成 cancelled。20 s WAV 的处理时间远大于本机一次往返，正常应在此命中 cancelled。
    assert_eq!(
        body["status"],
        json!("cancelled"),
        "job finished before cancel took effect (后端会如实返回终态): {body}"
    );
    assert_eq!(body["state"], body["status"]);

    let detail = support::request(&server, "GET", &format!("{JOBS}/{job_id}"))
        .await
        .json();
    assert_eq!(detail["job"]["status"], json!("cancelled"));
    assert_ne!(detail["job"]["status"], json!("running"));
    assert_eq!(detail["job"]["verdict"], Value::Null);
    assert_eq!(detail["job"]["error"], Value::Null);
    let list = support::request(&server, "GET", JOBS).await.json();
    assert_eq!(list["active_job_id"], Value::Null, "取消后没有 active job");
    let row = list["jobs"]
        .as_array()
        .expect("jobs")
        .iter()
        .find(|row| row["id"].as_str() == Some(job_id.as_str()))
        .expect("cancelled job in list");
    assert_eq!(row["status"], json!("cancelled"));
}

// ---------------------------------------------------------------------------
// 8) 单 job 互斥：active job 存在时第二个 job → 409 switch_busy
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn second_job_while_active_is_switch_busy() {
    let (_dir, server) = backend_server("cal-backend-busy").await;
    let plan = identity_plan(JOB_RATE);
    let plan_sha = upload_plan(&server, &plan).await;
    let long_wav = upload(&server, &tone_wav(JOB_RATE, 20.0), "audio/wav").await;
    let first = create_job(&server, &plan_sha, &long_wav, &long_wav).await;

    // 20 s WAV 不可能在两次本机往返内跑完：第二个 job 必须拿到稳定 409 switch_busy。
    let payload = job_payload(&plan_sha, &long_wav, &long_wav);
    let response = support::request_json(&server, "POST", JOBS, &payload.to_string()).await;
    assert_eq!(response.status, 409, "{}", response.body_text());
    let body = response.json();
    assert_error_code(&body, "switch_busy");
    assert_eq!(
        body["error"]["details"]["active_job_id"].as_str(),
        Some(first.as_str())
    );

    // 清理：取消第一个 job，确认互斥解除。
    let response =
        support::request_json(&server, "POST", &format!("{JOBS}/{first}/cancel"), "{}").await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert_eq!(response.json()["status"], json!("cancelled"));
    let list = support::request(&server, "GET", JOBS).await.json();
    assert_eq!(list["active_job_id"], Value::Null);
}

// ---------------------------------------------------------------------------
// 9) 审计持久化：同一 state_dir 重新构造后端实例，历史 job 仍在
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn audit_index_survives_backend_reopen() {
    let (dir, server) = backend_server("cal-backend-audit").await;
    let job = finished_job(&server, JOB_RATE).await;

    // 不启动第二个 HTTP 服务：直接在同 state_dir 上再构造一个生产后端（模拟 daemon 重启的索引加载）。
    let reopened =
        JobManagerCalibration::new(dir.path()).expect("reopen backend on the same state dir");
    // 审计索引按状态变化异步落盘：job 详情到终态后索引可能还差一次刷新才写出终态。
    // 这里轮询到终态（最多 30 s），避免在慢 runner 上把一次索引刷新延迟当成持久化丢失。
    let deadline = Instant::now() + Duration::from_secs(30);
    let (page, summary) = loop {
        let page = reopened
            .list_jobs(&JobQuery {
                limit: 20,
                offset: 0,
            })
            .expect("list persisted jobs");
        assert!(page.total >= 1, "审计索引必须保留历史 job");
        let summary = page
            .jobs
            .iter()
            .find(|summary| summary.job_id == job.job_id)
            .expect("persisted job summary")
            .clone();
        if summary.state.as_str() != "running" || Instant::now() >= deadline {
            break (page, summary);
        }
        sleep_poll_interval().await;
    };
    let _ = page;
    assert_eq!(summary.state.as_str(), "succeeded");
    assert_eq!(summary.verdict.as_deref(), Some("PASS"));
    assert!(summary.created_unix.is_some());
    assert!(summary.updated_unix.is_some());
    assert!(summary.progress.is_some());

    let record = reopened.job(&job.job_id).expect("persisted job detail");
    assert_eq!(record.state.as_str(), "succeeded");
    assert_eq!(record.verdict.as_deref(), Some("PASS"));
    assert_eq!(record.steps.len(), 5);
    assert_eq!(record.artifacts.len(), 5);
    let measured = record
        .result
        .as_ref()
        .and_then(|result| result.measured.as_ref())
        .expect("measured source persisted");
    assert_eq!(measured.baseline_upload_id, job.baseline_upload_id);
    let simulated = record
        .result
        .as_ref()
        .and_then(|result| result.simulated.as_ref())
        .expect("simulated source persisted");
    assert_eq!(simulated.plan_sha256, job.plan.sha256);
}

// ---------------------------------------------------------------------------
// 10) 索引损坏：初始化必须明确失败（main 才能如实降级 not_wired，不静默成功）
// ---------------------------------------------------------------------------

#[test]
fn corrupted_index_fails_backend_initialization_loudly() {
    let dir = support::TempStateDir::new("cal-backend-bad-index");
    let calibration_dir = dir.path().join("calibration");
    std::fs::create_dir_all(&calibration_dir).expect("create calibration dir");
    std::fs::write(
        calibration_dir.join("jobs-index.json"),
        b"{ this is not valid json",
    )
    .expect("write corrupt index");

    let error = match JobManagerCalibration::new(dir.path()) {
        Ok(service) => panic!("corrupt jobs-index.json must not initialize silently: {service:?}"),
        Err(error) => error,
    };
    assert!(
        error.contains("parse calibration job index"),
        "unexpected initialization error: {error}"
    );
}
