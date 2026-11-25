//! 计划 4.3「单点目标函数」集成测试（single_point）。
//!
//! 覆盖任务验收：
//! ① 单麦退化：design 只有 1 个麦时无 NaN/Inf，seat_std 相关字段缺失（null）且有说明；
//! ② 单点 vs 多点一致性：M 个麦里取 1 个跑 single_point，与"只给这 1 个麦、combined 且
//!    w_std=0"逐位一致（同一算术路径，容差 = 0）；并证明既有目标（seat-std/null-depth/combined）
//!    在 M>=2 时 report 摘要与改动前一致；
//! ③ 三态与目标指标同现：构造必超限用例，报告同时含目标指标与 outcome=PARTIAL/NONE + 残差。
//!
//! 全部只用 design 点选择；holdout 不参与选择（只出现在报告里）。

use khaen_mimo_design::limits::SafetyConfig;
use khaen_mimo_design::objective::{ObjectiveConfig, ObjectiveKind};
use khaen_mimo_design::report::Json;
use khaen_mimo_design::{run_scene, Cx, RunConfig, SceneInput};

const NFFT: usize = 8192;

fn freqs_on_grid(bins: &[usize]) -> Vec<f64> {
    bins.iter()
        .map(|k| *k as f64 * 48000.0 / NFFT as f64)
        .collect()
}

/// 5 个麦（design 0/1/2，holdout 3/4）、2 源、6 个 1/3 oct 带（每带 1 个频点）。
fn make_input() -> SceneInput {
    let freqs = freqs_on_grid(&[7, 9, 11, 13, 17, 21]);
    let k = freqs.len();
    let n_mics = 5;
    let mut h = Vec::with_capacity(n_mics);
    for m in 0..n_mics {
        let g = 0.6 + 0.35 * m as f64;
        let hmic: Vec<Vec<Cx>> = (0..k)
            .map(|kk| {
                let a = 0.2 * kk as f64 + 0.15 * m as f64;
                vec![
                    Cx::new(g * a.cos(), g * a.sin()),
                    Cx::new(0.55 * g * (a + 1.1).cos(), 0.55 * g * (a + 1.1).sin()),
                ]
            })
            .collect();
        h.push(hmic);
    }
    SceneInput {
        scene_id: "synthetic-single-point".to_string(),
        fixture_dir: "synthetic".to_string(),
        ground_truth_sha256: "0".repeat(64),
        seed: 11,
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
        design_mics: vec![0usize, 1, 2],
        holdout_mics: vec![3usize, 4],
        h,
        band_centers_hz: vec![40.0, 50.0, 63.0, 80.0, 100.0, 125.0],
        extended_centers_hz: vec![25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0],
    }
}

/// 把 design 集缩成只有 1 个麦（单点），holdout 保持不变。
fn one_design_input(mic_index: usize) -> SceneInput {
    let mut input = make_input();
    input.design_mics = vec![mic_index];
    input
}

fn base_cfg() -> RunConfig {
    RunConfig {
        taps: 64,
        delay_samples: 16,
        nfft: NFFT,
        target_beta: 0.70,
        ..RunConfig::default()
    }
}

fn cfg_of(kind: ObjectiveKind) -> RunConfig {
    let mut cfg = base_cfg();
    cfg.objective = ObjectiveConfig::for_kind(kind, None);
    cfg
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

fn jstr(root: &Json, keys: &[&str]) -> Option<String> {
    match jget(root, keys)? {
        Json::Str(s) => Some(s.clone()),
        _ => None,
    }
}

fn jarr<'a>(root: &'a Json, keys: &[&str]) -> Option<&'a Vec<Json>> {
    match jget(root, keys)? {
        Json::Arr(v) => Some(v),
        _ => None,
    }
}

/// ① 单麦退化：M=1 时无 NaN/Inf；seat_std 字段缺失（null）且有说明；单点块出现。
#[test]
fn single_point_m1_is_finite_and_reports_degenerate_seat_fields() {
    let input = one_design_input(0);
    let mut cfg = cfg_of(ObjectiveKind::SinglePoint);
    cfg.safety = SafetyConfig::enabled();
    let run = run_scene(&input, &cfg).expect("single_point M=1 必须跑通");
    let text = run.report.to_pretty();
    // JSON 层不允许写出非有限数（Json::num 会把 NaN/±Inf 写成 null；这里再按 JSON 值位置防漏）
    for bad in [": NaN", ": Infinity", ": -Infinity", ": inf", ": -inf"] {
        assert!(
            !text.contains(bad),
            "报告含非有限值 {}: {}",
            bad,
            text.len()
        );
    }
    // seat_std 相关字段：缺失（null），不是伪造的 0 值成功
    let seat_mean = jget(
        &run.report,
        &["variants", "B3", "design", "seat_std_mean_db"],
    )
    .expect("seat_std_mean_db 字段应存在");
    assert!(
        matches!(seat_mean, Json::Null),
        "M=1 的 seat_std_mean_db 应为 null，实际 {}",
        seat_mean.to_compact()
    );
    let seat_max = jget(
        &run.report,
        &["variants", "B3", "design", "seat_std_max_db"],
    )
    .unwrap();
    assert!(matches!(seat_max, Json::Null));
    // max_dev_db 在 M=1 时是 0（显式定义），有限
    let max_dev = jnum(&run.report, &["variants", "B3", "design", "max_dev_db"]).unwrap();
    assert_eq!(max_dev, 0.0);

    // 单点目标报告块：目标指标 + 三态同块；显式退化标记与说明
    assert!(
        jget(&run.report, &["objective_outcome"]).is_some(),
        "single_point 必须有 objective_outcome 块"
    );
    assert_eq!(
        jstr(&run.report, &["objective_outcome", "objective"]).as_deref(),
        Some("single_point")
    );
    assert_eq!(
        jbool(
            &run.report,
            &["objective_outcome", "single_point_degenerate"]
        ),
        Some(true)
    );
    // 单点只覆盖皇帝位，不得声称空间效果（计划 5.3 的口径）
    assert_eq!(
        jstr(&run.report, &["objective_outcome", "spatial_scope"]).as_deref(),
        Some("SINGLE_POINT_NOT_SPATIAL")
    );
    assert_eq!(
        jbool(
            &run.report,
            &["objective_outcome", "target_metrics", "seat_std_defined"]
        ),
        Some(false)
    );
    assert_eq!(
        jbool(
            &run.report,
            &["objective_outcome", "target_metrics", "null_depth_defined"]
        ),
        Some(false)
    );
    // 目标指标（衰减/谷深）都是有限数；M=1 时谷深恒 0 dB（显式说明）
    let e_modal = jnum(
        &run.report,
        &["objective_outcome", "target_metrics", "e_modal_db"],
    )
    .expect("e_modal_db");
    assert!(e_modal.is_finite(), "e_modal_db={}", e_modal);
    let null_depth = jnum(
        &run.report,
        &["objective_outcome", "target_metrics", "null_depth_mean_db"],
    )
    .expect("null_depth_mean_db");
    assert!(null_depth.is_finite() && null_depth == 0.0);
    // 交付目标值有限（不因座位项 NaN 传播）
    let delivered = jnum(
        &run.report,
        &["objective_outcome", "objective_value", "delivered"],
    )
    .expect("delivered objective_value");
    assert!(delivered.is_finite(), "delivered={}", delivered);
    // 说明：M=1 退化 + 单点目标语义
    assert!(
        text.contains("degenerate branch: only 1 design point"),
        "缺少 M=1 退化说明"
    );
    assert!(
        text.contains("single-point objective"),
        "缺少 single_point 目标语义说明"
    );
    // CV 不可能（单点）：如实标 skipped，不伪造折分数
    assert_eq!(jbool(&run.report, &["cv", "cv_skipped"]), Some(true));
    // 3 折 CV 在 M=1 无法划分 → 不给假分数
    assert!(jarr(&run.report, &["cv", "fold_scores"]).is_some());
}

/// ② 单点 vs 多点一致性：M 个麦里取 1 个跑 single_point，
/// 与"只给这 1 个麦、combined 且 w_std=0"逐位一致（容差 = 0）。
#[test]
fn single_point_equals_combined_with_zero_seat_weight_on_same_single_mic() {
    let input = one_design_input(2);
    let sp_cfg = cfg_of(ObjectiveKind::SinglePoint);
    let mut comb_cfg = cfg_of(ObjectiveKind::Combined);
    comb_cfg.objective.w_std = 0.0; // 单点没有"座位间"，combined 去掉座位项
    let sp = run_scene(&input, &sp_cfg).expect("single_point run");
    let comb = run_scene(&input, &comb_cfg).expect("combined(w_std=0) run");

    // 目标值/指标逐位一致（同一 band target + 同一 IRLS 权重 = 同一算术路径）
    for variant in ["B2", "B3"] {
        for field in [
            "e_modal_db",
            "e_modal",
            "r_max",
            "seat_std_mean_db",
            "null_depth_mean_db",
            "null_depth_worst_db",
            "max_dev_db",
        ] {
            let a = jnum(&sp.report, &["variants", variant, "design", field]);
            let b = jnum(&comb.report, &["variants", variant, "design", field]);
            assert_eq!(
                a.map(f64::to_bits),
                b.map(f64::to_bits),
                "{} design.{} 不一致: {:?} vs {:?}",
                variant,
                field,
                a,
                b
            );
        }
        let a = jnum(&sp.report, &["variants", variant, "objective_value"]);
        let b = jnum(&comb.report, &["variants", variant, "objective_value"]);
        assert_eq!(a.map(f64::to_bits), b.map(f64::to_bits));
    }
    // 设计产物（FIR）与逐点逐带指标（metrics.csv）逐位一致
    assert_eq!(sp.fir_files.len(), comb.fir_files.len());
    for ((path_a, text_a), (path_b, text_b)) in sp.fir_files.iter().zip(comb.fir_files.iter()) {
        assert_eq!(path_a, path_b);
        assert_eq!(text_a, text_b, "FIR {} 不一致", path_a);
    }
    assert_eq!(
        sp.metrics_csv, comb.metrics_csv,
        "逐点逐带指标（metrics.csv）不一致"
    );
    // single_point 单点目标值 = 模态衰减 + 谷深（M=1 谷深 0）→ 等于 e_modal_db
    let delivered = jnum(
        &sp.report,
        &["objective_outcome", "objective_value", "delivered"],
    )
    .unwrap();
    let e_modal = jnum(&sp.report, &["variants", "B3", "design", "e_modal_db"]).unwrap();
    assert!(
        (delivered - e_modal).abs() < 1e-12,
        "delivered={} e_modal_db={}",
        delivered,
        e_modal
    );
}

/// ③ 三态与目标指标同现：极端目标 → 必超安全限，报告同块含目标指标 + PARTIAL/NONE + 残差。
#[test]
fn over_limit_single_point_reports_target_metrics_and_three_state_with_residuals() {
    let input = one_design_input(0);
    let mut cfg = cfg_of(ObjectiveKind::SinglePoint);
    cfg.safety = SafetyConfig {
        max_pre_gain_reduction_db: 3.0,
        max_target_scale_db: 3.0,
        ..SafetyConfig::enabled()
    };
    // 极端目标（远超 0.85 名义值）→ 需要大 boost → 必撞单段帽/正向和/峰值
    cfg.objective.beta = 3.0;
    let run = run_scene(&input, &cfg).expect("run");

    // 目标指标（衰减/谷深）与三态在同一个块里
    let outcome = jstr(&run.report, &["objective_outcome", "outcome"]).expect("outcome");
    assert!(
        outcome == "PARTIAL" || outcome == "NONE",
        "必超限用例不应报 FULL: {}",
        outcome
    );
    assert_eq!(
        jstr(&run.report, &["variants", "B3", "outcome"]).as_deref(),
        Some(outcome.as_str()),
        "顶层 objective_outcome 与 variants.B3 的三态必须一致"
    );
    let e_modal = jnum(
        &run.report,
        &["objective_outcome", "target_metrics", "e_modal_db"],
    )
    .expect("target e_modal_db");
    assert!(e_modal.is_finite());
    assert!(jnum(
        &run.report,
        &["objective_outcome", "target_metrics", "null_depth_mean_db"]
    )
    .is_some());
    // 算法自评（钳位前）与交付解（钳位后）都在
    assert!(!matches!(
        jget(&run.report, &["objective_outcome", "algorithm_metrics"]).unwrap(),
        Json::Null
    ));
    assert!(jnum(
        &run.report,
        &["objective_outcome", "objective_value", "algorithm"]
    )
    .is_some());
    // 残差逐项（角色/频点/差多少/撞哪条限）与"安全限永不越过"
    let residuals = jarr(&run.report, &["objective_outcome", "residuals"]).expect("residuals");
    assert!(!residuals.is_empty(), "PARTIAL/NONE 必须带残差");
    for residual in residuals {
        if let Json::Obj(items) = residual {
            for key in ["role", "target_db", "achieved_db", "deficit_db", "limit"] {
                assert!(items.iter().any(|(k, _)| k == key), "残差缺少字段 {}", key);
            }
        }
    }
    assert_eq!(
        jbool(
            &run.report,
            &["objective_outcome", "safety", "never_exceeded"]
        ),
        Some(true),
        "安全限永不越过"
    );
    // 交付解与 algorithm 的目标值都存在（数值可能不同：钳位/缩放会改变目标值）
    assert!(jnum(
        &run.report,
        &["objective_outcome", "objective_value", "delivered"]
    )
    .unwrap()
    .is_finite());
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// 既有目标（M>=2 design）行为逐位不变：report.json 摘要冻结（改动前 HEAD 采集）。
#[test]
fn existing_v2_objectives_report_digests_frozen() {
    // 摘要采集夹具与本文件 make_input 相同（仅 scene_id 与采集脚本一致）
    let mut input = make_input(); // 3 design + 2 holdout，不是单点
    input.scene_id = "synthetic-v2".to_string();
    // 数值在 JSON 浮点规范化（10 位有效数字，跨平台稳定）之后重新采集。
    let expected: [(ObjectiveKind, u64, usize); 3] = [
        (ObjectiveKind::SeatStd, 9438548629171444640, 63838),
        (ObjectiveKind::NullDepth, 17911713529853422224, 63011),
        (ObjectiveKind::Combined, 11619756330895966557, 57686),
    ];
    for (kind, digest, len) in expected {
        let run = run_scene(&input, &cfg_of(kind)).expect("existing objective run");
        let text = run.report.to_pretty();
        assert_eq!(
            text.len(),
            len,
            "{} report 字节数变化（既有目标行为必须不变）",
            kind.tag()
        );
        assert_eq!(
            fnv1a64(text.as_bytes()),
            digest,
            "{} report 摘要变化（既有目标行为必须不变）",
            kind.tag()
        );
        // 既有目标不带 objective_outcome 块（仅 single_point/安全钳位才出现）
        assert!(
            jget(&run.report, &["objective_outcome"]).is_none(),
            "{} 不应出现 objective_outcome 块",
            kind.tag()
        );
    }
}
