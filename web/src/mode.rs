//! 运行模式（dante / aes67 / off）切换编排与持久化。
//!
//! 原则：Web 服务只做编排与校验，不把具体组件细节写死。
//! - 优先调用 audio-stack 提供的统一脚本 \`/usr/local/bin/tspi-mode dante|aes67|off\`
//!   （读 JSON 状态时用 \`tspi-mode status\`），脚本路径可配置；
//! - 脚本不可用时使用内置编排（systemctl 启停 + CamillaDSP 配置重载），组件与顺序来自 [mode.*] 配置；
//! - 同一时间只允许一个切换任务（在途返回 409）；失败回滚到切换前模式并记录错误；
//! - 当前模式持久化到 state.json，启动时按 restore_on_start 恢复。
//!
//! AES67 默认方案 B1：本机不做 PTP master，aes67 daemon 自带 PTPv2 slave，
//! 因此 [mode.aes67].ptp_services 为空，切换时不会启动 ptp4l/phc2sys；
//! 只有配置成 B2（ptp_services 含 ptp4l）时才走 linuxptp 编排。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::alsa::AlsaBackend;
use crate::camilla::extract_devices_fields;
use crate::camilla::{scan_xrun, CamillaBackend, XrunInfo};
use crate::cmd::CommandRunner;
use crate::config::{ModeConfig, ModeProfile, RunMode};
use crate::error::{AppError, AppResult};
use crate::ptp::PtpBackend;
use crate::service::{service_info, ServiceInfo, ServiceManager};
use crate::util;

// ===== 持久化 =====

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedMode {
    pub mode: RunMode,
    pub updated_at_unix: u64,
    #[serde(default)]
    pub last_error: Option<String>,
}

#[async_trait]
pub trait ModeStore: Send + Sync {
    async fn load(&self) -> Result<Option<PersistedMode>, String>;
    async fn save(&self, persisted: &PersistedMode) -> Result<(), String>;
}

/// 文件持久化（/etc/tspi-audio-web/state.json）。
pub struct FileModeStore {
    path: String,
}

impl FileModeStore {
    pub fn new(path: String) -> Self {
        Self { path }
    }
}

#[async_trait]
impl ModeStore for FileModeStore {
    async fn load(&self) -> Result<Option<PersistedMode>, String> {
        let path = std::path::Path::new(&self.path);
        if !tokio::fs::try_exists(path).await.unwrap_or(false) {
            return Ok(None);
        }
        let text = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| format!("读取 {} 失败: {e}", self.path))?;
        match serde_json::from_str::<PersistedMode>(&text) {
            Ok(p) => Ok(Some(p)),
            Err(e) => {
                eprintln!("warn: {} 解析失败（忽略）: {e}", self.path);
                Ok(None)
            }
        }
    }

    async fn save(&self, persisted: &PersistedMode) -> Result<(), String> {
        let text = serde_json::to_string_pretty(persisted).map_err(|e| e.to_string())?;
        let path = std::path::Path::new(&self.path);
        util::write_atomic(path, &text)
            .await
            .map_err(|e| format!("写入 {} 失败: {e}", self.path))?;
        Ok(())
    }
}

/// 内存持久化（mock / 测试）。
pub struct MockModeStore {
    inner: Mutex<Option<PersistedMode>>,
}

impl MockModeStore {
    pub fn new(initial: Option<PersistedMode>) -> Self {
        Self {
            inner: Mutex::new(initial),
        }
    }
    pub fn get(&self) -> Option<PersistedMode> {
        self.inner.lock().unwrap().clone()
    }
}

#[async_trait]
impl ModeStore for MockModeStore {
    async fn load(&self) -> Result<Option<PersistedMode>, String> {
        Ok(self.get())
    }
    async fn save(&self, persisted: &PersistedMode) -> Result<(), String> {
        *self.inner.lock().unwrap() = Some(persisted.clone());
        Ok(())
    }
}

// ===== 状态视图 =====

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SwitchState {
    Idle,
    Switching,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Running,
    Done,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize)]
pub struct StepView {
    pub key: String,
    pub label: String,
    pub status: StepStatus,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileView {
    pub mode: String,
    pub label: String,
    pub ptp_version: String,
    pub ptp_source: String,
    pub ptp_role: String,
    pub ptp_services: Vec<String>,
    pub aoip_service: String,
    pub camilla_config: String,
    pub input_format: String,
    pub output_format: String,
    pub default_chunksize: u32,
    pub default_target_level: u32,
}

/// audio-stack `tspi-mode status --json` 的状态（schema = tspi-audio/v1）。
/// 未知/新增字段用 Value 保留，避免 Web 端成为脚本契约的瓶颈。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScriptStatus {
    /// 契约版本，例如 "tspi-audio/v1"。
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub pid: Option<u64>,
    #[serde(default)]
    pub ptp_role: Option<String>,
    #[serde(default)]
    pub ptp_lock: Option<bool>,
    #[serde(default)]
    pub ptp_source: Option<String>,
    #[serde(default)]
    pub ok: Option<bool>,
    #[serde(default)]
    pub message: Option<String>,
    /// 嵌套状态摘要（契约保留原样，前端只做展示）。
    #[serde(default)]
    pub ptp: Option<serde_json::Value>,
    #[serde(default)]
    pub aoip: Option<serde_json::Value>,
    #[serde(default)]
    pub camilladsp: Option<serde_json::Value>,
    #[serde(default)]
    pub alsa: Option<serde_json::Value>,
    #[serde(default)]
    pub last_error: Option<serde_json::Value>,
}

/// tspi-mode 退出码含义（audio-stack 契约）。
pub fn script_exit_meaning(code: i32) -> &'static str {
    match code {
        0 => "就绪",
        2 => "参数错误",
        3 => "前置条件不满足",
        4 => "设备未就绪",
        5 => "PTP 未锁定",
        7 => "DSP 启动失败",
        8 => "自检失败",
        9 => "失败并已自动回滚",
        10 => "另一切换正在进行中",
        _ => "未知错误",
    }
}

/// 持久化模式（用于页面展示“上次模式”，D4 默认不自动恢复）。
#[derive(Debug, Clone, Serialize, Default)]
pub struct PersistedView {
    pub mode: RunMode,
    pub mode_label: String,
    pub updated_at_unix: u64,
    pub last_error: Option<String>,
    pub auto_restore: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModeStatusView {
    pub mode: RunMode,
    pub mode_label: String,
    pub target: Option<RunMode>,
    pub target_label: Option<String>,
    pub state: SwitchState,
    pub step: Option<String>,
    pub step_label: Option<String>,
    pub progress: u8,
    pub steps: Vec<StepView>,
    pub last_error: Option<String>,
    pub warnings: Vec<String>,
    pub started_unix: Option<u64>,
    pub finished_unix: Option<u64>,
    pub enabled: bool,
    pub restore_on_start: bool,
    pub switch_script: String,
    pub strategy: String,
    pub profiles: Vec<ProfileView>,
    pub script_status: Option<ScriptStatus>,
    /// 上次持久化的模式（可能未加载；D4 默认启动不自动恢复）。
    pub persisted: Option<PersistedView>,
}

impl ModeStatusView {
    pub fn state_label(&self) -> &'static str {
        match self.state {
            SwitchState::Idle => "空闲",
            SwitchState::Switching => "切换中",
            SwitchState::Failed => "上次切换失败",
        }
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PtpComponent {
    pub version: String,
    pub source: String,
    pub role: String,
    pub locked: bool,
    pub offset_ns: Option<i64>,
    pub port_state: Option<String>,
    pub gm_present: Option<bool>,
    pub detail: String,
    pub service: Option<ServiceInfo>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct AoipComponent {
    pub kind: Option<String>,
    pub label: Option<String>,
    pub service: Option<ServiceInfo>,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct CamillaComponent {
    pub service: Option<ServiceInfo>,
    pub reachable: bool,
    pub state: Option<String>,
    pub stop_reason: Option<String>,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub capture_channels: Option<u32>,
    pub playback_channels: Option<u32>,
    pub xrun_recent: u64,
    pub xrun_hint: String,
    pub config_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct AudioComponent {
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u32>,
    pub channels: Option<u32>,
    pub chunksize: Option<u32>,
    pub target_level: Option<u32>,
    pub input_format: String,
    pub output_format: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ModeComponents {
    pub ptp: PtpComponent,
    pub aoip: AoipComponent,
    pub camilla: CamillaComponent,
    pub audio: AudioComponent,
}

// ===== 内部运行状态 =====

struct Runtime {
    mode: RunMode,
    state: SwitchState,
    target: Option<RunMode>,
    steps: Vec<StepView>,
    last_error: Option<String>,
    warnings: Vec<String>,
    started_unix: Option<u64>,
    finished_unix: Option<u64>,
    strategy: String,
}

impl Runtime {
    fn new(mode: RunMode) -> Self {
        Self {
            mode,
            state: SwitchState::Idle,
            target: None,
            steps: Vec::new(),
            last_error: None,
            warnings: Vec::new(),
            started_unix: None,
            finished_unix: None,
            strategy: "builtin".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepKey {
    StopCamilla,
    StopAoip,
    StopPtp,
    PtpInternal,
    StartPtp,
    WaitPtp,
    StartAoip,
    WaitAoip,
    ApplyCamilla,
    StartCamilla,
    Verify,
    VerifyOff,
    ScriptSwitch,
    ScriptStatus,
    ScriptVerify,
}

impl StepKey {
    fn as_str(self) -> &'static str {
        match self {
            Self::StopCamilla => "stop_camilla",
            Self::StopAoip => "stop_aoip",
            Self::StopPtp => "stop_ptp",
            Self::PtpInternal => "ptp_internal",
            Self::StartPtp => "start_ptp",
            Self::WaitPtp => "wait_ptp",
            Self::StartAoip => "start_aoip",
            Self::WaitAoip => "wait_aoip",
            Self::ApplyCamilla => "apply_camilla_config",
            Self::StartCamilla => "start_camilla",
            Self::Verify => "verify",
            Self::VerifyOff => "verify_off",
            Self::ScriptSwitch => "script_switch",
            Self::ScriptStatus => "script_status",
            Self::ScriptVerify => "script_verify",
        }
    }
}

pub struct ModeController {
    cfg: ModeConfig,
    services: Arc<dyn ServiceManager>,
    camilla: Arc<dyn CamillaBackend>,
    ptp: Arc<dyn PtpBackend>,
    alsa: Arc<dyn AlsaBackend>,
    runner: Arc<dyn CommandRunner>,
    store: Arc<dyn ModeStore>,
    mock: bool,
    step_delay: Duration,
    runtime: Mutex<Runtime>,
    script_status: Mutex<Option<ScriptStatus>>,
}

impl ModeController {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cfg: ModeConfig,
        services: Arc<dyn ServiceManager>,
        camilla: Arc<dyn CamillaBackend>,
        ptp: Arc<dyn PtpBackend>,
        alsa: Arc<dyn AlsaBackend>,
        runner: Arc<dyn CommandRunner>,
        store: Arc<dyn ModeStore>,
        mock: bool,
        step_delay: Duration,
    ) -> Self {
        Self {
            cfg,
            services,
            camilla,
            ptp,
            alsa,
            runner,
            store,
            mock,
            step_delay,
            runtime: Mutex::new(Runtime::new(RunMode::Off)),
            script_status: Mutex::new(None),
        }
    }

    fn profile(&self, mode: RunMode) -> Option<&ModeProfile> {
        self.cfg.profile(mode)
    }

    /// 关闭模式展示 PTP 时使用的 linuxptp 单元名（[mode] 配置不持有该字段，用默认 ptp4l.service）。
    fn cfg_profile_ptp4l(&self) -> String {
        "ptp4l.service".to_string()
    }

    fn label(&self, mode: RunMode) -> String {
        self.profile(mode)
            .map(|p| p.label.clone())
            .unwrap_or_else(|| "关闭（Off）".into())
    }

    fn fmt_profile(p: &ModeProfile) -> (String, String) {
        let input = format!(
            "{} kHz / {}-bit / {} ch",
            p.input_sample_rate as f64 / 1000.0,
            p.input_bit_depth,
            p.channels
        );
        let output = format!(
            "MOTU {} kHz / {}-bit / {} ch",
            p.output_sample_rate as f64 / 1000.0,
            p.output_bit_depth,
            p.channels
        );
        (input, output)
    }

    pub fn profiles(&self) -> Vec<ProfileView> {
        [RunMode::Dante, RunMode::Aes67]
            .iter()
            .filter_map(|m| self.profile(*m).map(|p| (*m, p)))
            .map(|(m, p)| {
                let (input, output) = Self::fmt_profile(p);
                ProfileView {
                    mode: m.as_str().to_string(),
                    label: p.label.clone(),
                    ptp_version: p.ptp_version.clone(),
                    ptp_source: p.ptp_source.clone(),
                    ptp_role: p.ptp_role.clone(),
                    ptp_services: p.ptp_services.clone(),
                    aoip_service: p.aoip_service.clone(),
                    camilla_config: p.camilla_config.clone(),
                    input_format: input,
                    output_format: output,
                    default_chunksize: p.default_chunksize,
                    default_target_level: p.default_target_level,
                }
            })
            .collect()
    }

    pub fn status(&self) -> ModeStatusView {
        self.status_inner(None)
    }

    /// 带持久化状态的视图（GET /api/mode 使用；D4：仅展示，不自动恢复）。
    pub async fn status_with_persisted(&self) -> ModeStatusView {
        let persisted = self.store.load().await.ok().flatten();
        self.status_inner(persisted)
    }

    fn status_inner(&self, persisted: Option<PersistedMode>) -> ModeStatusView {
        let rt = self.runtime.lock().unwrap();
        let total = rt.steps.len();
        let done = rt
            .steps
            .iter()
            .filter(|s| {
                matches!(
                    s.status,
                    StepStatus::Done | StepStatus::Skipped | StepStatus::Failed
                )
            })
            .count();
        let mut progress = (done.saturating_mul(100) / total.max(1)).min(100) as u8;
        if rt.state == SwitchState::Switching {
            progress = progress.min(99);
        }
        let running = rt.steps.iter().find(|s| s.status == StepStatus::Running);
        ModeStatusView {
            mode: rt.mode,
            mode_label: self.label(rt.mode),
            target: rt.target,
            target_label: rt.target.map(|m| self.label(m)),
            state: rt.state,
            step: running
                .map(|s| s.key.clone())
                .or_else(|| rt.steps.last().map(|s| s.key.clone())),
            step_label: running
                .map(|s| s.label.clone())
                .or_else(|| rt.steps.last().map(|s| s.label.clone())),
            progress,
            steps: rt.steps.clone(),
            last_error: rt.last_error.clone(),
            warnings: rt.warnings.clone(),
            started_unix: rt.started_unix,
            finished_unix: rt.finished_unix,
            enabled: self.cfg.enabled,
            restore_on_start: self.cfg.restore_on_start,
            switch_script: self.cfg.switch_script.clone(),
            strategy: rt.strategy.clone(),
            profiles: self.profiles(),
            script_status: self.script_status.lock().unwrap().clone(),
            persisted: persisted.map(|p| PersistedView {
                mode_label: self.label(p.mode),
                mode: p.mode,
                updated_at_unix: p.updated_at_unix,
                last_error: p.last_error,
                auto_restore: self.cfg.restore_on_start,
            }),
        }
    }

    /// 当前已确认模式（不含切换中的目标）。
    pub fn current_mode(&self) -> RunMode {
        self.runtime.lock().unwrap().mode
    }

    fn build_plan(
        &self,
        target: RunMode,
        _previous: RunMode,
        use_script: bool,
    ) -> Vec<(StepKey, &'static str)> {
        if use_script {
            let mut plan = vec![
                (StepKey::ScriptSwitch, "调用 audio-stack 切换脚本"),
                (StepKey::ScriptStatus, "读取 tspi-mode status --json"),
            ];
            if target != RunMode::Off {
                plan.push((StepKey::ScriptVerify, "校验脚本切换结果"));
            }
            return plan;
        }
        let mut plan = vec![
            (StepKey::StopCamilla, "停止 CamillaDSP"),
            (StepKey::StopAoip, "停止当前 AoIP 服务"),
            (StepKey::StopPtp, "停止当前 PTP 服务"),
        ];
        match target {
            RunMode::Off => {
                plan.push((StepKey::VerifyOff, "确认服务已全部停止"));
            }
            _ => {
                let profile = self.profile(target);
                let has_ptp = profile.map(|p| !p.ptp_services.is_empty()).unwrap_or(false);
                if has_ptp {
                    plan.push((StepKey::StartPtp, "启动目标 PTP 服务"));
                    plan.push((StepKey::WaitPtp, "等待 PTP 锁定"));
                } else {
                    plan.push((StepKey::PtpInternal, "PTP 由 AoIP 服务内置处理"));
                }
                plan.push((StepKey::StartAoip, "启动目标 AoIP 服务"));
                plan.push((StepKey::WaitAoip, "等待 AoIP 服务就绪"));
                plan.push((StepKey::ApplyCamilla, "应用 CamillaDSP 模式配置"));
                plan.push((StepKey::StartCamilla, "启动 CamillaDSP"));
                plan.push((StepKey::Verify, "校验运行状态"));
            }
        }
        plan
    }

    async fn script_available(&self) -> bool {
        if self.mock {
            return false;
        }
        let p = self.cfg.switch_script.trim();
        if p.is_empty() {
            return false;
        }
        tokio::fs::metadata(p)
            .await
            .map(|m| m.is_file())
            .unwrap_or(false)
    }

    /// 开始切换：立即返回 202 视图；互斥冲突时返回 Conflict(409)。
    pub async fn begin_switch(self: &Arc<Self>, target: RunMode) -> AppResult<ModeStatusView> {
        if !self.cfg.enabled {
            return Err(AppError::forbidden(
                "运行模式切换未启用（[mode] enabled = false）",
            ));
        }
        let use_script = self.script_available().await;
        let mut rt = self.runtime.lock().unwrap();
        if rt.state == SwitchState::Switching {
            return Err(AppError::conflict(format!(
                "正在切换到 {}，请等待当前切换完成",
                rt.target.map(|m| self.label(m)).unwrap_or_default()
            )));
        }
        if target == rt.mode {
            return Err(AppError::conflict(format!(
                "已是当前模式（{}），无需切换",
                self.label(target)
            )));
        }
        let previous = rt.mode;
        let plan = self.build_plan(target, previous, use_script);
        let now = util::now_unix();
        let mut steps: Vec<StepView> = plan
            .iter()
            .map(|(key, label)| StepView {
                key: key.as_str().to_string(),
                label: (*label).to_string(),
                status: StepStatus::Pending,
                detail: None,
            })
            .collect();
        if let Some(idx) = plan.iter().position(|(k, _)| *k == StepKey::PtpInternal) {
            let detail = self
                .profile(target)
                .map(|p| {
                    format!(
                        "方案 B1：PTP 由 {}（{}）内置 {} 提供，不启动 linuxptp",
                        p.aoip_service, p.ptp_source, p.ptp_role
                    )
                })
                .unwrap_or_default();
            steps[idx].status = StepStatus::Skipped;
            steps[idx].detail = Some(detail);
        }
        let mut warnings = Vec::new();
        if !self.mock && !self.cfg.switch_script.trim().is_empty() && !use_script {
            warnings.push(format!(
                "切换脚本 {} 不可用，已回退到内置编排",
                self.cfg.switch_script
            ));
        }
        if self.mock {
            warnings.push("mock 模式：仅模拟切换流程，不执行真实 systemctl/脚本".into());
        }
        rt.state = SwitchState::Switching;
        rt.target = Some(target);
        rt.steps = steps;
        rt.last_error = None;
        rt.warnings = warnings;
        rt.started_unix = Some(now);
        rt.finished_unix = None;
        rt.strategy = if use_script {
            "script".into()
        } else {
            "builtin".into()
        };
        drop(rt);

        let controller = Arc::clone(self);
        tokio::spawn(async move {
            controller.run_switch(target, previous, use_script).await;
        });
        Ok(self.status())
    }

    async fn run_switch(self: Arc<Self>, target: RunMode, previous: RunMode, use_script: bool) {
        let plan = self.build_plan(target, previous, use_script);
        let result = if use_script {
            self.execute_script_switch(target, &plan).await
        } else {
            self.execute_builtin_switch(target, previous, &plan).await
        };
        let now = util::now_unix();
        match result {
            Ok(()) => {
                let persisted = PersistedMode {
                    mode: target,
                    updated_at_unix: now,
                    last_error: None,
                };
                if let Err(e) = self.store.save(&persisted).await {
                    eprintln!("warn: 持久化运行模式失败: {e}");
                }
                let mut rt = self.runtime.lock().unwrap();
                rt.mode = target;
                rt.state = SwitchState::Idle;
                rt.target = None;
                rt.finished_unix = Some(now);
                rt.last_error = None;
            }
            Err(err) => {
                let rollback_note = self.rollback(previous, target, use_script).await;
                let mut message = err;
                if !rollback_note.is_empty() {
                    message.push('；');
                    message.push_str(&rollback_note);
                }
                let persisted = PersistedMode {
                    mode: previous,
                    updated_at_unix: now,
                    last_error: Some(message.clone()),
                };
                if let Err(e) = self.store.save(&persisted).await {
                    eprintln!("warn: 持久化运行模式失败: {e}");
                }
                let mut rt = self.runtime.lock().unwrap();
                rt.mode = previous;
                rt.state = SwitchState::Failed;
                rt.target = None;
                rt.finished_unix = Some(now);
                rt.last_error = Some(message);
            }
        }
    }

    fn step_index(plan: &[(StepKey, &'static str)], key: StepKey) -> Option<usize> {
        plan.iter().position(|(k, _)| *k == key)
    }

    fn set_step(&self, index: usize, status: StepStatus, detail: Option<String>) {
        let mut rt = self.runtime.lock().unwrap();
        if let Some(step) = rt.steps.get_mut(index) {
            step.status = status;
            if detail.is_some() {
                step.detail = detail;
            }
        }
    }

    fn step_failed(&self, index: usize, err: &str) {
        self.set_step(index, StepStatus::Failed, Some(err.to_string()));
    }

    fn push_step(&self, key: &str, label: &str) -> usize {
        let mut rt = self.runtime.lock().unwrap();
        rt.steps.push(StepView {
            key: key.to_string(),
            label: label.to_string(),
            status: StepStatus::Pending,
            detail: None,
        });
        rt.steps.len() - 1
    }

    async fn pause(&self) {
        if !self.step_delay.is_zero() {
            tokio::time::sleep(self.step_delay).await;
        }
    }

    async fn execute_builtin_switch(
        &self,
        target: RunMode,
        previous: RunMode,
        plan: &[(StepKey, &'static str)],
    ) -> Result<(), String> {
        // 1) 停止 CamillaDSP
        if let Some(i) = Self::step_index(plan, StepKey::StopCamilla) {
            self.set_step(i, StepStatus::Running, None);
            match self.services.stop(self.camilla.service()).await {
                Ok(()) => self.set_step(i, StepStatus::Done, Some("CamillaDSP 已停止".into())),
                Err(e) => {
                    self.step_failed(i, &e.to_string());
                    return Err(format!("停止 CamillaDSP 失败: {e}"));
                }
            }
            self.pause().await;
        }

        // 2) 停止当前 AoIP 服务
        if let Some(i) = Self::step_index(plan, StepKey::StopAoip) {
            self.set_step(i, StepStatus::Running, None);
            match self.profile(previous) {
                Some(p) if !p.aoip_service.trim().is_empty() => {
                    match self.services.stop(&p.aoip_service).await {
                        Ok(()) => self.set_step(
                            i,
                            StepStatus::Done,
                            Some(format!("已停止 {}", p.aoip_service)),
                        ),
                        Err(e) => {
                            self.step_failed(i, &e.to_string());
                            return Err(format!("停止 {} 失败: {e}", p.aoip_service));
                        }
                    }
                }
                _ => self.set_step(i, StepStatus::Done, Some("当前模式未运行 AoIP 服务".into())),
            }
            self.pause().await;
        }

        // 3) 停止当前 PTP 服务
        if let Some(i) = Self::step_index(plan, StepKey::StopPtp) {
            self.set_step(i, StepStatus::Running, None);
            let units = self
                .profile(previous)
                .map(|p| p.ptp_services.clone())
                .unwrap_or_default();
            if units.is_empty() {
                self.set_step(
                    i,
                    StepStatus::Done,
                    Some("当前模式未运行 linuxptp（AES67 B1 由 daemon 内置 PTP）".into()),
                );
            } else {
                for unit in &units {
                    if let Err(e) = self.services.stop(unit).await {
                        self.step_failed(i, &e.to_string());
                        return Err(format!("停止 PTP 服务 {unit} 失败: {e}"));
                    }
                }
                self.set_step(
                    i,
                    StepStatus::Done,
                    Some(format!("已停止 {}", units.join(", "))),
                );
            }
            self.pause().await;
        }

        if target == RunMode::Off {
            if let Some(i) = Self::step_index(plan, StepKey::VerifyOff) {
                self.set_step(i, StepStatus::Running, None);
                let mut active = Vec::new();
                if self
                    .services
                    .status(self.camilla.service())
                    .await
                    .map(|s| s.active)
                    .unwrap_or(false)
                {
                    active.push(self.camilla.service().to_string());
                }
                if let Some(p) = self.profile(previous) {
                    for unit in p
                        .ptp_services
                        .iter()
                        .chain(std::iter::once(&p.aoip_service))
                    {
                        if unit.trim().is_empty() {
                            continue;
                        }
                        if self
                            .services
                            .status(unit)
                            .await
                            .map(|s| s.active)
                            .unwrap_or(false)
                        {
                            active.push(unit.clone());
                        }
                    }
                }
                if active.is_empty() {
                    self.set_step(i, StepStatus::Done, Some("所有音频服务已停止".into()));
                } else {
                    let msg = format!("以下服务仍在运行: {}", active.join(", "));
                    self.step_failed(i, &msg);
                    return Err(msg);
                }
                self.pause().await;
            }
            return Ok(());
        }

        let profile = self
            .profile(target)
            .cloned()
            .ok_or_else(|| format!("未知目标模式: {}", target.as_str()))?;

        // 4) 启动目标 PTP（若配置）
        if let Some(i) = Self::step_index(plan, StepKey::StartPtp) {
            self.set_step(i, StepStatus::Running, None);
            for unit in &profile.ptp_services {
                if let Err(e) = self.services.start(unit).await {
                    self.step_failed(i, &e.to_string());
                    return Err(format!("启动 PTP 服务 {unit} 失败: {e}"));
                }
            }
            self.set_step(
                i,
                StepStatus::Done,
                Some(format!("已启动 {}", profile.ptp_services.join(", "))),
            );
            self.pause().await;
        }

        // 5) 等待 PTP 锁定
        if let Some(i) = Self::step_index(plan, StepKey::WaitPtp) {
            self.set_step(i, StepStatus::Running, None);
            let timeout = Duration::from_secs(self.cfg.ptp_lock_timeout_secs.max(5));
            let started = Instant::now();
            loop {
                let status = self
                    .ptp
                    .status(&profile.ptp_source, Some(&profile.aoip_service))
                    .await;
                let last = status.lock_detail.clone();
                if status.locked {
                    self.set_step(
                        i,
                        StepStatus::Done,
                        Some(format!("{} 已锁定（{}）", profile.ptp_version, last)),
                    );
                    break;
                }
                if started.elapsed() > timeout {
                    let msg = format!(
                        "等待 {} 锁定超时（{} 秒）：{}",
                        profile.ptp_version,
                        timeout.as_secs(),
                        last
                    );
                    self.step_failed(i, &msg);
                    return Err(msg);
                }
                tokio::time::sleep(Duration::from_millis(self.cfg.poll_ms.clamp(100, 5000))).await;
            }
            self.pause().await;
        }

        // 6) 启动目标 AoIP
        if let Some(i) = Self::step_index(plan, StepKey::StartAoip) {
            self.set_step(i, StepStatus::Running, None);
            if let Err(e) = self.services.start(&profile.aoip_service).await {
                self.step_failed(i, &e.to_string());
                return Err(format!("启动 {} 失败: {e}", profile.aoip_service));
            }
            self.set_step(
                i,
                StepStatus::Done,
                Some(format!("已启动 {}", profile.aoip_service)),
            );
            self.pause().await;
        }

        // 7) 等待 AoIP 就绪
        if let Some(i) = Self::step_index(plan, StepKey::WaitAoip) {
            self.set_step(i, StepStatus::Running, None);
            let timeout = Duration::from_secs(self.cfg.aoip_ready_timeout_secs.max(5));
            let started = Instant::now();
            loop {
                match self.services.status(&profile.aoip_service).await {
                    Ok(st) if st.active => {
                        self.set_step(
                            i,
                            StepStatus::Done,
                            Some(format!(
                                "{} 已运行（{}）",
                                profile.aoip_service, st.active_state
                            )),
                        );
                        break;
                    }
                    Ok(st) => {
                        if started.elapsed() > timeout {
                            let msg = format!(
                                "等待 {} 就绪超时（{} 秒，状态 {}）",
                                profile.aoip_service,
                                timeout.as_secs(),
                                st.active_state
                            );
                            self.step_failed(i, &msg);
                            return Err(msg);
                        }
                    }
                    Err(e) => {
                        if started.elapsed() > timeout {
                            let msg = format!(
                                "无法读取 {} 状态（{} 秒）: {e}",
                                profile.aoip_service,
                                timeout.as_secs()
                            );
                            self.step_failed(i, &msg);
                            return Err(msg);
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(self.cfg.poll_ms.clamp(100, 5000))).await;
            }
            self.pause().await;
        }

        // 8) 应用 CamillaDSP 模式配置
        if let Some(i) = Self::step_index(plan, StepKey::ApplyCamilla) {
            self.set_step(i, StepStatus::Running, None);
            match self
                .camilla
                .apply_config_path(&profile.camilla_config)
                .await
            {
                Ok(outcome) => self.set_step(
                    i,
                    StepStatus::Done,
                    Some(format!(
                        "{}（{}）",
                        outcome.config_path, outcome.applied_via
                    )),
                ),
                Err(e) => {
                    self.step_failed(i, &e.to_string());
                    return Err(format!("应用 CamillaDSP 配置失败: {e}"));
                }
            }
            self.pause().await;
        }

        // 9) 启动 CamillaDSP
        if let Some(i) = Self::step_index(plan, StepKey::StartCamilla) {
            self.set_step(i, StepStatus::Running, None);
            match self.services.start(self.camilla.service()).await {
                Ok(()) => self.set_step(
                    i,
                    StepStatus::Done,
                    Some(format!("已启动 {}", self.camilla.service())),
                ),
                Err(e) => {
                    self.step_failed(i, &e.to_string());
                    return Err(format!("启动 CamillaDSP 失败: {e}"));
                }
            }
            self.pause().await;
        }

        // 10) 校验
        if let Some(i) = Self::step_index(plan, StepKey::Verify) {
            self.set_step(i, StepStatus::Running, None);
            match self.verify_target(&profile).await {
                Ok(detail) => self.set_step(i, StepStatus::Done, Some(detail)),
                Err(e) => {
                    self.step_failed(i, &e);
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    async fn verify_target(&self, profile: &ModeProfile) -> Result<String, String> {
        let timeout = Duration::from_secs(self.cfg.camilla_ready_timeout_secs.max(5));
        let started = Instant::now();
        let mut last = String::from("CamillaDSP 尚未响应");
        loop {
            let live = self.camilla.live().await;
            if live.reachable && live.is_running() {
                let ptp = self
                    .ptp
                    .status(&profile.ptp_source, Some(&profile.aoip_service))
                    .await;
                let alsa = self.alsa.view().await;
                let logs = self
                    .services
                    .logs(self.camilla.service(), 80)
                    .await
                    .unwrap_or_default();
                let xrun = scan_xrun(&logs);
                let ptp_note = if profile.ptp_services.is_empty() {
                    format!(
                        "PTP {}（{}）{}",
                        ptp.ptp_version,
                        ptp.ptp_source,
                        if ptp.locked {
                            "锁定"
                        } else {
                            "状态未知/未锁定（B1 无公开接口）"
                        }
                    )
                } else {
                    format!(
                        "PTP {} {}",
                        ptp.ptp_version,
                        if ptp.locked {
                            format!("已锁定 offset={:?}ns", ptp.offset_ns)
                        } else {
                            "未锁定".to_string()
                        }
                    )
                };
                return Ok(format!(
                    "CamillaDSP Running；{}；声卡 {} 张；最近 xrun 线索 {} 条",
                    ptp_note,
                    alsa.cards.len(),
                    xrun.recent_count
                ));
            }
            if let Some(e) = &live.error {
                last = e.clone();
            } else if let Some(state) = &live.state {
                last = format!("state={state}");
            }
            if started.elapsed() > timeout {
                return Err(format!(
                    "等待 CamillaDSP 进入 Running 超时（{} 秒）：{last}",
                    timeout.as_secs()
                ));
            }
            tokio::time::sleep(Duration::from_millis(self.cfg.poll_ms.clamp(100, 5000))).await;
        }
    }

    async fn execute_script_switch(
        &self,
        target: RunMode,
        plan: &[(StepKey, &'static str)],
    ) -> Result<(), String> {
        // 1) tspi-mode <mode> --json
        if let Some(i) = Self::step_index(plan, StepKey::ScriptSwitch) {
            self.set_step(i, StepStatus::Running, None);
            let timeout = Duration::from_secs(self.cfg.switch_timeout_secs.max(10));
            let out = self
                .runner
                .run_with_timeout(
                    &self.cfg.switch_script,
                    &[target.as_str(), "--json"],
                    timeout,
                )
                .await
                .map_err(|e| {
                    self.step_failed(i, &e.to_string());
                    format!("执行切换脚本失败: {e}")
                })?;
            let status = parse_script_status(&out.stdout);
            if let Some(s) = &status {
                *self.script_status.lock().unwrap() = Some(s.clone());
            }
            if !out.success {
                let code = out.status.unwrap_or(-1);
                let meaning = script_exit_meaning(code);
                let mut msg = format!("切换脚本失败（退出码 {code}：{meaning}）");
                if code == 9 {
                    msg.push_str("；脚本已回滚到切换前模式");
                } else if code == 10 {
                    msg.push_str("；另一切换正在进行中（请稍后重试）");
                }
                if let Some(detail) = script_error_detail(&status, &out) {
                    msg.push('：');
                    msg.push_str(&detail);
                }
                self.step_failed(i, &msg);
                return Err(msg);
            }
            if status.as_ref().and_then(|s| s.ok) == Some(false) {
                let msg = format!(
                    "切换脚本报告失败: {}",
                    script_error_detail(&status, &out).unwrap_or_else(|| "未提供原因".into())
                );
                self.step_failed(i, &msg);
                return Err(msg);
            }
            let schema_note = status
                .as_ref()
                .and_then(|s| s.schema.clone())
                .map(|s| format!("，schema={s}"))
                .unwrap_or_default();
            self.set_step(
                i,
                StepStatus::Done,
                Some(format!(
                    "{} {} 执行成功（退出码 0{schema_note}）",
                    self.cfg.switch_script,
                    target.as_str()
                )),
            );
            self.pause().await;
        }

        // 2) tspi-mode status --json
        if let Some(i) = Self::step_index(plan, StepKey::ScriptStatus) {
            self.set_step(i, StepStatus::Running, None);
            let timeout = Duration::from_secs(self.cfg.switch_timeout_secs.clamp(5, 30));
            let mut detail = String::from("脚本未返回状态 JSON");
            match self
                .runner
                .run_with_timeout(&self.cfg.switch_script, &["status", "--json"], timeout)
                .await
            {
                Ok(out) if out.success => {
                    if let Some(status) = parse_script_status(&out.stdout) {
                        detail = format!(
                            "schema={:?} mode={:?} pid={:?} ptp_role={:?} ptp_lock={:?}",
                            status.schema,
                            status.mode,
                            status.pid,
                            status.ptp_role,
                            status.ptp_lock
                        );
                        if let Some(schema) = &status.schema {
                            if !schema.starts_with("tspi-audio/") {
                                detail.push_str(&format!("（未知 schema {schema}）"));
                            }
                        }
                        *self.script_status.lock().unwrap() = Some(status);
                    }
                }
                Ok(out) => {
                    detail = format!(
                        "tspi-mode status 退出码 {:?}：{}",
                        out.status,
                        out.stderr.trim().lines().next().unwrap_or("无错误输出")
                    );
                }
                Err(e) => detail = format!("执行 tspi-mode status 失败: {e}"),
            }
            self.set_step(i, StepStatus::Done, Some(detail));
        }

        // 3) 校验目标链路（脚本已切换，Web 侧独立复核）
        if let Some(i) = Self::step_index(plan, StepKey::ScriptVerify) {
            self.set_step(i, StepStatus::Running, None);
            if let Some(profile) = self.profile(target) {
                match self.verify_target(profile).await {
                    Ok(detail) => self.set_step(i, StepStatus::Done, Some(detail)),
                    Err(e) => {
                        self.step_failed(i, &e);
                        return Err(e);
                    }
                }
            } else {
                self.set_step(
                    i,
                    StepStatus::Done,
                    Some("目标为 off，跳过 DSP 校验".into()),
                );
            }
        }
        Ok(())
    }

    /// 失败回滚到切换前模式（尽力而为）。
    async fn rollback(&self, previous: RunMode, failed: RunMode, use_script: bool) -> String {
        let index = self.push_step("rollback", &format!("回滚到 {}", self.label(previous)));
        self.set_step(index, StepStatus::Running, None);
        let mut errors = Vec::new();

        if use_script {
            let timeout = Duration::from_secs(self.cfg.switch_timeout_secs.clamp(10, 120));
            match self
                .runner
                .run_with_timeout(
                    &self.cfg.switch_script,
                    &[previous.as_str(), "--json"],
                    timeout,
                )
                .await
            {
                Ok(out) if out.success => {
                    self.set_step(index, StepStatus::Done, Some("脚本回滚成功".into()));
                    return "已通过切换脚本回滚".to_string();
                }
                Ok(out) => errors.push(format!(
                    "脚本回滚失败（{}）: {}",
                    out.status.map(|c| c.to_string()).unwrap_or_default(),
                    out.stderr.trim().lines().next().unwrap_or("")
                )),
                Err(e) => errors.push(format!("脚本回滚执行失败: {e}")),
            }
        } else {
            // 停掉失败目标可能已启动的组件（忽略错误）
            if failed != RunMode::Off {
                let _ = self.services.stop(self.camilla.service()).await;
                if let Some(p) = self.profile(failed) {
                    let _ = self.services.stop(&p.aoip_service).await;
                    for unit in &p.ptp_services {
                        let _ = self.services.stop(unit).await;
                    }
                }
            }
            // 恢复切换前模式
            match self.profile(previous) {
                None => {
                    let _ = self.services.stop(self.camilla.service()).await;
                }
                Some(p) => {
                    for unit in &p.ptp_services {
                        if let Err(e) = self.services.start(unit).await {
                            errors.push(format!("回滚启动 {unit} 失败: {e}"));
                        }
                    }
                    if let Err(e) = self.services.start(&p.aoip_service).await {
                        errors.push(format!("回滚启动 {} 失败: {e}", p.aoip_service));
                    }
                    if let Err(e) = self.camilla.apply_config_path(&p.camilla_config).await {
                        errors.push(format!("回滚应用 CamillaDSP 配置失败: {e}"));
                    }
                    if let Err(e) = self.services.start(self.camilla.service()).await {
                        errors.push(format!("回滚启动 CamillaDSP 失败: {e}"));
                    }
                }
            }
        }

        if errors.is_empty() {
            self.set_step(
                index,
                StepStatus::Done,
                Some(format!("已回滚到 {}", self.label(previous))),
            );
            format!("已回滚到 {}", self.label(previous))
        } else {
            let msg = errors.join("；");
            self.step_failed(index, &msg);
            format!("回滚失败: {msg}")
        }
    }

    /// 启动时按持久化状态恢复上次模式。
    pub async fn restore_from_store(self: &Arc<Self>) -> AppResult<Option<ModeStatusView>> {
        if !self.cfg.restore_on_start {
            return Ok(None);
        }
        match self.store.load().await {
            Ok(Some(persisted)) if persisted.mode != RunMode::Off => {
                eprintln!(
                    "恢复上次运行模式: {}（persisted at {})",
                    persisted.mode.as_str(),
                    persisted.updated_at_unix
                );
                match self.begin_switch(persisted.mode).await {
                    Ok(view) => Ok(Some(view)),
                    Err(AppError::Conflict(msg)) => {
                        eprintln!("跳过模式恢复: {msg}");
                        Ok(Some(self.status()))
                    }
                    Err(e) => {
                        eprintln!("模式恢复失败: {e}");
                        Ok(None)
                    }
                }
            }
            Ok(_) => Ok(None),
            Err(e) => {
                eprintln!("warn: 读取持久化模式失败: {e}");
                Ok(None)
            }
        }
    }

    /// 主动执行 `tspi-mode status --json` 读取脚本契约状态（mock 返回 None）。
    pub async fn query_script_status(self: &Arc<Self>) -> AppResult<Option<ScriptStatus>> {
        if self.mock {
            return Ok(None);
        }
        let path = self.cfg.switch_script.trim();
        if path.is_empty()
            || !tokio::fs::metadata(path)
                .await
                .map(|m| m.is_file())
                .unwrap_or(false)
        {
            return Ok(None);
        }
        let out = self
            .runner
            .run_with_timeout(path, &["status", "--json"], Duration::from_secs(10))
            .await
            .map_err(|e| AppError::unavailable(format!("执行 tspi-mode status 失败: {e}")))?;
        if !out.success {
            return Err(AppError::unavailable(format!(
                "tspi-mode status 退出码 {:?}: {}",
                out.status,
                out.stderr.trim().lines().next().unwrap_or("无错误输出")
            )));
        }
        let status = parse_script_status(&out.stdout);
        *self.script_status.lock().unwrap() = status.clone();
        Ok(status)
    }

    /// 组装模式卡片需要的组件状态（PTP 来源/角色、AoIP、CamillaDSP 输入输出、格式）。
    pub async fn components(&self, mode: RunMode) -> ModeComponents {
        let profile = self.profile(mode);
        let mut out = ModeComponents::default();

        // PTP：关闭模式下按 linuxptp（B2 路径）展示，便于用户仍能看到 pmc 状态
        if profile.is_none() {
            let status = self.ptp.status("linuxptp", None).await;
            out.ptp = PtpComponent {
                version: "PTPv2".into(),
                source: "linuxptp".into(),
                role: "slave".into(),
                locked: status.locked,
                offset_ns: status.offset_ns,
                port_state: status.port_state,
                gm_present: status.gm_present,
                detail: status.lock_detail,
                service: Some(
                    service_info(self.services.as_ref(), &self.cfg_profile_ptp4l()).await,
                ),
            };
        }
        if let Some(p) = profile {
            out.ptp.version = p.ptp_version.clone();
            out.ptp.source = p.ptp_source.clone();
            out.ptp.role = p.ptp_role.clone();
            out.ptp.service = if let Some(unit) = p.ptp_services.first() {
                Some(service_info(self.services.as_ref(), unit).await)
            } else {
                None
            };
            let status = self.ptp.status(&p.ptp_source, Some(&p.aoip_service)).await;
            out.ptp.locked = status.locked;
            out.ptp.offset_ns = status.offset_ns;
            out.ptp.port_state = status.port_state;
            out.ptp.gm_present = status.gm_present;
            out.ptp.detail = status.lock_detail;

            // AoIP
            out.aoip.kind = Some(match mode {
                RunMode::Aes67 => "aes67".into(),
                RunMode::Dante => "inferno".into(),
                RunMode::Off => String::new(),
            });
            out.aoip.label = Some(p.label.clone());
            let info = service_info(self.services.as_ref(), &p.aoip_service).await;
            out.aoip.active = info.active;
            out.aoip.service = Some(info);

            // 格式
            out.audio.sample_rate = Some(p.input_sample_rate);
            out.audio.bit_depth = Some(p.input_bit_depth);
            out.audio.channels = Some(p.channels);
            out.audio.chunksize = Some(p.default_chunksize);
            out.audio.target_level = Some(p.default_target_level);
            let (input, output) = Self::fmt_profile(p);
            out.audio.input_format = input;
            out.audio.output_format = output;
            if p.input_sample_rate == 48000 {
                out.audio
                    .warnings
                    .push("AES67 默认 48 kHz；量化 24-bit（兼容 16-bit 输入）".into());
            }
        }

        // CamillaDSP
        out.camilla.service =
            Some(service_info(self.services.as_ref(), self.camilla.service()).await);
        let live = self.camilla.live().await;
        out.camilla.reachable = live.reachable;
        out.camilla.state = live.state.clone();
        out.camilla.stop_reason = live.stop_reason.clone();
        out.camilla.config_path = live.config_path.clone();
        if let Ok(yaml) = self.camilla.current_config_yaml().await {
            if let Some(fields) = extract_devices_fields(&yaml) {
                out.camilla.input_device = fields.capture_device;
                out.camilla.output_device = fields.playback_device;
                out.camilla.capture_channels = fields.capture_channels;
                out.camilla.playback_channels = fields.playback_channels;
                if out.audio.sample_rate.is_none() {
                    out.audio.sample_rate = fields.sample_rate;
                }
                if out.audio.chunksize.is_none() {
                    out.audio.chunksize = fields.chunksize;
                }
                if out.audio.target_level.is_none() {
                    out.audio.target_level = fields.target_level;
                }
            }
        }
        let logs = self
            .services
            .logs(self.camilla.service(), 80)
            .await
            .unwrap_or_default();
        let xrun: XrunInfo = scan_xrun(&logs);
        out.camilla.xrun_recent = xrun.recent_count;
        out.camilla.xrun_hint = xrun.hint;
        out
    }
}

/// 脚本失败时提取可读原因：优先 JSON 里的 message/last_error，其次 stderr。
fn script_error_detail(
    status: &Option<ScriptStatus>,
    out: &crate::cmd::CommandOutput,
) -> Option<String> {
    if let Some(status) = status {
        if let Some(message) = status.message.clone().filter(|m| !m.trim().is_empty()) {
            return Some(message);
        }
        if let Some(err) = &status.last_error {
            return Some(match err {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            });
        }
    }
    let stderr = out.stderr.trim();
    if !stderr.is_empty() {
        return Some(stderr.lines().next().unwrap_or("").to_string());
    }
    let stdout = out.stdout.trim();
    if !stdout.is_empty() {
        return Some(stdout.lines().next().unwrap_or("").to_string());
    }
    None
}

fn parse_script_status(stdout: &str) -> Option<ScriptStatus> {
    let text = stdout.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(status) = serde_json::from_str::<ScriptStatus>(text) {
        return Some(status);
    }
    // 容错：脚本可能包在一行日志里，找第一个 { 到最后一个 }
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<ScriptStatus>(&text[start..=end]).ok()
}

// ===== mock / 测试 =====

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::alsa::MockAlsa;
    use crate::camilla::MockCamilla;
    use crate::cmd::MockCommandRunner;
    use crate::config::{AlsaConfig, CamillaConfig};
    use crate::ptp::MockPtp;
    use crate::service::MockServiceManager;
    use std::sync::Arc;
    use std::time::Duration;

    pub struct Harness {
        pub controller: Arc<ModeController>,
        pub services: Arc<MockServiceManager>,
        pub camilla: Arc<MockCamilla>,
        pub store: Arc<MockModeStore>,
    }

    pub fn harness(
        fail_start: Vec<String>,
        step_delay_ms: u64,
        persisted: Option<PersistedMode>,
    ) -> Harness {
        // mock 强制内置编排
        harness_with_config(
            ModeConfig {
                switch_script: String::new(),
                ..ModeConfig::default()
            },
            fail_start,
            step_delay_ms,
            persisted,
        )
    }

    pub fn harness_with_config(
        cfg: ModeConfig,
        fail_start: Vec<String>,
        step_delay_ms: u64,
        persisted: Option<PersistedMode>,
    ) -> Harness {
        let services = Arc::new(MockServiceManager::with_failures(
            fail_start,
            vec![],
            vec![],
        ));
        // 模拟“开机后未启用任何音频链路”的初始状态
        // F-04：旧 C 板端实际 unit 是 tspi-camilladsp.service；legacy web 不再用通用名 camilladsp.service。
        services.set_active("tspi-camilladsp.service", false);
        services.set_active("ptp4l.service", false);
        services.set_active("phc2sys.service", false);
        let camilla = Arc::new(MockCamilla::new(CamillaConfig::default()));
        let ptp = Arc::new(MockPtp::default());
        let alsa = Arc::new(MockAlsa::new(AlsaConfig::default()));
        let store = Arc::new(MockModeStore::new(persisted));
        let controller = Arc::new(ModeController::new(
            cfg,
            services.clone() as Arc<dyn ServiceManager>,
            camilla.clone() as Arc<dyn CamillaBackend>,
            ptp.clone() as Arc<dyn PtpBackend>,
            alsa.clone() as Arc<dyn AlsaBackend>,
            Arc::new(MockCommandRunner) as Arc<dyn CommandRunner>,
            store.clone() as Arc<dyn ModeStore>,
            true,
            Duration::from_millis(step_delay_ms),
        ));
        Harness {
            controller,
            services,
            camilla,
            store,
        }
    }

    pub async fn wait_idle(controller: &Arc<ModeController>, timeout_ms: u64) -> ModeStatusView {
        let started = std::time::Instant::now();
        loop {
            let view = controller.status();
            if view.state != SwitchState::Switching {
                return view;
            }
            if started.elapsed() > Duration::from_millis(timeout_ms) {
                panic!("切换超时，状态: {:?}", view);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::service::MockServiceManager;

    #[test]
    fn script_exit_code_contract() {
        assert_eq!(script_exit_meaning(0), "就绪");
        assert_eq!(script_exit_meaning(2), "参数错误");
        assert_eq!(script_exit_meaning(3), "前置条件不满足");
        assert_eq!(script_exit_meaning(4), "设备未就绪");
        assert_eq!(script_exit_meaning(5), "PTP 未锁定");
        assert_eq!(script_exit_meaning(7), "DSP 启动失败");
        assert_eq!(script_exit_meaning(8), "自检失败");
        assert_eq!(script_exit_meaning(9), "失败并已自动回滚");
        assert_eq!(script_exit_meaning(10), "另一切换正在进行中");
    }

    #[tokio::test]
    async fn builtin_switch_off_to_dante() {
        let h = harness(vec![], 1, None);
        let view = h.controller.begin_switch(RunMode::Dante).await.unwrap();
        assert_eq!(view.state, SwitchState::Switching);
        assert_eq!(view.target, Some(RunMode::Dante));
        let view = wait_idle(&h.controller, 5000).await;
        assert_eq!(view.state, SwitchState::Idle);
        assert_eq!(view.mode, RunMode::Dante);
        assert!(h.services.is_active("inferno.service"));
        assert!(h.services.is_active("statime.service"));
        assert!(h.services.is_active("tspi-camilladsp.service"));
        assert_eq!(h.camilla.applied_count(), 1);
        assert_eq!(h.store.get().unwrap().mode, RunMode::Dante);
        // 校验步骤存在且完成
        assert!(view
            .steps
            .iter()
            .any(|s| s.key == "verify" && s.status == StepStatus::Done));
    }

    #[tokio::test]
    async fn switch_dante_to_aes67_b1_no_linuxptp() {
        let h = harness(vec![], 1, None);
        h.controller.begin_switch(RunMode::Dante).await.unwrap();
        wait_idle(&h.controller, 5000).await;
        h.controller.begin_switch(RunMode::Aes67).await.unwrap();
        let view = wait_idle(&h.controller, 5000).await;
        assert_eq!(view.mode, RunMode::Aes67);
        assert!(h.services.is_active("aes67-daemon.service"));
        assert!(!h.services.is_active("inferno.service"));
        assert!(!h.services.is_active("statime.service"));
        // 方案 B1：不启动 ptp4l/phc2sys
        assert!(!h.services.is_active("ptp4l.service"));
        assert!(!h.services.is_active("phc2sys.service"));
        let ptp_step = view
            .steps
            .iter()
            .find(|s| s.key == "ptp_internal")
            .expect("应存在 B1 内置 PTP 步骤");
        assert_eq!(ptp_step.status, StepStatus::Skipped);
        assert!(h.camilla.applied_count() >= 2);
        assert_eq!(h.store.get().unwrap().mode, RunMode::Aes67);
    }

    #[tokio::test]
    async fn switch_to_off_stops_everything() {
        let h = harness(vec![], 1, None);
        h.controller.begin_switch(RunMode::Aes67).await.unwrap();
        wait_idle(&h.controller, 5000).await;
        h.controller.begin_switch(RunMode::Off).await.unwrap();
        let view = wait_idle(&h.controller, 5000).await;
        assert_eq!(view.mode, RunMode::Off);
        assert!(!h.services.is_active("aes67-daemon.service"));
        assert!(!h.services.is_active("tspi-camilladsp.service"));
        assert!(!h.services.is_active("ptp4l.service"));
        assert_eq!(h.store.get().unwrap().mode, RunMode::Off);
    }

    #[tokio::test]
    async fn concurrent_switch_conflicts() {
        let h = harness(vec![], 40, None);
        h.controller.begin_switch(RunMode::Dante).await.unwrap();
        let err = h.controller.begin_switch(RunMode::Aes67).await.unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)));
        let view = wait_idle(&h.controller, 5000).await;
        assert_eq!(view.state, SwitchState::Idle);
        assert_eq!(view.mode, RunMode::Dante);
    }

    #[tokio::test]
    async fn same_mode_rejected() {
        let h = harness(vec![], 1, None);
        h.controller.begin_switch(RunMode::Aes67).await.unwrap();
        wait_idle(&h.controller, 5000).await;
        let err = h.controller.begin_switch(RunMode::Aes67).await.unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)));
    }

    #[tokio::test]
    async fn failure_rolls_back_to_previous_mode() {
        // dell 先切到 AES67（B1），再切 Dante 时 Inferno 启动失败
        let h = harness(vec![], 1, None);
        h.controller.begin_switch(RunMode::Aes67).await.unwrap();
        wait_idle(&h.controller, 5000).await;
        // 注入 inferno 启动失败
        let h2 = harness(vec!["inferno.service".into()], 1, None);
        // 用同一个 services 不行，这里重建并先切 aes67
        h2.controller.begin_switch(RunMode::Aes67).await.unwrap();
        wait_idle(&h2.controller, 5000).await;
        let before_applied = h2.camilla.applied_count();
        let view = h2.controller.begin_switch(RunMode::Dante).await.unwrap();
        assert_eq!(view.target, Some(RunMode::Dante));
        let view = wait_idle(&h2.controller, 5000).await;
        assert_eq!(view.state, SwitchState::Failed);
        assert_eq!(view.mode, RunMode::Aes67, "失败后应回到切换前模式");
        assert!(view.last_error.as_deref().unwrap_or("").contains("inferno"));
        assert!(
            h2.services.is_active("aes67-daemon.service"),
            "回滚应重新拉起 AES67"
        );
        assert!(!h2.services.is_active("inferno.service"));
        assert!(
            h2.camilla.applied_count() > before_applied,
            "回滚应重新应用 AES67 配置"
        );
        assert!(view.steps.iter().any(|s| s.key == "rollback"));
        assert_eq!(h2.store.get().unwrap().mode, RunMode::Aes67);
    }

    #[tokio::test]
    async fn restore_from_persisted_mode() {
        // D4 默认关闭；这里显式启用，验证可选的“恢复上次模式”路径。
        let h = harness_with_config(
            ModeConfig {
                restore_on_start: true,
                ..ModeConfig::default()
            },
            vec![],
            1,
            Some(PersistedMode {
                mode: RunMode::Dante,
                updated_at_unix: 1,
                last_error: None,
            }),
        );
        let restored = h.controller.restore_from_store().await.unwrap();
        assert!(restored.is_some());
        let view = wait_idle(&h.controller, 5000).await;
        assert_eq!(view.mode, RunMode::Dante);
        assert!(h.services.is_active("inferno.service"));
    }

    #[tokio::test]
    async fn restore_off_is_noop() {
        let h = harness(
            vec![],
            1,
            Some(PersistedMode {
                mode: RunMode::Off,
                updated_at_unix: 1,
                last_error: None,
            }),
        );
        assert!(h.controller.restore_from_store().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn disabled_mode_switch_rejected() {
        let cfg = ModeConfig {
            enabled: false,
            ..ModeConfig::default()
        };
        let services = Arc::new(MockServiceManager::new());
        let camilla = Arc::new(crate::camilla::MockCamilla::new(
            crate::config::CamillaConfig::default(),
        ));
        let ptp = Arc::new(crate::ptp::MockPtp::default());
        let alsa = Arc::new(crate::alsa::MockAlsa::default());
        let store = Arc::new(MockModeStore::new(None));
        let controller = Arc::new(ModeController::new(
            cfg,
            services as Arc<dyn ServiceManager>,
            camilla as Arc<dyn CamillaBackend>,
            ptp as Arc<dyn PtpBackend>,
            alsa as Arc<dyn AlsaBackend>,
            Arc::new(crate::cmd::MockCommandRunner) as Arc<dyn CommandRunner>,
            store as Arc<dyn ModeStore>,
            true,
            Duration::ZERO,
        ));
        let err = controller.begin_switch(RunMode::Dante).await.unwrap_err();
        assert!(matches!(err, AppError::Forbidden(_)));
    }

    #[tokio::test]
    async fn components_show_mode_formats_and_ptp_source() {
        let h = harness(vec![], 1, None);
        h.controller.begin_switch(RunMode::Dante).await.unwrap();
        wait_idle(&h.controller, 5000).await;
        let c = h.controller.components(RunMode::Dante).await;
        assert_eq!(c.ptp.version, "PTPv1");
        assert_eq!(c.ptp.source, "statime");
        assert!(c.ptp.role == "slave");
        assert_eq!(c.audio.sample_rate, Some(96000));
        assert_eq!(c.audio.bit_depth, Some(24));
        assert!(c.audio.output_format.contains("MOTU"));
        assert_eq!(c.camilla.input_device.as_deref(), Some("hw:Loopback,0,0"));
        assert!(c.camilla.xrun_hint.contains("xrun"));
        let c2 = h.controller.components(RunMode::Aes67).await;
        assert_eq!(c2.ptp.version, "PTPv2");
        assert_eq!(c2.ptp.source, "aes67-daemon");
        assert_eq!(c2.audio.sample_rate, Some(48000));
    }

    #[tokio::test]
    async fn mock_ignores_script_and_uses_builtin() {
        let h = harness(vec![], 1, None);
        // 手动把脚本配成不存在的路径，mock 下应仍然走内置
        let _ = h;
        let cfg = ModeConfig {
            switch_script: "/nonexistent/tspi-mode".into(),
            ..ModeConfig::default()
        };
        let services = Arc::new(MockServiceManager::new());
        let camilla = Arc::new(crate::camilla::MockCamilla::new(
            crate::config::CamillaConfig::default(),
        ));
        let ptp = Arc::new(crate::ptp::MockPtp::default());
        let alsa = Arc::new(crate::alsa::MockAlsa::default());
        let store = Arc::new(MockModeStore::new(None));
        let controller = Arc::new(ModeController::new(
            cfg,
            services as Arc<dyn ServiceManager>,
            camilla as Arc<dyn CamillaBackend>,
            ptp as Arc<dyn PtpBackend>,
            alsa as Arc<dyn AlsaBackend>,
            Arc::new(crate::cmd::MockCommandRunner) as Arc<dyn CommandRunner>,
            store as Arc<dyn ModeStore>,
            true,
            Duration::ZERO,
        ));
        let view = controller.begin_switch(RunMode::Aes67).await.unwrap();
        assert_eq!(view.strategy, "builtin");
        let view = wait_idle(&controller, 5000).await;
        assert_eq!(view.mode, RunMode::Aes67);
        assert!(view.warnings.iter().any(|w| w.contains("mock")));
    }
}
