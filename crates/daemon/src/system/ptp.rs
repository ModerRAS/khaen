//! PTP（linuxptp）只读状态：`/api/status` 的 `ptp` 块（加法字段）。
//!
//! 数据来源是本机 `pmc`（linuxptp 管理客户端）连 `/var/run/ptp4l` 的只读查询：
//! `TIME_STATUS_NP` / `PARENT_DATA_SET` / `CURRENT_DATA_SET` /
//! `TIME_PROPERTIES_DATA_SET` / `DEFAULT_DATA_SET`。**不调时钟、不改 ptp4l、不写设备**；
//! 探测失败/未安装/未运行一律 `available:false` + 原因，不编造锁状态或数值。
//!
//! daemon 启动时用 [`start_background_probe`] 拉起后台探针线程，每 [`PROBE_INTERVAL`]
//! 刷新一次缓存；`/api/status` 只读缓存，不阻塞 HTTP。音频/ASRC 路径不读这个状态。
//!
//! 平台：Linux 上真实探测；其它平台如实 `available:false`（原因写明只在 Linux 可用）。
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

/// `/api/status.ptp` 的 schema。
pub const SCHEMA: &str = "khaen-ptp/v1";
/// 后台探针刷新间隔。
const PROBE_INTERVAL: Duration = Duration::from_secs(2);
/// pmc 子进程超时：ptp4l 未运行/无响应时不让探针线程卡死。
const PMC_TIMEOUT: Duration = Duration::from_millis(800);

/// 缓存未产生快照时的原因（Linux 启动后几秒内）。
#[cfg(target_os = "linux")]
const NOT_PROBED_REASON: &str = "the PTP probe has not produced a snapshot yet: the daemon starts the background linuxptp (pmc) probe at boot, the first result appears within a few seconds";
/// 非 Linux 的如实原因。
#[cfg(not(target_os = "linux"))]
const NOT_PROBED_REASON: &str =
    "PTP status is only available on Linux (linuxptp pmc reads /var/run/ptp4l)";

/// 一次成功探测的原始数据集（字段缺失为 `None`，解析不补默认值）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PtpProbe {
    /// TIME_STATUS_NP.gmPresent
    pub gm_present: Option<bool>,
    /// TIME_STATUS_NP.gmIdentity（回退 PARENT_DATA_SET.grandmasterIdentity）
    pub gm_identity: Option<String>,
    /// DEFAULT_DATA_SET.domainNumber
    pub domain: Option<u64>,
    /// PARENT_DATA_SET.gm.ClockClass
    pub clock_class: Option<u64>,
    /// CURRENT_DATA_SET.offsetFromMaster（ns）
    pub offset_ns: Option<f64>,
    /// CURRENT_DATA_SET.meanPathDelay（ns）
    pub mean_path_delay_ns: Option<f64>,
    /// CURRENT_DATA_SET.stepsRemoved
    pub steps_removed: Option<u64>,
    /// TIME_PROPERTIES_DATA_SET.timeSource
    pub time_source: Option<String>,
    /// TIME_PROPERTIES_DATA_SET.currentUtcOffset
    pub current_utc_offset: Option<i64>,
    /// ptp4l 时间戳模式（software/hardware；来自 argv 或 -f 配置，拿不到为 None）
    pub mode: Option<String>,
    /// ptp4l 绑定网卡（-i）
    pub interface: Option<String>,
}

#[derive(Clone, Debug)]
struct Cached {
    at: Instant,
    value: Value,
    reason: Option<String>,
}

static CACHE: OnceLock<Mutex<Option<Cached>>> = OnceLock::new();

/// 启动后台探针线程（daemon main 调一次；非 Linux no-op）。
pub fn start_background_probe() {
    #[cfg(target_os = "linux")]
    {
        let _ = std::thread::Builder::new()
            .name("khaen-ptp-probe".to_string())
            .spawn(|| loop {
                let (value, reason) = probe_value();
                store(value, reason);
                std::thread::sleep(PROBE_INTERVAL);
            });
    }
}

#[cfg(target_os = "linux")]
fn store(value: Value, reason: Option<String>) {
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = cache.lock() {
        *guard = Some(Cached {
            at: Instant::now(),
            value,
            reason,
        });
    }
}

/// `/api/status.ptp` 的当前快照 +（有则）原因。
///
/// 只读缓存；探针线程没跑/还没结果时返回全 null + 原因（不现场 spawn pmc，不阻塞 HTTP）。
pub fn status_value() -> (Value, Option<String>) {
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    match cache.lock() {
        Ok(guard) => match guard.as_ref() {
            Some(cached) => {
                let age_ms = u64::try_from(cached.at.elapsed().as_millis()).unwrap_or(u64::MAX);
                (with_age(&cached.value, age_ms), cached.reason.clone())
            }
            None => (unavailable_value(), Some(NOT_PROBED_REASON.to_string())),
        },
        Err(_) => (
            unavailable_value(),
            Some("the PTP status cache is poisoned".to_string()),
        ),
    }
}

#[cfg(target_os = "linux")]
fn probe_value() -> (Value, Option<String>) {
    match probe_once() {
        Ok(probe) => {
            let reason = if probe.gm_present == Some(true) {
                None
            } else {
                Some(
                    "ptp4l is running but gmPresent is not true: the receiver is not locked to a grandmaster"
                        .to_string(),
                )
            };
            (available_value(&probe), reason)
        }
        Err(reason) => (unavailable_value(), Some(reason)),
    }
}

fn available_value(probe: &PtpProbe) -> Value {
    let locked = probe.gm_present == Some(true) && probe.offset_ns.is_some();
    let mut map = Map::new();
    map.insert("schema".to_string(), json!(SCHEMA));
    map.insert("available".to_string(), json!(true));
    map.insert("source".to_string(), json!("linuxptp-pmc"));
    map.insert("locked".to_string(), json!(locked));
    map.insert("mode".to_string(), json!(probe.mode));
    map.insert("interface".to_string(), json!(probe.interface));
    map.insert("gm_present".to_string(), json!(probe.gm_present));
    map.insert("gm_identity".to_string(), json!(probe.gm_identity));
    map.insert("domain".to_string(), json!(probe.domain));
    map.insert("clock_class".to_string(), json!(probe.clock_class));
    map.insert("offset_ns".to_string(), json!(probe.offset_ns));
    map.insert(
        "mean_path_delay_ns".to_string(),
        json!(probe.mean_path_delay_ns),
    );
    map.insert("steps_removed".to_string(), json!(probe.steps_removed));
    map.insert("time_source".to_string(), json!(probe.time_source));
    map.insert(
        "current_utc_offset".to_string(),
        json!(probe.current_utc_offset),
    );
    map.insert("age_ms".to_string(), json!(0));
    Value::Object(map)
}

fn unavailable_value() -> Value {
    json!({
        "schema": SCHEMA,
        "available": false,
        "source": Value::Null,
        "locked": Value::Null,
        "mode": Value::Null,
        "interface": Value::Null,
        "gm_present": Value::Null,
        "gm_identity": Value::Null,
        "domain": Value::Null,
        "clock_class": Value::Null,
        "offset_ns": Value::Null,
        "mean_path_delay_ns": Value::Null,
        "steps_removed": Value::Null,
        "time_source": Value::Null,
        "current_utc_offset": Value::Null,
        "age_ms": Value::Null,
    })
}

fn with_age(value: &Value, age_ms: u64) -> Value {
    match value {
        Value::Object(map) => {
            let mut map = map.clone();
            map.insert("age_ms".to_string(), json!(age_ms));
            Value::Object(map)
        }
        other => other.clone(),
    }
}

// ===========================================================================
// Linux 真实探测（pmc + /proc）
// ===========================================================================

#[cfg(target_os = "linux")]
fn probe_once() -> Result<PtpProbe, String> {
    let output = run_pmc()?;
    let mut probe = parse_pmc_output(&output)?;
    if let Some(info) = probe_ptp4l_process() {
        probe.mode = info.mode;
        probe.interface = info.interface;
    }
    Ok(probe)
}

/// 解析 pmc 的 management 响应（section + key value 行）。
fn parse_pmc_output(text: &str) -> Result<PtpProbe, String> {
    let mut probe = PtpProbe::default();
    let mut section = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_suffix(':') {
            if name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
            {
                section = name.to_string();
                continue;
            }
        }
        let mut parts = line.splitn(2, |c: char| c.is_whitespace());
        let Some(key) = parts.next() else {
            continue;
        };
        let value = parts.next().unwrap_or("").trim();
        if value.is_empty() {
            continue;
        }
        match (section.as_str(), key) {
            ("TIME_STATUS_NP", "gmPresent") => probe.gm_present = parse_bool(value),
            ("TIME_STATUS_NP", "gmIdentity") => probe.gm_identity = Some(value.to_string()),
            ("PARENT_DATA_SET", "grandmasterIdentity") => {
                if probe.gm_identity.is_none() {
                    probe.gm_identity = Some(value.to_string());
                }
            }
            ("PARENT_DATA_SET", "gm.ClockClass") => probe.clock_class = value.parse().ok(),
            ("CURRENT_DATA_SET", "offsetFromMaster") => probe.offset_ns = value.parse().ok(),
            ("CURRENT_DATA_SET", "meanPathDelay") => probe.mean_path_delay_ns = value.parse().ok(),
            ("CURRENT_DATA_SET", "stepsRemoved") => probe.steps_removed = value.parse().ok(),
            ("TIME_PROPERTIES_DATA_SET", "timeSource") => {
                probe.time_source = Some(value.to_string())
            }
            ("TIME_PROPERTIES_DATA_SET", "currentUtcOffset") => {
                probe.current_utc_offset = value.parse().ok()
            }
            ("DEFAULT_DATA_SET", "domainNumber") => probe.domain = value.parse().ok(),
            _ => {}
        }
    }
    if probe.gm_present.is_none() && probe.offset_ns.is_none() && probe.gm_identity.is_none() {
        return Err(
            "pmc output did not contain PTP datasets (unexpected format, or pmc is not connected to ptp4l)"
                .to_string(),
        );
    }
    Ok(probe)
}

fn parse_bool(value: &str) -> Option<bool> {
    if value.eq_ignore_ascii_case("true") || value == "1" {
        Some(true)
    } else if value.eq_ignore_ascii_case("false") || value == "0" {
        Some(false)
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
fn run_pmc() -> Result<String, String> {
    use std::process::{Command, Stdio};

    let pmc = find_pmc();
    let mut child = Command::new(&pmc)
        .args([
            "-u",
            "-b",
            "0",
            "GET TIME_STATUS_NP",
            "GET PARENT_DATA_SET",
            "GET CURRENT_DATA_SET",
            "GET TIME_PROPERTIES_DATA_SET",
            "GET DEFAULT_DATA_SET",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("cannot run {pmc}: {err}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if started.elapsed() >= PMC_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "{pmc} did not answer within {} ms (is linuxptp ptp4l running on /var/run/ptp4l?)",
                        PMC_TIMEOUT.as_millis()
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(err) => return Err(format!("{pmc} wait failed: {err}")),
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|err| format!("{pmc} output failed: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        let hint = if detail.is_empty() {
            " (is linuxptp ptp4l running?)"
        } else {
            ""
        };
        return Err(format!(
            "{pmc} exited with {}: {detail}{hint}",
            output.status
        ));
    }
    let text =
        String::from_utf8(output.stdout).map_err(|_| format!("{pmc} output is not valid UTF-8"))?;
    if text.trim().is_empty() {
        return Err(format!(
            "{pmc} returned no data (is linuxptp ptp4l running /var/run/ptp4l?)"
        ));
    }
    Ok(text)
}

#[cfg(target_os = "linux")]
fn find_pmc() -> String {
    for candidate in ["/usr/sbin/pmc", "/usr/bin/pmc", "/sbin/pmc", "/bin/pmc"] {
        if std::path::Path::new(candidate).is_file() {
            return candidate.to_string();
        }
    }
    "pmc".to_string()
}

#[derive(Clone, Debug, Default)]
struct Ptp4lInfo {
    mode: Option<String>,
    interface: Option<String>,
}

/// 从 /proc 找 ptp4l 的 argv：-i 网卡、-S/--software、-H/--hardware，或 -f 配置里的
/// time_stamping。只用于展示，拿不到就返回 None（不影响锁状态本身）。
#[cfg(target_os = "linux")]
fn probe_ptp4l_process() -> Option<Ptp4lInfo> {
    let entries = std::fs::read_dir("/proc").ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Ok(comm) = std::fs::read_to_string(entry.path().join("comm")) else {
            continue;
        };
        if comm.trim() != "ptp4l" {
            continue;
        }
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let args: Vec<String> = cmdline
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect();
        let mut info = Ptp4lInfo::default();
        let mut config: Option<String> = None;
        let mut index = 1;
        while index < args.len() {
            match args[index].as_str() {
                "-i" | "--interface" => {
                    if let Some(value) = args.get(index + 1) {
                        info.interface = Some(value.clone());
                        index += 1;
                    }
                }
                "-S" | "--software" => info.mode = Some("software".to_string()),
                "-H" | "--hardware" => info.mode = Some("hardware".to_string()),
                "-f" | "--config" => {
                    if let Some(value) = args.get(index + 1) {
                        config = Some(value.clone());
                        index += 1;
                    }
                }
                _ => {}
            }
            index += 1;
        }
        if info.mode.is_none() {
            if let Some(path) = config {
                info.mode = mode_from_config(&path);
            }
        }
        return Some(info);
    }
    None
}

#[cfg(target_os = "linux")]
fn mode_from_config(path: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(2, |c: char| c.is_whitespace());
        if parts.next() == Some("time_stamping") {
            let value = parts.next().unwrap_or("").trim();
            if value.eq_ignore_ascii_case("software") {
                return Some("software".to_string());
            }
            if value.eq_ignore_ascii_case("hardware") {
                return Some("hardware".to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const PMC_SAMPLE: &str = r#"PARENT_DATA_SET:
	parentPortIdentity                    ae6f0d.fffe.8e86fc-1
	grandmasterPriority1                  0
	gm.ClockClass                         248
	gm.ClockAccuracy                      0xfe
	gm.OffsetScaledLogVariance            0xffff
	grandmasterPriority2                  0
	grandmasterIdentity                   ae6f0d.fffe.8e86fc
CURRENT_DATA_SET:
	stepsRemoved     1
	offsetFromMaster 14546.0
	meanPathDelay    36268.0
TIME_PROPERTIES_DATA_SET:
	currentUtcOffset      37
	currentUtcOffsetValid 0
	ptpTimescale          1
	timeTraceable         0
	frequencyTraceable    0
	timeSource            0xa0
DEFAULT_DATA_SET:
	domainNumber 0
TIME_STATUS_NP:
	master_offset              14546
	gmPresent                  true
	gmIdentity                 ae6f0d.fffe.8e86fc
"#;

    #[test]
    fn parses_the_linuxptp_probe_output() {
        let probe = parse_pmc_output(PMC_SAMPLE).expect("sample must parse");
        assert_eq!(probe.gm_present, Some(true));
        assert_eq!(probe.gm_identity.as_deref(), Some("ae6f0d.fffe.8e86fc"));
        assert_eq!(probe.domain, Some(0));
        assert_eq!(probe.clock_class, Some(248));
        assert_eq!(probe.offset_ns, Some(14546.0));
        assert_eq!(probe.mean_path_delay_ns, Some(36268.0));
        assert_eq!(probe.steps_removed, Some(1));
        assert_eq!(probe.time_source.as_deref(), Some("0xa0"));
        assert_eq!(probe.current_utc_offset, Some(37));
    }

    #[test]
    fn rejects_output_without_ptp_datasets() {
        assert!(parse_pmc_output("").is_err());
        assert!(parse_pmc_output("sending: GET TIME_STATUS_NP\nerror: no response").is_err());
    }

    #[test]
    fn available_value_reports_lock_and_nulls() {
        let probe = PtpProbe {
            gm_present: Some(true),
            gm_identity: Some("gm-1".to_string()),
            offset_ns: Some(14546.0),
            mode: Some("software".to_string()),
            interface: Some("enp2s0".to_string()),
            ..PtpProbe::default()
        };
        let value = available_value(&probe);
        assert_eq!(value["schema"], json!(SCHEMA));
        assert_eq!(value["available"], json!(true));
        assert_eq!(value["locked"], json!(true));
        assert_eq!(value["mode"], json!("software"));
        assert_eq!(value["interface"], json!("enp2s0"));
        assert_eq!(value["offset_ns"], json!(14546.0));
        assert_eq!(value["mean_path_delay_ns"], Value::Null);

        let not_locked = PtpProbe {
            gm_present: Some(false),
            offset_ns: Some(1.0),
            ..PtpProbe::default()
        };
        assert_eq!(available_value(&not_locked)["locked"], json!(false));
    }

    #[test]
    fn unavailable_value_has_only_nulls() {
        let value = unavailable_value();
        assert_eq!(value["schema"], json!(SCHEMA));
        assert_eq!(value["available"], json!(false));
        for key in [
            "source",
            "locked",
            "mode",
            "interface",
            "gm_present",
            "gm_identity",
            "domain",
            "clock_class",
            "offset_ns",
            "mean_path_delay_ns",
            "steps_removed",
            "time_source",
            "current_utc_offset",
            "age_ms",
        ] {
            assert_eq!(value[key], Value::Null, "{key} must be null");
        }
    }

    #[test]
    fn with_age_updates_age_only() {
        let value = with_age(
            &json!({"schema": SCHEMA, "available": true, "age_ms": 0}),
            1234,
        );
        assert_eq!(value["age_ms"], json!(1234));
        assert_eq!(value["available"], json!(true));
        assert_eq!(value["schema"], json!(SCHEMA));
    }

    #[test]
    fn parse_bool_accepts_ptp_spellings() {
        assert_eq!(parse_bool("true"), Some(true));
        assert_eq!(parse_bool("FALSE"), Some(false));
        assert_eq!(parse_bool("1"), Some(true));
        assert_eq!(parse_bool("0"), Some(false));
        assert_eq!(parse_bool("yes"), None);
    }
}
