//! 系统信息采集（T46）：主机名 / 内核 / 发行版 / 声卡 / systemd 服务 / journalctl 日志 / 重启。
//!
//! 设计：trait SystemProbe 是平台无关的采集接口，NativeProbe 是默认实现（读 /proc 与
//! /etc/os-release、spawn systemctl/journalctl）。所有失败都返回原因字符串，由 handler 变成
//! null + reasons；本模块从不 panic、不编造数值。
//!
//! 可测性：NativeProbe 的路径根（proc / etc）与命令执行器（CommandRunner）都可注入，因此
//! 解析逻辑、命令映射、超时降级都能在 Windows 本机用 fake /proc 目录 + 假命令执行器覆盖；
//! 真实 systemctl/journalctl 的输出语义属 NOT_RUN（CI / N150 覆盖）。

use std::collections::BTreeSet;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// daemon 的 systemd unit 名（部署 release 的 unit 文件同名）。
pub const DAEMON_UNIT: &str = "khaen-daemon";
/// rt-host 的 systemd unit 名。
pub const RT_HOST_UNIT: &str = "khaen-rt-host";
/// journalctl 单次调用超时（3 s，见任务 T46）。
pub const LOG_TIMEOUT: Duration = Duration::from_secs(3);
/// systemctl 单个查询超时（本机 systemd 正常毫秒级；3 s 足够）。
pub const SERVICE_TIMEOUT: Duration = Duration::from_secs(3);
/// systemctl restart 超时（重启 unit 允许更久）。
pub const RESTART_TIMEOUT: Duration = Duration::from_secs(10);
/// 日志行数上限（/api/system/logs 的 N 上限）。
pub const MAX_LOG_LINES: usize = 500;

/// 一条服务状态：三个字段各自可缺失（缺失原因在 reasons 里，字段名如 active/enabled/pid）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceSnapshot {
    /// systemctl is-active 的结果。
    pub active: Option<bool>,
    /// systemctl is-enabled 的结果。
    pub enabled: Option<bool>,
    /// systemctl show -p MainPID 的结果（0 视为未运行）。
    pub pid: Option<u32>,
    /// 字段名 -> 采集失败原因。
    pub reasons: Vec<(&'static str, String)>,
}

impl ServiceSnapshot {
    /// 某个字段缺失的原因。
    pub fn reason(&self, field: &str) -> Option<&str> {
        self.reasons
            .iter()
            .find(|(name, _)| *name == field)
            .map(|(_, reason)| reason.as_str())
    }

    /// 服务级原因（多个字段失败时合并；全部成功为 None）。
    pub fn summary_reason(&self) -> Option<String> {
        if self.reasons.is_empty() {
            return None;
        }
        let parts: Vec<String> = self
            .reasons
            .iter()
            .map(|(field, reason)| format!("{field}: {reason}"))
            .collect();
        Some(parts.join("; "))
    }
}

/// 一张声卡（/proc/asound/cards 的一条 + 可选的 stream 描述符补充）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceInfo {
    /// 声卡 id（/proc/asound/cards 方括号里的名字，如 UltraLitemk5）。
    pub card_id: Option<String>,
    /// 驱动名（cards 行里 "]: " 之后 " - " 之前，如 USB-Audio）。
    pub driver: Option<String>,
    /// 型号（cards 行第二行的描述，如 MOTU UltraLite mk5）。
    pub model: Option<String>,
    /// 通道数（来自 stream 描述符的 Channels:；ALSA 未暴露时为 None）。
    pub channels: Option<u32>,
    /// 支持的采样率（来自 stream 描述符的 Rates:；未暴露时为 None）。
    pub rates: Option<Vec<u32>>,
    /// 字段名（如 channels / rates）-> 缺失原因；只用于 info.reasons 诊断。
    pub reasons: Vec<(String, String)>,
}

/// 平台无关的系统采集接口（daemon 启动时注入；测试用假实现）。
pub trait SystemProbe: fmt::Debug + Send + Sync {
    /// 主机名（/proc/sys/kernel/hostname）。
    fn hostname(&self) -> Result<String, String>;
    /// 内核版本（/proc/sys/kernel/osrelease，退回 /proc/version 第一行）。
    fn kernel(&self) -> Result<String, String>;
    /// 发行版（/etc/os-release 的 PRETTY_NAME）。
    fn os(&self) -> Result<String, String>;
    /// 声卡列表（/proc/asound/cards）。
    fn devices(&self) -> Result<Vec<DeviceInfo>, String>;
    /// 单个 systemd unit 的 active/enabled/pid（失败字段进 reasons，不整体失败）。
    fn service(&self, unit: &str) -> ServiceSnapshot;
    /// journalctl -u unit -n lines --no-pager（失败返回原因，由 handler 映射 503）。
    fn logs(&self, unit: &str, lines: usize) -> Result<Vec<String>, String>;
    /// systemctl restart unit（失败返回原因）。
    fn restart_service(&self, unit: &str) -> Result<(), String>;
}

/// 采集路径根（生产 /proc 与 /etc；测试注入临时目录）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPaths {
    /// /proc 的根。
    pub proc_root: PathBuf,
    /// /etc 的根。
    pub etc_root: PathBuf,
}

impl Default for HostPaths {
    fn default() -> Self {
        Self {
            proc_root: PathBuf::from("/proc"),
            etc_root: PathBuf::from("/etc"),
        }
    }
}

impl HostPaths {
    /// 用指定根构造（测试）。
    pub fn new(proc_root: impl Into<PathBuf>, etc_root: impl Into<PathBuf>) -> Self {
        Self {
            proc_root: proc_root.into(),
            etc_root: etc_root.into(),
        }
    }
}

/// 一条外部命令的执行结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    /// 退出码（被信号终止时为 -1）。
    pub status: i32,
    /// stdout 原文。
    pub stdout: String,
    /// stderr 原文。
    pub stderr: String,
}

impl CommandOutput {
    /// 退出码是否为 0。
    pub fn success(&self) -> bool {
        self.status == 0
    }

    /// 失败时给人看的原因（优先 stderr，退回 stdout，再退回退出码）。
    pub fn failure_reason(&self) -> String {
        let detail = if !self.stderr.trim().is_empty() {
            self.stderr.trim()
        } else if !self.stdout.trim().is_empty() {
            self.stdout.trim()
        } else {
            "no output"
        };
        format!("exited with status {} ({detail})", self.status)
    }
}

/// 命令执行器（spawn + 超时 + 捕获输出）；测试注入假实现。
pub trait CommandRunner: fmt::Debug + Send + Sync {
    /// 运行 argv（含程序名），超时后 kill 并返回错误。
    fn run(&self, argv: &[String], timeout: Duration) -> Result<CommandOutput, String>;
}

/// 生产命令执行器：std::process::Command + 截止时间轮询，超时 kill（std only，无新依赖）。
#[derive(Debug, Clone, Copy, Default)]
pub struct StdCommandRunner;

impl CommandRunner for StdCommandRunner {
    fn run(&self, argv: &[String], timeout: Duration) -> Result<CommandOutput, String> {
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| "empty command".to_string())?;
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("cannot spawn {program}: {error}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("cannot capture stdout of {program}"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| format!("cannot capture stderr of {program}"))?;
        // 读线程防止子进程因管道写满阻塞（journalctl 可能输出数百行）。
        let stdout_reader = std::thread::spawn(move || read_all(stdout));
        let stderr_reader = std::thread::spawn(move || read_all(stderr));
        let command_line = argv.join(" ");
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = stdout_reader.join();
                        let _ = stderr_reader.join();
                        return Err(format!(
                            "{command_line} timed out after {} ms",
                            timeout.as_millis()
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Err(format!("cannot poll {command_line}: {error}"));
                }
            }
        };
        let stdout = stdout_reader.join().unwrap_or_default();
        let stderr = stderr_reader.join().unwrap_or_default();
        Ok(CommandOutput {
            status: status.code().unwrap_or(-1),
            stdout,
            stderr,
        })
    }
}

fn read_all(mut reader: impl Read) -> String {
    let mut text = String::new();
    let _ = reader.read_to_string(&mut text);
    text
}

/// 默认采集实现：/proc + /etc/os-release + systemctl/journalctl（Linux 语义；其它平台如实
/// 返回“不可用”原因，不 panic、不编译期绑定平台）。
#[derive(Debug, Clone)]
pub struct NativeProbe {
    paths: HostPaths,
    runner: Arc<dyn CommandRunner>,
}

impl Default for NativeProbe {
    fn default() -> Self {
        Self {
            paths: HostPaths::default(),
            runner: Arc::new(StdCommandRunner),
        }
    }
}

impl NativeProbe {
    /// 用指定路径根与命令执行器构造（测试注入 fake /proc 与假 runner）。
    pub fn new(paths: HostPaths, runner: Arc<dyn CommandRunner>) -> Self {
        Self { paths, runner }
    }

    /// 采集路径根。
    pub fn paths(&self) -> &HostPaths {
        &self.paths
    }

    /// 读 systemctl 的一条查询（失败带 unit/命令原因）。
    fn systemctl(&self, args: &[&str], timeout: Duration) -> Result<CommandOutput, String> {
        let mut argv = Vec::with_capacity(args.len() + 1);
        argv.push("systemctl".to_string());
        for arg in args {
            argv.push((*arg).to_string());
        }
        self.runner
            .run(&argv, timeout)
            .map_err(|error| format!("systemctl {} is unavailable: {error}", args.join(" ")))
    }

    /// 单张声卡的 stream 描述符汇总（channels 取各 Altset 最大值，rates 取并集排序）。
    fn stream_summary(&self, card_dir: &Path) -> Result<(Option<u32>, Option<Vec<u32>>), String> {
        let entries = std::fs::read_dir(card_dir)
            .map_err(|error| format!("{} is not readable: {error}", card_dir.display()))?;
        let mut channels: Option<u32> = None;
        let mut rates: BTreeSet<u32> = BTreeSet::new();
        let mut found = false;
        for entry in entries {
            let entry = entry.map_err(|error| format!("{}: {error}", card_dir.display()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("stream") {
                continue;
            }
            found = true;
            let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
            let (stream_channels, stream_rates) = parse_stream_info(&text);
            if let Some(value) = stream_channels {
                channels = Some(channels.map_or(value, |current: u32| current.max(value)));
            }
            if let Some(values) = stream_rates {
                rates.extend(values);
            }
        }
        if !found {
            return Err(format!(
                "{} has no stream* descriptor: ALSA exposes channels/rates here only for USB audio descriptors",
                card_dir.display()
            ));
        }
        let rates = if rates.is_empty() {
            None
        } else {
            Some(rates.into_iter().collect())
        };
        Ok((channels, rates))
    }
}

impl SystemProbe for NativeProbe {
    fn hostname(&self) -> Result<String, String> {
        let path = self.paths.proc_root.join("sys/kernel/hostname");
        read_trimmed(&path).map_err(|error| format!("{} is not readable: {error}", path.display()))
    }

    fn kernel(&self) -> Result<String, String> {
        let osrelease = self.paths.proc_root.join("sys/kernel/osrelease");
        if let Ok(value) = read_trimmed(&osrelease) {
            return Ok(value);
        }
        let version = self.paths.proc_root.join("version");
        let text = std::fs::read_to_string(&version)
            .map_err(|error| format!("{} is not readable: {error}", version.display()))?;
        let line = text.lines().next().unwrap_or_default().trim();
        if line.is_empty() {
            return Err(format!("{} is empty", version.display()));
        }
        // /proc/version: "Linux version 6.8.0-45-generic (buildd@...) ..." -> 版本号 token。
        Ok(line.split_whitespace().nth(2).unwrap_or(line).to_string())
    }

    fn os(&self) -> Result<String, String> {
        let path = self.paths.etc_root.join("os-release");
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("{} is not readable: {error}", path.display()))?;
        parse_os_release(&text)
            .ok_or_else(|| format!("{} has no PRETTY_NAME/NAME entry", path.display()))
    }

    fn devices(&self) -> Result<Vec<DeviceInfo>, String> {
        let cards_path = self.paths.proc_root.join("asound/cards");
        let text = std::fs::read_to_string(&cards_path)
            .map_err(|error| format!("{} is not readable: {error}", cards_path.display()))?;
        let cards = parse_cards(&text);
        if cards.is_empty() {
            return Err(format!("{} lists no sound cards", cards_path.display()));
        }
        let mut devices = Vec::with_capacity(cards.len());
        for card in cards {
            let mut device = DeviceInfo {
                card_id: Some(card.card_id),
                driver: Some(card.driver),
                model: Some(card.model),
                ..DeviceInfo::default()
            };
            let card_dir = self
                .paths
                .proc_root
                .join(format!("asound/card{}", card.index));
            match self.stream_summary(&card_dir) {
                Ok((channels, rates)) => {
                    device.channels = channels;
                    device.rates = rates;
                    if device.channels.is_none() {
                        device.reasons.push((
                            "channels".to_string(),
                            format!(
                                "{} stream descriptors do not report Channels",
                                card_dir.display()
                            ),
                        ));
                    }
                    if device.rates.is_none() {
                        device.reasons.push((
                            "rates".to_string(),
                            format!(
                                "{} stream descriptors do not report Rates",
                                card_dir.display()
                            ),
                        ));
                    }
                }
                Err(reason) => {
                    device
                        .reasons
                        .push(("channels".to_string(), reason.clone()));
                    device.reasons.push(("rates".to_string(), reason));
                }
            }
            devices.push(device);
        }
        Ok(devices)
    }

    fn service(&self, unit: &str) -> ServiceSnapshot {
        let mut snapshot = ServiceSnapshot::default();
        match self.systemctl(&["is-active", unit], SERVICE_TIMEOUT) {
            Ok(output) => match parse_active(&output) {
                Ok(active) => snapshot.active = Some(active),
                Err(reason) => snapshot.reasons.push(("active", reason)),
            },
            Err(reason) => snapshot
                .reasons
                .push(("active", format!("{reason} (unit {unit})"))),
        }
        match self.systemctl(&["is-enabled", unit], SERVICE_TIMEOUT) {
            Ok(output) => match parse_enabled(&output) {
                Ok(enabled) => snapshot.enabled = Some(enabled),
                Err(reason) => snapshot.reasons.push(("enabled", reason)),
            },
            Err(reason) => snapshot
                .reasons
                .push(("enabled", format!("{reason} (unit {unit})"))),
        }
        match self.systemctl(&["show", "-p", "MainPID", "--value", unit], SERVICE_TIMEOUT) {
            Ok(output) => match parse_main_pid(&output) {
                Ok(pid) => snapshot.pid = Some(pid),
                Err(reason) => snapshot.reasons.push(("pid", reason)),
            },
            Err(reason) => snapshot
                .reasons
                .push(("pid", format!("{reason} (unit {unit})"))),
        }
        snapshot
    }

    fn logs(&self, unit: &str, lines: usize) -> Result<Vec<String>, String> {
        if lines == 0 || lines > MAX_LOG_LINES {
            return Err(format!("lines must be 1..={MAX_LOG_LINES}, got {lines}"));
        }
        let argv: Vec<String> = vec![
            "journalctl".to_string(),
            "-u".to_string(),
            unit.to_string(),
            "-n".to_string(),
            lines.to_string(),
            "--no-pager".to_string(),
        ];
        let output = self
            .runner
            .run(&argv, LOG_TIMEOUT)
            .map_err(|error| format!("journalctl -u {unit} is unavailable: {error}"))?;
        if !output.success() {
            return Err(format!("journalctl -u {unit} {}", output.failure_reason()));
        }
        Ok(output.stdout.lines().map(ToString::to_string).collect())
    }

    fn restart_service(&self, unit: &str) -> Result<(), String> {
        let output = self.systemctl(&["restart", unit], RESTART_TIMEOUT)?.clone();
        if output.success() {
            Ok(())
        } else {
            Err(format!(
                "systemctl restart {unit} {}",
                output.failure_reason()
            ))
        }
    }
}

fn read_trimmed(path: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("file is empty".to_string());
    }
    Ok(trimmed.to_string())
}

/// /etc/os-release 解析：PRETTY_NAME 优先；否则 NAME + VERSION；引号去掉。
pub(crate) fn parse_os_release(text: &str) -> Option<String> {
    let mut pretty: Option<String> = None;
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        if value.is_empty() {
            continue;
        }
        match key.trim() {
            "PRETTY_NAME" => pretty = Some(value),
            "NAME" => name = Some(value),
            "VERSION" => version = Some(value),
            _ => {}
        }
    }
    if pretty.is_some() {
        return pretty;
    }
    match (name, version) {
        (Some(name), Some(version)) => Some(format!("{name} {version}")),
        (Some(name), None) => Some(name),
        _ => None,
    }
}

/// /proc/asound/cards 的一条。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CardEntry {
    pub index: u32,
    pub card_id: String,
    pub driver: String,
    pub model: String,
}

/// /proc/asound/cards 解析（两行一组：方括号里的 card id + 第二行 "厂商 型号 at 总线"）。
pub(crate) fn parse_cards(text: &str) -> Vec<CardEntry> {
    let mut cards: Vec<CardEntry> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        let Some(open) = trimmed.find('[') else {
            // 续行：属于上一张卡的描述（型号）。
            if let Some(last) = cards.last_mut() {
                let description = trimmed.trim();
                if !description.is_empty() {
                    let model = description
                        .split_once(" at ")
                        .map(|(model, _)| model)
                        .unwrap_or(description)
                        .trim();
                    if !model.is_empty() {
                        last.model = model.to_string();
                    }
                }
            }
            continue;
        };
        let Some(close) = trimmed[open..].find(']') else {
            continue;
        };
        let card_id = trimmed[open + 1..open + close].trim();
        if card_id.is_empty() {
            continue;
        }
        let Ok(index) = trimmed[..open].trim().parse::<u32>() else {
            continue;
        };
        let rest = trimmed[open + close + 1..].trim_start();
        let rest = rest.strip_prefix(':').unwrap_or(rest).trim_start();
        let (driver, model) = match rest.split_once(" - ") {
            Some((driver, model)) => (driver.trim(), model.trim()),
            None => (rest.trim(), ""),
        };
        cards.push(CardEntry {
            index,
            card_id: card_id.to_string(),
            driver: driver.to_string(),
            model: model.to_string(),
        });
    }
    cards
}

/// USB audio stream 描述符解析：Channels 取最大值，Rates 取并集。
pub(crate) fn parse_stream_info(text: &str) -> (Option<u32>, Option<Vec<u32>>) {
    let mut channels: Option<u32> = None;
    let mut rates: BTreeSet<u32> = BTreeSet::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Channels:") {
            if let Ok(value) = rest.trim().parse::<u32>() {
                channels = Some(channels.map_or(value, |current: u32| current.max(value)));
            }
        } else if let Some(rest) = trimmed.strip_prefix("Rates:") {
            for part in rest.split(',') {
                if let Ok(value) = part.trim().parse::<u32>() {
                    rates.insert(value);
                }
            }
        }
    }
    let rates = if rates.is_empty() {
        None
    } else {
        Some(rates.into_iter().collect())
    };
    (channels, rates)
}

/// systemctl is-active 输出 -> bool（只认明确状态值；过渡态/未知返回原因）。
pub(crate) fn parse_active(output: &CommandOutput) -> Result<bool, String> {
    match output.stdout.trim() {
        "active" => Ok(true),
        "inactive" | "failed" => Ok(false),
        other => Err(format!(
            "systemctl is-active reported {other:?} ({}), which is not a settled active/inactive state",
            output.failure_reason()
        )),
    }
}

/// systemctl is-enabled 输出 -> bool（没有 enablement 概念的状态如 static 返回原因）。
pub(crate) fn parse_enabled(output: &CommandOutput) -> Result<bool, String> {
    match output.stdout.trim() {
        "enabled" | "enabled-runtime" => Ok(true),
        "disabled" | "masked" | "masked-runtime" => Ok(false),
        other => Err(format!(
            "unit has no enablement state: systemctl is-enabled reported {other:?}"
        )),
    }
}

/// systemctl show -p MainPID --value 输出 -> pid（0 表示未运行，返回原因）。
pub(crate) fn parse_main_pid(output: &CommandOutput) -> Result<u32, String> {
    let text = output.stdout.trim();
    let text = text.strip_prefix("MainPID=").unwrap_or(text).trim();
    let pid: u32 = text.parse().map_err(|_| {
        format!(
            "systemctl show MainPID reported {text:?}, which is not a pid ({})",
            output.failure_reason()
        )
    })?;
    if pid == 0 {
        return Err("unit is not running: systemctl reports MainPID=0".to_string());
    }
    Ok(pid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// 假命令执行器：按完整命令行查预设结果；未命中的命令如实报“没有安装”。
    #[derive(Debug, Default)]
    struct FakeRunner {
        replies: Mutex<HashMap<String, Result<CommandOutput, String>>>,
        calls: Mutex<Vec<String>>,
    }

    impl FakeRunner {
        fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }

        fn reply(&self, command: &str, output: CommandOutput) {
            self.replies
                .lock()
                .expect("replies")
                .insert(command.to_string(), Ok(output));
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("calls").clone()
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, argv: &[String], _timeout: Duration) -> Result<CommandOutput, String> {
            let command = argv.join(" ");
            self.calls.lock().expect("calls").push(command.clone());
            match self.replies.lock().expect("replies").get(&command) {
                Some(reply) => reply.clone(),
                None => Err(format!("cannot spawn {}: not found in this test", argv[0])),
            }
        }
    }

    fn output(status: i32, stdout: &str) -> CommandOutput {
        CommandOutput {
            status,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    fn fake_proc(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("khaen-probe-{}-{tag}-{unique}", std::process::id()));
        std::fs::create_dir_all(&root).expect("temp proc root");
        root
    }

    #[test]
    fn parses_cards_and_stream_descriptors() {
        let cards = " 0 [UltraLitemk5   ]: USB-Audio - UltraLite mk5\n                      MOTU UltraLite mk5 at usb-0000:00:14.0-2.1, high speed\n 1 [PCH            ]: HDA-Intel - HDA Intel PCH\n                      HDA Intel PCH at 0xf7080000 irq 87\n";
        let parsed = parse_cards(cards);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].index, 0);
        assert_eq!(parsed[0].card_id, "UltraLitemk5");
        assert_eq!(parsed[0].driver, "USB-Audio");
        assert_eq!(parsed[0].model, "MOTU UltraLite mk5");
        assert_eq!(parsed[1].index, 1);
        assert_eq!(parsed[1].card_id, "PCH");
        assert_eq!(parsed[1].model, "HDA Intel PCH");

        let stream = "Format: S24_3LE\n  Rates: 44100, 48000, 88200, 96000\n  Channels: 22\n";
        assert_eq!(
            parse_stream_info(stream),
            (Some(22), Some(vec![44100, 48000, 88200, 96000]))
        );
        // 多 Altset：channels 取最大，rates 取并集。
        let multi = "Channels: 2\nRates: 44100, 48000\nChannels: 22\nRates: 48000, 96000\n";
        assert_eq!(
            parse_stream_info(multi),
            (Some(22), Some(vec![44100, 48000, 96000]))
        );
    }

    #[test]
    fn parses_os_release_with_pretty_name_or_name_plus_version() {
        assert_eq!(
            parse_os_release("PRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\nNAME=\"Debian\"\n"),
            Some("Debian GNU/Linux 12 (bookworm)".to_string())
        );
        assert_eq!(
            parse_os_release("NAME=Debian\nVERSION=12\n"),
            Some("Debian 12".to_string())
        );
        assert_eq!(parse_os_release("ID=khaen\n"), None);
    }

    #[test]
    fn service_states_map_to_booleans_and_reasons() {
        assert_eq!(parse_active(&output(0, "active\n")), Ok(true));
        assert_eq!(parse_active(&output(3, "inactive\n")), Ok(false));
        assert_eq!(parse_active(&output(1, "failed\n")), Ok(false));
        assert!(parse_active(&output(0, "activating\n")).is_err());

        assert_eq!(parse_enabled(&output(0, "enabled\n")), Ok(true));
        assert_eq!(parse_enabled(&output(1, "disabled\n")), Ok(false));
        assert!(parse_enabled(&output(0, "static\n")).is_err());

        assert_eq!(parse_main_pid(&output(0, "4211\n")), Ok(4211));
        assert_eq!(parse_main_pid(&output(0, "MainPID=4211\n")), Ok(4211));
        assert!(parse_main_pid(&output(0, "0\n")).is_err());
        assert!(parse_main_pid(&output(0, "nonsense\n")).is_err());
    }

    #[test]
    fn native_probe_reads_a_fake_proc_tree_and_degrades_per_field() {
        let root = fake_proc("tree");
        let proc_root = root.join("proc");
        let etc_root = root.join("etc");
        std::fs::create_dir_all(proc_root.join("sys/kernel")).expect("proc sys");
        std::fs::create_dir_all(proc_root.join("asound/card0")).expect("asound");
        std::fs::create_dir_all(&etc_root).expect("etc");
        std::fs::write(proc_root.join("sys/kernel/hostname"), "AoipDSP\n").expect("hostname");
        std::fs::write(proc_root.join("sys/kernel/osrelease"), "6.8.0-45-generic\n")
            .expect("osrelease");
        std::fs::write(
            etc_root.join("os-release"),
            "PRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\n",
        )
        .expect("os-release");
        std::fs::write(
            proc_root.join("asound/cards"),
            " 0 [UltraLitemk5   ]: USB-Audio - UltraLite mk5\n                      MOTU UltraLite mk5 at usb-1, high speed\n",
        )
        .expect("cards");
        std::fs::write(
            proc_root.join("asound/card0/stream0"),
            "Format: S24_3LE\n  Rates: 44100, 48000\n  Channels: 22\n",
        )
        .expect("stream0");

        let probe = NativeProbe::new(
            HostPaths::new(&proc_root, &etc_root),
            Arc::new(FakeRunner::default()),
        );
        assert_eq!(probe.hostname(), Ok("AoipDSP".to_string()));
        assert_eq!(probe.kernel(), Ok("6.8.0-45-generic".to_string()));
        assert_eq!(probe.os(), Ok("Debian GNU/Linux 12 (bookworm)".to_string()));
        let devices = probe.devices().expect("devices");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].card_id.as_deref(), Some("UltraLitemk5"));
        assert_eq!(devices[0].driver.as_deref(), Some("USB-Audio"));
        assert_eq!(devices[0].model.as_deref(), Some("MOTU UltraLite mk5"));
        assert_eq!(devices[0].channels, Some(22));
        assert_eq!(devices[0].rates, Some(vec![44100, 48000]));

        // 缺文件时逐字段真实降级（不 panic、不编造）。
        std::fs::remove_file(proc_root.join("sys/kernel/hostname")).expect("remove hostname");
        assert!(probe.hostname().is_err());
        std::fs::remove_dir_all(&etc_root).expect("remove etc");
        assert!(probe.os().is_err());
        // 无 stream 描述符的卡：channels/rates 为 None + 设备级原因。
        std::fs::remove_file(proc_root.join("asound/card0/stream0")).expect("remove stream0");
        let devices = probe.devices().expect("devices without stream");
        assert_eq!(devices[0].channels, None);
        assert_eq!(devices[0].rates, None);
        assert!(devices[0]
            .reasons
            .iter()
            .any(|(field, _)| field == "channels"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn service_probe_maps_systemctl_outputs_and_reports_missing_systemctl() {
        let runner = FakeRunner::new();
        runner.reply("systemctl is-active khaen-daemon", output(0, "active\n"));
        runner.reply("systemctl is-enabled khaen-daemon", output(0, "enabled\n"));
        runner.reply(
            "systemctl show -p MainPID --value khaen-daemon",
            output(0, "4211\n"),
        );
        let probe = NativeProbe::new(HostPaths::default(), runner.clone());
        let snapshot = probe.service("khaen-daemon");
        assert_eq!(snapshot.active, Some(true));
        assert_eq!(snapshot.enabled, Some(true));
        assert_eq!(snapshot.pid, Some(4211));
        assert!(snapshot.reason("active").is_none());
        assert_eq!(runner.calls().len(), 3);

        // systemd 不存在的机器：三个字段都 None + 原因（Windows 本机就是这个形态）。
        let missing = NativeProbe::new(HostPaths::default(), FakeRunner::new());
        let snapshot = missing.service("khaen-rt-host");
        assert_eq!(snapshot.active, None);
        assert_eq!(snapshot.enabled, None);
        assert_eq!(snapshot.pid, None);
        assert!(snapshot.summary_reason().is_some());
    }

    #[test]
    fn logs_call_journalctl_with_the_contract_argv_and_reports_failures() {
        let runner = FakeRunner::new();
        runner.reply(
            "journalctl -u khaen-daemon -n 200 --no-pager",
            output(0, "line one\nline two\n"),
        );
        let probe = NativeProbe::new(HostPaths::default(), runner.clone());
        let lines = probe.logs("khaen-daemon", 200).expect("logs");
        assert_eq!(lines, vec!["line one".to_string(), "line two".to_string()]);

        let failing = FakeRunner::new();
        failing.reply("journalctl -u khaen-daemon -n 10 --no-pager", output(1, ""));
        let probe = NativeProbe::new(HostPaths::default(), failing);
        let error = probe.logs("khaen-daemon", 10).expect_err("failed logs");
        assert!(error.contains("journalctl"), "{error}");

        // 参数越界不调用命令。
        let probe = NativeProbe::new(HostPaths::default(), FakeRunner::new());
        assert!(probe.logs("khaen-daemon", 0).is_err());
        assert!(probe.logs("khaen-daemon", MAX_LOG_LINES + 1).is_err());
    }

    #[test]
    fn restart_service_uses_systemctl_restart_and_reports_failures() {
        let runner = FakeRunner::new();
        runner.reply("systemctl restart khaen-rt-host", output(0, ""));
        let probe = NativeProbe::new(HostPaths::default(), runner);
        assert_eq!(probe.restart_service("khaen-rt-host"), Ok(()));

        let failing = FakeRunner::new();
        let mut reply = output(1, "");
        reply.stderr = "Job for khaen-rt-host.service failed".to_string();
        failing.reply("systemctl restart khaen-rt-host", reply);
        let probe = NativeProbe::new(HostPaths::default(), failing);
        let error = probe
            .restart_service("khaen-rt-host")
            .expect_err("restart failure");
        assert!(error.contains("failed"), "{error}");
    }

    #[test]
    fn command_runner_reports_spawn_failures() {
        let runner = StdCommandRunner;
        // 跨平台都会很快失败的 argv（程序不存在）：错误文本必须含程序名，不能 panic。
        let argv = vec!["definitely-not-a-real-binary-khaen".to_string()];
        let error = runner
            .run(&argv, Duration::from_millis(200))
            .expect_err("missing binary");
        assert!(
            error.contains("definitely-not-a-real-binary-khaen"),
            "{error}"
        );
    }

    #[test]
    fn command_runner_kills_the_child_after_the_deadline() {
        let runner = StdCommandRunner;
        // 跨平台的长命令：Windows 用 ping 计数，Unix 用 sleep（都在 5 s 左右），超时 150 ms。
        // 用"自己就是长命令"的直连程序（Windows 的 ping / Unix 的 sleep），不经过 shell：
        // 否则 kill 直接子进程后孙进程仍可能继承 stdout 管道，读取线程要等它自己结束。
        #[cfg(windows)]
        let argv: Vec<String> = vec![
            "ping".to_string(),
            "-n".to_string(),
            "6".to_string(),
            "127.0.0.1".to_string(),
        ];
        #[cfg(not(windows))]
        let argv: Vec<String> = vec!["sleep".to_string(), "5".to_string()];
        let started = Instant::now();
        let error = runner
            .run(&argv, Duration::from_millis(150))
            .expect_err("long command must time out");
        assert!(error.contains("timed out"), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "超时后必须及时 kill 子进程，而不是等它自己结束"
        );
    }
}
