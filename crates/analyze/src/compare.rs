//! 两段录音的相对频带电平与 delta（与 khaen-band-levels 同一口径）。
//!
//! delta_db = 10*log10(实测带功率 / 参考带功率)：带功率直接调用
//! khaen_calibration::band_powers（khaen-band-levels 用的同一份 Hann/Welch/频带积分代码），
//! 本模块只做 dB 换算、两侧绝对电平（dBFS，口径同 spectrum）与差值统计。
//! 因此同一对 WAV + 同一组中心频率下，本工具的 delta_db 与 khaen-band-levels 的 level_db
//! 逐位相同（集成测试锁定）。
//!
//! 用途：真机闭环复测里先看两段录音的逐带 delta 是否就是预期频响（诊断），再由
//! khaen-band-levels / khaen-closed-loop-run 做正式判定。

use khaen_calibration::band_levels::BandLevelsConfig;
use khaen_calibration::band_powers;
use serde_json::Value;

use crate::json;
use crate::spectrum;
use crate::wav::Wav;
use crate::{codes, AnalyzeError, COMPARE_SCHEMA, TOOL_NAME};

/// 两段录音在一个频带上的相对电平与 delta。
#[derive(Clone, Debug, PartialEq)]
pub struct CompareBand {
    /// 1/3 oct 中心频率（Hz）。
    pub center_hz: f64,
    /// 参考录音的绝对带电平（dBFS）；无数据为 None。
    pub reference_dbfs: Option<f64>,
    /// 实测录音的绝对带电平（dBFS）；无数据为 None。
    pub measured_dbfs: Option<f64>,
    /// 相对电平 = 10*log10(实测带功率/参考带功率)（dB）；任一侧无能量为 None。
    pub delta_db: Option<f64>,
}

/// 两段录音对比结果。
#[derive(Clone, Debug, PartialEq)]
pub struct CompareReport {
    /// 采样率（Hz，两段一致）。
    pub sample_rate_hz: f64,
    /// 每段 FFT 窗长（样本）。
    pub window_len: usize,
    /// 参与平均的分段数（两段相同输入长度时才一致；按参考录音计）。
    pub segments: usize,
    /// 逐带对比（中心频率升序）。
    pub bands: Vec<CompareBand>,
    /// 两侧都有能量的可比较带数。
    pub compared_bands: usize,
    /// 任一侧无能量的缺数据带数。
    pub missing_bands: usize,
    /// 可比较带的最大 |delta|（dB）；无可比较带为 None。
    pub max_abs_delta_db: Option<f64>,
    /// 可比较带 delta 的 RMS（dB）；无可比较带为 None。
    pub rms_delta_db: Option<f64>,
}

/// 对比两段 WAV（参考 = 无方案基线，实测 = 施加方案后的录音）。
pub fn compare_wavs(
    reference: &Wav,
    measured: &Wav,
    centers_hz: &[f64],
    window_len: usize,
) -> Result<CompareReport, AnalyzeError> {
    if (reference.sample_rate_hz - measured.sample_rate_hz).abs() > 1e-9 {
        return Err(AnalyzeError::new(
            codes::ANALYZE_ALIGNMENT,
            format!(
                "sample rate mismatch: reference {} Hz, measured {} Hz",
                reference.sample_rate_hz, measured.sample_rate_hz
            ),
        ));
    }
    let config = BandLevelsConfig { window_len };
    let reference_powers = band_powers(
        &reference.samples,
        reference.sample_rate_hz,
        centers_hz,
        &config,
    )
    .map_err(|err| AnalyzeError::new(err.code(), err.message().to_string()))?;
    let measured_powers = band_powers(
        &measured.samples,
        measured.sample_rate_hz,
        centers_hz,
        &config,
    )
    .map_err(|err| AnalyzeError::new(err.code(), err.message().to_string()))?;
    let reference_levels = reference_powers
        .iter()
        .map(|&power| power_to_db(power))
        .collect::<Vec<_>>();
    let measured_levels = measured_powers
        .iter()
        .map(|&power| power_to_db(power))
        .collect::<Vec<_>>();
    let mut bands = Vec::with_capacity(centers_hz.len());
    for (index, &center_hz) in centers_hz.iter().enumerate() {
        let reference_db = reference_levels[index];
        let measured_db = measured_levels[index];
        let delta_db = if reference_powers[index] > 0.0 && measured_powers[index] > 0.0 {
            Some(10.0 * (measured_powers[index] / reference_powers[index]).log10())
        } else {
            None
        };
        bands.push(CompareBand {
            center_hz,
            reference_dbfs: reference_db,
            measured_dbfs: measured_db,
            delta_db,
        });
    }
    let deltas: Vec<f64> = bands.iter().filter_map(|band| band.delta_db).collect();
    let compared_bands = deltas.len();
    let missing_bands = bands.len() - compared_bands;
    let max_abs_delta_db = deltas.iter().fold(None, |worst: Option<f64>, delta| {
        Some(worst.map_or(delta.abs(), |worst| worst.max(delta.abs())))
    });
    let rms_delta_db = if deltas.is_empty() {
        None
    } else {
        let mean_square =
            deltas.iter().map(|delta| delta * delta).sum::<f64>() / deltas.len() as f64;
        Some(mean_square.sqrt())
    };
    Ok(CompareReport {
        sample_rate_hz: reference.sample_rate_hz,
        window_len,
        segments: spectrum::segment_count(reference.samples.len(), window_len),
        bands,
        compared_bands,
        missing_bands,
        max_abs_delta_db,
        rms_delta_db,
    })
}

/// compare 子命令的确定性 JSON 文档。
pub fn document(reference_path: &str, measured_path: &str, report: &CompareReport) -> Value {
    let bands: Vec<Value> = report
        .bands
        .iter()
        .map(|band| {
            json::object([
                ("center_hz", json::number(band.center_hz)),
                ("reference_dbfs", json::opt_number(band.reference_dbfs)),
                ("measured_dbfs", json::opt_number(band.measured_dbfs)),
                ("delta_db", json::opt_number(band.delta_db)),
            ])
        })
        .collect();
    json::object([
        ("schema", Value::from(COMPARE_SCHEMA)),
        ("tool", Value::from(TOOL_NAME)),
        ("tool_version", Value::from(env!("CARGO_PKG_VERSION"))),
        ("command", Value::from("compare")),
        ("reference_wav", Value::from(reference_path)),
        ("measured_wav", Value::from(measured_path)),
        ("sample_rate_hz", json::number(report.sample_rate_hz)),
        ("window", Value::from(spectrum::WINDOW_NAME)),
        ("window_len", json::count(report.window_len as u64)),
        ("hop", json::count((report.window_len / 2) as u64)),
        ("segments", json::count(report.segments as u64)),
        ("level_unit", Value::from("dBFS")),
        ("bands", Value::Array(bands)),
        ("compared_bands", json::count(report.compared_bands as u64)),
        ("missing_bands", json::count(report.missing_bands as u64)),
        (
            "max_abs_delta_db",
            json::opt_number(report.max_abs_delta_db),
        ),
        ("rms_delta_db", json::opt_number(report.rms_delta_db)),
    ])
}

fn power_to_db(power: f64) -> Option<f64> {
    if power.is_finite() && power > 0.0 {
        Some(10.0 * power.log10())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spectrum::DEFAULT_WINDOW_LEN;
    use khaen_calibration::relative_band_levels;

    fn sine(freq_hz: f64, amplitude: f64, frames: usize, sample_rate_hz: f64) -> Wav {
        Wav {
            sample_rate_hz,
            channels: 1,
            samples: (0..frames)
                .map(|index| {
                    amplitude
                        * (2.0 * std::f64::consts::PI * freq_hz * index as f64 / sample_rate_hz)
                            .sin()
                })
                .collect(),
        }
    }

    #[test]
    fn delta_matches_calibration_relative_band_levels_bitwise() {
        let sample_rate_hz = 48_000.0;
        let centers = [100.0, 1_000.0, 10_000.0];
        let reference = sine(1_000.0, 0.5, 48_000, sample_rate_hz);
        let measured = sine(1_000.0, 0.25, 48_000, sample_rate_hz);
        let report =
            compare_wavs(&reference, &measured, &centers, DEFAULT_WINDOW_LEN).expect("compare");
        let expected = relative_band_levels(
            &measured.samples,
            &reference.samples,
            sample_rate_hz,
            &centers,
            &BandLevelsConfig {
                window_len: DEFAULT_WINDOW_LEN,
            },
        )
        .expect("relative levels");
        for (band, expected) in report.bands.iter().zip(expected.iter()) {
            assert_eq!(band.delta_db, expected.level_db);
        }
        let delta = report
            .bands
            .iter()
            .find(|band| (band.center_hz - 1_000.0).abs() < 1e-9)
            .and_then(|band| band.delta_db)
            .expect("1 kHz delta");
        assert!((delta - -6.0206).abs() < 0.01, "delta {delta}");
    }

    #[test]
    fn identical_recordings_have_zero_delta() {
        let recording = sine(1_000.0, 0.5, 24_000, 48_000.0);
        let report = compare_wavs(
            &recording,
            &recording,
            &[100.0, 1_000.0, 10_000.0],
            DEFAULT_WINDOW_LEN,
        )
        .expect("compare");
        for band in &report.bands {
            match band.delta_db {
                Some(delta) => assert_eq!(delta, 0.0),
                None => assert!(band.reference_dbfs.is_none() && band.measured_dbfs.is_none()),
            }
        }
    }

    #[test]
    fn sample_rate_mismatch_is_alignment_error() {
        let reference = sine(1_000.0, 0.5, 24_000, 48_000.0);
        let measured = sine(1_000.0, 0.5, 24_000, 96_000.0);
        let error = compare_wavs(&reference, &measured, &[1_000.0], DEFAULT_WINDOW_LEN)
            .expect_err("mismatch");
        assert_eq!(error.code(), codes::ANALYZE_ALIGNMENT);
    }
}
