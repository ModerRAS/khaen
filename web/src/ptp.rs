//! PTP 状态与配置。
//!
//! 两种 PTP 版本不同时存在：
//! - AES67 方案 B1（默认）：aes67 daemon 自带 PTPv2 slave，本机不跑 ptp4l，
//!   锁定状态从 aes67 daemon 服务日志读取（无公开状态接口，按关键词推断并在页面注明）；
//! - AES67 方案 B2 / 纯 PTP 页面：linuxptp 的 ptp4l，状态用
//!   \`pmc -u -b 0 "GET TIME_STATUS_NP"\` / \`"GET PORT_DATA_SET"\`（linuxptp 官方命令）；
//! - Dante：Inferno 配套 Statime（PTPv1），锁定状态从 statime 服务日志推断。
//!
//! 配置修改只针对 linuxptp（/etc/ptp4l.conf 的 masterOnly / priority1），先备份再重启。

use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;

use crate::cmd::CommandRunner;
use crate::config::PtpConfig;
use crate::error::{AppError, AppResult};
use crate::service::ServiceManager;
use crate::util;

#[derive(Debug, Clone, Serialize, Default)]
pub struct PtpStatus {
    pub ptp_source: String,
    pub ptp_role: String,
    pub ptp_version: String,
    pub locked: bool,
    pub lock_detail: String,
    pub offset_ns: Option<i64>,
    pub gm_present: Option<bool>,
    pub gm_identity: Option<String>,
    pub port_state: Option<String>,
    pub port_identity: Option<String>,
    pub steps_removed: Option<i64>,
    pub clock_class: Option<u8>,
    pub raw_time_status: String,
    pub raw_port_data_set: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PtpConfView {
    pub path: String,
    pub exists: bool,
    pub master_only: Option<bool>,
    pub priority1: Option<u8>,
    pub raw: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PtpApplyOutcome {
    pub backup: Option<String>,
    pub conf: PtpConfView,
    pub restarted: Vec<String>,
    pub message: String,
}

#[async_trait]
pub trait PtpBackend: Send + Sync {
    /// source: linuxptp / statime / aes67-daemon；aoip_service 供 statime/aes67 日志定位。
    async fn status(&self, source: &str, aoip_service: Option<&str>) -> PtpStatus;
    async fn get_conf(&self) -> PtpConfView;
    async fn apply_conf(
        &self,
        master_only: Option<bool>,
        priority1: Option<u8>,
    ) -> AppResult<PtpApplyOutcome>;
}

/// 解析 pmc 输出里的 key: value。
pub fn parse_pmc(text: &str) -> PtpStatus {
    let mut st = PtpStatus::default();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("sending:") {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            match key {
                "offsetFromMaster" | "master_offset" => st.offset_ns = value.parse().ok(),
                "gmPresent" => st.gm_present = value.parse().ok(),
                "gmIdentity" => st.gm_identity = Some(value.to_string()),
                "portState" => st.port_state = Some(value.to_string()),
                "portIdentity" => st.port_identity = Some(value.to_string()),
                "stepsRemoved" => st.steps_removed = value.parse().ok(),
                "clockClass" => st.clock_class = value.parse().ok(),
                _ => {}
            }
        } else if st.port_identity.is_none() && looks_like_identity(trimmed) {
            st.port_identity = Some(trimmed.to_string());
        }
    }
    st
}

fn looks_like_identity(s: &str) -> bool {
    // 6c1f4a.fffe.0a1234-0 这类 portIdentity
    !s.contains(':') && s.contains('.') && s.chars().any(|c| c.is_ascii_hexdigit())
}

/// 依据 portState / gmPresent / offset 判断是否锁定。
pub fn derive_lock(
    role: &str,
    port_state: Option<&str>,
    gm_present: Option<bool>,
    offset_ns: Option<i64>,
    threshold_ns: i64,
) -> (bool, String) {
    let state = port_state.unwrap_or("");
    if role.eq_ignore_ascii_case("master") {
        if state == "MASTER" {
            return (true, "ptp4l 处于 MASTER（本机作为 PTP 主时钟）".into());
        }
        return (false, format!("等待进入 MASTER（当前 portState={state}）"));
    }
    if state != "SLAVE" {
        return (false, format!("端口未从属（portState={state}）"));
    }
    if !gm_present.unwrap_or(false) {
        return (false, "已进入 SLAVE，但尚未收到 Grandmaster 通告".into());
    }
    if let Some(offset) = offset_ns {
        if offset.abs() > threshold_ns {
            return (
                false,
                format!("offset={offset} ns 超出锁定阈值 {threshold_ns} ns"),
            );
        }
        return (true, format!("已锁定：portState=SLAVE, offset={offset} ns"));
    }
    (true, "已锁定：portState=SLAVE".into())
}

/// 解析 ptp4l.conf 的 masterOnly / priority1（只看 [global]）。
pub fn parse_ptp_conf(text: &str) -> (Option<bool>, Option<u8>) {
    let mut section = String::from("global");
    let mut master_only = None;
    let mut priority1 = None;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with(';') {
            continue;
        }
        if t.starts_with('[') && t.ends_with(']') {
            section = t[1..t.len() - 1].trim().to_string();
            continue;
        }
        if section != "global" {
            continue;
        }
        let mut parts = t.split_whitespace();
        let Some(key) = parts.next() else { continue };
        let val = parts.next().unwrap_or("");
        match key {
            "masterOnly" => {
                master_only = match val {
                    "1" | "true" | "yes" | "on" => Some(true),
                    "0" | "false" | "no" | "off" => Some(false),
                    _ => None,
                }
            }
            "priority1" => priority1 = val.parse::<u8>().ok(),
            _ => {}
        }
    }
    (master_only, priority1)
}

/// 修改 ptp4l.conf 的 masterOnly / priority1，保留其余内容与注释。
pub fn patch_ptp_conf(text: &str, master_only: Option<bool>, priority1: Option<u8>) -> String {
    let global = "global".to_string();
    let mut section = global.clone();
    let mut out: Vec<String> = Vec::new();
    let mut set_master = false;
    let mut set_priority = false;
    let mut global_header_index: Option<usize> = None;

    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            section = t[1..t.len() - 1].trim().to_string();
            if section == global && global_header_index.is_none() {
                global_header_index = Some(out.len());
            }
        }
        let mut replaced = false;
        if section == global && !t.starts_with('#') && !t.starts_with(';') {
            let mut parts = t.split_whitespace();
            let key = parts.next().unwrap_or("");
            if key == "masterOnly" && !set_master {
                if let Some(v) = master_only {
                    out.push(format!("masterOnly {}", if v { 1 } else { 0 }));
                    set_master = true;
                    replaced = true;
                }
            } else if key == "priority1" && !set_priority {
                if let Some(v) = priority1 {
                    out.push(format!("priority1 {v}"));
                    set_priority = true;
                    replaced = true;
                }
            }
        }
        if !replaced {
            out.push(line.to_string());
        }
    }

    // 未找到的字段补进 [global] 段
    let mut extra = Vec::new();
    if !set_master {
        if let Some(v) = master_only {
            extra.push(format!("masterOnly {}", if v { 1 } else { 0 }));
        }
    }
    if !set_priority {
        if let Some(v) = priority1 {
            extra.push(format!("priority1 {v}"));
        }
    }
    if !extra.is_empty() {
        match global_header_index {
            Some(idx) => {
                let insert_at = idx + 1;
                for (offset, line) in extra.into_iter().enumerate() {
                    out.insert(insert_at + offset, line);
                }
            }
            None => {
                let mut prefixed = vec!["[global]".to_string()];
                prefixed.extend(extra);
                prefixed.extend(out);
                out = prefixed;
            }
        }
    }
    let mut text = out.join("\n");
    text.push('\n');
    text
}

pub struct RealPtp {
    cfg: PtpConfig,
    runner: Arc<dyn CommandRunner>,
    services: Arc<dyn ServiceManager>,
}

impl RealPtp {
    pub fn new(
        cfg: PtpConfig,
        runner: Arc<dyn CommandRunner>,
        services: Arc<dyn ServiceManager>,
    ) -> Self {
        Self {
            cfg,
            runner,
            services,
        }
    }

    async fn pmc(&self, command: &str) -> Result<String, String> {
        let out = self
            .runner
            .run_with_timeout(
                &self.cfg.pmc_bin,
                &["-u", "-b", "0", command],
                std::time::Duration::from_secs(5),
            )
            .await
            .map_err(|e| format!("执行 pmc 失败: {e}"))?;
        if !out.success && out.stdout.trim().is_empty() {
            return Err(format!(
                "pmc {command} 失败: {}",
                out.stderr.trim().lines().next().unwrap_or("无输出")
            ));
        }
        Ok(out.stdout)
    }

    async fn service_log_hint(&self, unit: &str) -> Option<String> {
        let logs = self.services.logs(unit, 60).await.ok()?;
        let mut last = None;
        for line in logs.lines() {
            let l = line.to_ascii_lowercase();
            if l.contains("locked")
                || l.contains("synchronized")
                || l.contains("sync")
                || l.contains("slave")
                || l.contains("master")
                || l.contains("ptp")
            {
                last = Some(line.trim().to_string());
            }
        }
        last
    }

    async fn heuristic(&self, source: &str, unit: &str, role: &str) -> PtpStatus {
        let mut st = PtpStatus {
            ptp_source: source.to_string(),
            ptp_role: role.to_string(),
            ..Default::default()
        };
        match self.services.status(unit).await {
            Ok(status) => {
                if !status.active {
                    st.lock_detail = format!("{unit} 未运行，无法提供 PTP");
                    st.error = Some(format!("服务 {unit} 未运行"));
                    return st;
                }
                let hint = self.service_log_hint(unit).await;
                st.locked = hint.is_some();
                st.lock_detail = match &hint {
                    Some(line) => format!("{unit} 运行中；日志线索: {line}"),
                    None => format!("{unit} 运行中，但日志中未发现锁定线索（无公开状态接口）"),
                };
                if let Some(line) = hint {
                    st.raw_time_status = line;
                }
            }
            Err(e) => {
                st.error = Some(e.to_string());
                st.lock_detail = format!("无法读取 {unit} 状态");
            }
        }
        st
    }
}

#[async_trait]
impl PtpBackend for RealPtp {
    async fn status(&self, source: &str, aoip_service: Option<&str>) -> PtpStatus {
        match source {
            "linuxptp" => {
                let mut st = PtpStatus {
                    ptp_source: "linuxptp".into(),
                    ptp_role: "slave".into(),
                    ptp_version: "PTPv2".into(),
                    ..Default::default()
                };
                match self.pmc("GET TIME_STATUS_NP").await {
                    Ok(text) => {
                        let mut parsed = parse_pmc(&text);
                        st.offset_ns = parsed.offset_ns;
                        st.gm_present = parsed.gm_present;
                        st.gm_identity = parsed.gm_identity.take();
                        st.steps_removed = parsed.steps_removed;
                        st.clock_class = parsed.clock_class;
                        st.port_identity = parsed.port_identity.take();
                        st.raw_time_status = text.trim().chars().take(4000).collect();
                    }
                    Err(e) => {
                        st.error = Some(e);
                    }
                }
                if let Ok(text) = self.pmc("GET PORT_DATA_SET").await {
                    let parsed = parse_pmc(&text);
                    st.port_state = parsed.port_state;
                    if st.port_identity.is_none() {
                        st.port_identity = parsed.port_identity;
                    }
                    st.raw_port_data_set = text.trim().chars().take(4000).collect();
                }
                let role = if st.raw_time_status.to_ascii_uppercase().contains("MASTER")
                    && st.port_state.as_deref() == Some("MASTER")
                {
                    "master"
                } else {
                    "slave"
                };
                st.ptp_role = role.to_string();
                let (locked, detail) = derive_lock(
                    role,
                    st.port_state.as_deref(),
                    st.gm_present,
                    st.offset_ns,
                    self.cfg.lock_offset_threshold_ns,
                );
                st.locked = locked;
                st.lock_detail = detail;
                st
            }
            "statime" => {
                self.heuristic("statime", &self.cfg.statime_service, "slave")
                    .await
            }
            "aes67-daemon" => {
                let unit = aoip_service.unwrap_or("aes67-daemon.service");
                let mut st = self.heuristic("aes67-daemon", unit, "slave").await;
                st.ptp_version = "PTPv2".into();
                st.lock_detail
                    .push_str("（aes67 daemon 内置 PTP slave，方案 B1 不启动 ptp4l）");
                st
            }
            other => PtpStatus {
                ptp_source: other.to_string(),
                error: Some(format!("未知 PTP 来源: {other}")),
                lock_detail: "未知 PTP 来源".into(),
                ..Default::default()
            },
        }
    }

    async fn get_conf(&self) -> PtpConfView {
        let path = std::path::PathBuf::from(&self.cfg.conf_file);
        let mut view = PtpConfView {
            path: path.display().to_string(),
            ..Default::default()
        };
        match util::read_text_limited(&path, 256 * 1024).await {
            Ok((text, truncated)) => {
                view.exists = true;
                let (m, p) = parse_ptp_conf(&text);
                view.master_only = m;
                view.priority1 = p;
                view.raw = if truncated {
                    format!("{}\n…（文件过大，已截断）", text)
                } else {
                    text
                };
            }
            Err(e) => {
                view.error = Some(format!("读取 {} 失败: {e}", path.display()));
            }
        }
        view
    }

    async fn apply_conf(
        &self,
        master_only: Option<bool>,
        priority1: Option<u8>,
    ) -> AppResult<PtpApplyOutcome> {
        if master_only.is_none() && priority1.is_none() {
            return Err(AppError::bad_request("至少提供 masterOnly 或 priority1"));
        }
        let path = std::path::PathBuf::from(&self.cfg.conf_file);
        let (text, _) = util::read_text_limited(&path, 256 * 1024)
            .await
            .map_err(|e| AppError::not_found(format!("读取 {} 失败: {e}", path.display())))?;
        let (old_m, old_p) = parse_ptp_conf(&text);
        let new_text = patch_ptp_conf(&text, master_only, priority1);
        // 校验结果
        let (new_m, new_p) = parse_ptp_conf(&new_text);
        if master_only.is_some() && new_m != master_only {
            return Err(AppError::internal(
                "修改 masterOnly 失败（写入后校验不一致）",
            ));
        }
        if priority1.is_some() && new_p != priority1 {
            return Err(AppError::internal(
                "修改 priority1 失败（写入后校验不一致）",
            ));
        }
        let backup = util::backup_file(&path)
            .await
            .map_err(|e| AppError::internal(format!("备份 {} 失败: {e}", path.display())))?
            .map(|p| p.display().to_string());
        util::write_atomic(&path, &new_text)
            .await
            .map_err(|e| AppError::internal(format!("写入 {} 失败: {e}", path.display())))?;
        util::prune_backups(&path, 10).await;

        // 重启 linuxptp 服务；失败则恢复备份
        let mut restarted = Vec::new();
        for unit in [&self.cfg.ptp4l_service, &self.cfg.phc2sys_service] {
            if unit.trim().is_empty() {
                continue;
            }
            match self.services.restart(unit).await {
                Ok(()) => restarted.push(unit.clone()),
                Err(e) => {
                    let rollback = match &backup {
                        Some(b) => tokio::fs::copy(b, &path).await.is_ok(),
                        None => false,
                    };
                    let _ = self.services.restart(&self.cfg.ptp4l_service).await;
                    return Err(AppError::unavailable(format!(
                        "重启 {unit} 失败: {e}；配置{}",
                        if rollback {
                            "已从备份恢复"
                        } else {
                            "备份恢复失败，请手动检查"
                        }
                    )));
                }
            }
        }
        let conf = self.get_conf().await;
        let mut message = format!("已写入 {} 并重启 {}", path.display(), restarted.join(", "));
        if let Some(b) = &backup {
            message.push_str(&format!("；备份: {b}"));
        }
        if let (Some(old_m), Some(old_p), Some(_), Some(_)) = (old_m, old_p, master_only, priority1)
        {
            if old_m != new_m.unwrap_or(old_m) || old_p != new_p.unwrap_or(old_p) {
                message.push_str("；注意：主/从切换会影响 AES67（B1 不依赖 ptp4l）与 Dante 模式");
            }
        }
        Ok(PtpApplyOutcome {
            backup,
            conf,
            restarted,
            message,
        })
    }
}

/// mock：返回固定的 PTP 锁定状态，配置保存在内存。
pub struct MockPtp {
    conf: Mutex<PtpMockInner>,
}

struct PtpMockInner {
    path: String,
    text: String,
    backups: Vec<String>,
    fail_restart: bool,
}

impl Default for MockPtp {
    fn default() -> Self {
        Self::new("/etc/ptp4l.conf")
    }
}

impl MockPtp {
    pub fn new(path: &str) -> Self {
        let text = "[global]
domainNumber 0
delay_mechanism E2E
masterOnly 0
priority1 128
priority2 128
clockClass 248
logAnnounceInterval 0
logSyncInterval -2
logMinDelReqInterval 0
";
        Self {
            conf: Mutex::new(PtpMockInner {
                path: path.to_string(),
                text: text.to_string(),
                backups: Vec::new(),
                fail_restart: false,
            }),
        }
    }
    pub fn fail_restart(&self, fail: bool) {
        self.conf.lock().unwrap().fail_restart = fail;
    }
    pub fn conf_text(&self) -> String {
        self.conf.lock().unwrap().text.clone()
    }
}

use std::sync::Mutex;

#[async_trait]
impl PtpBackend for MockPtp {
    async fn status(&self, source: &str, _aoip_service: Option<&str>) -> PtpStatus {
        let (version, source_label) = match source {
            "statime" => ("PTPv1", "statime"),
            "aes67-daemon" => ("PTPv2", "aes67-daemon"),
            _ => ("PTPv2", "linuxptp"),
        };
        PtpStatus {
            ptp_source: source_label.into(),
            ptp_role: "slave".into(),
            ptp_version: version.into(),
            locked: true,
            lock_detail: format!("mock：{version} 已锁定（offset -37 ns）"),
            offset_ns: Some(-37),
            gm_present: Some(true),
            gm_identity: Some("6c1f4a.fffe.0a1234".into()),
            port_state: Some("SLAVE".into()),
            port_identity: Some("6c1f4a.fffe.0a1234-1".into()),
            steps_removed: Some(1),
            clock_class: Some(248),
            raw_time_status: "mock TIME_STATUS_NP: offsetFromMaster -37, gmPresent true".into(),
            raw_port_data_set: "mock PORT_DATA_SET: portState SLAVE".into(),
            error: None,
        }
    }

    async fn get_conf(&self) -> PtpConfView {
        let inner = self.conf.lock().unwrap();
        let (m, p) = parse_ptp_conf(&inner.text);
        PtpConfView {
            path: inner.path.clone(),
            exists: true,
            master_only: m,
            priority1: p,
            raw: inner.text.clone(),
            error: None,
        }
    }

    async fn apply_conf(
        &self,
        master_only: Option<bool>,
        priority1: Option<u8>,
    ) -> AppResult<PtpApplyOutcome> {
        if master_only.is_none() && priority1.is_none() {
            return Err(AppError::bad_request("至少提供 masterOnly 或 priority1"));
        }
        let mut inner = self.conf.lock().unwrap();
        if inner.fail_restart {
            return Err(AppError::unavailable(
                "mock 注入：PTP 服务重启失败（配置已回滚）",
            ));
        }
        let backup = format!("{}.bak.mock{}", inner.path, util::now_unix());
        inner.backups.push(backup.clone());
        inner.text = patch_ptp_conf(&inner.text, master_only, priority1);
        let (m, p) = parse_ptp_conf(&inner.text);
        Ok(PtpApplyOutcome {
            backup: Some(backup),
            conf: PtpConfView {
                path: inner.path.clone(),
                exists: true,
                master_only: m,
                priority1: p,
                raw: inner.text.clone(),
                error: None,
            },
            restarted: vec!["ptp4l.service".into(), "phc2sys.service".into()],
            message: "mock：已更新 PTP 配置并模拟重启".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::MockServiceManager;

    const TIME_STATUS: &str = "sending: GET TIME_STATUS_NP
\t6c1f4a.fffe.0a1234-1
\t\tgmPresent: true
\t\tgmIdentity: 6c1f4a.fffe.0a1234
\t\toffsetFromMaster: -37
\t\tstepsRemoved: 1
\t\tclockClass: 248
";

    const PORT_DATA: &str = "sending: GET PORT_DATA_SET
\t6c1f4a.fffe.0a1234-1
\t\tportIdentity: 6c1f4a.fffe.0a1234-1
\t\tportState: SLAVE
";

    #[test]
    fn pmc_parse() {
        let st = parse_pmc(TIME_STATUS);
        assert_eq!(st.gm_present, Some(true));
        assert_eq!(st.offset_ns, Some(-37));
        assert_eq!(st.gm_identity.as_deref(), Some("6c1f4a.fffe.0a1234"));
        assert_eq!(st.steps_removed, Some(1));
        assert_eq!(st.clock_class, Some(248));
        assert_eq!(st.port_identity.as_deref(), Some("6c1f4a.fffe.0a1234-1"));
        let port = parse_pmc(PORT_DATA);
        assert_eq!(port.port_state.as_deref(), Some("SLAVE"));
    }

    #[test]
    fn lock_derivation() {
        let (locked, detail) =
            derive_lock("slave", Some("SLAVE"), Some(true), Some(-37), 1_000_000);
        assert!(locked);
        assert!(detail.contains("offset"));
        let (locked, _) = derive_lock(
            "slave",
            Some("SLAVE"),
            Some(true),
            Some(5_000_000),
            1_000_000,
        );
        assert!(!locked);
        let (locked, _) = derive_lock("slave", Some("LISTENING"), Some(false), None, 1_000_000);
        assert!(!locked);
        let (locked, _) = derive_lock("master", Some("MASTER"), None, None, 1_000_000);
        assert!(locked);
        let (locked, _) = derive_lock("master", Some("SLAVE"), None, None, 1_000_000);
        assert!(!locked);
    }

    #[test]
    fn ptp_conf_parse_and_patch() {
        let text = "[global]
# comment
masterOnly 0
priority1 128
priority2 128

[eth0]
masterOnly 1
";
        let (m, p) = parse_ptp_conf(text);
        assert_eq!(m, Some(false));
        assert_eq!(p, Some(128));
        let patched = patch_ptp_conf(text, Some(true), Some(42));
        let (m2, p2) = parse_ptp_conf(&patched);
        assert_eq!(m2, Some(true));
        assert_eq!(p2, Some(42));
        assert!(patched.contains("# comment"));
        // [eth0] 段里的同名项不应被改
        assert!(patched.contains("[eth0]\nmasterOnly 1"));
    }

    #[test]
    fn ptp_conf_patch_inserts_missing() {
        let text = "[global]\ndomainNumber 0\n";
        let patched = patch_ptp_conf(text, Some(true), Some(7));
        let (m, p) = parse_ptp_conf(&patched);
        assert_eq!(m, Some(true));
        assert_eq!(p, Some(7));
        // 无 [global] 时自动补
        let patched2 = patch_ptp_conf("domainNumber 0\n", Some(false), None);
        let (m2, _) = parse_ptp_conf(&patched2);
        assert_eq!(m2, Some(false));
    }

    #[tokio::test]
    async fn real_ptp_apply_conf_with_mock_services() {
        let dir = crate::util::test_tempdir();
        let conf_path = dir.path().join("ptp4l.conf");
        tokio::fs::write(&conf_path, "[global]\nmasterOnly 0\npriority1 128\n")
            .await
            .unwrap();
        let cfg = PtpConfig {
            conf_file: conf_path.display().to_string(),
            ..PtpConfig::default()
        };
        let services = Arc::new(MockServiceManager::new());
        let runner = Arc::new(crate::cmd::MockCommandRunner);
        let ptp = RealPtp::new(cfg, runner, services);
        let outcome = ptp.apply_conf(Some(true), Some(10)).await.unwrap();
        assert!(outcome.backup.is_some());
        let text = tokio::fs::read_to_string(&conf_path).await.unwrap();
        let (m, p) = parse_ptp_conf(&text);
        assert_eq!(m, Some(true));
        assert_eq!(p, Some(10));
        assert_eq!(outcome.restarted.len(), 2);
    }

    #[tokio::test]
    async fn real_ptp_apply_conf_rolls_back_on_restart_failure() {
        let dir = crate::util::test_tempdir();
        let conf_path = dir.path().join("ptp4l.conf");
        tokio::fs::write(&conf_path, "[global]\nmasterOnly 0\npriority1 128\n")
            .await
            .unwrap();
        let cfg = PtpConfig {
            conf_file: conf_path.display().to_string(),
            ..PtpConfig::default()
        };
        let services = Arc::new(MockServiceManager::with_failures(
            vec![],
            vec![],
            vec!["ptp4l.service".into()],
        ));
        let ptp = RealPtp::new(cfg, Arc::new(crate::cmd::MockCommandRunner), services);
        let err = ptp.apply_conf(Some(true), None).await.unwrap_err();
        assert!(matches!(err, AppError::ServiceUnavailable(_)));
        let text = tokio::fs::read_to_string(&conf_path).await.unwrap();
        let (m, _) = parse_ptp_conf(&text);
        assert_eq!(m, Some(false), "失败后应恢复原配置");
    }

    #[tokio::test]
    async fn mock_ptp_status_and_conf() {
        let ptp = MockPtp::default();
        let st = ptp.status("aes67-daemon", None).await;
        assert!(st.locked);
        assert_eq!(st.ptp_version, "PTPv2");
        let st2 = ptp.status("statime", None).await;
        assert_eq!(st2.ptp_version, "PTPv1");
        let view = ptp.get_conf().await;
        assert_eq!(view.master_only, Some(false));
        let out = ptp.apply_conf(Some(true), Some(3)).await.unwrap();
        assert!(out.backup.is_some());
        assert!(ptp.conf_text().contains("masterOnly 1"));
    }
}
