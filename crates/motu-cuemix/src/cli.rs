//! khaen-motu command line: info / mute on|off / volume <0..1|dB> / save <file>.
//!
//! Exit codes are stable:
//!   0 OK, 2 usage error, 3 device absent or control network unreachable,
//!   4 transport/auth/protocol failure, 5 write readback verification failed.
//!
//! Write commands send one key per frame and print before / wrote / readback.

use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::client::{
    resolve_control_addr, ClientConfig, CueMixClient, CueMixState, DeviceInfo, MainChannel, Stats,
};
use crate::detect::{self, DetectConfig, DeviceDetection};
use crate::error::{codes, CueMixError};
use crate::fader;
use crate::props::keys;
use crate::snapshot;

/// Successful exit.
pub const EXIT_OK: i32 = 0;
/// Usage error (bad arguments, out-of-range volume).
pub const EXIT_USAGE: i32 = 2;
/// Device absent or control network unreachable.
pub const EXIT_DEVICE: i32 = 3;
/// Transport, handshake, auth or protocol failure.
pub const EXIT_TRANSPORT: i32 = 4;
/// Write could not be verified (readback timeout / mismatch).
pub const EXIT_READBACK: i32 = 5;

/// Environment variable read for the control-network password.
pub const PASSWORD_ENV: &str = "KHAEN_CUEMIX_PASSWORD";

const DEFAULT_HOST: &str = "169.254.111.126";
const DEFAULT_PORT: u16 = 1280;
const PROBE_TIMEOUT: Duration = Duration::from_millis(1000);

/// Global CLI options.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// Control host (IP or name).
    pub host: String,
    /// Control port.
    pub port: u16,
    /// Control-network password.
    pub password: Option<String>,
    /// ALSA cards file used by detection.
    pub cards_path: PathBuf,
    /// TCP connect + handshake timeout.
    pub connect_timeout: Duration,
    /// Initial datastore sync timeout.
    pub sync_timeout: Duration,
    /// Readback deadline per write.
    pub readback_timeout: Duration,
    /// Snapshot collection window (None = client default).
    pub drain: Option<Duration>,
    /// Probe the control network before writing.
    pub probe: bool,
    /// Emit JSON (info).
    pub json: bool,
    /// Treat "not ready" as an error for info.
    pub strict: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_string(),
            port: DEFAULT_PORT,
            password: None,
            cards_path: PathBuf::from(detect::DEFAULT_CARDS_PATH),
            connect_timeout: crate::client::DEFAULT_CONNECT_TIMEOUT,
            sync_timeout: crate::client::DEFAULT_SYNC_TIMEOUT,
            readback_timeout: crate::client::DEFAULT_READBACK_TIMEOUT,
            drain: None,
            probe: true,
            json: false,
            strict: false,
        }
    }
}

impl Options {
    /// Resolve the control endpoint.
    pub fn addr(&self) -> Result<SocketAddr, CueMixError> {
        resolve_control_addr(&self.host, self.port)
    }

    fn client_config(&self, addr: SocketAddr) -> ClientConfig {
        let mut config = ClientConfig::new(addr);
        config.password = self.password.clone();
        config.connect_timeout = self.connect_timeout;
        config.sync_timeout = self.sync_timeout;
        config.readback_timeout = self.readback_timeout;
        if let Some(drain) = self.drain {
            config.snapshot_drain = drain;
        }
        config
    }

    fn detect_config(&self, addr: SocketAddr) -> DetectConfig {
        DetectConfig {
            cards_path: self.cards_path.clone(),
            control_addr: addr,
            probe_timeout: PROBE_TIMEOUT,
        }
    }
}

/// Parsed CLI command.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Devices + state diagnostics (read only).
    Info(Options),
    /// Write kMuteEnable[0].
    Mute {
        /// Desired mute state.
        on: bool,
        /// Global options.
        options: Options,
    },
    /// Write the stereo main bus fader.
    Volume {
        /// Target volume (linear or dB).
        target: VolumeTarget,
        /// Global options.
        options: Options,
    },
    /// Read-only JSON snapshot.
    Save {
        /// Output file.
        path: PathBuf,
        /// Global options.
        options: Options,
    },
    /// Print usage.
    Help,
    /// Print version.
    Version,
}

/// Volume argument from the command line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VolumeTarget {
    /// Linear amplitude 0.0..=1.0.
    Linear(f64),
    /// dB value (<= 0).
    Db(f64),
}

impl VolumeTarget {
    /// Linear volume after dB conversion / range validation.
    pub fn linear(self) -> Result<f64, CueMixError> {
        match self {
            Self::Linear(value) => fader::quantize_linear(value),
            Self::Db(value) => {
                let linear = fader::db_to_fader(value)?;
                fader::quantize_linear(linear)
            }
        }
    }

    /// Human description for the CLI header line.
    pub fn describe(self) -> String {
        match self {
            Self::Linear(value) => fader::describe_linear(value),
            Self::Db(value) => format!("{:.2} dB", value),
        }
    }
}

impl VolumeTarget {
    fn parse(text: &str) -> Result<Self, CliError> {
        let lowered = text.trim().to_ascii_lowercase();
        if let Some(db_text) = lowered.strip_suffix("db") {
            let value: f64 = db_text
                .trim()
                .parse()
                .map_err(|_| CliError::new(format!("cannot parse dB volume: {:?}", text)))?;
            if !value.is_finite() || value > 0.0 {
                return Err(CliError::new(format!(
                    "dB volume must be finite and <= 0, got {}",
                    text
                )));
            }
            fader::db_to_fader(value).map_err(CliError::from_cue_mix)?;
            return Ok(Self::Db(value));
        }
        let value: f64 = text
            .trim()
            .parse()
            .map_err(|_| CliError::new(format!("cannot parse volume: {:?}", text)))?;
        fader::quantize_linear(value).map_err(CliError::from_cue_mix)?;
        Ok(Self::Linear(value))
    }
}

/// CLI argument error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliError {
    message: String,
}

impl CliError {
    /// New usage error.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn from_cue_mix(error: CueMixError) -> Self {
        Self::new(error.to_string())
    }

    /// Human error text.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

struct FlagValue {
    name: String,
    value: String,
}

enum FlagToken<'a> {
    Flag(&'a str),
    WithValue(FlagValue),
}

fn is_negative_value(arg: &str) -> bool {
    let Some(rest) = arg.strip_prefix('-') else {
        return false;
    };
    matches!(rest.chars().next(), Some(character) if character.is_ascii_digit() || character == '.')
}

fn split_flag(arg: &str) -> Option<FlagToken<'_>> {
    let rest = arg.strip_prefix("--")?;
    if rest.is_empty() {
        return None;
    }
    match rest.split_once('=') {
        Some((name, value)) => Some(FlagToken::WithValue(FlagValue {
            name: name.to_string(),
            value: value.to_string(),
        })),
        None => Some(FlagToken::Flag(rest)),
    }
}

fn parse_seconds(text: &str, flag: &str) -> Result<Duration, CliError> {
    let seconds: f64 = text
        .trim()
        .parse()
        .map_err(|_| CliError::new(format!("--{} expects seconds, got {:?}", flag, text)))?;
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(CliError::new(format!(
            "--{} expects a positive number of seconds",
            flag
        )));
    }
    Ok(Duration::from_secs_f64(seconds))
}

/// Parse CLI arguments (without argv[0]).
pub fn parse_args(args: &[String], env_password: Option<String>) -> Result<Command, CliError> {
    let mut options = Options::default();
    let mut positionals: Vec<String> = Vec::new();
    let mut password_flag: Option<String> = None;
    let mut index = 0usize;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg == "-h" || arg == "--help" {
            return Ok(Command::Help);
        }
        if arg == "-V" || arg == "--version" {
            return Ok(Command::Version);
        }
        if let Some(token) = split_flag(arg) {
            match token {
                FlagToken::WithValue(flag) => {
                    apply_flag(&mut options, &mut password_flag, &flag.name, flag.value)?;
                }
                FlagToken::Flag(name) => match name {
                    "no-probe" => options.probe = false,
                    "json" => options.json = true,
                    "strict" => options.strict = true,
                    _ => {
                        let value = args
                            .get(index + 1)
                            .ok_or_else(|| CliError::new(format!("--{} requires a value", name)))?;
                        apply_flag(&mut options, &mut password_flag, name, value.clone())?;
                        index += 1;
                    }
                },
            }
        } else if is_negative_value(arg) {
            // Negative volumes (-0.1, -6dB) are positionals, not options.
            positionals.push(arg.to_string());
        } else if arg.starts_with('-') && arg.len() > 1 {
            return Err(CliError::new(format!("unknown option: {}", arg)));
        } else {
            positionals.push(arg.to_string());
        }
        index += 1;
    }

    if let Some(password) = password_flag {
        options.password = Some(password);
    } else if env_password.is_some() {
        options.password = env_password;
    }

    let Some(command_name) = positionals.first() else {
        return Err(CliError::new(
            "missing command (info | mute on|off | volume <0..1|dB> | save <file>)",
        ));
    };
    let extra = &positionals[1..];
    match command_name.as_str() {
        "help" => Ok(Command::Help),
        "version" => Ok(Command::Version),
        "info" => {
            if let Some(unexpected) = extra.first() {
                return Err(CliError::new(format!(
                    "info takes no argument, got {:?}",
                    unexpected
                )));
            }
            Ok(Command::Info(options))
        }
        "mute" => {
            let value = extra
                .first()
                .ok_or_else(|| CliError::new("mute requires on|off"))?;
            let on = match value.as_str() {
                "on" => true,
                "off" => false,
                other => {
                    return Err(CliError::new(format!(
                        "mute expects on|off, got {:?}",
                        other
                    )))
                }
            };
            if let Some(unexpected) = extra.get(1) {
                return Err(CliError::new(format!(
                    "mute takes one argument, got {:?}",
                    unexpected
                )));
            }
            Ok(Command::Mute { on, options })
        }
        "volume" => {
            let value = extra
                .first()
                .ok_or_else(|| CliError::new("volume requires a value (0..1 or dB, e.g. -6dB)"))?;
            let target = VolumeTarget::parse(value)?;
            if let Some(unexpected) = extra.get(1) {
                return Err(CliError::new(format!(
                    "volume takes one argument, got {:?}",
                    unexpected
                )));
            }
            Ok(Command::Volume { target, options })
        }
        "save" => {
            let value = extra
                .first()
                .ok_or_else(|| CliError::new("save requires an output file"))?;
            if let Some(unexpected) = extra.get(1) {
                return Err(CliError::new(format!(
                    "save takes one argument, got {:?}",
                    unexpected
                )));
            }
            Ok(Command::Save {
                path: PathBuf::from(value),
                options,
            })
        }
        other => Err(CliError::new(format!(
            "unknown command {:?} (info | mute | volume | save)",
            other
        ))),
    }
}

fn apply_flag(
    options: &mut Options,
    password_flag: &mut Option<String>,
    name: &str,
    value: String,
) -> Result<(), CliError> {
    match name {
        "host" => options.host = value,
        "port" => {
            options.port = value
                .parse::<u16>()
                .map_err(|_| CliError::new(format!("--port expects 1..65535, got {:?}", value)))?;
        }
        "password" => *password_flag = Some(value),
        "cards" => options.cards_path = PathBuf::from(value),
        "connect-timeout" => options.connect_timeout = parse_seconds(&value, name)?,
        "sync-timeout" => options.sync_timeout = parse_seconds(&value, name)?,
        "readback-timeout" => options.readback_timeout = parse_seconds(&value, name)?,
        "drain" => options.drain = Some(parse_seconds(&value, name)?),
        other => return Err(CliError::new(format!("unknown option: --{}", other))),
    }
    Ok(())
}

/// Usage text.
pub const HELP: &str =
    "khaen-motu - MOTU UltraLite-mk5 CueMix 5 control (single-key writes, verified readback)

USAGE:
  khaen-motu info [options]
  khaen-motu mute on|off [options]
  khaen-motu volume <0..1|dB> [options]
  khaen-motu save <file> [options]

COMMANDS:
  info     detect the device, connect read-only and print identity + state
  mute     write kMuteEnable[0] (one key), print before/wrote/readback
  volume   write koBusFader[0] then koBusFader[1] (two separate single-key
           writes, each verified), value is linear 0..1 or dB (e.g. -6dB)
  save     collect the datastore and write a read-only JSON snapshot
           (same shape as the reference client --save output)

OPTIONS:
  --host <host>              control host (default 169.254.111.126)
  --port <port>              control port (default 1280)
  --password <password>      control-network password (default: $KHAEN_CUEMIX_PASSWORD)
  --cards <path>             ALSA cards file (default /proc/asound/cards)
  --connect-timeout <secs>   connect + handshake timeout (default 2)
  --sync-timeout <secs>      initial datastore sync timeout (default 3)
  --readback-timeout <secs>  write readback deadline (default 1.5)
  --drain <secs>             snapshot collection window (default 3)
  --no-probe                 skip the control-network TCP probe before writes
  --json                     JSON output for info
  --strict                   info exits 3 when the device is not ready
  -h, --help                 print this help
  -V, --version              print the version

EXIT CODES:
  0 ok, 2 usage, 3 device absent/unreachable, 4 transport/auth/protocol, 5 readback failed
";

/// Execute one command, writing human output to out and errors to err.
pub fn execute<W: Write, E: Write>(command: Command, out: &mut W, err: &mut E) -> i32 {
    match command {
        Command::Help => {
            let _ = writeln!(out, "{}", HELP);
            EXIT_OK
        }
        Command::Version => {
            let _ = writeln!(out, "khaen-motu {}", crate::VERSION);
            EXIT_OK
        }
        Command::Info(options) => run_info(&options, out, err),
        Command::Mute { on, options } => run_mute(on, &options, out, err),
        Command::Volume { target, options } => run_volume(target, &options, out, err),
        Command::Save { path, options } => run_save(&path, &options, out, err),
    }
}

/// Read process args/env and execute; returns the process exit code.
pub fn run_from_env() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env_password = std::env::var(PASSWORD_ENV).ok();
    match parse_args(&args, env_password) {
        Ok(command) => {
            let stdout = std::io::stdout();
            let stderr = std::io::stderr();
            let mut out = stdout.lock();
            let mut err = stderr.lock();
            execute(command, &mut out, &mut err)
        }
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "khaen-motu: {}", error);
            EXIT_USAGE
        }
    }
}

fn exit_for(error: &CueMixError) -> i32 {
    match error.code() {
        codes::READBACK_TIMEOUT | codes::READBACK_MISMATCH => EXIT_READBACK,
        codes::NOT_DETECTED => EXIT_DEVICE,
        _ => EXIT_TRANSPORT,
    }
}

fn probe_control(addr: SocketAddr) -> bool {
    TcpStream::connect_timeout(&addr, PROBE_TIMEOUT).is_ok()
}

fn write_error<E: Write>(err: &mut E, context: &str, error: &CueMixError) -> i32 {
    let _ = writeln!(err, "{}: {}", context, error);
    exit_for(error)
}

fn blocked<E: Write>(code: i32, err: &mut E, message: &str) -> i32 {
    let _ = writeln!(err, "{}", message);
    code
}

fn run_info<W: Write, E: Write>(options: &Options, out: &mut W, err: &mut E) -> i32 {
    let addr = match options.addr() {
        Ok(addr) => addr,
        Err(error) => return write_error(err, "info", &error),
    };
    let detection = if options.probe {
        detect::detect(&options.detect_config(addr))
    } else {
        detect::detect_with(&options.detect_config(addr), None, &|_, _| false)
    };
    if options.json {
        let _ = writeln!(out, "{}", detection_json(&detection));
    } else {
        let _ = writeln!(out, "{}", detection_line(&detection));
    }
    if options.strict && !detection.is_ready() {
        return blocked(
            EXIT_DEVICE,
            err,
            &format!(
                "info: device not ready (detected={} control_reachable={} reason={})",
                detection.detected, detection.control_reachable, detection.reason
            ),
        );
    }
    if !detection.control_reachable {
        if !options.json {
            let _ = writeln!(
                out,
                "control: unreachable (reason={}); skipping device read",
                detection.reason
            );
        }
        return EXIT_OK;
    }
    let mut client = match CueMixClient::connect(options.client_config(addr)) {
        Ok(client) => client,
        Err(error) => return write_error(err, "info", &error),
    };
    let state = match client.read_state() {
        Ok(state) => state,
        Err(error) => return write_error(err, "info", &error),
    };
    let info = client.device_info();
    let stats = client.stats().clone();
    if options.json {
        let _ = writeln!(
            out,
            "{}",
            device_state_json(&detection, &info, &state, &stats)
        );
    } else {
        let _ = writeln!(out, "{}", device_line(&info));
        let _ = writeln!(out, "{}", state_line(&state));
        let _ = writeln!(out, "{}", stats_line(&stats, client.password_required()));
    }
    EXIT_OK
}

fn prepare_client<E: Write>(
    options: &Options,
    command: &str,
    err: &mut E,
) -> Result<CueMixClient, i32> {
    let addr = match options.addr() {
        Ok(addr) => addr,
        Err(error) => return Err(write_error(err, command, &error)),
    };
    if options.probe && !probe_control(addr) {
        return Err(blocked(
            EXIT_DEVICE,
            err,
            &format!(
                "{}: control network unreachable at {} (TCP probe failed)",
                command, addr
            ),
        ));
    }
    match CueMixClient::connect(options.client_config(addr)) {
        Ok(client) => Ok(client),
        Err(error) => Err(write_error(err, command, &error)),
    }
}

fn run_mute<W: Write, E: Write>(on: bool, options: &Options, out: &mut W, err: &mut E) -> i32 {
    let mut client = match prepare_client(options, "mute", err) {
        Ok(client) => client,
        Err(code) => return code,
    };
    let report = match client.set_mute(on) {
        Ok(report) => report,
        Err(error) => return write_error(err, "mute", &error),
    };
    let verification = if report.frame_sent {
        "verified"
    } else {
        "already at target; no write sent"
    };
    let _ = writeln!(
        out,
        "mute: before={} wrote={} readback={} [{}]",
        option_bool(report.before),
        bool_word(on),
        bool_word(report.readback),
        verification
    );
    EXIT_OK
}

fn run_volume<W: Write, E: Write>(
    target: VolumeTarget,
    options: &Options,
    out: &mut W,
    err: &mut E,
) -> i32 {
    let linear = match target.linear() {
        Ok(linear) => linear,
        Err(error) => {
            let _ = writeln!(err, "volume: {}", error);
            return EXIT_USAGE;
        }
    };
    let mut client = match prepare_client(options, "volume", err) {
        Ok(client) => client,
        Err(code) => return code,
    };
    let _ = writeln!(
        out,
        "volume: target {} ({})",
        target.describe(),
        fader::describe_linear(linear)
    );
    for channel in MainChannel::ALL {
        match client.set_volume_channel(channel, linear) {
            Ok(report) => {
                let verification = if report.frame_sent {
                    "verified"
                } else {
                    "already at target; no write sent"
                };
                let _ = writeln!(
                    out,
                    "  {}[{}] ({}): before={} wrote={} readback={} [{}]",
                    keys::KO_BUS_FADER,
                    channel.index(),
                    channel.label(),
                    option_fader(report.before),
                    fader::describe_linear(report.wrote),
                    fader::describe_linear(report.readback),
                    verification
                );
            }
            Err(error) => {
                let _ = writeln!(
                    err,
                    "  {}[{}] ({}): write failed: {}",
                    keys::KO_BUS_FADER,
                    channel.index(),
                    channel.label(),
                    error
                );
                return exit_for(&error);
            }
        }
    }
    EXIT_OK
}

fn run_save<W: Write, E: Write>(path: &Path, options: &Options, out: &mut W, err: &mut E) -> i32 {
    let mut client = match prepare_client(options, "save", err) {
        Ok(client) => client,
        Err(code) => return code,
    };
    match client.save_snapshot(path) {
        Ok(report) => {
            let _ = writeln!(
                out,
                "save: {} ({} bytes, {} datastore values, {} messages, authenticated={})",
                report.path.display(),
                report.bytes,
                report.datastore_values,
                report.messages,
                report.authenticated
            );
            EXIT_OK
        }
        Err(error) => write_error(err, "save", &error),
    }
}

fn bool_word(value: bool) -> &'static str {
    if value {
        "on"
    } else {
        "off"
    }
}

fn option_bool(value: Option<bool>) -> String {
    match value {
        Some(value) => format!("{}({})", bool_word(value), u8::from(value)),
        None => "unknown".to_string(),
    }
}

fn option_fader(value: Option<f64>) -> String {
    match value {
        Some(value) => fader::describe_linear(value),
        None => "unknown".to_string(),
    }
}

fn detection_line(detection: &DeviceDetection) -> String {
    format!(
        "detection: detected={} card_id={} card_index={} model={} control_reachable={} reason={}",
        detection.detected,
        detection.card_id.as_deref().unwrap_or("null"),
        detection
            .card_index
            .map(|index| index.to_string())
            .unwrap_or_else(|| "null".to_string()),
        detection.model.as_deref().unwrap_or("null"),
        detection.control_reachable,
        detection.reason
    )
}

fn device_line(info: &DeviceInfo) -> String {
    format!(
        "device: name={} model_id={} version={} api={} uid={}",
        info.name.as_deref().unwrap_or("null"),
        info.model_id.as_deref().unwrap_or("null"),
        info.version.as_deref().unwrap_or("null"),
        info.api_version
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_string()),
        info.uid
            .as_deref()
            .map(crate::crypto::hex)
            .unwrap_or_else(|| "null".to_string())
    )
}

fn state_line(state: &CueMixState) -> String {
    let missing = state.missing();
    let missing_note = if missing.is_empty() {
        String::new()
    } else {
        format!(" missing={} (reason: not_received)", missing.join(","))
    };
    format!(
        "state: mute={} sample_rate_hz={} main_fader={} main_trim={} ko_bus_mute={}{}",
        state
            .mute
            .map(|value| format!("{}({})", bool_word(value), u8::from(value)))
            .unwrap_or_else(|| "null".to_string()),
        state
            .sample_rate_hz
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_string()),
        state
            .main_fader
            .map(fader::describe_linear)
            .unwrap_or_else(|| "null".to_string()),
        state
            .main_trim
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_string()),
        state
            .ko_bus_mute
            .map(|value| format!("{}({})", bool_word(value), u8::from(value)))
            .unwrap_or_else(|| "null".to_string()),
        missing_note
    )
}

fn stats_line(stats: &Stats, password_required: Option<bool>) -> String {
    format!(
        "auth: authenticated={} password_required={} messages={} proxy_messages={} unknown_ids={} malformed={}",
        stats.authenticated,
        match password_required {
            Some(value) => value.to_string(),
            None => "unknown".to_string(),
        },
        stats.messages,
        stats.proxy_messages,
        stats.unknown_ids,
        stats.malformed
    )
}

fn detection_json(detection: &DeviceDetection) -> String {
    format!(
        "{{\"detection\":{{\"detected\":{},\"card_id\":{},\"card_index\":{},\"model\":{},\"control_reachable\":{},\"reason\":{}}}}}",
        detection.detected,
        json_opt_string(detection.card_id.as_deref()),
        detection
            .card_index
            .map(|index| index.to_string())
            .unwrap_or_else(|| "null".to_string()),
        json_opt_string(detection.model.as_deref()),
        detection.control_reachable,
        snapshot::json_string(detection.reason)
    )
}

fn device_state_json(
    detection: &DeviceDetection,
    info: &DeviceInfo,
    state: &CueMixState,
    stats: &Stats,
) -> String {
    let mut out = String::new();
    out.push_str("{\"detection\":{");
    out.push_str(&format!(
        "\"detected\":{},\"card_id\":{},\"card_index\":{},\"model\":{},\"control_reachable\":{},\"reason\":{}",
        detection.detected,
        json_opt_string(detection.card_id.as_deref()),
        detection
            .card_index
            .map(|index| index.to_string())
            .unwrap_or_else(|| "null".to_string()),
        json_opt_string(detection.model.as_deref()),
        detection.control_reachable,
        snapshot::json_string(detection.reason)
    ));
    out.push_str("},\"device\":{");
    out.push_str(&format!(
        "\"name\":{},\"model_id\":{},\"version\":{},\"api_version\":{},\"uid\":{}",
        json_opt_string(info.name.as_deref()),
        json_opt_string(info.model_id.as_deref()),
        json_opt_string(info.version.as_deref()),
        info.api_version
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_string()),
        info.uid
            .as_deref()
            .map(|bytes| {
                let mut text = String::from("[");
                for (index, byte) in bytes.iter().enumerate() {
                    if index > 0 {
                        text.push(',');
                    }
                    text.push_str(&byte.to_string());
                }
                text.push(']');
                text
            })
            .unwrap_or_else(|| "null".to_string())
    ));
    out.push_str("},\"state\":{");
    out.push_str(&format!(
        "\"mute\":{},\"sample_rate_hz\":{},\"main_fader\":{},\"main_trim\":{},\"ko_bus_mute\":{},\"missing\":[",
        json_opt_bool(state.mute),
        state
            .sample_rate_hz
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_string()),
        state
            .main_fader
            .map(snapshot::json_f64)
            .unwrap_or_else(|| "null".to_string()),
        state
            .main_trim
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_string()),
        json_opt_bool(state.ko_bus_mute)
    ));
    for (index, field) in state.missing().iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&snapshot::json_string(field));
    }
    out.push_str(&format!(
        "]}},\"auth\":{{\"authenticated\":{},\"messages\":{}}}}}",
        stats.authenticated, stats.messages
    ));
    out
}

fn json_opt_string(value: Option<&str>) -> String {
    match value {
        Some(text) => snapshot::json_string(text),
        None => "null".to_string(),
    }
}

fn json_opt_bool(value: Option<bool>) -> String {
    match value {
        Some(value) => value.to_string(),
        None => "null".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn parse(values: &[&str]) -> Command {
        parse_args(&args(values), None).expect("parse")
    }

    fn parse_with_env(values: &[&str], password: Option<&str>) -> Command {
        parse_args(&args(values), password.map(|value| value.to_string())).expect("parse")
    }

    #[test]
    fn parses_info_with_defaults() {
        match parse(&["info"]) {
            Command::Info(options) => {
                assert_eq!(options.host, "169.254.111.126");
                assert_eq!(options.port, 1280);
                assert_eq!(options.password, None);
                assert_eq!(options.cards_path, PathBuf::from("/proc/asound/cards"));
                assert!(options.probe);
                assert!(!options.json);
                assert!(!options.strict);
                assert_eq!(options.connect_timeout, Duration::from_secs(2));
            }
            other => panic!("expected info, got {:?}", other),
        }
    }

    #[test]
    fn parses_mute_on_and_off() {
        match parse(&["mute", "on"]) {
            Command::Mute { on, .. } => assert!(on),
            other => panic!("expected mute, got {:?}", other),
        }
        match parse(&["mute", "off"]) {
            Command::Mute { on, .. } => assert!(!on),
            other => panic!("expected mute, got {:?}", other),
        }
    }

    #[test]
    fn rejects_bad_mute_arguments() {
        assert!(parse_args(&args(&["mute"]), None).is_err());
        assert!(parse_args(&args(&["mute", "maybe"]), None).is_err());
        assert!(parse_args(&args(&["mute", "on", "extra"]), None).is_err());
    }

    #[test]
    fn parses_volume_linear_and_db() {
        match parse(&["volume", "0.5"]) {
            Command::Volume {
                target: VolumeTarget::Linear(value),
                ..
            } => assert_eq!(value, 0.5),
            other => panic!("expected linear volume, got {:?}", other),
        }
        match parse(&["volume", "-6dB"]) {
            Command::Volume {
                target: VolumeTarget::Db(value),
                ..
            } => assert!((value - -6.0).abs() < 1e-9),
            other => panic!("expected db volume, got {:?}", other),
        }
        match parse(&["volume", "-6.0206 db"]) {
            Command::Volume {
                target: VolumeTarget::Db(value),
                ..
            } => assert!((value - -6.0206).abs() < 1e-9),
            other => panic!("expected db volume, got {:?}", other),
        }
    }

    #[test]
    fn rejects_volume_out_of_range() {
        for value in ["1.5", "-0.1", "nan", "inf", "loud"] {
            assert!(
                parse_args(&args(&["volume", value]), None).is_err(),
                "volume {} must be rejected",
                value
            );
        }
        for value in ["3dB", "infdB", "nan dB"] {
            assert!(
                parse_args(&args(&["volume", value]), None).is_err(),
                "volume {} must be rejected",
                value
            );
        }
        assert!(parse_args(&args(&["volume"]), None).is_err());
    }

    #[test]
    fn volume_target_converts_db_to_linear() {
        let target = VolumeTarget::Db(-6.020_599_913_3);
        let linear = target.linear().expect("linear");
        assert!((linear - 0.5).abs() < 1e-6, "linear = {}", linear);
        assert_eq!(VolumeTarget::Linear(0.5).linear().expect("linear"), 0.5);
        assert!(VolumeTarget::Linear(2.0).linear().is_err());
    }

    #[test]
    fn parses_save_path() {
        match parse(&["save", "out.json"]) {
            Command::Save { path, .. } => assert_eq!(path, PathBuf::from("out.json")),
            other => panic!("expected save, got {:?}", other),
        }
        assert!(parse_args(&args(&["save"]), None).is_err());
    }

    #[test]
    fn parses_global_options_anywhere() {
        match parse(&[
            "--host",
            "127.0.0.1",
            "--port=1999",
            "info",
            "--json",
            "--strict",
        ]) {
            Command::Info(options) => {
                assert_eq!(options.host, "127.0.0.1");
                assert_eq!(options.port, 1999);
                assert!(options.json);
                assert!(options.strict);
            }
            other => panic!("expected info, got {:?}", other),
        }
    }

    #[test]
    fn parses_timeout_and_drain_options() {
        match parse(&[
            "info",
            "--connect-timeout=0.5",
            "--sync-timeout",
            "2",
            "--readback-timeout",
            "0.25",
            "--drain",
            "1",
            "--cards",
            "C:/tmp/cards",
        ]) {
            Command::Info(options) => {
                assert_eq!(options.connect_timeout, Duration::from_millis(500));
                assert_eq!(options.sync_timeout, Duration::from_secs(2));
                assert_eq!(options.readback_timeout, Duration::from_millis(250));
                assert_eq!(options.drain, Some(Duration::from_secs(1)));
                assert_eq!(options.cards_path, PathBuf::from("C:/tmp/cards"));
            }
            other => panic!("expected info, got {:?}", other),
        }
        assert!(parse_args(&args(&["info", "--connect-timeout", "0"]), None).is_err());
        assert!(parse_args(&args(&["info", "--sync-timeout", "-1"]), None).is_err());
        assert!(parse_args(&args(&["info", "--port", "99999"]), None).is_err());
    }

    #[test]
    fn password_flag_wins_over_environment() {
        match parse_with_env(&["mute", "on", "--password", "flag"], Some("env")) {
            Command::Mute { options, .. } => assert_eq!(options.password.as_deref(), Some("flag")),
            other => panic!("expected mute, got {:?}", other),
        }
        match parse_with_env(&["mute", "on"], Some("env")) {
            Command::Mute { options, .. } => assert_eq!(options.password.as_deref(), Some("env")),
            other => panic!("expected mute, got {:?}", other),
        }
    }

    #[test]
    fn parses_help_and_version() {
        assert_eq!(parse(&["--help"]), Command::Help);
        assert_eq!(parse(&["-h"]), Command::Help);
        assert_eq!(parse(&["-V"]), Command::Version);
        assert_eq!(parse(&["--version"]), Command::Version);
        assert_eq!(parse(&["version"]).clone(), Command::Version);
    }

    #[test]
    fn rejects_unknown_and_missing_commands() {
        assert!(parse_args(&args(&[]), None).is_err());
        assert!(parse_args(&args(&["mute"]), None).is_err());
        assert!(parse_args(&args(&["bogus"]), None).is_err());
        assert!(parse_args(&args(&["info", "--bogus"]), None).is_err());
        assert!(parse_args(&args(&["info", "-x"]), None).is_err());
        assert!(parse_args(&args(&["info", "--host"]), None).is_err());
    }

    #[test]
    fn help_text_lists_commands_and_exit_codes() {
        for needle in ["info", "mute", "volume", "save", "--host", "EXIT CODES"] {
            assert!(HELP.contains(needle), "help must mention {}", needle);
        }
    }

    #[test]
    fn execute_help_and_version_do_not_touch_network() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(execute(Command::Help, &mut out, &mut err), EXIT_OK);
        assert!(String::from_utf8_lossy(&out).contains("USAGE"));
        assert!(err.is_empty());

        let mut out = Vec::new();
        assert_eq!(execute(Command::Version, &mut out, &mut err), EXIT_OK);
        let text = String::from_utf8_lossy(&out).to_string();
        assert!(text.contains("khaen-motu"));
        assert!(text.contains(crate::VERSION));
    }

    #[test]
    fn execute_volume_out_of_range_is_usage_error() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let command = Command::Volume {
            target: VolumeTarget::Linear(1.5),
            options: Options::default(),
        };
        assert_eq!(execute(command, &mut out, &mut err), EXIT_USAGE);
        assert!(String::from_utf8_lossy(&err).contains("outside 0.0..=1.0"));
    }

    #[test]
    fn resolve_addr_option_defaults_to_device_link() {
        let options = Options::default();
        let addr = options.addr().expect("addr");
        assert_eq!(addr.port(), 1280);
        assert_eq!(options.client_config(addr).password, None);
    }

    #[test]
    fn exit_for_maps_stable_codes() {
        assert_eq!(
            exit_for(&CueMixError::ReadbackTimeout {
                key: keys::MUTE_ENABLE,
                index: 0
            }),
            EXIT_READBACK
        );
        assert_eq!(
            exit_for(&CueMixError::ReadbackMismatch {
                key: keys::MUTE_ENABLE,
                index: 0,
                expected: "1".to_string(),
                actual: "0".to_string()
            }),
            EXIT_READBACK
        );
        assert_eq!(exit_for(&CueMixError::AuthRequired), EXIT_TRANSPORT);
        assert_eq!(
            exit_for(&CueMixError::NotDetected { reason: "none" }),
            EXIT_DEVICE
        );
    }

    #[test]
    fn formats_state_and_detection_lines() {
        let detection = DeviceDetection {
            detected: true,
            card_id: Some("UltraLitemk5".to_string()),
            card_index: Some(0),
            model: Some("UltraLite-mk5".to_string()),
            control_reachable: true,
            reason: detect::REASON_DETECTED,
        };
        let line = detection_line(&detection);
        assert!(line.contains("detected=true"));
        assert!(line.contains("card_id=UltraLitemk5"));
        assert!(line.contains("model=UltraLite-mk5"));
        assert!(line.contains("control_reachable=true"));

        let state = CueMixState {
            mute: Some(false),
            sample_rate_hz: Some(48_000),
            main_fader: Some(1.0),
            main_trim: Some(31),
            ko_bus_mute: Some(false),
        };
        let line = state_line(&state);
        assert!(line.contains("mute=off(0)"));
        assert!(line.contains("sample_rate_hz=48000"));
        assert!(line.contains("main_fader=1.000000 (0.00 dB)"));
        assert!(!line.contains("missing="));

        let partial = CueMixState::default();
        assert!(state_line(&partial)
            .contains("missing=mute,sample_rate_hz,main_fader,main_trim,ko_bus_mute"));
    }
}
