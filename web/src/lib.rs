//! 泰山派3M（RK3576）音频 DSP 设备 Web 管理服务。
//!
//! 一期：认证、总览、CamillaDSP、PTP、AES67/Inferno、ALSA/MOTU、
//! 运行模式切换（dante / aes67 / off）、音频格式有限编辑、电源操作。
//! TSPI_MOCK=1 时全部后端走内存 mock，不执行任何系统命令。

pub mod alsa;
pub mod aoip;
pub mod assets;
pub mod audio;
pub mod auth;
pub mod camilla;
pub mod channelmap;
pub mod cmd;
pub mod config;
pub mod error;
pub mod mode;
pub mod motu;
pub mod power;
pub mod ptp;
pub mod routes;
pub mod service;
pub mod speaker_route;
pub mod state;
pub mod system;
pub mod util;

pub use config::AppConfig;
pub use error::{AppError, AppResult};
pub use state::AppState;

/// 服务版本（与 Cargo.toml 同步）。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 构建路由：测试与 main 共用同一个入口。
pub fn build_router(state: AppState) -> axum::Router {
    routes::build_router(state)
}
