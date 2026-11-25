//! 离线验收链路：mimo 场景 -> FIR CSV -> 方案 -> 预测电平 -> 两段 WAV -> 证据包。
//!
//! 这是一条**合成数据**的端到端演练（SIMULATED）：证明六个命令行工具能按现场顺序串起来、
//! 中间产物可复现、证据包清单自洽。它不证明真机测量/下发/复测（那一步 NOT_RUN）。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::{json, Value};

const SINGLE_POINT: &str = env!("CARGO_BIN_EXE_khaen-single-point");
const FIR_PLAN: &str = env!("CARGO_BIN_EXE_khaen-fir-plan");
const PLAN_LEVELS: &str = env!("CARGO_BIN_EXE_khaen-plan-levels");
const BAND_LEVELS: &str = env!("CARGO_BIN_EXE_khaen-band-levels");
const CLOSED_LOOP_RUN: &str = env!("CARGO_BIN_EXE_khaen-closed-loop-run");

const NFFT: usize = 8192;
const FS: u32 = 48_000;
const LEN: usize = 24_000;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "khaen-offline-chain-{}-{}-{}",
        std::process::id(),
        tag,
        unique
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn scene_json() -> Value {
    let freqs: Vec<f64> = [7usize, 9, 11, 13, 17, 21]
        .iter()
        .map(|bin| *bin as f64 * f64::from(FS) / NFFT as f64)
        .collect();
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
    json!({
        "schema": "khaen-single-point-scene-input/v1",
        "scene": {
            "scene_id": "offline-chain",
            "fixture_dir": "synthetic",
            "ground_truth_sha256": "0".repeat(64),
            "seed": 7,
            "rir_scale": 0.5,
            "sample_rate_hz": f64::from(FS),
            "freqs_hz": freqs,
            "source_ids": ["SUB1", "SUB2"],
            "mic_ids": mics,
            "design_mics": [0, 1, 2],
            "holdout_mics": [3, 4],
            "h": h,
            "band_centers_hz": [40.0, 50.0, 63.0, 80.0, 100.0, 125.0],
            "extended_centers_hz": [25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0],
        },
        "run": { "taps": 64, "delay_samples": 16, "nfft": NFFT, "safety": { "enabled": true } },
    })
}

fn write_scene(dir: &Path) -> PathBuf {
    let path = dir.join("scene.json");
    std::fs::write(&path, serde_json::to_string_pretty(&scene_json()).unwrap()).expect("scene");
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

fn collect_csv(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect_csv(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("csv") {
            out.push(path);
        }
    }
}

fn write_wav(path: &Path, samples: &[f64]) {
    let data_len = samples.len() * 2;
    let mut bytes = Vec::with_capacity(44 + data_len);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&FS.to_le_bytes());
    bytes.extend_from_slice(&(FS * 2).to_le_bytes());
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

fn tone_bed() -> Vec<f64> {
    (0..LEN)
        .map(|index| {
            let t = index as f64 / f64::from(FS);
            0.05 * ((2.0 * std::f64::consts::PI * 40.0 * t).sin()
                + (2.0 * std::f64::consts::PI * 63.0 * t).sin()
                + (2.0 * std::f64::consts::PI * 100.0 * t).sin())
        })
        .collect()
}

#[test]
fn synthetic_chain_runs_end_to_end_and_is_reproducible() {
    let root = temp_dir("chain");

    // 1) mimo 场景 -> 报告/指标/FIR CSV。
    let scene = write_scene(&root);
    let stage = root.join("stage");
    let single = run(
        SINGLE_POINT,
        &[
            "--scene",
            scene.to_str().unwrap(),
            "--out",
            stage.to_str().unwrap(),
        ],
    );
    assert!(
        matches!(single.code, 0 | 3 | 4),
        "single-point exit {}: {}",
        single.code,
        single.stderr
    );
    assert!(
        single.stdout.contains("khaen-single-point-scene-result/v1"),
        "unexpected single-point stdout: {}",
        single.stdout
    );

    // 2) FIR CSV -> 方案文件。
    let mut firs = Vec::new();
    collect_csv(&stage.join("single-point").join("fir"), &mut firs);
    firs.sort();
    assert!(!firs.is_empty(), "scene produced no FIR CSVs");
    let plan = root.join("plan.bin");
    let mut fir_args: Vec<String> = Vec::new();
    for path in &firs {
        fir_args.push("--fir".to_string());
        fir_args.push(path.to_str().unwrap().to_string());
    }
    fir_args.push("--out".to_string());
    fir_args.push(plan.to_str().unwrap().to_string());
    fir_args.push("--sample-rate".to_string());
    fir_args.push(FS.to_string());
    fir_args.push("--channels".to_string());
    fir_args.push(firs.len().to_string());
    let fir_plan = run(
        FIR_PLAN,
        &fir_args.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    assert_eq!(fir_plan.code, 0, "fir-plan: {}", fir_plan.stderr);
    assert!(
        fir_plan.stdout.contains("\"sha256\":\""),
        "{}",
        fir_plan.stdout
    );
    let plan_bytes = std::fs::read(&plan).expect("plan bytes");
    assert!(plan_bytes.starts_with(b"KHAENPLN"));

    // 3) 方案 -> 预测电平（六个四分之一工具之一）。
    let predicted = root.join("predicted.json");
    let levels = run(
        PLAN_LEVELS,
        &[
            "--plan",
            plan.to_str().unwrap(),
            "--out",
            predicted.to_str().unwrap(),
        ],
    );
    assert_eq!(levels.code, 0, "plan-levels: {}", levels.stderr);
    let predicted_json: Value =
        serde_json::from_slice(&std::fs::read(&predicted).expect("predicted")).expect("json");
    assert_eq!(predicted_json["schema"], "khaen-closed-loop-levels/v1");
    assert_eq!(
        predicted_json["points"].as_array().expect("points").len(),
        firs.len()
    );
    assert_eq!(
        predicted_json["points"][0]["bands"]
            .as_array()
            .expect("bands")
            .len(),
        30
    );

    // 4) 两段 WAV -> 实测相对电平（合成数据：相同信号 => 0 dB）。
    let baseline_wav = root.join("baseline.wav");
    let applied_wav = root.join("applied.wav");
    let bed = tone_bed();
    write_wav(&baseline_wav, &bed);
    write_wav(&applied_wav, &bed);
    let observed = root.join("observed.json");
    let band = run(
        BAND_LEVELS,
        &[
            "--measured",
            applied_wav.to_str().unwrap(),
            "--reference",
            baseline_wav.to_str().unwrap(),
            "--out",
            observed.to_str().unwrap(),
            "--point",
            "P01",
            "--centers",
            "40,63,100",
        ],
    );
    assert_eq!(band.code, 0, "band-levels: {}", band.stderr);

    // 5) 方案 + 两段 WAV -> 四文件证据包；同输入两次逐字节相同。
    let out1 = root.join("bundle1");
    let first = run(
        CLOSED_LOOP_RUN,
        &[
            "--plan",
            plan.to_str().unwrap(),
            "--baseline",
            baseline_wav.to_str().unwrap(),
            "--applied",
            applied_wav.to_str().unwrap(),
            "--out",
            out1.to_str().unwrap(),
            "--point",
            "P01",
            "--lane",
            "0",
            "--centers",
            "40,63,100",
        ],
    );
    assert!(
        matches!(first.code, 0 | 3 | 4),
        "closed-loop-run exit {}: {}",
        first.code,
        first.stderr
    );
    for name in [
        "predicted.json",
        "observed.json",
        "report.json",
        "manifest.json",
    ] {
        assert!(out1.join(name).is_file(), "missing {name}");
    }
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(out1.join("manifest.json")).expect("manifest"))
            .expect("manifest json");
    assert_eq!(manifest["schema"], "khaen-closed-loop-run-manifest/v1");
    assert_eq!(manifest["tool"], "khaen-closed-loop-run");
    assert_eq!(manifest["centers_hz"].as_array().expect("centers").len(), 3);

    let out2 = root.join("bundle2");
    let second = run(
        CLOSED_LOOP_RUN,
        &[
            "--plan",
            plan.to_str().unwrap(),
            "--baseline",
            baseline_wav.to_str().unwrap(),
            "--applied",
            applied_wav.to_str().unwrap(),
            "--out",
            out2.to_str().unwrap(),
            "--point",
            "P01",
            "--lane",
            "0",
            "--centers",
            "40,63,100",
        ],
    );
    assert_eq!(second.code, first.code);
    for name in [
        "predicted.json",
        "observed.json",
        "report.json",
        "manifest.json",
    ] {
        assert_eq!(
            std::fs::read(out1.join(name)).expect("first"),
            std::fs::read(out2.join(name)).expect("second"),
            "{name} differs between runs"
        );
    }

    // 6) 方案编译本身可复现：同 FIR 再编译一次字节/摘要相同。
    let plan2 = root.join("plan2.bin");
    let mut fir_args2 = fir_args.clone();
    let out_index = fir_args2
        .iter()
        .position(|arg| arg == "--out")
        .expect("--out");
    fir_args2[out_index + 1] = plan2.to_str().unwrap().to_string();
    let rebuilt = run(
        FIR_PLAN,
        &fir_args2.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    assert_eq!(rebuilt.code, 0, "{}", rebuilt.stderr);
    assert_eq!(
        std::fs::read(&plan).expect("plan"),
        std::fs::read(&plan2).expect("plan2")
    );

    let _ = std::fs::remove_dir_all(&root);
}
