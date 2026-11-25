//! khaen-closed-loop-run CLI 集成测试：四文件产物、确定性、三态退出码、错误处理。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use khaen_core::plan::{Plan, ProtectionEntry, ProtectionSection};
use khaen_plan_builder::{PlanBuilder, PlanConfig};

const BIN: &str = env!("CARGO_BIN_EXE_khaen-closed-loop-run");
const FS: u32 = 48_000;
const LEN: usize = 24_000;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "khaen-closed-loop-run-{}-{}-{}",
        std::process::id(),
        tag,
        unique
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn constant_gain_peq(gain_db: f64) -> khaen_core::plan::PeqSection {
    khaen_core::plan::PeqSection {
        b0: 10.0f64.powf(gain_db / 20.0) as f32,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    }
}

fn fixture_plan() -> Plan {
    PlanBuilder::new(PlanConfig::new(128, FS, 1))
        .set_content_identity(1)
        .set_crossover_identity(1)
        .set_matrix_identity(1)
        .set_protection(ProtectionSection {
            lanes: 1,
            entries: vec![ProtectionEntry {
                lane: 0,
                delay: 0,
                polarity: 1,
                gain_db: 0.0,
                peq: vec![constant_gain_peq(6.0)],
            }],
        })
        .set_routing_identity(1)
        .build()
        .expect("fixture plan")
}

fn write_plan(root: &Path) -> PathBuf {
    let path = root.join("plan.bin");
    let bytes = fixture_plan().encode_checked().expect("encode plan");
    std::fs::write(&path, bytes).expect("write plan");
    path
}

fn tone(freq_hz: f64, amplitude: f64) -> Vec<f64> {
    (0..LEN)
        .map(|index| {
            amplitude * (2.0 * std::f64::consts::PI * freq_hz * index as f64 / f64::from(FS)).sin()
        })
        .collect()
}

fn baseline() -> Vec<f64> {
    let mut samples = tone(100.0, 0.05);
    for (target, extra) in samples
        .iter_mut()
        .zip(tone(1_000.0, 0.05).iter().zip(tone(10_000.0, 0.05).iter()))
    {
        *target += extra.0 + extra.1;
    }
    samples
}

fn scaled(samples: &[f64], factor: f64) -> Vec<f64> {
    samples.iter().map(|value| value * factor).collect()
}

fn write_wav(path: &Path, sample_rate: u32, samples: &[f64]) {
    let data_len = samples.len() * 2;
    let mut bytes = Vec::with_capacity(44 + data_len);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data_len as u32).to_le_bytes());
    for sample in samples {
        let value = (sample * 32_767.0).round().clamp(-32_768.0, 32_767.0) as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    std::fs::write(path, bytes).expect("write wav");
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
        .expect("spawn khaen-closed-loop-run");
    Run {
        code: output.status.code().expect("exit code"),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn read_manifest(dir: &Path) -> serde_json::Value {
    let bytes = std::fs::read(dir.join("manifest.json")).expect("manifest");
    serde_json::from_slice(&bytes).expect("manifest json")
}

struct Setup {
    root: PathBuf,
    plan: PathBuf,
    baseline: PathBuf,
    applied: PathBuf,
}

fn setup(tag: &str, applied_factor: f64) -> Setup {
    let root = temp_dir(tag);
    let plan = write_plan(&root);
    let baseline_samples = baseline();
    let applied_samples = scaled(&baseline_samples, applied_factor);
    let baseline = root.join("baseline.wav");
    let applied = root.join("applied.wav");
    write_wav(&baseline, FS, &baseline_samples);
    write_wav(&applied, FS, &applied_samples);
    Setup {
        root,
        plan,
        baseline,
        applied,
    }
}

fn args_for<'a>(setup: &'a Setup, out: &'a Path) -> Vec<String> {
    vec![
        "--plan".to_string(),
        setup.plan.to_str().unwrap().to_string(),
        "--baseline".to_string(),
        setup.baseline.to_str().unwrap().to_string(),
        "--applied".to_string(),
        setup.applied.to_str().unwrap().to_string(),
        "--out".to_string(),
        out.to_str().unwrap().to_string(),
        "--centers".to_string(),
        "100,1000,10000".to_string(),
    ]
}

fn run_args(args: &[String]) -> Run {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run(&refs)
}

#[test]
fn pass_writes_four_reproducible_files() {
    let setup = setup("pass", 2.0);
    let out1 = setup.root.join("out1");
    let first = run_args(&args_for(&setup, &out1));
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert!(first.stdout.contains("\"verdict\":\"PASS\""));
    for name in [
        "predicted.json",
        "observed.json",
        "report.json",
        "manifest.json",
    ] {
        assert!(out1.join(name).is_file(), "missing {name}");
    }
    let manifest = read_manifest(&out1);
    assert_eq!(manifest["verdict"], "PASS");
    assert_eq!(manifest["point"], "P01");
    assert_eq!(manifest["lane"], 0);
    assert_eq!(manifest["centers_hz"].as_array().expect("centers").len(), 3);
    let plan_bytes = std::fs::read(&setup.plan).expect("plan");
    assert_eq!(
        manifest["inputs"]["plan_sha256"]
            .as_str()
            .expect("plan sha"),
        khaen_core::sha256::sha256_hex(&plan_bytes)
    );
    let report_bytes = std::fs::read(out1.join("report.json")).expect("report");
    assert_eq!(
        manifest["outputs"]["report_json_sha256"]
            .as_str()
            .expect("report sha"),
        khaen_core::sha256::sha256_hex(&report_bytes)
    );

    // 同输入到另一个目录：四份文件逐字节相同。
    let out2 = setup.root.join("out2");
    let second = run_args(&args_for(&setup, &out2));
    assert_eq!(second.code, 0, "{}", second.stderr);
    for name in [
        "predicted.json",
        "observed.json",
        "report.json",
        "manifest.json",
    ] {
        assert_eq!(
            std::fs::read(out1.join(name)).expect("first"),
            std::fs::read(out2.join(name)).expect("second"),
            "{name} differs"
        );
    }
    let _ = std::fs::remove_dir_all(&setup.root);
}

#[test]
fn failed_verdict_exits_three() {
    let setup = setup("fail", 1.0);
    let out = setup.root.join("out");
    let result = run_args(&args_for(&setup, &out));
    assert_eq!(result.code, 3, "{}", result.stderr);
    assert!(result.stdout.contains("\"verdict\":\"FAIL\""));
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(out.join("report.json")).expect("report"))
            .expect("json");
    assert_eq!(report["verdict"], "FAIL");
    assert_eq!(report["violations"], 3);
    let _ = std::fs::remove_dir_all(&setup.root);
}

#[test]
fn silent_recording_is_incomplete_exit_four() {
    let setup = setup("incomplete", 0.0);
    let out = setup.root.join("out");
    let result = run_args(&args_for(&setup, &out));
    assert_eq!(result.code, 4, "{}", result.stderr);
    assert!(result.stdout.contains("\"verdict\":\"INCOMPLETE\""));
    let _ = std::fs::remove_dir_all(&setup.root);
}

#[test]
fn input_usage_and_alignment_errors_exit_two() {
    assert_eq!(run(&[]).code, 2);
    assert!(run(&[]).stderr.contains("E_USAGE"));

    let setup = setup("errors", 2.0);
    let out = setup.root.join("out");

    let missing = run(&[
        "--plan",
        setup.root.join("nope.bin").to_str().unwrap(),
        "--baseline",
        setup.baseline.to_str().unwrap(),
        "--applied",
        setup.applied.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(missing.code, 2);
    assert!(missing.stderr.contains("E_IO"));

    let garbage = setup.root.join("garbage.bin");
    std::fs::write(&garbage, b"not a plan").expect("garbage");
    let bad_plan = run(&[
        "--plan",
        garbage.to_str().unwrap(),
        "--baseline",
        setup.baseline.to_str().unwrap(),
        "--applied",
        setup.applied.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(bad_plan.code, 2);
    assert!(bad_plan.stderr.contains("E_PLAN"));

    // 两段录音采样率不一致。
    let other_rate = setup.root.join("other.wav");
    write_wav(&other_rate, 96_000, &baseline());
    let mismatch = run(&[
        "--plan",
        setup.plan.to_str().unwrap(),
        "--baseline",
        setup.baseline.to_str().unwrap(),
        "--applied",
        other_rate.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(mismatch.code, 2);
    assert!(mismatch.stderr.contains("E_CLOSED_LOOP_RUN_ALIGNMENT"));

    // 配置错误（窗长不是 2 的幂）。
    let bad_window = run(&[
        "--plan",
        setup.plan.to_str().unwrap(),
        "--baseline",
        setup.baseline.to_str().unwrap(),
        "--applied",
        setup.applied.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--window",
        "1000",
    ]);
    assert_eq!(bad_window.code, 2);
    assert!(bad_window.stderr.contains("E_CLOSED_LOOP_RUN_CONFIG"));

    // 测点名非法。
    let bad_point = run(&[
        "--plan",
        setup.plan.to_str().unwrap(),
        "--baseline",
        setup.baseline.to_str().unwrap(),
        "--applied",
        setup.applied.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--point",
        "P 01",
    ]);
    assert_eq!(bad_point.code, 2);
    assert!(bad_point.stderr.contains("E_CLOSED_LOOP_RUN_CONFIG"));
    let _ = std::fs::remove_dir_all(&setup.root);
}

#[test]
fn non_empty_output_directory_is_refused() {
    let setup = setup("nonempty", 2.0);
    let out = setup.root.join("out");
    std::fs::create_dir_all(&out).expect("out dir");
    std::fs::write(out.join("old.txt"), "old").expect("old file");
    let result = run_args(&args_for(&setup, &out));
    assert_eq!(result.code, 1, "{}", result.stderr);
    assert!(result.stderr.contains("E_WRITE"));
    let _ = std::fs::remove_dir_all(&setup.root);
}
