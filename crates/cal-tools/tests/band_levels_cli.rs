//! khaen-band-levels CLI 集成测试：WAV 读取、相对电平、确定性、错误码与退出码。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_khaen-band-levels");
const FS: u32 = 48_000;
const LEN: usize = 24_000;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "khaen-band-levels-{}-{}-{}",
        std::process::id(),
        tag,
        unique
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn sine(freq_hz: f64, amplitude: f64) -> Vec<f64> {
    (0..LEN)
        .map(|index| {
            amplitude * (2.0 * std::f64::consts::PI * freq_hz * index as f64 / f64::from(FS)).sin()
        })
        .collect()
}

/// samples 是交错排列的帧（单声道时就是逐样本）；16 bit PCM。
fn write_wav(path: &Path, sample_rate: u32, channels: u16, samples: &[f64]) {
    assert_eq!(
        samples.len() % usize::from(channels),
        0,
        "interleaved frames"
    );
    let data_len = samples.len() * 2;
    let mut bytes = Vec::with_capacity(44 + data_len);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    let byte_rate = sample_rate * u32::from(channels) * 2;
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&(channels * 2).to_le_bytes());
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
        .expect("spawn khaen-band-levels");
    Run {
        code: output.status.code().expect("exit code"),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn band_level(path: &Path, center_hz: f64) -> Option<f64> {
    let bytes = std::fs::read(path).expect("read levels");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("parse levels");
    let bands = value["points"][0]["bands"].as_array().expect("bands array");
    let band = bands
        .iter()
        .find(|band| (band["center_hz"].as_f64().expect("center") - center_hz).abs() < 1e-6)
        .expect("band exists");
    band["level_db"].as_f64()
}

#[test]
fn measures_relative_gain_from_two_wavs() {
    let root = temp_dir("gain");
    let measured = root.join("measured.wav");
    let reference = root.join("reference.wav");
    write_wav(&measured, FS, 1, &sine(1_000.0, 0.5));
    write_wav(&reference, FS, 1, &sine(1_000.0, 0.25));
    let out = root.join("levels.json");
    let result = run(&[
        "--measured",
        measured.to_str().unwrap(),
        "--reference",
        reference.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--centers",
        "100,1000,10000",
    ]);
    assert_eq!(result.code, 0, "{}", result.stderr);
    assert!(result.stdout.contains("\"tool\":\"khaen-band-levels\""));
    assert!(result.stdout.contains("\"bands\":3"));
    let level = band_level(&out, 1_000.0).expect("1 kHz level");
    assert!(
        (level - 6.0206).abs() <= 0.05,
        "1 kHz relative gain {level}"
    );
    // 参考侧静音带 → null。
    let silence_reference = root.join("silence.wav");
    write_wav(&silence_reference, FS, 1, &vec![0.0; LEN]);
    let silent_out = root.join("silent.json");
    let result = run(&[
        "--measured",
        measured.to_str().unwrap(),
        "--reference",
        silence_reference.to_str().unwrap(),
        "--out",
        silent_out.to_str().unwrap(),
        "--centers",
        "100,1000,10000",
    ]);
    assert_eq!(result.code, 0, "{}", result.stderr);
    assert!(result.stdout.contains("\"missing\":3"));
    assert_eq!(band_level(&silent_out, 1_000.0), None);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn output_is_deterministic_and_point_is_configurable() {
    let root = temp_dir("determinism");
    let measured = root.join("measured.wav");
    let reference = root.join("reference.wav");
    write_wav(&measured, FS, 1, &sine(1_000.0, 0.4));
    write_wav(&reference, FS, 1, &sine(1_000.0, 0.2));
    let out1 = root.join("levels1.json");
    let out2 = root.join("levels2.json");
    let first = run(&[
        "--measured",
        measured.to_str().unwrap(),
        "--reference",
        reference.to_str().unwrap(),
        "--out",
        out1.to_str().unwrap(),
        "--point",
        "MIC-1",
        "--centers",
        "1000",
    ]);
    let second = run(&[
        "--measured",
        measured.to_str().unwrap(),
        "--reference",
        reference.to_str().unwrap(),
        "--out",
        out2.to_str().unwrap(),
        "--point",
        "MIC-1",
        "--centers",
        "1000",
    ]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert_eq!(second.code, 0, "{}", second.stderr);
    let bytes1 = std::fs::read(&out1).expect("first");
    let bytes2 = std::fs::read(&out2).expect("second");
    assert_eq!(bytes1, bytes2);
    let text = String::from_utf8_lossy(&bytes1);
    assert!(text.contains("\"name\":\"MIC-1\""));
    assert!(text.contains("\"schema\":\"khaen-closed-loop-levels/v1\""));
    let sha = |stdout: &str| -> String {
        stdout
            .split("\"levels_sha256\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("sha")
            .to_string()
    };
    assert_eq!(sha(&first.stdout), sha(&second.stdout));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn usage_and_io_errors_exit_two() {
    assert_eq!(run(&[]).code, 2);
    assert!(run(&[]).stderr.contains("E_USAGE"));

    let root = temp_dir("errors");
    let measured = root.join("measured.wav");
    write_wav(&measured, FS, 1, &sine(1_000.0, 0.4));
    let missing = run(&[
        "--measured",
        root.join("nope.wav").to_str().unwrap(),
        "--reference",
        measured.to_str().unwrap(),
        "--out",
        root.join("out.json").to_str().unwrap(),
    ]);
    assert_eq!(missing.code, 2);
    assert!(missing.stderr.contains("E_IO"));

    let garbage = root.join("garbage.wav");
    std::fs::write(&garbage, b"this is not a wav file at all").expect("garbage");
    let bad = run(&[
        "--measured",
        garbage.to_str().unwrap(),
        "--reference",
        measured.to_str().unwrap(),
        "--out",
        root.join("out2.json").to_str().unwrap(),
    ]);
    assert_eq!(bad.code, 2);
    assert!(bad.stderr.contains("E_INPUT"));

    let other_rate = root.join("other_rate.wav");
    write_wav(&other_rate, 96_000, 1, &sine(1_000.0, 0.4));
    let mismatch = run(&[
        "--measured",
        measured.to_str().unwrap(),
        "--reference",
        other_rate.to_str().unwrap(),
        "--out",
        root.join("out3.json").to_str().unwrap(),
    ]);
    assert_eq!(mismatch.code, 2);
    assert!(mismatch.stderr.contains("E_BAND_LEVELS_ALIGNMENT"));

    let unknown = run(&["--nope", "x"]);
    assert_eq!(unknown.code, 2);
    assert!(unknown.stderr.contains("E_USAGE"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn stereo_wav_is_downmixed_and_existing_output_is_refused() {
    let root = temp_dir("stereo");
    let measured = root.join("measured.wav");
    let reference = root.join("reference.wav");
    let tone = sine(1_000.0, 0.4);
    let interleaved: Vec<f64> = tone.iter().flat_map(|&sample| [sample, sample]).collect();
    write_wav(&measured, FS, 2, &interleaved);
    let halved: Vec<f64> = sine(1_000.0, 0.2)
        .iter()
        .flat_map(|&sample| [sample, sample])
        .collect();
    write_wav(&reference, FS, 2, &halved);
    let out = root.join("levels.json");
    let result = run(&[
        "--measured",
        measured.to_str().unwrap(),
        "--reference",
        reference.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--centers",
        "1000",
    ]);
    assert_eq!(result.code, 0, "{}", result.stderr);
    let level = band_level(&out, 1_000.0).expect("level");
    assert!(
        (level - 6.0206).abs() <= 0.05,
        "stereo relative gain {level}"
    );

    let again = run(&[
        "--measured",
        measured.to_str().unwrap(),
        "--reference",
        reference.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--centers",
        "1000",
    ]);
    assert_eq!(again.code, 1);
    assert!(again.stderr.contains("E_WRITE"));
    let _ = std::fs::remove_dir_all(&root);
}
