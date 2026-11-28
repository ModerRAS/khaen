//! ALSA 采集输入（IO I4a）：把真实 ALSA capture PCM 接成引擎的定长输入块。
//!
//! # 为什么单独一层
//!
//! AES67 输入是"网络收包 -> 抖动环 -> fill_block"，采集输入是"本机 PCM 设备 -> 块队列 ->
//! fill_block"：前者有 RTP/SSRC/抖动语义，后者没有。两者共用"定长块 + 稀疏 slot 映射"的
//! 输出形状（`block_frames × input_channels` 的帧交错 f32），但不硬把 ALSA 塞进 RTP
//! 结构里；"多源求和"属于后续 IO 总线任务，本层只提供一路采集源（D-007 的 mix 语义
//! 在总线层做，不在本层偷加 limiter/normalize）。
//!
//! # 线程与实时边界
//!
//! - 采集线程（非 RT）阻塞读 ALSA，把读满的块推进 SPSC；块在构造期一次性分配，运行期
//!   只搬运 `Box<[f32]>`，不分配/不释放。
//! - 块线程用 `CaptureInput::fill_block` 拉取块：无块时整块写 0 并计 underrun；积压多块
//!   时只取最新一块，旧块计为丢弃（采集不是媒体时钟主，不补播旧数据）。
//! - 采集线程从"空闲块队列"取缓冲，块线程把用完的块推回；两队列容量相同且总块数守恒，
//!   因此块线程推回永不需要分配。
//!
//! # 边界与未实现
//!
//! - 只支持 S24_3LE（与输出后端同一格式），不做采样率转换；采集设备速率必须等于方案速率，
//!   否则打开期 fail-closed。
//! - 同卡输入/输出各自打开一个 PCM 句柄，时钟由 ALSA 设备共享（同卡不跑 ASRC）；异源
//!   （例如 USB UMIK 相对 MOTU 输出）需要 ASRC，属于 IO I6，本层不假装同步。
//! - 不做设备枚举/热插拔/热重配：设备名由启动配置给出；设备掉线按 fatal 上报，由上层
//!   监督器决定重开。
//! - Windows 本机没有真实 ALSA：平台无关核心 + 假 PCM 设备测试；Linux 适配层由 CI 编译。
//!
//! # 实测 / 预测 / NOT_RUN
//!
//! - 实测：假设备下块映射、积压丢弃、欠载、xrun 恢复、线程停止都有单测（Windows）。
//! - 预测：真实 MOTU/板载 capture 在 S24_3LE + 同速率 + period=block 下能稳定出块；
//!   真实设备时钟漂移/xrun 频率需要真机数据。
//! - NOT_RUN：真实 ALSA capture 设备打开与长时间采集（需 Linux 真机/白天窗口）；
//!   N150 部署、同卡输入输出同时打开的 xrun 统计。

use core::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::alsa_backend::{PcmError, PcmFault};
use crate::engine::MAX_BLOCK_FRAMES;
use crate::ring::{self, Consumer, Producer};

/// 单路采集最多通道数（防御边界；真实 MOTU 输入远小于此）。
pub const MAX_CAPTURE_CHANNELS: usize = 64;
/// 输入总线 slot 上限（与方案 content.in_channels 的防御上限一致）。
pub const MAX_INPUT_CHANNELS: usize = 256;
/// 采集块队列与空闲块队列的最小容量（2 = 双缓冲，保证采集线程总能拿到一块）。
pub const MIN_PENDING_BLOCKS: usize = 2;
/// 采集块队列容量上限（防配置失控；每块 block_frames × channels × 4 字节）。
pub const MAX_PENDING_BLOCKS: usize = 64;
/// Linux 适配层请求的缓冲 period 数：buffer_frames = 4 × block_frames。
pub const BUFFER_PERIODS: usize = 4;

/// ALSA 采集配置（构造期固定，运行期不改）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureConfig {
    /// ALSA PCM 设备名（`hw:CARD=...,DEV=0`）；必须非空、无内嵌 NUL。
    pub device: String,
    /// 设备采集通道数（交织帧内的实际通道数）。
    pub channels: usize,
    /// 设备通道 -> 输入块 slot；长度必须等于 channels，目标 slot 必须互不重复且 < input_channels。
    pub channels_map: Vec<u16>,
    /// 输入块（引擎输入）通道数；未映射 slot 在块里恒为 0。
    pub input_channels: usize,
    /// 采集采样率（Hz）；必须与方案速率一致，不重采样。
    pub sample_rate: u32,
    /// 定长块帧数（与 EngineConfig::block_frames 一致）。
    pub block_frames: usize,
    /// 块队列/空闲块队列容量（向上取到 2 的幂；总块数守恒）。
    pub pending_blocks: usize,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            device: String::from("default"),
            channels: 2,
            channels_map: vec![0, 1],
            input_channels: 2,
            sample_rate: 48_000,
            block_frames: crate::engine::DEFAULT_BLOCK_FRAMES,
            pending_blocks: 4,
        }
    }
}

impl CaptureConfig {
    /// 校验配置；非法返回点名原因，不静默截断/回退。
    pub fn validate(&self) -> Result<(), CaptureError> {
        if self.device.is_empty() {
            return Err(CaptureError::BadConfig("device must not be empty"));
        }
        if self.device.bytes().any(|unit| unit == 0) {
            return Err(CaptureError::BadConfig("device must not contain NUL"));
        }
        if self.channels == 0 || self.channels > MAX_CAPTURE_CHANNELS {
            return Err(CaptureError::BadConfig(
                "channels must be in 1..=MAX_CAPTURE_CHANNELS",
            ));
        }
        if self.input_channels == 0 || self.input_channels > MAX_INPUT_CHANNELS {
            return Err(CaptureError::BadConfig(
                "input_channels must be in 1..=MAX_INPUT_CHANNELS",
            ));
        }
        if self.channels_map.len() != self.channels {
            return Err(CaptureError::BadConfig(
                "channels_map length must equal channels",
            ));
        }
        let mut used = 0u64;
        for &slot in &self.channels_map {
            let slot = usize::from(slot);
            if slot >= self.input_channels {
                return Err(CaptureError::BadConfig(
                    "channels_map slot must be < input_channels",
                ));
            }
            if slot < 64 {
                let bit = 1u64 << slot;
                if used & bit != 0 {
                    return Err(CaptureError::BadConfig("channels_map must be injective"));
                }
                used |= bit;
            } else if self
                .channels_map
                .iter()
                .filter(|&&other| usize::from(other) == slot)
                .count()
                > 1
            {
                return Err(CaptureError::BadConfig("channels_map must be injective"));
            }
        }
        if self.sample_rate == 0 {
            return Err(CaptureError::BadConfig("sample_rate must be positive"));
        }
        if self.block_frames == 0 || self.block_frames > MAX_BLOCK_FRAMES {
            return Err(CaptureError::BadConfig(
                "block_frames must be in 1..=MAX_BLOCK_FRAMES",
            ));
        }
        if self.pending_blocks < MIN_PENDING_BLOCKS || self.pending_blocks > MAX_PENDING_BLOCKS {
            return Err(CaptureError::BadConfig(
                "pending_blocks must be in MIN_PENDING_BLOCKS..=MAX_PENDING_BLOCKS",
            ));
        }
        self.samples_per_block()?;
        self.capacity_blocks()?;
        self.device_samples_per_block()?;
        Ok(())
    }

    /// 每块输入样本数（帧交错，长度 = block_frames × input_channels）。
    pub fn samples_per_block(&self) -> Result<usize, CaptureError> {
        self.block_frames
            .checked_mul(self.input_channels)
            .ok_or(CaptureError::BadConfig("block samples overflow"))
    }

    /// 队列实际容量（向上取到 2 的幂）；块总数 = 该值。
    pub fn capacity_blocks(&self) -> Result<usize, CaptureError> {
        let rounded = self.pending_blocks.max(1).next_power_of_two();
        if rounded > MAX_PENDING_BLOCKS {
            return Err(CaptureError::BadConfig(
                "pending_blocks rounds above MAX_PENDING_BLOCKS",
            ));
        }
        Ok(rounded)
    }

    /// 采集线程侧每块设备样本数（channels × block_frames）。
    pub fn device_samples_per_block(&self) -> Result<usize, CaptureError> {
        self.block_frames
            .checked_mul(self.channels)
            .ok_or(CaptureError::BadConfig("device block samples overflow"))
    }
}

/// 采集错误（构造/设备错误；运行期统计走 `CaptureStats`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureError {
    /// 配置非法（见 `CaptureConfig::validate`）。
    BadConfig(&'static str),
    /// 打开 ALSA 设备失败（上下文 + ALSA 描述；非实时路径允许分配）。
    Open(String),
    /// 采集线程创建失败/线程 panic。
    ThreadSpawn(String),
    /// 采集线程已经停止（停止后调用方不应再拉块）。
    Stopped,
}

impl CaptureError {
    /// 稳定错误名（证据/JSON/日志用）。
    pub const fn name(&self) -> &'static str {
        match self {
            CaptureError::BadConfig(_) => "ERR_CAPTURE_CONFIG",
            CaptureError::Open(_) => "ERR_CAPTURE_OPEN",
            CaptureError::ThreadSpawn(_) => "ERR_CAPTURE_THREAD",
            CaptureError::Stopped => "ERR_CAPTURE_STOPPED",
        }
    }
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::BadConfig(reason) => write!(f, "bad capture config: {reason}"),
            CaptureError::Open(reason) => write!(f, "open capture pcm: {reason}"),
            CaptureError::ThreadSpawn(reason) => write!(f, "spawn capture thread: {reason}"),
            CaptureError::Stopped => f.write_str("capture already stopped"),
        }
    }
}

impl std::error::Error for CaptureError {}

/// 平台无关的采集 PCM 设备（真实 ALSA 或测试假设备）。
///
/// - `PcmCapture::read` 的 `out` 是帧交错 f32；返回本次读到的帧数
///   （0 视为设备没有进展，上层计错误并停止）。
/// - xrun/挂起用 `PcmError::Recoverable` 上报，由采集线程 prepare 后重试；
///   致命故障用 `PcmError::Fatal` 上报并停止采集线程。
/// - 实现方在采集线程上运行；可以分配/IO，但不得 panic。
pub trait PcmCapture: Send {
    /// 读一批帧；返回读到的帧数。
    fn read(&mut self, out: &mut [f32]) -> Result<usize, PcmError>;

    /// xrun/挂起后的恢复（Linux：prepare/resume）。
    fn prepare(&mut self, fault: PcmFault) -> Result<(), CaptureError>;

    /// 关闭设备；幂等。
    fn close(&mut self);
}

/// 采集线程计数器（原子快照，不阻塞块线程）。
#[derive(Debug, Default)]
struct CaptureCounters {
    blocks_captured: AtomicU64,
    frames_captured: AtomicU64,
    frames_delivered: AtomicU64,
    blocks_skipped: AtomicU64,
    frames_dropped: AtomicU64,
    partial_blocks: AtomicU64,
    queue_full: AtomicU64,
    underruns: AtomicU64,
    xruns: AtomicU64,
    read_errors: AtomicU64,
    recycle_full: AtomicU64,
    running: AtomicBool,
}

/// 采集统计快照。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CaptureStats {
    /// 采集线程推进到块队列的完整块数。
    pub blocks_captured: u64,
    /// 从设备读到的总帧数（含最终被丢弃的部分块）。
    pub frames_captured: u64,
    /// 块线程实际交付给引擎的帧数（只计最新块；被跳过的块不计）。
    pub frames_delivered: u64,
    /// 因积压被跳过的旧块数。
    pub blocks_skipped: u64,
    /// 因积压被跳过的旧块帧数。
    pub frames_dropped: u64,
    /// 未读满一块就继续下一次拉的次数（设备短读；正常应为 0）。
    pub partial_blocks: u64,
    /// 空闲块队列为空 / 块队列满的等待次数（消费端停滞）。
    pub queue_full: u64,
    /// 块线程拉取时没有可用块的次数（欠载）。
    pub underruns: u64,
    /// 可恢复 xrun/挂起次数（每次 +1，含恢复失败前最后一次）。
    pub xruns: u64,
    /// 读设备失败次数（含致命、0 帧、恢复失败）。
    pub read_errors: u64,
    /// 块线程推回空闲块失败的次数（块总数守恒时不应发生）。
    pub recycle_full: u64,
    /// 采集线程是否在跑。
    pub running: bool,
    /// 最近一次错误（稳定静态描述；没有为 None）。成功后不清空。
    pub last_error: Option<&'static str>,
}

impl CaptureCounters {
    fn snapshot(&self, last_error: Option<&'static str>) -> CaptureStats {
        CaptureStats {
            blocks_captured: self.blocks_captured.load(Ordering::Relaxed),
            frames_captured: self.frames_captured.load(Ordering::Relaxed),
            frames_delivered: self.frames_delivered.load(Ordering::Relaxed),
            blocks_skipped: self.blocks_skipped.load(Ordering::Relaxed),
            frames_dropped: self.frames_dropped.load(Ordering::Relaxed),
            partial_blocks: self.partial_blocks.load(Ordering::Relaxed),
            queue_full: self.queue_full.load(Ordering::Relaxed),
            underruns: self.underruns.load(Ordering::Relaxed),
            xruns: self.xruns.load(Ordering::Relaxed),
            read_errors: self.read_errors.load(Ordering::Relaxed),
            recycle_full: self.recycle_full.load(Ordering::Relaxed),
            running: self.running.load(Ordering::Acquire),
            last_error,
        }
    }
}

/// 一块采集样本（构造期分配，运行期只搬运）。
struct CaptureBlock {
    samples: Box<[f32]>,
}

impl CaptureBlock {
    fn new(len: usize) -> Self {
        Self {
            samples: vec![0.0f32; len].into_boxed_slice(),
        }
    }
}

/// 一块的填充结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureFillStatus {
    /// 交付了一整块（最新块；旧块被跳过）。
    Filled,
    /// 没有可用块：输出整块静音。
    Underrun,
    /// 输出缓冲长度不是 block_frames × input_channels：写静音，不 panic。
    WrongOutputLength {
        /// 期望样本数。
        expected: usize,
        /// 实际样本数。
        actual: usize,
    },
}

impl CaptureFillStatus {
    /// 稳定状态名（证据/日志用）。
    pub const fn name(self) -> &'static str {
        match self {
            CaptureFillStatus::Filled => "filled",
            CaptureFillStatus::Underrun => "underrun",
            CaptureFillStatus::WrongOutputLength { .. } => "wrong_output_length",
        }
    }
}

/// 一次 fill_block 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureFillOutcome {
    /// 本块状态。
    pub status: CaptureFillStatus,
    /// 本块交付的帧数（Filled = block_frames，其余 = 0）。
    pub frames: usize,
    /// 本块跳过的旧块数（0 或正数）。
    pub skipped_blocks: usize,
    /// 累积统计快照。
    pub stats: CaptureStats,
}

/// 采集输入（块线程侧）：从采集线程拉最新块，按 channels_map 写进输入块 slot。
pub struct CaptureInput {
    config: CaptureConfig,
    filled: Consumer<CaptureBlock>,
    free: Producer<CaptureBlock>,
    stop: Arc<AtomicBool>,
    counters: Arc<CaptureCounters>,
    last_error: Arc<Mutex<Option<&'static str>>>,
    handle: Option<JoinHandle<()>>,
}

impl fmt::Debug for CaptureInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CaptureInput")
            .field("config", &self.config)
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

impl CaptureInput {
    /// 启动采集线程并返回块线程侧的输入。
    ///
    /// 线程所有权：源被移入线程，`CaptureInput::drop` 会置停止标志并 join（不 panic）。
    pub fn spawn<S: PcmCapture + 'static>(
        source: S,
        config: CaptureConfig,
    ) -> Result<Self, CaptureError> {
        Self::spawn_boxed(Box::new(source), config)
    }

    /// 与 `CaptureInput::spawn` 相同，但源已装箱（测试/装配用）。
    pub fn spawn_boxed(
        source: Box<dyn PcmCapture>,
        config: CaptureConfig,
    ) -> Result<Self, CaptureError> {
        config.validate()?;
        let capacity = config.capacity_blocks()?;
        let samples_per_block = config.samples_per_block()?;
        let (filled_tx, filled_rx) = ring::channel::<CaptureBlock>(capacity);
        let (free_tx, free_rx) = ring::channel::<CaptureBlock>(capacity);
        for _ in 0..capacity {
            if !free_tx.push(CaptureBlock::new(samples_per_block)) {
                return Err(CaptureError::BadConfig("capture block pool overflow"));
            }
        }
        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(CaptureCounters::default());
        counters.running.store(true, Ordering::Release);
        let last_error: Arc<Mutex<Option<&'static str>>> = Arc::new(Mutex::new(None));
        let thread_stop = Arc::clone(&stop);
        let thread_counters = Arc::clone(&counters);
        let thread_error = Arc::clone(&last_error);
        let thread_config = config.clone();
        let handle = thread::Builder::new()
            .name(String::from("khaen-alsa-capture"))
            .spawn(move || {
                let mut source = source;
                capture_loop(
                    source.as_mut(),
                    &thread_config,
                    &filled_tx,
                    &free_rx,
                    &thread_stop,
                    &thread_counters,
                    &thread_error,
                );
                source.close();
            })
            .map_err(|err| CaptureError::ThreadSpawn(err.to_string()))?;
        Ok(Self {
            config,
            filled: filled_rx,
            free: free_tx,
            stop,
            counters,
            last_error,
            handle: Some(handle),
        })
    }

    /// 当前配置。
    pub fn config(&self) -> &CaptureConfig {
        &self.config
    }

    /// 每块输入样本数。
    pub fn samples_per_block(&self) -> usize {
        self.config
            .block_frames
            .saturating_mul(self.config.input_channels)
    }

    /// 拉一块：输出整块帧交错 f32（长度 = block_frames × input_channels）。
    ///
    /// 实时约束：不分配、不加锁、不做 IO、不 panic（统计是原子读）。
    pub fn fill_block(&mut self, out: &mut [f32]) -> CaptureFillOutcome {
        let expected = self.samples_per_block();
        if out.len() != expected {
            out.fill(0.0);
            return CaptureFillOutcome {
                status: CaptureFillStatus::WrongOutputLength {
                    expected,
                    actual: out.len(),
                },
                frames: 0,
                skipped_blocks: 0,
                stats: self.stats(),
            };
        }
        out.fill(0.0);

        let mut latest: Option<CaptureBlock> = None;
        let mut skipped_blocks = 0usize;
        while let Some(block) = self.filled.pop() {
            if let Some(previous) = latest.replace(block) {
                skipped_blocks += 1;
                self.counters.blocks_skipped.fetch_add(1, Ordering::Relaxed);
                self.counters.frames_dropped.fetch_add(
                    (previous.samples.len() / self.config.channels.max(1)) as u64,
                    Ordering::Relaxed,
                );
                let _ = self.free.push(previous);
            }
        }

        let Some(block) = latest else {
            self.counters.underruns.fetch_add(1, Ordering::Relaxed);
            return CaptureFillOutcome {
                status: CaptureFillStatus::Underrun,
                frames: 0,
                skipped_blocks,
                stats: self.stats(),
            };
        };

        let channels = self.config.channels;
        let input_channels = self.config.input_channels;
        let frames = self.config.block_frames;
        for frame in 0..frames {
            let source = frame * channels;
            let destination = frame * input_channels;
            for (capture_channel, &slot) in self.config.channels_map.iter().enumerate() {
                out[destination + usize::from(slot)] = block.samples[source + capture_channel];
            }
        }
        self.counters
            .frames_delivered
            .fetch_add(frames as u64, Ordering::Relaxed);
        if !self.free.push(block) {
            self.counters.recycle_full.fetch_add(1, Ordering::Relaxed);
        }
        CaptureFillOutcome {
            status: CaptureFillStatus::Filled,
            frames,
            skipped_blocks,
            stats: self.stats(),
        }
    }

    /// 清空积压块（设备重开/块边界重置用）；统计保留。
    pub fn flush(&mut self) {
        while let Some(block) = self.filled.pop() {
            if !self.free.push(block) {
                self.counters.recycle_full.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// 统计快照。
    pub fn stats(&self) -> CaptureStats {
        let last_error = *self
            .last_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.counters.snapshot(last_error)
    }

    /// 采集线程是否在跑。
    pub fn is_running(&self) -> bool {
        self.stats().running
    }

    /// 停止采集线程并 join；幂等，返回停止后的统计。
    pub fn stop(&mut self) -> Result<CaptureStats, CaptureError> {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            handle
                .join()
                .map_err(|_| CaptureError::ThreadSpawn(String::from("capture thread panicked")))?;
        }
        self.counters.running.store(false, Ordering::Release);
        Ok(self.stats())
    }
}

impl Drop for CaptureInput {
    fn drop(&mut self) {
        // 析构不 panic：join 失败只表示线程已 panic，没有上报通道。
        let _ = self.stop();
    }
}

/// 采集线程主循环：取空闲块 -> 读满 -> 推入块队列；错误计数、xrun 恢复；停止后退出。
fn capture_loop(
    source: &mut dyn PcmCapture,
    config: &CaptureConfig,
    filled: &Producer<CaptureBlock>,
    free: &Consumer<CaptureBlock>,
    stop: &AtomicBool,
    counters: &CaptureCounters,
    last_error: &Mutex<Option<&'static str>>,
) {
    let channels = config.channels;
    let block_frames = config.block_frames;
    'outer: while !stop.load(Ordering::Acquire) {
        let mut block = match free.pop() {
            Some(block) => block,
            None => {
                counters.queue_full.fetch_add(1, Ordering::Relaxed);
                thread::sleep(Duration::from_millis(1));
                continue;
            }
        };
        let mut frames = 0usize;
        loop {
            if stop.load(Ordering::Acquire) {
                break 'outer;
            }
            let offset = frames.saturating_mul(channels);
            if offset >= block.samples.len() {
                break;
            }
            match source.read(&mut block.samples[offset..]) {
                Ok(read) if read > 0 => {
                    let read = read.min(block_frames - frames);
                    frames += read;
                    counters
                        .frames_captured
                        .fetch_add(read as u64, Ordering::Relaxed);
                }
                Ok(_zero) => {
                    counters.read_errors.fetch_add(1, Ordering::Relaxed);
                    *lock_error(last_error) = Some("capture read made no progress");
                    break 'outer;
                }
                Err(PcmError::Recoverable(fault)) => {
                    counters.xruns.fetch_add(1, Ordering::Relaxed);
                    *lock_error(last_error) = Some(fault.as_str());
                    if let Err(error) = source.prepare(fault) {
                        counters.read_errors.fetch_add(1, Ordering::Relaxed);
                        *lock_error(last_error) = Some(error.name());
                        break 'outer;
                    }
                }
                Err(PcmError::Fatal(reason)) => {
                    counters.read_errors.fetch_add(1, Ordering::Relaxed);
                    *lock_error(last_error) = Some(reason);
                    break 'outer;
                }
            }
            if frames == block_frames {
                break;
            }
        }
        if frames == block_frames {
            counters.blocks_captured.fetch_add(1, Ordering::Relaxed);
            if !filled.push(block) {
                // 块队列满：说明块线程不再消费；停止而不是丢弃后继续制造积压。
                counters.queue_full.fetch_add(1, Ordering::Relaxed);
                break 'outer;
            }
        } else {
            counters.partial_blocks.fetch_add(1, Ordering::Relaxed);
            // 未读满：保留同一块继续读（不把半块推回空闲队列，避免块总数漂移）。
        }
    }
    counters.running.store(false, Ordering::Release);
}

fn lock_error<'a>(
    last_error: &'a Mutex<Option<&'static str>>,
) -> std::sync::MutexGuard<'a, Option<&'static str>> {
    last_error
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(target_os = "linux")]
pub use linux::AlsaCapture;

#[cfg(target_os = "linux")]
mod linux {
    //! Linux ALSA capture 适配层：打开 hw 设备、协商 S24_3LE / 速率 / 通道 / period，
    //! 把 readi 的 S24_3LE 字节解码成 f32；xrun/挂起归类与输出后端同一套状态机。

    use alsa::pcm::{Access, Format, Frames, HwParams, State, PCM};
    use alsa::{Direction, ValueOr};

    use super::{CaptureConfig, CaptureError, PcmCapture, PcmError, PcmFault, BUFFER_PERIODS};
    use crate::aes67_input::FULL_SCALE_DIVISOR;
    use crate::alsa_backend::BYTES_PER_SAMPLE;

    /// 真实的 ALSA capture PCM 流。
    #[derive(Debug)]
    pub struct AlsaCapture {
        /// 已打开的 capture PCM；close 之后为 None。
        pcm: Option<PCM>,
        /// 设备通道数。
        channels: usize,
        /// S24_3LE 原始字节缓冲（构造期分配）。
        bytes: Vec<u8>,
    }

    impl AlsaCapture {
        /// 按配置打开采集设备并启动流；失败返回点名原因（fail-closed）。
        pub fn open(config: &CaptureConfig) -> Result<Self, CaptureError> {
            config.validate()?;
            let channels = u32::try_from(config.channels)
                .map_err(|_| CaptureError::BadConfig("channels does not fit in u32"))?;
            let period = Frames::try_from(config.block_frames)
                .map_err(|_| CaptureError::BadConfig("block_frames does not fit in i64"))?;
            let buffer = period
                .checked_mul(BUFFER_PERIODS as Frames)
                .ok_or(CaptureError::BadConfig("buffer frame count overflow"))?;
            let pcm = PCM::new(config.device.as_str(), Direction::Capture, false)
                .map_err(|err| open_fault("open capture pcm", err))?;
            {
                let hwp = HwParams::any(&pcm).map_err(|err| open_fault("query hw params", err))?;
                hwp.set_access(Access::RWInterleaved)
                    .map_err(|err| open_fault("set access RW_INTERLEAVED", err))?;
                hwp.set_format(Format::S243LE)
                    .map_err(|err| open_fault("set format S24_3LE", err))?;
                hwp.set_channels(channels)
                    .map_err(|err| open_fault("set channels", err))?;
                hwp.set_rate(config.sample_rate, ValueOr::Nearest)
                    .map_err(|err| open_fault("set rate", err))?;
                hwp.set_period_size(period, ValueOr::Nearest)
                    .map_err(|err| open_fault("set period size", err))?;
                hwp.set_buffer_size(buffer)
                    .map_err(|err| open_fault("set buffer size", err))?;
                pcm.hw_params(&hwp)
                    .map_err(|err| open_fault("apply hw params", err))?;
            }
            {
                let current = pcm
                    .hw_params_current()
                    .map_err(|err| open_fault("read back hw params", err))?;
                let actual_format = current
                    .get_format()
                    .map_err(|err| open_fault("read back format", err))?;
                if actual_format != Format::S243LE {
                    return Err(CaptureError::Open(format!(
                        "format mismatch: asked S24_3LE, got {actual_format:?}"
                    )));
                }
                let actual_rate = current
                    .get_rate()
                    .map_err(|err| open_fault("read back rate", err))?;
                if actual_rate != config.sample_rate {
                    return Err(CaptureError::Open(format!(
                        "rate mismatch: asked {} Hz, got {actual_rate} Hz",
                        config.sample_rate
                    )));
                }
                let actual_channels = current
                    .get_channels()
                    .map_err(|err| open_fault("read back channels", err))?;
                if actual_channels != channels {
                    return Err(CaptureError::Open(format!(
                        "channels mismatch: asked {channels}, got {actual_channels}"
                    )));
                }
                let actual_period = current
                    .get_period_size()
                    .map_err(|err| open_fault("read back period size", err))?;
                if actual_period != period {
                    return Err(CaptureError::Open(format!(
                        "period mismatch: asked {period} frames, got {actual_period} frames"
                    )));
                }
            }
            pcm.prepare()
                .map_err(|err| open_fault("prepare capture pcm", err))?;
            pcm.start()
                .map_err(|err| open_fault("start capture pcm", err))?;
            let bytes = vec![
                0u8;
                config
                    .block_frames
                    .checked_mul(config.channels)
                    .and_then(|samples| samples.checked_mul(BYTES_PER_SAMPLE))
                    .ok_or(CaptureError::BadConfig("capture byte buffer overflow"))?
            ];
            Ok(Self {
                pcm: Some(pcm),
                channels: config.channels,
                bytes,
            })
        }
    }

    impl PcmCapture for AlsaCapture {
        fn read(&mut self, out: &mut [f32]) -> Result<usize, PcmError> {
            let channels = self.channels;
            if channels == 0 || !out.len().is_multiple_of(channels) {
                return Err(PcmError::Fatal("capture read length not frame aligned"));
            }
            let frames = out.len() / channels;
            if frames == 0 {
                return Ok(0);
            }
            let needed_bytes = frames
                .checked_mul(channels)
                .and_then(|samples| samples.checked_mul(BYTES_PER_SAMPLE))
                .ok_or(PcmError::Fatal("capture read length overflow"))?;
            if needed_bytes > self.bytes.len() {
                return Err(PcmError::Fatal("capture read exceeds block buffer"));
            }
            let pcm = self
                .pcm
                .as_ref()
                .ok_or(PcmError::Fatal("capture pcm is closed"))?;
            let read_frames = {
                let io = pcm.io_bytes();
                match io.readi(&mut self.bytes[..needed_bytes]) {
                    Ok(frames) => frames,
                    Err(_err) => return Err(classify_state(pcm)),
                }
            };
            let read_frames = read_frames.min(frames);
            let samples = read_frames * channels;
            for (sample, chunk) in out[..samples]
                .iter_mut()
                .zip(self.bytes[..samples * BYTES_PER_SAMPLE].chunks(BYTES_PER_SAMPLE))
            {
                *sample = decode_s24_3le(chunk);
            }
            Ok(read_frames)
        }

        fn prepare(&mut self, fault: PcmFault) -> Result<(), CaptureError> {
            let pcm = self.pcm.as_ref().ok_or(CaptureError::Stopped)?;
            match fault {
                PcmFault::Underrun => pcm
                    .prepare()
                    .map_err(|err| open_fault("prepare after xrun", err)),
                PcmFault::Suspended => {
                    if pcm.resume().is_ok() {
                        return Ok(());
                    }
                    pcm.prepare()
                        .map_err(|err| open_fault("prepare after suspend", err))
                }
            }
        }

        fn close(&mut self) {
            if let Some(pcm) = self.pcm.take() {
                let _ = pcm.drop();
            }
        }
    }

    impl Drop for AlsaCapture {
        fn drop(&mut self) {
            self.close();
        }
    }

    /// 24-bit little-endian 三字节 -> f32。
    fn decode_s24_3le(chunk: &[u8]) -> f32 {
        let raw = i32::from(chunk[0]) | (i32::from(chunk[1]) << 8) | (i32::from(chunk[2]) << 16);
        // 符号扩展：S24_3LE 是 24-bit 二进制补码；左移 8 位得到满量程 i32。
        let signed = (raw << 8) >> 8;
        (signed << 8) as f32 / FULL_SCALE_DIVISOR
    }

    /// 非实时打开路径的 ALSA 错误映射（上下文 + 描述）。
    fn open_fault(context: &str, err: alsa::Error) -> CaptureError {
        CaptureError::Open(format!("{context}: {err}"))
    }

    /// 用 ALSA 状态机归类一次失败的读：XRUN/SUSPENDED 可恢复，其余致命。
    fn classify_state(pcm: &PCM) -> PcmError {
        match pcm.state() {
            State::XRun => PcmError::Recoverable(PcmFault::Underrun),
            State::Suspended => PcmError::Recoverable(PcmFault::Suspended),
            State::Disconnected => PcmError::Fatal("alsa capture pcm disconnected"),
            _ => PcmError::Fatal("alsa capture read failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    fn config() -> CaptureConfig {
        CaptureConfig {
            device: String::from("fake"),
            channels: 2,
            channels_map: vec![0, 1],
            input_channels: 4,
            sample_rate: 48_000,
            block_frames: 8,
            pending_blocks: 2,
        }
    }

    /// 假采集设备：每次 read 填一个递增序列；首轮注入一次 xrun。
    struct FakeCapture {
        frame: f32,
        xrun_once: bool,
        reads: Arc<AtomicUsize>,
    }

    impl FakeCapture {
        fn new() -> (Self, Arc<AtomicUsize>) {
            let reads = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    frame: 0.0,
                    xrun_once: true,
                    reads: Arc::clone(&reads),
                },
                reads,
            )
        }
    }

    impl PcmCapture for FakeCapture {
        fn read(&mut self, out: &mut [f32]) -> Result<usize, PcmError> {
            self.reads.fetch_add(1, AtomicOrdering::Relaxed);
            if self.xrun_once {
                self.xrun_once = false;
                return Err(PcmError::Recoverable(PcmFault::Underrun));
            }
            let channels = 2;
            if !out.len().is_multiple_of(channels) {
                return Err(PcmError::Fatal("fake read not frame aligned"));
            }
            let frames = out.len() / channels;
            for frame in 0..frames {
                for channel in 0..channels {
                    out[frame * channels + channel] = self.frame + channel as f32 * 0.25;
                }
                self.frame += 1.0;
            }
            Ok(frames)
        }

        fn prepare(&mut self, _fault: PcmFault) -> Result<(), CaptureError> {
            Ok(())
        }

        fn close(&mut self) {}
    }

    #[test]
    fn config_validation_rejects_bad_shapes() {
        assert!(config().validate().is_ok());
        let mut bad = config();
        bad.channels_map = vec![0];
        assert!(bad.validate().is_err());
        let mut bad = config();
        bad.channels_map = vec![0, 0];
        assert!(bad.validate().is_err());
        let mut bad = config();
        bad.channels_map = vec![0, 4];
        assert!(bad.validate().is_err());
        let mut bad = config();
        bad.pending_blocks = 1;
        assert!(bad.validate().is_err());
        let mut bad = config();
        bad.device = String::new();
        assert!(bad.validate().is_err());
        let mut bad = config();
        bad.block_frames = MAX_BLOCK_FRAMES + 1;
        assert!(bad.validate().is_err());
    }

    #[test]
    fn capture_thread_fills_mapped_slots_and_recovers_xrun() {
        let (source, reads) = FakeCapture::new();
        let mut input = CaptureInput::spawn(source, config()).expect("spawn");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while input.stats().blocks_captured < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let stats = input.stats();
        assert!(stats.blocks_captured >= 2, "stats={stats:?}");
        assert_eq!(stats.xruns, 1, "stats={stats:?}");
        assert!(stats.frames_captured >= 16);
        let mut block = vec![0.0f32; config().samples_per_block().unwrap()];
        let outcome = input.fill_block(&mut block);
        assert_eq!(outcome.status, CaptureFillStatus::Filled, "{outcome:?}");
        assert_eq!(outcome.frames, 8);
        for frame in 0..8 {
            assert_eq!(block[frame * 4 + 2], 0.0);
            assert_eq!(block[frame * 4 + 3], 0.0);
            assert!(block[frame * 4 + 1] - block[frame * 4] > 0.0);
        }
        assert!(reads.load(AtomicOrdering::Relaxed) >= 2);
        let stats = input.stop().expect("stop");
        assert!(!stats.running);
    }

    #[test]
    fn fill_without_blocks_is_underrun_and_wrong_length_is_rejected() {
        let (source, _reads) = FakeCapture::new();
        let mut input = CaptureInput::spawn(source, config()).expect("spawn");
        // 停线程后积压块仍在队列里；flush 之后拉取必然欠载（线程不会再加块）。
        input.stop().expect("stop");
        input.flush();
        let mut block = vec![0.0f32; config().samples_per_block().unwrap()];
        let outcome = input.fill_block(&mut block);
        assert_eq!(outcome.status, CaptureFillStatus::Underrun);
        assert!(outcome.stats.underruns >= 1);
        let mut short = vec![0.0f32; 3];
        let outcome = input.fill_block(&mut short);
        assert_eq!(
            outcome.status,
            CaptureFillStatus::WrongOutputLength {
                expected: 32,
                actual: 3
            }
        );
        assert!(short.iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn backlog_keeps_latest_block_and_counts_skips() {
        struct Fast {
            frame: f32,
        }
        impl PcmCapture for Fast {
            fn read(&mut self, out: &mut [f32]) -> Result<usize, PcmError> {
                let frames = out.len() / 2;
                for frame in 0..frames {
                    out[frame * 2] = self.frame;
                    out[frame * 2 + 1] = -self.frame;
                    self.frame += 1.0;
                }
                Ok(frames)
            }
            fn prepare(&mut self, _fault: PcmFault) -> Result<(), CaptureError> {
                Ok(())
            }
            fn close(&mut self) {}
        }
        let mut input = CaptureInput::spawn(Fast { frame: 0.0 }, config()).expect("spawn");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while input.stats().blocks_captured < 4 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let mut block = vec![0.0f32; 32];
        let outcome = input.fill_block(&mut block);
        assert_eq!(outcome.status, CaptureFillStatus::Filled);
        assert!(outcome.skipped_blocks >= 1, "{outcome:?}");
        assert_eq!(outcome.stats.frames_delivered, 8);
        input.stop().expect("stop");
    }
}
