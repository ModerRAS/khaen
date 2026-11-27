//! 校准 REST 层（T36）：jobs / uploads / artifacts / wizard。
//!
//! # 范围
//!
//! 本模块做 **REST + 存储 + 向导状态 + 生产 job 后端接线**：
//!
//! - [`service`]：服务抽象（`CalibrationService` + `JobSink`）与稳定错误；
//! - [`backend`]：生产实现（khaen-calibration JobManager + 离线 OfflineJobBackend +
//!   daemon 侧 job 审计索引；main 注入）；
//! - [`rt_host_backend`]：真实测量 job backend（C3b1：daemon 经 host link 驱动 rt-host 的
//!   `cal_enter/cal_measure/cal_status/cal_leave`，只落真实测量证据，不伪造 PASS）；
//! - [`not_wired`]：未接线/初始化失败时的诚实降级（503 `not_wired` / 空列表 + reason）；
//! - [`store`]：上传文件存储（内容寻址 + 原子写 + 0600）、产物白名单与 Content-Type；
//! - [`mic`]：麦克风 profile（真实校准桥接 C1）：UMIK txt 校验 + 内容寻址存储 +
//!   `calibration/mic.json`（0600 原子写）；rt-host 未接线时状态接口如实降级、不造假；
//! - [`wizard`]：`wizard.json` 持久化、PUT 校验、向导状态推导；
//! - [`api`]：axum 路由与 handler（登录会话 + 写操作 CSRF 由现有 auth 中间件统一守）。
//!
//! # 接线
//!
//! [`DaemonState`] 持有一个 [`CalibrationHub`]：默认 `NotWiredCalibration`；测试/未来生产
//! 用 `DaemonState::with_calibration` 注入实现，daemon 会把 [`Telemetry`] 作为 `JobSink`
//! 注入给服务。`/api/status` 的 calibration 三字段由 `hub.status()` 提供（服务状态与事件
//! 缓存合并，缺失一律 null + reason）。
//!
//! # 端点（全部在 `/api` 下，要求登录会话；写操作要求 `X-KHAEN-CSRF`）
//!
//! - `POST /api/calibration/uploads` 原始字节 → 存 `<state>/uploads/<sha256>`
//! - `GET|POST /api/calibration/jobs` 列表/创建（202）
//! - `GET /api/calibration/jobs/{id}` 状态机 + 步骤 + 产物 + verdict
//! - `POST /api/calibration/jobs/{id}/cancel|apply|rollback`
//! - `GET /api/calibration/jobs/{id}/artifacts/{name}` 白名单产物（按 manifest sha256 校验）
//! - `GET|PUT /api/calibration/wizard`、`GET .../wizard/state`、`POST .../wizard/apply`
//!
//! [`DaemonState`]: crate::DaemonState

pub mod api;
pub mod backend;
pub mod mic;
pub mod not_wired;
pub mod rt_host_backend;
pub mod service;
pub mod store;
pub mod wizard;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use mic::MicProfileStore;
use service::{CalibrationStatus, JobEvent, JobSink};
use store::UploadStore;
use wizard::WizardStore;

pub use api::routes;
pub use not_wired::{NotWiredCalibration, NOT_WIRED_REASON};
pub use service::CalibrationService;

/// 校准相关共享状态：服务实例 + 事件缓存 + 上传/向导存储。
pub struct CalibrationHub {
    service: Arc<dyn CalibrationService>,
    telemetry: Arc<Telemetry>,
    state_dir: PathBuf,
    uploads: UploadStore,
    wizard: WizardStore,
    /// 麦克风 profile 存储（`calibration/mic.json` + `calibration/mics/<sha256>.txt`）。
    mic: MicProfileStore,
    /// apply/rollback 两阶段草稿（REST prepare 签发，commit/abort 消费；见 [`ApplyStaging`]）。
    staging: ApplyStaging,
}

impl std::fmt::Debug for CalibrationHub {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CalibrationHub")
            .field("service", &"<calibration service>")
            .field("state_dir", &self.state_dir)
            .field("uploads", &self.uploads.root())
            .field("wizard", &self.wizard.path())
            .field("mic", &self.mic.root())
            .field("staged_ops", &self.staging.len())
            .finish()
    }
}

impl CalibrationHub {
    /// 默认（未接线）hub：服务为 [`NotWiredCalibration`]，存储落在 `state_dir`。
    pub fn new(state_dir: PathBuf) -> Self {
        let telemetry = Arc::new(Telemetry::new());
        let service = Arc::new(NotWiredCalibration::new());
        service.attach_sink(Arc::clone(&telemetry) as Arc<dyn JobSink>);
        Self {
            service,
            telemetry,
            uploads: UploadStore::new(&state_dir),
            wizard: WizardStore::new(&state_dir),
            mic: MicProfileStore::new(&state_dir),
            staging: ApplyStaging::new(),
            state_dir,
        }
    }

    /// 注入真实/测试服务（替换默认 NotWired；把事件汇接给服务）。
    pub fn with_service(mut self, service: Arc<dyn CalibrationService>) -> Self {
        service.attach_sink(Arc::clone(&self.telemetry) as Arc<dyn JobSink>);
        self.service = service;
        self
    }

    /// 当前服务。
    pub fn service(&self) -> &Arc<dyn CalibrationService> {
        &self.service
    }

    /// 上传存储。
    pub fn uploads(&self) -> &UploadStore {
        &self.uploads
    }

    /// 向导配置存储。
    pub fn wizard(&self) -> &WizardStore {
        &self.wizard
    }

    /// 麦克风 profile 存储（GET/PUT /api/calibration/mic 与 status 的来源）。
    pub fn mic(&self) -> &MicProfileStore {
        &self.mic
    }

    /// 状态目录（auth.json / uploads / wizard.json 同源）。
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// apply/rollback 两阶段草稿表。
    pub fn staging(&self) -> &ApplyStaging {
        &self.staging
    }

    /// 事件缓存快照（/api/status 与 wizard/state 的兜底来源）。
    pub fn telemetry(&self) -> TelemetrySnapshot {
        self.telemetry.snapshot()
    }

    /// /api/status 的 calibration 三字段来源：服务状态 + 进程内事件缓存合并。
    ///
    /// - 服务状态可用：以它为准（可跨进程重启恢复）；
    /// - 进程内有更新的 job 事件：事件值优先（同进程内更实时）；
    /// - 服务状态不可用：用事件缓存兜底并如实带降级原因，绝不 500。
    pub fn status(&self) -> CalibrationStatus {
        let mut status = match self.service.status() {
            Ok(status) => status,
            Err(error) => CalibrationStatus {
                wired: false,
                engine: None,
                active_job: None,
                last_job_id: None,
                last_verdict: None,
                reason: Some(format!(
                    "calibration service status is unavailable: {}",
                    error.message()
                )),
            },
        };
        let snapshot = self.telemetry.snapshot();
        if snapshot.has_events {
            status.active_job = snapshot.active_job.clone();
            if snapshot.last_job_id.is_some() {
                status.last_job_id = snapshot.last_job_id.clone();
            }
            if snapshot.last_verdict.is_some() {
                status.last_verdict = snapshot.last_verdict.clone();
            }
        }
        status
    }
}

// ---------------------------------------------------------------------------
// apply/rollback 两阶段草稿
// ---------------------------------------------------------------------------

/// 两阶段草稿的类型（apply 或 rollback；两者共用 token 表但不可混用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagedControlKind {
    /// job 产物方案 -> 控制面 plan 草稿。
    Apply,
    /// 回滚到 job apply 前的版本 -> 控制面 rollback 草稿。
    Rollback,
}

impl StagedControlKind {
    /// REST 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Apply => "apply",
            Self::Rollback => "rollback",
        }
    }
}

/// 一份已签发但未消费的控制面草稿。
///
/// token 就是控制面自己的 engine_token（process 目标是进程内控制线程的 token，rt-host 目标是
/// rt-host 的 token）：commit/abort 时原样转回同一个控制面，绝不重新 prepare 一个"看起来一样"
/// 的草稿。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedControlOp {
    /// 签发草稿的 job id。
    pub job_id: String,
    /// apply / rollback。
    pub kind: StagedControlKind,
    /// 控制面 engine_token（一次性）。
    pub control_token: String,
    /// apply 的目标方案 sha256（rollback 为 None）。
    pub plan_sha256: Option<String>,
    /// rollback 的目标版本（apply 为 None）。
    pub target_version: Option<u64>,
    /// prepare 时的运行版本（apply 的回滚目标 / rollback 的快照）。
    pub previous_version: Option<u64>,
    /// 签发时刻（RFC3339 UTC）。
    pub created_at: String,
}

/// 两阶段草稿表：token -> op；取走即消费（一次性），并发线程安全。
#[derive(Debug, Default)]
pub struct ApplyStaging {
    staged: Mutex<BTreeMap<String, StagedControlOp>>,
}

impl ApplyStaging {
    /// 空表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一份草稿（token 唯一；重复 token 覆盖旧值，避免控制面 token 复用造成串草稿）。
    pub fn stage(&self, op: StagedControlOp) {
        let token = op.control_token.clone();
        self.lock().insert(token, op);
    }

    /// 一次性取走：token 存在、job id 相同且类型相同才消费；否则返回 None（不消费别人的草稿）。
    pub fn take(
        &self,
        token: &str,
        job_id: &str,
        kind: StagedControlKind,
    ) -> Option<StagedControlOp> {
        let mut staged = self.lock();
        let matches = staged
            .get(token)
            .map(|op| op.job_id == job_id && op.kind == kind)
            .unwrap_or(false);
        if matches {
            staged.remove(token)
        } else {
            None
        }
    }

    /// 显式丢弃一份草稿（abort 成功后调用）。
    pub fn remove(&self, token: &str) -> Option<StagedControlOp> {
        self.lock().remove(token)
    }

    /// 当前未消费草稿数（诊断/测试）。
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, StagedControlOp>> {
        self.staged
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 进程内 job 事件缓存（JobSink 的 daemon 侧实现）。
#[derive(Debug)]
pub struct Telemetry {
    snapshot: Mutex<TelemetrySnapshot>,
}

/// 事件缓存快照。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TelemetrySnapshot {
    /// 进程启动以来是否收到过任何 job 事件。
    pub has_events: bool,
    /// 当前 active job id（终态事件会清掉）。
    pub active_job: Option<String>,
    /// 最近一个 job id。
    pub last_job_id: Option<String>,
    /// 最近一个 verdict。
    pub last_verdict: Option<String>,
    /// 最近事件时刻（RFC3339 UTC；服务给不出时钟则为 None）。
    pub last_event_at: Option<String>,
}

impl Telemetry {
    /// 新建空缓存。
    pub fn new() -> Self {
        Self {
            snapshot: Mutex::new(TelemetrySnapshot::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TelemetrySnapshot> {
        self.snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 当前快照。
    pub fn snapshot(&self) -> TelemetrySnapshot {
        self.lock().clone()
    }

    /// 记录一个 job 事件。
    pub fn record(&self, event: JobEvent) {
        let mut snapshot = self.lock();
        snapshot.has_events = true;
        if event.state.is_terminal() {
            if snapshot.active_job.as_deref() == Some(event.job_id.as_str()) {
                snapshot.active_job = None;
            }
            snapshot.last_job_id = Some(event.job_id);
            if event.verdict.is_some() {
                snapshot.last_verdict = event.verdict;
            }
        } else {
            snapshot.active_job = Some(event.job_id);
        }
        if event.at.is_some() {
            snapshot.last_event_at = event.at;
        }
    }
}

impl Default for Telemetry {
    fn default() -> Self {
        Self::new()
    }
}

impl JobSink for Telemetry {
    fn job_event(&self, event: JobEvent) {
        self.record(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use service::{code, JobState};

    #[test]
    fn staging_tokens_are_single_use_and_bound_to_their_job() {
        let staging = ApplyStaging::new();
        assert!(staging.is_empty());
        staging.stage(StagedControlOp {
            job_id: "job-0001".to_string(),
            kind: StagedControlKind::Apply,
            control_token: "tok-1".to_string(),
            plan_sha256: Some("a".repeat(64)),
            target_version: None,
            previous_version: Some(1),
            created_at: "2026-11-26T00:00:00Z".to_string(),
        });
        assert_eq!(staging.len(), 1);
        // 错的 job id / 错的 kind 不能消费别人的草稿（且不删除）。
        assert!(staging
            .take("tok-1", "job-0002", StagedControlKind::Apply)
            .is_none());
        assert!(staging
            .take("tok-1", "job-0001", StagedControlKind::Rollback)
            .is_none());
        assert_eq!(staging.len(), 1);
        // 正确的绑定：一次性消费。
        let op = staging
            .take("tok-1", "job-0001", StagedControlKind::Apply)
            .expect("token is consumed");
        assert_eq!(op.plan_sha256, Some("a".repeat(64)));
        assert!(staging
            .take("tok-1", "job-0001", StagedControlKind::Apply)
            .is_none());
        assert!(staging.is_empty());
        // abort 路径：显式 remove 幂等。
        staging.stage(StagedControlOp {
            job_id: "job-0002".to_string(),
            kind: StagedControlKind::Rollback,
            control_token: "tok-2".to_string(),
            plan_sha256: None,
            target_version: Some(1),
            previous_version: Some(2),
            created_at: "2026-11-26T00:00:01Z".to_string(),
        });
        assert!(staging.remove("tok-2").is_some());
        assert!(staging.remove("tok-2").is_none());
    }

    #[test]
    fn default_hub_is_unwired_but_uploads_and_wizard_work() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("khaen-cal-hub-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let hub = CalibrationHub::new(dir.clone());
        let status = hub.status();
        assert!(!status.wired);
        assert!(status.reason.is_some());
        assert!(status.active_job.is_none());
        assert!(hub.uploads().save(b"bytes").is_ok());
        assert!(!hub.wizard().load().saved);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn telemetry_tracks_active_then_last_job() {
        let telemetry = Telemetry::new();
        telemetry.record(JobEvent {
            job_id: "job-0001".into(),
            state: JobState::Running,
            verdict: None,
            at: Some("2026-01-01T00:00:01Z".into()),
        });
        let snapshot = telemetry.snapshot();
        assert!(snapshot.has_events);
        assert_eq!(snapshot.active_job.as_deref(), Some("job-0001"));
        assert!(snapshot.last_job_id.is_none());
        telemetry.record(JobEvent {
            job_id: "job-0001".into(),
            state: JobState::Succeeded,
            verdict: Some("PASS".into()),
            at: Some("2026-01-01T00:00:02Z".into()),
        });
        let snapshot = telemetry.snapshot();
        assert!(snapshot.active_job.is_none());
        assert_eq!(snapshot.last_job_id.as_deref(), Some("job-0001"));
        assert_eq!(snapshot.last_verdict.as_deref(), Some("PASS"));
        assert_eq!(
            snapshot.last_event_at.as_deref(),
            Some("2026-01-01T00:00:02Z")
        );
    }

    #[test]
    fn not_wired_reason_is_stable_and_documented() {
        assert!(!NOT_WIRED_REASON.is_empty());
        assert!(NOT_WIRED_REASON.contains("not wired"));
        assert_eq!(code::NOT_WIRED, "not_wired");
    }
}
