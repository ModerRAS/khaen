//! khaen-closed-loop CLI 集成测试：退出码三态、可复现报告、非法输入、非空输出目录。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_khaen-closed-loop");

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "khaen-closed-loop-{}-{}-{}",
        std::process::id(),
        tag,
        unique
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// 单测点 P01、4 个 1/3 oct 带；predicted 全 60 dB，observed = 60 + delta。
fn levels_json(rate: f64, deltas: &[Option<f64>]) -> String {
    let centers = [40.0, 50.0, 63.0, 80.0];
    let bands: Vec<String> = centers
        .iter()
        .zip(deltas.iter())
        .map(|(center, delta)| {
            let observed = delta.map(|delta| 60.0 + delta);
            format!(
                "{{\"center_hz\":{center},\"level_db\":{}}}",
                match observed {
                    Some(value) => format!("{value}"),
                    None => "null".to_string(),
                }
            )
        })
        .collect();
    format!(
        "{{\"schema\":\"khaen-closed-loop-levels/v1\",\"sample_rate_hz\":{rate},\"points\":[{{\"name\":\"P01\",\"bands\":[{}]}}]}}",
        bands.join(",")
    )
}

fn predicted_json(rate: f64) -> String {
    let centers = [40.0, 50.0, 63.0, 80.0];
    let bands: Vec<String> = centers
        .iter()
        .map(|center| format!("{{\"center_hz\":{center},\"level_db\":60.0}}"))
        .collect();
    format!(
        "{{\"schema\":\"khaen-closed-loop-levels/v1\",\"sample_rate_hz\":{rate},\"points\":[{{\"name\":\"P01\",\"bands\":[{}]}}]}}",
        bands.join(",")
    )
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str]) -> Run {
    let output = Command::new(BIN)
        .args(args)
        .output()
        .expect("spawn khaen-closed-loop");
    Run {
        code: output.status.code().expect("exit code"),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).expect("write fixture");
}

#[test]
fn pass_writes_reproducible_report() {
    let root = temp_dir("pass");
    let predicted = root.join("predicted.json");
    let observed = root.join("observed.json");
    write(&predicted, &predicted_json(48_000.0));
    write(
        &observed,
        &levels_json(48_000.0, &[Some(0.5), Some(-1.0), Some(2.0), Some(-2.0)]),
    );
    let predicted_path = predicted.to_str().unwrap().to_string();
    let observed_path = observed.to_str().unwrap().to_string();

    let out1 = root.join("out1");
    let first = run(&[
        "--predicted",
        &predicted_path,
        "--observed",
        &observed_path,
        "--out",
        out1.to_str().unwrap(),
    ]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert!(first.stdout.contains("\"verdict\":\"PASS\""));
    assert!(first.stdout.contains("\"report_sha256\":\""));
    let report1 = std::fs::read(out1.join("report.json")).expect("report json");
    let text = String::from_utf8_lossy(&report1);
    assert!(text.contains("\"verdict\":\"PASS\""));
    assert!(text.contains("\"schema\":\"khaen-closed-loop-report/v1\""));
    assert!(report1.ends_with(b"\n"));

    // 同输入到另一个空目录：报告逐字节相同（可复现 sha）。
    let out2 = root.join("out2");
    let second = run(&[
        "--predicted",
        &predicted_path,
        "--observed",
        &observed_path,
        "--out",
        out2.to_str().unwrap(),
    ]);
    assert_eq!(second.code, 0);
    let report2 = std::fs::read(out2.join("report.json")).expect("second report");
    assert_eq!(report1, report2);
    // sha 也在 stdout 里一致。
    let sha1 = first
        .stdout
        .split("\"report_sha256\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("sha");
    let sha2 = second
        .stdout
        .split("\"report_sha256\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("sha");
    assert_eq!(sha1, sha2);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn fail_exits_three_and_names_violation() {
    let root = temp_dir("fail");
    let predicted = root.join("predicted.json");
    let observed = root.join("observed.json");
    write(&predicted, &predicted_json(48_000.0));
    write(
        &observed,
        &levels_json(48_000.0, &[Some(0.5), Some(3.5), Some(-0.5), Some(0.0)]),
    );
    let out = root.join("out");
    let result = run(&[
        "--predicted",
        predicted.to_str().unwrap(),
        "--observed",
        observed.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(result.code, 3, "{}", result.stderr);
    assert!(result.stdout.contains("\"verdict\":\"FAIL\""));
    assert!(result.stdout.contains("\"violations\":1"));
    let report = std::fs::read_to_string(out.join("report.json")).expect("report");
    assert!(report.contains("\"verdict\":\"FAIL\""));
    assert!(report.contains("\"within_tolerance\":false"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn incomplete_exits_four() {
    let root = temp_dir("incomplete");
    let predicted = root.join("predicted.json");
    let observed = root.join("observed.json");
    write(&predicted, &predicted_json(48_000.0));
    write(
        &observed,
        &levels_json(48_000.0, &[Some(0.5), None, Some(-0.5), None]),
    );
    let out = root.join("out");
    let result = run(&[
        "--predicted",
        predicted.to_str().unwrap(),
        "--observed",
        observed.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(result.code, 4, "{}", result.stderr);
    assert!(result.stdout.contains("\"verdict\":\"INCOMPLETE\""));
    assert!(result.stdout.contains("\"compared_bands\":2"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn bad_inputs_exit_two() {
    let root = temp_dir("bad");
    // 缺参数 → E_USAGE。
    let missing = run(&[]);
    assert_eq!(missing.code, 2);
    assert!(missing.stderr.contains("E_USAGE"));

    // 非法 JSON → E_INPUT。
    let predicted = root.join("predicted.json");
    let observed = root.join("observed.json");
    write(&predicted, "not json");
    write(
        &observed,
        &levels_json(48_000.0, &[Some(0.0), Some(0.0), Some(0.0), Some(0.0)]),
    );
    let out = root.join("out");
    let result = run(&[
        "--predicted",
        predicted.to_str().unwrap(),
        "--observed",
        observed.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(result.code, 2);
    assert!(result.stderr.contains("E_INPUT"));

    // schema 不对 → E_INPUT。
    write(&predicted, "{\"schema\":\"wrong/v1\"}");
    let result = run(&[
        "--predicted",
        predicted.to_str().unwrap(),
        "--observed",
        observed.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(result.code, 2);
    assert!(result.stderr.contains("E_INPUT"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn non_empty_output_directory_is_refused() {
    let root = temp_dir("nonempty");
    let predicted = root.join("predicted.json");
    let observed = root.join("observed.json");
    write(&predicted, &predicted_json(48_000.0));
    write(
        &observed,
        &levels_json(48_000.0, &[Some(0.0), Some(0.0), Some(0.0), Some(0.0)]),
    );
    let out = root.join("out");
    std::fs::create_dir_all(&out).expect("out dir");
    write(&out.join("leftover.txt"), "old");
    let result = run(&[
        "--predicted",
        predicted.to_str().unwrap(),
        "--observed",
        observed.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(result.code, 1, "{}", result.stderr);
    assert!(result.stderr.contains("E_WRITE"));
    let _ = std::fs::remove_dir_all(&root);
}
