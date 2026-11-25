//! systemd 服务管理抽象。
//!
//! 真机通过 \`systemctl\` / \`journalctl\` 操作其他单元；mock 模式为内存状态机，
//! 支持注入启动/重启失败，用于验证模式切换的回滚路径。

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use async_trait::async_trait;
use serde::Serialize;

use crate::cmd::CommandRunner;

#[derive(Debug, Clone, Serialize)]
pub struct UnitStatus {
    pub unit: String,
    pub load_state: String,
    pub active_state: String,
    pub sub_state: String,
    pub description: String,
    pub fragment_path: String,
    pub main_pid: Option<u32>,
    pub active: bool,
}

impl UnitStatus {
    pub fn running(&self) -> bool {
        self.active
    }
}

#[derive(Debug, Clone)]
pub struct ServiceError {
    pub unit: String,
    pub op: &'static str,
    pub detail: String,
}

impl ServiceError {
    pub fn new(unit: impl Into<String>, op: &'static str, detail: impl Into<String>) -> Self {
        Self {
            unit: unit.into(),
            op,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {} 失败: {}", self.unit, self.op, self.detail)
    }
}

impl std::error::Error for ServiceError {}

/// 给前端/接口用的服务状态快照（不可用时不报整体错误）。
#[derive(Debug, Clone, Serialize)]
pub struct ServiceInfo {
    pub unit: String,
    pub available: bool,
    pub active: bool,
    pub status: Option<UnitStatus>,
    pub error: Option<String>,
}

pub async fn service_info(sm: &dyn ServiceManager, unit: &str) -> ServiceInfo {
    match sm.status(unit).await {
        Ok(st) => ServiceInfo {
            unit: unit.to_string(),
            available: true,
            active: st.active,
            status: Some(st),
            error: None,
        },
        Err(e) => ServiceInfo {
            unit: unit.to_string(),
            available: false,
            active: false,
            status: None,
            error: Some(e.to_string()),
        },
    }
}

#[async_trait]
pub trait ServiceManager: Send + Sync {
    async fn status(&self, unit: &str) -> Result<UnitStatus, ServiceError>;
    async fn start(&self, unit: &str) -> Result<(), ServiceError>;
    async fn stop(&self, unit: &str) -> Result<(), ServiceError>;
    async fn restart(&self, unit: &str) -> Result<(), ServiceError>;
    async fn logs(&self, unit: &str, lines: usize) -> Result<String, ServiceError>;
}

/// 真机实现。
pub struct RealServiceManager {
    runner: std::sync::Arc<dyn CommandRunner>,
}

impl RealServiceManager {
    pub fn new(runner: std::sync::Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    async fn simple(
        &self,
        unit: &str,
        op: &'static str,
        args: &[&str],
    ) -> Result<(), ServiceError> {
        let mut full = vec![op];
        full.extend_from_slice(args);
        let out = self
            .runner
            .run("systemctl", &full)
            .await
            .map_err(|e| ServiceError::new(unit, op, e.to_string()))?;
        if out.success {
            Ok(())
        } else {
            Err(ServiceError::new(
                unit,
                op,
                first_line(&out.stderr, &out.stdout),
            ))
        }
    }
}

fn first_line(stderr: &str, stdout: &str) -> String {
    let s = if !stderr.trim().is_empty() {
        stderr
    } else {
        stdout
    };
    s.trim().lines().next().unwrap_or("未知错误").to_string()
}

pub fn parse_systemctl_show(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    map
}

#[async_trait]
impl ServiceManager for RealServiceManager {
    async fn status(&self, unit: &str) -> Result<UnitStatus, ServiceError> {
        let out = self
            .runner
            .run(
                "systemctl",
                &[
                    "show",
                    "-p",
                    "LoadState,ActiveState,SubState,Description,FragmentPath,MainPID",
                    unit,
                ],
            )
            .await
            .map_err(|e| ServiceError::new(unit, "status", e.to_string()))?;
        let map = parse_systemctl_show(&out.stdout);
        if !out.success || map.is_empty() {
            return Err(ServiceError::new(
                unit,
                "status",
                first_line(&out.stderr, &out.stdout),
            ));
        }
        let load_state = map.get("LoadState").cloned().unwrap_or_default();
        if load_state == "not-found" || load_state.is_empty() {
            return Err(ServiceError::new(unit, "status", "单元不存在（not-found）"));
        }
        let active_state = map.get("ActiveState").cloned().unwrap_or_default();
        Ok(UnitStatus {
            unit: unit.to_string(),
            load_state,
            active: active_state == "active",
            active_state,
            sub_state: map.get("SubState").cloned().unwrap_or_default(),
            description: map.get("Description").cloned().unwrap_or_default(),
            fragment_path: map.get("FragmentPath").cloned().unwrap_or_default(),
            main_pid: map
                .get("MainPID")
                .and_then(|v| v.parse::<u32>().ok())
                .filter(|p| *p > 0),
        })
    }

    async fn start(&self, unit: &str) -> Result<(), ServiceError> {
        self.simple(unit, "start", &[unit]).await
    }
    async fn stop(&self, unit: &str) -> Result<(), ServiceError> {
        self.simple(unit, "stop", &[unit]).await
    }
    async fn restart(&self, unit: &str) -> Result<(), ServiceError> {
        self.simple(unit, "restart", &[unit]).await
    }

    async fn logs(&self, unit: &str, lines: usize) -> Result<String, ServiceError> {
        let n = lines.max(1).to_string();
        let out = self
            .runner
            .run(
                "journalctl",
                &["-u", unit, "-n", &n, "--no-pager", "-o", "short-iso"],
            )
            .await
            .map_err(|e| ServiceError::new(unit, "logs", e.to_string()))?;
        if out.success {
            Ok(out.stdout.trim_end().to_string())
        } else {
            Err(ServiceError::new(
                unit,
                "logs",
                first_line(&out.stderr, &out.stdout),
            ))
        }
    }
}

#[derive(Debug, Clone)]
pub struct MockUnit {
    pub load_state: String,
    pub active_state: String,
    pub sub_state: String,
    pub description: String,
}

impl MockUnit {
    pub fn active(description: &str) -> Self {
        Self {
            load_state: "loaded".into(),
            active_state: "active".into(),
            sub_state: "running".into(),
            description: description.into(),
        }
    }
    pub fn inactive(description: &str) -> Self {
        Self {
            load_state: "loaded".into(),
            active_state: "inactive".into(),
            sub_state: "dead".into(),
            description: description.into(),
        }
    }
}

fn default_units() -> HashMap<String, MockUnit> {
    let mut m = HashMap::new();
    m.insert(
        "camilladsp.service".into(),
        MockUnit::active("CamillaDSP 音频 DSP"),
    );
    m.insert(
        "ptp4l.service".into(),
        MockUnit::active("PTP 协议栈 (PTPv2)"),
    );
    m.insert(
        "phc2sys.service".into(),
        MockUnit::active("PHC 系统时钟同步"),
    );
    m.insert(
        "aes67-daemon.service".into(),
        MockUnit::inactive("AES67 Linux daemon"),
    );
    m.insert(
        "inferno.service".into(),
        MockUnit::inactive("Inferno (Dante/AoIP)"),
    );
    m.insert(
        "statime.service".into(),
        MockUnit::inactive("Statime PTPv1 (Dante)"),
    );
    // OEM v2 实际单元（task-15）：服务状态页展示；旧名字保留给既有模式编排测试/兼容。
    m.insert(
        "khaen-camilladsp.service".into(),
        MockUnit::active("khaen-audio: CamillaDSP (DSP engine)"),
    );
    m.insert(
        "khaen-ptp4l-master.service".into(),
        MockUnit::inactive("khaen-audio: ptp4l PTPv2 grandmaster (B2/B3 only)"),
    );
    m.insert(
        "khaen-phc2sys.service".into(),
        MockUnit::inactive("khaen-audio: phc2sys (NIC PHC sync)"),
    );
    m.insert(
        "khaen-statime-inferno.service".into(),
        MockUnit::inactive("khaen-audio: Statime PTPv1 follower (Inferno/Dante)"),
    );
    m.insert(
        "khaen-aes67-daemon.service".into(),
        MockUnit::inactive("khaen-audio: AES67 Linux daemon (PTPv2 slave)"),
    );
    m.insert(
        "khaen-ravenna-lkm.service".into(),
        MockUnit::inactive("khaen-audio: MergingRavennaALSA kernel module"),
    );
    m
}

fn default_logs() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert(
        "camilladsp.service".into(),
        "2026-11-15T09:00:01+08:00 khaen camilladsp[512]: CamillaDSP version 4.1.3
2026-11-15T09:00:01+08:00 khaen camilladsp[512]: Read config file '/etc/camilladsp/config.yml'
2026-11-15T09:00:02+08:00 khaen camilladsp[512]: Capture device: Alsa hw:Loopback,1,0
2026-11-15T09:00:02+08:00 khaen camilladsp[512]: Playback device: Alsa hw:MOTU,0,0
2026-11-15T09:00:02+08:00 khaen camilladsp[512]: Processing started, state Running
2026-11-15T09:03:11+08:00 khaen camilladsp[512]: ALSA playback buffer underrun (xrun), recovering
2026-11-15T09:05:40+08:00 khaen camilladsp[512]: signal range 1.91, state Running"
            .into(),
    );
    // F-04：旧 C 在板端的实际 unit 名；legacy web 默认 read-only 展示它（日志/状态样本保持一致）。
    m.insert(
        "khaen-camilladsp.service".into(),
        "2026-11-15T09:00:01+08:00 khaen camilladsp[512]: CamillaDSP version 4.1.3
2026-11-15T09:00:01+08:00 khaen camilladsp[512]: Read config file '/etc/camilladsp/config.yml'
2026-11-15T09:00:02+08:00 khaen camilladsp[512]: Capture device: Alsa hw:Loopback,1,0
2026-11-15T09:00:02+08:00 khaen camilladsp[512]: Playback device: Alsa hw:MOTU,0,0
2026-11-15T09:00:02+08:00 khaen camilladsp[512]: Processing started, state Running
2026-11-15T09:03:11+08:00 khaen camilladsp[512]: ALSA playback buffer underrun (xrun), recovering
2026-11-15T09:05:40+08:00 khaen camilladsp[512]: signal range 1.91, state Running"
            .into(),
    );
    m.insert(
        "ptp4l.service".into(),
        "2026-11-15T09:00:00+08:00 khaen ptp4l[401]: selected /dev/ptp0 as PTP clock
2026-11-15T09:00:00+08:00 khaen ptp4l[401]: port 1: INITIALIZING to LISTENING on INIT_COMPLETE
2026-11-15T09:00:01+08:00 khaen ptp4l[401]: port 1: LISTENING to SLAVE on MASTER_CLOCK_SELECTED
2026-11-15T09:00:01+08:00 khaen ptp4l[401]: master offset  -37 s2 freq +1234 path delay  512"
            .into(),
    );
    m.insert(
        "phc2sys.service".into(),
        "2026-11-15T09:00:01+08:00 khaen phc2sys[420]: CLOCK_REALTIME phc offset  -12 s2 freq  -512 delay  502"
            .into(),
    );
    m.insert(
        "aes67-daemon.service".into(),
        "2026-11-15T08:58:00+08:00 khaen systemd[1]: Starting AES67 daemon...
2026-11-15T08:58:00+08:00 khaen aes67-daemon[700]: AES67 daemon starting, web UI on :8080
2026-11-15T08:58:00+08:00 khaen systemd[1]: Started AES67 daemon."
            .into(),
    );
    m.insert(
        "inferno.service".into(),
        "2026-11-15T08:57:00+08:00 khaen systemd[1]: Starting Inferno (Dante/AoIP)...
2026-11-15T08:57:01+08:00 khaen inferno_aoip[720]: device ID 00000a00020f0001, process ID 1
2026-11-15T08:57:01+08:00 khaen systemd[1]: Started Inferno (Dante/AoIP)."
            .into(),
    );
    m.insert(
        "statime.service".into(),
        "2026-11-15T08:57:00+08:00 khaen systemd[1]: Starting Statime (PTPv1)...
2026-11-15T08:57:01+08:00 khaen statime[710]: configured domain 0, slave only
2026-11-15T08:57:02+08:00 khaen statime[710]: clock synchronized, offset 42ns"
            .into(),
    );
    m
}

/// mock 服务管理器：内存状态机 + 失败注入。
pub struct MockServiceManager {
    units: Mutex<HashMap<String, MockUnit>>,
    logs: Mutex<HashMap<String, String>>,
    calls: Mutex<Vec<String>>,
    fail_start: HashSet<String>,
    fail_stop: HashSet<String>,
    fail_restart: HashSet<String>,
}

impl MockServiceManager {
    pub fn new() -> Self {
        Self::with_failures(Vec::new(), Vec::new(), Vec::new())
    }

    pub fn with_failures(
        fail_start: Vec<String>,
        fail_stop: Vec<String>,
        fail_restart: Vec<String>,
    ) -> Self {
        Self {
            units: Mutex::new(default_units()),
            logs: Mutex::new(default_logs()),
            calls: Mutex::new(Vec::new()),
            fail_start: fail_start.into_iter().collect(),
            fail_stop: fail_stop.into_iter().collect(),
            fail_restart: fail_restart.into_iter().collect(),
        }
    }

    /// 从 KHAEN_MOCK_FAIL_START / KHAEN_MOCK_FAIL_RESTART 注入失败（演示回滚路径用）。
    pub fn from_env() -> Self {
        let split = |name: &str| -> Vec<String> {
            std::env::var(name)
                .ok()
                .map(|v| {
                    v.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default()
        };
        Self::with_failures(
            split("KHAEN_MOCK_FAIL_START"),
            split("KHAEN_MOCK_FAIL_STOP"),
            split("KHAEN_MOCK_FAIL_RESTART"),
        )
    }

    pub fn record_call(&self, call: impl Into<String>) {
        self.calls.lock().unwrap().push(call.into());
    }
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    pub fn is_active(&self, unit: &str) -> bool {
        self.units
            .lock()
            .unwrap()
            .get(unit)
            .map(|u| u.active_state == "active")
            .unwrap_or(false)
    }
    pub fn set_active(&self, unit: &str, active: bool) {
        if let Some(u) = self.units.lock().unwrap().get_mut(unit) {
            if active {
                u.active_state = "active".into();
                u.sub_state = "running".into();
            } else {
                u.active_state = "inactive".into();
                u.sub_state = "dead".into();
            }
        }
    }
    pub fn add_unit(&self, unit: &str, active: bool, description: &str) {
        self.units.lock().unwrap().insert(
            unit.to_string(),
            if active {
                MockUnit::active(description)
            } else {
                MockUnit::inactive(description)
            },
        );
    }

    fn op(&self, unit: &str, op: &'static str, active: bool) -> Result<(), ServiceError> {
        self.record_call(format!("{op}:{unit}"));
        let fail = match op {
            "start" => self.fail_start.contains(unit),
            "stop" => self.fail_stop.contains(unit),
            _ => self.fail_restart.contains(unit),
        };
        if fail {
            return Err(ServiceError::new(
                unit,
                op,
                format!("mock 注入的 {op} 失败（用于验证回滚）"),
            ));
        }
        let mut units = self.units.lock().unwrap();
        let Some(u) = units.get_mut(unit) else {
            return Err(ServiceError::new(unit, op, "单元不存在（mock）"));
        };
        if active {
            u.active_state = "active".into();
            u.sub_state = "running".into();
        } else {
            u.active_state = "inactive".into();
            u.sub_state = "dead".into();
        }
        Ok(())
    }
}

impl Default for MockServiceManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ServiceManager for MockServiceManager {
    async fn status(&self, unit: &str) -> Result<UnitStatus, ServiceError> {
        let units = self.units.lock().unwrap();
        let u = units
            .get(unit)
            .ok_or_else(|| ServiceError::new(unit, "status", "单元不存在（mock）"))?;
        Ok(UnitStatus {
            unit: unit.to_string(),
            load_state: u.load_state.clone(),
            active: u.active_state == "active",
            active_state: u.active_state.clone(),
            sub_state: u.sub_state.clone(),
            description: u.description.clone(),
            fragment_path: format!("/etc/systemd/system/{unit}"),
            main_pid: if u.active_state == "active" {
                Some(1000)
            } else {
                None
            },
        })
    }

    async fn start(&self, unit: &str) -> Result<(), ServiceError> {
        self.op(unit, "start", true)
    }
    async fn stop(&self, unit: &str) -> Result<(), ServiceError> {
        self.op(unit, "stop", false)
    }
    async fn restart(&self, unit: &str) -> Result<(), ServiceError> {
        self.record_call(format!("restart:{unit}"));
        if self.fail_restart.contains(unit) {
            return Err(ServiceError::new(
                unit,
                "restart",
                "mock 注入的 restart 失败（用于验证回滚）",
            ));
        }
        self.op(unit, "restart", true)
    }

    async fn logs(&self, unit: &str, lines: usize) -> Result<String, ServiceError> {
        let logs = self.logs.lock().unwrap();
        let text = logs
            .get(unit)
            .ok_or_else(|| ServiceError::new(unit, "logs", "单元不存在（mock）"))?;
        let all: Vec<&str> = text.lines().collect();
        let start = all.len().saturating_sub(lines.max(1));
        Ok(all[start..].join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_systemctl_show() {
        let text = "LoadState=loaded\nActiveState=active\nSubState=running\nDescription=CamillaDSP 音频 DSP\nFragmentPath=/lib/systemd/system/camilladsp.service\nMainPID=512\n";
        let map = parse_systemctl_show(text);
        assert_eq!(map["LoadState"], "loaded");
        assert_eq!(map["MainPID"], "512");
        assert_eq!(map["Description"], "CamillaDSP 音频 DSP");
    }

    #[tokio::test]
    async fn mock_service_lifecycle() {
        let sm = MockServiceManager::new();
        assert!(!sm.is_active("inferno.service"));
        sm.start("inferno.service").await.unwrap();
        assert!(sm.is_active("inferno.service"));
        let st = sm.status("inferno.service").await.unwrap();
        assert!(st.active);
        sm.stop("inferno.service").await.unwrap();
        assert!(!sm.is_active("inferno.service"));
        assert!(sm.status("nope.service").await.is_err());
    }

    #[tokio::test]
    async fn mock_failure_injection() {
        let sm = MockServiceManager::with_failures(vec!["statime.service".into()], vec![], vec![]);
        assert!(sm.start("statime.service").await.is_err());
        assert!(!sm.is_active("statime.service"));
    }

    #[tokio::test]
    async fn mock_logs_tail() {
        let sm = MockServiceManager::new();
        let logs = sm.logs("camilladsp.service", 2).await.unwrap();
        assert_eq!(logs.lines().count(), 2);
        assert!(logs.contains("state Running"));
    }
}
