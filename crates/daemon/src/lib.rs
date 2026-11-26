//! daemon 骨架（T30/T31）：把主机控制面暴露成 REST。
//!
//! 两个控制后端（`KHAEN_RT_TARGET`，见 `host_link` 模块）：
//! - `rt-host`（默认）：控制 op 经 UNIX socket 转发给真正出声的 rt-host（契约 B 行 JSON），
//!   响应原样返回；socket 不可用时如实回 503 + `host_link_unavailable`，**绝不**偷偷作用于
//!   daemon 进程内引擎冒充成功。
//! - `process`（仅测试/离线演示）：op 作用于进程内 `ControlState`（T30 的旧行为），响应与
//!   `/api/status` 显式标 `target: "daemon-process"`。
//!
//! 没有 MemoryEngine，也没有"假装成功"的接口：还没实现的 op 返回 501 并带原因，`/api/status`
//! 的 wired/not_wired 如实反映当前能力。
//!
//! # 错误码 → HTTP 状态码
//!
//! - `bad_request` → 400：请求字段缺失/类型不对、方案文件读不到、sha256 不符、方案解码失败。
//! - `unknown_op` → 404：REST 上没有这个 op 资源，与未知路径一致（另一种合理选择是 400，
//!   这里固定选 404，并由测试锁住）。
//! - `unsupported` → 501：op 存在但当前未实现。
//! - `admission_rejected` → 409：方案超预算/结构非法，或与当前运行时配置不匹配。
//! - `engine_token_invalid` → 409：没有对应草稿或 token 不符（先 prepare / 用正确 token）。
//! - `no_base_plan` → 409：surface 补丁前还没有已生效方案。
//! - `no_history` → 409：rollback 前还没有任何已生效版本。
//! - `host_link_unavailable` → 503：rt-host 控制 socket 不可用（连接失败/超时；已按语义重试
//!   一次，见 `host_link` 模块的重试策略）。
//! - 其余 → 500。
//!
//! # 路由（M0 静态托管）
//!
//! - `/api/*`：控制面 REST（语义见各 handler）；未知 `/api` 路径返回结构化 JSON 404
//!   （`{ok:false,error:"not_found",message}`），不回退成 SPA 页面。
//! - `/healthz`：无认证探活 `{ok,version,uptime_s}`。
//! - `/api/mute` / `/api/prepare` / `/api/commit` / `/api/abort` / `/api/coeff` /
//!   `/api/plan/inspect` / `/api/surface` / `/api/rollback` / `/api/format`：控制面 op。
//!   rt-host 目标下转成契约 B 请求发给 rt-host 并原样返回响应；process 目标下作用于进程内引擎，
//!   响应带 `target: "daemon-process"`。
//! - `/api/status`：M2 聚合状态（PLAN §4.4 的 `khaen-status/v1`，见 `status` 模块）：
//!   **主路径是 socket status**（rt-host 自己报的 age/state/plan/format/device/counters，
//!   `host_link.mode=socket`）；socket 不可用时退回 rt-host 状态文件
//!   （`KHAEN_RT_STATUS_FILE`，启动时注入；`host_link.mode=file`），文件也没有则
//!   `mode=none`；daemon 自身字段（版本/uptime/bind）照常返回，**绝不 500**。
//! - `/api/meta`：`{api_version,web_version,ops,surfaces,not_wired}`，值与能力来自
//!   `khaen_host::control` 的既有常量（见 `meta` 模块）。
//! - `/api/calibration/*`：T36 校准 REST（uploads/jobs/artifacts/wizard，见 `calibration` 模块）。
//!   T70h-b 起 `main` 注入生产后端 `calibration::backend::JobManagerCalibration`
//!   （khaen-calibration 的 JobManager + 离线 OfflineJobBackend；全程离线、不打开设备、不出声）：
//!   job 状态机 queued/running/succeeded/failed/cancelled + created/updated unix + progress +
//!   白名单 artifacts（manifest sha 下载时重算校验）+ apply/rollback 两阶段（按
//!   `host_link.target` 路由 process/rt-host）。初始化失败时如实降级 not_wired + 真实原因
//!   （`calibration::not_wired`）；上传与向导配置不依赖 job 后端，照常可用。
//! - `/api/system/*`：T46 系统页后端（info/logs/diagnostics/password/restart，见 `system` 模块）。
//!   缺失字段 null + reasons；journalctl 不可用 503 `logs_unavailable`；重启需
//!   `KHAEN_ALLOW_RESTART=1`（否则 403 `restart_disabled`）；daemon 自身重启 501。
//! - `/api/channels` 与 `/api/plan/upload|history`：T55 M4 后端（见 `channels` / `plan_api` /
//!   `plan_store` 模块）。T61 起 rt-host 目标真正接通 M3b 热重配：GET 优先用
//!   `channels_status` 的 current/device/probe/generation（拿不到 null + reasons，方案 routing
//!   作为 mapping 的补充来源）；PUT 两阶段 prepare -> `channels_prepare`、commit ->
//!   `channels_commit`（成功回执原样透传 applied/changed/generation/gap_ms/format/graph_dropped，
//!   失败回执的 rolled_back 等回滚真值原样带出）、abort -> `channels_abort`；
//!   本地镜像记在 `channels_mirror`（最近一次成功 commit + 方案路由失效标记）。
//!   process 目标（`KHAEN_RT_TARGET=process`）没有数据面热重配，PUT 任何阶段都 501
//!   `host_link_unsupported` + reason（不假成功）。
//! - `/api/motu`：T57 MOTU 控制面（见 `motu` 模块）。GET 恒 200：检测对象
//!   （detected/model/control_reachable/reason）+ mute/vol 现状，设备读不到就 null + reasons，
//!   Windows/无设备 detected=false 不报错。POST/PUT `/api/motu/volume` 两阶段：prepare 只校验
//!   并签发 token（不写设备），commit 才逐通道单键写 + 回读；批量 400、设备写失败 502 + 设备码。
//!   POST `/api/mute` 在 target=process 且检测到 MOTU 时把同一次静音写成设备副作用
//!   （kMuteEnable[0] 单键写 + 回读，失败 502 且响应保留原控制面回执 `control`）。
//! - `/api/auth/*`：M1 认证（setup/login/session/logout/password），argon2id PHC 存
//!   `${KHAEN_STATE_DIR:-/var/lib/khaen}/auth.json`（见 `auth` 模块）。除 `/healthz` 与
//!   `/api/auth/status|setup|login` 外，全部 `/api` 要求会话；写方法（Cookie 会话）额外要求
//!   `X-KHAEN-CSRF`；`/api` 响应统一带 `X-KHAEN-API-Version: 1` / `X-KHAEN-Web-Version` /
//!   `Cache-Control: no-store`。
//! - 其余 `GET`：内嵌 `webui/dist` 静态资源；未知路径回退 `index.html`（SPA fallback），
//!   路径查找只命中编译期内嵌表、从不读文件系统，目录穿越被显式拒绝（见 `webui` 模块）。

// 大号 status fixture（serde_json::json!）在测试里需要更深的宏递归。
#![recursion_limit = "256"]

pub mod auth;
pub mod bind;
/// 校准 REST 层（T36）：jobs / uploads / artifacts / wizard。
pub mod calibration;
/// M4 方案与声道后端（T55）：/api/channels 两阶段改声道映射 + /api/plan/upload|history。
pub mod channels;
/// /api/channels 的 daemon 本地镜像（T61）：草稿路由意图 + 最近一次成功 commit + 方案路由失效标记。
pub mod channels_mirror;
pub mod device;
/// daemon → rt-host 控制通道代理（T70f-b）：契约 B 行 JSON 客户端 + 诚实降级。
pub mod host_link;
mod meta;
/// MOTU UltraLite-mk5 控制面（T57）：/api/motu 检测 + 两阶段音量 + /api/mute 设备副作用。
pub mod motu;
/// M4 方案 REST（T55）：POST /api/plan/upload、GET /api/plan/history。
pub mod plan_api;
/// 方案库（T55）：上传内容寻址存储 + 已 prepare/commit 方案元数据与生效历史。
pub mod plan_store;
pub mod status;
/// 系统信息 / 日志 / 诊断 / 改密 / 重启 API（T46，M6 系统页后端提前）。
pub mod system;
mod webui;
pub mod webui_assets;

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::Value;

use khaen_host::control::{error, ControlError};
use khaen_host::{ControlState, EngineConfig};

use crate::host_link::{HostLink, LinkError, Target, WireReply};

pub use device::DeviceMonitor;

/// daemon 的 crate 版本（workspace `version`，即 `CARGO_PKG_VERSION`）。
///
/// 注意：这是编译进二进制的 crate 版本，**不是**部署 release 版本。`/healthz` 与
/// `/api/system/info` 的 `release_version` 来自构建期 `KHAEN_BUILD_VERSION`（T46，
/// 见 `system::build_info`）；两者不一致时系统页会醒目提示。`/api/status` 的
/// `daemon.version` 保持 crate 版本口径不变。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 控制台版本：构建期从 `webui/package.json` 的 `version` 读取
/// （`build.rs` 生成 `webui_assets::WEBUI_PACKAGE_VERSION`；`webui/package.json` 读不到时
/// 回退 workspace 版本）。`/api/meta` 的 `web_version` 直接用它，不另编一个数字。
pub const WEB_VERSION: &str = webui_assets::WEBUI_PACKAGE_VERSION;

/// REST API 版本：与契约 B 的主机控制协议版本同源（`khaen_host::PROTOCOL_VERSION` 是版本事实源）。
pub const API_VERSION: u32 = khaen_host::PROTOCOL_VERSION as u32;

/// daemon 启动错误：引擎配置错误或认证存储不可用（两者都 fail-closed，不静默降级）。
#[derive(Debug)]
pub enum DaemonError {
    /// 引擎/控制面构造失败。
    Engine(khaen_host::EngineError),
    /// 认证存储（auth.json）不可用或内容损坏。
    Auth(auth::AuthError),
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Engine(error) => write!(f, "engine: {error}"),
            Self::Auth(error) => write!(f, "auth: {error}"),
        }
    }
}

impl std::error::Error for DaemonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Engine(error) => Some(error),
            Self::Auth(error) => Some(error),
        }
    }
}

impl From<khaen_host::EngineError> for DaemonError {
    fn from(error: khaen_host::EngineError) -> Self {
        Self::Engine(error)
    }
}

impl From<auth::AuthError> for DaemonError {
    fn from(error: auth::AuthError) -> Self {
        Self::Auth(error)
    }
}

/// daemon 共享状态。
#[derive(Debug)]
pub struct DaemonState {
    control: Mutex<ControlState>,
    device: Mutex<DeviceMonitor>,
    /// 认证/会话存储（argon2id PHC + 会话 + 登录限速）。
    auth: Mutex<auth::AuthStore>,
    /// 进程启动时刻（`/healthz` 的 `uptime_s`）。
    started: std::time::Instant,
    /// 启动时解析的监听地址（`/api/status` 的 `daemon.bind`；handler 不读环境变量）。
    bind: std::net::SocketAddr,
    /// rt-host 状态文件路径（`/api/status` 退回读取；生产由 `KHAEN_RT_STATUS_FILE` 注入）。
    rt_status_file: std::path::PathBuf,
    /// 控制通道客户端（rt-host UNIX socket 代理，见 [`host_link`]）。
    ///
    /// 构造默认 rt-host 目标 + 默认 socket 路径；生产由 `main` 按 `KHAEN_RT_*` 环境变量注入，
    /// 测试注入假传输或 process 目标。
    host_link: Arc<HostLink>,
    /// 状态目录（auth.json / uploads / wizard.json 同源；测试注入 tempdir）。
    state_dir: std::path::PathBuf,
    /// 校准 REST 层（服务 + 事件缓存 + 上传/向导存储）。
    calibration: calibration::CalibrationHub,
    /// 系统采集实现（T46）：/proc、systemctl、journalctl；生产为 NativeProbe，测试注入假实现。
    system: Arc<dyn system::SystemProbe>,
    /// 构建信息（T46）：crate / release / commit 三口径（默认来自 build.rs 构建期常量）。
    build: system::BuildInfo,
    /// rt-host 重启开关（T46：KHAEN_ALLOW_RESTART，启动时解析；handler 不读环境）。
    restart: system::RestartPolicy,
    /// 最近一次成功重启 rt-host 的时刻（RFC3339；进程内，不持久化）。
    last_restart: Mutex<Option<String>>,
    /// 方案库（T55）：上传内容寻址存储 + 已 prepare/commit 方案元数据 + 生效历史。
    plans: plan_store::PlanStore,
    /// /api/channels 的本地镜像（T61）：prepare 记下的 lane -> DAC 路由意图、
    /// 最近一次成功 channels_commit 的回执快照、方案图被重配丢弃后的 routing 失效标记。
    channels_mirror: channels_mirror::ChannelsMirror,
    /// MOTU 控制面（T57）：检测 + 两阶段音量草稿 + 设备实现（生产 NativeMotu，测试注入假控制面）。
    motu: motu::MotuHub,
}

impl DaemonState {
    /// 用主机默认配置新建；设备监督器默认未接线（原因随 status 一起上报）；
    /// 认证状态目录取 `KHAEN_STATE_DIR`（缺省 `/var/lib/khaen`）。
    pub fn new(config: EngineConfig) -> Result<Self, DaemonError> {
        Self::new_with_auth(config, auth::AuthConfig::from_env())
    }

    /// 用显式认证配置新建（测试注入临时目录/低成本 KDF 参数；生产由 `new` 走环境变量）。
    pub fn new_with_auth(
        config: EngineConfig,
        auth_config: auth::AuthConfig,
    ) -> Result<Self, DaemonError> {
        // 状态目录先取出来：auth.json / uploads / wizard.json 都在同一个目录下
        // （auth 的语义不改，只是复用它的目录配置）。
        let state_dir = auth_config.state_dir().to_path_buf();
        Ok(Self {
            control: Mutex::new(ControlState::new(config)?),
            device: Mutex::new(DeviceMonitor::disabled(
                "device supervisor is not wired: no probe/rate-control configured in this process",
            )),
            auth: Mutex::new(auth::AuthStore::load(auth_config)?),
            started: std::time::Instant::now(),
            bind: bind::default_addr(),
            rt_status_file: std::path::PathBuf::from(status::DEFAULT_RT_STATUS_FILE),
            host_link: HostLink::new(host_link::HostLinkConfig::default()),
            calibration: calibration::CalibrationHub::new(state_dir.clone()),
            system: Arc::new(system::NativeProbe::default()),
            build: system::BuildInfo::from_build(),
            restart: system::RestartPolicy::from_env(),
            last_restart: Mutex::new(None),
            plans: plan_store::PlanStore::new(&state_dir),
            channels_mirror: channels_mirror::ChannelsMirror::new(),
            // 默认实现：crate 默认的 /proc/asound/cards + 默认控制网地址、无密码；
            // Windows/无设备时检测如实 detected=false，不建立任何连接。
            motu: motu::MotuHub::new(std::sync::Arc::new(motu::NativeMotu::default())),
            state_dir,
        })
    }

    /// 接线设备监督器（由 `main` 按配置构造；测试也用真实监督器 + 假组件）。
    pub fn with_device(mut self, monitor: DeviceMonitor) -> Self {
        self.device = Mutex::new(monitor);
        self
    }

    /// 注入启动时解析的监听地址（由 `main` 从 `KHAEN_BIND`/`KHAEN_PORT` 解析后传入）。
    ///
    /// `/api/status` 的 `daemon.bind` 用它，handler 里不读环境变量（测试可注入任意值）。
    pub fn with_bind(mut self, bind: std::net::SocketAddr) -> Self {
        self.bind = bind;
        self
    }

    /// 注入 rt-host 状态文件路径（`/api/status` 退回读取；测试注入 tempdir）。
    pub fn with_rt_status_file(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.rt_status_file = path.into();
        self
    }

    /// 注入控制通道客户端（生产由 `main` 按 `KHAEN_RT_SOCKET/KHAEN_RT_TARGET` 构造；
    /// 测试注入假传输或 `HostLink::process()`）。
    pub fn with_host_link(mut self, link: Arc<HostLink>) -> Self {
        self.host_link = link;
        self
    }

    /// 控制通道客户端（handler 用；`Arc` 便于把阻塞的 socket 往返丢给 blocking 线程池）。
    pub fn host_link(&self) -> &Arc<HostLink> {
        &self.host_link
    }

    /// 注入 MOTU 控制面实现（生产由 `main` 按环境变量构造 [motu::NativeMotu]；
    /// 测试注入 FakeCueMix，禁真实网络/设备）。
    pub fn with_motu(mut self, control: Arc<dyn motu::MotuControl>) -> Self {
        self.motu = motu::MotuHub::new(control);
        self
    }

    /// MOTU 控制面（/api/motu* 与 /api/mute 副作用使用）。
    pub fn motu(&self) -> &motu::MotuHub {
        &self.motu
    }

    /// 注入校准 job 服务（替换默认未接线实现；daemon 会把事件汇注入给它）。
    ///
    /// 生产由 lead 在 backend 落地后接线；测试注入 fake，REST 层用真实 TCP 端到端验证。
    pub fn with_calibration(
        mut self,
        service: std::sync::Arc<dyn calibration::service::CalibrationService>,
    ) -> Self {
        self.calibration = self.calibration.with_service(service);
        self
    }

    /// 校准 REST 共享状态（handler / status 聚合使用）。
    pub fn calibration(&self) -> &calibration::CalibrationHub {
        &self.calibration
    }

    /// 注入系统采集实现（T46；生产默认 NativeProbe，测试注入假实现）。
    pub fn with_system_probe(mut self, probe: Arc<dyn system::SystemProbe>) -> Self {
        self.system = probe;
        self
    }

    /// 注入构建信息（T46；测试注入 release 版本，验证 crate / release / commit 三口径）。
    pub fn with_build_info(mut self, build: system::BuildInfo) -> Self {
        self.build = build;
        self
    }

    /// 注入重启能力（T46；生产由 main 从 KHAEN_ALLOW_RESTART 解析，fail-closed）。
    pub fn with_restart_policy(mut self, policy: system::RestartPolicy) -> Self {
        self.restart = policy;
        self
    }

    /// 系统采集实现（/api/system/* handler 使用）。
    pub fn system_probe(&self) -> &Arc<dyn system::SystemProbe> {
        &self.system
    }

    /// 构建信息（/api/system/info 与 /healthz 的版本来源）。
    pub fn build_info(&self) -> &system::BuildInfo {
        &self.build
    }

    /// 重启能力（/api/system/info 的 restart，/api/system/restart 的守卫）。
    pub fn restart_policy(&self) -> &system::RestartPolicy {
        &self.restart
    }

    /// 最近一次成功重启 rt-host 的时刻（RFC3339；从未重启为 None）。
    pub fn last_restarted_at(&self) -> Option<String> {
        self.last_restart
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 记录一次成功的 rt-host 重启（进程内状态；不持久化）。
    pub(crate) fn record_restart(&self, at: String) {
        *self
            .last_restart
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(at);
    }

    /// 方案库（T55：/api/channels 的 routing 段与 /api/plan/history 的来源）。
    pub fn plans(&self) -> &plan_store::PlanStore {
        &self.plans
    }

    /// /api/channels 的本地镜像（T61；见 `channels_mirror` 模块）。
    pub fn channels_mirror(&self) -> &channels_mirror::ChannelsMirror {
        &self.channels_mirror
    }

    /// `/api/status` 的 calibration 三字段来源（服务状态 + 进程内事件缓存合并）。
    pub fn calibration_status(&self) -> calibration::service::CalibrationStatus {
        self.calibration.status()
    }

    /// 状态目录（auth.json / uploads / wizard.json 同源）。
    pub fn state_dir(&self) -> &std::path::Path {
        &self.state_dir
    }

    /// 控制面（测试与内部使用）。
    pub fn control(&self) -> std::sync::MutexGuard<'_, ControlState> {
        self.control
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 设备监督器（测试与内部使用）。
    pub fn device(&self) -> std::sync::MutexGuard<'_, DeviceMonitor> {
        self.device
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 认证/会话存储（中间件与 auth handler 使用）。
    pub(crate) fn auth(&self) -> std::sync::MutexGuard<'_, auth::AuthStore> {
        self.auth
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 轮询设备监督器，并在锁存时把引擎静音（fail-closed）。返回最新设备快照。
    ///
    /// 恢复必须是用户显式 `/api/mute false`，这里绝不自动解除静音。
    pub fn poll_device(&self) -> Value {
        let snapshot = self.device().poll();
        // 锁存期间每次轮询都重新下发静音：请求是块边界幂等的，代价只是一次控制面往返；
        // 用户若在锁存期间显式解除静音，下一次轮询必须把它重新压回去（fail-closed）。
        //
        // 目标为 rt-host 时静音必须发给真正出声的数据面（daemon 进程内引擎不发声）；
        // 链路不可用时只能如实放弃（rt-host 自己的设备监督器在设备缺失时也是 fail-closed），
        // 这里不因为发不出去就假装成功。
        if self.device().requires_mute() {
            match self.host_link().target() {
                Target::Process => {
                    let _ = self.control().mute_json(true);
                }
                Target::RtHost => {
                    let _ = self.host_link().call(serde_json::json!({
                        "op": "mute",
                        "mute": true,
                    }));
                }
            }
        }
        snapshot
    }

    /// 最近一次设备快照（不轮询）。
    pub fn device_json(&self) -> Value {
        self.device().status()
    }

    /// 进程启动以来的整秒数（`/healthz` 用；测试里通常为 0）。
    pub fn uptime_s(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    /// 启动时解析的监听地址（`/api/status` 的 `daemon.bind`）。
    pub fn bind(&self) -> std::net::SocketAddr {
        self.bind
    }

    /// rt-host 状态文件路径（`/api/status` 聚合读取）。
    pub fn rt_status_file(&self) -> &std::path::Path {
        &self.rt_status_file
    }

    /// 控制面当前的引擎配置（`/api/status` 的 `format.block_frames`/`channels` 来源）。
    pub fn engine_config(&self) -> EngineConfig {
        self.control().engine().config().clone()
    }
}

/// 组装路由。
pub fn router(state: Arc<DaemonState>) -> Router {
    Router::new()
        .route("/healthz", get(meta::healthz))
        // M1 认证（PLAN §4.2）：status/setup/login 匿名可访问，其余要求会话。
        .route("/api/auth/status", get(auth::status))
        .route("/api/auth/setup", post(auth::setup))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/session", get(auth::session))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/password", post(auth::password))
        .route("/api/meta", get(meta::api_meta))
        .route("/api/status", get(status))
        .route("/api/mute", post(mute))
        .route("/api/prepare", post(prepare))
        .route("/api/commit", post(commit))
        .route("/api/abort", post(abort))
        .route("/api/coeff", get(coeff))
        .route("/api/plan/inspect", post(inspect))
        .route("/api/surface", post(surface))
        .route("/api/rollback", post(rollback))
        .route("/api/format", post(format_prepare))
        .route("/api/device", get(device_status))
        .route("/api/device/retry", post(device_retry))
        // T36 校准 REST（uploads/jobs/artifacts/wizard）：登录会话与写操作 CSRF 由下面的
        // 全局 auth 中间件统一守，这里只注册路由。
        .merge(calibration::api::routes())
        // T46 系统页后端（info/logs/diagnostics/password/restart）。
        .merge(system::routes())
        // T55 M4 后端（channels 两阶段、plan upload/history）。
        .merge(channels::routes())
        .merge(plan_api::routes())
        // T57 MOTU 控制面（/api/motu 检测 + /api/motu/volume 两阶段）。
        .merge(motu::routes())
        // 非 /api 的 GET/HEAD 走内嵌静态资源 + SPA 回退；未知 /api/* 返回结构化 JSON 404。
        .fallback(webui::fallback)
        // 认证守卫包住全部路由（含 fallback）：只对 /api 生效，并给 /api 响应补版本/缓存头。
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            auth::guard,
        ))
        .with_state(state)
}

/// 启动 HTTP 服务（阻塞直到 listener 关闭）。
///
/// 用 `into_make_service_with_connect_info` 让中间件拿到对端 IP：登录限速按来源（IP）隔离。
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: Arc<DaemonState>,
) -> std::io::Result<()> {
    let app = router(state).into_make_service_with_connect_info::<std::net::SocketAddr>();
    axum::serve(listener, app).await
}

/// process 目标响应标记：显式声明"作用于 daemon 进程内引擎"，不冒充 rt-host 数据面。
fn process_marker(mut body: Value) -> Value {
    if let Some(object) = body.as_object_mut() {
        object.insert(
            "target".to_string(),
            Value::String(Target::Process.as_str().to_string()),
        );
    }
    body
}

/// 控制通道链路错误 → 503 + 稳定码（诚实降级：不作用于进程内引擎、不返回假成功）。
fn link_error_response(error: LinkError) -> (StatusCode, Json<Value>) {
    (StatusCode::SERVICE_UNAVAILABLE, Json(error.to_json()))
}

/// 转发一条契约 B 请求给 rt-host：成功/服务端业务错误都原样返回，链路失败回 503。
///
/// socket 往返是阻塞 IO（含至多一次重试），放到 tokio 的 blocking 线程池，不卡 axum reactor。
async fn forward(state: &Arc<DaemonState>, request: Value) -> (StatusCode, Json<Value>) {
    let link = Arc::clone(state.host_link());
    match tokio::task::spawn_blocking(move || link.call(request)).await {
        Ok(Ok(WireReply::Success(body))) => (StatusCode::OK, Json(body)),
        Ok(Ok(WireReply::Failure { code, body, .. })) => (http_status(&code), Json(body)),
        Ok(Err(error)) => link_error_response(error),
        Err(join_error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "ok": false,
                "error": "internal_error",
                "message": format!("control forwarding task failed: {join_error}"),
            })),
        ),
    }
}

/// `/api/device` 快照：rt-host 目标且 socket status 带 `device` 字段时优先用它（数据面权威），
/// 否则退回 daemon 设备监督器并标明来源（不把两种来源混成一个数）。
fn device_snapshot(monitor: Value, link: &host_link::LinkSnapshot) -> Value {
    match link {
        host_link::LinkSnapshot::Socket { status, .. } => {
            if let Some(device) = status.get("device").filter(|value| value.is_object()) {
                return serde_json::json!({
                    "wired": true,
                    "source": "rt-host",
                    "state": device.get("state").cloned().unwrap_or(Value::Null),
                    "plan_rate": device.get("plan_rate").cloned().unwrap_or(Value::Null),
                    "device_rate": device.get("device_rate").cloned().unwrap_or(Value::Null),
                    "rate_asserted": device.get("rate_asserted").cloned().unwrap_or(Value::Null),
                    "attempts": device.get("attempts").cloned().unwrap_or(Value::Null),
                    "since_ms": device.get("since_ms").cloned().unwrap_or(Value::Null),
                    "last_error": device.get("last_error").cloned().unwrap_or(Value::Null),
                    "device_state": status.get("device_state").cloned().unwrap_or(Value::Null),
                    "absent_since": status.get("absent_since").cloned().unwrap_or(Value::Null),
                    "reopen_attempts": status.get("reopen_attempts").cloned().unwrap_or(Value::Null),
                    "last_device_error": status.get("last_device_error").cloned().unwrap_or(Value::Null),
                    "daemon_monitor": monitor,
                });
            }
            monitor_with_source(monitor, "daemon-monitor", None)
        }
        host_link::LinkSnapshot::Unavailable { reason, .. } => {
            monitor_with_source(monitor, "daemon-monitor", Some(reason))
        }
        host_link::LinkSnapshot::Process => monitor_with_source(monitor, "daemon-monitor", None),
    }
}

/// 退回 daemon 监督器时的响应：加来源标记；rt-host 不可用时带上真实原因（不编码成正常）。
fn monitor_with_source(monitor: Value, source: &str, unavailable: Option<&String>) -> Value {
    let mut snapshot = monitor;
    if let Some(object) = snapshot.as_object_mut() {
        object.insert("source".to_string(), Value::String(source.to_string()));
        // 合同 §3：设备状态拿不到时 state 必须是 null（缺失键不算"null"），reason 说明原因。
        if !object.contains_key("state") {
            object.insert("state".to_string(), Value::Null);
        }
        if let Some(reason) = unavailable {
            object.insert(
                "rt_host_unavailable".to_string(),
                Value::String(reason.clone()),
            );
        }
    }
    snapshot
}

async fn format_prepare(
    State(state): State<Arc<DaemonState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let object = match payload.as_object() {
        Some(object) => object,
        None => return bad_request("request body must be a json object"),
    };
    if state.host_link().target() == Target::Process {
        return match state.control().prepare_format_json(object) {
            Ok(body) => (StatusCode::OK, Json(process_marker(body))),
            Err(err) => error_response(err),
        };
    }
    let mut request = serde_json::json!({ "op": "format" });
    let request_object = request.as_object_mut().expect("json object");
    for (key, value) in object {
        request_object.insert(key.clone(), value.clone());
    }
    forward(&state, request).await
}

/// M2 聚合状态：PLAN §4.4 的 `khaen-status/v1`（页面唯一数据源）。
///
/// 主路径是 socket status（rt-host 自报的 age/state/plan/format/device/counters，见 `host_link`）；
/// socket 不可用时退回状态文件（mode=file），文件也没有则 mode=none；只降级、任何情况下都返回 200
/// + null + reasons（见 `status` 模块，绝不 500、绝不编数值）。
async fn status(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    // socket status 往返可能阻塞（一个往返超时），放到 blocking 线程池；聚合本身仍是同步纯函数。
    let worker = Arc::clone(&state);
    let body = match tokio::task::spawn_blocking(move || {
        status::aggregate(&worker, std::time::SystemTime::now())
    })
    .await
    {
        Ok(body) => body,
        // 聚合任务 panic 也不能变成 500：/api/status 的契约是永远 200 + 结构化降级。
        Err(join_error) => status::internal_failure_body(&state, &join_error.to_string()),
    };
    (StatusCode::OK, Json(body))
}

async fn device_status(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    let monitor = state.device_json();
    if state.host_link().target() == Target::Process {
        // process 目标：设备快照来自 daemon 监督器，同样显式标 target（不冒充 rt-host）。
        let mut snapshot = device_snapshot(monitor, &host_link::LinkSnapshot::Process);
        if let Some(object) = snapshot.as_object_mut() {
            object.insert(
                "target".to_string(),
                Value::String(Target::Process.as_str().to_string()),
            );
        }
        return (StatusCode::OK, Json(snapshot));
    }
    let worker = Arc::clone(&state);
    let snapshot = tokio::task::spawn_blocking(move || worker.host_link().status_snapshot())
        .await
        .unwrap_or_else(|join_error| host_link::LinkSnapshot::Unavailable {
            socket: state.host_link().config().socket.clone(),
            reason: format!("rt-host status task failed: {join_error}"),
        });
    (StatusCode::OK, Json(device_snapshot(monitor, &snapshot)))
}

async fn device_retry(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    (StatusCode::OK, Json(state.device().request_retry()))
}

async fn mute(
    State(state): State<Arc<DaemonState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    // docs/API.md §4：字段名 mute；旧名 muted 作为别名接受（两者都给且冲突时 400）。
    let mute = match (payload.get("mute"), payload.get("muted")) {
        (Some(Value::Bool(mute)), None) | (None, Some(Value::Bool(mute))) => *mute,
        (Some(Value::Bool(mute)), Some(Value::Bool(muted))) if mute == muted => *mute,
        (Some(_), _) | (_, Some(_)) => {
            return bad_request("fields mute/muted must be booleans and must not disagree")
        }
        (None, None) => return bad_request("missing field: mute"),
    };
    if state.host_link().target() == Target::Process {
        // 原控制面路径先走完（语义不变），MOTU 设备写入只作为附加副作用；
        // 设备写失败会把状态码改成 502 并在响应里保留这里的 control 回执（不假成功）。
        let mut control_body = process_marker(state.control().mute_json(mute));
        if let Some(object) = control_body.as_object_mut() {
            object.insert("muted".to_string(), Value::Bool(mute));
        }
        return crate::motu::api::with_mute_side_effect(&state, mute, control_body).await;
    }
    let (status, Json(mut body)) =
        forward(&state, serde_json::json!({ "op": "mute", "mute": mute })).await;
    if status.is_success() {
        // 合同 §4：回执同时回传 mute（真实回读）与 muted 别名。
        let echo = body["mute"].clone();
        if let Some(object) = body.as_object_mut() {
            object.insert("muted".to_string(), echo);
        }
    }
    (status, Json(body))
}

async fn prepare(
    State(state): State<Arc<DaemonState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let (plan_path, plan_sha256) = match (payload.get("plan_path"), payload.get("plan_sha256")) {
        (Some(Value::String(path)), Some(Value::String(sha))) => (path.clone(), sha.clone()),
        _ => return bad_request("fields plan_path and plan_sha256 must be strings"),
    };
    if state.host_link().target() == Target::Process {
        // prepare 会读文件、解码、编译图：非实时路径。这里串行化控制面（音频线程不碰这把锁）。
        let result = state.control().prepare_json(&plan_path, &plan_sha256);
        return match result {
            Ok(body) => {
                // T55：旁路记录方案 routing 段（best-effort，失败不影响 prepare 响应）。
                record_prepared_plan(&state, plan_path.clone()).await;
                (StatusCode::OK, Json(process_marker(body)))
            }
            Err(err) => error_response(err),
        };
    }
    let reply = forward(
        &state,
        serde_json::json!({
            "op": "prepare",
            "plan_path": plan_path.clone(),
            "plan_sha256": plan_sha256,
        }),
    )
    .await;
    if reply.0.is_success() {
        record_prepared_plan(&state, plan_path).await;
    }
    reply
}

async fn commit(
    State(state): State<Arc<DaemonState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let engine_token = match payload.get("engine_token") {
        Some(Value::String(token)) => token.clone(),
        _ => return bad_request("field engine_token must be a string"),
    };
    if state.host_link().target() == Target::Process {
        let result = state.control().commit_json(&engine_token);
        return match result {
            Ok(body) => {
                record_commit(&state, &body);
                (StatusCode::OK, Json(process_marker(body)))
            }
            Err(err) => error_response(err),
        };
    }
    let reply = forward(
        &state,
        serde_json::json!({ "op": "commit", "engine_token": engine_token }),
    )
    .await;
    if reply.0.is_success() {
        // rt-host 的 commit 响应同样带 version/graph_id/applied_block，按真实回执记录。
        record_commit(&state, &reply.1 .0);
    }
    reply
}

async fn abort(
    State(state): State<Arc<DaemonState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let engine_token = match payload.get("engine_token") {
        Some(Value::String(token)) => token.clone(),
        _ => return bad_request("field engine_token must be a string"),
    };
    if state.host_link().target() == Target::Process {
        return match state.control().abort_json(&engine_token) {
            Ok(body) => (StatusCode::OK, Json(process_marker(body))),
            Err(err) => error_response(err),
        };
    }
    forward(
        &state,
        serde_json::json!({ "op": "abort", "engine_token": engine_token }),
    )
    .await
}

/// 语义界面第一阶段（volume/routing/layout）：只准备补丁草稿，commit 仍走 /api/commit。
async fn surface(
    State(state): State<Arc<DaemonState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let surface = match payload.get("surface") {
        Some(Value::String(surface)) => surface.clone(),
        Some(_) => return bad_request("field surface must be a string"),
        None => return bad_request("missing field: surface"),
    };
    let data = payload
        .get("data")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    if state.host_link().target() == Target::Process {
        return match state.control().prepare_surface_json(&surface, &data) {
            Ok(body) => (StatusCode::OK, Json(process_marker(body))),
            Err(err) => error_response(err),
        };
    }
    forward(
        &state,
        serde_json::json!({ "op": "surface", "surface": surface, "data": data }),
    )
    .await
}

/// 回滚第一阶段：把历史里的旧版本准备成草稿（版本号递增），commit 仍走 /api/commit。
async fn rollback(
    State(state): State<Arc<DaemonState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let target = match payload.get("target") {
        Some(Value::Number(number)) => match number.as_u64() {
            Some(version) => Some(version),
            None => return bad_request("field target must be a non-negative integer version"),
        },
        Some(Value::Null) | None => None,
        Some(_) => return bad_request("field target must be a non-negative integer version"),
    };
    if state.host_link().target() == Target::Process {
        return match state.control().rollback_json(target) {
            Ok(body) => (StatusCode::OK, Json(process_marker(body))),
            Err(err) => error_response(err),
        };
    }
    let mut request = serde_json::json!({ "op": "rollback" });
    if let Some(version) = target {
        request["target"] = serde_json::json!(version);
    }
    forward(&state, request).await
}

async fn coeff(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    if state.host_link().target() == Target::Process {
        return (
            StatusCode::OK,
            Json(process_marker(state.control().coeff_json())),
        );
    }
    forward(&state, serde_json::json!({ "op": "coeff" })).await
}

/// 只读试运行：读方案文件 → 解码 → 准入 → 图编译，不产生草稿、不改变正在跑的图。
async fn inspect(
    State(state): State<Arc<DaemonState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let plan_path = match payload.get("plan_path") {
        Some(Value::String(path)) => path.clone(),
        Some(_) => return bad_request("field plan_path must be a string"),
        None => return bad_request("missing field: plan_path"),
    };
    let plan_sha256 = match payload.get("plan_sha256") {
        Some(Value::String(sha)) => Some(sha.clone()),
        Some(_) => return bad_request("field plan_sha256 must be a string"),
        None => None,
    };
    if state.host_link().target() == Target::Process {
        return match state
            .control()
            .inspect_json(&plan_path, plan_sha256.as_deref())
        {
            Ok(body) => (
                StatusCode::OK,
                Json(inspect_contract_aliases(process_marker(body))),
            ),
            Err(err) => error_response(err),
        };
    }
    let mut request = serde_json::json!({ "op": "inspect", "plan_path": plan_path });
    if let Some(sha256) = plan_sha256 {
        request["plan_sha256"] = Value::String(sha256);
    }
    let (status, Json(body)) = forward(&state, request).await;
    if status.is_success() {
        (status, Json(inspect_contract_aliases(body)))
    } else {
        (status, Json(body))
    }
}

/// docs/API.md 第 3 节的 inspect 回执要点（decoded/admission/graph_id/budget）：
/// 控制面原始回执已含 ok/graph_id/admission（budget 在 admission.budget 内）与 plan 摘要。
/// 这里只做加法别名（不改任何原字段）：decoded = {staged,sha256,plan}，budget = admission.budget。
fn inspect_contract_aliases(mut body: Value) -> Value {
    if let Some(object) = body.as_object_mut() {
        if !object.contains_key("decoded") {
            let decoded = serde_json::json!({
                "staged": object.get("staged").cloned().unwrap_or(Value::Bool(false)),
                "sha256": object.get("sha256").cloned().unwrap_or(Value::Null),
                "plan": object.get("plan").cloned().unwrap_or(Value::Null),
            });
            object.insert("decoded".to_string(), decoded);
        }
        if !object.contains_key("budget") {
            let budget = object
                .get("admission")
                .and_then(|admission| admission.get("budget"))
                .cloned()
                .unwrap_or(Value::Null);
            object.insert("budget".to_string(), budget);
        }
    }
    body
}

/// prepare 成功后旁路记录方案元数据（T55：/api/channels 需要当前已生效方案的 routing 段）。
///
/// best-effort：读文件/解码失败不影响 prepare 的响应（prepare 已由控制面完成真实校验）；
/// 记录不到时 /api/channels 如实给 null + 原因。读盘与解码放 blocking 线程池，不堵 reactor。
async fn record_prepared_plan(state: &Arc<DaemonState>, plan_path: String) {
    let plans = state.plans().clone();
    let path = std::path::PathBuf::from(plan_path);
    let now = crate::status::rfc3339_utc(std::time::SystemTime::now());
    let _ = tokio::task::spawn_blocking(move || plans.remember_local_plan(&path, &now)).await;
}

/// commit 成功后记录已生效历史（T55：/api/plan/history 的真实来源）。
///
/// 只记录真正的方案提交（响应带 graph_id）；format / mode 草稿的 commit 不是方案版本，跳过。
/// T61：方案重新生效也意味着运行中的图又有已知 routing —— 清除 channels 镜像里的
/// routing 失效标记（此前 channels_commit 丢图后 /api/channels 如实报 mapping=null）。
fn record_commit(state: &DaemonState, response: &Value) {
    let now = crate::status::rfc3339_utc(std::time::SystemTime::now());
    state.plans().record_commit(response, "commit", &now);
    state.channels_mirror().note_plan_commit(response);
}

/// 控制面错误码 → HTTP 状态码（映射表见模块文档）。
fn http_status(code: &str) -> StatusCode {
    match code {
        error::BAD_REQUEST => StatusCode::BAD_REQUEST,
        error::UNKNOWN_OP => StatusCode::NOT_FOUND,
        error::UNSUPPORTED => StatusCode::NOT_IMPLEMENTED,
        error::ADMISSION_REJECTED
        | error::ENGINE_TOKEN_INVALID
        | error::NO_BASE_PLAN
        | error::NO_HISTORY => StatusCode::CONFLICT,
        host_link::HOST_LINK_UNAVAILABLE => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn error_response(err: ControlError) -> (StatusCode, Json<Value>) {
    (http_status(err.code), Json(err.to_json()))
}

fn bad_request(message: impl Into<String>) -> (StatusCode, Json<Value>) {
    error_response(ControlError::new(error::BAD_REQUEST, message))
}
