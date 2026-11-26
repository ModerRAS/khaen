//! T36 校准上传端到端测试：真实 TCP + 真实 axum 服务 + 临时状态目录。
//!
//! 覆盖：正常上传（内容寻址/幂等）、媒体类型、空 body、Content-Length 超限（413）、
//! 未鉴权 401、写操作缺 CSRF 403、PLAN §4.1 错误体形状。

mod support;

use std::path::Path;

use serde_json::{json, Value};

use khaen_host::sha256_hex;

const UPLOAD_PATH: &str = "/api/calibration/uploads";

fn upload_dir(state_dir: &Path) -> std::path::PathBuf {
    state_dir.join("uploads")
}

fn assert_error_shape(body: &Value, expected_code: &str) {
    let error = body
        .get("error")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("error body must be an object: {body}"));
    assert_eq!(
        error.len(),
        3,
        "PLAN §4.1 error body must be exactly {{code,message,details}}: {body}"
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

#[tokio::test(flavor = "current_thread")]
async fn upload_stores_raw_bytes_content_addressed_and_idempotent() {
    let server = support::spawn().await;
    let payload = b"RIFF....WAVEfmt baseline-wav-bytes";
    let response = support::request_bytes(
        &server,
        "POST",
        UPLOAD_PATH,
        "application/octet-stream",
        payload,
    )
    .await;
    assert_eq!(response.status, 201, "{}", response.body_text());
    // 现有 auth 中间件给全部 /api 响应补的版本/缓存头，新端点同样具备。
    assert_eq!(response.header("x-khaen-api-version"), Some("1"));
    assert_eq!(response.header("cache-control"), Some("no-store"));
    assert!(response.header("x-khaen-web-version").is_some());
    let body = response.json();
    assert_eq!(body["sha256"], json!(sha256_hex(payload)));
    assert_eq!(body["upload_id"], json!(sha256_hex(payload)));
    assert_eq!(body["bytes"], json!(payload.len() as u64));
    assert_eq!(body["created"], json!(true));

    // 文件真的落在 <state_dir>/uploads/<sha256>，内容逐字节一致。
    let path = upload_dir(&server.state_dir).join(sha256_hex(payload));
    assert!(
        path.is_file(),
        "upload must be stored at {}",
        path.display()
    );
    assert_eq!(std::fs::read(&path).expect("read upload"), payload);

    // 同内容再传：200 + created=false（幂等），id 不变。
    let again = support::request_bytes(
        &server,
        "POST",
        UPLOAD_PATH,
        "application/octet-stream",
        payload,
    )
    .await;
    assert_eq!(again.status, 200, "{}", again.body_text());
    let body = again.json();
    assert_eq!(body["upload_id"], json!(sha256_hex(payload)));
    assert_eq!(body["created"], json!(false));

    // 不同内容 → 不同 id。
    let other: &[u8] = b"different-bytes";
    let response = support::request_bytes(
        &server,
        "POST",
        UPLOAD_PATH,
        "application/octet-stream",
        other,
    )
    .await;
    assert_eq!(response.status, 201);
    assert_ne!(response.json()["upload_id"], json!(sha256_hex(payload)));

    // 上传大小上限是 64 MiB（写进 details，前端可提示）。
    assert_eq!(
        khaen_daemon::calibration::store::MAX_UPLOAD_BYTES,
        64 * 1024 * 1024
    );
}

#[tokio::test(flavor = "current_thread")]
async fn upload_accepts_audio_wav_and_parameters() {
    let server = support::spawn().await;
    let response = support::request_bytes(
        &server,
        "POST",
        UPLOAD_PATH,
        "audio/wav; codecs=1",
        b"wav-bytes",
    )
    .await;
    assert_eq!(response.status, 201, "{}", response.body_text());
    assert_eq!(response.json()["bytes"], json!(9));
}

#[tokio::test(flavor = "current_thread")]
async fn upload_rejects_unsupported_media_types_with_415() {
    let server = support::spawn().await;
    for content_type in ["text/plain", "application/json", "audio/mpeg"] {
        let response =
            support::request_bytes(&server, "POST", UPLOAD_PATH, content_type, b"x").await;
        assert_eq!(response.status, 415, "{content_type}");
        assert_error_shape(&response.json(), "unsupported_format");
        assert_eq!(
            response.json()["error"]["details"]["content_type"],
            json!(content_type)
        );
    }
    // 完全不带 Content-Type：同样 415（不猜类型）。
    let session = server.session().expect("session").clone();
    let auth_only = vec![
        (
            "Cookie".to_string(),
            format!("khaen_session={}", session.token),
        ),
        ("X-KHAEN-CSRF".to_string(), session.csrf.clone()),
    ];
    let response =
        support::request_bytes_with(&server, "POST", UPLOAD_PATH, &auth_only, Some(b"x")).await;
    assert_eq!(response.status, 415, "{}", response.body_text());
    assert_error_shape(&response.json(), "unsupported_format");
}

#[tokio::test(flavor = "current_thread")]
async fn upload_rejects_empty_body() {
    let server = support::spawn().await;
    let response = support::request_bytes(
        &server,
        "POST",
        UPLOAD_PATH,
        "application/octet-stream",
        b"",
    )
    .await;
    assert_eq!(response.status, 400, "{}", response.body_text());
    assert_error_shape(&response.json(), "invalid_request");
}

#[tokio::test(flavor = "current_thread")]
async fn upload_rejects_oversize_by_content_length_with_413() {
    let server = support::spawn().await;
    let session = server.session().expect("session").clone();
    let too_large = khaen_daemon::calibration::store::MAX_UPLOAD_BYTES + 1;
    let headers = vec![
        (
            "Cookie".to_string(),
            format!("khaen_session={}", session.token),
        ),
        ("X-KHAEN-CSRF".to_string(), session.csrf.clone()),
        (
            "Content-Type".to_string(),
            "application/octet-stream".to_string(),
        ),
        ("Content-Length".to_string(), too_large.to_string()),
    ];
    let response = support::request_bytes_with(&server, "POST", UPLOAD_PATH, &headers, None).await;
    assert_eq!(response.status, 413, "{}", response.body_text());
    let body = response.json();
    assert_error_shape(&body, "upload_too_large");
    assert_eq!(
        body["error"]["details"]["limit_bytes"],
        json!(too_large - 1)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn upload_requires_session_and_csrf() {
    let server = support::spawn().await;
    // 匿名（列表/上传都要登录会话）。
    let anonymous = support::request_bytes_with(
        &server,
        "POST",
        UPLOAD_PATH,
        &[("Content-Type".into(), "application/octet-stream".into())],
        Some(b"x"),
    )
    .await;
    assert_eq!(anonymous.status, 401, "{}", anonymous.body_text());
    assert_error_shape(&anonymous.json(), "unauthorized");

    // 有会话但缺 CSRF：403 forbidden（Cookie 会话的写方法必须带 X-KHAEN-CSRF）。
    let session = server.session().expect("session").clone();
    let headers = vec![
        (
            "Cookie".to_string(),
            format!("khaen_session={}", session.token),
        ),
        (
            "Content-Type".to_string(),
            "application/octet-stream".to_string(),
        ),
    ];
    let forbidden =
        support::request_bytes_with(&server, "POST", UPLOAD_PATH, &headers, Some(b"x")).await;
    assert_eq!(forbidden.status, 403, "{}", forbidden.body_text());
    assert_error_shape(&forbidden.json(), "forbidden");
}

#[tokio::test(flavor = "current_thread")]
async fn upload_path_is_never_influenced_by_client_input() {
    let server = support::spawn().await;
    // 同样的字节 + 恶意请求头也只会落在 uploads/<sha256>；客户端无法指定文件名。
    let headers = {
        let session = server.session().expect("session").clone();
        vec![
            (
                "Cookie".to_string(),
                format!("khaen_session={}", session.token),
            ),
            ("X-KHAEN-CSRF".to_string(), session.csrf.clone()),
            (
                "Content-Type".to_string(),
                "application/octet-stream".to_string(),
            ),
            (
                "X-KHAEN-Filename".to_string(),
                "../../auth.json".to_string(),
            ),
        ]
    };
    let response =
        support::request_bytes_with(&server, "POST", UPLOAD_PATH, &headers, Some(b"payload")).await;
    assert_eq!(response.status, 201, "{}", response.body_text());
    let body = response.json();
    let upload_id = body["upload_id"].as_str().expect("upload_id");
    let entries: Vec<String> = std::fs::read_dir(upload_dir(&server.state_dir))
        .expect("uploads dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, vec![upload_id.to_string()]);
    assert!(
        server.state_dir.join("auth.json").is_file(),
        "auth.json 不该被动过"
    );
}
