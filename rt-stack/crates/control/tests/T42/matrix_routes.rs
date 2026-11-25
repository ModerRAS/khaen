// T42/F-21 私有测试：compat-matrix R01-R51 <-> RestService::routes() 一致性自动检查。
//
// 本文件由 crates/control/src/rest.rs 的 #[cfg(test)] 子模块通过 include! 编译执行
// （与 tests/T42/rest_contract.rs 同约定；不改 integrator 独占的 public_registration.rs）。
// 运行：cargo test -p khaen-control rest matrix_routes -- --nocapture
//
// 权威输入：
// - spec/compat-matrix.md §3.7「R 矩阵 <-> rest.rs 路由落地对照」（机器可读 7 列表）
// - spec/compat-matrix.md §3「REST 兼容矩阵」（R 行处置列）
// - crates/control/src/rest.rs RestService::routes()
//
// 检查口径（矩阵 §13.9）：
// 1) §3.7 必须覆盖 R01-R51 各一次；处置/落地状态/路由列格式合法；
// 2) §3 行处置必须与 §3.7 处置一致；*-DEFERRED <=> DEFERRED、RETIRE <=> RETIRED_ROUTE；
// 3) ROUTED/RETIRED_ROUTE 行列出的每条 (METHOD, path) 必须已在 routes() 注册；
// 4) DEFERRED/CROSS_CUTTING 行不得列出/注册路由（P1 不落地 = 不注册，X01 JSON 404）；
// 5) routes() 每条路由必须恰被一个 R 行声明（无孤儿、无重复声明）。
//
// 负向用例（6 条）用同一 checker 断言注入漂移后必失败，证明检查本身不是恒真。

// use super::* 由外层 #[cfg(test)] 模块提供（include! 共享作用域）。
// 注意：rest_contract.rs 已 use BTreeMap，本文件只用 BTreeSet，BTreeMap 全限定。

use std::collections::BTreeSet;

const MATRIX_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/compat-matrix.md");
const LANDING_HEADING: &str = "### 3.7";
const KNOWN_STATUS: [&str; 4] = ["ROUTED", "RETIRED_ROUTE", "CROSS_CUTTING", "DEFERRED"];
const KNOWN_METHODS: [&str; 4] = ["GET", "POST", "PUT", "DELETE"];
const EXPECTED_ROWS: usize = 51;
const EXPECTED_ROUTED: usize = 13;
const EXPECTED_RETIRED_ROUTE: usize = 1;
const EXPECTED_CROSS_CUTTING: usize = 2;
const EXPECTED_DEFERRED: usize = 35;

/// §3.7 的一行（ID/处置/落地状态/声明路由）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct LandingRow {
    id: String,
    disposition: String,
    status: String,
    routes: Vec<(String, String)>,
}

fn matrix_text() -> String {
    std::fs::read_to_string(MATRIX_PATH)
        .unwrap_or_else(|error| panic!("cannot read compat matrix {MATRIX_PATH}: {error}"))
}

fn is_r_id(text: &str) -> bool {
    text.len() == 3 && text.starts_with('R') && text[1..].bytes().all(|byte| byte.is_ascii_digit())
}

/// 处置列 token：去掉 markdown 粗体与括号注解（**ADAPT-DEFERRED** / KEEP（...） -> 码）。
fn disposition_token(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches('*').trim();
    trimmed
        .chars()
        .take_while(|ch| ch.is_ascii_uppercase() || *ch == '-')
        .collect()
}

fn cell<'a>(parts: &[&'a str], index: usize) -> &'a str {
    parts[index].trim()
}

/// 路由列表列：空用 "—"/"-"；多路由以 ";" 分隔，每项 "METHOD /path"。
fn parse_routes_cell(raw: &str) -> Result<Vec<(String, String)>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "—" || trimmed == "-" {
        return Ok(Vec::new());
    }
    let mut routes = Vec::new();
    for item in trimmed.split(';') {
        let item = item.trim();
        let mut words = item.split_whitespace();
        let method = words
            .next()
            .ok_or_else(|| format!("empty route item in '{trimmed}'"))?;
        let path = words
            .next()
            .ok_or_else(|| format!("route item missing path: '{item}'"))?;
        if words.next().is_some() {
            return Err(format!("route item has extra fields: '{item}'"));
        }
        if !KNOWN_METHODS.contains(&method) {
            return Err(format!("unknown HTTP method '{method}' in '{item}'"));
        }
        if !path.starts_with('/') {
            return Err(format!("route path must start with '/': '{item}'"));
        }
        routes.push((method.to_string(), path.to_string()));
    }
    Ok(routes)
}

/// 解析 §3.7 表（7 列；分隔符 '|' 使 split 后为 9 段）。
fn parse_landing_rows(text: &str) -> Result<Vec<LandingRow>, Vec<String>> {
    let mut errors = Vec::new();
    let mut rows = Vec::new();
    let mut in_section = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if !in_section {
            if trimmed.starts_with(LANDING_HEADING) {
                in_section = true;
            }
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        if !trimmed.starts_with('|') {
            // 表前可为说明段落；表开始后遇到非表格行即结束。
            if rows.is_empty() {
                continue;
            }
            break;
        }
        let parts: Vec<&str> = trimmed.split('|').collect();
        if parts.len() != 9 {
            errors.push(format!("§3.7 行列数不是 7: {trimmed}"));
            continue;
        }
        let id = cell(&parts, 1);
        if id == "ID" || id.starts_with("---") {
            continue;
        }
        if !is_r_id(id) {
            errors.push(format!("§3.7 非法 R 号: '{id}'"));
            continue;
        }
        let disposition = disposition_token(cell(&parts, 2));
        let status = cell(&parts, 3).to_string();
        let routes = match parse_routes_cell(cell(&parts, 4)) {
            Ok(routes) => routes,
            Err(error) => {
                errors.push(format!("{id} 路由列非法: {error}"));
                Vec::new()
            }
        };
        rows.push(LandingRow {
            id: id.to_string(),
            disposition,
            status,
            routes,
        });
    }
    if !in_section {
        errors.push(format!("compat matrix 缺少 {LANDING_HEADING} 落地对照表"));
    }
    if rows.is_empty() && errors.is_empty() {
        errors.push("§3.7 落地对照表为空".to_string());
    }
    if errors.is_empty() {
        Ok(rows)
    } else {
        Err(errors)
    }
}

/// 解析 §3 人类可读 R 表（6 列；处置在第 4 数据列）。
fn parse_spec_rows(text: &str) -> Result<std::collections::BTreeMap<String, String>, Vec<String>> {
    let mut errors = Vec::new();
    let mut map = std::collections::BTreeMap::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("| R") {
            continue;
        }
        let parts: Vec<&str> = trimmed.split('|').collect();
        // §3 行 6 列 -> 8 段；§3.7 行 7 列 -> 9 段（跳过）。
        if parts.len() != 8 {
            continue;
        }
        let id = cell(&parts, 1);
        if !is_r_id(id) {
            continue;
        }
        let disposition = disposition_token(cell(&parts, 4));
        if disposition.is_empty() {
            errors.push(format!("§3 行 {id} 处置列不可解析"));
            continue;
        }
        if map.insert(id.to_string(), disposition).is_some() {
            errors.push(format!("§3 重复 R 行: {id}"));
        }
    }
    if map.len() != EXPECTED_ROWS {
        errors.push(format!("§3 R 行数 {} != {EXPECTED_ROWS}", map.len()));
    }
    if errors.is_empty() {
        Ok(map)
    } else {
        Err(errors)
    }
}

/// 核验 matrix（compat-matrix 全文）与 routes（RestService::routes()）一致。
/// Ok(声明路由数)；Err(逐项错误，供测试与负向用例断言)。
fn check_matrix_routes(matrix: &str, routes: &[(String, String)]) -> Result<usize, Vec<String>> {
    let mut errors = Vec::new();
    let landing = match parse_landing_rows(matrix) {
        Ok(rows) => rows,
        Err(mut errs) => {
            errors.append(&mut errs);
            Vec::new()
        }
    };
    let spec = match parse_spec_rows(matrix) {
        Ok(map) => map,
        Err(mut errs) => {
            errors.append(&mut errs);
            std::collections::BTreeMap::new()
        }
    };

    let expected: Vec<String> = (1..=EXPECTED_ROWS).map(|n| format!("R{n:02}")).collect();
    let mut seen = BTreeSet::new();
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for row in &landing {
        if !seen.insert(row.id.clone()) {
            errors.push(format!("§3.7 重复 R 行: {}", row.id));
        }
        if !KNOWN_STATUS.contains(&row.status.as_str()) {
            errors.push(format!("{} 未知落地状态 '{}'", row.id, row.status));
            continue;
        }
        *counts.entry(row.status.as_str()).or_default() += 1;
        let deferred = row.disposition.ends_with("-DEFERRED");
        match row.status.as_str() {
            "DEFERRED" => {
                if !deferred {
                    errors.push(format!(
                        "{} 落地状态 DEFERRED 但处置为 '{}'（应为 *-DEFERRED）",
                        row.id, row.disposition
                    ));
                }
                if !row.routes.is_empty() {
                    errors.push(format!(
                        "{} 已声明 DEFERRED 却列路由 {:?}（P1 不得注册）",
                        row.id, row.routes
                    ));
                }
            }
            "ROUTED" => {
                if deferred {
                    errors.push(format!(
                        "{} 处置为 '{}' 但落地状态是 ROUTED",
                        row.id, row.disposition
                    ));
                }
                if !matches!(row.disposition.as_str(), "KEEP" | "ADAPT" | "ADD") {
                    errors.push(format!(
                        "{} ROUTED 行的处置 '{}' 非法（应为 KEEP/ADAPT/ADD）",
                        row.id, row.disposition
                    ));
                }
                if row.routes.is_empty() {
                    errors.push(format!("{} ROUTED 行未声明路由", row.id));
                }
            }
            "RETIRED_ROUTE" => {
                if deferred {
                    errors.push(format!(
                        "{} 处置为 '{}' 但落地状态是 RETIRED_ROUTE",
                        row.id, row.disposition
                    ));
                }
                if row.disposition != "RETIRE" {
                    errors.push(format!(
                        "{} RETIRED_ROUTE 行的处置必须是 RETIRE（实际 '{}'）",
                        row.id, row.disposition
                    ));
                }
                if row.routes.is_empty() {
                    errors.push(format!("{} RETIRED_ROUTE 行未声明路由", row.id));
                }
            }
            "CROSS_CUTTING" => {
                if deferred {
                    errors.push(format!(
                        "{} 处置为 '{}' 但落地状态是 CROSS_CUTTING",
                        row.id, row.disposition
                    ));
                }
                if !matches!(
                    row.disposition.as_str(),
                    "KEEP" | "ADAPT" | "ADD" | "RETIRE"
                ) {
                    errors.push(format!(
                        "{} CROSS_CUTTING 行的处置 '{}' 非法",
                        row.id, row.disposition
                    ));
                }
                if !row.routes.is_empty() {
                    errors.push(format!(
                        "{} CROSS_CUTTING 行不应对应单一路径，却列路由 {:?}",
                        row.id, row.routes
                    ));
                }
            }
            other => errors.push(format!("{} 未知落地状态 '{other}'", row.id)),
        }
        match spec.get(&row.id) {
            Some(disposition) if disposition == &row.disposition => {}
            Some(disposition) => errors.push(format!(
                "{} §3 处置 '{}' != §3.7 处置 '{}'",
                row.id, disposition, row.disposition
            )),
            None => errors.push(format!("{} 缺 §3 R 行", row.id)),
        }
    }
    for id in &expected {
        if !seen.contains(id) {
            errors.push(format!("§3.7 缺 R 行: {id}"));
        }
    }
    if landing.len() != EXPECTED_ROWS {
        errors.push(format!("§3.7 行数 {} != {EXPECTED_ROWS}", landing.len()));
    }
    let expected_counts = [
        ("ROUTED", EXPECTED_ROUTED),
        ("RETIRED_ROUTE", EXPECTED_RETIRED_ROUTE),
        ("CROSS_CUTTING", EXPECTED_CROSS_CUTTING),
        ("DEFERRED", EXPECTED_DEFERRED),
    ];
    for (status, expected_count) in expected_counts {
        let actual = counts.get(status).copied().unwrap_or(0);
        if actual != expected_count {
            errors.push(format!(
                "落地状态 {status} 行数 {actual} != {expected_count}（矩阵汇总需同步更新）"
            ));
        }
    }

    let registered: BTreeSet<(String, String)> = routes.iter().cloned().collect();
    if registered.len() != routes.len() {
        errors.push("RestService::routes() 含重复 (method,path)".to_string());
    }
    let mut claimed: std::collections::BTreeMap<(String, String), Vec<String>> =
        std::collections::BTreeMap::new();
    for row in &landing {
        if !matches!(row.status.as_str(), "ROUTED" | "RETIRED_ROUTE") {
            continue;
        }
        for route in &row.routes {
            if !registered.contains(route) {
                errors.push(format!(
                    "{} 声明路由未在 routes() 注册: {} {}",
                    row.id, route.0, route.1
                ));
            }
            claimed
                .entry(route.clone())
                .or_default()
                .push(row.id.clone());
        }
    }
    for (route, owners) in &claimed {
        if owners.len() != 1 {
            errors.push(format!(
                "路由被多行声明: {} {} <- {:?}",
                route.0, route.1, owners
            ));
        }
    }
    for route in &registered {
        if !claimed.contains_key(route) {
            errors.push(format!(
                "孤儿路由未在任何 R 行声明: {} {}",
                route.0, route.1
            ));
        }
    }

    if errors.is_empty() {
        Ok(claimed.len())
    } else {
        Err(errors)
    }
}

fn routes_list() -> Vec<(String, String)> {
    RestService::routes()
        .into_iter()
        .map(|(method, path)| (method.to_string(), path.to_string()))
        .collect()
}

/// 在 §3.7 内把 id 行的第 column 个数据列（1=ID/2=处置/3=状态/4=路由）做一次替换。
fn replace_landing_cell(matrix: &str, id: &str, column: usize, from: &str, to: &str) -> String {
    let mut out = String::new();
    let mut in_section = false;
    let mut replaced = false;
    for line in matrix.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(LANDING_HEADING) {
            in_section = true;
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if in_section && !replaced && trimmed.starts_with('|') {
            let parts: Vec<&str> = trimmed.split('|').collect();
            if parts.len() == 9 && cell(&parts, 1) == id {
                let mut new_parts: Vec<String> =
                    parts.iter().map(|part| (*part).to_string()).collect();
                let before = new_parts[column].clone();
                new_parts[column] = before.replacen(from, to, 1);
                assert_ne!(
                    before, new_parts[column],
                    "§3.7 替换未生效: {id} 列{column} '{from}'"
                );
                out.push_str(&new_parts.join("|"));
                out.push('\n');
                replaced = true;
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    assert!(replaced, "§3.7 未找到 {id} 行");
    out
}

/// 在 §3 人类可读表内替换 id 行的处置列。
fn replace_spec_disposition(matrix: &str, id: &str, from: &str, to: &str) -> String {
    let mut out = String::new();
    let mut replaced = false;
    for line in matrix.lines() {
        let trimmed = line.trim();
        if !replaced && trimmed.starts_with("| R") {
            let parts: Vec<&str> = trimmed.split('|').collect();
            if parts.len() == 8 && cell(&parts, 1) == id {
                let mut new_parts: Vec<String> =
                    parts.iter().map(|part| (*part).to_string()).collect();
                let before = new_parts[4].clone();
                new_parts[4] = before.replacen(from, to, 1);
                assert_ne!(before, new_parts[4], "§3 替换未生效: {id} '{from}'");
                out.push_str(&new_parts.join("|"));
                out.push('\n');
                replaced = true;
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    assert!(replaced, "§3 未找到 {id} 行");
    out
}

fn assert_checker_error(errors: &[String], needles: &[&str]) {
    for needle in needles {
        assert!(
            errors.iter().any(|error| error.contains(needle)),
            "缺少错误 '{needle}': {errors:#?}"
        );
    }
}

#[test]
fn t42_rest_matrix_routes_consistency() {
    let matrix = matrix_text();
    let routes = routes_list();
    match check_matrix_routes(&matrix, &routes) {
        Ok(claimed) => {
            assert_eq!(claimed, routes.len(), "声明路由数必须等于 routes() 条数");
            assert!(claimed > 0, "routes() 不能为空");
            println!(
                "F21-MATRIX-ROUTES PASS rows={EXPECTED_ROWS} claimed_routes={claimed} routed={EXPECTED_ROUTED} retired_route={EXPECTED_RETIRED_ROUTE} cross={EXPECTED_CROSS_CUTTING} deferred={EXPECTED_DEFERRED}"
            );
            let landing = parse_landing_rows(&matrix).expect("§3.7 must parse");
            for row in &landing {
                let routes_text = if row.routes.is_empty() {
                    "—".to_string()
                } else {
                    row.routes
                        .iter()
                        .map(|(method, path)| format!("{method} {path}"))
                        .collect::<Vec<_>>()
                        .join("; ")
                };
                println!(
                    "F21-MATRIX-ROW {} {} {} {}",
                    row.id, row.disposition, row.status, routes_text
                );
            }
        }
        Err(errors) => panic!("matrix<->routes 不一致:\n{}", errors.join("\n")),
    }
}

#[test]
fn t42_rest_matrix_routes_rejects_missing_route() {
    let matrix = matrix_text();
    let broken = replace_landing_cell(&matrix, "R01", 4, "GET /healthz", "GET /healthz-missing");
    let errors = check_matrix_routes(&broken, &routes_list()).expect_err("缺路由必须失败");
    assert_checker_error(&errors, &["R01", "healthz-missing"]);
}

#[test]
fn t42_rest_matrix_routes_rejects_orphan_route() {
    let matrix = matrix_text();
    let broken = replace_landing_cell(&matrix, "R02", 4, "GET /api/auth/status", "—");
    let errors = check_matrix_routes(&broken, &routes_list()).expect_err("孤儿路由必须失败");
    assert_checker_error(&errors, &["/api/auth/status"]);
}

#[test]
fn t42_rest_matrix_routes_rejects_deferred_with_route() {
    let matrix = matrix_text();
    let broken = replace_landing_cell(&matrix, "R07", 4, "—", "GET /api/overview");
    let errors = check_matrix_routes(&broken, &routes_list()).expect_err("DEFERRED 列路由必须失败");
    assert_checker_error(&errors, &["R07"]);
    assert!(
        errors.iter().any(|error| error.contains("DEFERRED")),
        "{errors:#?}"
    );
}

#[test]
fn t42_rest_matrix_routes_rejects_disposition_status_mismatch() {
    let matrix = matrix_text();
    let broken = replace_landing_cell(&matrix, "R01", 2, "KEEP", "ADAPT-DEFERRED");
    let errors = check_matrix_routes(&broken, &routes_list()).expect_err("处置/状态不一致必须失败");
    assert_checker_error(&errors, &["R01"]);
}

#[test]
fn t42_rest_matrix_routes_rejects_duplicate_id() {
    let matrix = matrix_text();
    let broken = replace_landing_cell(&matrix, "R02", 1, "R02", "R01");
    let errors = check_matrix_routes(&broken, &routes_list()).expect_err("重复 R 号必须失败");
    assert!(
        errors.iter().any(|error| error.contains("重复")),
        "{errors:#?}"
    );
}

#[test]
fn t42_rest_matrix_routes_rejects_spec_table_mismatch() {
    let matrix = matrix_text();
    let broken = replace_spec_disposition(&matrix, "R01", "KEEP", "ADAPT");
    let errors =
        check_matrix_routes(&broken, &routes_list()).expect_err("§3 与 §3.7 不一致必须失败");
    assert_checker_error(&errors, &["R01"]);
}
