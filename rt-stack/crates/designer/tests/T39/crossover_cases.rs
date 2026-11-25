// T39 crossover 私有测试（crates/designer/tests/T39/crossover_cases.rs）
//
// 归属：WBS T39 / task-75，owner rt-fir-crossover；写域 leaf + tests/T39 + evidence/T39。
// 本文件通过 src/crossover.rs 末尾的 include! 进入 #[cfg(test)] 子模块执行：
//
//     cargo test -p tspi-fir-designer crossover
//
// 覆盖（spec fir-toolbox.md v1.2-draft §4.1 + §3.1/§1.2-1.5 口径）：
// - 类型解析/未知 type 拒收；解析 LR/BW 模型手算向量（fc = −6.0206/−3.0103 dB、和全通性、
//   BW 偶数阶 +3.0103 dB、解析相对相位 90°·order、解析极性 LR2/LR6 反相）；
// - LR2/4/6/8 与 BW 奇数阶：合并 |S| 平坦 <= 0.5 dB、交叉点相对相位、极性、相对 D；
// - BW 偶数阶：解析和本身不平坦 → 诚实 INFEASIBLE（design_strict Err + 原始测量值）；
// - LinearPhaseMagnitude（§4.1 字面构造）与 BW/ LR2 判据的冲突量化（不静默 PASS）；
// - 有限幅相近似误差（独立 naive DFT 复算，不调用被测响应路径）、群延迟、振铃、支撑；
// - 无默认角色 HP：显式赋值编译、未赋值 = bypass、重复/越界拒收；
// - 参数边界（fc/taps/fs/attenuation）、线性相位方法奇数 taps 约束、确定性、96 kHz。
//
// 独立性说明：本文件的 t39_direct_* 用最朴素的逐点 sin/cos 直接 DFT 与解析群延迟，不调用
// crossover 模块的 FFT/谱/报告路径；解析目标手算用闭合公式独立复算。

// ---------------------------------------------------------------------------
// 独立复算工具（不调用被测 FFT/响应报告路径）
// ---------------------------------------------------------------------------

fn t39_dtft(h: &[f64], fs: f64, f: f64) -> (f64, f64) {
    let mut re = 0.0f64;
    let mut im = 0.0f64;
    for (n, &hn) in h.iter().enumerate() {
        let ph = -2.0 * std::f64::consts::PI * f * n as f64 / fs;
        let (s, c) = ph.sin_cos();
        re += hn * c;
        im += hn * s;
    }
    (re, im)
}

fn t39_mag_db(h: &[f64], fs: f64, f: f64) -> f64 {
    let (re, im) = t39_dtft(h, fs, f);
    20.0 * (re.hypot(im)).max(1e-12).log10()
}

fn t39_gd(h: &[f64], fs: f64, f: f64) -> f64 {
    let w = -2.0 * std::f64::consts::PI * f / fs;
    let mut hr = 0.0f64;
    let mut hi = 0.0f64;
    let mut gr = 0.0f64;
    let mut gi = 0.0f64;
    for (n, &hn) in h.iter().enumerate() {
        let ph = w * n as f64;
        let (s, c) = ph.sin_cos();
        let nf = n as f64;
        hr += hn * c;
        hi += hn * s;
        gr += nf * hn * c;
        gi += nf * hn * s;
    }
    let den = hr * hr + hi * hi;
    (gr * hr + gi * hi) / den
}

fn t39_lin_grid(lo: f64, hi: f64, points: usize) -> Vec<f64> {
    (0..points)
        .map(|i| lo + (hi - lo) * i as f64 / (points - 1) as f64)
        .collect()
}

fn t39_log_grid(lo: f64, hi: f64, points: usize) -> Vec<f64> {
    let ratio = hi / lo;
    (0..points)
        .map(|i| lo * ratio.powf(i as f64 / (points - 1) as f64))
        .collect()
}

fn t39_wrap_deg(d: f64) -> f64 {
    let mut x = d % 360.0;
    if x <= -180.0 {
        x += 360.0;
    } else if x > 180.0 {
        x -= 360.0;
    }
    x
}

/// 独立复算 |S| = |H_LP + H_HP| 在 [fc/2, 2fc] 内相对 max 偏差（dB）。
fn t39_independent_sum_dev(lp: &[f64], hp: &[f64], fc: f64, fs: f64, points: usize) -> f64 {
    let lo = (fc / 2.0).max(20.0);
    let hi = (2.0 * fc).min(0.499 * fs);
    let grid = t39_lin_grid(lo, hi, points);
    let mut vals = Vec::with_capacity(grid.len());
    for &f in &grid {
        let (lr, li) = t39_dtft(lp, fs, f);
        let (hr, hii) = t39_dtft(hp, fs, f);
        let mag = (lr + hr).hypot(li + hii);
        vals.push(20.0 * mag.max(1e-12).log10());
    }
    let max_db = vals.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    vals.iter().fold(0.0f64, |m, v| m.max((v - max_db).abs()))
}

/// 独立解析 LR/BW 幅度（闭合公式；不调用被测解析函数）。
fn t39_analytic_mag(kind: CrossoverType, branch: Branch, f: f64, fc: f64) -> f64 {
    let x = f / fc;
    let n = kind.order() as i32;
    let xp = x.powi(n);
    if kind.is_linkwitz_riley() {
        match branch {
            Branch::Lowpass => 1.0 / (1.0 + xp),
            Branch::Highpass => xp / (1.0 + xp),
        }
    } else {
        let den = (1.0 + x.powi(2 * n)).sqrt();
        match branch {
            Branch::Lowpass => 1.0 / den,
            Branch::Highpass => x.powi(n) / den,
        }
    }
}

fn t39_spec(kind: CrossoverType, fc: f64, taps: usize) -> CrossoverSpec {
    CrossoverSpec::new(48_000.0, kind, fc, taps)
}

fn t39_print(tag: &str, d: &CrossoverDesign) {
    println!("[T39 {tag}] ---- report ----");
    for line in d.summary_lines() {
        println!("[T39 {tag}] {line}");
    }
    for c in &d.criteria.criteria {
        println!(
            "[T39 {tag}] criterion {} pass={} measured={:?} limit={:?} {}",
            c.id, c.pass, c.measured, c.limit, c.unit
        );
    }
}

// ---------------------------------------------------------------------------
// 1. 类型解析与未知 type 拒收
// ---------------------------------------------------------------------------

#[test]
fn t39_type_parse_rejects_unknown() {
    for kind in CrossoverType::ALL {
        assert_eq!(CrossoverType::parse(kind.as_str()).unwrap(), kind);
        assert_eq!(
            CrossoverType::parse(&kind.as_str().to_ascii_lowercase()).unwrap(),
            kind
        );
    }
    assert_eq!(CrossoverType::parse("  LR4 ").unwrap(), CrossoverType::Lr4);
    assert_eq!(CrossoverType::parse("bw8").unwrap(), CrossoverType::Bw8);
    for bad in ["LR3", "BW9", "BW0", "LR", "", "LINKWITZ4", "LR4x", "BW-1"] {
        let e = CrossoverType::parse(bad).expect_err("must reject unknown type");
        assert_eq!(e.code(), E_FIR_XOVER_TYPE, "bad type {bad:?}");
    }
    assert_eq!(CrossoverType::ALL.len(), 12);
    assert_eq!(CrossoverType::Lr4.order(), 4);
    assert_eq!(CrossoverType::Bw3.order(), 3);
    assert!(CrossoverType::Lr2.is_linkwitz_riley());
    assert!(CrossoverType::Bw2.is_butterworth());
}

// ---------------------------------------------------------------------------
// 2. 解析模型手算向量
// ---------------------------------------------------------------------------

#[test]
fn t39_analytic_model_hand_vectors() {
    let fs = 48_000.0;
    let fc = 1_000.0;
    for kind in CrossoverType::ALL {
        // fc 处幅度：LR = −6.0206 dB，BW = −3.0103 dB（两支相同）
        let expect_fc = kind.analytic_magnitude_at_fc_db();
        for branch in [Branch::Lowpass, Branch::Highpass] {
            let h = analytic_response(kind, branch, fc, fc);
            assert!(
                (h.magnitude_db() - expect_fc).abs() < 1e-9,
                "{} {:?} fc mag {} vs {}",
                kind.as_str(),
                branch,
                h.magnitude_db(),
                expect_fc
            );
            // 与闭合公式独立对照（fc、fc/2、2fc 三点）
            for f in [fc / 2.0, fc, 2.0 * fc] {
                let expect = 20.0 * t39_analytic_mag(kind, branch, f, fc).log10();
                let got = analytic_response(kind, branch, fc, f).magnitude_db();
                assert!(
                    (got - expect).abs() < 1e-9,
                    "{} {:?} f={f} mag {got} vs {expect}",
                    kind.as_str(),
                    branch
                );
            }
        }
        // DC / Nyquist 极限
        let lp0 = analytic_response(kind, Branch::Lowpass, fc, 0.0).magnitude();
        let hp0 = analytic_response(kind, Branch::Highpass, fc, 0.0).magnitude();
        assert!((lp0 - 1.0).abs() < 1e-12, "{} LP(0)=1", kind.as_str());
        assert!(hp0 < 1e-12, "{} HP(0)=0", kind.as_str());
        let hp_hi = analytic_response(kind, Branch::Highpass, fc, fs * 0.49).magnitude();
        assert!(
            hp_hi > 0.5,
            "{} HP(0.49fs) must approach passband",
            kind.as_str()
        );
        // 家族相对极性（RULED 2026-11-17）：
        //   LR 全阶 → 家族极性应用后 fc 相对相位 0°（同相和）；
        //   BW 奇数阶 → 一支反相（180° 极性），fc 相对相位按解析目标 ±90°（不强制 0°）；
        //   BW 偶数阶 → 相位对齐规则 order%4==2。
        let pol = kind.analytic_polarity();
        let expect_invert =
            (kind.is_butterworth() && kind.order() % 2 == 1) || kind.order() % 4 == 2;
        assert_eq!(
            pol.highpass == Polarity::Inverted,
            expect_invert,
            "{} family polarity",
            kind.as_str()
        );
        let raw = t39_wrap_deg(90.0 * kind.order() as f64);
        let expected_rel = if pol.highpass == Polarity::Inverted {
            t39_wrap_deg(raw + 180.0)
        } else {
            raw
        };
        let rel = analytic_relative_phase_at_fc_deg(kind);
        assert!(
            (rel - expected_rel).abs() < 1e-9,
            "{} relative phase {rel} vs {expected_rel}",
            kind.as_str()
        );
        if kind.is_linkwitz_riley() {
            assert!(rel.abs() < 1e-9, "{} LR must be in-phase 0°", kind.as_str());
        } else if kind.order() % 2 == 1 {
            assert!(
                (rel.abs() - 90.0).abs() < 1e-9,
                "{} BW odd phase target must be ±90° (not 0°), got {rel}",
                kind.as_str()
            );
            assert_eq!(kind.family_relative_polarity_deg(), 180.0);
        }
        assert_eq!(
            kind.family_relative_polarity_deg(),
            if pol.is_same() { 0.0 } else { 180.0 }
        );
        // 解析和的平坦性
        assert_eq!(
            kind.analytic_sum_is_flat(),
            kind.is_linkwitz_riley() || kind.order() % 2 == 1
        );
        if kind.is_butterworth() && kind.order() % 2 == 0 {
            assert!((kind.analytic_sum_bump_at_fc_db() - BW_EVEN_SUM_BUMP_DB).abs() < 1e-12);
        } else {
            assert_eq!(kind.analytic_sum_bump_at_fc_db(), 0.0);
        }
    }

    // 解析复数和：全通类在多个频点 |S|=1；BW 偶数阶 @fc = √2（+3.0103 dB）
    let grid = t39_log_grid(20.0, 20_000.0, 300);
    for kind in CrossoverType::ALL {
        let pol = kind.analytic_polarity();
        for &f in &grid {
            let lp = analytic_response(kind, Branch::Lowpass, 1_000.0, f);
            let hp = analytic_response(kind, Branch::Highpass, 1_000.0, f);
            let sign = pol.highpass.sign();
            let mag = (lp.re + sign * hp.re).hypot(lp.im + sign * hp.im);
            if kind.analytic_sum_is_flat() {
                assert!(
                    (mag - 1.0).abs() < 1e-9,
                    "{} sum at {f} = {mag}",
                    kind.as_str()
                );
            } else {
                assert!(mag >= 1.0 - 1e-12, "{} sum < 1 at {f}", kind.as_str());
            }
        }
        if kind.is_butterworth() && kind.order() % 2 == 0 {
            let lp = analytic_response(kind, Branch::Lowpass, 1_000.0, 1_000.0);
            let hp = analytic_response(kind, Branch::Highpass, 1_000.0, 1_000.0);
            let sign = pol.highpass.sign();
            let mag = (lp.re + sign * hp.re).hypot(lp.im + sign * hp.im);
            assert!(
                (mag - std::f64::consts::SQRT_2).abs() < 1e-9,
                "{} even BW bump {mag}",
                kind.as_str()
            );
        }
    }

    // FFT 原语自检：delta 频谱 → 常数时间序列往返
    let n = 64usize;
    let mut re = vec![0.0f64; n];
    let mut im = vec![0.0f64; n];
    re[0] = 1.0;
    fft_in_place(&mut re, &mut im, false);
    for k in 0..n {
        assert!((re[k] - 1.0).abs() < 1e-12 && im[k].abs() < 1e-12);
    }
    fft_in_place(&mut re, &mut im, true);
    assert!((re[0] - 1.0).abs() < 1e-12);
    for k in 1..n {
        assert!(re[k].abs() < 1e-12, "delta roundtrip re[{k}]={}", re[k]);
    }
}

// ---------------------------------------------------------------------------
// 3. LR4：合并平坦、同极性、相对 D、近似误差
// ---------------------------------------------------------------------------

#[test]
fn t39_lr4_design_flat_sum_same_polarity() {
    let spec = t39_spec(CrossoverType::Lr4, 80.0, 2_048);
    let d = design(&spec).expect("LR4 design");
    t39_print("LR4", &d);
    assert!(d.accepted(), "LR4 failures: {:?}", d.failed_criteria());
    assert_eq!(d.kind, CrossoverType::Lr4);
    assert_eq!(d.method, CrossoverMethod::AnalyticPhase);
    assert_eq!(d.fc_tier, "production");
    assert!(!d.spec.experimental);
    assert!(d.criteria.get("xover_production_cap").unwrap().pass);
    assert!(d.criteria.get("xover_normative_construction").unwrap().pass);
    // F-24：LR 行为不变——家族极性按 0.5° 限计 pass；相位@fc 用 LR 0.5° 限。
    let fam = d.criteria.get("xover_family_polarity").unwrap();
    assert!(fam.pass);
    assert_eq!(fam.limit, Some(LR_PHASE_FC_LIMIT_DEG));
    let ph = d.criteria.get("xover_phase_at_fc").unwrap();
    assert!(ph.pass);
    assert_eq!(ph.limit, Some(LR_PHASE_FC_LIMIT_DEG));
    assert_eq!(d.merge.family_relative_polarity_deg, 0.0);
    // §4.1：LR4 两支同极性
    assert_eq!(d.merge.polarity, BranchPolarity::SAME);
    assert_eq!(d.merge.polarity_source, POLARITY_SOURCE_ANALYTIC);
    assert!(d.criteria.get("xover_lr4_polarity_same").unwrap().pass);
    // 合并平坦（假设 LR4 LP+HP 解析和 = 1 → 系数和 = δ）
    assert!(
        d.merge.flat_max_deviation_db <= 0.5,
        "LR4 sum deviation {} dB",
        d.merge.flat_max_deviation_db
    );
    assert!(d.merge.flat_pass);
    // 独立 naive DFT 复算同一带的 |S| 偏差
    let indep = t39_independent_sum_dev(
        d.coeffs(Branch::Lowpass),
        d.coeffs(Branch::Highpass),
        80.0,
        48_000.0,
        64,
    );
    assert!(indep <= 0.5, "independent LR4 sum dev {indep} dB");
    // 交叉点相对相位目标 0°
    assert!(
        d.merge.relative_phase_error_deg.abs() <= LR_PHASE_FC_LIMIT_DEG,
        "LR4 phase error {}",
        d.merge.relative_phase_error_deg
    );
    assert!((d.merge.analytic_relative_phase_at_fc_deg).abs() < 1e-9);
    // 相对 D：两支同 L、同 D=(taps−1)/2，相对 D 整数 0
    assert_eq!(d.lowpass.taps, 2_048);
    assert_eq!(d.highpass.taps, 2_048);
    assert_eq!(d.lowpass.declared_delay_samples, 1_023.5);
    assert_eq!(d.highpass.declared_delay_samples, 1_023.5);
    assert_eq!(d.merge.relative_delay_samples, 0.0);
    assert!(d.merge.relative_delay_is_integer);
    // 交叉点幅度 = LR −6.0206 dB（FIR 近似）
    assert!(
        (d.lowpass.magnitude.measured_at_fc_db - LR_AT_FC_DB).abs() <= 0.5,
        "LP @fc {} vs {}",
        d.lowpass.magnitude.measured_at_fc_db,
        LR_AT_FC_DB
    );
    assert!(
        (d.highpass.magnitude.measured_at_fc_db - LR_AT_FC_DB).abs() <= 0.5,
        "HP @fc {}",
        d.highpass.magnitude.measured_at_fc_db
    );
    // 幅度近似误差门
    assert!(
        d.lowpass.magnitude.max_abs_err_db <= MAG_APPROX_LIMIT_DB,
        "LP mag err {}",
        d.lowpass.magnitude.max_abs_err_db
    );
    assert!(
        d.highpass.magnitude.max_abs_err_db <= MAG_APPROX_LIMIT_DB,
        "HP mag err {}",
        d.highpass.magnitude.max_abs_err_db
    );
}

// ---------------------------------------------------------------------------
// 4. LR 全阶：解析极性、合并平坦、相位目标 0°
// ---------------------------------------------------------------------------

#[test]
fn t39_lr_orders_polarity_and_flat_sum() {
    for kind in [
        CrossoverType::Lr2,
        CrossoverType::Lr4,
        CrossoverType::Lr6,
        CrossoverType::Lr8,
    ] {
        // 低 fc 高阶 LR 的 ringing 更长：taps 按阶数放大（见 evidence/T39 的 taps 依据）
        let taps = match kind {
            CrossoverType::Lr2 | CrossoverType::Lr4 => 2_048usize,
            _ => 8_192usize,
        };
        let spec = t39_spec(kind, 80.0, taps);
        let d = design(&spec).expect("LR design");
        println!(
            "[T39 LR] {} accepted={} pol=hp:{} dev={:.6} phase_err={:.6}",
            kind.as_str(),
            d.accepted(),
            d.merge.polarity.highpass.as_str(),
            d.merge.flat_max_deviation_db,
            d.merge.relative_phase_error_deg
        );
        assert!(
            d.accepted(),
            "{} failures: {:?}",
            kind.as_str(),
            d.failed_criteria()
        );
        assert_eq!(
            d.merge.polarity,
            kind.analytic_polarity(),
            "{} polarity",
            kind.as_str()
        );
        assert_eq!(d.merge.measured_best_polarity, d.merge.polarity);
        assert!(
            d.merge.flat_max_deviation_db <= SUM_DEVIATION_LIMIT_DB,
            "{} sum dev {}",
            kind.as_str(),
            d.merge.flat_max_deviation_db
        );
        assert!(
            d.merge.relative_phase_error_deg.abs() <= LR_PHASE_FC_LIMIT_DEG,
            "{} phase err {}",
            kind.as_str(),
            d.merge.relative_phase_error_deg
        );
        assert!(d.merge.analytic_sum_flat);
        let indep = t39_independent_sum_dev(
            d.coeffs(Branch::Lowpass),
            d.coeffs(Branch::Highpass),
            80.0,
            48_000.0,
            64,
        );
        assert!(indep <= 0.5, "{} independent dev {indep} dB", kind.as_str());
        // 家族相对极性（RULED）：LR 全阶 fc 同相 0°（LR2/LR6 经 HP 反相后回到 0°；LR4/LR8 同相）
        assert_eq!(
            d.merge.polarity.highpass == Polarity::Inverted,
            kind.order() % 4 == 2,
            "{} HP polarity",
            kind.as_str()
        );
        assert!(
            d.merge.measured_relative_phase_at_fc_deg.abs() <= LR_PHASE_FC_LIMIT_DEG,
            "{} LR in-phase {:.3}°",
            kind.as_str(),
            d.merge.measured_relative_phase_at_fc_deg
        );
        assert_eq!(
            d.merge.family_relative_polarity_deg,
            if kind.order() % 4 == 2 { 180.0 } else { 0.0 }
        );
        assert!(d.criteria.get("xover_family_polarity").unwrap().pass);
    }
}

// ---------------------------------------------------------------------------
// 5. BW 奇数阶：解析全通和 → 合并平坦
// ---------------------------------------------------------------------------

#[test]
fn t39_bw_odd_design_allpass_sum() {
    for (kind, taps) in [
        (CrossoverType::Bw1, 2_560usize),
        (CrossoverType::Bw3, 2_560usize),
        (CrossoverType::Bw5, 4_096usize),
        (CrossoverType::Bw7, 8_192usize),
    ] {
        let spec = t39_spec(kind, 100.0, taps);
        let d = design(&spec).expect("BW odd design");
        println!(
            "[T39 BWodd] {} accepted={} dev={:.6} phase={:.6} target={:.6}",
            kind.as_str(),
            d.accepted(),
            d.merge.flat_max_deviation_db,
            d.merge.measured_relative_phase_at_fc_deg,
            d.merge.analytic_relative_phase_at_fc_deg
        );
        assert!(
            d.accepted(),
            "{} failures: {:?}",
            kind.as_str(),
            d.failed_criteria()
        );
        assert!(d.merge.analytic_sum_flat);
        assert!(
            d.merge.flat_max_deviation_db <= SUM_DEVIATION_LIMIT_DB,
            "{} sum dev {}",
            kind.as_str(),
            d.merge.flat_max_deviation_db
        );
        assert!(
            d.merge.relative_phase_error_deg.abs() <= BW_PHASE_FC_LIMIT_DEG,
            "{} phase err {}",
            kind.as_str(),
            d.merge.relative_phase_error_deg
        );
        // RULED：BW 奇数阶允许/要求一支反相（极性关系 180°）；fc 相对相位按解析目标（±90°），
        // 不强制 0°、不要求线性相位；解析和 (1∓s^n)/D 对奇数 n 仍全通 → 反相后幅频仍平坦。
        assert_eq!(d.merge.polarity_source, POLARITY_SOURCE_ANALYTIC);
        assert_eq!(d.merge.polarity, kind.analytic_polarity());
        assert_eq!(
            d.merge.polarity.highpass,
            Polarity::Inverted,
            "{} BW odd must use one-branch inverted polarity",
            kind.as_str()
        );
        assert_eq!(d.merge.family_relative_polarity_deg, 180.0);
        // F-24：奇数阶行为不变——家族极性按 180° 限计 pass；相位@fc 按 BW ±5° 限计 pass。
        let fam = d.criteria.get("xover_family_polarity").unwrap();
        assert!(fam.pass);
        assert_eq!(fam.limit, Some(180.0));
        let ph = d.criteria.get("xover_phase_at_fc").unwrap();
        assert!(ph.pass);
        assert_eq!(ph.limit, Some(BW_PHASE_FC_LIMIT_DEG));
        assert!(
            d.merge.measured_relative_phase_at_fc_deg.abs() > 45.0,
            "{} BW odd fc phase target must NOT be forced to 0° (measured {:.3}°)",
            kind.as_str(),
            d.merge.measured_relative_phase_at_fc_deg
        );
        assert!(
            (d.merge.measured_relative_phase_at_fc_deg.abs() - 90.0).abs() <= 5.0,
            "{} BW odd fc phase ≈ ±90° target, got {:.3}°",
            kind.as_str(),
            d.merge.measured_relative_phase_at_fc_deg
        );
        let indep = t39_independent_sum_dev(
            d.coeffs(Branch::Lowpass),
            d.coeffs(Branch::Highpass),
            100.0,
            48_000.0,
            64,
        );
        assert!(indep <= 0.5, "{} independent dev {indep} dB", kind.as_str());
        // BW 交叉点幅度 −3.0103 dB
        assert!(
            (d.lowpass.magnitude.measured_at_fc_db - BW_AT_FC_DB).abs() <= 0.5,
            "{} LP @fc {}",
            kind.as_str(),
            d.lowpass.magnitude.measured_at_fc_db
        );
    }
}

// ---------------------------------------------------------------------------
// 6. BW 偶数阶：解析和本身不平坦 → 诚实 INFEASIBLE
// ---------------------------------------------------------------------------

#[test]
fn t39_bw_even_honest_infeasible() {
    for (kind, taps) in [
        (CrossoverType::Bw2, 2_560usize),
        (CrossoverType::Bw4, 2_560usize),
        (CrossoverType::Bw6, 4_096usize),
        (CrossoverType::Bw8, 8_192usize),
    ] {
        let spec = t39_spec(kind, 100.0, taps);
        let d = design(&spec).expect("BW even design must still report");
        println!(
            "[T39 BWeven] {} accepted={} flat_dev={:.6} analytic_dev={:.6} flat_pass={}",
            kind.as_str(),
            d.accepted(),
            d.merge.flat_max_deviation_db,
            d.merge.analytic_sum_max_deviation_db,
            d.merge.flat_pass
        );
        // 解析复数和：偶数阶 BW 在 fc 抬高 +3.0103 dB（本实现极性按解析相位对齐）；
        // 带内相对 max 偏差 = 抬高 − 带边最低抬升（不是 3.0103 本身）
        assert!(!d.merge.analytic_sum_flat);
        assert!(
            (kind.analytic_sum_bump_at_fc_db() - BW_EVEN_SUM_BUMP_DB).abs() < 1e-12,
            "{} theory bump",
            kind.as_str()
        );
        assert!(
            d.merge.analytic_sum_max_deviation_db > 1.0,
            "{} analytic dev {} <= 1 dB",
            kind.as_str(),
            d.merge.analytic_sum_max_deviation_db
        );
        assert!(
            d.merge.analytic_sum_max_deviation_db <= BW_EVEN_SUM_BUMP_DB + 0.1,
            "{} analytic dev {} > bump",
            kind.as_str(),
            d.merge.analytic_sum_max_deviation_db
        );
        // 判据失败，且失败项含 sum_flat；不得静默 PASS
        assert!(!d.accepted(), "{} must not be accepted", kind.as_str());
        assert!(
            d.failed_criteria().contains(&"xover_sum_flat"),
            "{} failed={:?}",
            kind.as_str(),
            d.failed_criteria()
        );
        assert!(d.merge.flat_max_deviation_db > SUM_DEVIATION_LIMIT_DB);
        // F-24：BW 偶数阶不定义相位验收——家族极性/相位@fc 只报告、标 NOT_APPLICABLE、不计 pass，
        // 也不得以相位门放行；拒收只走合并平坦判据（E_FIR_XOVER_SUM）。
        let fam = d.criteria.get("xover_family_polarity").unwrap();
        assert!(!fam.pass, "{} family_polarity 不得计 pass", kind.as_str());
        assert!(fam.limit.is_none());
        assert!(
            fam.statement.contains("NOT_APPLICABLE"),
            "{} statement={}",
            kind.as_str(),
            fam.statement
        );
        let ph = d.criteria.get("xover_phase_at_fc").unwrap();
        assert!(!ph.pass, "{} phase_at_fc 不得计 pass", kind.as_str());
        assert!(
            ph.limit.is_none(),
            "{} 偶数阶相位项不得再套相位限",
            kind.as_str()
        );
        assert!(
            ph.statement.contains("NOT_APPLICABLE"),
            "{} statement={}",
            kind.as_str(),
            ph.statement
        );
        assert!(
            d.failed_criteria().contains(&"xover_family_polarity")
                && d.failed_criteria().contains(&"xover_phase_at_fc"),
            "{} NOT_APPLICABLE 项必须呈现为不计 pass: {:?}",
            kind.as_str(),
            d.failed_criteria()
        );
        // warnings 必须写明偶数阶 BW 解析和不是全通（RULED 记录）
        assert!(
            d.warnings.iter().any(|w| w.contains("偶数阶")),
            "{} warnings: {:?}",
            kind.as_str(),
            d.warnings
        );
        // strict 必须拒绝并给 E_FIR_XOVER_SUM，消息含 INFEASIBLE 与实测偏差；
        // F-24：偶数阶不得以相位门拒收（相位项 NOT_APPLICABLE），拒收只走 SUM。
        let e = design_strict(&spec).expect_err("BW even strict must Err");
        assert_eq!(e.code(), E_FIR_XOVER_SUM);
        assert!(e.detail().contains("INFEASIBLE"));
        assert!(
            !e.detail().contains("E_FIR_XOVER_PHASE"),
            "{} 偶数阶拒收只走 SUM（detail={}）",
            kind.as_str(),
            e.detail()
        );
    }
}

// ---------------------------------------------------------------------------
// 7. LinearPhaseMagnitude（非规范对照）：LR4 性质可用但不得验收；BW/LR2 冲突量化
// ---------------------------------------------------------------------------

#[test]
fn t39_linear_phase_method_conflict() {
    // RULED 2026-11-17：§4.1 规范构造选 AnalyticPhase；LinearPhaseMagnitude 仅对照，
    // 判据 xover_normative_construction 失败 → 即使 LR4 幅相性质通过也不得 accepted。
    let lr4 = t39_spec(CrossoverType::Lr4, 80.0, 2_049)
        .with_method(CrossoverMethod::LinearPhaseMagnitude);
    let d4 = design(&lr4).expect("LR4 linear-phase design");
    t39_print("LR4-LP", &d4);
    assert!(!d4.accepted(), "LinearPhaseMagnitude 不得被验收");
    assert!(d4
        .failed_criteria()
        .contains(&"xover_normative_construction"));
    assert!(d4.warnings.iter().any(|w| w.contains("非规范")));
    let e4 = design_strict(&lr4).expect_err("LR4 linear-phase strict must Err");
    assert!(e4.detail().contains("INFEASIBLE"));
    assert_eq!(d4.merge.polarity, BranchPolarity::SAME);
    assert!(d4.merge.flat_max_deviation_db <= 0.5);
    assert!((d4.merge.relative_phase_error_deg).abs() <= 0.5);
    // 相位非设计目标：report-only（limit=None）
    let pc = d4.criteria.get("xover_phase_approx").unwrap();
    assert!(pc.limit.is_none() && pc.pass);
    // 线性相位只约束支路 FIR 自身设计：结构对称（Type I）
    let sym_err = crate::linear::symmetry_error(
        d4.coeffs(Branch::Lowpass),
        crate::linear::Symmetry::Symmetric,
    );
    let max_abs = d4.lowpass.max_abs_h;
    assert!(sym_err <= 1e-7 * max_abs, "LR4 LP symmetry err {sym_err}");

    // BW3：线性相位同 L/同 D 只能给 0°/180°，无法复现 −90° 解析相位 → §4.1 矛盾
    let bw3 = t39_spec(CrossoverType::Bw3, 100.0, 2_049)
        .with_method(CrossoverMethod::LinearPhaseMagnitude);
    let d3 = design(&bw3).expect("BW3 linear-phase design");
    t39_print("BW3-LP", &d3);
    assert!(!d3.accepted());
    assert!(d3.failed_criteria().contains(&"xover_phase_at_fc"));
    assert!(d3.failed_criteria().contains(&"xover_sum_flat"));
    assert!(d3
        .failed_criteria()
        .contains(&"xover_normative_construction"));
    assert!(
        d3.merge.flat_max_deviation_db > SUM_DEVIATION_LIMIT_DB,
        "BW3 LP-method sum dev {}",
        d3.merge.flat_max_deviation_db
    );
    assert!(d3.merge.relative_phase_error_deg.abs() > 30.0);
    let e3 = design_strict(&bw3).expect_err("BW3 linear-phase strict must Err");
    assert_eq!(e3.code(), E_FIR_XOVER_PHASE);
    assert!(d3
        .warnings
        .iter()
        .any(|w| w.contains("LinearPhaseMagnitude")));

    // LR2：解析极性要求 HP 反相（已应用），线性相位构造下合并凹陷 → 失败；默认方法可用
    let lr2_lp = t39_spec(CrossoverType::Lr2, 80.0, 2_049)
        .with_method(CrossoverMethod::LinearPhaseMagnitude);
    let d2 = design(&lr2_lp).expect("LR2 linear-phase design");
    t39_print("LR2-LP", &d2);
    assert!(!d2.accepted());
    assert!(d2.merge.flat_max_deviation_db > SUM_DEVIATION_LIMIT_DB);
    assert!(d2
        .failed_criteria()
        .contains(&"xover_normative_construction"));
    assert!(d2
        .warnings
        .iter()
        .any(|w| w.contains("LinearPhaseMagnitude")));
    let lr2 = design(&t39_spec(CrossoverType::Lr2, 80.0, 2_048)).expect("LR2 analytic design");
    assert!(
        lr2.accepted(),
        "LR2 analytic failures: {:?}",
        lr2.failed_criteria()
    );
    // 偶数 taps 在 LinearPhaseMagnitude 下被拒（Type II 无法承载 HP Nyquist 增益）
    let mut even = t39_spec(CrossoverType::Lr4, 80.0, 2_048)
        .with_method(CrossoverMethod::LinearPhaseMagnitude);
    even.taps = 2_048;
    let e = design(&even).expect_err("even taps linear-phase must Err");
    assert_eq!(e.code(), E_FIR_TAPS);
}

// ---------------------------------------------------------------------------
// 8. 无默认角色 HP：显式赋值编译，未赋值 = bypass
// ---------------------------------------------------------------------------

#[test]
fn t39_no_default_role_hp() {
    let d = design(&t39_spec(CrossoverType::Lr4, 80.0, 2_048)).expect("design");
    // 空赋值：全部直通 —— 绝不默认 HP（高度默认全频）
    let empty = plan_outputs(&d, 8, &[]).expect("empty plan");
    assert_eq!(empty.assigned_outputs().len(), 0);
    assert_eq!(empty.bypass_outputs(), vec![0, 1, 2, 3, 4, 5, 6, 7]);
    assert!(empty.is_bypass(4) && empty.is_bypass(5));
    // 显式只给 SUB（7）LP、FL（0）HP；其余（含高度 4/5）保持直通
    let plan = plan_outputs(
        &d,
        8,
        &[OutputAssignment::lowpass(7), OutputAssignment::highpass(0)],
    )
    .expect("explicit plan");
    assert_eq!(plan.assigned_outputs(), vec![0, 7]);
    assert_eq!(plan.bypass_outputs(), vec![1, 2, 3, 4, 5, 6]);
    assert_eq!(plan.filter(7).unwrap(), d.coeffs(Branch::Lowpass));
    assert_eq!(plan.filter(0).unwrap(), d.coeffs(Branch::Highpass));
    assert!(plan.is_bypass(4) && plan.is_bypass(5));
    // 显式赋值才产生分频；role 名不参与本 API（分频必须来自 profile/校准）
    let e = plan_outputs(
        &d,
        8,
        &[OutputAssignment::lowpass(7), OutputAssignment::highpass(7)],
    )
    .expect_err("duplicate index must Err");
    assert_eq!(e.code(), E_FIR_XOVER_LAYOUT);
    let e = plan_outputs(&d, 8, &[OutputAssignment::lowpass(8)]).expect_err("out of range");
    assert_eq!(e.code(), E_FIR_XOVER_LAYOUT);
    let e = plan_outputs(&d, 0, &[]).expect_err("zero outputs");
    assert_eq!(e.code(), E_FIR_XOVER_LAYOUT);
}

// ---------------------------------------------------------------------------
// 9. 相对 D / 长度不变量与参数边界
// ---------------------------------------------------------------------------

#[test]
fn t39_relative_delay_and_parameters() {
    // AnalyticPhase：D=(taps−1)/2（可为半整数），两支同 L/同 D，相对 D=0
    let d = design(&t39_spec(CrossoverType::Lr4, 80.0, 1_024)).expect("design");
    assert_eq!(d.declared_delay_samples(), 511.5);
    assert_eq!(
        d.lowpass.declared_delay_samples,
        d.highpass.declared_delay_samples
    );
    assert!((d.lowpass.declared_delay_ms - 511.5 / 48_000.0 * 1000.0).abs() < 1e-12);
    assert_eq!(d.merge.relative_delay_samples, 0.0);
    assert!(d.merge.relative_delay_is_integer);
    assert_eq!(d.coeffs(Branch::Lowpass).len(), 1_024);
    assert_eq!(d.coeffs(Branch::Highpass).len(), 1_024);
    assert_eq!(
        declared_delay_samples(CrossoverMethod::AnalyticPhase, 1_024),
        511.5
    );
    assert_eq!(
        declared_delay_samples(CrossoverMethod::LinearPhaseMagnitude, 1_025),
        512.0
    );
    // LinearPhaseMagnitude：D=(taps−1)/2，两支相同且相对 D 整数
    let lpd = design(
        &t39_spec(CrossoverType::Lr4, 80.0, 1_025)
            .with_method(CrossoverMethod::LinearPhaseMagnitude),
    )
    .expect("linear-phase design");
    assert_eq!(lpd.lowpass.declared_delay_samples, 512.0);
    assert_eq!(lpd.highpass.declared_delay_samples, 512.0);
    assert_eq!(lpd.merge.relative_delay_samples, 0.0);
    assert!((lpd.lowpass.declared_delay_ms - 512.0 / 48_000.0 * 1000.0).abs() < 1e-12);
    // 参数/可设计边界（RULED：生产 ≤0.2fs；实验 ≤0.225fs；>0.225fs INFEASIBLE；spec 参数域 ≤0.45fs）
    assert!(design(&t39_spec(CrossoverType::Lr4, 20.0, 4_096)).is_ok());
    assert!(design(&t39_spec(CrossoverType::Lr4, 0.2 * 48_000.0, 2_048)).is_ok());
    let e = design(&t39_spec(CrossoverType::Lr4, 19.99, 1_024)).expect_err("fc < 20");
    assert_eq!(e.code(), E_FIR_BAND);
    let e =
        design(&t39_spec(CrossoverType::Lr4, 0.45 * 48_000.0, 1_024)).expect_err("fc > 0.225fs");
    assert_eq!(e.code(), E_FIR_XOVER_FC_INFEASIBLE);
    let e = design(&t39_spec(CrossoverType::Lr4, 0.45 * 48_000.0 + 1.0, 1_024))
        .expect_err("fc > 0.45fs");
    assert_eq!(e.code(), E_FIR_BAND);
    let mut bad = t39_spec(CrossoverType::Lr4, 80.0, 1_024);
    bad.taps = 7;
    assert_eq!(design(&bad).expect_err("taps too small").code(), E_FIR_TAPS);
    bad.taps = 16_385;
    assert_eq!(design(&bad).expect_err("taps too large").code(), E_FIR_TAPS);
    bad.taps = 1_024;
    bad.attenuation_db = 20.0;
    assert_eq!(
        design(&bad).expect_err("atten out of range").code(),
        E_FIR_BAND
    );
    let e = design(&CrossoverSpec::new(
        1_000.0,
        CrossoverType::Lr4,
        80.0,
        1_024,
    ))
    .expect_err("fs too small");
    assert_eq!(e.code(), E_FIR_BAND);
}

// ---------------------------------------------------------------------------
// 10. 支撑不足 → 诚实 INFEASIBLE（不静默 PASS）
// ---------------------------------------------------------------------------

#[test]
fn t39_insufficient_taps_infeasible() {
    for (kind, fc, taps) in [
        (CrossoverType::Lr4, 80.0, 64usize),
        (CrossoverType::Bw3, 80.0, 64usize),
        (CrossoverType::Lr2, 20.0, 256usize),
    ] {
        let spec = t39_spec(kind, fc, taps);
        let d = design(&spec).expect("design must return full report even if infeasible");
        println!(
            "[T39 INFEASIBLE] {} taps={taps} accepted={} failed={:?} mag_err={:.4} sum_dev={:.4}",
            kind.as_str(),
            d.accepted(),
            d.failed_criteria(),
            d.lowpass.magnitude.max_abs_err_db,
            d.merge.flat_max_deviation_db
        );
        assert!(
            !d.accepted(),
            "{} taps={taps} 不应在支撑不足时 accepted",
            kind.as_str()
        );
        assert!(
            d.warnings.iter().any(|w| w.contains("INFEASIBLE")),
            "{} taps={taps} 缺 INFEASIBLE warning",
            kind.as_str()
        );
        let e = design_strict(&spec).expect_err("strict must Err");
        assert!(
            e.code() == E_FIR_XOVER_SUM || e.code() == E_FIR_XOVER_PHASE,
            "{} code {}",
            kind.as_str(),
            e.code()
        );
        assert!(e.detail().contains("INFEASIBLE"));
    }
}

// ---------------------------------------------------------------------------
// 11. 群延迟 / 振铃 / 支撑 / 参考增益 报告
// ---------------------------------------------------------------------------

#[test]
fn t39_group_delay_ringing_support_reported() {
    let spec = t39_spec(CrossoverType::Lr4, 80.0, 2_048);
    let d = design(&spec).expect("design");
    for b in [&d.lowpass, &d.highpass] {
        println!(
            "[T39 report] {} GD@fc={:.6} analytic={:.6} GD@1k={:.6} maxerr={:.6} ring pre={:?} post={:?} support eff={} peak={}",
            b.branch.as_str(),
            b.group_delay.at_fc_samples,
            b.group_delay.analytic_at_fc_samples,
            b.group_delay.at_1khz_samples,
            b.group_delay.max_abs_err_samples,
            b.ringing.pre_db,
            b.ringing.post_db,
            b.support.effective_taps,
            b.support.peak_index
        );
        assert!(b.max_abs_h > 0.0 && b.max_abs_h.is_finite());
        assert!(b.sum_abs_h >= b.max_abs_h);
        assert!(b.sum_abs_h.is_finite());
        assert!(b.group_delay.at_fc_samples.is_finite());
        assert!(b.group_delay.at_1khz_samples.is_finite());
        assert!(b.group_delay.min_samples.is_finite() && b.group_delay.max_samples.is_finite());
        // GD 是报告项（判据只锁 phase_at_fc/phase_approx）；band 边缘弱幅度处 GD 误差更大，
        // 这里只要求有限且不失控（LR4@80 taps=2048 实测 ~15 samples @ -24.6 dB 频点）。
        assert!(
            b.group_delay.max_abs_err_samples < 50.0,
            "{} GD err {}",
            b.branch.as_str(),
            b.group_delay.max_abs_err_samples
        );
        assert_eq!(b.ringing.exclusion_samples, 16); // ceil(0.000333*48000)
        assert!(b.support.effective_taps <= b.taps);
        assert!(b.support.peak_index < b.taps);
        assert!(b.support.max_abs > 0.0);
        // 系数有限、无隐藏整体增益（LP DC ≈ 1；HP DC ≈ 0）
        assert!(b.coefficients.iter().all(|h| h.is_finite()));
    }
    assert!(
        (d.lowpass.dc_gain - 1.0).abs() < 0.05,
        "LP DC {}",
        d.lowpass.dc_gain
    );
    assert!(
        d.highpass.dc_gain.abs() < 0.05,
        "HP DC {}",
        d.highpass.dc_gain
    );
    // 独立解析 GD 对照（naive 复指数求和 vs 解析中心差分）在 crossover 带
    let gd_indep = t39_gd(d.coeffs(Branch::Lowpass), 48_000.0, 80.0);
    assert!((gd_indep - d.lowpass.group_delay.at_fc_samples).abs() < 1e-6);
    assert!(
        (d.lowpass.group_delay.physical_at_fc_samples
            - d.lowpass.group_delay.analytic_at_fc_samples)
            .abs()
            < 10.0,
        "LP physical-vs-analytic GD @fc: {} vs {}",
        d.lowpass.group_delay.physical_at_fc_samples,
        d.lowpass.group_delay.analytic_at_fc_samples
    );
    // 合并和 @fc 的群延迟有限（全通相位/δ 的 GD）
    assert!(d.merge.sum_group_delay_at_fc_samples.is_finite());
}

// ---------------------------------------------------------------------------
// 12. 确定性、有限性、摘要行
// ---------------------------------------------------------------------------

#[test]
fn t39_determinism_summary_and_finite() {
    let spec = t39_spec(CrossoverType::Lr4, 300.0, 2_048);
    let a = design(&spec).expect("design a");
    let b = design(&spec).expect("design b");
    assert_eq!(a, b, "同一输入两次设计必须逐值一致");
    assert_eq!(a.family(), FAMILY);
    assert_eq!(a.spec, spec);
    assert_eq!(a.kind, spec.kind);
    assert_eq!(a.method, spec.method);
    assert_eq!(a.fc_tier, "production");
    assert!(!a.spec.experimental);
    let lines = a.summary_lines();
    assert!(lines.iter().any(|l| l.contains("kind=LR4")));
    assert!(lines.iter().any(|l| l.contains("method=analytic_phase")));
    assert!(lines
        .iter()
        .any(|l| l.contains("source=analytic_filter_relation")));
    assert!(lines.iter().any(|l| l.contains("PASS")));
    assert!(lines.iter().any(|l| l.contains("fc_tier=production")));
    assert!(lines.iter().any(|l| l.contains("experimental=false")));
    assert!(lines
        .iter()
        .any(|l| l.contains("decimal") || l.contains("declared D")));
    // 判据 id 唯一且全部有稳定 id
    let mut ids: Vec<&str> = a.criteria.criteria.iter().map(|c| c.id).collect();
    ids.sort_unstable();
    let mut dedup = ids.clone();
    dedup.dedup();
    assert_eq!(ids, dedup, "criterion id 必须唯一");
    assert!(a.criteria.get("xover_sum_flat").is_some());
    // notes 含无默认角色 HP / 极性禁听感 / NOT_RUN
    assert!(a.notes.iter().any(|n| n.contains("无默认角色 HP")));
    assert!(a.notes.iter().any(|n| n.contains("听感")));
    assert!(a.notes.iter().any(|n| n.contains("NOT_RUN")));
}

// ---------------------------------------------------------------------------
// 13. 96 kHz 与窗/衰减变体
// ---------------------------------------------------------------------------

#[test]
fn t39_96k_and_window_variants() {
    // 同 fc 下 96k 的 GD samples ≈ 48k 的一半（物理延迟相同）
    let spec48 = t39_spec(CrossoverType::Lr4, 2_000.0, 2_048);
    let d48 = design(&spec48).expect("48k design");
    let spec96 = CrossoverSpec::new(96_000.0, CrossoverType::Lr4, 2_000.0, 2_048);
    let d96 = design(&spec96).expect("96k design");
    assert!(d96.accepted(), "96k failures: {:?}", d96.failed_criteria());
    let ratio = d96.lowpass.group_delay.physical_at_fc_samples
        / d48.lowpass.group_delay.physical_at_fc_samples;
    println!(
        "[T39 96k] GD48={:.6} GD96={:.6} ratio={:.6}",
        d48.lowpass.group_delay.at_fc_samples, d96.lowpass.group_delay.at_fc_samples, ratio
    );
    // 同一物理 fc 下 GD 秒数不变，samples = t·fs → 96k 约为 48k 的 2 倍
    assert!((ratio - 2.0).abs() < 0.2, "96k GD ratio {ratio}");
    assert_eq!(d96.merge.relative_delay_samples, 0.0);

    // Blackman-Harris 窗（旁瓣固定）——可用且报告 warning
    let bh = t39_spec(CrossoverType::Lr4, 200.0, 2_048)
        .with_window(crate::linear::WindowKind::BlackmanHarris);
    let d_bh = design(&bh).expect("BlackmanHarris design");
    assert!(
        d_bh.warnings.iter().any(|w| w.contains("BlackmanHarris")),
        "BH warning missing"
    );
    assert!(d_bh.coeffs(Branch::Lowpass).iter().all(|h| h.is_finite()));

    // attenuation 60 dB Kinger beta 变体：构造成功且系数有限
    let a60 = t39_spec(CrossoverType::Lr4, 200.0, 2_048).with_attenuation(60.0);
    let d_a60 = design(&a60).expect("attenuation 60 design");
    assert!(d_a60.coeffs(Branch::Lowpass).iter().all(|h| h.is_finite()));
    assert!(d_a60.coeffs(Branch::Highpass).iter().all(|h| h.is_finite()));
}

// ---------------------------------------------------------------------------
// 14. 相位近似：解析方法达到报告门；线性相位方法只报告不判
// ---------------------------------------------------------------------------

#[test]
fn t39_phase_approximation_analytic_vs_linear_phase() {
    let spec = t39_spec(CrossoverType::Lr4, 200.0, 2_048);
    let d = design(&spec).expect("analytic design");
    println!(
        "[T39 phase] analytic LP max={:.6} HP max={:.6} considered={}",
        d.lowpass.phase.max_abs_err_deg,
        d.highpass.phase.max_abs_err_deg,
        d.lowpass.phase.considered_points
    );
    assert!(d.lowpass.phase.max_abs_err_deg <= PHASE_APPROX_LIMIT_DEG);
    assert!(d.highpass.phase.max_abs_err_deg <= PHASE_APPROX_LIMIT_DEG);
    assert!(d.lowpass.phase.considered_points > 100);
    assert!(d.highpass.phase.considered_points > 100);

    let lp = design(
        &t39_spec(CrossoverType::Lr4, 200.0, 2_049)
            .with_method(CrossoverMethod::LinearPhaseMagnitude),
    )
    .expect("linear-phase design");
    println!(
        "[T39 phase] linear-phase LP max={:.6} (report-only)",
        lp.lowpass.phase.max_abs_err_deg
    );
    assert!(lp.lowpass.phase.max_abs_err_deg > d.lowpass.phase.max_abs_err_deg);
    assert!(lp.lowpass.phase.max_abs_err_deg > 2.0);
    assert!(lp
        .criteria
        .get("xover_phase_approx")
        .unwrap()
        .limit
        .is_none());
    assert!(lp.lowpass.phase.method_note.contains("linear-phase"));
}

// ---------------------------------------------------------------------------
// 15. FIR 近似误差与 20 Hz/20 kHz 端点报告
// ---------------------------------------------------------------------------

#[test]
fn t39_fir_approximation_and_endpoints() {
    for kind in [CrossoverType::Lr4, CrossoverType::Bw3] {
        let spec = t39_spec(kind, 200.0, 2_048);
        let d = design(&spec).expect("design");
        for b in [&d.lowpass, &d.highpass] {
            println!(
                "[T39 approx] {} {} band_max={:.6} dB full_max={:.6} dB @ {:.1} Hz at_fc={:.6} dB",
                kind.as_str(),
                b.branch.as_str(),
                b.magnitude.max_abs_err_db,
                b.magnitude.full_band_max_abs_err_db,
                b.magnitude.full_band_worst_hz,
                b.magnitude.at_fc_err_db
            );
            assert!(b.magnitude.max_abs_err_db <= MAG_APPROX_LIMIT_DB);
            assert!(b.magnitude.at_fc_err_db.abs() <= MAG_APPROX_LIMIT_DB);
            assert!(b.magnitude.full_band_max_abs_err_db.is_finite());
            assert!(b.magnitude.full_band_max_abs_err_db >= 0.0);
            assert!(b.magnitude.grid_points == GRID_POINTS);
            assert!(b.magnitude.band_hz[0] >= 20.0);
            assert!(b.magnitude.band_hz[1] <= 0.499 * 48_000.0);
        }
    }
    // 20 Hz / 20 kHz 端点可测（全带报告覆盖 [20, min(20k, 0.499fs)]）
    let d = design(&t39_spec(CrossoverType::Lr4, 200.0, 2_048)).expect("design");
    for f in [20.0f64, 20_000.0] {
        let _ = t39_mag_db(d.coeffs(Branch::Lowpass), 48_000.0, f);
        let _ = t39_mag_db(d.coeffs(Branch::Highpass), 48_000.0, f);
    }
    // RULED：生产上限内的高 fc 设计仍应满足判据（fc=0.2fs，band 上界 0.4fs < fade 0.45fs）
    let d9k6 = design(&t39_spec(CrossoverType::Lr4, 0.2 * 48_000.0, 2_048)).expect("9.6k design");
    assert!(
        d9k6.accepted(),
        "fc=0.2fs failures: {:?}",
        d9k6.failed_criteria()
    );
    // 0.2083fs（10 kHz）不再生产接受（Lead 裁决；详见 test 16 三档）
    let e10k = design(&t39_spec(CrossoverType::Lr4, 10_000.0, 2_048)).expect_err("10k reject");
    assert_eq!(e10k.code(), E_FIR_XOVER_FC_LIMIT);
}

// ---------------------------------------------------------------------------
// 16. RULED 2026-11-17 档位：生产 fc≤0.2fs / 实验 0.2–0.225fs / >0.225fs INFEASIBLE
// ---------------------------------------------------------------------------

#[test]
fn t39_fc_production_experimental_infeasible_tiers() {
    let fs = 48_000.0;
    // 档1 生产：fc ≤ 0.2·fs（=0.2fs 允许，判据 pass 且 accepted）
    let d =
        design(&t39_spec(CrossoverType::Lr4, 0.2 * fs, 2_048)).expect("0.2fs production design");
    assert_eq!(d.fc_tier, "production");
    assert!(!d.spec.experimental);
    assert!(d.accepted(), "0.2fs failures: {:?}", d.failed_criteria());
    assert!(d.criteria.get("xover_production_cap").unwrap().pass);
    // 0.2fs 之上（默认）直接拒绝：E_FIR_XOVER_FC_LIMIT
    let mut over = t39_spec(CrossoverType::Lr4, 0.2 * fs + 1.0, 2_048);
    let e = design(&over).expect_err("fc>0.2fs default reject");
    assert_eq!(e.code(), E_FIR_XOVER_FC_LIMIT);
    assert!(e.detail().contains("0.2"));
    // 档2 实验：0.2083fs（10 kHz）默认拒绝；显式 flag 后可设计但不得生产接受
    let e10k = design(&t39_spec(CrossoverType::Lr4, 10_000.0, 2_048))
        .expect_err("10k=0.2083fs default reject");
    assert_eq!(e10k.code(), E_FIR_XOVER_FC_LIMIT);
    over.experimental = true;
    let d_exp = design(&over).expect("experimental design");
    assert_eq!(d_exp.fc_tier, "experimental");
    assert!(d_exp.spec.experimental);
    assert!(
        !d_exp.accepted(),
        "experimental design must not be production-accepted"
    );
    assert!(d_exp.failed_criteria().contains(&"xover_production_cap"));
    assert!(d_exp.warnings.iter().any(|w| w.contains("EXPERIMENTAL")));
    // 实验上限 0.225fs：显式 flag 允许设计（仍非生产接受）；>0.225fs INFEASIBLE
    let boundary = t39_spec(CrossoverType::Lr4, 0.225 * fs, 2_048).with_experimental(true);
    let d_b = design(&boundary).expect("0.225fs experimental design");
    assert_eq!(d_b.fc_tier, "experimental");
    assert!(!d_b.accepted());
    let e =
        design(&t39_spec(CrossoverType::Lr4, 0.225 * fs + 1.0, 2_048)).expect_err("fc > 0.225fs");
    assert_eq!(e.code(), E_FIR_XOVER_FC_INFEASIBLE);
    assert!(e.detail().contains("INFEASIBLE"));
    // spec §4.1 参数域内的 0.45fs 同样 INFEASIBLE（不再有设计报告）
    let e = design(&t39_spec(CrossoverType::Lr4, 0.45 * fs, 1_024)).expect_err("fc=0.45fs");
    assert_eq!(e.code(), E_FIR_XOVER_FC_INFEASIBLE);
    // strict 对实验设计报生产上限（E_FIR_XOVER_FC_LIMIT + INFEASIBLE）
    let e = design_strict(&over).expect_err("experimental strict must Err");
    assert_eq!(e.code(), E_FIR_XOVER_FC_LIMIT);
    assert!(e.detail().contains("INFEASIBLE"));
}

// ---------------------------------------------------------------------------
// 17. RULED 2026-11-17 家族相对极性：LR 偶数阶 fc 同相 0°；BW 奇数阶一支反相 180°
// ---------------------------------------------------------------------------

#[test]
fn t39_family_polarity_lr_in_phase_bw_odd_inverted() {
    // LR2：HP 反相（家族极性关系 180°），但 fc 相对相位 = 0°（同相和）→ 平坦
    let lr2 = design(&t39_spec(CrossoverType::Lr2, 80.0, 2_048)).expect("LR2 design");
    assert_eq!(lr2.merge.family_relative_polarity_deg, 180.0);
    assert_eq!(lr2.merge.polarity.highpass, Polarity::Inverted);
    assert!(lr2.merge.measured_relative_phase_at_fc_deg.abs() <= LR_PHASE_FC_LIMIT_DEG);
    assert!(lr2.criteria.get("xover_family_polarity").unwrap().pass);
    assert!(lr2.accepted(), "LR2 failures: {:?}", lr2.failed_criteria());
    // LR4：两支同极性（0°）
    let lr4 = design(&t39_spec(CrossoverType::Lr4, 80.0, 2_048)).expect("LR4 design");
    assert_eq!(lr4.merge.family_relative_polarity_deg, 0.0);
    assert!(lr4.criteria.get("xover_family_polarity").unwrap().pass);

    // BW 奇数阶：一支反相（180° 极性）；fc 相位按解析目标 ±90°，不得强制 0°；
    // 解析和 (1∓s^n)/D 对奇数 n 仍全通 → 反相后求和幅频仍平坦、accepted。
    for (kind, taps) in [
        (CrossoverType::Bw1, 2_560usize),
        (CrossoverType::Bw3, 2_560usize),
        (CrossoverType::Bw5, 4_096usize),
        (CrossoverType::Bw7, 8_192usize),
    ] {
        let d = design(&t39_spec(kind, 100.0, taps)).expect("BW odd design");
        assert_eq!(
            d.merge.polarity.highpass,
            Polarity::Inverted,
            "{} must use one-branch inverted polarity",
            kind.as_str()
        );
        assert_eq!(d.merge.family_relative_polarity_deg, 180.0);
        assert!(d.criteria.get("xover_family_polarity").unwrap().pass);
        assert!(
            d.accepted(),
            "{} failures: {:?}",
            kind.as_str(),
            d.failed_criteria()
        );
        assert!(
            (d.merge.measured_relative_phase_at_fc_deg.abs() - 90.0).abs() <= BW_PHASE_FC_LIMIT_DEG,
            "{} fc phase target must be ±90° (not 0°), measured {:.3}°",
            kind.as_str(),
            d.merge.measured_relative_phase_at_fc_deg
        );
        assert!(d.merge.flat_pass);
        assert!(d.merge.flat_max_deviation_db <= SUM_DEVIATION_LIMIT_DB);
        // 相反极性也全通（诊断字段存在，不用于放行）
        assert!(d.merge.opposite_polarity_max_deviation_db.is_finite());
    }

    // LinearPhaseMagnitude（线性相位只约束支路 FIR 自身）：normative 判据失败，不得验收
    let lp = design(
        &t39_spec(CrossoverType::Lr4, 80.0, 2_049)
            .with_method(CrossoverMethod::LinearPhaseMagnitude),
    )
    .expect("linear-phase design");
    assert!(!lp.accepted());
    assert!(lp
        .failed_criteria()
        .contains(&"xover_normative_construction"));
}

// ---------------------------------------------------------------------------
// 18. F-22 终审修复：错误码唯一性（代码 ↔ spec 一致性）
// ---------------------------------------------------------------------------

/// spec 路径（rt-stack/spec/fir-toolbox.md；以 crate 根 CARGO_MANIFEST_DIR 相对定位）。
fn t39_spec_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/fir-toolbox.md")
}

#[test]
fn t39_xover_error_codes_align_spec() {
    // F-22（Cursor v0.9 终审；Lead 裁决 as-built 为准，2026-11-17）：
    //   参数域错误唯一码 = E_FIR_BAND；历史 E_FIR_XOVER_BAND 撤销（alias→E_FIR_BAND，不得再输出）；
    //   布局/拓扑码 E_FIR_XOVER_LAYOUT 正式登记进 spec §2.5（触发域＝布局非法，不属参数域）。
    let fs = 48_000.0;
    // 代码侧：参数域四类触发全部只输出 E_FIR_BAND（一个参数域错误只对应一个码）。
    for (label, spec) in [
        ("fc<20", t39_spec(CrossoverType::Lr4, 19.99, 1_024)),
        (
            "fc>0.45fs",
            t39_spec(CrossoverType::Lr4, 0.45 * fs + 1.0, 1_024),
        ),
        (
            "fs<8k",
            CrossoverSpec::new(1_000.0, CrossoverType::Lr4, 80.0, 1_024),
        ),
        (
            "attenuation",
            t39_spec(CrossoverType::Lr4, 80.0, 1_024).with_attenuation(20.0),
        ),
    ] {
        let e = design(&spec).expect_err(label);
        assert_eq!(e.code(), E_FIR_BAND, "{label} 必须只输出 E_FIR_BAND: {e}");
        assert_ne!(e.code(), "E_FIR_XOVER_BAND", "{label}: 撤销名不得再输出");
    }
    assert_eq!(E_FIR_BAND, "E_FIR_BAND");
    // 布局码是独立码（不是参数域码），T39 在测且 spec 已登记。
    assert_eq!(E_FIR_XOVER_LAYOUT, "E_FIR_XOVER_LAYOUT");
    let d = design(&t39_spec(CrossoverType::Lr4, 80.0, 2_048)).expect("design");
    let e = plan_outputs(&d, 8, &[OutputAssignment::lowpass(8)]).expect_err("layout");
    assert_eq!(e.code(), E_FIR_XOVER_LAYOUT);

    // spec 侧：E_FIR_XOVER_BAND 不得再作为独立错误码行出现；LAYOUT 必须有登记行与触发域说明。
    let text = std::fs::read_to_string(t39_spec_path()).expect("读取 spec/fir-toolbox.md");
    assert!(
        !text
            .lines()
            .any(|line| line.trim_start().starts_with("| E_FIR_XOVER_BAND |")),
        "spec §2.5 不得再把 E_FIR_XOVER_BAND 列为独立错误码（alias→E_FIR_BAND）"
    );
    assert!(
        text.contains("alias → `E_FIR_BAND`"),
        "spec 缺少 E_FIR_XOVER_BAND 撤销/alias 说明"
    );
    assert!(
        text
            .lines()
            .any(|line| line.trim_start().starts_with("| E_FIR_XOVER_LAYOUT |")),
        "spec §2.5 缺少 E_FIR_XOVER_LAYOUT 登记行"
    );
    assert!(
        text.contains("布局/拓扑非法"),
        "spec 须写明 E_FIR_XOVER_LAYOUT 触发域＝布局/拓扑非法"
    );
    assert!(
        text.contains("output_index 越界"),
        "spec 须给出 E_FIR_XOVER_LAYOUT 最小触发条件"
    );
}
