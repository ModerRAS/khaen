// T36 私有测试：有限 brickwall 近似（Kaiser 窗 sinc）+ 判据 + 预算拒绝。
//
// 本文件由 crates/designer/src/brickwall.rs 的 #[cfg(test)] 子模块通过 include! 编译执行
// （cargo 不自动发现 tests/<Txx>/ 子目录；public_registration.rs 属 integrator，不改）。
// 运行：cargo test -p tspi-fir-designer brickwall
//
// use super::* 由外层 #[cfg(test)] 模块提供（include! 共享作用域）。

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

#[track_caller]
fn expect_code<T: std::fmt::Debug>(
    result: Result<T, BrickwallError>,
    code: &str,
) -> BrickwallError {
    match result {
        Ok(v) => panic!("期望错误 {code}，实际成功 {v:?}"),
        Err(e) => {
            assert_eq!(e.code(), code, "错误码不符：{e}");
            e
        }
    }
}

/// T04 V16 的 brickwall 参考构造：L=1024、f_p=19k、f_s=20.5k、fc=19750、Kaiser beta=8.96。
fn v16_spec() -> BrickwallSpec {
    BrickwallSpec::lowpass(48_000.0, 1024, 19_000.0, 20_500.0).with_kaiser_beta(8.96)
}

/// 可行且余量大的正例：A=60、Δf=19000、L_req=11、taps=256。
fn positive_spec() -> BrickwallSpec {
    BrickwallSpec::lowpass(48_000.0, 256, 1_000.0, 20_000.0).with_attenuation(60.0)
}

fn design_of(spec: &BrickwallSpec) -> BrickwallDesign {
    match design(spec) {
        Ok(d) => d,
        Err(e) => panic!("design 应成功：{e}"),
    }
}

fn verify_compact(spec: &BrickwallSpec, coeffs: &[f64]) -> BrickwallVerification {
    let opts = BrickwallAnalysisOptions::compact(spec.taps);
    match verify_brickwall_with(spec, coeffs, &opts) {
        Ok(v) => v,
        Err(e) => panic!("verify 应成功：{e}"),
    }
}

// ---------------------------------------------------------------------------
// 参数/边界/预算拒绝（§3.5 判据 7、§7.2）
// ---------------------------------------------------------------------------

#[test]
fn t36_spec_defaults_contract() {
    let s = BrickwallSpec::lowpass(48_000.0, 1024, 19_000.0, 20_500.0);
    assert_eq!(s.passband_hz, [20.0, 19_000.0]);
    assert_approx(
        s.stopband_hz[1],
        0.499 * 48_000.0,
        1e-12,
        "阻带上界=0.499fs",
    );
    assert_approx(s.ripple_db, 0.2, 0.0, "ripple 默认 0.2");
    assert_approx(s.attenuation_db, 80.0, 0.0, "A_s 默认 80");
    assert_eq!(s.method, BrickwallMethod::KaiserWindowedSinc);
    assert_eq!(s.kaiser_beta, None);
    // 常量与契约一致。
    assert_eq!(MIN_TAPS, 8);
    assert_eq!(MAX_TAPS, 16_384);
    assert_eq!(REGULAR_PROFILE_TAPS_CAP, 4_096);
}

#[test]
fn t36_rejects_zero_or_negative_transition() {
    // 过渡带 = 0（f_s == f_p）。
    let s = BrickwallSpec::lowpass(48_000.0, 256, 1_000.0, 1_000.0);
    let e = expect_code(design(&s), "E_FIR_BAND");
    assert!(e.detail().contains("过渡带"), "detail: {}", e.detail());
    // 过渡带为负（f_s < f_p）。
    let s2 = BrickwallSpec::lowpass(48_000.0, 256, 2_000.0, 1_000.0);
    expect_code(design(&s2), "E_FIR_BAND");
}

#[test]
fn t36_rejects_transition_below_20hz() {
    let s = BrickwallSpec::lowpass(48_000.0, 256, 10_000.0, 10_019.0);
    let e = expect_code(design(&s), "E_FIR_BAND");
    assert!(e.detail().contains("20"), "detail: {}", e.detail());
}

#[test]
fn t36_rejects_attenuation_out_of_range() {
    // §3.5 判据 7：A_s ∈ [60,120]。
    for bad in [59.9_f64, 120.1, 0.0, -80.0] {
        let s = positive_spec().with_attenuation(bad);
        expect_code(design(&s), "E_FIR_BAND");
    }
    // 边界值可接受（构造可达）。
    let ok60 = positive_spec().with_attenuation(60.0);
    assert!(design(&ok60).is_ok());
}

#[test]
fn t36_rejects_taps_bounds_and_offline_only() {
    // 下界：<8 → E_FIR_BAND。
    let s = positive_spec();
    let mut low = s.clone();
    low.taps = 7;
    expect_code(design(&low), "E_FIR_BAND");
    // 上界：>16384 → E_FIR_BUDGET（§7.2 >16384 仅 offline，runtime 不放行；不静默截断）。
    let mut over = s.clone();
    over.taps = 16_385;
    let e = expect_code(design(&over), "E_FIR_BUDGET");
    assert!(e.detail().contains("16384"), "detail: {}", e.detail());
    let mut way_over = s;
    way_over.taps = 40_960;
    expect_code(design(&way_over), "E_FIR_BUDGET");
    // 16384 本身可设计（extended profile）。
    let mut max_ok =
        BrickwallSpec::lowpass(48_000.0, 16_384, 1_000.0, 20_000.0).with_attenuation(60.0);
    max_ok.stopband_hz = [20_000.0, 0.499 * 48_000.0];
    let d = design(&max_ok).expect("16384 taps 应可设计");
    assert_eq!(d.taps(), 16_384);
    assert_eq!(d.budget.profile, BrickwallProfile::Extended);
}

#[test]
fn t36_rejects_bad_passband_or_stopband_structure() {
    // passband_hz[0] 必须=20 Hz（§1.2 测量网格下界）。
    let mut s = positive_spec();
    s.passband_hz[0] = 19.0;
    expect_code(design(&s), "E_FIR_BAND");
    // stopband_hz[1] 必须=0.499fs。
    let mut s2 = positive_spec();
    s2.stopband_hz[1] = 0.5 * 48_000.0;
    expect_code(design(&s2), "E_FIR_BAND");
    // f_s > 0.499fs。
    let mut s3 = positive_spec();
    s3.stopband_hz = [25_000.0, 0.499 * 48_000.0];
    expect_code(design(&s3), "E_FIR_BAND");
}

#[test]
fn t36_rejects_declared_delay_mismatch() {
    let s = v16_spec().with_declared_delay(511.0); // 正确 D=(1024−1)/2=511.5
    let e = expect_code(design(&s), "E_FIR_DELAY");
    assert!(e.detail().contains("511.5"), "detail: {}", e.detail());
    // 正确的半整数 D 可接受。
    assert!(design(&v16_spec().with_declared_delay(511.5)).is_ok());
}

#[test]
fn t36_rejects_unimplemented_methods() {
    // §3.5 允许声明 least_squares/remez，但本 leaf 只交付 kaiser_windowed_sinc：
    // 显式 E_FIR_FAMILY 拒绝（不冒充等波纹/最小二乘）。
    for method in [BrickwallMethod::LeastSquares, BrickwallMethod::Remez] {
        let s = positive_spec().with_method(method);
        let e = expect_code(design(&s), "E_FIR_FAMILY");
        assert!(e.detail().contains(method.as_str()));
        assert!(!method.implemented());
    }
    assert!(BrickwallMethod::KaiserWindowedSinc.implemented());
}

#[test]
fn t36_rejects_nonfinite_or_invalid_parameters() {
    let mut s = positive_spec();
    s.fs = f64::NAN;
    expect_code(design(&s), "E_FIR_BAND");
    let mut s2 = positive_spec();
    s2.fs = 0.0;
    expect_code(design(&s2), "E_FIR_BAND");
    let mut s3 = positive_spec();
    s3.ripple_db = -0.1;
    expect_code(design(&s3), "E_FIR_BAND");
    let mut s4 = positive_spec();
    s4.kaiser_beta = Some(-1.0);
    expect_code(design(&s4), "E_FIR_BAND");
    // 非有限频带。
    let mut s5 = positive_spec();
    s5.passband_hz[1] = f64::INFINITY;
    expect_code(design(&s5), "E_FIR_BAND");
}

#[test]
fn t36_kaiser_beta_default_formula() {
    // §3.1 标准公式：A=80 → 0.1102*(80−8.7)=7.85726；A=60 → 5.6045...
    let v80 = match validate(&positive_spec().with_attenuation(80.0)) {
        Ok(v) => v,
        Err(e) => panic!("validate 失败：{e}"),
    };
    assert_approx(v80.kaiser_beta, 7.857_26, 1e-6, "beta(A=80)");
    let mut s60 = positive_spec().with_attenuation(60.0);
    s60.stopband_hz = [20_000.0, 0.499 * 48_000.0];
    let v60 = validate(&s60).expect("validate A=60");
    // A>50 → beta=0.1102(A−8.7)=5.65326。
    let expected60 = 0.1102 * (60.0 - 8.7);
    assert_approx(v60.kaiser_beta, expected60, 1e-9, "beta(A=60)");
    // 显式 beta 覆盖默认。
    let vx = validate(&v16_spec()).expect("validate V16");
    assert_approx(vx.kaiser_beta, 8.96, 0.0, "显式 beta");
}

#[test]
fn t36_kaiser_required_taps_v13_table() {
    // §9 V13 七组复算值（14.357078 = 2.285*2π，评审 I11）。
    let cases = [
        (60.0, 1_000.0, 48_000.0, 176_u64),
        (80.0, 1_000.0, 48_000.0, 242),
        (100.0, 1_000.0, 48_000.0, 309),
        (120.0, 1_000.0, 48_000.0, 376),
        (80.0, 500.0, 48_000.0, 483),
        (80.0, 1_000.0, 96_000.0, 483),
        (60.0, 2_000.0, 96_000.0, 176),
    ];
    for (a, df, fs, want) in cases {
        let got = brickwall_required_taps(a, fs, df).expect("L_req");
        println!("V13 L_req: A_s={a} dB, df={df} Hz, fs={fs} Hz -> {got} (spec {want})");
        assert_eq!(got, want, "A={a} df={df} fs={fs}");
    }
    assert_eq!(brickwall_required_taps(80.0, 0.0, 1_000.0), None);
    assert_eq!(brickwall_required_taps(80.0, 48_000.0, 0.0), None);
}

#[test]
fn t36_budget_rejects_insufficient_taps() {
    // A=100、Δf=100 Hz @48k → L_req=3079；taps=1024 不足 → E_FIR_BUDGET。
    let s = BrickwallSpec::lowpass(48_000.0, 1024, 9_000.0, 9_100.0).with_attenuation(100.0);
    let v = validate(&s).expect("validate");
    println!(
        "budget reject: A_s=100 dB df=100 Hz fs=48000 -> L_req={:?}, taps=1024 -> E_FIR_BUDGET",
        v.required_taps
    );
    assert_eq!(v.required_taps, Some(3079));
    let e = expect_code(design(&s), "E_FIR_BUDGET");
    assert!(e.detail().contains("3079"), "detail: {}", e.detail());
    // L_req 恰好满足时可设计（不静默截短）。
    let ok = BrickwallSpec::lowpass(48_000.0, 3079, 9_000.0, 9_100.0).with_attenuation(100.0);
    let d = design(&ok).expect("taps=L_req 应可设计");
    assert_eq!(d.taps(), 3079);
}

#[test]
fn t36_budget_profiles_and_memory() {
    // §7.2：常规 <=4096；extended 8192/16384；>16384 offline only。
    let b = BrickwallBudget::for_taps(4096);
    assert_eq!(b.profile, BrickwallProfile::Regular);
    assert_eq!(b.route_taps_cap, 4096);
    assert_eq!(b.ram_bytes_f32, 16_384);
    assert!(b.fits_regular_profile && b.fits_extended_profile);
    let b = BrickwallBudget::for_taps(4097);
    assert_eq!(b.profile, BrickwallProfile::Extended);
    assert_eq!(b.route_taps_cap, 16_384);
    assert_eq!(b.ram_bytes_f32, 16_388);
    assert!(!b.fits_regular_profile && b.fits_extended_profile);
    let b = BrickwallBudget::for_taps(16_384);
    assert_eq!(b.profile, BrickwallProfile::Extended);
    assert_eq!(b.ram_bytes_f32, 65_536);
    assert!(b.fits_extended_profile);
    let b = BrickwallBudget::for_taps(16_385);
    assert_eq!(b.profile, BrickwallProfile::OfflineOnly);
    assert_eq!(b.route_taps_cap, 0);
    assert!(!b.fits_regular_profile && !b.fits_extended_profile);
    assert_eq!(BrickwallProfile::OfflineOnly.as_str(), "offline_only");
    // 极端 taps 不 panic（saturating；真实设计路径在 validate 已先拒 >16384）。
    let huge = BrickwallBudget::for_taps(usize::MAX);
    assert_eq!(huge.profile, BrickwallProfile::OfflineOnly);
    assert_eq!(huge.ram_bytes_f32, usize::MAX);
}

// ---------------------------------------------------------------------------
// 构造（§3.5 参考构造）+ 线性相位/D/归一化/确定性
// ---------------------------------------------------------------------------

#[test]
fn t36_design_v16_linear_phase_and_delay() {
    let spec = v16_spec();
    let d = design_of(&spec);
    println!(
        "V16 design: taps={} fc={:.1} Hz D={:.1} samples delay_ms={:.6} beta={:.4} peak={:.6} sum={:.15}",
        d.taps(),
        d.center_hz,
        d.delay_samples,
        d.delay_ms,
        d.kaiser_beta,
        d.peak_abs,
        d.sum_coeffs
    );
    assert_eq!(d.taps(), 1024);
    assert_eq!(d.method, BrickwallMethod::KaiserWindowedSinc);
    assert_approx(d.kaiser_beta, 8.96, 0.0, "beta");
    assert_approx(d.center_hz, 19_750.0, 1e-12, "fc=(fp+fs)/2");
    assert_approx(d.delay_samples, 511.5, 0.0, "D=(L-1)/2");
    assert_approx(d.delay_ms, 10.656_25, 1e-9, "delay_ms");
    // 构造的精确镜像对称（判据 1）。
    assert_eq!(symmetry_error(&d.coeffs), 0.0);
    // 常数群延迟 D（解析 GD）。
    for f in [20.0_f64, 100.0, 1_000.0, 10_000.0, 19_000.0] {
        assert_approx(group_delay_at(&d.coeffs, 48_000.0, f), 511.5, 1e-6, "GD");
    }
    // DC 归一化（reference 0 dB）。
    assert!(magnitude_db_at(&d.coeffs, 48_000.0, 0.0).abs() <= 1e-6);
    assert_approx(d.sum_coeffs, 1.0, 1e-12, "Σh");
    assert!(d.peak_abs > 0.0);
    // D 与 delay_ms 辅助函数一致。
    assert_approx(delay_samples(1024), 511.5, 0.0, "helper D");
    assert_approx(delay_ms(1024, 48_000.0), 10.656_25, 1e-9, "helper ms");
}

#[test]
fn t36_design_deterministic_and_effective_support() {
    let spec = v16_spec();
    let a = design_of(&spec);
    let b = design_of(&spec);
    assert_eq!(a.coeffs, b.coeffs, "同输入两次设计必须逐位一致");
    let (first, last, eff) = a.effective_support();
    assert!(last < a.taps());
    assert_eq!(eff, last - first + 1);
    // brickwall 是长支撑（能量铺满 ±D 附近）；有效支撑应远大于 1/2。
    assert!(eff > a.taps() / 2, "eff={eff}");
    // f32 导出长度一致。
    assert_eq!(a.coeffs_f32().len(), a.taps());
}

// ---------------------------------------------------------------------------
// 独立验证：正例（规范网格/紧凑网格）
// ---------------------------------------------------------------------------

#[test]
fn t36_verify_positive_compact_all_checks() {
    let spec = v16_spec();
    let d = design_of(&spec);
    let v = verify_compact(&spec, &d.coeffs);
    assert!(v.passed(), "failures: {:?}", v.failures);
    assert!(v.checks.finite && v.checks.symmetry && v.checks.delay);
    assert!(v.checks.group_delay && v.checks.passband && v.checks.stopband);
    assert!(v.checks.transition && v.checks.reference && v.checks.budget);
    assert_approx(v.delay_samples, 511.5, 0.0, "D");
    assert_approx(v.group_delay.min_samples, 511.5, 0.01, "GD min");
    assert_approx(v.group_delay.max_samples, 511.5, 0.01, "GD max");
    assert!(v.passband.ripple_db() <= spec.ripple_db + PASSBAND_RIPPLE_MARGIN_DB);
    assert!(v.stopband.max_db <= -(spec.attenuation_db - STOPBAND_MARGIN_DB));
    println!(
        "compact verify: passband ripple={:.6} dB ({} pts), stopband max={:.3} dB ({} pts), transition edge={:?} Hz, grid={}",
        v.passband.ripple_db(),
        v.passband.points,
        v.stopband.max_db,
        v.stopband.points,
        v.transition.edge_hz,
        v.grid_points
    );
    // 紧凑网格：2048 (compact(1024)) + 64 扩展点（0.499fs=23952 > 20000）。
    assert_eq!(v.grid_points, 2048 + 64);
    assert!(v.ringing.pre_ring_db.is_some() && v.ringing.post_ring_db.is_some());
}

#[test]
fn t36_verify_normative_grid_positive_small() {
    // taps=256 → §1.2 规范网格 N=max(8192, 8L)=8192（可负担的 normative 正例）。
    let spec = positive_spec();
    let d = design_of(&spec);
    let v = match verify_brickwall(&spec, &d.coeffs) {
        Ok(v) => v,
        Err(e) => panic!("verify normative 失败：{e}"),
    };
    println!(
        "normative verify: grid={} passband ripple={:.6} dB stopband max={:.3} dB transition edge={:?} Hz GD={:?}",
        v.grid_points,
        v.passband.ripple_db(),
        v.stopband.max_db,
        v.transition.edge_hz,
        v.group_delay
    );
    assert!(v.passed(), "failures: {:?}", v.failures);
    assert!(v.grid_points >= 8192, "grid_points={}", v.grid_points);
    assert_eq!(v.grid_points, 8192 + 256);
    assert_approx(v.passband.ripple_db(), 0.0, 0.05, "通带纹波应近 0");
    assert!(
        v.stopband.max_db <= -59.0,
        "stopband max={}",
        v.stopband.max_db
    );
    // 过渡带穿越点存在且 <= f_s + max(0.05Δf,20Hz)。
    let edge = v.transition.edge_hz.expect("应有 -A_s 穿越点");
    assert!(edge <= v.transition.limit_hz + v.transition.tolerance_hz + 1e-6);
    assert!(v.transition.points >= 8192);
}

#[test]
fn t36_group_delay_constant_stats() {
    let spec = positive_spec();
    let d = design_of(&spec);
    let v = verify_compact(&spec, &d.coeffs);
    let expected = 127.5; // (256−1)/2
    assert!(v.group_delay.class_constant);
    assert_approx(v.group_delay.min_samples, expected, 0.01, "GD min");
    assert_approx(v.group_delay.max_samples, expected, 0.01, "GD max");
    assert_approx(v.group_delay.mean_samples, expected, 0.01, "GD mean");
    assert_approx(v.group_delay.at_1khz_samples, expected, 0.01, "GD@1k");
}

#[test]
fn t36_reference_and_endpoints_measured_at_48k() {
    let spec = positive_spec();
    let d = design_of(&spec);
    let v = verify_compact(&spec, &d.coeffs);
    assert_approx(v.reference.measured_gain_db, 0.0, 0.01, "reference 0 dB");
    assert_eq!(v.reference.hz, 0.0);
    // 20 Hz 端点 measured 且近 0 dB。
    match &v.endpoints.f20 {
        Endpoint::Measured {
            freq_hz,
            mag_db,
            phase_deg,
        } => {
            assert_approx(*freq_hz, 20.0, 0.0, "f20 hz");
            assert_approx(*mag_db, 0.0, 0.05, "f20 mag");
            assert!(phase_deg.is_finite());
        }
        other => panic!("20 Hz 应为 measured：{other:?}"),
    }
    // 20 kHz 端点 measured（0.499*48k=23952 > 20000）且处于阻带。
    match &v.endpoints.f20k {
        Endpoint::Measured {
            freq_hz, mag_db, ..
        } => {
            assert_approx(*freq_hz, 20_000.0, 0.0, "f20k hz");
            assert!(*mag_db <= -59.0, "f20k mag={mag_db}");
        }
        other => panic!("20 kHz 应为 measured：{other:?}"),
    }
    assert_eq!(v.endpoints.f20.status(), "measured");
    assert_eq!(v.endpoints.f20k.status(), "measured");
}

#[test]
fn t36_endpoints_not_applicable_at_low_fs() {
    // fs=32000 → 0.499fs=15968 < 20000：20 kHz 端点必须 not_applicable_at_fs（§2.4，禁止省略/裸字符串）。
    let spec = BrickwallSpec::lowpass(32_000.0, 256, 8_000.0, 9_000.0).with_attenuation(60.0);
    let d = design_of(&spec);
    let v = verify_compact(&spec, &d.coeffs);
    assert_eq!(v.endpoints.f20k.status(), "not_applicable_at_fs");
    assert_eq!(v.endpoints.f20k.measured(), None);
    assert_eq!(v.endpoints.f20.status(), "measured");
    match &v.endpoints.f20k {
        Endpoint::NotApplicableAtFs { reason } => {
            assert!(reason.contains("20000"), "reason: {reason}");
        }
        other => panic!("应为 not_applicable：{other:?}"),
    }
}

#[test]
fn t36_ringing_v16_matches_t04_reference() {
    // T04 V16 参考构造：L=1024、fc=19750、beta=8.96、DC 归一：
    // pre_ring=−30.33 dB、pre_energy=−24.61 dB（spec §3.5/§9 V16；评审复算差 1–2 dB 内）。
    let spec = v16_spec();
    let d = design_of(&spec);
    let r = d.ringing();
    assert_eq!(r.exclusion_samples, 16); // W=ceil(0.000333*48000)
    let pre = r.pre_ring_db.expect("pre ring");
    let post = r.post_ring_db.expect("post ring");
    let pre_e = r.pre_ring_energy_db.expect("pre energy");
    assert_approx(pre, -30.33, 0.2, "V16 pre_ring");
    assert_approx(pre_e, -24.61, 0.2, "V16 pre_energy");
    // 对称设计：前后振铃应一致。
    assert_approx(post, pre, 0.02, "post_ring 对称");
    assert!(r.decay_to_minus_80db_samples > 0);
    assert_eq!(
        r.decay_to_minus_80db_samples,
        r.pre_decay_samples.max(r.post_decay_samples)
    );
    // §9 V16：[21 kHz, 0.499fs) 内 max <= −108 dB（该构造阻带远好于 80 dB 声明）。
    let mut max_db = f64::NEG_INFINITY;
    let n = 2000;
    for k in 0..=n {
        let f = 21_000.0 + (23_952.0 - 21_000.0) * k as f64 / n as f64;
        max_db = max_db.max(magnitude_db_at(&d.coeffs, 48_000.0, f));
    }
    println!(
        "V16 ringing: W={} pre_ring={:.4} dB post_ring={:.4} dB pre_energy={:.4} dB decay(-80dB)={} samples; [21k,0.499fs) max={:.3} dB",
        r.exclusion_samples, pre, post, pre_e, r.decay_to_minus_80db_samples, max_db
    );
    assert!(max_db <= -100.0, "V16 阻带 [21k,0.499fs) max={max_db} dB");
}

#[test]
fn t36_ringing_not_applicable_for_short_support() {
    // 支撑过短（taps=8、W=16）：§1.4 要求两侧写 n/a，不得硬算。
    let spec = BrickwallSpec::lowpass(48_000.0, 8, 1_000.0, 20_000.0).with_attenuation(60.0);
    let mut coeffs = vec![0.0_f64; 8];
    coeffs[3] = 1.0;
    let v = verify_compact(&spec, &coeffs);
    assert_eq!(v.ringing.exclusion_samples, 16);
    assert!(v.ringing.pre_ring_db.is_none());
    assert!(v.ringing.post_ring_db.is_none());
    assert!(v.ringing.pre_ring_energy_db.is_none());
    assert!(v.ringing.post_ring_energy_db.is_none());
    assert!(v.ringing.is_not_applicable());
    // 预算也不满足（taps=8 < L_req=11），如实进 failures。
    assert!(!v.checks.budget);
    assert!(!v.failures.is_empty());
}

#[test]
fn t36_family_boundary_045fs_brickwall() {
    // §3.8：brickwall 必须有非零过渡带与 >=60 dB 阻带；0.45fs 处 <= −60 dB（与 apodizing 区分）。
    let spec = v16_spec();
    let d = design_of(&spec);
    let db = BrickwallVerification::mag_db_at_045_fs(&d.coeffs, 48_000.0);
    assert!(db <= -60.0, "0.45fs mag={db} dB");
}

#[test]
fn t36_fs96k_design_and_endpoints() {
    let spec = BrickwallSpec::lowpass(96_000.0, 512, 20_000.0, 22_000.0).with_attenuation(80.0);
    let d = design_of(&spec);
    assert_approx(d.delay_samples, 255.5, 0.0, "D");
    assert_approx(d.delay_ms, 255.5 / 96_000.0 * 1000.0, 1e-12, "delay_ms");
    let v = verify_compact(&spec, &d.coeffs);
    assert!(v.passed(), "failures: {:?}", v.failures);
    assert_approx(v.stopband.hi_hz, 0.499 * 96_000.0, 1e-9, "阻带上界");
    assert!(
        v.stopband.max_db <= -79.0,
        "stopband max={}",
        v.stopband.max_db
    );
    // 20 kHz 在 96k 可测。
    assert_eq!(v.endpoints.f20k.status(), "measured");
}

// ---------------------------------------------------------------------------
// 独立验证：负例（constructed coeffs，验证真能拒绝坏设计）
// ---------------------------------------------------------------------------

#[test]
fn t36_verify_detects_stopband_and_transition_failure() {
    // 纯 1.5 sample 平滑（h[127]=h[128]=0.5）：通带近 0 dB，但阻带只有 −12 dB，达不到 −80。
    let spec = BrickwallSpec::lowpass(48_000.0, 256, 1_000.0, 20_000.0).with_attenuation(80.0);
    let mut coeffs = vec![0.0_f64; 256];
    coeffs[127] = 0.5;
    coeffs[128] = 0.5;
    let v = verify_compact(&spec, &coeffs);
    assert!(v.checks.symmetry, "对称");
    assert!(v.checks.passband, "通带应仍近平坦");
    assert!(!v.checks.stopband, "阻带应 FAIL");
    assert!(!v.checks.transition, "无 −A_s 穿越点应 FAIL");
    let joined = v.failures.join("；");
    assert!(joined.contains("阻带"), "failures: {joined}");
    assert!(joined.contains("过渡"), "failures: {joined}");
    assert_eq!(criteria_to_error(&v).code(), "E_FIR_BUDGET");
}

#[test]
fn t36_verify_detects_reference_failure() {
    // 整体系数 ×1.01（+0.086 dB）：仅参考归一化越界（±0.01 dB），其它判据保持通过。
    let spec = positive_spec();
    let d = design_of(&spec);
    let scaled: Vec<f64> = d.coeffs.iter().map(|h| h * 1.01).collect();
    let v = verify_compact(&spec, &scaled);
    assert!(!v.checks.reference, "参考应 FAIL");
    assert!(v.checks.passband && v.checks.stopband && v.checks.transition);
    let joined = v.failures.join("；");
    assert!(joined.contains("参考"), "failures: {joined}");
    assert_eq!(criteria_to_error(&v).code(), "E_FIR_REF");
}

#[test]
fn t36_verify_detects_symmetry_failure() {
    let spec = positive_spec();
    let d = design_of(&spec);
    let mut coeffs = d.coeffs.clone();
    coeffs.swap(0, 1); // 破坏镜像对称
    let v = verify_compact(&spec, &coeffs);
    assert!(!v.checks.symmetry, "对称应 FAIL");
    assert!(v.failures.iter().any(|f| f.contains("对称")));
    assert_eq!(criteria_to_error(&v).code(), "E_FIR_PHASE_CLASS");
}

#[test]
fn t36_verify_detects_ripple_failure() {
    // 矩形窗 sinc（非本 leaf 构造）：通带 Gibbs 纹波 ~0.8 dB > 0.2+0.05 → 判据 2 FAIL。
    let spec = BrickwallSpec::lowpass(48_000.0, 1742, 10_000.0, 10_100.0).with_attenuation(60.0);
    let fc = 10_050.0;
    let r = 2.0 * fc / 48_000.0;
    let dd = delay_samples(1742);
    let mut coeffs = vec![0.0_f64; 1742];
    for (n, h) in coeffs.iter_mut().enumerate() {
        *h = r * sinc_normalized(r * (n as f64 - dd));
    }
    let sum: f64 = coeffs.iter().sum();
    for h in coeffs.iter_mut() {
        *h /= sum;
    }
    let v = verify_compact(&spec, &coeffs);
    assert!(!v.checks.passband, "通带纹波应 FAIL");
    assert!(!v.checks.stopband, "矩形窗阻带也应 FAIL");
    assert!(v.failures.iter().any(|f| f.contains("通带纹波")));
}

#[test]
fn t36_verify_rejects_taps_mismatch_and_nonfinite() {
    let spec = positive_spec();
    let d = design_of(&spec);
    let short = &d.coeffs[..255];
    expect_code(
        verify_brickwall_with(&spec, short, &BrickwallAnalysisOptions::compact(256)),
        "E_FIR_TAPS",
    );
    let opts = BrickwallAnalysisOptions::compact(256);
    let mut bad = d.coeffs.clone();
    bad[10] = f64::NAN;
    expect_code(verify_brickwall_with(&spec, &bad, &opts), "E_FIR_NUMERIC");
    let mut bad2 = d.coeffs.clone();
    bad2[10] = f64::INFINITY;
    expect_code(verify_brickwall_with(&spec, &bad2, &opts), "E_FIR_NUMERIC");
    // 全零系数 → max|h|=0。
    let zeros = vec![0.0_f64; 256];
    expect_code(verify_brickwall_with(&spec, &zeros, &opts), "E_FIR_NUMERIC");
}

// ---------------------------------------------------------------------------
// design_checked：构造 + 独立验证闭环
// ---------------------------------------------------------------------------

#[test]
fn t36_design_checked_pass_and_reject() {
    // 可行正例：compact 网格闭环通过。
    let spec = positive_spec();
    let opts = BrickwallAnalysisOptions::compact(spec.taps);
    let d = design_checked_with(&spec, &opts).expect("design_checked 应通过");
    assert!(verify_compact(&spec, &d.coeffs).passed());
    // 不可行（L < L_req）→ E_FIR_BUDGET。
    let bad = BrickwallSpec::lowpass(48_000.0, 1024, 9_000.0, 9_100.0).with_attenuation(100.0);
    let e = expect_code(
        design_checked_with(&bad, &BrickwallAnalysisOptions::compact(1024)),
        "E_FIR_BUDGET",
    );
    assert!(!e.detail().is_empty());
    // 未实现 method → E_FIR_FAMILY。
    let m = positive_spec().with_method(BrickwallMethod::Remez);
    expect_code(
        design_checked_with(&m, &BrickwallAnalysisOptions::compact(256)),
        "E_FIR_FAMILY",
    );
}

#[test]
fn t36_error_codes_and_display() {
    // 每个错误码字符串与 Display 稳定（§2.5 消费方按 code 分支）。
    let cases = [
        (BrickwallError::Band("b".into()), "E_FIR_BAND"),
        (BrickwallError::Delay("d".into()), "E_FIR_DELAY"),
        (BrickwallError::Taps("t".into()), "E_FIR_TAPS"),
        (BrickwallError::Budget("bu".into()), "E_FIR_BUDGET"),
        (BrickwallError::Numeric("n".into()), "E_FIR_NUMERIC"),
        (BrickwallError::PhaseClass("p".into()), "E_FIR_PHASE_CLASS"),
        (BrickwallError::Family("f".into()), "E_FIR_FAMILY"),
        (BrickwallError::Reference("r".into()), "E_FIR_REF"),
    ];
    for (e, code) in cases {
        assert_eq!(e.code(), code);
        let s = format!("{e}");
        assert!(s.starts_with(code), "display={s}");
        assert!(!e.detail().is_empty());
    }
}
