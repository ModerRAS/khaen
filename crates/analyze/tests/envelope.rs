//! envelope 子命令：短时 RMS 包络、掉音检测位置/深度、阈值与最少窗数、错误码。

mod common;

use std::path::Path;

use common::*;

fn envelope_args(wav: &Path, out: &Path, extra: &[&str]) -> Vec<String> {
    let mut args = vec![
        "envelope".to_string(),
        "--wav".to_string(),
        wav.display().to_string(),
        "--out".to_string(),
        out.display().to_string(),
    ];
    args.extend(extra.iter().map(|arg| arg.to_string()));
    args
}

fn run_envelope(wav: &Path, out: &Path, extra: &[&str]) -> Run {
    run_args(&envelope_args(wav, out, extra))
}

const FRAMES: usize = FS as usize * 2;
const GAP_START: usize = 38_400; // 0.8 s
const GAP_END: usize = 48_000; // 1.0 s

#[test]
fn clean_sine_has_no_dropout() {
    let dir = temp_dir("envelope-clean");
    let wav = dir.join("tone.wav");
    write_float32(&wav, FS, 1, &sine(1_000.0, 0.5, FRAMES, FS));
    let out = dir.join("envelope.json");
    let result = run_envelope(&wav, &out, &[]);
    assert_eq!(result.code, 0, "{}", result.stderr);
    let document = read_json(&out);
    assert_eq!(document["schema"], "khaen-analyze-envelope/v1");
    assert_eq!(document["verdict"], "clean");
    assert_eq!(document["gap_count"], 0);
    assert_eq!(document["window_samples"], 2400);
    assert_eq!(document["hop_samples"], 1200);
    assert_eq!(document["windows"], (FRAMES - 2400) / 1200 + 1);
    let median = document["reference_median_dbfs"].as_f64().expect("median");
    assert!((median - -9.0309).abs() < 0.02, "median {median}");
    let levels = document["levels_dbfs"].as_array().expect("levels");
    for level in levels {
        let level = level.as_f64().expect("finite level");
        assert!((level - -9.0309).abs() < 0.05, "level {level}");
    }
}

#[test]
fn detects_dropout_gap_position_and_depth() {
    let dir = temp_dir("envelope-dropout");
    let wav = dir.join("tone.wav");
    // 0.8-1.0 s 掉到约 -69 dBFS（模拟真实掉音的本底噪声，而不是数字零）。
    let samples = sine_with_block(1_000.0, 0.5, FRAMES, FS, (GAP_START, GAP_END, 0.0005));
    write_float32(&wav, FS, 1, &samples);
    let out = dir.join("envelope.json");
    let result = run_envelope(&wav, &out, &[]);
    assert_eq!(result.code, 0, "{}", result.stderr);
    let document = read_json(&out);
    assert_eq!(document["verdict"], "dropout");
    assert_eq!(document["gap_count"], 1);
    let gaps = document["gaps"].as_array().expect("gaps");
    assert_eq!(gaps.len(), 1);
    let gap = &gaps[0];
    let start = gap["start_s"].as_f64().expect("start");
    let end = gap["end_s"].as_f64().expect("end");
    // 只有完全落在掉音段内的窗才会被判掉音：起止向内收缩，最多各缩一个步长/窗长。
    assert!((0.8 - 1e-9..=0.8 + 0.026).contains(&start), "start {start}");
    assert!((1.0 - 0.051..=1.0 + 1e-9).contains(&end), "end {end}");
    assert_eq!(gap["windows"], 7);
    let depth = gap["depth_db"].as_f64().expect("depth");
    assert!(depth < -50.0, "depth {depth}");
    let max_gap = document["max_gap_s"].as_f64().expect("max gap");
    assert!((max_gap - (end - start)).abs() < 1e-12);
    // summary 一行也给出判定与 gap 数。
    let summary = result.summary();
    assert_eq!(summary["verdict"], "dropout");
    assert_eq!(summary["gaps"], 1);
}

#[test]
fn requires_min_windows_of_contiguous_dropout() {
    let dir = temp_dir("envelope-min");
    let wav = dir.join("tone.wav");
    let samples = sine_with_block(1_000.0, 0.5, FRAMES, FS, (GAP_START, GAP_END, 0.0005));
    write_float32(&wav, FS, 1, &samples);
    let out = dir.join("envelope.json");
    let result = run_envelope(&wav, &out, &["--min-windows", "10"]);
    assert_eq!(result.code, 0, "{}", result.stderr);
    let document = read_json(&out);
    assert_eq!(document["verdict"], "clean");
    assert_eq!(document["gap_count"], 0);
}

#[test]
fn dropout_threshold_is_relative_to_median() {
    let dir = temp_dir("envelope-threshold");
    let wav = dir.join("tone.wav");
    // 块电平比主电平低 10 dB：默认 -20 dB 阈值不算掉音；-9 dB 阈值算。
    let block_amplitude = 0.5 * 10f64.powf(-10.0 / 20.0);
    let samples = sine_with_block(
        1_000.0,
        0.5,
        FRAMES,
        FS,
        (GAP_START, GAP_END, block_amplitude),
    );
    write_float32(&wav, FS, 1, &samples);
    let default_out = dir.join("default.json");
    let result = run_envelope(&wav, &default_out, &[]);
    assert_eq!(result.code, 0, "{}", result.stderr);
    assert_eq!(read_json(&default_out)["verdict"], "clean");
    let strict_out = dir.join("strict.json");
    let result = run_envelope(&wav, &strict_out, &["--dropout-db", "-9"]);
    assert_eq!(result.code, 0, "{}", result.stderr);
    assert_eq!(read_json(&strict_out)["verdict"], "dropout");
}

#[test]
fn all_silent_verdict_is_silent() {
    let dir = temp_dir("envelope-silent");
    let wav = dir.join("silence.wav");
    write_float32(&wav, FS, 1, &vec![0.0; FS as usize]);
    let out = dir.join("envelope.json");
    let result = run_envelope(&wav, &out, &[]);
    assert_eq!(result.code, 0, "{}", result.stderr);
    let document = read_json(&out);
    assert_eq!(document["verdict"], "silent");
    assert_eq!(document["gap_count"], 0);
    assert!(document["reference_median_dbfs"].is_null());
    assert!(document["signal_start_s"].is_null());
    assert!(document["signal_end_s"].is_null());
    for level in document["levels_dbfs"].as_array().expect("levels") {
        assert!(level.is_null());
    }
}

/// 仿 2026-11-26 真机闭环录音：9 s = 1.5 s 静音 + 5 s 指数扫频（-12 dBFS）+ 1.5 s 静音。
fn closed_loop_style_recording(dropout: Option<(usize, usize)>) -> Vec<f64> {
    let sample_rate = f64::from(FS);
    let frames = FS as usize * 9;
    let sweep_start = FS as usize * 3 / 2;
    let sweep_end = sweep_start + FS as usize * 5;
    let mut samples = vec![0.0f64; frames];
    let duration = 5.0;
    let (f0, f1) = (20.0f64, 20_000.0f64);
    let k = (f1 / f0).ln() / duration;
    for (index, sample) in samples
        .iter_mut()
        .enumerate()
        .take(sweep_end)
        .skip(sweep_start)
    {
        let t = (index - sweep_start) as f64 / sample_rate;
        let phase = 2.0 * std::f64::consts::PI * f0 * ((k * t).exp() - 1.0) / k;
        *sample = 0.25 * phase.sin();
    }
    if let Some((start, end)) = dropout {
        for (index, sample) in samples.iter_mut().enumerate().take(end).skip(start) {
            *sample = 0.0005 * (index as f64 * 0.7).sin();
        }
    }
    samples
}

#[test]
fn closed_loop_style_recording_with_gap_is_flagged() {
    // 2026-11-26 首次基线录音 4.3-4.9 s 掉音；重录前 30 带全频 FAIL，最大 delta 30.7 dB。
    let dir = temp_dir("envelope-incident");
    let wav = dir.join("baseline.wav");
    let samples = closed_loop_style_recording(Some((FS as usize * 43 / 10, FS as usize * 49 / 10)));
    write_float32(&wav, FS, 1, &samples);
    let out = dir.join("envelope.json");
    let result = run_envelope(&wav, &out, &[]);
    assert_eq!(result.code, 0, "{}", result.stderr);
    let document = read_json(&out);
    assert_eq!(document["verdict"], "dropout");
    let gap = &document["gaps"].as_array().expect("gaps")[0];
    let start = gap["start_s"].as_f64().expect("start");
    let end = gap["end_s"].as_f64().expect("end");
    assert!((start - 4.3).abs() < 0.03, "start {start}");
    assert!((end - 4.9).abs() < 0.03, "end {end}");
    let depth = gap["depth_db"].as_f64().expect("depth");
    assert!(depth < -20.0, "depth {depth}");
}

#[test]
fn closed_loop_style_recording_without_gap_is_clean() {
    // 1.5 s 静音 + 5 s 扫频 + 1.5 s 静音：静音占比 1/3，中位参考仍应落在扫频电平上。
    let dir = temp_dir("envelope-incident-clean");
    let wav = dir.join("baseline.wav");
    write_float32(&wav, FS, 1, &closed_loop_style_recording(None));
    let out = dir.join("envelope.json");
    let result = run_envelope(&wav, &out, &[]);
    assert_eq!(result.code, 0, "{}", result.stderr);
    let document = read_json(&out);
    assert_eq!(document["verdict"], "clean");
    assert_eq!(document["gap_count"], 0);
    let median = document["reference_median_dbfs"].as_f64().expect("median");
    assert!(
        (median - -15.0515).abs() < 0.2,
        "median {median} (sweep RMS -15.05 dBFS)"
    );
    // 头尾静音算边界信号区：1.5 s 静音之后的第一个窗覆盖到扫频起点。
    let signal_start = document["signal_start_s"].as_f64().expect("signal start");
    let signal_end = document["signal_end_s"].as_f64().expect("signal end");
    assert!(
        (signal_start - 1.475).abs() < 0.03,
        "signal start {signal_start}"
    );
    assert!((signal_end - 6.525).abs() < 0.03, "signal end {signal_end}");
}

#[test]
fn invalid_configs_report_stable_codes() {
    let dir = temp_dir("envelope-invalid");
    let wav = dir.join("tone.wav");
    write_float32(&wav, FS, 1, &sine(1_000.0, 0.5, FRAMES, FS));
    let cases: Vec<(Vec<&str>, &str)> = vec![
        (vec!["--window-ms", "0"], "E_ANALYZE_CONFIG"),
        (vec!["--dropout-db", "3"], "E_ANALYZE_CONFIG"),
        (vec!["--min-windows", "0"], "E_ANALYZE_CONFIG"),
        (vec!["--window-ms", "5000"], "E_ANALYZE_INPUT"),
        (vec!["--window-ms", "abc"], "E_USAGE"),
    ];
    for (index, (extra, code)) in cases.iter().enumerate() {
        let out = dir.join(format!("invalid{index}.json"));
        let result = run_envelope(&wav, &out, extra);
        assert_eq!(result.code, 2, "case {index}: {}", result.stderr);
        assert!(
            result.stderr.contains(code),
            "case {index}: expected {code}, got {}",
            result.stderr
        );
        assert!(!out.exists(), "case {index} must not write output");
    }
}
