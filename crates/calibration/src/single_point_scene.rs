//! T66v5：单点场景作业桥（合成场景跑批 → calibration job 证据链）。
//!
//! 把 `khaen-mimo-design` 的场景级单点求解（质量门 4.1 / 权威加权 4.4 / 安全钳位三态 4.4）
//! 接成一条可交付的作业：跑场景 → 产出 report/metrics/FIR 三类 artifact → 登记进校准 job。
//!
//! # 诚实边界（重要）
//!
//! `khaen-mimo-design` 是**合成场景沙盒**（其 lib.rs 与报告都写着 SIMULATED / UNPROVEN）。
//! 本桥只负责"把合成场景的结论变成可校验的证据链文件"，**不**把 predicted 写成 measured：
//! - 报告里 `judgement.measured/applied` 原样透传（当前恒为 false）；
//! - `summary.claim` 直接取沙盒的 CLAIM 文本；
//! - 本模块不产生 FULL/PARTIAL/NONE 判定，只把报告里已有的 `outcome`/`variant outcome` 透出。
//! 真实测量输入、apply 到设备、闭环复测仍属 NOT_RUN（需要设备窗口）。
//!
//! # 产物
//!
//! | 路径 | 内容 |
//! | --- | --- |
//! | `single-point/scene-report.json` | 场景报告（`Json::to_pretty`，确定性序列化） |
//! | `single-point/metrics.csv` | 指标 CSV |
//! | `single-point/fir/<name>` | 每个变体/路径的 FIR CSV（名字来自场景，做安全文件名检查） |

use khaen_mimo_design::pipeline::{
    run_scene, RunConfig, SceneInput, CLAIM, REPORT_FORMAT, TOOL_NAME,
};
use khaen_mimo_design::report::Json;

use crate::job::{ArtifactKind, ArtifactRecord, StageCtx, StageFailure};

/// 稳定错误码（可被 REST / manifest 直接匹配，不要改字符串）。
pub mod error {
    /// 场景求解本身失败（run_scene 返回 Err）。
    pub const RUN: &str = "E_SINGLE_POINT_SCENE_RUN";
    /// 报告结构不符合预期（缺少可透出的三态/判据块时不算错，只有形状非法才算）。
    pub const REPORT: &str = "E_SINGLE_POINT_SCENE_REPORT";
    /// FIR 文件名不安全（含路径分隔符、控制字符等）。
    pub const FIR_PATH: &str = "E_SINGLE_POINT_SCENE_FIR_PATH";
}

/// 场景报告 artifact 相对路径。
pub const REPORT_ARTIFACT_PATH: &str = "single-point/scene-report.json";
/// 指标 CSV artifact 相对路径。
pub const METRICS_ARTIFACT_PATH: &str = "single-point/metrics.csv";
/// FIR CSV artifact 目录。
pub const FIR_ARTIFACT_DIR: &str = "single-point/fir";

/// 桥错误：稳定码 + 说明。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneBridgeError {
    code: &'static str,
    message: String,
}

impl SceneBridgeError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 稳定错误码。
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// 人读说明。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl core::fmt::Display for SceneBridgeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SceneBridgeError {}

/// 从报告里透出的摘要（用于 REST/日志；**不新增判定**）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneSummary {
    /// 报告 schema。
    pub report_format: &'static str,
    /// 工具名。
    pub tool: &'static str,
    /// 沙盒声明（原样，例如 "synthetic offline study only; no real room, no measurement"）。
    pub claim: &'static str,
    /// 顶层三态（安全钳位启用时才有）。
    pub outcome: Option<String>,
    /// 顶层三态对应的变体名（例如 "B3"）。
    pub outcome_variant: Option<String>,
    /// 顶层 `predicted_status`（可能为 None）。
    pub predicted_status: Option<String>,
    /// 每个变体的三态（`variants[].key` + `variants[].outcome`），按报告顺序。
    pub variant_outcomes: Vec<(String, String)>,
    /// 质量门排除（Unknown）的 1/3 oct 频带数；质量门关闭时为 None。
    pub excluded_bands: Option<usize>,
    /// 报告里 `judgement.measured`（当前恒 false；透传，不修饰）。
    pub measured: bool,
    /// 报告里 `judgement.applied`（当前恒 false；透传，不修饰）。
    pub applied: bool,
}

/// 一次场景作业的产物（尚未落盘；落盘/登记见 [`write_scene_artifacts`]）。
#[derive(Clone, Debug)]
pub struct SceneArtifacts {
    /// `single-point/scene-report.json` 的字节内容（UTF-8）。
    pub report_json: String,
    /// `single-point/metrics.csv` 的内容。
    pub metrics_csv: String,
    /// (artifact 相对路径, FIR CSV 内容)，路径已做安全校验。
    pub fir_files: Vec<(String, String)>,
    /// 摘要（透传报告事实）。
    pub summary: SceneSummary,
}

/// 跑一次合成场景并收集三类 artifact（纯离线，不碰设备）。
pub fn run_single_point_scene(
    input: &SceneInput,
    config: &RunConfig,
) -> Result<SceneArtifacts, SceneBridgeError> {
    let run =
        run_scene(input, config).map_err(|message| SceneBridgeError::new(error::RUN, message))?;
    let report_json = run.report.to_pretty();
    let summary = summarize(&run.report)?;
    let mut fir_files = Vec::with_capacity(run.fir_files.len());
    for (name, csv) in &run.fir_files {
        fir_files.push((fir_artifact_path(name)?, csv.clone()));
    }
    Ok(SceneArtifacts {
        report_json,
        metrics_csv: run.metrics_csv,
        fir_files,
        summary,
    })
}

/// FIR artifact 相对路径。
///
/// 场景给的 FIR 名自带子目录（例如 `fir/B2_SUB1.csv`）；这里只做**安全路径**校验：
/// 非空、不以 '/' 开头、无 `//`、每段非空且不是 `.`/`..`、只用 ASCII 字母数字与 `-_.`。
/// 校验失败返回稳定码，绝不让场景数据决定一个可以跳出 artifacts 目录的路径。
pub fn fir_artifact_path(name: &str) -> Result<String, SceneBridgeError> {
    let safe_segment = |segment: &str| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    let valid = !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('/')
        && !name.contains("//")
        && name.split('/').all(safe_segment);
    if !valid {
        return Err(SceneBridgeError::new(
            error::FIR_PATH,
            format!("FIR file name is not a safe artifact name: {name:?}"),
        ));
    }
    Ok(format!("single-point/{name}"))
}

/// 把场景产物写进 job 的 artifacts 目录并登记（不可变语义由 job 负责）。
pub fn write_scene_artifacts(
    ctx: &StageCtx<'_>,
    artifacts: &SceneArtifacts,
    version: &str,
) -> Result<Vec<ArtifactRecord>, StageFailure> {
    if artifacts.report_json.is_empty() || artifacts.metrics_csv.is_empty() {
        return Err(StageFailure::failed(
            error::REPORT,
            "scene artifacts are empty: report/metrics must be present",
        ));
    }
    let mut records = Vec::with_capacity(artifacts.fir_files.len() + 2);
    records.push(ctx.write_artifact(
        ArtifactKind::Other("single_point_scene_report".to_string()),
        REPORT_ARTIFACT_PATH,
        artifacts.report_json.as_bytes(),
        version,
    )?);
    records.push(ctx.write_artifact(
        ArtifactKind::Other("single_point_scene_metrics".to_string()),
        METRICS_ARTIFACT_PATH,
        artifacts.metrics_csv.as_bytes(),
        version,
    )?);
    for (path, csv) in &artifacts.fir_files {
        records.push(ctx.write_artifact(
            ArtifactKind::Other("single_point_scene_fir".to_string()),
            path,
            csv.as_bytes(),
            version,
        )?);
    }
    Ok(records)
}

fn summarize(report: &Json) -> Result<SceneSummary, SceneBridgeError> {
    let Json::Obj(items) = report else {
        return Err(SceneBridgeError::new(
            error::REPORT,
            "scene report is not a JSON object",
        ));
    };
    let get = |key: &str| -> Option<&Json> {
        items
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    };
    let string_at = |value: Option<&Json>| match value {
        Some(Json::Str(text)) => Some(text.clone()),
        _ => None,
    };
    let outcome = string_at(get("outcome"));
    let outcome_variant = string_at(get("outcome_variant"));
    let predicted_status = string_at(get("predicted_status"));

    // variants 是对象（键 = B0/B1/B2/B3），每个变体对象里可能带 "outcome"（4.4 安全钳位启用时）。
    let mut variant_outcomes = Vec::new();
    if let Some(Json::Obj(variants)) = get("variants") {
        for (name, variant) in variants {
            let Json::Obj(fields) = variant else {
                return Err(SceneBridgeError::new(
                    error::REPORT,
                    "variant entries must be JSON objects",
                ));
            };
            let state = fields
                .iter()
                .find(|(key, _)| key == "outcome")
                .map(|(_, value)| value);
            if let Some(Json::Str(state)) = state {
                variant_outcomes.push((name.clone(), state.clone()));
            }
        }
    }

    let excluded_bands = match get("quality") {
        None => None,
        Some(Json::Obj(fields)) => {
            let bands = fields
                .iter()
                .find(|(name, _)| name == "bands")
                .map(|(_, value)| value);
            match bands {
                Some(Json::Arr(bands)) => {
                    let mut unknown = 0usize;
                    for band in bands {
                        let Json::Obj(band_fields) = band else {
                            return Err(SceneBridgeError::new(
                                error::REPORT,
                                "quality.bands entries must be JSON objects",
                            ));
                        };
                        let state = band_fields
                            .iter()
                            .find(|(name, _)| name == "state")
                            .map(|(_, value)| value);
                        if matches!(state, Some(Json::Str(text)) if text == "unknown") {
                            unknown += 1;
                        }
                    }
                    Some(unknown)
                }
                _ => {
                    return Err(SceneBridgeError::new(
                        error::REPORT,
                        "quality.bands must be a JSON array",
                    ))
                }
            }
        }
        Some(_) => {
            return Err(SceneBridgeError::new(
                error::REPORT,
                "quality must be a JSON object",
            ))
        }
    };

    let (mut measured, mut applied) = (false, false);
    if let Some(Json::Obj(judgement)) = get("judgement") {
        for (key, value) in judgement {
            match (key.as_str(), value) {
                ("measured", Json::Bool(flag)) => measured = *flag,
                ("applied", Json::Bool(flag)) => applied = *flag,
                _ => {}
            }
        }
    }

    Ok(SceneSummary {
        report_format: REPORT_FORMAT,
        tool: TOOL_NAME,
        claim: CLAIM,
        outcome,
        outcome_variant,
        predicted_status,
        variant_outcomes,
        excluded_bands,
        measured,
        applied,
    })
}
