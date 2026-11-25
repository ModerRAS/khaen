//! khaen-fir-plan CLI：CSV -> 方案文件、确定性、与预测电平/错误码联动。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_khaen-fir-plan");
const PLAN_LEVELS_BIN: &str = env!("CARGO_BIN_EXE_khaen-plan-levels");

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "khaen-fir-plan-{}-{}-{}",
        std::process::id(),
        tag,
        unique
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_fir(path: &Path, taps: &[f64]) {
    let mut text = String::from("# fir taps from mimo fir_csv\n");
    for tap in taps {
        text.push_str(&format!("{tap}\n"));
    }
    text.push('\n');
    std::fs::write(path, text).expect("write fir csv");
}

fn constant_gain(taps: usize, gain: f64) -> Vec<f64> {
    let mut values = vec![0.0; taps];
    values[0] = gain;
    values
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

#[test]
fn compiles_two_lane_plan_and_is_deterministic() {
    let root = temp_dir("ok");
    let lane0 = root.join("lane0.csv");
    let lane1 = root.join("lane1.csv");
    write_fir(&lane0, &constant_gain(8, 2.0));
    write_fir(&lane1, &constant_gain(8, 0.5));
    let out1 = root.join("plan1.bin");
    let first = run(
        BIN,
        &[
            "--fir",
            lane0.to_str().unwrap(),
            "--fir",
            lane1.to_str().unwrap(),
            "--out",
            out1.to_str().unwrap(),
            "--sample-rate",
            "48000",
        ],
    );
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert!(first.stdout.contains("\"ok\":true"));
    assert!(first.stdout.contains("\"lanes\":2"));
    assert!(first.stdout.contains("\"sha256\":\""));
    let bytes = std::fs::read(&out1).expect("plan bytes");
    assert!(bytes.starts_with(b"KHAENPLN"));

    let out2 = root.join("plan2.bin");
    let second = run(
        BIN,
        &[
            "--fir",
            lane0.to_str().unwrap(),
            "--fir",
            lane1.to_str().unwrap(),
            "--out",
            out2.to_str().unwrap(),
            "--sample-rate",
            "48000",
        ],
    );
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert_eq!(bytes, std::fs::read(&out2).expect("plan2"));
    let sha = |stdout: &str| -> String {
        stdout
            .split("\"sha256\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("sha")
            .to_string()
    };
    assert_eq!(sha(&first.stdout), sha(&second.stdout));
    assert_eq!(sha(&first.stdout).len(), 64);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn generated_plan_feeds_predicted_levels() {
    let root = temp_dir("levels");
    let lane0 = root.join("lane0.csv");
    write_fir(&lane0, &constant_gain(8, 2.0));
    let plan = root.join("plan.bin");
    let result = run(
        BIN,
        &[
            "--fir",
            lane0.to_str().unwrap(),
            "--out",
            plan.to_str().unwrap(),
            "--sample-rate",
            "48000",
            "--channels",
            "1",
            "--block-size",
            "128",
        ],
    );
    assert_eq!(result.code, 0, "{}", result.stderr);
    let levels = root.join("predicted.json");
    let predicted = run(
        PLAN_LEVELS_BIN,
        &[
            "--plan",
            plan.to_str().unwrap(),
            "--out",
            levels.to_str().unwrap(),
            "--centers",
            "100,1000,10000",
        ],
    );
    assert_eq!(predicted.code, 0, "{}", predicted.stderr);
    let text = std::fs::read_to_string(&levels).expect("levels");
    assert!(text.contains("\"name\":\"L00\""));
    // +6.0206 dB 出现三次（三个中心频率）。
    assert_eq!(text.matches("6.020").count(), 3);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn partial_lane_coverage_keeps_other_lanes_identity() {
    let root = temp_dir("partial");
    let lane1 = root.join("lane1.csv");
    write_fir(&lane1, &constant_gain(4, 2.0));
    let plan = root.join("plan.bin");
    let result = run(
        BIN,
        &[
            "--fir",
            lane1.to_str().unwrap(),
            "--out",
            plan.to_str().unwrap(),
            "--sample-rate",
            "48000",
            "--channels",
            "2",
        ],
    );
    assert_eq!(result.code, 0, "{}", result.stderr);
    // 第 0 条 --fir 总是 lane 0；这里只有一条 FIR，所以 lane 0 有 +6 dB，lane 1 恒等。
    let levels = root.join("predicted.json");
    let predicted = run(
        PLAN_LEVELS_BIN,
        &[
            "--plan",
            plan.to_str().unwrap(),
            "--out",
            levels.to_str().unwrap(),
            "--centers",
            "1000",
        ],
    );
    assert_eq!(predicted.code, 0, "{}", predicted.stderr);
    let text = std::fs::read_to_string(&levels).expect("levels");
    assert!(text.contains("\"name\":\"L00\""));
    assert!(text.contains("\"name\":\"L01\""));
    assert!(text.contains("\"level_db\":0.000000000e0"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn usage_csv_and_config_errors_exit_two() {
    assert_eq!(run(BIN, &[]).code, 2);
    assert!(run(BIN, &[]).stderr.contains("E_USAGE"));

    let root = temp_dir("errors");
    let good = root.join("good.csv");
    write_fir(&good, &constant_gain(4, 1.5));
    let out = root.join("plan.bin");

    let missing = run(
        BIN,
        &[
            "--fir",
            root.join("nope.csv").to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--sample-rate",
            "48000",
        ],
    );
    assert_eq!(missing.code, 2);
    assert!(missing.stderr.contains("E_IO"));

    let comma = root.join("comma.csv");
    std::fs::write(&comma, "1.0,2.0\n").expect("comma csv");
    let bad_csv = run(
        BIN,
        &[
            "--fir",
            comma.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--sample-rate",
            "48000",
        ],
    );
    assert_eq!(bad_csv.code, 2);
    assert!(bad_csv.stderr.contains("E_INPUT"));

    let not_number = root.join("text.csv");
    std::fs::write(&not_number, "hello\n").expect("text csv");
    let bad_tap = run(
        BIN,
        &[
            "--fir",
            not_number.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--sample-rate",
            "48000",
        ],
    );
    assert_eq!(bad_tap.code, 2);
    assert!(bad_tap.stderr.contains("E_INPUT"));

    let bad_rate = run(
        BIN,
        &[
            "--fir",
            good.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--sample-rate",
            "44100",
        ],
    );
    assert_eq!(bad_rate.code, 2);
    assert!(bad_rate.stderr.contains("E_FIR_PLAN_CONFIG"));

    let bad_block = run(
        BIN,
        &[
            "--fir",
            good.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--sample-rate",
            "48000",
            "--block-size",
            "100",
        ],
    );
    assert_eq!(bad_block.code, 2);
    assert!(bad_block.stderr.contains("E_FIR_PLAN_CONFIG"));

    let unknown = run(BIN, &["--nope", "x"]);
    assert_eq!(unknown.code, 2);
    assert!(unknown.stderr.contains("E_USAGE"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn existing_output_is_refused() {
    let root = temp_dir("write");
    let fir = root.join("lane.csv");
    write_fir(&fir, &constant_gain(4, 1.0));
    let out = root.join("plan.bin");
    std::fs::write(&out, b"old").expect("old file");
    let result = run(
        BIN,
        &[
            "--fir",
            fir.to_str().unwrap(),
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
