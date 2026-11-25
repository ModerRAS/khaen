//! T10 private integration tests: 手算向量/独立 oracle/独立 DFT（待 integrator 注册到 public_registration.rs）。
//!
//! 覆盖：规范 §9 V1–V4/V12/V13 + 与构造代码独立的系数 oracle + 独立 naive DFT 对拍。
//! 运行（integrator 注册前）：rustc --test 方式手工编译执行，见 evidence/T10/manifest。

use tspi_fir_designer::linear::{
    delay_ms, delay_samples, design, group_delay_at, kaiser_required_taps, magnitude_db_at,
    response_at, AnalysisOptions, LinearSpec, Symmetry, WindowKind,
};

/// 独立实现的 I0（A&S 级数），不调用被测模块的 bessel_i0。
fn oracle_bessel_i0(x: f64) -> f64 {
    let q = x * x / 4.0;
    let mut sum = 1.0_f64;
    let mut term = 1.0_f64;
    let mut k = 1.0_f64;
    while k < 500.0 {
        term *= q / (k * k);
        sum += term;
        if term.abs() < 1e-18 * sum.abs() {
            break;
        }
        k += 1.0;
    }
    sum
}

/// 独立实现的 Kaiser beta 公式。
fn oracle_kaiser_beta(a: f64) -> f64 {
    if a > 50.0 {
        0.1102 * (a - 8.7)
    } else if a >= 21.0 {
        0.5842 * (a - 21.0).powf(0.4) + 0.07886 * (a - 21.0)
    } else {
        0.0
    }
}

/// 独立实现的归一化 sinc。
fn oracle_sinc(x: f64) -> f64 {
    if x == 0.0 {
        1.0
    } else {
        (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
    }
}

/// 独立实现的 Kaiser 窗（全长度直接公式，与已镜像构造的模块实现路径不同）。
fn oracle_kaiser_window(taps: usize, beta: f64) -> Vec<f64> {
    let denom = oracle_bessel_i0(beta);
    (0..taps)
        .map(|n| {
            let t = 2.0 * n as f64 / (taps as f64 - 1.0) - 1.0;
            oracle_bessel_i0(beta * (1.0 - t * t).max(0.0).sqrt()) / denom
        })
        .collect()
}

/// 独立 LP 系数 oracle：理想 sinc × Kaiser 窗 + DC 归一（全长度直接公式）。
fn oracle_lowpass(fs: f64, taps: usize, fp: f64, fst: f64, a_db: f64) -> Vec<f64> {
    let fc = 0.5 * (fp + fst);
    let d = (taps - 1) as f64 / 2.0;
    let w = oracle_kaiser_window(taps, oracle_kaiser_beta(a_db));
    let r = 2.0 * fc / fs;
    let mut h: Vec<f64> = (0..taps)
        .map(|n| r * oracle_sinc(r * (n as f64 - d)) * w[n])
        .collect();
    let sum: f64 = h.iter().sum();
    for x in &mut h {
        *x /= sum;
    }
    h
}

/// 独立 HP 系数 oracle：δ(n−D) − LP_ideal，减均值消 DC，按通带几何中心归一。
fn oracle_highpass(fs: f64, taps: usize, fp: f64, fst: f64, a_db: f64) -> Vec<f64> {
    let fc = 0.5 * (fp + fst);
    let d = (taps - 1) as f64 / 2.0;
    let w = oracle_kaiser_window(taps, oracle_kaiser_beta(a_db));
    let r = 2.0 * fc / fs;
    let mut h: Vec<f64> = (0..taps)
        .map(|n| w[n] * (oracle_sinc(n as f64 - d) - r * oracle_sinc(r * (n as f64 - d))))
        .collect();
    let mean = h.iter().sum::<f64>() / taps as f64;
    for x in &mut h {
        *x -= mean;
    }
    let f_ref = (fp * 0.499 * fs).sqrt();
    let (re, im) = naive_dft(&h, fs, f_ref);
    let mag = re.hypot(im);
    for x in &mut h {
        *x /= mag;
    }
    h
}

/// 独立反对称 HP oracle（Type III/IV 偶数长）：全长度直接公式 + 归一。
fn oracle_antisymmetric_highpass(fs: f64, taps: usize, fp: f64, fst: f64, a_db: f64) -> Vec<f64> {
    let fc = 0.5 * (fp + fst);
    let d = (taps - 1) as f64 / 2.0;
    let w = oracle_kaiser_window(taps, oracle_kaiser_beta(a_db));
    let wc = 2.0 * std::f64::consts::PI * fc / fs;
    let mut h: Vec<f64> = (0..taps)
        .map(|n| {
            let m = n as f64 - d;
            if m == 0.0 {
                0.0
            } else {
                w[n] * ((wc * m).cos() - (std::f64::consts::PI * m).cos())
                    / (std::f64::consts::PI * m)
            }
        })
        .collect();
    let f_ref = (fp * 0.499 * fs).sqrt();
    let (re, im) = naive_dft(&h, fs, f_ref);
    let mag = re.hypot(im);
    for x in &mut h {
        *x /= mag;
    }
    h
}

/// 独立 naive DFT（逐样本 cos/sin，不复用被测模块的递推）。
fn naive_dft(h: &[f64], fs: f64, f: f64) -> (f64, f64) {
    let omega = 2.0 * std::f64::consts::PI * f / fs;
    let mut re = 0.0_f64;
    let mut im = 0.0_f64;
    for (n, &c) in h.iter().enumerate() {
        let a = omega * n as f64;
        re += c * a.cos();
        im -= c * a.sin();
    }
    (re, im)
}

/// 独立 naive 群延迟：Re(Σ n h e^{−jωn} / Σ h e^{−jωn})。
fn naive_group_delay(h: &[f64], fs: f64, f: f64) -> f64 {
    let omega = 2.0 * std::f64::consts::PI * f / fs;
    let mut re = 0.0_f64;
    let mut im = 0.0_f64;
    let mut br = 0.0_f64;
    let mut bi = 0.0_f64;
    for (n, &c) in h.iter().enumerate() {
        let a = omega * n as f64;
        let (s, co) = a.sin_cos();
        re += c * co;
        im -= c * s;
        br += n as f64 * c * co;
        bi -= n as f64 * c * s;
    }
    let den = re * re + im * im;
    (br * re + bi * im) / den
}

fn max_abs_diff(a: &[f64], b: &[f64]) -> f64 {
    a.iter()
        .zip(b.iter())
        .fold(0.0_f64, |m, (x, y)| m.max((x - y).abs()))
}

#[test]
fn t10_v2_delay_table_matches_spec() {
    assert_eq!(delay_samples(1024), 511.5);
    assert!((delay_ms(1024, 48_000.0) - 10.656_25).abs() < 1e-12);
    assert!((delay_ms(2048, 48_000.0) - 21.322_916_666_666_66).abs() < 1e-9);
    assert!((delay_ms(4096, 48_000.0) - 42.656_25).abs() < 1e-9);
    assert!((delay_ms(8192, 48_000.0) - 85.322_916_666_666_66).abs() < 1e-9);
    assert!((delay_ms(16_384, 48_000.0) - 170.656_25).abs() < 1e-9);
    assert!((delay_ms(4096, 96_000.0) - 21.328_125).abs() < 1e-9);
}

#[test]
fn t10_v3_three_tap_hand_vector() {
    let h = [0.25_f64, 0.5, 0.25];
    let fs = 48_000.0;
    let (re, im) = naive_dft(&h, fs, 0.25 * fs);
    let resp = response_at(&h, fs, 0.25 * fs);
    assert!((resp.re - re).abs() < 1e-15 && (resp.im - im).abs() < 1e-15);
    assert!((resp.magnitude_db() - (-6.020_599_913_279_624)).abs() < 1e-9);
    assert!((resp.phase_deg() - (-90.0)).abs() < 1e-9);
    assert!((response_at(&h, fs, 0.0).magnitude() - 1.0).abs() < 1e-15);
    assert!(magnitude_db_at(&h, fs, 0.5 * fs) <= -240.0);
    assert!((group_delay_at(&h, fs, 12_345.0) - 1.0).abs() < 1e-9);
    assert!((naive_group_delay(&h, fs, 12_345.0) - 1.0).abs() < 1e-9);
}

#[test]
fn t10_v4_five_tap_hand_vector() {
    let h = [1.0_f64 / 9.0, 2.0 / 9.0, 3.0 / 9.0, 2.0 / 9.0, 1.0 / 9.0];
    let fs = 48_000.0;
    let nyq = response_at(&h, fs, 0.5 * fs);
    assert!((nyq.magnitude() - 1.0 / 9.0).abs() < 1e-15);
    assert!((nyq.magnitude_db() - 20.0 * (1.0_f64 / 9.0).log10()).abs() < 1e-9);
    assert!((group_delay_at(&h, fs, 437.0) - 2.0).abs() < 1e-9);
    assert!((response_at(&h, fs, 0.0).magnitude() - 1.0).abs() < 1e-15);
}

#[test]
fn t10_v1_even_symmetric_nyquist_zero() {
    let spec = LinearSpec::lowpass(48_000.0, 512, 18_000.0, 20_000.0);
    let d = design(&spec).expect("design");
    let (re, im) = naive_dft(&d.coeffs, spec.fs, 0.5 * spec.fs);
    assert!(re.hypot(im) <= 1e-12);
    assert!(magnitude_db_at(&d.coeffs, spec.fs, 0.5 * spec.fs) <= -240.0);
}

#[test]
fn t10_v13_kaiser_required_taps_table() {
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
        assert_eq!(
            kaiser_required_taps(a, fs, df),
            Some(want),
            "A={a} df={df} fs={fs}"
        );
    }
}

#[test]
fn t10_coefficients_match_independent_lowpass_oracle() {
    let spec = LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0);
    let d = design(&spec).expect("design");
    let oracle = oracle_lowpass(48_000.0, 511, 18_000.0, 20_000.0, 80.0);
    assert!(max_abs_diff(&d.coeffs, &oracle) <= 1e-12);
}

#[test]
fn t10_coefficients_match_independent_highpass_oracle() {
    let spec = LinearSpec::highpass(48_000.0, 513, 5_000.0, 5_500.0).with_attenuation(60.0);
    let d = design(&spec).expect("design");
    let oracle = oracle_highpass(48_000.0, 513, 5_500.0, 5_000.0, 60.0);
    assert!(max_abs_diff(&d.coeffs, &oracle) <= 1e-12);
}

#[test]
fn t10_coefficients_match_independent_antisymmetric_oracle() {
    let spec = LinearSpec::highpass(48_000.0, 512, 5_000.0, 5_500.0)
        .with_attenuation(60.0)
        .with_symmetry(Symmetry::Antisymmetric);
    let d = design(&spec).expect("design");
    let oracle = oracle_antisymmetric_highpass(48_000.0, 512, 5_500.0, 5_000.0, 60.0);
    assert!(max_abs_diff(&d.coeffs, &oracle) <= 1e-12);
}

#[test]
fn t10_response_matches_naive_dft_across_band() {
    let spec = LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0);
    let d = design(&spec).expect("design");
    // 紧容差只用于 |H| 足够的频点（深阻带时 dB 对绝对误差敏感）。
    for f in [20.0, 100.0, 1_000.0, 10_000.0, 18_000.0, 19_000.0] {
        let (re, im) = naive_dft(&d.coeffs, spec.fs, f);
        let resp = response_at(&d.coeffs, spec.fs, f);
        let mag_naive = 20.0 * re.hypot(im).max(1e-12).log10();
        assert!(
            (resp.magnitude_db() - mag_naive).abs() <= 1e-6,
            "f={f} resp={} naive={}",
            resp.magnitude_db(),
            mag_naive
        );
        assert!((resp.re - re).abs() <= 1e-12 && (resp.im - im).abs() <= 1e-12);
    }
    // 深阻带（23.9 kHz）：两种实现需落在同一量级（dB 差 <=0.05），且满足阻带衰减。
    let (re, im) = naive_dft(&d.coeffs, spec.fs, 23_900.0);
    let naive_db = 20.0 * re.hypot(im).max(1e-12).log10();
    let resp_db = magnitude_db_at(&d.coeffs, spec.fs, 23_900.0);
    assert!(
        naive_db <= -79.5,
        "23.9k naive={naive_db} dB not in stopband"
    );
    assert!(
        (resp_db - naive_db).abs() <= 0.05,
        "23.9k resp={resp_db} naive={naive_db}"
    );
}

#[test]
fn t10_group_delay_matches_independent_formula() {
    let spec = LinearSpec::highpass(48_000.0, 512, 8_000.0, 9_000.0)
        .with_attenuation(60.0)
        .with_symmetry(Symmetry::Antisymmetric);
    let d = design(&spec).expect("design");
    for f in [100.0, 1_000.0, 5_000.0, 12_000.0, 20_000.0] {
        let gd = group_delay_at(&d.coeffs, spec.fs, f);
        let naive = naive_group_delay(&d.coeffs, spec.fs, f);
        // 两种独立实现的数值条件不同（低频 100 Hz 处约 1e-7 sample），用 1e-5 交叉核对。
        assert!((gd - naive).abs() <= 1e-5, "f={f} gd={gd} naive={naive}");
        assert!(
            (gd - 255.5).abs() <= 1e-4,
            "constant GD broken at f={f}: {gd}"
        );
    }
}

#[test]
fn t10_normative_grid_point_count() {
    let spec = LinearSpec::lowpass(48_000.0, 1024, 18_000.0, 20_000.0);
    let opts = AnalysisOptions::normative(&spec);
    assert_eq!(opts.mag_points, 8192);
    assert_eq!(opts.transition_points, 8192);
    let big = LinearSpec::lowpass(48_000.0, 2048, 18_000.0, 20_000.0);
    assert_eq!(AnalysisOptions::normative(&big).mag_points, 16_384);
}

#[test]
fn t10_blackman_harris_matches_independent_oracle_shape() {
    let spec = LinearSpec::lowpass(48_000.0, 511, 18_000.0, 20_000.0)
        .with_window(WindowKind::BlackmanHarris);
    let d = design(&spec).expect("design");
    // BH 4 项窗系数：独立直接公式。
    let bh: Vec<f64> = (0..511)
        .map(|n| {
            let x = 2.0 * std::f64::consts::PI * n as f64 / 510.0;
            0.35875 - 0.48829 * x.cos() + 0.14128 * (2.0 * x).cos() - 0.01168 * (3.0 * x).cos()
        })
        .collect();
    let fc = 0.5 * (18_000.0 + 20_000.0);
    let r = 2.0 * fc / 48_000.0;
    let dly = 255.0;
    let mut oracle: Vec<f64> = (0..511)
        .map(|n| r * oracle_sinc(r * (n as f64 - dly)) * bh[n])
        .collect();
    let sum: f64 = oracle.iter().sum();
    for x in &mut oracle {
        *x /= sum;
    }
    assert!(max_abs_diff(&d.coeffs, &oracle) <= 1e-12);
}
