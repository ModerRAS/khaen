//! Vue 控制台静态资源表（仓库根 `webui/dist` 的编译期内嵌版本）。
//!
//! 表由 `crates/daemon/build.rs` 在编译期扫描仓库根 `webui/dist` 生成
//! （`include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../webui/dist/..."))`），
//! 因此 daemon 二进制自带控制台：运行期不读磁盘、不依赖 Node/Python/反向代理。
//!
//! - `table()`：`(url_path, content_type, bytes)` 全集（构建期生成；dist 缺失/为空/缺
//!   `index.html` 时 `build.rs` 直接让编译失败）。
//! - `lookup()`：精确 URL 命中（`/` 归一为 `/index.html`）。
//! - `index()`：SPA 入口（`/`、`/index.html` 与所有未命中的非 `/api` GET）。
//! - `cache_control()`：带内容 hash 的 `/assets/*` 长缓存，其余（入口/无 hash 资源）不缓存。
//! - `WEBUI_PACKAGE_VERSION`：构建期从 `webui/package.json` 的 `version` 提取的控制台版本
//!   （`/api/meta` 的 `web_version`；读不到时回退 workspace 版本）。
//!
//! 改前端源码后必须 `cd webui && pnpm build` 并把 `webui/dist` 一起提交（见 `webui/README.md`）；
//! 只改源码不重建时 daemon 发出去的仍是旧表（`rerun-if-changed` 会在 dist 变化后自动重编）。

include!(concat!(env!("OUT_DIR"), "/webui_assets_table.rs"));

/// SPA 入口路径（`webui/dist/index.html`；`/` 是它的别名）。
pub const SPA_INDEX_PATH: &str = "/index.html";

/// 带内容 hash 的静态资源目录（可长缓存；文件名变化即缓存失效）。
pub const HASHED_ASSET_PREFIX: &str = "/assets/";

/// `/assets/*` 的 Cache-Control（文件名含内容 hash，可 immutable 长缓存）。
pub const IMMUTABLE_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
/// 入口/无 hash 资源的 Cache-Control（每次回源，避免发旧页面）。
pub const NO_STORE_CACHE_CONTROL: &str = "no-store";

/// 一条内嵌静态资源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaticAsset {
    /// URL 路径（`/index.html`、`/assets/index-XXXXXXXX.js`）。
    pub path: &'static str,
    /// 响应 Content-Type（构建期由扩展名定值）。
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
    let normalized = if path == "/" { SPA_INDEX_PATH } else { path };
    lookup_in(WEBUI_DIST_FILES, normalized)
}

/// 在给定表里精确查找（`path` 必须已归一；测试用合成表复用同一逻辑）。
pub(crate) fn lookup_in(
    table: &'static [(&'static str, &'static str, &'static [u8])],
    path: &str,
) -> Option<StaticAsset> {
    table
        .iter()
        .find(|(candidate, _, _)| *candidate == path)
        .map(|&(path, content_type, bytes)| StaticAsset {
            path,
            content_type,
            bytes,
        })
}

/// SPA 入口（`build.rs` 已保证 dist 里有 index.html）。
pub fn index() -> StaticAsset {
    index_in(WEBUI_DIST_FILES)
}

/// 取给定表的 SPA 入口（`build.rs` 对真实表保证存在；合成表由调用方保证）。
pub(crate) fn index_in(
    table: &'static [(&'static str, &'static str, &'static [u8])],
) -> StaticAsset {
    lookup_in(table, SPA_INDEX_PATH).expect("webui/dist/index.html 必须被 build.rs 内嵌")
}

/// 路径 → Cache-Control（`/assets/*` 长缓存，其余 no-store）。
pub fn cache_control(path: &str) -> &'static str {
    if path.starts_with(HASHED_ASSET_PREFIX) {
        IMMUTABLE_CACHE_CONTROL
    } else {
        NO_STORE_CACHE_CONTROL
    }
}
