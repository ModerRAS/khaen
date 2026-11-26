//! 控制通道传输实现：Unix domain socket（`cfg(unix)`）；其它平台是诚实的 unsupported 传输。
//!
//! 协议帧（一行 JSON 请求 / 一行 JSON 响应）在 [`crate::host_link::protocol`]，与平台无关；
//! 这里只负责“把连接塞进去”，所以同一份编解码在两个平台完全一致，Windows 用注入的假传输/TCP 回环
//! 也能跑完整语义测试。
//!
//! 生产传输的约束：
//! - 每次 op 一条短连接（rt-host 每连接一个处理线程，协议无连接状态），调用完即关闭；
//! - 连接后设置读写超时（`KHAEN_RT_TIMEOUT_MS`），超时按 [`TransportError::Timeout`] 上报，
//!   不无限挂住 daemon 的 REST 线程；
//! - 连接失败（socket 不存在/被拒）是“请求肯定没送达”，与“已送达但没回音”区分开，供重试策略使用。

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use super::protocol::{ExchangeError, WireReply};
use super::HostLinkConfig;

/// 控制通道传输层（可注入：生产是 Unix socket，测试是内存假实现/TCP 回环）。
pub trait Transport: Send + Sync + fmt::Debug {
    /// 发一行 JSON 请求并读一行 JSON 响应（已分类）。
    ///
    /// `timeout` 由调用方给出（每次往返的超时）；实现必须让阻塞读超时后返回
    /// [`TransportError::Timeout`]，而不是挂住调用线程。
    fn exchange(&self, request: &Value, timeout: Duration) -> Result<WireReply, TransportError>;
}

/// 传输层错误。
///
/// 关键区分：`Connect` 表示连接没建立（请求肯定没送达，重试安全）；其余变体表示连接已建立，
/// 请求可能已被服务端执行（重试的语义不确定，由上层按 op 决定）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// 连接没建立（socket 不存在 / 被拒 / 平台不支持）。
    Connect(String),
    /// 连接后读/写 IO 失败（含对端提前关闭）。
    Io(String),
    /// 等响应超时。
    Timeout(String),
    /// 对端回了不合契约的行（坏 JSON / 缺 ok / 超长行）。
    Protocol(String),
}

impl TransportError {
    /// 稳定类别（诊断/测试用）。
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Connect(_) => "connect",
            Self::Io(_) => "io",
            Self::Timeout(_) => "timeout",
            Self::Protocol(_) => "protocol",
        }
    }

    /// 人读说明。
    pub fn message(&self) -> &str {
        match self {
            Self::Connect(message)
            | Self::Io(message)
            | Self::Timeout(message)
            | Self::Protocol(message) => message,
        }
    }

    /// 请求是否**肯定**没送达（只有连接失败能保证；其余都可能已送达，重试语义不确定）。
    pub fn definitely_not_delivered(&self) -> bool {
        matches!(self, Self::Connect(_))
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind(), self.message())
    }
}

impl std::error::Error for TransportError {}

/// 按配置选择生产传输（Unix 平台是真实 socket；其它平台是 unsupported，status 会如实退回状态文件）。
pub fn for_config(config: &HostLinkConfig) -> Arc<dyn Transport> {
    #[cfg(unix)]
    {
        Arc::new(unix::UnixTransport::new(config.socket.clone()))
    }
    #[cfg(not(unix))]
    {
        let _ = config;
        Arc::new(UnsupportedTransport)
    }
}

/// 把 [`ExchangeError`] 分类成传输层错误（超时的 `ErrorKind` 映射成 [`TransportError::Timeout`]）。
///
/// 生产路径只有 Unix 传输用到它；Windows 上由单测覆盖，所以不能整段 cfg 掉。
#[cfg_attr(not(unix), allow(dead_code))]
fn classify_exchange_error(error: ExchangeError) -> TransportError {
    match error {
        ExchangeError::Io(error) => {
            let message = error.to_string();
            match error.kind() {
                // Unix 上 SO_RCVTIMEO 超时是 WouldBlock，Windows 上是 TimedOut。
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
                    TransportError::Timeout(message)
                }
                _ => TransportError::Io(message),
            }
        }
        ExchangeError::Protocol(error) => TransportError::Protocol(error.to_string()),
    }
}

#[cfg(unix)]
mod unix {
    use std::io::{BufReader, BufWriter};
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;
    use std::time::Duration;

    use serde_json::Value;

    use super::super::protocol::{self, WireReply};
    use super::{classify_exchange_error, Transport, TransportError};

    /// Unix domain socket 传输：每次 op 一条短连接（rt-host 的协议本身无连接状态）。
    #[derive(Debug)]
    pub(super) struct UnixTransport {
        path: PathBuf,
    }

    impl UnixTransport {
        pub(super) fn new(path: PathBuf) -> Self {
            Self { path }
        }
    }

    impl Transport for UnixTransport {
        fn exchange(
            &self,
            request: &Value,
            timeout: Duration,
        ) -> Result<WireReply, TransportError> {
            let stream = UnixStream::connect(&self.path).map_err(|error| {
                TransportError::Connect(format!(
                    "cannot connect to rt-host control socket {}: {error}",
                    self.path.display()
                ))
            })?;
            // 读超时必须设置：控制线程可能正忙于等块边界 ack（最长 5 s），没有超时会挂住 REST 线程。
            stream.set_read_timeout(Some(timeout)).map_err(|error| {
                TransportError::Io(format!("cannot set socket read timeout: {error}"))
            })?;
            stream.set_write_timeout(Some(timeout)).map_err(|error| {
                TransportError::Io(format!("cannot set socket write timeout: {error}"))
            })?;
            let writer_stream = stream.try_clone().map_err(|error| {
                TransportError::Io(format!("cannot clone control socket: {error}"))
            })?;
            let mut reader = BufReader::new(stream);
            let mut writer = BufWriter::new(writer_stream);
            protocol::exchange(&mut reader, &mut writer, request).map_err(classify_exchange_error)
        }
    }
}

/// 非 Unix 平台的传输：没有数据面，控制通道不可用（诚实报错，不假成功）。
///
/// Windows 上的 daemon 只是编译/测试桩：status 聚合会如实退回状态文件（mode=file/none），
/// 所有控制 op 返回 503 host_link_unavailable。
#[cfg(not(unix))]
#[derive(Debug)]
pub struct UnsupportedTransport;

#[cfg(not(unix))]
impl Transport for UnsupportedTransport {
    fn exchange(&self, _request: &Value, _timeout: Duration) -> Result<WireReply, TransportError> {
        Err(TransportError::Connect(
            "the rt-host control channel requires Unix domain sockets: this platform has no \
             control transport (the daemon binary here is a compile/test stub)"
                .to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;
    use crate::host_link::protocol::{ProtocolError, MAX_RESPONSE_BYTES};

    #[test]
    fn classifies_timeouts_separately_from_other_io_errors() {
        for kind in [io::ErrorKind::TimedOut, io::ErrorKind::WouldBlock] {
            let error = classify_exchange_error(ExchangeError::Io(io::Error::new(kind, "slow")));
            assert_eq!(error.kind(), "timeout", "{kind:?}");
            assert!(!error.definitely_not_delivered());
        }
        let error = classify_exchange_error(ExchangeError::Io(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "x",
        )));
        assert_eq!(error.kind(), "io");
        let error =
            classify_exchange_error(ExchangeError::Protocol(ProtocolError::ResponseTooLarge {
                bytes: MAX_RESPONSE_BYTES + 1,
                limit: MAX_RESPONSE_BYTES,
            }));
        assert_eq!(error.kind(), "protocol");
        assert!(!error.definitely_not_delivered());
        assert!(
            error.message().contains("line limit"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn only_connect_failures_are_definitely_not_delivered() {
        assert!(TransportError::Connect("x".into()).definitely_not_delivered());
        for error in [
            TransportError::Io("x".into()),
            TransportError::Timeout("x".into()),
            TransportError::Protocol("x".into()),
        ] {
            assert!(!error.definitely_not_delivered(), "{error}");
        }
        assert!(TransportError::Timeout("x".into())
            .to_string()
            .starts_with("timeout: "));
    }

    #[cfg(not(unix))]
    #[test]
    fn unsupported_platform_reports_a_stable_connect_error() {
        let config = crate::host_link::HostLinkConfig::rt_host("/run/khaen/rt-host.sock");
        let transport = for_config(&config);
        let error = transport
            .exchange(
                &serde_json::json!({ "op": "status" }),
                Duration::from_millis(10),
            )
            .expect_err("no transport on this platform");
        assert_eq!(error.kind(), "connect");
        assert!(error.definitely_not_delivered());
        assert!(
            error.message().contains("Unix domain sockets"),
            "{}",
            error.message()
        );
    }

    /// Unix 平台的生产传输自检：真实 UnixListener 上跑一行协议 + 缺 socket 时的连接错误。
    ///
    /// Windows 本机跑不到（cfg(unix)），由 CI 覆盖；这里写明语义供 CI 断言。
    #[cfg(unix)]
    #[test]
    fn unix_transport_round_trips_a_line_and_reports_missing_sockets() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = std::env::temp_dir().join(format!("khaen-host-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("rt-host.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind");
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut line = String::new();
            reader.read_line(&mut line).expect("read line");
            let request: Value = serde_json::from_str(line.trim_end()).expect("request json");
            assert_eq!(request["op"], serde_json::json!("status"));
            let mut writer = stream;
            writer
                .write_all(b"{\"ok\":true,\"runtime_state\":\"running\"}\n")
                .expect("write response");
            writer.flush().expect("flush");
        });

        let transport = unix::UnixTransport::new(path.clone());
        let reply = transport
            .exchange(
                &serde_json::json!({ "op": "status" }),
                Duration::from_secs(2),
            )
            .expect("roundtrip");
        assert!(matches!(reply, WireReply::Success(_)));
        server.join().expect("server thread");

        // socket 不存在：连接阶段就失败（请求肯定没送达），不是超时。
        std::fs::remove_file(&path).expect("remove socket");
        let error = transport
            .exchange(
                &serde_json::json!({ "op": "status" }),
                Duration::from_secs(2),
            )
            .expect_err("missing socket");
        assert_eq!(error.kind(), "connect");
        assert!(error.definitely_not_delivered());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
