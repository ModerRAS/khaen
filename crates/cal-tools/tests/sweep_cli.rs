//! khaen-sweep CLI 验收：WAV 格式/时长、确定性、幅度与声道、坏参数退出码。
//!
//! 全部离线（Windows 本机可跑），不碰设备。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use khaen_calibration::wav::parse_wav;

const BIN: &str = env!("CARGO_BIN_EXE_khaen-sweep");

fn temp_dir(tag: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let number = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "khaen-sweep-cli-{}-{}-{}",
        std::process::id(),
        tag,
        number
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .output()
        .expect("spawn khaen-sweep")
}

fn summary(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "khaen-sweep failed: status={:?} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout is one JSON object")
}

fn wav_bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).expect("read generated wav")
}

#[test]
fn generates_expected_format_and_duration() {
    let dir = temp_dir("format");
    let out = dir.join("sweep.wav");
    let out_arg = out.to_string_lossy().to_string();
    let output = run(&[
        "--out",
        out_arg.as_str(),
        "--rate",
        "48000",
        "--channels",
        "2",
        "--lead",
        "0.1",
        "--gap",
        "0.1",
        "--tail",
        "0.1",
        "--segment",
        "0.2:20:20000",
        "--segment",
        "0.3:20:200",
    ]);
    let summary = summary(&output);
    assert_eq!(summary["tool"], "khaen-sweep");
    let expected_seconds = 0.1 + 0.2 + 0.1 + 0.3 + 0.1;
    let duration = summary["duration_s"].as_f64().expect("duration_s");
    assert!(
        (duration - expected_seconds).abs() < 1e-6,
        "duration {duration} != {expected_seconds}"
    );
    let wav = parse_wav(&wav_bytes(&out)).expect("parse generated wav");
    assert_eq!(wav.channels, 2);
    assert!((wav.sample_rate_hz - 48_000.0).abs() < 1e-9);
    let expected_frames = (expected_seconds * 48_000.0).round() as usize;
    assert_eq!(wav.samples.len(), expected_frames);
    // lead/tail 是静音：首尾样本必须为 0（多声道平均后仍是 0）。
    assert_eq!(wav.samples[0], 0.0);
    assert_eq!(wav.samples[expected_frames - 1], 0.0);
    // 两个声道写同一单声道信号：原始 data 里第 0 帧 L/R 三字节应相同。
    let bytes = wav_bytes(&out);
    let data_start = 44usize;
    assert_eq!(
        &bytes[data_start..data_start + 3],
        &bytes[data_start + 3..data_start + 6]
    );
    let _ = &summary["segments"];
}

#[test]
fn output_is_deterministic() {
    let dir = temp_dir("deterministic");
    let first = dir.join("a.wav");
    let second = dir.join("b.wav");
    let run_one = |path: &Path| {
        run(&[
            "--out",
            &path.to_string_lossy(),
            "--rate",
            "48000",
            "--channels",
            "2",
            "--lead",
            "0.05",
            "--gap",
            "0.05",
            "--tail",
            "0.05",
            "--segment",
            "0.2:50:5000",
        ])
    };
    let first_summary = summary(&run_one(&first));
    let second_summary = summary(&run_one(&second));
    assert_eq!(first_summary["sha256"], second_summary["sha256"]);
    assert_eq!(wav_bytes(&first), wav_bytes(&second));
}

#[test]
fn amplitude_is_bounded_and_signal_is_not_silent() {
    let dir = temp_dir("amplitude");
    let out = dir.join("sweep.wav");
    let output = run(&[
        "--out",
        &out.to_string_lossy(),
        "--amplitude-db",
        "-12",
        "--lead",
        "0.0",
        "--tail",
        "0.0",
        "--segment",
        "0.1:100:2000",
    ]);
    let summary = summary(&output);
    assert_eq!(summary["channels"], 2);
    let wav = parse_wav(&wav_bytes(&out)).expect("parse generated wav");
    let peak = wav
        .samples
        .iter()
        .fold(0.0f64, |acc, value| acc.max(value.abs()));
    let bound = 10f64.powf(-12.0 / 20.0);
    assert!(
        peak <= bound + 1e-3,
        "peak {peak} exceeds amplitude bound {bound}"
    );
    assert!(peak > bound * 0.5, "signal too quiet: peak {peak}");
    assert!(wav.samples.iter().any(|value| *value != 0.0));
}

#[test]
fn rejects_bad_arguments_with_exit_code_two() {
    let dir = temp_dir("bad");
    let out = dir.join("never.wav");
    let out = out.to_string_lossy().to_string();
    let cases: Vec<Vec<&str>> = vec![
        vec![],
        vec!["--out", &out, "--rate", "1000"],
        vec!["--out", &out, "--channels", "0"],
        vec!["--out", &out, "--segment", "1:200:20"],
        vec!["--out", &out, "--segment", "1:20:30000"],
        vec!["--out", &out, "--segment", "bad"],
    ];
    for args in cases {
        let output = run(&args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "args {args:?} should exit 2, stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(std::fs::metadata(dir.join("never.wav")).is_err());
}
