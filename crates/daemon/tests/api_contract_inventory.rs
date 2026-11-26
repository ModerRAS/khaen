//! API 合同盘点测试（docs/API.md 是前后端唯一约定，lead 维护）。
//!
//! 规则：文档里端点表（`| 方法 | 路径 | ... |`）出现的每个“路径 + 方法”组合，
//! daemon 必须真的挂载；反过来 daemon 挂载的每个路径/方法也必须在文档端点表里出现。
//! 这样文档与路由的漂移会在 `cargo test` 直接失败，而不是等前端联调才发现。
//!
//! 已知边界：路径占位符统一成 `{}`、忽略 query string；路径若只出现在文档正文
//! 而非端点表，会被视为“未文档化”，需要把它补成表格行。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

type Inventory = BTreeMap<String, BTreeSet<String>>;

const METHODS: [&str; 5] = ["GET", "POST", "PUT", "DELETE", "PATCH"];

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
    let mut any = false;
    for method in cell.split('/') {
        if !METHODS.contains(&method) {
            return false;
        }
        any = true;
    }
    any
}

/// 从 docs/API.md 的端点表格行提取 路径 -> 方法集合。
fn documented_endpoints(markdown: &str) -> Inventory {
    let mut inventory = Inventory::new();
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
        let methods = inventory.entry(normalize(path_cell)).or_default();
        for method in cells[1].split('/') {
            methods.insert(method.to_string());
        }
    }
    inventory
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

/// 从 axum 路由参数里提取方法名（`get(...)`/`post(...).put(...)` 等）。
fn method_tokens(args: &str) -> BTreeSet<String> {
    let mut methods = BTreeSet::new();
    for token in args.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_') {
        let upper = token.to_ascii_uppercase();
        if METHODS.contains(&upper.as_str()) {
            methods.insert(upper);
        }
    }
    methods
}

/// 从 daemon 源码里提取 `.route("<path>", <methods>)` 的路径与方法。
fn routed_endpoints(source_dir: &Path) -> Inventory {
    let mut files = Vec::new();
    collect_rs_files(source_dir, &mut files);
    let mut inventory = Inventory::new();
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
            let args_start = open + 1 + close + 1;
            let mut depth = 0usize;
            let mut args_end = after.len();
            for (index, ch) in after[args_start..].char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        if depth == 0 {
                            args_end = args_start + index;
                            break;
                        }
                        depth -= 1;
                    }
                    _ => {}
                }
            }
            if raw.starts_with('/') {
                inventory
                    .entry(normalize(raw))
                    .or_default()
                    .extend(method_tokens(&after[args_start..args_end]));
            }
            rest = if args_end < after.len() {
                &after[args_end + 1..]
            } else {
                ""
            };
        }
    }
    inventory
}

#[test]
fn documented_endpoints_match_daemon_routes() {
    let root = repo_root();
    let docs_path = root.join("docs").join("API.md");
    let markdown = fs::read_to_string(&docs_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", docs_path.display()));
    let documented = documented_endpoints(&markdown);
    let routed = routed_endpoints(&root.join("crates").join("daemon").join("src"));

    let mut problems = Vec::new();
    for (path, methods) in &documented {
        match routed.get(path) {
            None => problems.push(format!("{path}: documented but not routed")),
            Some(routed_methods) if routed_methods != methods => problems.push(format!(
                "{path}: documented methods {methods:?}, routed methods {routed_methods:?}"
            )),
            Some(_) => {}
        }
    }
    for path in routed.keys() {
        if !documented.contains_key(path) {
            problems.push(format!("{path}: routed but not documented"));
        }
    }
    assert!(
        problems.is_empty(),
        "docs/API.md and the daemon router disagree (add/fix the endpoint table row):\n{}",
        problems.join("\n")
    );
    assert!(
        documented.len() >= 40,
        "suspiciously few documented endpoints ({}): the docs parser may be broken",
        documented.len()
    );
}
