//! plan-levels：从方案 ABI v3 推预测带域电平，并喂闭环对账。

use khaen_calibration::closed_loop::{
    compare, levels_bytes, levels_sha256, BandLevel, ClosedLoopConfig, LevelSet, PointLevels,
    Verdict,
};
use khaen_calibration::plan_levels::{
    predicted_levels, BAD_INPUT, CONFIG, DEFAULT_CENTERS_HZ, PLAN,
};
use khaen_core::plan::{
    ContentSection, CrossoverFilter, CrossoverSection, Fold, MatrixSection, PeqSection, Plan,
    ProtectionEntry, ProtectionSection, RoutingSection,
};
use khaen_designer::modal_peq::{BandType, Biquad};
use khaen_plan_builder::{PlanBuilder, PlanConfig};

const RATE: u32 = 48_000;
const FS: f64 = 48_000.0;

fn identity_peq() -> PeqSection {
    PeqSection {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    }
}

fn peq_from(biquad: Biquad) -> PeqSection {
    assert!((biquad.a[0] - 1.0).abs() <= 1e-15);
    PeqSection {
        b0: biquad.b[0] as f32,
        b1: biquad.b[1] as f32,
        b2: biquad.b[2] as f32,
        a1: biquad.a[1] as f32,
        a2: biquad.a[2] as f32,
    }
}

fn constant_gain_peq(gain_db: f64) -> PeqSection {
    PeqSection {
        b0: 10.0f64.powf(gain_db / 20.0) as f32,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    }
}

fn entry(lane: u16, gain_db: f32, peq: Vec<PeqSection>) -> ProtectionEntry {
    ProtectionEntry {
        lane,
        delay: 0,
        polarity: 1,
        gain_db,
        peq,
    }
}

fn fixture_plan(lanes: u16, entries: Vec<ProtectionEntry>) -> Plan {
    fixture_plan_at(RATE, lanes, entries)
}

fn fixture_plan_at(sample_rate: u32, lanes: u16, entries: Vec<ProtectionEntry>) -> Plan {
    PlanBuilder::new(PlanConfig::new(128, sample_rate, lanes))
        .set_content_identity(lanes)
        .set_crossover_identity(lanes)
        .set_matrix_identity(lanes)
        .set_protection(ProtectionSection { lanes, entries })
        .set_routing_identity(lanes)
        .build()
        .expect("fixture plan")
}

#[test]
fn identity_peq_predicts_zero_db_for_every_lane() {
    let plan = fixture_plan(
        2,
        vec![entry(0, 0.0, vec![]), entry(1, 0.0, vec![identity_peq()])],
    );
    let levels = predicted_levels(&plan, &[40.0, 1_000.0, 10_000.0]).expect("levels");
    assert_eq!(levels.sample_rate_hz, FS);
    assert_eq!(levels.points.len(), 2);
    assert_eq!(levels.points[0].name, "L00");
    assert_eq!(levels.points[1].name, "L01");
    for point in &levels.points {
        assert_eq!(point.bands.len(), 3);
        for band in &point.bands {
            assert!(band.level_db.expect("level").abs() <= 1e-9, "{band:?}");
        }
    }
}

#[test]
fn constant_gain_biquad_predicts_its_gain() {
    let plan = fixture_plan(1, vec![entry(0, 0.0, vec![constant_gain_peq(6.0)])]);
    let levels = predicted_levels(&plan, &[40.0, 1_000.0, 10_000.0]).expect("levels");
    for band in &levels.points[0].bands {
        assert!((band.level_db.unwrap() - 6.0).abs() <= 1e-5, "{band:?}");
    }
}

#[test]
fn peq_cascade_matches_designer_magnitude() {
    let first = Biquad::from_params(BandType::Peaking, 63.0, 4.0, 6.0, FS).expect("band 1");
    let second = Biquad::from_params(BandType::Peaking, 2_500.0, 2.0, -4.5, FS).expect("band 2");
    let plan = fixture_plan(
        1,
        vec![entry(0, 1.5, vec![peq_from(first), peq_from(second)])],
    );
    let centers = [40.0, 63.0, 100.0, 1_000.0, 2_500.0, 16_000.0];
    let levels = predicted_levels(&plan, &centers).expect("levels");
    // 方案里的系数是 f32；先按量化后的系数（转回 f64）用 designer 的公式算期望，
    // 这是对"我们的频响公式"本身的逐带精确对账。
    let quantized_first = Biquad {
        b: [first.b[0], first.b[1], first.b[2]].map(|value| f64::from(value as f32)),
        a: [first.a[0], first.a[1], first.a[2]].map(|value| f64::from(value as f32)),
    };
    let quantized_second = Biquad {
        b: [second.b[0], second.b[1], second.b[2]].map(|value| f64::from(value as f32)),
        a: [second.a[0], second.a[1], second.a[2]].map(|value| f64::from(value as f32)),
    };
    for band in &levels.points[0].bands {
        let expected = 1.5
            + quantized_first.magnitude_db(band.center_hz, FS)
            + quantized_second.magnitude_db(band.center_hz, FS);
        assert!(
            (band.level_db.unwrap() - expected).abs() <= 1e-9,
            "center {} Hz: predicted {:?}, quantized designer {expected}",
            band.center_hz,
            band.level_db
        );
    }
    // f32 量化在低频（分子接近抵消）会带来约 1e-2 dB 的预测误差，远小于 2 dB 判定容差；
    // 这里用 0.05 dB 锁住这个量级，避免将来悄悄换成 f64 而失去这个已知边界。
    for band in &levels.points[0].bands {
        let exact =
            1.5 + first.magnitude_db(band.center_hz, FS) + second.magnitude_db(band.center_hz, FS);
        assert!(
            (band.level_db.unwrap() - exact).abs() <= 0.05,
            "center {} Hz: predicted {:?}, f64 designer {exact}",
            band.center_hz,
            band.level_db
        );
    }
}

#[test]
fn lane_gain_adds_to_peq_gain() {
    let plan = fixture_plan(1, vec![entry(0, -3.0, vec![constant_gain_peq(2.0)])]);
    let levels = predicted_levels(&plan, &[1_000.0]).expect("levels");
    assert!((levels.points[0].bands[0].level_db.unwrap() + 1.0).abs() <= 1e-5);
}

#[test]
fn default_centers_are_one_third_octave_and_json_is_deterministic() {
    assert_eq!(DEFAULT_CENTERS_HZ.len(), 30);
    assert_eq!(DEFAULT_CENTERS_HZ[0], 25.0);
    assert_eq!(DEFAULT_CENTERS_HZ[29], 20_000.0);
    let plan = fixture_plan(1, vec![entry(0, 0.0, vec![constant_gain_peq(3.0)])]);
    let levels = predicted_levels(&plan, &DEFAULT_CENTERS_HZ).expect("levels");
    assert_eq!(levels.points[0].bands.len(), 30);
    let bytes = levels_bytes(&levels);
    assert_eq!(bytes, levels_bytes(&levels.clone()));
    assert!(bytes.ends_with(b"\n"));
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.starts_with("{\"schema\":\"khaen-closed-loop-levels/v1\""));
    assert!(text.contains("\"name\":\"L00\""));
    let sha = levels_sha256(&levels);
    assert_eq!(sha.len(), 64);
    assert!(sha.chars().all(|character| character.is_ascii_hexdigit()));
}

#[test]
fn closed_loop_verdict_consumes_predicted_levels() {
    let plan = fixture_plan(1, vec![entry(0, 0.0, vec![constant_gain_peq(4.0)])]);
    let centers = [40.0, 63.0, 100.0, 160.0, 250.0];
    let predicted = predicted_levels(&plan, &centers).expect("levels");

    let mut within = predicted.clone();
    for band in &mut within.points[0].bands {
        band.level_db = band.level_db.map(|value| value + 1.5);
    }
    let report = compare(&predicted, &within, &ClosedLoopConfig::default()).expect("compare");
    assert_eq!(report.verdict, Verdict::Pass);
    assert_eq!(report.compared_bands, 5);

    let mut violated = predicted.clone();
    for band in &mut violated.points[0].bands {
        band.level_db = band.level_db.map(|value| value + 2.5);
    }
    let report = compare(&predicted, &violated, &ClosedLoopConfig::default()).expect("compare");
    assert_eq!(report.verdict, Verdict::Fail);
    assert_eq!(report.violations, 5);

    let mut sparse = predicted.clone();
    for (index, band) in sparse.points[0].bands.iter_mut().enumerate() {
        if index > 0 {
            band.level_db = None;
        }
    }
    let report = compare(&predicted, &sparse, &ClosedLoopConfig::default()).expect("compare");
    assert_eq!(report.verdict, Verdict::Incomplete);
    assert_eq!(report.compared_bands, 1);
}

#[test]
fn unsupported_sample_rate_and_missing_protection_are_plan_errors() {
    let plan = fixture_plan_at(96_000, 1, vec![entry(0, 0.0, vec![])]);
    let levels = predicted_levels(&plan, &[1_000.0]).expect("96k plan is supported");
    assert_eq!(levels.sample_rate_hz, 96_000.0);

    let mut bad_rate = fixture_plan(1, vec![entry(0, 0.0, vec![])]);
    bad_rate.sample_rate = 44_100;
    let error = predicted_levels(&bad_rate, &[1_000.0]).expect_err("44.1k must fail");
    assert_eq!(error.code(), PLAN);

    let mut no_protection = fixture_plan(1, vec![entry(0, 0.0, vec![])]);
    no_protection.protection = None;
    let error = predicted_levels(&no_protection, &[1_000.0]).expect_err("missing protection");
    assert_eq!(error.code(), PLAN);
}

#[test]
fn incomplete_or_invalid_protection_is_plan_error() {
    // lane 越界
    let mut plan = fixture_plan(1, vec![entry(0, 0.0, vec![])]);
    plan.protection = Some(ProtectionSection {
        lanes: 1,
        entries: vec![entry(3, 0.0, vec![])],
    });
    assert_eq!(
        predicted_levels(&plan, &[1_000.0])
            .expect_err("lane out of range")
            .code(),
        PLAN
    );

    // 重复 lane
    let mut plan = fixture_plan(2, vec![entry(0, 0.0, vec![]), entry(1, 0.0, vec![])]);
    plan.protection = Some(ProtectionSection {
        lanes: 2,
        entries: vec![entry(0, 0.0, vec![]), entry(0, 0.0, vec![])],
    });
    assert_eq!(
        predicted_levels(&plan, &[1_000.0])
            .expect_err("duplicate lane")
            .code(),
        PLAN
    );

    // 缺 lane
    let mut plan = fixture_plan(2, vec![entry(0, 0.0, vec![]), entry(1, 0.0, vec![])]);
    plan.protection = Some(ProtectionSection {
        lanes: 2,
        entries: vec![entry(0, 0.0, vec![])],
    });
    assert_eq!(
        predicted_levels(&plan, &[1_000.0])
            .expect_err("missing lane")
            .code(),
        PLAN
    );

    // 非有限增益
    let mut plan = fixture_plan(1, vec![entry(0, 0.0, vec![])]);
    plan.protection = Some(ProtectionSection {
        lanes: 1,
        entries: vec![entry(0, f32::NAN, vec![])],
    });
    assert_eq!(
        predicted_levels(&plan, &[1_000.0])
            .expect_err("nan gain")
            .code(),
        PLAN
    );
}

#[test]
fn non_identity_sections_are_rejected_not_silently_ignored() {
    let base = fixture_plan(1, vec![entry(0, 0.0, vec![constant_gain_peq(3.0)])]);

    let mut content = base.clone();
    content.content = Some(ContentSection {
        in_channels: 1,
        out_lanes: 1,
        folds: vec![Fold {
            src: 0,
            dst: 0,
            gain: 0.5,
        }],
    });
    assert_eq!(
        predicted_levels(&content, &[1_000.0])
            .expect_err("non-identity content")
            .code(),
        PLAN
    );

    let mut matrix = base.clone();
    matrix.matrix = Some(MatrixSection {
        rows: 1,
        cols: 1,
        decimation: 1,
        interpolation: 1,
        taps: 1,
        coeffs: vec![0.5],
    });
    assert_eq!(
        predicted_levels(&matrix, &[1_000.0])
            .expect_err("non-identity matrix")
            .code(),
        PLAN
    );

    let mut routing = base;
    routing.routing = Some(RoutingSection {
        dac_channels: 1,
        routes: vec![(0, 1)],
    });
    assert_eq!(
        predicted_levels(&routing, &[1_000.0])
            .expect_err("non-identity routing")
            .code(),
        PLAN
    );
}

#[test]
fn crossover_fir_contributes_to_predicted_levels() {
    // 单 tap 2.0 = 恒定 +6.0206 dB；忽略保护段的 0 dB。
    let mut plan = fixture_plan(1, vec![entry(0, 0.0, vec![])]);
    plan.crossover = Some(CrossoverSection {
        lanes: 1,
        filters: vec![CrossoverFilter {
            lane: 0,
            coeffs: vec![2.0],
        }],
    });
    let levels = predicted_levels(&plan, &[100.0, 1_000.0, 10_000.0]).expect("levels");
    for band in &levels.points[0].bands {
        assert!((band.level_db.unwrap() - 6.0206).abs() <= 1e-4, "{band:?}");
    }

    // 两点低通 [0.5, 0.5]：|H(f)| = cos(pi f / fs)，与手算逐带对账。
    let mut lowpass = fixture_plan(1, vec![entry(0, 0.0, vec![])]);
    lowpass.crossover = Some(CrossoverSection {
        lanes: 1,
        filters: vec![CrossoverFilter {
            lane: 0,
            coeffs: vec![0.5, 0.5],
        }],
    });
    let centers = [100.0, 1_000.0, 10_000.0, 20_000.0];
    let levels = predicted_levels(&lowpass, &centers).expect("levels");
    for band in &levels.points[0].bands {
        let expected = 20.0
            * (std::f64::consts::PI * band.center_hz / FS)
                .cos()
                .abs()
                .log10();
        assert!(
            (band.level_db.unwrap() - expected).abs() <= 1e-4,
            "center {} Hz: predicted {:?}, expected {expected}",
            band.center_hz,
            band.level_db
        );
    }

    // 未给 FIR 的 lane 记 0 dB（恒等）；给两个 lane 时各算各的。
    let mut two_lanes = fixture_plan(2, vec![entry(0, 0.0, vec![]), entry(1, 0.0, vec![])]);
    two_lanes.crossover = Some(CrossoverSection {
        lanes: 2,
        filters: vec![CrossoverFilter {
            lane: 1,
            coeffs: vec![2.0],
        }],
    });
    let levels = predicted_levels(&two_lanes, &[1_000.0]).expect("levels");
    assert!((levels.points[0].bands[0].level_db.unwrap()).abs() <= 1e-12);
    assert!((levels.points[1].bands[0].level_db.unwrap() - 6.0206).abs() <= 1e-4);

    // lane 越界与重复 FIR 都是方案错误。
    let mut out_of_range = fixture_plan(1, vec![entry(0, 0.0, vec![])]);
    out_of_range.crossover = Some(CrossoverSection {
        lanes: 1,
        filters: vec![CrossoverFilter {
            lane: 3,
            coeffs: vec![1.0],
        }],
    });
    assert_eq!(
        predicted_levels(&out_of_range, &[1_000.0])
            .expect_err("lane out of range")
            .code(),
        PLAN
    );
    let mut duplicate = fixture_plan(1, vec![entry(0, 0.0, vec![])]);
    duplicate.crossover = Some(CrossoverSection {
        lanes: 1,
        filters: vec![
            CrossoverFilter {
                lane: 0,
                coeffs: vec![1.0],
            },
            CrossoverFilter {
                lane: 0,
                coeffs: vec![1.0],
            },
        ],
    });
    assert_eq!(
        predicted_levels(&duplicate, &[1_000.0])
            .expect_err("duplicate FIR")
            .code(),
        PLAN
    );
}
#[test]
fn invalid_band_centers_are_input_or_config_errors() {
    let plan = fixture_plan(1, vec![entry(0, 0.0, vec![])]);
    assert_eq!(
        predicted_levels(&plan, &[])
            .expect_err("empty centers")
            .code(),
        CONFIG
    );
    for centers in [
        vec![0.0],
        vec![-10.0],
        vec![f64::NAN],
        vec![f64::INFINITY],
        vec![24_000.0],
        vec![100.0, 100.0],
        vec![100.0, 63.0],
    ] {
        let error = predicted_levels(&plan, &centers).expect_err("bad centers");
        assert_eq!(error.code(), BAD_INPUT, "centers {centers:?}");
    }
}

#[test]
fn predicted_levels_round_trip_through_levels_json_shape() {
    let plan = fixture_plan(1, vec![entry(0, 2.5, vec![constant_gain_peq(1.5)])]);
    let levels = predicted_levels(&plan, &[40.0, 1_000.0, 10_000.0]).expect("levels");
    // 手工构造与 levels_bytes 相同语义的 LevelSet，确认结构可以被闭环对账直接使用。
    let rebuilt = LevelSet {
        sample_rate_hz: FS,
        points: vec![PointLevels {
            name: "L00".to_string(),
            bands: levels.points[0]
                .bands
                .iter()
                .map(|band| BandLevel {
                    center_hz: band.center_hz,
                    level_db: band.level_db,
                })
                .collect(),
        }],
    };
    let report = compare(&levels, &rebuilt, &ClosedLoopConfig::default()).expect("compare");
    assert_eq!(report.verdict, Verdict::Pass);
    assert_eq!(report.max_abs_delta_db, Some(0.0));
}
