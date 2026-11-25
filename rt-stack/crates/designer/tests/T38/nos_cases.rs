// T38 私有测试：NOS（直通/旁路）= delta + 显式旁路 + 后置输出保护不被绕过 + 无 DAC 复刻承诺。
//
// 本文件由 crates/designer/src/nos.rs 的 #[cfg(test)] 子模块通过 include! 编译执行
// （cargo 不自动发现 tests/<Txx>/ 子目录；public_registration.rs 属 integrator，不改）。
// 运行：cargo test -p khaen-designer nos
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
fn expect_code<T: std::fmt::Debug>(result: Result<T, NosError>, code: &str) -> NosError {
    match result {
        Ok(v) => panic!("期望错误 {code}，实际成功 {v:?}"),
        Err(e) => {
            assert_eq!(e.code(), code, "错误码不符：{e}");
            e
        }
    }
}

fn delta_spec() -> NosSpec {
    NosSpec::reference_preset_48k()
}

fn full_protection() -> PostOutputProtection {
    PostOutputProtection::fully_enabled()
}

// ---------------------------------------------------------------------------
// 常量 / 参数契约（§3.7、§2.5、§5.1）
// ---------------------------------------------------------------------------

#[test]
fn t38_constants_contract() {
    assert_eq!(FAMILY, "nos");
    assert_eq!(NOS_TAPS, 1);
    assert_eq!(NOS_COEFFICIENT, 1.0);
    assert_eq!(NOS_DELAY_SAMPLES, 0.0);
    assert_eq!(IDENTITY_TOLERANCE, 1e-12);
    assert_eq!(REFERENCE_TOLERANCE_DB, 0.01);
    assert_eq!(FILTERING_NONE, "none");
    assert_eq!(DAC_REPLICATION_NONE, "none");
    assert_eq!(BYPASS_SCOPE, "p_post.output_target_fir");
    assert_eq!(F32_BYTES, 4);
    assert_eq!(REGULAR_PROFILE_TAPS_CAP, 4096);
    assert_eq!(EXTENDED_PROFILE_TAPS_CAP, 16_384);
    // §3.7 报告必须句与保护声明。
    assert!(NO_DAC_REPLICATION_STATEMENT.contains("不改变/不模拟 MOTU DAC 内部重建滤波"));
    assert!(NO_DAC_REPLICATION_COMMITMENT.contains("不承诺复刻"));
    assert!(PROTECTION_NOT_BYPASSED_STATEMENT.contains("limiter/clip/ramp"));
    assert!(PROTECTION_NOT_BYPASSED_STATEMENT.contains("不被 bypass 绕过"));
    // §2.2/§3.7 filtering 字段差异已登记。
    assert!(FILTERING_FIELD_NOTE.contains("x_filtering"));
}

#[test]
fn t38_spec_delta_defaults() {
    let spec = NosSpec::delta(48_000.0);
    assert_eq!(spec.fs, 48_000.0);
    assert!(spec.bypass, "NOS 必须是显式 bypass");
    assert_eq!(spec.taps, 1);
    assert_eq!(spec.declared_delay_samples, 0.0);
    assert!(spec.requested_band.is_none());
    assert!(spec.requested_window.is_none());
    assert!(spec.recorded_target.is_none());
    assert_eq!(spec.nyquist_hz(), 24_000.0);
    let preset = NosSpec::reference_preset_48k();
    assert_eq!(preset, spec);
    // 字段可直改（拒绝路径靠它构造非法输入）。
    let mut bad = spec.clone();
    bad.taps = 2;
    assert_eq!(bad.taps, 2);
}

#[test]
fn t38_rejects_taps_not_one() {
    for taps in [0_usize, 2, 63, 1024, 16_384] {
        let e = expect_code(validate(&delta_spec().with_taps(taps)), "E_FIR_NOS");
        assert!(
            e.detail().contains("taps 必须=1"),
            "taps={taps} detail: {}",
            e.detail()
        );
    }
    // taps=1 本身通过。
    assert!(validate(&delta_spec()).is_ok());
}

#[test]
fn t38_rejects_implicit_bypass() {
    let e = expect_code(validate(&delta_spec().with_bypass(false)), "E_FIR_NOS");
    assert!(e.detail().contains("显式 bypass"), "detail: {}", e.detail());
}

#[test]
fn t38_rejects_band_params() {
    let band = NosBandParams::new([20.0, 19_000.0], [20_500.0, 23_952.0], 0.2, 80.0);
    let e = expect_code(
        validate(&delta_spec().with_band_params(band.clone())),
        "E_FIR_NOS",
    );
    assert!(e.detail().contains("频带参数"), "detail: {}", e.detail());
    assert!(
        e.detail().contains("19"),
        "detail 应带出非法参数：{}",
        e.detail()
    );
    // 任一字段非默认都应被携带并拒绝（这里四个字段都已设置）。
    assert_eq!(band.passband_hz, [20.0, 19_000.0]);
    assert_eq!(band.stopband_hz, [20_500.0, 23_952.0]);
    assert_eq!(band.ripple_db, 0.2);
    assert_eq!(band.attenuation_db, 80.0);
}

#[test]
fn t38_rejects_window_params() {
    for window in [
        NosRequestedWindow::Kaiser,
        NosRequestedWindow::Hann,
        NosRequestedWindow::BlackmanHarris,
        NosRequestedWindow::LeastSquares,
    ] {
        let e = expect_code(validate(&delta_spec().with_window(window)), "E_FIR_NOS");
        assert!(e.detail().contains("窗参数"), "detail: {}", e.detail());
        assert!(
            e.detail().contains(window.schema_name()),
            "detail 应带出窗名：{}",
            e.detail()
        );
    }
}

#[test]
fn t38_rejects_nonzero_delay() {
    // §2.2 家族规则 nos=0：任何 delay（正/负/分数）都 E_FIR_DELAY，delay 归 T40。
    for delay in [0.5_f64, 1.0, -1.0, 1024.0] {
        let e = expect_code(
            validate(&delta_spec().with_declared_delay_samples(delay)),
            "E_FIR_DELAY",
        );
        assert!(
            e.detail().contains("delay_samples 必须=0"),
            "detail: {}",
            e.detail()
        );
        assert!(
            e.detail().contains("T40"),
            "detail 应指向 T40：{}",
            e.detail()
        );
    }
    // NaN 也拒绝（有限性检查）。
    expect_code(
        validate(&delta_spec().with_declared_delay_samples(f64::NAN)),
        "E_FIR_DELAY",
    );
    // 显式 0 通过。
    assert!(validate(&delta_spec().with_declared_delay_samples(0.0)).is_ok());
}

#[test]
fn t38_rejects_bad_fs() {
    for fs in [
        0.0_f64,
        -48_000.0,
        7_999.0,
        384_001.0,
        f64::NAN,
        f64::INFINITY,
    ] {
        expect_code(validate(&NosSpec::delta(fs)), "E_FIR_BAND");
    }
    for fs in [8_000.0_f64, 48_000.0, 96_000.0, 384_000.0] {
        assert!(validate(&NosSpec::delta(fs)).is_ok(), "fs={fs} 应可用");
    }
}

#[test]
fn t38_accepts_recorded_target_only() {
    // §6.10：nos 的目标「可选（仅记录）」，允许提交，但不参与设计。
    let spec = delta_spec().with_recorded_target(NosTargetRecord::new("target.json", true, false));
    let validated = validate(&spec).expect("记录目标不应导致拒收");
    assert!(validated.target_recorded_only);
    assert!(validated.spec.recorded_target.is_some());
    let design = design(&spec).expect("带记录目标仍应可设计");
    assert!(
        !design.target_participated_in_design,
        "目标只记录、不参与设计（§6.10）"
    );
    assert_eq!(design.coeffs, vec![1.0]);
}

#[test]
fn t38_validated_derived_values() {
    let validated = validate(&delta_spec()).expect("delta 应通过 validate");
    assert_eq!(validated.taps, 1);
    assert_eq!(validated.delay_samples, 0.0);
    assert_eq!(validated.delay_ms, 0.0);
    assert_eq!(validated.nyquist_hz, 24_000.0);
    assert_eq!(validated.filtering, "none");
    assert_eq!(validated.replacement, BypassReplacement::Delta);
    assert_eq!(validated.budget, NosBudget::identity());
    assert_eq!(validated.budget.profile, NosProfile::Regular);
    assert_eq!(validated.budget.route_taps_cap, 4096);
    assert_eq!(validated.budget.ram_bytes_f32, 4);
    assert!(validated.budget.fits_regular_profile);
    assert!(validated.budget.fits_extended_profile);
    assert!(validated.protection_required);
    assert!(validated.target_recorded_only);
}

// ---------------------------------------------------------------------------
// 设计 / V18 / 响应 / 端点 / 振铃（§1.2–§1.5、§2.4、§3.7、§9 V18）
// ---------------------------------------------------------------------------

#[test]
fn t38_design_v18_identity() {
    let d = design(&delta_spec()).expect("NOS delta 设计应成功");
    assert_eq!(d.coeffs, vec![1.0], "§9 V18: h=[1.0]");
    assert_eq!(d.taps(), 1);
    assert_eq!(d.replacement, BypassReplacement::Delta);
    assert_eq!(d.filtering, "none");
    assert_eq!(d.delay_samples, 0.0);
    assert_eq!(d.delay_ms, 0.0);
    assert_eq!(d.reference_hz, 0.0);
    assert_eq!(d.reference_gain_db, 0.0);
    assert_eq!(d.peak_abs, 1.0);
    assert_eq!(d.sum_coeffs, 1.0);
    assert_eq!(d.sum_abs_coeffs, 1.0);
    assert_approx(d.peak_margin_hint_db, 0.0, 1e-15, "峰值余量建议");
    assert_eq!(d.effective_support(), (0, 0, 1));
    assert_eq!(d.coeffs_f32(), vec![1.0_f32]);
    assert_eq!(d.bypass_scope, BypassScope::output_target_fir_only());
    assert_eq!(d.bypass_scope.stage, OutputStage::TargetFir);
    assert_eq!(d.bypass_scope.replacement, BypassReplacement::Delta);
    assert!(d.bypass_scope.protection_required);
    assert!(d.verification.passed());
    assert!(d.verification.failures.is_empty());
    assert_eq!(d.verification.identity_error, 0.0);
    assert_eq!(
        BypassReplacement::Delta.description(),
        "h=[1.0]（delta at n=0，直通）"
    );
}

#[test]
fn t38_design_deterministic() {
    // §1.6 确定性：同输入同版本两次 design 逐位一致（无随机数/时间戳）。
    let a = design(&delta_spec()).expect("第一次");
    let b = design(&delta_spec()).expect("第二次");
    assert_eq!(a.coeffs.len(), b.coeffs.len());
    for (x, y) in a.coeffs.iter().zip(b.coeffs.iter()) {
        assert_eq!(x.to_bits(), y.to_bits(), "系数逐位一致");
    }
    assert_eq!(a, b);
}

#[test]
fn t38_design_response_flat() {
    let d = design(&delta_spec()).expect("设计");
    for f in [20.0_f64, 100.0, 1_000.0, 10_000.0, 20_000.0, 23_952.0] {
        let r = response_at(&d.coeffs, 48_000.0, f);
        assert_approx(r.magnitude_db(), 0.0, 1e-9, "|H| dB");
        assert_approx(r.phase_deg(), 0.0, 1e-9, "phase deg");
        assert_approx(r.magnitude(), 1.0, 1e-12, "|H|");
    }
    assert_eq!(d.verification.response.grid_points, 8_192);
    assert!(d.verification.response.max_abs_mag_db <= 1e-9);
    assert!(d.verification.response.max_abs_phase_deg <= 1e-9);
    assert_approx(
        d.verification.reference.measured_gain_db,
        0.0,
        1e-9,
        "DC 参考 0 dB",
    );
    assert_approx(d.verification.reference.error_db, 0.0, 1e-9, "参考误差");
}

#[test]
fn t38_design_endpoints_48k() {
    let d = design(&delta_spec()).expect("设计");
    let eps = &d.verification.endpoints;
    assert_eq!(eps.f20.status(), "measured");
    assert_eq!(eps.f20k.status(), "measured");
    let (f20, mag20, phase20) = eps.f20.measured().expect("f20 measured");
    assert_eq!(f20, 20.0);
    assert_approx(mag20, 0.0, 1e-9, "f20 mag");
    assert_approx(phase20, 0.0, 1e-9, "f20 phase");
    let (f20k, mag20k, phase20k) = eps.f20k.measured().expect("f20k measured");
    assert_eq!(f20k, 20_000.0);
    assert_approx(mag20k, 0.0, 1e-9, "f20k mag");
    assert_approx(phase20k, 0.0, 1e-9, "f20k phase");
}

#[test]
fn t38_design_endpoints_low_fs() {
    // fs=32000：0.499·fs=15968 < 20000 → f20k 必须如实标 not_applicable_at_fs。
    let d32 = design(&NosSpec::delta(32_000.0)).expect("32k 设计");
    assert_eq!(d32.verification.endpoints.f20.status(), "measured");
    assert_eq!(
        d32.verification.endpoints.f20k.status(),
        "not_applicable_at_fs"
    );
    match &d32.verification.endpoints.f20k {
        Endpoint::NotApplicableAtFs { reason } => {
            assert!(reason.contains("0.499"), "reason: {reason}");
            assert!(reason.contains("不可测"), "reason: {reason}");
        }
        other => panic!("应为 not_applicable_at_fs，实际 {other:?}"),
    }
    // fs=44100：0.499·fs=22005.9 >= 20000 → 可测。
    let d44 = design(&NosSpec::delta(44_100.0)).expect("44.1k 设计");
    assert_eq!(d44.verification.endpoints.f20k.status(), "measured");
}

#[test]
fn t38_design_ringing_not_applicable() {
    // §1.4：L=1 时主峰两侧排除窗（48k 下 W=16）都越界 → n/a，不得硬算。
    let r = ringing_stats(&[1.0], 48_000.0);
    assert_eq!(r.exclusion_samples, 16);
    assert!(!r.applicable);
    assert!(r.pre_ring_db.is_none());
    assert!(r.post_ring_db.is_none());
    assert!(
        r.reason.as_deref().unwrap_or("").contains("支撑过短"),
        "reason: {:?}",
        r.reason
    );
    let d = design(&delta_spec()).expect("设计");
    assert_eq!(d.ringing(), r);
    // 长支撑路径仍可算（防御性：模块不是只会 n/a）—— 手工 3-tap 对称序列。
    let r3 = ringing_stats(&[0.25, 0.5, 0.25], 48_000.0);
    assert!(!r3.applicable, "L=3 < W 两侧仍 n/a");
    // 更长的中心峰序列在 48k 且 L=64 时两侧可算。
    let mut long = vec![0.0_f64; 64];
    long[32] = 1.0;
    let r64 = ringing_stats(&long, 48_000.0);
    assert!(r64.applicable, "L=64 且主峰居中应可算");
    assert!(r64.pre_ring_db.unwrap() < -200.0);
    assert!(r64.post_ring_db.unwrap() < -200.0);
}

// ---------------------------------------------------------------------------
// 独立验证（§3.7 判据、E_FIR_* 硬/软错误区分）
// ---------------------------------------------------------------------------

#[test]
fn t38_verify_positive_passes() {
    let v = verify_nos(&delta_spec(), &[1.0]).expect("合法 delta 应通过 verify");
    assert!(v.passed());
    assert!(v.failures.is_empty());
    assert!(v.checks.finite);
    assert!(v.checks.taps);
    assert!(v.checks.identity);
    assert!(v.checks.delay_zero);
    assert!(v.checks.flat_response);
    assert!(v.checks.reference);
    assert!(v.checks.endpoints);
    assert!(v.checks.budget);
    assert!(v.checks.filtering_none);
    assert_eq!(v.taps, 1);
    assert_eq!(v.replacement, BypassReplacement::Delta);
    assert_eq!(v.filtering, "none");
    assert!(v.dac_replication.is_none());
    assert!(!v.target_participated_in_design);
    assert!(v.target_recorded.is_none());
    assert_eq!(v.budget, NosBudget::identity());
}

#[test]
fn t38_verify_rejects_wrong_length() {
    // 系数文件样本数与声明 taps=1 不符 → E_FIR_TAPS（硬错误）。
    let e = expect_code(verify_nos(&delta_spec(), &[1.0, 0.0]), "E_FIR_TAPS");
    assert!(
        e.detail().contains("样本数必须=1"),
        "detail: {}",
        e.detail()
    );
    expect_code(verify_nos(&delta_spec(), &[]), "E_FIR_TAPS");
}

#[test]
fn t38_verify_rejects_nan_inf() {
    expect_code(verify_nos(&delta_spec(), &[f64::NAN]), "E_FIR_NUMERIC");
    expect_code(verify_nos(&delta_spec(), &[f64::INFINITY]), "E_FIR_NUMERIC");
    expect_code(
        verify_nos(&delta_spec(), &[f64::NEG_INFINITY]),
        "E_FIR_NUMERIC",
    );
}

#[test]
fn t38_verify_rejects_non_identity() {
    // 长度 1 但非 1.0：结构合法 → 报告 checks 失败，criteria_to_error 映射 E_FIR_NOS。
    for bad in [0.0_f64, -1.0, 1.000_000_1, 1.1] {
        let v = verify_nos(&delta_spec(), &[bad]).expect("长度/finite 合法，应返回报告");
        assert!(!v.passed(), "h={bad} 不应通过");
        assert!(!v.checks.identity, "h={bad} 恒等判据应为 false");
        assert!(!v.failures.is_empty(), "h={bad} 应有 failures");
        assert!(v.failures.iter().any(|s| s.contains("恒等失败")));
        let e = v.criteria_to_error();
        assert_eq!(e.code(), "E_FIR_NOS");
    }
    // h=[0] 在 §2.5 属 nos 例外（max|h|=0 不是 E_FIR_NUMERIC），归恒等失败 E_FIR_NOS。
    let v0 = verify_nos(&delta_spec(), &[0.0]).expect("h=[0] 不触发 E_FIR_NUMERIC");
    assert_eq!(v0.criteria_to_error().code(), "E_FIR_NOS");
}

#[test]
fn t38_identity_tolerance_boundary() {
    // 1e-12 容差边界：用可精确表达的差值验证（f64 无法精确表达 1+1e-12）。
    let pass = verify_nos(&delta_spec(), &[1.0 + 5e-13]).expect("报告");
    assert!(pass.checks.identity);
    assert_approx(pass.identity_error, 5e-13, 1e-15, "1+5e-13 的恒等误差");
    let fail = verify_nos(&delta_spec(), &[1.0 + 2e-12]).expect("报告");
    assert!(!fail.checks.identity);
    assert!(!fail.passed());
    // 精确 1.0 的误差为 0。
    let exact = verify_nos(&delta_spec(), &[1.0]).expect("报告");
    assert_eq!(exact.identity_error, 0.0);
}

#[test]
fn t38_verify_target_recorded_but_not_designed() {
    let with_target =
        delta_spec().with_recorded_target(NosTargetRecord::new("room-target.csv", true, true));
    let without_target = delta_spec();
    let a = design(&with_target).expect("带 target 设计");
    let b = design(&without_target).expect("无 target 设计");
    // 系数逐位不随 target 变化（§6.10 不参与设计）。
    for (x, y) in a.coeffs.iter().zip(b.coeffs.iter()) {
        assert_eq!(x.to_bits(), y.to_bits());
    }
    assert!(!a.target_participated_in_design);
    assert_eq!(
        a.target_recorded,
        Some(NosTargetRecord::new("room-target.csv", true, true))
    );
    assert!(b.target_recorded.is_none());
    // 报告记录 target 但不消费：verification 仍全过。
    assert!(a.verification.passed());
    assert_eq!(a.verification.target_recorded, a.target_recorded);
    assert!(!a.verification.target_participated_in_design);
}

// ---------------------------------------------------------------------------
// 后置输出保护 / 旁路计划（§3.7、§5.1；DEC「NOS 不是绕保护」）
// ---------------------------------------------------------------------------

#[test]
fn t38_protection_fully_enabled_plan() {
    let plan = plan_bypass(48_000.0, full_protection()).expect("全保护应可旁路");
    assert_eq!(plan.fs, 48_000.0);
    assert_eq!(plan.scope, BypassScope::output_target_fir_only());
    assert_eq!(plan.bypassed_stage(), OutputStage::TargetFir);
    assert!(plan.protection.is_fully_enabled());
    assert_eq!(
        plan.protection.disabled_kinds(),
        Vec::<ProtectionKind>::new()
    );
    // 只旁路目标 FIR；保护/增益/延迟/极性/scatter 一律不旁路。
    assert!(plan.bypasses(OutputStage::TargetFir));
    for stage in [
        OutputStage::Protection,
        OutputStage::Gain,
        OutputStage::Delay,
        OutputStage::Polarity,
        OutputStage::Crossover,
        OutputStage::ModalPeq,
        OutputStage::InputSelect,
        OutputStage::LowBandMatrix,
        OutputStage::Scatter,
    ] {
        assert!(!plan.bypasses(stage), "{} 不得被旁路", stage.schema_name());
    }
    assert!(plan.protects(OutputStage::Protection));
    assert!(!plan.protects(OutputStage::TargetFir));
    assert!(plan.protection_after_bypass());
    assert!(plan.preserves_protection());
    assert_eq!(plan.chain, canonical_output_chain());
    assert_eq!(plan.stage_index(OutputStage::TargetFir), Some(4));
    assert_eq!(plan.stage_index(OutputStage::Protection), Some(8));
    assert_eq!(plan.stage_index(OutputStage::Scatter), Some(9));
    let text = plan.report_text();
    assert!(text.contains("p_post.output_target_fir"), "text: {text}");
    assert!(text.contains("protection_preserved=true"), "text: {text}");
    // fs 非法 → E_FIR_BAND（与 design 同一 fs 口径）。
    expect_code(plan_bypass(0.0, full_protection()), "E_FIR_BAND");
    expect_code(plan_bypass(f64::NAN, full_protection()), "E_FIR_BAND");
}

#[test]
fn t38_protection_rejects_disabled_kinds() {
    let cases = [
        (
            PostOutputProtection {
                limiter: false,
                clip: true,
                ramp: true,
            },
            "limiter",
        ),
        (
            PostOutputProtection {
                limiter: true,
                clip: false,
                ramp: true,
            },
            "clip",
        ),
        (
            PostOutputProtection {
                limiter: true,
                clip: true,
                ramp: false,
            },
            "ramp",
        ),
    ];
    for (protection, missing_name) in cases {
        assert!(!protection.is_fully_enabled());
        assert_eq!(protection.disabled_kinds().len(), 1);
        let e = expect_code(plan_bypass(48_000.0, protection), "E_FIR_NOS");
        assert!(
            e.detail().contains("后置输出保护不得被 NOS 旁路"),
            "detail: {}",
            e.detail()
        );
        assert!(e.detail().contains(missing_name), "detail: {}", e.detail());
    }
    // 三项全关：detail 列出全部三类。
    let none_on = PostOutputProtection {
        limiter: false,
        clip: false,
        ramp: false,
    };
    assert_eq!(none_on.disabled_kinds().len(), 3);
    let e = expect_code(plan_bypass(48_000.0, none_on), "E_FIR_NOS");
    for name in ["limiter", "clip", "ramp"] {
        assert!(e.detail().contains(name), "detail: {}", e.detail());
    }
    // 保护类枚举顺序与 schema 名固定。
    assert_eq!(ProtectionKind::all().len(), 3);
    assert_eq!(ProtectionKind::Limiter.schema_name(), "limiter");
    assert_eq!(ProtectionKind::Clip.schema_name(), "clip");
    assert_eq!(ProtectionKind::Ramp.schema_name(), "ramp");
}

#[test]
fn t38_protection_rejects_missing_protection_stage() {
    let chain = [
        OutputStage::InputSelect,
        OutputStage::LowBandMatrix,
        OutputStage::TargetFir,
        OutputStage::Scatter,
    ];
    let e = expect_code(
        plan_bypass_with_chain(48_000.0, &chain, full_protection()),
        "E_FIR_NOS",
    );
    assert!(e.detail().contains("protection"), "detail: {}", e.detail());
    assert!(e.detail().contains("缺少必需级"), "detail: {}", e.detail());
}

#[test]
fn t38_protection_rejects_order_violation() {
    // 保护被挪到目标 FIR 之前（旁路会把保护甩在旁路级之前/之后不一致）。
    let chain = [
        OutputStage::InputSelect,
        OutputStage::LowBandMatrix,
        OutputStage::Protection,
        OutputStage::TargetFir,
        OutputStage::Scatter,
    ];
    let e = expect_code(
        plan_bypass_with_chain(48_000.0, &chain, full_protection()),
        "E_FIR_NOS",
    );
    assert!(e.detail().contains("顺序"), "detail: {}", e.detail());
    // 保护被挪到 scatter 之后。
    let chain2 = [
        OutputStage::InputSelect,
        OutputStage::TargetFir,
        OutputStage::Scatter,
        OutputStage::Protection,
    ];
    let e2 = expect_code(
        plan_bypass_with_chain(48_000.0, &chain2, full_protection()),
        "E_FIR_NOS",
    );
    assert!(e2.detail().contains("顺序"), "detail: {}", e2.detail());
}

#[test]
fn t38_protection_rejects_missing_target_fir_or_scatter() {
    let no_target = [
        OutputStage::InputSelect,
        OutputStage::Protection,
        OutputStage::Scatter,
    ];
    let e = expect_code(
        plan_bypass_with_chain(48_000.0, &no_target, full_protection()),
        "E_FIR_NOS",
    );
    assert!(
        e.detail().contains("output_target_fir"),
        "detail: {}",
        e.detail()
    );
    let no_scatter = [
        OutputStage::InputSelect,
        OutputStage::TargetFir,
        OutputStage::Protection,
    ];
    let e2 = expect_code(
        plan_bypass_with_chain(48_000.0, &no_scatter, full_protection()),
        "E_FIR_NOS",
    );
    assert!(e2.detail().contains("scatter"), "detail: {}", e2.detail());
}

#[test]
fn t38_protection_rejects_duplicate_stages() {
    let dup_target = [
        OutputStage::TargetFir,
        OutputStage::Gain,
        OutputStage::Protection,
        OutputStage::TargetFir,
        OutputStage::Scatter,
    ];
    let e = expect_code(
        plan_bypass_with_chain(48_000.0, &dup_target, full_protection()),
        "E_FIR_NOS",
    );
    assert!(e.detail().contains("重复"), "detail: {}", e.detail());
    let dup_protection = [
        OutputStage::TargetFir,
        OutputStage::Protection,
        OutputStage::Protection,
        OutputStage::Scatter,
    ];
    expect_code(
        plan_bypass_with_chain(48_000.0, &dup_protection, full_protection()),
        "E_FIR_NOS",
    );
}

#[test]
fn t38_canonical_output_chain_contract() {
    // §5.1 规范链序（只取相关节点子序列）。
    let chain = OutputStage::canonical_order();
    assert_eq!(chain.len(), 10);
    assert_eq!(chain[0], OutputStage::InputSelect);
    assert_eq!(chain[1], OutputStage::LowBandMatrix);
    assert_eq!(chain[2], OutputStage::Crossover);
    assert_eq!(chain[3], OutputStage::ModalPeq);
    assert_eq!(chain[4], OutputStage::TargetFir);
    assert_eq!(chain[5], OutputStage::Gain);
    assert_eq!(chain[6], OutputStage::Delay);
    assert_eq!(chain[7], OutputStage::Polarity);
    assert_eq!(chain[8], OutputStage::Protection);
    assert_eq!(chain[9], OutputStage::Scatter);
    assert_eq!(canonical_output_chain(), chain.to_vec());
    // schema 名固定（报告/图计划消费）。
    assert_eq!(OutputStage::TargetFir.schema_name(), "output_target_fir");
    assert_eq!(OutputStage::Protection.schema_name(), "protection");
    assert_eq!(OutputStage::Scatter.schema_name(), "scatter");
    // 自定义合法链（子集）也应接受：目标 FIR → 保护 → scatter。
    let custom = [
        OutputStage::TargetFir,
        OutputStage::Protection,
        OutputStage::Scatter,
    ];
    let plan =
        plan_bypass_with_chain(48_000.0, &custom, full_protection()).expect("合法自定义链应接受");
    assert!(plan.preserves_protection());
    assert_eq!(plan.chain, custom.to_vec());
}

// ---------------------------------------------------------------------------
// manifest / 报告 / 无 DAC 复刻记录（§3.7、WBS T38）
// ---------------------------------------------------------------------------

#[test]
fn t38_manifest_declaration_fields() {
    let d = design(&delta_spec()).expect("设计");
    let m = d.manifest_declaration();
    assert_eq!(
        m,
        NosManifestDeclaration {
            schema: "tspi-fir/v1",
            family: "nos",
            taps: 1,
            delay_samples: 0.0,
            delay_ms: 0.0,
            filtering: "none",
            dac_replication: "none",
            protection_bypassed: false,
            response_flat: true,
        }
    );
    assert_eq!(m, manifest_declaration(&d));
    assert_eq!(m.schema, "tspi-fir/v1");
    assert_eq!(m.family, FAMILY);
    assert_eq!(m.taps, d.taps());
    assert_eq!(m.filtering, FILTERING_NONE);
    assert!(!m.protection_bypassed, "NOS 不得声明保护被旁路");
    assert!(m.response_flat, "response 必须全 0 dB/0°");
}

#[test]
fn t38_no_dac_replication_record() {
    let rec = DacReplicationRecord::software_bypass_only();
    assert_eq!(rec.claim, DacReplicationClaim::None);
    assert_eq!(rec.claim.schema_name(), "none");
    assert!(rec.is_none());
    assert_eq!(rec.statement, NO_DAC_REPLICATION_STATEMENT);
    assert_eq!(rec.commitment, NO_DAC_REPLICATION_COMMITMENT);
    assert!(rec.statement.contains("不模拟 MOTU DAC 内部重建滤波"));
    assert!(rec.commitment.contains("不承诺复刻任何专有 DAC"));
    let d = design(&delta_spec()).expect("设计");
    assert_eq!(d.dac_replication, rec);
    // 复刻承诺不是「有 DAC 行为」：claim schema 值为 none。
    assert_eq!(d.dac_replication.claim.schema_name(), DAC_REPLICATION_NONE);
}

#[test]
fn t38_report_text_contains_required_statements() {
    let d = design(&delta_spec()).expect("设计");
    let text = d.report_text();
    assert!(text.contains("family=nos"), "text: {text}");
    assert!(text.contains("filtering=none"), "text: {text}");
    assert!(text.contains("replacement=delta"), "text: {text}");
    assert!(text.contains("dac_replication=none"), "text: {text}");
    assert!(text.contains(NO_DAC_REPLICATION_STATEMENT), "text: {text}");
    assert!(text.contains(NO_DAC_REPLICATION_COMMITMENT), "text: {text}");
    assert!(
        text.contains(PROTECTION_NOT_BYPASSED_STATEMENT),
        "text: {text}"
    );
    assert!(
        text.contains("bypass_scope=p_post.output_target_fir"),
        "text: {text}"
    );
    // 保护声明本身可被下游直接复用。
    assert!(PROTECTION_NOT_BYPASSED_STATEMENT.contains("不被 bypass 绕过"));
}

#[test]
fn t38_error_codes_and_display() {
    let cases: [(NosError, &str); 5] = [
        (NosError::Taps("x".to_string()), "E_FIR_TAPS"),
        (NosError::Numeric("x".to_string()), "E_FIR_NUMERIC"),
        (NosError::Delay("x".to_string()), "E_FIR_DELAY"),
        (NosError::Band("x".to_string()), "E_FIR_BAND"),
        (NosError::Nos("x".to_string()), "E_FIR_NOS"),
    ];
    for (err, code) in cases {
        assert_eq!(err.code(), code);
        assert_eq!(err.detail(), "x");
        assert_eq!(err.to_string(), format!("{code}: x"));
    }
    let boxed: Box<dyn std::error::Error> = Box::new(NosError::Nos("boxed".to_string()));
    assert!(boxed.to_string().contains("E_FIR_NOS"));
    assert!(boxed.to_string().contains("boxed"));
}

#[test]
fn t38_criteria_to_error_mapping() {
    let base = verify_nos(&delta_spec(), &[1.0]).expect("基础报告");
    assert!(base.passed());
    // 全过时 criteria_to_error 是兜底（不用于成功路径），仍是 E_FIR_NOS。
    assert_eq!(base.criteria_to_error().code(), "E_FIR_NOS");

    let mut v = base.clone();
    v.checks.identity = false;
    assert_eq!(v.criteria_to_error().code(), "E_FIR_NOS");

    let mut v = base.clone();
    v.checks.taps = false;
    assert_eq!(v.criteria_to_error().code(), "E_FIR_TAPS");

    let mut v = base.clone();
    v.checks.finite = false;
    assert_eq!(v.criteria_to_error().code(), "E_FIR_NUMERIC");

    let mut v = base.clone();
    v.checks.flat_response = false;
    v.failures.push("flat".to_string());
    assert_eq!(v.criteria_to_error().code(), "E_FIR_NOS");
    assert!(v.criteria_to_error().detail().contains("flat_response"));
}

#[test]
fn t38_budget_and_design_checked() {
    let budget = NosBudget::identity();
    assert_eq!(budget.profile.schema_name(), "regular");
    assert_eq!(budget.route_taps_cap, REGULAR_PROFILE_TAPS_CAP);
    assert_eq!(budget.ram_bytes_f32, F32_BYTES);
    assert!(budget.fits_regular_profile && budget.fits_extended_profile);
    // design_checked 与 design 等价闭环。
    let a = design(&delta_spec()).expect("design");
    let b = design_checked(&delta_spec()).expect("design_checked");
    assert_eq!(a, b);
    assert!(b.verification.passed());
    // 非法参数在 design_checked 同样拒绝。
    expect_code(design_checked(&delta_spec().with_taps(2)), "E_FIR_NOS");
    // effective_support 空系数/全零边界（防御性）。
    assert_eq!(effective_support(&[], 1e-6), (0, 0, 0));
    assert_eq!(effective_support(&[0.0, 0.0], 1e-6), (0, 1, 2));
    assert_eq!(effective_support(&[1.0], 1e-6), (0, 0, 1));
}
