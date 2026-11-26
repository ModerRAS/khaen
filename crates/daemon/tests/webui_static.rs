//! M0-A 静态托管端到端测试：真实 TCP → 真实 axum → 编译期内嵌的 `webui/dist` 表。
//!
//! 覆盖：index（`/` 与 `/index.html`）、表内每个静态资源逐字节比对 + Content-Type、
//! 未知路径 SPA 回退、查询串不影响查找、目录穿越一律拒绝且不泄露文件、HEAD 无 body。

mod support;

use khaen_daemon::webui_assets;

fn index() -> webui_assets::StaticAsset {
    webui_assets::index()
}

#[tokio::test(flavor = "current_thread")]
async fn embedded_dist_is_not_empty_and_has_a_sane_index() {
    assert!(webui_assets::count() > 0, "build.rs 必须内嵌 webui/dist");
    let index = index();
    assert_eq!(index.path, "/index.html");
    assert_eq!(index.content_type, "text/html; charset=utf-8");
    assert!(!index.bytes.is_empty(), "index.html 不能为空");

    // 内嵌表的不变量：都是绝对路径、无穿越段、mime 与字节非空。
    for (path, mime, bytes) in webui_assets::table() {
        assert!(path.starts_with('/'), "URL 路径必须绝对：{path}");
        assert!(!path.contains(".."), "内嵌表不许有穿越段：{path}");
        assert!(!mime.is_empty(), "mime 不能为空：{path}");
        assert!(!bytes.is_empty(), "资源不能为空：{path}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn every_embedded_asset_is_served_byte_for_byte() {
    let server = support::spawn().await;
    for (path, mime, bytes) in webui_assets::table() {
        let response = support::request(&server, "GET", path).await;
        assert_eq!(response.status, 200, "{path}");
        assert_eq!(response.header("content-type"), Some(*mime), "{path}");
        assert_eq!(response.body.as_slice(), *bytes, "{path}");
        // /assets/*（文件名带内容 hash）长缓存；入口/无 hash 资源 no-store。
        let expected_cache = if path.starts_with("/assets/") {
            webui_assets::IMMUTABLE_CACHE_CONTROL
        } else {
            webui_assets::NO_STORE_CACHE_CONTROL
        };
        assert_eq!(
            response.header("cache-control"),
            Some(expected_cache),
            "{path}"
        );
        assert_eq!(
            response.header("x-content-type-options"),
            Some("nosniff"),
            "{path}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn root_and_index_serve_the_spa_index_with_no_store() {
    let server = support::spawn().await;
    let index = index();
    for path in ["/", "/index.html"] {
        let response = support::request(&server, "GET", path).await;
        assert_eq!(response.status, 200, "{path}");
        assert_eq!(
            response.header("content-type"),
            Some(index.content_type),
            "{path}"
        );
        assert_eq!(response.body.as_slice(), index.bytes, "{path}");
        assert_eq!(
            response.header("cache-control"),
            Some(webui_assets::NO_STORE_CACHE_CONTROL),
            "{path}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_non_api_paths_fall_back_to_index() {
    let server = support::spawn().await;
    let index = index();
    for path in [
        "/volume",
        "/volume/deep/link",
        "/routing",
        "/nope/x/y",
        "/assets/",
        "/assets",
    ] {
        let response = support::request(&server, "GET", path).await;
        assert_eq!(response.status, 200, "{path}");
        assert_eq!(
            response.header("content-type"),
            Some(index.content_type),
            "{path}"
        );
        assert_eq!(response.body.as_slice(), index.bytes, "{path}");
        assert_eq!(
            response.header("cache-control"),
            Some(webui_assets::NO_STORE_CACHE_CONTROL),
            "{path}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn query_string_is_ignored_for_static_lookup() {
    let server = support::spawn().await;
    let index = index();
    let response = support::request(&server, "GET", "/volume?tab=eq&x=1").await;
    assert_eq!(response.status, 200);
    assert_eq!(response.body.as_slice(), index.bytes);
}

#[tokio::test(flavor = "current_thread")]
async fn traversal_paths_are_rejected_and_never_leak_files() {
    let server = support::spawn().await;
    // Cargo.toml 的特征串：真被穿越读到就会出现在响应里（回退 index 不会包含它）。
    let leaked_marker = "[workspace]";
    for path in [
        "/../Cargo.toml",
        "/../../Cargo.toml",
        "/assets/../../Cargo.toml",
        "/%2e%2e/Cargo.toml",
        "/%2E%2E/%2E%2E/Cargo.toml",
        "/..%2f..%2fCargo.toml",
    ] {
        let response = support::request(&server, "GET", path).await;
        assert_eq!(response.status, 404, "{path} 应显式拒绝");
        assert!(
            !response.body_text().contains(leaked_marker),
            "{path} 泄露了 dist 之外的文件: {}",
            response.body_text()
        );
        let body = response.json();
        assert_eq!(
            body["error"],
            serde_json::Value::String("not_found".into()),
            "{path}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn head_returns_headers_without_body() {
    let server = support::spawn().await;
    let index = index();
    let response = support::request(&server, "HEAD", "/").await;
    assert_eq!(response.status, 200);
    assert_eq!(response.header("content-type"), Some(index.content_type));
    let expected_len = index.bytes.len().to_string();
    assert_eq!(
        response.header("content-length"),
        Some(expected_len.as_str())
    );
    assert!(response.body.is_empty(), "HEAD 不应有 body");
}
