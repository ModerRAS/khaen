//! tspi-audio-web 入口：参数解析、配置加载/首次生成 token、后端装配、HTTP 服务。

use std::net::SocketAddr;
use std::path::PathBuf;

use tspi_audio_web::config::{self, LoadReport};
use tspi_audio_web::{build_router, AppConfig, AppState, VERSION};

#[derive(Debug, Default)]
struct Args {
    config: Option<PathBuf>,
    listen: Option<String>,
    mock: bool,
    print_token: bool,
}

const USAGE: &str = "\
tspi-audio-web —— 泰山派3M 音频 DSP 设备 Web 管理服务

用法: tspi-audio-web [选项]

选项:
  -c, --config <路径>   配置文件路径（默认 Linux: /etc/tspi-audio-web/config.toml；
                        Windows 开发: ./config.toml）
  -l, --listen <地址>   监听地址，例如 0.0.0.0:80（默认；被占用时回退 0.0.0.0:8088）
      --mock            内存 mock 模式（等价于 TSPI_MOCK=1，不执行任何系统命令）
      --print-token     打印当前访问 token 后退出（便于找回）
  -h, --help            显示帮助
  -V, --version         显示版本

环境变量:
  TSPI_CONFIG           配置文件路径（低于 --config）
  TSPI_MOCK=1           mock 模式
  TSPI_LISTEN           监听地址
  TSPI_TOKEN            覆盖配置文件 token（临时）
  TSPI_ALLOW_REBOOT=1   开启重启按钮（仍默认关闭）
  TSPI_ALLOW_SHUTDOWN=1 开启关机按钮
";

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-c" | "--config" => {
                args.config = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| "--config 需要路径参数".to_string())?,
                ))
            }
            "-l" | "--listen" => {
                args.listen = Some(
                    iter.next()
                        .ok_or_else(|| "--listen 需要地址参数".to_string())?,
                )
            }
            "--mock" => args.mock = true,
            "--print-token" => args.print_token = true,
            "-V" | "--version" => {
                println!("tspi-audio-web {VERSION}");
                std::process::exit(0);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("未知参数: {other}（--help 查看用法）")),
        }
    }
    Ok(args)
}

fn print_banner(config: &AppConfig, report: &LoadReport) {
    eprintln!("=== tpsi-audio-web {} ===", VERSION);
    eprintln!("配置: {}", report.path.display());
    if report.created {
        eprintln!("首次启动：已按模板生成配置文件（权限 0600）");
    }
    for warning in &report.warnings {
        eprintln!("warn: {warning}");
    }
    if report.token_generated {
        eprintln!();
        eprintln!("************************************************************");
        eprintln!("  已生成新的访问 token（请妥善保存）:");
        eprintln!("    {}", config.token);
        eprintln!(
            "  查看: tspi-audio-web --config {} --print-token",
            report.path.display()
        );
        eprintln!("  修改: 编辑配置文件 token 后 systemctl restart tspi-audio-web");
        eprintln!("************************************************************");
        eprintln!();
    }
    if config.mock {
        eprintln!("模式: MOCK（不执行真实 systemctl/aplay/pmc，仅内存模拟）");
        eprintln!("登录 token: {}", config.token);
    } else {
        eprintln!("模式: 真机（需要 root 权限管理 systemd 服务）");
    }
    eprintln!("监听: http://{}", config.listen);
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    eprintln!("收到退出信号，正在停止 tspi-audio-web …");
}

#[tokio::main]
async fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };

    let mock_requested = args.mock || config::env_truthy("TSPI_MOCK").unwrap_or(false);

    let (mut config, report) = match config::load(args.config.clone(), mock_requested) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };
    if let Some(listen) = args.listen {
        config.listen = listen;
    }
    if args.print_token {
        println!("{}", config.token);
        return;
    }
    if config::is_placeholder_token(&config.token) {
        eprintln!("warn: 当前 token 仍是占位符，请立即修改配置文件（或删除后重启自动生成）");
    }

    print_banner(&config, &report);

    let listen = config.listen.clone();
    let state = AppState::from_config(config);

    // 启动后按持久化状态恢复上次运行模式（异步，不阻塞 HTTP 服务启动）。
    if state.mode.status().restore_on_start {
        let restore_state = state.clone();
        tokio::spawn(async move {
            match restore_state.mode.restore_from_store().await {
                Ok(Some(view)) => {
                    eprintln!(
                        "启动恢复运行模式: {} → {}",
                        view.mode.as_str(),
                        view.state_label()
                    );
                }
                Ok(None) => {}
                Err(e) => eprintln!("warn: 恢复运行模式失败: {e}"),
            }
        });
    }

    let app = build_router(state);
    let addr: SocketAddr = match listen.parse() {
        Ok(addr) => addr,
        Err(e) => {
            eprintln!("error: 监听地址 {listen} 无效: {e}");
            std::process::exit(2);
        }
    };
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("error: 无法监听 {addr}: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("服务已启动：http://{addr}");
    if let Err(e) = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    {
        eprintln!("error: HTTP 服务异常退出: {e}");
        std::process::exit(1);
    }
}
