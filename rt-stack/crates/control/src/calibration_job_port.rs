//! [NONRT] 校准 job 的 CalibrationPort 真实现（T60 JobManager -> T67 FitWiring -> T66 compile/apply）。
//!
//! 角色：khaen-rt-daemon（control/src/main.rs）在启动时构造本 port 并
//! RestService::with_calibration(..)，于是 T42 的 R48 端点
//! （start/status/list/cancel/apply/retest/rollback）接到**真实的** T60 job 状态机：
//! 单 job 互斥、stage 进度、checkpoint、协作式取消、prepared->apply、rollback last-good
//! 都由 khaen_calibration::job::JobManager 执行，不是内存投影。
//!
//! ## 离线口径（任务 task-226：夜间纯离线，不出声）
//! - 测量引擎 = stage_backend::EngineKind::Fake（合成样本，不打开声卡）；
//! - 设计输入 = fit_wiring::synthetic_design()（stereo-2.0 离线夹具，measured=false）；
//! - apply all-ack = OfflineSyntheticAck（T66 NONRT 离线模型，不是板端 4-worker actual）。
//!
//! 因此本 port 的 engine_info().synthetic **恒为 true**（real_audio=false），REST 响应
//! 必须原样带出 engine/synthetic，页面不得显示成板端 actual。真实 MOTU/UMIK 测量与板端
//! apply 属板端窗口（NOT_WIRED）；daemon 的 KHAEN_CAL_JOB_BACKEND=real 不挂本 port，
//! REST 显式 503，绝不冒充 actual。
//!
//! ## 语义要点
//! - start：JobManager::start 立即返回（worker 线程跑 stage）；同一时刻只允许一个
//!   active job，冲突映射 409 switch_busy。
//! - get/list：每次从 JobManager::snapshot()/last_finished() 刷新，返回 stage/整体进度/
//!   applied_version/last_good_version/failure 的只读投影。
//! - apply：只对停在 prepared（awaiting_apply）的 job 放行；等待 apply 段终态并
//!   要求 T66 回执带 applied_version（缺失按 503 拒绝，不假 ack）。已 apply 的终态 job
//!   重复 apply 幂等返回同一 view。
//! - cancel：协作式取消 active job 并等待终态；终态 job 取消 -> 409。
//! - retest：用已 applied version 作为 expected_applied_version 启动 Retest job。
//! - rollback：active job 走 JobManager::rollback（prepared 前/apply 中）；已终态 job
//!   走离线安全面 restore_last_good() 并返回 rolled_back view（必须带 last_good_version）。
//!
//! 测试：本文件的 #[cfg(test)] mod tests（纯离线、临时目录、不打开设备）；
//! REST 语义的 T42 契约用例仍在 tests/T42/rest_contract.rs（MemoryCalibration 替身）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use khaen_calibration::fit_wiring::{synthetic_design, FitWiring};
use khaen_calibration::job::{
    sha256_hex, CalibrationJobBackend, ExitDisposition, JobError, JobKind, JobManager,
    JobManagerConfig, JobRequest, JobSnapshot, JobStage, SafeOutputController, SafetyError,
};

use super::calibration_cli_stage::{
    CliSafety, CliStageBackend, CliStageConfig, CAL_PORT_BACKEND_CLI, CAL_PORT_ENGINE_CLI,
    CAL_PORT_NOTE_CLI,
};
use khaen_calibration::stage_backend::{
    EngineKind, StageBackend, StageBackendConfig, StageDeviceConfig, StageLevelConfig,
    StageMeasurementMode, StageSweepConfig,
};
use khaen_calibration::sweep::SweepOutput;
use khaen_core::abi::sample_format::S24_3LE_INTERLEAVED;

use super::*;

/// 离线合成 port 的 engine 名（REST 响应的 engine 字段；页面/证据用它区分 actual）。
pub const CAL_PORT_ENGINE_SYNTHETIC: &str = "t60-jobmanager-synthetic";
/// 离线合成 port 的后端说明（测量=stage fake，设计=synthetic_offline，apply=T66 NONRT 模型）。
pub const CAL_PORT_BACKEND_OFFLINE: &str = "stage-fake+synthetic_offline+offline-synthetic-ack";
/// apply/rollback 等待终态的上限（离线合成段应为毫秒级；超时按 503 报，不假成功）。
const WAIT_TERMINAL_TIMEOUT: Duration = Duration::from_secs(60);
/// last-good 版本号（离线安全面；不是板端真实回滚证据）。
const OFFLINE_LAST_GOOD: &str = "last-good-offline-v0";
/// 默认布局（synthetic_offline 夹具口径；板端 P1 4.1.2 由真实 backend 接管）。
const DEFAULT_LAYOUT: &str = "stereo-2.0";

/// 离线安全输出控制面（daemon 离线模式）。
///
/// T60 SafeOutputController 的设备交接/恢复钩子。离线模式没有真实设备、没有真实
/// last-good 图：这里只返回固定版本号，**不代表板端恢复/静音已执行**（证据必须标注）。
#[derive(Debug, Default)]
struct OfflineSafety;

impl SafeOutputController for OfflineSafety {
    fn acquire_calibration_owner(&self) -> Result<(), SafetyError> {
        Ok(())
    }

    fn release_calibration_owner(&self, _disposition: ExitDisposition) -> Result<(), SafetyError> {
        Ok(())
    }

    fn restore_last_good(&self) -> Result<String, SafetyError> {
        Ok(OFFLINE_LAST_GOOD.to_string())
    }

    fn activate_safe_mute(&self) -> Result<(), SafetyError> {
        Ok(())
    }

    fn last_good_version(&self) -> Option<String> {
        Some(OFFLINE_LAST_GOOD.to_string())
    }
}

/// 校准 job 的 REST port（T60 JobManager 适配；单实例，daemon 内串行使用）。
pub struct JobManagerPort {
    manager: JobManager,
    safety: Arc<dyn SafeOutputController>,
    /// 历史 job 投影（REST list 需要历史；JobManager 只保留 active + last_finished）。
    jobs: BTreeMap<String, CalJobView>,
    engine: &'static str,
    backend: &'static str,
    real_audio: bool,
    layout: String,
    sample_rate: u32,
    design_points: u32,
    holdout_points: u32,
    repeats: u32,
    drive_level_dbfs: f32,
    /// CLI stage 后端配置（None = 离线合成 Fake 后端，行为不变）。
    cli: Option<CliStageConfig>,
    /// 最近一次 REST start 的可闻扫频授权（真实后端 retest 复用；离线模式恒 true）。
    audio_authorized: bool,
}

impl JobManagerPort {
    /// 离线合成 port（默认 daemon 模式）：Fake 测量 + synthetic_offline 设计 + 离线 apply ack。
    ///
    /// artifacts 根目录不存在会被创建；checkpoint_path=None 表示无状态（daemon 离线模式
    /// 不承诺跨重启 resume；重启恢复属板端 follow-up，这里不伪造）。
    pub fn synthetic_offline(
        artifacts_root: impl Into<PathBuf>,
        checkpoint_path: Option<PathBuf>,
    ) -> Result<Self, String> {
        let safety: Arc<dyn SafeOutputController> = Arc::new(OfflineSafety);
        let manager = JobManager::new(
            JobManagerConfig {
                artifacts_root: artifacts_root.into(),
                checkpoint_path,
                apply_confirm_timeout: None,
            },
            safety.clone(),
        )
        .map_err(|error| format!("job manager init: {error}"))?;
        Ok(JobManagerPort {
            manager,
            safety,
            jobs: BTreeMap::new(),
            engine: CAL_PORT_ENGINE_SYNTHETIC,
            backend: CAL_PORT_BACKEND_OFFLINE,
            real_audio: false,
            layout: DEFAULT_LAYOUT.to_string(),
            sample_rate: 48_000,
            design_points: 1,
            holdout_points: 1,
            repeats: 1,
            drive_level_dbfs: -50.0,
            cli: None,
            audio_authorized: true,
        })
    }

    /// CLI stage 后端 port（`KHAEN_CAL_STAGE=cli`）：真实 MOTU/UMIK 测量 + file-backend apply。
    ///
    /// 与 `synthetic_offline` 共用同一 T60 JobManager/REST 语义；区别只在
    /// backend（本机 calib-stage.sh）与 safety（systemctl stop/start khaen-rt-host）。
    /// apply 只能写 active.yml（host 尚未消费 calibration.yml），因此 engine_info 的
    /// note 明确 NOT_APPLIED_TO_AUDIO，不得标 actual。
    pub fn cli_stage(
        artifacts_root: impl Into<PathBuf>,
        checkpoint_path: Option<PathBuf>,
    ) -> Result<Self, String> {
        let config = CliStageConfig::from_env();
        let safety: Arc<dyn SafeOutputController> = Arc::new(CliSafety::from_env());
        let manager = JobManager::new(
            JobManagerConfig {
                artifacts_root: artifacts_root.into(),
                checkpoint_path,
                apply_confirm_timeout: None,
            },
            safety.clone(),
        )
        .map_err(|error| format!("job manager init: {error}"))?;
        Ok(JobManagerPort {
            manager,
            safety,
            jobs: BTreeMap::new(),
            engine: CAL_PORT_ENGINE_CLI,
            backend: CAL_PORT_BACKEND_CLI,
            real_audio: true,
            layout: config.layout.clone(),
            sample_rate: config.sample_rate,
            design_points: 1,
            holdout_points: 1,
            repeats: config.repeats,
            drive_level_dbfs: config.drive_level_dbfs,
            cli: Some(config),
            audio_authorized: false,
        })
    }

    /// 启动时恢复 checkpoint 里的 pending job（离线合成 backend 重跑 in-progress stage）。
    ///
    /// 返回 Some(view) = 已恢复；None = 没有 pending；Err = checkpoint 坏/backend 构造失败
    /// （daemon 只记 warning，不假装恢复成功）。
    pub fn resume_pending(&mut self) -> Result<Option<CalJobView>, String> {
        let Some(pending) = self.manager.pending_resume() else {
            return Ok(None);
        };
        let label = pending.request.label.clone();
        let backend = self.build_backend(&label)?;
        let job_id = self
            .manager
            .resume(backend)
            .map_err(|error| format!("resume pending job: {error}"))?;
        self.refresh();
        Ok(self.jobs.get(&job_id.to_string()).cloned())
    }

    /// engine/synthetic 自述（REST 响应原样带出；synthetic=true 表示测量/apply 是离线模型）。
    ///
    /// CLI 模式返回 synthetic=false/real_audio=true，但 note 明确 apply 是 file-backend
    /// 且 host 尚未消费 calibration.yml = NOT_APPLIED_TO_AUDIO（不是 actual）。
    pub fn offline_engine_info(&self) -> CalEngineInfo {
        CalEngineInfo {
            engine: self.engine,
            synthetic: !self.real_audio,
            backend: self.backend,
            real_audio: self.real_audio,
            note: if self.real_audio {
                CAL_PORT_NOTE_CLI
            } else {
                "T60 JobManager 状态机真实执行；测量=stage fake、设计=synthetic_offline、apply=T66 离线合成 ack；板端 MOTU/UMIK 与真实 4-worker all-ack NOT_RUN"
            },
        }
    }

    /// 按 port 模式构造 job backend（合成 = 离线 Fake；CLI = 本机真实 stage 脚本）。
    fn build_backend(&self, label: &str) -> Result<Box<dyn CalibrationJobBackend>, String> {
        match &self.cli {
            Some(config) => Ok(Box::new(CliStageBackend::new(config.clone()))),
            None => build_offline_backend(label),
        }
    }

    fn unix_now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0)
    }

    fn view(&self, snapshot: &JobSnapshot) -> CalJobView {
        let mut failure = snapshot.failure.as_ref().map(|failure| {
            let stage = failure
                .stage
                .map(|stage| format!(" @{}", stage.as_str()))
                .unwrap_or_default();
            format!("{}{}: {}", failure.code, stage, failure.message)
        });
        // T60 的取消终态原因在 terminal_reason（failure 多为 None）；REST 页面需要可见原因。
        if failure.is_none() && snapshot.state.as_str() == "cancelled" {
            failure = Some(match &snapshot.terminal_reason {
                Some(reason) => format!("CANCELLED: {reason}"),
                None => "CANCELLED: cancelled_by_user".to_string(),
            });
        }
        CalJobView {
            job_id: snapshot.job_id.to_string(),
            kind: snapshot.kind.as_str().to_string(),
            state: snapshot.state.as_str().to_string(),
            stage: snapshot.stage.map(|stage| stage.as_str().to_string()),
            progress: f64::from(snapshot.overall_progress.clamp(0.0, 1.0)),
            applied_version: snapshot.applied_version.clone(),
            last_good_version: snapshot
                .last_good_version
                .clone()
                .or_else(|| self.safety.last_good_version()),
            failure,
            created_unix: snapshot.created_unix,
            updated_unix: snapshot.updated_unix,
        }
    }

    fn record(&mut self, snapshot: &JobSnapshot) {
        let view = self.view(snapshot);
        self.jobs.insert(view.job_id.clone(), view);
    }

    /// 从 JobManager（active + last_finished）刷新历史投影。
    fn refresh(&mut self) {
        if let Some(snapshot) = self.manager.snapshot() {
            self.record(&snapshot);
        }
        if let Some(snapshot) = self.manager.last_finished() {
            self.record(&snapshot);
        }
    }

    fn latest_applied_version(&self) -> Option<String> {
        self.jobs
            .values()
            .rev()
            .find_map(|view| view.applied_version.clone())
    }

    fn current_job_id(&self) -> Option<String> {
        self.manager
            .snapshot()
            .map(|snapshot| snapshot.job_id.to_string())
    }

    fn request_for(
        &self,
        kind: JobKind,
        label: &str,
        auto_apply: bool,
        audio_authorized: bool,
    ) -> JobRequest {
        JobRequest {
            kind,
            label: label.to_string(),
            layout: self.layout.clone(),
            sample_rate: self.sample_rate,
            design_points: self.design_points,
            holdout_points: self.holdout_points,
            repeats: self.repeats,
            drive_level_dbfs: self.drive_level_dbfs,
            // P08：离线 backend 不发声（Fake 测量），故不涉及可闻扫频授权；真实 backend
            // 必须由 REST start 显式授权（audible_sweep_authorized=true），否则拒绝（不静默放行）。
            audible_sweep_authorized: !self.real_audio || audio_authorized,
            auto_apply,
            expected_applied_version: None,
            inputs: Vec::new(),
        }
    }

    fn start_job(
        &mut self,
        kind: JobKind,
        label: &str,
        auto_apply: bool,
        expected_applied_version: Option<String>,
        audio_authorized: bool,
    ) -> Result<CalJobView, CalPortError> {
        let mut request = self.request_for(kind, label, auto_apply, audio_authorized);
        request.expected_applied_version = expected_applied_version;
        let backend = self
            .build_backend(label)
            .map_err(CalPortError::Unavailable)?;
        let job_id = self
            .manager
            .start(request, backend)
            .map_err(map_job_error)?;
        self.refresh();
        let key = job_id.to_string();
        self.jobs.get(&key).cloned().ok_or_else(|| {
            CalPortError::Unavailable(
                "job manager accepted the job but produced no snapshot".to_string(),
            )
        })
    }

    /// 等待终态（T60 的 completed 标志，而不是只看 state；有界超时，超时 503）。
    ///
    /// 注意：不能只轮询 state 字符串——worker 先写终态 state、后置 completed 并交还
    /// active 句柄，提前返回会让紧随的 retest 撞上 Busy。
    fn wait_terminal(&self) -> Result<JobSnapshot, CalPortError> {
        self.manager
            .wait_terminal(WAIT_TERMINAL_TIMEOUT)
            .map_err(|error| match error {
                JobError::Timeout => CalPortError::Unavailable(
                    "timed out waiting for calibration job terminal state".to_string(),
                ),
                other => CalPortError::Unavailable(format!("job manager: {other}")),
            })
    }

    /// 已终态 job 的 rollback：走离线安全面恢复 last-good，并返回 rolled_back 投影。
    ///
    /// 注意：JobManager 的 rollback 只接受 active job；终态 job 的「回滚到 last-good」
    /// 在 daemon 侧是一次安全面操作（板端对应真实 last-good 图/系数切换，NOT_RUN）。
    fn rollback_finished(&mut self, job_id: &str) -> Result<CalJobView, CalPortError> {
        let last_good = self
            .safety
            .last_good_version()
            .ok_or(CalPortError::Rejected(RtStatus::ERR_PLAN_STALE))?;
        self.safety
            .restore_last_good()
            .map_err(|error| CalPortError::Unavailable(format!("{error}")))?;
        let mut view = self
            .jobs
            .get(job_id)
            .cloned()
            .ok_or(CalPortError::NotFound)?;
        view.state = "rolled_back".to_string();
        view.stage = None;
        view.progress = 1.0;
        view.applied_version = None;
        view.last_good_version = Some(last_good);
        view.failure = None;
        view.updated_unix = Self::unix_now();
        self.jobs.insert(job_id.to_string(), view.clone());
        Ok(view)
    }
}

impl CalibrationPort for JobManagerPort {
    fn start(&mut self, request: &CalStartRequest) -> Result<CalJobView, CalPortError> {
        // level/delay/bass 的 host 字段尚未落地：JobManagerPort 没有这些 stage 的实现，
        // 明确 501 pending_host_field，不伪造 job/测量。
        if let Some(stage) = request.stage.as_deref() {
            if CAL_EXTRA_STAGES.contains(&stage) {
                return Err(CalPortError::PendingHostField(format!(
                    "stage {stage} is pending host fields; this daemon backend (T60 JobManager synthetic) has no level/delay/bass implementation"
                )));
            }
        }
        self.refresh();
        let label = match &request.label {
            Some(label) if !label.trim().is_empty() => label.clone(),
            _ => format!("rest-calibration-{}", Self::unix_now()),
        };
        // P08：记录本次显式授权（真实后端 retest 端点复用；离线模式无副作用）。
        self.audio_authorized = request.audible_sweep_authorized;
        match request.kind.as_str() {
            "full" => self.start_job(
                JobKind::Full,
                &label,
                request.auto_apply,
                None,
                request.audible_sweep_authorized,
            ),
            "retest" => {
                let expected = self
                    .latest_applied_version()
                    .ok_or(CalPortError::Rejected(RtStatus::ERR_PLAN_STALE))?;
                self.start_job(
                    JobKind::Retest,
                    &label,
                    request.auto_apply,
                    Some(expected),
                    request.audible_sweep_authorized,
                )
            }
            _ => Err(CalPortError::Rejected(RtStatus::ERR_BAD_LAYOUT)),
        }
    }

    fn get(&mut self, job_id: &str) -> Option<CalJobView> {
        self.refresh();
        self.jobs.get(job_id).cloned()
    }

    fn list(&mut self) -> Vec<CalJobView> {
        self.refresh();
        self.jobs.values().cloned().collect()
    }

    fn cancel(&mut self, job_id: &str) -> Result<CalJobView, CalPortError> {
        self.refresh();
        let Some(snapshot) = self.manager.snapshot() else {
            return match self.jobs.get(job_id) {
                Some(_) => Err(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY)),
                None => Err(CalPortError::NotFound),
            };
        };
        if snapshot.job_id.to_string() != job_id {
            return if self.jobs.contains_key(job_id) {
                Err(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY))
            } else {
                Err(CalPortError::NotFound)
            };
        }
        if snapshot.state.is_terminal() {
            return Err(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY));
        }
        self.manager.cancel().map_err(map_job_error)?;
        let terminal = self.wait_terminal()?;
        self.record(&terminal);
        self.jobs.get(job_id).cloned().ok_or(CalPortError::NotFound)
    }

    fn apply(&mut self, job_id: &str) -> Result<CalJobView, CalPortError> {
        self.refresh();
        match self.manager.snapshot() {
            Some(snapshot) if snapshot.job_id.to_string() == job_id => {
                if snapshot.state.is_terminal() {
                    // 幂等：已 apply 的终态 job 重复 apply 返回同一 view（不二次提交）。
                    if snapshot.applied_version.is_some() {
                        return self.jobs.get(job_id).cloned().ok_or(CalPortError::NotFound);
                    }
                    return Err(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY));
                }
                if !snapshot.awaiting_apply {
                    let already = snapshot.applied_by_job || snapshot.applied_version.is_some();
                    if !already {
                        // T60 worker 进入 Prepared stage 后才置 awaiting_apply；REST 在 state=prepared
                        // 的瞬间到达时短暂等待（有界），仍不满足才按 409 拒绝。
                        let deadline = Instant::now() + Duration::from_secs(5);
                        loop {
                            if Instant::now() >= deadline {
                                return Err(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY));
                            }
                            thread::sleep(Duration::from_millis(2));
                            let Some(next) = self.manager.snapshot() else {
                                return Err(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY));
                            };
                            if next.state.is_terminal() {
                                if next.applied_version.is_some() {
                                    self.record(&next);
                                    return self
                                        .jobs
                                        .get(job_id)
                                        .cloned()
                                        .ok_or(CalPortError::NotFound);
                                }
                                return Err(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY));
                            }
                            if next.awaiting_apply
                                || next.applied_by_job
                                || next.applied_version.is_some()
                            {
                                break;
                            }
                            if next.state.active_stage() != Some(JobStage::Prepared) {
                                return Err(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY));
                            }
                        }
                    }
                }
                self.manager.apply().map_err(map_job_error)?;
                let terminal = self.wait_terminal()?;
                self.record(&terminal);
                let view = self
                    .jobs
                    .get(job_id)
                    .cloned()
                    .ok_or(CalPortError::NotFound)?;
                // K14：apply 回执必须带 applied_version（T66 实际生效证据）；缺失即 503，不假 ack。
                if view.applied_version.is_none() {
                    return Err(CalPortError::Unavailable(
                        "calibration apply finished without applied_version (T66 ack missing); refusing to fake success"
                            .to_string(),
                    ));
                }
                Ok(view)
            }
            Some(_) => Err(CalPortError::NotFound),
            None => match self.jobs.get(job_id) {
                Some(view) if view.applied_version.is_some() && view.is_terminal() => {
                    Ok(view.clone())
                }
                Some(_) => Err(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY)),
                None => Err(CalPortError::NotFound),
            },
        }
    }

    fn retest(&mut self, job_id: &str) -> Result<CalJobView, CalPortError> {
        self.refresh();
        let view = self
            .jobs
            .get(job_id)
            .cloned()
            .ok_or(CalPortError::NotFound)?;
        let expected = view
            .applied_version
            .clone()
            .ok_or(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY))?;
        if !view.is_terminal() {
            return Err(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY));
        }
        self.start_job(
            JobKind::Retest,
            "rest-retest",
            false,
            Some(expected),
            self.audio_authorized,
        )
    }

    fn rollback(&mut self, job_id: &str) -> Result<CalJobView, CalPortError> {
        self.refresh();
        if self.current_job_id().as_deref() == Some(job_id) {
            let snapshot = self
                .manager
                .snapshot()
                .ok_or_else(|| CalPortError::Unavailable("job snapshot disappeared".to_string()))?;
            if snapshot.state.is_terminal() {
                return self.rollback_finished(job_id);
            }
            self.manager.rollback().map_err(map_job_error)?;
            let terminal = self.wait_terminal()?;
            self.record(&terminal);
            return self.jobs.get(job_id).cloned().ok_or(CalPortError::NotFound);
        }
        match self.jobs.get(job_id) {
            Some(view) if view.is_terminal() => self.rollback_finished(job_id),
            Some(_) => Err(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY)),
            None => Err(CalPortError::NotFound),
        }
    }

    fn engine_info(&self) -> CalEngineInfo {
        self.offline_engine_info()
    }
}

impl std::fmt::Debug for JobManagerPort {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JobManagerPort")
            .field("engine", &self.engine)
            .field("backend", &self.backend)
            .field("real_audio", &self.real_audio)
            .field("jobs", &self.jobs.len())
            .finish()
    }
}

/// JobError -> CalPortError（409 冲突族 / 503 不可用；不做「看起来像成功」的降级）。
fn map_job_error(error: JobError) -> CalPortError {
    match error {
        JobError::Busy | JobError::ResumePending => {
            CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY)
        }
        JobError::NoActiveJob | JobError::InvalidState(_) => {
            CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY)
        }
        JobError::InvalidRequest(message) => {
            CalPortError::Unavailable(format!("job request rejected: {message}"))
        }
        JobError::NoPendingResume => CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY),
        other => CalPortError::Unavailable(format!("job manager: {other}")),
    }
}

/// 构造离线合成 backend：StageBackend(Fake 引擎) 负责测量段，FitWiring 负责 fit/compile/apply 段。
fn build_offline_backend(label: &str) -> Result<Box<dyn CalibrationJobBackend>, String> {
    let design = synthetic_design().map_err(|error| format!("synthetic design: {error}"))?;
    let wiring = FitWiring::new(design).map_err(|error| format!("fit wiring: {error}"))?;
    let config = offline_stage_config(label);
    let backend = StageBackend::from_config(config)
        .map_err(|error| format!("stage backend: {error}"))?
        .with_delegate(Box::new(wiring));
    Ok(Box::new(backend))
}

/// 离线合成测量配置（Fake 引擎；不打开声卡/不出声）。
fn offline_stage_config(label: &str) -> StageBackendConfig {
    let graph_id = sha256_hex(b"khaen-rest-calibport/graph-v1");
    let mic_cal = sha256_hex(b"khaen-rest-calibport/synthetic-umik-cal");
    let mut config = StageBackendConfig::default();
    config.mode = StageMeasurementMode::AcousticUmik;
    config.engine = EngineKind::Fake;
    config.devices = StageDeviceConfig {
        playback_device: "hw:0,0".to_string(),
        playback_channels: 2,
        capture_device: "hw:1,0".to_string(),
        capture_channels: 1,
        capture_channel: None,
        sample_rate: 48_000,
        block_frames: 128,
        buffer_frames: 512,
        settle_ms: 10,
        sample_format: S24_3LE_INTERLEAVED,
    };
    config.mic = Some(StageBackendConfig::umik1_declaration(
        "hw:1,0",
        "SIM-REST-0001",
        "synthetic-umik-cal.txt",
        mic_cal,
        "90deg",
    ));
    config.level = StageLevelConfig::default();
    config.sweep = StageSweepConfig::smoke();
    config.outputs = vec![SweepOutput::new(0, "FL", 0), SweepOutput::new(1, "FR", 1)];
    config.session_label = label.to_string();
    config.graph_id = graph_id;
    config.epoch_id = 1;
    config
}

/// 测试/daemon 用：临时 artifacts 根（进程内唯一）。
///
/// 落在 workspace 的 target/tmp 下（不用 std::env::temp_dir：本机 TEMP 指向内存盘 R:，
/// 该卷对进程创建临时目录会返回 access denied，见 AGENTS.md §1.1/§8；测试路径必须在 C:）。
pub fn unique_offline_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let base = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("tmp")
        .join("t226-rest-calibport");
    let _ = std::fs::create_dir_all(&base);
    base.join(format!("{}-{nanos}-{tag}", std::process::id()))
}

/// 测试/daemon 用：删除离线 artifacts 根（尽力而为）。
pub fn cleanup_offline_root(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(tag: &str) -> (JobManagerPort, PathBuf) {
        let root = unique_offline_root(tag);
        let port = JobManagerPort::synthetic_offline(root.clone(), None)
            .unwrap_or_else(|error| panic!("build port: {error}"));
        (port, root)
    }

    fn wait_for_state(port: &mut JobManagerPort, job_id: &str, wanted: &[&str]) -> CalJobView {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            port.refresh();
            if let Some(view) = port.jobs.get(job_id).cloned() {
                if wanted.contains(&view.state.as_str()) {
                    return view;
                }
                if CAL_JOB_TERMINAL_STATES.contains(&view.state.as_str()) {
                    panic!(
                        "job {job_id} reached terminal {} before {:?}",
                        view.state, wanted
                    );
                }
            }
            if Instant::now() >= deadline {
                panic!("timeout waiting for job {job_id} state {wanted:?}");
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn start_full(port: &mut JobManagerPort) -> String {
        let view = port
            .start(&CalStartRequest {
                kind: "full".to_string(),
                label: Some("t226-unit-full".to_string()),
                auto_apply: false,
                audible_sweep_authorized: false,
                stage: None,
                f_hi: None,
                supports: Vec::new(),
                target: None,
                settings: None,
                lanes: Vec::new(),
                reference_lane: None,
                target_dbfs: None,
            })
            .expect("start full job");
        assert_eq!(view.kind, "full");
        assert!(
            view.last_good_version.is_some(),
            "offline last-good 必须可见"
        );
        view.job_id
    }

    #[test]
    fn t226_port_engine_info_is_explicitly_synthetic() {
        let (port, root) = port("info");
        let info = port.engine_info();
        assert_eq!(info.engine, CAL_PORT_ENGINE_SYNTHETIC);
        assert!(info.synthetic, "offline job manager port must be synthetic");
        assert!(!info.real_audio);
        assert!(info.backend.contains("stage-fake"));
        cleanup_offline_root(&root);
    }

    #[test]
    fn t226_port_full_job_progress_prepared_apply_complete() {
        let (mut port, root) = port("full");
        let job_id = start_full(&mut port);
        let prepared = wait_for_state(&mut port, &job_id, &["prepared"]);
        assert_eq!(prepared.stage.as_deref(), Some("prepared"));
        assert!(prepared.progress > 0.0 && prepared.progress <= 1.0);
        assert!(
            prepared.applied_version.is_none(),
            "prepared 不得带 applied_version"
        );
        let applied = port.apply(&job_id).expect("apply prepared job");
        assert_eq!(applied.state, "complete");
        assert!(applied
            .applied_version
            .as_deref()
            .map(|value| !value.is_empty())
            .unwrap_or(false));
        // 幂等：终态 job 重复 apply 返回同一 view。
        let again = port.apply(&job_id).expect("idempotent apply");
        assert_eq!(again.applied_version, applied.applied_version);
        let listed = port.list();
        assert!(listed
            .iter()
            .any(|view| view.job_id == job_id && view.state == "complete"));
        cleanup_offline_root(&root);
    }

    #[test]
    fn t226_port_cancel_is_cooperative_and_terminal() {
        let (mut port, root) = port("cancel");
        let job_id = start_full(&mut port);
        let cancelled = port.cancel(&job_id).expect("cancel active job");
        assert_eq!(cancelled.state, "cancelled");
        assert!(cancelled.is_terminal());
        assert!(cancelled.failure.is_some(), "cancel 终态必须有原因");
        assert_eq!(
            port.cancel(&job_id),
            Err(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY))
        );
        assert_eq!(port.cancel("job-9999"), Err(CalPortError::NotFound));
        cleanup_offline_root(&root);
    }

    #[test]
    fn t226_port_retest_and_rollback_semantics() {
        let (mut port, root) = port("retest");
        let job_id = start_full(&mut port);
        wait_for_state(&mut port, &job_id, &["prepared"]);
        // 未 applied 的 job 不能 retest（409）。
        assert_eq!(
            port.retest(&job_id),
            Err(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY))
        );
        port.apply(&job_id).expect("apply job");
        let retest = port.retest(&job_id).expect("retest applied job");
        assert_eq!(retest.kind, "retest");
        assert_ne!(retest.job_id, job_id, "retest 必须产生新 job");
        // 已终态 full job rollback -> rolled_back + last_good_version（T42 回执要求）。
        let rolled = port.rollback(&job_id).expect("rollback finished job");
        assert_eq!(rolled.state, "rolled_back");
        assert!(rolled.last_good_version.is_some());
        assert!(rolled.applied_version.is_none());
        assert_eq!(port.rollback("job-9999"), Err(CalPortError::NotFound));
        cleanup_offline_root(&root);
    }

    #[test]
    fn t226_port_rejects_apply_before_prepared_and_double_start() {
        let (mut port, root) = port("states");
        let job_id = start_full(&mut port);
        match port.apply(&job_id) {
            Err(CalPortError::Rejected(status)) => {
                assert_eq!(status, RtStatus::ERR_SWITCH_BUSY);
            }
            Ok(view) => panic!("apply before prepared must fail, got {:?}", view.state),
            Err(other) => panic!("unexpected apply error: {other:?}"),
        }
        match port.start(&CalStartRequest {
            kind: "full".to_string(),
            label: None,
            auto_apply: false,
            audible_sweep_authorized: false,
            stage: None,
            f_hi: None,
            supports: Vec::new(),
            target: None,
            settings: None,
            lanes: Vec::new(),
            reference_lane: None,
            target_dbfs: None,
        }) {
            Err(CalPortError::Rejected(status)) => {
                assert_eq!(status, RtStatus::ERR_SWITCH_BUSY);
            }
            Ok(view) => panic!("double start must be busy, got {}", view.job_id),
            Err(other) => panic!("unexpected start error: {other:?}"),
        }
        assert_eq!(port.get("job-9999"), None);
        port.cancel(&job_id).expect("cleanup cancel");
        cleanup_offline_root(&root);
    }

    // -----------------------------------------------------------------------
    // REST 层端到端（T42 handle + 本 port；token 鉴权；不启 HTTP 服务）
    // -----------------------------------------------------------------------

    fn rest_service(port: JobManagerPort) -> RestService {
        RestService::with_config(Box::new(MemoryEngine::new()), RestConfig::for_tests())
            .with_calibration(Box::new(port))
    }

    fn token_request(method: Method, path: &str) -> RestRequest {
        RestRequest::new(method, path).header(TOKEN_HEADER, "test-token-0123456789abcdef")
    }

    fn token_post(path: &str, body: &str) -> RestRequest {
        token_request(Method::Post, path).json_body(body)
    }

    fn body_json(response: &RestResponse) -> JsonValue {
        JsonValue::parse(&response.body).expect("JSON body")
    }

    fn field_str(value: &JsonValue, key: &str) -> String {
        match value.get(key) {
            Some(JsonValue::Str(text)) => text.clone(),
            other => panic!("field {key}: {other:?}"),
        }
    }

    #[test]
    fn t226_rest_calibration_endpoints_use_real_jobmanager_and_mark_synthetic() {
        let (port, root) = port("rest");
        let service = rest_service(port);
        let start = service.handle(&token_post(
            "/api/calibration/jobs",
            "{\"kind\":\"full\",\"label\":\"t226-rest-full\"}",
        ));
        assert_eq!(start.status, 201, "body={}", start.body);
        let start_body = body_json(&start);
        assert!(matches!(
            start_body.get("synthetic"),
            Some(JsonValue::Bool(true))
        ));
        assert_eq!(field_str(&start_body, "engine"), CAL_PORT_ENGINE_SYNTHETIC);
        assert!(matches!(
            start_body.get("real_audio"),
            Some(JsonValue::Bool(false))
        ));
        let job_id = field_str(&start_body, "job_id");

        let deadline = Instant::now() + Duration::from_secs(60);
        let prepared_body = loop {
            let response = service.handle(&token_request(
                Method::Get,
                &format!("/api/calibration/jobs/{job_id}"),
            ));
            assert_eq!(response.status, 200, "body={}", response.body);
            let body = body_json(&response);
            let state = field_str(&body, "state");
            if state == "prepared" {
                break body;
            }
            assert!(
                !CAL_JOB_TERMINAL_STATES.contains(&state.as_str()),
                "job {job_id} terminal early: {}",
                response.body
            );
            assert!(
                Instant::now() < deadline,
                "timeout waiting prepared: {state}"
            );
            thread::sleep(Duration::from_millis(2));
        };
        let job_view = prepared_body.get("job").expect("job object");
        assert_eq!(field_str(job_view, "kind"), "full");
        assert!(matches!(
            prepared_body.get("synthetic"),
            Some(JsonValue::Bool(true))
        ));

        let apply = service.handle(&token_post(
            &format!("/api/calibration/jobs/{job_id}/apply"),
            "{}",
        ));
        assert_eq!(apply.status, 200, "body={}", apply.body);
        let apply_body = body_json(&apply);
        let applied_version = field_str(&apply_body, "applied_version");
        assert!(!applied_version.is_empty());
        assert!(matches!(
            apply_body.get("synthetic"),
            Some(JsonValue::Bool(true))
        ));

        let retest = service.handle(&token_post(
            &format!("/api/calibration/jobs/{job_id}/retest"),
            "{}",
        ));
        assert_eq!(retest.status, 201, "body={}", retest.body);
        let retest_body = body_json(&retest);
        let retest_job = retest_body.get("job").expect("retest job object");
        assert_eq!(field_str(retest_job, "kind"), "retest");
        assert_ne!(field_str(&retest_body, "job_id"), job_id);

        let rollback = service.handle(&token_post(
            &format!("/api/calibration/jobs/{job_id}/rollback"),
            "{}",
        ));
        assert_eq!(rollback.status, 200, "body={}", rollback.body);
        let rollback_body = body_json(&rollback);
        assert!(matches!(
            rollback_body.get("rolled_back"),
            Some(JsonValue::Bool(true))
        ));
        assert!(!field_str(&rollback_body, "last_good_version").is_empty());

        let list = service.handle(&token_request(Method::Get, "/api/calibration/jobs"));
        assert_eq!(list.status, 200, "body={}", list.body);
        let list_body = body_json(&list);
        assert!(matches!(
            list_body.get("synthetic"),
            Some(JsonValue::Bool(true))
        ));
        assert_eq!(field_str(&list_body, "engine"), CAL_PORT_ENGINE_SYNTHETIC);
        match list_body.get("count") {
            Some(JsonValue::Num(count)) => assert!(*count >= 2.0),
            other => panic!("count field: {other:?}"),
        }
        assert!(
            !list.body.contains("\"actual\":true"),
            "offline job manager must never claim actual: {}",
            list.body
        );
        cleanup_offline_root(&root);
    }

    #[test]
    fn t226_rest_calibration_not_wired_is_503() {
        let service =
            RestService::with_config(Box::new(MemoryEngine::new()), RestConfig::for_tests());
        for path in ["/api/calibration/jobs", "/api/calibration/jobs/job-0001"] {
            let response = service.handle(&token_request(Method::Get, path));
            assert_eq!(response.status, 503, "path={path} body={}", response.body);
            let body = body_json(&response);
            let error = body.get("error").expect("error object");
            assert_eq!(field_str(error, "code"), "service_unavailable");
        }
    }
}
