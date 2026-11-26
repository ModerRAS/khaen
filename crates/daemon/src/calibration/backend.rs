//! 校准 job 生产后端（T70h-b 接线）：daemon 的 CalibrationService 由
//! khaen_calibration::job::JobManager + khaen_calibration::offline_job::OfflineJobBackend 实现。
//!
//! # 结构
//!
//! - 执行：JobManager（crates/calibration，T35/T60）持有唯一 active job、stage 状态机、
//!   checkpoint、artifact ledger 与 sha256 manifest；离线 job 真正的分析在
//!   OfflineJobBackend（T70h-b，已随 calibration crate 交付）。
//! - daemon 侧索引：JobManager 只保留 active + last_finished；REST 需要完整 job 历史，
//!   因此本模块在 <state>/calibration/jobs-index.json 持久化一份审计索引（原子写）。
//!   索引只是缓存：每次访问先 sync() 从 JobManager 快照刷新 active/last job，索引损坏时
//!   初始化失败并如实报 not_wired 原因，不静默丢失历史。
//! - artifacts：REST 白名单是 bare filename（report.json / plan.bin / predicted.json /
//!   observed.json / artifacts.manifest.json），本模块把 offline job 的嵌套相对路径
//!   （closed-loop/report.json 等）映射到白名单名字；下载时重算 sha256 与 manifest 比对。
//! - apply/rollback：本层只负责「job 产物 -> 控制面 plan 引用 / 回滚目标版本」；
//!   prepare->commit->abort 的跨进程控制由 REST 层（api.rs）按 host_link.target 路由。
//!
//! # 诚实性
//!
//! - 不假成功：真实执行失败进 job error（稳定码来自 backend），取消进 cancelled；
//! - 不编数值：拿不到的字段一律 null + reason；measured 只来自上传录音的内容寻址 id，
//!   simulated 明确标注是方案预测电平；
//! - 不开设备：离线 job 的 stage 计划不含任何 needs_calibration_device 的阶段，Safety 实现
//!   任何设备动作都 fail-closed（真被调用即报错，不静默装作成功）。
//!
//! # NOT_RUN
//!
//! 真机 UMIK/MOTU 采集（T70f-c）、rt-host 目标下的真实 apply（会重开 PCM/出声，需设备窗口）。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use khaen_calibration::job::{
    sha256_hex, ExitDisposition, InputHash, JobError as CalJobError, JobKind as CalJobKind,
    JobManager, JobManagerConfig, JobRequest, JobSnapshot, JobState as CalJobState,
    SafeOutputController, SafetyError, ARTIFACT_MANIFEST_FILE_NAME,
};
use khaen_calibration::offline_job::{
    OfflineJobBackend, OfflineJobConfig, CLOSED_LOOP_REPORT_FILE, COMPILE_REPORT_FILE,
    OBSERVED_FILE, PLAN_FILE, PREDICTED_FILE,
};
use khaen_core::plan::Plan;

use super::service::{
    AppliedVersion, ApplyPlan, ApplyState, ArtifactInfo, BandResult, CalibrationError,
    CalibrationService, CalibrationStatus, JobApply, JobError, JobInputs, JobMeasured, JobPage,
    JobQuery, JobRecord, JobResult, JobSimulated, JobSink, JobState, JobStep, JobSummary, NewJob,
    PlanResult, RollbackTarget, StepState, OFFLINE_JOB_STEPS,
};
use super::store::{self, UploadStore};

/// 状态目录下的后端子目录（jobs 产物 / checkpoint / 索引）。
const BACKEND_DIR: &str = "calibration";
/// 每 job artifacts 目录的父目录名（<state>/calibration/jobs/job-NNNN）。
const ARTIFACTS_DIR: &str = "jobs";
/// JobManager checkpoint 文件名。
const CHECKPOINT_FILE: &str = "job.checkpoint";
/// daemon 侧 job 审计索引文件名。
const INDEX_FILE: &str = "jobs-index.json";
/// 审计索引 schema（字段只增不改；破坏性变更升 v2）。
const INDEX_SCHEMA: &str = "khaen-daemon-calibration-index/v1";
/// job 类型机器名（与 REST 合同一致）。
const JOB_KIND: &str = "single_point_offline";
/// /api/status 的 calibration.engine 文案（如实说明是离线 JobManager，不是采集引擎）。
const ENGINE_NAME: &str = "offline single-point calibration (khaen-calibration JobManager)";
/// job id 前缀（与 JobId::to_string 的 job-NNNN 一致）。
const JOB_PREFIX: &str = "job-";
/// 取消后等待终态的时长上限（REST cancel 是同步接口；超时则如实返回当前状态）。
const CANCEL_WAIT: Duration = Duration::from_secs(3);
/// 离线 job 的固定测点名（单点/皇帝位；多点属 NOT_AVAILABLE）。
const DEFAULT_POINT: &str = "P01";
/// 离线 job 的固定 lane（预测侧取 lane 0；lane 语义属方案层）。
const DEFAULT_LANE: u16 = 0;

/// 白名单产物：REST 名字 -> offline job 相对路径。
///
/// 只暴露 daemon store::ARTIFACT_WHITELIST 内的名字；其余中间产物通过
/// artifacts.manifest.json 审计（下载清单即含 sha256/bytes）。
const EXPOSED_ARTIFACTS: [(&str, &str); 4] = [
    ("plan.bin", PLAN_FILE),
    ("predicted.json", PREDICTED_FILE),
    ("observed.json", OBSERVED_FILE),
    ("report.json", CLOSED_LOOP_REPORT_FILE),
];

// ---------------------------------------------------------------------------
// 持久化模型
// ---------------------------------------------------------------------------

/// 审计索引文件（原子写；job 按 job_id 升序存储）。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedIndex {
    schema: String,
    jobs: Vec<PersistedJob>,
}

/// 一个 job 的审计快照（daemon 侧；不含 artifacts 文件内容，只含 manifest 摘要）。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedJob {
    job_id: String,
    kind: String,
    /// daemon 状态机器名（queued/running/succeeded/failed/cancelled；canonical 双 l）。
    state: String,
    active_step: Option<String>,
    verdict: Option<String>,
    created_unix: u64,
    updated_unix: u64,
    started_unix: Option<u64>,
    finished_unix: Option<u64>,
    stage_index: u32,
    stage_count: u32,
    progress: f64,
    /// 创建时的输入；索引损坏/从 checkpoint 重建时可能为 None（REST 如实 null + reason）。
    inputs: Option<JobInputs>,
    artifacts: Vec<PersistedArtifact>,
    error: Option<JobError>,
    /// 失败/取消发生在哪个步骤（index）。
    ///
    /// JobManager 在**任何终态**都把 snapshot.stage_index 置为 stage_count（见 job.rs
    /// finish_complete/finish_non_complete），所以不能拿终态 stage_index 推导步骤状态；
    /// 这里单独记录失败阶段（failure.stage 精确值，缺失时用最后一个观察到的运行中 index 兜底）。
    #[serde(default)]
    failure_stage_index: Option<u32>,
    plan: Option<PlanResult>,
    result: Option<JobResult>,
    apply: JobApply,
}

/// 一个可下载产物的审计记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedArtifact {
    /// REST 白名单名字（bare filename）。
    name: String,
    /// job artifacts 目录内的相对路径。
    relative_path: String,
    sha256: String,
    bytes: u64,
}

impl PersistedJob {
    /// 新 job（queued；时间来自 daemon 时钟，stage 刷新后以 JobManager 快照为准）。
    fn queued(job_id: String, inputs: JobInputs, created_unix: u64) -> Self {
        Self {
            job_id,
            kind: JOB_KIND.to_string(),
            state: JobState::Queued.as_str().to_string(),
            active_step: None,
            verdict: None,
            created_unix,
            updated_unix: created_unix,
            started_unix: None,
            finished_unix: None,
            stage_index: 0,
            stage_count: OFFLINE_JOB_STEPS.len() as u32,
            progress: 0.0,
            inputs: Some(inputs),
            artifacts: Vec::new(),
            error: None,
            failure_stage_index: None,
            plan: None,
            result: None,
            apply: JobApply::default(),
        }
    }

    /// 从 JobManager 快照重建最小记录（索引缺失/损坏时用；inputs 无法还原 -> None）。
    fn from_snapshot(snapshot: &JobSnapshot) -> Self {
        Self {
            job_id: snapshot.job_id.to_string(),
            kind: snapshot.request.kind.as_str().to_string(),
            state: map_state(snapshot.state).as_str().to_string(),
            active_step: snapshot.stage.map(|stage| stage.as_str().to_string()),
            verdict: None,
            created_unix: snapshot.created_unix,
            updated_unix: snapshot.updated_unix,
            started_unix: snapshot.started_unix,
            finished_unix: snapshot.finished_unix,
            stage_index: snapshot.stage_index,
            stage_count: snapshot.stage_count,
            progress: f64::from(snapshot.overall_progress),
            inputs: None,
            artifacts: Vec::new(),
            error: snapshot.failure.as_ref().map(|failure| JobError {
                code: failure.code.clone(),
                message: failure.message.clone(),
            }),
            failure_stage_index: None,
            plan: None,
            result: None,
            apply: JobApply::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// 服务
// ---------------------------------------------------------------------------

/// 生产校准服务：JobManager + 离线 backend + daemon 审计索引。
pub struct JobManagerCalibration {
    manager: JobManager,
    uploads: UploadStore,
    artifacts_root: PathBuf,
    index_path: PathBuf,
    jobs: Mutex<BTreeMap<u64, PersistedJob>>,
    sink: Mutex<Option<Arc<dyn JobSink>>>,
}

impl std::fmt::Debug for JobManagerCalibration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let jobs = self.lock();
        formatter
            .debug_struct("JobManagerCalibration")
            .field("artifacts_root", &self.artifacts_root)
            .field("index_path", &self.index_path)
            .field("jobs", &jobs.len())
            .finish()
    }
}

impl JobManagerCalibration {
    /// 在 daemon 状态目录下创建生产后端。
    ///
    /// 失败（目录不可写/索引损坏）由调用方如实降级为 not_wired + 真实原因，不静默吞掉。
    pub fn new(state_dir: &Path) -> Result<Self, String> {
        let base = state_dir.join(BACKEND_DIR);
        fs::create_dir_all(&base)
            .map_err(|error| format!("create calibration state dir {}: {error}", base.display()))?;
        let artifacts_root = base.join(ARTIFACTS_DIR);
        let index_path = base.join(INDEX_FILE);
        let checkpoint_path = base.join(CHECKPOINT_FILE);
        let config = JobManagerConfig {
            artifacts_root: artifacts_root.clone(),
            checkpoint_path: Some(checkpoint_path),
            apply_confirm_timeout: None,
        };
        let manager = JobManager::new(config, Arc::new(OfflineJobSafety))
            .map_err(|error| format!("create calibration job manager: {error}"))?;
        let jobs = load_index(&index_path)?;
        let service = Self {
            manager,
            uploads: UploadStore::new(state_dir),
            artifacts_root,
            index_path,
            jobs: Mutex::new(jobs),
            sink: Mutex::new(None),
        };
        service.recover_pending()?;
        Ok(service)
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<u64, PersistedJob>> {
        self.jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn sink(&self) -> Option<Arc<dyn JobSink>> {
        self.sink
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 推一个 job 事件给 daemon（Telemetry 用；sink 未注入时静默跳过）。
    fn emit(&self, job_id: &str, state: JobState, verdict: Option<String>) {
        if let Some(sink) = self.sink() {
            sink.job_event(super::service::JobEvent {
                job_id: job_id.to_string(),
                state,
                verdict,
                at: Some(now_rfc3339()),
            });
        }
    }

    /// 原子写审计索引。
    fn persist(&self) -> Result<(), String> {
        let bytes = {
            let jobs = self.lock();
            let index = PersistedIndex {
                schema: INDEX_SCHEMA.to_string(),
                jobs: jobs.values().cloned().collect(),
            };
            serde_json::to_vec_pretty(&index)
                .map_err(|error| format!("serialize calibration job index: {error}"))?
        };
        store::write_atomic(&self.index_path, &bytes, 0o600).map_err(|error| {
            format!(
                "write calibration job index {}: {error}",
                self.index_path.display()
            )
        })
    }

    /// 从 JobManager 快照刷新索引。返回是否有 job 数据变化（用于事件推送）。
    fn sync(&self) -> bool {
        let Some(snapshot) = self.manager.snapshot() else {
            return false;
        };
        let id = snapshot.job_id.0;
        let (mut changed, transition) = {
            let mut jobs = self.lock();
            let entry = jobs
                .entry(id)
                .or_insert_with(|| PersistedJob::from_snapshot(&snapshot));
            let old_state = entry.state.clone();
            let changed = apply_snapshot(&self.artifacts_root, entry, &snapshot);
            let transition = if entry.state != old_state {
                JobState::parse(&entry.state)
            } else {
                None
            };
            (changed, transition)
        };
        // 竞态：JobManager 先把状态置 Complete、再落 artifacts.manifest.json；首个读到的
        // 终态快照可能还没有 manifest。这里短暂等待（最多 ~100ms）并补登一次，保证
        // REST 第一次看到终态时审计产物完整（不伪造：等不到就是没有）。
        if snapshot.state.is_terminal() && !self.manifest_present(id) {
            for _ in 0..50 {
                std::thread::sleep(Duration::from_millis(2));
                if self.manifest_present(id) {
                    break;
                }
            }
            if self.manifest_present(id) {
                let mut jobs = self.lock();
                if let Some(entry) = jobs.get_mut(&id) {
                    if collect_artifacts(&self.artifacts_root, entry, &snapshot) {
                        changed = true;
                    }
                }
            }
        }
        if changed {
            if let Err(error) = self.persist() {
                eprintln!("khaen-daemon: calibration job index: {error}");
            }
        }
        if let Some(state) = transition {
            self.emit(&snapshot.job_id.to_string(), state, None);
        }
        changed
    }

    /// 启动时恢复 pending checkpoint（daemon 重启中断的 job）。
    ///
    /// 能恢复就恢复（输入都落盘：上传内容寻址 + 方案 ABI）；不能恢复就 abandon 成 failed
    /// 并带真实原因——绝不假装 job 还在跑。
    fn recover_pending(&self) -> Result<(), String> {
        let Some(pending) = self.manager.pending_resume() else {
            return Ok(());
        };
        let id = pending.job_id.0;
        let inputs = self.lock().get(&id).and_then(|job| job.inputs.clone());
        let resume = match inputs {
            Some(inputs) => match self.build_offline_config(&inputs) {
                Ok(config) => self
                    .manager
                    .resume(Box::new(OfflineJobBackend::new(config)))
                    .map_err(|error| format!("resume: {error}")),
                Err(error) => Err(format!(
                    "cannot rebuild offline job inputs: {}",
                    error.message()
                )),
            },
            None => Err(
                "the persisted job index has no inputs for this job (index missing or corrupt)"
                    .to_string(),
            ),
        };
        match resume {
            Ok(job_id) => {
                self.sync();
                eprintln!(
                    "khaen-daemon: calibration job {} resumed from checkpoint after restart",
                    job_id
                );
                Ok(())
            }
            Err(reason) => {
                let abandon_reason = format!(
                    "daemon restarted while calibration job {} was running and it cannot be resumed: {reason}",
                    pending.job_id
                );
                match self.manager.abandon(&abandon_reason) {
                    Ok(snapshot) => {
                        self.sync();
                        let _ = self.persist();
                        eprintln!(
                            "khaen-daemon: calibration job {} abandoned after restart: {abandon_reason}",
                            snapshot.job_id
                        );
                        Ok(())
                    }
                    Err(error) => Err(format!(
                        "calibration job {} is stuck in a pending checkpoint (resume failed: {reason}; abandon failed: {error})",
                        pending.job_id
                    )),
                }
            }
        }
    }

    /// 上传 sha -> 真实上传记录（baseline/applied/plan 三个上传必须都存在）。
    fn job_upload(
        &self,
        upload_id: &str,
        field: &str,
    ) -> Result<store::UploadRecord, CalibrationError> {
        self.uploads.lookup(upload_id)?.ok_or_else(|| {
            CalibrationError::upload_not_found(upload_id)
                .with_detail("field", serde_json::json!(field))
        })
    }

    /// 构造离线 backend 配置（路径只用于读输入，不写进确定性证据）。
    fn build_offline_config(
        &self,
        inputs: &JobInputs,
    ) -> Result<OfflineJobConfig, CalibrationError> {
        let plan = self.job_upload(&inputs.plan_sha, "inputs.plan_sha")?;
        let baseline = self.job_upload(&inputs.baseline_upload_id, "inputs.baseline_upload_id")?;
        let applied = self.job_upload(&inputs.applied_upload_id, "inputs.applied_upload_id")?;
        let mut config = OfflineJobConfig::new(baseline.path, applied.path);
        config.plan_bin = Some(plan.path);
        config.design_json = None;
        config.point = DEFAULT_POINT.to_string();
        config.lane = DEFAULT_LANE;
        config.centers_hz = inputs.centers_hz.clone();
        config.tolerance_db = inputs.tolerance_db;
        config.min_compared_bands = inputs.min_bands as usize;
        config.window_len = 0;
        Ok(config)
    }

    /// 构造 JobRequest（离线 job 不出声：audible_sweep_authorized=false 是诚实标注）。
    fn build_request(&self, inputs: &JobInputs, sample_rate: u32) -> JobRequest {
        JobRequest {
            kind: CalJobKind::SinglePointOffline,
            label: format!(
                "offline single-point job for plan {}",
                short_sha(&inputs.plan_sha)
            ),
            layout: "single_point".to_string(),
            sample_rate,
            design_points: 1,
            holdout_points: 1,
            repeats: 1,
            drive_level_dbfs: -30.0,
            audible_sweep_authorized: false,
            auto_apply: false,
            expected_applied_version: None,
            inputs: vec![
                InputHash {
                    name: "plan".to_string(),
                    sha256: inputs.plan_sha.clone(),
                },
                InputHash {
                    name: "baseline".to_string(),
                    sha256: inputs.baseline_upload_id.clone(),
                },
                InputHash {
                    name: "applied".to_string(),
                    sha256: inputs.applied_upload_id.clone(),
                },
            ],
        }
    }

    /// 读方案上传并解码（start 阶段先验证「上传的确实是 ABI v3 方案」）。
    fn decode_uploaded_plan(
        &self,
        plan_sha: &str,
    ) -> Result<(store::UploadRecord, Plan), CalibrationError> {
        let record = self.job_upload(plan_sha, "inputs.plan_sha")?;
        let bytes = fs::read(&record.path).map_err(|error| {
            CalibrationError::internal(format!(
                "read uploaded plan {}: {error}",
                record.path.display()
            ))
        })?;
        let plan = Plan::decode(&bytes).map_err(|error| {
            CalibrationError::invalid_request(
                "inputs.plan_sha",
                format!("uploaded plan is not a valid khaen plan ABI v3 file: {error}"),
            )
        })?;
        Ok((record, plan))
    }

    fn job_dir(&self, id: u64) -> PathBuf {
        job_dir(&self.artifacts_root, id)
    }

    /// artifacts.manifest.json 是否已落盘（终态审计完整性用）。
    fn manifest_present(&self, id: u64) -> bool {
        self.job_dir(id).join(ARTIFACT_MANIFEST_FILE_NAME).is_file()
    }

    /// 解析 REST 的 job id（job-NNNN），非法即 404（不泄露内部格式错误细节）。
    fn parse_job_id(job_id: &str) -> Result<u64, CalibrationError> {
        job_id
            .strip_prefix(JOB_PREFIX)
            .and_then(|rest| rest.parse::<u64>().ok())
            .ok_or_else(|| CalibrationError::job_not_found(job_id))
    }

    /// 当前索引里的 job（不触发 sync；调用方负责先 sync）。
    fn require_job(&self, job_id: &str) -> Result<PersistedJob, CalibrationError> {
        let id = Self::parse_job_id(job_id)?;
        self.lock()
            .get(&id)
            .cloned()
            .ok_or_else(|| CalibrationError::job_not_found(job_id))
    }

    fn record(&self, job: &PersistedJob) -> JobRecord {
        let state = JobState::parse(&job.state).unwrap_or(JobState::Failed);
        let mut reasons = BTreeMap::new();
        if job.result.is_some() {
            reasons.insert(
                "result.design".to_string(),
                "offline single-point jobs do not run host admission in the job backend: the plan is decoded/re-encoded and compared with the measured recordings, so design.residual/admission are null instead of invented".to_string(),
            );
        }
        if job.inputs.is_none() {
            reasons.insert(
                "inputs".to_string(),
                "the daemon job index did not have the inputs for this job (recovered from a JobManager checkpoint or a corrupt index); replay the job request to recover them".to_string(),
            );
        }
        JobRecord {
            job_id: job.job_id.clone(),
            kind: job.kind.clone(),
            state,
            active_step: job.active_step.clone(),
            verdict: job.verdict.clone(),
            created_at: Some(rfc3339(job.created_unix)),
            created_unix: Some(job.created_unix),
            started_at: job.started_unix.map(rfc3339),
            finished_at: job.finished_unix.map(rfc3339),
            updated_unix: Some(job.updated_unix),
            duration_ms: duration_ms(job.started_unix, job.finished_unix),
            progress: Some(job.progress),
            engine: None,
            steps: derive_steps(job),
            artifacts: job
                .artifacts
                .iter()
                .map(|artifact| ArtifactInfo {
                    name: artifact.name.clone(),
                    sha256: artifact.sha256.clone(),
                    bytes: artifact.bytes,
                    content_type: None,
                })
                .collect(),
            inputs: job.inputs.clone(),
            result: job.result.clone(),
            error: job.error.clone(),
            apply: job.apply.clone(),
            reasons,
        }
    }

    fn summary(&self, job: &PersistedJob) -> JobSummary {
        let state = JobState::parse(&job.state).unwrap_or(JobState::Failed);
        JobSummary {
            job_id: job.job_id.clone(),
            kind: job.kind.clone(),
            state,
            verdict: job.verdict.clone(),
            created_at: Some(rfc3339(job.created_unix)),
            created_unix: Some(job.created_unix),
            started_at: job.started_unix.map(rfc3339),
            finished_at: job.finished_unix.map(rfc3339),
            updated_unix: Some(job.updated_unix),
            progress: Some(job.progress),
            duration_ms: duration_ms(job.started_unix, job.finished_unix),
            error: job.error.clone(),
            reason: Some(format!(
                "state is reported by the khaen-calibration JobManager ({})",
                job.state
            )),
        }
    }

    /// 读一个白名单产物（重算 sha256 与索引/manifest 比对，防篡改）。
    fn read_exposed_artifact(
        &self,
        job: &PersistedJob,
        name: &str,
    ) -> Result<Vec<u8>, CalibrationError> {
        let artifact = job
            .artifacts
            .iter()
            .find(|artifact| artifact.name == name)
            .ok_or_else(|| CalibrationError::artifact_not_found(&job.job_id, name))?;
        let id = Self::parse_job_id(&job.job_id)?;
        let path = self.job_dir(id).join(&artifact.relative_path);
        let bytes = fs::read(&path).map_err(|error| {
            CalibrationError::artifact_not_found(&job.job_id, name).with_detail(
                "reason",
                serde_json::json!(format!(
                    "artifact file {} is unreadable: {error}",
                    path.display()
                )),
            )
        })?;
        let actual = sha256_hex(&bytes);
        if !actual.eq_ignore_ascii_case(artifact.sha256.trim()) {
            return Err(CalibrationError::artifact_sha_mismatch(
                &job.job_id,
                name,
                artifact.sha256.trim(),
                &actual,
            ));
        }
        Ok(bytes)
    }
}

// ---------------------------------------------------------------------------
// CalibrationService 实现
// ---------------------------------------------------------------------------

impl CalibrationService for JobManagerCalibration {
    fn status(&self) -> Result<CalibrationStatus, CalibrationError> {
        self.sync();
        let jobs = self.lock();
        let active = jobs
            .values()
            .rev()
            .find(|job| {
                JobState::parse(&job.state)
                    .map(|state| !state.is_terminal())
                    .unwrap_or(true)
            })
            .map(|job| job.job_id.clone());
        let last = jobs.values().next_back();
        Ok(CalibrationStatus {
            wired: true,
            engine: Some(ENGINE_NAME.to_string()),
            active_job: active,
            last_job_id: last.map(|job| job.job_id.clone()),
            last_verdict: last.and_then(|job| job.verdict.clone()),
            reason: None,
        })
    }

    fn start_job(&self, request: &NewJob) -> Result<JobRecord, CalibrationError> {
        if request.kind != JOB_KIND {
            return Err(CalibrationError::invalid_request(
                "kind",
                format!(
                    "unsupported calibration job kind {:?}: only {:?} is defined",
                    request.kind, JOB_KIND
                ),
            ));
        }
        self.sync();
        // 单 job 互斥（比 JobManager 更早给出稳定 409，而不是把它当内部错误）。
        if let Some(pending) = self.manager.pending_resume() {
            return Err(CalibrationError::internal(format!(
                "a calibration job checkpoint is pending recovery ({}); restart the daemon to resume it before starting a new job",
                pending.job_id
            )));
        }
        if let Some(snapshot) = self.manager.snapshot() {
            if !snapshot.state.is_terminal() {
                return Err(CalibrationError::switch_busy(&snapshot.job_id.to_string()));
            }
        }
        // 方案上传必须是真实可解码的 ABI v3 文件（校验不重复造在 REST 层）。
        let (_, plan) = self.decode_uploaded_plan(&request.inputs.plan_sha)?;
        let config = self.build_offline_config(&request.inputs)?;
        let job_request = self.build_request(&request.inputs, plan.sample_rate);
        let job_id = self
            .manager
            .start(job_request, Box::new(OfflineJobBackend::new(config)))
            .map_err(start_error)?;
        let created_unix = now_unix();
        {
            let mut jobs = self.lock();
            jobs.insert(
                job_id.0,
                PersistedJob::queued(job_id.to_string(), request.inputs.clone(), created_unix),
            );
        }
        if let Err(error) = self.persist() {
            eprintln!("khaen-daemon: calibration job index: {error}");
        }
        self.sync();
        self.emit(&job_id.to_string(), JobState::Queued, None);
        let record = self.require_job(&job_id.to_string())?;
        Ok(self.record(&record))
    }

    fn list_jobs(&self, query: &JobQuery) -> Result<JobPage, CalibrationError> {
        self.sync();
        let jobs = self.lock();
        let total = jobs.len();
        let summaries = jobs
            .values()
            .rev()
            .skip(query.offset)
            .take(query.limit)
            .map(|job| self.summary(job))
            .collect();
        Ok(JobPage {
            jobs: summaries,
            total,
        })
    }

    fn job(&self, job_id: &str) -> Result<JobRecord, CalibrationError> {
        Self::parse_job_id(job_id)?;
        self.sync();
        let job = self.require_job(job_id)?;
        Ok(self.record(&job))
    }

    fn cancel(&self, job_id: &str) -> Result<JobRecord, CalibrationError> {
        let id = Self::parse_job_id(job_id)?;
        self.sync();
        let current = self.require_job(job_id)?;
        if JobState::parse(&current.state)
            .map(|state| state.is_terminal())
            .unwrap_or(true)
        {
            // 已经结束：如实返回终态，不伪造一个 cancelled。
            return Ok(self.record(&current));
        }
        match self.manager.cancel() {
            Ok(cancelled) => {
                if cancelled.0 == id {
                    // 协作式取消；等一会儿让 stage 边界收敛。超时也返回真实状态。
                    let _ = self.manager.wait_terminal(CANCEL_WAIT);
                }
            }
            Err(CalJobError::NoActiveJob) => {}
            Err(error) => return Err(manager_error(&error)),
        }
        self.sync();
        let job = self.require_job(job_id)?;
        Ok(self.record(&job))
    }

    fn artifacts(&self, job_id: &str) -> Result<Vec<ArtifactInfo>, CalibrationError> {
        self.sync();
        let job = self.require_job(job_id)?;
        Ok(job
            .artifacts
            .iter()
            .map(|artifact| ArtifactInfo {
                name: artifact.name.clone(),
                sha256: artifact.sha256.clone(),
                bytes: artifact.bytes,
                content_type: None,
            })
            .collect())
    }

    fn read_artifact(&self, job_id: &str, name: &str) -> Result<Vec<u8>, CalibrationError> {
        self.sync();
        let job = self.require_job(job_id)?;
        self.read_exposed_artifact(&job, name)
    }

    fn apply(&self, job_id: &str) -> Result<ApplyPlan, CalibrationError> {
        let id = Self::parse_job_id(job_id)?;
        self.sync();
        let job = self.require_job(job_id)?;
        let state = JobState::parse(&job.state).unwrap_or(JobState::Failed);
        if state != JobState::Succeeded {
            return Err(CalibrationError::job_not_succeeded(job_id, state.as_str()));
        }
        let plan = job.plan.as_ref().ok_or_else(|| {
            CalibrationError::plan_unavailable(
                job_id,
                "the job did not produce compile/compile-report.json with plan_sha256/graph_id",
            )
        })?;
        let path = self.job_dir(id).join(PLAN_FILE);
        let bytes = fs::read(&path).map_err(|error| {
            CalibrationError::plan_unavailable(
                job_id,
                format!("plan artifact {} is unreadable: {error}", path.display()),
            )
        })?;
        let actual = sha256_hex(&bytes);
        if !actual.eq_ignore_ascii_case(plan.sha256.trim()) {
            return Err(CalibrationError::plan_unavailable(
                job_id,
                format!(
                    "plan artifact sha256 mismatch: manifest records {}, file is {}",
                    plan.sha256.trim(),
                    actual
                ),
            ));
        }
        Ok(ApplyPlan {
            job_id: job.job_id.clone(),
            plan_path: path,
            plan_sha256: plan.sha256.clone(),
        })
    }

    fn apply_committed(
        &self,
        job_id: &str,
        applied: &AppliedVersion,
    ) -> Result<JobRecord, CalibrationError> {
        let id = Self::parse_job_id(job_id)?;
        self.sync();
        {
            let mut jobs = self.lock();
            let job = jobs
                .get_mut(&id)
                .ok_or_else(|| CalibrationError::job_not_found(job_id))?;
            // 幂等：同一版本重复回写不改动任何字段。
            if job.apply.state == ApplyState::Applied
                && job.apply.applied_version == Some(applied.version)
            {
                return Ok(self.record(job));
            }
            job.apply.state = ApplyState::Applied;
            job.apply.applied_version = Some(applied.version);
            job.apply.previous_version = applied.previous_version.or(job.apply.previous_version);
            job.apply.applied_at = Some(now_rfc3339());
            job.apply.plan_sha256 = job.plan.as_ref().map(|plan| plan.sha256.clone());
            job.updated_unix = now_unix();
        }
        if let Err(error) = self.persist() {
            eprintln!("khaen-daemon: calibration job index: {error}");
        }
        let job = self.require_job(job_id)?;
        self.emit(job_id, JobState::Succeeded, job.verdict.clone());
        Ok(self.record(&job))
    }

    fn rollback(&self, job_id: &str) -> Result<RollbackTarget, CalibrationError> {
        Self::parse_job_id(job_id)?;
        self.sync();
        let job = self.require_job(job_id)?;
        if job.apply.state != ApplyState::Applied {
            return Err(CalibrationError::rollback_unavailable(
                job_id,
                format!(
                    "job apply_state is {}: only an applied job can be rolled back",
                    job.apply.state.as_str()
                ),
            ));
        }
        let target = job.apply.previous_version.ok_or_else(|| {
            CalibrationError::rollback_unavailable(
                job_id,
                "the job was applied when no previous version was running, so there is no version to roll back to",
            )
        })?;
        if job.apply.applied_version == Some(target) {
            return Err(CalibrationError::rollback_unavailable(
                job_id,
                format!("rollback target version {target} is the version the job already applied"),
            ));
        }
        Ok(RollbackTarget {
            job_id: job.job_id.clone(),
            target_version: target,
        })
    }

    fn rollback_committed(
        &self,
        job_id: &str,
        applied: &AppliedVersion,
    ) -> Result<JobRecord, CalibrationError> {
        let id = Self::parse_job_id(job_id)?;
        self.sync();
        {
            let mut jobs = self.lock();
            let job = jobs
                .get_mut(&id)
                .ok_or_else(|| CalibrationError::job_not_found(job_id))?;
            if job.apply.state == ApplyState::RolledBack
                && job.apply.applied_version == Some(applied.version)
            {
                return Ok(self.record(job));
            }
            job.apply.state = ApplyState::RolledBack;
            job.apply.applied_version = Some(applied.version);
            job.apply.applied_at = Some(now_rfc3339());
            job.updated_unix = now_unix();
        }
        if let Err(error) = self.persist() {
            eprintln!("khaen-daemon: calibration job index: {error}");
        }
        let job = self.require_job(job_id)?;
        self.emit(job_id, JobState::Succeeded, job.verdict.clone());
        Ok(self.record(&job))
    }

    fn attach_sink(&self, sink: Arc<dyn JobSink>) {
        *self
            .sink
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(sink);
    }
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

/// 设备动作守卫：离线 job 从不需要设备；任何真实调用都 fail-closed（不静默成功）。
struct OfflineJobSafety;

const OFFLINE_DEVICE_ERROR: &str =
    "offline single-point calibration jobs never own or touch the audio device";

impl SafeOutputController for OfflineJobSafety {
    fn acquire_calibration_owner(&self) -> Result<(), SafetyError> {
        Err(SafetyError::new("OFFLINE_JOB_DEVICE", OFFLINE_DEVICE_ERROR))
    }

    fn release_calibration_owner(&self, _disposition: ExitDisposition) -> Result<(), SafetyError> {
        Err(SafetyError::new("OFFLINE_JOB_DEVICE", OFFLINE_DEVICE_ERROR))
    }

    fn restore_last_good(&self) -> Result<String, SafetyError> {
        Err(SafetyError::new("OFFLINE_JOB_DEVICE", OFFLINE_DEVICE_ERROR))
    }

    fn activate_safe_mute(&self) -> Result<(), SafetyError> {
        Err(SafetyError::new("OFFLINE_JOB_DEVICE", OFFLINE_DEVICE_ERROR))
    }

    fn last_good_version(&self) -> Option<String> {
        None
    }
}

/// JobManager 快照状态 -> daemon 五态（canonical：queued/running/succeeded/failed/cancelled）。
fn map_state(state: CalJobState) -> JobState {
    match state {
        CalJobState::Queued => JobState::Queued,
        CalJobState::Active(_) => JobState::Running,
        // 离线 job 执行成功 = 证据链完成；apply 的失败/回滚在 apply 段如实表达。
        CalJobState::Complete => JobState::Succeeded,
        CalJobState::Failed => JobState::Failed,
        CalJobState::Cancelled => JobState::Cancelled,
        // rollback 只属于 Full kind；离线 job 不会产生。真出现时执行本身已结束，
        // apply 段会给出 rolled_back，这里不新造 REST 状态名。
        CalJobState::RolledBack => JobState::Succeeded,
    }
}

/// 把 JobManager 快照刷新进索引；返回是否有字段变化。
fn apply_snapshot(artifacts_root: &Path, job: &mut PersistedJob, snapshot: &JobSnapshot) -> bool {
    let mut changed = false;
    let state = map_state(snapshot.state).as_str().to_string();
    if job.state != state {
        job.state = state;
        changed = true;
    }
    let active_step = snapshot.stage.map(|stage| stage.as_str().to_string());
    if job.active_step != active_step {
        job.active_step = active_step;
        changed = true;
    }
    let progress = f64::from(snapshot.overall_progress);
    if (job.progress - progress).abs() > f64::EPSILON {
        job.progress = progress;
        changed = true;
    }
    if job.updated_unix != snapshot.updated_unix {
        job.updated_unix = snapshot.updated_unix;
        changed = true;
    }
    if job.started_unix != snapshot.started_unix {
        job.started_unix = snapshot.started_unix;
        changed = true;
    }
    if job.finished_unix != snapshot.finished_unix {
        job.finished_unix = snapshot.finished_unix;
        changed = true;
    }
    let last_running_index = job.stage_index;
    if job.stage_index != snapshot.stage_index {
        job.stage_index = snapshot.stage_index;
        changed = true;
    }
    if snapshot.state.is_terminal() {
        // failure.stage -> plan 下标是精确值；没有 failure（例如用户取消）时用最后一个
        // 观察到的运行中 index 兜底（< stage_count 才算有效观测）。
        let from_failure = snapshot
            .failure
            .as_ref()
            .and_then(|failure| failure.stage)
            .and_then(|stage| {
                snapshot
                    .request
                    .kind
                    .plan()
                    .iter()
                    .position(|candidate| *candidate == stage)
            })
            .map(|index| index as u32);
        let fallback = if last_running_index < snapshot.stage_count {
            Some(last_running_index)
        } else {
            None
        };
        let failure_stage = from_failure.or(fallback);
        if job.failure_stage_index != failure_stage {
            job.failure_stage_index = failure_stage;
            changed = true;
        }
    }
    if job.stage_count != snapshot.stage_count {
        job.stage_count = snapshot.stage_count;
        changed = true;
    }
    let error = snapshot.failure.as_ref().map(|failure| JobError {
        code: failure.code.clone(),
        message: failure.message.clone(),
    });
    if job.error != error {
        job.error = error;
        changed = true;
    }
    // 终态写 manifest 与 snapshot 可见之间存在竞态：产物登记必须可增量补齐（幂等），
    // 不能只在 artifacts 为空时跑一次，否则 manifest 永远补不上。
    if snapshot.state.is_terminal() && collect_artifacts(artifacts_root, job, snapshot) {
        changed = true;
    }
    if snapshot.state == CalJobState::Complete && job.result.is_none() {
        collect_completion(artifacts_root, job, snapshot);
        changed = true;
    }
    changed
}

/// 终态时登记白名单产物（用 ledger 的 sha/bytes，不重复读大文件；manifest 现算一次）。
///
/// 幂等/可增量：已登记的名字不重复登记；manifest 由 JobManager 在状态变 Complete 之后才
/// 落盘，因此每轮 sync 都允许把后到的产物补上。返回是否有新增。
fn collect_artifacts(
    artifacts_root: &Path,
    job: &mut PersistedJob,
    snapshot: &JobSnapshot,
) -> bool {
    let dir = job_dir(artifacts_root, snapshot.job_id.0);
    let mut changed = false;
    for (name, relative_path) in EXPOSED_ARTIFACTS {
        if job.artifacts.iter().any(|artifact| artifact.name == name) {
            continue;
        }
        let entry = snapshot
            .artifacts
            .iter()
            .find(|artifact| artifact.relative_path == relative_path)
            .map(|record| PersistedArtifact {
                name: name.to_string(),
                relative_path: relative_path.to_string(),
                sha256: record.sha256.clone(),
                bytes: record.bytes,
            })
            .or_else(|| {
                fs::read(dir.join(relative_path))
                    .ok()
                    .map(|bytes| PersistedArtifact {
                        name: name.to_string(),
                        relative_path: relative_path.to_string(),
                        sha256: sha256_hex(&bytes),
                        bytes: bytes.len() as u64,
                    })
            });
        if let Some(entry) = entry {
            job.artifacts.push(entry);
            changed = true;
        }
    }
    if !job
        .artifacts
        .iter()
        .any(|artifact| artifact.name == ARTIFACT_MANIFEST_FILE_NAME)
    {
        if let Ok(bytes) = fs::read(dir.join(ARTIFACT_MANIFEST_FILE_NAME)) {
            job.artifacts.push(PersistedArtifact {
                name: ARTIFACT_MANIFEST_FILE_NAME.to_string(),
                relative_path: ARTIFACT_MANIFEST_FILE_NAME.to_string(),
                sha256: sha256_hex(&bytes),
                bytes: bytes.len() as u64,
            });
            changed = true;
        }
    }
    changed
}

/// 成功终态时读取编译报告与闭环报告，填充 plan / verdict / result（measured+simulated 分离）。
fn collect_completion(artifacts_root: &Path, job: &mut PersistedJob, snapshot: &JobSnapshot) {
    let dir = job_dir(artifacts_root, snapshot.job_id.0);
    let plan = read_json(&dir.join(COMPILE_REPORT_FILE)).and_then(|report| {
        let sha256 = report["plan_sha256"].as_str()?.to_string();
        let graph_id = report["graph_id"].as_str()?.to_string();
        Some(PlanResult { sha256, graph_id })
    });
    let report = read_json(&dir.join(CLOSED_LOOP_REPORT_FILE));
    job.verdict = report
        .as_ref()
        .and_then(|report| report["verdict"].as_str())
        .map(str::to_string);
    job.plan = plan.clone();
    job.result = match (report, plan) {
        (Some(report), Some(plan)) => Some(build_result(
            &report,
            plan,
            job.inputs.as_ref(),
            snapshot.request.sample_rate,
        )),
        _ => None,
    };
}

/// 闭环报告 JSON -> daemon JobResult（bands 来自 report.points[0].bands）。
fn build_result(
    report: &Value,
    plan: PlanResult,
    inputs: Option<&JobInputs>,
    sample_rate_hz: u32,
) -> JobResult {
    let bands = report["points"]
        .as_array()
        .and_then(|points| points.first())
        .and_then(|point| point["bands"].as_array())
        .map(|bands| {
            bands
                .iter()
                .filter_map(|band| {
                    let hz = band["center_hz"].as_f64()?;
                    let predicted_db = band["predicted_db"].as_f64();
                    let observed_db = band["observed_db"].as_f64();
                    let delta_db = band["delta_db"].as_f64();
                    let within_tolerance = band["within_tolerance"].as_bool();
                    Some(BandResult {
                        hz,
                        predicted_db,
                        observed_db,
                        delta_db,
                        missing: predicted_db.is_none()
                            || observed_db.is_none()
                            || within_tolerance.is_none(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let violations = report["violations"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    JobResult {
        verdict: report["verdict"].as_str().map(str::to_string),
        max_delta_db: report["max_abs_delta_db"].as_f64(),
        compared_bands: report["compared_bands"].as_u64(),
        missing_bands: report["missing_bands"].as_u64(),
        violations,
        bands,
        design: None,
        plan: Some(plan.clone()),
        measured: inputs.map(|inputs| JobMeasured {
            baseline_upload_id: inputs.baseline_upload_id.clone(),
            applied_upload_id: inputs.applied_upload_id.clone(),
            sample_rate_hz,
            source: "uploaded WAV recordings (offline single-point job, content-addressed uploads)"
                .to_string(),
        }),
        simulated: Some(JobSimulated {
            predicted_from: "khaen-calibration plan_levels: predicted band levels from plan.bin (simulated, not measured)".to_string(),
            plan_sha256: plan.sha256,
            graph_id: plan.graph_id,
        }),
    }
}

/// 由审计快照推导 5 个步骤（不伪造逐 stage 时间戳：只有 job 级时间可用）。
fn derive_steps(job: &PersistedJob) -> Vec<JobStep> {
    let state = JobState::parse(&job.state).unwrap_or(JobState::Failed);
    let count = job.stage_count.max(1) as usize;
    // 终态 stage_index 恒为 stage_count，失败/取消必须用记录下来的 failure_stage_index，
    // 否则会把"validate 就失败"的 job 的全部步骤标成 succeeded（不诚实）。
    let effective_index = if matches!(state, JobState::Failed | JobState::Cancelled) {
        job.failure_stage_index.unwrap_or(job.stage_index)
    } else {
        job.stage_index
    };
    (0..count)
        .map(|index| {
            let name = OFFLINE_JOB_STEPS
                .get(index)
                .copied()
                .unwrap_or("bundle")
                .to_string();
            let index_u32 = index as u32;
            let step_state = if state == JobState::Succeeded || index_u32 < effective_index {
                StepState::Succeeded
            } else if index_u32 == effective_index {
                match state {
                    JobState::Failed => StepState::Failed,
                    JobState::Cancelled => StepState::Cancelled,
                    JobState::Running => StepState::Running,
                    _ => StepState::Pending,
                }
            } else if state == JobState::Cancelled {
                StepState::Cancelled
            } else {
                StepState::Pending
            };
            JobStep {
                name: name.clone(),
                state: step_state,
                started_at: None,
                finished_at: None,
                sha256: stage_sha(job, &name),
                detail: None,
            }
        })
        .collect()
}

/// 步骤对应的产物 sha256（validate 的 inputs.json 不在 REST 白名单内，故为 None）。
fn stage_sha(job: &PersistedJob, step: &str) -> Option<String> {
    let name = match step {
        "compile" => "plan.bin",
        "levels" => "predicted.json",
        "closed_loop" => "report.json",
        "bundle" => ARTIFACT_MANIFEST_FILE_NAME,
        _ => return None,
    };
    job.artifacts
        .iter()
        .find(|artifact| artifact.name == name)
        .map(|artifact| artifact.sha256.clone())
}

fn load_index(path: &Path) -> Result<BTreeMap<u64, PersistedJob>, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => {
            return Err(format!(
                "read calibration job index {}: {error}",
                path.display()
            ))
        }
    };
    let index: PersistedIndex = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "parse calibration job index {}: {error} (delete/repair the file to re-enable calibration jobs)",
            path.display()
        )
    })?;
    if index.schema != INDEX_SCHEMA {
        return Err(format!(
            "calibration job index {} has unsupported schema {:?} (expected {INDEX_SCHEMA})",
            path.display(),
            index.schema
        ));
    }
    let mut jobs = BTreeMap::new();
    for job in index.jobs {
        let id = JobManagerCalibration::parse_job_id(&job.job_id).map_err(|_| {
            format!(
                "calibration job index {} contains an invalid job id {:?}",
                path.display(),
                job.job_id
            )
        })?;
        jobs.insert(id, job);
    }
    Ok(jobs)
}

fn job_dir(artifacts_root: &Path, id: u64) -> PathBuf {
    artifacts_root.join(format!("job-{id:04}"))
}

fn read_json(path: &Path) -> Option<Value> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn short_sha(sha256: &str) -> &str {
    sha256.get(..12).unwrap_or(sha256)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn rfc3339(unix: u64) -> String {
    crate::status::rfc3339_utc(UNIX_EPOCH + Duration::from_secs(unix))
}

fn now_rfc3339() -> String {
    crate::status::rfc3339_utc(SystemTime::now())
}

fn duration_ms(started_unix: Option<u64>, finished_unix: Option<u64>) -> Option<u64> {
    match (started_unix, finished_unix) {
        (Some(started), Some(finished)) => {
            Some(finished.saturating_sub(started).saturating_mul(1000))
        }
        _ => None,
    }
}

fn start_error(error: CalJobError) -> CalibrationError {
    match error {
        CalJobError::Busy => CalibrationError::new(
            axum::http::StatusCode::CONFLICT,
            super::service::code::SWITCH_BUSY,
            "another calibration job is active",
        ),
        CalJobError::InvalidRequest(message) => CalibrationError::invalid_request("job", message),
        other => CalibrationError::internal(format!("cannot start calibration job: {other}")),
    }
}

fn manager_error(error: &CalJobError) -> CalibrationError {
    match error {
        CalJobError::InvalidState(message) => {
            CalibrationError::invalid_request("state", message.clone())
        }
        other => CalibrationError::internal(format!("calibration job manager: {other}")),
    }
}
// ---------------------------------------------------------------------------
// 测试（离线：合成 WAV + ABI v3 方案，不打开设备、不出声）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::service::{
        AppliedVersion, JobInputs, NewJob, UploadRef, JOB_KIND_SINGLE_POINT_OFFLINE,
    };
    use super::*;
    use khaen_calibration::plan_compile::{compile_peq_design, PlanCompileOptions};
    use khaen_designer::modal_peq::{ChannelPeq, DesignMode, PeqDesign};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Instant;

    const SAMPLE_RATE: u32 = 48_000;
    const TONE_HZ: f64 = 1_000.0;

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "khaen-cal-backend-{}-{tag}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("temp dir");
        path
    }

    fn wav_bytes(samples: &[f64], sample_rate: u32) -> Vec<u8> {
        let data_bytes = (samples.len() * 2) as u32;
        let mut out = Vec::with_capacity(44 + samples.len() * 2);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_bytes).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&1u16.to_le_bytes()); // mono
        out.extend_from_slice(&sample_rate.to_le_bytes());
        out.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes()); // block align
        out.extend_from_slice(&16u16.to_le_bytes()); // bits
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_bytes.to_le_bytes());
        for sample in samples {
            let clamped = sample.clamp(-1.0, 1.0);
            let scaled = (clamped * 32767.0).round() as i32;
            out.extend_from_slice(&(scaled as i16).to_le_bytes());
        }
        out
    }

    fn tone(amplitude: f64, seconds: f64) -> Vec<f64> {
        let count = (f64::from(SAMPLE_RATE) * seconds).round() as usize;
        (0..count)
            .map(|index| {
                amplitude
                    * (2.0 * std::f64::consts::PI * TONE_HZ * index as f64 / f64::from(SAMPLE_RATE))
                        .sin()
            })
            .collect()
    }

    fn identity_plan_bytes() -> Vec<u8> {
        let mut design = PeqDesign::new(SAMPLE_RATE, DesignMode::ModalOnly).expect("design");
        design
            .add_channel(ChannelPeq::new("L", 0).expect("channel"))
            .expect("add channel");
        compile_peq_design(
            &design,
            &PlanCompileOptions {
                sample_rate: SAMPLE_RATE,
                block_size: 128,
                channels: 2,
                lane_gain_db: Vec::new(),
                lane_delay: Vec::new(),
            },
        )
        .expect("compile identity plan")
        .bytes
    }

    fn upload(store: &UploadStore, bytes: &[u8]) -> UploadRef {
        let (record, _) = store.save(bytes).expect("save upload");
        record.as_ref_record()
    }

    fn new_job(store: &UploadStore, baseline: &[u8], applied: &[u8]) -> NewJob {
        let plan = upload(store, &identity_plan_bytes());
        let baseline_ref = upload(store, baseline);
        let applied_ref = upload(store, applied);
        NewJob {
            kind: JOB_KIND_SINGLE_POINT_OFFLINE.to_string(),
            inputs: JobInputs {
                plan_sha: plan.sha256.clone(),
                baseline_upload_id: baseline_ref.upload_id.clone(),
                applied_upload_id: applied_ref.upload_id.clone(),
                centers_hz: vec![TONE_HZ],
                tolerance_db: 2.0,
                min_bands: 1,
            },
            baseline: baseline_ref,
            applied: applied_ref,
        }
    }

    fn wait_terminal(
        service: &JobManagerCalibration,
        job_id: &str,
        timeout: Duration,
    ) -> JobRecord {
        let deadline = Instant::now() + timeout;
        loop {
            let record = service.job(job_id).expect("job detail");
            if record.state.is_terminal() {
                return record;
            }
            assert!(
                Instant::now() < deadline,
                "job {job_id} did not reach a terminal state in {timeout:?}; last={record:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn offline_pipeline_is_auditable_and_apply_is_two_phase() {
        let dir = temp_dir("pipeline");
        let store = UploadStore::new(&dir);
        let service = JobManagerCalibration::new(&dir).expect("backend");
        let samples = tone(0.5, 1.0);
        let job = new_job(
            &store,
            &wav_bytes(&samples, SAMPLE_RATE),
            &wav_bytes(&samples, SAMPLE_RATE),
        );
        let started = service.start_job(&job).expect("start job");
        // 返回时 worker 可能已经进入首个 stage（queued -> running），两者都是真实状态。
        assert!(
            matches!(started.state, JobState::Queued | JobState::Running),
            "unexpected initial state: {:?}",
            started.state
        );
        assert!(started.created_unix.is_some());
        let job_id = started.job_id.clone();

        let done = wait_terminal(&service, &job_id, Duration::from_secs(60));
        assert_eq!(
            done.state,
            JobState::Succeeded,
            "job error: {:?}",
            done.error
        );
        assert_eq!(done.verdict.as_deref(), Some("PASS"));
        assert_eq!(done.progress, Some(1.0));
        assert!(done.created_unix.unwrap_or(0) > 0);
        assert!(done.updated_unix.unwrap_or(0) >= done.created_unix.unwrap_or(0));
        assert!(done.error.is_none());

        // 5 个步骤与产物 sha 都来自真实 ledger（不是编的）。
        assert_eq!(done.steps.len(), 5);
        let step_names: Vec<&str> = done.steps.iter().map(|step| step.name.as_str()).collect();
        assert_eq!(
            step_names,
            ["validate", "compile", "levels", "closed_loop", "bundle"]
        );
        for step in ["compile", "levels", "closed_loop", "bundle"] {
            let found = done
                .steps
                .iter()
                .find(|item| item.name == step)
                .and_then(|item| item.sha256.clone());
            assert!(
                found.is_some(),
                "step {step} must carry its artifact sha256"
            );
        }

        // 白名单产物 + manifest 审计。
        let names: Vec<&str> = done
            .artifacts
            .iter()
            .map(|artifact| artifact.name.as_str())
            .collect();
        for expected in [
            "plan.bin",
            "predicted.json",
            "observed.json",
            "report.json",
            "artifacts.manifest.json",
        ] {
            assert!(
                names.contains(&expected),
                "missing artifact {expected}: {names:?}"
            );
        }
        assert!(done
            .artifacts
            .iter()
            .all(|artifact| { artifact.sha256.len() == 64 && artifact.bytes > 0 }));
        let report_bytes = service
            .read_artifact(&job_id, "report.json")
            .expect("report artifact");
        let report_sha = done
            .artifacts
            .iter()
            .find(|artifact| artifact.name == "report.json")
            .map(|artifact| artifact.sha256.clone())
            .expect("report artifact record");
        assert_eq!(sha256_hex(&report_bytes), report_sha);
        let report: Value = serde_json::from_slice(&report_bytes).expect("report json");
        assert_eq!(
            report["schema"],
            serde_json::json!("khaen-closed-loop-report/v1")
        );
        let manifest_bytes = service
            .read_artifact(&job_id, "artifacts.manifest.json")
            .expect("manifest artifact");
        let manifest: Value = serde_json::from_slice(&manifest_bytes).expect("manifest json");
        assert_eq!(
            manifest["schema"],
            serde_json::json!("khaen-calibration-artifacts/v1")
        );

        // 实测/模拟分离（合同要求：模拟不得写成实测）。
        let result = done.result.expect("job result");
        let measured = result.measured.expect("measured block");
        assert_eq!(measured.baseline_upload_id, job.inputs.baseline_upload_id);
        assert_eq!(measured.applied_upload_id, job.inputs.applied_upload_id);
        assert_eq!(measured.sample_rate_hz, SAMPLE_RATE);
        let simulated = result.simulated.expect("simulated block");
        assert_eq!(simulated.plan_sha256, job.inputs.plan_sha);
        assert!(result.plan.is_some());

        // /api/status 来源：接线、无 active、最近 job + verdict。
        let status = service.status().expect("status");
        assert!(status.wired);
        assert!(status.active_job.is_none());
        assert_eq!(status.last_job_id.as_deref(), Some(job_id.as_str()));
        assert_eq!(status.last_verdict.as_deref(), Some("PASS"));
        let page = service
            .list_jobs(&JobQuery {
                limit: 20,
                offset: 0,
            })
            .expect("list jobs");
        assert_eq!(page.total, 1);
        assert_eq!(page.jobs[0].created_unix, done.created_unix);
        assert_eq!(page.jobs[0].progress, Some(1.0));

        // apply 两阶段：产物 -> 控制面 plan 引用（sha 与文件一致）。
        let apply_plan = service.apply(&job_id).expect("apply phase 1");
        assert_eq!(apply_plan.plan_sha256, simulated.plan_sha256);
        assert!(apply_plan.plan_path.ends_with(Path::new("plan/plan.bin")));
        let applied = service
            .apply_committed(
                &job_id,
                &AppliedVersion {
                    version: 1,
                    graph_id: simulated.graph_id.clone(),
                    previous_version: None,
                },
            )
            .expect("apply committed");
        assert_eq!(applied.apply.state, ApplyState::Applied);
        assert_eq!(applied.apply.applied_version, Some(1));
        assert!(applied.apply.applied_at.is_some());
        // 没有更旧版本 -> 回滚必须如实拒绝（不是假成功）。
        let no_target = service.rollback(&job_id).expect_err("no previous version");
        assert_eq!(no_target.code(), "rollback_unavailable");
        // 有旧版本时，回滚第一阶段给出目标版本；commit 后 job 记录 rolled_back。
        let reapplied = service
            .apply_committed(
                &job_id,
                &AppliedVersion {
                    version: 2,
                    graph_id: simulated.graph_id.clone(),
                    previous_version: Some(1),
                },
            )
            .expect("apply committed v2");
        assert_eq!(reapplied.apply.applied_version, Some(2));
        let target = service.rollback(&job_id).expect("rollback phase 1");
        assert_eq!(target.target_version, 1);
        let rolled_back = service
            .rollback_committed(
                &job_id,
                &AppliedVersion {
                    version: 3,
                    graph_id: simulated.graph_id.clone(),
                    previous_version: Some(1),
                },
            )
            .expect("rollback committed");
        assert_eq!(rolled_back.apply.state, ApplyState::RolledBack);
        assert_eq!(rolled_back.apply.applied_version, Some(3));

        // 审计索引跨实例持久化（daemon 重启后历史仍在，不要求自动 resume）。
        let reopened = JobManagerCalibration::new(&dir).expect("reopen backend");
        let persisted = reopened.job(&job_id).expect("persisted job");
        assert_eq!(persisted.state, JobState::Succeeded);
        assert_eq!(persisted.verdict.as_deref(), Some("PASS"));
        assert_eq!(persisted.artifacts.len(), done.artifacts.len());
        assert_eq!(
            reopened
                .list_jobs(&JobQuery {
                    limit: 20,
                    offset: 0
                })
                .expect("persisted list")
                .total,
            1
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_wav_fails_honestly_with_a_stable_code() {
        let dir = temp_dir("invalid-wav");
        let store = UploadStore::new(&dir);
        let service = JobManagerCalibration::new(&dir).expect("backend");
        let samples = tone(0.5, 1.0);
        let job = new_job(
            &store,
            b"this is not a RIFF/WAVE file",
            &wav_bytes(&samples, SAMPLE_RATE),
        );
        let started = service.start_job(&job).expect("start job");
        let done = wait_terminal(&service, &started.job_id, Duration::from_secs(60));
        assert_eq!(done.state, JobState::Failed);
        let error = done.error.expect("failed job must report an error");
        assert_eq!(error.code, "E_OFFLINE_JOB_INPUT");
        assert!(!error.message.is_empty());
        assert!(done.verdict.is_none());
        // 步骤状态必须诚实：失败发生在 validate 就只能是 validate=failed，其余 pending；
        // 绝不能因为终态 stage_index=stage_count 而把全部步骤标成 succeeded。
        assert_eq!(done.steps.len(), 5);
        assert_eq!(done.steps[0].state, StepState::Failed);
        assert!(done.steps[1..]
            .iter()
            .all(|step| step.state == StepState::Pending));
        // 失败 job 不可 apply：409 job_not_succeeded（不假成功）。
        let rejected = service
            .apply(&started.job_id)
            .expect_err("failed job apply");
        assert_eq!(rejected.code(), "job_not_succeeded");
        let status = service.status().expect("status");
        assert!(status.active_job.is_none());
        assert_eq!(status.last_job_id.as_deref(), Some(started.job_id.as_str()));
        assert!(status.last_verdict.is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn single_job_mutex_and_cancel_are_honest() {
        let dir = temp_dir("busy-cancel");
        let store = UploadStore::new(&dir);
        let service = JobManagerCalibration::new(&dir).expect("backend");
        // 60 s WAV 让 job 有足够时间停在 stage 里；取消是协作式的。
        let samples = tone(0.5, 60.0);
        let baseline = wav_bytes(&samples, SAMPLE_RATE);
        let job = new_job(&store, &baseline, &baseline);
        let started = service.start_job(&job).expect("start long job");
        // 单 job 互斥：第二个 job 在 active 期间必须 409 switch_busy。
        let second = new_job(&store, &baseline, &baseline);
        let busy = service
            .start_job(&second)
            .expect_err("second job must be busy");
        assert_eq!(busy.code(), "switch_busy");
        // 取消：如实进 cancelled 终态（若已完成则如实返回终态，不伪造 cancelled）。
        let cancelled = service.cancel(&started.job_id).expect("cancel");
        assert!(
            matches!(cancelled.state, JobState::Cancelled | JobState::Succeeded),
            "cancel must converge to a real terminal state, got {:?}",
            cancelled.state
        );
        if cancelled.state == JobState::Cancelled {
            assert!(cancelled.error.is_none());
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
