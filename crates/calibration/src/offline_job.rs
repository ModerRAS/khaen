//! T70h-b：校准 Job 的**生产 backend**（离线闭环 pipeline，接 `JobManager`）。
//!
//! 任务口径 = HANDOFF PLAN §4.7：REST 收到 `kind="single_point_offline"` 后，daemon 把
//! 上传的方案（plan_sha）与两段 WAV（baseline/applied upload）落成路径，交给本 backend；
//! 步骤如实映射 CLI（PLAN §4.7：compile -> levels -> closed_loop -> bundle），前面加
//! validate 输入校验步：
//!
//! | stage | 做什么 | 产物（相对 job artifacts_dir，确定性字节） |
//! | --- | --- | --- |
//! | `validate` | 校验请求/输入：WAV 可解析、采样率一致、方案可解码或设计 JSON 可解析、参数合法；stale 输入 sha 拒收 | `inputs.json` |
//! | `compile` | design/plan_compile：输入方案 → ABI v3 解码 + `encode_checked` 规范化；或设计 JSON（modal_peq/v1 或单点 params.json）→ `plan_compile::compile_peq_design` | `plan/plan.bin`、`compile/compile-report.json` |
//! | `levels` | 方案 → 预测带域电平（`plan_levels`）；两段 WAV → 实测相对电平（`band_levels`） | `levels/predicted.json`、`levels/observed.json` |
//! | `closed_loop` | 预测 vs 实测三态判定（`closed_loop`；FAIL/INCOMPLETE 是**结果**不是 stage 失败） | `closed-loop/report.json`、`closed-loop/summary.json` |
//! | `bundle` | 四件套证据包（`closed_loop_run`：predicted/observed/report/manifest，无时间戳） | `bundle/{predicted,observed,report,manifest}.json` |
//!
//! 硬规则（与 `job` 契约一致）：
//! - 不跳步、不假成功：每个 stage 只读**磁盘上**上一 stage 的产物（checkpoint 粒度是 stage，
//!   崩溃 resume 会重跑 in-progress stage）；缺产物按 `E_OFFLINE_JOB_ARTIFACT` 失败。
//! - 每 stage 开头检查 `CancelToken`；长循环（WAV 解码）按文件粒度检查取消。
//! - 证据字节里**不含时间戳/绝对路径/主机名**：同输入两次运行逐字节相同（浮点统一
//!   `{:.9e}`，与 `closed_loop` 报告同一规范；方案/电平由既有确定性序列化给出）。
//! - FAIL/INCOMPLETE 判定照样把 job 跑到 `complete`（step 都成功）；verdict 在
//!   `closed-loop/report.json` 与 `bundle/manifest.json` 里，daemon 从产物读取。
//!
//! NOT_RUN（本任务边界）：真实 UMIK/MOTU 采集（T70f-c）、daemon REST 上传/进度（T36）、
//! 设备侧 apply/rollback（T70h 之后）。

use std::fs;
use std::path::{Path, PathBuf};

use khaen_core::plan::Plan;
use khaen_designer::modal_peq::{BandType, ChannelPeq, DesignMode, ModalBand, PeqDesign};

use crate::band_levels::{relative_band_levels, BandLevelsConfig, DEFAULT_WINDOW_LEN};
use crate::closed_loop::{
    compare, levels_bytes, report_bytes, ClosedLoopConfig, LevelSet, PointLevels,
};
use crate::closed_loop_run::{build_run_bundle, RunConfig, RunInputs};
use crate::job::{
    sha256_hex, ArtifactKind, CalibrationJobBackend, JobStage, StageCtx, StageFailure,
};
use crate::plan_compile::{compile_peq_design, PlanCompileOptions};
use crate::plan_levels::predicted_levels;
use crate::wav::{parse_wav, Wav};

/// 产物 schema（`inputs.json`）。
pub const INPUTS_SCHEMA: &str = "khaen-offline-job-inputs/v1";
/// 产物 schema（`compile/compile-report.json`）。
pub const COMPILE_REPORT_SCHEMA: &str = "khaen-offline-job-compile/v1";
/// 产物 schema（`closed-loop/summary.json`）。
pub const SUMMARY_SCHEMA: &str = "khaen-offline-job-summary/v1";

/// 校验产物相对路径。
pub const INPUTS_FILE: &str = "inputs.json";
/// 方案产物相对路径（与 `plan_compile` 同一约定）。
pub const PLAN_FILE: &str = crate::plan_compile::PLAN_ARTIFACT_PATH;
/// 编译报告相对路径。
pub const COMPILE_REPORT_FILE: &str = "compile/compile-report.json";
/// 预测电平相对路径。
pub const PREDICTED_FILE: &str = "levels/predicted.json";
/// 实测电平相对路径。
pub const OBSERVED_FILE: &str = "levels/observed.json";
/// 闭环判定报告相对路径。
pub const CLOSED_LOOP_REPORT_FILE: &str = "closed-loop/report.json";
/// 闭环摘要（wizard 用；verdict + 预测 vs 实测 band 表）相对路径。
pub const CLOSED_LOOP_SUMMARY_FILE: &str = "closed-loop/summary.json";
/// 四件套证据包目录。
pub const BUNDLE_DIR: &str = "bundle";

/// 稳定错误码（会进 job 快照 `failure.code`；不要改字符串）。
pub mod error {
    /// 输入不可读/不是合法 WAV。
    pub const INPUT: &str = "E_OFFLINE_JOB_INPUT";
    /// 输入 sha256 与请求绑定值不一致（stale 输入）。
    pub const STALE: &str = "E_OFFLINE_JOB_STALE";
    /// 配置非法（测点名/频带/容差/窗长/lane）。
    pub const CONFIG: &str = "E_OFFLINE_JOB_CONFIG";
    /// 采样率不一致（方案 vs 录音 / 两段录音之间）。
    pub const ALIGNMENT: &str = "E_OFFLINE_JOB_ALIGNMENT";
    /// 既没有输入方案也没有设计 JSON（无法编译，拒绝伪造）。
    pub const NO_PLAN: &str = "E_OFFLINE_JOB_NO_PLAN";
    /// 方案 ABI v3 解码/编码失败。
    pub const PLAN: &str = "E_OFFLINE_JOB_PLAN";
    /// 设计 JSON 解析/编译失败。
    pub const DESIGN: &str = "E_OFFLINE_JOB_DESIGN";
    /// 带域电平计算失败。
    pub const LEVELS: &str = "E_OFFLINE_JOB_LEVELS";
    /// 闭环对账失败（配置/对齐层面的错误；verdict 本身不是错误）。
    pub const CLOSED_LOOP: &str = "E_OFFLINE_JOB_CLOSED_LOOP";
    /// 上一 stage 的产物缺失/不可解码（checkpoint resume 不跳步）。
    pub const ARTIFACT: &str = "E_OFFLINE_JOB_ARTIFACT";
    /// 不属于本 backend 的 stage。
    pub const STAGE: &str = "E_OFFLINE_JOB_STAGE";
}

/// 离线闭环 job 的输入配置（daemon 把 REST 的 upload_id/plan_sha 解析成路径后传入）。
///
/// 路径只用于读输入，**不会**写进确定性证据字节；证据只记录 sha256/字节数/采样率。
#[derive(Debug, Clone, PartialEq)]
pub struct OfflineJobConfig {
    /// 基线录音（不加校准滤波）。
    pub baseline_wav: PathBuf,
    /// 施加方案后的录音。
    pub applied_wav: PathBuf,
    /// 已上传的方案文件（ABI v3）；与 `design_json` 二选一（都缺 = `E_OFFLINE_JOB_NO_PLAN`）。
    pub plan_bin: Option<PathBuf>,
    /// 可选设计 JSON（`khaen-modal-peq/v1` 或单点 `params.json`）；plan_bin 缺省时用于编译。
    pub design_json: Option<PathBuf>,
    /// 测点名（报告/清单；ASCII 字母数字与 `-_.`，1..=64）。
    pub point: String,
    /// 方案 lane（预测侧取该 lane）。
    pub lane: u16,
    /// 1/3 oct 中心频率（升序，Hz；空 = `plan_levels::DEFAULT_CENTERS_HZ`）。
    pub centers_hz: Vec<f64>,
    /// 判定容差（dB，必须有限且 > 0）。
    pub tolerance_db: f64,
    /// 最少可比较带数（>= 1）。
    pub min_compared_bands: usize,
    /// 分析窗长（样本，2 的幂，256..=65536；0 = `band_levels::DEFAULT_WINDOW_LEN`）。
    pub window_len: usize,
}

impl OfflineJobConfig {
    /// 最小构造（其余取闭环默认：point=P01、centers=DEFAULT_CENTERS_HZ、2.0 dB、3 带、4096）。
    pub fn new(baseline_wav: impl Into<PathBuf>, applied_wav: impl Into<PathBuf>) -> Self {
        OfflineJobConfig {
            baseline_wav: baseline_wav.into(),
            applied_wav: applied_wav.into(),
            plan_bin: None,
            design_json: None,
            point: "P01".to_string(),
            lane: 0,
            centers_hz: crate::plan_levels::DEFAULT_CENTERS_HZ.to_vec(),
            tolerance_db: 2.0,
            min_compared_bands: 3,
            window_len: DEFAULT_WINDOW_LEN,
        }
    }

    fn window_len(&self) -> usize {
        if self.window_len == 0 {
            DEFAULT_WINDOW_LEN
        } else {
            self.window_len
        }
    }

    fn centers_hz(&self) -> &[f64] {
        &self.centers_hz
    }

    /// 结构校验（不读文件；文件内容错误在 validate stage 报）。
    pub fn validate(&self) -> Result<(), String> {
        if self.baseline_wav.as_os_str().is_empty() || self.applied_wav.as_os_str().is_empty() {
            return Err("baseline/applied WAV paths must be non-empty".to_string());
        }
        if self.plan_bin.is_none() && self.design_json.is_none() {
            return Err(
                "offline job needs plan_bin (ABI v3) or design_json (modal-peq/params)".to_string(),
            );
        }
        if self.point.is_empty() || self.point.chars().count() > 64 {
            return Err("point must be 1..=64 characters".to_string());
        }
        if !self
            .point
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(
                "point may only contain ASCII letters, digits, '-', '_' and '.'".to_string(),
            );
        }
        if self.centers_hz.is_empty() {
            return Err("centers_hz must not be empty".to_string());
        }
        for (index, center) in self.centers_hz.iter().enumerate() {
            if !center.is_finite() || *center <= 0.0 {
                return Err(format!("centers_hz[{index}] must be finite and > 0"));
            }
            if index > 0 && *center <= self.centers_hz[index - 1] {
                return Err("centers_hz must be strictly increasing".to_string());
            }
        }
        if !self.tolerance_db.is_finite() || self.tolerance_db <= 0.0 {
            return Err("tolerance_db must be finite and > 0".to_string());
        }
        if self.min_compared_bands == 0 {
            return Err("min_compared_bands must be >= 1".to_string());
        }
        let window = self.window_len();
        if !window.is_power_of_two()
            || !(crate::band_levels::MIN_WINDOW_LEN..=crate::band_levels::MAX_WINDOW_LEN)
                .contains(&window)
        {
            return Err(format!(
                "window_len must be a power of two in [{}, {}]",
                crate::band_levels::MIN_WINDOW_LEN,
                crate::band_levels::MAX_WINDOW_LEN
            ));
        }
        Ok(())
    }
}

/// 生产 backend：按 `SINGLE_POINT_OFFLINE_PLAN` 的五个 stage 执行离线闭环。
#[derive(Debug, Clone)]
pub struct OfflineJobBackend {
    config: OfflineJobConfig,
}

impl OfflineJobBackend {
    /// 构造（结构非法也构造成功；validate stage 会以稳定码失败，便于 REST 回放同一请求）。
    pub fn new(config: OfflineJobConfig) -> Self {
        OfflineJobBackend { config }
    }

    /// 本 backend 的配置。
    pub fn config(&self) -> &OfflineJobConfig {
        &self.config
    }

    // ----- stage: validate -----

    fn stage_validate(&self, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        self.config
            .validate()
            .map_err(|message| StageFailure::failed(error::CONFIG, message))?;
        check_cancel(ctx)?;
        let inputs = load_inputs_checked(ctx, &self.config)?;
        ctx.report_progress(0.5);
        check_cancel(ctx)?;
        // 方案/设计先“可解析”再进 compile：validate 不写方案，只证明输入结构合法。
        if let Some(path) = &self.config.plan_bin {
            let bytes = read_file(path, "plan_bin")?;
            verify_expected_sha(ctx, "plan", &bytes)?;
            let plan = Plan::decode(&bytes).map_err(|err| {
                StageFailure::failed(
                    error::PLAN,
                    format!(
                        "plan_bin {} is not a valid ABI v3 plan: {err}",
                        path.display()
                    ),
                )
            })?;
            if plan.sample_rate != 0 && plan.sample_rate != inputs.sample_rate_hz as u32 {
                return Err(StageFailure::failed(
                    error::ALIGNMENT,
                    format!(
                        "plan sample_rate {} Hz does not match recordings {} Hz",
                        plan.sample_rate, inputs.sample_rate_hz
                    ),
                ));
            }
        } else if let Some(path) = &self.config.design_json {
            let text = read_text(path, "design_json")?;
            verify_expected_sha(ctx, "design", text.as_bytes())?;
            let design = parse_design_json(&text, inputs.sample_rate_hz as u32)?;
            if design.fs != inputs.sample_rate_hz as u32 {
                return Err(StageFailure::failed(
                    error::ALIGNMENT,
                    format!(
                        "design fs {} does not match recordings {} Hz",
                        design.fs, inputs.sample_rate_hz
                    ),
                ));
            }
        }
        let text = inputs_json(&self.config, &inputs)?;
        ctx.write_artifact(
            ArtifactKind::Other("offline_inputs".to_string()),
            INPUTS_FILE,
            text.as_bytes(),
            INPUTS_SCHEMA,
        )?;
        ctx.report_progress(1.0);
        Ok(())
    }

    // ----- stage: compile (design / plan_compile) -----

    fn stage_compile(&self, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        check_cancel(ctx)?;
        let request = ctx.request();
        let sample_rate = request.sample_rate;
        let (bytes, source, channels) = if let Some(path) = &self.config.plan_bin {
            let raw = read_file(path, "plan_bin")?;
            verify_expected_sha(ctx, "plan", &raw)?;
            let plan = Plan::decode(&raw).map_err(|err| {
                StageFailure::failed(
                    error::PLAN,
                    format!(
                        "plan_bin {} is not a valid ABI v3 plan: {err}",
                        path.display()
                    ),
                )
            })?;
            let canonical = plan.encode_checked().map_err(|err| {
                StageFailure::failed(error::PLAN, format!("plan_bin re-encode failed: {err}"))
            })?;
            let channels = channels_of_plan(&plan);
            (canonical, "input_plan", channels)
        } else if let Some(path) = &self.config.design_json {
            let text = read_text(path, "design_json")?;
            verify_expected_sha(ctx, "design", text.as_bytes())?;
            let design = parse_design_json(&text, sample_rate)?;
            let channels = channels_of_design(&design);
            let options = PlanCompileOptions {
                sample_rate,
                block_size: 128,
                channels,
                lane_gain_db: Vec::new(),
                lane_delay: Vec::new(),
            };
            let compiled = compile_peq_design(&design, &options).map_err(|err| {
                StageFailure::failed(
                    error::DESIGN,
                    format!("design compile failed: {}: {}", err.code(), err.message()),
                )
            })?;
            (compiled.bytes, "design_params", channels)
        } else {
            return Err(StageFailure::failed(
                error::NO_PLAN,
                "offline job has neither plan_bin nor design_json; refusing to fabricate a plan",
            ));
        };
        ctx.report_progress(0.6);
        check_cancel(ctx)?;
        let plan = Plan::decode(&bytes).map_err(|err| {
            StageFailure::failed(error::PLAN, format!("compiled plan does not decode: {err}"))
        })?;
        let plan_sha256 = sha256_hex(&bytes);
        let graph_id = plan.graph_id_hex();
        ctx.write_artifact(
            ArtifactKind::CalibrationDraft,
            PLAN_FILE,
            &bytes,
            COMPILE_REPORT_SCHEMA,
        )?;
        let report = compile_report_json(
            source,
            sample_rate,
            plan.block_size,
            channels,
            &plan_sha256,
            &graph_id,
            bytes.len(),
        );
        ctx.write_artifact(
            ArtifactKind::Other("offline_compile_report".to_string()),
            COMPILE_REPORT_FILE,
            report.as_bytes(),
            COMPILE_REPORT_SCHEMA,
        )?;
        ctx.report_progress(1.0);
        Ok(())
    }

    // ----- stage: levels -----

    fn stage_levels(&self, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        check_cancel(ctx)?;
        let plan = load_plan_artifact(ctx)?;
        let inputs = load_inputs_checked(ctx, &self.config)?;
        check_alignment(&plan, &inputs)?;
        let (predicted, observed) = self.compute_levels(&plan, &inputs)?;
        ctx.report_progress(0.7);
        check_cancel(ctx)?;
        ctx.write_artifact(
            ArtifactKind::Other("predicted_levels".to_string()),
            PREDICTED_FILE,
            &levels_bytes(&predicted),
            INPUTS_SCHEMA,
        )?;
        ctx.write_artifact(
            ArtifactKind::Other("observed_levels".to_string()),
            OBSERVED_FILE,
            &levels_bytes(&observed),
            INPUTS_SCHEMA,
        )?;
        ctx.report_progress(1.0);
        Ok(())
    }

    // ----- stage: closed_loop -----

    fn stage_closed_loop(&self, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        check_cancel(ctx)?;
        let plan = load_plan_artifact(ctx)?;
        let inputs = load_inputs_checked(ctx, &self.config)?;
        check_alignment(&plan, &inputs)?;
        require_artifact(ctx, PREDICTED_FILE)?;
        require_artifact(ctx, OBSERVED_FILE)?;
        let (predicted, observed) = self.compute_levels(&plan, &inputs)?;
        let config = ClosedLoopConfig {
            tolerance_db: self.config.tolerance_db,
            min_compared_bands: self.config.min_compared_bands,
        };
        let report = compare(&predicted, &observed, &config).map_err(|err| {
            StageFailure::failed(
                error::CLOSED_LOOP,
                format!(
                    "closed-loop compare failed: {}: {}",
                    err.code(),
                    err.message()
                ),
            )
        })?;
        ctx.report_progress(0.8);
        ctx.write_artifact(
            ArtifactKind::Other("closed_loop_report".to_string()),
            CLOSED_LOOP_REPORT_FILE,
            &report_bytes(&report),
            SUMMARY_SCHEMA,
        )?;
        let summary = summary_json(&self.config, &report);
        ctx.write_artifact(
            ArtifactKind::Other("closed_loop_summary".to_string()),
            CLOSED_LOOP_SUMMARY_FILE,
            summary.as_bytes(),
            SUMMARY_SCHEMA,
        )?;
        ctx.report_progress(1.0);
        Ok(())
    }

    // ----- stage: bundle -----

    fn stage_bundle(&self, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        check_cancel(ctx)?;
        let plan = load_plan_artifact(ctx)?;
        let inputs = load_inputs_checked(ctx, &self.config)?;
        check_alignment(&plan, &inputs)?;
        require_artifact(ctx, CLOSED_LOOP_REPORT_FILE)?;
        let plan_bytes = read_artifact(ctx, PLAN_FILE)?;
        let plan_sha256 = sha256_hex(&plan_bytes);
        let bundle_config = RunConfig {
            point: self.config.point.clone(),
            lane: self.config.lane,
            centers_hz: self.config.centers_hz().to_vec(),
            tolerance_db: self.config.tolerance_db,
            min_compared_bands: self.config.min_compared_bands,
            window_len: self.config.window_len(),
        };
        let baseline_sha256 = sha256_hex(&inputs.baseline_bytes);
        let applied_sha256 = sha256_hex(&inputs.applied_bytes);
        let run_inputs = RunInputs {
            plan: &plan,
            plan_sha256: &plan_sha256,
            baseline_samples: &inputs.baseline.samples,
            baseline_wav_sha256: &baseline_sha256,
            applied_samples: &inputs.applied.samples,
            applied_wav_sha256: &applied_sha256,
            sample_rate_hz: inputs.sample_rate_hz,
        };
        ctx.report_progress(0.4);
        let bundle = build_run_bundle(&run_inputs, &bundle_config).map_err(|err| {
            StageFailure::failed(
                error::CLOSED_LOOP,
                format!("bundle build failed: {}: {}", err.code(), err.message()),
            )
        })?;
        for (file, bytes) in bundle.files() {
            let relative = format!("{BUNDLE_DIR}/{file}");
            let kind = match file {
                "predicted.json" => ArtifactKind::Other("bundle_predicted".to_string()),
                "observed.json" => ArtifactKind::Other("bundle_observed".to_string()),
                "report.json" => ArtifactKind::Other("bundle_report".to_string()),
                _ => ArtifactKind::Other("bundle_manifest".to_string()),
            };
            ctx.write_artifact(kind, &relative, bytes, SUMMARY_SCHEMA)?;
        }
        ctx.report_progress(1.0);
        Ok(())
    }

    fn compute_levels(
        &self,
        plan: &Plan,
        inputs: &LoadedInputs,
    ) -> Result<(LevelSet, LevelSet), StageFailure> {
        let centers = self.config.centers_hz();
        let predicted_all = predicted_levels(plan, centers).map_err(|err| {
            StageFailure::failed(
                error::LEVELS,
                format!("predicted levels failed: {}: {}", err.code(), err.message()),
            )
        })?;
        let lane = usize::from(self.config.lane);
        let lane_levels = predicted_all.points.get(lane).ok_or_else(|| {
            StageFailure::failed(
                error::CONFIG,
                format!(
                    "lane {} selected but plan predicts only {} lanes",
                    self.config.lane,
                    predicted_all.points.len()
                ),
            )
        })?;
        let predicted = LevelSet {
            sample_rate_hz: predicted_all.sample_rate_hz,
            points: vec![PointLevels {
                name: self.config.point.clone(),
                bands: lane_levels.bands.clone(),
            }],
        };
        let observed_bands = relative_band_levels(
            &inputs.applied.samples,
            &inputs.baseline.samples,
            inputs.sample_rate_hz,
            centers,
            &BandLevelsConfig {
                window_len: self.config.window_len(),
            },
        )
        .map_err(|err| {
            let code = if err.code() == crate::band_levels::ALIGNMENT {
                error::ALIGNMENT
            } else {
                error::LEVELS
            };
            StageFailure::failed(
                code,
                format!("observed levels failed: {}: {}", err.code(), err.message()),
            )
        })?;
        let observed = LevelSet {
            sample_rate_hz: inputs.sample_rate_hz,
            points: vec![PointLevels {
                name: self.config.point.clone(),
                bands: observed_bands,
            }],
        };
        Ok((predicted, observed))
    }
}

impl CalibrationJobBackend for OfflineJobBackend {
    fn run_stage(&mut self, stage: JobStage, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        match stage {
            JobStage::Validate => self.stage_validate(ctx),
            JobStage::Compile => self.stage_compile(ctx),
            JobStage::Levels => self.stage_levels(ctx),
            JobStage::ClosedLoop => self.stage_closed_loop(ctx),
            JobStage::Bundle => self.stage_bundle(ctx),
            other => Err(StageFailure::failed(
                error::STAGE,
                format!(
                    "stage {} is not part of the offline closed-loop job",
                    other.as_str()
                ),
            )),
        }
    }
}

// ===== 输入装载 / 校验 =====

struct LoadedInputs {
    baseline: Wav,
    applied: Wav,
    baseline_bytes: Vec<u8>,
    applied_bytes: Vec<u8>,
    sample_rate_hz: f64,
}

/// 装载输入并校验 `JobRequest.inputs` 里的 stale 绑定（baseline/applied 两个名字）。
fn load_inputs_checked(
    ctx: &StageCtx<'_>,
    config: &OfflineJobConfig,
) -> Result<LoadedInputs, StageFailure> {
    let inputs = load_inputs(config)?;
    verify_expected_sha(ctx, "baseline", &inputs.baseline_bytes)?;
    verify_expected_sha(ctx, "applied", &inputs.applied_bytes)?;
    Ok(inputs)
}

fn load_inputs(config: &OfflineJobConfig) -> Result<LoadedInputs, StageFailure> {
    let baseline_bytes = read_file(&config.baseline_wav, "baseline_wav")?;
    let applied_bytes = read_file(&config.applied_wav, "applied_wav")?;
    let baseline = parse_wav(&baseline_bytes).map_err(|err| {
        StageFailure::failed(
            error::INPUT,
            format!(
                "baseline WAV {} invalid: {}: {}",
                config.baseline_wav.display(),
                err.code(),
                err.message()
            ),
        )
    })?;
    let applied = parse_wav(&applied_bytes).map_err(|err| {
        StageFailure::failed(
            error::INPUT,
            format!(
                "applied WAV {} invalid: {}: {}",
                config.applied_wav.display(),
                err.code(),
                err.message()
            ),
        )
    })?;
    if (baseline.sample_rate_hz - applied.sample_rate_hz).abs() > 1e-9 {
        return Err(StageFailure::failed(
            error::ALIGNMENT,
            format!(
                "baseline {} Hz and applied {} Hz sample rates differ",
                baseline.sample_rate_hz, applied.sample_rate_hz
            ),
        ));
    }
    let sample_rate_hz = baseline.sample_rate_hz;
    Ok(LoadedInputs {
        baseline,
        applied,
        baseline_bytes,
        applied_bytes,
        sample_rate_hz,
    })
}

fn check_alignment(plan: &Plan, inputs: &LoadedInputs) -> Result<(), StageFailure> {
    if plan.sample_rate != 0 && plan.sample_rate != inputs.sample_rate_hz as u32 {
        return Err(StageFailure::failed(
            error::ALIGNMENT,
            format!(
                "plan sample_rate {} Hz does not match recordings {} Hz",
                plan.sample_rate, inputs.sample_rate_hz
            ),
        ));
    }
    Ok(())
}

fn verify_expected_sha(ctx: &StageCtx<'_>, name: &str, bytes: &[u8]) -> Result<(), StageFailure> {
    let Some(expected) = ctx
        .request()
        .inputs
        .iter()
        .find(|input| input.name == name)
        .map(|input| input.sha256.as_str())
    else {
        return Ok(());
    };
    let actual = sha256_hex(bytes);
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(StageFailure::failed(
            error::STALE,
            format!(
                "input {name} sha256 mismatch: request {expected}, file {actual} (stale input)"
            ),
        ));
    }
    Ok(())
}

fn read_file(path: &Path, label: &str) -> Result<Vec<u8>, StageFailure> {
    fs::read(path).map_err(|err| {
        StageFailure::failed(
            error::INPUT,
            format!("read {label} {}: {err}", path.display()),
        )
    })
}

fn read_text(path: &Path, label: &str) -> Result<String, StageFailure> {
    let bytes = read_file(path, label)?;
    String::from_utf8(bytes).map_err(|err| {
        StageFailure::failed(
            error::INPUT,
            format!("{label} {} is not UTF-8: {err}", path.display()),
        )
    })
}

fn check_cancel(ctx: &StageCtx<'_>) -> Result<(), StageFailure> {
    ctx.cancel_token().check()
}

// ===== 产物读写（resume 不跳步） =====

fn artifact_path(ctx: &StageCtx<'_>, relative: &str) -> PathBuf {
    ctx.artifacts_dir().join(relative)
}

fn read_artifact(ctx: &StageCtx<'_>, relative: &str) -> Result<Vec<u8>, StageFailure> {
    let path = artifact_path(ctx, relative);
    fs::read(&path).map_err(|err| {
        StageFailure::failed(
            error::ARTIFACT,
            format!("previous stage artifact {relative} is missing/read failed: {err}"),
        )
    })
}

fn require_artifact(ctx: &StageCtx<'_>, relative: &str) -> Result<(), StageFailure> {
    let path = artifact_path(ctx, relative);
    if !path.is_file() {
        return Err(StageFailure::failed(
            error::ARTIFACT,
            format!("previous stage artifact {relative} is missing; refusing to skip stages"),
        ));
    }
    Ok(())
}

fn load_plan_artifact(ctx: &StageCtx<'_>) -> Result<Plan, StageFailure> {
    let bytes = read_artifact(ctx, PLAN_FILE)?;
    Plan::decode(&bytes).map_err(|err| {
        StageFailure::failed(
            error::ARTIFACT,
            format!("compiled plan artifact {PLAN_FILE} does not decode: {err}"),
        )
    })
}

fn channels_of_plan(plan: &Plan) -> u16 {
    let lanes = plan
        .protection
        .as_ref()
        .map(|section| section.lanes)
        .unwrap_or(0);
    if lanes == 0 {
        1
    } else {
        lanes
    }
}

fn channels_of_design(design: &PeqDesign) -> u16 {
    design
        .channels
        .iter()
        .map(|channel| channel.index.saturating_add(1))
        .max()
        .unwrap_or(1)
        .min(u32::from(u16::MAX)) as u16
}

// ===== 确定性 JSON =====

fn fmt_sig(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.9e}")
    } else {
        "null".to_string()
    }
}

fn push_key(out: &mut String, key: &str) {
    out.push('"');
    out.push_str(key);
    out.push('"');
    out.push(':');
}

fn push_str(out: &mut String, key: &str, value: &str) {
    push_key(out, key);
    out.push('"');
    out.push_str(&json_escape(value));
    out.push('"');
    out.push(',');
}

fn push_str_last(out: &mut String, key: &str, value: &str) {
    push_key(out, key);
    out.push('"');
    out.push_str(&json_escape(value));
    out.push('"');
}

fn push_raw(out: &mut String, key: &str, value: &str) {
    push_key(out, key);
    out.push_str(value);
    out.push(',');
}

fn push_raw_last(out: &mut String, key: &str, value: &str) {
    push_key(out, key);
    out.push_str(value);
}

fn push_f64(out: &mut String, key: &str, value: f64) {
    push_raw(out, key, &fmt_sig(value));
}

fn push_f64_last(out: &mut String, key: &str, value: f64) {
    push_raw_last(out, key, &fmt_sig(value));
}

fn push_f64_array(out: &mut String, key: &str, values: &[f64]) {
    push_key(out, key);
    out.push('[');
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&fmt_sig(*value));
    }
    out.push_str("],");
}

fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn inputs_json(config: &OfflineJobConfig, inputs: &LoadedInputs) -> Result<String, StageFailure> {
    let mut out = String::with_capacity(1024);
    out.push('{');
    push_str(&mut out, "schema", INPUTS_SCHEMA);
    push_str(&mut out, "kind", "single_point_offline");
    push_str(&mut out, "point", &config.point);
    push_raw(&mut out, "lane", &config.lane.to_string());
    push_f64(&mut out, "sample_rate_hz", inputs.sample_rate_hz);
    push_f64_array(&mut out, "centers_hz", config.centers_hz());
    push_f64(&mut out, "tolerance_db", config.tolerance_db);
    push_raw(
        &mut out,
        "min_compared_bands",
        &config.min_compared_bands.to_string(),
    );
    push_raw(&mut out, "window_len", &config.window_len().to_string());
    push_key(&mut out, "baseline");
    out.push('{');
    push_str(&mut out, "sha256", &sha256_hex(&inputs.baseline_bytes));
    push_raw(&mut out, "bytes", &inputs.baseline_bytes.len().to_string());
    push_raw(&mut out, "channels", &inputs.baseline.channels.to_string());
    push_raw(
        &mut out,
        "samples",
        &inputs.baseline.samples.len().to_string(),
    );
    push_f64_last(&mut out, "sample_rate_hz", inputs.baseline.sample_rate_hz);
    out.push_str("},");
    push_key(&mut out, "applied");
    out.push('{');
    push_str(&mut out, "sha256", &sha256_hex(&inputs.applied_bytes));
    push_raw(&mut out, "bytes", &inputs.applied_bytes.len().to_string());
    push_raw(&mut out, "channels", &inputs.applied.channels.to_string());
    push_raw(
        &mut out,
        "samples",
        &inputs.applied.samples.len().to_string(),
    );
    push_f64_last(&mut out, "sample_rate_hz", inputs.applied.sample_rate_hz);
    out.push_str("},");
    push_key(&mut out, "inputs");
    out.push('{');
    if let Some(path) = &config.plan_bin {
        let bytes = fs::read(path).map_err(|err| {
            StageFailure::failed(
                error::INPUT,
                format!("read plan_bin {}: {err}", path.display()),
            )
        })?;
        push_str(&mut out, "plan_sha256", &sha256_hex(&bytes));
        push_raw_last(&mut out, "plan_bytes", &bytes.len().to_string());
    } else {
        push_str_last(&mut out, "plan_sha256", "null");
    }
    out.push('}');
    out.push('}');
    out.push('\n');
    Ok(out)
}

fn compile_report_json(
    source: &str,
    sample_rate: u32,
    block_size: u32,
    channels: u16,
    plan_sha256: &str,
    graph_id: &str,
    bytes: usize,
) -> String {
    let mut out = String::with_capacity(512);
    out.push('{');
    push_str(&mut out, "schema", COMPILE_REPORT_SCHEMA);
    push_str(&mut out, "source", source);
    push_raw(&mut out, "sample_rate", &sample_rate.to_string());
    push_raw(&mut out, "block_size", &block_size.to_string());
    push_raw(&mut out, "channels", &channels.to_string());
    push_str(&mut out, "plan_sha256", plan_sha256);
    push_str(&mut out, "graph_id", graph_id);
    push_raw(&mut out, "plan_bytes", &bytes.to_string());
    push_str_last(&mut out, "abi", "khaen-plan/v3");
    out.push('}');
    out.push('\n');
    out
}

fn summary_json(
    config: &OfflineJobConfig,
    report: &crate::closed_loop::ClosedLoopReport,
) -> String {
    let mut out = String::with_capacity(2048);
    out.push('{');
    push_str(&mut out, "schema", SUMMARY_SCHEMA);
    push_str(&mut out, "verdict", report.verdict.as_str());
    push_str(&mut out, "point", &config.point);
    push_raw(&mut out, "lane", &config.lane.to_string());
    push_f64(&mut out, "tolerance_db", report.tolerance_db);
    push_raw(
        &mut out,
        "min_compared_bands",
        &report.min_compared_bands.to_string(),
    );
    push_raw(
        &mut out,
        "compared_bands",
        &report.compared_bands.to_string(),
    );
    push_raw(&mut out, "missing_bands", &report.missing_bands.to_string());
    push_raw(&mut out, "violations", &report.violations.to_string());
    match report.max_abs_delta_db {
        Some(value) => push_f64(&mut out, "max_abs_delta_db", value),
        None => push_raw(&mut out, "max_abs_delta_db", "null"),
    }
    push_key(&mut out, "bands");
    out.push('[');
    let mut first = true;
    for point in &report.points {
        for band in &point.bands {
            if !first {
                out.push(',');
            }
            first = false;
            out.push('{');
            push_f64(&mut out, "center_hz", band.center_hz);
            match band.predicted_db {
                Some(value) => push_f64(&mut out, "predicted_db", value),
                None => push_raw(&mut out, "predicted_db", "null"),
            }
            match band.observed_db {
                Some(value) => push_f64(&mut out, "observed_db", value),
                None => push_raw(&mut out, "observed_db", "null"),
            }
            match band.delta_db {
                Some(value) => push_f64(&mut out, "delta_db", value),
                None => push_raw(&mut out, "delta_db", "null"),
            }
            match band.within_tolerance {
                Some(value) => push_raw_last(&mut out, "within_tolerance", &value.to_string()),
                None => push_raw_last(&mut out, "within_tolerance", "null"),
            }
            out.push('}');
        }
    }
    out.push_str("],");
    push_key(&mut out, "notes");
    out.push('[');
    for (index, note) in report.notes.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(&json_escape(note));
        out.push('"');
    }
    out.push_str("]}");
    out.push('\n');
    out
}

// ===== 设计 JSON 解析（modal_peq/v1 或单点 params.json） =====

/// 解析设计 JSON：`khaen-modal-peq/v1`（root.channels[].bands[].{b,a}）或
/// `khaen-single-point-bundle` 的 `params.json`（root.design.channels[].bands[].{b,a}）。
fn parse_design_json(text: &str, sample_rate_hz: u32) -> Result<PeqDesign, StageFailure> {
    let root = JsonParser::new(text).parse().map_err(|message| {
        StageFailure::failed(
            error::DESIGN,
            format!("design JSON parse failed: {message}"),
        )
    })?;
    let object = root
        .as_object()
        .ok_or_else(|| StageFailure::failed(error::DESIGN, "design JSON root must be an object"))?;
    let (channels_value, fs) = if let Some(channels) = object_get(object, "channels") {
        let fs = object_get(object, "fs")
            .and_then(Json::as_f64)
            .unwrap_or(f64::from(sample_rate_hz));
        (channels, fs)
    } else if let Some(design) = object_get(object, "design") {
        let design = design.as_object().ok_or_else(|| {
            StageFailure::failed(error::DESIGN, "params.json design must be an object")
        })?;
        let fs = object_get(design, "fs")
            .and_then(Json::as_f64)
            .unwrap_or(f64::from(sample_rate_hz));
        let channels = object_get(design, "channels").ok_or_else(|| {
            StageFailure::failed(error::DESIGN, "params.json design.channels is missing")
        })?;
        (channels, fs)
    } else {
        return Err(StageFailure::failed(
            error::DESIGN,
            "design JSON has neither channels nor design.channels",
        ));
    };
    let fs = fs.round();
    if !fs.is_finite() || fs <= 0.0 {
        return Err(StageFailure::failed(
            error::DESIGN,
            format!("design fs {fs} is invalid"),
        ));
    }
    let mut design = PeqDesign::new(fs as u32, DesignMode::ModalOnly).map_err(|err| {
        StageFailure::failed(
            error::DESIGN,
            format!("design fs {fs} rejected: {}", err.code),
        )
    })?;
    let channels = channels_value
        .as_array()
        .ok_or_else(|| StageFailure::failed(error::DESIGN, "design channels must be an array"))?;
    if channels.is_empty() {
        return Err(StageFailure::failed(
            error::DESIGN,
            "design has no channels",
        ));
    }
    for (position, channel) in channels.iter().enumerate() {
        let channel = channel.as_object().ok_or_else(|| {
            StageFailure::failed(
                error::DESIGN,
                format!("channel[{position}] must be an object"),
            )
        })?;
        let index = object_get(channel, "index")
            .and_then(Json::as_f64)
            .map(|value| value as u32)
            .unwrap_or(position as u32);
        let name = object_get(channel, "name")
            .and_then(Json::as_str)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string())
            .unwrap_or_else(|| format!("CH{index}"));
        let mut parsed = ChannelPeq::new(name, index).map_err(|err| {
            StageFailure::failed(
                error::DESIGN,
                format!("channel[{position}] rejected: {}", err.code),
            )
        })?;
        let bands = object_get(channel, "bands")
            .and_then(Json::as_array)
            .ok_or_else(|| {
                StageFailure::failed(
                    error::DESIGN,
                    format!("channel[{position}].bands must be an array"),
                )
            })?;
        for (band_position, band) in bands.iter().enumerate() {
            let band = band.as_object().ok_or_else(|| {
                StageFailure::failed(
                    error::DESIGN,
                    format!("channel[{position}].bands[{band_position}] must be an object"),
                )
            })?;
            let b = json_fixed3(band, "b", position, band_position)?;
            let a = json_fixed3(band, "a", position, band_position)?;
            let band_type = object_get(band, "type")
                .and_then(Json::as_str)
                .and_then(|value| BandType::parse(value).ok())
                .unwrap_or(BandType::Peaking);
            parsed.push_band(ModalBand {
                band_type,
                freq_hz: object_get(band, "freq")
                    .and_then(Json::as_f64)
                    .unwrap_or(0.0),
                q: object_get(band, "q").and_then(Json::as_f64).unwrap_or(1.0),
                gain_db: object_get(band, "gain_db")
                    .and_then(Json::as_f64)
                    .unwrap_or(0.0),
                b,
                a,
            });
        }
        design.add_channel(parsed).map_err(|err| {
            StageFailure::failed(
                error::DESIGN,
                format!("channel[{position}] add failed: {}", err.code),
            )
        })?;
    }
    Ok(design)
}

fn json_fixed3(
    band: &[(String, Json)],
    key: &str,
    channel: usize,
    index: usize,
) -> Result<[f64; 3], StageFailure> {
    let array = object_get(band, key)
        .and_then(Json::as_array)
        .ok_or_else(|| {
            StageFailure::failed(
                error::DESIGN,
                format!("channel[{channel}].bands[{index}].{key} must be an array"),
            )
        })?;
    let mut out = [0.0f64; 3];
    for slot in 0..3 {
        out[slot] = array.get(slot).and_then(Json::as_f64).ok_or_else(|| {
            StageFailure::failed(
                error::DESIGN,
                format!("channel[{channel}].bands[{index}].{key}[{slot}] must be a number"),
            )
        })?;
        if !out[slot].is_finite() {
            return Err(StageFailure::failed(
                error::DESIGN,
                format!("channel[{channel}].bands[{index}].{key}[{slot}] is not finite"),
            ));
        }
    }
    Ok(out)
}

fn object_get<'a>(object: &'a [(String, Json)], key: &str) -> Option<&'a Json> {
    object
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
}

// ===== 最小 JSON 解析器（无第三方依赖；只服务设计 JSON 输入） =====

#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Number(value) => Some(*value),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(value) => Some(value.as_str()),
            _ => None,
        }
    }

    fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(values) => Some(values.as_slice()),
            _ => None,
        }
    }

    fn as_object(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Object(entries) => Some(entries.as_slice()),
            _ => None,
        }
    }
}

struct JsonParser<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> JsonParser<'a> {
    fn new(text: &'a str) -> Self {
        JsonParser {
            bytes: text.as_bytes(),
            position: 0,
        }
    }

    fn parse(mut self) -> Result<Json, String> {
        let value = self.parse_value()?;
        self.skip_whitespace();
        if self.position != self.bytes.len() {
            return Err(format!("trailing bytes at {}", self.position));
        }
        Ok(value)
    }

    fn skip_whitespace(&mut self) {
        while let Some(byte) = self.bytes.get(self.position) {
            if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
                self.position += 1;
            } else {
                break;
            }
        }
    }

    fn parse_value(&mut self) -> Result<Json, String> {
        self.skip_whitespace();
        let Some(byte) = self.bytes.get(self.position).copied() else {
            return Err("unexpected end of JSON".to_string());
        };
        match byte {
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b'"' => self.parse_string().map(Json::String),
            b't' => self.parse_literal("true").map(|_| Json::Bool(true)),
            b'f' => self.parse_literal("false").map(|_| Json::Bool(false)),
            b'n' => self.parse_literal("null").map(|_| Json::Null),
            b'-' | b'0'..=b'9' => self.parse_number(),
            other => Err(format!(
                "unexpected byte {:?} at {}",
                other as char, self.position
            )),
        }
    }

    fn parse_literal(&mut self, literal: &str) -> Result<(), String> {
        if self
            .bytes
            .get(self.position..self.position + literal.len())
            .map(|slice| slice == literal.as_bytes())
            .unwrap_or(false)
        {
            self.position += literal.len();
            Ok(())
        } else {
            Err(format!("bad literal at {}", self.position))
        }
    }

    fn parse_object(&mut self) -> Result<Json, String> {
        self.position += 1; // '{'
        let mut entries = Vec::new();
        self.skip_whitespace();
        if self.bytes.get(self.position) == Some(&b'}') {
            self.position += 1;
            return Ok(Json::Object(entries));
        }
        loop {
            self.skip_whitespace();
            let key = self.parse_string()?;
            self.skip_whitespace();
            if self.bytes.get(self.position) != Some(&b':') {
                return Err(format!("expected ':' at {}", self.position));
            }
            self.position += 1;
            let value = self.parse_value()?;
            entries.push((key, value));
            self.skip_whitespace();
            match self.bytes.get(self.position) {
                Some(b',') => {
                    self.position += 1;
                }
                Some(b'}') => {
                    self.position += 1;
                    return Ok(Json::Object(entries));
                }
                _ => return Err(format!("expected ',' or '}}' at {}", self.position)),
            }
        }
    }

    fn parse_array(&mut self) -> Result<Json, String> {
        self.position += 1; // '['
        let mut values = Vec::new();
        self.skip_whitespace();
        if self.bytes.get(self.position) == Some(&b']') {
            self.position += 1;
            return Ok(Json::Array(values));
        }
        loop {
            values.push(self.parse_value()?);
            self.skip_whitespace();
            match self.bytes.get(self.position) {
                Some(b',') => {
                    self.position += 1;
                }
                Some(b']') => {
                    self.position += 1;
                    return Ok(Json::Array(values));
                }
                _ => return Err(format!("expected ',' or ']' at {}", self.position)),
            }
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        if self.bytes.get(self.position) != Some(&b'"') {
            return Err(format!("expected string at {}", self.position));
        }
        self.position += 1;
        let mut out = String::new();
        loop {
            let Some(byte) = self.bytes.get(self.position).copied() else {
                return Err("unterminated string".to_string());
            };
            self.position += 1;
            match byte {
                b'"' => return Ok(out),
                b'\\' => {
                    let Some(escape) = self.bytes.get(self.position).copied() else {
                        return Err("unterminated escape".to_string());
                    };
                    self.position += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let code = self.parse_unicode_escape()?;
                            out.push(code);
                        }
                        other => return Err(format!("bad escape {:?}", other as char)),
                    }
                }
                other => out.push(other as char),
            }
        }
    }

    fn parse_unicode_escape(&mut self) -> Result<char, String> {
        let digits = self
            .bytes
            .get(self.position..self.position + 4)
            .ok_or_else(|| "truncated unicode escape".to_string())?;
        let text = std::str::from_utf8(digits).map_err(|_| "bad unicode escape".to_string())?;
        let value = u32::from_str_radix(text, 16).map_err(|_| "bad unicode escape".to_string())?;
        self.position += 4;
        char::from_u32(value).ok_or_else(|| "invalid unicode scalar".to_string())
    }

    fn parse_number(&mut self) -> Result<Json, String> {
        let start = self.position;
        if self.bytes.get(self.position) == Some(&b'-') {
            self.position += 1;
        }
        while let Some(byte) = self.bytes.get(self.position) {
            if byte.is_ascii_digit() || matches!(byte, b'.' | b'e' | b'E' | b'+' | b'-') {
                self.position += 1;
            } else {
                break;
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.position])
            .map_err(|_| "bad number bytes".to_string())?;
        text.parse::<f64>()
            .map(Json::Number)
            .map_err(|_| format!("bad number {text:?}"))
    }
}

// ---------------------------------------------------------------------------
// 测试注册（leaf 私有测试自注册；integrator 不要再写 public_registration.rs）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod t70h_tests {
    include!("../tests/T70h/offline_job.rs");
}
