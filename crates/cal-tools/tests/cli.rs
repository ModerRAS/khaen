//! khaen-single-point CLI 验收：合法场景、确定性、三态退出码、坏输入、空目录约束。
//!
//! 全部离线（Windows 本机可跑），不碰设备。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_khaen-single-point");
const NFFT: usize = 8192;

fn scene_json(safety: bool, quality: Option<Vec<Value>>) -> Value {
    let freqs: Vec<f64> = [7usize, 9, 11, 13, 17, 21]
        .iter()
        .map(|bin| *bin as f64 * 48_000.0 / NFFT as f64)
        .collect();
    let sources = ["SUB1", "SUB2"];
    let mics = ["P01", "P02", "P03", "P04", "P05"];
    let mut h = Vec::new();
    for _mic in 0..mics.len() {
        let mut bins = Vec::new();
        for bin in 0..freqs.len() {
            let a = 0.15 * bin as f64;
            let ph2 = a + 0.8;
            bins.push(json!([
                [a.cos(), a.sin()],
                [0.6 * ph2.cos(), 0.6 * ph2.sin()],
            ]));
        }
        h.push(Value::Array(bins));
    }
    let mut run = json!({
        "taps": 64,
        "delay_samples": 16,
        "nfft": NFFT,
    });
    if safety {
        run["safety"] = json!({ "enabled": true });
    }
    if let Some(band_snr_db) = quality {
        run["quality"] = json!({
            "enabled": true,
            "min_snr_db": 10.0,
            "band_snr_db": band_snr_db,
        });
    }
    json!({
        "schema": "khaen-single-point-scene-input/v1",
        "scene": {
            "scene_id": "cli-scene",
            "fixture_dir": "synthetic",
            "ground_truth_sha256": "0".repeat(64),
            "seed": 7,
            "rir_scale": 0.5,
            "sample_rate_hz": 48000.0,
            "freqs_hz": freqs,
            "source_ids": sources,
            "mic_ids": mics,
            "design_mics": [0, 1, 2],
            "holdout_mics": [3, 4],
            "h": h,
            "band_centers_hz": [40.0, 50.0, 63.0, 80.0, 100.0, 125.0],
            "extended_centers_hz": [25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0],
        },
        "run": run,
    })
}

fn temp_dir(tag: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let number = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "khaen-single-point-cli-{}-{}-{}",
        std::process::id(),
        tag,
        number
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn write_scene(dir: &Path, scene: &Value) -> PathBuf {
    let path = dir.join("scene.json");
    std::fs::write(&path, serde_json::to_string_pretty(scene).unwrap()).expect("write scene");
    path
}

fn run_cli(scene: &Path, out: &Path) -> Output {
    Command::new(BIN)
        .arg("--scene")
        .arg(scene)
        .arg("--out")
        .arg(out)
        .output()
        .expect("run CLI")
}

fn stdout_json(output: &Output) -> Value {
    let text = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(text.trim()).expect("stdout must be one JSON line")
}

fn stderr_json(output: &Output) -> Value {
    let text = String::from_utf8_lossy(&output.stderr);
    serde_json::from_str(text.trim()).expect("stderr must be one JSON line")
}

#[test]
fn valid_scene_produces_verifiable_artifacts() {
    let root = temp_dir("valid");
    let scene = write_scene(&root, &scene_json(true, None));
    let out = root.join("out");
    let output = run_cli(&scene, &out);
    let code = output.status.code().expect("exit code");
    assert!(matches!(code, 0 | 3 | 4), "unexpected exit code {code}");
    let result = stdout_json(&output);
    assert_eq!(
        result["schema"],
        json!("khaen-single-point-scene-result/v1")
    );
    let outcome = result["outcome"].as_str().expect("outcome");
    assert!(matches!(outcome, "FULL" | "PARTIAL" | "NONE"));
    assert_eq!(
        result["measured"],
        json!(false),
        "predicted must never be reported as measured"
    );
    assert_eq!(result["applied"], json!(false));
    let bundle_sha = result["bundle_sha256"].as_str().expect("bundle sha");
    assert_eq!(bundle_sha.len(), 64);

    // manifest 可复核：逐文件 sha 与整包公式（路径排序 + "path\tsha\n"）。
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(out.join("manifest.json")).expect("manifest"),
    )
    .expect("manifest json");
    assert_eq!(
        manifest["schema"],
        json!("khaen-single-point-scene-manifest/v1")
    );
    assert_eq!(manifest["bundle_sha256"], json!(bundle_sha));
    let files = manifest["files"].as_array().expect("files");
    assert!(files.len() >= 3, "report/metrics/fir expected: {files:?}");
    let mut digest_input = String::new();
    for file in files {
        let path = file["path"].as_str().expect("path");
        let bytes = std::fs::read(out.join(path)).expect("artifact on disk");
        assert_eq!(
            khaen_calibration::job::sha256_hex(&bytes),
            file["sha256"].as_str().expect("sha")
        );
        assert_eq!(bytes.len(), file["bytes"].as_u64().unwrap() as usize);
        digest_input.push_str(path);
        digest_input.push('\t');
        digest_input.push_str(file["sha256"].as_str().unwrap());
        digest_input.push('\n');
    }
    assert_eq!(
        khaen_calibration::job::sha256_hex(digest_input.as_bytes()),
        bundle_sha
    );
    // report/summary 都是合法 JSON，且 summary 与 stdout 的 outcome 一致。
    let report: Value = serde_json::from_slice(
        &std::fs::read(out.join("single-point/scene-report.json")).expect("report"),
    )
    .expect("report json");
    assert_eq!(report["outcome"], json!(outcome));
    let summary: Value =
        serde_json::from_slice(&std::fs::read(out.join("summary.json")).expect("summary"))
            .expect("summary json");
    assert_eq!(summary["outcome"], json!(outcome));
    assert_eq!(summary["measured"], json!(false));
}

#[test]
fn same_input_same_artifacts() {
    let root = temp_dir("deterministic");
    let scene = write_scene(&root, &scene_json(true, None));
    let first = root.join("first");
    let second = root.join("second");
    assert!(run_cli(&scene, &first).status.success() || first.join("manifest.json").is_file());
    assert!(run_cli(&scene, &second).status.success() || second.join("manifest.json").is_file());
    let first_manifest = std::fs::read(first.join("manifest.json")).expect("first manifest");
    let second_manifest = std::fs::read(second.join("manifest.json")).expect("second manifest");
    assert_eq!(
        first_manifest, second_manifest,
        "manifest must be byte-identical for identical input"
    );
    let first_report = std::fs::read(first.join("single-point/scene-report.json")).expect("report");
    let second_report =
        std::fs::read(second.join("single-point/scene-report.json")).expect("report");
    assert_eq!(first_report, second_report);
}

#[test]
fn quality_unknown_bands_are_reported() {
    let root = temp_dir("quality");
    let mut bands = vec![json!(30.0); 31];
    bands[0] = Value::Null;
    bands[1] = Value::Null;
    bands[2] = Value::Null;
    let scene = write_scene(&root, &scene_json(true, Some(bands)));
    let out = root.join("out");
    let output = run_cli(&scene, &out);
    assert!(matches!(output.status.code(), Some(0 | 3 | 4)));
    let result = stdout_json(&output);
    assert_eq!(result["excluded_bands"], json!(3));
    let summary: Value =
        serde_json::from_slice(&std::fs::read(out.join("summary.json")).expect("summary"))
            .expect("summary json");
    assert_eq!(summary["excluded_bands"], json!(3));
}

#[test]
fn bad_input_is_rejected_with_stable_code() {
    let root = temp_dir("bad");
    // 缺 scene 字段。
    let mut scene = scene_json(true, None);
    scene.as_object_mut().unwrap().remove("scene");
    let path = write_scene(&root, &scene);
    let output = run_cli(&path, &root.join("out"));
    assert_eq!(output.status.code(), Some(2));
    let error = stderr_json(&output);
    assert_eq!(error["ok"], json!(false));
    assert_eq!(error["error"], json!("E_INPUT"));
    assert!(error["message"]
        .as_str()
        .is_some_and(|message| message.contains("scene")));
}

#[test]
fn non_empty_output_dir_is_rejected() {
    let root = temp_dir("non-empty");
    let scene = write_scene(&root, &scene_json(true, None));
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("keep.txt"), b"existing").unwrap();
    let output = run_cli(&scene, &out);
    assert_eq!(output.status.code(), Some(1));
    let error = stderr_json(&output);
    assert_eq!(error["error"], json!("E_WRITE"));
    assert!(error["message"]
        .as_str()
        .is_some_and(|message| message.contains("must be empty")));
}

#[test]
fn missing_arguments_are_usage_errors() {
    let output = Command::new(BIN).output().expect("run CLI");
    assert_eq!(output.status.code(), Some(2));
    let error = stderr_json(&output);
    assert_eq!(error["error"], json!("E_USAGE"));
}
