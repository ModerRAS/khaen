//! 构建脚本：把仓库根的 `webui/dist`（Vue 控制台构建产物）编成静态资源表。
//!
//! - 目标目录相对 `CARGO_MANIFEST_DIR`（`rt-stack/crates/control`）：
//!   `../../../webui/dist`，即仓库根 `webui/dist`。
//! - `webui/dist` 是**提交进仓库**的构建产物（见 `webui/README.md`），本脚本只读不生成。
//! - 生成 `$OUT_DIR/webui_assets_table.rs`：`(path, content_type, bytes)` 三元组表，
//!   bytes 用 `include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../webui/dist/..."))`
//!   编进 daemon 二进制，运行期不依赖任何磁盘文件。
//! - `rerun-if-changed` 指向 dist 目录：重新 `pnpm build` 后 `cargo build` 自动重编。
//! - dist 缺失/为空/缺 `index.html` 时**显式失败**（编译期报错，而不是悄悄出个空表）。
//!
//! 本脚本只用 std，不引入构建期依赖（control crate 的 E-1 口径：无第三方依赖）。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// dist 相对 crate 根的位置（`rt-stack/crates/control` → 仓库根 `webui/dist`）。
const DIST_RELATIVE: &str = "../../../webui/dist";
/// SPA 入口文件名（必须存在于 dist）。
const SPA_INDEX_FILE: &str = "index.html";

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let dist = manifest_dir.join(DIST_RELATIVE);
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", dist.display());

    if !dist.is_dir() {
        panic!(
            "webui/dist 不存在（{}）：Vue 控制台的构建产物必须提交在仓库里（cd webui && pnpm build），\
             见 webui/README.md；缺失时 daemon 无法内嵌控制台",
            dist.display()
        );
    }

    let mut files = Vec::new();
    collect(&dist, &dist, &mut files);
    files.sort();
    if files.is_empty() {
        panic!("webui/dist 为空（{}）：先构建控制台", dist.display());
    }
    if !files.iter().any(|file| file == SPA_INDEX_FILE) {
        panic!(
            "webui/dist 缺少 {SPA_INDEX_FILE}：SPA 入口与回退都依赖它（{}）",
            dist.display()
        );
    }

    let mut table = String::new();
    table.push_str("// 由 crates/control/build.rs 生成，源：webui/dist（不要手改）。\n");
    table.push_str("pub const WEBUI_DIST_FILES: &[(&str, &str, &[u8])] = &[\n");
    for relative in &files {
        let url = format!("/{relative}");
        table.push_str(&format!(
            "    ({:?}, {:?}, include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/{}/{}\"))),\n",
            url,
            content_type(&url),
            DIST_RELATIVE,
            relative,
        ));
    }
    table.push_str("];\n");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    fs::write(out_dir.join("webui_assets_table.rs"), table).expect("写 webui_assets_table.rs");
}

/// 递归收集 dist 下的文件（相对路径，统一 `/` 分隔；排序由调用方做）。
fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|error| panic!("读 {} 失败：{error}", dir.display()));
    for entry in entries {
        let entry = entry.expect("读 dist 目录项");
        let path = entry.path();
        let kind = entry.file_type().expect("读 dist 目录项类型");
        if kind.is_dir() {
            collect(root, &path, out);
            continue;
        }
        if !kind.is_file() {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .expect("dist 内的文件都在 dist 下")
            .to_string_lossy()
            .replace('\\', "/");
        out.push(relative);
    }
}

/// URL 路径 → Content-Type（静态表编译期定值；未知扩展名按二进制流）。
fn content_type(path: &str) -> &'static str {
    let extension = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match extension.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "png" => "image/png",
        "webp" => "image/webp",
        "jpg" | "jpeg" => "image/jpeg",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        _ => "application/octet-stream",
    }
}
