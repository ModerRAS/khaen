//! 实测带域电平：Welch 1/3 oct 积分 + 相对电平 + 与闭环判定的联动。

use khaen_calibration::band_levels::{
    band_powers, relative_band_levels, BandLevelsConfig, ALIGNMENT, BAD_INPUT, CONFIG,
};
use khaen_calibration::closed_loop::{
    compare, levels_bytes, BandLevel, ClosedLoopConfig, LevelSet, PointLevels, Verdict,
};

const FS: f64 = 48_000.0;
const LEN: usize = 24_000;

fn sine(freq_hz: f64, amplitude: f64, len: usize) -> Vec<f64> {
    (0..len)
        .map(|index| amplitude * (2.0 * std::f64::consts::PI * freq_hz * index as f64 / FS).sin())
        .collect()
}

fn add(left: &[f64], right: &[f64]) -> Vec<f64> {
    left.iter().zip(right.iter()).map(|(a, b)| a + b).collect()
}

#[test]
fn sine_gain_is_measured_per_band() {
    let reference = sine(1_000.0, 0.25, LEN);
    let measured = sine(1_000.0, 0.5, LEN);
    let centers = [100.0, 1_000.0, 10_000.0];
    let levels = relative_band_levels(
        &measured,
        &reference,
        FS,
        &centers,
        &BandLevelsConfig::default(),
    )
    .expect("levels");
    assert_eq!(levels.len(), 3);
    let mid = levels[1].level_db.expect("1 kHz level");
    assert!((mid - 6.0206).abs() <= 0.05, "1 kHz relative gain {mid}");
}

#[test]
fn identical_recordings_give_zero_db() {
    let reference = add(&sine(100.0, 0.2, LEN), &sine(1_000.0, 0.3, LEN));
    let levels = relative_band_levels(
        &reference,
        &reference,
        FS,
        &[100.0, 1_000.0, 10_000.0],
        &BandLevelsConfig::default(),
    )
    .expect("levels");
    for band in &levels {
        if let Some(value) = band.level_db {
            assert!(value.abs() <= 1e-12, "{band:?}");
        }
    }
    // 相同录音的每一带比值都应是精确的 0 dB；远离声源的带只有数值泄漏，两侧一致。
    assert!(levels.iter().all(|band| band.level_db.is_some()));
}

#[test]
fn two_tone_changes_track_their_bands() {
    let reference = add(&sine(100.0, 0.2, LEN), &sine(1_000.0, 0.4, LEN));
    let measured = add(&sine(100.0, 0.1, LEN), &sine(1_000.0, 0.8, LEN));
    let levels = relative_band_levels(
        &measured,
        &reference,
        FS,
        &[100.0, 1_000.0, 10_000.0],
        &BandLevelsConfig::default(),
    )
    .expect("levels");
    let low = levels[0].level_db.expect("100 Hz");
    let mid = levels[1].level_db.expect("1 kHz");
    assert!((low + 6.0206).abs() <= 0.1, "100 Hz relative gain {low}");
    assert!((mid - 6.0206).abs() <= 0.1, "1 kHz relative gain {mid}");
}

#[test]
fn sine_band_power_matches_known_rms() {
    let samples = sine(1_000.0, 1.0, LEN);
    let powers =
        band_powers(&samples, FS, &[1_000.0], &BandLevelsConfig::default()).expect("powers");
    // 满幅正弦的均方 = 0.5 → 10*log10(0.5) = -3.0103 dB。
    let level_db = 10.0 * powers[0].log10();
    assert!(
        (level_db + 3.0103).abs() <= 0.05,
        "band power level {level_db}"
    );
}

#[test]
fn silence_reference_marks_bands_missing() {
    let measured = sine(1_000.0, 0.5, LEN);
    let reference = vec![0.0; LEN];
    let levels = relative_band_levels(
        &measured,
        &reference,
        FS,
        &[100.0, 1_000.0],
        &BandLevelsConfig::default(),
    )
    .expect("levels");
    assert!(levels.iter().all(|band| band.level_db.is_none()));
}

#[test]
fn short_or_non_finite_input_is_rejected() {
    let short = vec![0.1; 100];
    let error = relative_band_levels(&short, &short, FS, &[1_000.0], &BandLevelsConfig::default())
        .expect_err("short recording");
    assert_eq!(error.code(), BAD_INPUT);

    let mut samples = sine(1_000.0, 0.5, LEN);
    samples[17] = f64::NAN;
    let error = relative_band_levels(
        &samples,
        &sine(1_000.0, 0.5, LEN),
        FS,
        &[1_000.0],
        &BandLevelsConfig::default(),
    )
    .expect_err("nan sample");
    assert_eq!(error.code(), BAD_INPUT);

    let error = relative_band_levels(
        &sine(1_000.0, 0.5, LEN),
        &sine(1_000.0, 0.5, LEN),
        0.0,
        &[1_000.0],
        &BandLevelsConfig::default(),
    )
    .expect_err("zero sample rate");
    assert_eq!(error.code(), BAD_INPUT);
}

#[test]
fn bad_config_and_centers_have_stable_codes() {
    let samples = sine(1_000.0, 0.5, LEN);
    let not_power_of_two = BandLevelsConfig { window_len: 1_000 };
    let error = relative_band_levels(&samples, &samples, FS, &[1_000.0], &not_power_of_two)
        .expect_err("non power of two");
    assert_eq!(error.code(), CONFIG);

    let too_small = BandLevelsConfig { window_len: 64 };
    let error = relative_band_levels(&samples, &samples, FS, &[1_000.0], &too_small)
        .expect_err("too small window");
    assert_eq!(error.code(), CONFIG);

    for centers in [
        vec![],
        vec![f64::NAN],
        vec![24_000.0],
        vec![1_000.0, 100.0],
        vec![1_000.0, 1_000.0],
    ] {
        let error = relative_band_levels(
            &samples,
            &samples,
            FS,
            &centers,
            &BandLevelsConfig::default(),
        )
        .expect_err("bad centers");
        assert_eq!(error.code(), ALIGNMENT, "centers {centers:?}");
    }
}

#[test]
fn relative_levels_feed_closed_loop_verdict() {
    let centers = [100.0, 1_000.0, 10_000.0];
    let reference = add(&sine(100.0, 0.3, LEN), &sine(1_000.0, 0.3, LEN));
    let predicted = LevelSet {
        sample_rate_hz: FS,
        points: vec![PointLevels {
            name: "P01".to_string(),
            bands: centers
                .iter()
                .map(|&center_hz| BandLevel {
                    center_hz,
                    level_db: Some(0.0),
                })
                .collect(),
        }],
    };

    // 实测与参考相同：三带相对电平都是 0 dB → PASS。
    let observed_levels = relative_band_levels(
        &reference,
        &reference,
        FS,
        &centers,
        &BandLevelsConfig::default(),
    )
    .expect("levels");
    let observed = LevelSet {
        sample_rate_hz: FS,
        points: vec![PointLevels {
            name: "P01".to_string(),
            bands: observed_levels.clone(),
        }],
    };
    assert_eq!(levels_bytes(&observed), levels_bytes(&observed.clone()));
    let report = compare(&predicted, &observed, &ClosedLoopConfig::default()).expect("compare");
    assert_eq!(report.verdict, Verdict::Pass);
    assert_eq!(report.violations, 0);

    // 1 kHz 实测放大 2 倍（+6 dB）：超 2 dB 容差 → FAIL。
    let measured = add(&sine(100.0, 0.3, LEN), &sine(1_000.0, 0.6, LEN));
    let observed_levels = relative_band_levels(
        &measured,
        &reference,
        FS,
        &centers,
        &BandLevelsConfig::default(),
    )
    .expect("levels");
    let observed = LevelSet {
        sample_rate_hz: FS,
        points: vec![PointLevels {
            name: "P01".to_string(),
            bands: observed_levels,
        }],
    };
    let report = compare(&predicted, &observed, &ClosedLoopConfig::default()).expect("compare");
    assert_eq!(report.verdict, Verdict::Fail);
    assert!(report.violations >= 1);
    assert!(report.points[0]
        .bands
        .iter()
        .any(|band| band.center_hz == 1_000.0 && band.within_tolerance == Some(false)));

    // 参考是静音：所有带都缺数据 → INCOMPLETE，绝不冒充通过。
    let silence = vec![0.0; LEN];
    let observed_levels = relative_band_levels(
        &reference,
        &silence,
        FS,
        &centers,
        &BandLevelsConfig::default(),
    )
    .expect("levels");
    let observed = LevelSet {
        sample_rate_hz: FS,
        points: vec![PointLevels {
            name: "P01".to_string(),
            bands: observed_levels,
        }],
    };
    let report = compare(&predicted, &observed, &ClosedLoopConfig::default()).expect("compare");
    assert_eq!(report.verdict, Verdict::Incomplete);
}
