//! T24 WS 控制面私有集成测试（WBS T24；写域 crates/compat/tests/T24/）。
//!
//! 覆盖 WBS T24 DoD（对应 compat-matrix 第 4 节 W01–W22、第 9 节 X10–X13）：
//! - W01 帧规则：文本 JSON、Binary 显式报错、Ping/Pong、Close、非法 UTF-8、超帧上限；
//! - W02/W03/W21 请求/回复形态与错误 envelope（同键、连接可继续、不 panic、不挂起）；
//! - W05–W15 读命令；
//! - W16 prepare/commit **实际 ack**（blocks>=1 + all-ack + version/graph/hash 一致才 Ok）；
//! - W17/W09 音量/静音（范围/越界/写后读回/互不覆盖）；
//! - W18 GetCaptureSignalRms（长度=采集通道、有限、-240 dBFS floor）；
//! - W19 fader（载荷/未知 id/越界/写后读回）；
//! - W20/W22 未知命令与 GetXruns 拒收；
//! - 命令表/错误码/JSON 编解码/ack 判据等契约级检查。
//!
//! Cargo 自动把 tests/T24/main.rs 发现为集成测试 target；DoD 命令：
//! cargo test -p khaen-compat ws（测试名统一 ws_ 前缀）。
//! 本文件不写临时文件（%TEMP% 可能在内存盘上），全部内存夹具。

use khaen_compat::ws::*;
use khaen_core::rt::RtStatus;

// ---------------------------------------------------------------------------
// 夹具辅助
// ---------------------------------------------------------------------------

fn hex64(byte: char) -> String {
    std::iter::repeat(byte).take(64).collect::<String>()
}

fn good_prepared(version: u32, workers: u8) -> PreparedConfig {
    PreparedConfig {
        plan_version: version,
        graph_id_hex: hex64('a'),
        config_sha256_hex: hex64('b'),
        expected_workers: workers,
        admitted_bytes: 4096,
        admission_note: "admitted".to_string(),
    }
}

fn good_ack(prepared: &PreparedConfig) -> CommitAck {
    CommitAck {
        plan_version: prepared.plan_version,
        previous_plan_version: prepared.plan_version.saturating_sub(1),
        graph_id_hex: prepared.graph_id_hex.clone(),
        config_sha256_hex: prepared.config_sha256_hex.clone(),
        expected_workers: prepared.expected_workers,
        blocks_committed: 3,
        ack_mask: ack_mask_for_workers(prepared.expected_workers),
        switched_at_block: 42,
        rolled_back: false,
    }
}

fn good_actual(version: u32) -> ActualPlan {
    ActualPlan {
        plan_version: version,
        graph_id_hex: hex64('c'),
        config_sha256_hex: hex64('d'),
    }
}

// ack 变异注入（非捕获 fn 指针，便于 FakePlane 保存）
fn ack_zero_blocks(p: &PreparedConfig) -> CommitAck {
    let mut ack = good_ack(p);
    ack.blocks_committed = 0;
    ack
}
fn ack_partial(p: &PreparedConfig) -> CommitAck {
    let mut ack = good_ack(p);
    ack.ack_mask = ack_mask_for_workers(p.expected_workers) >> 1;
    ack
}
fn ack_extra_bit(p: &PreparedConfig) -> CommitAck {
    let mut ack = good_ack(p);
    ack.ack_mask |= 0b1_0000;
    ack
}
fn ack_version_mismatch(p: &PreparedConfig) -> CommitAck {
    let mut ack = good_ack(p);
    ack.plan_version = p.plan_version + 1;
    ack
}
fn ack_graph_mismatch(p: &PreparedConfig) -> CommitAck {
    let mut ack = good_ack(p);
    ack.graph_id_hex = hex64('e');
    ack
}
fn ack_config_mismatch(p: &PreparedConfig) -> CommitAck {
    let mut ack = good_ack(p);
    ack.config_sha256_hex = hex64('f');
    ack
}
fn ack_rolled_back(p: &PreparedConfig) -> CommitAck {
    let mut ack = good_ack(p);
    ack.rolled_back = true;
    ack
}
fn ack_no_advance(p: &PreparedConfig) -> CommitAck {
    let mut ack = good_ack(p);
    ack.previous_plan_version = ack.plan_version;
    ack
}
fn ack_workers_mismatch(p: &PreparedConfig) -> CommitAck {
    let mut ack = good_ack(p);
    ack.expected_workers = if p.expected_workers == 4 { 2 } else { 4 };
    ack
}
fn ack_switched_zero(p: &PreparedConfig) -> CommitAck {
    let mut ack = good_ack(p);
    ack.switched_at_block = 0;
    ack
}

fn prepare_rejected(_: &str) -> Result<PreparedConfig, WsOpError> {
    Err(WsOpError::new(
        WsErrorCode::PrepareRejected,
        "config schema invalid",
    ))
}
fn prepare_bad_hex(_: &str) -> Result<PreparedConfig, WsOpError> {
    let mut prepared = good_prepared(8, 4);
    prepared.graph_id_hex = "not-a-sha256".to_string();
    Ok(prepared)
}
fn prepare_zero_version(_: &str) -> Result<PreparedConfig, WsOpError> {
    Ok(good_prepared(0, 4))
}
fn prepare_bad_workers(_: &str) -> Result<PreparedConfig, WsOpError> {
    Ok(good_prepared(8, 3))
}
fn commit_rejected(_: &PreparedConfig) -> Result<CommitAck, WsOpError> {
    Err(WsOpError::new(
        WsErrorCode::RuntimeFault,
        "device lost during commit",
    ))
}
fn volume_write_nan(_: f64) -> Result<f64, WsOpError> {
    Ok(f64::NAN)
}
fn volume_write_out_of_range(_: f64) -> Result<f64, WsOpError> {
    Ok(60.0)
}

/// Fake control plane：模拟 T26 将实现的新 host 控制面（所有状态可注入）。
struct FakePlane {
    state: WsRunState,
    stop_reason: WsStopReason,
    config_path: String,
    config_title: String,
    volume_db: f64,
    mute: bool,
    capture_rate_hz: u32,
    capture_channels: u32,
    clipped: u64,
    load: f64,
    buffer: i64,
    rate_adjust: f64,
    rate_adjust_available: bool,
    config_document: String,
    rms: Vec<f64>,
    faders: [Option<f64>; 4],
    actual: ActualPlan,
    prepared: PreparedConfig,
    prepare_override: Option<fn(&str) -> Result<PreparedConfig, WsOpError>>,
    commit_override: Option<fn(&PreparedConfig) -> Result<CommitAck, WsOpError>>,
    ack_override: Option<fn(&PreparedConfig) -> CommitAck>,
    abort_override: Option<fn(&PreparedConfig) -> Result<(), WsOpError>>,
    volume_write_override: Option<fn(f64) -> Result<f64, WsOpError>>,
    prepare_calls: u32,
    commit_calls: u32,
    abort_calls: u32,
    volume_write_calls: u32,
    last_config: String,
    last_committed_version: Option<u32>,
}

impl FakePlane {
    fn new() -> Self {
        Self {
            state: WsRunState::Running,
            stop_reason: WsStopReason::None,
            config_path: "/run/khaen/graph.khaen.json".to_string(),
            config_title: "khaen playback graph".to_string(),
            volume_db: -20.0,
            mute: false,
            capture_rate_hz: 48_000,
            capture_channels: 2,
            clipped: 7,
            load: 0.25,
            buffer: 512,
            rate_adjust: 1.5,
            rate_adjust_available: true,
            config_document: "{\"schema\":\"khaen-graph/v1\"}".to_string(),
            rms: vec![-23.5, -24.25],
            faders: [Some(0.0), Some(-120.0), None, None],
            actual: good_actual(7),
            prepared: good_prepared(8, 4),
            prepare_override: None,
            commit_override: None,
            ack_override: None,
            abort_override: None,
            volume_write_override: None,
            prepare_calls: 0,
            commit_calls: 0,
            abort_calls: 0,
            volume_write_calls: 0,
            last_config: String::new(),
            last_committed_version: None,
        }
    }
}

impl WsControlPlane for FakePlane {
    fn run_state(&self) -> WsRunState {
        self.state
    }
    fn stop_reason(&self) -> WsStopReason {
        self.stop_reason
    }
    fn config_path(&self) -> String {
        self.config_path.clone()
    }
    fn config_title(&self) -> String {
        self.config_title.clone()
    }
    fn volume_db(&self) -> f64 {
        self.volume_db
    }
    fn set_volume_db(&mut self, db: f64) -> Result<f64, WsOpError> {
        self.volume_write_calls += 1;
        if let Some(override_fn) = self.volume_write_override {
            return override_fn(db);
        }
        self.volume_db = db;
        Ok(db)
    }
    fn mute(&self) -> bool {
        self.mute
    }
    fn set_mute(&mut self, mute: bool) -> Result<bool, WsOpError> {
        self.mute = mute;
        Ok(mute)
    }
    fn capture_rate_hz(&self) -> u32 {
        self.capture_rate_hz
    }
    fn capture_channel_count(&self) -> u32 {
        self.capture_channels
    }
    fn clipped_samples(&self) -> u64 {
        self.clipped
    }
    fn processing_load(&self) -> f64 {
        self.load
    }
    fn buffer_level_frames(&self) -> i64 {
        self.buffer
    }
    fn rate_adjust_ppm(&self) -> f64 {
        self.rate_adjust
    }
    fn rate_adjust_available(&self) -> bool {
        self.rate_adjust_available
    }
    fn config_document(&self) -> String {
        self.config_document.clone()
    }
    fn capture_signal_rms_db(&self) -> Vec<f64> {
        self.rms.clone()
    }
    fn fader_volume_db(&self, fader_id: i32) -> Option<f64> {
        if fader_id < 0 {
            return None;
        }
        self.faders.get(fader_id as usize).copied().flatten()
    }
    fn set_fader_volume_db(&mut self, fader_id: i32, db: f64) -> Result<f64, WsOpError> {
        let slot = self
            .faders
            .get_mut(fader_id as usize)
            .ok_or_else(|| WsOpError::new(WsErrorCode::UnknownFader, "fader is not allocated"))?;
        if slot.is_none() {
            return Err(WsOpError::new(
                WsErrorCode::UnknownFader,
                "fader is not allocated",
            ));
        }
        *slot = Some(db);
        Ok(db)
    }
    fn actual_plan(&self) -> ActualPlan {
        self.actual.clone()
    }
    fn prepare_config_json(&mut self, config_json: &str) -> Result<PreparedConfig, WsOpError> {
        self.prepare_calls += 1;
        self.last_config = config_json.to_string();
        if let Some(override_fn) = self.prepare_override {
            return override_fn(config_json);
        }
        Ok(self.prepared.clone())
    }
    fn commit_prepared(&mut self, prepared: &PreparedConfig) -> Result<CommitAck, WsOpError> {
        self.commit_calls += 1;
        self.last_committed_version = Some(prepared.plan_version);
        if let Some(override_fn) = self.commit_override {
            return override_fn(prepared);
        }
        if let Some(override_fn) = self.ack_override {
            return Ok(override_fn(prepared));
        }
        Ok(good_ack(prepared))
    }
    fn abort_prepared(&mut self, prepared: &PreparedConfig) -> Result<(), WsOpError> {
        self.abort_calls += 1;
        if let Some(override_fn) = self.abort_override {
            return override_fn(prepared);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 回复解析辅助
// ---------------------------------------------------------------------------

fn new_session() -> WsSession {
    WsSession::new(WsLimits::default())
}

fn send(session: &mut WsSession, plane: &mut FakePlane, request: &str) -> JsonValue {
    let reply = session.handle_text(plane, request);
    let text = reply
        .as_text()
        .unwrap_or_else(|| panic!("expected text reply for {request}, got {reply:?}"))
        .to_string();
    json_parse(&text).unwrap_or_else(|err| panic!("reply is not JSON ({err}): {text}"))
}

fn send_raw(session: &mut WsSession, plane: &mut FakePlane, request: &str) -> String {
    session
        .handle_text(plane, request)
        .as_text()
        .unwrap_or_else(|| panic!("expected text reply for {request}"))
        .to_string()
}

fn first_pair(value: &JsonValue) -> (&str, &JsonValue) {
    let pairs = value.as_object().expect("reply must be a JSON object");
    assert_eq!(pairs.len(), 1, "reply must have exactly one command key");
    (pairs[0].0.as_str(), &pairs[0].1)
}

fn cmd_of(reply: &JsonValue) -> &str {
    first_pair(reply).0
}

fn cmd_payload(reply: &JsonValue) -> &JsonValue {
    first_pair(reply).1
}

fn ok_value(reply: &JsonValue) -> &JsonValue {
    let payload = cmd_payload(reply);
    assert_eq!(
        payload.get("result").and_then(|v| v.as_str()),
        Some("Ok"),
        "expected result Ok in {reply:?}"
    );
    payload.get("value").expect("Ok reply must carry value")
}

fn result_object(reply: &JsonValue) -> &JsonValue {
    let payload = cmd_payload(reply);
    let result = payload.get("result").expect("result field");
    assert!(
        result.as_str() != Some("Ok"),
        "expected error result in {reply:?}"
    );
    result
}

fn err_code(reply: &JsonValue) -> &str {
    result_object(reply)
        .get("code")
        .and_then(|v| v.as_str())
        .expect("error result must carry code")
}

fn err_message(reply: &JsonValue) -> &str {
    let result = result_object(reply);
    result
        .get("Error")
        .and_then(|v| v.as_str())
        .expect("error result must carry Error text")
}

fn generic_error_code(reply: &JsonValue) -> &str {
    reply
        .get("error")
        .and_then(|e| e.get("code"))
        .and_then(|v| v.as_str())
        .expect("generic error envelope must carry code")
}

fn number_value(reply: &JsonValue) -> f64 {
    ok_value(reply).as_f64().expect("numeric value")
}

fn string_value(reply: &JsonValue) -> &str {
    ok_value(reply).as_str().expect("string value")
}

// ---------------------------------------------------------------------------
// W01：帧规则（文本 JSON / Binary 报错 / Ping-Pong / Close / UTF-8 / 上限）
// ---------------------------------------------------------------------------

#[test]
fn ws_w01_text_frame_returns_keyed_json() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let reply = send(&mut session, &mut plane, "\"GetState\"");
    assert_eq!(cmd_of(&reply), "GetState");
    assert_eq!(string_value(&reply), "Running");
}

#[test]
fn ws_w01_binary_frame_is_explicit_error_not_text() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let reply = session.handle_frame(&mut plane, WsFrame::Binary(b"\"GetState\""));
    let text = reply
        .as_text()
        .expect("binary must get explicit text error");
    let parsed = json_parse(text).expect("error reply is JSON");
    assert_eq!(generic_error_code(&parsed), "invalid_frame_type");
    // 连接可继续：后续文本命令仍然工作（W01/W20）。
    let next = send(&mut session, &mut plane, "\"GetState\"");
    assert_eq!(string_value(&next), "Running");
    assert_eq!(session.stats().binary_frames, 1);
    assert_eq!(session.stats().invalid_frames, 1);
}

#[test]
fn ws_w01_ping_echoes_pong_with_same_payload() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let reply = session.handle_frame(&mut plane, WsFrame::Ping(b"keepalive"));
    assert_eq!(reply, WsReply::Pong(b"keepalive".to_vec()));
    assert_eq!(session.stats().ping_frames, 1);
}

#[test]
fn ws_w01_pong_is_ignored() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let reply = session.handle_frame(&mut plane, WsFrame::Pong(b"x"));
    assert_eq!(reply, WsReply::None);
    assert_eq!(session.stats().pong_frames, 1);
}

#[test]
fn ws_w01_close_returns_normal_closure_and_marks_closed() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let reply = session.handle_frame(&mut plane, WsFrame::Close);
    assert_eq!(reply.as_close(), Some((WS_CLOSE_NORMAL, "normal_closure")));
    assert!(session.is_closed());
    // 关闭后不再处理帧（传输层不应再喂帧）。
    assert_eq!(
        session.handle_text(&mut plane, "\"GetState\""),
        WsReply::None
    );
    assert_eq!(session.stats().close_frames, 1);
}

#[test]
fn ws_w01_close_aborts_pending_prepare() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let prepared = send(
        &mut session,
        &mut plane,
        "{\"PrepareConfigJson\":\"{\\\"schema\\\":\\\"khaen-graph/v1\\\"}\"}",
    );
    assert_eq!(
        ok_value(&prepared)
            .get("prepared")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
    assert!(session.pending_prepare().is_some());
    let reply = session.handle_frame(&mut plane, WsFrame::Close);
    assert!(reply.as_close().is_some());
    assert!(session.pending_prepare().is_none());
    assert_eq!(plane.abort_calls, 1);
}

#[test]
fn ws_w01_invalid_utf8_bytes_close_1007() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let reply = session.handle_bytes(&mut plane, &[0xFF, 0xFE, 0xFD]);
    assert_eq!(
        reply.as_close(),
        Some((WS_CLOSE_INVALID_PAYLOAD, "invalid_utf8"))
    );
    assert!(session.is_closed());
}

#[test]
fn ws_w01_oversized_frame_closes_1009() {
    let mut session = WsSession::new(WsLimits::new(16, 8));
    let mut plane = FakePlane::new();
    let oversized = "\"GetStateXXXXXXXXXXXXXXXXXXXX\"";
    let reply = session.handle_text(&mut plane, oversized);
    let (code, reason) = reply.as_close().expect("oversized frame must close");
    assert_eq!(code, WS_CLOSE_TOO_BIG);
    assert_eq!(reason, "payload_too_large");
    assert!(session.is_closed());
}

#[test]
fn ws_w01_bytes_path_accepts_valid_text() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let reply = session.handle_bytes(&mut plane, b"\"GetVersion\"");
    let text = reply
        .as_text()
        .expect("valid text bytes must be dispatched");
    let parsed = json_parse(text).expect("JSON reply");
    assert_eq!(cmd_of(&parsed), "GetVersion");
}

// ---------------------------------------------------------------------------
// W02/W03/W21：请求形态、回复形态、非法帧拒收（不猜、不 panic）
// ---------------------------------------------------------------------------

#[test]
fn ws_w02_string_command_supported() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let reply = send(&mut session, &mut plane, "\"GetVersion\"");
    assert_eq!(cmd_of(&reply), "GetVersion");
    assert_eq!(string_value(&reply), WS_PROTOCOL_VERSION);
}

#[test]
fn ws_w02_single_key_object_supported() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let reply = send(&mut session, &mut plane, "{\"SetVolume\":-12.5}");
    assert_eq!(cmd_of(&reply), "SetVolume");
    assert_eq!(number_value(&reply), -12.5);
}

#[test]
fn ws_w02_multi_key_object_rejected_not_guessed() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let raw = send_raw(
        &mut session,
        &mut plane,
        "{\"GetState\":null,\"GetVolume\":null}",
    );
    let reply = json_parse(&raw).expect("JSON error reply");
    assert_eq!(generic_error_code(&reply), "invalid_request");
    assert_eq!(plane.volume_write_calls, 0);
}

#[test]
fn ws_w02_empty_object_rejected() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let reply = json_parse(&send_raw(&mut session, &mut plane, "{}")).expect("JSON");
    assert_eq!(generic_error_code(&reply), "invalid_request");
}

#[test]
fn ws_w02_scalar_and_array_requests_rejected() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    for raw in ["42", "true", "null", "[\"GetState\"]"] {
        let reply = json_parse(&send_raw(&mut session, &mut plane, raw)).expect("JSON");
        assert_eq!(generic_error_code(&reply), "invalid_request", "input {raw}");
    }
}

#[test]
fn ws_w02_invalid_command_names_rejected() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    for raw in [
        "\"\"",
        "\"Get State\"",
        "\"Get-State\"",
        "{\"Get/State\":null}",
    ] {
        let reply = json_parse(&send_raw(&mut session, &mut plane, raw)).expect("JSON");
        assert_eq!(generic_error_code(&reply), "invalid_request", "input {raw}");
    }
    assert_eq!(session.stats().commands_ok, 0);
}

#[test]
fn ws_w03_ok_reply_has_result_ok_and_value() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let reply = send(&mut session, &mut plane, "\"GetMute\"");
    let payload = cmd_payload(&reply);
    assert_eq!(payload.get("result").and_then(|v| v.as_str()), Some("Ok"));
    assert_eq!(payload.get("value").and_then(|v| v.as_bool()), Some(false));
}

#[test]
fn ws_w03_error_reply_is_same_keyed_object_with_code() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let reply = send(&mut session, &mut plane, "\"GetXruns\"");
    assert_eq!(cmd_of(&reply), "GetXruns");
    assert_eq!(err_code(&reply), "unknown_command");
    assert!(err_message(&reply).contains("unknown_command"));
}

#[test]
fn ws_w03_reply_is_always_single_keyed() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    for request in ["\"GetState\"", "\"GetVersion\"", "\"NoSuchCommand\""] {
        let reply = send(&mut session, &mut plane, request);
        let pairs = reply.as_object().expect("object reply");
        assert_eq!(pairs.len(), 1, "reply for {request} must be single-key");
    }
}

#[test]
fn ws_w04_sequential_commands_one_reply_each_in_order() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let requests = [
        "\"GetVersion\"",
        "\"GetState\"",
        "\"GetCaptureRate\"",
        "\"GetVolume\"",
        "\"GetMute\"",
    ];
    let expected = [
        "GetVersion",
        "GetState",
        "GetCaptureRate",
        "GetVolume",
        "GetMute",
    ];
    for (request, name) in requests.iter().zip(expected.iter()) {
        let reply = send(&mut session, &mut plane, request);
        assert_eq!(cmd_of(&reply), *name);
    }
    let stats = session.stats();
    assert_eq!(stats.frames, 5);
    assert_eq!(stats.text_frames, 5);
    assert_eq!(stats.commands_ok, 5);
    assert_eq!(stats.commands_err, 0);
}

#[test]
fn ws_w21_non_json_rejected_without_panic() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    for raw in ["", "not json", "{", "}", "{\"a\":}", "{'a':1}", "tru", "01"] {
        let reply = json_parse(&send_raw(&mut session, &mut plane, raw));
        let reply = reply.expect("frame-level error must be valid JSON");
        assert_eq!(generic_error_code(&reply), "invalid_json", "input {raw}");
        assert!(
            !session.is_closed(),
            "recoverable parse errors keep connection"
        );
    }
    assert!(session.stats().invalid_frames >= 8);
}

#[test]
fn ws_w21_deep_nesting_rejected_not_stack_overflow() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let mut deep = String::new();
    for _ in 0..200 {
        deep.push('[');
    }
    deep.push_str("1");
    for _ in 0..200 {
        deep.push(']');
    }
    let reply = json_parse(&send_raw(&mut session, &mut plane, &deep)).expect("JSON");
    assert_eq!(generic_error_code(&reply), "invalid_json");
}

#[test]
fn ws_w21_control_character_in_string_rejected() {
    assert!(json_parse("\"bad\u{0001}char\"").is_err());
}

#[test]
fn ws_w21_invalid_json_does_not_lose_connection() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let _ = session.handle_text(&mut plane, "###");
    let reply = send(&mut session, &mut plane, "\"GetState\"");
    assert_eq!(string_value(&reply), "Running");
}

// ---------------------------------------------------------------------------
// W05–W15：读命令（类型/actual 语义/口径）
// ---------------------------------------------------------------------------

#[test]
fn ws_w05_get_version_is_new_stack_protocol() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let reply = send(&mut session, &mut plane, "\"GetVersion\"");
    let version = string_value(&reply);
    assert_eq!(version, "khaen-ws/1");
    assert_eq!(version, WS_PROTOCOL_VERSION);
    assert_ne!(version, "CamillaDSP");
}

#[test]
fn ws_w06_get_state_reports_five_states_without_optimism() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    for expected in [
        WsRunState::Inactive,
        WsRunState::Starting,
        WsRunState::Running,
        WsRunState::Paused,
        WsRunState::Error,
    ] {
        plane.state = expected;
        let reply = send(&mut session, &mut plane, "\"GetState\"");
        assert_eq!(string_value(&reply), expected.as_str());
    }
    // 非 Running 状态绝不乐观报 Running。
    plane.state = WsRunState::Error;
    let reply = send(&mut session, &mut plane, "\"GetState\"");
    assert_ne!(string_value(&reply), "Running");
}

#[test]
fn ws_w07_get_stop_reason_none_is_null() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let reply = send(&mut session, &mut plane, "\"GetStopReason\"");
    assert!(ok_value(&reply).is_null());
}

#[test]
fn ws_w07_get_stop_reason_error_codes_are_stable() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    for reason in [
        WsStopReason::CaptureError,
        WsStopReason::PlaybackError,
        WsStopReason::CaptureFormatChange,
        WsStopReason::PlaybackFormatChange,
    ] {
        plane.stop_reason = reason;
        let reply = send(&mut session, &mut plane, "\"GetStopReason\"");
        assert_eq!(string_value(&reply), reason.as_str());
        assert!(reason.is_error());
    }
}

#[test]
fn ws_w07_get_stop_reason_done_is_not_error() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.stop_reason = WsStopReason::Done;
    let reply = send(&mut session, &mut plane, "\"GetStopReason\"");
    assert_eq!(string_value(&reply), "Done");
    assert!(!WsStopReason::Done.is_error());
    assert!(!WsStopReason::None.is_error());
}

#[test]
fn ws_w08_config_path_and_title() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.config_path = "/run/khaen/graph.json".to_string();
    plane.config_title = "P1 graph".to_string();
    assert_eq!(
        string_value(&send(&mut session, &mut plane, "\"GetConfigFilePath\"")),
        "/run/khaen/graph.json"
    );
    assert_eq!(
        string_value(&send(&mut session, &mut plane, "\"GetConfigTitle\"")),
        "P1 graph"
    );
}

#[test]
fn ws_w09_get_volume_and_mute_are_actual_values() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.volume_db = -6.5;
    plane.mute = true;
    assert_eq!(
        number_value(&send(&mut session, &mut plane, "\"GetVolume\"")),
        -6.5
    );
    assert_eq!(
        ok_value(&send(&mut session, &mut plane, "\"GetMute\"")).as_bool(),
        Some(true)
    );
}

#[test]
fn ws_w10_capture_rate_actual_values() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    for rate in [48_000u32, 96_000] {
        plane.capture_rate_hz = rate;
        let reply = send(&mut session, &mut plane, "\"GetCaptureRate\"");
        assert_eq!(ok_value(&reply).as_u64(), Some(rate as u64));
    }
}

#[test]
fn ws_w10_capture_rate_zero_means_not_running() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.state = WsRunState::Inactive;
    plane.capture_rate_hz = 0;
    let reply = send(&mut session, &mut plane, "\"GetCaptureRate\"");
    assert_eq!(ok_value(&reply).as_u64(), Some(0));
}

#[test]
fn ws_w10_unsupported_capture_rate_rejected() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.capture_rate_hz = 44_100;
    let reply = send(&mut session, &mut plane, "\"GetCaptureRate\"");
    assert_eq!(err_code(&reply), "internal");
}

#[test]
fn ws_w11_clipped_samples_exact_u64() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.clipped = 9_007_199_254_741_993;
    let raw = send_raw(&mut session, &mut plane, "\"GetClippedSamples\"");
    // u64 计数必须精确编码（不经 f64 丢精度）。
    assert!(raw.contains("9007199254741993"), "raw reply: {raw}");
    let reply = json_parse(&raw).expect("JSON");
    assert_eq!(cmd_of(&reply), "GetClippedSamples");
}

#[test]
fn ws_w12_processing_load_bounded_and_finite() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.load = 0.37;
    assert_eq!(
        number_value(&send(&mut session, &mut plane, "\"GetProcessingLoad\"")),
        0.37
    );
    plane.load = 1.8; // 过载如实上报，不夹取
    assert_eq!(
        number_value(&send(&mut session, &mut plane, "\"GetProcessingLoad\"")),
        1.8
    );
}

#[test]
fn ws_w12_processing_load_nan_or_negative_rejected() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.load = f64::NAN;
    let raw = send_raw(&mut session, &mut plane, "\"GetProcessingLoad\"");
    assert!(!raw.contains("NaN"), "NaN must never reach wire: {raw}");
    let reply = json_parse(&raw).expect("JSON");
    assert_eq!(err_code(&reply), "internal");
    plane.load = -0.1;
    let reply = send(&mut session, &mut plane, "\"GetProcessingLoad\"");
    assert_eq!(err_code(&reply), "internal");
}

#[test]
fn ws_w13_buffer_level_i64() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.buffer = -256;
    let reply = send(&mut session, &mut plane, "\"GetBufferLevel\"");
    assert_eq!(ok_value(&reply).as_i64(), Some(-256));
}

#[test]
fn ws_w14_rate_adjust_finite_passthrough() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.rate_adjust = -12.75;
    assert_eq!(
        number_value(&send(&mut session, &mut plane, "\"GetRateAdjust\"")),
        -12.75
    );
    plane.rate_adjust = f64::INFINITY;
    let reply = send(&mut session, &mut plane, "\"GetRateAdjust\"");
    assert_eq!(err_code(&reply), "internal");
    // F-28：没有唯一 clock servo 读数时显式不可用，不得回 0.0 冒充测量值。
    plane.rate_adjust = 0.0;
    plane.rate_adjust_available = false;
    let reply = send(&mut session, &mut plane, "\"GetRateAdjust\"");
    assert_eq!(err_code(&reply), "unsupported");
}

#[test]
fn ws_w15_get_config_returns_document_string() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.config_document = "{\"schema\":\"khaen-graph/v1\",\"version\":3}".to_string();
    let reply = send(&mut session, &mut plane, "\"GetConfig\"");
    assert_eq!(
        string_value(&reply),
        "{\"schema\":\"khaen-graph/v1\",\"version\":3}"
    );
}

// ---------------------------------------------------------------------------
// W17/W09：音量/静音写路径（范围/越界/写后读回/互不覆盖）
// ---------------------------------------------------------------------------

#[test]
fn ws_w17_set_volume_returns_actual_and_reads_back() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let reply = send(&mut session, &mut plane, "{\"SetVolume\":-12.0}");
    assert_eq!(number_value(&reply), -12.0);
    assert_eq!(plane.volume_db, -12.0);
    let readback = send(&mut session, &mut plane, "\"GetVolume\"");
    assert_eq!(number_value(&readback), -12.0);
}

#[test]
fn ws_w17_set_volume_bounds_inclusive() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    assert_eq!(
        number_value(&send(&mut session, &mut plane, "{\"SetVolume\":-150.0}")),
        VOLUME_MIN_DB
    );
    assert_eq!(
        number_value(&send(&mut session, &mut plane, "{\"SetVolume\":50.0}")),
        VOLUME_MAX_DB
    );
}

#[test]
fn ws_w17_set_volume_out_of_range_rejected_before_host() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    for raw in ["{\"SetVolume\":50.001}", "{\"SetVolume\":-150.001}"] {
        let reply = send(&mut session, &mut plane, raw);
        assert_eq!(err_code(&reply), "invalid_argument", "input {raw}");
    }
    assert_eq!(plane.volume_write_calls, 0);
}

#[test]
fn ws_w17_set_volume_type_errors_rejected() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    for raw in [
        "{\"SetVolume\":\"loud\"}",
        "{\"SetVolume\":true}",
        "\"SetVolume\"",
    ] {
        let reply = send(&mut session, &mut plane, raw);
        assert_eq!(err_code(&reply), "invalid_argument", "input {raw}");
    }
    assert_eq!(plane.volume_write_calls, 0);
}

#[test]
fn ws_w17_host_actual_nan_or_out_of_range_is_internal() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.volume_write_override = Some(volume_write_nan);
    let reply = send(&mut session, &mut plane, "{\"SetVolume\":-3.0}");
    assert_eq!(err_code(&reply), "internal");
    plane.volume_write_override = Some(volume_write_out_of_range);
    let reply = send(&mut session, &mut plane, "{\"SetVolume\":-3.0}");
    assert_eq!(err_code(&reply), "internal");
}

#[test]
fn ws_w09_mute_and_volume_do_not_overwrite_each_other() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.volume_db = -10.0;
    // 改音量不影响 mute。
    assert_eq!(
        number_value(&send(&mut session, &mut plane, "{\"SetVolume\":-30.0}")),
        -30.0
    );
    assert!(!ok_value(&send(&mut session, &mut plane, "\"GetMute\""))
        .as_bool()
        .unwrap());
    // 设 mute 不影响 volume。
    assert_eq!(
        ok_value(&send(&mut session, &mut plane, "{\"SetMute\":true}")).as_bool(),
        Some(true)
    );
    assert_eq!(
        number_value(&send(&mut session, &mut plane, "\"GetVolume\"")),
        -30.0
    );
    assert_eq!(plane.mute, true);
    assert_eq!(plane.volume_db, -30.0);
}

#[test]
fn ws_w09_set_mute_type_error_rejected() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    for raw in ["{\"SetMute\":1}", "{\"SetMute\":\"true\"}", "\"SetMute\""] {
        let reply = send(&mut session, &mut plane, raw);
        assert_eq!(err_code(&reply), "invalid_argument", "input {raw}");
    }
}

#[test]
fn ws_w09_host_volume_nan_rejected_on_read() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.volume_db = f64::NEG_INFINITY;
    let raw = send_raw(&mut session, &mut plane, "\"GetVolume\"");
    assert!(!raw.contains("inf"), "inf must never reach wire: {raw}");
    let reply = json_parse(&raw).expect("JSON");
    assert_eq!(err_code(&reply), "internal");
}

// ---------------------------------------------------------------------------
// W18：GetCaptureSignalRms
// ---------------------------------------------------------------------------

#[test]
fn ws_w18_rms_array_matches_capture_channels() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.capture_channels = 3;
    plane.rms = vec![-23.5, -24.25, -240.0];
    let reply = send(&mut session, &mut plane, "\"GetCaptureSignalRms\"");
    let values = ok_value(&reply).as_array().expect("rms array");
    assert_eq!(values.len(), 3);
    assert!((values[0].as_f64().unwrap() - (-23.5)).abs() < 1e-9);
    assert!((values[2].as_f64().unwrap() - (-240.0)).abs() < 1e-9);
}

#[test]
fn ws_w18_rms_length_mismatch_rejected() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.capture_channels = 2;
    plane.rms = vec![-20.0];
    let reply = send(&mut session, &mut plane, "\"GetCaptureSignalRms\"");
    assert_eq!(err_code(&reply), "internal");
}

#[test]
fn ws_w18_rms_non_finite_rejected_without_wire_nan() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.capture_channels = 1;
    plane.rms = vec![f64::NAN];
    let raw = send_raw(&mut session, &mut plane, "\"GetCaptureSignalRms\"");
    assert!(!raw.contains("NaN"), "NaN must never reach wire: {raw}");
    let reply = json_parse(&raw).expect("JSON");
    assert_eq!(err_code(&reply), "internal");
}

#[test]
fn ws_w18_rms_silence_floor_is_documented_value() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.capture_channels = 1;
    plane.rms = vec![-1000.0];
    let reply = send(&mut session, &mut plane, "\"GetCaptureSignalRms\"");
    let values = ok_value(&reply).as_array().expect("rms array");
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].as_f64(), Some(RMS_FLOOR_DB));
    assert_eq!(RMS_FLOOR_DB, -240.0);
}

#[test]
fn ws_w18_rms_zero_channels_is_empty_array() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.state = WsRunState::Inactive;
    plane.capture_channels = 0;
    plane.rms = vec![];
    let reply = send(&mut session, &mut plane, "\"GetCaptureSignalRms\"");
    let values = ok_value(&reply).as_array().expect("rms array");
    assert!(values.is_empty());
}

// ---------------------------------------------------------------------------
// W19/W13：fader 兼容
// ---------------------------------------------------------------------------

#[test]
fn ws_w19_set_fader_known_id_and_readback() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let reply = send(&mut session, &mut plane, "{\"SetFaderVolume\":[1,-6.0]}");
    assert_eq!(number_value(&reply), -6.0);
    assert_eq!(plane.faders[1], Some(-6.0));
    let readback = send(&mut session, &mut plane, "{\"GetFaderVolume\":1}");
    assert_eq!(number_value(&readback), -6.0);
}

#[test]
fn ws_w19_set_fader_unknown_id_is_explicit_error() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let reply = send(&mut session, &mut plane, "{\"SetFaderVolume\":[3,-6.0]}");
    assert_eq!(err_code(&reply), "unknown_fader");
    assert_eq!(plane.faders[3], None);
    let read = send(&mut session, &mut plane, "{\"GetFaderVolume\":3}");
    assert_eq!(err_code(&read), "unknown_fader");
}

#[test]
fn ws_w19_fader_payload_shape_errors() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    for raw in [
        "{\"SetFaderVolume\":\"1,-6\"}",
        "{\"SetFaderVolume\":[1]}",
        "{\"SetFaderVolume\":[1,-6.0,0.0]}",
        "{\"SetFaderVolume\":[1.5,-6.0]}",
        "{\"SetFaderVolume\":[-1,-6.0]}",
        "{\"SetFaderVolume\":[\"1\",-6.0]}",
        "\"SetFaderVolume\"",
    ] {
        let reply = send(&mut session, &mut plane, raw);
        assert_eq!(err_code(&reply), "invalid_argument", "input {raw}");
    }
    assert_eq!(plane.faders[1], Some(-120.0));
}

#[test]
fn ws_w19_fader_volume_range_enforced() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    for raw in [
        "{\"SetFaderVolume\":[1,50.5]}",
        "{\"SetFaderVolume\":[1,-150.5]}",
    ] {
        let reply = send(&mut session, &mut plane, raw);
        assert_eq!(err_code(&reply), "invalid_argument", "input {raw}");
    }
    assert_eq!(plane.faders[1], Some(-120.0));
}

#[test]
fn ws_w19_fader_zero_based_id_supported() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let reply = send(&mut session, &mut plane, "{\"SetFaderVolume\":[0,-3.0]}");
    assert_eq!(number_value(&reply), -3.0);
}

// ---------------------------------------------------------------------------
// W16：prepare/commit 实际 ack（本任务核心；不假 Ok）
// ---------------------------------------------------------------------------

fn request_object(command: &str, payload: JsonValue) -> String {
    json_encode(&json_object(vec![(command, payload)])).expect("encode request")
}

fn set_config_request(config: &str) -> String {
    request_object("SetConfigJson", JsonValue::String(config.to_string()))
}

fn prepare_config_request(config: &str) -> String {
    request_object("PrepareConfigJson", JsonValue::String(config.to_string()))
}

fn commit_request(payload: JsonValue) -> String {
    request_object("CommitConfig", payload)
}

const CONFIG: &str = "{\"schema\":\"khaen-graph/v1\",\"version\":1}";

#[test]
fn ws_w16_set_config_json_returns_actual_ack_only_after_all_ack() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let reply = send(&mut session, &mut plane, &set_config_request(CONFIG));
    let value = ok_value(&reply);
    assert_eq!(value.get("applied").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(value.get("plan_version").and_then(|v| v.as_u64()), Some(8));
    assert_eq!(
        value.get("previous_plan_version").and_then(|v| v.as_u64()),
        Some(7)
    );
    assert_eq!(
        value.get("graph_id").and_then(|v| v.as_str()),
        Some(hex64('a').as_str())
    );
    assert_eq!(
        value.get("config_sha256").and_then(|v| v.as_str()),
        Some(hex64('b').as_str())
    );
    assert_eq!(
        value.get("blocks_committed").and_then(|v| v.as_u64()),
        Some(3)
    );
    assert_eq!(value.get("ack_mask").and_then(|v| v.as_u64()), Some(0xF));
    assert_eq!(
        value.get("expected_workers").and_then(|v| v.as_u64()),
        Some(4)
    );
    assert_eq!(plane.prepare_calls, 1);
    assert_eq!(plane.commit_calls, 1);
    assert_eq!(plane.abort_calls, 0);
    assert_eq!(plane.last_config, CONFIG);
    assert_eq!(plane.last_committed_version, Some(8));
}

#[test]
fn ws_w16_two_worker_profile_uses_two_bit_ack_mask() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.prepared = good_prepared(9, 2);
    let reply = send(&mut session, &mut plane, &set_config_request(CONFIG));
    let value = ok_value(&reply);
    assert_eq!(value.get("ack_mask").and_then(|v| v.as_u64()), Some(0b11));
    assert_eq!(
        value.get("expected_workers").and_then(|v| v.as_u64()),
        Some(2)
    );
    assert_eq!(value.get("plan_version").and_then(|v| v.as_u64()), Some(9));
}

#[test]
fn ws_w16_commit_not_acked_when_zero_blocks() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.ack_override = Some(ack_zero_blocks);
    let reply = send(&mut session, &mut plane, &set_config_request(CONFIG));
    assert_eq!(err_code(&reply), "commit_not_acked");
    assert!(err_message(&reply).contains("block"));
    // 未达到实际 ack 必须触发 abort，绝不假 Ok。
    assert_eq!(plane.abort_calls, 1);
}

#[test]
fn ws_w16_commit_not_acked_when_partial_worker_ack() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.ack_override = Some(ack_partial);
    let reply = send(&mut session, &mut plane, &set_config_request(CONFIG));
    assert_eq!(err_code(&reply), "commit_not_acked");
    assert!(err_message(&reply).contains("all-ack"));
    assert_eq!(plane.abort_calls, 1);
}

#[test]
fn ws_w16_commit_not_acked_when_extra_ack_bit() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.ack_override = Some(ack_extra_bit);
    let reply = send(&mut session, &mut plane, &set_config_request(CONFIG));
    assert_eq!(err_code(&reply), "commit_not_acked");
    assert_eq!(plane.abort_calls, 1);
}

#[test]
fn ws_w16_commit_not_acked_when_version_mismatch() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.ack_override = Some(ack_version_mismatch);
    let reply = send(&mut session, &mut plane, &set_config_request(CONFIG));
    assert_eq!(err_code(&reply), "commit_not_acked");
    assert!(err_message(&reply).contains("plan_version"));
}

#[test]
fn ws_w16_commit_not_acked_when_graph_id_mismatch() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.ack_override = Some(ack_graph_mismatch);
    let reply = send(&mut session, &mut plane, &set_config_request(CONFIG));
    assert_eq!(err_code(&reply), "commit_not_acked");
    assert!(err_message(&reply).contains("graph_id"));
}

#[test]
fn ws_w16_commit_not_acked_when_config_hash_mismatch() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.ack_override = Some(ack_config_mismatch);
    let reply = send(&mut session, &mut plane, &set_config_request(CONFIG));
    assert_eq!(err_code(&reply), "commit_not_acked");
    assert!(err_message(&reply).contains("config_sha256"));
}

#[test]
fn ws_w16_commit_not_acked_when_host_rolled_back() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.ack_override = Some(ack_rolled_back);
    let reply = send(&mut session, &mut plane, &set_config_request(CONFIG));
    assert_eq!(err_code(&reply), "commit_not_acked");
    assert!(err_message(&reply).contains("rollback"));
}

#[test]
fn ws_w16_commit_not_acked_when_plan_version_did_not_advance() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.ack_override = Some(ack_no_advance);
    let reply = send(&mut session, &mut plane, &set_config_request(CONFIG));
    assert_eq!(err_code(&reply), "commit_not_acked");
    assert!(err_message(&reply).contains("advance"));
}

#[test]
fn ws_w16_commit_not_acked_when_worker_count_mismatch() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.ack_override = Some(ack_workers_mismatch);
    let reply = send(&mut session, &mut plane, &set_config_request(CONFIG));
    assert_eq!(err_code(&reply), "commit_not_acked");
}

#[test]
fn ws_w16_commit_host_error_is_reported_and_no_double_abort() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.commit_override = Some(commit_rejected);
    let reply = send(&mut session, &mut plane, &set_config_request(CONFIG));
    assert_eq!(err_code(&reply), "runtime_fault");
    // host 承诺 commit 失败已回 last-good/mute：WS 不再重复 abort。
    assert_eq!(plane.abort_calls, 0);
    assert_eq!(plane.commit_calls, 1);
}

#[test]
fn ws_w16_prepare_host_error_maps_to_prepare_rejected() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.prepare_override = Some(prepare_rejected);
    let reply = send(&mut session, &mut plane, &set_config_request(CONFIG));
    assert_eq!(err_code(&reply), "prepare_rejected");
    assert_eq!(plane.commit_calls, 0);
    assert_eq!(plane.abort_calls, 0);
}

#[test]
fn ws_w16_prepare_invalid_hex_is_internal() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.prepare_override = Some(prepare_bad_hex);
    let reply = send(&mut session, &mut plane, &set_config_request(CONFIG));
    assert_eq!(err_code(&reply), "internal");
    assert_eq!(plane.commit_calls, 0);
}

#[test]
fn ws_w16_prepare_zero_version_or_bad_workers_is_internal() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.prepare_override = Some(prepare_zero_version);
    assert_eq!(
        err_code(&send(&mut session, &mut plane, &set_config_request(CONFIG))),
        "internal"
    );
    let mut session2 = new_session();
    let mut plane2 = FakePlane::new();
    plane2.prepare_override = Some(prepare_bad_workers);
    assert_eq!(
        err_code(&send(
            &mut session2,
            &mut plane2,
            &set_config_request(CONFIG)
        )),
        "internal"
    );
}

#[test]
fn ws_w16_config_oversize_rejected_before_host_call() {
    let mut session = WsSession::new(WsLimits::new(4096, 16));
    let mut plane = FakePlane::new();
    let long_config = "x".repeat(64);
    let reply = send(&mut session, &mut plane, &set_config_request(&long_config));
    assert_eq!(err_code(&reply), "payload_too_large");
    assert_eq!(plane.prepare_calls, 0);
}

#[test]
fn ws_w16_config_empty_or_wrong_type_rejected() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    assert_eq!(
        err_code(&send(&mut session, &mut plane, &set_config_request("   "))),
        "invalid_argument"
    );
    let reply = send(
        &mut session,
        &mut plane,
        &request_object("SetConfigJson", JsonValue::Bool(true)),
    );
    assert_eq!(err_code(&reply), "invalid_argument");
    assert_eq!(plane.prepare_calls, 0);
}

#[test]
fn ws_w16_explicit_prepare_commit_roundtrip() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let prepared = send(&mut session, &mut plane, &prepare_config_request(CONFIG));
    let value = ok_value(&prepared);
    assert_eq!(value.get("prepared").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(value.get("plan_version").and_then(|v| v.as_u64()), Some(8));
    assert_eq!(session.pending_prepare().map(|p| p.plan_version), Some(8));
    assert_eq!(plane.commit_calls, 0);

    let committed = send(&mut session, &mut plane, &commit_request(JsonValue::Null));
    let value = ok_value(&committed);
    assert_eq!(value.get("applied").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(value.get("plan_version").and_then(|v| v.as_u64()), Some(8));
    assert!(session.pending_prepare().is_none());
    assert_eq!(plane.commit_calls, 1);
}

#[test]
fn ws_w16_prepare_twice_is_prepare_busy() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let _ = send(&mut session, &mut plane, &prepare_config_request(CONFIG));
    let reply = send(&mut session, &mut plane, &prepare_config_request(CONFIG));
    assert_eq!(err_code(&reply), "prepare_busy");
    assert_eq!(plane.prepare_calls, 1);
}

#[test]
fn ws_w16_set_config_json_rejected_while_pending_prepare() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let _ = send(&mut session, &mut plane, &prepare_config_request(CONFIG));
    let reply = send(&mut session, &mut plane, &set_config_request(CONFIG));
    assert_eq!(err_code(&reply), "prepare_busy");
    assert_eq!(plane.prepare_calls, 1);
    assert_eq!(plane.commit_calls, 0);
}

#[test]
fn ws_w16_commit_without_prepare_is_not_prepared() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let reply = send(&mut session, &mut plane, &commit_request(JsonValue::Null));
    assert_eq!(err_code(&reply), "not_prepared");
    assert_eq!(plane.commit_calls, 0);
}

#[test]
fn ws_w16_commit_token_mismatch_is_stale_plan_and_keeps_pending() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let _ = send(&mut session, &mut plane, &prepare_config_request(CONFIG));
    let token = json_object(vec![("plan_version", JsonValue::UInt(99))]);
    let reply = send(&mut session, &mut plane, &commit_request(token));
    assert_eq!(err_code(&reply), "stale_plan");
    assert!(session.pending_prepare().is_some());
    assert_eq!(plane.commit_calls, 0);
}

#[test]
fn ws_w16_commit_with_matching_token_succeeds() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let _ = send(&mut session, &mut plane, &prepare_config_request(CONFIG));
    let token = json_object(vec![("plan_version", JsonValue::UInt(8))]);
    let reply = send(&mut session, &mut plane, &commit_request(token));
    assert_eq!(
        ok_value(&reply)
            .get("plan_version")
            .and_then(|v| v.as_u64()),
        Some(8)
    );
}

#[test]
fn ws_w16_abort_config_aborts_pending_and_is_idempotent_error() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let _ = send(&mut session, &mut plane, &prepare_config_request(CONFIG));
    let reply = send(&mut session, &mut plane, "\"AbortConfig\"");
    assert_eq!(
        ok_value(&reply).get("aborted").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(plane.abort_calls, 1);
    assert!(session.pending_prepare().is_none());
    assert_eq!(
        err_code(&send(&mut session, &mut plane, "\"AbortConfig\"")),
        "not_prepared"
    );
    assert_eq!(
        err_code(&send(
            &mut session,
            &mut plane,
            &commit_request(JsonValue::Null)
        )),
        "not_prepared"
    );
}

#[test]
fn ws_w16_commit_ignores_switched_at_block_as_audit_field() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.ack_override = Some(ack_switched_zero);
    let reply = send(&mut session, &mut plane, &set_config_request(CONFIG));
    assert_eq!(
        ok_value(&reply)
            .get("switched_at_block")
            .and_then(|v| v.as_u64()),
        Some(0)
    );
}

// ---------------------------------------------------------------------------
// actual version 读回（GetConfigVersion，P1 ADD；W16 的读侧补充）
// ---------------------------------------------------------------------------

#[test]
fn ws_get_config_version_reports_actual_plan() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.actual = good_actual(9);
    let reply = send(&mut session, &mut plane, "\"GetConfigVersion\"");
    let value = ok_value(&reply);
    assert_eq!(value.get("plan_version").and_then(|v| v.as_u64()), Some(9));
    assert_eq!(
        value.get("graph_id").and_then(|v| v.as_str()),
        Some(hex64('c').as_str())
    );
    assert_eq!(
        value.get("config_sha256").and_then(|v| v.as_str()),
        Some(hex64('d').as_str())
    );
}

#[test]
fn ws_get_config_version_before_first_plan_is_zero() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.actual = ActualPlan::none();
    let reply = send(&mut session, &mut plane, "\"GetConfigVersion\"");
    let value = ok_value(&reply);
    assert_eq!(value.get("plan_version").and_then(|v| v.as_u64()), Some(0));
    assert_eq!(value.get("graph_id").and_then(|v| v.as_str()), Some(""));
}

#[test]
fn ws_get_config_version_invalid_host_value_is_internal() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    plane.actual = ActualPlan {
        plan_version: 0,
        graph_id_hex: hex64('a'),
        config_sha256_hex: String::new(),
    };
    let reply = send(&mut session, &mut plane, "\"GetConfigVersion\"");
    assert_eq!(err_code(&reply), "internal");
}

// ---------------------------------------------------------------------------
// W20/W22/X10–X13：未知命令与拒收
// ---------------------------------------------------------------------------

#[test]
fn ws_w20_unknown_command_keyed_error_and_connection_continues() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let reply = send(&mut session, &mut plane, "\"Frobnicate\"");
    assert_eq!(cmd_of(&reply), "Frobnicate");
    assert_eq!(err_code(&reply), "unknown_command");
    let next = send(&mut session, &mut plane, "\"GetState\"");
    assert_eq!(string_value(&next), "Running");
}

#[test]
fn ws_w20_unknown_commands_counted() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    for command in ["NoSuchOne", "SetNothing", "GetNope"] {
        let request = request_object(command, JsonValue::Null);
        let reply = send(&mut session, &mut plane, &request);
        assert_eq!(cmd_of(&reply), command);
        assert_eq!(err_code(&reply), "unknown_command");
    }
    let stats = session.stats();
    assert_eq!(stats.unknown_commands, 3);
    assert_eq!(stats.commands_err, 3);
    assert_eq!(stats.commands_ok, 0);
}

#[test]
fn ws_w22_getxruns_is_unknown_command() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let reply = send(&mut session, &mut plane, "\"GetXruns\"");
    assert_eq!(cmd_of(&reply), "GetXruns");
    assert_eq!(err_code(&reply), "unknown_command");
    assert!(!is_supported_command("GetXruns"));
}

#[test]
fn ws_w20_unknown_command_with_object_payload_keeps_key() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let reply = send(&mut session, &mut plane, "{\"SetGain\":[1,2]}");
    assert_eq!(cmd_of(&reply), "SetGain");
    assert_eq!(err_code(&reply), "unknown_command");
}

// ---------------------------------------------------------------------------
// 契约级检查：命令表 / 枚举 / 错误码映射 / ack 判据 / JSON 值
// ---------------------------------------------------------------------------

#[test]
fn ws_required_subset_commands_all_supported() {
    assert_eq!(REQUIRED_SUBSET_COMMANDS.len(), 17);
    for command in REQUIRED_SUBSET_COMMANDS {
        assert!(
            is_supported_command(command),
            "missing required command {command}"
        );
    }
}

#[test]
fn ws_p1_added_commands_all_supported() {
    assert_eq!(P1_ADDED_COMMANDS.len(), 6);
    for command in P1_ADDED_COMMANDS {
        assert!(
            is_supported_command(command),
            "missing added command {command}"
        );
    }
}

#[test]
fn ws_supported_commands_table_is_consistent() {
    assert_eq!(
        SUPPORTED_COMMANDS.len(),
        REQUIRED_SUBSET_COMMANDS.len() + P1_ADDED_COMMANDS.len()
    );
    for command in SUPPORTED_COMMANDS {
        assert!(is_valid_command_name(command), "bad name {command}");
    }
    let mut sorted = SUPPORTED_COMMANDS.to_vec();
    sorted.sort_unstable();
    let mut dedup = sorted.clone();
    dedup.dedup();
    assert_eq!(sorted, dedup, "command table must not have duplicates");
    for command in REQUIRED_SUBSET_COMMANDS {
        assert!(SUPPORTED_COMMANDS.contains(command));
    }
    for command in P1_ADDED_COMMANDS {
        assert!(SUPPORTED_COMMANDS.contains(command));
    }
}

#[test]
fn ws_s1_web_live_commands_all_supported() {
    // 既有 web/src/camilla.rs 的 12 条 live 读命令 + GetConfig/SetConfigJson/SetVolume。
    let s1_web = [
        "GetVersion",
        "GetState",
        "GetStopReason",
        "GetConfigFilePath",
        "GetConfigTitle",
        "GetVolume",
        "GetMute",
        "GetCaptureRate",
        "GetClippedSamples",
        "GetProcessingLoad",
        "GetBufferLevel",
        "GetRateAdjust",
        "GetConfig",
        "SetConfigJson",
        "SetVolume",
    ];
    for command in s1_web {
        assert!(
            is_supported_command(command),
            "S1 web command unsupported: {command}"
        );
    }
}

#[test]
fn ws_s1_upmix_commands_supported() {
    // S1 tpspi-upmix 使用的两条命令（W18/W19）。
    assert!(is_supported_command("GetCaptureSignalRms"));
    assert!(is_supported_command("SetFaderVolume"));
}

#[test]
fn ws_every_supported_command_dispatches() {
    for command in SUPPORTED_COMMANDS {
        let mut session = new_session();
        let mut plane = FakePlane::new();
        let request = json_encode(&JsonValue::String(command.to_string())).expect("encode");
        let reply = send(&mut session, &mut plane, &request);
        assert_eq!(cmd_of(&reply), *command);
        let payload = cmd_payload(&reply);
        if payload.get("result").and_then(|v| v.as_str()) == Some("Ok") {
            continue;
        }
        let code = payload
            .get("result")
            .and_then(|r| r.get("code"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_ne!(code, "unknown_command", "dispatch missing for {command}");
        // 无载荷的写命令会报 invalid_argument，这是预期（不是 dispatch 分叉）。
        assert!(
            matches!(
                code,
                "invalid_argument" | "not_prepared" | "prepare_busy" | "stale_plan"
            ),
            "unexpected error {code} for supported command {command}"
        );
    }
}

#[test]
fn ws_protocol_version_contract() {
    assert!(!WS_PROTOCOL_VERSION.is_empty());
    assert!(WS_PROTOCOL_VERSION.starts_with("khaen-ws/"));
    assert!(!PROCESSING_LOAD_HINT.is_empty());
}

#[test]
fn ws_error_code_table_contract() {
    let mut seen: Vec<&str> = Vec::new();
    for code in WsErrorCode::ALL {
        let text = code.as_str();
        assert!(!text.is_empty());
        assert!(!text.contains(' '), "code must be stable token: {text}");
        assert!(!seen.contains(&text), "duplicate code {text}");
        seen.push(text);
        assert_eq!(WsErrorCode::from_str(text), Some(*code));
        assert_eq!(code.to_string(), text);
    }
    assert_eq!(WsErrorCode::from_str("no_such_code"), None);
    let error = WsOpError::new(WsErrorCode::CommitNotAcked, "not acked");
    assert_eq!(error.code(), WsErrorCode::CommitNotAcked);
    assert_eq!(error.message(), "not acked");
    let json = error.to_json();
    assert_eq!(
        json.get("code").and_then(|v| v.as_str()),
        Some("commit_not_acked")
    );
    assert!(json
        .get("Error")
        .and_then(|v| v.as_str())
        .unwrap()
        .contains("not acked"));
}

#[test]
fn ws_run_state_table_contract() {
    assert_eq!(WsRunState::ALL.len(), 5);
    let mut seen: Vec<&str> = Vec::new();
    for state in WsRunState::ALL {
        assert!(!seen.contains(&state.as_str()));
        seen.push(state.as_str());
        assert_eq!(WsRunState::from_str(state.as_str()), Some(*state));
    }
    assert_eq!(WsRunState::from_str("running"), None);
}

#[test]
fn ws_stop_reason_table_contract() {
    assert_eq!(WsStopReason::ALL.len(), 6);
    for reason in WsStopReason::ALL {
        assert_eq!(WsStopReason::from_str(reason.as_str()), Some(*reason));
    }
    assert!(!WsStopReason::None.is_error());
    assert!(!WsStopReason::Done.is_error());
    for reason in [
        WsStopReason::CaptureError,
        WsStopReason::PlaybackError,
        WsStopReason::CaptureFormatChange,
        WsStopReason::PlaybackFormatChange,
    ] {
        assert!(reason.is_error(), "{reason:?} must be an error reason");
    }
}

#[test]
fn ws_error_code_from_rt_status_mapping() {
    let cases = [
        (RtStatus::ERR_PLAN_STALE, WsErrorCode::StalePlan),
        (RtStatus::ERR_SWITCH_BUSY, WsErrorCode::SwitchBusy),
        (RtStatus::ERR_CAPACITY, WsErrorCode::AdmissionRejected),
        (
            RtStatus::ERR_ADMISSION_MEMORY,
            WsErrorCode::AdmissionRejected,
        ),
        (
            RtStatus::ERR_ADMISSION_SCHED,
            WsErrorCode::AdmissionRejected,
        ),
        (
            RtStatus::ERR_ADMISSION_CLOCK,
            WsErrorCode::AdmissionRejected,
        ),
        (RtStatus::ERR_DEVICE_BUSY, WsErrorCode::DeviceBusy),
        (RtStatus::ERR_DEVICE_PARAMS, WsErrorCode::InvalidArgument),
        (
            RtStatus::ERR_QUARANTINE_EXHAUSTED,
            WsErrorCode::QuarantineExhausted,
        ),
        (RtStatus::ERR_UNSUPPORTED, WsErrorCode::Unsupported),
        (
            RtStatus::from_code(RT_STATUS_MODE_ROLLED_BACK),
            WsErrorCode::RolledBack,
        ),
        (RtStatus::ERR_ABI_VERSION, WsErrorCode::PlanRejected),
        (RtStatus::ERR_MAGIC, WsErrorCode::PlanRejected),
        (RtStatus::RT_E_INPUT_CLIP, WsErrorCode::RuntimeFault),
        (RtStatus::RT_E_WORKER_LOST, WsErrorCode::RuntimeFault),
        (RtStatus::OK, WsErrorCode::Internal),
    ];
    for (status, expected) in cases {
        let mapped = WsOpError::from_rt_status(status, "prepare");
        assert_eq!(mapped.code(), expected, "status {status:?}");
        assert!(mapped.message().contains("rt_status="));
    }
}

#[test]
fn ws_ack_mask_helpers() {
    assert_eq!(ack_mask_for_workers(2), 0b11);
    assert_eq!(ack_mask_for_workers(4), 0b1111);
    assert_eq!(required_ack_mask(2), Ok(0b11));
    assert_eq!(required_ack_mask(4), Ok(0b1111));
    assert_eq!(
        required_ack_mask(3).map_err(|e| e.code()),
        Err(WsErrorCode::Internal)
    );
    assert_eq!(
        required_ack_mask(1).map_err(|e| e.code()),
        Err(WsErrorCode::Internal)
    );
}

#[test]
fn ws_validate_commit_ack_helper_accepts_and_rejects() {
    let prepared = good_prepared(8, 4);
    let ack = good_ack(&prepared);
    assert!(validate_commit_ack(&prepared, &ack).is_ok());
    for mutate in [
        ack_zero_blocks,
        ack_partial,
        ack_extra_bit,
        ack_version_mismatch,
        ack_graph_mismatch,
        ack_config_mismatch,
        ack_rolled_back,
        ack_no_advance,
        ack_workers_mismatch,
    ] {
        let bad = mutate(&prepared);
        let err = validate_commit_ack(&prepared, &bad).expect_err("must reject mutilated ack");
        assert_eq!(err.code(), WsErrorCode::CommitNotAcked);
    }
    // prepared 本身形状非法 -> internal，而不是 commit_not_acked。
    let mut broken = prepared.clone();
    broken.graph_id_hex = "zz".to_string();
    let err = validate_commit_ack(&broken, &ack).expect_err("must reject bad prepared");
    assert_eq!(err.code(), WsErrorCode::Internal);
}

#[test]
fn ws_prepared_config_validate_contract() {
    good_prepared(8, 4).validate().expect("valid prepared");
    let cases: Vec<PreparedConfig> = vec![
        {
            let mut p = good_prepared(0, 4);
            p.plan_version = 0;
            p
        },
        good_prepared(8, 3),
        {
            let mut p = good_prepared(8, 4);
            p.graph_id_hex = hex64('A'); // 大写 hex 不接受
            p
        },
        {
            let mut p = good_prepared(8, 4);
            p.config_sha256_hex = String::new();
            p
        },
    ];
    for case in cases {
        assert_eq!(
            case.validate().map_err(|e| e.code()),
            Err(WsErrorCode::Internal)
        );
    }
}

#[test]
fn ws_actual_plan_validate_contract() {
    good_actual(7).validate().expect("valid actual plan");
    ActualPlan::none().validate().expect("empty actual plan");
    let bad = ActualPlan {
        plan_version: 0,
        graph_id_hex: hex64('a'),
        config_sha256_hex: String::new(),
    };
    assert_eq!(
        bad.validate().map_err(|e| e.code()),
        Err(WsErrorCode::Internal)
    );
}

#[test]
fn ws_is_sha256_hex_contract() {
    assert!(is_sha256_hex(&hex64('a')));
    assert!(is_sha256_hex(&hex64('0')));
    assert!(!is_sha256_hex(""));
    assert!(!is_sha256_hex(&hex64('A')));
    assert!(!is_sha256_hex(&"a".repeat(63)));
    assert!(!is_sha256_hex(&"z".repeat(64)));
}

#[test]
fn ws_ack_json_helpers_shape() {
    let prepared = good_prepared(8, 4);
    let prepare_json = prepare_ack_json(&prepared);
    assert_eq!(
        prepare_json.get("prepared").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        prepare_json.get("plan_version").and_then(|v| v.as_u64()),
        Some(8)
    );
    assert_eq!(
        prepare_json
            .get("expected_workers")
            .and_then(|v| v.as_u64()),
        Some(4)
    );
    assert_eq!(
        prepare_json.get("admitted_bytes").and_then(|v| v.as_u64()),
        Some(4096)
    );

    let ack = good_ack(&prepared);
    let ack_json = commit_ack_json(&ack);
    assert_eq!(
        ack_json.get("applied").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        ack_json.get("blocks_committed").and_then(|v| v.as_u64()),
        Some(3)
    );
    assert_eq!(ack_json.get("ack_mask").and_then(|v| v.as_u64()), Some(15));
    assert_eq!(
        ack_json.get("switched_at_block").and_then(|v| v.as_u64()),
        Some(42)
    );

    let actual_json = actual_plan_json(&good_actual(9));
    assert_eq!(
        actual_json.get("plan_version").and_then(|v| v.as_u64()),
        Some(9)
    );
    assert_eq!(
        actual_json.get("config_sha256").and_then(|v| v.as_str()),
        Some(hex64('d').as_str())
    );
}

// ---------------------------------------------------------------------------
// JSON 编解码 / 请求解码 / 限制 / 统计（协议基础，W02/W03/W21）
// ---------------------------------------------------------------------------

#[test]
fn ws_json_parse_and_encode_roundtrip() {
    let text = "{\"a\":1,\"b\":[true,false,null],\"c\":\"x\",\"d\":-2.5}";
    let value = json_parse(text).expect("parse");
    let encoded = json_encode(&value).expect("encode");
    let reparsed = json_parse(&encoded).expect("reparse");
    assert_eq!(reparsed.get("a").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(
        reparsed
            .get("b")
            .and_then(|v| v.as_array())
            .map(|a| a.len()),
        Some(3)
    );
    assert_eq!(reparsed.get("c").and_then(|v| v.as_str()), Some("x"));
    assert_eq!(reparsed.get("d").and_then(|v| v.as_f64()), Some(-2.5));
}

#[test]
fn ws_json_parse_rejects_bad_number_forms() {
    for text in [
        "01",
        "1.",
        "-",
        "1e",
        "+1",
        "NaN",
        "Infinity",
        "-Infinity",
        "1e999",
    ] {
        assert!(json_parse(text).is_err(), "must reject {text}");
    }
}

#[test]
fn ws_json_parse_string_escapes_and_unicode() {
    let value = json_parse("\"quote:\\\" slash:\\\\ nl:\\n tab:\\t u:A \\uD83D\\uDE00\"")
        .expect("parse escapes");
    let text = value.as_str().expect("string");
    assert!(text.contains("quote:\""));
    assert!(text.contains("nl:\n"));
    assert!(text.contains('A'));
    assert!(text.contains('😀'));
    assert!(
        json_parse("\"\\uD800\"").is_err(),
        "lone high surrogate rejected"
    );
    assert!(
        json_parse("\"\\uDC00\"").is_err(),
        "lone low surrogate rejected"
    );
    assert!(json_parse("\"\\q\"").is_err());
}

#[test]
fn ws_json_parse_rejects_trailing_and_control() {
    assert!(json_parse("{} x").is_err());
    assert!(json_parse("\"a\"\"b\"").is_err());
    assert!(json_parse("\"line\nbreak\"").is_err());
    assert!(json_parse("{,}").is_err());
    assert!(json_parse("[1,]").is_err());
}

#[test]
fn ws_json_encode_rejects_nonfinite_numbers() {
    let nan = JsonValue::Number(f64::NAN);
    assert!(json_encode(&nan).is_err());
    let inf = JsonValue::Number(f64::INFINITY);
    assert!(json_encode(&inf).is_err());
    let neg_inf = JsonValue::Number(f64::NEG_INFINITY);
    assert!(json_encode(&neg_inf).is_err());
}

#[test]
fn ws_json_encode_number_forms() {
    assert_eq!(json_encode(&JsonValue::Number(-0.0)).unwrap(), "0");
    assert_eq!(json_encode(&JsonValue::Number(48000.0)).unwrap(), "48000");
    assert_eq!(json_encode(&JsonValue::Number(-12.5)).unwrap(), "-12.5");
    assert_eq!(
        json_encode(&JsonValue::UInt(9_007_199_254_741_993)).unwrap(),
        "9007199254741993"
    );
    assert_eq!(json_encode(&JsonValue::Int(-256)).unwrap(), "-256");
    let tiny = json_encode(&JsonValue::Number(1e-7)).unwrap();
    assert!(
        json_parse(&tiny).is_ok(),
        "tiny number must stay valid JSON: {tiny}"
    );
}

#[test]
fn ws_json_encode_string_escaping() {
    let value = JsonValue::String("a\"b\\c\nd\te".to_string());
    let encoded = json_encode(&value).expect("encode string");
    let reparsed = json_parse(&encoded).expect("reparse string");
    assert_eq!(reparsed.as_str(), Some("a\"b\\c\nd\te"));
}

#[test]
fn ws_json_value_accessors() {
    let value =
        json_parse("{\"n\":12,\"f\":1.5,\"u\":7,\"b\":true,\"s\":\"x\",\"a\":[],\"z\":null}")
            .expect("parse");
    assert_eq!(value.get("n").and_then(|v| v.as_i64()), Some(12));
    assert_eq!(value.get("f").and_then(|v| v.as_f64()), Some(1.5));
    assert_eq!(value.get("u").and_then(|v| v.as_u64()), Some(7));
    assert_eq!(value.get("b").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(value.get("s").and_then(|v| v.as_str()), Some("x"));
    assert_eq!(
        value.get("a").and_then(|v| v.as_array()).map(|a| a.len()),
        Some(0)
    );
    assert!(value.get("z").expect("z").is_null());
    assert_eq!(value.get("n").and_then(|v| v.as_u64()), Some(12));
    assert_eq!(value.get("fid").and_then(|v| v.as_i64()), None);
    assert_eq!(JsonValue::Int(-3).as_u64(), None);
    assert_eq!(JsonValue::UInt(3).as_i64(), Some(3));
    assert_eq!(JsonValue::Bool(true).as_f64(), None);
    let object = json_object(vec![("k", JsonValue::Int(1))]);
    assert_eq!(object.get("k").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(object.get("missing"), None);
}

#[test]
fn ws_decode_request_forms() {
    let value = json_parse("\"GetState\"").expect("parse");
    let request = decode_request(&value).expect("string request");
    assert_eq!(request.command, "GetState");
    assert!(request.payload.is_none());
    assert!(request.payload_or_null().is_null());

    let value = json_parse("{\"SetVolume\":-9.5}").expect("parse");
    let request = decode_request(&value).expect("object request");
    assert_eq!(request.command, "SetVolume");
    assert_eq!(request.payload_or_null().as_f64(), Some(-9.5));
}

#[test]
fn ws_decode_request_rejections() {
    let cases = [
        ("{}", WsErrorCode::InvalidRequest),
        ("{\"a\":1,\"b\":2}", WsErrorCode::InvalidRequest),
        ("[]", WsErrorCode::InvalidRequest),
        ("42", WsErrorCode::InvalidRequest),
        ("null", WsErrorCode::InvalidRequest),
        ("\"bad name\"", WsErrorCode::InvalidRequest),
        ("{\"\":1}", WsErrorCode::InvalidRequest),
    ];
    for (text, expected) in cases {
        let value = json_parse(text).expect("parse");
        let error = decode_request(&value).expect_err("must reject");
        assert_eq!(error.code(), expected, "input {text}");
    }
}

#[test]
fn ws_command_name_validation() {
    assert!(is_valid_command_name("GetState"));
    assert!(is_valid_command_name("A1"));
    assert!(!is_valid_command_name(""));
    assert!(!is_valid_command_name("Get State"));
    assert!(!is_valid_command_name("Get-State"));
    assert!(!is_valid_command_name(&"A".repeat(65)));
}

#[test]
fn ws_limits_defaults() {
    let limits = WsLimits::default();
    assert_eq!(limits.max_frame_bytes, DEFAULT_MAX_FRAME_BYTES);
    assert_eq!(limits.max_config_bytes, MAX_CONFIG_BYTES);
    assert!(limits.max_frame_bytes > limits.max_config_bytes);
    assert_eq!(MAX_CONFIG_BYTES, 2 * 1024 * 1024);
    let custom = WsLimits::new(10, 5);
    assert_eq!(custom.max_frame_bytes, 10);
    assert_eq!(custom.max_config_bytes, 5);
}

#[test]
fn ws_session_default_uses_default_limits() {
    let session = WsSession::default();
    assert_eq!(session.limits(), WsLimits::default());
    assert!(!session.is_closed());
    assert!(session.pending_prepare().is_none());
    assert_eq!(session.stats(), WsSessionStats::default());
}

#[test]
fn ws_session_stats_counters() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let _ = session.handle_frame(&mut plane, WsFrame::Ping(b"p"));
    let _ = session.handle_frame(&mut plane, WsFrame::Pong(b"p"));
    let _ = session.handle_frame(&mut plane, WsFrame::Binary(b"x"));
    let _ = session.handle_text(&mut plane, "\"GetState\"");
    let _ = session.handle_text(&mut plane, "\"NoSuch\"");
    let _ = session.handle_text(&mut plane, "###");
    let stats = session.stats();
    assert_eq!(stats.frames, 6);
    assert_eq!(stats.ping_frames, 1);
    assert_eq!(stats.pong_frames, 1);
    assert_eq!(stats.binary_frames, 1);
    assert_eq!(stats.text_frames, 3);
    assert_eq!(stats.commands_ok, 1);
    assert_eq!(stats.commands_err, 1);
    assert_eq!(stats.unknown_commands, 1);
    assert_eq!(stats.invalid_frames, 2);
}

#[test]
fn ws_session_closed_ignores_frames() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    let _ = session.handle_frame(&mut plane, WsFrame::Close);
    assert!(session.is_closed());
    assert_eq!(
        session.handle_frame(&mut plane, WsFrame::Text("\"GetState\"")),
        WsReply::None
    );
    assert_eq!(
        session.handle_frame(&mut plane, WsFrame::Ping(b"x")),
        WsReply::None
    );
    assert_eq!(
        session.handle_bytes(&mut plane, b"\"GetState\""),
        WsReply::None
    );
}

#[test]
fn ws_session_abort_pending_is_idempotent() {
    let mut session = new_session();
    let mut plane = FakePlane::new();
    session.abort_pending(&mut plane).expect("no pending is ok");
    assert_eq!(plane.abort_calls, 0);
    let _ = send(&mut session, &mut plane, &prepare_config_request(CONFIG));
    session.abort_pending(&mut plane).expect("abort pending");
    assert_eq!(plane.abort_calls, 1);
    session
        .abort_pending(&mut plane)
        .expect("second abort is ok");
    assert_eq!(plane.abort_calls, 1);
}

// ---------------------------------------------------------------------------
// 与 T42 REST 的接口一致性（同一判据 / 同一 RtStatus 映射，不两套口径）
// ---------------------------------------------------------------------------

#[test]
fn ws_error_code_rest_equivalence_mapping() {
    // T42 rest.rs 稳定码（crates/control/src/rest.rs 的 CODE_*）对照表；WS 侧不得自造分叉命名。
    let expected = [
        (WsErrorCode::InvalidJson, Some("invalid_json")),
        (WsErrorCode::InvalidRequest, Some("invalid_field")),
        (WsErrorCode::InvalidArgument, Some("invalid_field")),
        (WsErrorCode::InvalidFrameType, None),
        (WsErrorCode::UnknownCommand, None),
        (WsErrorCode::UnknownFader, Some("invalid_field")),
        (WsErrorCode::PayloadTooLarge, Some("payload_too_large")),
        (WsErrorCode::Unsupported, Some("unsupported")),
        (WsErrorCode::PrepareBusy, Some("busy")),
        (WsErrorCode::NotPrepared, Some("not_prepared")),
        (WsErrorCode::PrepareRejected, Some("invalid_plan")),
        (WsErrorCode::PlanRejected, Some("invalid_plan")),
        (WsErrorCode::AdmissionRejected, Some("admission_failed")),
        (WsErrorCode::StalePlan, Some("stale")),
        (WsErrorCode::SwitchBusy, Some("switch_busy")),
        (
            WsErrorCode::QuarantineExhausted,
            Some("quarantine_exhausted"),
        ),
        (WsErrorCode::DeviceBusy, Some("device_busy")),
        (WsErrorCode::RolledBack, Some("rolled_back")),
        (WsErrorCode::CommitNotAcked, Some("internal_error")),
        (WsErrorCode::RuntimeFault, Some("internal_error")),
        (WsErrorCode::Internal, Some("internal_error")),
    ];
    assert_eq!(
        expected.len(),
        WsErrorCode::ALL.len(),
        "mapping table covers all codes"
    );
    for (code, rest) in expected {
        assert_eq!(rest_equivalent(code), rest, "mapping for {code}");
        let mapped = rest_equivalent(code);
        if let Some(text) = mapped {
            assert!(!text.is_empty());
            assert!(
                !text.contains(' '),
                "REST code must be a stable token: {text}"
            );
        }
    }
    // T42 的 RtStatus 映射与 WS 侧 from_rt_status 从同一 §9.2 派生（抽样核对关键码）。
    assert_eq!(
        WsOpError::from_rt_status(RtStatus::ERR_PLAN_STALE, "prepare").code(),
        WsErrorCode::StalePlan
    );
    assert_eq!(
        rest_equivalent(WsOpError::from_rt_status(RtStatus::ERR_PLAN_STALE, "x").code()),
        Some("stale")
    );
    assert_eq!(
        rest_equivalent(WsOpError::from_rt_status(RtStatus::ERR_DEVICE_BUSY, "x").code()),
        Some("device_busy")
    );
    assert_eq!(
        rest_equivalent(WsOpError::from_rt_status(RtStatus::ERR_ADMISSION_MEMORY, "x").code()),
        Some("admission_failed")
    );
    // F-30：模式回滚不复用 119（stale），WS/REST 都用 rolled_back 稳定码。
    let rolled_back =
        WsOpError::from_rt_status(RtStatus::from_code(RT_STATUS_MODE_ROLLED_BACK), "mode");
    assert_eq!(rolled_back.code(), WsErrorCode::RolledBack);
    assert_eq!(rest_equivalent(rolled_back.code()), Some("rolled_back"));
    assert_ne!(
        RtStatus::from_code(RT_STATUS_MODE_ROLLED_BACK),
        RtStatus::ERR_PLAN_STALE
    );
}
