//! 控制通道测试替身：脚本化内存假传输 + 真实 TCP 回环传输（Windows 可跑）。
//!
//! 生产传输是 `cfg(unix)` 的 Unix domain socket；这两个替身实现同一个
//! `khaen_daemon::host_link::Transport` trait，行协议/超时/重试语义与生产完全同一份
//! （都走 `host_link::protocol` 的编解码），所以 Windows 上也能端到端验证：
//! - [`FakeTransport`]：脚本化响应，逐个 op 断言“转发出去的是什么、返回给 REST 的是什么”；
//! - [`LoopbackServer`] + [`TcpTransport`]：真实 TCP socket 上跑一行请求/一行响应，
//!   覆盖 framing（半行、多行、挂断、超时）。
//!
//! 注意：这些替身只存在于测试代码，不会被编进 daemon 二进制。

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use khaen_daemon::host_link::protocol::{self, WireReply};
use khaen_daemon::host_link::{Transport, TransportError};
use serde_json::{json, Value};

/// 一份完整的 rt-host 控制 status 响应（形状与 `crates/rt-host/src/control/core.rs` 同源）。
///
/// 用 JSON 文本而不是 `json!` 宏：这个 fixture 被 `tests/support` 带进每个测试 crate，
/// 字面量嵌套会触发各 crate 的 `json_internal!` 递归上限（宏递归跟 crate 走，逐文件加
/// `recursion_limit` 只会污染别人的测试）。解析一次的开销可以忽略。
pub fn rt_status_document() -> Value {
    serde_json::from_str(
        r#"{
  "ok": true,
  "protocol_version": 1,
  "runtime_state": "running",
  "graph_id": "cafebabe0011",
  "epoch": 3,
  "blocks": 4321,
  "xruns": 2,
  "underruns": 1,
  "faults": 0,
  "silent": false,
  "mute": false,
  "safe_mode": false,
  "actual_version": 7,
  "mode": "aes67",
  "format": { "sample_rate": 48000, "block_size": 256, "channels": 24 },
  "control_format": { "sample_rate": 48000, "block_size": 256, "channels": 24 },
  "format_mismatch": false,
  "device_restart_required": false,
  "switch_pending": false,
  "plan_rate": 48000,
  "device_rate": 48000,
  "rate_asserted": true,
  "last_error": null,
  "output_errors": 5,
  "output_started": true,
  "input": {
    "packets": 111,
    "frames_filled": 2222,
    "lost": 0,
    "late": 0,
    "underruns": 2,
    "overruns": 0,
    "malformed": 0,
    "jitter_fill": 192,
    "ssrc": 534361700
  },
  "output": { "xruns": 2, "blocks_written": 1200, "bytes_written": 10137600 },
  "receiver": {
    "packets_received": 115,
    "bytes_received": 1000,
    "queue_full_drops": 1,
    "recv_errors": 0,
    "running": true
  },
  "device": {
    "state": "streaming",
    "plan_rate": 48000,
    "device_rate": 48000,
    "rate_asserted": true,
    "last_error": null,
    "attempts": 0,
    "since_ms": 0
  },
  "device_state": "present",
  "absent_since": null,
  "reopen_attempts": 0,
  "last_device_error": null,
  "wired": ["status", "mute"],
  "not_wired": [],
  "calibration": { "enabled": false },
  "surfaces": ["volume"],
  "surface_reasons": {},
  "age_ms": 42,
  "stale": false,
  "snapshot_interval_ms": 2,
  "reasons": {},
  "notes": {}
}"#,
    )
    .expect("rt-host status fixture must be valid JSON")
}

/// 一份完整的 rt-host channels_status 响应（M3b 热重配；形状与
/// `crates/rt-host/src/control/reconfigure.rs::ChannelsState::status_json` 同源）。
pub fn channels_status_document() -> Value {
    serde_json::from_str(
        r#"{
  "ok": true,
  "op": "channels_status",
  "generation": 2,
  "current": {
    "channels": 4,
    "sample_rate": 48000,
    "block_frames": 128,
    "aes67_group": "239.69.1.1",
    "aes67_port": 5004,
    "aes67_payload_type": 96,
    "aes67_channels": 2,
    "aes67_map": [0, 1],
    "jitter_frames": 192,
    "plan_rate": 48000,
    "device": "hw:CARD=UltraLite"
  },
  "format": { "block_frames": 128, "sample_rate": 48000, "channels": 4 },
  "device": {
    "name": "hw:CARD=UltraLite",
    "state": "present",
    "probe": { "available": true, "sample_rate": 48000, "channels": 4 }
  },
  "pending": null,
  "busy": false,
  "last_gap_ms": null,
  "last_result": null,
  "reasons": {}
}"#,
    )
    .expect("channels_status fixture must be valid JSON")
}

/// 一份完整的 rt-host channels_commit 成功响应（graph 未丢弃：只改 AES67 输入/参数）。
pub fn channels_commit_document() -> Value {
    serde_json::from_str(
        r#"{
  "ok": true,
  "op": "channels_commit",
  "applied": true,
  "changed": true,
  "idempotent": false,
  "generation": 3,
  "gap_ms": 412,
  "pid": 4242,
  "format": { "block_frames": 128, "sample_rate": 48000, "channels": 4 },
  "channels": {
    "channels": 4,
    "sample_rate": 48000,
    "block_frames": 128,
    "aes67_group": "239.69.1.1",
    "aes67_port": 5006,
    "aes67_payload_type": 96,
    "aes67_channels": 2,
    "aes67_map": [0, 1],
    "jitter_frames": 192,
    "plan_rate": 48000,
    "device": "hw:CARD=UltraLite"
  },
  "device_restarted": true,
  "pcm_reconfigured": true,
  "receiver_rebound": false,
  "engine_rebuilt": false,
  "graph_dropped": false,
  "device_was_absent": false,
  "hw": { "sample_rate": 48000, "channels": 4 },
  "alsa_stats_reset": true,
  "input_stats_reset": true,
  "rolled_back": false
}"#,
    )
    .expect("channels_commit fixture must be valid JSON")
}

/// 脚本化假传输：记录所有请求；脚本耗尽后按 op 返回与 rt-host ControlState 同形的成功响应。
#[derive(Debug, Default)]
pub struct FakeTransport {
    state: Mutex<FakeState>,
}

#[derive(Debug, Default)]
struct FakeState {
    script: VecDeque<Result<WireReply, TransportError>>,
    calls: Vec<Value>,
    status: Option<Value>,
    channels_status: Option<Value>,
}

impl FakeTransport {
    /// 空脚本：按 op 返回默认成功响应（status 用 `starting` 的诚实降级形状）。
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// status op 返回给定文档（其它 op 仍是默认成功响应）。
    pub fn with_rt_status(status: Value) -> Arc<Self> {
        let transport = Self::new();
        transport.state.lock().expect("fake transport").status = Some(status);
        transport
    }

    /// channels_status op 返回给定文档（其它 op 仍是默认成功响应）。
    pub fn with_channels_status(channels_status: Value) -> Arc<Self> {
        let transport = Self::new();
        transport
            .state
            .lock()
            .expect("fake transport")
            .channels_status = Some(channels_status);
        transport
    }

    /// 同时预设 status 与 channels_status 文档（/api/channels 的 GET 聚合用）。
    pub fn with_rt_status_and_channels(status: Value, channels_status: Value) -> Arc<Self> {
        let transport = Self::with_rt_status(status);
        transport
            .state
            .lock()
            .expect("fake transport")
            .channels_status = Some(channels_status);
        transport
    }

    /// 预设下一次调用返回完整的 ok=false 响应体（rt-host 回滚失败之类的多字段错误）。
    pub fn push_error_body(&self, body: Value) {
        let code = body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown_op")
            .to_string();
        let message = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        self.state
            .lock()
            .expect("fake transport")
            .script
            .push_back(Ok(WireReply::Failure {
                code,
                message,
                body,
            }));
    }

    /// 预设下一次（或之后若干次）调用的结果（成功后脚本耗尽回到默认响应）。
    pub fn push_reply(&self, reply: Value) {
        self.state
            .lock()
            .expect("fake transport")
            .script
            .push_back(Ok(WireReply::Success(reply)));
    }

    /// 预设下一次调用返回“服务端业务错误”（ok=false，带稳定码）：REST 必须按码表映射状态码且不重试。
    pub fn push_error_reply(&self, code: &str, message: &str) {
        self.state
            .lock()
            .expect("fake transport")
            .script
            .push_back(Ok(WireReply::Failure {
                code: code.to_string(),
                message: message.to_string(),
                body: json!({ "ok": false, "error": code, "message": message }),
            }));
    }

    /// 预设下一次调用返回传输层错误（超时/连接失败等，用于重试语义）。
    pub fn push_transport_error(&self, error: TransportError) {
        self.state
            .lock()
            .expect("fake transport")
            .script
            .push_back(Err(error));
    }

    /// 记录到的请求（按调用顺序）。
    pub fn calls(&self) -> Vec<Value> {
        self.state.lock().expect("fake transport").calls.clone()
    }

    /// 调用次数（重试断言用）。
    pub fn call_count(&self) -> usize {
        self.state.lock().expect("fake transport").calls.len()
    }
}

impl Transport for FakeTransport {
    fn exchange(&self, request: &Value, _timeout: Duration) -> Result<WireReply, TransportError> {
        let mut state = self.state.lock().expect("fake transport");
        state.calls.push(request.clone());
        if let Some(next) = state.script.pop_front() {
            return next;
        }
        let op = request.get("op").and_then(Value::as_str).unwrap_or("");
        Ok(default_reply(op, request, &state))
    }
}

/// 默认成功响应：每个 op 与 rt-host 的响应形状一致（字段顺序无关），值固定便于断言透传。
fn default_reply(op: &str, request: &Value, state: &FakeState) -> WireReply {
    let body = match op {
        "status" => state.status.clone().unwrap_or_else(|| {
            json!({
                "ok": true,
                "protocol_version": 1,
                "runtime_state": "starting",
                "age_ms": null,
                "format": { "sample_rate": 48_000, "block_size": 128, "channels": 2 },
                "reasons": {
                    "snapshot": "the block loop has not published its first runtime snapshot yet",
                },
            })
        }),
        "mute" => json!({
            "ok": true,
            "mute": request.get("mute").cloned().unwrap_or(json!(false)),
            "applied_block": 11,
            "idempotent": false,
        }),
        "prepare" => json!({
            "ok": true,
            "engine_token": "0123456789abcdef",
            "planned_version": 2,
            "graph_id": "graph-2",
            "admission": { "accepted": true, "budget": {} },
        }),
        "commit" => json!({
            "ok": true,
            "applied_block": 11,
            "version": 2,
            "all_ack": true,
            "graph_id": "graph-2",
        }),
        "abort" => json!({ "ok": true, "aborted": true }),
        "coeff" => json!({
            "ok": true,
            "version": 2,
            "last_good_version": 2,
            "graph_id": "graph-2",
            "applied_block": 11,
            "available_versions": [1, 2],
            "age_ms": 42,
            "reasons": {},
        }),
        "inspect" => json!({
            "ok": true,
            "staged": false,
            "sha256": "aabbccdd",
            "graph_id": "graph-2",
            "plan": {
                "block_size": 128,
                "sample_rate": 48_000,
                "in_channels": 2,
                "lanes": 2,
                "dac_channels": 2,
            },
            "admission": { "accepted": true, "rejections": [], "budget": {} },
            "compile": { "compiled": true, "latency_samples": 64 },
        }),
        "surface" => json!({
            "ok": true,
            "surface": request.get("surface").cloned().unwrap_or(json!("volume")),
            "engine_token": "fedcba9876543210",
            "planned_version": 3,
            "graph_id": "graph-3",
            "admission": { "accepted": true, "budget": {} },
        }),
        "rollback" => json!({
            "ok": true,
            "rolled_back_to": 1,
            "engine_token": "0011223344556677",
            "planned_version": 4,
            "graph_id": "graph-4",
            "admission": { "accepted": true, "budget": {} },
        }),
        "format" => json!({
            "ok": true,
            "engine_token": "8899aabbccddeeff",
            "planned_version": 5,
            "format": { "sample_rate": 48_000, "block_size": 128, "channels": 2 },
            "changed": true,
            "applies_to": "control-plane engine",
            "device_restart_required": true,
            "note": "fake transport: control-plane format only",
        }),
        // M3b 热重配（T61）：默认应答与 rt-host reconfigure 模块同形。
        "channels_status" => state
            .channels_status
            .clone()
            .unwrap_or_else(channels_status_document),
        "channels_prepare" => channels_prepare_reply(request),
        "channels_commit" => channels_commit_document(),
        "channels_abort" => json!({
            "ok": true,
            "op": "channels_abort",
            "aborted": true,
            "aborted_token": request.get("token").cloned().unwrap_or(Value::Null),
            "note": "fake transport: channels draft discarded",
        }),
        // T71 通道识别测试音（与 crates/rt-host/src/control/test_tone.rs 的回执同形）：
        // 把请求字段原样当回执（forwarded 字段可在 calls() 里逐字断言）。
        "test_tone" => json!({
            "ok": true,
            "dac": request.get("dac").cloned().unwrap_or(json!(0)),
            "ms": request.get("ms").cloned().unwrap_or(json!(1000)),
            "kind": request.get("kind").cloned().unwrap_or(json!("noise")),
            "freq_hz": request.get("freq_hz").cloned().unwrap_or(Value::Null),
            "level_db": request.get("db").cloned().unwrap_or(json!(-30.0)),
            "total_blocks": 8,
            "remaining_ms": request.get("ms").cloned().unwrap_or(json!(1000)),
        }),
        "test_tone_stop" => json!({
            "ok": true,
            "stopped": true,
            "was_active": true,
        }),
        other => {
            return WireReply::Failure {
                code: "unknown_op".to_string(),
                message: format!("fake transport has no canned reply for {other:?}"),
                body: json!({
                    "ok": false,
                    "error": "unknown_op",
                    "message": format!("fake transport has no canned reply for {other:?}"),
                }),
            }
        }
    };
    WireReply::Success(body)
}

/// channels_prepare 的默认成功回执：把请求字段原样当作目标（forwarded 字段可在 calls() 里逐字断言）。
fn channels_prepare_reply(request: &Value) -> Value {
    let current = channels_status_document()["current"].clone();
    let mut target = current.clone();
    if let Some(object) = target.as_object_mut() {
        if let Some(fields) = request.as_object() {
            for (key, value) in fields {
                if key == "op" {
                    continue;
                }
                object.insert(key.clone(), value.clone());
            }
        }
    }
    json!({
        "ok": true,
        "op": "channels_prepare",
        "token": "channels-token-1",
        "engine_token": "channels-token-1",
        "planned_generation": 3,
        "changed": true,
        "diff": { "aes67_port": { "from": 5004, "to": request.get("aes67_port").cloned().unwrap_or(json!(5004)) } },
        "current": current,
        "target": target,
        "format": { "block_frames": 128, "sample_rate": 48000, "channels": 4 },
        "device": {
            "name": "hw:CARD=UltraLite",
            "state": "present",
            "probe": { "available": true, "sample_rate": 48000, "channels": 4 }
        },
        "graph": {
            "action": "kept",
            "reason": "fake transport: engine format unchanged"
        },
        "admission": { "accepted": true },
        "applies_to": "fake transport",
    })
}

/// 一条回环连接的处理方式。
#[derive(Debug, Clone)]
pub enum LoopbackAction {
    /// 回一行 JSON 后关闭。
    Reply(Value),
    /// 直接关闭（不回应，模拟对端在响应前挂断）。
    Close,
    /// 等一会儿再回（模拟响应超时：客户端读超时先触发）。
    Delay(Duration, Value),
}

/// 真实 TCP 回环服务端：把接受的连接当作“rt-host 控制 socket”，跑同一条行协议。
pub struct LoopbackServer {
    addr: SocketAddr,
    connections: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl LoopbackServer {
    /// 启动回环服务端；`responder` 收到（第几条连接, 请求 JSON）并决定怎么回。
    pub fn start(
        mut responder: impl FnMut(u32, &Value) -> LoopbackAction + Send + 'static,
    ) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback listener");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let addr = listener.local_addr().expect("loopback addr");
        let connections = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_connections = Arc::clone(&connections);
        let thread_stop = Arc::clone(&stop);
        let join = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let index = thread_connections.fetch_add(1, Ordering::SeqCst) + 1;
                        handle_connection(stream, index, &mut responder);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        Self {
            addr,
            connections,
            stop,
            join: Some(join),
        }
    }

    /// 指向本回环服务端的传输。
    pub fn transport(&self) -> TcpTransport {
        TcpTransport { addr: self.addr }
    }

    /// 已接受的连接数（重试断言用）。
    pub fn connections(&self) -> u32 {
        self.connections.load(Ordering::SeqCst)
    }
}

impl Drop for LoopbackServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    index: u32,
    responder: &mut impl FnMut(u32, &Value) -> LoopbackAction,
) {
    // Windows 的 accept 会继承监听 socket 的非阻塞模式（Winsock 与 BSD 不同）：
    // 不清掉的话首个 read 可能立刻 WouldBlock，客户端看到 EOF 后重试，连接数偶发为 2。
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let cloned = match stream.try_clone() {
        Ok(cloned) => cloned,
        Err(_) => return,
    };
    let mut reader = BufReader::new(cloned);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let request: Value = serde_json::from_str(line.trim_end()).unwrap_or(Value::Null);
    match responder(index, &request) {
        LoopbackAction::Reply(reply) => write_line(&mut stream, &reply),
        LoopbackAction::Close => {}
        LoopbackAction::Delay(delay, reply) => {
            std::thread::sleep(delay);
            write_line(&mut stream, &reply);
        }
    }
}

fn write_line(stream: &mut TcpStream, reply: &Value) {
    let _ = stream.write_all(format!("{reply}\n").as_bytes());
    let _ = stream.flush();
}

/// 永远连不上的传输（socket 不存在/被拒）：请求肯定没送达（connect 错误）。
#[derive(Debug, Default)]
pub struct DownTransport;

impl Transport for DownTransport {
    fn exchange(&self, _request: &Value, _timeout: Duration) -> Result<WireReply, TransportError> {
        Err(TransportError::Connect(
            "rt-host control socket is not available in this test (connect refused)".to_string(),
        ))
    }
}

/// 真实 TCP 回环传输（与生产 Unix 传输共用 `protocol::exchange` 的编解码）。
#[derive(Debug, Clone)]
pub struct TcpTransport {
    addr: SocketAddr,
}

impl Transport for TcpTransport {
    fn exchange(&self, request: &Value, timeout: Duration) -> Result<WireReply, TransportError> {
        let stream = TcpStream::connect_timeout(&self.addr, timeout)
            .map_err(|error| TransportError::Connect(format!("connect: {error}")))?;
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|error| TransportError::Io(format!("set read timeout: {error}")))?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|error| TransportError::Io(format!("set write timeout: {error}")))?;
        let cloned = stream
            .try_clone()
            .map_err(|error| TransportError::Io(format!("clone stream: {error}")))?;
        let mut reader = BufReader::new(cloned);
        let mut writer = stream;
        protocol::exchange(&mut reader, &mut writer, request).map_err(classify)
    }
}

/// 与生产 Unix 传输同一份错误分类（超时的 ErrorKind → Timeout）。
fn classify(error: protocol::ExchangeError) -> TransportError {
    match error {
        protocol::ExchangeError::Io(error) => {
            let message = error.to_string();
            match error.kind() {
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
                    TransportError::Timeout(message)
                }
                _ => TransportError::Io(message),
            }
        }
        protocol::ExchangeError::Protocol(error) => TransportError::Protocol(error.to_string()),
    }
}
