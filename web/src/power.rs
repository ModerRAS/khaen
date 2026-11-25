//! 电源操作（重启/关机），默认关闭，配置开启后仍需二次确认短语。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;

use crate::cmd::CommandRunner;
use crate::config::PowerConfig;
use crate::error::{AppError, AppResult};

/// 二次确认短语（前端要求用户输入）。
pub const CONFIRM_REBOOT: &str = "REBOOT";
pub const CONFIRM_SHUTDOWN: &str = "SHUTDOWN";

#[derive(Debug, Clone, Serialize)]
pub struct PowerResult {
    pub action: String,
    pub mock: bool,
    pub message: String,
}

#[async_trait]
pub trait PowerBackend: Send + Sync {
    fn mock(&self) -> bool;
    async fn action(&self, action: PowerAction) -> AppResult<PowerResult>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    Reboot,
    Shutdown,
}

impl PowerAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reboot => "reboot",
            Self::Shutdown => "shutdown",
        }
    }
    pub fn confirm_phrase(self) -> &'static str {
        match self {
            Self::Reboot => CONFIRM_REBOOT,
            Self::Shutdown => CONFIRM_SHUTDOWN,
        }
    }
}

pub struct RealPower {
    runner: Arc<dyn CommandRunner>,
}

impl RealPower {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
}

#[async_trait]
impl PowerBackend for RealPower {
    fn mock(&self) -> bool {
        false
    }

    async fn action(&self, action: PowerAction) -> AppResult<PowerResult> {
        let op = action.as_str();
        let out = self
            .runner
            .run_with_timeout("systemctl", &[op], Duration::from_secs(15))
            .await
            .map_err(|e| AppError::unavailable(format!("执行 systemctl {op} 失败: {e}")))?;
        if !out.success {
            return Err(AppError::unavailable(format!(
                "systemctl {op} 返回失败: {}",
                out.stderr.trim().lines().next().unwrap_or("未知错误")
            )));
        }
        Ok(PowerResult {
            action: op.to_string(),
            mock: false,
            message: format!("已下发 systemctl {op} 命令"),
        })
    }
}

pub struct MockPower {
    actions: Mutex<Vec<String>>,
}

impl MockPower {
    pub fn new() -> Self {
        Self {
            actions: Mutex::new(Vec::new()),
        }
    }
    pub fn actions(&self) -> Vec<String> {
        self.actions.lock().unwrap().clone()
    }
}

impl Default for MockPower {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PowerBackend for MockPower {
    fn mock(&self) -> bool {
        true
    }

    async fn action(&self, action: PowerAction) -> AppResult<PowerResult> {
        self.actions
            .lock()
            .unwrap()
            .push(action.as_str().to_string());
        Ok(PowerResult {
            action: action.as_str().to_string(),
            mock: true,
            message: format!("mock：已模拟 {}（不会真的执行）", action.as_str()),
        })
    }
}

/// 校验电源操作是否允许（配置 + 二次确认短语）。
pub fn check_power(cfg: &PowerConfig, action: PowerAction, confirm: Option<&str>) -> AppResult<()> {
    let allowed = match action {
        PowerAction::Reboot => cfg.allow_reboot,
        PowerAction::Shutdown => cfg.allow_shutdown,
    };
    if !allowed {
        return Err(AppError::forbidden(format!(
            "{} 未在配置中开启（[system] allow_{} = true）",
            action.as_str(),
            action.as_str()
        )));
    }
    let phrase = confirm.unwrap_or("").trim();
    if phrase != action.confirm_phrase() {
        return Err(AppError::bad_request(format!(
            "二次确认短语不正确：需要输入 {}",
            action.confirm_phrase()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_requires_config_and_phrase() {
        let cfg = PowerConfig::default();
        assert!(check_power(&cfg, PowerAction::Reboot, Some("REBOOT")).is_err());
        let cfg = PowerConfig {
            allow_reboot: true,
            allow_shutdown: true,
        };
        assert!(check_power(&cfg, PowerAction::Reboot, Some("WRONG")).is_err());
        assert!(check_power(&cfg, PowerAction::Reboot, Some("reboot")).is_err());
        assert!(check_power(&cfg, PowerAction::Reboot, Some("REBOOT")).is_ok());
        assert!(check_power(&cfg, PowerAction::Shutdown, Some("SHUTDOWN")).is_ok());
    }

    #[tokio::test]
    async fn mock_power_records_actions() {
        let p = MockPower::new();
        p.action(PowerAction::Reboot).await.unwrap();
        assert_eq!(p.actions(), vec!["reboot"]);
    }
}
