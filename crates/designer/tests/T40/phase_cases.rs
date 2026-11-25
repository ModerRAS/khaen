// T40 私有测试：phase / delay / polarity FIR 工具。
//
// 本文件由 crates/designer/src/phase.rs 的 #[cfg(test)] 子模块通过 include! 编译执行
// （cargo 不自动发现 tests/<Txx>/ 子目录；public_registration.rs 属 integrator，不改）。
// 运行：cargo test -p khaen-designer phase
//
// 覆盖：§4.2 整数/分数 delay、taps 支撑、极性、输入/输出方向、因果、独立解析向量（V11 等）、
// 测量网格/端点/振铃/参考归一化、预算事实、失败码、确定性。use super::* 由外层提供。

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
fn expect_code<T: std::fmt::Debug>(result: Result<T, PhaseError>, code: &str) -> PhaseError {
    match result {
        Ok(v) => panic!("期望错误 {code}，实际成功 {v:?}"),
        Err(e) => {
            assert_eq!(e.code(), code, "错误码不符：{e}");
            e
        }
    }
}

fn integer(fs: f64, delay: u64) -> PhaseDesign {
    design_integer(&IntegerDelaySpec::new(fs, delay)).unwrap()
}

fn v11(fs: f64) -> PhaseDesign {
    design_fractional(&FractionalDelaySpec::v11_reference(fs)).unwrap()
}

fn impl_approx(a: &[f64], b: &[f64], tol: f64, what: &str) {
    assert_eq!(a.len(), b.len(), "{what}: 长度不一致");
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        assert_approx(*x, *y, tol, &format!("{what}[{i}]"));
    }
}

// ---------------------------------------------------------------------------
// 常量 / 失败码 / 方向与极性说明
// ---------------------------------------------------------------------------

#[test]
fn t40_constants_and_error_table() {
    assert_eq!(TOOL_KIND, "phase-delay-polarity");
    assert_eq!(TOOL_VERSION, "0.1.0");
    assert_eq!(MIN_WINDOW_TAPS, 63);
    assert_eq!(V11_WINDOW_TAPS, 63);
    assert_approx(DEFAULT_KAISER_BETA, 8.96, 1e-12, "beta");
    assert_approx(V11_FRACTIONAL_DELAY, 0.5, 1e-12, "d");
    assert_approx(V11_TOTAL_DELAY_SAMPLES, 31.5, 1e-12, "D");
    assert_eq!(V11_MAX_MAG_DEVIATION_DB, 0.009_77);
    assert_eq!(V11_DC_DEVIATION_DB, -0.009_54);
    assert_eq!(V11_GD_MINUS_31_TOLERANCE, 0.0002);
    assert_eq!(FRACTIONAL_MAG_TOL_DB, 0.1);
    assert_eq!(FRACTIONAL_GD_TOL_SAMPLES, 0.02);
    assert_eq!(MEASUREMENT_LO_HZ, 20.0);
    assert_eq!(ENDPOINT_HI_HZ, 20_000.0);
    assert_eq!(REFERENCE_TOLERANCE_DB, 0.01);
    assert_eq!(MAX_TAPS, 16_384);
    assert_eq!(REGULAR_PROFILE_TAPS_CAP, 4_096);
    assert_eq!(EXTENDED_PROFILE_TAPS_CAP, 16_384);
    assert_eq!(DELAY_BUDGET_MS, [15.0, 25.0, 40.0]);
    assert_eq!(POLARITY_INVERT_SIGN, -1.0);
    assert_eq!(MIN_MEASUREMENT_GRID_POINTS, 8_192);
    assert_eq!(MEASUREMENT_GRID_FACTOR, 8);
    assert_eq!(MAX_LAGRANGE_ORDER, 15);
    // §2.5 失败码表覆盖本模块使用的码。
    for code in [
        E_FIR_TAPS,
        E_FIR_DELAY,
        E_FIR_NUMERIC,
        E_FIR_BAND,
        E_FIR_BUDGET,
        E_FIR_NONCAUSAL,
        E_FIR_REF,
        E_PHASE_ARG,
    ] {
        assert!(
            PHASE_ERROR_CODE_TABLE.iter().any(|(c, _)| *c == code),
            "错误码表缺 {code}"
        );
    }
    // 登记说明与方向/因果/XOR 语句。
    assert!(FAMILY_NOTE.contains("不是七家族"));
    assert!(INPUT_SIDE_STATEMENT.contains("列"));
    assert!(OUTPUT_SIDE_STATEMENT.contains("行"));
    assert!(COMMUTATIVITY_STATEMENT.contains("不可交换"));
    assert!(CALIBRATION_XOR_STATEMENT.contains("XOR"));
    assert!(CALIBRATION_XOR_STATEMENT.contains("禁止双乘"));
    assert!(CAUSALITY_STATEMENT.contains("E_FIR_NONCAUSAL"));
    assert!(CANONICAL_CHAIN_STATEMENT.contains("P_post"));
    // ApplySide 默认与字符串。
    assert_eq!(ApplySide::default_side(), ApplySide::OutputRows);
    assert_eq!(ApplySide::OutputRows.as_str(), "output_rows");
    assert_eq!(ApplySide::InputColumns.as_str(), "input_columns");
    assert_eq!(ApplySide::OutputRows.dimension(), "行");
    assert_eq!(ApplySide::InputColumns.dimension(), "列");
    // Polarity 精确符号与反推。
    assert_eq!(Polarity::Normal.sign(), 1.0);
    assert_eq!(Polarity::Invert.sign(), -1.0);
    assert_eq!(Polarity::from_sign(1.0), Some(Polarity::Normal));
    assert_eq!(Polarity::from_sign(-1.0), Some(Polarity::Invert));
    assert_eq!(Polarity::from_sign(0.5), None);
}

// ---------------------------------------------------------------------------
// 整数延迟：解析向量、支撑、taps 填充
// ---------------------------------------------------------------------------

#[test]
fn t40_integer_delay_analytic_vector() {
    let fs = 48_000.0;
    let d = integer(fs, 7);
    assert_eq!(d.taps, 8, "最小支撑 L=D+1");
    assert_eq!(d.delay_samples, 7.0);
    assert_approx(d.delay_ms, 7.0 / fs * 1000.0, 1e-12, "delay_ms");
    assert_approx(d.onset_ms(), 7.0 / fs * 1000.0, 1e-12, "onset_ms");
    assert_eq!(d.effective_support(), (7, 7, 1));
    assert_eq!(d.trailing_zeros(), 0);
    let mut expected = vec![0.0; 8];
    expected[7] = 1.0;
    assert_eq!(d.coefficients, expected);
    // 解析：|H|=1、phase=-360 f D/fs、GD=D（常数）。
    for f in [0.0, 20.0, 1_000.0, 19_000.0] {
        let r = d.response_at(f);
        assert_approx(r.magnitude_db, 0.0, 1e-12, &format!("mag@{f}"));
        assert_approx(r.group_delay_samples, 7.0, 1e-9, &format!("GD@{f}"));
        let expect_phase = wrap_deg_180(-360.0 * f * 7.0 / fs);
        assert_approx(r.phase_deg, expect_phase, 1e-9, &format!("phase@{f}"));
    }
    assert_approx(d.response_at(1_000.0).phase_deg, -52.5, 1e-9, "1k phase");
    let refs = d.reference_stats();
    assert_approx(refs.measured_gain_db, 0.0, 1e-12, "DC reference");
    assert!(refs.ok);
    let v = d.verify();
    assert!(v.passed(), "{:?}", v.summary_lines());
    assert!(v.fully_spec_compliant());
    assert!(v.grid_meets_spec);
    assert_eq!(v.grid_points, 8_192);
}

#[test]
fn t40_integer_zero_identity() {
    let d = integer(48_000.0, 0);
    assert_eq!(d.taps, 1);
    assert_eq!(d.coefficients, vec![1.0]);
    assert_eq!(d.delay_samples, 0.0);
    assert_eq!(d.delay_ms, 0.0);
    assert_eq!(d.onset_samples(), 0);
    let r = d.response_at(12_345.0);
    assert_approx(r.magnitude_db, 0.0, 1e-12, "identity mag");
    assert_approx(r.phase_deg, 0.0, 1e-12, "identity phase");
    assert_approx(r.group_delay_samples, 0.0, 1e-12, "identity GD");
    let ring = d.ringing();
    assert!(ring.pre_ring_db.is_none() && ring.post_ring_db.is_none());
    assert!(ring.note.contains("n/a"));
    let v = d.verify();
    assert!(v.passed());
    assert!(v.fully_spec_compliant());
}

#[test]
fn t40_integer_padded_taps_support() {
    let fs = 48_000.0;
    let d = design_integer(&IntegerDelaySpec::new(fs, 7).with_taps(64)).unwrap();
    assert_eq!(d.taps, 64);
    assert_eq!(d.coefficients.len(), 64);
    assert_eq!(d.coefficients[7], 1.0);
    assert!(d
        .coefficients
        .iter()
        .enumerate()
        .all(|(i, h)| i == 7 || *h == 0.0));
    assert_eq!(d.effective_support(), (7, 7, 1));
    assert_eq!(d.trailing_zeros(), 64 - (7 + 1));
    assert_eq!(d.delay_samples, 7.0);
    assert_approx(d.group_delay_at(19_000.0), 7.0, 1e-9, "GD@19k");
    assert_eq!(d.budget_facts().taps, 64);
    assert_eq!(d.budget_facts().profile, TapsProfile::Regular);
}

#[test]
fn t40_integer_delay_rejections() {
    // taps < D+1。
    let e = expect_code(
        design_integer(&IntegerDelaySpec::new(48_000.0, 7).with_taps(7)),
        E_FIR_TAPS,
    );
    assert!(e.detail().contains("D+1"), "detail: {}", e.detail());
    // D=16384 => 最小支撑 16385 超 16384。
    expect_code(
        design_integer(&IntegerDelaySpec::new(48_000.0, 16_384)),
        E_FIR_BUDGET,
    );
    // 显式 taps 超上限。
    expect_code(
        design_integer(&IntegerDelaySpec::new(48_000.0, 1).with_taps(20_000)),
        E_FIR_BUDGET,
    );
    // 声明 delay 残差 >0.01。
    expect_code(
        design_integer(&IntegerDelaySpec::new(48_000.0, 7).with_declared_delay_samples(7.5)),
        E_FIR_DELAY,
    );
    // 声明 delay_ms 与 D/fs*1000 不一致。
    expect_code(
        design_integer(&IntegerDelaySpec::new(48_000.0, 7).with_declared_delay_ms(0.15)),
        E_FIR_DELAY,
    );
    // target delay 残差 >0.01（delay_removed 不猜）。
    expect_code(
        design_integer(&IntegerDelaySpec::new(48_000.0, 7).with_target_delay_samples(7.5)),
        E_FIR_DELAY,
    );
    // fs 越界 / NaN。
    expect_code(design_integer(&IntegerDelaySpec::new(100.0, 7)), E_FIR_BAND);
    expect_code(
        design_integer(&IntegerDelaySpec::new(f64::NAN, 7)),
        E_FIR_NUMERIC,
    );
    // 正确的显式声明应通过。
    let ok = design_integer(
        &IntegerDelaySpec::new(48_000.0, 7)
            .with_declared_delay_samples(7.0)
            .with_declared_delay_ms(7.0 / 48_000.0 * 1000.0)
            .with_target_delay_samples(7.0),
    )
    .unwrap();
    assert!(ok.delay_audit().samples_ok);
    assert!(ok.delay_audit().delay_ms_ok);
    assert!(ok.delay_audit().target_ok);
}

// ---------------------------------------------------------------------------
// 分数延迟：V11 解析向量（63-tap Kaiser beta 8.96、d=0.5、D=31.5）
// ---------------------------------------------------------------------------

#[test]
fn t40_fractional_v11_kaiser_vector() {
    let fs = 48_000.0;
    let d = v11(fs);
    assert_eq!(d.taps, 63);
    assert_eq!(d.support_start, 0);
    assert_eq!(d.window_taps(), Some(63));
    assert_approx(d.delay_samples, 31.5, 0.0, "D");
    assert_approx(d.delay_ms, 0.656_25, 0.0, "delay_ms");
    assert_approx(d.fractional_delay_d().unwrap(), 0.5, 1e-12, "fractional d");
    assert_eq!(d.method(), Some(FractionalMethod::kaiser_sinc()));
    match d.kind {
        DelayKind::Fractional {
            integer_delay_samples,
            ..
        } => assert_eq!(integer_delay_samples, 31),
        _ => panic!("应为 fractional kind"),
    }
    // V11 记录的 DC 偏差（§9 V11：DC -0.00954 dB，容差 1e-5）。
    let refs = d.reference_stats();
    assert_approx(refs.measured_gain_db, V11_DC_DEVIATION_DB, 1e-5, "V11 DC");
    assert!(refs.ok, "DC 偏差应在 ±0.01 dB 内");
    // h[k]=sinc(k-c-frac)·w[k]（c=31）；抽头解析值。
    let h = &d.coefficients;
    assert_approx(h[31], 0.636_619_772_367_581_4, 1e-9, "h[31]");
    assert_approx(h[0], -9.595_323_040_022_806e-6, 1e-12, "h[0]");
    assert_approx(h[62], 9.909_923_795_433_388e-6, 1e-12, "h[62]");
    let w = FractionalWindow::kaiser_v11().values(63);
    for k in [0_usize, 1, 15, 31, 47, 61, 62] {
        let expected = sinc_normalized(k as f64 - 31.0 - 0.5) * w[k];
        assert_approx(h[k], expected, 1e-15, &format!("h[{k}] 解析"));
    }
    // V11 GD：20 Hz–10 kHz 总 GD=31.5±0.001，即 GD−31=0.500±0.001（§9 V11 已修正）。
    let v10k = d.verify_with(&VerifyOptions::band(fs, [20.0, 10_000.0]));
    assert!(v10k.passed(), "{:?}", v10k.summary_lines());
    assert!(
        v10k.band.gd_max_abs_dev_samples <= 0.001,
        "20-10k GD 偏差 {}",
        v10k.band.gd_max_abs_dev_samples
    );
    assert!(
        v10k.band.gd_min_samples >= 31.5 - 0.001 && v10k.band.gd_max_samples <= 31.5 + 0.001,
        "GD 范围 [{},{}]",
        v10k.band.gd_min_samples,
        v10k.band.gd_max_samples
    );
    assert_approx(d.group_delay_at(1_000.0), 31.5, 0.001, "GD@1k");
    assert_approx(
        d.response_at(1_000.0).phase_deg,
        123.750_171_904,
        1e-3,
        "V11 phase@1k (wrap)",
    );
    // V11 |H|：20 Hz–20 kHz 最大偏差 0.00977 dB（<=0.01）。
    let v20k = d.verify_with(&VerifyOptions::band(fs, [20.0, 20_000.0]));
    assert!(v20k.passed(), "{:?}", v20k.summary_lines());
    assert!(
        v20k.band.mag_max_abs_dev_db <= V11_MAX_MAG_DEVIATION_DB + 1e-4,
        "20-20k |H| 偏差 {}",
        v20k.band.mag_max_abs_dev_db
    );
    // §4.2 全带判据（[20, 0.9·fs/2]）与 §2.5 预算。
    let v = d.verify();
    assert!(v.passed(), "{:?}", v.summary_lines());
    assert!(v.fully_spec_compliant(), "V11 应满足 §4.2 全带");
    assert!(v.band.mag_max_abs_dev_db <= FRACTIONAL_MAG_TOL_DB);
    assert!(v.band.gd_max_abs_dev_samples <= FRACTIONAL_GD_TOL_SAMPLES);
    let budget = d.budget_facts();
    assert_eq!(budget.profile, TapsProfile::Regular);
    assert_eq!(budget.ram_bytes_f32, 4 * 63);
    assert_eq!(budget.fits_delay_budget_ms, [true, true, true]);
}

#[test]
fn t40_fractional_support_shift_and_taps() {
    let fs = 48_000.0;
    // D=100.5, Lw=63 => c=31, shift=69, support L=132。
    let d = design_fractional(&FractionalDelaySpec::windowed_sinc(fs, 100.5)).unwrap();
    assert_eq!(d.taps, 132);
    assert_eq!(d.support_start, 69);
    assert_eq!(d.coefficients.len(), 132);
    assert_approx(d.delay_samples, 100.5, 0.0, "D");
    assert_approx(d.delay_ms, 100.5 / fs * 1000.0, 0.0, "delay_ms 2.09375");
    assert_eq!(d.onset_samples(), 69);
    let v = d.verify();
    assert!(v.passed(), "{:?}", v.summary_lines());
    // 显式 taps 必须等于计算支撑。
    expect_code(
        design_fractional(&FractionalDelaySpec::windowed_sinc(fs, 100.5).with_taps(131)),
        E_FIR_TAPS,
    );
    // 大整数部分：D=2000.5 => shift=1969，支撑 L=2032。
    let big = design_fractional(&FractionalDelaySpec::windowed_sinc(fs, 2000.5)).unwrap();
    assert_eq!(big.support_start, 1969);
    assert_eq!(big.taps, 2032);
    assert_eq!(big.budget_facts().profile, TapsProfile::Regular);
    assert!(big.trailing_zeros() <= 1);
}

#[test]
fn t40_fractional_hann_and_blackman_harris() {
    let fs = 48_000.0;
    // Hann 63-tap 在 0.9·fs/2 的 GD 偏差约 0.045 samples > 0.02：全带判据 FAIL（不静默放行）。
    let hann63 = FractionalDelaySpec::windowed_sinc(fs, 31.25)
        .with_window_taps(63)
        .with_method(FractionalMethod::hann_sinc());
    let d63 = design_fractional(&hann63).unwrap();
    let v63 = d63.verify();
    assert!(!v63.passed(), "Hann 63 应不满足全带判据");
    assert!(!v63.checks.group_delay_band_ok);
    assert!(v63.band.gd_max_abs_dev_samples > FRACTIONAL_GD_TOL_SAMPLES);
    let e = expect_code(design_checked(&PhaseSpec::Fractional(hann63)), E_FIR_BUDGET);
    assert!(e.detail().contains("INFEASIBLE"), "detail: {}", e.detail());
    // Hann 127-tap 通过全带判据。
    let hann127 = FractionalDelaySpec::windowed_sinc(fs, 63.25)
        .with_window_taps(127)
        .with_method(FractionalMethod::hann_sinc());
    let d127 = design_checked(&PhaseSpec::Fractional(hann127)).unwrap();
    let v127 = d127.verify();
    assert!(v127.passed(), "{:?}", v127.summary_lines());
    assert!(v127.fully_spec_compliant());
    assert!(v127.band.gd_max_abs_dev_samples <= FRACTIONAL_GD_TOL_SAMPLES);
    // Blackman-Harris 127-tap 同样通过。
    let bh = FractionalDelaySpec::windowed_sinc(fs, 63.25)
        .with_window_taps(127)
        .with_method(FractionalMethod::blackman_harris_sinc());
    let dbh = design_checked(&PhaseSpec::Fractional(bh)).unwrap();
    let vbh = dbh.verify();
    assert!(vbh.passed());
    assert!(vbh.band.mag_max_abs_dev_db <= 0.005);
    assert_eq!(dbh.window_taps(), Some(127));
}

#[test]
fn t40_fractional_lagrange_coeffs() {
    let fs = 48_000.0;
    // order1, frac=0.25 => D=0.75, h=[0.25,0.75]（解析）。
    let s1 = FractionalDelaySpec::lagrange(fs, 0.75, 1);
    let d1 = design_fractional(&s1).unwrap();
    assert_eq!(d1.taps, 2);
    impl_approx(&d1.coefficients, &[0.25, 0.75], 1e-15, "order1");
    assert_approx(d1.delay_samples, 0.75, 0.0, "D1");
    assert_approx(d1.fractional_delay_d().unwrap(), 0.25, 1e-12, "d1");
    // order3, frac=0.5 => D=2（整数 delta at n=2）。
    let s3 = FractionalDelaySpec::lagrange(fs, 2.0, 3);
    let d3 = design_fractional(&s3).unwrap();
    assert_eq!(d3.taps, 4);
    impl_approx(
        &d3.coefficients,
        &[0.0, 0.0, 1.0, 0.0],
        1e-15,
        "order3 frac0.5",
    );
    // order3, frac=0.25 => D=1.75，解析 [-5,35,105,-7]/128。
    let s4 = FractionalDelaySpec::lagrange(fs, 1.75, 3);
    let d4 = design_fractional(&s4).unwrap();
    let expected = [-5.0 / 128.0, 35.0 / 128.0, 105.0 / 128.0, -7.0 / 128.0];
    impl_approx(&d4.coefficients, &expected, 1e-15, "order3 frac0.25");
    // 支撑固定 taps=order+1。
    expect_code(
        design_fractional(&FractionalDelaySpec::lagrange(fs, 1.75, 3).with_taps(5)),
        E_FIR_TAPS,
    );
    // Lagrange 不使用窗长。
    expect_code(
        design_fractional(&FractionalDelaySpec::lagrange(fs, 1.75, 3).with_window_taps(63)),
        E_PHASE_ARG,
    );
}

#[test]
fn t40_fractional_lagrange_band_declaration() {
    let fs = 48_000.0;
    let full = FractionalDelaySpec::lagrange(fs, 1.75, 3);
    let dfull = design_fractional(&full).unwrap();
    let vfull = dfull.verify();
    assert!(!vfull.passed(), "Lagrange order3 全带应 INFEASIBLE");
    assert!(!vfull.checks.magnitude_band_ok);
    expect_code(design_checked(&PhaseSpec::Fractional(full)), E_FIR_BUDGET);
    // 显式低带 [20,4800] 声明后通过，但 spec_band_compliant=false + warning。
    let narrow = FractionalDelaySpec::lagrange(fs, 1.75, 3).with_verify_band([20.0, 4_800.0]);
    let dn = design_checked(&PhaseSpec::Fractional(narrow)).unwrap();
    let vn = dn.verify();
    assert!(vn.passed(), "{:?}", vn.summary_lines());
    assert!(
        !vn.fully_spec_compliant(),
        "Lagrange 窄带不是 §4.2 全带合规"
    );
    assert!(!vn.checks.spec_band_compliant);
    assert!(!vn.checks.spec_band_required);
    assert!(vn.warnings.iter().any(|w| w.contains("Lagrange")));
    assert!(vn.band.mag_max_abs_dev_db <= FRACTIONAL_MAG_TOL_DB);
    assert!(vn.band.gd_max_abs_dev_samples <= FRACTIONAL_GD_TOL_SAMPLES);
    // 窄带声明也必须合法（lo>=20、hi<=0.9fs/2）。
    expect_code(
        design_fractional(
            &FractionalDelaySpec::lagrange(fs, 1.75, 3).with_verify_band([10.0, 4_800.0]),
        ),
        E_FIR_BAND,
    );
}

#[test]
fn t40_fractional_rejections() {
    let fs = 48_000.0;
    // 窗长 <63 / 偶数。
    expect_code(
        design_fractional(&FractionalDelaySpec::windowed_sinc(fs, 31.5).with_window_taps(61)),
        E_FIR_TAPS,
    );
    expect_code(
        design_fractional(&FractionalDelaySpec::windowed_sinc(fs, 31.5).with_window_taps(64)),
        E_FIR_TAPS,
    );
    // D < 窗中心 31 => 不可因果。
    let e = expect_code(
        design_fractional(&FractionalDelaySpec::windowed_sinc(fs, 10.5)),
        E_FIR_NONCAUSAL,
    );
    assert!(e.detail().contains("窗中心"), "detail: {}", e.detail());
    // NaN / 负 D。
    expect_code(
        design_fractional(&FractionalDelaySpec::windowed_sinc(fs, f64::NAN)),
        E_FIR_NUMERIC,
    );
    expect_code(
        design_fractional(&FractionalDelaySpec::windowed_sinc(fs, -1.0)),
        E_FIR_NONCAUSAL,
    );
    // 支撑超 16384：D=16383.5 => shift=16352, L=16415。
    expect_code(
        design_fractional(&FractionalDelaySpec::windowed_sinc(fs, 16_383.5)),
        E_FIR_BUDGET,
    );
    // 窗函数 sinc 不得声明窄带放宽 §4.2 判据。
    expect_code(
        design_fractional(
            &FractionalDelaySpec::windowed_sinc(fs, 31.5).with_verify_band([20.0, 10_000.0]),
        ),
        E_FIR_BAND,
    );
    // Kaiser beta 非法。
    expect_code(
        design_fractional(
            &FractionalDelaySpec::windowed_sinc(fs, 31.5)
                .with_method(FractionalMethod::windowed(FractionalWindow::kaiser(0.0))),
        ),
        E_FIR_NUMERIC,
    );
    // Lagrange order 越界 / D 越出 [order/2, order/2+1)。
    expect_code(
        design_fractional(&FractionalDelaySpec::lagrange(fs, 1.75, 0)),
        E_FIR_TAPS,
    );
    expect_code(
        design_fractional(&FractionalDelaySpec::lagrange(fs, 1.75, 16)),
        E_FIR_TAPS,
    );
    expect_code(
        design_fractional(&FractionalDelaySpec::lagrange(fs, 2.5, 3)),
        E_FIR_DELAY,
    );
    expect_code(
        design_fractional(&FractionalDelaySpec::lagrange(fs, 1.4, 3)),
        E_FIR_DELAY,
    );
    // 声明 delay / delay_ms / target 残差。
    expect_code(
        design_fractional(
            &FractionalDelaySpec::windowed_sinc(fs, 31.5).with_declared_delay_samples(31.52),
        ),
        E_FIR_DELAY,
    );
    expect_code(
        design_fractional(
            &FractionalDelaySpec::windowed_sinc(fs, 31.5).with_declared_delay_ms(0.7),
        ),
        E_FIR_DELAY,
    );
    expect_code(
        design_fractional(
            &FractionalDelaySpec::windowed_sinc(fs, 31.5).with_target_delay_samples(31.6),
        ),
        E_FIR_DELAY,
    );
    // 显式 taps 与支撑不一致。
    expect_code(
        design_fractional(&FractionalDelaySpec::windowed_sinc(fs, 31.5).with_taps(64)),
        E_FIR_TAPS,
    );
}

// ---------------------------------------------------------------------------
// 极性
// ---------------------------------------------------------------------------

#[test]
fn t40_polarity_exact_invert() {
    for (name, normal) in [
        ("integer", integer(48_000.0, 7)),
        ("fractional", v11(48_000.0)),
    ] {
        let inverted = match &normal.kind {
            DelayKind::Integer { delay_samples } => {
                let mut spec =
                    IntegerDelaySpec::new(48_000.0, *delay_samples).with_polarity(Polarity::Invert);
                if normal.taps > (*delay_samples as usize) + 1 {
                    spec = spec.with_taps(normal.taps);
                }
                design_integer(&spec).unwrap()
            }
            DelayKind::Fractional { .. } => design_fractional(
                &FractionalDelaySpec::v11_reference(48_000.0).with_polarity(Polarity::Invert),
            )
            .unwrap(),
        };
        assert_eq!(inverted.polarity, Polarity::Invert);
        assert_eq!(inverted.taps, normal.taps);
        // 系数精确乘 −1。
        for (a, b) in inverted.coefficients.iter().zip(&normal.coefficients) {
            assert_eq!(*a, -*b, "{name}: invert 必须精确乘 -1");
        }
        // 幅度/延迟/参考不变；相位差 180 度。
        for f in [0.0, 20.0, 1_000.0, 19_000.0] {
            let rn = normal.response_at(f);
            let ri = inverted.response_at(f);
            assert_approx(
                ri.magnitude_db,
                rn.magnitude_db,
                1e-12,
                &format!("{name} mag@{f}"),
            );
            assert_approx(
                ri.group_delay_samples,
                rn.group_delay_samples,
                1e-9,
                &format!("{name} GD@{f}"),
            );
            if f > 0.0 {
                let shifted = wrap_deg_180(rn.phase_deg + 180.0);
                assert_approx(ri.phase_deg, shifted, 1e-9, &format!("{name} phase@{f}"));
            }
        }
        assert!(inverted.reference_stats().ok);
        assert_eq!(inverted.delay_samples, normal.delay_samples);
        // 反相后 DC 增益符号与 polarity 一致（verify 的 polarity_sign_ok）。
        assert!(inverted.verify().checks.polarity_sign_ok);
        assert!(normal.verify().checks.polarity_sign_ok);
    }
    // 整数 invert 的精确 delta。
    let inv = design_integer(&IntegerDelaySpec::new(48_000.0, 7).with_polarity(Polarity::Invert))
        .unwrap();
    assert_eq!(inv.coefficients[7], -1.0);
    assert_eq!(inv.effective_polarity(Polarity::Normal), Polarity::Invert);
    assert_eq!(inv.effective_polarity(Polarity::Invert), Polarity::Normal);
}

#[test]
fn t40_polarity_xor_with_calibration() {
    let truth = [
        (Polarity::Normal, Polarity::Normal, Polarity::Normal),
        (Polarity::Normal, Polarity::Invert, Polarity::Invert),
        (Polarity::Invert, Polarity::Normal, Polarity::Invert),
        (Polarity::Invert, Polarity::Invert, Polarity::Normal),
    ];
    for (design, calibration, expected) in truth {
        let plan = PolarityPlan::new(design, calibration);
        assert_eq!(plan.effective(), expected);
        assert_approx(
            plan.effective_sign(),
            expected.sign(),
            0.0,
            "effective_sign 必须与 XOR 一致",
        );
        assert!(plan.note().contains("XOR"));
    }
    let both = PolarityPlan::new(Polarity::Invert, Polarity::Invert);
    assert!(both.both_inverted());
    assert_eq!(both.effective(), Polarity::Normal);
    // 设计系数 + 校准 invert 只允许一处应用；effective 极性写报告。
    let d = integer(48_000.0, 7);
    assert_eq!(d.effective_polarity(Polarity::Invert), Polarity::Invert);
    let di = design_integer(&IntegerDelaySpec::new(48_000.0, 7).with_polarity(Polarity::Invert))
        .unwrap();
    assert_eq!(di.effective_polarity(Polarity::Invert), Polarity::Normal);
}

// ---------------------------------------------------------------------------
// 输入/输出方向
// ---------------------------------------------------------------------------

#[test]
fn t40_direction_statements_and_defaults() {
    let d = integer(48_000.0, 7);
    assert_eq!(d.side, ApplySide::OutputRows);
    let text = d.report_text();
    assert!(text.contains("side: output_rows"), "report: {text}");
    assert!(text.contains("输出侧"), "report: {text}");
    assert!(text.contains("不可交换"), "report: {text}");
    assert!(text.contains("XOR"), "report: {text}");
    assert!(text.contains("causality:"), "report: {text}");
    let input_side =
        design_integer(&IntegerDelaySpec::new(48_000.0, 7).with_side(ApplySide::InputColumns))
            .unwrap();
    assert_eq!(input_side.side, ApplySide::InputColumns);
    let text_in = input_side.report_text();
    assert!(text_in.contains("side: input_columns"));
    assert!(text_in.contains("输入侧"));
    assert!(input_side
        .manifest_declaration()
        .apply_scope
        .contains("A/G"));
}

#[test]
fn t40_direction_identity_analytic() {
    let fs = 48_000.0;
    // 2x2 恒等路由矩阵（零路由用空 vec）。
    let matrix = vec![
        vec![1.0],  // (0,0)
        Vec::new(), // (0,1)
        Vec::new(), // (1,0)
        vec![1.0],  // (1,1)
    ];
    let in_plan = DirectionalPlan::input(vec![
        ChannelPhaseOp::delay(0.0),
        ChannelPhaseOp::delay(2.0).inverted(),
    ]);
    let out_plan = DirectionalPlan::output(vec![
        ChannelPhaseOp::delay(1.0).inverted(),
        ChannelPhaseOp::delay(0.0),
    ]);
    let a = apply_directional_phase(fs, 2, 2, &matrix, &in_plan).unwrap();
    assert_eq!(a.side, ApplySide::InputColumns);
    assert_eq!(a.route_firs[0], vec![1.0]); // (0,0)：col0 delay0 normal
    assert_eq!(a.route_firs[1], Vec::<f64>::new()); // 零路由保持空
    assert_eq!(a.route_firs[2], Vec::<f64>::new());
    assert_eq!(a.route_firs[3], vec![0.0, 0.0, -1.0]); // (1,1)：col1 delay2 invert
    assert!(a.statement.contains("列"));
    let b = apply_directional_phase(fs, 2, 2, &matrix, &out_plan).unwrap();
    assert_eq!(b.side, ApplySide::OutputRows);
    assert_eq!(b.route_firs[0], vec![0.0, -1.0]); // row0 delay1 invert
    assert_eq!(b.route_firs[3], vec![1.0]); // row1 delay0 normal
    assert!(b.statement.contains("行"));
    // 非对称作用：同一矩阵、不同方向 => 路由 IR 不同。
    assert_ne!(a.route_firs, b.route_firs);
}

#[test]
fn t40_direction_noncommutative_asymmetric() {
    let fs = 48_000.0;
    // 非对称矩阵：各路由 IR 不同。
    let matrix = vec![
        vec![1.0, 0.5],   // (0,0)
        vec![0.25],       // (0,1)
        vec![0.5, -0.25], // (1,0)
        vec![1.0],        // (1,1)
    ];
    let in_plan = DirectionalPlan::input(vec![
        ChannelPhaseOp::delay(0.0),
        ChannelPhaseOp::delay(1.0).inverted(),
    ]);
    let out_plan = DirectionalPlan::output(vec![
        ChannelPhaseOp::delay(0.0),
        ChannelPhaseOp::delay(2.0).inverted(),
    ]);
    let a = apply_directional_phase(fs, 2, 2, &matrix, &in_plan).unwrap();
    let b = apply_directional_phase(fs, 2, 2, &matrix, &out_plan).unwrap();
    // (0,1)：输入侧 col1 delay1 invert -> conv([0.25],[0,-1])=[0,-0.25]；输出侧 row0 delay0 -> [0.25]。
    impl_approx(&a.route_firs[1], &[0.0, -0.25], 1e-15, "input (0,1)");
    impl_approx(&b.route_firs[1], &[0.25], 1e-15, "output (0,1)");
    // (1,0)：输入侧 col0 delay0 -> [0.5,-0.25]；输出侧 row1 delay2 invert -> [0,0,-0.5,0.25]。
    impl_approx(&a.route_firs[2], &[0.5, -0.25], 1e-15, "input (1,0)");
    impl_approx(
        &b.route_firs[2],
        &[0.0, 0.0, -0.5, 0.25],
        1e-15,
        "output (1,0)",
    );
    assert_ne!(a.route_firs, b.route_firs, "非对称矩阵下方向不可交换");
    // 同侧逐通道 op 不同 => 路由 IR 随行/列索引变化。
    assert_ne!(a.route_firs[0], a.route_firs[1]);
    // 公共 LTI 时移（所有通道相同 op）时可交换（卷积交换律）。
    let same_in =
        DirectionalPlan::input(vec![ChannelPhaseOp::delay(3.0), ChannelPhaseOp::delay(3.0)]);
    let same_out =
        DirectionalPlan::output(vec![ChannelPhaseOp::delay(3.0), ChannelPhaseOp::delay(3.0)]);
    let sa = apply_directional_phase(fs, 2, 2, &matrix, &same_in).unwrap();
    let sb = apply_directional_phase(fs, 2, 2, &matrix, &same_out).unwrap();
    assert_eq!(sa.route_firs, sb.route_firs, "公共 LTI 时移应可交换");
    // 参数误用：ops 数量错 / 矩阵 shape 错 / 负 delay。
    expect_code(
        apply_directional_phase(
            fs,
            2,
            2,
            &matrix,
            &DirectionalPlan::input(vec![ChannelPhaseOp::delay(0.0)]),
        ),
        E_PHASE_ARG,
    );
    expect_code(
        apply_directional_phase(fs, 2, 2, &matrix[..3], &in_plan),
        E_PHASE_ARG,
    );
    expect_code(
        apply_directional_phase(
            fs,
            2,
            2,
            &matrix,
            &DirectionalPlan::input(vec![
                ChannelPhaseOp::delay(0.0),
                ChannelPhaseOp::delay(-1.0),
            ]),
        ),
        E_FIR_NONCAUSAL,
    );
}

// ---------------------------------------------------------------------------
// 因果：bulk D 吸收
// ---------------------------------------------------------------------------

#[test]
fn t40_causal_delay_absorption() {
    let fs = 48_000.0;
    let plan = plan_causal_delay(fs, -3.0, 10.0).unwrap();
    assert_approx(plan.total_delay_samples, 7.0, 1e-12, "total");
    assert_approx(plan.delay_ms, 7.0 / fs * 1000.0, 1e-12, "total ms");
    assert!(plan.is_integer_total());
    assert_eq!(plan.integer_total(), Some(7));
    assert_approx(plan.relative_vs_bulk_samples, -3.0, 1e-12, "relative");
    // 吸收后的总 D 可直接设计整数/分数延迟。
    let d = design_integer(&IntegerDelaySpec::new(fs, plan.integer_total().unwrap())).unwrap();
    assert_eq!(d.delay_samples, plan.total_delay_samples);
    assert_approx(d.delay_ms, plan.delay_ms, 1e-12, "plan/design delay_ms");
    // 分数总延迟：bulk 40 + (-8.5) = 31.5（V11 可用）。
    let frac_plan = plan_causal_delay(fs, -8.5, 40.0).unwrap();
    assert_approx(frac_plan.total_delay_samples, 31.5, 1e-12, "frac total");
    assert!(!frac_plan.is_integer_total());
    assert_eq!(frac_plan.integer_total(), None);
    let dv = design_fractional(&FractionalDelaySpec::windowed_sinc(
        fs,
        frac_plan.total_delay_samples,
    ))
    .unwrap();
    assert_approx(dv.delay_samples, 31.5, 1e-12, "fractional D");
    // 吸收失败：total<0。
    let e = expect_code(plan_causal_delay(fs, -3.0, 2.0), E_FIR_NONCAUSAL);
    assert!(e.detail().contains("吸收"), "detail: {}", e.detail());
    // 负 bulk。
    expect_code(plan_causal_delay(fs, 0.0, -1.0), E_FIR_NONCAUSAL);
    // 非有限。
    expect_code(plan_causal_delay(fs, f64::NAN, 10.0), E_FIR_NUMERIC);
    // 分数总延迟小于 63-tap 窗中心 31 => 仍需更大 bulk。
    let small = plan_causal_delay(fs, -12.5, 40.0).unwrap();
    assert_approx(small.total_delay_samples, 27.5, 1e-12, "27.5");
    expect_code(
        design_fractional(&FractionalDelaySpec::windowed_sinc(
            fs,
            small.total_delay_samples,
        )),
        E_FIR_NONCAUSAL,
    );
}

// ---------------------------------------------------------------------------
// 确定性
// ---------------------------------------------------------------------------

#[test]
fn t40_design_determinism() {
    let fs = 48_000.0;
    let a = integer(fs, 7);
    let b = integer(fs, 7);
    assert_eq!(a, b, "整数延迟两次设计必须逐位一致");
    let va = v11(fs);
    let vb = v11(fs);
    assert_eq!(va, vb, "分数延迟两次设计必须逐位一致");
    let spec = PhaseSpec::Fractional(FractionalDelaySpec::v11_reference(fs));
    let c1 = design(&spec).unwrap();
    let c2 = design(&spec).unwrap();
    assert_eq!(c1, c2);
    // 不同输入产生不同系数（防止空实现）。注意窗函数 sinc 按 fs 归一化，96k 同 D 的系数与
    // 48k 相同；改 fractional d / 窗长才会改变系数。
    let other_d = design_fractional(&FractionalDelaySpec::windowed_sinc(48_000.0, 31.25)).unwrap();
    assert_ne!(va.coefficients, other_d.coefficients);
    assert!(va.coefficients.len() > 0);
    let same_shape_96k = v11(96_000.0);
    assert_eq!(va.coefficients, same_shape_96k.coefficients);
}

// ---------------------------------------------------------------------------
// 预算事实 / 延迟预算（§7.2 / V12）
// ---------------------------------------------------------------------------

#[test]
fn t40_budget_facts_and_delay_budget() {
    let fs = 48_000.0;
    // 整数延迟 D=512/1024/2048 samples => 10.6667/21.3333/42.6667 ms（D 就是 FIR 体延迟；
    // V12 的 10.65625 ms 是 linear family D=(L-1)/2 的口径，不能与整数 delay D=L 混写）。
    let d512 = integer(fs, 512);
    assert_eq!(d512.taps, 513);
    assert_approx(d512.delay_ms, 512.0 / fs * 1000.0, 1e-12, "512 ms");
    assert_eq!(d512.budget_facts().fits_delay_budget_ms, [true, true, true]);
    let d1024 = integer(fs, 1024);
    assert_approx(d1024.delay_ms, 21.333_333_333_333_332, 1e-9, "1024 ms");
    assert_eq!(
        d1024.budget_facts().fits_delay_budget_ms,
        [false, true, true]
    );
    let d2048 = integer(fs, 2048);
    assert_approx(d2048.delay_ms, 42.666_666_666_666_67, 1e-9, "2048 ms");
    assert_eq!(
        d2048.budget_facts().fits_delay_budget_ms,
        [false, false, false]
    );
    // profile 边界。
    assert_eq!(
        integer(fs, 4095).budget_facts().profile,
        TapsProfile::Regular
    );
    let ext = integer(fs, 4096).budget_facts();
    assert_eq!(ext.profile, TapsProfile::Extended);
    assert_eq!(ext.route_taps_cap, EXTENDED_PROFILE_TAPS_CAP);
    assert_eq!(ext.ram_bytes_f32, 4 * 4097);
    let max = integer(fs, 16_383).budget_facts();
    assert_eq!(max.taps, 16_384);
    assert_eq!(max.profile, TapsProfile::Extended);
    assert!(max.fits_regular_profile == false);
    assert!(max.note.contains("整链"));
    // 预算事实不宣称整链通过。
    assert_eq!(d1024.budget_facts().delay_budget_ms, DELAY_BUDGET_MS);
}

// ---------------------------------------------------------------------------
// 振铃 / 端点 / 参考归一化 / 测量网格
// ---------------------------------------------------------------------------

#[test]
fn t40_ringing_and_endpoints() {
    let fs = 48_000.0;
    // V11 振铃：W=16，主峰 p=31，前/后振铃可测且受 Kaiser 窗抑制。
    let d = v11(fs);
    let ring = d.ringing();
    assert_eq!(ring.exclusion_samples, 16);
    assert_eq!(ring.peak_index, 31);
    assert!(ring.pre_ring_db.is_some() && ring.post_ring_db.is_some());
    assert!(
        ring.pre_ring_db.unwrap() < -35.0,
        "pre ring {}",
        ring.pre_ring_db.unwrap()
    );
    assert!(
        ring.post_ring_db.unwrap() < -35.0,
        "post ring {}",
        ring.post_ring_db.unwrap()
    );
    assert!(ring.pre_ring_energy_db.is_some());
    assert!(ring.note.contains("W=ceil"));
    // 整数短支撑：p±W 越出支撑 => n/a。
    let di = integer(fs, 7);
    let ring_i = di.ringing();
    assert!(ring_i.pre_ring_db.is_none() && ring_i.post_ring_db.is_none());
    assert!(ring_i.note.contains("n/a"));
    // 端点：48k 下 20 Hz / 20 kHz 都可测。
    let ep = d.endpoints();
    assert_eq!(ep.f20.status(), "measured");
    assert_eq!(ep.f20k.status(), "measured");
    let (f20, mag20, phase20) = ep.f20.measured().unwrap();
    assert_eq!(f20, 20.0);
    assert_approx(mag20, V11_DC_DEVIATION_DB, 1e-4, "f20 mag");
    // 分数延迟 GD 在 20 Hz 是 31.50011 samples（V11），相位比理想 31.5 略偏 1.7e-5 度。
    assert_approx(
        phase20,
        wrap_deg_180(-360.0 * 20.0 * 31.5 / fs),
        1e-3,
        "f20 phase",
    );
    let (f20k, mag20k, _) = ep.f20k.measured().unwrap();
    assert_eq!(f20k, 20_000.0);
    assert_approx(mag20k, -0.009_768_931, 1e-4, "V11 f20k mag");
    // 32k 下 20 kHz 不可测：必须写 not_applicable_at_fs 联合类型，不能省略。
    let low = integer(32_000.0, 7);
    let ep_low = low.endpoints();
    assert_eq!(ep_low.f20k.status(), "not_applicable_at_fs");
    assert!(ep_low.f20k.measured().is_none());
    // 参考归一化：整数精确 0 dB；V11 DC 偏差在 ±0.01。
    assert_approx(
        integer(fs, 0).reference_stats().measured_gain_db,
        0.0,
        1e-12,
        "identity DC ref",
    );
    assert!(d.reference_stats().ok);
}

#[test]
fn t40_measurement_grid_and_band_guard() {
    let fs = 48_000.0;
    let d = integer(fs, 7);
    let v = d.verify();
    assert_eq!(v.grid_points, 8_192, "自动网格 = max(8192, 8L)");
    assert!(v.grid_meets_spec);
    // 故意给低网格：如实标 grid_meets_spec=false + warning（不冒充规范网格）。
    let low = d.verify_with(&VerifyOptions::spec(fs).with_grid(1_024));
    assert_eq!(low.grid_points, 1_024);
    assert!(!low.grid_meets_spec);
    assert!(low.warnings.iter().any(|w| w.contains("低于")));
    // 频带越出 §4.2 规范：warning + 不能算全带合规。
    let out = d.verify_with(&VerifyOptions::band(fs, [20.0, 22_000.0]));
    assert!(out.warnings.iter().any(|w| w.contains("越出")));
    assert!(!out.checks.spec_band_compliant);
    assert!(!out.fully_spec_compliant());
    // 96k 时 0.9fs/2=43200，V11 判据按相对频率不变。
    let d96 = v11(96_000.0);
    assert_approx(d96.delay_ms, 31.5 / 96_000.0 * 1000.0, 0.0, "96k delay_ms");
    let v96 = d96.verify();
    assert!(v96.passed(), "{:?}", v96.summary_lines());
    assert!(v96.fully_spec_compliant());
    assert!(v96.spec_band_hz[1] > 43_000.0);
    // 16k 下 20 kHz 端点不可测，验证带上界 = 0.9*8k=7.2k。
    let d16 = integer(16_000.0, 3);
    let v16 = d16.verify();
    assert!(v16.passed());
    assert!(v16.spec_band_hz[1] <= 7_200.0 + 1e-9);
    assert_eq!(d16.endpoints().f20k.status(), "not_applicable_at_fs");
}

// ---------------------------------------------------------------------------
// 报告 / manifest / 统一入口
// ---------------------------------------------------------------------------

#[test]
fn t40_report_and_manifest_declaration() {
    let fs = 48_000.0;
    let d = v11(fs);
    let text = d.report_text();
    assert!(text.contains("tool: phase-delay-polarity 0.1.0"));
    assert!(text.contains("kind: fractional_delay"));
    assert!(text.contains("delay_samples: 31.500000"));
    assert!(text.contains("delay_ms: 0.656250"));
    assert!(text.contains("fractional_delay_d: 0.500000"));
    assert!(text.contains("method: windowed_sinc"));
    assert!(text.contains("window: kaiser"));
    assert!(text.contains("beta: 8.96"));
    assert!(text.contains("effective_support:"));
    assert!(text.contains("family_note:"));
    assert!(text.contains("不是七家族"));
    let summary = d.summary_lines();
    assert!(summary.iter().any(|l| l.contains("checks passed=true")));
    assert!(summary
        .iter()
        .any(|l| l.contains("budget: profile=regular")));
    let m = d.manifest_declaration();
    assert_eq!(m.tool, TOOL_KIND);
    assert_eq!(m.version, TOOL_VERSION);
    assert_eq!(m.kind, "fractional_delay");
    assert_eq!(m.taps, 63);
    assert_approx(m.delay_samples, 31.5, 0.0, "manifest D");
    assert_approx(m.delay_ms, 0.656_25, 0.0, "manifest delay_ms");
    assert_approx(m.fractional_delay_d.unwrap(), 0.5, 1e-12, "manifest d");
    assert_eq!(m.method, "windowed_sinc");
    assert_eq!(m.window, "kaiser");
    assert_eq!(m.window_taps, Some(63));
    assert_approx(
        m.window_beta.unwrap(),
        DEFAULT_KAISER_BETA,
        1e-12,
        "manifest beta",
    );
    assert_eq!(m.polarity, Polarity::Normal);
    assert_eq!(m.side, ApplySide::OutputRows);
    assert!(m.apply_scope.contains("P_post"));
    assert!(m.spec_band_required);
    assert!(m.family_note.contains("不是七家族"));
    // 整数 manifest。
    let mi = integer(fs, 7).manifest_declaration();
    assert_eq!(mi.kind, "integer_delay");
    assert_eq!(mi.method, "none");
    assert_eq!(mi.window, "none");
    assert!(mi.fractional_delay_d.is_none());
    // 反相报告写 polarity invert。
    let di = design_integer(&IntegerDelaySpec::new(fs, 7).with_polarity(Polarity::Invert)).unwrap();
    assert!(di.report_text().contains("polarity: invert"));
}

#[test]
fn t40_unified_design_and_checked() {
    let fs = 48_000.0;
    // 整数 design_checked 通过且全带合规。
    let int_spec = PhaseSpec::Integer(IntegerDelaySpec::new(fs, 7));
    let di = design_checked(&int_spec).unwrap();
    assert_eq!(di.taps, 8);
    assert!(di.verify().fully_spec_compliant());
    // 分数 V11 design_checked 通过。
    let frac_spec = PhaseSpec::Fractional(FractionalDelaySpec::v11_reference(fs));
    let df = design_checked(&frac_spec).unwrap();
    assert!(df.verify().fully_spec_compliant());
    // PhaseSpec 访问器。
    assert_eq!(int_spec.fs(), fs);
    assert_eq!(int_spec.side(), ApplySide::OutputRows);
    assert_eq!(int_spec.polarity(), Polarity::Normal);
    assert_approx(int_spec.delay_samples(), 7.0, 0.0, "spec D");
    assert_approx(frac_spec.delay_samples(), 31.5, 0.0, "frac spec D");
    // 统一 design 错误透传。
    expect_code(
        design(&PhaseSpec::Fractional(FractionalDelaySpec::windowed_sinc(
            fs, 1.0,
        ))),
        E_FIR_NONCAUSAL,
    );
}
// ---------------------------------------------------------------------------
// 证据用：打印 T40 可复算向量（cargo test ... -- --nocapture 时进日志）
// ---------------------------------------------------------------------------

#[test]
fn t40_phase_evidence_vectors_print() {
    let fs = 48_000.0;
    let d = v11(fs);
    let v11_full = d.verify();
    let v11_10k = d.verify_with(&VerifyOptions::band(fs, [20.0, 10_000.0]));
    let v11_20k = d.verify_with(&VerifyOptions::band(fs, [20.0, 20_000.0]));
    println!(
        "T40-EVIDENCE V11 kaiser63 d=0.5 D=31.5 taps={} support_start={}",
        d.taps, d.support_start
    );
    println!(
        "T40-EVIDENCE V11 DC_measured_gain_db={:.9}",
        d.reference_stats().measured_gain_db
    );
    println!(
        "T40-EVIDENCE V11 mag_dev_max_20_20k_db={:.9}",
        v11_20k.band.mag_max_abs_dev_db
    );
    println!(
        "T40-EVIDENCE V11 GD_20_10k_min={:.9} max={:.9} dev_max={:.9} gd_1k={:.9}",
        v11_10k.band.gd_min_samples,
        v11_10k.band.gd_max_samples,
        v11_10k.band.gd_max_abs_dev_samples,
        d.group_delay_at(1_000.0)
    );
    println!(
        "T40-EVIDENCE V11 phase_1k_wrap_deg={:.9}",
        d.response_at(1_000.0).phase_deg
    );
    println!(
        "T40-EVIDENCE V11 full_band_passed={} spec_band_compliant={} grid={}",
        v11_full.passed(),
        v11_full.checks.spec_band_compliant,
        v11_full.grid_points
    );
    println!("T40-EVIDENCE V11 ringing W={} peak={} pre_db={:?} post_db={:?} pre_energy_db={:?} post_energy_db={:?}",
        d.ringing().exclusion_samples, d.ringing().peak_index,
        d.ringing().pre_ring_db, d.ringing().post_ring_db,
        d.ringing().pre_ring_energy_db, d.ringing().post_ring_energy_db);
    let ep = d.endpoints();
    println!(
        "T40-EVIDENCE V11 endpoints f20={:?} f20k={:?}",
        ep.f20.measured(),
        ep.f20k.measured()
    );
    let di = integer(fs, 7);
    println!(
        "T40-EVIDENCE INT7 taps={} delay_ms={:.9} GD_1k={:.9} phase_1k={:.9}",
        di.taps,
        di.delay_ms,
        di.group_delay_at(1_000.0),
        di.response_at(1_000.0).phase_deg
    );
    let hann63 = design_fractional(
        &FractionalDelaySpec::windowed_sinc(fs, 31.25)
            .with_window_taps(63)
            .with_method(FractionalMethod::hann_sinc()),
    )
    .unwrap()
    .verify();
    println!(
        "T40-EVIDENCE HANN63 passed={} mag_dev={:.6} gd_dev={:.6}",
        hann63.passed(),
        hann63.band.mag_max_abs_dev_db,
        hann63.band.gd_max_abs_dev_samples
    );
    let hann127 = design_fractional(
        &FractionalDelaySpec::windowed_sinc(fs, 63.25)
            .with_window_taps(127)
            .with_method(FractionalMethod::hann_sinc()),
    )
    .unwrap()
    .verify();
    println!(
        "T40-EVIDENCE HANN127 passed={} mag_dev={:.6} gd_dev={:.6}",
        hann127.passed(),
        hann127.band.mag_max_abs_dev_db,
        hann127.band.gd_max_abs_dev_samples
    );
    let lag = design_fractional(
        &FractionalDelaySpec::lagrange(fs, 1.75, 3).with_verify_band([20.0, 4_800.0]),
    )
    .unwrap()
    .verify();
    println!(
        "T40-EVIDENCE LAGRANGE3 narrow_passed={} spec_band_compliant={} mag_dev={:.6} gd_dev={:.6}",
        lag.passed(),
        lag.checks.spec_band_compliant,
        lag.band.mag_max_abs_dev_db,
        lag.band.gd_max_abs_dev_samples
    );
    // 打印的数值同时断言可复算（防止日志与实现漂移）。
    assert!(v11_full.passed());
    assert!(v11_20k.band.mag_max_abs_dev_db <= V11_MAX_MAG_DEVIATION_DB + 1e-4);
    assert!(v11_10k.band.gd_max_abs_dev_samples <= 0.001);
    assert!(di.response_at(1_000.0).phase_deg < 0.0);
}
