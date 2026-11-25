//! 系统总览：CPU/内存/温度/uptime/网络/内核 RT 状态。
//!
//! 全部数据来自 /proc、/sys 与 iproute2，不做任何特权操作；缺文件时返回部分数据并给出 warning。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;

use crate::cmd::CommandRunner;

#[derive(Debug, Clone, Serialize, Default)]
pub struct CpuInfo {
    pub usage_percent: f32,
    pub cores: usize,
    pub model: String,
    pub load1: f32,
    pub load5: f32,
    pub load15: f32,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MemInfo {
    pub total_kb: u64,
    pub available_kb: u64,
    pub used_percent: f32,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct NetInterface {
    pub name: String,
    pub ipv4: Option<String>,
    pub prefix: Option<u8>,
    pub ipv6: Option<String>,
    pub state: String,
    pub mac: Option<String>,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ThermalZone {
    pub name: String,
    pub temp_c: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct RealtimeInfo {
    /// /sys/kernel/realtime 内容（1/0）；文件不存在 = None。
    pub realtime_sysfs: Option<bool>,
    pub kernel_cmdline: String,
    pub preempt: Option<String>,
    pub threadirqs: bool,
    pub isolcpus: Option<String>,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct SystemInfo {
    pub hostname: String,
    pub kernel: String,
    pub os: String,
    pub uptime_secs: u64,
    pub uptime_human: String,
    pub cpu: CpuInfo,
    pub memory: MemInfo,
    pub temperature_c: Option<f32>,
    pub temperature_zone: Option<String>,
    pub thermal_zones: Vec<ThermalZone>,
    pub realtime: RealtimeInfo,
    pub network: Vec<NetInterface>,
    pub warnings: Vec<String>,
    pub mock: bool,
}

#[async_trait]
pub trait SystemBackend: Send + Sync {
    async fn info(&self) -> SystemInfo;
}

pub fn fmt_uptime(secs: u64) -> String {
    let d = secs / 86400;
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if d > 0 {
        format!("{d} 天 {h} 小时 {m} 分")
    } else if h > 0 {
        format!("{h} 小时 {m} 分")
    } else if m > 0 {
        format!("{m} 分 {s} 秒")
    } else {
        format!("{s} 秒")
    }
}

pub fn parse_uptime(text: &str) -> u64 {
    text.split_whitespace()
        .next()
        .and_then(|v| v.parse::<f64>().ok())
        .map(|v| v as u64)
        .unwrap_or(0)
}

pub fn parse_loadavg(text: &str) -> (f32, f32, f32) {
    let mut it = text.split_whitespace();
    let get = |it: &mut std::str::SplitWhitespace| {
        it.next().and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0)
    };
    (get(&mut it), get(&mut it), get(&mut it))
}

pub fn parse_meminfo(text: &str) -> MemInfo {
    let mut total = 0u64;
    let mut available = 0u64;
    let mut free = 0u64;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else { continue };
        let val = parts
            .next()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        match key {
            "MemTotal:" => total = val,
            "MemAvailable:" => available = val,
            "MemFree:" => free = val,
            _ => {}
        }
    }
    if available == 0 {
        available = free;
    }
    let used_percent = if total > 0 && available <= total {
        ((total - available) as f64 / total as f64 * 100.0) as f32
    } else {
        0.0
    };
    MemInfo {
        total_kb: total,
        available_kb: available,
        used_percent: (used_percent * 10.0).round() / 10.0,
    }
}

/// 解析 /proc/stat 第一行，返回 (total, idle) 计数。
pub fn parse_proc_stat_cpu(text: &str) -> Option<(u64, u64)> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    let nums: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|v| v.parse::<u64>().ok())
        .collect();
    if nums.len() < 4 {
        return None;
    }
    let total: u64 = nums.iter().sum();
    // idle + iowait
    let idle = nums[3] + nums.get(4).copied().unwrap_or(0);
    Some((total, idle))
}

pub fn cpu_usage_between(prev: (u64, u64), cur: (u64, u64)) -> f32 {
    let total = cur.0.saturating_sub(prev.0);
    let idle = cur.1.saturating_sub(prev.1);
    if total == 0 {
        return 0.0;
    }
    let usage = (total.saturating_sub(idle)) as f64 / total as f64 * 100.0;
    (usage * 10.0).round() as f32 / 10.0
}

pub fn parse_os_release(text: &str) -> String {
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("PRETTY_NAME=") {
            return v.trim_matches('"').to_string();
        }
    }
    String::new()
}

pub fn parse_cpu_model(cpuinfo: &str) -> String {
    for key in ["model name", "Hardware", "Processor"] {
        for line in cpuinfo.lines() {
            if let Some((k, v)) = line.split_once(':') {
                if k.trim() == key {
                    let v = v.trim();
                    if !v.is_empty() {
                        return v.to_string();
                    }
                }
            }
        }
    }
    "未知 CPU".to_string()
}

/// 解析内核 cmdline 中的 RT 相关标志。
pub fn parse_cmdline(cmdline: &str, realtime_sysfs: Option<bool>) -> RealtimeInfo {
    let mut preempt = None;
    let mut threadirqs = false;
    let mut isolcpus = None;
    let mut rcu_nocbs = None;
    for token in cmdline.split_whitespace() {
        if let Some(v) = token.strip_prefix("preempt=") {
            preempt = Some(v.to_string());
        } else if token == "threadirqs" {
            threadirqs = true;
        } else if let Some(v) = token.strip_prefix("isolcpus=") {
            isolcpus = Some(v.to_string());
        } else if let Some(v) = token.strip_prefix("rcu_nocbs=") {
            rcu_nocbs = Some(v.to_string());
        }
    }
    let mut summary = match realtime_sysfs {
        Some(true) => "PREEMPT_RT 已生效（/sys/kernel/realtime=1）".to_string(),
        Some(false) => {
            "内核支持 PREEMPT_RT，但当前未以实时抢占模式运行（/sys/kernel/realtime=0）".to_string()
        }
        None => "未检测到 /sys/kernel/realtime（可能不是 RT 内核）".to_string(),
    };
    let mut flags = Vec::new();
    if let Some(p) = &preempt {
        flags.push(format!("preempt={p}"));
    }
    if threadirqs {
        flags.push("threadirqs".into());
    }
    if let Some(i) = &isolcpus {
        flags.push(format!("isolcpus={i}"));
    }
    if let Some(r) = &rcu_nocbs {
        flags.push(format!("rcu_nocbs={r}"));
    }
    if !flags.is_empty() {
        summary.push_str("；cmdline: ");
        summary.push_str(&flags.join(" "));
    }
    RealtimeInfo {
        realtime_sysfs,
        kernel_cmdline: cmdline.trim().to_string(),
        preempt,
        threadirqs,
        isolcpus,
        summary,
    }
}

pub fn parse_thermal_millideg(text: &str) -> Option<f32> {
    let v = text.trim().parse::<i64>().ok()?;
    Some((v as f32) / 1000.0)
}

/// 解析 \`ip -j addr show\` 输出。
pub fn parse_ip_json(text: &str) -> Result<Vec<NetInterface>, serde_json::Error> {
    let value: serde_json::Value = serde_json::from_str(text)?;
    let mut out = Vec::new();
    let Some(arr) = value.as_array() else {
        return Ok(out);
    };
    for dev in arr {
        let name = dev
            .get("ifname")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            continue;
        }
        let mut ipv4 = None;
        let mut prefix = None;
        let mut ipv6 = None;
        if let Some(addrs) = dev.get("addr_info").and_then(|v| v.as_array()) {
            for a in addrs {
                let family = a.get("family").and_then(|v| v.as_str()).unwrap_or("");
                let local = a.get("local").and_then(|v| v.as_str()).unwrap_or("");
                if family == "inet" && ipv4.is_none() {
                    ipv4 = Some(local.to_string());
                    prefix = a.get("prefixlen").and_then(|v| v.as_u64()).map(|v| v as u8);
                } else if family == "inet6" && ipv6.is_none() {
                    ipv6 = Some(local.to_string());
                }
            }
        }
        let kind = if name == "lo" {
            "loopback"
        } else if name.starts_with("wl") {
            "wifi"
        } else if name.starts_with("eth") || name.starts_with("en") || name.starts_with("end") {
            "ethernet"
        } else {
            "other"
        };
        out.push(NetInterface {
            name,
            ipv4,
            prefix,
            ipv6,
            state: dev
                .get("operstate")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            mac: dev
                .get("address")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            kind: kind.to_string(),
        });
    }
    Ok(out)
}

async fn read_opt(path: &str) -> Option<String> {
    tokio::fs::read_to_string(path).await.ok()
}

pub struct RealSystemBackend {
    runner: Arc<dyn CommandRunner>,
}

impl RealSystemBackend {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    async fn sample_cpu(&self) -> Option<(u64, u64)> {
        let text = read_opt("/proc/stat").await?;
        parse_proc_stat_cpu(&text)
    }
}

#[async_trait]
impl SystemBackend for RealSystemBackend {
    async fn info(&self) -> SystemInfo {
        let mut warnings = Vec::new();
        let mut out = SystemInfo {
            mock: false,
            ..Default::default()
        };
        out.hostname = match read_opt("/etc/hostname").await {
            Some(t) if !t.trim().is_empty() => t.trim().to_string(),
            _ => std::env::var("HOSTNAME").unwrap_or_else(|_| "khaen-audio".to_string()),
        };
        out.kernel = match read_opt("/proc/sys/kernel/osrelease").await {
            Some(t) => t.trim().to_string(),
            None => {
                if let Ok(co) = self.runner.run("uname", &["-r"]).await {
                    co.stdout.trim().to_string()
                } else {
                    warnings.push("/proc/sys/kernel/osrelease 不可读，uname 不可用".into());
                    String::new()
                }
            }
        };
        if let Some(text) = read_opt("/etc/os-release").await {
            out.os = parse_os_release(&text);
        }
        if let Some(text) = read_opt("/proc/uptime").await {
            out.uptime_secs = parse_uptime(&text);
            out.uptime_human = fmt_uptime(out.uptime_secs);
        }
        if let Some(text) = read_opt("/proc/loadavg").await {
            let (l1, l5, l15) = parse_loadavg(&text);
            out.cpu.load1 = l1;
            out.cpu.load5 = l5;
            out.cpu.load15 = l15;
        }
        if let Some(text) = read_opt("/proc/meminfo").await {
            out.memory = parse_meminfo(&text);
        }
        if let Some(text) = read_opt("/proc/cpuinfo").await {
            out.cpu.model = parse_cpu_model(&text);
        }
        out.cpu.cores = std::thread::available_parallelism()
            .map(|v| v.get())
            .unwrap_or(0);
        let prev = self.sample_cpu().await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        if let (Some(prev), Some(cur)) = (prev, self.sample_cpu().await) {
            out.cpu.usage_percent = cpu_usage_between(prev, cur);
        }

        // 温度：多热区取最高值
        let mut zones = Vec::new();
        if let Ok(mut dir) = tokio::fs::read_dir("/sys/class/thermal").await {
            while let Ok(Some(entry)) = dir.next_entry().await {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.starts_with("thermal_zone") {
                    continue;
                }
                let base = entry.path();
                let temp = tokio::fs::read_to_string(base.join("temp"))
                    .await
                    .ok()
                    .and_then(|t| parse_thermal_millideg(&t));
                let label = tokio::fs::read_to_string(base.join("type"))
                    .await
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|_| name.clone());
                zones.push(ThermalZone {
                    name: label,
                    temp_c: temp,
                });
            }
        }
        zones.sort_by(|a, b| {
            b.temp_c
                .unwrap_or(f32::MIN)
                .partial_cmp(&a.temp_c.unwrap_or(f32::MIN))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if let Some(z) = zones.first() {
            out.temperature_c = z.temp_c;
            out.temperature_zone = Some(z.name.clone());
        }
        out.thermal_zones = zones;

        let realtime_sysfs = read_opt("/sys/kernel/realtime")
            .await
            .and_then(|t| t.trim().parse::<u8>().ok())
            .map(|v| v == 1);
        let cmdline = read_opt("/proc/cmdline").await.unwrap_or_default();
        out.realtime = parse_cmdline(&cmdline, realtime_sysfs);

        match self.runner.run("ip", &["-j", "addr", "show"]).await {
            Ok(co) if co.success => match parse_ip_json(&co.stdout) {
                Ok(v) => out.network = v,
                Err(e) => warnings.push(format!("ip -j 输出解析失败: {e}")),
            },
            Ok(co) => warnings.push(format!(
                "ip -j addr show 失败: {}",
                co.stderr.trim().lines().next().unwrap_or("")
            )),
            Err(e) => warnings.push(format!("无法执行 ip: {e}")),
        }

        out.warnings = warnings;
        out
    }
}

/// mock 后端：返回一份固定的 RK3576 + Debian12 RT 数据。
pub struct MockSystemBackend;

#[async_trait]
impl SystemBackend for MockSystemBackend {
    async fn info(&self) -> SystemInfo {
        SystemInfo {
            hostname: "khaen-audio".into(),
            kernel: "6.1.99-rt36-rk3576".into(),
            os: "Debian GNU/Linux 12 (bookworm)".into(),
            uptime_secs: 12_345,
            uptime_human: fmt_uptime(12_345),
            cpu: CpuInfo {
                usage_percent: 12.5,
                cores: 8,
                model: "Rockchip RK3576 (4×Cortex-A72 + 4×Cortex-A53)".into(),
                load1: 0.42,
                load5: 0.31,
                load15: 0.25,
            },
            memory: MemInfo {
                total_kb: 4_194_304,
                available_kb: 2_617_536,
                used_percent: 37.6,
            },
            temperature_c: Some(48.6),
            temperature_zone: Some("soc-thermal".into()),
            thermal_zones: vec![
                ThermalZone {
                    name: "soc-thermal".into(),
                    temp_c: Some(48.6),
                },
                ThermalZone {
                    name: "bigcore-thermal".into(),
                    temp_c: Some(46.1),
                },
            ],
            realtime: parse_cmdline(
                "console=ttyS2,1500000n8 root=/dev/mmcblk0p4 rw isolcpus=4-7 threadirqs preempt=rt",
                Some(true),
            ),
            network: vec![
                NetInterface {
                    name: "eth0".into(),
                    ipv4: Some("192.168.1.50".into()),
                    prefix: Some(24),
                    ipv6: Some("fe80::21e:6ff:fe12:3456".into()),
                    state: "UP".into(),
                    mac: Some("00:1e:06:12:34:56".into()),
                    kind: "ethernet".into(),
                },
                NetInterface {
                    name: "lo".into(),
                    ipv4: Some("127.0.0.1".into()),
                    prefix: Some(8),
                    ipv6: Some("::1".into()),
                    state: "UNKNOWN".into(),
                    mac: Some("00:00:00:00:00:00".into()),
                    kind: "loopback".into(),
                },
            ],
            warnings: vec![],
            mock: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meminfo_parsing() {
        let text = "MemTotal:        4194304 kB\nMemFree:         1048576 kB\nMemAvailable:    2621440 kB\n";
        let m = parse_meminfo(text);
        assert_eq!(m.total_kb, 4_194_304);
        assert_eq!(m.available_kb, 2_621_440);
        assert!((m.used_percent - 37.5).abs() < 0.2);
    }

    #[test]
    fn uptime_and_loadavg() {
        assert_eq!(parse_uptime("12345.67 98765.43\n"), 12345);
        let (a, b, c) = parse_loadavg("0.42 0.31 0.25 1/234 5678");
        assert!((a - 0.42).abs() < 1e-6);
        assert!((b - 0.31).abs() < 1e-6);
        assert!((c - 0.25).abs() < 1e-6);
    }

    #[test]
    fn cpu_usage_delta() {
        // 100 总节拍，20 idle => 80% 使用
        let prev = (1_000u64, 500u64);
        let cur = (1_100u64, 520u64);
        assert!((cpu_usage_between(prev, cur) - 80.0).abs() < 0.01);
        assert_eq!(cpu_usage_between(prev, prev), 0.0);
    }

    #[test]
    fn proc_stat_parse() {
        let text = "cpu  100 2 50 200 10 0 0 0 0 0\ncpu0 10 0 5 20 1 0 0 0 0 0\n";
        let (total, idle) = parse_proc_stat_cpu(text).unwrap();
        assert_eq!(total, 362);
        assert_eq!(idle, 210);
    }

    #[test]
    fn os_release_and_cpu_model() {
        let os = "NAME=\"Debian GNU/Linux\"\nVERSION_ID=\"12\"\nPRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\n";
        assert_eq!(parse_os_release(os), "Debian GNU/Linux 12 (bookworm)");
        let cpu = "processor\t: 0\nmodel name\t: ARMv8 Processor rev 1\n";
        assert_eq!(parse_cpu_model(cpu), "ARMv8 Processor rev 1");
    }

    #[test]
    fn cmdline_rt_flags() {
        let info = parse_cmdline(
            "root=/dev/mmcblk0p4 rw isolcpus=4-7 threadirqs preempt=rt",
            Some(true),
        );
        assert!(info.threadirqs);
        assert_eq!(info.preempt.as_deref(), Some("rt"));
        assert_eq!(info.isolcpus.as_deref(), Some("4-7"));
        assert!(info.summary.contains("已生效"));
        let non_rt = parse_cmdline("root=/dev/x", None);
        assert!(non_rt.summary.contains("未检测到"));
        let disabled = parse_cmdline("root=/dev/x preempt=voluntary", Some(false));
        assert!(disabled.summary.contains("未以实时抢占模式运行"));
    }

    #[test]
    fn thermal_parse() {
        assert_eq!(parse_thermal_millideg("48600\n"), Some(48.6));
        assert_eq!(parse_thermal_millideg("not-a-number"), None);
    }

    #[test]
    fn ip_json_parse() {
        let text = r#"[
          {"ifindex":2,"ifname":"eth0","flags":["BROADCAST","UP"],"operstate":"UP","address":"00:1e:06:12:34:56",
           "addr_info":[
             {"family":"inet","local":"192.168.1.50","prefixlen":24},
             {"family":"inet6","local":"fe80::1","prefixlen":64}
           ]},
          {"ifindex":1,"ifname":"lo","flags":["LOOPBACK","UP"],"operstate":"UNKNOWN","address":"00:00:00:00:00:00",
           "addr_info":[{"family":"inet","local":"127.0.0.1","prefixlen":8}]}
        ]"#;
        let nets = parse_ip_json(text).unwrap();
        assert_eq!(nets.len(), 2);
        assert_eq!(nets[0].name, "eth0");
        assert_eq!(nets[0].ipv4.as_deref(), Some("192.168.1.50"));
        assert_eq!(nets[0].prefix, Some(24));
        assert_eq!(nets[0].kind, "ethernet");
        assert_eq!(nets[1].kind, "loopback");
    }

    #[test]
    fn uptime_formatting() {
        assert_eq!(fmt_uptime(59), "59 秒");
        assert_eq!(fmt_uptime(3600), "1 小时 0 分");
        assert_eq!(fmt_uptime(90_000), "1 天 1 小时 0 分");
    }
}
