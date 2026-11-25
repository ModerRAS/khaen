//! capture 固定块源（[NONRT 适配]）：抖动缓冲 → host 图输入的固定 128 帧块。
//!
//! 与 rt-host 的 capture 输入契约对齐：consumer 每块调用一次，输出**交错 i32（满量程）**
//! + 状态（Ok/Warmup/Underrun/NotDue），由 host 适配层再转 planar f32 + 角色映射
//! （见 `rt-host/src/aes67_rx.rs`）。
//!
//! - `read_block` 是 RT 热路径：只调用 `JitterReader::read_block` + 计数更新（普通
//!   `&mut self` 字段），无分配/无锁/无 syscall；
//! - 欠载/预热/未到期都输出整块静音（0），由调用方按策略处理（T43 失流语义：停滞
//!   整块静音、持续失流 fail closed）；
//! - 支持块长 64/128（core-v1 BLOCK_SIZES）；默认 128。

use crate::clock::MediaClock;
use crate::jitter::{BlockRead, BlockStatus, JitterError, JitterReader, JitterStats};

/// host 允许的 capture 块长（core-v1 §3.2：64/128）。
pub const SUPPORTED_BLOCK_FRAMES: [usize; 2] = [64, 128];

/// capture 块源错误。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureError {
    /// 抖动缓冲错误（缓冲过小等）。
    Jitter(JitterError),
    /// 块长不在 64/128 合同内。
    UnsupportedBlock(usize),
}

impl CaptureError {
    /// 稳定错误名。
    pub const fn name(self) -> &'static str {
        match self {
            CaptureError::Jitter(_) => "ERR_CAPTURE_JITTER",
            CaptureError::UnsupportedBlock(_) => "ERR_CAPTURE_BLOCK_SIZE",
        }
    }
}

/// capture 块源计数（consumer 单线程普通字段；非阻塞）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureCounters {
    /// `read_block` 调用次数。
    pub blocks: u64,
    /// 正常块数。
    pub ok_blocks: u64,
    /// 未锚定（首包未到）静音块数。
    pub warmup_blocks: u64,
    /// 欠载静音块数。
    pub underrun_blocks: u64,
    /// 时钟未到期块数（诊断）。
    pub not_due_blocks: u64,
    /// 交付的音频帧数（仅 Ok 块计入）。
    pub frames_delivered: u64,
}

/// 固定块 capture 源（独占 `JitterReader`；单 consumer 线程）。
#[derive(Debug)]
pub struct CaptureBlockSource {
    reader: JitterReader,
    block_frames: usize,
    channels: usize,
    rate_hz: u32,
    counters: CaptureCounters,
}

impl CaptureBlockSource {
    /// 用抖动缓冲读端构造（块长取 reader 配置；必须是 64/128）。
    pub fn new(reader: JitterReader) -> Result<Self, CaptureError> {
        let block_frames = reader.block_frames();
        if !SUPPORTED_BLOCK_FRAMES.contains(&block_frames) {
            return Err(CaptureError::UnsupportedBlock(block_frames));
        }
        Ok(Self {
            block_frames,
            channels: reader.channels(),
            rate_hz: reader.rate_hz(),
            reader,
            counters: CaptureCounters::default(),
        })
    }

    /// 块长（帧）。
    pub fn block_frames(&self) -> usize {
        self.block_frames
    }

    /// 通道数。
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// 每块交错样本数（= block × channels）。
    pub fn block_samples(&self) -> usize {
        self.block_frames * self.channels
    }

    /// 媒体速率。
    pub fn rate_hz(&self) -> u32 {
        self.rate_hz
    }

    /// capture 计数快照。
    pub fn counters(&self) -> CaptureCounters {
        self.counters
    }

    /// 抖动缓冲统计快照。
    pub fn jitter_stats(&self) -> JitterStats {
        self.reader.stats()
    }

    /// 首包锚定时刻（虚拟时钟 ns；未锚定 None）。
    pub fn origin_ns(&self) -> Option<u64> {
        self.reader.origin_ns()
    }

    /// 当前播放游标（下一个待消费帧号；诊断/时基重同步用）。
    pub fn read_frame(&self) -> u64 {
        self.reader.read_frame()
    }

    /// 流纪元（reanchor 时 +1；合成媒体时钟检测新会话/内部重锚用）。
    pub fn epoch(&self) -> u32 {
        self.reader.epoch()
    }

    /// 目标延迟帧数。
    pub fn latency_frames(&self) -> u64 {
        self.reader.latency_frames()
    }

    /// 当前可读连续帧数（水位 − 播放游标；测试/诊断用）。
    pub fn available_frames(&self) -> u64 {
        self.reader.available_frames()
    }

    /// 热路径：取一个固定块（交错 i32；不足/未到期返回整块静音 + 状态）。
    pub fn read_block(&mut self, now_ns: u64, out: &mut [i32]) -> Result<BlockRead, CaptureError> {
        let read = self
            .reader
            .read_block(now_ns, out)
            .map_err(CaptureError::Jitter)?;
        self.record(&read);
        Ok(read)
    }

    /// 便捷：用给定媒体时钟读一个块。
    pub fn read_block_with_clock<C: MediaClock>(
        &mut self,
        clock: &C,
        out: &mut [i32],
    ) -> Result<BlockRead, CaptureError> {
        self.read_block(clock.now_ns(), out)
    }

    /// 更新 capture 计数（单 consumer 线程，无需原子）。
    fn record(&mut self, read: &BlockRead) {
        self.counters.blocks += 1;
        match read.status {
            BlockStatus::Ok => {
                self.counters.ok_blocks += 1;
                self.counters.frames_delivered += read.frames as u64;
            }
            BlockStatus::Warmup => self.counters.warmup_blocks += 1,
            BlockStatus::Underrun => self.counters.underrun_blocks += 1,
            BlockStatus::NotDue => self.counters.not_due_blocks += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::MockClock;
    use crate::jitter::{self, JitterConfig};
    use crate::rtp::Header;

    fn header(sequence: u16, timestamp: u32) -> Header {
        Header {
            version: 2,
            padding: false,
            extension: false,
            csrc_count: 0,
            marker: false,
            payload_type: 96,
            sequence,
            timestamp,
            ssrc: 3,
        }
    }

    fn samples(frames: usize, channels: usize, start: i32) -> Vec<i32> {
        (0..frames * channels)
            .map(|index| start + index as i32)
            .collect()
    }

    #[test]
    fn capture_source_delivers_fixed_block_after_anchor() {
        let config = JitterConfig::new(2, 48_000, 128, 8_192, 4_000).unwrap();
        let (writer, reader) = jitter::channel(config).unwrap();
        let mut source = CaptureBlockSource::new(reader).expect("source");
        assert_eq!(source.block_frames(), 128);
        assert_eq!(source.channels(), 2);
        assert_eq!(source.block_samples(), 256);
        // 预写 4 包（48 帧/包 = 192 帧）。
        for index in 0..4u16 {
            let result = writer.push_interleaved(
                1_000,
                &header(index, index as u32 * 48),
                &samples(48, 2, (index as i32) * 96),
            );
            assert!(
                matches!(result, jitter::PushResult::Accepted { .. }),
                "{result:?}"
            );
        }
        let origin = source.origin_ns().expect("origin");
        let latency = source.latency_frames();
        let clock = MockClock::new(48_000);
        clock.set_ns(origin + crate::clock::frames_to_ns(latency, 48_000) + 1_000);
        let mut out = vec![0i32; 256];
        let read = source
            .read_block_with_clock(&clock, &mut out)
            .expect("read");
        assert_eq!(read.status, BlockStatus::Ok);
        assert_eq!(read.frames, 128);
        assert_eq!(out[0], 0);
        assert_eq!(out[1], 1);
        assert_eq!(out[254], 254);
        assert_eq!(out[255], 255);
        assert_eq!(source.counters().ok_blocks, 1);
        assert_eq!(source.counters().frames_delivered, 128);
        // 第二块欠载（192 帧只够一块半）。
        clock.advance_frames(128);
        let read = source
            .read_block_with_clock(&clock, &mut out)
            .expect("read2");
        assert_eq!(read.status, BlockStatus::Underrun);
        assert!(out.iter().all(|&sample| sample == 0));
        assert_eq!(source.counters().underrun_blocks, 1);
    }

    #[test]
    fn capture_source_warmup_and_not_due() {
        let config = JitterConfig::new(1, 48_000, 64, 512, 4_000).unwrap();
        let (writer, reader) = jitter::channel(config).unwrap();
        let mut source = CaptureBlockSource::new(reader).expect("source");
        assert_eq!(source.block_frames(), 64);
        let mut out = vec![0i32; 64];
        // 未锚定 → Warmup。
        let read = source.read_block(10_000, &mut out).expect("warmup");
        assert_eq!(read.status, BlockStatus::Warmup);
        assert_eq!(source.counters().warmup_blocks, 1);
        // 锚定（48 帧 < 64 帧块）→ 首块 Unterrun（欠载静音）。
        writer.push_interleaved(1_000, &header(0, 0), &samples(48, 1, 0));
        let read = source.read_block(2_000, &mut out).expect("underrun");
        assert_eq!(read.status, BlockStatus::Underrun);
        assert_eq!(source.counters().underrun_blocks, 1);
        // 时钟未推进到下一块 → NotDue（不重复消费）。
        let read = source.read_block(2_000, &mut out).expect("not due");
        assert_eq!(read.status, BlockStatus::NotDue);
        assert_eq!(source.counters().not_due_blocks, 1);
        assert_eq!(source.jitter_stats().packets, 1);
    }

    #[test]
    fn capture_source_rejects_non_contract_block() {
        let config = JitterConfig::new(2, 48_000, 32, 512, 4_000).unwrap();
        let (_writer, reader) = jitter::channel(config).unwrap();
        let error = CaptureBlockSource::new(reader).unwrap_err();
        assert_eq!(error, CaptureError::UnsupportedBlock(32));
        assert_eq!(error.name(), "ERR_CAPTURE_BLOCK_SIZE");
    }
}
