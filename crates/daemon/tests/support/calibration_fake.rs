//! 校准服务的测试替身：REST 层端到端测试注入它（不依赖 T70h-b 生产 backend）。
//!
//! 只实现 REST 层需要的行为，语义尽量贴近真实后端契约：
//! - start_job 原子互斥（已有 active job → 409 switch_busy）；
//! - 步骤按 compile→levels→closed_loop→bundle 前进；
//! - apply/rollback 两阶段：apply 返回预先登记的 plan 引用，apply_committed/rollback_committed
//!   记录控制面 commit 结果并推事件给 daemon（JobSink）；
//! - 产物来自登记表；可以登记“字节与 manifest sha 不符”的产物来测 409。
//!
//! 测试通过公开方法造数据（seed_succeeded/set_apply_plan/add_artifact/...），并按需检查
//! 调用记录（calls）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use khaen_daemon::calibration::service::{
    AppliedVersion, ApplyPlan, ApplyState, ArtifactInfo, CalibrationError, CalibrationService,
    CalibrationStatus, JobApply, JobEvent, JobInputs, JobPage, JobQuery, JobRecord, JobSink,
    JobState, JobStep, NewJob, RollbackTarget, StepState, OFFLINE_JOB_STEPS,
};
use khaen_host::sha256_hex;

/// 测试夹具统一用的时间戳（固定值，断言稳定）。
pub const TS: &str = "2026-11-26T00:00:00Z";

#[derive(Default)]
struct FakeState {
    /// 旧 → 新（列表按新 → 旧返回）。
    jobs: Vec<JobRecord>,
    next_id: u64,
    /// job_id → 可应用的 plan。
    plans: HashMap<String, ApplyPlan>,
    /// job_id → 回滚目标版本（未登记时用 apply.previous_version）。
    rollback_targets: HashMap<String, u64>,
    artifacts: HashMap<String, Vec<ArtifactInfo>>,
    /// (job_id, name) → 产物字节。
    artifact_bytes: HashMap<(String, String), Vec<u8>>,
    status_error: Option<String>,
    calls: Vec<String>,
}

/// 可注入的假校准服务。
pub struct FakeCalibration {
    state: Mutex<FakeState>,
    sink: Mutex<Option<Arc<dyn JobSink>>>,
}

impl FakeCalibration {
    /// 新建（返回 Arc，直接交给 spawn_with_calibration）。
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(FakeState::default()),
            sink: Mutex::new(None),
        })
    }

    fn lock(&self) -> MutexGuard<'_, FakeState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn sink(&self) -> Option<Arc<dyn JobSink>> {
        self.sink
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 推一个 job 事件给 daemon（测试 /api/status 的事件缓存合并）。
    pub fn publish(&self, job_id: &str, state: JobState, verdict: Option<String>) {
        if let Some(sink) = self.sink() {
            sink.job_event(JobEvent {
                job_id: job_id.to_string(),
                state,
                verdict,
                at: Some(TS.to_string()),
            });
        }
    }

    /// 调用记录（断言 apply/rollback 是否真的走到服务）。
    pub fn calls(&self) -> Vec<String> {
        self.lock().calls.clone()
    }

    /// 让 status() 返回 500（测 /api/status 的事件缓存兜底与 /api/meta 的降级）。
    pub fn fail_status(&self, message: &str) {
        self.lock().status_error = Some(message.to_string());
    }

    /// 直接种一个 job 快照（造历史 job / 详情用）。
    pub fn seed_job(&self, record: JobRecord) {
        self.lock().jobs.push(record);
    }

    /// 造一个已成功、带 5 个步骤 sha、result.plan 的 job；返回 job_id。
    pub fn seed_succeeded(&self, verdict: &str, plan_sha: &str, graph_id: &str) -> String {
        let mut state = self.lock();
        state.next_id += 1;
        let job_id = format!("job-{:04}", state.next_id);
        let steps = OFFLINE_JOB_STEPS
            .iter()
            .enumerate()
            .map(|(index, name)| JobStep {
                name: (*name).to_string(),
                state: StepState::Succeeded,
                started_at: Some(TS.to_string()),
                finished_at: Some(TS.to_string()),
                sha256: Some(format!("{:064x}", index + 1)),
                detail: None,
            })
            .collect();
        let record = JobRecord {
            job_id: job_id.clone(),
            kind: "single_point_offline".to_string(),
            state: JobState::Succeeded,
            active_step: None,
            verdict: Some(verdict.to_string()),
            created_at: Some(TS.to_string()),
            created_unix: Some(1_700_000_000),
            started_at: Some(TS.to_string()),
            finished_at: Some(TS.to_string()),
            updated_unix: Some(1_700_000_001),
            progress: Some(1.0),
            duration_ms: Some(1234),
            engine: None,
            steps,
            artifacts: Vec::new(),
            inputs: Some(JobInputs {
                plan_sha: "0".repeat(64),
                baseline_upload_id: "1".repeat(64),
                applied_upload_id: "2".repeat(64),
                centers_hz: vec![100.0, 1000.0, 10_000.0],
                tolerance_db: 2.0,
                min_bands: 3,
            }),
            result: Some(khaen_daemon::calibration::service::JobResult {
                verdict: Some(verdict.to_string()),
                max_delta_db: Some(0.5),
                compared_bands: Some(3),
                missing_bands: Some(0),
                violations: Vec::new(),
                bands: Vec::new(),
                design: Some(khaen_daemon::calibration::service::DesignResult {
                    residual: Some("FULL".to_string()),
                    admission_accepted: Some(true),
                    admission_code: None,
                    admission_reason: None,
                }),
                plan: Some(khaen_daemon::calibration::service::PlanResult {
                    sha256: plan_sha.to_string(),
                    graph_id: graph_id.to_string(),
                }),
                measured: None,
                simulated: None,
            }),
            error: None,
            apply: JobApply::default(),
            reasons: Default::default(),
        };
        state.jobs.push(record);
        job_id
    }

    /// 登记 job 的可应用 plan（apply 第一阶段返回它）。
    pub fn set_apply_plan(
        &self,
        job_id: &str,
        plan_path: impl Into<std::path::PathBuf>,
        plan_sha: &str,
    ) {
        self.lock().plans.insert(
            job_id.to_string(),
            ApplyPlan {
                job_id: job_id.to_string(),
                plan_path: plan_path.into(),
                plan_sha256: plan_sha.to_string(),
            },
        );
    }

    /// 登记 job 的回滚目标版本。
    pub fn set_rollback_target(&self, job_id: &str, version: u64) {
        self.lock()
            .rollback_targets
            .insert(job_id.to_string(), version);
    }

    /// 登记产物（manifest sha 与字节一致）。
    pub fn add_artifact(&self, job_id: &str, name: &str, bytes: &[u8]) {
        let sha256 = sha256_hex(bytes);
        self.add_artifact_with_sha(job_id, name, bytes, &sha256);
    }

    /// 登记产物（manifest sha 由调用方指定；可以故意与字节不符来测 409）。
    pub fn add_artifact_with_sha(&self, job_id: &str, name: &str, bytes: &[u8], sha256: &str) {
        let mut state = self.lock();
        let info = ArtifactInfo {
            name: name.to_string(),
            sha256: sha256.to_string(),
            bytes: bytes.len() as u64,
            content_type: None,
        };
        state
            .artifacts
            .entry(job_id.to_string())
            .or_default()
            .push(info.clone());
        // 后端快照里的 artifacts 与 manifest 同源（真实 backend 也是这样回显的）。
        if let Some(job) = state.jobs.iter_mut().find(|job| job.job_id == job_id) {
            job.artifacts.push(info);
        }
        state
            .artifact_bytes
            .insert((job_id.to_string(), name.to_string()), bytes.to_vec());
    }

    /// 把 job 置为终态（测试造 failed/cancelled 历史）。
    pub fn finish_job(&self, job_id: &str, state: JobState, verdict: Option<String>) {
        let mut guard = self.lock();
        if let Some(job) = guard.jobs.iter_mut().find(|job| job.job_id == job_id) {
            job.state = state;
            job.verdict = verdict;
            job.finished_at = Some(TS.to_string());
            job.active_step = None;
        }
    }
}

impl CalibrationService for FakeCalibration {
    fn status(&self) -> Result<CalibrationStatus, CalibrationError> {
        let state = self.lock();
        if let Some(message) = &state.status_error {
            return Err(CalibrationError::internal(message.clone()));
        }
        let active_job = state
            .jobs
            .iter()
            .rev()
            .find(|job| !job.state.is_terminal())
            .map(|job| job.job_id.clone());
        let last = state.jobs.iter().rev().find(|job| job.state.is_terminal());
        Ok(CalibrationStatus {
            wired: true,
            engine: None,
            active_job,
            last_job_id: last.map(|job| job.job_id.clone()),
            last_verdict: last.and_then(|job| job.verdict.clone()),
            reason: None,
        })
    }

    fn start_job(&self, request: &NewJob) -> Result<JobRecord, CalibrationError> {
        let mut state = self.lock();
        state.calls.push("start_job".to_string());
        if let Some(active) = state.jobs.iter().find(|job| !job.state.is_terminal()) {
            return Err(CalibrationError::switch_busy(&active.job_id));
        }
        state.next_id += 1;
        let job_id = format!("job-{:04}", state.next_id);
        let mut record = JobRecord::new_queued(job_id.clone(), request, Some(TS.to_string()));
        record.state = JobState::Running;
        record.started_at = Some(TS.to_string());
        record.active_step = Some(OFFLINE_JOB_STEPS[0].to_string());
        if let Some(step) = record.steps.first_mut() {
            step.state = StepState::Running;
            step.started_at = Some(TS.to_string());
        }
        let created = record.clone();
        state.jobs.push(record);
        drop(state);
        self.publish(&job_id, JobState::Running, None);
        Ok(created)
    }

    fn list_jobs(&self, query: &JobQuery) -> Result<JobPage, CalibrationError> {
        let state = self.lock();
        let total = state.jobs.len();
        let jobs: Vec<_> = state
            .jobs
            .iter()
            .rev()
            .skip(query.offset)
            .take(query.limit)
            .map(khaen_daemon::calibration::service::JobSummary::from_record)
            .collect();
        Ok(JobPage { jobs, total })
    }

    fn job(&self, job_id: &str) -> Result<JobRecord, CalibrationError> {
        let state = self.lock();
        state
            .jobs
            .iter()
            .find(|job| job.job_id == job_id)
            .cloned()
            .ok_or_else(|| CalibrationError::job_not_found(job_id))
    }

    fn cancel(&self, job_id: &str) -> Result<JobRecord, CalibrationError> {
        let mut state = self.lock();
        state.calls.push(format!("cancel:{job_id}"));
        let job = state
            .jobs
            .iter_mut()
            .find(|job| job.job_id == job_id)
            .ok_or_else(|| CalibrationError::job_not_found(job_id))?;
        if !job.state.is_terminal() {
            job.state = JobState::Cancelled;
            job.finished_at = Some(TS.to_string());
            job.active_step = None;
            for step in &mut job.steps {
                if step.state == StepState::Running {
                    step.state = StepState::Cancelled;
                    step.finished_at = Some(TS.to_string());
                }
            }
        }
        let record = job.clone();
        drop(state);
        self.publish(job_id, JobState::Cancelled, None);
        Ok(record)
    }

    fn artifacts(&self, job_id: &str) -> Result<Vec<ArtifactInfo>, CalibrationError> {
        let state = self.lock();
        if !state.jobs.iter().any(|job| job.job_id == job_id) {
            return Err(CalibrationError::job_not_found(job_id));
        }
        Ok(state.artifacts.get(job_id).cloned().unwrap_or_default())
    }

    fn read_artifact(&self, job_id: &str, name: &str) -> Result<Vec<u8>, CalibrationError> {
        let state = self.lock();
        state
            .artifact_bytes
            .get(&(job_id.to_string(), name.to_string()))
            .cloned()
            .ok_or_else(|| CalibrationError::artifact_not_found(job_id, name))
    }

    fn apply(&self, job_id: &str) -> Result<ApplyPlan, CalibrationError> {
        let mut state = self.lock();
        state.calls.push(format!("apply:{job_id}"));
        let job = state
            .jobs
            .iter()
            .find(|job| job.job_id == job_id)
            .ok_or_else(|| CalibrationError::job_not_found(job_id))?;
        if job.state != JobState::Succeeded {
            return Err(CalibrationError::job_not_succeeded(
                job_id,
                job.state.as_str(),
            ));
        }
        state.plans.get(job_id).cloned().ok_or_else(|| {
            CalibrationError::plan_unavailable(job_id, "fake has no plan registered")
        })
    }

    fn apply_committed(
        &self,
        job_id: &str,
        applied: &AppliedVersion,
    ) -> Result<JobRecord, CalibrationError> {
        let mut state = self.lock();
        state.calls.push(format!("apply_committed:{job_id}"));
        let plan_sha256 = state.plans.get(job_id).map(|plan| plan.plan_sha256.clone());
        let job = state
            .jobs
            .iter_mut()
            .find(|job| job.job_id == job_id)
            .ok_or_else(|| CalibrationError::job_not_found(job_id))?;
        job.apply = JobApply {
            state: ApplyState::Applied,
            applied_version: Some(applied.version),
            previous_version: applied.previous_version,
            applied_at: Some(TS.to_string()),
            plan_sha256,
        };
        let record = job.clone();
        drop(state);
        self.publish(job_id, JobState::Succeeded, record.verdict.clone());
        Ok(record)
    }

    fn rollback(&self, job_id: &str) -> Result<RollbackTarget, CalibrationError> {
        let mut state = self.lock();
        state.calls.push(format!("rollback:{job_id}"));
        let job = state
            .jobs
            .iter()
            .find(|job| job.job_id == job_id)
            .ok_or_else(|| CalibrationError::job_not_found(job_id))?;
        if job.apply.state != ApplyState::Applied {
            return Err(CalibrationError::rollback_unavailable(
                job_id,
                format!("apply_state is {}", job.apply.state.as_str()),
            ));
        }
        let target = state
            .rollback_targets
            .get(job_id)
            .copied()
            .or(job.apply.previous_version)
            .ok_or_else(|| {
                CalibrationError::rollback_unavailable(
                    job_id,
                    "the job was applied before any plan version existed",
                )
            })?;
        Ok(RollbackTarget {
            job_id: job_id.to_string(),
            target_version: target,
        })
    }

    fn rollback_committed(
        &self,
        job_id: &str,
        applied: &AppliedVersion,
    ) -> Result<JobRecord, CalibrationError> {
        let mut state = self.lock();
        state.calls.push(format!("rollback_committed:{job_id}"));
        let job = state
            .jobs
            .iter_mut()
            .find(|job| job.job_id == job_id)
            .ok_or_else(|| CalibrationError::job_not_found(job_id))?;
        job.apply.state = ApplyState::RolledBack;
        job.apply.applied_version = Some(applied.version);
        job.apply.previous_version = applied.previous_version;
        job.apply.applied_at = Some(TS.to_string());
        let record = job.clone();
        drop(state);
        self.publish(job_id, JobState::Succeeded, record.verdict.clone());
        Ok(record)
    }

    fn attach_sink(&self, sink: Arc<dyn JobSink>) {
        *self
            .sink
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(sink);
    }
}
