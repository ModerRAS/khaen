//! khaen AES67 输入 ASRC（T41）：rubato 5 异步重采样 + 抖动环漂移控制。
//!
//! # 为什么放在独立 crate
//!
//! `khaen-core` 是零依赖的 DSP 内核层（只允许纯数学 + 标准库）；rubato 会引入
//! `audioadapter`/`realfft`/`rustfft` 等依赖与平台相关 SIMD 代码。ASRC 属于**输入适配层**
//! 而非内核算子，放在独立 crate 既不污染 core 的零依赖约束，也让 `cargo tree -p khaen-core`
//! 保持干净。host 通过 `khaen-asrc` 依赖接线。
//!
//! # 数据流（host 侧）
//!
//! ~~~text
//! RTP/L24 ─► 抖动环（媒体帧域） ─► 按 input_frames_next() 取帧 ─► Async 固定输出块(128)
//!    ▲                                                                     │
//!    └──────────── DriftController(fill 误差 → ppm) ◄── fill 快照 ◄────────┘
//! ~~~
//!
//! - 重采样器固定输出 `block_frames` 帧、输入帧数随比率变化（[rubato::FixedAsync::Output]）；
//! - 控制器看抖动环 fill 误差，输出比率修正 ppm（PI + 限幅 ±max_ppm + 条件积分抗饱和 +
//!   启动冻结），把 fill 稳在 `target_fill_frames`；
//! - 输出声道映射、状态上报、bypass 由 host 负责；本 crate 不碰 socket/时钟/PTP。
//!
//! # 实时约束
//!
//! [Asrc::process] / [Asrc::update_fill] / [DriftController::update] 都是纯算术 + 对构造期
//! 预分配缓冲的拷贝：零分配、零锁、无 IO、不 panic（错误经返回值/`last_error` 上报）。
//! 所有缓冲（planar 输入/输出、rubato 内部缓冲、sinc 表）在 [Asrc::new] 一次分配。
//!
//! # 版本/许可/MSRV
//!
//! - rubato 5.1.0（MIT OR Apache-2.0）；
//! - rubato 5.1.0 的 rust-version = 1.87，因此 workspace 的 `rust-version` 需要从 1.80
//!   提到 1.87（由 lead 集成时改根 Cargo.toml）；
//! - 依赖在本地 cargo 缓存齐全，构建/测试可 `--offline`。
//!
//! # poly vs sinc（取舍见 README）
//!
//! - poly（默认，[Interpolation::Poly]，Cubic）：无抗混叠滤波、延迟 ~2 输出帧、成本最低；
//!   在 ±500 ppm 的近似 1:1 比率与 48 kHz 音频下 THD+N 已足够低（实测见 README）；
//! - sinc（备用，[Interpolation::Sinc]）：带抗混叠滤波、延迟 ~sinc_len/2 输出帧、成本高数倍；
//!   只有 PM 要求更高纯度或比率范围更宽时才需要。

mod config;
mod controller;
mod error;
mod resampler;
mod status;

pub use config::{
    AsrcConfig, Interpolation, DEFAULT_FILL_LPF_SEC, DEFAULT_KI_PPM_PER_FRAME_SEC,
    DEFAULT_KP_PPM_PER_FRAME, DEFAULT_MAX_PPM, DEFAULT_SINC_LEN, DEFAULT_SINC_OVERSAMPLING,
    DEFAULT_STARTUP_BLOCKS, DEFAULT_TARGET_BLOCKS, MAX_ASRC_CHANNELS, MAX_CONFIGURABLE_PPM,
};
pub use controller::DriftController;
pub use error::AsrcError;
pub use resampler::{AsyncResampler, PlanarBuffers, ProcessOutcome};
pub use status::AsrcStatus;

/// 组合封装：漂移控制器 + 异步重采样器。host 每个音频块调用一次 [Asrc::process]，
/// 再用消费后的抖动环 fill 调用 [Asrc::update_fill] 调整下一块比率。
pub struct Asrc {
    config: AsrcConfig,
    resampler: AsyncResampler,
    controller: DriftController,
    last_error: Option<&'static str>,
    bypassed: bool,
    blocks: u64,
}

impl std::fmt::Debug for Asrc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Asrc")
            .field("config", &self.config)
            .field("resampler", &self.resampler)
            .field("ratio_ppm", &self.resampler.ratio_ppm())
            .field("ppm_estimate", &self.controller.estimate_ppm())
            .field("bypassed", &self.bypassed)
            .field("blocks", &self.blocks)
            .finish_non_exhaustive()
    }
}

impl Asrc {
    /// 构造 ASRC（校验配置 + 一次分配全部缓冲/sinc 表）。
    pub fn new(config: AsrcConfig) -> Result<Self, AsrcError> {
        config.validate()?;
        let resampler = AsyncResampler::new(&config)?;
        let controller = DriftController::new(&config)?;
        Ok(Self {
            config,
            resampler,
            controller,
            last_error: None,
            bypassed: false,
            blocks: 0,
        })
    }

    /// 配置视图。
    pub fn config(&self) -> &AsrcConfig {
        &self.config
    }

    /// 下一次 [process](Self::process) 需要的输入帧数。
    pub fn input_frames_next(&self) -> usize {
        self.resampler.input_frames_next()
    }

    /// 每次 [process](Self::process) 的固定输出帧数。
    pub fn output_frames(&self) -> usize {
        self.resampler.output_frames()
    }

    /// 重采样器群延迟（输出帧）。
    pub fn output_delay(&self) -> usize {
        self.resampler.output_delay()
    }

    /// 输入 planar 缓冲：调用方把 `input_frames_next()` 帧填好，缺帧写 0。
    pub fn input_mut(&mut self) -> &mut PlanarBuffers {
        self.resampler.input_mut()
    }

    /// 最近一次 [process](Self::process) 的输出（planar，每声道 `output_frames()` 帧）。
    pub fn output(&self) -> &PlanarBuffers {
        self.resampler.output()
    }

    /// 处理一块：消耗 `input_frames_next()` 帧输入、产出 `output_frames()` 帧输出。
    ///
    /// 失败时记录 `last_error` 并置 bypassed（host 应停止使用 ASRC、回退到原输入路径）。
    pub fn process(&mut self) -> Result<ProcessOutcome, AsrcError> {
        match self.resampler.process() {
            Ok(outcome) => {
                self.blocks = self.blocks.saturating_add(1);
                Ok(outcome)
            }
            Err(error) => {
                self.last_error = Some(error.name());
                self.bypassed = true;
                Err(error)
            }
        }
    }

    /// 用消费后的抖动环 fill（帧）驱动控制器，并把修正后的比率应用到下一块。
    ///
    /// 返回本次施加的 ppm。失败时记录 `last_error` 并置 bypassed。
    pub fn update_fill(&mut self, fill_frames: usize) -> Result<f64, AsrcError> {
        let ppm = self.controller.update(fill_frames);
        match self.resampler.set_ratio_ppm(ppm) {
            Ok(()) => Ok(ppm),
            Err(error) => {
                self.last_error = Some(error.name());
                self.bypassed = true;
                Err(error)
            }
        }
    }

    /// 状态快照（`fill_frames` 由调用方提供当前抖动环水位）。
    pub fn status(&self, fill_frames: usize) -> AsrcStatus {
        AsrcStatus {
            enabled: self.config.enable && !self.bypassed,
            ratio_ppm: self.resampler.ratio_ppm() as f32,
            fill_frames,
            ppm_estimate: self.controller.estimate_ppm() as f32,
            last_error: self.last_error,
        }
    }

    /// 是否已因失败被 bypass。
    pub fn is_bypassed(&self) -> bool {
        self.bypassed
    }

    /// 强制记为 bypass（host 侧检测到其他不可恢复条件时用）。
    pub fn note_bypass(&mut self, error: AsrcError) {
        self.last_error = Some(error.name());
        self.bypassed = true;
    }

    /// 清除 bypass / 错误标记（例如换流后重新启用，由 host 决定）。
    pub fn clear_bypass(&mut self) {
        self.bypassed = false;
        self.last_error = None;
    }

    /// 已处理块数（诊断用）。
    pub fn blocks(&self) -> u64 {
        self.blocks
    }

    /// 控制器最近一次进入 PI 的 fill 误差（帧，目标 − 低通后 fill；诊断用）。
    pub fn controller_error_frames(&self) -> f64 {
        self.controller.last_error_frames()
    }

    /// 换流/失效复位：清空重采样器内部状态与控制器积分，回到启动阶段。
    pub fn reset(&mut self) {
        self.resampler.reset();
        self.controller.reset();
        self.last_error = None;
        self.bypassed = false;
        self.blocks = 0;
    }
}
