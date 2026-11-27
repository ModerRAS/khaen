//! 块引擎：固定块长、块边界生效、零分配、零锁。
//!
//! 结构：采集线程（后端）把一块输入交进来，引擎调处理链算出一块输出，再交回后端。
//! 所有随音频线程跑的缓冲都在构造时分配好；运行期只做切片读写与整数计数。
//!
//! 块边界语义：
//! - 控制命令（例如静音）不会立刻改变正在算的块，而是排到下个块开始时生效，
//!   并记录生效块号；重复提交同一命令返回同一块号且 `idempotent=true`。
//! - 因此上层的"立即生效"其实是"下一块生效"，这一点必须如实上报，不能谎报。
//!
//! # 两种执行器
//!
//! - [`BlockEngine`]：编译期确定处理链类型的块引擎，运行期不可换图。
//! - [`SlotEngine`]：在同一套块边界语义上增加"下一块换图"——方案提交（commit）后不会打断
//!   当前块，下一块 `run_block` 入口原子换上新的处理链，并记录 applied_block / version /
//!   graph_id 与自增的 epoch。换图路径与处理路径都不分配、不加锁、不做 IO、不 panic。
//!   换下的旧图不在实时线程析构：先移入退役槽，等下一次 `stage_processor`（控制线程）释放。

use khaen_core::rt::SAMPLE_RATES_HZ;

use crate::backend::{AudioBackend, BackendError, DeviceConfig};

/// 默认块长（帧）。
pub const DEFAULT_BLOCK_FRAMES: usize = 128;
/// 允许的最大块长（帧），超过视为配置错误。
pub const MAX_BLOCK_FRAMES: usize = 8192;

/// 引擎配置。
///
/// 输入与输出通道数分开（D-009）：应用可以从 2 路输入算出 22 路执行器/设备输出，
/// 不再把“输入宽度 / 处理 lane 数 / 物理输出宽度”合成一个数。方案 ABI v3 里
/// content.in_channels 对应 Self::input_channels，routing.dac_channels
/// （缺 routing 时 matrix.rows）对应 Self::output_channels。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineConfig {
    /// 每块帧数。
    pub block_frames: usize,
    /// 采样率。
    pub sample_rate: u32,
    /// 输入通道数（喂给 content 段的实际输入宽度）。
    pub input_channels: usize,
    /// 输出通道数（routing 写出的物理 DAC 通道数）。
    pub output_channels: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            block_frames: DEFAULT_BLOCK_FRAMES,
            sample_rate: 96_000,
            input_channels: 2,
            output_channels: 2,
        }
    }
}

impl EngineConfig {
    /// 输入块样本数（block_frames × input_channels）。
    pub fn input_samples_per_block(&self) -> usize {
        self.block_frames * self.input_channels
    }

    /// 输出块样本数（block_frames × output_channels）。
    pub fn output_samples_per_block(&self) -> usize {
        self.block_frames * self.output_channels
    }

    /// 校验配置；不合法时点名原因。
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.block_frames == 0 || self.block_frames > MAX_BLOCK_FRAMES {
            return Err(EngineError::BadConfig("block_frames out of range"));
        }
        if self.input_channels == 0 {
            return Err(EngineError::BadConfig("input_channels must be positive"));
        }
        if self.output_channels == 0 {
            return Err(EngineError::BadConfig("output_channels must be positive"));
        }
        if self.sample_rate == 0 {
            return Err(EngineError::BadConfig("sample_rate must be positive"));
        }
        // 速率是方案数据，也是运行时能力：集合由 core::rt::SAMPLE_RATES_HZ 唯一决定。
        // 不在集合里的速率必须在构造引擎时就 fail-closed，而不是等设备打开后才发现。
        if !SAMPLE_RATES_HZ.contains(&self.sample_rate) {
            return Err(EngineError::BadConfig(
                "sample_rate is not in khaen_core::rt::SAMPLE_RATES_HZ (supported: 48000, 96000)",
            ));
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
        let output = vec![0.0f32; config.output_samples_per_block()];
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

    /// 输出块样本数（= 调用方交给 `playout` 的长度）。
    pub fn output_samples_per_block(&self) -> usize {
        self.config.output_samples_per_block()
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
        let expected = self.config.input_samples_per_block();
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
        if scratch.len() != self.config.input_samples_per_block() {
            return Err(EngineError::WrongBlockLength {
                expected: self.config.input_samples_per_block(),
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
            input_channels: self.config.input_channels,
            output_channels: self.config.output_channels,
        }
    }
}

// ===========================================================================
// 可切换执行器：SlotEngine
// ===========================================================================

/// 已应用图的快照；全部为 `None` 表示还没有任何方案图生效（执行器仍跑初始处理链）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphInfo {
    /// 当前生效图的 graph_id（方案 ABI v3 的 sha256 十六进制串）。
    pub graph_id: Option<String>,
    /// 生效代数：每成功换图一次加一，从 1 开始；还没换过图时保持 `None`。
    pub epoch: Option<u64>,
    /// 当前生效图对应的方案版本号（控制面 prepare 时分配的 `planned_version`）。
    pub version: Option<u64>,
    /// 当前图开始生效的块号（1 起，对应 `run_block` 的块序号）；未换过图为 `None`。
    pub applied_block: Option<u64>,
}

/// 待生效的图切换：`stage_processor` 放入，下一次 `run_block` 入口取出并应用。
struct PendingSwitch {
    /// 新图的处理链。
    processor: Box<dyn Processor + Send>,
    /// 新图对应的方案版本号。
    version: u64,
    /// 新图的 graph_id。
    graph_id: String,
}

/// 退役槽：换图时被换下的旧图与旧 graph_id。
///
/// 换图发生在实时线程的块边界，但旧图的析构（释放卷积状态、延迟线等缓冲）不能放在实时线程。
/// 这里先把旧图移入退役槽，等下一次 `stage_processor`（控制/管理线程）再释放，
/// 因此实时路径既不分配也不释放堆内存。
struct Retired {
    /// 被换下的处理链。
    processor: Box<dyn Processor + Send>,
    /// 被换下的 graph_id（与处理链一起退役，避免在实时线程释放 String）。
    graph_id: Option<String>,
}

/// 块边界可换图的执行器。
///
/// 与 [`BlockEngine`] 的区别只有一个：处理链是 `Box<dyn Processor + Send>`，可以在运行期由
/// [`SlotEngine::stage_processor`] 排入新图，并在下一块开始的入口原子换上。除此之外的块边界
/// 语义（静音命令、块号、统计、错误）与 `BlockEngine` 完全一致。
///
/// 实时约束：`run_block` 不分配、不加锁、不做 IO、不打印、不 panic；所有可能失败的校验
/// （块长）在进入处理前返回错误，且不应用任何待生效命令（该块视作没开始）。
pub struct SlotEngine {
    config: EngineConfig,
    /// 当前生效的处理链。
    processor: Box<dyn Processor + Send>,
    /// 退役槽（见 [`Retired`]）：只在换图时写入，在下一次 `stage_processor` 清空。
    retired: Option<Retired>,
    /// 输出缓冲，构造时一次分配。
    output: Vec<f32>,
    stats: EngineStats,
    pending_mute: Option<bool>,
    /// 上一次已生效的静音状态，用于幂等回执。
    applied_mute: bool,
    applied_mute_block: u64,
    /// 待生效的图切换（下一次 `run_block` 入口应用）。
    pending_switch: Option<PendingSwitch>,
    /// 当前生效图的信息。
    graph: GraphInfo,
    /// 生效代数计数器（见 [`GraphInfo::epoch`]）。
    epoch: u64,
}

impl core::fmt::Debug for SlotEngine {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SlotEngine")
            .field("config", &self.config)
            .field("stats", &self.stats)
            .field("graph", &self.graph)
            .field("switch_pending", &self.pending_switch.is_some())
            .field("mute", &self.applied_mute)
            .finish_non_exhaustive()
    }
}

impl SlotEngine {
    /// 新建执行器。输出缓冲在此一次分配完毕，之后不再分配；`processor` 是初始处理链
    /// （还没有加载方案时可传 [`Passthrough`]）。
    pub fn new(
        config: EngineConfig,
        processor: Box<dyn Processor + Send>,
    ) -> Result<Self, EngineError> {
        config.validate()?;
        let output = vec![0.0f32; config.output_samples_per_block()];
        Ok(Self {
            config,
            processor,
            retired: None,
            output,
            stats: EngineStats::default(),
            pending_mute: None,
            applied_mute: false,
            applied_mute_block: 0,
            pending_switch: None,
            graph: GraphInfo::default(),
            epoch: 0,
        })
    }

    /// 配置。
    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// 输出块样本数（= 调用方交给 `playout` 的长度）。
    pub fn output_samples_per_block(&self) -> usize {
        self.config.output_samples_per_block()
    }

    /// 统计快照。
    pub fn stats(&self) -> EngineStats {
        self.stats.clone()
    }

    /// 已应用图快照（未加载方案时各字段为 `None`）。
    pub fn graph_info(&self) -> GraphInfo {
        self.graph.clone()
    }

    /// 是否有已 stage、尚未在块边界生效的图切换。
    pub fn switch_pending(&self) -> bool {
        self.pending_switch.is_some()
    }

    /// 排入一张新图，返回它将被应用的块号（= 下一块）。
    ///
    /// 只登记，不打断当前块；下一次 [`SlotEngine::run_block`] 入口原子换上。连续 stage 且中间
    /// 没有跑块时，以最后一次为准（前一张还没生效的新图被丢弃）。释放上一次换图退役的旧图也在
    /// 这里发生，因此只允许在控制/管理线程调用。
    pub fn stage_processor(
        &mut self,
        processor: Box<dyn Processor + Send>,
        version: u64,
        graph_id: String,
    ) -> u64 {
        // 释放上一次换图退役的旧图：这是控制线程，允许析构（绝不在 run_block 里做）。
        if let Some(retired) = self.retired.take() {
            let Retired {
                processor,
                graph_id,
            } = retired;
            drop(processor);
            drop(graph_id);
        }
        let staged_block = self.stats.blocks + 1;
        // 上一次 stage 的新图若还没被块边界应用就被覆盖：它从未跑到音频线程，
        // 在这里（控制线程）随 Option 替换直接释放，是安全的。
        self.pending_switch = Some(PendingSwitch {
            processor,
            version,
            graph_id,
        });
        self.stats.pending_commands += 1;
        staged_block
    }

    /// 处理一块：校验长度、应用待生效命令（含块边界换图）、调用处理链或静音。
    ///
    /// 实时约束：不分配、不加锁、不做 IO、不打印。
    /// 换图语义：pending 的图在本块入口换上，**本块从头就用新图处理**（`applied_block` 即本块）；
    /// stage 之后、本块之前的状态快照仍报告旧图，不谎报。
    pub fn run_block(&mut self, input: &[f32]) -> Result<&[f32], EngineError> {
        let expected = self.config.input_samples_per_block();
        if input.len() != expected {
            // 块没有开始：待生效命令保持挂起，留给下一个合法块。
            return Err(EngineError::WrongBlockLength {
                expected,
                actual: input.len(),
            });
        }
        let block_number = self.stats.blocks + 1;
        if let Some(pending) = self.pending_switch.take() {
            // 块边界换图：本块从头就用新图。
            // 旧图与旧 graph_id 移入退役槽（只做移动，不分配也不析构），由下一次 stage 释放。
            let previous_processor = core::mem::replace(&mut self.processor, pending.processor);
            let previous_graph_id = self.graph.graph_id.replace(pending.graph_id);
            self.retired = Some(Retired {
                processor: previous_processor,
                graph_id: previous_graph_id,
            });
            self.epoch += 1;
            self.graph.epoch = Some(self.epoch);
            self.graph.version = Some(pending.version);
            self.graph.applied_block = Some(block_number);
        }
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
        Ok(&self.output)
    }

    /// 请求静音/取消静音；在下一块生效（与 [`BlockEngine::request_mute`] 同一语义）。
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

    /// 设备配置视图（供后端打开时使用）。
    pub fn device_config(&self, device: crate::owner::DeviceId) -> DeviceConfig {
        DeviceConfig {
            device,
            sample_rate: self.config.sample_rate,
            block_frames: self.config.block_frames,
            input_channels: self.config.input_channels,
            output_channels: self.config.output_channels,
        }
    }
}
