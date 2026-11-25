//! 场景级编排：B0/B1/B2/B3 → 指标 → report.json / metrics.csv / FIR CSV。
//!
//! 口径（与 roomforge-mimo 冻结，见 CONTRACT §8）：
//! - design 块：所有 variant 的 R_k 分母 = B0(design) 的 E_k0，α 也用 B0(design)；
//! - holdout 块：before = B0 在 holdout 点集上的每带能量；每点 per_point_band_db 的 before = 该点 B0；
//! - 目标 A(f) = beta · design 空间 RMS（功率域 1/12 oct 平滑，相位 0），beta 默认 0.70；
//! - B2 固定 λ_rel = 1e-6 · mean|H|²；B3 λ 由 3 折 design-only CV 选择；
//! - FIR 约束（增益帽 + headroom 全局缩放）只作用于 B2/B3；B0/B1 如实报告标量参数。

use crate::authority::{self, AuthorityConfig, AuthorityWeights};
use crate::baseline;
use crate::constraints::{self, FirConstraintReport, ScalarConstraintReport};
use crate::evaluate::{self, BandPlan, SpatialStats};
use crate::limits::{self, SafetyConfig, SafetyReport};
use crate::objective::{self, ObjectiveConfig, ObjectiveKind};
use crate::quality::{QualityConfig, QualityGate};
use crate::report::{self, Json};
use crate::solve::{self, Cx, DesignOptions};
use crate::target;

pub const REPORT_FORMAT: &str = "khaen-mimo-sim-report/v1";
pub const TOOL_NAME: &str = "khaen-mimo-opt";
pub const CLAIM: &str = "synthetic offline study only; no real room, no measurement";

/// 算法参数（默认值 = 契约 §6/§8 冻结默认）。
#[derive(Clone, Debug)]
pub struct RunConfig {
    pub taps: usize,
    pub delay_samples: usize,
    pub irls_iterations: usize,
    pub lambda_b2_rel: f64,
    pub lambda_grid_rel: Vec<f64>,
    pub target_beta: f64,
    pub diag_load_rel: f64,
    pub cv_folds: usize,
    pub gain_cap_db: f64,
    /// IFFT 频谱长度（与夹具 rir.fft_size 一致，契约 65536）
    pub nfft: usize,
    /// OBJECTIVES-V2 目标配置（默认 emodal / beta=0.70 = v1 回归路径）
    pub objective: ObjectiveConfig,
    /// 4.1 测量质量门（默认关闭 → 旧行为逐位一致）
    pub quality: QualityConfig,
    /// 4.4 权威加权（默认关闭 → 旧行为逐位一致）
    pub authority: AuthorityConfig,
    /// 4.4 安全钳位 + 三态输出（默认关闭 → 旧行为逐位一致）
    pub safety: SafetyConfig,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            taps: 2048,
            delay_samples: 1024,
            irls_iterations: 2,
            lambda_b2_rel: 1e-6,
            lambda_grid_rel: vec![1e-6, 1e-5, 1e-4, 1e-3, 1e-2, 1e-1],
            target_beta: 0.70,
            diag_load_rel: 1e-9,
            cv_folds: 3,
            gain_cap_db: constraints::GAIN_CAP_DB,
            nfft: 65536,
            objective: ObjectiveConfig::default(),
            quality: QualityConfig::default(),
            authority: AuthorityConfig::default(),
            safety: SafetyConfig::default(),
        }
    }
}

/// 场景输入（由 CLI 从 room-sim 夹具转换；与 room-sim 类型解耦，便于纯算法单测）。
#[derive(Clone, Debug)]
pub struct SceneInput {
    pub scene_id: String,
    pub fixture_dir: String,
    pub ground_truth_sha256: String,
    pub seed: u64,
    pub rir_scale: f64,
    pub sample_rate_hz: f64,
    /// 精确频点（Hz），长度 K
    pub freqs_hz: Vec<f64>,
    pub source_ids: Vec<String>,
    pub mic_ids: Vec<String>,
    /// design 测点在 mic_ids 中的索引
    pub design_mics: Vec<usize>,
    /// holdout 测点在 mic_ids 中的索引
    pub holdout_mics: Vec<usize>,
    /// H[mic][bin][source]
    pub h: Vec<Vec<Vec<Cx>>>,
    /// evaluated spec 1/3 oct 中心（Hz）
    pub band_centers_hz: Vec<f64>,
    /// extended 集合（仅报告）
    pub extended_centers_hz: Vec<f64>,
}

/// 场景运行结果。
pub struct SceneRun {
    pub report: Json,
    pub fir_files: Vec<(String, String)>,
    pub metrics_csv: String,
}

#[derive(Clone, Debug, Default)]
struct DesignBlock {
    r_k: Vec<Option<f64>>,
    e_k0: Vec<f64>,
    alpha: Vec<f64>,
    e_modal: f64,
    e_modal_db: f64,
    e_modal_eq: f64,
    e_modal_eq_db: f64,
    r_max: Option<f64>,
    argmax_band_hz: Option<f64>,
    band_delta_db: Vec<Option<f64>>,
    energy_migration: bool,
    spatial: Vec<SpatialStats>,
    per_point_band_db: Vec<(String, Vec<Option<f64>>)>,
    obj: objective::ObjMetrics,
}

#[derive(Clone, Debug, Default)]
struct HoldoutBlock {
    r_k: Vec<Option<f64>>,
    e_modal: f64,
    e_modal_db: f64,
    e_modal_eq: f64,
    e_modal_eq_db: f64,
    r_max: Option<f64>,
    argmax_band_hz: Option<f64>,
    band_delta_db: Vec<Option<f64>>,
    energy_migration: bool,
    per_point_band_db: Vec<(String, Vec<Option<f64>>)>,
    point_weighted_db: Vec<(String, f64)>,
    consistency_c: f64,
    valid_holdout_count: usize,
    severe_regression: bool,
    severe_points: Vec<String>,
    improved_points: Vec<String>,
    spatial: Vec<SpatialStats>,
    obj: objective::ObjMetrics,
}

#[derive(Clone, Debug)]
struct FoldScore {
    lambda_rel: f64,
    mean_e_modal_db: f64,
    fold_e_modal_db: Vec<f64>,
}

fn select_rows<T: Clone>(v: &[T], idx: &[usize]) -> Vec<T> {
    idx.iter().map(|i| v[*i].clone()).collect()
}

fn mean_h_sq(h_design: &[Vec<Vec<Cx>>]) -> f64 {
    let mut sum = 0.0;
    let mut cnt = 0usize;
    for mic in h_design {
        for hk in mic {
            for v in hk {
                sum += v.norm_sqr();
                cnt += 1;
            }
        }
    }
    if cnt == 0 {
        f64::NAN
    } else {
        sum / cnt as f64
    }
}

/// 计算一个 variant 的 design/holdout 指标块。
#[allow(clippy::too_many_arguments)]
fn eval_variant(
    p_after: &[Vec<Cx>],
    p_before: &[Vec<Cx>],
    design_mics: &[usize],
    holdout_mics: &[usize],
    mic_ids: &[String],
    plan: &BandPlan,
    alpha: &[f64],
    e_k0: &[f64],
) -> (DesignBlock, HoldoutBlock) {
    let after_design = evaluate::band_energies(p_after, design_mics, plan);
    let em = evaluate::emodal_from_energies(e_k0, &after_design, alpha);
    let max_e0 = e_k0.iter().cloned().fold(0.0f64, f64::max);
    let band_delta_db: Vec<Option<f64>> = (0..plan.len())
        .map(|b| {
            if e_k0[b] > 1e-12 * max_e0 {
                Some(evaluate::delta_db(e_k0[b], after_design[b]))
            } else {
                None
            }
        })
        .collect();
    let spatial = evaluate::spatial_stats_all(p_after, design_mics, plan);
    let per_point = evaluate::point_band_deltas_db(p_before, p_after, design_mics, plan);
    let per_point_band_db = design_mics
        .iter()
        .enumerate()
        .map(|(i, m)| (mic_ids[*m].clone(), per_point[i].clone()))
        .collect();
    let design = DesignBlock {
        r_k: em.r_k.clone(),
        e_k0: e_k0.to_vec(),
        alpha: alpha.to_vec(),
        e_modal: em.e_modal,
        e_modal_db: em.e_modal_db,
        e_modal_eq: em.e_modal_eq,
        e_modal_eq_db: em.e_modal_eq_db,
        r_max: em.r_max,
        argmax_band_hz: em.argmax_band.map(|b| plan.centers_hz[b]),
        band_delta_db,
        energy_migration: em.energy_migration,
        spatial,
        per_point_band_db,
        obj: objective::metrics_for_points(p_after, p_before, design_mics, plan, e_k0, alpha),
    };
    // ---- holdout：before = B0 在 holdout 点集上的每带能量 ----
    let before_h = evaluate::band_energies(p_before, holdout_mics, plan);
    let after_h = evaluate::band_energies(p_after, holdout_mics, plan);
    let emh = evaluate::emodal_from_energies(&before_h, &after_h, alpha);
    let max_e0h = before_h.iter().cloned().fold(0.0f64, f64::max);
    let band_delta_h: Vec<Option<f64>> = (0..plan.len())
        .map(|b| {
            if before_h[b] > 1e-12 * max_e0h {
                Some(evaluate::delta_db(before_h[b], after_h[b]))
            } else {
                None
            }
        })
        .collect();
    let hev = evaluate::holdout_evaluate(p_before, p_after, holdout_mics, plan, alpha);
    let spatial_h = evaluate::spatial_stats_all(p_after, holdout_mics, plan);
    let per_point_h: Vec<(String, Vec<Option<f64>>)> = hev
        .points
        .iter()
        .map(|pt| (mic_ids[pt.mic_index].clone(), pt.band_delta_db.clone()))
        .collect();
    let point_weighted: Vec<(String, f64)> = hev
        .points
        .iter()
        .map(|pt| (mic_ids[pt.mic_index].clone(), pt.weighted_mean_delta_db))
        .collect();
    let severe_points: Vec<String> = {
        let mut v: Vec<String> = hev
            .severe_cases
            .iter()
            .map(|(m, _, _)| mic_ids[*m].clone())
            .collect();
        v.sort();
        v.dedup();
        v
    };
    let improved_points: Vec<String> = hev
        .points
        .iter()
        .filter(|p| p.improved)
        .map(|p| mic_ids[p.mic_index].clone())
        .collect();
    let holdout_obj = objective::metrics_for_points(
        p_after,
        p_before,
        holdout_mics,
        plan,
        &evaluate::band_energies(p_before, holdout_mics, plan),
        alpha,
    );
    let holdout = HoldoutBlock {
        r_k: emh.r_k.clone(),
        e_modal: emh.e_modal,
        e_modal_db: emh.e_modal_db,
        e_modal_eq: emh.e_modal_eq,
        e_modal_eq_db: emh.e_modal_eq_db,
        r_max: emh.r_max,
        argmax_band_hz: emh.argmax_band.map(|b| plan.centers_hz[b]),
        band_delta_db: band_delta_h,
        energy_migration: emh.energy_migration,
        per_point_band_db: per_point_h,
        point_weighted_db: point_weighted,
        consistency_c: hev.consistency,
        valid_holdout_count: hev.valid_points,
        severe_regression: hev.severe_regression,
        severe_points,
        improved_points,
        spatial: spatial_h,
        obj: holdout_obj,
    };
    (design, holdout)
}

/// 3 折 design-only CV 选 λ（holdout 绝不参与）。
fn cv_select_lambda(
    h_design: &[Vec<Vec<Cx>>],
    p_b0_design: &[Vec<Cx>],
    freqs_hz: &[f64],
    fs: f64,
    plan: &BandPlan,
    e_k0: &[f64],
    alpha: &[f64],
    cfg: &RunConfig,
    mean_h_sq: f64,
    opts: DesignOptions<'_>,
) -> Result<(Vec<FoldScore>, f64), String> {
    let n = h_design.len();
    let folds = cfg.cv_folds.max(2).min(n);
    if folds < 2 {
        return Err("CV 需要至少 2 个 design 点".to_string());
    }
    let mut scores = Vec::with_capacity(cfg.lambda_grid_rel.len());
    for &lambda_rel in &cfg.lambda_grid_rel {
        let lambda_abs = lambda_rel * mean_h_sq;
        let mut fold_db = Vec::with_capacity(folds);
        for f in 0..folds {
            let train: Vec<usize> = (0..n).filter(|i| i % folds != f).collect();
            let val: Vec<usize> = (0..n).filter(|i| i % folds == f).collect();
            if train.is_empty() || val.is_empty() {
                continue;
            }
            let h_train = select_rows(h_design, &train);
            let p0_train = select_rows(p_b0_design, &train);
            let a_train = target::build_target(freqs_hz, &p0_train, cfg.target_beta);
            let design = solve::design_mimo_irls_with(
                freqs_hz,
                fs,
                &h_train,
                &a_train,
                lambda_abs,
                cfg.taps,
                cfg.delay_samples,
                cfg.diag_load_rel,
                cfg.irls_iterations,
                cfg.nfft,
                opts,
            )?;
            let h_val = select_rows(h_design, &val);
            let p_val_after = solve::pressure(&h_val, &design.g_actual);
            let local: Vec<usize> = (0..val.len()).collect();
            let after = evaluate::band_energies(&p_val_after, &local, plan);
            // 统一口径：CV E_modal 也用 B0(design) 的 E_k0 与 α（跨折可比、与主报告一致）
            let em = evaluate::emodal_from_energies(e_k0, &after, alpha);
            fold_db.push(em.e_modal_db);
        }
        if fold_db.is_empty() {
            continue;
        }
        let finite: Vec<f64> = fold_db.iter().cloned().filter(|v| v.is_finite()).collect();
        let mean = if finite.is_empty() {
            f64::INFINITY
        } else {
            finite.iter().sum::<f64>() / finite.len() as f64
        };
        scores.push(FoldScore {
            lambda_rel,
            mean_e_modal_db: mean,
            fold_e_modal_db: fold_db,
        });
    }
    if scores.is_empty() {
        return Err("CV 没有可用 λ 分数".to_string());
    }
    // 选 CV E_modal 最小；并列时取网格中更小 λ（先出现者）
    let mut best = scores[0].clone();
    for s in scores.iter().skip(1) {
        if s.mean_e_modal_db < best.mean_e_modal_db - 1e-12 {
            best = s.clone();
        }
    }
    Ok((scores, best.lambda_rel))
}

fn spatial_json(stats: &[SpatialStats]) -> Json {
    Json::Arr(
        stats
            .iter()
            .map(|s| {
                Json::Obj(vec![
                    ("center_hz".to_string(), Json::num(s.center_hz)),
                    ("mean_db".to_string(), Json::num(s.mean_db)),
                    ("std_db".to_string(), Json::num(s.std_db)),
                    ("range_db".to_string(), Json::num(s.range_db)),
                    ("p90_p10_db".to_string(), Json::num(s.p90_p10_db)),
                    (
                        "coverage_within_3db".to_string(),
                        Json::num(s.coverage_within_3db),
                    ),
                    ("worst_null_db".to_string(), Json::num(s.worst_null_db)),
                    (
                        "max_peak_above_mean_db".to_string(),
                        Json::num(s.max_peak_above_mean_db),
                    ),
                    (
                        "interference_null_db".to_string(),
                        Json::num(s.interference_null_db),
                    ),
                ])
            })
            .collect(),
    )
}

fn opt_arr(v: &[Option<f64>]) -> Json {
    Json::Arr(v.iter().map(|x| Json::opt_num(*x)).collect())
}

fn per_point_json(points: &[(String, Vec<Option<f64>>)]) -> Json {
    Json::Obj(
        points
            .iter()
            .map(|(id, vals)| (id.clone(), opt_arr(vals)))
            .collect(),
    )
}

fn weighted_json(points: &[(String, f64)]) -> Json {
    Json::Obj(
        points
            .iter()
            .map(|(id, v)| (id.clone(), Json::num(*v)))
            .collect(),
    )
}

/// 座位离散量序列化：无定义（design 点 < 2）时写 null（M=1 显式退化，不伪造 0 值成功）。
fn seat_std_json(defined: bool, value: f64) -> Json {
    if defined {
        Json::num(value)
    } else {
        Json::Null
    }
}

fn design_json(d: &DesignBlock) -> Json {
    Json::Obj(vec![
        ("r_k".to_string(), opt_arr(&d.r_k)),
        (
            "e_k0".to_string(),
            Json::Arr(d.e_k0.iter().map(|v| Json::num(*v)).collect()),
        ),
        (
            "alpha".to_string(),
            Json::Arr(d.alpha.iter().map(|v| Json::num(*v)).collect()),
        ),
        ("e_modal".to_string(), Json::num(d.e_modal)),
        ("e_modal_db".to_string(), Json::num(d.e_modal_db)),
        ("e_modal_eq".to_string(), Json::num(d.e_modal_eq)),
        ("e_modal_eq_db".to_string(), Json::num(d.e_modal_eq_db)),
        ("r_max".to_string(), Json::opt_num(d.r_max)),
        (
            "argmax_band_hz".to_string(),
            Json::opt_num(d.argmax_band_hz),
        ),
        ("band_delta_db".to_string(), opt_arr(&d.band_delta_db)),
        (
            "energy_migration".to_string(),
            Json::Bool(d.energy_migration),
        ),
        (
            "seat_std_mean_db".to_string(),
            seat_std_json(d.obj.seat_std_defined, d.obj.seat_std_mean_db),
        ),
        (
            "seat_std_max_db".to_string(),
            seat_std_json(d.obj.seat_std_defined, d.obj.seat_std_max_db),
        ),
        ("max_dev_db".to_string(), Json::num(d.obj.max_dev_db)),
        (
            "null_depth_mean_db".to_string(),
            Json::num(d.obj.null_depth_mean_db),
        ),
        (
            "null_depth_worst_db".to_string(),
            Json::num(d.obj.null_depth_worst_db),
        ),
        ("spatial".to_string(), spatial_json(&d.spatial)),
        (
            "per_point_band_db".to_string(),
            per_point_json(&d.per_point_band_db),
        ),
    ])
}

fn holdout_json(h: &HoldoutBlock) -> Json {
    Json::Obj(vec![
        ("r_k".to_string(), opt_arr(&h.r_k)),
        ("e_modal".to_string(), Json::num(h.e_modal)),
        ("e_modal_db".to_string(), Json::num(h.e_modal_db)),
        ("e_modal_eq".to_string(), Json::num(h.e_modal_eq)),
        ("e_modal_eq_db".to_string(), Json::num(h.e_modal_eq_db)),
        ("r_max".to_string(), Json::opt_num(h.r_max)),
        (
            "argmax_band_hz".to_string(),
            Json::opt_num(h.argmax_band_hz),
        ),
        ("band_delta_db".to_string(), opt_arr(&h.band_delta_db)),
        (
            "energy_migration".to_string(),
            Json::Bool(h.energy_migration),
        ),
        (
            "seat_std_mean_db".to_string(),
            seat_std_json(h.obj.seat_std_defined, h.obj.seat_std_mean_db),
        ),
        (
            "seat_std_max_db".to_string(),
            seat_std_json(h.obj.seat_std_defined, h.obj.seat_std_max_db),
        ),
        ("max_dev_db".to_string(), Json::num(h.obj.max_dev_db)),
        (
            "null_depth_mean_db".to_string(),
            Json::num(h.obj.null_depth_mean_db),
        ),
        (
            "null_depth_worst_db".to_string(),
            Json::num(h.obj.null_depth_worst_db),
        ),
        (
            "per_point_band_db".to_string(),
            per_point_json(&h.per_point_band_db),
        ),
        (
            "point_weighted_db".to_string(),
            weighted_json(&h.point_weighted_db),
        ),
        ("consistency_c".to_string(), Json::num(h.consistency_c)),
        (
            "valid_holdout_count".to_string(),
            Json::Num(h.valid_holdout_count as f64),
        ),
        (
            "severe_regression".to_string(),
            Json::Bool(h.severe_regression),
        ),
        (
            "severe_points".to_string(),
            Json::Arr(
                h.severe_points
                    .iter()
                    .map(|s| Json::str(s.clone()))
                    .collect(),
            ),
        ),
        (
            "improved_points".to_string(),
            Json::Arr(
                h.improved_points
                    .iter()
                    .map(|s| Json::str(s.clone()))
                    .collect(),
            ),
        ),
        ("spatial".to_string(), spatial_json(&h.spatial)),
    ])
}

fn fir_constraints_json(c: &FirConstraintReport, identity_energy_ratio_db: f64) -> Json {
    Json::Obj(vec![
        (
            "per_source_max_gain_db".to_string(),
            Json::Arr(
                c.per_source_max_gain_db
                    .iter()
                    .map(|v| Json::num(*v))
                    .collect(),
            ),
        ),
        (
            "peak_bound_per_source".to_string(),
            Json::Arr(
                c.peak_bound_per_source
                    .iter()
                    .map(|v| Json::num(*v))
                    .collect(),
            ),
        ),
        (
            "predicted_peak_per_source".to_string(),
            Json::Arr(
                c.predicted_peak_per_source
                    .iter()
                    .map(|v| Json::num(*v))
                    .collect(),
            ),
        ),
        ("global_scale".to_string(), Json::num(c.global_scale)),
        ("effort".to_string(), Json::num(c.effort)),
        (
            "effort_vs_baseline".to_string(),
            Json::num(c.effort_vs_baseline),
        ),
        (
            "total_delay_samples".to_string(),
            Json::Num(c.total_delay_samples as f64),
        ),
        ("applied".to_string(), Json::Bool(c.applied)),
        ("headroom_ok".to_string(), Json::Bool(c.headroom_ok)),
        (
            "identity_energy_ratio_db".to_string(),
            Json::num(identity_energy_ratio_db),
        ),
        (
            "gain_cap_scale".to_string(),
            Json::Arr(c.gain_cap_scale.iter().map(|v| Json::num(*v)).collect()),
        ),
        (
            "gain_cap_triggered".to_string(),
            Json::Bool(c.gain_cap_triggered),
        ),
        (
            "headroom_triggered".to_string(),
            Json::Bool(c.headroom_triggered),
        ),
    ])
}

fn scalar_constraints_json(c: &ScalarConstraintReport) -> Json {
    Json::Obj(vec![
        (
            "per_source_max_gain_db".to_string(),
            Json::Arr(
                c.per_source_max_gain_db
                    .iter()
                    .map(|v| Json::num(*v))
                    .collect(),
            ),
        ),
        (
            "peak_bound_per_source".to_string(),
            Json::Arr(
                c.peak_bound_per_source
                    .iter()
                    .map(|v| Json::num(*v))
                    .collect(),
            ),
        ),
        ("global_scale".to_string(), Json::num(c.global_scale)),
        ("effort".to_string(), Json::num(c.effort)),
        (
            "effort_vs_baseline".to_string(),
            Json::num(c.effort_vs_baseline),
        ),
        (
            "total_delay_samples".to_string(),
            Json::Num(c.total_delay_samples as f64),
        ),
        (
            "identity_energy_ratio_db".to_string(),
            Json::num(c.identity_energy_ratio_db),
        ),
        (
            "headroom_applied".to_string(),
            Json::Bool(c.headroom_applied),
        ),
    ])
}

fn fir_files_json(files: &[(String, String)], taps: usize, delay: usize) -> Json {
    Json::Arr(
        files
            .iter()
            .map(|(path, _)| {
                let src = path
                    .rsplit('/')
                    .next()
                    .unwrap_or(path)
                    .trim_end_matches(".csv")
                    .trim_start_matches("B2_")
                    .to_string();
                Json::Obj(vec![
                    ("source".to_string(), Json::str(src)),
                    ("in".to_string(), Json::Null),
                    ("out".to_string(), Json::Null),
                    ("path".to_string(), Json::str(path.clone())),
                    ("taps".to_string(), Json::Num(taps as f64)),
                    ("delay_samples".to_string(), Json::Num(delay as f64)),
                ])
            })
            .collect(),
    )
}

/// 运行一个场景。
pub fn run_scene(input: &SceneInput, cfg: &RunConfig) -> Result<SceneRun, String> {
    let n_src = input.source_ids.len();
    let n_mics = input.mic_ids.len();
    let k = input.freqs_hz.len();
    if n_src == 0 || n_mics == 0 || k == 0 {
        return Err(format!(
            "run_scene: 空输入 n_src={} n_mics={} K={}",
            n_src, n_mics, k
        ));
    }
    if input.h.len() != n_mics {
        return Err(format!(
            "run_scene: h.len()={} != n_mics={}",
            input.h.len(),
            n_mics
        ));
    }
    for (mi, mic) in input.h.iter().enumerate() {
        if mic.len() != k {
            return Err(format!(
                "run_scene: h[{}].len()={} != K={}",
                mi,
                mic.len(),
                k
            ));
        }
        for (ki, hk) in mic.iter().enumerate() {
            if hk.len() != n_src {
                return Err(format!(
                    "run_scene: h[{}][{}].len()={} != n_src={}",
                    mi,
                    ki,
                    hk.len(),
                    n_src
                ));
            }
        }
    }
    if input.design_mics.is_empty() || input.holdout_mics.is_empty() {
        return Err("run_scene: design/holdout 为空".to_string());
    }
    let fs = input.sample_rate_hz;
    let plan = BandPlan::from_centers(&input.band_centers_hz, &input.freqs_hz);
    for (b, bins) in plan.bins.iter().enumerate() {
        if bins.is_empty() {
            return Err(format!(
                "run_scene: evaluated band {} ({} Hz) 内无频点",
                b, plan.centers_hz[b]
            ));
        }
    }
    let design = &input.design_mics;
    let holdout = &input.holdout_mics;

    // ---- B0 ----
    let g_b0 = baseline::b0_gains(n_src, k);
    let p_b0 = solve::pressure(&input.h, &g_b0);
    let e_k0 = evaluate::band_energies(&p_b0, design, &plan);
    let alpha = evaluate::alpha_weights(&e_k0);
    let (b0_design, b0_holdout) = eval_variant(
        &p_b0,
        &p_b0,
        design,
        holdout,
        &input.mic_ids,
        &plan,
        &alpha,
        &e_k0,
    );
    let b0_constraints = constraints::report_identity(n_src, 0.0);

    // ---- 4.1 测量质量门：Unknown 频段不参与拟合（默认关闭 = 旧行为逐位一致）----
    let quality_gate: Option<QualityGate> = if cfg.quality.enabled {
        let gate = cfg.quality.gate()?;
        let mask = gate.bin_fit_mask(&input.freqs_hz);
        if !mask.iter().any(|fits| *fits) {
            return Err(format!(
                "run_scene: 质量门把全部 {} 个频点判为 Unknown（无可用拟合频段；min_snr_db={}）",
                input.freqs_hz.len(),
                gate.min_snr_db
            ));
        }
        let excluded = gate.excluded_for_plan(&plan);
        if excluded.iter().all(|e| *e) {
            return Err(
                "run_scene: 质量门的 Unknown 频段覆盖了全部 evaluated 1/3 oct 带".to_string(),
            );
        }
        Some(gate)
    } else {
        None
    };
    let fit_mask: Option<Vec<bool>> = quality_gate
        .as_ref()
        .map(|gate| gate.bin_fit_mask(&input.freqs_hz));

    // ---- B1 ----
    let h_design = select_rows(&input.h, design);
    let p_b0_design = select_rows(&p_b0, design);

    // ---- 4.4 权威加权：正则项 Γ_s(f) = Γ0/|H_s(f)|²（默认关闭）----
    let authority_weights: Option<AuthorityWeights> = if cfg.authority.enabled {
        Some(authority::authority_weights(
            &h_design,
            &input.freqs_hz,
            &cfg.authority,
        )?)
    } else {
        None
    };
    let design_opts = DesignOptions {
        fit_mask: fit_mask.as_deref(),
        ridge: authority_weights.as_ref().map(|w| w.gamma.as_slice()),
    };
    let b1 = baseline::run_b1(&h_design, &input.freqs_hz, fs, &plan, &p_b0_design);
    let g_b1 = baseline::gains_from_params(&b1.params, &input.freqs_hz, fs);
    let p_b1 = solve::pressure(&input.h, &g_b1);
    let (b1_design, b1_holdout) = eval_variant(
        &p_b1,
        &p_b0,
        design,
        holdout,
        &input.mic_ids,
        &plan,
        &alpha,
        &e_k0,
    );
    let b1_constraints =
        constraints::report_b1(&b1.params, b1.level_ratio_db, baseline::LEVEL_TOLERANCE_DB)?;

    // ---- B2/B3：按 objective 分派（emodal 保持 v1 冻结路径；其余 v2 目标 + 可行化）----
    let mut outcome = if cfg.objective.kind == ObjectiveKind::EModal {
        compute_emodal_variants(
            &input,
            &h_design,
            &p_b0_design,
            &p_b0,
            cfg,
            fs,
            &plan,
            &e_k0,
            &alpha,
            design_opts,
        )?
    } else {
        compute_v2_variants(
            &input,
            &h_design,
            &p_b0_design,
            &p_b0,
            cfg,
            fs,
            &plan,
            &e_k0,
            &alpha,
            design_opts,
        )?
    };
    // ---- 4.4 安全钳位 + FULL/PARTIAL/NONE（默认关闭 = 旧行为逐位一致）----
    if cfg.safety.enabled {
        apply_safety_limits(
            &mut outcome.b2,
            &input,
            cfg,
            fs,
            &plan,
            &p_b0,
            &e_k0,
            &alpha,
        )?;
        apply_safety_limits(
            &mut outcome.b3,
            &input,
            cfg,
            fs,
            &plan,
            &p_b0,
            &e_k0,
            &alpha,
        )?;
    }
    let p_b2 = &outcome.b2.p_after;
    let p_b3 = &outcome.b3.p_after;
    let (b2_design, b2_holdout) = eval_variant(
        p_b2,
        &p_b0,
        design,
        holdout,
        &input.mic_ids,
        &plan,
        &alpha,
        &e_k0,
    );
    let (b3_design, b3_holdout) = eval_variant(
        p_b3,
        &p_b0,
        design,
        holdout,
        &input.mic_ids,
        &plan,
        &alpha,
        &e_k0,
    );

    // 各变体 design 总能量相对 B0 的变化（evaluated 带内；B1 电平约束同口径）
    let e_b0_bands = evaluate::total_energy_in_bands(&p_b0, design, &plan);
    let ratio_db = |p_after: &[Vec<Cx>]| -> f64 {
        let e = evaluate::total_energy_in_bands(p_after, design, &plan);
        if e > 0.0 && e_b0_bands > 0.0 {
            10.0 * (e / e_b0_bands).log10()
        } else {
            f64::NAN
        }
    };
    let ratio_b2 = ratio_db(p_b2);
    let ratio_b3 = ratio_db(p_b3);
    let feas_json_b2 = feasibility_json(
        &outcome.b2.feasibility,
        &input.mic_ids,
        cfg.objective.max_db_rise_db,
    );
    let feas_json_b3 = feasibility_json(
        &outcome.b3.feasibility,
        &input.mic_ids,
        cfg.objective.max_db_rise_db,
    );
    let status_b0 = predicted_status(&b0_design, &b0_holdout, None, true);
    let status_b1 = predicted_status(&b1_design, &b1_holdout, None, false);
    // gain_cap 缩放不自动 FAIL（最终 FIR 已满足帽；constraints 里保留审计字段）
    let status_b2 = predicted_status(
        &b2_design,
        &b2_holdout,
        Some(&outcome.b2.feasibility),
        false,
    );
    let status_b3 = predicted_status(
        &b3_design,
        &b3_holdout,
        Some(&outcome.b3.feasibility),
        false,
    );

    // ---- 变体 JSON ----
    let b1_params_json = Json::Obj(vec![(
        "sources".to_string(),
        Json::Arr(
            b1.params
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    Json::Obj(vec![
                        ("id".to_string(), Json::str(input.source_ids[i].clone())),
                        (
                            "delay_samples".to_string(),
                            Json::Num(p.delay_samples as f64),
                        ),
                        ("gain_db".to_string(), Json::num(p.gain_db)),
                        ("polarity".to_string(), Json::Num(p.polarity)),
                    ])
                })
                .collect(),
        ),
    )]);
    let mut variants = Json::Obj(vec![
        (
            "B0".to_string(),
            Json::Obj(vec![
                (
                    "description".to_string(),
                    Json::str("RAW: G=1, no correction"),
                ),
                ("backend".to_string(), Json::str("none")),
                ("params".to_string(), Json::Obj(vec![])),
                ("objective".to_string(), variant_objective_json(&cfg.objective, None, None)),
                (
                    "feasibility".to_string(),
                    Json::Obj(vec![("applicable".to_string(), Json::Bool(false))]),
                ),
                ("predicted_status".to_string(), Json::str(status_b0)),
                ("design".to_string(), design_json(&b0_design)),
                ("holdout".to_string(), holdout_json(&b0_holdout)),
                (
                    "constraints".to_string(),
                    scalar_constraints_json(&b0_constraints),
                ),
                ("fir_files".to_string(), Json::Arr(vec![])),
            ]),
        ),
        (
            "B1".to_string(),
            Json::Obj(vec![
                (
                    "description".to_string(),
                    Json::str(
                        "delay/gain/polarity alignment, objective = mean spatial std (dB) over evaluated spec bands with |total design energy change| <= 1.0 dB vs B0 (legacy-baseline stand-in, NOT a PEQ fit; can worsen E_modal/holdout)",
                    ),
                ),
                ("backend".to_string(), Json::str("scalar-search")),
                ("params".to_string(), b1_params_json),
                ("objective".to_string(), variant_objective_json(&cfg.objective, None, None)),
                (
                    "feasibility".to_string(),
                    Json::Obj(vec![("applicable".to_string(), Json::Bool(false))]),
                ),
                ("predicted_status".to_string(), Json::str(status_b1)),
                ("design".to_string(), design_json(&b1_design)),
                ("holdout".to_string(), holdout_json(&b1_holdout)),
                (
                    "constraints".to_string(),
                    scalar_constraints_json(&b1_constraints),
                ),
                ("fir_files".to_string(), Json::Arr(vec![])),
            ]),
        ),
        (
            "B2".to_string(),
            Json::Obj(vec![
                (
                    "description".to_string(),
                    Json::str("MIMO FIR (lambda ~ 0), own IRLS + ridge per-frequency LS + causal FIR fit"),
                ),
                (
                    "objective".to_string(),
                    variant_objective_json(
                        &cfg.objective,
                        outcome.b2.target_level,
                        Some(outcome.b2.lambda_rel),
                    ),
                ),
                (
                    "feasibility".to_string(),
                    feas_json_b2.clone(),
                ),
                ("predicted_status".to_string(), Json::str(status_b2)),
                ("objective_value".to_string(), Json::num(outcome.b2.objective_value)),
                ("backend".to_string(), Json::str("own-ls")),
                (
                    "params".to_string(),
                    Json::Obj(vec![
                        (
                            "lambda_rel".to_string(),
                            Json::num(outcome.b2.lambda_rel),
                        ),
                        ("target_beta".to_string(), Json::num(cfg.target_beta)),
                        (
                            "target_level".to_string(),
                            Json::opt_num(outcome.b2.target_level),
                        ),
                    ]),
                ),
                (
                    "solver".to_string(),
                    Json::Obj(vec![
                        ("status".to_string(), Json::str("n/a")),
                        (
                            "note".to_string(),
                            Json::str("own IRLS+ridge per-frequency solve + causal FIR fit"),
                        ),
                    ]),
                ),
                ("design".to_string(), design_json(&b2_design)),
                ("holdout".to_string(), holdout_json(&b2_holdout)),
                ("constraints".to_string(), fir_constraints_json(&outcome.b2.constraints, ratio_b2)),
                (
                    "fir_files".to_string(),
                    fir_files_json(&outcome.b2.fir_files, cfg.taps, cfg.delay_samples),
                ),
            ]),
        ),
        (
            "B3".to_string(),
            Json::Obj(vec![
                (
                    "description".to_string(),
                    Json::str("MIMO FIR + design-only CV ridge + OBJECTIVES-V2 target/feasibility ladder"),
                ),
                (
                    "objective".to_string(),
                    variant_objective_json(
                        &cfg.objective,
                        outcome.b3.target_level,
                        Some(outcome.b3.lambda_rel),
                    ),
                ),
                (
                    "feasibility".to_string(),
                    feas_json_b3.clone(),
                ),
                ("predicted_status".to_string(), Json::str(status_b3)),
                ("objective_value".to_string(), Json::num(outcome.b3.objective_value)),
                ("backend".to_string(), Json::str("own-ls")),
                (
                    "params".to_string(),
                    Json::Obj(vec![
                        ("lambda_rel".to_string(), Json::num(outcome.b3.lambda_rel)),
                        ("target_beta".to_string(), Json::num(cfg.target_beta)),
                        (
                            "target_level".to_string(),
                            Json::opt_num(outcome.b3.target_level),
                        ),
                    ]),
                ),
                (
                    "solver".to_string(),
                    Json::Obj(vec![
                        ("status".to_string(), Json::str("n/a")),
                        (
                            "note".to_string(),
                            Json::str("own IRLS+ridge per-frequency solve + causal FIR fit"),
                        ),
                    ]),
                ),
                ("design".to_string(), design_json(&b3_design)),
                ("holdout".to_string(), holdout_json(&b3_holdout)),
                ("constraints".to_string(), fir_constraints_json(&outcome.b3.constraints, ratio_b3)),
                (
                    "fir_files".to_string(),
                    fir_files_json(&outcome.b3.fir_files, cfg.taps, cfg.delay_samples),
                ),
            ]),
        ),
    ]);
    // 4.4 三态：B2/B3 各自的 FULL/PARTIAL/NONE + 逐项残差（仅在安全钳位启用时出现）
    if cfg.safety.enabled {
        for (key, variant) in [("B2", &outcome.b2), ("B3", &outcome.b3)] {
            if let Some(safety) = &variant.safety {
                add_variant_field(
                    &mut variants,
                    key,
                    "outcome",
                    Json::str(safety.state.as_str()),
                );
                add_variant_field(
                    &mut variants,
                    key,
                    "safety",
                    safety.json(&input.source_ids, &plan),
                );
            }
        }
    }

    // ---- 判据/诚实状态（独立输出，互不掩盖）----
    let delta_b2 = b1_design.e_modal_db - b2_design.e_modal_db;
    let delta_b3 = b1_design.e_modal_db - b3_design.e_modal_db;
    let worst_improved_b3 = match (b3_design.r_max, b1_design.r_max) {
        (Some(a), Some(b)) => a < b,
        _ => false,
    };
    let judgement = Json::Obj(vec![
        (
            "spec_status".to_string(),
            Json::str("UNPROVEN"),
        ),
        (
            "measured".to_string(),
            Json::Bool(false),
        ),
        ("applied".to_string(), Json::Bool(false)),
        (
            "delta_mimo_vs_b1_e_modal_db".to_string(),
            Json::Obj(vec![
                ("B2".to_string(), Json::num(delta_b2)),
                ("B3".to_string(), Json::num(delta_b3)),
            ]),
        ),
        (
            "mimo_gain_vs_delaygain_b1_ge_1db".to_string(),
            Json::Obj(vec![
                ("B2".to_string(), Json::Bool(delta_b2 >= 1.0)),
                ("B3".to_string(), Json::Bool(delta_b3 >= 1.0)),
            ]),
        ),
        (
            "worst_mode_improved_b3_vs_b1".to_string(),
            Json::Bool(worst_improved_b3),
        ),
        (
            "holdout_consistency_pass_b3".to_string(),
            Json::Bool(
                b3_holdout.valid_holdout_count >= 3
                    && b3_holdout.consistency_c.is_finite()
                    && b3_holdout.consistency_c >= evaluate::CONSISTENCY_MIN,
            ),
        ),
        (
            "holdout_severe_regression_b3".to_string(),
            Json::Bool(b3_holdout.severe_regression),
        ),
        (
            "b1_level_constraint_abs_db".to_string(),
            Json::num(b1_constraints.identity_energy_ratio_db.abs()),
        ),
        (
            "b1_level_constraint_within_1db".to_string(),
            Json::Bool(b1_constraints.identity_energy_ratio_db.abs() <= 1.0 + 1e-9),
        ),
        (
            "degenerate_solution_detected".to_string(),
            Json::Bool(
                outcome.b2.constraints.global_scale < 0.1
                    || outcome.b3.constraints.global_scale < 0.1
                    || outcome
                        .b2
                        .constraints
                        .per_source_max_gain_db
                        .iter()
                        .any(|d| !d.is_finite() || *d < -40.0)
                    || outcome
                        .b3
                        .constraints
                        .per_source_max_gain_db
                        .iter()
                        .any(|d| !d.is_finite() || *d < -40.0),
            ),
        ),
        (
            "criteria_note".to_string(),
            Json::str(
                "All values SIMULATED/UNPROVEN; B1 is a delay/gain/polarity stand-in, not a PEQ-only baseline; \
                 spec 6.1 delta is therefore only a PREDICTED comparison against the delay/gain baseline.",
            ),
        ),
    ]);

    let cv = outcome.cv.clone();

    let base_notes: Vec<&str> = vec![
        "SIMULATED / UNPROVEN: RoomForge shoebox ISM synthetic fixtures; no real room, no measurement, no device.",
        "PREDICTED only: no measured apply/remeasure; spec PASS is not claimed.",
        "B1 is a deterministic delay/gain/polarity coordinate-descent stand-in for legacy alignment, NOT the spec PEQ-only baseline (synthetic fixtures have no PEQ fit data).",
        "spec/mimo-objective-v1 evaluates 8 bands 40..200 Hz; this fixture sweep is 20-150 Hz, so only 6 spec bands (40,50,63,80,100,125 Hz) are evaluated (160/200 Hz N/A).",
        "design R_k denominators uniformly = B0(design) E_k0 and alpha = B0(design) alpha (per frozen pipeline convention).",
        "holdout R_k denominators / per-point before = B0 on the holdout point set (independent evaluation, spec 6.2); holdout never used for any selection.",
        "target A(f) = 0.70 * smoothed spatial RMS of B0 design pressure (power-domain 1/12 oct), zero phase; beta is a declared design choice, not a measurement.",
        "B2/B3 FIRs: 2048 taps @48k, common delay D=1024, real causal; exact-frequency response used for metrics (not ideal F).",
        "FIR gain cap <= +6 dB per source in-band and headroom global scale at max peak bound > 0.5 are applied and recorded; B1 scalar baseline is reported unscaled (legacy alignment is not a deliverable correction filter).",
        "CV E_modal uses the unified B0(design) denominators for cross-fold comparability.",
        "E_modal_db / E_modal_eq_db are normalized by sum(alpha over valid bands) / count(valid bands) so B0 = 0.00 dB (CONTRACT v2).",
        "FIR fit deviation from CONTRACT §6.4: the Toeplitz normal equations with mu=1e-9*g[0] are numerically ill-conditioned at K~69 << L=2048 (Levinson produced sum|h| ~ 3.5e4 -> headroom scaling -> silent fake improvement). Actual FIR design uses frequency-sampling IFFT on NFFT=65536 (in-band F interpolated in log-f; out-of-band pass-through exp(-j2pi f D/fs)); Levinson-Durbin remains implemented and unit-tested.",
        "Headroom deviation from CONTRACT §6.6: sum|h| is not used as a -6 dBFS peak bound (delta_D pass-through alone gives sum|h| >= 1). Declarative approximation used instead: predicted output peak = 0.5*max|G| <= 1.0 (equivalent to in-band max|G| <= +6 dB, same as gain cap); sum|h| still reported. No 0.5/sum|h| global scaling is applied (global_scale is 1.0; gain cap per source is the only scaling).",
        "B1 level constraint: |10log10(sum_design_in_evaluated_bands P_after / P_B0)| <= 1.0 dB; B1 optimizes spatial uniformity, not E_modal, so it may worsen E_modal / holdout severely - reported honestly and not hidden.",
    ];
    let mut all_notes: Vec<String> = base_notes.iter().map(|s| s.to_string()).collect();
    all_notes.push(format!(
        "OBJECTIVES-V2 objective = {}; beta = {:.3}; max_db_rise_db = {:.2}; IRLS iterations = {}; target_level ladder = {}.",
        objective_name(&cfg.objective),
        cfg.objective.beta,
        cfg.objective.max_db_rise_db,
        cfg.objective.irls_iterations,
        if cfg.objective.ladder_enabled {
            "enabled"
        } else {
            "disabled (emodal v1 reproduction path)"
        }
    ));
    if cfg.objective.kind.is_band_target() {
        all_notes.push(format!(
            "band-constant target A_b = {:.3} * mean_m sqrt(mean_{{f in B_b}} |P0_m(f)|^2); per-(mic,freq) IRLS weights = {}; selected target_level(B3) = {:?}; p_norm = {}.",
            cfg.objective.beta,
            objective_name(&cfg.objective),
            outcome.b3.target_level,
            cfg.objective.p_norm
        ));
    }
    all_notes.push(format!(
        "holdout-safe feasibility: max_db_rise_db = {:.2}; B2 feasible = {}, B3 feasible = {}; ladder levels = {:?}; holdout never used for selection.",
        cfg.objective.max_db_rise_db,
        outcome.b2.feasibility.feasible,
        outcome.b3.feasibility.feasible,
        cfg.objective.ladder_levels()
    ));
    all_notes.push(
        "predicted_status priority (frozen): severe > INFEASIBLE(feasibility) > PREDICTED_PASS > PREDICTED_PARTIAL > INCOMPLETE; B0 = BASELINE; B1 by metric rules only. gain_cap scaling does NOT auto-FAIL (kept in constraints for audit). All values SIMULATED / UNPROVEN (predicted only)."
            .to_string(),
    );
    if let Some(gate) = &quality_gate {
        all_notes.push(format!(
            "4.1 quality gate: min_snr_db = {:.2}; {} of {} 1/3-oct bands Unknown (excluded from fitting, identity F=1 there); Unknown != insufficient authority (see authority block). Excluded: {:?}.",
            gate.min_snr_db,
            gate.excluded_bands().len(),
            gate.len(),
            gate.excluded_bands()
                .iter()
                .map(|b| b.center_hz)
                .collect::<Vec<f64>>()
        ));
    }
    if let Some(w) = &authority_weights {
        all_notes.push(format!(
            "4.4 authority weighting: ON (ridge term lambda*sum_s Gamma_s(f)|f_s|^2, Gamma_s(f) = passband_mean|H_s|^2 / |H_s(f)|^2); gamma range {:.3e}..{:.3e}; default OFF is bit-identical.",
            w.gamma.iter().flatten().cloned().fold(f64::INFINITY, f64::min),
            w.gamma.iter().flatten().cloned().fold(0.0f64, f64::max)
        ));
    }
    if cfg.safety.enabled {
        let st = outcome
            .b3
            .safety
            .as_ref()
            .map(|s| s.state.as_str())
            .unwrap_or("NONE");
        all_notes.push(format!(
            "4.4 safety clamp: hard limits band boost <= +{:.1} dB, positive gain sum <= +{:.1} dB, predicted peak <= {:.1} dBFS; B3 outcome = {}. FULL = no clamp and target met; PARTIAL = clamped or target residuals (listed per item); NONE = no feasible solution inside the safety limits. Safety limits are never crossed.",
            cfg.safety.max_band_boost_db, cfg.safety.max_positive_sum_db, cfg.safety.max_predicted_peak_dbfs, st
        ));
    }

    if cfg.objective.kind == ObjectiveKind::SinglePoint {
        all_notes.push(format!(
            "4.3 single-point objective: value = w_e*e_modal_db + w_null*(-null_depth_mean_db); seat-dispersion term is structurally absent (w_std = 0); band target A_b and band constants reuse OBJECTIVES-V2 (beta_level = {:.3}, same band_targets/target_per_bin), IRLS weights = null-depth weights.",
            cfg.objective.beta
        ));
    }
    if design.len() < 2 {
        all_notes.push(
            "4.3 M=1 degenerate branch: only 1 design point -> seat_std undefined (reported null, value defined as 0, no NaN/Inf); inter-source null depth identically 0 dB (null_depth_defined = false); CV has no in/out folds -> B3 uses nominal target_level + B2 lambda (cv_skipped = true). Single-point valley control is carried by the null-depth IRLS weights 1+k_null*((A-|P|)/A)^2."
                .to_string(),
        );
    }

    let mut report_fields = vec![
        ("format".to_string(), Json::str(REPORT_FORMAT)),
        ("tool".to_string(), Json::str(TOOL_NAME)),
        ("scene_id".to_string(), Json::str(input.scene_id.clone())),
        (
            "provenance".to_string(),
            Json::Obj(vec![
                ("data_kind".to_string(), Json::str("SIMULATED")),
                ("honesty_status".to_string(), Json::str("UNPROVEN")),
                ("claim".to_string(), Json::str(CLAIM)),
            ]),
        ),
        (
            "fixture".to_string(),
            Json::Obj(vec![
                ("dir".to_string(), Json::str(input.fixture_dir.clone())),
                (
                    "ground_truth_sha256".to_string(),
                    Json::str(input.ground_truth_sha256.clone()),
                ),
                ("seed".to_string(), Json::Num(input.seed as f64)),
                ("rir_scale".to_string(), Json::num(input.rir_scale)),
            ]),
        ),
        (
            "scene".to_string(),
            Json::Obj(vec![
                ("n_sources".to_string(), Json::Num(n_src as f64)),
                ("n_mics".to_string(), Json::Num(n_mics as f64)),
                ("n_design".to_string(), Json::Num(design.len() as f64)),
                ("n_holdout".to_string(), Json::Num(holdout.len() as f64)),
                (
                    "source_ids".to_string(),
                    Json::Arr(input.source_ids.iter().map(|s| Json::str(s.clone())).collect()),
                ),
                (
                    "design_mic_ids".to_string(),
                    Json::Arr(
                        design
                            .iter()
                            .map(|m| Json::str(input.mic_ids[*m].clone()))
                            .collect(),
                    ),
                ),
                (
                    "holdout_mic_ids".to_string(),
                    Json::Arr(
                        holdout
                            .iter()
                            .map(|m| Json::str(input.mic_ids[*m].clone()))
                            .collect(),
                    ),
                ),
            ]),
        ),
        (
            "config".to_string(),
            Json::Obj(vec![
                ("sample_rate_hz".to_string(), Json::num(fs)),
                ("taps".to_string(), Json::Num(cfg.taps as f64)),
                ("delay_samples".to_string(), Json::Num(cfg.delay_samples as f64)),
                ("lambda_b2_rel".to_string(), Json::num(cfg.lambda_b2_rel)),
                ("lambda_b3_rel".to_string(), Json::num(outcome.b3.lambda_rel)),
                ("objective".to_string(), objective_config_json(&cfg.objective, cfg.target_beta)),
                (
                    "lambda_grid_rel".to_string(),
                    Json::Arr(cfg.lambda_grid_rel.iter().map(|v| Json::num(*v)).collect()),
                ),
                ("irls_iterations".to_string(), Json::Num(cfg.irls_iterations as f64)),
                ("cv_folds".to_string(), Json::Num(cfg.cv_folds as f64)),
                ("target_beta".to_string(), Json::num(cfg.target_beta)),
                (
                    "target_mode".to_string(),
                    Json::str("design-spatial-rms-smoothed"),
                ),
                ("diag_load_rel".to_string(), Json::num(cfg.diag_load_rel)),
                ("gain_cap_db".to_string(), Json::num(cfg.gain_cap_db)),
                ("nfft".to_string(), Json::Num(cfg.nfft as f64)),
                (
                    "fir_fit_method".to_string(),
                    Json::str("frequency-sampling IFFT (band-limited; Toeplitz/Levinson numerically ill-conditioned at K<<L)"),
                ),
                (
                    "headroom_mode".to_string(),
                    Json::str("declarative: 0.5*max|G| <= 1.0 (in-band max|G| <= +6 dB); no 0.5/sum|h| scaling"),
                ),
                ("e_modal_db_normalized".to_string(), Json::Bool(true)),
                (
                    "diag_load_rel_note".to_string(),
                    Json::str("Toeplitz/Levinson path keeps mu=1e-9*g[0]; actual FIR uses frequency sampling"),
                ),
                (
                    "band_plan".to_string(),
                    Json::Obj(vec![
                        (
                            "evaluated_spec_hz".to_string(),
                            Json::Arr(
                                input
                                    .band_centers_hz
                                    .iter()
                                    .map(|v| Json::num(*v))
                                    .collect(),
                            ),
                        ),
                        (
                            "extended_hz".to_string(),
                            Json::Arr(
                                input
                                    .extended_centers_hz
                                    .iter()
                                    .map(|v| Json::num(*v))
                                    .collect(),
                            ),
                        ),
                    ]),
                ),
            ]),
        ),
        ("variants".to_string(), variants),
        ("cv".to_string(), cv),
        ("judgement".to_string(), judgement),
        ("predicted_status".to_string(), Json::str(status_b3)),
        ("feasibility".to_string(), feas_json_b3.clone()),
        (
            "notes".to_string(),
            Json::Arr(all_notes.iter().map(|s| Json::str(s.clone())).collect()),
        ),
    ];
    // 4.1 / 4.4 报告块（默认关闭时不出现 → 旧报告逐位一致）
    if let Some(gate) = &quality_gate {
        report_fields.push(("quality".to_string(), gate.json()));
    }
    if let Some(w) = &authority_weights {
        report_fields.push((
            "authority".to_string(),
            w.json(&cfg.authority, &input.source_ids),
        ));
    }
    if cfg.safety.enabled {
        let state = outcome
            .b3
            .safety
            .as_ref()
            .map(|s| s.state.as_str())
            .unwrap_or("NONE");
        report_fields.push(("outcome".to_string(), Json::str(state)));
        report_fields.push(("outcome_variant".to_string(), Json::str("B3")));
    }
    // 4.3 目标报告：目标指标（衰减/谷深）与三态（FULL/PARTIAL/NONE + 残差）同块输出，
    // 让调用者一眼分开"算法认为修到什么程度"与"限值允许修到什么程度"。
    // 仅在 single_point 或启用安全钳位时出现 → 默认 emodal 报告逐位不变。
    if cfg.safety.enabled || cfg.objective.kind == ObjectiveKind::SinglePoint {
        report_fields.push((
            "objective_outcome".to_string(),
            objective_outcome_json(
                cfg,
                &outcome.b3,
                &b3_design.obj,
                outcome.b3.objective_value,
                input.design_mics.len(),
            ),
        ));
    }
    let report = Json::Obj(report_fields);

    // ---- metrics.csv：每 variant × 点 × 带一行 ----
    let mut metrics_csv = String::new();
    metrics_csv.push_str(
        "scene,variant,role,point_id,band_center_hz,before_energy,after_energy,delta_db\n",
    );
    let all_points: Vec<usize> = design.iter().chain(holdout.iter()).cloned().collect();
    let before_energies = evaluate::band_energies_per_mic(&p_b0, &all_points, &plan);
    let variants_after: Vec<(&str, &[Vec<Cx>])> = vec![
        ("B0", p_b0.as_slice()),
        ("B1", p_b1.as_slice()),
        ("B2", p_b2.as_slice()),
        ("B3", p_b3.as_slice()),
    ];
    for (variant_id, p_after) in variants_after {
        let after_energies = evaluate::band_energies_per_mic(p_after, &all_points, &plan);
        for (pi, &mic) in all_points.iter().enumerate() {
            let role = if design.contains(&mic) {
                "design"
            } else {
                "holdout"
            };
            for b in 0..plan.len() {
                let be = before_energies[pi][b];
                let ae = after_energies[pi][b];
                metrics_csv.push_str(&format!(
                    "{},{},{},{},{},{},{},{}\n",
                    report::csv_escape(&input.scene_id),
                    variant_id,
                    role,
                    report::csv_escape(&input.mic_ids[mic]),
                    report::fmt_g17(plan.centers_hz[b]),
                    report::fmt_g17(be),
                    report::fmt_g17(ae),
                    report::fmt_g17(evaluate::delta_db(be, ae)),
                ));
            }
        }
    }

    let mut fir_files = Vec::new();
    fir_files.extend(outcome.b2.fir_files.clone());
    fir_files.extend(outcome.b3.fir_files.clone());

    Ok(SceneRun {
        report,
        fir_files,
        metrics_csv,
    })
}

// ===========================================================================
// OBJECTIVES-V2：变体输出结构 / 可行化 / 目标分派
// ===========================================================================

/// 目标名（schema：emodal | seat_std | null_depth | combined | single_point）。
fn objective_name(obj: &ObjectiveConfig) -> &'static str {
    match obj.kind {
        ObjectiveKind::EModal => "emodal",
        ObjectiveKind::SeatStd => "seat_std",
        ObjectiveKind::NullDepth => "null_depth",
        ObjectiveKind::Combined => "combined",
        ObjectiveKind::SinglePoint => "single_point",
    }
}

fn objective_config_json(obj: &ObjectiveConfig, target_beta: f64) -> Json {
    Json::Obj(vec![
        ("name".to_string(), Json::str(objective_name(obj))),
        ("beta_emodal".to_string(), Json::num(target_beta)),
        (
            "beta_level".to_string(),
            if obj.kind.is_band_target() {
                Json::num(obj.beta)
            } else {
                Json::Null
            },
        ),
        ("p_norm".to_string(), Json::num(obj.p_norm)),
        ("maxdev_weight".to_string(), Json::num(obj.maxdev_weight)),
        ("w_std".to_string(), Json::num(obj.w_std)),
        ("w_null".to_string(), Json::num(obj.w_null)),
        ("w_e".to_string(), Json::num(obj.w_e)),
        ("k_null".to_string(), Json::num(obj.k_null)),
        ("max_db_rise_db".to_string(), Json::num(obj.max_db_rise_db)),
        (
            "irls_iterations".to_string(),
            Json::Num(obj.irls_iterations as f64),
        ),
    ])
}

fn variant_objective_json(
    obj: &ObjectiveConfig,
    target_level: Option<f64>,
    lambda_rel: Option<f64>,
) -> Json {
    let mut params = vec![("beta_emodal".to_string(), Json::num(obj.beta))];
    if let Some(lv) = target_level {
        params.push(("target_level".to_string(), Json::num(lv)));
        params.push(("beta_level".to_string(), Json::num(obj.beta)));
    }
    if let Some(lr) = lambda_rel {
        params.push(("lambda_rel".to_string(), Json::num(lr)));
    }
    if obj.kind == ObjectiveKind::SeatStd {
        params.push(("maxdev_weight".to_string(), Json::num(obj.maxdev_weight)));
    }
    if obj.kind.is_band_target() {
        params.push(("p_norm".to_string(), Json::num(obj.p_norm)));
        params.push(("k_null".to_string(), Json::num(obj.k_null)));
        params.push(("w_std".to_string(), Json::num(obj.w_std)));
        params.push(("w_null".to_string(), Json::num(obj.w_null)));
        params.push(("w_e".to_string(), Json::num(obj.w_e)));
    }
    Json::Obj(vec![
        ("name".to_string(), Json::str(objective_name(obj))),
        ("params".to_string(), Json::Obj(params)),
    ])
}

// ===========================================================================
// 4.3 单点目标 / 目标报告：目标指标（衰减/谷深）+ 三态（FULL/PARTIAL/NONE）+ 残差
// ===========================================================================

/// 目标指标 JSON：模态衰减（emodal 口径）+ 谷深（null-depth 口径）+ 座位离散（仅参考）。
fn target_metrics_json(m: &objective::ObjMetrics) -> Json {
    Json::Obj(vec![
        // 衰减：v1 同口径 E_modal（B0 = 0 dB；负值 = 能量下降/衰减到位）
        ("e_modal_db".to_string(), Json::num(m.e_modal_db)),
        ("r_max".to_string(), Json::opt_num(m.r_max)),
        // 谷深：10log10(min_m E_m,b / mean_m E_m,b)，0 dB = 无源间干涉谷（越接近 0 越好）
        (
            "null_depth_mean_db".to_string(),
            Json::num(m.null_depth_mean_db),
        ),
        (
            "null_depth_worst_db".to_string(),
            Json::num(m.null_depth_worst_db),
        ),
        (
            "null_depth_defined".to_string(),
            Json::Bool(m.null_depth_defined),
        ),
        // 座位离散：single_point 目标不含该权重；design 点 < 2 时无定义 → null
        (
            "seat_std_mean_db".to_string(),
            seat_std_json(m.seat_std_defined, m.seat_std_mean_db),
        ),
        (
            "seat_std_max_db".to_string(),
            seat_std_json(m.seat_std_defined, m.seat_std_max_db),
        ),
        (
            "seat_std_defined".to_string(),
            Json::Bool(m.seat_std_defined),
        ),
        ("max_dev_db".to_string(), Json::num(m.max_dev_db)),
    ])
}

/// 4.3 目标报告块：同一块里同时给出
/// - target_metrics：最终交付解（安全钳位后）的目标指标（衰减/谷深）→ "限值允许修到什么程度"；
/// - algorithm_metrics：安全钳位前的目标指标（算法自评）→ "算法认为修到什么程度"；
/// - outcome + residuals：FULL/PARTIAL/NONE 与逐项残差（安全限永不越过）；
/// - objective_value：钳位前/后的目标函数值。
///
/// 只新增键，不改既有键语义；默认（emodal + 安全关闭）不出现该块。
fn objective_outcome_json(
    cfg: &RunConfig,
    variant: &FirVariantResult,
    delivered_metrics: &objective::ObjMetrics,
    delivered_value: f64,
    n_design: usize,
) -> Json {
    let safety = variant.safety.as_ref();
    let mut notes: Vec<&str> = Vec::new();
    if n_design < 2 {
        notes.push(
            "design 点只有 1 个（单点退化）：seat_std 无定义（seat_std_defined=false → 字段 null），single_point 目标本就不含该权重；源间干涉 null 口径退化为 0 dB（null_depth_defined=false），单点谷深由 null-depth IRLS 权重承担。最终判据看 e_modal_db（衰减）与安全残差。",
        );
    }
    if safety.is_none() {
        notes.push(
            "safety 未启用：outcome=null、residuals=[]；target_metrics 即最终交付解指标（无钳位）。启用 --safety 可得到 FULL/PARTIAL/NONE + 逐项残差。",
        );
    } else {
        notes.push(
            "target_metrics = 安全钳位后的最终交付解；algorithm_metrics = 钳位前的算法自评；outcome/residuals = 限值允许修到什么程度（FULL=无钳位且达标；PARTIAL=撞限或残差；NONE=限内无解）。完整钳位明细见 variants.B3.safety。",
        );
    }
    let outcome = match safety {
        Some(s) => Json::str(s.state.as_str()),
        None => Json::Null,
    };
    let residuals = match safety {
        Some(s) => s.residuals_json(),
        None => Json::Arr(Vec::new()),
    };
    let safety_summary = match safety {
        Some(s) => Json::Obj(vec![
            ("never_exceeded".to_string(), Json::Bool(s.never_exceeded)),
            ("exhausted".to_string(), Json::Bool(s.exhausted)),
            (
                "target_deficit_max_db".to_string(),
                Json::num(s.target_deficit_max_db),
            ),
            ("pre_gain_db".to_string(), Json::num(s.pre_gain_db)),
            ("target_scale_db".to_string(), Json::num(s.target_scale_db)),
            ("final_guard_db".to_string(), Json::num(s.final_guard_db)),
            ("clamp_hits".to_string(), Json::Num(s.hits.len() as f64)),
        ]),
        None => Json::Null,
    };
    Json::Obj(vec![
        (
            "objective".to_string(),
            Json::str(objective_name(&cfg.objective)),
        ),
        ("variant".to_string(), Json::str("B3")),
        ("design_points".to_string(), Json::Num(n_design as f64)),
        (
            "single_point_degenerate".to_string(),
            Json::Bool(n_design < 2),
        ),
        (
            "target_metrics".to_string(),
            target_metrics_json(delivered_metrics),
        ),
        (
            "algorithm_metrics".to_string(),
            match &variant.pre_safety_metrics {
                Some(m) => target_metrics_json(m),
                None => Json::Null,
            },
        ),
        (
            "objective_value".to_string(),
            Json::Obj(vec![
                (
                    "algorithm".to_string(),
                    Json::opt_num(variant.pre_safety_objective_value),
                ),
                ("delivered".to_string(), Json::num(delivered_value)),
            ]),
        ),
        // 单点（M=1）只覆盖"皇帝位"：其余位置未验证（计划 5.3），不得据此声称空间效果。
        (
            "spatial_scope".to_string(),
            Json::str(if n_design < 2 {
                "SINGLE_POINT_NOT_SPATIAL"
            } else {
                "MULTI_POINT_DESIGN"
            }),
        ),
        ("outcome".to_string(), outcome),
        ("outcome_variant".to_string(), Json::str("B3")),
        ("safety".to_string(), safety_summary),
        ("residuals".to_string(), residuals),
        (
            "notes".to_string(),
            Json::Arr(notes.iter().map(|s| Json::str(*s)).collect()),
        ),
    ])
}
#[derive(Clone, Debug, Default)]
struct FeasibilityResult {
    design_check: objective::RiseCheck,
    cv_fold_max_rise_db: Vec<f64>,
    cv_violations: Vec<(usize, objective::RiseViolation)>,
    feasible: bool,
}

#[derive(Clone, Debug)]
struct FirVariantResult {
    p_after: Vec<Vec<Cx>>,
    /// 设计出的 FIR（[source][tap]；安全钳位输入/输出）
    h: Vec<Vec<f64>>,
    /// 逐频点目标幅度 A(f)（安全残差用）
    target_a: Vec<f64>,
    /// FIR 文件名前缀（"B2" / ""）
    fir_prefix: String,
    constraints: FirConstraintReport,
    fir_files: Vec<(String, String)>,
    target_level: Option<f64>,
    lambda_rel: f64,
    objective_value: f64,
    feasibility: FeasibilityResult,
    /// 4.4 安全钳位 + 三态报告（默认关闭时 None）
    safety: Option<SafetyReport>,
    /// 4.3 目标报告：安全钳位前的 design 目标指标（算法自评；仅安全钳位启用时 Some）
    pre_safety_metrics: Option<objective::ObjMetrics>,
    /// 安全钳位前的目标函数值（与 pre_safety_metrics 同源）
    pre_safety_objective_value: Option<f64>,
}

struct VariantOutcome {
    b2: FirVariantResult,
    b3: FirVariantResult,
    cv: Json,
}

fn feasibility_json(f: &FeasibilityResult, mic_ids: &[String], max_db_rise_db: f64) -> Json {
    let mut violations: Vec<Json> = f
        .design_check
        .violations
        .iter()
        .map(|v| {
            Json::Obj(vec![
                ("scope".to_string(), Json::str("design")),
                (
                    "point".to_string(),
                    Json::str(mic_ids.get(v.mic_index).cloned().unwrap_or_default()),
                ),
                ("band_center_hz".to_string(), Json::num(v.band_center_hz)),
                ("rise_db".to_string(), Json::num(v.rise_db)),
            ])
        })
        .collect();
    for (fidx, v) in &f.cv_violations {
        violations.push(Json::Obj(vec![
            ("scope".to_string(), Json::str(format!("cv_fold_{}", fidx))),
            (
                "point".to_string(),
                Json::str(mic_ids.get(v.mic_index).cloned().unwrap_or_default()),
            ),
            ("band_center_hz".to_string(), Json::num(v.band_center_hz)),
            ("rise_db".to_string(), Json::num(v.rise_db)),
        ]));
    }
    Json::Obj(vec![
        ("max_db_rise_db".to_string(), Json::num(max_db_rise_db)),
        (
            "design_max_rise_db".to_string(),
            Json::num(f.design_check.max_rise_db),
        ),
        (
            "cv_fold_max_rise_db".to_string(),
            Json::Arr(
                f.cv_fold_max_rise_db
                    .iter()
                    .map(|v| Json::num(*v))
                    .collect(),
            ),
        ),
        ("feasible".to_string(), Json::Bool(f.feasible)),
        ("violations".to_string(), Json::Arr(violations)),
    ])
}

/// 变体 predicted_status（冻结规则，优先级 severe > INFEASIBLE > PASS > PARTIAL > INCOMPLETE）。
fn predicted_status(
    design: &DesignBlock,
    holdout: &HoldoutBlock,
    feasibility: Option<&FeasibilityResult>,
    is_baseline: bool,
) -> &'static str {
    if is_baseline {
        return "BASELINE";
    }
    if holdout.severe_regression {
        return "PREDICTED_FAIL";
    }
    if let Some(f) = feasibility {
        if !f.feasible {
            return "INFEASIBLE";
        }
    }
    if design.e_modal_db < 0.0
        && design.r_max.map(|r| r < 1.0).unwrap_or(false)
        && !design.energy_migration
        && holdout.consistency_c.is_finite()
        && holdout.consistency_c >= evaluate::CONSISTENCY_MIN
        && holdout.valid_holdout_count >= 3
    {
        return "PREDICTED_PASS";
    }
    if holdout.valid_holdout_count < 3 {
        return "INCOMPLETE";
    }
    "PREDICTED_PARTIAL"
}

fn design_obj_value(
    p_after: &[Vec<Cx>],
    p_before: &[Vec<Cx>],
    mics: &[usize],
    plan: &BandPlan,
    e_k0: &[f64],
    alpha: &[f64],
    obj_cfg: &ObjectiveConfig,
) -> f64 {
    let m = objective::metrics_for_points(p_after, p_before, mics, plan, e_k0, alpha);
    obj_cfg.objective_value(&m)
}

fn make_fir_files(
    input: &SceneInput,
    cfg: &RunConfig,
    h: &[Vec<f64>],
    prefix: &str,
) -> Vec<(String, String)> {
    let mut files = Vec::with_capacity(h.len());
    for (i, src) in input.source_ids.iter().enumerate() {
        let path = if prefix.is_empty() {
            format!("fir/{}.csv", src)
        } else {
            format!("fir/{}_{}.csv", prefix, src)
        };
        files.push((
            path,
            report::fir_csv(src, &h[i], input.sample_rate_hz as u32, cfg.delay_samples),
        ));
    }
    files
}

/// 4.4 安全钳位 + 三态：对 variant 的最终解做投影，并用投影后的解重算
/// 压力 / 约束 / FIR 文件 / 可行性（安全限永不越过；不达标只报 PARTIAL/NONE）。
#[allow(clippy::too_many_arguments)]
fn apply_safety_limits(
    variant: &mut FirVariantResult,
    input: &SceneInput,
    cfg: &RunConfig,
    fs: f64,
    plan: &BandPlan,
    p_b0: &[Vec<Cx>],
    e_k0: &[f64],
    alpha: &[f64],
) -> Result<(), String> {
    let mut eval_mics: Vec<usize> = input.design_mics.clone();
    eval_mics.extend_from_slice(&input.holdout_mics);
    // 4.3 目标报告：先记录"算法自评"（安全钳位前的 design 目标指标 + 目标值），再钳位。
    // 这样 objective_outcome 块能同时给出"算法认为修到什么程度"与"限值允许修到什么程度"。
    let pre_metrics = objective::metrics_for_points(
        &variant.p_after,
        p_b0,
        &input.design_mics,
        plan,
        e_k0,
        alpha,
    );
    variant.pre_safety_objective_value = Some(cfg.objective.objective_value(&pre_metrics));
    variant.pre_safety_metrics = Some(pre_metrics);
    let clamp_input = limits::ClampInput {
        h: &variant.h,
        h_matrix: &input.h,
        p_b0,
        target_a: &variant.target_a,
        freqs_hz: &input.freqs_hz,
        fs,
        taps: cfg.taps,
        delay_samples: cfg.delay_samples,
        nfft: cfg.nfft,
        plan,
        design_mics: &input.design_mics,
        eval_mics: &eval_mics,
    };
    let (h_safe, _g_clamped, report) = limits::clamp_solution(&clamp_input, &cfg.safety)?;
    let (h_final, g_final, cons) = constraints::enforce_fir_constraints(
        &h_safe,
        &input.freqs_hz,
        fs,
        cfg.gain_cap_db,
        cfg.delay_samples,
        true,
    )?;
    variant.fir_files = make_fir_files(input, cfg, &h_final, &variant.fir_prefix);
    variant.h = h_final;
    variant.p_after = solve::pressure(&input.h, &g_final);
    variant.constraints = cons;
    variant.objective_value = design_obj_value(
        &variant.p_after,
        p_b0,
        &input.design_mics,
        plan,
        e_k0,
        alpha,
        &cfg.objective,
    );
    let check = objective::rise_check(
        &variant.p_after,
        p_b0,
        &input.design_mics,
        plan,
        cfg.objective.max_db_rise_db,
    );
    variant.feasibility.feasible = check.violations.is_empty()
        && variant
            .feasibility
            .cv_fold_max_rise_db
            .iter()
            .all(|r| *r <= cfg.objective.max_db_rise_db + 1e-9);
    variant.feasibility.design_check = check;
    variant.safety = Some(report);
    Ok(())
}

/// 给已构造的 variant JSON 追加字段（安全/三态块用）。
fn add_variant_field(variants: &mut Json, variant: &str, key: &str, value: Json) {
    if let Json::Obj(items) = variants {
        if let Some((_, Json::Obj(fields))) = items.iter_mut().find(|(k, _)| k == variant) {
            fields.push((key.to_string(), value));
        }
    }
}

/// v2：给定 per-bin 幅度目标与 objective IRLS 的 FIR 设计。
///
/// 幅度目标（seat-std/null-depth/combined）：首轮用相位 0 目标，随后用 phase-fixed 目标
/// target_m = A * P_m/|P_m|（只拉幅度、不强制相位对齐），避免把所有 mic 强制同相。
#[allow(clippy::too_many_arguments)]
fn design_objective_with_target(
    h_design: &[Vec<Vec<Cx>>],
    a_bin: &[f64],
    freqs_hz: &[f64],
    fs: f64,
    plan: &BandPlan,
    obj_cfg: &ObjectiveConfig,
    lambda_abs: f64,
    cfg: &RunConfig,
    opts: DesignOptions<'_>,
) -> Result<solve::FirDesign, String> {
    let n = h_design.len();
    let k = freqs_hz.len();
    let mut weights = vec![vec![1.0f64; k]; n];
    let mut targets: Vec<Vec<Cx>> = (0..n)
        .map(|_| a_bin.iter().map(|a| Cx::new(*a, 0.0)).collect())
        .collect();
    let iters = obj_cfg.irls_iterations.max(1);
    for iter in 0..iters {
        let d = if iter == 0 {
            solve::design_mimo_once_w_with(
                freqs_hz,
                fs,
                h_design,
                a_bin,
                &weights,
                lambda_abs,
                cfg.taps,
                cfg.delay_samples,
                cfg.diag_load_rel,
                cfg.nfft,
                opts,
            )?
        } else {
            solve::design_mimo_once_multi_targets_with(
                freqs_hz,
                fs,
                h_design,
                &targets,
                &weights,
                lambda_abs,
                cfg.taps,
                cfg.delay_samples,
                cfg.diag_load_rel,
                cfg.nfft,
                opts,
            )?
        };
        let p_after = solve::pressure(h_design, &d.g_actual);
        weights = objective::irls_weights(obj_cfg.kind, obj_cfg, &p_after, a_bin, plan);
        targets = objective::phase_fixed_targets(a_bin, &p_after);
    }
    solve::design_mimo_once_multi_targets_with(
        freqs_hz,
        fs,
        h_design,
        &targets,
        &weights,
        lambda_abs,
        cfg.taps,
        cfg.delay_samples,
        cfg.diag_load_rel,
        cfg.nfft,
        opts,
    )
}

/// v2：由 B0 design 压力与 target_level 生成目标，再设计 FIR。
#[allow(clippy::too_many_arguments)]
fn design_objective(
    h_design: &[Vec<Vec<Cx>>],
    p_b0_design: &[Vec<Cx>],
    freqs_hz: &[f64],
    fs: f64,
    plan: &BandPlan,
    obj_cfg: &ObjectiveConfig,
    target_level: f64,
    lambda_abs: f64,
    cfg: &RunConfig,
    opts: DesignOptions<'_>,
) -> Result<solve::FirDesign, String> {
    let a_band = objective::band_targets(p_b0_design, plan, target_level);
    let a_bin = objective::target_per_bin(freqs_hz, plan, &a_band);
    design_objective_with_target(
        h_design, &a_bin, freqs_hz, fs, plan, obj_cfg, lambda_abs, cfg, opts,
    )
}

/// emodal v1 路径的 CV 折 rise（供 feasibility 块；不改变 v1 数值）。
#[allow(clippy::too_many_arguments)]
fn emodal_fold_rises(
    h_design: &[Vec<Vec<Cx>>],
    p_b0_design: &[Vec<Cx>],
    freqs_hz: &[f64],
    fs: f64,
    plan: &BandPlan,
    cfg: &RunConfig,
    lambda_rel: f64,
    h_scale: f64,
    opts: DesignOptions<'_>,
) -> Result<(Vec<f64>, Vec<(usize, objective::RiseViolation)>), String> {
    let n = h_design.len();
    let folds = cfg.cv_folds.max(2).min(n);
    let lambda_abs = lambda_rel * h_scale;
    let mut rises = Vec::new();
    let mut violations = Vec::new();
    for f in 0..folds {
        let train: Vec<usize> = (0..n).filter(|i| i % folds != f).collect();
        let val: Vec<usize> = (0..n).filter(|i| i % folds == f).collect();
        if train.is_empty() || val.is_empty() {
            continue;
        }
        let h_train = select_rows(h_design, &train);
        let p0_train = select_rows(p_b0_design, &train);
        let a_train = target::build_target(freqs_hz, &p0_train, cfg.target_beta);
        let d = solve::design_mimo_irls_with(
            freqs_hz,
            fs,
            &h_train,
            &a_train,
            lambda_abs,
            cfg.taps,
            cfg.delay_samples,
            cfg.diag_load_rel,
            cfg.irls_iterations,
            cfg.nfft,
            opts,
        )?;
        let h_val = select_rows(h_design, &val);
        let p_val_before = select_rows(p_b0_design, &val);
        let p_val_after = solve::pressure(&h_val, &d.g_actual);
        let local: Vec<usize> = (0..h_val.len()).collect();
        let chk = objective::rise_check(
            &p_val_after,
            &p_val_before,
            &local,
            plan,
            cfg.objective.max_db_rise_db,
        );
        rises.push(chk.max_rise_db);
        for v in chk.violations {
            violations.push((f, v));
        }
    }
    Ok((rises, violations))
}

fn cv_json_v1(fold_scores: &[FoldScore], selected_lambda_rel: f64, cfg: &RunConfig) -> Json {
    Json::Obj(vec![
        ("folds".to_string(), Json::Num(cfg.cv_folds as f64)),
        (
            "fold_scores".to_string(),
            Json::Arr(
                fold_scores
                    .iter()
                    .map(|s| {
                        Json::Obj(vec![
                            ("lambda_rel".to_string(), Json::num(s.lambda_rel)),
                            ("mean_e_modal_db".to_string(), Json::num(s.mean_e_modal_db)),
                            (
                                "fold_e_modal_db".to_string(),
                                Json::Arr(
                                    s.fold_e_modal_db.iter().map(|v| Json::num(*v)).collect(),
                                ),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "selected_lambda_rel".to_string(),
            Json::num(selected_lambda_rel),
        ),
        (
            "lambda_grid_abs_note".to_string(),
            Json::str("lambda_abs = lambda_rel * mean_{m,n,k}|H|^2 (design points)"),
        ),
        (
            "cv_metric_note".to_string(),
            Json::str(
                "CV E_modal uses B0(design) E_k0 denominators and design alpha; holdout never used in CV",
            ),
        ),
    ])
}

#[allow(clippy::too_many_arguments)]
fn compute_emodal_variants(
    input: &SceneInput,
    h_design: &[Vec<Vec<Cx>>],
    p_b0_design: &[Vec<Cx>],
    p_b0: &[Vec<Cx>],
    cfg: &RunConfig,
    fs: f64,
    plan: &BandPlan,
    e_k0: &[f64],
    alpha: &[f64],
    opts: DesignOptions<'_>,
) -> Result<VariantOutcome, String> {
    let design = &input.design_mics;
    let a_target = target::build_target(&input.freqs_hz, p_b0_design, cfg.target_beta);
    let h_scale = mean_h_sq(h_design);
    if !(h_scale.is_finite() && h_scale > 0.0) {
        return Err(format!("run_scene: mean|H|²={} 非正/非有限", h_scale));
    }
    let max_rise = cfg.objective.max_db_rise_db;
    // ---- B2：固定 λ_rel ----
    let lambda_b2_abs = cfg.lambda_b2_rel * h_scale;
    let d2 = solve::design_mimo_irls_with(
        &input.freqs_hz,
        fs,
        h_design,
        &a_target,
        lambda_b2_abs,
        cfg.taps,
        cfg.delay_samples,
        cfg.diag_load_rel,
        cfg.irls_iterations,
        cfg.nfft,
        opts,
    )?;
    let (h2, g2, c2) = constraints::enforce_fir_constraints(
        &d2.h,
        &input.freqs_hz,
        fs,
        cfg.gain_cap_db,
        cfg.delay_samples,
        true,
    )?;
    let p_b2 = solve::pressure(&input.h, &g2);
    let objv2 = design_obj_value(&p_b2, p_b0, design, plan, e_k0, alpha, &cfg.objective);
    let check2 = objective::rise_check(&p_b2, p_b0, design, plan, max_rise);
    let feas2 = FeasibilityResult {
        feasible: check2.violations.is_empty(),
        design_check: check2,
        cv_fold_max_rise_db: vec![],
        cv_violations: vec![],
    };
    let files2 = make_fir_files(input, cfg, &h2, "B2");
    // ---- B3：CV 选 λ（v1 同路径）----
    let (fold_scores, lambda_b3_rel) = cv_select_lambda(
        h_design,
        p_b0_design,
        &input.freqs_hz,
        fs,
        plan,
        e_k0,
        alpha,
        cfg,
        h_scale,
        opts,
    )?;
    let lambda_b3_abs = lambda_b3_rel * h_scale;
    let d3 = solve::design_mimo_irls_with(
        &input.freqs_hz,
        fs,
        h_design,
        &a_target,
        lambda_b3_abs,
        cfg.taps,
        cfg.delay_samples,
        cfg.diag_load_rel,
        cfg.irls_iterations,
        cfg.nfft,
        opts,
    )?;
    let (h3, g3, c3) = constraints::enforce_fir_constraints(
        &d3.h,
        &input.freqs_hz,
        fs,
        cfg.gain_cap_db,
        cfg.delay_samples,
        true,
    )?;
    let p_b3 = solve::pressure(&input.h, &g3);
    let objv3 = design_obj_value(&p_b3, p_b0, design, plan, e_k0, alpha, &cfg.objective);
    let (fold_rises, fold_violations) = emodal_fold_rises(
        h_design,
        p_b0_design,
        &input.freqs_hz,
        fs,
        plan,
        cfg,
        lambda_b3_rel,
        h_scale,
        opts,
    )?;
    let check3 = objective::rise_check(&p_b3, p_b0, design, plan, max_rise);
    let mut feas3 = FeasibilityResult {
        design_check: check3,
        cv_fold_max_rise_db: fold_rises,
        cv_violations: fold_violations,
        feasible: false,
    };
    feas3.feasible = feas3.design_check.violations.is_empty()
        && feas3
            .cv_fold_max_rise_db
            .iter()
            .all(|r| *r <= max_rise + 1e-9);
    let files3 = make_fir_files(input, cfg, &h3, "");
    let cv = cv_json_v1(&fold_scores, lambda_b3_rel, cfg);
    Ok(VariantOutcome {
        b2: FirVariantResult {
            p_after: p_b2,
            h: h2,
            target_a: a_target.clone(),
            fir_prefix: "B2".to_string(),
            constraints: c2,
            fir_files: files2,
            target_level: None,
            lambda_rel: cfg.lambda_b2_rel,
            objective_value: objv2,
            feasibility: feas2,
            safety: None,
            pre_safety_metrics: None,
            pre_safety_objective_value: None,
        },
        b3: FirVariantResult {
            p_after: p_b3,
            h: h3,
            target_a: a_target,
            fir_prefix: String::new(),
            constraints: c3,
            fir_files: files3,
            target_level: None,
            lambda_rel: lambda_b3_rel,
            objective_value: objv3,
            feasibility: feas3,
            safety: None,
            pre_safety_metrics: None,
            pre_safety_objective_value: None,
        },
        cv,
    })
}

#[derive(Clone, Debug)]
struct V2Candidate {
    level: f64,
    lambda_rel: f64,
    mean_objective: f64,
    fold_objective: Vec<f64>,
    fold_rises: Vec<f64>,
    fold_violations: Vec<(usize, objective::RiseViolation)>,
    cv_feasible: bool,
}

#[allow(clippy::too_many_arguments)]
fn compute_v2_variants(
    input: &SceneInput,
    h_design: &[Vec<Vec<Cx>>],
    p_b0_design: &[Vec<Cx>],
    p_b0: &[Vec<Cx>],
    cfg: &RunConfig,
    fs: f64,
    plan: &BandPlan,
    e_k0: &[f64],
    alpha: &[f64],
    opts: DesignOptions<'_>,
) -> Result<VariantOutcome, String> {
    let design = &input.design_mics;
    let obj_cfg = &cfg.objective;
    let h_scale = mean_h_sq(h_design);
    if !(h_scale.is_finite() && h_scale > 0.0) {
        return Err(format!("run_scene: mean|H|²={} 非正/非有限", h_scale));
    }
    let max_rise = obj_cfg.max_db_rise_db;
    // ---- B2：固定 λ≈0、名义 target_level、不做 CV ----
    let level_b2 = obj_cfg.beta;
    let a_band_b2 = objective::band_targets(p_b0_design, plan, level_b2);
    let a_bin_b2 = objective::target_per_bin(&input.freqs_hz, plan, &a_band_b2);
    let d2 = design_objective(
        h_design,
        p_b0_design,
        &input.freqs_hz,
        fs,
        plan,
        obj_cfg,
        level_b2,
        cfg.lambda_b2_rel * h_scale,
        cfg,
        opts,
    )?;
    let (h2, g2, c2) = constraints::enforce_fir_constraints(
        &d2.h,
        &input.freqs_hz,
        fs,
        cfg.gain_cap_db,
        cfg.delay_samples,
        true,
    )?;
    let p_b2 = solve::pressure(&input.h, &g2);
    let objv2 = design_obj_value(&p_b2, p_b0, design, plan, e_k0, alpha, obj_cfg);
    let check2 = objective::rise_check(&p_b2, p_b0, design, plan, max_rise);
    let feas2 = FeasibilityResult {
        feasible: check2.violations.is_empty(),
        design_check: check2,
        cv_fold_max_rise_db: vec![],
        cv_violations: vec![],
    };
    let files2 = make_fir_files(input, cfg, &h2, "B2");
    // ---- B3：target_level 阶梯 × λ 网格，design-only 3 折 CV + rise 可行性 ----
    let levels = obj_cfg.ladder_levels();
    let n = h_design.len();
    let folds = cfg.cv_folds.max(2).min(n);
    // 计划 4.3 显式退化分支：design 点只有 1 个（单点）时 folds < 2，折内/折外无法划分。
    // 单点目标不伪造 CV 分数：B3 直接采用名义 target_level + B2 的 λ（CV 块标 cv_skipped）。
    // 这里对全部 v2 幅度目标生效（M=1 是合法输入：只做"单点"校准；emodal 的 v1 冻结路径不动）。
    let cv_skipped = folds < 2;
    let mut candidates: Vec<V2Candidate> = Vec::new();
    if cv_skipped {
        candidates.push(V2Candidate {
            level: obj_cfg.beta,
            lambda_rel: cfg.lambda_b2_rel,
            mean_objective: f64::NAN,
            fold_objective: Vec::new(),
            fold_rises: Vec::new(),
            fold_violations: Vec::new(),
            cv_feasible: true,
        });
    }
    let levels_used: Vec<f64> = if cv_skipped {
        Vec::new()
    } else {
        levels.clone()
    };
    for &level in &levels_used {
        for &lambda_rel in &cfg.lambda_grid_rel {
            let lambda_abs = lambda_rel * h_scale;
            let mut fold_objective = Vec::new();
            let mut fold_rises = Vec::new();
            let mut fold_violations: Vec<(usize, objective::RiseViolation)> = Vec::new();
            for f in 0..folds {
                let train: Vec<usize> = (0..n).filter(|i| i % folds != f).collect();
                let val: Vec<usize> = (0..n).filter(|i| i % folds == f).collect();
                if train.is_empty() || val.is_empty() {
                    continue;
                }
                let h_train = select_rows(h_design, &train);
                let p0_train = select_rows(p_b0_design, &train);
                let a_band = objective::band_targets(&p0_train, plan, level);
                let a_bin = objective::target_per_bin(&input.freqs_hz, plan, &a_band);
                let d = design_objective_with_target(
                    &h_train,
                    &a_bin,
                    &input.freqs_hz,
                    fs,
                    plan,
                    obj_cfg,
                    lambda_abs,
                    cfg,
                    opts,
                )?;
                let h_val = select_rows(h_design, &val);
                let p_val_before = select_rows(p_b0_design, &val);
                let p_val_after = solve::pressure(&h_val, &d.g_actual);
                let local: Vec<usize> = (0..h_val.len()).collect();
                let m = objective::metrics_for_points(
                    &p_val_after,
                    &p_val_before,
                    &local,
                    plan,
                    e_k0,
                    alpha,
                );
                // 兼容既有目标逐位不变：3 折 × 3 design 点的标准配置下，每折验证集恰好 1 个点
                // （M=1 退化）。历史口径里 seat-std/combined 的折目标值是 NaN（seat_std 无定义），
                // 这里保留该口径，否则 B3 的 CV 选择会变（虽然 M=1 的 seat 离散度本身没有意义）。
                let fold_value = if !m.seat_std_defined
                    && matches!(
                        obj_cfg.kind,
                        ObjectiveKind::SeatStd | ObjectiveKind::Combined
                    ) {
                    f64::NAN
                } else {
                    obj_cfg.objective_value(&m)
                };
                fold_objective.push(fold_value);
                let chk =
                    objective::rise_check(&p_val_after, &p_val_before, &local, plan, max_rise);
                fold_rises.push(chk.max_rise_db);
                for v in chk.violations {
                    fold_violations.push((f, v));
                }
            }
            if fold_objective.is_empty() {
                continue;
            }
            let finite: Vec<f64> = fold_objective
                .iter()
                .cloned()
                .filter(|v| v.is_finite())
                .collect();
            let mean_objective = if finite.is_empty() {
                f64::INFINITY
            } else {
                finite.iter().sum::<f64>() / finite.len() as f64
            };
            let cv_feasible = fold_rises.iter().all(|r| *r <= max_rise + 1e-9);
            candidates.push(V2Candidate {
                level,
                lambda_rel,
                mean_objective,
                fold_objective,
                fold_rises,
                fold_violations,
                cv_feasible,
            });
        }
    }
    if candidates.is_empty() {
        return Err("run_scene: v2 CV 没有候选".to_string());
    }
    let mut order: Vec<usize> = (0..candidates.len())
        .filter(|i| candidates[*i].cv_feasible)
        .collect();
    order.sort_by(|a, b| {
        candidates[*a]
            .mean_objective
            .partial_cmp(&candidates[*b].mean_objective)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.cmp(b))
    });
    let mut fallback: Vec<usize> = (0..candidates.len()).collect();
    fallback.sort_by(|a, b| {
        candidates[*a]
            .mean_objective
            .partial_cmp(&candidates[*b].mean_objective)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.cmp(b))
    });
    for idx in &fallback {
        if !order.contains(idx) {
            order.push(*idx);
        }
    }
    struct Chosen {
        idx: usize,
        p_after: Vec<Vec<Cx>>,
        h: Vec<Vec<f64>>,
        target_a: Vec<f64>,
        constraints: FirConstraintReport,
        fir_files: Vec<(String, String)>,
        feasibility: FeasibilityResult,
        objective_value: f64,
    }
    let mut chosen: Option<Chosen> = None;
    let mut first_any: Option<Chosen> = None;
    for idx in order {
        let c = &candidates[idx];
        let d = design_objective(
            h_design,
            p_b0_design,
            &input.freqs_hz,
            fs,
            plan,
            obj_cfg,
            c.level,
            c.lambda_rel * h_scale,
            cfg,
            opts,
        )?;
        let (h, g, cons) = constraints::enforce_fir_constraints(
            &d.h,
            &input.freqs_hz,
            fs,
            cfg.gain_cap_db,
            cfg.delay_samples,
            true,
        )?;
        let p_after = solve::pressure(&input.h, &g);
        let chk = objective::rise_check(&p_after, p_b0, design, plan, max_rise);
        let mut feas = FeasibilityResult {
            design_check: chk,
            cv_fold_max_rise_db: c.fold_rises.clone(),
            cv_violations: c.fold_violations.clone(),
            feasible: false,
        };
        feas.feasible = feas.design_check.violations.is_empty() && c.cv_feasible;
        let objv = design_obj_value(&p_after, p_b0, design, plan, e_k0, alpha, obj_cfg);
        let files = make_fir_files(input, cfg, &h, "");
        let a_band_chosen = objective::band_targets(p_b0_design, plan, c.level);
        let a_bin_chosen = objective::target_per_bin(&input.freqs_hz, plan, &a_band_chosen);
        let entry = Chosen {
            idx,
            p_after,
            h,
            target_a: a_bin_chosen,
            constraints: cons,
            fir_files: files,
            feasibility: feas.clone(),
            objective_value: objv,
        };
        if feas.feasible {
            chosen = Some(entry);
            break;
        }
        if first_any.is_none() {
            first_any = Some(entry);
        }
    }
    let chosen = chosen
        .or(first_any)
        .ok_or_else(|| "run_scene: v2 没有可评估候选".to_string())?;
    let c = &candidates[chosen.idx];
    let level_selected = Some(c.level);
    let lambda_selected = c.lambda_rel;
    let mut cv_fields: Vec<(String, Json)> = vec![
        ("folds".to_string(), Json::Num(folds as f64)),
        ("objective".to_string(), Json::str(objective_name(obj_cfg))),
        (
            "fold_scores".to_string(),
            Json::Arr(
                candidates
                    .iter()
                    .map(|c| {
                        Json::Obj(vec![
                            ("target_level".to_string(), Json::num(c.level)),
                            ("lambda_rel".to_string(), Json::num(c.lambda_rel)),
                            ("mean_objective".to_string(), Json::num(c.mean_objective)),
                            (
                                "fold_objective".to_string(),
                                Json::Arr(c.fold_objective.iter().map(|v| Json::num(*v)).collect()),
                            ),
                            (
                                "fold_max_rise_db".to_string(),
                                Json::Arr(c.fold_rises.iter().map(|v| Json::num(*v)).collect()),
                            ),
                            ("cv_feasible".to_string(), Json::Bool(c.cv_feasible)),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "selected_target_level".to_string(),
            Json::opt_num(level_selected),
        ),
        (
            "selected_lambda_rel".to_string(),
            Json::num(lambda_selected),
        ),
        (
            "selection".to_string(),
            Json::str(if cv_skipped {
                "CV skipped (design points < 2): B3 = nominal target_level + B2 lambda_rel; holdout never used"
            } else {
                "design-only 3-fold CV objective + holdout-safe max_db_rise feasibility; holdout never used"
            }),
        ),
    ];
    if cv_skipped {
        // 单点退化：如实标注没有 CV，不伪造折分数（B3 = 名义 target_level + B2 λ）。
        cv_fields.push(("cv_skipped".to_string(), Json::Bool(true)));
        cv_fields.push((
            "cv_skip_reason".to_string(),
            Json::str("design 点 < 2：单点无法做折内/折外划分；B3 采用名义 target_level 与 B2 的 lambda_rel（不伪造 CV 分数）"),
        ));
    }
    let cv = Json::Obj(cv_fields);
    Ok(VariantOutcome {
        b2: FirVariantResult {
            p_after: p_b2,
            h: h2,
            target_a: a_bin_b2,
            fir_prefix: "B2".to_string(),
            constraints: c2,
            fir_files: files2,
            target_level: Some(level_b2),
            lambda_rel: cfg.lambda_b2_rel,
            objective_value: objv2,
            feasibility: feas2,
            safety: None,
            pre_safety_metrics: None,
            pre_safety_objective_value: None,
        },
        b3: FirVariantResult {
            p_after: chosen.p_after,
            h: chosen.h,
            target_a: chosen.target_a,
            fir_prefix: String::new(),
            constraints: chosen.constraints,
            fir_files: chosen.fir_files,
            target_level: level_selected,
            lambda_rel: lambda_selected,
            objective_value: chosen.objective_value,
            feasibility: chosen.feasibility,
            safety: None,
            pre_safety_metrics: None,
            pre_safety_objective_value: None,
        },
        cv,
    })
}

#[cfg(test)]
mod status_tests {
    use super::*;

    fn good_design() -> DesignBlock {
        let mut d = DesignBlock::default();
        d.e_modal_db = -3.0;
        d.r_max = Some(0.5);
        d.energy_migration = false;
        d
    }

    fn good_holdout() -> HoldoutBlock {
        let mut h = HoldoutBlock::default();
        h.consistency_c = 1.0;
        h.valid_holdout_count = 3;
        h.severe_regression = false;
        h
    }

    fn feasibility(ok: bool) -> FeasibilityResult {
        let mut f = FeasibilityResult::default();
        f.feasible = ok;
        f
    }

    /// 冻结优先级：BASELINE / severe>INFEASIBLE / INFEASIBLE / PASS / PARTIAL / INCOMPLETE。
    #[test]
    fn predicted_status_priority_rules() {
        let d = good_design();
        let h = good_holdout();
        assert_eq!(predicted_status(&d, &h, None, true), "BASELINE");
        let mut severe = good_holdout();
        severe.severe_regression = true;
        assert_eq!(
            predicted_status(&d, &severe, Some(&feasibility(false)), false),
            "PREDICTED_FAIL"
        );
        assert_eq!(
            predicted_status(&d, &h, Some(&feasibility(false)), false),
            "INFEASIBLE"
        );
        assert_eq!(
            predicted_status(&d, &h, Some(&feasibility(true)), false),
            "PREDICTED_PASS"
        );
        let mut partial = good_holdout();
        partial.consistency_c = 0.25;
        assert_eq!(
            predicted_status(&d, &partial, Some(&feasibility(true)), false),
            "PREDICTED_PARTIAL"
        );
        let mut inc = good_holdout();
        inc.valid_holdout_count = 2;
        let mut d2 = good_design();
        d2.e_modal_db = 0.5;
        assert_eq!(
            predicted_status(&d2, &inc, Some(&feasibility(true)), false),
            "INCOMPLETE"
        );
        assert_eq!(predicted_status(&d, &severe, None, false), "PREDICTED_FAIL");
        let mut d3 = good_design();
        d3.energy_migration = true;
        assert_eq!(
            predicted_status(&d3, &h, Some(&feasibility(true)), false),
            "PREDICTED_PARTIAL"
        );
    }
}

#[cfg(test)]
mod pipeline_tests {
    use super::*;

    fn synthetic_input() -> SceneInput {
        // NFFT=8192 网格上的精确 bin 频率（k=[7,9,11,13,17,21]），覆盖 40/50/63/80/100/125 Hz 带
        let freqs = vec![
            7.0 * 48000.0 / 8192.0,
            9.0 * 48000.0 / 8192.0,
            11.0 * 48000.0 / 8192.0,
            13.0 * 48000.0 / 8192.0,
            17.0 * 48000.0 / 8192.0,
            21.0 * 48000.0 / 8192.0,
        ];
        let k = freqs.len();
        let h: Vec<Vec<Vec<Cx>>> = (0..4)
            .map(|m| {
                (0..k)
                    .map(|kk| {
                        vec![
                            Cx::new(1.0 + 0.1 * m as f64, 0.05 * kk as f64),
                            Cx::new(0.3 + 0.02 * kk as f64, -0.1 * m as f64),
                        ]
                    })
                    .collect()
            })
            .collect();
        SceneInput {
            scene_id: "synthetic-pipeline".to_string(),
            fixture_dir: "synthetic".to_string(),
            ground_truth_sha256: "0".repeat(64),
            seed: 1,
            rir_scale: 0.5,
            sample_rate_hz: 48000.0,
            freqs_hz: freqs,
            source_ids: vec!["SUB1".to_string(), "SUB2".to_string()],
            mic_ids: vec![
                "P01".to_string(),
                "P02".to_string(),
                "P03".to_string(),
                "P04".to_string(),
            ],
            design_mics: vec![0, 1, 2],
            holdout_mics: vec![3],
            h,
            band_centers_hz: vec![40.0, 50.0, 63.0, 80.0, 100.0, 125.0],
            extended_centers_hz: vec![25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0],
        }
    }

    fn cfg() -> RunConfig {
        let mut c = RunConfig::default();
        c.taps = 64;
        c.delay_samples = 16;
        c.nfft = 8192;
        c
    }

    #[test]
    fn synthetic_scene_runs_and_has_all_variants_and_status() {
        let run = run_scene(&synthetic_input(), &cfg()).expect("run_scene");
        let text = run.report.to_compact();
        for key in ["\"B0\"", "\"B1\"", "\"B2\"", "\"B3\"", "\"cv\"", "UNPROVEN"] {
            assert!(text.contains(key), "missing {} in report", key);
        }
        assert!(!run.metrics_csv.is_empty());
        assert_eq!(run.fir_files.len(), 4);
        assert!(run.fir_files.iter().any(|(p, _)| p == "fir/SUB1.csv"));
        // B0 固定 BASELINE；B2/B3 有 status 且 B0 design e_modal_db 归一化为 0
        assert_eq!(
            jget(&run.report, &["variants", "B0", "predicted_status"]).map(|v| v.to_compact()),
            Some("\"BASELINE\"".to_string())
        );
        let b0 = match jget(&run.report, &["variants", "B0", "design", "e_modal_db"]) {
            Some(Json::Num(v)) => *v,
            other => panic!("missing B0 e_modal_db: {:?}", other),
        };
        assert!(b0.abs() < 1e-9);
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
