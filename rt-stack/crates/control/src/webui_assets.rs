//! Vue 控制台静态资源表（`webui/dist` 的编译期内嵌版本）。
//!
//! 表由 `crates/control/build.rs` 在编译期扫描仓库根 `webui/dist` 生成
//! （`include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../webui/dist/..."))`），
//! 因此 daemon 二进制自带控制台：不读磁盘、不依赖 Python/Node/反代。
//!
//! - `table()`：`(path, content_type, bytes)` 全集（7 个文件：index.html、favicon.svg、
//!   theme-init.js、assets/*.js|*.css）。
//! - `lookup()`：精确 URL 命中（`/` 归一为 `/index.html`）；未命中返回 `None`，
//!   由 `web::static_asset` 继续走 SPA 回退。
//! - `index()`：SPA 入口（`/`、`/index.html` 与所有未命中的非 `/api` 路径）。
//! - `cache_control()`：带内容 hash 的 `/assets/*` 长缓存，其余（index.html /
//!   favicon.svg / theme-init.js）不缓存。
//!
//! 改前端源码后必须 `cd webui && pnpm build` 并把 `webui/dist` 一起提交（见 `webui/README.md`）；
//! 只改源码不重建时，daemon 发出去的仍是旧表（`rerun-if-changed` 会在 dist 变化后自动重编）。

include!(concat!(env!("OUT_DIR"), "/webui_assets_table.rs"));

/// SPA 入口路径（`webui/dist/index.html`；`/` 是它的别名）。
pub const SPA_INDEX_PATH: &str = "/index.html";

/// 带内容 hash 的静态资源目录（可长缓存；文件名变化即缓存失效）。
pub const HASHED_ASSET_PREFIX: &str = "/assets/";

/// `/assets/*` 的 Cache-Control（文件名含内容 hash，可 immutable 长缓存）。
pub const IMMUTABLE_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
/// 入口/无 hash 资源的 Cache-Control（每次回源校验，避免发旧页面）。
pub const NO_STORE_CACHE_CONTROL: &str = "no-store";

/// 一条内嵌静态资源。
#[derive(Debug, Clone, Copy)]
pub struct StaticAsset {
    /// URL 路径（`/index.html`、`/assets/index-XXXXXXXX.js`）。
    pub path: &'static str,
    /// 响应 Content-Type（编译期由扩展名定值）。
    pub content_type: &'static str,
    /// 文件字节（`include_bytes!` 编进二进制）。
    pub bytes: &'static [u8],
}

/// 内嵌静态资源全集（构建期生成；表非空由 `build.rs` 保证）。
pub fn table() -> &'static [(&'static str, &'static str, &'static [u8])] {
    WEBUI_DIST_FILES
}

/// 表长度（诊断/测试用）。
pub fn count() -> usize {
    WEBUI_DIST_FILES.len()
}

/// 精确路径查找（`/` 归一为 `/index.html`；未命中返回 `None`）。
pub fn lookup(path: &str) -> Option<StaticAsset> {
    let path = if path == "/" { SPA_INDEX_PATH } else { path };
    WEBUI_DIST_FILES
        .iter()
        .find(|(candidate, _, _)| *candidate == path)
        .map(|(path, content_type, bytes)| StaticAsset {
            path,
            content_type,
            bytes,
        })
}

/// SPA 入口（`build.rs` 已保证 dist 里有 index.html，这里的 `expect` 不会触发）。
pub fn index() -> StaticAsset {
    lookup(SPA_INDEX_PATH).expect("webui/dist/index.html 必须被 build.rs 内嵌")
}

/// 路径 → Cache-Control（`/assets/*` 长缓存，其余 no-store）。
pub fn cache_control(path: &str) -> &'static str {
    if path.starts_with(HASHED_ASSET_PREFIX) {
        IMMUTABLE_CACHE_CONTROL
    } else {
        NO_STORE_CACHE_CONTROL
    }
}
