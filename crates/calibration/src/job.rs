//! [NONRT] 校准 job 状态/互斥/progress/cancel/checkpoint/artifact（T60） ——WBS T60 写域。
//!
//! 本模块实现新校准 daemon 的 **job 执行层**（只属于控制面，禁止在 RT 线程调用）：
//! 状态机、单 job 互斥、设备独占交接、progress、协作式 cancel、stage 边界 checkpoint 与恢复、
//! artifact 记录/manifest、失败/取消时只回自研 last-good 或安全 mute。
//!
//! 权威输入（只读）：
//! - WBS T60 行与 §1/§5/§7（scratch/lead/planner/KHAEN-RT-STACK-WBS.md）；
//! - DSP-HOST-DECISION §2（新 daemon 拥有 job/state/progress/cancel/失败恢复）与 §2.6 状态集；
//! - rt-stack/spec/core-v1.md §5.4（DeviceOwner 唯一 owner）、§9（安全动作）、§12.1（本 crate 无 RT 区）；
//! - rt-stack/spec/compat-matrix.md R48/P08/P12/Y16/Y24/K14/K33/K56（job 状态集/扫频安全/应用回滚/stale/互斥）；
//! - calibration-spec/CONTRACT.md 与 ACCEPTANCE.md（artifact/报告字段与 stale 语义）。
//!
//! ## 状态机（DEC §2.6 冻结；机器名见 as_str）
//!
//! queued -> preflight -> sweep_capture -> quality_check -> fit_p -> solve_g -> compile -> prepared
//!        -> apply -> remeasure -> independent_holdout -> complete
//! 失败/取消终态：failed / cancelled / rolled_back（内部另有 apply 前的安全 abort 与 apply 后的自动
//! 回滚：都映射到 DEC 冻结的终态集，不新造 REST 状态名）。
//!
//! 不变量（与 DoD 一一对应）：
//! 1. **单 job 互斥**：同一 JobManager 同时只允许一个 active job；第二个 start 返回 Busy；
//! 2. **设备独占**：测量 stage（sweep_capture / remeasure / independent_holdout）前后必须通过
//!    SafeOutputController 完成 OwnerPlayback <-> OwnerCalibration 交接（core-v1 §5.4）；
//! 3. **progress**：每个 stage 0..1，overall =（已完成 stage 数 + 当前 stage 比例）/ stage 总数；
//!    单调不减且 clamp；终态 overall=1.0；
//! 4. **cancel**：协作式 CancelToken；stage 内由 backend 每步检查，stage 边界由 runner 检查；
//!    取消在 apply 提交前 -> Cancelled；apply 提交后 -> 自动回滚 last-good -> RolledBack；
//! 5. **checkpoint**：每次进入 stage 与 stage 完成/awaiting apply/终态时原子写（tmp+rename）；
//!    恢复粒度 = stage：resume 会重跑 checkpoint 记录中的 in-progress stage（stage 实现必须可重入/幂等，
//!    且不得依赖上一次进程的临时内存状态）；
//! 6. **artifact**：每个 stage 产出登记 ArtifactRecord（relative path + SHA-256 + bytes + version +
//!    stage + created_unix）；同一 relative path 内容不同即拒绝（artifact 不可变）；终态写
//!    artifacts.manifest.json（schema khaen-calibration-artifacts/v1）；
//! 7. **失败只 last-good/mute**：任何 failure/cancel 都不启动 CamillaDSP、不 spawn 外部进程、
//!    不假 ack；job 只调用 SafeOutputController 的 last-good 恢复 / 安全 mute 两个动作；
//!    没有 apply 过（applied_by_job=false）时 last-good 原样保持，只释放设备；
//! 8. **apply 门**：prepared 完成后默认停在 awaiting_apply（R48 apply 端点/用户确认）；只有
//!    auto_apply=true 或显式 JobManager::apply() 才进入 apply；apply 幂等（重复 apply 返回同一 job）。
//!
//! 与旧 calibrate 手工流程的区别：本模块不是「跑一次 khaen-calibrate 再把文件拷回去」，而是给 daemon
//! 用的 job 对象：start/status/cancel/apply/rollback 全生命周期 + 可恢复 checkpoint + 证据 artifact。
//!
//! 写域纪律：只由 T60 维护本文件；T42（REST）/T44（mode）/T61–T66（sweep/capture/positions/fit/compile_apply）
//! 通过本模块的公开 trait/类型接入，不修改本文件以外的公共注册（lib.rs/Cargo.toml 归 integrator）。

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// job 快照/checkpoint 的逻辑 schema（REST/T70 消费；字段只增不改，破坏性变更升 v2）。
pub const JOB_SCHEMA: &str = "khaen-calibration-job/v1";
/// checkpoint 文本文件 schema（内部持久化格式；不是 REST 契约）。
pub const CHECKPOINT_SCHEMA: &str = "khaen-calibration-checkpoint/v1";
/// artifact manifest JSON schema（T59/T66/证据链消费）。
pub const ARTIFACT_MANIFEST_SCHEMA: &str = "khaen-calibration-artifacts/v1";
/// 默认 checkpoint 文件名（放在 JobManagerConfig::checkpoint_path 指定目录或直接指定文件）。
pub const CHECKPOINT_FILE_NAME: &str = "calibration-job.checkpoint";
/// artifact manifest 文件名（每个 job 的 artifacts_dir 下）。
pub const ARTIFACT_MANIFEST_FILE_NAME: &str = "artifacts.manifest.json";
/// 单个 job 允许登记的 artifact 数上限（防止坏 backend 无限登记）。
pub const MAX_ARTIFACTS: usize = 256;
/// job request 绑定的输入 hash 数上限（配置/role/P/fs 等 stale 输入）。
pub const MAX_INPUT_HASHES: usize = 32;
/// job label 最大字符数。
pub const MAX_LABEL_CHARS: usize = 200;

/// 单调递增的 job id（同一 manager 进程内唯一；跨 restart 由 checkpoint 续号）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JobId(pub u64);

impl JobId {
    /// 数值 id（REST 可用 u64，也可用 Display 的 job-NNNN 字符串）。
    pub fn as_u64(self) -> u64 {
        self.0
    }

    /// 每 job 的 artifact 目录名（artifacts_root/job-NNNN）。
    pub fn artifact_dir_name(self) -> String {
        format!("job-{:04}", self.0)
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "job-{:04}", self.0)
    }
}

/// job 类型。
///
/// - `Full`：完整校准（扫频/拟合/编译/应用/复测/holdout）；
/// - `Retest`：对已应用系数只做复测+holdout；
/// - `SinglePointOffline`（T70h-b）：离线闭环证据 job（PLAN §4.7）。输入是已上传的方案 +
///   两段 WAV（基线/施加），步骤 `validate -> compile -> levels -> closed_loop -> bundle`，
///   不打开设备、不跑 RT 图（真实测量采集属 T70f-c）。
/// - `SinglePointRtHost`（C3b1）：真实单点测量 job。daemon 经 host link 驱动 rt-host 的
///   `cal_enter -> cal_measure -> cal_status -> cal_leave`（真实设备/UMIK 采集），步骤
///   `validate -> sweep_capture -> quality_check -> bundle`；本切片只产出真实测量证据，
///   solve/apply（方案设计/应用）属 C3b2，job 不产出可应用方案、不报 PASS。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Full,
    Retest,
    SinglePointOffline,
    SinglePointRtHost,
}

/// Full 的 stage 顺序（DEC §2.6 冻结）。
const FULL_PLAN: [JobStage; 10] = [
    JobStage::Preflight,
    JobStage::SweepCapture,
    JobStage::QualityCheck,
    JobStage::FitP,
    JobStage::SolveG,
    JobStage::Compile,
    JobStage::Prepared,
    JobStage::Apply,
    JobStage::Remeasure,
    JobStage::IndependentHoldout,
];

/// Retest 的 stage 顺序：preflight（校验 applied version/设备/时钟）-> remeasure -> holdout。
const RETEST_PLAN: [JobStage; 3] = [
    JobStage::Preflight,
    JobStage::Remeasure,
    JobStage::IndependentHoldout,
];

/// SinglePointOffline 的 stage 顺序（PLAN §4.7：编译 -> 电平 -> 闭环判定 -> 证据包；
/// validate 是前面的输入校验步，不跳步）。
pub const SINGLE_POINT_OFFLINE_PLAN: [JobStage; 5] = [
    JobStage::Validate,
    JobStage::Compile,
    JobStage::Levels,
    JobStage::ClosedLoop,
    JobStage::Bundle,
];

/// SinglePointRtHost 的 stage 顺序（C3b1：真实测量证据链，不跳步）。
///
/// `validate`（MicProfile/rt-host 可用性/可闻授权）-> `sweep_capture`（rt-host cal_enter +
/// cal_measure + 轮询 cal_status）-> `quality_check`（只搬 rt-host 真实质量字段；没有就写
/// unavailable）-> `bundle`（报告 + manifest）。这里没有 compile/apply：本切片不产出可应用方案。
pub const SINGLE_POINT_RT_HOST_PLAN: [JobStage; 4] = [
    JobStage::Validate,
    JobStage::SweepCapture,
    JobStage::QualityCheck,
    JobStage::Bundle,
];

impl JobKind {
    /// 机器名（REST 字段值）。
    pub const fn as_str(self) -> &'static str {
        match self {
            JobKind::Full => "full",
            JobKind::Retest => "retest",
            JobKind::SinglePointOffline => "single_point_offline",
            JobKind::SinglePointRtHost => "single_point_rt_host",
        }
    }

    /// 解析机器名；未知值拒绝（不静默降级）。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "full" => Some(JobKind::Full),
            "retest" => Some(JobKind::Retest),
            "single_point_offline" => Some(JobKind::SinglePointOffline),
            "single_point_rt_host" => Some(JobKind::SinglePointRtHost),
            _ => None,
        }
    }

    /// 本 job 类型的 stage 计划。
    pub const fn plan(self) -> &'static [JobStage] {
        match self {
            JobKind::Full => &FULL_PLAN,
            JobKind::Retest => &RETEST_PLAN,
            JobKind::SinglePointOffline => &SINGLE_POINT_OFFLINE_PLAN,
            JobKind::SinglePointRtHost => &SINGLE_POINT_RT_HOST_PLAN,
        }
    }
}

/// 校准 job 的 stage（与 DEC §2.6 状态一一对应；REST 用 as_str 的机器名）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum JobStage {
    Preflight,
    SweepCapture,
    QualityCheck,
    FitP,
    SolveG,
    Compile,
    Prepared,
    Apply,
    Remeasure,
    IndependentHoldout,
    /// 离线闭环 job：校验输入（方案/WAV/参数）。
    Validate,
    /// 离线闭环 job：电平→闭环判定之后无；本 stage 不用于 FULL/RETEST。
    Levels,
    /// 离线闭环 job：预测 vs 实测三态判定。
    ClosedLoop,
    /// 离线闭环 job：确定性证据包（predicted/observed/report/manifest）。
    Bundle,
}

impl JobStage {
    /// DEC §2.6 的完整 stage 枚举（不含 Retest 子集）。
    pub const ALL: [JobStage; 10] = FULL_PLAN;

    /// REST/JSON 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            JobStage::Preflight => "preflight",
            JobStage::SweepCapture => "sweep_capture",
            JobStage::QualityCheck => "quality_check",
            JobStage::FitP => "fit_p",
            JobStage::SolveG => "solve_g",
            JobStage::Compile => "compile",
            JobStage::Prepared => "prepared",
            JobStage::Apply => "apply",
            JobStage::Remeasure => "remeasure",
            JobStage::IndependentHoldout => "independent_holdout",
            JobStage::Validate => "validate",
            JobStage::Levels => "levels",
            JobStage::ClosedLoop => "closed_loop",
            JobStage::Bundle => "bundle",
        }
    }

    /// 在 FULL_PLAN 中的下标（0-based）。
    pub const fn index(self) -> usize {
        match self {
            JobStage::Preflight => 0,
            JobStage::SweepCapture => 1,
            JobStage::QualityCheck => 2,
            JobStage::FitP => 3,
            JobStage::SolveG => 4,
            JobStage::Compile => 5,
            JobStage::Prepared => 6,
            JobStage::Apply => 7,
            JobStage::Remeasure => 8,
            JobStage::IndependentHoldout => 9,
            // 以下四个 stage 只属于 SinglePointOffline 计划，不在 FULL_PLAN 内；
            // 调度用 plan 位置（stage_pos），本函数仅 Full/Retest 的 DEC 语义，故给 0。
            JobStage::Validate | JobStage::Levels | JobStage::ClosedLoop | JobStage::Bundle => 0,
        }
    }

    /// 解析机器名；未知值拒绝（含 SinglePointOffline 专属 stage 名）。
    pub fn parse(value: &str) -> Option<Self> {
        if let Some(stage) = JobStage::ALL.iter().copied().find(|s| s.as_str() == value) {
            return Some(stage);
        }
        match value {
            "validate" => Some(JobStage::Validate),
            "levels" => Some(JobStage::Levels),
            "closed_loop" => Some(JobStage::ClosedLoop),
            "bundle" => Some(JobStage::Bundle),
            _ => None,
        }
    }

    /// 该 stage 是否需要独占音频设备（core-v1 §5.4 OwnerCalibration）。
    pub const fn needs_calibration_device(self) -> bool {
        matches!(
            self,
            JobStage::SweepCapture | JobStage::Remeasure | JobStage::IndependentHoldout
        )
    }
}

/// job 状态（active stage 内嵌；终态来自 DEC §2.6 的 complete/failed/cancelled/rolled_back）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Active(JobStage),
    Complete,
    Failed,
    Cancelled,
    RolledBack,
}

impl JobState {
    /// REST/JSON 状态名（active 用 stage 名；与 DEC §2.6 状态集逐字对应）。
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Active(stage) => stage.as_str(),
            JobState::Complete => "complete",
            JobState::Failed => "failed",
            JobState::Cancelled => "cancelled",
            JobState::RolledBack => "rolled_back",
        }
    }

    /// 当前 active stage（终态/queued 为 None）。
    pub fn active_stage(self) -> Option<JobStage> {
        match self {
            JobState::Active(stage) => Some(stage),
            _ => None,
        }
    }

    /// 是否终态（complete/failed/cancelled/rolled_back）。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            JobState::Complete | JobState::Failed | JobState::Cancelled | JobState::RolledBack
        )
    }

    /// 解析状态名（queued / 10 个 stage 名 / 4 个终态）。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(JobState::Queued),
            "complete" => Some(JobState::Complete),
            "failed" => Some(JobState::Failed),
            "cancelled" => Some(JobState::Cancelled),
            "rolled_back" => Some(JobState::RolledBack),
            other => JobStage::parse(other).map(JobState::Active),
        }
    }
}

/// 设备交接后 playback 的处置方式（不允许回旧 Camilla；只有正常恢复或安全 mute）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitDisposition {
    /// 释放校准设备、回到 self-built playback/last-good 图（不启动任何外部进程）。
    ResumePlayback,
    /// 释放校准设备、playback 进入预分配安全 mute。
    SafetyMute,
}

impl ExitDisposition {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            ExitDisposition::ResumePlayback => "resume_playback",
            ExitDisposition::SafetyMute => "safety_mute",
        }
    }
}

/// 安全控制面失败（设备交接/last-good 恢复/mute）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetyError {
    /// 稳定错误码（REST/log 用，例如 DEVICE_BUSY）。
    pub code: String,
    /// 人读说明。
    pub message: String,
}

impl SafetyError {
    /// 构造安全错误。
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        SafetyError {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for SafetyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SafetyError {}

/// 校准 job 失败后的安全恢复结果（DoD：失败只 last-good/mute，无第三条路）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryOutcome {
    /// 已回自研 last-good 图/系数（version 为自研 last-good 标识，不是 Camilla 进程）。
    LastGood { version: String },
    /// last-good 不可用/恢复失败，已进入预分配安全 mute。
    Mute,
    /// last-good 与 mute 都失败（不可恢复；必须人工/重启处理，绝不自称安全）。
    Failed { error: String },
}

impl RecoveryOutcome {
    /// 机器名（last_good/mute/failed）。
    pub const fn as_str(&self) -> &'static str {
        match self {
            RecoveryOutcome::LastGood { .. } => "last_good",
            RecoveryOutcome::Mute => "mute",
            RecoveryOutcome::Failed { .. } => "failed",
        }
    }
}

/// 一次恢复动作的完整证据（写 checkpoint/manifest/REST status）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    /// 恢复结果。
    pub outcome: RecoveryOutcome,
    /// 触发恢复的原因（cancel 来源/失败 stage/错误码）。
    pub reason: String,
    /// 是否已释放校准设备 owner。
    pub device_released: bool,
    /// 恢复发生时间（unix 秒）。
    pub at_unix: u64,
}

/// 自研 host 的安全输出控制面（T20/T21/T26 实现；T60 只调用这四个动作）。
///
/// 硬约束：实现只能操作 self-built host 的 last-good 图/系数与预分配 mute buffer；
/// **不得**通过 systemd 启停 CamillaDSP、不得 spawn 外部进程、不得回旧 C 流程。
/// 所有方法必须可在非 RT 控制线程调用（本 trait 不承诺 RT 安全性）。
pub trait SafeOutputController: Send + Sync {
    /// 暂停/收尾 playback 并把音频设备 owner 交接给校准 daemon（OwnerPlayback -> OwnerCalibration）。
    /// 失败时必须保证设备没有被两个 owner 同时打开（core-v1 §5.4）。
    fn acquire_calibration_owner(&self) -> Result<(), SafetyError>;

    /// 释放校准设备 owner，恢复 playback（last-good 或安全 mute）。
    fn release_calibration_owner(&self, disposition: ExitDisposition) -> Result<(), SafetyError>;

    /// 回滚到自研 last-good 图/系数；返回 last-good version 标识。失败时调用方会再试安全 mute。
    fn restore_last_good(&self) -> Result<String, SafetyError>;

    /// 进入预分配安全 mute（不重算昂贵 P、不输出半图）。
    fn activate_safe_mute(&self) -> Result<(), SafetyError>;

    /// 当前 last-good version（用于失败但未 apply 时记录“仍在 last-good”的证据；可为 None）。
    fn last_good_version(&self) -> Option<String>;
}

/// backend stage 失败（不是 panic；panic 由 runner catch 并记 BACKEND_PANIC）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageFailure {
    /// 稳定错误码（例如 E_QUALITY_SNR）。
    pub code: String,
    /// 人读说明。
    pub message: String,
    /// true=协作式取消（backend 检测到 CancelToken）；false=真实失败。
    pub cancelled: bool,
}

impl StageFailure {
    /// 真实失败。
    pub fn failed(code: impl Into<String>, message: impl Into<String>) -> Self {
        StageFailure {
            code: code.into(),
            message: message.into(),
            cancelled: false,
        }
    }

    /// 协作式取消（backend 应在每个可中断点检查 CancelToken::check）。
    pub fn cancelled(message: impl Into<String>) -> Self {
        StageFailure {
            code: "CANCELLED".to_string(),
            message: message.into(),
            cancelled: true,
        }
    }
}

impl fmt::Display for StageFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for StageFailure {}

/// artifact 类别（Y24：每 artifact 带 version/hash/stage）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactKind {
    RawSweep,
    RawCapture,
    QualityReport,
    CalibrationDraft,
    DesignReport,
    FirBundle,
    ApplyReport,
    RemeasureReport,
    HoldoutReport,
    RecoveryReport,
    /// 扩展类别（必须是非空、无控制字符的稳定名字，例如 session_meta）。
    Other(String),
}

impl ArtifactKind {
    /// 机器名（REST/manifest 字段值）。
    pub fn as_str(&self) -> &str {
        match self {
            ArtifactKind::RawSweep => "raw_sweep",
            ArtifactKind::RawCapture => "raw_capture",
            ArtifactKind::QualityReport => "quality_report",
            ArtifactKind::CalibrationDraft => "calibration_draft",
            ArtifactKind::DesignReport => "design_report",
            ArtifactKind::FirBundle => "fir_bundle",
            ArtifactKind::ApplyReport => "apply_report",
            ArtifactKind::RemeasureReport => "remeasure_report",
            ArtifactKind::HoldoutReport => "holdout_report",
            ArtifactKind::RecoveryReport => "recovery_report",
            ArtifactKind::Other(name) => name.as_str(),
        }
    }

    /// checkpoint 编码（Other 用 other:<name> 前缀区分已知枚举与扩展名）。
    fn checkpoint_encode(&self) -> String {
        match self {
            ArtifactKind::Other(name) => format!("other:{}", name),
            other => other.as_str().to_string(),
        }
    }

    /// 从 checkpoint 编码解析；未知扩展名也接受（Other）。
    fn checkpoint_parse(value: &str) -> Option<Self> {
        let known = match value {
            "raw_sweep" => Some(ArtifactKind::RawSweep),
            "raw_capture" => Some(ArtifactKind::RawCapture),
            "quality_report" => Some(ArtifactKind::QualityReport),
            "calibration_draft" => Some(ArtifactKind::CalibrationDraft),
            "design_report" => Some(ArtifactKind::DesignReport),
            "fir_bundle" => Some(ArtifactKind::FirBundle),
            "apply_report" => Some(ArtifactKind::ApplyReport),
            "remeasure_report" => Some(ArtifactKind::RemeasureReport),
            "holdout_report" => Some(ArtifactKind::HoldoutReport),
            "recovery_report" => Some(ArtifactKind::RecoveryReport),
            _ => None,
        };
        if known.is_some() {
            return known;
        }
        if let Some(rest) = value.strip_prefix("other:") {
            if !rest.is_empty() {
                return Some(ArtifactKind::Other(rest.to_string()));
            }
        }
        None
    }
}

/// 一个 job artifact 的登记记录（文件系统里的真实文件 + 内容 hash + 产生 stage/version）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRecord {
    /// 类别。
    pub kind: ArtifactKind,
    /// 产生该 artifact 的 stage。
    pub stage: JobStage,
    /// 相对 job artifacts_dir 的路径（只允许 '/' 分隔、无 '..'、无盘符；restart 后可迁移）。
    pub relative_path: String,
    /// 文件内容 SHA-256（小写 hex，64 字符）。
    pub sha256: String,
    /// 文件字节数。
    pub bytes: u64,
    /// artifact/内容版本标识（例如 calibration draft version 或 producer version）。
    pub version: String,
    /// 登记时间（unix 秒）。
    pub created_unix: u64,
}

/// job request 绑定的只读输入 hash（config/role/P/fs 等；用于 Y24/X18 的 stale 判定证据）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputHash {
    /// 输入名（例如 layout-presets / calibration-spec / fir-toolbox；非空、唯一）。
    pub name: String,
    /// 输入内容 SHA-256（64 hex）。
    pub sha256: String,
}

/// 启动一个校准 job 的请求（R48 start 的 daemon 侧参数；REST 外形由 T42 冻结）。
#[derive(Debug, Clone, PartialEq)]
pub struct JobRequest {
    /// job 类型。
    pub kind: JobKind,
    /// 人读 label（例如 4.1.2 客厅 2026-11-17；≤200 字符）。
    pub label: String,
    /// 目标布局 id（layout-presets 的 output_layouts id，例如 4.1.2）。
    pub layout: String,
    /// 采样率（48000/96000 为部署口径；44100/88200 兼容）。
    pub sample_rate: u32,
    /// design 测点数（拟合允许使用；≥1）。
    pub design_points: u32,
    /// holdout 测点数（不参与拟合；≥1，ACCEPTANCE §0.6）。
    pub holdout_points: u32,
    /// 每点重复次数（≥1；建议 ≥3）。
    pub repeats: u32,
    /// 扫频驱动电平（dBFS，-60..=0；P08 安全电平）。
    pub drive_level_dbfs: f32,
    /// 用户是否已授权可闻扫频（P08：必须显式授权；false 拒绝 start）。
    pub audible_sweep_authorized: bool,
    /// prepared 后是否自动 apply；false（默认）停在 awaiting_apply 等显式命令。
    pub auto_apply: bool,
    /// Retest 时被复测的 applied version（Full 必须 None）。
    pub expected_applied_version: Option<String>,
    /// stale 判定用的输入 hash 清单（≤32；可为空但生产应绑定）。
    pub inputs: Vec<InputHash>,
}

impl JobRequest {
    /// 本请求的 stage 计划。
    pub const fn plan(&self) -> &'static [JobStage] {
        self.kind.plan()
    }

    /// 严格校验（未知/越界全部拒绝，不静默降级）。
    pub fn validate(&self) -> Result<(), String> {
        if self.label.trim().is_empty() {
            return Err("label must be non-empty".to_string());
        }
        if self.label.chars().count() > MAX_LABEL_CHARS {
            return Err(format!("label exceeds {MAX_LABEL_CHARS} chars"));
        }
        if self.layout.trim().is_empty() || self.layout.chars().count() > 64 {
            return Err("layout must be non-empty and <=64 chars".to_string());
        }
        if !matches!(self.sample_rate, 44100 | 48000 | 88200 | 96000) {
            return Err(format!("unsupported sample_rate: {}", self.sample_rate));
        }
        if !(1..=64).contains(&self.design_points) {
            return Err("design_points must be in 1..=64".to_string());
        }
        if !(1..=16).contains(&self.holdout_points) {
            return Err("holdout_points must be in 1..=16".to_string());
        }
        if !(1..=10).contains(&self.repeats) {
            return Err("repeats must be in 1..=10".to_string());
        }
        if !self.drive_level_dbfs.is_finite() || !(-60.0..=0.0).contains(&self.drive_level_dbfs) {
            return Err("drive_level_dbfs must be finite and in -60..=0".to_string());
        }
        // 离线证据 job（SinglePointOffline）不打开设备、不播放扫频，P08 的可闻授权不适用；
        // 任何会采音/出声的 kind 仍必须显式授权（fail-closed）。
        if !self.audible_sweep_authorized && self.kind != JobKind::SinglePointOffline {
            return Err(
                "audible_sweep_authorized must be true for jobs that capture audio (P08)"
                    .to_string(),
            );
        }
        match self.kind {
            JobKind::Full | JobKind::SinglePointOffline | JobKind::SinglePointRtHost => {
                if self.expected_applied_version.is_some() {
                    return Err("expected_applied_version is only valid for retest".to_string());
                }
            }
            JobKind::Retest => {
                let version = self
                    .expected_applied_version
                    .as_deref()
                    .ok_or_else(|| "retest requires expected_applied_version".to_string())?;
                if version.trim().is_empty() || version.chars().count() > 128 {
                    return Err(
                        "expected_applied_version must be non-empty and <=128 chars".to_string()
                    );
                }
            }
        }
        if self.inputs.len() > MAX_INPUT_HASHES {
            return Err(format!("too many input hashes (> {MAX_INPUT_HASHES})"));
        }
        let mut seen = Vec::with_capacity(self.inputs.len());
        for input in &self.inputs {
            if input.name.trim().is_empty() || input.name.chars().count() > 64 {
                return Err("input name must be non-empty and <=64 chars".to_string());
            }
            if !is_sha256_hex(&input.sha256) {
                return Err(format!(
                    "input {} sha256 must be 64 lowercase hex chars",
                    input.name
                ));
            }
            if seen.contains(&input.name.as_str()) {
                return Err(format!("duplicate input name: {}", input.name));
            }
            seen.push(input.name.as_str());
        }
        Ok(())
    }
}

/// job 失败信息（REST status 的 error 字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobFailure {
    /// 稳定错误码（stage 错误码/DEVICE_OWNERSHIP/CHECKPOINT_IO/...）。
    pub code: String,
    /// 人读说明。
    pub message: String,
    /// 失败所在 stage（若可定位）。
    pub stage: Option<JobStage>,
}

/// 对 REST/evidence 暴露的 job 快照（不可变拷贝；时间单位 unix 秒；progress 0..=1）。
#[derive(Debug, Clone, PartialEq)]
pub struct JobSnapshot {
    /// 快照 schema（JOB_SCHEMA）。
    pub schema: &'static str,
    /// job id。
    pub job_id: JobId,
    /// job 类型。
    pub kind: JobKind,
    /// 当前状态。
    pub state: JobState,
    /// 当前 active stage（终态/queued 为 None）。
    pub stage: Option<JobStage>,
    /// 当前 stage 在 plan 中下标（终态=stage_count）。
    pub stage_index: u32,
    /// plan 的 stage 总数。
    pub stage_count: u32,
    /// 当前 stage 进度 0..=1。
    pub stage_progress: f32,
    /// 整体进度 0..=1。
    pub overall_progress: f32,
    /// 是否停在 prepared 等 apply 命令。
    pub awaiting_apply: bool,
    /// 是否已请求 cancel（终态可能是 cancelled/rolled_back）。
    pub cancel_requested: bool,
    /// 创建时间。
    pub created_unix: u64,
    /// 最近更新时间。
    pub updated_unix: u64,
    /// 开始执行时间（首 stage 进入）。
    pub started_unix: Option<u64>,
    /// 终态时间。
    pub finished_unix: Option<u64>,
    /// artifact 根目录（绝对路径字符串；REST/证据定位用）。
    pub artifacts_dir: String,
    /// job 请求参数。
    pub request: JobRequest,
    /// 是否由本 job apply 了新的 set（决定失败是否需要 restore last-good）。
    pub applied_by_job: bool,
    /// 当前/最近 applied version（Full apply 成功后；Retest 为 expected applied version）。
    pub applied_version: Option<String>,
    /// 自研 last-good version。
    pub last_good_version: Option<String>,
    /// 失败/取消后的安全恢复报告。
    pub recovery: Option<RecoveryReport>,
    /// 失败信息（cancelled/rolled_back 也可能带 apply 失败码）。
    pub failure: Option<JobFailure>,
    /// 终态证据写失败记录（不伪造成 PASS；写入失败时如实标注）。
    pub evidence_errors: Vec<String>,
    /// 终态原因（completed / cancelled_by_user / stage_failed:<stage> / ...）。
    pub terminal_reason: Option<String>,
    /// 已登记 artifact（按登记顺序）。
    pub artifacts: Vec<ArtifactRecord>,
}

impl JobSnapshot {
    fn new(job_id: JobId, request: &JobRequest, artifacts_dir: &Path) -> Self {
        let now = unix_now();
        let plan = request.plan();
        JobSnapshot {
            schema: JOB_SCHEMA,
            job_id,
            kind: request.kind,
            state: JobState::Queued,
            stage: None,
            stage_index: 0,
            stage_count: plan.len() as u32,
            stage_progress: 0.0,
            overall_progress: 0.0,
            awaiting_apply: false,
            cancel_requested: false,
            created_unix: now,
            updated_unix: now,
            started_unix: None,
            finished_unix: None,
            artifacts_dir: artifacts_dir.to_string_lossy().to_string(),
            request: request.clone(),
            applied_by_job: false,
            applied_version: if request.kind == JobKind::Retest {
                request.expected_applied_version.clone()
            } else {
                None
            },
            last_good_version: None,
            recovery: None,
            failure: None,
            evidence_errors: Vec::new(),
            terminal_reason: None,
            artifacts: Vec::new(),
        }
    }

    /// 当前状态机器名。
    pub fn state_name(&self) -> &'static str {
        self.state.as_str()
    }

    /// REST/日志用 JSON（手写序列化；无第三方依赖；字段稳定）。
    pub fn to_json(&self) -> String {
        let artifacts = self
            .artifacts
            .iter()
            .map(artifact_json)
            .collect::<Vec<_>>()
            .join(",");
        let evidence_errors = self
            .evidence_errors
            .iter()
            .map(|e| json_string(e))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            concat!(
                "{{",
                "\"schema\":{},",
                "\"job_id\":{},",
                "\"kind\":{},",
                "\"state\":{},",
                "\"stage\":{},",
                "\"stage_index\":{},",
                "\"stage_count\":{},",
                "\"stage_progress\":{},",
                "\"overall_progress\":{},",
                "\"awaiting_apply\":{},",
                "\"cancel_requested\":{},",
                "\"created_unix\":{},",
                "\"updated_unix\":{},",
                "\"started_unix\":{},",
                "\"finished_unix\":{},",
                "\"artifacts_dir\":{},",
                "\"request\":{},",
                "\"applied_by_job\":{},",
                "\"applied_version\":{},",
                "\"last_good_version\":{},",
                "\"recovery\":{},",
                "\"failure\":{},",
                "\"evidence_errors\":[{}],",
                "\"terminal_reason\":{},",
                "\"artifacts\":[{}]",
                "}}"
            ),
            json_string(JOB_SCHEMA),
            json_string(&self.job_id.to_string()),
            json_string(self.kind.as_str()),
            json_string(self.state.as_str()),
            json_opt_stage(self.stage),
            self.stage_index,
            self.stage_count,
            json_f32(self.stage_progress),
            json_f32(self.overall_progress),
            self.awaiting_apply,
            self.cancel_requested,
            self.created_unix,
            self.updated_unix,
            json_opt_u64(self.started_unix),
            json_opt_u64(self.finished_unix),
            json_string(&self.artifacts_dir),
            request_json(&self.request),
            self.applied_by_job,
            json_opt_string(&self.applied_version),
            json_opt_string(&self.last_good_version),
            recovery_json(self.recovery.as_ref()),
            failure_json(self.failure.as_ref()),
            evidence_errors,
            json_opt_string(&self.terminal_reason),
            artifacts
        )
    }

    /// 终态 artifact manifest JSON（schema khaen-calibration-artifacts/v1）。
    pub fn artifact_manifest_json(&self, generated_unix: u64) -> String {
        let artifacts = self
            .artifacts
            .iter()
            .map(artifact_json)
            .collect::<Vec<_>>()
            .join(",");
        format!(
            concat!(
                "{{",
                "\"schema\":{},",
                "\"generated_unix\":{},",
                "\"job_id\":{},",
                "\"kind\":{},",
                "\"state\":{},",
                "\"stage_index\":{},",
                "\"stage_count\":{},",
                "\"applied_by_job\":{},",
                "\"applied_version\":{},",
                "\"last_good_version\":{},",
                "\"terminal_reason\":{},",
                "\"failure\":{},",
                "\"recovery\":{},",
                "\"artifacts\":[{}]",
                "}}"
            ),
            json_string(ARTIFACT_MANIFEST_SCHEMA),
            generated_unix,
            json_string(&self.job_id.to_string()),
            json_string(self.kind.as_str()),
            json_string(self.state.as_str()),
            self.stage_index,
            self.stage_count,
            self.applied_by_job,
            json_opt_string(&self.applied_version),
            json_opt_string(&self.last_good_version),
            json_opt_string(&self.terminal_reason),
            failure_json(self.failure.as_ref()),
            recovery_json(self.recovery.as_ref()),
            artifacts
        )
    }
}

/// job 控制面错误（manager 方法返回；REST 映射由 T42 决定）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobError {
    /// 已有 active job（单 job 互斥失败）。
    Busy,
    /// checkpoint 里有一个未完成的 job，必须先 resume 或 abandon 才能 start。
    ResumePending,
    /// 没有待恢复的 checkpoint。
    NoPendingResume,
    /// 没有 active job。
    NoActiveJob,
    /// 请求参数非法（字段级说明）。
    InvalidRequest(String),
    /// 当前状态不允许该命令（含状态说明）。
    InvalidState(String),
    /// checkpoint 读写/解析失败。
    Checkpoint(String),
    /// 文件系统 IO 失败。
    Io(String),
    /// 线程创建失败。
    Thread(String),
    /// 等待终态超时。
    Timeout,
}

impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobError::Busy => write!(f, "another calibration job is active"),
            JobError::ResumePending => write!(f, "a checkpointed job is pending resume"),
            JobError::NoPendingResume => write!(f, "no checkpointed job to resume"),
            JobError::NoActiveJob => write!(f, "no active calibration job"),
            JobError::InvalidRequest(message) => write!(f, "invalid job request: {message}"),
            JobError::InvalidState(message) => write!(f, "invalid job state: {message}"),
            JobError::Checkpoint(message) => write!(f, "checkpoint error: {message}"),
            JobError::Io(message) => write!(f, "io error: {message}"),
            JobError::Thread(message) => write!(f, "thread error: {message}"),
            JobError::Timeout => write!(f, "timed out waiting for job terminal state"),
        }
    }
}

impl std::error::Error for JobError {}

/// 协作式取消令牌（Clone 后共享同一标志；backend 在 stage 内周期调用 check）。
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    /// 新令牌（未取消）。
    pub fn new() -> Self {
        CancelToken {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 请求取消（幂等；worker 在 stage 边界与 backend 在 stage 内检查）。
    pub fn request(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// 是否已请求取消。
    pub fn is_requested(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// 已取消则返回 StageFailure::cancelled（便于 backend 用 ? 传播）。
    pub fn check(&self) -> Result<(), StageFailure> {
        if self.is_requested() {
            Err(StageFailure::cancelled("calibration job cancelled"))
        } else {
            Ok(())
        }
    }
}

/// stage 执行上下文：backend 通过它报告进度、登记 artifact、读写版本（只在本 job 生命周期内有效）。
pub struct StageCtx<'a> {
    job_id: JobId,
    stage: JobStage,
    request: &'a JobRequest,
    cancel: CancelToken,
    slot: &'a Arc<JobSlotShared>,
}

impl<'a> StageCtx<'a> {
    fn new(
        job_id: JobId,
        stage: JobStage,
        request: &'a JobRequest,
        cancel: CancelToken,
        slot: &'a Arc<JobSlotShared>,
    ) -> Self {
        StageCtx {
            job_id,
            stage,
            request,
            cancel,
            slot,
        }
    }

    /// job id。
    pub fn job_id(&self) -> JobId {
        self.job_id
    }

    /// 当前 stage。
    pub fn stage(&self) -> JobStage {
        self.stage
    }

    /// job 请求参数（只读）。
    pub fn request(&self) -> &JobRequest {
        self.request
    }

    /// 取消令牌（backend 在长循环内周期检查）。
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// 本 job 的 artifact 根目录。
    pub fn artifacts_dir(&self) -> PathBuf {
        self.slot.data().artifacts_dir.clone()
    }

    /// 当前 applied version（apply 成功后由 backend 设置）。
    pub fn applied_version(&self) -> Option<String> {
        self.slot.data().snapshot.applied_version.clone()
    }

    /// 当前 last-good version。
    pub fn last_good_version(&self) -> Option<String> {
        self.slot.data().snapshot.last_good_version.clone()
    }

    /// 报告当前 stage 进度（0..=1；越界 clamp；单调不减）。
    pub fn report_progress(&self, fraction: f32) {
        let mut data = self.slot.data();
        if data.plan.get(data.stage_pos) != Some(&self.stage)
            || data.snapshot.stage != Some(self.stage)
            || data.snapshot.state.is_terminal()
        {
            return;
        }
        let clamped = if fraction.is_finite() {
            fraction.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if clamped >= data.snapshot.stage_progress {
            data.snapshot.stage_progress = clamped;
            data.snapshot.overall_progress = overall_progress(
                data.snapshot.stage_index as usize,
                clamped,
                data.snapshot.stage_count as usize,
            );
            data.snapshot.updated_unix = unix_now();
            drop(data);
            self.slot.cv.notify_all();
        }
    }

    /// 写一个 artifact 文件并登记（父目录自动创建；内容 hash 由本模块计算）。
    pub fn write_artifact(
        &self,
        kind: ArtifactKind,
        relative_path: &str,
        bytes: &[u8],
        version: &str,
    ) -> Result<ArtifactRecord, StageFailure> {
        validate_artifact_path(relative_path)
            .map_err(|message| StageFailure::failed("E_ARTIFACT_PATH", message))?;
        let dir = self.artifacts_dir();
        let path = dir.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                StageFailure::failed(
                    "E_ARTIFACT_IO",
                    format!("create artifact dir {}: {e}", parent.display()),
                )
            })?;
        }
        fs::write(&path, bytes).map_err(|e| {
            StageFailure::failed(
                "E_ARTIFACT_IO",
                format!("write artifact {}: {e}", path.display()),
            )
        })?;
        self.record_artifact(
            kind,
            relative_path,
            bytes.len() as u64,
            sha256_hex(bytes),
            version,
        )
    }

    /// 登记一个已存在的 artifact 文件（计算 SHA-256；不复制文件）。
    pub fn record_artifact_file(
        &self,
        kind: ArtifactKind,
        relative_path: &str,
        version: &str,
    ) -> Result<ArtifactRecord, StageFailure> {
        validate_artifact_path(relative_path)
            .map_err(|message| StageFailure::failed("E_ARTIFACT_PATH", message))?;
        let path = self.artifacts_dir().join(relative_path);
        let bytes = fs::read(&path).map_err(|e| {
            StageFailure::failed(
                "E_ARTIFACT_IO",
                format!("read artifact {}: {e}", path.display()),
            )
        })?;
        self.record_artifact(
            kind,
            relative_path,
            bytes.len() as u64,
            sha256_hex(&bytes),
            version,
        )
    }

    fn record_artifact(
        &self,
        kind: ArtifactKind,
        relative_path: &str,
        bytes: u64,
        sha256: String,
        version: &str,
    ) -> Result<ArtifactRecord, StageFailure> {
        if version.trim().is_empty() || version.chars().count() > 128 {
            return Err(StageFailure::failed(
                "E_ARTIFACT_VERSION",
                "artifact version must be non-empty and <=128 chars",
            ));
        }
        let record = ArtifactRecord {
            kind,
            stage: self.stage,
            relative_path: relative_path.to_string(),
            sha256,
            bytes,
            version: version.to_string(),
            created_unix: unix_now(),
        };
        let mut data = self.slot.data();
        if let Some(existing) = data
            .snapshot
            .artifacts
            .iter()
            .find(|a| a.relative_path == record.relative_path)
        {
            if existing.sha256 == record.sha256 && existing.bytes == record.bytes {
                return Ok(existing.clone());
            }
            return Err(StageFailure::failed(
                "E_ARTIFACT_IMMUTABLE",
                format!(
                    "artifact {} already registered with different content",
                    record.relative_path
                ),
            ));
        }
        if data.snapshot.artifacts.len() >= MAX_ARTIFACTS {
            return Err(StageFailure::failed(
                "E_ARTIFACT_LIMIT",
                format!("artifact count exceeds {MAX_ARTIFACTS}"),
            ));
        }
        data.snapshot.artifacts.push(record.clone());
        data.snapshot.updated_unix = unix_now();
        drop(data);
        self.slot.cv.notify_all();
        Ok(record)
    }

    /// apply stage 成功后设置实际 applied version（同时置 applied_by_job=true）。
    pub fn set_applied_version(&self, version: impl Into<String>) {
        let version = version.into();
        let mut data = self.slot.data();
        data.snapshot.applied_version = Some(version);
        data.snapshot.applied_by_job = true;
        data.snapshot.updated_unix = unix_now();
        drop(data);
        self.slot.cv.notify_all();
    }

    /// apply stage 开始时记录 apply 前的 last-good version（用于回滚证据）。
    pub fn set_last_good_version(&self, version: impl Into<String>) {
        let version = version.into();
        let mut data = self.slot.data();
        data.snapshot.last_good_version = Some(version);
        data.snapshot.updated_unix = unix_now();
        drop(data);
        self.slot.cv.notify_all();
    }
}

/// 校准 job 的 stage backend（T61–T66 的复合实现 / fake backend 单测实现）。
///
/// 约定：
/// - 只在本 job 的 worker 线程（NONRT）调用；禁止触碰 RT 图。
/// - 每个 stage 必须在可控点检查 ctx.cancel_token()（长扫频/采集至少每个 chunk 检查），
///   取消时返回 StageFailure::cancelled；
/// - **stage 必须可重入/幂等**：checkpoint 粒度是 stage，进程崩溃后 resume 会重跑 checkpoint 记录的
///   in-progress stage；不得依赖上一次进程的临时状态（磁盘 artifact 可以复用/覆盖同内容）。
/// - apply stage 必须在真正 commit+ack 后才调用 ctx.set_applied_version；准备失败不得设版本。
/// - 本 trait 不提供任何启动 CamillaDSP/旧 C 进程的能力（DoD：失败只 last-good/mute）。
pub trait CalibrationJobBackend: Send {
    /// 执行一个 stage；Ok=成功，Err=失败/取消（cancelled 标志决定终态映射）。
    fn run_stage(&mut self, stage: JobStage, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure>;
}

// ===== SHA-256（无第三方依赖；artifact/checkpoint 内容 hash） =====

const SHA256_K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// 计算 SHA-256 并返回小写 hex（64 字符）。用于 artifact/checkpoint/输入 hash。
pub fn sha256_hex(data: &[u8]) -> String {
    let mut state: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut message = Vec::with_capacity(data.len() + 72);
    message.extend_from_slice(data);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut a = state[0];
        let mut b = state[1];
        let mut c = state[2];
        let mut d = state[3];
        let mut e = state[4];
        let mut f = state[5];
        let mut g = state[6];
        let mut h = state[7];
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }
    let mut out = String::with_capacity(64);
    for word in state {
        out.push_str(&format!("{:08x}", word));
    }
    out
}

/// 计算文件内容 SHA-256（小写 hex）。
pub fn sha256_file(path: impl AsRef<Path>) -> io::Result<String> {
    let bytes = fs::read(path)?;
    Ok(sha256_hex(&bytes))
}

// ===== checkpoint 持久化 =====

/// checkpoint 读写/解析错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointError {
    /// 人读说明。
    pub message: String,
}

impl CheckpointError {
    fn malformed(message: impl Into<String>) -> Self {
        CheckpointError {
            message: message.into(),
        }
    }

    fn io(context: impl fmt::Display, error: impl fmt::Display) -> Self {
        CheckpointError {
            message: format!("{context}: {error}"),
        }
    }
}

impl fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CheckpointError {}

/// checkpoint 的完整可恢复状态（公开字段便于 daemon/recovery 工具检查；格式不是 REST 契约）。
#[derive(Debug, Clone, PartialEq)]
pub struct JobCheckpoint {
    /// job id。
    pub job_id: JobId,
    /// job 类型。
    pub kind: JobKind,
    /// 状态（active stage / queued / 终态）。
    pub state: JobState,
    /// 当前 stage 在 plan 中下标（queued=0；终态=plan.len）。
    pub stage_index: u32,
    /// 本 job 的 stage 计划（必须与 kind.plan() 一致）。
    pub plan: Vec<JobStage>,
    /// job 请求参数。
    pub request: JobRequest,
    /// artifact 根目录。
    pub artifacts_dir: PathBuf,
    /// checkpoint 时是否仍持有校准设备（恢复时需先释放）。
    pub device_held: bool,
    /// 是否停在 prepared 等 apply。
    pub awaiting_apply: bool,
    /// 本 job 是否 apply 过新 set。
    pub applied_by_job: bool,
    /// applied version。
    pub applied_version: Option<String>,
    /// last-good version。
    pub last_good_version: Option<String>,
    /// 创建时间。
    pub created_unix: u64,
    /// 最近更新时间。
    pub updated_unix: u64,
    /// 开始执行时间。
    pub started_unix: Option<u64>,
    /// 终态时间。
    pub finished_unix: Option<u64>,
    /// artifact 登记。
    pub artifacts: Vec<ArtifactRecord>,
    /// 恢复报告（终态）。
    pub recovery: Option<RecoveryReport>,
    /// 失败信息。
    pub failure: Option<JobFailure>,
    /// 终态原因。
    pub terminal_reason: Option<String>,
    /// 证据写失败记录。
    pub evidence_errors: Vec<String>,
}

impl JobCheckpoint {
    /// 从 job 快照构造 checkpoint。
    pub fn from_snapshot(snapshot: &JobSnapshot, artifacts_dir: &Path, device_held: bool) -> Self {
        JobCheckpoint {
            job_id: snapshot.job_id,
            kind: snapshot.kind,
            state: snapshot.state,
            stage_index: snapshot.stage_index,
            plan: snapshot.request.plan().to_vec(),
            request: snapshot.request.clone(),
            artifacts_dir: artifacts_dir.to_path_buf(),
            device_held,
            awaiting_apply: snapshot.awaiting_apply,
            applied_by_job: snapshot.applied_by_job,
            applied_version: snapshot.applied_version.clone(),
            last_good_version: snapshot.last_good_version.clone(),
            created_unix: snapshot.created_unix,
            updated_unix: snapshot.updated_unix,
            started_unix: snapshot.started_unix,
            finished_unix: snapshot.finished_unix,
            artifacts: snapshot.artifacts.clone(),
            recovery: snapshot.recovery.clone(),
            failure: snapshot.failure.clone(),
            terminal_reason: snapshot.terminal_reason.clone(),
            evidence_errors: snapshot.evidence_errors.clone(),
        }
    }

    /// 还原为快照（resume/restart status 用；stage_progress 从 0 起，终态 progress=1）。
    pub fn to_snapshot(&self) -> JobSnapshot {
        let stage_count = self.plan.len().max(1) as u32;
        let terminal = self.state.is_terminal();
        JobSnapshot {
            schema: JOB_SCHEMA,
            job_id: self.job_id,
            kind: self.kind,
            state: self.state,
            stage: self.state.active_stage(),
            stage_index: if terminal {
                stage_count
            } else {
                self.stage_index.min(stage_count)
            },
            stage_count,
            stage_progress: if terminal { 1.0 } else { 0.0 },
            overall_progress: if terminal {
                1.0
            } else {
                let index = self.stage_index.min(stage_count);
                index as f32 / stage_count as f32
            },
            awaiting_apply: self.awaiting_apply,
            cancel_requested: false,
            created_unix: self.created_unix,
            updated_unix: self.updated_unix,
            started_unix: self.started_unix,
            finished_unix: self.finished_unix,
            artifacts_dir: self.artifacts_dir.to_string_lossy().to_string(),
            request: self.request.clone(),
            applied_by_job: self.applied_by_job,
            applied_version: self.applied_version.clone(),
            last_good_version: self.last_good_version.clone(),
            recovery: self.recovery.clone(),
            failure: self.failure.clone(),
            evidence_errors: self.evidence_errors.clone(),
            terminal_reason: self.terminal_reason.clone(),
            artifacts: self.artifacts.clone(),
        }
    }

    fn validate(&self) -> Result<(), CheckpointError> {
        if self.plan.is_empty() {
            return Err(CheckpointError::malformed("checkpoint plan is empty"));
        }
        if self.plan.as_slice() != self.kind.plan() {
            return Err(CheckpointError::malformed(
                "checkpoint plan does not match job kind",
            ));
        }
        if self.request.kind != self.kind {
            return Err(CheckpointError::malformed(
                "checkpoint request.kind does not match kind",
            ));
        }
        self.request
            .validate()
            .map_err(|message| CheckpointError::malformed(format!("invalid request: {message}")))?;
        if !self.state.is_terminal()
            && !matches!(self.state, JobState::Queued | JobState::Active(_))
        {
            return Err(CheckpointError::malformed("invalid checkpoint state"));
        }
        if self.state.is_terminal() && self.finished_unix.is_none() {
            return Err(CheckpointError::malformed(
                "terminal checkpoint missing finished_unix",
            ));
        }
        if let Some(stage) = self.state.active_stage() {
            let position = self.stage_index as usize;
            if position >= self.plan.len() || self.plan[position] != stage {
                return Err(CheckpointError::malformed(
                    "active stage does not match checkpoint plan",
                ));
            }
        }
        Ok(())
    }
}

/// 文件 checkpoint store：确定性文本格式 + 原子写（tmp+rename；Windows 下先删目标再 rename）。
pub struct FileCheckpointStore {
    path: PathBuf,
}

impl FileCheckpointStore {
    /// 指定 checkpoint 文件路径（父目录必须已存在；save 不会自动创建目录，便于暴露 IO 故障）。
    pub fn new(path: impl Into<PathBuf>) -> Self {
        FileCheckpointStore { path: path.into() }
    }

    /// checkpoint 文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 读取 checkpoint；不存在返回 Ok(None)。
    pub fn load(&self) -> Result<Option<JobCheckpoint>, CheckpointError> {
        match fs::read(&self.path) {
            Ok(bytes) => {
                let text = String::from_utf8(bytes).map_err(|e| {
                    CheckpointError::io(
                        format!("checkpoint {} is not utf-8", self.path.display()),
                        e,
                    )
                })?;
                Ok(Some(decode_checkpoint(&text)?))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(CheckpointError::io(
                format!("read checkpoint {}", self.path.display()),
                error,
            )),
        }
    }

    /// 原子写 checkpoint。
    pub fn save(&self, checkpoint: &JobCheckpoint) -> Result<(), CheckpointError> {
        checkpoint.validate()?;
        let text = encode_checkpoint(checkpoint);
        atomic_write(&self.path, text.as_bytes()).map_err(|e| {
            CheckpointError::io(format!("write checkpoint {}", self.path.display()), e)
        })
    }

    /// 删除 checkpoint（恢复完成后清理）。
    pub fn clear(&self) -> Result<(), CheckpointError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(CheckpointError::io(
                format!("remove checkpoint {}", self.path.display()),
                error,
            )),
        }
    }
}

fn encode_checkpoint(checkpoint: &JobCheckpoint) -> String {
    let mut out = String::new();
    out.push_str(CHECKPOINT_SCHEMA);
    out.push('\n');
    macro_rules! push {
        ($key:expr, $value:expr $(,)?) => {{
            out.push_str($key);
            out.push('=');
            out.push_str(&escape_checkpoint_field($value));
            out.push('\n');
        }};
    }
    push!("job_id", &checkpoint.job_id.0.to_string());
    push!("kind", checkpoint.kind.as_str());
    push!("state", &checkpoint_state_encode(checkpoint.state));
    push!("stage_index", &checkpoint.stage_index.to_string());
    push!(
        "plan",
        &checkpoint
            .plan
            .iter()
            .map(|stage| stage.as_str())
            .collect::<Vec<_>>()
            .join(","),
    );
    push!("artifacts_dir", &checkpoint.artifacts_dir.to_string_lossy(),);
    push!("device_held", bool_str(checkpoint.device_held));
    push!("awaiting_apply", bool_str(checkpoint.awaiting_apply));
    push!("applied_by_job", bool_str(checkpoint.applied_by_job));
    push!(
        "applied_version",
        checkpoint.applied_version.as_deref().unwrap_or(""),
    );
    push!(
        "last_good_version",
        checkpoint.last_good_version.as_deref().unwrap_or(""),
    );
    push!("created_unix", &checkpoint.created_unix.to_string());
    push!("updated_unix", &checkpoint.updated_unix.to_string());
    push!(
        "started_unix",
        &checkpoint
            .started_unix
            .map(|v| v.to_string())
            .unwrap_or_default(),
    );
    push!(
        "finished_unix",
        &checkpoint
            .finished_unix
            .map(|v| v.to_string())
            .unwrap_or_default(),
    );
    push!(
        "terminal_reason",
        checkpoint.terminal_reason.as_deref().unwrap_or("")
    );
    push!("request.kind", checkpoint.request.kind.as_str());
    push!("request.label", &checkpoint.request.label);
    push!("request.layout", &checkpoint.request.layout);
    push!(
        "request.sample_rate",
        &checkpoint.request.sample_rate.to_string(),
    );
    push!(
        "request.design_points",
        &checkpoint.request.design_points.to_string(),
    );
    push!(
        "request.holdout_points",
        &checkpoint.request.holdout_points.to_string(),
    );
    push!("request.repeats", &checkpoint.request.repeats.to_string());
    push!(
        "request.drive_level_dbfs",
        &checkpoint.request.drive_level_dbfs.to_string(),
    );
    push!(
        "request.audible_sweep_authorized",
        bool_str(checkpoint.request.audible_sweep_authorized),
    );
    push!(
        "request.auto_apply",
        bool_str(checkpoint.request.auto_apply)
    );
    push!(
        "request.expected_applied_version",
        checkpoint
            .request
            .expected_applied_version
            .as_deref()
            .unwrap_or(""),
    );
    for input in &checkpoint.request.inputs {
        out.push_str("request.input=");
        out.push_str(&escape_checkpoint_field(&input.name));
        out.push('\t');
        out.push_str(&escape_checkpoint_field(&input.sha256));
        out.push('\n');
    }
    for artifact in &checkpoint.artifacts {
        out.push_str("artifact=");
        let fields = [
            artifact.kind.checkpoint_encode(),
            artifact.stage.as_str().to_string(),
            artifact.relative_path.clone(),
            artifact.sha256.clone(),
            artifact.bytes.to_string(),
            artifact.version.clone(),
            artifact.created_unix.to_string(),
        ];
        out.push_str(
            &fields
                .iter()
                .map(|field| escape_checkpoint_field(field))
                .collect::<Vec<_>>()
                .join("\t"),
        );
        out.push('\n');
    }
    if let Some(recovery) = &checkpoint.recovery {
        push!("recovery.outcome", recovery.outcome.as_str());
        push!(
            "recovery.version",
            match &recovery.outcome {
                RecoveryOutcome::LastGood { version } => version,
                _ => "",
            },
        );
        push!(
            "recovery.error",
            match &recovery.outcome {
                RecoveryOutcome::Failed { error } => error,
                _ => "",
            },
        );
        push!("recovery.reason", &recovery.reason);
        push!(
            "recovery.device_released",
            bool_str(recovery.device_released)
        );
        push!("recovery.at_unix", &recovery.at_unix.to_string());
    }
    if let Some(failure) = &checkpoint.failure {
        push!("failure.code", &failure.code);
        push!("failure.message", &failure.message);
        push!(
            "failure.stage",
            failure.stage.map(|stage| stage.as_str()).unwrap_or(""),
        );
    }
    for error in &checkpoint.evidence_errors {
        push!("evidence_error", error);
    }
    out
}

fn decode_checkpoint(text: &str) -> Result<JobCheckpoint, CheckpointError> {
    let mut lines = text.lines();
    let header = lines
        .next()
        .ok_or_else(|| CheckpointError::malformed("empty checkpoint"))?
        .trim_end();
    if header != CHECKPOINT_SCHEMA {
        return Err(CheckpointError::malformed(format!(
            "unexpected checkpoint schema: {header}"
        )));
    }
    let mut job_id: Option<JobId> = None;
    let mut kind: Option<JobKind> = None;
    let mut state: Option<JobState> = None;
    let mut stage_index: Option<u32> = None;
    let mut plan: Option<Vec<JobStage>> = None;
    let mut artifacts_dir: Option<PathBuf> = None;
    let mut device_held = false;
    let mut awaiting_apply = false;
    let mut applied_by_job = false;
    let mut applied_version: Option<String> = None;
    let mut last_good_version: Option<String> = None;
    let mut created_unix: Option<u64> = None;
    let mut updated_unix: Option<u64> = None;
    let mut started_unix: Option<u64> = None;
    let mut finished_unix: Option<u64> = None;
    let mut terminal_reason: Option<String> = None;
    let mut request_kind: Option<JobKind> = None;
    let mut request_label: Option<String> = None;
    let mut request_layout: Option<String> = None;
    let mut request_sample_rate: Option<u32> = None;
    let mut request_design_points: Option<u32> = None;
    let mut request_holdout_points: Option<u32> = None;
    let mut request_repeats: Option<u32> = None;
    let mut request_drive_level: Option<f32> = None;
    let mut request_audible = false;
    let mut request_auto_apply = false;
    let mut request_expected_version: Option<String> = None;
    let mut inputs: Vec<InputHash> = Vec::new();
    let mut artifacts: Vec<ArtifactRecord> = Vec::new();
    let mut recovery_outcome: Option<String> = None;
    let mut recovery_version: Option<String> = None;
    let mut recovery_error: Option<String> = None;
    let mut recovery_reason: Option<String> = None;
    let mut recovery_device_released = false;
    let mut recovery_at_unix: Option<u64> = None;
    let mut failure_code: Option<String> = None;
    let mut failure_message: Option<String> = None;
    let mut failure_stage: Option<JobStage> = None;
    let mut evidence_errors: Vec<String> = Vec::new();

    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let (key, raw_value) = line.split_once('=').ok_or_else(|| {
            CheckpointError::malformed(format!("checkpoint line without '=': {line}"))
        })?;
        let value = if key == "artifact" || key == "request.input" {
            raw_value.to_string()
        } else {
            unescape_checkpoint_field(raw_value)?
        };
        match key {
            "job_id" => {
                job_id = Some(JobId(value.parse().map_err(|_| {
                    CheckpointError::malformed(format!("invalid job_id: {value}"))
                })?))
            }
            "kind" => {
                kind =
                    Some(JobKind::parse(&value).ok_or_else(|| {
                        CheckpointError::malformed(format!("invalid kind: {value}"))
                    })?)
            }
            "state" => {
                state = Some(checkpoint_state_decode(&value)?);
            }
            "stage_index" => {
                stage_index = Some(value.parse().map_err(|_| {
                    CheckpointError::malformed(format!("invalid stage_index: {value}"))
                })?)
            }
            "plan" => {
                let mut stages = Vec::new();
                for part in value.split(',') {
                    let stage = JobStage::parse(part.trim()).ok_or_else(|| {
                        CheckpointError::malformed(format!("invalid plan stage: {part}"))
                    })?;
                    stages.push(stage);
                }
                plan = Some(stages);
            }
            "artifacts_dir" => artifacts_dir = Some(PathBuf::from(value)),
            "device_held" => device_held = parse_bool(&value, key)?,
            "awaiting_apply" => awaiting_apply = parse_bool(&value, key)?,
            "applied_by_job" => applied_by_job = parse_bool(&value, key)?,
            "applied_version" => applied_version = non_empty(value),
            "last_good_version" => last_good_version = non_empty(value),
            "created_unix" => {
                created_unix = Some(value.parse().map_err(|_| {
                    CheckpointError::malformed(format!("invalid created_unix: {value}"))
                })?)
            }
            "updated_unix" => {
                updated_unix = Some(value.parse().map_err(|_| {
                    CheckpointError::malformed(format!("invalid updated_unix: {value}"))
                })?)
            }
            "started_unix" => {
                started_unix = if value.is_empty() {
                    None
                } else {
                    Some(value.parse().map_err(|_| {
                        CheckpointError::malformed(format!("invalid started_unix: {value}"))
                    })?)
                }
            }
            "finished_unix" => {
                finished_unix = if value.is_empty() {
                    None
                } else {
                    Some(value.parse().map_err(|_| {
                        CheckpointError::malformed(format!("invalid finished_unix: {value}"))
                    })?)
                }
            }
            "terminal_reason" => terminal_reason = non_empty(value),
            "request.kind" => {
                request_kind = Some(JobKind::parse(&value).ok_or_else(|| {
                    CheckpointError::malformed(format!("invalid request.kind: {value}"))
                })?)
            }
            "request.label" => request_label = Some(value),
            "request.layout" => request_layout = Some(value),
            "request.sample_rate" => {
                request_sample_rate = Some(value.parse().map_err(|_| {
                    CheckpointError::malformed(format!("invalid request.sample_rate: {value}"))
                })?)
            }
            "request.design_points" => {
                request_design_points = Some(value.parse().map_err(|_| {
                    CheckpointError::malformed(format!("invalid request.design_points: {value}"))
                })?)
            }
            "request.holdout_points" => {
                request_holdout_points = Some(value.parse().map_err(|_| {
                    CheckpointError::malformed(format!("invalid request.holdout_points: {value}"))
                })?)
            }
            "request.repeats" => {
                request_repeats = Some(value.parse().map_err(|_| {
                    CheckpointError::malformed(format!("invalid request.repeats: {value}"))
                })?)
            }
            "request.drive_level_dbfs" => {
                request_drive_level = Some(value.parse().map_err(|_| {
                    CheckpointError::malformed(format!("invalid request.drive_level_dbfs: {value}"))
                })?)
            }
            "request.audible_sweep_authorized" => request_audible = parse_bool(&value, key)?,
            "request.auto_apply" => request_auto_apply = parse_bool(&value, key)?,
            "request.expected_applied_version" => request_expected_version = non_empty(value),
            "request.input" => {
                let (name, sha) = raw_value
                    .split_once('\t')
                    .ok_or_else(|| CheckpointError::malformed("invalid request.input line"))?;
                inputs.push(InputHash {
                    name: unescape_checkpoint_field(name)?,
                    sha256: unescape_checkpoint_field(sha)?,
                });
            }
            "artifact" => artifacts.push(decode_artifact(raw_value)?),
            "recovery.outcome" => recovery_outcome = non_empty(value),
            "recovery.version" => recovery_version = non_empty(value),
            "recovery.error" => recovery_error = non_empty(value),
            "recovery.reason" => recovery_reason = Some(value),
            "recovery.device_released" => recovery_device_released = parse_bool(&value, key)?,
            "recovery.at_unix" => {
                recovery_at_unix = if value.is_empty() {
                    None
                } else {
                    Some(value.parse().map_err(|_| {
                        CheckpointError::malformed(format!("invalid recovery.at_unix: {value}"))
                    })?)
                }
            }
            "failure.code" => failure_code = non_empty(value),
            "failure.message" => failure_message = Some(value),
            "failure.stage" => {
                failure_stage = if value.is_empty() {
                    None
                } else {
                    Some(JobStage::parse(&value).ok_or_else(|| {
                        CheckpointError::malformed(format!("invalid failure.stage: {value}"))
                    })?)
                }
            }
            "evidence_error" => evidence_errors.push(value),
            _ => {}
        }
    }

    let job_id = job_id.ok_or_else(|| CheckpointError::malformed("missing job_id"))?;
    let kind = kind.ok_or_else(|| CheckpointError::malformed("missing kind"))?;
    let state = state.ok_or_else(|| CheckpointError::malformed("missing state"))?;
    let stage_index = stage_index.unwrap_or(0);
    let plan = plan.ok_or_else(|| CheckpointError::malformed("missing plan"))?;
    let artifacts_dir =
        artifacts_dir.ok_or_else(|| CheckpointError::malformed("missing artifacts_dir"))?;
    let created_unix =
        created_unix.ok_or_else(|| CheckpointError::malformed("missing created_unix"))?;
    let updated_unix = updated_unix.unwrap_or(created_unix);
    let request = JobRequest {
        kind: request_kind.ok_or_else(|| CheckpointError::malformed("missing request.kind"))?,
        label: request_label.ok_or_else(|| CheckpointError::malformed("missing request.label"))?,
        layout: request_layout
            .ok_or_else(|| CheckpointError::malformed("missing request.layout"))?,
        sample_rate: request_sample_rate
            .ok_or_else(|| CheckpointError::malformed("missing request.sample_rate"))?,
        design_points: request_design_points
            .ok_or_else(|| CheckpointError::malformed("missing request.design_points"))?,
        holdout_points: request_holdout_points
            .ok_or_else(|| CheckpointError::malformed("missing request.holdout_points"))?,
        repeats: request_repeats
            .ok_or_else(|| CheckpointError::malformed("missing request.repeats"))?,
        drive_level_dbfs: request_drive_level
            .ok_or_else(|| CheckpointError::malformed("missing request.drive_level_dbfs"))?,
        audible_sweep_authorized: request_audible,
        auto_apply: request_auto_apply,
        expected_applied_version: request_expected_version,
        inputs,
    };
    let recovery = match recovery_outcome.as_deref() {
        None => None,
        Some("last_good") => Some(RecoveryReport {
            outcome: RecoveryOutcome::LastGood {
                version: recovery_version.unwrap_or_else(|| "last-good".to_string()),
            },
            reason: recovery_reason.unwrap_or_default(),
            device_released: recovery_device_released,
            at_unix: recovery_at_unix.unwrap_or(0),
        }),
        Some("mute") => Some(RecoveryReport {
            outcome: RecoveryOutcome::Mute,
            reason: recovery_reason.unwrap_or_default(),
            device_released: recovery_device_released,
            at_unix: recovery_at_unix.unwrap_or(0),
        }),
        Some("failed") => Some(RecoveryReport {
            outcome: RecoveryOutcome::Failed {
                error: recovery_error.unwrap_or_default(),
            },
            reason: recovery_reason.unwrap_or_default(),
            device_released: recovery_device_released,
            at_unix: recovery_at_unix.unwrap_or(0),
        }),
        Some(other) => {
            return Err(CheckpointError::malformed(format!(
                "invalid recovery.outcome: {other}"
            )))
        }
    };
    let failure = failure_code.map(|code| JobFailure {
        code,
        message: failure_message.unwrap_or_default(),
        stage: failure_stage,
    });
    let checkpoint = JobCheckpoint {
        job_id,
        kind,
        state,
        stage_index,
        plan,
        request,
        artifacts_dir,
        device_held,
        awaiting_apply,
        applied_by_job,
        applied_version,
        last_good_version,
        created_unix,
        updated_unix,
        started_unix,
        finished_unix,
        artifacts,
        recovery,
        failure,
        terminal_reason,
        evidence_errors,
    };
    checkpoint.validate()?;
    Ok(checkpoint)
}

fn decode_artifact(raw: &str) -> Result<ArtifactRecord, CheckpointError> {
    let fields: Vec<&str> = raw.split('\t').collect();
    if fields.len() != 7 {
        return Err(CheckpointError::malformed(format!(
            "artifact line has {} fields (expected 7)",
            fields.len()
        )));
    }
    let kind = ArtifactKind::checkpoint_parse(&unescape_checkpoint_field(fields[0])?)
        .ok_or_else(|| CheckpointError::malformed("invalid artifact kind"))?;
    let stage = JobStage::parse(&unescape_checkpoint_field(fields[1])?)
        .ok_or_else(|| CheckpointError::malformed("invalid artifact stage"))?;
    let relative_path = unescape_checkpoint_field(fields[2])?;
    let sha256 = unescape_checkpoint_field(fields[3])?;
    let bytes: u64 = unescape_checkpoint_field(fields[4])?
        .parse()
        .map_err(|_| CheckpointError::malformed("invalid artifact bytes"))?;
    let version = unescape_checkpoint_field(fields[5])?;
    let created_unix: u64 = unescape_checkpoint_field(fields[6])?
        .parse()
        .map_err(|_| CheckpointError::malformed("invalid artifact created_unix"))?;
    Ok(ArtifactRecord {
        kind,
        stage,
        relative_path,
        sha256,
        bytes,
        version,
        created_unix,
    })
}

fn checkpoint_state_encode(state: JobState) -> String {
    match state {
        JobState::Active(stage) => format!("active:{}", stage.as_str()),
        other => other.as_str().to_string(),
    }
}

fn checkpoint_state_decode(value: &str) -> Result<JobState, CheckpointError> {
    if let Some(stage_name) = value.strip_prefix("active:") {
        let stage = JobStage::parse(stage_name).ok_or_else(|| {
            CheckpointError::malformed(format!("invalid active stage: {stage_name}"))
        })?;
        return Ok(JobState::Active(stage));
    }
    JobState::parse(value)
        .ok_or_else(|| CheckpointError::malformed(format!("invalid job state: {value}")))
}

fn escape_checkpoint_field(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match *byte {
            b'%' => out.push_str("%25"),
            b'\t' => out.push_str("%09"),
            b'\n' => out.push_str("%0A"),
            b'\r' => out.push_str("%0D"),
            other => out.push(other as char),
        }
    }
    out
}

fn unescape_checkpoint_field(value: &str) -> Result<String, CheckpointError> {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(CheckpointError::malformed("truncated checkpoint escape"));
            }
            let hex = &value[index + 1..index + 3];
            let decoded = u8::from_str_radix(hex, 16)
                .map_err(|_| CheckpointError::malformed("invalid checkpoint escape"))?;
            out.push(decoded);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).map_err(|_| CheckpointError::malformed("checkpoint field is not utf-8"))
}

fn parse_bool(value: &str, key: &str) -> Result<bool, CheckpointError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(CheckpointError::malformed(format!(
            "invalid bool for {key}: {value}"
        ))),
    }
}

fn bool_str(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

fn non_empty(value: String) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

// ===== 内部：slot/worker/manager =====

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn overall_progress(stage_index: usize, stage_progress: f32, stage_count: usize) -> f32 {
    if stage_count == 0 {
        return 0.0;
    }
    let value = (stage_index as f32 + stage_progress.clamp(0.0, 1.0)) / stage_count as f32;
    value.clamp(0.0, 1.0)
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_artifact_path(relative_path: &str) -> Result<(), String> {
    if relative_path.is_empty() || relative_path.len() > 512 {
        return Err("artifact path must be non-empty and <=512 bytes".to_string());
    }
    if relative_path.starts_with('/')
        || relative_path.contains('\\')
        || relative_path.contains(':')
        || relative_path.contains('\0')
    {
        return Err(format!(
            "artifact path must be relative with '/' separators: {relative_path}"
        ));
    }
    for component in relative_path.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(format!("invalid artifact path component: {relative_path}"));
        }
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory")
    })?;
    if !parent.as_os_str().is_empty() && !parent.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("directory does not exist: {}", parent.display()),
        ));
    }
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state");
    let temp = parent.join(format!(
        ".{}.tmp.{}.{}",
        file_name,
        std::process::id(),
        counter
    ));
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    match fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(first_error) => {
            // Windows 的 rename 覆盖已存在目标会失败：先删目标再 rename（仍保持 tmp 完整写入）。
            let _ = fs::remove_file(&temp);
            match fs::remove_file(path) {
                Ok(()) | Err(_) => {}
            }
            let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let temp_retry = parent.join(format!(
                ".{}.tmp.{}.{}",
                file_name,
                std::process::id(),
                counter
            ));
            {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&temp_retry)?;
                file.write_all(bytes)?;
                file.sync_all()?;
            }
            fs::rename(&temp_retry, path).map_err(|second_error| {
                let _ = fs::remove_file(&temp_retry);
                io::Error::new(
                    second_error.kind(),
                    format!(
                        "atomic write {} failed: {second_error} (first rename: {first_error})",
                        path.display()
                    ),
                )
            })
        }
    }
}

fn scan_next_job_id(artifacts_root: &Path) -> u64 {
    let mut next = 1;
    if let Ok(entries) = fs::read_dir(artifacts_root) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if let Some(number) = name.strip_prefix("job-") {
                    if let Ok(value) = number.parse::<u64>() {
                        next = next.max(value + 1);
                    }
                }
            }
        }
    }
    next
}

fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
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

fn json_string(value: &str) -> String {
    format!("\"{}\"", json_escape(value))
}

fn json_opt_string(value: &Option<String>) -> String {
    match value {
        Some(value) => json_string(value),
        None => "null".to_string(),
    }
}

fn json_opt_u64(value: Option<u64>) -> String {
    match value {
        Some(value) => value.to_string(),
        None => "null".to_string(),
    }
}

fn json_opt_stage(value: Option<JobStage>) -> String {
    match value {
        Some(stage) => json_string(stage.as_str()),
        None => "null".to_string(),
    }
}

fn json_f32(value: f32) -> String {
    if value.is_finite() {
        format!("{value}")
    } else {
        "null".to_string()
    }
}

fn request_json(request: &JobRequest) -> String {
    let inputs = request
        .inputs
        .iter()
        .map(|input| {
            format!(
                "{{\"name\":{},\"sha256\":{}}}",
                json_string(&input.name),
                json_string(&input.sha256)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        concat!(
            "{{",
            "\"kind\":{},",
            "\"label\":{},",
            "\"layout\":{},",
            "\"sample_rate\":{},",
            "\"design_points\":{},",
            "\"holdout_points\":{},",
            "\"repeats\":{},",
            "\"drive_level_dbfs\":{},",
            "\"audible_sweep_authorized\":{},",
            "\"auto_apply\":{},",
            "\"expected_applied_version\":{},",
            "\"inputs\":[{}]",
            "}}"
        ),
        json_string(request.kind.as_str()),
        json_string(&request.label),
        json_string(&request.layout),
        request.sample_rate,
        request.design_points,
        request.holdout_points,
        request.repeats,
        json_f32(request.drive_level_dbfs),
        request.audible_sweep_authorized,
        request.auto_apply,
        json_opt_string(&request.expected_applied_version),
        inputs
    )
}

fn artifact_json(artifact: &ArtifactRecord) -> String {
    let kind_detail = match &artifact.kind {
        ArtifactKind::Other(name) => json_string(name),
        _ => "null".to_string(),
    };
    format!(
        concat!(
            "{{",
            "\"kind\":{},",
            "\"kind_detail\":{},",
            "\"stage\":{},",
            "\"relative_path\":{},",
            "\"sha256\":{},",
            "\"bytes\":{},",
            "\"version\":{},",
            "\"created_unix\":{}",
            "}}"
        ),
        json_string(artifact.kind.as_str()),
        kind_detail,
        json_string(artifact.stage.as_str()),
        json_string(&artifact.relative_path),
        json_string(&artifact.sha256),
        artifact.bytes,
        json_string(&artifact.version),
        artifact.created_unix
    )
}

fn recovery_json(recovery: Option<&RecoveryReport>) -> String {
    let Some(recovery) = recovery else {
        return "null".to_string();
    };
    let (version, error) = match &recovery.outcome {
        RecoveryOutcome::LastGood { version } => (json_string(version), "null".to_string()),
        RecoveryOutcome::Mute => ("null".to_string(), "null".to_string()),
        RecoveryOutcome::Failed { error } => ("null".to_string(), json_string(error)),
    };
    format!(
        concat!(
            "{{",
            "\"outcome\":{},",
            "\"version\":{},",
            "\"error\":{},",
            "\"reason\":{},",
            "\"device_released\":{},",
            "\"at_unix\":{}",
            "}}"
        ),
        json_string(recovery.outcome.as_str()),
        version,
        error,
        json_string(&recovery.reason),
        recovery.device_released,
        recovery.at_unix
    )
}

fn failure_json(failure: Option<&JobFailure>) -> String {
    let Some(failure) = failure else {
        return "null".to_string();
    };
    format!(
        "{{\"code\":{},\"message\":{},\"stage\":{}}}",
        json_string(&failure.code),
        json_string(&failure.message),
        json_opt_stage(failure.stage)
    )
}

struct JobSlotShared {
    data: Mutex<SlotData>,
    cv: Condvar,
}

impl JobSlotShared {
    fn new(data: SlotData) -> Self {
        JobSlotShared {
            data: Mutex::new(data),
            cv: Condvar::new(),
        }
    }

    fn data(&self) -> MutexGuard<'_, SlotData> {
        self.data
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

struct SlotData {
    snapshot: JobSnapshot,
    plan: &'static [JobStage],
    stage_pos: usize,
    awaiting_apply: bool,
    apply_granted: bool,
    rollback_requested: bool,
    cancel: CancelToken,
    completed: bool,
    device_held: bool,
    artifacts_dir: PathBuf,
}

struct ActiveHandle {
    slot: Arc<JobSlotShared>,
    join: Option<JoinHandle<()>>,
}

struct ManagerState {
    next_id: u64,
    active: Option<ActiveHandle>,
    last_finished: Option<JobSnapshot>,
    pending: Option<JobCheckpoint>,
}

/// JobManager 配置。
#[derive(Debug, Clone)]
pub struct JobManagerConfig {
    /// artifact 根目录（每 job 使用 artifacts_root/job-NNNN）。
    pub artifacts_root: PathBuf,
    /// checkpoint 文件路径；None=不持久化（仅测试/无状态模式，生产必须配置）。
    pub checkpoint_path: Option<PathBuf>,
    /// prepared -> apply 的确认超时；None=无限等待（显式 apply/取消）。超时按失败处理并恢复 last-good。
    pub apply_confirm_timeout: Option<Duration>,
}

impl Default for JobManagerConfig {
    fn default() -> Self {
        JobManagerConfig {
            artifacts_root: std::env::temp_dir().join("khaen-calibration-jobs"),
            checkpoint_path: None,
            apply_confirm_timeout: None,
        }
    }
}

struct ManagerInner {
    cfg: JobManagerConfig,
    safety: Arc<dyn SafeOutputController>,
    state: Mutex<ManagerState>,
}

/// 校准 job 管理器：单 job 互斥 + worker 线程 + checkpoint 恢复入口（daemon 单例使用）。
pub struct JobManager {
    inner: Arc<ManagerInner>,
}

impl JobManager {
    /// 新建 manager：创建 artifacts 根/checkpoint 父目录，并加载已有 checkpoint（restart 恢复入口）。
    pub fn new(
        config: JobManagerConfig,
        safety: Arc<dyn SafeOutputController>,
    ) -> Result<Self, JobError> {
        if config.artifacts_root.as_os_str().is_empty() {
            return Err(JobError::InvalidRequest(
                "artifacts_root must be non-empty".to_string(),
            ));
        }
        fs::create_dir_all(&config.artifacts_root).map_err(|e| {
            JobError::Io(format!(
                "create artifacts root {}: {e}",
                config.artifacts_root.display()
            ))
        })?;
        let mut next_id = scan_next_job_id(&config.artifacts_root);
        let mut last_finished = None;
        let mut pending = None;
        if let Some(checkpoint_path) = &config.checkpoint_path {
            if let Some(parent) = checkpoint_path.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent).map_err(|e| {
                        JobError::Io(format!("create checkpoint dir {}: {e}", parent.display()))
                    })?;
                }
            }
            let store = FileCheckpointStore::new(checkpoint_path.clone());
            match store.load().map_err(|e| JobError::Checkpoint(e.message))? {
                Some(checkpoint) => {
                    next_id = next_id.max(checkpoint.job_id.0 + 1);
                    if checkpoint.state.is_terminal() {
                        last_finished = Some(checkpoint.to_snapshot());
                    } else {
                        pending = Some(checkpoint);
                    }
                }
                None => {}
            }
        }
        Ok(JobManager {
            inner: Arc::new(ManagerInner {
                cfg: config,
                safety,
                state: Mutex::new(ManagerState {
                    next_id,
                    active: None,
                    last_finished,
                    pending,
                }),
            }),
        })
    }

    /// manager 配置。
    pub fn config(&self) -> &JobManagerConfig {
        &self.inner.cfg
    }

    /// 启动 Full 或 Retest job（非阻塞；worker 线程执行 stage）。已有 active job -> Busy。
    pub fn start(
        &self,
        request: JobRequest,
        backend: Box<dyn CalibrationJobBackend>,
    ) -> Result<JobId, JobError> {
        request.validate().map_err(JobError::InvalidRequest)?;
        self.reap_finished();
        let mut state = self.lock_state();
        if state.pending.is_some() {
            return Err(JobError::ResumePending);
        }
        if state.active.is_some() {
            return Err(JobError::Busy);
        }
        let job_id = JobId(state.next_id);
        state.next_id += 1;
        let plan = request.kind.plan();
        let artifacts_dir = self
            .inner
            .cfg
            .artifacts_root
            .join(job_id.artifact_dir_name());
        fs::create_dir_all(&artifacts_dir).map_err(|e| {
            JobError::Io(format!(
                "create job artifacts dir {}: {e}",
                artifacts_dir.display()
            ))
        })?;
        let snapshot = JobSnapshot::new(job_id, &request, &artifacts_dir);
        let slot = Arc::new(JobSlotShared::new(SlotData {
            snapshot,
            plan,
            stage_pos: 0,
            awaiting_apply: false,
            apply_granted: false,
            rollback_requested: false,
            cancel: CancelToken::new(),
            completed: false,
            device_held: false,
            artifacts_dir: artifacts_dir.clone(),
        }));
        if let Err(error) = self.save_checkpoint_for(&slot) {
            let _ = fs::remove_dir_all(&artifacts_dir);
            return Err(error);
        }
        let inner = self.inner.clone();
        let worker_slot = slot.clone();
        let handle = thread::Builder::new()
            .name(format!("khaen-cal-job-{}", job_id.0))
            .spawn(move || worker_main(inner, worker_slot, backend))
            .map_err(|e| JobError::Thread(e.to_string()))?;
        state.active = Some(ActiveHandle {
            slot,
            join: Some(handle),
        });
        Ok(job_id)
    }

    /// 当前/最近一个 job 的快照（active 优先，否则 last_finished）。
    pub fn snapshot(&self) -> Option<JobSnapshot> {
        self.reap_finished();
        let slot = {
            let state = self.lock_state();
            state.active.as_ref().map(|active| active.slot.clone())
        };
        if let Some(slot) = slot {
            return Some(slot.data().snapshot.clone());
        }
        self.lock_state().last_finished.clone()
    }

    /// 最近一个终态 job 的快照。
    pub fn last_finished(&self) -> Option<JobSnapshot> {
        self.reap_finished();
        self.lock_state().last_finished.clone()
    }

    /// 等待 active job 到终态（2ms 轮询；返回终态快照）。无 active 但有 last_finished 时直接返回。
    pub fn wait_terminal(&self, timeout: Duration) -> Result<JobSnapshot, JobError> {
        let deadline = Instant::now() + timeout;
        loop {
            self.reap_finished();
            {
                let state = self.lock_state();
                if let Some(active) = &state.active {
                    let data = active.slot.data();
                    if data.completed {
                        return Ok(data.snapshot.clone());
                    }
                } else if let Some(last) = &state.last_finished {
                    return Ok(last.clone());
                } else {
                    return Err(JobError::NoActiveJob);
                }
            }
            if Instant::now() >= deadline {
                return Err(JobError::Timeout);
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    /// 请求取消 active job（幂等；stage 内由 backend 协作检查）。
    pub fn cancel(&self) -> Result<JobId, JobError> {
        let slot = self.active_slot()?;
        let mut data = slot.data();
        if data.completed || data.snapshot.state.is_terminal() {
            return Err(JobError::InvalidState("job already finished".to_string()));
        }
        data.cancel.request();
        data.snapshot.cancel_requested = true;
        data.snapshot.updated_unix = unix_now();
        let job_id = data.snapshot.job_id;
        drop(data);
        slot.cv.notify_all();
        Ok(job_id)
    }

    /// 放行 prepared 的 apply（幂等：重复 apply 返回同一 job）。
    pub fn apply(&self) -> Result<JobId, JobError> {
        let slot = self.active_slot()?;
        let mut data = slot.data();
        if data.completed || data.snapshot.state.is_terminal() {
            return Err(JobError::InvalidState("job already finished".to_string()));
        }
        if !data.snapshot.awaiting_apply {
            // 幂等：已经放行/已经 apply/已过 prepared 的重复 apply 返回同一 job，不二次提交。
            let already_applied = data.snapshot.applied_by_job
                || data.apply_granted
                || data.stage_pos > JobStage::Prepared.index();
            if already_applied {
                return Ok(data.snapshot.job_id);
            }
            return Err(JobError::InvalidState(format!(
                "apply requires the job to wait at prepared (current state: {})",
                data.snapshot.state.as_str()
            )));
        }
        data.apply_granted = true;
        data.snapshot.updated_unix = unix_now();
        let job_id = data.snapshot.job_id;
        drop(data);
        slot.cv.notify_all();
        Ok(job_id)
    }

    /// 请求回滚：prepared（未 apply）-> 安全 abort 并回 last-good；apply 后 -> 回滚 applied set 并回 last-good。
    pub fn rollback(&self) -> Result<JobId, JobError> {
        let slot = self.active_slot()?;
        let mut data = slot.data();
        if data.completed || data.snapshot.state.is_terminal() {
            return Err(JobError::InvalidState("job already finished".to_string()));
        }
        if data.snapshot.kind != JobKind::Full {
            return Err(JobError::InvalidState(
                "rollback is only valid for a full calibration job".to_string(),
            ));
        }
        data.rollback_requested = true;
        data.cancel.request();
        data.snapshot.cancel_requested = true;
        data.snapshot.updated_unix = unix_now();
        let job_id = data.snapshot.job_id;
        drop(data);
        slot.cv.notify_all();
        Ok(job_id)
    }

    /// restart 后待恢复的 job 快照（None=没有 pending）。
    pub fn pending_resume(&self) -> Option<JobSnapshot> {
        self.lock_state()
            .pending
            .as_ref()
            .map(JobCheckpoint::to_snapshot)
    }

    /// 从 checkpoint 恢复 pending job（重跑 checkpoint 记录的 in-progress stage）。
    pub fn resume(&self, backend: Box<dyn CalibrationJobBackend>) -> Result<JobId, JobError> {
        let mut state = self.lock_state();
        if state.active.is_some() {
            return Err(JobError::Busy);
        }
        let checkpoint = state.pending.take().ok_or(JobError::NoPendingResume)?;
        if checkpoint.device_held {
            // 上一个进程崩溃时可能仍持有设备：恢复前先释放（失败由后续 start 的 acquire 再报）。
            let _ = self
                .inner
                .safety
                .release_calibration_owner(ExitDisposition::ResumePlayback);
        }
        fs::create_dir_all(&checkpoint.artifacts_dir).map_err(|e| {
            JobError::Io(format!(
                "create restored artifacts dir {}: {e}",
                checkpoint.artifacts_dir.display()
            ))
        })?;
        let snapshot = checkpoint.to_snapshot();
        let plan = checkpoint.kind.plan();
        let stage_pos = match checkpoint.state {
            JobState::Queued => 0,
            JobState::Active(stage) => {
                match checkpoint.kind.plan().iter().position(|s| *s == stage) {
                    Some(position) => position,
                    None => {
                        state.pending = Some(checkpoint);
                        return Err(JobError::Checkpoint(
                            "resume stage is not in the job plan".to_string(),
                        ));
                    }
                }
            }
            _ => {
                // 终态 checkpoint 不应进入 pending；防坏数据。
                state.pending = Some(checkpoint);
                return Err(JobError::Checkpoint(
                    "terminal checkpoint cannot be resumed".to_string(),
                ));
            }
        };
        let job_id = snapshot.job_id;
        let artifacts_dir = checkpoint.artifacts_dir.clone();
        let slot = Arc::new(JobSlotShared::new(SlotData {
            snapshot,
            plan,
            stage_pos,
            awaiting_apply: checkpoint.awaiting_apply,
            apply_granted: false,
            rollback_requested: false,
            cancel: CancelToken::new(),
            completed: false,
            device_held: false,
            artifacts_dir,
        }));
        let inner = self.inner.clone();
        let worker_slot = slot.clone();
        let handle = thread::Builder::new()
            .name(format!("khaen-cal-job-resume-{}", job_id.0))
            .spawn(move || worker_main(inner, worker_slot, backend))
            .map_err(|e| JobError::Thread(e.to_string()))?;
        state.active = Some(ActiveHandle {
            slot,
            join: Some(handle),
        });
        Ok(job_id)
    }

    /// 放弃 pending job（无法恢复时使用）：按 applied_by_job 回 last-good/mute 并写终态 checkpoint。
    pub fn abandon(&self, reason: &str) -> Result<JobSnapshot, JobError> {
        let checkpoint = {
            let mut state = self.lock_state();
            if state.active.is_some() {
                return Err(JobError::Busy);
            }
            state.pending.take().ok_or(JobError::NoPendingResume)?
        };
        let mut device_held = checkpoint.device_held;
        let applied = checkpoint.applied_by_job;
        let recovery = do_recover(&self.inner, None, &mut device_held, applied, reason);
        let state = if applied && matches!(recovery.outcome, RecoveryOutcome::LastGood { .. }) {
            JobState::RolledBack
        } else {
            JobState::Failed
        };
        let mut snapshot = checkpoint.to_snapshot();
        let now = unix_now();
        snapshot.state = state;
        snapshot.stage = None;
        snapshot.stage_index = snapshot.stage_count;
        snapshot.stage_progress = 1.0;
        snapshot.overall_progress = 1.0;
        snapshot.awaiting_apply = false;
        snapshot.finished_unix = Some(now);
        snapshot.updated_unix = now;
        snapshot.recovery = Some(recovery);
        snapshot.failure = Some(JobFailure {
            code: "ABANDONED".to_string(),
            message: reason.to_string(),
            stage: checkpoint.state.active_stage(),
        });
        snapshot.terminal_reason = Some(reason.to_string());
        best_effort_manifest(&self.inner, &snapshot, &checkpoint.artifacts_dir);
        if let Some(path) = &self.inner.cfg.checkpoint_path {
            let checkpoint_out =
                JobCheckpoint::from_snapshot(&snapshot, &checkpoint.artifacts_dir, false);
            if let Err(error) = FileCheckpointStore::new(path.clone()).save(&checkpoint_out) {
                snapshot
                    .evidence_errors
                    .push(format!("terminal checkpoint: {}", error.message));
            }
        }
        self.lock_state().last_finished = Some(snapshot.clone());
        Ok(snapshot)
    }

    /// 关闭：取消 active job 并等待终态（daemon 退出/测试清理用）。
    pub fn shutdown(&self, timeout: Duration) -> Result<Option<JobSnapshot>, JobError> {
        let finished = {
            let state = self.lock_state();
            if state.active.is_none() {
                Some(state.last_finished.clone())
            } else {
                None
            }
        };
        if let Some(last_finished) = finished {
            return Ok(last_finished);
        }
        let _ = self.cancel();
        match self.wait_terminal(timeout) {
            Ok(snapshot) => Ok(Some(snapshot)),
            Err(error) => Err(error),
        }
    }

    fn active_slot(&self) -> Result<Arc<JobSlotShared>, JobError> {
        self.reap_finished();
        let state = self.lock_state();
        if state.pending.is_some() {
            return Err(JobError::ResumePending);
        }
        state
            .active
            .as_ref()
            .map(|active| active.slot.clone())
            .ok_or(JobError::NoActiveJob)
    }

    fn lock_state(&self) -> MutexGuard<'_, ManagerState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn save_checkpoint_for(&self, slot: &Arc<JobSlotShared>) -> Result<(), JobError> {
        let Some(path) = &self.inner.cfg.checkpoint_path else {
            return Ok(());
        };
        let checkpoint = {
            let data = slot.data();
            JobCheckpoint::from_snapshot(&data.snapshot, &data.artifacts_dir, data.device_held)
        };
        FileCheckpointStore::new(path.clone())
            .save(&checkpoint)
            .map_err(|e| JobError::Checkpoint(e.message))
    }

    fn reap_finished(&self) {
        let handle = {
            let mut state = self.lock_state();
            let finished = match &state.active {
                Some(active) => active.slot.data().completed,
                None => false,
            };
            if !finished {
                return;
            }
            match state.active.take() {
                Some(handle) => handle,
                None => return,
            }
        };
        let snapshot = handle.slot.data().snapshot.clone();
        if let Some(join) = handle.join {
            let _ = join.join();
        }
        self.lock_state().last_finished = Some(snapshot);
    }
}

// ===== worker =====

enum Termination {
    Complete,
    Failed {
        code: String,
        message: String,
        stage: Option<JobStage>,
    },
    Cancelled {
        reason: String,
    },
    RolledBack {
        reason: String,
    },
}

enum GateDecision {
    Granted,
    Cancelled,
    Rollback,
    Timeout,
}

struct Worker {
    inner: Arc<ManagerInner>,
    slot: Arc<JobSlotShared>,
    backend: Box<dyn CalibrationJobBackend>,
    request: JobRequest,
    job_id: JobId,
    plan: &'static [JobStage],
    device_held: bool,
    applied_in_job: bool,
}

impl Worker {
    fn data(&self) -> MutexGuard<'_, SlotData> {
        self.slot.data()
    }

    fn notify(&self) {
        self.slot.cv.notify_all();
    }

    fn run(mut self) {
        let mut pos = self.data().stage_pos;
        while pos < self.plan.len() {
            if let Some(termination) = self.control_termination() {
                self.finish(termination);
                return;
            }
            let stage = self.plan[pos];
            if let Err(error) = self.enter_stage(stage, pos) {
                self.finish(Termination::Failed {
                    code: "CHECKPOINT_IO".to_string(),
                    message: error,
                    stage: Some(stage),
                });
                return;
            }
            if stage.needs_calibration_device() && !self.device_held {
                match self.inner.safety.acquire_calibration_owner() {
                    Ok(()) => self.set_device_held(true),
                    Err(error) => {
                        self.finish(Termination::Failed {
                            code: "DEVICE_OWNERSHIP".to_string(),
                            message: error.to_string(),
                            stage: Some(stage),
                        });
                        return;
                    }
                }
            }
            let cancel = self.data().cancel.clone();
            let result = {
                let backend = &mut self.backend;
                let mut ctx = StageCtx::new(self.job_id, stage, &self.request, cancel, &self.slot);
                backend.run_stage(stage, &mut ctx)
            };
            self.applied_in_job = {
                let data = self.data();
                data.snapshot.applied_by_job
            };
            match result {
                Ok(()) => {
                    self.mark_stage_done(stage, pos);
                    if let Err(error) = self.save_checkpoint() {
                        self.finish(Termination::Failed {
                            code: "CHECKPOINT_IO".to_string(),
                            message: error,
                            stage: Some(stage),
                        });
                        return;
                    }
                    let next_needs_device = self
                        .plan
                        .get(pos + 1)
                        .map(|next| next.needs_calibration_device())
                        .unwrap_or(false);
                    if self.device_held && !next_needs_device {
                        if let Err(error) = self.release_device(ExitDisposition::ResumePlayback) {
                            self.finish(Termination::Failed {
                                code: "DEVICE_RELEASE".to_string(),
                                message: error.to_string(),
                                stage: Some(stage),
                            });
                            return;
                        }
                    }
                    if stage == JobStage::Prepared && !self.request.auto_apply {
                        if let Err(error) = self.set_awaiting_apply(true) {
                            self.finish(Termination::Failed {
                                code: "CHECKPOINT_IO".to_string(),
                                message: error,
                                stage: Some(stage),
                            });
                            return;
                        }
                        match self.wait_apply_gate() {
                            GateDecision::Granted => {
                                if let Err(error) = self.set_awaiting_apply(false) {
                                    self.finish(Termination::Failed {
                                        code: "CHECKPOINT_IO".to_string(),
                                        message: error,
                                        stage: Some(stage),
                                    });
                                    return;
                                }
                            }
                            GateDecision::Cancelled => {
                                self.finish(Termination::Cancelled {
                                    reason: "cancelled_while_awaiting_apply".to_string(),
                                });
                                return;
                            }
                            GateDecision::Rollback => {
                                self.finish(Termination::RolledBack {
                                    reason: "rollback_requested_before_apply".to_string(),
                                });
                                return;
                            }
                            GateDecision::Timeout => {
                                self.finish(Termination::Failed {
                                    code: "APPLY_CONFIRM_TIMEOUT".to_string(),
                                    message: "apply was not confirmed in time; last-good kept"
                                        .to_string(),
                                    stage: Some(stage),
                                });
                                return;
                            }
                        }
                    }
                    pos += 1;
                }
                Err(failure) => {
                    self.applied_in_job = {
                        let data = self.data();
                        data.snapshot.applied_by_job
                    };
                    if let Some(termination) = self.control_termination() {
                        self.finish(termination);
                        return;
                    }
                    if failure.cancelled {
                        let termination = if self.applied_in_job {
                            Termination::RolledBack {
                                reason: format!("cancelled_after_apply: {}", failure.message),
                            }
                        } else {
                            Termination::Cancelled {
                                reason: failure.message,
                            }
                        };
                        self.finish(termination);
                    } else {
                        self.finish(Termination::Failed {
                            code: failure.code,
                            message: failure.message,
                            stage: Some(stage),
                        });
                    }
                    return;
                }
            }
        }
        self.finish(Termination::Complete);
    }

    fn control_termination(&self) -> Option<Termination> {
        let data = self.data();
        if data.rollback_requested {
            return Some(Termination::RolledBack {
                reason: "rollback_requested".to_string(),
            });
        }
        if data.cancel.is_requested() {
            if data.snapshot.applied_by_job {
                return Some(Termination::RolledBack {
                    reason: "cancelled_after_apply".to_string(),
                });
            }
            return Some(Termination::Cancelled {
                reason: "cancelled_by_user".to_string(),
            });
        }
        None
    }

    fn enter_stage(&mut self, stage: JobStage, pos: usize) -> Result<(), String> {
        {
            let mut data = self.data();
            if data.snapshot.started_unix.is_none() {
                data.snapshot.started_unix = Some(unix_now());
            }
            data.stage_pos = pos;
            data.snapshot.state = JobState::Active(stage);
            data.snapshot.stage = Some(stage);
            data.snapshot.stage_index = pos as u32;
            data.snapshot.stage_progress = 0.0;
            data.snapshot.overall_progress = overall_progress(pos, 0.0, data.plan.len());
            data.snapshot.awaiting_apply = false;
            data.snapshot.updated_unix = unix_now();
        }
        self.notify();
        self.save_checkpoint()
    }

    fn mark_stage_done(&mut self, _stage: JobStage, pos: usize) {
        {
            let mut data = self.data();
            data.snapshot.stage_progress = 1.0;
            data.snapshot.overall_progress = overall_progress(pos, 1.0, data.plan.len());
            data.snapshot.updated_unix = unix_now();
        }
        self.notify();
    }

    fn set_awaiting_apply(&mut self, awaiting: bool) -> Result<(), String> {
        {
            let mut data = self.data();
            data.awaiting_apply = awaiting;
            data.snapshot.awaiting_apply = awaiting;
            data.snapshot.updated_unix = unix_now();
        }
        self.notify();
        self.save_checkpoint()
    }

    fn wait_apply_gate(&mut self) -> GateDecision {
        let deadline = self
            .inner
            .cfg
            .apply_confirm_timeout
            .map(|timeout| Instant::now() + timeout);
        let mut data = self.data();
        loop {
            if data.apply_granted {
                return GateDecision::Granted;
            }
            if data.rollback_requested {
                return GateDecision::Rollback;
            }
            if data.cancel.is_requested() {
                return GateDecision::Cancelled;
            }
            let wait_for = match deadline {
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return GateDecision::Timeout;
                    }
                    deadline - now
                }
                None => Duration::from_millis(50),
            };
            let (next, _timeout) = self
                .slot
                .cv
                .wait_timeout(data, wait_for)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            data = next;
        }
    }

    fn set_device_held(&mut self, held: bool) {
        self.device_held = held;
        let mut data = self.data();
        data.device_held = held;
        data.snapshot.updated_unix = unix_now();
    }

    fn release_device(&mut self, disposition: ExitDisposition) -> Result<(), SafetyError> {
        let result = do_release(
            &self.inner,
            Some(&self.slot),
            &mut self.device_held,
            disposition,
        );
        result
    }

    fn save_checkpoint(&self) -> Result<(), String> {
        let Some(path) = &self.inner.cfg.checkpoint_path else {
            return Ok(());
        };
        let checkpoint = {
            let data = self.data();
            JobCheckpoint::from_snapshot(&data.snapshot, &data.artifacts_dir, data.device_held)
        };
        FileCheckpointStore::new(path.clone())
            .save(&checkpoint)
            .map_err(|error| error.message)
    }

    fn write_manifest(&self) -> Result<(), String> {
        let snapshot = self.data().snapshot.clone();
        let dir = PathBuf::from(&snapshot.artifacts_dir);
        let manifest = snapshot.artifact_manifest_json(unix_now());
        let path = dir.join(ARTIFACT_MANIFEST_FILE_NAME);
        atomic_write(&path, manifest.as_bytes())
            .map_err(|error| format!("write artifact manifest {}: {error}", path.display()))
    }

    fn push_evidence_error(&self, message: String) {
        let mut data = self.data();
        data.snapshot.evidence_errors.push(message);
        data.snapshot.updated_unix = unix_now();
    }

    fn finish(&mut self, termination: Termination) {
        match termination {
            Termination::Complete => self.finish_complete(),
            other => self.finish_non_complete(other),
        }
    }

    fn finish_complete(&mut self) {
        if self.device_held {
            if let Err(error) = self.release_device(ExitDisposition::ResumePlayback) {
                self.finish_non_complete(Termination::Failed {
                    code: "DEVICE_RELEASE".to_string(),
                    message: error.to_string(),
                    stage: None,
                });
                return;
            }
        }
        let snapshot = {
            let mut data = self.data();
            let now = unix_now();
            let plan_len = data.plan.len();
            data.snapshot.state = JobState::Complete;
            data.snapshot.stage = None;
            data.snapshot.stage_index = plan_len as u32;
            data.snapshot.stage_progress = 1.0;
            data.snapshot.overall_progress = 1.0;
            data.snapshot.awaiting_apply = false;
            data.snapshot.finished_unix = Some(now);
            data.snapshot.updated_unix = now;
            data.snapshot.terminal_reason = Some("completed".to_string());
            data.snapshot.clone()
        };
        if let Err(error) = self.write_manifest() {
            self.finish_non_complete(Termination::Failed {
                code: "ARTIFACT_MANIFEST_IO".to_string(),
                message: error,
                stage: None,
            });
            return;
        }
        if let Err(error) = self.save_checkpoint() {
            let _ = snapshot;
            self.finish_non_complete(Termination::Failed {
                code: "CHECKPOINT_IO".to_string(),
                message: error,
                stage: None,
            });
            return;
        }
        self.notify();
    }

    fn finish_non_complete(&mut self, termination: Termination) {
        let (reason, failure, requested_state) = match termination {
            Termination::Complete => unreachable!("complete handled by finish_complete"),
            Termination::Failed {
                code,
                message,
                stage,
            } => (
                format!("stage_failed:{}", code),
                Some(JobFailure {
                    code,
                    message,
                    stage,
                }),
                JobState::Failed,
            ),
            Termination::Cancelled { reason } => (reason, None, JobState::Cancelled),
            Termination::RolledBack { reason } => (reason, None, JobState::RolledBack),
        };
        let applied = self.applied_in_job || self.data().snapshot.applied_by_job;
        let recovery = do_recover(
            &self.inner,
            Some(&self.slot),
            &mut self.device_held,
            applied,
            &reason,
        );
        let state = match requested_state {
            JobState::Failed => {
                if applied && matches!(recovery.outcome, RecoveryOutcome::LastGood { .. }) {
                    JobState::RolledBack
                } else {
                    JobState::Failed
                }
            }
            JobState::Cancelled => {
                if applied {
                    JobState::RolledBack
                } else {
                    JobState::Cancelled
                }
            }
            JobState::RolledBack => JobState::RolledBack,
            _ => requested_state,
        };
        {
            let mut data = self.data();
            let now = unix_now();
            let plan_len = data.plan.len();
            data.snapshot.state = state;
            data.snapshot.stage = None;
            data.snapshot.stage_index = plan_len as u32;
            data.snapshot.stage_progress = 1.0;
            data.snapshot.overall_progress = 1.0;
            data.snapshot.awaiting_apply = false;
            data.snapshot.finished_unix = Some(now);
            data.snapshot.updated_unix = now;
            data.snapshot.recovery = Some(recovery);
            data.snapshot.failure = failure;
            data.snapshot.terminal_reason = Some(reason.clone());
            data.snapshot.cancel_requested = data.cancel.is_requested();
        }
        self.notify();
        if let Err(error) = self.write_manifest() {
            self.push_evidence_error(format!("artifact manifest: {error}"));
        }
        if let Err(error) = self.save_checkpoint() {
            self.push_evidence_error(format!("checkpoint: {error}"));
        }
    }
}

fn worker_main(
    inner: Arc<ManagerInner>,
    slot: Arc<JobSlotShared>,
    backend: Box<dyn CalibrationJobBackend>,
) {
    let panic_result = catch_unwind(AssertUnwindSafe(|| {
        let (request, job_id, plan) = {
            let data = slot.data();
            (
                data.snapshot.request.clone(),
                data.snapshot.job_id,
                data.plan,
            )
        };
        let worker = Worker {
            request,
            job_id,
            plan,
            inner: inner.clone(),
            slot: slot.clone(),
            backend,
            device_held: false,
            applied_in_job: false,
        };
        worker.run();
    }));
    if panic_result.is_err() {
        recover_from_panic(&inner, &slot);
    }
    {
        let mut data = slot.data();
        data.completed = true;
        data.snapshot.updated_unix = unix_now();
    }
    slot.cv.notify_all();
}

fn recover_from_panic(inner: &Arc<ManagerInner>, slot: &Arc<JobSlotShared>) {
    let (applied, device_held) = {
        let data = slot.data();
        (data.snapshot.applied_by_job, data.device_held)
    };
    let mut device_held = device_held;
    let recovery = do_recover(
        inner,
        Some(slot),
        &mut device_held,
        applied,
        "backend_panic",
    );
    let state = if applied && matches!(recovery.outcome, RecoveryOutcome::LastGood { .. }) {
        JobState::RolledBack
    } else {
        JobState::Failed
    };
    {
        let mut data = slot.data();
        let now = unix_now();
        let plan_len = data.plan.len();
        data.snapshot.state = state;
        data.snapshot.stage = None;
        data.snapshot.stage_index = plan_len as u32;
        data.snapshot.stage_progress = 1.0;
        data.snapshot.overall_progress = 1.0;
        data.snapshot.awaiting_apply = false;
        data.snapshot.finished_unix = Some(now);
        data.snapshot.updated_unix = now;
        data.snapshot.recovery = Some(recovery);
        data.snapshot.failure = Some(JobFailure {
            code: "BACKEND_PANIC".to_string(),
            message: "calibration backend panicked; job contained and recovered".to_string(),
            stage: data.snapshot.stage,
        });
        data.snapshot.terminal_reason = Some("backend_panic".to_string());
        data.device_held = device_held;
    }
    if let Err(error) = write_manifest_for(inner, slot) {
        let mut data = slot.data();
        data.snapshot
            .evidence_errors
            .push(format!("artifact manifest: {error}"));
    }
    if let Err(error) = save_checkpoint_for(inner, slot) {
        let mut data = slot.data();
        data.snapshot
            .evidence_errors
            .push(format!("checkpoint: {error}"));
    }
    slot.cv.notify_all();
}

fn write_manifest_for(inner: &Arc<ManagerInner>, slot: &Arc<JobSlotShared>) -> Result<(), String> {
    let _ = inner;
    let snapshot = slot.data().snapshot.clone();
    let path = PathBuf::from(&snapshot.artifacts_dir).join(ARTIFACT_MANIFEST_FILE_NAME);
    let manifest = snapshot.artifact_manifest_json(unix_now());
    atomic_write(&path, manifest.as_bytes())
        .map_err(|error| format!("write artifact manifest {}: {error}", path.display()))
}

fn save_checkpoint_for(inner: &Arc<ManagerInner>, slot: &Arc<JobSlotShared>) -> Result<(), String> {
    let Some(path) = &inner.cfg.checkpoint_path else {
        return Ok(());
    };
    let checkpoint = {
        let data = slot.data();
        JobCheckpoint::from_snapshot(&data.snapshot, &data.artifacts_dir, data.device_held)
    };
    FileCheckpointStore::new(path.clone())
        .save(&checkpoint)
        .map_err(|error| error.message)
}

fn best_effort_manifest(inner: &ManagerInner, snapshot: &JobSnapshot, artifacts_dir: &Path) {
    let _ = inner;
    let path = artifacts_dir.join(ARTIFACT_MANIFEST_FILE_NAME);
    let manifest = snapshot.artifact_manifest_json(unix_now());
    let _ = atomic_write(&path, manifest.as_bytes());
}

fn do_release(
    inner: &ManagerInner,
    slot: Option<&Arc<JobSlotShared>>,
    device_held: &mut bool,
    disposition: ExitDisposition,
) -> Result<(), SafetyError> {
    inner.safety.release_calibration_owner(disposition)?;
    *device_held = false;
    if let Some(slot) = slot {
        let mut data = slot.data();
        data.device_held = false;
        data.snapshot.updated_unix = unix_now();
    }
    Ok(())
}

fn do_mute(
    inner: &ManagerInner,
    slot: Option<&Arc<JobSlotShared>>,
    device_held: &mut bool,
) -> RecoveryOutcome {
    let mute_ok = inner.safety.activate_safe_mute().is_ok();
    let release_ok = if *device_held {
        do_release(inner, slot, device_held, ExitDisposition::SafetyMute).is_ok()
    } else {
        true
    };
    if mute_ok && release_ok {
        RecoveryOutcome::Mute
    } else {
        RecoveryOutcome::Failed {
            error: "last-good restore failed and safe mute could not be confirmed".to_string(),
        }
    }
}

fn do_recover(
    inner: &ManagerInner,
    slot: Option<&Arc<JobSlotShared>>,
    device_held: &mut bool,
    applied: bool,
    reason: &str,
) -> RecoveryReport {
    let outcome = if applied {
        match inner.safety.restore_last_good() {
            Ok(version) => {
                if let Some(slot) = slot {
                    let mut data = slot.data();
                    data.snapshot.last_good_version = Some(version.clone());
                }
                if *device_held {
                    match do_release(inner, slot, device_held, ExitDisposition::ResumePlayback) {
                        Ok(()) => RecoveryOutcome::LastGood { version },
                        Err(_) => do_mute(inner, slot, device_held),
                    }
                } else {
                    RecoveryOutcome::LastGood { version }
                }
            }
            Err(_) => do_mute(inner, slot, device_held),
        }
    } else if *device_held {
        match do_release(inner, slot, device_held, ExitDisposition::ResumePlayback) {
            Ok(()) => RecoveryOutcome::LastGood {
                version: inner
                    .safety
                    .last_good_version()
                    .unwrap_or_else(|| "last-good".to_string()),
            },
            Err(_) => do_mute(inner, slot, device_held),
        }
    } else {
        RecoveryOutcome::LastGood {
            version: inner
                .safety
                .last_good_version()
                .unwrap_or_else(|| "last-good".to_string()),
        }
    };
    RecoveryReport {
        outcome,
        reason: reason.to_string(),
        device_released: !*device_held,
        at_unix: unix_now(),
    }
}

// ===== T60 unit tests =====
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Condvar, Mutex};
    use std::time::{Duration, Instant};

    // ===== fake safe-output controller（记录调用、校验设备互斥、可注入故障） =====

    struct FakeSafety {
        calls: Mutex<Vec<String>>,
        last_good: Mutex<String>,
        acquire_fail: AtomicBool,
        restore_fail: AtomicBool,
        mute_fail: AtomicBool,
        device_held: AtomicBool,
        violations: Mutex<Vec<String>>,
    }

    impl FakeSafety {
        fn new() -> Arc<Self> {
            Arc::new(FakeSafety {
                calls: Mutex::new(Vec::new()),
                last_good: Mutex::new("last-good-initial".to_string()),
                acquire_fail: AtomicBool::new(false),
                restore_fail: AtomicBool::new(false),
                mute_fail: AtomicBool::new(false),
                device_held: AtomicBool::new(false),
                violations: Mutex::new(Vec::new()),
            })
        }

        fn push(&self, call: impl Into<String>) {
            self.calls.lock().unwrap().push(call.into());
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        fn count(&self, needle: &str) -> usize {
            self.calls()
                .iter()
                .filter(|call| call.contains(needle))
                .count()
        }

        fn violations(&self) -> Vec<String> {
            self.violations.lock().unwrap().clone()
        }

        fn violation(&self, message: impl Into<String>) {
            self.violations.lock().unwrap().push(message.into());
        }

        fn set_restore_fail(&self, fail: bool) {
            self.restore_fail.store(fail, Ordering::SeqCst);
        }

        fn set_mute_fail(&self, fail: bool) {
            self.mute_fail.store(fail, Ordering::SeqCst);
        }

        fn is_device_held(&self) -> bool {
            self.device_held.load(Ordering::SeqCst)
        }
    }

    impl SafeOutputController for FakeSafety {
        fn acquire_calibration_owner(&self) -> Result<(), SafetyError> {
            self.push("acquire");
            if self.device_held.swap(true, Ordering::SeqCst) {
                self.violation("calibration device acquired twice");
                return Err(SafetyError::new("DEVICE_BUSY", "mock double acquire"));
            }
            if self.acquire_fail.load(Ordering::SeqCst) {
                self.device_held.store(false, Ordering::SeqCst);
                return Err(SafetyError::new("DEVICE_BUSY", "mock acquire failure"));
            }
            Ok(())
        }

        fn release_calibration_owner(
            &self,
            disposition: ExitDisposition,
        ) -> Result<(), SafetyError> {
            self.push(format!("release:{}", disposition.as_str()));
            if !self.device_held.swap(false, Ordering::SeqCst) {
                self.violation("calibration device released without acquire");
                return Err(SafetyError::new(
                    "DEVICE_NOT_HELD",
                    "mock release without acquire",
                ));
            }
            Ok(())
        }

        fn restore_last_good(&self) -> Result<String, SafetyError> {
            self.push("restore_last_good");
            if self.restore_fail.load(Ordering::SeqCst) {
                return Err(SafetyError::new(
                    "LAST_GOOD_UNAVAILABLE",
                    "mock restore failure",
                ));
            }
            let restored = self.count("restore_last_good");
            let mut last_good = self.last_good.lock().unwrap();
            *last_good = format!("restored-v{restored}");
            Ok(last_good.clone())
        }

        fn activate_safe_mute(&self) -> Result<(), SafetyError> {
            self.push("activate_safe_mute");
            if self.mute_fail.load(Ordering::SeqCst) {
                return Err(SafetyError::new("MUTE_FAILED", "mock mute failure"));
            }
            Ok(())
        }

        fn last_good_version(&self) -> Option<String> {
            Some(self.last_good.lock().unwrap().clone())
        }
    }

    // ===== fake backend（脚本化 stage 行为；无第三方依赖） =====

    #[derive(Clone)]
    struct Gate {
        inner: Arc<(Mutex<bool>, Condvar)>,
    }

    impl Gate {
        fn new() -> Self {
            Gate {
                inner: Arc::new((Mutex::new(false), Condvar::new())),
            }
        }

        fn open(&self) {
            let (open, wake) = &*self.inner;
            *open.lock().unwrap() = true;
            wake.notify_all();
        }

        fn wait(&self) {
            let (open, wake) = &*self.inner;
            let mut guard = open.lock().unwrap();
            while !*guard {
                guard = wake.wait(guard).unwrap();
            }
        }
    }

    enum FakeAction {
        Fail {
            code: &'static str,
            message: &'static str,
        },
        Panic,
        CancelLoop,
        Block(Gate),
        ProgressThenBlock {
            gate: Gate,
            values: Vec<f32>,
        },
        FailAfterApplyCommit,
        ArtifactTwice,
        DeleteDir(PathBuf),
    }

    struct FakeBackend {
        calls: Arc<Mutex<Vec<JobStage>>>,
        actions: HashMap<JobStage, FakeAction>,
        artifacts: bool,
    }

    impl FakeBackend {
        fn new(calls: Arc<Mutex<Vec<JobStage>>>) -> Self {
            FakeBackend {
                calls,
                actions: HashMap::new(),
                artifacts: false,
            }
        }

        fn with_action(mut self, stage: JobStage, action: FakeAction) -> Self {
            self.actions.insert(stage, action);
            self
        }

        fn with_artifacts(mut self, artifacts: bool) -> Self {
            self.artifacts = artifacts;
            self
        }

        fn write_default_artifacts(
            &self,
            stage: JobStage,
            ctx: &mut StageCtx<'_>,
        ) -> Result<(), StageFailure> {
            if !self.artifacts {
                return Ok(());
            }
            match stage {
                JobStage::SweepCapture => {
                    ctx.write_artifact(
                        ArtifactKind::RawSweep,
                        "sweep/raw.bin",
                        b"raw-sweep-data",
                        "sweep-v1",
                    )?;
                }
                JobStage::QualityCheck => {
                    ctx.write_artifact(
                        ArtifactKind::QualityReport,
                        "quality/report.json",
                        b"{\"snr_db\":42}",
                        "quality-v1",
                    )?;
                }
                JobStage::Compile => {
                    ctx.write_artifact(
                        ArtifactKind::FirBundle,
                        "fir/bundle.wav",
                        b"fir-bytes",
                        "fir-v1",
                    )?;
                }
                JobStage::Apply => {
                    ctx.set_last_good_version("last-good-v1");
                    ctx.set_applied_version("applied-v1");
                }
                JobStage::Remeasure => {
                    ctx.write_artifact(
                        ArtifactKind::RemeasureReport,
                        "remeasure/report.json",
                        b"{\"ok\":true}",
                        "remeasure-v1",
                    )?;
                }
                JobStage::IndependentHoldout => {
                    ctx.write_artifact(
                        ArtifactKind::HoldoutReport,
                        "holdout/report.json",
                        b"{\"residual\":0.8}",
                        "holdout-v1",
                    )?;
                }
                _ => {}
            }
            Ok(())
        }
    }

    impl CalibrationJobBackend for FakeBackend {
        fn run_stage(
            &mut self,
            stage: JobStage,
            ctx: &mut StageCtx<'_>,
        ) -> Result<(), StageFailure> {
            self.calls.lock().unwrap().push(stage);
            if stage == JobStage::Apply && !self.actions.contains_key(&stage) {
                ctx.set_last_good_version("last-good-v1");
                ctx.set_applied_version("applied-v1");
            }
            self.write_default_artifacts(stage, ctx)?;
            let Some(action) = self.actions.get(&stage) else {
                return Ok(());
            };
            match action {
                FakeAction::Fail { code, message } => Err(StageFailure::failed(*code, *message)),
                FakeAction::Panic => panic!("fake backend panic at {}", stage.as_str()),
                FakeAction::CancelLoop => loop {
                    if ctx.cancel_token().is_requested() {
                        return Err(StageFailure::cancelled("sweep cancelled by test"));
                    }
                    ctx.report_progress(0.1);
                    thread::sleep(Duration::from_millis(2));
                },
                FakeAction::Block(gate) => {
                    gate.wait();
                    Ok(())
                }
                FakeAction::ProgressThenBlock { gate, values } => {
                    for value in values {
                        ctx.report_progress(*value);
                    }
                    gate.wait();
                    Ok(())
                }
                FakeAction::FailAfterApplyCommit => {
                    ctx.set_last_good_version("last-good-v1");
                    ctx.set_applied_version("applied-v1");
                    Err(StageFailure::failed(
                        "E_APPLY_ACK",
                        "apply ack timed out after commit",
                    ))
                }
                FakeAction::ArtifactTwice => {
                    ctx.write_artifact(
                        ArtifactKind::Other("meta".to_string()),
                        "preflight/meta.json",
                        b"v1",
                        "meta-v1",
                    )?;
                    if let Err(error) = ctx.write_artifact(
                        ArtifactKind::Other("meta".to_string()),
                        "preflight/meta.json",
                        b"v2",
                        "meta-v2",
                    ) {
                        return Err(error);
                    }
                    Ok(())
                }
                FakeAction::DeleteDir(path) => {
                    fs::remove_dir_all(path).unwrap();
                    Ok(())
                }
            }
        }
    }

    // ===== harness =====

    fn temp_root(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let number = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "khaen-caljob-t60-{}-{}-{}",
            std::process::id(),
            tag,
            number
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn test_config(root: &Path, timeout: Option<Duration>) -> JobManagerConfig {
        let state_dir = root.join("state");
        fs::create_dir_all(&state_dir).unwrap();
        JobManagerConfig {
            artifacts_root: root.join("artifacts"),
            checkpoint_path: Some(state_dir.join(CHECKPOINT_FILE_NAME)),
            apply_confirm_timeout: timeout,
        }
    }

    struct TestEnv {
        root: PathBuf,
        safety: Arc<FakeSafety>,
        manager: JobManager,
    }

    impl TestEnv {
        fn new(tag: &str) -> Self {
            Self::with_timeout(tag, None)
        }

        fn with_timeout(tag: &str, timeout: Option<Duration>) -> Self {
            let root = temp_root(tag);
            let safety = FakeSafety::new();
            let manager = JobManager::new(test_config(&root, timeout), safety.clone()).unwrap();
            TestEnv {
                root,
                safety,
                manager,
            }
        }

        fn checkpoint_store(&self) -> FileCheckpointStore {
            FileCheckpointStore::new(self.root.join("state").join(CHECKPOINT_FILE_NAME))
        }

        fn load_checkpoint(&self) -> JobCheckpoint {
            self.checkpoint_store()
                .load()
                .unwrap()
                .expect("checkpoint should exist")
        }
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            let _ = self.manager.shutdown(Duration::from_secs(5));
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn full_request(auto_apply: bool) -> JobRequest {
        JobRequest {
            kind: JobKind::Full,
            label: "T60 full flow".to_string(),
            layout: "4.1.2".to_string(),
            sample_rate: 48_000,
            design_points: 1,
            holdout_points: 2,
            repeats: 2,
            drive_level_dbfs: -30.0,
            audible_sweep_authorized: true,
            auto_apply,
            expected_applied_version: None,
            inputs: vec![InputHash {
                name: "layout-presets".to_string(),
                sha256: sha256_hex(b"layout-presets-v2"),
            }],
        }
    }

    fn retest_request() -> JobRequest {
        JobRequest {
            kind: JobKind::Retest,
            expected_applied_version: Some("applied-v1".to_string()),
            auto_apply: false,
            ..full_request(false)
        }
    }

    fn offline_request(audible: bool) -> JobRequest {
        JobRequest {
            kind: JobKind::SinglePointOffline,
            audible_sweep_authorized: audible,
            ..full_request(false)
        }
    }

    fn rt_host_request(audible: bool) -> JobRequest {
        JobRequest {
            kind: JobKind::SinglePointRtHost,
            label: "rt-host single-point measurement".to_string(),
            layout: "single_point".to_string(),
            sample_rate: 48_000,
            design_points: 1,
            holdout_points: 1,
            repeats: 1,
            drive_level_dbfs: -12.0,
            audible_sweep_authorized: audible,
            auto_apply: false,
            expected_applied_version: None,
            inputs: vec![InputHash {
                name: "mic_cal".to_string(),
                sha256: sha256_hex(b"umik-cal-file"),
            }],
        }
    }

    #[test]
    fn single_point_offline_does_not_require_audible_sweep_authorization() {
        // 离线证据 job 不打开设备、不播放扫频：P08 的可闻授权不适用，传 false 才诚实；
        // 采音类 kind 仍然必须显式授权（fail-closed，不允许靠 kind 绕过）。
        offline_request(false)
            .validate()
            .expect("offline job is silent and needs no audible authorization");
        offline_request(true)
            .validate()
            .expect("an explicit authorization flag is still accepted");
        let mut full = full_request(false);
        full.audible_sweep_authorized = false;
        assert!(full.validate().is_err());
        let mut retest = retest_request();
        retest.audible_sweep_authorized = false;
        assert!(retest.validate().is_err());
    }

    #[test]
    fn single_point_rt_host_kind_name_plan_and_authorization_are_frozen() {
        assert_eq!(JobKind::SinglePointRtHost.as_str(), "single_point_rt_host");
        assert_eq!(
            JobKind::parse("single_point_rt_host"),
            Some(JobKind::SinglePointRtHost)
        );
        assert_eq!(
            JobKind::SinglePointRtHost.plan(),
            &SINGLE_POINT_RT_HOST_PLAN
        );
        let names: Vec<&str> = JobKind::SinglePointRtHost
            .plan()
            .iter()
            .map(|stage| stage.as_str())
            .collect();
        assert_eq!(
            names,
            ["validate", "sweep_capture", "quality_check", "bundle"]
        );
        // 真实测量会出声/采音：P08 要求显式可闻授权，缺少授权必须拒绝（fail-closed）。
        rt_host_request(true)
            .validate()
            .expect("explicit audible authorization accepted");
        let error = rt_host_request(false)
            .validate()
            .expect_err("real measurement without audible authorization must be rejected");
        assert!(error.contains("audible_sweep_authorized"), "{error}");
        // expected_applied_version 只属于 retest。
        let mut request = rt_host_request(true);
        request.expected_applied_version = Some("v1".to_string());
        assert!(request.validate().is_err());
        // 该计划里的采集 stage 需要设备交接（SweepCapture），其余不要。
        assert!(JobStage::SweepCapture.needs_calibration_device());
        assert!(!JobStage::Validate.needs_calibration_device());
        assert!(!JobStage::QualityCheck.needs_calibration_device());
        assert!(!JobStage::Bundle.needs_calibration_device());
    }

    fn wait_state(
        manager: &JobManager,
        timeout: Duration,
        mut predicate: impl FnMut(&JobSnapshot) -> bool,
    ) -> JobSnapshot {
        let deadline = Instant::now() + timeout;
        let mut last = None;
        loop {
            if let Some(snapshot) = manager.snapshot() {
                if predicate(&snapshot) {
                    return snapshot;
                }
                last = Some(snapshot);
            }
            if Instant::now() >= deadline {
                panic!("timed out waiting for job state; last={last:?}");
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn wait_terminal(manager: &JobManager) -> JobSnapshot {
        manager
            .wait_terminal(Duration::from_secs(60))
            .unwrap_or_else(|error| {
                panic!(
                    "wait_terminal failed: {error}; last={:?}",
                    manager.snapshot()
                )
            })
    }

    fn approx(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1e-5,
            "expected {expected}, got {actual}"
        );
    }

    fn checkpoint_at(root: &Path, job_id: JobId, state: JobState) -> JobCheckpoint {
        let request = full_request(true);
        let artifacts_dir = root.join("artifacts").join(job_id.artifact_dir_name());
        fs::create_dir_all(&artifacts_dir).unwrap();
        let now = unix_now();
        JobCheckpoint {
            job_id,
            kind: JobKind::Full,
            state,
            stage_index: state
                .active_stage()
                .and_then(|stage| JobKind::Full.plan().iter().position(|s| *s == stage))
                .map(|position| position as u32)
                .unwrap_or(0),
            plan: JobKind::Full.plan().to_vec(),
            request,
            artifacts_dir,
            device_held: false,
            awaiting_apply: false,
            applied_by_job: false,
            applied_version: None,
            last_good_version: Some("checkpointed-last-good".to_string()),
            created_unix: now,
            updated_unix: now,
            started_unix: Some(now),
            finished_unix: None,
            artifacts: Vec::new(),
            recovery: None,
            failure: None,
            terminal_reason: None,
            evidence_errors: Vec::new(),
        }
    }

    // ===== 1) 状态/阶段/机器名冻结 =====

    #[test]
    fn job_state_and_stage_names_match_dec_contract() {
        assert_eq!(JobKind::Full.plan().len(), 10);
        assert_eq!(JobKind::Retest.plan().len(), 3);
        let mut names = vec!["queued".to_string()];
        names.extend(JobStage::ALL.iter().map(|stage| stage.as_str().to_string()));
        names.extend(
            ["complete", "failed", "cancelled", "rolled_back"]
                .iter()
                .map(|name| name.to_string()),
        );
        let expected = [
            "queued",
            "preflight",
            "sweep_capture",
            "quality_check",
            "fit_p",
            "solve_g",
            "compile",
            "prepared",
            "apply",
            "remeasure",
            "independent_holdout",
            "complete",
            "failed",
            "cancelled",
            "rolled_back",
        ];
        assert_eq!(
            names,
            expected
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>()
        );
        for stage in JobStage::ALL {
            assert_eq!(
                JobState::parse(stage.as_str()),
                Some(JobState::Active(stage))
            );
            assert_eq!(JobStage::parse(stage.as_str()), Some(stage));
        }
        for name in expected {
            assert!(JobState::parse(name).is_some(), "unparsable state {name}");
        }
        assert_eq!(JobState::parse("bogus"), None);
        assert_eq!(JobStage::parse("bogus"), None);
        assert_eq!(JobKind::parse("bogus"), None);
        assert!(JobStage::SweepCapture.needs_calibration_device());
        assert!(JobStage::Remeasure.needs_calibration_device());
        assert!(JobStage::IndependentHoldout.needs_calibration_device());
        assert!(!JobStage::Apply.needs_calibration_device());
        assert!(JobState::Complete.is_terminal());
        assert!(!JobState::Active(JobStage::Prepared).is_terminal());
        assert_eq!(JobId(7).to_string(), "job-0007");
    }

    // ===== 2) SHA-256 已知答案 =====

    #[test]
    fn job_sha256_matches_known_answers() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            sha256_hex(&vec![b'a'; 1_000_000]),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        assert_eq!(json_escape("a\"b\\c\n\t"), "a\\\"b\\\\c\\n\\t");
    }

    // ===== 3) full happy path（auto_apply）：状态/progress/artifact/checkpoint/last-good 语义 =====

    #[test]
    fn job_full_flow_auto_apply_completes_with_artifacts_and_checkpoint() {
        let env = TestEnv::new("full-flow");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend = FakeBackend::new(calls.clone()).with_artifacts(true);
        let job_id = env
            .manager
            .start(full_request(true), Box::new(backend))
            .unwrap();
        let snapshot = wait_terminal(&env.manager);
        assert_eq!(snapshot.job_id, job_id);
        assert_eq!(snapshot.state, JobState::Complete);
        assert_eq!(snapshot.state_name(), "complete");
        assert_eq!(snapshot.stage_count, 10);
        assert_eq!(snapshot.stage_index, 10);
        approx(snapshot.overall_progress, 1.0);
        approx(snapshot.stage_progress, 1.0);
        assert_eq!(calls.lock().unwrap().as_slice(), JobStage::ALL.as_slice());
        assert_eq!(snapshot.applied_version.as_deref(), Some("applied-v1"));
        assert_eq!(snapshot.last_good_version.as_deref(), Some("last-good-v1"));
        assert!(snapshot.applied_by_job);
        assert!(snapshot.failure.is_none());
        assert!(snapshot.recovery.is_none());
        assert_eq!(snapshot.artifacts.len(), 5);
        for artifact in &snapshot.artifacts {
            let path = PathBuf::from(&snapshot.artifacts_dir).join(&artifact.relative_path);
            assert_eq!(
                sha256_file(&path).unwrap(),
                artifact.sha256,
                "artifact hash mismatch"
            );
            assert_eq!(artifact.bytes, fs::read(&path).unwrap().len() as u64);
        }
        let manifest_path =
            PathBuf::from(&snapshot.artifacts_dir).join(ARTIFACT_MANIFEST_FILE_NAME);
        let manifest = fs::read_to_string(&manifest_path).unwrap();
        assert!(manifest.contains("\"schema\":\"khaen-calibration-artifacts/v1\""));
        for artifact in &snapshot.artifacts {
            assert!(
                manifest.contains(&artifact.sha256),
                "manifest missing artifact hash"
            );
        }
        let checkpoint = env.load_checkpoint();
        assert_eq!(checkpoint.state, JobState::Complete);
        assert_eq!(checkpoint.artifacts.len(), 5);
        assert_eq!(checkpoint.applied_version.as_deref(), Some("applied-v1"));
        assert!(checkpoint.finished_unix.is_some());
        // 设备独占：两次 acquire、两次 release，且无 restore（成功路径不进恢复）
        assert_eq!(env.safety.count("acquire"), 2);
        assert_eq!(env.safety.count("release:resume_playback"), 2);
        assert_eq!(env.safety.count("restore_last_good"), 0);
        assert!(
            env.safety.violations().is_empty(),
            "{:?}",
            env.safety.violations()
        );
        // JSON 契约关键字段
        let json = snapshot.to_json();
        assert!(json.contains("\"schema\":\"khaen-calibration-job/v1\""));
        assert!(json.contains("\"state\":\"complete\""));
        assert!(json.contains("\"job_id\":\"job-0001\""));
        assert!(json.contains("\"overall_progress\":1"));
        let open = json.chars().filter(|ch| *ch == '{').count();
        let close = json.chars().filter(|ch| *ch == '}').count();
        assert_eq!(open, close, "unbalanced JSON braces");
    }

    // ===== 4) prepared 必须显式 apply（R48 apply 门；幂等/错状态拒绝） =====

    #[test]
    fn job_apply_gate_waits_for_explicit_apply() {
        let env = TestEnv::new("apply-gate");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend = FakeBackend::new(calls.clone()).with_artifacts(true);
        env.manager
            .start(full_request(false), Box::new(backend))
            .unwrap();
        let prepared = wait_state(&env.manager, Duration::from_secs(60), |snapshot| {
            snapshot.state == JobState::Active(JobStage::Prepared) && snapshot.awaiting_apply
        });
        assert!(!calls.lock().unwrap().contains(&JobStage::Apply));
        approx(prepared.overall_progress, 0.7);
        // 幂等：prepare 已放行后再调 apply 返回同一 job，不二次提交
        assert_eq!(env.manager.apply().unwrap(), prepared.job_id);
        let terminal = wait_terminal(&env.manager);
        assert_eq!(terminal.state, JobState::Complete);
        assert_eq!(calls.lock().unwrap().as_slice(), JobStage::ALL.as_slice());
        // 终态后 apply/cancel/rollback 都显式拒绝（worker 已回收 -> NoActiveJob；未回收 -> InvalidState）
        let inactive = |result: Result<JobId, JobError>| {
            matches!(
                result,
                Err(JobError::InvalidState(_)) | Err(JobError::NoActiveJob)
            )
        };
        assert!(inactive(env.manager.apply()));
        assert!(inactive(env.manager.cancel()));
        assert!(inactive(env.manager.rollback()));
    }

    // ===== 5) apply 前 cancel：Cancelled + last-good 保持 + 设备释放 =====

    #[test]
    fn job_cancel_before_apply_is_cancelled_and_keeps_last_good() {
        let env = TestEnv::new("cancel-prepared");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend = FakeBackend::new(calls.clone()).with_artifacts(true);
        env.manager
            .start(full_request(false), Box::new(backend))
            .unwrap();
        wait_state(&env.manager, Duration::from_secs(60), |snapshot| {
            snapshot.state == JobState::Active(JobStage::Prepared) && snapshot.awaiting_apply
        });
        assert_eq!(env.manager.cancel().unwrap().as_u64(), 1);
        let terminal = wait_terminal(&env.manager);
        assert_eq!(terminal.state, JobState::Cancelled);
        assert!(terminal.failure.is_none());
        assert!(terminal.terminal_reason.is_some());
        let recovery = terminal
            .recovery
            .expect("cancelled job must record recovery");
        assert!(matches!(recovery.outcome, RecoveryOutcome::LastGood { .. }));
        assert_eq!(env.safety.count("restore_last_good"), 0);
        assert_eq!(env.safety.count("acquire"), 1);
        assert_eq!(env.safety.count("release:resume_playback"), 1);
        assert!(!env.safety.is_device_held());
        assert!(!calls.lock().unwrap().contains(&JobStage::Apply));
        assert!(terminal
            .artifacts
            .iter()
            .any(|artifact| artifact.kind == ArtifactKind::RawSweep));
        assert_eq!(env.load_checkpoint().state, JobState::Cancelled);
        assert!(
            env.safety.violations().is_empty(),
            "{:?}",
            env.safety.violations()
        );
    }

    // ===== 6) 扫频中 cancel：backend 协作中断 + 设备释放 =====

    #[test]
    fn job_cancel_during_sweep_interrupts_backend() {
        let env = TestEnv::new("cancel-sweep");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend = FakeBackend::new(calls.clone())
            .with_action(JobStage::SweepCapture, FakeAction::CancelLoop);
        env.manager
            .start(full_request(true), Box::new(backend))
            .unwrap();
        wait_state(&env.manager, Duration::from_secs(60), |snapshot| {
            snapshot.state == JobState::Active(JobStage::SweepCapture)
        });
        env.manager.cancel().unwrap();
        let terminal = wait_terminal(&env.manager);
        assert_eq!(terminal.state, JobState::Cancelled);
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            [JobStage::Preflight, JobStage::SweepCapture]
        );
        assert_eq!(env.safety.count("acquire"), 1);
        assert_eq!(env.safety.count("release:resume_playback"), 1);
        assert_eq!(env.safety.count("restore_last_good"), 0);
        assert!(!env.safety.is_device_held());
        assert!(
            env.safety.violations().is_empty(),
            "{:?}",
            env.safety.violations()
        );
    }

    // ===== 7) apply 前失败：Failed + last-good 保持（不调用 restore） =====

    #[test]
    fn job_failure_before_apply_fails_without_restore() {
        let env = TestEnv::new("fail-fit");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend = FakeBackend::new(calls.clone()).with_action(
            JobStage::FitP,
            FakeAction::Fail {
                code: "E_FIT",
                message: "modal fit did not converge",
            },
        );
        env.manager
            .start(full_request(true), Box::new(backend))
            .unwrap();
        let terminal = wait_terminal(&env.manager);
        assert_eq!(terminal.state, JobState::Failed);
        let failure = terminal.failure.expect("failure details required");
        assert_eq!(failure.code, "E_FIT");
        assert_eq!(failure.stage, Some(JobStage::FitP));
        let recovery = terminal.recovery.expect("recovery required");
        assert!(matches!(recovery.outcome, RecoveryOutcome::LastGood { .. }));
        assert_eq!(env.safety.count("restore_last_good"), 0);
        assert_eq!(env.safety.count("acquire"), 1);
        assert_eq!(env.safety.count("release:resume_playback"), 1);
        assert_eq!(env.load_checkpoint().state, JobState::Failed);
        assert!(
            env.safety.violations().is_empty(),
            "{:?}",
            env.safety.violations()
        );
    }

    // ===== 8) apply 后失败：自动回滚 -> RolledBack + restore last-good =====

    #[test]
    fn job_failure_after_apply_rolls_back_to_last_good() {
        let env = TestEnv::new("fail-remeasure");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend = FakeBackend::new(calls.clone()).with_action(
            JobStage::Remeasure,
            FakeAction::Fail {
                code: "E_REMEASURE",
                message: "post-apply remeasure failed",
            },
        );
        env.manager
            .start(full_request(true), Box::new(backend))
            .unwrap();
        let terminal = wait_terminal(&env.manager);
        assert_eq!(terminal.state, JobState::RolledBack);
        assert_eq!(terminal.failure.as_ref().unwrap().code, "E_REMEASURE");
        let recovery = terminal.recovery.expect("recovery required");
        assert!(matches!(recovery.outcome, RecoveryOutcome::LastGood { .. }));
        assert_eq!(env.safety.count("restore_last_good"), 1);
        assert_eq!(env.safety.count("acquire"), 2);
        assert_eq!(env.safety.count("release:resume_playback"), 2);
        assert!(!env.safety.is_device_held());
        assert_eq!(terminal.last_good_version.as_deref(), Some("restored-v1"));
        assert_eq!(env.load_checkpoint().state, JobState::RolledBack);
        assert!(
            env.safety.violations().is_empty(),
            "{:?}",
            env.safety.violations()
        );
    }

    // ===== 9) last-good 恢复失败 -> 安全 mute（Failed + Mute，绝不自称安全） =====

    #[test]
    fn job_recovery_falls_back_to_safe_mute() {
        let env = TestEnv::new("mute-fallback");
        env.safety.set_restore_fail(true);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend = FakeBackend::new(calls).with_action(
            JobStage::Remeasure,
            FakeAction::Fail {
                code: "E_REMEASURE",
                message: "post-apply remeasure failed",
            },
        );
        env.manager
            .start(full_request(true), Box::new(backend))
            .unwrap();
        let terminal = wait_terminal(&env.manager);
        assert_eq!(terminal.state, JobState::Failed);
        let recovery = terminal.recovery.expect("recovery required");
        assert!(matches!(recovery.outcome, RecoveryOutcome::Mute));
        assert_eq!(env.safety.count("activate_safe_mute"), 1);
        assert_eq!(env.safety.count("release:safety_mute"), 1);
        assert!(!env.safety.is_device_held());
        assert!(
            env.safety.violations().is_empty(),
            "{:?}",
            env.safety.violations()
        );
    }

    // ===== 10) apply commit 后 ack 失败 -> 回滚（RolledBack） =====

    #[test]
    fn job_apply_failure_after_commit_rolls_back() {
        let env = TestEnv::new("apply-fail-commit");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend =
            FakeBackend::new(calls).with_action(JobStage::Apply, FakeAction::FailAfterApplyCommit);
        env.manager
            .start(full_request(true), Box::new(backend))
            .unwrap();
        let terminal = wait_terminal(&env.manager);
        assert_eq!(terminal.state, JobState::RolledBack);
        assert_eq!(terminal.failure.as_ref().unwrap().code, "E_APPLY_ACK");
        assert!(terminal.applied_by_job);
        assert_eq!(env.safety.count("restore_last_good"), 1);
        assert!(matches!(
            terminal.recovery.expect("recovery").outcome,
            RecoveryOutcome::LastGood { .. }
        ));
        assert!(
            env.safety.violations().is_empty(),
            "{:?}",
            env.safety.violations()
        );
    }

    // ===== 11) apply commit 前失败 -> Failed（不需要回滚） =====

    #[test]
    fn job_apply_failure_without_commit_fails() {
        let env = TestEnv::new("apply-fail-nocommit");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend = FakeBackend::new(calls).with_action(
            JobStage::Apply,
            FakeAction::Fail {
                code: "E_APPLY_PREPARE",
                message: "prepare rejected before commit",
            },
        );
        env.manager
            .start(full_request(true), Box::new(backend))
            .unwrap();
        let terminal = wait_terminal(&env.manager);
        assert_eq!(terminal.state, JobState::Failed);
        assert!(!terminal.applied_by_job);
        assert_eq!(env.safety.count("restore_last_good"), 0);
        assert!(
            env.safety.violations().is_empty(),
            "{:?}",
            env.safety.violations()
        );
    }

    // ===== 12) 单 job 互斥：active 时 Busy；终态后可再 start =====

    #[test]
    fn job_busy_rejects_concurrent_start() {
        let env = TestEnv::new("busy");
        let gate = Gate::new();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend = FakeBackend::new(calls.clone())
            .with_action(JobStage::Preflight, FakeAction::Block(gate.clone()));
        env.manager
            .start(full_request(false), Box::new(backend))
            .unwrap();
        wait_state(&env.manager, Duration::from_secs(60), |snapshot| {
            snapshot.state == JobState::Active(JobStage::Preflight)
        });
        let second_calls = Arc::new(Mutex::new(Vec::new()));
        let second = env
            .manager
            .start(full_request(true), Box::new(FakeBackend::new(second_calls)));
        assert_eq!(second.unwrap_err(), JobError::Busy);
        gate.open();
        wait_state(&env.manager, Duration::from_secs(60), |snapshot| {
            snapshot.state == JobState::Active(JobStage::Prepared) && snapshot.awaiting_apply
        });
        env.manager.cancel().unwrap();
        let first_terminal = wait_terminal(&env.manager);
        assert_eq!(first_terminal.state, JobState::Cancelled);
        // 终态后可启动新 job（id 递增）
        let third_calls = Arc::new(Mutex::new(Vec::new()));
        let third_id = env
            .manager
            .start(
                full_request(true),
                Box::new(FakeBackend::new(third_calls.clone())),
            )
            .unwrap();
        assert_eq!(third_id.as_u64(), first_terminal.job_id.as_u64() + 1);
        let third_terminal = wait_terminal(&env.manager);
        assert_eq!(third_terminal.state, JobState::Complete);
        assert_eq!(
            third_calls.lock().unwrap().as_slice(),
            JobStage::ALL.as_slice()
        );
        assert!(
            env.safety.violations().is_empty(),
            "{:?}",
            env.safety.violations()
        );
    }

    // ===== 13) 无 active job 时 apply/cancel/rollback/wait 的错误 =====

    #[test]
    fn job_commands_require_active_job() {
        let env = TestEnv::new("no-active");
        assert_eq!(env.manager.cancel().unwrap_err(), JobError::NoActiveJob);
        assert_eq!(env.manager.apply().unwrap_err(), JobError::NoActiveJob);
        assert_eq!(env.manager.rollback().unwrap_err(), JobError::NoActiveJob);
        assert_eq!(
            env.manager
                .wait_terminal(Duration::from_millis(10))
                .unwrap_err(),
            JobError::NoActiveJob
        );
        assert!(env.manager.snapshot().is_none());
        // 未到 prepared 的 apply 被拒
        let gate = Gate::new();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend = FakeBackend::new(calls)
            .with_action(JobStage::Preflight, FakeAction::Block(gate.clone()));
        env.manager
            .start(full_request(false), Box::new(backend))
            .unwrap();
        wait_state(&env.manager, Duration::from_secs(60), |snapshot| {
            snapshot.state == JobState::Active(JobStage::Preflight)
        });
        assert!(matches!(
            env.manager.apply(),
            Err(JobError::InvalidState(_))
        ));
        gate.open();
        env.manager.cancel().unwrap();
        assert_eq!(wait_terminal(&env.manager).state, JobState::Cancelled);
    }

    // ===== 14) prepared 时显式 rollback -> RolledBack（未 apply，不发 restore） =====

    #[test]
    fn job_rollback_before_apply_is_rolled_back() {
        let env = TestEnv::new("rollback-prepared");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend = FakeBackend::new(calls).with_artifacts(true);
        env.manager
            .start(full_request(false), Box::new(backend))
            .unwrap();
        wait_state(&env.manager, Duration::from_secs(60), |snapshot| {
            snapshot.state == JobState::Active(JobStage::Prepared) && snapshot.awaiting_apply
        });
        env.manager.rollback().unwrap();
        let terminal = wait_terminal(&env.manager);
        assert_eq!(terminal.state, JobState::RolledBack);
        assert_eq!(env.safety.count("restore_last_good"), 0);
        assert_eq!(env.safety.count("release:resume_playback"), 1);
        assert!(matches!(
            terminal.recovery.expect("recovery").outcome,
            RecoveryOutcome::LastGood { .. }
        ));
        assert!(
            env.safety.violations().is_empty(),
            "{:?}",
            env.safety.violations()
        );
    }

    // ===== 15) apply 确认超时 -> Failed + last-good（不假成功） =====

    #[test]
    fn job_apply_confirm_timeout_fails_safe() {
        let env = TestEnv::with_timeout("apply-timeout", Some(Duration::from_millis(30)));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend = FakeBackend::new(calls.clone());
        env.manager
            .start(full_request(false), Box::new(backend))
            .unwrap();
        let terminal = wait_terminal(&env.manager);
        assert_eq!(terminal.state, JobState::Failed);
        assert_eq!(
            terminal.failure.as_ref().unwrap().code,
            "APPLY_CONFIRM_TIMEOUT"
        );
        assert_eq!(env.safety.count("restore_last_good"), 0);
        assert!(!calls.lock().unwrap().contains(&JobStage::Apply));
        assert_eq!(env.load_checkpoint().state, JobState::Failed);
    }

    // ===== 16) checkpoint 阶段粒度恢复：pending -> resume 重跑 in-progress stage =====

    #[test]
    fn job_checkpoint_roundtrip_and_resume() {
        let env = TestEnv::new("resume-source");
        let crafted = checkpoint_at(&env.root, JobId(7), JobState::Active(JobStage::FitP));
        fs::create_dir_all(&env.root.join("state")).unwrap();
        FileCheckpointStore::new(env.root.join("state").join(CHECKPOINT_FILE_NAME))
            .save(&crafted)
            .unwrap();
        // 模拟 daemon restart：新 manager 从同一 checkpoint 恢复
        let safety = FakeSafety::new();
        let manager = JobManager::new(test_config(&env.root, None), safety.clone()).unwrap();
        let pending = manager.pending_resume().expect("pending resume snapshot");
        assert_eq!(pending.state, JobState::Active(JobStage::FitP));
        assert_eq!(pending.job_id, JobId(7));
        // pending 未 resume 前 start 被拒
        let refused = manager.start(
            full_request(true),
            Box::new(FakeBackend::new(Arc::new(Mutex::new(Vec::new())))),
        );
        assert_eq!(refused.unwrap_err(), JobError::ResumePending);
        // resume：从 FitP 重跑（之前 stage 不重放）
        let calls = Arc::new(Mutex::new(Vec::new()));
        let resumed_id = manager
            .resume(Box::new(
                FakeBackend::new(calls.clone()).with_artifacts(true),
            ))
            .unwrap();
        assert_eq!(resumed_id, JobId(7));
        let terminal = manager.wait_terminal(Duration::from_secs(60)).unwrap();
        assert_eq!(terminal.state, JobState::Complete);
        assert_eq!(calls.lock().unwrap().first().copied(), Some(JobStage::FitP));
        assert!(!calls.lock().unwrap().contains(&JobStage::Preflight));
        assert_eq!(
            calls.lock().unwrap().last().copied(),
            Some(JobStage::IndependentHoldout)
        );
        // resume 后 checkpoint 变为终态
        let checkpoint =
            FileCheckpointStore::new(env.root.join("state").join(CHECKPOINT_FILE_NAME))
                .load()
                .unwrap()
                .unwrap();
        assert_eq!(checkpoint.state, JobState::Complete);
        let _ = manager.shutdown(Duration::from_secs(5));
    }

    // ===== 17) pending abandon：无法恢复时回 last-good/mute 并标 FAILED =====

    #[test]
    fn job_abandon_pending_marks_failed() {
        let env = TestEnv::new("abandon");
        let crafted = checkpoint_at(&env.root, JobId(9), JobState::Active(JobStage::FitP));
        fs::create_dir_all(&env.root.join("state")).unwrap();
        FileCheckpointStore::new(env.root.join("state").join(CHECKPOINT_FILE_NAME))
            .save(&crafted)
            .unwrap();
        let safety = FakeSafety::new();
        let manager = JobManager::new(test_config(&env.root, None), safety.clone()).unwrap();
        let snapshot = manager.abandon("daemon restart could not resume").unwrap();
        assert_eq!(snapshot.state, JobState::Failed);
        assert_eq!(snapshot.failure.as_ref().unwrap().code, "ABANDONED");
        assert!(matches!(
            snapshot.recovery.expect("recovery").outcome,
            RecoveryOutcome::LastGood { .. }
        ));
        assert!(manager.pending_resume().is_none());
        assert!(manager.snapshot().is_some());
        let checkpoint =
            FileCheckpointStore::new(env.root.join("state").join(CHECKPOINT_FILE_NAME))
                .load()
                .unwrap()
                .unwrap();
        assert_eq!(checkpoint.state, JobState::Failed);
        assert_eq!(safety.count("restore_last_good"), 0);
    }

    // ===== 18) Retest job：只做 preflight/remeasure/holdout，不 apply =====

    #[test]
    fn job_retest_measures_applied_set_without_apply() {
        let env = TestEnv::new("retest");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend = FakeBackend::new(calls.clone()).with_artifacts(true);
        env.manager
            .start(retest_request(), Box::new(backend))
            .unwrap();
        let terminal = wait_terminal(&env.manager);
        assert_eq!(terminal.state, JobState::Complete);
        assert_eq!(terminal.kind, JobKind::Retest);
        assert_eq!(terminal.stage_count, 3);
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            [
                JobStage::Preflight,
                JobStage::Remeasure,
                JobStage::IndependentHoldout
            ]
        );
        assert_eq!(terminal.applied_version.as_deref(), Some("applied-v1"));
        assert!(!terminal.applied_by_job);
        assert_eq!(terminal.artifacts.len(), 2);
        assert_eq!(env.safety.count("acquire"), 1);
        assert_eq!(env.safety.count("release:resume_playback"), 1);
        assert_eq!(env.safety.count("restore_last_good"), 0);
        // Retest 不支持 rollback（没有本 job apply 的 set）
        assert!(matches!(
            env.manager.rollback(),
            Err(JobError::InvalidState(_)) | Err(JobError::NoActiveJob)
        ));
        assert!(
            env.safety.violations().is_empty(),
            "{:?}",
            env.safety.violations()
        );
    }

    // ===== 19) 请求校验：非法参数整单拒绝（X02/P08/ACCEPTANCE） =====

    #[test]
    fn job_request_validation_rejects_invalid_requests() {
        let env = TestEnv::new("request-validation");
        let mut cases = Vec::new();
        let mut bad = full_request(true);
        bad.label = "  ".to_string();
        cases.push(bad);
        let mut bad = full_request(true);
        bad.sample_rate = 12_345;
        cases.push(bad);
        let mut bad = full_request(true);
        bad.design_points = 0;
        cases.push(bad);
        let mut bad = full_request(true);
        bad.holdout_points = 0;
        cases.push(bad);
        let mut bad = full_request(true);
        bad.repeats = 0;
        cases.push(bad);
        let mut bad = full_request(true);
        bad.drive_level_dbfs = f32::NAN;
        cases.push(bad);
        let mut bad = full_request(true);
        bad.audible_sweep_authorized = false;
        cases.push(bad);
        let mut bad = full_request(true);
        bad.expected_applied_version = Some("applied-v1".to_string());
        cases.push(bad);
        let mut bad = retest_request();
        bad.expected_applied_version = None;
        cases.push(bad);
        let mut bad = full_request(true);
        bad.inputs[0].sha256 = "xyz".to_string();
        cases.push(bad);
        let mut bad = full_request(true);
        bad.inputs.push(bad.inputs[0].clone());
        cases.push(bad);
        for (index, request) in cases.into_iter().enumerate() {
            let error = env
                .manager
                .start(
                    request,
                    Box::new(FakeBackend::new(Arc::new(Mutex::new(Vec::new())))),
                )
                .unwrap_err();
            assert!(
                matches!(error, JobError::InvalidRequest(_)),
                "case {index} expected InvalidRequest, got {error:?}"
            );
        }
        assert!(env.manager.snapshot().is_none());
    }

    // ===== 20) backend panic 被捕获并安全恢复；manager 仍可再用 =====

    #[test]
    fn job_backend_panic_is_contained_and_recovered() {
        let env = TestEnv::new("panic");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend =
            FakeBackend::new(calls.clone()).with_action(JobStage::FitP, FakeAction::Panic);
        env.manager
            .start(full_request(true), Box::new(backend))
            .unwrap();
        let terminal = wait_terminal(&env.manager);
        assert_eq!(terminal.state, JobState::Failed);
        assert_eq!(terminal.failure.as_ref().unwrap().code, "BACKEND_PANIC");
        assert!(matches!(
            terminal.recovery.expect("recovery").outcome,
            RecoveryOutcome::LastGood { .. }
        ));
        assert!(!env.safety.is_device_held());
        assert!(
            env.safety.violations().is_empty(),
            "{:?}",
            env.safety.violations()
        );
        // manager 仍可启动新 job
        let second_calls = Arc::new(Mutex::new(Vec::new()));
        env.manager
            .start(
                full_request(true),
                Box::new(FakeBackend::new(second_calls.clone())),
            )
            .unwrap();
        let second = wait_terminal(&env.manager);
        assert_eq!(second.state, JobState::Complete);
        assert_eq!(
            second_calls.lock().unwrap().as_slice(),
            JobStage::ALL.as_slice()
        );
        assert_eq!(second.failure, None);
    }

    // ===== 21) checkpoint 写失败 -> 明确失败并恢复，不静默继续 =====

    #[test]
    fn job_checkpoint_write_failure_fails_job() {
        let env = TestEnv::new("checkpoint-fail");
        let state_dir = env.root.join("state");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend = FakeBackend::new(calls)
            .with_action(JobStage::Preflight, FakeAction::DeleteDir(state_dir));
        env.manager
            .start(full_request(true), Box::new(backend))
            .unwrap();
        let terminal = wait_terminal(&env.manager);
        assert_eq!(terminal.state, JobState::Failed);
        assert_eq!(terminal.failure.as_ref().unwrap().code, "CHECKPOINT_IO");
        assert!(!terminal.evidence_errors.is_empty());
        assert!(matches!(
            terminal.recovery.expect("recovery").outcome,
            RecoveryOutcome::LastGood { .. }
        ));
        assert!(
            env.safety.violations().is_empty(),
            "{:?}",
            env.safety.violations()
        );
    }

    // ===== 22) artifact 不可变：同路径不同内容拒绝 =====

    #[test]
    fn job_artifact_duplicate_content_is_rejected() {
        let env = TestEnv::new("artifact-immutable");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend =
            FakeBackend::new(calls).with_action(JobStage::Preflight, FakeAction::ArtifactTwice);
        env.manager
            .start(full_request(true), Box::new(backend))
            .unwrap();
        let terminal = wait_terminal(&env.manager);
        assert_eq!(terminal.state, JobState::Failed);
        assert_eq!(
            terminal.failure.as_ref().unwrap().code,
            "E_ARTIFACT_IMMUTABLE"
        );
        assert_eq!(terminal.artifacts.len(), 1);
        assert_eq!(terminal.artifacts[0].relative_path, "preflight/meta.json");
    }

    // ===== 23) progress clamp/单调：越界值不放大 overall，终态为 1.0 =====

    #[test]
    fn job_progress_is_bounded_and_terminal() {
        let env = TestEnv::new("progress");
        let gate = Gate::new();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend = FakeBackend::new(calls).with_action(
            JobStage::Preflight,
            FakeAction::ProgressThenBlock {
                gate: gate.clone(),
                values: vec![-0.5, 0.5, 1.5],
            },
        );
        env.manager
            .start(full_request(true), Box::new(backend))
            .unwrap();
        let blocked = wait_state(&env.manager, Duration::from_secs(60), |snapshot| {
            snapshot.state == JobState::Active(JobStage::Preflight)
                && snapshot.stage_progress >= 0.999
        });
        assert!(blocked.stage_progress <= 1.0);
        approx(blocked.overall_progress, 0.1);
        gate.open();
        let terminal = wait_terminal(&env.manager);
        assert_eq!(terminal.state, JobState::Complete);
        approx(terminal.overall_progress, 1.0);
        approx(terminal.stage_progress, 1.0);
    }

    // ===== 24) checkpoint 跨 manager restart 可读（终态） =====

    #[test]
    fn job_terminal_checkpoint_survives_restart() {
        let env = TestEnv::new("restart-terminal");
        let calls = Arc::new(Mutex::new(Vec::new()));
        env.manager
            .start(full_request(true), Box::new(FakeBackend::new(calls)))
            .unwrap();
        let terminal = wait_terminal(&env.manager);
        assert_eq!(terminal.state, JobState::Complete);
        // 模拟 restart：同 checkpoint/artifacts 根的新 manager
        let safety = FakeSafety::new();
        let manager = JobManager::new(test_config(&env.root, None), safety).unwrap();
        assert!(manager.pending_resume().is_none());
        let restored = manager.snapshot().expect("terminal snapshot restored");
        assert_eq!(restored.state, JobState::Complete);
        assert_eq!(restored.job_id, terminal.job_id);
        // 新 job id 续号，不覆盖历史 artifact
        let next_calls = Arc::new(Mutex::new(Vec::new()));
        let next_id = manager
            .start(full_request(true), Box::new(FakeBackend::new(next_calls)))
            .unwrap();
        assert!(next_id.as_u64() > terminal.job_id.as_u64());
        let next_terminal = manager.wait_terminal(Duration::from_secs(60)).unwrap();
        assert_eq!(next_terminal.state, JobState::Complete);
        let _ = manager.shutdown(Duration::from_secs(5));
    }

    // ===== 25) 坏 checkpoint 整份拒绝（不静默清空） =====

    #[test]
    fn job_corrupt_checkpoint_is_rejected() {
        let root = temp_root("corrupt-checkpoint");
        let state_dir = root.join("state");
        fs::create_dir_all(&state_dir).unwrap();
        let path = state_dir.join(CHECKPOINT_FILE_NAME);
        fs::write(&path, b"this is not a checkpoint\n").unwrap();
        let safety = FakeSafety::new();
        let result = JobManager::new(test_config(&root, None), safety);
        assert!(matches!(result, Err(JobError::Checkpoint(_))));
        let _ = fs::remove_dir_all(&root);
    }

    // ===== 27) last-good 与 mute 都失败：如实记 Failed（不得自称安全） =====

    #[test]
    fn job_recovery_failed_when_mute_also_fails() {
        let env = TestEnv::new("recovery-failed");
        env.safety.set_restore_fail(true);
        env.safety.set_mute_fail(true);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend = FakeBackend::new(calls).with_action(
            JobStage::Remeasure,
            FakeAction::Fail {
                code: "E_REMEASURE",
                message: "post-apply remeasure failed",
            },
        );
        env.manager
            .start(full_request(true), Box::new(backend))
            .unwrap();
        let terminal = wait_terminal(&env.manager);
        assert_eq!(terminal.state, JobState::Failed);
        assert!(matches!(
            terminal.recovery.expect("recovery").outcome,
            RecoveryOutcome::Failed { .. }
        ));
        assert_eq!(env.safety.count("restore_last_good"), 1);
        assert_eq!(env.safety.count("activate_safe_mute"), 1);
    }

    // ===== 26) 静态禁令：job 层不 spawn 进程、不操作 systemd（失败只能 last-good/mute） =====

    #[test]
    fn job_layer_never_spawns_processes_or_uses_systemd() {
        let source = include_str!("job.rs");
        let forbidden_command = concat!("std::process", "::Command");
        let forbidden_new = concat!("Command", "::new");
        let forbidden_systemd = concat!("system", "ctl");
        assert!(!source.contains(forbidden_command));
        assert!(!source.contains(forbidden_new));
        assert!(!source.contains(forbidden_systemd));
        // 恢复路径只通过 SafeOutputController 的 last-good/mute 接口（见 trait 定义）。
        assert!(source.contains("fn restore_last_good"));
        assert!(source.contains("fn activate_safe_mute"));
    }
}
