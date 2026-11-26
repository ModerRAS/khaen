//! daemon → rt-host 控制通道代理（T70f-b）。
//!
//! daemon（控制面，REST）与 rt-host（数据面：AES67 -> 方案图 -> ALSA）是两个进程。rt-host 在
//! `crates/rt-host/src/control/` 里提供契约 B 的行 JSON 服务端（UNIX socket，一行请求 / 一行响应），
//! 本模块是它的客户端：把 daemon 的 REST 控制请求转成契约 B 请求、把响应原样带回来；socket 不可用时
//! **诚实降级**，绝不偷偷作用于 daemon 进程内引擎冒充成功。
//!
//! # 目标开关（KHAEN_RT_TARGET）
//!
//! - `rt-host`（默认）：所有控制 op 经 UNIX socket 发给真正的 rt-host；socket 不可用时 REST 返回
//!   503 + 稳定错误码 [`HOST_LINK_UNAVAILABLE`]（不再作用于 daemon 进程内引擎）。
//! - `process`：仅测试/离线演示；op 仍作用于 daemon 进程内 [`ControlState`]（旧行为），
//!   响应与 `/api/status` 显式标 `target: "daemon-process"`，不会冒充 rt-host。
//!
//! socket 路径取 `KHAEN_RT_SOCKET`（默认 [`DEFAULT_SOCKET_PATH`]，与 rt-host 的
//! `KHAEN_CONTROL_SOCKET` 默认值一致）；单次往返超时取 `KHAEN_RT_TIMEOUT_MS`（默认
//! [`DEFAULT_TIMEOUT_MS`]；必须大于 rt-host 的 5 s 块边界 ack 上限，否则正常 commit 会被误判超时）。
//!
//! # 重试语义（一次，且不掩盖歧义）
//!
//! 连接失败时请求肯定没送达，所有 op 都重试一次。读到超时/协议错误时请求可能已经被执行：
//! - 只读 op（status/coeff/inspect/channels_status）与幂等/可替换 op
//!   （mute/prepare/surface/format/rollback/abort/channels_prepare）重试一次；
//! - `commit`、`channels_commit`、`channels_abort` **不重试**：第一次请求若已生效，
//!   重试只会得到 `engine_token_invalid` / `no_channels_draft` / `channels_busy`，
//!   把“已生效”报成失败（比超时更糟）。这里如实返回 503 + 原因，不制造假的成功/失败。
//!
//! # 分层
//!
//! ```text
//! REST handler ──HostLink::call──> Transport（trait，平台无关）
//!                                    ├─ cfg(unix): UnixTransport（Unix domain socket）
//!                                    └─ 其它平台: UnsupportedTransport（诚实报错，status 退回状态文件）
//!                          └── protocol::{encode_request, decode_response, exchange}（纯函数，Windows 可测）
//! ```

pub mod protocol;
pub mod transport;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

pub use protocol::{ProtocolError, WireReply};
pub use transport::{Transport, TransportError};

/// rt-host 控制 socket 路径环境变量（与 rt-host 的 `KHAEN_CONTROL_SOCKET` 语义对应）。
pub const SOCKET_ENV: &str = "KHAEN_RT_SOCKET";
/// 目标环境变量：`rt-host`（默认）| `process`（仅测试/离线演示）。
pub const TARGET_ENV: &str = "KHAEN_RT_TARGET";
/// 单次请求/响应往返超时环境变量（毫秒；默认 [`DEFAULT_TIMEOUT_MS`]）。
pub const TIMEOUT_ENV: &str = "KHAEN_RT_TIMEOUT_MS";
/// 默认 UNIX socket 路径（与 rt-host `DEFAULT_CONTROL_SOCKET` 相同）。
pub const DEFAULT_SOCKET_PATH: &str = "/run/khaen/rt-host.sock";
/// 默认往返超时（毫秒）：必须大于 rt-host 的 5 s 块边界 ack 上限（含设备缺失时的轮询余量）。
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;
/// 控制通道不可用时 REST 的稳定错误码（客户端可据此重试/提示，不随文案变化）。
pub const HOST_LINK_UNAVAILABLE: &str = "host_link_unavailable";

/// 已转发给 rt-host 的控制 op（不含只读的 status；与 `khaen_host::control::WIRED_OPS` 对齐）。
pub const FORWARDED_OPS: &[&str] = &[
    "mute", "prepare", "commit", "abort", "coeff", "inspect", "surface", "rollback", "format",
];
/// 只读状态 op。
pub const STATUS_OP: &str = "status";

/// rt-host M3b 热重配 op（T61）：与 `crates/rt-host/src/control/reconfigure.rs` 的
/// `CHANNELS_OPS` 同源。它们不在契约 B 的 `khaen_host::control::WIRED_OPS` 里——是 rt-host
/// 控制层对契约 B 的增量能力（同一行 JSON 通道、同一份传输与编解码），因此单独一份常量，
/// 不并入 [`FORWARDED_OPS`]（那个断言必须与 `WIRED_OPS` 一一对应）。
pub const CHANNELS_OPS: &[&str] = &[
    "channels_status",
    "channels_prepare",
    "channels_commit",
    "channels_abort",
];

/// 控制目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// 真数据面：经 UNIX socket 命令 rt-host（默认）。
    RtHost,
    /// 仅测试/离线演示：作用于 daemon 进程内引擎，响应/状态标 `daemon-process`。
    Process,
}

impl Target {
    /// 稳定字符串（`/api/status` / `/api/meta` 与响应标记共用）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RtHost => "rt-host",
            Self::Process => "daemon-process",
        }
    }

    /// 解析环境变量值（大小写不敏感）。
    fn parse(value: &str) -> Result<Self, ConfigError> {
        let value = value.trim();
        if value.eq_ignore_ascii_case("rt-host") || value.eq_ignore_ascii_case("rt_host") {
            Ok(Self::RtHost)
        } else if value.eq_ignore_ascii_case("process") {
            Ok(Self::Process)
        } else {
            Err(ConfigError::InvalidTarget {
                value: value.to_string(),
            })
        }
    }
}

/// 控制通道配置（启动时由 `main` 从环境变量解析并注入，handler 不读环境）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostLinkConfig {
    /// 控制目标（默认 rt-host）。
    pub target: Target,
    /// UNIX socket 路径（process 目标下不使用，但保留配置便于诊断）。
    pub socket: PathBuf,
    /// 单次往返超时。
    pub timeout: Duration,
}

impl Default for HostLinkConfig {
    fn default() -> Self {
        Self::rt_host(DEFAULT_SOCKET_PATH)
    }
}

impl HostLinkConfig {
    /// rt-host 目标 + 指定 socket 路径 + 默认超时。
    pub fn rt_host(path: impl Into<PathBuf>) -> Self {
        Self {
            target: Target::RtHost,
            socket: path.into(),
            timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
        }
    }

    /// process 目标（仅测试/离线演示；socket/超时保留默认值但不使用）。
    pub fn process() -> Self {
        Self {
            target: Target::Process,
            ..Self::rt_host(DEFAULT_SOCKET_PATH)
        }
    }

    /// 设置往返超时（测试用短超时）。
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// 纯解析（测试不读进程环境）：三个值都是可选原始环境变量值。
    pub fn parse(
        target: Option<&str>,
        socket: Option<&str>,
        timeout_ms: Option<&str>,
    ) -> Result<Self, ConfigError> {
        let target = match target {
            None => Target::RtHost,
            Some(value) if value.trim().is_empty() => Target::RtHost,
            Some(value) => Target::parse(value)?,
        };
        let socket = match socket {
            None => PathBuf::from(DEFAULT_SOCKET_PATH),
            Some(value) if value.trim().is_empty() => return Err(ConfigError::InvalidSocket),
            Some(value) => PathBuf::from(value.trim()),
        };
        let timeout = match timeout_ms {
            None => Duration::from_millis(DEFAULT_TIMEOUT_MS),
            Some(value) => {
                let millis =
                    value
                        .trim()
                        .parse::<u64>()
                        .map_err(|_| ConfigError::InvalidTimeout {
                            value: value.to_string(),
                        })?;
                if millis == 0 {
                    return Err(ConfigError::InvalidTimeout {
                        value: value.to_string(),
                    });
                }
                Duration::from_millis(millis)
            }
        };
        Ok(Self {
            target,
            socket,
            timeout,
        })
    }

    /// 从进程环境解析（daemon 入口使用；唯一读这些变量的地方）。
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::parse(
            std::env::var(TARGET_ENV).ok().as_deref(),
            std::env::var(SOCKET_ENV).ok().as_deref(),
            std::env::var(TIMEOUT_ENV).ok().as_deref(),
        )
    }
}

/// 配置错误（非法值点名变量 + 原值，启动即失败而不是静默回退）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// `KHAEN_RT_TARGET` 不是 rt-host/process。
    InvalidTarget {
        /// 原始值。
        value: String,
    },
    /// `KHAEN_RT_SOCKET` 是空串。
    InvalidSocket,
    /// `KHAEN_RT_TIMEOUT_MS` 不是正整数毫秒。
    InvalidTimeout {
        /// 原始值。
        value: String,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTarget { value } => write!(
                f,
                "invalid {TARGET_ENV} value {value:?}: expected \"rt-host\" (default) or \"process\" (test/offline only)"
            ),
            Self::InvalidSocket => {
                write!(f, "invalid {SOCKET_ENV} value: must not be empty")
            }
            Self::InvalidTimeout { value } => write!(
                f,
                "invalid {TIMEOUT_ENV} value {value:?}: expected a positive number of milliseconds"
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

/// 控制通道传输层错误（连接/IO/超时/协议）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkError {
    /// 稳定错误码（目前只有 [`HOST_LINK_UNAVAILABLE`]）。
    pub code: &'static str,
    /// 人读说明（含 socket 路径与尝试次数；不含敏感信息）。
    pub message: String,
}

impl LinkError {
    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            code: HOST_LINK_UNAVAILABLE,
            message: message.into(),
        }
    }

    /// REST 错误体（与既有错误形状一致：`{ok:false,error,message}`）。
    pub fn to_json(&self) -> Value {
        json!({ "ok": false, "error": self.code, "message": self.message })
    }
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for LinkError {}

/// `/api/status` 的数据来源快照（每次聚合现取，不缓存，避免报旧数据）。
#[derive(Debug)]
pub enum LinkSnapshot {
    /// rt-host 控制 socket 应答了 status：带 rt-host 自己报的完整状态 + 快照年龄。
    Socket {
        /// 配置的 socket 路径（诊断用）。
        socket: PathBuf,
        /// rt-host 的 status 响应体（契约 B，见 `crates/rt-host/src/control/core.rs`）。
        status: Value,
        /// 快照年龄（毫秒；rt-host 首个快照发布前为 `None`）。
        age_ms: Option<u64>,
    },
    /// rt-host 目标但 socket 调用失败（原因含重试次数；status 聚合按实况退回状态文件）。
    Unavailable {
        /// 配置的 socket 路径（诊断用）。
        socket: PathBuf,
        /// 稳定的人读原因。
        reason: String,
    },
    /// process 目标：控制 op 作用于 daemon 进程内引擎，不访问 socket。
    Process,
}

impl LinkSnapshot {
    /// socket 路径（process 目标为 None）。
    pub fn socket(&self) -> Option<&Path> {
        match self {
            Self::Socket { socket, .. } | Self::Unavailable { socket, .. } => Some(socket),
            Self::Process => None,
        }
    }

    /// 是否拿到了 socket status。
    pub fn is_socket(&self) -> bool {
        matches!(self, Self::Socket { .. })
    }
}

/// daemon 到 rt-host 的控制通道客户端（一个进程一份，内部传输实现可注入）。
pub struct HostLink {
    config: HostLinkConfig,
    transport: Arc<dyn Transport>,
}

impl fmt::Debug for HostLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostLink")
            .field("target", &self.config.target)
            .field("socket", &self.config.socket)
            .field("timeout", &self.config.timeout)
            .field("transport", &self.transport)
            .finish()
    }
}

impl HostLink {
    /// 生产构造：按配置选择传输（Unix 平台是真实 socket；其它平台是诚实的 unsupported 传输）。
    pub fn new(config: HostLinkConfig) -> Arc<Self> {
        let transport = transport::for_config(&config);
        Arc::new(Self { config, transport })
    }

    /// 测试/嵌入用：注入自定义传输（内存假实现或 TCP 回环），协议/重试逻辑完全同一份。
    pub fn with_transport(config: HostLinkConfig, transport: Arc<dyn Transport>) -> Arc<Self> {
        Arc::new(Self { config, transport })
    }

    /// process 目标（旧行为；响应/状态标 `daemon-process`）。
    pub fn process() -> Arc<Self> {
        Self::new(HostLinkConfig::process())
    }

    /// 当前配置。
    pub fn config(&self) -> &HostLinkConfig {
        &self.config
    }

    /// 控制目标。
    pub fn target(&self) -> Target {
        self.config.target
    }

    /// 配置的 socket 路径；process 目标下 `/api/status` 的 `host_link.socket` 为 null（不使用）。
    pub fn socket(&self) -> Option<&Path> {
        match self.config.target {
            Target::RtHost => Some(&self.config.socket),
            Target::Process => None,
        }
    }

    /// 转发一条契约 B 请求（一行 JSON）并返回服务端回复。
    ///
    /// 重试策略见模块文档：连接失败重试一次；超时/协议错误只对非 `commit` 的 op 重试一次。
    /// 最终失败返回 [`LinkError`]（稳定码 [`HOST_LINK_UNAVAILABLE`]），调用方映射成 503。
    pub fn call(&self, request: Value) -> Result<WireReply, LinkError> {
        if self.config.target == Target::Process {
            // 防御：handler 在 process 目标下不该走到这里（会直接作用于进程内引擎并打标记）。
            return Err(LinkError::unavailable(format!(
                "no rt-host control link in {TARGET_ENV}=process mode: control ops act on the daemon process engine"
            )));
        }
        let op = request
            .get("op")
            .and_then(Value::as_str)
            .unwrap_or("<missing op>")
            .to_string();
        let mut attempts: u8 = 0;
        // 循环只在“第一次失败且可重试”时继续；结束（break）时 last 一定有值。
        let last: TransportError;
        loop {
            attempts += 1;
            match self.transport.exchange(&request, self.config.timeout) {
                Ok(reply) => return Ok(reply),
                Err(error) => {
                    let retry = attempts == 1
                        && (error.definitely_not_delivered() || retry_after_ambiguous_failure(&op));
                    if !retry {
                        last = error;
                        break;
                    }
                }
            }
        }
        let error = last;
        Err(LinkError::unavailable(format!(
            "rt-host control socket {} is unavailable (op {op}, {attempts} attempt(s), timeout {}ms): {error}",
            self.config.socket.display(),
            self.config.timeout.as_millis()
        )))
    }

    /// 取一次 rt-host 状态快照（`/api/status` 的主数据源）；失败如实返回原因，不编状态。
    pub fn status_snapshot(&self) -> LinkSnapshot {
        match self.config.target {
            Target::Process => LinkSnapshot::Process,
            Target::RtHost => match self.call(json!({ "op": STATUS_OP })) {
                Ok(WireReply::Success(status)) => LinkSnapshot::Socket {
                    socket: self.config.socket.clone(),
                    age_ms: status.get("age_ms").and_then(Value::as_u64),
                    status,
                },
                Ok(WireReply::Failure { code, message, .. }) => LinkSnapshot::Unavailable {
                    socket: self.config.socket.clone(),
                    reason: format!("rt-host rejected the status op: {code}: {message}"),
                },
                Err(error) => LinkSnapshot::Unavailable {
                    socket: self.config.socket.clone(),
                    reason: error.message,
                },
            },
        }
    }
}

/// 请求可能已被执行之后，重试是否安全：提交类 op 重试会把成功报成失败/忙。
///
/// - `commit`：重试只会得到 `engine_token_invalid`（草稿已消费）；
/// - `channels_commit`：重试可能得到 `channels_busy`（首笔仍在飞）或 `no_channels_draft`；
/// - `channels_abort`：首笔若已丢弃草稿，重试会得到 `no_channels_draft`（把“已 abort”报成错误）。
fn retry_after_ambiguous_failure(op: &str) -> bool {
    !matches!(op, "commit" | "channels_commit" | "channels_abort")
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;

    /// 脚本化的假传输：按顺序吐出预设结果，并记录所有请求（测试独占，无并发）。
    #[derive(Debug, Default)]
    struct FakeTransport {
        outcomes: Mutex<VecDeque<Result<WireReply, TransportError>>>,
        calls: Mutex<Vec<Value>>,
    }

    impl FakeTransport {
        fn scripted(outcomes: Vec<Result<WireReply, TransportError>>) -> Arc<Self> {
            Arc::new(Self {
                outcomes: Mutex::new(outcomes.into()),
                calls: Mutex::new(Vec::new()),
            })
        }

        fn call_count(&self) -> usize {
            self.calls.lock().expect("calls").len()
        }

        fn calls(&self) -> Vec<Value> {
            self.calls.lock().expect("calls").clone()
        }
    }

    impl Transport for FakeTransport {
        fn exchange(
            &self,
            request: &Value,
            _timeout: Duration,
        ) -> Result<WireReply, TransportError> {
            self.calls.lock().expect("calls").push(request.clone());
            self.outcomes
                .lock()
                .expect("outcomes")
                .pop_front()
                .unwrap_or_else(|| {
                    Err(TransportError::Connect(
                        "no scripted transport outcome left".to_string(),
                    ))
                })
        }
    }

    fn link(
        outcomes: Vec<Result<WireReply, TransportError>>,
        op_timeout_ms: u64,
    ) -> (Arc<HostLink>, Arc<FakeTransport>) {
        let transport = FakeTransport::scripted(outcomes);
        let config = HostLinkConfig::rt_host("/tmp/khaen-test.sock")
            .with_timeout(Duration::from_millis(op_timeout_ms));
        let injected: Arc<dyn Transport> = transport.clone();
        let link = HostLink::with_transport(config, injected);
        (link, transport)
    }

    fn success() -> WireReply {
        WireReply::Success(json!({ "ok": true }))
    }

    #[test]
    fn default_config_is_rt_host_with_the_shared_socket_path() {
        let config = HostLinkConfig::parse(None, None, None).expect("defaults");
        assert_eq!(config.target, Target::RtHost);
        assert_eq!(config.socket, PathBuf::from(DEFAULT_SOCKET_PATH));
        assert_eq!(config.socket, PathBuf::from("/run/khaen/rt-host.sock"));
        assert_eq!(config.timeout, Duration::from_millis(DEFAULT_TIMEOUT_MS));
        // 必须比 rt-host 的 5 s 块边界 ack 上限宽裕，否则正常 commit 会被误判超时（编译期断言）。
        const _: () = assert!(DEFAULT_TIMEOUT_MS > 5_000);
    }

    #[test]
    fn parse_accepts_explicit_values_and_rejects_malformed_ones() {
        let config = HostLinkConfig::parse(Some("process"), Some("/tmp/x.sock"), Some("250"))
            .expect("parse");
        assert_eq!(config.target, Target::Process);
        assert_eq!(config.socket, PathBuf::from("/tmp/x.sock"));
        assert_eq!(config.timeout, Duration::from_millis(250));

        // 大小写不敏感 + 空串目标按默认。
        assert_eq!(
            HostLinkConfig::parse(Some("RT-HOST"), None, None)
                .expect("target")
                .target,
            Target::RtHost
        );
        assert_eq!(
            HostLinkConfig::parse(Some("  "), None, None)
                .expect("blank")
                .target,
            Target::RtHost
        );

        let error = HostLinkConfig::parse(Some("rt-host-typo"), None, None).expect_err("target");
        assert!(matches!(error, ConfigError::InvalidTarget { .. }));
        assert!(error.to_string().contains(TARGET_ENV), "{error}");
        assert!(error.to_string().contains("rt-host-typo"), "{error}");

        let error = HostLinkConfig::parse(None, Some("  "), None).expect_err("socket");
        assert!(matches!(error, ConfigError::InvalidSocket));
        assert!(error.to_string().contains(SOCKET_ENV), "{error}");

        for value in ["abc", "0", "-5", "1.5"] {
            let error = HostLinkConfig::parse(None, None, Some(value)).expect_err("timeout");
            assert!(
                matches!(error, ConfigError::InvalidTimeout { .. }),
                "{value}"
            );
            assert!(error.to_string().contains(TIMEOUT_ENV), "{error}");
        }
    }

    #[test]
    fn forwarded_ops_match_the_contract_wired_ops() {
        // 转发集合必须来自契约 B 的已实现 op（不能自造 op），且除 status 外一项不少。
        let wired = khaen_host::control::WIRED_OPS;
        assert_eq!(wired.len(), FORWARDED_OPS.len() + 1, "wired={wired:?}");
        assert!(wired.contains(&STATUS_OP), "status 必须已实现");
        for op in FORWARDED_OPS {
            assert!(
                wired.contains(op),
                "转发 op {op} 不在契约 B 的 WIRED_OPS 里"
            );
        }
    }

    #[test]
    fn call_retries_once_after_a_connect_failure() {
        let (link, transport) = link(
            vec![
                Err(TransportError::Connect("socket missing".into())),
                Ok(success()),
            ],
            200,
        );
        let reply = link
            .call(json!({ "op": "mute", "mute": true }))
            .expect("retry succeeds");
        assert!(!reply.is_failure());
        assert_eq!(transport.call_count(), 2, "连接失败必须重试一次");
        let calls = transport.calls();
        assert_eq!(calls[0], calls[1], "重试的是同一条请求");
        assert_eq!(calls[0]["op"], json!("mute"));
        assert_eq!(calls[0]["mute"], json!(true));
    }

    #[test]
    fn call_retries_read_only_ops_after_a_timeout() {
        let (link, transport) = link(
            vec![
                Err(TransportError::Timeout("no response".into())),
                Ok(WireReply::Success(
                    json!({ "ok": true, "runtime_state": "running" }),
                )),
            ],
            50,
        );
        let reply = link
            .call(json!({ "op": "status" }))
            .expect("retry succeeds");
        assert!(matches!(reply, WireReply::Success(_)));
        assert_eq!(transport.call_count(), 2, "只读 op 超时重试一次");
    }

    #[test]
    fn call_does_not_retry_commit_after_an_ambiguous_timeout() {
        let (link, transport) = link(
            vec![Err(TransportError::Timeout("block loop busy".into()))],
            50,
        );
        let error = link
            .call(json!({ "op": "commit", "engine_token": "t" }))
            .expect_err("timeout");
        assert_eq!(error.code, HOST_LINK_UNAVAILABLE);
        assert_eq!(
            transport.call_count(),
            1,
            "commit 超时不能重试（避免把已生效报成失败）"
        );
        assert!(error.message.contains("commit"), "{}", error.message);
        assert!(error.message.contains("1 attempt"), "{}", error.message);
        assert!(!error.message.contains("2 attempt"), "{}", error.message);
    }

    #[test]
    fn call_still_retries_commit_when_the_request_was_never_delivered() {
        let (link, transport) = link(
            vec![
                Err(TransportError::Connect("no listener".into())),
                Ok(WireReply::Success(
                    json!({ "ok": true, "applied_block": 9 }),
                )),
            ],
            50,
        );
        let reply = link
            .call(json!({ "op": "commit", "engine_token": "t" }))
            .expect("connect failure is safe to retry");
        assert!(matches!(reply, WireReply::Success(_)));
        assert_eq!(transport.call_count(), 2);
    }

    #[test]
    fn call_surfaces_server_errors_without_retrying() {
        let failure = WireReply::Failure {
            code: "engine_token_invalid".into(),
            message: "no prepared plan draft".into(),
            body: json!({
                "ok": false,
                "error": "engine_token_invalid",
                "message": "no prepared plan draft",
            }),
        };
        let (link, transport) = link(vec![Ok(failure.clone()), Ok(success())], 50);
        let reply = link
            .call(json!({ "op": "commit", "engine_token": "bad" }))
            .expect("server reply is a reply");
        assert_eq!(reply, failure);
        assert_eq!(
            transport.call_count(),
            1,
            "服务端业务错误是有效响应，不重试"
        );
    }

    #[test]
    fn call_reports_persistent_failure_after_one_retry_with_stable_code() {
        let (link, transport) = link(
            vec![
                Err(TransportError::Timeout("first".into())),
                Err(TransportError::Timeout("second".into())),
            ],
            50,
        );
        let error = link
            .call(json!({ "op": "mute", "mute": true }))
            .expect_err("down");
        assert_eq!(error.code, HOST_LINK_UNAVAILABLE);
        assert_eq!(transport.call_count(), 2, "超时后重试一次，仍失败才报错");
        assert!(error.message.contains("2 attempt"), "{}", error.message);
        assert!(
            error.message.contains("/tmp/khaen-test.sock"),
            "{}",
            error.message
        );
        assert!(error.message.contains("timeout 50ms"), "{}", error.message);
        // 错误体形状与 REST 既有错误一致。
        let body = error.to_json();
        assert_eq!(body["ok"], json!(false));
        assert_eq!(body["error"], json!(HOST_LINK_UNAVAILABLE));
        assert!(body["message"]
            .as_str()
            .is_some_and(|text| !text.is_empty()));
    }

    #[test]
    fn call_in_process_mode_fails_closed_instead_of_pretending() {
        let transport = FakeTransport::scripted(vec![Ok(success())]);
        let injected: Arc<dyn Transport> = transport.clone();
        let link = HostLink::with_transport(HostLinkConfig::process(), injected);
        let error = link
            .call(json!({ "op": "mute", "mute": true }))
            .expect_err("no link");
        assert_eq!(error.code, HOST_LINK_UNAVAILABLE);
        assert!(error.message.contains("process"), "{}", error.message);
        assert_eq!(transport.call_count(), 0, "process 目标不该碰 socket");
    }

    #[test]
    fn status_snapshot_uses_the_socket_status_and_reports_its_age() {
        let status = json!({
            "ok": true,
            "runtime_state": "running",
            "age_ms": 42,
            "format": { "sample_rate": 48000, "block_size": 128, "channels": 22 },
        });
        let (link, transport) = link(vec![Ok(WireReply::Success(status.clone()))], 50);
        match link.status_snapshot() {
            LinkSnapshot::Socket {
                socket,
                status: got,
                age_ms,
            } => {
                assert_eq!(socket, PathBuf::from("/tmp/khaen-test.sock"));
                assert_eq!(got, status);
                assert_eq!(age_ms, Some(42));
            }
            other => panic!("expected socket snapshot: {other:?}"),
        }
        assert_eq!(transport.calls()[0], json!({ "op": "status" }));
    }

    #[test]
    fn status_snapshot_degrades_to_unavailable_with_the_real_reason() {
        let (link, transport) = link(
            vec![
                Err(TransportError::Connect("ENOENT".into())),
                Err(TransportError::Connect("ENOENT".into())),
            ],
            50,
        );
        match link.status_snapshot() {
            LinkSnapshot::Unavailable { socket, reason } => {
                assert_eq!(socket, PathBuf::from("/tmp/khaen-test.sock"));
                assert!(reason.contains("unavailable"), "{reason}");
                assert!(reason.contains("2 attempt"), "{reason}");
            }
            other => panic!("expected unavailable: {other:?}"),
        }
        assert_eq!(
            transport.call_count(),
            2,
            "status 是只读 op，连接失败重试一次"
        );
    }

    #[test]
    fn status_snapshot_in_process_mode_never_touches_the_socket() {
        let transport = FakeTransport::scripted(vec![]);
        let injected: Arc<dyn Transport> = transport.clone();
        let link = HostLink::with_transport(HostLinkConfig::process(), injected);
        assert!(matches!(link.status_snapshot(), LinkSnapshot::Process));
        assert_eq!(transport.call_count(), 0);
        assert_eq!(link.socket(), None, "process 目标不使用 socket");
        assert_eq!(link.target(), Target::Process);
        assert_eq!(Target::Process.as_str(), "daemon-process");
        assert_eq!(Target::RtHost.as_str(), "rt-host");
    }

    #[test]
    fn retry_policy_excludes_commit_like_ops_after_ambiguous_failures() {
        for op in FORWARDED_OPS
            .iter()
            .chain(std::iter::once(&STATUS_OP))
            .chain(CHANNELS_OPS.iter())
        {
            let expected = !matches!(*op, "commit" | "channels_commit" | "channels_abort");
            assert_eq!(retry_after_ambiguous_failure(op), expected, "{op}");
        }
    }

    #[test]
    fn channels_ops_are_a_separate_rt_host_extension_set() {
        // 热重配 op 是 rt-host 控制层对契约 B 的增量；不能混进 FORWARDED_OPS（它与
        // WIRED_OPS 一一对应），也不能自造 op（必须 channels_ 前缀且是这四个）。
        assert_eq!(
            CHANNELS_OPS,
            &[
                "channels_status",
                "channels_prepare",
                "channels_commit",
                "channels_abort"
            ]
        );
        for op in CHANNELS_OPS {
            assert!(op.starts_with("channels_"), "{op}");
            assert!(
                !FORWARDED_OPS.contains(op),
                "{op} must not be in FORWARDED_OPS"
            );
            assert!(
                !khaen_host::control::WIRED_OPS.contains(op),
                "{op} is not part of contract B WIRED_OPS"
            );
        }
    }

    #[test]
    fn channels_commit_and_abort_are_not_retried_after_ambiguous_failures() {
        for op in ["channels_commit", "channels_abort"] {
            let (link, transport) = link(vec![Err(TransportError::Timeout("busy".into()))], 50);
            let error = link
                .call(json!({ "op": op, "token": "t" }))
                .expect_err("timeout");
            assert_eq!(error.code, HOST_LINK_UNAVAILABLE);
            assert_eq!(transport.call_count(), 1, "{op} must not be retried");
        }
        // channels_prepare 可替换：超时后重试一次（幂等，rt-host 返回同一 token）。
        let (link, transport) = link(
            vec![
                Err(TransportError::Timeout("slow probe".into())),
                Ok(success()),
            ],
            50,
        );
        link.call(json!({ "op": "channels_prepare", "aes67_port": 5006 }))
            .expect("retry succeeds");
        assert_eq!(transport.call_count(), 2);
    }
}
