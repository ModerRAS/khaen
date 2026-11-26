//! 构建脚本：把仓库根 `webui/dist`（Vue 控制台构建产物）编成 daemon 的内嵌静态资源表。
//!
//! - dist 位置相对 `CARGO_MANIFEST_DIR`（`crates/daemon`）：`../../webui/dist`，即仓库根
//!   `webui/dist`（Windows 上为 `C:\WorkSpace\Audio\khaen\webui\dist`）。
//! - `webui/dist` 是提交进仓库的构建产物（见 `webui/README.md`），本脚本只读、不生成。
//! - 生成 `$OUT_DIR/webui_assets_table.rs`：`(url_path, content_type, bytes)` 表，bytes 用
//!   `include_bytes!` 在编译期编进二进制；运行期不读磁盘、不需要 Node/反向代理。
//! - `rerun-if-changed` 指向 dist 目录与每个文件：前端重新构建后 `cargo build` 自动重编。
//! - dist 缺失 / 为空 / 缺 `index.html` 时**编译期显式失败**，而不是悄悄生成空表。
//!
//! 只用 std，不引入构建期依赖。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// dist 相对 crate 根的位置（`crates/daemon` → 仓库根 `webui/dist`）。
const DIST_RELATIVE: &str = "../../webui/dist";
/// SPA 入口文件名（必须存在；`/` 与 SPA 回退都用它）。
const SPA_INDEX_FILE: &str = "index.html";
/// 控制台版本的事实源（`webui/package.json` 的 `version`；读不到时回退 workspace 版本）。
const PACKAGE_JSON_RELATIVE: &str = "../../webui/package.json";

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let dist = manifest_dir.join(DIST_RELATIVE);
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", dist.display());

    if !dist.is_dir() {
        panic!(
            "webui/dist 不存在（{}）：控制台构建产物必须提交在仓库里（见 webui/README.md），daemon 需要把它内嵌进二进制",
            dist.display()
        );
    }

    let mut files = Vec::new();
    collect(&dist, &dist, &mut files);
    files.sort();
    if files.is_empty() {
        panic!(
            "webui/dist 为空（{}）：先构建控制台（见 webui/README.md）",
            dist.display()
        );
    }
    if !files.iter().any(|file| file == SPA_INDEX_FILE) {
        panic!(
            "webui/dist 缺少 {SPA_INDEX_FILE}（{}）：SPA 入口与回退都依赖它",
            dist.display()
        );
    }

    // 控制台版本：读 webui/package.json 的 version（真实值，不另外编一个数字）；
    // 读不到时回退 daemon 的 workspace 版本（`webui/` 与 daemon 同仓发布）。
    let package_json = manifest_dir.join(PACKAGE_JSON_RELATIVE);
    if package_json.is_file() {
        println!("cargo:rerun-if-changed={}", package_json.display());
    }
    let web_version = webui_package_version(&package_json)
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());

    let mut table =
        String::from("// 由 crates/daemon/build.rs 生成，源：仓库根 webui/dist（不要手改）。\n");
    table.push_str(
        "/// 控制台版本：构建期读 webui/package.json 的 version（读不到时回退 workspace 版本）。\n",
    );
    table.push_str(&format!(
        "pub const WEBUI_PACKAGE_VERSION: &str = {web_version:?};\n"
    ));
    table.push_str("pub const WEBUI_DIST_FILES: &[(&str, &str, &[u8])] = &[\n");
    for relative in &files {
        println!("cargo:rerun-if-changed={}", dist.join(relative).display());
        let url = format!("/{relative}");
        let include_path = format!("{DIST_RELATIVE}/{relative}");
        table.push_str(&format!(
            "    ({:?}, {:?}, include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/\", {:?}))),\n",
            url,
            content_type(&url),
            include_path,
        ));
    }
    table.push_str("];\n");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    fs::write(out_dir.join("webui_assets_table.rs"), table).expect("写 webui_assets_table.rs");

    // T46 构建信息：release 版本 / commit / rustc / profile / target。
    // 只读环境变量与 git；缺失时如实生成原因，不编造发布信息（由 src/system/build_info.rs 使用）。
    println!("cargo:rerun-if-env-changed=KHAEN_BUILD_VERSION");
    println!("cargo:rerun-if-env-changed=KHAEN_BUILD_COMMIT");
    write_build_info(&manifest_dir, &out_dir);
}

/// 生成 `$OUT_DIR/build_info.rs`：构建期版本事实（T46）。
///
/// - release 版本：`KHAEN_BUILD_VERSION`（release workflow 注入）；缺失时 `None` + 原因。
/// - commit：`KHAEN_BUILD_COMMIT` 优先，否则构建期 `git rev-parse HEAD`；都拿不到时 `None` + 原因。
///   commit 来源也一并生成，便于诊断区分“发布流水线注入”与“开发者本机 git”。
/// - rustc：构建期 `$RUSTC --version`（拿不到时 `None` + 原因）。
/// - profile / target：来自 cargo 的 `PROFILE` / `TARGET` 环境变量（恒有真实值）。
fn write_build_info(manifest_dir: &Path, out_dir: &Path) {
    let release = env_trimmed("KHAEN_BUILD_VERSION");
    let release_reason = match &release {
        Some(_) => None,
        None => Some(format!(
            "KHAEN_BUILD_VERSION is not set at build time: this is an unversioned build (crate version {})",
            env!("CARGO_PKG_VERSION")
        )),
    };

    let (commit, commit_source, commit_reason) = match env_trimmed("KHAEN_BUILD_COMMIT") {
        Some(value) => (
            Some(value),
            Some("KHAEN_BUILD_COMMIT (injected at build time)".to_string()),
            None,
        ),
        None => match git_commit(manifest_dir) {
            Some(sha) => (
                Some(sha),
                Some("git rev-parse HEAD at build time".to_string()),
                None,
            ),
            None => (
                None,
                None,
                Some(
                    "KHAEN_BUILD_COMMIT is not set and git metadata is not available in this build"
                        .to_string(),
                ),
            ),
        },
    };

    let (rustc, rustc_reason) = match rustc_version() {
        Some(version) => (Some(version), None),
        None => (
            None,
            Some("rustc --version failed at build time".to_string()),
        ),
    };

    let build_profile = env::var("PROFILE").unwrap_or_else(|_| "unknown".to_string());
    let target = env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());

    let mut generated = String::from(
        "// 由 crates/daemon/build.rs 生成（构建信息，T46；不要手改）。
",
    );
    generated.push_str(
        "/// 部署 release 版本（`KHAEN_BUILD_VERSION`，如 v0.1.2；未注入时为 None）。
",
    );
    generated.push_str(&format!(
        "pub const RELEASE_VERSION: Option<&str> = {};
",
        option_literal(release.as_deref())
    ));
    generated.push_str(
        "/// release 版本缺失的原因（仅有缺失时）。
",
    );
    generated.push_str(&format!(
        "pub const RELEASE_VERSION_REASON: Option<&str> = {};
",
        option_literal(release_reason.as_deref())
    ));
    generated.push_str(
        "/// 构建 commit（`KHAEN_BUILD_COMMIT` 或构建期 git HEAD；都没有时为 None）。
",
    );
    generated.push_str(&format!(
        "pub const COMMIT: Option<&str> = {};
",
        option_literal(commit.as_deref())
    ));
    generated.push_str(
        "/// commit 的来源（发布流水线注入 / 本机 git）。
",
    );
    generated.push_str(&format!(
        "pub const COMMIT_SOURCE: Option<&str> = {};
",
        option_literal(commit_source.as_deref())
    ));
    generated.push_str(
        "/// commit 缺失的原因（仅有缺失时）。
",
    );
    generated.push_str(&format!(
        "pub const COMMIT_REASON: Option<&str> = {};
",
        option_literal(commit_reason.as_deref())
    ));
    generated.push_str(
        "/// 构建用 rustc 版本（`rustc --version` 第一行）。
",
    );
    generated.push_str(&format!(
        "pub const RUSTC_VERSION: Option<&str> = {};
",
        option_literal(rustc.as_deref())
    ));
    generated.push_str(
        "/// rustc 版本缺失的原因（仅有缺失时）。
",
    );
    generated.push_str(&format!(
        "pub const RUSTC_REASON: Option<&str> = {};
",
        option_literal(rustc_reason.as_deref())
    ));
    generated.push_str(
        "/// cargo 构建 profile（debug/release）。
",
    );
    generated.push_str(&format!(
        "pub const BUILD_PROFILE: &str = {:?};
",
        build_profile
    ));
    generated.push_str(
        "/// 交叉编译目标三元组。
",
    );
    generated.push_str(&format!(
        "pub const TARGET: &str = {:?};
",
        target
    ));

    fs::write(out_dir.join("build_info.rs"), generated).expect("写 build_info.rs");
}

/// 去空白后的环境变量值；空串视为未设置（与 main 的其它配置解析一致）。
fn env_trimmed(key: &str) -> Option<String> {
    let value = env::var(key).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// `Option<&str>` → Rust 源码字面量（`None` / `Some("...")`）。
fn option_literal(value: Option<&str>) -> String {
    match value {
        Some(text) => format!("Some({text:?})"),
        None => "None".to_string(),
    }
}

/// 构建期 git HEAD（仓库在工作树内时有效）；拿不到返回 None（不报错）。
///
/// 同时登记 `.git/HEAD` / `refs` / `packed-refs` 为 rerun 依赖：切换 commit 后重新构建
/// 会重新采集，而不是一直用旧的 sha。
fn git_commit(manifest_dir: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(manifest_dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() || !sha.chars().all(|character| character.is_ascii_hexdigit()) {
        return None;
    }
    if let Some(git_dir) = git_dir(manifest_dir) {
        for path in [
            git_dir.join("HEAD"),
            git_dir.join("refs"),
            git_dir.join("packed-refs"),
        ] {
            if path.exists() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
    Some(sha)
}

/// `.git` 目录的绝对路径（`git rev-parse --git-dir`；worktree/子模块下也可能是相对路径）。
fn git_dir(manifest_dir: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(manifest_dir)
        .args(["rev-parse", "--git-dir"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return None;
    }
    let path = PathBuf::from(path);
    if path.is_absolute() {
        Some(path)
    } else {
        Some(manifest_dir.join(path))
    }
}

/// 构建期 rustc 版本（`$RUSTC --version` 的第一行）。
fn rustc_version() -> Option<String> {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let output = std::process::Command::new(rustc)
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()?
        .trim()
        .to_string();
    if line.is_empty() {
        None
    } else {
        Some(line)
    }
}

/// 递归收集 dist 下的普通文件（相对路径统一 `/` 分隔；排序由调用方做）。
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

/// 读 `webui/package.json` 的 `version`（无构建期 JSON 依赖：只提取顶层第一个 `"version": "…"`）。
///
/// 失败（文件不存在/没有 version/值不像版本号）时返回 `None`，由调用方回退 workspace 版本。
fn webui_package_version(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let key = "\"version\"";
    let mut search_from = 0;
    while let Some(index) = text.get(search_from..)?.find(key) {
        let after = search_from + index + key.len();
        let rest = text.get(after..)?;
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix(':') {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('"') {
                if let Some(end) = rest.find('"') {
                    let version = &rest[..end];
                    let plausible = !version.is_empty()
                        && version.chars().all(|c| {
                            c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_')
                        });
                    if plausible {
                        return Some(version.to_string());
                    }
                }
            }
        }
        search_from = after;
    }
    None
}

/// URL 路径 → Content-Type（编译期定值；未知扩展名按二进制流）。
fn content_type(path: &str) -> &'static str {
    let extension = match path.rsplit_once('.') {
        Some((_, extension)) => extension.to_ascii_lowercase(),
        None => return "application/octet-stream",
    };
    match extension.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "txt" | "md" => "text/plain; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "png" => "image/png",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}
