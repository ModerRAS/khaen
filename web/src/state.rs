//! 应用状态：真机后端或 mock 后端装配。

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::alsa::{AlsaBackend, MockAlsa, RealAlsa};
use crate::aoip::AoipBackend;
use crate::auth::AuthState;
use crate::camilla::{CamillaBackend, MockCamilla, RealCamilla};
use crate::channelmap::ChannelMapService;
use crate::cmd::{CommandRunner, MockCommandRunner, RealCommandRunner};
use crate::config::AppConfig;
use crate::mode::{FileModeStore, MockModeStore, ModeController, ModeStore, PersistedMode};
use crate::motu::{MockMotu, MotuBackend, RealMotu};
use crate::power::{MockPower, PowerBackend, RealPower};
use crate::ptp::{MockPtp, PtpBackend, RealPtp};
use crate::service::{MockServiceManager, RealServiceManager, ServiceManager};
use crate::system::{MockSystemBackend, RealSystemBackend, SystemBackend};
use crate::util;

/// mock 装配选项（测试可精确控制失败注入与切换速度）。
#[derive(Debug, Clone, Default)]
pub struct MockOptions {
    pub fail_start: Vec<String>,
    pub fail_restart: Vec<String>,
    pub camilla_apply_fails: bool,
    pub switch_step_delay_ms: u64,
    pub persisted_mode: Option<crate::config::RunMode>,
    /// mock channelmap 预置布局（空 = 未配置，走首启向导）。
    pub channelmap_preset: Option<String>,
    pub channelmap_apply_fails: bool,
    pub channelmap_preload: bool,
    /// TSPI_MOCK_MOTU_APPLY_FAILS=1 时 CueMix 写入不落地并返回回滚结果。
    pub motu_apply_fails: bool,
}

impl MockOptions {
    /// 从环境变量读取（用于浏览器演示失败/回滚路径）：
    /// - TSPI_MOCK_FAIL_START=inferno.service,statime.service
    /// - TSPI_MOCK_FAIL_RESTART=...
    /// - TSPI_MOCK_FAIL_APPLY=1
    /// - TSPI_MOCK_STEP_DELAY_MS=150
    /// - TSPI_MOCK_PERSISTED_MODE=dante
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
        let persisted_mode = std::env::var("TSPI_MOCK_PERSISTED_MODE")
            .ok()
            .and_then(|v| v.parse().ok());
        Self {
            fail_start: split("TSPI_MOCK_FAIL_START"),
            fail_restart: split("TSPI_MOCK_FAIL_RESTART"),
            camilla_apply_fails: crate::config::env_truthy("TSPI_MOCK_FAIL_APPLY").unwrap_or(false),
            switch_step_delay_ms: std::env::var("TSPI_MOCK_STEP_DELAY_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(120),
            persisted_mode,
            channelmap_preset: std::env::var("TSPI_MOCK_CHANNELMAP_PRESET")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            channelmap_apply_fails: crate::config::env_truthy("TSPI_MOCK_CHANNELMAP_FAIL_APPLY")
                .unwrap_or(false),
            channelmap_preload: crate::config::env_truthy("TSPI_MOCK_CHANNELMAP_PRELOAD")
                .unwrap_or(false),
            motu_apply_fails: crate::config::env_truthy("TSPI_MOCK_MOTU_APPLY_FAILS")
                .unwrap_or(false),
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<AppConfig>,
    pub started_at: Instant,
    pub auth: Arc<AuthState>,
    pub runner: Arc<dyn CommandRunner>,
    pub services: Arc<dyn ServiceManager>,
    pub system: Arc<dyn SystemBackend>,
    pub camilla: Arc<dyn CamillaBackend>,
    pub ptp: Arc<dyn PtpBackend>,
    pub alsa: Arc<dyn AlsaBackend>,
    pub power: Arc<dyn PowerBackend>,
    pub aoip: Arc<AoipBackend>,
    pub mode: Arc<ModeController>,
    pub channelmap: Arc<ChannelMapService>,
    pub motu: Arc<dyn MotuBackend>,
    pub mock: bool,
    /// 仅 mock 模式下有值，便于测试断言。
    pub mock_power: Option<Arc<MockPower>>,
    pub mock_store: Option<Arc<MockModeStore>>,
}

impl AppState {
    pub fn from_config(config: AppConfig) -> Self {
        if config.mock {
            Self::mock_with(config, MockOptions::from_env())
        } else {
            Self::real(config)
        }
    }

    pub fn real(config: AppConfig) -> Self {
        let runner: Arc<dyn CommandRunner> =
            Arc::new(RealCommandRunner::new(Duration::from_secs(20)));
        let services: Arc<dyn ServiceManager> = Arc::new(RealServiceManager::new(runner.clone()));
        let camilla: Arc<dyn CamillaBackend> = Arc::new(RealCamilla::new(
            config.camilladsp.clone(),
            services.clone(),
        ));
        let ptp: Arc<dyn PtpBackend> = Arc::new(RealPtp::new(
            config.ptp.clone(),
            runner.clone(),
            services.clone(),
        ));
        let alsa: Arc<dyn AlsaBackend> =
            Arc::new(RealAlsa::new(config.alsa.clone(), runner.clone()));
        let system: Arc<dyn SystemBackend> = Arc::new(RealSystemBackend::new(runner.clone()));
        let power: Arc<dyn PowerBackend> = Arc::new(RealPower::new(runner.clone()));
        let aoip = Arc::new(AoipBackend::new(
            services.clone(),
            config.aes67.clone(),
            config.inferno.clone(),
            false,
        ));
        let store: Arc<dyn ModeStore> =
            Arc::new(FileModeStore::new(config.mode.state_file.clone()));
        let mode = Arc::new(ModeController::new(
            config.mode.clone(),
            services.clone(),
            camilla.clone(),
            ptp.clone(),
            alsa.clone(),
            runner.clone(),
            store,
            false,
            Duration::ZERO,
        ));
        let channelmap = Arc::new(ChannelMapService::new(
            config.channelmap.clone(),
            runner.clone(),
            false,
        ));
        let motu: Arc<dyn MotuBackend> =
            Arc::new(RealMotu::new(config.motu.clone(), runner.clone()));
        Self {
            auth: Arc::new(AuthState::new(&config)),
            started_at: Instant::now(),
            config: Arc::new(config),
            runner,
            services,
            system,
            camilla,
            ptp,
            alsa,
            power,
            aoip,
            mode,
            channelmap,
            motu,
            mock: false,
            mock_power: None,
            mock_store: None,
        }
    }

    pub fn mock(config: AppConfig) -> Self {
        Self::mock_with(config, MockOptions::default())
    }

    pub fn mock_with(config: AppConfig, opts: MockOptions) -> Self {
        let mut config = config;
        config.mock = true;
        let mock_services = Arc::new(MockServiceManager::with_failures(
            opts.fail_start.clone(),
            Vec::new(),
            opts.fail_restart.clone(),
        ));
        let services: Arc<dyn ServiceManager> = mock_services.clone();
        let mock_camilla = Arc::new(MockCamilla::with_fail_apply(
            config.camilladsp.clone(),
            opts.camilla_apply_fails,
        ));
        let camilla: Arc<dyn CamillaBackend> = mock_camilla.clone();
        let ptp: Arc<dyn PtpBackend> = Arc::new(MockPtp::new(&config.ptp.conf_file));
        let alsa: Arc<dyn AlsaBackend> = Arc::new(MockAlsa::new(config.alsa.clone()));
        let system: Arc<dyn SystemBackend> = Arc::new(MockSystemBackend);
        let mock_power = Arc::new(MockPower::new());
        let power: Arc<dyn PowerBackend> = mock_power.clone();
        let runner: Arc<dyn CommandRunner> = Arc::new(MockCommandRunner);
        let aoip = Arc::new(AoipBackend::new(
            services.clone(),
            config.aes67.clone(),
            config.inferno.clone(),
            true,
        ));
        let persisted = opts.persisted_mode.map(|mode| PersistedMode {
            mode,
            updated_at_unix: util::now_unix(),
            last_error: None,
        });
        let mock_store = Arc::new(MockModeStore::new(persisted.clone()));
        let store: Arc<dyn ModeStore> = mock_store.clone();
        let mode = Arc::new(ModeController::new(
            config.mode.clone(),
            services.clone(),
            camilla.clone(),
            ptp.clone(),
            alsa.clone(),
            runner.clone(),
            store,
            true,
            Duration::from_millis(opts.switch_step_delay_ms),
        ));
        let channelmap = {
            let svc = ChannelMapService::new(config.channelmap.clone(), runner.clone(), true);
            if let Some(preset) = opts.channelmap_preset.clone() {
                let presets = crate::channelmap::LayoutPresets::parse(
                    crate::channelmap::EMBEDDED_LAYOUT_PRESETS,
                )
                .unwrap_or_default();
                let (input, output) = if let Some((a, b)) = preset.split_once("->") {
                    (a.trim().to_string(), b.trim().to_string())
                } else {
                    let input = if presets.input_layouts.contains_key(&preset) {
                        preset.clone()
                    } else {
                        "h16-7.1.4".to_string()
                    };
                    let output = if presets.output_layouts.contains_key(&preset) {
                        preset.clone()
                    } else {
                        "5.3.2".to_string()
                    };
                    (input, output)
                };
                let doc = crate::channelmap::draft_document(
                    &input, &output, &preset, &presets, None, None,
                );
                svc.with_mock_doc(doc)
            } else {
                svc
            }
        };
        channelmap.set_mock_apply_fail(opts.channelmap_apply_fails);
        let channelmap = Arc::new(channelmap);
        let motu: Arc<dyn MotuBackend> = Arc::new(MockMotu::new(opts.motu_apply_fails));
        Self {
            auth: Arc::new(AuthState::new(&config)),
            started_at: Instant::now(),
            config: Arc::new(config),
            runner,
            services,
            system,
            camilla,
            ptp,
            alsa,
            power,
            aoip,
            mode,
            channelmap,
            motu,
            mock: true,
            mock_power: Some(mock_power),
            mock_store: Some(mock_store),
        }
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// 总览页展示的服务单元（去重）。
    pub fn overview_units(&self) -> Vec<String> {
        let mut units = vec![
            self.config.camilladsp.service.clone(),
            self.config.ptp.ptp4l_service.clone(),
            self.config.ptp.phc2sys_service.clone(),
            self.config.ptp.statime_service.clone(),
            self.config.aes67.service.clone(),
            self.config.inferno.service.clone(),
        ];
        units.retain(|u| !u.trim().is_empty());
        units.dedup();
        units
    }

    // ===== 测试辅助 =====
    pub fn mock_power_actions(&self) -> Vec<String> {
        self.mock_power
            .as_ref()
            .map(|p| p.actions())
            .unwrap_or_default()
    }

    pub fn mock_store_mode(&self) -> Option<crate::config::RunMode> {
        self.mock_store
            .as_ref()
            .and_then(|s| s.get())
            .map(|p| p.mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_state_assembly() {
        let state = AppState::mock(AppConfig::mock_defaults());
        assert!(state.mock);
        assert!(state.auth.session_count() == 0);
        assert!(state.uptime_secs() <= 1);
        assert!(!state.overview_units().is_empty());
        assert_eq!(state.mock_store_mode(), None);
    }

    #[tokio::test]
    async fn mock_state_services_respond() {
        let state = AppState::mock(AppConfig::mock_defaults());
        let camilla = state.camilla.live().await;
        assert!(camilla.reachable);
        let sys = state.system.info().await;
        assert!(sys.mock);
        assert!(sys.cpu.cores > 0);
    }

    #[tokio::test]
    async fn mock_power_records() {
        let state = AppState::mock(AppConfig::mock_defaults());
        state
            .power
            .action(crate::power::PowerAction::Reboot)
            .await
            .unwrap();
        assert_eq!(state.mock_power_actions(), vec!["reboot"]);
    }
}
