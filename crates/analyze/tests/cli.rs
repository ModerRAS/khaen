//! CLI 行为：确定性（同输入两次逐字节相同）、stdout 摘要、退出码与错误码。

mod common;

use std::path::Path;

use common::*;

fn path_arg(path: &Path) -> String {
    path.display().to_string()
}

fn tone_wav(dir: &Path) -> std::path::PathBuf {
    let wav = dir.join("tone.wav");
    write_float32(
        &wav,
        FS,
        1,
        &sine_with_block(1_000.0, 0.5, FS as usize * 2, FS, (38_400, 48_000, 0.0005)),
    );
    wav
}

#[test]
fn spectrum_output_is_deterministic() {
    let dir = temp_dir("cli-spectrum-determinism");
    let wav = tone_wav(&dir);
    let out = dir.join("nested").join("spectrum.json");
    let args: Vec<String> = vec![
        "spectrum".into(),
        "--wav".into(),
        path_arg(&wav),
        "--out".into(),
        path_arg(&out),
        "--centers".into(),
        "100,1000,10000".into(),
        "--psd".into(),
        "1".into(),
    ];
    let first = run_args(&args);
    assert_eq!(first.code, 0, "{}", first.stderr);
    let first_bytes = std::fs::read(&out).expect("read first output");
    assert_eq!(first_bytes.last(), Some(&b'\n'), "trailing newline");
    let summary = first.summary();
    assert_eq!(summary["ok"], true);
    assert_eq!(summary["command"], "spectrum");
    assert_eq!(summary["bands"], 3);
    assert_eq!(
        summary["levels_sha256"].as_str().expect("sha"),
        sha256_file(&out)
    );
    assert_eq!(first.stdout.lines().count(), 1);
    std::fs::remove_file(&out).expect("remove output");
    let second = run_args(&args);
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert_eq!(
        std::fs::read(&out).expect("read second output"),
        first_bytes
    );
    assert_eq!(second.stdout, first.stdout, "summary must be identical");
}

#[test]
fn envelope_output_is_deterministic() {
    let dir = temp_dir("cli-envelope-determinism");
    let wav = tone_wav(&dir);
    let out = dir.join("envelope.json");
    let args: Vec<String> = vec![
        "envelope".into(),
        "--wav".into(),
        path_arg(&wav),
        "--out".into(),
        path_arg(&out),
    ];
    let first = run_args(&args);
    assert_eq!(first.code, 0, "{}", first.stderr);
    let first_bytes = std::fs::read(&out).expect("read first output");
    let summary = first.summary();
    assert_eq!(summary["verdict"], "dropout");
    assert_eq!(
        summary["levels_sha256"].as_str().expect("sha"),
        sha256_file(&out)
    );
    std::fs::remove_file(&out).expect("remove output");
    let second = run_args(&args);
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert_eq!(
        std::fs::read(&out).expect("read second output"),
        first_bytes
    );
    assert_eq!(second.stdout, first.stdout);
}

#[test]
fn compare_output_is_deterministic() {
    let dir = temp_dir("cli-compare-determinism");
    let reference = dir.join("reference.wav");
    let measured = dir.join("measured.wav");
    write_float32(&reference, FS, 1, &sine(1_000.0, 0.5, FS as usize, FS));
    write_float32(&measured, FS, 1, &sine(1_000.0, 0.25, FS as usize, FS));
    let out = dir.join("compare.json");
    let args: Vec<String> = vec![
        "compare".into(),
        "--reference".into(),
        path_arg(&reference),
        "--measured".into(),
        path_arg(&measured),
        "--out".into(),
        path_arg(&out),
        "--centers".into(),
        "100,1000,10000".into(),
    ];
    let first = run_args(&args);
    assert_eq!(first.code, 0, "{}", first.stderr);
    let first_bytes = std::fs::read(&out).expect("read first output");
    std::fs::remove_file(&out).expect("remove output");
    let second = run_args(&args);
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert_eq!(
        std::fs::read(&out).expect("read second output"),
        first_bytes
    );
    assert_eq!(second.stdout, first.stdout);
}

#[test]
fn usage_errors_exit_2_with_stable_codes() {
    let dir = temp_dir("cli-usage");
    let wav = tone_wav(&dir);
    let out = dir.join("out.json");
    let cases: Vec<Vec<String>> = vec![
        vec![],
        vec!["frobnicate".into()],
        vec!["spectrum".into()],
        vec!["spectrum".into(), "--out".into(), path_arg(&out)],
        vec![
            "spectrum".into(),
            "--wav".into(),
            path_arg(&wav),
            "--out".into(),
            path_arg(&out),
            "--psd".into(),
            "2".into(),
        ],
        vec![
            "spectrum".into(),
            "--wav".into(),
            path_arg(&wav),
            "--out".into(),
            path_arg(&out),
            "--centers".into(),
            "100,,1000".into(),
        ],
        vec![
            "spectrum".into(),
            "--wav".into(),
            path_arg(&wav),
            "--out".into(),
            path_arg(&out),
            "--bogus".into(),
        ],
        vec!["envelope".into(), "--wav".into(), path_arg(&wav)],
        vec!["compare".into(), "--reference".into(), path_arg(&wav)],
    ];
    for (index, args) in cases.iter().enumerate() {
        let result = run_args(args);
        assert_eq!(result.code, 2, "case {index}: {}", result.stderr);
        assert!(
            result.stderr.contains("E_USAGE"),
            "case {index}: {}",
            result.stderr
        );
    }
}

#[test]
fn wav_errors_exit_2_with_stable_codes() {
    let dir = temp_dir("cli-wav-errors");
    let out = dir.join("out.json");
    let garbage = dir.join("garbage.wav");
    std::fs::write(&garbage, b"definitely not a wav file").expect("write garbage");
    let missing = dir.join("missing.wav");
    let cases: Vec<(Vec<String>, &str)> = vec![
        (
            vec![
                "spectrum".into(),
                "--wav".into(),
                path_arg(&missing),
                "--out".into(),
                path_arg(&out),
            ],
            "E_WAV_IO",
        ),
        (
            vec![
                "envelope".into(),
                "--wav".into(),
                path_arg(&missing),
                "--out".into(),
                path_arg(&out),
            ],
            "E_WAV_IO",
        ),
        (
            vec![
                "spectrum".into(),
                "--wav".into(),
                path_arg(&garbage),
                "--out".into(),
                path_arg(&out),
            ],
            "E_WAV_INPUT",
        ),
    ];
    for (index, (args, code)) in cases.iter().enumerate() {
        let result = run_args(args);
        assert_eq!(result.code, 2, "case {index}: {}", result.stderr);
        assert!(
            result.stderr.contains(code),
            "case {index}: expected {code}, got {}",
            result.stderr
        );
        assert!(!out.exists(), "case {index} must not write output");
    }
}

#[test]
fn existing_output_exits_1_with_write_code() {
    let dir = temp_dir("cli-write");
    let wav = tone_wav(&dir);
    let out = dir.join("spectrum.json");
    let args: Vec<String> = vec![
        "spectrum".into(),
        "--wav".into(),
        path_arg(&wav),
        "--out".into(),
        path_arg(&out),
    ];
    let first = run_args(&args);
    assert_eq!(first.code, 0, "{}", first.stderr);
    let second = run_args(&args);
    assert_eq!(second.code, 1, "{}", second.stderr);
    assert!(second.stderr.contains("E_WRITE"), "{}", second.stderr);
}

#[test]
fn help_exits_0() {
    for flag in ["--help", "-h", "help"] {
        let result = run_bin(&[flag]);
        assert_eq!(result.code, 0, "{flag}: {}", result.stderr);
        assert!(result.stdout.contains("spectrum"));
        assert!(result.stdout.contains("envelope"));
        assert!(result.stdout.contains("compare"));
    }
}
