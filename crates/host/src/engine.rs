//! 块引擎：固定块长、块边界生效、零分配、零锁。
//!
//! 结构：采集线程（后端）把一块输入交进来，引擎调处理链算出一块输出，再交回后端。
//! 所有随音频线程跑的缓冲都在构造时分配好；运行期只做切片读写与整数计数。
//!
//! 块边界语义：
//! - 控制命令（例如静音）不会立刻改变正在算的块，而是排到下个块开始时生效，
//!   并记录生效块号；重复提交同一命令返回同一块号且 `idempotent=true`。
//! - 因此上层的"立即生效"其实是"下一块生效"，这一点必须如实上报，不能谎报。

use crate::backend::{AudioBackend, BackendError, DeviceConfig};

/// 默认块长（帧）。
pub const DEFAULT_BLOCK_FRAMES: usize = 128;
/// 允许的最大块长（帧），超过视为配置错误。
pub const MAX_BLOCK_FRAMES: usize = 8192;

/// 引擎配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineConfig {
    /// 每块帧数。
    pub block_frames: usize,
    /// 采样率。
    pub sample_rate: u32,
    /// 通道数。
    pub channels: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            block_frames: DEFAULT_BLOCK_FRAMES,
            sample_rate: 96_000,
            channels: 2,
        }
    }
}

impl EngineConfig {
    /// 每块样本数。
    pub fn samples_per_block(&self) -> usize {
        self.block_frames * self.channels
    }

    /// 校验配置；不合法时点名原因。
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.block_frames == 0 || self.block_frames > MAX_BLOCK_FRAMES {
            return Err(EngineError::BadConfig("block_frames out of range"));
        }
        if self.channels == 0 {
            return Err(EngineError::BadConfig("channels must be positive"));
        }
        if self.sample_rate == 0 {
            return Err(EngineError::BadConfig("sample_rate must be positive"));
        }
        Ok(())
    }
}

/// 引擎错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// 配置非法。
    BadConfig(&'static str),
    /// 输入块长度与配置不符。
    WrongBlockLength { expected: usize, actual: usize },
    /// 后端错误。
    Backend(BackendError),
}

impl core::fmt::Display for EngineError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EngineError::BadConfig(msg) => write!(f, "bad config: {msg}"),
            EngineError::WrongBlockLength { expected, actual } => {
                write!(f, "wrong block length: expected {expected}, got {actual}")
            }
            EngineError::Backend(err) => write!(f, "backend: {err}"),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<BackendError> for EngineError {
    fn from(value: BackendError) -> Self {
        EngineError::Backend(value)
    }
}

/// 处理链。实现方必须零分配、零锁、无 IO。
pub trait Processor {
    /// 处理一块样本（帧交错）：输入与输出长度相同。
    fn process(&mut self, input: &[f32], output: &mut [f32]);
}

/// 直通处理链（用于骨架与门禁）。
#[derive(Debug, Default)]
pub struct Passthrough;

impl Processor for Passthrough {
    fn process(&mut self, input: &[f32], output: &mut [f32]) {
        output.copy_from_slice(input);
    }
}

/// 静音处理链（把输出清零）。
#[derive(Debug, Default)]
pub struct Silent;

impl Processor for Silent {
    fn process(&mut self, _input: &[f32], output: &mut [f32]) {
        output.fill(0.0);
    }
}

/// 引擎运行统计（原子快照）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EngineStats {
    /// 已处理的块数。
    pub blocks: u64,
    /// 提交但尚未生效的控制命令数。
    pub pending_commands: u64,
    /// 缓冲欠载次数。
    pub underruns: u64,
    /// 缓冲溢出次数。
    pub overruns: u64,
    /// 处理链故障次数。
    pub faults: u64,
    /// 当前是否静音（已生效值）。
    pub mute: bool,
    /// 静音生效的块号；未静音为 0。
    pub mute_applied_block: u64,
}

/// 静音命令回执。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MuteReceipt {
    /// 生效后的静音状态。
    pub mute: bool,
    /// 生效块号：请求之后即将开始的那一块。
    pub applied_block: u64,
    /// 是否因为状态已相同而没有排队。
    pub idempotent: bool,
}

/// 块引擎。
#[derive(Debug)]
pub struct BlockEngine<P: Processor> {
    config: EngineConfig,
    processor: P,
    output: Vec<f32>,
    stats: EngineStats,
    pending_mute: Option<bool>,
    /// 上一次已生效的静音状态，用于幂等回执。
    applied_mute: bool,
    applied_mute_block: u64,
}

impl<P: Processor> BlockEngine<P> {
    /// 新建引擎。缓冲在此一次分配完毕，之后不再分配。
    pub fn new(config: EngineConfig, processor: P) -> Result<Self, EngineError> {
        config.validate()?;
        let output = vec![0.0f32; config.samples_per_block()];
        Ok(Self {
            config,
            processor,
            output,
            stats: EngineStats::default(),
            pending_mute: None,
            applied_mute: false,
            applied_mute_block: 0,
        })
    }

    /// 配置。
    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// 每块样本数。
    pub fn samples_per_block(&self) -> usize {
        self.config.samples_per_block()
    }

    /// 统计快照。
    pub fn stats(&self) -> EngineStats {
        self.stats.clone()
    }

    /// 处理一块：校验长度、应用待生效命令、调用处理链或静音。
    ///
    /// 实时约束：不分配、不加锁、不做 IO、不打印。
    pub fn run_block(&mut self, input: &[f32]) -> Result<&[f32], EngineError> {
        self.process_in_place(input)?;
        Ok(&self.output)
    }

    /// 原地处理一块：只更新内部输出缓冲与统计，不返回借用。
    fn process_in_place(&mut self, input: &[f32]) -> Result<(), EngineError> {
        let expected = self.samples_per_block();
        if input.len() != expected {
            return Err(EngineError::WrongBlockLength {
                expected,
                actual: input.len(),
            });
        }
        let block_number = self.stats.blocks + 1;
        if let Some(mute) = self.pending_mute.take() {
            self.applied_mute = mute;
            self.applied_mute_block = block_number;
        }
        if self.applied_mute {
            self.output.fill(0.0);
        } else {
            self.processor.process(input, &mut self.output);
        }
        self.stats.blocks = block_number;
        self.stats.mute = self.applied_mute;
        self.stats.mute_applied_block = self.applied_mute_block;
        Ok(())
    }

    /// 请求静音/取消静音；在下一块生效。
    pub fn request_mute(&mut self, mute: bool) -> MuteReceipt {
        let effective = self.pending_mute.unwrap_or(self.applied_mute);
        if effective == mute {
            return MuteReceipt {
                mute,
                applied_block: if self.pending_mute.is_some() {
                    self.stats.blocks + 1
                } else {
                    self.applied_mute_block
                },
                idempotent: true,
            };
        }
        self.pending_mute = Some(mute);
        self.stats.pending_commands += 1;
        MuteReceipt {
            mute,
            applied_block: self.stats.blocks + 1,
            idempotent: false,
        }
    }

    /// 处理链的可变引用（仅非实时路径使用，例如换系数草稿）。
    pub fn processor_mut(&mut self) -> &mut P {
        &mut self.processor
    }

    /// 后端驱动的单块往返：采集 → 处理 → 送出。
    pub fn pump<B: AudioBackend>(
        &mut self,
        backend: &mut B,
        scratch: &mut [f32],
    ) -> Result<u64, EngineError> {
        if scratch.len() != self.samples_per_block() {
            return Err(EngineError::WrongBlockLength {
                expected: self.samples_per_block(),
                actual: scratch.len(),
            });
        }
        backend.capture(scratch)?;
        self.process_in_place(scratch)?;
        let produced = self.stats.blocks;
        backend.playout(&self.output)?;
        Ok(produced)
    }

    /// 设备配置视图（供后端打开时使用）。
    pub fn device_config(&self, device: crate::owner::DeviceId) -> DeviceConfig {
        DeviceConfig {
            device,
            sample_rate: self.config.sample_rate,
            block_frames: self.config.block_frames,
            channels: self.config.channels,
        }
    }
}
