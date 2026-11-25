//! mimo-opt CLI（契约 §8）：RoomForge 仿真夹具 → 低频 MIMO 优化 → report.json / fir/*.csv / metrics.csv。
//!
//! 用法：
//!   cargo run --release -p khaen-mimo-opt -- --fixtures <dir> --out <runs-dir> [--scene <id>] [--verify-fixtures]
//!
//! SIMULATED / UNPROVEN：全部输出基于合成夹具，不是实测；predicted 不得写成 PASS。

use std::path::{Path, PathBuf};

use khaen_mimo_design::objective::{ObjectiveConfig, ObjectiveKind};
use khaen_mimo_design::report::Json;
use khaen_mimo_design::{run_scene, Cx, RunConfig, SceneInput};
use khaen_room_sim::fixture::SceneFixture;
use khaen_room_sim::sha256;

#[derive(Default, Debug)]
struct Args {
    fixtures: Option<String>,
    out: Option<String>,
    scene: Option<String>,
    verify: bool,
    solver: String,
    // OBJECTIVES-V2
    objective: Option<String>,
    beta: Option<f64>,
    target_level: Option<f64>,
    p_norm: Option<f64>,
    w_std: Option<f64>,
    w_null: Option<f64>,
    w_e: Option<f64>,
    max_db_rise: Option<f64>,
    maxdev_weight: Option<f64>,
}

fn parse_f64(flag: &str, v: Option<String>) -> Result<f64, String> {
    let s = v.ok_or_else(|| format!("{} 缺少参数", flag))?;
    s.parse::<f64>()
        .map_err(|_| format!("{} 参数不是数字: {}", flag, s))
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        solver: "own".to_string(),
        ..Default::default()
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--fixtures" => {
                args.fixtures = Some(it.next().ok_or("--fixtures 缺少参数")?);
            }
            "--out" => {
                args.out = Some(it.next().ok_or("--out 缺少参数")?);
            }
            "--scene" => {
                args.scene = Some(it.next().ok_or("--scene 缺少参数")?);
            }
            "--verify-fixtures" => args.verify = true,
            "--solver" => {
                args.solver = it.next().ok_or("--solver 缺少参数")?;
            }
            "--objective" => {
                args.objective = Some(it.next().ok_or("--objective 缺少参数")?);
            }
            "--beta" => args.beta = Some(parse_f64("--beta", it.next())?),
            "--target-level" => args.target_level = Some(parse_f64("--target-level", it.next())?),
            "--p-norm" => args.p_norm = Some(parse_f64("--p-norm", it.next())?),
            "--w-std" => args.w_std = Some(parse_f64("--w-std", it.next())?),
            "--w-null" => args.w_null = Some(parse_f64("--w-null", it.next())?),
            "--w-e" => args.w_e = Some(parse_f64("--w-e", it.next())?),
            "--max-db-rise" => args.max_db_rise = Some(parse_f64("--max-db-rise", it.next())?),
            "--maxdev-weight" => {
                args.maxdev_weight = Some(parse_f64("--maxdev-weight", it.next())?)
            }
            "--help" | "-h" => {
                println!(
                    "用法: mimo-opt --fixtures <dir> --out <runs-dir> [--scene <id>] [--verify-fixtures] [--solver own|modal]\n       [--objective emodal|seat-std|null-depth|combined] [--beta 0.70] [--target-level 1.0]\n       [--p-norm 6] [--w-std 1.0] [--w-null 1.0] [--w-e 1.0] [--max-db-rise 2.0] [--maxdev-weight 0.1]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("未知参数: {}", other)),
        }
    }
    if args.fixtures.is_none() {
        return Err("缺少 --fixtures <dir>".to_string());
    }
    if args.out.is_none() {
        return Err("缺少 --out <runs-dir>".to_string());
    }
    if args.solver != "own" && args.solver != "modal" {
        return Err(format!("未知 --solver {}（支持 own|modal）", args.solver));
    }
    Ok(args)
}

fn build_config(args: &Args) -> Result<RunConfig, String> {
    let kind = match args.objective.as_deref() {
        None => ObjectiveKind::EModal,
        Some(s) => ObjectiveKind::parse(s).ok_or_else(|| {
            format!(
                "未知 --objective {}（支持 emodal|seat-std|null-depth|combined）",
                s
            )
        })?,
    };
    // --beta 是 emodal 的 beta；--target-level 是 band-target 目标的 beta_level（优先级更高）
    let level_override = args.target_level.or(args.beta);
    let mut obj = ObjectiveConfig::for_kind(kind, level_override);
    if let Some(v) = args.p_norm {
        obj.p_norm = v;
    }
    if let Some(v) = args.w_std {
        obj.w_std = v;
    }
    if let Some(v) = args.w_null {
        obj.w_null = v;
    }
    if let Some(v) = args.w_e {
        obj.w_e = v;
    }
    if let Some(v) = args.max_db_rise {
        obj.max_db_rise_db = v;
    }
    if let Some(v) = args.maxdev_weight {
        obj.maxdev_weight = v;
    }
    // OBJECTIVES-V2 允许 IRLS <=4；band-target 目标用 2 次（与 v1 emodal 一致，控时）
    obj.irls_iterations = 2;
    if !(obj.beta.is_finite() && obj.beta > 0.0) {
        return Err(format!("非法 beta/target-level: {}", obj.beta));
    }
    let mut cfg = RunConfig::default();
    cfg.objective = obj.clone();
    if kind == ObjectiveKind::EModal {
        cfg.target_beta = obj.beta;
    }
    Ok(cfg)
}

fn list_scenes(fixtures: &Path) -> Result<Vec<String>, String> {
    let mut scenes = Vec::new();
    let rd = std::fs::read_dir(fixtures)
        .map_err(|e| format!("读取夹具目录 {} 失败: {}", fixtures.display(), e))?;
    for entry in rd {
        let entry = entry.map_err(|e| format!("遍历夹具目录失败: {}", e))?;
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        if !p.join("ground-truth.json").is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        scenes.push(name);
    }
    scenes.sort();
    if scenes.is_empty() {
        return Err(format!(
            "{} 下没有 scene-*/ground-truth.json",
            fixtures.display()
        ));
    }
    Ok(scenes)
}

fn select_scenes(all: &[String], pick: &Option<String>) -> Result<Vec<String>, String> {
    match pick {
        None => Ok(all.to_vec()),
        Some(id) => {
            if all.iter().any(|s| s == id) {
                Ok(vec![id.clone()])
            } else {
                Err(format!("--scene {} 不存在；可用: {:?}", id, all))
            }
        }
    }
}

/// 从 room-sim 夹具 + HMatrix 构造 mimo-design 的纯算法输入。
fn build_input(
    fx: &SceneFixture,
    hm: &khaen_room_sim::matrix::HMatrix,
    scene: &str,
    ground_truth_sha256: String,
) -> Result<SceneInput, String> {
    let k = hm.n_bins();
    let n_src = hm.n_sources;
    let n_mics = hm.n_mics;
    let mut h = vec![vec![vec![Cx::zero(); n_src]; k]; n_mics];
    for m in 0..n_mics {
        for kk in 0..k {
            for n in 0..n_src {
                let c = hm.at(m, n, kk);
                h[m][kk][n] = Cx::new(c.re, c.im);
            }
        }
    }
    let source_ids: Vec<String> = (0..n_src).map(|i| fx.sources[i].id.clone()).collect();
    let mic_ids: Vec<String> = (0..n_mics).map(|i| fx.mics[i].id.clone()).collect();
    let design_mics: Vec<usize> = fx
        .design_mic_ids
        .iter()
        .map(|id| {
            fx.mic_index(id)
                .ok_or_else(|| format!("design mic {} 不在夹具 mic 列表", id))
        })
        .collect::<Result<_, _>>()?;
    let holdout_mics: Vec<usize> = fx
        .holdout_mic_ids
        .iter()
        .map(|id| {
            fx.mic_index(id)
                .ok_or_else(|| format!("holdout mic {} 不在夹具 mic 列表", id))
        })
        .collect::<Result<_, _>>()?;
    Ok(SceneInput {
        scene_id: fx.scene_id.clone(),
        fixture_dir: scene.to_string(),
        ground_truth_sha256,
        seed: fx.seed,
        rir_scale: fx.rir.scale,
        sample_rate_hz: fx.sample_rate_hz as f64,
        freqs_hz: hm.freqs_hz.clone(),
        source_ids,
        mic_ids,
        design_mics,
        holdout_mics,
        h,
        band_centers_hz: fx.bands.evaluated_spec_centers_hz.clone(),
        extended_centers_hz: fx.bands.extended_centers_hz.clone(),
    })
}

fn write_file(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建目录 {} 失败: {}", parent.display(), e))?;
    }
    std::fs::write(path, content).map_err(|e| format!("写 {} 失败: {}", path.display(), e))
}

// ---------- JSON 读取辅助（summary 用） ----------

fn jget<'a>(root: &'a Json, keys: &[&str]) -> Option<&'a Json> {
    let mut cur = root;
    for key in keys {
        match cur {
            Json::Obj(items) => {
                cur = &items.iter().find(|(k, _)| k == key)?.1;
            }
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

fn scene_sort_key(id: &str) -> String {
    id.to_string()
}

// ---------- 设计模式 ----------

fn process_scene(
    fixtures: &Path,
    out: &Path,
    scene: &str,
    cfg: &RunConfig,
    solver: &str,
) -> Result<Json, String> {
    if solver != "own" {
        return Err("--solver modal 尚未实现（P2；own 后端为默认设计结果）".to_string());
    }
    let dir = fixtures.join(scene);
    let fx =
        SceneFixture::load(&dir).map_err(|e| format!("加载夹具 {} 失败: {}", dir.display(), e))?;
    let hm = fx.h_matrix()?;
    let gt_sha = sha256::file_hex(&dir.join("ground-truth.json"))?;
    let input = build_input(&fx, &hm, scene, gt_sha)?;
    let run = run_scene(&input, cfg)?;
    let scene_out = out.join(scene);
    // report.json
    write_file(&scene_out.join("report.json"), &run.report.to_pretty())?;
    // fir/<...>.csv
    for (rel, content) in &run.fir_files {
        let path = scene_out.join(rel);
        write_file(&path, content)?;
    }
    // metrics.csv
    write_file(&scene_out.join("metrics.csv"), &run.metrics_csv)?;
    Ok(run.report)
}

fn summary_line(scene: &str, report: &Json) -> String {
    let g = |variant: &str, block: &str, key: &str| {
        jnum(report, &["variants", variant, block, key]).unwrap_or(f64::NAN)
    };
    let rmax = |variant: &str, block: &str| {
        jnum(report, &["variants", variant, block, "r_max"]).unwrap_or(f64::NAN)
    };
    let b3c = jnum(report, &["variants", "B3", "holdout", "consistency_c"]).unwrap_or(f64::NAN);
    let b3sev = jbool(report, &["variants", "B3", "holdout", "severe_regression"]).unwrap_or(true);
    let b3std = jnum(report, &["variants", "B3", "design", "seat_std_mean_db"]).unwrap_or(f64::NAN);
    let b3null =
        jnum(report, &["variants", "B3", "design", "null_depth_mean_db"]).unwrap_or(f64::NAN);
    let b3hold = jnum(report, &["variants", "B3", "holdout", "e_modal_db"]).unwrap_or(f64::NAN);
    let b3status = match jget(report, &["variants", "B3", "predicted_status"]) {
        Some(Json::Str(s)) => s.clone(),
        _ => "?".to_string(),
    };
    let feasible = jbool(report, &["variants", "B3", "feasibility", "feasible"]).unwrap_or(false);
    format!(
        "{:<20} B0 {:7.2} B1 {:7.2} B2 {:7.2} B3 {:7.2} | B3hold {:7.2} | Rmax {:6.3} seat_std {:6.3} null {:7.3} | C {:.2} sev {:<5} feas {:<5} | {}",
        scene,
        g("B0", "design", "e_modal_db"),
        g("B1", "design", "e_modal_db"),
        g("B2", "design", "e_modal_db"),
        g("B3", "design", "e_modal_db"),
        b3hold,
        rmax("B3", "design"),
        b3std,
        b3null,
        b3c,
        if b3sev { "yes" } else { "no" },
        if feasible { "yes" } else { "no" },
        b3status
    )
}

fn run_design(args: &Args, scenes: &[String]) -> Result<(), String> {
    let fixtures = PathBuf::from(args.fixtures.as_ref().unwrap());
    let out = PathBuf::from(args.out.as_ref().unwrap());
    let cfg = build_config(args)?;
    println!(
        "mimo-opt: SIMULATED/UNPROVEN  synthetic fixtures only; solver={} objective={} scenes={}",
        args.solver,
        cfg.objective.kind.tag(),
        scenes.len()
    );
    println!(
        "config: taps={} delay={} irls={} lambda_b2_rel={:e} lambda_grid={:?} target_beta={} cv_folds={} objective={} beta={:.3} max_db_rise={:.2} ladder={}",
        cfg.taps,
        cfg.delay_samples,
        cfg.irls_iterations,
        cfg.lambda_b2_rel,
        cfg.lambda_grid_rel,
        cfg.target_beta,
        cfg.cv_folds,
        cfg.objective.kind.tag(),
        cfg.objective.beta,
        cfg.objective.max_db_rise_db,
        cfg.objective.ladder_enabled
    );
    let mut rows: Vec<String> = Vec::new();
    let mut failures: Vec<(String, String)> = Vec::new();
    let mut sorted = scenes.to_vec();
    sorted.sort_by_key(|s| scene_sort_key(s));
    for scene in &sorted {
        let t0 = std::time::Instant::now();
        match process_scene(&fixtures, &out, scene, &cfg, &args.solver) {
            Ok(report) => {
                let line = summary_line(scene, &report);
                println!("[ok] {}  ({:.2}s)", line, t0.elapsed().as_secs_f64());
                rows.push(line);
            }
            Err(e) => {
                eprintln!("[FAIL] {}: {}", scene, e);
                failures.push((scene.clone(), e));
            }
        }
    }
    println!("---- summary (design E_modal_db / R_max / holdout C / severe) ----");
    for r in &rows {
        println!("{}", r);
    }
    if !failures.is_empty() {
        return Err(format!(
            "{} 个场景失败: {:?}",
            failures.len(),
            failures
                .iter()
                .map(|(s, e)| format!("{}: {}", s, e))
                .collect::<Vec<_>>()
        ));
    }
    println!(
        "exit 0: {} 场景全部成功；输出目录 {}",
        rows.len(),
        out.display()
    );
    Ok(())
}

// ---------- 夹具校验模式 ----------

fn verify_scene(fixtures: &Path, scene: &str) -> Result<Json, String> {
    let dir = fixtures.join(scene);
    let fx =
        SceneFixture::load(&dir).map_err(|e| format!("加载夹具 {} 失败: {}", dir.display(), e))?;
    let hm = fx.h_matrix()?;
    let rr = fx.rerender_check()?;
    let gt_sha = sha256::file_hex(&dir.join("ground-truth.json"))?;
    let pairs: Vec<Json> = rr
        .pairs
        .iter()
        .map(|p| {
            Json::obj(vec![
                ("source", Json::str(p.source_id.clone())),
                ("mic", Json::str(p.mic_id.clone())),
                ("max_abs_delta", Json::num(p.max_abs_delta)),
                ("max_relative_delta", Json::num(p.max_relative_delta)),
                ("relative_rms", Json::num(p.relative_rms)),
            ])
        })
        .collect();
    Ok(Json::obj(vec![
        ("scene_id", Json::str(fx.scene_id.clone())),
        ("dir", Json::str(scene)),
        ("ground_truth_sha256", Json::str(gt_sha)),
        ("sha256_ok", Json::Bool(true)),
        (
            "h_matrix",
            Json::obj(vec![
                ("n_sources", Json::Num(hm.n_sources as f64)),
                ("n_mics", Json::Num(hm.n_mics as f64)),
                ("n_bins", Json::Num(hm.n_bins() as f64)),
                (
                    "freq_range_hz",
                    Json::arr(vec![
                        Json::num(hm.freqs_hz.first().copied().unwrap_or(f64::NAN)),
                        Json::num(hm.freqs_hz.last().copied().unwrap_or(f64::NAN)),
                    ]),
                ),
            ]),
        ),
        (
            "rerender",
            Json::obj(vec![
                ("tolerance", Json::num(rr.tolerance)),
                ("max_abs_delta", Json::num(rr.max_abs_delta)),
                ("max_relative_delta", Json::num(rr.max_relative_delta)),
                ("max_relative_rms", Json::num(rr.max_relative_rms)),
                ("passed", Json::Bool(rr.passed)),
                ("pairs", Json::arr(pairs)),
            ]),
        ),
        ("passed", Json::Bool(rr.passed)),
        (
            "warnings",
            Json::arr(fx.warnings.iter().map(|w| Json::str(w.clone())).collect()),
        ),
    ]))
}

fn run_verify(args: &Args, scenes: &[String]) -> Result<(), String> {
    let fixtures = PathBuf::from(args.fixtures.as_ref().unwrap());
    let out = PathBuf::from(args.out.as_ref().unwrap());
    println!(
        "mimo-opt --verify-fixtures: SIMULATED/UNPROVEN  synthetic fixtures only; scenes={}",
        scenes.len()
    );
    let mut entries = Vec::new();
    let mut all_ok = true;
    let mut failures: Vec<(String, String)> = Vec::new();
    for scene in scenes {
        match verify_scene(&fixtures, scene) {
            Ok(entry) => {
                let passed = jbool(&entry, &["passed"]).unwrap_or(false);
                if !passed {
                    all_ok = false;
                }
                let rr = jnum(&entry, &["rerender", "max_relative_delta"]).unwrap_or(f64::NAN);
                let abs = jnum(&entry, &["rerender", "max_abs_delta"]).unwrap_or(f64::NAN);
                println!(
                    "[{}] {:<20} rerender max|Δ|={:.3e} max_rel={:.3e}",
                    if passed { "ok" } else { "FAIL" },
                    scene,
                    abs,
                    rr
                );
                entries.push(entry);
            }
            Err(e) => {
                eprintln!("[FAIL] {}: {}", scene, e);
                failures.push((scene.clone(), e));
                all_ok = false;
            }
        }
    }
    let verify = Json::obj(vec![
        ("format", Json::str("tspi-mimo-verify/v1")),
        (
            "provenance",
            Json::obj(vec![
                ("data_kind", Json::str("SIMULATED")),
                ("honesty_status", Json::str("UNPROVEN")),
            ]),
        ),
        ("all_passed", Json::Bool(all_ok)),
        ("scenes", Json::arr(entries)),
        (
            "failures",
            Json::arr(
                failures
                    .iter()
                    .map(|(s, e)| {
                        Json::obj(vec![
                            ("scene", Json::str(s.clone())),
                            ("error", Json::str(e.clone())),
                        ])
                    })
                    .collect(),
            ),
        ),
    ]);
    std::fs::create_dir_all(&out)
        .map_err(|e| format!("创建输出目录 {} 失败: {}", out.display(), e))?;
    write_file(&out.join("verify.json"), &verify.to_pretty())?;
    println!("verify.json -> {}", out.join("verify.json").display());
    if !all_ok {
        return Err("夹具校验失败（SHA256 或重渲染误差超门限）".to_string());
    }
    Ok(())
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let fixtures = PathBuf::from(args.fixtures.as_ref().unwrap());
    let scenes_all = list_scenes(&fixtures)?;
    let scenes = select_scenes(&scenes_all, &args.scene)?;
    if args.verify {
        run_verify(&args, &scenes)
    } else {
        run_design(&args, &scenes)
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("mimo-opt: ERROR: {}", e);
        std::process::exit(1);
    }
}
