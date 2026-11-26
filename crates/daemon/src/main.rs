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
//!
//! 一个进程只能有一个监督器 owner；音频线程用块路径上的安全检查，不另开状态机。

use std::sync::Arc;
use std::time::Duration;

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
    // rt-host 状态文件路径：启动时解析一次并注入 DaemonState（handler 不读环境）。
    let rt_status_file = std::env::var("KHAEN_RT_STATUS_FILE")
        .ok()
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_RT_STATUS_FILE));
    let state = Arc::new(
        DaemonState::new(config)?
            .with_bind(bind)
            .with_device(device_monitor(plan_rate))
            .with_rt_status_file(rt_status_file),
    );

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
