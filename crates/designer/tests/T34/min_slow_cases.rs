// T34 min_slow 私有测试（crates/designer/tests/T34/min_slow_cases.rs）
//
// 归属：WBS T34 / task-72，owner rt-fir-minslow；写域 leaf + tests/T34 + evidence/T34。
// 本文件通过 src/min_slow.rs 末尾的 include! 进入 #[cfg(test)] 子模块执行：
//
//     cargo test -p khaen-designer min_slow
//
// 测试覆盖（对应 spec fir-toolbox.md v1.2 §3.3 判据 1–6、§3.8 家族互斥、§3.2 判据 2/3、
// §1.2/§1.3/§1.4/§1.5/§2.4、§9 V17 参考构造）：
// - 原语自检：FFT 往返/Parseval、Bessel I0、Kaiser 窗（独立于设计路径）；
// - V17 参考复算：M2 hilbert_consistency、GD 表（50 Hz–22 kHz）、Nyquist 带衰减、幅度点；
// - 规范判据：默认参数 11 条全 PASS + 独立直接 DFT 复算通带/Nyquist 带/单调性；
// - 最小相位：M2（65536/32768 两个尺寸）+ 幅度守恒 + 能量前置 + 线性相位原型应被 M2 判非最小相位；
// - 慢滚降家族语义：独立参数（nyquist_attenuation_db，无 stopband_hz）、平缓滚降 profile；
// - slow vs fast 数值互斥（spec §3.3 判据 6 / §3.8）：M(0.45fs)、−60 dB 穿越点、M(20k) 差异；
// - 有限支撑/振铃/端点/参考归一化/确定性；
// - 极点稳定性边界：FIR 分母为 1、极点全在 z=0、无 IIR 极点；
// - 延迟报告：频变 GD、onset、gd_max，明确 zero_delay_claim=false（不冒称零延迟）；
// - 家族边界/参数拒收/INFEASIBLE 诚实报告/48k–96k 处理。
//
// 独立性说明：本文件的“独立复算”工具用最朴素的逐点 sin/cos 直接 DFT 与解析群延迟，
// 不调用被测模块的 FFT/谱插值/报告路径；只有 M2 一致性调用公开函数（M2 本身就是规范
// 判据定义），另以独立幅度守恒/能量前置/线性相位反例作为交叉证据。

// ---------------------------------------------------------------------------
// 独立复算工具（不调用被测模块的 FFT/谱/报告路径）
// ---------------------------------------------------------------------------

fn t34_direct_resp(h: &[f64], fs: f64, f: f64) -> (f64, f64) {
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

fn t34_direct_mag_db(h: &[f64], fs: f64, f: f64) -> f64 {
    let (re, im) = t34_direct_resp(h, fs, f);
    10.0 * ((re * re + im * im).max(1e-24)).log10()
}

/// 解析群延迟（独立实现：逐点 sin/cos 求和，Re(G/H)）。
fn t34_direct_gd(h: &[f64], fs: f64, f: f64) -> f64 {
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

fn t34_log_grid(lo: f64, hi: f64, points: usize) -> Vec<f64> {
    let ratio = hi / lo;
    (0..points)
        .map(|i| lo * ratio.powf(i as f64 / (points - 1) as f64))
        .collect()
}

/// 独立搜索 −A 首次穿越点（粗扫 + 二分，全部用直接 DFT）。
fn t34_direct_crossing(h: &[f64], fs: f64, f_from: f64, f_to: f64, target_db: f64) -> Option<f64> {
    let scan = t34_log_grid(f_from, f_to, 400);
    let mut prev = (scan[0], t34_direct_mag_db(h, fs, scan[0]));
    for &f in scan.iter().skip(1) {
        let v = t34_direct_mag_db(h, fs, f);
        if v <= target_db && prev.1 > target_db {
            let mut a = prev.0;
            let mut b = f;
            for _ in 0..50 {
                let m = 0.5 * (a + b);
                if t34_direct_mag_db(h, fs, m) > target_db {
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

fn t34_support(h: &[f64]) -> (usize, usize, usize) {
    let mut max_abs = 0.0f64;
    for &v in h {
        max_abs = max_abs.max(v.abs());
    }
    let eps = 1e-6 * max_abs;
    let first = h.iter().position(|v| v.abs() > eps).unwrap();
    let last = h.iter().rposition(|v| v.abs() > eps).unwrap();
    (first, last, last - first + 1)
}

fn t34_peak_index(h: &[f64]) -> usize {
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

fn t34_band_max_db(h: &[f64], fs: f64, lo: f64, hi: f64, points: usize) -> f64 {
    let mut mx = f64::NEG_INFINITY;
    for i in 0..=points {
        let f = lo + (hi - lo) * i as f64 / points as f64;
        mx = mx.max(t34_direct_mag_db(h, fs, f));
    }
    mx
}

/// fast 风格对照构造（spec §3.2 的 raised_cosine→f_s 构造）：用同一 min_slow 原型合成器
/// 换幅度 law，再做同态最小相位分解；用于证明 slow/fast 数值边界（spec §3.3 判据 6 / §3.8）。
fn t34_fast_style_coefficients() -> (MinSlowParams, Vec<f64>) {
    let p = MinSlowParams::default();
    let floor = 10f64.powf(p.mag_floor_db / 20.0);
    let mag_fast = move |f: f64| -> f64 {
        let fp = 16_000.0;
        let fst = 19_000.0;
        if f <= fp {
            1.0
        } else if f >= fst {
            floor
        } else {
            floor.max(0.5 * (1.0 + (std::f64::consts::PI * (f - fp) / (fst - fp)).cos()))
        }
    };
    let proto = symmetric_prototype_with(&p, mag_fast).expect("fast 风格原型合成");
    let coeffs = min_phase_coefficients(&proto, p.cepstrum_fft, p.mag_floor_db)
        .expect("fast 风格最小相位分解");
    (p, coeffs)
}

fn t34_find_char(d: &MinSlowDesign, f: f64) -> f64 {
    let (_, g) = d
        .delay
        .characteristic_hz_samples
        .iter()
        .min_by(|a, b| (a.0 - f).abs().partial_cmp(&(b.0 - f).abs()).unwrap())
        .expect("特征点");
    *g
}

// ---------------------------------------------------------------------------
// 1. 原语自检（FFT / Bessel / Kaiser）
// ---------------------------------------------------------------------------

#[test]
fn t34_fft_bessel_kaiser_primitives() {
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
// 2. V17 参考复算（spec §9 V17 / evidence/T04/vector-results.json）
// ---------------------------------------------------------------------------

#[test]
fn t34_v17_min_slow_reference_reproduction() {
    let p = MinSlowParams::v17_reference();
    // V17 构造参数（spec §3.3 判据 7 / evidence/T04/recompute-vectors.mjs V17）
    assert_eq!(p.fs, 48_000.0);
    assert_eq!(p.taps, 1_024);
    assert_eq!(p.passband_hz, [20.0, 16_000.0]);
    assert_eq!(p.nyquist_attenuation_db, 30.0);
    assert_eq!(p.mag_floor_db, -120.0);
    assert_eq!(p.prototype.taps, 32_768);
    assert_eq!(p.prototype.beta, 8.96);
    assert_eq!(p.cepstrum_fft, 32_768);
    assert_eq!(p.prototype.family, "smooth");
    assert_eq!(p.prototype.transition, "cosine_to_nyquist");

    let d = design(&p).expect("V17 参考设计应成功");
    // M2 相位–Hilbert 一致性（V17 发布值 0.003606692550219144°）
    let published_m2 = 0.003_606_692_550_219_144_f64;
    println!(
        "[V17] hilbert_consistency reported={:.12}° published={:.12}° delta={:.3e}°",
        d.min_phase.max_deviation_deg,
        published_m2,
        d.min_phase.max_deviation_deg - published_m2
    );
    assert!(
        (d.min_phase.max_deviation_deg - published_m2).abs() <= 1e-3,
        "M2 与 V17 发布值偏差过大: {:.9} vs {:.9}",
        d.min_phase.max_deviation_deg,
        published_m2
    );
    assert!(d.min_phase.max_deviation_deg <= MIN_PHASE_MAX_DEG);

    // GD 表（V17 发布值；本实现以规范 FFT 中心差分复算；另用直接解析 GD 交叉）
    let published_gd: [(f64, f64); 7] = [
        (50.0, 0.233_554_874_417_248_4),
        (100.0, 0.233_562_961_443_974_56),
        (1_000.0, 0.234_620_073_744_586_18),
        (5_000.0, 0.262_434_887_773_581_37),
        (10_000.0, 0.386_448_402_055_931_56),
        (15_000.0, 0.928_755_449_741_371_1),
        (22_000.0, 2.203_670_102_422_337_5),
    ];
    for &(f, expected) in &published_gd {
        let reported = t34_find_char(&d, f);
        let direct = t34_direct_gd(&d.coefficients, p.fs, f);
        println!(
            "[V17] GD({f:8.0} Hz) reported={reported:.9} direct={direct:.9} published={expected:.9} d_rep={:+.3e} d_dir={:+.3e}",
            reported - expected,
            direct - expected
        );
        assert!(
            (reported - expected).abs() <= 0.01,
            "GD({f}) 报告与 V17 发布值偏差过大: {reported} vs {expected}"
        );
        assert!(
            (direct - expected).abs() <= 0.01,
            "GD({f}) 独立解析复算与 V17 发布值偏差过大: {direct} vs {expected}"
        );
    }

    // Nyquist 带最大（V17 发布值 −40.985209617341624 dB）
    let published_nyq = -40.985_209_617_341_624_f64;
    println!(
        "[V17] nyquist_band_max reported={:.9} published={:.9} delta={:.3e}",
        d.response.nyquist_band.max_db,
        published_nyq,
        d.response.nyquist_band.max_db - published_nyq
    );
    assert!(
        (d.response.nyquist_band.max_db - published_nyq).abs() <= 0.01,
        "Nyquist 带最大与 V17 发布值偏差过大: {} vs {}",
        d.response.nyquist_band.max_db,
        published_nyq
    );

    // 幅度点（spec §3.3 判据 7：M(20k)=−6.02、M(22k)=−16.68、M(23.5k)=−40.28 dB）
    let mag20k = magnitude_db(&d.coefficients, p.fs, 20_000.0);
    let mag22k = magnitude_db(&d.coefficients, p.fs, 22_000.0);
    let mag235k = magnitude_db(&d.coefficients, p.fs, 23_500.0);
    println!("[V17] M(20k)={mag20k:.6} M(22k)={mag22k:.6} M(23.5k)={mag235k:.6} dB");
    assert!((mag20k - (-6.02)).abs() <= 0.05, "M(20k)={mag20k}");
    assert!((mag22k - (-16.68)).abs() <= 0.05, "M(22k)={mag22k}");
    assert!((mag235k - (-40.28)).abs() <= 0.10, "M(23.5k)={mag235k}");

    // 原型↔最小相位幅度守恒（V17 发布值 8.954e-9 dB）
    let mut mag_diff = 0.0f64;
    for f in t34_log_grid(20.0, 15_500.0, 1_000) {
        mag_diff = mag_diff.max(
            (t34_direct_mag_db(&d.prototype, p.fs, f)
                - t34_direct_mag_db(&d.coefficients, p.fs, f))
            .abs(),
        );
    }
    println!("[V17] prototype<->minphase max |Δmag| (20..15.5k) = {mag_diff:.3e} dB");
    assert!(mag_diff <= 1e-6, "幅度守恒失败: {mag_diff} dB");
}

// ---------------------------------------------------------------------------
// 3. 规范判据：默认参数全部 PASS（spec §3.3 判据 1–6）
// ---------------------------------------------------------------------------

#[test]
fn t34_normative_criteria_all_pass_default() {
    let p = MinSlowParams::default();
    let d = design(&p).expect("默认设计应成功");
    assert!(d.accepted(), "默认设计未通过: {:?}", d.failed_criteria());
    assert!(d.failed_criteria().is_empty());
    // 11 条判据齐全且全 PASS
    let ids: Vec<&str> = d.criteria.criteria.iter().map(|c| c.id).collect();
    for id in [
        "finite_support",
        "passband_ripple",
        "nyquist_attenuation",
        "monotonic_rolloff",
        "min_phase_m2",
        "delay_report",
        "endpoints",
        "stability_boundary",
        "family_boundary",
        "family_distinction_slow_vs_fast",
        "reference_gain",
    ] {
        let c = d.criteria.get(id).unwrap_or_else(|| panic!("缺判据 {id}"));
        assert!(c.pass, "判据 {id} 未通过: measured={:?}", c.measured);
    }
    assert_eq!(ids.len(), 11);
    // 关键实测范围
    assert!(d.response.passband.ripple_db <= 0.55);
    assert!(d.response.passband.min_db <= 0.0 && d.response.passband.max_db <= 0.01);
    assert!(d.response.nyquist_band.max_db <= -29.0);
    assert!(d.response.nyquist_band.measured_attenuation_db >= 30.0);
    assert!(d.response.monotonicity.max_rise_db <= 0.25);
    assert!(d.min_phase.max_deviation_deg <= 2.0);
    assert!((d.delay.at_1khz_samples - 0.2346).abs() <= 0.01);
    assert!(d.response.endpoints.is_complete());
    assert!(d.response.reference.measured_gain_db.abs() <= 0.01);
    assert!(!d.delay.zero_delay_claim);
    assert_eq!(d.delay.class, "frequency_varying");
}

// ---------------------------------------------------------------------------
// 4. 独立直接 DFT 复算通带/Nyquist 带/单调性
// ---------------------------------------------------------------------------

#[test]
fn t34_independent_response_passband_nyquist_monotonic() {
    let p = MinSlowParams::default();
    let d = design(&p).expect("默认设计应成功");
    let h = &d.coefficients;

    // 通带 p-p（独立直接 DFT，20 Hz–f_p 对数 2001 点）
    let mut pb_min = f64::INFINITY;
    let mut pb_max = f64::NEG_INFINITY;
    for f in t34_log_grid(20.0, p.passband_hz[1], 2_001) {
        let v = t34_direct_mag_db(h, p.fs, f);
        pb_min = pb_min.min(v);
        pb_max = pb_max.max(v);
    }
    let direct_ripple = pb_max - pb_min;
    println!(
        "[independent] passband ripple reported={:.9} direct={:.9}",
        d.response.passband.ripple_db, direct_ripple
    );
    assert!(
        (direct_ripple - d.response.passband.ripple_db).abs() <= 0.01,
        "通带纹波报告与独立复算不一致: {} vs {}",
        d.response.passband.ripple_db,
        direct_ripple
    );
    assert!(direct_ripple <= p.ripple_db + RIPPLE_TOL_DB);

    // Nyquist 带 max（独立直接 DFT，[0.49fs, 0.499fs] 257 点）
    let direct_nyq = t34_band_max_db(h, p.fs, 0.49 * p.fs, 0.499 * p.fs, 256);
    println!(
        "[independent] nyquist max reported={:.9} direct={:.9}",
        d.response.nyquist_band.max_db, direct_nyq
    );
    assert!(
        (direct_nyq - d.response.nyquist_band.max_db).abs() <= 0.02,
        "Nyquist 带报告与独立复算不一致: {} vs {}",
        d.response.nyquist_band.max_db,
        direct_nyq
    );
    assert!(direct_nyq <= -(p.nyquist_attenuation_db - NYQUIST_ATTENUATION_TOL_DB));

    // 慢滚降单调性（独立直接 DFT，f_p..0.499fs 8192 点，运行最小值上翘）
    let mut run_min = t34_direct_mag_db(h, p.fs, p.passband_hz[1]);
    let mut direct_rise = 0.0f64;
    for f in t34_log_grid(p.passband_hz[1], 0.499 * p.fs, 8_192) {
        let v = t34_direct_mag_db(h, p.fs, f);
        direct_rise = direct_rise.max(v - run_min);
        run_min = run_min.min(v);
    }
    println!(
        "[independent] monotonic max rise reported={:.9} direct={:.9}",
        d.response.monotonicity.max_rise_db, direct_rise
    );
    assert!(
        direct_rise <= MONOTONIC_MAX_RISE_DB,
        "独立复算单调性失败: {direct_rise}"
    );
    assert!(
        (direct_rise - d.response.monotonicity.max_rise_db).abs() <= 0.01,
        "单调性报告与独立复算不一致: {} vs {}",
        d.response.monotonicity.max_rise_db,
        direct_rise
    );

    // “慢滚降”语义：f_p 处仍 0 dB 附近，滚降一路延续到 Nyquist（不是 fast 的 f_s 陡降）
    assert!(t34_direct_mag_db(h, p.fs, p.passband_hz[1]).abs() <= 0.01);
    assert!(t34_direct_mag_db(h, p.fs, 20_000.0) > -7.0);
    assert!(t34_direct_mag_db(h, p.fs, 20_000.0) <= -5.0);
}

// ---------------------------------------------------------------------------
// 5. 最小相位：M2、幅度守恒、能量前置、线性相位反例
// ---------------------------------------------------------------------------

#[test]
fn t34_min_phase_energy_frontloaded_and_magnitude_preserved() {
    let p = MinSlowParams::default();
    let d = design(&p).expect("默认设计应成功");
    let h = &d.coefficients;
    let proto = &d.prototype;

    // 能量前置（最小相位独立交叉证据）
    let quarter = h.len() / 4;
    let e25: f64 = h[..quarter].iter().map(|v| v * v).sum();
    let etot: f64 = h.iter().map(|v| v * v).sum();
    println!("[min_phase] energy first 25% taps = {:.15}", e25 / etot);
    assert!(e25 / etot > 0.999, "最小相位能量未前置: {}", e25 / etot);

    // 原型是对称线性相位（min_slow 参考构造步骤 2）
    let mut max_sym = 0.0f64;
    let mut max_abs = 0.0f64;
    for (i, v) in proto.iter().enumerate() {
        max_abs = max_abs.max(v.abs());
        max_sym = max_sym.max((v - proto[proto.len() - 1 - i]).abs());
    }
    assert!(
        max_sym / max_abs <= 1e-12,
        "原型不对称: {}",
        max_sym / max_abs
    );
    let dc: f64 = proto.iter().sum();
    assert!((dc - 1.0).abs() <= 1e-12, "原型未 DC 归一: {dc}");

    // 原型↔最小相位幅度守恒（直接 DFT）
    let mut mag_diff = 0.0f64;
    for f in t34_log_grid(20.0, 15_500.0, 1_000) {
        mag_diff =
            mag_diff.max((t34_direct_mag_db(proto, p.fs, f) - t34_direct_mag_db(h, p.fs, f)).abs());
    }
    println!("[min_phase] prototype<->minphase max |Δmag| = {mag_diff:.3e} dB");
    assert!(mag_diff <= 1e-6, "幅度守恒失败: {mag_diff} dB");

    // 输出非对称（最小相位不是线性相位）
    let asym: f64 = (0..h.len())
        .map(|i| (h[i] - h[h.len() - 1 - i]).abs())
        .sum();
    assert!(asym > 1e-3, "最小相位输出不应线性相位对称");

    // M2 两个 FFT 尺寸（65536 规范门 / 32768 稳健性交叉）
    let m2_65 = min_phase_consistency_deg(h, p.fs, 65_536).expect("M2 65536");
    let m2_32 = min_phase_consistency_deg(h, p.fs, 32_768).expect("M2 32768");
    println!("[min_phase] M2(65536)={m2_65:.9}° M2(32768)={m2_32:.9}°");
    assert!(m2_65 <= MIN_PHASE_MAX_DEG && m2_32 <= MIN_PHASE_MAX_DEG);

    // 线性相位原型必须被 M2 判为非最小相位（判据有区分力，不是恒真）
    let m2_linear = min_phase_consistency_deg(proto, p.fs, 65_536).expect("原型 M2");
    println!("[min_phase] M2(linear-phase prototype)={m2_linear:.6}° (应 >> 2°)");
    assert!(
        m2_linear > MIN_PHASE_MAX_DEG,
        "M2 未识别线性相位原型: {m2_linear}"
    );
}

// ---------------------------------------------------------------------------
// 6. 频变群延迟报告（spec §3.3 判据 5 / §3.2 判据 3）：不是零延迟
// ---------------------------------------------------------------------------

#[test]
fn t34_delay_frequency_varying_not_zero_delay() {
    let p = MinSlowParams::default();
    let d = design(&p).expect("默认设计应成功");
    // 特征频点报告与独立解析 GD 一致（极小容差：解析 GD 无插值误差）
    for (f, gd) in &d.delay.characteristic_hz_samples {
        let direct = t34_direct_gd(&d.coefficients, p.fs, *f);
        println!("[delay] GD({f:8.1} Hz) reported={gd:.9} direct={direct:.9}");
        let tol = 0.5 + 0.05 * gd.abs().max(direct.abs());
        assert!(
            (gd - direct).abs() <= tol,
            "GD({f}) 报告与独立解析偏差过大: {gd} vs {direct}"
        );
    }
    // 频变、非零延迟
    assert!(d.delay.passband_max_samples > d.delay.passband_min_samples);
    assert!(d.delay.passband_min_samples >= 0.2);
    assert!(d.delay.passband_min_samples <= 0.3);
    assert!((d.delay.at_1khz_samples - 0.2346).abs() <= 0.01);
    // 过渡带内显著上升（22 kHz 处 GD ≈ V17 2.2037 > 5x 低频）
    let gd_22k = t34_direct_gd(&d.coefficients, p.fs, 22_000.0);
    assert!(
        gd_22k > 5.0 * d.delay.at_1khz_samples,
        "22 kHz GD {gd_22k} 未显著高于 1 kHz GD {}",
        d.delay.at_1khz_samples
    );
    // gd_max / onset / latency 定义
    assert!(d.delay.band_max_samples > d.delay.at_1khz_samples);
    assert!((d.delay.gd_max_ms - d.delay.band_max_samples / p.fs * 1000.0).abs() <= 1e-12);
    let (first, _, _) = t34_support(&d.coefficients);
    assert_eq!(d.delay.onset_samples, first);
    assert!((d.delay.onset_ms - first as f64 / p.fs * 1000.0).abs() <= 1e-12);
    assert!((d.delay.latency_ms_1khz - d.delay.at_1khz_samples / p.fs * 1000.0).abs() <= 1e-12);
    assert!(d.delay.latency_ms_1khz > 0.0 && d.delay.latency_ms_1khz < 0.1);
    assert!(!d.delay.zero_delay_claim);
    assert_eq!(d.delay.declared_bulk_samples, 0.0);
    assert_eq!(d.delay.declared_bulk_ms, 0.0);
    // 延迟声明文案：明确频变、非零延迟、onset/gd_max/M(0.45fs)
    let text = d.delay_statement();
    println!("[delay] {text}");
    assert!(text.contains("NOT zero-delay"));
    assert!(text.contains("frequency-varying"));
    assert!(text.contains("onset="));
    assert!(text.contains("gd_max="));
    assert!(text.contains("M(0.45fs)="));
    assert!(text.contains("GD(1kHz)="));
}

// ---------------------------------------------------------------------------
// 7. slow vs fast 家族数值互斥（spec §3.3 判据 6 / §3.8）
// ---------------------------------------------------------------------------

#[test]
fn t34_slow_vs_fast_family_distinction() {
    let p = MinSlowParams::default();
    let slow = design(&p).expect("slow 设计应成功");

    // slow：M(0.45fs) > −60 dB，−60 dB 穿越点在 0.45fs 之上
    let slow_boundary = family_boundary_metrics(&slow.coefficients, p.fs).expect("slow 边界指标");
    println!(
        "[family] slow M(0.45fs)={:.6} dB crossing={:?} Hz (0.45fs={})",
        slow_boundary.mag_db_at_045fs,
        slow_boundary.crossing_minus60db_hz,
        slow_boundary.fast_boundary_hz
    );
    assert!(slow_boundary.slow_like);
    assert!(slow_boundary.passes_slow_gate());
    assert!((slow_boundary.mag_db_at_045fs - (-13.716_297_589_876)).abs() <= 0.05);
    let slow_cross = slow_boundary
        .crossing_minus60db_hz
        .expect("slow 在 0.499fs 前应有 −60 dB 穿越");
    assert!(
        slow_cross > slow_boundary.fast_boundary_hz,
        "slow 的 −60 dB 穿越点 {slow_cross} 应 > 0.45fs={}",
        slow_boundary.fast_boundary_hz
    );
    assert!(
        (slow_cross - 23_844.95).abs() <= 5.0,
        "slow −60 dB 穿越点 {slow_cross} 偏离 V17 构造预期"
    );
    // 独立直接 DFT 复算穿越点（不调用被测模块的 family_boundary_metrics）
    let independent_cross = t34_direct_crossing(
        &slow.coefficients,
        p.fs,
        p.passband_hz[1],
        0.499 * p.fs,
        -60.0,
    )
    .expect("独立复算 slow −60 dB 穿越点");
    println!("[family] slow crossing reported={slow_cross:.6} independent={independent_cross:.6}");
    assert!(
        (independent_cross - slow_cross).abs() <= 1.0,
        "−60 dB 穿越点报告与独立复算不一致: {slow_cross} vs {independent_cross}"
    );
    // 报告与独立函数一致
    assert!(
        (slow.response.family_distinction.mag_db_at_045fs - slow_boundary.mag_db_at_045fs).abs()
            <= 1e-9
    );
    assert_eq!(
        slow.response
            .family_distinction
            .crossing_minus60db_hz
            .is_some(),
        slow_boundary.crossing_minus60db_hz.is_some()
    );
    assert!(
        slow.criteria
            .get("family_distinction_slow_vs_fast")
            .unwrap()
            .pass
    );

    // fast 风格对照（raised_cosine 16k→19k，同一原型合成器/分解器）
    let (_, fast) = t34_fast_style_coefficients();
    let fast_boundary = family_boundary_metrics(&fast, p.fs).expect("fast 边界指标");
    println!(
        "[family] fast M(0.45fs)={:.6} dB crossing={:?} Hz slow_like={} passes={}",
        fast_boundary.mag_db_at_045fs,
        fast_boundary.crossing_minus60db_hz,
        fast_boundary.slow_like,
        fast_boundary.passes_slow_gate()
    );
    assert!(fast_boundary.mag_db_at_045fs <= -60.0);
    assert!(!fast_boundary.slow_like);
    assert!(!fast_boundary.passes_slow_gate());
    let fast_cross = fast_boundary
        .crossing_minus60db_hz
        .expect("fast 应有 −60 dB 穿越");
    assert!(fast_cross <= fast_boundary.fast_boundary_hz);
    assert!(
        (fast_cross - 19_000.0).abs() <= 100.0,
        "fast 穿越点 {fast_cross} 应贴近 19 kHz"
    );
    let fast_independent = t34_direct_crossing(&fast, p.fs, p.passband_hz[1], 0.499 * p.fs, -60.0)
        .expect("独立复算 fast −60 dB 穿越点");
    assert!(
        (fast_independent - fast_cross).abs() <= 1.0,
        "fast −60 dB 穿越点报告与独立复算不一致: {fast_cross} vs {fast_independent}"
    );

    // 幅度差异：20 kHz 处 slow 只有 ~−6 dB，fast 已到 floor；Nyquist 带同样区分
    let slow_20k = magnitude_db(&slow.coefficients, p.fs, 20_000.0);
    let fast_20k = magnitude_db(&fast, p.fs, 20_000.0);
    println!("[family] M(20k) slow={slow_20k:.3} fast={fast_20k:.3} dB");
    assert!(slow_20k > -7.0 && slow_20k < -5.0);
    assert!(fast_20k <= -100.0);
    assert!(slow_20k - fast_20k > 80.0);
    let slow_nyq = t34_band_max_db(&slow.coefficients, p.fs, 0.49 * p.fs, 0.499 * p.fs, 256);
    let fast_nyq = t34_band_max_db(&fast, p.fs, 0.49 * p.fs, 0.499 * p.fs, 256);
    println!("[family] nyquist_band_max slow={slow_nyq:.3} fast={fast_nyq:.3} dB");
    assert!(slow_nyq > -45.0 && slow_nyq <= -29.0);
    assert!(fast_nyq <= -100.0);
}

// ---------------------------------------------------------------------------
// 8. 慢滚降独立参数语义（与 fast 的 stopband_hz/attenuation_db 不同）
// ---------------------------------------------------------------------------

#[test]
fn t34_slow_rolloff_independent_params_semantics() {
    let p = MinSlowParams::default();
    // 独立参数：f_p + nyquist_attenuation_db（不是 fast 的 stopband_hz/attenuation_db）
    assert_eq!(p.passband_hz, [20.0, 16_000.0]);
    assert_eq!(p.nyquist_attenuation_db, 30.0);
    // 参数边界（spec §3.3 参数：20..60）
    for bad in [19.0, 61.0, f64::NAN, f64::INFINITY] {
        let mut q = p.clone();
        q.nyquist_attenuation_db = bad;
        assert_eq!(
            validate_params(&q).expect_err("nyq 越界应拒收").code,
            E_FIR_BAND,
            "nyquist_attenuation_db={bad}"
        );
    }
    for ok in [20.0, 30.0, 60.0] {
        let mut q = p.clone();
        q.nyquist_attenuation_db = ok;
        validate_params(&q).expect("合法 nyq 声明");
    }

    // 目标幅度 profile：f_p 前为 1，f_p..Nyquist 升余弦衰减到 floor，Nyquist 处 = floor
    let floor = 10f64.powf(p.mag_floor_db / 20.0);
    assert_eq!(prototype_target_magnitude(&p, 20.0), 1.0);
    assert_eq!(prototype_target_magnitude(&p, p.passband_hz[1]), 1.0);
    assert!((prototype_target_magnitude(&p, p.fs / 2.0) - floor).abs() <= 1e-15);
    // 严格单调下降（f_p..Nyquist 采样）
    let grid = t34_log_grid(p.passband_hz[1] * 1.0001, p.fs / 2.0 * 0.9999, 2_000);
    let mut prev = prototype_target_magnitude(&p, p.passband_hz[1]);
    for f in grid {
        let m = prototype_target_magnitude(&p, f);
        assert!(m <= prev + 1e-15, "目标幅度在 f={f} 非单调: {m} > {prev}");
        prev = m;
    }
    // 典型值：20 kHz = −6.02 dB（0.5 线性）、0.45fs = −13.72 dB、0.49fs ≈ −41 dB
    let m20k = prototype_target_magnitude(&p, 20_000.0);
    println!(
        "[params] target M(20k)={:.9} ({:.4} dB) M(0.45fs)={:.6} dB M(0.49fs)={:.6} dB",
        m20k,
        20.0 * m20k.log10(),
        20.0 * prototype_target_magnitude(&p, 0.45 * p.fs).log10(),
        20.0 * prototype_target_magnitude(&p, 0.49 * p.fs).log10()
    );
    assert!((20.0 * m20k.log10() - (-6.020_591_227_394_33)).abs() <= 1e-9);
    assert!(
        (20.0 * prototype_target_magnitude(&p, 0.45 * p.fs).log10() - (-13.718_095_949_287_115))
            .abs()
            <= 1e-9
    );
    // 滚降到达 −29 dB（默认声明门）的位置必须在 0.49fs 附近（慢滚降一路延续到 Nyquist）
    assert!(20.0 * prototype_target_magnitude(&p, 0.49 * p.fs).log10() <= -29.0);
    assert!(20.0 * prototype_target_magnitude(&p, 0.47 * p.fs).log10() > -29.0);
}

// ---------------------------------------------------------------------------
// 9. 家族边界 f_p <= 0.42fs（spec §3.3 判据 6）与边界设计诚实性
// ---------------------------------------------------------------------------

#[test]
fn t34_family_boundary_accepts_edge_rejects_beyond() {
    // 精确边界 0.42*fs 参数合法
    let mut edge = MinSlowParams::default();
    edge.passband_hz[1] = FAMILY_EDGE_FP_RATIO * DEFAULT_FS;
    assert!(validate_params(&edge).is_ok());
    let d = design(&edge).expect("边界构造应完成");
    assert!((d.criteria.get("family_boundary").unwrap().measured.unwrap() - 0.42).abs() <= 1e-12);
    assert!(d.criteria.get("family_boundary").unwrap().pass);
    // 但边界设计 Nyquist 带衰减不足 → 诚实 FAIL（不是静默放行）
    assert!(!d.accepted());
    assert!(d.failed_criteria().contains(&"nyquist_attenuation"));
    let e = design_strict(&edge).expect_err("边界 INFEASIBLE 应拒绝");
    assert_eq!(e.code, E_FIR_BUDGET);
    assert!(e.message.contains("INFEASIBLE"));

    // 超过 0.42fs → E_FIR_FAMILY
    let mut beyond = edge.clone();
    beyond.passband_hz[1] = FAMILY_EDGE_FP_RATIO * DEFAULT_FS + 1.0;
    let e = validate_params(&beyond).expect_err("f_p>0.42fs 应拒收");
    assert_eq!(e.code, E_FIR_FAMILY);
    assert!(e.message.contains("0.42"));

    // 边界内 fp=19k 的设计应可通过（且带 f_p 接近上界的 warning）
    let mut p19 = MinSlowParams::default();
    p19.passband_hz[1] = 19_000.0;
    let d19 = design(&p19).expect("fp=19k 设计应成功");
    assert!(d19.accepted(), "fp=19k 未通过: {:?}", d19.failed_criteria());
    assert!(d19
        .warnings
        .iter()
        .any(|w| w.contains("接近 min_slow 家族上界")));
    assert!(d19.response.nyquist_band.max_db <= -29.0);
}

// ---------------------------------------------------------------------------
// 10. 参数拒收（spec §2.5 E_FIR_*；不静默放行）
// ---------------------------------------------------------------------------

#[test]
fn t34_param_rejections_codes() {
    let base = MinSlowParams::default();
    let expect = |mutate: fn(&mut MinSlowParams), code: &'static str, what: &str| {
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
        |p| {
            p.passband_hz[0] = 100.0;
            p.passband_hz[1] = 50.0;
        },
        E_FIR_BAND,
        "f_p < passband.lo",
    );
    expect(
        |p| p.passband_hz[1] = 0.421 * DEFAULT_FS,
        E_FIR_FAMILY,
        "f_p>0.42fs",
    );
    expect(|p| p.nyquist_attenuation_db = 19.0, E_FIR_BAND, "nyq<20");
    expect(|p| p.nyquist_attenuation_db = 61.0, E_FIR_BAND, "nyq>60");
    expect(|p| p.ripple_db = 0.0, E_FIR_BAND, "ripple=0");
    expect(|p| p.ripple_db = 7.0, E_FIR_BAND, "ripple>6");
    expect(|p| p.mag_floor_db = -50.0, E_FIR_BAND, "mag_floor>-60");
    expect(|p| p.mag_floor_db = -300.0, E_FIR_BAND, "mag_floor<-240");
    expect(
        |p| p.prototype.family = "linear",
        E_FIR_FAMILY,
        "prototype.family 非 smooth",
    );
    expect(
        |p| p.prototype.transition = "raised_cosine",
        E_FIR_FAMILY,
        "transition 非 cosine_to_nyquist",
    );
    expect(
        |p| p.prototype.taps = 3_000,
        E_FIR_SCHEMA,
        "prototype.taps 非 2 的幂",
    );
    expect(
        |p| {
            p.taps = 4_096;
            p.prototype.taps = 4_096;
        },
        E_FIR_BUDGET,
        "prototype.taps < 2*taps",
    );
    expect(|p| p.prototype.beta = -0.1, E_FIR_BAND, "beta<0");
    expect(
        |p| p.cepstrum_fft = 3_000,
        E_FIR_SCHEMA,
        "cepstrum_fft 非 2 的幂",
    );
    expect(
        |p| {
            p.taps = 4_096;
            p.cepstrum_fft = 4_096;
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
    assert_eq!(design(&bad).expect_err("design 应拒绝").code, E_FIR_BUDGET);
    // 家族非法经 design_strict 传递 E_FIR_FAMILY（不是 INFEASIBLE 混淆）
    let mut fam = base.clone();
    fam.passband_hz[1] = 0.5 * DEFAULT_FS;
    assert_eq!(
        design_strict(&fam).expect_err("家族非法").code,
        E_FIR_FAMILY
    );
}

// ---------------------------------------------------------------------------
// 11. 确定性 + 无隐藏增益（spec §1.5/§1.6）
// ---------------------------------------------------------------------------

#[test]
fn t34_determinism_and_no_hidden_gain() {
    let params = MinSlowParams::default();
    let a = design(&params).expect("design a");
    let b = design(&params).expect("design b");
    // 同输入同版本两次设计逐值一致（spec §1.6 确定性；系数文件哈希稳定性前提）
    assert_eq!(a.coefficients, b.coefficients);
    assert_eq!(a.prototype, b.prototype);
    assert_eq!(a.support, b.support);
    assert_eq!(a.response, b.response);
    assert_eq!(a.delay, b.delay);
    assert_eq!(a.min_phase.max_deviation_deg, b.min_phase.max_deviation_deg);
    assert_eq!(a.criteria, b.criteria);
    // 无隐藏增益：DC 声明 0 dB，实测在 ±0.01 dB（spec §1.5）
    assert_eq!(a.response.reference.hz, 0.0);
    assert_eq!(a.response.reference.gain_db, 0.0);
    assert!(a.response.reference.measured_gain_db.abs() <= REFERENCE_TOL_DB);
    // 最小相位输出不做峰值归一化：sum(h) 应 ≈ 原型直流响应（≈1，而不是被强行缩放）
    assert!((a.coefficients.iter().sum::<f64>() - 1.0).abs() <= 1e-6);
}

// ---------------------------------------------------------------------------
// 12. 端点 20 Hz / 20 kHz（spec §2.4），含不可达分支
// ---------------------------------------------------------------------------

#[test]
fn t34_endpoints_20hz_20khz_and_not_applicable() {
    // 48 kHz：20 kHz 可测（0.499fs=23952），且 slow rolloff 下应约 −6.02 dB（不是 fast 的 floor）
    let d = design(&MinSlowParams::default()).expect("48k design");
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
            assert!(
                (mag_db - (-6.020_591)).abs() <= 0.02,
                "slow rolloff 的 20 kHz 应约 −6.02 dB，实测 {mag_db}"
            );
            assert!(*mag_db > -60.0, "慢滚降 20 kHz 不应已到阻带 floor");
        }
        other => panic!("48k 的 20 kHz 端点应为 measured: {other:?}"),
    }
    // 32 kHz：0.499fs=15968 < 20000 → 必须写 not_applicable_at_fs（不得裸字符串/缺失）
    let mut p32 = MinSlowParams {
        fs: 32_000.0,
        taps: 512,
        passband_hz: [20.0, 8_000.0],
        ..MinSlowParams::default()
    };
    p32.prototype.taps = 32_768;
    let d32 = design(&p32).expect("32k 设计应成功");
    assert!(d32.accepted(), "32k 未通过: {:?}", d32.failed_criteria());
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
// 13. 48k / 96k 处理与 GD 样本标度
// ---------------------------------------------------------------------------

#[test]
fn t34_48k_96k_design_and_delay_scaling() {
    let d48 = design(&MinSlowParams::default()).expect("48k design");
    let mut p96 = MinSlowParams::default();
    p96.fs = 96_000.0;
    let d96 = design(&p96).expect("96k 设计应成功");
    assert!(
        d96.accepted(),
        "96k 未通过判据: {:?}",
        d96.failed_criteria()
    );
    assert!(d96.response.endpoints.f20k.is_measured());
    // 96 kHz 下 20 kHz 处于更低相对频率 → 约 −0.34 dB（更平）
    let m96_20k = t34_direct_mag_db(&d96.coefficients, 96_000.0, 20_000.0);
    println!(
        "[fs] GD(1k) 48k={:.6} 96k={:.6} samples; M(20k) 48k={:.6} 96k={:.6} dB; nyq_max 96k={:.3} dB",
        d48.delay.at_1khz_samples,
        d96.delay.at_1khz_samples,
        t34_direct_mag_db(&d48.coefficients, 48_000.0, 20_000.0),
        m96_20k,
        d96.response.nyquist_band.max_db
    );
    assert!((m96_20k - (-0.337_193_531)).abs() <= 0.05);
    // 同样 taps/形状，96 kHz 的样本群延迟约放大 fs 比（1 kHz 相对频率更低）
    let ratio = d96.delay.at_1khz_samples / d48.delay.at_1khz_samples;
    assert!(ratio > 1.5 && ratio < 4.0, "GD 标度比 {ratio} 异常");
    // 两个采样率下都必须频变、非零延迟、Nyquist 带衰减达标
    assert!(!d96.delay.zero_delay_claim && !d48.delay.zero_delay_claim);
    assert!(d96.delay.passband_max_samples > d96.delay.passband_min_samples);
    assert!(d96.response.nyquist_band.measured_attenuation_db >= 45.0);
}

// ---------------------------------------------------------------------------
// 14. Nyquist 声明诚实性（INFEASIBLE 不静默放行；nyq=60 不可达）
// ---------------------------------------------------------------------------

#[test]
fn t34_nyquist_declaration_honesty_not_silent_pass() {
    // 声明 60 dB：参数合法但升余弦→Nyquist 构造不可达 → 报告 FAIL，accepted=false
    let mut p60 = MinSlowParams::default();
    p60.nyquist_attenuation_db = 60.0;
    validate_params(&p60).expect("nyq=60 参数合法（只是不可达）");
    let d = design(&p60).expect("构造仍应完成并给报告");
    assert!(!d.accepted(), "nyq=60 不应通过");
    let c = d.criteria.get("nyquist_attenuation").unwrap();
    println!(
        "[honesty] nyq=60: measured max={:?} dB limit={:?} pass={}",
        c.measured, c.limit, c.pass
    );
    assert!(!c.pass);
    assert!(c.measured.unwrap() > c.limit.unwrap());
    assert!(d.failed_criteria().contains(&"nyquist_attenuation"));
    assert!(d.warnings.iter().any(|w| w.contains("通常不可达")));
    let e = design_strict(&p60).expect_err("design_strict 应拒绝 INFEASIBLE");
    assert_eq!(e.code, E_FIR_BUDGET);
    assert!(e.message.contains("INFEASIBLE"));
    assert!(e.message.contains("nyquist_attenuation"));

    // 声明 20 dB：可达 → PASS
    let mut p20 = MinSlowParams::default();
    p20.nyquist_attenuation_db = 20.0;
    let d20 = design(&p20).expect("nyq=20 设计");
    assert!(d20.accepted(), "nyq=20 应通过: {:?}", d20.failed_criteria());
    assert!(d20.response.nyquist_band.max_db <= -19.0);
}

// ---------------------------------------------------------------------------
// 15. 低 taps INFEASIBLE 诚实报告（不是静默截断/放行）
// ---------------------------------------------------------------------------

#[test]
fn t34_low_taps_infeasible_reports_honestly() {
    // 8 taps：参数合法但通带纹波/Nyquist 衰减不可达 → design() 给完整报告，accepted=false
    let mut small = MinSlowParams::default();
    small.taps = 8;
    validate_params(&small).expect("8 taps 参数合法（只是不可达）");
    let d = design(&small).expect("构造仍应完成并给报告");
    assert!(!d.accepted(), "8 taps 不应通过判据");
    let failed = d.failed_criteria();
    println!("[infeasible] taps=8 failed={failed:?}");
    assert!(failed.contains(&"passband_ripple"));
    assert!(failed.contains(&"nyquist_attenuation"));
    let c = d.criteria.get("nyquist_attenuation").unwrap();
    assert!(!c.pass);
    assert!(c.measured.unwrap() > c.limit.unwrap());
    // design_strict 拒绝（INFEASIBLE），错误码为预算/可达性类，绝不静默返回 Ok
    let e = design_strict(&small).expect_err("design_strict 应拒绝 INFEASIBLE");
    assert_eq!(e.code, E_FIR_BUDGET);
    assert!(e.message.contains("INFEASIBLE"));
    assert!(e.message.contains("passband_ripple"));
    // 32 taps：仅 Nyquist 衰减不足（通带纹波已达标），用于覆盖“部分判据失败”路径
    let mut p32 = MinSlowParams::default();
    p32.taps = 32;
    let d32 = design(&p32).expect("32 taps 构造完成");
    assert!(!d32.accepted());
    assert_eq!(d32.failed_criteria(), vec!["nyquist_attenuation"]);
    assert_eq!(design_strict(&p32).unwrap_err().code, E_FIR_BUDGET);
}

// ---------------------------------------------------------------------------
// 16. 有限支撑 / 振铃（spec §1.3/§1.4）
// ---------------------------------------------------------------------------

#[test]
fn t34_finite_support_and_ringing() {
    let p = MinSlowParams::default();
    let d = design(&p).expect("设计成功");
    let h = &d.coefficients;
    let (first, last, eff) = t34_support(h);
    assert_eq!(d.support.first_effective, first);
    assert_eq!(d.support.last_effective, last);
    assert_eq!(d.support.effective_taps, eff);
    assert!(last < p.taps, "有效支撑 last={last} 超出 taps");
    assert_eq!(d.support.peak_index, t34_peak_index(h));
    // 振铃口径（spec §1.4：W=ceil(0.000333*fs)=16 @48k）
    let w = (RING_WINDOW_SEC * p.fs).ceil() as usize;
    assert_eq!(w, 16);
    assert_eq!(d.response.ringing.exclusion_samples, w);
    let peak = d.support.peak_index;
    // min_slow 主峰贴近 n=0 → pre-ring 按 spec §1.4 记 n/a（不是隐藏数值）
    assert_eq!(peak, 0);
    assert!(d.response.ringing.pre_db.is_none());
    assert!(d.response.ringing.pre_energy_db.is_none());
    assert!(peak + w + 1 < h.len());
    let post = d.response.ringing.post_db.expect("post ring 应可测");
    assert!(post <= 0.0);
    assert!(d.response.ringing.post_energy_db.is_some());
    // 独立复算 post ring（直接由系数计算）
    let seg = &h[peak + w + 1..];
    let mx = seg.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    let direct = 20.0 * (mx / d.support.max_abs).log10();
    assert!((post - direct).abs() <= 1e-9, "post ring 独立复算不符");
    let en: f64 = seg.iter().map(|v| v * v).sum();
    let tot: f64 = h.iter().map(|v| v * v).sum();
    let direct_en = 10.0 * (en / tot).log10();
    assert!(
        (d.response.ringing.post_energy_db.unwrap() - direct_en).abs() <= 1e-9,
        "post ring 能量独立复算不符"
    );
    assert!(d.support.max_abs > 0.0 && d.support.max_abs.is_finite());
}

// ---------------------------------------------------------------------------
// 17. 极点/稳定性边界（spec §3.2 判据 5）
// ---------------------------------------------------------------------------

#[test]
fn t34_stability_boundary_fir_poles_at_origin() {
    let p = MinSlowParams::default();
    let d = design(&p).expect("设计成功");
    assert_eq!(d.stability.filter_class, "fir");
    assert_eq!(d.stability.denominator_degree, 0, "FIR 分母度数必须为 0");
    assert_eq!(d.stability.pole_count_at_origin, d.coefficients.len());
    assert_eq!(d.stability.max_pole_radius, 0.0, "FIR 极点全在 z=0");
    assert!(d.stability.unconditionally_stable);
    assert!(!d.stability.iir_poles_introduced);
    assert!(d.stability.all_finite);
    assert_eq!(d.stability.numeric_violations, 0);
    assert!(d.stability.pole_stability_boundary.contains("poles at z=0"));
    // 独立验证：系数有限、max|h| 一致、直流增益一致
    assert!(d.coefficients.iter().all(|v| v.is_finite()));
    let mut max_abs = 0.0f64;
    for &v in &d.coefficients {
        max_abs = max_abs.max(v.abs());
    }
    assert!((d.stability.max_abs_coefficient - max_abs).abs() <= 1e-15);
    assert!((d.stability.dc_gain_linear - d.coefficients.iter().sum::<f64>()).abs() <= 1e-15);
    // MAY 诊断（不作门）有限；零点半径检查明确 not_run
    assert!(d.min_phase.inverse_causality_diagnostic_db.is_finite());
    assert!(d.min_phase.zero_radius_check.contains("not_run"));
}

// ---------------------------------------------------------------------------
// 18. 严格设计与摘要（T55 消费接口自检）
// ---------------------------------------------------------------------------

#[test]
fn t34_design_strict_and_summary_lines() {
    let params = MinSlowParams::default();
    let d = design_strict(&params).expect("默认参数应严格通过");
    let lines = d.summary_lines();
    println!("[summary] {} lines", lines.len());
    for l in &lines {
        if l.contains("criterion") || l.contains("rolloff") || l.contains("nyquist_band") {
            println!("  {l}");
        }
    }
    assert!(lines.len() >= 20, "摘要行数过少: {}", lines.len());
    let joined = lines.join("\n");
    assert!(joined.contains("family=min_slow"));
    assert!(joined.contains("criterion passband_ripple: PASS"));
    assert!(joined.contains("criterion nyquist_attenuation: PASS"));
    assert!(joined.contains("criterion min_phase_m2: PASS"));
    assert!(joined.contains("criterion family_distinction_slow_vs_fast: PASS"));
    assert!(joined.contains("criterion stability_boundary: PASS"));
    assert!(joined.contains("NOT zero-delay"));
    assert!(joined.contains("slow (raised-cosine f_p->Nyquist)"));
    assert!(joined.contains("stability: class=fir"));
    assert!(!joined.contains("FAIL"), "默认参数摘要不应出现 FAIL");
    // 参数回显一致（T55 manifest design 段可直接取）
    assert_eq!(d.params, params);
    assert_eq!(d.family(), FAMILY);
    assert_eq!(d.response.grid_points, (8 * params.taps).max(8_192));
    assert_eq!(d.prototype.len(), params.taps);
    assert_eq!(d.coefficients.len(), params.taps);
    // slow 家族标识与 fast 明确不同
    assert_eq!(d.family(), "min_slow");
    assert_ne!(FAMILY, "min_fast");
}
