//! T10 private integration tests: 参数边界/E_FIR_* 失败码/确定性/f32 导出/支撑与 API 契约。
//!
//! 运行（integrator 注册前）：rustc --test 手工编译执行，见 evidence/T10/manifest。

use khaen_designer::linear::{
    delay_samples, design, design_checked, design_checked_with, effective_support, magnitude_db_at,
    response_at, symmetry_error, validate, verify_linear, verify_linear_with, AnalysisOptions,
    LinearError, LinearSpec, Symmetry, WindowKind, MAX_TAPS, MIN_TAPS,
};

#[test]
fn t10_contract_min_taps_design_ok_and_delay() {
    let spec = LinearSpec::lowpass(48_000.0, MIN_TAPS, 2_000.0, 5_000.0).with_attenuation(20.0);
    assert!(validate(&spec).is_ok());
    let d = design(&spec).expect("8-tap design must construct");
    assert_eq!(d.taps(), MIN_TAPS);
    assert_eq!(delay_samples(MIN_TAPS), 3.5);
    assert!((d.delay_samples - 3.5).abs() < 1e-12);
    assert!((d.delay_ms - 3.5 / 48_000.0 * 1000.0).abs() < 1e-12);
}

#[test]
fn t10_contract_max_taps_design_ok() {
    let spec = LinearSpec::lowpass(48_000.0, MAX_TAPS, 18_000.0, 20_000.0);
    let d = design(&spec).expect("16384-tap design must construct");
    assert_eq!(d.taps(), MAX_TAPS);
    assert!((d.delay_samples - 8191.5).abs() < 1e-12);
    assert!((d.delay_ms - 170.656_25).abs() < 1e-9);
}

#[test]
fn t10_contract_taps_out_of_range_rejected() {
    for taps in [MIN_TAPS - 1, MAX_TAPS + 1] {
        let spec = LinearSpec::lowpass(48_000.0, taps, 18_000.0, 20_000.0);
        let err = validate(&spec).expect_err("taps out of range must fail");
        assert_eq!(err.code(), "E_FIR_BAND");
    }
}

#[test]
fn t10_contract_band_and_transition_validation_codes() {
    let mut spec = LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0);
    spec.passband_hz[1] = 20_100.0;
    assert_eq!(validate(&spec).unwrap_err().code(), "E_FIR_BAND");

    let mut spec = LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0);
    spec.stopband_hz[0] = 18_010.0; // 过渡带 10 Hz < 30 Hz
    assert_eq!(validate(&spec).unwrap_err().code(), "E_FIR_BAND");

    let mut spec = LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0);
    spec.stopband_hz[1] = 23_000.0; // 阻带上界必须 = 0.499fs
    assert_eq!(validate(&spec).unwrap_err().code(), "E_FIR_BAND");

    let lp_antisym = LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0)
        .with_symmetry(Symmetry::Antisymmetric);
    assert_eq!(validate(&lp_antisym).unwrap_err().code(), "E_FIR_BAND");

    let bh_beta = LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0)
        .with_window(WindowKind::BlackmanHarris)
        .with_kaiser_beta(5.0);
    assert_eq!(validate(&bh_beta).unwrap_err().code(), "E_FIR_BAND");
}

#[test]
fn t10_contract_declared_delay_code() {
    let bad = LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0).with_declared_delay(256.0);
    assert_eq!(validate(&bad).unwrap_err().code(), "E_FIR_DELAY");
    let ok = LinearSpec::lowpass(48_000.0, 512, 18_000.0, 20_000.0).with_declared_delay(255.5);
    assert!(validate(&ok).is_ok());
}

#[test]
fn t10_contract_budget_code() {
    // Kaiser V13：A=80/Δf=2000 要求 taps >= 122。
    let too_few = LinearSpec::lowpass(48_000.0, 120, 18_000.0, 20_000.0);
    let err = design(&too_few).unwrap_err();
    assert_eq!(err.code(), "E_FIR_BUDGET");
    assert!(err.detail().contains("122"), "detail: {}", err.detail());
    // BH 固定旁瓣约 −92 dB，不能承诺 100 dB。
    let bh_too_much = LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0)
        .with_window(WindowKind::BlackmanHarris)
        .with_attenuation(100.0);
    assert_eq!(design(&bh_too_much).unwrap_err().code(), "E_FIR_BUDGET");
}

#[test]
fn t10_contract_hp_nyquist_parity_code() {
    let sym_even = LinearSpec::highpass_to_nyquist(48_000.0, 512, 5_000.0, 5_500.0);
    assert_eq!(validate(&sym_even).unwrap_err().code(), "E_FIR_HP_NYQ");
    let anti_odd = LinearSpec::highpass_to_nyquist(48_000.0, 513, 5_000.0, 5_500.0)
        .with_symmetry(Symmetry::Antisymmetric);
    assert_eq!(validate(&anti_odd).unwrap_err().code(), "E_FIR_HP_NYQ");
    // 结构正确（symmetric 奇长 = Type I，antisymmetric 偶长 = Type IV）可到 0.5fs。
    let sym_odd = LinearSpec::highpass_to_nyquist(48_000.0, 513, 5_000.0, 5_500.0);
    assert!(validate(&sym_odd).is_ok());
    let anti_even = LinearSpec::highpass_to_nyquist(48_000.0, 512, 5_000.0, 5_500.0)
        .with_symmetry(Symmetry::Antisymmetric);
    assert!(validate(&anti_even).is_ok());
}

#[test]
fn t10_contract_verify_taps_and_numeric_codes() {
    let spec = LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0);
    let d = design(&spec).expect("design");
    let short = &d.coeffs[..510];
    assert_eq!(
        verify_linear(&spec, short).unwrap_err().code(),
        "E_FIR_TAPS"
    );

    let mut bad = d.coeffs.clone();
    bad[10] = f64::NAN;
    assert_eq!(
        verify_linear(&spec, &bad).unwrap_err().code(),
        "E_FIR_NUMERIC"
    );
}

#[test]
fn t10_contract_verify_detects_bad_symmetry_and_reference() {
    let spec = LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0);
    let d = design(&spec).expect("design");
    // 非对称（脉冲在 n=0）→ 判据 1 失败。
    let mut asym = vec![0.0_f64; 511];
    asym[0] = 1.0;
    let v = verify_linear(&spec, &asym).expect("verify");
    assert!(!v.checks.symmetry);
    assert!(!v.passed());

    // 整体增益 2× → 参考归一化判据 6 失败（设计器不偷偷加增益）。
    let scaled: Vec<f64> = d.coeffs.iter().map(|x| x * 2.0).collect();
    let v = verify_linear(&spec, &scaled).expect("verify");
    assert!(!v.checks.reference);
    assert!(!v.passed());
}

#[test]
fn t10_contract_design_checked_rejects_infeasible_bad_gain() {
    let spec = LinearSpec::lowpass(48_000.0, 32, 18_000.0, 20_000.0).with_attenuation(30.0);
    let err = design_checked_with(&spec, &AnalysisOptions::compact(spec.taps)).unwrap_err();
    assert!(
        err.code() == "E_FIR_BUDGET" || err.code() == "E_FIR_PHASE_CLASS",
        "got {}: {err}",
        err.code()
    );
}

#[test]
fn t10_contract_design_checked_accepts_feasible() {
    let spec = LinearSpec::highpass(48_000.0, 513, 5_000.0, 5_500.0).with_attenuation(60.0);
    let d = design_checked_with(&spec, &AnalysisOptions::compact(spec.taps)).expect("feasible HP");
    let v =
        verify_linear_with(&spec, &d.coeffs, &AnalysisOptions::compact(spec.taps)).expect("verify");
    assert!(v.passed(), "failures: {:?}", v.failures);
}

#[test]
fn t10_contract_deterministic_byte_identical() {
    let spec = LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0);
    let a = design(&spec).expect("a");
    let b = design(&spec).expect("b");
    assert_eq!(a.coeffs, b.coeffs);
    assert_eq!(a.delay_samples, b.delay_samples);
    assert_eq!(a.spec, b.spec);
}

#[test]
fn t10_contract_f32_quantization() {
    let spec = LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0);
    let d = design(&spec).expect("design");
    let f32s = d.coeffs_f32();
    assert_eq!(f32s.len(), 511);
    let mut max_err = 0.0_f64;
    for (a, b) in d.coeffs.iter().zip(f32s.iter()) {
        max_err = max_err.max((f64::from(*b) - a).abs());
    }
    assert!(max_err <= 1e-7 * d.peak_abs);
}

#[test]
fn t10_contract_effective_support_and_dc() {
    let spec = LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0);
    let d = design(&spec).expect("design");
    assert!((d.sum_coeffs - 1.0).abs() < 1e-12, "LP DC 归一必须是 1");
    assert!((response_at(&d.coeffs, 48_000.0, 0.0).magnitude() - 1.0).abs() < 1e-12);
    assert!((magnitude_db_at(&d.coeffs, 48_000.0, 0.0)).abs() <= 0.01);

    let (first, last, eff) = d.effective_support();
    assert!(
        first <= 4 && last >= 506,
        "expected near-full support, got {first}..{last}"
    );
    assert!(eff >= 500 && eff <= 511);
    assert_eq!(effective_support(&d.coeffs, 1e-6), (first, last, eff));

    let hp = LinearSpec::highpass(48_000.0, 513, 5_000.0, 5_500.0).with_attenuation(60.0);
    let dh = design(&hp).expect("hp");
    assert!(dh.sum_coeffs.abs() < 1e-12, "HP DC 必须为 0");
    let anti = LinearSpec::highpass(48_000.0, 512, 5_000.0, 5_500.0)
        .with_attenuation(60.0)
        .with_symmetry(Symmetry::Antisymmetric);
    let da = design(&anti).expect("anti");
    assert_eq!(symmetry_error(&da.coeffs, Symmetry::Antisymmetric), 0.0);
    assert!(da.sum_coeffs.abs() < 1e-12);
}

#[test]
fn t10_contract_error_codes_are_spec_codes() {
    let codes = [
        "E_FIR_BAND",
        "E_FIR_DELAY",
        "E_FIR_TAPS",
        "E_FIR_BUDGET",
        "E_FIR_NUMERIC",
        "E_FIR_PHASE_CLASS",
        "E_FIR_HP_NYQ",
        "E_FIR_REF",
    ];
    // 通过实际构造确认关键码可达（其余经 verify 报告路径覆盖）。
    let bad = LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0).with_declared_delay(0.0);
    assert!(codes.contains(&validate(&bad).unwrap_err().code()));
    let too_few = LinearSpec::lowpass(48_000.0, 120, 18_000.0, 20_000.0);
    assert!(codes.contains(&design(&too_few).unwrap_err().code()));
    let nyq = LinearSpec::highpass_to_nyquist(48_000.0, 512, 5_000.0, 5_500.0);
    assert!(codes.contains(&validate(&nyq).unwrap_err().code()));
    // LinearError Display 必须带 code。
    let e: LinearError = design(&too_few).unwrap_err();
    assert!(format!("{e}").starts_with("E_FIR_BUDGET"));
}

#[test]
fn t10_contract_design_checked_normative_small() {
    // normative 网格设计可接受（taps=255 → N=8192，验证成本可接受）。
    let spec = LinearSpec::lowpass(48_000.0, 255, 18_000.0, 20_000.0).with_attenuation(60.0);
    let d = design_checked(&spec).expect("normative design_checked must accept feasible design");
    assert_eq!(d.taps(), 255);
    let v = verify_linear(&spec, &d.coeffs).expect("verify");
    assert!(v.passed(), "failures: {:?}", v.failures);
    assert_eq!(v.grid_points, 8192);
}
