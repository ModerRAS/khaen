//! 监听地址解析：`KHAEN_BIND`（完整 socket 地址）优先，其次 `KHAEN_PORT` / `PORT`（仅端口）。
//!
//! - 默认 `127.0.0.1:8730`（开发/测试，只服务本机）；生产部署用 `KHAEN_BIND=0.0.0.0:80`。
//! - 非法值**启动即失败**：main 打印稳定错误（`invalid KHAEN_BIND ...` / `invalid KHAEN_PORT ...`）
//!   并以退出码 2 结束，不再静默回退默认值。
//! - 优先级：`KHAEN_BIND` > `KHAEN_PORT` > `PORT` > 默认。`KHAEN_BIND` 已给出时不再读端口变量
//!   （旧部署的 `KHAEN_PORT` 仍兼容，二者都只给端口时 `KHAEN_PORT` 胜出）。
//! - 测试用注入的 `lookup` 解析，不修改进程环境（避免测试间互相干扰）。

use std::fmt;
use std::net::SocketAddr;

/// 默认监听地址（开发/测试）：只绑定本机回环。
pub const DEFAULT_BIND: &str = "127.0.0.1:8730";
/// 完整监听地址环境变量（生产用 `0.0.0.0:80`）。
pub const BIND_VAR: &str = "KHAEN_BIND";
/// 仅端口环境变量（兼容旧配置）。
pub const PORT_VAR: &str = "KHAEN_PORT";
/// 通用端口环境变量（容器平台惯例；仅在 `KHAEN_PORT` 未设置时使用）。
pub const FALLBACK_PORT_VAR: &str = "PORT";

/// 监听地址解析错误（错误信息稳定，测试与运维可依赖）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindError {
    /// `KHAEN_BIND` 不是合法的 socket 地址（如缺少端口、主机名、空串）。
    InvalidBind {
        /// 原始环境变量值（原样回显，便于定位配置）。
        value: String,
        /// 解析器给出的原因。
        reason: String,
    },
    /// 端口变量不是合法 `u16`。
    InvalidPort {
        /// 出错的环境变量名（`KHAEN_PORT` 或 `PORT`）。
        variable: &'static str,
        /// 原始环境变量值。
        value: String,
    },
}

impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBind { value, reason } => write!(
                f,
                "invalid {BIND_VAR} value {value:?}: expected an IPv4/IPv6 socket address \
                 such as 127.0.0.1:8730 or 0.0.0.0:80 ({reason})"
            ),
            Self::InvalidPort { variable, value } => write!(
                f,
                "invalid {variable} value {value:?}: expected a port number between 0 and 65535"
            ),
        }
    }
}

impl std::error::Error for BindError {}

/// 用给定的环境变量查找函数解析监听地址（生产传 `std::env::var`，测试传假环境）。
pub fn resolve(lookup: impl Fn(&str) -> Option<String>) -> Result<SocketAddr, BindError> {
    if let Some(value) = lookup(BIND_VAR) {
        return value
            .parse::<SocketAddr>()
            .map_err(|error| BindError::InvalidBind {
                value,
                reason: error.to_string(),
            });
    }
    for variable in [PORT_VAR, FALLBACK_PORT_VAR] {
        if let Some(value) = lookup(variable) {
            let port = value
                .parse::<u16>()
                .map_err(|_| BindError::InvalidPort { variable, value })?;
            return Ok(SocketAddr::from(([127, 0, 0, 1], port)));
        }
    }
    Ok(default_addr())
}

/// 默认监听地址（`DEFAULT_BIND` 的解析结果；常量本身保证合法）。
pub fn default_addr() -> SocketAddr {
    DEFAULT_BIND
        .parse()
        .expect("DEFAULT_BIND is a valid socket address")
}

/// 从进程环境解析监听地址（daemon 入口使用）。
pub fn from_env() -> Result<SocketAddr, BindError> {
    resolve(|key| std::env::var(key).ok())
}
