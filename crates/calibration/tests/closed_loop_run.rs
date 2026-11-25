//! 闭环复测运行包：方案 + 两段录音 → 预测/实测/报告/清单（确定性、可审计）。

use khaen_calibration::closed_loop_run::{
    build_run_bundle, ClosedLoopRunError, RunBundle, RunConfig, RunInputs, ALIGNMENT, CONFIG,
    INPUT, MANIFEST_FILE, OBSERVED_FILE, PLAN, PREDICTED_FILE, REPORT_FILE,
};
use khaen_calibration::Verdict;
use khaen_core::plan::{Plan, ProtectionEntry, ProtectionSection};
use khaen_plan_builder::{PlanBuilder, PlanConfig};

const FS: f64 = 48_000.0;
const LEN: usize = 24_000;

fn constant_gain_peq(gain_db: f64) -> khaen_core::plan::PeqSection {
    khaen_core::plan::PeqSection {
        b0: 10.0f64.powf(gain_db / 20.0) as f32,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    }
}

fn fixture_plan() -> Plan {
    PlanBuilder::new(PlanConfig::new(128, 48_000, 1))
        .set_content_identity(1)
        .set_crossover_identity(1)
        .set_matrix_identity(1)
        .set_protection(ProtectionSection {
            lanes: 1,
            entries: vec![ProtectionEntry {
                lane: 0,
                delay: 0,
                polarity: 1,
                gain_db: 0.0,
                peq: vec![constant_gain_peq(6.0)],
            }],
        })
        .set_routing_identity(1)
        .build()
        .expect("fixture plan")
}

fn sine(freq_hz: f64, amplitude: f64) -> Vec<f64> {
    (0..LEN)
        .map(|index| amplitude * (2.0 * std::f64::consts::PI * freq_hz * index as f64 / FS).sin())
        .collect()
}

fn baseline() -> Vec<f64> {
    let mut samples = sine(100.0, 0.05);
    for (target, extra) in samples
        .iter_mut()
        .zip(sine(1_000.0, 0.05).iter().zip(sine(10_000.0, 0.05).iter()))
    {
        *target += extra.0 + extra.1;
    }
    samples
}

fn scaled(samples: &[f64], factor: f64) -> Vec<f64> {
    samples.iter().map(|value| value * factor).collect()
}

fn sha(character: char) -> String {
    character.to_string().repeat(64)
}

struct Fixture {
    plan: Plan,
    baseline: Vec<f64>,
    applied: Vec<f64>,
}

fn fixture() -> Fixture {
    let baseline = baseline();
    let applied = scaled(&baseline, 2.0);
    Fixture {
        plan: fixture_plan(),
        baseline,
        applied,
    }
}

fn config() -> RunConfig {
    RunConfig {
        point: "P01".to_string(),
        lane: 0,
        centers_hz: vec![100.0, 1_000.0, 10_000.0],
        tolerance_db: 2.0,
        min_compared_bands: 3,
        window_len: 4096,
    }
}

fn build_default(fixture: &Fixture) -> Result<RunBundle, ClosedLoopRunError> {
    let plan_sha = sha('a');
    let baseline_sha = sha('b');
    let applied_sha = sha('c');
    let inputs = RunInputs {
        plan: &fixture.plan,
        plan_sha256: &plan_sha,
        baseline_samples: &fixture.baseline,
        baseline_wav_sha256: &baseline_sha,
        applied_samples: &fixture.applied,
        applied_wav_sha256: &applied_sha,
        sample_rate_hz: FS,
    };
    build_run_bundle(&inputs, &config())
}

#[test]
fn builds_pass_bundle_with_four_deterministic_files() {
    let fixture = fixture();
    let first = build_default(&fixture).expect("bundle");
    assert_eq!(first.verdict(), Verdict::Pass);
    assert_eq!(first.report().compared_bands, 3);
    assert_eq!(first.report().violations, 0);
    let names: Vec<&str> = first.files().iter().map(|(name, _)| *name).collect();
    assert_eq!(
        names,
        vec![PREDICTED_FILE, OBSERVED_FILE, REPORT_FILE, MANIFEST_FILE]
    );
    for (_, bytes) in first.files() {
        assert!(bytes.ends_with(b"\n"));
        assert!(!bytes.is_empty());
    }
    assert_eq!(first.predicted_sha256().len(), 64);
    assert_eq!(first.report_sha256().len(), 64);
    assert_eq!(first.manifest_sha256().len(), 64);

    let second = build_default(&fixture).expect("second bundle");
    assert_eq!(first.predicted_bytes(), second.predicted_bytes());
    assert_eq!(first.observed_bytes(), second.observed_bytes());
    assert_eq!(first.report_bytes(), second.report_bytes());
    assert_eq!(first.manifest_bytes(), second.manifest_bytes());
    assert_eq!(first.manifest_sha256(), second.manifest_sha256());
}

#[test]
fn manifest_records_inputs_outputs_and_verdict() {
    let fixture = fixture();
    let bundle = build_default(&fixture).expect("bundle");
    let shas = (sha('a'), sha('b'), sha('c'));
    let manifest: serde_json::Value =
        serde_json::from_slice(bundle.manifest_bytes()).expect("manifest json");
    assert_eq!(manifest["schema"], "khaen-closed-loop-run-manifest/v1");
    assert_eq!(manifest["tool"], "khaen-closed-loop-run");
    assert_eq!(manifest["verdict"], "PASS");
    assert_eq!(manifest["point"], "P01");
    assert_eq!(manifest["tolerance_db"], 2.0);
    assert_eq!(manifest["window_len"], 4096);
    assert_eq!(manifest["centers_hz"].as_array().expect("centers").len(), 3);
    assert_eq!(manifest["inputs"]["plan_sha256"], shas.0.as_str());
    assert_eq!(manifest["inputs"]["baseline_wav_sha256"], shas.1.as_str());
    assert_eq!(manifest["inputs"]["applied_wav_sha256"], shas.2.as_str());
    assert_eq!(
        manifest["outputs"]["report_json_sha256"]
            .as_str()
            .expect("sha"),
        bundle.report_sha256()
    );
    let report: serde_json::Value =
        serde_json::from_slice(bundle.report_bytes()).expect("report json");
    assert_eq!(report["verdict"], "PASS");
}

#[test]
fn unmatched_gain_is_fail_but_bundle_is_still_built() {
    let mut fixture = fixture();
    fixture.applied = fixture.baseline.clone();
    let bundle = build_default(&fixture).expect("bundle");
    assert_eq!(bundle.verdict(), Verdict::Fail);
    assert_eq!(bundle.report().violations, 3);
    let manifest: serde_json::Value =
        serde_json::from_slice(bundle.manifest_bytes()).expect("manifest");
    assert_eq!(manifest["verdict"], "FAIL");
}

#[test]
fn silent_applied_recording_is_incomplete_not_fail() {
    let mut fixture = fixture();
    fixture.applied = vec![0.0; LEN];
    let bundle = build_default(&fixture).expect("bundle");
    assert_eq!(bundle.verdict(), Verdict::Incomplete);
    assert_eq!(bundle.report().compared_bands, 0);
    assert_eq!(bundle.report().missing_bands, 3);
}

#[test]
fn alignment_and_input_errors_are_stable_codes() {
    let fixture = fixture();
    let plan_sha = sha('a');
    let baseline_sha = sha('b');
    let applied_sha = sha('c');

    // 方案采样率与录音不一致。
    let mut other_rate = fixture_plan();
    other_rate.sample_rate = 96_000;
    let inputs = RunInputs {
        plan: &other_rate,
        plan_sha256: &plan_sha,
        baseline_samples: &fixture.baseline,
        baseline_wav_sha256: &baseline_sha,
        applied_samples: &fixture.applied,
        applied_wav_sha256: &applied_sha,
        sample_rate_hz: FS,
    };
    assert_eq!(
        build_run_bundle(&inputs, &config())
            .expect_err("rate mismatch")
            .code(),
        ALIGNMENT
    );

    // sha256 不是 64 位十六进制。
    let bad_sha = "not-a-sha";
    let inputs = RunInputs {
        plan: &fixture.plan,
        plan_sha256: bad_sha,
        baseline_samples: &fixture.baseline,
        baseline_wav_sha256: &baseline_sha,
        applied_samples: &fixture.applied,
        applied_wav_sha256: &applied_sha,
        sample_rate_hz: FS,
    };
    assert_eq!(
        build_run_bundle(&inputs, &config())
            .expect_err("bad sha")
            .code(),
        INPUT
    );

    // 录音太短。
    let short = vec![0.1; 100];
    let inputs = RunInputs {
        plan: &fixture.plan,
        plan_sha256: &plan_sha,
        baseline_samples: &short,
        baseline_wav_sha256: &baseline_sha,
        applied_samples: &short,
        applied_wav_sha256: &applied_sha,
        sample_rate_hz: FS,
    };
    assert_eq!(
        build_run_bundle(&inputs, &config())
            .expect_err("short")
            .code(),
        INPUT
    );

    // 采样率非有限。
    let inputs = RunInputs {
        plan: &fixture.plan,
        plan_sha256: &plan_sha,
        baseline_samples: &fixture.baseline,
        baseline_wav_sha256: &baseline_sha,
        applied_samples: &fixture.applied,
        applied_wav_sha256: &applied_sha,
        sample_rate_hz: f64::NAN,
    };
    assert_eq!(
        build_run_bundle(&inputs, &config())
            .expect_err("nan rate")
            .code(),
        INPUT
    );
}

#[test]
fn invalid_config_and_plan_are_stable_codes() {
    let fixture = fixture();
    let plan_sha = sha('a');
    let baseline_sha = sha('b');
    let applied_sha = sha('c');
    let inputs = RunInputs {
        plan: &fixture.plan,
        plan_sha256: &plan_sha,
        baseline_samples: &fixture.baseline,
        baseline_wav_sha256: &baseline_sha,
        applied_samples: &fixture.applied,
        applied_wav_sha256: &applied_sha,
        sample_rate_hz: FS,
    };

    for point in ["", "P 01", "p/01"] {
        let mut bad = config();
        bad.point = point.to_string();
        assert_eq!(
            build_run_bundle(&inputs, &bad)
                .expect_err("bad point")
                .code(),
            CONFIG,
            "point {point:?}"
        );
    }
    for tolerance in [0.0, -1.0, f64::NAN] {
        let mut bad = config();
        bad.tolerance_db = tolerance;
        assert_eq!(
            build_run_bundle(&inputs, &bad)
                .expect_err("bad tolerance")
                .code(),
            CONFIG
        );
    }
    let mut bad = config();
    bad.min_compared_bands = 0;
    assert_eq!(
        build_run_bundle(&inputs, &bad)
            .expect_err("bad min bands")
            .code(),
        CONFIG
    );
    for window in [1_000usize, 64, 131_072] {
        let mut bad = config();
        bad.window_len = window;
        assert_eq!(
            build_run_bundle(&inputs, &bad)
                .expect_err("bad window")
                .code(),
            CONFIG
        );
    }

    // 选中不存在的 lane。
    let mut bad = config();
    bad.lane = 7;
    assert_eq!(
        build_run_bundle(&inputs, &bad)
            .expect_err("lane out of range")
            .code(),
        PLAN
    );

    // 非恒等矩阵段：预测电平拒绝，运行包给方案错误。
    let mut plan = fixture_plan();
    plan.matrix = Some(khaen_core::plan::MatrixSection {
        rows: 1,
        cols: 1,
        decimation: 1,
        interpolation: 1,
        taps: 1,
        coeffs: vec![0.5],
    });
    let inputs = RunInputs {
        plan: &plan,
        plan_sha256: &plan_sha,
        baseline_samples: &fixture.baseline,
        baseline_wav_sha256: &baseline_sha,
        applied_samples: &fixture.applied,
        applied_wav_sha256: &applied_sha,
        sample_rate_hz: FS,
    };
    assert_eq!(
        build_run_bundle(&inputs, &config())
            .expect_err("non identity")
            .code(),
        PLAN
    );
}
