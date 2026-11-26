//! API 合同盘点测试（docs/API.md 是前后端唯一约定，lead 维护）。
//!
//! 规则：文档里端点表（`| 方法 | 路径 | ... |`）出现的每个路径，daemon 必须真的挂载；
//! 反过来 daemon 挂载的每个 `/api/*` 或 `/healthz` 路径也必须在文档端点表里出现。
//! 这样文档与路由的漂移会在 `cargo test` 直接失败，而不是等前端联调才发现。
//!
//! 已知边界：只比较路径集合（占位符统一成 `{}`），不比较 HTTP 方法组合；
//! 路径若出现在文档正文而非端点表，会被视为“未文档化”，需要把它补成表格行。

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/daemon has a parent")
        .parent()
        .expect("repository has a root")
        .to_path_buf()
}

/// 把 `/api/calibration/jobs/{job_id}` 与文档里的 `{id}` 统一成 `/api/calibration/jobs/{}`。
fn normalize(raw: &str) -> String {
    let path = raw.split('?').next().unwrap_or(raw).trim();
    let mut out = String::new();
    let mut in_placeholder = false;
    for ch in path.chars() {
        match ch {
            '{' => in_placeholder = true,
            '}' if in_placeholder => {
                in_placeholder = false;
                out.push_str("{}");
            }
            _ if in_placeholder => {}
            _ => out.push(ch),
        }
    }
    out
}

fn is_method_cell(cell: &str) -> bool {
    !cell.is_empty()
        && cell
            .split('/')
            .all(|method| matches!(method, "GET" | "POST" | "PUT" | "DELETE" | "PATCH"))
}

/// 从 docs/API.md 的端点表格行提取路径。
fn documented_endpoints(markdown: &str) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    for line in markdown.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = trimmed.split('|').map(str::trim).collect();
        if cells.len() < 4 || !is_method_cell(cells[1]) {
            continue;
        }
        let path_cell = cells[2].trim_matches('`');
        if !path_cell.starts_with('/') {
            continue;
        }
        paths.insert(normalize(path_cell));
    }
    paths
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));
    for entry in entries {
        let path = entry.expect("readable dir entry").path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

/// 从 daemon 源码里提取 `.route("<path>", ...)` 的路径。
fn routed_endpoints(source_dir: &Path) -> BTreeSet<String> {
    let mut files = Vec::new();
    collect_rs_files(source_dir, &mut files);
    let mut paths = BTreeSet::new();
    for file in files {
        let source = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
        let mut rest = source.as_str();
        while let Some(position) = rest.find(".route(") {
            let after = &rest[position + ".route(".len()..];
            let Some(open) = after.find('"') else {
                break;
            };
            let Some(close) = after[open + 1..].find('"') else {
                break;
            };
            let raw = &after[open + 1..open + 1 + close];
            if raw.starts_with('/') {
                paths.insert(normalize(raw));
            }
            rest = &after[open + 1 + close + 1..];
        }
    }
    paths
}

#[test]
fn documented_endpoints_match_daemon_routes() {
    let root = repo_root();
    let docs_path = root.join("docs").join("API.md");
    let markdown = fs::read_to_string(&docs_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", docs_path.display()));
    let documented = documented_endpoints(&markdown);
    let routed = routed_endpoints(&root.join("crates").join("daemon").join("src"));

    let missing_routes: Vec<&String> = documented.difference(&routed).collect();
    let undocumented: Vec<&String> = routed.difference(&documented).collect();
    assert!(
        missing_routes.is_empty(),
        "docs/API.md documents endpoints that the daemon does not route: {missing_routes:?}"
    );
    assert!(
        undocumented.is_empty(),
        "the daemon routes endpoints missing from docs/API.md: {undocumented:?}"
    );
    assert!(
        documented.len() >= 40,
        "suspiciously few documented endpoints ({}): the docs parser may be broken",
        documented.len()
    );
}
