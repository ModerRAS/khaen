//! khaen-single-point：离线单点校准命令行（场景 JSON → 可复现 artifact + 三态摘要）。
//!
//! 这是 T66v5 场景作业桥的**可交付入口**：把 `khaen-mimo-design` 的合成场景单点求解
//! （4.1 质量门 / 4.4 权威加权 / 4.4 安全钳位 FULL/PARTIAL/NONE）跑成一组可校验的文件。
//!
//! # 用法
//!
//!     khaen-single-point --scene scene.json --out out/
//!
//! - 输入：`schema = khaen-single-point-scene-input/v1`（结构见 `README` / 测试夹具）。
//! - 输出：`out/single-point/scene-report.json`、`out/single-point/metrics.csv`、
//!   `out/single-point/fir/*.csv`、`out/manifest.json`（逐文件 sha256 + 整包 sha256）、
//!   `out/summary.json`。
//! - stdout：一行 JSON（`schema = khaen-single-point-scene-result/v1`），机器可读。
//!
//! # 退出码
//!
//! | 码 | 含义 |
//! | --- | --- |
//! | 0 | 跑完；安全钳位启用时 outcome = FULL（或未启用钳位） |
//! | 3 | 跑完；outcome = PARTIAL（有钳位/残差，见 report.json） |
//! | 4 | 跑完；outcome = NONE（限值内无可行解） |
//! | 2 | 输入/用法错误（`E_USAGE` / `E_INPUT`） |
//! | 1 | 运行错误（`E_RUN` / `E_WRITE`） |
//!
//! 三态是**结果**不是失败：PARTIAL/NONE 也返回可校验产物，调用方按退出码区分。
//!
//! # 诚实边界
//!
//! 输入是合成场景（`synthetic` / SIMULATED），产物里 `measured/applied` 恒 false；
//! 本工具不把 predicted 写成 measured，也不做真实的采集/下发/复测。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use khaen_calibration::job::sha256_hex;
use khaen_calibration::single_point_scene::{
    run_single_point_scene, SceneArtifacts, METRICS_ARTIFACT_PATH, REPORT_ARTIFACT_PATH,
};
use khaen_mimo_design::pipeline::RunConfig;
use khaen_mimo_design::quality::{QualityConfig, THIRD_OCTAVE_CENTERS_HZ_20_20K};
use khaen_mimo_design::{Cx, SceneInput};
use serde_json::{json, Value};

/// 输入文件 schema。
pub const INPUT_SCHEMA: &str = "khaen-single-point-scene-input/v1";
/// stdout 结果 schema。
pub const RESULT_SCHEMA: &str = "khaen-single-point-scene-result/v1";
/// manifest schema。
pub const MANIFEST_SCHEMA: &str = "khaen-single-point-scene-manifest/v1";

/// 稳定错误码。
pub mod error {
    /// 命令行参数缺失/未知。
    pub const USAGE: &str = "E_USAGE";
    /// 输入 JSON 结构/数值不合法。
    pub const INPUT: &str = "E_INPUT";
    /// 场景求解失败。
    pub const RUN: &str = "E_RUN";
    /// 输出目录/文件写入失败。
    pub const WRITE: &str = "E_WRITE";
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CliError {
    code: &'static str,
    message: String,
}

impl CliError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn to_json(&self) -> String {
        json!({ "ok": false, "error": self.code, "message": self.message }).to_string()
    }
}

fn main() -> ExitCode {
    match parse_args() {
        Ok((scene, out)) => match execute(&scene, &out) {
            Ok((result_json, exit_code)) => {
                println!("{result_json}");
                ExitCode::from(exit_code)
            }
            Err(err) => {
                eprintln!("{}", err.to_json());
                ExitCode::from(error_exit_code(err.code))
            }
        },
        Err(err) => {
            eprintln!("{}", err.to_json());
            ExitCode::from(2)
        }
    }
}

/// 错误码 → 退出码：输入/用法错误 2，其它运行期错误 1。
fn error_exit_code(code: &str) -> u8 {
    if code == error::INPUT || code == error::USAGE {
        2
    } else {
        1
    }
}

fn parse_args() -> Result<(PathBuf, PathBuf), CliError> {
    let mut scene: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scene" => {
                scene = Some(PathBuf::from(args.next().ok_or_else(|| {
                    CliError::new(error::USAGE, "--scene requires a path")
                })?));
            }
            "--out" => {
                out = Some(PathBuf::from(args.next().ok_or_else(|| {
                    CliError::new(error::USAGE, "--out requires a path")
                })?));
            }
            "--help" | "-h" => {
                return Err(CliError::new(
                    error::USAGE,
                    "usage: khaen-single-point --scene <scene.json> --out <dir>",
                ));
            }
            other => {
                return Err(CliError::new(
                    error::USAGE,
                    format!("unknown argument: {other}"),
                ));
            }
        }
    }
    let scene = scene.ok_or_else(|| CliError::new(error::USAGE, "missing --scene"))?;
    let out = out.ok_or_else(|| CliError::new(error::USAGE, "missing --out"))?;
    Ok((scene, out))
}

fn execute(scene_path: &Path, out_dir: &Path) -> Result<(String, u8), CliError> {
    let text = std::fs::read_to_string(scene_path).map_err(|err| {
        CliError::new(
            error::INPUT,
            format!("cannot read scene file {}: {err}", scene_path.display()),
        )
    })?;
    let doc: Value = serde_json::from_str(&text)
        .map_err(|err| CliError::new(error::INPUT, format!("scene is not valid JSON: {err}")))?;
    let schema = doc
        .get("schema")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if schema != INPUT_SCHEMA {
        return Err(CliError::new(
            error::INPUT,
            format!("scene schema must be {INPUT_SCHEMA}, got {schema:?}"),
        ));
    }
    let scene = parse_scene(&doc)?;
    let config = parse_run(doc.get("run"))?;
    let artifacts = run_single_point_scene(&scene, &config)
        .map_err(|err| CliError::new(error::RUN, format!("{}: {}", err.code(), err.message())))?;
    let manifest = write_artifacts(out_dir, &artifacts)?;
    let exit_code = match artifacts.summary.outcome.as_deref() {
        None | Some("FULL") => 0,
        Some("PARTIAL") => 3,
        Some("NONE") => 4,
        Some(other) => {
            return Err(CliError::new(
                error::RUN,
                format!("unexpected safety outcome: {other}"),
            ))
        }
    };
    let result = json!({
        "schema": RESULT_SCHEMA,
        "outcome": artifacts.summary.outcome,
        "outcome_variant": artifacts.summary.outcome_variant,
        "predicted_status": artifacts.summary.predicted_status,
        "variant_outcomes": artifacts.summary.variant_outcomes
            .iter()
            .map(|(key, state)| (key.clone(), json!(state)))
            .collect::<serde_json::Map<String, Value>>(),
        "excluded_bands": artifacts.summary.excluded_bands,
        "measured": artifacts.summary.measured,
        "applied": artifacts.summary.applied,
        "claim": artifacts.summary.claim,
        "artifacts_dir": out_dir.to_string_lossy(),
        "files": manifest.files.iter().map(|file| json!({
            "path": file.path,
            "sha256": file.sha256,
            "bytes": file.bytes,
        })).collect::<Vec<_>>(),
        "bundle_sha256": manifest.bundle_sha256,
    });
    Ok((result.to_string(), exit_code))
}

struct ManifestFile {
    path: String,
    sha256: String,
    bytes: usize,
}

struct Manifest {
    files: Vec<ManifestFile>,
    bundle_sha256: String,
}

fn write_artifacts(out_dir: &Path, artifacts: &SceneArtifacts) -> Result<Manifest, CliError> {
    ensure_empty_or_absent(out_dir)?;
    std::fs::create_dir_all(out_dir)
        .map_err(|err| CliError::new(error::WRITE, format!("cannot create out dir: {err}")))?;

    let mut pending: Vec<(String, Vec<u8>)> = Vec::new();
    pending.push((
        REPORT_ARTIFACT_PATH.to_string(),
        artifacts.report_json.as_bytes().to_vec(),
    ));
    pending.push((
        METRICS_ARTIFACT_PATH.to_string(),
        artifacts.metrics_csv.as_bytes().to_vec(),
    ));
    for (path, csv) in &artifacts.fir_files {
        pending.push((path.clone(), csv.as_bytes().to_vec()));
    }

    let mut files = Vec::with_capacity(pending.len());
    for (path, bytes) in pending {
        let target = out_dir.join(&path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                CliError::new(
                    error::WRITE,
                    format!("cannot create directory {}: {err}", parent.display()),
                )
            })?;
        }
        std::fs::write(&target, &bytes).map_err(|err| {
            CliError::new(
                error::WRITE,
                format!("cannot write {}: {err}", target.display()),
            )
        })?;
        files.push(ManifestFile {
            path,
            sha256: sha256_hex(&bytes),
            bytes: bytes.len(),
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));

    let mut digest_input = String::new();
    for file in &files {
        digest_input.push_str(&file.path);
        digest_input.push('\t');
        digest_input.push_str(&file.sha256);
        digest_input.push('\n');
    }
    let bundle_sha256 = sha256_hex(digest_input.as_bytes());

    let manifest_json = json!({
        "schema": MANIFEST_SCHEMA,
        "files": files.iter().map(|file| json!({
            "path": file.path,
            "sha256": file.sha256,
            "bytes": file.bytes,
        })).collect::<Vec<_>>(),
        "bundle_sha256": bundle_sha256,
    });
    let mut manifest_text = serde_json::to_string_pretty(&manifest_json)
        .map_err(|err| CliError::new(error::WRITE, format!("cannot serialize manifest: {err}")))?;
    manifest_text.push('\n');
    write_text(out_dir, "manifest.json", &manifest_text)?;

    let summary_json = json!({
        "schema": "khaen-single-point-scene-summary/v1",
        "outcome": artifacts.summary.outcome,
        "outcome_variant": artifacts.summary.outcome_variant,
        "predicted_status": artifacts.summary.predicted_status,
        "variant_outcomes": artifacts.summary.variant_outcomes
            .iter()
            .map(|(key, state)| (key.clone(), json!(state)))
            .collect::<serde_json::Map<String, Value>>(),
        "excluded_bands": artifacts.summary.excluded_bands,
        "measured": artifacts.summary.measured,
        "applied": artifacts.summary.applied,
        "claim": artifacts.summary.claim,
        "bundle_sha256": bundle_sha256,
    });
    let mut summary_text = serde_json::to_string_pretty(&summary_json)
        .map_err(|err| CliError::new(error::WRITE, format!("cannot serialize summary: {err}")))?;
    summary_text.push('\n');
    write_text(out_dir, "summary.json", &summary_text)?;

    Ok(Manifest {
        files,
        bundle_sha256,
    })
}

fn write_text(out_dir: &Path, name: &str, text: &str) -> Result<(), CliError> {
    let target = out_dir.join(name);
    std::fs::write(&target, text.as_bytes()).map_err(|err| {
        CliError::new(
            error::WRITE,
            format!("cannot write {}: {err}", target.display()),
        )
    })
}

fn ensure_empty_or_absent(dir: &Path) -> Result<(), CliError> {
    match std::fs::read_dir(dir) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                return Err(CliError::new(
                    error::WRITE,
                    format!(
                        "output directory must be empty or absent: {}",
                        dir.display()
                    ),
                ));
            }
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(CliError::new(
            error::WRITE,
            format!("cannot inspect output directory {}: {err}", dir.display()),
        )),
    }
}

fn parse_scene(doc: &Value) -> Result<SceneInput, CliError> {
    let scene = doc
        .get("scene")
        .ok_or_else(|| CliError::new(error::INPUT, "missing field: scene"))?;
    let string_field = |name: &str| -> Result<String, CliError> {
        scene
            .get(name)
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .ok_or_else(|| CliError::new(error::INPUT, format!("scene.{name} must be a string")))
    };
    let number_field = |name: &str| -> Result<f64, CliError> {
        scene
            .get(name)
            .and_then(Value::as_f64)
            .ok_or_else(|| CliError::new(error::INPUT, format!("scene.{name} must be a number")))
    };
    let u64_field = |name: &str| -> Result<u64, CliError> {
        scene.get(name).and_then(Value::as_u64).ok_or_else(|| {
            CliError::new(
                error::INPUT,
                format!("scene.{name} must be a non-negative integer"),
            )
        })
    };
    let string_list = |name: &str| -> Result<Vec<String>, CliError> {
        let items = scene
            .get(name)
            .and_then(Value::as_array)
            .ok_or_else(|| CliError::new(error::INPUT, format!("scene.{name} must be an array")))?;
        items
            .iter()
            .map(|item| {
                item.as_str().map(ToString::to_string).ok_or_else(|| {
                    CliError::new(
                        error::INPUT,
                        format!("scene.{name} entries must be strings"),
                    )
                })
            })
            .collect()
    };
    let index_list = |name: &str| -> Result<Vec<usize>, CliError> {
        let items = scene
            .get(name)
            .and_then(Value::as_array)
            .ok_or_else(|| CliError::new(error::INPUT, format!("scene.{name} must be an array")))?;
        items
            .iter()
            .map(|item| {
                item.as_u64().map(|value| value as usize).ok_or_else(|| {
                    CliError::new(
                        error::INPUT,
                        format!("scene.{name} entries must be non-negative integers"),
                    )
                })
            })
            .collect()
    };
    let number_list = |name: &str| -> Result<Vec<f64>, CliError> {
        let items = scene
            .get(name)
            .and_then(Value::as_array)
            .ok_or_else(|| CliError::new(error::INPUT, format!("scene.{name} must be an array")))?;
        items
            .iter()
            .map(|item| {
                item.as_f64().ok_or_else(|| {
                    CliError::new(
                        error::INPUT,
                        format!("scene.{name} entries must be numbers"),
                    )
                })
            })
            .collect()
    };

    let freqs_hz = number_list("freqs_hz")?;
    let source_ids = string_list("source_ids")?;
    let mic_ids = string_list("mic_ids")?;
    let design_mics = index_list("design_mics")?;
    let holdout_mics = index_list("holdout_mics")?;
    let band_centers_hz = number_list("band_centers_hz")?;
    let extended_centers_hz = number_list("extended_centers_hz")?;
    let sample_rate_hz = number_field("sample_rate_hz")?;
    let seed = u64_field("seed")?;
    let rir_scale = number_field("rir_scale")?;
    if freqs_hz.is_empty() {
        return Err(CliError::new(
            error::INPUT,
            "scene.freqs_hz must not be empty",
        ));
    }
    if source_ids.is_empty() {
        return Err(CliError::new(
            error::INPUT,
            "scene.source_ids must not be empty",
        ));
    }
    if band_centers_hz.is_empty() {
        return Err(CliError::new(
            error::INPUT,
            "scene.band_centers_hz must not be empty",
        ));
    }
    if design_mics.is_empty() {
        return Err(CliError::new(
            error::INPUT,
            "scene.design_mics must not be empty",
        ));
    }
    for index in design_mics.iter().chain(holdout_mics.iter()) {
        if *index >= mic_ids.len() {
            return Err(CliError::new(
                error::INPUT,
                format!(
                    "scene mic index {index} is out of range ({} mics)",
                    mic_ids.len()
                ),
            ));
        }
    }

    let h_value = scene
        .get("h")
        .and_then(Value::as_array)
        .ok_or_else(|| CliError::new(error::INPUT, "scene.h must be an array"))?;
    if h_value.len() != mic_ids.len() {
        return Err(CliError::new(
            error::INPUT,
            format!(
                "scene.h has {} mics but mic_ids has {}",
                h_value.len(),
                mic_ids.len()
            ),
        ));
    }
    let mut h: Vec<Vec<Vec<Cx>>> = Vec::with_capacity(h_value.len());
    for (mic_index, mic) in h_value.iter().enumerate() {
        let bins = mic.as_array().ok_or_else(|| {
            CliError::new(
                error::INPUT,
                format!("scene.h[{mic_index}] must be an array of bins"),
            )
        })?;
        if bins.len() != freqs_hz.len() {
            return Err(CliError::new(
                error::INPUT,
                format!(
                    "scene.h[{mic_index}] has {} bins but freqs_hz has {}",
                    bins.len(),
                    freqs_hz.len()
                ),
            ));
        }
        let mut mic_bins = Vec::with_capacity(bins.len());
        for (bin_index, bin) in bins.iter().enumerate() {
            let sources = bin.as_array().ok_or_else(|| {
                CliError::new(
                    error::INPUT,
                    format!("scene.h[{mic_index}][{bin_index}] must be an array of sources"),
                )
            })?;
            if sources.len() != source_ids.len() {
                return Err(CliError::new(
                    error::INPUT,
                    format!(
                        "scene.h[{mic_index}][{bin_index}] has {} sources but source_ids has {}",
                        sources.len(),
                        source_ids.len()
                    ),
                ));
            }
            let mut complex = Vec::with_capacity(sources.len());
            for (source_index, value) in sources.iter().enumerate() {
                let pair = value.as_array().ok_or_else(|| {
                    CliError::new(
                        error::INPUT,
                        format!(
                            "scene.h[{mic_index}][{bin_index}][{source_index}] must be [re, im]"
                        ),
                    )
                })?;
                if pair.len() != 2 {
                    return Err(CliError::new(
                        error::INPUT,
                        "complex values must be [re, im] pairs".to_string(),
                    ));
                }
                let re = pair[0].as_f64().ok_or_else(|| {
                    CliError::new(error::INPUT, "complex real part must be a number")
                })?;
                let im = pair[1].as_f64().ok_or_else(|| {
                    CliError::new(error::INPUT, "complex imaginary part must be a number")
                })?;
                if !re.is_finite() || !im.is_finite() {
                    return Err(CliError::new(
                        error::INPUT,
                        "complex values must be finite".to_string(),
                    ));
                }
                complex.push(Cx::new(re, im));
            }
            mic_bins.push(complex);
        }
        h.push(mic_bins);
    }

    Ok(SceneInput {
        scene_id: string_field("scene_id")?,
        fixture_dir: string_field("fixture_dir").unwrap_or_else(|_| "synthetic".to_string()),
        ground_truth_sha256: string_field("ground_truth_sha256").unwrap_or_else(|_| "0".repeat(64)),
        seed,
        rir_scale,
        sample_rate_hz,
        freqs_hz,
        source_ids,
        mic_ids,
        design_mics,
        holdout_mics,
        h,
        band_centers_hz,
        extended_centers_hz,
    })
}

fn parse_run(value: Option<&Value>) -> Result<RunConfig, CliError> {
    let mut config = RunConfig::default();
    let Some(value) = value else {
        return Ok(config);
    };
    let object = value
        .as_object()
        .ok_or_else(|| CliError::new(error::INPUT, "run must be a JSON object"))?;
    if let Some(taps) = object.get("taps").and_then(Value::as_u64) {
        config.taps = taps as usize;
    }
    if let Some(delay) = object.get("delay_samples").and_then(Value::as_u64) {
        config.delay_samples = delay as usize;
    }
    if let Some(nfft) = object.get("nfft").and_then(Value::as_u64) {
        config.nfft = nfft as usize;
    }
    if let Some(iterations) = object.get("irls_iterations").and_then(Value::as_u64) {
        config.irls_iterations = iterations as usize;
    }
    if let Some(beta) = object.get("target_beta").and_then(Value::as_f64) {
        config.target_beta = beta;
    }
    if let Some(quality) = object.get("quality") {
        config.quality = parse_quality(quality)?;
    }
    if let Some(authority) = object.get("authority") {
        let authority = authority
            .as_object()
            .ok_or_else(|| CliError::new(error::INPUT, "run.authority must be an object"))?;
        config.authority.enabled = authority
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    }
    if let Some(safety) = object.get("safety") {
        let safety = safety
            .as_object()
            .ok_or_else(|| CliError::new(error::INPUT, "run.safety must be an object"))?;
        config.safety.enabled = safety
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if let Some(tolerance) = safety.get("tolerance_db").and_then(Value::as_f64) {
            config.safety.tolerance_db = tolerance;
        }
        if let Some(peak) = safety
            .get("max_predicted_peak_dbfs")
            .and_then(Value::as_f64)
        {
            config.safety.max_predicted_peak_dbfs = peak;
        }
    }
    Ok(config)
}

fn parse_quality(value: &Value) -> Result<QualityConfig, CliError> {
    let object = value
        .as_object()
        .ok_or_else(|| CliError::new(error::INPUT, "run.quality must be an object"))?;
    let enabled = object
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !enabled {
        return Ok(QualityConfig::default());
    }
    let centers = THIRD_OCTAVE_CENTERS_HZ_20_20K.to_vec();
    let min_snr_db = object
        .get("min_snr_db")
        .and_then(Value::as_f64)
        .unwrap_or(khaen_mimo_design::quality::DEFAULT_MIN_SNR_DB);
    let band_snr_db =
        match object.get("band_snr_db") {
            None => return Err(CliError::new(
                error::INPUT,
                "run.quality.enabled requires run.quality.band_snr_db (per 1/3 oct band evidence)",
            )),
            Some(Value::Array(items)) => {
                if items.len() != centers.len() {
                    return Err(CliError::new(
                        error::INPUT,
                        format!(
                        "run.quality.band_snr_db has {} entries but {} 1/3 oct bands are expected",
                        items.len(),
                        centers.len()
                    ),
                    ));
                }
                items
                    .iter()
                    .map(|item| match item {
                        Value::Null => Ok(None),
                        Value::Number(_) => item.as_f64().map(Some).ok_or_else(|| {
                            CliError::new(error::INPUT, "band_snr_db entry is not a number")
                        }),
                        _ => Err(CliError::new(
                            error::INPUT,
                            "run.quality.band_snr_db entries must be numbers or null",
                        )),
                    })
                    .collect::<Result<Vec<_>, CliError>>()?
            }
            Some(_) => {
                return Err(CliError::new(
                    error::INPUT,
                    "run.quality.band_snr_db must be an array",
                ))
            }
        };
    Ok(QualityConfig {
        enabled: true,
        min_snr_db,
        centers_hz: centers,
        band_snr_db,
        ..QualityConfig::default()
    })
}
