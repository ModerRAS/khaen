//! 抖动缓冲（**RT 核心**）：预分配固定帧环 + 绝对媒体帧号 + 原子游标 SPSC。
//!
//! 数据流：IO 线程（writer）把 RTP 序号/时间戳/L24 解码后写入环形样本区；RT/capture
//! 线程（reader）按媒体时钟推进的**绝对帧号**整块取出（固定 `block_frames` = 128）。
//!
//! ## 并发模型
//! - 单 producer（IO 线程）/单 consumer（capture/RT 线程），无锁：`written_upto`
//!   （producer 写、consumer 读）+ `read_frame`（consumer 写、producer 读）+ 原子
//!   统计；帧有效位图（`AtomicU64` words）支撑乱序包补齐与连续水位推进。
//! - 样本区 `UnsafeCell<Box<[i32]>>` 由上述游标隔离：producer 只写 `[read_frame,
//!   read_frame+capacity)` 内的帧，consumer 只读 `written_upto` 已确认连续的帧。
//! - 热路径零分配：`push_packet`/`read_block` 只用预分配缓冲与原子，无锁、无系统
//!   调用、无格式化/日志、无 panic 路径（错误全部计数后返回）。
//!
//! ## 播放策略（固定延迟，时钟驱动）
//! - 第一包到达时刻 = 时钟原点 `origin_ns`，其 RTP 时间戳 = 帧 0；
//! - 第 f 帧的播放时刻 = `origin_ns + latency + frames_to_ns(f)`（`latency` =
//!   目标抖动容限，默认 4 ms，向上取整到整块）；
//! - reader 每块计算 `target = floor((elapsed - latency)/block)*block`：
//!   数据水位 ≥ target+block → 整块输出；否则**整块静音**（underrun++，时钟不倒退，
//!   不半块、不阻塞）；target 已越过 writer 水位（调度停顿）→ 跳帧并计数。
//! - 过期帧（`frame < read_frame`）由 producer 丢弃并计数，不覆盖消费者已读区。
//!
//! ## 流重启
//! SSRC 变化或时间戳跳变 > 10 s 视为新流：writer 清空环 + 重置锚点/游标并递增
//! `epoch`；reader 在下一个 `read_block` 检测 epoch 变化后按新原点继续（best-effort
//! resync，见 `StreamRestart` 计数）。

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use crate::clock::{ns_to_frames, MediaClock};
use crate::rtp::{self, Header};

/// 默认 capture 块长（48k / 128 帧 = 2.667 ms）。
pub const DEFAULT_BLOCK_FRAMES: usize = 128;
/// 默认环容量（帧）：8192 帧 ≈ 170.7 ms @48k（8 ms×8=… 足够覆盖网络突发）。
pub const DEFAULT_RING_CAPACITY_FRAMES: usize = 8_192;
/// 环容量最小值（必须 ≥ 4 block，且为 2 的幂）。
pub const MIN_RING_CAPACITY_FRAMES: usize = 512;
/// 单流最大通道数（AES67 媒体面）。
pub const MAX_CHANNELS: usize = 8;
/// 默认目标抖动容限（µs）：4 ms（AES67 常用 1 ms ptime 下约 2–4 ms 缓冲）。
pub const DEFAULT_LATENCY_US: u32 = 4_000;
/// 时间戳跳变阈值（秒）：超过视为新流并 reanchor。
pub const TS_JUMP_RESET_SEC: u32 = 10;

/// 抖动缓冲配置（启动期固定；构建后不可变）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JitterConfig {
    /// 通道数（1..=8）。
    pub channels: u16,
    /// 媒体采样率（48k）。
    pub rate_hz: u32,
    /// 固定块长（帧；core-v1 允许 64/128，capture 原型固定 128）。
    pub block_frames: usize,
    /// 环容量（帧；2 的幂，≥4 block）。
    pub capacity_frames: usize,
    /// 目标抖动容限（µs；向上取整到整块）。
    pub latency_us: u32,
}

impl JitterConfig {
    /// 构造并校验（启动期调用；热路径不调用）。
    pub fn new(
        channels: u16,
        rate_hz: u32,
        block_frames: usize,
        capacity_frames: usize,
        latency_us: u32,
    ) -> Result<Self, JitterError> {
        let config = Self {
            channels,
            rate_hz,
            block_frames,
            capacity_frames,
            latency_us,
        };
        config.validate()?;
        Ok(config)
    }

    /// AES67 默认配置（8ch / 48k / 128 帧 / 8192 帧环 / 4 ms）。
    pub fn aes67_default(channels: u16) -> Result<Self, JitterError> {
        Self::new(
            channels,
            crate::AES67_RATE_HZ,
            DEFAULT_BLOCK_FRAMES,
            DEFAULT_RING_CAPACITY_FRAMES,
            DEFAULT_LATENCY_US,
        )
    }

    /// 校验配置（通道/速率/块/容量/延迟）。
    pub fn validate(&self) -> Result<(), JitterError> {
        if self.channels == 0 || self.channels as usize > MAX_CHANNELS {
            return Err(JitterError::Config);
        }
        if self.rate_hz == 0 {
            return Err(JitterError::Config);
        }
        if self.block_frames == 0 || self.block_frames > self.capacity_frames {
            return Err(JitterError::Config);
        }
        if !self.capacity_frames.is_power_of_two() {
            return Err(JitterError::Config);
        }
        if self.capacity_frames < MIN_RING_CAPACITY_FRAMES
            || self.capacity_frames < self.block_frames * 4
        {
            return Err(JitterError::Config);
        }
        if self.latency_us == 0 {
            return Err(JitterError::Config);
        }
        Ok(())
    }

    /// 目标延迟帧数（latency_us → 帧，向上取整到整块）。
    pub fn latency_frames(&self) -> u64 {
        let frames = ns_to_frames(self.latency_us as u64 * 1_000, self.rate_hz);
        let block = self.block_frames as u64;
        if frames == 0 {
            block
        } else {
            ((frames + block - 1) / block) * block
        }
    }

    /// 每通道样本数 = 环容量 × 通道数。
    fn total_samples(&self) -> usize {
        self.capacity_frames * self.channels as usize
    }
}

/// 抖动缓冲配置/布局/缓冲错误（Copy；稳定错误名）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JitterError {
    /// 配置非法（通道/速率/块长/容量/延迟）。
    Config,
    /// 输出缓冲小于 block_frames × channels。
    BufferTooSmall,
    /// 单包帧数超过块长（ptime > block）。
    PacketTooLarge,
}

impl JitterError {
    /// 稳定错误名。
    pub const fn name(self) -> &'static str {
        match self {
            JitterError::Config => "ERR_JITTER_CONFIG",
            JitterError::BufferTooSmall => "ERR_JITTER_BUFFER_TOO_SMALL",
            JitterError::PacketTooLarge => "ERR_JITTER_PACKET_TOO_LARGE",
        }
    }
}

/// 一包写入结果（Copy；热路径返回，不分配）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushResult {
    /// 已写入上界内的帧。
    Accepted {
        /// 本包帧数。
        frames: usize,
    },
    /// 包内帧已全部越过播放游标（过期丢弃）。
    Late {
        /// 丢弃帧数。
        frames: usize,
    },
    /// 帧号超出环窗口（producer 过快/consumer 停顿；丢弃并计数）。
    Overflow {
        /// 丢弃帧数。
        frames: usize,
    },
    /// 重复序号（丢当前包）。
    Duplicate,
    /// payload 不是整帧 / 通道数不符。
    Malformed,
    /// SSRC 变化/时间戳大跳变后已重建流锚点并写入本包。
    Restarted {
        /// 本包帧数。
        frames: usize,
    },
}

impl PushResult {
    /// 稳定名称（证据/日志用）。
    pub const fn name(self) -> &'static str {
        match self {
            PushResult::Accepted { .. } => "accepted",
            PushResult::Late { .. } => "late",
            PushResult::Overflow { .. } => "overflow",
            PushResult::Duplicate => "duplicate",
            PushResult::Malformed => "malformed",
            PushResult::Restarted { .. } => "restarted",
        }
    }
}

/// 一次 `read_block` 的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockStatus {
    /// 正常输出整块；
    Ok,
    /// 尚未锚定（首包未到）→ 输出静音。
    Warmup,
    /// 播放窗口内数据不足 → 输出整块静音并计数。
    Underrun,
    /// 时钟尚未推进到下一块（调用过快/时钟未走）→ 输出静音、不重复消费。
    NotDue,
}

impl BlockStatus {
    /// 稳定名称。
    pub const fn name(self) -> &'static str {
        match self {
            BlockStatus::Ok => "ok",
            BlockStatus::Warmup => "warmup",
            BlockStatus::Underrun => "underrun",
            BlockStatus::NotDue => "not_due",
        }
    }
}

/// `read_block` 返回值。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockRead {
    /// 状态。
    pub status: BlockStatus,
    /// 本次有效输出帧数（Ok = block_frames；其余 = 0，缓冲已置零为 block_frames 静音）。
    pub frames: usize,
}

/// 抖动缓冲计数快照（Copy；非阻塞读取，任何线程可调）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JitterStats {
    /// 收到并通过 RTP 头校验的包数（含 malformed payload 之外）。
    pub packets: u64,
    /// 写入环的帧数。
    pub frames_received: u64,
    /// 输出到 capture 的帧数。
    pub frames_played: u64,
    /// 播放窗口数据不足次数（每次 = 一整块静音）。
    pub underruns: u64,
    /// 未锚定（首包未到）导致的静音块数。
    pub warmups: u64,
    /// 时钟未推进到下一块的调用次数（诊断；不计欠载）。
    pub not_due: u64,
    /// 环窗口溢出丢弃的包数。
    pub overruns: u64,
    /// 过期（帧号已越过播放游标）丢弃的包数。
    pub late_packets: u64,
    /// 过期丢弃的帧数。
    pub late_frames: u64,
    /// 序号间隙估算的丢包数。
    pub lost_packets: u64,
    /// 重复序号包数。
    pub duplicate_packets: u64,
    /// 乱序（旧序号）包数。
    pub out_of_order_packets: u64,
    /// 时间戳不连续（与序号推算不符）次数。
    pub timestamp_jumps: u64,
    /// 畸形 payload 次数。
    pub malformed_packets: u64,
    /// 流重启（SSRC 变化/大跳变）次数。
    pub stream_restarts: u64,
    /// 调度停顿导致的跳帧块数。
    pub resync_blocks: u64,
    /// 环内最大占用（帧；诊断延迟/缓冲压力）。
    pub high_water_frames: u64,
}

/// 原子统计（writer/reader 共用；读取不加锁、不阻塞）。
#[derive(Debug, Default)]
struct Stats {
    packets: AtomicU64,
    frames_received: AtomicU64,
    frames_played: AtomicU64,
    underruns: AtomicU64,
    warmups: AtomicU64,
    not_due: AtomicU64,
    overruns: AtomicU64,
    late_packets: AtomicU64,
    late_frames: AtomicU64,
    lost_packets: AtomicU64,
    duplicate_packets: AtomicU64,
    out_of_order_packets: AtomicU64,
    timestamp_jumps: AtomicU64,
    malformed_packets: AtomicU64,
    stream_restarts: AtomicU64,
    resync_blocks: AtomicU64,
    high_water_frames: AtomicU64,
}

impl Stats {
    fn snapshot(&self) -> JitterStats {
        use Ordering::Relaxed;
        JitterStats {
            packets: self.packets.load(Relaxed),
            frames_received: self.frames_received.load(Relaxed),
            frames_played: self.frames_played.load(Relaxed),
            underruns: self.underruns.load(Relaxed),
            warmups: self.warmups.load(Relaxed),
            not_due: self.not_due.load(Relaxed),
            overruns: self.overruns.load(Relaxed),
            late_packets: self.late_packets.load(Relaxed),
            late_frames: self.late_frames.load(Relaxed),
            lost_packets: self.lost_packets.load(Relaxed),
            duplicate_packets: self.duplicate_packets.load(Relaxed),
            out_of_order_packets: self.out_of_order_packets.load(Relaxed),
            timestamp_jumps: self.timestamp_jumps.load(Relaxed),
            malformed_packets: self.malformed_packets.load(Relaxed),
            stream_restarts: self.stream_restarts.load(Relaxed),
            resync_blocks: self.resync_blocks.load(Relaxed),
            high_water_frames: self.high_water_frames.load(Relaxed),
        }
    }
}

/// writer/reader 共享状态（SPSC：单 producer + 单 consumer）。
struct Shared {
    channels: usize,
    rate_hz: u32,
    block_frames: usize,
    capacity_frames: u64,
    mask: u64,
    latency_frames: u64,
    /// 环样本区（交错 i32，满量程）。仅由 SPSC 游标隔离访问（见模块头）。
    samples: std::cell::UnsafeCell<Box<[i32]>>,
    /// 帧有效位图（word = frame/64），支撑乱序补齐。
    valid: Box<[AtomicU64]>,
    /// 已写入的连续水位（绝对帧号；consumer 读到该值即保证 [read_frame, w) 可读）。
    written_upto: AtomicU64,
    /// 下一个待消费的绝对帧号。
    read_frame: AtomicU64,
    /// 是否已由首包锚定时间轴。
    anchored: AtomicBool,
    /// 第一包到达时的时钟 ns。
    origin_ns: AtomicU64,
    /// 第一包的 RTP 时间戳（帧 0 基准）。
    ts0: AtomicU32,
    reanchor_req: AtomicBool,
    /// 当前流的 SSRC。
    ssrc: AtomicU32,
    /// 流纪元（reanchor 时 +1；reader 用于检测流重启）。
    epoch: AtomicU32,
    /// 最近接收序号（u32::MAX = 无）。
    seq_last: AtomicU32,
    /// 最近接收时间戳（u32::MAX = 无）。
    last_ts: AtomicU32,
    stats: Stats,
}

// SAFETY: Shared 只通过 JitterWriter（单 producer 线程）与 JitterReader（单 consumer
// 线程）访问。samples 的访问由 SPSC 游标隔离：
// - producer 只写绝对帧 [read_frame, read_frame+capacity) 内的样本（push 前后原子
//   读取 read_frame；写前 frame_start ≥ read_frame 由窗口检查保证）；
// - consumer 只读 [read_frame, written_upto) 内的样本（written_upto 是 producer 在
//   样本写入后用 Release 发布的水位；consumer 用 Acquire 读取；reorder 由 valid
//   bitset 的 AcqRel RMW 保证）；
// - 两者不会同时触碰同一帧位置：consumer 读的帧一定 < written_upto（producer 已写完），
//   producer 写的帧一定 ≥ read_frame（consumer 尚未读），环回绕窗口由 capacity 检查
//   隔离（producer 在 frame ≥ read_frame+capacity 时丢弃）。
// 其余字段均为原子或构造后只读。调用方必须遵守 SPSC 约束（每方向最多一个线程）。
unsafe impl Sync for Shared {}
unsafe impl Send for Shared {}

impl fmt::Debug for Shared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JitterShared")
            .field("channels", &self.channels)
            .field("rate_hz", &self.rate_hz)
            .field("block_frames", &self.block_frames)
            .field("capacity_frames", &self.capacity_frames)
            .field("latency_frames", &self.latency_frames)
            .field("anchored", &self.anchored.load(Ordering::Relaxed))
            .field("read_frame", &self.read_frame.load(Ordering::Relaxed))
            .field("written_upto", &self.written_upto.load(Ordering::Relaxed))
            .finish()
    }
}

impl Shared {
    fn word_index(&self, frame: u64) -> usize {
        ((frame & self.mask) >> 6) as usize
    }

    fn bit_mask(&self, frame: u64) -> u64 {
        1u64 << ((frame & self.mask) & 63)
    }

    fn bit_is_set(&self, frame: u64) -> bool {
        let word = self.valid[self.word_index(frame)].load(Ordering::Acquire);
        word & self.bit_mask(frame) != 0
    }

    /// 置位 [start, start+frames) 的有效位（word 级 RMW；帧数 ≤ 一个 ptime）。
    fn mark_valid(&self, start: u64, frames: u64) {
        let mut remaining = frames;
        let mut frame = start;
        while remaining > 0 {
            let pos = frame & self.mask;
            let word_index = (pos >> 6) as usize;
            let bit_off = (pos & 63) as u32;
            let word_bits = 64 - bit_off as u64;
            let in_word = remaining.min(word_bits).min(self.capacity_frames - pos);
            let mask = if in_word == 64 {
                u64::MAX
            } else {
                ((1u64 << in_word) - 1) << bit_off
            };
            self.valid[word_index].fetch_or(mask, Ordering::AcqRel);
            frame += in_word;
            remaining -= in_word;
        }
    }

    /// 清除 [start, start+frames) 的有效位（consumer 读走或跳过时调用）。
    fn clear_valid(&self, start: u64, frames: u64) {
        let mut remaining = frames;
        let mut frame = start;
        while remaining > 0 {
            let pos = frame & self.mask;
            let word_index = (pos >> 6) as usize;
            let bit_off = (pos & 63) as u32;
            let word_bits = 64 - bit_off as u64;
            let in_word = remaining.min(word_bits).min(self.capacity_frames - pos);
            let mask = if in_word == 64 {
                u64::MAX
            } else {
                ((1u64 << in_word) - 1) << bit_off
            };
            self.valid[word_index].fetch_and(!mask, Ordering::AcqRel);
            frame += in_word;
            remaining -= in_word;
        }
    }

    /// 清空整个环（流重启；启动期/异常路径，不在稳态热路径）。
    fn clear_all(&self) {
        for word in self.valid.iter() {
            word.store(0, Ordering::Relaxed);
        }
    }

    /// 由连续有效位推进 `written_upto`（producer 在写完后调用）。
    fn advance_watermark(&self) {
        let read = self.read_frame.load(Ordering::Acquire);
        let mut written = self.written_upto.load(Ordering::Relaxed).max(read);
        let limit = read.saturating_add(self.capacity_frames);
        while written < limit && self.bit_is_set(written) {
            written += 1;
        }
        self.written_upto.store(written, Ordering::Release);
    }

    fn update_high_water(&self, frames: u64) {
        use Ordering::Relaxed;
        let mut current = self.stats.high_water_frames.load(Relaxed);
        while frames > current {
            match self
                .stats
                .high_water_frames
                .compare_exchange_weak(current, frames, Relaxed, Relaxed)
            {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }

    fn epoch(&self) -> u32 {
        self.epoch.load(Ordering::Acquire)
    }
}

/// 构造一对 writer/reader（启动期一次性分配；返回的两个句柄可跨线程 move）。
pub fn channel(config: JitterConfig) -> Result<(JitterWriter, JitterReader), JitterError> {
    config.validate()?;
    let channels = config.channels as usize;
    let shared = Arc::new(Shared {
        channels,
        rate_hz: config.rate_hz,
        block_frames: config.block_frames,
        capacity_frames: config.capacity_frames as u64,
        mask: config.capacity_frames as u64 - 1,
        latency_frames: config.latency_frames(),
        samples: std::cell::UnsafeCell::new(vec![0i32; config.total_samples()].into_boxed_slice()),
        valid: (0..config.capacity_frames / 64)
            .map(|_| AtomicU64::new(0))
            .collect(),
        written_upto: AtomicU64::new(0),
        read_frame: AtomicU64::new(0),
        anchored: AtomicBool::new(false),
        origin_ns: AtomicU64::new(0),
        ts0: AtomicU32::new(0),
        reanchor_req: AtomicBool::new(false),
        ssrc: AtomicU32::new(0),
        epoch: AtomicU32::new(0),
        seq_last: AtomicU32::new(u32::MAX),
        last_ts: AtomicU32::new(u32::MAX),
        stats: Stats::default(),
    });
    Ok((
        JitterWriter {
            shared: Arc::clone(&shared),
        },
        JitterReader {
            shared,
            epoch_seen: 0,
        },
    ))
}

/// 抖动缓冲写端（**单 producer 线程**：接收 IO 线程）。
#[derive(Clone, Debug)]
pub struct JitterWriter {
    shared: Arc<Shared>,
}

impl JitterWriter {
    /// 目标延迟帧数。
    pub fn latency_frames(&self) -> u64 {
        self.shared.latency_frames
    }

    /// 请求"内部重锚"：下一个到达的包被当作新流首包（重置 ts0/游标/水位）。
    /// 用于流间隙后新会话 SSRC 未变/时间戳复位的场景：接收端可在进程内恢复，不必重启进程。
    pub fn request_reanchor(&self) {
        self.shared.reanchor_req.store(true, Ordering::Release);
    }

    /// 热路径：按 RTP 头 + L24 payload 收一包（解码直接写入环，零分配）。
    ///
    /// `now_ns` 由 IO 线程从 `MediaClock` 读到（仅首包用作时间轴原点）。
    pub fn push_packet(&self, now_ns: u64, header: &Header, payload: &[u8]) -> PushResult {
        let channels = self.shared.channels as u16;
        let frames = match rtp::frames_in_payload(payload.len(), channels, rtp::L24_SAMPLE_BYTES) {
            Ok(frames) => frames,
            Err(_) => {
                self.shared
                    .stats
                    .malformed_packets
                    .fetch_add(1, Ordering::Relaxed);
                return PushResult::Malformed;
            }
        };
        self.accept(now_ns, header, frames, |index| {
            rtp::unpack_l24_sample(payload, index)
        })
    }

    /// 测试/适配器用：按 RTP 头 + 交错满量程 i32 样本收一包（无 L24 解码，语义同
    /// `push_packet`）。
    pub fn push_interleaved(&self, now_ns: u64, header: &Header, samples: &[i32]) -> PushResult {
        let channels = self.shared.channels;
        if channels == 0 || samples.len() % channels != 0 {
            self.shared
                .stats
                .malformed_packets
                .fetch_add(1, Ordering::Relaxed);
            return PushResult::Malformed;
        }
        let frames = samples.len() / channels;
        self.accept(now_ns, header, frames, |index| samples[index])
    }

    /// 内部验收路径（RTP 序号/时间戳/窗口 + 写入）。
    fn accept<F>(&self, now_ns: u64, header: &Header, frames: usize, sample_at: F) -> PushResult
    where
        F: Fn(usize) -> i32,
    {
        let shared = &self.shared;
        if frames == 0 {
            shared
                .stats
                .malformed_packets
                .fetch_add(1, Ordering::Relaxed);
            return PushResult::Malformed;
        }
        let frames_u64 = frames as u64;
        if frames_u64 > shared.capacity_frames {
            shared.stats.overruns.fetch_add(1, Ordering::Relaxed);
            return PushResult::Overflow { frames };
        }
        shared.stats.packets.fetch_add(1, Ordering::Relaxed);

        // 流切换（SSRC 变化）：重建锚点。
        let previous_ssrc = shared.ssrc.load(Ordering::Acquire);
        let mut restarted = false;
        // 内部重锚请求：把本包当作新流首包（用于流间隙后新会话的进程内恢复）
        if shared.reanchor_req.swap(false, Ordering::AcqRel) {
            self.reanchor(now_ns, header);
            restarted = true;
        }
        if shared.anchored.load(Ordering::Acquire) && previous_ssrc != header.ssrc {
            self.reanchor(now_ns, header);
            shared.stats.stream_restarts.fetch_add(1, Ordering::Relaxed);
            restarted = true;
        }
        if !shared.anchored.load(Ordering::Acquire) {
            self.anchor(now_ns, header);
        }

        // 序号：重复/丢包/乱序统计。
        let seq_last = shared.seq_last.load(Ordering::Acquire);
        if seq_last != u32::MAX {
            let last = seq_last as u16;
            let delta = header.sequence.wrapping_sub(last);
            if delta == 0 {
                shared
                    .stats
                    .duplicate_packets
                    .fetch_add(1, Ordering::Relaxed);
                return PushResult::Duplicate;
            } else if delta < 0x8000 {
                if delta > 1 {
                    shared
                        .stats
                        .lost_packets
                        .fetch_add((delta - 1) as u64, Ordering::Relaxed);
                }
                // 时间戳连续性（按探测到的序号步长推算）。
                let expected = shared
                    .last_ts
                    .load(Ordering::Relaxed)
                    .wrapping_add((delta as u32).wrapping_mul(frames as u32));
                if shared.last_ts.load(Ordering::Relaxed) != u32::MAX
                    && header.timestamp != expected
                {
                    shared.stats.timestamp_jumps.fetch_add(1, Ordering::Relaxed);
                }
                shared
                    .seq_last
                    .store(header.sequence as u32, Ordering::Release);
                shared.last_ts.store(header.timestamp, Ordering::Release);
            } else {
                shared
                    .stats
                    .out_of_order_packets
                    .fetch_add(1, Ordering::Relaxed);
            }
        } else {
            shared
                .seq_last
                .store(header.sequence as u32, Ordering::Release);
            shared.last_ts.store(header.timestamp, Ordering::Release);
        }

        // 时间戳 → 绝对帧号（帧 0 = 首包时间戳）。
        //
        // 跳变判定必须相对**上一包时间戳**，不能相对 ts0 的绝对时长：
        // 旧实现 `|timestamp-ts0| > 10s` 会把任何持续超过 10 s 的正常流在 ~10 s 处
        // 误判为"时间戳大跳变"并重锚，形成 **每 10.0 s 一次 24 ms 断音**（现场实测
        // 23 次/230 s、间隔 10.000–10.004 s 无漂移，而 pcap 中 SSRC/seq/ts 连续）。
        let last_ts = shared.last_ts.load(Ordering::Acquire);
        let rate = shared.rate_hz as i64;
        if last_ts != u32::MAX {
            let step = header.timestamp.wrapping_sub(last_ts) as i32 as i64;
            if step.abs() > rate * TS_JUMP_RESET_SEC as i64 {
                // 真实时间戳大跳变（>10 s，前后向）：按新流重建。
                self.reanchor(now_ns, header);
                shared.stats.stream_restarts.fetch_add(1, Ordering::Relaxed);
                restarted = true;
            }
        }
        let ts0 = shared.ts0.load(Ordering::Acquire);
        let frame_offset = header.timestamp.wrapping_sub(ts0) as i32 as i64;
        if frame_offset < 0 {
            shared.stats.late_packets.fetch_add(1, Ordering::Relaxed);
            shared
                .stats
                .late_frames
                .fetch_add(frames_u64, Ordering::Relaxed);
            return PushResult::Late { frames };
        }
        let frame_start = frame_offset as u64;
        let read_frame = shared.read_frame.load(Ordering::Acquire);
        if frame_start.saturating_add(frames_u64) <= read_frame {
            shared.stats.late_packets.fetch_add(1, Ordering::Relaxed);
            shared
                .stats
                .late_frames
                .fetch_add(frames_u64, Ordering::Relaxed);
            return PushResult::Late { frames };
        }
        if frame_start >= read_frame.saturating_add(shared.capacity_frames) {
            shared.stats.overruns.fetch_add(1, Ordering::Relaxed);
            return PushResult::Overflow { frames };
        }
        if frame_start < read_frame {
            // 部分过期：整包丢弃（不写半包，避免覆盖已消费帧）。
            shared.stats.late_packets.fetch_add(1, Ordering::Relaxed);
            shared
                .stats
                .late_frames
                .fetch_add(frames_u64, Ordering::Relaxed);
            return PushResult::Late { frames };
        }

        // 写入样本（直接解码进环；SPSC 安全性见 Shared 的 SAFETY 注释）。
        let channels = shared.channels;
        let mask = shared.mask;
        // SAFETY: 本线程是唯一 producer；写入帧范围 [frame_start, frame_start+frames)
        // 已按窗口检查 ≥ read_frame 且 < read_frame+capacity，consumer 不会读这些帧
        // （见 Shared SAFETY 注释）。
        let samples = unsafe { &mut *shared.samples.get() };
        for frame in 0..frames {
            let pos = ((frame_start + frame as u64) & mask) as usize;
            let base = pos * channels;
            let sample_base = frame * channels;
            for channel in 0..channels {
                samples[base + channel] = sample_at(sample_base + channel);
            }
        }
        shared.mark_valid(frame_start, frames_u64);
        shared.advance_watermark();
        shared
            .stats
            .frames_received
            .fetch_add(frames_u64, Ordering::Relaxed);
        let written = shared.written_upto.load(Ordering::Relaxed);
        let read = shared.read_frame.load(Ordering::Relaxed);
        shared.update_high_water(written.saturating_sub(read));
        if restarted {
            PushResult::Restarted { frames }
        } else {
            PushResult::Accepted { frames }
        }
    }

    /// 以首包锚定时间轴（producer 单线程，无需 CAS）。
    fn anchor(&self, now_ns: u64, header: &Header) {
        let shared = &self.shared;
        if shared.anchored.load(Ordering::Acquire) {
            return;
        }
        shared.origin_ns.store(now_ns, Ordering::Relaxed);
        shared.ts0.store(header.timestamp, Ordering::Relaxed);
        shared.ssrc.store(header.ssrc, Ordering::Relaxed);
        shared.seq_last.store(u32::MAX, Ordering::Relaxed);
        shared.last_ts.store(u32::MAX, Ordering::Relaxed);
        shared.epoch.fetch_add(1, Ordering::AcqRel);
        shared.anchored.store(true, Ordering::Release);
    }

    /// 重建流锚点（SSRC 变化/时间戳大跳变；异常路径，允许固定大小清环）。
    fn reanchor(&self, now_ns: u64, header: &Header) {
        let shared = &self.shared;
        shared.anchored.store(false, Ordering::Release);
        shared.clear_all();
        shared.written_upto.store(0, Ordering::Relaxed);
        shared.read_frame.store(0, Ordering::Relaxed);
        shared.origin_ns.store(now_ns, Ordering::Relaxed);
        shared.ts0.store(header.timestamp, Ordering::Relaxed);
        shared.ssrc.store(header.ssrc, Ordering::Relaxed);
        shared.seq_last.store(u32::MAX, Ordering::Relaxed);
        shared.last_ts.store(u32::MAX, Ordering::Relaxed);
        shared.epoch.fetch_add(1, Ordering::AcqRel);
        shared.anchored.store(true, Ordering::Release);
    }

    /// 统计快照（非阻塞）。
    pub fn stats(&self) -> JitterStats {
        self.shared.stats.snapshot()
    }

    /// 是否已由首包锚定。
    pub fn is_anchored(&self) -> bool {
        self.shared.anchored.load(Ordering::Acquire)
    }
}

/// 抖动缓冲读端（**单 consumer 线程**：capture/RT 线程）。
#[derive(Clone, Debug)]
pub struct JitterReader {
    shared: Arc<Shared>,
    epoch_seen: u32,
}

impl JitterReader {
    /// 通道数。
    pub fn channels(&self) -> usize {
        self.shared.channels
    }

    /// 固定块长（帧）。
    pub fn block_frames(&self) -> usize {
        self.shared.block_frames
    }

    /// 目标延迟帧数。
    pub fn latency_frames(&self) -> u64 {
        self.shared.latency_frames
    }

    /// 媒体速率。
    pub fn rate_hz(&self) -> u32 {
        self.shared.rate_hz
    }

    /// 是否已锚定。
    pub fn is_anchored(&self) -> bool {
        self.shared.anchored.load(Ordering::Acquire)
    }

    /// 首包锚定时的时钟 ns（未锚定 = None）。
    pub fn origin_ns(&self) -> Option<u64> {
        if self.is_anchored() {
            Some(self.shared.origin_ns.load(Ordering::Acquire))
        } else {
            None
        }
    }

    /// 当前可读的连续帧数（水位 - 播放游标；诊断/测试等待用）。
    pub fn available_frames(&self) -> u64 {
        let written = self.shared.written_upto.load(Ordering::Acquire);
        let read = self.shared.read_frame.load(Ordering::Acquire);
        written.saturating_sub(read)
    }

    /// 当前播放游标（下一个待消费帧号；诊断用）。
    pub fn read_frame(&self) -> u64 {
        self.shared.read_frame.load(Ordering::Acquire)
    }

    /// 统计快照（非阻塞；热路径外调用）。
    pub fn stats(&self) -> JitterStats {
        self.shared.stats.snapshot()
    }

    /// 流纪元（reanchor 时递增；诊断用）。
    pub fn epoch(&self) -> u32 {
        self.shared.epoch()
    }

    /// 热路径：按媒体时钟取一个固定块（交错 i32，满量程）。
    ///
    /// `now_ns` 来自 `MediaClock`（每块读一次）。缓冲不足时 `out[..block*channels]`
    /// 被写成整块静音并计数；**不分配、不阻塞、不持锁**。
    pub fn read_block(&mut self, now_ns: u64, out: &mut [i32]) -> Result<BlockRead, JitterError> {
        let shared = &self.shared;
        let channels = shared.channels;
        let block = shared.block_frames;
        let required = block * channels;
        if out.len() < required {
            return Err(JitterError::BufferTooSmall);
        }
        let out = &mut out[..required];
        out.fill(0);

        let epoch = shared.epoch();
        if epoch != self.epoch_seen {
            // 流重启：投递时间轴由 writer 重建（read_frame 已被重置为 0）。
            self.epoch_seen = epoch;
        }
        if !shared.anchored.load(Ordering::Acquire) {
            shared.stats.warmups.fetch_add(1, Ordering::Relaxed);
            return Ok(BlockRead {
                status: BlockStatus::Warmup,
                frames: 0,
            });
        }
        let origin = shared.origin_ns.load(Ordering::Acquire);
        if now_ns <= origin {
            shared.stats.warmups.fetch_add(1, Ordering::Relaxed);
            return Ok(BlockRead {
                status: BlockStatus::Warmup,
                frames: 0,
            });
        }
        let elapsed = ns_to_frames(now_ns - origin, shared.rate_hz);
        let latency = shared.latency_frames;
        let block_u64 = block as u64;
        let target = elapsed.saturating_sub(latency) / block_u64 * block_u64;
        let mut read_frame = shared.read_frame.load(Ordering::Acquire);
        if target < read_frame {
            shared.stats.not_due.fetch_add(1, Ordering::Relaxed);
            return Ok(BlockRead {
                status: BlockStatus::NotDue,
                frames: 0,
            });
        }
        if target > read_frame {
            // 调度停顿/时钟快进：跳帧（不阻塞、不补历史）。
            let skipped = target - read_frame;
            let cleared = skipped.min(shared.capacity_frames);
            shared.clear_valid(read_frame, cleared);
            shared
                .stats
                .resync_blocks
                .fetch_add(skipped / block_u64, Ordering::Relaxed);
            read_frame = target;
            shared.read_frame.store(read_frame, Ordering::Release);
            shared.written_upto.fetch_max(read_frame, Ordering::AcqRel);
        }
        let end = read_frame.saturating_add(block_u64);
        let written = shared.written_upto.load(Ordering::Acquire);
        if written < end {
            // 欠载：整块静音，播放游标照常前进（时钟不倒退）。
            shared.stats.underruns.fetch_add(1, Ordering::Relaxed);
            shared.clear_valid(read_frame, block_u64);
            shared.read_frame.store(end, Ordering::Release);
            shared.written_upto.fetch_max(end, Ordering::AcqRel);
            return Ok(BlockRead {
                status: BlockStatus::Underrun,
                frames: 0,
            });
        }
        // 正常整块输出（written_upto 已 Release 发布；Acquire 读保证样本可见）。
        let mask = shared.mask;
        // SAFETY: 本线程是唯一 consumer；读取帧范围 [read_frame, read_frame+block)
        // 全部 < written_upto（producer 已写完并发布），producer 不会写这些帧
        // （它只写 ≥ read_frame 且本函数在读完前不推进 read_frame 越过该范围）。
        let samples = unsafe { &*shared.samples.get() };
        for frame in 0..block {
            let pos = ((read_frame + frame as u64) & mask) as usize;
            let src = pos * channels;
            let dst = frame * channels;
            out[dst..dst + channels].copy_from_slice(&samples[src..src + channels]);
        }
        shared.clear_valid(read_frame, block_u64);
        shared.read_frame.store(end, Ordering::Release);
        shared
            .stats
            .frames_played
            .fetch_add(block_u64, Ordering::Relaxed);
        let available = written.saturating_sub(end);
        shared.update_high_water(available);
        Ok(BlockRead {
            status: BlockStatus::Ok,
            frames: block,
        })
    }

    /// [NONRT 诊断] 用给定媒体时钟循环调用 `read_block` 的便捷封装（测试/证据）。
    pub fn read_block_with_clock<C: MediaClock>(
        &mut self,
        clock: &C,
        out: &mut [i32],
    ) -> Result<BlockRead, JitterError> {
        self.read_block(clock.now_ns(), out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::frames_to_ns;

    fn header(seq: u16, timestamp: u32, ssrc: u32) -> Header {
        Header {
            version: 2,
            padding: false,
            extension: false,
            csrc_count: 0,
            marker: false,
            payload_type: 96,
            sequence: seq,
            timestamp,
            ssrc,
        }
    }

    /// 生成 frames×channels 个满量程 i32 样本（按帧号/通道编码，便于校验）。
    fn make_samples(frames: usize, channels: usize, start_frame: u64) -> Vec<i32> {
        let mut samples = vec![0i32; frames * channels];
        for frame in 0..frames {
            for channel in 0..channels {
                samples[frame * channels + channel] =
                    ((start_frame + frame as u64) as i32) * 10 + channel as i32;
            }
        }
        samples
    }

    fn push_frames(
        writer: &JitterWriter,
        now_ns: u64,
        seq: u16,
        timestamp: u32,
        ssrc: u32,
        frames: usize,
        channels: usize,
        start_frame: u64,
    ) -> PushResult {
        let samples = make_samples(frames, channels, start_frame);
        writer.push_interleaved(now_ns, &header(seq, timestamp, ssrc), &samples)
    }

    /// 时钟推进到 target 块（加 1ns 避免 floor 误差）。
    fn block_time(origin: u64, latency_frames: u64, block: usize, index: u64) -> u64 {
        origin + frames_to_ns(latency_frames + index * block as u64, 48_000) + 1_000
    }

    #[test]
    fn jitter_order_reads_exact_frames_and_counts() {
        let config = JitterConfig::aes67_default(2).expect("config");
        let (writer, mut reader) = channel(config).expect("channel");
        let origin = 10_000u64;
        // 1 ms 包（48 帧），4 包 = 192 帧。
        for index in 0..4u16 {
            let result = push_frames(
                &writer,
                origin,
                index,
                index as u32 * 48,
                7,
                48,
                2,
                index as u64 * 48,
            );
            assert!(
                matches!(result, PushResult::Accepted { frames } if frames == 48),
                "{result:?}"
            );
        }
        let latency = reader.latency_frames();
        assert_eq!(latency, 256); // 4ms=192 帧 → 向上取整到 256
        let mut out = vec![0i32; 128 * 2];
        let mut played = 0u64;
        for index in 0..4u64 {
            let now = block_time(origin, latency, 128, index);
            let read = reader.read_block(now, &mut out).expect("read");
            // 前两块数据齐全（192 帧只够 1.5 块）；断言基于实际状态。
            match read.status {
                BlockStatus::Ok => {
                    for frame in 0..128usize {
                        for channel in 0..2usize {
                            let expected = ((played + frame as u64) as i32) * 10 + channel as i32;
                            assert_eq!(
                                out[frame * 2 + channel],
                                expected,
                                "block {index} frame {frame} ch {channel}"
                            );
                        }
                    }
                    played += 128;
                }
                BlockStatus::Underrun => {
                    assert!(out.iter().all(|&sample| sample == 0));
                }
                other => panic!("unexpected status {other:?}"),
            }
        }
        let stats = reader.stats();
        assert_eq!(stats.frames_played, played);
        assert_eq!(stats.packets, 4);
        assert_eq!(stats.frames_received, 192);
        assert!(stats.high_water_frames > 0);
    }

    #[test]
    fn jitter_underrun_emits_block_silence_and_counts() {
        let config = JitterConfig::aes67_default(2).expect("config");
        let (writer, mut reader) = channel(config).expect("channel");
        let origin = 1_000u64;
        push_frames(&writer, origin, 0, 0, 5, 48, 2, 0);
        let mut out = vec![9i32; 128 * 2];
        let latency = reader.latency_frames();
        let read = reader
            .read_block(block_time(origin, latency, 128, 0), &mut out)
            .expect("read");
        assert_eq!(read.status, BlockStatus::Underrun);
        assert_eq!(read.frames, 0);
        assert!(out.iter().all(|&sample| sample == 0));
        assert_eq!(reader.stats().underruns, 1);
        // 播放游标已推进一个块。
        assert_eq!(reader.read_frame(), 128);
    }

    #[test]
    fn jitter_late_duplicate_and_overflow_counters() {
        let config = JitterConfig::aes67_default(2).expect("config");
        let (writer, mut reader) = channel(config).expect("channel");
        let origin = 5_000u64;
        for index in 0..4u16 {
            push_frames(
                &writer,
                origin,
                index,
                index as u32 * 48,
                11,
                48,
                2,
                index as u64 * 48,
            );
        }
        let latency = reader.latency_frames();
        let mut out = vec![0i32; 256];
        // 读 2 块，游标到 256。
        for index in 0..2u64 {
            let _ = reader
                .read_block(block_time(origin, latency, 128, index), &mut out)
                .unwrap();
        }
        assert_eq!(reader.read_frame(), 256);
        // 重复序号（与 seq=3 同）→ Duplicate。
        assert_eq!(
            push_frames(&writer, origin, 3, 3 * 48, 11, 48, 2, 3 * 48),
            PushResult::Duplicate
        );
        // 过期时间戳（帧 0 已越过游标 256）→ Late。
        let late = push_frames(&writer, origin, 4, 0, 11, 48, 2, 0);
        assert!(matches!(late, PushResult::Late { frames } if frames == 48));
        // 帧号超出窗口（read 256 + capacity 8192 = 8448；ts 9600 越界但 < 10s 跳变）→ Overflow。
        let far = writer.push_interleaved(origin, &header(5, 9_600, 11), &make_samples(48, 2, 0));
        assert!(matches!(far, PushResult::Overflow { frames } if frames == 48));
        let stats = reader.stats();
        assert_eq!(stats.duplicate_packets, 1);
        assert_eq!(stats.late_packets, 1);
        assert_eq!(stats.late_frames, 48);
        assert_eq!(stats.overruns, 1);
    }

    #[test]
    fn jitter_reorder_fills_hole_before_watermark() {
        let config = JitterConfig::aes67_default(2).expect("config");
        let (writer, mut reader) = channel(config).expect("channel");
        let origin = 100u64;
        // 首包 seq0/ts0 锚定（帧 0..48）。
        push_frames(&writer, origin, 0, 0, 13, 48, 2, 0);
        assert_eq!(reader.available_frames(), 48);
        // 乱序：seq2（帧 96..144）先到，水位不动（帧 48..96 缺失）。
        push_frames(&writer, origin, 2, 96, 13, 48, 2, 96);
        assert_eq!(reader.available_frames(), 48);
        // seq1 补齐空洞 → 水位推进到 144 帧。
        push_frames(&writer, origin, 1, 48, 13, 48, 2, 48);
        assert_eq!(reader.available_frames(), 144);
        assert_eq!(reader.stats().out_of_order_packets, 1);
        // 144 帧足以整块输出第 0 块（128 帧）。
        let origin_ns = reader.origin_ns().expect("anchored");
        let now = origin_ns + frames_to_ns(reader.latency_frames(), 48_000) + 1_000;
        let mut block = vec![0i32; 128 * 2];
        let read = reader.read_block(now, &mut block).expect("read");
        assert_eq!(read.status, BlockStatus::Ok);
        assert_eq!(block[0], 0);
        assert_eq!(block[1], 1);
        // make_samples 编码 = 帧号×10 + 通道号：帧 127 = 1270/1271。
        assert_eq!(block[254], 1_270);
        assert_eq!(block[255], 1_271);
    }

    #[test]
    fn jitter_reanchor_on_ssrc_change() {
        let config = JitterConfig::aes67_default(2).expect("config");
        let (writer, reader) = channel(config).expect("channel");
        let origin = 2_000u64;
        push_frames(&writer, origin, 0, 0, 21, 48, 2, 0);
        assert!(writer.is_anchored());
        // 新 SSRC → Restarted（epoch 递增、锚点重建）。
        let epoch_before = reader.epoch();
        let result = push_frames(&writer, origin + 1_000_000, 0, 0, 22, 48, 2, 0);
        assert!(matches!(result, PushResult::Restarted { frames } if frames == 48));
        assert_eq!(reader.epoch(), epoch_before + 1);
        let stats = reader.stats();
        assert_eq!(stats.stream_restarts, 1);
    }

    /// 持续 11 s 的正常流（1 ms 包、ts 恒 +48、SSRC 不变）**不得**触发重锚。
    /// 回归防护：旧实现用 |ts-ts0|>10s 判定，会在 10 s 处误重锚（现场 10 s 周期卡顿根因）。
    #[test]
    fn jitter_long_continuous_stream_does_not_reanchor() {
        let config = JitterConfig::aes67_default(2).expect("config");
        let (writer, mut reader) = channel(config).expect("channel");
        let origin = 5_000u64;
        let latency = reader.latency_frames();
        let block = 128usize;
        let mut out = vec![0i32; block * 2];
        let mut played_blocks = 0u64;
        let packets: u32 = 11_000; // 11 s @ 1 ms
        for index in 0..packets {
            let ts = index * 48;
            let result = push_frames(
                &writer,
                origin,
                (index % 65_536) as u16,
                ts,
                7,
                48,
                2,
                index as u64 * 48,
            );
            assert!(
                matches!(result, PushResult::Accepted { frames } if frames == 48),
                "packet {index}: {result:?}"
            );
            while reader.available_frames() >= block as u64 {
                let now = block_time(origin, latency, block, played_blocks);
                let read = reader.read_block(now, &mut out).expect("read");
                assert_eq!(read.status, BlockStatus::Ok, "block {played_blocks}");
                played_blocks += 1;
            }
        }
        let stats = reader.stats();
        assert_eq!(stats.stream_restarts, 0, "持续 11 s 正常流不得重锚");
        assert_eq!(stats.timestamp_jumps, 0, "连续 ts 步进不应算跳变");
        assert_eq!(stats.frames_received, packets as u64 * 48);
        assert!(played_blocks >= 4_000, "应已消费大部分数据，got {played_blocks}");
    }

    #[test]
    fn jitter_config_validation() {
        assert_eq!(
            JitterConfig::new(0, 48_000, 128, 8192, 4_000).unwrap_err(),
            JitterError::Config
        );
        assert_eq!(
            JitterConfig::new(9, 48_000, 128, 8192, 4_000).unwrap_err(),
            JitterError::Config
        );
        assert_eq!(
            JitterConfig::new(8, 48_000, 128, 1000, 4_000).unwrap_err(),
            JitterError::Config
        );
        assert_eq!(
            JitterConfig::new(8, 48_000, 128, 8192, 0).unwrap_err(),
            JitterError::Config
        );
        let config = JitterConfig::aes67_default(8).unwrap();
        assert_eq!(config.latency_frames(), 256);
        assert_eq!(JitterError::Config.name(), "ERR_JITTER_CONFIG");
    }

    #[test]
    fn jitter_not_due_and_buffer_too_small() {
        let config = JitterConfig::aes67_default(1).unwrap();
        let (writer, mut reader) = channel(config).unwrap();
        let _ = &writer;
        let origin = 50u64;
        push_frames(&writer, origin, 0, 0, 1, 48, 1, 0);
        let mut out = vec![0i32; 128];
        let small = reader.read_block(block_time(origin, 256, 128, 0), &mut out[..64]);
        assert_eq!(small.unwrap_err(), JitterError::BufferTooSmall);
        let latency = reader.latency_frames();
        let first = reader
            .read_block(block_time(origin, latency, 128, 0), &mut out)
            .unwrap();
        // 只有 48 帧 → 欠载。
        assert_eq!(first.status, BlockStatus::Underrun);
        let again = reader
            .read_block(block_time(origin, latency, 128, 0), &mut out)
            .unwrap();
        assert_eq!(again.status, BlockStatus::NotDue);
    }
}
