//! khaen-analyze band-snr 子命令验收：信号带/噪声带分离、确定性、错误路径。
//!
//! 全部离线（Windows 本机可跑），不碰设备。

mod common;

use common::{band_field, read_json, run_bin, sha256_file, temp_dir, write_pcm24, FS};

/// 确定性伪噪声（LCG），幅度 amp（峰值）。
fn pseudo_noise(frames: usize, amp: f64, seed: u32) -> Vec<f64> {
    let mut state = seed.max(1);
    (0..frames)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            ((f64::from(state) / f64::from(u32::MAX)) - 0.5) * 2.0 * amp
        })
        .collect()
}

fn tone(freq_hz: f64, amp: f64, frames: usize) -> Vec<f64> {
    (0..frames)
        .map(|index| {
            amp * (2.0 * std::f64::consts::PI * freq_hz * index as f64 / f64::from(FS)).sin()
        })
        .collect()
}

/// 1.0 s 伪噪声 + 2.0 s 1 kHz 音 + 1.0 s 伪噪声（噪声幅度约 -65 dBFS）。
fn signal_in_noise() -> Vec<f64> {
    let noise_amp = 0.001;
    let mut samples = pseudo_noise((1.0 * f64::from(FS)) as usize, noise_amp, 7);
    let start = samples.len();
    let tone = tone(1_000.0, 0.1, (2.0 * f64::from(FS)) as usize);
    let mut noise = pseudo_noise(tone.len(), noise_amp, 11);
    for (index, value) in tone.iter().enumerate() {
        noise[index] += value;
    }
    samples.extend_from_slice(&noise);
    let _ = start;
    samples.extend(pseudo_noise((1.0 * f64::from(FS)) as usize, noise_amp, 13));
    samples
}

#[test]
fn band_snr_flags_tone_band_measurable_and_quiet_band_not() {
    let dir = temp_dir("snr");
    let wav = dir.join("signal.wav");
    write_pcm24(&wav, FS, 1, &signal_in_noise());
    let out = dir.join("snr.json");
    let run = run_bin(&[
        "band-snr",
        "--wav",
        wav.to_str().expect("path"),
        "--out",
        out.to_str().expect("path"),
        "--centers",
        "100,1000,4000",
        "--min-snr-db",
        "20",
    ]);
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let summary = run.summary();
    assert_eq!(summary["command"], "band-snr");
    assert_eq!(summary["bands"], 3);
    let document = read_json(&out);
    assert_eq!(summary["min_snr_db"], 20.0);
    let snr_1k = band_field(&document, 1_000.0, "snr_db").expect("1 kHz SNR");
    assert!(snr_1k > 40.0, "1 kHz SNR should be high, got {snr_1k}");
    let measurable_1k = &document["bands"]
        .as_array()
        .expect("bands")
        .iter()
        .find(|band| (band["center_hz"].as_f64().expect("center") - 1_000.0).abs() < 1e-6)
        .expect("1 kHz band")["measurable"];
    assert_eq!(measurable_1k, &serde_json::Value::Bool(true));
    let snr_100 = band_field(&document, 100.0, "snr_db").expect("100 Hz SNR");
    assert!(
        snr_100 < 20.0,
        "noise-only band should be below the 20 dB gate, got {snr_100}"
    );
    let measurable_100 = &document["bands"]
        .as_array()
        .expect("bands")
        .iter()
        .find(|band| (band["center_hz"].as_f64().expect("center") - 100.0).abs() < 1e-6)
        .expect("100 Hz band")["measurable"];
    assert_eq!(measurable_100, &serde_json::Value::Bool(false));
    assert!(document["signal_start_s"].as_f64().expect("start") > 0.2);
    assert!(document["noise_end_s"].as_f64().expect("noise end") > 0.1);
}

#[test]
fn band_snr_output_is_deterministic() {
    let dir = temp_dir("snr-deterministic");
    let wav = dir.join("signal.wav");
    write_pcm24(&wav, FS, 1, &signal_in_noise());
    let first = dir.join("a.json");
    let second = dir.join("b.json");
    for out in [&first, &second] {
        let run = run_bin(&[
            "band-snr",
            "--wav",
            wav.to_str().expect("path"),
            "--out",
            out.to_str().expect("path"),
        ]);
        assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    }
    assert_eq!(sha256_file(&first), sha256_file(&second));
}

#[test]
fn band_snr_rejects_silent_recording_with_input_error() {
    let dir = temp_dir("snr-silent");
    let wav = dir.join("silent.wav");
    write_pcm24(&wav, FS, 1, &vec![0.0; (2.0 * f64::from(FS)) as usize]);
    let out = dir.join("snr.json");
    let run = run_bin(&[
        "band-snr",
        "--wav",
        wav.to_str().expect("path"),
        "--out",
        out.to_str().expect("path"),
    ]);
    assert_eq!(run.code, 2, "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("E_ANALYZE_INPUT"),
        "stderr: {}",
        run.stderr
    );
    assert!(!out.exists());
}

#[test]
fn band_snr_rejects_recording_without_quiet_region() {
    let dir = temp_dir("snr-no-quiet");
    let wav = dir.join("tone.wav");
    write_pcm24(
        &wav,
        FS,
        1,
        &tone(1_000.0, 0.2, (2.0 * f64::from(FS)) as usize),
    );
    let out = dir.join("snr.json");
    let run = run_bin(&[
        "band-snr",
        "--wav",
        wav.to_str().expect("path"),
        "--out",
        out.to_str().expect("path"),
    ]);
    assert_eq!(run.code, 2, "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("E_ANALYZE_INPUT"),
        "stderr: {}",
        run.stderr
    );
}
