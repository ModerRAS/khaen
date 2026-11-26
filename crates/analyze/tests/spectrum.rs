//! spectrum 子命令：1/3 oct 带电平（dBFS）、PSD、参数校验、与 calibration 同口径。

mod common;

use std::path::Path;

use common::*;

fn spectrum_args(wav: &Path, out: &Path, extra: &[&str]) -> Vec<String> {
    let mut args = vec![
        "spectrum".to_string(),
        "--wav".to_string(),
        wav.display().to_string(),
        "--out".to_string(),
        out.display().to_string(),
    ];
    args.extend(extra.iter().map(|arg| arg.to_string()));
    args
}

fn run_spectrum(wav: &Path, out: &Path, extra: &[&str]) -> Run {
    run_args(&spectrum_args(wav, out, extra))
}

#[test]
fn sine_band_level_matches_rms_dbfs() {
    let dir = temp_dir("spectrum-sine");
    let wav = dir.join("sine.wav");
    write_float32(&wav, FS, 1, &sine(1_000.0, 0.5, FS as usize * 2, FS));
    let out = dir.join("spectrum.json");
    let result = run_spectrum(&wav, &out, &["--centers", "100,1000,10000"]);
    assert_eq!(result.code, 0, "{}", result.stderr);
    let document = read_json(&out);
    let level = band_field(&document, 1_000.0, "level_dbfs").expect("1 kHz band");
    assert!((level - -9.0309).abs() < 0.02, "level {level}");
    // 只有 1 kHz 带有真实信号；其余带是 FFT 舍入的数值底噪（约 -130 dBFS 级），
    // 口径与 khaen-band-levels 一致：带功率 > 0 就如实报，不编造、也不假装 null。
    for center_hz in [100.0, 10_000.0] {
        let floor = band_field(&document, center_hz, "level_dbfs");
        assert!(
            floor.is_none_or(|level| level < -100.0),
            "{center_hz} Hz floor {floor:?}"
        );
    }
    assert_eq!(document["schema"], "khaen-analyze-spectrum/v1");
    assert_eq!(document["level_unit"], "dBFS");
    assert_eq!(document["window_len"], 4096);
    assert_eq!(document["hop"], 2048);
    assert_eq!(document["segments"], (96_000 - 4096) / 2048 + 1);
}

#[test]
fn full_scale_sine_reads_minus_3_dbfs() {
    let dir = temp_dir("spectrum-fullscale");
    let wav = dir.join("full.wav");
    write_float32(&wav, FS, 1, &sine(1_000.0, 1.0, FS as usize, FS));
    let out = dir.join("spectrum.json");
    let result = run_spectrum(&wav, &out, &["--centers", "1000"]);
    assert_eq!(result.code, 0, "{}", result.stderr);
    let document = read_json(&out);
    let level = band_field(&document, 1_000.0, "level_dbfs").expect("1 kHz band");
    assert!((level - -3.0103).abs() < 0.02, "level {level}");
}

#[test]
fn two_tones_keep_relative_level() {
    let dir = temp_dir("spectrum-tones");
    let frames = FS as usize * 2;
    let low = sine(500.0, 0.25, frames, FS);
    let high = sine(4_000.0, 0.5, frames, FS);
    let mixed: Vec<f64> = low.iter().zip(high.iter()).map(|(a, b)| a + b).collect();
    let wav = dir.join("tones.wav");
    write_float32(&wav, FS, 1, &mixed);
    let out = dir.join("spectrum.json");
    let result = run_spectrum(&wav, &out, &["--centers", "500,4000"]);
    assert_eq!(result.code, 0, "{}", result.stderr);
    let document = read_json(&out);
    let low = band_field(&document, 500.0, "level_dbfs").expect("500 Hz");
    let high = band_field(&document, 4_000.0, "level_dbfs").expect("4 kHz");
    assert!((high - low - 6.0206).abs() < 0.03, "low {low} high {high}");
}

#[test]
fn pcm16_and_float32_agree() {
    let dir = temp_dir("spectrum-formats");
    let frames = FS as usize * 2;
    let pcm = dir.join("pcm16.wav");
    let float = dir.join("float32.wav");
    write_pcm16(&pcm, FS, 1, &sine(1_000.0, 0.5, frames, FS));
    write_float32(&float, FS, 1, &sine(1_000.0, 0.5, frames, FS));
    let mut levels = Vec::new();
    for (index, wav) in [pcm, float].iter().enumerate() {
        let out = dir.join(format!("spectrum{index}.json"));
        let result = run_spectrum(wav, &out, &["--centers", "1000"]);
        assert_eq!(result.code, 0, "{}", result.stderr);
        let document = read_json(&out);
        levels.push(band_field(&document, 1_000.0, "level_dbfs").expect("band"));
    }
    assert!((levels[0] - levels[1]).abs() < 0.01, "levels {levels:?}");
}

#[test]
fn psd_output_integrates_to_band_levels() {
    let dir = temp_dir("spectrum-psd");
    let wav = dir.join("tone.wav");
    write_float32(&wav, FS, 1, &sine(1_000.0, 0.5, FS as usize, FS));
    let out = dir.join("spectrum.json");
    let result = run_spectrum(&wav, &out, &["--centers", "1000", "--psd", "1"]);
    assert_eq!(result.code, 0, "{}", result.stderr);
    let document = read_json(&out);
    let psd = &document["psd"];
    let values = psd["values"].as_array().expect("psd values");
    assert_eq!(values.len(), 4096 / 2 + 1);
    let bin_hz = psd["bin_hz"].as_f64().expect("bin_hz");
    assert!((bin_hz - 48_000.0 / 4096.0).abs() < 1e-12);
    let ratio = 2.0f64.powf(1.0 / 6.0);
    let mut integrated = 0.0f64;
    for (bin, value) in values.iter().enumerate() {
        let frequency = bin as f64 * bin_hz;
        if frequency >= 1_000.0 / ratio && frequency < 1_000.0 * ratio {
            integrated += value.as_f64().expect("psd sample") * bin_hz;
        }
    }
    let level = band_field(&document, 1_000.0, "level_dbfs").expect("band");
    let band_power = 10.0f64.powf(level / 10.0);
    assert!(
        (integrated - band_power).abs() <= band_power * 1e-9,
        "integrated {integrated} vs band {band_power}"
    );
}

#[test]
fn custom_window_is_used() {
    let dir = temp_dir("spectrum-window");
    let wav = dir.join("tone.wav");
    write_float32(&wav, FS, 1, &sine(1_000.0, 0.5, FS as usize, FS));
    let out = dir.join("spectrum.json");
    let result = run_spectrum(&wav, &out, &["--centers", "1000", "--window", "1024"]);
    assert_eq!(result.code, 0, "{}", result.stderr);
    let document = read_json(&out);
    assert_eq!(document["window_len"], 1024);
    assert_eq!(document["hop"], 512);
    let level = band_field(&document, 1_000.0, "level_dbfs").expect("band");
    assert!((level - -9.0309).abs() < 0.05, "level {level}");
}

#[test]
fn invalid_window_reports_stable_config_code() {
    let dir = temp_dir("spectrum-bad-window");
    let wav = dir.join("tone.wav");
    write_float32(&wav, FS, 1, &sine(1_000.0, 0.5, FS as usize, FS));
    for window in ["1000", "128"] {
        let out = dir.join(format!("window-{window}.json"));
        let result = run_spectrum(&wav, &out, &["--window", window]);
        assert_eq!(result.code, 2, "{}", result.stderr);
        assert!(
            result.stderr.contains("E_BAND_LEVELS_CONFIG"),
            "{}",
            result.stderr
        );
        assert!(!out.exists());
    }
}

#[test]
fn invalid_centers_report_stable_alignment_code() {
    let dir = temp_dir("spectrum-bad-centers");
    let wav = dir.join("tone.wav");
    write_float32(&wav, FS, 1, &sine(1_000.0, 0.5, FS as usize, FS));
    for centers in ["1000,100", "30000"] {
        let out = dir.join(format!("centers-{}.json", centers.replace(',', "-")));
        let result = run_spectrum(&wav, &out, &["--centers", centers]);
        assert_eq!(result.code, 2, "{}", result.stderr);
        assert!(
            result.stderr.contains("E_BAND_LEVELS_ALIGNMENT"),
            "{}",
            result.stderr
        );
    }
}

#[test]
fn recording_shorter_than_window_reports_input_code() {
    let dir = temp_dir("spectrum-short");
    let wav = dir.join("short.wav");
    write_float32(&wav, FS, 1, &sine(1_000.0, 0.5, 1_000, FS));
    let out = dir.join("spectrum.json");
    let result = run_spectrum(&wav, &out, &["--centers", "1000"]);
    assert_eq!(result.code, 2, "{}", result.stderr);
    assert!(
        result.stderr.contains("E_BAND_LEVELS_INPUT"),
        "{}",
        result.stderr
    );
}
