//! 多路 AES67 输入总线（IO I3a）：每路独立抖动环/ASRC，按各自的 channels_map 稀疏混入
//! 同一条帧交错输入总线。
//!
//! # 混音语义（与总方案 IO/混音段一致）
//!
//! - 每路只写自己 `channels_map` 指定的槽位，未映射槽位是 0；
//! - 多个流写到同一槽位时按样本相加（f64 累加，最后转 f32），**不是** last-wins；
//! - 断流/欠载/未锚定的流按贡献 0 处理：不 gate 其它流，也不断开总线；
//! - 不自动归一化、不加 limiter；|sum| > 1.0 的样本计数上报（供上层 headroom/clip meter），
//!   写入值保持未缩放，由调用方决定增益/保护；
//! - 所有缓冲在构造期分配，`feed_packet` / `fill_block` 不分配、不加锁、不做 IO、不 panic。
//!
//! 本模块只做“多流 → 一条输入总线”；把总线接到 Runtime/rt-host/daemon 的配置面属于后续切片。

use crate::aes67_input::{
    Aes67Input, Aes67InputConfig, Aes67InputStats, BlockStatus, FeedOutcome, InputError,
};
use khaen_asrc::Interpolation;

/// 一条输入总线最多容纳的 AES67 流数（内存/诊断预算；不是协议上限）。
pub const MAX_BUS_STREAMS: usize = 16;

/// 多流输入总线错误（稳定名字供证据/日志；不参与 RT 决策）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusError {
    /// 总线配置不自洽（空流表/宽度不一致/块长不一致/超流数上限）。
    BadConfig(&'static str),
    /// 流下标越界。
    StreamOutOfRange {
        /// 请求的流下标。
        index: usize,
        /// 总线里的流数。
        streams: usize,
    },
    /// 某一路的 Aes67Input 错误（配置/feed）。
    Stream {
        /// 流下标。
        index: usize,
        /// 原始错误。
        error: InputError,
    },
}

impl std::fmt::Display for BusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadConfig(message) => write!(f, "input bus config: {message}"),
            Self::StreamOutOfRange { index, streams } => {
                write!(
                    f,
                    "input bus stream index {index} is out of range ({streams} streams)"
                )
            }
            Self::Stream { index, error } => {
                write!(f, "input bus stream {index}: {}", error.name())
            }
        }
    }
}

impl std::error::Error for BusError {}

impl BusError {
    /// 稳定错误名。
    pub const fn name(self) -> &'static str {
        match self {
            Self::BadConfig(_) => "bad_config",
            Self::StreamOutOfRange { .. } => "stream_out_of_range",
            Self::Stream { .. } => "stream",
        }
    }
}

/// 一条总线的块输出结果（Copy；每块返回，不分配）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BusBlockOutcome {
    /// 总线流数。
    pub streams: usize,
    /// 本块真正有数据的流数。
    pub filled_streams: usize,
    /// 本块处于预热（未锚定/未达预填水位）的流数。
    pub warmup_streams: usize,
    /// 本块处于欠载的流数。
    pub underrun_streams: usize,
    /// 本块有效媒体帧数（至少一路 Filled 时 = block_frames，否则 0）。
    pub frames: usize,
    /// 本块 |sum| > 1.0 的样本数（未缩放写入）。
    pub over_range_samples: usize,
}

/// 总线累计统计（控制面快照；会分配 Vec，不能在 RT 路径调用）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Aes67InputBusStats {
    /// 已 fill 的块数。
    pub blocks: u64,
    /// 至少一路有数据的块数。
    pub filled_blocks: u64,
    /// 全静音块数（所有流 warmup/underrun）。
    pub silent_blocks: u64,
    /// 累计 |sum| > 1.0 的样本数。
    pub over_range_samples: u64,
    /// 每路 Aes67Input 的累计统计（长度 = 流数）。
    pub streams: Vec<Aes67InputStats>,
}

/// 多路 AES67 输入总线。
#[derive(Debug)]
pub struct Aes67InputBus {
    /// 输入宽度（帧交错声道数）；所有流一致。
    output_channels: usize,
    /// 块长（帧）；所有流一致。
    block_frames: usize,
    /// 每路独立抖动环。
    inputs: Vec<Aes67Input>,
    /// 单路 fill_block 的暂存块（复用；长度 = block_frames × output_channels）。
    scratch: Vec<f32>,
    /// f64 累加器（长度 = block_frames × output_channels）。
    mix: Vec<f64>,
    /// 累计计数（只在块路径做原子般的普通 u64 自增；由唯一所有者串行调用）。
    blocks: u64,
    filled_blocks: u64,
    silent_blocks: u64,
    over_range_samples: u64,
}

impl Aes67InputBus {
    /// 用默认 ASRC 策略（读 `KHAEN_ASRC`，与单路输入一致）构造总线。
    pub fn new(streams: Vec<Aes67InputConfig>) -> Result<Self, BusError> {
        let inputs = streams
            .iter()
            .enumerate()
            .map(|(index, config)| {
                Aes67Input::new(config.clone()).map_err(|error| BusError::Stream { index, error })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::from_inputs(streams, inputs)
    }

    /// 显式指定 ASRC 插值方式构造总线（测试/需要绕过环境变量的接线用）。
    pub fn new_with_asrc(
        streams: Vec<Aes67InputConfig>,
        interp: Option<Interpolation>,
    ) -> Result<Self, BusError> {
        let inputs = streams
            .iter()
            .enumerate()
            .map(|(index, config)| {
                Aes67Input::new_with_asrc(config.clone(), interp)
                    .map_err(|error| BusError::Stream { index, error })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::from_inputs(streams, inputs)
    }

    fn from_inputs(
        streams: Vec<Aes67InputConfig>,
        inputs: Vec<Aes67Input>,
    ) -> Result<Self, BusError> {
        let Some(first) = streams.first() else {
            return Err(BusError::BadConfig("input bus needs at least one stream"));
        };
        if streams.len() > MAX_BUS_STREAMS {
            return Err(BusError::BadConfig("input bus exceeds MAX_BUS_STREAMS"));
        }
        let output_channels = first.output_channels;
        let block_frames = first.block_frames;
        for (index, config) in streams.iter().enumerate() {
            if config.output_channels != output_channels {
                return Err(BusError::BadConfig(
                    "all streams must use the same output_channels (input bus width)",
                ));
            }
            if config.block_frames != block_frames {
                return Err(BusError::BadConfig(
                    "all streams must use the same block_frames",
                ));
            }
            // Aes67Input::new 已逐路校验；这里再保证 map 的槽位不越界（防御未来配置来源绕过）。
            if config
                .channels_map
                .iter()
                .any(|slot| usize::from(*slot) >= output_channels)
            {
                return Err(BusError::Stream {
                    index,
                    error: InputError::BadConfig("channels_map slot must be < output_channels"),
                });
            }
        }
        let samples = output_channels
            .checked_mul(block_frames)
            .ok_or(BusError::BadConfig("input bus block size overflows usize"))?;
        Ok(Self {
            output_channels,
            block_frames,
            inputs,
            scratch: vec![0.0; samples],
            mix: vec![0.0; samples],
            blocks: 0,
            filled_blocks: 0,
            silent_blocks: 0,
            over_range_samples: 0,
        })
    }

    /// 输入总线宽度（帧交错声道数）。
    pub fn output_channels(&self) -> usize {
        self.output_channels
    }

    /// 块长（帧）。
    pub fn block_frames(&self) -> usize {
        self.block_frames
    }

    /// 每块样本数 = block_frames × output_channels。
    pub fn samples_per_block(&self) -> usize {
        self.blocks_frames_samples()
    }

    fn blocks_frames_samples(&self) -> usize {
        self.block_frames * self.output_channels
    }

    /// 流数。
    pub fn stream_count(&self) -> usize {
        self.inputs.len()
    }

    /// 某一路的只读输入视图（诊断/测试；不要与块线程并发访问）。
    pub fn stream_input(&self, index: usize) -> Option<&Aes67Input> {
        self.inputs.get(index)
    }

    /// 喂一个 RTP 包给指定流。
    ///
    /// 实时约束：不分配、不加锁、不做 IO、不 panic。
    pub fn feed_packet(&mut self, stream: usize, packet: &[u8]) -> Result<FeedOutcome, BusError> {
        let Some(input) = self.inputs.get_mut(stream) else {
            return Err(BusError::StreamOutOfRange {
                index: stream,
                streams: self.inputs.len(),
            });
        };
        input.feed_packet(packet).map_err(|error| BusError::Stream {
            index: stream,
            error,
        })
    }

    /// 输出一块混音后的帧交错 f32（长度必须 = samples_per_block）。
    ///
    /// 每个流先独立 fill_block（未映射槽为 0，欠载/预热整块为 0），再按样本相加。
    /// 所有流都无数据时输出静音但照常返回（不 gate、不 panic）。
    pub fn fill_block(&mut self, out: &mut [f32]) -> Result<BusBlockOutcome, BusError> {
        let expected = self.blocks_frames_samples();
        if out.len() != expected {
            return Err(BusError::BadConfig(
                "output buffer length must equal block_frames * output_channels",
            ));
        }
        self.mix.fill(0.0);
        let mut filled = 0usize;
        let mut warmup = 0usize;
        let mut underrun = 0usize;
        for (index, input) in self.inputs.iter_mut().enumerate() {
            let outcome = input.fill_block(&mut self.scratch);
            match outcome.status {
                BlockStatus::Filled => {
                    filled += 1;
                    for (mix, sample) in self.mix.iter_mut().zip(self.scratch.iter()) {
                        *mix += f64::from(*sample);
                    }
                }
                BlockStatus::Warmup => warmup += 1,
                BlockStatus::Underrun => underrun += 1,
                BlockStatus::WrongOutputLength { .. } => {
                    // scratch 长度由构造期固定，不可能走到这里；按配置错误上报而不是静默。
                    return Err(BusError::Stream {
                        index,
                        error: InputError::BadConfig(
                            "stream fill_block reported a wrong output length",
                        ),
                    });
                }
            }
        }
        let mut over = 0usize;
        for (slot, sum) in out.iter_mut().zip(self.mix.iter()) {
            let value = *sum as f32;
            if value.abs() > 1.0 {
                over += 1;
            }
            *slot = value;
        }
        self.blocks = self.blocks.saturating_add(1);
        if filled > 0 {
            self.filled_blocks = self.filled_blocks.saturating_add(1);
        } else {
            self.silent_blocks = self.silent_blocks.saturating_add(1);
        }
        self.over_range_samples = self.over_range_samples.saturating_add(over as u64);
        Ok(BusBlockOutcome {
            streams: self.inputs.len(),
            filled_streams: filled,
            warmup_streams: warmup,
            underrun_streams: underrun,
            frames: if filled > 0 { self.block_frames } else { 0 },
            over_range_samples: over,
        })
    }

    /// 累计统计快照（分配 Vec；控制面/测试用，不能在 RT 路径调用）。
    pub fn stats(&self) -> Aes67InputBusStats {
        Aes67InputBusStats {
            blocks: self.blocks,
            filled_blocks: self.filled_blocks,
            silent_blocks: self.silent_blocks,
            over_range_samples: self.over_range_samples,
            streams: self.inputs.iter().map(Aes67Input::stats).collect(),
        }
    }
}
