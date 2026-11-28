//! AES67 输入后端（T70b）：把 RTP/L24 收包接成引擎的定长块输入。
//!
//! 承担原第三方 daemon + Merging LKM 中"协议 + 缓冲"的角色：RTP 头解析、L24 解码、
//! 抖动环形缓冲、SSRC 切换重置、定长帧交错块输出。**不做** socket/组播 join/收包线程/
//! 网络设备管理，也不做 PTP 时钟（见文末"未实现与 API 边界"）。
//!
//! # 数据流
//!
//! ~~~text
//! UDP 包 ──feed_packet()──► RTP 头解析 ──► L24 解码 ──► 按 RTP 时间戳写入
//!                                                      预分配抖动环（绝对帧号索引）
//!                                                           │
//! fill_block(out: &mut [f32]) ◄── 按 channels_map 映射 ◄────┘
//!      （帧交错 f32，长度 = block_frames × output_channels；缺口/欠载填 0）
//! ~~~
//!
//! # 实时约束
//!
//! - [Aes67Input::feed_packet] 与 [Aes67Input::fill_block] 都只在构造期预分配的缓冲上
//!   工作：不分配、不加锁、不做 IO、不打印、不 panic；所有错误经返回值/统计上报。
//! - 抖动环样本区、帧有效位图、声道映射表都在 [Aes67Input::new] 一次分配完毕。
//! - 当前是单所有者（&mut self）对象，刻意不引入任何锁：socket 线程与 RT 线程分离所需
//!   的 SPSC 拆分不属于本任务（见文末 TODO），接线时不要把 Mutex 带进 RT 线程。
//!
//! # 时间语义
//!
//! RTP 时间戳以会话内第一包锚定为媒体帧 0（RTP 时间戳单位 = 每声道样本数；AES67 媒体面
//! 为 48 kHz）。后续包按 timestamp - ts0 的绝对帧号写入环形缓冲，因此乱序包按时间戳
//! 就位、不按到达顺序播放。fill_block 是拉取式接口：块节奏由调用方的音频时钟决定；
//! 首块输出前按 jitter_target_frames 水位预填（0 = 不预填，凑够一块就出）；欠载输出
//! 整块静音并推进播放游标（与 khaen-aoip-aes67::jitter 的欠载语义一致），不输出半块。
//!
//! # 与引擎的接线
//!
//! fill_block 的输出块（帧交错 f32）就是 BlockEngine 输入块的形状：EngineConfig::channels
//! 必须等于 Aes67InputConfig::output_channels（未映射的输出声道恒 0）；块序由调用方音频时钟
//! 驱动，把 fill_block 写好的切片喂给 BlockEngine::run_block 的 input 即可。
//!
//! # ASRC（T41：`KHAEN_ASRC=off|poly|sinc`，默认 off）
//!
//! 可选地在抖动环与引擎块之间插入异步重采样（crate `khaen-asrc`）：抖动环仍以媒体帧
//! 写入（RTP 时间戳域），但消费端不再"每块硬取 block_frames 帧"，而是按重采样器需要的
//! `input_frames_next()` 取帧、固定输出 block_frames 帧；[Aes67InputStats::jitter_fill]
//! 作为漂移控制器的反馈量，把水位稳定在目标值（默认 2 × block_frames，且 ≤ 环容量一半）。
//!
//! - `KHAEN_ASRC=off`（默认）：原路径逐字节不变；
//! - `poly`：rubato Async + Cubic 多项式插值（默认推荐，±500 ppm 近 1:1 足够）；
//! - `sinc`：rubato Async + sinc 抗混叠插值（更高成本/延迟的备用）；
//! - 非法值按配置错误处理（不静默回退）；
//! - ASRC 处理失败时本块自动回退到原输入路径并累计 [Aes67InputStats::asrc_bypasses]，
//!   fail-closed 语义不变（欠载仍整块静音、不输出半块）；换流（SSRC/时间戳跳变）会 reset
//!   重采样器与控制环路并重新尝试启用。
//!
//! 启用且未 bypass 时 [Aes67InputStats::asrc] 暴露 `input.asrc` 快照（enabled/ratio_ppm/
//! ppm_estimate/fill_frames/last_error）；rt-host 的 status 写盘函数负责把它序列化。
//!
//! # 待与 aoip crate 统一
//!
//! host 目前不依赖 khaen-aoip-aes67（Cargo.toml 不在本任务写范围），因此本模块内的
//! RTP 最小解析与 L24 解码是**临时实现**：数值公式、错误分类刻意与
//! khaen-aoip-aes67::rtp（RFC 3550 基础头 + CSRC/扩展/padding 跳过；L24 = 24-bit 大端
//! 线性；归一化 i32/2^31 = 24bit/2^23）保持一致。接线后应改为复用 aoip crate 的
//! rtp/jitter 实现，避免两份协议代码漂移（TODO: 待与 aoip crate 统一）。
//!
//! # 未实现与 API 边界（TODO）
//!
//! - UDP socket/收包线程/SO_RCVBUF/IGMP：本任务不实现；[Aes67InputConfig] 的
//!   multicast_addr/port 只保存给接线层使用（组播 join 或单播 bind 由 runtime 的
//!   `UdpPacketSource` 决定）。
//! - 收包线程 → RT 线程的无锁 SPSC 环：当前 [Aes67Input] 是单所有者对象，真实集成需
//!   拆成 writer/reader 两半（参考 khaen-aoip-aes67::jitter 的原子游标 + 有效位图），
//!   或在接线层用无锁环搬运解码后的帧。
//! - PTP/媒体时钟锚定（首块播放时刻 = origin_ns + latency）：拉取式 fill_block 只负责
//!   数据；时钟策略与首块延迟由上层（T70c 设备监督器/主机时钟）决定。
//! - 冗余流（AES67 双流热备）与丢包补偿/重传：只做单流，双流选择留待后续。
//! - 多流混入同一条输入总线：见 [crate::aes67_bus]（I3a 原语；尚未接到 Runtime/rt-host）。

use core::fmt;
use std::net::Ipv4Addr;

pub use khaen_asrc::Interpolation;
use khaen_asrc::{Asrc, AsrcConfig, AsrcError, AsrcStatus};

use crate::engine::MAX_BLOCK_FRAMES;

/// RTP 版本号（RFC 3550 v=2）。
pub const RTP_VERSION: u8 = 2;
/// RTP 基础头字节数（无 CSRC/扩展）。
pub const RTP_HEADER_LEN: usize = 12;
/// L24（24-bit 线性、大端 3 字节）每样本字节数。
pub const L24_SAMPLE_BYTES: usize = 3;
/// L24 满量程归一化：满量程 i32 / 2^31（与 khaen-aoip-aes67::rtp 同式，保证接线后数值一致）。
pub const FULL_SCALE_DIVISOR: f32 = 2_147_483_648.0;
/// 单条 AES67 媒体流最大通道数（AES67 profile 每流 ≤ 8）。
pub const MAX_RTP_CHANNELS: usize = 8;
/// 输出帧交错声道数上限（配置防御边界；设备 DAC 数远小于此）。
pub const MAX_OUTPUT_CHANNELS: usize = 64;
/// 时间戳跳变阈值（秒）：与上一包时间戳偏离超过该值视为新流并重置。
pub const TS_JUMP_RESET_SEC: u32 = 10;
/// ASRC 开关环境变量：`off`（默认）|`poly`|`sinc`，大小写不敏感。
pub const ASRC_ENV: &str = "KHAEN_ASRC";
/// ASRC 默认目标水位（块数）：2 块 ≈ 5.3 ms @128 帧；host 实际取 min(该值, 环容量/2)。
const ASRC_TARGET_BLOCKS: usize = 2;

/// 抖动环容量下限（帧）。
const MIN_CAPACITY_FRAMES: usize = 512;
/// 抖动环容量上限（帧）：65536 帧 ≈ 1.365 s @48k，防止配置失控。
const MAX_CAPACITY_FRAMES: usize = 65_536;
/// 序号缺口跟踪窗口（包）：乱序补齐时回退 lost 估算。
const MISSING_TRACK_PACKETS: usize = 64;

/// AES67 输入配置（构造期固定，运行期不改）。
///
/// 校验见 [Aes67InputConfig::validate]；[Aes67Input::new] 会先校验再分配。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Aes67InputConfig {
    /// 目的地址：组播组（如 239.69.0.1）或单播本端地址（如 192.0.2.222）。
    /// 本字段只保存，socket 接线时决定 join 组播还是 bind 单播本端地址。
    pub multicast_addr: Ipv4Addr,
    /// 目的端口（RTP 端口，通常 5004）。
    pub port: u16,
    /// 期望的 RTP 动态 payload type（AES67 使用 96..=127）；不匹配的包计为 foreign 并忽略。
    pub payload_type: u8,
    /// 媒体采样率（Hz）。必须与方案速率一致（不重采样）；只要求非 0，常见为 48000。
    pub sample_rate: u32,
    /// RTP 声道数（每帧样本数；AES67 profile 每流 ≤ 8）。
    pub channels: usize,
    /// RTP 声道 → 输出帧交错声道位置；长度必须等于 channels，目标位置必须互不重复。
    pub channels_map: Vec<u16>,
    /// 输出帧交错声道数（≥ 映射用到的位置；未映射的位置在块输出里恒为 0）。
    pub output_channels: usize,
    /// 定长块帧数（与 EngineConfig::block_frames / DeviceConfig::block_frames 一致）。
    pub block_frames: usize,
    /// 目标抖动深度（媒体帧 = 每声道样本数）：首块输出前预填到该水位，0 = 不预填。
    ///
    /// 例：4 ms @48k = 192 帧。该值只决定"首块预填水位"与抖动环容量，不引入额外时钟延迟；
    /// 真实播放延迟由上层媒体时钟策略决定。
    pub jitter_target_frames: usize,
}

impl Default for Aes67InputConfig {
    fn default() -> Self {
        Self {
            multicast_addr: Ipv4Addr::new(239, 69, 0, 1),
            port: 5004,
            payload_type: 96,
            sample_rate: 48_000,
            channels: 2,
            channels_map: vec![0, 1],
            output_channels: 2,
            block_frames: crate::engine::DEFAULT_BLOCK_FRAMES,
            jitter_target_frames: 0,
        }
    }
}

impl Aes67InputConfig {
    /// 校验配置；不合法时返回点名原因（fail-closed）。
    pub fn validate(&self) -> Result<(), InputError> {
        if self.multicast_addr.is_unspecified() {
            return Err(InputError::BadConfig(
                "multicast_addr must be a concrete IPv4 destination (multicast group or unicast local address, not 0.0.0.0)",
            ));
        }
        if self.port == 0 {
            return Err(InputError::BadConfig("port must be non-zero"));
        }
        if !(96..=127).contains(&self.payload_type) {
            return Err(InputError::BadConfig(
                "payload_type must be a dynamic RTP type in 96..=127",
            ));
        }
        if self.sample_rate == 0 {
            return Err(InputError::BadConfig("sample_rate must be positive"));
        }
        if self.channels == 0 || self.channels > MAX_RTP_CHANNELS {
            return Err(InputError::BadConfig(
                "channels must be in 1..=MAX_RTP_CHANNELS",
            ));
        }
        if self.channels_map.len() != self.channels {
            return Err(InputError::BadConfig(
                "channels_map length must equal channels",
            ));
        }
        if self.output_channels == 0 || self.output_channels > MAX_OUTPUT_CHANNELS {
            return Err(InputError::BadConfig(
                "output_channels must be in 1..=MAX_OUTPUT_CHANNELS",
            ));
        }
        let mut used_slots = 0u64;
        for &slot in &self.channels_map {
            let slot = usize::from(slot);
            if slot >= self.output_channels {
                return Err(InputError::BadConfig(
                    "channels_map slot must be < output_channels",
                ));
            }
            let bit = 1u64 << slot;
            if used_slots & bit != 0 {
                return Err(InputError::BadConfig("channels_map must be injective"));
            }
            used_slots |= bit;
        }
        if self.block_frames == 0 || self.block_frames > MAX_BLOCK_FRAMES {
            return Err(InputError::BadConfig(
                "block_frames must be in 1..=MAX_BLOCK_FRAMES",
            ));
        }
        if self.jitter_target_frames > MAX_CAPACITY_FRAMES {
            return Err(InputError::BadConfig(
                "jitter_target_frames exceeds jitter ring limit",
            ));
        }
        // 容量按目标深度 + 一块余量推导；超上限即拒绝（不允许静默截断预填目标）。
        let _ = self.capacity_frames()?;
        Ok(())
    }

    /// 由配置推导抖动环容量（帧，2 的幂）。
    ///
    /// max(4 × block_frames, jitter_target_frames + block_frames, MIN_CAPACITY_FRAMES)
    /// 向上取到 2 的幂；超过上限返回 BadConfig。
    fn capacity_frames(&self) -> Result<usize, InputError> {
        let wanted = self
            .block_frames
            .saturating_mul(4)
            .max(self.jitter_target_frames.saturating_add(self.block_frames))
            .max(MIN_CAPACITY_FRAMES);
        if wanted > MAX_CAPACITY_FRAMES {
            return Err(InputError::BadConfig(
                "jitter_target_frames too large for jitter ring",
            ));
        }
        Ok(wanted.next_power_of_two())
    }
}

/// AES67 输入错误（Copy；稳定错误名供日志/证据使用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputError {
    /// 配置非法（见 [Aes67InputConfig::validate]）。
    BadConfig(&'static str),
    /// UDP 包短于 12 字节 RTP 基础头。
    PacketTooShort,
    /// RTP 版本不是 2。
    BadVersion,
    /// CSRC 列表越界。
    CsrcOverflow,
    /// header extension 越界。
    ExtensionOverflow,
    /// padding 长度非法。
    BadPadding,
    /// payload 空或不是整帧（长度不能被 channels × 3 整除）。
    BadPayload,
}

impl InputError {
    /// 稳定错误名（证据/JSON/日志用，不参与 RT 决策）。
    pub const fn name(self) -> &'static str {
        match self {
            InputError::BadConfig(_) => "ERR_AES67_INPUT_CONFIG",
            InputError::PacketTooShort => "ERR_AES67_INPUT_RTP_TOO_SHORT",
            InputError::BadVersion => "ERR_AES67_INPUT_RTP_BAD_VERSION",
            InputError::CsrcOverflow => "ERR_AES67_INPUT_RTP_CSRC_OVERFLOW",
            InputError::ExtensionOverflow => "ERR_AES67_INPUT_RTP_EXTENSION_OVERFLOW",
            InputError::BadPadding => "ERR_AES67_INPUT_RTP_BAD_PADDING",
            InputError::BadPayload => "ERR_AES67_INPUT_RTP_BAD_PAYLOAD",
        }
    }
}

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InputError::BadConfig(message) => write!(f, "bad config: {message}"),
            other => f.write_str(other.name()),
        }
    }
}

impl std::error::Error for InputError {}

/// 一包喂入结果（Copy；热路径返回，不分配）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedOutcome {
    /// 已按 RTP 时间戳写入抖动环。
    Accepted {
        /// 本包帧数。
        frames: usize,
    },
    /// SSRC 变化：已重置抖动环/锚点（旧流数据丢弃），本包已按新流锚定写入。
    Restarted {
        /// 本包帧数。
        frames: usize,
    },
    /// 时间戳大跳变（超过 TS_JUMP_RESET_SEC 秒）：按新流重置后写入。
    TimestampJump {
        /// 本包帧数。
        frames: usize,
    },
    /// 重复包（与最近序号相同），丢弃。
    Duplicate,
    /// 过期包（帧号已低于播放游标，或早于锚点），丢弃。
    Late {
        /// 丢弃帧数。
        frames: usize,
    },
    /// 帧号超出抖动环窗口（收包过快/消费者停顿），丢弃。
    Overflow {
        /// 丢弃帧数。
        frames: usize,
    },
    /// payload type 不是本流（其他流/杂包），忽略。
    ForeignPayload,
}

impl FeedOutcome {
    /// 稳定结果名（证据/日志用）。
    pub const fn name(self) -> &'static str {
        match self {
            FeedOutcome::Accepted { .. } => "accepted",
            FeedOutcome::Restarted { .. } => "restarted",
            FeedOutcome::TimestampJump { .. } => "timestamp_jump",
            FeedOutcome::Duplicate => "duplicate",
            FeedOutcome::Late { .. } => "late",
            FeedOutcome::Overflow { .. } => "overflow",
            FeedOutcome::ForeignPayload => "foreign_payload",
        }
    }
}

/// 一次 fill_block 的结果状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockStatus {
    /// 输出一整块（帧交错；映射后的 RTP 声道有值，其余声道为 0）。
    Filled,
    /// 尚未锚定/未达到预填水位：整块静音，播放游标不前进。
    Warmup,
    /// 已开始播放但数据不足：整块静音，播放游标前进（这些帧视为丢失）。
    Underrun,
    /// 输出缓冲长度不是 block_frames × output_channels：写入整块静音，不 panic。
    WrongOutputLength {
        /// 期望样本数。
        expected: usize,
        /// 实际样本数。
        actual: usize,
    },
}

impl BlockStatus {
    /// 稳定状态名（证据/日志用）。
    pub const fn name(self) -> &'static str {
        match self {
            BlockStatus::Filled => "filled",
            BlockStatus::Warmup => "warmup",
            BlockStatus::Underrun => "underrun",
            BlockStatus::WrongOutputLength { .. } => "wrong_output_length",
        }
    }
}

/// 一次 fill_block 的返回值：状态 + 块内有效帧数 + 累积统计快照。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockOutcome {
    /// 本块状态。
    pub status: BlockStatus,
    /// 本块写出的有效媒体帧数（Filled = block_frames，其余 = 0；静音块整块为 0）。
    pub frames: usize,
    /// 累积统计快照（含 jitter_fill 当前水位；Copy，不分配）。
    pub stats: Aes67InputStats,
}

/// ASRC 状态快照（Copy/Eq；ppm 取整到 i32，方便作为立即数进 JSON/日志）。
///
/// 与 `khaen_asrc::AsrcStatus` 一一对应，但用整数 ppm 保持 [Aes67InputStats] 的 `Eq`。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AsrcInputStats {
    /// ASRC 是否有效启用（配置启用且未被 bypass）。
    pub enabled: bool,
    /// 当前施加的比率修正（ppm，整数化）。
    pub ratio_ppm: i32,
    /// 估计的稳态时钟偏差修正（ppm，= 控制器积分项，整数化）。
    pub ppm_estimate: i32,
    /// 抖动环 fill 快照（帧）。
    pub fill_frames: usize,
    /// 最近一次失败的错误名（无失败为 None）。
    pub last_error: Option<&'static str>,
}

impl From<AsrcStatus> for AsrcInputStats {
    fn from(status: AsrcStatus) -> Self {
        Self {
            enabled: status.enabled,
            ratio_ppm: status.ratio_ppm.round() as i32,
            ppm_estimate: status.ppm_estimate.round() as i32,
            fill_frames: status.fill_frames,
            last_error: status.last_error,
        }
    }
}

/// AES67 输入累积统计（Copy 快照；jitter_fill/ssrc/asrc 是快照值，其余为累计值）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Aes67InputStats {
    /// 收到并通过 RTP 头解析的包数（含 foreign/duplicate/late/overflow；不含解析失败）。
    pub packets: u64,
    /// 写入抖动环的媒体帧数。
    pub frames_received: u64,
    /// 通过 fill_block 输出的有效媒体帧数（不含静音块）。
    pub frames_filled: u64,
    /// 按序号缺口估算的丢包数（乱序补齐时回退；超过跟踪窗口的缺口只增不减）。
    pub lost: u64,
    /// 过期丢弃的包数。
    pub late: u64,
    /// 欠载/预热静音块数（每次 fill_block 未填满一块即 +1）。
    pub underruns: u64,
    /// 抖动环窗口溢出丢弃的包数。
    pub overruns: u64,
    /// 重复序号包数。
    pub duplicates: u64,
    /// 乱序（旧序号）包数。
    pub out_of_order: u64,
    /// 畸形包数（RTP 解析失败或 payload 非整帧）。
    pub malformed: u64,
    /// payload type 不匹配而忽略的包数。
    pub foreign_payloads: u64,
    /// 时间戳大跳变触发的流重置次数。
    pub timestamp_jumps: u64,
    /// SSRC 变化触发的流重置次数。
    pub ssrc_changes: u64,
    /// 快照：当前抖动环内连续可播放帧数（水位 − 播放游标）。
    pub jitter_fill: usize,
    /// 快照：当前流 SSRC（尚未锚定时为 None）。
    pub ssrc: Option<u32>,
    /// 快照：ASRC 状态（未启用 ASRC 时为 None）。
    pub asrc: Option<AsrcInputStats>,
    /// ASRC 因失败自动 bypass 的次数（每次失败当块回退到原输入路径；fail-closed）。
    pub asrc_bypasses: u64,
}

/// host 侧 ASRC 运行状态（构造期固定配置，运行期只更新内部状态）。
#[derive(Debug)]
struct AsrcState {
    /// 重采样器 + 控制器（crate khaen-asrc）。
    asrc: Asrc,
    /// 抖动环目标水位（帧）：prefill 门槛与控制器目标一致（构造期确定）。
    target_fill_frames: usize,
}

/// AES67 定长块输入：RTP/L24 → 抖动环 → 按声道映射输出帧交错 f32 块。
///
/// 构造期一次性分配全部缓冲；运行期（[feed_packet](Self::feed_packet) /
/// [fill_block](Self::fill_block)）不分配、不加锁、不做 IO、不 panic。
pub struct Aes67Input {
    config: Aes67InputConfig,
    /// 抖动环容量（帧，2 的幂）。
    capacity: usize,
    /// 帧号取模掩码 = capacity - 1。
    mask: usize,
    /// 环样本区（帧交错，RTP 声道顺序；长度 = capacity × channels）。
    samples: Vec<f32>,
    /// 帧有效位图（word = frame/64；有效帧 = 已写入且播放游标未越过）。
    valid: Vec<u64>,
    /// 下一个待播放的绝对媒体帧号。
    read_frame: u64,
    /// 已连续写入的水位（绝对帧号；[read_frame, written_upto) 可整块播放）。
    written_upto: u64,
    /// 首块预填是否已完成（预填只做一次；欠载后不二次预填）。
    prefill_done: bool,
    /// 当前流 SSRC（None = 尚未收到任何包）。
    ssrc: Option<u32>,
    /// 锚点 RTP 时间戳（= 媒体帧 0）。
    ts0: u32,
    /// 上一包时间戳（跳变检测用；换流后为 None）。
    last_ts: Option<u32>,
    /// 最近接收序号（单调，用于丢包估算）。
    last_seq: Option<u16>,
    /// 序号缺口跟踪表（固定容量，乱序补齐时回退 lost）。
    missing: [u16; MISSING_TRACK_PACKETS],
    /// 缺口表当前条目数。
    missing_len: usize,
    /// 累积统计（jitter_fill/ssrc/asrc 在快照时填）。
    stats: Aes67InputStats,
    /// 可选 ASRC：None = KHAEN_ASRC=off；Some 时 fill_block 走重采样路径。
    asrc: Option<AsrcState>,
    /// 测试用：下一次 fill_block 强制 ASRC 失败（验证自动 bypass 路径）。
    #[cfg(test)]
    asrc_fail_next: bool,
}

impl fmt::Debug for Aes67Input {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Aes67Input")
            .field("config", &self.config)
            .field("capacity_frames", &self.capacity)
            .field("ssrc", &self.ssrc)
            .field("read_frame", &self.read_frame)
            .field("written_upto", &self.written_upto)
            .field("prefill_done", &self.prefill_done)
            .field("asrc", &self.asrc)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl Aes67Input {
    /// 新建输入：读 `KHAEN_ASRC=off|poly|sinc`（默认 off），校验配置并一次性分配
    /// 抖动环/位图/ASRC 缓冲（之后运行期不再分配）。
    pub fn new(config: Aes67InputConfig) -> Result<Self, InputError> {
        Self::new_with_asrc(config, asrc_interp_from_env()?)
    }

    /// 显式指定插值方式的新建（测试与需要绕过环境变量的接线用）。
    ///
    /// `interp = None` 等价于 `KHAEN_ASRC=off`（原路径，逐字节不变）。
    pub fn new_with_asrc(
        config: Aes67InputConfig,
        interp: Option<Interpolation>,
    ) -> Result<Self, InputError> {
        config.validate()?;
        let capacity = config.capacity_frames()?;
        let asrc = match interp {
            None => None,
            Some(interp) => {
                let target_fill_frames = asrc_target_fill(&config, capacity);
                let mut asrc_config =
                    AsrcConfig::new(config.channels, config.block_frames, config.sample_rate);
                asrc_config.interp = interp;
                asrc_config.target_fill_frames = target_fill_frames;
                let asrc = Asrc::new(asrc_config).map_err(map_asrc_error)?;
                Some(AsrcState {
                    asrc,
                    target_fill_frames,
                })
            }
        };
        let channels = config.channels;
        let samples = vec![0.0f32; capacity * channels];
        let valid = vec![0u64; capacity / 64];
        Ok(Self {
            config,
            capacity,
            mask: capacity - 1,
            samples,
            valid,
            read_frame: 0,
            written_upto: 0,
            prefill_done: false,
            ssrc: None,
            ts0: 0,
            last_ts: None,
            last_seq: None,
            missing: [0u16; MISSING_TRACK_PACKETS],
            missing_len: 0,
            stats: Aes67InputStats::default(),
            asrc,
            #[cfg(test)]
            asrc_fail_next: false,
        })
    }

    /// 当前是否有效启用 ASRC（已构造且未被 bypass）。
    pub fn asrc_enabled(&self) -> bool {
        self.asrc
            .as_ref()
            .is_some_and(|state| !state.asrc.is_bypassed())
    }

    /// ASRC 目标水位（帧）；未启用 ASRC 时为 None。
    pub fn asrc_target_fill_frames(&self) -> Option<usize> {
        self.asrc.as_ref().map(|state| state.target_fill_frames)
    }

    /// ASRC 的插值方式；未启用 ASRC 时为 None。
    pub fn asrc_interpolation(&self) -> Option<Interpolation> {
        self.asrc.as_ref().map(|state| state.asrc.config().interp)
    }

    /// ASRC 状态快照（未启用时为 None）。
    pub fn asrc_status(&self) -> Option<AsrcStatus> {
        self.asrc
            .as_ref()
            .map(|state| state.asrc.status(self.jitter_fill()))
    }

    /// 配置视图。
    pub fn config(&self) -> &Aes67InputConfig {
        &self.config
    }

    /// 抖动环容量（帧，2 的幂；诊断用）。
    pub fn capacity_frames(&self) -> usize {
        self.capacity
    }

    /// 每块样本数 = block_frames × output_channels（fill_block 的入参长度）。
    pub fn samples_per_block(&self) -> usize {
        self.config.block_frames * self.config.output_channels
    }

    /// 当前抖动环内连续可播放帧数（水位 − 播放游标）。
    pub fn jitter_fill(&self) -> usize {
        self.written_upto
            .saturating_sub(self.read_frame)
            .min(self.capacity as u64) as usize
    }

    /// 累积统计快照（含当前 jitter_fill/ssrc/asrc；不分配、不阻塞）。
    pub fn stats(&self) -> Aes67InputStats {
        let mut stats = self.stats;
        let fill = self.jitter_fill();
        stats.jitter_fill = fill;
        stats.ssrc = self.ssrc;
        stats.asrc = self
            .asrc
            .as_ref()
            .map(|state| AsrcInputStats::from(state.asrc.status(fill)));
        stats
    }

    /// 喂一个 UDP 载荷（完整 RTP 包）到抖动环。
    ///
    /// 按 RTP 时间戳（相对锚点）落到绝对媒体帧号；乱序包按时间戳就位。SSRC 变化/时间戳
    /// 大跳变会重置抖动环（旧流数据丢弃）并计数，绝不把旧流样本混进新流输出。
    ///
    /// 实时约束：不分配、不加锁、不做 IO、不 panic；错误经 Err/计数上报。
    pub fn feed_packet(&mut self, pkt: &[u8]) -> Result<FeedOutcome, InputError> {
        let parsed = match parse_rtp(pkt) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.stats.malformed = self.stats.malformed.saturating_add(1);
                return Err(error);
            }
        };
        self.stats.packets = self.stats.packets.saturating_add(1);

        let header = parsed.header;
        if header.payload_type != self.config.payload_type {
            self.stats.foreign_payloads = self.stats.foreign_payloads.saturating_add(1);
            return Ok(FeedOutcome::ForeignPayload);
        }
        let frames = match frames_in_payload(parsed.payload_len, self.config.channels) {
            Ok(frames) => frames,
            Err(error) => {
                self.stats.malformed = self.stats.malformed.saturating_add(1);
                return Err(error);
            }
        };

        // 流选择：首包锚定；SSRC 变化/时间戳大跳变按新流重置（不把旧流样本混入）。
        let mut restart = None;
        if let Some(current_ssrc) = self.ssrc {
            if current_ssrc != header.ssrc {
                self.stats.ssrc_changes = self.stats.ssrc_changes.saturating_add(1);
                self.restart_stream(header.ssrc, header.timestamp);
                restart = Some(RestartKind::Ssrc);
            } else if let Some(last_ts) = self.last_ts {
                let step = Self::timestamp_step(last_ts, header.timestamp);
                let threshold = i64::from(self.config.sample_rate) * i64::from(TS_JUMP_RESET_SEC);
                if step.abs() > threshold {
                    self.stats.timestamp_jumps = self.stats.timestamp_jumps.saturating_add(1);
                    self.restart_stream(header.ssrc, header.timestamp);
                    restart = Some(RestartKind::Timestamp);
                }
            }
        } else {
            self.restart_stream(header.ssrc, header.timestamp);
        }
        self.last_ts = Some(header.timestamp);

        // 序号缺口估算：前向跳变登记缺口（lost += gap）；乱序补齐时回退 lost。
        match self.last_seq {
            None => self.last_seq = Some(header.sequence),
            Some(last_seq) => {
                let gap = i32::from(header.sequence.wrapping_sub(last_seq) as i16);
                if gap == 0 {
                    self.stats.duplicates = self.stats.duplicates.saturating_add(1);
                    return Ok(FeedOutcome::Duplicate);
                }
                if gap > 0 {
                    // 相邻序号 gap = 1 表示没有缺口；缺口数 = gap - 1。
                    self.note_missing(last_seq.wrapping_add(1), (gap - 1) as usize);
                    self.last_seq = Some(header.sequence);
                } else {
                    self.stats.out_of_order = self.stats.out_of_order.saturating_add(1);
                    let found = self.missing[..self.missing_len]
                        .iter()
                        .position(|&sequence| sequence == header.sequence);
                    if let Some(index) = found {
                        self.missing.copy_within(index + 1..self.missing_len, index);
                        self.missing_len -= 1;
                        self.stats.lost = self.stats.lost.saturating_sub(1);
                    }
                }
            }
        }

        // RTP 时间戳 → 绝对媒体帧号（负偏移 = 早于锚点的旧包）。
        let offset = i64::from(header.timestamp.wrapping_sub(self.ts0) as i32);
        if offset < 0 {
            self.stats.late = self.stats.late.saturating_add(1);
            return Ok(FeedOutcome::Late { frames });
        }
        let frame_start = offset as u64;
        let frames_u64 = frames as u64;
        let read_frame = self.read_frame;
        let window_end = read_frame.saturating_add(self.capacity as u64);
        if frame_start < read_frame {
            // 整包过期：不写半包（避免覆盖已消费帧），与 aoip crate 的 late 语义一致。
            self.stats.late = self.stats.late.saturating_add(1);
            return Ok(FeedOutcome::Late { frames });
        }
        if frame_start >= window_end || frame_start.saturating_add(frames_u64) > window_end {
            self.stats.overruns = self.stats.overruns.saturating_add(1);
            return Ok(FeedOutcome::Overflow { frames });
        }

        let payload = &pkt[parsed.payload_offset..parsed.payload_offset + parsed.payload_len];
        let channels = self.config.channels;
        let mask = self.mask;
        for frame in 0..frames {
            let absolute = frame_start + frame as u64;
            let position = (absolute & mask as u64) as usize;
            let destination = position * channels;
            let source = frame * channels * L24_SAMPLE_BYTES;
            for channel in 0..channels {
                let byte = source + channel * L24_SAMPLE_BYTES;
                self.samples[destination + channel] = decode_l24_sample(payload, byte);
            }
            self.set_valid(absolute);
        }
        self.stats.frames_received = self.stats.frames_received.saturating_add(frames_u64);
        self.advance_watermark();

        Ok(match restart {
            Some(RestartKind::Ssrc) => FeedOutcome::Restarted { frames },
            Some(RestartKind::Timestamp) => FeedOutcome::TimestampJump { frames },
            None => FeedOutcome::Accepted { frames },
        })
    }

    /// 输出一块帧交错 f32（长度 = block_frames × output_channels）。
    ///
    /// 按 channels_map 把 RTP 声道放到输出位置，未映射位置写 0。数据不足（预热/欠载）
    /// 时整块写静音并递增 underruns，不 panic；欠载时播放游标照常前进（媒体时间不倒退）。
    ///
    /// 启用 ASRC 时走重采样路径（固定输出 block_frames 帧、输入帧数随比率变化）；
    /// ASRC 处理失败时本块自动回退原路径并累计 asrc_bypasses（fail-closed 语义不变）。
    ///
    /// 实时约束：不分配、不加锁、不做 IO、不 panic。
    pub fn fill_block(&mut self, out: &mut [f32]) -> BlockOutcome {
        let expected = self.samples_per_block();
        if out.len() != expected {
            out.fill(0.0);
            return BlockOutcome {
                status: BlockStatus::WrongOutputLength {
                    expected,
                    actual: out.len(),
                },
                frames: 0,
                stats: self.stats(),
            };
        }
        out.fill(0.0);
        if self.ssrc.is_none() {
            self.stats.underruns = self.stats.underruns.saturating_add(1);
            return BlockOutcome {
                status: BlockStatus::Warmup,
                frames: 0,
                stats: self.stats(),
            };
        }
        let block = self.config.block_frames as u64;
        let available = self.written_upto.saturating_sub(self.read_frame);
        if !self.prefill_done {
            // ASRC 启用时预填到它自己的目标水位（控制器目标），否则沿用原 jitter_target 语义。
            let gate = match &self.asrc {
                Some(state) => state.target_fill_frames as u64,
                None => block.max(self.config.jitter_target_frames as u64),
            };
            if available < gate {
                self.stats.underruns = self.stats.underruns.saturating_add(1);
                return BlockOutcome {
                    status: BlockStatus::Warmup,
                    frames: 0,
                    stats: self.stats(),
                };
            }
            self.prefill_done = true;
        }
        if let Some(mut state) = self.asrc.take() {
            if !state.asrc.is_bypassed() {
                if self.asrc_injected_failure() {
                    self.stats.asrc_bypasses = self.stats.asrc_bypasses.saturating_add(1);
                    state
                        .asrc
                        .note_bypass(AsrcError::Process("injected failure"));
                } else {
                    match self.fill_block_asrc(&mut state, out) {
                        Ok(mut outcome) => {
                            self.asrc = Some(state);
                            // fill_block_asrc 期间 asrc 被 take 出 self，这里补上含 asrc 的快照。
                            outcome.stats = self.stats();
                            return outcome;
                        }
                        Err(error) => {
                            self.stats.asrc_bypasses = self.stats.asrc_bypasses.saturating_add(1);
                            state.asrc.note_bypass(error);
                        }
                    }
                }
            }
            self.asrc = Some(state);
        }
        self.fill_block_legacy(out)
    }

    /// ASRC 路径：从抖动环取 `input_frames_next()` 帧（缺帧补 0），固定输出一块。
    ///
    /// 欠载（环内连续帧不足）时仍推进重采样器（缺帧按静音），但整块输出静音并计 underruns，
    /// 与原有"不输出半块"的欠载语义一致；返回 Err 表示 ASRC 自身失败（调用方回退原路径）。
    fn fill_block_asrc(
        &mut self,
        state: &mut AsrcState,
        out: &mut [f32],
    ) -> Result<BlockOutcome, AsrcError> {
        let need = state.asrc.input_frames_next();
        let capacity = self.capacity;
        if need > capacity {
            return Err(AsrcError::Process("input chunk exceeds jitter ring"));
        }
        let channels = self.config.channels;
        let available = self
            .written_upto
            .saturating_sub(self.read_frame)
            .min(capacity as u64) as usize;
        let frames_in_ring = available.min(need);
        {
            let input = state.asrc.input_mut();
            for frame in 0..frames_in_ring {
                let position = ((self.read_frame + frame as u64) & self.mask as u64) as usize;
                let source = position * channels;
                for channel in 0..channels {
                    input.channel_mut(channel)[frame] = self.samples[source + channel];
                }
            }
            for channel in 0..channels {
                input.clear_range(channel, frames_in_ring, need);
            }
        }
        let outcome = state.asrc.process()?;
        if outcome.output_frames != self.config.block_frames {
            return Err(AsrcError::Process("resampler output size mismatch"));
        }
        // 媒体时间前进 need 帧（缺帧按静音补；游标照常前进，不倒退）。
        self.advance_read(need as u64);
        let fill = self
            .written_upto
            .saturating_sub(self.read_frame)
            .min(capacity as u64) as usize;
        state.asrc.update_fill(fill)?;
        if frames_in_ring < need {
            // 欠载：out 已预先清零，整块静音（不输出半块）。
            self.stats.underruns = self.stats.underruns.saturating_add(1);
            return Ok(BlockOutcome {
                status: BlockStatus::Underrun,
                frames: 0,
                stats: self.stats(),
            });
        }
        let output = state.asrc.output();
        let out_channels = self.config.output_channels;
        for frame in 0..self.config.block_frames {
            let destination = frame * out_channels;
            for (rtp_channel, &slot) in self.config.channels_map.iter().enumerate() {
                out[destination + usize::from(slot)] = output.channel(rtp_channel)[frame];
            }
        }
        self.stats.frames_filled = self
            .stats
            .frames_filled
            .saturating_add(self.config.block_frames as u64);
        Ok(BlockOutcome {
            status: BlockStatus::Filled,
            frames: self.config.block_frames,
            stats: self.stats(),
        })
    }

    /// 原路径（无 ASRC / ASRC 失败回退）：每块硬消费 block_frames 帧。
    fn fill_block_legacy(&mut self, out: &mut [f32]) -> BlockOutcome {
        let block = self.config.block_frames as u64;
        let available = self.written_upto.saturating_sub(self.read_frame);
        if available < block {
            // 欠载：整块静音 + 播放游标前进；缺口内的已到帧丢弃（与 aoip 语义一致）。
            self.stats.underruns = self.stats.underruns.saturating_add(1);
            self.advance_read(block);
            return BlockOutcome {
                status: BlockStatus::Underrun,
                frames: 0,
                stats: self.stats(),
            };
        }

        let channels = self.config.channels;
        let out_channels = self.config.output_channels;
        let mask = self.mask;
        let read_frame = self.read_frame;
        for frame in 0..self.config.block_frames {
            let position = ((read_frame + frame as u64) & mask as u64) as usize;
            let source = position * channels;
            let destination = frame * out_channels;
            for (rtp_channel, &slot) in self.config.channels_map.iter().enumerate() {
                out[destination + usize::from(slot)] = self.samples[source + rtp_channel];
            }
        }
        self.stats.frames_filled = self
            .stats
            .frames_filled
            .saturating_add(self.config.block_frames as u64);
        self.advance_read(block);
        BlockOutcome {
            status: BlockStatus::Filled,
            frames: self.config.block_frames,
            stats: self.stats(),
        }
    }

    /// 测试专用：本次 fill_block 是否强制 ASRC 失败（生产构建恒为 false）。
    #[cfg(test)]
    fn asrc_injected_failure(&mut self) -> bool {
        std::mem::take(&mut self.asrc_fail_next)
    }

    /// 生产构建：没有失败注入。
    #[cfg(not(test))]
    fn asrc_injected_failure(&mut self) -> bool {
        false
    }
    /// 重置为新流：清空抖动环与锚点，统计计数保持累计（调用方自行计 ssrc_changes）。
    fn restart_stream(&mut self, ssrc: u32, timestamp: u32) {
        self.ssrc = Some(ssrc);
        self.ts0 = timestamp;
        self.last_ts = None;
        self.last_seq = None;
        self.missing_len = 0;
        self.read_frame = 0;
        self.written_upto = 0;
        self.prefill_done = false;
        // 换流后 ASRC 复位（清重采样器状态/控制器积分/bypass 标记），新流重新尝试启用。
        if let Some(state) = &mut self.asrc {
            state.asrc.reset();
        }
        // 样本区不清零：有效位图已清，新流写入前不会被读到（避免换流时 O(capacity) memset，
        // 也保证不会把旧流样本当新流输出）。
        self.valid.fill(0);
    }

    /// 登记 count 个缺失序号（从 first 起，回绕）；缺口表满后只累计 lost 不再跟踪。
    fn note_missing(&mut self, first: u16, count: usize) {
        for index in 0..count {
            if self.missing_len >= MISSING_TRACK_PACKETS {
                break;
            }
            self.missing[self.missing_len] = first.wrapping_add(index as u16);
            self.missing_len += 1;
        }
        self.stats.lost = self.stats.lost.saturating_add(count as u64);
    }

    /// 写入帧的有效位。
    #[inline]
    fn set_valid(&mut self, frame: u64) {
        let position = (frame & self.mask as u64) as usize;
        self.valid[position >> 6] |= 1u64 << (position & 63);
    }

    /// 推进连续水位（从 written_upto 起，直到碰到未写或已消费的帧）。
    fn advance_watermark(&mut self) {
        let limit = self.read_frame.saturating_add(self.capacity as u64);
        while self.written_upto < limit && self.valid_bit(self.written_upto) {
            self.written_upto += 1;
        }
    }

    /// 帧是否有效。
    #[inline]
    fn valid_bit(&self, frame: u64) -> bool {
        let position = (frame & self.mask as u64) as usize;
        self.valid[position >> 6] & (1u64 << (position & 63)) != 0
    }

    /// 清掉 [from, from + count) 的有效位。
    fn clear_valid(&mut self, from: u64, count: u64) {
        let end = from.saturating_add(count);
        for frame in from..end {
            let position = (frame & self.mask as u64) as usize;
            self.valid[position >> 6] &= !(1u64 << (position & 63));
        }
    }

    /// 播放游标前进 frames 帧：清有效位、推进游标；水位至少跟上游标。
    ///
    /// 欠载时水位跟上游标，保证后续迟到的旧包按 late 丢弃，不会把过期数据当新数据播放。
    fn advance_read(&mut self, frames: u64) {
        let end = self.read_frame.saturating_add(frames);
        self.clear_valid(self.read_frame, frames);
        self.read_frame = end;
        if self.written_upto < end {
            self.written_upto = end;
        }
    }

    /// 时间戳跳变判定的有符号差（wrap 安全）。
    fn timestamp_step(previous: u32, current: u32) -> i64 {
        i64::from(current.wrapping_sub(previous) as i32)
    }
}

/// 换流原因（内部）。
#[derive(Clone, Copy)]
enum RestartKind {
    /// SSRC 变化。
    Ssrc,
    /// 时间戳大跳变。
    Timestamp,
}

/// 解析后的 RTP 头（本模块最小实现；字段只保留输入后端用到的部分）。
struct RtpHeader {
    payload_type: u8,
    sequence: u16,
    timestamp: u32,
    ssrc: u32,
}

/// 解析后的 RTP 包视图（payload 在原始缓冲区内的偏移/长度）。
struct ParsedRtp {
    header: RtpHeader,
    payload_offset: usize,
    payload_len: usize,
}

/// 解析 RTP 头（RFC 3550 基础头 + CSRC + 单个 header extension + padding 跳过）。
///
/// 与 khaen-aoip-aes67::rtp::Header::parse 行为一致；只支持 v=2、单扩展（按长度跳过，
/// 不解释内容），不实现 RTCP/SRTP/冗余。待与 aoip crate 统一（见模块文档）。
fn parse_rtp(buf: &[u8]) -> Result<ParsedRtp, InputError> {
    if buf.len() < RTP_HEADER_LEN {
        return Err(InputError::PacketTooShort);
    }
    let version = buf[0] >> 6;
    if version != RTP_VERSION {
        return Err(InputError::BadVersion);
    }
    let padding = buf[0] & 0x20 != 0;
    let extension = buf[0] & 0x10 != 0;
    let csrc_count = buf[0] & 0x0f;
    let payload_type = buf[1] & 0x7f;
    let sequence = u16::from_be_bytes([buf[2], buf[3]]);
    let timestamp = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let ssrc = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);

    let mut offset = RTP_HEADER_LEN + usize::from(csrc_count) * 4;
    if buf.len() < offset {
        return Err(InputError::CsrcOverflow);
    }
    if extension {
        if buf.len() < offset + 4 {
            return Err(InputError::ExtensionOverflow);
        }
        let words = usize::from(u16::from_be_bytes([buf[offset + 2], buf[offset + 3]]));
        offset += 4 + words * 4;
        if buf.len() < offset {
            return Err(InputError::ExtensionOverflow);
        }
    }
    let mut payload_len = buf.len() - offset;
    if padding {
        if payload_len == 0 {
            return Err(InputError::BadPadding);
        }
        let padding_len = usize::from(buf[buf.len() - 1]);
        if padding_len == 0 || padding_len > payload_len {
            return Err(InputError::BadPadding);
        }
        payload_len -= padding_len;
    }
    Ok(ParsedRtp {
        header: RtpHeader {
            payload_type,
            sequence,
            timestamp,
            ssrc,
        },
        payload_offset: offset,
        payload_len,
    })
}

/// 由 payload 长度与 RTP 声道数求帧数（必须整除）。
#[allow(clippy::manual_is_multiple_of)]
fn frames_in_payload(payload_len: usize, channels: usize) -> Result<usize, InputError> {
    if channels == 0 {
        return Err(InputError::BadPayload);
    }
    let frame_bytes = channels * L24_SAMPLE_BYTES;
    if payload_len == 0 || payload_len % frame_bytes != 0 {
        return Err(InputError::BadPayload);
    }
    Ok(payload_len / frame_bytes)
}

/// 解码一个 L24 样本（大端 3 字节，24-bit 线性）为 f32。
///
/// 公式与 khaen-aoip-aes67::rtp 一致：24-bit 符号扩展后左移 8 位得到满量程 i32
/// （i32 / 2^31），数值上等于 24bit / 2^23。offset 为样本在 payload 内的字节偏移，
/// 调用方保证 offset + 3 <= payload.len()。
#[inline]
fn decode_l24_sample(payload: &[u8], offset: usize) -> f32 {
    let raw = (i32::from(payload[offset]) << 16)
        | (i32::from(payload[offset + 1]) << 8)
        | i32::from(payload[offset + 2]);
    (raw << 8) as f32 / FULL_SCALE_DIVISOR
}
/// 解析 `KHAEN_ASRC` 取值（`off`|`poly`|`sinc`，大小写不敏感/允许首尾空白）。
///
/// None（未设置）与 off 都返回 None；其它值返回 BadConfig（不静默回退）。
fn parse_asrc_interp(value: Option<&str>) -> Result<Option<Interpolation>, InputError> {
    match value {
        None => Ok(None),
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.eq_ignore_ascii_case("off") {
                return Ok(None);
            }
            match Interpolation::parse(trimmed) {
                Some(interp) => Ok(Some(interp)),
                None => Err(InputError::BadConfig("KHAEN_ASRC must be off|poly|sinc")),
            }
        }
    }
}

/// 读环境变量 `KHAEN_ASRC`（未设置 = off；非 Unicode/非法值 = 配置错误）。
fn asrc_interp_from_env() -> Result<Option<Interpolation>, InputError> {
    match std::env::var(ASRC_ENV) {
        Ok(value) => parse_asrc_interp(Some(&value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(InputError::BadConfig("KHAEN_ASRC must be valid UTF-8"))
        }
    }
}

/// 由输入配置与抖动环容量推导 ASRC 目标水位（帧）。
///
/// - `jitter_target_frames > 0` 时尊重用户配置，否则默认 `2 × block_frames`；
/// - 上限 `capacity / 2`：给"写到读游标 + 容量"之前的突发包留 headroom，避免 ASRC
///   把 fill 稳在环的悬崖边；
/// - 下限 `block_frames`：至少留一块可消费数据（capacity ≥ 4 × block 时成立）。
fn asrc_target_fill(config: &Aes67InputConfig, capacity_frames: usize) -> usize {
    let desired = if config.jitter_target_frames > 0 {
        config.jitter_target_frames
    } else {
        ASRC_TARGET_BLOCKS * config.block_frames
    };
    desired.min(capacity_frames / 2).max(config.block_frames)
}

/// ASRC 构造错误 → 输入配置错误（保留点名原因；其余归为通用构造失败）。
fn map_asrc_error(error: AsrcError) -> InputError {
    match error {
        AsrcError::BadConfig(reason) => InputError::BadConfig(reason),
        _ => InputError::BadConfig("ASRC construction failed"),
    }
}

#[cfg(test)]
mod asrc_unit_tests {
    use super::*;

    fn config() -> Aes67InputConfig {
        Aes67InputConfig {
            multicast_addr: Ipv4Addr::new(239, 69, 0, 1),
            port: 5004,
            payload_type: 96,
            sample_rate: 48_000,
            channels: 2,
            channels_map: vec![0, 1],
            output_channels: 2,
            block_frames: crate::engine::DEFAULT_BLOCK_FRAMES,
            jitter_target_frames: 0,
        }
    }

    #[test]
    fn asrc_env_parsing_is_strict() {
        assert_eq!(parse_asrc_interp(None), Ok(None));
        assert_eq!(parse_asrc_interp(Some("off")), Ok(None));
        assert_eq!(parse_asrc_interp(Some("OFF")), Ok(None));
        assert_eq!(parse_asrc_interp(Some(" off ")), Ok(None));
        assert_eq!(
            parse_asrc_interp(Some("poly")),
            Ok(Some(Interpolation::Poly))
        );
        assert_eq!(
            parse_asrc_interp(Some("Sinc")),
            Ok(Some(Interpolation::Sinc))
        );
        assert_eq!(
            parse_asrc_interp(Some("bogus")),
            Err(InputError::BadConfig("KHAEN_ASRC must be off|poly|sinc"))
        );
        assert!(parse_asrc_interp(Some("")).is_err());
    }

    #[test]
    fn asrc_target_fill_respects_ring_headroom() {
        let config = config();
        let capacity = config.capacity_frames().expect("capacity");
        let target = asrc_target_fill(&config, capacity);
        assert_eq!(target, 2 * config.block_frames);
        assert!(target <= capacity / 2);
        // jitter_target 大于容量一半时被夹住（不让 fill 稳在悬崖边）。
        let mut large = config;
        large.jitter_target_frames = 4096;
        let capacity = large.capacity_frames().expect("capacity");
        assert_eq!(asrc_target_fill(&large, capacity), capacity / 2);
    }

    #[test]
    fn new_with_asrc_exposes_status_and_interpolation() {
        let input = Aes67Input::new_with_asrc(config(), Some(Interpolation::Sinc)).expect("input");
        assert!(input.asrc_enabled());
        assert_eq!(input.asrc_interpolation(), Some(Interpolation::Sinc));
        let target = input.asrc_target_fill_frames().expect("target");
        let status = input.asrc_status().expect("status");
        assert!(status.enabled);
        assert_eq!(status.fill_frames, 0);
        assert_eq!(status.ratio_ppm, 0.0);
        assert!(status.last_error.is_none());
        assert!(target > 0);
        // off = 完全走原路径。
        let plain = Aes67Input::new_with_asrc(config(), None).expect("input");
        assert!(!plain.asrc_enabled());
        assert!(plain.asrc_status().is_none());
        assert!(plain.stats().asrc.is_none());
        assert_eq!(plain.stats().asrc_bypasses, 0);
    }

    #[test]
    fn asrc_failure_bypasses_and_counts() {
        let mut input =
            Aes67Input::new_with_asrc(config(), Some(Interpolation::Poly)).expect("input");
        // 喂若干 128 帧静音包（每包都在抖动环窗口内），让预填水位达标。
        let target = input.asrc_target_fill_frames().expect("target");
        let packet_frames = 128usize;
        let samples = vec![0i32; packet_frames * input.config().channels];
        let packets = (target + 256).div_ceil(packet_frames);
        for index in 0..packets {
            let timestamp = (index * packet_frames) as u32;
            let packet = tests_packet(index as u16, timestamp, &samples);
            input.feed_packet(&packet).expect("feed");
        }
        let mut out = vec![0.0f32; input.samples_per_block()];
        assert_eq!(input.fill_block(&mut out).status, BlockStatus::Filled);
        assert_eq!(input.stats().asrc_bypasses, 0);
        // 注入一次失败：本块回退原路径，计数 +1、bypass 置位。
        input.asrc_fail_next = true;
        assert_eq!(input.fill_block(&mut out).status, BlockStatus::Filled);
        let stats = input.stats();
        assert_eq!(stats.asrc_bypasses, 1);
        assert!(!input.asrc_enabled());
        let report = stats.asrc.expect("asrc report");
        assert!(!report.enabled);
        assert_eq!(report.last_error, Some("ERR_ASRC_PROCESS"));
        // bypass 后继续按原路径输出，不再重复计数。
        assert_eq!(input.fill_block(&mut out).status, BlockStatus::Filled);
        assert_eq!(input.stats().asrc_bypasses, 1);
    }

    /// 组一个最小 RTP 包（测试用；与 integration test 的构造保持同一格式）。
    fn tests_packet(sequence: u16, timestamp: u32, samples: &[i32]) -> Vec<u8> {
        let mut bytes = vec![0u8; RTP_HEADER_LEN + samples.len() * L24_SAMPLE_BYTES];
        bytes[0] = 0x80;
        bytes[1] = 96;
        bytes[2..4].copy_from_slice(&sequence.to_be_bytes());
        bytes[4..8].copy_from_slice(&timestamp.to_be_bytes());
        bytes[8..12].copy_from_slice(&0x0bad_cafeu32.to_be_bytes());
        for (index, &sample) in samples.iter().enumerate() {
            let raw = (sample as u32) & 0x00ff_ffff;
            let offset = RTP_HEADER_LEN + index * L24_SAMPLE_BYTES;
            bytes[offset] = (raw >> 16) as u8;
            bytes[offset + 1] = (raw >> 8) as u8;
            bytes[offset + 2] = raw as u8;
        }
        bytes
    }
}
