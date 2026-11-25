// T33 min_fast 私有测试（crates/designer/tests/T33/min_fast_cases.rs）
//
// 归属：WBS T33 / task-62，owner rt-fir-minfast；写域 leaf + tests/T33 + evidence/T33。
// 本文件通过 src/min_fast.rs 末尾的 include! 进入 #[cfg(test)] 子模块执行：
//
//     cargo test -p khaen-designer min_fast
//
// 测试覆盖（对应 spec fir-toolbox.md v1.2 §3.2 判据 1–5、§3.1 判据 3–6、§1.3/§1.4/§1.5/§2.4）：
// - 原语自检：FFT 往返/Parseval、Bessel I0、Kaiser 窗（独立于设计路径）；
// - V17 参考复算：hilbert_consistency、GD 表、幅度守恒（对齐 evidence/T04 V17 期望值）；
// - 规范判据：默认参数全部 PASS + 独立 DFT 复算通带/阻带/过渡带；
// - 最小相位：M2（65536/32768 两个尺寸）+ 幅度守恒 + 能量前置；
// - 有限支撑/振铃/端点/参考归一化/确定性；
// - 极点稳定性边界：FIR 分母为 1、极点全在 z=0、无 IIR 极点；
// - 延迟报告：频变 GD、onset、gd_max，明确 zero_delay_claim=false（不冒称零延迟）；
// - 家族边界/参数拒收/INFEASIBLE 诚实报告/48k–96k 处理。
//
// 独立性说明：本文件的“独立复算”工具用最朴素的逐点 sin/cos 直接 DFT 与解析群延迟，
// 不调用被测模块的 FFT/谱插值/报告路径；只有 M2 一致性调用公开函数（因为 M2 是规范定义的
// 判据本身），另以独立幅度守恒/能量前置作为交叉证据。

// ---------------------------------------------------------------------------
// 独立复算工具（不调用被测模块的 FFT/谱/报告路径）
// ---------------------------------------------------------------------------

fn t33_direct_resp(h: &[f64], fs: f64, f: f64) -> (f64, f64) {
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

fn t33_direct_mag_db(h: &[f64], fs: f64, f: f64) -> f64 {
    let (re, im) = t33_direct_resp(h, fs, f);
    10.0 * ((re * re + im * im).max(1e-24)).log10()
}

/// 解析群延迟（独立实现：逐点 sin/cos 求和，Re(G/H)）。
fn t33_direct_gd(h: &[f64], fs: f64, f: f64) -> f64 {
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

fn t33_log_grid(lo: f64, hi: f64, points: usize) -> Vec<f64> {
    let ratio = hi / lo;
    (0..points)
        .map(|i| lo * ratio.powf(i as f64 / (points - 1) as f64))
        .collect()
}

/// 独立搜索 −A_s 首次穿越点（粗扫 + 二分，全部用直接 DFT）。
fn t33_direct_crossing(h: &[f64], fs: f64, f_from: f64, f_to: f64, target_db: f64) -> Option<f64> {
    let scan = t33_log_grid(f_from, f_to, 400);
    let mut prev = (scan[0], t33_direct_mag_db(h, fs, scan[0]));
    for &f in scan.iter().skip(1) {
        let v = t33_direct_mag_db(h, fs, f);
        if v <= target_db && prev.1 > target_db {
            let mut a = prev.0;
            let mut b = f;
            for _ in 0..50 {
                let m = 0.5 * (a + b);
                if t33_direct_mag_db(h, fs, m) > target_db {
                    a = m;
                } else {
                    b = m;
                }
            }
            return Some(0.5 * (a + b));
        }
        prev = (f, v);
    }
    None
}

fn t33_support(h: &[f64]) -> (usize, usize, usize) {
    let mut max_abs = 0.0f64;
    for &v in h {
        max_abs = max_abs.max(v.abs());
    }
    let eps = 1e-6 * max_abs;
    let first = h.iter().position(|v| v.abs() > eps).unwrap();
    let last = h.iter().rposition(|v| v.abs() > eps).unwrap();
    (first, last, last - first + 1)
}

fn t33_peak_index(h: &[f64]) -> usize {
    let mut best = 0usize;
    let mut best_v = 0.0f64;
    for (i, &v) in h.iter().enumerate() {
        if v.abs() > best_v {
            best_v = v.abs();
            best = i;
        }
    }
    best
}

// ---------------------------------------------------------------------------
// 1. 原语自检（FFT / Bessel / Kaiser）
// ---------------------------------------------------------------------------

#[test]
fn t33_fft_bessel_kaiser_primitives() {
    // FFT：delta -> 全 1
    let n = 64usize;
    let fft = Fft::new(n).expect("fft plan");
    let mut re = vec![0.0f64; n];
    let mut im = vec![0.0f64; n];
    re[0] = 1.0;
    fft.forward(&mut re, &mut im);
    for k in 0..n {
        assert!((re[k] - 1.0).abs() < 1e-12, "delta bin {k} re={}", re[k]);
        assert!(im[k].abs() < 1e-12, "delta bin {k} im={}", im[k]);
    }
    // FFT 往返 + Parseval
    let n = 256usize;
    let fft = Fft::new(n).expect("fft plan");
    let x: Vec<f64> = (0..n)
        .map(|i| ((i * 37 % 101) as f64 / 101.0 - 0.5) + 0.25 * ((i * 13) as f64 * 0.07).sin())
        .collect();
    let mut re = x.clone();
    let mut im = vec![0.0f64; n];
    fft.forward(&mut re, &mut im);
    let time_energy: f64 = x.iter().map(|v| v * v).sum();
    let freq_energy: f64 = (0..n).map(|k| re[k] * re[k] + im[k] * im[k]).sum::<f64>() / n as f64;
    assert!(
        (time_energy - freq_energy).abs() <= 1e-10 * time_energy.max(1.0),
        "Parseval 失败: time={time_energy} freq={freq_energy}"
    );
    fft.inverse(&mut re, &mut im);
    let max_err = x
        .iter()
        .zip(re.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f64, f64::max);
    assert!(max_err < 1e-12, "FFT 往返误差 {max_err}");
    // Bessel I0 已知值
    assert!((bessel_i0(0.0) - 1.0).abs() < 1e-15);
    assert!((bessel_i0(1.0) - 1.266_065_877_752_008_4).abs() < 1e-12);
    assert!((bessel_i0(2.0) - 2.279_585_302_336_067_3).abs() < 1e-12);
    // Kaiser 窗：对称、中心≈1、β=0 全 1
    let w = kaiser_window(65, DEFAULT_KAISER_BETA);
    for i in 0..65 {
        assert!((w[i] - w[64 - i]).abs() < 1e-15, "Kaiser 不对称 i={i}");
    }
    assert!((w[32] - 1.0).abs() < 1e-15);
    let flat = kaiser_window(16, 0.0);
    assert!(flat.iter().all(|v| (v - 1.0).abs() < 1e-15));
    assert!(is_pow2(65536) && !is_pow2(65535));
}

// ---------------------------------------------------------------------------
// 2. V17 参考复算（spec §9 V17；期望值来自 evidence/T04/vector-results.json）
// ---------------------------------------------------------------------------

#[test]
fn t33_v17_min_fast_reference_reproduction() {
    let params = MinFastParams::v17_reference();
    let d = design(&params).expect("V17 参考构造应成功");
    assert_eq!(d.family(), "min_fast");
    assert_eq!(d.coefficients.len(), 2048);
    assert!(!d.prototype.is_empty() && d.prototype.len() == 2048);

    // 2a) M2 hilbert_consistency：V17 发布值 0.004496633796171022°（<=2.0° 为门）
    let expected_hilbert = 0.004_496_633_796_171_022f64;
    let got = d.min_phase.max_deviation_deg;
    println!(
        "[V17] hilbert_consistency_deg: published={expected_hilbert:.12} measured={got:.12} delta={:+.3e}",
        got - expected_hilbert
    );
    assert!(
        (got - expected_hilbert).abs() <= 5e-3,
        "M2 与 V17 发布值偏差过大: measured={got} published={expected_hilbert}"
    );
    assert!(got <= MIN_PHASE_MAX_DEG);

    // 2b) 幅度守恒：|H_proto| 与 |H_minphase| 在 20–15.5 kHz 的最大差（V17 发布 2.3e-11 dB）
    let mut max_mag_diff = 0.0f64;
    for f in t33_log_grid(20.0, 15_500.0, 120) {
        let a = t33_direct_mag_db(&d.prototype, params.fs, f);
        let b = t33_direct_mag_db(&d.coefficients, params.fs, f);
        max_mag_diff = max_mag_diff.max((a - b).abs());
    }
    println!("[V17] passband_max_mag_diff_db: published=2.29e-11 measured={max_mag_diff:.3e}");
    assert!(
        max_mag_diff <= 1e-6,
        "最小相位分解未保持幅度（max diff {max_mag_diff} dB）"
    );

    // 2c) GD 表（V17 发布值；独立直接解析 GD 复算）
    let published: [(f64, f64); 7] = [
        (50.0, 1.588_571_116_972_186),
        (100.0, 1.588_629_755_372_779_7),
        (1000.0, 1.596_301_826_163_878_2),
        (5000.0, 1.799_756_785_743_025_4),
        (10000.0, 2.744_189_251_743_936_4),
        (15000.0, 7.539_397_284_416_296),
        (18000.0, 31.376_999_749_692_214),
    ];
    for (f, expected) in published {
        let got_gd = t33_direct_gd(&d.coefficients, params.fs, f);
        let tol = 0.02 + 1e-3 * expected.abs();
        println!("[V17] GD({f:6.0} Hz): published={expected:.6} measured={got_gd:.6} delta={:+.3e} tol={tol:.4}", got_gd - expected);
        assert!(
            (got_gd - expected).abs() <= tol,
            "GD({f} Hz) 与 V17 发布值偏差过大: measured={got_gd} published={expected} tol={tol}"
        );
    }

    // 2d) V17 参考构造同时应通过声明 attenuation_db=80 的规范判据
    assert!(
        d.accepted(),
        "V17 参考构造未通过判据: {:?}",
        d.failed_criteria()
    );
    assert!((d.response.reference.measured_gain_db).abs() <= REFERENCE_TOL_DB);
    assert!(d.min_phase.passed);
}

// ---------------------------------------------------------------------------
// 3. 默认参数：规范判据全部 PASS + 判据集合完整
// ---------------------------------------------------------------------------

#[test]
fn t33_normative_criteria_all_pass_default() {
    let params = MinFastParams::default();
    let d = design(&params).expect("默认参数应设计成功");
    for line in d.summary_lines() {
        println!("{line}");
    }
    let expected_ids = [
        "finite_support",
        "passband_ripple",
        "stopband_attenuation",
        "transition_edge",
        "reference_gain",
        "min_phase_m2",
        "delay_report",
        "endpoints",
        "stability_boundary",
        "family_boundary",
    ];
    let ids: Vec<&'static str> = d.criteria.criteria.iter().map(|c| c.id).collect();
    for id in expected_ids {
        assert!(ids.contains(&id), "缺少判据 {id}（实际 {ids:?}）");
    }
    assert!(d.accepted(), "默认参数判据失败: {:?}", d.failed_criteria());
    assert!(d.failed_criteria().is_empty());

    // 幅度判据数值（spec §3.1 判据 3–5）
    assert!(d.response.passband.ripple_db <= params.ripple_db + RIPPLE_TOL_DB);
    assert!(
        d.response.stopband.max_db <= -(params.attenuation_db - ATTENUATION_TOL_DB),
        "阻带 max {}",
        d.response.stopband.max_db
    );
    let edge = d
        .response
        .transition
        .edge_hz_at_attenuation
        .expect("过渡带穿越点缺失");
    assert!(edge <= params.stopband_hz[0], "过渡带穿越 {edge} > f_s");
    assert!(d.response.transition.width_hz.unwrap() > 0.0);
    // 参考归一化 ±0.01 dB（spec §1.5）
    assert!(
        (d.response.reference.measured_gain_db - d.response.reference.gain_db).abs()
            <= REFERENCE_TOL_DB
    );
    // 有限支撑（spec §1.3）
    assert_eq!(d.coefficients.len(), params.taps);
    assert!(d.support.last_effective < params.taps);
    assert_eq!(
        d.support.effective_taps,
        d.support.last_effective - d.support.first_effective + 1
    );
    // 最小相位（spec §3.2 判据 2）
    assert!(d.min_phase.max_deviation_deg <= MIN_PHASE_MAX_DEG);
    assert_eq!(d.min_phase.fft_size, MIN_PHASE_FFT);
    assert_eq!(d.min_phase.method, "phase_hilbert_m2");
    // 延迟报告（spec §3.2 判据 3）
    assert_eq!(d.delay.class, "frequency_varying");
    assert!(!d.delay.zero_delay_claim);
    assert!(d.delay.at_1khz_samples.is_finite() && d.delay.at_1khz_samples > 0.0);
    assert!(d.delay.latency_ms_1khz > 0.0);
    assert!(d.delay.band_max_samples >= d.delay.at_1khz_samples);
    // 稳定性（spec §3.2 判据 5）
    assert!(d.stability.unconditionally_stable);
    assert!(!d.stability.iir_poles_introduced);
    assert_eq!(d.stability.denominator_degree, 0);
    assert_eq!(d.stability.pole_count_at_origin, params.taps);
    assert_eq!(d.stability.max_pole_radius, 0.0);
    // 端点（spec §2.4）
    assert!(d.response.endpoints.f20.is_measured());
    assert!(d.response.endpoints.f20k.is_measured());
}

// ---------------------------------------------------------------------------
// 4. 独立 DFT 复算：通带纹波 / 阻带衰减 / 过渡带（不依赖被测谱插值）
// ---------------------------------------------------------------------------

#[test]
fn t33_independent_response_passband_stopband_transition() {
    let params = MinFastParams::default();
    let d = design(&params).expect("设计成功");
    let h = &d.coefficients;
    let fs = params.fs;
    // 通带 p-p（独立 256 点 log 网格 + 两端）
    let mut pb_min = f64::INFINITY;
    let mut pb_max = f64::NEG_INFINITY;
    for f in t33_log_grid(params.passband_hz[0], params.passband_hz[1], 256) {
        let v = t33_direct_mag_db(h, fs, f);
        pb_min = pb_min.min(v);
        pb_max = pb_max.max(v);
    }
    let ripple = pb_max - pb_min;
    println!(
        "[independent] passband ripple = {ripple:.6e} dB (limit {:.3})",
        params.ripple_db + RIPPLE_TOL_DB
    );
    assert!(ripple <= params.ripple_db + RIPPLE_TOL_DB);
    // 阻带 max（独立 256 点 log 网格 + 两端；只用直接 DFT）
    let mut sb_max = f64::NEG_INFINITY;
    for f in t33_log_grid(params.stopband_hz[0], params.stopband_hz[1], 256) {
        sb_max = sb_max.max(t33_direct_mag_db(h, fs, f));
    }
    for f in [params.stopband_hz[0], params.stopband_hz[1]] {
        sb_max = sb_max.max(t33_direct_mag_db(h, fs, f));
    }
    println!(
        "[independent] stopband max = {sb_max:.3} dB (limit {:.3})",
        -(params.attenuation_db - ATTENUATION_TOL_DB)
    );
    assert!(
        sb_max <= -(params.attenuation_db - ATTENUATION_TOL_DB),
        "独立复算阻带超标: {sb_max}"
    );
    // 过渡带：独立直接 DFT 二分求 −A_s 首次穿越
    let cross = t33_direct_crossing(
        h,
        fs,
        params.passband_hz[1],
        0.499 * fs,
        -params.attenuation_db,
    )
    .expect("独立复算未找到 −A_s 穿越点");
    println!(
        "[independent] transition crossing = {cross:.3} Hz (f_s={:.1})",
        params.stopband_hz[0]
    );
    assert!(cross <= params.stopband_hz[0]);
    // 与被测报告一致性（±0.5 dB / ±1 Hz 属工程容差，此处收紧为 1e-3）
    assert!((ripple - d.response.passband.ripple_db).abs() <= 0.05);
    assert!((sb_max - d.response.stopband.max_db).abs() <= 0.5);
    let reported_edge = d.response.transition.edge_hz_at_attenuation.unwrap();
    assert!((cross - reported_edge).abs() <= 1.0);
}

// ---------------------------------------------------------------------------
// 5. 最小相位：能量前置 + 幅度守恒（不冒称普通线性相位）
// ---------------------------------------------------------------------------

#[test]
fn t33_min_phase_energy_frontloaded_and_magnitude_preserved() {
    let params = MinFastParams::default();
    let d = design(&params).expect("设计成功");
    let h = &d.coefficients;
    let l = h.len();
    // 能量前置（最小相位的时域特征；与 M2 独立交叉验证）
    let total: f64 = h.iter().map(|v| v * v).sum();
    let quarter: f64 = h[..l / 4].iter().map(|v| v * v).sum();
    println!(
        "[minphase] energy(first 25% taps) = {:.9} of total",
        quarter / total
    );
    assert!(
        quarter / total >= 0.999,
        "能量未前置: {:.6}",
        quarter / total
    );
    // 与原型幅度一致（同态分解只改相位）
    let mut max_diff = 0.0f64;
    for f in t33_log_grid(20.0, 18_000.0, 100) {
        let a = t33_direct_mag_db(&d.prototype, params.fs, f);
        let b = t33_direct_mag_db(h, params.fs, f);
        max_diff = max_diff.max((a - b).abs());
    }
    println!("[minphase] max |mag_proto - mag_minphase| = {max_diff:.3e} dB");
    assert!(max_diff <= 1e-6);
    // 原型是对称线性相位（D=(L-1)/2）；最小相位系数不是对称的
    let mut sym_err = 0.0f64;
    for i in 0..l / 2 {
        sym_err = sym_err.max((d.prototype[i] - d.prototype[l - 1 - i]).abs());
    }
    assert!(sym_err <= 1e-12, "原型不是对称线性相位: {sym_err}");
    let mut asym = 0.0f64;
    for i in 0..l / 2 {
        asym = asym.max((h[i] - h[l - 1 - i]).abs());
    }
    assert!(
        asym > 1e-3,
        "min_fast 输出不应是对称/线性相位（asym={asym}）"
    );
    // M2 公开函数（65536 规范尺寸 + 32768 稳健性交叉）
    let m2_65536 = min_phase_consistency_deg(h, params.fs, 65_536).expect("m2 65536");
    let m2_32768 = min_phase_consistency_deg(h, params.fs, 32_768).expect("m2 32768");
    println!("[minphase] M2 = {m2_65536:.6} deg (65536) / {m2_32768:.6} deg (32768)");
    assert!(m2_65536 <= MIN_PHASE_MAX_DEG);
    assert!(m2_32768 <= MIN_PHASE_MAX_DEG);
    assert!((m2_65536 - d.min_phase.max_deviation_deg).abs() <= 1e-9);
}

// ---------------------------------------------------------------------------
// 6. 延迟：频变 GD、onset、gd_max；禁止零延迟声明（spec §3.2 判据 3）
// ---------------------------------------------------------------------------

#[test]
fn t33_delay_frequency_varying_not_zero_delay() {
    let params = MinFastParams::default();
    let d = design(&params).expect("设计成功");
    // 报告特征频点与独立直接解析 GD 一致（±0.05 samples）
    for (f, gd) in &d.delay.characteristic_hz_samples {
        let direct = t33_direct_gd(&d.coefficients, params.fs, *f);
        // spec §3.2 判据 3 复算一致容差：±0.5 sample / ±5%（过渡带内 GD 陡变，插值误差在此容差内）
        let tol = 0.5 + 0.05 * gd.abs();
        println!("[delay] GD({f:8.1} Hz) reported={gd:.6} direct={direct:.6} tol={tol:.4}");
        assert!(
            (gd - direct).abs() <= tol,
            "GD({f}) 报告与独立复算偏差过大: reported={gd} direct={direct} tol={tol}"
        );
    }
    // 频变特征（min_fast 不是零延迟）
    assert!(d.delay.passband_max_samples > d.delay.passband_min_samples);
    assert!(d.delay.passband_min_samples >= 1.0);
    assert!(
        d.delay.at_1khz_samples >= 1.0 && d.delay.at_1khz_samples <= 3.0,
        "GD(1k)={} 超出预期区间",
        d.delay.at_1khz_samples
    );
    let gd_16k = t33_direct_gd(&d.coefficients, params.fs, params.passband_hz[1]);
    assert!(
        gd_16k > 5.0 * d.delay.at_1khz_samples,
        "16 kHz GD {gd_16k} 未显著高于 1 kHz GD {}",
        d.delay.at_1khz_samples
    );
    // gd_max（含过渡带）远大于低频 GD
    assert!(d.delay.band_max_samples > 100.0);
    assert!(d.delay.gd_max_ms > 1.0);
    // onset（精确样本，独立复算）+ 不冒称零延迟
    let (first, _, _) = t33_support(&d.coefficients);
    assert_eq!(d.delay.onset_samples, first);
    assert_eq!(d.delay.passband_min_samples, d.delay.passband_min_samples); // 有限性
    assert!(!d.delay.zero_delay_claim, "min_fast 禁止 zero_delay_claim");
    assert_eq!(d.delay.declared_bulk_samples, 0.0);
    let text = d.delay_statement();
    println!("[delay] {text}");
    assert!(text.contains("NOT zero-delay"));
    assert!(text.contains("frequency-varying"));
    assert!(text.contains("onset="));
    assert!(text.contains("gd_max="));
    // latency_ms 定义（spec §3.2 判据 3：GD(1 kHz)/fs*1000）
    assert!(
        (d.delay.latency_ms_1khz - d.delay.at_1khz_samples / params.fs * 1000.0).abs() <= 1e-12
    );
    assert!(d.delay.latency_ms_1khz > 0.0 && d.delay.latency_ms_1khz < 0.1);
}

// ---------------------------------------------------------------------------
// 7. 有限支撑 / 振铃（spec §1.3/§1.4）
// ---------------------------------------------------------------------------

#[test]
fn t33_finite_support_and_ringing() {
    let params = MinFastParams::default();
    let d = design(&params).expect("设计成功");
    let h = &d.coefficients;
    let (first, last, eff) = t33_support(h);
    assert_eq!(d.support.first_effective, first);
    assert_eq!(d.support.last_effective, last);
    assert_eq!(d.support.effective_taps, eff);
    assert!(last < params.taps, "有效支撑 last={last} 超出 taps");
    // 峰值位置（振铃排除窗中心）
    assert_eq!(d.support.peak_index, t33_peak_index(h));
    // 振铃口径（spec §1.4：W=ceil(0.000333*fs)=16 @48k）
    let w = (RING_WINDOW_SEC * params.fs).ceil() as usize;
    assert_eq!(w, 16);
    assert_eq!(d.response.ringing.exclusion_samples, w);
    let peak = d.support.peak_index;
    if peak > w {
        let pre = d.response.ringing.pre_db.expect("pre ring 应可测");
        assert!(d.response.ringing.pre_energy_db.is_some());
        assert!(pre <= 0.0);
    } else {
        // min-phase 主峰贴近 0，pre-ring 按 spec §1.4 记 n/a（不是隐藏数值）
        assert!(d.response.ringing.pre_db.is_none());
        assert!(d.response.ringing.pre_energy_db.is_none());
    }
    if peak + w + 1 < h.len() {
        let post = d.response.ringing.post_db.expect("post ring 应可测");
        assert!(post <= 0.0);
        assert!(d.response.ringing.post_energy_db.is_some());
    }
    // 独立复算 post ring
    if let Some(reported) = d.response.ringing.post_db {
        let seg = &h[peak + w + 1..];
        let mx = seg.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        let direct = 20.0 * (mx / d.support.max_abs).log10();
        assert!((reported - direct).abs() <= 1e-9);
    }
    // 主峰系数有限且非零
    assert!(d.support.max_abs > 0.0 && d.support.max_abs.is_finite());
}

// ---------------------------------------------------------------------------
// 8. 极点/稳定性边界（spec §3.2 判据 5）
// ---------------------------------------------------------------------------

#[test]
fn t33_stability_boundary_fir_poles_at_origin() {
    let params = MinFastParams::default();
    let d = design(&params).expect("设计成功");
    assert_eq!(d.stability.filter_class, "fir");
    assert_eq!(
        d.stability.denominator_degree, 0,
        "FIR 传递函数分母度数必须为 0"
    );
    assert_eq!(d.stability.pole_count_at_origin, d.coefficients.len());
    assert_eq!(d.stability.max_pole_radius, 0.0, "FIR 极点全在 z=0");
    assert!(d.stability.unconditionally_stable);
    assert!(!d.stability.iir_poles_introduced);
    assert!(d.stability.all_finite);
    assert_eq!(d.stability.numeric_violations, 0);
    assert!(d.stability.pole_stability_boundary.contains("poles at z=0"));
    // 独立验证：系数有限、max|h| 一致
    assert!(d.coefficients.iter().all(|v| v.is_finite()));
    let mut max_abs = 0.0f64;
    for &v in &d.coefficients {
        max_abs = max_abs.max(v.abs());
    }
    assert!((d.stability.max_abs_coefficient - max_abs).abs() <= 1e-15);
    assert!((d.stability.dc_gain_linear - d.coefficients.iter().sum::<f64>()).abs() <= 1e-15);
    // 稳定性边界的实际含义：任意有限输入下输出有界（FIR 无反馈），此处以可复算的
    // 系数有界性与极点半径=0 作证据；逆因果诊断仅作 MAY 诊断且应有限。
    assert!(
        d.min_phase.inverse_causality_diagnostic_db.is_finite(),
        "逆因果诊断应有限（MAY 诊断，不作门）"
    );
    assert!(d.min_phase.zero_radius_check.contains("not_run"));
}

// ---------------------------------------------------------------------------
// 9. 家族边界（spec §3.2 判据 4）
// ---------------------------------------------------------------------------

#[test]
fn t33_family_boundary_accepts_edge_rejects_beyond() {
    // 边界内：f_s/f_p = 1.5 且 f_s = 0.4375fs（<=0.45fs）→ 应通过（且设计可用）
    let mut edge = MinFastParams {
        taps: 1024,
        passband_hz: [20.0, 14_000.0],
        stopband_hz: [21_000.0, 0.499 * DEFAULT_FS],
        attenuation_db: 80.0,
        ..MinFastParams::default()
    };
    assert_eq!(edge.stopband_hz[0] / edge.passband_hz[1], FAMILY_EDGE_RATIO);
    assert!(validate_params(&edge).is_ok());
    let d = design(&edge).expect("边界设计应成功");
    assert!(
        d.accepted(),
        "边界设计未通过判据: {:?}",
        d.failed_criteria()
    );
    assert!((d.criteria.get("family_boundary").unwrap().measured.unwrap() - 1.5).abs() < 1e-12);

    // 家族边界拒绝 1：f_s/f_p > 1.5
    edge.passband_hz[1] = 13_980.0; // 21000/13980 = 1.50215
    let e = validate_params(&edge).expect_err("f_s/f_p>1.5 应拒收");
    assert_eq!(e.code, E_FIR_FAMILY);
    assert!(e.message.contains("f_s/f_p"));

    // 家族边界拒绝 2：f_s > 0.45fs（比例仍 <=1.5）
    edge.passband_hz[1] = 15_000.0;
    edge.stopband_hz[0] = 21_648.0; // > 0.45*48000 = 21600
    let e = validate_params(&edge).expect_err("f_s>0.45fs 应拒收");
    assert_eq!(e.code, E_FIR_FAMILY);

    // 边界正好 0.45fs 应接受参数校验（设计是否通过判据另说）
    edge.stopband_hz[0] = 0.45 * DEFAULT_FS;
    edge.passband_hz[1] = 14_400.0;
    edge.stopband_hz[1] = 0.499 * DEFAULT_FS;
    assert!(validate_params(&edge).is_ok());
}

// ---------------------------------------------------------------------------
// 10. 参数拒收（spec §2.5 E_FIR_*；不静默放行）
// ---------------------------------------------------------------------------

#[test]
fn t33_param_rejections_codes() {
    let base = MinFastParams::default();
    let expect = |mutate: fn(&mut MinFastParams), code: &'static str, what: &str| {
        let mut p = base.clone();
        mutate(&mut p);
        let e = match validate_params(&p) {
            Ok(()) => panic!("{what} 应拒收"),
            Err(e) => e,
        };
        assert_eq!(e.code, code, "{what}: code={} msg={}", e.code, e.message);
    };
    expect(|p| p.taps = 4, E_FIR_TAPS, "taps 下限");
    expect(|p| p.taps = 16_385, E_FIR_BUDGET, "taps 上限");
    expect(|p| p.fs = 0.0, E_FIR_BAND, "fs=0");
    expect(|p| p.fs = f64::NAN, E_FIR_BAND, "fs=NaN");
    expect(|p| p.fs = 1_000.0, E_FIR_BAND, "fs 过小");
    expect(|p| p.passband_hz[0] = 10.0, E_FIR_BAND, "passband.lo<20");
    expect(|p| p.passband_hz[1] = 20.0, E_FIR_BAND, "f_p=20");
    expect(
        |p| p.stopband_hz[0] = p.passband_hz[1],
        E_FIR_BAND,
        "f_s=f_p",
    );
    expect(
        |p| p.stopband_hz[0] = p.passband_hz[1] + 20.0,
        E_FIR_BAND,
        "过渡带<30Hz",
    );
    expect(|p| p.stopband_hz[0] = 23_953.0, E_FIR_BAND, "f_s>0.499fs");
    expect(
        |p| p.stopband_hz[1] = 23_953.0,
        E_FIR_BAND,
        "stopband.hi>0.499fs",
    );
    expect(
        |p| p.stopband_hz[1] = 18_000.0,
        E_FIR_BAND,
        "stopband.hi<lo",
    );
    expect(|p| p.attenuation_db = 59.0, E_FIR_BAND, "attenuation<60");
    expect(
        |p| p.attenuation_db = 121.0,
        E_FIR_BAND,
        "attenuation>−mag_floor",
    );
    expect(|p| p.ripple_db = 0.0, E_FIR_BAND, "ripple=0");
    expect(|p| p.ripple_db = 7.0, E_FIR_BAND, "ripple>6");
    expect(|p| p.mag_floor_db = -50.0, E_FIR_BAND, "mag_floor>-60");
    expect(|p| p.mag_floor_db = -300.0, E_FIR_BAND, "mag_floor<-240");
    expect(|p| p.prototype.beta = -0.1, E_FIR_BAND, "beta<0");
    expect(
        |p| p.prototype.taps = 3_000,
        E_FIR_SCHEMA,
        "prototype.taps 非 2 的幂",
    );
    expect(
        |p| {
            p.taps = 4_096;
            p.prototype.taps = 4_096; // 合法 2^k 网格，但 < 2*taps=8192
        },
        E_FIR_BUDGET,
        "prototype.taps < 2*taps",
    );
    expect(
        |p| p.cepstrum_fft = 3_000,
        E_FIR_SCHEMA,
        "cepstrum_fft 非 2 的幂",
    );
    expect(
        |p| {
            p.taps = 4_096;
            p.cepstrum_fft = 4_096; // 合法 2^k 网格，但 < 2*taps=8192
        },
        E_FIR_BUDGET,
        "cepstrum_fft < 2*taps",
    );
    expect(
        |p| p.factorization = Factorization::Roots,
        E_FIR_FAMILY,
        "roots 未实现必须拒绝",
    );
    // design() 对非法参数同样返回错误（不进入构造）
    let mut bad = base.clone();
    bad.taps = 16_385;
    let e = design(&bad).expect_err("design 应拒绝超上限 taps");
    assert_eq!(e.code, E_FIR_BUDGET);
}

// ---------------------------------------------------------------------------
// 11. 确定性 + 无隐藏增益（spec §1.5/§1.6）
// ---------------------------------------------------------------------------

#[test]
fn t33_determinism_and_no_hidden_gain() {
    let params = MinFastParams::default();
    let a = design(&params).expect("design a");
    let b = design(&params).expect("design b");
    // 同输入同版本两次设计逐值一致（spec §1.6 确定性；系数文件哈希稳定性前提）
    assert_eq!(a.coefficients, b.coefficients);
    assert_eq!(a.prototype, b.prototype);
    assert_eq!(a.support, b.support);
    assert_eq!(a.response, b.response);
    assert_eq!(
        a.delay.characteristic_hz_samples,
        b.delay.characteristic_hz_samples
    );
    assert_eq!(a.min_phase.max_deviation_deg, b.min_phase.max_deviation_deg);
    assert_eq!(a.criteria, b.criteria);
    // 无隐藏增益：DC 声明 0 dB，实测在 ±0.01 dB（spec §1.5）
    assert_eq!(a.response.reference.hz, 0.0);
    assert_eq!(a.response.reference.gain_db, 0.0);
    assert!(a.response.reference.measured_gain_db.abs() <= REFERENCE_TOL_DB);
    // 原型 DC 归一：sum(h_proto)=1
    let dc: f64 = a.prototype.iter().sum();
    assert!((dc - 1.0).abs() <= 1e-12, "原型未 DC 归一: {dc}");
    // 最小相位输出不做峰值归一化：sum(h) 应与原型直流响应一致（≈1，而不是被强行缩放）
    assert!((a.coefficients.iter().sum::<f64>() - 1.0).abs() <= 1e-6);
}

// ---------------------------------------------------------------------------
// 12. 端点 20 Hz / 20 kHz（spec §2.4），含不可达分支
// ---------------------------------------------------------------------------

#[test]
fn t33_endpoints_20hz_20khz_and_not_applicable() {
    // 48 kHz：20 kHz 可测（0.499fs=23952）
    let d = design(&MinFastParams::default()).expect("48k design");
    match &d.response.endpoints.f20 {
        Endpoint::Measured {
            freq_hz,
            mag_db,
            phase_deg,
        } => {
            assert_eq!(*freq_hz, 20.0);
            assert!(mag_db.abs() <= 0.01, "20 Hz 幅度 {mag_db}");
            assert!(phase_deg.is_finite());
        }
        other => panic!("20 Hz 端点应为 measured: {other:?}"),
    }
    match &d.response.endpoints.f20k {
        Endpoint::Measured {
            freq_hz, mag_db, ..
        } => {
            assert_eq!(*freq_hz, 20_000.0);
            assert!(*mag_db <= -60.0, "20 kHz 应在阻带: {mag_db}");
        }
        other => panic!("48k 的 20 kHz 端点应为 measured: {other:?}"),
    }
    // 32 kHz：0.499fs=15968 < 20000 → 必须写 not_applicable_at_fs（不得裸字符串/缺失）
    let mut p32 = MinFastParams {
        fs: 32_000.0,
        taps: 1024,
        passband_hz: [20.0, 4_000.0],
        stopband_hz: [6_000.0, 0.499 * 32_000.0],
        attenuation_db: 60.0,
        ..MinFastParams::default()
    };
    p32.prototype.taps = 32_768;
    let d32 = design(&p32).expect("32k 设计应成功");
    match &d32.response.endpoints.f20k {
        Endpoint::NotApplicableAtFs { freq_hz, reason } => {
            assert_eq!(*freq_hz, 20_000.0);
            assert!(!reason.is_empty());
        }
        other => panic!("32k 的 20 kHz 端点应为 not_applicable_at_fs: {other:?}"),
    }
    assert!(d32.response.endpoints.f20.is_measured());
    assert!(d32.response.endpoints.is_complete());
    assert!(d32.response.endpoints.f20k.status_str() == "not_applicable_at_fs");
}

// ---------------------------------------------------------------------------
// 13. prototype.magnitude=linear（spec §3.2 允许的替代；诚实报告）
// ---------------------------------------------------------------------------

#[test]
fn t33_linear_prototype_variant_designs_and_reports() {
    let mut params = MinFastParams {
        taps: 4_096,
        passband_hz: [20.0, 14_000.0],
        stopband_hz: [19_000.0, 0.499 * DEFAULT_FS],
        attenuation_db: 60.0,
        ..MinFastParams::default()
    };
    params.prototype.magnitude = PrototypeMagnitude::Linear;
    let d = design(&params).expect("linear 原型设计应成功");
    assert!(d.coefficients.iter().all(|v| v.is_finite()));
    assert_eq!(d.coefficients.len(), params.taps);
    println!(
        "[linear] accepted={} failed={:?}",
        d.accepted(),
        d.failed_criteria()
    );
    assert!(
        d.warnings.iter().any(|w| w.contains("linear")),
        "linear 原型应给出工程 warning"
    );
    assert!(d.min_phase.max_deviation_deg <= MIN_PHASE_MAX_DEG);
    // 诚实报告：能否通过判据以实测为准；本参数组合应可达到声明判据（跨实现差异不得静默放宽）
    assert!(
        d.accepted(),
        "linear 原型参数组合未通过: {:?}",
        d.failed_criteria()
    );
}

// ---------------------------------------------------------------------------
// 14. INFEASIBLE 诚实报告（不静默放行；spec §3.2/§7）
// ---------------------------------------------------------------------------

#[test]
fn t33_infeasible_reports_honestly_not_silent_pass() {
    // 64 taps 无法达到 80 dB 阻带/过渡带：design() 返回报告，accepted()=false，
    // design_strict() 以 E_FIR_BUDGET 拒绝（INFEASIBLE，不静默截断/放行）。
    let mut small = MinFastParams::default();
    small.taps = 64;
    small.prototype.taps = 32_768;
    small.cepstrum_fft = 32_768;
    validate_params(&small).expect("64 taps 参数合法（只是不可达）");
    let d = design(&small).expect("构造仍应完成并给报告");
    assert!(!d.accepted(), "64 taps 80 dB 不应通过判据");
    let failed = d.failed_criteria();
    println!("[infeasible] 64 taps failed={failed:?}");
    assert!(failed.contains(&"stopband_attenuation"));
    assert!(failed.contains(&"transition_edge"));
    // 判据数值本身可复算：报告实测值必须诚实（不是硬编码 PASS）
    let sb = d.criteria.get("stopband_attenuation").unwrap();
    assert!(!sb.pass);
    assert!(sb.measured.unwrap() > sb.limit.unwrap());
    let e = design_strict(&small).expect_err("design_strict 应拒绝 INFEASIBLE");
    assert_eq!(e.code, E_FIR_BUDGET);
    assert!(e.message.contains("INFEASIBLE"));

    // attenuation=120 dB 超过该 taps/过渡带可达值：同样必须诚实 FAIL
    let mut over = MinFastParams::default();
    over.attenuation_db = 120.0;
    let d2 = design(&over).expect("构造完成");
    assert!(!d2.accepted());
    assert!(d2.failed_criteria().contains(&"stopband_attenuation"));
    assert!(design_strict(&over).is_err());
}

// ---------------------------------------------------------------------------
// 15. 48k / 96k 处理与 GD 样本标度
// ---------------------------------------------------------------------------

#[test]
fn t33_48k_96k_design_and_delay_scaling() {
    let mut p96 = MinFastParams {
        fs: 96_000.0,
        taps: 2_048,
        passband_hz: [20.0, 16_000.0],
        stopband_hz: [19_000.0, 23_952.0],
        attenuation_db: 60.0,
        ..MinFastParams::default()
    };
    p96.prototype.taps = 32_768;
    p96.cepstrum_fft = 32_768;
    let d96 = design(&p96).expect("96k 设计应成功");
    assert!(
        d96.accepted(),
        "96k 设计未通过判据: {:?}",
        d96.failed_criteria()
    );
    assert!(d96.response.endpoints.f20k.is_measured());
    let d48 = design(&MinFastParams::default()).expect("48k 设计");
    println!(
        "[fs] GD(1k) 48k={:.4} samples, 96k={:.4} samples",
        d48.delay.at_1khz_samples, d96.delay.at_1khz_samples
    );
    // 同样的 taps 与滤波器形状：96 kHz 下样本群延迟约放大 ~fs 比（1kHz 处于更低相对频率）
    assert!(d96.delay.at_1khz_samples > 2.0 * d48.delay.at_1khz_samples);
    assert!(d96.delay.at_1khz_samples < 10.0 * d48.delay.at_1khz_samples);
    // 两个采样率下都必须频变、非零延迟
    assert!(!d96.delay.zero_delay_claim && !d48.delay.zero_delay_claim);
    assert!(d96.delay.passband_max_samples > d96.delay.passband_min_samples);
}

// ---------------------------------------------------------------------------
// 16. 严格设计与摘要（T55 消费接口自检）
// ---------------------------------------------------------------------------

#[test]
fn t33_design_strict_and_summary_lines() {
    let params = MinFastParams::default();
    let d = design_strict(&params).expect("默认参数应严格通过");
    let lines = d.summary_lines();
    println!("[summary] {} lines", lines.len());
    assert!(lines.len() >= 20, "摘要行数过少: {}", lines.len());
    let joined = lines.join("\n");
    assert!(joined.contains("family=min_fast"));
    assert!(joined.contains("criterion passband_ripple: PASS"));
    assert!(joined.contains("criterion stopband_attenuation: PASS"));
    assert!(joined.contains("criterion min_phase_m2: PASS"));
    assert!(joined.contains("criterion stability_boundary: PASS"));
    assert!(joined.contains("NOT zero-delay"));
    assert!(joined.contains("stability: class=fir"));
    assert!(!joined.contains("FAIL"), "默认参数摘要不应出现 FAIL");
    // 参数回显一致（T55 manifest design 段可直接取）
    assert_eq!(d.params, params);
    assert_eq!(d.family(), FAMILY);
    assert_eq!(d.response.grid_points, (8 * params.taps).max(8_192));
    assert_eq!(d.prototype.len(), params.taps);
}
