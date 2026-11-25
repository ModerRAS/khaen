//! AES67 daemon 与 Inferno（Dante）通用状态/日志/配置只读展示。

use std::sync::Arc;

use serde::Serialize;

use crate::config::AoipConfig;
use crate::error::{AppError, AppResult};
use crate::service::{service_info, ServiceInfo, ServiceManager};
use crate::util;

const MAX_CONFIG_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AoipKind {
    Aes67,
    Inferno,
}

impl AoipKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Aes67 => "aes67",
            Self::Inferno => "inferno",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Aes67 => "AES67 daemon",
            Self::Inferno => "Inferno（Dante / AoIP）",
        }
    }
    pub fn ptp_hint(self) -> &'static str {
        match self {
            Self::Aes67 => {
                "PTPv2：默认方案 B1，由 aes67 daemon 内置 PTP slave 提供（不启动 ptp4l）"
            }
            Self::Inferno => "PTPv1：由 Inferno 配套的 Statime 提供（statime.service）",
        }
    }
    pub fn notes(self) -> Vec<String> {
        match self {
            Self::Aes67 => vec![
                "AES67 daemon 自带 Web UI（默认端口 8080，本服务默认使用 80，不与它冲突）".into(),
                "方案 B1：aes67 daemon 自带 PTPv2 slave，本机不启动 ptp4l/phc2sys".into(),
                "若切换为方案 B2（本机 ptp4l 做 GM），请改 [mode.aes67] 的 ptp_services/ptp_source".into(),
            ],
            Self::Inferno => vec![
                "Inferno 是 ALSA 插件（libasound_module_pcm_inferno），没有独立 systemd 服务；状态页的 service 指向 khaen-statime-inferno.service（PTPv1 从时钟）".into(),
                "Inferno 上游没有统一配置文件：通过 INFERNO_* 环境变量或 ALSA 插件参数配置，此处仅只读展示".into(),
                "Dante 模式使用 PTPv1，需要 Statime（见 [ptp] statime_service）".into(),
                "Inferno 需要实时权限（rtprio/ulimit -r）与 Dante 时钟同步，详见官方 README".into(),
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct AoipConfigFile {
    pub path: String,
    pub exists: bool,
    pub size: u64,
    pub content: Option<String>,
    pub truncated: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AoipView {
    pub kind: String,
    pub label: String,
    pub service: ServiceInfo,
    pub config: Vec<AoipConfigFile>,
    pub webui_url: String,
    pub ptp_hint: String,
    pub notes: Vec<String>,
    pub mock: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AoipLogs {
    pub kind: String,
    pub unit: String,
    pub lines: usize,
    pub text: String,
}

pub struct AoipBackend {
    pub services: Arc<dyn ServiceManager>,
    pub aes67: AoipConfig,
    pub inferno: AoipConfig,
    pub mock: bool,
}

impl AoipBackend {
    pub fn new(
        services: Arc<dyn ServiceManager>,
        aes67: AoipConfig,
        inferno: AoipConfig,
        mock: bool,
    ) -> Self {
        Self {
            services,
            aes67,
            inferno,
            mock,
        }
    }

    pub fn cfg(&self, kind: AoipKind) -> &AoipConfig {
        match kind {
            AoipKind::Aes67 => &self.aes67,
            AoipKind::Inferno => &self.inferno,
        }
    }

    async fn config_files(&self, kind: AoipKind) -> Vec<AoipConfigFile> {
        let cfg = self.cfg(kind);
        if self.mock {
            return kind.id_placeholder();
        }
        let mut out = Vec::new();
        for path in &cfg.config_files {
            let p = std::path::Path::new(path);
            let meta = tokio::fs::metadata(p).await.ok();
            let mut entry = AoipConfigFile {
                path: path.clone(),
                exists: meta.is_some(),
                size: meta.map(|m| m.len()).unwrap_or(0),
                ..Default::default()
            };
            if entry.exists {
                match util::read_text_limited(p, MAX_CONFIG_BYTES).await {
                    Ok((text, truncated)) => {
                        entry.content = Some(text);
                        entry.truncated = truncated;
                    }
                    Err(e) => entry.error = Some(e.to_string()),
                }
            }
            out.push(entry);
        }
        out
    }

    pub async fn view(&self, kind: AoipKind) -> AoipView {
        let cfg = self.cfg(kind);
        let service = service_info(self.services.as_ref(), &cfg.service).await;
        AoipView {
            kind: kind.as_str().to_string(),
            label: kind.label().to_string(),
            service,
            config: self.config_files(kind).await,
            webui_url: cfg.webui_url.clone(),
            ptp_hint: kind.ptp_hint().to_string(),
            notes: kind.notes(),
            mock: self.mock,
        }
    }

    pub async fn logs(&self, kind: AoipKind, lines: usize) -> AppResult<AoipLogs> {
        let cfg = self.cfg(kind);
        let n = lines.clamp(1, 2000);
        let text =
            self.services.logs(&cfg.service, n).await.map_err(|e| {
                AppError::unavailable(format!("读取 {} 日志失败: {e}", kind.label()))
            })?;
        Ok(AoipLogs {
            kind: kind.as_str().to_string(),
            unit: cfg.service.clone(),
            lines: n,
            text,
        })
    }

    pub async fn start(&self, kind: AoipKind) -> AppResult<ServiceInfo> {
        let cfg = self.cfg(kind);
        self.services
            .start(&cfg.service)
            .await
            .map_err(|e| AppError::unavailable(format!("启动 {} 失败: {e}", kind.label())))?;
        Ok(service_info(self.services.as_ref(), &cfg.service).await)
    }

    pub async fn stop(&self, kind: AoipKind) -> AppResult<ServiceInfo> {
        let cfg = self.cfg(kind);
        self.services
            .stop(&cfg.service)
            .await
            .map_err(|e| AppError::unavailable(format!("停止 {} 失败: {e}", kind.label())))?;
        Ok(service_info(self.services.as_ref(), &cfg.service).await)
    }

    pub async fn restart(&self, kind: AoipKind) -> AppResult<ServiceInfo> {
        let cfg = self.cfg(kind);
        self.services
            .restart(&cfg.service)
            .await
            .map_err(|e| AppError::unavailable(format!("重启 {} 失败: {e}", kind.label())))?;
        Ok(service_info(self.services.as_ref(), &cfg.service).await)
    }
}

impl AoipKind {
    fn id_placeholder(self) -> Vec<AoipConfigFile> {
        match self {
            Self::Aes67 => vec![AoipConfigFile {
                path: "/etc/aes67-daemon/aes67-daemon.json".into(),
                exists: true,
                size: 512,
                content: Some(
                    r#"{
  "interface_name": "eth0",
  "sample_rate": 48000,
  "ptp_domain": 0,
  "ptp_priority1": 128,
  "lkm": true,
  "web_ui_port": 8080
}"#
                    .into(),
                ),
                truncated: false,
                error: None,
            }],
            Self::Inferno => vec![AoipConfigFile {
                path: "/etc/default/inferno".into(),
                exists: true,
                size: 256,
                content: Some(
                    "INFERNO_BIND_IP=eth0
INFERNO_NAME=KHAEN-Dante
INFERNO_RX_CHANNELS=2
INFERNO_TX_CHANNELS=2
INFERNO_RX_LATENCY_NS=10000000
INFERNO_CLOCK_PATH=/dev/ptp0"
                        .into(),
                ),
                truncated: false,
                error: None,
            }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::MockServiceManager;

    fn backend() -> AoipBackend {
        AoipBackend::new(
            Arc::new(MockServiceManager::new()),
            AoipConfig::aes67_default(),
            AoipConfig::inferno_default(),
            true,
        )
    }

    #[tokio::test]
    async fn aes67_view_has_service_and_config() {
        let b = backend();
        let view = b.view(AoipKind::Aes67).await;
        assert_eq!(view.kind, "aes67");
        assert!(view.service.available);
        assert!(!view.config.is_empty());
        assert!(view.config[0]
            .content
            .as_deref()
            .unwrap()
            .contains("sample_rate"));
        assert!(view.mock);
    }

    #[tokio::test]
    async fn inferno_notes_mention_env_vars() {
        let b = backend();
        let view = b.view(AoipKind::Inferno).await;
        assert!(view.notes.iter().any(|n| n.contains("INFERNO_")));
        assert!(view.ptp_hint.contains("PTPv1"));
    }

    #[tokio::test]
    async fn logs_and_lifecycle() {
        let b = backend();
        let logs = b.logs(AoipKind::Aes67, 10).await.unwrap();
        assert!(!logs.text.is_empty());
        let info = b.start(AoipKind::Inferno).await.unwrap();
        assert!(info.active);
        let info = b.stop(AoipKind::Inferno).await.unwrap();
        assert!(!info.active);
    }
}
