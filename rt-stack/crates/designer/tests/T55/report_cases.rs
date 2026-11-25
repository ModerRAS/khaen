// T55 私有测试用例（通过 crates/designer/src/report.rs 末尾 #[cfg(test)] include! 编译执行）。
//
// 归属：WBS T55 / task-96，owner rt-fir-reporter2；写域 crates/designer/src/report.rs +
// crates/designer/tests/T55 + rt-stack/evidence/T55。
//
// 本文件只测试 report 模块的独立 oracle / 七族报告 / 端点 / 拒绝 / 互斥 / 预算；真实载体、板端、
// ALSA/RT/soak、WAV/manifest 落盘均 NOT_RUN（见 evidence/T55/MANIFEST.md）。
//
// 独立性：被测实现是 report.rs 自己的 direct DTFT + 解析 GD + 自带 FFT；线性/最小相位/砖墙/
// apodizing/nos 模块的标量函数只作为【交叉对照】（两个独立实现互证），不参与被测报告路径。

use crate::{apodizing, brickwall, linear, min_fast, min_slow, nos};

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

fn assert_close(what: &str, a: f64, b: f64, tol: f64) {
    assert!(
        close(a, b, tol),
        "{what}: got {a:.12}, expected {b:.12} (tol {tol})"
    );
}

fn criterion<'a>(report: &'a FirReport, id: &str) -> &'a Criterion {
    report
        .criteria
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("criterion {id} not found in report for {}", report.family))
}

fn declared_linear(spec: &linear::LinearSpec, design: &linear::LinearDesign) -> DeclaredDesign {
    let mode = match spec.mode {
        linear::LinearMode::Lowpass => BandMode::Lowpass,
        linear::LinearMode::Highpass => BandMode::Highpass,
    };
    let symmetry = match design.symmetry {
        linear::Symmetry::Symmetric => SymmetryMode::Symmetric,
        linear::Symmetry::Antisymmetric => SymmetryMode::Antisymmetric,
    };
    DeclaredDesign::new(
        Family::Linear,
        spec.fs,
        spec.taps,
        FamilyParams::Linear {
            mode,
            passband_hz: spec.passband_hz,
            stopband_hz: spec.stopband_hz,
            ripple_db: spec.ripple_db,
            attenuation_db: spec.attenuation_db,
            symmetry,
        },
    )
    .with_delay(design.delay_samples)
    .with_reference(design.reference_hz, design.reference_gain_db)
    .with_method("linear::design_checked (Kaiser)")
}

fn declared_min_fast(design: &min_fast::MinFastDesign) -> DeclaredDesign {
    let p = &design.params;
    DeclaredDesign::new(
        Family::MinFast,
        p.fs,
        p.taps,
        FamilyParams::MinFast {
            passband_hz: p.passband_hz,
            stopband_hz: p.stopband_hz,
            ripple_db: p.ripple_db,
            attenuation_db: p.attenuation_db,
        },
    )
    .with_delay(0.0)
    .with_reference(0.0, 0.0)
    .with_method("min_fast::design (cepstrum)")
}

fn declared_min_slow(design: &min_slow::MinSlowDesign) -> DeclaredDesign {
    let p = &design.params;
    DeclaredDesign::new(
        Family::MinSlow,
        p.fs,
        p.taps,
        FamilyParams::MinSlow {
            passband_hz: p.passband_hz,
            nyquist_attenuation_db: p.nyquist_attenuation_db,
            ripple_db: p.ripple_db,
        },
    )
    .with_delay(0.0)
    .with_reference(0.0, 0.0)
    .with_method("min_slow::design (cepstrum)")
}

fn declared_apodizing(
    spec: &apodizing::ApodizingSpec,
    design: &apodizing::ApodizingDesign,
) -> DeclaredDesign {
    DeclaredDesign::new(
        Family::Apodizing,
        spec.fs,
        spec.taps,
        FamilyParams::Apodizing {
            passband_hz: spec.passband_hz,
            ripple_db: spec.ripple_db,
            nyquist_attenuation_db: spec.nyquist_attenuation_db,
            pre_ring_bound_db: spec.pre_ring_bound_db,
        },
    )
    .with_delay(design.delay_samples)
    .with_reference(design.reference_hz, design.reference_gain_db)
    .with_method("apodizing::design_checked (kaiser)")
}

fn declared_brickwall(
    spec: &brickwall::BrickwallSpec,
    design: &brickwall::BrickwallDesign,
) -> DeclaredDesign {
    let method = match spec.method {
        brickwall::BrickwallMethod::KaiserWindowedSinc => BrickwallMethod::KaiserWindowedSinc,
        brickwall::BrickwallMethod::LeastSquares => BrickwallMethod::LeastSquares,
        brickwall::BrickwallMethod::Remez => BrickwallMethod::Remez,
    };
    DeclaredDesign::new(
        Family::Brickwall,
        spec.fs,
        spec.taps,
        FamilyParams::Brickwall {
            mode: BandMode::Lowpass,
            passband_hz: spec.passband_hz,
            stopband_hz: spec.stopband_hz,
            ripple_db: spec.ripple_db,
            attenuation_db: spec.attenuation_db,
            method,
        },
    )
    .with_delay(design.delay_samples)
    .with_reference(design.reference_hz, design.reference_gain_db)
    .with_method(format!("brickwall::design_checked ({})", method.as_str()))
}

fn declared_nos(design: &nos::NosDesign) -> DeclaredDesign {
    DeclaredDesign::new(Family::Nos, design.spec.fs, 1, FamilyParams::nos())
        .with_delay(0.0)
        .with_reference(0.0, 0.0)
        .with_method("nos::design_checked")
}

/// V11 分数延迟向量：63-tap Kaiser(beta=8.96)，d=0.5 相对 (L-1)/2=31。
fn fractional_delay_h() -> Vec<f64> {
    let taps = 63usize;
    let beta = 8.96;
    let w = linear::kaiser_window(taps, beta);
    (0..taps)
        .map(|n| {
            let x = n as f64 - 31.5;
            linear::sinc_normalized(x) * w[n]
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 1) 独立 oracle：§9 手算向量（V1/V3/V4/V11 等）
// ---------------------------------------------------------------------------

#[test]
fn t55_oracle_hand_vectors_v1_v3_v4_v11() {
    // V3: h=[0.25,0.5,0.25]，L=3、Type I 对称、D=1。
    let h3 = [0.25, 0.5, 0.25];
    let p_dc = oracle_at(&h3, 48_000.0, 0.0);
    assert_close("V3 H(0) dB", p_dc.mag_db, 0.0, 1e-12);
    let p_q = oracle_at(&h3, 48_000.0, 0.25 * 48_000.0);
    assert_close("V3 H(0.25fs) dB", p_q.mag_db, -6.020_599_913_279_624, 1e-9);
    assert_close("V3 H(0.25fs) phase", wrap_deg(p_q.phase_deg), -90.0, 1e-9);
    assert_close("V3 GD", p_q.group_delay_samples, 1.0, 1e-9);
    // V1: 偶数长对称 FIR 在 0.5fs 处必为 0（幅度 floor -240 dB）。
    let n = 64usize;
    let even_sym: Vec<f64> = (0..n)
        .map(|i| {
            let k = if i < n / 2 { i } else { n - 1 - i };
            (k as f64 + 1.0).sin() / (k as f64 + 1.0)
        })
        .collect();
    let p_ny = oracle_at(&even_sym, 48_000.0, 24_000.0);
    assert!(
        p_ny.magnitude.abs() <= 1e-12,
        "V1 偶数长对称 FIR H(0.5fs) 必须 ~0，实测 {:.3e}",
        p_ny.magnitude
    );
    assert_close("V1 H(0.5fs) dB floor", p_ny.mag_db, -240.0, 1e-9);
    assert!(nyquist_value(&even_sym).abs() <= 1e-12);
    // V4: h=[1,2,3,2,1]/9。
    let h5: Vec<f64> = [1.0, 2.0, 3.0, 2.0, 1.0].iter().map(|v| v / 9.0).collect();
    assert_close(
        "V4 H(0) dB",
        oracle_at(&h5, 48_000.0, 0.0).mag_db,
        0.0,
        1e-12,
    );
    assert_close(
        "V4 H(0.5fs) dB",
        oracle_at(&h5, 48_000.0, 24_000.0).mag_db,
        20.0 * (1.0f64 / 9.0).log10(),
        1e-9,
    );
    assert_close(
        "V4 GD",
        oracle_at(&h5, 48_000.0, 1234.0).group_delay_samples,
        2.0,
        1e-9,
    );
    // V11: 分数延迟 63-tap Kaiser beta 8.96 d=0.5 => 总 GD 31.5 samples（20 Hz-10 kHz）。
    let hf = fractional_delay_h();
    for f in [20.0_f64, 100.0, 1_000.0, 5_000.0, 10_000.0] {
        let p = oracle_at(&hf, 48_000.0, f);
        assert_close(
            &format!("V11 GD @{f} Hz"),
            p.group_delay_samples,
            31.5,
            2.0e-3,
        );
        assert_close(&format!("V11 |H| dev @{f} Hz"), p.mag_db, 0.0, 0.05);
    }
    // V2: D=(L-1)/2 与 delay_ms 表（线性相位）。
    let cases = [
        (512usize, 255.5, 5.322_916_666_666_667),
        (1024, 511.5, 10.656_25),
        (4096, 2047.5, 42.656_25),
    ];
    for (taps, d_samples, d_ms) in cases {
        let d = (taps as f64 - 1.0) / 2.0;
        assert_close("V2 D", d, d_samples, 1e-12);
        assert_close("V2 delay_ms", d / 48_000.0 * 1000.0, d_ms, 1e-12);
    }
}

#[test]
fn t55_oracle_fft_roundtrip_and_min_phase_consistency() {
    // 自带 FFT：delta / 逆变换往返 / Parseval。
    let n = 1024usize;
    let mut re: Vec<f64> = (0..n).map(|i| ((i * 37) as f64 * 0.01).sin()).collect();
    let im: Vec<f64> = (0..n).map(|i| ((i * 11) as f64 * 0.02).cos()).collect();
    let re0 = re.clone();
    let im0 = im.clone();
    let mut im_mut = im.clone();
    fft_in_place(&mut re, &mut im_mut);
    let energy_time: f64 = re0.iter().zip(im0.iter()).map(|(a, b)| a * a + b * b).sum();
    let energy_freq: f64 = re
        .iter()
        .zip(im_mut.iter())
        .map(|(a, b)| a * a + b * b)
        .sum::<f64>()
        / n as f64;
    assert!(
        (energy_time - energy_freq).abs() <= 1e-9 * energy_time.max(1.0),
        "Parseval 失败"
    );
    ifft_in_place(&mut re, &mut im_mut);
    for (i, ((r, i2), (r0, i0))) in re
        .iter()
        .zip(im_mut.iter())
        .zip(re0.iter().zip(im0.iter()))
        .enumerate()
    {
        assert!(close(*r, *r0, 1e-9), "IFFT 实部 {i} 不往返");
        assert!(close(*i2, *i0, 1e-9), "IFFT 虚部 {i} 不往返");
    }

    // M2：min_fast 设计应 <= 2 deg；线性相位对称设计（非最小相位）应显著超限。
    let mut p = min_fast::MinFastParams::default();
    p.taps = 512;
    p.passband_hz = [20.0, 16_000.0];
    p.stopband_hz = [19_000.0, 23_952.0];
    p.prototype.taps = 8_192;
    p.cepstrum_fft = 8_192;
    let design = min_fast::design(&p).expect("min_fast design");
    let m2_report = min_phase_consistency_deg(&design.coefficients, 48_000.0, MIN_PHASE_FFT);
    let m2_module =
        min_fast::min_phase_consistency_deg(&design.coefficients, 48_000.0, MIN_PHASE_FFT)
            .expect("module M2");
    assert!(
        m2_report <= MIN_PHASE_LIMIT_DEG,
        "M2={m2_report} 应 <= 2 deg"
    );
    assert_close("M2 report vs module", m2_report, m2_module, 0.5);

    let lspec = linear::LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0);
    let ldesign = linear::design_checked(&lspec).expect("linear design");
    let m2_linear = min_phase_consistency_deg(&ldesign.coeffs, 48_000.0, MIN_PHASE_FFT);
    assert!(
        m2_linear > MIN_PHASE_LIMIT_DEG,
        "线性相位设计的 M2 一致性应远超 2 deg（实测 {m2_linear}），否则 M2 无判别力"
    );
}

// ---------------------------------------------------------------------------
// 2) 七族报告：独立 oracle 的幅相/GD/振铃/支撑/端点（与 leaf 模块交叉对照）
// ---------------------------------------------------------------------------

#[test]
fn t55_report_linear_family_end_to_end() {
    let spec = linear::LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0);
    let design = linear::design_checked(&spec).expect("linear design");
    let declared = declared_linear(&spec, &design);
    let report = build_report(&ReportRequest::new(&declared, &design.coeffs)).expect("report");

    assert!(report.passed, "linear 报告应通过：{:?}", report.failures());
    report.require_pass().unwrap();
    assert_eq!(report.family, Family::Linear);
    assert_eq!(report.grid.points, 8192);
    assert_close("grid lo", report.grid.lo_hz, 20.0, 1e-12);
    assert_close("grid hi", report.grid.hi_hz, 20_000.0, 1e-9);
    assert!(report.grid.meets_spec);
    assert!(report.response_passband.is_some());
    assert!(report.response_stopband.is_some());
    assert!(report.response_transition.as_ref().unwrap().passed);
    assert_eq!(report.ringing.exclusion_samples, 16);
    match report.endpoints.f20k {
        Endpoint::Measured {
            freq_hz,
            mag_db,
            phase_deg,
        } => {
            assert_close("f20k freq", freq_hz, 20_000.0, 1e-9);
            // LP 的 20 kHz 是阻带边（f_s=20k），必须满足阻带声明 -A_s+0.5。
            assert!(
                mag_db <= -(80.0 - 0.5),
                "20 kHz 阻带幅度 {mag_db} dB 应 <= -79.5"
            );
            // 线性相位 + 实对称 A(ω)：展开相位 = -360*f*D/fs 或差 180° 分支（该频点 A(ω) 符号）。
            let expected = -360.0 * 20_000.0 * 255.0 / 48_000.0;
            let w = wrap_deg(phase_deg - expected);
            assert!(
                w.abs() <= 1.0 || (w.abs() - 180.0).abs() <= 1.0,
                "20 kHz 展开相位 {phase_deg} 与 -360 f D/fs={expected} 的差应 ~0 或 ~180（wrap 实得 {w}）"
            );
        }
        other => panic!("20 kHz 端点应 measured，实得 {other:?}"),
    }
    match report.endpoints.f20 {
        Endpoint::Measured {
            freq_hz,
            mag_db,
            phase_deg,
        } => {
            assert_close("f20 freq", freq_hz, 20.0, 1e-9);
            assert!(mag_db.abs() <= 0.01);
            assert_close(
                "f20 展开相位",
                phase_deg,
                -360.0 * 20.0 * 255.0 / 48_000.0,
                1e-6,
            );
        }
        other => panic!("20 Hz 端点应 measured，实得 {other:?}"),
    }
    assert!(criterion(&report, "linear_symmetry").passed);
    assert!(criterion(&report, "linear_group_delay_constant").passed);
    assert!(report.reference.passed);
    assert_eq!(report.group_delay.class, PhaseClass::Constant);
    assert_close(
        "linear D",
        report.group_delay.declared_delay_ms,
        255.0 / 48_000.0 * 1000.0,
        1e-9,
    );
    assert!(report.budget.fits_runtime_profile);
    assert_eq!(report.budget.taps_profile, TapsProfile::Regular);
    assert_eq!(report.budget.ram_bytes_f32_per_route, 511 * 4);
    assert_eq!(report.budget.latency_tiers.len(), 3);
    assert!(report.budget.latency_tiers.iter().all(|t| t.fits));
    assert_eq!(report.support.first_effective, 0);
    assert!(report
        .summary_lines()
        .iter()
        .any(|l| l.contains("criterion[family] linear_symmetry")));

    // 交叉对照：独立 oracle vs leaf 模块的标量测量（两个实现，容差取工程精度）。
    for f in [100.0_f64, 1_000.0, 10_000.0, 17_000.0] {
        let mine = oracle_at(&design.coeffs, 48_000.0, f);
        let theirs = linear::magnitude_db_at(&design.coeffs, 48_000.0, f);
        assert_close(&format!("linear mag @{f}"), mine.mag_db, theirs, 1e-6);
        let gd_theirs = linear::group_delay_at(&design.coeffs, 48_000.0, f);
        assert_close(
            &format!("linear GD @{f}"),
            mine.group_delay_samples,
            gd_theirs,
            1e-4,
        );
    }
}

#[test]
fn t55_report_linear_highpass_lead_ruling_a() {
    let spec = linear::LinearSpec::highpass(48_000.0, 513, 2_000.0, 2_500.0);
    // 用 design() 构造（该 513-tap 702 设计在 T10 的 HP 严格判据下差 0.4 dB，本测试关注报告器
    // 的独立判定）；报告声明 A_s=78 dB（低于设计 80 dB，合法：实测优于声明）。
    let design = linear::design(&spec).expect("linear HP design");
    let mut declared = declared_linear(&spec, &design);
    let declared_as = 78.0_f64;
    if let FamilyParams::Linear { attenuation_db, .. } = &mut declared.params {
        *attenuation_db = declared_as;
    }
    let report = build_report(&ReportRequest::new(&declared, &design.coeffs)).expect("report");
    assert!(report.passed, "HP 报告应通过：{:?}", report.failures());
    // Lead 裁决 A：HP 阻带 [0, f_s] 含 DC，整段纹波峰值 < -A_s（不留 0.5 dB 余量）。
    let sb = report.response_stopband.as_ref().expect("HP stopband");
    assert_close("HP stopband lo（含 DC）", sb.band.lo_hz, 0.0, 1e-12);
    assert!(
        sb.band.max_db < -declared_as,
        "HP 阻带峰值 {:.3} 必须 < -{declared_as} dB",
        sb.band.max_db
    );
    assert!(criterion(&report, "stopband_attenuation").passed);
    // HP 镜像读法：最后一次离开 -A_s 的穿越点 >= f_s - tol。
    let t = report.response_transition.as_ref().expect("HP transition");
    let tol = transition_tolerance(2_500.0 - 2_000.0);
    assert_eq!(t.mode, BandMode::Highpass);
    assert!(
        t.edge_hz_at_attenuation >= 2_000.0 - tol,
        "HP 穿越点 {:.3} < f_s - tol",
        t.edge_hz_at_attenuation
    );
    assert!(t.passed);
    // Nyquist 值报告（LP 期待 0 / HP 视长度）：513 为奇数 Type I，HP 的 H(0.5fs) ≈ 通带；
    // 报告必须给出该事实字段（§3.1 报告额外字段）。
    let h_nyq = amplitude_to_db(nyquist_value(&design.coeffs).abs());
    assert!(
        h_nyq > -1.0,
        "奇数长 HP 的 H(0.5fs) 应接近通带（实测 {h_nyq} dB）"
    );
    assert!(report.statements.iter().any(|s| s.contains("H(0.5fs)")));
}

#[test]
fn t55_report_min_fast_m2_and_frequency_varying_gd() {
    // §9 V17 参考构造（L=2048、f_p=16 kHz、f_s=19 kHz），模块自身判据全过。
    let p = min_fast::MinFastParams::v17_reference();
    let design = min_fast::design(&p).expect("min_fast design");
    assert!(
        design.accepted(),
        "min_fast 自身判据未过：{:?}",
        design.failed_criteria()
    );
    let declared = declared_min_fast(&design);
    let report =
        build_report(&ReportRequest::new(&declared, &design.coefficients)).expect("report");
    assert!(
        report.passed,
        "min_fast 报告应通过：{:?}",
        report.failures()
    );
    assert_eq!(report.group_delay.class, PhaseClass::Minimum);
    assert!(criterion(&report, "min_phase_hilbert").passed);
    assert!(criterion(&report, "min_fast_boundary_ratio").passed);
    assert!(criterion(&report, "min_fast_boundary_nyquist").passed);
    // 频变 GD：min/max 明显不同且非零；禁止零延迟声称。
    assert!(
        report.group_delay.max_samples - report.group_delay.min_samples > 0.5,
        "min_fast GD 应频变：min={} max={}",
        report.group_delay.min_samples,
        report.group_delay.max_samples
    );
    assert!(report.group_delay.max_samples > 1.0);
    assert!(report.group_delay.statement.contains("NOT zero-delay"));
    assert!(report.warnings.iter().any(|w| w.contains("零延迟")));
    // 独立 oracle vs leaf 模块标量测量。
    for f in [100.0_f64, 1_000.0, 10_000.0, 18_000.0] {
        let mine = oracle_at(&design.coefficients, 48_000.0, f);
        assert_close(
            &format!("min_fast mag @{f}"),
            mine.mag_db,
            min_fast::magnitude_db(&design.coefficients, 48_000.0, f),
            1e-6,
        );
        assert_close(
            &format!("min_fast GD @{f}"),
            mine.group_delay_samples,
            min_fast::group_delay_samples_exact(&design.coefficients, 48_000.0, f),
            1e-4,
        );
    }
    // 端点仍必须齐全（20 Hz / 20 kHz 可测）。
    assert!(matches!(report.endpoints.f20, Endpoint::Measured { .. }));
    assert!(matches!(report.endpoints.f20k, Endpoint::Measured { .. }));
}

#[test]
fn t55_report_min_slow_family_distinction() {
    let mut p = min_slow::MinSlowParams::default();
    p.taps = 1_024;
    p.prototype.taps = 8_192;
    p.cepstrum_fft = 8_192;
    let design = min_slow::design(&p).expect("min_slow design");
    let declared = declared_min_slow(&design);
    let report =
        build_report(&ReportRequest::new(&declared, &design.coefficients)).expect("report");
    assert!(
        report.passed,
        "min_slow 报告应通过：{:?}",
        report.failures()
    );
    // §3.3 判据 6：M(0.45fs) > -60 dB（否则应改 min_fast）。
    let probe = oracle_at(&design.coefficients, 48_000.0, 0.45 * 48_000.0).mag_db;
    assert!(probe > -60.0, "min_slow M(0.45fs)={probe} 应 > -60 dB");
    assert!(criterion(&report, "min_slow_distinction").passed);
    assert!(criterion(&report, "min_phase_hilbert").passed);
    let nb = report.response_nyquist_band.as_ref().expect("nyquist band");
    assert!(nb.max_db <= -(p.nyquist_attenuation_db - 1.0));
    assert!(criterion(&report, "transition_monotonic").passed);
    assert!(criterion(&report, "min_slow_fp_boundary").passed);
}

#[test]
fn t55_report_apodizing_pre_ring_and_statement() {
    let spec = apodizing::ApodizingSpec::reference_preset_48k();
    let design = apodizing::design_checked(&spec).expect("apodizing design");
    let declared = declared_apodizing(&spec, &design);
    let report = build_report(&ReportRequest::new(&declared, &design.coeffs)).expect("report");
    assert!(
        report.passed,
        "apodizing 报告应通过：{:?}",
        report.failures()
    );
    let pre = report.ringing.pre_ring_db.expect("pre ring");
    let post = report.ringing.post_ring_db.expect("post ring");
    assert!(pre <= -45.0, "apodizing pre_ring={pre} 应 <= -45 dB");
    assert!(report.ringing.pre_ring_energy_db.unwrap() <= -45.0);
    // 对称设计：pre/post 幅度对称。
    assert_close("pre/post ring 对称", pre, post, 1e-6);
    assert!(criterion(&report, "pre_ring").passed);
    assert!(report
        .statements
        .iter()
        .any(|s| s.contains("不复刻任何专有 DAC")));
    assert!(report
        .statements
        .iter()
        .any(|s| s.contains("仅自研可算窗/目标")));

    // 家族边界：f_p > 0.85*Nyquist -> E_FIR_APO_FP（判定为 FAIL 判据，不静默）。
    let mut bad = declared.clone();
    if let FamilyParams::Apodizing { passband_hz, .. } = &mut bad.params {
        passband_hz[1] = 21_000.0; // 0.85*24000 = 20400
    }
    let bad_report =
        build_report(&ReportRequest::new(&bad, &design.coeffs)).expect("bad report still built");
    assert!(!bad_report.passed);
    assert!(criterion(&bad_report, "apodizing_fp_boundary").passed == false);
    let err = bad_report.require_pass().unwrap_err();
    assert!(
        bad_report.failure_codes().contains(&E_FIR_APO_FP),
        "failure codes 应含 E_FIR_APO_FP: {:?}",
        bad_report.failure_codes()
    );
    assert!(!err.message.is_empty());
}

#[test]
fn t55_report_brickwall_feasibility_and_ringing_duration() {
    let spec = brickwall::BrickwallSpec::lowpass(48_000.0, 1_024, 19_000.0, 20_500.0)
        .with_attenuation(80.0)
        .with_method(brickwall::BrickwallMethod::KaiserWindowedSinc);
    let design = brickwall::design_checked(&spec).expect("brickwall design");
    let declared = declared_brickwall(&spec, &design);
    let report = build_report(&ReportRequest::new(&declared, &design.coeffs)).expect("report");
    assert!(
        report.passed,
        "brickwall 报告应通过：{:?}",
        report.failures()
    );
    assert!(report.response_transition.as_ref().unwrap().passed);
    assert!(report.ringing.duration_to_minus80_samples.is_some());
    let k = report.budget.kaiser.as_ref().expect("kaiser feasibility");
    assert!(k.applies_hard);
    assert!(k.feasible);
    assert_eq!(
        k.required_taps,
        kaiser_required_taps(80.0, 48_000.0, 1_500.0)
    );
    assert!(report.statements.iter().any(|s| s.contains("有限砖墙近似")));
    // 与 apodizing 的振铃取舍差异（§9 V16 的区分性，本实现自算，不作 golden）。
    assert!(report.ringing.pre_ring_db.unwrap() > -45.0);

    // 可行性预检失败：A_s=120/Δf=200 需要 ~1876 taps > 1024 -> E_FIR_BUDGET 判据失败。
    let mut bad = declared.clone();
    if let FamilyParams::Brickwall {
        stopband_hz,
        attenuation_db,
        ..
    } = &mut bad.params
    {
        stopband_hz[0] = 19_200.0;
        *attenuation_db = 120.0;
    }
    let bad_report = build_report(&ReportRequest::new(&bad, &design.coeffs)).expect("bad report");
    let crit = criterion(&bad_report, "brickwall_feasibility");
    assert!(!crit.passed, "brickwall feasibility 应失败：{:?}", crit);
    assert_eq!(crit.code, E_FIR_BUDGET);
    assert!(bad_report.failure_codes().contains(&E_FIR_BUDGET));
}

#[test]
fn t55_report_nos_identity_and_endpoints() {
    let design = nos::design_checked(&nos::NosSpec::reference_preset_48k()).expect("nos design");
    let declared = declared_nos(&design);
    let report = build_report(&ReportRequest::new(&declared, &design.coeffs)).expect("report");
    assert!(report.passed, "nos 报告应通过：{:?}", report.failures());
    assert_eq!(report.support.len, 1);
    assert_close("nos h0", report.support.sum_h, 1.0, 1e-12);
    assert_close("nos D", report.group_delay.declared_delay_ms, 0.0, 1e-12);
    assert_eq!(report.group_delay.class, PhaseClass::None);
    assert!(report.statements.iter().any(|s| s.contains("旁路")));
    assert!(report.statements.iter().any(|s| s.contains("保护")));
    assert!(criterion(&report, "nos_identity").passed);
    assert!(criterion(&report, "nos_flat_response").passed);
    let f20 = match report.endpoints.f20 {
        Endpoint::Measured {
            mag_db, phase_deg, ..
        } => (mag_db, phase_deg),
        _ => panic!("nos 20 Hz 端点应 measured"),
    };
    assert_close("nos f20 mag", f20.0, 0.0, 1e-12);
    assert_close("nos f20 phase", f20.1, 0.0, 1e-12);

    // 低采样率：20 kHz 不可测 -> not_applicable_at_fs（不省略、不裸字符串）。
    let design32 = nos::design_checked(&nos::NosSpec::delta(32_000.0)).expect("nos 32k");
    let declared32 = declared_nos(&design32);
    let report32 =
        build_report(&ReportRequest::new(&declared32, &design32.coeffs)).expect("report 32k");
    match &report32.endpoints.f20k {
        Endpoint::NotApplicableAtFs { freq_hz, reason } => {
            assert_close("f20k freq", *freq_hz, 20_000.0, 1e-12);
            assert!(!reason.is_empty());
            assert!(reason.contains("20000"));
        }
        other => panic!("32k 的 20 kHz 端点应为 not_applicable_at_fs，实得 {other:?}"),
    }
    assert!(matches!(report32.endpoints.f20, Endpoint::Measured { .. }));
    assert!(criterion(&report32, "endpoint_20khz_present").passed);

    // 96 kHz：20 kHz 可测。
    let design96 = nos::design_checked(&nos::NosSpec::delta(96_000.0)).expect("nos 96k");
    let declared96 = declared_nos(&design96);
    let report96 =
        build_report(&ReportRequest::new(&declared96, &design96.coeffs)).expect("report 96k");
    assert!(matches!(report96.endpoints.f20k, Endpoint::Measured { .. }));
}

#[test]
fn t55_report_corrected_complex_target_equivalence_and_delay() {
    let fs = 48_000.0_f64;
    let taps = 64usize;
    let d = 8usize;
    let mut h = vec![0.0_f64; taps];
    h[d] = 1.0;
    let points: Vec<TargetPointSample> = (0..1024)
        .map(|i| {
            let f = 20.0 * (20_000.0f64 / 20.0).powf(i as f64 / 1023.0);
            TargetPointSample {
                freq_hz: f,
                mag_db: 0.0,
                phase_deg: -360.0 * f * d as f64 / fs,
            }
        })
        .collect();
    let target = ComplexTargetCurve::new(points, PhaseTarget::Absolute);
    let declared = DeclaredDesign::new(
        Family::Corrected,
        fs,
        taps,
        FamilyParams::corrected([20.0, 20_000.0], PhaseTarget::Absolute, false),
    )
    .with_delay(d as f64)
    .with_reference(0.0, 0.0);
    let request = ReportRequest::new(&declared, &h).with_target(&target);
    let report = build_report(&request).expect("corrected report");
    assert!(
        report.passed,
        "corrected 报告应通过：{:?}",
        report.failures()
    );
    assert_eq!(report.group_delay.class, PhaseClass::FrequencyVarying);
    assert!(criterion(&report, "corrected_phase_target").passed);
    assert!(criterion(&report, "corrected_complex_error").passed);
    assert_close(
        "corrected GD(1k)",
        report.group_delay.at_1khz_samples,
        d as f64,
        1e-6,
    );

    // corrected 声称与 linear 等价 -> E_FIR_EQUIV_CLAIM 判据失败。
    let mut bad_claim = declared.clone();
    if let FamilyParams::Corrected {
        equivalence_claim, ..
    } = &mut bad_claim.params
    {
        *equivalence_claim = EquivalenceClaim::LinearPhase;
    }
    let bad_report = build_report(&ReportRequest::new(&bad_claim, &h).with_target(&target))
        .expect("claim report");
    assert!(!bad_report.passed);
    assert!(criterion(&bad_report, "corrected_equivalence_claim").passed == false);
    assert!(bad_report.failure_codes().contains(&E_FIR_EQUIV_CLAIM));

    // 缺显式相位目标 -> 直接拒绝 E_TGT_PHASE_UNSUPPORTED（不静默记 NOT_RUN）。
    let err = build_report(&ReportRequest::new(&declared, &h)).unwrap_err();
    assert_eq!(err.code, E_TGT_PHASE_UNSUPPORTED);

    // delay_removed：|D_used - D_target| > 0.01 -> E_FIR_DELAY 判据失败。
    let target_ex: Vec<TargetPointSample> = (0..1024)
        .map(|i| {
            let f = 20.0 * (20_000.0f64 / 20.0).powf(i as f64 / 1023.0);
            // total = phase_ex - 360 f D_target/fs；取 D_target=9 时 phase_ex=+360 f/fs。
            TargetPointSample {
                freq_hz: f,
                mag_db: 0.0,
                phase_deg: 360.0 * f / fs,
            }
        })
        .collect();
    let target_removed = ComplexTargetCurve::new(
        target_ex,
        PhaseTarget::DelayRemoved {
            target_delay_samples: 9.0,
        },
    );
    let declared_removed = DeclaredDesign::new(
        Family::Corrected,
        fs,
        taps,
        FamilyParams::corrected(
            [20.0, 20_000.0],
            PhaseTarget::DelayRemoved {
                target_delay_samples: 9.0,
            },
            false,
        ),
    )
    .with_delay(8.0);
    let removed_report =
        build_report(&ReportRequest::new(&declared_removed, &h).with_target(&target_removed))
            .expect("removed report");
    let crit = criterion(&removed_report, "corrected_delay_consistency");
    assert!(!crit.passed);
    assert_eq!(crit.code, E_FIR_DELAY);

    // magnitude_only -> 陈述必须写 magnitude_only / not equivalent（§3.6 判据 6）。
    let declared_mag = DeclaredDesign::new(
        Family::Corrected,
        fs,
        taps,
        FamilyParams::corrected([20.0, 20_000.0], PhaseTarget::Absolute, true),
    )
    .with_delay(0.0);
    let mag_report = build_report(&ReportRequest::new(&declared_mag, &h).with_target(&target))
        .expect("mag report");
    assert!(mag_report
        .statements
        .iter()
        .any(|s| s.contains("magnitude_only")));
    assert!(mag_report
        .statements
        .iter()
        .any(|s| s.contains("not equivalent to measured complex matrix")));

    // 非因果目标（total GD < 0）-> E_FIR_NONCAUSAL。
    let noncausal_points: Vec<TargetPointSample> = (0..1024)
        .map(|i| {
            let f = 20.0 * (20_000.0f64 / 20.0).powf(i as f64 / 1023.0);
            TargetPointSample {
                freq_hz: f,
                mag_db: 0.0,
                phase_deg: 360.0 * f * 20.0 / fs,
            }
        })
        .collect();
    let noncausal = ComplexTargetCurve::new(
        noncausal_points,
        PhaseTarget::DelayRemoved {
            target_delay_samples: 0.0,
        },
    );
    let declared_nc = DeclaredDesign::new(
        Family::Corrected,
        fs,
        taps,
        FamilyParams::corrected(
            [20.0, 20_000.0],
            PhaseTarget::DelayRemoved {
                target_delay_samples: 0.0,
            },
            false,
        ),
    )
    .with_delay(0.0);
    let nc_report = build_report(&ReportRequest::new(&declared_nc, &h).with_target(&noncausal))
        .expect("nc report");
    assert!(criterion(&nc_report, "corrected_causality").passed == false);
    assert!(nc_report.failure_codes().contains(&E_FIR_NONCAUSAL));
}

// ---------------------------------------------------------------------------
// 3) 拒绝路径（unsupported / 非法规格）
// ---------------------------------------------------------------------------

#[test]
fn t55_unsupported_family_and_spec_rejections() {
    for valid in [
        "linear",
        "min_fast",
        "min_slow",
        "apodizing",
        "brickwall",
        "corrected",
        "lowdelay",
        "nos",
    ] {
        assert_eq!(parse_family(valid).unwrap().as_str(), valid);
    }
    // 相邻工具不是七族预设；大小写/空串/未知一律 E_FIR_SCHEMA 拒绝。
    for bad in [
        "",
        "crossover",
        "phase",
        "LINEAR",
        "minfast",
        "fir",
        "brick-wall",
        " corrected",
    ] {
        let err = parse_family(bad).unwrap_err();
        assert_eq!(
            err.code, E_FIR_SCHEMA,
            "family {bad:?} 应被 E_FIR_SCHEMA 拒绝"
        );
    }
    // family 与族参数不一致 -> E_FIR_SCHEMA。
    let h = vec![0.5, 0.5];
    let mismatched = DeclaredDesign::new(
        Family::MinFast,
        48_000.0,
        2,
        FamilyParams::linear_lowpass(18_000.0, 20_000.0),
    );
    assert_eq!(
        build_report(&ReportRequest::new(&mismatched, &h))
            .unwrap_err()
            .code,
        E_FIR_SCHEMA
    );
    // taps 与 len(h) 不符 -> E_FIR_TAPS。
    let d = DeclaredDesign::new(
        Family::Linear,
        48_000.0,
        3,
        FamilyParams::linear_lowpass(18_000.0, 20_000.0),
    );
    assert_eq!(
        build_report(&ReportRequest::new(&d, &h)).unwrap_err().code,
        E_FIR_TAPS
    );
    // NaN -> E_FIR_NUMERIC；全零 -> E_FIR_NUMERIC。
    let d2 = DeclaredDesign::new(
        Family::Linear,
        48_000.0,
        2,
        FamilyParams::linear_lowpass(18_000.0, 20_000.0),
    )
    .with_delay(0.5);
    assert_eq!(
        build_report(&ReportRequest::new(&d2, &[f64::NAN, 0.5]))
            .unwrap_err()
            .code,
        E_FIR_NUMERIC
    );
    assert_eq!(
        build_report(&ReportRequest::new(&d2, &[0.0, 0.0]))
            .unwrap_err()
            .code,
        E_FIR_NUMERIC
    );
    // fs 非法 -> E_FIR_SCHEMA；taps 超 16384 -> E_FIR_TAPS。
    let bad_fs = DeclaredDesign::new(
        Family::Linear,
        1_000.0,
        2,
        FamilyParams::linear_lowpass(18_000.0, 20_000.0),
    );
    assert_eq!(
        build_report(&ReportRequest::new(&bad_fs, &h))
            .unwrap_err()
            .code,
        E_FIR_SCHEMA
    );
    let huge = vec![1.0_f64; 20_000];
    let d_huge = DeclaredDesign::new(
        Family::Linear,
        48_000.0,
        20_000,
        FamilyParams::linear_lowpass(18_000.0, 20_000.0),
    );
    assert_eq!(
        build_report(&ReportRequest::new(&d_huge, &huge))
            .unwrap_err()
            .code,
        E_FIR_TAPS
    );
    // nos 非恒等 -> E_FIR_NOS。
    let nos_bad = DeclaredDesign::new(Family::Nos, 48_000.0, 1, FamilyParams::nos());
    assert_eq!(
        build_report(&ReportRequest::new(&nos_bad, &[0.5]))
            .unwrap_err()
            .code,
        E_FIR_NOS
    );
    let nos_taps2 = DeclaredDesign::new(Family::Nos, 48_000.0, 2, FamilyParams::nos());
    assert_eq!(
        build_report(&ReportRequest::new(&nos_taps2, &[1.0, 0.0]))
            .unwrap_err()
            .code,
        E_FIR_NOS
    );
    // 相邻工具的 family 字符串不会静默当成预设。
    assert!(parse_family("crossover").is_err());
}

#[test]
fn t55_family_mutual_exclusion_checks() {
    // (a) 砖墙设计冒充 apodizing：0.45fs 处 <= -60 dB -> E_FIR_FAMILY（§3.8）。
    let bspec = brickwall::BrickwallSpec::lowpass(48_000.0, 1_024, 19_000.0, 20_500.0)
        .with_attenuation(80.0)
        .with_method(brickwall::BrickwallMethod::KaiserWindowedSinc);
    let bdesign = brickwall::design_checked(&bspec).expect("brickwall design");
    let apod_declared = DeclaredDesign::new(
        Family::Apodizing,
        48_000.0,
        bdesign.taps(),
        FamilyParams::Apodizing {
            passband_hz: [20.0, 19_000.0],
            ripple_db: 0.5,
            nyquist_attenuation_db: 30.0,
            pre_ring_bound_db: -45.0,
        },
    )
    .with_delay(bdesign.delay_samples);
    let report =
        build_report(&ReportRequest::new(&apod_declared, &bdesign.coeffs)).expect("report");
    assert!(!report.passed);
    let crit = criterion(&report, "family_apodizing_vs_brickwall");
    assert!(!crit.passed);
    assert_eq!(crit.code, E_FIR_FAMILY);
    assert!(report.failure_codes().contains(&E_FIR_FAMILY));

    // (b) min_slow 设计冒充 min_fast：f_s=22 kHz > 0.45fs=21.6 kHz -> E_FIR_FAMILY（§3.2 判据 4）。
    let mut msp = min_slow::MinSlowParams::default();
    msp.taps = 512;
    msp.prototype.taps = 8_192;
    msp.cepstrum_fft = 8_192;
    let msdesign = min_slow::design(&msp).expect("min_slow design");
    let fast_declared = DeclaredDesign::new(
        Family::MinFast,
        msp.fs,
        msp.taps,
        FamilyParams::MinFast {
            passband_hz: msp.passband_hz,
            stopband_hz: [22_000.0, 23_952.0],
            ripple_db: 0.5,
            attenuation_db: 80.0,
        },
    );
    let fast_report =
        build_report(&ReportRequest::new(&fast_declared, &msdesign.coefficients)).expect("report");
    assert!(!fast_report.passed);
    let crit = criterion(&fast_report, "min_fast_boundary_nyquist");
    assert!(!crit.passed);
    assert_eq!(crit.code, E_FIR_FAMILY);
    assert!(fast_report.failure_codes().contains(&E_FIR_FAMILY));
}

#[test]
fn t55_report_hp_nyquist_even_length_rejected() {
    // 512（偶数）对称 HP：H(0.5fs)=0；若声明 passband 上界含 0.5fs -> E_FIR_HP_NYQ（§3.1 判据 9）。
    let spec = linear::LinearSpec::highpass(48_000.0, 512, 2_000.0, 2_500.0);
    let design = linear::design(&spec).expect("linear HP even design");
    let mut declared = declared_linear(&spec, &design);
    if let FamilyParams::Linear { passband_hz, .. } = &mut declared.params {
        passband_hz[1] = 24_000.0; // 0.5 fs
    }
    let report = build_report(&ReportRequest::new(&declared, &design.coeffs)).expect("report");
    let crit = criterion(&report, "hip_nyquist_type");
    assert!(
        !crit.passed,
        "偶数长 HP 声明 passband 到 0.5fs 应 E_FIR_HP_NYQ"
    );
    assert_eq!(crit.code, E_FIR_HP_NYQ);
    assert!(report.failure_codes().contains(&E_FIR_HP_NYQ));
    assert!(nyquist_value(&design.coeffs).abs() <= 1e-12);
}

#[test]
fn t55_report_keeps_full_evidence_on_failed_criteria() {
    // 非对称 h（最小相位 min_fast 设计）冒充 linear：对称性/常数 GD 判据失败，但报告完整返回、
    // 保留全部实测证据（WBS §6：失败保原始证据，不静默清零）。
    let mut p = min_fast::MinFastParams::default();
    p.taps = 512;
    p.passband_hz = [20.0, 16_000.0];
    p.stopband_hz = [19_000.0, 23_952.0];
    p.prototype.taps = 8_192;
    p.cepstrum_fft = 8_192;
    let design = min_fast::design(&p).expect("min_fast design");
    let declared = DeclaredDesign::new(
        Family::Linear,
        p.fs,
        p.taps,
        FamilyParams::Linear {
            mode: BandMode::Lowpass,
            passband_hz: [20.0, 16_000.0],
            stopband_hz: [19_000.0, 23_952.0],
            ripple_db: 0.5,
            attenuation_db: 80.0,
            symmetry: SymmetryMode::Symmetric,
        },
    )
    .with_delay((p.taps as f64 - 1.0) / 2.0)
    .with_reference(0.0, 0.0);
    let report =
        build_report(&ReportRequest::new(&declared, &design.coefficients)).expect("report");
    assert!(!report.passed, "非对称 h 冒充 linear 不应通过");
    let sym = criterion(&report, "linear_symmetry");
    assert!(!sym.passed);
    assert_eq!(sym.code, E_FIR_PHASE_CLASS);
    assert!(report.failure_codes().contains(&E_FIR_PHASE_CLASS));
    // 证据仍完整：幅相/GD/振铃/端点/预算/网格都在。
    assert!(report.response_passband.is_some());
    assert!(report.response_stopband.is_some());
    assert!(!report.group_delay.third_octave.is_empty());
    assert!(matches!(report.endpoints.f20, Endpoint::Measured { .. }));
    assert_eq!(report.verification.grid_points, report.grid.points);
    assert!(report
        .summary_lines()
        .iter()
        .any(|l| l.contains("FAIL") && l.contains("linear_symmetry")));
    let err = report.require_pass().unwrap_err();
    assert!(!err.message.is_empty());
}

// ---------------------------------------------------------------------------
// 4) 预算（§2.3/§3.5/§7.2）与端点/网格/一致性
// ---------------------------------------------------------------------------

#[test]
fn t55_budget_kaiser_delay_ram_cost() {
    // §9 V13 Kaiser 可行性表。
    let table = [
        (60.0_f64, 48_000.0_f64, 1_000.0_f64, 176u64),
        (80.0, 48_000.0, 1_000.0, 242),
        (100.0, 48_000.0, 1_000.0, 309),
        (120.0, 48_000.0, 1_000.0, 376),
        (80.0, 48_000.0, 500.0, 483),
        (80.0, 96_000.0, 1_000.0, 483),
        (60.0, 96_000.0, 2_000.0, 176),
    ];
    for (a_s, fs, df, expected) in table {
        assert_eq!(
            kaiser_required_taps(a_s, fs, df),
            Some(expected),
            "V13 {a_s}/{fs}/{df}"
        );
    }
    assert_eq!(kaiser_required_taps(30.0, 48_000.0, 1_000.0), None);

    // §9 V12 延迟表（线性相位 D=(L-1)/2，@48k）。
    let v12 = [
        (512usize, 5.322_916_666_666_667),
        (1_024, 10.656_25),
        (2_048, 21.322_916_666_666_668),
        (4_096, 42.656_25),
    ];
    for (taps, ms) in v12 {
        let d = (taps as f64 - 1.0) / 2.0;
        assert_close("V12 delay_ms", d / 48_000.0 * 1000.0, ms, 1e-12);
    }
    // 15/25/40 三档：4096@48k=42.65625 > 40 -> 全不满足（§7.2）；1024 全满足。
    let opts = ReportOptions::default();
    let t4096 = latency_tier(42.656_25, 40.0, &opts);
    assert!(!t4096.fits);
    let t1024 = latency_tier(10.656_25, 15.0, &opts);
    assert!(t1024.fits);
    let t2048_25 = latency_tier(21.322_916_666_666_668, 25.0, &opts);
    assert!(t2048_25.fits);
    let t2048_15 = latency_tier(21.322_916_666_666_668, 15.0, &opts);
    assert!(!t2048_15.fits);

    // §7.2 内存：4*taps bytes/路由；25x4096x4 = 409600 B = 400 KiB。
    assert_eq!(25 * 4_096 * F32_BYTES_PER_TAP, 409_600);
    // taps profile / 未列出的中间值不得当 extended。
    assert_eq!(TapsProfile::for_taps(4_096), TapsProfile::Regular);
    assert_eq!(TapsProfile::for_taps(4_097), TapsProfile::NotAdmitted);
    assert_eq!(TapsProfile::for_taps(8_192), TapsProfile::Extended);
    assert_eq!(TapsProfile::for_taps(16_384), TapsProfile::Extended);
    assert_eq!(TapsProfile::for_taps(20_000), TapsProfile::OfflineOnly);

    // §2.3 成本：25 条 5119 tap direct = 12.2856 GFLOP/s（§7.1 同一模型）。
    let inputs = CostInputs {
        fs: 48_000.0,
        paths: vec![5_119usize; 25],
        block: 128,
        transforms_in: 1,
        transforms_out: 1,
    };
    let cost = estimate_costs(&inputs);
    assert_close("§2.3 gflops_direct", cost.gflops_direct, 12.2856, 1e-9);
    assert_eq!(cost.taps_max, 5_119);
    assert_eq!(cost.paths, 25);
    // FFT 模型按 §2.3 公式独立复算比对。
    let block = 128.0_f64;
    let k = 2.0 * block;
    let f = block + 1.0;
    let q = (5_119.0_f64 / block).ceil();
    let r = 48_000.0 / block;
    let expected_fft = (8.0 * 25.0 * q * f * r + 2.0 * 2.5 * k * k.log2() * r) / 1e9;
    assert_close("§2.3 gflops_fft", cost.gflops_fft, expected_fft, 1e-12);

    // §9 V14 融合长度 L_F = L_G + L_P - 1（用 build_budget 直接验证，避开全网格测量）。
    let dummy_delay = GroupDelayReport {
        class: PhaseClass::Constant,
        band_lo_hz: 20.0,
        band_hi_hz: 20_000.0,
        min_samples: 0.0,
        max_samples: 0.0,
        mean_samples: 0.0,
        at_1khz_samples: 0.0,
        onset_samples: 0.0,
        onset_ms: 0.0,
        latency_ms_1khz: 0.0,
        gd_max_ms: 0.0,
        declared_delay_ms: 0.0,
        third_octave: Vec::new(),
        characteristic_hz_samples: Vec::new(),
        statement: String::new(),
    };
    let d8 = DeclaredDesign::new(
        Family::Linear,
        48_000.0,
        8,
        FamilyParams::linear_lowpass(18_000.0, 20_000.0),
    );
    let h8 = vec![0.125_f64; 8];
    for (g, p, expected_len) in [
        (4_096usize, 1_024usize, 5_119usize),
        (8_192, 4_096, 12_287),
        (2_048, 2_048, 4_095),
    ] {
        let opts = ReportOptions {
            fusion_pair: Some((g, p)),
            ..ReportOptions::default()
        };
        let budget = build_budget(&d8, &h8, &opts, &dummy_delay);
        let fusion = budget.fusion.expect("fusion");
        assert_eq!(fusion.fusion_length, expected_len, "V14 fusion length");
        assert_eq!(fusion.taps_g, g);
        assert_eq!(fusion.taps_p, p);
    }
}

#[test]
fn t55_report_consistency_with_declared_response() {
    let spec = linear::LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0);
    let design = linear::design_checked(&spec).expect("linear design");
    let declared = declared_linear(&spec, &design);
    let base = build_report(&ReportRequest::new(&declared, &design.coeffs)).expect("report");
    let pb = base.response_passband.as_ref().unwrap();
    let sb = base.response_stopband.as_ref().unwrap();
    let ep20 = match base.endpoints.f20 {
        Endpoint::Measured {
            mag_db, phase_deg, ..
        } => (mag_db, phase_deg),
        _ => panic!(),
    };
    let ep20k = match base.endpoints.f20k {
        Endpoint::Measured {
            mag_db, phase_deg, ..
        } => (mag_db, phase_deg),
        _ => panic!(),
    };
    let expected = DeclaredResponse {
        passband_ripple_db: Some(pb.ripple_db),
        passband_min_db: Some(pb.min_db),
        passband_max_db: Some(pb.max_db),
        stopband_attenuation_db: Some(sb.attenuation_db),
        stopband_max_db: Some(sb.band.max_db),
        transition_edge_hz: Some(
            base.response_transition
                .as_ref()
                .unwrap()
                .edge_hz_at_attenuation,
        ),
        group_delay_min_samples: Some(base.group_delay.min_samples),
        group_delay_max_samples: Some(base.group_delay.max_samples),
        group_delay_mean_samples: Some(base.group_delay.mean_samples),
        group_delay_at_1khz_samples: Some(base.group_delay.at_1khz_samples),
        ringing_pre_db: base.ringing.pre_ring_db,
        ringing_post_db: base.ringing.post_ring_db,
        ringing_pre_energy_db: base.ringing.pre_ring_energy_db,
        ringing_post_energy_db: base.ringing.post_ring_energy_db,
        endpoint_f20_mag_db: Some(ep20.0),
        endpoint_f20_phase_deg: Some(ep20.1),
        endpoint_f20k_mag_db: Some(ep20k.0),
        endpoint_f20k_phase_deg: Some(ep20k.1),
    };
    let ok = build_report(&ReportRequest::new(&declared, &design.coeffs).with_expected(&expected))
        .expect("report");
    assert!(ok.verification.consistency.as_ref().unwrap().passed);
    assert!(criterion(&ok, "requested_response_consistency").passed);

    // 篡改一个 dB 值（+0.2 dB > 0.05 容差）-> E_FIR_SCHEMA 判据失败、consistency.passed=false。
    let mut corrupted = expected.clone();
    corrupted.passband_ripple_db = Some(pb.ripple_db + 0.2);
    let bad =
        build_report(&ReportRequest::new(&declared, &design.coeffs).with_expected(&corrupted))
            .expect("report");
    assert!(!bad.passed);
    let crit = criterion(&bad, "requested_response_consistency");
    assert!(!crit.passed);
    assert_eq!(crit.code, E_FIR_SCHEMA);
    assert!(!bad.verification.consistency.as_ref().unwrap().passed);
}

#[test]
fn t55_grid_endpoint_and_manifest_face_fields() {
    let spec = linear::LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0);
    let design = linear::design_checked(&spec).expect("linear design");
    let declared = declared_linear(&spec, &design);
    let report = build_report(&ReportRequest::new(&declared, &design.coeffs)).expect("report");
    // 网格：定义与点数必须落盘（§1.2）。
    assert_eq!(
        report.grid.points,
        MIN_GRID_POINTS.max(GRID_POINTS_PER_TAP * 511)
    );
    let ratio = (report.grid.hi_hz / report.grid.lo_hz).ln() / std::f64::consts::LN_2;
    let expected_step = ratio / (report.grid.points as f64 - 1.0);
    assert_close(
        "网格 min step oct",
        report.grid.min_step_oct,
        expected_step,
        1e-12,
    );
    assert_close(
        "网格 max step oct",
        report.grid.max_step_oct,
        expected_step,
        1e-12,
    );
    // §2.2 response 子集字段齐全。
    assert!(report.response_passband.is_some());
    assert!(report.response_stopband.is_some());
    assert!(report.response_transition.is_some());
    assert_eq!(report.group_delay.class, PhaseClass::Constant);
    assert!(!report.group_delay.third_octave.is_empty());
    assert!(matches!(report.endpoints.f20, Endpoint::Measured { .. }));
    assert!(matches!(report.endpoints.f20k, Endpoint::Measured { .. }));
    assert_eq!(report.endpoints.f20.status(), "measured");
    assert_eq!(report.endpoints.f20k.status(), "measured");
    assert!(report.reference.passed);
    assert_eq!(report.verification.grid_points, report.grid.points);
    assert!(report.verification.method.contains("direct DTFT"));
    assert!(!report.statements.is_empty());
    // 预算字段：limits 面。
    assert_eq!(report.budget.route_taps_cap, Some(REGULAR_PROFILE_TAPS_CAP));
    assert_eq!(report.budget.ram_bytes_f32_per_route, 4 * 511);
    assert_eq!(report.budget.cost.paths, 1);
    assert!(report.budget.kaiser.as_ref().unwrap().feasible);
}

#[test]
fn t55_determinism_and_no_hidden_gain() {
    let spec = linear::LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0);
    let design = linear::design_checked(&spec).expect("linear design");
    let declared = declared_linear(&spec, &design);
    let a = build_report(&ReportRequest::new(&declared, &design.coeffs)).expect("report a");
    let b = build_report(&ReportRequest::new(&declared, &design.coeffs)).expect("report b");
    assert_eq!(a, b, "同一输入两次报告必须完全一致（§1.6 确定性）");
    assert_eq!(a.summary_lines(), b.summary_lines());
    // 参考归一化 ±0.01 dB（§1.5）：不允许偷偷加整体增益。
    assert!(a.reference.passed);
    assert_close(
        "DC 参考",
        measured_reference_db(&design.coeffs, 48_000.0, 0.0),
        0.0,
        0.01,
    );
    assert_close("sum_h", a.support.sum_h, 1.0, 0.01);
}

// ---------------------------------------------------------------------------
// 5) 证据用：打印样例报告（cargo test -- --nocapture > evidence/T55/report-sample.txt）
// ---------------------------------------------------------------------------

#[test]
fn t55_emit_sample_report_summary() {
    // 七族各出一条真实报告摘要，作为 evidence/T55/report-sample.txt 的可复算来源。
    let lspec = linear::LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0);
    let ldesign = linear::design_checked(&lspec).expect("linear design");
    let ldeclared = declared_linear(&lspec, &ldesign);
    let lreport =
        build_report(&ReportRequest::new(&ldeclared, &ldesign.coeffs)).expect("linear report");
    println!("### linear (511 taps, LP 18k/20k)");
    for line in lreport.summary_lines() {
        println!("{line}");
    }

    let aspec = apodizing::ApodizingSpec::reference_preset_48k();
    let adesign = apodizing::design_checked(&aspec).expect("apodizing design");
    let adeclared = declared_apodizing(&aspec, &adesign);
    let areport =
        build_report(&ReportRequest::new(&adeclared, &adesign.coeffs)).expect("apodizing report");
    println!();
    println!("### apodizing (1024 taps, f_p=18k)");
    for line in areport.summary_lines() {
        println!("{line}");
    }

    let ndesign = nos::design_checked(&nos::NosSpec::delta(32_000.0)).expect("nos 32k");
    let ndeclared = declared_nos(&ndesign);
    let nreport =
        build_report(&ReportRequest::new(&ndeclared, &ndesign.coeffs)).expect("nos report");
    println!();
    println!("### nos @32k (20 kHz not_applicable_at_fs)");
    for line in nreport.summary_lines() {
        println!("{line}");
    }
}
