//! 真机 CueMix：发现 CDC-ECM 口、补链路本地地址、听 UDP 1280 信标、用 WebSocket 读写 datastore。
//!
//! mock 不会构造这个类型，因此也不会打开套接字或执行 `ip`。

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::cmd::CommandRunner;
use crate::config::MotuConfig;
use crate::error::{AppError, AppResult};
use crate::motu::codec::{
    encode_ops, host_faders_changed, is_protected_fader_op, mismatches, plan_monitor, plan_restore,
    plan_restore_direct, plan_zero_direct, rollback_ops, Datastore, WriteOp, FINAL_INSERT_CELLS,
};
use crate::motu::store::FileSnapshotStore;
use crate::motu::{
    build_snapshot, build_status, DiscoveryInfo, MotuBackend, MotuStatus, SnapshotView, WriteReport,
};

const IP_CACHE: Duration = Duration::from_secs(60);

struct Cache {
    store: Datastore,
    discovery: DiscoveryInfo,
    online: bool,
    reason: Option<String>,
    last_error: Option<String>,
    last_read_unix: Option<u64>,
    fetched_at: Option<Instant>,
    last_ip: Option<String>,
    last_ip_at: Option<Instant>,
    latest_snapshot_id: Option<String>,
}

pub struct RealMotu {
    cfg: MotuConfig,
    runner: Arc<dyn CommandRunner>,
    snaps: FileSnapshotStore,
    inner: tokio::sync::Mutex<Cache>,
}

impl RealMotu {
    pub fn new(cfg: MotuConfig, runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            snaps: FileSnapshotStore::new(&cfg.snapshot_dir),
            cfg,
            runner,
            inner: tokio::sync::Mutex::new(Cache {
                store: Datastore::default(),
                discovery: DiscoveryInfo::default(),
                online: false,
                reason: Some("尚未读取设备".into()),
                last_error: None,
                last_read_unix: None,
                fetched_at: None,
                last_ip: None,
                last_ip_at: None,
                latest_snapshot_id: None,
            }),
        }
    }

    fn ttl(&self) -> Duration {
        Duration::from_millis(self.cfg.cache_ttl_ms.clamp(500, 60_000))
    }

    async fn status_from_cache(cache: &Cache, age_ms: u64) -> MotuStatus {
        build_status(
            &cache.store,
            false,
            cache.online,
            cache.reason.clone(),
            cache.discovery.clone(),
            cache.last_read_unix,
            cache.last_error.clone(),
            cache.latest_snapshot_id.clone(),
            age_ms,
        )
    }

    async fn refresh_locked(&self, cache: &mut Cache) -> MotuStatus {
        match self.read_live(cache).await {
            Ok(()) => {}
            Err(reason) => {
                cache.online = false;
                cache.reason = Some(clip(&reason));
                cache.fetched_at = Some(Instant::now());
                eprintln!("motu: offline {}", clip(&reason));
            }
        }
        let age = cache
            .fetched_at
            .map(|t| t.elapsed().as_millis() as u64)
            .unwrap_or(0);
        Self::status_from_cache(cache, age).await
    }

    async fn read_live(&self, cache: &mut Cache) -> Result<(), String> {
        let (iface, beacon) = self.locate(cache).await?;
        let read = read_ws(
            &beacon.ip,
            self.cfg.ws_port,
            Duration::from_millis(self.cfg.read_timeout_ms.max(500)),
            Duration::from_millis(self.cfg.idle_ms.clamp(50, 2_000)),
        )
        .await?;
        if read.password_enabled == Some(true) {
            cache.online = false;
            cache.store = Datastore::default();
            cache.discovery = beacon_discovery(&iface, &beacon);
            cache.reason = Some("设备已设密码，暂不支持".into());
            cache.last_error = cache.reason.clone();
            cache.fetched_at = Some(Instant::now());
            return Err("设备已设密码，暂不支持".into());
        }
        let count = read.store.message_count;
        eprintln!(
            "motu: iface={iface} ip={} messages={count} bytes={}",
            beacon.ip, read.bytes
        );
        cache.store = read.store;
        cache.discovery = beacon_discovery(&iface, &beacon);
        cache.online = true;
        cache.reason = None;
        cache.last_error = None;
        cache.last_read_unix = Some(crate::util::now_unix());
        cache.fetched_at = Some(Instant::now());
        cache.last_ip = Some(beacon.ip);
        cache.last_ip_at = Some(Instant::now());
        Ok(())
    }

    async fn locate(&self, cache: &Cache) -> Result<(String, Beacon), String> {
        let link = self
            .runner
            .run("ip", &["-o", "link"])
            .await
            .map_err(|e| format!("读取网口失败: {e}"))?;
        if link.stdout.trim().is_empty() && !link.success {
            return Err(format!("读取网口失败: {}", clip(&link.stderr)));
        }
        let ifaces = parse_enx_ifaces(&link.stdout);
        let iface = pick_iface(&ifaces)
            .map(|item| item.name.clone())
            .ok_or_else(|| "未发现 CDC-ECM 网口（enx*）".to_string())?;
        ensure_link_local(&*self.runner, &iface, &self.cfg.link_local_cidr).await?;
        match listen_beacon(Duration::from_millis(
            self.cfg.discover_timeout_ms.clamp(200, 10_000),
        ))
        .await
        {
            Ok(beacon) => Ok((iface, beacon)),
            Err(err) => {
                if let (Some(ip), Some(at)) = (&cache.last_ip, cache.last_ip_at) {
                    if at.elapsed() < IP_CACHE {
                        return Ok((
                            iface,
                            Beacon {
                                uid: cache.discovery.uid.clone(),
                                name: cache.discovery.name.clone(),
                                ip: ip.clone(),
                                ipv6: cache.discovery.ipv6.clone(),
                                model: cache.discovery.model.clone(),
                                version: cache.discovery.version.clone(),
                                interval: cache.discovery.interval,
                            },
                        ));
                    }
                }
                Err(err)
            }
        }
    }

    async fn commit(
        &self,
        cache: &mut Cache,
        ops: Vec<WriteOp>,
        candidates: usize,
        action: &str,
        guard_host: bool,
    ) -> AppResult<WriteReport> {
        if !cache.online {
            return Err(AppError::unavailable(
                cache.reason.clone().unwrap_or_else(|| "设备离线".into()),
            ));
        }
        if guard_host && ops.iter().any(is_protected_fader_op) {
            return Err(AppError::internal("拒绝写入：计划包含 USB 播放路径 18–29"));
        }
        if ops.is_empty() {
            return Ok(WriteReport::noop(action, candidates));
        }
        let before = cache.store.clone();
        let snapshot_id = self.snaps.save(&before).await?;
        cache.latest_snapshot_id = Some(snapshot_id.clone());
        let ip = cache
            .last_ip
            .clone()
            .ok_or_else(|| AppError::unavailable("没有设备 IP，无法写入"))?;
        let frames = encode_ops(&ops).map_err(AppError::internal)?;
        if let Err(err) = push_frames(&ip, self.cfg.ws_port, &frames).await {
            return Ok(self
                .rollback(
                    cache,
                    &before,
                    &ops,
                    &ip,
                    &snapshot_id,
                    candidates,
                    0,
                    Vec::new(),
                    &err,
                )
                .await);
        }
        match read_ws(
            &ip,
            self.cfg.ws_port,
            Duration::from_millis(self.cfg.read_timeout_ms.max(500)),
            Duration::from_millis(self.cfg.idle_ms.clamp(50, 2_000)),
        )
        .await
        {
            Ok(after) => {
                let bad = mismatches(&after.store, &ops);
                let host_bad = guard_host && host_faders_changed(&before, &after.store);
                if bad.is_empty() && !host_bad {
                    cache.store = after.store;
                    cache.online = true;
                    cache.reason = None;
                    cache.last_error = None;
                    cache.last_read_unix = Some(crate::util::now_unix());
                    cache.fetched_at = Some(Instant::now());
                    return Ok(WriteReport::applied(
                        action,
                        ops.len(),
                        candidates,
                        snapshot_id,
                        !host_faders_changed(&before, &cache.store),
                    ));
                }
                let cause = if host_bad {
                    "写入后 USB 播放路径发生变化".to_string()
                } else {
                    format!("读回校验不一致（{} 处）", bad.len())
                };
                Ok(self
                    .rollback(
                        cache,
                        &before,
                        &ops,
                        &ip,
                        &snapshot_id,
                        candidates,
                        ops.len(),
                        bad,
                        &cause,
                    )
                    .await)
            }
            Err(err) => Ok(self
                .rollback(
                    cache,
                    &before,
                    &ops,
                    &ip,
                    &snapshot_id,
                    candidates,
                    ops.len(),
                    Vec::new(),
                    &format!("写入后读回失败: {err}"),
                )
                .await),
        }
    }

    async fn rollback(
        &self,
        cache: &mut Cache,
        before: &Datastore,
        ops: &[WriteOp],
        ip: &str,
        snapshot_id: &str,
        candidates: usize,
        written: usize,
        mismatches_found: Vec<crate::motu::codec::Mismatch>,
        cause: &str,
    ) -> WriteReport {
        let rb = rollback_ops(before, ops);
        let mut note = cause.to_string();
        if !rb.is_empty() {
            match encode_ops(&rb) {
                Ok(frames) => {
                    if let Err(err) = push_frames(ip, self.cfg.ws_port, &frames).await {
                        note = format!("{note}；回滚发送失败: {err}");
                    }
                }
                Err(err) => note = format!("{note}；回滚编码失败: {err}"),
            }
        }
        let rollback_verified = match read_ws(
            ip,
            self.cfg.ws_port,
            Duration::from_millis(self.cfg.read_timeout_ms.max(500)),
            Duration::from_millis(self.cfg.idle_ms.clamp(50, 2_000)),
        )
        .await
        {
            Ok(after) => {
                let verified = mismatches(&after.store, &rb).is_empty();
                cache.store = after.store;
                cache.online = true;
                cache.reason = None;
                cache.last_read_unix = Some(crate::util::now_unix());
                verified
            }
            Err(err) => {
                cache.online = false;
                cache.reason = Some(clip(&format!("{note}；回滚后读回失败: {err}")));
                false
            }
        };
        cache.last_error = Some(clip(&note));
        cache.fetched_at = Some(Instant::now());
        eprintln!(
            "motu: rollback verified={rollback_verified} cause={}",
            clip(&note)
        );
        WriteReport::rolled_back(
            ops.len(),
            written,
            candidates,
            snapshot_id,
            rollback_verified,
            &note,
            mismatches_found,
        )
    }
}

#[async_trait]
impl MotuBackend for RealMotu {
    async fn status(&self) -> MotuStatus {
        let mut cache = self.inner.lock().await;
        if let Some(at) = cache.fetched_at {
            if at.elapsed() < self.ttl() {
                return Self::status_from_cache(&cache, at.elapsed().as_millis() as u64).await;
            }
        }
        self.refresh_locked(&mut cache).await
    }

    async fn snapshot(&self, raw: bool) -> AppResult<SnapshotView> {
        let mut cache = self.inner.lock().await;
        if cache
            .fetched_at
            .map(|t| t.elapsed() >= self.ttl())
            .unwrap_or(true)
        {
            self.refresh_locked(&mut cache).await;
        }
        Ok(build_snapshot(
            &cache.store,
            cache.online,
            cache.reason.clone(),
            cache.discovery.clone(),
            raw,
        ))
    }

    async fn set_monitor(&self, trim: u8) -> AppResult<WriteReport> {
        let mut cache = self.inner.lock().await;
        if let Err(reason) = self.read_live(&mut cache).await {
            return Err(AppError::unavailable(reason));
        }
        let ops = plan_monitor(&cache.store, trim);
        self.commit(&mut cache, ops, 1, "设置 Main Out 监听衰减", false)
            .await
    }

    async fn direct_mute(&self, mute: bool) -> AppResult<WriteReport> {
        let mut cache = self.inner.lock().await;
        if let Err(reason) = self.read_live(&mut cache).await {
            return Err(AppError::unavailable(reason));
        }
        let (ops, action) = if mute {
            (plan_zero_direct(&cache.store), "关闭输入直通")
        } else {
            let (_id, snap) = self.snaps.load(None).await?;
            (plan_restore_direct(&cache.store, &snap), "按快照恢复直通")
        };
        if ops.iter().any(is_protected_fader_op) {
            return Err(AppError::internal("拒绝写入：计划包含 USB 播放路径 18–29"));
        }
        self.commit(&mut cache, ops, FINAL_INSERT_CELLS, action, true)
            .await
    }

    async fn restore(&self, snapshot_id: Option<String>) -> AppResult<WriteReport> {
        let mut cache = self.inner.lock().await;
        if let Err(reason) = self.read_live(&mut cache).await {
            return Err(AppError::unavailable(reason));
        }
        let (_id, snap) = self.snaps.load(snapshot_id.as_deref()).await?;
        let (candidates, ops) = plan_restore(&cache.store, &snap);
        self.commit(&mut cache, ops, candidates, "恢复快照", false)
            .await
    }
}

struct LiveRead {
    store: Datastore,
    bytes: usize,
    password_enabled: Option<bool>,
}

async fn read_ws(ip: &str, port: u16, total: Duration, idle: Duration) -> Result<LiveRead, String> {
    let url = ws_url(ip, port);
    let (ws, _) = tokio::time::timeout(Duration::from_secs(3), connect_async(&url))
        .await
        .map_err(|_| format!("连接 {url} 超时"))?
        .map_err(|e| format!("连接 {url} 失败: {e}"))?;
    let (mut sink, mut stream) = ws.split();
    let mut store = Datastore::default();
    let mut bytes = 0usize;
    let mut password_enabled = None;
    let started = Instant::now();
    let mut last_rx: Option<Instant> = None;
    loop {
        if started.elapsed() >= total {
            break;
        }
        if last_rx.is_some_and(|t| t.elapsed() >= idle) && store.message_count > 0 {
            break;
        }
        let slice = if last_rx.is_some() {
            idle.saturating_sub(last_rx.unwrap().elapsed())
        } else {
            total.saturating_sub(started.elapsed())
        };
        let wait = slice
            .min(total.saturating_sub(started.elapsed()))
            .max(Duration::from_millis(1));
        match tokio::time::timeout(wait, stream.next()).await {
            Ok(Some(Ok(Message::Binary(data)))) => {
                bytes += data.len();
                let decoded = store.ingest_frame(&data);
                if decoded.password_enabled == Some(true) {
                    password_enabled = Some(true);
                } else if decoded.password_enabled == Some(false) && password_enabled.is_none() {
                    password_enabled = Some(false);
                }
                last_rx = Some(Instant::now());
            }
            Ok(Some(Ok(Message::Ping(payload)))) => {
                let _ = sink.send(Message::Pong(payload)).await;
            }
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) => break,
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(err))) => return Err(format!("读取 {url} 失败: {err}")),
            Err(_) => {
                if last_rx.is_some_and(|t| t.elapsed() >= idle) && store.message_count > 0 {
                    break;
                }
                if started.elapsed() >= total {
                    break;
                }
            }
        }
    }
    let _ = sink.send(Message::Close(None)).await;
    if password_enabled == Some(true) {
        return Ok(LiveRead {
            store,
            bytes,
            password_enabled,
        });
    }
    if store.message_count == 0 {
        return Err(format!("已连接 {url}，但在超时内没有收到 datastore"));
    }
    Ok(LiveRead {
        store,
        bytes,
        password_enabled,
    })
}

async fn push_frames(ip: &str, port: u16, frames: &[Vec<u8>]) -> Result<(), String> {
    let url = ws_url(ip, port);
    let (ws, _) = tokio::time::timeout(Duration::from_secs(3), connect_async(&url))
        .await
        .map_err(|_| format!("连接 {url} 超时"))?
        .map_err(|e| format!("连接 {url} 失败: {e}"))?;
    let (mut sink, _stream) = ws.split();
    let n = frames.len();
    let bytes: usize = frames.iter().map(Vec::len).sum();
    for frame in frames {
        sink.send(Message::binary(frame.clone()))
            .await
            .map_err(|e| format!("发送失败: {e}"))?;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let _ = sink.send(Message::Close(None)).await;
    eprintln!("motu: wrote frames={n} bytes={bytes}");
    Ok(())
}

#[derive(Debug, Clone)]
struct Beacon {
    uid: Option<String>,
    name: Option<String>,
    ip: String,
    ipv6: Option<String>,
    model: Option<String>,
    version: Option<String>,
    interval: Option<u64>,
}

fn beacon_discovery(iface: &str, beacon: &Beacon) -> DiscoveryInfo {
    DiscoveryInfo {
        uid: beacon.uid.clone(),
        name: beacon.name.clone(),
        ip: Some(beacon.ip.clone()),
        ipv6: beacon.ipv6.clone(),
        model: beacon.model.clone(),
        version: beacon.version.clone(),
        iface: Some(iface.to_string()),
        interval: beacon.interval,
    }
}

fn ws_url(ip: &str, port: u16) -> String {
    if ip.contains(':') {
        format!("ws://[{ip}]:{port}/")
    } else {
        format!("ws://{ip}:{port}/")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfaceInfo {
    pub name: String,
    pub mac: String,
}

pub fn parse_enx_ifaces(text: &str) -> Vec<IfaceInfo> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((_, after_idx)) = line.split_once(':') else {
            continue;
        };
        let after_idx = after_idx.trim();
        let Some((name, _)) = after_idx.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if !name.starts_with("enx") {
            continue;
        }
        let mac = line
            .split("link/ether")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap_or("")
            .to_string();
        out.push(IfaceInfo {
            name: name.to_string(),
            mac,
        });
    }
    out
}

pub fn pick_iface(ifaces: &[IfaceInfo]) -> Option<&IfaceInfo> {
    ifaces
        .iter()
        .find(|item| item.mac.to_ascii_lowercase().starts_with("00:01:f2"))
        .or_else(|| ifaces.first())
}

pub fn addr_present(output: &str, ip: &str) -> bool {
    output
        .split_whitespace()
        .any(|word| word == ip || word.starts_with(&format!("{ip}/")))
}

async fn ensure_link_local(
    runner: &dyn CommandRunner,
    iface: &str,
    cidr: &str,
) -> Result<(), String> {
    let ip = cidr
        .split('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(cidr);
    let show = runner
        .run("ip", &["-o", "addr", "show", "dev", iface])
        .await
        .map_err(|e| format!("读取 {iface} 地址失败: {e}"))?;
    if addr_present(&show.stdout, ip) {
        return Ok(());
    }
    let add = runner
        .run("ip", &["addr", "add", cidr, "dev", iface])
        .await
        .map_err(|e| format!("为 {iface} 添加 {cidr} 失败: {e}"))?;
    if add.success
        || add.stderr.contains("File exists")
        || add.stderr.to_ascii_lowercase().contains("exists")
    {
        return Ok(());
    }
    Err(format!(
        "为 {iface} 添加 {cidr} 失败: {}",
        clip(&add.stderr)
    ))
}

fn parse_beacon_bytes(buf: &[u8]) -> Option<Beacon> {
    let text = String::from_utf8_lossy(buf);
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end < start {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&text[start..=end]).ok()?;
    let ip = value.get("ip")?.as_str()?.trim().to_string();
    if ip.is_empty() {
        return None;
    }
    Some(Beacon {
        uid: value
            .get("uid")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        name: value
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        ip,
        ipv6: value
            .get("ipv6")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        model: value
            .get("model")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        version: value
            .get("version")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        interval: value.get("interval").and_then(|v| v.as_u64()),
    })
}

async fn listen_beacon(timeout: Duration) -> Result<Beacon, String> {
    let sock = tokio::net::UdpSocket::bind("0.0.0.0:1280")
        .await
        .map_err(|e| format!("无法监听 UDP 1280（信标）: {e}"))?;
    let _ = sock.set_broadcast(true);
    let mut buf = vec![0u8; 2048];
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("未收到 MOTU UDP 1280 信标".into());
        }
        match tokio::time::timeout(left, sock.recv(&mut buf)).await {
            Ok(Ok(n)) => {
                if let Some(beacon) = parse_beacon_bytes(&buf[..n]) {
                    return Ok(beacon);
                }
            }
            Ok(Err(err)) => return Err(format!("读取信标失败: {err}")),
            Err(_) => return Err("未收到 MOTU UDP 1280 信标".into()),
        }
    }
}

fn clip(text: &str) -> String {
    let mut out = String::new();
    for (i, ch) in text.chars().enumerate() {
        if i >= 300 {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_enx_and_beacon_without_sockets() {
        let text = "\
2: lo: <LOOPBACK,UP> mtu 65536 qdisc noqueue state UNKNOWN mode DEFAULT group default qlen 1000\\    link/loopback 00:00:00:00:00:00 brd 00:00:00:00:00:00
3: enx0001f2fe7e6f: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc fq_codel state UP mode DEFAULT group default qlen 1000\\    link/ether 00:01:f2:fe:7e:6f brd ff:ff:ff:ff:ff:ff
4: enx001122334455: <BROADCAST> mtu 1500 qdisc fq_codel state DOWN mode DEFAULT group default qlen 1000\\    link/ether 00:11:22:33:44:55 brd ff:ff:ff:ff:ff:ff
";
        let ifaces = parse_enx_ifaces(text);
        assert_eq!(ifaces.len(), 2);
        assert_eq!(pick_iface(&ifaces).unwrap().name, "enx0001f2fe7e6f");
        assert!(addr_present(
            "2: enx0001f2fe7e6f    inet 169.254.111.200/16 brd 169.254.255.255 scope link",
            "169.254.111.200"
        ));
        assert!(!addr_present("inet 192.168.1.20/24", "169.254.111.200"));

        let raw = br#"noise {"uid":"UL5LFE7E6E","name":"UltraLite-mk5","ip":"169.254.111.126","ipv6":"fe80::1","model":"UltraLite-mk5","version":"2.1.2+3352","interval":1}"#;
        let beacon = parse_beacon_bytes(raw).unwrap();
        assert_eq!(beacon.ip, "169.254.111.126");
        assert_eq!(beacon.version.as_deref(), Some("2.1.2+3352"));
        assert!(parse_beacon_bytes(b"not json").is_none());
    }
}
