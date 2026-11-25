//! T10 private integration tests: 规范 §3.1 判据 1–7 的独立响应验证（normative 网格，DoD 证据）。
//!
//! 运行（integrator 注册前）：rustc --test 手工编译执行，见 evidence/T10/manifest。
//! Lead 2026-11-17 裁决 A（HP 判据 5 镜像 + 整段阻带峰值 < −A_s）已在本文件断言中体现。

use tspi_fir_designer::linear::{
    design, design_checked_with, verify_linear, verify_linear_with, AnalysisOptions, LinearSpec,
    Symmetry, WindowKind, GROUP_DELAY_TOLERANCE_SAMPLES, PASSBAND_RIPPLE_MARGIN_DB,
    STOPBAND_MARGIN_DB,
};

fn print_verification(tag: &str, v: &tspi_fir_designer::linear::LinearVerification) {
    println!(
        "[T10 {tag}] fs={} taps={} D={} ms={:.6} grid={} sym_err={:.3e} pass_ripple={:.6}dB \
         stop_max={:.3}dB gd=[{:.9},{:.9}] gd1k={:.9} trans_edge={:?} ref={:.9}dB nyq={:.3}dB \
         checks={:?}",
        v.fs,
        v.taps,
        v.delay_samples,
        v.delay_ms,
        v.grid_points,
        v.symmetry_max_abs_error,
        v.passband.ripple_db(),
        v.stopband.max_db,
        v.group_delay.min_samples,
        v.group_delay.max_samples,
        v.group_delay.at_1khz_samples,
        v.transition.edge_hz,
        v.reference.measured_gain_db,
        v.nyquist_mag_db,
        v.checks
    );
}

#[test]
fn t10_dod_lowpass_511_normative_grid() {
    let spec = LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0);
    let d = design(&spec).expect("LP design");
    let v = verify_linear(&spec, &d.coeffs).expect("LP verify");
    print_verification("LP511", &v);
    assert!(v.passed(), "LP failures: {:?}", v.failures);
    assert_eq!(v.grid_points, 8192, "规范 §1.2 N=max(8192,8L)");
    assert_eq!(v.taps, 511);
    assert!((v.delay_samples - 255.0).abs() < 1e-12);
    assert!((v.delay_ms - 5.3125).abs() < 1e-12);
    // 判据 1：对称性。
    assert!(v.symmetry_max_abs_error <= 1e-7 * v.max_abs_h);
    // 判据 2：常数 GD。
    assert!(
        (v.group_delay.min_samples - 255.0).abs() <= GROUP_DELAY_TOLERANCE_SAMPLES
            && (v.group_delay.max_samples - 255.0).abs() <= GROUP_DELAY_TOLERANCE_SAMPLES
    );
    // 判据 3/4：通带纹波、阻带衰减（含 margin）。
    assert!(v.passband.ripple_db() <= spec.ripple_db + PASSBAND_RIPPLE_MARGIN_DB);
    assert!(v.stopband.max_db <= -(spec.attenuation_db - STOPBAND_MARGIN_DB));
    // 判据 5：LP 首次达到 −A_s 的频点 <= f_s。
    let edge = v.transition.edge_hz.expect("LP transition edge must exist");
    assert!(edge > spec.passband_hz[1] && edge <= spec.stopband_hz[0] + 1e-6);
    // 判据 6：参考。
    assert!((v.reference.measured_gain_db - 0.0).abs() <= 0.01);
    assert_eq!(v.reference.hz, 0.0);
    // 判据 7（V1 的偶长 H(0.5fs)=0 由 linear_vectors 的 512-tap 用例覆盖；本用例 511 奇长
    // 的 0.5fs 属阻带，按判据 4 口径检查）。
    assert!((v.delay_samples - (v.taps as f64 - 1.0) / 2.0).abs() < 1e-12);
    assert!(
        v.nyquist_mag_db <= -(spec.attenuation_db - STOPBAND_MARGIN_DB),
        "LP H(0.5fs) 应在阻带内，实际 {} dB",
        v.nyquist_mag_db
    );
}

#[test]
fn t10_dod_highpass_symmetric_513_normative_grid() {
    // A_s=60 有余量：整段阻带峰值严格 < −A_s（Lead 裁决 A 要求）。
    let spec = LinearSpec::highpass(48_000.0, 513, 5_000.0, 5_500.0).with_attenuation(60.0);
    let d = design(&spec).expect("HP design");
    let v = verify_linear(&spec, &d.coeffs).expect("HP verify");
    print_verification("HP513sym", &v);
    assert!(v.passed(), "HP sym failures: {:?}", v.failures);
    assert!((v.delay_samples - 256.0).abs() < 1e-12);
    assert!(v.group_delay.class_constant);
    assert!((v.group_delay.at_1khz_samples - 256.0).abs() <= GROUP_DELAY_TOLERANCE_SAMPLES);
    assert!(v.passband.ripple_db() <= spec.ripple_db + PASSBAND_RIPPLE_MARGIN_DB);
    // 判据 4 与 Lead 裁决 A 的严格项。
    assert!(v.stopband.max_db <= -(spec.attenuation_db - STOPBAND_MARGIN_DB));
    assert!(
        v.stopband.max_db < -spec.attenuation_db,
        "HP 整段阻带峰值必须严格 < −A_s（ruling A）"
    );
    // 判据 5：HP 穿越点 >= f_s − tol（ruling A 镜像）。
    let edge = v.transition.edge_hz.expect("HP transition edge must exist");
    assert!(
        edge >= spec.stopband_hz[1] - 1e-6,
        "HP edge {edge} must be >= f_s {}",
        spec.stopband_hz[1]
    );
    assert!((v.reference.measured_gain_db - 0.0).abs() <= 0.01);
    // 判据 9 相关：通带未到 Nyquist，不强制 H(π)!=0；报告即可。
    assert!(v.nyquist_mag_db.is_finite());
}

#[test]
fn t10_dod_highpass_antisymmetric_512_normative_grid() {
    let spec = LinearSpec::highpass(48_000.0, 512, 5_000.0, 5_500.0)
        .with_attenuation(60.0)
        .with_symmetry(Symmetry::Antisymmetric);
    let d = design(&spec).expect("HP antisym design");
    let v = verify_linear(&spec, &d.coeffs).expect("HP antisym verify");
    print_verification("HP512anti", &v);
    assert!(v.passed(), "HP antisym failures: {:?}", v.failures);
    assert!((v.delay_samples - 255.5).abs() < 1e-12);
    assert_eq!(v.checks.symmetry, true);
    assert!(v.stopband.max_db < -spec.attenuation_db);
    assert!(d.sum_coeffs.abs() < 1e-12, "antisymmetric H(0)=0");
}

#[test]
fn t10_dod_highpass_to_nyquist_type_iv() {
    // 判据 9：antisymmetric 偶数 taps（Type IV）允许通带到 0.5fs（H(π)!=0）。
    let spec = LinearSpec::highpass_to_nyquist(48_000.0, 512, 5_000.0, 5_500.0)
        .with_attenuation(60.0)
        .with_symmetry(Symmetry::Antisymmetric);
    let d = design(&spec).expect("HP to Nyquist design");
    let v = verify_linear(&spec, &d.coeffs).expect("HP to Nyquist verify");
    print_verification("HP512antiNyq", &v);
    assert!(v.passed(), "HP to Nyquist failures: {:?}", v.failures);
    assert!(
        v.nyquist_mag_db > -1.0,
        "Type IV HP must keep |H(0.5fs)|≈0 dB, got {} dB",
        v.nyquist_mag_db
    );
    assert!(v.passband.ripple_db() <= spec.ripple_db + PASSBAND_RIPPLE_MARGIN_DB);
}

#[test]
fn t10_dod_blackman_harris_window_compact() {
    let spec = LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0)
        .with_window(WindowKind::BlackmanHarris);
    let d = design_checked_with(&spec, &AnalysisOptions::compact(spec.taps)).expect("BH design");
    let v = verify_linear_with(&spec, &d.coeffs, &AnalysisOptions::compact(spec.taps))
        .expect("BH verify");
    print_verification("LP511BH", &v);
    assert!(v.passed(), "BH failures: {:?}", v.failures);
    assert!(v.stopband.max_db <= -(spec.attenuation_db - STOPBAND_MARGIN_DB));
}

#[test]
fn t10_dod_96k_lowpass_compact() {
    let spec = LinearSpec::lowpass(96_000.0, 1023, 20_000.0, 22_000.0);
    let d = design(&spec).expect("96k design");
    let v = verify_linear_with(&spec, &d.coeffs, &AnalysisOptions::compact(spec.taps))
        .expect("96k verify");
    print_verification("LP96k", &v);
    assert!(v.passed(), "96k failures: {:?}", v.failures);
    assert!((v.delay_samples - 511.0).abs() < 1e-12);
    assert!(v.stopband.max_db <= -(spec.attenuation_db - STOPBAND_MARGIN_DB));
    assert!((v.delay_ms - 5.322_916_666_666_66).abs() < 1e-9);
}
