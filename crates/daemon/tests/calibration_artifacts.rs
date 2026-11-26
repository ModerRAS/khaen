//! T36 校准产物下载端到端测试（白名单 / manifest sha256 校验 / Content-Type）。
//!
//! 产物清单来自注入的 fake 服务（对应未来 backend 的 artifacts.manifest.json）；daemon 负责
//! 白名单、重新计算 sha256 与 media type，字节永远不经过控制面。

mod support;

use serde_json::{json, Value};

use khaen_host::sha256_hex;
use support::calibration_fake::FakeCalibration;

const JOBS: &str = "/api/calibration/jobs";

fn artifact_path(job_id: &str, name: &str) -> String {
    format!("{JOBS}/{job_id}/artifacts/{name}")
}

fn assert_error_code(body: &Value, expected_code: &str) {
    assert_eq!(
        body["error"]["code"],
        json!(expected_code),
        "unexpected error body: {body}"
    );
    assert!(body["error"]["message"]
        .as_str()
        .is_some_and(|text| !text.is_empty()));
    assert!(body["error"]["details"].is_object());
}

#[tokio::test(flavor = "current_thread")]
async fn downloads_whitelisted_artifact_with_verified_sha_and_content_type() {
    let fake = FakeCalibration::new();
    let server = support::spawn_with_calibration(fake.clone()).await;
    let job_id = fake.seed_succeeded("PASS", &"a".repeat(64), "graph");
    let report = br#"{"schema":"khaen-calibration-report/v1","verdict":"PASS"}"#;
    fake.add_artifact(&job_id, "report.json", report);
    fake.add_artifact(&job_id, "plan.bin", b"\x00\x01plan-bytes");

    let response = support::request(&server, "GET", &artifact_path(&job_id, "report.json")).await;
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert_eq!(response.body, report.to_vec());
    assert_eq!(response.header("content-type"), Some("application/json"));
    assert_eq!(
        response.header("x-khaen-artifact-sha256"),
        Some(sha256_hex(report).as_str())
    );
    assert!(response
        .header("content-disposition")
        .is_some_and(|value| value.contains("report.json")));

    let binary = support::request(&server, "GET", &artifact_path(&job_id, "plan.bin")).await;
    assert_eq!(binary.status, 200, "{}", binary.body_text());
    assert_eq!(
        binary.header("content-type"),
        Some("application/octet-stream")
    );
    assert_eq!(binary.body, b"\x00\x01plan-bytes".to_vec());
}

#[tokio::test(flavor = "current_thread")]
async fn rejects_sha_mismatch_with_409() {
    let fake = FakeCalibration::new();
    let server = support::spawn_with_calibration(fake.clone()).await;
    let job_id = fake.seed_succeeded("PASS", &"b".repeat(64), "graph");
    let bytes = b"corrupted-artifact-bytes";
    let declared = "0".repeat(64);
    fake.add_artifact_with_sha(&job_id, "levels.json", bytes, &declared);

    let response = support::request(&server, "GET", &artifact_path(&job_id, "levels.json")).await;
    assert_eq!(response.status, 409, "{}", response.body_text());
    let body = response.json();
    assert_error_code(&body, "artifact_sha_mismatch");
    assert_eq!(body["error"]["details"]["expected_sha256"], json!(declared));
    assert_eq!(
        body["error"]["details"]["actual_sha256"],
        json!(sha256_hex(bytes))
    );
    assert_eq!(body["error"]["details"]["name"], json!("levels.json"));
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_or_non_whitelisted_artifacts_are_404() {
    let fake = FakeCalibration::new();
    let server = support::spawn_with_calibration(fake.clone()).await;
    let job_id = fake.seed_succeeded("PASS", &"c".repeat(64), "graph");
    fake.add_artifact(&job_id, "report.json", b"{}");
    // 不在 manifest 的白名单名字。
    let response = support::request(&server, "GET", &artifact_path(&job_id, "plan.bin")).await;
    assert_eq!(response.status, 404, "{}", response.body_text());
    assert_error_code(&response.json(), "artifact_not_found");
    // 不在白名单的任意后端文件（例如 checkpoint）→ 404，不暴露存在性。
    for name in ["calibration-job.checkpoint", "secret.txt", "unrelated.log"] {
        fake.add_artifact(&job_id, name, b"secret");
        let response = support::request(&server, "GET", &artifact_path(&job_id, name)).await;
        assert_eq!(response.status, 404, "{name}: {}", response.body_text());
        assert_error_code(&response.json(), "artifact_not_found");
    }
    // 未知 job → 服务 404 job_not_found。
    let response =
        support::request(&server, "GET", &artifact_path("job-9999", "report.json")).await;
    assert_eq!(response.status, 404, "{}", response.body_text());
    assert_error_code(&response.json(), "job_not_found");
}

#[tokio::test(flavor = "current_thread")]
async fn traversal_artifact_names_are_rejected() {
    let fake = FakeCalibration::new();
    let server = support::spawn_with_calibration(fake.clone()).await;
    let job_id = fake.seed_succeeded("PASS", &"d".repeat(64), "graph");
    fake.add_artifact(&job_id, "report.json", b"{}");
    // 百分号编码的 ../：axum 解出带分隔符的名字，必须 404 而不是读到别的文件。
    for name in ["..", "%2E%2E", "..%2F..%2Fauth.json", "report.json%2F.."] {
        let path = artifact_path(&job_id, name);
        let response = support::request(&server, "GET", &path).await;
        assert_eq!(response.status, 404, "{name}: {}", response.body_text());
        let body = response.json();
        assert!(
            body["error"]["code"] == json!("artifact_not_found")
                || body["error"] == json!("not_found"),
            "{name}: {body}"
        );
    }
    // auth.json 没有被读出来（输出里不含 auth 存储内容）。
    let response = support::request(&server, "GET", &artifact_path(&job_id, "..")).await;
    assert!(!response.body_text().contains("argon2"));
}

#[tokio::test(flavor = "current_thread")]
async fn artifact_download_requires_session() {
    let fake = FakeCalibration::new();
    let server = support::spawn_with_calibration(fake.clone()).await;
    let job_id = fake.seed_succeeded("PASS", &"e".repeat(64), "graph");
    fake.add_artifact(&job_id, "report.json", b"{}");
    let response = support::request_with(
        &server,
        "GET",
        &artifact_path(&job_id, "report.json"),
        &[],
        None,
    )
    .await;
    assert_eq!(response.status, 401, "{}", response.body_text());
    assert_error_code(&response.json(), "unauthorized");
}

#[tokio::test(flavor = "current_thread")]
async fn job_detail_lists_artifacts_with_content_types() {
    let fake = FakeCalibration::new();
    let server = support::spawn_with_calibration(fake.clone()).await;
    let job_id = fake.seed_succeeded("PASS", &"f".repeat(64), "graph");
    fake.add_artifact(&job_id, "report.json", b"{}");
    fake.add_artifact(&job_id, "plan.bin", b"\x00");
    fake.add_artifact(&job_id, "checkpoint.bin", b"hidden");
    let detail = support::request(&server, "GET", &format!("{JOBS}/{job_id}"))
        .await
        .json();
    let artifacts = detail["job"]["artifacts"].as_array().expect("artifacts");
    assert_eq!(artifacts.len(), 3);
    let report = artifacts
        .iter()
        .find(|artifact| artifact["name"] == json!("report.json"))
        .expect("report");
    assert_eq!(report["content_type"], json!("application/json"));
    assert_eq!(report["downloadable"], json!(true));
    assert_eq!(report["sha256"], json!(sha256_hex(b"{}")));
    assert_eq!(report["bytes"], json!(2));
    let hidden = artifacts
        .iter()
        .find(|artifact| artifact["name"] == json!("checkpoint.bin"))
        .expect("checkpoint");
    assert_eq!(hidden["downloadable"], json!(false), "非白名单产物不可下载");
}
