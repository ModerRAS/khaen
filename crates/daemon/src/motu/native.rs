//! 生产 MOTU 控制面实现：crates/motu-cuemix 的阻塞客户端 + daemon 侧生命周期。
//!
//! - 检测复用 crate 的 `detect_with`（/proc/asound/cards 路径 + TCP probe 均可注入）；
//!   Windows / 无设备 -> detected=false + 原因，不报错。
//! - 连接懒建立（第一次 read/write 时 `CueMixClient::connect`），成功后复用；任何一次设备
//!   操作失败就丢弃连接，下一次请求重新建连（不自动重试写，避免一次请求发多帧）。
//! - 设备访问在 `Mutex` 上串行化：并发 handler 的写不会交错到同一个 WebSocket 连接上。
//! - 所有方法都是阻塞 IO，调用方（API 层）负责放进 tokio blocking 线程池。

use std::fmt;
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use khaen_motu_cuemix::{
    keys, ClientConfig, CueMixClient, DetectConfig, DeviceDetection, MainChannel,
};

use super::{
    device_codes, MotuControl, MotuError, MotuReading, WriteReceipt, CHANNEL_COUNT, REASON_DISABLED,
};

/// `KHAEN_MOTU_DISABLE`：显式关闭 MOTU 控制（1/true/on 关闭，0/false/off 启用）。
pub const DISABLE_ENV: &str = "KHAEN_MOTU_DISABLE";
/// `KHAEN_MOTU_CARDS`：ALSA cards 文件路径覆盖（测试/非标准发行版）。
pub const CARDS_ENV: &str = "KHAEN_MOTU_CARDS";
/// `KHAEN_MOTU_ADDR`：USB-CDC 控制网地址覆盖（默认 `169.254.111.126:1280`）。
pub const ADDR_ENV: &str = "KHAEN_MOTU_ADDR";
/// `KHAEN_MOTU_PROBE_MS`：TCP probe 超时（毫秒，1..=60000）。
pub const PROBE_MS_ENV: &str = "KHAEN_MOTU_PROBE_MS";
/// `KHAEN_MOTU_PASSWORD`：控制网密码（设备启用密码时需要）。
pub const PASSWORD_ENV: &str = "KHAEN_MOTU_PASSWORD";
/// crates/motu-cuemix CLI 用的密码环境变量（daemon 也接受它作为回退）。
pub const CUEMIX_PASSWORD_ENV: &str = "KHAEN_CUEMIX_PASSWORD";

/// 检测用 TCP probe 函数（可注入：测试不碰真实网络）。
pub type DetectionProbe = Box<dyn Fn(SocketAddr, Duration) -> bool + Send + Sync>;

/// 生产 MOTU 控制面的启动配置（daemon 入口解析环境变量后注入，handler 不读环境）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MotuConfig {
    /// /proc/asound/cards 路径。
    pub cards_path: PathBuf,
    /// USB-CDC 控制网地址。
    pub control_addr: SocketAddr,
    /// TCP probe 超时。
    pub probe_timeout: Duration,
    /// 控制网密码（None = 设备未启用密码）。
    pub password: Option<String>,
    /// 显式关闭（KHAEN_MOTU_DISABLE=1）：检测如实报 `motu_control_disabled`。
    pub disabled: bool,
}

impl Default for MotuConfig {
    fn default() -> Self {
        let detect = DetectConfig::default();
        Self {
            cards_path: detect.cards_path,
            control_addr: detect.control_addr,
            probe_timeout: detect.probe_timeout,
            password: None,
            disabled: false,
        }
    }
}

impl MotuConfig {
    /// 从进程环境解析（daemon 入口唯一读这些环境变量的地方；非法值返回错误，入口退出码 2）。
    pub fn from_env() -> Result<Self, String> {
        Self::from_reader(|key| std::env::var(key).ok())
    }

    /// 纯解析（读环境函数注入，测试不改进程环境）。
    pub fn from_reader(read: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        // 一般值去首尾空白；密码保持原样（只把全空白当未设置），免得口令里的空格被吃掉。
        let trimmed = |key: &str| {
            read(key).and_then(|value| {
                let trimmed = value.trim().to_string();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            })
        };
        let raw = |key: &str| read(key).filter(|value| !value.trim().is_empty());
        let mut config = Self::default();
        if let Some(path) = trimmed(CARDS_ENV) {
            config.cards_path = PathBuf::from(path);
        }
        if let Some(raw) = trimmed(ADDR_ENV) {
            config.control_addr = raw.parse::<SocketAddr>().map_err(|error| {
                format!("{ADDR_ENV}={raw:?} is not a socket address (expected host:port): {error}")
            })?;
        }
        if let Some(raw) = trimmed(PROBE_MS_ENV) {
            let millis: u64 = raw.parse().map_err(|error| {
                format!("{PROBE_MS_ENV}={raw:?} is not an integer number of milliseconds: {error}")
            })?;
            if !(1..=60_000).contains(&millis) {
                return Err(format!("{PROBE_MS_ENV}={millis} is outside 1..=60000"));
            }
            config.probe_timeout = Duration::from_millis(millis);
        }
        config.password = raw(PASSWORD_ENV).or_else(|| raw(CUEMIX_PASSWORD_ENV));
        config.disabled = parse_disable_flag(trimmed(DISABLE_ENV).as_deref())?;
        Ok(config)
    }

    /// crate 的检测配置（复用其默认地址/超时语义）。
    pub fn detect_config(&self) -> DetectConfig {
        DetectConfig {
            cards_path: self.cards_path.clone(),
            control_addr: self.control_addr,
            probe_timeout: self.probe_timeout,
        }
    }
}

/// 解析关闭开关：未设置/0/false/off 启用；1/true/on 关闭；其它值报错（不静默猜）。
pub fn parse_disable_flag(raw: Option<&str>) -> Result<bool, String> {
    let Some(raw) = raw else {
        return Ok(false);
    };
    match raw.to_ascii_lowercase().as_str() {
        "" | "0" | "false" | "off" | "no" => Ok(false),
        "1" | "true" | "on" | "yes" => Ok(true),
        other => Err(format!(
            "{DISABLE_ENV}={other:?} is not recognized: use 1/true/on to disable MOTU control, 0/false/off to enable it"
        )),
    }
}

/// 默认 TCP probe（1 s 级 connect 尝试，失败即 false，不重试）。
pub fn tcp_probe(addr: SocketAddr, timeout: Duration) -> bool {
    TcpStream::connect_timeout(&addr, timeout).is_ok()
}

/// 复用中的控制连接（失败后置 None，下次请求重建）。
#[derive(Default)]
struct Connection {
    client: Option<CueMixClient>,
}

impl fmt::Debug for Connection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Connection")
            .field("connected", &self.client.is_some())
            .finish()
    }
}

/// 生产实现：懒连接的 CueMix 客户端 + 串行化的设备访问。
pub struct NativeMotu {
    config: MotuConfig,
    probe: DetectionProbe,
    client_config: ClientConfig,
    connection: Mutex<Connection>,
}

impl fmt::Debug for NativeMotu {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeMotu")
            .field("config", &self.config)
            .field("connection", &self.lock())
            .finish()
    }
}

impl Default for NativeMotu {
    fn default() -> Self {
        Self::new(MotuConfig::default())
    }
}

impl NativeMotu {
    /// 用显式配置构造（默认 probe = [tcp_probe]）。
    pub fn new(config: MotuConfig) -> Self {
        let mut client_config = ClientConfig::new(config.control_addr);
        if let Some(password) = &config.password {
            client_config = client_config.with_password(password.clone());
        }
        Self {
            config,
            probe: Box::new(tcp_probe),
            client_config,
            connection: Mutex::new(Connection::default()),
        }
    }

    /// 从环境变量构造（daemon 入口调用；非法值返回错误）。
    pub fn from_env() -> Result<Self, String> {
        Ok(Self::new(MotuConfig::from_env()?))
    }

    /// 注入检测 probe（单元测试用；不碰真实网络）。
    pub fn with_probe(mut self, probe: DetectionProbe) -> Self {
        self.probe = probe;
        self
    }

    /// 启动配置。
    pub fn config(&self) -> &MotuConfig {
        &self.config
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 取（或建立）控制连接。
    fn connect<'a>(
        &self,
        connection: &'a mut Connection,
    ) -> Result<&'a mut CueMixClient, MotuError> {
        if connection.client.is_none() {
            let client = CueMixClient::connect(self.client_config.clone())
                .map_err(|error| MotuError::from_device(&error))?;
            connection.client = Some(client);
        }
        match connection.client.as_mut() {
            Some(client) => Ok(client),
            None => Err(MotuError::new(
                device_codes::IO,
                "MOTU control connection was not established",
            )),
        }
    }

    /// 读设备状态（调用方已持有 connection 锁）。
    fn read_locked(&self, connection: &mut Connection) -> Result<MotuReading, MotuError> {
        let client = self.connect(connection)?;
        let state = client
            .read_state()
            .map_err(|error| MotuError::from_device(&error))?;
        let mut main_faders = Vec::with_capacity(CHANNEL_COUNT as usize);
        for index in 0..CHANNEL_COUNT {
            main_faders.push(client.fader_value(keys::KO_BUS_FADER, index));
        }
        let info = client.device_info();
        let mut missing: Vec<String> = state.missing().into_iter().map(str::to_string).collect();
        for (position, fader) in main_faders.iter().enumerate() {
            if fader.is_none() {
                missing.push(format!("vol[{}]", position + 1));
            }
        }
        Ok(MotuReading {
            model_id: info.model_id,
            device_name: info.name,
            version: info.version,
            api_version: info.api_version,
            mute: state.mute,
            sample_rate_hz: state.sample_rate_hz,
            main_faders,
            main_trim: state.main_trim,
            main_bus_mute: state.ko_bus_mute,
            missing,
        })
    }

    /// 单键写 kMuteEnable[0]（调用方已持有 connection 锁）。
    fn write_mute_locked(
        &self,
        connection: &mut Connection,
        mute: bool,
    ) -> Result<WriteReceipt<bool>, MotuError> {
        let client = self.connect(connection)?;
        let report = client
            .set_mute(mute)
            .map_err(|error| MotuError::from_device(&error))?;
        Ok(WriteReceipt {
            key: report.key,
            index: report.index,
            before: report.before,
            wrote: report.wrote,
            readback: report.readback,
            changed: report.changed,
            frame_sent: report.frame_sent,
        })
    }

    /// 单键写 koBusFader[channel_index]（调用方已持有 connection 锁）。
    fn write_volume_locked(
        &self,
        connection: &mut Connection,
        channel_index: u16,
        linear: f64,
    ) -> Result<WriteReceipt<f64>, MotuError> {
        let channel = match channel_index {
            index if index == MainChannel::Left.index() => MainChannel::Left,
            index if index == MainChannel::Right.index() => MainChannel::Right,
            other => {
                return Err(MotuError::new(
                    device_codes::UNSUPPORTED,
                    format!(
                        "MOTU main channel index {other} is not addressable by this daemon (only 0/1 = Main 1/Main 2)"
                    ),
                ))
            }
        };
        let client = self.connect(connection)?;
        let report = client
            .set_volume_channel(channel, linear)
            .map_err(|error| MotuError::from_device(&error))?;
        Ok(WriteReceipt {
            key: report.key,
            index: report.index,
            before: report.before,
            wrote: report.wrote,
            readback: report.readback,
            changed: report.changed,
            frame_sent: report.frame_sent,
        })
    }

    /// 失败时丢弃连接（下一次请求重建），避免继续用一个坏连接。
    fn release_on_error<T>(&self, connection: &mut Connection, result: &Result<T, MotuError>) {
        if result.is_err() {
            connection.client = None;
        }
    }
}

impl MotuControl for NativeMotu {
    fn detection(&self) -> DeviceDetection {
        if self.config.disabled {
            return DeviceDetection {
                detected: false,
                card_id: None,
                card_index: None,
                model: None,
                control_reachable: false,
                reason: REASON_DISABLED,
            };
        }
        let probe = &self.probe;
        khaen_motu_cuemix::detect_with(&self.config.detect_config(), None, &**probe)
    }

    fn read(&self) -> Result<MotuReading, MotuError> {
        let mut connection = self.lock();
        let result = self.read_locked(&mut connection);
        self.release_on_error(&mut connection, &result);
        result
    }

    fn write_mute(&self, mute: bool) -> Result<WriteReceipt<bool>, MotuError> {
        let mut connection = self.lock();
        let result = self.write_mute_locked(&mut connection, mute);
        self.release_on_error(&mut connection, &result);
        result
    }

    fn write_volume(
        &self,
        channel_index: u16,
        linear: f64,
    ) -> Result<WriteReceipt<f64>, MotuError> {
        let mut connection = self.lock();
        let result = self.write_volume_locked(&mut connection, channel_index, linear);
        self.release_on_error(&mut connection, &result);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use khaen_motu_cuemix::{
        DEFAULT_CARDS_PATH, DEFAULT_CONTROL_ADDR, DEFAULT_PROBE_TIMEOUT, REASON_CARDS_UNAVAILABLE,
        REASON_CONTROL_UNREACHABLE, REASON_DETECTED, REASON_NO_MOTU_CARD,
    };

    const N150_CARDS: &str = " 0 [UltraLitemk5   ]: USB-Audio - UltraLite-mk5\n                      MOTU UltraLite-mk5 at usb-0000:00:14.0-2.1, high speed\n 1 [U18dB          ]: USB-Audio - Umik-1  Gain: 18dB\n";
    const NO_MOTU_CARDS: &str = " 3 [PCH            ]: HDA-Intel - HDA Intel PCH\n 1 [U18dB          ]: USB-Audio - Umik-1  Gain: 18dB\n";

    /// 唯一临时文件名（测试之间不共享）。
    fn temp_cards(tag: &str, text: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "khaen-motu-cards-{}-{tag}-{unique}.txt",
            std::process::id()
        ));
        std::fs::write(&path, text).expect("write cards fixture");
        path
    }

    fn config_with_cards(cards_path: PathBuf) -> MotuConfig {
        MotuConfig {
            cards_path,
            control_addr: SocketAddr::from(([127, 0, 0, 1], 1)),
            probe_timeout: Duration::from_millis(20),
            password: None,
            disabled: false,
        }
    }

    #[test]
    fn detection_uses_the_injected_cards_path_and_probe() {
        let cards = temp_cards("ready", N150_CARDS);
        let probe_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = std::sync::Arc::clone(&probe_calls);
        let native = NativeMotu::new(config_with_cards(cards.clone())).with_probe(Box::new(
            move |addr, timeout| {
                seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                assert_eq!(addr, SocketAddr::from(([127, 0, 0, 1], 1)));
                assert_eq!(timeout, Duration::from_millis(20));
                true
            },
        ));
        let detection = native.detection();
        assert!(detection.is_ready());
        assert_eq!(detection.reason, REASON_DETECTED);
        assert_eq!(detection.model.as_deref(), Some("UltraLite-mk5"));
        assert_eq!(detection.card_index, Some(0));
        assert_eq!(
            probe_calls.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "probe runs exactly once when the MOTU card is present"
        );
        let _ = std::fs::remove_file(cards);
    }

    #[test]
    fn unreachable_control_network_is_reported_without_error() {
        let cards = temp_cards("unreachable", N150_CARDS);
        let native =
            NativeMotu::new(config_with_cards(cards.clone())).with_probe(Box::new(|_, _| false));
        let detection = native.detection();
        assert!(detection.detected);
        assert!(!detection.control_reachable);
        assert_eq!(detection.reason, REASON_CONTROL_UNREACHABLE);
        let _ = std::fs::remove_file(cards);
    }

    #[test]
    fn no_motu_card_and_missing_cards_file_are_not_errors() {
        let cards = temp_cards("no-motu", NO_MOTU_CARDS);
        let probe = Box::new(|_, _| -> bool { panic!("probe must not run without a MOTU card") });
        let native = NativeMotu::new(config_with_cards(cards.clone())).with_probe(probe);
        let detection = native.detection();
        assert!(!detection.detected);
        assert_eq!(detection.reason, REASON_NO_MOTU_CARD);
        let _ = std::fs::remove_file(cards);

        let missing = std::env::temp_dir().join("khaen-motu-definitely-missing-cards");
        let probe = Box::new(|_, _| -> bool { panic!("probe must not run without a cards file") });
        let native = NativeMotu::new(config_with_cards(missing)).with_probe(probe);
        let detection = native.detection();
        assert!(!detection.detected);
        assert_eq!(detection.reason, REASON_CARDS_UNAVAILABLE);
    }

    #[test]
    fn disabled_control_reports_the_disabled_reason_and_skips_expensive_work() {
        let mut config = config_with_cards(PathBuf::from("/proc/asound/cards"));
        config.disabled = true;
        let probe = Box::new(|_, _| -> bool { panic!("probe must not run when disabled") });
        let native = NativeMotu::new(config).with_probe(probe);
        let detection = native.detection();
        assert!(!detection.detected);
        assert_eq!(detection.reason, REASON_DISABLED);
        assert!(!native.read().is_ok());
    }

    #[test]
    fn default_config_points_at_the_linux_cards_file_and_default_endpoint() {
        let config = MotuConfig::default();
        assert_eq!(config.cards_path, PathBuf::from(DEFAULT_CARDS_PATH));
        assert_eq!(
            config.control_addr,
            DEFAULT_CONTROL_ADDR
                .parse::<SocketAddr>()
                .expect("default addr")
        );
        assert_eq!(config.probe_timeout, DEFAULT_PROBE_TIMEOUT);
        assert!(!config.disabled);
        assert!(config.password.is_none());
    }

    #[test]
    fn from_reader_parses_overrides_and_defaults() {
        let values: std::collections::HashMap<&str, &str> = std::collections::HashMap::from([
            (CARDS_ENV, "/tmp/cards"),
            (ADDR_ENV, "10.0.0.9:1700"),
            (PROBE_MS_ENV, "250"),
            (CUEMIX_PASSWORD_ENV, "secret"),
        ]);
        let config = MotuConfig::from_reader(|key| values.get(key).map(|value| value.to_string()))
            .expect("config");
        assert_eq!(config.cards_path, PathBuf::from("/tmp/cards"));
        assert_eq!(
            config.control_addr,
            "10.0.0.9:1700".parse::<SocketAddr>().expect("addr")
        );
        assert_eq!(config.probe_timeout, Duration::from_millis(250));
        assert_eq!(config.password.as_deref(), Some("secret"));
        assert!(!config.disabled);

        let config = MotuConfig::from_reader(|_| None).expect("defaults");
        assert_eq!(config, MotuConfig::default());
    }

    #[test]
    fn from_reader_rejects_invalid_values() {
        for (key, value) in [
            (ADDR_ENV, "not-an-address"),
            (PROBE_MS_ENV, "0"),
            (PROBE_MS_ENV, "60001"),
            (PROBE_MS_ENV, "fast"),
            (DISABLE_ENV, "maybe"),
        ] {
            let error =
                MotuConfig::from_reader(|requested| (requested == key).then(|| value.to_string()))
                    .expect_err("invalid value must be rejected");
            assert!(!error.is_empty(), "{key}={value}");
        }
    }

    #[test]
    fn password_is_kept_verbatim_but_whitespace_only_counts_as_unset() {
        let values: std::collections::HashMap<&str, &str> =
            std::collections::HashMap::from([(PASSWORD_ENV, "  pa ss  ")]);
        let config = MotuConfig::from_reader(|key| values.get(key).map(|value| value.to_string()))
            .expect("config");
        assert_eq!(config.password.as_deref(), Some("  pa ss  "));

        let values: std::collections::HashMap<&str, &str> =
            std::collections::HashMap::from([(PASSWORD_ENV, "   ")]);
        let config = MotuConfig::from_reader(|key| values.get(key).map(|value| value.to_string()))
            .expect("config");
        assert!(config.password.is_none());
    }

    #[test]
    fn explicit_password_wins_over_the_cli_fallback() {
        let values: std::collections::HashMap<&str, &str> = std::collections::HashMap::from([
            (PASSWORD_ENV, "daemon-password"),
            (CUEMIX_PASSWORD_ENV, "cli-password"),
        ]);
        let config = MotuConfig::from_reader(|key| values.get(key).map(|value| value.to_string()))
            .expect("config");
        assert_eq!(config.password.as_deref(), Some("daemon-password"));
    }

    #[test]
    fn disable_flag_is_strict() {
        assert!(!parse_disable_flag(None).expect("unset"));
        assert!(!parse_disable_flag(Some("0")).expect("0"));
        assert!(!parse_disable_flag(Some("off")).expect("off"));
        assert!(parse_disable_flag(Some("1")).expect("1"));
        assert!(parse_disable_flag(Some("ON")).expect("ON"));
        assert!(parse_disable_flag(Some("anything-else")).is_err());
    }
}
