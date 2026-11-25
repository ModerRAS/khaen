//! daemon 入口：绑定 127.0.0.1:<PORT>，只服务本机（公网暴露由前置代理负责）。

use std::sync::Arc;

use khaen_daemon::{serve, DaemonState};
use khaen_host::EngineConfig;
use tokio::net::TcpListener;

const DEFAULT_PORT: u16 = 8730;

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
    let state = Arc::new(DaemonState::new(config)?);
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    println!("khaen-daemon listening on http://127.0.0.1:{port}");
    serve(listener, state).await?;
    Ok(())
}
