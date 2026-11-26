//! CueMix 5 control client: WebSocket datastore mirror, single-key writes with
//! immediate readback verification, read-only state and snapshots.
//!
//! Safety rules baked into this module:
//! * a write call sends at most ONE datastore value in ONE WebSocket frame;
//! * the stereo main volume is written as two separate single-key writes, each
//!   followed by its own readback; a failure on the right channel reports the
//!   already-completed left channel instead of pretending success;
//! * nothing is assumed: a write is only reported as applied after the device
//!   streamed the same value back within the readback deadline.

use std::collections::BTreeMap;
use std::fmt;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::error::CueMixError;
use crate::fader;
use crate::message::{self, Incoming};
use crate::props::{self, keys, Prop};
use crate::snapshot::{self, SnapshotMeta};
use crate::transport::{Transport, WsConfig, WsTransport};
use crate::value::Value;

/// Default MOTU control endpoint (device side of the USB-CDC link).
pub const DEFAULT_CONTROL_ADDR: &str = "169.254.111.126:1280";
/// Default WebSocket request path.
pub const DEFAULT_CONTROL_PATH: &str = "/";
/// Default TCP connect + WebSocket handshake timeout.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Default wait for the initial datastore burst.
pub const DEFAULT_SYNC_TIMEOUT: Duration = Duration::from_secs(3);
/// Default wait for the device to stream a written value back.
pub const DEFAULT_READBACK_TIMEOUT: Duration = Duration::from_millis(1500);
/// Default idle window used when refreshing state before reads/writes.
pub const DEFAULT_IDLE_DRAIN: Duration = Duration::from_millis(150);
/// Default datastore collection window for save-snapshot.
pub const DEFAULT_SNAPSHOT_DRAIN: Duration = Duration::from_secs(3);
/// Hard cap on one idle drain, so a continuously talking device cannot stall a call.
const MAX_IDLE_DRAIN: Duration = Duration::from_secs(2);

/// Resolve the control endpoint from a host name/IP and port.
pub fn resolve_control_addr(host: &str, port: u16) -> Result<SocketAddr, CueMixError> {
    let mut addresses = (host, port)
        .to_socket_addrs()
        .map_err(|source| CueMixError::io("resolve control address", source))?;
    addresses.next().ok_or_else(|| CueMixError::Protocol {
        reason: format!("no address resolved for {}:{}", host, port),
    })
}

/// Client configuration.
#[derive(Clone, Debug)]
pub struct ClientConfig {
    /// Control endpoint.
    pub addr: SocketAddr,
    /// WebSocket request path.
    pub path: String,
    /// Control-network password (None when the device has none).
    pub password: Option<String>,
    /// TCP connect + WebSocket handshake timeout.
    pub connect_timeout: Duration,
    /// Initial datastore burst timeout.
    pub sync_timeout: Duration,
    /// Readback deadline after one write.
    pub readback_timeout: Duration,
    /// Idle window used to refresh the mirror before reads/writes.
    pub idle_drain: Duration,
    /// Datastore collection window for save_snapshot.
    pub snapshot_drain: Duration,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            addr: SocketAddr::from(([169, 254, 111, 126], 1280)),
            path: DEFAULT_CONTROL_PATH.to_string(),
            password: None,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            sync_timeout: DEFAULT_SYNC_TIMEOUT,
            readback_timeout: DEFAULT_READBACK_TIMEOUT,
            idle_drain: DEFAULT_IDLE_DRAIN,
            snapshot_drain: DEFAULT_SNAPSHOT_DRAIN,
        }
    }
}

impl ClientConfig {
    /// Defaults for the given endpoint.
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            ..Self::default()
        }
    }

    /// Set the control-network password.
    pub fn with_password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }

    fn ws_config(&self) -> WsConfig {
        WsConfig {
            addr: self.addr,
            path: self.path.clone(),
            connect_timeout: self.connect_timeout,
            user_agent: format!("khaen-motu/{}", crate::VERSION),
        }
    }
}

/// Counters of one client session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Datastore messages received.
    pub messages: u64,
    /// Proxy/auth messages received.
    pub proxy_messages: u64,
    /// Unknown property ids ignored.
    pub unknown_ids: u64,
    /// Malformed property payloads ignored.
    pub malformed: u64,
    /// Frames with trailing bytes too short for a header.
    pub incomplete: u64,
    /// Text frames ignored (control channel is binary only).
    pub text_frames: u64,
    /// Auth response frame was sent.
    pub auth_response_sent: bool,
    /// At least one datastore message was received.
    pub authenticated: bool,
    /// Opaque proxy status byte, when the device sent one.
    pub proxy_status: Option<u8>,
}

/// Datastore state reported by read_state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CueMixState {
    /// kMuteEnable[0]: global mute.
    pub mute: Option<bool>,
    /// kSampleRate[0]: device sample rate in Hz.
    pub sample_rate_hz: Option<u32>,
    /// koBusFader[0]: Main 1 / left main bus fader (0.0..=1.0 linear).
    pub main_fader: Option<f64>,
    /// kMainTrim[0]: main trim.
    pub main_trim: Option<u8>,
    /// koBusMute[0]: Main 1 / left main bus mute flag.
    pub ko_bus_mute: Option<bool>,
}

impl CueMixState {
    /// Field names in declaration order (used for missing-field reporting).
    pub const FIELD_NAMES: [&'static str; 5] = [
        "mute",
        "sample_rate_hz",
        "main_fader",
        "main_trim",
        "ko_bus_mute",
    ];

    /// Names of fields that were not received from the device.
    pub fn missing(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.mute.is_none() {
            missing.push(Self::FIELD_NAMES[0]);
        }
        if self.sample_rate_hz.is_none() {
            missing.push(Self::FIELD_NAMES[1]);
        }
        if self.main_fader.is_none() {
            missing.push(Self::FIELD_NAMES[2]);
        }
        if self.main_trim.is_none() {
            missing.push(Self::FIELD_NAMES[3]);
        }
        if self.ko_bus_mute.is_none() {
            missing.push(Self::FIELD_NAMES[4]);
        }
        missing
    }

    /// All five fields were received.
    pub fn is_complete(&self) -> bool {
        self.missing().is_empty()
    }
}

/// Static device identity values read from the datastore.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Model id (kModelID), e.g. "UL5L".
    pub model_id: Option<String>,
    /// Device name (kDevName), e.g. "UltraLite-mk5".
    pub name: Option<String>,
    /// Firmware version (kDevVersion).
    pub version: Option<String>,
    /// Control API version (kAPIVersion).
    pub api_version: Option<u16>,
    /// Device UID bytes (kDevUID).
    pub uid: Option<Vec<u8>>,
    /// Device MAC bytes (kDeviceMAC).
    pub mac: Option<Vec<u8>>,
}

impl DeviceInfo {
    /// A human model label when one of name/model id is known.
    pub fn display_name(&self) -> Option<&str> {
        self.name.as_deref().or(self.model_id.as_deref())
    }
}

/// One verified single-key write.
#[derive(Clone, Debug, PartialEq)]
pub struct WriteReport<T> {
    /// Datastore key written.
    pub key: &'static str,
    /// Datastore index written.
    pub index: u16,
    /// Value observed before the write (None when the key was not received yet).
    pub before: Option<T>,
    /// Value that was written (or the already-present target value).
    pub wrote: T,
    /// Value the device streamed back (equals wrote when the report exists).
    pub readback: T,
    /// The value actually changed.
    pub changed: bool,
    /// A write frame was transmitted (false when the target was already current).
    pub frame_sent: bool,
}

/// Report of set_mute.
pub type MuteReport = WriteReport<bool>;

/// Main output channel of the stereo main bus fader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainChannel {
    /// Main 1 (koBusFader[0], left).
    Left,
    /// Main 2 (koBusFader[1], right).
    Right,
}

impl MainChannel {
    /// Datastore index written for this channel.
    pub const fn index(self) -> u16 {
        match self {
            Self::Left => props::MAIN_FADER_LEFT_INDEX,
            Self::Right => props::MAIN_FADER_RIGHT_INDEX,
        }
    }

    /// Human label used in CLI output.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Left => "Main 1 (left)",
            Self::Right => "Main 2 (right)",
        }
    }

    /// Both channels in write order.
    pub const ALL: [Self; 2] = [Self::Left, Self::Right];
}

/// Report of a fully verified stereo volume change.
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeReport {
    /// Quantized linear value that was written to both channels.
    pub linear: f64,
    /// dB equivalent of the quantized linear value.
    pub db: Option<f64>,
    /// Left channel write report.
    pub left: WriteReport<f64>,
    /// Right channel write report.
    pub right: WriteReport<f64>,
}

/// Failure of set_volume: reports the channels already written.
#[derive(Debug)]
pub struct VolumeError {
    /// Verified writes that completed before the failure (0 or 1 entries).
    pub completed: Vec<WriteReport<f64>>,
    /// Underlying failure.
    pub error: CueMixError,
}

impl VolumeError {
    /// Stable error code of the underlying failure.
    pub fn code(&self) -> &'static str {
        self.error.code()
    }
}

impl fmt::Display for VolumeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.completed.is_empty() {
            write!(f, "volume not written: {}", self.error)
        } else {
            write!(
                f,
                "volume partially written ({} channel(s) verified): {}",
                self.completed.len(),
                self.error
            )
        }
    }
}

impl std::error::Error for VolumeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Report of save_snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct SnapshotReport {
    /// File written.
    pub path: PathBuf,
    /// Bytes written.
    pub bytes: usize,
    /// Non-null datastore values in the snapshot.
    pub datastore_values: usize,
    /// Datastore messages collected during the session.
    pub messages: u64,
    /// At least one datastore message was received.
    pub authenticated: bool,
}

#[derive(Debug, Default)]
struct AuthState {
    pubkey_pem: Option<String>,
    password_enabled: Option<bool>,
    serial: Option<String>,
    pending_auth_request: bool,
    response_sent: bool,
}

/// Datastore mirror: per-key arrays with None for not-yet-received indices.
#[derive(Debug, Default)]
pub(crate) struct Mirror {
    values: BTreeMap<&'static str, Vec<Option<Value>>>,
}

impl Mirror {
    fn set(&mut self, prop: &'static Prop, index: u16, value: Value) {
        let array = self.values.entry(prop.key).or_default();
        let index = index as usize;
        if array.len() <= index {
            array.resize(index + 1, None);
        }
        array[index] = Some(value);
    }

    fn get(&self, key: &str, index: u16) -> Option<&Value> {
        self.values
            .get(key)
            .and_then(|array| array.get(index as usize))
            .and_then(|value| value.as_ref())
    }

    fn arrays(&self) -> &BTreeMap<&'static str, Vec<Option<Value>>> {
        &self.values
    }

    fn value_count(&self) -> usize {
        self.values
            .values()
            .map(|array| array.iter().filter(|value| value.is_some()).count())
            .sum()
    }
}

/// Connected CueMix 5 control client.
pub struct CueMixClient {
    tp: Box<dyn Transport>,
    cfg: ClientConfig,
    mirror: Mirror,
    stats: Stats,
    auth: AuthState,
}

impl fmt::Debug for CueMixClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CueMixClient")
            .field("transport", &self.tp.description())
            .field("messages", &self.stats.messages)
            .field("authenticated", &self.stats.authenticated)
            .field("datastore_values", &self.mirror.value_count())
            .finish()
    }
}

impl CueMixClient {
    /// Connect over TCP + WebSocket and synchronize the initial datastore burst.
    pub fn connect(config: ClientConfig) -> Result<Self, CueMixError> {
        let transport = WsTransport::connect(&config.ws_config())?;
        Self::with_transport(Box::new(transport), config)
    }

    /// Build a client on an injected transport (tests, daemon fakes) and run
    /// the auth handshake + initial synchronization.
    pub fn with_transport(
        transport: Box<dyn Transport>,
        config: ClientConfig,
    ) -> Result<Self, CueMixError> {
        let mut client = Self {
            tp: transport,
            cfg: config,
            mirror: Mirror::default(),
            stats: Stats::default(),
            auth: AuthState::default(),
        };
        client.sync()?;
        Ok(client)
    }

    /// Configuration of this client.
    pub fn config(&self) -> &ClientConfig {
        &self.cfg
    }

    /// Session counters.
    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Transport description ("ws:host:port").
    pub fn transport_description(&self) -> String {
        self.tp.description()
    }

    /// A datastore value, when received.
    pub fn value(&self, key: &str, index: u16) -> Option<&Value> {
        self.mirror.get(key, index)
    }

    /// Byte value for a B property.
    pub fn byte_value(&self, key: &str, index: u16) -> Option<u8> {
        self.mirror.get(key, index).and_then(Value::as_u8)
    }

    /// Bool value for a B property (0 = false).
    pub fn bool_value(&self, key: &str, index: u16) -> Option<bool> {
        self.mirror.get(key, index).and_then(Value::as_bool)
    }

    /// i32 value for an I property.
    pub fn i32_value(&self, key: &str, index: u16) -> Option<i32> {
        self.mirror.get(key, index).and_then(Value::as_i32)
    }

    /// u16 value for an I16 property.
    pub fn u16_value(&self, key: &str, index: u16) -> Option<u16> {
        self.mirror.get(key, index).and_then(Value::as_u16)
    }

    /// f64 value for an F property.
    pub fn fader_value(&self, key: &str, index: u16) -> Option<f64> {
        self.mirror.get(key, index).and_then(Value::as_f64)
    }

    /// String value for an S property.
    pub fn text_value(&self, key: &str, index: u16) -> Option<&str> {
        self.mirror.get(key, index).and_then(Value::as_str)
    }

    /// Bytes value for an A property.
    pub fn bytes_value(&self, key: &str, index: u16) -> Option<&[u8]> {
        self.mirror.get(key, index).and_then(Value::as_bytes)
    }

    /// Number of non-null datastore values currently mirrored.
    pub fn datastore_value_count(&self) -> usize {
        self.mirror.value_count()
    }

    /// Static device identity (all fields Option: unknown = not reported).
    pub fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            model_id: self.text_value(keys::MODEL_ID, 0).map(str::to_string),
            name: self.text_value(keys::DEV_NAME, 0).map(str::to_string),
            version: self.text_value(keys::DEV_VERSION, 0).map(str::to_string),
            api_version: self.u16_value(keys::API_VERSION, 0),
            uid: self.bytes_value(keys::DEV_UID, 0).map(<[u8]>::to_vec),
            mac: self.bytes_value(keys::DEVICE_MAC, 0).map(<[u8]>::to_vec),
        }
    }

    /// Password required by the device, when it reported the flag.
    pub fn password_required(&self) -> Option<bool> {
        self.auth.password_enabled
    }

    /// Device serial sent with the auth request, when authentication happened.
    pub fn auth_serial(&self) -> Option<&str> {
        self.auth.serial.as_deref()
    }

    /// Drain pending updates, then report the current datastore state.
    pub fn read_state(&mut self) -> Result<CueMixState, CueMixError> {
        self.drain_idle()?;
        Ok(self.state_snapshot())
    }

    /// Report the mirrored datastore state without touching the transport.
    pub fn state_snapshot(&self) -> CueMixState {
        CueMixState {
            mute: self.bool_value(keys::MUTE_ENABLE, 0),
            sample_rate_hz: self
                .i32_value(keys::SAMPLE_RATE, 0)
                .and_then(|rate| u32::try_from(rate).ok()),
            main_fader: self.fader_value(keys::KO_BUS_FADER, props::MAIN_FADER_LEFT_INDEX),
            main_trim: self.byte_value(keys::MAIN_TRIM, 0),
            ko_bus_mute: self.bool_value(keys::KO_BUS_MUTE, 0),
        }
    }

    /// Write kMuteEnable[0] and verify the readback.
    pub fn set_mute(&mut self, on: bool) -> Result<MuteReport, CueMixError> {
        let prop = required_prop(keys::MUTE_ENABLE)?;
        let report = self.write_one(prop, 0, Value::Byte(u8::from(on)))?;
        Ok(WriteReport {
            key: report.key,
            index: report.index,
            before: report.before.as_ref().and_then(Value::as_bool),
            wrote: on,
            readback: report.readback.as_bool().unwrap_or(on),
            changed: report.changed,
            frame_sent: report.frame_sent,
        })
    }

    /// Write ONE main bus fader channel (koBusFader[index]) and verify readback.
    pub fn set_volume_channel(
        &mut self,
        channel: MainChannel,
        linear: f64,
    ) -> Result<WriteReport<f64>, CueMixError> {
        let quantized = fader::quantize_linear(linear)?;
        let prop = required_prop(keys::KO_BUS_FADER)?;
        let report = self.write_one(prop, channel.index(), Value::Fader(quantized))?;
        Ok(WriteReport {
            key: report.key,
            index: report.index,
            before: report.before.as_ref().and_then(Value::as_f64),
            wrote: quantized,
            readback: report.readback.as_f64().unwrap_or(quantized),
            changed: report.changed,
            frame_sent: report.frame_sent,
        })
    }

    /// Set the stereo main volume: two separate single-key writes, each with
    /// its own readback. On failure, VolumeError reports the channels already
    /// verified (never assumes the whole operation succeeded).
    pub fn set_volume(&mut self, linear: f64) -> Result<VolumeReport, VolumeError> {
        let quantized = fader::quantize_linear(linear).map_err(|error| VolumeError {
            completed: Vec::new(),
            error,
        })?;
        let left = self
            .set_volume_channel(MainChannel::Left, quantized)
            .map_err(|error| VolumeError {
                completed: Vec::new(),
                error,
            })?;
        let right = match self.set_volume_channel(MainChannel::Right, quantized) {
            Ok(report) => report,
            Err(error) => {
                return Err(VolumeError {
                    completed: vec![left],
                    error,
                })
            }
        };
        Ok(VolumeReport {
            linear: quantized,
            db: fader::fader_to_db(quantized),
            left,
            right,
        })
    }

    /// Collect the datastore for snapshot_drain, then write a read-only JSON
    /// snapshot (same shape as the reference client --save output).
    pub fn save_snapshot(&mut self, path: &Path) -> Result<SnapshotReport, CueMixError> {
        self.drain_for(self.cfg.snapshot_drain)?;
        let meta = SnapshotMeta {
            transport: self.transport_description(),
            model: self.text_value(keys::MODEL_ID, 0).map(str::to_string),
            name: self.text_value(keys::DEV_NAME, 0).map(str::to_string),
            uid: self.bytes_value(keys::DEV_UID, 0).map(<[u8]>::to_vec),
            authenticated: self.stats.authenticated,
            messages: self.stats.messages,
            unknown_ids: self.stats.unknown_ids,
            password_required: self.auth.password_enabled,
        };
        let json = snapshot::render(self.mirror.arrays(), &meta);
        std::fs::write(path, json.as_bytes())
            .map_err(|source| CueMixError::io("write snapshot", source))?;
        Ok(SnapshotReport {
            path: path.to_path_buf(),
            bytes: json.len(),
            datastore_values: self.mirror.value_count(),
            messages: self.stats.messages,
            authenticated: self.stats.authenticated,
        })
    }

    /// Best-effort close of the underlying transport.
    pub fn close(&mut self) {
        self.tp.close();
    }

    // -------------------------------------------------------------- internals

    fn sync(&mut self) -> Result<(), CueMixError> {
        let deadline = Instant::now() + self.cfg.sync_timeout;
        loop {
            if self.has_state_keys() {
                return Ok(());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match self.tp.recv_binary(remaining)? {
                Some(frame) => self.handle_frame(&frame)?,
                None => break,
            }
        }
        if self.auth.password_enabled == Some(true) && !self.stats.authenticated {
            return Err(CueMixError::AuthFailed {
                reason: "device required a password; no datastore data after the auth response"
                    .to_string(),
            });
        }
        Ok(())
    }

    fn has_state_keys(&self) -> bool {
        self.mirror.get(keys::MUTE_ENABLE, 0).is_some()
            && self.mirror.get(keys::SAMPLE_RATE, 0).is_some()
            && self
                .mirror
                .get(keys::KO_BUS_FADER, props::MAIN_FADER_LEFT_INDEX)
                .is_some()
            && self.mirror.get(keys::MAIN_TRIM, 0).is_some()
            && self.mirror.get(keys::KO_BUS_MUTE, 0).is_some()
    }

    fn handle_frame(&mut self, payload: &[u8]) -> Result<(), CueMixError> {
        for item in message::parse_frame(payload) {
            match item {
                Incoming::Datastore { prop, index, value } => {
                    self.mirror.set(prop, index, value);
                    self.stats.messages = self.stats.messages.saturating_add(1);
                    self.stats.authenticated = true;
                }
                Incoming::Proxy { sub_id, payload } => {
                    self.stats.proxy_messages = self.stats.proxy_messages.saturating_add(1);
                    self.handle_proxy(sub_id, &payload)?;
                }
                Incoming::ProxyStatus { status } => {
                    self.stats.proxy_messages = self.stats.proxy_messages.saturating_add(1);
                    self.stats.proxy_status = status;
                }
                Incoming::Meters { .. } => {}
                Incoming::Unknown { .. } => {
                    self.stats.unknown_ids = self.stats.unknown_ids.saturating_add(1);
                }
                Incoming::Malformed { .. } => {
                    self.stats.malformed = self.stats.malformed.saturating_add(1);
                }
                Incoming::Incomplete { .. } => {
                    self.stats.incomplete = self.stats.incomplete.saturating_add(1);
                }
            }
        }
        Ok(())
    }

    fn handle_proxy(&mut self, sub_id: u16, payload: &[u8]) -> Result<(), CueMixError> {
        match sub_id {
            message::PROXY_SUB_PUBLIC_KEY => {
                self.auth.pubkey_pem = Some(String::from_utf8_lossy(payload).into_owned());
                if self.auth.pending_auth_request {
                    self.send_auth_response()?;
                }
            }
            message::PROXY_SUB_PASSWORD_STATUS => {
                let enabled = payload.first().map(|byte| *byte != 0);
                self.auth.password_enabled = enabled;
                if enabled == Some(true) && self.cfg.password.is_none() {
                    return Err(CueMixError::AuthRequired);
                }
            }
            message::PROXY_SUB_AUTH_REQUEST => {
                let serial_len = payload.len().min(10);
                self.auth.serial =
                    Some(String::from_utf8_lossy(&payload[..serial_len]).into_owned());
                if self.cfg.password.is_none() {
                    return Err(CueMixError::AuthRequired);
                }
                if self.auth.pubkey_pem.is_some() {
                    self.send_auth_response()?;
                } else {
                    self.auth.pending_auth_request = true;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn send_auth_response(&mut self) -> Result<(), CueMixError> {
        if self.auth.response_sent {
            return Ok(());
        }
        let password = match self.cfg.password.clone() {
            Some(password) => password,
            None => return Err(CueMixError::AuthRequired),
        };
        let pem = match self.auth.pubkey_pem.clone() {
            Some(pem) => pem,
            None => {
                return Err(CueMixError::AuthFailed {
                    reason: "device requested auth before sending its public key".to_string(),
                })
            }
        };
        let cipher = crate::crypto::encrypt_password(&pem, &password)?;
        let frame = message::build_proxy(message::PROXY_SUB_AUTH_RESPONSE, &cipher);
        self.tp.send_binary(&frame)?;
        self.auth.response_sent = true;
        self.auth.pending_auth_request = false;
        self.stats.auth_response_sent = true;
        Ok(())
    }

    /// Read frames until the idle window passes without a datastore/proxy message.
    fn drain_idle(&mut self) -> Result<(), CueMixError> {
        let idle = self.cfg.idle_drain;
        let hard_deadline = Instant::now() + idle + MAX_IDLE_DRAIN;
        let mut last_progress = Instant::now();
        loop {
            let now = Instant::now();
            if now >= hard_deadline {
                return Ok(());
            }
            let elapsed = now.saturating_duration_since(last_progress);
            if elapsed >= idle {
                return Ok(());
            }
            let wait = idle - elapsed;
            let progress_before = self.stats.messages + self.stats.proxy_messages;
            match self.tp.recv_binary(wait)? {
                Some(frame) => {
                    self.handle_frame(&frame)?;
                    if self.stats.messages + self.stats.proxy_messages != progress_before {
                        last_progress = Instant::now();
                    }
                }
                None => return Ok(()),
            }
        }
    }

    /// Read datastore updates until the duration elapses (used by save).
    fn drain_for(&mut self, duration: Duration) -> Result<(), CueMixError> {
        let deadline = Instant::now() + duration;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            match self.tp.recv_binary(remaining)? {
                Some(frame) => self.handle_frame(&frame)?,
                None => return Ok(()),
            }
        }
    }

    /// Send exactly one datastore value in one frame, then wait for the device
    /// to stream the same value back. Never assumes success.
    fn write_one(
        &mut self,
        prop: &'static Prop,
        index: u16,
        target: Value,
    ) -> Result<WriteReport<Value>, CueMixError> {
        self.drain_idle()?;
        let before = self.mirror.get(prop.key, index).cloned();
        if before.as_ref() == Some(&target) {
            return Ok(WriteReport {
                key: prop.key,
                index,
                before,
                wrote: target.clone(),
                readback: target,
                changed: false,
                frame_sent: false,
            });
        }
        let payload = message::build_write(prop, index, &target)?;
        self.tp.send_binary(&payload)?;
        let deadline = Instant::now() + self.cfg.readback_timeout;
        let mut observed_other: Option<Value> = None;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match self.tp.recv_binary(remaining)? {
                Some(frame) => {
                    self.handle_frame(&frame)?;
                    match self.mirror.get(prop.key, index) {
                        Some(current) if *current == target => {
                            return Ok(WriteReport {
                                key: prop.key,
                                index,
                                before,
                                wrote: target.clone(),
                                readback: current.clone(),
                                changed: true,
                                frame_sent: true,
                            })
                        }
                        Some(current) => observed_other = Some(current.clone()),
                        None => {}
                    }
                }
                None => break,
            }
        }
        match observed_other {
            Some(actual) => Err(CueMixError::ReadbackMismatch {
                key: prop.key,
                index,
                expected: target.describe(),
                actual: actual.describe(),
            }),
            None => Err(CueMixError::ReadbackTimeout {
                key: prop.key,
                index,
            }),
        }
    }
}

fn required_prop(key: &str) -> Result<&'static Prop, CueMixError> {
    props::by_key(key).ok_or(CueMixError::Unsupported {
        what: "datastore property not in the UltraLite-mk5 table",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto;
    use crate::crypto::test_vectors as v;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum EchoMode {
        Normal,
        Wrong,
        Drop,
    }

    /// Shared handles of the fake transport, so tests can push device frames
    /// after the client (which owns the transport) was constructed.
    #[derive(Clone, Default)]
    struct FakeHandles {
        incoming: Arc<Mutex<VecDeque<Vec<u8>>>>,
        sent: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl FakeHandles {
        fn push(&self, frame: Vec<u8>) {
            self.incoming
                .lock()
                .expect("incoming lock")
                .push_back(frame);
        }

        fn push_update(&self, key: &'static str, index: u16, value: Value) {
            let prop = props::by_key(key).expect("known property");
            let mut frame = prop.id.to_be_bytes().to_vec();
            frame.extend_from_slice(&index.to_be_bytes());
            frame.extend_from_slice(&value.encode(prop.ty).expect("encode value"));
            self.push(frame);
        }

        fn push_proxy(&self, sub_id: u16, payload: &[u8]) {
            self.push(message::build_proxy(sub_id, payload));
        }

        fn push_state_burst(&self) {
            self.push_update(keys::MUTE_ENABLE, 0, Value::Byte(0));
            self.push_update(keys::SAMPLE_RATE, 0, Value::Int(48_000));
            self.push_update(
                keys::KO_BUS_FADER,
                props::MAIN_FADER_LEFT_INDEX,
                Value::Fader(1.0),
            );
            self.push_update(
                keys::KO_BUS_FADER,
                props::MAIN_FADER_RIGHT_INDEX,
                Value::Fader(1.0),
            );
            self.push_update(keys::MAIN_TRIM, 0, Value::Byte(31));
            self.push_update(keys::KO_BUS_MUTE, 0, Value::Byte(0));
            self.push_update(keys::MODEL_ID, 0, Value::Str("UL5L".to_string()));
            self.push_update(keys::DEV_NAME, 0, Value::Str("UltraLite-mk5".to_string()));
            self.push_update(keys::API_VERSION, 0, Value::UInt16(10));
        }

        fn sent(&self) -> Vec<Vec<u8>> {
            self.sent.lock().expect("sent lock").clone()
        }

        fn sent_len(&self) -> usize {
            self.sent.lock().expect("sent lock").len()
        }
    }

    struct FakeTransport {
        handles: FakeHandles,
        echo: EchoMode,
        drop_index: Option<u16>,
        closed: bool,
    }

    impl FakeTransport {
        fn new() -> (Self, FakeHandles) {
            let handles = FakeHandles::default();
            (
                Self {
                    handles: handles.clone(),
                    echo: EchoMode::Normal,
                    drop_index: None,
                    closed: false,
                },
                handles,
            )
        }
    }

    impl Transport for FakeTransport {
        fn send_binary(&mut self, payload: &[u8]) -> Result<(), CueMixError> {
            if self.closed {
                return Err(CueMixError::Closed);
            }
            self.handles
                .sent
                .lock()
                .expect("sent lock")
                .push(payload.to_vec());
            if self.echo == EchoMode::Drop || payload.len() < 6 {
                return Ok(());
            }
            let id = u16::from_be_bytes([payload[0], payload[1]]);
            if matches!(
                id,
                message::PROXY_ID | message::PROXY_STATUS_ID | message::METERS_ID
            ) {
                return Ok(());
            }
            let index = u16::from_be_bytes([payload[2], payload[3]]);
            if self.drop_index == Some(index) {
                return Ok(());
            }
            let len = u16::from_be_bytes([payload[4], payload[5]]) as usize;
            let available = payload.len().saturating_sub(6);
            let mut value = payload[6..6 + len.min(available)].to_vec();
            if self.echo == EchoMode::Wrong {
                if let Some(last) = value.last_mut() {
                    *last ^= 0xff;
                }
            }
            let mut frame = id.to_be_bytes().to_vec();
            frame.extend_from_slice(&index.to_be_bytes());
            frame.extend_from_slice(&value);
            self.handles
                .incoming
                .lock()
                .expect("incoming lock")
                .push_back(frame);
            Ok(())
        }

        fn recv_binary(&mut self, _timeout: Duration) -> Result<Option<Vec<u8>>, CueMixError> {
            Ok(self
                .handles
                .incoming
                .lock()
                .expect("incoming lock")
                .pop_front())
        }

        fn description(&self) -> String {
            "fake:test".to_string()
        }

        fn close(&mut self) {
            self.closed = true;
        }
    }

    fn test_config() -> ClientConfig {
        let mut config = ClientConfig::new(SocketAddr::from(([127, 0, 0, 1], 1280)));
        // Debug-build RSA-OAEP (Montgomery modexp) is CPU heavy; keep the
        // synchronization deadline generous so the auth test is not racy.
        config.sync_timeout = Duration::from_secs(30);
        config.readback_timeout = Duration::from_millis(50);
        config.idle_drain = Duration::from_millis(1);
        config.snapshot_drain = Duration::from_millis(1);
        config
    }

    fn connect_with(fake: FakeTransport) -> Result<CueMixClient, CueMixError> {
        CueMixClient::with_transport(Box::new(fake), test_config())
    }

    fn count_host_messages(frame: &[u8]) -> usize {
        let mut offset = 0usize;
        let mut count = 0usize;
        while frame.len() >= offset + 6 {
            let len = u16::from_be_bytes([frame[offset + 4], frame[offset + 5]]) as usize;
            offset += 6 + len;
            count += 1;
        }
        count
    }

    fn hex_bytes(text: &str) -> Vec<u8> {
        let compact: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        let padded = if compact.len() % 2 == 1 {
            format!("0{}", compact)
        } else {
            compact
        };
        crypto::hex_decode(&padded).expect("hex")
    }

    /// OAEP-SHA1 decrypt of a ciphertext with the test-only private exponent.
    fn decrypt_password_cipher(cipher: &[u8]) -> String {
        let key = crypto::parse_rsa_public_key_pem(v::RSA2048_PEM_SPKI).expect("key");
        let d = hex_bytes(v::RSA2048_D_HEX);
        let em = crypto::rsa_modpow(cipher, &d, &key.modulus_be).expect("decrypt");
        assert_eq!(em[0], 0);
        let body = &em[1..];
        let hlen = 20usize;
        let masked_seed = &body[..hlen];
        let masked_db = &body[hlen..];
        let seed_mgf = crypto::mgf1_sha1(masked_db, hlen);
        let seed: Vec<u8> = masked_seed
            .iter()
            .zip(seed_mgf)
            .map(|(left, right)| left ^ right)
            .collect();
        let db_mgf = crypto::mgf1_sha1(&seed, key.modulus_be.len() - hlen - 1);
        let db: Vec<u8> = masked_db
            .iter()
            .zip(db_mgf)
            .map(|(left, right)| left ^ right)
            .collect();
        let mut index = hlen;
        while db[index] == 0 {
            index += 1;
        }
        assert_eq!(db[index], 0x01);
        String::from_utf8(db[index + 1..].to_vec()).expect("digest is ASCII")
    }

    #[test]
    fn connects_reads_state_and_device_info() {
        let (fake, handles) = FakeTransport::new();
        handles.push_state_burst();
        let mut client = connect_with(fake).expect("connect");
        let state = client.read_state().expect("read state");
        assert_eq!(state.mute, Some(false));
        assert_eq!(state.sample_rate_hz, Some(48_000));
        assert_eq!(state.main_fader, Some(1.0));
        assert_eq!(state.main_trim, Some(31));
        assert_eq!(state.ko_bus_mute, Some(false));
        assert!(state.is_complete());

        let info = client.device_info();
        assert_eq!(info.model_id.as_deref(), Some("UL5L"));
        assert_eq!(info.name.as_deref(), Some("UltraLite-mk5"));
        assert_eq!(info.api_version, Some(10));
        assert_eq!(client.stats().messages, 9);
        assert!(client.stats().authenticated);
        assert_eq!(client.transport_description(), "fake:test");
    }

    #[test]
    fn read_state_reports_missing_fields_without_error() {
        let (fake, _handles) = FakeTransport::new();
        let mut client = connect_with(fake).expect("connect");
        let state = client.read_state().expect("read state");
        assert!(!state.is_complete());
        assert_eq!(
            state.missing(),
            vec![
                "mute",
                "sample_rate_hz",
                "main_fader",
                "main_trim",
                "ko_bus_mute"
            ]
        );
        assert!(client.device_info().name.is_none());
    }

    #[test]
    fn late_updates_are_drained_by_read_state() {
        let (fake, handles) = FakeTransport::new();
        handles.push_state_burst();
        let mut client = connect_with(fake).expect("connect");
        assert_eq!(client.state_snapshot().mute, Some(false));
        handles.push_update(keys::MUTE_ENABLE, 0, Value::Byte(1));
        let state = client.read_state().expect("read state");
        assert_eq!(state.mute, Some(true));
    }

    #[test]
    fn password_enabled_without_password_is_auth_required() {
        let (fake, handles) = FakeTransport::new();
        handles.push_proxy(message::PROXY_SUB_PASSWORD_STATUS, &[1]);
        let error = connect_with(fake).expect_err("must require password");
        assert_eq!(error.code(), crate::error::codes::AUTH_REQUIRED);
    }

    #[test]
    fn auth_request_without_password_is_auth_required() {
        let (fake, handles) = FakeTransport::new();
        handles.push_proxy(message::PROXY_SUB_AUTH_REQUEST, b"UL5LFE7E6E");
        let error = connect_with(fake).expect_err("must require password");
        assert_eq!(error.code(), crate::error::codes::AUTH_REQUIRED);
    }

    #[test]
    fn authenticates_and_sends_oaep_password_digest() {
        let (fake, handles) = FakeTransport::new();
        handles.push_proxy(
            message::PROXY_SUB_PUBLIC_KEY,
            v::RSA2048_PEM_SPKI.as_bytes(),
        );
        handles.push_proxy(message::PROXY_SUB_PASSWORD_STATUS, &[1]);
        handles.push_proxy(message::PROXY_SUB_AUTH_REQUEST, b"UL5LFE7E6E");
        handles.push_state_burst();
        let mut config = test_config();
        config.password = Some("khaen-test-password".to_string());
        let client = CueMixClient::with_transport(Box::new(fake), config).expect("connect");
        assert!(client.stats().auth_response_sent);
        assert!(client.stats().authenticated);
        assert_eq!(client.password_required(), Some(true));
        assert_eq!(client.auth_serial(), Some("UL5LFE7E6E"));

        let sent = handles.sent();
        let auth_frame = sent
            .iter()
            .find(|frame| frame.len() > 6 && frame[0..2] == message::PROXY_ID.to_be_bytes())
            .expect("auth response frame");
        assert_eq!(
            &auth_frame[2..4],
            &message::PROXY_SUB_AUTH_RESPONSE.to_be_bytes()
        );
        let len = u16::from_be_bytes([auth_frame[4], auth_frame[5]]) as usize;
        assert_eq!(len, 256);
        assert_eq!(auth_frame.len(), 6 + 256);
        assert_eq!(
            decrypt_password_cipher(&auth_frame[6..]),
            crypto::password_digest_hex("khaen-test-password")
        );
    }

    #[test]
    fn auth_failure_without_data_after_response() {
        let (fake, handles) = FakeTransport::new();
        handles.push_proxy(
            message::PROXY_SUB_PUBLIC_KEY,
            v::RSA2048_PEM_SPKI.as_bytes(),
        );
        handles.push_proxy(message::PROXY_SUB_PASSWORD_STATUS, &[1]);
        handles.push_proxy(message::PROXY_SUB_AUTH_REQUEST, b"UL5LFE7E6E");
        let mut config = test_config();
        config.password = Some("wrong".to_string());
        let error = CueMixClient::with_transport(Box::new(fake), config).expect_err("auth fails");
        assert_eq!(error.code(), crate::error::codes::AUTH_FAILED);
    }

    #[test]
    fn set_mute_sends_one_frame_and_verifies_readback() {
        let (fake, handles) = FakeTransport::new();
        handles.push_state_burst();
        let mut client = connect_with(fake).expect("connect");
        let report = client.set_mute(true).expect("set mute");
        assert_eq!(report.key, keys::MUTE_ENABLE);
        assert_eq!(report.index, 0);
        assert_eq!(report.before, Some(false));
        assert!(report.wrote);
        assert!(report.readback);
        assert!(report.changed);
        assert!(report.frame_sent);
        let sent = handles.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(count_host_messages(&sent[0]), 1);
        assert_eq!(sent[0], vec![0x13, 0x9b, 0x00, 0x00, 0x00, 0x01, 0x01]);
        assert_eq!(client.state_snapshot().mute, Some(true));
    }

    #[test]
    fn set_mute_is_a_noop_when_already_at_target() {
        let (fake, handles) = FakeTransport::new();
        handles.push_state_burst();
        handles.push_update(keys::MUTE_ENABLE, 0, Value::Byte(1));
        let mut client = connect_with(fake).expect("connect");
        let state = client.read_state().expect("read");
        assert_eq!(state.mute, Some(true));
        let report = client.set_mute(true).expect("set mute");
        assert!(!report.changed);
        assert!(!report.frame_sent);
        assert!(report.readback);
        assert_eq!(handles.sent_len(), 0);
    }

    #[test]
    fn readback_mismatch_has_stable_code_and_details() {
        let (mut fake, handles) = FakeTransport::new();
        handles.push_state_burst();
        fake.echo = EchoMode::Wrong;
        let mut client = connect_with(fake).expect("connect");
        let error = client.set_mute(true).expect_err("mismatch");
        assert_eq!(error.code(), crate::error::codes::READBACK_MISMATCH);
        let text = error.to_string();
        assert!(text.contains("kMuteEnable[0]"), "error was {}", text);
        assert!(text.contains("wrote 1"), "error was {}", text);
    }

    #[test]
    fn readback_timeout_has_stable_code() {
        let (mut fake, handles) = FakeTransport::new();
        handles.push_state_burst();
        fake.echo = EchoMode::Drop;
        let mut client = connect_with(fake).expect("connect");
        let error = client.set_mute(true).expect_err("timeout");
        assert_eq!(error.code(), crate::error::codes::READBACK_TIMEOUT);
        assert!(error.is_retryable());
    }

    #[test]
    fn set_volume_writes_two_separate_single_key_frames() {
        let (fake, handles) = FakeTransport::new();
        handles.push_state_burst();
        let mut client = connect_with(fake).expect("connect");
        let report = client.set_volume(0.5).expect("set volume");
        assert_eq!(report.linear, 0.5);
        let db = report.db.expect("db");
        assert!((db - -6.020_599_913_3).abs() < 1e-6, "db = {}", db);
        assert_eq!(report.left.index, props::MAIN_FADER_LEFT_INDEX);
        assert_eq!(report.right.index, props::MAIN_FADER_RIGHT_INDEX);
        assert_eq!(report.left.before, Some(1.0));
        assert_eq!(report.left.readback, 0.5);
        assert_eq!(report.right.readback, 0.5);
        let sent = handles.sent();
        assert_eq!(sent.len(), 2);
        for frame in &sent {
            assert_eq!(count_host_messages(frame), 1, "one message per frame");
        }
        assert_eq!(&sent[0][..4], &[0x04, 0x03, 0x00, 0x00]);
        assert_eq!(&sent[1][..4], &[0x04, 0x03, 0x00, 0x01]);
        assert_eq!(client.state_snapshot().main_fader, Some(0.5));
    }

    #[test]
    fn set_volume_channel_writes_only_that_channel() {
        let (fake, handles) = FakeTransport::new();
        handles.push_state_burst();
        let mut client = connect_with(fake).expect("connect");
        let report = client
            .set_volume_channel(MainChannel::Right, 0.25)
            .expect("right channel");
        assert_eq!(report.index, props::MAIN_FADER_RIGHT_INDEX);
        let sent = handles.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(&sent[0][..4], &[0x04, 0x03, 0x00, 0x01]);
    }

    #[test]
    fn set_volume_rejects_out_of_range_without_any_write() {
        let (fake, handles) = FakeTransport::new();
        handles.push_state_burst();
        let mut client = connect_with(fake).expect("connect");
        let error = client.set_volume(1.5).expect_err("range error");
        assert_eq!(error.code(), crate::error::codes::RANGE);
        assert!(error.completed.is_empty());
        assert_eq!(handles.sent_len(), 0);
        let error = client.set_volume(f64::NAN).expect_err("nan");
        assert_eq!(error.code(), crate::error::codes::RANGE);
        assert_eq!(handles.sent_len(), 0);
    }

    #[test]
    fn set_volume_partial_failure_reports_left_channel() {
        let (mut fake, handles) = FakeTransport::new();
        handles.push_state_burst();
        fake.drop_index = Some(props::MAIN_FADER_RIGHT_INDEX);
        let mut client = connect_with(fake).expect("connect");
        let error = client.set_volume(0.5).expect_err("right channel timeout");
        assert_eq!(error.code(), crate::error::codes::READBACK_TIMEOUT);
        assert_eq!(error.completed.len(), 1);
        assert_eq!(error.completed[0].index, props::MAIN_FADER_LEFT_INDEX);
        assert_eq!(error.completed[0].readback, 0.5);
        assert!(error.to_string().contains("partially"));
        assert_eq!(handles.sent_len(), 2);
    }

    #[test]
    fn unknown_ids_and_malformed_payloads_are_counted_not_fatal() {
        let (fake, handles) = FakeTransport::new();
        let mut unknown = 9999u16.to_be_bytes().to_vec();
        unknown.extend_from_slice(&[0x00, 0x00, 0xaa]);
        handles.push(unknown);
        handles.push(vec![0x00, 0x0a, 0x00, 0x00, 0x00, 0x00]);
        handles.push_state_burst();
        let client = connect_with(fake).expect("connect");
        assert_eq!(client.stats().unknown_ids, 1);
        assert_eq!(client.stats().malformed, 1);
        assert!(client.stats().authenticated);
    }

    #[test]
    fn save_snapshot_writes_read_only_json() {
        let (fake, handles) = FakeTransport::new();
        handles.push_state_burst();
        let mut client = connect_with(fake).expect("connect");
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "khaen-motu-snapshot-test-{}-{}.json",
            std::process::id(),
            nanos
        ));
        let report = client.save_snapshot(&path).expect("save");
        let text = std::fs::read_to_string(&path).expect("read snapshot");
        let _ = std::fs::remove_file(&path);
        assert!(report.bytes > 0);
        assert_eq!(report.bytes, text.len());
        assert!(report.datastore_values >= 9);
        assert!(text.contains("\"kMuteEnable\": [0]"));
        assert!(text.contains("\"koBusFader\": [1.0,1.0]"));
        assert!(text.contains("\"model\": \"UL5L\""));
    }

    #[test]
    fn connect_failure_from_transport_is_propagated() {
        struct FailingTransport;
        impl Transport for FailingTransport {
            fn send_binary(&mut self, _payload: &[u8]) -> Result<(), CueMixError> {
                Err(CueMixError::Closed)
            }
            fn recv_binary(&mut self, _timeout: Duration) -> Result<Option<Vec<u8>>, CueMixError> {
                Err(CueMixError::Closed)
            }
            fn description(&self) -> String {
                "failing".to_string()
            }
            fn close(&mut self) {}
        }
        let error = CueMixClient::with_transport(Box::new(FailingTransport), test_config())
            .expect_err("closed");
        assert_eq!(error.code(), crate::error::codes::CLOSED);
    }

    #[test]
    fn resolve_control_addr_accepts_ip_and_localhost() {
        let addr = resolve_control_addr("127.0.0.1", 1280).expect("resolve");
        assert_eq!(addr.port(), 1280);
        let addr = resolve_control_addr("localhost", 1281).expect("resolve localhost");
        assert_eq!(addr.port(), 1281);
    }

    #[test]
    fn decrypt_helper_recovers_password_digest() {
        let cipher =
            crypto::encrypt_password(v::RSA2048_PEM_SPKI, "khaen-test-password").expect("encrypt");
        assert_eq!(cipher.len(), 256);
        assert_eq!(
            decrypt_password_cipher(&cipher),
            crypto::password_digest_hex("khaen-test-password")
        );
    }
}
