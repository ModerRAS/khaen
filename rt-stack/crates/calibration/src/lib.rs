//! TSPI 校准 daemon（crate tspi-calibration） ——WBS T06 scaffold。
//!
//! **本 crate 目前只有 stub**：leaf 模块由各任务实现；lib.rs/公共注册只由 integrator 写。
//! scaffold 状态：NOT_RUN（无 leaf 实现、无板端、无 RT 测量）。
//!
//! 权威契约：rt-stack/spec/core-v1.md（T02）与 rt-stack/README.md 写域纪律。

pub mod capture;
pub mod compile_apply;
pub mod crossover_post;
pub mod fit_wiring;
pub mod host_apply;
pub mod ir_distance;
pub mod ir_extract;
pub use tspi_cal_dsp::job;
pub use tspi_cal_dsp::microfft;
pub mod measured_design;
pub mod noise_est;
pub use tspi_cal_dsp::peq_fit;
pub mod peq_plan;
pub mod phase_fit;
pub mod rt60;
pub use tspi_cal_dsp::positions;
pub mod stage_backend;
pub mod sweep;

#[cfg(test)]
mod t64_recovery {
    use std::path::PathBuf;

    use tspi_cal_dsp::peq_fit::*;
    use tspi_fir_designer::modal_peq::{BandType, ChannelPeq, ModalBand};

    include!("../tests/T64/target_aware_recovery.rs");
}
