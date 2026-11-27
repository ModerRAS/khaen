//! 校准服务抽象（T36）：REST 层与 job 后端之间的唯一接口。
//!
//! # 为什么是 trait
//!
//! daemon 只负责 REST/存储/向导状态，真正的 job 执行（compile → levels → closed_loop →
//! bundle、设备交接、产物落盘）属于校准后端（T70h-b，crates/calibration 的生产 backend）。
//! 本模块用 [`CalibrationService`] 把两者隔开：
//!
//! - 生产默认实现是 [`NotWiredCalibration`]：后端未接线时如实返回 503 `not_wired`，
//!   不冒充成功（AGENTS.md §5：能力上报必须真实）；
//! - 测试注入 fake 实现：REST 层全部路径（创建/列表/详情/取消/应用/回滚/产物/状态集成）
//!   都能用真实的 axum + TCP 端到端验证，不需要等后端；
//! - 后端落地后，把 fake 替换成适配 khaen_calibration::job::JobManager 的实现即可，
//!   REST 契约与测试不变（本 crate 生产代码编译期**不依赖**该校准后端 API）。
//!
//! # JobSink
//!
//! 服务通过 [`JobSink`] 把 job 生命周期事件推给 daemon；daemon 的 CalibrationHub 在事件
//! 缓存与 status() 之间合并出 /api/status 的 calibration 三字段
//! （active_job/last_job_id/last_verdict）。后端未接线时 sink 不会被调用，字段保持
//! null + reason。
//!
//! [`NotWiredCalibration`]: crate::calibration::not_wired::NotWiredCalibration

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

// ---------------------------------------------------------------------------
// 稳定错误码
// ---------------------------------------------------------------------------

/// REST 错误码（稳定；前端按 code 映射文案，见 task-37 契约）。
///
/// 全部错误体是 PLAN §4.1 的固定形状：
/// `{"error":{"code","message","details":{}}}`。
pub mod code {
    /// 校准后端未接线（503）。写操作一律如实失败；GET 列表返回空列表 + reason。
    pub const NOT_WIRED: &str = "not_wired";
    /// 同一时刻只允许一个 active job（409）。
    pub const SWITCH_BUSY: &str = "switch_busy";
    /// job id 不存在（404）。
    pub const JOB_NOT_FOUND: &str = "job_not_found";
    /// 引用的上传不存在（404）。
    pub const UPLOAD_NOT_FOUND: &str = "upload_not_found";
    /// 请求体/字段非法（400）：JSON 形状、类型、枚举、范围、未知字段。
    pub const INVALID_REQUEST: &str = "invalid_request";
    /// upload id 格式非法（不是 64 位小写十六进制；400），含路径穿越。
    pub const INVALID_UPLOAD: &str = "invalid_upload";
    /// 上传/下载的媒体类型不支持（415）。
    pub const UNSUPPORTED_FORMAT: &str = "unsupported_format";
    /// 上传超过大小上限（413）。
    pub const UPLOAD_TOO_LARGE: &str = "upload_too_large";
    /// 应用/回滚的 job 还没成功（409）。
    pub const JOB_NOT_SUCCEEDED: &str = "job_not_succeeded";
    /// job 没有可应用的方案产物（409）。
    pub const PLAN_UNAVAILABLE: &str = "plan_unavailable";
    /// 产物不存在或不在白名单（404）。
    pub const ARTIFACT_NOT_FOUND: &str = "artifact_not_found";
    /// 产物字节与 manifest 记录的 sha256 不一致（409）。
    pub const ARTIFACT_SHA_MISMATCH: &str = "artifact_sha_mismatch";
    /// 向导配置还没保存（409）。
    pub const WIZARD_NOT_SAVED: &str = "wizard_not_saved";
    /// job 应用前没有可回滚的旧版本（409）。
    pub const ROLLBACK_UNAVAILABLE: &str = "rollback_unavailable";
    /// 向导应用时没有可应用的 job（409）。
    pub const NO_APPLICABLE_JOB: &str = "no_applicable_job";
    /// 真实测量 job 需要已保存的麦克风 profile，但当前没有（409）。
    pub const MIC_PROFILE_MISSING: &str = "mic_profile_missing";
    /// 两阶段 token 无效/已消费/不属于该 job（409）。
    pub const ENGINE_TOKEN_INVALID: &str = "engine_token_invalid";
    /// 内部错误（500）。
    pub const INTERNAL_ERROR: &str = "internal_error";
}

/// 校准 REST 错误：HTTP 状态 + 稳定码 + 人读消息 + 结构化 details。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalibrationError {
    status: StatusCode,
    code: &'static str,
    message: String,
    details: Map<String, Value>,
}

impl CalibrationError {
    /// 构造。
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            details: Map::new(),
        }
    }

    /// 附加结构化字段（链式；重复 key 覆盖）。
    pub fn with_detail(mut self, key: &str, value: Value) -> Self {
        self.details.insert(key.to_string(), value);
        self
    }

    /// 后端未接线（503）。
    pub fn not_wired(reason: &str) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            code::NOT_WIRED,
            format!("calibration backend is not wired: {reason}"),
        )
    }

    /// 请求字段非法（400）。
    pub fn invalid_request(field: &str, reason: impl Into<String>) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            code::INVALID_REQUEST,
            reason.into(),
        )
        .with_detail("field", json!(field))
    }

    /// upload id 格式非法（400）。
    pub fn invalid_upload(reason: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code::INVALID_UPLOAD, reason.into())
    }

    /// 引用的上传不存在（404）。
    pub fn upload_not_found(upload_id: &str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            code::UPLOAD_NOT_FOUND,
            format!(
                "upload {upload_id} does not exist: upload it with POST /api/calibration/uploads first"
            ),
        )
        .with_detail("upload_id", json!(upload_id))
    }

    /// 上传媒体类型不支持（415）。
    pub fn unsupported_format(content_type: Option<&str>) -> Self {
        Self::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            code::UNSUPPORTED_FORMAT,
            "upload content-type must be application/octet-stream or audio/wav",
        )
        .with_detail("content_type", json!(content_type))
    }

    /// 上传超过大小上限（413）。
    pub fn upload_too_large(limit: u64) -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            code::UPLOAD_TOO_LARGE,
            format!("upload exceeds the {limit}-byte limit"),
        )
        .with_detail("limit_bytes", json!(limit))
    }

    /// 已有 active job（409）。
    pub fn switch_busy(active_job_id: &str) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            code::SWITCH_BUSY,
            format!(
                "another calibration job is active: {active_job_id}; wait for it to finish or cancel it"
            ),
        )
        .with_detail("active_job_id", json!(active_job_id))
    }

    /// job 不存在（404）。
    pub fn job_not_found(job_id: &str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            code::JOB_NOT_FOUND,
            format!("calibration job {job_id} does not exist"),
        )
        .with_detail("job_id", json!(job_id))
    }

    /// job 还没成功，不能 apply/rollback（409）。
    pub fn job_not_succeeded(job_id: &str, state: &str) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            code::JOB_NOT_SUCCEEDED,
            format!(
                "calibration job {job_id} is {state}: only succeeded jobs have an applicable plan"
            ),
        )
        .with_detail("job_id", json!(job_id))
        .with_detail("state", json!(state))
    }

    /// job 没有方案产物（409）。
    pub fn plan_unavailable(job_id: &str, reason: impl Into<String>) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            code::PLAN_UNAVAILABLE,
            format!(
                "calibration job {job_id} has no applicable plan: {}",
                reason.into()
            ),
        )
        .with_detail("job_id", json!(job_id))
    }

    /// 产物不存在/不在白名单（404）。
    pub fn artifact_not_found(job_id: &str, name: &str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            code::ARTIFACT_NOT_FOUND,
            format!("calibration job {job_id} has no downloadable artifact {name:?}"),
        )
        .with_detail("job_id", json!(job_id))
        .with_detail("name", json!(name))
    }

    /// 产物 sha256 与 manifest 不符（409）。
    pub fn artifact_sha_mismatch(job_id: &str, name: &str, expected: &str, actual: &str) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            code::ARTIFACT_SHA_MISMATCH,
            format!(
                "artifact {name} of job {job_id} does not match its manifest: expected sha256 {expected}, got {actual}"
            ),
        )
        .with_detail("job_id", json!(job_id))
        .with_detail("name", json!(name))
        .with_detail("expected_sha256", json!(expected))
        .with_detail("actual_sha256", json!(actual))
    }

    /// 回滚目标不可用（409）。
    pub fn rollback_unavailable(job_id: &str, reason: impl Into<String>) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            code::ROLLBACK_UNAVAILABLE,
            format!(
                "calibration job {job_id} cannot be rolled back: {}",
                reason.into()
            ),
        )
        .with_detail("job_id", json!(job_id))
    }

    /// 向导应用没有可应用的 job（409）。
    pub fn no_applicable_job(reason: impl Into<String>) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            code::NO_APPLICABLE_JOB,
            format!("no calibration job to apply: {}", reason.into()),
        )
    }

    /// 真实测量 job 缺少已保存的麦克风 profile（409）。
    ///
    /// 这里是**创建期**拒绝：没有 mic 声明就没有真实测量的几何/校准依据，不能排队一个注定
    /// 失败或会产生伪造数据的 job（C3b1 契约）。
    pub fn mic_profile_missing(reason: impl Into<String>) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            code::MIC_PROFILE_MISSING,
            format!(
                "a saved microphone profile is required before a real rt-host measurement job: {}",
                reason.into()
            ),
        )
    }

    /// 内部错误（500）。
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            code::INTERNAL_ERROR,
            message.into(),
        )
    }

    /// HTTP 状态。
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// 稳定错误码。
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// 人读消息。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 结构化 details。
    pub fn details(&self) -> &Map<String, Value> {
        &self.details
    }
}

impl std::fmt::Display for CalibrationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} ({}): {}",
            self.code,
            self.status.as_u16(),
            self.message
        )
    }
}

impl std::error::Error for CalibrationError {}

impl IntoResponse for CalibrationError {
    fn into_response(self) -> Response {
        let body = json!({
            "error": {
                "code": self.code,
                "message": self.message,
                "details": Value::Object(self.details),
            }
        });
        (self.status, Json(body)).into_response()
    }
}

// ---------------------------------------------------------------------------
// job 枚举与输入
// ---------------------------------------------------------------------------

/// 离线单点校准 job 种类（PLAN §4.7）。
pub const JOB_KIND_SINGLE_POINT_OFFLINE: &str = "single_point_offline";

/// rt-host 真实单点测量 job 种类（C3b1：daemon 驱动 rt-host 的 cal_* op）。
pub const JOB_KIND_SINGLE_POINT_RT_HOST: &str = "single_point_rt_host";

/// 离线单点 job 的步骤顺序（如实映射 khaen_calibration::job::SINGLE_POINT_OFFLINE_PLAN：
/// validate → compile → levels → closed_loop → bundle；不允许跳过步骤直接成功）。
pub const OFFLINE_JOB_STEPS: [&str; 5] = ["validate", "compile", "levels", "closed_loop", "bundle"];

/// rt-host 真实测量 job 的步骤顺序（如实映射 khaen_calibration::job::SINGLE_POINT_RT_HOST_PLAN：
/// validate -> sweep_capture -> quality_check -> bundle；本切片不包含 solve/apply）。
pub const RT_HOST_JOB_STEPS: [&str; 4] = ["validate", "sweep_capture", "quality_check", "bundle"];

/// 按 job 种类取步骤名（REST 步骤/索引重建共用；未知 kind 按离线步骤，调用方各自校验 kind）。
pub fn job_step_names(kind: &str) -> &'static [&'static str] {
    if kind == JOB_KIND_SINGLE_POINT_RT_HOST {
        &RT_HOST_JOB_STEPS
    } else {
        &OFFLINE_JOB_STEPS
    }
}

/// job 状态机（PLAN §4.7：queued → running(步骤) → succeeded|failed|cancelled）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    /// 已创建，等待执行。
    Queued,
    /// 执行中（active_step 指出当前步骤）。
    Running,
    /// 成功（步骤全部完成，verdict/产物由 backend 决定）。
    Succeeded,
    /// 失败（error 给稳定码）。
    Failed,
    /// 已取消。
    Cancelled,
}

impl JobState {
    /// REST 机器名（与前端 task-37 契约逐字一致）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// 是否终态。
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    /// 解析机器名；未知值返回 None（不静默降级）。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// 单个 job 步骤的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    /// 还没开始。
    Pending,
    /// 执行中。
    Running,
    /// 成功。
    Succeeded,
    /// 失败。
    Failed,
    /// 取消。
    Cancelled,
}

impl StepState {
    /// REST 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// 解析机器名。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// job 的 apply 生命周期（与 job 执行状态机分开：回滚不改 job 的执行结果）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyState {
    /// 还没应用。
    NotApplied,
    /// 已应用到控制面。
    Applied,
    /// 应用过、已回滚。
    RolledBack,
}

impl ApplyState {
    /// REST 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotApplied => "not_applied",
            Self::Applied => "applied",
            Self::RolledBack => "rolled_back",
        }
    }
}

/// job 输入（创建时校验并持久化；列表/详情回显同一份）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobInputs {
    /// 基准方案的 sha256（64 位十六进制）。
    pub plan_sha: String,
    /// baseline WAV 的上传 id（= 内容 sha256）。
    pub baseline_upload_id: String,
    /// applied WAV 的上传 id。
    pub applied_upload_id: String,
    /// 1/3-oct 中心频率（升序，Hz）。
    pub centers_hz: Vec<f64>,
    /// 判定容差（dB，> 0）。
    pub tolerance_db: f64,
    /// 最少可比较带数（>= 1）。
    pub min_bands: u32,
}

/// rt-host 真实测量 job 的会话输入（C3b1）。
///
/// 真实测量不读上传录音：扫频由 rt-host 在真实设备上播放/采集，daemon 只传会话参数。
/// 所有可选字段缺省时由 rt-host 自己的配置决定（daemon 不替换默认值、不猜）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RtHostJobInputs {
    /// 用户是否显式授权可闻扫频（P08；本 kind 必须是 true）。
    pub audible: bool,
    /// 要测量的 DAC 通道（None = 用 rt-host 配置默认；给了就原样转发给 cal_enter）。
    #[serde(default)]
    pub channels: Option<Vec<u32>>,
    /// 驱动电平（dBFS，[-60, -1]；None = 用 rt-host 配置默认）。
    #[serde(default)]
    pub level_dbfs: Option<f64>,
}

/// rt-host 测量的电平下限（与 crates/rt-host/src/control/cal/mod.rs 的 [-60, -1] 同源）。
pub const RT_HOST_MIN_LEVEL_DBFS: f64 = -60.0;
/// rt-host 测量的电平上限（-1 dBFS：比 JobRequest 的 0 dBFS 更保守，避免满刻度）。
pub const RT_HOST_MAX_LEVEL_DBFS: f64 = -1.0;
/// rt-host 会话最多允许的测量通道数（rt-host 运行时最多 22 个输出通道）。
pub const RT_HOST_MAX_CHANNELS: usize = 22;

/// REST 层解析后的上传引用（上传存在校验通过后才会传给服务）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadRef {
    /// 上传 id（= 内容 sha256）。
    pub upload_id: String,
    /// 内容 sha256（64 位十六进制）。
    pub sha256: String,
    /// 字节数。
    pub bytes: u64,
    /// 状态目录下的真实路径（后端读取 WAV 用）。
    pub path: PathBuf,
}

/// 新 job 请求：REST 层的 kind/inputs/上传引用（上传已经存盘）。
#[derive(Debug, Clone, PartialEq)]
pub struct NewJob {
    /// job 种类（当前只接受 single_point_offline）。
    pub kind: String,
    /// 校验后的输入。
    pub inputs: JobInputs,
    /// baseline WAV 引用。
    pub baseline: UploadRef,
    /// applied WAV 引用。
    pub applied: UploadRef,
}

// ---------------------------------------------------------------------------
// job 快照
// ---------------------------------------------------------------------------

/// 一个步骤的快照（缺失字段由 api 层补 reason，不编值）。
#[derive(Debug, Clone)]
pub struct JobStep {
    /// 步骤名（OFFLINE_JOB_STEPS 之一）。
    pub name: String,
    /// 步骤状态。
    pub state: StepState,
    /// 开始时刻（RFC3339 UTC；未开始为 None）。
    pub started_at: Option<String>,
    /// 结束时刻。
    pub finished_at: Option<String>,
    /// 步骤产物的 sha256（如 plan.bin / levels.json；未产出为 None）。
    pub sha256: Option<String>,
    /// 人读细节（如编译错误摘要）。
    pub detail: Option<String>,
}

/// 一个可下载产物的记录（来自 backend 的 artifacts manifest）。
#[derive(Debug, Clone)]
pub struct ArtifactInfo {
    /// 产物文件名（bare filename；REST 只按白名单暴露）。
    pub name: String,
    /// manifest 记录的 sha256（下载时 daemon 会重新计算并比对）。
    pub sha256: String,
    /// 字节数。
    pub bytes: u64,
    /// 媒体类型（None 时 daemon 按文件名推断）。
    pub content_type: Option<String>,
}

/// 一个频带的对比结果（result.bands）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BandResult {
    /// 中心频率（Hz）。
    pub hz: f64,
    /// 预测电平（dB；无法预测为 None）。
    pub predicted_db: Option<f64>,
    /// 实测电平（dB；缺失为 None）。
    pub observed_db: Option<f64>,
    /// 差值（dB）。
    pub delta_db: Option<f64>,
    /// 该带是否缺失（不可比较）。
    pub missing: bool,
}

/// 设计阶段的残差与准入结论（result.design）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesignResult {
    /// 残差等级（FULL|PARTIAL|NONE；backend 未给为 None）。
    pub residual: Option<String>,
    /// 准入是否通过。
    pub admission_accepted: Option<bool>,
    /// 准入拒绝码。
    pub admission_code: Option<String>,
    /// 准入理由。
    pub admission_reason: Option<String>,
}

/// 应用所需的方案产物（result.plan）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanResult {
    /// 方案文件 sha256。
    pub sha256: String,
    /// 方案 graph_id。
    pub graph_id: String,
}

/// 实测来源（离线 job：两段上传的真实录音；绝不把模拟值写进这里）。
///
/// 合同要求 `measured`/`simulated` 如实标记：本结构只描述**录音输入**，字段全部来自上传
/// 内容寻址 id（内容 sha256），daemon 不重新编造。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobMeasured {
    /// baseline 录音的上传 id（= 内容 sha256）。
    pub baseline_upload_id: String,
    /// applied 录音的上传 id。
    pub applied_upload_id: String,
    /// 录音采样率（Hz）。
    pub sample_rate_hz: u32,
    /// 数据来源（固定说明，例如 "uploaded WAV recordings (offline job)"）。
    pub source: String,
}

/// 模拟来源（离线 job：由方案预测出的带域电平；绝不把模拟值写进 `measured`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobSimulated {
    /// 预测电平的来源（例如 "plan_levels: plan.bin 的方案预测，非实测"）。
    pub predicted_from: String,
    /// 预测所用方案的 sha256。
    pub plan_sha256: String,
    /// 预测所用方案的 graph_id。
    pub graph_id: String,
}

/// job 结果（PLAN §10 第 6/7 步：predicted vs observed + verdict + 残差 + plan）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobResult {
    /// 判定（PASS|FAIL|INCOMPLETE）。
    pub verdict: Option<String>,
    /// 最大偏差（dB）。
    pub max_delta_db: Option<f64>,
    /// 参与比较的带数。
    pub compared_bands: Option<u64>,
    /// 缺失（不可比较）的带数。
    pub missing_bands: Option<u64>,
    /// 违约说明（人读）。
    pub violations: Vec<String>,
    /// 每个带的对比。
    pub bands: Vec<BandResult>,
    /// 设计/准入结论。
    pub design: Option<DesignResult>,
    /// 可应用的方案产物。
    pub plan: Option<PlanResult>,
    /// 实测来源（两段上传录音；缺失为 None，不编）。
    pub measured: Option<JobMeasured>,
    /// 模拟来源（方案预测电平；缺失为 None，不编）。
    pub simulated: Option<JobSimulated>,
}

/// job 失败详情（稳定码 + 人读）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobError {
    /// 稳定错误码（backend 提供，例如 E_CLOSED_LOOP_INPUT）。
    pub code: String,
    /// 人读消息。
    pub message: String,
}

/// job 的 apply 状态快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobApply {
    /// apply 生命周期状态。
    pub state: ApplyState,
    /// 应用后控制面生效的版本号。
    pub applied_version: Option<u64>,
    /// 应用前的版本号（回滚目标；应用前没有版本时 None）。
    pub previous_version: Option<u64>,
    /// 应用时刻（RFC3339 UTC）。
    pub applied_at: Option<String>,
    /// 应用的 plan sha256。
    pub plan_sha256: Option<String>,
}

impl Default for JobApply {
    fn default() -> Self {
        Self {
            state: ApplyState::NotApplied,
            applied_version: None,
            previous_version: None,
            applied_at: None,
            plan_sha256: None,
        }
    }
}

/// job 完整快照（详情接口与服务内部共用的数据形状）。
#[derive(Debug, Clone)]
pub struct JobRecord {
    /// job id（REST 路径参数 / 列表 id）。
    pub job_id: String,
    /// job 种类（当前只有 single_point_offline）。
    pub kind: String,
    /// 状态机。
    pub state: JobState,
    /// 当前步骤名（仅 running 时有意义）。
    pub active_step: Option<String>,
    /// 判定（PASS|FAIL|INCOMPLETE；未完成为 None）。
    pub verdict: Option<String>,
    /// 创建时刻。
    pub created_at: Option<String>,
    /// 创建时刻（unix 秒；审计字段，与 created_at 同源）。
    pub created_unix: Option<u64>,
    /// 开始时刻。
    pub started_at: Option<String>,
    /// 结束时刻。
    pub finished_at: Option<String>,
    /// 最近更新时刻（unix 秒）。
    pub updated_unix: Option<u64>,
    /// 耗时（毫秒）。
    pub duration_ms: Option<u64>,
    /// 整体进度 0.0..=1.0（backend 上报；未知为 None，不编）。
    pub progress: Option<f64>,
    /// 执行引擎（离线 job 一般 None；backend 报到什么就填什么，不编）。
    pub engine: Option<String>,
    /// 四个步骤（允许缺失的置 Pending）。
    pub steps: Vec<JobStep>,
    /// 产物清单（manifest）。
    pub artifacts: Vec<ArtifactInfo>,
    /// 输入回显。
    pub inputs: Option<JobInputs>,
    /// 结果。
    pub result: Option<JobResult>,
    /// 失败详情（未失败为 None）。
    pub error: Option<JobError>,
    /// apply 状态。
    pub apply: JobApply,
    /// backend 给出的缺失字段原因（api 层会合并自己推导的原因）。
    pub reasons: BTreeMap<String, String>,
}

impl JobRecord {
    /// 新建一个 queued job（fake/后端创建路径共用；步骤全部 Pending）。
    pub fn new_queued(
        job_id: impl Into<String>,
        new_job: &NewJob,
        created_at: Option<String>,
    ) -> Self {
        let steps = OFFLINE_JOB_STEPS
            .iter()
            .map(|name| JobStep {
                name: (*name).to_string(),
                state: StepState::Pending,
                started_at: None,
                finished_at: None,
                sha256: None,
                detail: None,
            })
            .collect();
        Self {
            job_id: job_id.into(),
            kind: new_job.kind.clone(),
            state: JobState::Queued,
            active_step: None,
            verdict: None,
            created_at,
            created_unix: None,
            started_at: None,
            finished_at: None,
            updated_unix: None,
            duration_ms: None,
            progress: None,
            engine: None,
            steps,
            artifacts: Vec::new(),
            inputs: Some(new_job.inputs.clone()),
            result: None,
            error: None,
            apply: JobApply::default(),
            reasons: BTreeMap::new(),
        }
    }
}

/// job 列表元素（分页接口）。
#[derive(Debug, Clone)]
pub struct JobSummary {
    /// job id。
    pub job_id: String,
    /// job 种类。
    pub kind: String,
    /// 状态机。
    pub state: JobState,
    /// 判定。
    pub verdict: Option<String>,
    /// 创建时刻。
    pub created_at: Option<String>,
    /// 创建时刻（unix 秒）。
    pub created_unix: Option<u64>,
    /// 开始时刻。
    pub started_at: Option<String>,
    /// 结束时刻。
    pub finished_at: Option<String>,
    /// 最近更新时刻（unix 秒）。
    pub updated_unix: Option<u64>,
    /// 整体进度 0.0..=1.0。
    pub progress: Option<f64>,
    /// 耗时（毫秒）。
    pub duration_ms: Option<u64>,
    /// 失败详情。
    pub error: Option<JobError>,
    /// 该行的补充原因（backend 提供；api 层缺失字段会补）。
    pub reason: Option<String>,
}

impl JobSummary {
    /// 从完整快照取列表所需字段。
    pub fn from_record(record: &JobRecord) -> Self {
        Self {
            job_id: record.job_id.clone(),
            kind: record.kind.clone(),
            state: record.state,
            verdict: record.verdict.clone(),
            created_at: record.created_at.clone(),
            created_unix: record.created_unix,
            started_at: record.started_at.clone(),
            finished_at: record.finished_at.clone(),
            updated_unix: record.updated_unix,
            progress: record.progress,
            duration_ms: record.duration_ms,
            error: record.error.clone(),
            reason: record.reasons.get("state").cloned(),
        }
    }
}

/// 分页参数（REST 层已校验）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobQuery {
    /// 每页条数（1..=100）。
    pub limit: usize,
    /// 偏移。
    pub offset: usize,
}

/// 分页结果。
#[derive(Debug, Clone)]
pub struct JobPage {
    /// 本页 job。
    pub jobs: Vec<JobSummary>,
    /// 总条数（不受分页影响）。
    pub total: usize,
}

/// /api/status 的 calibration 三字段来源。
#[derive(Debug, Clone, Default)]
pub struct CalibrationStatus {
    /// 后端是否已接线。
    pub wired: bool,
    /// 执行引擎（backend 报什么填什么；未接线为 None）。
    pub engine: Option<String>,
    /// 当前 active job id。
    pub active_job: Option<String>,
    /// 最近一个 job id。
    pub last_job_id: Option<String>,
    /// 最近一个 verdict（PASS|FAIL|INCOMPLETE）。
    pub last_verdict: Option<String>,
    /// 未接线/降级原因。
    pub reason: Option<String>,
}

/// apply 第一阶段：job 产出的可应用 plan（daemon 随后走 prepare/commit）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyPlan {
    /// 所属 job。
    pub job_id: String,
    /// 方案文件路径（daemon 控制面读它）。
    pub plan_path: PathBuf,
    /// 方案文件 sha256（控制面 prepare 会再校验一次）。
    pub plan_sha256: String,
}

/// rollback 第一阶段：job 应用前的版本（daemon 随后走 rollback/commit）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackTarget {
    /// 所属 job。
    pub job_id: String,
    /// 要恢复到的版本号（必须是控制面历史里的旧版本）。
    pub target_version: u64,
}

/// commit 之后回写的生效信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedVersion {
    /// 新生效版本号。
    pub version: u64,
    /// 生效后的 graph_id。
    pub graph_id: String,
    /// 生效前的运行版本（没有为 None）。
    pub previous_version: Option<u64>,
}

/// 服务推给 daemon 的 job 事件。
#[derive(Debug, Clone)]
pub struct JobEvent {
    /// job id。
    pub job_id: String,
    /// 事件时的状态。
    pub state: JobState,
    /// 判定（终态时可能带上）。
    pub verdict: Option<String>,
    /// 事件时刻（RFC3339 UTC；backend 没有时钟来源时为 None）。
    pub at: Option<String>,
}

/// job 事件汇（daemon 注入到服务；服务在 job 生命周期变化时调用）。
pub trait JobSink: Send + Sync {
    /// 一次 job 状态变化。
    fn job_event(&self, event: JobEvent);
}

// ---------------------------------------------------------------------------
// 服务 trait
// ---------------------------------------------------------------------------

/// 校准 job 服务：REST 层唯一依赖的 job 后端接口。
///
/// 实现方负责：
/// - **单 job 互斥**：start_job 在有 active job 时必须原子地返回 switch_busy
///   （同一 JobManager 内互斥，REST 层无法替后端做这件事）；
/// - **状态机真实性**：步骤只能按 OFFLINE_JOB_STEPS 前进，不允许跳步成功；
/// - **产物来源**：artifacts / read_artifact 必须来自 job 的 artifacts manifest；
///   daemon 会按 manifest 记录的 sha256 重新校验字节；
/// - **apply/rollback 两阶段**：apply/rollback 只返回控制面需要的 plan 引用/目标版本，
///   真正的 prepare→commit 由 daemon 用现有控制面完成，成功后 daemon 回调
///   apply_committed/rollback_committed 让后端记录结果（这两个方法必须幂等）。
pub trait CalibrationService: Send + Sync {
    /// 只读状态（/api/status 与向导状态用）。未接线实现返回 wired=false + reason。
    fn status(&self) -> Result<CalibrationStatus, CalibrationError>;

    /// 创建 job（202）。互斥/校验失败返回稳定错误。
    fn start_job(&self, request: &NewJob) -> Result<JobRecord, CalibrationError>;

    /// 创建 rt-host 真实测量 job（C3b1，202）。
    ///
    /// 默认实现如实返回 503 not_wired：只有接入了 rt-host 控制通道的生产后端才能执行真实
    /// 测量；未接线实现/fake 不得排队一个永远不会真跑的 job（不假成功）。
    fn start_rt_host_job(&self, _inputs: &RtHostJobInputs) -> Result<JobRecord, CalibrationError> {
        Err(CalibrationError::not_wired(
            "this calibration backend cannot run rt-host measurement jobs (kind single_point_rt_host); it is not wired to the rt-host control channel",
        ))
    }

    /// rt-host job 的会话输入回显（离线 job/未知 job 返回 None）。
    ///
    /// 存在的意义：JobInputs（离线字段）不适用于真实测量，detail 用这个字段如实回显
    /// audible/channels/level_dbfs，而不是把会话参数塞进上传录音字段里（不编造）。
    fn rt_host_inputs(&self, _job_id: &str) -> Option<RtHostJobInputs> {
        None
    }

    /// 分页列表；未接线实现返回空页（REST 层加 available=false + reason）。
    fn list_jobs(&self, query: &JobQuery) -> Result<JobPage, CalibrationError>;

    /// 单个 job 详情。
    fn job(&self, job_id: &str) -> Result<JobRecord, CalibrationError>;

    /// 取消 job（协作式；已终态应如实返回当前状态或 409）。
    fn cancel(&self, job_id: &str) -> Result<JobRecord, CalibrationError>;

    /// job 的产物清单。
    fn artifacts(&self, job_id: &str) -> Result<Vec<ArtifactInfo>, CalibrationError>;

    /// 读产物原始字节（按 manifest 找文件；不存在返回 artifact_not_found）。
    fn read_artifact(&self, job_id: &str, name: &str) -> Result<Vec<u8>, CalibrationError>;

    /// apply 第一阶段：job 产出的可应用 plan。
    fn apply(&self, job_id: &str) -> Result<ApplyPlan, CalibrationError>;

    /// apply 第二阶段：daemon 控制面 commit 成功后回写（幂等）。
    fn apply_committed(
        &self,
        job_id: &str,
        applied: &AppliedVersion,
    ) -> Result<JobRecord, CalibrationError>;

    /// rollback 第一阶段：job 应用前的版本。
    fn rollback(&self, job_id: &str) -> Result<RollbackTarget, CalibrationError>;

    /// rollback 第二阶段：daemon 控制面 commit 成功后回写（幂等）。
    fn rollback_committed(
        &self,
        job_id: &str,
        applied: &AppliedVersion,
    ) -> Result<JobRecord, CalibrationError>;

    /// 注入 daemon 的事件汇（daemon 接线时调用一次；默认忽略）。
    fn attach_sink(&self, _sink: Arc<dyn JobSink>) {}

    /// 是否已接线（默认取 status）。
    fn wired(&self) -> bool {
        self.status().map(|status| status.wired).unwrap_or(false)
    }

    /// 未接线/降级原因（默认取 status）。
    fn reason(&self) -> Option<String> {
        match self.status() {
            Ok(status) => status.reason,
            Err(error) => Some(error.message().to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_state_names_are_plan_4_7_verbatim_and_roundtrip() {
        for state in [
            JobState::Queued,
            JobState::Running,
            JobState::Succeeded,
            JobState::Failed,
            JobState::Cancelled,
        ] {
            assert_eq!(JobState::parse(state.as_str()), Some(state));
        }
        assert_eq!(JobState::parse("complete"), None);
        assert!(JobState::Succeeded.is_terminal());
        assert!(JobState::Failed.is_terminal());
        assert!(!JobState::Running.is_terminal());
        assert!(!JobState::Queued.is_terminal());
    }

    #[test]
    fn step_state_names_roundtrip() {
        for state in [
            StepState::Pending,
            StepState::Running,
            StepState::Succeeded,
            StepState::Failed,
            StepState::Cancelled,
        ] {
            assert_eq!(StepState::parse(state.as_str()), Some(state));
        }
        assert_eq!(StepState::parse("done"), None);
        assert_eq!(
            OFFLINE_JOB_STEPS,
            ["validate", "compile", "levels", "closed_loop", "bundle"]
        );
        assert_eq!(
            RT_HOST_JOB_STEPS,
            ["validate", "sweep_capture", "quality_check", "bundle"]
        );
        assert_eq!(
            job_step_names(JOB_KIND_SINGLE_POINT_RT_HOST),
            RT_HOST_JOB_STEPS
        );
        assert_eq!(
            job_step_names(JOB_KIND_SINGLE_POINT_OFFLINE),
            OFFLINE_JOB_STEPS
        );
    }

    #[test]
    fn rt_host_job_inputs_roundtrip_without_inventing_optional_fields() {
        let minimal = serde_json::from_str::<RtHostJobInputs>(r#"{"audible":true}"#)
            .expect("minimal rt-host inputs parse");
        assert_eq!(
            minimal,
            RtHostJobInputs {
                audible: true,
                channels: None,
                level_dbfs: None,
            }
        );
        let full = RtHostJobInputs {
            audible: true,
            channels: Some(vec![0, 1, 5]),
            level_dbfs: Some(-12.0),
        };
        let json = serde_json::to_value(&full).expect("serialize");
        assert_eq!(json["channels"], serde_json::json!([0, 1, 5]));
        assert_eq!(json["level_dbfs"], serde_json::json!(-12.0));
        assert_eq!(
            serde_json::from_value::<RtHostJobInputs>(json).expect("parse"),
            full
        );
    }

    #[test]
    fn queued_job_has_all_five_steps_pending_and_no_invented_values() {
        let new_job = NewJob {
            kind: JOB_KIND_SINGLE_POINT_OFFLINE.to_string(),
            inputs: JobInputs {
                plan_sha: "0".repeat(64),
                baseline_upload_id: "1".repeat(64),
                applied_upload_id: "2".repeat(64),
                centers_hz: vec![100.0, 1000.0],
                tolerance_db: 2.0,
                min_bands: 2,
            },
            baseline: UploadRef {
                upload_id: "1".repeat(64),
                sha256: "1".repeat(64),
                bytes: 3,
                path: PathBuf::from("baseline.wav"),
            },
            applied: UploadRef {
                upload_id: "2".repeat(64),
                sha256: "2".repeat(64),
                bytes: 4,
                path: PathBuf::from("applied.wav"),
            },
        };
        let record =
            JobRecord::new_queued("job-0001", &new_job, Some("2026-01-01T00:00:00Z".into()));
        assert_eq!(record.state, JobState::Queued);
        assert_eq!(record.steps.len(), 5);
        assert!(record
            .steps
            .iter()
            .all(|step| step.state == StepState::Pending && step.sha256.is_none()));
        assert_eq!(record.verdict, None);
        assert!(record.result.is_none());
        assert_eq!(record.apply.state, ApplyState::NotApplied);
    }

    #[test]
    fn error_codes_and_statuses_are_stable() {
        let cases = [
            (
                CalibrationError::not_wired("x"),
                StatusCode::SERVICE_UNAVAILABLE,
                code::NOT_WIRED,
            ),
            (
                CalibrationError::mic_profile_missing("no profile"),
                StatusCode::CONFLICT,
                code::MIC_PROFILE_MISSING,
            ),
            (
                CalibrationError::switch_busy("job-0001"),
                StatusCode::CONFLICT,
                code::SWITCH_BUSY,
            ),
            (
                CalibrationError::job_not_found("job-0002"),
                StatusCode::NOT_FOUND,
                code::JOB_NOT_FOUND,
            ),
            (
                CalibrationError::upload_not_found("aa"),
                StatusCode::NOT_FOUND,
                code::UPLOAD_NOT_FOUND,
            ),
            (
                CalibrationError::invalid_request("inputs.plan_sha", "bad"),
                StatusCode::BAD_REQUEST,
                code::INVALID_REQUEST,
            ),
            (
                CalibrationError::invalid_upload("traversal"),
                StatusCode::BAD_REQUEST,
                code::INVALID_UPLOAD,
            ),
            (
                CalibrationError::unsupported_format(Some("text/plain")),
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                code::UNSUPPORTED_FORMAT,
            ),
            (
                CalibrationError::upload_too_large(64),
                StatusCode::PAYLOAD_TOO_LARGE,
                code::UPLOAD_TOO_LARGE,
            ),
            (
                CalibrationError::job_not_succeeded("job-1", "running"),
                StatusCode::CONFLICT,
                code::JOB_NOT_SUCCEEDED,
            ),
            (
                CalibrationError::plan_unavailable("job-1", "no plan.bin"),
                StatusCode::CONFLICT,
                code::PLAN_UNAVAILABLE,
            ),
            (
                CalibrationError::artifact_not_found("job-1", "report.json"),
                StatusCode::NOT_FOUND,
                code::ARTIFACT_NOT_FOUND,
            ),
            (
                CalibrationError::artifact_sha_mismatch("job-1", "report.json", "aa", "bb"),
                StatusCode::CONFLICT,
                code::ARTIFACT_SHA_MISMATCH,
            ),
            (
                CalibrationError::rollback_unavailable("job-1", "no previous version"),
                StatusCode::CONFLICT,
                code::ROLLBACK_UNAVAILABLE,
            ),
            (
                CalibrationError::no_applicable_job("no job"),
                StatusCode::CONFLICT,
                code::NO_APPLICABLE_JOB,
            ),
            (
                CalibrationError::internal("boom"),
                StatusCode::INTERNAL_SERVER_ERROR,
                code::INTERNAL_ERROR,
            ),
        ];
        for (error, status, expected_code) in cases {
            assert_eq!(error.status(), status, "{expected_code}");
            assert_eq!(error.code(), expected_code);
            assert!(!error.message().is_empty());
        }
    }
}
