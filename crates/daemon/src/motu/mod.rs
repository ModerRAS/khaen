//! MOTU UltraLite-mk5 控制面接线（T57）：检测 + 静音/音量端点。
//!
//! 端点（全部挂到全局 [crate::auth::guard] 认证中间件之下：需要 M1 会话，写方法还要 CSRF）：
//! - `GET /api/motu`：检测对象（detected / model / card_id / card_index / control_reachable /
//!   reason）+ 当前 `mute` / `vol` 现状；设备读不到时字段 null + `reasons`（路径 -> 原因），
//!   恒 200。Windows / 无设备时 detected=false、不报错、不 500。
//! - `POST /api/motu/volume`（也接受 PUT）：两阶段音量端点。
//!   `stage=prepare`（默认）只校验请求、确认设备可用，返回 `engine_token` + 写计划（**不写设备**）；
//!   `stage=commit` 才逐通道单键写 + 回读。批量（一次请求超过两个主通道，或点名非主通道的
//!   批量列表）400 `motu_batch_rejected`——CueMix 设备对批量写敏感，这是硬约束。
//!   回读不一致/超时如实 502 + 设备错误码（不假成功）；部分成功时 `completed` 报告已验证的通道。
//! - `POST /api/mute`：原控制面语义不变（target=process 时仍作用于进程内引擎，响应仍带
//!   `target: "daemon-process"`）。target=process 且检测到 MOTU 时，额外把同一次静音请求
//!   单键写入 `kMuteEnable[0]` 并回读；设备写失败如实 502 + 错误码（响应里保留 `control`
//!   原回执，绝不把"引擎静音成功"当成"设备静音成功"）。target=rt-host 时行为完全不变
//!   （MOTU 写入不在这一路径的职责内）。
//!
//! 设计约束：
//! - 设备访问统一走 [MotuControl] 抽象：默认实现 [NativeMotu]（crates/motu-cuemix 的阻塞客户端，
//!   在 blocking 线程池里调用），测试注入 FakeCueMix（禁真实网络/设备）。
//! - 检测逻辑复用 crates/motu-cuemix 的 `detect`（/proc/asound/cards 可注入路径 + TCP probe
//!   可注入），daemon 侧不再实现一套。
//! - 单键写：`kMuteEnable[0]` 与 `koBusFader[index]` 每次调用只发一帧；写完必须回读到同值才
//!   算成功（由 crate 保证，daemon 只如实转述 before/wrote/readback）。
//! - 能力如实：设备不可用时音量 prepare 503 + 稳定码；GET 的 `write` 对象明确
//!   status=wired/unreachable/not_detected、batch=false、two_phase=true。
//! - 两阶段只用在音量写入上：`/api/mute` 保持单步（既有安全动作的控制面契约不变，前端
//!   已经按单步调用；设备写入仍然单键 + 回读验证，失败如实 502），音量才是 prepare/commit。
//! - 批量写被拒绝是硬约束：CueMix 设备对批量写敏感（一次写数百个属性会让 USB-CDC 控制口
//!   失联），所以一次请求最多两个主通道，且每通道一帧、回读通过后再写下一帧。

pub mod api;
pub mod native;

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use api::routes;
pub use khaen_motu_cuemix::{
    bus_label, codes as device_codes, db_to_fader, detect_with, fader_to_db, quantize_linear,
    DetectConfig, DeviceDetection, MainChannel, MAIN_FADER_CHANNELS, REASON_CARDS_UNAVAILABLE,
    REASON_CONTROL_UNREACHABLE, REASON_DETECTED, REASON_NO_MOTU_CARD,
};
pub use native::{MotuConfig, NativeMotu};

/// `GET /api/motu` 的 schema。
pub const SCHEMA_STATUS: &str = "khaen-motu/v1";
/// `POST /api/motu/volume` 的 schema。
pub const SCHEMA_VOLUME: &str = "khaen-motu-volume/v1";
/// 可寻址的主输出通道数（Main 1 / Main 2；一批最多写这么多，逐通道单键写）。
pub const CHANNEL_COUNT: u16 = MAIN_FADER_CHANNELS;
/// 音量草稿（prepare 的 token）有效期。
pub const PENDING_TTL_SECS: u64 = 120;
/// 音量草稿有效期（[Duration] 形式）。
pub const PENDING_TTL: Duration = Duration::from_secs(PENDING_TTL_SECS);
/// 音量端点请求体上限（JSON 控制面）。
pub const MAX_JSON_BODY: usize = 16 * 1024;
/// MOTU 控制被环境变量显式关闭时的检测原因（`KHAEN_MOTU_DISABLE=1`）。
pub const REASON_DISABLED: &str = "motu_control_disabled";
/// 音量写入的作用对象（响应里原样带出，避免前端猜）。
pub const APPLIES_TO: &str = "MOTU CueMix datastore: one single-key write per main channel (koBusFader[0] / koBusFader[1] / kMuteEnable[0]), each verified by device readback";

/// MOTU 端点的稳定错误码（写操作失败时客户端按码分支，不依赖文案）。
pub mod error_code {
    use super::REASON_DISABLED;

    /// 没有检测到 MOTU 设备（cards_unavailable / no_motu_card）。
    pub const NOT_DETECTED: &str = "motu_not_detected";
    /// 设备在但 USB-CDC 控制网 TCP probe 不通。
    pub const CONTROL_UNREACHABLE: &str = "motu_control_unreachable";
    /// 检测/写入被 `KHAEN_MOTU_DISABLE=1` 显式关闭。
    pub const CONTROL_DISABLED: &str = REASON_DISABLED;
    /// 批量写被拒绝（一次最多两个主通道，逐通道单键写）。
    pub const BATCH_REJECTED: &str = "motu_batch_rejected";
}

// ---------------------------------------------------------------------------
// 设备抽象
// ---------------------------------------------------------------------------

/// 一次失败：稳定错误码 + 文案 + 是否值得重试（码复用 crates/motu-cuemix 的 `device_codes::*`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MotuError {
    /// 稳定机器码（`E_*` 设备码或 daemon 侧码）。
    pub code: &'static str,
    /// 人类可读原因（可含设备返回的期望/实际值）。
    pub message: String,
    /// 简单重试（设备稍后稳定）是否可能成功。
    pub retryable: bool,
}

impl MotuError {
    /// 构造一个不可重试的失败。
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
        }
    }

    /// 标记是否可重试。
    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    /// 从 crates/motu-cuemix 的失败转换（错误码/文案/重试性原样保留）。
    pub fn from_device(error: &khaen_motu_cuemix::CueMixError) -> Self {
        Self {
            code: error.code(),
            message: error.to_string(),
            retryable: error.is_retryable(),
        }
    }

    /// 响应用的 JSON（`{code,message,retryable}`）。
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "code": self.code,
            "message": self.message,
            "retryable": self.retryable,
        })
    }
}

impl fmt::Display for MotuError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for MotuError {}

/// 设备当前状态（`GET /api/motu` 的数据来源）；拿不到的字段是 None，绝不编数值。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MotuReading {
    /// kModelID（如 "UL5L"）。
    pub model_id: Option<String>,
    /// kDevName（如 "UltraLite-mk5"）。
    pub device_name: Option<String>,
    /// 固件版本（kDevVersion）。
    pub version: Option<String>,
    /// 控制 API 版本（kAPIVersion）。
    pub api_version: Option<u16>,
    /// kMuteEnable[0]：设备全局静音。
    pub mute: Option<bool>,
    /// kSampleRate[0]：设备采样率。
    pub sample_rate_hz: Option<u32>,
    /// koBusFader[0]/[1]：主输出两通道线性音量（0.0..=1.0）。
    pub main_faders: Vec<Option<f64>>,
    /// kMainTrim[0]：面板主输出 trim。
    pub main_trim: Option<u8>,
    /// koBusMute[0]：Main 1 总线静音标志（只读）。
    pub main_bus_mute: Option<bool>,
    /// 设备没有上报的字段名（如实列出，不补默认值）。
    pub missing: Vec<String>,
}

/// 一次已验证的单键写回执（写帧发出后必须回读到同值才会是 Ok）。
#[derive(Clone, Debug, PartialEq)]
pub struct WriteReceipt<T> {
    /// 写入的 datastore 键（e.g. "koBusFader"）。
    pub key: &'static str,
    /// datastore 数组下标（e.g. 0 = Main 1）。
    pub index: u16,
    /// 写入前设备上的值（未收到过为 None）。
    pub before: Option<T>,
    /// 写入的目标值（8.24 量化后的值）。
    pub wrote: T,
    /// 设备回读到的值。
    pub readback: T,
    /// 值确实变了（false = 目标值本来就在位）。
    pub changed: bool,
    /// true = 真的发了一帧；false = 目标值已在位，未发写帧。
    pub frame_sent: bool,
}

impl<T: PartialEq> WriteReceipt<T> {
    /// 回读与写入一致（Ok 回执恒为 true；写成独立方法便于响应里明示 verified）。
    pub fn verified(&self) -> bool {
        self.readback == self.wrote
    }
}

impl<T: PartialEq + Copy + Into<serde_json::Value>> WriteReceipt<T> {
    /// 回执 JSON（before/wrote/readback/changed/frame_sent/verified）。
    ///
    /// 手写 Value 而不是依赖 serde 派生：daemon 只直接依赖 serde_json，bool/f64 都能转 Value。
    pub fn to_json(&self) -> serde_json::Value {
        let mut object = serde_json::Map::new();
        object.insert("key".to_string(), serde_json::Value::from(self.key));
        object.insert("index".to_string(), serde_json::Value::from(self.index));
        object.insert(
            "before".to_string(),
            self.before
                .map(Into::into)
                .unwrap_or(serde_json::Value::Null),
        );
        object.insert("wrote".to_string(), self.wrote.into());
        object.insert("readback".to_string(), self.readback.into());
        object.insert("changed".to_string(), serde_json::Value::Bool(self.changed));
        object.insert(
            "frame_sent".to_string(),
            serde_json::Value::Bool(self.frame_sent),
        );
        object.insert(
            "verified".to_string(),
            serde_json::Value::Bool(self.verified()),
        );
        serde_json::Value::Object(object)
    }
}

/// daemon 需要的 MOTU 控制面能力（注入点：生产 [NativeMotu]，测试 FakeCueMix）。
///
/// 实现方保证：每个写方法最多发一帧（单键写），且只有在设备流回同值时才返回 Ok。
pub trait MotuControl: Send + Sync + fmt::Debug {
    /// 检测设备（只读 /proc/asound/cards + TCP probe；Windows/无设备 detected=false 不报错）。
    fn detection(&self) -> DeviceDetection;
    /// 读设备当前状态（可能需要建立/复用控制连接）。
    fn read(&self) -> Result<MotuReading, MotuError>;
    /// 单键写 kMuteEnable[0] + 回读。
    fn write_mute(&self, mute: bool) -> Result<WriteReceipt<bool>, MotuError>;
    /// 单键写 koBusFader[channel_index] + 回读（0 = Main 1，1 = Main 2）。
    fn write_volume(&self, channel_index: u16, linear: f64)
        -> Result<WriteReceipt<f64>, MotuError>;
}

// ---------------------------------------------------------------------------
// 两阶段音量草稿
// ---------------------------------------------------------------------------

/// prepare 产出的音量草稿（commit 用 token 找回它）。
#[derive(Clone, Debug, PartialEq)]
pub struct PendingVolumeDraft {
    /// 一次性 token（16 hex）。
    pub token: String,
    /// 目标通道（1..=2，升序）。
    pub channels: Vec<u16>,
    /// 8.24 量化后的线性音量（0.0..=1.0）。
    pub linear: f64,
    /// 量化后线性值的 dB 等价值（0.0 -> None = -inf）。
    pub db: Option<f64>,
    /// 草稿创建时刻（单调时钟；只用来看是否过期）。
    pub created: Instant,
    /// 草稿创建时刻（RFC3339；响应里给用户看）。
    pub created_at: String,
}

impl PendingVolumeDraft {
    /// 是否过期。
    pub fn is_expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.created) > PENDING_TTL
    }
}

/// 音量草稿 token 校验失败（全部映射 409 `engine_token_invalid`，文案区分原因）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotuTokenError {
    /// 没有草稿（未 prepare 或已被 commit 消费）。
    NoDraft,
    /// token 与当前草稿不符（可能是 prepare 被更新的请求覆盖）。
    Mismatch,
    /// 草稿超过有效期。
    Expired,
}

impl MotuTokenError {
    /// 稳定错误码（与控制面 prepare/commit 的 token 语义同源）。
    pub fn code(self) -> &'static str {
        khaen_host::control::error::ENGINE_TOKEN_INVALID
    }

    /// 人类可读原因。
    pub fn message(self) -> &'static str {
        match self {
            Self::NoDraft => {
                "no MOTU volume draft is pending: call stage=prepare first (a commit consumes the draft)"
            }
            Self::Mismatch => {
                "engine_token does not match the pending MOTU volume draft (another prepare replaced it)"
            }
            Self::Expired => "the MOTU volume draft expired: call stage=prepare again",
        }
    }
}

/// MOTU 控制面共享状态：设备抽象 + 两阶段音量草稿（单草稿槽）。
#[derive(Debug)]
pub struct MotuHub {
    control: Arc<dyn MotuControl>,
    pending: Mutex<Option<PendingVolumeDraft>>,
}

impl MotuHub {
    /// 用给定控制面实现构造（生产 NativeMotu；测试 FakeCueMix）。
    pub fn new(control: Arc<dyn MotuControl>) -> Self {
        Self {
            control,
            pending: Mutex::new(None),
        }
    }

    /// 设备抽象（handler 在 blocking 线程池里调用）。
    pub fn control(&self) -> &Arc<dyn MotuControl> {
        &self.control
    }

    /// 检测设备（透传；调用方负责放到 blocking 线程池）。
    pub fn detection(&self) -> DeviceDetection {
        self.control.detection()
    }

    /// 保存音量草稿：同一目标（通道 + 量化值）且未过期时幂等返回同一 token；
    /// 不同目标覆盖旧草稿（旧 token 随即失效）。
    pub fn put_draft(
        &self,
        channels: Vec<u16>,
        linear: f64,
        db: Option<f64>,
        now: Instant,
        created_at: &str,
    ) -> PendingVolumeDraft {
        let mut pending = self.lock();
        if let Some(existing) = pending.as_ref() {
            if !existing.is_expired(now)
                && existing.channels == channels
                && existing.linear.to_bits() == linear.to_bits()
                && existing.db == db
            {
                return existing.clone();
            }
        }
        let draft = PendingVolumeDraft {
            token: next_token(&channels, linear, db, created_at),
            channels,
            linear,
            db,
            created: now,
            created_at: created_at.to_string(),
        };
        *pending = Some(draft.clone());
        draft
    }

    /// 消费草稿：token 匹配且未过期则取走（一次性），否则返回原因。
    pub fn take_draft(
        &self,
        token: &str,
        now: Instant,
    ) -> Result<PendingVolumeDraft, MotuTokenError> {
        let mut pending = self.lock();
        let Some(draft) = pending.clone() else {
            return Err(MotuTokenError::NoDraft);
        };
        if draft.token != token {
            return Err(MotuTokenError::Mismatch);
        }
        if draft.is_expired(now) {
            *pending = None;
            return Err(MotuTokenError::Expired);
        }
        *pending = None;
        Ok(draft)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<PendingVolumeDraft>> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 草稿 token：sha256(目标 + 创建时刻 + 进程内计数器) 前 16 位（每次新草稿都不同）。
fn next_token(channels: &[u16], linear: f64, db: Option<f64>, created_at: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let canonical = format!(
        "khaen-motu-volume/v1|{created_at}|{counter}|{:?}|{:.12}|{:?}",
        channels, linear, db
    );
    let digest = khaen_host::sha256_hex(canonical.as_bytes());
    digest[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只有一个可编排检测结果的控制面替身（token 逻辑测试不需要设备访问）。
    #[derive(Debug)]
    struct StubControl {
        detection: DeviceDetection,
    }

    impl MotuControl for StubControl {
        fn detection(&self) -> DeviceDetection {
            self.detection.clone()
        }
        fn read(&self) -> Result<MotuReading, MotuError> {
            Err(MotuError::new(
                device_codes::NOT_DETECTED,
                "stub has no device",
            ))
        }
        fn write_mute(&self, _mute: bool) -> Result<WriteReceipt<bool>, MotuError> {
            Err(MotuError::new(
                device_codes::NOT_DETECTED,
                "stub has no device",
            ))
        }
        fn write_volume(
            &self,
            _channel_index: u16,
            _linear: f64,
        ) -> Result<WriteReceipt<f64>, MotuError> {
            Err(MotuError::new(
                device_codes::NOT_DETECTED,
                "stub has no device",
            ))
        }
    }

    fn absent_detection() -> DeviceDetection {
        DeviceDetection {
            detected: false,
            card_id: None,
            card_index: None,
            model: None,
            control_reachable: false,
            reason: REASON_CARDS_UNAVAILABLE,
        }
    }

    fn hub() -> MotuHub {
        MotuHub::new(Arc::new(StubControl {
            detection: absent_detection(),
        }))
    }

    #[test]
    fn draft_reuses_the_same_token_for_the_same_target() {
        let hub = hub();
        let now = Instant::now();
        let first = hub.put_draft(vec![1, 2], 0.5, None, now, "2026-11-26T00:00:00Z");
        let second = hub.put_draft(vec![1, 2], 0.5, None, now, "2026-11-26T00:00:01Z");
        assert_eq!(first.token, second.token);
        assert_eq!(first.token.len(), 16);
        assert!(first.token.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn a_different_target_replaces_the_draft_and_invalidates_the_old_token() {
        let hub = hub();
        let now = Instant::now();
        let old = hub.put_draft(vec![1], 0.5, None, now, "2026-11-26T00:00:00Z");
        let new = hub.put_draft(vec![2], 0.25, None, now, "2026-11-26T00:00:02Z");
        assert_ne!(old.token, new.token);
        assert_eq!(
            hub.take_draft(&old.token, now),
            Err(MotuTokenError::Mismatch)
        );
        assert_eq!(
            hub.take_draft(&new.token, now).expect("new token").channels,
            vec![2]
        );
    }

    #[test]
    fn take_draft_is_single_use() {
        let hub = hub();
        let now = Instant::now();
        let draft = hub.put_draft(vec![1], 0.5, None, now, "2026-11-26T00:00:00Z");
        assert!(hub.take_draft(&draft.token, now).is_ok());
        assert_eq!(
            hub.take_draft(&draft.token, now),
            Err(MotuTokenError::NoDraft)
        );
    }

    #[test]
    fn expired_draft_is_rejected_and_dropped() {
        let hub = hub();
        let now = Instant::now();
        let draft = hub.put_draft(vec![1], 0.5, None, now, "2026-11-26T00:00:00Z");
        let later = now + PENDING_TTL + Duration::from_secs(1);
        assert_eq!(
            hub.take_draft(&draft.token, later),
            Err(MotuTokenError::Expired)
        );
        assert_eq!(
            hub.take_draft(&draft.token, later),
            Err(MotuTokenError::NoDraft)
        );
        // 过期后同一目标会生成新 token（旧草稿已被丢弃）。
        let next = hub.put_draft(vec![1], 0.5, None, later, "2026-11-26T00:02:01Z");
        assert_ne!(next.token, draft.token);
    }

    #[test]
    fn token_errors_use_the_control_plane_code() {
        for error in [
            MotuTokenError::NoDraft,
            MotuTokenError::Mismatch,
            MotuTokenError::Expired,
        ] {
            assert_eq!(
                error.code(),
                khaen_host::control::error::ENGINE_TOKEN_INVALID
            );
            assert!(!error.message().is_empty());
        }
    }

    #[test]
    fn device_error_conversion_keeps_code_message_and_retryability() {
        let device = khaen_motu_cuemix::CueMixError::ReadbackTimeout {
            key: "kMuteEnable",
            index: 0,
        };
        let error = MotuError::from_device(&device);
        assert_eq!(error.code, device_codes::READBACK_TIMEOUT);
        assert!(error.retryable);
        assert!(error.message.contains("kMuteEnable"));
        assert_eq!(error.to_json()["code"], device_codes::READBACK_TIMEOUT);
    }

    #[test]
    fn receipt_reports_verified_and_unverified_values() {
        let verified = WriteReceipt {
            key: "koBusFader",
            index: 0,
            before: Some(1.0),
            wrote: 0.5,
            readback: 0.5,
            changed: true,
            frame_sent: true,
        };
        assert!(verified.verified());
        assert_eq!(verified.to_json()["verified"], serde_json::json!(true));

        let mismatched = WriteReceipt {
            readback: 0.625,
            ..verified
        };
        assert!(!mismatched.verified());
        assert_eq!(mismatched.to_json()["verified"], serde_json::json!(false));
    }
}
