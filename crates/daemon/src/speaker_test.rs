//! 通道识别测试音代理（docs/API.md §12.2/§12.6；SPEAKER-MAPPING P3.5）。
//!
//! 职责（daemon 只做控制面：校验 + 门禁 + 转发，出声在 rt-host 数据面）：
//! - POST /api/speaker-test -> 转发 rt-host op test_tone；POST /api/speaker-test/stop -> test_tone_stop。
//!   测试音直接写 rt-host 原始 DAC：不经过 plan routing、不写 CueMix、不改图/状态（§12.4）。
//! - 服务端安全门：KHAEN_SPEAKER_TEST=on 才启用（否则 503 not_wired + 原因）；
//!   KHAEN_SOUND_WINDOW（如 "08:30-12:00,14:30-18:00"）配置时窗口外 403 sound_window_closed；
//!   忙（校准 job / rt-host busy）409；rt-host 未接线/not_applied 503；链路不可用 503
//!   host_link_unavailable（绝不假成功）。
//! - audible=true 且 confirm=true 必须同时显式给出；dac 0..=MAX_OUTPUT_CHANNELS-1（协议上限；
//!   实际输出设备更窄时由 rt-host 按 format.channels 拒绝，不假设 MOTU 22 通道）、ms 500..=10000、
//!   db -60..=-12（默认 -30）在 daemon 二次校验（rt-host 自己也校验）。
//!
//! 时间来源可注入（SpeakerTestRuntime::with_clock）；窗口判断是纯函数（SoundWindow），
//! 离线测试不依赖真实时钟。跨午夜窗口（start > end）按"环绕"处理：m >= start 或 m < end。

use std::sync::Arc;
use std::time::SystemTime;

use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Map, Value};

use crate::api_error::{read_json_body, ApiError};
use crate::host_link::{LinkError, Target, WireReply};
use crate::status;
use crate::DaemonState;

/// KHAEN_SPEAKER_TEST 环境变量名。
pub const ENABLED_ENV: &str = "KHAEN_SPEAKER_TEST";
/// KHAEN_SOUND_WINDOW 环境变量名。
pub const WINDOW_ENV: &str = "KHAEN_SOUND_WINDOW";

/// 协议允许的最大 DAC 数（`khaen_core::rt::MAX_OUTPUT_CHANNELS`）。
///
/// 这不是“设备一定有 22 路”：具体设备可能更窄，rt-host 会按它真实的 `format.channels`
/// 拒绝越界 DAC 并透传；daemon 只做协议上限校验，不把 MOTU 22 通道写死成设备假设。
pub const DAC_CHANNELS: usize = khaen_core::rt::MAX_OUTPUT_CHANNELS;
/// 测试音最短时长（毫秒）。
pub const MIN_MS: u64 = 500;
/// 测试音最长时长（毫秒；硬上限 10 s，与出声纪律一致）。
pub const MAX_MS: u64 = 10_000;
/// 默认电平（dBFS，很轻，有声即可）。
pub const DEFAULT_DB: f32 = -30.0;
/// 最轻电平（dBFS）。
pub const MIN_DB: f32 = -60.0;
/// 最响电平（dBFS；硬上限）。
pub const MAX_DB: f32 = -12.0;
/// 正弦测试音最低频率（Hz；与 rt-host 执行侧契约一致）。
pub const MIN_SINE_HZ: f64 = 100.0;
/// 正弦测试音最高频率（Hz）。
pub const MAX_SINE_HZ: f64 = 10_000.0;

/// 未配置 KHAEN_SPEAKER_TEST 时的稳定原因（503 not_wired）。
pub const NOT_WIRED_REASON: &str = "KHAEN_SPEAKER_TEST is not enabled: the speaker test output is disabled by configuration, so no audible test tone is played (set KHAEN_SPEAKER_TEST=on to enable it; the server-side gate is mandatory)";
/// 窗口外 403 的稳定文案。
pub const WINDOW_CLOSED_REASON: &str = "the speaker test sound window is closed (KHAEN_SOUND_WINDOW is configured and the current local time is outside it): no test tone is played";
/// process 目标（daemon 进程内引擎）没有数据面测试音路径。
pub const PROCESS_TARGET_REASON: &str = "KHAEN_RT_TARGET=process: control ops act on the daemon process-internal engine, which has no data-plane ALSA output; no test tone can be played, so the request is refused instead of pretending success";

/// 测试音种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestToneKind {
    /// 带限粉噪（rt-host 用 500 Hz - 4 kHz 带限噪声）。
    Noise,
    /// 单频正弦。
    Sine,
}

impl TestToneKind {
    /// 稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Noise => "noise",
            Self::Sine => "sine",
        }
    }

    /// 解析（未知值 None）。
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "noise" => Some(Self::Noise),
            "sine" => Some(Self::Sine),
            _ => None,
        }
    }
}

/// 已校验的测试音请求（daemon 二次校验后的口径）。
#[derive(Clone, Debug, PartialEq)]
pub struct TestToneRequest {
    /// 目标 DAC（0..=`MAX_OUTPUT_CHANNELS-1`；设备更窄时由 rt-host 拒绝）。
    pub dac: u16,
    /// 时长（毫秒，500..=10000）。
    pub ms: u64,
    /// 种类（默认 noise）。
    pub kind: TestToneKind,
    /// 电平（dBFS，-60..=-12，默认 -30）。
    pub level_db: f32,
    /// 正弦频率（只有 kind=sine；缺省由 rt-host 用 1 kHz）。
    pub freq_hz: Option<f64>,
}

// ---------------------------------------------------------------------------
// 声音窗口（纯函数 + 可注入当前时间）
// ---------------------------------------------------------------------------

/// 声音窗口（分钟数区间；多段 OR；start > end 视为跨午夜环绕）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SoundWindow {
    windows: Vec<(u32, u32)>,
    spec: String,
}

impl SoundWindow {
    /// 解析 "08:30-12:00,14:30-18:00"（也支持跨午夜 "22:00-06:00"；白空格忽略）。
    pub fn parse(spec: &str) -> Result<Self, String> {
        let trimmed = spec.trim();
        if trimmed.is_empty() {
            return Err("KHAEN_SOUND_WINDOW is empty".to_string());
        }
        let mut windows = Vec::new();
        for raw_segment in trimmed.split(',') {
            let segment = raw_segment.trim();
            if segment.is_empty() {
                return Err(format!(
                    "KHAEN_SOUND_WINDOW has an empty segment: {spec:?} (expected HH:MM-HH:MM[,HH:MM-HH:MM...])"
                ));
            }
            let mut parts = segment.split('-');
            let (Some(start), Some(end), None) = (parts.next(), parts.next(), parts.next()) else {
                return Err(format!(
                    "KHAEN_SOUND_WINDOW segment {segment:?} must be HH:MM-HH:MM"
                ));
            };
            let start = parse_hhmm(start.trim()).ok_or_else(|| {
                format!("KHAEN_SOUND_WINDOW segment {segment:?}: invalid start time (expected HH:MM, 00:00..23:59)")
            })?;
            let end = parse_hhmm(end.trim()).ok_or_else(|| {
                format!("KHAEN_SOUND_WINDOW segment {segment:?}: invalid end time (expected HH:MM, 00:00..23:59)")
            })?;
            if start == end {
                return Err(format!(
                    "KHAEN_SOUND_WINDOW segment {segment:?} is zero-length: the first time is inclusive and the second is exclusive; omit KHAEN_SOUND_WINDOW to allow all day"
                ));
            }
            windows.push((start, end));
        }
        Ok(Self {
            windows,
            spec: trimmed.to_string(),
        })
    }

    /// 当前分钟数（0..1440，本地墙钟）是否在窗口内。
    ///
    /// 区间语义：[start, end)；跨午夜（start > end）表示 m >= start 或 m < end。
    pub fn contains(&self, minute_of_day: u32) -> bool {
        if minute_of_day >= 24 * 60 {
            return false;
        }
        self.windows.iter().any(|(start, end)| {
            if start < end {
                minute_of_day >= *start && minute_of_day < *end
            } else {
                minute_of_day >= *start || minute_of_day < *end
            }
        })
    }

    /// 原始配置（原因文案/响应展示用）。
    pub fn spec(&self) -> &str {
        &self.spec
    }

    /// 区间列表（未排序；测试/诊断用）。
    pub fn windows(&self) -> &[(u32, u32)] {
        &self.windows
    }
}

/// 解析 HH:MM 为当天分钟数（小时 0..=23、分钟 0..=59）。
pub fn parse_hhmm(text: &str) -> Option<u32> {
    let mut parts = text.split(':');
    let (Some(hour), Some(minute), None) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    let hour: u32 = hour.trim().parse().ok()?;
    let minute: u32 = minute.trim().parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    Some(hour * 60 + minute)
}

/// 服务端安全门判定结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gate {
    /// 允许出声。
    Open,
    /// KHAEN_SPEAKER_TEST 未开启。
    Disabled {
        /// 稳定原因。
        reason: String,
    },
    /// 配置了窗口且当前在窗口外。
    WindowClosed {
        /// 原始窗口配置。
        spec: String,
    },
    /// 配置了窗口但本地墙钟时间读不到（fail-closed）。
    ClockUnavailable {
        /// 人读原因。
        reason: String,
    },
}

/// 测试音运行时配置（启动时由 main 从环境变量解析；handler 不读环境）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeakerTestConfig {
    enabled: bool,
    window: Option<SoundWindow>,
    disabled_reason: String,
}

impl SpeakerTestConfig {
    /// 未配置/显式关闭时的默认配置（DaemonState 默认；测试也用）。
    pub fn disabled_default() -> Self {
        Self {
            enabled: false,
            window: None,
            disabled_reason: NOT_WIRED_REASON.to_string(),
        }
    }

    /// 纯解析（测试不读进程环境）：enabled 原始值 + window 原始值。
    pub fn parse(enabled_raw: Option<&str>, window_raw: Option<&str>) -> Result<Self, ConfigError> {
        let enabled_state = enabled_raw.unwrap_or("unset");
        let enabled = match enabled_raw.map(str::trim) {
            None | Some("") => false,
            Some(value)
                if value.eq_ignore_ascii_case("on")
                    || value == "1"
                    || value.eq_ignore_ascii_case("true") =>
            {
                true
            }
            Some(value)
                if value.eq_ignore_ascii_case("off")
                    || value == "0"
                    || value.eq_ignore_ascii_case("false") =>
            {
                false
            }
            Some(value) => {
                return Err(ConfigError::InvalidEnabled {
                    value: value.to_string(),
                })
            }
        };
        let window = match window_raw.map(str::trim) {
            None | Some("") => None,
            Some(spec) => {
                Some(
                    SoundWindow::parse(spec).map_err(|reason| ConfigError::InvalidWindow {
                        spec: spec.to_string(),
                        reason,
                    })?,
                )
            }
        };
        let disabled_reason = if enabled {
            String::new()
        } else {
            format!(
                "the speaker test output is disabled by configuration ({ENABLED_ENV}={enabled_state}): set {ENABLED_ENV}=on to allow audible speaker test tones (the server-side gate is mandatory, so nothing is played now)"
            )
        };
        Ok(Self {
            enabled,
            window,
            disabled_reason,
        })
    }

    /// 从进程环境解析（唯一读这些变量的地方之一：main；handler 不读环境）。
    pub fn from_env() -> Result<Self, ConfigError> {
        let enabled = std::env::var(ENABLED_ENV).ok();
        let window = std::env::var(WINDOW_ENV).ok();
        Self::parse(enabled.as_deref(), window.as_deref())
    }

    /// 是否启用。
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// 配置的窗口。
    pub fn window(&self) -> Option<&SoundWindow> {
        self.window.as_ref()
    }

    /// 未启用时的稳定原因。
    pub fn disabled_reason(&self) -> &str {
        &self.disabled_reason
    }

    /// 只用配置做门禁判定（时间由调用方给；纯函数，便于离线测试）。
    pub fn gate_at(&self, minute_of_day: Option<u32>) -> Gate {
        if !self.enabled {
            return Gate::Disabled {
                reason: self.disabled_reason.clone(),
            };
        }
        let Some(window) = &self.window else {
            return Gate::Open;
        };
        match minute_of_day {
            Some(minute) if window.contains(minute) => Gate::Open,
            Some(_) => Gate::WindowClosed {
                spec: window.spec().to_string(),
            },
            None => Gate::ClockUnavailable {
                reason: "the local wall-clock time is unavailable".to_string(),
            },
        }
    }
}

/// 配置错误（非法值启动即失败，与 bind/host_link/motu 同样的 fail-closed 策略）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// KHAEN_SPEAKER_TEST 不是 on/off 类取值。
    InvalidEnabled {
        /// 原始值。
        value: String,
    },
    /// KHAEN_SOUND_WINDOW 解析失败。
    InvalidWindow {
        /// 原始值。
        spec: String,
        /// 解析器给的原因。
        reason: String,
    },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidEnabled { value } => write!(
                formatter,
                "invalid {ENABLED_ENV} value {value:?}: expected on/1/true (enable) or off/0/false/unset (disable)"
            ),
            Self::InvalidWindow { spec, reason } => {
                write!(formatter, "invalid {WINDOW_ENV} value {spec:?}: {reason}")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// 可注入的本地墙钟（分钟数 0..1440）。
pub type ClockFn = Arc<dyn Fn() -> Result<u32, String> + Send + Sync>;

/// 测试音运行时（配置 + 可注入墙钟；DaemonState 持有）。
#[derive(Clone)]
pub struct SpeakerTestRuntime {
    config: SpeakerTestConfig,
    clock: ClockFn,
    clock_source: &'static str,
}

impl std::fmt::Debug for SpeakerTestRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SpeakerTestRuntime")
            .field("config", &self.config)
            .field("clock_source", &self.clock_source)
            .finish()
    }
}

impl SpeakerTestRuntime {
    /// 生产构造：墙钟取本地时间（unix 用 date +%H:%M；其它平台如实不可用）。
    pub fn new(config: SpeakerTestConfig) -> Self {
        Self {
            config,
            clock: production_clock(),
            clock_source: if cfg!(unix) {
                "unix-date-local"
            } else {
                "unavailable"
            },
        }
    }

    /// 测试注入墙钟（不读真实时钟/不依赖平台）。
    pub fn with_clock(config: SpeakerTestConfig, clock: ClockFn) -> Self {
        Self {
            config,
            clock,
            clock_source: "injected",
        }
    }

    /// 配置。
    pub fn config(&self) -> &SpeakerTestConfig {
        &self.config
    }

    /// 当前本地分钟数。
    pub fn now_minute(&self) -> Result<u32, String> {
        (self.clock)()
    }

    /// 当前门禁判定（配置 + 实际墙钟）。
    pub fn gate_now(&self) -> Gate {
        if !self.config.enabled {
            return Gate::Disabled {
                reason: self.config.disabled_reason.clone(),
            };
        }
        if self.config.window.is_none() {
            return Gate::Open;
        }
        match self.now_minute() {
            Ok(minute) => self.config.gate_at(Some(minute)),
            Err(reason) => Gate::ClockUnavailable { reason },
        }
    }
}

/// 生产墙钟：unix 用 date +%H:%M 取本地时间（std 没有本地时区 API）；其它平台如实不可用。
fn production_clock() -> ClockFn {
    if cfg!(unix) {
        Arc::new(|| {
            let output = std::process::Command::new("date")
                .arg("+%H:%M")
                .output()
                .map_err(|error| {
                    format!("cannot read the local wall-clock time (date +%H:%M failed): {error}")
                })?;
            if !output.status.success() {
                return Err(format!(
                    "cannot read the local wall-clock time (date +%H:%M exited with {})",
                    output.status
                ));
            }
            let text = String::from_utf8_lossy(&output.stdout);
            parse_hhmm(text.trim()).ok_or_else(|| {
                format!(
                    "unexpected local time output from date +%H:%M: {:?}",
                    text.trim()
                )
            })
        })
    } else {
        Arc::new(|| {
            Err("the local wall-clock time is not available on this platform (only unix date +%H:%M is wired): the KHAEN_SOUND_WINDOW gate cannot be evaluated".to_string())
        })
    }
}

// ---------------------------------------------------------------------------
// 请求校验（纯函数）
// ---------------------------------------------------------------------------

/// 解析并二次校验 POST /api/speaker-test 请求（400 bad_request）。
pub fn parse_request(object: &Map<String, Value>) -> Result<TestToneRequest, ApiError> {
    // audible=true 且 confirm=true 必须同时显式给出（不默认同意出声）。
    let mut missing = Vec::new();
    for name in ["audible", "confirm"] {
        match object.get(name) {
            Some(Value::Bool(true)) => {}
            Some(Value::Bool(false)) => {
                return Err(ApiError::bad_request(format!(
                    "field {name} must be true: a speaker test tone is audible, so the request must acknowledge it explicitly"
                ))
                .with_detail("field", json!(name)))
            }
            Some(_) => {
                return Err(ApiError::invalid_field(
                    name,
                    format!("field {name} must be a boolean"),
                ))
            }
            None => missing.push(name),
        }
    }
    if !missing.is_empty() {
        return Err(ApiError::bad_request(
            "explicit confirmation is required to play a test tone: send audible=true and confirm=true",
        )
        .with_detail("fields", json!(missing)));
    }

    let dac = integer_field(object, "dac")?;
    if dac >= DAC_CHANNELS as u64 {
        return Err(ApiError::invalid_field(
            "dac",
            format!(
                "field dac must be in 0..={} (the protocol maximum; the actual output device may report fewer channels, and rt-host rejects DACs beyond format.channels)",
                DAC_CHANNELS - 1
            ),
        ));
    }
    let dac = u16::try_from(dac).expect("dac range checked");
    let ms = integer_field(object, "ms")?;
    if !(MIN_MS..=MAX_MS).contains(&ms) {
        return Err(ApiError::invalid_field(
            "ms",
            format!(
                "field ms must be in {MIN_MS}..={MAX_MS} milliseconds (at most 10 s of continuous sound)"
            ),
        ));
    }
    let kind = match object.get("kind") {
        None | Some(Value::Null) => TestToneKind::Noise,
        Some(Value::String(text)) => TestToneKind::parse(text).ok_or_else(|| {
            ApiError::invalid_field(
                "kind",
                format!("field kind must be noise or sine (got {text:?})"),
            )
        })?,
        Some(_) => {
            return Err(ApiError::invalid_field(
                "kind",
                "field kind must be a string",
            ))
        }
    };
    let level_db = match object.get("db") {
        None | Some(Value::Null) => DEFAULT_DB,
        Some(Value::Number(number)) => number
            .as_f64()
            .filter(|value| value.is_finite())
            .map(|value| value as f32)
            .filter(|value| value.is_finite())
            .ok_or_else(|| ApiError::invalid_field("db", "field db must be a finite number"))?,
        Some(_) => return Err(ApiError::invalid_field("db", "field db must be a number")),
    };
    if !(MIN_DB..=MAX_DB).contains(&level_db) {
        return Err(ApiError::invalid_field(
            "db",
            format!("field db must be in {MIN_DB}..={MAX_DB} dBFS (default {DEFAULT_DB})"),
        ));
    }
    let freq_hz = match object.get("freq_hz") {
        None | Some(Value::Null) => None,
        Some(Value::Number(number)) => Some(
            number
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or_else(|| {
                    ApiError::invalid_field("freq_hz", "field freq_hz must be a finite number")
                })?,
        ),
        Some(_) => {
            return Err(ApiError::invalid_field(
                "freq_hz",
                "field freq_hz must be a number",
            ))
        }
    };
    let freq_hz = match kind {
        // 噪声是固定的 500 Hz - 4 kHz 带限噪声：freq_hz 对它无意义，忽略（宽容）。
        TestToneKind::Noise => None,
        TestToneKind::Sine => match freq_hz {
            Some(value) if !(MIN_SINE_HZ..=MAX_SINE_HZ).contains(&value) => {
                return Err(ApiError::invalid_field(
                    "freq_hz",
                    format!("field freq_hz must be in {MIN_SINE_HZ}..={MAX_SINE_HZ} Hz"),
                ))
            }
            other => other,
        },
    };
    Ok(TestToneRequest {
        dac,
        ms,
        kind,
        level_db,
        freq_hz,
    })
}

fn integer_field(object: &Map<String, Value>, name: &str) -> Result<u64, ApiError> {
    match object.get(name) {
        Some(Value::Number(number)) => number.as_u64().ok_or_else(|| {
            ApiError::invalid_field(name, format!("field {name} must be a non-negative integer"))
        }),
        Some(_) => Err(ApiError::invalid_field(
            name,
            format!("field {name} must be a non-negative integer"),
        )),
        None => Err(ApiError::invalid_field(
            name,
            format!("missing field: {name}"),
        )),
    }
}

// ---------------------------------------------------------------------------
// /api/status 的 speaker_test 投影
// ---------------------------------------------------------------------------

/// speaker_test 全 null 形状（不可读时使用，不编 false）。
pub fn unavailable_status() -> Value {
    json!({
        "active": Value::Null,
        "dac": Value::Null,
        "remaining_ms": Value::Null,
        "kind": Value::Null,
    })
}

/// 把 rt-host 的 speaker_test 对象规范化成 §12.2 的加法字段（缺字段填 null）。
pub fn normalize_status(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    let active = object.get("active")?.as_bool()?;
    Some(json!({
        "active": active,
        "dac": object.get("dac").cloned().unwrap_or(Value::Null),
        "remaining_ms": object.get("remaining_ms").cloned().unwrap_or(Value::Null),
        "kind": object.get("kind").cloned().unwrap_or(Value::Null),
    }))
}

/// 从 rt-host 来源投影 speaker_test（socket 优先，其次状态文件）。
///
/// 返回 (字段, 不可读原因)。文件里 speaker_test=null/缺失（控制通道未启用的文件回退）
/// 给全 null + 原因，绝不编 active:false。
pub fn project_status(
    socket_document: Option<&Value>,
    file_document: Option<&Value>,
    unavailable_reason: &str,
) -> (Value, Option<String>) {
    if let Some(document) = socket_document {
        return match document.get("speaker_test") {
            Some(value) => match normalize_status(value) {
                Some(status) => (status, None),
                None => (
                    unavailable_status(),
                    Some(
                        "the rt-host control status reports a malformed speaker_test field (active must be a boolean); the daemon does not fabricate the test-tone state"
                            .to_string(),
                    ),
                ),
            },
            None => (
                unavailable_status(),
                Some(
                    "the rt-host control status does not report speaker_test (rt-host build without the speaker-test extension): the daemon cannot know whether a test tone is playing"
                        .to_string(),
                ),
            ),
        };
    }
    if let Some(document) = file_document {
        return match document.get("speaker_test") {
            Some(Value::Null) | None => (
                unavailable_status(),
                Some(
                    "the rt-host status file reports speaker_test=null (the rt-host control channel / test-tone path is not enabled in that process, so the status file cannot carry the state): the daemon does not fabricate active:false"
                        .to_string(),
                ),
            ),
            Some(value) => match normalize_status(value) {
                Some(status) => (status, None),
                None => (
                    unavailable_status(),
                    Some(
                        "the rt-host status file has a malformed speaker_test field; the daemon does not fabricate the test-tone state"
                            .to_string(),
                    ),
                ),
            },
        };
    }
    (
        unavailable_status(),
        Some(format!(
            "the speaker test state is unavailable: {unavailable_reason}"
        )),
    )
}

// ---------------------------------------------------------------------------
// /api/meta 能力条目
// ---------------------------------------------------------------------------

/// /api/meta 的 speaker_test 能力对象（如实：configured / wired / code / window / reason）。
///
/// 调用方在 blocking 线程池里调用（会做一次 rt-host status 探测）。
pub fn meta_capability(state: &DaemonState) -> Value {
    let config = state.speaker_test().config();
    let window_configured = config.window().is_some();
    let window_spec = config
        .window()
        .map(|window| Value::String(window.spec().to_string()))
        .unwrap_or(Value::Null);
    let mut wired = false;
    let mut code: Option<&str> = None;
    let mut reason: Option<String> = None;
    if !config.enabled() {
        code = Some("not_wired");
        reason = Some(config.disabled_reason().to_string());
    } else if state.host_link().target() == Target::Process {
        code = Some("not_wired");
        reason = Some(PROCESS_TARGET_REASON.to_string());
    } else {
        match state.host_link().status_snapshot() {
            crate::host_link::LinkSnapshot::Socket { status, .. } => {
                let advertises = status
                    .get("wired")
                    .and_then(Value::as_array)
                    .is_some_and(|ops| ops.iter().any(|op| op.as_str() == Some("test_tone")));
                if advertises {
                    wired = true;
                } else {
                    code = Some("not_wired");
                    reason = Some(
                        "the connected rt-host does not advertise the test_tone op in status.wired (rt-host build without the speaker-test extension): no test tone can be played"
                            .to_string(),
                    );
                }
            }
            crate::host_link::LinkSnapshot::Unavailable {
                reason: link_reason,
                ..
            } => {
                code = Some("host_link_unavailable");
                reason = Some(format!(
                    "the rt-host control socket is unavailable ({link_reason}); the speaker test output cannot be verified or commanded"
                ));
            }
            crate::host_link::LinkSnapshot::Process => {
                code = Some("not_wired");
                reason = Some(PROCESS_TARGET_REASON.to_string());
            }
        }
    }
    json!({
        "configured": config.enabled(),
        "wired": wired,
        "code": code,
        "window": {
            "configured": window_configured,
            "spec": window_spec,
        },
        "reason": reason,
    })
}

// ---------------------------------------------------------------------------
// REST 路由
// ---------------------------------------------------------------------------

/// 组装 speaker-test 路由（挂到全局 auth 中间件之下）。
pub fn routes() -> Router<Arc<DaemonState>> {
    Router::new()
        .route("/api/speaker-test", post(speaker_test))
        .route("/api/speaker-test/stop", post(speaker_test_stop))
}

/// POST /api/speaker-test：校验 + 门禁 + 转发 rt-host test_tone。
pub(crate) async fn speaker_test(State(state): State<Arc<DaemonState>>, body: Body) -> Response {
    let object = match read_json_body(body).await {
        Ok(object) => object,
        Err(error) => return error.into_response(),
    };
    let request = match parse_request(&object) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    if let Some(error) = gate_error(state.speaker_test().gate_now()) {
        return error.into_response();
    }
    // daemon 侧忙：校准 job 在跑时不出声（rt-host 自己也会拒绝 busy/cal_busy）。
    let calibration = state.calibration_status();
    if let Some(active_job_id) = calibration.active_job {
        return ApiError::conflict(
            "busy",
            format!(
                "a calibration job is active ({active_job_id}): a speaker test tone would play into a running measurement; wait for it to finish or cancel it"
            ),
        )
        .with_detail("active_job_id", json!(active_job_id))
        .into_response();
    }
    if state.host_link().target() == Target::Process {
        return ApiError::not_wired("not_wired", PROCESS_TARGET_REASON).into_response();
    }
    let mut payload = json!({
        "op": "test_tone",
        "dac": request.dac,
        "ms": request.ms,
        "kind": request.kind.as_str(),
        "db": f64::from(request.level_db),
        "audible": true,
        "confirm": true,
    });
    if let Some(freq_hz) = request.freq_hz {
        payload["freq_hz"] = json!(freq_hz);
    }
    let link = Arc::clone(state.host_link());
    let reply = tokio::task::spawn_blocking(move || link.call(payload)).await;
    match reply {
        Ok(Ok(WireReply::Success(body))) => (StatusCode::OK, Json(body)).into_response(),
        Ok(Ok(WireReply::Failure {
            code,
            message,
            body,
            ..
        })) => rt_host_error(&code, &message, body, "test_tone").into_response(),
        Ok(Err(error)) => link_error(error).into_response(),
        Err(join_error) => {
            ApiError::internal(format!("speaker test forwarding task failed: {join_error}"))
                .into_response()
        }
    }
}

/// POST /api/speaker-test/stop：幂等转发 rt-host test_tone_stop（停止不受窗口/开关限制）。
pub(crate) async fn speaker_test_stop(State(state): State<Arc<DaemonState>>) -> Response {
    if state.host_link().target() == Target::Process {
        return ApiError::not_wired("not_wired", PROCESS_TARGET_REASON).into_response();
    }
    let link = Arc::clone(state.host_link());
    let reply =
        tokio::task::spawn_blocking(move || link.call(json!({ "op": "test_tone_stop" }))).await;
    match reply {
        Ok(Ok(WireReply::Success(body))) => (StatusCode::OK, Json(body)).into_response(),
        Ok(Ok(WireReply::Failure {
            code,
            message,
            body,
            ..
        })) => rt_host_error(&code, &message, body, "test_tone_stop").into_response(),
        Ok(Err(error)) => link_error(error).into_response(),
        Err(join_error) => ApiError::internal(format!(
            "speaker test stop forwarding task failed: {join_error}"
        ))
        .into_response(),
    }
}

fn gate_error(gate: Gate) -> Option<ApiError> {
    match gate {
        Gate::Open => None,
        Gate::Disabled { reason } => Some(ApiError::not_wired("not_wired", reason)),
        Gate::WindowClosed { spec } => Some(ApiError::new(
            StatusCode::FORBIDDEN,
            "sound_window_closed",
            format!("{WINDOW_CLOSED_REASON} ({WINDOW_ENV}={spec})"),
        )),
        Gate::ClockUnavailable { reason } => Some(ApiError::not_wired(
            "not_wired",
            format!(
                "the sound window is configured but the local wall-clock time is unavailable, so the gate cannot be evaluated: refusing to play (fail-closed): {reason}"
            ),
        )),
    }
}

fn link_error(error: LinkError) -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        crate::host_link::HOST_LINK_UNAVAILABLE,
        error.message,
    )
}

/// rt-host 业务错误 -> M1 错误（§12.2/§12.4：bad_request 400、busy 409、not_applied 503、
/// 未实现 503 not_wired；其余上游错误 502，不伪装成成功）。
fn rt_host_error(code: &str, message: &str, body: Value, op: &str) -> ApiError {
    let error = match code {
        "bad_request" => ApiError::new(StatusCode::BAD_REQUEST, "bad_request", message),
        "busy" | "cal_busy" | "reconfiguring" | "channels_busy" => {
            ApiError::conflict("busy", message)
        }
        "not_applied" => ApiError::not_wired(
            "not_wired",
            format!("the rt-host did not apply the speaker test change ({op}): {message}"),
        ),
        "unsupported" | "unknown_op" | "not_wired" | "not_implemented" => ApiError::not_wired(
            "not_wired",
            format!("the connected rt-host does not support the {op} op ({code}: {message})"),
        ),
        _ => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "upstream_error",
            format!("rt-host rejected {op} with {code}: {message}"),
        ),
    };
    error
        .with_detail("rt_host_code", json!(code))
        .with_detail("rt_host", body)
}

/// 由 main 打印的启动摘要（enabled/window；不打印敏感信息）。
pub fn startup_line(runtime: &SpeakerTestRuntime) -> String {
    format!(
        "khaen-daemon speaker-test enabled={} window={} clock={}",
        runtime.config.enabled(),
        runtime
            .config
            .window()
            .map(|window| window.spec().to_string())
            .unwrap_or_else(|| "-".to_string()),
        runtime.clock_source
    )
}

/// 当前时刻的 RFC3339（服务端来源一致：status 模块的 UTC 口径；仅测试/诊断用）。
pub fn now_rfc3339() -> String {
    status::rfc3339_utc(SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(spec: &str) -> SoundWindow {
        SoundWindow::parse(spec).expect("window")
    }

    #[test]
    fn sound_window_parses_multiple_segments_and_boundaries() {
        let window = window("08:30-12:00, 14:30-18:00");
        assert_eq!(
            window.windows(),
            &[(8 * 60 + 30, 12 * 60), (14 * 60 + 30, 18 * 60)]
        );
        // 开始边界含、结束边界不含。
        assert!(!window.contains(8 * 60 + 29));
        assert!(window.contains(8 * 60 + 30));
        assert!(window.contains(11 * 60 + 59));
        assert!(!window.contains(12 * 60));
        assert!(!window.contains(13 * 60));
        assert!(window.contains(14 * 60 + 30));
        assert!(window.contains(17 * 60 + 59));
        assert!(!window.contains(18 * 60));
        assert!(!window.contains(0));
        assert!(!window.contains(23 * 60 + 59));
    }

    #[test]
    fn sound_window_supports_cross_midnight_ranges() {
        let window = window("22:00-06:00");
        assert!(!window.contains(21 * 60 + 59));
        assert!(window.contains(22 * 60));
        assert!(window.contains(23 * 60 + 59));
        assert!(window.contains(0));
        assert!(window.contains(5 * 60 + 59));
        assert!(!window.contains(6 * 60));
        assert!(!window.contains(12 * 60));
    }

    #[test]
    fn sound_window_rejects_malformed_specs() {
        for spec in [
            "",
            "  ",
            "08:30",
            "08:30-",
            "-12:00",
            "24:00-25:00",
            "08:60-09:00",
            "08:30-08:30",
            "08:30-12:00,",
            "08:30-12:00-13:00",
            "aa:bb-12:00",
        ] {
            assert!(
                SoundWindow::parse(spec).is_err(),
                "{spec:?} must be rejected"
            );
        }
        // 宽松：单位数小时 + 异常格式里 '-' 拆出三段会拒绝；'8:30-12:0' 接受（分钟补零不强制）。
        assert!(SoundWindow::parse("8:30-12:00").is_ok());
    }

    #[test]
    fn gate_is_a_pure_function_of_config_and_injected_time() {
        let config = SpeakerTestConfig::parse(Some("on"), Some("08:30-12:00")).expect("config");
        assert_eq!(config.gate_at(Some(8 * 60 + 30)), Gate::Open);
        assert_eq!(
            config.gate_at(Some(12 * 60)),
            Gate::WindowClosed {
                spec: "08:30-12:00".to_string()
            }
        );
        assert_eq!(
            config.gate_at(None),
            Gate::ClockUnavailable {
                reason: "the local wall-clock time is unavailable".to_string()
            }
        );
        // 未启用 -> Disabled（不看窗口/时间）。
        let disabled = SpeakerTestConfig::parse(None, None).expect("config");
        assert!(matches!(
            disabled.gate_at(Some(10 * 60)),
            Gate::Disabled { .. }
        ));
        // 启用且无窗口 -> 永远 Open。
        let open = SpeakerTestConfig::parse(Some("on"), None).expect("config");
        assert_eq!(open.gate_at(None), Gate::Open);
    }

    #[test]
    fn config_parse_is_fail_closed_on_invalid_values() {
        let error = SpeakerTestConfig::parse(Some("nope"), None).expect_err("enabled");
        assert!(matches!(error, ConfigError::InvalidEnabled { .. }));
        let error = SpeakerTestConfig::parse(Some("on"), Some("08:30")).expect_err("window");
        assert!(matches!(error, ConfigError::InvalidWindow { .. }));
        for (raw, expected) in [
            (Some("on"), true),
            (Some("ON"), true),
            (Some("1"), true),
            (Some("true"), true),
            (Some("off"), false),
            (Some("0"), false),
            (Some(""), false),
            (None, false),
        ] {
            let config = SpeakerTestConfig::parse(raw, None).expect("config");
            assert_eq!(config.enabled(), expected, "{raw:?}");
        }
        let disabled = SpeakerTestConfig::parse(Some("off"), None).expect("config");
        assert!(disabled.disabled_reason().contains(ENABLED_ENV));
        assert!(!disabled.disabled_reason().is_empty());
    }

    #[test]
    fn runtime_gate_uses_the_injected_clock() {
        let config = SpeakerTestConfig::parse(Some("on"), Some("08:30-12:00")).expect("config");
        let runtime = SpeakerTestRuntime::with_clock(config.clone(), Arc::new(|| Ok(9 * 60)));
        assert_eq!(runtime.gate_now(), Gate::Open);
        let runtime = SpeakerTestRuntime::with_clock(config.clone(), Arc::new(|| Ok(13 * 60)));
        assert!(matches!(runtime.gate_now(), Gate::WindowClosed { .. }));
        let runtime =
            SpeakerTestRuntime::with_clock(config, Arc::new(|| Err("no clock".to_string())));
        assert!(matches!(runtime.gate_now(), Gate::ClockUnavailable { .. }));
    }

    fn request_object(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("object")
    }

    fn valid_request() -> Map<String, Value> {
        request_object(json!({
            "dac": 3,
            "ms": 1000,
            "kind": "sine",
            "freq_hz": 1000,
            "db": -30,
            "audible": true,
            "confirm": true,
        }))
    }

    #[test]
    fn request_parsing_applies_the_documented_defaults_and_ranges() {
        let request = parse_request(&valid_request()).expect("valid");
        assert_eq!(request.dac, 3);
        assert_eq!(request.ms, 1000);
        assert_eq!(request.kind, TestToneKind::Sine);
        assert_eq!(request.level_db, -30.0);
        assert_eq!(request.freq_hz, Some(1000.0));

        // 默认 kind=noise、db=-30；noise 忽略 freq_hz。
        let object = request_object(json!({
            "dac": 0,
            "ms": 500,
            "audible": true,
            "confirm": true,
            "freq_hz": 1000,
        }));
        let request = parse_request(&object).expect("defaults");
        assert_eq!(request.kind, TestToneKind::Noise);
        assert_eq!(request.level_db, DEFAULT_DB);
        assert_eq!(request.freq_hz, None);
        // 边界值合法。
        for (ms, db, dac) in [(500, -60.0, 0), (10_000, -12.0, 21)] {
            let object = request_object(json!({
                "dac": dac,
                "ms": ms,
                "db": db,
                "audible": true,
                "confirm": true,
            }));
            parse_request(&object).expect("boundary");
        }
    }

    #[test]
    fn request_parsing_rejects_missing_confirmation_and_out_of_range_fields() {
        let cases = [
            json!({ "dac": 0, "ms": 1000, "confirm": true }),
            json!({ "dac": 0, "ms": 1000, "audible": true }),
            json!({ "dac": 0, "ms": 1000, "audible": true, "confirm": false }),
            json!({ "dac": 0, "ms": 1000, "audible": true, "confirm": "yes" }),
        ];
        for case in cases {
            let error = parse_request(&request_object(case.clone())).expect_err("confirm");
            assert_eq!(error.status(), StatusCode::BAD_REQUEST, "{case}");
        }
        for case in [
            json!({ "dac": 22, "ms": 1000, "audible": true, "confirm": true }),
            json!({ "dac": -1, "ms": 1000, "audible": true, "confirm": true }),
            json!({ "dac": 1.5, "ms": 1000, "audible": true, "confirm": true }),
            json!({ "dac": 0, "ms": 499, "audible": true, "confirm": true }),
            json!({ "dac": 0, "ms": 10_001, "audible": true, "confirm": true }),
            json!({ "dac": 0, "ms": 1000, "db": -61, "audible": true, "confirm": true }),
            json!({ "dac": 0, "ms": 1000, "db": -11, "audible": true, "confirm": true }),
            json!({ "dac": 0, "ms": 1000, "kind": "square", "audible": true, "confirm": true }),
            json!({ "dac": 0, "ms": 1000, "kind": "sine", "freq_hz": 5, "audible": true, "confirm": true }),
        ] {
            let error = parse_request(&request_object(case.clone())).expect_err("range");
            assert_eq!(error.status(), StatusCode::BAD_REQUEST, "{case}");
        }
        let error = parse_request(&request_object(json!({
            "ms": 1000,
            "audible": true,
            "confirm": true,
        })))
        .expect_err("missing dac");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn status_projection_reads_socket_status_first() {
        let socket = json!({
            "speaker_test": { "active": true, "dac": 3, "remaining_ms": 420, "kind": "sine" }
        });
        let (status, reason) = project_status(Some(&socket), None, "");
        assert_eq!(reason, None);
        assert_eq!(status["active"], json!(true));
        assert_eq!(status["dac"], json!(3));
        assert_eq!(status["remaining_ms"], json!(420));
        assert_eq!(status["kind"], json!("sine"));
        // 文件来源不参与（socket 优先）。
        let file = json!({ "speaker_test": null });
        let (status, reason) = project_status(Some(&socket), Some(&file), "");
        assert_eq!(reason, None);
        assert_eq!(status["active"], json!(true));
    }

    #[test]
    fn status_projection_keeps_null_when_the_source_cannot_report_it() {
        // socket 模式没有 speaker_test 字段（旧 rt-host）。
        let (status, reason) = project_status(Some(&json!({ "ok": true })), None, "");
        assert_eq!(status["active"], Value::Null);
        assert_eq!(status["dac"], Value::Null);
        assert_eq!(status["remaining_ms"], Value::Null);
        assert_eq!(status["kind"], Value::Null);
        assert!(reason.is_some_and(|reason| reason.contains("does not report")));
        // 文件回退里 speaker_test=null：不编 false。
        let (status, reason) = project_status(None, Some(&json!({ "speaker_test": null })), "");
        assert_eq!(status["active"], Value::Null);
        assert!(reason.is_some_and(|reason| reason.contains("does not fabricate")));
        // 文件里没有字符串；同样 null + 原因。
        let (status, reason) = project_status(None, Some(&json!({ "ok": true })), "");
        assert_eq!(status["active"], Value::Null);
        assert!(reason.is_some_and(|reason| reason.contains("does not fabricate")));
        // 全部不可用（none/process）：null + 传入原因。
        let (status, reason) = project_status(None, None, "no rt-host link");
        assert_eq!(status["active"], Value::Null);
        assert!(reason.is_some_and(|reason| reason.contains("no rt-host link")));
        // 畸形 active 类型：null + 原因（不猜）。
        let (status, reason) = project_status(
            Some(&json!({ "speaker_test": { "active": "yes" } })),
            None,
            "",
        );
        assert_eq!(status["active"], Value::Null);
        assert!(reason.is_some_and(|reason| reason.contains("malformed")));
    }

    #[test]
    fn rt_host_error_mapping_is_stable() {
        let forbidden = |code: &str| rt_host_error(code, "m", json!({}), "test_tone");
        assert_eq!(forbidden("bad_request").status(), StatusCode::BAD_REQUEST);
        assert_eq!(forbidden("bad_request").code(), "bad_request");
        assert_eq!(forbidden("busy").status(), StatusCode::CONFLICT);
        assert_eq!(forbidden("busy").code(), "busy");
        assert_eq!(
            forbidden("not_applied").status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(forbidden("not_applied").code(), "not_wired");
        assert_eq!(
            forbidden("unsupported").status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            forbidden("unknown_op").status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            forbidden("device_unavailable").status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(forbidden("device_unavailable").code(), "upstream_error");
    }
}
