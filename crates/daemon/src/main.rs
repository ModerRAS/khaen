//! daemon 入口：绑 127.0.0.1:<PORT>，只服务本机（公网暴露由前置代理负责）。
//!
//! 设备监督器（T70c）在这里接线：
//! - `KHAEN_DEVICE_ID`：设备标识（在 `/sys/class/sound/card*/id` 里按子串匹配；MOTU 用 `MOTU`）。
//! - `KHAEN_RATE_READ` / `KHAEN_RATE_ASSERT`：读速率 / 写速率命令（argv 用空格分隔，写命令可用
//!   `{rate}` 占位符）。不配置时不做速率写操作，`/api/status` 的 `device.wired` 如实为 false。
//! - `KHAEN_CONTROL_COMMAND`：可选的“拉起控制通道”命令（等价 v1 的 tspi-motu-net-up）。
//! - `KHAEN_DEVICE_POLL_MS`：监督器轮询间隔，默认 500。
//!
//! 一个进程只能有一个监督器 owner；音频线程用块路径上的安全检查，不另开状态机。

use std::sync::Arc;
use std::time::Duration;

use khaen_daemon::{serve, DaemonState, DeviceMonitor};
use khaen_host::supervisor::{
    CommandRateControl, MonotonicClock, NullStream, SupervisorPolicy, SysfsProbe,
};
use khaen_host::EngineConfig;
use tokio::net::TcpListener;

const DEFAULT_PORT: u16 = 8730;
const DEFAULT_POLL_MS: u64 = 500;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = std::env::var("KHAEN_PORT")
        .or_else(|_| std::env::var("PORT"))
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
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
    let state = Arc::new(DaemonState::new(config)?.with_device(device_monitor(plan_rate)));

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

    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    println!("khaen-daemon listening on http://127.0.0.1:{port}");
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
