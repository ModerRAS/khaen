//! 静态资源托管：内嵌 `webui/dist` → 精确命中；未知非 `/api` GET → SPA 回退 `index.html`。
//!
//! 安全边界：
//! - 路径查找**只命中编译期内嵌表**，从不访问文件系统（没有 `fs`、没有 `File::open`），
//!   目录穿越在结构上不可能读到 dist 之外的文件。
//! - 另外对 `..` / `.` 段、反斜杠与百分号编码的点/反斜杠显式返回 404：穿越尝试得到确定的
//!   拒绝，而不是悄悄回退成 SPA 首页。
//! - 未知 `/api` / `/api/*` 不参与静态回退，返回结构化 JSON 404，避免前端把 404 当页面。
//!
//! 缓存策略：`/assets/*`（文件名带内容 hash）→ `immutable` 长缓存；入口/回退 → `no-store`。

use axum::body::Body;
use axum::http::header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::webui_assets::{self, StaticAsset, SPA_INDEX_PATH};

/// 未知 API 路径的结构化 404 错误码（与既有错误体 `{ok,error,message}` 同形）。
pub(crate) const ERROR_NOT_FOUND: &str = "not_found";

/// 静态路径解析结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resolution {
    /// 精确命中的内嵌静态资源。
    Asset(StaticAsset),
    /// SPA 回退：未知的非 `/api` GET 一律回 `index.html`。
    SpaIndex(StaticAsset),
    /// 可疑路径（目录穿越等）：不进入资源查找，直接 404。
    Traversal,
}

/// 静态路由的总入口（axum fallback）：先分流 `/api`，再按方法决定静态/404。
pub(crate) async fn fallback(method: Method, uri: Uri) -> Response {
    let path = uri.path();
    if is_api_path(path) {
        return json_error(
            StatusCode::NOT_FOUND,
            ERROR_NOT_FOUND,
            format!("unknown API path: {path}"),
        );
    }
    let head_only = method == Method::HEAD;
    match method {
        Method::GET | Method::HEAD => serve(path, head_only),
        _ => json_error(
            StatusCode::NOT_FOUND,
            ERROR_NOT_FOUND,
            format!("unknown path: {path}"),
        ),
    }
}

/// `/api` 与 `/api/*` 都属于 API 命名空间（未知路径走 JSON 404，绝不回退成 SPA 页面）。
pub(crate) fn is_api_path(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

/// 纯函数：URL 路径 → 解析结果（无 IO、无文件系统访问）。
pub(crate) fn resolve(path: &str) -> Resolution {
    resolve_in(webui_assets::table(), path)
}

/// 在给定表上解析（生产用内嵌表；单元测试用合成表覆盖 `/assets/*` 等分支）。
fn resolve_in(
    table: &'static [(&'static str, &'static str, &'static [u8])],
    path: &str,
) -> Resolution {
    if has_traversal(path) {
        return Resolution::Traversal;
    }
    let normalized = if path == "/" { SPA_INDEX_PATH } else { path };
    match webui_assets::lookup_in(table, normalized) {
        Some(asset) => Resolution::Asset(asset),
        None => Resolution::SpaIndex(webui_assets::index_in(table)),
    }
}

/// 目录穿越防护：拒绝 `..`/`.` 段、反斜杠（Windows 路径语义）与百分号编码的
/// `.` / `/` / `\`（`%2e` / `%2f` / `%5c`）。
///
/// 注意这里不是安全边界（安全边界是"只查内嵌表、不读文件系统"），只是让穿越尝试得到确定 404。
fn has_traversal(path: &str) -> bool {
    if path.contains('\\') {
        return true;
    }
    if path
        .split('/')
        .any(|segment| segment == ".." || segment == ".")
    {
        return true;
    }
    if path.contains('%') {
        let lower = path.to_ascii_lowercase();
        return lower.contains("%2e") || lower.contains("%2f") || lower.contains("%5c");
    }
    false
}

/// 返回静态资源 / SPA 回退（若路径可疑则 404）。
fn serve(path: &str, head_only: bool) -> Response {
    match resolve(path) {
        Resolution::Traversal => json_error(
            StatusCode::NOT_FOUND,
            ERROR_NOT_FOUND,
            format!("path traversal is not allowed: {path}"),
        ),
        Resolution::Asset(asset) | Resolution::SpaIndex(asset) => asset_response(asset, head_only),
    }
}

/// 内嵌资源 → HTTP 响应（Content-Type / Cache-Control / Content-Length；HEAD 不带 body）。
fn asset_response(asset: StaticAsset, head_only: bool) -> Response {
    let body = if head_only {
        Body::empty()
    } else {
        Body::from(asset.bytes)
    };
    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str(asset.content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static(webui_assets::cache_control(asset.path)),
    );
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(CONTENT_LENGTH, HeaderValue::from(asset.bytes.len() as u64));
    response
}

/// 与既有控制面错误体同形的 JSON 响应：`{ok:false,error,message}`。
fn json_error(status: StatusCode, code: &'static str, message: String) -> Response {
    (
        status,
        Json(json!({ "ok": false, "error": code, "message": message })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[(&str, &str, &[u8])] = &[
        (
            "/index.html",
            "text/html; charset=utf-8",
            b"<html>spa</html>",
        ),
        (
            "/assets/app-abc123.js",
            "text/javascript; charset=utf-8",
            b"console.log(1)",
        ),
        ("/favicon.svg", "image/svg+xml; charset=utf-8", b"<svg/>"),
    ];

    #[test]
    fn root_resolves_to_index() {
        let Resolution::Asset(asset) = resolve_in(FIXTURE, "/") else {
            panic!("root must resolve to the embedded index");
        };
        assert_eq!(asset.path, "/index.html");
    }

    #[test]
    fn exact_asset_hit_keeps_its_recorded_type() {
        let Resolution::Asset(asset) = resolve_in(FIXTURE, "/assets/app-abc123.js") else {
            panic!("exact asset path must hit the table");
        };
        assert_eq!(asset.content_type, "text/javascript; charset=utf-8");
        assert_eq!(asset.bytes, b"console.log(1)");
        assert_eq!(
            webui_assets::cache_control(asset.path),
            webui_assets::IMMUTABLE_CACHE_CONTROL
        );
    }

    #[test]
    fn unknown_path_falls_back_to_index() {
        let Resolution::SpaIndex(asset) = resolve_in(FIXTURE, "/volume/deep/link") else {
            panic!("unknown non-api path must fall back to index");
        };
        assert_eq!(asset.path, "/index.html");
        assert_eq!(
            webui_assets::cache_control(asset.path),
            webui_assets::NO_STORE_CACHE_CONTROL
        );
    }

    #[test]
    fn traversal_paths_are_rejected() {
        for path in [
            "/../secret",
            "/assets/../../Cargo.toml",
            "/./index.html",
            "//..",
            "/..%2f..%2fCargo.toml",
            "/%2e%2e/Cargo.toml",
            "/%2E%2E/Cargo.toml",
            "/..\\Cargo.toml",
        ] {
            assert_eq!(resolve_in(FIXTURE, path), Resolution::Traversal, "{path}");
        }
    }

    #[test]
    fn api_namespace_is_never_a_static_path() {
        for path in ["/api", "/api/", "/api/nope", "/api/device/nope"] {
            assert!(is_api_path(path), "{path}");
        }
        for path in ["/", "/apix", "/volume", "/api.html"] {
            assert!(!is_api_path(path), "{path}");
        }
    }

    #[test]
    fn hashed_assets_are_immutable_and_everything_else_is_no_store() {
        assert_eq!(
            webui_assets::cache_control("/assets/index-abc.js"),
            webui_assets::IMMUTABLE_CACHE_CONTROL
        );
        assert_eq!(
            webui_assets::cache_control("/index.html"),
            webui_assets::NO_STORE_CACHE_CONTROL
        );
        assert_eq!(
            webui_assets::cache_control("/favicon.svg"),
            webui_assets::NO_STORE_CACHE_CONTROL
        );
    }
}
