//! 系统信息 / 日志 / 诊断 / 改密 / 重启 API（T46；M6 系统页后端提前到本轮）。
//!
//! 端点（全部挂到全局 auth 中间件之下；只有 /healthz 匿名）：
//! - GET /api/system/info：版本三口径（crate_version / release_version / commit）、构建信息、
//!   主机信息、服务状态（khaen-daemon / khaen-rt-host 的 active/enabled/pid）、声卡设备、
//!   控制通道实况、重启能力；缺失字段一律 null + reasons（字段路径 -> 原因）。
//! - GET /api/system/logs?unit=daemon|rt-host&lines=N：journalctl -u ... -n N --no-pager 最近 N 行，
//!   N 上限 500；journalctl 不可用返回 503 logs_unavailable + details.reason（不 500、不假成功）。
//! - GET /api/system/diagnostics：合并 status + system info + 设备 + 最近日志摘要的 JSON 下载，
//!   Content-Disposition 带文件名；不包含密码 / 会话 token / CSRF 等敏感字段。
//! - POST /api/system/password：{old,new}，复用 M1 认证语义（必须登录 + CSRF；旧密码错
//!   401 invalid_credentials；新密码短 400 invalid_password；成功作废旧会话并下发新会话）。
//! - POST /api/system/restart：{target:"rt-host"}，只有 KHAEN_ALLOW_RESTART=1 才启用
//!   （systemctl restart khaen-rt-host）；daemon 自身重启不实现（501 + reason）。
//!
//! 数据来源全部真实：build.rs 的构建期常量（release 版本 / commit / rustc / profile / target）、
//! 启动时注入的 DaemonState（bind / state_dir / rt_status_file / uptime）、host_link 的实况
//! 快照，以及 SystemProbe（/proc、systemctl、journalctl）的现场采集。采集失败只降级为
//! null + 原因，绝不编造数值，也绝不因为一条命令失败返回 500。
//!
//! 平台说明：SystemProbe 的默认实现 NativeProbe 在 Linux 上读 /proc、/etc/os-release 并调用
//! systemctl/journalctl；Windows 本机这些路径/命令不存在，因而如实全 null + 原因。测试通过
//! 注入假 SystemProbe（或 fake /proc 目录 + 假命令执行器）覆盖全部逻辑；真机 systemctl /
//! journalctl 语义属 NOT_RUN（由 CI / N150 覆盖）。

pub mod api;
pub mod build_info;
pub mod probe;

use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;

use crate::DaemonState;

pub use build_info::BuildInfo;
pub use probe::{DeviceInfo, HostPaths, NativeProbe, ServiceSnapshot, SystemProbe};

/// /api/system/info 的 schema（前端 T47 按需校验；存在必须匹配）。
pub const INFO_SCHEMA: &str = "khaen-system-info/v1";
/// /api/system/logs 的 schema。
pub const LOGS_SCHEMA: &str = "khaen-system-logs/v1";
/// /api/system/diagnostics 下载内容的 schema。
pub const DIAGNOSTICS_SCHEMA: &str = "khaen-diagnostics/v1";
/// 日志行数默认值（前端默认请求 200）。
pub const DEFAULT_LOG_LINES: usize = 200;
/// 日志行数上限（超过 400 invalid_request）。
pub const MAX_LOG_LINES: usize = probe::MAX_LOG_LINES;
/// 允许重启的目标（daemon 自身重启不实现）。
pub const RESTART_TARGET: &str = "rt-host";
/// 重启开关的环境变量（启动时解析进 DaemonState，handler 不读环境）。
pub const RESTART_ENV: &str = "KHAEN_ALLOW_RESTART";
/// rt-host 的 systemd unit 名。
pub const RT_HOST_UNIT: &str = probe::RT_HOST_UNIT;
/// daemon 的 systemd unit 名。
pub const DAEMON_UNIT: &str = probe::DAEMON_UNIT;

/// 稳定错误码（客户/前端按码分支，不随文案变化）。
pub mod error_code {
    /// 参数错（unit/lines/target/body 非法）。
    pub const INVALID_REQUEST: &str = "invalid_request";
    /// 新密码不满足最小长度（T47 契约的稳定码，与 /api/auth/password 的 weak_password 区分）。
    pub const INVALID_PASSWORD: &str = "invalid_password";
    /// 旧密码错（复用 M1 语义）。
    pub const INVALID_CREDENTIALS: &str = "invalid_credentials";
    /// journalctl 不可用/超时/退出码非 0。
    pub const LOGS_UNAVAILABLE: &str = "logs_unavailable";
    /// 重启未启用（KHAEN_ALLOW_RESTART 未设置）。
    pub const RESTART_DISABLED: &str = "restart_disabled";
    /// 目标不支持（daemon 自身重启）。
    pub const NOT_IMPLEMENTED: &str = "not_implemented";
    /// systemctl restart 失败。
    pub const RESTART_FAILED: &str = "restart_failed";
    /// 未预期的内部错误（采集/存储任务 panic 等）。
    pub const INTERNAL_ERROR: &str = "internal_error";
}

/// 逻辑日志单元 -> systemd unit 名。
pub fn log_unit_name(logical: &str) -> Option<&'static str> {
    match logical {
        "daemon" => Some(DAEMON_UNIT),
        "rt-host" | "rt_host" => Some(RT_HOST_UNIT),
        _ => None,
    }
}

/// 重启能力（启动时由 KHAEN_ALLOW_RESTART 解析；fail-closed：只有明确开启才允许）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartPolicy {
    allowed: bool,
    reason: Option<String>,
}

impl RestartPolicy {
    /// 允许 rt-host 重启（测试/显式配置用）。
    pub fn enabled() -> Self {
        Self {
            allowed: true,
            reason: None,
        }
    }

    /// 不允许重启，并携带原因。
    pub fn disabled(reason: impl Into<String>) -> Self {
        Self {
            allowed: false,
            reason: Some(reason.into()),
        }
    }

    /// 从进程环境解析（daemon 入口唯一读 KHAEN_ALLOW_RESTART 的地方）。
    pub fn from_env() -> Self {
        Self::from_value(std::env::var(RESTART_ENV).ok().as_deref())
    }

    /// 纯解析（测试不读进程环境）：只有 "1" 启用；未设置/空/其它值一律不启用并带原因。
    pub fn from_value(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            None | Some("") => Self::disabled(format!(
                "{RESTART_ENV} is not set: restarting rt-host is disabled (set {RESTART_ENV}=1 to enable)"
            )),
            Some("1") => Self::enabled(),
            Some("0") => Self::disabled(format!(
                "{RESTART_ENV}=0: restarting rt-host is explicitly disabled"
            )),
            Some(other) => Self::disabled(format!(
                "{RESTART_ENV}={other:?} is not recognized: only \"1\" enables restart"
            )),
        }
    }

    /// 是否允许重启。
    pub fn allowed(&self) -> bool {
        self.allowed
    }

    /// 不允许时的原因（允许时为 None）。
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self::from_value(None)
    }
}

/// 组装 /api/system/* 路由（挂到全局 auth 中间件之下；/healthz 由 lib::router 直接注册）。
pub fn routes() -> Router<Arc<DaemonState>> {
    Router::new()
        .route("/api/system/info", get(api::info))
        .route("/api/system/logs", get(api::logs))
        .route("/api/system/diagnostics", get(api::diagnostics))
        .route("/api/system/password", post(api::password))
        .route("/api/system/restart", post(api::restart))
}
