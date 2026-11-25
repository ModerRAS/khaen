//! khaen-scene-plan CLI：按报告选定变体取 FIR -> 方案；变体覆盖、错误码、确定性。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_khaen-scene-plan");
const PLAN_LEVELS_BIN: &str = env!("CARGO_BIN_EXE_khaen-plan-levels");

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "khaen-scene-plan-{}-{}-{}",
        std::process::id(),
        tag,
        unique
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_fir(path: &Path, taps: &[f64]) {
    let mut text = String::from("# fir taps\n");
    for tap in taps {
        text.push_str(&format!("{tap}\n"));
    }
    std::fs::write(path, text).expect("write fir csv");
}

fn constant_gain(gain: f64) -> Vec<f64> {
    let mut values = vec![0.0; 4];
    values[0] = gain;
    values
}

fn write_report(root: &Path, outcome_variant: Option<&str>) -> PathBuf {
    let fir_dir = root.join("fir");
    std::fs::create_dir_all(&fir_dir).expect("fir dir");
    write_fir(&fir_dir.join("SUB1.csv"), &constant_gain(2.0));
    write_fir(&fir_dir.join("SUB2.csv"), &constant_gain(0.5));
    write_fir(&fir_dir.join("B2_SUB1.csv"), &constant_gain(4.0));
    write_fir(&fir_dir.join("B2_SUB2.csv"), &constant_gain(1.0));
    let mut report = serde_json::json!({
        "format": "khaen-mimo-sim-report/v1",
        "tool": "khaen-mimo-opt",
        "variants": {
            "B2": { "fir_files": [
                { "source": "SUB1", "path": "fir/B2_SUB1.csv", "taps": 4, "delay_samples": 2 },
                { "source": "SUB2", "path": "fir/B2_SUB2.csv", "taps": 4, "delay_samples": 2 }
            ]},
            "B3": { "fir_files": [
                { "source": "SUB1", "path": "fir/SUB1.csv", "taps": 4, "delay_samples": 2 },
                { "source": "SUB2", "path": "fir/SUB2.csv", "taps": 4, "delay_samples": 2 }
            ]}
        }
    });
    if let Some(variant) = outcome_variant {
        report["outcome_variant"] = serde_json::Value::String(variant.to_string());
    }
    let path = root.join("scene-report.json");
    std::fs::write(&path, serde_json::to_vec(&report).expect("report json")).expect("report");
    path
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(bin: &str, args: &[&str]) -> Run {
    let output = Command::new(bin).args(args).output().expect("spawn child");
    Run {
        code: output.status.code().expect("exit code"),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn predicted_levels(plan: &Path, out: &Path) -> serde_json::Value {
    let result = run(
        PLAN_LEVELS_BIN,
        &[
            "--plan",
            plan.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--centers",
            "100,1000",
        ],
    );
    assert_eq!(result.code, 0, "{}", result.stderr);
    serde_json::from_slice(&std::fs::read(out).expect("levels")).expect("levels json")
}

#[test]
fn picks_outcome_variant_and_maps_sources_to_lanes() {
    let root = temp_dir("default");
    let report = write_report(&root, Some("B3"));
    let plan = root.join("plan.bin");
    let result = run(
        BIN,
        &[
            "--report",
            report.to_str().unwrap(),
            "--fir-dir",
            root.join("fir").to_str().unwrap(),
            "--out",
            plan.to_str().unwrap(),
            "--sample-rate",
            "48000",
        ],
    );
    assert_eq!(result.code, 0, "{}", result.stderr);
    assert!(result.stdout.contains("\"variant\":\"B3\""));
    assert!(result.stdout.contains("\"lanes\":2"));
    assert!(result.stdout.contains("\"sources\":[\"SUB1\",\"SUB2\"]"));
    let bytes = std::fs::read(&plan).expect("plan");
    assert!(bytes.starts_with(b"KHAENPLN"));

    let levels = predicted_levels(&plan, &root.join("levels.json"));
    let points = levels["points"].as_array().expect("points");
    assert_eq!(points.len(), 2);
    let level = |index: usize, band: usize| -> f64 {
        points[index]["bands"][band]["level_db"]
            .as_f64()
            .expect("level")
    };
    assert!((level(0, 0) - 6.0206).abs() <= 1e-4, "{}", level(0, 0));
    assert!((level(1, 0) + 6.0206).abs() <= 1e-4, "{}", level(1, 0));

    // 同输入再跑一次：方案逐字节相同。
    let plan2 = root.join("plan2.bin");
    let again = run(
        BIN,
        &[
            "--report",
            report.to_str().unwrap(),
            "--fir-dir",
            root.join("fir").to_str().unwrap(),
            "--out",
            plan2.to_str().unwrap(),
            "--sample-rate",
            "48000",
        ],
    );
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert_eq!(bytes, std::fs::read(&plan2).expect("plan2"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn variant_override_selects_other_fir_set() {
    let root = temp_dir("override");
    let report = write_report(&root, Some("B3"));
    let plan = root.join("plan.bin");
    let result = run(
        BIN,
        &[
            "--report",
            report.to_str().unwrap(),
            "--fir-dir",
            root.join("fir").to_str().unwrap(),
            "--out",
            plan.to_str().unwrap(),
            "--sample-rate",
            "48000",
            "--variant",
            "B2",
        ],
    );
    assert_eq!(result.code, 0, "{}", result.stderr);
    assert!(result.stdout.contains("\"variant\":\"B2\""));
    let levels = predicted_levels(&plan, &root.join("levels.json"));
    let points = levels["points"].as_array().expect("points");
    let first = points[0]["bands"][0]["level_db"].as_f64().expect("level");
    assert!(
        (first - 12.0412).abs() <= 1e-3,
        "B2 lane 0 = +12.04 dB, got {first}"
    );
    let second = points[1]["bands"][0]["level_db"].as_f64().expect("level");
    assert!(second.abs() <= 1e-6, "B2 lane 1 = 0 dB, got {second}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn report_errors_exit_two() {
    let root = temp_dir("errors");
    let report = write_report(&root, Some("B3"));
    let fir_dir = root.join("fir");
    let out = root.join("plan.bin");

    assert_eq!(run(BIN, &[]).code, 2);
    assert!(run(BIN, &[]).stderr.contains("E_USAGE"));

    let missing = run(
        BIN,
        &[
            "--report",
            root.join("nope.json").to_str().unwrap(),
            "--fir-dir",
            fir_dir.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--sample-rate",
            "48000",
        ],
    );
    assert_eq!(missing.code, 2);
    assert!(missing.stderr.contains("E_IO"));

    let garbage = root.join("garbage.json");
    std::fs::write(&garbage, b"not json").expect("garbage");
    let bad_json = run(
        BIN,
        &[
            "--report",
            garbage.to_str().unwrap(),
            "--fir-dir",
            fir_dir.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--sample-rate",
            "48000",
        ],
    );
    assert_eq!(bad_json.code, 2);
    assert!(bad_json.stderr.contains("E_INPUT"));

    let wrong_format = root.join("wrong.json");
    std::fs::write(
        &wrong_format,
        br#"{"format":"something/else","variants":{}}"#,
    )
    .expect("json");
    let bad_format = run(
        BIN,
        &[
            "--report",
            wrong_format.to_str().unwrap(),
            "--fir-dir",
            fir_dir.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--sample-rate",
            "48000",
        ],
    );
    assert_eq!(bad_format.code, 2);
    assert!(bad_format.stderr.contains("E_INPUT"));

    let no_variant = write_report(&root, None);
    let missing_variant = run(
        BIN,
        &[
            "--report",
            no_variant.to_str().unwrap(),
            "--fir-dir",
            fir_dir.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--sample-rate",
            "48000",
        ],
    );
    assert_eq!(missing_variant.code, 2);
    assert!(missing_variant.stderr.contains("E_INPUT"));

    let unknown_variant = run(
        BIN,
        &[
            "--report",
            report.to_str().unwrap(),
            "--fir-dir",
            fir_dir.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--sample-rate",
            "48000",
            "--variant",
            "B9",
        ],
    );
    assert_eq!(unknown_variant.code, 2);
    assert!(unknown_variant.stderr.contains("E_INPUT"));

    // FIR 文件缺失 -> E_IO（先恢复 B3 报告与文件，再删掉一条 FIR）。
    let report = write_report(&root, Some("B3"));
    std::fs::remove_file(fir_dir.join("SUB2.csv")).expect("remove fir");
    let missing_fir = run(
        BIN,
        &[
            "--report",
            report.to_str().unwrap(),
            "--fir-dir",
            fir_dir.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--sample-rate",
            "48000",
        ],
    );
    assert_eq!(missing_fir.code, 2);
    assert!(missing_fir.stderr.contains("E_IO"));

    // 采样率不支持 / 通道数不足 -> E_FIR_PLAN_CONFIG。
    write_fir(&fir_dir.join("SUB2.csv"), &constant_gain(0.5));
    let bad_rate = run(
        BIN,
        &[
            "--report",
            report.to_str().unwrap(),
            "--fir-dir",
            fir_dir.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--sample-rate",
            "44100",
        ],
    );
    assert_eq!(bad_rate.code, 2);
    assert!(bad_rate.stderr.contains("E_FIR_PLAN_CONFIG"));
    let bad_channels = run(
        BIN,
        &[
            "--report",
            report.to_str().unwrap(),
            "--fir-dir",
            fir_dir.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--sample-rate",
            "48000",
            "--channels",
            "1",
        ],
    );
    assert_eq!(bad_channels.code, 2);
    assert!(bad_channels.stderr.contains("E_FIR_PLAN_CONFIG"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn existing_output_is_refused() {
    let root = temp_dir("write");
    let report = write_report(&root, Some("B3"));
    let out = root.join("plan.bin");
    std::fs::write(&out, b"old").expect("old file");
    let result = run(
        BIN,
        &[
            "--report",
            report.to_str().unwrap(),
            "--fir-dir",
            root.join("fir").to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--sample-rate",
            "48000",
        ],
    );
    assert_eq!(result.code, 1);
    assert!(result.stderr.contains("E_WRITE"));
    let _ = std::fs::remove_dir_all(&root);
}
