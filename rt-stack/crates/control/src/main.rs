//! khaen-rt-daemon —— KHAEN 控制面/校准 daemon 入口（WBS T70/T42/T44/T60 的部署接线，T155）。
//!
//! 角色（与 service/khaen-rt-daemon.service 契约一致）：REST/WS/Web 的**唯一**控制面进程；
//! **不直接打开 PCM**（ALSA/设备 owner 只在 khaen-rt-host）。本文件只做进程接线：
//! 读 config -> 建 RestService（T42）+ WebServer（T70，含 WS/REST 桥）-> 监听端口 ->
//! 优雅退出（SIGINT/SIGTERM 或 --exit-after-ms/T155 证据用）。
//!
//! 已知边界（NOT_WIRED/NOT_TESTED，禁止当已验证）：
//! - 模式切换已接 T44 `ModeController`（`ModeEngine` 适配器）：epoch/servo/回滚/在途 409
//!   等状态机语义真实执行；但服务面仍是 `SimulatedServices`（无 systemd/设备），所以
//!   `/api/mode` 响应显式 `mode_services="simulated"` + `mode_actual=false`（F-28）。
//! - 内层 engine 仍是 control 层 MemoryEngine 测试替身：CommitOutcome 的
//!   blocks_committed/ack_mask 是内存合成的，`engine_info().synthetic=true` 会原样出现在
//!   commit/status 回执里，页面显示 synthetic 而不是「已生效（actual）」（F-13）。
//!   真实 host engine 经 T26 注入属 follow-up；T70 evidence E-1/E-3 已登记；
//! - 校准 job 已挂：默认 KHAEN_CAL_JOB_BACKEND=synthetic 构造 T60 JobManager 的
//!   CalibrationPort 适配（calibration_job_port.rs）——job 状态机/互斥/进度/apply/rollback
//!   走真实 T60/T66 代码，但测量=stage fake、设计=synthetic_offline、apply=T66 离线合成
//!   ack，所以 REST/页面显式 engine=t60-jobmanager-synthetic + synthetic=true（F-13 口径，
//!   不冒充板端 actual）。KHAEN_CAL_JOB_BACKEND=real 把请求转到 host 的 Unix socket
//!   （socket 不在时该次请求 503，不造测量记录）；=off 显式禁用（REST 503）；
//! - 真实浏览器/板端/80 端口部署、真实校准 job/REST 板端闭环 NOT_RUN。
//!
//! 用法：khaen-rt-daemon serve --config <rt-daemon.toml> [--exit-after-ms N] [--bind ADDR] [--check]

use std::env;
use std::fs;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use khaen_control::modes::{ModeEngine, SimulatedServices};
use khaen_control::rest::calibration_job_port::JobManagerPort;
use khaen_control::rest::{CalibrationPort, MemoryEngine, RestService};
use khaen_control::web::{WebConfig, WebServer};

const DEFAULT_LISTEN: &str = "127.0.0.1:8090";
const DEFAULT_MAX_CONNECTIONS: usize = 64;
const DEFAULT_IO_TIMEOUT_SECS: u64 = 10;

static STOP: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" {
    fn signal(signum: i32, handler: usize) -> usize;
}

#[cfg(unix)]
extern "C" fn on_signal(_signum: i32) {
    STOP.store(true, Ordering::SeqCst);
}

/// daemon 配置（rt-daemon.toml；DRAFT -> T155 冻结为 khaen-rt-daemon/v1）。
#[derive(Clone, Debug)]
struct DaemonConfig {
    /// 监听地址（host:port；生产 0.0.0.0:80，本机开发 127.0.0.1:8090）。
    listen: String,
    /// 状态目录（只读记录；本进程不写）。
    state_dir: String,
    /// 鉴权文件路径（REST auth 状态；当前由 RestService 默认策略接管，仅记录）。
    auth_file: String,
    /// 校准 job 目录（T60 follow-up；仅记录）。
    job_dir: String,
    /// host 控制 URL（仅记录，供页面/诊断展示）。
    control_url: String,
    /// 并发连接上限。
    max_connections: usize,
    /// I/O 超时秒。
    io_timeout_secs: u64,
    /// 校准 stage 后端（toml `[calibration] stage`；None = 未配置，按 env/默认 synthetic）。
    ///
    /// 取值与 `KHAEN_CAL_JOB_BACKEND`/`KHAEN_CAL_STAGE` 同口径（synthetic/cli/off/real）；
    /// `cli` = 真实 MOTU/UMIK 音频（本机 calib-stage.sh），apply 为 file-backend
    /// NOT_APPLIED_TO_AUDIO。
    cal_stage: Option<String>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            listen: DEFAULT_LISTEN.to_string(),
            state_dir: "/var/lib/khaen-rt-stack".to_string(),
            auth_file: "/etc/khaen-rt-stack/daemon-auth.json".to_string(),
            job_dir: "/var/lib/khaen-rt-stack/calibration".to_string(),
            control_url: "http://127.0.0.1:8090".to_string(),
            max_connections: DEFAULT_MAX_CONNECTIONS,
            io_timeout_secs: DEFAULT_IO_TIMEOUT_SECS,
            cal_stage: None,
        }
    }
}

fn trim_comment(line: &str) -> &str {
    match line.find('#') {
        Some(index) => &line[..index],
        None => line,
    }
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1].to_string()
    } else if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

/// 解析 TOML-lite（section + key = value；未知键忽略并计数）。
fn parse_config(text: &str, warnings: &mut Vec<String>) -> Result<DaemonConfig, String> {
    let mut config = DaemonConfig::default();
    let mut section = String::new();
    for raw in text.lines() {
        let line = trim_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_ascii_lowercase();
            continue;
        }
        let (key, value) = match line.split_once('=') {
            Some(pair) => pair,
            None => return Err(format!("invalid config line: {line}")),
        };
        let key = key.trim().to_string();
        let value = unquote(value);
        match (section.as_str(), key.as_str()) {
            ("", "listen") | ("daemon", "listen") | ("web", "listen") => config.listen = value,
            ("paths", "state_dir") | ("daemon", "state_dir") => config.state_dir = value,
            ("daemon", "auth_file") | ("daemon", "auth") => config.auth_file = value,
            ("daemon", "job_dir") => config.job_dir = value,
            ("control", "url") | ("daemon", "control_url") => config.control_url = value,
            // `[calibration] stage = "cli"` → 真实 CLI stage 后端（默认仍是 synthetic）。
            ("calibration", "stage") | ("calibration", "backend") => config.cal_stage = Some(value),
            ("web", "max_connections") | ("daemon", "max_connections") => {
                config.max_connections = value
                    .parse()
                    .map_err(|_| format!("bad max_connections: {value}"))?
            }
            ("web", "io_timeout_secs") | ("daemon", "io_timeout_secs") => {
                config.io_timeout_secs = value
                    .parse()
                    .map_err(|_| format!("bad io_timeout_secs: {value}"))?
            }
            _ => warnings.push(format!("unknown config key [{section}] {key}")),
        }
    }
    Ok(config)
}

fn load_config(path: &str) -> Result<(DaemonConfig, Vec<String>), String> {
    let text = fs::read_to_string(path).map_err(|err| format!("read {path}: {err}"))?;
    let text = text.trim_start_matches('﻿');
    let mut warnings = Vec::new();
    let mut config = parse_config(text, &mut warnings)?;
    // 环境变量覆盖（systemd EnvironmentFile 契约；T155 冻结）。
    if let Ok(value) = env::var("KHAEN_DAEMON_LISTEN") {
        config.listen = value;
    }
    if let Ok(value) = env::var("KHAEN_DAEMON_STATE_DIR") {
        config.state_dir = value;
    }
    if let Ok(value) = env::var("KHAEN_DAEMON_AUTH_FILE") {
        config.auth_file = value;
    }
    if let Ok(value) = env::var("KHAEN_DAEMON_JOB_DIR") {
        config.job_dir = value;
    }
    if let Ok(value) = env::var("KHAEN_RT_HOST_CONTROL_URL") {
        config.control_url = value;
    }
    // `KHAEN_CAL_STAGE=cli` 最高优先（systemd EnvironmentFile 契约）；显式空值视为未配置。
    if let Ok(value) = env::var("KHAEN_CAL_STAGE") {
        if !value.trim().is_empty() {
            config.cal_stage = Some(value);
        }
    }
    Ok((config, warnings))
}

fn json_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn usage() -> &'static str {
    concat!(
        "khaen-rt-daemon (T155 control-plane entry)\n",
        "usage:\n",
        "  khaen-rt-daemon serve --config <rt-daemon.toml> [--exit-after-ms N] [--bind ADDR]\n",
        "  khaen-rt-daemon check --config <rt-daemon.toml>\n",
        "calibration backend:\n",
        "  default synthetic  = real T60 state machine, Fake measurement/offline apply ack\n",
        "  KHAEN_CAL_STAGE=cli = real MOTU/UMIK audio via /usr/local/lib/khaen/calib-stage.sh;\n",
        "                       apply is file-backend active.yml = NOT_APPLIED_TO_AUDIO\n",
        "                       (host does not consume calibration.yml yet)\n",
        "  KHAEN_CAL_JOB_BACKEND=off|real = REST calibration endpoints 503 (not faked)\n",
        "NOT_TESTED: real browser/board/80-port deployment; engine=synthetic(memory), ",
        "mode_services=simulated (T26/T44 follow-up)\n"
    )
}

fn install_signals() {
    #[cfg(unix)]
    unsafe {
        // SIGINT=2 / SIGTERM=15 -> STOP（仅 Unix；Windows 用 --exit-after-ms 停）。
        let _ = signal(2, on_signal as usize);
        let _ = signal(15, on_signal as usize);
    }
}

// ---------------------------------------------------------------------------
// 校准 job 接线（T60 JobManager -> T42 CalibrationPort；task-226）
// ---------------------------------------------------------------------------

/// 校准 backend 模式（env KHAEN_CAL_JOB_BACKEND；默认 synthetic）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CalBackendMode {
    /// 离线合成（默认）：真实 T60 状态机 + Fake 测量/synthetic_offline 设计/离线 apply ack。
    Synthetic,
    /// CLI stage 后端：真实 MOTU/UMIK 测量（本机 calib-stage.sh）；apply=file-backend
    /// active.yml，host 尚未消费 calibration.yml = NOT_APPLIED_TO_AUDIO（不是 actual）。
    Cli,
    /// 显式禁用：不挂 port，REST 校准端点 503。
    Off,
    /// 真实设备测量/板端 apply：NOT_WIRED；不挂 port（REST 503），不伪造 actual。
    Real,
}

impl CalBackendMode {
    /// 机器名（ready 行/证据）。
    fn as_str(self) -> &'static str {
        match self {
            CalBackendMode::Synthetic => "synthetic",
            CalBackendMode::Cli => "cli",
            CalBackendMode::Off => "off",
            CalBackendMode::Real => "real",
        }
    }

    /// 解析（未知值按 synthetic 处理并记 warning；不静默启用 real）。
    fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" | "disabled" | "none" => CalBackendMode::Off,
            "real" | "device" | "board" => CalBackendMode::Real,
            "cli" | "cli-stage" | "cli_stage" | "stage" => CalBackendMode::Cli,
            _ => CalBackendMode::Synthetic,
        }
    }

    /// 解析模式：toml `[calibration] stage`（已含 env `KHAEN_CAL_STAGE` 覆盖）优先，
    /// 其次 env `KHAEN_CAL_JOB_BACKEND`，缺省 synthetic。
    fn resolve(config_stage: Option<&str>) -> Self {
        if let Some(value) = config_stage {
            return CalBackendMode::parse(value);
        }
        match env::var("KHAEN_CAL_JOB_BACKEND") {
            Ok(value) => CalBackendMode::parse(&value),
            Err(_) => CalBackendMode::Synthetic,
        }
    }
}

/// daemon 启动时的校准接线自述（ready 行/证据用；不含内部路径细节）。
struct CalibrationReport {
    mode: &'static str,
    wired: bool,
    engine: &'static str,
    synthetic: bool,
    real_audio: bool,
    backend: &'static str,
    resumed_job: Option<String>,
    reason: Option<String>,
}

impl CalibrationReport {
    fn disabled(mode: &'static str, reason: impl Into<String>) -> Self {
        CalibrationReport {
            mode,
            wired: false,
            engine: "not_wired",
            synthetic: true,
            real_audio: false,
            backend: "not_wired",
            resumed_job: None,
            reason: Some(reason.into()),
        }
    }

    fn to_json(&self) -> String {
        let resumed = match &self.resumed_job {
            Some(job) => format!("\"{}\"", json_escape(job)),
            None => "null".to_string(),
        };
        let reason = match &self.reason {
            Some(reason) => format!("\"{}\"", json_escape(reason)),
            None => "null".to_string(),
        };
        format!(
            "{{\"mode\":\"{}\",\"wired\":{},\"engine\":\"{}\",\"synthetic\":{},\"real_audio\":{},\"backend\":\"{}\",\"resumed_job\":{},\"reason\":{}}}",
            self.mode,
            self.wired,
            self.engine,
            self.synthetic,
            self.real_audio,
            json_escape(self.backend),
            resumed,
            reason
        )
    }
}

/// 构造校准 port（严格离线；失败/禁用时返回 None + 自述，daemon 仍可启动但 REST 校准 503）。
fn build_calibration_with_mode(
    job_dir: &str,
    mode: CalBackendMode,
) -> (Option<Box<dyn CalibrationPort + Send>>, CalibrationReport) {
    match mode {
        CalBackendMode::Off => (
            None,
            CalibrationReport::disabled(
                mode.as_str(),
                "calibration disabled by KHAEN_CAL_JOB_BACKEND=off; REST calibration endpoints return 503",
            ),
        ),
        CalBackendMode::Real => (
            Some(Box::new(khaen_control::host_socket::HostSocketPort::new(
                khaen_rt_host::cal_control::socket_path(),
            ))),
            CalibrationReport {
                mode: mode.as_str(),
                wired: true,
                engine: "host-socket",
                synthetic: true,
                real_audio: false,
                backend: "unix-socket",
                resumed_job: None,
                reason: Some(
                    "emperor requests go to khaen-rt-host; a missing socket fails the request; synthetic stays true until a recorded measurement exists"
                        .to_string(),
                ),
            },
        ),
        CalBackendMode::Cli => {
            let root = PathBuf::from(job_dir);
            let checkpoint = Some(root.join("state").join("calibration-job.checkpoint"));
            match JobManagerPort::cli_stage(&root, checkpoint) {
                Ok(mut port) => {
                    let resumed_job = match port.resume_pending() {
                        Ok(Some(view)) => Some(view.job_id),
                        Ok(None) => None,
                        Err(error) => {
                            eprintln!("calibration checkpoint resume warning: {error}");
                            None
                        }
                    };
                    let info = port.offline_engine_info();
                    (
                        Some(Box::new(port)),
                        CalibrationReport {
                            mode: mode.as_str(),
                            wired: true,
                            engine: info.engine,
                            synthetic: info.synthetic,
                            real_audio: info.real_audio,
                            backend: info.backend,
                            resumed_job,
                            reason: None,
                        },
                    )
                }
                Err(error) => (
                    None,
                    CalibrationReport::disabled(
                        mode.as_str(),
                        format!(
                            "cli stage job manager init failed: {error}; REST calibration endpoints return 503"
                        ),
                    ),
                ),
            }
        }
        CalBackendMode::Synthetic => {
            let root = PathBuf::from(job_dir);
            let checkpoint = Some(root.join("state").join("calibration-job.checkpoint"));
            match JobManagerPort::synthetic_offline(&root, checkpoint) {
                Ok(mut port) => {
                    let resumed_job = match port.resume_pending() {
                        Ok(Some(view)) => Some(view.job_id),
                        Ok(None) => None,
                        Err(error) => {
                            eprintln!("calibration checkpoint resume warning: {error}");
                            None
                        }
                    };
                    let info = port.offline_engine_info();
                    (
                        Some(Box::new(port)),
                        CalibrationReport {
                            mode: mode.as_str(),
                            wired: true,
                            engine: info.engine,
                            synthetic: info.synthetic,
                            real_audio: info.real_audio,
                            backend: info.backend,
                            resumed_job,
                            reason: None,
                        },
                    )
                }
                Err(error) => (
                    None,
                    CalibrationReport::disabled(
                        mode.as_str(),
                        format!(
                            "job manager init failed: {error}; REST calibration endpoints return 503"
                        ),
                    ),
                ),
            }
        }
    }
}

/// 按 toml `[calibration] stage` / env（KHAEN_CAL_STAGE / KHAEN_CAL_JOB_BACKEND）构造校准 port。
fn build_calibration(
    job_dir: &str,
    cal_stage: Option<&str>,
) -> (Option<Box<dyn CalibrationPort + Send>>, CalibrationReport) {
    build_calibration_with_mode(job_dir, CalBackendMode::resolve(cal_stage))
}

fn run_serve(
    config_path: &str,
    exit_after_ms: Option<u64>,
    bind_override: Option<&str>,
) -> Result<i32, String> {
    let (config, warnings) = load_config(config_path)?;
    let bind_addr = bind_override.unwrap_or(&config.listen).to_string();
    if bind_addr.is_empty() {
        return Err("empty listen address".to_string());
    }
    let web_config = WebConfig {
        bind_addr: bind_addr.clone(),
        max_connections: config.max_connections,
        io_timeout_secs: config.io_timeout_secs,
        ..WebConfig::default()
    };
    // F-28：mode 切换走 T44 ModeController（真实的互斥/epoch/servo/回滚状态机），
    // 不再直接改 MemoryEngine snapshot。服务面仍为模拟实现（T26 换 systemd/clock）。
    let engine = ModeEngine::new(
        Box::new(MemoryEngine::new()),
        Box::new(SimulatedServices::new()),
    )
    .map_err(|error| {
        format!(
            "mode controller init failed: kind={} detail={}",
            error.kind.as_str(),
            error.detail
        )
    })?;
    // task-226：挂真实 T60 JobManager 的 CalibrationPort（离线合成 backend；synthetic 显式
    // 标注）。real/off 模式不挂 port -> REST 校准端点 503，不伪造 job/actual。
    let (calibration_port, calibration) =
        build_calibration(&config.job_dir, config.cal_stage.as_deref());
    let mut rest = RestService::new(Box::new(engine));
    if let Some(port) = calibration_port {
        rest = rest.with_calibration(port);
    }
    let server = Arc::new(
        WebServer::bind_with(web_config, rest).map_err(|err| format!("bind {bind_addr}: {err}"))?,
    );
    let local_addr = server
        .local_addr()
        .map_err(|err| format!("local_addr: {err}"))?
        .to_string();
    println!(
        "{{\"status\":\"ready\",\"mode\":\"khaen-rt-daemon\",\"listen\":\"{}\",\"state_dir\":\"{}\",\"job_dir\":\"{}\",\"control_url\":\"{}\",\"engine\":\"memory\",\"synthetic\":true,\"mode_controller\":\"T44\",\"mode_services\":\"simulated\",\"calibration\":{},\"warnings\":[{}]}}",
        json_escape(&local_addr),
        json_escape(&config.state_dir),
        json_escape(&config.job_dir),
        json_escape(&config.control_url),
        calibration.to_json(),
        warnings.iter().map(|w| format!("\"{}\"", json_escape(w))).collect::<Vec<_>>().join(",")
    );
    install_signals();
    STOP.store(false, Ordering::SeqCst);
    let started = Instant::now();
    let stop_flag = &STOP;
    let worker = {
        let server = Arc::clone(&server);
        thread::spawn(move || server.run_until(stop_flag))
    };
    let deadline = exit_after_ms.map(|ms| Instant::now() + Duration::from_millis(ms));
    loop {
        if STOP.load(Ordering::SeqCst) {
            break;
        }
        if let Some(deadline) = deadline {
            if Instant::now() >= deadline {
                break;
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    STOP.store(true, Ordering::SeqCst);
    // task-245：accept 线程事件驱动、阻塞在 accept(2)；停止后必须显式唤醒它（自连接），
    // 否则 join 会一直等下去（旧实现是非阻塞轮询，不需要这一步）。
    server.wake();
    let result = worker
        .join()
        .map_err(|_| "web thread panicked".to_string())?;
    if let Err(err) = result {
        return Err(format!("web run: {err}"));
    }
    let uptime_ms = started.elapsed().as_millis() as u64;
    println!(
        "{{\"status\":\"ok\",\"mode\":\"khaen-rt-daemon\",\"listen\":\"{}\",\"uptime_ms\":{},\"graceful\":true}}",
        json_escape(&local_addr),
        uptime_ms
    );
    Ok(0)
}

fn probe_free_port(bind: &str) -> Result<(), String> {
    // check 模式：仅验证地址语法/端口可用（不长期监听）。
    let listener = TcpListener::bind(bind).map_err(|err| format!("bind {bind}: {err}"))?;
    drop(listener);
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let command = args.get(1).map(String::as_str).unwrap_or("serve");
    let mut config_path: Option<String> = None;
    let mut exit_after_ms: Option<u64> = None;
    let mut bind_override: Option<String> = None;
    let mut index = 2usize;
    while index < args.len() {
        match args[index].as_str() {
            "--config" | "-c" => match args.get(index + 1) {
                Some(value) => {
                    config_path = Some(value.clone());
                    index += 2;
                }
                None => {
                    eprintln!("missing --config value");
                    return ExitCode::from(2);
                }
            },
            "--exit-after-ms" => match args.get(index + 1).and_then(|v| v.parse().ok()) {
                Some(value) => {
                    exit_after_ms = Some(value);
                    index += 2;
                }
                None => {
                    eprintln!("bad --exit-after-ms");
                    return ExitCode::from(2);
                }
            },
            "--bind" => match args.get(index + 1) {
                Some(value) => {
                    bind_override = Some(value.clone());
                    index += 2;
                }
                None => {
                    eprintln!("missing --bind value");
                    return ExitCode::from(2);
                }
            },
            "-h" | "--help" => {
                println!("{}", usage());
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown argument: {other}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(config_path) = config_path else {
        eprintln!("missing --config");
        eprintln!("{}", usage());
        return ExitCode::from(2);
    };
    match command {
        "serve" => match run_serve(&config_path, exit_after_ms, bind_override.as_deref()) {
            Ok(code) => ExitCode::from(code as u8),
            Err(err) => {
                eprintln!(
                    "{{\"status\":\"error\",\"mode\":\"khaen-rt-daemon\",\"error\":\"{}\"}}",
                    json_escape(&err)
                );
                ExitCode::from(1)
            }
        },
        "check" => {
            match load_config(&config_path) {
                Ok((config, warnings)) => {
                    let bind = bind_override
                        .clone()
                        .unwrap_or_else(|| config.listen.clone());
                    match probe_free_port(&bind) {
                        Ok(()) => {
                            println!(
                            "{{\"status\":\"ok\",\"mode\":\"khaen-rt-daemon-check\",\"listen\":\"{}\",\"warnings\":[{}]}}",
                            json_escape(&bind),
                            warnings.iter().map(|w| format!("\"{}\"", json_escape(w))).collect::<Vec<_>>().join(",")
                        );
                            ExitCode::SUCCESS
                        }
                        Err(err) => {
                            eprintln!("{{\"status\":\"error\",\"mode\":\"khaen-rt-daemon-check\",\"error\":\"{}\"}}", json_escape(&err));
                            ExitCode::from(1)
                        }
                    }
                }
                Err(err) => {
                    eprintln!("{{\"status\":\"error\",\"mode\":\"khaen-rt-daemon-check\",\"error\":\"{}\"}}", json_escape(&err));
                    ExitCode::from(1)
                }
            }
        }
        other => {
            eprintln!("unknown command: {other}");
            eprintln!("{}", usage());
            ExitCode::from(2)
        }
    }
}

// ---------------------------------------------------------------------------
// 私有测试（task-226：daemon 校准接线；纯离线、不打开设备/不出声）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use khaen_control::rest::calibration_job_port::{
        cleanup_offline_root, unique_offline_root, CAL_PORT_ENGINE_SYNTHETIC,
    };
    use khaen_control::rest::{JsonValue, Method, RestConfig, RestRequest, TOKEN_HEADER};

    const TEST_TOKEN: &str = "test-token-0123456789abcdef";

    fn authed(method: Method, path: &str) -> RestRequest {
        RestRequest::new(method, path).header(TOKEN_HEADER, TEST_TOKEN)
    }

    fn post_json(path: &str, body: &str) -> RestRequest {
        authed(Method::Post, path).json_body(body)
    }

    fn service_with_port(port: Box<dyn CalibrationPort + Send>) -> RestService {
        RestService::with_config(Box::new(MemoryEngine::new()), RestConfig::for_tests())
            .with_calibration(port)
    }

    #[test]
    fn t226_daemon_synthetic_calibration_wiring_marks_synthetic() {
        let dir = unique_offline_root("daemon-wiring");
        let (port, report) =
            build_calibration_with_mode(dir.to_string_lossy().as_ref(), CalBackendMode::Synthetic);
        assert!(report.wired, "synthetic 模式必须挂 port");
        assert!(report.synthetic, "离线 job manager 必须标 synthetic");
        assert!(!report.real_audio);
        assert_eq!(report.engine, CAL_PORT_ENGINE_SYNTHETIC);
        let status = report.to_json();
        assert!(status.contains("\"synthetic\":true"), "status={status}");
        assert!(status.contains("\"wired\":true"), "status={status}");
        assert!(status.contains("\"engine\":\"t60-jobmanager-synthetic\""));

        let service = service_with_port(port.expect("synthetic port"));
        let start = service.handle(&post_json("/api/calibration/jobs", "{\"kind\":\"full\"}"));
        assert_eq!(start.status, 201, "body={}", start.body);
        assert!(
            start.body.contains("\"synthetic\":true"),
            "REST start 必须显式 synthetic：{}",
            start.body
        );
        let body = JsonValue::parse(&start.body).expect("start JSON");
        let job_id = match body.get("job_id") {
            Some(JsonValue::Str(job_id)) => job_id.clone(),
            other => panic!("job_id: {other:?}"),
        };
        // 收尾：取消并等终态（避免测试结束时 worker 线程仍写临时目录）。
        let cancel = service.handle(&post_json(
            &format!("/api/calibration/jobs/{job_id}/cancel"),
            "{}",
        ));
        assert!(
            cancel.status == 200 || cancel.status == 409,
            "cancel body={}",
            cancel.body
        );
        cleanup_offline_root(&dir);
    }

    #[test]
    fn t226_daemon_real_calibration_backend_is_refused_not_faked() {
        let dir = unique_offline_root("daemon-real");
        let (port, report) =
            build_calibration_with_mode(dir.to_string_lossy().as_ref(), CalBackendMode::Real);
        assert!(port.is_some(), "real 挂 host socket 客户端");
        assert!(report.wired);
        assert_eq!(report.engine, "host-socket");
        assert!(report.synthetic, "没有房间录音之前不得标成 actual");
        let status = report.to_json();
        assert!(
            status.contains("\"engine\":\"host-socket\""),
            "status={status}"
        );
        let service = service_with_port(port.expect("port"));
        let full = service.handle(&post_json(
            "/api/calibration/jobs",
            r#"{"kind":"full","audible_sweep_authorized":true}"#,
        ));
        assert_eq!(full.status, 400, "body={}", full.body);
        let quiet = service.handle(&post_json(
            "/api/calibration/jobs",
            r#"{"kind":"emperor","stage":"measure","audible_sweep_authorized":false}"#,
        ));
        assert_eq!(quiet.status, 400, "body={}", quiet.body);
        let down = service.handle(&post_json(
            "/api/calibration/jobs",
            r#"{"kind":"emperor","stage":"solve","audible_sweep_authorized":false}"#,
        ));
        assert_eq!(down.status, 503, "body={}", down.body);
        assert!(
            down.body.contains("service_unavailable"),
            "body={}",
            down.body
        );
        assert!(!down.body.contains("FIELD_SHORTER"), "body={}", down.body);
        cleanup_offline_root(&dir);
    }

    #[test]
    fn emperor_synthetic_stays_predicted_only() {
        let dir = unique_offline_root("daemon-emperor-synthetic");
        let (port, _) =
            build_calibration_with_mode(dir.to_string_lossy().as_ref(), CalBackendMode::Synthetic);
        let service = service_with_port(port.expect("synthetic port"));
        let response = service.handle(&post_json("/api/calibration/jobs", r#"{"kind":"emperor"}"#));
        assert_eq!(response.status, 201, "body={}", response.body);
        assert!(
            response.body.contains("\"synthetic\":true"),
            "body={}",
            response.body
        );
        assert!(
            response.body.contains("PREDICTED_ONLY"),
            "body={}",
            response.body
        );
        assert!(
            !response.body.contains("FIELD_SHORTER"),
            "body={}",
            response.body
        );
        cleanup_offline_root(&dir);
    }

    #[test]
    fn t226_daemon_off_mode_disables_calibration() {
        let dir = unique_offline_root("daemon-off");
        let (port, report) =
            build_calibration_with_mode(dir.to_string_lossy().as_ref(), CalBackendMode::Off);
        assert!(port.is_none());
        assert!(!report.wired);
        assert_eq!(report.mode, "off");
        assert!(report.reason.as_deref().unwrap_or("").contains("disabled"));
        cleanup_offline_root(&dir);
    }

    #[test]
    fn t226_daemon_config_job_dir_is_parsed() {
        let mut warnings = Vec::new();
        let config = parse_config("[daemon]\njob_dir = \"/tmp/khaen-jobs\"\n", &mut warnings)
            .expect("parse");
        assert_eq!(config.job_dir, "/tmp/khaen-jobs");
        assert!(warnings.is_empty(), "warnings={warnings:?}");
    }

    #[test]
    fn t226_daemon_backend_mode_parse_never_silently_enables_real() {
        assert_eq!(CalBackendMode::parse("real"), CalBackendMode::Real);
        assert_eq!(CalBackendMode::parse("board"), CalBackendMode::Real);
        assert_eq!(CalBackendMode::parse("off"), CalBackendMode::Off);
        assert_eq!(CalBackendMode::parse("cli"), CalBackendMode::Cli);
        assert_eq!(CalBackendMode::parse("cli-stage"), CalBackendMode::Cli);
        assert_eq!(CalBackendMode::parse("offline"), CalBackendMode::Synthetic);
        assert_eq!(CalBackendMode::parse(""), CalBackendMode::Synthetic);
    }

    #[test]
    fn calib_web_daemon_cli_backend_reports_real_audio_but_not_applied_to_audio() {
        let dir = unique_offline_root("daemon-cli");
        let (port, report) =
            build_calibration_with_mode(dir.to_string_lossy().as_ref(), CalBackendMode::Cli);
        assert_eq!(report.mode, "cli");
        assert!(report.wired, "cli backend should wire a port");
        assert!(report.real_audio, "cli backend uses real audio");
        assert!(!report.synthetic, "cli backend is not synthetic");
        assert!(report.engine.contains("cli"), "engine={}", report.engine);
        let port = port.expect("cli port");
        let info = port.engine_info();
        assert!(!info.synthetic);
        assert!(info.real_audio);
        assert!(
            info.note.contains("NOT_APPLIED_TO_AUDIO"),
            "note must disclose file-backend apply: {}",
            info.note
        );
        // 不在这里 start job：真实 CLI backend 会在 stage 里 spawn systemctl/声卡路径，
        // 单元测试只验证接线自述；端到端由证据里的 HTTP job 验证。
        cleanup_offline_root(&dir);
    }

    #[test]
    fn calib_web_daemon_calibration_stage_config_is_parsed() {
        let mut warnings = Vec::new();
        let config = parse_config(
            r#"[calibration]
stage = "cli"
[daemon]
listen = "0.0.0.0:80"
"#,
            &mut warnings,
        )
        .expect("parse");
        assert_eq!(config.cal_stage.as_deref(), Some("cli"));
        assert_eq!(config.listen, "0.0.0.0:80");
        assert!(warnings.is_empty(), "warnings={warnings:?}");
    }
}
