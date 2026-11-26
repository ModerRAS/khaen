//! compare 子命令：相对频带电平与 delta，与 khaen-band-levels / calibration 逐位一致。

mod common;

use std::path::Path;

use common::*;

fn compare_args(reference: &Path, measured: &Path, out: &Path, extra: &[&str]) -> Vec<String> {
    let mut args = vec![
        "compare".to_string(),
        "--reference".to_string(),
        reference.display().to_string(),
        "--measured".to_string(),
        measured.display().to_string(),
        "--out".to_string(),
        out.display().to_string(),
    ];
    args.extend(extra.iter().map(|arg| arg.to_string()));
    args
}

fn run_compare(reference: &Path, measured: &Path, out: &Path, extra: &[&str]) -> Run {
    run_args(&compare_args(reference, measured, out, extra))
}

#[test]
fn halved_amplitude_reports_minus_6_db() {
    let dir = temp_dir("compare-gain");
    let reference = dir.join("reference.wav");
    let measured = dir.join("measured.wav");
    let frames = FS as usize * 2;
    write_float32(&reference, FS, 1, &sine(1_000.0, 0.5, frames, FS));
    write_float32(&measured, FS, 1, &sine(1_000.0, 0.25, frames, FS));
    let out = dir.join("compare.json");
    let result = run_compare(
        &reference,
        &measured,
        &out,
        &["--centers", "100,1000,10000"],
    );
    assert_eq!(result.code, 0, "{}", result.stderr);
    let document = read_json(&out);
    assert_eq!(document["schema"], "khaen-analyze-compare/v1");
    let delta = band_field(&document, 1_000.0, "delta_db").expect("1 kHz delta");
    assert!((delta - -6.0206).abs() < 0.01, "delta {delta}");
    // 两侧绝对电平（dBFS）也在同一份输出里。
    let reference_db = band_field(&document, 1_000.0, "reference_dbfs").expect("reference");
    let measured_db = band_field(&document, 1_000.0, "measured_dbfs").expect("measured");
    assert!(
        (reference_db - -9.0309).abs() < 0.02,
        "reference {reference_db}"
    );
    assert!(
        (measured_db - -15.0515).abs() < 0.02,
        "measured {measured_db}"
    );
    // 两侧都有带功率的带才可比较；不可比较的带必须计入 missing（缺数据不算通过）。
    let compared = document["compared_bands"].as_u64().expect("compared");
    let missing = document["missing_bands"].as_u64().expect("missing");
    assert_eq!(compared + missing, 3);
}

#[test]
fn delta_matches_calibration_bitwise() {
    let dir = temp_dir("compare-calibration");
    let reference_path = dir.join("reference.wav");
    let measured_path = dir.join("measured.wav");
    let frames = FS as usize * 2;
    write_pcm16(&reference_path, FS, 1, &sine(1_000.0, 0.5, frames, FS));
    write_pcm16(&measured_path, FS, 1, &sine(1_000.0, 0.25, frames, FS));
    let centers = [100.0f64, 500.0, 1_000.0, 2_000.0, 10_000.0];
    let out = dir.join("compare.json");
    let center_list = centers
        .iter()
        .map(|center| center.to_string())
        .collect::<Vec<String>>()
        .join(",");
    let result = run_compare(
        &reference_path,
        &measured_path,
        &out,
        &["--centers", &center_list],
    );
    assert_eq!(result.code, 0, "{}", result.stderr);
    let reference = khaen_analyze::wav::read_wav_file(&reference_path).expect("reference");
    let measured = khaen_analyze::wav::read_wav_file(&measured_path).expect("measured");
    let expected = khaen_calibration::relative_band_levels(
        &measured.samples,
        &reference.samples,
        reference.sample_rate_hz,
        &centers,
        &khaen_calibration::BandLevelsConfig::default(),
    )
    .expect("relative levels");
    let document = read_json(&out);
    for (band, expected) in expected.iter().zip(centers.iter()) {
        assert_eq!(
            band_field(&document, *expected, "delta_db"),
            band.level_db,
            "band {} Hz",
            expected
        );
    }
}

#[test]
fn identical_recordings_have_zero_delta() {
    let dir = temp_dir("compare-identical");
    let path = dir.join("tone.wav");
    write_float32(&path, FS, 1, &sine(1_000.0, 0.5, FS as usize, FS));
    let out = dir.join("compare.json");
    let result = run_compare(&path, &path, &out, &["--centers", "100,1000,10000"]);
    assert_eq!(result.code, 0, "{}", result.stderr);
    let document = read_json(&out);
    assert_eq!(band_field(&document, 1_000.0, "delta_db"), Some(0.0));
    assert_eq!(document["max_abs_delta_db"].as_f64(), Some(0.0));
    assert_eq!(document["rms_delta_db"].as_f64(), Some(0.0));
}

#[test]
fn digitally_silent_side_reports_missing_bands() {
    let dir = temp_dir("compare-silent");
    let reference = dir.join("silence.wav");
    let measured = dir.join("tone.wav");
    write_float32(&reference, FS, 1, &vec![0.0; FS as usize]);
    write_float32(&measured, FS, 1, &sine(1_000.0, 0.5, FS as usize, FS));
    let out = dir.join("compare.json");
    let result = run_compare(
        &reference,
        &measured,
        &out,
        &["--centers", "100,1000,10000"],
    );
    assert_eq!(result.code, 0, "{}", result.stderr);
    let document = read_json(&out);
    // 参考侧全零 -> 带功率精确为 0 -> delta 必须 null（缺数据，绝不算通过）。
    assert_eq!(document["compared_bands"].as_u64(), Some(0));
    assert_eq!(document["missing_bands"].as_u64(), Some(3));
    assert!(band_field(&document, 1_000.0, "delta_db").is_none());
    assert!(band_field(&document, 1_000.0, "reference_dbfs").is_none());
    assert_eq!(document["max_abs_delta_db"], serde_json::Value::Null);
}

#[test]
fn sample_rate_mismatch_is_alignment_error() {
    let dir = temp_dir("compare-rate");
    let reference = dir.join("reference.wav");
    let measured = dir.join("measured.wav");
    write_float32(&reference, FS, 1, &sine(1_000.0, 0.5, 24_000, FS));
    write_float32(&measured, 96_000, 1, &sine(1_000.0, 0.5, 48_000, 96_000));
    let out = dir.join("compare.json");
    let result = run_compare(&reference, &measured, &out, &["--centers", "1000"]);
    assert_eq!(result.code, 2, "{}", result.stderr);
    assert!(
        result.stderr.contains("E_ANALYZE_ALIGNMENT"),
        "{}",
        result.stderr
    );
}
