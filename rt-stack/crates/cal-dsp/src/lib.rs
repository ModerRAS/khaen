//! 校准求解用的纯计算。不打开声卡，不依赖 rt-host。
//!
//! `job` / `positions` / `peq_fit` / `microfft` 的源文件仍在 calibration 目录，
//! 这里按路径编进来，calibration 再原样导出。`ir_extract::deconvolve` 转调本 crate 的同名函数。

#[path = "../../calibration/src/microfft.rs"]
pub mod microfft;

#[path = "../../calibration/src/job.rs"]
pub mod job;

/// 听音位朝向字面值。calibration 的采集模块重新导出这一份，避免两处各写一个。
pub mod capture {
    pub const UMIK_ORIENTATION_VERTICAL: &str = "90deg";
}

#[path = "../../calibration/src/positions.rs"]
pub mod positions;

#[path = "../../calibration/src/peq_fit.rs"]
pub mod peq_fit;

mod deconvolve;

pub use deconvolve::deconvolve;
