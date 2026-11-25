//! 外部命令执行抽象。
//!
//! 真机通过 tokio::process 启动 systemctl / pmc / aplay / ip 等命令；
//! mock 模式使用 `MockCommandRunner`，任何命令调用都返回错误，
//! 确保 TSPI_MOCK=1 时绝不会真的操作系统。

use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;

/// 单次命令执行的输出。
#[derive(Debug, Clone, Serialize)]
pub struct CommandOutput {
    pub program: String,
    pub args: Vec<String>,
    /// 退出码；被信号终止时为 None。
    pub status: Option<i32>,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub fn stdout_trimmed(&self) -> &str {
        self.stdout.trim()
    }
}

#[derive(Debug, Clone)]
pub enum CommandError {
    Spawn(String),
    Timeout(Duration),
    Io(String),
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(m) => write!(f, "命令启动失败: {m}"),
            Self::Timeout(d) => write!(f, "命令执行超时({}ms)", d.as_millis()),
            Self::Io(m) => write!(f, "命令执行失败: {m}"),
        }
    }
}

impl std::error::Error for CommandError {}

#[async_trait]
pub trait CommandRunner: Send + Sync {
    /// 执行命令并等待结束（带默认超时），不因非零退出码报错。
    async fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, CommandError>;

    /// 带自定义超时的执行（默认回退到 run）。模式切换脚本等长任务使用。
    async fn run_with_timeout(
        &self,
        program: &str,
        args: &[&str],
        _timeout: Duration,
    ) -> Result<CommandOutput, CommandError> {
        self.run(program, args).await
    }
}

/// 真机实现：tokio 进程 + 超时 + kill_on_drop。
pub struct RealCommandRunner {
    timeout: Duration,
}

impl RealCommandRunner {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

#[async_trait]
impl CommandRunner for RealCommandRunner {
    async fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, CommandError> {
        self.run_with_timeout(program, args, self.timeout).await
    }

    async fn run_with_timeout(
        &self,
        program: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<CommandOutput, CommandError> {
        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let fut = command.output();
        let output = match tokio::time::timeout(timeout, fut).await {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => return Err(CommandError::Spawn(format!("{program}: {e}"))),
            Err(_) => return Err(CommandError::Timeout(timeout)),
        };
        Ok(CommandOutput {
            program: program.to_string(),
            args: args.iter().map(|a| a.to_string()).collect(),
            status: output.status.code(),
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

/// mock 模式禁用命令执行：防止测试/演示误操作系统。
pub struct MockCommandRunner;

#[async_trait]
impl CommandRunner for MockCommandRunner {
    async fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, CommandError> {
        Err(CommandError::Spawn(format!(
            "mock 模式不执行真实命令: {program} {}",
            args.join(" ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_runner_rejects_everything() {
        let runner = MockCommandRunner;
        let err = runner.run("systemctl", &["reboot"]).await.unwrap_err();
        assert!(err.to_string().contains("mock"));
    }

    #[tokio::test]
    async fn real_runner_reports_missing_program() {
        let runner = RealCommandRunner::new(Duration::from_secs(2));
        let err = runner
            .run("tspi-audio-web-definitely-not-exist", &["--version"])
            .await
            .unwrap_err();
        assert!(matches!(err, CommandError::Spawn(_)));
    }
}
