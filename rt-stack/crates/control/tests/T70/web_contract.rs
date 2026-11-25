// T70 私有测试：真实 REST/WS Web 入口与页面（WBS T70）。
//
// 本文件由 crates/control/src/web.rs 的 #[cfg(test)] 子模块通过 include! 编译执行
// （见 tests/T70/README.md；不改 integrator 独占的 public_registration.rs）。
// 运行：cargo test -p khaen-control web
//
// 全部离线：真实 TCP loopback + T42 MemoryEngine + 测试内 SimCalibration 假后端
// （模拟 T60 JobState/JobStage 与 T66 apply/retest/rollback 回执）；不打开设备/不启服务。
//
// use super::* 由外层 #[cfg(test)] 模块提供（include! 共享作用域）。

use std::io::{Cursor, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use khaen_compat::ws;
use khaen_core::rt::RtStatus;

use crate::rest::calibration_contract::{
    CalAppliedState, CalApplyRequest, CalBassResults, CalLaneResult, CalLaneResults, CAL_LANES,
};
use crate::rest::{
    CalJobView, CalPortError, CalStartRequest, CalibrationPort, JsonValue, MemoryEngine,
    RestConfig, RestService, CAL_JOB_STATES, CAL_JOB_TERMINAL_STATES, RUNTIME_STATUS_SCHEMA,
};

const TEST_TOKEN: &str = "t70-web-token-0123456789abcdef";
const TEST_PASSWORD: &str = "t70 password for e2e";
const HEADER_TIMEOUT: Duration = Duration::from_secs(5);

fn rest_config() -> RestConfig {
    RestConfig {
        api_token: Some(TEST_TOKEN.to_string()),
        pbkdf2_iterations: 100,
        ..RestConfig::default()
    }
}

fn service() -> RestService {
    RestService::with_config(Box::new(MemoryEngine::new()), rest_config())
}

fn service_with_cal() -> RestService {
    RestService::with_config(Box::new(MemoryEngine::new()), rest_config())
        .with_calibration(Box::new(SimCalibration::new()))
}

fn test_web_config() -> WebConfig {
    WebConfig {
        bind_addr: "127.0.0.1:0".to_string(),
        max_connections: 16,
        io_timeout_secs: 5,
        ws_auth: WsAuthMode::RequireAuth,
    }
}

fn token_header() -> (&'static str, &'static str) {
    ("X-TSPI-Token", TEST_TOKEN)
}

fn json_body(response: &RawResponse) -> JsonValue {
    JsonValue::parse(&response.text()).expect("response body must be valid JSON")
}

fn error_code(response: &RawResponse) -> String {
    let body = json_body(response);
    match body.get("error").and_then(|error| error.get("code")) {
        Some(JsonValue::Str(code)) => code.clone(),
        other => panic!(
            "expected error envelope, got {other:?} / body={}",
            response.text()
        ),
    }
}

// ---------------------------------------------------------------------------
// T60/T66 假后端：SimCalibration
// ---------------------------------------------------------------------------

/// T60 FULL_PLAN 的 stage 机器名（crates/calibration/src/job.rs JobStage::as_str；
/// REST 层用 rest::CAL_JOB_STATES 校验；本测试断言两套名字一致 —— T60 接线词汇冻结）。
const T60_FULL_STAGES: [&str; 10] = [
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
];
/// T60 prepared stage 下标（FULL_PLAN 中第 7 个 stage；apply 单独命令，不随轮询推进）。
const SIM_PREPARED_INDEX: usize = 6;
/// 测试假后端从 queued 推进到 prepared 的最大轮询次数（每次 get/list 推进一步）。
const SIM_MAX_PROGRESS_POLLS: usize = 12;

#[derive(Debug, Clone)]
struct SimJob {
    id: String,
    kind: String,
    /// level/delay/bass：直接返回带 per-lane 结果的完成态（不推进 T60 stage）。
    stage_override: Option<String>,
    stage_index: Option<usize>,
    terminal: Option<String>,
    applied_version: Option<String>,
    last_good_version: Option<String>,
    failure: Option<String>,
    created: u64,
    updated: u64,
}

#[derive(Debug, Default)]
struct SimCalState {
    jobs: Vec<SimJob>,
    active: Option<usize>,
    next_id: u64,
}

/// 模拟 T60 JobManager 的最小状态机（queue -> 10 stage -> prepared -> apply/retest/rollback）。
/// 每次 get/list（REST 轮询）推进一个 stage，模拟 daemon 进度上报；apply 只在 prepared 成功后
/// 返回 applied_version（T66：actual ack 缺失即 500），rollback 只在有 last-good 时成功。
struct SimCalibration {
    state: std::sync::Mutex<SimCalState>,
}

impl SimCalibration {
    fn new() -> Self {
        SimCalibration {
            state: std::sync::Mutex::new(SimCalState::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SimCalState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn advance(state: &mut SimCalState) {
        let Some(index) = state.active else { return };
        let Some(job) = state.jobs.get_mut(index) else {
            return;
        };
        if job.terminal.is_some() {
            return;
        }
        job.stage_index = match job.stage_index {
            None => Some(0),
            // 只推进到 prepared；apply/remeasure/independent_holdout 由显式命令/后续 job 驱动。
            Some(current) if current < SIM_PREPARED_INDEX => Some(current + 1),
            Some(current) => Some(current),
        };
        job.updated += 1;
    }

    fn view(job: &SimJob) -> CalJobView {
        if let Some(stage) = &job.stage_override {
            return CalJobView {
                job_id: job.id.clone(),
                kind: job.kind.clone(),
                state: "complete".to_string(),
                stage: Some(stage.clone()),
                progress: 1.0,
                applied_version: job.applied_version.clone(),
                last_good_version: job.last_good_version.clone(),
                failure: job.failure.clone(),
                created_unix: job.created,
                updated_unix: job.updated,
            };
        }
        let (stage, state_name) = match (&job.terminal, job.stage_index) {
            (Some(terminal), _) => (None, terminal.clone()),
            (None, Some(index)) => (
                Some(T60_FULL_STAGES[index].to_string()),
                T60_FULL_STAGES[index].to_string(),
            ),
            (None, None) => (None, "queued".to_string()),
        };
        let progress = match (&job.terminal, job.stage_index) {
            (Some(_), _) => 1.0,
            (None, Some(index)) => (index as f64 + 1.0) / T60_FULL_STAGES.len() as f64,
            (None, None) => 0.0,
        };
        CalJobView {
            job_id: job.id.clone(),
            kind: job.kind.clone(),
            state: state_name,
            stage,
            progress,
            applied_version: job.applied_version.clone(),
            last_good_version: job.last_good_version.clone(),
            failure: job.failure.clone(),
            created_unix: job.created,
            updated_unix: job.updated,
        }
    }

    fn create(state: &mut SimCalState, kind: &str, stage_override: Option<String>) -> CalJobView {
        state.next_id += 1;
        let id = format!("job-{:04}", state.next_id);
        let job = SimJob {
            id,
            kind: kind.to_string(),
            stage_override,
            stage_index: None,
            terminal: None,
            applied_version: None,
            last_good_version: Some("cal-baseline".to_string()),
            failure: None,
            created: state.next_id,
            updated: state.next_id,
        };
        let view = SimCalibration::view(&job);
        state.jobs.push(job);
        state.active = Some(state.jobs.len() - 1);
        view
    }

    fn find(state: &SimCalState, job_id: &str) -> Option<usize> {
        state.jobs.iter().position(|job| job.id == job_id)
    }
}

impl SimCalibration {
    fn lanes_for(state: &SimCalState, job_id: &str) -> Option<CalLaneResults> {
        let job = state.jobs.iter().find(|job| job.id == job_id)?;
        let stage = job.stage_override.clone()?;
        let lanes = match stage.as_str() {
            "level" => CAL_LANES
                .iter()
                .enumerate()
                .map(|(index, lane)| {
                    CalLaneResult::level_locked(
                        lane,
                        -12.0 - index as f64 * 0.1,
                        27.5 - index as f64 * 0.5,
                        -2.0 + index as f64 * 0.4,
                    )
                })
                .collect(),
            "delay" => CAL_LANES
                .iter()
                .enumerate()
                .map(|(index, lane)| {
                    let relative = index as i64 * 8;
                    CalLaneResult::delay_locked(lane, 900 + relative, relative)
                })
                .collect(),
            "fir" => CAL_LANES
                .iter()
                .map(|lane| {
                    CalLaneResult::fir_locked(lane, "flat", 4096, "mixed", 0.45, 3.2, -1.0)
                })
                .collect(),
            "modal" => CAL_LANES
                .iter()
                .map(|lane| {
                    CalLaneResult::modal_locked(
                        lane,
                        vec![crate::rest::calibration_contract::ModalNotch {
                            freq_hz: 63.0,
                            gain_db: -6.0,
                            q: 4.0,
                        }],
                    )
                })
                .collect(),
            "bass" => Vec::new(),
            _ => return None,
        };
        let bass = if stage == "bass" {
            let mut bass_settings = crate::rest::calibration_contract::BassSettings::default();
            bass_settings.enabled = true;
            bass_settings.recompute_pre_gain();
            Some(CalBassResults::passed(&bass_settings))
        } else {
            None
        };
        Some(CalLaneResults {
            stage: stage.clone(),
            reference_lane: (stage == "bass").then(|| "FL".to_string()),
            target_dbfs: (stage == "level").then_some(-12.0),
            lanes,
            bass,
        })
    }
}

impl CalibrationPort for SimCalibration {
    fn start(&mut self, request: &CalStartRequest) -> Result<CalJobView, CalPortError> {
        let mut state = self.lock();
        if let Some(active) = state.active {
            if let Some(job) = state.jobs.get(active) {
                if job.stage_override.is_none() && job.terminal.is_none() {
                    return Err(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY));
                }
            }
        }
        let stage_override = request
            .stage
            .as_deref()
            .filter(|stage| matches!(*stage, "level" | "delay" | "bass" | "fir" | "modal"))
            .map(str::to_string);
        Ok(SimCalibration::create(
            &mut state,
            &request.kind,
            stage_override,
        ))
    }

    fn get(&mut self, job_id: &str) -> Option<CalJobView> {
        let mut state = self.lock();
        SimCalibration::advance(&mut state);
        let index = SimCalibration::find(&state, job_id)?;
        state.jobs.get(index).map(SimCalibration::view)
    }

    fn list(&mut self) -> Vec<CalJobView> {
        let mut state = self.lock();
        SimCalibration::advance(&mut state);
        state.jobs.iter().map(SimCalibration::view).collect()
    }

    fn cancel(&mut self, job_id: &str) -> Result<CalJobView, CalPortError> {
        let mut state = self.lock();
        let index = SimCalibration::find(&state, job_id).ok_or(CalPortError::NotFound)?;
        let job = state.jobs.get_mut(index).ok_or(CalPortError::NotFound)?;
        if job.terminal.is_some() {
            return Err(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY));
        }
        job.terminal = Some("cancelled".to_string());
        job.failure = Some("cancelled by user".to_string());
        let view = SimCalibration::view(job);
        state.active = None;
        Ok(view)
    }

    fn apply(&mut self, job_id: &str) -> Result<CalJobView, CalPortError> {
        let mut state = self.lock();
        let index = SimCalibration::find(&state, job_id).ok_or(CalPortError::NotFound)?;
        let job = state.jobs.get_mut(index).ok_or(CalPortError::NotFound)?;
        let prepared = job.stage_index == Some(SIM_PREPARED_INDEX);
        if !prepared || job.terminal.is_some() {
            return Err(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY));
        }
        // T66：apply 只在实际生效后返回 applied_version；状态为 APPLIED_NOT_CONFIRMED，
        // 需 retest 实测通过后才 CONFIRMED（回执字段由 REST 层校验非空）。
        let job_id = job.id.clone();
        job.terminal = Some("complete".to_string());
        job.applied_version = Some(format!("{job_id}-applied"));
        job.last_good_version = Some("cal-baseline".to_string());
        let view = SimCalibration::view(job);
        state.active = None;
        Ok(view)
    }

    fn retest(&mut self, job_id: &str) -> Result<CalJobView, CalPortError> {
        let mut state = self.lock();
        let index = SimCalibration::find(&state, job_id).ok_or(CalPortError::NotFound)?;
        let job = state.jobs.get(index).ok_or(CalPortError::NotFound)?;
        if job.applied_version.is_none() {
            return Err(CalPortError::Rejected(RtStatus::ERR_SWITCH_BUSY));
        }
        Ok(SimCalibration::create(&mut state, "retest", None))
    }

    fn lane_results(&mut self, job_id: &str) -> Option<CalLaneResults> {
        let state = self.lock();
        SimCalibration::lanes_for(&state, job_id)
    }

    fn apply_trims(
        &mut self,
        request: &CalApplyRequest,
    ) -> Result<CalAppliedState, CalPortError> {
        let mut applied = CalAppliedState::default();
        applied.level_trims = request.level_trims.clone();
        applied.delays = request.delays.clone();
        applied.bass = request.bass.clone();
        applied.applied_unix = 3;
        applied.source_job = request.job_id.clone();
        Ok(applied)
    }

    fn clear_trims(&mut self, _targets: &[String]) -> Result<CalAppliedState, CalPortError> {
        Ok(CalAppliedState {
            cleared_unix: 4,
            ..CalAppliedState::default()
        })
    }

    fn rollback(&mut self, job_id: &str) -> Result<CalJobView, CalPortError> {
        let mut state = self.lock();
        let index = SimCalibration::find(&state, job_id).ok_or(CalPortError::NotFound)?;
        let job = state.jobs.get_mut(index).ok_or(CalPortError::NotFound)?;
        if job.last_good_version.is_none() {
            return Err(CalPortError::Rejected(RtStatus::ERR_PLAN_STALE));
        }
        job.terminal = Some("rolled_back".to_string());
        job.failure = None;
        let view = SimCalibration::view(job);
        state.active = None;
        Ok(view)
    }
}

// ---------------------------------------------------------------------------
// 测试服务器 / 真实 TCP HTTP + WS 客户端
// ---------------------------------------------------------------------------

struct TestServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
    _server: Arc<WebServer>,
}

impl TestServer {
    fn start(rest: RestService) -> Self {
        TestServer::start_with(test_web_config(), rest)
    }

    fn start_with(config: WebConfig, rest: RestService) -> Self {
        let server = Arc::new(WebServer::bind_with(config, rest).expect("bind test server"));
        let addr = server.local_addr().expect("local addr");
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let server_thread = Arc::clone(&server);
        let join = thread::spawn(move || {
            let _ = server_thread.run_until(&stop_thread);
        });
        TestServer {
            addr,
            stop,
            join: Some(join),
            _server: server,
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // task-245：accept 循环改为阻塞/事件驱动，停止后必须 wake 打断 accept(2)；
        // 否则这里的 join 会挂死（旧非阻塞轮询实现不需要）。
        self._server.wake();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// 原始 HTTP 响应。
#[derive(Debug, Clone)]
struct RawResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl RawResponse {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn content_type(&self) -> String {
        self.header("Content-Type").unwrap_or_default().to_string()
    }
}

fn tcp_connect(addr: SocketAddr) -> TcpStream {
    let stream = TcpStream::connect(addr).expect("connect test server");
    stream
        .set_read_timeout(Some(HEADER_TIMEOUT))
        .expect("read timeout");
    stream
        .set_write_timeout(Some(HEADER_TIMEOUT))
        .expect("write timeout");
    stream
}

/// 读 HTTP 头（到 CRLFCRLF）；返回 (status, headers, 已读 body 前缀)。
fn read_http_head(stream: &mut TcpStream) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(position) = find_subslice(&buffer, b"\r\n\r\n") {
            break position + 4;
        }
        let read = stream.read(&mut chunk).expect("read response head");
        if read == 0 {
            panic!(
                "connection closed before response head: {:?}",
                String::from_utf8_lossy(&buffer)
            );
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let status_line = lines.next().expect("status line");
    let status = status_line
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("bad status line {status_line:?}"));
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    (status, headers, buffer[head_end..].to_vec())
}

/// 读完整 HTTP 响应（按 Content-Length）。
fn read_http_response(stream: &mut TcpStream) -> RawResponse {
    let (status, headers, mut body) = read_http_head(stream);
    let expected = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("Content-Length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut chunk = [0u8; 4096];
    while body.len() < expected {
        let read = stream.read(&mut chunk).expect("read response body");
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    RawResponse {
        status,
        headers,
        body,
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn write_http_request(
    stream: &mut TcpStream,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) {
    let mut request = String::new();
    request.push_str(method);
    request.push(' ');
    request.push_str(path);
    request.push_str(" HTTP/1.1\r\nHost: tspi-test\r\n");
    for (name, value) in headers {
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    if let Some(body) = body {
        request.push_str("Content-Type: application/json\r\nContent-Length: ");
        request.push_str(&body.len().to_string());
        request.push_str("\r\n");
    }
    request.push_str("Connection: close\r\n\r\n");
    if let Some(body) = body {
        request.push_str(body);
    }
    stream.write_all(request.as_bytes()).expect("write request");
    stream.flush().expect("flush request");
}

/// 一次性 HTTP 请求（新连接；Connection: close）。
fn http_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> RawResponse {
    let mut stream = tcp_connect(addr);
    write_http_request(&mut stream, method, path, headers, body);
    read_http_response(&mut stream)
}

fn http_authed(addr: SocketAddr, method: &str, path: &str, body: Option<&str>) -> RawResponse {
    http_request(addr, method, path, &[token_header()], body)
}

fn assert_status(response: &RawResponse, expected: u16) {
    assert_eq!(
        response.status,
        expected,
        "unexpected status; body={}",
        response.text()
    );
}

#[track_caller]
fn expect_json_str(value: &JsonValue, key: &str) -> String {
    match value.get(key) {
        Some(JsonValue::Str(text)) => text.clone(),
        other => panic!("field {key}: expected string, got {other:?}"),
    }
}

#[track_caller]
fn expect_json_f64(value: &JsonValue, key: &str) -> f64 {
    match value.get(key) {
        Some(JsonValue::Num(number)) => *number,
        other => panic!("field {key}: expected number, got {other:?}"),
    }
}

#[track_caller]
fn expect_json_bool(value: &JsonValue, key: &str) -> bool {
    match value.get(key) {
        Some(JsonValue::Bool(flag)) => *flag,
        other => panic!("field {key}: expected bool, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// WS 客户端（RFC 6455 原始帧；测试侧自己编码/解码，独立于被测实现）
// ---------------------------------------------------------------------------

const WS_TEST_KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";

struct WsClient {
    stream: TcpStream,
}

impl WsClient {
    fn connect(addr: SocketAddr, headers: &[(&str, &str)]) -> (RawResponse, Option<WsClient>) {
        let mut stream = tcp_connect(addr);
        let mut request = format!(
            "GET {} HTTP/1.1\r\nHost: tspi-test\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {}\r\n",
            WS_PATH, WS_TEST_KEY
        );
        for (name, value) in headers {
            request.push_str(name);
            request.push_str(": ");
            request.push_str(value);
            request.push_str("\r\n");
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes()).expect("write upgrade");
        stream.flush().expect("flush upgrade");
        let (status, headers, mut body) = read_http_head(&mut stream);
        if status == 101 {
            let response = RawResponse {
                status,
                headers,
                body: Vec::new(),
            };
            (response, Some(WsClient { stream }))
        } else {
            // 非 101：正常 HTTP 响应带 Content-Length/body（head 已读，继续读 body）。
            let expected = headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("Content-Length"))
                .and_then(|(_, value)| value.parse::<usize>().ok())
                .unwrap_or(0);
            let mut chunk = [0u8; 4096];
            while body.len() < expected {
                let read = stream.read(&mut chunk).expect("read ws error body");
                if read == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..read]);
            }
            let response = RawResponse {
                status,
                headers,
                body,
            };
            (response, None)
        }
    }

    fn send_text(&mut self, text: &str) {
        self.send_opcode(WS_OPCODE_TEXT, text.as_bytes());
    }

    fn send_opcode(&mut self, opcode: u8, payload: &[u8]) {
        let frame = encode_ws_client_frame(opcode, payload, [0x11, 0x22, 0x33, 0x44]);
        self.stream.write_all(&frame).expect("write ws frame");
        self.stream.flush().expect("flush ws frame");
    }

    fn read_frame(&mut self) -> (u8, Vec<u8>) {
        let mut header = [0u8; 2];
        self.stream
            .read_exact(&mut header)
            .expect("read ws frame head");
        let fin = header[0] & 0x80 != 0;
        assert!(fin, "server frames must be FIN=1");
        let opcode = header[0] & 0x0F;
        assert_eq!(header[1] & 0x80, 0, "server frames must be unmasked");
        let mut length = (header[1] & 0x7F) as usize;
        if length == 126 {
            let mut raw = [0u8; 2];
            self.stream.read_exact(&mut raw).expect("read ws len16");
            length = u16::from_be_bytes(raw) as usize;
        } else if length == 127 {
            let mut raw = [0u8; 8];
            self.stream.read_exact(&mut raw).expect("read ws len64");
            length = u64::from_be_bytes(raw) as usize;
        }
        let mut payload = vec![0u8; length];
        self.stream
            .read_exact(&mut payload)
            .expect("read ws payload");
        (opcode, payload)
    }

    fn read_text(&mut self) -> String {
        let (opcode, payload) = self.read_frame();
        assert_eq!(opcode, WS_OPCODE_TEXT, "expected text frame");
        String::from_utf8(payload).expect("text frame utf8")
    }

    fn exchange(&mut self, text: &str) -> String {
        self.send_text(text);
        self.read_text()
    }
}

fn ws_reply_value(raw: &str, command: &str) -> ws::JsonValue {
    let parsed =
        ws::json_parse(raw).unwrap_or_else(|error| panic!("ws reply not JSON ({error}): {raw}"));
    let entry = parsed
        .get(command)
        .unwrap_or_else(|| panic!("ws reply missing command key {command}: {raw}"));
    let result = entry
        .get("result")
        .unwrap_or_else(|| panic!("ws reply missing result: {raw}"));
    assert_eq!(
        result,
        &ws::JsonValue::String("Ok".to_string()),
        "ws reply result not Ok: {raw}"
    );
    entry
        .get("value")
        .unwrap_or_else(|| panic!("ws reply missing value: {raw}"))
        .clone()
}

fn ws_reply_error_code(raw: &str, command: &str) -> String {
    let parsed =
        ws::json_parse(raw).unwrap_or_else(|error| panic!("ws reply not JSON ({error}): {raw}"));
    let entry = parsed
        .get(command)
        .unwrap_or_else(|| panic!("ws reply missing command key {command}: {raw}"));
    let result = entry
        .get("result")
        .unwrap_or_else(|| panic!("ws reply missing result: {raw}"));
    match result.get("code") {
        Some(ws::JsonValue::String(code)) => code.clone(),
        other => panic!("ws error reply has no code: {other:?} / {raw}"),
    }
}

// ---------------------------------------------------------------------------
// A. 页面/资源（"真实页面"而非占位按钮或只 API）
// ---------------------------------------------------------------------------

#[test]
fn t70_web_page_exposes_real_interaction_actions() {
    for action in [
        "login",
        "setup",
        "logout",
        "refresh",
        "mode",
        "volume-apply",
        "mute-toggle",
        "routing-apply",
        "routing-reload",
        "format-apply",
        "cal-start-full",
        "cal-start-retest",
        "ws-connect",
        "ws-disconnect",
    ] {
        let needle = format!("data-action=\"{action}\"");
        assert!(
            INDEX_HTML.contains(&needle),
            "页面缺少真实交互按钮 {needle}（不能是占位按钮）"
        );
    }
    for action in ["cal-cancel", "cal-apply", "cal-retest", "cal-rollback"] {
        assert!(
            APP_JS.contains(&format!("\"{action}\"")),
            "app.js 缺少 job 行操作 {action}"
        );
    }
    for mode in ["off", "toslink", "target_multichannel", "aes67", "dante"] {
        let needle = format!("data-mode=\"{mode}\"");
        assert!(INDEX_HTML.contains(&needle), "缺少模式按钮 {needle}");
    }
    for id in [
        "st-runtime",
        "st-engine",
        "st-mode",
        "st-rate",
        "st-owner",
        "st-version",
        "st-epoch",
        "st-graph",
        "st-target-hash",
        "routing-body",
        "volume-slider",
        "fmt-actual",
        "cal-body",
        "ws-log",
    ] {
        let needle = format!("id=\"{id}\"");
        assert!(INDEX_HTML.contains(&needle), "页面缺少状态挂点 {needle}");
    }
}

#[test]
fn t70_web_page_assets_have_no_external_resources() {
    for (name, asset) in [
        ("index", INDEX_HTML),
        ("app.js", APP_JS),
        ("style.css", STYLE_CSS),
    ] {
        assert!(
            !asset.contains("http://"),
            "{name} 不得引用外部资源 http://"
        );
        assert!(
            !asset.contains("https://"),
            "{name} 不得引用外部资源 https://"
        );
        assert!(
            !asset.contains("onclick="),
            "{name} 不得使用 inline onclick"
        );
        assert!(!asset.contains("onload="), "{name} 不得使用 inline onload");
        assert!(
            !asset.contains("style="),
            "{name} 不得使用 inline style 属性"
        );
        assert!(!asset.contains("<style"), "{name} 不得内联 style 块");
    }
    assert!(INDEX_HTML.contains("<script src=\"/app.js\" defer>"));
    assert!(INDEX_HTML.contains("href=\"/style.css\""));
    assert!(INDEX_HTML.contains("href=\"/favicon.svg\""));
    assert!(!INDEX_HTML.contains("<script>"), "不得内联 script");
    assert!(!APP_JS.contains("eval("), "页面脚本不得使用 eval");
    assert!(
        !APP_JS.contains("document.write("),
        "页面脚本不得使用 document.write"
    );
    assert!(
        !APP_JS.contains("innerHTML"),
        "页面用 textContent 渲染（防注入）"
    );
}

#[test]
fn t70_web_app_js_drives_all_rest_and_ws_surfaces() {
    for endpoint in [
        "/api/auth/status",
        "/api/auth/setup",
        "/api/auth/login",
        "/api/auth/session",
        "/api/auth/logout",
        "/api/runtime/status",
        "/api/runtime/mode",
        "/api/runtime/volume",
        "/api/runtime/mute",
        "/api/runtime/target",
        "/api/runtime/prepare",
        "/api/runtime/commit",
        "/api/audio/format",
        "/api/calibration/jobs",
        "/ws",
    ] {
        assert!(APP_JS.contains(endpoint), "app.js 未驱动端点 {endpoint}");
    }
    for action in ["cancel", "apply", "retest", "rollback"] {
        assert!(
            APP_JS.contains(&format!("jobAction(\"{action}\"")),
            "app.js 未接校准操作 {action}"
        );
    }
    for command in [
        "GetVersion",
        "GetState",
        "GetVolume",
        "GetMute",
        "GetCaptureRate",
        "GetClippedSamples",
        "GetConfigVersion",
        "GetCalibrationJobs",
    ] {
        assert!(APP_JS.contains(command), "app.js 未使用 WS 命令 {command}");
    }
    assert!(APP_JS.contains("new WebSocket("), "页面必须实际建立 WS");
    assert!(APP_JS.contains("fetch("), "页面必须实际调用 REST");
    assert!(STYLE_CSS.contains("--accent"));
    assert!(STYLE_CSS.contains(".kv"));
}

#[test]
fn t70_web_assets_are_embedded_and_identifiable() {
    assert!(INDEX_HTML.starts_with("<!DOCTYPE html>"));
    assert!(INDEX_HTML.contains("lang=\"zh-CN\""));
    assert!(INDEX_HTML.contains("</html>"));
    assert!(
        APP_JS.starts_with("\"use strict\";"),
        "app.js head={:?}",
        APP_JS.chars().take(24).collect::<String>()
    );
    assert!(APP_JS.contains("tspi.token"));
    assert!(FAVICON_SVG.starts_with("<svg"));
    assert!(
        INDEX_HTML.len() > 6000,
        "index too small: {}",
        INDEX_HTML.len()
    );
    assert!(APP_JS.len() > 15000, "app.js too small: {}", APP_JS.len());
    assert!(
        STYLE_CSS.len() > 2000,
        "style too small: {}",
        STYLE_CSS.len()
    );
    let index_hash = hex_sha1(INDEX_HTML.as_bytes());
    let js_hash = hex_sha1(APP_JS.as_bytes());
    let css_hash = hex_sha1(STYLE_CSS.as_bytes());
    assert_eq!(index_hash.len(), 40);
    assert_ne!(index_hash, js_hash);
    assert_ne!(js_hash, css_hash);
    println!(
        "SAMPLE_ASSET index bytes={} sha1={}",
        INDEX_HTML.len(),
        index_hash
    );
    println!(
        "SAMPLE_ASSET app.js bytes={} sha1={}",
        APP_JS.len(),
        js_hash
    );
    println!(
        "SAMPLE_ASSET style.css bytes={} sha1={}",
        STYLE_CSS.len(),
        css_hash
    );
    assert_eq!(index_html(), INDEX_HTML);
    assert_eq!(app_js(), APP_JS);
    assert_eq!(style_css(), STYLE_CSS);
    assert_eq!(favicon_svg(), FAVICON_SVG);
}

/// SHA-1 hex（测试证据/哈希断言用；不用被测 base64/sha1 断言自身）。
fn hex_sha1(bytes: &[u8]) -> String {
    sha1(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn t70_web_static_routes_have_ui_security_headers() {
    let server = TestServer::start(service());
    for (path, content_type) in [
        ("/", "text/html"),
        ("/index.html", "text/html"),
        ("/style.css", "text/css"),
        ("/app.js", "text/javascript"),
        ("/favicon.svg", "image/svg+xml"),
        ("/favicon.ico", "image/svg+xml"),
    ] {
        let response = http_request(server.addr, "GET", path, &[], None);
        assert_status(&response, 200);
        assert!(
            response.content_type().starts_with(content_type),
            "{path} content-type={}",
            response.content_type()
        );
        assert_eq!(
            response.header("Content-Security-Policy"),
            Some(UI_CONTENT_SECURITY_POLICY)
        );
        assert_eq!(response.header("X-Frame-Options"), Some("DENY"));
        assert_eq!(response.header("X-Content-Type-Options"), Some("nosniff"));
        assert_eq!(response.header("Cache-Control"), Some("no-store"));
    }
    let html = http_request(server.addr, "GET", "/", &[], None).text();
    assert!(html.contains("TSPI"));
    assert!(html.contains("/app.js"));
    println!(
        "SAMPLE_STATIC index bytes={} head={:?}",
        html.len(),
        html.chars().take(120).collect::<String>()
    );
    let missing = http_request(server.addr, "GET", "/nope.txt", &[], None);
    assert_status(&missing, 404);
    assert_eq!(missing.content_type(), "text/plain; charset=utf-8");
    assert_eq!(missing.text(), "not found");
}

#[test]
fn t70_web_static_rejects_non_get_methods() {
    let server = TestServer::start(service());
    let response = http_request(server.addr, "POST", "/", &[], Some("{}"));
    assert_status(&response, 405);
    assert_eq!(response.header("Allow"), Some("GET, POST, PUT, DELETE"));
    assert_eq!(error_code(&response), "method_not_allowed");
    let head = http_request(server.addr, "HEAD", "/", &[], None);
    assert_status(&head, 405);
}

// ---------------------------------------------------------------------------
// B. HTTP 传输（解析/keep-alive/限额；E-5）
// ---------------------------------------------------------------------------

fn parse_request_bytes(raw: &[u8]) -> Result<Option<HttpRequest>, HttpError> {
    let mut cursor = Cursor::new(raw.to_vec());
    read_http_request(&mut cursor)
}

#[test]
fn t70_web_http_parses_request_line_headers_and_body() {
    let raw = b"POST /api/runtime/volume?x=1&y=%E4%B8%AD HTTP/1.1\r\nHost: h\r\nX-TSPI-Token: tok\r\nContent-Length: 10\r\n\r\n{\"db\": -3}";
    let request = parse_request_bytes(raw).expect("parse").expect("request");
    assert_eq!(request.method, Method::Post);
    assert_eq!(request.path, "/api/runtime/volume");
    assert_eq!(
        request.query,
        vec![
            ("x".to_string(), "1".to_string()),
            ("y".to_string(), "中".to_string())
        ]
    );
    assert_eq!(request.header_value("x-tspi-token"), Some("tok"));
    assert_eq!(request.body, b"{\"db\": -3}".to_vec());
    assert!(request.keep_alive);
    assert_eq!(request.version, "HTTP/1.1");
    let rest = request.to_rest_request();
    assert_eq!(rest.path, "/api/runtime/volume");
    assert_eq!(rest.body, b"{\"db\": -3}".to_vec());
}

#[test]
fn t70_web_http_parses_absolute_form_and_connection_close() {
    let raw = b"GET http://host.example/healthz?probe=1 HTTP/1.0\r\nConnection: close\r\n\r\n";
    let request = parse_request_bytes(raw).expect("parse").expect("request");
    assert_eq!(request.path, "/healthz");
    assert_eq!(request.query, vec![("probe".to_string(), "1".to_string())]);
    assert!(!request.keep_alive);
    assert_eq!(request.version, "HTTP/1.0");
    let raw = b"GET / HTTP/1.1\r\nConnection: keep-alive, close\r\n\r\n";
    let request = parse_request_bytes(raw).expect("parse").expect("request");
    assert!(!request.keep_alive);
}

#[test]
fn t70_web_http_rejects_malformed_requests() {
    let cases: Vec<(&[u8], u16, &str)> = vec![
        (b"HEAD / HTTP/1.1\r\n\r\n", 405, "method_not_allowed"),
        (b"GET / HTTP/2.0\r\n\r\n", 505, "invalid_request"),
        (
            b"GET relative/path HTTP/1.1\r\n\r\n",
            400,
            "invalid_request",
        ),
        (
            b"POST /api/runtime/volume HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n",
            400,
            "unsupported_value",
        ),
        (
            b"POST /api/runtime/volume HTTP/1.1\r\nContent-Length: abc\r\n\r\n",
            400,
            "invalid_request",
        ),
        (
            b"POST /api/runtime/volume HTTP/1.1\r\nContent-Length: 999999999999999999999\r\n\r\n",
            400,
            "invalid_request",
        ),
        (
            b"GET / HTTP/1.1\r\nContent-Length: 3000000\r\n\r\n",
            413,
            "payload_too_large",
        ),
        (b"GET / HTTP/1.1\r\nX-Bad\r\n\r\n", 400, "invalid_request"),
        (b"GET /%zz HTTP/1.1\r\n\r\n", 400, "invalid_request"),
        (
            b"GET / HTTP/1.1\r\nExpect: 100-continue\r\n\r\n",
            417,
            "unsupported_value",
        ),
    ];
    for (raw, status, code) in cases {
        let error = parse_request_bytes(raw).expect_err("must be rejected");
        assert_eq!(error.status, status, "raw={}", String::from_utf8_lossy(raw));
        assert_eq!(error.code, code, "raw={}", String::from_utf8_lossy(raw));
        let response = error.into_response();
        assert_eq!(response.status, status);
        if status == 405 {
            assert!(response.header_value("Allow").is_some());
        }
    }
}

#[test]
fn t70_web_http_eof_returns_none() {
    let mut cursor = Cursor::new(Vec::new());
    assert!(read_http_request(&mut cursor).expect("parse").is_none());
    // keep-alive 下客户端用 FIN 结束（无半截头）也是 None。
    let mut cursor = Cursor::new(b"GET /healthz HTTP/1.1\r\nHost: h\r\n\r\n".to_vec());
    assert!(read_http_request(&mut cursor).expect("parse").is_some());
    assert!(read_http_request(&mut cursor).expect("parse").is_none());
}

#[test]
fn t70_web_http_keep_alive_serves_multiple_requests() {
    let server = TestServer::start(service());
    let mut stream = tcp_connect(server.addr);
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: tspi-test\r\n\r\n")
        .expect("write");
    stream.flush().expect("flush");
    let first = read_http_response(&mut stream);
    assert_status(&first, 200);
    assert_eq!(first.header("Connection"), Some("keep-alive"));
    stream
        .write_all(b"GET /style.css HTTP/1.1\r\nHost: tspi-test\r\nConnection: close\r\n\r\n")
        .expect("write");
    stream.flush().expect("flush");
    let second = read_http_response(&mut stream);
    assert_status(&second, 200);
    assert_eq!(second.header("Connection"), Some("close"));
}

#[test]
fn t70_web_http_transport_errors_return_json_envelope() {
    let server = TestServer::start(service());
    let mut stream = tcp_connect(server.addr);
    stream
        .write_all(
            b"POST /api/runtime/volume HTTP/1.1\r\nHost: t\r\nTransfer-Encoding: chunked\r\n\r\n",
        )
        .expect("write");
    stream.flush().expect("flush");
    let response = read_http_response(&mut stream);
    assert_status(&response, 400);
    assert_eq!(error_code(&response), "unsupported_value");

    let mut stream = tcp_connect(server.addr);
    stream
        .write_all(
            b"POST /api/runtime/volume HTTP/1.1\r\nHost: t\r\nContent-Length: 3000000\r\n\r\n",
        )
        .expect("write");
    stream.flush().expect("flush");
    let response = read_http_response(&mut stream);
    assert_status(&response, 413);
    assert_eq!(error_code(&response), "payload_too_large");
}

#[test]
fn t70_web_http_connection_limit_returns_503() {
    let mut config = test_web_config();
    config.max_connections = 1;
    let server = TestServer::start_with(config, service());
    let mut held = tcp_connect(server.addr);
    held.write_all(b"GET /healthz HTTP/1.1\r\nHost: t\r\n\r\n")
        .expect("write");
    held.flush().expect("flush");
    let first = read_http_response(&mut held);
    assert_status(&first, 200);
    let rejected = http_request(server.addr, "GET", "/healthz", &[], None);
    assert_status(&rejected, 503);
    assert_eq!(error_code(&rejected), "service_unavailable");
}

#[test]
fn t70_web_run_until_blocks_idle_and_stop_with_wake_returns() {
    // task-245 回归：accept 循环必须事件驱动——空闲时阻塞在 accept(2)（不是非阻塞紧循环
    // 空转烧核），且「stop.store(true) + wake()」后必须及时退出，不能把 join 挂死。
    let server = Arc::new(
        WebServer::bind_with(test_web_config(), service()).expect("bind lifecycle server"),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let server_thread = Arc::clone(&server);
    let join = thread::spawn(move || server_thread.run_until(&stop_thread));
    // 无请求的空闲窗口：循环必须仍阻塞在该线程里（而不是退出/错误）。
    thread::sleep(Duration::from_millis(100));
    assert!(
        !join.is_finished(),
        "idle accept loop must stay blocked in accept(2), not exit"
    );
    // 停止契约：置位 + wake；3s 内必须干净退出（否则说明 accept 没被唤醒）。
    stop.store(true, Ordering::SeqCst);
    server.wake();
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let joined = join.join().is_ok();
        let _ = tx.send(joined);
    });
    let joined = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("run_until must return within 3s after stop+wake (blocking accept woken)");
    assert!(joined, "accept loop thread must exit cleanly after stop+wake");
}

// ---------------------------------------------------------------------------
// C. REST 传输接线（T42 语义经真实 HTTP）
// ---------------------------------------------------------------------------

#[test]
fn t70_web_api_public_healthz_and_auth_required() {
    let server = TestServer::start(service());
    let health = http_request(server.addr, "GET", "/healthz", &[], None);
    assert_status(&health, 200);
    assert_eq!(health.header("X-TSPI-API-Version"), Some("1"));
    let body = json_body(&health);
    assert_eq!(expect_json_str(&body, "status"), "ok");
    assert!(!expect_json_str(&body, "version").is_empty());
    assert!(!expect_json_bool(&body, "mock"));
    assert!(expect_json_f64(&body, "uptime_secs") >= 0.0);
    assert_eq!(
        health.header("Content-Security-Policy"),
        Some("default-src 'none'; frame-ancestors 'none'")
    );
    println!(
        "SAMPLE_HEALTHZ status={} body={}",
        health.status,
        health.text()
    );

    let anonymous = http_request(server.addr, "GET", "/api/runtime/status", &[], None);
    assert_status(&anonymous, 401);
    assert_eq!(error_code(&anonymous), "unauthorized");
    let wrong = http_request(
        server.addr,
        "GET",
        "/api/runtime/status",
        &[("X-TSPI-Token", "wrong-token")],
        None,
    );
    assert_status(&wrong, 401);
    assert_eq!(error_code(&wrong), "unauthorized");

    let status = http_authed(server.addr, "GET", "/api/runtime/status", None);
    assert_status(&status, 200);
    let body = json_body(&status);
    assert_eq!(expect_json_str(&body, "schema"), RUNTIME_STATUS_SCHEMA);
    assert_eq!(expect_json_str(&body, "runtime_state"), "Running");
    assert_eq!(expect_json_str(&body, "actual_version"), "v0001");
    assert_eq!(expect_json_f64(&body, "sample_rate"), 48_000.0);
    assert_eq!(expect_json_f64(&body, "chunksize"), 128.0);
    assert_eq!(expect_json_f64(&body, "volume_db"), -12.0);
    assert_eq!(expect_json_str(&body, "mode"), "toslink");
    let target = body.get("target").expect("target");
    assert_eq!(expect_json_str(target, "layout"), "4.1.2");
    assert_eq!(expect_json_str(&body, "actual_version"), "v0001");
    assert!(expect_json_str(&body, "graph_id").len() == 64);
    println!("SAMPLE_STATUS body={}", status.text());
}

#[test]
fn t70_web_api_unknown_paths_and_methods_are_json_errors() {
    let server = TestServer::start(service());
    let missing = http_authed(server.addr, "GET", "/api/does-not-exist", None);
    assert_status(&missing, 404);
    assert_eq!(error_code(&missing), "not_found");
    assert!(missing.content_type().starts_with("application/json"));

    let wrong_method = http_authed(server.addr, "DELETE", "/api/runtime/status", None);
    assert_status(&wrong_method, 405);
    assert_eq!(error_code(&wrong_method), "method_not_allowed");

    let invalid_json = http_authed(server.addr, "PUT", "/api/runtime/volume", Some("{bad"));
    assert_status(&invalid_json, 400);
    assert_eq!(error_code(&invalid_json), "invalid_json");

    let unknown_field = http_authed(
        server.addr,
        "PUT",
        "/api/runtime/volume",
        Some("{\"db\": -3, \"volume_db\": -3}"),
    );
    assert_status(&unknown_field, 400);
    assert_eq!(error_code(&unknown_field), "invalid_field");

    let bad_version = http_request(
        server.addr,
        "GET",
        "/api/runtime/status",
        &[token_header(), ("X-TSPI-API-Version", "nope")],
        None,
    );
    assert_status(&bad_version, 400);
    assert_eq!(error_code(&bad_version), "unsupported_api_version");
    assert!(
        !bad_version.text().contains(TEST_TOKEN),
        "错误响应不得泄露 token"
    );
}

#[test]
fn t70_web_api_volume_and_mute_round_trip_actual_ack() {
    let server = TestServer::start(service());
    let applied = http_authed(
        server.addr,
        "PUT",
        "/api/runtime/volume",
        Some("{\"db\": -6.5}"),
    );
    assert_status(&applied, 200);
    let body = json_body(&applied);
    assert_eq!(expect_json_f64(&body, "volume_db"), -6.5);
    assert_eq!(expect_json_f64(&body, "requested_volume_db"), -6.5);
    let readback = json_body(&http_authed(
        server.addr,
        "GET",
        "/api/runtime/volume",
        None,
    ));
    assert_eq!(expect_json_f64(&readback, "volume_db"), -6.5);
    println!("SAMPLE_VOLUME body={}", applied.text());

    let out_of_range = http_authed(
        server.addr,
        "PUT",
        "/api/runtime/volume",
        Some("{\"db\": 1000}"),
    );
    assert_status(&out_of_range, 400);
    assert_eq!(error_code(&out_of_range), "invalid_field");

    let muted = http_authed(
        server.addr,
        "PUT",
        "/api/runtime/mute",
        Some("{\"mute\": true}"),
    );
    assert_status(&muted, 200);
    let muted_body = json_body(&muted);
    assert!(expect_json_bool(&muted_body, "mute"));
    assert_eq!(
        expect_json_f64(&muted_body, "volume_db"),
        -6.5,
        "mute 不得改 volume"
    );
    let status = json_body(&http_authed(
        server.addr,
        "GET",
        "/api/runtime/status",
        None,
    ));
    assert!(expect_json_bool(&status, "mute"));
    assert_eq!(expect_json_f64(&status, "volume_db"), -6.5);
    let unmuted = http_authed(
        server.addr,
        "PUT",
        "/api/runtime/mute",
        Some("{\"mute\": false}"),
    );
    assert_status(&unmuted, 200);
    assert!(!expect_json_bool(&json_body(&unmuted), "mute"));
}

#[test]
fn t70_web_api_mode_switch_returns_202_then_actual() {
    let server = TestServer::start(service());
    let before = json_body(&http_authed(server.addr, "GET", "/api/mode", None));
    assert_eq!(expect_json_str(&before, "mode"), "toslink");
    assert!(before.get("profiles").is_some());
    let switched = http_authed(
        server.addr,
        "PUT",
        "/api/runtime/mode",
        Some("{\"mode\": \"target_multichannel\"}"),
    );
    assert_status(&switched, 202);
    let body = json_body(&switched);
    assert_eq!(expect_json_str(&body, "mode"), "target_multichannel");
    assert_eq!(expect_json_str(&body, "switch_state"), "idle");
    // F-28：MemoryEngine 未接 ModeController -> 回执显式 synthetic/非 actual。
    assert!(expect_json_bool(&body, "synthetic"));
    assert!(!expect_json_bool(&body, "mode_controller"));
    assert!(!expect_json_bool(&body, "mode_actual"));
    assert!(body.get("mode_evidence").is_some());
    let after = json_body(&http_authed(
        server.addr,
        "GET",
        "/api/runtime/status",
        None,
    ));
    assert_eq!(expect_json_str(&after, "mode"), "target_multichannel");
    let unknown = http_authed(
        server.addr,
        "PUT",
        "/api/runtime/mode",
        Some("{\"mode\": \"bogus\"}"),
    );
    assert_status(&unknown, 400);
    assert_eq!(error_code(&unknown), "unknown_mode");
}

#[test]
fn t70_web_api_audio_format_prepare_commit_actual() {
    let server = TestServer::start(service());
    let format = json_body(&http_authed(server.addr, "GET", "/api/audio/format", None));
    assert_eq!(expect_json_f64(&format, "sample_rate"), 48_000.0);
    let limits = format.get("limits").expect("limits");
    match limits.get("chunksizes") {
        Some(JsonValue::Arr(items)) => {
            let values: Vec<f64> = items
                .iter()
                .filter_map(|item| match item {
                    JsonValue::Num(number) => Some(*number),
                    _ => None,
                })
                .collect();
            assert_eq!(values, vec![64.0, 128.0]);
        }
        other => panic!("chunksizes: {other:?}"),
    }
    let applied = http_authed(
        server.addr,
        "PUT",
        "/api/audio/format",
        Some("{\"sample_rate\": 96000, \"chunksize\": 64}"),
    );
    assert_status(&applied, 200);
    let body = json_body(&applied);
    let outcome = body.get("outcome").expect("outcome");
    assert_eq!(expect_json_f64(outcome, "blocks_committed"), 1.0);
    let readback = json_body(&http_authed(server.addr, "GET", "/api/audio/format", None));
    assert_eq!(expect_json_f64(&readback, "sample_rate"), 96_000.0);
    assert_eq!(expect_json_f64(&readback, "chunksize"), 64.0);

    let rejected = http_authed(
        server.addr,
        "PUT",
        "/api/audio/format",
        Some("{\"chunksize\": 32}"),
    );
    assert_status(&rejected, 400);
    assert_eq!(error_code(&rejected), "unsupported_value");
    let unchanged = json_body(&http_authed(server.addr, "GET", "/api/audio/format", None));
    assert_eq!(
        expect_json_f64(&unchanged, "chunksize"),
        64.0,
        "拒绝后不得半生效"
    );
}

#[test]
fn t70_web_api_routing_prepare_commit_actual_ack() {
    let server = TestServer::start(service());
    let target = json_body(&http_authed(
        server.addr,
        "GET",
        "/api/runtime/target",
        None,
    ));
    let actual = target.get("target").expect("target");
    assert_eq!(expect_json_str(actual, "layout"), "4.1.2");
    match actual.get("roles") {
        Some(JsonValue::Arr(roles)) => {
            let names: Vec<String> = roles
                .iter()
                .filter_map(|entry| match entry.get("role") {
                    Some(JsonValue::Str(role)) => Some(role.clone()),
                    _ => None,
                })
                .collect();
            assert_eq!(names, vec!["FL", "FR", "SL", "SR", "HFL", "HFR", "SUB1"]);
            let dacs: Vec<f64> = roles
                .iter()
                .filter_map(|entry| match entry.get("dac") {
                    Some(JsonValue::Num(dac)) => Some(*dac),
                    _ => None,
                })
                .collect();
            assert_eq!(dacs, vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 7.0]);
        }
        other => panic!("roles: {other:?}"),
    }
    fn target_with(roles: &[(&str, u8)], channels: u16, profile: &str) -> String {
        let roles_json: Vec<String> = roles
            .iter()
            .map(|(role, dac)| format!("{{\"role\":\"{role}\",\"dac\":{dac}}}"))
            .collect();
        format!(
            "{{\"target\":{{\"layout\":\"4.1.2\",\"channels\":{channels},\"profile\":\"{profile}\",\"roles\":[{}],\"low_band_dacs\":[0,1,2,3,7]}}}}",
            roles_json.join(",")
        )
    }
    let p1_roles = [
        ("FL", 0),
        ("FR", 1),
        ("SL", 2),
        ("SR", 3),
        ("HFL", 4),
        ("HFR", 5),
        ("SUB1", 7),
    ];
    // F-14：4.1.2 只能与 P1_412_ROLES 逐项一致（SUB1 固定槽 7，ch6 空）；
    // 任何槽位改动 / 额外 SUB2 / 缺角色都必须稳定拒收，而不是「channels 足够就放行」。
    let mut sr6 = p1_roles;
    sr6[3] = ("SR", 6);
    let mut sub1_6 = p1_roles;
    sub1_6[6] = ("SUB1", 6);
    let mut sub1_9 = p1_roles;
    sub1_9[6] = ("SUB1", 9);
    let mut sub2_extra = p1_roles.to_vec();
    sub2_extra.push(("SUB2", 6));
    let missing_sub1 = &p1_roles[..6];
    let strict_cases: [(&str, String); 5] = [
        ("SR 3->6", target_with(&sr6, 8, "p1-412")),
        ("SUB1 7->6", target_with(&sub1_6, 8, "p1-412")),
        ("SUB1 7->9 ch10", target_with(&sub1_9, 10, "p1-412")),
        ("SUB2 extra", target_with(&sub2_extra, 8, "p1-412")),
        ("missing SUB1", target_with(missing_sub1, 8, "p1-412")),
    ];
    for (case, body) in strict_cases {
        let rejected = http_authed(server.addr, "PUT", "/api/runtime/target", Some(&body));
        assert_status(&rejected, 400);
        assert_eq!(
            error_code(&rejected),
            "layout_roles_mismatch",
            "case {case}: body={}",
            rejected.text()
        );
    }
    // 合法变更：仅 profile 变化（角色/槽位保持 P1 表）验证 prepare/commit 真实生效。
    let target_body = target_with(&p1_roles, 8, "p1-412-room-a");
    let put = http_authed(
        server.addr,
        "PUT",
        "/api/runtime/target",
        Some(&target_body),
    );
    assert_status(&put, 200);
    let prepared = http_authed(server.addr, "POST", "/api/runtime/prepare", Some("{}"));
    assert_status(&prepared, 200);
    let prepared_body = json_body(&prepared);
    println!("SAMPLE_PREPARE body={}", prepared.text());
    let prepare_id = expect_json_str(&prepared_body, "prepare_id");
    assert_eq!(expect_json_f64(&prepared_body, "expected_workers"), 4.0);
    let commit_body = format!("{{\"prepare_id\":\"{prepare_id}\"}}");
    let commit = http_authed(
        server.addr,
        "POST",
        "/api/runtime/commit",
        Some(&commit_body),
    );
    assert_status(&commit, 200);
    let committed = json_body(&commit);
    println!("SAMPLE_COMMIT body={}", commit.text());
    assert!(expect_json_bool(&committed, "committed"));
    // F-13：内存 engine 的 commit 回执必须显式 synthetic/actual=false。
    assert!(expect_json_bool(&committed, "synthetic"));
    assert!(!expect_json_bool(&committed, "actual"));
    assert_eq!(expect_json_str(&committed, "engine"), "memory");
    assert_eq!(expect_json_f64(&committed, "blocks_committed"), 1.0);
    assert_eq!(expect_json_f64(&committed, "ack_mask"), 15.0);
    assert_eq!(expect_json_f64(&committed, "plan_version"), 2.0);
    let graph_id = expect_json_str(&committed, "graph_id");
    assert_eq!(graph_id.len(), 64);
    assert!(graph_id
        .bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    let status = json_body(&http_authed(
        server.addr,
        "GET",
        "/api/runtime/status",
        None,
    ));
    assert_eq!(expect_json_str(&status, "actual_version"), "v0002");
    let applied_target = status.get("target").expect("target");
    assert_eq!(
        expect_json_str(applied_target, "profile"),
        "p1-412-room-a",
        "commit 后 actual target.profile 必须是新 profile"
    );
    let replay = http_authed(
        server.addr,
        "POST",
        "/api/runtime/commit",
        Some(&commit_body),
    );
    assert_status(&replay, 200);
    assert!(expect_json_bool(&json_body(&replay), "replayed"));

    let invalid = http_authed(
        server.addr,
        "PUT",
        "/api/runtime/target",
        Some("{\"target\":{\"layout\":\"4.1.2\",\"channels\":8,\"profile\":\"p1-412\",\"roles\":[{\"role\":\"SUB4\",\"dac\":6}],\"low_band_dacs\":[]}}"),
    );
    assert_status(&invalid, 400);
    assert_eq!(error_code(&invalid), "unknown_role");
}

#[test]
fn t70_web_api_commit_requires_matching_prepare() {
    let server = TestServer::start(service());
    let commit = http_authed(
        server.addr,
        "POST",
        "/api/runtime/commit",
        Some("{\"prepare_id\":\"prep-9999\"}"),
    );
    assert_status(&commit, 409);
    assert_eq!(error_code(&commit), "not_prepared");
    let prepared = http_authed(server.addr, "POST", "/api/runtime/prepare", Some("{}"));
    assert_status(&prepared, 200);
    let prepare_id = expect_json_str(&json_body(&prepared), "prepare_id");
    // 配置意图变化使 prepare stale。
    let changed = http_authed(
        server.addr,
        "PUT",
        "/api/runtime/config",
        Some("{\"chunksize\": 64}"),
    );
    assert_status(&changed, 200);
    let commit_body = format!("{{\"prepare_id\":\"{prepare_id}\"}}");
    let stale = http_authed(
        server.addr,
        "POST",
        "/api/runtime/commit",
        Some(&commit_body),
    );
    assert_status(&stale, 409);
    assert_eq!(error_code(&stale), "stale");
    let status = json_body(&http_authed(
        server.addr,
        "GET",
        "/api/runtime/status",
        None,
    ));
    assert_eq!(
        expect_json_str(&status, "actual_version"),
        "v0001",
        "未 commit 不得改变 actual"
    );
}

// ---------------------------------------------------------------------------
// D. 校准全交互（start/cancel/apply/retest/rollback；T60 状态机 + T66 回执语义）
// ---------------------------------------------------------------------------

fn cal_list(addr: SocketAddr) -> JsonValue {
    let response = http_authed(addr, "GET", "/api/calibration/jobs", None);
    assert_status(&response, 200);
    json_body(&response)
}

fn cal_job(addr: SocketAddr, job_id: &str) -> JsonValue {
    let path = format!("/api/calibration/jobs/{job_id}");
    let response = http_authed(addr, "GET", &path, None);
    assert_status(&response, 200);
    let body = json_body(&response);
    body.get("job").expect("job object").clone()
}

fn poll_job_until_prepared(addr: SocketAddr, job_id: &str) -> JsonValue {
    let mut last_state = String::new();
    for _ in 0..SIM_MAX_PROGRESS_POLLS {
        let job = cal_job(addr, job_id);
        last_state = expect_json_str(&job, "state");
        if last_state == "prepared" {
            return job;
        }
    }
    panic!("job {job_id} 未到 prepared，最后状态 {last_state}");
}

#[test]
fn t70_web_cal_without_port_is_503_not_fake() {
    let server = TestServer::start(service());
    let list = http_authed(server.addr, "GET", "/api/calibration/jobs", None);
    assert_status(&list, 503);
    assert_eq!(error_code(&list), "service_unavailable");
    let start = http_authed(
        server.addr,
        "POST",
        "/api/calibration/jobs",
        Some("{\"kind\":\"full\"}"),
    );
    assert_status(&start, 503);
    assert_eq!(error_code(&start), "service_unavailable");
}

#[test]
fn t70_web_cal_full_lifecycle_http() {
    let server = TestServer::start(service_with_cal());
    let started = http_authed(
        server.addr,
        "POST",
        "/api/calibration/jobs",
        Some("{\"kind\":\"full\",\"label\":\"T70 e2e\"}"),
    );
    assert_status(&started, 201);
    let started_body = json_body(&started);
    println!("SAMPLE_CAL_START body={}", started.text());
    assert_eq!(expect_json_str(&started_body, "job_id"), "job-0001");
    assert_eq!(expect_json_str(&started_body, "state"), "queued");

    let prepared = poll_job_until_prepared(server.addr, "job-0001");
    assert_eq!(expect_json_str(&prepared, "kind"), "full");
    assert_eq!(expect_json_str(&prepared, "stage"), "prepared");
    assert_eq!(expect_json_f64(&prepared, "progress"), 0.7);
    println!("SAMPLE_CAL_PREPARED body={}", prepared.to_json());

    let applied = http_authed(
        server.addr,
        "POST",
        "/api/calibration/jobs/job-0001/apply",
        Some("{}"),
    );
    assert_status(&applied, 200);
    let applied_body = json_body(&applied);
    println!("SAMPLE_CAL_APPLY body={}", applied.text());
    assert!(expect_json_bool(&applied_body, "ok"));
    assert_eq!(
        expect_json_str(&applied_body, "applied_version"),
        "job-0001-applied"
    );
    let applied_job = applied_body.get("job").expect("job");
    assert_eq!(expect_json_str(applied_job, "state"), "complete");

    let retest = http_authed(
        server.addr,
        "POST",
        "/api/calibration/jobs/job-0001/retest",
        Some("{}"),
    );
    assert_status(&retest, 201);
    let retest_body = json_body(&retest);
    println!("SAMPLE_CAL_RETEST body={}", retest.text());
    let retest_job = retest_body.get("job").expect("retest job");
    assert_eq!(expect_json_str(retest_job, "kind"), "retest");
    assert_eq!(expect_json_str(&retest_body, "job_id"), "job-0002");

    let rollback = http_authed(
        server.addr,
        "POST",
        "/api/calibration/jobs/job-0001/rollback",
        Some("{}"),
    );
    assert_status(&rollback, 200);
    let rollback_body = json_body(&rollback);
    println!("SAMPLE_CAL_ROLLBACK body={}", rollback.text());
    assert!(expect_json_bool(&rollback_body, "rolled_back"));
    assert_eq!(
        expect_json_str(&rollback_body, "last_good_version"),
        "cal-baseline"
    );
    let rolled = rollback_body.get("job").expect("job");
    assert_eq!(expect_json_str(rolled, "state"), "rolled_back");

    let list = cal_list(server.addr);
    assert_eq!(expect_json_f64(&list, "count"), 2.0);
    match list.get("jobs") {
        Some(JsonValue::Arr(jobs)) => assert_eq!(jobs.len(), 2),
        other => panic!("jobs: {other:?}"),
    }
}

#[test]
fn t70_web_cal_cancel_active_job() {
    let server = TestServer::start(service_with_cal());
    let started = http_authed(
        server.addr,
        "POST",
        "/api/calibration/jobs",
        Some("{\"kind\":\"full\"}"),
    );
    assert_status(&started, 201);
    let cancelled = http_authed(
        server.addr,
        "POST",
        "/api/calibration/jobs/job-0001/cancel",
        Some("{}"),
    );
    assert_status(&cancelled, 200);
    let body = json_body(&cancelled);
    assert!(expect_json_bool(&body, "cancelled"));
    let job = body.get("job").expect("job");
    assert_eq!(expect_json_str(job, "state"), "cancelled");
    let list = cal_list(server.addr);
    assert_eq!(expect_json_f64(&list, "count"), 1.0);
}

#[test]
fn t70_web_cal_apply_requires_prepared() {
    let server = TestServer::start(service_with_cal());
    let started = http_authed(
        server.addr,
        "POST",
        "/api/calibration/jobs",
        Some("{\"kind\":\"full\"}"),
    );
    assert_status(&started, 201);
    let apply = http_authed(
        server.addr,
        "POST",
        "/api/calibration/jobs/job-0001/apply",
        Some("{}"),
    );
    assert_status(&apply, 409);
    assert_eq!(error_code(&apply), "switch_busy");
    let retest = http_authed(
        server.addr,
        "POST",
        "/api/calibration/jobs/job-0001/retest",
        Some("{}"),
    );
    assert_status(&retest, 409);
}

#[test]
fn t70_web_cal_unknown_job_and_bad_start() {
    let server = TestServer::start(service_with_cal());
    let get = http_authed(server.addr, "GET", "/api/calibration/jobs/job-9999", None);
    assert_status(&get, 404);
    assert_eq!(error_code(&get), "not_found");
    let cancel = http_authed(
        server.addr,
        "POST",
        "/api/calibration/jobs/job-9999/cancel",
        Some("{}"),
    );
    assert_status(&cancel, 404);
    let bad_kind = http_authed(
        server.addr,
        "POST",
        "/api/calibration/jobs",
        Some("{\"kind\":\"bogus\"}"),
    );
    assert_status(&bad_kind, 400);
    assert_eq!(error_code(&bad_kind), "invalid_field");
}

#[test]
fn t70_web_cal_states_match_t60_stage_vocabulary() {
    // T42 REST 的 CAL_JOB_STATES 必须覆盖 T60 JobState/JobStage 冻结机器名（T60 接线词汇）。
    assert_eq!(CAL_JOB_STATES.len(), 15);
    assert_eq!(CAL_JOB_STATES[0], "queued");
    for stage in T60_FULL_STAGES {
        assert!(
            CAL_JOB_STATES.contains(&stage),
            "T42 REST 缺少 T60 stage 机器名 {stage}"
        );
    }
    for terminal in CAL_JOB_TERMINAL_STATES {
        assert!(CAL_JOB_STATES.contains(&terminal));
    }
    // T66 rollback 回执语义：REST rollback 必须返回 last_good_version；apply 必须返回 applied_version。
    // （由 t70_web_cal_full_lifecycle_http 实际断言；此处固定 T66 状态词汇供页面显示。）
    assert!(INDEX_HTML.contains("APPLIED_NOT_CONFIRMED"));
}

// ---------------------------------------------------------------------------
// E. WebSocket（T24 兼容子集 + T70 校准进度扩展；真实 TCP + RFC 6455 帧）
// ---------------------------------------------------------------------------

/// 客户端帧（可控 FIN）。
fn client_frame(fin: bool, opcode: u8, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut frame = encode_ws_client_frame(opcode, payload, mask);
    if !fin {
        frame[0] &= 0x7F;
    }
    frame
}

/// 解析服务端连续帧（测试侧独立解析）。
fn decode_server_frames(bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut frames = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        let byte0 = bytes[index];
        let byte1 = bytes[index + 1];
        assert_eq!(byte1 & 0x80, 0, "server frame must be unmasked");
        let opcode = byte0 & 0x0F;
        let mut length = (byte1 & 0x7F) as usize;
        let mut cursor = index + 2;
        if length == 126 {
            length = u16::from_be_bytes([bytes[cursor], bytes[cursor + 1]]) as usize;
            cursor += 2;
        } else if length == 127 {
            let mut raw = [0u8; 8];
            raw.copy_from_slice(&bytes[cursor..cursor + 8]);
            length = u64::from_be_bytes(raw) as usize;
            cursor += 8;
        }
        frames.push((opcode, bytes[cursor..cursor + length].to_vec()));
        index = cursor + length;
    }
    frames
}

fn reader_error_code(raw: Vec<u8>, max_frame_bytes: usize) -> u16 {
    let mut reader = WsFrameReader::new(Cursor::new(raw), max_frame_bytes);
    match reader.next_message() {
        Err(WsReadError::Protocol { code, reason }) => {
            assert!(!reason.is_empty());
            code
        }
        other => panic!("expected protocol error, got {other:?}"),
    }
}

#[test]
fn t70_web_ws_handshake_crypto_vectors() {
    assert_eq!(ws_accept_key(WS_TEST_KEY), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    assert_eq!(hex_sha1(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    assert_eq!(hex_sha1(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
    assert_eq!(base64_encode(b"ab"), "YWI=");
    assert_eq!(base64_encode(b"a"), "YQ==");
    assert_eq!(
        base64_decode("aGVsbG8=").as_deref(),
        Some(b"hello".as_slice())
    );
    assert_eq!(base64_decode("YWJj").as_deref(), Some(b"abc".as_slice()));
    assert_eq!(base64_decode("bad"), None);
    assert!(is_valid_ws_key(WS_TEST_KEY));
    assert!(!is_valid_ws_key("not-base64"));
    assert!(!is_valid_ws_key("dGhlIHNhbXBsZSBub25jZQ"));
    let response = ws_handshake_response("abc");
    assert_eq!(response.status, 101);
    assert_eq!(response.header_value("Upgrade"), Some("websocket"));
    assert_eq!(response.header_value("Sec-WebSocket-Accept"), Some("abc"));
}

#[test]
fn t70_web_ws_frame_codec_and_fragmentation() {
    let payload = b"hello world";
    let frame = encode_ws_client_frame(WS_OPCODE_TEXT, payload, [1, 2, 3, 4]);
    assert_eq!(frame[0], 0x81);
    assert_eq!(frame[1] & 0x80, 0x80);
    let mut reader = WsFrameReader::new(Cursor::new(frame), 1024);
    match reader.next_message().expect("decode") {
        Some(WsMessage::Text(bytes)) => assert_eq!(bytes, payload),
        other => panic!("unexpected {other:?}"),
    }
    assert!(reader.next_message().expect("eof").is_none());

    assert_eq!(encode_ws_text("hi"), vec![0x81, 0x02, b'h', b'i']);
    assert_eq!(encode_ws_pong(b"p"), vec![0x8A, 0x01, b'p']);
    let close_payload = [0x03u8, 0xE8, b'b', b'y', b'e'];
    let frame = encode_ws_client_frame(WS_OPCODE_CLOSE, &close_payload, [9, 9, 9, 9]);
    let mut reader = WsFrameReader::new(Cursor::new(frame), 1024);
    match reader.next_message().expect("close") {
        Some(WsMessage::Close(Some(1000), reason)) => assert_eq!(reason, "bye"),
        other => panic!("unexpected {other:?}"),
    }

    // 分片文本 + 交织 ping：ping 先回，续帧完成后整体文本。
    let mut input = Vec::new();
    input.extend(client_frame(false, WS_OPCODE_TEXT, b"hello ", [1, 1, 1, 1]));
    input.extend(client_frame(true, WS_OPCODE_PING, b"p", [2, 2, 2, 2]));
    input.extend(client_frame(
        true,
        WS_OPCODE_CONTINUATION,
        b"world",
        [3, 3, 3, 3],
    ));
    let mut reader = WsFrameReader::new(Cursor::new(input), 64);
    match reader.next_message().expect("ping first") {
        Some(WsMessage::Ping(payload)) => assert_eq!(payload, b"p"),
        other => panic!("unexpected {other:?}"),
    }
    match reader.next_message().expect("text") {
        Some(WsMessage::Text(bytes)) => assert_eq!(bytes, b"hello world"),
        other => panic!("unexpected {other:?}"),
    }
    assert!(reader.next_message().expect("eof").is_none());
}

#[test]
fn t70_web_ws_frame_reader_rejects_protocol_violations() {
    // 未 mask 的客户端帧 -> 1002。
    assert_eq!(reader_error_code(vec![0x81, 0x02, b'h', b'i'], 64), 1002);
    // RSV 位非零 -> 1002。
    let mut rsv_frame = encode_ws_client_frame(WS_OPCODE_TEXT, b"x", [1, 2, 3, 4]);
    rsv_frame[0] |= 0x40;
    assert_eq!(reader_error_code(rsv_frame, 64), 1002);
    // 超过 max_frame_bytes -> 1009。
    assert_eq!(
        reader_error_code(
            encode_ws_client_frame(WS_OPCODE_TEXT, b"0123456789abcdef", [1, 2, 3, 4]),
            8
        ),
        1009
    );
    // 非法操作码 -> 1002。
    assert_eq!(
        reader_error_code(encode_ws_client_frame(0x3, b"x", [1, 2, 3, 4]), 64),
        1002
    );
    // 无 pending 消息的 continuation -> 1002。
    assert_eq!(
        reader_error_code(
            encode_ws_client_frame(WS_OPCODE_CONTINUATION, b"x", [1, 2, 3, 4]),
            64
        ),
        1002
    );
}

#[test]
fn t70_web_ws_upgrade_requires_same_auth_as_rest() {
    let server = TestServer::start(service());
    let (anonymous, client) = WsClient::connect(server.addr, &[]);
    assert!(client.is_none());
    assert_status(&anonymous, 401);
    assert_eq!(error_code(&anonymous), "unauthorized");
    let (wrong, client) = WsClient::connect(server.addr, &[("X-TSPI-Token", "wrong")]);
    assert!(client.is_none());
    assert_status(&wrong, 401);
    assert_eq!(error_code(&wrong), "unauthorized");
}

#[test]
fn t70_web_ws_telemetry_and_volume_actual_ack() {
    let server = TestServer::start(service());
    let (handshake, client) = WsClient::connect(server.addr, &[token_header()]);
    assert_status(&handshake, 101);
    let expected_accept = ws_accept_key(WS_TEST_KEY);
    assert_eq!(
        handshake.header("Sec-WebSocket-Accept"),
        Some(expected_accept.as_str())
    );
    assert_eq!(handshake.header("Upgrade"), Some("websocket"));
    assert_eq!(handshake.header("Connection"), Some("Upgrade"));
    let mut ws = client.expect("ws client");

    assert_eq!(
        ws_reply_value(&ws.exchange("\"GetVersion\""), "GetVersion"),
        ws::JsonValue::String(ws::WS_PROTOCOL_VERSION.to_string())
    );
    assert_eq!(
        ws_reply_value(&ws.exchange("\"GetState\""), "GetState"),
        ws::JsonValue::String("Running".to_string())
    );
    assert_eq!(
        ws_reply_value(&ws.exchange("\"GetVolume\""), "GetVolume"),
        ws::JsonValue::Number(-12.0)
    );
    assert_eq!(
        ws_reply_value(&ws.exchange("{\"SetVolume\": -20.0}"), "SetVolume"),
        ws::JsonValue::Number(-20.0)
    );
    assert_eq!(
        ws_reply_value(&ws.exchange("\"GetVolume\""), "GetVolume"),
        ws::JsonValue::Number(-20.0)
    );
    assert_eq!(
        ws_reply_value(&ws.exchange("\"GetMute\""), "GetMute"),
        ws::JsonValue::Bool(false)
    );
    assert_eq!(
        ws_reply_value(&ws.exchange("{\"SetMute\": true}"), "SetMute"),
        ws::JsonValue::Bool(true)
    );
    assert_eq!(
        ws_reply_value(&ws.exchange("\"GetVolume\""), "GetVolume"),
        ws::JsonValue::Number(-20.0),
        "mute 不得改 volume"
    );
    assert_eq!(
        ws_reply_value(&ws.exchange("\"GetCaptureRate\""), "GetCaptureRate"),
        ws::JsonValue::Number(48_000.0)
    );
    assert_eq!(
        ws_reply_value(&ws.exchange("\"GetClippedSamples\""), "GetClippedSamples"),
        ws::JsonValue::Number(0.0)
    );
    assert_eq!(
        ws_reply_value(&ws.exchange("\"GetProcessingLoad\""), "GetProcessingLoad"),
        ws::JsonValue::Number(0.0)
    );
    assert_eq!(
        ws_reply_value(&ws.exchange("\"GetBufferLevel\""), "GetBufferLevel"),
        ws::JsonValue::Number(0.0)
    );
    // F-28：REST 桥没有唯一 clock servo 读数 -> GetRateAdjust 显式不可用，不得恒回 0.0。
    let rate_adjust = ws.exchange("\"GetRateAdjust\"");
    assert_eq!(
        ws_reply_error_code(&rate_adjust, "GetRateAdjust"),
        "unsupported",
        "reply={rate_adjust}"
    );
    let version = ws_reply_value(&ws.exchange("\"GetConfigVersion\""), "GetConfigVersion");
    assert_eq!(
        version.get("plan_version"),
        Some(&ws::JsonValue::Number(1.0))
    );
    match version.get("graph_id") {
        Some(ws::JsonValue::String(graph_id)) => {
            assert_eq!(graph_id.len(), 64);
            assert!(graph_id.bytes().all(|b| b.is_ascii_hexdigit()));
        }
        other => panic!("graph_id: {other:?}"),
    }
    println!("SAMPLE_WS_GETSTATE reply={}", ws.exchange("\"GetState\""));
    println!("SAMPLE_WS_GETVOLUME reply={}", ws.exchange("\"GetVolume\""));
    let config_doc = ws_reply_value(&ws.exchange("\"GetConfig\""), "GetConfig");
    match config_doc {
        ws::JsonValue::String(document) => {
            assert!(document.contains("tspi-runtime-config/v1"));
        }
        other => panic!("GetConfig must be a string: {other:?}"),
    }
}

#[test]
fn t70_web_ws_config_prepare_commit_abort_actual_ack() {
    let server = TestServer::start(service());
    let config_doc = http_authed(server.addr, "GET", "/api/runtime/config", None).text();
    let (handshake, client) = WsClient::connect(server.addr, &[token_header()]);
    assert_status(&handshake, 101);
    let mut ws = client.expect("ws client");

    let request = ws::json_object(vec![(
        "SetConfigJson",
        ws::JsonValue::String(config_doc.clone()),
    )]);
    let request_text = ws::json_encode(&request).expect("encode");
    println!("SAMPLE_WS_SETCONFIG json={request_text}");
    let committed = ws_reply_value(&ws.exchange(&request_text), "SetConfigJson");
    println!("SAMPLE_WS_SETCONFIG reply={:?}", committed);
    assert_eq!(committed.get("applied"), Some(&ws::JsonValue::Bool(true)));
    assert_eq!(
        committed.get("plan_version"),
        Some(&ws::JsonValue::Number(2.0))
    );
    assert_eq!(
        committed.get("blocks_committed"),
        Some(&ws::JsonValue::Number(1.0))
    );
    let status = http_authed(server.addr, "GET", "/api/runtime/status", None);
    let status_body = json_body(&status);
    assert_eq!(expect_json_str(&status_body, "actual_version"), "v0002");

    let request = ws::json_object(vec![(
        "PrepareConfigJson",
        ws::JsonValue::String(config_doc),
    )]);
    let request_text = ws::json_encode(&request).expect("encode");
    let prepared = ws_reply_value(&ws.exchange(&request_text), "PrepareConfigJson");
    assert_eq!(prepared.get("prepared"), Some(&ws::JsonValue::Bool(true)));
    assert_eq!(
        prepared.get("plan_version"),
        Some(&ws::JsonValue::Number(3.0))
    );
    let aborted = ws_reply_value(&ws.exchange("\"AbortConfig\""), "AbortConfig");
    assert_eq!(aborted.get("aborted"), Some(&ws::JsonValue::Bool(true)));
    assert_eq!(
        aborted.get("plan_version"),
        Some(&ws::JsonValue::Number(3.0))
    );
    let request = ws::json_object(vec![("CommitConfig", ws::JsonValue::Object(Vec::new()))]);
    let request_text = ws::json_encode(&request).expect("encode");
    let raw = ws.exchange(&request_text);
    assert_eq!(ws_reply_error_code(&raw, "CommitConfig"), "not_prepared");
    let status = json_body(&http_authed(
        server.addr,
        "GET",
        "/api/runtime/status",
        None,
    ));
    assert_eq!(
        expect_json_str(&status, "actual_version"),
        "v0002",
        "abort/未 commit 不得改变 actual"
    );
}

#[test]
fn t70_web_ws_unknown_command_ping_binary_close() {
    let server = TestServer::start(service());
    let (handshake, client) = WsClient::connect(server.addr, &[token_header()]);
    assert_status(&handshake, 101);
    let mut ws = client.expect("ws client");

    let raw = ws.exchange("\"GetNope\"");
    assert_eq!(ws_reply_error_code(&raw, "GetNope"), "unknown_command");

    ws.send_opcode(WS_OPCODE_PING, b"hb");
    let (opcode, payload) = ws.read_frame();
    assert_eq!(opcode, WS_OPCODE_PONG);
    assert_eq!(payload, b"hb");

    ws.send_opcode(WS_OPCODE_BINARY, b"\x00\x01");
    let raw = ws.read_text();
    let parsed = ws::json_parse(&raw).expect("error json");
    match parsed.get("error").and_then(|error| error.get("code")) {
        Some(ws::JsonValue::String(code)) => assert_eq!(code, "invalid_frame_type"),
        other => panic!("binary error code: {other:?} / {raw}"),
    }

    ws.send_opcode(WS_OPCODE_CLOSE, &[0x03, 0xE8, b'b', b'y', b'e']);
    let (opcode, payload) = ws.read_frame();
    assert_eq!(opcode, WS_OPCODE_CLOSE);
    assert!(payload.len() >= 2);
    assert_eq!(u16::from_be_bytes([payload[0], payload[1]]), 1000);
}

#[test]
fn t70_web_ws_calibration_progress_extension() {
    let server = TestServer::start(service_with_cal());
    let started = http_authed(
        server.addr,
        "POST",
        "/api/calibration/jobs",
        Some("{\"kind\":\"full\"}"),
    );
    assert_status(&started, 201);
    let (handshake, client) = WsClient::connect(server.addr, &[token_header()]);
    assert_status(&handshake, 101);
    let mut ws = client.expect("ws client");

    let raw = ws.exchange("\"GetCalibrationJobs\"");
    println!("SAMPLE_WS_CAL_JOBS reply={raw}");
    let value = ws_reply_value(&raw, "GetCalibrationJobs");
    assert_eq!(value.get("count"), Some(&ws::JsonValue::Number(1.0)));
    match value.get("jobs") {
        Some(ws::JsonValue::Array(jobs)) => {
            assert_eq!(jobs.len(), 1);
            assert_eq!(
                jobs[0].get("job_id"),
                Some(&ws::JsonValue::String("job-0001".to_string()))
            );
        }
        other => panic!("jobs: {other:?}"),
    }
    let request = ws::json_object(vec![(
        "GetCalibrationJob",
        ws::JsonValue::String("job-0001".to_string()),
    )]);
    let request_text = ws::json_encode(&request).expect("encode");
    let value = ws_reply_value(&ws.exchange(&request_text), "GetCalibrationJob");
    match value.get("job") {
        Some(job) => assert_eq!(
            job.get("job_id"),
            Some(&ws::JsonValue::String("job-0001".to_string()))
        ),
        other => panic!("job: {other:?}"),
    }
    let request = ws::json_object(vec![(
        "GetCalibrationJob",
        ws::JsonValue::String("job-9999".to_string()),
    )]);
    let request_text = ws::json_encode(&request).expect("encode");
    let raw = ws.exchange(&request_text);
    assert_eq!(
        ws_reply_error_code(&raw, "GetCalibrationJob"),
        "invalid_argument"
    );
    let request = ws::json_object(vec![("GetCalibrationJob", ws::JsonValue::UInt(42))]);
    let request_text = ws::json_encode(&request).expect("encode");
    let raw = ws.exchange(&request_text);
    assert_eq!(
        ws_reply_error_code(&raw, "GetCalibrationJob"),
        "invalid_argument"
    );
}

#[test]
fn t70_web_ws_session_cookie_auth_csrf_write() {
    let server = TestServer::start(service());
    let setup_body =
        format!("{{\"password\":\"{TEST_PASSWORD}\",\"confirm\":\"{TEST_PASSWORD}\"}}");
    let setup = http_request(
        server.addr,
        "POST",
        "/api/auth/setup",
        &[],
        Some(&setup_body),
    );
    assert_status(&setup, 200);
    let csrf = expect_json_str(&json_body(&setup), "csrf_token");
    assert!(!csrf.is_empty());
    let set_cookie = setup.header("Set-Cookie").expect("Set-Cookie").to_string();
    let cookie = set_cookie
        .split(';')
        .next()
        .expect("cookie pair")
        .to_string();
    assert!(cookie.starts_with("khaen_session="));

    let (handshake, client) = WsClient::connect(server.addr, &[("Cookie", cookie.as_str())]);
    assert_status(&handshake, 101);
    let mut ws = client.expect("ws client");
    // 写命令需要 CSRF：浏览器无法在 WS 升级头里设置，Web 层用服务端会话补 CSRF（E-4）。
    let value = ws_reply_value(&ws.exchange("{\"SetVolume\": -33.0}"), "SetVolume");
    assert_eq!(value, ws::JsonValue::Number(-33.0));
    assert_eq!(
        ws_reply_value(&ws.exchange("\"GetVolume\""), "GetVolume"),
        ws::JsonValue::Number(-33.0)
    );
    assert_eq!(
        ws_reply_value(&ws.exchange("\"GetState\""), "GetState"),
        ws::JsonValue::String("Running".to_string())
    );
    // 未认证升级：伪造/已失效 Cookie -> 401。
    let (unauthorized, client) =
        WsClient::connect(server.addr, &[("Cookie", "khaen_session=deadbeef")]);
    assert!(client.is_none());
    assert_status(&unauthorized, 401);
    let _ = csrf;
}

#[test]
fn t70_web_ws_in_memory_transport_drives_same_rest_core() {
    let rest = Arc::new(service());
    let auth_headers = vec![("X-TSPI-Token".to_string(), TEST_TOKEN.to_string())];
    let mut plane = RestWsPlane::new(Arc::clone(&rest), auth_headers.clone());
    let context = WebWsContext::new(Arc::clone(&rest), auth_headers);
    let mut input = Vec::new();
    input.extend(client_frame(
        true,
        WS_OPCODE_TEXT,
        b"\"GetState\"",
        [1, 2, 3, 4],
    ));
    input.extend(client_frame(
        true,
        WS_OPCODE_TEXT,
        b"\"GetVersion\"",
        [5, 6, 7, 8],
    ));
    input.extend(client_frame(
        true,
        WS_OPCODE_TEXT,
        b"{\"GetCalibrationJobs\":null}",
        [9, 9, 9, 9],
    ));
    let mut reader = std::io::BufReader::new(Cursor::new(input));
    let mut output: Vec<u8> = Vec::new();
    run_ws_connection(&mut reader, &mut output, &mut plane, &context).expect("ws transport");
    let frames = decode_server_frames(&output);
    assert_eq!(frames.len(), 3, "frames={frames:?}");
    let first = String::from_utf8(frames[0].1.clone()).expect("utf8");
    assert_eq!(
        ws_reply_value(&first, "GetState"),
        ws::JsonValue::String("Running".to_string())
    );
    let second = String::from_utf8(frames[1].1.clone()).expect("utf8");
    assert_eq!(
        ws_reply_value(&second, "GetVersion"),
        ws::JsonValue::String(ws::WS_PROTOCOL_VERSION.to_string())
    );
    // 无 T60 port：校准扩展命令如实报错（不伪造 job 列表）。
    let third = String::from_utf8(frames[2].1.clone()).expect("utf8");
    assert_eq!(
        ws_reply_error_code(&third, "GetCalibrationJobs"),
        "unsupported"
    );
}

// ---------------------------------------------------------------------------
// F. 横切契约（CSP 双口径 / 扩展命令 / schema）
// ---------------------------------------------------------------------------

#[test]
fn t70_web_csp_is_split_between_ui_and_api() {
    let server = TestServer::start(service());
    let page = http_request(server.addr, "GET", "/", &[], None);
    assert_eq!(
        page.header("Content-Security-Policy"),
        Some(UI_CONTENT_SECURITY_POLICY)
    );
    assert!(UI_CONTENT_SECURITY_POLICY.contains("script-src 'self'"));
    assert!(UI_CONTENT_SECURITY_POLICY.contains("connect-src 'self'"));
    assert!(!UI_CONTENT_SECURITY_POLICY.contains("unsafe-inline"));
    assert!(!UI_CONTENT_SECURITY_POLICY.contains("unsafe-eval"));
    assert!(!UI_CONTENT_SECURITY_POLICY.contains("http:"));
    let api = http_request(server.addr, "GET", "/healthz", &[], None);
    assert_eq!(
        api.header("Content-Security-Policy"),
        Some("default-src 'none'; frame-ancestors 'none'")
    );
}

#[test]
fn t70_web_ws_extension_command_contract() {
    assert_eq!(WS_CAL_COMMANDS, ["GetCalibrationJobs", "GetCalibrationJob"]);
    assert_eq!(WS_CAL_SCHEMA, "tspi-ws-cal/v1");
    for command in WS_CAL_COMMANDS {
        assert!(
            !ws::SUPPORTED_COMMANDS.contains(&command),
            "{command} 是 T70 扩展命令，不得冒充 T24 冻结命令集"
        );
        assert!(APP_JS.contains(command));
    }
    // T24 冻结命令集仍在页面/WS 实际使用。
    for command in [
        "GetVersion",
        "GetState",
        "GetVolume",
        "GetMute",
        "GetConfigVersion",
    ] {
        assert!(ws::SUPPORTED_COMMANDS.contains(&command));
    }
}

#[test]
fn t70_web_status_schema_and_limits_are_contract_bound() {
    let server = TestServer::start(service());
    let status = json_body(&http_authed(
        server.addr,
        "GET",
        "/api/runtime/status",
        None,
    ));
    assert_eq!(expect_json_str(&status, "schema"), RUNTIME_STATUS_SCHEMA);
    assert_eq!(expect_json_f64(&status, "api_version"), 1.0);
    for field in [
        "runtime_state",
        "device_owner",
        "epoch",
        "actual_version",
        "graph_id",
        "target",
        "target_hash",
        "mode",
        "sample_rate",
        "chunksize",
        "volume_db",
        "mute",
        "xruns",
        "deadline_misses",
        "clipped_samples",
        "calibration_owner",
    ] {
        assert!(status.get(field).is_some(), "status 缺少字段 {field}");
    }
    let format = json_body(&http_authed(server.addr, "GET", "/api/audio/format", None));
    match format
        .get("limits")
        .and_then(|limits| limits.get("chunksizes"))
    {
        Some(JsonValue::Arr(items)) => {
            assert_eq!(items.len(), 2, "runtime 仅 64/128 chunksize");
        }
        other => panic!("chunksizes: {other:?}"),
    }
    let input_mode = json_body(&http_authed(
        server.addr,
        "GET",
        "/api/channelmap/input-mode",
        None,
    ));
    assert_eq!(expect_json_str(&input_mode, "status"), "not_supported");
    assert_eq!(
        expect_json_str(&input_mode, "reason"),
        "p1-no-content-upmix"
    );
}

#[test]
fn t70_web_synthetic_ack_is_not_rendered_as_actual() {
    // F-13：页面必须按 synthetic 分支显示，不得把内存 engine 的 commit 回执写成已生效。
    let server = TestServer::start(service());
    let status = json_body(&http_authed(
        server.addr,
        "GET",
        "/api/runtime/status",
        None,
    ));
    assert!(expect_json_bool(&status, "synthetic"));
    assert!(!expect_json_bool(&status, "actual"));
    assert_eq!(expect_json_str(&status, "engine"), "memory");
    let prepared = json_body(&http_authed(
        server.addr,
        "POST",
        "/api/runtime/prepare",
        Some("{}"),
    ));
    let prepare_id = expect_json_str(&prepared, "prepare_id");
    let commit = json_body(&http_authed(
        server.addr,
        "POST",
        "/api/runtime/commit",
        Some(&format!("{{\"prepare_id\":\"{prepare_id}\"}}")),
    ));
    assert!(expect_json_bool(&commit, "synthetic"));
    assert!(!expect_json_bool(&commit, "actual"));
    // 页面脚本/HTML 必须带 engine+synthetic 分支（静态断言：不允许只有「已生效（actual）」一条路径）。
    assert!(INDEX_HTML.contains("st-engine"), "页面缺少 engine 挂点");
    assert!(APP_JS.contains("isSynthetic"), "页面缺少 synthetic 分支");
    assert!(APP_JS.contains("synthetic 回执"), "页面 commit 分支未区分 synthetic");
    assert!(APP_JS.contains("非 actual"), "页面未说明 synthetic 不是 actual");
    // F-13：volume/mute/format 回执同样带身份；页面不得把内存 engine 写成 actual。
    for (path, body) in [
        ("/api/runtime/volume", "{\"db\":-20.0}"),
        ("/api/runtime/mute", "{\"mute\":true}"),
    ] {
        let ack = json_body(&http_authed(server.addr, "PUT", path, Some(body)));
        assert_eq!(expect_json_str(&ack, "engine"), "memory");
        assert!(expect_json_bool(&ack, "synthetic"));
        assert!(!expect_json_bool(&ack, "actual"));
        assert!(APP_JS.contains("ackText(payload)"), "页面音量/静音回执未走 synthetic 分支");
    }
    let format = json_body(&http_authed(server.addr, "GET", "/api/audio/format", None));
    assert_eq!(expect_json_str(&format, "engine"), "memory");
    assert!(expect_json_bool(&format, "synthetic"));
    assert!(!expect_json_bool(&format, "actual"));
    assert_ne!(
        expect_json_str(&format, "source"),
        "internal RT host actual (T26)"
    );
    assert!(
        !APP_JS.contains("\"actual \" + compact(payload.volume_db)"),
        "页面音量回执不得写死 actual"
    );
    assert!(
        !APP_JS.contains("mute = \" + compact(payload.mute) + \"（actual）"),
        "页面静音回执不得写死 actual"
    );
}

// ---------------------------------------------------------------------------
// 证据快照（仅 T70_EVIDENCE_PAGE_DIR 设置时写盘；repro.ps1 用于生成 evidence/T70/page/）
// ---------------------------------------------------------------------------

#[test]
fn t70_web_evidence_page_snapshot() {
    let Some(dir) = std::env::var_os("T70_EVIDENCE_PAGE_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("create evidence page dir");
    std::fs::write(dir.join("index.html"), INDEX_HTML).expect("write index.html");
    std::fs::write(dir.join("app.js"), APP_JS).expect("write app.js");
    std::fs::write(dir.join("style.css"), STYLE_CSS).expect("write style.css");
    std::fs::write(dir.join("favicon.svg"), FAVICON_SVG).expect("write favicon.svg");
    println!("SAMPLE_EVIDENCE page snapshot dir={}", dir.display());
}

// ---------------------------------------------------------------------------
// G. 校准向导页面 / 校准 UI 契约（daemon 静态资源 + settings/level/delay/bass）
// ---------------------------------------------------------------------------

fn cal_obj<'a>(value: &'a JsonValue, key: &str) -> &'a JsonValue {
    value.get(key).unwrap_or_else(|| panic!("field {key} must exist"))
}

fn cal_arr<'a>(value: &'a JsonValue, key: &str) -> &'a Vec<JsonValue> {
    match value.get(key) {
        Some(JsonValue::Arr(items)) => items,
        other => panic!("field {key}: expected array, got {other:?}"),
    }
}

#[test]
fn t70_web_calibration_wizard_page_and_assets_are_served() {
    let server = TestServer::start(service_with_cal());
    let page = http_request(server.addr, "GET", "/calibration", &[], None);
    assert_status(&page, 200);
    assert!(page.content_type().starts_with("text/html"));
    let html = page.text();
    // 页面是内嵌模板；交互控件由 /calibration/app.js 渲染（下面断言 app.js 包含控件）。
    assert!(html.contains("TSPI 校准向导"));
    assert!(html.contains("btn-run-all"));
    // 校准向导是正式页面：外部同源资源，无 inline script/style（CSP script-src 'self'）。
    assert!(html.contains("href=\"/calibration/style.css\""));
    assert!(html.contains("src=\"/calibration/app.js\""));
    assert!(!html.contains("<script>"), "不得有 inline script");
    assert!(!html.contains("<style>"), "不得有 inline style");
    assert!(!html.contains("?live=1"), "正式页面不得保留 ?live=1 演示分支");
    let csp = page.header("Content-Security-Policy").unwrap_or_default();
    assert!(csp.contains("script-src 'self'"), "{csp}");

    let css = http_request(server.addr, "GET", "/calibration/style.css", &[], None);
    assert_status(&css, 200);
    assert!(css.content_type().starts_with("text/css"));
    assert!(css.text().contains(".lane-table"));
    let js = http_request(server.addr, "GET", "/calibration/app.js", &[], None);
    assert_status(&js, 200);
    assert!(js.content_type().starts_with("text/javascript"));
    let script = js.text();
    for needle in [
        "/api/calibration/settings",
        "/api/calibration/jobs",
        "/api/calibration/apply",
        "/api/calibration/clear",
        "pending_host_field",
        "bassPreview",
        "renderStepLevel",
        "renderStepDelay",
        "renderStepBass",
        "renderStepFir",
        "renderStepModal",
        "data-test=\"btn-settings-save\"",
        "data-test=\"mic-kind\"",
        "data-test=\"subs-table\"",
        "data-test=\"routing-table\"",
        "data-test=\"bass-pre-gain\"",
        "data-test=\"level-lanes\"",
        "data-test=\"delay-lanes\"",
        "data-test=\"fir-lanes\"",
        "data-test=\"modal-lanes\"",
        "data-test=\"runner-panel\"",
        "data-test=\"btn-run-retry\"",
    ] {
        assert!(script.contains(needle), "app.js 缺少 {needle}");
    }
    // 静态资源仅 GET；POST /calibration -> 405（不落到 REST/WS）。
    let post = http_request(server.addr, "POST", "/calibration", &[], None);
    assert_status(&post, 405);
}

#[test]
fn t70_web_api_calibration_settings_level_delay_bass_lanes() {
    let server = TestServer::start(service_with_cal());
    let settings = http_authed(server.addr, "GET", "/api/calibration/settings", None);
    assert_status(&settings, 200);
    let body = json_body(&settings);
    assert_eq!(expect_json_str(&body, "schema"), "tspi-cal-settings/v1");
    assert_eq!(
        expect_json_str(&body, "calibration_state"),
        "RELATIVE_UNCALIBRATED"
    );
    assert_eq!(
        expect_json_f64(cal_obj(cal_obj(&body, "settings"), "bass"), "subs_count"),
        3.0
    );

    let save = http_authed(
        server.addr,
        "POST",
        "/api/calibration/settings",
        Some(r#"{"mic":{"kind":"analog_48v","device":"hw:UltraLitemk5,0","channel":0,"phantom_48v":true,"gain_db":24},"cal_file":{"path":"/var/lib/tspi-rt-stack/cal/umik.txt","name":"umik.txt","sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","source":"path"},"orientation_deg":90,"target_mode":"flat","bass":{"enabled":true,"crossover_hz":80,"main_slope":"LR4","sub_slope":"LR4","linear_phase":true,"sub_gain_db":0,"strategy":"multi_b"}}"#),
    );
    assert_status(&save, 200);
    let saved = json_body(&save);
    assert_eq!(expect_json_str(&saved, "calibration_state"), "CALIBRATED_ABSOLUTE");
    assert!(expect_json_bool(cal_obj(&saved, "validation"), "ok"));

    let level = http_authed(
        server.addr,
        "POST",
        "/api/calibration/jobs",
        Some("{\"kind\":\"emperor\",\"stage\":\"level\",\"audible_sweep_authorized\":true}"),
    );
    assert_status(&level, 201);
    let body = json_body(&level);
    assert_eq!(expect_json_str(cal_obj(&body, "job"), "stage"), "level");
    let lanes = cal_obj(&body, "lanes");
    assert_eq!(expect_json_str(lanes, "stage"), "level");
    let rows = cal_arr(lanes, "lanes");
    assert_eq!(rows.len(), 7);
    assert_eq!(expect_json_str(&rows[0], "lane"), "FL");
    assert_eq!(expect_json_str(&rows[0], "state"), "locked");
    expect_json_f64(&rows[0], "achieved_dbfs");
    expect_json_f64(&rows[0], "snr_db");
    expect_json_f64(&rows[0], "locked_gain_db");
    assert!(expect_json_str(&body, "progress_text").contains("Level"));

    let apply = http_authed(
        server.addr,
        "POST",
        "/api/calibration/apply",
        Some(r#"{"targets":["level"],"level_trims":[{"lane":"FL","gain_db":-2.0},{"lane":"SUB1","gain_db":0.0}]}"#),
    );
    assert_status(&apply, 200);
    let applied = json_body(&apply);
    assert!(!expect_json_bool(&applied, "applied_to_audio"));
    assert_eq!(cal_arr(cal_obj(&applied, "applied"), "level_trims").len(), 2);
    let status = json_body(&http_authed(server.addr, "GET", "/api/calibration/status", None));
    assert_eq!(expect_json_str(&status, "schema"), "tspi-cal-status/v1");
    assert_eq!(
        cal_arr(cal_obj(&status, "applied"), "level_trims").len(),
        2
    );
    assert!(cal_arr(&status, "workflow")
        .iter()
        .any(|item| expect_json_str(item, "stage") == "level"));

    let delay = http_authed(
        server.addr,
        "POST",
        "/api/calibration/jobs",
        Some("{\"kind\":\"emperor\",\"stage\":\"delay\",\"audible_sweep_authorized\":true}"),
    );
    assert_status(&delay, 201);
    let delay_body = json_body(&delay);
    let delay_rows = cal_arr(cal_obj(&delay_body, "lanes"), "lanes");
    assert_eq!(expect_json_str(&delay_rows[0], "method"), "double_gate");
    assert!(delay_rows
        .iter()
        .any(|row| expect_json_str(row, "method") == "lfe_xcorr_40_150"));

    let bass = http_authed(
        server.addr,
        "POST",
        "/api/calibration/jobs",
        Some("{\"kind\":\"emperor\",\"stage\":\"bass\",\"audible_sweep_authorized\":true}"),
    );
    assert_status(&bass, 201);
    let bass_body = json_body(&bass);
    let bass_result = cal_obj(cal_obj(&bass_body, "lanes"), "bass");
    assert!(expect_json_bool(bass_result, "guardrail_passed"));
    assert_eq!(cal_arr(bass_result, "subs").len(), 3);
    expect_json_f64(bass_result, "pre_gain_db");

    // host 字段未就绪的 pending 语义（没有 port 的 503 已由旧用例覆盖；这里验证 501 文案）。
    let pending = http_authed(
        server.addr,
        "POST",
        "/api/calibration/settings/cal-file",
        Some("{}"),
    );
    assert_status(&pending, 501);
    assert_eq!(error_code(&pending), "pending_host_field");
}

#[test]
fn t70_web_calibration_one_click_six_stage_flow() {
    use std::time::Instant;
    let started = Instant::now();
    let server = TestServer::start(service_with_cal());
    let save = http_authed(
        server.addr,
        "POST",
        "/api/calibration/settings",
        Some(r#"{"mic":{"kind":"usb","device":"hw:U18dB,0","channel":0,"phantom_48v":false,"gain_db":18},"orientation_deg":90,"target_mode":"flat","bass":{"enabled":true,"crossover_hz":80,"main_slope":"LR4","sub_slope":"LR4","linear_phase":true,"sub_gain_db":0,"strategy":"multi_b"},"fir":{"enabled":true,"target_curve":"flat","taps":4096,"phase":"mixed"},"modal":{"enabled":true,"auto_detect":true,"max_notches":4}}"#),
    );
    assert_status(&save, 200);

    // 1) 一条命令/一个按钮的 dry-run：六阶段计划；未实现阶段必须明确 pending，不假装通过。
    let dry_run = http_authed(
        server.addr,
        "POST",
        "/api/calibration/run",
        Some("{\"dry_run\":true}"),
    );
    assert_status(&dry_run, 200);
    let plan = json_body(&dry_run);
    assert_eq!(expect_json_str(&plan, "schema"), "tspi-cal-run/v1");
    assert!(expect_json_bool(&plan, "dry_run"));
    assert!(!expect_json_bool(&plan, "audible"));
    let stages = cal_arr(&plan, "stages");
    assert_eq!(stages.len(), 6);
    let order: Vec<String> = stages.iter().map(|s| expect_json_str(s, "stage")).collect();
    assert_eq!(order, vec!["bass", "fir", "modal", "level", "delay", "measure"]);
    for stage in &stages[..5] {
        assert!(!expect_json_bool(stage, "available"), "未实现阶段必须标 pending");
        assert!(!cal_arr(stage, "pending_host_fields").is_empty());
    }
    assert!(expect_json_bool(&stages[5], "available"));

    // 2) 六阶段逐个走现有 job 状态机（真实 TCP + SimCalibration；无声）。
    let mut transitions = Vec::new();
    for stage in ["bass", "fir", "modal", "level", "delay"] {
        let body = format!(
            "{{\"kind\":\"emperor\",\"stage\":\"{stage}\",\"audible_sweep_authorized\":true}}"
        );
        let start = http_authed(server.addr, "POST", "/api/calibration/jobs", Some(&body));
        assert_status(&start, 201);
        let started = json_body(&start);
        let job_id = expect_json_str(&started, "job_id");
        assert_eq!(expect_json_str(cal_obj(&started, "job"), "stage"), stage);
        let lanes = cal_obj(&started, "lanes");
        assert_eq!(expect_json_str(lanes, "stage"), stage);
        if stage == "bass" {
            assert!(expect_json_bool(cal_obj(lanes, "bass"), "guardrail_passed"));
        } else {
            assert_eq!(cal_arr(lanes, "lanes").len(), 7);
        }
        transitions.push(format!("{stage}:queued->complete(job={job_id})"));
    }
    // 第 6 阶段 emperor/A-B：用 T60 状态机（full kind；synthetic engine 下 emperor 直接回 PREDICTED_ONLY），
    // 证明 queued -> preflight ... -> prepared -> apply complete 的真实状态流转。
    let start = http_authed(
        server.addr,
        "POST",
        "/api/calibration/jobs",
        Some("{\"kind\":\"full\",\"label\":\"one-click-emperor-ab\"}"),
    );
    assert_status(&start, 201);
    let started_body = json_body(&start);
    let job_id = expect_json_str(&started_body, "job_id");
    let mut seen = vec![expect_json_str(&started_body, "state")];
    for _ in 0..SIM_MAX_PROGRESS_POLLS {
        let job = cal_job(server.addr, &job_id);
        seen.push(expect_json_str(&job, "state"));
        if expect_json_str(&job, "state") == "prepared" {
            break;
        }
    }
    assert!(seen.contains(&"queued".to_string()));
    assert_eq!(seen.last().map(String::as_str), Some("prepared"));
    transitions.push(format!("measure:{}", seen.join("->")));
    let apply = http_authed(
        server.addr,
        "POST",
        &format!("/api/calibration/jobs/{job_id}/apply"),
        Some("{}"),
    );
    assert_status(&apply, 200);
    assert!(expect_json_bool(&json_body(&apply), "ok"));
    transitions.push("measure:prepared->complete(applied)".to_string());
    println!(
        "SAMPLE_CAL_ONE_CLICK stages=6 dry_run=ok transitions={} elapsed_ms={}",
        transitions.join(" | "),
        started.elapsed().as_millis()
    );
}
