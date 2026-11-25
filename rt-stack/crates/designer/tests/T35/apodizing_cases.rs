// T35 私有测试：apodizing 低前振铃族（可算窗/目标、前后振铃/阻带/相位量化、家族互斥）。
//
// 本文件由 crates/designer/src/apodizing.rs 的 #[cfg(test)] 子模块通过 include! 编译执行
// （见 tests/T35/README.md；不改 integrator 独占的 public_registration.rs）。
// 运行：cargo test -p khaen-designer apodizing
//
// T04 参考向量：evidence/T04/recompute-vectors.mjs V16（apodizing/brickwall/边界）。

// use super::* 由外层 #[cfg(test)] 模块提供（include! 共享作用域）

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

fn approx(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

#[track_caller]
fn assert_approx(a: f64, b: f64, tol: f64, what: &str) {
    assert!(approx(a, b, tol), "{what}: {a} vs {b} (tol {tol})");
}

fn expect_code<T: std::fmt::Debug>(result: Result<T, ApodizingError>, code: &str) {
    match result {
        Ok(v) => panic!("期望错误 {code}，实际成功 {v:?}"),
        Err(e) => assert_eq!(e.code(), code, "错误码不符：{e}"),
    }
}

/// 规范 §3.4 参考构造的直接求和式（独立于模块 FFT 实现，用于小 N 逐点对照）。
fn direct_synthesis(target: &ApodizingTarget, taps: usize, window: &[f64], n: usize) -> Vec<f64> {
    let fs = target.fs;
    let d = (taps - 1) as f64 / 2.0;
    let mut h = vec![0.0_f64; taps];
    for (idx, hi) in h.iter_mut().enumerate() {
        let mut acc = target.amplitude_at(0.0);
        for k in 1..n / 2 {
            let v = target.amplitude_at(k as f64 * fs / n as f64);
            acc += 2.0 * v * (2.0 * PI * (k as f64) * (idx as f64 - d) / (n as f64)).cos();
        }
        acc += target.amplitude_at(fs / 2.0) * (PI * (idx as f64 - d)).cos();
        *hi = acc / n as f64;
    }
    for (h, w) in h.iter_mut().zip(window.iter()) {
        *h *= *w;
    }
    let sum: f64 = h.iter().sum();
    for h in h.iter_mut() {
        *h /= sum;
    }
    h
}

/// brickwall 式对照构造（T04 V16 brickwall）：Kaiser 窗 sinc，fc=(通带边)，DC 归一。
fn kaiser_sinc_lowpass(taps: usize, fc: f64, fs: f64, beta: f64) -> Vec<f64> {
    let d = (taps - 1) as f64 / 2.0;
    let w = kaiser_window_values(taps, beta);
    let r = 2.0 * fc / fs;
    let mut h: Vec<f64> = (0..taps)
        .map(|n| r * sinc_normalized(r * (n as f64 - d)) * w[n])
        .collect();
    let sum: f64 = h.iter().sum();
    for x in h.iter_mut() {
        *x /= sum;
    }
    h
}

fn ref_spec() -> ApodizingSpec {
    ApodizingSpec::reference_preset_48k()
}

fn ref_design() -> ApodizingDesign {
    design(&ref_spec()).expect("reference design")
}

// ---------------------------------------------------------------------------
// V16 参考构造 / 边界
// ---------------------------------------------------------------------------

#[test]
fn t35_reference_preset_matches_v16_parameters() {
    let spec = ref_spec();
    assert_eq!(spec.fs, 48_000.0);
    assert_eq!(spec.taps, 1_024);
    assert_eq!(spec.passband_hz, [20.0, 18_000.0]);
    assert_eq!(spec.ripple_db, 0.5);
    assert_eq!(spec.nyquist_attenuation_db, 30.0);
    assert_eq!(spec.window, ApodizingWindowKind::Kaiser);
    assert_eq!(spec.kaiser_beta, None);
    assert_eq!(spec.pre_ring_bound_db, -45.0);
    assert_eq!(spec.fp_limit_hz(), 20_400.0);
    let v = validate(&spec).expect("validate");
    assert_eq!(v.kaiser_beta, Some(8.96));
    assert_eq!(v.delay_samples, 511.5);
    assert_approx(v.delay_ms, 10.656_25, 1e-12, "delay_ms");
    assert_eq!(v.target.passband_edge_hz, 18_000.0);
    assert_eq!(v.target.floor_amplitude, MAG_FLOOR_AMPLITUDE);
}

#[test]
fn t35_reference_design_reproduces_v16_metrics() {
    let spec = ref_spec();
    let d = ref_design();
    assert_eq!(d.taps(), 1_024);
    assert_approx(d.delay_samples, 511.5, 1e-12, "D");
    assert_approx(d.delay_ms, 10.656_25, 1e-12, "delay_ms");
    assert_approx(d.sum_coeffs, 1.0, 1e-12, "DC sum");
    assert!(symmetry_error(&d.coeffs) <= SYMMETRY_TOLERANCE * d.peak_abs);
    let v = verify_apodizing_with(&spec, &d.coeffs, &AnalysisOptions::compact(spec.taps))
        .expect("verify");
    assert!(v.passed(), "failures: {:?}", v.failures);
    // T04 V16 apodizing 参考值（pre/post -54.6360800281、pre_energy -55.4196553526、
    // Nyquist 带 -36.0084895582、GD 511.5、通带 ripple 5.7e-9）。
    assert_approx(
        v.ringing.pre_db.unwrap(),
        -54.636_080_028_118,
        1e-4,
        "V16 pre_ring",
    );
    assert_approx(
        v.ringing.post_db.unwrap(),
        -54.636_080_028_118,
        1e-4,
        "V16 post_ring",
    );
    assert_approx(
        v.ringing.pre_energy_db.unwrap(),
        -55.419_655_352_642,
        1e-4,
        "V16 pre_energy",
    );
    assert_approx(
        v.stopband.max_db,
        -36.008_489_558_219,
        1e-3,
        "V16 nyquist band max",
    );
    assert_approx(v.group_delay.at_1khz_samples, 511.5, 1e-6, "V16 GD 1 kHz");
    // V16 参考构造在 [20,17800] 的 ripple 为 5.7e-9 dB；本报告按声明通带 [20, f_p] 含
    // 上边界 f_p=18 kHz 的过渡肩，实测 ~5.6e-4 dB（仍远优于 0.5 dB 声明）。
    assert!(
        v.passband.ripple_db() < 1e-3,
        "passband ripple {} dB (declared 0.5)",
        v.passband.ripple_db()
    );
    assert!(v.checks.f32_quantization, "f32 quantization report");
}

#[test]
fn t35_synthesis_reproduces_v16_and_direct_formula() {
    let target = ApodizingTarget::new(48_000.0, 18_000.0);
    let window = apodizing_window_values(ApodizingWindowKind::Kaiser, 64, None).unwrap();
    let fft = synthesize_apodizing(&target, 64, &window, 1_024).expect("fft synthesis");
    let direct = direct_synthesis(&target, 64, &window, 1_024);
    let peak = fft.iter().fold(0.0_f64, |a, h| a.max(h.abs()));
    let mut max_diff = 0.0_f64;
    for (a, b) in fft.iter().zip(direct.iter()) {
        max_diff = max_diff.max((a - b).abs());
    }
    assert!(
        max_diff <= 1e-11 * peak,
        "FFT vs direct max_diff={max_diff} peak={peak}"
    );
}

#[test]
fn t35_v16_pre_ring_boundary_fp_085_and_090_nyquist() {
    let window = apodizing_window_values(ApodizingWindowKind::Kaiser, 1_024, None).unwrap();
    let mut pre = Vec::new();
    for fp in [20_400.0, 21_600.0] {
        let target = ApodizingTarget::new(48_000.0, fp);
        let coeffs = synthesize_apodizing(&target, 1_024, &window, SYNTHESIS_POINTS).unwrap();
        let ring = ring_metrics(&coeffs, 48_000.0);
        pre.push(ring.pre_db.unwrap());
    }
    // T04 V16 边界复算：0.85*Nyq -> -49.90 dB；0.90*Nyq -> -37.87 dB（> -45 → 不达 apodizing 门）
    assert_approx(
        pre[0],
        -49.896_040_212_934,
        0.01,
        "V16 boundary 0.85*Nyq pre_ring",
    );
    assert_approx(
        pre[1],
        -37.869_074_657_955,
        0.01,
        "V16 boundary 0.90*Nyq pre_ring",
    );
    assert!(pre[0] <= -45.0);
    assert!(pre[1] > -45.0);
}

#[test]
fn t35_brickwall_proxy_is_rejected_by_family_checks() {
    let spec = ref_spec();
    let coeffs = kaiser_sinc_lowpass(1_024, 19_750.0, 48_000.0, 8.96);
    let v = verify_apodizing_with(&spec, &coeffs, &AnalysisOptions::compact(spec.taps))
        .expect("verify brickwall proxy");
    assert!(!v.passed());
    assert!(
        !v.checks.pre_ring,
        "brickwall pre-ring must fail -45 dB gate"
    );
    assert!(
        !v.checks.family,
        "brickwall 0.45fs <= -60 dB must fail family rule"
    );
    assert!(v.family.not_brickwall == false);
    assert_approx(
        v.ringing.pre_db.unwrap(),
        -30.332_243_183_032,
        0.01,
        "V16 brickwall pre_ring",
    );
    assert_approx(
        v.ringing.pre_energy_db.unwrap(),
        -24.607_563_615_770,
        0.01,
        "V16 brickwall pre_energy",
    );
    expect_code(verify_apodizing_checked(&spec, &coeffs), "E_FIR_FAMILY");
}

// ---------------------------------------------------------------------------
// 参数 / 家族边界校验
// ---------------------------------------------------------------------------

#[test]
fn t35_validate_accepts_fp_at_085_nyquist_and_rejects_above() {
    let ok = ApodizingSpec::new(48_000.0, 1_024, 20_400.0);
    assert!(validate(&ok).is_ok(), "f_p = 0.85*Nyq 必须接受");
    for fp in [20_400.001, 20_401.0, 21_600.0] {
        let bad = ApodizingSpec::new(48_000.0, 1_024, fp);
        expect_code(validate(&bad), "E_FIR_APO_FP");
    }
}

#[test]
fn t35_validate_rejects_bad_parameters() {
    let base = ref_spec();
    let mut s = base.clone();
    s.fs = f64::NAN;
    expect_code(validate(&s), "E_FIR_BAND");
    let mut s = base.clone();
    s.fs = 0.0;
    expect_code(validate(&s), "E_FIR_BAND");
    let mut s = base.clone();
    s.fs = 4_000.0;
    expect_code(validate(&s), "E_FIR_BAND");
    let mut s = base.clone();
    s.taps = 7;
    expect_code(validate(&s), "E_FIR_BAND");
    let mut s = base.clone();
    s.taps = 16_385;
    expect_code(validate(&s), "E_FIR_BAND");
    let mut s = base.clone();
    s.passband_hz[0] = 19.0;
    expect_code(validate(&s), "E_FIR_BAND");
    let mut s = base.clone();
    s.passband_hz[1] = 20.0;
    expect_code(validate(&s), "E_FIR_BAND");
    let mut s = base.clone();
    s.ripple_db = -0.1;
    expect_code(validate(&s), "E_FIR_BAND");
    let mut s = base.clone();
    s.nyquist_attenuation_db = 29.9;
    expect_code(validate(&s), "E_FIR_BAND");
    let mut s = base.clone();
    s.window = ApodizingWindowKind::HannSquared;
    s.kaiser_beta = Some(8.96);
    expect_code(validate(&s), "E_FIR_BAND");
    let mut s = base.clone();
    s.pre_ring_bound_db = -40.0;
    expect_code(validate(&s), "E_FIR_BAND");
    // 更严的 bound 合法（不得静默放宽、只允许更严）。
    let mut strict = base.clone();
    strict.pre_ring_bound_db = -50.0;
    assert!(validate(&strict).is_ok());
}

#[test]
fn t35_taps_bounds_and_engineering_fs_range() {
    // 规范公共支撑边界 [8, 16384] + 工程 fs 范围 [8000, 384000]。
    assert!(validate(&ApodizingSpec::new(48_000.0, 8, 18_000.0)).is_ok());
    assert!(validate(&ApodizingSpec::new(48_000.0, 16_384, 18_000.0)).is_ok());
    assert!(validate(&ApodizingSpec::new(8_000.0, 128, 3_000.0)).is_ok());
    assert!(validate(&ApodizingSpec::new(96_000.0, 1_024, 36_000.0)).is_ok());
    assert!(validate(&ApodizingSpec::new(384_000.0, 1_024, 100_000.0)).is_ok());
    assert!(validate(&ApodizingSpec::new(7_999.0, 1_024, 18_000.0)).is_err());
    assert!(validate(&ApodizingSpec::new(384_001.0, 1_024, 18_000.0)).is_err());
}

// ---------------------------------------------------------------------------
// 可算窗 / 可算目标
// ---------------------------------------------------------------------------

#[test]
fn t35_window_definitions_are_computable_and_symmetric() {
    let k = kaiser_window_values(1_024, 8.96);
    assert_eq!(k[0], k[1_023]);
    assert_eq!(k[511], k[512]);
    assert!(k[511] > 0.99 && k[511] <= 1.0, "kaiser center {}", k[511]);
    assert!(k[0] > 0.0009 && k[0] < 0.0011, "kaiser edge {}", k[0]);
    assert!(symmetry_error(&k) < 1e-15);

    let h = hann_squared_window(5);
    assert_eq!(h.len(), 5);
    assert_eq!(h[0], 0.0);
    assert_approx(h[1], 0.25, 1e-15, "hann2 n=1");
    assert_approx(h[2], 1.0, 1e-15, "hann2 center");
    assert_approx(h[3], 0.25, 1e-15, "hann2 n=3");
    assert_eq!(h[4], 0.0);
    let big = hann_squared_window(1_024);
    assert_eq!(big[0], 0.0);
    assert_eq!(big[1_023], 0.0);
    assert_eq!(big[511], big[512]);
    assert!(symmetry_error(&big) < 1e-15);

    assert_eq!(
        apodizing_window_values(ApodizingWindowKind::Kaiser, 1_024, None).unwrap(),
        kaiser_window_values(1_024, 8.96)
    );
    assert_eq!(
        apodizing_window_values(ApodizingWindowKind::Kaiser, 1_024, Some(8.96)).unwrap(),
        apodizing_window_values(ApodizingWindowKind::Kaiser, 1_024, None).unwrap()
    );
    expect_code(
        apodizing_window_values(ApodizingWindowKind::HannSquared, 1_024, Some(8.96)),
        "E_FIR_BAND",
    );
}

#[test]
fn t35_window_kind_schema_roundtrip() {
    assert_eq!(ApodizingWindowKind::Kaiser.schema_name(), "kaiser");
    assert_eq!(
        ApodizingWindowKind::HannSquared.schema_name(),
        "hann_squared"
    );
    assert_eq!(
        ApodizingWindowKind::parse("kaiser"),
        Some(ApodizingWindowKind::Kaiser)
    );
    assert_eq!(
        ApodizingWindowKind::parse("hann_squared"),
        Some(ApodizingWindowKind::HannSquared)
    );
    assert_eq!(ApodizingWindowKind::parse("hann"), None);
}

#[test]
fn t35_target_is_magnitude_only_and_fs2_allowed_by_ruling() {
    let target = ApodizingTarget::new(48_000.0, 18_000.0);
    assert_eq!(target.amplitude_at(0.0), 1.0);
    assert_eq!(target.amplitude_at(18_000.0), 1.0);
    // Lead fs/2 裁决：magnitude-only 目标允许 =fs/2；合成时 Nyquist bin 取 floor（不 NaN/不拒收）。
    assert_eq!(target.amplitude_at(24_000.0), MAG_FLOOR_AMPLITUDE);
    assert_approx(
        target.magnitude_db_at(24_000.0),
        -120.0,
        1e-9,
        "fs/2 target dB",
    );
    assert_approx(
        target.magnitude_db_at(18_000.0),
        0.0,
        1e-12,
        "f_p target dB",
    );
    // 过渡区单调不增（升余弦），采样 2000 点检查。
    let mut prev = target.amplitude_at(20.0);
    for i in 0..=2_000 {
        let f = 20.0 + (24_000.0 - 20.0) * i as f64 / 2_000.0;
        let a = target.amplitude_at(f);
        assert!(
            a <= prev + 1e-12,
            "target not monotone at {f}: {a} > {prev}"
        );
        prev = a;
    }
}

#[test]
fn t35_synthesis_rejects_bad_options_and_targets() {
    let target = ApodizingTarget::new(48_000.0, 18_000.0);
    let window = kaiser_window_values(1_024, 8.96);
    expect_code(
        synthesize_apodizing(&target, 1_024, &window, 1_000),
        "E_FIR_BUDGET",
    );
    expect_code(
        synthesize_apodizing(&target, 1_024, &window, 512),
        "E_FIR_BUDGET",
    );
    expect_code(
        synthesize_apodizing(&target, 1_024, &window, 1_024),
        "E_FIR_BUDGET",
    );
    expect_code(
        synthesize_apodizing(&target, 1_024, &window[..1_023], SYNTHESIS_POINTS),
        "E_FIR_TAPS",
    );
    let bad_target = ApodizingTarget::new(48_000.0, 24_000.0);
    expect_code(
        synthesize_apodizing(&bad_target, 64, &kaiser_window_values(64, 8.96), 1_024),
        "E_FIR_BAND",
    );
}

// ---------------------------------------------------------------------------
// 设计 / 独立验证
// ---------------------------------------------------------------------------

#[test]
fn t35_reference_design_passes_compact_and_normative_checks() {
    let spec = ref_spec();
    let d = design_checked_with(&spec, &AnalysisOptions::compact(spec.taps)).expect("checked");
    let v = verify_apodizing(&spec, &d.coeffs).expect("normative verify");
    assert!(v.passed(), "failures: {:?}", v.failures);
    assert!(v.failures.is_empty());
    assert!(v.checks.finite && v.checks.taps && v.checks.symmetry && v.checks.delay);
    assert!(v.checks.group_delay && v.checks.passband_ripple && v.checks.stopband);
    assert!(v.checks.pre_ring && v.checks.pre_ring_energy && v.checks.post_ring_reported);
    assert!(v.checks.transition_monotonic && v.checks.reference && v.checks.endpoints);
    assert!(v.checks.family && v.checks.fp_boundary);
    assert!(v.checks.f32_quantization);
    // normative 网格：N=max(8192, 8*L)（§1.2），Nyquist 带 4096 点。
    assert_eq!(v.passband.points, 8_192);
    assert_eq!(v.stopband.points, 4_096);
    assert_eq!(v.transition.points, 8_192);
    assert_eq!(v.family.statement, NO_PROPRIETARY_DAC_STATEMENT);
    assert!(v.family.statement.contains("不复刻任何专有 DAC"));
    assert!(v.warnings.is_empty(), "warnings: {:?}", v.warnings);
}

#[test]
fn t35_normative_analysis_options_grid_counts() {
    let spec = ref_spec();
    let opt = AnalysisOptions::normative(&spec);
    assert_eq!(opt.passband_points, 8_192);
    assert_eq!(opt.transition_points, 8_192);
    assert_eq!(opt.stopband_points, 4_096);
    assert_eq!(opt.quantization_points, 2_048);
    let compact = AnalysisOptions::compact(spec.taps);
    assert_eq!(compact.passband_points, 512);
    assert_eq!(compact.transition_points, 1_024);
    assert_eq!(compact.stopband_points, 256);
    assert_eq!(compact.quantization_points, 128);
    // 大 taps 时 normative 按 8*L 扩展。
    let big = ApodizingSpec::new(48_000.0, 2_048, 18_000.0);
    assert_eq!(AnalysisOptions::normative(&big).passband_points, 16_384);
}

#[test]
fn t35_design_is_deterministic_and_f32_quantization_is_reported() {
    let a = ref_design();
    let b = ref_design();
    assert_eq!(a.coeffs, b.coeffs);
    assert_eq!(a.peak_abs, b.peak_abs);
    let f32s = a.coeffs_f32();
    assert_eq!(f32s.len(), a.coeffs.len());
    let mut max_err = 0.0_f64;
    for (x, y) in a.coeffs.iter().zip(f32s.iter()) {
        assert!(y.is_finite());
        max_err = max_err.max((f64::from(*y) - x).abs());
    }
    assert!(max_err <= 1e-7 * a.peak_abs, "f32 max abs err {max_err}");
    let spec = ref_spec();
    let v = verify_apodizing_with(&spec, &a.coeffs, &AnalysisOptions::compact(spec.taps)).unwrap();
    assert!(v.phase_quantization.f32_quantization_ok);
    assert!(v.phase_quantization.f32_max_magnitude_dev_db < 1e-4);
    assert!(v.phase_quantization.f32_max_phase_dev_deg < 1e-5);
    assert!(v.phase_quantization.f32_relative_l2_error < 1e-6);
}

#[test]
fn t35_phase_quantization_report_uses_linear_phase_model() {
    let spec = ref_spec();
    let d = ref_design();
    let v = verify_apodizing_with(&spec, &d.coeffs, &AnalysisOptions::compact(spec.taps)).unwrap();
    let pq = &v.phase_quantization;
    assert!(pq.linear_phase, "apodizing 必须线性相位");
    assert_eq!(pq.delay_samples, 511.5);
    assert!(
        pq.linear_phase_max_dev_deg < 1e-6,
        "linear phase dev {}",
        pq.linear_phase_max_dev_deg
    );
    // 1 kHz 处解析相位 wrap = -360*1000*511.5/48000 + 11*360 = 123.75 度。
    assert!(wrap_deg_diff(pq.phase_at_1khz_deg, 123.75) < 0.1);
    assert_approx(v.group_delay.at_1khz_samples, 511.5, 1e-6, "GD 1 kHz");
    assert!(v.group_delay.class_constant);
    assert!(v.group_delay.max_deviation_samples <= GROUP_DELAY_TOLERANCE_SAMPLES);
    assert!(v.group_delay.min_samples <= v.group_delay.max_samples);
}

#[test]
fn t35_hann_squared_design_passes_family_gate() {
    let spec = ref_spec().with_window(ApodizingWindowKind::HannSquared);
    let d = design_checked_with(&spec, &AnalysisOptions::compact(spec.taps)).expect("hann2");
    assert_eq!(d.window, ApodizingWindowKind::HannSquared);
    assert_eq!(d.kaiser_beta, None);
    let v = verify_apodizing_with(&spec, &d.coeffs, &AnalysisOptions::compact(spec.taps)).unwrap();
    assert!(v.passed(), "failures: {:?}", v.failures);
    // Hann² 参考构造的 pre-ring 与 Kaiser 同量级（probe: -54.6425 dB）。
    assert_approx(
        v.ringing.pre_db.unwrap(),
        -54.642_525_911_529,
        0.01,
        "hann2 pre_ring",
    );
    assert!(v.ringing.pre_energy_db.unwrap() <= PRE_RING_ENERGY_BOUND_DB);
    assert!(v.checks.family);
}

#[test]
fn t35_design_96k_reference_preset() {
    let spec = ApodizingSpec::reference_preset(96_000.0, 1_024);
    assert_eq!(spec.passband_hz[1], 36_000.0);
    let d = design_checked_with(&spec, &AnalysisOptions::compact(spec.taps)).expect("96k");
    assert_approx(d.delay_samples, 511.5, 1e-12, "D");
    assert_approx(d.delay_ms, 5.328_125, 1e-12, "96k delay_ms");
    let v = verify_apodizing_with(&spec, &d.coeffs, &AnalysisOptions::compact(spec.taps)).unwrap();
    assert!(v.passed(), "failures: {:?}", v.failures);
    assert!(v.endpoints.f20k.is_measured());
    // 96k 的 W=ceil(0.000333*96000)=32。
    assert_eq!(v.ringing.exclusion_samples, 32);
}

#[test]
fn t35_design_32k_endpoint_not_applicable() {
    let spec = ApodizingSpec::new(32_000.0, 512, 12_000.0);
    let d = design_checked_with(&spec, &AnalysisOptions::compact(spec.taps)).expect("32k");
    let v = verify_apodizing_with(&spec, &d.coeffs, &AnalysisOptions::compact(spec.taps)).unwrap();
    assert!(v.passed(), "failures: {:?}", v.failures);
    assert!(v.endpoints.f20.is_measured());
    assert_eq!(v.endpoints.f20k.status, EndpointStatus::NotApplicableAtFs);
    assert!(v.endpoints.f20k.reason.is_some());
    assert!(!v.warnings.is_empty());
}

#[test]
fn t35_short_taps_rejected_as_budget_not_silently_shortened() {
    let spec = ApodizingSpec::new(48_000.0, 64, 18_000.0);
    let d = design(&spec).expect("construction ok");
    assert_eq!(d.taps(), 64);
    let v = verify_apodizing_with(&spec, &d.coeffs, &AnalysisOptions::compact(spec.taps)).unwrap();
    assert!(!v.passed());
    assert!(
        !v.checks.stopband,
        "64-tap 无法满足 30 dB Nyquist 带（不静默截短）"
    );
    expect_code(
        design_checked_with(&spec, &AnalysisOptions::compact(spec.taps)),
        "E_FIR_BUDGET",
    );
}

#[test]
fn t35_ringing_not_applicable_for_too_short_support() {
    let spec = ApodizingSpec::new(48_000.0, 32, 18_000.0);
    let d = design(&spec).expect("construction ok");
    let ring = ring_metrics(&d.coeffs, 48_000.0);
    assert_eq!(ring.exclusion_samples, 16);
    assert_eq!(ring.center_samples, 15.5);
    assert!(ring.pre_db.is_none(), "center-W<0 时 pre 必须 n/a（§1.4）");
    assert!(!ring.pre_applicable());
    assert!(ring.post_db.is_some());
    assert_approx(
        ring.post_db.unwrap(),
        -240.0,
        1e-9,
        "空 post 区 = -240 dB 下限",
    );
    let v = verify_apodizing_with(&spec, &d.coeffs, &AnalysisOptions::compact(spec.taps)).unwrap();
    assert!(!v.passed());
    assert!(!v.checks.pre_ring);
    assert!(!v.checks.pre_ring_energy);
    assert!(v.failures.iter().any(|f| f.contains("支撑过短")));
}

// ---------------------------------------------------------------------------
// 阻带 / 前振铃门 / 家族互斥的负向控制
// ---------------------------------------------------------------------------

#[test]
fn t35_nyquist_attenuation_requirement_rejected_when_infeasible() {
    // 参考构造的 Nyquist 带实测约 -36 dB；声明 40 dB 属支撑/目标不可达（不静默放宽）。
    let spec = ref_spec().with_nyquist_attenuation(40.0);
    expect_code(
        design_checked_with(&spec, &AnalysisOptions::compact(spec.taps)),
        "E_FIR_BUDGET",
    );
}

#[test]
fn t35_stricter_pre_ring_bound_is_enforced() {
    let passes = ref_spec().with_pre_ring_bound(-50.0);
    assert!(design_checked_with(&passes, &AnalysisOptions::compact(passes.taps)).is_ok());
    let fails = ref_spec().with_pre_ring_bound(-56.0);
    expect_code(
        design_checked_with(&fails, &AnalysisOptions::compact(fails.taps)),
        "E_FIR_FAMILY",
    );
}

#[test]
fn t35_fp_at_boundary_passes_pre_ring_but_stopband_is_budget() {
    let spec = ApodizingSpec::new(48_000.0, 1_024, 20_400.0);
    let d = design(&spec).expect("boundary design");
    let v = verify_apodizing_with(&spec, &d.coeffs, &AnalysisOptions::compact(spec.taps)).unwrap();
    assert!(v.checks.fp_boundary);
    assert!(v.family.fp_ok);
    assert!(
        v.checks.pre_ring,
        "边界 f_p=0.85*Nyq 的 pre_ring 应仍 <= -45；{:?}",
        v.ringing.pre_db
    );
    assert!(v.ringing.pre_db.unwrap() <= -45.0);
    assert_approx(
        v.ringing.pre_db.unwrap(),
        -49.896_040_212_934,
        0.01,
        "boundary pre_ring",
    );
    // 0.49fs..0.499fs 目标只有 ~-27 dB，30 dB 声明不可达 → stopband 失败（Budget，不是静默通过）。
    assert!(!v.checks.stopband);
    expect_code(
        design_checked_with(&spec, &AnalysisOptions::compact(spec.taps)),
        "E_FIR_BUDGET",
    );
}

#[test]
fn t35_aggressive_target_pre_ring_above_bound_rejected_as_family() {
    // 用 0.90*Nyq 的“过度激进”目标合成系数，再按参考 spec 报告 → 前振铃 -37.87 dB 不达 -45。
    let target = ApodizingTarget::new(48_000.0, 21_600.0);
    let window = apodizing_window_values(ApodizingWindowKind::Kaiser, 1_024, None).unwrap();
    let coeffs = synthesize_apodizing(&target, 1_024, &window, SYNTHESIS_POINTS).unwrap();
    let spec = ref_spec();
    let v = verify_apodizing_with(&spec, &coeffs, &AnalysisOptions::compact(spec.taps)).unwrap();
    assert!(!v.passed());
    assert!(!v.checks.pre_ring);
    assert!(v.ringing.pre_db.unwrap() > -45.0);
    assert_approx(
        v.ringing.pre_db.unwrap(),
        -37.869_074_657_955,
        0.01,
        "aggressive pre_ring",
    );
    expect_code(verify_apodizing_checked(&spec, &coeffs), "E_FIR_FAMILY");
}

#[test]
fn t35_verify_rejects_wrong_length_and_nonfinite_coefficients() {
    let spec = ref_spec();
    let d = ref_design();
    let short = &d.coeffs[..1_023];
    let opt = AnalysisOptions::compact(spec.taps);
    expect_code(verify_apodizing_with(&spec, short, &opt), "E_FIR_TAPS");

    let mut nan = d.coeffs.clone();
    nan[0] = f64::NAN;
    expect_code(verify_apodizing_with(&spec, &nan, &opt), "E_FIR_NUMERIC");

    let zeros = vec![0.0_f64; spec.taps];
    expect_code(verify_apodizing_with(&spec, &zeros, &opt), "E_FIR_NUMERIC");
}

#[test]
fn t35_asymmetric_coefficients_rejected_as_phase_class() {
    let spec = ApodizingSpec::new(48_000.0, 64, 18_000.0);
    let mut coeffs = vec![0.0_f64; 64];
    coeffs[0] = 1.0;
    coeffs[1] = 0.5;
    let opt = AnalysisOptions::compact(spec.taps);
    let v = verify_apodizing_with(&spec, &coeffs, &opt).unwrap();
    assert!(!v.checks.symmetry);
    assert!(!v.passed());
    expect_code(
        verify_apodizing_checked(&spec, &coeffs),
        "E_FIR_PHASE_CLASS",
    );
}

// ---------------------------------------------------------------------------
// 报告字段（前后振铃 / 阻带 / 相位量化 / 支撑 / 端点）
// ---------------------------------------------------------------------------

#[test]
fn t35_ring_metrics_exclusion_window_and_symmetry() {
    let d = ref_design();
    let ring = ring_metrics(&d.coeffs, 48_000.0);
    assert_eq!(ring.exclusion_samples, 16);
    assert_eq!(ring.center_samples, 511.5);
    assert_eq!(ring.peak_index, 511);
    let pre = ring.pre_db.unwrap();
    let post = ring.post_db.unwrap();
    assert_approx(pre, post, 1e-12, "对称设计 pre/post 必须相等");
    let pre_e = ring.pre_energy_db.unwrap();
    let post_e = ring.post_energy_db.unwrap();
    assert_approx(pre_e, post_e, 1e-12, "对称设计 pre/post 能量必须相等");
    assert!(pre <= -45.0 && pre_e <= -45.0);
    assert!(pre < -50.0 && pre_e < -50.0, "参考构造应显著优于 -45 dB 门");
    assert!(ring.pre_applicable() && ring.post_applicable());
}

#[test]
fn t35_effective_support_and_delay_report() {
    let d = ref_design();
    let (first, last, eff) = d.effective_support();
    assert!(first > 0 && last < 1_023);
    assert_eq!(eff, last - first + 1);
    assert!(
        eff > 250 && eff < 450,
        "V16 Kaiser 设计有效支撑应在 ~338 附近，实际 {eff}"
    );
    let spec = ref_spec();
    let v = verify_apodizing_with(&spec, &d.coeffs, &AnalysisOptions::compact(spec.taps)).unwrap();
    assert_eq!(v.effective_support.first, first);
    assert_eq!(v.effective_support.last, last);
    assert_eq!(v.effective_support.effective_taps, eff);
    assert_eq!(v.effective_support.eps_micro, 1);
    assert_approx(v.delay_samples, 511.5, 1e-12, "D");
    assert_approx(v.delay_ms, 10.656_25, 1e-12, "delay_ms");
}

#[test]
fn t35_endpoints_20hz_20khz_reported() {
    let spec = ref_spec();
    let d = ref_design();
    let v = verify_apodizing_with(&spec, &d.coeffs, &AnalysisOptions::compact(spec.taps)).unwrap();
    assert!(v.endpoints.f20.is_measured());
    assert_approx(v.endpoints.f20.freq_hz, 20.0, 0.0, "f20");
    assert!(
        v.endpoints.f20.mag_db.abs() < 0.01,
        "20 Hz 幅度 {}",
        v.endpoints.f20.mag_db
    );
    assert!(v.endpoints.f20k.is_measured());
    // 目标在 20 kHz：0.5*(1+cos(pi*2000/6000)) = 0.75 → -2.4988 dB。
    assert_approx(
        v.endpoints.f20k.mag_db,
        -2.498_774_982_557,
        0.02,
        "20 kHz 幅度",
    );
    assert!(v.endpoints.ok());
    assert_eq!(EndpointStatus::Measured.schema_name(), "measured");
    assert_eq!(
        EndpointStatus::NotApplicableAtFs.schema_name(),
        "not_applicable_at_fs"
    );
}

#[test]
fn t35_family_probe_and_statement() {
    let spec = ref_spec();
    let d = ref_design();
    let v = verify_apodizing_with(&spec, &d.coeffs, &AnalysisOptions::compact(spec.taps)).unwrap();
    // 0.45fs=21600 Hz 处为升余弦过渡区（远高于 -60 dB，故不是砖墙式）。
    assert_approx(v.family.brickwall_probe_hz, 21_600.0, 0.0, "probe hz");
    assert_approx(
        v.family.brickwall_probe_db,
        -9.230_249_805_632,
        0.01,
        "0.45fs dB",
    );
    assert!(v.family.not_brickwall);
    assert!(v.family.ok());
    assert_eq!(v.family.statement, NO_PROPRIETARY_DAC_STATEMENT);
    assert!(v.family.statement.contains("仅自研可算窗/目标"));
    assert!(v.family.statement.contains("不复刻任何专有 DAC"));
}

#[test]
fn t35_passband_and_transition_stats_sane() {
    let spec = ref_spec();
    let d = ref_design();
    let v = verify_apodizing_with(&spec, &d.coeffs, &AnalysisOptions::compact(spec.taps)).unwrap();
    assert_eq!(v.passband.lo_hz, 20.0);
    assert_eq!(v.passband.hi_hz, 18_000.0);
    assert!(v.passband.ripple_db() <= 0.55);
    assert!(v.passband.ripple_db() < 1e-3, "V16 参考构造通带极平");
    assert_approx(v.transition.lo_hz, 18_000.0, 0.0, "transition lo");
    assert_approx(v.transition.hi_hz, 0.499 * 48_000.0, 1e-9, "transition hi");
    assert!(
        v.transition.monotonic(),
        "max rise {}",
        v.transition.max_rise_db
    );
    assert!(
        v.transition.min_db < -40.0,
        "过渡区应持续衰减（实际 min {} dB）",
        v.transition.min_db
    );
    assert!(v.transition.max_db < 0.6, "过渡区起点（f_p）应接近 0 dB");
    assert_eq!(v.stopband.lo_hz, 0.49 * 48_000.0);
    assert_approx(v.stopband.hi_hz, 0.499 * 48_000.0, 1e-9, "stop hi");
    assert!(v.stopband.attenuation_db > 30.0);
    assert!(v.stopband.max_db <= -(30.0 - 1.0));
}

// ---------------------------------------------------------------------------
// 其他契约/常量
// ---------------------------------------------------------------------------

#[test]
fn t35_constants_match_spec() {
    assert_eq!(MIN_TAPS, 8);
    assert_eq!(MAX_TAPS, 16_384);
    assert_eq!(MEASUREMENT_LO_HZ, 20.0);
    assert_eq!(MEASUREMENT_HI_HZ, 20_000.0);
    assert_eq!(NYQUIST_GUARD_FRACTION, 0.499);
    assert_eq!(FP_MAX_FRACTION_OF_NYQUIST, 0.85);
    assert_eq!(DEFAULT_RIPPLE_DB, 0.5);
    assert_eq!(PASSBAND_RIPPLE_MARGIN_DB, 0.05);
    assert_eq!(DEFAULT_NYQUIST_ATTENUATION_DB, 30.0);
    assert_eq!(NYQUIST_BAND_MARGIN_DB, 1.0);
    assert_eq!(NYQUIST_BAND_LO_FRACTION, 0.49);
    assert_eq!(DEFAULT_KAISER_BETA, 8.96);
    assert_eq!(MAG_FLOOR_AMPLITUDE, 1e-6);
    assert_approx(MAG_FLOOR_DB, -120.0, 0.0, "mag floor dB");
    assert_eq!(SYNTHESIS_POINTS, 32_768);
    assert_eq!(PRE_RING_BOUND_DB, -45.0);
    assert_eq!(PRE_RING_ENERGY_BOUND_DB, -45.0);
    assert_eq!(BRICKWALL_PROBE_FRACTION, 0.45);
    assert_eq!(BRICKWALL_PROBE_DB, -60.0);
    assert_eq!(TRANSITION_RISE_TOLERANCE_DB, 0.5);
    assert_eq!(RING_EXCLUSION_FRACTION, 0.000333);
    assert_eq!(GROUP_DELAY_TOLERANCE_SAMPLES, 0.01);
    assert_eq!(SYMMETRY_TOLERANCE, 1e-7);
    assert_eq!(REFERENCE_TOLERANCE_DB, 0.01);
    assert_eq!(FS_ENGINEERING_MIN_HZ, 8_000.0);
    assert_eq!(FS_ENGINEERING_MAX_HZ, 384_000.0);
    assert_eq!(
        NO_PROPRIETARY_DAC_STATEMENT,
        "仅自研可算窗/目标，不复刻任何专有 DAC"
    );
}

#[test]
fn t35_reference_preset_generic_and_spec_getters() {
    let s = ApodizingSpec::reference_preset(48_000.0, 1_024);
    assert_eq!(s, ApodizingSpec::reference_preset_48k());
    let s = ApodizingSpec::reference_preset(96_000.0, 2_048);
    assert_eq!(s.passband_hz[1], 36_000.0);
    assert_eq!(s.taps, 2_048);
    assert_eq!(s.passband_edge_hz(), 36_000.0);
    assert_eq!(s.nyquist_hz(), 48_000.0);
    assert_eq!(s.fp_limit_hz(), 40_800.0);
    let t = s.target();
    assert_eq!(t.fs, 96_000.0);
    assert_eq!(t.passband_edge_hz, 36_000.0);
    assert_eq!(t.nyquist_hz(), 48_000.0);
}

#[test]
fn t35_response_helpers_on_identity_delta() {
    let h = [1.0_f64];
    for f in [0.0, 20.0, 1_000.0, 20_000.0] {
        let r = response_at(&h, 48_000.0, f);
        assert_approx(r.magnitude_db(), 0.0, 1e-12, "delta magnitude");
        assert_approx(group_delay_at(&h, 48_000.0, f), 0.0, 1e-9, "delta GD");
        assert_approx(magnitude_db_at(&h, 48_000.0, f), 0.0, 1e-12, "delta mag db");
    }
    assert_eq!(symmetry_error(&[1.0, 2.0, 1.0]), 0.0);
    assert!(symmetry_error(&[1.0, 2.0, 3.0]) > 1.0);
    let (f, l, e) = effective_support(&[0.0, 1.0, 0.0], 1e-6);
    assert_eq!((f, l, e), (1, 1, 1));
    assert_eq!(effective_support(&[], 1e-6), (0, 0, 0));
}

#[test]
fn t35_design_options_custom_synthesis_points() {
    let spec = ref_spec();
    let d = design_with_options(&spec, &DesignOptions::with_synthesis_points(8_192)).expect("8192");
    assert_eq!(d.synthesis_points, 8_192);
    let v = verify_apodizing_with(&spec, &d.coeffs, &AnalysisOptions::compact(spec.taps)).unwrap();
    assert!(v.passed(), "failures: {:?}", v.failures);
    // N=8192 与 N=32768 的参考构造 pre-ring 差应远小于工程容差。
    assert!((v.ringing.pre_db.unwrap() - (-54.636_080_028_118)).abs() < 1.0);
    // 再次构造逐位一致（确定性）。
    let d2 = design_with_options(&spec, &DesignOptions::with_synthesis_points(8_192)).unwrap();
    assert_eq!(d.coeffs, d2.coeffs);
}

#[test]
fn t35_verify_apodizing_checked_accepts_reference_design() {
    let spec = ref_spec();
    let d = ref_design();
    let v = verify_apodizing_checked(&spec, &d.coeffs).expect("checked path");
    assert!(v.passed());
    let v2 = verify_apodizing_design(&d).expect("verify design helper");
    assert!(v2.passed());
}

#[test]
fn t35_verify_checked_classifies_reference_failure() {
    // 直接把参考系数按“更严参考增益”不可能触发；改用冲激系数（对称但幅频全 0 dB）：
    // 通带通过、Nyquist 带不满足 30 dB（0 dB > -29）→ E_FIR_BUDGET（非 PhaseClass/Family）。
    let spec = ApodizingSpec::new(48_000.0, 1_023, 18_000.0); // 奇长 Type I：Nyquist 处可非零
    let mut delta = vec![0.0_f64; spec.taps];
    delta[0] = 1.0;
    // 偶长对称 FIR 在 Nyquist 必为零；用奇长中心冲激（H=1 全频）验证 stopband→Budget 分类。
    let mut sym = vec![0.0_f64; spec.taps];
    sym[511] = 1.0;
    let opt = AnalysisOptions::compact(spec.taps);
    let v = verify_apodizing_with(&spec, &sym, &opt).unwrap();
    assert!(v.checks.symmetry, "中心冲激对称");
    assert!(!v.checks.stopband, "0 dB Nyquist 带不满足 30 dB");
    expect_code(verify_apodizing_checked(&spec, &sym), "E_FIR_BUDGET");
    // 非中心冲激（不对称）→ PhaseClass。
    let v2 = verify_apodizing_with(&spec, &delta, &opt).unwrap();
    assert!(!v2.checks.symmetry);
    expect_code(verify_apodizing_checked(&spec, &delta), "E_FIR_PHASE_CLASS");
}

// ---------------------------------------------------------------------------
// 诊断输出（--nocapture 时打印关键数值，供 evidence/T35 引用）
// ---------------------------------------------------------------------------

#[test]
fn t35_apodizing_diagnostics_v16_numbers() {
    let spec = ref_spec();
    let d = ref_design();
    let v = verify_apodizing_with(&spec, &d.coeffs, &AnalysisOptions::compact(spec.taps)).unwrap();
    println!(
        "T35_DIAG taps={} delay_samples={} delay_ms={}",
        v.taps, v.delay_samples, v.delay_ms
    );
    println!(
        "T35_DIAG pre_ring_db={:.12} post_ring_db={:.12} pre_energy_db={:.12} post_energy_db={:.12}",
        v.ringing.pre_db.unwrap(),
        v.ringing.post_db.unwrap(),
        v.ringing.pre_energy_db.unwrap(),
        v.ringing.post_energy_db.unwrap()
    );
    println!(
        "T35_DIAG stopband_max_db={:.12} attenuation_db={:.12} nyquist_band=[{:.3},{:.3}]",
        v.stopband.max_db, v.stopband.attenuation_db, v.stopband.lo_hz, v.stopband.hi_hz
    );
    println!(
        "T35_DIAG passband_ripple_db={:.12} gd_1k={:.12} linear_phase_dev_deg={:.3e}",
        v.passband.ripple_db(),
        v.group_delay.at_1khz_samples,
        v.phase_quantization.linear_phase_max_dev_deg
    );
    println!(
        "T35_DIAG f32_mag_dev_db={:.3e} f32_phase_dev_deg={:.3e} f32_rel_l2={:.3e}",
        v.phase_quantization.f32_max_magnitude_dev_db,
        v.phase_quantization.f32_max_phase_dev_deg,
        v.phase_quantization.f32_relative_l2_error
    );
    println!(
        "T35_DIAG probe_0.45fs_hz={:.3} probe_db={:.12} not_brickwall={}",
        v.family.brickwall_probe_hz, v.family.brickwall_probe_db, v.family.not_brickwall
    );
    println!(
        "T35_DIAG endpoint_20hz={:.6} dB endpoint_20khz={:.6} dB effective_taps={}",
        v.endpoints.f20.mag_db, v.endpoints.f20k.mag_db, v.effective_support.effective_taps
    );
    println!("T35_DIAG statement={}", v.family.statement);
    assert!(v.passed());
}
