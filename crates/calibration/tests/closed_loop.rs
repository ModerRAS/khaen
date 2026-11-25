//! 闭环复测判定（T42 离线判定器）：三态、容差边界、缺数据、对齐错误、确定性。

use khaen_calibration::{
    compare, report_bytes, report_sha256, BandLevel, ClosedLoopConfig, LevelSet, PointLevels,
    Verdict, DEFAULT_MIN_COMPARED_BANDS, DEFAULT_TOLERANCE_DB,
};

const CENTERS: [f64; 4] = [40.0, 50.0, 63.0, 80.0];

type FixtureBands = Vec<(f64, Option<f64>)>;
type FixturePoint = (&'static str, FixtureBands);

fn levels(rate: f64, points: Vec<FixturePoint>) -> LevelSet {
    LevelSet {
        sample_rate_hz: rate,
        points: points
            .into_iter()
            .map(|(name, bands)| PointLevels {
                name: name.to_string(),
                bands: bands
                    .into_iter()
                    .map(|(center_hz, level_db)| BandLevel {
                        center_hz,
                        level_db,
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn point(name: &str, values: Vec<Option<f64>>) -> Vec<FixturePoint> {
    let bands: FixtureBands = CENTERS.iter().copied().zip(values).collect();
    let name: &'static str = Box::leak(name.to_string().into_boxed_str());
    vec![(name, bands)]
}

fn config() -> ClosedLoopConfig {
    ClosedLoopConfig::default()
}

#[test]
fn all_bands_within_tolerance_pass() {
    let predicted = levels(
        48_000.0,
        point("P01", vec![Some(60.0), Some(62.0), Some(64.0), Some(66.0)]),
    );
    let observed = levels(
        48_000.0,
        point("P01", vec![Some(61.0), Some(60.5), Some(65.5), Some(64.5)]),
    );
    let report = compare(&predicted, &observed, &config()).expect("compare");
    assert_eq!(report.verdict, Verdict::Pass);
    assert_eq!(report.tolerance_db, DEFAULT_TOLERANCE_DB);
    assert_eq!(report.min_compared_bands, DEFAULT_MIN_COMPARED_BANDS);
    assert_eq!(report.compared_bands, 4);
    assert_eq!(report.missing_bands, 0);
    assert_eq!(report.violations, 0);
    assert_eq!(report.max_abs_delta_db, Some(1.5));
    assert_eq!(report.points.len(), 1);
    assert_eq!(report.points[0].compared_bands, 4);
    assert_eq!(report.points[0].max_abs_delta_db, Some(1.5));
    assert!(report
        .notes
        .iter()
        .any(|note| note.contains("never as pass")));
}

#[test]
fn tolerance_boundary_is_inclusive() {
    let predicted = levels(
        48_000.0,
        point("P01", vec![Some(60.0), Some(60.0), Some(60.0), Some(60.0)]),
    );
    let observed = levels(
        48_000.0,
        point("P01", vec![Some(62.0), Some(58.0), Some(60.0), Some(61.0)]),
    );
    let report = compare(&predicted, &observed, &config()).expect("compare");
    assert_eq!(report.verdict, Verdict::Pass);
    assert_eq!(report.violations, 0);
    assert_eq!(report.max_abs_delta_db, Some(2.0));
    assert!(report.points[0]
        .bands
        .iter()
        .all(|band| band.within_tolerance.is_some_and(|within| within)));
}

#[test]
fn band_over_tolerance_fails_with_details() {
    let predicted = levels(
        48_000.0,
        point("P01", vec![Some(60.0), Some(60.0), Some(60.0), Some(60.0)]),
    );
    let observed = levels(
        48_000.0,
        point("P01", vec![Some(60.5), Some(63.5), Some(59.0), Some(60.0)]),
    );
    let report = compare(&predicted, &observed, &config()).expect("compare");
    assert_eq!(report.verdict, Verdict::Fail);
    assert_eq!(report.violations, 1);
    assert_eq!(report.max_abs_delta_db, Some(3.5));
    let failed: Vec<&_> = report.points[0]
        .bands
        .iter()
        .filter(|band| band.within_tolerance == Some(false))
        .collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].center_hz, 50.0);
    assert_eq!(failed[0].delta_db, Some(3.5));
    // 自定义容差 4 dB 后同一对数据转为 PASS。
    let relaxed = ClosedLoopConfig {
        tolerance_db: 4.0,
        ..config()
    };
    let report = compare(&predicted, &observed, &relaxed).expect("compare");
    assert_eq!(report.verdict, Verdict::Pass);
}

#[test]
fn missing_bands_never_count_as_pass() {
    let predicted = levels(
        48_000.0,
        point("P01", vec![Some(60.0), None, Some(64.0), None]),
    );
    let observed = levels(
        48_000.0,
        point("P01", vec![Some(60.5), Some(62.0), Some(64.5), Some(66.0)]),
    );
    let report = compare(&predicted, &observed, &config()).expect("compare");
    // 只有 2 个可比较带 < 默认 3 → INCOMPLETE，不判 PASS。
    assert_eq!(report.verdict, Verdict::Incomplete);
    assert_eq!(report.compared_bands, 2);
    assert_eq!(report.missing_bands, 2);
    assert_eq!(report.violations, 0);
    assert!(report.points[0].bands[1].delta_db.is_none());
    assert!(report.points[0].bands[1].within_tolerance.is_none());
}

#[test]
fn incomplete_when_point_set_has_too_few_bands() {
    let predicted = levels(48_000.0, point("P01", vec![Some(1.0), None, None, None]));
    let observed = levels(
        48_000.0,
        point("P01", vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0)]),
    );
    let report = compare(&predicted, &observed, &config()).expect("compare");
    assert_eq!(report.verdict, Verdict::Incomplete);
    assert_eq!(report.compared_bands, 1);
    // 把最少带数降到 1：同样的数据变成 PASS（数据足够时不再 INCOMPLETE）。
    let relaxed = ClosedLoopConfig {
        min_compared_bands: 1,
        ..config()
    };
    let report = compare(&predicted, &observed, &relaxed).expect("compare");
    assert_eq!(report.verdict, Verdict::Pass);
}

#[test]
fn alignment_errors_are_stable_codes() {
    // 测点集合不一致。
    let predicted = levels(
        48_000.0,
        point("P01", vec![Some(1.0), Some(1.0), Some(1.0), Some(1.0)]),
    );
    let observed = levels(
        48_000.0,
        point("P02", vec![Some(1.0), Some(1.0), Some(1.0), Some(1.0)]),
    );
    let err = compare(&predicted, &observed, &config()).expect_err("must reject");
    assert_eq!(err.code(), khaen_calibration::closed_loop::error::ALIGNMENT);

    // 采样率不一致。
    let observed = levels(
        96_000.0,
        point("P01", vec![Some(1.0), Some(1.0), Some(1.0), Some(1.0)]),
    );
    let err = compare(&predicted, &observed, &config()).expect_err("must reject");
    assert_eq!(err.code(), khaen_calibration::closed_loop::error::ALIGNMENT);

    // 频带中心不一致。
    let observed = levels(
        48_000.0,
        vec![(
            "P01",
            vec![
                (40.0, Some(1.0)),
                (50.0, Some(1.0)),
                (63.0, Some(1.0)),
                (81.0, Some(1.0)),
            ],
        )],
    );
    let err = compare(&predicted, &observed, &config()).expect_err("must reject");
    assert_eq!(err.code(), khaen_calibration::closed_loop::error::ALIGNMENT);
}

#[test]
fn bad_inputs_and_config_are_rejected() {
    let good = levels(
        48_000.0,
        point("P01", vec![Some(1.0), Some(1.0), Some(1.0), Some(1.0)]),
    );
    // 重复测点。
    let duplicate = LevelSet {
        sample_rate_hz: 48_000.0,
        points: vec![
            PointLevels {
                name: "P01".into(),
                bands: good.points[0].bands.clone(),
            },
            PointLevels {
                name: "P01".into(),
                bands: good.points[0].bands.clone(),
            },
        ],
    };
    let err = compare(&duplicate, &good, &config()).expect_err("duplicate points");
    assert_eq!(err.code(), khaen_calibration::closed_loop::error::BAD_INPUT);

    // 非有限电平。
    let mut bad_level = good.clone();
    bad_level.points[0].bands[0].level_db = Some(f64::NAN);
    let err = compare(&bad_level, &good, &config()).expect_err("nan level");
    assert_eq!(err.code(), khaen_calibration::closed_loop::error::BAD_INPUT);

    // 非法容差 / 最少带数。
    let bad_tolerance = ClosedLoopConfig {
        tolerance_db: 0.0,
        ..config()
    };
    let err = compare(&good, &good, &bad_tolerance).expect_err("tolerance");
    assert_eq!(err.code(), khaen_calibration::closed_loop::error::CONFIG);
    let bad_min = ClosedLoopConfig {
        min_compared_bands: 0,
        ..config()
    };
    let err = compare(&good, &good, &bad_min).expect_err("min bands");
    assert_eq!(err.code(), khaen_calibration::closed_loop::error::CONFIG);
}

#[test]
fn report_is_deterministic_and_has_sha256() {
    let predicted = levels(
        48_000.0,
        point("P01", vec![Some(60.0), Some(61.0), Some(62.0), Some(63.0)]),
    );
    let observed = levels(
        48_000.0,
        point("P01", vec![Some(60.5), Some(61.5), Some(62.5), Some(63.5)]),
    );
    let first = compare(&predicted, &observed, &config()).expect("compare");
    let second = compare(&predicted, &observed, &config()).expect("compare");
    let first_bytes = report_bytes(&first);
    let second_bytes = report_bytes(&second);
    assert_eq!(first_bytes, second_bytes);
    assert!(String::from_utf8_lossy(&first_bytes)
        .contains("\"schema\":\"khaen-closed-loop-report/v1\""));
    assert!(first_bytes.ends_with(b"\n"));
    let first_sha = report_sha256(&first);
    assert_eq!(first_sha.len(), 64);
    assert_eq!(first_sha, report_sha256(&second));
    // 结论不同 → 报告字节不同（防止 sha 恒定）。
    let failing = levels(
        48_000.0,
        point("P01", vec![Some(70.0), Some(61.0), Some(62.0), Some(63.0)]),
    );
    let failed = compare(&predicted, &failing, &config()).expect("compare");
    assert_ne!(first_sha, report_sha256(&failed));
}
