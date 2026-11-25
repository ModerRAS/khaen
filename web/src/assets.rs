//! 内嵌静态资源（无构建步骤、无外部 CDN）。
//!
//! 页面、样式、脚本在编译期通过 include_str!/include_bytes! 打包进二进制，
//! 固件里不需要额外目录；测试音 WAV 同样内嵌。

use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

pub const INDEX_HTML: &str = include_str!("../assets/index.html");
pub const STYLE_CSS: &str = include_str!("../assets/style.css");
pub const APP_JS: &str = include_str!("../assets/app.js");
pub const FAVICON_SVG: &str = include_str!("../assets/favicon.svg");
pub const TEST_TONE_WAV: &[u8] = include_bytes!("../assets/test-tone.wav");

fn text(body: &'static str, content_type: &'static str) -> Response {
    (
        [(header::CONTENT_TYPE, HeaderValue::from_static(content_type))],
        body,
    )
        .into_response()
}

fn bytes(body: &'static [u8], content_type: &'static str) -> Response {
    (
        [(header::CONTENT_TYPE, HeaderValue::from_static(content_type))],
        body,
    )
        .into_response()
}

/// 静态资源入口：公开可访问（不含任何业务数据，数据都在 /api 后面）。
pub async fn static_handler(uri: Uri) -> Response {
    match uri.path() {
        "/" | "/index.html" => text(INDEX_HTML, "text/html; charset=utf-8"),
        "/style.css" => text(STYLE_CSS, "text/css; charset=utf-8"),
        "/app.js" => text(APP_JS, "application/javascript; charset=utf-8"),
        "/favicon.svg" | "/favicon.ico" => text(FAVICON_SVG, "image/svg+xml"),
        "/test-tone.wav" => bytes(TEST_TONE_WAV, "audio/wav"),
        _ => (
            StatusCode::NOT_FOUND,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            )],
            "404 Not Found",
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assets_are_embedded() {
        assert!(INDEX_HTML.contains("泰山派"));
        assert!(STYLE_CSS.contains("--bg"));
        assert!(APP_JS.contains("api"));
        assert_eq!(&TEST_TONE_WAV[0..4], b"RIFF");
        assert!(TEST_TONE_WAV.len() > 48_000);
        assert!(FAVICON_SVG.contains("svg"));
    }
}
