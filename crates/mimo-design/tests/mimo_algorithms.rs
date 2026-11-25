//! mimo-design 集成测试（契约 §6 / §10 必测项）。
//!
//! 覆盖：
//! 1) 小规模合成 H：B3 在 design 上 E_modal 优于 B0/B1（解析可推的退化情形）；
//! 2) holdout 指标被计算但绝不参与选择（改变 holdout H 不改变 design 指标与 CV λ）；
//! 3) FIR 长度/因果/增益帽/声明式 headroom 生效；
//! 4) Levinson-Durbin 对已知 Toeplitz 的解析解正确；
//! 5) λ CV 选择可复现（两次运行完全一致）。

use khaen_mimo_design::report::Json;
use khaen_mimo_design::{run_scene, Cx, RunConfig, SceneInput};

const NFFT: usize = 8192;

fn freqs_on_grid(bins: &[usize]) -> Vec<f64> {
    bins.iter()
        .map(|k| *k as f64 * 48000.0 / NFFT as f64)
        .collect()
}

fn cfg() -> RunConfig {
    RunConfig {
        taps: 64,
        delay_samples: 16,
        nfft: NFFT,
        ..RunConfig::default()
    }
}

/// 合成场景：3 design + 2 holdout；design 点 H 完全相同（空间 std(B0)=0 → B1 无法改进），
/// 让 B3 的宽带能量残余（目标 beta=0.7）成为唯一可改进方向。
/// holdout_variant 只改变 holdout 点的源间幅度/相位平衡（不改变 design）→ 验证 holdout 指标被算但不参与选择。
fn make_input(holdout_variant: f64) -> SceneInput {
    let freqs = freqs_on_grid(&[7, 9, 11, 13, 17, 21]);
    let k = freqs.len();
    let n_mics = 5;
    let design_mics = vec![0usize, 1, 2];
    let holdout_mics = vec![3usize, 4];
    let mut h = Vec::with_capacity(n_mics);
    for m in 0..n_mics {
        let is_holdout = holdout_mics.contains(&m);
        let (g2_mag, g2_ph) = if is_holdout {
            (1.0 + 0.5 * holdout_variant, 0.4 * holdout_variant)
        } else {
            (1.0, 0.0)
        };
        let hmic: Vec<Vec<Cx>> = (0..k)
            .map(|kk| {
                let a = 0.15 * kk as f64;
                let ph2 = a + 0.8 + g2_ph;
                vec![
                    Cx::new(a.cos(), a.sin()),
                    Cx::new(g2_mag * 0.6 * ph2.cos(), g2_mag * 0.6 * ph2.sin()),
                ]
            })
            .collect();
        h.push(hmic);
    }
    SceneInput {
        scene_id: "synthetic-integration".to_string(),
        fixture_dir: "synthetic".to_string(),
        ground_truth_sha256: "0".repeat(64),
        seed: 7,
        rir_scale: 0.5,
        sample_rate_hz: 48000.0,
        freqs_hz: freqs,
        source_ids: vec!["SUB1".to_string(), "SUB2".to_string()],
        mic_ids: vec![
            "P01".to_string(),
            "P02".to_string(),
            "P03".to_string(),
            "P04".to_string(),
            "P05".to_string(),
        ],
        design_mics,
        holdout_mics,
        h,
        band_centers_hz: vec![40.0, 50.0, 63.0, 80.0, 100.0, 125.0],
        extended_centers_hz: vec![25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0],
    }
}

fn jget<'a>(root: &'a Json, keys: &[&str]) -> Option<&'a Json> {
    let mut cur = root;
    for key in keys {
        match cur {
            Json::Obj(items) => cur = &items.iter().find(|(k, _)| k == key)?.1,
            _ => return None,
        }
    }
    Some(cur)
}

fn jnum(root: &Json, keys: &[&str]) -> Option<f64> {
    match jget(root, keys)? {
        Json::Num(v) => Some(*v),
        _ => None,
    }
}

fn jbool(root: &Json, keys: &[&str]) -> Option<bool> {
    match jget(root, keys)? {
        Json::Bool(v) => Some(*v),
        _ => None,
    }
}

#[test]
fn b3_design_e_modal_beats_b0_and_b1_on_synthetic_h() {
    let input = make_input(2.0);
    let run = run_scene(&input, &cfg()).expect("run_scene");
    let r = &run.report;
    let b0 = jnum(r, &["variants", "B0", "design", "e_modal_db"]).unwrap();
    let b1 = jnum(r, &["variants", "B1", "design", "e_modal_db"]).unwrap();
    let b3 = jnum(r, &["variants", "B3", "design", "e_modal_db"]).unwrap();
    // B0 归一化后必须为 0 dB
    assert!(b0.abs() < 1e-9, "B0 e_modal_db={} != 0", b0);
    // 合成 H 下 B1 目标（空间 std）在 B0 已为 0 → B1 应保持 identity，E_modal=0
    assert!(b1.abs() < 1e-6, "B1 e_modal_db={}（应保持 identity）", b1);
    // B3 必须显著改善（目标 beta=0.7 → 理论 ~-3.1 dB；允许 FIR 拟合/截断偏差）
    assert!(b3 < -0.5, "B3 design e_modal_db={} 未改善", b3);
    assert!(
        b3 < b0 - 0.5 && b3 < b1 - 0.5,
        "B3 未优于 B0/B1: B0={} B1={} B3={}",
        b0,
        b1,
        b3
    );
    // 所有 design band r_k < 1（无能量迁移到某带）
    if let Some(Json::Arr(rk)) = jget(r, &["variants", "B3", "design", "r_k"]) {
        for (i, v) in rk.iter().enumerate() {
            if let Json::Num(x) = v {
                assert!(*x < 1.0, "band {} r_k={} >= 1", i, x);
            }
        }
    } else {
        panic!("missing B3 design r_k");
    }
}

#[test]
fn holdout_is_computed_but_never_used_in_design_or_cv() {
    let a = run_scene(&make_input(0.0), &cfg()).expect("run a");
    let b = run_scene(&make_input(1.0), &cfg()).expect("run b");
    // design 指标完全一致（holdout 不参与）
    let da = jget(&a.report, &["variants", "B3", "design"])
        .unwrap()
        .to_compact();
    let db = jget(&b.report, &["variants", "B3", "design"])
        .unwrap()
        .to_compact();
    assert_eq!(da, db, "design 指标随 holdout 改变而变化（holdout 泄漏）");
    // 选中的 λ 与 CV 表也一致
    let cva = jget(&a.report, &["cv"]).unwrap().to_compact();
    let cvb = jget(&b.report, &["cv"]).unwrap().to_compact();
    assert_eq!(cva, cvb, "CV 随 holdout 改变而变化（holdout 泄漏）");
    // holdout 指标确实被计算且不同
    let ha = jnum(&a.report, &["variants", "B3", "holdout", "e_modal_db"]).unwrap();
    let hb = jnum(&b.report, &["variants", "B3", "holdout", "e_modal_db"]).unwrap();
    assert!(ha.is_finite() && hb.is_finite(), "holdout 指标缺失");
    assert!(
        (ha - hb).abs() > 1e-9,
        "holdout 指标未区分不同 H: {} vs {}",
        ha,
        hb
    );
}

#[test]
fn fir_length_causality_and_constraints_are_enforced() {
    let cfg = cfg();
    let input = make_input(2.0);
    let run = run_scene(&input, &cfg).expect("run_scene");
    // B3 canonical FIR：fir/SUB1.csv
    let (_path, content) = run
        .fir_files
        .iter()
        .find(|(p, _)| p == "fir/SUB1.csv")
        .expect("missing fir/SUB1.csv");
    assert!(content.starts_with("# source=SUB1 taps=64 sample_rate_hz=48000 delay_samples=16"));
    let taps: Vec<f64> = content
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.trim().parse::<f64>().expect("tap parse"))
        .collect();
    assert_eq!(taps.len(), cfg.taps, "FIR 长度错误: {}", taps.len());
    assert!(taps.iter().all(|v| v.is_finite()));
    // 因果 + 公共延迟：冲激主峰位于 delay 样本
    let (argmax, _) = taps
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
        .unwrap();
    assert_eq!(
        argmax, cfg.delay_samples,
        "主峰不在公共延迟 D 处（非因果/延迟错）"
    );
    // 每个 B3 FIR 文件都存在且长度正确
    for src in &input.source_ids {
        let p = format!("fir/{}.csv", src);
        assert!(run.fir_files.iter().any(|(pp, _)| *pp == p));
    }
    // 约束：in-band max|G| <= +6 dB、声明式 headroom、无 0.5/Σ|h| 全局缩放
    let scale = jnum(
        &run.report,
        &["variants", "B3", "constraints", "global_scale"],
    )
    .unwrap();
    assert!(
        (scale - 1.0).abs() < 1e-12,
        "global_scale={}（不应使用 Σ|h| 全局缩放）",
        scale
    );
    assert!(jbool(
        &run.report,
        &["variants", "B3", "constraints", "headroom_ok"]
    )
    .unwrap());
    if let Some(Json::Arr(gains)) = jget(
        &run.report,
        &["variants", "B3", "constraints", "per_source_max_gain_db"],
    ) {
        for g in gains {
            if let Json::Num(v) = g {
                assert!(*v <= 6.0 + 1e-9, "per_source_max_gain_db={} > +6 dB", v);
            }
        }
    } else {
        panic!("missing per_source_max_gain_db");
    }
    // 有效 effort 与预测峰值必须报告
    assert!(jnum(&run.report, &["variants", "B3", "constraints", "effort"]).unwrap() > 0.0);
    assert!(jget(
        &run.report,
        &["variants", "B3", "constraints", "predicted_peak_per_source"]
    )
    .is_some());
}

#[test]
fn cv_lambda_selection_is_reproducible() {
    let cfg = cfg();
    let input = make_input(2.0);
    let a = run_scene(&input, &cfg).expect("run a");
    let b = run_scene(&input, &cfg).expect("run b");
    let ca = jget(&a.report, &["cv"]).unwrap().to_compact();
    let cb = jget(&b.report, &["cv"]).unwrap().to_compact();
    assert_eq!(ca, cb, "CV λ 选择/分数不可复现");
    assert!(jnum(&a.report, &["cv", "selected_lambda_rel"]).unwrap() > 0.0);
    // fold 表结构完整
    if let Some(Json::Arr(scores)) = jget(&a.report, &["cv", "fold_scores"]) {
        assert_eq!(scores.len(), cfg.lambda_grid_rel.len());
    } else {
        panic!("missing cv.fold_scores");
    }
}

#[test]
fn levinson_matches_known_toeplitz() {
    use khaen_mimo_design::fir::levinson_solve;
    // 与 lib 单测一致的手算 3x3 解
    let ac = [1.0, 0.5, 0.2];
    let b = [1.0, 2.0, 3.0];
    let x = levinson_solve(&ac, &b).expect("levinson");
    let expect = [0.17857142857142858, 0.5714285714285714, 2.6785714285714284];
    for (i, (got, want)) in x.iter().zip(expect.iter()).enumerate() {
        assert!((got - want).abs() < 1e-12, "idx {}: {} != {}", i, got, want);
    }
    // 残差 T x - b ≈ 0
    for (i, want) in b.iter().enumerate() {
        let mut s = 0.0;
        for (j, xj) in x.iter().enumerate() {
            let tau = (i as isize - j as isize).unsigned_abs();
            s += ac[tau] * xj;
        }
        assert!((s - want).abs() < 1e-12);
    }
}
