//! [NONRT] 固定 buffer block source：文件输入 -> host 可拉取的 `B x N` 输入块。
//!
//! WBS T14 要求“导出供 host 接实际播放的固定 buffer source 接口（不直接占 ALSA）”。
//! 本模块把一个已解码的输入（[crate::wav::DecodedAudio]）变成**确定性**的固定 block 序列：
//! T26 host 在播放线程/SPSC 生产端按 MOTU 播放时轴逐块拉取，块大小、尾部零填充与
//! block 对齐语义与离线 [crate::runner] **同源**（runner 自身也用本类型驱动），因此
//! “离线 run 的输入切分”与“host 实播的输入切分”逐位一致。
//!
//! # 语义（与 core-v1 §6.3 P-08 / T03 SPEC §3.3 一致）
//!
//! 1. 输入尾部补 `flush_zeros = L_F - 1` 个零（滤波器 group delay 的因果尾），
//!    再补零到 `block_size` 整数倍；
//! 2. 第 i 块的帧区间是 `[i*B, (i+1)*B)`；落在输入区间内的样本取输入值，其余为 `+0.0`；
//! 3. 总块数 = `ceil((input_frames + flush_zeros) / B)`；两者都为 0 时 0 块；
//! 4. 每块为 **planar**（channel-major，长度恒为 `N * B`），与 core
//!    `DirectTopology` 的输入缓冲布局一致；没有 stride、没有隐式重采样/上混/下混。
//!
//! # 明确边界
//!
//! - 不打开/不占用 ALSA，不建线程，不做 RT 准入；调用方（T26 host）负责把块搬进
//!   有界 SPSC 并按播放时轴驱动 DAC；
//! - 不做格式转换（输入已由 [crate::wav] 解码为 f32）；解码期 clip/nonfinite 统计在
//!   [crate::wav::DecodeStats] 里，本模块不重复计数；
//! - 不改变图/保护语义：identity 保护与 mute 由 [crate::graph]/[crate::runner] 决定，
//!   source 只负责“喂什么输入块”。

use crate::graph::PreparedSession;
use crate::wav::DecodedAudio;
use crate::{RunnerError, RunnerResult, E_FILE_CAPACITY, E_FILE_INTERNAL};

/// 一块输入块的元数据（固定缓冲接口；不参与图版本）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockChunk {
    /// 块序号（0 起，跨 reset 重新计数）。
    pub block_index: u64,
    /// 本块在 `input_frames + flush_zeros` 时间轴上的起始帧。
    pub frame_start: usize,
    /// 本块有效帧数（`<= block_size`；末块通常 < block_size）。
    pub frames_valid: usize,
    /// 本块中来自输入文件的帧数。
    pub input_frames: usize,
    /// 本块中因 flush/对齐补零的帧数（`frames_valid - input_frames`）。
    pub flush_frames: usize,
    /// 本块是否含输入结束后的零（flush 块/首块之后的补零块）。
    pub is_flush: bool,
    /// 本块是否时间轴最后一块（之后 [BlockSource::next_block] 返回 `None`）。
    pub is_last: bool,
}

/// 固定 block 输入源（借用已解码输入，不复制样本）。
#[derive(Clone, Debug)]
pub struct BlockSource<'a> {
    planar: &'a [f32],
    n_src: usize,
    input_frames: usize,
    block_size: usize,
    flush_zeros: usize,
    total_frames: usize,
    blocks_total: u64,
    next_block: u64,
}

impl<'a> BlockSource<'a> {
    /// 构造固定 block 源。
    ///
    /// `block_size` 必须 > 0（运行时只准入 64/128，由 session 校验）；`flush_zeros` 通常为
    /// `L_F - 1`（[crate::graph::ExecutableGraph::flush_zeros]）。
    pub fn new(input: &'a DecodedAudio, block_size: u16, flush_zeros: u32) -> RunnerResult<Self> {
        if block_size == 0 {
            return Err(RunnerError::new(E_FILE_INTERNAL, "block_size is zero"));
        }
        let n_src = input.channels();
        if n_src == 0 {
            return Err(RunnerError::new(E_FILE_INTERNAL, "input has no channels"));
        }
        let input_frames = input.frames();
        if input.planar.len() != n_src * input_frames {
            return Err(RunnerError::new(
                E_FILE_INTERNAL,
                format!(
                    "input planar len {} != channels*frames {}",
                    input.planar.len(),
                    n_src * input_frames
                ),
            ));
        }
        let total_frames = input_frames
            .checked_add(flush_zeros as usize)
            .ok_or_else(|| RunnerError::new(E_FILE_CAPACITY, "source total_frames overflow"))?;
        let block = block_size as usize;
        let blocks_total = if total_frames == 0 {
            0
        } else {
            ((total_frames + block - 1) / block) as u64
        };
        Ok(Self {
            planar: &input.planar,
            n_src,
            input_frames,
            block_size: block,
            flush_zeros: flush_zeros as usize,
            total_frames,
            blocks_total,
            next_block: 0,
        })
    }

    /// 从已 prepare 的 session 构造（host 入口）：block 大小与 flush 长度取自实际执行图；
    /// mute 保护图没有执行图时 flush=0。
    pub fn from_prepared(prepared: &'a PreparedSession) -> RunnerResult<Self> {
        let (block_size, flush_zeros) = match prepared.graph.as_ref() {
            Some(graph) => (graph.block_size, graph.flush_zeros),
            None => (prepared.session.engine.block_size, 0),
        };
        Self::new(&prepared.input, block_size, flush_zeros)
    }

    /// 源通道数 N。
    pub fn channels(&self) -> usize {
        self.n_src
    }

    /// 块大小 B。
    pub fn block_size(&self) -> usize {
        self.block_size
    }

    /// 输入文件帧数（不含 flush）。
    pub fn input_frames(&self) -> usize {
        self.input_frames
    }

    /// flush 零长度（= L_F - 1）。
    pub fn flush_zeros(&self) -> usize {
        self.flush_zeros
    }

    /// 时间轴总帧数（= input_frames + flush_zeros）。
    pub fn total_frames(&self) -> usize {
        self.total_frames
    }

    /// 总块数。
    pub fn blocks_total(&self) -> u64 {
        self.blocks_total
    }

    /// 尚未产出的块数。
    pub fn blocks_remaining(&self) -> u64 {
        self.blocks_total - self.next_block
    }

    /// 是否已到 EOF。
    pub fn is_exhausted(&self) -> bool {
        self.next_block >= self.blocks_total
    }

    /// 回到第一块（同源可重复播放；不改变样本）。
    pub fn reset(&mut self) {
        self.next_block = 0;
    }

    /// 拉取下一块，写入调用方预分配的 planar 缓冲（长度必须 = `N * B`）。
    ///
    /// 返回 `Ok(None)` 表示 EOF（一块都不写）；返回的 [BlockChunk] 描述本块有效帧/是否
    /// flush/是否末块。整块总是被完整写入（尾部补 `+0.0`），调用方无需自己清零。
    pub fn next_block(&mut self, out: &mut [f32]) -> RunnerResult<Option<BlockChunk>> {
        let expected = self.n_src * self.block_size;
        if out.len() != expected {
            return Err(RunnerError::new(
                E_FILE_CAPACITY,
                format!("block buffer len {} != N*B {}", out.len(), expected),
            ));
        }
        if self.is_exhausted() {
            return Ok(None);
        }
        let block_index = self.next_block;
        let base = (block_index as usize) * self.block_size;
        let valid = (self.total_frames - base).min(self.block_size);
        let input_end = self.input_frames.min(base + valid);
        let input_valid = input_end.saturating_sub(base);
        for channel in 0..self.n_src {
            let src_base = channel * self.input_frames;
            let dst_base = channel * self.block_size;
            for offset in 0..self.block_size {
                let frame = base + offset;
                let value = if offset < input_valid {
                    self.planar[src_base + frame]
                } else {
                    0.0
                };
                out[dst_base + offset] = value;
            }
        }
        self.next_block += 1;
        Ok(Some(BlockChunk {
            block_index,
            frame_start: base,
            frames_valid: valid,
            input_frames: input_valid,
            flush_frames: valid - input_valid,
            is_flush: input_valid < valid,
            is_last: self.next_block >= self.blocks_total,
        }))
    }
}
