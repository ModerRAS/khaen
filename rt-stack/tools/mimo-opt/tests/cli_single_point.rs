//! 计划 4.3 单点目标（single-point）CLI 冒烟测试：
//! ① --objective single-point 可与 --authority/--safety 同开并跑通；
//! ② 报告出现 objective_outcome 块（目标指标 + 三态 + 残差）且目标名 = single_point；
//: ③ 产物 manifest 的 params_sha256 / result_sha256 正常，--verify 通过；
//! ④ 非零 --w-std 与 single-point 冲突（fail-closed，不静默忽略）。

mod common;

use std::path::Path;
use std::process::{Command, Output};

const SCENE: &str = "scene-cli-smoke";

fn run_cli(args: &[String]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mimo-opt"))
        .args(args)
        .output()
        .expect("spawn mimo-opt")
}

fn base_args(fixtures: &Path, out: &Path) -> Vec<String> {
    vec![
        "--fixtures".to_string(),
        fixtures.display().to_string(),
        "--out".to_string(),
        out.display().to_string(),
        "--scene".to_string(),
        SCENE.to_string(),
    ]
}

fn manifest_str(manifest: &str, key: &str) -> String {
    let needle = format!("\"{}\": \"", key);
    let start = manifest
        .find(&needle)
        .unwrap_or_else(|| panic!("manifest 缺少字段 {}", key))
        + needle.len();
    let rest = &manifest[start..];
    let end = rest.find('"').expect("字段值未闭合");
    rest[..end].to_string()
}

fn assert_sha256_hex(value: &str, label: &str) {
    assert_eq!(value.len(), 64, "{} 必须是 64 位十六进制: {}", label, value);
    assert!(
        value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{} 必须是十六进制: {}",
        label,
        value
    );
}

/// ①/②/③ 单点目标全链路冒烟：single-point + authority + safety 一起开。
#[test]
fn single_point_objective_smoke_emits_verifiable_artifact() {
    let scratch = common::scratch("single-point");
    let (fixtures, _) = common::build_fixture(&scratch.root);
    let out = scratch.root.join("out");
    let artifacts = scratch.root.join("artifacts").join("single-point-run");
    let mut args = base_args(&fixtures, &out);
    for item in [
        "--objective",
        "single-point",
        "--authority",
        "--safety",
        "--out-dir",
        artifacts.to_str().unwrap(),
    ] {
        args.push(item.to_string());
    }
    let output = run_cli(&args);
    assert!(
        output.status.success(),
        "single-point 冒烟失败: status={:?} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    // 报告：目标名 + 目标指标 + 三态 + 残差同块（不改变既有键语义，只新增块）
    let report_path = out.join(SCENE).join("report.json");
    let report = std::fs::read_to_string(&report_path).unwrap_or_else(|_| {
        panic!("缺少旧布局报告 {}", report_path.display());
    });
    assert!(
        report.contains("\"objective_outcome\""),
        "缺少 objective_outcome 块"
    );
    assert!(
        report.contains("\"objective\": \"single_point\""),
        "objective_outcome.objective 应为 single_point"
    );
    for key in [
        "\"target_metrics\"",
        "\"null_depth_mean_db\"",
        "\"e_modal_db\"",
        "\"outcome\"",
        "\"residuals\"",
        "\"algorithm_metrics\"",
    ] {
        assert!(report.contains(key), "objective_outcome 缺少 {}", key);
    }
    // 单点语义说明进了 notes
    assert!(
        report.contains("single-point objective"),
        "缺少 single_point 目标语义说明"
    );

    // params 里记录 objective = single-point（可复现快照）
    let params = std::fs::read_to_string(artifacts.join("params.json")).expect("params.json");
    assert!(params.contains("\"objective\": \"single-point\""));
    assert!(params.contains("\"w_std\""));

    // 产物 manifest：params_sha256 / result_sha256 正常且 --verify 通过
    let manifest = std::fs::read_to_string(artifacts.join("manifest.json")).expect("manifest.json");
    assert_sha256_hex(&manifest_str(&manifest, "params_sha256"), "params_sha256");
    assert_sha256_hex(&manifest_str(&manifest, "result_sha256"), "result_sha256");
    let verify = run_cli(&["--verify".to_string(), artifacts.display().to_string()]);
    assert!(
        verify.status.success(),
        "--verify 失败: stderr={}",
        String::from_utf8_lossy(&verify.stderr)
    );
    // 兼容旧布局：<out>/<scene>/report.json 仍然写出（single-point 不改变既有产物布局）
    assert!(out.join(SCENE).join("report.json").is_file());
}

/// ④ single-point 的座位离散项权重固定为 0：非零 --w-std 必须 fail-closed，不静默忽略。
#[test]
fn single_point_rejects_nonzero_seat_weight_and_accepts_zero() {
    let scratch = common::scratch("single-point-wstd");
    let (fixtures, _) = common::build_fixture(&scratch.root);

    let out_bad = scratch.root.join("out-bad");
    let mut args = base_args(&fixtures, &out_bad);
    for item in ["--objective", "single-point", "--w-std", "1"] {
        args.push(item.to_string());
    }
    let bad = run_cli(&args);
    assert!(
        !bad.status.success(),
        "single-point 不应接受非零 --w-std（应 fail-closed）"
    );
    let stderr = String::from_utf8_lossy(&bad.stderr);
    assert!(
        stderr.contains("single-point") && stderr.contains("--w-std"),
        "stderr 应说明冲突: {}",
        stderr
    );

    // 显式 0 允许（语义与 single-point 一致）
    let out_ok = scratch.root.join("out-ok");
    let mut args = base_args(&fixtures, &out_ok);
    for item in ["--objective", "single-point", "--w-std", "0"] {
        args.push(item.to_string());
    }
    let ok = run_cli(&args);
    assert!(
        ok.status.success(),
        "single-point --w-std 0 应跑通: stderr={}",
        String::from_utf8_lossy(&ok.stderr)
    );
}
