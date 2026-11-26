//! daemon 入口：按 `KHAEN_BIND` / `KHAEN_PORT` 绑定监听地址（默认 `127.0.0.1:8730`，只服务本机）。
//!
//! - `KHAEN_BIND`：完整 socket 地址（如生产 `0.0.0.0:80`）；默认 `127.0.0.1:8730`。
//! - `KHAEN_PORT`（兼容旧配置）/ `PORT`：只给端口，绑定 `127.0.0.1`。
//! - 非法值**启动即失败**：stderr 打印稳定错误并以退出码 2 退出，不再静默回退默认值
//!   （解析规则与优先级见 `khaen_daemon::bind`）。
//!
//! 设备监督器（T70c）在这里接线：
//! - `KHAEN_DEVICE_ID`：设备标识（在 `/sys/class/sound/card*/id` 里按子串匹配；MOTU 用 `MOTU`）。
//! - `KHAEN_RATE_READ` / `KHAEN_RATE_ASSERT`：读速率 / 写速率命令（argv 用空格分隔，写命令可用
//!   `{rate}` 占位符）。不配置时不做速率写操作，`/api/device` 的 `device.wired` 如实为 false。
//! - `KHAEN_RT_STATUS_FILE`：rt-host 状态文件路径（`/api/status` 聚合读取它；默认
//!   `/var/lib/khaen/rt-host-status.json`）。这里是唯一读这些环境变量的地方，
//!   解析结果注入 `DaemonState`，handler 不读环境。
//! - `KHAEN_CONTROL_COMMAND`：可选的“拉起控制通道”命令（等价 v1 的 tspi-motu-net-up）。
//! - `KHAEN_DEVICE_POLL_MS`：监督器轮询间隔，默认 500。
//! - `KHAEN_CAL_BACKEND`：校准 job 生产后端开关（缺省在 daemon 状态目录下接线
//!   JobManager + 离线 backend；`off`/`0` 显式关闭）。初始化失败（目录不可写/索引损坏）
//!   如实降级为 not_wired + 真实原因，不静默、不假成功。
//! - `KHAEN_RT_TARGET` / `KHAEN_RT_SOCKET` / `KHAEN_RT_TIMEOUT_MS`：daemon→rt-host 控制通道
//!   （T70f-b，见 `khaen_daemon::host_link`）。目标默认 `rt-host`（`process` 仅测试/离线演示，
//!   响应与状态显式标 `target: daemon-process`）；socket 默认 `/run/khaen/rt-host.sock`；
//!   往返超时默认 10000 ms（必须大于 rt-host 的 5 s 块边界 ack 上限）。非法值启动即失败
//!   （稳定错误 + 退出码 2），不静默回退。
//!
//! 一个进程只能有一个监督器 owner；音频线程用块路径上的安全检查，不另开状态机。

use std::sync::Arc;
use std::time::Duration;

use khaen_daemon::motu::{MotuConfig, MotuControl, NativeMotu};
use khaen_daemon::status::DEFAULT_RT_STATUS_FILE;
use khaen_daemon::{serve, DaemonState, DeviceMonitor};
use khaen_host::supervisor::{
    CommandRateControl, MonotonicClock, NullStream, SupervisorPolicy, SysfsProbe,
};
use khaen_host::EngineConfig;
use tokio::net::TcpListener;

const DEFAULT_POLL_MS: u64 = 500;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 先解析监听地址：配置非法时立刻退出（稳定错误 + 退出码 2），不做任何设备/线程初始化。
    let bind = match khaen_daemon::bind::from_env() {
        Ok(bind) => bind,
        Err(error) => {
            eprintln!("khaen-daemon: {error}");
            std::process::exit(2);
        }
    };
    let config = match (
        std::env::var("KHAEN_BLOCK_FRAMES").ok(),
        std::env::var("KHAEN_SAMPLE_RATE").ok(),
        std::env::var("KHAEN_CHANNELS").ok(),
    ) {
        (Some(frames), Some(rate), Some(channels)) => EngineConfig {
            block_frames: frames.parse().unwrap_or(128),
            sample_rate: rate.parse().unwrap_or(96_000),
            channels: channels.parse().unwrap_or(2),
        },
        _ => EngineConfig::default(),
    };
    let plan_rate = config.sample_rate;
    // 控制通道配置：默认 rt-host（真数据面）；非法值启动即失败（与 bind 同样的 fail-closed 策略）。
    let host_link = match khaen_daemon::host_link::HostLinkConfig::from_env() {
        Ok(config) => khaen_daemon::host_link::HostLink::new(config),
        Err(error) => {
            eprintln!("khaen-daemon: {error}");
            std::process::exit(2);
        }
    };
    // rt-host 状态文件路径：启动时解析一次并注入 DaemonState（handler 不读环境）。
    let rt_status_file = std::env::var("KHAEN_RT_STATUS_FILE")
        .ok()
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_RT_STATUS_FILE));
    // 系统页重启能力（T46）：只有 KHAEN_ALLOW_RESTART=1 才允许 POST /api/system/restart
    // 重启 khaen-rt-host；handler 不读环境，解析后注入 DaemonState。
    let restart_policy = khaen_daemon::system::RestartPolicy::from_env();
    // MOTU 控制面（T57）：默认跟随 crate 默认检测（/proc/asound/cards + 169.254.111.126:1280）；
    // KHAEN_MOTU_* 覆盖地址/密码/超时或显式关闭，非法值启动即失败（与 bind 同样的 fail-closed）。
    let motu_config = match MotuConfig::from_env() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("khaen-daemon: {error}");
            std::process::exit(2);
        }
    };
    let motu_control: Arc<dyn MotuControl> = Arc::new(NativeMotu::new(motu_config.clone()));
    let mut state = DaemonState::new(config)?
        .with_bind(bind)
        .with_device(device_monitor(plan_rate))
        .with_rt_status_file(rt_status_file)
        .with_restart_policy(restart_policy.clone())
        .with_host_link(host_link)
        .with_motu(Arc::clone(&motu_control));
    // 校准 job 生产后端（T70h-b）：默认在 daemon 状态目录下接线 JobManager + 离线 backend。
    // 初始化失败（目录不可写/索引损坏）时如实降级为 not_wired + 真实原因，不静默吞掉；
    // KHAEN_CAL_BACKEND=off 显式关闭（保留上传/向导，job 写操作 503 not_wired）。
    let calibration_backend = match std::env::var("KHAEN_CAL_BACKEND").ok().as_deref() {
        Some("off") | Some("0") => Err(
            "KHAEN_CAL_BACKEND=off: the calibration job backend is disabled by configuration"
                .to_string(),
        ),
        _ => khaen_daemon::calibration::backend::JobManagerCalibration::new(state.state_dir()),
    };
    match calibration_backend {
        Ok(backend) => {
            let status =
                khaen_daemon::calibration::CalibrationService::status(&backend).unwrap_or_default();
            println!(
                "khaen-daemon calibration backend wired=true state_dir={} engine={}",
                state.state_dir().display(),
                status.engine.as_deref().unwrap_or("-")
            );
            state = state.with_calibration(Arc::new(backend));
        }
        Err(reason) => {
            eprintln!("khaen-daemon calibration backend not wired: {reason}");
            state = state.with_calibration(Arc::new(
                khaen_daemon::calibration::NotWiredCalibration::with_reason(format!(
                    "calibration job backend failed to initialize: {reason}"
                )),
            ));
        }
    }
    let state = Arc::new(state);

    let poll_ms = std::env::var("KHAEN_DEVICE_POLL_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_POLL_MS)
        .max(1);
    let poll_state = Arc::clone(&state);
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(poll_ms));
        poll_state.poll_device();
    });

    let host_link = state.host_link();
    println!(
        "khaen-daemon host-link target={} socket={} timeout_ms={}",
        host_link.target().as_str(),
        host_link
            .socket()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "-".to_string()),
        host_link.config().timeout.as_millis()
    );
    println!(
        "khaen-daemon system restart rt-host allowed={} reason={}",
        restart_policy.allowed(),
        restart_policy.reason().unwrap_or("-")
    );
    // MOTU 检测结果（只读：Windows/无设备 detected=false 不报错）。
    let motu_detection = motu_control.detection();
    println!(
        "khaen-daemon motu detected={} control_reachable={} model={} reason={} disabled={}",
        motu_detection.detected,
        motu_detection.control_reachable,
        motu_detection.model.as_deref().unwrap_or("-"),
        motu_detection.reason,
        motu_config.disabled,
    );
    let device = state.device_json();
    println!(
        "khaen-daemon device wired={} state={} reason={}",
        device["wired"],
        device["state"],
        device["reason"].as_str().unwrap_or("-")
    );

    let listener = TcpListener::bind(bind).await?;
    println!("khaen-daemon listening on http://{bind}");
    serve(listener, state).await?;
    Ok(())
}

/// 按环境变量构造设备监督器；缺少设备标识或速率命令时如实报告 not wired。
fn device_monitor(plan_rate: u32) -> DeviceMonitor {
    let Some(device_id) = std::env::var("KHAEN_DEVICE_ID")
        .ok()
        .filter(|id| !id.is_empty())
    else {
        return DeviceMonitor::disabled(
            "KHAEN_DEVICE_ID is not set: device supervisor disabled (no probe)",
        );
    };
    let rate_read = split_argv("KHAEN_RATE_READ");
    let rate_assert = split_argv("KHAEN_RATE_ASSERT");
    let (Some(rate_read), Some(rate_assert)) = (rate_read, rate_assert) else {
        return DeviceMonitor::disabled(format!(
            "device {device_id} present but KHAEN_RATE_READ/KHAEN_RATE_ASSERT are not configured:              rate ownership disabled (fail-closed)"
        ));
    };
    let mut probe = SysfsProbe::new(device_id.clone()).with_card_match(device_id);
    if let Some(argv) = split_argv("KHAEN_CONTROL_COMMAND") {
        probe = probe.with_control_command(argv);
    }
    DeviceMonitor::with_components(
        plan_rate,
        SupervisorPolicy::default(),
        Box::new(probe),
        Box::new(CommandRateControl::new(rate_read, rate_assert)),
        Box::new(NullStream),
        Box::new(MonotonicClock::new()),
    )
}

fn split_argv(key: &str) -> Option<Vec<String>> {
    let value = std::env::var(key).ok()?;
    let argv: Vec<String> = value.split_whitespace().map(ToString::to_string).collect();
    if argv.is_empty() {
        None
    } else {
        Some(argv)
    }
}
