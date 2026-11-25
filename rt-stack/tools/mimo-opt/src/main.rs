//! mimo-opt CLI（契约 `8 + 计划 4.5）：RoomForge 仿真夹具 → 低频 MIMO 优化 →
//! report.json / fir/*.csv / metrics.csv + 可复现产物目录（params.json + manifest.json）。
//!
//! 用法：
//!   cargo run --release -p khaen-mimo-opt -- --fixtures <dir> --out <runs-dir> [--scene <id>] [--verify-fixtures]
//!   cargo run --release -p khaen-mimo-opt -- --fixtures <dir> --out <runs-dir> [--scene <id>] \
//!       [--min-snr-db 15] [--authority] [--authority-eps-rel 1e-6] [--authority-max-gamma 1e4] \
//!       [--safety] [--max-band-boost-db 3] [--max-positive-sum-db 6] [--max-predicted-peak-dbfs -1] \
//!       [--out-dir <artifacts-dir>]
//!   cargo run --release -p khaen-mimo-opt -- --verify <artifacts-dir>
//!
//! 4.3 单点目标（例：单点校准，模态衰减 + 皇帝位谷深，座位离散权重为 0；
//! 可与 --authority/--safety 同时开启）：
//!   cargo run --release -p khaen-mimo-opt -- --fixtures <dir> --out <runs-dir> --scene <id> \
//!       --objective single-point --authority --safety --out-dir <artifacts-dir>
//!
//! 4.5 产物语义（见 src/artifact.rs）：
//! - 一次成功运行产出一个自洽目录（默认 <out>/run-<params_sha256 前 16 位>/，可用 --out-dir 指定）：
//!   params.json（完整输入快照）+ report.json + metrics.csv + fir/*.csv + manifest.json；
//! - manifest 里每个文件的 relative path + sha256 + bytes，加 params_sha256 / result_sha256 / schema；
//! - 不写时间戳 / 主机名 / 绝对路径 / PID / 随机数：同输入同参数两次运行 sha256 全同；
//! - --verify <dir> 重算并比对 manifest，失败指出具体文件。
//!
//! SIMULATED / UNPROVEN：全部输出基于合成夹具，不是实测；predicted 不得写成 PASS。

mod artifact;

use std::path::{Path, PathBuf};

use khaen_mimo_design::authority::AuthorityConfig;
use khaen_mimo_design::limits::SafetyConfig;
use khaen_mimo_design::objective::{ObjectiveConfig, ObjectiveKind};
use khaen_mimo_design::quality::{
    band_snr_estimate, QualityConfig, THIRD_OCTAVE_CENTERS_HZ_20_20K,
};
use khaen_mimo_design::report::Json;
use khaen_mimo_design::{run_scene, Cx, RunConfig, SceneInput, SceneRun};
use khaen_room_sim::fixture::SceneFixture;
use khaen_room_sim::sha256;

use artifact::{ArtifactFile, FileRole, RunWriteRequest};

/// 本工具名（写进 manifest / params 快照）。
const TOOL_NAME: &str = "khaen-mimo-opt";
/// plan.bin 缺失原因（写进 manifest；本 CLI 路径不伪造运行期 host plan ABI）。
const PLAN_ABSENT_REASON: &str = "mimo-opt 输出 FIR 候选库（fir/*.csv，文本方案内容）；\
本离线设计路径不编译运行期 host plan ABI，因此不产出 plan.bin（不伪造二进制格式）";

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
    // 4.1 质量门
    min_snr_db: Option<f64>,
    // 4.4 权威加权
    authority: Option<bool>,
    authority_eps_rel: Option<f64>,
    authority_max_gamma: Option<f64>,
    // 4.4 安全钳位
    safety: Option<bool>,
    max_band_boost_db: Option<f64>,
    max_positive_sum_db: Option<f64>,
    max_predicted_peak_dbfs: Option<f64>,
    // 4.5 产物
    out_dir: Option<String>,
    verify_artifact: Option<String>,
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
            // ---- 4.1 测量质量门 ----
            "--min-snr-db" => args.min_snr_db = Some(parse_f64("--min-snr-db", it.next())?),
            // ---- 4.4 权威加权 ----
            "--authority" => args.authority = Some(true),
            "--no-authority" => args.authority = Some(false),
            "--authority-eps-rel" => {
                args.authority_eps_rel = Some(parse_f64("--authority-eps-rel", it.next())?)
            }
            "--authority-max-gamma" => {
                args.authority_max_gamma = Some(parse_f64("--authority-max-gamma", it.next())?)
            }
            // ---- 4.4 安全钳位 ----
            "--safety" => args.safety = Some(true),
            "--no-safety" => args.safety = Some(false),
            "--max-band-boost-db" => {
                args.max_band_boost_db = Some(parse_f64("--max-band-boost-db", it.next())?)
            }
            "--max-positive-sum-db" => {
                args.max_positive_sum_db = Some(parse_f64("--max-positive-sum-db", it.next())?)
            }
            "--max-predicted-peak-dbfs" => {
                args.max_predicted_peak_dbfs =
                    Some(parse_f64("--max-predicted-peak-dbfs", it.next())?)
            }
            // ---- 4.5 可复现产物 ----
            "--out-dir" => {
                let dir = it.next().ok_or("--out-dir 缺少参数")?;
                if dir.is_empty() {
                    return Err("--out-dir 的目录为空".to_string());
                }
                args.out_dir = Some(dir);
            }
            "--verify" => {
                args.verify_artifact = Some(it.next().ok_or("--verify 缺少参数")?);
            }
            "--help" | "-h" => {
                println!(
                    "用法: mimo-opt --fixtures <dir> --out <runs-dir> [--scene <id>] [--verify-fixtures] [--solver own|modal]\n\
                     \x20      [--objective emodal|seat-std|null-depth|combined|single-point] [--beta 0.70] [--target-level 1.0]\n\
                     \x20      [--p-norm 6] [--w-std 1.0] [--w-null 1.0] [--w-e 1.0] [--max-db-rise 2.0] [--maxdev-weight 0.1]\n\
                     \x20      [--min-snr-db 15] [--authority|--no-authority] [--authority-eps-rel 1e-6] [--authority-max-gamma 1e4]\n\
                     \x20      [--safety|--no-safety] [--max-band-boost-db 3] [--max-positive-sum-db 6] [--max-predicted-peak-dbfs -1]\n\
                     \x20      [--out-dir <artifacts-dir>]\n\
                     \x20      mimo-opt --verify <artifacts-dir>\n\
                     \x20  例（计划 4.3 单点目标 + 权威 + 安全钳位）:\n\
                     \x20      mimo-opt --fixtures <dir> --out <runs-dir> --objective single-point --authority --safety"
                );
                std::process::exit(0);
            }
            other => return Err(format!("未知参数: {}", other)),
        }
    }

    // ---- 4.5 --verify <dir>：只做产物校验，不需要夹具/输出参数 ----
    if let Some(dir) = &args.verify_artifact {
        if dir.is_empty() {
            return Err("--verify 的目录为空".to_string());
        }
        if args.fixtures.is_some() || args.out.is_some() || args.verify || args.out_dir.is_some() {
            return Err(
                "--verify <dir> 只校验产物目录，不接受 --fixtures/--out/--out-dir/--verify-fixtures"
                    .to_string(),
            );
        }
        return Ok(args);
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
    // A/B 开关与配套参数冲突时 fail-closed（不静默忽略用户显式给的 --no-*）
    if args.authority == Some(false)
        && (args.authority_eps_rel.is_some() || args.authority_max_gamma.is_some())
    {
        return Err("--no-authority 与 --authority-eps-rel/--authority-max-gamma 冲突".to_string());
    }
    if args.safety == Some(false)
        && (args.max_band_boost_db.is_some()
            || args.max_positive_sum_db.is_some()
            || args.max_predicted_peak_dbfs.is_some())
    {
        return Err("--no-safety 与 --max-band-boost-db/--max-positive-sum-db/--max-predicted-peak-dbfs 冲突".to_string());
    }
    Ok(args)
}

fn build_config(args: &Args) -> Result<RunConfig, String> {
    let kind = match args.objective.as_deref() {
        None => ObjectiveKind::EModal,
        Some(s) => ObjectiveKind::parse(s).ok_or_else(|| {
            format!(
                "未知 --objective {}（支持 emodal|seat-std|null-depth|combined|single-point）",
                s
            )
        })?,
    };
    // 计划 4.3：single-point 的座位离散项权重固定为 0（目标里结构上不存在该项）。
    // 显式给非零 --w-std 时 fail-closed，不静默忽略用户参数。
    if kind == ObjectiveKind::SinglePoint && args.w_std.map(|v| v != 0.0).unwrap_or(false) {
        return Err(
            "--objective single-point 的座位离散项权重固定为 0，不接受非零 --w-std".to_string(),
        );
    }
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

    // ---- 4.1 测量质量门：--min-snr-db 给了就启用；逐带 SNR 证据按场景从夹具 IR 派生 ----
    if let Some(min_snr) = args.min_snr_db {
        if !(min_snr.is_finite() && min_snr >= 0.0) {
            return Err(format!("--min-snr-db 非法: {}", min_snr));
        }
        cfg.quality = QualityConfig {
            enabled: true,
            min_snr_db: min_snr,
            ..QualityConfig::default()
        };
    }

    // ---- 4.4 权威加权：--authority / --no-authority；配套参数给了也隐含启用 ----
    let authority_enabled = args
        .authority
        .unwrap_or(args.authority_eps_rel.is_some() || args.authority_max_gamma.is_some());
    let mut authority = AuthorityConfig::default();
    authority.enabled = authority_enabled;
    if let Some(v) = args.authority_eps_rel {
        if !(v.is_finite() && v > 0.0) {
            return Err(format!("--authority-eps-rel 非法: {}", v));
        }
        authority.eps_rel = v;
    }
    if let Some(v) = args.authority_max_gamma {
        if !(v.is_finite() && v >= 1.0) {
            return Err(format!("--authority-max-gamma 非法: {}", v));
        }
        authority.max_gamma = v;
    }
    cfg.authority = authority;

    // ---- 4.4 安全钳位：--safety / --no-safety；配套限值给了也隐含启用 ----
    let safety_enabled = args.safety.unwrap_or(
        args.max_band_boost_db.is_some()
            || args.max_positive_sum_db.is_some()
            || args.max_predicted_peak_dbfs.is_some(),
    );
    let mut safety = SafetyConfig::default();
    safety.enabled = safety_enabled;
    if let Some(v) = args.max_band_boost_db {
        if !v.is_finite() {
            return Err(format!("--max-band-boost-db 非法: {}", v));
        }
        safety.max_band_boost_db = v;
    }
    if let Some(v) = args.max_positive_sum_db {
        if !(v.is_finite() && v >= 0.0) {
            return Err(format!("--max-positive-sum-db 非法: {}", v));
        }
        safety.max_positive_sum_db = v;
    }
    if let Some(v) = args.max_predicted_peak_dbfs {
        if !v.is_finite() {
            return Err(format!("--max-predicted-peak-dbfs 非法: {}", v));
        }
        safety.max_predicted_peak_dbfs = v;
    }
    cfg.safety = safety;
    Ok(cfg)
}

/// 从夹具 RIR 派生逐 1/3 oct SNR 证据（质量门用）。
///
/// 口径：对每个 1/3 oct 中心，取 **design 麦 × 全部源** 的最小 SNR（最差测量），
/// 任一估计失败 → 该带 None（Unknown，fail-closed，不当 0 dB 处理）。
/// holdout 麦克风绝不参与（质量门影响拟合选择，holdout 不参与选择）。
fn quality_config_from_fixture(
    fx: &SceneFixture,
    design_mics: &[usize],
    min_snr_db: f64,
) -> QualityConfig {
    let centers_hz = THIRD_OCTAVE_CENTERS_HZ_20_20K.to_vec();
    let fs = fx.sample_rate_hz as f64;
    let mut band_snr_db: Vec<Option<f64>> = Vec::with_capacity(centers_hz.len());
    for center_hz in &centers_hz {
        let mut worst: Option<f64> = None;
        let mut failed = design_mics.is_empty();
        'pairs: for source_index in 0..fx.sources.len() {
            for mic_index in design_mics {
                let Some(ir_f32) = fx.rir(source_index, *mic_index) else {
                    failed = true;
                    break 'pairs;
                };
                let ir: Vec<f64> = ir_f32.iter().map(|sample| *sample as f64).collect();
                match band_snr_estimate(&ir, fs, *center_hz) {
                    Ok(snr) => {
                        worst = Some(worst.map_or(snr, |value: f64| value.min(snr)));
                    }
                    Err(_) => {
                        failed = true;
                        break 'pairs;
                    }
                }
            }
        }
        band_snr_db.push(if failed { None } else { worst });
    }
    QualityConfig {
        enabled: true,
        min_snr_db,
        centers_hz,
        band_snr_db,
        evidence_snr_band_db: None,
        evidence_band_hz: None,
    }
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

// ---------- 4.5 参数快照 / 产物 ----------

/// 一个场景的输入快照（写进 params.json 的 scenes[]）。
struct SceneRecord {
    fixture: Json,
    input: Json,
    /// 质量门逐带 SNR 证据（仅 --min-snr-db 时；含实际参与拟合的数值）
    quality_evidence: Option<Json>,
}

impl SceneRecord {
    fn json(&self, scene_id: &str) -> Json {
        Json::Obj(vec![
            ("scene_id".to_string(), Json::str(scene_id.to_string())),
            ("fixture".to_string(), self.fixture.clone()),
            ("input".to_string(), self.input.clone()),
            (
                "quality_evidence".to_string(),
                self.quality_evidence.clone().unwrap_or(Json::Null),
            ),
        ])
    }
}

fn opt_arr(values: &[Option<f64>]) -> Json {
    Json::Arr(values.iter().map(|value| Json::opt_num(*value)).collect())
}

fn num_arr(values: &[f64]) -> Json {
    Json::Arr(values.iter().map(|value| Json::num(*value)).collect())
}

fn str_arr(values: &[String]) -> Json {
    Json::Arr(
        values
            .iter()
            .map(|value| Json::str(value.clone()))
            .collect(),
    )
}

fn quality_config_json(quality: &QualityConfig) -> Json {
    Json::Obj(vec![
        ("enabled".to_string(), Json::Bool(quality.enabled)),
        ("min_snr_db".to_string(), Json::num(quality.min_snr_db)),
        ("centers_hz".to_string(), num_arr(&quality.centers_hz)),
        ("band_snr_db".to_string(), opt_arr(&quality.band_snr_db)),
        (
            "evidence_snr_band_db".to_string(),
            Json::opt_num(quality.evidence_snr_band_db),
        ),
        (
            "evidence_band_hz".to_string(),
            match quality.evidence_band_hz {
                Some((low, high)) => Json::Arr(vec![Json::num(low), Json::num(high)]),
                None => Json::Null,
            },
        ),
    ])
}

fn authority_config_json(authority: &AuthorityConfig) -> Json {
    Json::Obj(vec![
        ("enabled".to_string(), Json::Bool(authority.enabled)),
        (
            "passband_hz".to_string(),
            match authority.passband_hz {
                Some((low, high)) => Json::Arr(vec![Json::num(low), Json::num(high)]),
                None => Json::Null,
            },
        ),
        ("eps_rel".to_string(), Json::num(authority.eps_rel)),
        ("max_gamma".to_string(), Json::num(authority.max_gamma)),
    ])
}

fn safety_config_json(safety: &SafetyConfig) -> Json {
    Json::Obj(vec![
        ("enabled".to_string(), Json::Bool(safety.enabled)),
        (
            "max_band_boost_db".to_string(),
            Json::num(safety.max_band_boost_db),
        ),
        (
            "max_positive_sum_db".to_string(),
            Json::num(safety.max_positive_sum_db),
        ),
        (
            "max_predicted_peak_dbfs".to_string(),
            Json::num(safety.max_predicted_peak_dbfs),
        ),
        (
            "reference_peak_dbfs".to_string(),
            Json::num(safety.reference_peak_dbfs),
        ),
        ("pre_gain_db".to_string(), Json::num(safety.pre_gain_db)),
        (
            "max_pre_gain_reduction_db".to_string(),
            Json::num(safety.max_pre_gain_reduction_db),
        ),
        (
            "max_target_scale_db".to_string(),
            Json::num(safety.max_target_scale_db),
        ),
        (
            "target_scale_step_db".to_string(),
            Json::num(safety.target_scale_step_db),
        ),
        ("tolerance_db".to_string(), Json::num(safety.tolerance_db)),
    ])
}

fn objective_config_json(objective: &ObjectiveConfig) -> Json {
    Json::Obj(vec![
        ("kind".to_string(), Json::str(objective.kind.tag())),
        ("beta".to_string(), Json::num(objective.beta)),
        ("p_norm".to_string(), Json::num(objective.p_norm)),
        ("weight_min".to_string(), Json::num(objective.weight_min)),
        ("weight_max".to_string(), Json::num(objective.weight_max)),
        ("k_null".to_string(), Json::num(objective.k_null)),
        ("w_std".to_string(), Json::num(objective.w_std)),
        ("w_null".to_string(), Json::num(objective.w_null)),
        ("w_e".to_string(), Json::num(objective.w_e)),
        (
            "max_db_rise_db".to_string(),
            Json::num(objective.max_db_rise_db),
        ),
        (
            "maxdev_weight".to_string(),
            Json::num(objective.maxdev_weight),
        ),
        (
            "ladder_enabled".to_string(),
            Json::Bool(objective.ladder_enabled),
        ),
        ("ladder_min".to_string(), Json::num(objective.ladder_min)),
        (
            "irls_iterations".to_string(),
            Json::Num(objective.irls_iterations as f64),
        ),
    ])
}

/// 完整 RunConfig 快照（所有影响结果的字段）。
fn run_config_json(cfg: &RunConfig) -> Json {
    Json::Obj(vec![
        ("taps".to_string(), Json::Num(cfg.taps as f64)),
        (
            "delay_samples".to_string(),
            Json::Num(cfg.delay_samples as f64),
        ),
        (
            "irls_iterations".to_string(),
            Json::Num(cfg.irls_iterations as f64),
        ),
        ("lambda_b2_rel".to_string(), Json::num(cfg.lambda_b2_rel)),
        ("lambda_grid_rel".to_string(), num_arr(&cfg.lambda_grid_rel)),
        ("target_beta".to_string(), Json::num(cfg.target_beta)),
        ("diag_load_rel".to_string(), Json::num(cfg.diag_load_rel)),
        ("cv_folds".to_string(), Json::Num(cfg.cv_folds as f64)),
        ("gain_cap_db".to_string(), Json::num(cfg.gain_cap_db)),
        ("nfft".to_string(), Json::Num(cfg.nfft as f64)),
        (
            "objective".to_string(),
            objective_config_json(&cfg.objective),
        ),
        ("quality".to_string(), quality_config_json(&cfg.quality)),
        (
            "authority".to_string(),
            authority_config_json(&cfg.authority),
        ),
        ("safety".to_string(), safety_config_json(&cfg.safety)),
    ])
}

fn fixture_json(fx: &SceneFixture, scene: &str, ground_truth_sha256: &str) -> Json {
    Json::Obj(vec![
        ("dir".to_string(), Json::str(scene.to_string())),
        ("format".to_string(), Json::str(fx.format.clone())),
        ("generator".to_string(), Json::str(fx.generator.clone())),
        (
            "generator_version".to_string(),
            Json::str(fx.generator_version.clone()),
        ),
        (
            "ground_truth_sha256".to_string(),
            Json::str(ground_truth_sha256.to_string()),
        ),
        (
            "rir_length_samples".to_string(),
            Json::Num(fx.rir.length_samples as f64),
        ),
        (
            "rir_fft_size".to_string(),
            Json::Num(fx.rir.fft_size as f64),
        ),
    ])
}

fn input_json(input: &SceneInput) -> Json {
    Json::Obj(vec![
        ("scene_id".to_string(), Json::str(input.scene_id.clone())),
        ("seed".to_string(), Json::Num(input.seed as f64)),
        ("rir_scale".to_string(), Json::num(input.rir_scale)),
        (
            "sample_rate_hz".to_string(),
            Json::num(input.sample_rate_hz),
        ),
        ("freqs_hz".to_string(), num_arr(&input.freqs_hz)),
        ("source_ids".to_string(), str_arr(&input.source_ids)),
        ("mic_ids".to_string(), str_arr(&input.mic_ids)),
        (
            "design_mic_ids".to_string(),
            str_arr(
                &input
                    .design_mics
                    .iter()
                    .map(|index| input.mic_ids[*index].clone())
                    .collect::<Vec<_>>(),
            ),
        ),
        (
            "holdout_mic_ids".to_string(),
            str_arr(
                &input
                    .holdout_mics
                    .iter()
                    .map(|index| input.mic_ids[*index].clone())
                    .collect::<Vec<_>>(),
            ),
        ),
        (
            "band_centers_hz".to_string(),
            num_arr(&input.band_centers_hz),
        ),
        (
            "extended_centers_hz".to_string(),
            num_arr(&input.extended_centers_hz),
        ),
    ])
}

/// 完整输入参数快照（params.json 内容；键排序由 artifact::canonical_json 负责）。
fn build_params(
    args: &Args,
    cfg: &RunConfig,
    completed: &[(String, SceneRun, SceneRecord)],
) -> Json {
    let scenes: Vec<Json> = completed
        .iter()
        .map(|(scene, _run, record)| record.json(scene))
        .collect();
    let cli = Json::Obj(vec![
        ("solver".to_string(), Json::str(args.solver.clone())),
        ("objective".to_string(), Json::str(cfg.objective.kind.tag())),
        (
            "scenes_selected".to_string(),
            str_arr(
                &completed
                    .iter()
                    .map(|(scene, _, _)| scene.clone())
                    .collect::<Vec<_>>(),
            ),
        ),
        ("min_snr_db".to_string(), Json::opt_num(args.min_snr_db)),
        (
            "quality_evidence_mode".to_string(),
            if cfg.quality.enabled {
                Json::str("design_mic_ir_1/3_oct_snr_min_over_sources")
            } else {
                Json::Null
            },
        ),
        ("authority".to_string(), Json::Bool(cfg.authority.enabled)),
        ("safety".to_string(), Json::Bool(cfg.safety.enabled)),
        ("beta".to_string(), Json::opt_num(args.beta)),
        ("target_level".to_string(), Json::opt_num(args.target_level)),
        ("p_norm".to_string(), Json::opt_num(args.p_norm)),
        ("w_std".to_string(), Json::opt_num(args.w_std)),
        ("w_null".to_string(), Json::opt_num(args.w_null)),
        ("w_e".to_string(), Json::opt_num(args.w_e)),
        ("max_db_rise".to_string(), Json::opt_num(args.max_db_rise)),
        (
            "maxdev_weight".to_string(),
            Json::opt_num(args.maxdev_weight),
        ),
    ]);
    Json::Obj(vec![
        ("schema".to_string(), Json::str(artifact::PARAMS_SCHEMA)),
        (
            "tool".to_string(),
            Json::Obj(vec![
                ("name".to_string(), Json::str(TOOL_NAME)),
                ("version".to_string(), Json::str(env!("CARGO_PKG_VERSION"))),
            ]),
        ),
        ("config".to_string(), run_config_json(cfg)),
        ("cli".to_string(), cli),
        ("scenes".to_string(), Json::Arr(scenes)),
        (
            "note".to_string(),
            Json::str(
                "完整输入参数快照：config = 全量 RunConfig（含 quality/authority/safety），\
                 scenes[].input = 夹具派生的算法输入（频点/源/麦/频带），scenes[].fixture 用 \
                 ground-truth.json 的 sha256 标识夹具数据（WAV 内容由夹具加载器逐文件校验），\
                 scenes[].quality_evidence = 质量门实际使用的逐带 SNR 证据；\
                 输出目录（--out/--out-dir）不影响结果，故不写入本快照。",
            ),
        ),
    ])
}

/// 旧布局（<out>/<scene>/）逐字节保持原有内容，保持向后兼容。
fn write_scene_outputs(scene_out: &Path, run: &SceneRun) -> Result<(), String> {
    write_file(
        &scene_out.join(artifact::REPORT_FILE),
        &run.report.to_pretty(),
    )?;
    for (rel, content) in &run.fir_files {
        write_file(&scene_out.join(rel), content)?;
    }
    write_file(&scene_out.join("metrics.csv"), &run.metrics_csv)?;
    Ok(())
}

/// 4.5 产物文件清单：单场景平铺（与旧布局同名），多场景放 scenes/<scene>/。
fn build_artifact_files(completed: &[(String, SceneRun, SceneRecord)]) -> Vec<ArtifactFile> {
    let single_scene = completed.len() == 1;
    let mut files = Vec::new();
    for (scene, run, _record) in completed {
        let prefix = if single_scene {
            String::new()
        } else {
            format!("scenes/{}/", scene)
        };
        files.push(ArtifactFile::new(
            format!("{}{}", prefix, artifact::REPORT_FILE),
            FileRole::Report,
            Some(scene.clone()),
            run.report.to_pretty().into_bytes(),
        ));
        files.push(ArtifactFile::new(
            format!("{}metrics.csv", prefix),
            FileRole::Aux,
            Some(scene.clone()),
            run.metrics_csv.clone().into_bytes(),
        ));
        for (rel, content) in &run.fir_files {
            files.push(ArtifactFile::new(
                format!("{}{}", prefix, rel),
                FileRole::Plan,
                Some(scene.clone()),
                content.clone().into_bytes(),
            ));
        }
    }
    files
}

// ---------- 设计模式 ----------

/// 跑一个场景：加载夹具 → 派生质量门证据 → run_scene；返回运行结果 + 参数快照。
fn design_scene(
    fixtures: &Path,
    scene: &str,
    cfg: &RunConfig,
    args: &Args,
) -> Result<(SceneRun, SceneRecord), String> {
    if args.solver != "own" {
        return Err("--solver modal 尚未实现（P2；own 后端为默认设计结果）".to_string());
    }
    let dir = fixtures.join(scene);
    let fx =
        SceneFixture::load(&dir).map_err(|e| format!("加载夹具 {} 失败: {}", dir.display(), e))?;
    let hm = fx.h_matrix()?;
    let ground_truth_sha256 = sha256::file_hex(&dir.join("ground-truth.json"))?;
    let input = build_input(&fx, &hm, scene, ground_truth_sha256.clone())?;

    let mut scene_cfg = cfg.clone();
    let mut quality_evidence = None;
    if cfg.quality.enabled {
        let quality = quality_config_from_fixture(&fx, &input.design_mics, cfg.quality.min_snr_db);
        quality_evidence = Some(quality_config_json(&quality));
        scene_cfg.quality = quality;
    }

    let run = run_scene(&input, &scene_cfg)?;
    let record = SceneRecord {
        fixture: fixture_json(&fx, scene, &ground_truth_sha256),
        input: input_json(&input),
        quality_evidence,
    };
    Ok((run, record))
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
    if cfg.quality.enabled {
        println!(
            "quality gate: min_snr_db={} evidence = design-mic IR 1/3-oct SNR (min over sources; Unknown -> excluded from fit)",
            cfg.quality.min_snr_db
        );
    }
    if cfg.authority.enabled {
        println!(
            "authority weighting: ON Gamma_s(f)=Gamma0/|H_s(f)|^2 eps_rel={:e} max_gamma={}",
            cfg.authority.eps_rel, cfg.authority.max_gamma
        );
    }
    if cfg.safety.enabled {
        println!(
            "safety clamp: ON band_boost<=+{} dB positive_sum<=+{} dB predicted_peak<={} dBFS",
            cfg.safety.max_band_boost_db,
            cfg.safety.max_positive_sum_db,
            cfg.safety.max_predicted_peak_dbfs
        );
    }
    let mut rows: Vec<String> = Vec::new();
    let mut failures: Vec<(String, String)> = Vec::new();
    let mut completed: Vec<(String, SceneRun, SceneRecord)> = Vec::new();
    let mut sorted = scenes.to_vec();
    sorted.sort_by_key(|s| scene_sort_key(s));
    for scene in &sorted {
        let t0 = std::time::Instant::now();
        match design_scene(&fixtures, scene, &cfg, args) {
            Ok((run, record)) => {
                if let Err(e) = write_scene_outputs(&out.join(scene), &run) {
                    eprintln!("[FAIL] {}: {}", scene, e);
                    failures.push((scene.clone(), e));
                    continue;
                }
                let line = summary_line(scene, &run.report);
                println!("[ok] {}  ({:.2}s)", line, t0.elapsed().as_secs_f64());
                rows.push(line);
                completed.push((scene.clone(), run, record));
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

    // ---- 4.5 可复现产物（一次成功运行 = 一个自洽目录）----
    let params = build_params(args, &cfg, &completed);
    let params_text = artifact::canonical_json(&params);
    let params_sha256 = sha256::hex_digest(params_text.as_bytes());
    let artifact_dir = match &args.out_dir {
        Some(dir) => PathBuf::from(dir),
        None => out.join(artifact::run_dir_name(&params_sha256)),
    };
    let files = build_artifact_files(&completed);
    let outcome = artifact::write_run(RunWriteRequest {
        dir: &artifact_dir,
        tool_name: TOOL_NAME,
        tool_version: env!("CARGO_PKG_VERSION"),
        params_text,
        files,
        plan: None,
        plan_absent_reason: PLAN_ABSENT_REASON,
    })?;
    println!(
        "artifacts: {} ({} files, run_id={})",
        outcome.dir.display(),
        outcome.file_count,
        outcome.run_id
    );
    println!("  manifest = {}", outcome.manifest_path.display());
    println!("  params_sha256 = {}", outcome.params_sha256);
    println!("  result_sha256 = {}", outcome.result_sha256);
    println!("  verify: mimo-opt --verify {}", outcome.dir.display());

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
        ("format", Json::str("khaen-mimo-verify/v1")),
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

// ---------- 4.5 产物校验（--verify <dir>） ----------

fn run_verify_artifact(dir: &str) -> Result<(), String> {
    let dir = PathBuf::from(dir);
    println!("mimo-opt --verify: {}", dir.display());
    let outcome = artifact::verify_run(&dir);
    for check in &outcome.checks {
        println!(
            "[{}] {}: {}",
            if check.ok { "ok" } else { "FAIL" },
            check.subject,
            check.detail
        );
    }
    if !outcome.ok() {
        eprintln!(
            "mimo-opt: verify FAILED（{} 项不符）:",
            outcome.errors.len()
        );
        for error in &outcome.errors {
            eprintln!("  - {}", error);
        }
        return Err(format!(
            "产物校验失败: {} 个文件/字段不符",
            outcome.errors.len()
        ));
    }
    println!(
        "mimo-opt: verify OK: {} 个文件, params_sha256={}, result_sha256={}",
        outcome.file_count,
        outcome.params_sha256.clone().unwrap_or_default(),
        outcome.result_sha256.clone().unwrap_or_default()
    );
    Ok(())
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    if let Some(dir) = &args.verify_artifact {
        return run_verify_artifact(dir);
    }
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
