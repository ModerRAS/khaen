//! SAP 扫描示例：监听 RFC 2974 SAP 通告，打印发现的会话（控制面，不进入 RT 音频路径）。
//!
//! 用法：
//!
//! ```text
//! cargo run -p khaen-aoip-aes67 --example sap_scan -- --interface 192.168.63.94 --seconds 40
//! ```

use std::env;
use std::net::Ipv4Addr;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use khaen_aoip_aes67::sap::{SapMonitor, SapMonitorConfig};

fn main() -> ExitCode {
    let mut interface: Option<Ipv4Addr> = None;
    let mut seconds: u64 = 20;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--interface" => match args.next().and_then(|text| text.parse::<Ipv4Addr>().ok()) {
                Some(address) => interface = Some(address),
                None => {
                    eprintln!("--interface 需要一个 IPv4 地址");
                    return ExitCode::from(2);
                }
            },
            "--seconds" => match args.next().and_then(|text| text.parse::<u64>().ok()) {
                Some(value) => seconds = value,
                None => {
                    eprintln!("--seconds 需要一个整数秒数");
                    return ExitCode::from(2);
                }
            },
            other => {
                eprintln!("未知参数 {other:?}");
                return ExitCode::from(2);
            }
        }
    }
    let config = SapMonitorConfig {
        interface,
        ..SapMonitorConfig::global()
    };
    let monitor = match SapMonitor::start(config) {
        Ok(monitor) => monitor,
        Err(error) => {
            eprintln!("SAP 监听启动失败: {error:?}");
            return ExitCode::from(3);
        }
    };
    println!(
        "SAP 监听 {} 秒，本端 {}，组播组 224.2.127.254:9875",
        seconds,
        monitor.local_addr()
    );
    let started = Instant::now();
    let mut previous: Vec<(String, String)> = Vec::new();
    while started.elapsed() < Duration::from_secs(seconds) {
        thread::sleep(Duration::from_millis(500));
        let mut current: Vec<(String, String)> = Vec::new();
        for session in monitor.snapshot() {
            let key = session.key.text();
            let summary = summarise(&session.sdp);
            let changed = !previous
                .iter()
                .any(|(old_key, old_summary)| old_key == &key && old_summary == &summary);
            if changed {
                println!(
                    "发现会话 {key} profile_error={:?} announce_period_ms={}",
                    session.profile_error, session.announce_period_ms
                );
                println!("    {summary}");
            }
            current.push((key, summary));
        }
        previous = current;
    }
    let sessions = monitor.snapshot();
    let counters = monitor.counters();
    println!(
        "结束：{} 个会话，datagrams={} announcements={} rejected={} expired={}",
        sessions.len(),
        counters.datagrams,
        counters.announcements,
        counters.rejected,
        counters.expired
    );
    monitor.stop();
    ExitCode::SUCCESS
}

fn summarise(sdp: &str) -> String {
    sdp.lines()
        .filter(|line| {
            line.starts_with("s=")
                || line.starts_with("o=")
                || line.starts_with("c=")
                || line.starts_with("m=")
                || line.starts_with("a=rtpmap")
                || line.starts_with("a=ptime")
                || line.starts_with("a=clock-domain")
        })
        .map(str::to_string)
        .collect::<Vec<_>>()
        .join(" | ")
}
