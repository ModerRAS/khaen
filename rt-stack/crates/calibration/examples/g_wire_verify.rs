// g_wire_verify —— 单点 G 实验 plan 数字域一致性校验（EXPERIMENT / NON_ACCEPTABLE / SINGLE_POINT_NOT_SPATIAL）。
//
// 用途：把「声明注入的求解器 G」与「实际 compile-report 里的 plan 路径」逐 cell 对拍，用与
// 编译侧相同的 f32 canonical 口径（GPathInput::coeffs_f32_sha256）比较，避免任何口径漂移。
//
// 注意（plan 语义）：compile-report 的对角路径系数 = G_diag **+ delta_D baseline**
// （core-v1 §T03 §2.4；GPathInput.coeffs 不含 baseline），本工具按同一语义重算期望值。
//
// 用法：
//   g_wire_verify --g-paths FILE --compile-report FILE [--plan FILE] [--out FILE]
// 退出码：0 = 全部一致；1 = 有不一致；2 = 用法/解析错误。
//
// 纪律：只读文件、不打开设备、不写板端；输出永远是 EXPERIMENT/NON_ACCEPTABLE，不写 PASS/READY。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use tspi_calibration::compile_apply::GPathInput;
use tspi_calibration::job::sha256_hex;
use tspi_calibration::measured_design::load_g_paths_file;
use tspi_dsp_compat::ws::{json_encode, json_object, json_parse, JsonValue};

fn text(value: impl Into<String>) -> JsonValue {
    JsonValue::String(value.into())
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    match run(&raw) {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            eprintln!("g_wire_verify: {message}");
            ExitCode::from(2)
        }
    }
}

fn value(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|argument| argument == key)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn run(args: &[String]) -> Result<u8, String> {
    let g_paths_path = value(args, "--g-paths")
        .ok_or_else(|| "--g-paths FILE is required".to_string())?;
    let report_path = value(args, "--compile-report")
        .ok_or_else(|| "--compile-report FILE is required".to_string())?;
    let plan_path = value(args, "--plan").map(PathBuf::from);
    let out_path = value(args, "--out").map(PathBuf::from);

    let injected = load_g_paths_file(Path::new(&g_paths_path))?;
    let report_text = std::fs::read_to_string(&report_path)
        .map_err(|error| format!("read compile-report {report_path:?}: {error}"))?;
    let report = json_parse(&report_text)
        .map_err(|error| format!("parse compile-report (offset {}): {error}", error.offset))?;

    let participants = &injected.participants;
    let d_samples = injected.low_band_d as usize;
    let l_g = injected.l_g as usize;

    // 声明 G：dense (out,in) -> 系数。
    let mut declared = Vec::new();
    for cell in 0..participants.len() * participants.len() {
        let out_dense = cell / participants.len();
        let in_dense = cell % participants.len();
        let path = injected
            .paths
            .iter()
            .find(|path| path.out_dense as usize == out_dense && path.in_dense as usize == in_dense)
            .ok_or_else(|| format!("g-paths 缺 cell {out_dense}/{in_dense}"))?;
        let nonzero = path.coeffs.iter().any(|value| *value != 0.0);
        declared.push((out_dense, in_dense, path, nonzero));
    }
    let declared_nonzero = declared.iter().filter(|(_, _, _, nz)| *nz).count();

    // plan 路径：compile-report.paths[]。
    let paths = report
        .get("paths")
        .and_then(|value| value.as_array())
        .ok_or_else(|| "compile-report 缺 paths 数组".to_string())?;

    let mut failures: Vec<String> = Vec::new();
    let mut checked = 0usize;
    let mut plan_keys: Vec<(String, String)> = Vec::new();
    for path in paths {
        let out_role = path
            .get("out_role")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "paths[].out_role 缺失".to_string())?;
        let in_role = path
            .get("in_role")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "paths[].in_role 缺失".to_string())?;
        plan_keys.push((out_role.to_string(), in_role.to_string()));
        let taps = path.get("taps").and_then(|value| value.as_u64()).unwrap_or(0) as usize;
        let actual_sha = path
            .get("coeff_sha256")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let diag_baseline = path
            .get("diag_baseline")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let out_dense = participants
            .iter()
            .position(|role| role == out_role)
            .ok_or_else(|| format!("plan 输出角色 {out_role:?} 不在注入 participants"))?;
        let in_dense = participants
            .iter()
            .position(|role| role == in_role)
            .ok_or_else(|| format!("plan 输入角色 {in_role:?} 不在注入 participants"))?;
        let (_, _, declared_path, nonzero) = declared
            .iter()
            .find(|(out, in_, _, _)| *out == out_dense && *in_ == in_dense)
            .ok_or_else(|| format!("plan path {out_role}/{in_role} 在注入 G 中不存在"))?;
        if !nonzero {
            failures.push(format!("plan 出现注入 G 里全零的 cell {out_role}/{in_role}"));
            continue;
        }
        if taps != l_g {
            failures.push(format!(
                "path {out_role}/{in_role} taps={taps} != 声明 L_G={l_g}"
            ));
            continue;
        }
        if diag_baseline != (out_dense == in_dense) {
            failures.push(format!(
                "path {out_role}/{in_role} diag_baseline={diag_baseline} 与 dense 对角语义不符"
            ));
        }
        let mut expected_coeffs: Vec<f64> = declared_path.coeffs.clone();
        if diag_baseline {
            expected_coeffs[d_samples] += 1.0;
        }
        let expected = GPathInput::new(
            out_dense as u16,
            in_dense as u16,
            expected_coeffs,
            diag_baseline,
            false,
            sha256_hex(b"g-wire-verify"),
        )
        .map_err(|error| format!("expected GPathInput: {error}"))?
        .coeffs_f32_sha256()
        .map_err(|error| format!("expected f32 sha: {error}"))?;
        checked += 1;
        if expected != actual_sha {
            failures.push(format!(
                "path {out_role}/{in_role} coeff_sha256 不一致（expected {expected}, actual {actual_sha}）"
            ));
        }
    }

    // 反向：注入 G 的每个非零 cell 都必须出现在 plan 里。
    for (out_dense, in_dense, _, nonzero) in &declared {
        if !*nonzero {
            continue;
        }
        let out_role = &participants[*out_dense];
        let in_role = &participants[*in_dense];
        if !plan_keys
            .iter()
            .any(|(out, in_)| out == out_role && in_ == in_role)
        {
            failures.push(format!("注入 G 的非零 cell {out_role}/{in_role} 未进 plan"));
        }
    }
    if checked != declared_nonzero || paths.len() != declared_nonzero {
        failures.push(format!(
            "路径计数不一致：plan paths={} checked={checked} 注入非零 cell={declared_nonzero}",
            paths.len()
        ));
    }

    // plan 字节 sha 与报告声明一致（可选）。
    let mut plan_sha = JsonValue::Null;
    if let Some(plan_path) = plan_path.as_ref() {
        let bytes = std::fs::read(plan_path)
            .map_err(|error| format!("read plan {plan_path:?}: {error}"))?;
        let actual = sha256_hex(&bytes);
        let declared_sha = report
            .get("plan")
            .and_then(|value| value.get("plan_sha256"))
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if actual != declared_sha {
            failures.push(format!(
                "plan 字节 sha256 {actual} != compile-report 声明 {declared_sha}"
            ));
        }
        plan_sha = text(actual);
    }

    // 报告统计。
    let report_l_g = report
        .get("plan")
        .and_then(|value| value.get("l_g"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    if report_l_g != injected.l_g as u64 {
        failures.push(format!(
            "compile-report l_g={report_l_g} != 注入声明的 L_G={}",
            injected.l_g
        ));
    }

    let document = json_object(vec![
        ("schema", text("tspi-cal-g-wire-verify/v1")),
        ("verdict", text(if failures.is_empty() { "EXPERIMENT" } else { "MISMATCH" })),
        ("acceptability", text("NON_ACCEPTABLE")),
        ("scope", text("SINGLE_POINT_NOT_SPATIAL")),
        ("applied", JsonValue::Bool(false)),
        (
            "g_paths_source",
            text(g_paths_path.clone()),
        ),
        (
            "g_paths_sha256",
            text(sha256_hex(
                std::fs::read(&g_paths_path)
                    .map_err(|error| format!("read g-paths: {error}"))?
                    .as_slice(),
            )),
        ),
        ("declared_participants", JsonValue::UInt(participants.len() as u64)),
        ("declared_l_g", JsonValue::UInt(injected.l_g as u64)),
        ("declared_nonzero_cells", JsonValue::UInt(declared_nonzero as u64)),
        ("plan_paths", JsonValue::UInt(paths.len() as u64)),
        ("checked_paths", JsonValue::UInt(checked as u64)),
        ("report_l_g", JsonValue::UInt(report_l_g)),
        ("plan_sha256", plan_sha),
        (
            "failures",
            JsonValue::Array(failures.iter().map(|item| text(item.clone())).collect()),
        ),
        (
            "note",
            text(
                "数字域一致性：plan 路径的 taps/对角 baseline/每 cell f32 系数 sha256 与声明注入的求解器 G 逐条一致；本产物为 EXPERIMENT/NON_ACCEPTABLE，不是 READY/交付"
                    .to_string(),
            ),
        ),
    ]);
    let encoded = json_encode(&document).map_err(|error| format!("encode verdict: {error}"))?;
    if let Some(out_path) = out_path.as_ref() {
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("create out dir {}: {error}", parent.display()))?;
        }
        std::fs::write(out_path, encoded.as_bytes())
            .map_err(|error| format!("write {}: {error}", out_path.display()))?;
    }
    println!("{encoded}");
    if failures.is_empty() {
        Ok(0)
    } else {
        Ok(1)
    }
}
